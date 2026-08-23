# Rabbit-rs Performance Gap Correction Design

**Date:** 2026-08-23  
**Status:** Draft  
**Scope:** Recovery bugs + Consumer bugs + OOM protection + Consume pipeline + Publish optimizations + Benchmark fairness  
**Predecessor:** `2026-08-22-rabbit-rs-performance-correction-design.md` (Implemented), `2026-08-23-performance-gap-analysis.md` (Diagnostic)

## Context

After PR #13 (`perf/improve-perf`), the performance gap analysis (`2026-08-23-performance-gap-analysis.md`)
verified all root causes in the current code. The targets remain unmet:

| Metric                    | Target    | Actual   | Gap    |
|---------------------------|-----------|----------|--------|
| Publish msg/s (safe)      | 40-60k    | ~19k     | 2-3x   |
| Consume msg/s (manual ack)| 25-30k    | ~16k     | 1.5-2x |
| Consume p99               | <300ms    | ~646ms   | 2x     |

The previous design (2026-08-22) optimized the actor (settlement lanes, pipeline, event dispatch), but the
bottleneck is elsewhere: the `block_on` ack model, double clones on the publish path, `async_trait` boxing,
sequential waiter polling, and benchmark unfairness.

## Decisions

| Decision                    | Choice                                                                              |
|-----------------------------|-------------------------------------------------------------------------------------|
| Recovery generation rollback | On `recover_generation` failure, roll back `last_generation` and drive actor back to `Recovering` so recovery re-attempts. |
| Recovery handle invalidation | Client cached handles carry generation; stale handles are invalidated on recovery. |
| Recovery multi-broker        | Start coordinators for all distinct brokers in a worker profile, not just the first. |
| Recovery old consumer cleanup | Close old `ConsumerSet` before inserting the new one in the coordinator's map. |
| `try_next_batch` partial     | Return partial batch on error; stash error for next call. Never discard extracted deliveries. |
| Ledger/budget on failure     | Only release budget and remove ledger on `Ok` or terminal errors (`StaleGeneration`, `Transport`). Retryable errors keep budget and ledger. |
| `ConsumerHandle` Drop         | Close on last drop (refcount-based), not first drop. Fix clone-then-drop footgun. |
| OOM hard gate                 | Actor stops accepting from mpsc when `buffered_bytes > max_buffered_bytes`. Backpressure propagates to `spawn_source` and Lapin. |
| OOM permits                   | `spawn_source` uses permit-based backpressure: only reads when permits available. Actor grants permits when under budget. |
| Ack model                   | Fire-and-forget with error queue. PHP gets `bool` receipt, errors surface at next `pop()` via `drainErrors()`. |
| `no_ack` mode               | Implement in transport (`lapin.rs`). Gated behind `best_effort=true` (already enforced). |
| Headers clone               | `Arc<Headers>` in `TransportDelivery`. Clone at transport boundary, refcount thereafter. |
| Prefetch                    | Raise to 128 globally in benchmark `Config.php` for all drivers.                   |
| Publish clone               | Arc-fields (`Arc<str>`) on `PublishRequest` — clone becomes refcount bump. `into_transport_request()` stays by-reference (replay needs retained original). |
| Publish boxing              | `TaggedFuture` wrapper — eliminates the actor's `Box::pin` allocation. Only `async_trait` BoxFuture remains (inevitable with `dyn Trait`). |
| Publish wait                | `wait_all()` with `FuturesUnordered` — single collective await instead of 256 sequential. |
| Multi-channel publish       | **Deferred** — too invasive for this plan. Single actor, single channel stays.      |
| Benchmark AUTO_ACK          | `confirms=false` + `no_ack=true` — matches amqplib's fire-and-forget + no_ack path. |
| Benchmark SKIP              | Fix recovery generation rollback first (root cause), then investigate timing if SKIP persists. |
| Laravel `pop()`             | One-at-a-time, optimized. No custom worker, no batch pop. `drainErrors()` called at top of `pop()`. |
| At-least-once               | Preserved. If ack fails or process crashes, RabbitMQ redelivers. Duplicates identified by `message_id`. |
| `unsafe_code`               | Remains `#![forbid(unsafe_code)]`. No changes.                                      |
| `nextBatch()` off-by-one    | Fix slow path: `max=1` returns 2 instead of 1. Skip `try_next_batch` when `max <= 1`. |
| `ackBatch()` bound          | Bound to 256 deliveries. Combined with fire-and-forget (no `block_on` per message). |
| Benchmark `basic_consume`   | Migrate BunnyDriver and AmqpExtDriver from `basic_get` (poll) to `basic_consume` (push) for fair comparison. |
| Benchmark `prefetch_count`  | Fix `' prefetch_count'` leading space in `LaravelCompareBenchmark.php:146`. |

## Section A — Recovery: Generation Rollback, Handle Invalidation, Multi-Broker

### Problem

Three compounding bugs in the recovery coordinator chain cause silent consumer stalls after any connection
recovery:

1. **Generation memorized before recovery succeeds** (`recovery_coordinator.rs:266-296`): `last_generation`
   is set to `generation` on line 270 **before** `recover_generation` runs on line 272. If recovery fails,
   the error is printed to stderr (line 293) and swallowed — no retry, no state transition. The coordinator
   is stuck in `Ready` with no consumer created. Subsequent `Ready { generation }` events are skipped
   because `generation == last_generation` (line 267).

2. **Client loops on `Ready` forever** (`client.rs:294-328`): `consumer()` and `publisher()` call
   `wait_for_state(|s| matches!(s, Ready { .. } | ...))`. When the coordinator is `Ready` but the consumer
   was never created (Bug 1), `wait_for_state` returns immediately (state is already `Ready`),
   `coordinator.consumer(profile)` returns `Err`, and the loop retries — hanging forever.

3. **Recreated consumer not in client cache** (`client.rs:47` + `recovery_coordinator.rs:448`): Recovery
   inserts a new `ConsumerHandle` in the **coordinator's** map (line 448), but the **client's** map
   (`client.rs:47`) still holds the stale handle from the pre-recovery connection. The client returns the
   stale handle indefinitely. The new handle is orphaned — its `ConsumerSet` actor task runs but nobody
   reads its buffer. Deliveries pile up silently.

4. **Multi-broker only starts first coordinator** (`client.rs:286-293`): `consumer()` only starts the
   coordinator for `worker.subscriptions[0].broker`. If a profile has subscriptions on multiple brokers,
   only the first broker's coordinator is started. The other brokers' subscriptions are never consumed.

### Design

#### A.1 — Generation rollback on recovery failure

In `recovery_coordinator.rs`, when `recover_generation` returns `Err`:

- Roll back `last_generation` so a re-issued `Ready` re-attempts recovery:
  `last_generation = generation.saturating_sub(1)`
- Drive the actor back to `Recovering` via `actor.connection_lost(...)` so the deterministic recovery
  order is re-attempted
- Record the failure in metrics (not just stderr)

#### A.2 — Fix client loop predicate

In `client.rs:294-328` and `client.rs:500-534`, change `wait_for_state` to wait for state transitions
**away** from `Ready`:

```rust
coordinator.wait_for_state(|state| {
    matches!(state, Recovering { .. } | Connecting { .. } | FailedPermanent { .. } | Closed)
}).await;
```

This makes the loop observe `Recovering → Ready` transitions and re-attempt after Bug A.1 drives the actor
back to `Recovering`.

#### A.3 — Generation-aware handle invalidation

Client cached handles (`consumers` and `publishers` maps) must carry their generation. On lookup, stale
handles (generation != current) are evicted and the client re-fetches from the coordinator.

- Add `generation: u64` field to `ConsumerHandle` and `PublisherHandle`
- `client.rs:ready()` checks `handle.generation() == current_generation` before returning cached handle
- If stale, evict and fall through to the coordinator

Additionally, `recover_generation` must close the old `ConsumerSet` before inserting the new one:

```rust
let mut guard = consumers.lock().await;
if let Some(old) = guard.insert(worker.name.clone(), consumer) {
    let _ = old.close().await;  // stop the old actor task, free channels
}
```

#### A.4 — Start all broker coordinators

In `client.rs:286-293`, iterate all distinct brokers in the worker profile:

```rust
let mut brokers: Vec<String> = worker.subscriptions.iter().map(|s| s.broker.clone()).collect();
brokers.dedup();
for broker in &brokers {
    self.coordinator(broker).await?;
}
```

Then wait for each coordinator to expose the consumer and merge handles. Since the current architecture
has one `ConsumerSet` per broker per worker, the client must compose a multi-broker consumer from all
coordinators' handles.

### At-least-once

- Bug A.1: without the fix, recovery failure means consumers are never recreated → RabbitMQ redelivers
  on reconnect but nobody consumes → silent stall. The fix ensures recovery re-attempts.
- Bug A.3: without the fix, the stale handle points to a dead connection → deliveries never arrive.
  The fix ensures the client always returns a handle on the current generation.

### Tests

- Recovery failure: `recover_generation` returns `Err` → `last_generation` rolled back → actor driven
  to `Recovering` → next `Ready` re-attempts
- Stale handle: cached handle with old generation is evicted on lookup → new handle fetched from
  coordinator
- Multi-broker: profile with 2 brokers → both coordinators started → both subscriptions consumed
- Old consumer cleanup: `recover_generation` closes old `ConsumerSet` before inserting new one

## Section B — Consumer: `try_next_batch` Partial Batch + Ledger/Budget on Failure

### Problem

Two consumer correctness bugs cause silent data loss and ledger corruption:

1. **`try_next_batch` discards extracted deliveries on error** (`set.rs:290-307`): when an error appears
   mid-batch, the already-extracted deliveries (in the `batch` Vec) are dropped. The user gets `Err(error)`
   instead of the partial batch. The dropped deliveries are unacked — `in_flight` is never decremented,
   causing a budget leak. After enough errors, `max_in_flight` is exhausted and the consumer stalls.

2. **Ledger/budget released after settlement failure** (`actor.rs:540-639`): on settlement completion, the
   actor calls `release_budget()` and `ledger.pending.remove()` **unconditionally** — even for retryable
   errors (non-`StaleGeneration`, non-`Transport`). The token is reset to `Pending` (user can retry), but
   the ledger entry is gone. On retry, `ack_through` fails with "delivery tag not found in ledger".

### Design

#### B.1 — `try_next_batch` partial batch with stashed error

Add a `pending_error: Option<ConsumerError>` to `ConsumerHandle` (using a `Mutex<Option<ConsumerError>>`
or `RefCell` if single-threaded access is guaranteed). On error mid-batch:

- If `batch` is non-empty: stash the error, return `Ok(batch)` (partial batch)
- If `batch` is empty: return `Err(error)` immediately
- At the top of `try_next_batch` and `try_next`: check stashed error first, return it if the previous
  batch was fully consumed

This ensures no deliveries are lost and the error is surfaced on the next call when the batch is empty.

#### B.2 — Conditional ledger/budget release

In `actor.rs:535-578` (single settle) and `actor.rs:580-645` (settle-through), split the cleanup by
outcome:

- **`Ok` (success)**: release budget, remove ledger entry, record metrics
- **`Err(StaleGeneration | Transport)` (terminal)**: release budget, remove ledger entry (delivery is
  `Lost`, user cannot retry)
- **`Err(other)` (retryable)**: do NOT release budget, do NOT remove ledger entry. Only reset the
  `settling` flag so the user can retry. The delivery stays `Pending` in the ledger.

### At-least-once

- Bug B.1: without the fix, extracted deliveries are silently dropped on error → budget leak → consumer
  stall. The fix returns the partial batch so the user can process and settle them.
- Bug B.2: without the fix, retryable failures corrupt the ledger → `ack_through` broken permanently.
  The fix preserves the ledger entry for retries.

### Tests

- `try_next_batch` with mid-batch error: returns partial `Ok(batch)`, stashes error, returns `Err` on
  next empty call
- Settlement failure (retryable): budget NOT released, ledger entry NOT removed, token reset to `Pending`
- Settlement failure (terminal): budget released, ledger removed, token set to `Lost`
- `ack_through` retry after retryable failure: succeeds (ledger entry preserved)

#### B.2 — `nextBatch()` PHP off-by-one

`consumer.rs:138` (PHP extension) slow path calls `try_next_batch(max.saturating_sub(1))`. When `max=1`,
`saturating_sub(1)` = 0, which the core clamps to 1 (`set.rs:291`: `max.clamp(1, 256)`). This returns up to
1 additional delivery, totaling 2 when the caller requested `max=1`.

Fix: skip `try_next_batch` when `max <= 1`:

```rust
let more = if max > 1 {
    self.handle.try_next_batch(max.saturating_sub(1))
        .map_err(|error| consumer_php_exception(&error))?
} else {
    Vec::new()
};
```

#### B.3 — `ackBatch()` bound + fire-and-forget

`consumer.rs:162-192` (PHP extension) iterates over all deliveries without a bound and calls `block_on`
per delivery. Two fixes:
1. Bound the loop to 256 deliveries (matching the core `try_next_batch` clamp)
2. Replace `block_on(ack())` with fire-and-forget `try_ack()` (from Section 1)

## Section C — OOM Protection: Hard Gate + Permit-Based Backpressure

### Problem

When `no_ack=true` (Section 2), RabbitMQ ignores prefetch and pushes messages without flow control. The
current backpressure chain has two gaps:

1. **Actor `VecDeque` unbounded** (`actor.rs:399-402`): the actor always accepts from the mpsc, even when
   `buffered_bytes > max_buffered_bytes`. The `over_budget` check (line 393-398) only skips `dispatch()`,
   not acceptance. The `VecDeque` grows without limit.

2. **Lapin `flume::unbounded()`** (`consumer.rs:160` in Lapin 4.10.0): Lapin's consumer delivery channel
   is hardcoded unbounded and not configurable. The IO loop always drains the socket and pushes to the
   unbounded channel. `send()` always succeeds.

### Chain trace (the OOM scenario)

```
RabbitMQ (no_ack=true, no prefetch limit)
  ↓ AMQP frames
OS TCP receive buffer (~256 KB, bounded)
  ↓ read()
Lapin IO loop (dedicated thread, always running)
  ↓ handle_frames() → flume::unbounded().send() (ALWAYS succeeds)
  ↓
Lapin Consumer unbounded channel (UNBOUNDED — no config option)
  ↓ Consumer::poll_next() → try_recv()
  ↓
spawn_source task (tokio)
  ↓ stream.next().await → gets delivery
  ↓ commands.send(Incoming).await (mpsc, 256 capacity)
  ↓
Actor — always accepts from mpsc → pushes to VecDeque (UNBOUNDED)
  ↓ over_budget: skip dispatch() (but still accepts!)
  ↓
flume::bounded() buffer to PHP (bounded)
```

When PHP stops pulling, the flume buffer fills, `dispatch()` can't push, but the actor keeps accepting
from the mpsc into the `VecDeque`. The mpsc is continuously drained, so `spawn_source` rarely blocks.
The unbounded Lapin channel is continuously drained by `spawn_source`. The IO loop keeps reading from
the socket. TCP backpressure never applies. The `VecDeque` grows until OOM.

### Design

#### C.1 — Hard gate on actor acceptance

The actor must **stop accepting** from the mpsc when `buffered_bytes > max_buffered_bytes`. Currently,
`over_budget` only skips `dispatch()`. Change it to also skip the `buffer.push_back()` and
`buffered_bytes` increment:

```rust
if over_budget {
    // Do NOT push to buffer, do NOT increment buffered_bytes
    // Leave the delivery in the mpsc — backpressure propagates to spawn_source
    // Record a metric for the backpressure event
    state.metrics.record_backpressure(&subscription);
    continue;  // or break to yield control
}
// Only push to buffer if under budget
if let Some(buffer) = state.buffers.get_mut(&subscription) {
    buffer.push_back(delivery);
    state.scheduler.mark_ready(&subscription);
}
```

When the actor stops draining the mpsc, `spawn_source` blocks on `commands.send().await`, which stops
it from calling `stream.next().await`, which stops draining the Lapin unbounded channel. The unbounded
channel grows (bounded by network rate), but the IO loop's `receive_buffer` eventually fills, and TCP
backpressure applies.

**Remaining gap**: the Lapin unbounded channel can still accumulate messages between the IO loop and
`spawn_source`. For small messages (256 bytes), 10k messages = ~2.5 MB — acceptable. For large messages
(1 MiB), 10k messages = 10 GB — OOM risk. Mitigation: document that `no_ack=true` is recommended only
for small messages or bounded queues. A full fix requires a bounded Lapin consumer channel (upstream
change or fork).

#### C.2 — Permit-based backpressure on `spawn_source`

As an additional layer, wrap the `DeliveryStream` with a permit system. `spawn_source` only reads from
the stream when it has a permit. The actor grants permits when `buffered_bytes < max_buffered_bytes`:

```rust
fn spawn_source(
    subscription: SubscriptionId,
    mut stream: Box<dyn DeliveryStream>,
    commands: mpsc::Sender<ConsumerCommand>,
    permits: Arc<Semaphore>,  // bounded permits
) {
    tokio::spawn(async move {
        loop {
            let _permit = permits.acquire().await;  // blocks until permit available
            if let Some(result) = stream.next().await {
                if commands.send(ConsumerCommand::Incoming { subscription: subscription.clone(), result }).await.is_err() {
                    return;
                }
            } else {
                return;
            }
        }
    });
}
```

The actor releases permits as deliveries are dispatched (not just accepted). This limits the total
in-flight deliveries between Lapin and the actor to `permits.capacity()`, bounding the unbounded Lapin
channel's growth.

### At-least-once

- The hard gate does not affect at-least-once: deliveries left in the mpsc are not lost — `spawn_source`
  blocks on `send().await` until the actor resumes accepting. If the connection drops, RabbitMQ redelivers.
- The permit system does not affect at-least-once: a permit is released only after the delivery is
  dispatched to the flume buffer (and thus reaches PHP). If the process crashes, the permit is released
  and RabbitMQ redelivers.

### Tests

- Hard gate: `buffered_bytes > max_buffered_bytes` → actor skips `buffer.push_back()` → mpsc fills →
  `spawn_source` blocks on `send().await`
- Permit system: `spawn_source` blocks on `permits.acquire()` when all permits are held → `stream.next()`
  not called → Lapin channel not drained
- Recovery: after `buffered_bytes` drops below budget (PHP drains flume), actor resumes accepting and
  grants permits
- `no_ack=true` mode: hard gate + permits together bound memory to `max_buffered_bytes + permits × avg_msg_size`

## Section 1 — Consume: Ack Fire-and-Forget with Error Queue

### Problem

`Delivery::ack()` (`delivery.rs:66-74`) calls `self.runtime.block_on(self.inner.ack())`, which sends
`ConsumerCommand::Settle` with a `oneshot::Sender`, then parks the PHP thread until the actor completes
`channel.ack(tag, false)` and responds via the oneshot. This is a full async round-trip per message, creating
a stop-and-wait pattern: pop → process → ack (block) → pop → process → ack (block) → ...

### Design

#### PHP-side (`Delivery::ack()`)

- `DeliveryToken::settle()` sends `ConsumerCommand::Settle` **without** a `oneshot` via `try_send` on the
  mpsc channel. Returns `Result<(), SettleError>`:
  - `Ok(())`: command accepted into the actor's command queue
  - `Err(SettleError::ChannelFull)`: channel full (256 capacity) — apply backpressure (see below)
  - `Err(SettleError::Closed)`: channel closed — consumer is shutting down
- No `block_on` — returns immediately after `try_send`
- `Delivery::ack()` (PHP) returns `void` (no return value). On `Err(Closed)`, throws
  `RabbitRsException`. On `Err(ChannelFull)`, applies bounded backpressure (see below).
  This preserves the current PHP API contract (ack returns void or throws).
- `Delivery::release()` and `Delivery::reject()` follow the same pattern.

#### Backpressure on `ChannelFull`

When `try_send` returns `ChannelFull`, the actor's command queue is saturated (256 capacity). Instead of
immediately throwing an exception (which would crash the worker under high throughput), apply a bounded
retry with backoff:

1. **Bounded spin-yield**: retry `try_send` up to N times (e.g., 64) with `std::thread::yield_now()`
   between attempts. This gives the actor's `select!` loop a chance to drain the queue.
2. **If still full after N retries**: fall back to a bounded `block_on` with a short timeout
   (e.g., 10ms) on `commands.send(...).await`. This parks the PHP thread briefly, letting the actor
   process pending commands.
3. **If the bounded `block_on` times out**: throw `RabbitRsException` with `ChannelFull` — the ack was
   not accepted. RabbitMQ will redeliver the message (at-least-once preserved).

This avoids the destructive exception under transient saturation while bounding the worst-case latency
to ~10ms. Under normal load, the command queue (256 capacity) should never fill — the actor drains
settlements continuously.

#### Error detection latency on empty queue

When the last message in a queue fails to ack and the queue becomes empty, the worker enters `sleep()`
(default 3s). The error would only surface at the next `pop()` — delaying reconnection by up to 3s.

**Fix: drain on sleep.** The Laravel `RabbitMqWorkCommandExtension` already listens to `WorkerIdle`
event (dispatched when no job is available). Register an additional listener that calls
`drainSettlementErrors()` on `WorkerIdle`:

```php
$events->listen(WorkerIdle::class, function (WorkerIdle $event) use ($queue) {
    $queue->drainSettlementErrors();
});
```

This ensures errors are drained even when the queue is empty and the worker is about to sleep. If the
error is a `ConnectionException`, `drainSettlementErrors()` throws it, and the worker's
`stopIfNecessary()` catches it via `stopWorkerIfLostConnection()`.

Additionally, `drainSettlementErrors()` is called at the top of `pop()` (every iteration), so errors
from the previous iteration's ack are always checked before the next `pop()`.

#### Actor-side (`actor.rs`)

- `ConsumerCommand::Settle` loses its `completed: oneshot::Sender<SettlementResult>` field
- The actor processes settlements as today: ordering per-channel via `settlement_in_flight`, queued in
  `settlement_queues` when a settlement is already in-flight
- On settlement completion (success or failure), the actor does **not** respond via oneshot. Instead:
  - **Success**: updates the ledger, decrements `in_flight`, dispatches more
  - **Failure**: sends a `SettlementError` to a `flume::bounded<SettlementError>(256)` channel. The actor
    holds the `flume::Sender`, `ConsumerHandle` holds the `flume::Receiver`. No `VecDeque` on `ActorState` —
    the flume channel is the buffer.

#### SettlementError struct

```rust
struct SettlementError {
    delivery_tag: u64,
    subscription: SubscriptionId,
    kind: SettlementErrorKind,  // StaleGeneration, Transport, AlreadySettled, Closed
    message: String,
    timestamp: Instant,
}
```

Bounded `flume::bounded(256)` channel between actor and `ConsumerHandle`. When full, the actor drops the
newest error and increments a `dropped_errors` metric counter (lossy — oldest errors preserved for
diagnostic continuity).

#### `ackThrough()` — same treatment

- `ConsumerCommand::SettleThrough` loses its `oneshot` field
- Becomes fire-and-forget. The actor validates the contiguous prefix and sends `basic.ack(tag, multiple=true)`
- Errors follow the same `flume` error channel

#### `release()` and `reject()` — same treatment

- `Delivery::release(delay)` and `Delivery::reject(requeue)` follow the same fire-and-forget pattern.
  `DeliveryToken::settle()` is already generic — it takes a `Settlement` enum (Ack, Release, Reject).
  All settlement variants become fire-and-forget via `try_send`, returning `Result<(), SettleError>`.
- `RabbitMqJob::release()` (`RabbitMqJob.php:66-80`) calls `delivery->release(ms)` — currently
  `block_on`. Becomes fire-and-forget. Errors surface at next `pop()` via `drainErrors()`.
- `RabbitMqJob::delete()` on failure path (via `Job::fail()`) calls `ack()` — already covered.

#### `ackBatch()` — same treatment

- `Consumer::ackBatch(array $deliveries)` (`consumer.rs:162-192`) currently calls `block_on` per
  delivery. Each `Delivery::ack()` becomes fire-and-forget, so `ackBatch` becomes a loop of
  `try_send` calls — no `block_on` at all.

#### `drainErrors()` — new API

Three layers:

1. **Rust core** (`consumer/set.rs`): `ConsumerHandle` holds a `flume::Receiver<SettlementError>`.
   The actor holds the `flume::Sender`. `drain_errors() -> Vec<SettlementError>` does a lock-free
   `try_recv` loop until empty. No `block_on`, no command round-trip to the actor.

2. **PHP extension** (`consumer.rs`): `Consumer::drainErrors(): array`. Calls
   `handle.drain_errors()` (lock-free `try_recv` loop). Returns an array of
   `{ delivery_tag, subscription, error_kind, message }`. No `block_on`.

3. **Laravel** (`RabbitMqQueue.php`): `drainSettlementErrors()` called at two points:
   - At the top of `pop()` (every iteration):
     ```php
     public function pop($queue = null, $index = 0) {
         $this->drainSettlementErrors();  // ← new
         // ... existing pop logic ...
     }
     ```
   - On `WorkerIdle` event (when queue is empty, before sleep):
     ```php
     $events->listen(WorkerIdle::class, function (WorkerIdle $event) use ($queue) {
         $queue->drainSettlementErrors();
     });
     ```
   - Iterates `$consumer->drainErrors()`
   - For `StaleGeneration` or `Transport` errors: throws `ConnectionException` so the worker's
     `getNextJob()` catch block (line 477-483) calls `stopWorkerIfLostConnection()` — same behavior as today
   - For other errors (AlreadySettled, Closed): logs via Laravel's error handler, does not throw

### At-least-once preservation

- If the process crashes before the actor writes the ack frame: RabbitMQ redelivers the message
- If the ack fails (connection drop): RabbitMQ redelivers after reconnection
- If `try_send` returns `ChannelFull` after bounded backpressure: throws `RabbitRsException`,
  message not acked, RabbitMQ redelivers
- If `try_send` returns `Closed`: throws `RabbitRsException`, consumer is shutting down
- Duplicates identified by `message_id` (already handled by Laravel)
- The one-iteration delay (error surfaces at next `pop()` or `WorkerIdle`) is acceptable: the message
  is redelivered regardless

### Type changes

- `ConsumerCommand::Settle`: remove `completed: oneshot::Sender<SettlementResult>`
- `ConsumerCommand::SettleThrough`: remove `completed: oneshot::Sender<SettlementResult>`
- `DeliveryToken::settle()`: `try_send` instead of `send().await` + no oneshot creation. Returns
  `Result<(), SettleError>` (`Ok(())` on accepted, `Err(ChannelFull)` or `Err(Closed)` on failure).
- `Delivery::ack()` / `release()` / `reject()` (PHP): no `block_on` on the fast path. Returns `void`.
  On `Err(ChannelFull)`: bounded spin-yield (64 retries) → bounded `block_on(10ms)` → throw if still full.
  On `Err(Closed)`: throws `RabbitRsException`. Same API contract as today — void on success, throws on
  failure.
- `Consumer::ackBatch()` (PHP): loop of fire-and-forget `ack()` calls, no `block_on` on the fast path.
- New: `flume::bounded<SettlementError>(256)` channel — actor sends, `ConsumerHandle` drains via `try_recv`
- New: `ConsumerHandle::drain_errors() -> Vec<SettlementError>` (lock-free `try_recv` loop)
- New: `Consumer::drainErrors()` PHP method
- New: `RabbitMqQueue::drainSettlementErrors()` called at top of `pop()` and on `WorkerIdle` event

### Tests

- Fire-and-forget ack: `try_send` returns `Ok(())`, no `block_on` on fast path, delivery state
  transitions to `Transitioning`
- Backpressure: `try_send` returns `ChannelFull` → spin-yield retries → bounded `block_on` → throw if
  still full after timeout
- Error queue: actor records failure on settlement error, `drain_errors()` returns it
- Bounded buffer: overflow drops newest, metrics capture count
- Laravel integration: `drainSettlementErrors()` in `pop()` throws `ConnectionException` on stale generation
- Laravel integration: `drainSettlementErrors()` on `WorkerIdle` catches errors before sleep
- At-least-once: settlement fails → message redelivered (mock transport test with paused time)

## Section 2 — Consume: `no_ack=true` in Transport

### Problem

`lapin.rs:246` hardcodes `no_ack: false`. In `early_ack` mode, the actor spawns a `tokio::spawn` per delivery
(`actor.rs:238`) to send an individual `basic.ack` frame. This is N ack frames + N spawned tasks for N messages,
while RabbitMQ can auto-ack internally with zero frames.

### Design

#### Transport (`transport.rs` + `lapin.rs`)

- Add `no_ack: bool` field to `ConsumerRequest` (currently absent — struct only has `queue`, `consumer_tag`,
  `exclusive`)
- `LapinConsumerChannel::consume()` propagates `request.no_ack` to `BasicConsumeOptions { no_ack: request.no_ack, ... }`

#### Consumer set (`set.rs`)

- Add `no_ack: bool` field to `Subscription` (default `false`), independent of `early_ack`.
- `ConsumerRequest` is built with `no_ack: subscription.no_ack`.
- Config mapping in Laravel:
  - `early_ack=true` + `no_ack` not set → `early_ack=true`, `no_ack=false` (current behavior, ack
    via spawned task, prefetch respected)
  - `early_ack=true` + `no_ack=true` → both active, zero ack frames, prefetch ignored by broker
  - `early_ack=false` + `no_ack=true` → invalid combination (no_ack without early_ack makes no
    sense — deliveries arrive as AutoAcked but PHP can't ack them). Reject at config validation.
  - `early_ack=false` + `no_ack=false` → manual ack (current default)
- This preserves the existing `early_ack` semantic while adding `no_ack` as an opt-in optimization.

#### Actor (`actor.rs`)

- When the subscription uses `no_ack=true`, the actor does **not** spawn an ack task. RabbitMQ auto-acks
  internally.
- `Delivery::new_auto_acked()` is used as today, but the ack is done by the broker, not by a spawned task
- `in_flight` is not incremented (same as current early_ack behavior)

### At-least-once

- `no_ack=true` means RabbitMQ marks the message as acked **before** sending it to the consumer. If the
  process crashes before processing, the message is **lost**.
- This is identical to the current `early_ack` behavior — the design eliminates the ack frame overhead, not
  the semantic.
- Gated behind `best_effort=true` in Laravel config (already enforced at `ConfigNormalizer.php:237-350`)

### Prefetch interaction

When `no_ack=true`, RabbitMQ ignores `basic.qos` prefetch — all messages are sent immediately
without flow control. This differs from the current `early_ack` mode where `no_ack=false` and the
actor acks individually while respecting prefetch.

Trade-off: `no_ack=true` eliminates ack frames and spawned tasks, but removes broker-side flow
control. The byte budget (`max_buffered_bytes = 64 MiB`) becomes the sole protection against
unbounded buffering. This is acceptable for the `best_effort` use case (already semantically
not at-least-once).

### OOM protection: socket read pause

When `no_ack=true`, the broker pushes messages without flow control. Without backpressure on the
consumer side, the OS socket buffers + Lapin internal channels + the flume buffer can accumulate
unbounded data, risking OOM.

**Required: backpressure at the transport boundary.** When the actor's `buffered_bytes` for a
subscription exceeds `max_buffered_bytes`, the actor must **stop reading** from the
`DeliveryStream`. The `spawn_source` task (`set.rs:205-224`) that pumps deliveries from the Lapin
stream into the actor's mpsc must respect a backpressure signal:

1. The actor tracks `buffered_bytes[subscription]` (already exists at `actor.rs:109`).
2. When `buffered_bytes[subscription] + delivery_bytes > max_buffered_bytes`, the actor skips
   `dispatch()` for that subscription (already exists at `actor.rs:393-409`).
3. **New**: The actor must also signal the `spawn_source` task to pause reading from the
   `DeliveryStream`. This can be a `tokio::sync::mpsc` with capacity 0 (rendezvous), a `Notify`,
   or a bounded channel that blocks the sender when full.
4. When `buffered_bytes` drops below `max_buffered_bytes` (after PHP drains the flume buffer), the
   actor signals the source task to resume reading.

This ensures that even without broker-side prefetch, the consumer's memory stays bounded by
`max_buffered_bytes`. The OS socket buffer absorbs a few messages (typically 128 KiB - 256 KiB),
but the TCP window closes when the application stops reading, causing RabbitMQ to back off.

### Type changes

- `ConsumerRequest`: add `no_ack: bool` field
- `Subscription`: add `no_ack: bool` field (default `false`), independent of `early_ack`
- `LapinConsumerChannel::consume()`: use `request.no_ack` instead of hardcoded `false`
- `set.rs`: set `no_ack` on `ConsumerRequest` from `subscription.no_ack`
- `actor.rs`: skip `tokio::spawn(ack)` when `no_ack` is active on the subscription
- `ConfigNormalizer.php`: validate `no_ack=true` requires `early_ack=true`; reject invalid combination
- `config/rabbit-rs.php`: add `no_ack` option per subscription (default `false`)

### Tests

- `no_ack=true` in `ConsumerRequest` propagates to `BasicConsumeOptions`
- Actor does not spawn ack task when `no_ack=true`
- Benchmark AUTO_ACK scenario uses `no_ack=true` (see Section 9)

## Section 3 — Consume: `Arc<Headers>` in TransportDelivery

### Problem

`actor.rs:232`: `let headers = Arc::new(delivery.headers.clone());` — `delivery.headers` is
`Headers` (= `BTreeMap<String, HeaderValue>`). This is a deep clone of the entire BTreeMap per delivery,
followed by an Arc allocation. It's the most expensive clone on the consume hot path.

### Design

#### Transport (`transport.rs`)

- Change `Delivery.headers: Headers` → `Delivery.headers: Arc<Headers>`
- `LapinDeliveryStream::next()` (`lapin.rs:273-306`) wraps headers in `Arc::new(map_headers(...))` at
  reception — one allocation at the transport boundary

#### Actor (`actor.rs`)

- `let headers = Arc::clone(&delivery.headers);` instead of `Arc::new(delivery.headers.clone())`
- `Delivery::new()` and `Delivery::new_auto_acked()` already take `Arc<Headers>` — no downstream changes

### Impact

- Eliminates deep BTreeMap clone + Arc allocation per delivery
- Replaced by `Arc::clone` (atomic refcount bump, ~2ns)

### Type changes

- `transport::Delivery.headers`: `Headers` → `Arc<Headers>`
- `LapinDeliveryStream::next()`: `Arc::new(map_headers(...))`
- `actor.rs:232`: `Arc::clone(&delivery.headers)`
- Mock transport: update `MockDelivery` / mock stream to produce `Arc<Headers>`

### Tests

- Transport delivery has `Arc<Headers>` type
- Actor uses `Arc::clone`, not `Arc::new(delivery.headers.clone())`
- Mock transport updated to match

## Section 4 — Benchmark: Prefetch Adaptatif (Global)

### Problem

`Config.php:30`: `PREFETCH_COUNT = 16` for all drivers. This limits `nextBatch(256)` to ~16-24 messages for
rabbit-rs (buffer = prefetch × 3 / 2 = 24). The benchmark cannot test large batch throughput.

### Design

- Raise `PREFETCH_COUNT` to 128 in `benchmarks/src/Config.php` for **all** drivers (rabbit-rs and amqplib)
- rabbit-rs buffer: `128 × 3 / 2 = 192` — `nextBatch(256)` can return meaningful batches
- amqplib: prefetch 128 — fair comparison, both drivers have the same broker-side window
- The byte budget (`max_buffered_bytes = 64 MiB`) protects against large messages for rabbit-rs

### Changes

- `benchmarks/src/Config.php`: `PREFETCH_COUNT = 128`

### Tests

- Benchmark runs with prefetch 128 for all drivers
- `nextBatch(256)` can return > 24 messages when buffer has content

## Section 5 — Publish: Cheap Clone via Arc-fields (Eliminate Double Clone)

### Problem

`client.rs:151` clones `PublishRequest` (because `publish_batch` iterates by reference), then
`actor.rs:702-749` re-clones all fields (exchange, routing_key, message_id, correlation_id, headers) into a
`TransportRequest`. Two rounds of string allocations × 256 per batch.

### Constraint: replay requires retained `PublishRequest`

The actor retains the **original** `publisher::PublishRequest` inside `RetainedPublish` across all states
(`publishing`, `ledger`, `replay`). On connection drop, `suspend()` drains all `RetainedPublish` into
`state.replay`. On recovery, `flush_replay()` re-derives a fresh `TransportRequest` from each retained
`PublishRequest` by calling `into_transport_request()` again. The `deadline: Instant` field (only on
`publisher::PublishRequest`) is used for replay expiry.

**Ownership transfer (by-value) would break replay**: `into_transport_request(retained.request)` would
move `request` out of `RetainedPublish`, making `state.publishing.insert(sequence, retained)` fail to
compile (partial move). Even with `Option::take()`, the actor would lose the original `PublishRequest`
and could not replay with the original deadline.

### Design

Make `PublishRequest` cheap to clone by wrapping heavy fields in `Arc`, so the existing by-reference
`into_transport_request()` and the `clone()` in `publish_batch()` become refcount bumps instead of
heap allocations.

#### `Destination` — `Arc<str>` fields

```rust
pub struct Destination {
    pub exchange: Arc<str>,       // was String
    pub routing_key: Arc<str>,   // was String
}
```

`Arc<str>` clone is a refcount bump (~2ns) vs `String` clone (heap alloc + memcpy). Construction from
`impl Into<String>` remains ergonomic via `Arc::from(s.into())`.

#### `MessageProperties` — `Arc<str>` for `message_id`

```rust
pub struct MessageProperties {
    pub message_id: Arc<str>,            // was String — always present, cloned per publish
    pub content_type: Option<Arc<str>>,   // was Option<String>
    pub correlation_id: Option<Arc<str>>, // was Option<String>
    pub delay_ms: Option<u64>,
    pub headers: PublishHeaders,          // already cheap if PublishHeaders uses Arc internally
}
```

#### `into_transport_request()` — stays by reference

```rust
fn into_transport_request(
    request: &PublishRequest,   // unchanged — by reference
    delay_strategy: Option<&DelayStrategy>,
    mandatory: bool,
) -> TransportRequest {
    TransportRequest {
        exchange: request.destination.exchange.clone(),        // Arc<str> → Arc<str> clone (cheap)
        routing_key: request.destination.routing_key.clone(),  // same
        payload: request.payload.clone(),                      // Bytes clone (Arc bump, already cheap)
        mandatory,
        properties: PublishProperties {
            content_type: request.properties.content_type.clone(),
            correlation_id: request.properties.correlation_id.clone(),
            message_id: Some(request.properties.message_id.clone()),
            delay_ms: request.properties.delay_ms,
            headers: request.properties.headers.clone(),
            persistent: true,
        },
    }
}
```

The `TransportRequest` fields stay `String` / `Option<String>` — Lapin needs owned strings. The
`Arc<str> → String` conversion at the transport boundary is a single `(*arc).to_string()` or
`arc.as_ref().into()`. Or, change `TransportRequest` to also use `Arc<str>` — but that couples the
transport to the publisher's types. Decision: keep `TransportRequest` as-is, convert `Arc<str>` → `String`
in `into_transport_request()`. The conversion is one allocation per field, but it happens once per
publish (not twice — the first clone in `publish_batch` is now cheap).

#### `publish_batch()` — `clone()` becomes cheap

`publisher.try_publish(request.clone())` at `client.rs:151` still clones, but now the clone is:
- `Destination`: 2 `Arc::clone` (refcount bumps)
- `MessageProperties`: 1 `Arc::clone` for `message_id` + 2 `Option<Arc<str>>` clones (refcount bumps or None)
- `Bytes`: 1 `Arc::clone` (already cheap)
- `PublishHeaders`: depends on implementation (already Arc-based or needs same treatment)

Total: ~5-7 refcount bumps instead of ~5-7 heap allocations + memcpys.

#### `PublishHeaders` — verify Arc usage

Check if `PublishHeaders` already uses `Arc` internally. If it contains `BTreeMap<String, HeaderValue>`,
apply the same `Arc` wrapping as `Headers` in Section 3.

### Impact

- **First clone** (`publish_batch`): ~5-7 refcount bumps instead of heap allocations
- **Second clone** (`into_transport_request`): `Arc<str> → String` conversion (one alloc per field, but
  only for fields that are actually `Some`). Net: still one allocation per string field per publish, but
  only once (at transport boundary), not twice.
- **Replay**: `RetainedPublish` retains the original `PublishRequest` with `Arc<str>` fields — cloning for
  replay is now cheap too (refcount bumps)
- **Net gain**: eliminates ~256 × 5-7 heap allocations per batch (the first clone round), keeps ~256 × 3-5
  allocations for the transport conversion (second round, unavoidable — Lapin needs owned strings)

### Type changes

- `Destination.exchange`: `String` → `Arc<str>`
- `Destination.routing_key`: `String` → `Arc<str>`
- `MessageProperties.message_id`: `String` → `Arc<str>`
- `MessageProperties.content_type`: `Option<String>` → `Option<Arc<str>>`
- `MessageProperties.correlation_id`: `Option<String>` → `Option<Arc<str>>`
- `PublishHeaders`: verify and apply Arc wrapping if needed
- `into_transport_request()`: `Arc<str>` → `String` conversion at transport boundary (unchanged signature)
- `publish_batch()`: `clone()` stays but is now cheap (refcount bumps)
- `TransportRequest`: unchanged (stays `String` / `Option<String>`)

### Tests

- `PublishRequest` clone is cheap (Arc refcount bumps, no heap allocations)
- `into_transport_request()` converts `Arc<str>` → `String` correctly
- Replay mechanism works: `RetainedPublish` retains `PublishRequest` with `Arc<str>` fields, replay
  re-derives `TransportRequest` correctly
- Publish correctness preserved (confirmations, returns, errors)

## Section 6 — Publish: Eliminate Double BoxFuture via TaggedFuture

### Problem

`PublisherChannel::publish()` is `#[async_trait]` (`transport.rs:403`), returning `Pin<Box<dyn Future>>`
(BoxFuture). The actor wraps this in its own `Box::pin(async move { ... })` at `actor.rs:623` to tag the
result with a sequence number. This is **two heap allocations per publish**: one from `async_trait`,
one from the actor's `Box::pin`. Additionally, `Arc::clone(&channel)` at `actor.rs:622` bumps the refcount
per publish.

### Design

#### `TaggedFuture` — zero-allocation wrapper

Instead of `Box::pin(async move { ... })`, create a struct that wraps the `async_trait` BoxFuture and tags
it with the sequence number:

```rust
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

- `Pin<Box<T>>` is `Unpin` → `TaggedFuture` contains only `Unpin` fields → `TaggedFuture` is `Unpin`
- `FuturesUnordered<TaggedFuture>` stores futures inline in its slab — **no additional heap allocation**
- `now_or_never()` works on `&mut TaggedFuture` (requires `Unpin` — satisfied)

#### Actor changes (`actor.rs:622-640`)

Current:
```rust
let channel_for_pub = Arc::clone(&channel);            // refcount bump
let mut publish_fut = Box::pin(async move {             // heap alloc #2
    let result = channel_for_pub.publish(request).await;
    (sequence, result)
});
match publish_fut.as_mut().now_or_never() { ... }
```

Proposed:
```rust
let publish_fut = channel.publish(request);             // BoxFuture from async_trait (alloc #1, inevitable)
let tagged = TaggedFuture { fut: publish_fut, sequence };
match Pin::new(&mut tagged).now_or_never() { ... }       // no alloc, inline in FuturesUnordered
```

- No `Arc::clone` — the actor's `channel` is used directly (the trait method takes `&self`)
- No `Box::pin` — `TaggedFuture` is `Unpin` and stored inline by `FuturesUnordered`
- The `async_trait` BoxFuture remains (alloc #1) — this is inevitable with `dyn PublisherChannel` in
  Rust stable. Eliminating it fully would require a generic actor (no `dyn`), which is too invasive.

### Impact

- **2 heap allocations → 1** per publish (eliminates the actor's `Box::pin`)
- **1 `Arc::clone` → 0** per publish (channel used by reference)
- Net: one fewer heap allocation + one fewer atomic refcount bump per publish

### Verification needed during implementation

Confirm that `channel` at `actor.rs:622` can be used by reference (the trait method `publish(&self, ...)`
takes `&self`). If the actor needs an owned `Arc` for the `FuturesUnordered` lifetime (e.g., the future
outlives the actor's borrow), an `Arc::clone` may still be needed — but the `TaggedFuture` still
eliminates the `Box::pin` allocation regardless.

### Type changes

- New: `TaggedFuture` struct in `publisher/actor.rs`
- `actor.rs:622-640`: replace `Box::pin(async move { ... })` with `TaggedFuture`
- `publish_in_flight`: `FuturesUnordered<TaggedFuture>` (or keep existing type if compatible)

### Tests

- `TaggedFuture` polls the inner future and tags the result with the sequence
- `now_or_never()` works on `TaggedFuture` (fast path)
- `FuturesUnordered<TaggedFuture>` drains correctly
- Publish correctness preserved (confirmations, returns, errors)


## Section 7 — Publish: Batch Wait Unifié

### Problem

`client.rs:163-172`: a `for` loop awaits each `waiter.wait()` sequentially. 256 await/wake cycles per batch.
amqplib uses `wait_for_pending_acks(5)` — a single call that blocks once for all pending confirms.

### Design

#### `PublishWaiter::wait_all()`

New method:
```rust
pub async fn wait_all(
    waiters: Vec<(usize, PublishWaiter)>
) -> Vec<(usize, Result<PublishOutcome, PublishError>)>
```

1. Build a `FuturesUnordered` from the waiters
2. `while let Some(result) = futures.next().await { ... }` — single collective drain
3. Return results in the original order (via the index tuple)

#### `publish_batch()` (`client.rs:163-172`)

Replace:
```rust
for (index, waiter) in waiters {
    match waiter.wait().await { ... }
}
```

With:
```rust
let results = PublishWaiter::wait_all(waiters).await;
for (index, result) in results { ... }
```

### Impact

- 256 sequential await/wake cycles → 1 collective await
- The runtime can coalesce wakeups (all confirms arrive in a burst)
- Note: AMQP confirmations arrive in order, so the total wait time is approximately the same as
  sequential polling. The gain is primarily in scheduling overhead: 256 await/wake cycles reduced
  to 1 collective drain, allowing the runtime to coalesce wakeups.

### Type changes

- New: `PublishWaiter::wait_all()` static method
- `publish_batch()` and `publish_batch_detailed()`: use `wait_all()`

### Tests

- `wait_all()` returns results in original order
- All waiters resolve (success and failure cases)
- Confirmations arriving out of order are handled

## Section 8 — Publish: Multi-Channel (Deferred)

Not in scope for this plan. Single publisher actor, single channel. The bottleneck after sections 5-7
is the socket, not the actor. Multi-channel parallelism is a future optimization if needed.

## Section 9 — Benchmark: AUTO_ACK Fairness + SKIP Investigation

### 9.1 AUTO_ACK Fairness

#### Problem

- `RabbitRsDriver.php:60`: AUTO_ACK sets `confirms=true` (publisher confirms) + `early_ack=true` (spawn ack/delivery)
- `AmqplibDriver.php:89-98`: AUTO_ACK uses fire-and-forget publish (no `confirm_select`, no
  `wait_for_pending_acks`) + `no_ack=true` (broker auto-ack, zero ack frames)
- rabbit-rs does safe-delivery (confirms + individual acks) while amqplib does fire-and-forget + no_ack

#### Fix

- `RabbitRsDriver.php` AUTO_ACK: `confirms=false` (fire-and-forget publish) + `early_ack=true` with
  `no_ack=true` at the transport level (Section 2)
- Both drivers now do the same work in AUTO_ACK: fire-and-forget publish + broker-side auto-ack

#### Changes

- `RabbitRsDriver.php`: AUTO_ACK scenario sets `confirms=false` and uses `no_ack=true` via early_ack

### 9.2 Benchmark SKIP Investigation

#### Symptom

```
=== auto-ack / rabbit-rs ===
  SKIP: publish deadline expired during connection recovery
```

#### Root cause

`publisher/actor.rs:381-401` — `expire_replay()`: when the publisher is in `Suspended` phase (connection
dropped), pending publishes are queued in `replay` with a deadline of `confirm_timeout` (30s in benchmark).
If recovery takes > 30s, `expire_replay()` fails them with `PublishErrorKind::Timeout`. The benchmark
catches the exception and prints SKIP.

#### Investigation tasks

1. Check if the RabbitMQ broker is ready before the benchmark starts (health check / `rabbitmqctl await_startup`)
2. Check connection pool behavior — is a connection recycled mid-benchmark?
3. Check recovery timing — how long does `recovery_coordinator.rs` take to reconnect + restore topology?
4. If recovery is legitimately slow (> 30s for local broker = abnormal), increase `confirm_timeout` in the
   benchmark config
5. If the connection drops due to resource limits (file descriptors, heartbeat timeout), fix the environment

#### Possible fixes

- **Environment**: ensure broker is ready before benchmark (`rabbitmqctl await_startup` or health check)
- **Timeout**: increase `confirm_timeout` from 30s to 60s in benchmark config
- **Connection**: investigate pool recycling, heartbeat, file descriptor limits

### Tests

- AUTO_ACK scenario: rabbit-rs uses `confirms=false` + `no_ack=true`, matching amqplib
- Benchmark runs without SKIP when broker is healthy
- `confirm_timeout` is configurable and documented

### 9.3 `basic_consume` Fairness — BunnyDriver + AmqpExtDriver Migration

#### Problem

`BunnyDriver.php:137` and `AmqpExtDriver.php:142` use `basic.get` (poll) to consume messages, while
`AmqplibDriver.php:164` and `RabbitRsDriver.php:136` use `basic_consume` (push). `basic.get` sends a
request-response round-trip per message; `basic_consume` has the broker push messages continuously. This
makes the benchmark results incomparable across drivers.

Additionally, `LaravelCompareBenchmark.php:146` has `' prefetch_count'` (leading space) instead of
`'prefetch_count'`, so the prefetch setting is silently ignored for the php-amqplib driver.

#### Fix

1. Migrate `BunnyDriver` from `basic.get` to `basic_consume` with a callback + `wait()` loop, following
   the `AmqplibDriver` pattern
2. Migrate `AmqpExtDriver` from `basic.get` to `AMQPQueue::consume()` with a callback, using the amqp
   extension's native consume API
3. Fix `' prefetch_count'` → `'prefetch_count'` in `LaravelCompareBenchmark.php:146`

#### Tests

- All four drivers use `basic_consume` (push) for consumption
- `LaravelCompareBenchmark` php-amqplib config has `'prefetch_count'` (no leading space)

## Architecture: How It Fits Together

### Consume Pipeline (After)

```
RabbitMQ Broker
    ↓ (AMQP 0-9-1, no_ack=true if subscription.no_ack)
Lapin Consumer (LapinDeliveryStream)
    ↓ DeliveryStream::next() → Arc<Headers> (no clone)
spawn_source (tokio task per subscription)
    ↓ ConsumerCommand::Incoming
Consumer Actor (run_actor)
    ↓ dispatch() → no ack spawn if no_ack=true
    ↓ try_send into flume buffer
flume::bounded (buffer_rx)
    ↓ try_recv (lock-free, no block_on)
Consumer::tryNext() / nextBatch() → PHP
    ↓
Laravel pop() → drainSettlementErrors() → consumer->next()
    ↓
Job processed → Delivery::ack() → try_send (fire-and-forget, no block_on)
    ↓
Actor processes settlement async → records error in settlement_errors if failure
    ↓
Next pop() → drainSettlementErrors() surfaces the error → ConnectionException if stale
```

### Publish Pipeline (After)

```
PHP publish_batch()
    ↓ clone PublishRequest (Arc<str> fields — refcount bumps, cheap)
PublisherActor Command::Publish
    ↓ into_transport_request(&retained.request) — by reference (replay-safe), Arc<str>→String at boundary
    ↓ channel.publish(request) → BoxFuture (async_trait)
    ↓ TaggedFuture — inline in FuturesUnordered (no extra Box::pin)
    ↓ now_or_never() fast path or FuturesUnordered
    ↓ confirmations resolve
PublishWaiter::wait_all() — single collective await
    ↓ results in original order
PHP
```

### Laravel Worker Loop (After)

```
while (true) {
    pop():
        drainSettlementErrors()  ← surfaces async ack errors from previous iteration
        consumer->next(blockForMs)  ← lock-free try_recv, block_on only if empty
    if job:
        fire() → handle() → delete()
            Delivery::ack() → try_send (fire-and-forget)
            on ChannelFull: bounded spin-yield → bounded block_on(10ms) → throw if still full
    else:
        WorkerIdle event → drainSettlementErrors()  ← catch errors before sleep
        sleep(sleep seconds)
    stopIfNecessary()
}
```

## Implementation Order

1. **Section A** (recovery bugs) — fix recovery chain first, root cause of benchmark SKIP
2. **Section B** (consumer bugs) — fix try_next_batch + ledger before perf changes interact with them
3. **Section 3** (Arc<Headers>) — isolated, lowest risk, immediate consume hot path improvement
4. **Section C** (OOM hard gate + permits) — must precede `no_ack` activation
5. **Section 2** (no_ack transport) — isolated transport change, enables benchmark fairness
6. **Section 1** (ack fire-and-forget + error queue) — largest consume change, needs careful testing
7. **Section 5** (publish Arc<str> fields) — isolated publish optimization
8. **Section 7** (batch wait) — publish optimization, depends on section 5 for clean testing
9. **Section 6** (TaggedFuture — eliminate double BoxFuture) — publish optimization
10. **Section 9.1** (benchmark AUTO_ACK) — depends on sections 1-2 being implemented
11. **Section 9.2** (benchmark SKIP) — likely fixed by Section A, investigate if persists
12. **Section 4** (prefetch global) — trivial config change, last

## Invariants Preserved

- **At-least-once**: fire-and-forget ack preserves redelivery on failure. `no_ack` gated behind `best_effort`.
- **`#![forbid(unsafe_code)]`**: no changes to lint configuration.
- **No PHP callbacks from Rust threads**: polling model preserved. `drainErrors()` is pull-based.
- **Bounded memory**: settlement_errors buffer bounded (256). Byte budget unchanged. Flume buffer bounded.
- **Recovery order**: connection, channels, exchanges, queues, bindings, QoS, consumers — unchanged.
- **Connection generation awareness**: stale settlements rejected — unchanged (errors go to queue instead of
  oneshot).
- **No credentials in Debug/errors/metrics**: `SettlementError` contains no credentials.

## Out of Scope

- Multi-channel publish (Section 8) — deferred
- Batch pop() for Laravel — pop() stays one-at-a-time
- Callback/push model from Rust to PHP — polling model preserved
- Publisher confirms elimination — confirms stay for safe mode, only disabled in AUTO_ACK benchmark
- Zero-copy pipeline end-to-end — payloads transit as `Bytes` (Arc'd) but not fully zero-copy
- TLS `server_name`/`verify` wiring — separate bug, not in this scope
- `Config::validate()` duplicate/zero-budget rejection — separate bug, not in this scope
- TTL queue cache staleness after expiration — separate bug, not in this scope
- `queue_size`/`purge_queue` double connection — separate bug, not in this scope
- Bounded Lapin consumer channel — requires upstream Lapin change or fork; mitigated by hard gate + permits
- `ConsumerHandle` Drop semantics (close on last drop) — included in Section B but may require careful
  migration if existing code relies on close-on-first-drop behavior
- Zero-copy pipeline end-to-end — payloads transit as `Bytes` (Arc'd) but not fully zero-copy
