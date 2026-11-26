# Reliability investigation handoff

## Latest checkpoint update

This update supersedes conflicting status statements below. Work resumed after the first handoff. Both Laravel regression tests (reentrant logger and older extension on close) were first observed failing, then fixed. A message ID accidentally matching a substring in an error was also covered by a failing-then-passing regression. The full Laravel suite passed 626 tests / 2,027 assertions. The full extension Pest and PHPT suites passed before the latest blind-close change.

Independent review then identified that `Pool::close()` no longer waited for the blind-mode publication pump. A deterministic transport-gate regression failed before the fix and passed afterwards. Rust formatting and full workspace clippy passed after that fix. The final full Rust, extension and broker checks have not yet been rerun after the last change.

Before the blind-close change, `scripts/check.sh` passed 500 Rust tests, composer, Pint and PHPStan, then failed cargo-deny on the pre-existing RUSTSEC-2026-0592 advisory. An existing separate branch contains the URL dependency fix.

Next: rerun focused and full extension tests and the real-broker publish-error test; rerun the complete gate. Ask the reviewer to recheck the blind close fix. The settlement error ring still evicts on overflow without a counter; add TDD coverage and observable accounting before declaring remediation Task 1 complete. The Laravel destructor can drain before the native teardown produces its final errors; this remains a known gap. Continue investigation of delayed routing, and the remaining tasks in the plans. Do not claim the entire audit is resolved.

This file is a checkpoint aid, not final project documentation; remove it from a final PR. The user requested local checkpoint commits on both worktrees; inspect the branches and commits before resuming. No PR has been opened.

This is a local handoff, not a completed implementation or a file intended for a pull request. Work is paused at the user's request. Preserve all existing worktrees and uncommitted changes.

## Isolation and scope

- Main checkout: `/Users/zacharyvolpi/dev/perso/rabbit-rs`. It is on `docs/laravel-usage-guide`, with unrelated changes and untracked copies of both plans. Do not modify or clean it as part of this work.
- Investigation worktree: `/Users/zacharyvolpi/.codex/worktrees/7f3e/rabbit-rs`, branch `test/broker-reliability`.
- Remediation worktree: `/Users/zacharyvolpi/.codex/worktrees/37f7/rabbit-rs`, branch `fix/async-error-accounting`.
- Neither branch has been committed or submitted as a PR. Both contain intentional, uncommitted work. The separate `fix/url-rustsec-2026-0592` branch belongs to existing work and must not be modified or duplicated.
- Read `/Users/zacharyvolpi/.codex/RTK.md` and project instructions before resuming. These worktrees are outside the configured writable roots; writes and build commands require the appropriate sandbox authorization.

## Investigation branch

Plans: `docs/superpowers/plans/2026-11-25-real-broker-reliability-investigation.md` and `2026-11-25-audit-remediation.md`. Evidence: `docs/audit/2026-11-25-real-broker-evidence.md` in the investigation worktree.

`RealBrokerReliabilityTest.php` has two passing real-broker/Toxiproxy tests (31 assertions): publication and ID, plus actual socket interruption and recovery. Experimental delayed-routing tests are outside the standard suite. Confirmed delayed publications were not delivered within observation windows after their binding was removed and restored; this does not yet prove their final fate. A possible missing custom delayed-exchange binding needs a dedicated positive-control regression. Details and limits are in the evidence file.

The isolated Docker Compose project is `rabbitrs-audit-7f3e`. It was started for these tests and was not stopped during handoff; its current state has not been rechecked. Some experimental quorum queues were retained because conditional deletion was refused by RabbitMQ. Do not use the default lab up/down scripts: they can run `down -v` against a shared default project. Only inspect or manage the explicitly named audit project and its own resources.

## Remediation branch

The current changes aim to preserve and surface asynchronous publish and settlement errors instead of silently discarding surplus records. A real-broker publish-error retention test has passed. A previous full Laravel suite passed 623 tests and 2,019 assertions, before the last two regression tests were added. An earlier `scripts/check.sh` run passed 500 Rust tests, formatting, clippy, composer and Pint, then failed PHPStan on the `method_exists` compatibility check; an earlier run reached cargo-deny and reported the baseline `url` advisory covered by the separate branch.

The last native change preserves the existing nonthrowing publication-error behavior of explicit `Pool::close`, while keeping its publication failures available through `drainErrors()`. The extension was rebuilt and `tests/Pool/PublishErrorSurfacingTest.php` passed 4 tests / 14 assertions. This is focused evidence only, not a full validation. The stub must be regenerated because Rust public method documentation changed.

Two additional Laravel tests were just added but have **not yet been run**: reentrant logging during settlement-error draining and draining before closing an older consumer that rejects `drainErrors()` after close. The implementation has not yet been updated for either case, and these tests are expected to expose failures. Their presence is intentional TDD work in progress.

Next steps:

1. Run the two new Laravel regressions with `RabbitMqQueuePopTest.php` included because it defines shared `popWorkers()`/`popRoutes()` helpers. Confirm the intended failures.
2. Guard the entire `drainSettlementErrors()` against reentrancy with a flag and `try/finally`; remove each pending record before calling logger callbacks. Also review teardown handling for reentrant callbacks.
3. In `closeConsumers()`, best-effort drain each consumer before and after close: the pre-close drain retains compatibility with older native extensions and reduces risk of overflow, while the post-close drain observes failures generated during close.
4. Add a targeted PHPStan suppression with an explanation for the `method_exists(BackpressureException::class, 'throw')` check: older supported extensions lack the factory. Regenerate native stubs.
5. Review the native close implementation and error identity formatting, including batch-level errors and queue capacity; add focused coverage if needed. Destructor-time failures remain best-effort, and the bounded native settlement queue can evict records; do not claim complete losslessness.
6. Run targeted Laravel and extension tests, real-broker integration, then full Laravel and extension suites and `rtk ./scripts/check.sh` sequentially. Do not run extension builds and the Rust gate in parallel: both can replace the same debug extension artifact. Report the separate pre-existing advisory if it still blocks the gate.
7. Review the changes, update the relevant plan and evidence, and prepare logical commits/PRs only after regressions and validation are addressed. Do not include this handoff file.

All completed shell commands from the active session have returned. A process-list check was denied by the sandbox, so no claim is made about unrelated processes on the machine.
