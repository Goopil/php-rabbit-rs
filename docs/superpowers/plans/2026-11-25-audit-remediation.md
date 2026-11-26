# November 25 Audit Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task by task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve every November 25 audit finding, including any additional defects established by the companion real-broker investigation, and make the documented delivery guarantees match the behavior actually supported.

**Architecture:** Fix independently reproducible errors first, while the companion plan characterizes broker behavior. Gate the choice of delay, dead-letter and retry designs on recorded evidence, compatibility and explicit guarantee boundaries. Every behavior change starts with a focused failing test, then a minimal implementation and regression checks at the affected Rust, extension, Laravel and real-broker boundaries.

**Tech Stack:** Rust 1.98.1, Tokio, Lapin behind `Transport`, ext-php-rs, PHP 8.2+, Laravel 12/13, Pest, RabbitMQ lab, Toxiproxy, `rtk`.

## Global Constraints

- Treat `docs/plans/2026-07-30-rabbitmq-native-design.md`, `docs/plans/2026-07-30-rabbitmq-native-implementation.md` and the root `AGENTS.md` as sources of truth. Reconcile changed contracts and milestones in those documents when a task completes; all repository artifacts are English.
- At-least-once forbids silent loss inside the documented confirmed path; duplicates are permitted only with an honest identity and observability contract. In-memory replay does not survive a PHP process crash; never claim otherwise.
- Unsafe Rust is forbidden; preserve the transport abstraction, bounded queues/retries/buffers, generation-aware tokens and deterministic recovery order. Do not expose credentials or keep PHP/Zend state on Rust threads.
- Preserve unrelated dirty work. The original audit was against a local `main` 15 commits behind `origin/main`; re-check the exact revision and existing fixes before changing code. Do not reset, clean, pull, or commit unrelated changes.
- Behavior changes follow TDD: add the focused failing test and observe the intended failure; implement; rerun focused tests and `rtk cargo fmt --all` after Rust edits. Use paused Tokio time and mock transport for asynchronous unit tests, not real sleeps.
- The companion is `docs/superpowers/plans/2026-11-25-real-broker-reliability-investigation.md`; its report is `docs/audit/2026-11-25-real-broker-evidence.md`. The report is future work until it contains actual observations. An inconclusive or blocked experiment cannot close its corresponding finding.
- The lab scripts delete lab volumes and the integration runner invokes `composer update`: follow the ownership and isolation rules in the companion plan; do not run them against shared or non-disposable state.

## Dependency and decision gates

| Audit finding | Plan task | Required investigation input |
|---|---|---|
| TTL transfer can lose messages | 4 | Investigation Task 2, default and at-least-once policy evidence |
| Plugin confirm does not prove final routing | 5 | Investigation Task 3, binding-removal and proxy evidence |
| Multiple drained errors disappear | 1 | Investigation Task 5 for end-to-end coverage; unit fix may start immediately |
| Classic immediate retries stop counting | 6 | Investigation Task 5, classic and quorum evidence |
| Retried bulk gets new IDs | 3 | Investigation Task 5, partial-success evidence; unit fix may start immediately |
| TTL queue expiry and keep-alive gaps | 4 | Investigation Task 4, including explicit inconclusive outcomes |
| Invalid Laravel payload passes poison validation | 2 | Investigation Task 5 for worker behavior; unit fix may start immediately |
| `url` advisory | 7 | Current `cargo-deny` output and dependency compatibility |
| Bulk quadratic copying and memory | 8 | Investigation Task 6 baseline and post-change measurements |
| Conflicting delay documentation | 5 and 9 | Effective-mode evidence from Investigation Task 3 |

If investigation refutes a proposed failure, retain the passing regression and record the evidence and contract implications rather than forcing an unnecessary fix. New findings from the report must be added as tracked tasks with tests and an owner before the final gate.

## Task 1: Account for every asynchronous outcome

**Files:** Modify `packages/laravel-queue/src/RabbitMqQueue.php`; test `packages/laravel-queue/tests/Unit/RabbitMqQueueDrainErrorsTest.php` and `tests/Integration/PublishErrorSurfacingTest.php`. Inspect `crates/rabbit-rs-php/src/classes/publish_buffer.rs` and `classes/pool.rs` for existing bounded queues and counters.

**Interfaces:** Every record returned by `Pool::drainErrors(): array` and `Consumer::drainErrors(): array` must be either surfaced to the caller, retained in a bounded queue for later surfacing, or logged with its original message/delivery identity. If capacity is exceeded, preserve an explicit counter and observable error; do not silently drop. Preserve the first actionable exception's existing type.

- [ ] Add unit cases for two publish errors with distinct IDs, mixed publish `Transport` and `Returned`, and consumer `Transport` followed by `MaxAttempts`/`InvalidDelay`; call the drain again and inspect logs/counters. Each record must be accounted for once and poison records must retain their error-level context.
- [ ] Run `rtk ./scripts/test-laravel.sh tests/Unit/RabbitMqQueueDrainErrorsTest.php` and record the expected missing-second-record failure before edits.
- [ ] Implement bounded accounting without logging credentials or payloads. Ensure a throwing logger or consumer cannot silently erase records belonging to other consumers. Preserve exception classification and teardown behavior, and document any unavoidable aggregation semantics.
- [ ] Rerun the focused unit test and the real-broker multiple-return scenario from Investigation Task 5. Verify the native overflow counter is not falsely reported as zero for discarded records.

## Task 2: Validate Laravel job payload shape

**Files:** Modify `packages/laravel-queue/src/Jobs/RabbitMqJob.php` and, if needed, `RabbitMqQueue.php`; test `packages/laravel-queue/tests/Unit/` and `tests/Integration/PoisonDeliveryTest.php`.

**Interfaces:** Construction rejects syntactically invalid JSON and JSON that cannot be interpreted as a Laravel job with `InvalidArgumentException`, so `pop()` routes it through the existing poison policy. Preserve valid Laravel 12/13 object and string job payloads, encrypted jobs, custom handlers, and `pushRaw` payloads that satisfy Laravel's actual requirements.

- [ ] Add cases for `null`, scalar, `[]`, `{}`, missing/empty/non-string `job`, malformed `data`, invalid JSON, and actual payloads generated for object and string jobs. Assert the malformed cases use the existing poison settlement path exactly once and valid controls still execute.
- [ ] Run the new focused Pest tests and observe the current failures. Check Laravel 12 and 13 payload expectations before enforcing a `data` type; do not reject a valid framework payload on assumption.
- [ ] Implement the smallest validated schema required for safe worker dispatch. Avoid logging the raw body; keep message ID, reason and settlement outcome actionable.
- [ ] Run focused Unit/Feature tests and the live invalid-payload scenarios from Investigation Task 5 with and without a configured DLQ.

## Task 3: Honest identity and partial success in bulk publication

**Files:** Modify `packages/laravel-queue/src/RabbitMqQueue.php`, `src/Support/MessageMapper.php` as needed, and `packages/laravel-queue/docs/reference.md`; test `tests/Feature/BulkChunkingTest.php` and `tests/Unit/`.

**Interfaces:** Replaying a *prepared publication* retains its ID and payload; a fresh `bulk()` call cannot magically recover the prior generated UUID. Either provide a compatible, caller-controlled stable identity mechanism for logical retries, or expose sufficient partial-success information to let callers retry only unresolved work. Never derive IDs solely from payload content in a way that conflates intentionally identical jobs. Document the chosen API and migration behavior.

- [ ] Add a test where chunk 1 succeeds and chunk 2 fails; retry the same logical jobs and capture all published IDs. Include two intentionally identical jobs, after-commit jobs and a no-failure control. Demonstrate the current new-ID behavior and the incorrect existing documentation claim.
- [ ] Evaluate Laravel 12/13 queue and Horizon contracts before changing `bulk()` return types or exception classes. Select an API preserving compatibility; define how callers persist stable IDs or identify completed chunks across process boundaries. If no compatible API exists, correct the guarantee immediately and explicitly track the new API/migration as part of this task.
- [ ] Implement the selected bounded mechanism and focused tests. Confirm publisher recovery of one prepared request retains its original ID independently of application-level redispatch.
- [ ] Run `rtk ./scripts/test-laravel.sh tests/Feature/BulkChunkingTest.php`, relevant Horizon tests and the companion real-broker partial-success scenario. Update examples to show a usable idempotency key or retry strategy.

## Task 4: Durable delayed transfer and bucket lifecycle

**Files:** Inspect and modify as evidence requires `crates/rabbit-rs-core/src/topology/delay.rs`, `publisher/delay.rs`, `pool/recovery_coordinator.rs`, `transport/lapin.rs`, configuration/compiler files, and delay/poison tests. Update core and Laravel delay documentation.

**Interfaces:** A confirmed TTL-delayed message is either retained until a confirmed transfer to a usable destination or the API/documentation explicitly rejects that guarantee before acceptance. Configuration changes and process shutdown must not delete a bucket while it still contains confirmed work. All storage limits and failure behavior must be explicit.

- [ ] Read Investigation Tasks 2 and 4 and the exact broker-version/policy evidence. Add focused Rust tests for generated queue specs, stable queue identity, policy/argument selection, recovery and old-bucket lifecycle; first show the current unsafe or unverified behavior. Add a broker acceptance test for a destination unavailable through expiry and restored afterwards.
- [ ] Choose a compatible design based on those results: enable RabbitMQ quorum at-least-once dead-lettering and `reject-publish` through supported queue arguments or a verified operator policy; or use another explicitly designed confirmed transfer. Verify feature/version prerequisites and queue-limit/backpressure behavior. Do not call a configured policy effective unless the broker reports it.
- [ ] Eliminate dependence on a live application keep-alive for survival of confirmed delayed work. If removing `x-expires`, provide bounded, explicit GC that verifies emptiness and handles races; if using another lifecycle, prove it survives publisher exit, configuration rotation and recovery. Address already-created queues and rolling-upgrade compatibility: broker queue arguments may be immutable.
- [ ] Test default and external/verify topologies, unavailable exchange/binding, cluster leader changes, restarts, process exit, configuration rotation, bounded backlog and duplicate IDs. If a scenario remains inconclusive, keep this task open and retain the operational limitation prominently in the docs.
- [ ] Run `rtk cargo fmt --all`, `rtk cargo test -p rabbit-rs-core --test publisher`, `rtk cargo test -p rabbit-rs-core --test topology`, `rtk cargo test -p rabbit-rs-core --test recovery`, and the relevant broker tests.

## Task 5: Plugin routing and effective delay contract

**Files:** Inspect `crates/rabbit-rs-core/src/publisher/delay.rs`, `src/topology/plan.rs`, Laravel connection compiler and delay guard; modify the necessary paths; update `docs/reference.md`, `packages/laravel-queue/docs/reference.md`, and `packages/laravel-queue/docs/recipes.md`.

**Interfaces:** Never represent a publisher confirm with `mandatory=false` as proof of future routing. If future delivery cannot be assured for plugin mode, fail closed where required or expose and prominently document the reduced guarantee and operational prerequisites. A one-time binding probe cannot prove a binding will remain present until expiry.

- [ ] Use Investigation Task 3 to add broker regressions for a binding absent at publication and removed after confirm; include a healthy binding control and external/verify modes. Add unit tests for effective `auto`, plugin and TTL selection under present, absent and unreachable management API outcomes.
- [ ] Decide and implement the supported product contract from evidence: e.g. route reliability-critical delays through a confirmed durable strategy, or make plugin mode an explicitly weaker opt-in. Do not claim a preflight check closes the race. Preserve immediate safe-mode mandatory returns.
- [ ] Correct all descriptions of `auto`, plugin mandatory behavior, runtime fallback and publish confirmations; align both native and Laravel behavior without assuming they resolve `auto` identically. Make compatibility and migration notes concrete.
- [ ] Run the delay unit tests and broker scenarios for both lab profiles; ensure a missing plugin or unverifiable management API produces exactly the documented result.

## Task 6: Classic retry counting and poison behavior

**Files:** `crates/rabbit-rs-core/src/consumer/attempts.rs`, `consumer/actor.rs`, related transport/delivery tests, and Laravel retry documentation.

**Interfaces:** Document separately the guarantees for classic and quorum queues, immediate requeue, delayed republish, crashes and broker redelivery. Never claim a monotonically increasing attempt count when RabbitMQ supplies only a boolean redelivery flag. If preserving counts requires republish, confirm the replacement before acknowledging the original and preserve message identity; explain duplicate and crash windows.

- [ ] Add deterministic tests for four immediate classic redeliveries, quorum delivery counts, delayed retries, and the configured maximum; reproduce the stagnant count. Check Investigation Task 5 against the supported RabbitMQ versions and Laravel's `tries` behavior.
- [ ] Select the supported correction: confirmed republish with a preserved/incremented application header where it actually solves the observed path, or an explicitly rejected/documented classic configuration where broker crash redelivery makes durable counting impossible. Do not use a process-local counter as a cross-process guarantee.
- [ ] Verify no ACK precedes replacement confirmation, terminal poison policy is visible, and a crash/connection loss allows redelivery. Run `rtk cargo test -p rabbit-rs-core --test consumer`, `rtk cargo test -p rabbit-rs-core --test poison`, and the real classic/quorum scenarios.

## Task 7: Dependency advisory and quality gate

**Files:** `Cargo.lock`; `Cargo.toml` or `crates/rabbit-rs-core/Cargo.toml` only if a constraint change is necessary. Advisory: `RUSTSEC-2026-0592` for `url <=2.5.8`.

- [ ] Run `rtk cargo deny check advisories` and record the current advisory and dependency tree; do not assert exploitability without a demonstrated call path.
- [ ] Run `rtk cargo update -p url --precise 2.5.9` if compatible and available, or select the smallest newer compatible release. Inspect the lockfile diff for unrelated updates. If the registry is unavailable, preserve the failure and follow the environment's network permission workflow.
- [ ] Run `rtk cargo deny check`, `rtk cargo test -p rabbit-rs-core --test transport_tuning`, and the full quality gate. Check compatibility with the pinned Rust toolchain and supported platforms; document any outstanding advisory rather than suppressing it.

## Task 8: Bulk and keep-alive performance

**Files:** `packages/laravel-queue/src/RabbitMqQueue.php`; existing `benchmarks/` harness or a focused new benchmark; relevant bulk tests. Modify keep-alive code only if the measurements justify it.

- [ ] Capture the baseline defined in Investigation Task 6: same PHP version, batch sizes, payloads, broker setup and warmup for each run; report elapsed time, peak RSS, chunk count and broker operations. Preserve raw sanitized results.
- [ ] Add a behavioral regression proving order and IDs across multiple chunks, then replace repeated spread-copy accumulation with append semantics. Consider streaming preparation only if it preserves Laravel events, after-commit semantics, bounded failure behavior and a demonstrable memory benefit.
- [ ] Measure the same workloads again and compare median and variability over at least five repetitions. Do not claim a performance gain without measurements; retain only changes that preserve correctness and have a justified cost/benefit.
- [ ] If keep-alive channel churn is material in Task 6 and Task 4 retains that mechanism, profile it and add bounded concurrency/reuse only with a test for failed passive declarations and channel closure isolation. Otherwise record the measured result and do not introduce speculative complexity.

## Task 9: Documentation, integration and final closure

**Files:** `docs/reference.md`, `packages/laravel-queue/docs/reference.md`, `packages/laravel-queue/docs/recipes.md`, relevant README/getting-started pages, both source-of-truth plans, and the evidence report.

- [ ] Build a traceability table in the evidence report linking every finding to its regression test, actual evidence, code or documentation change and remaining operational constraints. Record newly discovered findings and do not mark them closed without evidence.
- [ ] Check that immediate, TTL, plugin, classic, quorum, process-crash, DLQ, retries and duplicate-identity guarantees are mutually consistent across docs. Include concrete operating requirements and failure behavior; remove unsupported claims rather than hiding them in footnotes.
- [ ] Run the focused real-broker suite under `with-plugin` and `without-plugin`, including confirmed Toxiproxy faults, and the existing integration suite on an owned disposable lab. Verify no skipped required scenario is treated as a pass. Run the relevant PHP extension tests and `rtk ./scripts/test-laravel.sh`.
- [ ] Run `rtk cargo fmt --all -- --check` and `rtk ./scripts/check.sh`. Report exact failures, skipped tools and reasons. Run `rtk git status --short` to verify unrelated user changes are intact and update the implementation plan's progress/next step.
- [ ] **Closure criteria:** all ten findings have a linked test or explicit evidence-backed correction, no confirmed loss is hidden behind a documentation change, outstanding inconclusive cases are tracked as open limitations, all required gates pass, and results identify the exact revision and broker/plugin versions tested.
