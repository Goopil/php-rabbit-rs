# Design — Runtime worker profile synthesis (`auto_subscribe`, natively)

- **Date:** 2026-09-06 (revised same day)
- **Status:** approved; revised to shape S2 after owner re-review — no runtime registration API
- **Source:** #164 audit item 2 (coordinator-snapshot 404 on `__auto__.` queues) + market review (vyuldashev/laravel-queue-rabbitmq 2.1k★ — declare-on-use, hosts-only minimal config, **no** runtime registration API)
- **Absorbs:** #167
- **Aligns with:** #164 (tracking issue), #162 (`PrefetchConfig` in `SubscriptionConfig`), #157 (typed-but-gated precedent for upstream limits)

## Motivation

The current `auto_subscribe` path has the Laravel resolver invent an
`__auto__.{queue}` profile by mutating the config array *before* pool
creation. Two failure modes follow from that design:

1. **Late resolution fails.** When the pool is created before the first pop
   (Horizon supervisor boot, Octane warm pools), the runtime queue is not in
   the built `ValidatedConfig` and `Client::consumer()` rejects it:
   `workers.{profile}: unknown worker profile` (`client.rs:331`).
2. **Even early resolution can 404.** `TopologyPlan::from_config` runs once
   at coordinator spawn (`client.rs:742`) from the immutable config. A
   queue that only exists in a runtime profile is absent from that plan, so
   the declare-before-subscribe reconcile (`recovery_coordinator.rs:703`)
   never declares it and `basic.consume` fails with 404 in `Declare` mode.

The market standard makes both moot: the reference Laravel driver works with
hosts-only config and declares queues on use — and it offers **no** runtime
registration API either; dynamic queues simply run on global defaults while
tuned queues live in connection config. Zero-config consumption is table
stakes; the gap here is purely architectural: php-amqplib declares through
a synchronous channel RPC, while this driver routes declarations through
the topology plan and recovery coordinator (pooled connections, long-lived
push consumers, adaptive prefetch).

## Goals

- `pop($queue)` works for any queue with no `workers.*` entry (market parity).
- Explicit surfaces stay first-class and unchanged: `workers.*` config, the
  actionable `unknown worker profile` error for plain unknown names.
- The 404 dies at the root: the topology plan is derived from config *plus*
  requested runtime profiles at reconcile time.
- Everything bounded, typed, deterministic: no unbounded maps, config-path
  style errors, deterministic recovery order.
- No change to at-least-once semantics, the recovery-order invariant, or the
  #49 requested-vs-dormant consumer semantics.

## Non-goals

- **No `Pool::registerWorker` FFI and no runtime registration API.** A
  payload-carrying runtime registry exists internally, but its only entry
  point is the pop path itself (see §1 — this is what keeps #49 safe).
  If a real need for runtime tuning appears, a registration API can be
  added later on top of the same registry.
- No runtime broker/exchange/vhost synthesis — queues only.
- No persistence across a PHP process crash (same memory boundary as the
  in-flight replay buffers).
- No unregistration API — entries are added on first pop, bounded, and the
  process exit reclaims everything.
- No compatibility shim for the internal config-mutation mechanism (the
  user-visible behavior — pop just works — is unchanged).

## 1. Core: payload-carrying requested map (`crates/rabbit-rs-core`)

`Client.requested_profiles` changes type from
`Arc<StdMutex<HashSet<String>>>` (`client.rs:65`) to
`Arc<StdMutex<BTreeMap<String, WorkerProfile>>>`. One object, one semantic:
**every requested profile carries its payload, whether it came from config
or was synthesized.** The Arc already flows into every `CoordinatorContext`
(`client.rs:746`), so no new sharing machinery is introduced. Entries enter
the map **only** through `consumer()` resolution — there is no second
insertion path, which is what keeps registration and consumption request
from ever being confused (#49 stays intact).

- **Synthesis gate** in `consumer()` (`client.rs:328`): an unknown profile
  name prefixed `__auto__.` synthesizes a default profile — one
  subscription on the queue named after the prefix, all other fields from
  the same serde defaults a config-declared subscription gets, targeting
  the configured broker (single-broker configs; with multiple brokers,
  synthesis fails with an actionable error telling the user to declare the
  profile in `workers.*`). The synthesized profile runs through
  `validate_worker`, so every existing bound applies. The profile is then
  recorded in the map and resolution proceeds unchanged. Unknown names
  without the prefix keep today's `unknown worker profile` error — no
  typo-queues.
- **Map recording**: after resolution (config or synthesized), the payload
  lands via `entry(name).or_insert(worker.clone())`, replacing the bare
  name insert (`client.rs:338`). `is_requested` becomes `contains_key` —
  identical semantics. Config payloads are immutable (the `ValidatedConfig`
  never changes within a process), so a recorded payload can never drift
  from config.
- **Recovery loop** (`recovery_coordinator.rs:576`) keeps its config-order
  iteration for config workers filtered by the requested snapshot, then
  appends map extras (names not in config, name-sorted via `BTreeMap`),
  also filtered by requested. Deterministic, satisfying the recovery-order
  invariant.
- **`establish_requested_profile`** resolves the worker payload from
  config-or-map instead of `context.config.worker(profile)` alone.
- **The frozen plan is deleted.** `topology_plan` is removed from
  `RecoveryCoordinatorConfig` and `CoordinatorContext` (currently
  `recovery_coordinator.rs:123`, `:164`, `:196`). The two reconcile sites
  (`:532` recovery loop, `:703` establish) compute the plan fresh:
  `TopologyPlan::from_config_and(&context.config, &runtime_extras)` where
  `runtime_extras` are requested-map profiles whose names are not in
  config. This is the driver's declare-on-use, and it also covers a queue
  first popped after a publisher-only coordinator already spawned.

## 2. Laravel: resolver retarget (`packages/laravel-queue`)

The resolver keeps producing `__auto__.{queue}` profile names — that
convention now belongs to the core. The pre-creation config mutation
(`registerAutoProfile` writing into the `workers` array) is **removed**:
`pop($queue)` maps the queue to its `__auto__.` profile name and pops; the
core synthesizes. Nothing else changes in the PHP layer — no FFI surface,
no registration guards, no per-process bookkeeping. The Horizon wrapper
rides the same path.

## 3. Errors and bounds

| Case | Message |
|---|---|
| Unknown `__auto__.` name, multiple brokers configured | `workers.{name}: automatic profiles require a single configured broker; declare this profile under workers.*` |
| Synthesized profile fails a bound | passes through `validate_worker` unchanged (by construction this should not trigger; the check guards the invariant) |
| Unknown plain name (unchanged) | `workers.{profile}: unknown worker profile` |
| TopologyMode `External` | runtime queue is never declared; `basic.consume` surfaces the broker's 404 — same contract as vyuldashev's `declare => false` (documented, no code; verify the exact compile path at implementation) |

- Map bound: **64 synthesized profiles** (config profiles are not stored as
  extras beyond their requested payloads), insertion-rejected — a pool
  popping more than 64 distinct auto queues per process is a configuration
  smell, and the error says so.
- All existing subscription/scheduler bounds apply to synthesized profiles
  via `validate_worker`.

## 4. Testing (TDD — one failing test before each implementation step)

- **Core unit:** synthesis semantics — `__auto__.` prefix gate, defaults
  match config serde defaults, single-broker targeting, multi-broker
  error, plain-name error unchanged, deterministic map iteration, and a
  name never requested is never established (#49).
- **Core cross-module** (`tests/`, mock transport + paused Tokio time):
  pop-driven synthesis establishes the consumer *and declares the queue*
  (plan included it); recovery replays it; a synthesized profile whose
  name is not requested stays dormant; a queue first popped after a
  publisher-only coordinator spawned still gets declared.
- **Laravel Feature** (fake pool, no extension): `pop('__auto__.x')` — or
  the queue-mapped equivalent — resolves the `__auto__.` profile name and
  pops without any config mutation; `workers.*`-declared queues never hit
  the auto path.
- **Laravel Integration** (real broker): an auto queue pops and acks
  end-to-end in `Declare` mode; the poison/DLQ path stays intact.

## 5. Documentation

- `packages/laravel-queue/docs/laravel.md`: rewrite the auto_subscribe
  section — zero-config pop, the `__auto__.` contract, `External`-mode
  caveat, multi-broker caveat.
- Root `README.md`: one-line mention that unknown queues just work.
- `CHANGELOG.md` (Unreleased): note the internal mechanism change and the
  new core-side synthesis contract.
