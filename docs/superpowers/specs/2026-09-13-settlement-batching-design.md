# Settlement Batching in the Consumer Actor

Date: 2026-09-13
Issue: [#282](https://github.com/Goopil/php-rabbit-rs/issues/282) (manual-ack decomposition)
Status: approved design

## Context

The attribution pass measured the ~30 µs/message overhead of the manual-ack
mode versus the 173k msg/s no_ack supply. The overhead is the per-ack
settlement machinery end to end: one spawned settlement task per ack
(`launch_settlement`), one scheduler wake, one cross-thread semaphore signal
to lapin-io, one socket write, plus dispatch accounting. The transport
already exposes `ack(delivery_tag, multiple)` and the actor already contains
a validated contiguous-prefix multi-ack path (`handle_settle_through` +
`validate_contiguous_prefix`) with per-channel queues and in-flight
serialization — it is simply not fed by the per-message ack path.

## Goal

Coalesce plain acknowledgements into contiguous-prefix bursts so a burst of
N acks costs one scheduler wake, one lapin-io wake, and one socket write
instead of N of each. Target: lift the manual-ack ceiling from ~25k toward
the measured 173k supply reference.

## Non-Goals

- No PHP, FFI, or driver changes: `try_ack` remains a fire-into-channel
  call; the pop loop is untouched.
- Non-ack settlements (reject, release, delayed release, poison) keep the
  per-item path — they are rare and semantically varied.
- No dispatch-accounting batching (the remaining ~5 µs; second-order, later
  cycle).
- No threshold/timer knobs: batching is drain-driven, no added latency.

## Design

All changes live in `crates/rabbit-rs-core/src/consumer/actor.rs`.

### Ack recording

`handle_settle` with `Settlement::Ack` no longer launches a settlement. It
marks the ledger entry `Acked` and the token's logical state `Acked` at
recording time (from the consumer's perspective the settlement is complete)
and records the delivery tag in a small per-channel `acked_pending` set.
If a later wire flush fails terminally, the existing `SettlementError`
reporting surfaces it; the broker then redelivers on connection recovery —
the same observable outcome as a failed individual ack today.

### Flush point

A `flush_acked` step runs at the end of each actor select iteration, after
the ready-command drain. Per channel:

1. Walk the ledger's contiguous prefix of `Acked` entries; the highest
   contiguous delivery tag is the watermark.
2. If the watermark advanced: launch one settle_through (existing
   `launch_settle_through` machinery → `ack(watermark, multiple=true)`).
   The contiguous-prefix validation, per-channel queues, and in-flight
   serialization are reused unchanged.
3. Acked entries beyond a hole (out-of-order user acks) flush immediately
   as individual `ack(tag, multiple=false)` settlements — today's behavior
   for them, zero added delay. This choice was approved over holding until
   the hole fills: a hole must never delay downstream acks on the wire.

In the common sequential case the watermark advances every iteration, so
the flush emits exactly one burst per wake.

### Close-time sweep

`close_set` drains `Settle` commands through the same recording path before
the bounded flush, so coalescing applies at shutdown too; the existing
500 ms drain budget bounds the exit.

### Guarantees

- Only the contiguous prefix is acked on the wire with `multiple=true` —
  the transport never acks more than the consumer acknowledged.
- Buffers stay bounded: `acked_pending` is bounded by the ledger, which is
  bounded by the in-flight budget and prefetch.
- Delivery tokens and generation awareness unchanged; a failed or
  generation-stale flush follows the existing `SettlementError` paths
  (terminal Transport/StaleGeneration reporting).
- Crash semantics identical to today's worst case: an ack recorded but not
  yet flushed is redelivered after a crash (at-least-once preserved).

## Testing

- Focused new test: sequential ack burst produces one wire
  `Ack{multiple: true}`; a hole produces immediate individual
  `Ack{multiple: false}` for downstream acks; the watermark resumes after
  the hole fills.
- Close-time test: acked-but-unflushed deliveries land within the bounded
  drain.
- Existing tests asserting per-message `Ack{multiple: false}` operations
  will observe batched operations and are adjusted case by case; the
  at-least-once and recovery suites must pass unchanged in intent.
- The `consumer_delivery` divan bench (CodSpeed-tracked) measures the
  effect on the full pop+ack cycle.

## Expected Outcome

Per-burst cost drops from 3 wakeups + 1 write per message to 3 wakeups + 1
write per burst. Combined with the measured supply ceiling (173k msg/s),
the realistic manual-ack target is 60-100k msg/s, recorded as lead 1 in
#282's re-ranking.
