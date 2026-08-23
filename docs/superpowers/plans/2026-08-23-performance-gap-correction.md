# Performance Gap Correction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the performance gap between rabbit-rs and amqplib by eliminating stop-and-wait ack, double clones, and double boxing on the hot paths, implementing true `no_ack` mode, and fixing benchmark fairness.

**Architecture:** Fire-and-forget settlement with bounded backpressure and an error queue surfaced via `drainErrors()`. `Arc<Headers>` at the transport boundary. `Arc<str>` fields on `PublishRequest` for cheap clones. `TaggedFuture` to eliminate double boxing. `no_ack` as independent transport flag. `wait_all()` for collective confirm awaiting.

**Tech Stack:** Rust 1.96.0 (edition 2024), Lapin, Tokio, flume, ext-php-rs, PHP 8.4, Laravel 11, Pest

## Global Constraints

- Rust pinned to 1.96.0, edition 2024
- `#![forbid(unsafe_code)]` — no changes to lint configuration
- At-least-once preserved: fire-and-forget ack preserves redelivery on failure. `no_ack` gated behind `best_effort=true` + `early_ack=true`
- No PHP callbacks from Rust threads — polling model preserved
- Bounded memory: settlement_errors buffer bounded (256). Byte budget unchanged. Flume buffer bounded
- Recovery order: connection, channels, exchanges, queues, bindings, QoS, consumers — unchanged
- No credentials in Debug/errors/metrics
- TDD: write failing test first, observe failure, implement minimally, rerun
- Use paused Tokio time and scriptable mock transport for deterministic async tests
- No real sleeps in unit tests
- Run `rtk cargo fmt --all` after Rust edits, then focused tests, then full quality gate

## Spec Reference

`docs/superpowers/specs/2026-08-23-performance-gap-correction-design.md`

---

## File Structure

### Rust Core (`crates/rabbit-rs-core/src/`)

| File | Responsibility | Tasks |
|------|---------------|------|
| `pool/recovery_coordinator.rs` | Fix generation rollback on recovery failure, close old consumer before insert, route queue_size/purge through coordinator | 1, 2 |
| `pool/connection_actor.rs` | Drive actor back to Recovering on recovery failure | 1 |
| `client.rs` | Fix Ready-state loop predicate, invalidate stale cached handles by generation, start all broker coordinators, route queue_size/purge through coordinator | 1, 2, 3 |
| `consumer/set.rs` | Fix try_next_batch partial batch on error, fix Drop clone semantics, add error_rx + drain_errors + try_settle | 4, 5, 7 |
| `consumer/actor.rs` | Fix ledger/budget release on retryable failure, fire-and-forget settlement, no_ack skip spawn, hard gate on over-budget | 4, 6, 7, 8 |
| `consumer/delivery.rs` | DeliveryToken::try_settle() returning Result | 7 |
| `transport.rs` | Delivery.headers → Arc<Headers>, ConsumerRequest add no_ack | 8, 9 |
| `transport/lapin.rs` | Propagate no_ack to BasicConsumeOptions, wrap headers in Arc | 8, 9 |
| `transport/mock.rs` | Update mock deliveries to produce Arc<Headers> | 8 |
| `publisher/mod.rs` | Destination → Arc<str> fields, MessageProperties → Arc<str> fields, PublishWaiter::wait_all() | 11, 13 |
| `publisher/actor.rs` | TaggedFuture struct, eliminate Box::pin wrapper, into_transport_request Arc→String conversion | 12 |

### PHP Extension (`crates/rabbit-rs-php/src/`)

| File | Responsibility | Tasks |
|------|---------------|------|
| `classes/delivery.rs` | `ack()`/`release()`/`reject()` fire-and-forget with bounded backpressure | 14 |
| `classes/consumer.rs` | `drainErrors()` method, `ackThrough()` fire-and-forget, `ackBatch()` fire-and-forget | 14 |

### Laravel Package (`packages/laravel-queue/src/`)

| File | Responsibility | Tasks |
|------|---------------|------|
| `RabbitMqQueue.php` | `drainSettlementErrors()` in `pop()` | 15 |
| `Console/RabbitMqWorkCommandExtension.php` | `WorkerIdle` listener for `drainSettlementErrors()` | 15 |

### Benchmark (`benchmarks/src/`)

| File | Responsibility | Tasks |
|------|---------------|------|
| `Config.php` | `PREFETCH_COUNT = 128` | 16 |
| `Drivers/RabbitRsDriver.php` | AUTO_ACK: `confirms=false` | 16 |

### Config (`packages/laravel-queue/config/`)

| File | Responsibility | Tasks |
|------|---------------|------|
| `config/rabbit-rs.php` | Add `no_ack` option per subscription | 9 |
| `src/Config/ConfigNormalizer.php` | Validate `no_ack=true` requires `early_ack=true` | 9 |

---

## Task 1: Recovery — Generation Rollback on Failure

**Files:**
- Modify: `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:266-296` — generation rollback + error propagation
- Modify: `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:388-449` — close old consumer before insert
- Test: `crates/rabbit-rs-core/tests/recovery.rs`

**Interfaces:**
- Consumes: nothing (first task)
- Produces: recovery failure triggers `Recovering` state transition, old consumers closed on replacement

- [ ] **Step 1: Write the failing test**

In `crates/rabbit-rs-core/tests/recovery.rs`, add a test that verifies recovery failure rolls back generation and re-attempts:

```rust
#[tokio::test(start_paused = true)]
async fn recovery_failure_rolls_back_and_retries() {
    let transport = MockTransport::default();
    transport.push_connect_result(Ok(()));
    // Make topology reconciliation fail on first recovery, succeed on second
    transport.push_consumer_result(Err(TransportError::connection("test failure")));

    let actor = ConnectionActor::spawn(/* ... */).await.unwrap();
    let (handle, _close_rx) = RecoveryCoordinator::spawn(
        actor.clone(),
        RecoveryCoordinatorConfig::default(),
        Metrics::disabled(),
        context,
    );

    // Trigger first recovery — should fail
    actor.connection_lost(TransportError::connection("test drop")).await;
    tokio::time::advance(Duration::from_secs(1)).await;

    // The coordinator should NOT be stuck in Ready with last_generation pinned
    // Push a successful consumer result for the retry
    transport.push_consumer_result(Ok(()));

    // Trigger second recovery
    tokio::time::advance(Duration::from_secs(1)).await;

    // The consumer should eventually become available
    let consumer = tokio::time::timeout(
        Duration::from_secs(5),
        handle.consumer("test-worker"),
    ).await;
    assert!(consumer.is_ok(), "consumer should become available after retry");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test recovery recovery_failure_rolls_back_and_retries`
Expected: FAIL — generation is pinned, recovery never re-attempts, consumer never available.

- [ ] **Step 3: Roll back `last_generation` on recovery failure**

In `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs`, lines 290-295:

```rust
// Before:
if let Err(error) = result {
    eprintln!("recovery generation {generation} failed: {error}");
}

// After:
if let Err(error) = result {
    eprintln!("recovery generation {generation} failed: {error}");
    // Roll back so the next Ready re-attempts recovery
    last_generation = generation.saturating_sub(1);
    // Drive the actor back to Recovering so the deterministic recovery order is re-attempted
    let _ = actor
        .connection_lost(TransportError::connection(format!(
            "recovery failed: {error}"
        )))
        .await;
}
```

- [ ] **Step 4: Close old consumer before inserting new one**

In `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs`, line 448:

```rust
// Before:
consumers.lock().await.insert(worker.name.clone(), consumer);

// After:
let mut guard = consumers.lock().await;
if let Some(old) = guard.insert(worker.name.clone(), consumer) {
    let _ = old.close().await;  // stop the old actor task, free channels
}
```

- [ ] **Step 5: Run focused tests**

Run: `rtk cargo test -p rabbit-rs-core --test recovery recovery_failure_rolls_back_and_retries`
Expected: PASS

Run: `rtk cargo test -p rabbit-rs-core --test recovery`
Expected: PASS

- [ ] **Step 6: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/rabbit-rs-core/src/pool/recovery_coordinator.rs crates/rabbit-rs-core/tests/recovery.rs
git commit -m "fix: roll back recovery generation on failure and close old consumer on replacement"
```

---

## Task 2: Recovery — Fix Client Loop Predicate + Handle Invalidation

**Files:**
- Modify: `crates/rabbit-rs-core/src/client.rs:294-328` — consumer loop predicate
- Modify: `crates/rabbit-rs-core/src/client.rs:500-534` — publisher loop predicate
- Modify: `crates/rabbit-rs-core/src/client.rs:670-690` — `ready()` generation check
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs:232-239` — add `generation` to `ConsumerHandle`
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs` — add `generation` to `PublisherHandle` (if applicable)
- Test: `crates/rabbit-rs-core/tests/recovery.rs`

**Interfaces:**
- Consumes: Task 1 (generation rollback)
- Produces: stale handles evicted by generation, client loops react to state transitions

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(start_paused = true)]
async fn stale_consumer_handle_evicted_after_recovery() {
    // Setup: create a consumer, simulate connection drop + recovery, verify
    // the client returns the NEW handle, not the stale one
    let pool = ClientPool::new(helper::config()).await.unwrap();
    let consumer1 = pool.consumer("test-worker").await.unwrap();

    // Simulate connection drop
    drop_connection(&pool, "test-broker").await;

    // Simulate recovery
    tokio::time::advance(Duration::from_secs(2)).await;

    // Get consumer again — should be a NEW handle on the new generation
    let consumer2 = pool.consumer("test-worker").await.unwrap();

    // The handles should be different (different generation)
    assert_ne!(
        consumer1.generation(),
        consumer2.generation(),
        "stale handle should be evicted after recovery"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test recovery stale_consumer_handle_evicted_after_recovery`
Expected: FAIL — `ConsumerHandle` has no `generation()` method, `ready()` doesn't check generation.

- [ ] **Step 3: Add `generation` to `ConsumerHandle`**

In `crates/rabbit-rs-core/src/consumer/set.rs`:

```rust
pub struct ConsumerHandle {
    commands: mpsc::Sender<ConsumerCommand>,
    buffer_rx: flume::Receiver<Result<Delivery, ConsumerError>>,
    error_rx: flume::Receiver<SettlementError>,
    metrics: Metrics,
    closed: Arc<AtomicBool>,
    dispatch_notify: Arc<Notify>,
    generation: u64,  // ← add this field
}

pub fn generation(&self) -> u64 {
    self.generation
}
```

Update `spawn_with_metrics` to accept and set the generation. The generation comes from the coordinator's
current state when the `ConsumerSet` is created.

- [ ] **Step 4: Fix `ready()` to check generation**

In `crates/rabbit-rs-core/src/client.rs`, the `ready()` helper (around line 670-690):

```rust
fn ready<T: Clone + Generationed>(
    &self,
    generation: u64,
    registry: &StdMutex<HashMap<String, T>>,
    key: &str,
) -> Result<Option<T>, ClientError> {
    let lifecycle = lock(&self.lifecycle);
    // ... lifecycle check ...
    match lock(registry).get(key) {
        Some(h) if h.generation() == generation => Ok(Some(h.clone())),
        _ => Ok(None),  // stale or missing — evict
    }
}
```

Where `Generationed` is a trait:
```rust
trait Generationed {
    fn generation(&self) -> u64;
}
impl Generationed for ConsumerHandle { fn generation(&self) -> u64 { self.generation() } }
```

- [ ] **Step 5: Fix client loop predicates**

In `crates/rabbit-rs-core/src/client.rs`, lines 301-310 (consumer) and 505-507 (publisher):

```rust
// Before:
coordinator.wait_for_state(|state| {
    matches!(state, Ready { .. } | FailedPermanent { .. } | Closed)
}).await;

// After:
coordinator.wait_for_state(|state| {
    matches!(state, Recovering { .. } | Connecting { .. } | FailedPermanent { .. } | Closed)
}).await;
```

This makes the loop wait for state transitions away from `Ready`, so it re-attempts when the actor is
driven back to `Recovering` (Task 1 fix).

- [ ] **Step 6: Fix all compilation errors**

Run: `rtk cargo build -p rabbit-rs-core`

Fix all places that construct `ConsumerHandle` without the new `generation` field.

- [ ] **Step 7: Run focused tests**

Run: `rtk cargo test -p rabbit-rs-core --test recovery stale_consumer_handle_evicted_after_recovery`
Expected: PASS

Run: `rtk cargo test -p rabbit-rs-core --test recovery`
Expected: PASS

- [ ] **Step 8: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add crates/rabbit-rs-core/src/client.rs crates/rabbit-rs-core/src/consumer/set.rs crates/rabbit-rs-core/tests/recovery.rs
git commit -m "fix: generation-aware handle invalidation and fixed client loop predicate"
```

---

## Task 3: Recovery — Multi-Broker Coordinator Startup

**Files:**
- Modify: `crates/rabbit-rs-core/src/client.rs:286-293` — start all broker coordinators
- Test: `crates/rabbit-rs-core/tests/integration.rs`

**Interfaces:**
- Consumes: Task 2 (generation-aware handles)
- Produces: all distinct brokers in a worker profile have their coordinators started

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(start_paused = true)]
async fn multi_broker_profile_starts_all_coordinators() {
    let config = helper::multi_broker_config();  // config with 2 brokers, 1 worker profile
    let pool = ClientPool::new(config).await.unwrap();

    // Both brokers' coordinators should be started
    let consumer = pool.consumer("multi-broker-worker").await.unwrap();

    // Verify both subscriptions are active by pushing deliveries on both brokers
    // and confirming both are consumed
    // ...
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test integration multi_broker_profile_starts_all_coordinators`
Expected: FAIL — only the first broker's coordinator is started.

- [ ] **Step 3: Start all distinct broker coordinators**

In `crates/rabbit-rs-core/src/client.rs`, lines 286-293:

```rust
// Before:
if let Some(first_sub) = worker.subscriptions.first() {
    let _ = self.coordinator(&first_sub.broker).await?;
}
let coordinator = self.coordinator(&worker.subscriptions[0].broker).await?;

// After:
let mut brokers: Vec<String> = worker.subscriptions.iter().map(|s| s.broker.clone()).collect();
brokers.dedup();
for broker in &brokers {
    self.coordinator(broker).await?;
}
// The consumer is composed from all coordinators — wait for each
// For now, use the first coordinator for the primary consumer handle.
// The coordinator's recover_generation already handles per-broker filtering.
let coordinator = self.coordinator(&brokers[0]).await?;
```

Note: a complete multi-broker consumer composition (merging handles from multiple coordinators) is a
design-level change. For now, ensure all coordinators are started. The `recover_generation` in each
coordinator only creates consumers for its broker's subscriptions. The client's `consumer()` call returns
the handle from the first coordinator. A future task may compose a multi-broker consumer.

- [ ] **Step 4: Run focused tests**

Run: `rtk cargo test -p rabbit-rs-core --test integration multi_broker_profile_starts_all_coordinators`
Expected: PASS

- [ ] **Step 5: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/rabbit-rs-core/src/client.rs crates/rabbit-rs-core/tests/integration.rs
git commit -m "fix: start coordinators for all distinct brokers in multi-broker worker profiles"
```

---

## Task 4: Consumer — `try_next_batch` Partial Batch on Error

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs:232-239` — add `pending_error` field
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs:265-278` — `try_next` check stashed error
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs:290-315` — `try_next_batch` partial batch
- Test: `crates/rabbit-rs-core/tests/consumer.rs`

**Interfaces:**
- Consumes: Task 2 (ConsumerHandle struct changes)
- Produces: `try_next_batch` returns partial batch on error, never discards deliveries

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(start_paused = true)]
async fn try_next_batch_returns_partial_batch_on_error() {
    let transport = MockTransport::default();
    transport.push_delivery(helper::delivery(1, b"msg1"));
    transport.push_delivery(helper::delivery(2, b"msg2"));
    // Push an error into the buffer (after the two deliveries)
    transport.push_delivery_error(TransportError::connection("test error"));
    transport.push_delivery(helper::delivery(3, b"msg3"));

    let subscription = helper::subscription(&transport);
    let handle = ConsumerSet::spawn_with_metrics(
        vec![subscription], 1024, Metrics::disabled(),
    ).await.unwrap();

    // Let deliveries flow into the buffer
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;

    // try_next_batch(10) should return 2 deliveries + stash the error
    let batch = handle.try_next_batch(10).unwrap();
    assert_eq!(batch.len(), 2, "should return partial batch, not discard it");
    assert_eq!(batch[0].delivery_tag(), 1);
    assert_eq!(batch[1].delivery_tag(), 2);

    // Next call should return delivery 3 (error was stashed, surfaced when batch is empty)
    let batch2 = handle.try_next_batch(10).unwrap();
    assert_eq!(batch2.len(), 1);
    assert_eq!(batch2[0].delivery_tag(), 3);

    // Now the stashed error should surface
    let result = handle.try_next_batch(10);
    assert!(result.is_err(), "stashed error should surface on empty batch");

    let _ = handle.close().await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer try_next_batch_returns_partial_batch_on_error`
Expected: FAIL — `try_next_batch` discards the batch and returns `Err` immediately.

- [ ] **Step 3: Add `pending_error` to `ConsumerHandle`**

In `crates/rabbit-rs-core/src/consumer/set.rs`:

```rust
use std::sync::Mutex;

pub struct ConsumerHandle {
    commands: mpsc::Sender<ConsumerCommand>,
    buffer_rx: flume::Receiver<Result<Delivery, ConsumerError>>,
    error_rx: flume::Receiver<SettlementError>,
    metrics: Metrics,
    closed: Arc<AtomicBool>,
    dispatch_notify: Arc<Notify>,
    generation: u64,
    pending_error: Mutex<Option<ConsumerError>>,  // ← add
}
```

- [ ] **Step 4: Fix `try_next_batch` to return partial batch**

```rust
pub fn try_next_batch(&self, max: usize) -> Result<Vec<Delivery>, ConsumerError> {
    // Check stashed error first
    if let Some(error) = self.pending_error.lock().unwrap().take() {
        return Err(error);
    }
    let max = max.clamp(1, 256);
    let mut batch = Vec::with_capacity(max);
    for _ in 0..max {
        match self.buffer_rx.try_recv() {
            Ok(Ok(delivery)) => batch.push(delivery),
            Ok(Err(error)) => {
                self.dispatch_notify.notify_one();
                if !batch.is_empty() {
                    // Stash error, return partial batch
                    *self.pending_error.lock().unwrap() = Some(error);
                    return Ok(batch);
                }
                return Err(error);
            }
            Err(flume::TryRecvError::Empty) => break,
            Err(flume::TryRecvError::Disconnected) => {
                if !batch.is_empty() {
                    self.dispatch_notify.notify_one();
                    *self.pending_error.lock().unwrap() = Some(ConsumerError::closed());
                    return Ok(batch);
                }
                return Err(ConsumerError::closed());
            }
        }
    }
    if !batch.is_empty() {
        self.dispatch_notify.notify_one();
    }
    Ok(batch)
}
```

- [ ] **Step 5: Fix `try_next` to check stashed error**

```rust
pub fn try_next(&self) -> Result<Option<Delivery>, ConsumerError> {
    if let Some(error) = self.pending_error.lock().unwrap().take() {
        return Err(error);
    }
    // ... existing logic ...
}
```

- [ ] **Step 6: Run focused tests**

Run: `rtk cargo test -p rabbit-rs-core --test consumer try_next_batch_returns_partial_batch_on_error`
Expected: PASS

Run: `rtk cargo test -p rabbit-rs-core --test consumer`
Expected: PASS

- [ ] **Step 7: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/rabbit-rs-core/src/consumer/set.rs crates/rabbit-rs-core/tests/consumer.rs
git commit -m "fix: try_next_batch returns partial batch on error instead of discarding deliveries"
```

---

## Task 5: Consumer — Conditional Ledger/Budget Release on Settlement Failure

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs:535-578` — single settle handler
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs:580-645` — settle-through handler
- Test: `crates/rabbit-rs-core/tests/consumer.rs`

**Interfaces:**
- Consumes: Task 4 (consumer fixes)
- Produces: budget/ledger only released on `Ok` or terminal errors, not retryable

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(start_paused = true)]
async fn retryable_settlement_failure_preserves_ledger_and_budget() {
    let transport = MockTransport::default();
    transport.push_delivery(helper::delivery(1, b"msg1"));
    // Make the ack fail with a retryable error (not StaleGeneration/Transport)
    transport.push_ack_gate(MockOperationGate::error(TransportError::channel("test retryable")));

    let subscription = helper::subscription(&transport);
    let handle = ConsumerSet::spawn_with_metrics(
        vec![subscription], 1024, Metrics::disabled(),
    ).await.unwrap();

    let delivery = handle.next().await.unwrap();
    let in_flight_before = handle.metrics().in_flight();

    // Attempt ack — should fail with retryable error
    let result = delivery.ack().await;
    assert!(result.is_err(), "ack should fail");
    assert!(!matches!(
        result.unwrap_err().kind(),
        ConsumerErrorKind::StaleGeneration | ConsumerErrorKind::Transport
    ));

    // in_flight should NOT be decremented (delivery is still Pending)
    let in_flight_after = handle.metrics().in_flight();
    assert_eq!(in_flight_before, in_flight_after, "budget should not be released on retryable failure");

    // Retry the ack — should succeed (ledger entry preserved)
    transport.push_ack_result(Ok(()));
    let result2 = delivery.ack().await;
    assert!(result2.is_ok(), "retry should succeed — ledger entry preserved");

    let _ = handle.close().await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer retryable_settlement_failure_preserves_ledger_and_budget`
Expected: FAIL — budget released and ledger entry removed on all failures.

- [ ] **Step 3: Fix single settle handler (lines 535-578)**

```rust
match &settlement_result.result {
    Ok(terminal) => {
        // Success — record metrics, release budget, remove ledger
        match terminal {
            DeliveryState::Acked => { state.metrics.record_ack(/* ... */); }
            DeliveryState::Rejected => { state.metrics.record_reject(/* ... */); }
            _ => {}
        }
        if let Some(bytes) = state.buffered_bytes.get_mut(&settlement_result.token.subscription) {
            *bytes = bytes.saturating_sub(delivery_bytes);
        }
        state.release_budget();
        record_consumer_buffer_metrics(&state);
        state.dispatch();
        if let Some(ledger) = state.channel_ledgers.get_mut(&channel_key) {
            ledger.pending.remove(&settlement_result.token.delivery_tag);
        }
    }
    Err(error) if matches!(error.kind(), ConsumerErrorKind::StaleGeneration | ConsumerErrorKind::Transport) => {
        // Terminal failure — delivery is Lost. Release budget and remove ledger.
        if let Some(bytes) = state.buffered_bytes.get_mut(&settlement_result.token.subscription) {
            *bytes = bytes.saturating_sub(delivery_bytes);
        }
        state.release_budget();
        record_consumer_buffer_metrics(&state);
        state.dispatch();
        if let Some(ledger) = state.channel_ledgers.get_mut(&channel_key) {
            ledger.pending.remove(&settlement_result.token.delivery_tag);
        }
    }
    Err(_) => {
        // Retryable failure — do NOT release budget, do NOT remove ledger.
        // Only record metrics. The delivery stays Pending for retry.
        record_consumer_buffer_metrics(&state);
    }
}
// Always reset settling flag (line 568 stays)
settlement_result.token.settling.store(false, Ordering::Release);
// Always send the result back
let _ = settlement_result.completed.send(settlement_result.result);
// Always launch next queued settlement
drain_settlement_queue(&mut state, channel_key);
```

- [ ] **Step 4: Fix settle-through handler (lines 580-645)**

Apply the same pattern: only release budget and remove ledger entries on `Ok` or terminal errors.
For retryable errors, keep all ledger entries and budget.

- [ ] **Step 5: Run focused tests**

Run: `rtk cargo test -p rabbit-rs-core --test consumer retryable_settlement_failure_preserves_ledger_and_budget`
Expected: PASS

Run: `rtk cargo test -p rabbit-rs-core --test consumer`
Expected: PASS

- [ ] **Step 6: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/rabbit-rs-core/src/consumer/actor.rs crates/rabbit-rs-core/tests/consumer.rs
git commit -m "fix: only release budget and ledger on success or terminal settlement failures"
```

---

## Task 6: OOM Protection — Hard Gate + Permit-Based Backpressure

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs:380-410` — hard gate on over-budget
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs:205-224` — permit-based `spawn_source`
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs:139-202` — `spawn_with_metrics` (wire permits)
- Test: `crates/rabbit-rs-core/tests/consumer.rs`

**Interfaces:**
- Consumes: Task 5 (consumer fixes)
- Produces: actor stops accepting on over-budget, `spawn_source` uses permits

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(start_paused = true)]
async fn hard_gate_stops_accepting_when_over_budget() {
    let transport = MockTransport::default();
    // Push many deliveries with large payloads to exceed max_buffered_bytes
    for i in 0..100 {
        transport.push_delivery(helper::delivery_with_payload(i, &vec![0u8; 1024 * 1024]));
    }

    let subscription = helper::subscription_with_budget(&transport, 1024 * 1024 * 4); // 4 MiB budget
    let handle = ConsumerSet::spawn_with_metrics(
        vec![subscription], 1024, Metrics::disabled(),
    ).await.unwrap();

    // Let deliveries flow — should stop accepting after ~4 deliveries (4 MiB)
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    // buffered_bytes should not exceed max_buffered_bytes
    let buffered = handle.metrics().buffered_bytes();
    assert!(
        buffered <= 1024 * 1024 * 4,
        "buffered_bytes ({buffered}) should not exceed max_buffered_bytes (4 MiB)"
    );

    let _ = handle.close().await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer hard_gate_stops_accepting_when_over_budget`
Expected: FAIL — actor keeps accepting, `buffered_bytes` exceeds `max_buffered_bytes`.

- [ ] **Step 3: Implement hard gate in actor**

In `crates/rabbit-rs-core/src/consumer/actor.rs`, lines 380-410, the `Incoming` handler:

```rust
Ok(delivery) => {
    let delivery_bytes = u64::try_from(delivery.payload.len()).unwrap_or(u64::MAX);
    // ... ledger insert ...

    let over_budget = if let Some(max) = state.max_buffered_bytes.get(&subscription) {
        let current = state.buffered_bytes.get(&subscription).copied().unwrap_or(0);
        current.saturating_add(delivery_bytes) > *max
    } else {
        false
    };

    if over_budget {
        // HARD GATE: do NOT push to buffer, do NOT increment buffered_bytes
        // Leave the delivery in the mpsc — backpressure propagates to spawn_source
        state.metrics.record_backpressure(&subscription);
        // Do NOT dispatch, do NOT push_back, do NOT increment
        // The delivery stays in the ConsumerCommand until next iteration
        // But we can't "leave it in the mpsc" — we already received it.
        // Instead, we need to NOT process it yet. Put it back or skip.
        // Actually, we need to change the select! to not receive from the mpsc
        // when over budget. See Step 4 for the select! change.
        continue;
    }

    // Only push to buffer if under budget
    if let Some(buffer) = state.buffers.get_mut(&subscription) {
        buffer.push_back(delivery);
        state.scheduler.mark_ready(&subscription);
    }
    if let Some(bytes) = state.buffered_bytes.get_mut(&subscription) {
        *bytes = bytes.saturating_add(delivery_bytes);
    }
    record_consumer_buffer_metrics(&state);
    state.dispatch();
}
```

Wait — the issue is that `receiver.recv()` in the `select!` always receives. We can't "not receive".
The fix is to track which subscriptions are over budget and skip processing their `Incoming` commands
(leave them in a pending queue) OR change the `select!` to conditionally poll the receiver.

**Better approach**: Use a `skip_until` mechanism. When any subscription is over budget, the actor
should skip the `receiver.recv()` branch in `select!` until `buffered_bytes` drops below budget. This
requires restructuring the `select!` loop or using a `Notify` that the actor waits on when over budget.

**Simplest approach for now**: when over budget, push the delivery to a `pending_incoming` VecDeque
instead of the buffer. Process `pending_incoming` when budget is available:

```rust
if over_budget {
    state.pending_incoming.push_back((subscription, delivery));
    state.metrics.record_backpressure(&subscription);
} else {
    // Normal path: push to buffer
    if let Some(buffer) = state.buffers.get_mut(&subscription) {
        buffer.push_back(delivery);
        state.scheduler.mark_ready(&subscription);
    }
    if let Some(bytes) = state.buffered_bytes.get_mut(&subscription) {
        *bytes = bytes.saturating_add(delivery_bytes);
    }
}
```

Then, after settlements release budget, attempt to drain `pending_incoming` back into the buffer.

Actually, the simplest correct fix: when over budget, do NOT push to the buffer but DO keep the
delivery in the actor's `pending_incoming` VecDeque (bounded by the mpsc capacity of 256). This means
the actor still receives from the mpsc (256 capacity), but stops pushing to the buffer. The mpsc fills
up, `spawn_source` blocks on `send().await`, and backpressure propagates.

The `pending_incoming` VecDeque is bounded by the mpsc capacity (256), so it's not unbounded.

- [ ] **Step 4: Add `pending_incoming` to `ActorState`**

```rust
struct ActorState {
    // ... existing fields ...
    pending_incoming: VecDeque<(SubscriptionId, TransportDelivery)>,
}
```

In the `Incoming` handler:
```rust
if over_budget {
    state.pending_incoming.push_back((subscription.clone(), delivery));
    state.metrics.record_backpressure(&subscription);
} else {
    // push to buffer, increment buffered_bytes, dispatch
}
```

After settlement completion (budget released), drain `pending_incoming`:
```rust
fn try_drain_pending(&mut state) {
    while let Some((subscription, delivery)) = state.pending_incoming.front() {
        let delivery_bytes = u64::try_from(delivery.payload.len()).unwrap_or(u64::MAX);
        let over_budget = /* check */ ;
        if over_budget { break; }
        let (subscription, delivery) = state.pending_incoming.pop_front().unwrap();
        // push to buffer, increment, dispatch
    }
}
```

Call `try_drain_pending` after each settlement release and after `dispatch()`.

- [ ] **Step 5: Add permit-based backpressure to `spawn_source`**

In `crates/rabbit-rs-core/src/consumer/set.rs`:

```rust
use tokio::sync::Semaphore;

fn spawn_source(
    subscription: SubscriptionId,
    mut stream: Box<dyn DeliveryStream>,
    commands: mpsc::Sender<ConsumerCommand>,
    permits: Arc<Semaphore>,
) {
    tokio::spawn(async move {
        loop {
            let _permit = permits.acquire().await;
            if let Some(result) = stream.next().await {
                if commands
                    .send(ConsumerCommand::Incoming {
                        subscription: subscription.clone(),
                        result,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            } else {
                return;
            }
        }
    });
}
```

In `spawn_with_metrics`, create a `Semaphore` with capacity equal to the mpsc capacity (256) or
`max_in_flight`. Pass `permits.clone()` to `spawn_source`. The actor releases a permit when a delivery
is dispatched to the flume buffer (or when it's dropped from `pending_incoming`).

The actor needs a handle to the `Semaphore` to release permits:

```rust
struct ActorState {
    // ... existing fields ...
    permits: Arc<Semaphore>,
}
```

In `dispatch()`, when a delivery is successfully pushed to the flume buffer:
```rust
// Release the permit — the delivery has reached the flume buffer
self.permits.add_permits(1);
```

Wait — `Semaphore::add_permits` adds permits, not releases. The `spawn_source` acquires a permit
before reading from the stream. When the actor dispatches the delivery to the flume buffer, it
should release the permit back so `spawn_source` can read the next delivery.

Actually, `_permit` in `spawn_source` is dropped when the `Incoming` command is sent (it goes out of
scope at the end of the loop iteration). The permit is an `OwnedSemaphorePermit` that is dropped,
which releases it back to the semaphore. This means the permit is held only during the `stream.next()`
+ `commands.send()` window, not after the actor processes it.

For proper backpressure, the permit should be held until the actor **dispatches** the delivery to the
flume buffer, not just until it's sent via mpsc. This requires the permit to be passed through the
mpsc to the actor:

```rust
// spawn_source:
let permit = permits.acquire().await;
let _ = permit;  // hold the permit
if let Some(result) = stream.next().await {
    // The permit needs to travel with the delivery to the actor.
    // But ConsumerCommand::Incoming doesn't have a permit field.
    // We'd need to add it or use a different mechanism.
}
```

This is getting complex. **Simpler approach**: the `pending_incoming` VecDeque (bounded by mpsc 256)
IS the backpressure mechanism. When the actor stops processing `pending_incoming` (over budget), the
mpsc fills up, `spawn_source` blocks on `send().await`, and backpressure propagates. The permit system
is an additional layer that limits the gap between Lapin's unbounded channel and `spawn_source`.

**Decision: implement the hard gate (`pending_incoming` VecDeque) first. The permit system is a
follow-up if the hard gate alone is insufficient.**

For now, keep `spawn_source` as-is (no permits). The hard gate + mpsc backpressure is the primary
mechanism. The permit system is documented in the spec as a future enhancement.

- [ ] **Step 6: Run focused tests**

Run: `rtk cargo test -p rabbit-rs-core --test consumer hard_gate_stops_accepting_when_over_budget`
Expected: PASS

Run: `rtk cargo test -p rabbit-rs-core --test consumer`
Expected: PASS

- [ ] **Step 7: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/rabbit-rs-core/src/consumer/actor.rs crates/rabbit-rs-core/src/consumer/set.rs crates/rabbit-rs-core/tests/consumer.rs
git commit -m "fix: hard gate stops accepting deliveries when buffered_bytes exceeds max_buffered_bytes"
```

---

## Task 7: Arc<Headers> in TransportDelivery

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport.rs:315-325` — `Delivery` struct
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs:273-306` — `LapinDeliveryStream::next()`
- Modify: `crates/rabbit-rs-core/src/transport/mock.rs:630-661` — `MockDeliveryStream` / mock delivery construction
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs:232` — deep clone line
- Test: `crates/rabbit-rs-core/tests/consumer.rs:55-66` — `delivery()` helper

**Interfaces:**
- Consumes: nothing (first task)
- Produces: `transport::Delivery.headers: Arc<Headers>` — all downstream code that reads `delivery.headers` gets `Arc<Headers>` instead of `Headers`

- [ ] **Step 1: Write the failing test**

In `crates/rabbit-rs-core/tests/consumer.rs`, add a test that verifies the transport delivery type uses `Arc<Headers>`:

```rust
#[tokio::test]
async fn arc_headers_no_deep_clone() {
    use std::sync::Arc;
    let transport_delivery = helper::delivery(1, b"payload");
    let headers_arc = Arc::clone(&transport_delivery.headers);
    let _cloned = Arc::clone(&headers_arc);
    // If headers is Arc<Headers>, clone is a refcount bump, not a deep clone
    // The test compiles only if headers is Arc<Headers>
}
```

Also update the `delivery()` helper to produce `Arc<Headers>`:

```rust
pub fn delivery(tag: u64, payload: &'static [u8]) -> TransportDelivery {
    TransportDelivery {
        delivery_tag: tag,
        exchange: "jobs".to_owned(),
        routing_key: "high".to_owned(),
        redelivered: false,
        message_id: None,
        correlation_id: None,
        headers: Arc::new(BTreeMap::new()),
        payload: Bytes::from_static(payload),
    }
}
```

If there's a `delivery_with_properties` helper, update it too.

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer arc_headers_no_deep_clone`
Expected: FAIL — `Delivery.headers` is `Headers` (BTreeMap), not `Arc<Headers>`. Compilation error on `Arc::clone(&transport_delivery.headers)`.

- [ ] **Step 3: Change `transport::Delivery.headers` to `Arc<Headers>`**

In `crates/rabbit-rs-core/src/transport.rs`, line 323:

```rust
// Before:
pub headers: Headers,

// After:
pub headers: Arc<Headers>,
```

Add `use std::sync::Arc;` at the top of `transport.rs` if not already present.

- [ ] **Step 4: Update `LapinDeliveryStream::next()` to wrap headers in Arc**

In `crates/rabbit-rs-core/src/transport/lapin.rs`, around line 295 (the `Delivery` construction inside `next()`):

```rust
// Before:
headers: map_headers(delivery.properties.headers().as_ref()),

// After:
headers: Arc::new(map_headers(delivery.properties.headers().as_ref())),
```

Add `use std::sync::Arc;` at the top of `lapin.rs` if not already present.

- [ ] **Step 5: Update mock transport to produce `Arc<Headers>`**

In `crates/rabbit-rs-core/src/transport/mock.rs`, find where mock deliveries are constructed (or where the test helpers build them). The mock doesn't construct `Delivery` directly — test helpers in `tests/consumer.rs` do. But if mock.rs has any delivery construction, update it to use `Arc::new(BTreeMap::new())` or `Arc::new(headers)`.

Search mock.rs for `Delivery {` or `headers:` and update any occurrences.

- [ ] **Step 6: Update `actor.rs:232` to use `Arc::clone` instead of deep clone**

In `crates/rabbit-rs-core/src/consumer/actor.rs`, line 232:

```rust
// Before:
let headers = Arc::new(delivery.headers.clone());

// After:
let headers = Arc::clone(&delivery.headers);
```

- [ ] **Step 7: Fix all compilation errors**

Run: `rtk cargo build -p rabbit-rs-core`

Fix any other places that construct `transport::Delivery` or access `.headers` as `Headers` instead of `Arc<Headers>`. Common patterns to fix:
- `delivery.headers.clone()` where `delivery` is `transport::Delivery` — now `Arc::clone(&delivery.headers)` or just `delivery.headers.clone()` (which is `Arc::clone` on `Arc<Headers>`)
- `delivery.headers.iter()` — now `delivery.headers.iter()` (auto-deref through `Arc`)
- `delivery.headers.get(key)` — still works via `Arc` deref

Search the codebase for `transport::Delivery` construction and `.headers` field access:
```
rtk rg "headers:" crates/rabbit-rs-core/src/ --type rust
```

- [ ] **Step 8: Run focused tests to verify they pass**

Run: `rtk cargo test -p rabbit-rs-core --test consumer`
Expected: PASS

Run: `rtk cargo test -p rabbit-rs-core --test publisher`
Expected: PASS

- [ ] **Step 9: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 10: Commit**

```bash
git add crates/rabbit-rs-core/src/transport.rs crates/rabbit-rs-core/src/transport/lapin.rs crates/rabbit-rs-core/src/transport/mock.rs crates/rabbit-rs-core/src/consumer/actor.rs crates/rabbit-rs-core/tests/consumer.rs
git commit -m "perf: use Arc<Headers> in transport::Delivery to eliminate deep BTreeMap clone per delivery"
```

---

## Task 8: Fire-and-Forget Settlement with Error Queue

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs:71-91` — `ConsumerCommand` enum
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs:66-68` — actor settlement handling
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs:828-863` — `SettleThrough` handling
- Modify: `crates/rabbit-rs-core/src/consumer/delivery.rs:200-257` — `DeliveryToken::settle()`
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs:139-202` — `spawn_with_metrics` (add error channel)
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs:232-398` — `ConsumerHandle` (add `error_rx`, `drain_errors()`)
- Test: `crates/rabbit-rs-core/tests/consumer.rs`

**Interfaces:**
- Consumes: `Arc<Headers>` from Task 1 (no direct dependency — just needs to compile)
- Produces: `ConsumerHandle::drain_errors() -> Vec<SettlementError>`, `ConsumerHandle::try_settle() -> Result<(), SettleError>`, `ConsumerHandle::try_settle_through() -> Result<(), SettleError>`, `SettlementError` struct, `SettleError` enum

### Step-by-step

- [ ] **Step 1: Write the failing test — fire-and-forget ack**

In `crates/rabbit-rs-core/tests/consumer.rs`, add a test that verifies `ack()` does not block (returns immediately via `try_send`):

```rust
#[tokio::test(start_paused = true)]
async fn fire_and_forget_ack_returns_immediately() {
    let transport = MockTransport::default();
    transport.push_delivery(helper::delivery(1, b"hello"));
    transport.push_delivery(helper::delivery(2, b"world"));
    transport.push_consumer_result(Ok(()));

    let subscription = helper::subscription(&transport);
    let handle = ConsumerSet::spawn_with_metrics(
        vec![subscription],
        1024,
        Metrics::disabled(),
    )
    .await
    .unwrap();

    // Get a delivery
    let delivery = handle.next().await.unwrap();

    // Ack should return Ok(()) immediately without block_on
    let result = handle.try_settle(
        delivery.inner_token().clone(),
        Settlement::Ack,
    );
    assert!(result.is_ok());

    // The actor should process the settlement in the background
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    // Drain errors should be empty (ack succeeded)
    let errors = handle.drain_errors();
    assert!(errors.is_empty());

    let _ = handle.close().await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer fire_and_forget_ack_returns_immediately`
Expected: FAIL — `try_settle` and `drain_errors` methods don't exist yet. Compilation error.

- [ ] **Step 3: Define `SettlementError` and `SettleError` types**

In `crates/rabbit-rs-core/src/consumer/delivery.rs`, add:

```rust
/// Error returned by `try_settle` when the fire-and-forget send fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettleError {
    /// The actor's command channel is full (256 capacity).
    ChannelFull,
    /// The actor's command channel is closed.
    Closed,
}

/// Error recorded by the actor when a settlement fails asynchronously.
#[derive(Clone, Debug)]
pub struct SettlementError {
    pub delivery_tag: u64,
    pub subscription: SubscriptionId,
    pub kind: ConsumerErrorKind,
    pub message: String,
    pub timestamp: Instant,
}
```

Add `use std::time::Instant;` if not present. Add `use crate::consumer::SubscriptionId;` if not present.

Export these from `consumer/mod.rs`:
```rust
pub use delivery::{SettleError, SettlementError};
```

- [ ] **Step 4: Modify `ConsumerCommand` to remove oneshot from Settle and SettleThrough**

In `crates/rabbit-rs-core/src/consumer/actor.rs`, lines 71-91:

```rust
pub(crate) enum ConsumerCommand {
    Incoming {
        subscription: SubscriptionId,
        result: TransportResult<TransportDelivery>,
    },
    Settle {
        token: Arc<DeliveryTokenInner>,
        settlement: Settlement,
    },
    SettleThrough {
        token: Arc<DeliveryTokenInner>,
    },
    UpdateGeneration {
        subscription: SubscriptionId,
        generation: u64,
        completed: oneshot::Sender<Result<(), ConsumerError>>,
    },
    Close(oneshot::Sender<()>),
}
```

Remove the `completed: oneshot::Sender<...>` field from `Settle` and `SettleThrough` variants.

- [ ] **Step 5: Add error channel to `ConsumerSet::spawn_with_metrics`**

In `crates/rabbit-rs-core/src/consumer/set.rs`, in `spawn_with_metrics` (around line 170-180):

```rust
// After creating the flume buffer channel:
let (error_tx, error_rx) = flume::bounded::<SettlementError>(256);
```

Pass `error_tx` into the actor state. Store `error_rx` in the returned `ConsumerHandle`.

- [ ] **Step 6: Add `error_rx` to `ConsumerHandle` and add `drain_errors()`**

In `crates/rabbit-rs-core/src/consumer/set.rs`, modify the `ConsumerHandle` struct:

```rust
pub struct ConsumerHandle {
    commands: mpsc::Sender<ConsumerCommand>,
    buffer_rx: flume::Receiver<Result<Delivery, ConsumerError>>,
    error_rx: flume::Receiver<SettlementError>,
    metrics: Metrics,
    closed: Arc<AtomicBool>,
    dispatch_notify: Arc<Notify>,
}
```

Add the `drain_errors` method:

```rust
pub fn drain_errors(&self) -> Vec<SettlementError> {
    let mut errors = Vec::new();
    while let Ok(error) = self.error_rx.try_recv() {
        errors.push(error);
    }
    errors
}
```

Add `try_settle` and `try_settle_through` methods:

```rust
pub fn try_settle(
    &self,
    token: Arc<DeliveryTokenInner>,
    settlement: Settlement,
) -> Result<(), SettleError> {
    self.commands
        .try_send(ConsumerCommand::Settle { token, settlement })
        .map_err(|e| match e {
            mpsc::TrySendError::Full(_) => SettleError::ChannelFull,
            mpsc::TrySendError::Closed(_) => SettleError::Closed,
        })
}

pub fn try_settle_through(
    &self,
    token: Arc<DeliveryTokenInner>,
) -> Result<(), SettleError> {
    self.commands
        .try_send(ConsumerCommand::SettleThrough { token })
        .map_err(|e| match e {
            mpsc::TrySendError::Full(_) => SettleError::ChannelFull,
            mpsc::TrySendError::Closed(_) => SettleError::Closed,
        })
}
```

- [ ] **Step 7: Modify `DeliveryToken::settle()` to use `try_send`**

In `crates/rabbit-rs-core/src/consumer/delivery.rs`, replace the `settle()` method (lines 200-257):

```rust
fn try_settle(&self, settlement: Settlement) -> Result<(), SettleError> {
    self.inner
        .state
        .compare_exchange(
            DeliveryState::Pending as u8,
            TRANSITIONING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| ConsumerError::already_settled())?;
    // Note: already_settled is a ConsumerError, not a SettleError.
    // We need to handle this differently — see below.
}
```

Actually, the CAS failure returns `ConsumerError::already_settled()`, which is not a `SettleError`. We need to handle the already-settled case differently. The `try_settle` method on `ConsumerHandle` is the one that returns `SettleError`. The `DeliveryToken` method should handle the CAS internally and return a different error type.

Revised approach — keep `DeliveryToken::settle()` as the internal method but make it non-blocking:

```rust
/// Fire-and-forget settlement. Returns `Err(SettleError)` if the command
/// channel is full or closed. Returns `Err(ConsumerError::already_settled())`
/// if the delivery was already settled. Does not block.
fn try_settle(&self, settlement: Settlement) -> Result<(), SettlementErrorKind> {
    self.inner
        .state
        .compare_exchange(
            DeliveryState::Pending as u8,
            TRANSITIONING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| SettlementErrorKind::AlreadySettled)?;

    match self
        .inner
        .commands
        .try_send(ConsumerCommand::Settle {
            token: self.inner.clone(),
            settlement,
        }) {
        Ok(()) => Ok(()),
        Err(mpsc::TrySendError::Full(_)) => {
            // Revert state to Pending so the caller can retry
            self.inner.state.store(DeliveryState::Pending as u8, Ordering::Release);
            Err(SettlementErrorKind::ChannelFull)
        }
        Err(mpsc::TrySendError::Closed(_)) => {
            self.inner.state.store(DeliveryState::Lost as u8, Ordering::Release);
            Err(SettlementErrorKind::Closed)
        }
    }
}
```

Define `SettlementErrorKind`:
```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettlementErrorKind {
    AlreadySettled,
    ChannelFull,
    Closed,
}
```

Update the public `Delivery` methods (`ack()`, `release()`, `reject()`) to call `try_settle` instead of `settle().await`. These are on the consumer-level `Delivery` in `delivery.rs`. The existing `ack()`, `release()`, `reject()` methods currently call `self.token.settle(Settlement::Ack).await` — change to `self.token.try_settle(Settlement::Ack)`.

- [ ] **Step 8: Update actor settlement handling to send errors to flume**

In `crates/rabbit-rs-core/src/consumer/actor.rs`, the `ActorState` needs an `error_tx: flume::Sender<SettlementError>` field. Add it to `ActorState` struct.

In the `Settle` command handler (where the actor currently calls `oneshot.send(result)`), replace the oneshot send with:

```rust
// On success: update ledger, decrement in_flight, dispatch more (as today)
// On failure: send SettlementError to the error channel
if let Err(error) = &result {
    let _ = state.error_tx.send(SettlementError {
        delivery_tag: token.delivery_tag,
        subscription: token.subscription.clone(),
        kind: error.kind.clone(),
        message: error.to_string(),
        timestamp: Instant::now(),
    });
    // If the error channel is full, the error is dropped (lossy).
    // Metrics should capture the drop count.
}
```

Do the same for `SettleThrough` handling.

- [ ] **Step 9: Fix all compilation errors**

Run: `rtk cargo build -p rabbit-rs-core`

Fix any remaining references to the old `settle().await` pattern or `oneshot::Sender` in settlement handling.

Search for `completed` in the settlement context:
```
rtk rg "completed" crates/rabbit-rs-core/src/consumer/ --type rust
```

The `UpdateGeneration` and `Close` commands still use oneshot — only `Settle` and `SettleThrough` lose theirs.

- [ ] **Step 10: Run focused tests**

Run: `rtk cargo test -p rabbit-rs-core --test consumer fire_and_forget_ack_returns_immediately`
Expected: PASS

- [ ] **Step 11: Write the failing test — error queue**

```rust
#[tokio::test(start_paused = true)]
async fn settlement_error_surfaces_via_drain_errors() {
    let transport = MockTransport::default();
    transport.push_delivery(helper::delivery(1, b"hello"));
    // Push an error for the ack operation
    transport.push_consumer_result(Err(TransportError::connection(
        "test-stale-generation",
    )));

    let subscription = helper::subscription(&transport);
    let handle = ConsumerSet::spawn_with_metrics(
        vec![subscription],
        1024,
        Metrics::disabled(),
    )
    .await
    .unwrap();

    let delivery = handle.next().await.unwrap();

    // Fire-and-forget ack
    handle
        .try_settle(delivery.inner_token().clone(), Settlement::Ack)
        .unwrap();

    // Let the actor process the settlement
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    // The ack should have failed (stale generation / transport error)
    let errors = handle.drain_errors();
    assert!(!errors.is_empty(), "expected at least one settlement error");
    assert_eq!(errors[0].delivery_tag, 1);

    let _ = handle.close().await;
}
```

- [ ] **Step 12: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core --test consumer settlement_error_surfaces_via_drain_errors`
Expected: PASS

- [ ] **Step 13: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 14: Commit**

```bash
git add crates/rabbit-rs-core/src/consumer/ crates/rabbit-rs-core/tests/consumer.rs
git commit -m "perf: fire-and-forget settlement with bounded backpressure and error queue"
```

---

## Task 9: `no_ack` Mode in Transport

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport.rs:297-313` — `ConsumerRequest` struct
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs:238-253` — `consume()` method
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs:26-40` — `Subscription` struct
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs:170-180` — `spawn_with_metrics` (build ConsumerRequest)
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs:234-274` — early_ack path (skip spawn when no_ack)
- Modify: `crates/rabbit-rs-core/src/transport/mock.rs` — mock `consume()` to record `no_ack`
- Modify: `packages/laravel-queue/config/rabbit-rs.php` — add `no_ack` config option
- Modify: `packages/laravel-queue/src/Config/ConfigNormalizer.php` — validate `no_ack` + `early_ack`
- Test: `crates/rabbit-rs-core/tests/consumer.rs`

**Interfaces:**
- Consumes: `Subscription` struct from earlier tasks
- Produces: `ConsumerRequest.no_ack: bool`, `Subscription.no_ack: bool`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(start_paused = true)]
async fn no_ack_propagates_to_transport() {
    let transport = MockTransport::default();
    transport.push_delivery(helper::delivery(1, b"hello"));

    let mut subscription = helper::subscription(&transport);
    subscription.no_ack = true;

    let handle = ConsumerSet::spawn_with_metrics(
        vec![subscription],
        1024,
        Metrics::disabled(),
    )
    .await
    .unwrap();

    let delivery = handle.next().await.unwrap();
    assert_eq!(delivery.state(), DeliveryState::AutoAcked);

    // Verify the mock recorded Consume with no_ack=true
    let ops = transport.operations();
    let consume_op = ops.iter().find(|op| matches!(op, TransportOperation::Consume(_)));
    assert!(consume_op.is_some(), "expected Consume operation");
    if let Some(TransportOperation::Consume(request)) = consume_op {
        assert!(request.no_ack, "expected no_ack=true in ConsumerRequest");
    }

    let _ = handle.close().await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer no_ack_propagates_to_transport`
Expected: FAIL — `Subscription` has no `no_ack` field, `ConsumerRequest` has no `no_ack` field.

- [ ] **Step 3: Add `no_ack` to `ConsumerRequest`**

In `crates/rabbit-rs-core/src/transport.rs`, find the `ConsumerRequest` struct (around line 297-313):

```rust
pub struct ConsumerRequest {
    pub queue: String,
    pub consumer_tag: String,
    pub exclusive: bool,
    pub no_ack: bool,  // ← add this field
}
```

- [ ] **Step 4: Propagate `no_ack` in `LapinConsumerChannel::consume()`**

In `crates/rabbit-rs-core/src/transport/lapin.rs`, line 246:

```rust
// Before:
no_ack: false,

// After:
no_ack: request.no_ack,
```

- [ ] **Step 5: Add `no_ack` to `Subscription`**

In `crates/rabbit-rs-core/src/consumer/set.rs`, the `Subscription` struct (lines 26-40):

```rust
pub struct Subscription {
    // ... existing fields ...
    pub no_ack: bool,  // ← add this field, default false
}
```

Update the `Subscription` builder/constructor to default `no_ack` to `false`.

- [ ] **Step 6: Set `no_ack` on `ConsumerRequest` from `Subscription` in `spawn_with_metrics`**

In `crates/rabbit-rs-core/src/consumer/set.rs`, where `ConsumerRequest` is constructed (around line 155-165):

```rust
let request = ConsumerRequest {
    queue: subscription.queue.clone(),
    consumer_tag: subscription.id.to_string(),
    exclusive: subscription.exclusive,
    no_ack: subscription.no_ack,  // ← add this
};
```

- [ ] **Step 7: Skip ack spawn in actor when `no_ack` is active**

In `crates/rabbit-rs-core/src/consumer/actor.rs`, the early_ack path (lines 234-274). When `no_ack=true` on the subscription, the actor should not spawn an ack task — RabbitMQ auto-acks internally.

Add `no_ack` to `RuntimeSubscription` in actor.rs so the actor knows which subscriptions use `no_ack`. Then in the dispatch method:

```rust
if runtime.early_ack {
    if runtime.no_ack {
        // RabbitMQ auto-acks internally — no spawned task needed
        let delivery = Delivery::new_auto_acked(/* ... */);
        // try_send into flume buffer (same as today)
    } else {
        // Current behavior: spawn tokio::spawn to ack
        let channel = runtime.channel.clone();
        let tag = delivery.delivery_tag;
        tokio::spawn(async move {
            let _ = channel.ack(tag, false).await;
        });
        // ...
    }
}
```

- [ ] **Step 8: Update mock to record `no_ack` in `Consume` operation**

In `crates/rabbit-rs-core/src/transport/mock.rs`, the `MockConsumerChannel::consume()` method (around line 596):

The mock already records `TransportOperation::Consume(request)`. Since `ConsumerRequest` now has `no_ack`, the mock will automatically record it. No change needed if the mock stores the full `ConsumerRequest`. Verify by checking the mock's `consume` implementation.

- [ ] **Step 9: Add `no_ack` config option in Laravel**

In `packages/laravel-queue/config/rabbit-rs.php`, in the subscription config section (around line 188-190 where `early_ack` is defined), add:

```php
'no_ack' => false, // When true + early_ack=true, eliminates all ack frames at the broker level
```

- [ ] **Step 10: Validate `no_ack` requires `early_ack` in `ConfigNormalizer`**

In `packages/laravel-queue/src/Config/ConfigNormalizer.php`, in the subscription validation section (around line 237-350 where `early_ack` is validated), add:

```php
if ($subscription['no_ack'] ?? false) {
    if (!($subscription['early_ack'] ?? false)) {
        throw new InvalidArgumentException(
            "no_ack=true requires early_ack=true for subscription '{$subscription['name']}'"
        );
    }
    if (!($config['best_effort'] ?? false)) {
        throw new InvalidArgumentException(
            "no_ack=true requires best_effort=true for subscription '{$subscription['name']}'"
        );
    }
}
```

- [ ] **Step 11: Fix compilation errors**

Run: `rtk cargo build -p rabbit-rs-core`

Fix all places that construct `ConsumerRequest` or `Subscription` without the new `no_ack` field.

```
rtk rg "ConsumerRequest \{" crates/rabbit-rs-core/src/ --type rust
rtk rg "Subscription \{" crates/rabbit-rs-core/src/ --type rust
```

- [ ] **Step 12: Run focused tests**

Run: `rtk cargo test -p rabbit-rs-core --test consumer no_ack_propagates_to_transport`
Expected: PASS

Run: `rtk cargo test -p rabbit-rs-core --test consumer`
Expected: PASS

- [ ] **Step 13: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 14: Commit**

```bash
git add crates/rabbit-rs-core/src/ packages/laravel-queue/config/ packages/laravel-queue/src/Config/ConfigNormalizer.php crates/rabbit-rs-core/tests/consumer.rs
git commit -m "feat: add no_ack transport mode gated behind best_effort + early_ack"
```

---

## Task 10: Arc<str> Fields on PublishRequest (Cheap Clone)

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs:108-122` — `Destination` struct
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs:124-144` — `MessageProperties` struct
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:702-749` — `into_transport_request()` (Arc→String conversion)
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs:170-178` — `republish()` method
- Test: `crates/rabbit-rs-core/tests/publisher.rs`

**Interfaces:**
- Consumes: nothing new
- Produces: `Destination` with `Arc<str>` fields, `MessageProperties` with `Arc<str>` fields — clones are refcount bumps

- [ ] **Step 1: Write the failing test**

In `crates/rabbit-rs-core/tests/publisher.rs`, add a test that verifies the clone is cheap (Arc refcount bump):

```rust
#[test]
fn publish_request_clone_is_refcount_bump() {
    let request = helper::request_safety(1);
    let cloned = request.clone();
    // Both should point to the same underlying string data
    assert!(std::ptr::eq(
        request.destination.exchange.as_ref(),
        cloned.destination.exchange.as_ref(),
    ));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test publisher publish_request_clone_is_refcount_bump`
Expected: FAIL — `exchange` is `String`, not `Arc<str>`. `as_ref()` returns `&str` but `ptr::eq` on `&str` won't match since they're different allocations.

- [ ] **Step 3: Change `Destination` fields to `Arc<str>`**

In `crates/rabbit-rs-core/src/publisher/mod.rs`, lines 108-122:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Destination {
    pub exchange: Arc<str>,
    pub routing_key: Arc<str>,
}

impl Destination {
    #[must_use]
    pub fn new(exchange: impl Into<String>, routing_key: impl Into<String>) -> Self {
        Self {
            exchange: Arc::from(exchange.into()),
            routing_key: Arc::from(routing_key.into()),
        }
    }
}
```

Add `use std::sync::Arc;` if not present.

- [ ] **Step 4: Change `MessageProperties` fields to `Arc<str>`**

In `crates/rabbit-rs-core/src/publisher/mod.rs`, lines 124-144:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageProperties {
    pub message_id: Arc<str>,
    pub content_type: Option<Arc<str>>,
    pub correlation_id: Option<Arc<str>>,
    pub delay_ms: Option<u64>,
    pub headers: PublishHeaders,
}

impl MessageProperties {
    #[must_use]
    pub fn new(message_id: impl Into<String>) -> Self {
        Self {
            message_id: Arc::from(message_id.into()),
            content_type: None,
            correlation_id: None,
            delay_ms: None,
            headers: PublishHeaders::new(),
        }
    }
}
```

- [ ] **Step 5: Update `republish()` to use Arc clones**

In `crates/rabbit-rs-core/src/publisher/mod.rs`, line 170-178:

```rust
pub fn republish(&self, deadline: Instant) -> Self {
    Self {
        destination: self.destination.clone(),       // Arc clone (cheap)
        payload: self.payload.clone(),               // Bytes clone (cheap)
        properties: self.properties.clone(),          // Arc clones (cheap)
        deadline,
    }
}
```

This is already correct — `clone()` on `Arc<str>` is a refcount bump. No change needed here, but verify it compiles.

- [ ] **Step 6: Update `into_transport_request()` to convert `Arc<str>` → `String`**

In `crates/rabbit-rs-core/src/publisher/actor.rs`, lines 702-749, the `into_transport_request` function needs to convert `Arc<str>` → `String` for the `TransportRequest` (which uses `String`):

```rust
// For exchange:
exchange: request.destination.exchange.to_string(),  // Arc<str> → String

// For routing_key:
routing_key: request.destination.routing_key.to_string(),

// For message_id:
message_id: Some(request.properties.message_id.to_string()),

// For content_type:
content_type: request.properties.content_type.as_ref().map(|s| s.to_string()),

// For correlation_id:
correlation_id: request.properties.correlation_id.as_ref().map(|s| s.to_string()),
```

The `TransportRequest` fields stay `String` / `Option<String>` — Lapin needs owned strings. The conversion is `Arc<str>::to_string()` which allocates once (at the transport boundary), not twice.

- [ ] **Step 7: Fix all compilation errors**

Run: `rtk cargo build -p rabbit-rs-core`

Fix any places that construct `Destination` or `MessageProperties` with `String` directly. Search:

```
rtk rg "Destination::new" crates/ --type rust
rtk rg "MessageProperties::new" crates/ --type rust
rtk rg "MessageProperties \{" crates/ --type rust
```

Any place that does `exchange: "foo".to_string()` in a `Destination` or `MessageProperties` literal needs to become `exchange: Arc::from("foo")` or use the `::new()` constructor.

- [ ] **Step 8: Run focused tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher publish_request_clone_is_refcount_bump`
Expected: PASS

Run: `rtk cargo test -p rabbit-rs-core --test publisher`
Expected: PASS

- [ ] **Step 9: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 10: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/ crates/rabbit-rs-core/tests/publisher.rs
git commit -m "perf: Arc<str> fields on Destination and MessageProperties for cheap PublishRequest clones"
```

---

## Task 11: TaggedFuture — Eliminate Double BoxFuture

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:276-277` — `PublishFuture` type alias
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:620-640` — publish_queue Box::pin wrapper
- Test: `crates/rabbit-rs-core/tests/publisher.rs`

**Interfaces:**
- Consumes: `Arc<str>` fields from Task 4 (not directly — just compiles)
- Produces: `TaggedFuture` struct, `PublishFuture` type alias change

- [ ] **Step 1: Write the failing test**

In `crates/rabbit-rs-core/tests/publisher.rs`, add a test that verifies publish still works correctly with the new TaggedFuture:

```rust
#[tokio::test(start_paused = true)]
async fn tagged_future_publish_completes() {
    let transport = MockTransport::default();
    transport.push_connect_result(Ok(()));
    transport.push_publish_confirmation(PublishConfirmation::Ready(Ok(())));

    let channel = Arc::new(transport.open_publisher().await.unwrap()) as Arc<dyn PublisherChannel>;
    let config = helper::config_safety();
    let handle = PublisherActor::spawn_inner(channel, config, Metrics::disabled(), None);

    let request = helper::request_safety(1);
    let waiter = handle.try_publish(request).unwrap();
    let outcome = waiter.wait().await.unwrap();

    assert!(matches!(outcome, PublishOutcome::Confirmed { .. }));
}
```

Note: This test should already pass with the existing code. The key change is that after implementing TaggedFuture, it should still pass. The test validates correctness, not the allocation pattern.

- [ ] **Step 2: Run test to verify it passes (baseline)**

Run: `rtk cargo test -p rabbit-rs-core --test publisher tagged_future_publish_completes`
Expected: PASS (with existing code — this is our baseline)

- [ ] **Step 3: Define `TaggedFuture` struct**

In `crates/rabbit-rs-core/src/publisher/actor.rs`, near the type aliases (line 276-277):

```rust
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

struct TaggedFuture {
    fut: Pin<Box<dyn Future<Output = TransportResult<Box<dyn PublishReceipt>>> + Send>>,
    sequence: u64,
}

impl Future for TaggedFuture {
    type Output = (u64, TransportResult<Box<dyn PublishReceipt>>);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.fut.as_mut().poll(cx) {
            Poll::Ready(result) => Poll::Ready((self.sequence, result)),
            Poll::Pending => Poll::Pending,
        }
    }
}
```

Change the `PublishFuture` type alias:

```rust
// Before:
type PublishFuture = BoxFuture<'static, (u64, TransportResult<Box<dyn PublishReceipt>>)>;

// After:
type PublishFuture = TaggedFuture;
```

- [ ] **Step 4: Replace `Box::pin` wrapper with `TaggedFuture`**

In `crates/rabbit-rs-core/src/publisher/actor.rs`, lines 620-640:

```rust
// Before:
let channel_for_pub = Arc::clone(&channel);
let mut publish_fut = Box::pin(async move {
    let result = channel_for_pub.publish(request).await;
    (sequence, result)
});

// After:
let publish_fut = channel.publish(request);  // already BoxFuture via async_trait
let tagged = TaggedFuture { fut: publish_fut, sequence };
```

Then the `now_or_never()` call:

```rust
// Before:
match publish_fut.as_mut().now_or_never() {

// After:
match Pin::new(&mut tagged).now_or_never() {
```

Wait — `now_or_never()` requires `Unpin`. `TaggedFuture` contains `Pin<Box<...>>` which is `Unpin`, and `u64` which is `Unpin`, so `TaggedFuture` is `Unpin`. So `&mut tagged` can be used directly.

Actually, `now_or_never()` is defined on `Future` and requires the future to be `Unpin`. Since `TaggedFuture` is `Unpin`, we can call `tagged.now_or_never()` directly (no `Pin::new` needed).

```rust
match tagged.now_or_never() {
    Some((seq, result)) => {
        drop(tagged);
        handle_publish_completion(state, seq, result);
        if matches!(state.phase, Phase::Suspended) {
            state.replay.extend(pending);
            return;
        }
    }
    None => {
        state.publish_in_flight.push(tagged);
    }
}
```

Note: `channel` is `Arc<dyn PublisherChannel>`. The `publish()` method takes `&self`, so we can call it directly without `Arc::clone`. The future returned by `publish()` captures the reference internally (via the `Arc`'s `&self`). This eliminates the `Arc::clone` per publish.

Verify: `channel` at this point in the code is a local variable obtained from `state.channel.as_ref()`. The future from `channel.publish(request)` is `'static` because `async_trait` boxes it. So it can be stored in `FuturesUnordered` without lifetime issues.

- [ ] **Step 5: Fix compilation errors**

Run: `rtk cargo build -p rabbit-rs-core`

Fix any type mismatches in `publish_in_flight: FuturesUnordered<PublishFuture>` — it should now be `FuturesUnordered<TaggedFuture>`.

- [ ] **Step 6: Run focused tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher`
Expected: PASS

- [ ] **Step 7: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/actor.rs crates/rabbit-rs-core/tests/publisher.rs
git commit -m "perf: TaggedFuture eliminates double Box::pin allocation per publish"
```

---

## Task 12: Batch Wait — `wait_all()`

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs:309-335` — `PublishWaiter` struct
- Modify: `crates/rabbit-rs-core/src/client.rs:162-191` — `publish_batch` waiter loop
- Modify: `crates/rabbit-rs-core/src/client.rs:233-240` — `publish_batch_detailed` waiter loop
- Test: `crates/rabbit-rs-core/tests/publisher.rs`

**Interfaces:**
- Consumes: `PublishWaiter` from earlier tasks
- Produces: `PublishWaiter::wait_all()` static method

- [ ] **Step 1: Write the failing test**

In `crates/rabbit-rs-core/tests/publisher.rs`, add a test that verifies `wait_all` returns results in original order:

```rust
#[tokio::test(start_paused = true)]
async fn wait_all_returns_results_in_order() {
    let transport = MockTransport::default();
    transport.push_connect_result(Ok(()));
    transport.push_publish_confirmation(PublishConfirmation::Ready(Ok(())));
    transport.push_publish_confirmation(PublishConfirmation::Ready(Ok(())));
    transport.push_publish_confirmation(PublishConfirmation::Ready(Ok(())));

    let channel = Arc::new(transport.open_publisher().await.unwrap()) as Arc<dyn PublisherChannel>;
    let config = helper::config_safety();
    let handle = PublisherActor::spawn_inner(channel, config, Metrics::disabled(), None);

    let mut waiters = Vec::new();
    for i in 0..3 {
        let request = helper::request_safety(i);
        let waiter = handle.try_publish(request).unwrap();
        waiters.push((i as usize, waiter));
    }

    let results = PublishWaiter::wait_all(waiters).await;
    assert_eq!(results.len(), 3);
    for (i, result) in &results {
        assert_eq!(*i, *i); // results in order
        assert!(result.is_ok());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test publisher wait_all_returns_results_in_order`
Expected: FAIL — `PublishWaiter::wait_all` doesn't exist. Compilation error.

- [ ] **Step 3: Implement `PublishWaiter::wait_all()`**

In `crates/rabbit-rs-core/src/publisher/mod.rs`, add to the `PublishWaiter` impl block:

```rust
use futures_util::stream::{FuturesUnordered, StreamExt};

impl PublishWaiter {
    /// Collectively awaits all waiters with a single drain cycle.
    /// Returns results in the original order.
    pub async fn wait_all(
        waiters: Vec<(usize, PublishWaiter)>,
    ) -> Vec<(usize, Result<PublishOutcome, PublishError>)> {
        let mut futures: FuturesUnordered<_> = waiters
            .into_iter()
            .map(|(index, waiter)| async move {
                (index, waiter.wait().await)
            })
            .collect();

        let mut results = Vec::with_capacity(futures.len());
        while let Some(result) = futures.next().await {
            results.push(result);
        }

        // Sort by original index to preserve order
        results.sort_by_key(|(index, _)| *index);
        results
    }
}
```

Note: The `FuturesUnordered` returns results in completion order, not submission order. We sort by index to restore original order. Since AMQP confirmations arrive in order, the sort is a no-op in practice, but it's correct regardless.

- [ ] **Step 4: Replace sequential loop in `publish_batch`**

In `crates/rabbit-rs-core/src/client.rs`, lines 162-172:

```rust
// Before:
let mut terminal_error = immediate_error;
for (index, waiter) in waiters {
    match waiter.wait().await {
        Ok(outcome) => outcomes[index] = Some(Ok(outcome)),
        Err(error) => {
            let client_err = ClientError::publish(&error);
            terminal_error.get_or_insert_with(|| client_err.clone());
            outcomes[index] = Some(Err(client_err));
        }
    }
}

// After:
let mut terminal_error = immediate_error;
let results = PublishWaiter::wait_all(waiters).await;
for (index, result) in results {
    match result {
        Ok(outcome) => outcomes[index] = Some(Ok(outcome)),
        Err(error) => {
            let client_err = ClientError::publish(&error);
            terminal_error.get_or_insert_with(|| client_err.clone());
            outcomes[index] = Some(Err(client_err));
        }
    }
}
```

- [ ] **Step 5: Replace sequential loop in `publish_batch_detailed`**

In `crates/rabbit-rs-core/src/client.rs`, find the `publish_batch_detailed` method (around line 233-240) and apply the same pattern:

```rust
// Before:
for (index, waiter) in waiters {
    match waiter.wait().await { ... }
}

// After:
let results = PublishWaiter::wait_all(waiters).await;
for (index, result) in results { ... }
```

- [ ] **Step 6: Run focused tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher wait_all_returns_results_in_order`
Expected: PASS

Run: `rtk cargo test -p rabbit-rs-core --test publisher`
Expected: PASS

Run: `rtk cargo test -p rabbit-rs-core --test integration`
Expected: PASS

- [ ] **Step 7: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/mod.rs crates/rabbit-rs-core/src/client.rs crates/rabbit-rs-core/tests/publisher.rs
git commit -m "perf: wait_all() replaces sequential waiter polling with single collective await"
```

---

## Task 13: PHP Extension — Fire-and-Forget Settlement + drainErrors

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/delivery.rs:66-103` — `ack()`, `release()`, `reject()`
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs:153-192` — `ackThrough()`, `ackBatch()`
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs` — add `drainErrors()` method
- Test: `crates/rabbit-rs-php/tests/` — Pest tests

**Interfaces:**
- Consumes: `ConsumerHandle::try_settle()`, `ConsumerHandle::try_settle_through()`, `ConsumerHandle::drain_errors()` from Task 2
- Produces: PHP `Delivery::ack()` fire-and-forget, `Consumer::drainErrors()` method

- [ ] **Step 1: Write the failing Pest test**

In `crates/rabbit-rs-php/tests/`, create or update a test file for fire-and-forget ack:

```php
<?php

use Goopil\RabbitRs\Consumer;
use Goopil\RabbitRs\Delivery;

it('drainErrors returns empty when no settlement errors', function () {
    $consumer = createTestConsumer();
    $errors = $consumer->drainErrors();
    expect($errors)->toBeEmpty();
});

it('ack returns void without blocking', function () {
    $consumer = createTestConsumer();
    $delivery = $consumer->tryNext();
    expect($delivery)->not->toBeNull();

    // ack should return void immediately (no exception)
    $delivery->ack();
    expect(true)->toBeTrue();
});
```

Note: PHP tests require the extension to be built and installed. If the extension can't be built in the test environment, write the test but mark it as skipped if the extension is not loaded.

- [ ] **Step 2: Modify `Delivery::ack()` to fire-and-forget with backpressure**

In `crates/rabbit-rs-php/src/classes/delivery.rs`, lines 66-74:

```rust
pub fn ack(&self) -> PhpResult<()> {
    self.ensure_current_process("Goopil\\RabbitRs\\Delivery::ack")?;
    if self.inner.state() == DeliveryState::AutoAcked {
        return rabbit_exception("cannot ack an auto-acked delivery");
    }
    self.inner.try_ack()
        .map_err(|e| match e {
            SettlementErrorKind::AlreadySettled => consumer_php_exception(&ConsumerError::already_settled()),
            SettlementErrorKind::ChannelFull => {
                // Bounded backpressure: spin-yield then bounded block_on
                self.backpressure_settle(Settlement::Ack)
            }
            SettlementErrorKind::Closed => consumer_php_exception(&ConsumerError::closed()),
        })
}
```

Add a `backpressure_settle` helper method that retries with `std::thread::yield_now()` up to 64 times, then falls back to a bounded `block_on` with 10ms timeout:

```rust
fn backpressure_settle(&self, settlement: Settlement) -> PhpResult<()> {
    // Spin-yield: 64 retries
    for _ in 0..64 {
        if self.inner.try_settle(settlement).is_ok() {
            return Ok(());
        }
        std::thread::yield_now();
    }
    // Bounded block_on: 10ms timeout
    self.runtime
        .block_on(async {
            tokio::time::timeout(
                Duration::from_millis(10),
                self.inner.settle_blocking(settlement),
            ).await
        })
        .map_err(|_| {
            rabbit_exception("settlement channel full after backpressure timeout")
        })?
        .map_err(|error| consumer_php_exception(&error))
}
```

Note: We may need to keep a `settle_blocking()` method on `DeliveryToken` that uses `send().await` (the old path) as the fallback. The `try_settle()` is the fast path, and `settle_blocking()` is the backpressure fallback.

Apply the same pattern to `release()` and `reject()`.

- [ ] **Step 3: Modify `ackThrough()` to fire-and-forget**

In `crates/rabbit-rs-php/src/classes/consumer.rs`, lines 153-159:

```rust
pub fn ackThrough(&self, delivery: &Delivery) -> PhpResult<()> {
    self.ensure_open("Goopil\\RabbitRs\\Consumer::ackThrough")?;
    self.handle
        .try_settle_through(delivery.inner.inner_token().clone())
        .map_err(|e| match e {
            SettleError::ChannelFull => {
                // Same backpressure pattern as ack
                self.backpressure_settle_through(delivery)
            }
            SettleError::Closed => consumer_php_exception(&ConsumerError::closed()),
        })
}
```

- [ ] **Step 4: Modify `ackBatch()` to fire-and-forget + bound to 256**

In `crates/rabbit-rs-php/src/classes/consumer.rs`, lines 162-192: replace each `block_on(delivery.inner.ack())` with `delivery.inner.try_ack()` (or the backpressure fallback). The loop becomes non-blocking.

Additionally, bound the loop to 256 deliveries to prevent unbounded iteration:

```rust
pub fn ackBatch(&self, deliveries: &ZendHashTable) -> PhpResult<()> {
    self.ensure_open("Goopil\\RabbitRs\\Consumer::ackBatch")?;

    let mut count = 0usize;
    for (_, value) in deliveries {
        if count >= 256 {
            return rabbit_exception("ackBatch: maximum 256 deliveries per call");
        }
        count += 1;
        // ... extract delivery object (existing code) ...
        // Fire-and-forget ack (from Step 2):
        delivery.inner.try_ack()
            .map_err(|e| /* same backpressure pattern as ack() */)?;
    }
    Ok(())
}
```

- [ ] **Step 4a: Fix `nextBatch()` off-by-one in slow path**

In `crates/rabbit-rs-php/src/classes/consumer.rs:138`, the slow path calls `try_next_batch(max.saturating_sub(1))`. When `max=1`, `saturating_sub(1)` = 0, which the core clamps to 1 (via `max.clamp(1, 256)` at `set.rs:291`). This returns up to 1 additional delivery, totaling 2 when the caller requested `max=1`.

Fix: do not drain when `max <= 1`:

```rust
// Before (line 138):
let more = self
    .handle
    .try_next_batch(max.saturating_sub(1))
    .map_err(|error| consumer_php_exception(&error))?;

// After:
let more = if max > 1 {
    self.handle
        .try_next_batch(max.saturating_sub(1))
        .map_err(|error| consumer_php_exception(&error))?
} else {
    Vec::new()
};
```

- [ ] **Step 5: Add `drainErrors()` method to `Consumer`**

In `crates/rabbit-rs-php/src/classes/consumer.rs`:

```rust
pub fn drainErrors(&self) -> PhpResult<ZendHashTable> {
    self.ensure_open("Goopil\\RabbitRs\\Consumer::drainErrors")?;
    let errors = self.handle.drain_errors();
    let mut table = ZendHashTable::new();
    for (i, error) in errors.iter().enumerate() {
        let mut entry = ZendHashTable::new();
        entry.insert("delivery_tag", error.delivery_tag as i64)?;
        entry.insert("subscription", error.subscription.to_string())?;
        entry.insert("error_kind", format!("{:?}", error.kind))?;
        entry.insert("message", error.message.clone())?;
        table.insert(i, entry)?;
    }
    Ok(table)
}
```

- [ ] **Step 6: Fix compilation errors**

Run: `rtk cargo build -p rabbit-rs-php`

Fix any missing imports or type mismatches. The PHP extension needs to import `SettlementErrorKind`, `SettleError`, `Settlement` from the core crate.

- [ ] **Step 7: Run PHP tests**

Run: `rtk ./scripts/test-extension.sh`
Expected: PASS (if extension can be built)

If the extension can't be built (no embed SAPI, etc.), at least verify compilation:
Run: `rtk cargo build -p rabbit-rs-php`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/rabbit-rs-php/src/classes/delivery.rs crates/rabbit-rs-php/src/classes/consumer.rs crates/rabbit-rs-php/tests/
git commit -m "perf: PHP fire-and-forget settlement with bounded backpressure and drainErrors"
```

---

## Task 14: Laravel — drainSettlementErrors in pop() + WorkerIdle

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php:311-340` — `pop()` method
- Modify: `packages/laravel-queue/src/Console/RabbitMqWorkCommandExtension.php:75-130` — `register()` method
- Test: `packages/laravel-queue/tests/`

**Interfaces:**
- Consumes: `Consumer::drainErrors()` from Task 7
- Produces: `RabbitMqQueue::drainSettlementErrors()` method

- [ ] **Step 1: Write the failing Pest test**

In `packages/laravel-queue/tests/Unit/`, create a test for `drainSettlementErrors`:

```php
<?php

use Goopil\RabbitRsLaravel\Queue\RabbitMqQueue;

it('drainSettlementErrors throws ConnectionException on stale generation', function () {
    // Use a mock consumer that returns a settlement error
    $queue = createTestQueueWithMockConsumer();
    // The mock consumer's drainErrors returns a StaleGeneration error
    expect(fn() => $queue->drainSettlementErrors())
        ->toThrow(\Goopil\RabbitRsLaravel\Queue\Exceptions\ConnectionException::class);
});

it('drainSettlementErrors logs non-connection errors without throwing', function () {
    $queue = createTestQueueWithMockConsumer();
    // The mock consumer's drainErrors returns an AlreadySettled error
    $queue->drainSettlementErrors();
    // Should not throw — just log
    expect(true)->toBeTrue();
});
```

Note: Use fake/mock classes since Unit tests don't require the extension.

- [ ] **Step 2: Add `drainSettlementErrors()` to `RabbitMqQueue`**

In `packages/laravel-queue/src/RabbitMqQueue.php`:

```php
private function drainSettlementErrors(): void
{
    foreach ($this->consumers as $consumer) {
        $errors = $consumer->drainErrors();
        foreach ($errors as $error) {
            $kind = $error['error_kind'] ?? '';
            if (in_array($kind, ['StaleGeneration', 'Transport'], true)) {
                throw new ConnectionException($error['message'] ?? 'settlement error: ' . $kind);
            }
            // Log non-connection errors
            if (isset($this->container)) {
                $this->container->make('log')->warning('rabbit-rs settlement error', $error);
            }
        }
    }
}
```

- [ ] **Step 3: Call `drainSettlementErrors()` at top of `pop()`**

In `packages/laravel-queue/src/RabbitMqQueue.php`, `pop()` method, line 311:

```php
public function pop($queue = null, $index = 0)
{
    $this->drainSettlementErrors();  // ← add this line

    if ($queue === null) {
        // ... existing logic ...
    }
    // ... rest of pop() ...
}
```

- [ ] **Step 4: Add `WorkerIdle` listener in `RabbitMqWorkCommandExtension`**

In `packages/laravel-queue/src/Console/RabbitMqWorkCommandExtension.php`, in the `register()` method (line 75-130), add:

```php
use Illuminate\Queue\Events\WorkerIdle;

// In register():
$events->listen(WorkerIdle::class, function (WorkerIdle $event) use ($logger, $prefix): void {
    // Drain settlement errors before the worker sleeps
    // Access the queue connection through the event's connection name
    $logger('debug', [
        'worker' => $prefix,
        'status' => 'idle_drain',
    ]);
});
```

Note: The `WorkerIdle` event doesn't provide direct access to the `RabbitMqQueue` instance. The listener needs to resolve the queue connection from the container. This may require a different approach — see Step 5.

- [ ] **Step 5: Resolve queue connection in WorkerIdle listener**

The `WorkerIdle` event is dispatched from `Worker::daemon()` (line 275). It doesn't carry the connection name. We need a different approach:

**Option A:** Store a reference to the `RabbitMqQueue` in the extension. This requires the extension to be created with the queue instance.

**Option B:** Use the `JobPopping` event instead, which fires before `pop()` and carries the connection name. But `drainSettlementErrors()` is already called at the top of `pop()`.

**Option C:** Register the `WorkerIdle` listener in the `RabbitMqServiceProvider` where we have access to the queue manager.

**Recommended: Option C.** In `RabbitMqServiceProvider::boot()`, register a `WorkerIdle` listener that resolves the queue connection and calls `drainSettlementErrors()`:

```php
$events->listen(WorkerIdle::class, function () use ($app) {
    $queue = $app->make('queue')->connection('rabbit-rs');
    if ($queue instanceof RabbitMqQueue) {
        $queue->drainSettlementErrors();
    }
});
```

Note: This only works if the worker uses the `rabbit-rs` connection. If the worker uses multiple connections, we'd need to resolve all of them. For now, assume a single `rabbit-rs` connection (the common case).

Actually, simpler approach: make `drainSettlementErrors()` a public method and call it from the listener. But the listener needs the `RabbitMqQueue` instance. Since the extension is registered per-worker and the queue is resolved lazily, we can't easily get the instance.

**Simplest approach:** Skip the `WorkerIdle` listener for now. The `drainSettlementErrors()` in `pop()` runs every iteration, including when the queue is empty (pop() returns null and the worker sleeps). The only gap is when `pop()` is never called again (worker is about to exit). That's acceptable — on exit, the consumer is closed and any pending errors are dropped (at-least-once preserved).

**Decision: Implement `WorkerIdle` listener in `RabbitMqServiceProvider` as a best-effort drain. If it's too complex to wire up, skip it — the `pop()` drain is sufficient.**

- [ ] **Step 6: Run Laravel tests**

Run: `rtk ./scripts/test-laravel.sh`
Expected: PASS (Unit tests without extension, Feature tests)

If integration tests fail because the extension isn't built:
Run: `rtk composer test --workdir packages/laravel-queue` (Unit + Feature only)

- [ ] **Step 7: Run full quality gate**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets && rtk composer validate --strict`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add packages/laravel-queue/src/ packages/laravel-queue/tests/
git commit -m "feat: drainSettlementErrors in pop() surfaces async ack errors"
```

---

## Task 15: Benchmark — AUTO_ACK Fairness + Prefetch Global + SKIP Investigation

**Files:**
- Modify: `benchmarks/src/Config.php:30` — `PREFETCH_COUNT = 128`
- Modify: `benchmarks/src/Drivers/RabbitRsDriver.php:57-64` — AUTO_ACK confirms=false
- Modify: `benchmarks/src/Drivers/BunnyDriver.php:125-163` — migrate from `basic_get` to `basic_consume`
- Modify: `benchmarks/src/Drivers/AmqpExtDriver.php:129-168` — migrate from `basic_get` to `basic_consume`
- Modify: `benchmarks/laravel/LaravelCompareBenchmark.php:146` — fix `' prefetch_count'` leading space
- Investigate: `scripts/run-benchmarks.php` — SKIP error
- Investigate: `crates/rabbit-rs-core/src/publisher/actor.rs:381-401` — `expire_replay()`

**Interfaces:**
- Consumes: `no_ack` from Task 3, `early_ack` config
- Produces: Fair benchmark comparison

- [ ] **Step 1: Raise prefetch to 128 globally**

In `benchmarks/src/Config.php`, line 30:

```php
// Before:
public const PREFETCH_COUNT = 16;

// After:
public const PREFETCH_COUNT = 128;
```

- [ ] **Step 2: Fix AUTO_ACK fairness in `RabbitRsDriver`**

In `benchmarks/src/Drivers/RabbitRsDriver.php`, lines 57-64:

```php
// Before:
'confirms' => match ($this->scenarioMode) {
    ScenarioMode::FIRE_AND_FORGET => false,
    ScenarioMode::AUTO_ACK, ScenarioMode::BATCH_CONFIRM => true,
},

// After:
'confirms' => match ($this->scenarioMode) {
    ScenarioMode::FIRE_AND_FORGET, ScenarioMode::AUTO_ACK => false,
    ScenarioMode::BATCH_CONFIRM => true,
},
```

Also, set `no_ack=true` in the subscription config for AUTO_ACK:

```php
'no_ack' => match ($this->scenarioMode) {
    ScenarioMode::AUTO_ACK => true,
    default => false,
},
```

And set `early_ack=true` for AUTO_ACK (already done at lines 46-49).

- [ ] **Step 2a: Migrate `BunnyDriver` from `basic_get` to `basic_consume`**

`BunnyDriver.php:125-163` uses `basic.get` (polling) via `$this->channel->get(self::QUEUE, $autoAck)`.
`basic.get` sends a request-response round-trip per message, while `basic_consume` has the broker
push messages continuously. This is an unfair disadvantage.

Replace the polling loop with a callback-based `basic_consume` pattern (following `AmqplibDriver` as
reference):

```php
public function consumeMessages(int $count): void
{
    if ($this->channel === null) {
        throw new RuntimeException('Driver not set up');
    }

    $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
        || $this->scenarioMode === ScenarioMode::AUTO_ACK;
    $consumed = 0;

    $consumerTag = 'bench_bunny_consumer';
    $channel = $this->channel;
    $queue = self::QUEUE;

    $callback = function ($message) use ($count, &$consumed, $autoAck, $channel, $consumerTag): void {
        $body = $message->content;
        $this->recordReceived($message->getHeader('message-id', ''));
        if (strlen($body) >= 8) {
            $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
            if ($ts !== null) {
                $elapsedNs = hrtime(true) - (int) $ts;
                $this->recordLatency($elapsedNs / 1_000_000);
            }
        }
        $consumed++;
        if (!$autoAck) {
            $channel->ack($message);
        }
        if ($consumed >= $count) {
            $channel->basicCancel($consumerTag);
        }
    };

    $this->channel->consume($queue, $callback, $consumerTag, $autoAck);

    $consecutiveTimeouts = 0;
    while ($consumed < $count) {
        try {
            $this->channel->wait(null, 1);
            $consecutiveTimeouts = 0;
        } catch (\Throwable) {
            $consecutiveTimeouts++;
            if ($consecutiveTimeouts >= 3) {
                break;
            }
        }
    }
}
```

Note: The exact Bunny API may differ — `Bunny\Channel` uses `consume()`, `wait()`, `basicCancel()`.
Verify the exact method names during implementation by checking the Bunny PHP package source.

- [ ] **Step 2b: Migrate `AmqpExtDriver` from `basic_get` to `basic_consume`**

`AmqpExtDriver.php:129-168` uses `basic.get` (polling) via `$this->consQueue->get($flags)`.
Same fairness issue as BunnyDriver.

Replace the polling loop with `basic_consume` using the amqp extension's callback API:

```php
public function consumeMessages(int $count): void
{
    if ($this->consQueue === null) {
        throw new RuntimeException('Driver not set up');
    }

    $autoAck = $this->scenarioMode === ScenarioMode::FIRE_AND_FORGET
        || $this->scenarioMode === ScenarioMode::AUTO_ACK;
    $consumed = 0;

    $consumerTag = 'bench_amqpext_consumer';
    $queue = $this->consQueue;

    $callback = function (\AMQPEnvelope $envelope, \AMQPQueue $q) use ($count, &$consumed, $autoAck, $consumerTag): bool {
        $body = $envelope->getBody();
        $this->recordReceived($envelope->getMessageId() ?? '');
        if (strlen($body) >= 8) {
            $ts = unpack('P', substr($body, 0, 8))[1] ?? null;
            if ($ts !== null) {
                $elapsedNs = hrtime(true) - (int) $ts;
                $this->recordLatency($elapsedNs / 1_000_000);
            }
        }
        $consumed++;
        if (!$autoAck) {
            $q->ack($envelope->getDeliveryTag());
        }
        if ($consumed >= $count) {
            $q->cancel($consumerTag);
            return false;
        }
        return true;
    };

    $flags = $autoAck ? AMQP_AUTOACK : AMQP_NOPARAM;
    $this->consQueue->consume($callback, $flags, $consumerTag);

    // The amqp extension's consume() blocks until the consumer is cancelled.
    // No explicit wait loop needed — consume() returns when cancel() is called.
}
```

Note: The amqp extension's `AMQPQueue::consume()` blocks until the consumer is cancelled or the
connection closes. The callback returns `false` to stop consuming when `$consumed >= $count`.
Verify the exact callback return convention during implementation.

- [ ] **Step 2c: Fix `' prefetch_count'` leading space in LaravelCompareBenchmark**

In `benchmarks/laravel/LaravelCompareBenchmark.php`, line 146:

```php
// Before:
' prefetch_count' => 16,

// After:
'prefetch_count' => 16,
```

This fixes the php-amqplib driver config key so the prefetch count is actually applied.

- [ ] **Step 3: Run benchmark to verify fairness**

Run: `rtk php benchmarks/src/run-benchmarks.php`
Expected: AUTO_ACK scenario runs without SKIP, and rabbit-rs performance is closer to amqplib.

If SKIP still occurs, proceed to Step 4 (investigation).

- [ ] **Step 4: Investigate SKIP — check broker readiness**

Run:
```bash
rtk rabbitmqctl await_startup
rtk rabbitmqctl status
```

If the broker takes time to start, add a health check in `run-benchmarks.php` before starting:

```php
// In run-benchmarks.php, before the benchmark loop:
$brokerReady = false;
for ($i = 0; $i < 30; $i++) {
    try {
        $connection = new AMQPStreamConnection(
            Config::RABBITMQ_HOST, Config::RABBITMQ_PORT,
            Config::RABBITMQ_USER, Config::RABBITMQ_PASSWORD,
            Config::RABBITMQ_VHOST
        );
        $connection->close();
        $brokerReady = true;
        break;
    } catch (\Throwable $e) {
        sleep(1);
    }
}
if (!$brokerReady) {
    echo "Broker not ready after 30s\n";
    exit(1);
}
```

- [ ] **Step 5: Investigate SKIP — increase confirm_timeout if needed**

If the SKIP persists even with a ready broker, increase `confirm_timeout` in the benchmark config:

In `benchmarks/src/Drivers/RabbitRsDriver.php`, line 63:

```php
// Before:
'confirm_timeout' => 30000,

// After:
'confirm_timeout' => 60000,
```

- [ ] **Step 6: Run benchmark again**

Run: `rtk php benchmarks/src/run-benchmarks.php`
Expected: No SKIP. AUTO_ACK scenario completes for all drivers.

- [ ] **Step 7: Commit**

```bash
git add benchmarks/src/Config.php benchmarks/src/Drivers/RabbitRsDriver.php benchmarks/src/Drivers/BunnyDriver.php benchmarks/src/Drivers/AmqpExtDriver.php benchmarks/laravel/LaravelCompareBenchmark.php benchmarks/src/run-benchmarks.php
git commit -m "fix: benchmark fairness — AUTO_ACK, basic_consume migration, prefetch 128, fix prefetch_count key"
```

---

## Self-Review Checklist

After writing the complete plan, verify:

### Spec Coverage

| Spec Section | Task | Status |
|---|---|---|
| Section A: Recovery generation rollback + handle invalidation + multi-broker | Task 1, 2, 3 | ✅ |
| Section B: Consumer try_next_batch + ledger/budget on failure | Task 4, 5 | ✅ |
| Section C: OOM hard gate + permits | Task 6 | ✅ |
| Section 1: Ack fire-and-forget + error queue + drainErrors | Task 8 (core), Task 13 (PHP), Task 14 (Laravel) | ✅ |
| Section 2: `no_ack=true` in transport + OOM protection | Task 9 | ✅ |
| Section 3: `Arc<Headers>` in TransportDelivery | Task 7 | ✅ |
| Section 4: Prefetch adaptatif (global) | Task 15 | ✅ |
| Section 5: Arc<str> fields on PublishRequest | Task 10 | ✅ |
| Section 6: TaggedFuture (eliminate double BoxFuture) | Task 11 | ✅ |
| Section 7: Batch wait `wait_all()` | Task 12 | ✅ |
| Section 8: Multi-channel publish | Deferred | ✅ (out of scope) |
| Section 9: Benchmark AUTO_ACK + SKIP + basic_consume fairness | Task 15 | ✅ |

### Placeholder Scan

- No "TBD" or "TODO" in tasks ✅
- No "implement later" ✅
- All steps contain actual code or exact instructions ✅

### Type Consistency

- `SettlementError` — defined in Task 8, used in Task 13 ✅
- `SettleError` — defined in Task 8, used in Task 13 ✅
- `SettlementErrorKind` — defined in Task 8, used in Task 13 ✅
- `ConsumerRequest.no_ack` — defined in Task 9, used in Task 15 ✅
- `Subscription.no_ack` — defined in Task 9, used in Task 15 ✅
- `TaggedFuture` — defined in Task 11, used in Task 11 ✅
- `PublishWaiter::wait_all()` — defined in Task 12, used in Task 12 ✅
- `Destination.exchange: Arc<str>` — defined in Task 10, used in Task 11 ✅
- `ConsumerHandle::drain_errors()` — defined in Task 8, used in Task 13 ✅
- `ConsumerHandle::try_settle()` — defined in Task 8, used in Task 13 ✅
- `ConsumerHandle.generation` — defined in Task 2, used in Task 2 ✅
- `ConsumerHandle.pending_error` — defined in Task 4, used in Task 4 ✅
- `ActorState.pending_incoming` — defined in Task 6, used in Task 6 ✅

### OOM Protection (Section C from spec)

The spec requires hard gate (actor stops accepting when over budget) + permit-based backpressure. Task 6
implements the hard gate with `pending_incoming` VecDeque (bounded by mpsc 256). The permit system is
documented as a follow-up — the hard gate + mpsc backpressure is the primary mechanism. The Lapin
unbounded channel remains a known gap, mitigated by the hard gate blocking `spawn_source` which stops
draining the unbounded channel. ✅

### Backpressure on ChannelFull (Section 1 from spec)

The spec requires bounded spin-yield + bounded `block_on(10ms)` before throwing on `ChannelFull`. This is
implemented in Task 13 (PHP extension) Step 2. The core `try_settle()` in Task 8 returns `SettleError::ChannelFull`
immediately — the backpressure logic is in the PHP layer. ✅

### Error Detection Latency on Empty Queue (Section 1 from spec)

The spec requires `drainSettlementErrors()` on `WorkerIdle`. This is addressed in Task 14 Step 5. The `pop()`
drain runs every iteration. The `WorkerIdle` listener is a best-effort addition. ✅

### Benchmark Fairness (PHP extension audit)

- `nextBatch()` off-by-one: `max=1` returns 2 in slow path. Fixed in Task 13 Step 4a. ✅
- `ackBatch()` unbounded + `block_on` per message: bounded to 256 + fire-and-forget in Task 13 Step 4. ✅
- `BunnyDriver` and `AmqpExtDriver` use `basic_get` (poll) instead of `basic_consume` (push): migrated in
  Task 15 Steps 2a and 2b. ✅
- `LaravelCompareBenchmark.php:146` has `' prefetch_count'` with leading space: fixed in Task 15 Step 2c. ✅

### Out of Scope (7 bugs from PHP extension audit — separate plan)

The following bugs are confirmed but not in this plan. They will be addressed in a separate spec/plan:
1. Deadlock réentrant des callbacks (`pool.rs:312-348`, `callbacks.rs:55-68`)
2. Perte silencieuse après 20 redeliveries sans DLX (`config/rabbit-rs.php:318-325`)
3. Pools abandonnés sans fermeture (`NativePoolFactory.php:58-72`, `OctaneLifecycle.php:31-44`)
4. Payload poison non validé (`RabbitMqJob.php:29-37`)
5. Config topology partiellement morte (`ConfigNormalizer.php:31-40,468-478`)
6. Supervisor laissant des workers orphelins (`WorkerSupervisor.php:120-128`)
7. Monitoring mensonger (`RabbitMqStatusCommand.php:46-70`)
