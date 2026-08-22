# Rabbit-rs Performance Correction Plan

**Date:** 2026-08-22
**Status:** Approved
**Scope:** Bugs + API gaps + Performance optimizations

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

## Decisions

| Decision | Choice |
|----------|--------|
| Scope | All (bugs + gaps + performance) |
| Consume callback | Preserve constraint: no PHP callbacks from Rust threads. Polling only. |
| no_ack configuration | Per-subscription (`SubscriptionConfig.no_ack`) |
| Batch ack API | `Consumer::ackMultiple(int $deliveryTag)` |
| Compatibility | Retro-compatible — new fields/methods are optional with current behavior as default |
| Organization | 3 parallel tracks (config, consume, publish) |

## Design

### Track 1: Config (prerequisite)

**`config.rs`:**
- Add `no_ack: bool` field to `SubscriptionConfig` (default: `false`)
- Validation: if `no_ack=true`, `prefetch` is ignored by the broker (no credit tracking). Log a warning if prefetch is set but no_ack is true.

**`pool/recovery_coordinator.rs`:**
- Pass `no_ack` from `SubscriptionConfig` into `ConsumerRequest` when opening consumers during recovery

### Track 2: Consume

#### 2a: no_ack mode

**`transport.rs` + `transport/lapin.rs`:**
- Add `no_ack: bool` field to `ConsumerRequest`
- `LapinConsumerChannel::consume()` passes `no_ack` to `BasicConsumeOptions`

**`consumer/set.rs`:**
- `Subscription` carries `no_ack: bool`
- `ConsumerSet::spawn()` passes `no_ack` to `ConsumerRequest`

**`consumer/actor.rs`:**
- When `no_ack=true`:
  - Do not create a `DeliveryToken` (no settlement tracking needed)
  - Do not increment `in_flight` (no budget tracking — broker auto-acks)
  - `Delivery` is created without a settlement token
  - Dispatch continues without waiting for settlements

**`delivery.rs` (PHP):**
- `Delivery::ack()`, `Delivery::release()`, `Delivery::reject()` check `no_ack` flag and return immediately (no-op, no `block_on`)
- Add `Delivery::deliveryTag(): int` accessor (needed for batch ack)

**`consumer.rs` (PHP):**
- `Consumer` stores `no_ack: bool` from consumer creation
- Pass `no_ack` to each `Delivery` object

#### 2b: Batch ack

**`consumer.rs` (PHP):**
- Add `Consumer::ackMultiple(int $deliveryTag): void` — ack all messages up to `$deliveryTag` with `multiple=true`
- Single `block_on` for the entire batch

**`consumer/actor.rs`:**
- Add `ConsumerCommand::SettleMultiple { delivery_tag: u64, settlement: Settlement, generation: u64, completed: oneshot::Sender }`
- Actor validates generation, then calls `channel.ack(delivery_tag, multiple=true).await` — single network call
- `release_budget()` for all affected deliveries (count from ledger or track running count)

**`transport.rs`:**
- `ConsumerChannel::ack()` already accepts `multiple: bool` — no change needed

**`delivery.rs` (PHP):**
- Add `Delivery::deliveryTag(): int` — returns the AMQP delivery tag from the native delivery

#### 2c: tryNextBatch

**`consumer.rs` (PHP):**
- Add `Consumer::tryNextBatch(int $max): array` — returns up to `$max` `Delivery` objects by draining the flume buffer
- Single FFI call for N deliveries
- Existing `tryNext()` and `next()` remain unchanged as the primary API

**`consumer/set.rs` + `consumer.rs` (PHP):**
- `ConsumerHandle::try_next_batch(max: usize) -> Vec<Delivery>` — loop `try_recv()` up to `max` times or until buffer empty
- Notify actor once to dispatch more deliveries

**Delivery objects:**
- Each `Delivery` in the batch is a lazy zval — `payload()` remains an individual FFI call
- Main gain is eliminating N-1 `tryNext()` FFI calls

### Track 3: Publish

#### 3a: Pipeline basic_publish

**`publisher/actor.rs`:**
- Modify `publish_queue()` to fire `channel.publish()` without awaiting between messages
- Lapin's `BasicPublish` returns a `PublisherConfirm` handle without blocking — AMQP allows pipelining on a channel
- Collect all `PublisherConfirm` handles, then push their `wait()` futures to `FuturesUnordered`
- The `buffer_capacity` semaphore (1024) already limits in-flight publishes — backpressure is preserved

Current behavior: `publish_queue()` awaits each `channel.publish()` sequentially. The actor loop blocks on each `basic_publish` frame before sending the next.

Solution: push publish futures to a `FuturesUnordered` and poll them in the actor's `select!` loop alongside confirmations. When a publish completes, insert into the ledger and push the confirm future to the confirmations set.

**Design detail — ledger insertion and replay safety:**

The current code inserts into the ledger *immediately* after the publish completes (`Ok(receipt)` block at line 562). In the pipelined version, the publish future completes asynchronously in the `select!` loop. This creates a window between "publish frame sent to socket" and "ledger entry inserted."

To preserve the at-least-once invariant, the ledger insertion must happen **before** the confirm future is polled. The pipelined `select!` arm does both atomically:

```rust
// In the select! loop:
Some((sequence, result)) = state.publish_in_flight.next() => {
    match result {
        Ok(receipt) => {
            // Ledger FIRST — guarantees entry exists before any confirm can arrive
            state.ledger.insert(sequence, InFlightPublish {
                retained,  // retained was moved into the publish future
                generation,
            });
            state.confirmations.push(Box::pin(async move {
                let result = match time::timeout_at(deadline, receipt.wait()).await {
                    Ok(result) => ConfirmationResult::Completed(result),
                    Err(_) => ConfirmationResult::TimedOut,
                };
                (sequence, generation, result)
            }));
        }
        Err(error) if error.is_recoverable() => {
            // Recover the retained publish from the future for replay
            // See "Error handling" below
        }
        Err(error) => {
            // Terminal error — complete the waiter immediately
            complete_error(retained, transport_publish_error(&error));
        }
    }
}
```

**Design detail — error handling for recoverable failures:**

When a publish future returns a recoverable error (e.g., connection lost mid-pipeline), ALL in-flight publishes must be recovered for replay, not just the one that errored. The actor must:

1. Drain `publish_in_flight` completely — every future still pending
2. Extract the `RetainedPublish` from each (the future captures ownership)
3. Push all recovered retains into `state.replay` in sequence order
4. Suspend the actor (`state.suspend(generation)`)

To make this possible, the publish future must return its `RetainedPublish` on error, not just the error:

```rust
type PublishFuture = Pin<Box<dyn Future<Output = (u64, Option<RetainedPublish>, Result<PublisherConfirm, TransportError>)>>>;

// In publish_queue():
while let Some(retained) = pending.pop_front() {
    if retained.request.deadline <= time::Instant::now() {
        complete_error(retained, PublishError::new(PublishErrorKind::Timeout, "publish deadline expired"));
        continue;
    }

    let Some(channel) = state.channel.clone() else {
        state.replay.push_back(retained);
        state.replay.extend(pending);
        return;
    };

    // ... delay topology check (awaits as before — cold path) ...

    state.sequence = state.sequence.saturating_add(1);
    let sequence = state.sequence;
    let generation = state.generation;
    let deadline = retained.request.deadline
        .min(time::Instant::now() + state.config.confirm_timeout);
    let request = into_transport_request(&retained.request, ...);

    state.publish_in_flight.push(Box::pin(async move {
        let result = channel.publish(request).await;
        (sequence, Some(retained), result)
    }));
}
```

On recoverable error in the `select!` arm:

```rust
Err(error) if error.is_recoverable() => {
    // 1. Recover this publish
    if let Some(retained) = retained_opt {
        state.replay.push_back(retained);
    }
    // 2. Drain all remaining in-flight publishes and recover them
    while let Some((_, Some(retained), _)) = state.publish_in_flight.next().await {
        state.replay.push_back(retained);
    }
    // 3. Sort replay by sequence to preserve deterministic order
    state.replay.make_contiguous().sort_by_key(|r| r.sequence);
    // 4. Suspend
    state.suspend(generation);
    return;
}
```

Note: `RetainedPublish` must carry its `sequence` for deterministic replay ordering. If it doesn't already, add a `sequence: u64` field set at enqueue time.

**Design detail — drain after push (non-blocking progress):**

Inspired by the archived `php-ext-rabbit-rs` pump pattern. After pushing a publish future into `FuturesUnordered`, immediately drain any futures that are already ready. This prevents a one-tick latency where a completed publish waits for the next `select!` poll before being processed.

```rust
// In publish_queue(), after the push loop:
while state.publish_in_flight.next().now_or_never().flatten().is_some() {
    // Process completed publishes immediately
    // (the select! arm logic is extracted into a function — see below)
}
```

To avoid duplicating the completion logic, extract the `select!` arm into a shared function:

```rust
fn handle_publish_completion(state: &mut ActorState, sequence: u64, retained_opt: Option<RetainedPublish>, result: Result<PublisherConfirm, TransportError>) {
    match result {
        Ok(receipt) => {
            state.ledger.insert(sequence, InFlightPublish { retained, generation });
            state.confirmations.push(...);
        }
        Err(error) if error.is_recoverable() => { /* replay + drain */ }
        Err(error) => { complete_error(retained, ...); }
    }
}
```

Both the `select!` arm and the `now_or_never()` drain loop call `handle_publish_completion`.

**Design detail — fire-and-forget when `confirms = false`:**

When publisher confirms are disabled (`config.confirms = false`), the publish future does not need to push anything into `state.confirmations`. The `basic_publish()` frame write to the socket is the terminal event — there is no `PublisherConfirm` to await.

Current behavior: even with `confirms = false`, `publish_queue()` pushes a confirm future that resolves immediately. This adds unnecessary `FuturesUnordered` overhead.

Optimized behavior: when `confirms = false`, the publish future completes on `channel.publish().await` and the waiter is resolved immediately in the `select!` arm — no ledger entry, no confirm future:

```rust
// In publish_queue(), branch on confirms:
if state.config.confirms {
    // Existing path: push to publish_in_flight, ledger + confirmations
    state.publish_in_flight.push(Box::pin(async move {
        let result = channel.publish(request).await;
        (sequence, Some(retained), result)
    }));
} else {
    // Fire-and-forget: resolve on socket write, no confirm tracking
    state.publish_in_flight.push(Box::pin(async move {
        let result = channel.publish(request).await;
        (sequence, Some(retained), result)
    }));
    // The select! arm for this case does NOT push to confirmations
    // and resolves the waiter immediately with PublishOutcome::Published
}
```

The `select!` arm distinguishes via `state.config.confirms`:

```rust
Some((sequence, retained_opt, result)) = state.publish_in_flight.next() => {
    match result {
        Ok(receipt) => {
            if state.config.confirms {
                // Ledger + confirm future (existing path)
                state.ledger.insert(sequence, InFlightPublish { retained, generation });
                state.confirmations.push(Box::pin(async move {
                    time::timeout_at(deadline, receipt.wait()).await
                }));
            } else {
                // Fire-and-forget: resolve immediately
                let retained = retained_opt.unwrap();
                complete_success(retained, PublishOutcome::Published);
            }
        }
        Err(error) if error.is_recoverable() => { /* replay + drain — same for both modes */ }
        Err(error) => { complete_error(retained_opt.unwrap(), transport_publish_error(&error)); }
    }
}
```

**Design detail — barrier mechanism for batch completion:**

When `publish_batch()` enqueues N messages, it needs to know when all N publish frames have been written to the socket (and, if confirms are enabled, when all N confirms have arrived) before returning to PHP.

Currently, `publish_batch()` creates `PublishWaiter` objects that resolve on confirm. With pipelining, the publish frame write and the confirm are decoupled. We need a mechanism to signal "all publishes in this batch have been sent."

Inspired by the archived `php-ext-rabbit-rs` barrier pattern (`oneshot::Sender` in `PublishJob`), add a barrier command to the actor:

```rust
enum Command {
    Publish(RetainedPublish),
    ConnectionEvent(PublisherConnectionEvent, oneshot::Sender<...>),
    Close(oneshot::Sender<()>),
    // New: barrier — signals when all in-flight publishes are completed
    FlushBarrier {
        target_sequence: u64,       // highest sequence in this batch
        completed: oneshot::Sender<()>,
    },
}
```

`publish_batch()` sends all `Command::Publish` messages, then sends `Command::FlushBarrier`. The actor processes the barrier only when `publish_in_flight` is empty AND `confirmations` has processed everything up to `target_sequence`:

```rust
// In the select! loop, new arm:
Some(Command::FlushBarrier { target_sequence, completed }) = ... => {
    state.pending_barriers.push_back((target_sequence, completed));
}
// Check barriers after each confirmation/publish completion:
fn check_barriers(state: &mut ActorState) {
    while let Some((target, _)) = state.pending_barriers.front() {
        if state.last_resolved_sequence >= *target && state.publish_in_flight.is_empty() {
            let (_, completed) = state.pending_barriers.pop_front().unwrap();
            let _ = completed.send(());
        } else {
            break;
        }
    }
}
```

This allows `publish_batch()` to:
1. Send N `Command::Publish` messages (non-blocking)
2. Send one `Command::FlushBarrier`
3. `await` the barrier oneshot — returns when all N publishes are sent (fire-and-forget) or confirmed (safe mode)

The barrier is also useful for `flush()` calls, graceful shutdown, and connection events that need to wait for the pipeline to drain.

**Summary of changes to `publish_queue()`:**

```rust
async fn publish_queue(state: &mut ActorState, mut pending: VecDeque<RetainedPublish>) {
    while let Some(retained) = pending.pop_front() {
        // 1. Deadline check (unchanged)
        // 2. Channel availability check (unchanged)
        // 3. Delay topology (unchanged — awaited, cold path)
        // 4. Sequence assignment (unchanged)

        // 5. Push publish future — NO await
        state.publish_in_flight.push(Box::pin(async move {
            let result = channel.publish(request).await;
            (sequence, Some(retained), result)
        }));

        // 6. Non-blocking drain — complete any ready futures immediately
        while let Some((seq, ret, result)) = state.publish_in_flight.next().now_or_never().flatten() {
            handle_publish_completion(state, seq, ret, result);
        }
    }
}
```

And in the `select!` loop:
```rust
// New arm:
Some((sequence, retained_opt, result)) = state.publish_in_flight.next(),
    if !state.publish_in_flight.is_empty() => {
    handle_publish_completion(state, sequence, retained_opt, result);
    state.check_barriers();
}
```

#### 3b: Cache publisher handle

**`client.rs`:**
- In `publish_batch()`, group messages by broker first, then acquire publisher handle once per broker:

```rust
let mut by_broker: HashMap<&str, Vec<PublishRequest>> = group_requests(requests);
for (broker, msgs) in by_broker {
    let publisher = self.publisher(broker).await;  // 1 lookup per broker
    for msg in msgs {
        waiters.push(publisher.try_publish(msg));
    }
}
```

- Single-message `publish()` already does 1 lookup — no change needed.

#### 3c: Flush barrier for batch completion

**`publisher/actor.rs`:**
- Add `Command::FlushBarrier` to the actor protocol (see 3a design detail above)
- Actor tracks `last_resolved_sequence` and drains pending barriers after each completion
- `client.rs` `publish_batch()` sends a barrier after the last publish command and awaits it

**`publisher/handle.rs`:**
- Add `PublisherHandle::flush_barrier(target_sequence: u64) -> impl Future<Output = ()>` helper

This replaces the current implicit synchronization (all `PublishWaiter` objects resolve) with an explicit pipeline drain signal that works correctly even when publish frames are pipelined asynchronously.

## Stub updates

`crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` — add:
- `Consumer::tryNextBatch(int $max): array`
- `Consumer::ackMultiple(int $deliveryTag): void`
- `Delivery::deliveryTag(): int`

## Testing

**Rust tests** (`crates/rabbit-rs-core/tests/`):
- Mock transport: `no_ack` mode — verify no settlement tracking, deliveries arrive without tokens
- Mock transport: `SettleMultiple` command — verify `channel.ack(tag, multiple=true)` called once
- Mock transport: pipeline publish — verify multiple `basic_publish` frames sent before any confirmation
- Mock transport: pipeline publish — verify `now_or_never()` drain completes ready futures without waiting for next `select!` tick
- Mock transport: pipeline publish with recoverable error mid-batch — verify ALL in-flight publishes are recovered into `state.replay` in sequence order, actor suspends
- Mock transport: fire-and-forget mode (`confirms=false`) — verify no confirm future pushed, waiter resolves immediately on socket write
- Mock transport: flush barrier — verify `FlushBarrier` oneshot resolves only after all publishes up to `target_sequence` are sent (fire-and-forget) or confirmed (safe mode)
- Mock transport: flush barrier with pending confirms — verify barrier waits for all confirms, not just publish frame writes
- Unit tests: `SubscriptionConfig` with `no_ack=true` validation
- Unit tests: `RetainedPublish` carries `sequence` for deterministic replay ordering

**PHP tests** (`crates/rabbit-rs-php/tests/`):
- PHPT reflection: `Consumer::tryNextBatch`, `Consumer::ackMultiple`, `Delivery::deliveryTag` exist
- PHPT functional: `ackMultiple()` with mock or real broker

**Benchmark update:**
- `RabbitRsDriver`: use `no_ack=true` for auto-ack scenario, `ackMultiple()` for batch-confirm, `tryNextBatch(256)` in consume loop
- `RabbitRsDriver`: test publish with `confirms=false` (fire-and-forget) separately from `confirms=true` (safe mode) to measure the fire-and-forget gain

## Merge order

1. **Track 1** (config) — small, unblocks Track 2
2. **Track 3a + 3b** (pipeline + cache) — independent, can merge in parallel with Track 2
3. **Track 3c** (flush barrier) — depends on 3a, merges after pipeline is validated
4. **Track 2** (consume) — largest track, merges after Track 1

## Expected results

| Metric | Current | Target | Improvement |
|--------|---------|--------|-------------|
| Publish msg/s (safe mode) | 19k | 40-60k | 2-3x (pipeline + cache + barrier) |
| Publish msg/s (fire-and-forget) | 19k | 60-80k | 3-4x (no confirm tracking overhead) |
| Consume msg/s (manual ack) | 16k | 25-30k | 1.5-2x (batch ack + tryNextBatch) |
| Consume msg/s (no_ack) | 16k | 30-40k | 2-2.5x (no_ack + tryNextBatch) |
| Consume p99 | 646ms | <300ms | 2x (fewer FFI calls) |
| Budget check | FAIL | PASS | All scenarios pass |

## Constraints preserved

- `#![forbid(unsafe_code)]` — no unsafe Rust
- Zend values not retained in Rust threads — polling only, no PHP callbacks from async runtime
- Lapin behind `Transport` abstraction — all changes go through the trait
- Bounded queues, channels, in-flight work — existing limits preserved
- Delivery tokens remain connection-generation-aware — `SettleMultiple` validates generation
- Recovery order remains deterministic — `no_ack` consumers are re-created with same option

## New invariants introduced by Track 3a (pipeline)

- **Ledger-before-confirm:** a ledger entry is inserted before the confirm future is polled. This guarantees that any confirm arriving asynchronously will find its entry. The `handle_publish_completion` function does insert-then-push atomically within the `select!` arm.
- **Replay completeness on recoverable error:** when a publish future returns a recoverable error, ALL in-flight publishes are drained from `publish_in_flight` and their `RetainedPublish` values are recovered into `state.replay`. No in-flight publish is silently dropped.
- **Deterministic replay order:** recovered publishes are sorted by `sequence` before being pushed to the replay queue. `RetainedPublish` carries its `sequence` field for this purpose.
- **Barrier correctness:** a `FlushBarrier` oneshot resolves only when `last_resolved_sequence >= target_sequence` AND `publish_in_flight.is_empty()`. This guarantees the caller that all publishes (and their confirms, if enabled) have completed before proceeding.
- **Fire-and-forget waiters resolve on socket write:** when `confirms=false`, the waiter resolves immediately when `channel.publish().await` completes. No confirm future is created. The at-least-once contract for fire-and-forget is "message reached kernel socket buffer" — same guarantee as amqplib's fire-and-forget.
