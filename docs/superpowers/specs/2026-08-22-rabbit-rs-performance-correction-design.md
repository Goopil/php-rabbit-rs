# Rabbit-rs Performance Correction Plan

**Date:** 2026-08-22
**Status:** Needs revision
**Scope:** Bugs + API gaps + Performance optimizations + Stability

## Context

Benchmark results revealed rabbit-rs is significantly slower than amqplib:

| Metric | rabbit-rs | amqplib | Ratio |
|--------|-----------|---------|-------|
| Publish msg/s | 19k | 78k | 4.1x slower |
| Consume msg/s | 16k | 39k | 2.4x slower |
| Consume p99 | 646ms | 254ms | 2.5x higher |

Root causes identified through source analysis:

1. **`no_ack` mode hardcoded to `false`** — `LapinConsumerChannel::consume()` in `transport/lapin.rs:238` hardcodes `no_ack: false`. No configuration path exists to enable it. The benchmark auto-ack scenario acks every message manually, adding 1 FFI call + 1 network round-trip per delivery.

2. **No batch acknowledgement** — `Delivery::ack()` in `consumer/actor.rs:321` always passes `multiple=false`. Each delivery requires a separate `ConsumerCommand::Settle` → actor → `channel.ack()` → response round-trip via `block_on`.

3. **Sequential `basic_publish` in `publish_queue()`** — `publisher/actor.rs:519-588` awaits each `channel.publish()` before sending the next. Confirmations run concurrently via `FuturesUnordered`, but the write side is serialized.

4. **Publisher handle lookup per message** — `client.rs:131` calls `self.publisher(&broker).await` for each message in a batch — 256 mutex + hashmap lookups for a 256-message batch to the same broker.

5. **3 FFI boundary crossings per delivery** — `tryNext()` + `payload()` + `ack()`. For 10k messages, that's 30k FFI calls.

6. **Benchmark scenario mismatch** — `RabbitRsDriver.php:28-53` does not configure `publisher.confirms` per scenario. `RabbitRsDriver.php:117-140` always acks each message individually. Prefetch is 500 for rabbit-rs but 16 for php-amqplib. The publish p99 measured is actually a consume latency (all messages published before any consumed). Message ID uniqueness is not verified, so losses masked by duplicates are invisible.

7. **Consumer actor blocks on settlement** — `consumer/actor.rs:296-350` runs `channel.ack().await` and `delayed_release().await` (which calls `publisher.publish().await`) directly in the `select!` loop. A slow ACK or a delayed release blocks all subscriptions, recovery, and `close()`.

8. **Memory bounded by message count, not bytes** — `buffer_capacity` (1024) bounds the number of in-flight publishes but 1024 × 1 MiB payloads = ~1 GiB. The `total_prefetch` sum in `consumer/set.rs:155` uses `u16` which can overflow.

## Decisions

| Decision | Choice |
|----------|--------|
| Scope | All (bugs + gaps + performance + stability) |
| Consume callback | Preserve constraint: no PHP callbacks from Rust threads. Polling only. |
| Best-effort mode | **Early-ACK** as primary best-effort (preserves broker QoS). True `no_ack` experimental only. |
| Best-effort classification | Not at-least-once. Forbidden by default in the Laravel driver. |
| Batch ack API | `Consumer::ackThrough(Delivery $delivery)` (simple, single channel) + `Consumer::ackBatch(array $deliveries)` (advanced, multi-channel) |
| Batch consume API | `Consumer::nextBatch(int $max, int $timeoutMs): array` — wait once then drain |
| Compatibility | Retro-compatible — new fields/methods are optional with current behavior as default |
| Pipeline replay | External `publishing` registry — futures carry sequence only, not `RetainedPublish` |
| Barrier | **No barrier.** Batch is fully enqueued before awaiting waiters; FIFO ordering is implicit. |
| Batch failure | Indexed report per message (`confirmed`, `returned`, `failed`, `not_accepted`) |
| Memory bounds | Byte budgets per subscription and global, `u64` sums with checked arithmetic |
| Organization | 8-step ordered implementation (benchmark first, early-ACK last) |

## Design

### Track 1: Config (prerequisite)

**`config.rs`:**
- Add `no_ack: bool` field to `SubscriptionConfig` (default: `false`)
- Add `early_ack: bool` field to `SubscriptionConfig` (default: `false`)
- Add `max_message_bytes: Option<u64>` to `SubscriptionConfig` (default: `None` = unlimited per-message, but bounded by global budget)
- Add `max_buffered_bytes: u64` to `PublisherConfig` (default: 64 MiB)
- Validation: if `no_ack=true`, log a warning that this mode is best-effort and not at-least-once, and that `early_ack` is recommended instead. The `prefetch` field is still used for buffer sizing in `consumer/set.rs:155-158`.
- Validation: if `early_ack=true`, log that this mode is best-effort (message ACKed before PHP processes it).

**`consumer/set.rs`:**
- Fix `total_prefetch` overflow: change from `u16` sum to `u64` with `checked_sum` or `saturating_sum`
  ```rust
  let total_prefetch: u64 = subscriptions
      .iter()
      .map(|s| s.prefetch as u64)
      .sum();
  ```

**`pool/recovery_coordinator.rs`:**
- Pass `no_ack` and `early_ack` from `SubscriptionConfig` into `ConsumerRequest` when opening consumers during recovery

### Track 2: Consume

#### 2a: Consumer actor unblocking (stability fix, no API change)

> **Problem:** `consumer/actor.rs:296-350` runs `settle()` directly in the `select!` loop. `channel.ack().await` blocks on network I/O. `delayed_release()` calls `publisher.publish().await` which can take seconds. While a settlement is in progress, the actor cannot process `Incoming`, `Close`, `UpdateGeneration`, or dispatch new deliveries.

**`consumer/actor.rs`:**

**Settlement lanes:**

Move settlement execution out of the main `select!` loop. Each settlement runs in a spawned task. The main loop processes the result when the task completes.

```rust
// New: track pending settlements
struct ActorState {
    // ... existing fields ...
    pending_settlements: FuturesUnordered<Pin<Box<dyn Future<Output = SettlementResult>>>>,
}

struct SettlementResult {
    token: Arc<DeliveryTokenInner>,
    result: Result<DeliveryState, ConsumerError>,
}

// In the select! loop, the Settle arm spawns instead of awaiting:
Some(ConsumerCommand::Settle { token, settlement, completed }) => {
    let channel = state.subscriptions.get(&token.subscription)
        .map(|r| r.channel.clone());
    let connection_key = ...;
    let generation = ...;
    let channel_id = ...;
    let delivery_tag = token.delivery_tag;

    state.pending_settlements.push(Box::pin(async move {
        // Validate identity (same checks as current settle())
        // Execute settlement (ack/reject/release)
        SettlementResult { token, result }
    }));
    // Store completed sender alongside the token for later resolution
}

// New select! arm:
Some(settlement_result) = state.pending_settlements.next(),
    if !state.pending_settlements.is_empty() => {
    // Release budget, record metrics, dispatch, resolve oneshot
    state.release_budget();
    state.dispatch();
    // Resolve the completed oneshot sender
}
```

Bounded by `max_in_flight` — the number of concurrent settlement lanes is naturally limited by the in-flight budget.

**Event-driven dispatch:**

Remove the 1ms dispatch timer (`actor.rs:203`). Dispatch on:
- `Incoming` — a new delivery arrived, try to dispatch
- Settlement completed — budget freed, try to dispatch
- `dispatch_notify` — pump signaled more deliveries available

```rust
loop {
    tokio::select! {
        // ... command arms ...
        Some(ConsumerCommand::Incoming { .. }) => {
            // Push to buffer, mark ready
            state.dispatch();  // immediate dispatch attempt
        }
        Some(settlement_result) = state.pending_settlements.next(), ... => {
            // Release budget, dispatch
            state.dispatch();
        }
        () = dispatch_notify.notified() => {
            state.dispatch();
        }
        // NO dispatch_timer.tick() arm
    }
}
```

**Reduce clones in `dispatch()`:**

Current `dispatch()` (`actor.rs:117-121`) clones `TransportDelivery` from the buffer front, then clones headers again at line 141. Replace with `pop_front()` and move ownership:

```rust
// Before: .front().cloned() — clones the delivery
// After:
let Some(delivery) = self.buffers.get_mut(&subscription).and_then(VecDeque::pop_front) else {
    self.scheduler.mark_empty(&subscription);
    return;
};
// Move delivery fields into Delivery::new() — no clone
```

This eliminates one `TransportDelivery::clone()` and one `headers.clone()` per dispatched delivery.

#### 2b: Early-ACK best-effort mode

> **Classification: best-effort.** The message is ACKed to RabbitMQ before PHP processes it. If PHP crashes, the message is lost. This mode preserves broker QoS (prefetch still works), unlike true `no_ack`.

**`consumer/actor.rs`:**

When `early_ack=true`:
- On `Incoming`, immediately ACK the delivery via the channel: `channel.ack(delivery_tag, false).await` — but in a settlement lane, not the main loop
- Do not create a `DeliveryToken` (no settlement tracking needed — already acked)
- Do not increment `in_flight` (no budget tracking — already acked)
- Create `Delivery` with settlement state `AutoAcked`
- Dispatch to the flume buffer immediately after the ACK completes
- The delivery still carries `delivery_tag` for user-facing API consistency

**`delivery.rs` (PHP):**
- `Delivery::ack()`, `Delivery::release()`, `Delivery::reject()` on an `AutoAcked` delivery: raise an `InvalidArgumentException` explicitly — do not silently no-op
- `Delivery::deliveryTag(): int` accessor for all modes

**`consumer.rs` (PHP):**
- `Consumer` stores `early_ack` and `no_ack` per subscription
- Pass the settlement state to each `Delivery` object

**True `no_ack` mode (experimental):**

When `no_ack=true` (not `early_ack`):
- Pass `no_ack: true` to `BasicConsumeOptions` — broker auto-acks, prefetch is ignored
- Same delivery behavior as `early_ack` (no token, `AutoAcked` state)
- **Additional bound:** the `VecDeque` buffer per subscription must be capped at `max(total_prefetch, 256)` entries. When full, stop polling that subscription's stream until the buffer drains. This is the only flow control available since the broker will not block.
- Documented as experimental — only use if benchmarks prove it is necessary and the memory bounds are sufficient

**Laravel driver guard:**
- `packages/laravel-queue/` rejects `no_ack=true` and `early_ack=true` in the default reliable configuration
- Only allow when the user explicitly sets a best-effort opt-in

#### 2c: Batch consume and ack APIs

**`consumer.rs` (PHP):**

Add `Consumer::nextBatch(int $max, int $timeoutMs): array`:
- Wait for the first delivery with `recv_timeout(timeoutMs)` — blocks once
- Drain remaining buffer with `try_recv()` up to `$max` total deliveries — non-blocking
- Single FFI call for N deliveries
- `$max` clamped to `1..=256`
- Returns empty array if timeout expires with no deliveries
- Existing `tryNext()`, `next()` remain unchanged

Add `Consumer::ackThrough(Delivery $delivery): void`:
- ACK all messages up to and including `$delivery` on its originating channel, using `multiple=true`
- The `Delivery` carries its `DeliveryToken` with full identity: `(subscription_id, connection_key, generation, channel_id, delivery_tag)`
- Single `block_on` for the entire batch

Add `Consumer::ackBatch(array $deliveries): void`:
- For multi-channel `ConsumerSet` scenarios
- Group deliveries by `(subscription_id, channel_id, generation)`
- For each group, validate the contiguous prefix and send one `channel.ack(highest_tag, multiple=true)`
- Reject if any delivery is already terminal
- Reject if the prefix is non-contiguous (gap in delivery tags)
- The core renders all affected tokens terminal after the ACK

**`consumer/actor.rs`:**

Add `ConsumerCommand::SettleThrough { token: Arc<DeliveryTokenInner>, completed: oneshot::Sender<Result<DeliveryState, ConsumerError>> }`:
- Actor validates full identity from the token (connection, generation, channel, tag)
- Checks the per-channel delivery ledger for a contiguous prefix up to `delivery_tag`
- Calls `channel.ack(delivery_tag, multiple=true).await` in a settlement lane
- Releases budget for all affected deliveries (count from ledger)
- Renders all affected tokens terminal

Add `ConsumerCommand::SettleBatch { tokens: Vec<Arc<DeliveryTokenInner>>, completed: oneshot::Sender<Result<Vec<DeliveryState>, ConsumerError>> }`:
- Group tokens by `(subscription_id, channel_id, generation)`
- Validate each group independently
- Execute one ACK per group
- Return per-token results

**`transport.rs`:**
- `ConsumerChannel::ack()` already accepts `multiple: bool` — no change needed

**`delivery.rs` (PHP):**
- Add `Delivery::deliveryTag(): int` — returns the AMQP delivery tag

### Track 3: Publish

#### 3a: Pipeline basic_publish with external `publishing` registry

**`publisher/actor.rs`:**
- Modify `publish_queue()` to fire `channel.publish()` without awaiting between messages
- Lapin's `BasicPublish` returns a `PublisherConfirm` handle without blocking — AMQP allows pipelining on a channel
- The `buffer_capacity` semaphore (1024) already limits in-flight publishes — backpressure is preserved

**Core design — external `publishing` registry:**

Keep `RetainedPublish` values in an explicit registry `publishing: HashMap<u64, RetainedPublish>` owned by `ActorState`. The futures pushed to `FuturesUnordered` carry only their `sequence: u64` and the `Result`. On any failure path, the registry is moved to `replay` directly — futures are abandoned.

```rust
// In ActorState:
publishing: HashMap<u64, RetainedPublish>,   // sequence → retained (owned by actor)
publish_in_flight: FuturesUnordered<PublishFuture>,  // futures carry sequence only

type PublishFuture = Pin<Box<dyn Future<Output = (u64, Result<PublisherConfirm, TransportError>)>>>;
```

In `publish_queue()`:
```rust
while let Some(retained) = pending.pop_front() {
    // 1. Deadline check (unchanged)
    // 2. Channel availability check (unchanged)
    // 3. Delay topology (unchanged — awaited, cold path)
    // 4. Sequence assignment (unchanged)

    state.sequence = state.sequence.saturating_add(1);
    let sequence = state.sequence;
    let generation = state.generation;

    // 5. Register in publishing BEFORE launching the future
    state.publishing.insert(sequence, retained);

    // 6. Push publish future — carries sequence only, NOT the retained
    state.publish_in_flight.push(Box::pin(async move {
        let result = channel.publish(request).await;
        (sequence, result)
    }));

    // 7. Non-blocking drain — complete any ready futures immediately
    while let Some((seq, result)) = state.publish_in_flight.next().now_or_never().flatten() {
        handle_publish_completion(state, seq, result);
    }
}
```

The `select!` loop arm:
```rust
Some((sequence, result)) = state.publish_in_flight.next(),
    if !state.publish_in_flight.is_empty() => {
    handle_publish_completion(state, sequence, result);
}
```

Shared completion handler:
```rust
fn handle_publish_completion(
    state: &mut ActorState,
    sequence: u64,
    result: Result<PublisherConfirm, TransportError>,
) {
    let Some(retained) = state.publishing.remove(&sequence) else {
        return; // Already recovered during a previous error
    };

    match result {
        Ok(receipt) => {
            if state.config.confirms {
                state.ledger.insert(sequence, InFlightPublish { retained, generation });
                let deadline = retained.request.deadline
                    .min(time::Instant::now() + state.config.confirm_timeout);
                state.confirmations.push(Box::pin(async move {
                    let result = match time::timeout_at(deadline, receipt.wait()).await {
                        Ok(result) => ConfirmationResult::Completed(result),
                        Err(_) => ConfirmationResult::TimedOut,
                    };
                    (sequence, generation, result)
                }));
            } else {
                // Best-effort: resolve waiter immediately
                // Guarantee: "transport accepted the frame" — NOT at-least-once
                let _ = retained.completion.send(Ok(PublishOutcome::Published));
            }
        }
        Err(error) if error.is_recoverable() => {
            state.replay.push_back(retained);
            state.publish_in_flight.clear();
            // Global sort: merge publishing registry + this element, sort by sequence
            let mut all: Vec<RetainedPublish> = state.publishing.drain()
                .map(|(_, r)| r)
                .collect();
            // The faulty element is already in replay — merge and sort everything
            let mut combined = std::mem::take(&mut state.replay)
                .into_iter()
                .chain(all)
                .collect::<Vec<_>>();
            combined.sort_by_key(|r| r.sequence);
            state.replay = combined.into_iter().collect();
            state.suspend(generation);
        }
        Err(error) => {
            let _ = retained.completion.send(Err(transport_publish_error(&error)));
        }
    }
}
```

**Key invariants of the `publishing` registry:**

1. **Insert before launch:** `publishing.insert(sequence, retained)` happens before the future is pushed.
2. **Remove on completion:** `publishing.remove(&sequence)` happens in `handle_publish_completion` on success/terminal-error.
3. **Drain on recoverable error:** `publishing.drain()` recovers ALL retains. Futures are abandoned (`publish_in_flight.clear()`). The faulty element and registry contents are merged and sorted globally by `sequence` for deterministic replay.
4. **Bounded by semaphore:** the `buffer_capacity` semaphore limits entries in `publishing`. Each `RetainedPublish` holds a `_permit` released on terminal resolution.

**Recovery, close, and permanent failure must also drain `publishing`:**

- On `ConnectionEvent::Recovering`: merge `publishing` + `replay` + `ledger` (entries not yet confirmed), sort globally by sequence, clear `publish_in_flight`.
- On `ConnectionEvent::FailedPermanent`: drain `publishing` and fail all waiters with the permanent error.
- On `Command::Close`: drain `publishing` and fail all waiters with `Closed`.
- On deadline expiry: check `publishing` for expired entries and fail their waiters.

**`RetainedPublish` must carry `sequence`:**

Add a `sequence: u64` field to `RetainedPublish`, set at the moment `state.sequence` is assigned.

**AMQP frame ordering:**

`FuturesUnordered` does not guarantee completion order, but AMQP frame send order is determined by the order `channel.publish()` is called. Since `publish_queue()` calls them sequentially without `await` between publishes, the frame order on the wire is preserved. `FuturesUnordered` only affects when we observe the completion, not the send order.

#### 3b: Cache publisher handle with result ordering

**`client.rs`:**
- In `publish_batch()`, group messages by broker first, then acquire publisher handle once per broker
- Preserve input order in results: track original positions so `outcomes` is returned in the same order as `requests`

```rust
let mut by_broker: HashMap<&str, Vec<(usize, PublishRequest)>> = HashMap::new();
for (i, (broker, request)) in requests.into_iter().enumerate() {
    by_broker.entry(broker).or_default().push((i, request));
}

let mut outcomes: Vec<Option<Result<PublishOutcome, ClientError>>> = vec![None; total_count];
let mut waiters: Vec<(usize, PublishWaiter)> = Vec::new();

for (broker, msgs) in by_broker {
    let publisher = self.publisher(broker).await?;
    for (original_index, msg) in msgs {
        match publisher.try_publish(msg) {
            Ok(waiter) => waiters.push((original_index, waiter)),
            Err(error) => {
                outcomes[original_index] = Some(Err(ClientError::publish(&error)));
            }
        }
    }
}

for (index, waiter) in waiters {
    match waiter.wait().await {
        Ok(outcome) => outcomes[index] = Some(Ok(outcome)),
        Err(error) => outcomes[index] = Some(Err(ClientError::publish(&error))),
    }
}
```

- Single-message `publish()` already does 1 lookup — no change needed.

#### 3c: Partial batch failure reporting

> **No barrier needed.** The batch is fully enqueued via `try_publish()` before awaiting any waiters. Waiters resolve in the background via the actor. Awaiting the last waiter guarantees all prior waiters have been enqueued. The barrier from the previous spec revision is unnecessary and has been removed.

**`client.rs`:**

Replace the current "first terminal error wins" semantics (`client.rs:150-161`) with an indexed report:

```rust
pub struct BatchOutcome {
    pub results: Vec<MessageOutcome>,
}

pub enum MessageOutcome {
    Confirmed(PublishOutcome),   // broker confirmed via publisher confirm
    Returned(ReturnInfo),        // basic.return — message was unroutable (mandatory)
    Failed(PublishError),         // terminal error (timeout, channel closed, etc.)
    NotAccepted(PublishError),    // never enqueued (backpressure, unknown broker)
}

pub async fn publish_batch(&self, requests: Vec<(String, PublishRequest)>) -> Result<BatchOutcome, ClientError> {
    // ... grouping and enqueuing (Track 3b) ...
    // ... awaiting waiters ...

    let results = outcomes.into_iter().map(|o| match o {
        Some(Ok(outcome)) => MessageOutcome::Confirmed(outcome),
        Some(Err(error)) => MessageOutcome::Failed(error.into()),
        None => MessageOutcome::NotAccepted(/* ... */),
    }).collect();

    Ok(BatchOutcome { results })
}
```

- The caller inspects `results[i]` for each message at index `i` in the input
- A `Returned` result means `basic.return` took precedence over the ACK (preserved invariant)
- Backward compatibility: a wrapper method `publish_batch_simple()` retains the old `Result<Vec<PublishOutcome>, ClientError>` semantics for callers that don't want per-message detail

### Track 4: Memory budgets and operational stability

#### 4a: Byte budgets

**`consumer/actor.rs`:**
- Track `buffered_bytes: u64` per subscription and `total_buffered_bytes: u64` global
- When a delivery arrives, add `delivery.payload.len()` to the subscription and global counters
- When a delivery is dispatched (popped from buffer), subtract its size
- If `total_buffered_bytes` exceeds `max_buffered_bytes` (from `SubscriptionConfig` or global config), stop polling streams until bytes drain
- This bounds memory regardless of payload size

**`publisher/actor.rs`:**
- Track `publishing_bytes: u64` — sum of payload sizes in the `publishing` registry + `ledger` + `replay`
- Add to `publishing` when inserting: `publishing_bytes += retained.request.payload.len()`
- Subtract when removing (completion, error, drain)
- If `publishing_bytes` exceeds `max_buffered_bytes` (from `PublisherConfig`, default 64 MiB), apply backpressure: stop accepting new `Command::Publish` until bytes drain
- The semaphore (message count) and byte budget work together — either can trigger backpressure

**`consumer/set.rs`:**
- Fix `total_prefetch` overflow (Track 1): use `u64` with `saturating_sum`
- Buffer sizing also considers `max_message_bytes` if set — buffer capacity = `min(message_count_based, byte_budget_based)`

#### 4b: Bounded shutdown

**`publisher/actor.rs`:**
- On `Command::Close`, set a deadline (e.g., `config.confirm_timeout` from the close command)
- Drain `publishing`, `ledger`, `confirmations` — resolve all waiters
- If the deadline expires before all waiters are resolved, force-resolve remaining with `Err(Closed)` and return
- No PHP destructor can block indefinitely

**`consumer/actor.rs`:**
- On `Command::Close`, set a deadline
- Cancel all pending settlement lanes
- Drain buffers — close all channels
- If the deadline expires, force-close channels and return
- No PHP destructor can block indefinitely

#### 4c: Depth metrics and observability

Add to `Metrics`:
- `publishing_depth: AtomicU64` — current size of `publishing` registry (high-water mark tracked)
- `publishing_bytes: AtomicU64` — current bytes in `publishing` + `ledger` + `replay`
- `replay_depth: AtomicU64` — current size of replay queue
- `replay_count: AtomicU64` — total number of replays since start
- `consumer_buffer_depth: AtomicU64` — total deliveries in all `VecDeque` buffers (high-water mark tracked)
- `consumer_buffer_bytes: AtomicU64` — total bytes in buffers
- `settlement_lane_depth: AtomicU64` — current pending settlements
- `backpressure_duration: AtomicU64` — total time spent in backpressure (microseconds)
- `duplicate_count: AtomicU64` — total duplicate deliveries detected (via `message_id`)

These are read by `stats()` and surfaced in the Laravel status command.

## Stub updates

`crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` — add:
- `Consumer::nextBatch(int $max, int $timeoutMs): array`
- `Consumer::ackThrough(Delivery $delivery): void`
- `Consumer::ackBatch(array $deliveries): void`
- `Delivery::deliveryTag(): int`

## Testing

**Rust tests** (`crates/rabbit-rs-core/tests/`):

*Consumer actor:*
- Event-driven dispatch: verify `Incoming` triggers immediate dispatch (no 1ms timer)
- Settlement lane: verify a slow ACK does not block `Incoming` processing
- Settlement lane: verify `delayed_release` (which calls `publisher.publish()`) does not block the main loop
- Settlement lane: verify `Close` is processed even with pending settlements
- Dispatch clone reduction: verify deliveries are moved, not cloned, from buffers
- `SettleThrough` — verify `channel.ack(tag, multiple=true)` called once with correct channel
- `SettleThrough` with stale generation — verify rejection with `StaleGeneration` error
- `SettleThrough` with already-terminal delivery — verify rejection
- `SettleThrough` with non-contiguous prefix (gap) — verify rejection
- `SettleThrough` across multiple subscriptions/channels — verify each channel acked independently
- `SettleBatch` — verify grouping by channel and one ACK per group
- `SettleBatch` with mixed terminal/pending — verify rejection of terminal, ACK of pending
- Early-ACK mode — verify delivery ACKed before dispatch to buffer
- Early-ACK mode — verify `Delivery::ack()` raises error (not silent no-op)
- True `no_ack` mode — verify per-subscription buffer cap prevents unbounded growth under broker flood
- Byte budget — verify streams stop polling when `max_buffered_bytes` exceeded
- Byte budget — verify publisher backpressure when `publishing_bytes` exceeds limit

*Publisher pipeline:*
- Pipeline publish — verify multiple `basic_publish` frames sent before any confirmation
- Pipeline publish — verify `now_or_never()` drain completes ready futures without waiting for next `select!` tick
- Pipeline publish with recoverable error mid-batch — verify `publishing` registry is drained to `replay`, `publish_in_flight` cleared, actor suspends
- Pipeline publish with recoverable error — verify futures that never complete do not block recovery
- Pipeline publish — verify AMQP frame send order is preserved (sequence order on the wire)
- Pipeline publish — verify in-flight count stays bounded by `buffer_capacity` semaphore
- Pipeline publish — verify `publishing_bytes` stays bounded by `max_buffered_bytes`
- Fire-and-forget mode (`confirms=false`) — verify no confirm future pushed, waiter resolves on transport acceptance
- Global sort on recovery — verify `publishing` + `replay` + `ledger` are merged and sorted by sequence
- Basic.return precedence over ACK — verify a mandatory return takes precedence over its following ACK
- `publish_batch()` result ordering — verify outcomes returned in input order after broker grouping
- Partial batch failure — verify indexed report with `Confirmed`, `Returned`, `Failed`, `NotAccepted`
- Bounded shutdown — verify `Close` resolves within deadline even with pending confirmations
- Bounded shutdown — verify remaining waiters get `Err(Closed)` after deadline

*Config:*
- `SubscriptionConfig` with `no_ack=true` validation and warning
- `SubscriptionConfig` with `early_ack=true` validation and warning
- `total_prefetch` overflow — verify `u64` sum does not overflow with large prefetch values
- `RetainedPublish` carries `sequence` for deterministic replay ordering

**PHP tests** (`crates/rabbit-rs-php/tests/`):
- PHPT reflection: `Consumer::nextBatch`, `Consumer::ackThrough`, `Consumer::ackBatch`, `Delivery::deliveryTag` exist
- PHPT functional: `nextBatch()` with `$max` clamping (0 → 1, 999 → 256)
- PHPT functional: `ackThrough()` with mock or real broker
- PHPT functional: `ackBatch()` with multi-channel grouping
- PHPT functional: `Delivery::ack()` on `AutoAcked` delivery raises `InvalidArgumentException`

**Laravel tests** (`packages/laravel-queue/tests/`):
- Unit test: `no_ack=true` and `early_ack=true` rejected in reliable configuration
- Unit test: best-effort modes allowed only with explicit opt-in

**Benchmark protocol (must precede performance claims):**
- Record: machine, PHP version, Rust version, RabbitMQ version, payload size, warm-up duration, iteration count, median, p99, dispersion
- Equalize prefetch across all drivers (same value for all)
- Fix `RabbitRsDriver.php:28-53`: configure `publisher.confirms` per scenario
- Fix `RabbitRsDriver.php:117-140`: use `ackThrough()` for batch-confirm, `early_ack=true` for auto-ack
- Count unique `message_id` — verify `losses=0` AND `duplicates=0` in reliable mode
- Measure publish latency separately from consume latency (interleaved publish+consume, not all-then-all)
- Separate scenarios clearly:
  - **Reliable:** `confirms=true`, manual ack via `ackThrough()`, `losses=0` and `duplicates=0` verified
  - **Reliable batch:** `confirms=true`, acks grouped via `ackBatch()`, `losses=0` and `duplicates=0` verified
  - **Best-effort (early-ACK):** `confirms=true`, `early_ack=true`, no at-least-once claim
  - **Best-effort (fire-and-forget):** `confirms=false`, no at-least-once claim

**Soak and chaos testing:**
- Soak test: 1M messages, reliable mode, verify `losses=0`, `duplicates=0`, RSS bounded, all waiters resolved
- Chaos: broker restart mid-publish — verify replay + recovery, no lost messages in reliable mode
- Chaos: connection drop mid-consume — verify redelivery of unacked messages
- Chaos: publish future blocked forever — verify recovery is not blocked (futures abandoned, registry drained)
- Chaos: Octane/FPM reload — verify bounded shutdown, no hung destructors
- Chaos: backpressure sustained — verify byte budgets enforced, RSS bounded

## Merge order

1. **Fix benchmark** — correct `RabbitRsDriver` scenario configuration, equalize prefetch, count unique `message_id`, measure publish/consume latency separately, freeze a reproducible baseline
2. **Byte budgets and metrics** — `max_buffered_bytes`, `max_message_bytes`, fix `u16` overflow, depth metrics
3. **Consumer actor unblocking** — settlement lanes, event-driven dispatch, remove 1ms timer, reduce clones
4. **`nextBatch()` + `ackThrough()` + `ackBatch()`** — batch consume and ack APIs
5. **Publisher pipeline + cache + partial failure** — external `publishing` registry, non-blocking drain, broker grouping, indexed batch report
6. **Bounded shutdown** — deadline-based close for publisher and consumer actors
7. **Early-ACK best-effort** — `early_ack` mode, Laravel guard
8. **True `no_ack`** — experimental only, if benchmarks prove necessity

## Expected results

| Metric | Current | Target | Improvement |
|--------|---------|--------|-------------|
| Publish msg/s (safe mode) | 19k | 40-60k | 2-3x (pipeline + cache) |
| Publish msg/s (best-effort) | 19k | 60-80k | 3-4x (no confirm tracking) |
| Consume msg/s (manual ack) | 16k | 25-30k | 1.5-2x (ackThrough + nextBatch) |
| Consume msg/s (early-ACK) | 16k | 30-40k | 2-2.5x (early-ACK + nextBatch) |
| Consume p99 | 646ms | <300ms | 2x (fewer FFI calls + unblocked actor) |
| Budget check | FAIL | PASS | All scenarios pass |
| RSS under load | unbounded | bounded | Byte budgets enforced |
| Shutdown | unbounded | bounded | Deadline-based close |

> Targets are plausible but must be validated against the reproducible benchmark protocol. No at-least-once claim is made for best-effort modes.

## Constraints preserved

- `#![forbid(unsafe_code)]` — no unsafe Rust
- Zend values not retained in Rust threads — polling only, no PHP callbacks from async runtime
- Lapin behind `Transport` abstraction — all changes go through the trait
- Bounded queues, channels, in-flight work — existing limits preserved, augmented by byte budgets
- Delivery tokens remain connection-generation-aware — `SettleThrough` and `SettleBatch` validate full identity
- Recovery order remains deterministic — `no_ack`/`early_ack` consumers are re-created with same option
- `basic.return` precedence over ACK — preserved in the confirm resolution path
- Settlement no longer blocks the consumer actor main loop

## Invariants introduced by this design

- **Publishing registry completeness:** every in-flight publish is in the `publishing` registry before its future is launched. On any failure path, the registry is drained — no publish is silently dropped.
- **Futures are disposable:** publish futures carry only `sequence: u64`. They can be abandoned (`clear()`) on channel death without losing `RetainedPublish` ownership. Recovery is never blocked by pending futures.
- **Global sort on recovery:** `publishing` + `replay` + `ledger` (unconfirmed) are merged and sorted by `sequence` on any recovery path. Deterministic replay order is preserved.
- **Ledger-before-confirm:** a ledger entry is inserted before the confirm future is polled.
- **No barrier needed:** the batch is fully enqueued before awaiting waiters. FIFO ordering is implicit — awaiting the last waiter guarantees all prior waiters are enqueued.
- **Batch ack full identity:** `ackThrough` and `ackBatch` validate `(connection_key, generation, channel_id, delivery_tag)` from the `DeliveryToken`. Stale generations, wrong channels, already-terminal deliveries, and non-contiguous prefixes are rejected.
- **Early-ACK is best-effort:** `early_ack=true` ACKs before PHP processing. Not at-least-once. Preserves broker QoS (prefetch still works). `Delivery::ack()` on an `AutoAcked` delivery raises an explicit error.
- **True `no_ack` is experimental:** `no_ack=true` disables broker prefetch. Per-subscription buffer bounds are the only flow control. Only used if benchmarks prove necessity.
- **Byte budgets bound memory:** `max_buffered_bytes` limits publisher memory (`publishing` + `ledger` + `replay`) and consumer buffer memory. Either message-count or byte budget can trigger backpressure.
- **Settlement does not block the actor:** settlements run in spawned lanes. The main `select!` loop processes `Incoming`, `Close`, and dispatch independently.
- **Event-driven dispatch:** no 1ms timer. Dispatch triggers on `Incoming`, settlement completion, and pump notification.
- **Bounded shutdown:** `Close` resolves within a deadline. Remaining waiters and settlements are force-resolved after deadline. No PHP destructor blocks indefinitely.
- **Fire-and-forget guarantee wording:** when `confirms=false`, the guarantee is "transport accepted the frame", not "message reached kernel socket buffer". This mode does not provide at-least-once.
- **Result ordering preserved:** `publish_batch()` returns outcomes in the same order as input requests, even after grouping by broker.
- **Partial batch failure is explicit:** `publish_batch()` returns an indexed report per message. Callers inspect `results[i]` rather than getting an opaque global error.
