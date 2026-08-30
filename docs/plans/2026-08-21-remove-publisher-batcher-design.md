# Remove Publisher Batcher — Design Spec

**Date:** 21 August 2026
**Status:** Approved

## Context

The publisher actor accumulates incoming messages in a `Batcher` before flushing them to the AMQP channel. AMQP 0-9-1 has no `batch_publish` primitive — each message gets its own `basic_publish` regardless. The batcher introduces a 1 ms flush delay and ~150 lines of complexity across `actor.rs`, `batcher.rs`, `PublisherConfig`, and 10+ test files for marginal throughput gain in Octane concurrent scenarios.

The `publishBatch` path (FFI + `ClientPool::publish_batch`) is separate and remains — it reduces FFI crossings and enables enqueue-all-then-wait-all semantics. Only the actor-internal batcher is removed.

## Decision

Remove the batcher. Publish each message immediately upon receipt in the Ready phase. Keep `publishBatch` (FFI + ClientPool).

## Changes

### 1. Delete `crates/rabbit-rs-core/src/publisher/batcher.rs`

Remove the file. Remove `pub mod batcher;` from `publisher/mod.rs`.

### 2. `PublisherConfig` (`publisher/mod.rs`)

Remove fields: `max_messages`, `max_bytes`, `flush_interval`.

Keep: `buffer_capacity`, `confirm_timeout`, `confirms`, `mandatory`.

New constructors:
```rust
pub const fn new(buffer_capacity: usize, confirm_timeout: Duration) -> Self
pub const fn with_flags(buffer_capacity: usize, confirm_timeout: Duration, confirms: bool, mandatory: bool) -> Self
```

### 3. `PublisherActor` (`publisher/actor.rs`)

**ActorState**: remove `batch: Batcher<RetainedPublish>` and `flush_deadline: Option<time::Instant>`.

**accept_publish Ready branch**: publish immediately instead of batching:
```rust
Phase::Ready => {
    let pending = VecDeque::from([retained]);
    publish_queue(state, pending).await;
}
```

**Delete**: `flush_batch()`, `flush_interval()`.

**next_deadline Ready branch**: return `None` (no flush deadline).

**suspend()**: remove `self.replay.extend(self.batch.take())` — no batch to drain.

**fail_all()**: remove `self.batch.take()` loop — no batch to fail.

**run_actor select!**: `wait_for_deadline` branch in Ready becomes dead code (next_deadline returns None). Keep the branch for Suspended (expire_replay) but Ready becomes a no-op.

### 4. `client.rs`

Remove constants `DEFAULT_MAX_MESSAGES` and `DEFAULT_MAX_BYTES`.

Update `publisher_config()`:
```rust
fn publisher_config(config: &ValidatedConfig) -> PublisherConfig {
    let publisher = config.publisher();
    PublisherConfig::with_flags(
        DEFAULT_BUFFER_CAPACITY,
        publisher.confirm_timeout,
        publisher.confirms,
        publisher.mandatory,
    )
}
```

### 5. `crates/rabbit-rs-php/src/testing.rs`

Update `PublisherConfig::new(...)` call (line 105-111):
```rust
let publisher_config = PublisherConfig::new(
    scenario.publisher_capacity,
    Duration::from_secs(30),
);
```

### 6. Test files — all `PublisherConfig::new(...)` and `with_flags(...)` calls

| File | Current signature | New signature |
|------|-------------------|---------------|
| `tests/publisher_safety.rs` | `config(max_messages, max_bytes)` → `new(max_messages, max_bytes, Duration::from_millis(1), 32, Duration::from_secs(5))` | `config()` → `new(32, Duration::from_secs(5))` |
| `tests/publisher_safety.rs` | `with_flags(max_messages, max_bytes, flush, cap, timeout, confirms, mandatory)` | `with_flags(cap, timeout, confirms, mandatory)` |
| `tests/publisher_recovery.rs` | `config(capacity)` → `new(capacity, 1_024, Duration::from_millis(1), 8, Duration::from_secs(5))` | `config(capacity)` → `new(capacity, Duration::from_secs(5))` |
| `tests/publisher_recovery.rs` | line 196 `new(10, 1_024, Duration::from_secs(1), 8, Duration::from_secs(5))` | `new(8, Duration::from_secs(5))` |
| `tests/publisher_delay.rs` | `publisher_config()` → `new(32, 1_024_000, Duration::from_millis(1), 32, Duration::from_secs(5))` | `publisher_config()` → `new(32, Duration::from_secs(5))` |
| `tests/recovery_coordinator.rs` | `publisher_config()` → `new(...)` 5 params | `publisher_config()` → `new(32, Duration::from_secs(5))` |
| `tests/metrics_snapshot.rs` | 3 calls to `new(...)` 5 params | 3 calls to `new(capacity, Duration::from_secs(5))` |
| `tests/delivery_attempts.rs` | 1 call to `new(...)` 5 params | 1 call to `new(32, Duration::from_secs(5))` |
| `tests/consumer_semantics.rs` | 2 calls: `new(32, 1_024, Duration::from_millis(1), 64, timeout)` and `new(1, 1_024, Duration::from_millis(1), 8, timeout)` | `new(64, timeout)` and `new(8, timeout)` |
| `benches/batching.rs` | `with_flags(batch_size, 2*1024*1024, Duration::from_millis(1), 1024, ...)` | `with_flags(1024, Duration::from_secs(30), confirms, true)` — remove batch_size dimension from config |
| `benches/publisher_actor.rs` | `with_flags(batch_size, 2*1024*1024, Duration::from_millis(1), 1024, ...)` | `with_flags(1024, Duration::from_secs(30), true, true)` |

**Delete tests**: `flushes_when_max_messages_is_reached` and `flushes_when_max_bytes_is_reached` in `publisher_safety.rs` — they test batcher-specific behavior that no longer exists.

**Update tests**: any test that relied on a 1ms flush delay now sees immediate publish. The mock transport's `wait_for_publish_count` should still work since `publish_queue` is called immediately.

### 7. Laravel package — no changes

- `config/rabbit-rs.php`: no batch-related config exists
- `ConfigNormalizer.php`: no batch-related normalization
- `RabbitMqQueue.php`: `bulk()` and `publishBatch` use the native `Pool::publishBatch()` which is unchanged
- `MessageMapper.php`: no batch references

### 8. Documentation — `docs/plans/2026-07-30-rabbitmq-native-design.md`

Update the "Publication" section:
- Remove "groups commands by destination and channel" (step 3)
- Remove from healthy defaults: "maximum batch of 256 messages or 1 MiB" and "flush at 1 ms"
- Keep "publisher buffer bounded to 8192 commands" (this is `buffer_capacity`, which stays)

## What does NOT change

| Feature | Why it's independent |
|---------|---------------------|
| Publisher confirms | Per-message sequence ledger, generation-aware — no batcher dependency |
| Mandatory returns | Flag on each `basic_publish` — no batcher dependency |
| Replay buffer | `VecDeque<RetainedPublish>` in Suspended phase — no batcher dependency |
| Backpressure | `Semaphore(buffer_capacity)` + mpsc capacity — no batcher dependency |
| Generation-aware ACKs | Ledger generation check — no batcher dependency |
| Timeouts | Per-message deadline + `confirm_timeout` — no batcher dependency |
| publishBatch (FFI + ClientPool) | Enqueue-all-then-wait-all — no batcher dependency |
| Delay routing | `ensure_delay_topology` + `into_transport_request` — no batcher dependency |

## Behavioral change

Before: a single `push()` call added a message to the batcher; the actor waited up to 1ms (or until max_messages/max_bytes) before flushing to the channel.

After: a single `push()` call publishes to the channel immediately. The 1ms latency is eliminated. Throughput is identical for sequential publishes (AMQP 0-9-1 `basic_publish` is synchronous per message regardless).

For `publishBatch` with N messages: before, N messages were batched and flushed in one `publish_queue` call. After, each message triggers its own `publish_queue` as it arrives from the mpsc. The `FuturesUnordered` multiplexes confirms. Throughput is identical because `basic_publish` is sequential in both cases.
