# Post-Audit Stabilization Implementation Plan (audit 2026-10-01)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix every HIGH/MEDIUM finding of the 2026-10-01 adversarial audit (6 passes: publish, consume, pool/recovery, PHP FFI, Laravel, launch-readiness), gate the performance work behind a fresh profile, and clear the public-launch hygiene punch list.

**Architecture:** Four code phases touch the three layers (rabbit-rs-core, rabbit-rs-php, packages/laravel-queue) with strict TDD: a failing test proves each audit finding first, then the minimal fix. Phase 3 (performance) is deliberately a *profiling protocol with decision gates*, not pre-written optimizations — this repo's own rule is "no perf change without a fresh post-Round-D profile". Phases 4–5 are mechanical hygiene/contract fixes with verification steps instead of test cycles.

**Tech Stack:** Rust 1.98.1 (edition 2024, `#![forbid(unsafe_code)]`), Tokio (paused-time tests), Lapin behind `Transport`, ext-php-rs 0.15.x, Pest + Testbench (Laravel 12/13), RabbitMQ 4.2.9 lab (Docker).

## Global Constraints

- All committed artifacts in English (AGENTS.md language policy).
- Unsafe Rust is forbidden; never weaken `#![forbid(unsafe_code)]` or workspace lints.
- TDD for every behavior change: failing test first, observe intended failure, minimal fix, rerun.
- Deterministic async tests: paused Tokio time + scriptable mock transport. No real sleeps in unit tests.
- Everything bounded: queues, channels, in-flight work, retries, replay buffers.
- Never expose credentials, full broker URIs, or certificate material through `Debug`, errors, metrics, or logs.
- Rust threads hold no Zend values / PHP objects / callbacks.
- Recovery order stays deterministic: connection → channels → exchanges → queues → bindings → QoS → consumers.
- At-least-once: silent loss unacceptable; duplicates permitted, identifiable, measurable.
- Use `rtk`-prefixed commands (`rtk cargo test -p rabbit-rs-core config::tests`, `rtk ./scripts/check.sh`).
- Commit style (repo history): `fix(core): …`, `feat(laravel): …`, `docs: …` — one logical commit per green task; never include `.air/`, IDE metadata, build artifacts.
- Test-scenario sketches below were derived from the 2026-10-01 audit with exact file:line anchors; when an enum/variant/helper name differs in the working tree, keep the *scenario* and adapt the name to the actual code — never weaken the assertion.
- Integration tests requiring the lab (`./scripts/test-integration.sh`) run only where listed; they need the Docker lab up (`./scripts/lab-up.sh`).

## Preconditions (before Task 1)

Sequencing decision (2026-10-01): **land PR #338 first** ("Native AMQP fallback when management_url is absent", which also carries the single-copy follow-ups `89dfc01`/`ab2452e` and the locally merged #336/#337), then start this round from the updated `main`. Rationale: Task 23 touches `DoctorProbe.php`/`RabbitMqDoctorCommand.php`, the same files as #338 — sequencing removes all conflict risk.

- [x] **Step 0.1:** Confirm PR #338 CI is green and merge it (`gh pr checks 338`, then `gh pr merge 338 --merge` to match the repo's merge-commit history).
- [x] **Step 0.2:** Create the execution branch from the updated `main` (the plan file itself is committed as the first docs commit of this branch):

```bash
git checkout main && git pull && git checkout -b fix/audit-2026-10-01
git add docs/superpowers/plans/2026-10-01-post-audit-stabilization.md
git commit -m "docs(plans): add post-audit stabilization implementation plan"
```

- [x] **Step 0.3:** Baseline green gate before any change: `rtk ./scripts/check.sh` → PASS.

---

## Phase 1 — Stability (audit HIGH findings)

### Task 1: Consumer settle-through guards (HIGH 1–3: double settle panics + stale-generation poisoning)

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (`claim_settlement` ~1525, `handle_settle_through` ~1258-1295, completion arm ~1061-1098, `validate_contiguous_prefix` ~1835)
- Modify: `crates/rabbit-rs-core/src/consumer/composite.rs:392-412` (secondary: the single-slot `pending_error` can swallow the one-shot `SourceReplaced` signal — `retire()` skips pushing when the slot holds a stashed batch error, and `next()` returns the batch error so the replaced broker is never re-fetched)
- Test: `crates/rabbit-rs-core/tests/consumer.rs` (extend; mirror setup of `settlement_errors_never_stall_the_actor_when_never_drained` at consumer.rs:2091 and the stale-generation pattern of `tests/recovery.rs:405`)

**Context:** `try_settle_through` performs no CAS; `claim_settlement` checks only the `settling` flag (reset to `false` when the first settle-through completes), never `token.state`. A second `SettleThrough` for an already-settled prefix reaches `ledger.pending.range(acked_prefix + 1..=target_tag)` with start > end → panic inside the actor task → every subscription stops settling, buffered deliveries are never served, `close()` hangs. Secondary panic: `.last().unwrap()` on an empty `affected_tokens` in the completion arm. Third: a stale-generation settle-through whose numeric tag collides with a live token poisons live tokens (`DeliveryState::Lost`) and double-subtracts their bytes from `buffered_bytes` because `validate_contiguous_prefix` runs before the generation fence.

**Interfaces:**
- Produces: `SettlementError::AlreadySettled` returned (not panicked) for any settle-through targeting an already-settled or in-progress prefix; settle-through rejected with `StaleGeneration` *before* touching any token when the token's `(connection_key, generation, channel_id)` does not match the live actor.

- [x] **Step 1: Write failing tests (4 scenarios, in `tests/consumer.rs`)**

```rust
// 1. Double settle-through must return AlreadySettled, never panic, actor stays alive.
#[test]
fn double_settle_through_returns_already_settled_and_actor_survives() {
    // setup identical to existing settle-through tests in this file:
    // pool + consumer set on mock transport, deliver tags 1..=3, pop all.
    // settle through 3  -> expect Ok
    // settle through 3 again -> expect Err(AlreadySettled)   <-- today: PANIC (range start > end)
    // then deliver tag 4 and settle it -> must still succeed (actor alive)
}

// 2. ackThrough after ack(): flush_acked advanced past target -> AlreadySettled, no panic.
#[test]
fn settle_through_after_ack_returns_already_settled() { /* ack(1..=3) via ack(); then try_settle_through(3) */ }

// 3. Stale-generation settle-through with colliding tag must NOT touch live tokens.
#[test]
fn stale_generation_settle_through_leaves_live_tokens_settleable() {
    // recover generation; keep a token from gen 1; settle-through its tag from the stale handle
    // expect: Err(StaleGeneration) raised BEFORE tokens are staged
    // assert every live token of gen 2 still settles Ok and buffered_bytes unchanged
}

// 4. Completion arm must tolerate an empty affected_tokens (no unwrap panic).
#[test]
fn settle_through_with_no_affected_tokens_does_not_panic() { /* force expected<=target false path, wire ack Ok(Acked) */ }

// 5. SourceReplaced must not be swallowed by a stashed batch error in the composite slot.
#[test]
fn source_replaced_signal_survives_a_stashed_batch_error() {
    // stash a mid-drain batch error in pending_error, then retire the source
    // expect: the next next() still returns the re-fetch signal (SourceReplaced), not only the batch error
    //   (queue the retire signal separately from batch errors)
}
```

- [x] **Step 2: Verify failure**

Run: `rtk cargo test -p rabbit-rs-core --test consumer double_settle_through -- --nocapture`
Expected: FAIL — test 1 panics with a `BTreeMap` range assertion ("range start index …") inside the actor task.

- [x] **Step 3: Implement minimal guards in `consumer/actor.rs`**

1. In `claim_settlement` (and/or `handle_settle_through` before `validate_contiguous_prefix`): check `token.state` — anything not `Pending` → return `Err(SettlementError::AlreadySettled)`; also treat `target_tag <= acked_prefix` as `AlreadySettled` before computing the range.
2. In `handle_settle_through`: verify `(connection_key, generation, channel_id)` against the token *before* `validate_contiguous_prefix` collects `affected_tokens`; mismatch → `Err(StaleGeneration)` with zero token mutations.
3. In the `ControlCommand::SettleThrough` completion arm (~1061-1068): replace `affected_tokens.last().unwrap()` with an `if let Some(last)` guard (empty → record ack metric only).
4. Exclude any foreign-generation token from `affected_tokens` at stage time (defense in depth for 3).
5. In `composite.rs`, keep the retire/`SourceReplaced` signal in a separate slot (or small queue) from stashed batch errors so `next()` always returns the re-fetch signal even when a batch error is pending.

- [x] **Step 4: Verify pass + no regressions**

Run: `rtk cargo test -p rabbit-rs-core --test consumer && rtk cargo test -p rabbit-rs-core --test poison && rtk cargo test -p rabbit-rs-core --test recovery`
Expected: PASS (new tests green; existing settlement/recovery tests untouched).

- [x] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core/src/consumer/actor.rs crates/rabbit-rs-core/tests/consumer.rs
git commit -m "fix(core): guard settle-through against double settlement, stale generation and empty token ranges"
```

### Task 2: Permanent-error classification for topology declares and coordinator causes (HIGH 4–5: infinite reconnect loops)

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs:787-805` (`map_lapin_error`)
- Modify: `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:431-443` (`recovery_loss_error`), `367-375` (on-demand consumer path)
- Modify: error carriers as needed so `CoordinatorError::Consumer`/`Publisher` preserve a permanence flag or source `TransportError` (check `crates/rabbit-rs-core/src/consumer/set.rs`, `src/publisher/mod.rs`, `src/error.rs`)
- Test: `crates/rabbit-rs-core/tests/recovery.rs` (extend; the 406 scenario mirrors `tests/recovery.rs:819` which covers only ACCESS_REFUSED)

**Context:** `map_lapin_error` special-cases only lapin ids 403/530 as permanent. A 406 PRECONDITION_FAILED on a topology declare (pre-existing queue with incompatible arguments — never self-healing) or a 404 in verify mode maps to a recoverable `TransportErrorKind::Connection`; `recovery_loss_error` then flattens it and `route_loss` resets failures to 1 → reconnect + re-declare forever at ~100 ms backoff — the exact "406 storm" class the project eliminated for delay queues (#79). Same flattening for `CoordinatorError::Consumer`/`Publisher`: an ACCESS_REFUSED on `basic.consume` (no read permission) or QoS precondition failure loops forever and tears down the shared connection each cycle.

**Interfaces:**
- Produces: `map_lapin_error` classifies AMQP ids 404 (NOT_FOUND) and 406 (PRECONDITION_FAILED) as permanent (same bucket as 403/530) — *except* when raised by a lazy TTL-delay-queue declare whose identity fingerprint expects a fresh queue (documented exception, see below). `recovery_loss_error` honors a permanence flag carried in `CoordinatorError::Consumer`/`Publisher` and routes it through the same `!is_recoverable` gate as transport errors.

- [x] **Step 1: Write failing tests in `tests/recovery.rs`**

```rust
// 1. Topology declare failing with PRECONDITION_FAILED (406) -> FailedPermanent, coordinator stops.
#[test]
fn precondition_failed_topology_declare_fails_permanently_without_reconnect_loop() {
    // mock transport: reconcile channel's declare_queue returns Err(TransportError) kind
    // mapping to lapin id 406 semantics (use the same constructor the 403 test at :819 uses)
    // assert: state becomes FailedPermanent (or equivalent), no further recover attempt scheduled
    // assert: recovery_failures_total stops incrementing after the first failure
}

// 2. consumer establishment ACCESS_REFUSED (403) wrapped as CoordinatorError::Consumer -> permanent.
#[test]
fn permanent_consumer_establishment_error_stays_permanent() {
    // basic.consume refused -> CoordinatorError::Consumer { .. } carries permanence
    // assert: recovery_loss_error returns a non-recoverable error; no reconnect loop
}

// 3. Regression: transient connection loss during reconcile still retries with backoff.
#[test]
fn transient_reconcile_failure_still_retries() { /* existing behavior must remain green */ }
```

- [x] **Step 2: Verify failure**

Run: `rtk cargo test -p rabbit-rs-core --test recovery precondition_failed -- --nocapture`
Expected: FAIL — the 406 error is currently classified recoverable; the coordinator schedules another recovery (test times out or observes repeated attempts).

- [x] **Step 3: Implement**

1. `lapin.rs::map_lapin_error`: add ids `404` and `406` to the permanent mapping. Caveat to preserve: lazily-synthesized TTL delay queues rely on *create-if-missing* semantics — verify no internal declare path depends on retrying a 404 for a queue it is about to create; if one exists (delay-queue lazy declare), keep 404 retryable for that specific call site via an explicit parameter, and 406 permanent everywhere.
2. `recovery_coordinator.rs`: extend `CoordinatorError::Consumer`/`Publisher` (or their source payloads) with the underlying error/flag; in `recovery_loss_error`, run the same `is_recoverable` check on the carried cause before flattening; permanent → `FailedPermanent`.
3. On-demand path `consumer()` (`367-375`): surface the real error kind instead of generic `connection_lost` when the connection is healthy.
4. Publisher-slot mutex (same file, audit MEDIUM): `shutdown_coordinator` (~560-573) and `recover_generation` (~742-769) hold `publisher.lock()` across awaited actor round-trips, parking `wait_for_publisher`/`publisher()` with no deadline. Clone the handle out of the slot, drop the guard, then await. Test: a parked `wait_for_publisher` still completes while a recovery generation transition is in flight.
5. Doc comment on `recovery_loss_error` updated to match behavior (it already *claims* permanence is preserved — make it true).

- [x] **Step 4: Verify**

Run: `rtk cargo test -p rabbit-rs-core --test recovery && rtk cargo test -p rabbit-rs-core --test transport_liveness`
Expected: PASS.

- [x] **Step 5: Lab check (integration, real broker)**

Run: `./scripts/test-integration.sh`
Expected: PASS (24/24 Laravel + Rust integration suites; no behavior change for healthy topologies).

- [x] **Step 6: Commit**

```bash
git add crates/rabbit-rs-core/src/transport/lapin.rs crates/rabbit-rs-core/src/pool/recovery_coordinator.rs crates/rabbit-rs-core/tests/recovery.rs
git commit -m "fix(core): classify topology 404/406 as permanent and preserve permanence through coordinator errors"
```

### Task 3: Laravel `bulk()` chunking (HIGH 6)

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php:261-290` (`prepareBatch`/`publishBatch` shared path — chunking must live here so the Horizon subclass benefits; `partitionJobsByAfterCommit` at :296 gets the same `isset($this->container)` guard as the rest of the class — audit LOW 9, same lines)
- Test: `packages/laravel-queue/tests/Feature/BulkChunkingTest.php` (new)

**Context:** `bulk()` maps all immediate jobs into one `Pool::publishBatch()`. The native layer hard-fails the whole call at >256 messages or >1 MiB cumulative payload (validation precedes send: atomic, nothing published). `docs/reference.md:157-166` promises "a single native call (publishBatch) for all immediate jobs" — update the doc to the chunked contract.

- [x] **Step 1: Write failing Pest test**

```php
// tests/Feature/BulkChunkingTest.php
it('chunks bulk publishes beyond the native batch limits', function () {
    $queue = $this->rabbitMqQueue();           // existing test helper (fake Pool) — mirror DrainErrorsTest setup
    $queue->bulk([300 dispatched-job payloads]);
    expect($this->poolFake->publishBatchCalls)->toHaveCount(2);   // 256 + 44
    expect($this->poolFake->publishedPayloads)->toHaveCount(300); // nothing lost
});

it('chunks bulk by cumulative payload below 1 MiB per chunk', function () { /* 3 x 400 KiB payloads -> 2 chunks */ });

it('surfaces the chunk failure after earlier chunks were published', function () {
    // chunk 2 fails (fake throws) -> expect QueueException thrown; chunk 1 publications remain visible
    // (documented semantics: at-least-once, caller retry re-publishes — message_id dedupe is the user's job)
});
```

- [x] **Step 2: Verify failure**

Run: `cd packages/laravel-queue && php -n vendor/bin/pest tests/Feature/BulkChunkingTest.php`
Expected: FAIL — a single publishBatch call overflows the fake's limit / throws.

- [x] **Step 3: Implement in the shared protected path (`prepareBatch`/`publishBatch`)**

1. Partition payloads into chunks respecting both native bounds: ≤256 messages AND cumulative payload < 1 MiB (split a chunk early when adding the next payload would cross 1 MiB; a single payload > 1 MiB fails that chunk with the existing per-message conversion error naming the limit).
2. Loop chunks: `publishBatch($chunk)`; after each chunk, `drainSettlementErrors()` (existing helper) so pipeline failures surface per chunk.
3. Add the `isset($this->container)` guard to `partitionJobsByAfterCommit` (audit LOW 9).
4. Update `docs/reference.md:157-166` and the `bulk()` docblock: chunked contract, ≤256 msgs / <1 MiB per chunk, partial-success semantics on mid-chunk failure.

- [x] **Step 4: Verify**

Run: `cd packages/laravel-queue && php -n vendor/bin/pest --testsuite "Rabbit RS Laravel"`
Expected: PASS (new tests + no regression in Horizon/bulk suites).

- [x] **Step 5: Commit**

```bash
git add packages/laravel-queue/src/RabbitMqQueue.php packages/laravel-queue/tests/Feature/BulkChunkingTest.php packages/laravel-queue/docs/reference.md
git commit -m "fix(laravel): chunk bulk publishes to the native batch bounds and document partial success"
```

- [x] **Step 6: Phase 1 gate**

Run: `rtk ./scripts/check.sh && ./scripts/test-integration.sh`
Expected: PASS both.

---

## Phase 2 — Robustness (audit MEDIUM findings)

### Task 4: Config validation — `max_buffered_bytes = 0` and duplicate names

**Files:**
- Modify: `crates/rabbit-rs-core/src/config.rs:800-880` (`validate_worker`), `:640-661` (`validate`)
- Test: `crates/rabbit-rs-core/src/config.rs` tests module (unit tests next to `validate`)

**Context:** `max_buffered_bytes = 0` passes validation; `handle_incoming` then classifies *every* delivery as oversized → settle_oversized → ack-and-drop (no DLX): total silent destruction from a plausible misconfiguration. Duplicate broker/worker names are accepted; `ValidatedConfig::broker()/worker()` silently return the first match.

- [x] **Step 1: Write failing tests**

```rust
#[test] fn rejects_zero_max_buffered_bytes_with_exact_path() { /* worker config with max_buffered_bytes=0 -> Err naming workers.<name>.max_buffered_bytes */ }
#[test] fn rejects_duplicate_broker_names() { /* two brokers named "a" -> Err naming brokers.a */ }
#[test] fn rejects_duplicate_worker_names() { /* two workers named "w" -> Err naming workers.w */ }
```

- [x] **Step 2: Verify failure** — Run: `rtk cargo test -p rabbit-rs-core config::tests` → Expected: FAIL (configs accepted).

- [x] **Step 3: Implement** — In `validate_worker`: reject `max_buffered_bytes == 0` with the exact input path; in `validate`: detect duplicate names for brokers and workers, error with the exact path (mirroring the existing subscription-name uniqueness check).

- [x] **Step 4: Verify** — Run: `rtk cargo test -p rabbit-rs-core config::tests && rtk cargo test -p rabbit-rs-core --test auto_profiles` → PASS.

- [x] **Step 5: Commit** — `git commit -m "fix(core): reject zero max_buffered_bytes and duplicate broker/worker names"`

### Task 5: Delayed release honors the subscription's `max_attempts`

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs:1753-1755` (`delayed_release` — pass the subscription's configured cap instead of `AttemptsResolver::default()`)
- Test: `crates/rabbit-rs-core/tests/poison.rs` (extend)

**Context:** Dispatch correctly uses the per-subscription cap (wired from config at `recovery_coordinator.rs:882`), but delayed release validates against the default cap (20). A subscription with `max_attempts = 50` dead-letters at attempt 21 on delayed release.

- [x] **Step 1: Failing test** — subscription `max_attempts = 50`, delivery at attempt 21, `release(delay > 0)` → must succeed (delayed republish), not settle terminally. Today: `MaxAttempts` → terminal poison.
- [x] **Step 2: Verify failure** — Run: `rtk cargo test -p rabbit-rs-core --test poison delayed_release` → FAIL (terminal settlement).
- [x] **Step 3: Implement** — thread the subscription's `max_attempts` (already present in the runtime state next to `SettlementLaunch`) into `delayed_release`'s header construction.
- [x] **Step 4: Verify** — Run: `rtk cargo test -p rabbit-rs-core --test poison && rtk cargo test -p rabbit-rs-core --test consumer` → PASS.
- [x] **Step 5: Commit** — `git commit -m "fix(core): validate delayed release against the subscription max_attempts"`

### Task 6: FFI — bound `Consumer::next(timeoutMs)`

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs:47` + `:309-325`
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` (docblock for `next`) — regenerate via `./scripts/stubs.sh --out crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` after editing the Rust docblock
- Test: `packages/laravel-queue/tests/Unit/ConsumerNextTimeoutTest.php` (fake-level) or the ext Pest suite (`crates/rabbit-rs-php` Pest tests, `ConsumerTest.php`)

**Context:** `next(PHP_INT_MAX)` parks an FPM worker forever (`max_execution_time` counts CPU only while parked in `block_on`; tokio clamps overflow to `far_future`). The boundary's own discipline caps publish `timeout_ms` at 24 h (`conversion.rs:179-183`) — `next()` must match.

- [x] **Step 1: Failing test (ext Pest, `ConsumerTest.php`)** — `next()` with `timeout_ms > 86_400_000` → `ValueError` naming the bound (mirror the publish-side error text). Today: accepted.
- [x] **Step 2: Verify failure** — Run: `./scripts/test-extension.sh` → FAIL.
- [x] **Step 3: Implement** — validate/clamp `timeout_ms` with the same `MAX_TIMEOUT_MS` ceiling used by publish; document the bound in the Rust docblock; regenerate stubs; `php -l` the stub.
- [x] **Step 4: Verify** — Run: `./scripts/test-extension.sh` → PASS; `rtk ./scripts/test-laravel.sh` → PASS.
- [x] **Step 5: Commit** — `git commit -m "fix(php-ext): bound Consumer::next timeout to the shared 24h ceiling"`

### Task 7: `EventBridge::drain()` early-return when no callbacks

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/bridge.rs:96-109`
- Test: existing `NativeEventDispatchTest` (Laravel) must stay green; add a Rust unit test in `bridge.rs` tests if the module is testable without a PHP thread — otherwise behavior-pinned by existing suites.

**Context:** Every publish/tryNext/next/flush/stats/getMessage pays `connection_states()` (mutex + HashMap + String clones), a full `metrics_snapshot()` copy, and two extra mutex acquisitions — all discarded when both `CallbackRegistry`s are empty (the common case). 1–3 allocations + 3 locks per message on the advertised allocation-free hot path.

- [x] **Step 1: Add test** — with both registries empty, `drain()` performs no client/metrics calls (assert via a counting fake client if available in `testing.rs`; otherwise assert observable behavior unchanged and rely on existing suites).
- [x] **Step 2: Verify** — Run: `rtk cargo test -p rabbit-rs-php` → baseline.
- [x] **Step 3: Implement** — first line of `drain()`: `if self.callbacks.connection.is_empty() && self.callbacks.backpressure.is_empty() { return; }`. Leaving `last_connection_states`/`last_backpressure_total` stale is correct: a later registration still diffs against them and fires on change (add a doc comment saying exactly that).
- [x] **Step 4: Verify** — Run: `rtk cargo test -p rabbit-rs-php && rtk ./scripts/test-laravel.sh` → PASS (event tests still fire when callbacks registered).
- [x] **Step 5: Commit** — `git commit -m "perf(php-ext): skip event bridge drain work when no callbacks are registered"`

### Task 8: Publisher wire-write deadline

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:763-832` (`publish_in_flight` / `next_deadline`)
- Test: `crates/rabbit-rs-core/tests/publisher.rs` (extend; mock transport gate — mirror existing paused-time confirm-timeout tests)

**Context:** The confirm phase is bounded by `min(request.deadline, now + confirm_timeout)`, but the pre-confirm wire-write future has no timeout; a transport publish that never resolves holds the permit + byte reservation forever and the waiter never resolves. Only `close()`'s quiesce escapes it.

- [x] **Step 1: Failing test** — mock transport gate holds the publish future open past `request.deadline` → the waiter must resolve terminally on the deadline, the permit/byte reservation must be released (assert a subsequent publish of the same size succeeds), actor stays alive. Today: hangs (test fails on timeout).
- [x] **Step 2: Verify failure** — Run: `rtk cargo test -p rabbit-rs-core --test publisher wire_write` → FAIL (hang/assert).
- [x] **Step 3: Implement** — wrap the wire-write future in `tokio::time::timeout(min(request.deadline, now + confirm_timeout), fut)`; on expiry resolve the waiter terminal (reuse the deadline-expiry path, which already counts `publication_retries_total` semantics correctly) and release permit/bytes through the existing terminal path. Also fix the shutdown-race leak (audit LOW 4, same file): a publish command still queued in the mpsc when the actor exits leaks its byte reservation (the `_permit` is RAII-released but `payload_bytes` is only released by terminal paths that never run) — RAII the byte reservation symmetrically with the pump's `BudgetGuard`, or drain-and-fail the command queue before returning.
- [x] **Step 4: Verify** — Run: `rtk cargo test -p rabbit-rs-core --test publisher && rtk cargo test -p rabbit-rs-core --test publisher_replay_machine && rtk cargo test -p rabbit-rs-core --test blind_pump` → PASS.
- [x] **Step 5: Commit** — `git commit -m "fix(core): bound the publisher wire-write phase by the request deadline"`

### Task 9: PublishBuffer — bounded handle vecs

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/publish_buffer.rs:102-106, 489-492, 531-534, 555-558` (`drain_handles`/`timer_handles`), `:566-592` (permit-timeout re-buffer leaves a batch with no armed timer), `:653-675` (`flush_teardown` outcome misclassification + µs `tearing_down` window), `:45-49, 381, 466-470` (teardown budget composed of two sequential 500 ms budgets)
- Test: `crates/rabbit-rs-core/tests/publish_buffer_state_machine.rs` harness already drives a real client over the mock transport — add a long-run case there if it drives the PHP-side buffer; otherwise add focused unit tests in `publish_buffer.rs`.

**Context:** `flush_pipelined`/`run_flush_timer` push a `JoinHandle` per drain/timer; the vecs are pruned only by `quiesce()`, which `publish()`/`flush_triggered()` never call. A publish-only long-lived process accumulates completed handles (~1–2 per flush cycle, default flush interval 1 ms). Related same-file defects: a permit-timeout re-buffer can leave a batch with no armed timer (flushes only at the next publish — breaches the #218 flush-interval contract); `flush_teardown` treats an `Ok` batch containing `Returned` outcomes as fully confirmed and counts every publication dropped on timeout (some still confirm afterwards); a drain completing in the µs window between `quiesce()` and `tearing_down.store(true)` re-buffers uncounted; the destructor can block two full 500 ms budgets plus `block_on` overhead although the documented ceiling (audit F-18) is one.

- [x] **Step 1: Failing tests** — (a) run 10_000 pipelined flush cycles without `quiesce()` → assert `drain_handles.len()` stays under a small cap (e.g. ≤ 64). Today: grows to ~10_000. (b) a re-buffered batch after permit timeout always has a timer armed (assert flush occurs within `flush_interval` on the mock clock). (c) `flush_teardown` reports returned/unconfirmed distinctly instead of lumping into dropped; the `tearing_down` flag is set before `quiesce()` so late-completing drains cannot re-buffer uncounted.
- [x] **Step 2: Verify failure** — Run: `rtk cargo test -p rabbit-rs-php publish_buffer` → FAIL.
- [x] **Step 3: Implement** — (a) after each push, prune completed handles (`handles.retain(|h| !h.is_finished())`) — cheap, keeps abort-safety semantics identical (aborted-but-running handles are retained until finished). (b) call `ensure_flush_timer()` after `rebuffer_or_drop` in both saturation paths (idempotent via the `timer_pending` swap). (c) set `tearing_down` before `quiesce()`; inspect outcomes in the teardown `Ok` arm and report returned/unconfirmed distinctly. (d) one shared deadline across quiesce + teardown batch flush (single 500 ms ceiling), or document the composition in the constant's doc comment.
- [x] **Step 4: Verify** — Run: `rtk cargo test -p rabbit-rs-php && rtk ./scripts/test-extension.sh` → PASS.
- [x] **Step 5: Commit** — `git commit -m "fix(php-ext): bound publish-buffer handle growth and make teardown accounting exact"`

### Task 10: Connection actor — command-arm timeouts + Connecting-phase permanent loss

**Files:**
- Modify: `crates/rabbit-rs-core/src/pool/connection_actor.rs:417-430` (command arm), `:519-562` (shutdown), `:314` (`handle_connecting` catch-all)
- Test: `crates/rabbit-rs-core/tests/transport_liveness.rs` (extend — gated mock transport, existing 10 s CONNECT_TIMEOUT pattern at connection_actor.rs:21)

**Context:** `open_publisher`/`open_consumer`/`connection.close()` run inside the command arm with no timeout; `Command::Close`, `ConnectionLost`, and error events queue behind them; stall bounded only by heartbeat detection (config up to 65535 s). `Pool::close` → `close_claim` → `client.close()` has no budget. Also, a permanent `ConnectionLost` reported during `Connecting` is swallowed by the catch-all (sibling phase `handle_recovering` promotes it to `FailedPermanent`).

- [x] **Step 1: Failing tests**

```rust
// 1. Close while open_channel is gated -> close completes within a bounded budget, not heartbeat-bound.
#[test] fn close_completes_within_budget_when_channel_open_is_gated() { /* gate open_publisher; call close(); assert completion < 1s (paused time) */ }
// 2. Permanent ConnectionLost during Connecting -> FailedPermanent.
#[test] fn permanent_connection_lost_during_connecting_fails_permanently() { /* recoverable=false loss mid-connect -> FailedPermanent */ }
```

- [x] **Step 2: Verify failure** — Run: `rtk cargo test -p rabbit-rs-core --test transport_liveness close_completes_within_budget` → FAIL (test times out).
- [x] **Step 3: Implement** — (a) wrap per-command channel ops (`open_publisher`, `open_consumer`) and the shutdown `connection.close()` in `tokio::time::timeout` bounded by the existing 10 s `CONNECT_TIMEOUT` constant (reuse; no new knob — document in code); on timeout treat as a connection loss (existing `handle_recovering` path). (b) in `handle_connecting`, mirror `handle_recovering`: non-recoverable `ConnectionLost` → `FailedPermanent` instead of `{}`.
- [x] **Step 4: Verify** — Run: `rtk cargo test -p rabbit-rs-core --test transport_liveness && rtk cargo test -p rabbit-rs-core --test recovery` → PASS.
- [x] **Step 5: Commit** — `git commit -m "fix(core): bound connection-actor channel ops and honor permanent loss during Connecting"`

### Task 11: Registry pool eviction (bounded process footprint)

**Files:**
- Modify: `crates/rabbit-rs-core/src/runtime.rs:69, 168-180`
- Test: `crates/rabbit-rs-core/tests/pool_claims.rs` (extend)

**Context:** `ProcessState::pools` never evicts; a pool dropped without `close()` keeps its client, actors, and AMQP socket alive forever. A worker building pools for rotating fingerprints leaks one connection set per fingerprint.

- [x] **Step 1: Failing test** — create K (> cap, e.g. 17) distinct pools with zero claims, acquire once more → the registry map size is bounded (≤ cap, default 16, oldest zero-claim evicted and closed). Today: 17 entries + 17 live sockets.
- [x] **Step 2: Verify failure** — Run: `rtk cargo test -p rabbit-rs-core --test pool_claims registry_eviction` → FAIL.
- [x] **Step 3: Implement** — on `acquire`, after inserting, evict at most one entry when `pools.len() > cap` (const `MAX_POOLS: usize = 16`): choose the zero-claim entry with the oldest last-use instant; if none is zero-claim, skip eviction (never close an in-use pool) and log a `warn!` via the log facade. Eviction = `close()` + remove (reuse the existing close path so budgets apply).
- [x] **Step 4: Verify** — Run: `rtk cargo test -p rabbit-rs-core --test pool_claims && rtk cargo test -p rabbit-rs-core --test pool_clear` → PASS.
- [x] **Step 5: Commit** — `git commit -m "fix(core): bound the runtime registry with LRU eviction of idle pools"`

### Task 12: `connection_blocked` gauge counts instead of set/clear

**Files:**
- Modify: `crates/rabbit-rs-core/src/metrics.rs:105-117, 142-144`
- Test: `crates/rabbit-rs-core/tests/connection_blocked.rs` (extend — multi-broker case)

**Context:** The gauge is pool-global but cleared per-broker: broker B's Unblocked resets the gauge while broker A is still under a memory alarm.

- [x] **Step 1: Failing test** — two brokers; A blocked, B blocked, B unblocked → gauge must read 1 (A still blocked). Today: 0.
- [x] **Step 2: Verify failure** — Run: `rtk cargo test -p rabbit-rs-core --test connection_blocked multi_broker` → FAIL.
- [x] **Step 3: Implement** — replace set/clear with increment/decrement (`fetch_add(1)` on Blocked, `fetch_sub(1)` saturated on Unblocked) keyed by episode as today; keep the episode counter semantics intact.
- [x] **Step 4: Verify** — Run: `rtk cargo test -p rabbit-rs-core --test connection_blocked` → PASS.
- [x] **Step 5: Commit** — `git commit -m "fix(core): make connection_blocked gauge count blocked brokers instead of set/clear"`

### Task 13: Laravel — extension version enforced at resolution + connector env casting + status isolation

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqServiceProvider.php:59-69, 124-132`
- Modify: `packages/laravel-queue/src/Connectors/RabbitMqConnector.php:48-62`
- Modify: `packages/laravel-queue/src/Console/RabbitMqStatusCommand.php:66-77`
- Modify: `packages/laravel-queue/src/Console/DoctorProbe.php` (extract `satisfiesCaret` to a shared helper — new `src/Support/ExtensionConstraint.php` or method on the provider)
- Test: `packages/laravel-queue/tests/Feature/` (extend `ExtensionVersionTest` + new connector-casting cases)

**Context:** The connector guard checks only `extension_loaded('rabbit_rs')`; a loaded 0.2.x binary sails through and fails confusingly at pool creation (`deny_unknown_fields` on newer config). `block_for => '3'` (env string) and `after_commit => '1'` throw raw `InvalidArgumentException` without the config path, although the compiler's casters accept env strings. One bad connection aborts the whole `rabbit-rs:status` and CLI stats print zeros without context.

- [x] **Step 1: Failing tests**

```php
it('rejects a loaded extension below the caret constraint at connection resolution', function () {
    // fake phpversion('rabbit_rs') = '0.2.9' (test shim used by ExtensionVersionTest) -> expect RuntimeException
    //   naming the loaded version, required ^0.3.10 and `pie install goopil/rabbit-rs-native`
})->skip(needs shim); // adjust to the existing fake mechanism

it('casts block_for and after_commit from env-style strings with the config path on error', function () {
    // block_for='3' -> accepted as 3; block_for='soon' -> InvalidArgumentException naming queue.connections.<name>.block_for
});

it('keeps rabbit-rs:status running when one connection fails to compile', function () {
    // one broken connection, one good -> good one listed; broken one reported with its error line
});
```

- [x] **Step 2: Verify failure** — Run: `cd packages/laravel-queue && php -n vendor/bin/pest tests/Feature` → FAIL.
- [x] **Step 3: Implement** — (a) extract caret check into a shared support class; call it in the connector closure after `assertNativeExtensionLoaded()` (skip when the extension is absent — the missing-extension error already names the constraint). (b) route connector-read keys through the compiler's `integer()`/`boolean()` casters (or duplicate their env-string handling) and prefix failures with `queue.connections.<name>.<key>`. (c) wrap per-connection stat collection in try/catch, render the error row, add a "same-process counters" hint line to the command output. (d) fix `docs/management-api.md:18` behavior matrix — the "Queue depth sampling" row claims native `Pool::size()` with or without `management_url`, but the sampler uses the Management API whenever `management_url` is set (ready + unacked) and native (ready-only) only as fallback; correct the row and note the unacked-gauge asymmetry applies to the native leg only.
- [x] **Step 4: Verify** — Run: `cd packages/laravel-queue && php -n vendor/bin/pest` → PASS.
- [x] **Step 5: Commit** — `git commit -m "fix(laravel): enforce extension caret at resolution, cast connector keys, isolate status failures"`

### Task 14: Laravel — `queue` key coherence with `subscriptions` + actionable `pop(null)`

**Files:**
- Modify: `packages/laravel-queue/src/Config/ConnectionCompiler.php:314-359` (validation), `packages/laravel-queue/src/Console/WorkPlanResolver.php:142-160`, `packages/laravel-queue/src/RabbitMqQueue.php:473-495`
- Test: `packages/laravel-queue/tests/Unit/ConnectionCompilerTest.php` + Feature test for pop fallback

**Context:** With `subscriptions` present, the compiler ignores the connection `queue` key, but `WorkPlanResolver` still plans it (children die per-pop with `unknown worker profile`) and `pop(null)` falls to `$profile = $queueName` → native error instead of the actionable message explicit pops produce.

- [x] **Step 1: Failing tests** — (a) connection with `subscriptions` whose queues do not cover `queue` → compile-time `InvalidArgumentException` naming `queue.connections.<name>.queue` with the two remediation options (remove the key or add it as a subscription); (b) identical case where `queue` matches one subscription queue → accepted (no false positive).
- [x] **Step 2: Verify failure** — Run: `cd packages/laravel-queue && php -n vendor/bin/pest tests/Unit/ConnectionCompilerTest.php` → FAIL.
- [x] **Step 3: Implement** — validation in `ConnectionCompiler` (step 1a/1b); `pop(null)` fallback: when the resolved profile is not a known profile name, throw the same actionable `InvalidArgumentException` (mirror the explicit-pop path).
- [x] **Step 4: Verify** — Run: `cd packages/laravel-queue && php -n vendor/bin/pest` → PASS.
- [x] **Step 5: Commit** — `git commit -m "fix(laravel): validate the queue key against subscriptions and give pop(null) an actionable error"`

### Task 15: Laravel — make the K8s `prestop` drain real

**Files:**
- Modify: `packages/laravel-queue/src/Support/ProbeStatefile.php` (add a `drain_requested` marker), `packages/laravel-queue/src/Console/WorkerSupervisor.php:939-947` (supervise loop), `packages/laravel-queue/src/Console/RabbitMqProbeCommand.php:99-112`
- Test: `packages/laravel-queue/tests/Feature/WorkerSupervisorIntegrationTest.php` (extend — stub-process pattern exists)

**Context:** `prestop` SIGTERMs each worker PID from its statefile; `queue:work` exits 0 on SIGTERM and the supervisor immediately recycles the slot (fresh PID, new statefile). The hook tracks only the original statefiles, sees them `draining`, and reports success while respawned workers keep consuming.

- [x] **Step 1: Failing test** — integration stub: mark a worker's statefile `drain_requested`, SIGTERM it → supervisor must NOT respawn that slot until the marker is cleared; `prestop` waits for zero non-draining workers among *tracked* PIDs and returns success only when the fleet (not just original PIDs) is quiesced.
- [x] **Step 2: Verify failure** — Run: `cd packages/laravel-queue && php -n vendor/bin/pest tests/Feature/WorkerSupervisorIntegrationTest.php` → FAIL (slot respawns).
- [x] **Step 3: Implement** — `prestop` writes `drain_requested: true` into each tracked statefile *and* signals the supervisor (SIGUSR1-style or marker file under the probes dir); the supervisor's recycle path checks the marker before restarting a clean-exited slot (defer restart while set); `prestop` clears markers after its bounded wait.
- [x] **Step 4: Verify** — Run: `cd packages/laravel-queue && php -n vendor/bin/pest` → PASS.
- [x] **Step 5: Commit** — `git commit -m "fix(laravel): supervisor honors drain requests so the k8s prestop hook actually drains"`

### Task 16: `clear()` covers TTL delay queues + consumer settlement drain on close

**Files:**
- Modify: `crates/rabbit-rs-core/src/client.rs` (`clear_route`/`clear` — include the route's synthesized delay-bucket queues when `delay.mode = ttl`; names resolvable from the compiled `TopologyPlan`/`DelayRouter`), `crates/rabbit-rs-php/src/classes/pool.rs` (pass-through unchanged if API identical)
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php:673-683` (`closeConsumers` — best-effort `drainErrors()` + `logPending…` before close, mirroring the publish-side teardown)
- Test: `crates/rabbit-rs-core/tests/pool_clear.rs` (extend); `packages/laravel-queue/tests/Feature/RabbitMqQueueCleanupTest.php` (extend)

**Context:** In `delay.mode = ttl`, `queue:clear` reports success while deferred jobs survive in bucket queues and execute later. `closeConsumers()` discards undrained consumer settlement records silently (the publish side logs its unsurfaced records).

- [x] **Step 1: Failing tests** — (a) core: pool with ttl delay config, publish delayed to bucket, `clear(route)` → delay bucket empty (`queue_size` on the bucket = 0). (b) Laravel: consumer with a pending settlement error record → `closeConsumers()` logs it (assert via Log spy) instead of dropping it.
- [x] **Step 2: Verify failure** — Run: `rtk cargo test -p rabbit-rs-core --test pool_clear clear_delay` → FAIL.
- [x] **Step 3: Implement** — (a) `clear` purges delay buckets derived from the plan for that route (skip buckets already swept/GC'd; document that plugin-mode delayed messages live on the delayed exchange path and cannot be purged selectively — doc note). (b) Laravel: drain-then-close with the existing best-effort logging pattern.
- [x] **Step 4: Verify** — Run: `rtk cargo test -p rabbit-rs-core --test pool_clear && cd packages/laravel-queue && php -n vendor/bin/pest tests/Feature/RabbitMqQueueCleanupTest.php` → PASS.
- [x] **Step 5: Commit** — `git commit -m "fix(core): purge ttl delay buckets on clear and drain consumer settlement records on close"`

- [x] **Step 6: Phase 2 gate**

Run: `rtk ./scripts/check.sh && ./scripts/test-integration.sh && ./scripts/test-fpm.sh && ./scripts/test-octane.sh`
Expected: PASS all.

---

## Phase 3 — Performance (GATED: profile first, no code before data)

### Task 17: Fresh profile + optimization decision gate

**Files:**
- Create: `benchmarks/results/round-l-profile/README.md` (evidence + decisions)
- No production code changes in this task.

**Context:** Repo rule (ROADMAP): perf work is gated on a fresh post-Round-D profile. Audit-identified candidates (scheduler per-pick `Vec` + O(n²) `contains` at `consumer/scheduler.rs:96-124`; one `tokio::spawn` per delivery on the early-ack path at `consumer/actor.rs:441-446`; 2 `String` allocations per message for `MessageId` at `actor.rs:390-398, 495`; 2–4 per-publish `String` allocations at `publisher/delay.rs:142-158` + `transport/lapin.rs:665-693`; eager budget-location formatting at `rabbit-rs-php/src/conversion.rs:55-75, 392, 446`) are *hypotheses until measured*.

- [x] **Step 1:** Fresh lab: `./scripts/lab-down.sh && ./scripts/lab-up.sh && ./scripts/lab-ready.sh`.
- [x] **Step 2:** Release build; run the driver-bench standard protocol (`benchmarks/driver-bench/bin/bench.php` scenarios: worker, safe publish, blind publish, consumer) — archive JSON under `benchmarks/results/round-l-profile/`.
- [x] **Step 3:** Profile (samply/perf on the reference machine): publish safe path end-to-end + `Consumer::next()` ~60 µs attribution (the explicit post-Round-K open question). Record per-stage breakdown (FFI boundary, conversion, pump hand-off, confirm waiter, socket write) in the README.
- [x] **Step 4:** For each of the 5 candidates: measured cost vs projected gain in the archived profile → keep (with issue + task) or reject (with the number). Only profile-proven items graduate to implementation tasks appended to this plan.
- [x] **Step 5:** Commit the evidence + decisions: `git commit -m "docs(bench): round-l profile evidence and optimization decisions"`.

---

## Phase 4 — Launch hygiene (repo is already public; mechanical fixes)

### Task 18: Slug and version sweep (the 404 day-one experience)

**Files:**
- Modify: `CONTRIBUTING.md:17` (clone URL → `https://github.com/Goopil/php-rabbit-rs.git`), `Cargo.toml:14` (`repository` → `https://github.com/Goopil/php-rabbit-rs`), `docs/reference.md:556-557` (issues links), `packages/laravel-queue/CHANGELOG.md:3, 324` (workspace-changelog link + nonexistent `docs/configuration.md` → real path), `README.md:104` (`^0.3.6` → `^0.3.10` or version-generic wording), `CHANGELOG.md:565` + `packages/laravel-queue/CHANGELOG.md:394` (add `[0.3.7]`–`[0.3.10]` link refs, repoint `[Unreleased]` to `v0.3.10...HEAD`), `scripts/check-docs.sh` (add a guard pattern for stale `^0.3.x` support-table claims)
- Verify: zero occurrences of the old slug in maintained docs (historical plans/audits excluded, matching `check-docs.sh` exclusions).

- [x] **Step 1:** Apply the edits above.
- [x] **Step 2:** Verify: `grep -rn "Goopil/rabbit-rs\b" CONTRIBUTING.md Cargo.toml README.md docs/reference.md packages/laravel-queue/CHANGELOG.md CHANGELOG.md` → only historical docs match; `./scripts/check-docs.sh` PASS; `rtk composer validate --strict` (root + package) PASS.
- [x] **Step 3:** Commit: `git commit -m "docs: fix stale repo slug and version references from the rename"`

### Task 19: Community files, `.idea` untrack, gitignore

**Files:**
- Create: `.github/ISSUE_TEMPLATE/bug_report.md` (fields per the project's own launch gate #177 item 5: `php -v`, `php --ri rabbit_rs`, OS/arch/libc, install method, redacted config, expected/actual), `.github/ISSUE_TEMPLATE/config.yml` (route security reports to SECURITY.md), `.github/PULL_REQUEST_TEMPLATE.md`, `CODE_OF_CONDUCT.md` (Contributor Covenant)
- Modify: `.gitignore` (add `/target-php84/`, `/packages/laravel-queue/build/`, `/.ruff_cache/`, `.DS_Store`), remove `/tests export-ignore` dead line in `.gitattributes:11`
- Remove: `.idea/` from the index (`git rm -r --cached .idea`)

- [x] **Step 1:** Create the four community files (concise; SECURITY.md already covers reporting — config.yml points there).
- [x] **Step 2:** `git rm -r --cached .idea && git commit` (dedicated commit so the removal is revertable).
- [x] **Step 3:** Apply `.gitignore`/`.gitattributes` edits; verify `git status` is clean of the three artifact dirs.
- [x] **Step 4:** Commit: `git commit -m "chore: add community templates and ignore local build artifacts"`

### Task 20: Front-page honesty — ROAST files, Packagist metadata, claims, CI budgets

**Files:**
- Move: `ROAST-php-rabbit-rs.md`, `ROAST-rabbit-rs-laravel.md` → `docs/audits/2026-07-xx-roast-*.md` (preserving dates) with a status banner at top: "Superseded findings tracker — status as of v0.3.10: see ROADMAP and the 2026-10-01 audit; open items are tracked in the post-audit plan `docs/superpowers/plans/2026-10-01-post-audit-stabilization.md`."
- Modify: both `composer.json` (add `authors`, `keywords: ["rabbitmq","amqp","laravel","queue","rust"]`, `homepage`) → then refresh Packagist after the next tag; `crates/rabbit-rs-core/Cargo.toml` + `crates/rabbit-rs-php/Cargo.toml` (add `publish = false`)
- Modify: `README.md:93` hero claim → workload-scoped form per `benchmarks/README.md:185` (quote the two numbers + cite `benchmarks/results/round-2-rebench/`)
- Modify: `.github/workflows/ci.yml` + `coverage.yml` (add explicit `timeout-minutes` 20–60 per job), `.github/workflows/homebrew-formula-test.yml` (add `concurrency:` group)
- Optional consolidation: `docs/audit/` → `docs/audits/` (single dir) with a README line; cross-link `examples/laravel/` from the README docs table.

- [x] **Step 1:** Apply edits/moves.
- [x] **Step 2:** Verify: `rtk composer validate --strict` (both), `cargo metadata` parses (publish=false respected), YAML lint of touched workflows, README internal links resolve.
- [x] **Step 3:** Commit: `git commit -m "chore: relocate stale roast docs, complete packagist metadata, scope benchmark claims, bound ci jobs"`

---

## Phase 5 — Contracts & observability polish

### Task 21: Stub/docblock contract fixes

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/delivery.rs:43-48` + `:180-188` (metadata docblock: nested arrays ARE exposed — `array<string, bool|int|float|string|array|null>`; ackThrough spin: re-match the error kind instead of masking state transitions with a generic "channel full" message), `crates/rabbit-rs-php/src/classes/pool.rs:245-258` (`publishBatch` docblock: partial-success semantics on first `Returned`; validate input before the flush so errors name the right operation — swap `ensure_open`/`flush` order), `:166-178` + `consumer.rs` (`ackBatch` docblock: partial settlement on mid-loop failure), `:722-732` (`surface_publish_errors`: count discarded records in `dropped_error_records_total` instead of clearing silently), `crates/rabbit-rs-php/src/classes/publish_buffer.rs:411-422` (sync flush records non-first `Returned` outcomes into the pending-error queue instead of discarding them, matching the pipelined path — `returns_total` already counts each)
- Modify: `scripts/stubs.sh` (post-process: strip/merge the macro-generated duplicate `@return` tag when the docblock already declares one)
- Regenerate: `./scripts/stubs.sh --out crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`

- [x] **Step 1:** Failing test (ext Pest): `metadata()` on a delivery carrying `x-death` returns the nested array (already tested in `NestedHeadersTest` — assert the *stub docblock* change by `php -l` + reading the generated stub in the script step; the behavioral pin already exists).
- [x] **Step 2:** Apply Rust docblock/message fixes; swap `ensure_open("…::publishBatch")` before `flush()`; add the discarded-records counter bump.
- [x] **Step 3:** Regenerate stubs; `php -l`; Run: `./scripts/test-extension.sh` → PASS.
- [x] **Step 4:** Commit: `git commit -m "fix(php-ext): align stubs and docs with shipped metadata/batch semantics and count discarded error records"`

### Task 22: Core observability & misc hardening batch

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/delivery.rs:56-68` (`Debug` redacts header *values* — print keys + payload_len only), `crates/rabbit-rs-core/src/consumer/actor.rs:1506-1509` + `composite.rs:360-364` (close fan-out via `join_all` bounded ~2 s total instead of N × 2 s sequential), `crates/rabbit-rs-core/src/consumer/actor.rs:374-381` (defensive missing-runtime branch `break`s instead of hot-looping), `crates/rabbit-rs-core/src/client.rs:697-702` (plugin detection maps on `ErrorKind::ProtocolError` ids, not `to_string().contains`), `crates/rabbit-rs-php/src/sink.rs:35-44` (`stderr().write_all` + ignore errors instead of `eprintln!`), `crates/rabbit-rs-php/src/callbacks.rs:92-99` (fallback error message when `set_zval` fails so a callback exception is never silently lost)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs` doc comment (document the intentional post-suspend replay ordering: never-attempted publications replay before attempted ones, submission order is not part of the at-least-once contract — audit LOW 3, currently only pinned by the state-machine test)
- Test: extend `tests/consumer.rs` (close fan-out timing with paused time), `tests/log_facade.rs` (redaction), plugin-detection unit test next to `client.rs`.

- [ ] **Step 1:** Failing tests: (a) redacted `Debug` for `Delivery` (no header values in output); (b) close of a 5-subscription set with 4 gated channels completes ~2 s (paused time), not 10 s; (c) plugin detection fires on a `ProtocolError(540, "NOT_IMPLEMENTED")`-style typed error with a different Display text.
- [ ] **Step 2:** Verify failure: `rtk cargo test -p rabbit-rs-core --test consumer close_fanout && rtk cargo test -p rabbit-rs-core --test log_facade` → FAIL.
- [ ] **Step 3:** Implement the six edits above (each minimal).
- [ ] **Step 4:** Verify: `rtk cargo test -p rabbit-rs-core && rtk cargo test -p rabbit-rs-php` → PASS.
- [ ] **Step 5:** Commit: `git commit -m "fix(core): redact delivery debug output, parallelize close fan-out, harden sink and plugin detection"`

### Task 23: Laravel polish batch

**Files:**
- Modify: `packages/laravel-queue/src/Console/DoctorProbe.php:28, 393, 458` (rename `CANARY_DLQ_POLL_MS` → `CANARY_DLQ_POLL_MICROSECONDS`), `src/Support/ProbeStatefile.php:156-161, 186-196` (sweep `*.json.tmp` by age), `src/Console/RabbitMqWorkCommand.php:67` (clamp `--min-workers` floor to 1), `src/RabbitMqQueue.php:165-178` + `:394-404` (docblocks: `delayedSize`/`reservedSize`/`creationTimeOfOldestPendingJob` not implementable over AMQP; `drainSettlementErrors` kind list reconciled with the code), `src/Console/WorkerSupervisor.php:175-182` (resolve child `artisan` against the Laravel base path), `:997-1004` (`stopAllSlots` → non-blocking `posix_kill` + SIGKILL escalation, matching the scale-down path), `README.md:124-132` (add `rabbit-rs:probe` + `RABBIT_RS_PROBES_PATH` rows), `tests/Pest.php:195` (pass `RabbitRsConnections::packageDefaults()` in `integrationPoolAndQueue()` so integration compiles with the shipped prefetch 1000 default instead of the 64 hard fallback — `ConnectionCompiler.php:395`)
- Test: adjust affected Pest tests; add a base-path assertion for the child command.

- [x] **Step 1:** Failing tests: (a) supervisor child command is absolute under a changed cwd; (b) `--min-workers=0` throws a validation error.
- [x] **Step 2:** Verify failure: `cd packages/laravel-queue && php -n vendor/bin/pest tests/Feature/WorkerSupervisorIntegrationTest.php` → FAIL.
- [x] **Step 3:** Apply the edits.
- [x] **Step 4:** Verify: `cd packages/laravel-queue && php -n vendor/bin/pest && rtk composer validate --strict` → PASS.
- [x] **Step 5:** Commit: `git commit -m "fix(laravel): polish supervisor paths, probe statefile sweep and docblock accuracy"`

---

## Final gate + record

- [ ] **Step F.1:** Full quality gate: `rtk ./scripts/check.sh` → PASS.
- [ ] **Step F.2:** Integration + lifecycle: `./scripts/test-integration.sh && ./scripts/test-fpm.sh && ./scripts/test-octane.sh` → PASS.
- [ ] **Step F.3:** If any hot-path file changed (publish/consume actor, scheduler, conversion, publish_buffer): re-run the driver-bench standard protocol on the lab and compare against the frozen budgets in `benchmarks/results/round-*` (non-regression gate) before merging.
- [ ] **Step F.4:** Record the round in `docs/plans/ROADMAP.md` (new "Round L — audit 2026-10-01 stabilization" entry: motivation = this plan; scope = tasks 1–23; status = delivered with PR refs), and reference the consolidated audit findings doc.
- [ ] **Step F.5:** Merge via MR with the audit summary as description; tag a patch release (`v0.3.11`) so the lockstep versioning picks up the FFI/Laravel fixes.

## Findings deliberately NOT in scope (with reasons)

- **Fingerprint narrowing** (client-side knobs in the pool key, TLS-content keying) — behavior/design change with pool-sharing implications; needs its own small design note + migration note (pools re-create once on upgrade). Filed for the round after.
- **`close_claim` TOCTOU** (`pool/mod.rs:108-120`) — unreachable from PHP today; guard-rail fix belongs with the registry-eviction design follow-up.
- **ZTS** — out of policy (V2).
- **Remaining core LOWs** (kept on record so nothing is lost): SNI validated only against the first sorted host on failover (`lapin.rs:44-60`) — document the cert-coverage requirement; `ChannelsLimitReached` mapped permanent without review (`lapin.rs:787-805`) — deserves its own classification; `spin_on_ready` re-runs heavy establishment per loop (`client.rs:794-796`); stale-generation establishment churn window (`recovery_coordinator.rs:853, 875`) — self-correcting; `tokio::spawn` panics outside a runtime context for direct core-API users (`recovery_coordinator.rs:182`, `client.rs:989-1021`); `Draining` state naming vs `PoolLifecycle::Closing`; `Endpoint::new("", port)` accepted (`config.rs:24-31`); `effective_safety` silently downgrades explicit `safety: "safe"` with legacy `confirms: false` (`config.rs:494-503`).
- **Sonar duplication / docs/audits directory consolidation beyond Task 20** — cosmetic; batch later.

---

## Phase 3 appendix — graduated from the round-l profile decision gate

### Task 24: Eliminate duplicate per-publish property string clones

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/delay.rs:142-158` (`route_transport_request` — stop `.to_owned()`-ing `content_type`/`correlation_id`/`message_id` into the transport `PublishRequest` per publish) and `crates/rabbit-rs-core/src/transport/lapin.rs:665-693` (`publish_properties` — stop re-`clone()`-ing the same strings into `BasicProperties`; share one `Arc<str>`/borrow through both sites)
- Callers to keep compiling: `crates/rabbit-rs-core/src/publisher/pump.rs:269`, `crates/rabbit-rs-core/src/publisher/actor.rs:769`
- Test: `crates/rabbit-rs-core/tests/` (publisher suite — property parity: wire properties byte-identical before/after)

**Context:** The round-l profile (`benchmarks/results/round-l-profile/README.md`, decision gate candidate 4) measured the per-publish property conversion chain at ≈0.3–0.8 µs of the extension-boundary publish p50 (paired A/B `--props=minimal|full`: +0.33/+1.08/+0.21 µs; allocator-family self-time ≈0.8 µs/publish across main/actor/io threads): `route_transport_request` allocates fresh `String`s for every property on every publish and `publish_properties` clones them again into AMQP values, although the data flows unchanged from the PHP zval to the wire. This was the only profile-proven candidate; the other four audit candidates were rejected with numbers (scheduler pick 283.8 ns/pick @32 subs; early-ack spawn +0.6 µs p50/+0.11 µs CPU per delivery; MessageId clones ≤0.15 µs/delivery; budget formatting 15 ns/publish).

- [x] **Step 1: Characterization test** — a publisher test asserting the wire `BasicProperties` (content_type, correlation_id, message_id, headers) produced for a fixed request are byte-identical across the refactor (golden comparison over the mock transport), so the optimization cannot change what goes on the wire.
- [x] **Step 2: Verify green-then-refactor** — the characterization test passes pre-change (no behavior change intended; this is a perf refactor, so the gate is the benchmark, not a failing test).
- [x] **Step 3: Implement** — change the `transport::PublishRequest` property fields to `Arc<str>` (or `Option<Arc<str>>`) fed from the core request without a fresh allocation per publish, and have `publish_properties` borrow/clone only the `Arc` pointer into `AMQPValue::LongString` ( constructing the wire value may still need one owned string — keep exactly one, not two). No API breaks outside `pub(crate)`/`transport` internals; if `PublishRequest` is publicly constructible, add the field-type change to the changelog as a minor breaking change or add a constructor preserving the public shape.
- [x] **Step 4: Verify** — `rtk cargo test -p rabbit-rs-core` and `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings` PASS; re-run the round-l props A/B on the lab: `RABBIT_RS_SAFETY=safe php -n -d extension=<dylib> benchmarks/results/round-l-profile/tools/micro-publish.php --iters=50000 --props=full` ×3 — the `full` p50 median must move measurably toward the `minimal` floor (≈1.2 µs) without regression in blind/unsafe; record the before/after JSONs under `benchmarks/results/round-l-profile/` (post-fix addendum).
- [x] **Step 5: Commit** — `git commit -m "perf(core): share per-publish property strings instead of re-cloning them onto the wire"`
