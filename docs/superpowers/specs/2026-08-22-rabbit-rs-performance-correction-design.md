# Rabbit-rs Performance Correction Plan

**Date:** 2026-08-22 **Status:** Needs revision **Scope:** Bugs + API gaps + Performance optimizations + Stability

## Context

Benchmark results revealed rabbit-rs is significantly slower than amqplib:

| Metric        | rabbit-rs | amqplib | Ratio       |
|---------------|-----------|---------|-------------|
| Publish msg/s | 19k       | 78k     | 4.1x slower |
| Consume msg/s | 16k       | 39k     | 2.4x slower |
| Consume p99   | 646ms     | 254ms   | 2.5x higher |

Root causes identified through source analysis:

1. **`no_ack` mode hardcoded to `false`** — `LapinConsumerChannel::consume()` in `transport/lapin.rs:238` hardcodes
   `no_ack: false`. No configuration path exists to enable it. The benchmark auto-ack scenario acks every message
   manually, adding 1 FFI call + 1 network round-trip per delivery.

2. **No batch acknowledgement** — `Delivery::ack()` in `consumer/actor.rs:321` always passes `multiple=false`. Each
   delivery requires a separate `ConsumerCommand::Settle` → actor → `channel.ack()` → response round-trip via
   `block_on`.

3. **Sequential `basic_publish` in `publish_queue()`** — `publisher/actor.rs:519-588` awaits each `channel.publish()`
   before sending the next. Confirmations run concurrently via `FuturesUnordered`, but the write side is serialized.

4. **Publisher handle lookup per message** — `client.rs:131` calls `self.publisher(&broker).await` for each message in a
   batch — 256 mutex + hashmap lookups for a 256-message batch to the same broker.

5. **3 FFI boundary crossings per delivery** — `tryNext()` + `payload()` + `ack()`. For 10k messages, that's 30k FFI
   calls.

6. **Benchmark scenario mismatch** — `RabbitRsDriver.php:28-53` does not configure `publisher.confirms` per scenario.
   `RabbitRsDriver.php:117-140` always acks each message individually. Prefetch is 500 for rabbit-rs but 16 for
   php-amqplib. The publish p99 measured is actually a consume latency (all messages published before any consumed).
   Message ID uniqueness is not verified, so losses masked by duplicates are invisible.

7. **Consumer actor blocks on settlement** — `consumer/actor.rs:296-350` runs `channel.ack().await` and
   `delayed_release().await` (which calls `publisher.publish().await`) directly in the `select!` loop. A slow ACK or a
   delayed release blocks all subscriptions, recovery, and `close()`.

8. **Memory bounded by message count, not bytes** — `buffer_capacity` (1024) bounds the number of in-flight publishes
   but 1024 × 1 MiB payloads = ~1 GiB. The `total_prefetch` sum in `consumer/set.rs:155` uses `u16` which can overflow.

## Decisions

| Decision                   | Choice                                                                                                                                  |
|----------------------------|-----------------------------------------------------------------------------------------------------------------------------------------|
| Scope                      | All (bugs + gaps + performance + stability)                                                                                             |
| Consume callback           | Preserve constraint: no PHP callbacks from Rust threads. Polling only.                                                                  |
| Best-effort mode           | **Early-ACK** as primary best-effort (preserves broker QoS). True `no_ack` experimental only.                                           |
| Best-effort classification | Not at-least-once. Forbidden by default in the Laravel driver.                                                                          |
| Batch ack API              | `Consumer::ackThrough(Delivery $delivery)` (simple, single channel) + `Consumer::ackBatch(array $deliveries)` (advanced, multi-channel) |
| Batch consume API          | `Consumer::nextBatch(int $max, int $timeoutMs): array` — wait once then drain                                                           |
| Compatibility              | Retro-compatible — new fields/methods are optional with current behavior as default                                                     |
| Pipeline replay            | External `publishing` registry — futures carry sequence only, not `RetainedPublish`                                                     |
| Barrier                    | **No barrier.** Batch is fully enqueued before awaiting waiters; FIFO ordering is implicit.                                             |
| Batch failure              | Indexed report per message (`confirmed`, `returned`, `failed`, `not_accepted`)                                                          |
| Memory bounds              | Byte budgets per subscription and global, `u64` sums with checked arithmetic                                                            |
| Organization               | 8-step ordered implementation (benchmark first, early-ACK last)                                                                         |

## Design

### Track 1: Config (prerequisite)

**`config.rs`:**

- Add `no_ack: bool` field to `SubscriptionConfig` (default: `false`)
- Add `early_ack: bool` field to `SubscriptionConfig` (default: `false`)
- Add `max_message_bytes: Option<u64>` to `SubscriptionConfig` (default: `None` = unlimited per-message, but bounded by
  global budget)
- Add `max_buffered_bytes: u64` to `PublisherConfig` (default: 64 MiB)
- Validation: if `no_ack=true`, log a warning that this mode is best-effort and not at-least-once, and that `early_ack`
  is recommended instead. The `prefetch` field is still used for buffer sizing in `consumer/set.rs:155-158`.
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

> **Problem:** `consumer/actor.rs:296-350` runs `settle()` directly in the `select!` loop. `channel.ack().await` blocks
> on network I/O. `delayed_release()` calls `publisher.publish().await` which can take seconds. While a settlement is in
> progress, the actor cannot process `Incoming`, `Close`, `UpdateGeneration`, or dispatch new deliveries.

**`consumer/actor.rs`:**

**Settlement lanes — serialized per channel:**

Move settlement execution out of the main `select!` loop. However, settlements on the **same AMQP channel** must be
serialized — concurrent `basic.ack` frames on the same channel can race on frame ordering. Settlements on **different
channels** can run in parallel.

```rust
// New: track pending settlements, keyed by channel for serialization
struct ActorState {
    // ... existing fields ...
    // One pending settlement future per channel — serializes acks on that channel
    pending_settlements: HashMap<ChannelKey, Pin<Box<dyn Future<Output=SettlementResult>>>>,
}

// ChannelKey uniquely identifies a channel: (subscription_id, connection_key, generation, channel_id)
// Two deliveries on the same channel share the same ChannelKey.
// The HashMap ensures only one settlement is in-flight per channel at a time.

struct SettlementResult {
    channel_key: ChannelKey,
    token: Arc<DeliveryTokenInner>,
    result: Result<DeliveryState, ConsumerError>,
    completed: oneshot::Sender<Result<DeliveryState, ConsumerError>>,
}
```

In the `select!` loop, the `Settle` arm:

1. **Reserves the token** — marks it as "settling" to prevent double-settlement. The `in_flight` budget is NOT released
   yet — it is released only when the settlement completes.
2. Checks if there is already a pending settlement for this channel (`pending_settlements.contains_key(&channel_key)`).
    - If yes: queue the settlement in a per-channel FIFO
      (`settlement_queue: HashMap<ChannelKey, VecDeque<SettleParams>>`). It will be launched when the current one
      completes.
    - If no: launch the settlement immediately.
3. The settlement future runs `channel.ack().await` (or `reject`, `release`) — the main loop is not blocked.

When a settlement completes:

1. Release the `in_flight` budget.
2. Resolve the `oneshot` sender.
3. Check the per-channel queue — if there is a queued settlement, launch it next.
4. Dispatch new deliveries.

```rust
// New select! arm — poll all pending settlements simultaneously
// (each is on a different channel, so no ordering conflict)
Some(settlement_result) = next_settlement( & mut state.pending_settlements) => {
let channel_key = settlement_result.channel_key;
state.pending_settlements.remove( & channel_key);
state.release_budget();
state.dispatch();
let _ = (settlement_result.completed).send(settlement_result.result);

// Launch next queued settlement for this channel
if let Some(queue) = state.settlement_queue.get_mut( & channel_key) {
if let Some(next) = queue.pop_front() {
launch_settlement( & mut state, channel_key, next);
}
}
}
```

**Token reservation before I/O:**

The `DeliveryTokenInner` must have a `settling: AtomicBool` field. When `Settle` is received:

1. `token.settling.compare_exchange(false, true)` — if already `true`, reject with `AlreadySettling` error.
2. Launch the settlement future.
3. On completion, set `settling = false` (or mark terminal).

This prevents a second `ackThrough()` or `ack()` on the same delivery from launching a duplicate settlement while the
first is still in-flight.

Bounded by `max_in_flight` — the total number of concurrent settlement lanes across all channels is naturally limited by
the in-flight budget. The per-channel serialization adds fairness without increasing the bound.

**Event-driven dispatch:**

Remove the 1ms dispatch timer (`actor.rs:203`). Dispatch on:

- `Incoming` — a new delivery arrived, try to dispatch
- Settlement completed — budget freed, try to dispatch
- `dispatch_notify` — pump signaled more deliveries available

```rust
loop {
tokio::select ! {
// ... command arms ...
Some(ConsumerCommand::Incoming { .. }) => {
// Push to buffer, mark ready
state.dispatch();  // immediate dispatch attempt
}
Some(settlement_result) = state.pending_settlements.next(), ...=> {
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

Current `dispatch()` (`actor.rs:117-121`) clones `TransportDelivery` from the buffer front, then clones headers again at
line 141. Replace with `pop_front()` and move ownership:

```rust
// Before: .front().cloned() — clones the delivery
// After:
let Some(delivery) = self .buffers.get_mut( & subscription).and_then(VecDeque::pop_front) else {
self.scheduler.mark_empty( & subscription);
return;
};
// Move delivery fields into Delivery::new() — no clone
```

This eliminates one `TransportDelivery::clone()` and one `headers.clone()` per dispatched delivery.

#### 2b: Early-ACK best-effort mode

> **Classification: best-effort.** The message is ACKed to RabbitMQ before PHP processes it. If PHP crashes, the message
> is lost. This mode preserves broker QoS (prefetch still works), unlike true `no_ack`.

**`consumer/actor.rs`:**

When `early_ack=true`:

- On `Incoming`, immediately ACK the delivery via the channel: `channel.ack(delivery_tag, false).await` — but in a
  settlement lane, not the main loop
- Do not create a `DeliveryToken` (no settlement tracking needed — already acked)
- Do not increment `in_flight` (no budget tracking — already acked)
- Create `Delivery` with settlement state `AutoAcked`
- Dispatch to the flume buffer immediately after the ACK completes
- The delivery still carries `delivery_tag` for user-facing API consistency

**`delivery.rs` (PHP):**

- `Delivery::ack()`, `Delivery::release()`, `Delivery::reject()` on an `AutoAcked` delivery: raise an
  `InvalidArgumentException` explicitly — do not silently no-op
- `Delivery::deliveryTag(): int` accessor for all modes

**`consumer.rs` (PHP):**

- `Consumer` stores `early_ack` and `no_ack` per subscription
- Pass the settlement state to each `Delivery` object

**True `no_ack` mode (experimental):**

When `no_ack=true` (not `early_ack`):

- Pass `no_ack: true` to `BasicConsumeOptions` — broker auto-acks, prefetch is ignored
- Same delivery behavior as `early_ack` (no token, `AutoAcked` state)
- **Additional bound:** the `VecDeque` buffer per subscription must be capped at `max(total_prefetch, 256)` entries.
  When full, stop polling that subscription's stream until the buffer drains. This is the only flow control available
  since the broker will not block.
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
- The `Delivery` carries its `DeliveryToken` with full identity:
  `(subscription_id, connection_key, generation, channel_id, delivery_tag)`
- Single `block_on` for the entire batch

Add `Consumer::ackBatch(array $deliveries): void`:

- For multi-channel `ConsumerSet` scenarios
- Group deliveries by `(subscription_id, channel_id, generation)`
- For each group, validate the contiguous prefix and send one `channel.ack(highest_tag, multiple=true)`
- Reject if any delivery is already terminal
- Reject if the prefix is non-contiguous (gap in delivery tags)
- The core renders all affected tokens terminal after the ACK

**`consumer/actor.rs`:**

**Per-channel delivery ledger — populated from `Incoming`:**

The per-channel delivery ledger must be populated **at `Incoming` time**, not at `dispatch()` time. This is required for
`ackThrough` and `ackBatch` to know about all pending deliveries on a channel, including those still buffered in the
`VecDeque` and not yet dispatched to PHP.

```rust
struct ChannelLedger {
    // delivery_tag → ledger entry (populated at Incoming, removed at settlement)
    pending: BTreeMap<u64, DeliveryLedgerEntry>,
    // The contiguous acked prefix (highest tag acked with multiple=true)
    acked_prefix: u64,
}

struct DeliveryLedgerEntry {
    subscription: SubscriptionId,
    delivery_tag: u64,
    state: DeliveryState,  // Pending | Settling | Acked | Rejected
    token: Option<Arc<DeliveryTokenInner>>,  // None until dispatch() creates the token
}
```

On `Incoming`:

```rust
Some(ConsumerCommand::Incoming { subscription, result }) => match result {
Ok(delivery) => {
// Register in the channel ledger BEFORE buffering
state.channel_ledgers
.entry(channel_key_for( & subscription, & state))
.or_default()
.pending
.insert(delivery.delivery_tag, DeliveryLedgerEntry {
subscription: subscription.clone(),
delivery_tag: delivery.delivery_tag,
state: DeliveryState::Pending,
token: None,  // filled at dispatch()
});

// Then buffer and mark ready
if let Some(buffer) = state.buffers.get_mut( & subscription) {
buffer.push_back(delivery);
state.scheduler.mark_ready( & subscription);
}
}
// ...
}
```

At `dispatch()`, the token is created and stored back into the ledger entry:

```rust
// In dispatch(), after creating the token:
state.channel_ledgers.get_mut( & channel_key)
.and_then( | ledger| ledger.pending.get_mut( & delivery_tag))
.map( | entry| entry.token = Some(token.clone()));
```

**`SettleThrough` — contiguous prefix validation:**

```rust
Add `ConsumerCommand::SettleThrough { token: Arc<DeliveryTokenInner>, completed: oneshot::Sender<Result<DeliveryState, ConsumerError> > }`:
```

Actor validates:

1. Full identity from the token (connection, generation, channel, tag)
2. The ledger contains a contiguous prefix from `acked_prefix + 1` up to and including `delivery_tag`
3. No delivery in that prefix is already terminal (Acked/Rejected)
4. If valid: calls `channel.ack(delivery_tag, multiple=true).await` in a settlement lane
5. Releases budget for all affected deliveries (count = number of entries in the prefix)
6. Renders all affected tokens terminal

**`ackThrough` concurrent processing limitation:**

`ackThrough()` on the same channel must not run concurrently. If a `SettleThrough` is already in-flight for a channel, a
second `SettleThrough` for the same channel must be rejected with `SettlementInProgress` error. This is enforced by the
per-channel settlement serialization (Point 2) and the `settling: AtomicBool` on the token.

For `ackBatch`, the same rule applies per channel group — only one batch ACK per channel at a time.

```rust
Add `ConsumerCommand::SettleBatch { tokens: Vec<Arc<DeliveryTokenInner> >, completed: oneshot::Sender<Result<Vec<DeliveryState>, ConsumerError> > }`:
```

- Group tokens by `(subscription_id, channel_id, generation)`
- For each group: validate the contiguous oldest prefix, reject if any gap or terminal delivery
- Execute one ACK per group — serialized per channel via the settlement queue
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

**Core design — external `publishing` registry + sequential `now_or_never()`:**

Keep `RetainedPublish` values in an explicit registry `publishing: HashMap<u64, RetainedPublish>` owned by `ActorState`.
Publishes are polled **sequentially** via `now_or_never()` — not via `FuturesUnordered` — to guarantee AMQP frame send
order. `FuturesUnordered` is used only as a fallback for the rare case where `now_or_never()` returns `None` (Lapin
buffer full, ~1%).

**Why not `FuturesUnordered` for publishes:** `FuturesUnordered` polls futures in arbitrary order. Tokio may poll future
B before future A, causing `channel.publish()` to be called out of order. AMQP requires frames to be sent in sequence
order on a channel. Sequential `now_or_never()` guarantees that `basic_publish()` is called in sequence order — it polls
each future exactly once, synchronously, without yielding to the runtime.

**Cost comparison:**

- `.await` (current): ~5-20us per publish (full Tokio poll/wake/schedule cycle)
- `now_or_never()` sequential: ~0.5-1us per publish (single `poll()`, no wake, no re-schedule)
- `FuturesUnordered`: ~1-3us per publish but **order not guaranteed**

`now_or_never()` is the fastest ordered approach. It is essentially a synchronous call that avoids the async runtime
overhead entirely for the ~99% common case.

```rust
// In ActorState:
publishing: HashMap<u64, RetainedPublish>,   // sequence → retained (owned by actor)
publish_in_flight: FuturesUnordered<PublishFuture>,  // ONLY for buffer-full fallback

type PublishFuture = Pin<Box<dyn Future<Output=(u64, Result<PublisherConfirm, TransportError>)>>>;
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

// 5. Register in publishing BEFORE launching the publish
state.publishing.insert(sequence, retained);

// 6. Create the publish future — do NOT push to FuturesUnordered yet
let publish_fut = channel.publish(request);

// 7. Poll sequentially with now_or_never() — guarantees frame send order
match publish_fut.now_or_never() {
Some(result) => {
// Fast path (~99%): basic_publish wrote to Lapin buffer immediately
handle_publish_completion(state, sequence, result);
}
None => {
// Slow path (~1%): Lapin buffer full, need to await
// Push to FuturesUnordered for async polling in the select! loop
state.publish_in_flight.push(Box::pin(async move {
let result = publish_fut.await;
(sequence, result)
}));
}
}
}
```

The `select!` loop arm (only for the buffer-full fallback futures):

```rust
Some((sequence, result)) = state.publish_in_flight.next(),
if ! state.publish_in_flight.is_empty() => {
handle_publish_completion(state, sequence, result);
}
```

**Deadline cancellation per sequence:**

Each entry in `publishing` must carry its deadline. The `select!` loop must also poll a deadline timer that checks
`publishing` for expired entries. On expiry, remove the entry from `publishing`, fail its waiter with `Err(Timeout)`,
and cancel the corresponding future if it is in `publish_in_flight`.

Since `publish_in_flight` only contains the rare buffer-full futures, the deadline check iterates `publishing` (a
`HashMap`) rather than the `FuturesUnordered`. This is efficient because `publishing` is typically small (most publishes
complete synchronously via `now_or_never()`).

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
2. **Remove on completion:** `publishing.remove(&sequence)` happens in `handle_publish_completion` on
   success/terminal-error.
3. **Drain on recoverable error:** `publishing.drain()` recovers ALL retains. Futures are abandoned
   (`publish_in_flight.clear()`). The faulty element and registry contents are merged and sorted globally by `sequence`
   for deterministic replay.
4. **Bounded by semaphore:** the `buffer_capacity` semaphore limits entries in `publishing`. Each `RetainedPublish`
   holds a `_permit` released on terminal resolution.

**Recovery, close, and permanent failure must also drain `publishing`:**

- On `ConnectionEvent::Recovering`: merge `publishing` + `replay` + `ledger` (entries not yet confirmed), sort globally
  by sequence, clear `publish_in_flight`.
- On `ConnectionEvent::FailedPermanent`: drain `publishing` and fail all waiters with the permanent error.
- On `Command::Close`: drain `publishing` and fail all waiters with `Closed`.
- On deadline expiry: check `publishing` for expired entries and fail their waiters.

**`RetainedPublish` must carry `sequence`:**

Add a `sequence: u64` field to `RetainedPublish`, set at the moment `state.sequence` is assigned.

**AMQP frame ordering:**

`FuturesUnordered` does not guarantee completion order, but AMQP frame send order is determined by the order
`channel.publish()` is called. Since `publish_queue()` calls them sequentially without `await` between publishes, the
frame order on the wire is preserved. `FuturesUnordered` only affects when we observe the completion, not the send
order.

#### 3b: Cache publisher handle with result ordering

**`client.rs`:**

- In `publish_batch()`, group messages by broker first, then acquire publisher handle once per broker
- Preserve input order in results: track original positions so `outcomes` is returned in the same order as `requests`

```rust
let mut by_broker: HashMap< & str, Vec<(usize, PublishRequest) > > = HashMap::new();
for (i, (broker, request)) in requests.into_iter().enumerate() {
by_broker.entry(broker).or_default().push((i, request));
}

let mut outcomes: Vec<Option<Result<PublishOutcome, ClientError> > > = vec![None; total_count];
let mut waiters: Vec<(usize, PublishWaiter) > = Vec::new();

for (broker, msgs) in by_broker {
let publisher = self.publisher(broker).await ?;
for (original_index, msg) in msgs {
match publisher.try_publish(msg) {
Ok(waiter) => waiters.push((original_index, waiter)),
Err(error) => {
outcomes[original_index] = Some(Err(ClientError::publish( & error)));
}
}
}
}

for (index, waiter) in waiters {
match waiter.wait().await {
Ok(outcome) => outcomes[index] = Some(Ok(outcome)),
Err(error) => outcomes[index] = Some(Err(ClientError::publish( & error))),
}
}
```

- Single-message `publish()` already does 1 lookup — no change needed.

#### 3c: Partial batch failure reporting

> **No barrier needed.** The batch is fully enqueued via `try_publish()` before awaiting any waiters. Waiters resolve in
> the background via the actor. Awaiting the last waiter guarantees all prior waiters have been enqueued. The barrier from
> the previous spec revision is unnecessary and has been removed.

**`client.rs`:**

**Preserve the existing `publish_batch()` signature** for backward compatibility. Add a new `publish_batch_detailed()`
method for the indexed report.

```rust
// EXISTING signature — UNCHANGED
pub async fn publish_batch(
    &self,
    requests: Vec<(String, PublishRequest)>,
) -> Result<Vec<PublishOutcome>, ClientError> {
    // Uses publish_batch_detailed() internally, then maps to the old format:
    // - If any message is Failed or NotAccepted, returns the first error
    // - Otherwise returns Vec<PublishOutcome> in input order
    let detailed = self.publish_batch_detailed(requests).await?;
    let mut outcomes = Vec::with_capacity(detailed.results.len());
    let mut terminal_error = None;
    for result in detailed.results {
        match result {
            MessageOutcome::Confirmed(o) => outcomes.push(o),
            MessageOutcome::Returned(info) => {
                terminal_error.get_or_insert_with(|| ClientError::publish(
                    &PublishError::new(PublishErrorKind::Returned, info.to_string())
                ));
            }
            MessageOutcome::Failed(e) => {
                terminal_error.get_or_insert_with(|| ClientError::publish(&e));
            }
            MessageOutcome::NotAccepted(e) => {
                terminal_error.get_or_insert_with(|| ClientError::publish(&e));
            }
        }
    }
    terminal_error.map_or(Ok(outcomes), Err)
}

// NEW — indexed report per message
pub struct BatchOutcome {
    pub results: Vec<MessageOutcome>,
}

pub enum MessageOutcome {
    Confirmed(PublishOutcome),   // broker confirmed via publisher confirm
    Returned(ReturnInfo),        // basic.return — message was unroutable (mandatory)
    Failed(PublishError),         // terminal error (timeout, channel closed, etc.)
    NotAccepted(PublishError),    // never enqueued (backpressure, unknown broker)
}

pub async fn publish_batch_detailed(
    &self,
    requests: Vec<(String, PublishRequest)>,
) -> Result<BatchOutcome, ClientError> {
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

- The existing `publish_batch()` preserves the current semantics: first terminal failure after resolving all accepted
  publications (`client.rs:122-162`)
- `publish_batch_detailed()` returns the full indexed report — callers inspect `results[i]` for each message at index
  `i`
- A `Returned` result means `basic.return` took precedence over the ACK (preserved invariant)

### Track 4: Memory budgets and operational stability

#### 4a: Byte budgets — reserve early, release late

> **Principle:** bytes must be reserved **before** the message enters the command channel, and the permit must be held
> until the **real terminal resolution** (confirm, error, or drain). This prevents a burst of messages from exceeding the
> byte budget between the command channel and the actor.

**`config.rs`:**

- Add `max_buffered_bytes: u64` to `PublisherConfig` (default: 64 MiB) — covers `publishing` + `ledger` + `replay`
- Add `max_buffered_bytes: u64` to `SubscriptionConfig` (default: 64 MiB) — covers consumer `VecDeque` buffers + flume
  buffer
- Add `max_message_bytes: Option<u64>` to `SubscriptionConfig` (default: `None` = unlimited per-message, bounded by
  global budget)

**Publisher byte budget — `client.rs` + `publisher/actor.rs`:**

The byte budget is checked in `PublisherHandle::try_publish()` (client side), **before** the message is sent to the
command channel. This is the earliest possible enforcement point.

```rust
// In PublisherHandle:
struct PublisherHandle {
    // ... existing fields ...
    byte_budget: Arc<ByteBudget>,  // shared between handle and actor
}

struct ByteBudget {
    current_bytes: AtomicU64,
    max_bytes: u64,
}

impl PublisherHandle {
    pub fn try_publish(&self, request: PublishRequest) -> Result<PublishWaiter, PublishError> {
        let payload_bytes = request.payload.len() as u64;
        // Reserve bytes BEFORE acquiring the semaphore or sending the command
        self.byte_budget.reserve(payload_bytes).map_err(|_| {
            self.metrics.record_backpressure();
            PublishError::new(PublishErrorKind::Backpressure, "byte budget exhausted")
        })?;

        // Acquire semaphore (message count) — may fail
        let permit = self.capacity.clone().try_acquire_owned().map_err(|_| {
            self.byte_budget.release(payload_bytes);  // release bytes on failure
            self.metrics.record_backpressure();
            PublishError::new(PublishErrorKind::Backpressure, "capacity exhausted")
        })?;

        // Send command
        // ...
        Ok(PublishWaiter::new(receiver))
    }
}
```

The byte budget is released when the `RetainedPublish` is consumed — i.e., when `handle_publish_completion` removes it
from `publishing` (success, terminal error), or when it is drained to `replay` (recoverable error, but then `replay`
bytes are counted separately).

**The `_permit` (semaphore) and byte reservation are both held by `RetainedPublish`** and released together at terminal
resolution. This ensures neither is leaked.

**Consumer byte budget — `consumer/set.rs` + `consumer/actor.rs`:**

- Track `buffered_bytes: u64` per subscription and `total_buffered_bytes: u64` global
- When a delivery arrives (`Incoming`), add `delivery.payload.len()` to the subscription and global counters **before**
  pushing to the buffer
- When a delivery is dispatched (popped from buffer), the bytes stay counted — they are released only when the
  settlement completes (the payload is referenced by the `DeliveryToken` until then)
- If `total_buffered_bytes` exceeds `max_buffered_bytes` for the subscription, stop polling that subscription's stream
  until bytes drain
- The flume buffer bytes are also counted: `flume::bounded` capacity is in messages, but the byte budget adds a second
  dimension

**Coverage:** the byte budget covers the full pipeline:

- Publisher: command channel → `publishing` registry → `ledger` → `replay`
- Consumer: `Incoming` → `VecDeque` buffer → flume buffer → `in_flight` (until settlement)

**`consumer/set.rs`:**

- Fix `total_prefetch` overflow (Track 1): use `u64` with `saturating_sum`
- Buffer sizing also considers `max_message_bytes` if set — buffer capacity =
  `min(message_count_based, byte_budget_based)`

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
- Settlement serialization: verify two ACKs on the same channel are serialized (not concurrent)
- Settlement serialization: verify ACKs on different channels run concurrently
- Token reservation: verify double-settlement on same delivery is rejected with `AlreadySettling`
- Dispatch clone reduction: verify deliveries are moved, not cloned, from buffers
- Channel ledger: verify deliveries registered at `Incoming` time, not `dispatch()` time
- Channel ledger: verify buffered (not-yet-dispatched) deliveries are visible to `ackThrough`
- `SettleThrough` — verify `channel.ack(tag, multiple=true)` called once with correct channel
- `SettleThrough` with stale generation — verify rejection with `StaleGeneration` error
- `SettleThrough` with already-terminal delivery — verify rejection
- `SettleThrough` with non-contiguous prefix (gap) — verify rejection
- `SettleThrough` across multiple subscriptions/channels — verify each channel acked independently
- `SettleThrough` concurrent on same channel — verify rejection with `SettlementInProgress`
- `SettleBatch` — verify grouping by channel and one ACK per group
- `SettleBatch` with mixed terminal/pending — verify rejection of terminal, ACK of pending
- Early-ACK mode — verify delivery ACKed before dispatch to buffer
- Early-ACK mode — verify `Delivery::ack()` raises error (not silent no-op)
- True `no_ack` mode — verify per-subscription buffer cap prevents unbounded growth under broker flood
- Byte budget (consumer) — verify streams stop polling when `max_buffered_bytes` exceeded
- Byte budget (consumer) — verify bytes counted from `Incoming` until settlement completes
- Byte budget (publisher) — verify backpressure when `publishing_bytes` exceeds limit
- Byte budget (publisher) — verify bytes reserved before command channel, released at terminal resolution

*Publisher pipeline:*

- Pipeline publish — verify multiple `basic_publish` frames sent before any confirmation
- Pipeline publish — verify `now_or_never()` sequential poll preserves AMQP frame send order (sequence order on the
  wire)
- Pipeline publish — verify `now_or_never()` fast path completes without yielding to runtime (~99% case)
- Pipeline publish — verify buffer-full fallback pushes to `FuturesUnordered` for async polling
- Pipeline publish with recoverable error mid-batch — verify `publishing` registry is drained to `replay`,
  `publish_in_flight` cleared, actor suspends
- Pipeline publish with recoverable error — verify futures that never complete do not block recovery
- Pipeline publish — verify in-flight count stays bounded by `buffer_capacity` semaphore
- Pipeline publish — verify `publishing_bytes` stays bounded by `max_buffered_bytes`
- Pipeline publish — verify byte budget reserved before command channel send, released at terminal resolution
- Pipeline publish — verify deadline cancellation per sequence (expired entries removed from `publishing`, waiters
  failed)
- Fire-and-forget mode (`confirms=false`) — verify no confirm future pushed, waiter resolves on transport acceptance
- Global sort on recovery — verify `publishing` + `replay` + `ledger` (unconfirmed) are merged and sorted by sequence
- Basic.return precedence over ACK — verify a mandatory return takes precedence over its following ACK
- `publish_batch()` — verify existing signature returns `Result<Vec<PublishOutcome>, ClientError>` (backward compatible)
- `publish_batch_detailed()` — verify indexed report with `Confirmed`, `Returned`, `Failed`, `NotAccepted`
- `publish_batch()` result ordering — verify outcomes returned in input order after broker grouping
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

- Record: machine, PHP version, Rust version, RabbitMQ version, payload size, warm-up duration, iteration count, median,
  p99, dispersion
- Equalize prefetch across all drivers (same value for all)
- Fix `RabbitRsDriver.php:28-53`: configure `publisher.confirms` per scenario
- Fix `RabbitRsDriver.php:117-140`: use `ackThrough()` for batch-confirm (after step 4), `early_ack=true` for auto-ack
  (after step 8)
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

1. **Fix benchmark** — correct `RabbitRsDriver` scenario configuration, equalize prefetch, count unique `message_id`,
   measure publish/consume latency separately, freeze a reproducible baseline. Use existing APIs only (individual ack,
   no early-ack).
2. **Byte budgets and metrics** — `max_buffered_bytes` (publisher + consumer), `max_message_bytes`, fix `u16` overflow,
   depth metrics. Reserve bytes before command channel, hold permit until terminal resolution.
3. **Consumer actor unblocking** — settlement lanes serialized per `(connection, generation, channel)`, event-driven
   dispatch, remove 1ms timer, reduce clones. Token reservation via `settling: AtomicBool`.
4. **`nextBatch()` + `ackThrough()` + `ackBatch()`** — batch consume and ack APIs. Per-channel delivery ledger populated
   from `Incoming`. Contiguous prefix validation.
5. **Update benchmark** — re-run with `ackThrough()` for batch-confirm scenario, `nextBatch()` in consume loop. Now that
   step 4 is implemented.
6. **Publisher pipeline + cache + partial failure** — external `publishing` registry, sequential `now_or_never()`,
   `FuturesUnordered` fallback only, broker grouping, `publish_batch_detailed()`.
7. **Bounded shutdown** — deadline-based close for publisher and consumer actors.
8. **Early-ACK best-effort** — `early_ack` mode, Laravel guard.
9. **Update benchmark** — re-run with `early_ack=true` for auto-ack scenario. Now that step 8 is implemented.
10. **True `no_ack`** — experimental only, if benchmarks prove necessity.

## Expected results

| Metric                      | Current   | Target  | Improvement                            |
|-----------------------------|-----------|---------|----------------------------------------|
| Publish msg/s (safe mode)   | 19k       | 40-60k  | 2-3x (pipeline + cache)                |
| Publish msg/s (best-effort) | 19k       | 60-80k  | 3-4x (no confirm tracking)             |
| Consume msg/s (manual ack)  | 16k       | 25-30k  | 1.5-2x (ackThrough + nextBatch)        |
| Consume msg/s (early-ACK)   | 16k       | 30-40k  | 2-2.5x (early-ACK + nextBatch)         |
| Consume p99                 | 646ms     | <300ms  | 2x (fewer FFI calls + unblocked actor) |
| Budget check                | FAIL      | PASS    | All scenarios pass                     |
| RSS under load              | unbounded | bounded | Byte budgets enforced                  |
| Shutdown                    | unbounded | bounded | Deadline-based close                   |

> Targets are plausible but must be validated against the reproducible benchmark protocol. No at-least-once claim is
> made for best-effort modes.

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

- **Publishing registry completeness:** every in-flight publish is in the `publishing` registry before its future is
  launched. On any failure path, the registry is drained — no publish is silently dropped.
- **Sequential frame ordering:** publishes are polled via `now_or_never()` in sequence order, guaranteeing AMQP frame
  send order. `FuturesUnordered` is used only as a fallback for the rare buffer-full case (~1%), where the frame may be
  slightly delayed but the replay mechanism recovers it.
- **Futures are disposable:** publish futures carry only `sequence: u64`. They can be abandoned (`clear()`) on channel
  death without losing `RetainedPublish` ownership. Recovery is never blocked by pending futures.
- **Deadline cancellation per sequence:** each entry in `publishing` carries its deadline. Expired entries are removed
  and their waiters failed with `Err(Timeout)` independently of `FuturesUnordered`.
- **Global sort on recovery:** `publishing` + `replay` + `ledger` (unconfirmed) are merged and sorted by `sequence` on
  any recovery path. Deterministic replay order is preserved.
- **Ledger-before-confirm:** a ledger entry is inserted before the confirm future is polled.
- **No barrier needed:** the batch is fully enqueued before awaiting waiters. FIFO ordering is implicit — awaiting the
  last waiter guarantees all prior waiters are enqueued.
- **Settlement serialized per channel:** two settlements on the same AMQP channel are never concurrent. A per-channel
  FIFO queue ensures ordering. Settlements on different channels run in parallel.
- **Token reservation before I/O:** a `settling: AtomicBool` on the token prevents double-settlement. The `in_flight`
  budget is reserved before the settlement future is launched and released only on completion.
- **Channel ledger from `Incoming`:** every delivery is registered in the per-channel ledger at `Incoming` time,
  including buffered (not-yet-dispatched) deliveries. `ackThrough` and `ackBatch` see the full set of pending
  deliveries.
- **Batch ack full identity:** `ackThrough` and `ackBatch` validate
  `(connection_key, generation, channel_id, delivery_tag)` from the `DeliveryToken`. Stale generations, wrong channels,
  already-terminal deliveries, and non-contiguous prefixes are rejected.
- **`ackThrough` concurrency limit:** only one `ackThrough` or `ackBatch` per channel at a time. Concurrent attempts are
  rejected with `SettlementInProgress`.
- **Early-ACK is best-effort:** `early_ack=true` ACKs before PHP processing. Not at-least-once. Preserves broker QoS
  (prefetch still works). `Delivery::ack()` on an `AutoAcked` delivery raises an explicit error.
- **True `no_ack` is experimental:** `no_ack=true` disables broker prefetch. Per-subscription buffer bounds are the only
  flow control. Only used if benchmarks prove necessity.
- **Byte budgets reserve early, release late:** publisher bytes are reserved in `try_publish()` before the command is
  sent. Consumer bytes are counted at `Incoming`. Both are released only at terminal resolution (confirm, error,
  settlement). Coverage spans command channel, `publishing`, `ledger`, `replay`, `VecDeque` buffers, and flume.
- **Settlement does not block the actor:** settlements run in serialized per-channel lanes. The main `select!` loop
  processes `Incoming`, `Close`, and dispatch independently.
- **Event-driven dispatch:** no 1ms timer. Dispatch triggers on `Incoming`, settlement completion, and pump
  notification.
- **Bounded shutdown:** `Close` resolves within a deadline. Remaining waiters and settlements are force-resolved after
  deadline. No PHP destructor blocks indefinitely.
- **Fire-and-forget guarantee wording:** when `confirms=false`, the guarantee is "transport accepted the frame", not
  "message reached kernel socket buffer". This mode does not provide at-least-once.
- **`publish_batch()` signature preserved:** the existing `Result<Vec<PublishOutcome>, ClientError>` signature is
  unchanged. `publish_batch_detailed()` provides the indexed report as a new additive API.
- **Result ordering preserved:** both `publish_batch()` and `publish_batch_detailed()` return outcomes in the same order
  as input requests, even after grouping by broker.
- **Partial batch failure is explicit:** `publish_batch_detailed()` returns an indexed report per message. Callers
  inspect `results[i]` rather than getting an opaque global error.

## Open questions (post-implementation)

### Adaptive prefetch for consumers

After the current fixes are validated and measured, consider adding adaptive prefetch — dynamic adjustment of
`basic_qos` per subscription based on observed processing rate.

**Motivation:**

- If PHP processes fast → increase prefetch → fuller pipeline → higher throughput
- If PHP processes slowly → decrease prefetch → less memory wasted and fewer redeliveries on crash
- In a multi-subscription `ConsumerSet`, each subscription could converge to its own optimal prefetch

**Why deferred:**

- The current fixes (actor unblocking, pipelining, batch APIs) address the real bottlenecks first. Measure after those
  land before adding auto-tuning.
- `basic_qos` is a network round-trip per adjustment — the algorithm must be conservative to avoid thrashing.
- Interacts with `WeightedFairScheduler`, byte budgets, and settlement lanes — adds combinatorial test surface.

**Proposed approach (if measurements justify it):**

- **Signal:** buffer consistently empty → increase; buffer consistently full → decrease. Use existing
  `consumer_buffer_depth` metric.
- **Hysteresis:** adjust every 5-10s, minimum delta of 25%, clamp to `[min_prefetch, max_prefetch]` configurable bounds.
- **Execution:** `basic_qos` in the actor cold path (recovery or periodic check), not in the hot dispatch loop.
- **Interaction with byte budget:** prefetch is the message-count dimension; `max_buffered_bytes` is the byte dimension.
  Adaptive prefetch adjusts the count; byte budget is the hard ceiling. The effective prefetch is
  `min(adaptive_prefetch, byte_budget / avg_message_size)`.
