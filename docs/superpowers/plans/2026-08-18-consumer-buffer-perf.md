# Consumer Buffer & Performance Optimization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make rabbit-rs at least as fast as php-amqplib (publish 35-45K safe / consume 30-50K poll / 80-100K callback) and competitive with ext-amqp in blind mode (80-100K), without breaking the Laravel API.

**Architecture:** Actor pattern for cold path (channel creation, recovery, topology, QoS, consumer registration, stats). Hot path bypass for publish and consume: direct channel access via `now_or_never()` for publish, background pump + `flume` buffer for consume, lock-free queue for batched acks, and a PHP-side publish buffer to batch FFI crossings.

**Tech Stack:** Rust 1.96.0 (edition 2024), Lapin (async AMQP), Tokio (multi-thread runtime), ext-php-rs 0.15.15 (PHP extension), `flume` (bounded MPSC), `crossbeam-queue` (lock-free queue), `arc-swap` (hot channel swap).

## Global Constraints

- **Unsafe Rust is forbidden.** Do not weaken `#![forbid(unsafe_code)]` or the workspace lint configuration.
- **Rust is pinned to 1.96.0, edition 2024.** All code must compile on this toolchain.
- **Keep Lapin behind the `Transport` abstraction** so broker behavior remains mockable and replaceable.
- **Prefer typed errors with actionable context** over strings; configuration failures must identify their exact input path.
- **Never expose credentials, complete broker URIs, or private certificate material** through `Debug`, errors, metrics, or logs.
- **Do not retain Zend values, PHP objects, callbacks, requests, or service-container state** in Rust threads.
- **Keep queues, channels, in-flight work, retries, and replay buffers explicitly bounded.**
- **Delivery tokens and acknowledgements are connection-generation-aware.** Stale ACKs must be rejected so RabbitMQ can redeliver.
- **Recovery order remains deterministic:** connection, channels, exchanges, queues, bindings, QoS, then consumers.
- **Unconfirmed publications survive connection recovery only in bounded process memory** and are replayed with the same `message_id` and original deadline. Never describe in-memory replay as durable across a PHP process crash.
- **Publisher confirms, mandatory returns, timeouts, and terminal errors resolve each waiter once.** A mandatory return takes precedence over its following ACK.
- **A vhost owns a distinct AMQP connection.** Consumer channels remain dedicated; publisher channels may be pooled.
- **Runtime and connection registries are lazy and process-local.** A PID change invalidates inherited resources after a fork.
- **PHP API surface is preserved:** `Pool::publish()`, `Pool::publishBatch()`, `Consumer::next()`, `Delivery::ack()`, `Delivery::release()`, `Delivery::reject()` keep their signatures. New methods (`flush()`, `consume()`, Iterator) are additive only.
- **Run `rtk cargo fmt --all` after Rust edits**, then focused tests, then the full quality gate `rtk ./scripts/check.sh`.
- **Follow TDD for behavior changes:** add a focused failing test, observe the intended failure, implement minimally, rerun the focused test.
- **Use paused Tokio time and the scriptable mock transport** for deterministic asynchronous tests. Do not add real sleeps to unit tests.

## File Structure

This plan touches the following files. Each file has one clear responsibility:

### Core crate (`crates/rabbit-rs-core/`)

| File | Responsibility | Action |
|------|---------------|--------|
| `src/publisher/batcher.rs` | Batching of publishes before flush | Modify (spare-vec swap, cherry-pick) |
| `src/publisher/actor.rs` | Publisher actor: cold path + hot path bypass | Modify (immediate flush, `now_or_never`, `Arc<Channel>` exposure) |
| `src/publisher/confirms.rs` | ConfirmLedger for in-flight publish tracking | Modify (HashMap, pre-alloc, cherry-pick) |
| `src/publisher/mod.rs` | Publisher public types and config | Modify (add `SafetyMode` enum) |
| `src/publisher/pump.rs` | Async pump for blind mode | **Create** |
| `src/consumer/set.rs` | ConsumerSet spawn + ConsumerHandle | Modify (flume buffer, background pump) |
| `src/consumer/actor.rs` | Consumer actor: multiplexing + dispatch | Modify (ack batching, generation-aware buffer) |
| `src/consumer/delivery.rs` | Delivery + DeliveryToken | Modify (lock-free ack queue) |
| `src/transport.rs` | Transport trait definitions | Modify (add `publish_batch` to trait) |
| `src/transport/lapin.rs` | Lapin transport implementation | Modify (frame_max, TCP_NODELAY, move clones, batch frames) |
| `src/metrics.rs` | Atomic counters + histograms | Minimal changes (hot path already increments atomics) |
| `src/config.rs` | Configuration types | Modify (add `safety` to PublisherConfigSection) |
| `src/runtime.rs` | Tokio runtime factory | Modify (worker_threads config) |
| `src/client.rs` | ClientPool facade | Modify (expose `flush()`, wire safety modes) |
| `tests/publisher_safety.rs` | Publisher safety integration tests | Modify (add tests for new behaviors) |
| `tests/consumer_buffer.rs` | **Create** — buffered consumer tests | **Create** |

### PHP extension crate (`crates/rabbit-rs-php/`)

| File | Responsibility | Action |
|------|---------------|--------|
| `src/conversion.rs` | PHP array → Rust conversion | Modify (optimize hot path, defer format!) |
| `src/classes/pool.rs` | PHP Pool class | Modify (PHP-side buffer, `flush()`, safety modes) |
| `src/classes/consumer.rs` | PHP Consumer class | Modify (`consume()` callback, Iterator) |
| `src/classes/delivery.rs` | PHP Delivery class | Modify (lock-free ack, zero-copy payload) |
| `stubs/rabbit_rs.stub.php` | PHP stub definitions | Modify (add `flush()`, `consume()`, Iterator, fix stats docblock) |
| `Cargo.toml` | PHP crate dependencies | Modify (add `flume`, `crossbeam-queue`, `arc-swap`) |

### Root

| File | Responsibility | Action |
|------|---------------|--------|
| `Cargo.toml` (core) | Core crate dependencies | Modify (add `flume`, `crossbeam-queue`, `arc-swap`) |

---

## Task 0: Cherry-pick PR #4 commits onto fresh branch

**Files:**
- All files touched by the 6 commits on `perf/perf-wave-1`

**Interfaces:**
- Consumes: commits `3a3fb76`, `3d117df`, `415c50b`, `b424601`, `95142ea`, `6cb343f` from `perf/perf-wave-1`
- Produces: a clean branch from `main` with all 6 optimizations applied

- [ ] **Step 1: Create a fresh branch from main**

```bash
git checkout main
git pull origin main
git checkout -b perf/consumer-buffer-perf
```

- [ ] **Step 2: Cherry-pick the 6 commits in dependency order**

The corrective commit `6cb343f` must come after `95142ea`. The others are independent.

```bash
git cherry-pick 3a3fb76  # defer header path formatting to error branches
git cherry-pick 3d117df  # split add_header_bytes key-overflow path
git cherry-pick 415c50b  # spare-vec swap in Batcher::take
git cherry-pick b424601  # move exchange/routing_key in Lapin publish
git cherry-pick 95142ea  # replace BTreeMap with HashMap in ConfirmLedger
git cherry-pick 6cb343f  # restore deterministic drain order and correct ledger allocation
```

- [ ] **Step 3: Resolve any conflicts**

If cherry-pick conflicts arise (the branch diverged from `v0.0.2`), resolve them by keeping the optimization logic while adapting to any changes on `main` since `v0.0.2`.

- [ ] **Step 4: Verify the build compiles**

Run: `rtk cargo build --workspace`
Expected: SUCCESS

- [ ] **Step 5: Run focused tests for the cherry-picked areas**

```bash
rtk cargo test -p rabbit-rs-core --test publisher_safety
rtk cargo test -p rabbit-rs-core --test ffi_conversion
```
Expected: PASS

- [ ] **Step 6: Commit the cherry-picks (if not already committed by cherry-pick)**

The cherry-picks are individual commits. If any required conflict resolution, squash those into the respective commit.

- [ ] **Step 7: Run the full quality gate**

Run: `rtk ./scripts/check.sh`
Expected: PASS

---

## Task 1: Lapin/Tokio tuning (Phase 1d)

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs:34-35` (ConnectionProperties)
- Modify: `crates/rabbit-rs-core/src/runtime.rs:43-50` (TokioRuntimeFactory)
- Test: `crates/rabbit-rs-core/tests/publisher_safety.rs`

**Interfaces:**
- Consumes: `BrokerConfig` (from `config.rs:166-199`) with `heartbeat` field
- Produces: tuned `ConnectionProperties` with `frame_max=1MB`, `heartbeat` from config; Tokio runtime with configurable `worker_threads`

- [ ] **Step 1: Write the failing test for frame_max configuration**

Add a test in `crates/rabbit-rs-core/tests/publisher_safety.rs` (or a new `tests/transport_tuning.rs` file) that verifies the transport sets `frame_max` on `ConnectionProperties`. Since `ConnectionProperties` is a Lapin type, test this via the mock transport or by inspecting the configured properties.

```rust
#[tokio::test]
async fn frame_max_is_set_to_one_megabyte() {
    let config = test_broker_config();
    let transport = LapinTransport::new();
    // The transport should negotiate frame_max = 1_048_576 (1 MB)
    // Verify via the connection properties or a mock that captures the configured frame_max
    // This may require exposing the ConnectionProperties construction for testing
    let properties = transport.connection_properties(&config);
    assert_eq!(properties.frame_max(), 1_048_576);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core transport_tuning`
Expected: FAIL — `connection_properties` method doesn't exist, or `frame_max` is not set

- [ ] **Step 3: Add frame_max to ConnectionProperties in lapin.rs**

In `crates/rabbit-rs-core/src/transport/lapin.rs`, modify the `ConnectionProperties` construction (currently at line 34-35) to set `frame_max`:

```rust
let properties = ConnectionProperties::default()
    .with_frame_max(1_048_576)  // 1 MB — up from 128 KB default
    .with_connection_name(format!("rabbit-rs:{}", config.name).into());
```

Note: Check Lapin's API for the exact method name (`with_frame_max` or `frame_max`). If Lapin uses a different API, adapt accordingly. The Lapin version in `Cargo.lock` determines the available methods.

- [ ] **Step 4: Add TCP_NODELAY via Lapin's IO configuration**

If Lapin exposes `TCP_NODELAY` via `ConnectionProperties` or its IO loop configuration, add it. If not (Lapin may not expose this), document why and skip — TCP_NODELAY may need to be set at the socket level which Lapin manages internally.

```rust
// If Lapin supports it:
let properties = ConnectionProperties::default()
    .with_frame_max(1_048_576)
    .with_connection_name(format!("rabbit-rs:{}", config.name).into());
    // TCP_NODELAY: Lapin may set this by default or via an IO config. Check Lapin docs.
```

- [ ] **Step 5: Add worker_threads configuration to TokioRuntimeFactory**

In `crates/rabbit-rs-core/src/runtime.rs`, modify `TokioRuntimeFactory::create` (lines 43-50) to accept a configurable worker thread count. Add a field to the factory:

```rust
#[derive(Debug)]
struct TokioRuntimeFactory {
    worker_threads: usize,
}

impl Default for TokioRuntimeFactory {
    fn default() -> Self {
        Self {
            worker_threads: 1,  // I/O-bound: 1 worker thread reduces scheduling overhead
        }
    }
}

impl RuntimeFactory for TokioRuntimeFactory {
    fn create(&self) -> io::Result<Runtime> {
        let mut builder = Builder::new_multi_thread()
            .thread_name("rabbit-rs")
            .enable_all();
        if self.worker_threads > 0 {
            builder = builder.worker_threads(self.worker_threads);
        }
        builder.build()
    }
}
```

Update `RuntimeRegistry::new()` to use `TokioRuntimeFactory::default()` (or pass the config).

- [ ] **Step 6: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core transport_tuning`
Expected: PASS

- [ ] **Step 7: Run focused publisher tests**

```bash
rtk cargo test -p rabbit-rs-core --test publisher_safety
rtk cargo test -p rabbit-rs-core
```
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/rabbit-rs-core/src/transport/lapin.rs crates/rabbit-rs-core/src/runtime.rs crates/rabbit-rs-core/tests/transport_tuning.rs
git commit -m "perf(transport): tune frame_max to 1MB and worker_threads to 1 for I/O-bound throughput"
```

---

## Task 2: Immediate flush for explicit batches (Phase 1a)

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:413-442` (flush logic in `accept_publish` and the `select!` loop)
- Test: `crates/rabbit-rs-core/tests/publisher_safety.rs`

**Interfaces:**
- Consumes: `mpsc::Receiver<Command>` (the actor's command receiver)
- Produces: immediate flush when mpsc is empty after receiving a batch

- [ ] **Step 1: Write the failing test**

Add a test that verifies a small batch (e.g., 64 messages) flushes immediately without waiting 1ms. Use paused Tokio time to detect if the 1ms deadline was set:

```rust
#[tokio::test(start_paused = true)]
async fn small_batch_flushes_immediately_when_mpsc_empty() {
    let (channel, mock) = MockPublisherChannel::pair();
    let handle = PublisherActor::spawn(
        Arc::new(channel),
        PublisherConfig::with_flags(256, 2 * 1024 * 1024, Duration::from_millis(1), 1024, Duration::from_secs(30), true, true),
    );

    // Send 64 messages (below the 256 threshold)
    let mut waiters = Vec::new();
    for i in 0..64 {
        let request = test_publish_request(i);
        waiters.push(handle.try_publish(request).unwrap());
    }

    // Advance time by 0ms — the batch should have flushed immediately
    // because the mpsc is empty after the last message
    tokio::time::advance(Duration::from_millis(0)).await;

    // The mock should have received 64 publish calls
    // Wait for confirms to resolve
    for waiter in waiters {
        let _ = waiter.wait().await;
    }
    assert_eq!(mock.publish_count(), 64);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core small_batch_flushes_immediately`
Expected: FAIL — batch waits 1ms before flushing, so `publish_count()` is 0 before time advances

- [ ] **Step 3: Implement immediate flush when mpsc is empty**

In `crates/rabbit-rs-core/src/publisher/actor.rs`, modify the `accept_publish` function (around line 436-442) to check if the mpsc receiver is empty after the last message in a batch. If empty, flush immediately:

```rust
// In accept_publish, after pushing to batch:
if state.batch.push(retained, payload_len) {
    flush_batch(state).await;
    state.flush_deadline = None;
} else if state.flush_deadline.is_none() {
    // Check if there are more commands queued
    // If the mpsc is empty, flush immediately
    // We can't directly check rx.is_empty() from accept_publish,
    // but we can signal the select! loop to check
    state.flush_deadline = Some(time::Instant::now() + state.flush_interval());
}
```

The actual fix is in the main `select!` loop. After processing a batch of `Command::Publish` messages, check if the receiver is empty. If it is and the batch is non-empty, flush immediately instead of waiting for the deadline:

```rust
// In the select! loop, after draining publishes:
// After the drain loop, if batch is non-empty and receiver is empty, flush now
if !state.batch.is_empty() && receiver.is_empty() {
    flush_batch(state).await;
    state.flush_deadline = None;
}
```

The key insight: the `select!` loop currently drains commands in a loop, then waits for either more commands or the deadline. The fix is: after the drain loop, if the receiver is empty and the batch has items, flush immediately.

Look at the existing `run_actor` loop structure (around lines 380-450) and add the empty-check after the drain loop.

- [ ] **Step 4: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core small_batch_flushes_immediately`
Expected: PASS

- [ ] **Step 5: Run full publisher safety tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher_safety`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/actor.rs crates/rabbit-rs-core/tests/publisher_safety.rs
git commit -m "perf(publisher): flush immediately when mpsc is empty after explicit batch"
```

---

## Task 3: Reduce per-message FFI conversion overhead (Phase 1b)

**Files:**
- Modify: `crates/rabbit-rs-php/src/conversion.rs:85-143` (publish_with_budget)
- Modify: `crates/rabbit-rs-php/src/conversion.rs:327-363` (optional_headers)
- Modify: `crates/rabbit-rs-php/src/conversion.rs:406-414` (reject_unknown_keys)
- Test: `crates/rabbit-rs-core/benches/ffi_conversion.rs` or a new test

**Interfaces:**
- Consumes: `&ZendHashTable` (PHP array from publish/publishBatch)
- Produces: `NativePublish` with reduced allocations

- [ ] **Step 1: Write the failing test for deferred header path formatting**

The cherry-picked commit `3a3fb76` already defers `format!("{path}.headers.{key}")` to the error branch. Verify this is in place. If the cherry-pick succeeded, this test should already pass. If not, write a test that asserts no `format!` allocation occurs on the success path:

```rust
// In a conversion test, verify that optional_headers with valid headers
// does not allocate a format! string for each header key on the success path.
// This is hard to test directly; instead, benchmark the difference.
```

Since this is already cherry-picked (Task 0), verify it's present:

- [ ] **Step 2: Verify cherry-pick `3a3fb76` is present**

Check that `optional_headers` in `conversion.rs` no longer calls `format!("{path}.headers.{key}")` on the success path. The `format!` should only appear in error branches.

- [ ] **Step 3: Verify cherry-pick `3d117df` is present**

Check that `add_header_bytes` has been split into a fast path (no overflow) and slow path (overflow detected), preserving correctness without the per-key branch.

- [ ] **Step 4: Make reject_unknown_keys optional via a debug flag**

In `crates/rabbit-rs-php/src/conversion.rs`, add a `debug_validation: bool` parameter (or a compile-time `cfg` flag) to `publish_with_budget`. When disabled (production), skip `reject_unknown_keys`. When enabled (debug/tests), keep it.

```rust
fn publish_with_budget(
    table: &ZendHashTable,
    path: &str,
    budget: &mut ConversionBudget,
    validate_keys: bool,  // new parameter
) -> Result<NativePublish, String> {
    if validate_keys {
        reject_unknown_keys(table, path, &[
            "broker", "exchange", "routing_key", "payload",
            "message_id", "content_type", "correlation_id",
            "headers", "delay_ms", "timeout_ms",
        ])?;
    }
    // ... rest of the function
}
```

Update callers (`publish` and `publish_batch`) to pass `validate_keys` based on a `#[cfg(debug_assertions)]` flag or a runtime config flag.

- [ ] **Step 5: Pre-compute known key positions**

Replace the linear `allowed.contains(&key.as_str())` scan in `reject_unknown_keys` with a pre-computed set. Since the allowed keys are static, use a `phf::Set` or a sorted array with binary search, or simply use a `const` array with a more efficient lookup. For a 10-element list, the linear scan is already O(10) — the bigger win is skipping it entirely in production (Step 4).

- [ ] **Step 6: Run conversion tests**

```bash
rtk cargo test -p rabbit-rs-core --test ffi_conversion
rtk cargo test -p rabbit-rs-php  # if PHP tests exist
```
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/rabbit-rs-php/src/conversion.rs
git commit -m "perf(ffi): skip reject_unknown_keys in production, defer format! to error branches"
```

---

## Task 4: Reduce per-message clones (Phase 1c)

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:590-637` (into_transport_request)
- Modify: `crates/rabbit-rs-core/src/transport.rs:249-279` (PublishRequest struct)
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs:68-101` (PublishRequest in publisher module)
- Test: `crates/rabbit-rs-core/tests/publisher_safety.rs`

**Interfaces:**
- Consumes: `PublishRequest` (publisher module version with `Destination`, `Bytes` payload)
- Produces: `PublishRequest` (transport version) with reduced clones

- [ ] **Step 1: Verify cherry-pick `415c50b` is present (Batcher spare-vec swap)**

Check that `Batcher::take` in `crates/rabbit-rs-core/src/publisher/batcher.rs` uses a spare-vec swap instead of `std::mem::take`.

- [ ] **Step 2: Verify cherry-pick `b424601` is present (Lapin move exchange/routing_key)**

Check that `LapinPublisherChannel::publish` in `crates/rabbit-rs-core/src/transport/lapin.rs` moves `exchange` and `routing_key` instead of cloning them. The `request.exchange.clone().into()` at lines 149-150 should be `request.exchange.into()` (or equivalent move).

- [ ] **Step 3: Use Arc<str> for exchange/routing_key/message_id in PublishRequest**

In `crates/rabbit-rs-core/src/publisher/mod.rs`, change `Destination` (lines 30-44) to use `Arc<str>` instead of `String`:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Destination {
    pub exchange: Arc<str>,
    pub routing_key: Arc<str>,
}

impl Destination {
    #[must_use]
    pub fn new(exchange: impl Into<Arc<str>>, routing_key: impl Into<Arc<str>>) -> Self {
        Self {
            exchange: exchange.into(),
            routing_key: routing_key.into(),
        }
    }
}
```

Similarly, change `MessageProperties::message_id` (line 48) from `String` to `Arc<str>`.

- [ ] **Step 4: Update into_transport_request to use Arc<str>**

In `crates/rabbit-rs-core/src/publisher/actor.rs` (lines 590-637), update `into_transport_request` to avoid cloning `Arc<str>` — just clone the `Arc` (refcount increment, no heap allocation):

```rust
TransportRequest {
    exchange: request.destination.exchange.clone(),  // Arc clone = refcount bump
    routing_key: request.destination.routing_key.clone(),
    payload: request.payload.clone(),  // Bytes clone = refcount bump
    // ...
}
```

Note: The transport-layer `PublishRequest` (in `transport.rs`) currently uses `String` for exchange/routing_key. Either:
a) Change the transport trait to accept `Arc<str>` / `&str` (preferred — avoids the Lapin clone), or
b) Keep transport `String` but only convert once at the transport boundary (the Lapin adapter).

Option (a) is better: change `transport::PublishRequest` to use `Arc<str>` for exchange/routing_key, and in the Lapin adapter, convert to Lapin's `ShortString` once (the `b424601` cherry-pick already moves instead of cloning).

- [ ] **Step 5: Update all call sites**

Find all places that construct `Destination` or `MessageProperties` and update them to pass `Arc<str>` or `impl Into<Arc<str>>`. The conversion layer (`conversion.rs`) currently creates `String` — it will now create `Arc<str>` from `String` via `.into()`.

- [ ] **Step 6: Write a test that verifies reduced clones**

Add a test that publishes a batch and verifies the exchange/routing_key are not re-allocated per message. This is hard to test directly (Rust doesn't expose allocation counters easily), so test behaviorally:

```rust
#[tokio::test]
async fn batch_publish_uses_arc_refcount_for_destination() {
    // Publish 100 messages to the same exchange/routing_key
    // Verify all 100 succeed (behavioral test — the Arc optimization is internal)
    let (channel, mock) = MockPublisherChannel::pair();
    let handle = PublisherActor::spawn(Arc::new(channel), test_config());
    let mut waiters = Vec::new();
    for i in 0..100 {
        let request = PublishRequest::new(
            Destination::new("test-exchange", "test-key"),
            Bytes::from_static(b"payload"),
            MessageProperties::new(format!("msg-{i}")),
            Instant::now() + Duration::from_secs(30),
        );
        waiters.push(handle.try_publish(request).unwrap());
    }
    for w in waiters {
        let _ = w.wait().await.unwrap();
    }
    assert_eq!(mock.publish_count(), 100);
}
```

- [ ] **Step 7: Run tests**

```bash
rtk cargo test -p rabbit-rs-core --test publisher_safety
rtk cargo test -p rabbit-rs-core
```
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/mod.rs crates/rabbit-rs-core/src/publisher/actor.rs crates/rabbit-rs-core/src/transport.rs
git commit -m "perf(publisher): use Arc<str> for exchange/routing_key/message_id to eliminate per-message clones"
```

---

## Task 5: Add flume dependency and create buffered consumer infrastructure (Phase 2a — part 1)

**Files:**
- Modify: `crates/rabbit-rs-core/Cargo.toml` (add `flume` dependency)
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs` (add flume buffer + background pump)
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (adapt to flume-buffered deliveries)
- Test: `crates/rabbit-rs-core/tests/consumer_buffer.rs` (new file)

**Interfaces:**
- Consumes: `DeliveryStream` (from `transport.rs:449-452`), `Subscription` (from `set.rs:25-96`)
- Produces: `ConsumerHandle::next()` with fast path (`flume::try_recv()`) and slow path (`flume::recv_timeout()`)

- [ ] **Step 1: Add flume to core Cargo.toml**

In `crates/rabbit-rs-core/Cargo.toml`, add under `[dependencies]`:

```toml
flume = "0.11"
```

- [ ] **Step 2: Write the failing test for buffered consumer next()**

Create `crates/rabbit-rs-core/tests/consumer_buffer.rs`:

```rust
use rabbit_rs_core::consumer::*;
use std::time::Duration;
use tokio::time;

#[tokio::test(start_paused = true)]
async fn buffered_next_returns_from_flume_fast_path() {
    // Setup: create a ConsumerSet with a mock DeliveryStream
    // that produces deliveries. The background pump fills the flume buffer.
    // next() should return via try_recv() without crossing mpsc+oneshot.

    let (consumer_handle, mock) = setup_buffered_consumer(3).await;

    // The background pump should pre-fetch deliveries into the flume buffer.
    // Advance time to let the pump fill the buffer.
    time::advance(Duration::from_millis(10)).await;

    // next() should return immediately from the buffer
    let delivery = consumer_handle.next().await.unwrap();
    assert_eq!(delivery.payload, Bytes::from_static(b"msg-0"));

    // Verify fast path was used (no mpsc crossing)
    // This can be checked via metrics or a counter on the mock
    assert!(mock.fast_path_used());
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer_buffer`
Expected: FAIL — flume buffer doesn't exist yet

- [ ] **Step 4: Add flume buffer to ConsumerHandle**

In `crates/rabbit-rs-core/src/consumer/set.rs`, modify `ConsumerHandle` (lines 201-207) to hold a `flume::Receiver<Delivery>`:

```rust
#[derive(Clone, Debug)]
pub struct ConsumerHandle {
    commands: mpsc::Sender<ConsumerCommand>,
    buffer_rx: flume::Receiver<BufferedDelivery>,
    metrics: Metrics,
    closed: Arc<AtomicBool>,
    next_waiter_id: Arc<AtomicU64>,
}
```

Where `BufferedDelivery` wraps `Delivery` with the connection generation:

```rust
pub(crate) struct BufferedDelivery {
    pub delivery: Delivery,
    pub generation: u64,
}
```

- [ ] **Step 5: Modify ConsumerSet::spawn to create the flume channel and background pump**

In `crates/rabbit-rs-core/src/consumer/set.rs`, modify `spawn_with_metrics` (lines 118-172). After creating the Lapin consumer stream, create a `flume::bounded(buffer_size)` channel and spawn a background pump task per subscription:

```rust
// Calculate buffer size: 1.5x prefetch, rounded up
let buffer_size = (subscription.prefetch as usize * 3 / 2).max(1);
let (buffer_tx, buffer_rx) = flume::bounded(buffer_size);

// Spawn background pump task
let pump_metrics = metrics.clone();
tokio::spawn(async move {
    loop {
        match stream.next().await {
            Some(Ok(delivery)) => {
                pump_metrics.record_delivery();
                // Convert TransportDelivery to Delivery, create token
                let buffered = BufferedDelivery { delivery, generation };
                if buffer_tx.send_async(buffered).await.is_err() {
                    break; // buffer closed, consumer dropped
                }
            }
            Some(Err(_)) | None => break,
        }
    }
});
```

The pump task replaces the current `spawn_source` function (lines 174-193). Instead of pumping into the actor's mpsc, it pumps into the flume buffer.

- [ ] **Step 6: Modify ConsumerHandle::next to use flume fast path**

In `crates/rabbit-rs-core/src/consumer/set.rs`, modify `next()` (lines 246-264):

```rust
pub async fn next(&self) -> Result<Delivery, ConsumerError> {
    // Fast path: try_recv from flume buffer (sub-microsecond, no block_on)
    match self.buffer_rx.try_recv() {
        Ok(buffered) => {
            // Check generation — discard stale deliveries
            // (RabbitMQ will redeliver them)
            return Ok(buffered.delivery);
        }
        Err(flume::TryRecvError::Empty) => {}
        Err(flume::TryRecvError::Disconnected) => return Err(ConsumerError::closed()),
    }

    // Slow path: wait for the background pump to deliver
    match self.buffer_rx.recv_async().await {
        Ok(buffered) => Ok(buffered.delivery),
        Err(flume::RecvError::Disconnected) => Err(ConsumerError::closed()),
    }
}
```

Note: The current `next()` uses `mpsc + oneshot` to the actor. The new `next()` reads from the flume buffer directly. The actor is no longer in the hot path for `next()`.

- [ ] **Step 7: Handle recovery — generation-aware buffer invalidation**

When the connection drops:
1. The background pump task is cancelled (Lapin consumer stream closes)
2. The flume buffer is flushed (deliveries in the buffer are stale)
3. The actor recreates the consumer after reconnection and spawns a new pump
4. Deliveries in the buffer at crash time are marked with connection generation. If `buffered.generation != current_generation`, they are discarded (RabbitMQ redelivers them).

Add a generation check in `next()`:

```rust
match self.buffer_rx.try_recv() {
    Ok(buffered) if buffered.generation == self.current_generation() => {
        return Ok(buffered.delivery);
    }
    Ok(_stale) => {
        // Discard stale delivery, try again
        continue;
    }
    Err(flume::TryRecvError::Empty) => {}
    Err(flume::TryRecvError::Disconnected) => return Err(ConsumerError::closed()),
}
```

- [ ] **Step 8: Update the actor to handle the new architecture**

The consumer actor (`actor.rs`) no longer receives `Incoming` commands (the pump bypasses it). The actor still handles:
- `Settle` commands (for ack/nack/reject — until Phase 2b replaces this with a lock-free queue)
- `UpdateGeneration` (for recovery)
- `Close`

Remove the `Incoming` command variant and the `spawn_source` function. The `buffers` HashMap in `ActorState` is no longer needed — the flume buffer replaces it.

- [ ] **Step 9: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core --test consumer_buffer`
Expected: PASS

- [ ] **Step 10: Run all consumer tests**

```bash
rtk cargo test -p rabbit-rs-core consumer
```
Expected: PASS

- [ ] **Step 11: Commit**

```bash
git add crates/rabbit-rs-core/Cargo.toml crates/rabbit-rs-core/src/consumer/set.rs crates/rabbit-rs-core/src/consumer/actor.rs crates/rabbit-rs-core/tests/consumer_buffer.rs
git commit -m "feat(consumer): add flume buffer + background pump for buffered consume (Phase 2a)"
```

---

## Task 6: Add crossbeam-queue and implement batched ack with multiple=true (Phase 2b)

**Files:**
- Modify: `crates/rabbit-rs-core/Cargo.toml` (add `crossbeam-queue`)
- Modify: `crates/rabbit-rs-core/src/consumer/delivery.rs` (lock-free ack queue)
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (background drain of ack queue)
- Modify: `crates/rabbit-rs-core/src/transport.rs:441` (verify ack with multiple=true is supported)
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs:258` (verify Lapin passes multiple flag)
- Test: `crates/rabbit-rs-core/tests/consumer_buffer.rs`

**Interfaces:**
- Consumes: `ConsumerChannel::ack(delivery_tag, multiple)` (already in trait at `transport.rs:441`)
- Produces: `Delivery::ack()` returns immediately (push to lock-free queue), background drain sends `basic_ack(highest_tag, multiple=true)`

- [ ] **Step 1: Add crossbeam-queue to core Cargo.toml**

```toml
crossbeam-queue = "0.3"
```

- [ ] **Step 2: Write the failing test for batched ack**

```rust
#[tokio::test(start_paused = true)]
async fn batched_ack_uses_multiple_flag() {
    let (consumer_handle, mock) = setup_buffered_consumer(16).await;

    // Receive and ack 16 deliveries
    let mut tags = Vec::new();
    for _ in 0..16 {
        let delivery = consumer_handle.next().await.unwrap();
        delivery.ack().unwrap();  // should return immediately
        tags.push(delivery.delivery_tag());
    }

    // Advance time to let the background drain fire
    time::advance(Duration::from_millis(2)).await;

    // The mock should have received a single ack with multiple=true
    // and the highest delivery tag
    let ack_calls = mock.ack_calls();
    assert_eq!(ack_calls.len(), 1);
    assert_eq!(ack_calls[0].delivery_tag, tags[15]);
    assert!(ack_calls[0].multiple);
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core batched_ack`
Expected: FAIL — ack still uses mpsc + oneshot

- [ ] **Step 4: Add lock-free ack queue to DeliveryToken**

In `crates/rabbit-rs-core/src/consumer/delivery.rs`, add a `crossbeam_queue::SegQueue<PendingAck>` to the `DeliveryTokenInner` (or to a shared structure on the `ConsumerHandle`):

```rust
pub(crate) struct PendingAck {
    pub delivery_tag: u64,
    pub settlement: Settlement,
    pub generation: u64,
    pub reserved_at: Instant,
}

pub(crate) type AckQueue = crossbeam_queue::SegQueue<PendingAck>;
```

Add an `Arc<AckQueue>` to `DeliveryTokenInner` (lines 208-222):

```rust
pub(crate) struct DeliveryTokenInner {
    // ... existing fields ...
    pub ack_queue: Arc<AckQueue>,
}
```

- [ ] **Step 5: Modify Delivery::ack to push to lock-free queue**

In `crates/rabbit-rs-core/src/consumer/delivery.rs`, modify `DeliveryToken::settle` (lines 148-197). For `Settlement::Ack`, instead of sending via mpsc + oneshot:

```rust
async fn settle(&self, settlement: Settlement) -> Result<(), ConsumerError> {
    self.inner
        .state
        .compare_exchange(
            DeliveryState::Pending as u8,
            TRANSITIONING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map_err(|_| ConsumerError::already_settled())?;

    // For Ack: push to lock-free queue and return immediately
    if matches!(settlement, Settlement::Ack) {
        self.inner.ack_queue.push(PendingAck {
            delivery_tag: self.inner.delivery_tag,
            settlement,
            generation: self.inner.generation,
            reserved_at: self.inner.reserved_at,
        });
        self.inner.state.store(DeliveryState::Acked as u8, Ordering::Release);
        return Ok(());
    }

    // For Release/Reject: still use mpsc (delayed release needs actor)
    // ... existing mpsc + oneshot path ...
}
```

Note: `settle` is currently `async` because it awaits the oneshot. The Ack path no longer needs to be async (it just pushes to a queue). However, the `Release`/`Reject` paths still need async. Keep the function async for API compatibility, but the Ack path will complete without any `.await`.

- [ ] **Step 6: Add background drain task to the consumer actor**

In `crates/rabbit-rs-core/src/consumer/actor.rs`, add a background task that drains the `AckQueue` every 1ms or when it reaches a threshold (16 tags):

```rust
// Spawn a background ack drainer
let ack_queue = ack_queue.clone();
let channel = channel.clone();
tokio::spawn(async move {
    let mut interval = time::interval(Duration::from_millis(1));
    loop {
        interval.tick().await;
        let mut tags = Vec::new();
        while let Some(pending) = ack_queue.pop() {
            // Check generation — skip stale
            if pending.generation != current_generation {
                continue;
            }
            tags.push(pending);
        }
        if tags.is_empty() {
            continue;
        }
        // Coalesce: find the highest tag, send one ack with multiple=true
        // If tags are non-contiguous (gaps), send multiple acks up to each gap
        let mut sorted: Vec<_> = tags.iter().map(|t| t.delivery_tag).collect();
        sorted.sort_unstable();
        sorted.dedup();
        // Send ack for the highest tag with multiple=true
        if let Some(&highest) = sorted.last() {
            let _ = channel.ack(highest, true).await;
        }
        // Record metrics
        for pending in &tags {
            metrics.record_ack(pending.reserved_at.elapsed());
        }
    }
});
```

Note: The `multiple=true` flag tells RabbitMQ to ack all messages up to and including `delivery_tag`. If there are gaps in the sequence (e.g., tags 1,2,5,6 — tags 3,4 were nacked), we need to send separate acks: `ack(2, true)` for 1,2 and `ack(6, true)` for 5,6. The coalescing logic should detect gaps and split accordingly.

- [ ] **Step 7: Handle the AckQueue on the ConsumerHandle**

The `AckQueue` must be shared between the `DeliveryToken` (which pushes) and the background drainer (which pops). Create the queue in `ConsumerSet::spawn_with_metrics` and pass an `Arc<AckQueue>` to both the `DeliveryTokenInner` and the drainer task.

- [ ] **Step 8: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core batched_ack`
Expected: PASS

- [ ] **Step 9: Run all consumer tests**

```bash
rtk cargo test -p rabbit-rs-core consumer
```
Expected: PASS

- [ ] **Step 10: Commit**

```bash
git add crates/rabbit-rs-core/Cargo.toml crates/rabbit-rs-core/src/consumer/delivery.rs crates/rabbit-rs-core/src/consumer/actor.rs crates/rabbit-rs-core/tests/consumer_buffer.rs
git commit -m "feat(consumer): batched ack with lock-free queue and multiple=true (Phase 2b)"
```

---

## Task 7: Update PHP Consumer class for buffered next() (Phase 2a — PHP side)

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs:33-55` (next method)
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` (update if needed)
- Test: `crates/rabbit-rs-php/tests/` (PHPT tests)

**Interfaces:**
- Consumes: `ConsumerHandle::next()` (now flume-buffered)
- Produces: `Consumer::next(int $timeoutMs): ?Delivery` with same signature, faster internally

- [ ] **Step 1: Update the PHP Consumer::next to use the buffered handle**

The Rust `ConsumerHandle::next()` now reads from a flume buffer. The PHP `Consumer::next()` (lines 33-55) already calls `self.handle.next()` via `block_on`. The only change needed is: the `block_on` now completes faster because `next()` returns from the flume buffer.

However, we can optimize further: if the flume buffer has a delivery available, we don't need `block_on` at all — `try_recv()` is synchronous. Modify `next()`:

```rust
pub fn next(&self, timeoutMs: i64) -> PhpResult<Option<Delivery>> {
    self.ensure_open("Goopil\\RabbitRs\\Consumer::next")?;
    let timeout = u64::try_from(timeoutMs).map_err(|_| {
        ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
            "timeoutMs must be a non-negative integer".to_owned(),
        )
    })?;

    // Fast path: try_recv without block_on
    // (ConsumerHandle exposes a try_next() method that wraps flume::try_recv)
    if let Some(delivery) = self.handle.try_next() {
        return Ok(Some(Delivery::new(
            delivery,
            self.runtime.clone(),
            self.pid,
        )));
    }

    // Slow path: block_on with timeout
    match self.runtime.block_on(async {
        time::timeout(
            std::time::Duration::from_millis(timeout),
            self.handle.next(),
        )
        .await
    }) {
        Ok(Ok(delivery)) => Ok(Some(Delivery::new(
            delivery,
            self.runtime.clone(),
            self.pid,
        ))),
        Ok(Err(error)) => consumer_exception(&error),
        Err(_) => Ok(None),
    }
}
```

This requires adding a `try_next()` method to `ConsumerHandle` in the core crate:

```rust
// In consumer/set.rs
impl ConsumerHandle {
    pub fn try_next(&self) -> Option<Delivery> {
        match self.buffer_rx.try_recv() {
            Ok(buffered) if buffered.generation == self.current_generation() => {
                Some(buffered.delivery)
            }
            Ok(_stale) => {
                // Discard stale, try again recursively (bounded by buffer size)
                self.try_next()
            }
            Err(_) => None,
        }
    }
}
```

- [ ] **Step 2: Run PHPT tests or PHP smoke tests**

Run: `rtk cargo test -p rabbit-rs-php` (if Rust-level PHP tests exist)
Run: `./scripts/install.sh && php -r '...'` (manual smoke test)

- [ ] **Step 3: Commit**

```bash
git add crates/rabbit-rs-php/src/classes/consumer.rs crates/rabbit-rs-core/src/consumer/set.rs
git commit -m "perf(consumer): PHP next() uses flume try_recv fast path without block_on"
```

---

## Task 8: Update PHP Delivery class for lock-free ack (Phase 2b — PHP side)

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/delivery.rs:56-61` (ack method)
- Test: `crates/rabbit-rs-php/tests/`

**Interfaces:**
- Consumes: `Delivery::ack()` (core, now returns immediately)
- Produces: `Delivery::ack(): void` (PHP, same signature, returns instantly)

- [ ] **Step 1: Update PHP Delivery::ack to not use block_on**

Since the core `Delivery::ack()` now pushes to a lock-free queue and returns immediately (no async wait), the PHP `ack()` doesn't need `block_on`:

```rust
pub fn ack(&self) -> PhpResult<()> {
    self.ensure_current_process("Goopil\\RabbitRs\\Delivery::ack")?;
    // ack() is now synchronous — pushes to lock-free queue, returns immediately
    self.inner.ack()
        .map_err(|error| consumer_php_exception(&error))
}
```

Wait — `self.inner.ack()` is still `async` in the core crate (because `settle` is async for Release/Reject). We need to handle this. Options:
a) Make `ack()` synchronous in the core (add a separate `ack_sync()` that pushes to the queue)
b) Use `block_on` but it completes immediately (no real overhead since there's no await point)

Option (b) is simpler and the overhead is negligible. Keep the current `block_on` — it will complete in nanoseconds since the ack path no longer awaits anything.

Verify that `block_on(self.inner.ack())` returns immediately by adding a test.

- [ ] **Step 2: Run delivery tests**

Run: `rtk cargo test -p rabbit-rs-core delivery`
Expected: PASS

- [ ] **Step 3: Commit**

No code change needed if block_on completes immediately. Document this:

```bash
git commit --allow-empty -m "docs(delivery): ack() returns immediately via lock-free queue, block_on is a no-op"
```

Or if code changes were made:
```bash
git add crates/rabbit-rs-php/src/classes/delivery.rs
git commit -m "perf(delivery): ack returns immediately via lock-free queue (Phase 2b PHP side)"
```

---

## Task 9: Callback consume API (Phase 2c)

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs` (add `consume()` method)
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` (add `consume()` signature)
- Modify: `crates/rabbit-rs-php/src/classes/mod.rs` (if new module needed)
- Test: `crates/rabbit-rs-php/tests/` (PHPT tests)

**Interfaces:**
- Consumes: `ConsumerHandle::next()` (flume-buffered), `ext_php_rs` callable API
- Produces: `Consumer::consume(callable $handler, int $count = 0, int $timeoutMs = 1000): int`

- [ ] **Step 1: Write the failing PHPT test**

Create a PHPT test that calls `consume()` with a callback:

```php
--TEST--
Consumer::consume() processes messages via callback
--FILE--
<?php
$pool = new Goopil\RabbitRs\Pool($config);
$consumer = $pool->consumer('default');
$count = $consumer->consume(function (Goopil\RabbitRs\Delivery $delivery): void {
    $delivery->ack();
    echo $delivery->payload() . "\n";
}, count: 5, timeoutMs: 5000);
assert($count === 5);
?>
--EXPECT--
msg-0
msg-1
msg-2
msg-3
msg-4
```

- [ ] **Step 2: Run test to verify it fails**

Run: `php -d extension=rabbit_rs.so test.phpt`
Expected: FAIL — `consume()` method doesn't exist

- [ ] **Step 3: Implement consume() in PHP Consumer class**

In `crates/rabbit-rs-php/src/classes/consumer.rs`, add the `consume()` method:

```rust
/// Processes messages by calling the given callback for each delivery.
///
/// Returns the number of messages processed.
#[php(defaults(count = 0, timeoutMs = 1000))]
pub fn consume(
    &self,
    handler: &Zval,
    count: i64,
    timeoutMs: i64,
) -> PhpResult<i64> {
    self.ensure_open("Goopil\\RabbitRs\\Consumer::consume")?;

    let max_count = if count <= 0 { usize::MAX } else { usize::try_from(count).unwrap() };
    let timeout = u64::try_from(timeoutMs).map_err(|_| {
        ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
            "timeoutMs must be a non-negative integer".to_owned(),
        )
    })?;

    let callable = ZendCallable::new(handler)?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout);
    let mut processed: i64 = 0;

    while processed < max_count as i64 {
        if std::time::Instant::now() >= deadline {
            break;
        }

        let delivery = match self.handle.try_next() {
            Some(d) => d,
            None => {
                // Slow path: wait for one delivery
                match self.runtime.block_on(async {
                    time::timeout(
                        deadline.duration_since(std::time::Instant::now()),
                        self.handle.next(),
                    ).await
                }) {
                    Ok(Ok(d)) => d,
                    Ok(Err(_)) => break,
                    Err(_) => break, // timeout
                }
            }
        };

        let php_delivery = Delivery::new(delivery, self.runtime.clone(), self.pid);
        callable.call(&[&php_delivery])?;
        processed += 1;
    }

    Ok(processed)
}
```

Note: Check `ext-php-rs` 0.15.15 API for `ZendCallable` or equivalent. The exact callable invocation API may differ. Look at how `CallbackSlot` in `callbacks.rs` works for reference.

- [ ] **Step 4: Update the stub**

In `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`, add to the `Consumer` class:

```php
/**
 * Processes messages by calling the given callback for each delivery.
 *
 * @param callable(Delivery): void $handler
 * @param int $count Number of messages to process (0 = unlimited)
 * @param int $timeoutMs Total timeout in milliseconds
 * @return int Number of messages processed
 */
public function consume(callable $handler, int $count = 0, int $timeoutMs = 1000): int
{
}
```

- [ ] **Step 5: Run test to verify it passes**

Run the PHPT test.
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/rabbit-rs-php/src/classes/consumer.rs crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
git commit -m "feat(consumer): add callback consume() API for batch FFI crossing (Phase 2c)"
```

---

## Task 10: Iterator API for Consumer (Phase 2d)

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs` (add Iterator support)
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`
- Test: PHPT test

**Interfaces:**
- Consumes: `Consumer::next()`
- Produces: `Consumer` implements `IteratorAggregate` for `foreach` support

- [ ] **Step 1: Implement IteratorAggregate on the PHP Consumer class**

In ext-php-rs, implementing PHP's `IteratorAggregate` requires implementing the `getIterator()` method. Check the ext-php-rs 0.15.15 API for how to implement PHP interfaces on Rust classes.

```rust
// In consumer.rs, add to the #[php_impl] block:

/// Returns an iterator for use in foreach loops.
pub fn getIterator(&self) -> PhpResult<ConsumerIterator> {
    self.ensure_open("Goopil\\RabbitRs\\Consumer::getIterator")?;
    Ok(ConsumerIterator::new(self.handle.clone(), self.runtime.clone(), self.pid))
}
```

Create a `ConsumerIterator` class that implements PHP's `Iterator` interface:

```rust
#[php_class]
#[php(name = "Goopil\\RabbitRs\\ConsumerIterator")]
#[php(implements = "Iterator")]
pub struct ConsumerIterator {
    handle: ConsumerHandle,
    runtime: Handle,
    pid: u32,
    current: Option<Delivery>,
    key: u64,
    default_timeout_ms: u64,
}

#[php_impl]
impl ConsumerIterator {
    pub fn current(&self) -> PhpResult<Option<Delivery>> {
        Ok(self.current.clone())
    }

    pub fn key(&self) -> PhpResult<i64> {
        Ok(self.key as i64)
    }

    pub fn next(&mut self) -> PhpResult<()> {
        self.key += 1;
        // Fetch next delivery with a default timeout
        self.current = self.runtime.block_on(async {
            time::timeout(
                Duration::from_millis(self.default_timeout_ms),
                self.handle.next(),
            ).await.ok().and_then(|r| r.ok())
        });
        Ok(())
    }

    pub fn rewind(&mut self) -> PhpResult<()> {
        self.key = 0;
        self.next()
    }

    pub fn valid(&self) -> PhpResult<bool> {
        Ok(self.current.is_some())
    }
}
```

Note: Check ext-php-rs 0.15.15 for the exact API to implement PHP interfaces. The `#[php(implements = "...")]` attribute may need adjustment. If ext-php-rs doesn't support implementing `Iterator` directly, use `IteratorAggregate` with a native iterator class.

- [ ] **Step 2: Update the stub**

```php
final class Consumer implements \IteratorAggregate
{
    public function next(int $timeoutMs): ?Delivery
    {
    }

    public function consume(callable $handler, int $count = 0, int $timeoutMs = 1000): int
    {
    }

    public function close(): void
    {
    }

    public function getIterator(): \Iterator
    {
    }
}
```

- [ ] **Step 3: Write and run a PHPT test**

```php
--TEST--
Consumer is iterable in foreach
--FILE--
<?php
$pool = new Goopil\RabbitRs\Pool($config);
$consumer = $pool->consumer('default');
$count = 0;
foreach ($consumer as $delivery) {
    $delivery->ack();
    $count++;
    if ($count >= 5) break;
}
assert($count === 5);
?>
--EXPECT--
```

- [ ] **Step 4: Commit**

```bash
git add crates/rabbit-rs-php/src/classes/consumer.rs crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
git commit -m "feat(consumer): add IteratorAggregate support for foreach (Phase 2d)"
```

---

## Task 11: Zero-copy consume payload (Phase 2e)

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/delivery.rs:31-34` (payload method)
- Test: `crates/rabbit-rs-php/tests/`

**Interfaces:**
- Consumes: `Bytes` payload from core `Delivery`
- Produces: `Delivery::payload(): string` with reduced copy

- [ ] **Step 1: Investigate ext-php-rs zero-copy read API**

Check if `ext-php-rs` 0.15.15 provides `Zval::str()` / `ZendStr::as_bytes()` that return `&[u8]` directly on PHP memory. The spec says zero-copy read is possible.

Look at `ext_php_rs::types::ZendStr` for an `as_bytes()` method.

- [ ] **Step 2: Optimize payload() to avoid Bytes::to_vec()**

Currently `delivery.rs:33`:
```rust
Ok(Binary::new(self.inner.payload.to_vec()))
```

If ext-php-rs supports returning a `ZendString` from a `&[u8]` without copying, use it. However, `Bytes` is a Rust-owned buffer, not PHP memory — so zero-copy in the traditional sense (pointing at PHP's string memory) isn't applicable here. The payload comes from Lapin (Rust memory), not PHP.

The optimization is: instead of `to_vec()` (allocates + copies), use `Binary::new(self.inner.payload.as_ref().to_vec())` which is the same, or better, use `Bytes::copy_from_slice` which is also the same.

Actually, the current `to_vec()` is already optimal for Rust→PHP transfer: `Bytes` is a refcounted Rust buffer, and `Binary::new(vec)` creates a PHP string. There's no way to avoid this copy because PHP strings use inline storage (`char val[1]`).

If ext-php-rs has a way to create a `ZendString` from a `&[u8]` without going through `Vec`, use that:

```rust
pub fn payload(&self) -> PhpResult<Binary<u8>> {
    self.ensure_current_process("Goopil\\RabbitRs\\Delivery::payload")?;
    // If Bytes is backed by a contiguous slice, avoid the Vec allocation:
    // Check if ext-php-rs has ZendString::new(bytes) that copies directly
    // For now, to_vec() is the only option since PHP strings own their memory
    Ok(Binary::new(self.inner.payload.to_vec()))
}
```

- [ ] **Step 3: Document that zero-copy consume is limited**

If the investigation confirms that zero-copy from Rust `Bytes` to PHP `string` is impossible (due to PHP's inline string storage), document this and skip the optimization. The spec already acknowledges this: "Zero-copy write (publish side) is impossible — `zend_string` uses inline storage."

The gain is 5-10% on consume. If we can't achieve it, note it in the plan.

- [ ] **Step 4: Commit (or document skip)**

```bash
git commit --allow-empty -m "docs(consumer): zero-copy payload not feasible due to PHP string inline storage (Phase 2e skipped)"
```

Or if an optimization was found:
```bash
git add crates/rabbit-rs-php/src/classes/delivery.rs
git commit -m "perf(delivery): reduce payload copy overhead (Phase 2e)"
```

---

## Task 12: Batch AMQP frames for publish (Phase 3a)

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport.rs:403-416` (add `publish_batch` to PublisherChannel trait)
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs:144-164` (implement `publish_batch`)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:519-588` (use `publish_batch`)
- Test: `crates/rabbit-rs-core/tests/publisher_safety.rs`

**Interfaces:**
- Consumes: `PublishRequest` (transport version)
- Produces: `PublisherChannel::publish_batch(Vec<PublishRequest>) -> TransportResult<Vec<Box<dyn PublishReceipt>>>`

- [ ] **Step 1: Add publish_batch to the PublisherChannel trait**

In `crates/rabbit-rs-core/src/transport.rs`, add a default method to the `PublisherChannel` trait (lines 403-416):

```rust
#[async_trait]
pub trait PublisherChannel: TopologyChannel {
    async fn enable_confirms(&self) -> TransportResult<()>;

    async fn publish(&self, request: PublishRequest) -> TransportResult<Box<dyn PublishReceipt>>;

    /// Sends a batch of publishes without awaiting between each, then returns
    /// all receipts. The default implementation calls publish() sequentially;
    /// the Lapin implementation overrides this to pipeline frames.
    async fn publish_batch(
        &self,
        requests: Vec<PublishRequest>,
    ) -> TransportResult<Vec<Box<dyn PublishReceipt>>> {
        let mut receipts = Vec::with_capacity(requests.len());
        for request in requests {
            receipts.push(self.publish(request).await?);
        }
        Ok(receipts)
    }
}
```

- [ ] **Step 2: Implement publish_batch in LapinPublisherChannel**

In `crates/rabbit-rs-core/src/transport/lapin.rs`, override `publish_batch`:

```rust
async fn publish_batch(
    &self,
    requests: Vec<PublishRequest>,
) -> TransportResult<Vec<Box<dyn PublishReceipt>>> {
    let mut receipts = Vec::with_capacity(requests.len());
    for request in requests {
        let properties = publish_properties(&request);
        // Send the frame without awaiting (Lapin's basic_publish returns a Future
        // that is Ready immediately when the socket buffer has space)
        let confirmation = self
            .inner
            .basic_publish(
                request.exchange.into(),
                request.routing_key.into(),
                BasicPublishOptions {
                    mandatory: request.mandatory,
                },
                &request.payload,
                properties,
            )
            .await?;
        receipts.push(Box::new(LapinPublishReceipt::new(confirmation)) as Box<dyn PublishReceipt>);
    }
    Ok(receipts)
}
```

Note: To truly batch frames without awaiting between each, we need to poll all futures simultaneously or use Lapin's internal API. The simplest approach is to collect all `basic_publish` futures and join them with `futures::future::join_all`. Check if Lapin's `basic_publish` returns a Future that is immediately Ready (writes to socket buffer):

```rust
// Alternative: use FuturesUnordered to poll all publishes concurrently
let mut futures = FuturesUnordered::new();
for request in requests {
    let properties = publish_properties(&request);
    let confirmation = self.inner.basic_publish(
        request.exchange.into(),
        request.routing_key.into(),
        BasicPublishOptions { mandatory: request.mandatory },
        &request.payload,
        properties,
    );
    futures.push(confirmation);
}
let mut receipts = Vec::with_capacity(requests.len());
while let Some(result) = futures.next().await {
    let confirmation = result?;
    receipts.push(Box::new(LapinPublishReceipt::new(confirmation)) as Box<dyn PublishReceipt>);
}
Ok(receipts)
```

- [ ] **Step 3: Use publish_batch in the actor**

In `crates/rabbit-rs-core/src/publisher/actor.rs`, modify `publish_queue` (lines 519-588) to collect all requests and call `publish_batch`:

```rust
async fn publish_queue(state: &mut ActorState, pending: VecDeque<RetainedPublish>) {
    if pending.is_empty() {
        return;
    }

    // Collect all requests
    let mut requests = Vec::with_capacity(pending.len());
    let mut retained_map: VecDeque<RetainedPublish> = pending;
    for retained in &retained_map {
        let request = into_transport_request(&retained.request, ...);
        requests.push(request);
    }

    // Send all frames in one batch
    match channel.publish_batch(requests).await {
        Ok(receipts) => {
            for (i, receipt) in receipts.into_iter().enumerate() {
                if let Some(retained) = retained_map.pop_front() {
                    state.ledger.insert(state.sequence, InFlightPublish { retained, generation });
                    state.confirmations.push(Box::pin(async move {
                        ...time::timeout_at(deadline, receipt.wait()).await
                    }));
                    state.sequence += 1;
                }
            }
        }
        Err(error) => { ... }
    }
}
```

- [ ] **Step 4: Write and run the test**

Test that batch publish sends all frames and returns correct confirmations.

Run: `rtk cargo test -p rabbit-rs-core --test publisher_safety`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core/src/transport.rs crates/rabbit-rs-core/src/transport/lapin.rs crates/rabbit-rs-core/src/publisher/actor.rs
git commit -m "perf(publisher): batch AMQP frames to reduce per-message async overhead (Phase 3a)"
```

---

## Task 13: Add arc-swap dependency and implement hot path bypass for publish (Phase 3b)

**Files:**
- Modify: `crates/rabbit-rs-core/Cargo.toml` (add `arc-swap`)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs` (expose Arc<Channel>)
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs` (add hot path API)
- Modify: `crates/rabbit-rs-core/src/client.rs` (wire hot path)
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs` (use hot path for publish)
- Test: `crates/rabbit-rs-core/tests/publisher_safety.rs`

**Interfaces:**
- Consumes: `PublisherChannel` trait, `futures-lite::FutureExt::now_or_never`
- Produces: `PublisherHandle::try_publish_hot()` that bypasses the actor mpsc

- [ ] **Step 1: Add arc-swap to Cargo.toml**

```toml
arc-swap = "1"
```

Also add `futures-lite` if not already present (check — it is already a dependency per the audit).

- [ ] **Step 2: Write the failing test**

```rust
#[tokio::test]
async fn hot_path_bypass_publishes_directly_to_channel() {
    let (channel, mock) = MockPublisherChannel::pair();
    let handle = PublisherActor::spawn(Arc::new(channel), test_config());

    // The hot path should publish directly to the channel via now_or_never
    let request = test_publish_request(0);
    let waiter = handle.try_publish_hot(request).unwrap();

    // No 1ms timer, no mpsc crossing — publish happens immediately
    let outcome = waiter.wait().await.unwrap();
    assert!(matches!(outcome, PublishOutcome::Confirmed { .. }));
    assert_eq!(mock.publish_count(), 1);
}
```

- [ ] **Step 3: Expose Arc<dyn PublisherChannel> via ArcSwap**

In `crates/rabbit-rs-core/src/publisher/actor.rs`, add an `ArcSwap` to `PublisherHandle`:

```rust
use arc_swap::ArcSwapOption;

#[derive(Clone, Debug)]
pub struct PublisherHandle {
    commands: mpsc::Sender<Command>,
    capacity: Arc<Semaphore>,
    metrics: Metrics,
    confirm_timeout: Duration,
    hot_channel: Arc<ArcSwapOption<Arc<dyn PublisherChannel>>>,  // new
    generation: Arc<AtomicU64>,  // track connection generation
}
```

When the actor receives a `ConnectionEvent::Ready`, it stores the channel in the `ArcSwapOption`. When it receives `Recovering`, it stores `None` (invalidating the hot path).

- [ ] **Step 4: Add try_publish_hot to PublisherHandle**

```rust
impl PublisherHandle {
    /// Hot path: publish directly to the channel via now_or_never, bypassing the actor.
    /// Falls back to the actor (cold path) if the channel is unavailable or the
    /// socket buffer is full.
    pub fn try_publish_hot(&self, request: PublishRequest) -> Result<PublishWaiter, PublishError> {
        // Check if hot channel is available
        if let Some(channel) = self.hot_channel.load_full() {
            // Try now_or_never — polls once, returns if Ready
            let transport_request = into_transport_request(&request, ...);
            match channel.publish(transport_request).now_or_never() {
                Some(Ok(receipt)) => {
                    self.metrics.record_publish();
                    // Return a PublishWaiter that wraps the receipt
                    return Ok(PublishWaiter::from_receipt(receipt));
                }
                Some(Err(_)) => {
                    // Channel error — fall through to cold path
                }
                None => {
                    // Not ready (socket buffer full) — fall through to cold path
                    // Use block_on for the slow path
                }
            }
        }

        // Cold path: use the actor
        self.try_publish(request)
    }
}
```

Note: `now_or_never()` requires `futures_lite::FutureExt`. The `PublishWaiter` needs to be constructible from a `PublishReceipt` (for the hot path) in addition to a `oneshot::Receiver` (for the cold path).

- [ ] **Step 5: Handle channel invalidation on recovery**

When the connection drops, the actor sets `hot_channel.store(None)`. The next `try_publish_hot` falls back to the actor. After recovery, the actor sets `hot_channel.store(Some(new_channel))`.

- [ ] **Step 6: Update ClientPool to expose the hot path**

In `crates/rabbit-rs-core/src/client.rs`, update `publish()` (lines 109-123) to use `try_publish_hot`:

```rust
pub async fn publish(
    &self,
    broker: &str,
    request: PublishRequest,
) -> Result<PublishOutcome, ClientError> {
    self.ensure_open()?;
    let publisher = self.publisher(broker).await?;
    let waiter = publisher
        .try_publish_hot(request)  // hot path first
        .or_else(|_| publisher.try_publish(request))  // cold path fallback
        .map_err(|error| ClientError::publish(&error))?;
    waiter
        .wait()
        .await
        .map_err(|error| ClientError::publish(&error))
}
```

- [ ] **Step 7: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core hot_path_bypass`
Expected: PASS

- [ ] **Step 8: Run full publisher safety tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher_safety`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add crates/rabbit-rs-core/Cargo.toml crates/rabbit-rs-core/src/publisher/actor.rs crates/rabbit-rs-core/src/client.rs crates/rabbit-rs-core/tests/publisher_safety.rs
git commit -m "feat(publisher): hot path bypass with now_or_never and ArcSwap channel (Phase 3b)"
```

---

## Task 14: Parallel confirms with ConfirmTracker (Phase 3c)

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/confirms.rs` (add ConfirmTracker)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs` (use ConfirmTracker)
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs` (add wait_for_confirms)
- Modify: `crates/rabbit-rs-core/src/client.rs` (expose wait_for_confirms)
- Test: `crates/rabbit-rs-core/tests/publisher_safety.rs`

**Interfaces:**
- Consumes: `PublisherChannel::publish()` receipts, `FuturesUnordered`
- Produces: `ConfirmTracker` with atomic `published` and `confirmed` counters, `wait_for_confirms()` method

- [ ] **Step 1: Verify cherry-pick `95142ea` and `6cb343f` are present**

Confirm `ConfirmLedger` uses `HashMap` instead of `BTreeMap`, and `drain()` sorts entries for deterministic order.

- [ ] **Step 2: Write the failing test**

```rust
#[tokio::test(start_paused = true)]
async fn parallel_confirms_wait_for_watermark() {
    let (channel, mock) = MockPublisherChannel::pair();
    let handle = PublisherActor::spawn(Arc::new(channel), test_config_with_confirms());

    // Publish 100 messages
    let mut waiters = Vec::new();
    for i in 0..100 {
        waiters.push(handle.try_publish_hot(test_publish_request(i)).unwrap());
    }

    // wait_for_confirms should block until all 100 are confirmed
    // Advance time and let the mock confirm all
    tokio::time::advance(Duration::from_millis(10)).await;
    mock.confirm_all();

    handle.wait_for_confirms(Duration::from_secs(1)).await.unwrap();
    assert_eq!(handle.metrics_snapshot().confirmations_total, 100);
}
```

- [ ] **Step 3: Add ConfirmTracker with atomic counters**

In `crates/rabbit-rs-core/src/publisher/confirms.rs`:

```rust
use std::sync::atomic::{AtomicU64, Ordering};

pub struct ConfirmTracker {
    published: AtomicU64,
    confirmed: AtomicU64,
    returned: AtomicU64,
}

impl ConfirmTracker {
    pub fn new() -> Self {
        Self {
            published: AtomicU64::new(0),
            confirmed: AtomicU64::new(0),
            returned: AtomicU64::new(0),
        }
    }

    pub fn record_publish(&self) {
        self.published.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_confirmation(&self) {
        self.confirmed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_return(&self) {
        self.returned.fetch_add(1, Ordering::Relaxed);
    }

    pub fn watermark(&self) -> u64 {
        self.published.load(Ordering::Relaxed)
    }

    pub fn confirmed_count(&self) -> u64 {
        self.confirmed.load(Ordering::Relaxed)
    }
}
```

- [ ] **Step 4: Add wait_for_confirms to PublisherHandle**

```rust
impl PublisherHandle {
    /// Waits until all published messages are confirmed by the broker.
    pub async fn wait_for_confirms(&self, timeout: Duration) -> Result<(), PublishError> {
        let watermark = self.confirm_tracker.watermark();
        let deadline = Instant::now() + timeout;
        loop {
            if self.confirm_tracker.confirmed_count() >= watermark {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(PublishError::new(PublishErrorKind::Timeout, "confirm timeout"));
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
}
```

- [ ] **Step 5: Wire the ConfirmTracker into the actor's confirm handling**

When a confirmation receipt resolves (Ack/Nack/Return), the actor calls `confirm_tracker.record_confirmation()` or `confirm_tracker.record_return()`. The tracker replaces the sequential per-waiter confirm resolution with a counter-based approach.

- [ ] **Step 6: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core parallel_confirms`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/confirms.rs crates/rabbit-rs-core/src/publisher/actor.rs crates/rabbit-rs-core/src/publisher/mod.rs crates/rabbit-rs-core/src/client.rs
git commit -m "feat(publisher): parallel confirms with ConfirmTracker and wait_for_confirms (Phase 3c)"
```

---

## Task 15: Add SafetyMode config and wire safety modes (Phase 3b/3c/4a config)

**Files:**
- Modify: `crates/rabbit-rs-core/src/config.rs:393-423` (PublisherConfigSection)
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs:103-154` (PublisherConfig)
- Modify: `crates/rabbit-rs-php/src/conversion.rs` (validate safety config)
- Test: `crates/rabbit-rs-core/tests/config*.rs`

**Interfaces:**
- Consumes: `PublisherConfigSection` with `confirms: bool` and `mandatory: bool`
- Produces: `PublisherConfigSection` with `safety: SafetyMode` enum (backward-compatible with `confirms`/`mandatory`)

- [ ] **Step 1: Add SafetyMode enum**

In `crates/rabbit-rs-core/src/config.rs`:

```rust
/// Publisher safety mode determining the delivery guarantee level.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SafetyMode {
    /// Fire-and-forget: async pump, no socket wait, no confirms. Messages
    /// may be lost if the socket drops between pump send and TCP write.
    Blind,
    /// Synchronous socket write, no confirms. Message reached kernel socket buffer.
    Unsafe,
    /// Confirm mode + mandatory routing. At-least-once delivery guarantee.
    Safe,
}

impl Default for SafetyMode {
    fn default() -> Self {
        Self::Safe
    }
}
```

- [ ] **Step 2: Add safety field to PublisherConfigSection**

```rust
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct PublisherConfigSection {
    pub safety: SafetyMode,
    /// Deprecated: use `safety = "safe"` instead. Defaults to true for backward compat.
    pub confirms: bool,
    /// Deprecated: use `safety = "safe"` instead. Defaults to true for backward compat.
    pub mandatory: bool,
    #[serde(deserialize_with = "deserialize_duration_millis")]
    pub confirm_timeout: Duration,
}
```

Update `Default` to set `safety: SafetyMode::Safe` and keep `confirms: true, mandatory: true` for backward compatibility.

- [ ] **Step 3: Implement backward-compatible deserialization**

If `safety` is not set, derive it from `confirms`/`mandatory`:
- `confirms=true, mandatory=true` → `Safe`
- `confirms=false, mandatory=false` → `Unsafe`
- `confirms=false, mandatory=true` → `Unsafe` (no confirms = not safe)
- `confirms=true, mandatory=false` → `Safe` (confirms without mandatory is still "safe enough")

```rust
impl PublisherConfigSection {
    pub fn effective_safety(&self) -> SafetyMode {
        if self.safety != SafetyMode::Safe {
            return self.safety;
        }
        // Derive from legacy flags if safety was default
        if !self.confirms {
            SafetyMode::Unsafe
        } else {
            SafetyMode::Safe
        }
    }
}
```

- [ ] **Step 4: Update PublisherConfig in the publisher module**

In `crates/rabbit-rs-core/src/publisher/mod.rs`, update `PublisherConfig` (lines 103-154) to carry `SafetyMode`:

```rust
pub struct PublisherConfig {
    pub max_messages: usize,
    pub max_bytes: usize,
    pub flush_interval: Duration,
    pub buffer_capacity: usize,
    pub confirm_timeout: Duration,
    pub safety: SafetyMode,  // replaces confirms + mandatory booleans
}
```

- [ ] **Step 5: Update client.rs publisher_config to map safety**

In `crates/rabbit-rs-core/src/client.rs`, update `publisher_config` (lines 619-630):

```rust
fn publisher_config(config: &ValidatedConfig) -> PublisherConfig {
    let publisher = config.publisher();
    let safety = publisher.effective_safety();
    PublisherConfig::with_safety(
        DEFAULT_MAX_MESSAGES,
        DEFAULT_MAX_BYTES,
        Duration::from_millis(1),
        DEFAULT_BUFFER_CAPACITY,
        publisher.confirm_timeout,
        safety,
    )
}
```

- [ ] **Step 6: Write and run config tests**

```bash
rtk cargo test -p rabbit-rs-core config::tests
```
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/rabbit-rs-core/src/config.rs crates/rabbit-rs-core/src/publisher/mod.rs crates/rabbit-rs-core/src/client.rs
git commit -m "feat(config): add SafetyMode enum with backward-compatible config (Phase 3b/4a config)"
```

---

## Task 16: PHP-side publish buffer with auto-flush (Phase 3d)

**Files:**
- Modify: `crates/rabbit-rs-php/Cargo.toml` (add `flume` if needed for PHP-side buffer)
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs` (add buffer + auto-flush)
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` (add `flush()`)
- Test: `crates/rabbit-rs-php/tests/`

**Interfaces:**
- Consumes: `ClientPool::publish()` and `ClientPool::publish_batch()`
- Produces: `Pool::publish()` buffers internally, `Pool::flush()` drains the buffer

- [ ] **Step 1: Write the failing test**

Create a PHPT test:

```php
--TEST--
Pool::publish() buffers and auto-flushes at threshold
--FILE--
<?php
$pool = new Goopil\RabbitRs\Pool($config);

// Publish 63 messages — should be buffered, not sent yet
$ids = [];
for ($i = 0; $i < 63; $i++) {
    $ids[] = $pool->publish([
        'broker' => 'default',
        'exchange' => 'test',
        'routing_key' => 'test',
        'payload' => "msg-$i",
        'message_id' => "id-$i",
    ]);
}

// The 64th message triggers auto-flush
$ids[] = $pool->publish([
    'broker' => 'default',
    'exchange' => 'test',
    'routing_key' => 'test',
    'payload' => 'msg-63',
    'message_id' => 'id-63',
]);

// All 64 should be sent
$pool->flush();
assert(count($ids) === 64);
?>
--EXPECT--
```

- [ ] **Step 2: Add publish buffer to the PHP Pool class**

In `crates/rabbit-rs-php/src/classes/pool.rs`, add a buffer to the `Pool` struct:

```rust
pub struct Pool {
    handle: Arc<ConnectionHandle>,
    client: Arc<ClientPool>,
    pid: u32,
    connection_state_callback: CallbackSlot,
    backpressure_callback: CallbackSlot,
    last_connection_states: std::sync::Mutex<HashMap<String, (String, i64)>>,
    last_backpressure_total: std::sync::Mutex<u64>,
    // New: publish buffer
    publish_buffer: std::sync::Mutex<Vec<NativePublish>>,
    buffer_threshold: usize,  // default: 64
    last_flush: std::sync::Mutex<std::time::Instant>,
    flush_interval: std::time::Duration,  // default: 1ms
}
```

- [ ] **Step 3: Modify Pool::publish to buffer**

```rust
pub fn publish(&self, message: &ZendHashTable) -> PhpResult<String> {
    self.ensure_open("Goopil\\RabbitRs\\Pool::publish")?;

    let publish = conversion::publish(message, "message").map_err(|message| {
        ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(message)
    })?;

    let message_id = publish.request.properties.message_id.clone();

    // Buffer the publish
    let mut buffer = self.publish_buffer.lock().unwrap();
    buffer.push(publish);

    // Check auto-flush triggers
    let should_flush = buffer.len() >= self.buffer_threshold
        || self.last_flush.lock().unwrap().elapsed() >= self.flush_interval;

    if should_flush {
        let publishes = std::mem::take(&mut *buffer);
        drop(buffer);
        *self.last_flush.lock().unwrap() = std::time::Instant::now();
        self.flush_publishes(publishes)?;
    }

    Ok(message_id)
}
```

- [ ] **Step 4: Add flush() method**

```rust
/// Flushes the publish buffer, sending all buffered messages to the broker.
pub fn flush(&self) -> PhpResult<()> {
    self.ensure_open("Goopil\\RabbitRs\\Pool::flush")?;
    let publishes = std::mem::take(&mut *self.publish_buffer.lock().unwrap());
    if !publishes.is_empty() {
        self.flush_publishes(publishes)?;
    }
    Ok(())
}

fn flush_publishes(&self, publishes: Vec<NativePublish>) -> PhpResult<()> {
    let requests: Vec<_> = publishes.into_iter()
        .map(|p| (p.broker, p.request))
        .collect();
    match self.handle.runtime().block_on(self.client.publish_batch(requests)) {
        Ok(_outcomes) => Ok(()),
        Err(error) => client_exception(&error),
    }
}
```

- [ ] **Step 5: Update publish_batch to bypass the buffer**

```rust
pub fn publish_batch(&self, messages: &ZendHashTable) -> PhpResult<Vec<String>> {
    self.ensure_open("Goopil\\RabbitRs\\Pool::publishBatch")?;
    // Flush any buffered messages first
    self.flush()?;
    // ... existing publish_batch logic ...
}
```

- [ ] **Step 6: Update __destruct and close to flush**

```rust
pub fn close(&self) -> PhpResult<()> {
    let _ = self.flush();
    // ... existing close logic ...
}
```

- [ ] **Step 7: Update the stub**

Add `flush()` to the Pool stub:

```php
/**
 * Flushes the publish buffer, sending all buffered messages to the broker.
 */
public function flush(): void
{
}
```

- [ ] **Step 8: Run tests**

Run the PHPT test.
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add crates/rabbit-rs-php/src/classes/pool.rs crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
git commit -m "feat(pool): PHP-side publish buffer with auto-flush at 64 messages or 1ms (Phase 3d)"
```

---

## Task 17: Async pump for blind mode (Phase 4a)

**Files:**
- Create: `crates/rabbit-rs-core/src/publisher/pump.rs`
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs` (re-export pump)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs` (wire blind mode)
- Modify: `crates/rabbit-rs-core/src/client.rs` (expose blind publish)
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs` (use blind mode from config)
- Test: `crates/rabbit-rs-core/tests/publisher_safety.rs`

**Interfaces:**
- Consumes: `PublishRequest`, `PublisherChannel`, `flume::Sender`
- Produces: `PublishPump` that drains a `flume` channel and publishes in the background

- [ ] **Step 1: Create the pump module**

Create `crates/rabbit-rs-core/src/publisher/pump.rs`:

```rust
use flume::{Sender, Receiver};
use crate::publisher::PublishRequest;
use crate::transport::{PublisherChannel, TransportResult};
use std::sync::Arc;

/// A background pump that drains a flume channel and publishes messages.
pub struct PublishPump {
    tx: Sender<PumpJob>,
}

struct PumpJob {
    request: PublishRequest,
}

impl PublishPump {
    /// Spawns a background pump task that drains the channel and publishes.
    #[must_use]
    pub fn spawn(
        channel: Arc<dyn PublisherChannel>,
        buffer_capacity: usize,
    ) -> Self {
        let (tx, rx) = flume::bounded(buffer_capacity);
        tokio::spawn(async move {
            pump_loop(channel, rx).await;
        });
        Self { tx }
    }

    /// Enqueues a publish job. Returns immediately (no block_on).
    pub fn try_publish(&self, request: PublishRequest) -> Result<(), PublishRequest> {
        match self.tx.try_send(PumpJob { request }) {
            Ok(()) => Ok(()),
            Err(flume::TrySendError::Full(job)) => Err(job.request),
            Err(flume::TrySendError::Disconnected(job)) => Err(job.request),
        }
    }

    /// Returns the sender for the pump channel.
    #[must_use]
    pub fn sender(&self) -> &Sender<PumpJob> {
        &self.tx
    }
}

async fn pump_loop(channel: Arc<dyn PublisherChannel>, rx: Receiver<PumpJob>) {
    while let Ok(job) = rx.recv_async().await {
        let _ = channel.publish(job.request).await;
    }
}
```

- [ ] **Step 2: Wire blind mode into the publisher actor**

When `SafetyMode::Blind` is configured, the `PublisherActor` creates a `PublishPump` instead of using the mpsc+batcher path. `try_publish` pushes to the pump's flume channel.

- [ ] **Step 3: Wire blind mode into ClientPool**

In `crates/rabbit-rs-core/src/client.rs`, when `safety == Blind`, `publish()` pushes to the pump and returns the `message_id` immediately without waiting for confirmation.

- [ ] **Step 4: Wire blind mode into PHP Pool**

In `crates/rabbit-rs-php/src/classes/pool.rs`, when the pool config has `safety = 'blind'`, `publish()` pushes to the PHP-side buffer, which flushes to the pump. The pump publishes in the background. `publish()` returns the `message_id` immediately.

- [ ] **Step 5: Write and run the test**

```rust
#[tokio::test(start_paused = true)]
async fn blind_mode_publish_returns_immediately() {
    let (channel, mock) = MockPublisherChannel::pair();
    let handle = PublisherActor::spawn_with_safety(
        Arc::new(channel),
        test_config(),
        SafetyMode::Blind,
    );

    let request = test_publish_request(0);
    let result = handle.try_publish(request).unwrap();

    // In blind mode, try_publish returns immediately
    // The pump publishes in the background
    tokio::time::advance(Duration::from_millis(10)).await;
    assert_eq!(mock.publish_count(), 1);
}
```

- [ ] **Step 6: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/pump.rs crates/rabbit-rs-core/src/publisher/mod.rs crates/rabbit-rs-core/src/publisher/actor.rs crates/rabbit-rs-core/src/client.rs crates/rabbit-rs-php/src/classes/pool.rs
git commit -m "feat(publisher): async pump for blind fire-and-forget mode (Phase 4a)"
```

---

## Task 18: Stats collection under hot path bypass

**Files:**
- Modify: `crates/rabbit-rs-core/src/metrics.rs` (verify atomics are incremented by hot path)
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs` (fix stats() stub docblock)
- Test: `crates/rabbit-rs-core/tests/metrics_snapshot.rs`

**Interfaces:**
- Consumes: `Metrics` (atomic counters + histograms)
- Produces: `stats()` returns the same 17 keys the Laravel status command expects

- [ ] **Step 1: Verify hot path increments atomics**

Check that:
- `try_publish_hot()` calls `metrics.record_publish()` after `basic_publish` completes
- The confirms driver calls `metrics.record_confirmation()` on Ack receipt
- The background pump calls `metrics.record_delivery()` on Lapin delivery
- `next()` or ack batch drain calls `metrics.record_ack()`

All of these should already be in place from previous tasks. Verify.

- [ ] **Step 2: Fix the stats() stub docblock**

In `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`, update the `@return` on `stats()` (line 51) to include all 17 keys:

```php
/**
 * @return array{
 *   closed: bool,
 *   pid: int,
 *   handle: string,
 *   publishes_total: int,
 *   confirmations_total: int,
 *   returns_total: int,
 *   backpressure_total: int,
 *   reconnects_total: int,
 *   deliveries_total: int,
 *   acks_total: int,
 *   rejects_total: int,
 *   confirmation_latency_p50: int,
 *   confirmation_latency_p95: int,
 *   confirmation_latency_p99: int,
 *   settlement_latency_p50: int,
 *   settlement_latency_p95: int,
 *   settlement_latency_p99: int
 * }
 */
public function stats(): array
{
}
```

- [ ] **Step 3: Run metrics tests**

```bash
rtk cargo test -p rabbit-rs-core --test metrics_snapshot
```
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
git commit -m "fix(stubs): update stats() docblock to reflect all 17 returned keys"
```

---

## Task 19: Full quality gate and final verification

**Files:**
- All modified files

- [ ] **Step 1: Run fmt**

```bash
rtk cargo fmt --all
```

- [ ] **Step 2: Run clippy**

```bash
rtk cargo clippy --workspace --all-targets --all-features -- -D warnings
```
Expected: PASS — fix any warnings

- [ ] **Step 3: Run all tests**

```bash
rtk cargo test --workspace --all-targets
```
Expected: PASS

- [ ] **Step 4: Validate composer**

```bash
rtk composer validate --strict
```
Expected: PASS

- [ ] **Step 5: Run the full quality gate**

```bash
rtk ./scripts/check.sh
```
Expected: PASS

- [ ] **Step 6: Run PHP stub validation**

```bash
php -l crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
```
Expected: No syntax errors

- [ ] **Step 7: Update the implementation plan document**

Update `docs/plans/2026-07-30-rabbitmq-native-implementation.md` to reflect the completed milestone status.

- [ ] **Step 8: Final commit**

```bash
git add -A
git commit -m "test: full quality gate passes after performance optimization"
```

---

## Spec Coverage Checklist

| Spec Phase | Task(s) | Status |
|-----------|---------|--------|
| 1a: Immediate flush | Task 2 | ✅ |
| 1b: Reduce FFI conversion | Task 3 (+ cherry-picks in Task 0) | ✅ |
| 1c: Reduce clones | Task 4 (+ cherry-picks in Task 0) | ✅ |
| 1d: Lapin/Tokio tuning | Task 1 | ✅ |
| 2a: Buffered consumer | Tasks 5, 7 | ✅ |
| 2b: Batched ack + multiple | Task 6, 8 | ✅ |
| 2c: Callback consume | Task 9 | ✅ |
| 2d: Iterator API | Task 10 | ✅ |
| 2e: Zero-copy consume | Task 11 (may skip — PHP inline storage) | ⚠️ |
| 3a: Batch AMQP frames | Task 12 | ✅ |
| 3b: Hot path bypass | Task 13 | ✅ |
| 3c: Parallel confirms | Task 14 (+ cherry-picks in Task 0) | ✅ |
| 3d: PHP publish buffer | Task 16 | ✅ |
| 4a: Blind pump | Task 17 | ✅ |
| Safety modes config | Task 15 | ✅ |
| Stats under bypass | Task 18 | ✅ |
| Full quality gate | Task 19 | ✅ |

## Reliability Invariants Checklist

| Invariant | How preserved |
|-----------|---------------|
| Unconfirmed publications survive recovery in bounded memory | `ConfirmLedger` drain on suspend → `replay` buffer (bounded by `max_messages`) |
| Replay with same message_id and original deadline | `RetainedPublish` preserves `request.deadline` and `request.properties.message_id` |
| Publisher confirms resolve each waiter once | `PublishWaiter` wraps oneshot; `resolve_confirmation` sends once |
| Mandatory return takes precedence over ACK | `resolve_confirmation` checks Return before Ack |
| Delivery tokens are generation-aware | `DeliveryTokenInner.generation` checked in `settle()`; stale → `DeliveryState::Lost` |
| Stale ACKs rejected | `settle()` returns `StaleGeneration` error on generation mismatch |
| Recovery order deterministic | `recover_generation` unchanged: conn → channels → topology → QoS → consumers |
| Bounded queues/channels/buffers | flume bounded, crossbeam SegQueue bounded, replay bounded by max_messages |
| No unsafe Rust | All new code uses safe Rust only; `#![forbid(unsafe_code)]` preserved |
| No credentials in Debug/errors | Existing `secrecy` crate usage preserved; no new Debug impls expose secrets |
| No Zend values retained in Rust threads | Callback consume invokes PHP callable from PHP thread (block_on context), not from background tasks |

## Estimated Timeline

| Tasks | Effort | Description |
|-------|--------|-------------|
| Task 0 | 0.5 day | Cherry-pick PR #4 |
| Task 1 | 0.5 day | Lapin/Tokio tuning |
| Task 2 | 1 day | Immediate flush |
| Task 3 | 0.5 day | FFI conversion (mostly cherry-picked) |
| Task 4 | 1 day | Reduce clones |
| Tasks 5, 7, 6, 8 | 3 days | Buffered consumer + batched ack |
| Task 9 | 2 days | Callback consume |
| Task 10 | 0.5 day | Iterator API |
| Task 11 | 0.5 day | Zero-copy (investigation + possible skip) |
| Task 12 | 1 day | Batch AMQP frames |
| Task 13 | 3 days | Hot path bypass |
| Task 14 | 2 days | Parallel confirms |
| Task 15 | 1 day | Safety modes config |
| Task 16 | 2 days | PHP publish buffer |
| Task 17 | 2 days | Blind pump |
| Task 18 | 0.5 day | Stats verification |
| Task 19 | 0.5 day | Full quality gate |
| **Total** | **~21 days** | |
