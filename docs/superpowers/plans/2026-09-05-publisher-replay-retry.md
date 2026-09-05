# Publisher replay retry-once Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A publish whose deadline expires while it is parked in replay (connection lost, recovery in progress) is re-armed **once** with a fresh deadline instead of failing terminally — the upstream "first publish after idle" scenario becomes transparent to the caller.

**Architecture:** Change contained in the publisher actor (`expire_replay` is the single decision point): `RetainedPublish` gains `timeout` (captured at acceptance) and `retried`; a metrics counter makes the re-arm measurable. Confirm-timeout paths (connection alive, outcome unknown) stay terminal — no semantics change for them.

**Tech Stack:** Rust 1.96 / edition 2024, tokio paused time + `MockTransport`, Pest on the PHP side.

## Global Constraints

- `#![forbid(unsafe_code)]` intact; fmt + clippy `-D warnings` clean.
- At-least-once contract: duplicates permitted and identifiable (`message_id` preserved by the retry).
- Async tests use `#[tokio::test(start_paused = true)]` only — no real sleeps.
- Retry **only** on replay expiry (suspension) — never on confirm-timeout nor on `FailedPermanent`.
- All committed artifacts in English.

---

### Task 1: Retry-once in the publisher actor + metrics counter

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs` (`RetainedPublish` :352-359, `try_publish` :162-169, `expire_replay` :478-496)
- Modify: `crates/rabbit-rs-core/src/metrics.rs` (counter + snapshot :38-51, :106-119, :123-148)
- Test: `crates/rabbit-rs-core/tests/publisher.rs` (updates `deadline_expiring_during_outage_prevents_replay` :1047-1075, adds 1 test)

**Interfaces:**
- Consumes: `PublishRequest.deadline` (tokio `Instant`), `time` = `tokio::time` (actor.rs:12), `PublisherHandle::metrics_snapshot()` (actor.rs:262).
- Produces: `MetricsSnapshot.publication_retries_total: u64` + `Metrics::record_publication_retry(&self)` (pub(crate)) — used by Task 2.

- [ ] **Step 1: Save this plan** to `docs/superpowers/plans/2026-09-05-publisher-replay-retry.md`.

- [ ] **Step 2: Write the failing test (retry-once exhausted)** — replace `deadline_expiring_during_outage_prevents_replay` (tests/publisher.rs:1047-1075) with:

```rust
#[tokio::test(start_paused = true)]
async fn replay_expiry_during_outage_is_retried_once_then_fails() {
    let transport = MockTransport::default();
    let actor = actor_recovery(&transport, 8).await;
    suspend(&actor).await;
    let waiter = actor
        .try_publish(request_recovery(
            "expired",
            Instant::now() + Duration::from_millis(10),
        ))
        .expect("queued");
    tokio::task::yield_now().await;

    // First expiry re-arms the publication once with a fresh 10 ms window.
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;
    // Second expiry exhausts the single retry: terminal Timeout.
    tokio::time::advance(Duration::from_millis(10)).await;
    assert_eq!(
        waiter.wait().await.expect_err("retry exhausted").kind(),
        PublishErrorKind::Timeout
    );

    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("resume after expiry");
    assert!(
        publish_operations(&transport).is_empty(),
        "expiry must never reach the wire"
    );
    assert_eq!(actor.metrics_snapshot().publication_retries_total, 1);
}
```

- [ ] **Step 3: Write the regression test (the report's scenario)** — add next to it:

```rust
#[tokio::test(start_paused = true)]
async fn replay_expiry_during_outage_is_retried_once_and_confirmed_after_ready() {
    let transport = MockTransport::default();
    let actor = actor_recovery(&transport, 8).await;
    suspend(&actor).await;
    let waiter = actor
        .try_publish(request_recovery(
            "idle-publish",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("queued");
    tokio::task::yield_now().await;

    // Recovery outage outlasts the original deadline: expiry re-arms once.
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::task::yield_now().await;

    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("publisher resumed");

    assert_eq!(
        waiter.wait().await.expect("confirmed after retry"),
        PublishOutcome::Confirmed {
            message_id: "idle-publish".into()
        }
    );
    let attempts = publish_operations(&transport);
    assert_eq!(attempts.len(), 1, "re-arm parks in replay, no early republish");
    assert_eq!(attempts[0].properties.message_id.as_deref(), Some("idle-publish"));
}
```

- [ ] **Step 4: Verify failure**

Run: `rtk cargo test -p rabbit-rs-core --test publisher replay_expiry`
Expected: FAIL — the first test receives `Timeout` at the first `advance` (waiter resolved early), the second fails (waiter already resolved with an error). Both fail because `publication_retries_total` does not exist either (compile error expected on the first run).

- [ ] **Step 5: Implement the minimum**

`metrics.rs` — follow the existing family pattern: field `publication_retries_total: AtomicU64` in `MetricsInner` (:106), `load()` in `snapshot()` (:38), doc on the `MetricsSnapshot` field (:123): "Publications whose deadline expired during a recovery suspension and were re-armed once.", and:

```rust
    pub(crate) fn record_publication_retry(&self) {
        increment(&self.inner.publication_retries_total);
    }
```

`actor.rs` — `RetainedPublish` (:352) gains two fields:

```rust
    timeout: Duration,
    retried: bool,
```

`try_publish` (:162) computes before moving `request`:

```rust
        let timeout = request.deadline.saturating_duration_since(time::Instant::now());
        let command = Command::Publish(Box::new(RetainedPublish {
            timeout,
            retried: false,
            request,
            completion,
            accepted_at: Instant::now(),
            _permit: permit,
            sequence: 0,
            payload_bytes,
        }));
```

`expire_replay` (:478) — the retry branch (single decision point):

```rust
        while let Some(mut pending) = self.replay.pop_front() {
            if pending.request.deadline <= now {
                if pending.retried {
                    self.byte_budget.release(pending.payload_bytes);
                    complete_error(
                        pending,
                        PublishError::new(
                            PublishErrorKind::Timeout,
                            "publish deadline expired during connection recovery",
                        ),
                    );
                } else {
                    pending.retried = true;
                    pending.request.deadline = now + pending.timeout;
                    self.metrics.record_publication_retry();
                    retained.push_back(pending);
                }
            } else {
                retained.push_back(pending);
            }
        }
```

- [ ] **Step 6: Verify green + no regression**

Run: `rtk cargo test -p rabbit-rs-core --test publisher replay_expiry && rtk cargo test -p rabbit-rs-core`
Expected: PASS everywhere — in particular `confirmation_timeout_is_typed`, `confirm_timeout_from_config_is_applied` and `mid_batch_connection_loss_replays_unconfirmed_publications_identically` must stay green (confirm-timeout unchanged).

- [ ] **Step 7: fmt + clippy, then commit**

```bash
rtk cargo fmt --all && rtk cargo clippy -p rabbit-rs-core --all-targets --all-features -- -D warnings
git add crates/rabbit-rs-core/src/publisher/actor.rs crates/rabbit-rs-core/src/metrics.rs crates/rabbit-rs-core/tests/publisher.rs docs/superpowers/plans/2026-09-05-publisher-replay-retry.md
git commit -m "fix(core): re-arm once a publication whose deadline expired during recovery suspension"
```

---

### Task 2: PHP exposure + documentation + full gate

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs:221-231` (stats key), `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php:111` (@return shape)
- Modify: `AGENTS.md` (replay invariant), `docs/reliability.md` (~:27), `docs/configuration.md` (:102), `packages/laravel-queue/README.md` (:290)

**Interfaces:**
- Consumes: `MetricsSnapshot.publication_retries_total` (Task 1).
- Produces: `publication_retries_total` key in `Pool::stats()`.

- [ ] **Step 1: PHP test first (exposure)** — in an existing stats Pest test under `crates/rabbit-rs-php/tests/` (same pattern as `BackpressureTest.php:51`), add the assertion `expect($pool->stats()['publication_retries_total'])->toBe(0);` on a healthy pool.

- [ ] **Step 2: Verify failure** — Run: `./scripts/test-extension.sh` → FAIL (missing key).

- [ ] **Step 3: Implement** — `pool.rs`: add `("publication_retries_total", metrics.publication_retries_total),` to the list :221-231. Stub :111: add `publication_retries_total: int` to the `@return array{...}`. (The existing stub already omits keys — only add.)

- [ ] **Step 4: Docs** (English, concise):
  - `AGENTS.md` — invariant: "…replayed with the same `message_id` and original deadline. A publication whose deadline expired while parked during a recovery suspension is re-armed exactly once with a fresh deadline before failing terminally (measured by `publication_retries_total`)."
  - `docs/reliability.md` ~:27 + `docs/configuration.md` :102 + `packages/laravel-queue/README.md` :290 — one sentence each: during a recovery, a publish parked in replay is retried once with a fresh deadline; a confirm-timeout on a live connection stays terminal (unknown outcome → no automatic resend).

- [ ] **Step 5: Verify** — Run: `./scripts/test-extension.sh` → PASS.

- [ ] **Step 6: Full gate**

Run: `rtk ./scripts/check.sh`
Expected: PASS (fmt, clippy, nextest workspace, composer validate).

- [ ] **Step 7: Commit**

```bash
git add crates/rabbit-rs-php/src/classes/pool.rs crates/rabbit-rs-php/stubs/rabbit_rs.stub.php AGENTS.md docs/reliability.md docs/configuration.md packages/laravel-queue/README.md
git commit -m "feat: expose publication_retries_total and document replay re-arm policy"
```

---

## Self-review (done)

- **Coverage**: report symptom → Task 1 (test B = exact scenario, A = once bound, existing guards :295/:507/:866 pin confirm-timeout terminal). ✅
- **Placeholders**: none. ✅
- **Type consistency**: `publication_retries_total` identical in metrics.rs → actor assert → pool.rs → stub. `timeout`/`retried` defined in Task 1, used only there. ✅
- **Adversarial checks**: `MetricsSnapshot` constructed only once (metrics.rs:38); PHP/Rust tests never assert the exhaustive shape → adding a field is safe. `PublishWaiter::wait(self)` consumes → both tests call `wait()` once. Blind pump untouched (pump ≠ actor replay).

**Deliberately excluded**: retry on confirm-timeout (unknown outcome → unnecessary duplicate risk), checkout gate (proven no-op), `ConnectionLost` event (not requested by the chosen fix).
