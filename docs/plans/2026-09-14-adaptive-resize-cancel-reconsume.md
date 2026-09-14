# Adaptive Prefetch Resize (cancel + re-consume) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Apply an adaptive prefetch adjustment to a *running* consumer by cancelling and re-consuming, instead of the current silent no-op `basic.qos`.

**Architecture:** RabbitMQ applies per-consumer `basic.qos` (`global=false`) only to consumers created after the call, and quorum queues reject `global=true` entirely (RUSTSEC-style finding documented in issue #300). The delivery pump (`spawn_source`) owns the stream, so it performs the resize: on a watch signal from the actor's controller tick it cancels the consumer tag, drains the old stream until it ends (in-flight deliveries stay ackable on the same channel — no requeue, no duplicates), applies the new QoS, and re-consumes with the same tag. The PHP embedder is untouched.

**Tech Stack:** Rust 1.96 / edition 2024, tokio (paused-time tests), lapin behind the `Transport` abstraction, mock transport (`transport/mock.rs`).

## Global Constraints

- Unsafe Rust forbidden (`#![forbid(unsafe_code)]`); do not weaken workspace lints.
- Delivery contract is at-least-once: silent loss unacceptable; the resize must never requeue or drop in-flight deliveries.
- Do not touch `.github/workflows/` (concurrent agents own CI changes).
- Tests use paused Tokio time and the scriptable mock transport; no real sleeps.
- Quality gate before claiming done: `rtk ./scripts/check.sh` (fmt + clippy `-D warnings` + nextest + composer validate).
- All repository artifacts in English.
- Conversations with the user in French; commits/PRs/docs in English.

---

### Task 1: Transport `cancel` — trait, lapin impl, mock stream epochs

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport.rs` (trait `ConsumerChannel`, near line 405)
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs` (impl `LapinChannel`, near line 236)
- Modify: `crates/rabbit-rs-core/src/transport/mock.rs` (`TransportOperation` enum line 17, `MockState` line 44 area, `MockConsumerChannel` line 589, `MockDeliveryStream` line 653)
- Test: `crates/rabbit-rs-core/src/transport/mock.rs` (`#[cfg(test)]` module or a new test block)

**Interfaces:**
- Consumes: existing `ConsumerChannel` trait, `TransportOperation` recording, `MockDeliveryStream` parking semantics (`keep_delivery_stream_open`, `delivery_notify`).
- Produces: `ConsumerChannel::cancel(&self, consumer_tag: &str) -> TransportResult<()>`; `TransportOperation::Cancel { consumer_tag: String }` (public enum — matching test code compiles against it); mock behavior: after `cancel`, streams created before the cancel return `None` once drained (overrides `keep_delivery_stream_open`).

- [ ] **Step 1: Write the failing mock-behavior test**

Add to `mock.rs` test module:

```rust
#[tokio::test]
async fn cancelled_delivery_stream_ends_after_draining_in_flight() {
    let transport = MockTransport::default();
    transport.keep_delivery_stream_open();
    transport.push_delivery(Ok(delivery_stub(1)));
    let mut stream = open_consumer_stream(&transport).await;

    // In-flight delivery still surfaces after the cancel request...
    transport.cancel_consumer_tag("rabbit-rs.default").await;
    let first = stream.next().await.expect("in-flight delivery");
    assert!(first.is_ok());

    // ...then the cancelled stream ends instead of parking, even though
    // keep_delivery_stream_open is set.
    assert!(stream.next().await.is_none());
}
```

`delivery_stub` / `open_consumer_stream` are existing test helpers if present; otherwise construct via the existing mock public API (`transport.connect(...).open_consumer(...)` — mirror how `transport_liveness.rs` opens consumer streams).

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --lib transport::mock`
Expected: FAIL — `cancel` / `cancel_consumer_tag` does not exist (compile error is an acceptable RED).

- [ ] **Step 3: Implement the mock change**

In `TransportOperation` (public enum, line 17) add:

```rust
Cancel { consumer_tag: String },
```

In `MockState` add:

```rust
/// Bumped by `cancel`: streams created before the bump end once drained.
stream_epoch: u64,
```

Add to `MockTransport` (public test surface):

```rust
pub async fn cancel_consumer_tag(&self, consumer_tag: &str) {
    let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    state.stream_epoch += 1;
    state.delivery_notify.notify_one();
}
```

(If `MockTransport` exposes a channel object instead, put the epoch bump behind the `MockConsumerChannel::cancel` impl and give `MockTransport` a thin async wrapper that calls it.)

`MockConsumerChannel` gains:

```rust
async fn cancel(&self, consumer_tag: &str) -> TransportResult<()> {
    self.record_consumer(TransportOperation::Cancel {
        consumer_tag: consumer_tag.to_owned(),
    })?;
    let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    state.stream_epoch += 1;
    state.delivery_notify.notify_one();
    Ok(())
}
```

`consume()` stamps the new stream with the current epoch:

```rust
let epoch = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).stream_epoch;
Ok(Box::new(MockDeliveryStream { state: self.state.clone(), epoch }))
```

`MockDeliveryStream::next()` — after the empty-check, before the keep-open park:

```rust
let cancelled = epoch_stale; // computed inside the same lock as `delivery`
if cancelled {
    return None; // a deliberately cancelled stream ends even when keep-open is set
}
```

- [ ] **Step 4: Add the trait method + lapin impl**

`transport.rs` trait `ConsumerChannel` (next to `set_qos`):

```rust
/// Cancels the consumer registered under `consumer_tag`. Deliveries
/// already dispatched remain acknowledged-able on the channel; only new
/// dispatches stop. Returns once the broker confirmed the cancellation.
async fn cancel(&self, consumer_tag: &str) -> TransportResult<()>;
```

`lapin.rs` impl (next to `set_qos`):

```rust
async fn cancel(&self, consumer_tag: &str) -> TransportResult<()> {
    self.inner
        .basic_cancel(consumer_tag, BasicCancelOptions::default())
        .await
        .map_err(map_lapin_error)
}
```

Import `BasicCancelOptions` alongside the existing `BasicQosOptions` import.

- [ ] **Step 5: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core --lib transport::mock`
Expected: PASS. Also run `rtk cargo test -p rabbit-rs-core` — any `ConsumerChannel` impl outside these two files (search `impl ConsumerChannel for`) must be updated for the new trait method; expect compile errors guiding to them.

- [ ] **Step 6: Commit**

```bash
git add crates/rabbit-rs-core/src/transport.rs crates/rabbit-rs-core/src/transport/lapin.rs crates/rabbit-rs-core/src/transport/mock.rs
git commit -m "feat(transport): add ConsumerChannel::cancel with lapin and mock impls"
```

---

### Task 2: Pump-driven resize — watch plumbing, `spawn_source` state machine, actor tick

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs` (`spawn_with_generation` line 198, `spawn_source` line 303)
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (`RuntimeSubscription` line 98 or `ActorState` line 114, `collect_prefetch_updates` line 254, tick arm line 704, `run_actor` signature line ~604)
- Test: `crates/rabbit-rs-core/tests/consumer.rs` (append near the adaptive tests, after line 2237)

**Interfaces:**
- Consumes: `ConsumerChannel::cancel` (Task 1), `AdaptivePrefetch::tick() -> Option<u16>` (existing), `PREFETCH_TICK = 1s`, test helpers `adaptive_subscription`, `let_sources_fill`, `let_actor_process`, `delivery`, `connection_key` (tests/consumer.rs helper mod).
- Produces: per-subscription `watch::Sender<u16>` stored in `ActorState.qos_txs`; `collect_prefetch_updates(&mut self) -> Vec<(SubscriptionId, u16)>`; `spawn_source(subscription, stream, commands, channel, consumer_tag, queue, no_ack, qos_rx)`.

- [ ] **Step 1: Write the failing focal test**

Append to `crates/rabbit-rs-core/tests/consumer.rs`:

```rust
#[tokio::test(start_paused = true)]
async fn adaptive_adjustment_cancels_and_reconsumes_with_the_new_window() {
    let transport = MockTransport::default();
    transport.keep_delivery_stream_open();
    for tag in 1..=3 {
        transport.push_delivery(Ok(delivery(tag, b"job")));
    }
    let consumer = ConsumerSet::spawn_with_metrics(
        vec![adaptive_subscription(&transport, "adaptive", connection_key("adaptive", "/")).await],
        Metrics::default(),
    )
    .await
    .expect("consumer set");
    let_sources_fill().await;

    // Feed the controller three record-time samples through sequential acks.
    for _ in 0..3 {
        let delivery = consumer.next().await.expect("delivery");
        delivery.ack().await.expect("ack");
        let_actor_process().await;
    }

    // Fire the controller tick under paused time; the pump performs the
    // resize: cancel, drain, set_qos, re-consume.
    tokio::time::advance(Duration::from_secs(1)).await;
    let_actor_process().await;
    let_actor_process().await;

    // Deliveries published after the resize must flow through the new
    // stream without a terminal error.
    transport.push_delivery(Ok(delivery(4, b"job")));
    let_sources_fill().await;
    let delivery = consumer.next().await.expect("post-resize delivery");
    delivery.ack().await.expect("ack");

    let operations = transport.operations();
    let cancels = operations
        .iter()
        .filter(|op| matches!(op, TransportOperation::Cancel { .. }))
        .count();
    let consumes = operations
        .iter()
        .filter(|op| matches!(op, TransportOperation::Consume(_)))
        .count();
    let qos_values: Vec<u16> = operations
        .iter()
        .filter_map(|op| match op {
            TransportOperation::Qos { prefetch } => Some(*prefetch),
            _ => None,
        })
        .collect();
    assert_eq!(cancels, 1, "exactly one deliberate cancel");
    assert_eq!(consumes, 2, "initial subscribe plus re-consume");
    assert_eq!(qos_values[0], 16, "initial window");
    assert!(qos_values[1] > 16, "resized window grows: {qos_values:?}");
}
```

Note: `TransportOperation` is already imported in this test file (used by the adaptive tests at line 2006).

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer adaptive_adjustment_cancels_and_reconsumes`
Expected: FAIL — currently no `Cancel` operation is ever recorded (the actor calls `set_qos` detached and never cancels/re-consumes); `cancels` is 0 and the post-resize pop likely surfaces the terminal stream-end error.

- [ ] **Step 3: Implement the pump state machine (`set.rs`)**

In `spawn_with_generation` (line 198), inside the per-subscription setup loop, create the watch and keep the sender:

```rust
let (qos_tx, qos_rx) = tokio::sync::watch::channel(subscription.prefetch.initial_value());
qos_txs.insert(subscription.id.clone(), qos_tx);
```

(`qos_txs: HashMap<SubscriptionId, watch::Sender<u16>>` declared before the loop, passed to `run_actor` as a new argument.)

Replace the `for (subscription, stream) in streams` hand-off (line 284) so `spawn_source` receives everything the resize needs:

```rust
for (subscription, stream) in streams {
    spawn_source(
        subscription.id.clone(),
        stream,
        commands.clone(),
        Arc::clone(&subscription.channel),
        format!("rabbit-rs.{}", subscription.id.as_str()),
        subscription.queue.clone(),
        subscription.no_ack,
        qos_rxs.remove(&subscription.id).expect("qos watch"),
    );
}
```

(`qos_rxs: HashMap<SubscriptionId, watch::Receiver<u16>>` built alongside `qos_txs`.)

Rewrite `spawn_source`:

```rust
#[allow(clippy::too_many_arguments)]
fn spawn_source(
    subscription: SubscriptionId,
    mut stream: Box<dyn DeliveryStream>,
    commands: mpsc::Sender<ConsumerCommand>,
    channel: Arc<dyn ConsumerChannel>,
    consumer_tag: String,
    queue: String,
    no_ack: bool,
    mut qos_rx: tokio::sync::watch::Receiver<u16>,
) {
    tokio::spawn(async move {
        // A pending resize: the controller raised/lowered the window and the
        // tag was cancelled. The old stream drains its in-flight deliveries,
        // then ends; `None` triggers the re-consume with the new window.
        let mut pending_qos: Option<u16> = None;
        loop {
            tokio::select! {
                result = stream.next() => match result {
                    Some(result) => {
                        if commands
                            .send(ConsumerCommand::Incoming {
                                subscription: subscription.clone(),
                                result,
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    None => {
                        match pending_qos.take() {
                            // Deliberate cancellation: apply the new window
                            // and re-subscribe. In-flight deliveries were
                            // drained above and stay ackable on the channel.
                            Some(value) => {
                                if let Err(error) = channel.set_qos(value).await {
                                    let _ = commands
                                        .send(ConsumerCommand::Incoming {
                                            subscription: subscription.clone(),
                                            result: Err(TransportError::connection(format!(
                                                "adaptive prefetch resize set_qos failed: {error}"
                                            ))),
                                        })
                                        .await;
                                    return;
                                }
                                match channel
                                    .consume(ConsumerRequest {
                                        queue: queue.clone(),
                                        consumer_tag: consumer_tag.clone(),
                                        exclusive: false,
                                        no_ack,
                                    })
                                    .await
                                {
                                    Ok(new_stream) => stream = new_stream,
                                    Err(error) => {
                                        let _ = commands
                                            .send(ConsumerCommand::Incoming {
                                                subscription,
                                                result: Err(TransportError::connection(format!(
                                                    "adaptive prefetch re-consume failed: {error}"
                                                ))),
                                            })
                                            .await;
                                        return;
                                    }
                                }
                            }
                            // Unexpected stream termination: the subscription
                            // is dead (connection lost, channel closed).
                            None => {
                                let _ = commands
                                    .send(ConsumerCommand::Incoming {
                                        subscription,
                                        result: Err(TransportError::connection(
                                            "consumer delivery stream ended",
                                        )),
                                    })
                                    .await;
                                return;
                            }
                        }
                    }
                },
                changed = qos_rx.changed() => {
                    if changed.is_err() {
                        // The actor is gone; close terminates the pump through
                        // the command channel. Nothing to resize.
                        continue;
                    }
                    let value = *qos_rx.borrow();
                    if pending_qos.is_none() {
                        // First resize request of the cycle: stop the broker
                        // from dispatching further deliveries to the old tag.
                        if let Err(error) = channel.cancel(&consumer_tag).await {
                            let _ = commands
                                .send(ConsumerCommand::Incoming {
                                    subscription: subscription.clone(),
                                    result: Err(TransportError::connection(format!(
                                        "adaptive prefetch cancel failed: {error}"
                                    ))),
                                })
                                .await;
                            return;
                        }
                    }
                    pending_qos = Some(value);
                }
            }
        }
    });
}
```

- [ ] **Step 4: Switch the actor tick to watch sends (`actor.rs`)**

Add to `ActorState` (line 114 area):

```rust
qos_txs: HashMap<SubscriptionId, tokio::sync::watch::Sender<u16>>,
```

Thread it: `run_actor` (line 604) takes `qos_txs: HashMap<SubscriptionId, tokio::sync::watch::Sender<u16>>`; `ActorState::new` takes and stores it.

Replace `collect_prefetch_updates` (line 254):

```rust
/// Advances every adaptive controller and returns the window changes to
/// publish on the per-subscription resize watches.
fn collect_prefetch_updates(&mut self) -> Vec<(SubscriptionId, u16)> {
    let mut updates = Vec::new();
    for (id, controller) in &mut self.adaptive_prefetch {
        if let Some(value) = controller.tick() {
            updates.push((id.clone(), value));
        }
    }
    updates
}
```

Replace the tick arm (line 704):

```rust
_ = prefetch_interval.tick(), if has_adaptive => {
    // The controller tick is pure; the pump performs the cancel +
    // set_qos + re-consume sequence on its own stream (never blocking
    // dispatch or settlements).
    for (subscription, value) in state.collect_prefetch_updates() {
        if let Some(qos_tx) = state.qos_txs.get(&subscription) {
            let _ = qos_tx.send(value);
        }
    }
}
```

- [ ] **Step 5: Run the focal test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core --test consumer adaptive_adjustment_cancels_and_reconsumes`
Expected: PASS.

- [ ] **Step 6: Run the whole consumer suite and fix fallouts**

Run: `rtk cargo test -p rabbit-rs-core --test consumer`
Known fallout candidates (adapt assertions, do not weaken semantics):
- `adaptive_prefetch_grows_to_max_after_fast_jobs` (line 1975): Qos ops still recorded (by the pump now); `qos_values.len() == 2` should still hold — a second tick adjustment would add entries only if hysteresis allows; if flaky, assert the first two values instead of the exact length.
- `adaptive_prefetch_set_qos_failure_surfaces_and_actor_survives` (line 2068): the failure now surfaces from the pump via the terminal `Incoming` path. The "actor survives" expectation holds; adjust how the error is observed if the test polled the detached task's error channel.
- `adaptive_controller_learns_at_ack_record_time_not_at_settlement_completion` (line 2177): unchanged mechanism; if the re-consume changes pop counts, review the test's `expect` calls.
- `consumer_delivery_stream_termination_surfaces_a_terminal_error`: must stay green unchanged (no pending resize → terminal error).

- [ ] **Step 7: Run the full core suite**

Run: `rtk cargo test -p rabbit-rs-core`
Expected: all green.

- [ ] **Step 8: Commit**

```bash
git add crates/rabbit-rs-core/src/consumer/set.rs crates/rabbit-rs-core/src/consumer/actor.rs crates/rabbit-rs-core/tests/consumer.rs
git commit -m "feat(consumer): apply adaptive prefetch adjustments via cancel and re-consume"
```

---

### Task 3: Docs, gate, integration, PR

**Files:**
- Modify: `docs/plans/2026-07-30-rabbitmq-native-design.md` (adaptive prefetch section — describe the resize mechanism)
- Test: integration via `scripts/test-integration.sh` (lab running)

**Interfaces:**
- Consumes: everything from Tasks 1-2.
- Produces: PR closing #300.

- [ ] **Step 1: Update the design doc**

In the adaptive prefetch section of `docs/plans/2026-07-30-rabbitmq-native-design.md`, document: adjustments apply through `basic.cancel` → `basic.qos` → `basic.consume` on the dedicated channel (RabbitMQ applies per-consumer qos to new consumers only; quorum queues reject `global=true`); in-flight deliveries stay ackable (no requeue); recovery re-spawns the set at the configured initial window, so the learned window resets across recovery generations (known limitation, follow-up).

- [ ] **Step 2: Run the integration suite**

Run: `rtk ./scripts/test-integration.sh`
Expected: Rust integration green (lab on 5672 must be up).

- [ ] **Step 3: Optional live validation (bench)**

Rebuild the release dylib (`rtk cargo build --release --manifest-path crates/rabbit-rs-php/Cargo.toml`) and run the adaptive worker bench with `RABBIT_RS_PREFETCH='{"mode":"adaptive","initial":128,"min":100,"max":2000,"target_buffer_seconds":5}'` on the quorum `bench.goopil.driver-bench` queue. Expected: rates converge toward the ~44-50k fixed-2000 plateau instead of staying pinned at ~20-25k. Record numbers in the PR.

- [ ] **Step 4: Full gate**

Run: `rtk cargo fmt --all` then `rtk ./scripts/check.sh`
Expected: all green.

- [ ] **Step 5: Commit docs and push, open PR**

```bash
git add docs/plans/2026-07-30-rabbitmq-native-design.md
git commit -m "docs: describe the cancel and re-consume adaptive resize mechanism"
git push -u origin feat/adaptive-resize-consume
gh pr create --base main --title "feat(consumer): apply adaptive prefetch adjustments via cancel and re-consume" --body "..."
```

PR body: summary (mechanism + why: per-consumer qos applies to new consumers only, quorum rejects global qos — evidence in #300), bench numbers before/after, `Fixes #300`, `Refs #282`. CI green + explicit user GO before merge (merge commit).

## Self-Review

- Spec coverage: resize mechanism (Tasks 1-2), no-loss invariant (drain-then-resubscribe ordering in Task 2), mock observability (Task 1 Cancel op), docs + validation (Task 3). Recovery-time learned-window persistence explicitly out of scope — noted in Task 3 docs step and PR body.
- Placeholder scan: none — all code blocks concrete; the only free-form step is the PR body, templated above.
- Type consistency: `cancel(&self, consumer_tag: &str)`, `TransportOperation::Cancel { consumer_tag: String }`, `collect_prefetch_updates -> Vec<(SubscriptionId, u16)>`, `qos_txs/qos_rxs: HashMap<SubscriptionId, watch::{Sender,Receiver}<u16>>` — consistent across tasks.
