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

```rust
// In ActorState:
publish_in_flight: FuturesUnordered<PublishFuture>

// In the select! loop:
Some((sequence, result)) = state.publish_in_flight.next() => {
    match result {
        Ok(receipt) => {
            state.ledger.insert(sequence, InFlightPublish { ... });
            state.confirmations.push(Box::pin(async move {
                time::timeout_at(deadline, receipt.wait()).await
            }));
        }
        Err(e) => { /* handle error */ }
    }
}
```

In `publish_queue()`, instead of awaiting each publish:
```rust
while let Some(retained) = pending.pop_front() {
    let sequence = state.next_sequence();
    state.publish_in_flight.push(Box::pin(async move {
        let result = channel.publish(request).await;
        (sequence, result)
    }));
}
```

This allows multiple `basic_publish` frames to be in-flight simultaneously — the AMQP channel buffers and sends frames without waiting for confirms. The `buffer_capacity` semaphore (1024) still limits total in-flight publishes.

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
- Unit tests: `SubscriptionConfig` with `no_ack=true` validation

**PHP tests** (`crates/rabbit-rs-php/tests/`):
- PHPT reflection: `Consumer::tryNextBatch`, `Consumer::ackMultiple`, `Delivery::deliveryTag` exist
- PHPT functional: `ackMultiple()` with mock or real broker

**Benchmark update:**
- `RabbitRsDriver`: use `no_ack=true` for auto-ack scenario, `ackMultiple()` for batch-confirm, `tryNextBatch(256)` in consume loop

## Merge order

1. **Track 1** (config) — small, unblocks Track 2
2. **Track 3** (publish) — independent, can merge in parallel with Track 2
3. **Track 2** (consume) — largest track, merges after Track 1

## Expected results

| Metric | Current | Target | Improvement |
|--------|---------|--------|-------------|
| Publish msg/s | 19k | 40-60k | 2-3x (pipeline + cache) |
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
