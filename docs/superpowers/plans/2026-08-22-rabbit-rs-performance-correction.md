# Rabbit-rs Performance Correction Implementation Plan (v2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the performance gap between rabbit-rs and amqplib by integrating PR #7 optimizations (except consume callback), adding no_ack mode, batch ack via lock-free queue, and caching publisher handles.

**Architecture:** Four tracks — PR #7 Port (foundation optimizations), Config (no_ack prerequisite), Consume (no_ack + lock-free batch ack + tryNextBatch), Publish (cache handle). All retro-compatible. Consume callback from PR #7 is excluded (violates Zend safety constraint).

**Tech Stack:** Rust 1.96.0 (edition 2024), Lapin AMQP client, ext-ffi PHP extension (Zend), Tokio runtime, flume channels, crossbeam ArrayQueue, arc-swap

## Global Constraints

- `#![forbid(unsafe_code)]` — no unsafe Rust, never weaken this lint
- Do not retain Zend values, PHP objects, callbacks, or requests in Rust threads — **consume(callback) from PR #7 is excluded**
- Keep Lapin behind the `Transport` abstraction — no Lapin types cross module boundaries
- All queues, channels, in-flight work, and buffers must remain explicitly bounded
- Delivery tokens remain connection-generation-aware — stale ACKs rejected
- Recovery order stays deterministic: connection → channels → topology → QoS → consumers
- `tryNext()` and `next()` remain the primary consume API — `tryNextBatch()` is additive
- New config fields default to current behavior (`no_ack: false`, `safety: safe`)
- Run `rtk cargo fmt --all` after Rust edits, then focused tests, then full gate
- Test framework: Rust integration tests in `crates/rabbit-rs-core/tests/`, PHP Pest + PHPT in `crates/rabbit-rs-php/tests/`
- Mock transport for deterministic async tests — no real sleeps in unit tests

---

## Source Attribution

| Optimization | Source | Adaptation |
|-------------|--------|------------|
| Lock-free batch ack (ArrayQueue + drainer) | PR #7 commit `934f6d8` | Integrate as-is, add `no_ack` skip |
| `try_next()` sync fast path | PR #7 commit `23d759f` | Already present in current code |
| `try_ack()` sync fast path | PR #7 commit `adbbe65` | Already present in current code |
| Hot path bypass publish (`now_or_never` + ArcSwap) | PR #7 commit `3fd1588` | Integrate, wire for Safe+Unsafe modes |
| `SafetyMode` enum (Blind/Unsafe/Safe) | PR #7 commit `256f622` | Integrate config + effective_safety |
| Lapin tuning (`frame_max=1MB`, `worker_threads=1`) | PR #7 commit `348cf5a` | Integrate |
| `Arc<str>` for exchange/routing_key/message_id | PR #7 commit `fb0450e` | Integrate |
| Skip `reject_unknown_keys` in release | PR #7 commit `f386c2a` | Integrate |
| PHP publish buffer (auto-flush 64 msgs / 1ms) | PR #7 commit `822c6fd` | Integrate |
| Flush immediately when mpsc empty | PR #7 commit `cab7cdc` | Integrate |
| `IteratorAggregate` for foreach | PR #7 commit `1c6ed88` | Integrate (no callback, just iterator) |
| `Pool::__destruct` flush buffer | PR #7 commit `36eaef4` | Integrate |
| Batcher spare-vec | PR #7 commit `d391bfc` | Integrate |
| HashMap ConfirmLedger | PR #7 commit `80bb45c` | Integrate |
| Deferred header path formatting | PR #7 commit `a75e099` | Integrate |
| Async PublishPump for blind mode | PR #7 commit `2830998` | Integrate |
| Hot-swap PublishPump on recovery | PR #7 commit `36eaef4` | Integrate |
| **`no_ack` mode** | **Our plan** | New — not in PR #7 |
| **Cache publisher handle per broker** | **Our plan** | New — not in PR #7 |
| **`tryNextBatch()` polling** | **Our plan** | New — PR #7 used callback instead |
| ~~`consume(callback)` API~~ | ~~PR #7~~ | **EXCLUDED** — violates Zend constraint |

---

## File Structure

### Track 0: PR #7 Foundation Port

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/rabbit-rs-core/Cargo.toml` | Modify | Add `arc-swap`, `crossbeam-queue`, `flume 0.11` |
| `crates/rabbit-rs-core/src/config.rs` | Modify | `SafetyMode` enum, `PublisherConfigSection.safety`, `effective_safety()` |
| `crates/rabbit-rs-core/src/runtime.rs` | Modify | `worker_threads=1`, `frame_max` via URI |
| `crates/rabbit-rs-core/src/transport/lapin.rs` | Modify | `Arc<str>` for exchange/routing_key, move instead of clone |
| `crates/rabbit-rs-core/src/publisher/actor.rs` | Modify | Batch AMQP frames, flush when mpsc empty, hot path bypass |
| `crates/rabbit-rs-core/src/publisher/batcher.rs` | Modify | Spare-vec swap in `take()` |
| `crates/rabbit-rs-core/src/publisher/confirms.rs` | Modify | HashMap instead of BTreeMap, `with_capacity` |
| `crates/rabbit-rs-core/src/publisher/mod.rs` | Modify | `SafetyMode` in `PublisherConfig`, `try_publish_hot`, `try_publish_blind` |
| `crates/rabbit-rs-core/src/publisher/pump.rs` | Create | Async PublishPump for blind mode |
| `crates/rabbit-rs-core/src/client.rs` | Modify | Wire `safety` in `publish()` and `publish_batch()` |
| `crates/rabbit-rs-core/src/conversion.rs` | Modify | Skip `reject_unknown_keys` in release, deferred header formatting |
| `crates/rabbit-rs-core/src/consumer/actor.rs` | Modify | Lock-free ArrayQueue batch ack + background drainer |
| `crates/rabbit-rs-core/src/consumer/delivery.rs` | Modify | `AckQueue`, `PendingAck`, `try_ack()` fast path |
| `crates/rabbit-rs-core/src/consumer/set.rs` | Modify | `ConsumerHandle` with flume buffer + background pump |
| `crates/rabbit-rs-php/src/classes/consumer.rs` | Modify | `try_ack()` sync, `IteratorAggregate`, `ConsumerIterator` |
| `crates/rabbit-rs-php/src/classes/pool.rs` | Modify | PHP publish buffer, `__destruct` flush, `flush()` |
| `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` | Modify | New methods, `ConsumerIterator` class |

### Track 1: Config (no_ack prerequisite)

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/rabbit-rs-core/src/config.rs` | Modify | Add `no_ack` to `SubscriptionConfig` |
| `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs` | Modify | Pass `no_ack` into `ConsumerRequest` |

### Track 2: Consume (no_ack + tryNextBatch)

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/rabbit-rs-core/src/transport.rs` | Modify | Add `no_ack` to `ConsumerRequest` |
| `crates/rabbit-rs-core/src/transport/lapin.rs` | Modify | Pass `no_ack` to `BasicConsumeOptions` |
| `crates/rabbit-rs-core/src/transport/mock.rs` | Modify | Track `no_ack` in mock |
| `crates/rabbit-rs-core/src/consumer/set.rs` | Modify | `Subscription` carries `no_ack`; `try_next_batch()` |
| `crates/rabbit-rs-core/src/consumer/actor.rs` | Modify | no-op settlement when `no_ack` |
| `crates/rabbit-rs-core/src/consumer/delivery.rs` | Modify | `Delivery` without token when `no_ack` |
| `crates/rabbit-rs-php/src/classes/consumer.rs` | Modify | `tryNextBatch()`, no-op ack when `no_ack` |
| `crates/rabbit-rs-php/src/classes/delivery.rs` | Modify | `deliveryTag()`, no-op ack when `no_ack` |
| `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` | Modify | New method stubs |

### Track 3: Publish (cache handle)

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/rabbit-rs-core/src/client.rs` | Modify | Group by broker in `publish_batch()` |

### Benchmark update

| File | Action | Responsibility |
|------|--------|----------------|
| `benchmarks/src/Drivers/RabbitRsDriver.php` | Modify | Use `no_ack`, `tryNextBatch()`, tuned prefetch/max_in_flight |

---

## Track 0: PR #7 Foundation Port

### Task 0a: Add dependencies and Lapin tuning

**Files:**
- Modify: `crates/rabbit-rs-core/Cargo.toml`
- Modify: `crates/rabbit-rs-core/src/runtime.rs`
- Test: `crates/rabbit-rs-core/tests/transport_tuning.rs`

**Interfaces:**
- Produces: `arc-swap`, `crossbeam-queue`, `flume 0.11` deps; `worker_threads=1`; `frame_max=1MB` in URI

- [ ] **Step 1: Add Cargo dependencies**

In `crates/rabbit-rs-core/Cargo.toml`, add:
```toml
arc-swap = "1"
crossbeam-queue = "0.3"
flume = "0.11"
```

- [ ] **Step 2: Set `worker_threads=1` in RuntimeRegistry**

In `crates/rabbit-rs-core/src/runtime.rs`, modify the Tokio runtime builder to use `worker_threads(1)` to reduce thread contention for I/O-bound AMQP workloads.

- [ ] **Step 3: Set `frame_max=1MB` via URI**

In the Lapin connection setup, append `?frame_max=1048576` to the AMQP URI (or pass via `ConnectionProperties`).

- [ ] **Step 4: Write test verifying frame_max and worker_threads**

In `crates/rabbit-rs-core/tests/transport_tuning.rs`, verify the runtime uses 1 worker thread and the URI contains frame_max.

- [ ] **Step 5: Run tests**

Run: `rtk cargo test -p rabbit-rs-core --test transport_tuning`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/rabbit-rs-core/Cargo.toml crates/rabbit-rs-core/src/runtime.rs crates/rabbit-rs-core/tests/transport_tuning.rs
git commit -m "perf(transport): tune frame_max to 1MB and worker_threads to 1"
```

---

### Task 0b: SafetyMode enum and publisher config

**Files:**
- Modify: `crates/rabbit-rs-core/src/config.rs`
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs`
- Modify: `crates/rabbit-rs-core/src/client.rs`
- Test: `crates/rabbit-rs-core/tests/publisher_safety.rs`

**Interfaces:**
- Produces: `SafetyMode` enum (Blind/Unsafe/Safe), `PublisherConfigSection.safety`, `effective_safety()`

- [ ] **Step 1: Write failing test for SafetyMode**

```rust
#[test]
fn safety_mode_defaults_to_safe() {
    assert_eq!(SafetyMode::default(), SafetyMode::Safe);
}

#[test]
fn effective_safety_derives_from_legacy_confirms() {
    let pub_section = PublisherConfigSection { confirms: false, ..Default::default() };
    assert_eq!(pub_section.effective_safety(), SafetyMode::Unsafe);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test publisher_safety safety_mode`
Expected: FAIL — `SafetyMode` doesn't exist

- [ ] **Step 3: Add `SafetyMode` enum to config.rs**

```rust
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SafetyMode {
    Blind,
    Unsafe,
    #[default]
    Safe,
}
```

Add `safety: SafetyMode` to `PublisherConfigSection` (default `Safe`). Add `effective_safety()` method that derives from legacy `confirms`/`mandatory` when `safety` was not explicitly set.

- [ ] **Step 4: Wire SafetyMode in PublisherConfig and ClientPool**

In `publisher/mod.rs`, add `safety: SafetyMode` to `PublisherConfig`. Replace `confirms: bool, mandatory: bool` with `safety: SafetyMode` internally.

In `client.rs`, use `effective_safety()` to determine `try_publish_hot` vs `try_publish_blind`.

- [ ] **Step 5: Run tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher_safety`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/rabbit-rs-core/src/config.rs crates/rabbit-rs-core/src/publisher/mod.rs crates/rabbit-rs-core/src/client.rs crates/rabbit-rs-core/tests/publisher_safety.rs
git commit -m "feat(config): add SafetyMode enum with backward-compatible config"
```

---

### Task 0c: Publisher optimizations (batch frames, hot path, spare-vec, HashMap ledger, Arc<str>)

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs`
- Modify: `crates/rabbit-rs-core/src/publisher/batcher.rs`
- Modify: `crates/rabbit-rs-core/src/publisher/confirms.rs`
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs`
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs`
- Test: `crates/rabbit-rs-core/tests/publisher_safety.rs`

**Interfaces:**
- Produces: `try_publish_hot()`, `try_publish_blind()`, batched AMQP frames, spare-vec batcher, HashMap ConfirmLedger, `Arc<str>` publish fields

- [ ] **Step 1: Write failing test for batched frames**

```rust
#[tokio::test]
async fn batch_publish_sends_all_frames_before_awaiting_confirm() {
    // Mock transport that delays confirmations
    // Publish 5 messages
    // Verify all 5 basic_publish called before any confirm resolved
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test publisher_safety batch_publish`
Expected: FAIL

- [ ] **Step 3: Batch AMQP frames in publish_queue()**

In `publisher/actor.rs`, modify `publish_queue()` to send all `basic_publish` calls without awaiting between them. Collect `PublisherConfirm` handles, then push their `wait()` futures to `FuturesUnordered`.

- [ ] **Step 4: Add hot path bypass (try_publish_hot)**

In `publisher/mod.rs`, add `try_publish_hot()` that uses `now_or_never()` + `ArcSwap` to publish directly to the transport channel without crossing the actor mpsc. Return `Ambiguous` when confirmation is pending (prevents duplicate via cold path).

- [ ] **Step 5: Add spare-vec to Batcher**

In `publisher/batcher.rs`, modify `take()` to swap the internal Vec with a pre-allocated spare, avoiding reallocation on the next batch fill.

- [ ] **Step 6: Replace BTreeMap with HashMap in ConfirmLedger**

In `publisher/confirms.rs`, replace `BTreeMap<u64, T>` with `HashMap<u64, T>`. Add `with_capacity(max_messages)` constructor. Fix `drain()` to sort by sequence number for deterministic recovery order.

- [ ] **Step 7: Use Arc<str> for exchange/routing_key/message_id**

In `transport/lapin.rs` and `publisher/mod.rs`, change `PublishRequest` fields from `String` to `Arc<str>` to avoid per-message clones. Move instead of clone in Lapin publish.

- [ ] **Step 8: Flush immediately when mpsc is empty**

In `publisher/actor.rs`, after processing an explicit `Command::Publish`, check if the mpsc channel is empty. If so, flush immediately instead of waiting for the 1ms timer.

- [ ] **Step 9: Run tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher_safety`
Expected: PASS

- [ ] **Step 10: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/ crates/rabbit-rs-core/src/transport/lapin.rs crates/rabbit-rs-core/tests/publisher_safety.rs
git commit -m "perf(publisher): batch AMQP frames, hot path bypass, spare-vec, HashMap ledger, Arc<str>"
```

---

### Task 0d: Async PublishPump for blind mode

**Files:**
- Create: `crates/rabbit-rs-core/src/publisher/pump.rs`
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs`
- Modify: `crates/rabbit-rs-core/src/client.rs`
- Test: `crates/rabbit-rs-core/tests/publisher_safety.rs`

- [ ] **Step 1: Write failing test for blind mode pump**

```rust
#[tokio::test]
async fn blind_mode_pump_publishes_without_confirmation() {
    // Configure SafetyMode::Blind
    // Publish 10 messages
    // Verify all 10 reach the transport without waiting for confirms
}
```

- [ ] **Step 2: Create PublishPump**

In `crates/rabbit-rs-core/src/publisher/pump.rs`, create an async pump that uses a flume channel + background task. The pump bypasses the actor entirely for blind mode. On recovery, hot-swap the channel via `ArcSwapOption` so blind publishes go to the new channel.

- [ ] **Step 3: Wire blind mode in ClientPool**

In `client.rs`, when `safety == Blind`, route through `try_publish_blind()` → PublishPump.

- [ ] **Step 4: Run tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher_safety`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/pump.rs crates/rabbit-rs-core/src/publisher/mod.rs crates/rabbit-rs-core/src/client.rs crates/rabbit-rs-core/tests/publisher_safety.rs
git commit -m "feat(publisher): async pump for blind fire-and-forget mode"
```

---

### Task 0e: Lock-free batch ack (ArrayQueue + background drainer)

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/delivery.rs`
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs`
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs`
- Modify: `crates/rabbit-rs-php/src/classes/delivery.rs`
- Test: `crates/rabbit-rs-core/tests/consumer_buffer.rs`

**Interfaces:**
- Produces: `AckQueue` (crossbeam ArrayQueue), `PendingAck`, `try_ack()` lock-free fast path, background drainer with `multiple=true`

- [ ] **Step 1: Write failing test for batch ack**

```rust
#[tokio::test]
async fn lock_free_batch_ack_coalesces_deliveries() {
    // Consume 5 deliveries
    // Call try_ack() on each — should be instant (no block_on)
    // Verify background drainer sends 1 channel.ack(5, multiple=true) after 1ms
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer_buffer lock_free_batch_ack`
Expected: FAIL

- [ ] **Step 3: Add AckQueue and PendingAck**

In `consumer/delivery.rs`, add:
- `PendingAck { delivery_tag: u64, generation: u64 }`
- `AckQueue` — wraps `crossbeam_queue::ArrayQueue<PendingAck>` with `Arc<AtomicU64>` for generation check
- `try_ack()` — pushes to ArrayQueue, returns `Ok(())` immediately (no mpsc, no oneshot, no async)

- [ ] **Step 4: Add background drainer to actor**

In `consumer/actor.rs`:
- Actor loop uses `tokio::time::timeout(1ms, recv())` instead of `select!` with interval
- Every 1ms, drain the `AckQueue`: group by subscription, coalesce contiguous delivery-tag runs, send one `channel.ack(highest, true)` per run
- Stale generation tags are discarded at drain time
- Release and Reject remain through the mpsc + oneshot path (they're rare)

- [ ] **Step 5: Add try_ack() to PHP Delivery**

In `crates/rabbit-rs-php/src/classes/delivery.rs`, `ack()` calls `try_ack()` on the native delivery — synchronous, no `block_on`.

- [ ] **Step 6: Run tests**

Run: `rtk cargo test -p rabbit-rs-core --test consumer_buffer`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/rabbit-rs-core/src/consumer/ crates/rabbit-rs-php/src/classes/delivery.rs crates/rabbit-rs-core/tests/consumer_buffer.rs
git commit -m "perf(consumer): lock-free batch acks with ArrayQueue and multiple=true"
```

---

### Task 0f: PHP-side optimizations (publish buffer, IteratorAggregate, __destruct)

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs`
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs`
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`
- Test: `crates/rabbit-rs-php/tests/`

- [ ] **Step 1: Add PHP publish buffer to Pool**

In `crates/rabbit-rs-php/src/classes/pool.rs`:
- Internal buffer array, auto-flushes at 64 messages or 1ms via `publishBatch()`
- Add `flush()` method to explicitly flush
- Add `__destruct` to flush on GC

- [ ] **Step 2: Add IteratorAggregate to Consumer**

In `crates/rabbit-rs-php/src/classes/consumer.rs`:
- Implement `IteratorAggregate` on `Consumer`
- Create `ConsumerIterator` class (registered before Consumer in module)
- Iterator uses `try_next()` fast path, `next(1000)` slow path
- `ConsumerIterator::__destruct` does NOT close the shared `ConsumerHandle`

- [ ] **Step 3: Update stub**

In `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`:
- Add `ConsumerIterator` class
- Add `Pool::flush()` method
- Add `Consumer::getIterator()` method

- [ ] **Step 4: Run tests**

Run: `php -d xdebug.mode=off -l crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`
Expected: No syntax errors

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-php/src/classes/pool.rs crates/rabbit-rs-php/src/classes/consumer.rs crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
git commit -m "feat(php): publish buffer, IteratorAggregate, Pool::__destruct flush"
```

---

### Task 0g: FFI optimizations (skip validation in release, deferred formatting)

**Files:**
- Modify: `crates/rabbit-rs-php/src/conversion.rs`

- [ ] **Step 1: Skip reject_unknown_keys in release**

In `crates/rabbit-rs-php/src/conversion.rs`, add `validate_keys` parameter to `publish_with_budget()`. When `false` (release builds via `cfg!(debug_assertions)`), skip the O(n) key validation scan. Debug builds retain the check.

- [ ] **Step 2: Defer header path formatting to error branches**

Pass `parent_path` and `key` separately through `header_value`, `add_headers`, and `add_header_bytes` so `format!` is only called inside `Err` branches. No `format!` on the success path.

- [ ] **Step 3: Run tests**

Run: `rtk cargo test -p rabbit-rs-core`
Run: `rtk cargo clippy -p rabbit-rs-php -- -D warnings`
Expected: All pass

- [ ] **Step 4: Commit**

```bash
git add crates/rabbit-rs-php/src/conversion.rs
git commit -m "perf(ffi): skip key validation in release, defer header path formatting to errors"
```

---

## Track 1: Config (no_ack prerequisite)

### Task 1: Add `no_ack` to `SubscriptionConfig`

**Files:**
- Modify: `crates/rabbit-rs-core/src/config.rs`
- Test: `crates/rabbit-rs-core/tests/consumer_safety.rs`

**Interfaces:**
- Produces: `SubscriptionConfig.no_ack: bool` (default `false`)

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn subscription_config_supports_no_ack() {
    let config = SubscriptionConfig {
        queue: "test".into(),
        prefetch: 500,
        no_ack: true,
        ..Default::default()
    };
    assert!(config.no_ack);
}
```

- [ ] **Step 2: Run to verify fail**

Run: `rtk cargo test -p rabbit-rs-core --test consumer_safety subscription_config_supports_no_ack`
Expected: FAIL

- [ ] **Step 3: Add field**

In `crates/rabbit-rs-core/src/config.rs`, add to `SubscriptionConfig`:
```rust
#[serde(default)]
pub no_ack: bool,
```
Default: `false`.

- [ ] **Step 4: Run to verify pass**

Run: `rtk cargo test -p rabbit-rs-core --test consumer_safety subscription_config_supports_no_ack`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core/src/config.rs crates/rabbit-rs-core/tests/consumer_safety.rs
git commit -m "feat(core): add no_ack field to SubscriptionConfig"
```

---

### Task 2: Pass `no_ack` through `ConsumerRequest`

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport.rs`
- Modify: `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs`
- Modify: `crates/rabbit-rs-core/src/transport/mock.rs`
- Test: `crates/rabbit-rs-core/tests/consumer_safety.rs`

**Interfaces:**
- Consumes: `SubscriptionConfig.no_ack` from Task 1
- Produces: `ConsumerRequest.no_ack: bool`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn consumer_request_carries_no_ack() {
    let req = ConsumerRequest {
        queue: "test".into(),
        consumer_tag: "tag".into(),
        exclusive: false,
        no_ack: true,
    };
    assert!(req.no_ack);
}
```

- [ ] **Step 2: Run to verify fail**

Run: `rtk cargo test -p rabbit-rs-core --test consumer_safety consumer_request_carries_no_ack`
Expected: FAIL

- [ ] **Step 3: Add field and update construction sites**

In `transport.rs`, add `pub no_ack: bool` to `ConsumerRequest`.
In `recovery_coordinator.rs`, pass `no_ack: subscription.no_ack`.
In `transport/mock.rs`, update mock to include `no_ack: false` (default).

- [ ] **Step 4: Run to verify pass**

Run: `rtk cargo test -p rabbit-rs-core --test consumer_safety consumer_request_carries_no_ack`
Expected: PASS

- [ ] **Step 5: Run full suite**

Run: `rtk cargo test -p rabbit-rs-core`
Expected: All pass

- [ ] **Step 6: Commit**

```bash
git add crates/rabbit-rs-core/src/transport.rs crates/rabbit-rs-core/src/pool/recovery_coordinator.rs crates/rabbit-rs-core/src/transport/mock.rs crates/rabbit-rs-core/tests/consumer_safety.rs
git commit -m "feat(core): pass no_ack through ConsumerRequest"
```

---

## Track 2: Consume (no_ack + tryNextBatch)

### Task 3: Lapin passes `no_ack` to `basic_consume`

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs`
- Modify: `crates/rabbit-rs-core/src/transport/mock.rs`
- Test: `crates/rabbit-rs-core/tests/consumer_safety.rs`

- [ ] **Step 1: Write failing test**

```rust
#[tokio::test]
async fn lapin_consumer_no_ack_flag() {
    // Mock transport verify no_ack=true passed to BasicConsumeOptions
}
```

- [ ] **Step 2: Run to verify fail**

- [ ] **Step 3: Update LapinConsumerChannel::consume()**

Change `no_ack: false` to `no_ack: request.no_ack`.

- [ ] **Step 4: Run to verify pass**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(transport): pass no_ack to basic_consume"
```

---

### Task 4: `Subscription` carries `no_ack`

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs`
- Test: `crates/rabbit-rs-core/tests/consumer_safety.rs`

- [ ] **Step 1: Write failing test**

```rust
#[tokio::test]
async fn subscription_carries_no_ack() {
    let sub = Subscription::new("sub", "key", "queue", channel)
        .no_ack(true);
    assert!(sub.no_ack);
}
```

- [ ] **Step 2: Run to verify fail**

- [ ] **Step 3: Add `no_ack` to `Subscription` + builder + pass to `ConsumerRequest` in `spawn()`**

- [ ] **Step 4: Run to verify pass**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(consumer): Subscription carries no_ack flag"
```

---

### Task 5: Actor no-op settlement when `no_ack`

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs`
- Modify: `crates/rabbit-rs-core/src/consumer/delivery.rs`
- Test: `crates/rabbit-rs-core/tests/consumer_safety.rs`

- [ ] **Step 1: Write failing test**

```rust
#[tokio::test]
async fn no_ack_deliveries_skip_settlement() {
    // Consume in no_ack mode — deliveries arrive without tokens
    // try_ack() is a no-op, no error
    // in_flight not incremented
}
```

- [ ] **Step 2: Run to verify fail**

- [ ] **Step 3: Modify actor dispatch**

When `subscription.no_ack`:
- Create `Delivery` without `DeliveryToken`
- Do not increment `in_flight`
- `try_ack()` returns `Ok(())` immediately

- [ ] **Step 4: Run to verify pass**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(consumer): no-op settlement when no_ack mode"
```

---

### Task 6: `ConsumerHandle::try_next_batch()`

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs`
- Test: `crates/rabbit-rs-core/tests/consumer_safety.rs`

- [ ] **Step 1: Write failing test**

```rust
#[tokio::test]
async fn try_next_batch_drains_buffer() {
    // Deliver 5, try_next_batch(3) → 3, try_next_batch(3) → 2, try_next_batch(3) → 0
}
```

- [ ] **Step 2: Run to verify fail**

- [ ] **Step 3: Implement try_next_batch()**

```rust
pub fn try_next_batch(&self, max: usize) -> Vec<Delivery> {
    let mut results = Vec::with_capacity(max);
    for _ in 0..max {
        match self.buffer_rx.try_recv() {
            Ok(delivery) => results.push(delivery),
            Err(_) => break,
        }
    }
    if !results.is_empty() {
        self.dispatch_notify.notify_one();
    }
    results
}
```

- [ ] **Step 4: Run to verify pass**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(consumer): add try_next_batch to ConsumerHandle"
```

---

### Task 7: PHP `tryNextBatch()`, `Delivery::deliveryTag()`, no-op ack

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs`
- Modify: `crates/rabbit-rs-php/src/classes/delivery.rs`
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`
- Test: `crates/rabbit-rs-php/tests/`

- [ ] **Step 1: Write failing PHPT tests**

Test that `Consumer::tryNextBatch()` and `Delivery::deliveryTag()` exist via reflection.

- [ ] **Step 2: Run to verify fail**

- [ ] **Step 3: Implement `tryNextBatch()` in PHP extension**

```rust
pub fn try_next_batch(&self, max: i64) -> PhpResult<Vec<Delivery>> {
    let handle = self.handle.as_ref().ok_or_else(|| ...)?;
    let max = max.max(1) as usize;
    Ok(handle.try_next_batch(max).into_iter().map(|d| Delivery::from_native(d, handle)).collect())
}
```

- [ ] **Step 4: Implement `deliveryTag()` in Delivery**

```rust
pub fn delivery_tag(&self) -> PhpResult<u64> {
    Ok(self.inner.delivery_tag())
}
```

- [ ] **Step 5: Make `ack()` no-op when `no_ack`**

Store `no_ack: bool` on `Delivery`. When true, `ack()` returns `Ok(())` immediately.

- [ ] **Step 6: Update stub**

Add `tryNextBatch(int $max): array`, `deliveryTag(): int` to stubs.

- [ ] **Step 7: Run to verify pass**

- [ ] **Step 8: Commit**

```bash
git commit -m "feat(php): add tryNextBatch, deliveryTag, no-op ack in no_ack mode"
```

---

## Track 3: Publish (cache handle)

### Task 8: Cache publisher handle per broker in `publish_batch()`

**Files:**
- Modify: `crates/rabbit-rs-core/src/client.rs`
- Test: `crates/rabbit-rs-core/tests/publisher_safety.rs`

- [ ] **Step 1: Write failing test**

```rust
#[tokio::test]
async fn publish_batch_caches_publisher_per_broker() {
    // Publish 256 to same broker, verify 1 lookup
}
```

- [ ] **Step 2: Run to verify fail**

- [ ] **Step 3: Group by broker**

```rust
let mut by_broker: HashMap<String, Vec<PublishRequest>> = HashMap::new();
for (broker, request) in requests {
    by_broker.entry(broker).or_default().push(request);
}
for (broker, msgs) in by_broker {
    let publisher = self.publisher(&broker).await?;
    for msg in msgs {
        waiters.push(publisher.try_publish(msg)?);
    }
}
```

- [ ] **Step 4: Run to verify pass**

- [ ] **Step 5: Commit**

```bash
git commit -m "perf(publisher): cache publisher handle per broker in publish_batch"
```

---

## Benchmark Update

### Task 9: Update `RabbitRsDriver`

**Files:**
- Modify: `benchmarks/src/Drivers/RabbitRsDriver.php`

- [ ] **Step 1: Use `no_ack` config for AUTO_ACK scenario**

In `setUp()`, set `no_ack: true` when `scenarioMode === AUTO_ACK`.

- [ ] **Step 2: Use `tryNextBatch()` in consume loop**

```php
while ($consumed < $count) {
    $batch = $this->consumer->tryNextBatch(256);
    if (empty($batch)) {
        $delivery = $this->consumer->next(1000);
        if ($delivery === null) break;
        $batch = [$delivery];
    }
    foreach ($batch as $delivery) {
        // record latency...
        $delivery->ack(); // lock-free try_ack from Track 0
    }
    $consumed += count($batch);
}
```

- [ ] **Step 3: Tune prefetch and max_in_flight**

Set `prefetch=512`, `max_in_flight=2048` (from PR #7 benchmark config).

- [ ] **Step 4: Rebuild extension**

Run: `./scripts/install.sh --release --yes`

- [ ] **Step 5: Run benchmark**

Run: `php -d xdebug.mode=off benchmarks/src/run-benchmarks.php --driver=rabbit-rs 2>/dev/null`
Expected: All 3 scenarios pass, improved throughput

- [ ] **Step 6: Commit**

```bash
git commit -m "perf(bench): update RabbitRsDriver for tryNextBatch, no_ack, tuned prefetch"
```

---

## Final Verification

### Task 10: Full quality gate

- [ ] **Step 1: Rust quality gate**

Run: `rtk cargo fmt --all -- --check`
Run: `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`
Run: `rtk cargo test --workspace --all-targets`
Expected: All pass

- [ ] **Step 2: PHP lint**

Run: `php -d xdebug.mode=off -l crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`
Expected: No syntax errors

- [ ] **Step 3: Full benchmark**

Run: `php -d xdebug.mode=off benchmarks/src/run-benchmarks.php 2>/dev/null`
Expected: All 12 combos pass, 0 losses, rabbit-rs budget ALL PASS

- [ ] **Step 4: `scripts/check.sh`**

Run: `rtk ./scripts/check.sh`
Expected: All pass

---

## Expected Results

| Metric | Current | Target | Driver |
|--------|---------|--------|--------|
| Publish msg/s | 19k | 50-80k | Track 0 (batch + hot path + Arc<str>) + Track 3 (cache) |
| Consume msg/s (manual ack) | 16k | 25-35k | Track 0 (lock-free ack) + Track 2 (tryNextBatch) |
| Consume msg/s (no_ack) | 16k | 35-50k | Track 2 (no_ack) + Track 0 (try_ack) |
| Consume p99 | 646ms | <250ms | Fewer FFI calls, lock-free ack |
| Budget check | FAIL | ALL PASS | All tracks |
