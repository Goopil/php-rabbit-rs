# Real-Broker Reliability Evidence

## Run 2026-11-26: initial plugin and TTL binding experiments

- Tested revision: `778ec9d043b93110f1f6cbfee4ff3f8cfcbba51b` (`origin/main` as locally fetched); branch `test/broker-reliability`. The test files are uncommitted additions on this branch.
- Environment: isolated Compose project `rabbitrs-audit-7f3e`, three healthy RabbitMQ 4.2.9 nodes, delayed-message plugin 4.2.0, Toxiproxy. PHP dependencies were installed separately in the worktree; the local extension was rebuilt with `extension-tests`.
- Baseline `rtk ./scripts/check.sh`: failed at `cargo-deny` on `url 2.5.8`, `RUSTSEC-2026-0592`. A separate existing branch `fix/url-rustsec-2026-0592` contains the dependency fix; it was not modified here.
- Existing `rtk ./scripts/test-laravel.sh tests/Integration/DelayedJobTest.php`: 2 passed, 3 assertions.
- New `rtk ./scripts/test-laravel.sh tests/Integration/RealBrokerReliabilityTest.php`: 2 passed, 31 assertions. The first verified the returned message ID through a private proxy. The second established an AMQP handshake, applied upstream and downstream timeout toxics, observed the socket close, removed the toxics, and established another handshake. Proxy creation/removal and toxic state were checked via the API.

### Delayed routing when a binding disappears

Command: `rtk ./scripts/test-laravel.sh --with-extension tests/Experiments/DelayedRoutingAcceptanceTest.php`.

For plugin mode, a custom exchange's delayed binding was created explicitly. A confirmed control message arrived with its original ID. A second delayed publication was confirmed; the binding was deleted and its absence verified through management before its four-second delay elapsed. After seven seconds the binding was restored. No corresponding job arrived during the following ten-second observation window. The acceptance assertion failed at the expected delivery assertion.

For TTL mode the same experiment was repeated with a broker-declared quorum bucket. Management reported `x-message-ttl=5000`, `x-expires=65000`, the expected dead-letter target and no effective policy. The bound control arrived; the second publication was confirmed before its target binding was removed. No corresponding job arrived within ten seconds of restoring the binding. A follow-up isolated TTL-only run reproduced the expected delivery assertion failure (15 assertions); the bucket and destination resources were retained for inspection.

**Interpretation:** These runs demonstrate failure of the stated eventual-delivery acceptance window under the specified missing-binding fault. They do not by themselves establish an upper bound on broker latency or prove the exact instant at which a message was discarded. The initial runs did not print individual message IDs; the experiment was subsequently updated to print them for later runs. Repeat with separately verified at-least-once dead-letter policy and more detailed broker-side evidence before selecting or certifying a correction.

### TTL with an effective at-least-once dead-letter policy

Command: `rtk ./scripts/test-laravel.sh --with-extension tests/Experiments/DelayedRoutingAcceptanceTest.php --filter=ttl_at_least_once`.

The experiment created a policy matching only its unique bucket and verified through management that `dead-letter-strategy=at-least-once` and `overflow=reject-publish` were effective. The positive control arrived. A second publication was confirmed with message ID `c2e6638b-f2a4-4563-8fcb-c09c7433940b`; its routing binding was absent over the five-second message TTL and then restored. The job did not arrive during the next 90 seconds. Management polling observed that the bucket disappeared 63.784 seconds after restoration, while the PHP pool was still open. The bucket's recorded `x-expires` was 65 seconds. A previous run with the same effective policy also failed to deliver within 90 seconds.

This narrows the lifecycle risk: turning on the safer dead-letter policy alone did not meet the acceptance test, and the bucket itself disappeared during the observation window. The test does **not** establish whether the message remained in the bucket at the instant of disappearance or whether queue expiry caused its disappearance. Capture broker-side pending dead-letter state and a deletion event before making that causal claim. The test-owned destination and policy remain in the isolated lab for inspection; do not switch off the policy while it might still protect pending work.

An earlier plugin run without a manually created custom delayed binding failed its positive control. Inspection of `topology/plan.rs` shows the plan binds the shared `rabbit-rs.delayed` exchange but does not add bindings for `{custom-exchange}.delayed`. Treat this as a separate candidate finding: add a focused regression with explicit management observations before classifying its scope.

### Cleanup limitation

RabbitMQ 4.2.9 rejected both `if-unused=true` and `if-empty=true` queue-delete options for quorum queues with HTTP 400. The original TTL test's cleanup assertion therefore masked its main delivery failure. The experiment now retains TTL resources instead of forcing deletion. Do not run the lab's `down -v` scripts against a shared Compose project. The owned isolated lab remains available for inspection; cleanup requires verifying ownership and an explicit safe lifecycle for the retained test messages.

## Open investigations

- Inspect the broker-side pending dead-letter state and queue deletion event under the test-scoped at-least-once policy; test a missing DLQ destination separately.
- Investigate `x-expires` after all publishers stop and after bucket configuration rotation; an empty queue disappearing does not prove loss.
- Test classic-queue retries, invalid Laravel payloads, multiple asynchronous errors, and bulk partial success with real broker evidence.
- Exercise plugin and auto-mode resolution without the plugin; use a separate owned Compose project or a serial profile transition after checking lab ownership.
- Capture baseline and post-change performance measurements. No performance gain has been measured yet.
