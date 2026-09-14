# Consumer Backpressure Reliability Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the three confirmed consumer reliability defects from the 2026-09-14 external audit: control-command starvation under backpressure, permanent head-of-line blocking by oversized messages, and duplicate `basic_ack` frames in `early_ack` mode.

**Architecture:** All three defects live in the consumer actor (`crates/rabbit-rs-core/src/consumer/actor.rs`). The root cause of #1 is a single multiplexed command channel whose recv arm is gated by `pending_incoming` capacity; the fix splits a dedicated, always-polled control channel. #2 is the absence of a terminal path for deliveries whose size alone exceeds `max_buffered_bytes`; the fix routes them through the existing poison settlement contract. #3 is an unconditional network ack in the `early_ack` dispatch path that re-fires on re-dispatch; the fix is a bounded per-channel record of already-acked tags. Each fix is preceded by a deterministic failing repro test using the mock transport and paused Tokio time.

**Tech Stack:** Rust 1.96 (edition 2024), tokio (paused time in tests), mock transport (`MockTransport`), `cargo-nextest`.

## Global Constraints

- No `unsafe` — `#![forbid(unsafe_code)]` and workspace lint config must not be weakened.
- At-least-once delivery: silent loss is unacceptable; duplicates must remain measurable.
- Bounded structures only: any new collection must have a bounded capacity and a documented bound.
- Typed errors with actionable context; never credentials or full broker URIs in logs/errors.
- Tests use paused Tokio time (`#[tokio::test(start_paused = true)]`) and the scriptable mock transport; no real sleeps.
- Keep Lapin behind the `Transport` abstraction — no lapin-specific code in the actor.
- English only in code, comments, docs, commit messages.
- After every Rust edit: `rtk cargo fmt --all`. Before claiming any task complete: run its focused tests.
- Final gate before declaring done: `rtk ./scripts/check.sh`.

## Verified Evidence (from the 2026-09-14 audit confrontation)

| # | Defect | Verdict | Key evidence |
|---|--------|---------|--------------|
| 1 | Control starvation under backpressure | CONFIRMED | `actor.rs:621-623`: the `receiver.recv()` select arm is gated by `state.pending_incoming.len() < state.pending_capacity`, which disables the *entire* command channel (Settle, SettleThrough, GetPrefetchStats included). Budget (`buffered_bytes`) is only released at wire-settlement completion (`actor.rs:714-717`, `:848-868`), which requires consuming Settle commands → self-sustaining deadlock. Precedent: `close_rx` was already moved off the command channel for this exact reason (`actor.rs:641-644`, `set.rs:390-392`). No test covers Settle at full pending capacity. |
| 2 | Message larger than `max_buffered_bytes` | CONFIRMED | `actor.rs:939-951`: `current + delivery_bytes > *max` is always true when `delivery_bytes > *max` → pushed to `pending_incoming` forever; `drain_pending` breaks at the head (`actor.rs:533-543`); `pending_incoming` is a single cross-subscription deque (`actor.rs:120`) → HOL blocking for all subscriptions. No reject/bypass path exists. No test. |
| 3 | Early ACK re-fired on re-dispatch | CONFIRMED (opt-in mode only; default `early_ack=false`) | `actor.rs:363-392`: network ack spawned before the PHP `try_send`; on send failure the delivery is re-pushed (`:385-391`) and the next dispatch pass spawns a *second* `channel.ack(tag)` for the same tag → RabbitMQ closes the channel with 406 PRECONDITION_FAILED. No per-delivery ack-state tracking on this path. |
| 4 | `ackThrough` over an in-flight lower tag | PARTIAL — internal path safe (`flush_acked`, `actor.rs:1099-1126` uses a contiguous *settled* prefix); public API documents caller responsibility (`composite.rs:165-166`) | OUT OF SCOPE (separate plan if the public API must be tightened). |
| 5 | Generation fencing | MAIN PATHS CORRECT (`ensure_live_generation`, `actor.rs:1358-1376`, checks connection generation + channel id); `spawn_poison_settlement` (`actor.rs:1545-1561`) skips fencing; the test named `..._rejects_stale_acks` (`recovery.rs:291`) tests no such thing | IN SCOPE (Task 5, hardening). |
| 6 | Publisher confirm timeout = silent loss | REFUTED — mechanism is as described but documented deliberate design (`docs/reference.md:357`); caller receives typed `Timeout`, never silent success | OUT OF SCOPE. |
| 7 | Mandatory return vs confirm race | REFUTED — single state machine, return precedence implemented and tested (`tests/publisher.rs:273`), unique completion proptest-verified | OUT OF SCOPE. |

---

### Task 1: Repro test — control starvation under backpressure

**Files:**
- Test: `crates/rabbit-rs-core/tests/consumer.rs` (append near the flood test, ~line 1163)

**Interfaces:**
- Consumes: existing test helpers (`MockTransport`, `subscription(...)`, `connection_key(...)`, `delivery(tag, payload)`, `let_sources_fill()`), `ConsumerSet::spawn_with_metrics`, `transport.operations()` returning `Vec<TransportOperation>`, `TransportOperation::Ack { delivery_tag, .. }`.
- Produces: a failing test named `settle_progresses_while_pending_incoming_is_saturated` (fixed by Task 2).

- [ ] **Step 1: Write the failing test**

Pattern after `no_ack_flood_is_bounded_and_every_delivery_still_arrives` (`tests/consumer.rs:1107-1163`):

```rust
/// Regression (audit 2026-09-14 #1): when `pending_incoming` is saturated,
/// the command channel must stay drained — a PHP settlement must still reach
/// the wire and reopen the gate. Before the control-channel fix, the gated
/// `receiver.recv()` arm starves Settle commands and the actor deadlocks.
#[tokio::test(start_paused = true)]
async fn settle_progresses_while_pending_incoming_is_saturated() {
    let transport = MockTransport::default();
    transport.keep_delivery_stream_open();
    let mut sub = subscription(&transport, "sat", connection_key("sat", "/"), 1).await;
    // Budget holds exactly one payload: after the first delivery dispatches,
    // every further delivery is over-budget and lands in `pending_incoming`.
    sub = sub.max_buffered_bytes(7);
    let consumer = ConsumerSet::spawn_with_metrics(vec![sub], Metrics::default())
        .await
        .expect("consumer set");

    let_sources_fill().await;
    let held = consumer.next().await.expect("first delivery dispatched");
    assert_eq!(held.delivery_tag(), 1);

    // Saturate `pending_incoming` (capacity = max(256, total prefetch)) with
    // over-budget deliveries so the incoming gate closes.
    for tag in 2..=512u64 {
        transport.push_delivery(Ok(delivery(tag, b"payload")));
    }
    for _ in 0..500 {
        tokio::time::advance(Duration::from_millis(2)).await;
    }

    // The embedder settles the held delivery. This must reach the wire even
    // though the incoming gate is closed.
    held.ack().await.expect("settlement accepted");

    let mut acked = false;
    for _ in 0..500 {
        tokio::time::advance(Duration::from_millis(2)).await;
        acked = transport.operations().iter().any(
            |op| matches!(op, TransportOperation::Ack { delivery_tag: 1, .. }),
        );
        if acked {
            break;
        }
    }
    assert!(acked, "settlement must reach the wire while pending_incoming is saturated");

    // Liveness: freeing the budget must make flooded deliveries dispatchable.
    let mut dispatched = 0;
    for _ in 0..2_000 {
        tokio::time::advance(Duration::from_millis(2)).await;
        dispatched = consumer.metrics_snapshot().deliveries_total;
        if dispatched > 1 {
            break;
        }
    }
    assert!(dispatched > 1, "drained budget must reopen the gate and dispatch flooded deliveries");

    consumer.close().await.expect("close");
}
```

Adapt helper names/signatures to what the file actually provides (read `tests/consumer.rs` helpers first). If `subscription(...)` does not expose a `max_buffered_bytes` builder on the resulting `Subscription`, mirror the `WorkerProfile`-based construction used by the flood test (`tests/consumer.rs:1118-1131`).

- [ ] **Step 2: Run the test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer settle_progresses_while_pending_incoming_is_saturated`
Expected: FAIL — `settlement must reach the wire while pending_incoming is saturated` (the Settle command is never consumed because the recv arm is gated).

- [ ] **Step 3: Commit the repro**

```bash
git add crates/rabbit-rs-core/tests/consumer.rs
git commit -m "test(core): reproduce control starvation under consumer backpressure"
```

---

### Task 2: Fix — dedicated control channel, gate `Incoming` only

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (select loop ~600-700, drain at loop top ~600, command enum ~76-91)
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs` (channel construction ~26, ~211-213, pump wiring ~309-320, `try_settle` senders)
- Modify: `crates/rabbit-rs-core/src/consumer/delivery.rs` (token command-sender routing ~306-341)
- Modify: `crates/rabbit-rs-core/src/consumer/composite.rs` (if it forwards command senders)

**Interfaces:**
- Consumes: `ConsumerCommand` variants `Settle { token, settlement }`, `SettleThrough { token }`, `GetPrefetchStats { completed }` (`actor.rs:81-90`).
- Produces: `ControlCommand` enum with those three variants; the actor now owns two receivers: `incoming_rx` (gated, `ConsumerCommand::Incoming` only) and `control_rx` (never gated, `ControlCommand` only). Delivery tokens and stats handles route through the control sender.

- [ ] **Step 1: Introduce the control command type and channels**

In `actor.rs`, add next to `ConsumerCommand`:

```rust
/// Control-plane commands. These are never gated by backpressure: they are
/// what frees the byte budget and what the embedder uses to observe and stop
/// the consumer. They ride a dedicated channel so a saturated
/// `pending_incoming` cannot starve them (audit 2026-09-14 #1, same lesson as
/// the `close_rx` watch channel).
pub(crate) enum ControlCommand {
    Settle {
        token: Arc<DeliveryTokenInner>,
        settlement: Settlement,
    },
    SettleThrough {
        token: Arc<DeliveryTokenInner>,
    },
    GetPrefetchStats {
        completed: oneshot::Sender<Vec<PrefetchStat>>,
    },
}
```

Remove the three variants from `ConsumerCommand` (it keeps only `Incoming`). In the actor loop, add `control_rx: mpsc::Receiver<ControlCommand>` alongside `receiver`. Select arms:

```rust
tokio::select! {
    biased;
    command = control_rx.recv() => { /* existing Settle/SettleThrough/GetStats handlers, matched on ControlCommand */ }
    command = receiver.recv(), if state.pending_incoming.len() < state.pending_capacity => {
        // Incoming-only handling (unchanged)
    }
    /* pending_settlements, pending_settle_throughs, dispatch_notify,
       close_rx.changed(), adaptive prefetch tick arms unchanged */
}
```

The loop-top drain must poll **both** receivers: `control_rx.try_recv()` ungated (loop until `Empty`), `receiver.try_recv()` only while `pending_incoming.len() < state.pending_capacity` (unchanged gate, `actor.rs:600`).

- [ ] **Step 2: Route senders**

In `set.rs`: construct both channels with capacity `COMMAND_CAPACITY` (256). Pumps keep `commands.send(ConsumerCommand::Incoming)` (`set.rs:309-320`) on the incoming channel. `try_settle` / `try_settle_through` / `get_prefetch_stats` (currently `delivery.rs:306-316`, `set.rs`) send `ControlCommand` on the control channel with `try_send` (unchanged error semantics: `ChannelFull` reverts token state to `Pending`). Tokens hold `control_tx` (`delivery.rs:339` routing). Update `handle_settle`, `handle_settle_through`, and the stats arm to match `ControlCommand`. Keep `close_rx` exactly as is.

- [ ] **Step 3: Make the repro pass, keep the suite green**

Run: `rtk cargo test -p rabbit-rs-core --test consumer settle_progresses_while_pending_incoming_is_saturated`
Expected: PASS.

Run: `rtk cargo nextest run -p rabbit-rs-core --test consumer --test recovery --test pool_clear --test transport_liveness`
Expected: all PASS (in particular `drop_with_saturated_command_channel_still_closes_the_actor`, `set.rs:621`, and `no_ack_flood_is_bounded_and_every_delivery_still_arrives`, `tests/consumer.rs:1107`).

- [ ] **Step 4: Liveness property note**

Add a short doc comment on `ControlCommand` stating the liveness invariant: *every control command accepted by the embedder is eventually consumed, regardless of delivery backpressure.* This is the falsifiable property the audit demanded.

- [ ] **Step 5: Format, commit**

```bash
rtk cargo fmt --all
git add -A crates/rabbit-rs-core/src/consumer crates/rabbit-rs-core/tests/consumer.rs
git commit -m "fix(core): keep consumer control commands alive under delivery backpressure"
```

---

### Task 3: Repro + fix — oversized message settles terminally (poison contract)

**Files:**
- Test: `crates/rabbit-rs-core/tests/consumer.rs` (append after Task 1's test)
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (`handle_incoming` over-budget branch ~939-951, `drain_pending` ~533-543, near `settle_poison` ~479-530)
- Modify: `docs/reference.md` (consumer section: oversized contract + ops note)

**Interfaces:**
- Consumes: `settle_poison(...)` (`actor.rs:488-522`), `spawn_poison_settlement` semantics (DLX → `Reject{requeue:false}`; no DLX → `Ack` + logged typed error), `has_dead_letter` on `RuntimeSubscription` (`actor.rs:104`), `poison_settlement_message(detail, message_id, has_dead_letter)`.
- Produces: oversized deliveries (`delivery_bytes > max_buffered_bytes` alone) are settled terminally at arrival — never queued into `pending_incoming`; byte accounting stays consistent; PHP surfaces a typed settlement error whose message contains `"exceeds max_buffered_bytes"`.

**Design decision (validated with the project owner):** the terminal poison contract, NOT pause and NOT requeue.
- Pause does not help: the oversized delivery is already in actor memory at the head of the shared `pending_incoming` deque; only terminal settlement of *that* message unblocks the head-of-line.
- Raw requeue is a hot loop: RabbitMQ re-delivers an unchanged message immediately, forever (deterministic condition → infinite retry). Delayed requeue (TTL retry queues / quorum `x-delivery-limit`) is an application pattern, not a transport behavior.
- This matches Spring AMQP's `ConditionalRejectingErrorHandler` (reject-don't-requeue → DLQ) and Kafka's `RecordTooLargeException` philosophy: too-big = loud terminal failure.

- [ ] **Step 1: Write the failing tests**

```rust
/// Regression (audit 2026-09-14 #2): a delivery whose size alone exceeds
/// `max_buffered_bytes` can never satisfy the capacity predicate; it must be
/// settled terminally (poison contract) instead of blocking the shared
/// `pending_incoming` deque head-of-line forever.
#[tokio::test(start_paused = true)]
async fn oversized_message_is_settled_terminally_and_never_blocks_the_pipeline() {
    let transport = MockTransport::default();
    transport.keep_delivery_stream_open();
    let mut sub = subscription(&transport, "big", connection_key("big", "/"), 4).await;
    sub = sub.max_buffered_bytes(8); // deliberately smaller than "large_payload"
    let consumer = ConsumerSet::spawn_with_metrics(vec![sub], Metrics::default())
        .await
        .expect("consumer set");

    const LARGE: &[u8] = b"0123456789abcdef"; // 16 bytes > 8-byte budget
    transport.push_delivery(Ok(delivery(1, LARGE)));   // oversized
    transport.push_delivery(Ok(delivery(2, b"tiny"))); // must still flow
    transport.push_delivery(Ok(delivery(3, b"tiny")));

    let d2 = consumer.next().await.expect("small delivery 2 dispatches");
    let d3 = consumer.next().await.expect("small delivery 3 dispatches");
    assert_eq!(d2.delivery_tag(), 2);
    assert_eq!(d3.delivery_tag(), 3);

    // Terminal settlement on the wire: Reject(requeue=false) when a dead-letter
    // target is configured; Ack (ack-and-log) otherwise — mirroring
    // `settle_poison`'s documented policy.
    let ops = transport.operations();
    let rejected = ops.iter().any(|op| matches!(op,
        TransportOperation::Reject { delivery_tag: 1, requeue: false, .. }));
    let acked = ops.iter().any(|op| matches!(op,
        TransportOperation::Ack { delivery_tag: 1, .. }));
    assert!(rejected || acked, "oversized delivery must be settled terminally, never parked");

    // The embedder observes a typed settlement error.
    let errors = consumer.drain_errors();
    assert!(errors.iter().any(|e| e.message.contains("exceeds max_buffered_bytes")),
        "oversized settlement must surface a typed error");

    // Byte accounting invariant (audit hardening): buffered_bytes == sum of
    // sizes actually held for this subscription.
    let snapshot = consumer.metrics_snapshot();
    assert!(snapshot.buffered_bytes <= 8, "held bytes must stay within the budget");

    consumer.close().await.expect("close");
}
```

Check the actual `TransportOperation::Reject` variant shape and the `metrics_snapshot()` / `drain_errors()` accessor names in the existing tests and adapt. If a no-DLX and a with-DLX variant require different config (the default helper config has `dead_letter: None`), assert the `Ack` path in this test and add a second test configuring a dead-letter target asserting `Reject { requeue: false }` — copy the config pattern from an existing dead-letter test if one exists, otherwise construct `Config { dead_letter: Some(...) }` following `connection_key()`'s builder in `tests/consumer.rs:55-72`.

- [ ] **Step 2: Run to verify failure**

Run: `rtk cargo test -p rabbit-rs-core --test consumer oversized_message`
Expected: FAIL — small deliveries never dispatch (head-of-line blocked), oversized never settled.

- [ ] **Step 3: Implement the terminal path**

In `handle_incoming` (over-budget branch, `actor.rs:939-951`):

```rust
if over_budget {
    if delivery_bytes > *max {
        // A delivery whose size alone exceeds the budget can never satisfy
        // the capacity predicate: settle it terminally via the poison
        // contract instead of parking it forever (audit 2026-09-14 #2).
        self.settle_oversized(&subscription, delivery);
        return; // or continue, matching the surrounding control flow
    }
    state.pending_incoming.push_back((subscription, delivery));
    state.metrics.record_backpressure();
}
```

In `drain_pending` (`actor.rs:533-543`), when the head is over budget, distinguish oversized (terminal) from merely over-budget (break):

```rust
if over_budget {
    if delivery_bytes > *max {
        self.settle_oversized(&subscription, delivery);
        self.pending_incoming.pop_front();
        continue;
    }
    break;
}
```

Add `settle_oversized`, mirroring `settle_poison` but keeping byte accounting exact:

```rust
/// Settles a delivery whose size alone exceeds the subscription's byte
/// budget. Reuses the poison contract: `reject(requeue=false)` toward the
/// DLX when one is configured, otherwise an explicit acknowledge recorded as
/// a typed settlement error (`MaxAttempts` kind, `Oversized` detail). The
/// delivery was parked in `pending_incoming` without being counted in
/// `buffered_bytes`, so it is counted then released to keep
/// `buffered_bytes` equal to the sum of held sizes.
fn settle_oversized(&mut self, subscription: &SubscriptionId, delivery: TransportDelivery) {
    let payload_bytes = u64::try_from(delivery.payload.len()).unwrap_or(u64::MAX);
    if let Some(bytes) = self.buffered_bytes.get_mut(subscription) {
        *bytes = bytes.saturating_add(payload_bytes); // count-then-release
    }
    let has_dead_letter = self
        .subscriptions
        .get(subscription)
        .map(|runtime| runtime.has_dead_letter)
        .unwrap_or(false);
    let message_id = delivery.message_id.clone().unwrap_or_default();
    self.settle_poison(
        subscription,
        has_dead_letter,
        delivery.delivery_tag,
        &message_id,
        payload_bytes,
        "message size exceeds max_buffered_bytes",
        Duration::ZERO,
    );
}
```

Verify the exact `message_id` type on `TransportDelivery` (`Option<MessageId>` per `tests/consumer.rs:80`) and adapt (`unwrap_or_default` or an explicit placeholder that `poison_settlement_message` accepts — check how the MaxAttempts completion arm builds `message_id`, `actor.rs:765-768`). Do not add a new `ConsumerErrorKind` variant — reuse `MaxAttempts` with the oversized detail (YAGNI); the detail string is the discriminator.

- [ ] **Step 4: Make tests pass, run the focused suite**

Run: `rtk cargo test -p rabbit-rs-core --test consumer oversized_message && rtk cargo nextest run -p rabbit-rs-core --test consumer`
Expected: PASS, no regressions (in particular the flood test's boundedness assertion).

- [ ] **Step 5: Document the contract**

In `docs/reference.md`, consumer section, add:

> Deliveries whose size alone exceeds `max_buffered_bytes` are settled terminally using the poison policy: `basic.reject(requeue=false)` toward the dead-letter exchange when one is configured, otherwise an explicit acknowledge with a typed settlement error. They are never requeued and never parked indefinitely. Operators should align the broker's `max_message_size` with consumer `max_buffered_bytes` so oversized payloads are rejected at publish time; the consumer-side terminal path is the last line of defense.

- [ ] **Step 6: Format, commit**

```bash
rtk cargo fmt --all
git add crates/rabbit-rs-core/src/consumer/actor.rs crates/rabbit-rs-core/tests/consumer.rs docs/reference.md
git commit -m "fix(core): settle oversized deliveries terminally instead of blocking pending_incoming"
```

---

### Task 4: Repro + fix — early ACK fires at most once per delivery tag

**Files:**
- Test: `crates/rabbit-rs-core/tests/consumer.rs` (append near `early_ack_acks_before_dispatch_to_buffer`, ~line 1513)
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (early-ack dispatch branch ~363-392)

**Interfaces:**
- Consumes: early-ack dispatch block (`actor.rs:363-392`): ack spawned before `buffer_tx.try_send`, re-push on failure at `:385-391`.
- Produces: at most one `TransportOperation::Ack` per delivery tag per channel generation, even across repeated failed dispatch attempts.

- [ ] **Step 1: Write the failing test**

```rust
/// Regression (audit 2026-09-14 #3): in `early_ack` mode the network ack is
/// spawned before the PHP handoff. If the handoff fails, the delivery is
/// re-dispatched — the same tag must NOT be acked on the wire a second time
/// (RabbitMQ answers the duplicate with PRECONDITION_FAILED / 406).
#[tokio::test(start_paused = true)]
async fn early_ack_fires_at_most_once_per_delivery_tag_across_redispatches() {
    let transport = MockTransport::default();
    transport.keep_delivery_stream_open();
    let mut sub = subscription(&transport, "once", connection_key("once", "/"), 1).await;
    sub = sub.early_ack(true).max_buffered_bytes(8);
    // NOTE: make the PHP-side buffer full so the first try_send fails and the
    // delivery is re-pushed. Use the smallest embedder buffer the test
    // helpers expose (check how other tests saturate buffer_tx; if no direct
    // knob exists, call next() zero times and rely on the buffer capacity
    // being 1 so the second delivery overflows — adapt to the real API).
    let consumer = ConsumerSet::spawn_with_metrics(vec![sub], Metrics::default())
        .await
        .expect("consumer set");

    transport.push_delivery(Ok(delivery(1, b"payload")));
    // Force at least two dispatch passes over the same delivery: fill the
    // embedder buffer with another delivery so the first one is re-pushed,
    // then free the buffer and let it re-dispatch.
    for _ in 0..500 {
        tokio::time::advance(Duration::from_millis(2)).await;
    }

    let ack_count = transport.operations().iter()
        .filter(|op| matches!(op, TransportOperation::Ack { delivery_tag: 1, .. }))
        .count();
    assert!(ack_count >= 1, "delivery must have been acked");
    assert_eq!(ack_count, 1, "early ack must fire at most once per delivery tag");

    consumer.close().await.expect("close");
}
```

The key engineering here is forcing a failed dispatch deterministically. Inspect how `buffer_tx` capacity is configured (`ConsumerSet`/flume buffer construction in `set.rs`) and choose the simplest deterministic saturation: either (a) spawn with an embedder buffer of capacity 1, push tag 1 and tag 2 so tag 1's dispatch fails after its ack, then drain tag 2 and observe tag 1's re-dispatch; or (b) if the buffer is not configurable from the test, drop the PHP receiver handle to force `try_send` failures. Pick whichever the existing helpers support; do not add sleeps.

- [ ] **Step 2: Run to verify failure**

Run: `rtk cargo test -p rabbit-rs-core --test consumer early_ack_fires_at_most_once`
Expected: FAIL — `ack_count == 2` (the ack re-fires on re-dispatch).

- [ ] **Step 3: Implement the ack-once guard**

Add a bounded per-channel record of early-acked tags to the actor state:

```rust
/// Delivery tags early-acked on the wire, per channel, in dispatch order.
/// Bounded: re-dispatch of an early-acked delivery only happens while the
/// delivery is still held in actor memory (`self.buffers` / re-push), so a
/// ring sized to the pending bound is always sufficient. Guards against a
/// second `basic_ack` for the same tag (channel 406, audit 2026-09-14 #3).
early_acked_tags: HashMap<ChannelKey, VecDeque<u64>>,
```

Capacity: `max(pending_capacity, total_prefetch)` — same bound the actor already uses for in-flight work. In the early-ack branch:

```rust
if early_ack {
    let delivery_bytes = u64::try_from(delivery.payload.len()).unwrap_or(u64::MAX);
    if !no_ack && !self.early_acked(tag, &channel_key) {
        self.remember_early_acked(tag, &channel_key);
        let tag = delivery.delivery_tag;
        tokio::spawn(async move {
            let _ = channel.ack(tag, false).await;
        });
    }
    // ... rest unchanged
```

with

```rust
fn early_acked(&self, tag: u64, key: &ChannelKey) -> bool {
    self.early_acked_tags.get(key).is_some_and(|ring| ring.contains(&tag))
}

fn remember_early_acked(&mut self, tag: u64, key: &ChannelKey) {
    let ring = self.early_acked_tags.entry(key.clone()).or_default();
    if ring.len() >= self.early_acked_capacity {
        ring.pop_front();
    }
    ring.push_back(tag);
}
```

Clear the ring when the channel/generation is torn down (follow wherever `channel_ledgers` entries are removed — same lifecycle). If `channel_key` is not available in `dispatch()` where the spawn happens, derive it the same way the settlement executors do (`channel_key_for`, `actor.rs:511`).

- [ ] **Step 4: Make tests pass**

Run: `rtk cargo test -p rabbit-rs-core --test consumer early_ack && rtk cargo nextest run -p rabbit-rs-core --test consumer`
Expected: new test PASS; existing `early_ack_*` tests still PASS (they assert exactly 1 ack on the happy path).

- [ ] **Step 5: Format, commit**

```bash
rtk cargo fmt --all
git add crates/rabbit-rs-core/src/consumer/actor.rs crates/rabbit-rs-core/tests/consumer.rs
git commit -m "fix(core): fire early ack at most once per delivery tag across re-dispatches"
```

---

### Task 5: Hardening — fence the poison settlement path + real stale-generation test

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (`settle_poison` ~488-522, the MaxAttempts completion arm ~765-768, `spawn_poison_settlement` ~1545-1561)
- Test: `crates/rabbit-rs-core/tests/recovery.rs` (rename test at ~291; add a real stale-ack test)

**Interfaces:**
- Consumes: `ensure_live_generation(connection_key, generation, channel_id, token)` (`actor.rs:1358-1376`), `RuntimeSubscription { connection_key, generation, channel_id, .. }` (`actor.rs:93-97`).
- Produces: `spawn_poison_settlement` is only called for live generations; a stale token settled after recovery yields `StaleGeneration` with **zero** wire operations.

- [ ] **Step 1: Rename the misleading test**

`recovery.rs:291` `consumer_generation_updates_after_reconnection_rejects_stale_acks` asserts only the generation bump. Rename to `consumer_generation_increments_after_reconnection` and adjust any references.

- [ ] **Step 2: Write the real stale-ack rejection test**

Following the RecoveryCoordinator patterns already in `tests/recovery.rs` (force a reconnection so generation goes N → N+1, keep a delivery token captured before the reconnection), then:

```rust
/// Regression (audit 2026-09-14 #5): a token from generation N settled after
/// the connection recovered to N+1 must be rejected with `StaleGeneration`
/// and produce ZERO wire operations — never an ack on the new channel.
#[tokio::test(start_paused = true)]
async fn stale_generation_token_is_rejected_without_any_wire_settlement() {
    // ... build consumer set via RecoveryCoordinator, capture token (gen 1)
    // ... force reconnection (mock transport drops / coordinator recovers)
    let err = stale_token.ack().await.expect_err("stale token must fail");
    assert_eq!(err.kind(), ConsumerErrorKind::StaleGeneration);
    let ack_count = transport.operations().iter()
        .filter(|op| matches!(op, TransportOperation::Ack { .. }))
        .count();
    assert_eq!(ack_count, 0, "stale token must never reach the wire");
}
```

Copy the reconnection-forcing pattern from the existing recovery tests (they use mock transport disconnects + paused time). If the public token API cannot produce a stale-generation token through the coordinator, construct the scenario the executors defend against: a `DeliveryTokenInner` captured pre-recovery is the natural way; adapt to what `tests/recovery.rs` already does for stale handles (`recovery.rs:325`, `:1049`).

- [ ] **Step 3: Run to verify current behavior**

Run: `rtk cargo test -p rabbit-rs-core --test recovery stale_generation_token_is_rejected`
Expected: likely PASS already for the main executors (they fence) — this test pins the invariant. If it fails, fix `execute_settlement` fencing first before proceeding.

- [ ] **Step 4: Fence the poison path**

In `settle_poison` and the MaxAttempts completion arm, before `spawn_poison_settlement`, validate the generation the same way the executors do:

```rust
if let Some(runtime) = self.subscriptions.get(subscription) {
    if runtime.generation == token_generation
        && runtime.connection_key == token_connection_key
    {
        spawn_poison_settlement(Arc::clone(&runtime.channel), has_dead_letter, delivery_tag);
    }
    // Stale: skip the wire op; the typed settlement error below still
    // surfaces the poison condition and the broker redelivers.
}
```

`settle_poison` currently receives only `delivery_tag` — thread the token's generation/connection key through from both call sites (`settle_poison` caller `:502-504` and MaxAttempts arm `:765-768`), or pass the token itself. Choose the smaller diff.

- [ ] **Step 5: Full consumer + recovery suite, format, commit**

```bash
rtk cargo nextest run -p rabbit-rs-core --test recovery --test consumer
rtk cargo fmt --all
git add crates/rabbit-rs-core/src/consumer/actor.rs crates/rabbit-rs-core/tests/recovery.rs
git commit -m "fix(core): fence poison settlements against stale generations and pin the invariant in tests"
```

---

## Final Verification

- [ ] Full quality gate: `rtk ./scripts/check.sh` (fmt + clippy + nextest + composer validate) — must pass with zero warnings.
- [ ] Focused suites green: `rtk cargo nextest run -p rabbit-rs-core --test consumer --test recovery --test poison`
- [ ] The four audit repro properties hold:
  1. Settlement accepted while `pending_incoming` is saturated reaches the wire and reopens the gate (Task 1/2).
  2. A delivery larger than `max_buffered_bytes` is settled terminally; the actor stays reactive; no HOL blocking (Task 3).
  3. At most one wire ack per delivery tag in `early_ack` mode across re-dispatches (Task 4).
  4. Stale-generation settlements never touch the new channel (Task 5).
- [ ] Update `docs/plans/2026-07-30-rabbitmq-native-implementation.md` if the milestone tracker references consumer reliability status.

## Out of Scope (documented decisions)

- **`ackThrough` frontier tightening (audit #4):** the internal path is a contiguous *settled* prefix and safe; the public API documents caller responsibility (`composite.rs:165-166`). Tightening it requires delivery-state tracking in the ledger — separate plan if demanded.
- **Publisher confirm timeout semantics (audit #6):** typed terminal `Timeout` with no automatic resend is a documented design decision (`docs/reference.md:357`, publisher replay plan). No change.
- **Mandatory return/confirm race (audit #7):** already correct and tested (`tests/publisher.rs:273`, proptest `publisher_replay_machine.rs`). No change.
- **Runtime-mutable `max_buffered_bytes` (Task 6 candidate):** rejected as speculative. Task 2's control channel provides the infrastructure if a real need appears; until then the value is fixed per subscription and a miscalibration is observable (typed error + DLX) instead of fatal.
