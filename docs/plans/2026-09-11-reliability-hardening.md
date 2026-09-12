# Reliability hardening — generative invariant testing (2026-09-11)

## Context

A reviewer assessment (2026-09-11) concluded the core mechanisms are sound but
lack *systematic proof that the reliability invariants hold under generated
interleavings*. The v1 readiness epic (#230) already owns runtime
certification (WS5), CI trust (WS1), and performance baselines (WS8/D14).
This plan covers the piece that epic does not: property-based state-machine
tests over the mock transport, executed **in parallel with wave 1** of the
epic as a strictly test-only stream (no source files, no `ci.yml` changes).

## Wave 1 (this plan, executed now)

Property-based state machines (`proptest-state-machine`) driving the
scriptable mock transport. After every generated transition, the invariants
below are checked against a reference model. Time runs real: the buffer's
`Handle::block_on` facade requires a multi-thread runtime, where paused Tokio
time is unsupported — clock-dependent behavior is therefore covered through
an oversized `flush_interval` (the timer can never fire) and pre-expired
publication deadlines instead of clock advance.

### Invariants (from the reviewer assessment, mapped to the code)

1. Every accepted publication resolves exactly once: confirmed, returned
   (unroutable), or counted once in `dropped_publications` — never two of
   those, never silent.
2. A replayed publication carries the same `message_id` and its original
   deadline; a deadline expired while parked is dropped exactly once
   (never re-buffered past its deadline).
3. No work is left stuck: after `quiesce`/`flush_teardown`, pending-error
   draining and further operations produce no new records and the buffer is
   empty; drain timers/permits cannot leak past `quiesce` (bounded by
   `TEARDOWN_FLUSH_BUDGET`).
4. Memory budgets stay explicit: buffered messages/bytes track the model
   exactly; the ceiling refusal path (`would_overflow`) is exercised, and
   the bounded pending-error queue evicts oldest-first, counted in
   `dropped_error_records`.
5. A stale confirmation (earlier generation) never finalizes a delivery of
   the current generation (delivery tokens are connection-generation-aware).

### Machine 1 — `PublishBuffer` (crates/rabbit-rs-php)

Pure-Rust `#[cfg(test)]` module driving the buffer's public operations with
the mock transport: enqueue (healthy and pre-expired deadlines),
threshold-triggered, explicit, pop, and teardown flushes, plus scripted batch
outcomes (all acknowledged, first returned, first non-recoverable failure)
and pool close. Confirmed facts baked into the model: pre-expired
publications are rejected by the publisher actor at mailbox-processing time —
before any wire write and without consuming a confirmation — and resolve as
per-message timeouts whose kind folds into the batch-level error; a
batch-level failure re-buffers the conservative superset (deadline filter
applied) or drops it on a closing pool/teardown; only the pipelined drain
records pending errors (synchronous paths raise, teardown stays silent).

### Machine 2 — publisher replay across recovery (crates/rabbit-rs-core)

Integration test driving the publisher path across scripted connection
losses: unconfirmed publications are replayed with identical `message_id`
and original deadline, stale confirmations from the lost generation are
dropped, and every waiter resolves exactly once.

### CI

Runs inside the existing gate (`cargo nextest run --workspace
--all-targets`, no workflow changes). Deterministic budget: `PROPTEST_CASES`
env var set in local `scripts/check.sh` invocations stays at the default;
the regressions files (`*.proptest-regressions`) are committed so shrinking
results persist. Runtime target: < 60 s for both machines combined.

## Backlog (decided, not executed in this wave)

| Item | Decision | Notes |
|---|---|---|
| `connection.blocked`/`unblocked` handling | Post-1.0 issue | Behavior today is bounded (confirm timeouts + deadlines, no infinite stall); the gap is a *measured signal*, not a blocker. Lab can trigger real alarms via `rabbitmqctl set_vm_memory_high_watermark`. |
| Broker-side soak assertions (connections/channels stable, no unacked residue, no orphan TTL queues, broker memory) | Post-1.0 | Extends `benchmarks/driver-bench/bin/soak.php` with management-API checks. |
| Race/cancellation matrix beyond machine coverage (PHP object destruction mid-publish, concurrent consumers) | Partially absorbed | Machine 1 covers buffer-level interleavings; PHP-runtime cancellation belongs to WS5-style runtime certification. |
| PHP↔Rust boundary fuzzing | Post-1.0, CI-scheduled | `cargo-fuzz` targets on `conversion.rs` (headers, sizes, invalid configs). Requires nightly; never in the PR gate. |
| Performance regression gates | Owned by v1 epic | WS8 (#229) does repo-stored baselines + RC budget check (D14); CodSpeed (#158) stays post-1.0. |

## Coordination with the v1 epic (#230)

- This wave is parallel, non-blocking, test-only: dev-dependencies
  (`proptest`, `proptest-state-machine`, `test-support` feature for the ext
  crate's dev builds) plus new test files and this document.
- WS2 (#223) owns the refcounted handle fix for #221 (direction 1; the
  probe-fingerprint mitigation was rejected). It will unpin the pool-free
  `RouteBindingTest` pin introduced for the shared-handle bug.
- If a generated counterexample exposes a real product bug: stop, file a
  dedicated issue, do not fix inside this wave (the fix may belong to a
  v1 workstream).
