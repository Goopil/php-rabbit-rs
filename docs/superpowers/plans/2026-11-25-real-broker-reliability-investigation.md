# Real-Broker Reliability Investigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task by task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish reproducible, evidence-backed behavior for the November 25 audit findings using the real RabbitMQ lab, the PHP extension, Laravel, and isolated Toxiproxy fault injection.

**Architecture:** Build on the existing three-node lab and Pest integration helpers. Characterization experiments first record what the current revision actually does; separate acceptance tests express the desired contract and are allowed to fail until the companion remediation plan fixes the corresponding issue. Preserve enough evidence to distinguish a proven loss, an observed explicit failure, a duplicate, an unexercised fault, and an inconclusive run.

**Tech Stack:** Rust 1.98.1, PHP/Pest, Laravel 12/13, RabbitMQ lab Docker Compose profiles, RabbitMQ Management API, Toxiproxy, `rtk`.

## Global Constraints

- All repository artifacts are in English. Preserve unrelated changes and do not reset, clean, pull, or change branches without a deliberate workspace decision.
- The audited local `main` was 15 commits behind `origin/main` on 2026-11-25. Record the actual revision and reconcile relevant upstream changes before making findings or fixes; do not assume the same findings still exist there.
- `scripts/lab-up.sh` runs Compose `down --remove-orphans -v`; `scripts/lab-down.sh` also removes volumes. Before using either, establish that the `rabbitrs` lab is disposable and not in use. If it contains data to preserve or another run owns it, stop and obtain an isolated lab instead. Do not use production brokers or a foreign Toxiproxy.
- Existing proxy API: `http://localhost:18474`; fingerprint proxy `rabbitmq-1`, upstream `rabbitmq-1:5672`; per-test listen ports 24504–24509. Validate the fingerprint before mutation, create a unique proxy, and remove only resources created by the test in `finally`/teardown. Fail if a required toxic or its removal is not confirmed.
- The `with-plugin` and `without-plugin` profiles expose the same host ports; run them serially and record the actual broker/plugin versions and image digests. The first uses the plugin image; the second has no plugin and uses its own cluster. Do not assume versions match.
- Toxiproxy here only affects connections deliberately routed through it. It does **not** partition RabbitMQ node-to-node traffic or stop RabbitMQ's internal dead-letter worker. Use a separately owned, uniquely named temporary destination/topology and controlled lab node operations for broker-side failures; prove the intended fault occurred.
- Protect credentials and private keys in logs and artifacts. Bound queues, messages, retry budgets, and poll deadlines. Use deadline-bounded polling on real broker events, never timing alone as proof. Unit tests use paused Tokio time and no real sleeps.
- Keep the connection-recovery order and stable identity rules in `docs/plans/2026-07-30-rabbitmq-native-design.md` and `AGENTS.md`. Avoid treating in-process replay as durability across crashes.

## Outputs and result convention

- Create `docs/audit/2026-11-25-real-broker-evidence.md`: revision, commands, versions, per-scenario observed events and IDs, diagnostics, links to sanitized artifacts, status, and next decision. Do not write results before running experiments.
- Create `packages/laravel-queue/tests/Integration/RealBrokerReliabilityTest.php` for the focused scenarios; reuse or extract the helpers in `AtLeastOnceChaosTest.php` and `tests/Pest.php` without redeclaring global PHP functions. If needed create `packages/laravel-queue/tests/Support/RealBrokerHarness.php` for a namespaced fixture, ownership tracking and bounded polling.
- Each scenario reports **confirmed**, **refuted**, **inconclusive**, or **blocked**, not a green check based solely on elapsed time. A timeout without proof of the injected failure and health of the observation path is inconclusive. Capture sent ID, application correlation ID, publish outcome, confirmations/returns, queue states, consumed IDs, duplicate count, settlement events, and recovery generation where applicable.
- A characterization test may document the observed unsafe behavior without asserting that it is correct. Acceptance tests live separately or behind an explicitly documented regression group: their required no-loss assertion must not be weakened, silently skipped, or made green by retrying a different message.
- For every scenario that is inconclusive or blocked, record the missing evidence, owner action and rerun command; its matching remediation item stays open. A negative result is not proof of safety without the planned positive control and verified fault.

## Task 1: Baseline and isolated fixture

**Files:** Create `tests/Support/RealBrokerHarness.php` and `tests/Integration/RealBrokerReliabilityTest.php` under `packages/laravel-queue/`; create the evidence report above. Inspect `lab/rabbitmq/compose.yaml`, `scripts/lab-up.sh`, `scripts/lab-ready.sh`, `scripts/test-integration.sh`, and existing chaos helpers.

**Interfaces:** The fixture must provide a unique run ID; an owned private proxy and verified toxic lifecycle; uniquely prefixed queues, exchanges, bindings and policies; management operations that verify HTTP status; monotonic deadline-bounded polling; and cleanup of only the resources it created. Do not globally reset Toxiproxy or purge shared queues.

- [ ] Record `rtk git status --short --branch`, `rtk git rev-parse HEAD`, tool versions, available Docker profiles, broker/plugin versions and existing lab ownership. Preserve the untracked PHPUnit cache and any other unrelated file if present.
- [ ] Inspect the lab and confirm it is disposable before running `rtk ./scripts/lab-up.sh with-plugin` and `rtk ./scripts/lab-ready.sh`. If readiness is not immediate, poll the existing ready script to a 120-second deadline and fail on expiry. Do not run `scripts/test-integration.sh` against a lab that must be preserved: it starts and destroys the lab and runs `composer update`.
- [ ] Run `rtk cargo build --manifest-path crates/rabbit-rs-php/Cargo.toml --features extension-tests` to rebuild the extension for the tested revision, then `rtk ./scripts/test-laravel.sh tests/Integration/DelayedJobTest.php` and `rtk ./scripts/test-laravel.sh tests/Integration/AtLeastOnceChaosTest.php`. Record baseline failures and skips separately. These commands load the local extension; prove the reported extension version and tested artifact correspond to the build.
- [ ] Implement and exercise the fixture: successful positive-control publish and consume, proxy fingerprint failure, toxic applied and removed, queue cleanup after an injected exception. A foreign endpoint or failed injection must fail loudly. Run `rtk ./scripts/test-laravel.sh tests/Integration/RealBrokerReliabilityTest.php` and record the outcome.

## Task 2: TTL dead-letter transfer under destination failure

**Files:** `RealBrokerReliabilityTest.php`, fixture, evidence report. Inspect `crates/rabbit-rs-core/src/topology/delay.rs` and `packages/laravel-queue/docs/reference.md`.

**Interfaces:** Test both default broker policy and a test-owned policy with `dead-letter-strategy=at-least-once` plus `overflow=reject-publish`, where the running RabbitMQ version supports it. TTL bucket names are hashed and start with `rabbit-rs.delay.`, not the test queue prefix: derive the exact bucket name from the test's unique destination and configuration, then anchor and escape that name in the policy pattern. Observe effective arguments/policy through management. Do not change global policies.

- [ ] Positive control: publish one uniquely identified TTL-delayed message with a live bound destination, confirm the initial publication and consume the same ID after its bucket expiry. Record the bucket queue name and effective queue/policy settings.
- [ ] Unavailable-destination case: confirm a second publication into its bucket, then remove its test-owned destination binding before TTL expiry. Verify the absence through management and wait until the source reaches its dead-letter processing point. Restore the binding, observe for a bounded recovery period and record source and destination state plus all consumed IDs. Assert no duplicate is mistaken for the original by checking ID and correlation payload.
- [ ] Repeat with the test-owned at-least-once policy, proving the policy is effective **before** publishing and the fault persists **after** expiry. The desired acceptance is eventual delivery after restoration with the original ID, allowing duplicates. Compare default behavior without assuming that failure must reproduce on every timing run.
- [ ] Repeat for a poison-message DLQ transfer with a test-owned source and missing destination. Toxiproxy alone is not evidence of broker-to-broker transfer failure. If the policy cannot be applied or state cannot be observed, mark the experiment inconclusive and retain remediation as open.

## Task 3: Plugin routing, bindings and outage boundaries

**Files:** `RealBrokerReliabilityTest.php`, fixture, evidence report. Inspect `crates/rabbit-rs-core/src/publisher/delay.rs` and `docs/reference.md`.

- [ ] In `with-plugin`, verify plugin presence and run a positive control: publish with a positive delay, await confirm, consume the original ID after expiry. Record routing key, exchange, binding and publisher outcome.
- [ ] Confirm another delayed publication, remove only its owned binding before routing, prove the binding is absent, wait past expiry and observe both target and source/management evidence. Restore it and record whether the job arrives and whether the publisher ever received a return or failure. Keep the application connection live so absence of an error is meaningful.
- [ ] Use a private proxy to inject `reset_peer` and a paired upstream/downstream `timeout` separately around publish/confirm; assert via the API that each toxic is present and that the connection/generation or operation actually changes. Remove toxics and verify recovery. Track each ID from publication through consumption; duplicate count is allowed but silent loss of an accepted confirmed publication is not.
- [ ] Exercise `auto` in `without-plugin` after serial profile switch, verifying the management probe outcome, effective mode and a real delayed delivery. Record what happens when management is unavailable separately from plugin absence. Mark broker-side delivery guarantees not established by a client-only proxy as unresolved.

## Task 4: Expiry, restart and configuration rotation

**Files:** `RealBrokerReliabilityTest.php`, fixture, evidence report. Inspect `DelayKeepAlive` in `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs`.

- [ ] Create short, supported TTL buckets and expiry margins in a uniquely owned topology. Record the effective `x-message-ttl`, `x-expires`, queue arguments and each keep-alive observation. Establish a control where the producer remains alive and the job is delivered.
- [ ] After a confirmed delayed publish, stop all test-owned clients and observe the queue and message through an independent management connection until delivery or the queue-expiry deadline plus a bounded margin. Repeat with a new client using a changed bucket/margin fingerprint. Verify that the old queue is actually unmaintained and distinguish message TTL processing, dead-letter transfer and queue deletion.
- [ ] If normal TTL processing drains the message before expiry, report that fact: it does not prove deletion with an in-flight message. To test that condition, use a controlled lab-only broker state that demonstrably leaves a message in the old bucket past `x-expires`; if this state cannot be produced, report **inconclusive**, not a fabricated loss.
- [ ] Independently test a client-to-broker outage crossing the keep-alive margin with paired Toxiproxy timeouts and a broker restart/leader event only where the lab ownership is established. Verify the injected fault and queue existence, then restore and account for each confirmed ID.

## Task 5: Attempts, invalid payloads and asynchronous failures

**Files:** `RealBrokerReliabilityTest.php`, `packages/laravel-queue/tests/Unit/RabbitMqQueueDrainErrorsTest.php`, fixture, evidence report.

- [ ] With a genuine classic queue, run a job that deterministically fails and is released with delay zero at least four times; use a bounded worker and record the delivery flag, headers and Laravel `attempts()` each time. Repeat with quorum and with delayed release. Set a finite limit and assert that the test cannot itself loop indefinitely. Report separately the attempt counter, Laravel failed-job behavior and native poison limit.
- [ ] Publish raw JSON `null`, `[]`, `{}` and a valid Laravel payload with unique IDs; use a worker configured with finite tries and capture whether each job reaches the poison path, is retried or is failed. Check that the valid control executes exactly once in the healthy case.
- [ ] Create multiple independent unroutable safe-mode publishes and wait for their asynchronous error records. Through the Laravel queue path, drain them and record surfaced/logged message IDs and the native dropped-error counter. Repeat with mixed settlement records using a deterministic fake in the existing unit file; record which records disappear after a connection error. The acceptance oracle accounts for **every** terminal error, including bounded-overflow telemetry.
- [ ] Reproduce bulk partial success with more than 256 jobs and deterministic failure of a later chunk. Persist the first chunk's IDs and retry the same logical jobs; compare payload UUID and AMQP `message_id` on each attempt. Assert partial-success accounting and report stable and unstable identities separately.

## Task 6: Cost characterization and investigation exit

**Files:** evidence report; existing `benchmarks/` and `crates/*/benches/` when an existing harness fits; `.github/workflows/nightly.yml` only if the focused suite is demonstrably stable and safe in CI.

- [ ] Measure PHP `bulk()` at 256, 2,560 and 25,600 small jobs with a fake publisher to isolate batching overhead, and with a broker at a practical bounded size to measure end-to-end time. Record wall time, peak memory, chunk count, runtime and hardware; compare results without asserting an unmeasured speedup. Also measure keep-alive channel operations as destinations and buckets grow; report workload and broker impact.
- [ ] Run the focused suite with each required profile, at least three repetitions for timing-sensitive failure scenarios, and the existing integration suite once. Restore every owned toxic, queue, binding, policy and stopped lab node; verify the final inventory. Run profile switches only after confirming the current lab may be destroyed.
- [ ] Fill the evidence report with a row for each audit finding: evidence, status, exact reproducible command, test name, limitations, severity, and action in `2026-11-25-audit-remediation.md`. Link raw sanitized artifacts and separate client fault injection from broker fault injection.
- [ ] **Exit gate:** every scenario is confirmed or refuted with a non-vacuous positive control, or explicitly marked inconclusive/blocked with an owner and next experiment. Do not claim the delivery contract is certified while a loss scenario is unresolved. The companion plan can start independent fixes immediately, but cannot close investigation-dependent tasks until their evidence and regression tests exist.
