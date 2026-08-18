# Consumer Buffer & Performance Optimization Plan

**Date:** August 18, 2026
**Status:** Draft (iterating)

---

## Problem

The native rabbit-rs PHP extension is 4x slower than php-amqplib (pure PHP):

- rabbit-rs publish: ~9K msgs/s (safe mode)
- php-amqplib publish: ~39K msgs/s (safe mode)
- rabbit-rs consume: ~7K msgs/s
- php-amqplib consume: ~17K msgs/s

Root cause: every publish and consume traverses 7-13 layers of abstraction (FFI, conversion, `block_on`, mpsc, actor, batcher, 1ms timer, Lapin, socket). The actor pattern manages the cold path well but should not be in the hot path.

The old implementation (`Goopil/php-ext-rabbit-rs`, archived) achieved ~30-40K publish / ~15-20K consume by keeping the hot path to 3 layers (PHP -> `AmqpChannel` -> Lapin -> socket). The `ext-amqp` pecl extension (C, based on `librabbitmq`) goes further with 1 layer (PHP -> `amqp_basic_publish` -> socket).

---

## Goal

Make rabbit-rs at least as fast as php-amqplib in unsafe mode, faster in safe mode, and competitive with `ext-amqp` in blind mode — **without breaking the Laravel API**.

Target:
- Publish blind: 80-100K msgs/s
- Publish unsafe: 50-80K msgs/s
- Publish safe: 35-45K msgs/s
- Consume (poll): 30-50K msgs/s
- Consume (callback): 80-100K msgs/s

---

## Architecture Principle

**Actor for cold path, bypass for hot path.**

The actor keeps its role for: channel creation, recovery, topology reconciliation, QoS, consumer registration, stats collection. Once the channel is created, the hot path (publish/consume of each message) bypasses the actor and uses direct channel access or background buffers.

```
┌─────────────────────────────────────────────────────────┐
│  COLD PATH (actor)                                      │
│  • Channel creation & ownership                         │
│  • Recovery / reconnection                              │
│  • Topology reconciliation                              │
│  • QoS / prefetch                                       │
│  • Consumer registration                                │
│  • Stats collection (reads atomics)                     │
└──────────────────────┬──────────────────────────────────┘
                       │  Exposes Arc<Channel> / flume buffer
                       ▼
┌─────────────────────────────────────────────────────────┐
│  HOT PATH (bypass)                                      │
│                                                         │
│  Publish:  PHP → FFI → block_on(channel.basic_publish) │
│  Consume:  Background pump → flume buffer → PHP next() │
│  Ack:      PHP → lock-free queue → background drain     │
└─────────────────────────────────────────────────────────┘
```

---

## Phase 1: Quick Wins (no architecture change)

### 1a. Immediate flush for explicit batches

**Problem:** The publisher actor has a 1ms flush timer. When `publishBatch` sends 64 messages, the batch never reaches the 256-message threshold, so every batch waits 1ms.

**Fix:** After receiving all messages from an explicit `publishBatch`, flush immediately. Detect that the mpsc is empty (`rx.is_empty()`) after the last message and flush without waiting for the timer.

**Location:** `crates/rabbit-rs-core/src/publisher/actor.rs:436-441`, `crates/rabbit-rs-core/src/client.rs:624`

**Gain:** ~14% on publish (9K -> 10.5K msgs/s)

**Laravel compatibility:** Internal to Rust. No API change.

### 1b. Reduce per-message FFI conversion

**Problem:** Each message in `publishBatch` goes through `reject_unknown_keys()` (iterates all keys) + 10+ hash lookups + 4-6 string allocations.

**Fix:**
- Validate message format once at Pool creation, not per message
- Eliminate `reject_unknown_keys` in the hot path (make it optional via a debug flag)
- Pre-compute known key positions in the ZendHashTable
- **Defer header path formatting to error branches** — `format!("{path}.headers.{key}")` runs on every header even on the success path. Move it to the error branch only. (From PR #4, commit `3a3fb76`)
- **Split `add_header_bytes` key-overflow path** — the overflow check was branching on every key. Split into a fast path (no overflow) and slow path (overflow detected), preserving correctness without the per-key branch. (From PR #4, commit `3d117df`)

**Location:** `crates/rabbit-rs-php/src/conversion.rs:85-143`

**Gain:** 5-16% on publish (PR #4 optimizations already provide a portion of this)

**Laravel compatibility:** No API change. Message array format stays identical.

**Status:** PR #4 commits `3a3fb76` and `3d117df` implement the header path and overflow splits. Cherry-pick these into the perf MR.

### 1c. Reduce per-message clones and allocations

**Problem:** `into_transport_request()` clones exchange, routing_key, payload, message_id per message. The batcher allocates a fresh `Vec` on every flush.

**Fix:**
- Use `Arc<str>` for exchange/routing_key/message_id and `Bytes` (refcount, clone = Arc increment) for payload
- **Spare-vec swap in `Batcher::take`** — instead of `std::mem::take` which allocates a new `Vec` each flush, swap the items vec with a pre-allocated spare vec, then replace the spare with a fresh allocation. This reuses the allocation across flushes. (From PR #4, commit `415c50b`)
- **Move exchange/routing_key in Lapin publish** — the transport layer cloned `exchange` and `routing_key` on every `channel.publish()` call. Move them by reference or use `&str` to avoid two `String` clones per published message. (From PR #4, commit `b424601`)

**Location:** `crates/rabbit-rs-core/src/publisher/actor.rs:590-637`, `crates/rabbit-rs-core/src/publisher/batcher.rs`, `crates/rabbit-rs-core/src/transport/lapin.rs`

**Gain:** 1-3% on publish (clones) + batcher allocation reuse

**Laravel compatibility:** No API change.

**Status:** PR #4 commits `415c50b` (batcher spare-vec) and `b424601` (Lapin move) are ready to cherry-pick. The batcher optimization becomes obsolete once phase 3b (hot path bypass) removes the batcher, but is valuable in the interim.

---

## Phase 2: Buffered Consumer (major change)

### 2a. Background pump + local buffer for consume

**Problem:** Each `next()` crosses FFI + `block_on` + mpsc + oneshot = ~30-50us overhead per message.

**Solution:** The consumer actor spawns a Tokio task that continuously drains the Lapin consumer into a `flume::bounded(buffer_size)` channel. When PHP calls `next()`:

1. **Fast path** (buffer non-empty): `flume.try_recv()` — sub-microsecond return, no `block_on`
2. **Slow path** (buffer empty): `block_on(flume.recv_timeout(timeout))` — waits for background task to receive

```
┌─ Consumer Actor (cold path) ─────────────────────────┐
│  • Creates Lapin consumer (basic_consume)            │
│  • Manages QoS, recovery, consumer tag                │
│  • Spawns background pump task                        │
│  • Collects stats (reads atomics)                     │
│  • Closes consumer + buffer on close()               │
└───────────┬──────────────────────────────────────────┘
            │  spawn
            ▼
┌─ Background Pump Task (hot path) ────────────────────┐
│  loop {                                              │
│    delivery = lapin_consumer.next().await             │
│    DELIVERIES_TOTAL.fetch_add(1)   // atomic          │
│    flume.send_async(delivery).await                   │
│    // flume::bounded blocks if buffer full            │
│    // -> natural flow control via prefetch            │
│  }                                                   │
└───────────┬──────────────────────────────────────────┘
            │  flume::bounded(buffer_size)
            ▼
┌─ PHP next() (hot path) ──────────────────────────────┐
│  FFI crossing (single)                               │
│  -> try_recv()  // fast path, ~0.5us                  │
│  -> recv_timeout() // slow path, ~5-10us             │
│  -> return Delivery                                  │
└──────────────────────────────────────────────────────┘
```

**Buffer size:** 1.5x the prefetch count, rounded up. Example: prefetch 16 -> buffer 24. This gives the background task headroom to pre-fetch messages while PHP is processing one. Future: adaptive buffer that grows/shrinks based on observed consume rate.

**Recovery:**
- If the connection drops, the background task is cancelled (Lapin consumer stream closes)
- The buffer is flushed (deliveries are re-delivered by RabbitMQ as unacked)
- The actor recreates the consumer after reconnection and spawns a new background task
- Deliveries in the buffer at crash time are stale — marked with connection generation. If generation mismatch, they are discarded (RabbitMQ redelivers them)

**Stats:**
- Background task increments `DELIVERIES_TOTAL` (AtomicU64) per message received
- `next()` increments `ACKS_TOTAL` when ack is called
- Settlement latency measured by background task (time between Lapin receive and ack)

**Location:** `crates/rabbit-rs-core/src/consumer/set.rs`, `crates/rabbit-rs-php/src/classes/consumer.rs`

**Gain:** 4-7x on consume (7K -> 30-50K msgs/s)

**Laravel compatibility:** **No API change.** `Consumer::next(int $timeoutMs): ?Delivery` keeps the same signature. `Delivery::ack()`, `release()`, `reject()` unchanged. The `pop()` loop in `RabbitMqQueue` works identically.

### 2b. Batched ack with `multiple=true` (lock-free queue)

**Problem:** Each `Delivery::ack()` crosses FFI + `block_on` + mpsc + oneshot = ~30-50us overhead.

**Solution:** The ack is pushed to a `crossbeam_queue::SegQueue<u64>` (lock-free) that the actor drains in the background. PHP returns immediately.

```
PHP ack() -> FFI -> lock-free queue.push(delivery_tag) -> return immediately
Background: actor drains queue -> Lapin basic_ack(multiple=true)
```

The `multiple=true` flag (AMQP protocol feature, used by `ext-amqp` via `AMQP_MULTIPLE`) allows acking all messages up to a given delivery_tag in a single AMQP frame. Instead of sending N frames for N acks, the actor drains the queue, takes the highest tag, and sends one `basic_ack(tag, multiple=true)` frame. This reduces AMQP frame overhead by Nx.

**Coalescing strategy:**
- The actor drains the lock-free queue on a timer (every 1ms) or when it reaches a threshold (16 tags)
- It sends a single `basic_ack(highest_tag, multiple=true)` for all drained tags
- If tags are non-contiguous (gap in sequence), it sends multiple acks up to each gap
- For `release()` / `nack()`, the same queue is used with a `requeue=true` flag

**Location:** `crates/rabbit-rs-core/src/consumer/delivery.rs`, `crates/rabbit-rs-php/src/classes/delivery.rs`

**Gain:** 15-25x on ack (30-50us -> 1-2us per ack) + Nx fewer AMQP frames

**Laravel compatibility:** `Delivery::ack()` and `Delivery::release()` keep the same signature. Behavior is identical — ack is just async internally.

**Risk:** If the process crashes before the actor drains the ack queue, unacked messages are re-delivered by RabbitMQ. This is **conformant with the at-least-once contract** — duplicates are permitted and identifiable via `message_id`.

### 2c. Callback consume (1 FFI crossing per batch)

**Problem:** Even with the buffered consumer (2a), each `next()` is one FFI crossing. At 50K msgs/s, that's 50K FFI crossings/s.

**Solution:** A callback-based consume API where the Rust side calls a PHP callable with a batch of deliveries in a single FFI crossing. This mirrors `ext-amqp`'s `AMQPQueue::consume(callback)` which loops in C and calls `zend_call_function` for each message without crossing FFI.

```php
$consumer->consume(function (Delivery $delivery): void {
    $delivery->ack();  // lock-free queue, no FFI
    process($delivery->payload());
}, count: 0, timeoutMs: 1000);
```

**How it works:**
1. PHP calls `consume(callback, count, timeout)` — 1 FFI crossing
2. Rust enters a `block_on` loop that drains the `flume` buffer
3. For each delivery, Rust calls the PHP callback via `ext-php-rs` `ZendCallable`
4. The callback processes the message and calls `ack()` (which pushes to the lock-free queue — no FFI)
5. Loop continues until `count` messages are processed or timeout
6. Returns the number of messages processed

**`count=0` means unlimited** — the loop runs until timeout or the consumer is closed. This matches the Laravel `queue:work` daemon pattern.

**Why this is faster than `next()` in a loop:**
- 1 FFI crossing for the entire batch vs N crossings for `next()` × N
- The callback is called from within the Rust `block_on`, so the delivery is already in Rust memory — no marshalling back to PHP and back
- `ack()` inside the callback uses the lock-free queue (phase 2b) — no FFI

**Location:** `crates/rabbit-rs-php/src/classes/consumer.rs`, `crates/rabbit-rs-core/src/consumer/set.rs`

**Gain:** 30-50K -> 80-100K msgs/s (FFI crossings reduced from N to 1)

**Laravel compatibility:** Additive. Laravel's `pop()` loop using `next()` continues to work. The callback API is an **optional optimization** that the Laravel package can adopt in a future version by replacing its `pop()` loop with `consume()`. No breaking change.

### 2d. Iterator API (syntactic sugar)

**Solution:** `Consumer` implements `IteratorAggregate` so it can be used in `foreach`:

```php
foreach ($pool->consumer('default') as $delivery) {
    $delivery->ack();
    process($delivery->payload());
}
```

The iterator calls `next()` internally with a configurable default timeout. `next()` remains the primary API.

**Location:** `crates/rabbit-rs-php/src/classes/consumer.rs`

**Gain:** Ergonomic, no perf gain

**Laravel compatibility:** Additive. Laravel's `pop()` loop is unchanged.

---

## Phase 3: Publish Hot Path Optimization

### 3a. Batch AMQP frames

**Problem:** The publisher sends 64 sequential `channel.publish().await` calls per batch. Each await has ~5-20us of async overhead.

**Solution:** New `publish_batch` method on the Transport that sends all BasicPublish frames to Lapin without awaiting between each, then a single final await.

**Location:** `crates/rabbit-rs-core/src/publisher/actor.rs:519-588`, `crates/rabbit-rs-core/src/transport.rs`

**Gain:** 5-18% on publish

**Laravel compatibility:** Internal to Rust. `publishBatch(array $messages): array` keeps the same signature.

### 3b. Hot path bypass for publish

**Problem:** Each publish traverses 13 layers: PHP -> FFI -> conversion -> block_on -> ClientPool -> mpsc -> actor -> batcher -> 1ms timer -> Lapin -> socket.

**Solution:** The actor exposes an `Arc<lapin::Channel>` at the first publish. The hot path becomes: PHP -> FFI -> conversion -> `channel.basic_publish()` -> socket. The actor remains the channel owner for the cold path.

**`now_or_never()` optimization:** Lapin's `basic_publish()` returns a `Future` that is almost always `Ready` immediately (it writes to the socket buffer and returns). Instead of `block_on(future)` which goes through the full Tokio poll/wake/schedule cycle, use `future.now_or_never()` which polls once and returns if ready:

```rust
match channel.basic_publish(...).now_or_never() {
    Some(result) => result,  // fast path — ~99% of the time
    None => block_on(channel.basic_publish(...)),  // slow path — socket buffer full
}
```

This eliminates the Tokio runtime overhead (poll, wake, schedule, context switch) for the common case. Inspired by `ext-amqp` which calls `amqp_basic_publish()` directly — no async runtime at all.

```
┌─ Publish Actor (cold path) ─────────────────────────┐
│  • Creates and owns the lapin::Channel              │
│  • Manages confirm_select, recovery                 │
│  • Exposes Arc<Channel> on first publish             │
│  • If channel closed -> provides a new one          │
│  • Collects stats (reads atomics)                   │
└───────────┬──────────────────────────────────────────┘
            │  Arc<Channel> via ArcSwap
            ▼
┌─ PHP publish() (hot path) ──────────────────────────┐
│  FFI -> conversion -> now_or_never(                  │
│    channel.basic_publish(exchange, routing_key,      │
│                          payload, props)             │
│  ) -> socket                                        │
│  PUBLISHES_TOTAL.fetch_add(1)  // atomic             │
│  // No actor, no mpsc, no timer, no block_on          │
└──────────────────────────────────────────────────────┘
```

If the channel is closed (detected via `channel.status().connected()`), PHP re-requests a channel from the actor (cold path). The actor creates a new one and exposes it.

**Recovery:** The actor watches the channel. If closed, it invalidates the `Arc<Channel>` on the PHP side (via `ArcSwap` or an `AtomicBool` flag). The next publish detects invalidation and re-requests a channel.

**Stats:** The PHP publish path increments atomic counters (`AtomicU64`) that the actor reads for `stats()`. The confirms driver (phase 3c) measures confirmation latency.

**Location:** `crates/rabbit-rs-core/src/client.rs`, `crates/rabbit-rs-core/src/publisher/actor.rs`, `crates/rabbit-rs-php/src/classes/pool.rs`

**Gain:** ~50-80% of indirection overhead eliminated (9K -> 30-40K msgs/s)

**Laravel compatibility:** `Pool::publish(array $message): string` and `Pool::publishBatch(array $messages): array` keep the same signature. Exceptions (`BackpressureException`, `ConnectionException`) stay the same types.

### 3c. Parallel confirms

**Problem:** Each publish in safe mode waits for a oneshot from the actor for confirmation. Confirms are sequential.

**Solution:** Replace sequential confirms with a confirms driver using `FuturesUnordered`, like the old implementation. Publishes don't block waiting for each ack — a `ConfirmTracker` (atomics) counts `published` and `confirmed`. `wait_for_confirms()` waits until `confirmed >= watermark`.

**ConfirmLedger optimization:** The `ConfirmLedger` that tracks in-flight publishes by sequence number uses a `BTreeMap` (O(log n) insert/remove). Replace with `HashMap` for O(1) insert/remove on the hot path. The deterministic drain order invariant (required for recovery) is preserved by sorting entries on `drain()` — a cold-path operation called only during suspend/fail_all. Also correct the over-allocation: pre-allocate to `max_messages` (batch size, default 256) instead of `buffer_capacity` (default 1024). (From PR #4, commits `95142ea` and `6cb343f`)

```
PHP publish(msg, safety='safe') ->
  block_on(channel.basic_publish().await)  // frame sent
  -> tracker.published.fetch_add(1)        // return immediately
  // Confirm arrives async, tracker records it

PHP wait_for_confirms() (called after a batch) ->
  block_on(async { loop until confirmed >= watermark })
```

`wait_for_confirms()` is called automatically after each `publishBatch()` for backward compatibility. It can also be called explicitly.

**Stats collection:**
- `PUBLISHES_TOTAL` (AtomicU64) — incremented by the hot path after `basic_publish().await`
- `CONFIRMATIONS_TOTAL` (AtomicU64) — incremented by the confirms driver when an Ack is received
- `RETURNS_TOTAL` (AtomicU64) — incremented when a basic_return is received
- `CONFIRMATION_LATENCY` — measured by the confirms driver: `confirm_time - publish_time`. The `publish_time` is stored in a `ConcurrentHashMap<delivery_tag, Instant>` or passed via BasicProperties timestamp. Recorded into a lock-free histogram with 1/100 sampling to reduce overhead.

**Location:** New subsystem in `crates/rabbit-rs-core/src/publisher/confirms.rs`

**Gain:** major gain on safe mode (30-40K -> 35-45K msgs/s)

**Laravel compatibility:** Config `publisher.confirms: true` enables safe mode. `timeout_ms` in the message controls confirm timeout. `publish()` always returns the `message_id`. `publishBatch()` calls `wait_for_confirms()` before returning.

### 3d. PHP-side publish buffer with auto-flush

**Problem:** Laravel calls `publish()` per job, not `publishBatch()`. Each `publish()` = 1 FFI crossing. At 50K msgs/s, that's 50K FFI crossings/s — the bottleneck shifts from the actor to the FFI boundary itself.

**Solution:** A buffer on the PHP side (inside the extension, not userland PHP) that accumulates messages and flushes in batch. This mirrors `ext-amqp`'s approach where `amqp_basic_publish()` writes directly to the socket buffer — but we batch at the FFI boundary instead.

```php
$pool->publish($msg1);  // buffered in extension, no FFI to Rust
$pool->publish($msg2);  // buffered
// ...
$pool->publish($msg64); // triggers auto-flush -> 1 FFI crossing for 64 messages
```

**Auto-flush triggers:**
- Message count threshold (default: 64, configurable)
- Timer (1ms via `hrtime()` check on each publish)
- Explicit `flush()` call
- `publishBatch()` — bypasses the buffer, sends directly
- Pool destruction / `MSHUTDOWN`

**Message ID generation:** The `message_id` is generated PHP-side (UUID via `random_bytes`) and returned immediately from `publish()` — the message hasn't been sent yet, but the ID is known. This matches the current `MessageMapper::messageId()` behavior in Laravel.

**Safety modes and the buffer:**
- `blind`: buffer + async pump (phase 4a) — `publish()` returns instantly
- `unsafe`: buffer + `now_or_never()` (phase 3b) — `publish()` returns after socket write
- `safe`: buffer + `now_or_never()` + `wait_for_confirms()` on flush — `publish()` returns after socket write, confirms waited on flush

For `safe` mode, `wait_for_confirms()` is called on flush, not per-publish. This means individual `publish()` calls return faster, but the batch confirm happens at flush time. `publishBatch()` flushes immediately and waits for confirms before returning (backward compatible).

**Location:** `crates/rabbit-rs-php/src/classes/pool.rs` (PHP-side buffer in Rust)

**Gain:** 50K -> 80-100K msgs/s (FFI crossings reduced from N to N/64)

**Laravel compatibility:** `publish(array $message): string` keeps the same signature and returns the `message_id`. The buffer is transparent — Laravel doesn't know it exists. `publishBatch()` bypasses the buffer for explicit batching. `flush()` is additive.

---

## Phase 4: True Fire-and-Forget (blind mode)

### 4a. Async pump for blind mode

**Problem:** No true fire-and-forget mode exists. Even in "unsafe" mode, the publish does a `block_on` + synchronous socket write.

**Solution:** A new `blind` mode where publish sends a `PublishJob` to an async pump via `flume::try_send` and returns **without `block_on`**. The pump publishes messages in the background.

```
PHP publish(msg, safety='blind') ->
  FFI -> PublishJob constructed -> flume.try_send(job) -> return immediately
  // No block_on, no socket wait

Background pump:
  loop { job = rx.recv_async().await
       channel.basic_publish(job.exchange, ...).await }
```

**API:**
- `Pool::publish(array $message): string` — the safety mode is determined by the pool config (`publisher.safety`)
- `Pool::publishBatch(array $messages): array` — same, safety from pool config
- The `safety` key in a message array can override the pool-level safety per-publish (optional, not used by Laravel by default)
- `Pool::flush(): void` — waits for the pump to drain its buffer (for tests and graceful shutdown)

**Safety modes (configurable at pool level via `publisher.safety`):**

| Mode | Behavior | Guarantee | Use case |
|------|----------|-----------|----------|
| `blind` | Send to async pump, return immediately, no socket wait | None — if socket drops between pump send and TCP write, message is lost | Logs, telemetry, non-critical |
| `unsafe` | `block_on(channel.basic_publish().await)` — synchronous socket write, no confirms | Message reached kernel socket buffer | Best-effort, high throughput |
| `safe` | `confirm_select` + mandatory + parallel confirms driver | At-least-once — broker received and routed the message | Critical jobs, Laravel default |

**Location:** `crates/rabbit-rs-core/src/publisher/pump.rs` (new), `crates/rabbit-rs-php/src/classes/pool.rs`

**Gain:** 50-80K msgs/s in blind publish (competitive with php-amqplib fire-and-forget at 83K)

**Laravel compatibility:**
- Config `publisher.safety` added to `config/rabbit-rs.php` (default: `safe`)
- `publish()` and `publishBatch()` keep the same signature
- Laravel can adopt `blind` for non-critical jobs without breaking `safe` for critical ones
- The `safety` per-message override is optional and additive

---

## Stats Collection Under Hot Path Bypass

### Problem

Currently, the actor collects all stats because all publishes pass through it. With the hot path bypass, the actor no longer sees individual publishes. The Laravel `RabbitMqStatusCommand` expects these keys from `stats()`:

- `publishes_total`, `confirmations_total`, `returns_total`, `backpressure_total`, `reconnects_total`
- `deliveries_total`, `acks_total`, `rejects_total`
- `confirmation_latency_p50`, `confirmation_latency_p95`, `confirmation_latency_p99`
- `settlement_latency_p50`, `settlement_latency_p95`, `settlement_latency_p99`

### Solution: Atomic counters + lock-free histogram

**Counters** (AtomicU64, incremented by hot path, read by `stats()`):
- `PUBLISHES_TOTAL` — incremented after `channel.basic_publish().await` completes
- `CONFIRMATIONS_TOTAL` — incremented by confirms driver on Ack receipt
- `RETURNS_TOTAL` — incremented by confirms driver on basic_return receipt
- `DELIVERIES_TOTAL` — incremented by background pump task on Lapin delivery
- `ACKS_TOTAL` — incremented by `next()` or ack batch drain
- `REJECTS_TOTAL` — incremented by `reject()` / `nack()`
- `BACKPRESSURE_TOTAL` — incremented when flume send fails (blind mode pump full)
- `RECONNECTS_TOTAL` — incremented by actor on reconnection

**Latency histograms** (lock-free, with 1/100 sampling):
- `confirmation_latency` — measured by confirms driver: `confirm_time - publish_time`. The `publish_time` is stored in a `ConcurrentHashMap<delivery_tag, Instant>` or passed via the Lapin `BasicProperties` timestamp field. Sampling reduces overhead.
- `settlement_latency` — measured by background pump: `ack_time - delivery_time`

**Implementation:**
- Use `hdrhistogram::Histogram` wrapped in a `parking_lot::Mutex` (sampled writes are infrequent, contention is low)
- Or a simpler `Vec<Duration>` ring buffer with 1000 samples, percentiles computed on `stats()` call
- The actor reads these atomics and histograms when `stats()` is called

**Stats shape preserved:** `stats()` returns the same keys the Laravel status command expects. No Laravel change needed.

---

## Implementation Order

1. **Phase 1** (1a + 1b + 1c) — quick wins, ~1-2 days, immediate gain, no risk
2. **Phase 2a** (buffered consumer) — biggest consume gain, ~2-3 days
3. **Phase 2b** (batched ack + `multiple=true`) — completes consume optimization, ~1-2 days
4. **Phase 2c** (callback consume) — 1 FFI crossing per batch, ~2-3 days
5. **Phase 2d** (iterator API) — syntactic sugar, ~0.5 days
6. **Phase 3a** (batch frames) — moderate publish gain, ~1 day
7. **Phase 3b** (hot path bypass + `now_or_never()`) — biggest publish gain, ~3-5 days
8. **Phase 3c** (parallel confirms) — optimizes safe mode, ~2-3 days
9. **Phase 3d** (PHP-side publish buffer) — reduces FFI crossings, ~2-3 days
10. **Phase 4a** (blind mode) — true fire-and-forget, ~2-3 days

Total: ~17-25 days of implementation.

---

## Estimated Gains Summary

| Phase | Change | Publish blind | Publish unsafe | Publish safe | Consume (poll) | Consume (callback) | Effort |
|-------|--------|:---:|:---:|:---:|:---:|:---:|:---:|
| - | Current | ~9K | ~9K | ~9K | ~7K | n/a | - |
| 1a | Immediate flush | - | - | 10.5K | - | - | Low |
| 1b | Reduce FFI conversion | - | - | 11.5K | - | - | Medium |
| 1c | Reduce clones | - | - | 12K | - | - | Low |
| 2a | **Buffered consumer** | - | - | - | **30-50K** | - | **Medium** |
| 2b | **Batched ack + multiple** | - | - | - | **+15-25x on ack** | - | **Medium** |
| 2c | **Callback consume** | - | - | - | - | **80-100K** | **Medium** |
| 2d | Iterator API | - | - | - | - (ergonomic) | - | Low |
| 3a | Batch AMQP frames | - | - | 14K | - | - | Medium |
| 3b | **Hot path + now_or_never** | 30-40K | 30-40K | 30-40K | - | - | **High** |
| 3c | Parallel confirms | - | - | 35-45K | - | - | High |
| 3d | **PHP publish buffer** | **80-100K** | **80-100K** | - | - | - | **Medium** |
| 4a | **Blind pump** | **80-100K** | - | - | - | - | **Medium** |

**Final targets:**
- Publish blind: 80-100K msgs/s (vs php-amqplib 83K fire-and-forget, vs ext-amqp ~100K)
- Publish unsafe: 50-80K msgs/s (vs php-amqplib 63K unsafe)
- Publish safe: 35-45K msgs/s (vs php-amqplib 39K safest)
- Consume (poll): 30-50K msgs/s (vs php-amqplib 17K)
- Consume (callback): 80-100K msgs/s (vs ext-amqp ~100K)

---

## Laravel Compatibility

### API surface preserved (no breaking changes)

**Pool:**
- `new Pool(array $config)` — same config format, new optional `publisher.safety` key
- `publish(array $message): string` — same signature, optional `safety` key in message
- `publishBatch(array $messages): array` — same signature
- `consumer(string $profile): Consumer` — unchanged
- `size()`, `clear()`, `stats()`, `close()` — unchanged
- `onConnectionState()`, `onBackpressure()` — unchanged
- New: `flush(): void` — waits for blind pump to drain (additive)

**Consumer:**
- `next(int $timeoutMs): ?Delivery` — unchanged (buffered internally)
- `close(): void` — unchanged
- New: `consume(callable $handler, int $count = 0, int $timeoutMs = 1000): int` — callback batch consume (additive, optional)
- New: `IteratorAggregate` for `foreach` support (additive)

**Delivery:**
- `payload()`, `metadata()`, `ack()`, `release()`, `reject()` — all unchanged
- `ack()` becomes async internally (lock-free queue) but signature and semantics are the same

**Exceptions:**
- `Exception`, `BackpressureException`, `ConnectionException` — all unchanged

**Stats:**
- `stats()` returns the same keys (including latency percentiles)
- Internally collected via atomics + lock-free histogram instead of actor

### Config additions (additive, backward-compatible)

```php
// config/rabbit-rs.php
'publisher' => [
    'safety' => 'safe',  // 'blind' | 'unsafe' | 'safe' (default: 'safe')
    'confirms' => true,   // deprecated, use 'safety' => 'safe'
    'mandatory' => true,  // deprecated, use 'safety' => 'safe'
    'confirm_timeout' => 30000, // ms, unchanged
],
```

If `safety` is not set, the extension falls back to the existing `confirms`/`mandatory` flags for backward compatibility.

---

## Open Questions (for further iteration)

1. **Adaptive buffer size** — Phase 2a starts with 1.5x prefetch. Future: grow/shrink based on consume rate. How to detect consume rate? Moving average of `next()` call frequency vs `deliveries_total` rate.

2. **Blind mode durability** — If the process crashes with messages in the blind pump buffer, they are lost. Should we expose a `flush()` on `__destruct()` / `MSHUTDOWN`? Is this conformant with the at-least-once contract (blind mode is explicitly "no guarantee")?

3. **Per-publish safety override** — Should the `safety` key in a message array override the pool-level safety? This is powerful but adds per-message overhead (checking the key). Alternative: only check at `publishBatch` level, not per-message.

4. **Confirm timeout per-batch** — Currently `timeout_ms` is per-message. With parallel confirms, should `wait_for_confirms()` use a single timeout for the whole batch or respect per-message timeouts?

5. **ArcSwap vs AtomicBool for channel invalidation** — `ArcSwap` allows hot-swapping the `Arc<Channel>` atomically. `AtomicBool` is simpler but requires a lock on re-fetch. Which is better for the hot path?

---

## Insights from Reference Implementations

### Old `php-ext-rabbit-rs` (archived, Rust + ext-php-rs)

The old implementation achieved ~30-40K publish / ~15-20K consume with a simpler architecture:

- **3 layers** between PHP and socket (PHP -> `AmqpChannel` -> Lapin -> socket) vs our 13 layers
- **Publisher pump** using `flume` bounded channel + `FuturesUnordered` for parallel confirms — publishes are non-blocking, confirms handled by a separate driver
- **`simple_consume_poll_batch(timeout, max)`** — retrieves a batch of messages in a single FFI crossing, not one `next()` per message
- No actor pattern, no mpsc, no batcher, no 1ms timer — direct channel access

We keep the actor for the cold path (recovery, topology, fairness) but adopt the pump and batch patterns for the hot path.

### `ext-amqp` (pecl, C + librabbitmq)

The `ext-amqp` extension is the performance reference for PHP RabbitMQ clients (~100K msgs/s). Key architectural choices:

- **1 layer** between PHP and socket — `amqp_basic_publish()` writes directly to the socket buffer, no async runtime
- **Callback consume** — `AMQPQueue::consume(callback)` loops in C, calls `zend_call_function` per message, zero FFI crossings per message
- **`AMQP_MULTIPLE` flag** — `basic_ack(tag, multiple=true)` acks all messages up to `tag` in a single AMQP frame
- **Synchronous direct calls** — no `block_on`, no `Future`, no runtime — just C function calls

We can't go fully synchronous (Lapin is async-only), but `now_or_never()` approximates the synchronous path for the ~99% case where `basic_publish()` is immediately ready.

### Zero-copy analysis (ext-php-rs)

ext-php-rs provides zero-copy **read** via `Zval::str()` / `ZendStr::as_bytes()` (returns `&[u8]` directly on PHP memory). Zero-copy **write** is impossible — `zend_string` uses inline storage (`char val[1]`), so data must be copied into the allocation.

- **Consume**: replace `Bytes::copy_from_slice()` with a wrapper around `&[u8]` from `ZendStr::as_bytes()` — saves 1 copy per message (~5-10% on consume)
- **Publish**: no zero-copy possible, but the PHP-side buffer (3d) reduces FFI crossings which matters more than the copy
- Zero-copy is a **second-order optimization** — the primary gains come from reducing FFI crossings (batch consume, publish buffer) and eliminating the actor overhead (hot path bypass)
