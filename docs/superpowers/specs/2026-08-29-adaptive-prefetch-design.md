# Design — Adaptive per-subscription prefetch

Date: 2026-08-29
Status: approved (brainstorming)
Branch: `feat-auto-prefetch`
Scope: Rust core crate + Laravel package + docs. No dedicated benchmark in this milestone.

## Motivation

Fixed prefetch per subscription cannot suit queues with opposite profiles at the same time:

- fast jobs (5-20 ms): a low prefetch (e.g. 8) lets the pipeline drain between two acks,
  the network RTT becomes visible and throughput caps at `prefetch / job_duration`;
- slow jobs (30 s): a high prefetch (e.g. 64, Laravel default) keeps 64 messages
  in flight for nothing — wasted memory and an amplified crash radius (every message
  unacked at crash time must be redelivered, i.e. replayed).

The original design document (`docs/plans/2026-07-30-rabbitmq-native-design.md`,
§ "Planned evolutions") anticipated this evolution: *"adaptive prefetch based on
EWMA, target buffer time, hysteresis, and memory pressure"*, with the union config
format (`'prefetch' => ['mode' => ..., ...]`) already present in the Laravel layer.
The required metrics (settlement latency including PHP job duration via
`reserved_at`) have been collected since V1.

## Validated decisions

1. **Control signal**: EWMA of settlement latency (reservation → ack,
   including PHP job duration). Target prefetch = `target_buffer_seconds / ewma_duration`,
   clamped to `[min, max]`, applied with hysteresis.
2. **Config surface**: complete — `mode`, `initial`, `min`, `max`,
   `target_buffer_seconds`.
3. **early_ack / no_ack**: rejected in validation with the adaptive mode (the signal
   does not exist: the ack is sent before the PHP job, or does not exist at all).
4. **Observability**: included — `ConsumerHandle::prefetch_stats()` + PHP method
   `Consumer::getPrefetchStats()`.
5. **Approach**: pure controller + tick in the actor (approach A). Alternatives
   (separate controller task, PHP-driven control) are rejected at the end of this document.
6. **Recommended adaptive defaults**: `initial = 64` (continuity with the current
   Laravel default), `min = 1`, `max = 256` (prudent cap, the user can raise it),
   `target_buffer_seconds = 5`. AMQP `0` (= unlimited) remains refused everywhere.

## 1. Core configuration (`crates/rabbit-rs-core/src/config.rs`)

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrefetchConfig {
    Fixed(u16),
    Adaptive {
        initial: u16,
        min: u16,
        max: u16,
        target_buffer: Duration,
    },
}
```

- `SubscriptionConfig.prefetch` changes from `u16` to `PrefetchConfig`.
- **Accepted wire formats** (custom deserialization, backward compatible):
  1. bare integer: `"prefetch": 16` → `Fixed(16)` (the form the Laravel
     normalizer emits today for fixed mode — behavior unchanged);
  2. fixed union: `{"mode": "fixed", "value": 16}` → `Fixed(16)`;
  3. adaptive union: `{"mode": "adaptive", "initial": 64, "min": 1, "max": 256,
     "target_buffer_seconds": 5}` → `Adaptive { .. }` (`target_buffer_seconds`
     deserialized via the existing `deserialize_duration_seconds` helper).
- **Validation** (typed error paths, existing convention):
  - bare integer form: path `workers.X.subscriptions.Y.prefetch`, message unchanged
    ("prefetch must be greater than zero") — compatibility with current tests;
  - union form: path `...prefetch.value` / `...prefetch.min` / `...prefetch.max` /
    `...prefetch.initial` / `...prefetch.target_buffer_seconds`;
  - rules: fixed `value ≥ 1`; adaptive `min ≥ 1`, `min ≤ initial ≤ max`,
    `target_buffer_seconds > 0` (floor 1 ms);
  - **cross rejection**: adaptive combined with `early_ack = true` or `no_ack = true` →
    error "adaptive prefetch requires consumer acknowledgements…" with the path
    of the subscription;
  - `max ≤ 65535` guaranteed by `u16`.
- **Fingerprint** (config digest, config.rs:759): canonical hash of the enum —
  discriminant + fields (`to_be_bytes` for the `u16`s, seconds for the duration).
  Two configs differing only in adaptive mode must produce distinct
  fingerprints.

## 2. Pure controller (`crates/rabbit-rs-core/src/consumer/prefetch.rs`, new)

```rust
pub(crate) struct AdaptivePrefetch {
    bounds: PrefetchBounds,   // min, max, target_buffer
    current: u16,             // prefetch applied on the broker side
    ewma_ns: f64,             // EWMA of settlement latency
    samples: u64,
}
```

Pure API (no async dependency, unit-testable):

- `observe(&mut self, latency: Duration)`: EWMA update, α = 0.25 (documented
  internal constant).
- `tick(&mut self) -> Option<u16>`:
  - `samples < 3` → `None` (not enough data);
  - target = `ceil(target_buffer / ewma)`, saturating `f64 → u16` conversion,
    then clamped to `[min, max]`;
  - change applied only if `|target − current| ≥ max(1, current / 4)`
    (25% relative hysteresis) — otherwise `None`;
  - accepted change → `current = target`, returns `Some(target)`.

Internal constants (not configurable — YAGNI):

- `EWMA_ALPHA: f64 = 0.25`
- `PREFETCH_TICK: Duration = Duration::from_secs(1)` (application interval)
- `MIN_SAMPLES: u64 = 3`
- hysteresis: `25%` relative to current

Target semantics: `target_buffer_seconds` = amount of *work* (processing time)
that should be ready in the buffer. Examples: target 5 s, job 250 ms →
prefetch 20; job 10 ms → 500 → clamped to `max`; job 30 s → 1.

## 3. Consumer actor (`crates/rabbit-rs-core/src/consumer/actor.rs`)

- `ActorState` owns an `AdaptivePrefetch` per adaptive subscription
  (`HashMap<SubscriptionId, AdaptivePrefetch>`); fixed subscriptions have
  no entry.
- **Observation**: fed only by genuine acks — in the successful `Ack`-type
  settlement completions, for the `Settle` and `SettleThrough` commands, at the points where the actor already records
  `record_ack(token.reserved_at.elapsed())`. Explicitly excluded: releases
  (delay — the latency would include the delay and corrupt the EWMA), rejects, and the
  early-ack path (`record_ack(Duration::ZERO)`).
- **Application tick**: additional arm in the `tokio::select!` of
  `run_actor`, armed by the precondition `if has_adaptive` (no active interval
  when no subscription is adaptive — current behavior preserved identically otherwise). Each tick (1 s): for each subscription whose
  `tick()` returns `Some(v)` → apply `set_qos(v)`.
- **`set_qos` off the critical path**: it is a network round trip; running it
  directly in the actor arm would block dispatch and settlements during
  the RTT. Each application is therefore performed in a detached tokio task
  (`tokio::spawn`), which pushes an error into `error_tx` on transport failure
  (same error channel as settlements, bounded capacity 256, drop of the oldest).
  The tick itself is pure and instantaneous.
- Adjustments on a given channel are rare (25% hysteresis + 1 s tick);
  no additional guard against concurrent QoS calls.

## 4. Spawn, buffers, and recovery

- `Subscription` (`consumer/set.rs`): the field `prefetch: u16` becomes
  `prefetch: PrefetchConfig` + helper `effective_prefetch() -> u16` (initial value
  applied at spawn: `value` for fixed, `initial` for adaptive).
- **Buffer sizing** (`spawn_with_generation`, set.rs:170-217): the mpsc/flume capacity is computed from the sum of the **max** of adaptive
  subscriptions (and the values of fixed subscriptions) when at least one
  subscription is adaptive; otherwise the current behavior is unchanged. Runtime
  prefetch growth thus always has the required internal capacity —
  the "everything is explicitly bounded" invariant is preserved.
- **Recovery** (`pool/recovery_coordinator.rs:422`): reconstruction with the config;
  `.prefetch(...)` becomes `.prefetch(config)` (clone of the enum). The EWMA state is
  lost on re-spawn: the controller restarts from `initial` and re-learns. Accepted and
  documented (recovery already follows the deterministic order connection → … → QoS →
  consumers).

## 5. Laravel layer (`packages/laravel-queue`)

- `ConfigNormalizer::prefetch()` (ConfigNormalizer.php:421):
  - `['mode' => 'fixed', 'value' => N]` → always emits the bare integer `N` to the
    native layer (**zero wire change** for the fixed form);
  - `['mode' => 'adaptive', ...]` → validates `min ≥ 1`, `min ≤ initial ≤ max`,
    `target_buffer_seconds > 0` (messages with exact path, existing
    `positiveInt` convention, cap 65535) then emits the native union to the core;
  - **cross rejection**: adaptive + `early_ack`/`no_ack` → explicit error at the
    subscription path;
  - bare integer as Laravel input → treated as fixed (user config backward
    compat).
- `config/rabbit-rs.php` (doc block lines 175-190): documentation of the
  adaptive mode + commented example (disabled by default; fixed 64 default unchanged).
- `README.md`: prefetch section updated (env table + adaptive example).

## 6. Observability

- New command `ConsumerCommand::GetPrefetchStats { completed }` (oneshot).
- `ConsumerHandle::prefetch_stats() -> Vec<PrefetchStat>` with
  `PrefetchStat { subscription, queue, mode, current, ewma }` (`ewma: Duration`,
  zero as long as `samples == 0`). Instant response (actor state), one-shot
  command — not a tick.
- PHP extension: method `Consumer::getPrefetchStats()` returning an associative array
  `subscription → { mode, prefetch, ewma_ms }` (zero value for fixed).
- No change to global metrics (`Metrics`): no labels available,
  the per-subscription state goes through the dedicated command.

## 7. Tests (TDD — one focused failing test before each implementation)

- **Rust unit** (`consumer/prefetch.rs`, `#[cfg(test)]`):
  - EWMA: convergence, α applied, first sample;
  - tick: no change below the hysteresis threshold, change beyond it,
    clamping `[min, max]`, `ceil` saturation, fewer than 3 samples → `None`;
  - edge cases: very fast job (clamp to max), very slow (min), zero target
    forbidden upstream.
- **Rust config** (`config.rs::tests`): parsing of the 3 wire forms + invalid ones
  (integer 0, union missing field, `min > max`, `initial` out of bounds,
  `target_buffer_seconds = 0`); early_ack/no_ack cross rejection; fingerprint
  differentiated by mode.
- **Rust integration** (`crates/rabbit-rs-core/tests/`, paused tokio time +
  scriptable mock transport):
  - deliveries + acks at scripted latencies → after advancing time past the
    tick, assert the sequence `TransportOperation::Qos { prefetch: X }` on the
    mocked channel;
  - below the hysteresis threshold → no additional `Qos` operation;
  - mocked `set_qos` failure → error present in `drain_errors()`, the actor
    keeps going;
  - existing fixed set: no active tick arm (no Qos operation beyond spawn) —
    regression.
- **PHP Pest (Laravel)**: `ConfigNormalizerTest` — fixed unchanged (regression),
  adaptive pass-through with validated values, validation errors (exact
  paths), early_ack/no_ack cross rejection; Feature provider test with an
  adaptive config.
- **Extension**: PHPT/reflection for `getPrefetchStats()` (presence, shape).
- **Final gate**: full `rtk ./scripts/check.sh`.

## 8. Documentation

- `docs/plans/2026-07-30-rabbitmq-native-design.md`: the "adaptive prefetch" item
  of "Planned evolutions" marked implemented (reference to this spec).
- Laravel config comments + README (§5).

## Rejected alternatives

- **Separate controller task per ConsumerSet**: the same pure logic, but a
  dedicated tokio task that would send `ConsumerCommand::SetPrefetch`. Rejected: it
  would require exporting per-subscription latencies out of the actor (extra
  channel), fragile temporal coupling, more moving parts for an identical result.
- **PHP-driven adaptation** (`setPrefetch()` + Laravel loop): rejected —
  reaction limited by PHP polling, does not fit the "enabled by configuration"
  design of the Rust actor, one more API surface to maintain.
- **Composite signal (buffer depth + memory)**: rejected for this milestone — the EWMA
  of job time covers the need, fewer parameters to tune. The existing memory
  guard (`max_buffered_bytes`) stays in place.

## Known limitations (documented, accepted)

- The EWMA state is lost at recovery (ConsumerSet re-spawn): re-learning
  from `initial`.
- `early_ack`/`no_ack` have no usable signal: combinations rejected.
- Prefetch has no broker-side effect for `no_ack` consumers (AMQP
  semantics) — rejected with adaptive anyway.
- Quorum queues: high prefetch increases broker-side memory; the default
  `max = 256` and the hysteresis bound the exposure.
