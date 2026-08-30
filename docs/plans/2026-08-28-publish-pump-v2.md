# Publish Pump v2 — true fire-and-forget

Status: executed — merged via PR #30 (d923fe1, 2026-08-28). Benchmarks now run in release
mode (debug masks ~4×). Next: `2026-08-29-post-pump-simplification.md`.

Date: 2026-08-28
Base: `perf/publish-optimizations` (2b8a9b5) → merge PR #29 → new branch `perf/publish-pump`

## Context

The former extension (Goopil/php-ext-rabbit-rs, archived) reached much higher publish
fire-and-forget throughput thanks to: a dedicated pump with `FuturesUnordered` (2048
in-flight), `tokio::select! biased` overlapping intake and drain, a 4096 flume channel,
a multi-thread runtime, and a PHP path that **returns after enqueueing** (never waiting
for outcomes).

The current rabbit-rs blind path goes through the actor (`try_publish_hot` =
`try_publish`): semaphore + oneshot + Box + HashMap + metrics **per message**, then
sequential awaiting of outcomes in the same `block_on` (client.rs:173-183). The existing
pump (pump.rs) is sequential (1 lapin publish at a time) and saturated (flume 1024).

User goal: ≥ 80-100k msg/s publish in fire-and-forget. Beyond that, pointless.

## Settled decisions

- A (pump v2 + blind routing): GO without reservation.
- B (PHP/outcome decoupling): inherent to blind routing towards the pump; decided after
  measurement. The post-bench decision point is about slimming the actor path
  (Safe/Unsafe), not blind.
- Blind semantics: backpressure = **blocking** (`send_async` on the bounded flume, like
  the old one), not an error. Transport error after enqueueing = silent loss (true
  fire-and-forget, documented). Safe/Unsafe modes unchanged (actor, at-least-once,
  replay).
- Flush barrier: port of the old pattern (`flush_fire_and_forget`) — `Pool::flush()` in
  blind mode waits until everything enqueued has been handed to lapin.

## Global constraints

- `#![forbid(unsafe_code)]` untouched; no weakening of workspace lints.
- Rust 1.96.0, edition 2024. `rtk cargo fmt --all` after every Rust edit.
- Full gate before declaring a task done: `rtk ./scripts/check.sh` (or clippy + workspace
  test + composer validate for non-Rust tasks).
- At-least-once for Safe/Unsafe unchanged. Blind = explicit opt-in, documented semantics.
- TDD: focused failing test first, minimal implementation, re-run.
- Mock transport + paused tokio time for deterministic async tests. No real sleeps.
- No credentials/complete URIs in Debug/errors/metrics/logs.
- Preserve unrelated work in the tree. Logical, scoped commits.

## Phase 0 — Housekeeping merge (on `perf/publish-optimizations`)

### Task 1 — Restore Task 13 re-buffering in flush_publishes

The publish optimization commit (`7d3b20f`) replaced the `flush_publishes` re-buffering
(pool.rs) with a `publish_batch` call that throws without re-buffering. The comments at
pool.rs:289-290 and 309 claim otherwise — they are wrong.

Reference: commit `7bc5c88` (« fix(ffi): re-buffer remaining messages on
PublishOutcome::Returned mid-flush ») — restore the semantics adapted to the batch path:

- `Err(error)` from `publish_batch`: re-buffer **all** flush messages into
  `publish_buffer` (order preserved), then raise the exception. Exception: `Closed`
  error (pool dying) → no re-buffer, raise the exception.
- `Ok(outcomes)`: zip outcomes and requests by index. `Confirmed` → `publish_message_id`.
  `Returned` → re-buffer the affected request + raise on the first `Returned`
  (other already-resolved outcomes stay reported). Per-message outcome errors
  (backpressure etc.) → re-buffer the affected request, first error raises.
- Duplicates are permitted and identifiable via `message_id` (at-least-once contract).
- Tests: re-adapt the ones from commit `7bc5c88` (see `git show 7bc5c88`) to the batch
  path. Pool FFI tests run via `./scripts/test-extension.sh`.

### Task 2 — Docs cleanup for `max_in_flight`

Fields removed but docs remained (minors deferred from the consumer-tuning review):

- `packages/laravel-queue/README.md`: `max_in_flight` / `BackpressureDetected` sections
- `packages/laravel-queue/config/rabbit-rs.php`: `RABBIT_RS_MAX_IN_FLIGHT` (ignored by
  the normalizer)
- `docs/configuration.md`, `docs/troubleshooting.md`: `max_in_flight` references
- `benchmarks/src/Drivers/RabbitRsDriver.php`: `max_in_flight => 1024` (harmless, remove)

### Task 3 — Merge

`git merge main` into `perf/publish-optimizations` (27 CI/docs commits), gates, push,
merge PR #29 via `gh`.

## Phase 1 — Pump v2 (branch `perf/publish-pump` from post-merge main)

### Task 4 — Pipelined pump v2 (pump.rs)

Rewrite of `pump_loop` following the old model
(`/tmp/php-ext-rabbit-rs/src/core/channels/channel_publisher.rs`, `start_pump_if_needed`):

- `flume::bounded(config.buffer_capacity)` (intake queue, default 1024).
- In-flight cap: `config.buffer_capacity.saturating_mul(2).max(128)` (default 2048).
- `tokio::select! { biased; }`:
  1. completion drain: `Some(_) = inflight.next(), if !inflight.is_empty()`
  2. intake: `maybe_job = rx.recv_async(), if inflight.len() < inflight_cap` — push the
     `channel.publish(request)` future into `FuturesUnordered`, then non-blocking drain
     (`while inflight.next().now_or_never().flatten().is_some() {}`)
  3. `else => break` (sender dropped AND in-flight empty)
- Barrier job: `PumpJob { barrier_tx: Option<oneshot::Sender<()>> }` — on receiving a
  barrier: drain the whole in-flight (`while inflight.next().await.is_some() {}`) then
  respond.
- `PublishPump::try_publish` (try_send, non-blocking) kept for compatibility, no longer
  used by the main blind path — see Task 5.
- New: `PublishPump::send(request)` async — `rx.send_async(job).await` (backpressure by
  blocking); and `PublishPump::flush()` async — barrier + await.
- Recovery: verify/wire `clear_channel()` on Recovering events and `update_channel()` on
  Ready (the `ArcSwapOption` plumbing already exists). On channel `None`: queued jobs
  are ignored (drop, blind semantics) — no error.
- Blind publish error: discreet metric log + drop (no replay, no waiter).

### Task 5 — Blind routing to the pump

- `PublisherHandle`: expose `publish_blind(request)` async → `into_transport_request` +
  `pump.send(request).await` (backpressure by blocking, no error unless pump closed) +
  return `PublishWaiter::resolved(Confirmed)` (the outcome is never read in blind batch).
  Keep `try_publish_blind` (try_send) for existing non-blocking uses.
- `client.rs publish_batch`: blind branch → `publisher.publish_blind(request)` per
  message, **no outcome wait**, return after full enqueue. Errors: closed pump only.
- `client.rs publish`: blind branch → `publish_blind(...).await` (already on the pump,
  but via blocking send instead of try_send + error).
- `pool.rs`: `flush()` in blind mode → `flush_blind()` (barrier) to guarantee
  "everything enqueued before flush is handed to lapin on return" — port of
  `flush_fire_and_forget`.
- Safe/Unsafe modes: **no change** (actor, wait_all, replay, ledger).
- Doc: `SafetyMode::Blind` = explicit fire-and-forget; transport error after enqueueing =
  silent loss; backpressure = blocking of the calling thread (bounded). Update
  `docs/configuration.md` + `SafetyMode` doc-comment.

### Task 6 — Tests + gates

- Pipelining test: mock transport with a gate — M messages enqueued while the I/O is
  blocked; M ≤ in-flight cap accepted without blocking (biased select drains during
  intake).
- Backpressure test: full queue → `send` blocks (paused time / gate) and unblocks on
  drain.
- Barrier test: `flush()` returns only after everything enqueued was handed to lapin.
- Recovery test: `clear_channel` → enqueue OK without error (drop), `update_channel` →
  recovery.
- Routing test: blind publish_batch does not touch the actor (mock: the actor receives
  no Publish command in blind); Safe/Unsafe still go through the actor (existing tests
  stay green).
- Full gate + `./scripts/test-extension.sh` + `./scripts/test-laravel.sh`.

### Task 7 — Interleaved benchmark

- Build ext debug, RabbitMQ lab. 2 alternating runs main vs branch, queues purged
  between runs.
- Report F&F publish/consume, batch-confirm, auto-ack + p99 in the ledger.
- Target: ≥ 80k msg/s F&F publish. Otherwise → decision point (per-batch barrier?
  actor diet?).

## Phase 2 — Optional A/Bs (post-measurement)

- Task 8: `worker_threads` 1 vs 2 (runtime.rs) — 2 runs, keep the best, documented.
- Task 9: ext release vs debug — 1 run each, decide whether benchmarks move to release.
- Task 10 (contingent): actor path diet (Command::PublishBatch, batched metrics) if
  Safe/Unsafe call for more throughput.
