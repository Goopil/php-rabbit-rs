# Strict audit — Rabbit RS plans (design + implementation) vs implemented code

**Date:** July 31, 2026
**Auditor:** euria-code
**Scope:** `docs/plans/2026-07-30-rabbitmq-native-design.md`, `docs/plans/2026-07-30-rabbitmq-native-implementation.md`, code of Tasks 1–12 (Milestone A complete).

## Executive summary

The overall architecture is sound: core/extension/package separation, `Transport` abstraction, Tokio actors, deterministic tests. Milestone A passes its gate (100 tests, clippy, fmt). However, the audit reveals **5 HIGH bugs** (data loss, deadlock, memory leak), **7 MEDIUM issues** and **5 LOW issues**, plus **6 divergences** between the plans and the code. Most HIGH items are silent bugs that will only manifest in real integration (Milestone D) or production.

| Severity | Count | Blocking? |
|----------|-------|------------|
| 🔴 HIGH   | 5     | Yes — before Milestone B |
| 🟡 MEDIUM | 7     | Recommended — before Milestone C |
| 🔵 LOW    | 5     | No — backlog |
| 🏗️ Architecture/drift | 6 | Recommended — before Milestone B |

---

## Critical bugs (HIGH)

### #1 — AMQP `message_id` lost at consumption

**File:** `crates/rabbit-rs-core/src/transport/lapin.rs:251`

`map_headers` only extracts `properties.headers()`. The basic properties (`message_id`, `correlation_id`, `timestamp`, `delivery_mode`) are discarded. The Rust `Delivery` builds a synthetic ID `"generation:channel:delivery_tag"` which **changes on every redelivery**.

**Impact:** Laravel `getJobId()` must return the stable UUID set by the publisher. Without it, Laravel retries, deduplication, and failed jobs are broken.

**Fix:** Extend the transport `Delivery` with `message_id: Option<String>` and `correlation_id: Option<String>`, extract them in `map_delivery` from `delivery.properties`, and propagate up to the public consumer `Delivery`.

### #2 — Generation rejection bug in the publisher

**File:** `crates/rabbit-rs-core/src/publisher/actor.rs:408`

```rust
if generation <= state.generation {  // BUG: should be <
```

If the coordinator sends `Recovering { generation: N }` then `Ready { generation: N }` (same generation, normal case), `suspend()` advances `self.generation` to `N`. Then `Ready { generation: N }` is rejected because `N <= N`. The publisher never resumes.

`connection_lost()` masks the bug by sending `generation: 0`, but the real coordinator (Task 6 `ConnectionActor`) will send the actual generation.

**Impact:** After a recovery, the publisher stays suspended indefinitely. All pending publications remain in replay and never leave.

**Fix:** Change `<=` to `<`. Add a test: `Recovering { generation: 3 }` then `Ready { generation: 3 }` must succeed.

### #3 — `SubscriptionId` has no accessor → corrupted consumer tag

**File:** `crates/rabbit-rs-core/src/consumer/set.rs:133`

```rust
format!("rabbit-rs.{:?}", subscription.id)
```

`SubscriptionId(String)` derives `Debug` → produces `rabbit-rs.SubscriptionId("orders_high")` with the struct name and quotes. The AMQP tag contains invalid characters and an unexpected format.

**Impact:** Unreadable consumer tag in RabbitMQ Management, difficult debugging, potentially rejected by some strict brokers.

**Fix:** Add `pub fn as_str(&self) -> &str { &self.0 }` to `SubscriptionId` and replace `{:?}` with `{}` via `as_str()`.

### #4 — Unbounded `source_errors` — memory leak

**File:** `crates/rabbit-rs-core/src/consumer/actor.rs:56`

```rust
source_errors: VecDeque<ConsumerError>,  // no limit
```

If a stream produces errors continuously (flappy connection) with no waiters to consume them, memory grows indefinitely. The design requires "bounded buffers" everywhere.

**Impact:** OOM in production on an unstable connection.

**Fix:** Bound to `max_in_flight` or a constant (e.g. 64). When the limit is reached, drop old errors or pause the stream.

### #5 — `RuntimeRegistry::acquire` can block indefinitely

**File:** `crates/rabbit-rs-core/src/runtime.rs:111`

`close_state` → `state.take()` drops the Tokio `Runtime` under the `Mutex<Option<ProcessState>>`. If tasks (ConnectionActor, ConsumerActor) are still running, the runtime drop blocks. No explicit `shutdown_timeout()`.

**Impact:** After a fork, the child process can hang on the first `acquire()` if parent tasks are still running. Possible deadlock.

**Fix:** Use `runtime.shutdown_timeout(Duration::from_secs(1))` before dropping, or move the drop out of the Mutex via `std::mem::take` + spawn a thread to join.

---

## Moderate bugs (MEDIUM)

### #6 — Hardcoded 30 s deadline for delayed release

**File:** `crates/rabbit-rs-core/src/consumer/actor.rs:363`

```rust
tokio::time::Instant::now() + Duration::from_secs(30)
```

Not configurable. If the broker is slow during recovery, the delayed release fails by timeout with no retry.

**Fix:** Replace with `state.config.publish_deadline` or derive it from the publisher's `confirm_timeout`.

### #7 — Delivery lost if waiter dropped

**File:** `crates/rabbit-rs-core/src/consumer/actor.rs:170`

```rust
if waiter.send(Ok(item)).is_ok() {
    self.in_flight = self.in_flight.saturating_add(1);
}
```

If `send` fails (waiter cancelled on the Laravel side), the message leaves the buffer without an ack. It remains unacked on the broker side until the next disconnection.

**Fix:** If `send` fails, push the `TransportDelivery` back into the subscription buffer and `mark_ready`.

### #8 — `ConsumerSet::spawn` without rollback

**File:** `crates/rabbit-rs-core/src/consumer/set.rs:121`

If `set_qos` succeeds for subscription 1 but `consume` fails for 2, the already-configured channels remain open. No cleanup.

**Fix:** On error, close all already-opened channels before propagating the error.

### #9 — URI with credentials passed to Lapin

**File:** `crates/rabbit-rs-core/src/transport/lapin.rs:39`

`Connection::connect(uri.as_str(), ...)` where the URI contains `user:password@host`. If Lapin logs this URI (error path, debug), the password leaks. The design forbids "full URI" in logs.

**Fix:** Build the URI without credentials and pass `Credentials` separately via Lapin's `ConnectionProperties`, or use an opaque URI and log only `host:port/vhost`.

### #10 — `PublishRequest::new` defaults `mandatory: false`

**File:** `crates/rabbit-rs-core/src/transport.rs:186`

The actor overrides it to `true`, but the transport API allows publishing without mandatory. A direct transport call (bypassing the actor) loses mandatory routing.

**Fix:** Either default `mandatory: true`, or mark the constructor `pub(crate)` and require a builder.

### #11 — No `Reject` on the Rust `Delivery`

**File:** `crates/rabbit-rs-core/src/consumer/delivery.rs`

`Settlement` only has `Ack` and `Release(Duration)`. `reject(requeue=false)` (discard) is not implemented. The Task 13 PHP API (`reject(bool $requeue)`) requires it.

**Fix:** Add `Settlement::Reject { requeue: bool }` and implement it in `settle`.

### #12 — `DeliveryToken::settle` can loop on transport error

**File:** `crates/rabbit-rs-core/src/consumer/delivery.rs:173`

A (non-stale) transport error resets the state to `Pending`, allowing an immediate retry. If the generation has not been updated yet, the retry hits the same dead connection → loop until `UpdateGeneration` arrives. No backoff or limit.

**Fix:** Count retries and return `ConsumerErrorKind::Transport` after N attempts, or wait for an `UpdateGeneration` before allowing a new `settle`.

---

## Minor bugs (LOW)

### #13 — `effective_priority` divides nanos — `scheduler.rs:161`

`as_nanos()` returns `u128`. The `i64` conversion is handled, but with `starvation_after = 30s`, the first step only arrives after 30 s. Acceptable but not configurable from config.

### #14 — `ConnectionKey` contains a hash of the password — `config.rs:341`

SHA-256 includes the password. `ConnectionKey` derives `Debug`. Not a direct leak, but if the key is exposed in metrics, an attacker knowing the algorithm could attempt a rainbow table. Low risk.

### #15 — `publish_properties` encodes all headers as `LongString` — `transport/lapin.rs:405`

Even booleans and integers come out as `LongString` (via `to_string().into_bytes()`). Reading back via `map_header_value` decodes them as strings. The round-trip loses the original type.

---

## Plan ↔ code divergences (Architecture)

### A1 — `SchedulerConfig` vs design

The design places `max_in_flight` in the `scheduler` key (`config/rabbit-rs.php`), but the Rust code puts it at the `WorkerProfile` level. The Laravel normalizer will have to translate — confusion risk.

### A2 — `Settlement` does not expose `Reject`

The planned PHP API (Task 13) has `reject(bool $requeue)`, but Rust only has `Ack` and `Release(Duration)`. `reject(false)` (discard without requeue) does not exist yet.

### A3 — `AttemptsResolver` default = no limit

`Default::default()` yields `max_attempts: None`. The design says "delivery limit of 20 unless external policy". The default should be `NonZeroU32::new(20)`.

### A4 — Jitter at 50% instead of 20%

The design says "20% jitter", but `EqualJitter` returns 50–100% of the delay. Silent drift.

### A5 — `starvation_after` missing from `SubscriptionConfig`

`SubscriptionPolicy::new()` panics if `starvation_after` is zero, but `SubscriptionConfig` does not provide this value. The config→runtime binding will have to fill this gap or panic.

### A6 — `Delivery` does not expose the AMQP `message_id`

See bug #1.

---

## Implementation plan

### Phase 1 — HIGH fixes (before any Milestone B work)

| # | Task | Files | Required test | Effort |
|---|------|-------|---------------|--------|
| 1 | Extend transport `Delivery` with `message_id` + `correlation_id`, extract in Lapin, propagate to the public `Delivery` | `transport.rs`, `transport/lapin.rs`, `consumer/delivery.rs`, `consumer/actor.rs` | `delivery_attempts.rs`: verify `message_id` stable after redelivery | 2 h |
| 2 | Change `<=` to `<` in `handle_connection_event` Ready | `publisher/actor.rs:408` | `publisher_recovery.rs`: `Recovering { gen: 3 }` then `Ready { gen: 3 }` succeeds | 15 min |
| 3 | Add `SubscriptionId::as_str()`, replace `{:?}` with `as_str()` in the consumer tag | `consumer/scheduler.rs`, `consumer/set.rs` | `consumer_semantics.rs`: tag does not contain `SubscriptionId(` | 15 min |
| 4 | Bound `source_errors` to `max_in_flight.max(64)` | `consumer/actor.rs` | `consumer_semantics.rs`: 100 consecutive errors do not grow memory beyond the bound | 30 min |
| 5 | Call `shutdown_timeout` before dropping the runtime, outside the Mutex | `runtime.rs` | `runtime.rs tests`: fork with running tasks does not hang | 1 h |

### Phase 2 — MEDIUM fixes (during Milestone B)

| # | Task | Files | Effort |
|---|------|-------|--------|
| 6 | Configure the delayed release deadline | `consumer/actor.rs`, `consumer/set.rs` | 30 min |
| 7 | Push the delivery back if the waiter is dropped | `consumer/actor.rs` | 30 min |
| 8 | Roll back open channels on `spawn` failure | `consumer/set.rs` | 45 min |
| 9 | Pass credentials to Lapin outside the URI | `transport/lapin.rs` | 1 h |
| 10 | `mandatory: true` default on `PublishRequest` | `transport.rs` | 15 min |
| 11 | Add `Settlement::Reject { requeue: bool }` | `consumer/delivery.rs`, `consumer/actor.rs` | 1 h |
| 12 | Retry counter on `DeliveryToken::settle` | `consumer/delivery.rs` | 45 min |

### Phase 3 — LOW fixes + drift corrections (backlog)

| # | Task | Effort |
|---|------|--------|
| 13 | Document the aging computation in the scheduler | 15 min |
| 14 | Do not hash the password in `ConnectionKey` (or do not expose in Debug) | 30 min |
| 15 | Encode headers according to their original AMQP type | 1 h |
| A1 | Align `max_in_flight` between design config and Rust code | 30 min |
| A3 | Default `AttemptsResolver` to 20 | 15 min |
| A4 | Fix `EqualJitter` to 20% | 15 min |
| A5 | Add `starvation_after` to `SubscriptionConfig` | 30 min |

### Final verification

After Phase 1:
```sh
rtk cargo fmt --all
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
rtk cargo test -p rabbit-rs-core
rtk ./scripts/check.sh
```

After Phase 2:
```sh
rtk cargo test -p rabbit-rs-core --test consumer_semantics --test publisher_recovery --test delivery_attempts
rtk ./scripts/check.sh
```

### Suggested order

1. **#2** (15 min, maximum impact, trivial fix)
2. **#3** (15 min, trivial fix)
3. **#1** (2 h, structural impact but required before Milestone B)
4. **#5** (1 h, fork safety)
5. **#4** (30 min, memory leak)
6. Phase 2 in parallel with Milestone B
7. Phase 3 as backlog

---

## Risks not covered by this audit

- **Missing real integration tests**: Milestone A only uses the mock transport. Bugs #1, #2 and #9 will only manifest with a real broker (Milestone D).
- **PHP extension not written yet**: the PHP API (Task 13+) will reveal further gaps (types, conversion, lifecycle).
- **No fuzzing**: the scheduler, the batcher, and the confirms ledger are ideal targets for property tests (proptest/quickcheck).
- **No benchmark**: batch/prefetch values are arbitrary. Milestone E must calibrate them.
