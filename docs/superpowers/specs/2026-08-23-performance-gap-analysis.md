# Performance gap analysis — rabbit-rs vs targets

**Date:** 2026-08-23
**Branch:** `perf/improve-perf`
**Context:** Post-implementation of the performance correction plan (11 tasks, PR #13)

## Targets vs reality

| Metric | Target | Current (before fix) | Gap |
|----------|-------|---------------------|------|
| Publish msg/s (safe) | 40-60k | ~19k | 2-3x |
| Consume msg/s (manual ack) | 25-30k | ~16k | 1.5-2x |
| Consume p99 | <300ms | ~646ms | 2x |

The implementation optimized the Rust actor (settlement lanes, pipeline, event dispatch), but the bottleneck lies elsewhere.

---

## 1. Implementation limitations (code we wrote)

### 1.1 Publish — Double cloning per message

`PublishRequest` is cloned in `publish_batch()` (`client.rs:150`), then re-cloned in
`into_transport_request()` (`actor.rs:702-748`). Two rounds of string allocations (exchange,
routing_key, message_id, correlation_id…) × 256 per batch.

`PublishRequest` (`publisher/mod.rs:146-179`) contains:
- `Destination` with two `String`s (heap allocs)
- `Bytes` payload (cheap, Arc'd)
- `MessageProperties` with `message_id: String`, optional `content_type`, `correlation_id`, and `PublishHeaders`

Then `into_transport_request()` (`actor.rs:702-748`) clones all of that **again** into a `TransportRequest`.

### 1.2 Publish — `async_trait` = BoxFuture per publish

`Transport::publish()` is `#[async_trait]` (`transport/lapin.rs:144`), so every call to
`now_or_never()` allocates a `Box::pin` + vtable dispatch (`actor.rs:623`). The fast path of
`now_or_never()` avoids runtime scheduling, but still pays:
- A heap allocation for the `BoxFuture` (`actor.rs:623`)
- A vtable indirection through `dyn PublisherChannel`
- An `Arc::clone(&channel)` per publish (`actor.rs:622`)
- An `into_transport_request()` that clones exchange, routing_key, payload, and all the
  properties (`actor.rs:702-748`)

### 1.3 Publish — Sequential waiter polling

`publish_batch` (`client.rs:159-169`) awaits each `waiter.wait()` one by one in a for loop.
Confirmations arrive in order (the total time ≈ time for all confirms, not
256× the individual time), but there are 256 await/wake cycles per batch.

amqplib uses a single `wait_for_pending_acks(5)` call that blocks once for all pending
confirms — much less overhead.

### 1.4 Publish — Single publisher actor, no channel parallelism

The publisher actor runs on a single `tokio::spawn` (`actor.rs:80`). It processes
`Command::Publish` one at a time via `select!`. Each command triggers `accept_publish` →
`publish_queue` with a 1-element queue (`actor.rs:488-493`). The actor can pipeline frames via
`now_or_never()`, but cannot parallelize across channels — the architecture
uses one channel per publisher actor.

### 1.5 Consume — `ackThrough` + `block_on` = stop-and-wait

`ackThrough` (`consumer.rs:155-157`) calls `self.runtime.block_on(self.handle.ack_through(...))`.
This parks the PHP thread until Lapin writes the `basic_ack` frame to the socket.

Resulting pattern: fetch ~16 → ack → wait → fetch ~16 → ack → wait.

amqplib pipelines continuously: `$msg->ack()` is fire-and-forget (writes the frame, returns
immediately), and `wait()` reads the next delivery. Messages are permanently in flight
while acks are in flight.

### 1.6 Consume — Headers deep-clone per delivery

`actor.rs:232`: `let headers = Arc::new(delivery.headers.clone());` — `delivery.headers` is
a `HashMap<String, HeaderValue>`. This is a **deep clone** of the HashMap followed by an Arc
allocation, per delivery. This is the most expensive clone in the hot path.

`delivery.payload.clone()` (`actor.rs:286`, `actor.rs:301`) — `Bytes` clone (Arc bump, cheap).

The plan (Task 3, Step 15) said to use `Arc::clone(&delivery.headers)` but
`delivery.headers` is not an `Arc` — it is `Headers` (a HashMap). The clone is
unavoidable without changing `TransportDelivery` to hold `Arc<Headers>`.

### 1.7 Consume — `nextBatch` never returns 256

The flume buffer is sized at `consumer/set.rs:175-179`:

```rust
let total_prefetch: u64 = subscriptions.iter().map(|s| u64::from(s.prefetch)).sum();
let buffer_size = usize::try_from(total_prefetch).unwrap_or(usize::MAX) * BUFFER_CAPACITY_FACTOR / 2;
```

With prefetch=16 and `BUFFER_CAPACITY_FACTOR=3`: buffer_size = **24**.
`nextBatch(256)` can never return more than what is in the buffer (~16-24).
The effective batch size is limited by the prefetch, not by the API parameter.

---

## 2. Design divergences vs amqplib

| Aspect | rabbit-rs | amqplib |
|--------|-----------|---------|
| Ack model | `block_on(ack_through)` — synchronous, parks the PHP thread | `$msg->ack()` — fire-and-forget, writes the frame and returns |
| Consume model | Polling (`nextBatch`/`tryNext`) — no PHP callbacks from Rust | Callback (`$channel->consume(callback)`) — messages pushed to the callback |
| `no_ack` mode | **Hardcoded `false`** (`lapin.rs:246`) — not implemented | `no_ack=true` — zero ack frames, RabbitMQ auto-acks internally |
| early_ack | 1 `tokio::spawn` per delivery (`actor.rs:238`) for individual acks | N/A (uses `no_ack`) |
| Confirms | `now_or_never()` + `FuturesUnordered` — per-future polling | `wait_for_pending_acks(5)` — a single call blocks for all |
| Channel parallelism | 1 actor, 1 channel per publisher | Multi-channel in the same process |

The rabbit-rs polling model (by architectural constraint: no PHP callbacks from
Rust threads) fundamentally introduces more latency than amqplib's callback model.
This is an architecture choice, not a bug.

---

## 3. Benchmark methodology

### 3.1 AUTO_ACK unfairly compared

- `RabbitRsDriver.php:60`: AUTO_ACK sets `confirms=true`
- `AmqplibDriver.php:89-98`: AUTO_ACK uses the fire-and-forget path (no
  `confirm_select()`, no `wait_for_pending_acks`)

rabbit-rs does publisher confirms + early_ack (spawning tasks, tracking confirms) while
amqplib does fire-and-forget publish + `no_ack=true` consume. amqplib has zero confirm
overhead and zero ack overhead.

### 3.2 Prefetch=16 limits batches

`Config.php:30`: `PREFETCH_COUNT = 16` (equalized with amqplib for fairness).

But this limits `nextBatch(256)` to ~16-24 messages per batch. The benchmark cannot
test the case where `nextBatch` would return large batches — a higher prefetch would be
needed for rabbit-rs (which could support a wider buffer) or a dedicated scenario.

### 3.3 p99 is an artifact of stop-and-wait

With prefetch=16, the pattern is:
1. `nextBatch` returns ~16 messages (fast)
2. `ackThrough` → `block_on` → writes the ack frame → unparks PHP (sync point)
3. RabbitMQ releases the prefetch → sends the next 16
4. Source task reads → actor dispatch → flume → PHP
5. `nextBatch` returns the next ~16

If any step of this chain has latency (network, runtime scheduling, `block_on`
overhead), messages at the end of the queue wait for the full round-trip. With 10,000
messages and ~625 round-trips (10000/16), even a small per-round-trip overhead accumulates
into a high p99.

amqplib maintains a continuous pipeline — messages are always in flight, so p99 is
bounded by processing time, not by round-trip waiting.

---

## 4. What was not implemented from the plan

### 4.1 `no_ack` true mode — not implemented

Explicitly deferred by the plan (merge order step 10: "if benchmarks prove necessity").
`transport/lapin.rs:246` still hardcodes `no_ack: false`.

Yet this was the main lever to catch up with amqplib in auto-ack: `no_ack=true`
completely removes ack frames (RabbitMQ auto-acks internally), whereas `early_ack`
still spawns one task per delivery to send an individual ack.

### 4.2 Byte budgets — soft limit only

Added in the fix wave but it is a soft limit: it only skips `dispatch()` in the `Incoming`
handler (`actor.rs:407-409`), not a hard gate on all `dispatch()` paths. Other
paths (settlement completion, `dispatch_notify`) do not check the byte budget.

### 4.3 `now_or_never()` tested only with a synchronous mock

The mock transport (`mock.rs`) always returns `Ok(receipt)` synchronously without yielding,
so `now_or_never()` always returns `Some` on the first poll. The `now_or_never()`
fast path is not validated against real Lapin — we do not know whether `basic_publish` truly
completes synchronously or yields in practice.

The `pipeline_publishes_before_confirmation` test (`publisher.rs:598-627`) passes identically
with the old code (sequential `.await`) and the new one (`now_or_never()`) — it cannot
distinguish the two implementations.

---

## 5. FFI and runtime

### 5.1 `block_on` per FFI call

Each `block_on` parks the PHP thread, schedules the future on the Tokio multi-thread runtime,
polls, and unparks. Estimated overhead: ~5-15 μs per call (thread park/unpark + runtime
scheduling).

- `nextBatch` fast path (flume `try_recv`): **no `block_on`** — lock-free. ✓
- `nextBatch` slow path (empty buffer): `block_on` wraps `time::timeout(dur, handle.next())`.
- `ackThrough`: `block_on` — parks until the ack completes.
- `tryNext`: no `block_on` (lock-free `try_recv`). ✓
- `next(timeoutMs)`: `block_on` on `time::timeout`.
- `ack()`: `block_on` — parks until the ack completes.

### 5.2 Single shared runtime

`runtime.rs:44-49`: a single `multi_thread` runtime per process, shared by all pool
handles. All actors, source tasks, settlement futures, and `block_on` calls share
this runtime. No runtime creation per call. ✓

### 5.3 Publisher handle cache — used

`client.rs:485-538`: `publisher()` checks the cache first. For the benchmark, all
messages go to the 'default' broker, so the handle is acquired once and reused.
`publish_batch` (`client.rs:147`) calls `self.publisher(broker)` once per broker group. ✓

---

## 6. Root cause summary

| Target | Root cause | Location |
|--------|-------------------|-------------|
| Publish 40-60k | Double cloning per message (PublishRequest + TransportRequest) | `client.rs:150`, `actor.rs:702-748` |
| Publish 40-60k | `async_trait` BoxFuture allocation per publish | `transport/lapin.rs:144`, `actor.rs:623` |
| Publish 40-60k | Sequential waiter polling (256 await cycles per batch) | `client.rs:159-169` |
| Publish 40-60k | Single publisher actor, no channel parallelism | `actor.rs:80` |
| Consume 25-30k | prefetch=16 limits batches to ~16, not 256 | `Config.php:30`, `set.rs:175-179` |
| Consume 25-30k | `ackThrough` + `block_on` creates a stop-and-wait sync point | `consumer.rs:155-157` |
| Consume 25-30k | amqplib pipelines ack+read, rabbit-rs serializes them | `AmqplibDriver.php:143-164` vs `RabbitRsDriver.php:135-152` |
| Consume p99 <300ms | Stop-and-wait: ~625 round-trips for 10k messages, latency accumulates | `consumer.rs:155-157` + prefetch=16 |
| Consume p99 <300ms | Deep HashMap clone per delivery in dispatch | `actor.rs:232` |
| AUTO_ACK unfair | rabbit-rs: confirms=true + early_ack (spawned acks); amqplib: fire-and-forget + no_ack=true | `RabbitRsDriver.php:60` vs `AmqplibDriver.php:89-98` |
| early_ack overhead | `no_ack` hardcoded false, 1 tokio::spawn per delivery to ack | `lapin.rs:246`, `actor.rs:238` |
| `no_ack` not implemented | Explicitly deferred (step 10) — it was the main lever for auto-ack | `lapin.rs:246` |

---

## 7. Improvement avenues (non-exhaustive)

### Short term
1. **Implement `no_ack=true`** in the transport (`lapin.rs:246`) — removes all ack
   frames in auto-ack mode
2. **Eliminate double cloning** — make `into_transport_request()` take ownership of
   `PublishRequest` instead of cloning
3. **Replace `async_trait`** with a concrete impl or `impl Trait` to avoid the BoxFuture
4. **Raise the prefetch** for rabbit-rs in the benchmark (it can support a bigger
   buffer thanks to the byte budget)
5. **Fix the AUTO_ACK unfairness** — rabbit-rs should do fire-and-forget + no_ack like
   amqplib in this scenario

### Medium term
6. **Async ack** — let `ackThrough` return before the frame is written (fire-
   and-forget with ordering guarantee)
7. **Multi-channel publish** — parallelize across channels in the publisher actor
8. **`Arc<Headers>` in `TransportDelivery`** — eliminate the deep HashMap clone
9. **Batch wait** — a single call that waits for all pending confirms (like
   `wait_for_pending_acks`)
10. **Adaptive prefetch** — dynamically adjust prefetch based on the processing rate

### Long term
11. **Reassess polling vs callback model** — polling fundamentally introduces more
    latency. An async PHP API (via Fiber or Symfony Runtime) would enable a continuous
    pipeline.
12. **Zero-copy pipeline** — carry payloads as `Bytes` (Arc'd) end to end
    without cloning, from PHP input to socket
