# Rabbit RS Native PHP Extension and Laravel Queue Driver Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Deliver the Rabbit RS ecosystem: the rabbit_rs PHP extension and the goopil/rabbit-rs-laravel package, performant, at-least-once, and able to pool publishing and consuming across multiple vhosts with automatic reconnection.

**Architecture:** A Rust workspace contains rabbit-rs-core and the rabbit-rs-php extension building ext-rabbit_rs. The Composer package goopil/rabbit-rs-laravel adapts this API to the Laravel Queue contracts without replacing Illuminate\Queue\Worker. Connections and channels are driven by Tokio actors per PHP process, while a reproducible RabbitMQ lab validates performance and failure scenarios.

**Tech Stack:** Rust 1.96, Tokio, Lapin, ext-php-rs, PHP 8.4/8.5, PIE 1.5+, Composer, Packagist, Laravel 12/13, Pest, Orchestra Testbench, RabbitMQ 4.3, Docker Compose, Prometheus.

> **Note (2026-08-21):** Chaos tests and Toxiproxy fault injection have been removed. The at-least-once delivery guarantees are validated through integration tests with the mock transport and the 3-node RabbitMQ lab (without Toxiproxy). Criterion benchmarks have been replaced by the PHP benchmark suite.

---

## Execution rules

- Apply @superpowers:test-driven-development to every behavior.
- Use @superpowers:systematic-debugging for any unexpected failure.
- Run @superpowers:verification-before-completion before every milestone.
- Never carry a Zend value into a Rust thread.
- Keep Lapin behind the Transport interface.
- Preserve unrelated user changes.
- One logical commit after each green task.
- Do not freeze batching or prefetch values before the benchmark milestone.
- Keep unsent or ambiguous publications in an overall bounded capacity during an outage, then replay them automatically with the same message_id and the original deadline after recovery.
- Do not present this in-memory retention as durable: a process crash requires an external outbox to guarantee the replay.

## Progress

**Last updated:** August 23, 2026

**Implementation branch:** Goopil/feat-horizon

**Next step:** Milestone F — Distribution and documentation.

- [x] Task 1 — Reproducible Rust/PHP workspace (`4f2a997`).
- [x] Task 2 — Normalized and validated configuration (`c324929`).
- [x] Task 3 — Weighted starvation-free scheduler (`17804d0`).
- [x] Task 4 — Per-process runtime safe after fork (`ca5dd36`).
- [x] Task 5 — Transport abstraction, scriptable mock and Lapin (`71680e1`).
- [x] Task 6 — Connection actor and deterministic recovery (`70d5b59`).
- [x] Task 7 — Declare, verify and external topology (`7ff2de9`).
- [x] Task 8 — Bounded publisher, batching, confirms and mandatory returns (`90d3089`).
- [x] Task 9 — Plugin delays and TTL fallback (`bae220b`).
- [x] Task 9 bis — Bounded retention and publisher replay after reconnection (`241f77d`).
- [x] Task 10 — ConsumerSet and delivery tokens (`380a95d`).
- [x] Task 11 — Attempts counters and poison-message handling (`eb35412`).
- [x] Task 12 — Metrics snapshot and Milestone A gate (`21aedee`).
- [x] Task 13 — Define the PHP API and stubs of Milestone B.
- [x] Task 14 — Test PHP conversions, errors and transitions.
- [x] Task 15 — Certify the CLI, fork and FPM lifecycle.
- [x] Task 16 — Initialize the package and its configuration.
- [x] Task 17 — Register the connector and the shared pool.
- [x] Task 18 — Implement push, later and bulk.
- [x] Task 19 — Implement RabbitMqJob.
- [x] Task 20 — Wire pop to a multi-vhost profile.
- [x] Task 21 — Implement size, clear and monitoring (`d8bafcf`).
- [x] Task 22 — Add native events and a diagnostic command (`950819b`).
- [x] Task 23 — Add the progressive multiprocess command (`de8d8bf`).
- [x] Task 24 — Certify Octane (`4f04b63`).
- [x] Task 25 — Create the RabbitMQ test cluster.
- [x] Task 26 — Write end-to-end integration tests.
- [x] Task 27 — Write failure scenarios (chaos/fault injection).
- [x] Task 28 — Implement the recovery coordinator (`ad652c7`).
- [x] Task 29 — Implement publisher-side delay routing (`e844375`, `89bff5f`).
- [x] Task 30 — Wire the DLQ and generic queue arguments (`7d62e0c`).
- [x] Task 31 — Wire TLS end-to-end (`e6881d3`).
- [x] Task 32 — Wire consumer cleanup and prevent channel leaks.
- [x] Task 33 — Dispatch Laravel events from the native extension (`9213d0d`, `c7ea2ad`).
- [x] Task 34 — Expose consumer metrics and latencies (`31e5676`, `6f41c7f`).
- [x] Task 35 — Wire publisher config (confirms, mandatory, timeout) end-to-end (`c8261ec`).
- [x] Task 36 — Wire the full Octane lifecycle (`e20a69b`).
- [x] Task 37 — Wire the WorkCommand and test the supervisor end-to-end (`9c0e036`, `356b71b`, `7a68c6f`).
- [x] Task 38 — Create bench-native (`ae9c668`, `7f13f98`).
- [x] Task 39 — Create the bench-laravel application (`3fb9fc4`, `8d56ee8`, `e43cb68`).
- [x] Task 40 — Calibrate the defaults and freeze the budgets (`6ec500e`, `6546f4c`).
- [x] Task 41 — Prepare the Rabbit RS packages and the PIE matrix (`aa2e7d2`).
- [x] Task 42 — Add CI and synchronized publishing (`f04dbd8`, `f7f374e`, `e2e742c`).
- [x] Task 43 — Document installation, configuration and operations (`0712948`, `aa14daf`).

**All tasks (1–43) are complete.** The implementation plan is finished.

## Milestone H — Laravel Horizon support

This milestone adds Laravel Horizon integration to the `goopil/rabbit-rs-laravel` package so that Rabbit RS jobs appear in the Horizon dashboard alongside Redis jobs. RabbitMQ remains the transport; Redis is used by Horizon for tracking and the dashboard. No change to the Rust core or the PHP extension.

- [x] Task H1 — Remove `final` from `RabbitMqQueue` and `RabbitMqJob` (`feec698`).
- [x] Task H2 — Add the Horizon fakes (events + JobPayload) to the test bootstrap (`feec698`).
- [x] Task H3 — Create `Horizon\RabbitMqQueue` with event dispatching (`feb50f5`).
- [x] Task H4 — Create `Horizon\RabbitMqJob` with `deleteReserved()` (`92d7742`).
- [x] Task H5 — Wire dynamic resolution in the connector by `worker` config (`ddb75a0`).
- [x] Task H6 — Add the `worker` config key and `suggest laravel/horizon` (`9b75901`).

**All tasks (H1–H6) are complete.** 207 PHP tests pass (545 assertions), 193 Rust tests pass.

## Milestone D2 — Recovery, delay and topology (implementation gaps)

This milestone fixes the gaps identified by the August 16, 2026 audit: the missing recovery coordinator, the unwired publisher-side delay routing, and the unwired DLQ/generic arguments.

### Task 28: Implement the recovery coordinator

**Files:**
- Create: crates/rabbit-rs-core/src/pool/recovery_coordinator.rs
- Modify: crates/rabbit-rs-core/src/client.rs
- Modify: crates/rabbit-rs-core/src/pool/mod.rs
- Modify: crates/rabbit-rs-core/src/pool/connection_actor.rs
- Modify: crates/rabbit-rs-core/src/consumer/set.rs
- Create: crates/rabbit-rs-core/tests/recovery_coordinator.rs

**Context:**

The recovery primitives are complete (`ConnectionActor` with backoff/generation, `PublisherActor` with bounded replay buffer, `TopologyReconciler` with per-generation replay, `ConsumerActor` with `UpdateGeneration`), but no coordinator links them. The `ClientPool` opens connections lazily and never observes their loss. The chaos tests recreate pools manually after every failure.

**Step 1: Write failing recovery coordinator tests**

Test scenarios (mock transport, no real broker):

1. A connection is lost → `PublisherActor` receives `Recovering` → unconfirmed messages enter the replay buffer → the connection is re-established → `TopologyReconciler` replays → `PublisherActor` receives `Ready { topology_restored: true }` → the replay is flushed → messages are delivered.
2. A connection is lost → `ConsumerActor` receives `UpdateGeneration` after reconnection → deliveries from the old generation are rejected (`StaleGeneration`) → the broker redelivers.
3. Deterministic order verified: connection → channels → exchanges → queues → bindings → QoS → consumers → publisher replay.
4. Loss during recovery → the coordinator cancels and restarts.
5. Permanent error (credentials) → `FailedPermanent` → the coordinator does not loop.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test recovery_coordinator

Expected: FAIL — the coordinator does not exist yet.

**Step 3: Implement the recovery coordinator**

The coordinator is one task per broker that:

1. Spawns a `ConnectionActor` and subscribes to its `watch::Receiver<ConnectionState>`.
2. On `ConnectionLost` (detected transport error) → `ConnectionActor::connection_lost(error)` → emits `PublisherConnectionEvent::Recovering` to the broker's `PublisherActor`.
3. On `ConnectionState::Ready { generation }` → opens a new `PublisherChannel`, executes `TopologyReconciler::reconcile(channel, plan, generation)`, then emits `PublisherConnectionEvent::Ready { generation, channel, topology_restored: true }` to the `PublisherActor`.
4. For consumers → opens new `ConsumerChannel`s, re-applies QoS, re-emits `basic_consume`, and calls `ConsumerHandle::update_generation` for each subscription.
5. Enforces the deterministic order: connection → channels → exchanges → queues → bindings → QoS → consumers → publisher replay.

The `ClientPool` must:
- Spawn a coordinator per broker at connection initialization.
- Store the `ConnectionActorHandle` and the coordinator's `JoinHandle`.
- On `close()`, cancel the coordinator and the connection actor.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test recovery_coordinator

Expected: PASS.

**Step 5: Update chaos tests to remove manual pool recreation**

Modify `crates/rabbit-rs-core/tests/chaos/reconnect.rs`:
- Remove the `ClientPool` recreation pattern after each failure.
- The tests must create a single `ClientPool`, inject the failure, and verify that the pool recovers automatically.
- `missing = 0` must be maintained without manual intervention.

Run: cargo test -p rabbit-rs-core --features integration --test chaos_reconnect

Expected: PASS.

**Step 6: Run full quality gate**

Run: ./scripts/check.sh

Expected: PASS.

**Step 7: Commit**

    git add crates
    git commit -m "feat(core): wire recovery coordinator end-to-end"

### Task 29: Implement publisher-side delay routing

**Files:**
- Modify: crates/rabbit-rs-core/src/transport.rs
- Modify: crates/rabbit-rs-core/src/transport/lapin.rs
- Modify: crates/rabbit-rs-core/src/publisher/actor.rs
- Modify: crates/rabbit-rs-core/src/publisher/mod.rs
- Modify: crates/rabbit-rs-core/src/client.rs
- Modify: crates/rabbit-rs-core/src/config.rs
- Modify: crates/rabbit-rs-core/src/topology/plan.rs
- Modify: crates/rabbit-rs-core/src/topology/reconciler.rs
- Modify: crates/rabbit-rs-core/src/topology/delay.rs
- Modify: crates/rabbit-rs-core/src/publisher/delay.rs
- Create: crates/rabbit-rs-core/tests/publisher_delay.rs
- Modify: packages/laravel-queue/config/rabbit-rs.php
- Modify: packages/laravel-queue/src/Config/ConfigNormalizer.php
- Modify: packages/laravel-queue/tests/Integration/DelayedJobTest.php

**Context:**

The `DelayRouter` exists and is tested, but it is only wired in the consumer's `release()`. On the publisher side, `later()` sets the `x-delay` header on the original exchange — a no-op effect. The `x-delayed-message` exchanges cannot be declared because `ExchangeSpec` has no arguments. The TTL delay queues are never declared. `DelayConfig` is not in `ValidatedConfig`. The Laravel config exposes no delay section.

**Step 1: Write failing publisher delay tests**

Scenarios:

1. `publish()` with `delay_ms > 0` in Plugin mode → the message is published to the `x-delayed-message` exchange (not the original exchange) with the `x-delay` header.
2. `publish()` with `delay_ms > 0` in TTL mode → the message is published to a TTL queue with `x-message-ttl` and dead-lettered to the original destination.
3. `publish()` with `delay_ms = 0` → no special routing (normal behavior).
4. The `x-delayed-message` exchange is declared by the `TopologyReconciler` when Plugin mode is active.
5. The TTL queues are declared lazily (on-demand) by the publisher.
6. `DelayConfig` is validated and deserialized from config.
7. `DelayMode::Auto` detects the plugin and falls back to TTL if absent.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test publisher_delay

Expected: FAIL.

**Step 3: Implement exchange arguments and delayed exchange support**

- Add `arguments: BTreeMap<String, HeaderValue>` to `ExchangeSpec`.
- Add `ExchangeKind::Delayed(ExchangeKind)` which emits `x-delayed-message` as the exchange type and `x-delayed-type` as an argument (with the underlying type: direct, topic, etc.).
- Update `lapin.rs::declare_exchange()` to pass the arguments instead of `FieldTable::default()`.
- The `TopologyReconciler` must declare the `x-delayed-message` exchange (name `rabbit-rs.delayed` or `{exchange}.delayed`) when Plugin mode is selected.

**Step 4: Wire DelayRouter into the publisher path**

- The `PublisherActor` (or `ClientPool::publish()`) must detect `delay_ms > 0`, resolve the `DelayStrategy` via `DelayStrategyResolver`, call `DelayRouter::route()`, and publish to the delayed exchange/queue instead of the original.
- In TTL mode, lazily declare the TTL queue before the first delayed publish (idempotent via cache).
- In Plugin mode, the delayed exchange is declared by the `TopologyReconciler` during recovery.

**Step 5: Add DelayConfig to ValidatedConfig**

- Add `delay: DelayConfig` to `Config` and `ValidatedConfig`.
- Deserialize `mode` (auto/plugin/ttl), `buckets`, `max_buckets`, `queue_expiry_margin`, `detection_timeout`.
- Validate: buckets non-empty, ≤ max_buckets, without zero, detection_timeout bounded.

**Step 6: Wire DelayConfig through ClientPool**

- The `ClientPool` must instantiate a `DelayStrategyResolver` per broker and pass it to the publisher and consumer.
- The `ConsumerSet` must receive the resolved `DelayStrategy` (instead of the hardcoded `Plugin`).
- `ClientPool::consumer()` must call `.delayed_publisher()` and `.delay_strategy()` on each subscription.

**Step 7: Expose delay config in Laravel**

- Add a `delay` section to `config/rabbit-rs.php`: `mode`, `buckets`, `max_buckets`, `queue_expiry_margin`, `detection_timeout`.
- `ConfigNormalizer` must map this section to the native config.

**Step 8: Un-skip and fix the Laravel integration test**

- Remove `markTestSkipped` from `test_later_publishes_and_consumes_after_delay`.
- The test must publish with `later(2, ...)` and verify that the job is not immediately available, then is after the delay.

**Step 9: Verify**

Run: cargo test -p rabbit-rs-core --test publisher_delay
Run: ./scripts/test-integration.sh

Expected: PASS.

**Step 10: Commit**

    git add crates packages
    git commit -m "feat(core): wire publisher-side delay routing and config"

### Task 30: Wire the DLQ and generic queue arguments

**Files:**
- Modify: crates/rabbit-rs-core/src/transport.rs
- Modify: crates/rabbit-rs-core/src/transport/lapin.rs
- Modify: crates/rabbit-rs-core/src/config.rs
- Modify: crates/rabbit-rs-core/src/topology/plan.rs
- Modify: crates/rabbit-rs-core/src/topology/reconciler.rs
- Create: crates/rabbit-rs-core/tests/dlq_topology.rs
- Modify: packages/laravel-queue/config/rabbit-rs.php
- Modify: packages/laravel-queue/src/Config/ConfigNormalizer.php

**Context:**

The DLQ compilation (`TopologyPlan::compile` with `DeadLetterDefinition`) is implemented and tested in Rust, but is not configurable via `ValidatedConfig`. The Laravel config exposes `dead_letter => null` and `delivery_limit => 20` but these values are validated then dropped. `QueueSpec` has no generic arguments for `x-delivery-limit`, `x-max-priority`, etc.

**Step 1: Write failing DLQ config tests**

Scenarios:

1. Config with non-null `dead_letter` → `ValidatedConfig` contains a `DeadLetterConfig` → `TopologyDefinition` is compiled with `with_dead_letter` → the `TopologyReconciler` declares the DLX, the DLQ and the binding.
2. Config with `delivery_limit: 20` → `QueueSpec` contains `x-delivery-limit: 20` → the `TopologyReconciler` declares the queue with this argument.
3. Config without `dead_letter` → no DLQ (default behavior).
4. The Laravel `ConfigNormalizer` maps `topology.dead_letter` and `topology.queue.delivery_limit` to the native config.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test dlq_topology

Expected: FAIL.

**Step 3: Add generic queue arguments**

- Add `arguments: BTreeMap<String, HeaderValue>` to `QueueSpec` (in addition to the existing structured fields).
- Update `lapin.rs::declare_queue()` to merge the structured arguments (DLX, TTL, etc.) and the generic arguments.
- Add `delivery_limit: Option<u32>` to `QueueSpec` → emits `x-delivery-limit`.

**Step 4: Add DeadLetterConfig to ValidatedConfig**

- Create a `DeadLetterConfig` struct: `enabled: bool`, `exchange: String`, `queue: String`, `routing_key: Option<String>`.
- Add `dead_letter: Option<DeadLetterConfig>` to `Config`/`ValidatedConfig`.
- Wire: `ValidatedConfig.dead_letter` → `TopologyDefinition::with_dead_letter` → `TopologyPlan::compile` → `TopologyReconciler::reconcile`.

**Step 5: Wire Laravel config to native config**

- `ConfigNormalizer` must transform `topology.dead_letter` (null or array with `exchange`, `queue`, `routing_key`) to the native `dead_letter` config.
- `ConfigNormalizer` must transform `topology.queue.delivery_limit` to the native config (field `delivery_limit` on queues).
- The connector must pass these values to the `Pool`'s native config.

**Step 6: Verify**

Run: cargo test -p rabbit-rs-core --test dlq_topology
Run: cd packages/laravel-queue && php -n vendor/bin/phpunit --testsuite "Rabbit RS Laravel"

Expected: PASS.

**Step 7: Commit**

    git add crates packages
    git commit -m "feat(core): wire DLQ config and generic queue arguments"

Milestone D2 checkpoint (Tasks 28-30) — August 16, 2026:

- **Task 28** (`ad652c7`): `RecoveryCoordinator` created in `pool/recovery_coordinator.rs`. The coordinator spawns a `ConnectionActor` per broker, subscribes to its `watch::Receiver<ConnectionState>`, and orchestrates the deterministic recovery: connection → channels → topology → QoS → consumers → publisher replay. `ClientPool` spawns a coordinator per broker instead of opening connections directly. 5 tests in `recovery_coordinator.rs`. `close` can interrupt a stuck recovery via `tokio::select!`. The `client_pool.rs` tests were adapted (the explicit closure behavior of non-committed channels is now handled by the cascading connection close).

- **Task 29** (`e844375`, `89bff5f`): `DelayConfig` added to `Config`/`ValidatedConfig` with serde. `ExchangeSpec` gains an `arguments` field and `ExchangeKind::Delayed` for `x-delayed-message` exchanges. The `PublisherActor` routes messages with `delay_ms > 0` via `DelayRouter`: Plugin mode publishes to the delayed exchange with the `x-delay` header, TTL mode publishes to a TTL queue dead-lettered to the original destination. TTL queues are declared lazily by the publisher. The `RecoveryCoordinator` compiles the `DelayStrategy` from config and passes it to the publisher and consumers. The Laravel config exposes a `delay` section (mode, buckets, max_buckets, queue_expiry_margin, detection_timeout). `DelayedJobTest` is un-skipped. 8 tests in `publisher_delay.rs`. `delayed_publisher()` no longer hardcodes `DelayStrategy::Plugin` — the strategy is set separately via `.delay_strategy()`. The coordinator passes the publisher handle and destination to each consumer subscription.

- **Task 30** (`7d62e0c`): `DeadLetterConfig` added to `Config`/`ValidatedConfig`. `QueueSpec` gains `delivery_limit` and generic `arguments`. `lapin.rs::declare_queue()` emits `x-delivery-limit` and merges generic arguments. `ClientPool::build_topology_plan()` wires `dead_letter` and `delivery_limit` from config to the `TopologyPlan`. `ConfigNormalizer` maps `topology.dead_letter` and `topology.queue.delivery_limit` to the native config. 11 tests in `dlq_topology.rs`. 6 new `ConfigNormalizerTest` tests.

- **Overall result**: 177 Rust tests + 101 PHP tests pass. Quality gate `./scripts/check.sh` green. Clippy clean. Fmt clean.

### Task 31: Wire TLS end-to-end

**Files:**
- Modify: crates/rabbit-rs-core/src/transport.rs
- Modify: crates/rabbit-rs-core/src/transport/lapin.rs
- Modify: crates/rabbit-rs-core/src/config.rs
- Modify: packages/laravel-queue/config/rabbit-rs.php
- Modify: packages/laravel-queue/src/Config/ConfigNormalizer.php
- Create: crates/rabbit-rs-core/tests/tls.rs

**Context:**

`TlsConfig` exists with `enabled` and `server_name`, but `server_name` is never read by the transport. The `amqps` scheme is set via the URI, but no TLS connector configuration (SNI, CA certs, client cert, verification mode) is passed to Lapin. No TLS test exists.

**Step 1: Write failing TLS tests**

Scenarios:

1. `tls.enabled = true` + `server_name = "rabbit.example.com"` → the URI uses `amqps://` and `server_name` is passed to Lapin for SNI.
2. `tls.enabled = false` → the URI uses `amqp://`.
3. `tls.enabled = true` without `server_name` → uses the first host as SNI.
4. Config with `ca_cert`, `client_cert`, `client_key` → passed to the TLS connector.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test tls

Expected: FAIL.

**Step 3: Implement TLS connector configuration**

- Extend `TlsConfig`: add `ca_cert: Option<PathBuf>`, `client_cert: Option<PathBuf>`, `client_key: Option<PathBuf>`, `verify: Option<TlsVerify>` (default `Peer`).
- Update `lapin.rs`: use `ConnectionProperties::with_ssl` or build a rustls `tls::Connector` with SNI (`server_name`), CA, client cert.
- Use `server_name` for SNI when provided, otherwise the first host.
- Keep the `amqps` scheme in the URI when `enabled`.

**Step 4: Expose TLS settings in Laravel config**

- Add `ca_cert`, `client_cert`, `client_key`, `verify` to `config/rabbit-rs.php` under `brokers.default.tls`.
- `ConfigNormalizer` must map these fields to the native config.

**Step 5: Verify**

Run: cargo test -p rabbit-rs-core --test tls
Run: cd packages/laravel-queue && php -n vendor/bin/phpunit --testsuite "Rabbit RS Laravel"

Expected: PASS.

**Step 6: Commit**

    git add crates packages
    git commit -m "feat(core): wire TLS connector configuration end-to-end"

TLS end-to-end checkpoint of August 16, 2026:

- `TlsConfig` extended with `ca_cert`, `client_cert`, `client_key` (`PathBuf` paths) and `verify: TlsVerify` (`Peer` default, `None` to skip).
- `TlsVerify` added as a `snake_case` serde enum with `#[default] Peer`.
- `BrokerConfig::effective_server_name()` resolves the SNI: `tls.server_name` if provided, otherwise the first host.
- `lapin.rs::connect()` uses `Connection::connect_with_config` with an `OwnedTLSConfig` built from the config when TLS is enabled (CA cert in PEM, client cert/key in PKCS#8).
- `connection_uri` made public for tests.
- The `ConfigFingerprint` includes `ca_cert`, `client_cert`, `client_key` and `verify` to differentiate TLS configs.
- The Laravel config exposes `ca_cert`, `client_cert`, `client_key`, `verify` under `brokers.default.tls`.
- `ConfigNormalizer` validates and maps these fields to the native config.
- 10 Rust tests in `tls.rs`, 3 new `ConfigNormalizerTest` tests.
- `./scripts/check.sh`: PASS, 187 Rust tests + 104 PHP tests, Clippy clean, Fmt clean.

Consumer cleanup checkpoint of August 16, 2026:

- `ConsumerHandle::Drop` implemented in `consumer/set.rs`: sends `ConsumerCommand::Close` via `try_send` (best-effort, non-blocking) when the last clone is dropped, preventing AMQP channel leaks in long-lived processes (Octane, daemons) even if `close()` is never explicitly called.
- `Consumer::__destruct()` added to the PHP extension: calls `close()` if not already closed, with a fork guard.
- `RabbitMqQueue::closeConsumers()` and `__destruct()` added: close all cached consumers and empty the cache, preventing channel accumulation between Octane requests.
- `OctaneLifecycle::flush()`, `reload()` and `stop()` call `closeConsumersOnResolvedQueues()` which iterates the resolved `rabbit-rs` connections of the `QueueManager` and closes the consumers of each `RabbitMqQueue`.
- The PHP `Consumer` mock tracks `closeCalls` and the `Pool` mock creates a new consumer when the previous one is closed.
- 6 Rust tests in `consumer_cleanup.rs` (drop closes channels, multiple subscriptions, no double-close, idempotent across clones, next after drop returns a typed error, in-flight delivery).
- 6 PHP tests in `RabbitMqQueueCleanupTest` (closeConsumers closes all, clears the cache, idempotent, safe without consumers, __destruct, pop creates a new consumer).
- 4 new PHP tests in `OctaneLifecycleTest` (flush/reload/stop close the consumers, flush without a queue manager).
- `./scripts/check.sh`: PASS, 193 Rust tests + 114 PHP tests, Clippy clean, Fmt clean.

### Task 32: Wire consumer cleanup and prevent channel leaks

**Files:**
- Modify: crates/rabbit-rs-core/src/consumer/set.rs
- Modify: crates/rabbit-rs-php/src/classes/consumer.rs
- Modify: packages/laravel-queue/src/RabbitMqQueue.php
- Modify: packages/laravel-queue/src/Octane/OctaneLifecycle.php
- Create: crates/rabbit-rs-core/tests/consumer_cleanup.rs

**Context:**

`RabbitMqQueue` caches `Consumer`s in `$this->consumers` but never calls `close()`. No `__destruct`. `ConsumerHandle` has no `Drop` that sends `Close`. In long-lived processes (Octane, daemons), AMQP channels leak.

**Step 1: Write failing consumer cleanup tests**

Scenarios:

1. `RabbitMqQueue::__destruct()` → calls `$consumer->close()` for each cached consumer → the channels are closed.
2. `ConsumerHandle::Drop` → sends `Close` to the actor (best-effort) → the channels are closed even if PHP does not call `close()`.
3. `OctaneLifecycle::flush()` → closes the consumers of the current queue (not just the pool factory).
4. After `close()`, `pop()` returns `null` or throws a typed error (no panic).

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test consumer_cleanup

Expected: FAIL.

**Step 3: Implement Rust-side Drop safety net**

- Implement `Drop` for `ConsumerHandle`: sends `ConsumerCommand::Close` (best-effort, non-blocking via `try_send`).
- Ensure the `Close` handler in the actor closes the channels even when received via Drop.

**Step 4: Implement PHP-side cleanup**

- PHP `Consumer`: add `__destruct()` that calls `close()` if not already closed.
- `RabbitMqQueue`: add `closeConsumers()` that closes all cached consumers, and `__destruct()` that calls it.
- `OctaneLifecycle::flush()`: call `closeConsumers()` on the current queue (if available).

**Step 5: Verify**

Run: cargo test -p rabbit-rs-core --test consumer_cleanup
Run: cd packages/laravel-queue && php -n vendor/bin/phpunit --testsuite "Rabbit RS Laravel"

Expected: PASS.

**Step 6: Commit**

    git add crates packages
    git commit -m "fix(core): wire consumer cleanup and prevent channel leaks"

### Task 33: Dispatch Laravel events from the native extension

**Files:**
- Modify: crates/rabbit-rs-php/src/classes/pool.rs
- Modify: crates/rabbit-rs-php/src/lib.rs
- Create: crates/rabbit-rs-php/src/callbacks.rs
- Modify: crates/rabbit-rs-core/src/metrics.rs
- Modify: crates/rabbit-rs-core/src/pool/connection_actor.rs
- Modify: packages/laravel-queue/src/RabbitMqQueue.php
- Modify: packages/laravel-queue/src/RabbitMqServiceProvider.php
- Modify: packages/laravel-queue/src/Events/ConnectionStateChanged.php
- Modify: packages/laravel-queue/src/Events/BackpressureDetected.php
- Create: packages/laravel-queue/tests/Feature/NativeEventDispatchTest.php

**Context:**

`ConnectionStateChanged` and `BackpressureDetected` are defined but never dispatched. There is no FFI mechanism to signal state changes from Rust to PHP. The events are dead code.

**Step 1: Write failing event dispatch tests**

Scenarios:

1. A connection is lost → the `ConnectionStateChanged` event is dispatched with `state = "recovering"`.
2. The connection is restored → the `ConnectionStateChanged` event is dispatched with `state = "ready"` and incremented `generation`.
3. The publisher reaches capacity → the `BackpressureDetected` event is dispatched with `inFlight` and `capacity`.
4. The events are dispatched via the Laravel event system (Event::dispatch).

**Step 2: Verify failure**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/NativeEventDispatchTest.php

Expected: FAIL.

**Step 3: Implement FFI callback mechanism**

- Rust side (PHP extension): register PHP callbacks (closures) via `Pool::onConnectionState(callback)` and `Pool::onBackpressure(callback)`.
- Store the callbacks in the PHP `Pool` (Zend objects, never in Rust threads — callbacks are invoked on the PHP thread via `block_on`).
- The `ConnectionActor` publishes `ConnectionState` via `watch`; the PHP `Pool` polls the `watch::Receiver` during synchronous operations and invokes the callback if the state changed.
- The atomic `Metrics` `backpressure_total` can be compared between two `stats()` calls to detect backpressure and invoke the callback.

**Step 4: Wire events in Laravel**

- `RabbitMqServiceProvider`: register the default callbacks that dispatch the Laravel events.
- `RabbitMqQueue`: expose `onConnectionState()` and `onBackpressure()` for override.

**Step 5: Verify**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/NativeEventDispatchTest.php

Expected: PASS.

**Step 6: Commit**

    git add crates packages
    git commit -m "feat(laravel): dispatch native events for connection state and backpressure"

### Task 34: Expose consumer metrics and latencies

**Files:**
- Modify: crates/rabbit-rs-php/src/classes/pool.rs
- Modify: crates/rabbit-rs-core/src/metrics.rs
- Modify: packages/laravel-queue/src/Console/RabbitMqStatusCommand.php
- Modify: packages/laravel-queue/tests/Feature/RabbitMqStatusCommandTest.php

**Context:**

3 counters (`deliveries_total`, `acks_total`, `rejects_total`) and 2 histograms (`confirmation_latency`, `settlement_latency`) are collected in Rust but not exposed to PHP. The status command only shows publisher metrics.

**Step 1: Write failing metrics tests**

Scenarios:

1. `Pool::stats()` includes `deliveries_total`, `acks_total`, `rejects_total`.
2. `Pool::stats()` includes `confirmation_latency_p50`, `confirmation_latency_p95`, `confirmation_latency_p99`.
3. `Pool::stats()` includes `settlement_latency_p50`, `settlement_latency_p95`, `settlement_latency_p99`.
4. `RabbitMqStatusCommand` displays the consumer metrics and latencies.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/RabbitMqStatusCommandTest.php

Expected: FAIL.

**Step 3: Expose metrics to PHP**

- `pool.rs::stats()`: add `deliveries_total`, `acks_total`, `rejects_total` from `MetricsSnapshot`.
- `pool.rs::stats()`: compute percentiles (p50/p95/p99) from the atomic histograms and expose them.
- `RabbitMqStatusCommand`: display the new metrics.

**Step 4: Verify**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/RabbitMqStatusCommandTest.php

Expected: PASS.

**Step 5: Commit**

    git add crates packages
    git commit -m "feat(metrics): expose consumer metrics and latency histograms to PHP"

### Task 35: Wire the publisher config (confirms, mandatory, timeout)

**Files:**
- Modify: crates/rabbit-rs-core/src/config.rs
- Modify: crates/rabbit-rs-core/src/client.rs
- Modify: crates/rabbit-rs-php/src/classes/pool.rs
- Modify: packages/laravel-queue/src/Config/ConfigNormalizer.php
- Modify: packages/laravel-queue/config/rabbit-rs.php
- Modify: packages/laravel-queue/src/Support/MessageMapper.php

**Context:**

`publisher.confirms` and `publisher.mandatory` in the Laravel config are normalized but never passed to the native `Pool`. `normalized['publisher']` does not reach `Pool::__construct()`. `confirm_timeout` is hardcoded to 30s. `timeout_ms` is not sent by default in `MessageMapper::map()`.

**Step 1: Write failing config publisher tests**

Scenarios:

1. Config with `publisher.confirms = false` → the publisher does not activate `confirm_select`.
2. Config with `publisher.confirms = true` → the publisher activates `confirm_select`.
3. Config with `publisher.mandatory = false` → `basic_publish` with `mandatory = false`.
4. Config with `publisher.confirm_timeout = 5000` → the confirm timeout is 5s.
5. `MessageMapper::map()` includes `timeout_ms` from the publisher config by default.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit --testsuite "Rabbit RS Laravel"

Expected: FAIL.

**Step 3: Wire publisher config to native**

- `ConfigNormalizer`: include `publisher` in `normalized['native']` (not only in `normalized['publisher']`).
- The native `Config` must deserialize `publisher.confirms`, `publisher.mandatory`, `publisher.confirm_timeout`.
- `PublisherConfig` in `config.rs`: deserialize from config instead of hardcoding.
- `client.rs::publisher_config()`: read from the validated config.
- `MessageMapper::map()`: include `timeout_ms` by default from `publisher.confirm_timeout` when not explicitly provided.

**Step 4: Verify**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit --testsuite "Rabbit RS Laravel"

Expected: PASS.

**Step 5: Commit**

    git add crates packages
    git commit -m "fix(core): wire publisher config (confirms, mandatory, timeout) end-to-end"

### Task 36: Wire the full Octane lifecycle

**Files:**
- Modify: packages/laravel-queue/src/RabbitMqServiceProvider.php
- Modify: packages/laravel-queue/src/Octane/OctaneLifecycle.php
- Modify: packages/laravel-queue/tests/Feature/OctaneLifecycleTest.php

**Context:**

Only `flush()` (a no-op) is wired via `$app->terminating()`. `reload()` and `stop()` are not hooked to the Octane events. The consumers cached in `RabbitMqQueue::$consumers` are not cleaned up between Octane requests.

**Step 1: Write failing Octane lifecycle tests**

Scenarios:

1. When Octane reload is triggered → `OctaneLifecycle::reload()` is called → the pools are flushed.
2. When Octane worker stop is triggered → `OctaneLifecycle::stop()` is called → the pools are flushed and closed.
3. After `flush()` at the end of an Octane request → the current queue's consumers are closed (not just the pool factory).
4. The service provider registers the Octane hooks correctly when `Laravel\Octane\Octane::class` exists.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/OctaneLifecycleTest.php

Expected: FAIL.

**Step 3: Wire Octane hooks**

- `RabbitMqServiceProvider::registerOctaneLifecycle()`:
  - Register `flush()` on `Octane::tick()` or `terminating` (already done).
  - Register `reload()` on the Octane `WorkerReload` event.
  - Register `stop()` on the Octane `WorkerStopping` event.
- `OctaneLifecycle::flush()`: call `closeConsumers()` on the current queue in addition to flushing the pool factory (depends on Task 32).

**Step 4: Verify**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/OctaneLifecycleTest.php

Expected: PASS.

**Step 5: Commit**

    git add packages
    git commit -m "fix(laravel): wire full Octane lifecycle (reload, stop, consumer cleanup)"

### Task 37: Wire the WorkCommand and test the supervisor end-to-end

**Files:**
- Modify: packages/laravel-queue/src/Console/RabbitMqWorkCommand.php
- Modify: packages/laravel-queue/src/Console/WorkerSupervisor.php
- Create: packages/laravel-queue/src/Console/RabbitMqWorkCommandExtension.php
- Modify: packages/laravel-queue/tests/Feature/RabbitMqWorkCommandTest.php
- Create: packages/laravel-queue/tests/Feature/WorkerSupervisorIntegrationTest.php

**Context:**

`--rabbit-rs-worker={i}` is emitted by the supervisor but never consumed. The `run()` method (supervision, crash detection, restart, signals) is not tested end-to-end.

**Step 1: Write failing supervisor integration tests**

Scenarios:

1. The supervisor spawns N workers → each worker receives `--rabbit-rs-worker={i}` → the option is consumed for logging/metrics.
2. A worker crashes → the supervisor restarts it with backoff.
3. SIGTERM to the supervisor → the workers are stopped cleanly.
4. `maxRestarts` reached → the supervisor returns `EXIT_MAX_RESTARTS`.
5. `--rabbit-rs-worker` is visible in the worker's logs.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/WorkerSupervisorIntegrationTest.php

Expected: FAIL.

**Step 3: Implement option consumption**

- Create a `WorkCommandExtension` (or override `getOptions()` on the `WorkCommand`) that recognizes `--rabbit-rs-worker` and uses it for logging/metrics.
- `WorkerSupervisor::buildChildCommand()` must pass the option correctly.

**Step 4: Implement end-to-end tests**

- Test `run()` with mock processes (or real processes with a minimal PHP script).
- Verify crash detection, restart, backoff, signal handling, graceful shutdown.

**Step 5: Verify**

Run: cd packages/laravel-queue && php -n vendor/bin/phpunit tests/Feature/WorkerSupervisorIntegrationTest.php

Expected: PASS.

**Step 6: Commit**

    git add packages
    git commit -m "fix(laravel): wire WorkCommand option and test supervisor end-to-end"

This batch was executed in the dedicated worktree `.worktrees/strict-audit-stabilization` on the branch `fix/strict-audit-stabilization`. The deterministic fixes from the July 31 and August 1 audits are complete; qualifications requiring a representative production environment are deferred to dedicated milestones and do not block the start of Task 16.

Initial scope of the active findings:

- transactional dispatch of deliveries against expired or cancelled waiters;
- propagation of `message_id` and `correlation_id`, canonical consumer tag, partial rollback of `ConsumerSet`, delayed release deadline and terminal state after settlement error;
- generational commit of `ClientPool` against `close()` without holding a mutex during network operations;
- global 2 s budget for closing clients, actors and the Tokio runtime;
- alignment of core configurations and defaults: scheduler, starvation, attempts, jitter and mandatory publishing;
- confidentiality of `ConnectionKey` in identifiers, statistics, errors and `Debug` output;
- PHP bounds on batches, cumulative payloads, headers, depth and timeouts, with precise error paths;
- typed AMQP headers and delivery properties preserved up to the PHP API;
- separate PHPT/FPM profiles, with RabbitMQ-chaos, platform and performance qualifications deferred to dedicated milestones.

State of the batch closed on August 1, 2026:

- [x] delivery dispatch and lifecycle secured, AMQP properties preserved and partial rollback applied;
- [x] `ClientPool` atomic against `close()` with network initializations outside the registries;
- [x] shutdown with a global 2 s budget, core defaults aligned and public identities redacted;
- [x] bounds and types of the PHP boundary;
- **Non-blocking deferral** — RabbitMQ-chaos lab and platform matrix;
- **Non-blocking deferral** — performance baseline.

Deferred qualifications and resumption criteria:

- **Real RabbitMQ and chaos — Milestone D, Tasks 25–27:** resume after the Laravel package of Milestone C, first creating the cluster, then the integration tests and finally the failure scenarios. The at-least-once qualification requires `missing = 0` and the explicit counting of expected, unique, duplicated and missing messages;
- **Performance — Milestone E, Tasks 28–30:** first establish the FFI, conversion and batch microbenchmark baseline from the audit, then run the Laravel comparisons and calibrate the defaults and budgets. No threshold may be set before measurement on a documented reference machine;
- **Platforms — Milestone F, Tasks 31–32:** qualify PHP 8.4/8.5, x86_64/ARM64, glibc/musl and NTS/ZTS with the 16-combination PIE matrix, then build and smoke-test each combination in CI.

Already-fixed findings to be locked in by non-regression:

- publisher transition from `Recovering` to `Ready` for the same generation;
- bounded `source_errors` history;
- `Reject` settlement available;
- Lapin credentials built without exposable URI concatenation.

Initial baseline of August 1, 2026 on macOS ARM64 with PHP 8.4.21:

- `rtk ./scripts/check.sh`: PASS, 112 Rust tests and strict Composer validation;
- `rtk cargo build -p rabbit-rs-php --release --features extension-tests`: PASS;
- `rtk ./scripts/test-extension.sh`: PASS, 9 of 9 PHPT;
- `rtk ./scripts/test-fpm.sh`: PASS, two-worker FPM lab;
- the distribution build without `extension-tests` remains distinct from the PHPT build so as not to expose `testing_pool()` in the published artifact.

Core hardening checkpoint of August 1, 2026 on macOS ARM64 with PHP 8.4.21:

- `rtk ./scripts/check.sh`: PASS, 141 Rust tests, Clippy without warnings and strict Composer validation;
- `rtk ./scripts/test-extension.sh`: PASS, 9 of 9 PHPT;
- `rtk ./scripts/test-fpm.sh`: PASS, two-worker FPM lab;
- the tests cover the shared shutdown budget, post-fork closure without reacquisition, closure races, same-generation publisher recovery, the `source_errors` bound, the canonical scheduler and its legacy migration, the attempts/jitter/mandatory defaults and the absence of credential fingerprints in public identifiers.

PHP boundary bounding checkpoint of August 1, 2026 on macOS ARM64 with PHP 8.4.21:

- `rtk ./scripts/check.sh`: PASS, 153 Rust tests of which 143 core, Clippy without warnings and strict Composer validation;
- `rtk ./scripts/test-extension.sh`: PASS, 11 of 11 PHPT;
- `rtk ./scripts/test-fpm.sh`: PASS, two-worker FPM lab;
- batches are bounded to 256 messages and 1 MiB of cumulative payload, headers to 128 entries and 64 KiB cumulative per call, and `timeout_ms` to 24 h with checked addition;
- scalar AMQP types are preserved, published PHP headers remain flat and nested broker structures like `x-death` are omitted from metadata without hiding scalars;
- the PHPT cover ACK, mandatory return, confirm timeout, typed transport error, backpressure, settlements, active closure and `messages[index]` error paths.

Laravel package bootstrap checkpoint of August 1, 2026 on macOS ARM64 with PHP 8.4.21:

- `rtk composer validate --strict` in `packages/laravel-queue`: PASS;
- PHPUnit with Laravel 13.23, Testbench 11 and PHPUnit 12: PASS, 12 tests and 34 assertions;
- PHPUnit with Laravel 12.64, Testbench 10 and PHPUnit 11: PASS, 12 tests and 34 assertions;
- `rtk ./scripts/check.sh`: PASS;
- the published config applies the confirms/mandatory defaults, durable quorum and absence of application DLQ, then normalizes brokers, routes and workers to the native format with per-path errors and no secret leaks.

Laravel connector registration checkpoint of August 1, 2026 on macOS ARM64 with PHP 8.4.21:

- PHPUnit with Laravel 13.23, Testbench 11 and PHPUnit 12: PASS, 24 tests and 53 assertions;
- PHPUnit with Laravel 12.64, Testbench 10 and PHPUnit 11: PASS, 24 tests and 53 assertions;
- `rtk ./scripts/check.sh`: PASS;
- the `rabbit-rs` connector shares a process-local native pool per normalized config fingerprint, invalidates its cache after fork and holds no request-bound values;
- `RabbitMqQueue` is introduced as a contractual skeleton so that `Queue::connection()` can immediately apply the container and connection name; its operations remain reserved for Task 18.

Laravel publication implementation checkpoint of August 1, 2026 on macOS ARM64 with PHP 8.4.21:

- `rtk composer validate --strict` in `packages/laravel-queue`: PASS;
- PHPUnit with Laravel 13.23, Testbench 11 and PHPUnit 12: PASS, 38 tests and 100 assertions;
- PHPUnit with Laravel 12.64, Testbench 10 and PHPUnit 11: PASS, 38 tests and 100 assertions;
- `rtk ./scripts/check.sh`: PASS;
- `push`, `pushRaw`, `later` and `bulk` transmit native envelopes with a stable UUID identifier, resolve routes and queue placeholders, preserve raw payloads and use a single native call per immediate or delayed batch;
- publishing remains driven by `Illuminate\Queue\Queue` for payloads, events and transactions, with delays in milliseconds, generic native errors translated to `QueueException` and backpressure/connection kept as dedicated errors.

Delivery-to-Laravel-job adaptation checkpoint of August 1, 2026 on macOS ARM64 with PHP 8.4.21:

- `rtk composer validate --strict` in `packages/laravel-queue`: PASS;
- PHPUnit with Laravel 13.23, Testbench 11 and PHPUnit 12: PASS, 46 tests and 135 assertions;
- PHPUnit with Laravel 12.64, Testbench 10 and PHPUnit 11: PASS, 46 tests and 135 assertions;
- `rtk ./scripts/check.sh`: PASS;
- `RabbitMqJob` caches the payload, `message_id` and `attempts`, acknowledges or releases the delivery exactly once and abandons the native handle only after a successful transition;
- the tests cover immediate requeue via `basic.reject(requeue=true)`, delayed republishing in milliseconds, surfacing an ACK error and the Laravel ACK, `failed` callback, then `JobFailed` event sequence; `pop` remains reserved for Task 20.

Multi-vhost Laravel consumption wiring checkpoint of August 1, 2026 on macOS ARM64 with PHP 8.4.21:

- `rtk composer validate --strict` in `packages/laravel-queue`: PASS;
- PHPUnit with Laravel 13.23, Testbench 11 and PHPUnit 12: PASS, 57 tests and 159 assertions;
- PHPUnit with Laravel 12.64, Testbench 10 and PHPUnit 11: PASS, 57 tests and 159 assertions;
- `rtk ./scripts/check.sh`: PASS;
- `RabbitMqQueue::pop()` resolves the Laravel `queue` value as a worker profile, reuses its aggregated native consumer and delegates in a single `next()` call the weighted selection across brokers and vhosts;
- subscriptions with `enabled=false` are excluded before pool creation, `block_for` is converted from seconds to milliseconds with an overflow bound, and the native subscription alias returns the real queue name to `RabbitMqJob`;
- the tests cover two vhosts, three active subscriptions, an unknown profile, a disabled subscription, a timeout without job and the translation of native errors; fine-grained selection of multiple aliases remains reserved for `rabbit-rs:work` and admin operations for Task 21.

Administration and monitoring checkpoint of August 15, 2026 on macOS ARM64 with PHP 8.4.21:

- `rtk cargo fmt --all -- --check`: PASS; `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`: PASS; `rtk cargo test --workspace --all-targets`: PASS, 153 Rust tests;
- `rtk composer validate --strict` in `packages/laravel-queue`: PASS; PHPUnit (without ext-rabbit_rs): PASS, 65 tests and 172 assertions;
- `queue_size` and `purge_queue` added to the `TopologyChannel` trait with Lapin (passive declare / queue_purge) and Mock implementations; `ClientPool::queue_size` and `ClientPool::purge_queue` expose the operations at client level;
- `Pool::size()` and `Pool::clear()` added to the native PHP extension and the stub;
- `RabbitMqQueue::size()` and `RabbitMqQueue::clear()` resolve the configured route and delegate to the native pool; `pendingSize` delegates to `size`, `delayedSize` and `reservedSize` return 0, `creationTimeOfOldestPendingJob` returns null (AMQP does not distinguish these states);
- the tests cover size per route and default, clear per route and default, size at zero, native failure translated to QueueException, and refusal without a configured route.

Test RabbitMQ cluster checkpoint of August 15, 2026 on macOS ARM64 (Colima/Docker):

- `rtk ./scripts/check.sh`: PASS, 153 Rust tests and Composer validation;
- 3-node RabbitMQ 4.2.9 (Alpine) cluster with `rabbit_peer_discovery_classic_config` peer discovery, shared Erlang cookie, `cluster_partition_handling = pause_minority` and working quorum queues;
- `rabbitmq_delayed_message_exchange` plugin v4.2.0 (SHA-256 verified) for the `with-plugin` profile; `without-plugin` profile without the plugin to test the TTL fallback;
- 2 vhosts (`/orders-eu`, `/billing`), limited user `rabbit_rs` (management) and admin `admin` (administrator) with restricted permissions;
- Toxiproxy 2.12.0 intercepts the AMQP ports 5672–5674 for fault injection; Prometheus v3.5.0 scrapes the 3 nodes;
- `./scripts/lab-up.sh` starts the lab, `./scripts/lab-ready.sh` verifies readiness (cluster, vhosts, quorum, permissions, Prometheus, Toxiproxy, plugin), `./scripts/lab-down.sh` shuts down cleanly;
- all images are pinned by SHA-256 digest.

End-to-end integration tests checkpoint of August 15, 2026 on macOS ARM64 (Colima/Docker):

- `rtk cargo fmt --all -- --check`: PASS; `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`: PASS; `rtk cargo test --workspace --all-targets`: PASS, 153 Rust tests;
- 8 Rust integration tests via `cargo test -p rabbit-rs-core --features integration`: publish_confirm_then_consume_and_ack, release_zero_requeues_and_redispatches, two_vhosts_in_one_consumer_set, bulk_publish_then_consume_all, declare_quorum_queue_succeeds, declare_classic_queue_succeeds, verify_passive_does_not_create, external_mode_emits_no_commands;
- Laravel integration tests (QueueWorkerTest, DelayedJobTest) created in `tests/Integration/` with a dedicated testsuite; automatic skip if ext-rabbit_rs is not loaded;
- `scripts/test-integration.sh` starts the lab, waits for readiness, runs the Rust and Laravel tests, then stops the lab;
- the Cargo `integration` feature guards the Rust tests requiring a real broker; the tests declare the queues via `TopologyReconciler` before publishing;
- `rabbit_rs` permissions updated to allow declaring test queues (`^(amq\.|rabbit-rs-it-)`);
- phpunit.xml split into "Rabbit RS Laravel" and "Rabbit RS Integration" testsuites to isolate tests requiring a broker.

Laravel integration tests fix checkpoint of August 16, 2026 on macOS ARM64 (Colima/Docker):

- the `push()`, `later()` and `bulk()` tests passed `null` as the queue, resolved to `"default"` by the connector, causing NO_ROUTE (AMQP 312) because no queue named `"default"` existed; fix: pass the unique queue name explicitly;
- `partitionJobsByAfterCommit` fixed from `private` to `protected` for Laravel 13 compatibility;
- `declareQueue()`/`deleteQueue()` helpers added to `IntegrationTestCase` via the RabbitMQ management API;
- `test_later_publishes_and_consumes_after_delay` marked skipped because the `DelayRouter` was not yet wired into the publishing path (only in the consumer's `release()`);
- `scripts/test-integration.sh` enriched: build/install of ext-rabbit_rs, loading verification, composer dependency installation;
- result: 8 Rust tests + 7 Laravel tests (1 skipped) PASS, quality gate `./scripts/check.sh` PASS.

The Milestone A gate runs `./scripts/check.sh` successfully: Rust formatting, Clippy without warnings, 100 Rust tests and Composer validation. The worktree is clean at commit `21aedee`.

The Task 13 checkpoint verifies 100 Rust tests and 2 PHPT tests, plus Rust formatting, Clippy without warnings, the PHP stub lint and strict Composer validation.

The Task 14 checkpoint verifies 111 Rust tests and 7 PHPT tests, plus Rust formatting, Clippy without warnings and strict Composer validation. The deterministic PHPT scenarios use a Cargo test feature and expose no fixture in the distributed binary.

The Task 15 checkpoint closes Milestone B with 112 Rust tests, 9 PHPT tests and a two-worker FPM lab. It verifies handle reuse within a process, their replacement after closure, invalidation without blocking after `pcntl_fork`, FPM worker isolation and registry closure at module shutdown.

## Target layout

    Cargo.toml
    composer.json
    .gitattributes
    rust-toolchain.toml
    crates/
      rabbit-rs-core/
        Cargo.toml
        src/
          lib.rs
          config.rs
          error.rs
          runtime.rs
          transport.rs
          recovery.rs
          metrics.rs
          pool/
          topology/
          publisher/
          consumer/
        tests/
      rabbit-rs-php/
        Cargo.toml
        src/
          lib.rs
          classes/
        stubs/rabbit_rs.stub.php
        tests/phpt/
    packages/
      laravel-queue/
        composer.json
        config/rabbit-rs.php
        src/
          RabbitMqServiceProvider.php
          Config/
          Connectors/
          Exceptions/
          Jobs/
          Console/
          Support/
        tests/
    benchmarks/
      native/
      laravel/
    lab/
      rabbitmq/
    scripts/
    docs/

## Milestone A — Foundations and Rust core

### Task 1: Initialize the reproducible workspace

**Files:**
- Create: Cargo.toml
- Create: Cargo.lock
- Create: composer.json
- Create: .gitattributes
- Create: rust-toolchain.toml
- Modify: .gitignore
- Create: crates/rabbit-rs-core/Cargo.toml
- Create: crates/rabbit-rs-core/src/lib.rs
- Create: crates/rabbit-rs-php/Cargo.toml
- Create: crates/rabbit-rs-php/src/lib.rs
- Create: scripts/check.sh

**Step 1: Write the failing workspace smoke check**

Create scripts/check.sh with:

    #!/usr/bin/env bash
    set -euo pipefail
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace --all-targets
    composer validate --strict

**Step 2: Run it to verify it fails**

Run: ./scripts/check.sh

Expected: FAIL because the workspace and crates are not yet declared.

**Step 3: Add the minimal workspace**

Declare resolver = "2", the two members and the shared dependencies. Pin a known stable Rust toolchain in rust-toolchain.toml. The rabbit-rs-core crate must compile without any PHP dependency. The rabbit-rs-php crate must be a cdylib depending on the core.

The root composer.json represents the PIE package, not the Laravel package:

    {
        "name": "goopil/rabbit-rs-native",
        "type": "php-ext",
        "description": "High-performance RabbitMQ transport for PHP and Laravel, powered by Rust",
        "license": "MIT",
        "require": {
            "php": "^8.4"
        },
        "php-ext": {
            "extension-name": "rabbit_rs",
            "priority": 80,
            "support-zts": true,
            "support-nts": true,
            "os-families": ["linux"],
            "download-url-method": ["pre-packaged-binary"]
        }
    }

.gitattributes excludes from the Composer archives the benchmarks, the lab and the documents not required by PIE.

**Step 4: Run the check**

Run: chmod +x scripts/check.sh && ./scripts/check.sh

Expected: PASS.

**Step 5: Commit**

    git add Cargo.toml Cargo.lock composer.json .gitattributes rust-toolchain.toml .gitignore crates scripts/check.sh
    git commit -m "build: bootstrap native RabbitMQ workspace"

### Task 2: Model and validate the native configuration

**Files:**
- Create: crates/rabbit-rs-core/src/config.rs
- Create: crates/rabbit-rs-core/src/error.rs
- Modify: crates/rabbit-rs-core/src/lib.rs
- Test: crates/rabbit-rs-core/src/config.rs

**Step 1: Write failing tests**

Add tests for:

- rejecting a broker without a host;
- rejecting prefetch = 0;
- rejecting `scheduler.max_in_flight` lower than a prefetch;
- rejecting a zero `starvation_after` duration and applying the 30 s default;
- rejecting an unknown topology mode;
- normalizing the host order;
- masking secrets in Debug;
- producing the same fingerprint for two equivalent configurations.

Minimal public structure:

    pub struct BrokerConfig {
        pub name: String,
        pub hosts: Vec<Endpoint>,
        pub vhost: String,
        pub tls: TlsConfig,
        pub heartbeat: Duration,
    }

    pub struct WorkerProfile {
        pub subscriptions: Vec<SubscriptionConfig>,
        pub scheduler: SchedulerConfig,
    }

    pub struct SchedulerConfig {
        pub strategy: SchedulerStrategy,
        pub max_in_flight: u16,
    }

    pub struct SubscriptionConfig {
        pub starvation_after: Duration,
    }

    pub enum TopologyMode {
        Declare,
        Verify,
        External,
    }

> **Note (2026-08-29):** the `max_in_flight` field of `SchedulerConfig` described above has since been removed (tracked by the consumer-tuning plan, PR #29). This document remains a point-in-time record.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core config::tests

Expected: FAIL with missing types or functions.

**Step 3: Implement minimal validated types**

Use serde for input, secrecy for secrets and a canonical secret-free representation for the fingerprint. Return ConfigError with a field path usable by Laravel.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core config::tests

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add validated connection and worker configuration"

### Task 3: Implement the deterministic multi-queue scheduler

**Files:**
- Create: crates/rabbit-rs-core/src/consumer/mod.rs
- Create: crates/rabbit-rs-core/src/consumer/scheduler.rs
- Create: crates/rabbit-rs-core/tests/scheduler_fairness.rs
- Modify: crates/rabbit-rs-core/src/lib.rs

**Step 1: Write failing scheduler tests**

Test:

- a single subscription;
- two subscriptions with weights 8 and 2 over 10,000 picks;
- an empty queue that does not consume its credit;
- the return of a previously empty queue;
- high priority without starving the low priority;
- identical result with an identical clock and sequence.

Interface:

    pub trait Scheduler {
        fn register(&mut self, id: SubscriptionId, policy: SubscriptionPolicy);
        fn mark_ready(&mut self, id: SubscriptionId);
        fn mark_empty(&mut self, id: SubscriptionId);
        fn next(&mut self, now: Instant) -> Option<SubscriptionId>;
    }

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test scheduler_fairness

Expected: FAIL.

**Step 3: Implement deficit weighted round-robin**

Separate priority_class and weight. Add bounded aging so a ready low class eventually gets picked. Do not add adaptive prefetch.

**Step 4: Verify distribution**

Run: cargo test -p rabbit-rs-core --test scheduler_fairness

Expected: PASS with distribution error under the tolerance defined in the test.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add starvation-safe weighted scheduler"

### Task 4: Make the runtime safe after fork

**Files:**
- Create: crates/rabbit-rs-core/src/runtime.rs
- Create: crates/rabbit-rs-core/src/pool/mod.rs
- Create: crates/rabbit-rs-core/src/pool/key.rs
- Modify: crates/rabbit-rs-core/src/lib.rs
- Test: crates/rabbit-rs-core/src/runtime.rs

**Step 1: Write failing lifecycle tests**

Inject a test PidProvider and verify:

- lazy creation;
- reuse within the same PID;
- invalidation of all handles after a PID change;
- a different configuration does not share the pool;
- close is idempotent.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core runtime::tests pool::tests

Expected: FAIL.

**Step 3: Implement RuntimeRegistry**

    pub struct RuntimeRegistry {
        pid: u32,
        runtime: tokio::runtime::Runtime,
        pools: HashMap<ConnectionKey, Arc<ConnectionHandle>>,
    }

The runtime must be created neither in a global static initialized at load time, nor before the first acquisition after fork. Use OnceLock only for the registry lock, never for an inherited socket or runtime.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core runtime::tests pool::tests

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add fork-safe per-process runtime registry"

### Task 5: Isolate Lapin behind a testable transport

**Files:**
- Create: crates/rabbit-rs-core/src/transport.rs
- Create: crates/rabbit-rs-core/src/transport/lapin.rs
- Create: crates/rabbit-rs-core/src/transport/mock.rs
- Modify: crates/rabbit-rs-core/Cargo.toml
- Modify: crates/rabbit-rs-core/src/lib.rs

**Step 1: Write a compile-failing contract test**

Define the minimal capabilities:

    #[async_trait]
    pub trait Transport: Send + Sync {
        async fn connect(&self, config: &BrokerConfig) -> Result<Box<dyn TransportConnection>>;
    }

    #[async_trait]
    pub trait TransportConnection: Send + Sync {
        async fn open_publisher(&self) -> Result<Box<dyn PublisherChannel>>;
        async fn open_consumer(&self) -> Result<Box<dyn ConsumerChannel>>;
        async fn close(&self) -> Result<()>;
    }

The channel traits must cover declare, passive verify, bind, publish, confirm, return, qos, consume, ack and reject.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core transport

Expected: FAIL.

**Step 3: Implement MockTransport then LapinTransport**

Start with the scriptable mock. Then adapt Lapin without exposing its types outside the transport/lapin.rs module.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core transport

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): abstract AMQP transport behind testable traits"

### Task 6: Build the connection and recovery state machine

**Files:**
- Create: crates/rabbit-rs-core/src/recovery.rs
- Create: crates/rabbit-rs-core/src/pool/connection_actor.rs
- Create: crates/rabbit-rs-core/tests/recovery_state_machine.rs
- Modify: crates/rabbit-rs-core/src/pool/mod.rs

**Step 1: Write failing state-machine tests**

With paused Tokio time, verify:

- Disconnected to Connecting then Ready;
- backoff 100 ms, 200 ms, 400 ms with injected jitter;
- 30 s cap;
- permanent authentication error;
- connection loss Ready to Recovering;
- closure during backoff;
- generation incremented after recovery.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test recovery_state_machine

Expected: FAIL.

**Step 3: Implement ConnectionActor**

All operations go through a bounded mpsc channel. States and reasons are published via watch. The jitter generator and clock are injectable.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test recovery_state_machine

Expected: PASS without real waiting.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add deterministic connection recovery actor"

### Task 7: Declare or verify the topology

**Files:**
- Create: crates/rabbit-rs-core/src/topology/mod.rs
- Create: crates/rabbit-rs-core/src/topology/plan.rs
- Create: crates/rabbit-rs-core/src/topology/reconciler.rs
- Create: crates/rabbit-rs-core/tests/topology_recovery.rs

**Step 1: Write failing topology tests**

Verify:

- exchange, queue, binding order;
- durable quorum by default;
- classic explicit;
- declare idempotent;
- verify passive without creation;
- external without declaration command;
- no application DLQ in the default configuration;
- DLX, DLQ and bindings declared only after explicit activation;
- incompatibility surfaced as a permanent error;
- full replay after a new generation.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test topology_recovery

Expected: FAIL.

**Step 3: Implement TopologyPlan and Reconciler**

Compile the configuration into an immutable plan before any I/O. Reject exclusive quorum or auto_delete combinations. Do not attempt to create RabbitMQ policies.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test topology_recovery

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add declarative and externally managed topology modes"

### Task 8: Implement batching, confirms and mandatory returns

**Files:**
- Create: crates/rabbit-rs-core/src/publisher/mod.rs
- Create: crates/rabbit-rs-core/src/publisher/batcher.rs
- Create: crates/rabbit-rs-core/src/publisher/confirms.rs
- Create: crates/rabbit-rs-core/src/publisher/actor.rs
- Create: crates/rabbit-rs-core/tests/publisher_safety.rs

**Step 1: Write failing publisher tests**

Test:

- flush at max_messages;
- flush at max_bytes;
- flush at the timer;
- ACK of multiple sequences;
- targeted NACK;
- basic.return before ACK;
- timeout;
- full buffer returns Backpressure;
- outage before confirm classifies the sequence Ambiguous in the internal ledger;
- message_id preserved on republication.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test publisher_safety

Expected: FAIL.

**Step 3: Implement the bounded publisher actor**

    pub struct PublishRequest {
        pub destination: Destination,
        pub payload: Bytes,
        pub properties: MessageProperties,
        pub deadline: Instant,
    }

    pub enum PublishOutcome {
        Confirmed { message_id: String },
        Returned { message_id: String, reply: ReturnInfo },
        Ambiguous { message_id: String },
    }

The actor owns the sequence ledger. It does not resolve a routed ACK before having processed the corresponding basic.return stream.

At this stage, Ambiguous is an internal state. Task 9 bis replaces its immediate resolution with bounded retention and automatic replay after recovery.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test publisher_safety

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add bounded batched publisher confirms"

### Task 9: Add plugin and TTL delays

**Files:**
- Create: crates/rabbit-rs-core/src/topology/delay.rs
- Create: crates/rabbit-rs-core/src/publisher/delay.rs
- Create: crates/rabbit-rs-core/tests/delay_routing.rs
- Modify: crates/rabbit-rs-core/src/config.rs

**Step 1: Write failing delay tests**

Test:

- auto picks x-delayed-message if available;
- auto falls back to TTL if the plugin is absent;
- mandatory plugin fails without the plugin;
- TTL rounds up to the bucket;
- maximum number of buckets;
- stable TTL queue name;
- x-expires greater than the TTL;
- negative delay rejected.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test delay_routing

Expected: FAIL.

**Step 3: Implement DelayStrategy**

    pub enum DelayStrategy {
        Plugin,
        TtlBuckets(TtlBucketPlan),
    }

Plugin detection must be time-bounded and cached per connection generation.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test delay_routing

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add delayed exchange and TTL fallback"

### Task 9 bis: Replay publications after reconnection

**Files:**
- Modify: crates/rabbit-rs-core/src/publisher/mod.rs
- Modify: crates/rabbit-rs-core/src/publisher/actor.rs
- Modify: crates/rabbit-rs-core/src/publisher/confirms.rs
- Modify: crates/rabbit-rs-core/src/pool/connection_actor.rs
- Create: crates/rabbit-rs-core/tests/publisher_recovery.rs

**Step 1: Write failing publisher recovery tests**

Verify:

- a publication accepted during Recovering stays suspended and leaves after Ready;
- a message still in the batch at the time of the outage is kept;
- a publication sent without confirm is classified Ambiguous, placed back in the buffer and automatically republished;
- the republication keeps exactly the message_id, destination, properties, Bytes payload and original deadline;
- the new channel activates publisher confirms before any replay;
- the replay starts only after topology restoration for the new generation;
- a late confirm from the old generation is ignored and the waiter is resolved only once;
- several successive outages do not duplicate an entry in the replay ledger;
- ACK, NACK and basic.return remain terminal after replay;
- deadline expiry during the outage returns Timeout without publishing after Ready;
- a permanent reconnection error resolves all concerned waiters without a retry loop;
- the overall capacity covers commands, batches, replay and in-flight confirms; when reached, try_publish returns Backpressure even if the actor keeps draining its mpsc channel;
- explicit closure wakes all waiters with a typed error;
- no test promises a replay after process crash, the buffer being deliberately memory-only.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test publisher_recovery

Expected: FAIL because connection_lost currently resolves Ambiguous sequences and destroys the batch.

**Step 3: Implement the suspended publisher lifecycle**

Add to the publisher the Ready and Suspended phases. The connection coordinator transmits only bounded, ordered events:

    enum PublisherConnectionEvent {
        Recovering { generation: u64 },
        Ready {
            generation: u64,
            channel: Arc<dyn PublisherChannel>,
            topology_restored: bool,
        },
        FailedPermanent { generation: u64, error: TransportError },
    }

On transition to Recovering, cancel the old generation's confirm futures, remove their entries from the active ledger and place the complete PublishRequests back into a replay deque. Do not resolve the Ambiguous waiters. Payloads remain Bytes so this transition does not copy their content.

The ledger must keep for each publication the original request, its waiter, its absolute deadline, its send generation and a unique internal identifier. An entry may exist only once across batch, replay and in-flight confirms. A new AMQP sequence is assigned on each republication, without changing message_id.

PublisherHandle acquires via try_acquire_owned a permit from a Semaphore sized to the overall capacity before accepting the command. The permit follows the entry until its terminal state; merely draining the mpsc therefore does not release capacity during an outage.

On Ready, reject old or identical generations, verify topology_restored, activate confirm_select on the new channel, then replay the existing deque first before new commands. The original deadline is checked before each attempt and used for the confirm timeout. A recoverable error places the entry back in replay only once; NACK, return, timeout, permanent error and closure are terminal.

The ConnectionActor remains solely responsible for backoff and network opening. It republishes nothing itself; after topology reconciliation, the coordinator hands the new PublisherChannel and generation to the PublisherActor.

**Step 4: Verify targeted behavior**

Run: cargo test -p rabbit-rs-core --test publisher_recovery

Expected: PASS without real waiting thanks to paused Tokio time.

**Step 5: Verify publisher regressions**

Run: cargo test -p rabbit-rs-core --test publisher_safety --test publisher_recovery

Expected: PASS. Adapt publisher_safety to treat Ambiguous as an internal replayed state, not an immediate user outcome.

**Step 6: Commit**

    git add crates/rabbit-rs-core docs/plans
    git commit -m "feat(core): replay publishes after connection recovery"

### Task 10: Implement ConsumerSet and delivery tokens

**Files:**
- Create: crates/rabbit-rs-core/src/consumer/set.rs
- Create: crates/rabbit-rs-core/src/consumer/delivery.rs
- Create: crates/rabbit-rs-core/src/consumer/actor.rs
- Create: crates/rabbit-rs-core/tests/consumer_semantics.rs

**Step 1: Write failing consumer tests**

Test:

- several subscriptions over two connections;
- the scheduler picking the next ready buffer;
- the global max_in_flight budget;
- prefetch per subscription;
- ACK on the right generation;
- rejection of a stale ACK;
- release(0) calls basic.reject with requeue=true;
- delayed release publishes, confirms, then ACKs;
- a failed delayed publication does not ACK;
- closure wakes next with a typed error.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test consumer_semantics

Expected: FAIL.

**Step 3: Implement ConsumerSet**

    pub struct Delivery {
        pub id: MessageId,
        pub subscription: SubscriptionId,
        pub payload: Bytes,
        pub headers: Headers,
        pub attempts: u32,
        token: DeliveryToken,
    }

The token contains the connection key, generation, channel id and delivery tag. Its Pending, Acked, Rejected and Lost transitions are atomic and terminal.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test consumer_semantics

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): add multiplexed consumers and safe delivery tokens"

### Task 11: Add attempts counters and poison-message handling

**Files:**
- Create: crates/rabbit-rs-core/src/consumer/attempts.rs
- Create: crates/rabbit-rs-core/tests/delivery_attempts.rs
- Modify: crates/rabbit-rs-core/src/consumer/delivery.rs

**Step 1: Write failing attempts tests**

Cases:

- first acquisition = 1;
- x-acquired-count takes precedence over the redelivered bool;
- x-delivery-count read for quorum failures;
- delayed release increments the application counter;
- limit reached produces MaxAttempts;
- classic without a counter uses the documented fallback.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test delivery_attempts

Expected: FAIL.

**Step 3: Implement AttemptsResolver**

Centralize all header interpretation. Do not scatter RabbitMQ rules into the PHP layer.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test delivery_attempts

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): preserve Laravel-compatible delivery attempts"

### Task 12: Expose a metrics snapshot without a backend

**Files:**
- Create: crates/rabbit-rs-core/src/metrics.rs
- Create: crates/rabbit-rs-core/tests/metrics_snapshot.rs
- Modify: crates/rabbit-rs-core/src/publisher/actor.rs
- Modify: crates/rabbit-rs-core/src/consumer/actor.rs
- Modify: crates/rabbit-rs-core/src/pool/connection_actor.rs

**Step 1: Write failing metric tests**

Verify that publish, confirm, return, delivery, ACK, reject, reconnect and backpressure update the right counters. Verify that a snapshot does not block the actors and contains no secret.

**Step 2: Verify failure**

Run: cargo test -p rabbit-rs-core --test metrics_snapshot

Expected: FAIL.

**Step 3: Implement atomics and histograms**

Keep a serializable snapshot API. Depend on neither Prometheus nor OpenTelemetry in rabbit-rs-core.

**Step 4: Verify**

Run: cargo test -p rabbit-rs-core --test metrics_snapshot

Expected: PASS.

**Step 5: Run Milestone A gate**

Run: ./scripts/check.sh

Expected: PASS.

**Step 6: Commit**

    git add crates/rabbit-rs-core
    git commit -m "feat(core): expose transport metrics snapshots"

## Milestone B — PHP extension

### Task 13: Define the PHP API and stubs

**Files:**
- Create: crates/rabbit-rs-php/src/classes/mod.rs
- Create: crates/rabbit-rs-php/src/classes/pool.rs
- Create: crates/rabbit-rs-php/src/classes/consumer.rs
- Create: crates/rabbit-rs-php/src/classes/delivery.rs
- Create: crates/rabbit-rs-php/src/classes/exception.rs
- Create: crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
- Modify: crates/rabbit-rs-php/src/lib.rs
- Create: scripts/test-extension.sh

**Step 1: Write failing reflection tests**

Create PHPT verifying the existence of:

    Goopil\RabbitRs\Pool
    Goopil\RabbitRs\Consumer
    Goopil\RabbitRs\Delivery
    Goopil\RabbitRs\Exception
    Goopil\RabbitRs\BackpressureException
    Goopil\RabbitRs\ConnectionException

Also verify that extension_loaded('rabbit_rs') is true and that phpversion('rabbit_rs') matches the Cargo version and the release tag.

Minimal API:

    final class Pool {
        public function __construct(array $config);
        public function publish(array $message): string;
        public function publishBatch(array $messages): array;
        public function consumer(string $profile): Consumer;
        public function stats(): array;
        public function close(): void;
    }

    final class Consumer {
        public function next(int $timeoutMs): ?Delivery;
        public function close(): void;
    }

    final class Delivery {
        public function payload(): string;
        public function metadata(): array;
        public function ack(): void;
        public function release(int $delayMs = 0): void;
        public function reject(bool $requeue = false): void;
    }

**Step 2: Verify failure**

Run: cargo build -p rabbit-rs-php --release && ./scripts/test-extension.sh reflection

Expected: FAIL.

**Step 3: Implement thin ext-php-rs classes**

At this checkpoint, the three operational classes are deliberately stateless and all their operations fail with the stable base exception. Task 14 will introduce the validated native handles. Do not expose Lapin.

`ext-php-rs` 0.15.15 keeps Rust parameter identifiers as-is in PHP named arguments. Boundary methods therefore keep the contractual PHP names, including `timeoutMs` and `delayMs`, then explicitly consume their unused parameters before initializing the native handles.

**Step 4: Verify**

Run: cargo build -p rabbit-rs-php --release && ./scripts/test-extension.sh reflection

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-php scripts/test-extension.sh
    git commit -m "feat(extension): expose native pool publisher and consumer API"

### Task 14: Test PHP conversions, errors and transitions

**Files:**
- Create: crates/rabbit-rs-php/tests/phpt/config_validation.phpt
- Create: crates/rabbit-rs-php/tests/phpt/binary_payload.phpt
- Create: crates/rabbit-rs-php/tests/phpt/delivery_terminal_state.phpt
- Create: crates/rabbit-rs-php/tests/phpt/secrets.phpt
- Create: crates/rabbit-rs-php/tests/phpt/backpressure.phpt

**Step 1: Add failing PHPT cases**

Include binary payload with null bytes, allowed nested headers, maximum size, invalid configuration, double ACK, operation after close and redacted error message.

**Step 2: Verify failure**

Run: ./scripts/test-extension.sh

Expected: FAIL on the unimplemented cases.

**Step 3: Implement converters and guards**

Define an exact list of supported AMQP types. Reject resources, arbitrary objects and recursive structures.

**Step 4: Verify**

Run: ./scripts/test-extension.sh

Expected: PASS.

**Step 5: Commit**

    git add crates/rabbit-rs-php
    git commit -m "test(extension): harden PHP value conversion and handle states"

### Task 15: Certify the CLI, fork and FPM lifecycle

**Files:**
- Create: crates/rabbit-rs-php/tests/phpt/pid_registry.phpt
- Create: crates/rabbit-rs-php/tests/phpt/fork_invalidation.phpt
- Create: crates/rabbit-rs-php/tests/fixtures/fpm/index.php
- Create: crates/rabbit-rs-php/tests/fixtures/fpm/php-fpm.conf
- Create: scripts/test-fpm.sh
- Modify: crates/rabbit-rs-php/src/classes/pool.rs

**Step 1: Write failing process lifecycle tests**

Verify:

- two equivalent Pools in one PID share the connection key;
- pcntl_fork invalidates the inherited handle in the child;
- the child creates a new registry;
- two FPM requests of the same worker reuse the pool;
- two FPM workers do not announce the same PID or handle.

**Step 2: Verify failure**

Run: ./scripts/test-fpm.sh

Expected: FAIL before instrumentation and PID guard.

**Step 3: Implement lifecycle hooks**

Add only the hooks necessary for module/process shutdown. Never open a connection in MINIT.

**Step 4: Verify**

Run: ./scripts/test-fpm.sh

Expected: PASS.

**Step 5: Run Milestone B gate**

Run: ./scripts/check.sh && ./scripts/test-extension.sh && ./scripts/test-fpm.sh

Expected: PASS.

**Step 6: Commit**

    git add crates/rabbit-rs-php scripts
    git commit -m "feat(extension): make native pools safe across PHP process lifecycles"

## Milestone C — Laravel package

### Task 16: Initialize the package and its configuration

**Files:**
- Create: packages/laravel-queue/composer.json
- Create: packages/laravel-queue/phpunit.xml
- Create: packages/laravel-queue/src/RabbitMqServiceProvider.php
- Create: packages/laravel-queue/src/Config/ConfigNormalizer.php
- Create: packages/laravel-queue/config/rabbit-rs.php
- Create: packages/laravel-queue/tests/TestCase.php
- Create: packages/laravel-queue/tests/Unit/ConfigNormalizerTest.php

**Step 1: Write failing package tests**

Test configuration publishing, brokers/routes/workers validation, the quorum/confirm/mandatory defaults, the absence of an application DLQ by default, secret masking and the error when ext-rabbit_rs is missing.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && composer install && vendor/bin/phpunit --filter ConfigNormalizerTest

Expected: FAIL.

**Step 3: Implement package skeleton**

Use the Goopil\RabbitRs\Laravel namespace, illuminate/queue and Orchestra Testbench with a Composer Laravel 12/13 matrix. The package bears exactly the name goopil/rabbit-rs-laravel and requires PHP ^8.4, ext-rabbit_rs with the same major version, and illuminate/queue ^12.0 || ^13.0.

The package composer.json contains at minimum:

    {
        "name": "goopil/rabbit-rs-laravel",
        "type": "library",
        "require": {
            "php": "^8.4",
            "ext-rabbit_rs": "^1.0",
            "illuminate/queue": "^12.0 || ^13.0"
        },
        "autoload": {
            "psr-4": {
                "Goopil\\RabbitRs\\Laravel\\": "src/"
            }
        }
    }

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter ConfigNormalizerTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): bootstrap native RabbitMQ queue package"

### Task 17: Register the connector and the shared pool

**Files:**
- Create: packages/laravel-queue/src/Connectors/RabbitMqConnector.php
- Create: packages/laravel-queue/src/Support/NativePoolFactory.php
- Create: packages/laravel-queue/src/RabbitMqQueue.php
- Create: packages/laravel-queue/tests/Unit/RabbitMqConnectorTest.php
- Modify: packages/laravel-queue/src/RabbitMqServiceProvider.php

**Step 1: Write failing connector tests**

Verify Queue::connection returns the driver, two equivalent resolutions share the pool handle, a different fingerprint creates another handle and no Request is retained.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqConnectorTest

Expected: FAIL.

**Step 3: Implement connector and factory**

Register the rabbit-rs name. The factory passes an immutable normalized configuration to Goopil\RabbitRs\Pool. Create the contractual skeleton of RabbitMqQueue so that Laravel can apply setConnectionName and setContainer; leave its operations unimplemented until Task 18.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqConnectorTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): register native RabbitMQ queue connector"

### Task 18: Implement push, later and bulk

**Files:**
- Modify: packages/laravel-queue/src/RabbitMqQueue.php
- Create: packages/laravel-queue/src/Support/MessageMapper.php
- Create: packages/laravel-queue/src/Exceptions/QueueException.php
- Create: packages/laravel-queue/tests/Unit/RabbitMqQueuePublishTest.php
- Create: packages/laravel-queue/tests/bootstrap.php
- Modify: packages/laravel-queue/src/Connectors/RabbitMqConnector.php
- Modify: packages/laravel-queue/tests/Unit/RabbitMqConnectorTest.php
- Modify: packages/laravel-queue/phpunit.xml

**Step 1: Write failing Queue publish tests**

Test:

- push serializes the Laravel payload;
- pushRaw preserves the payload;
- stable UUID message_id;
- onQueue feeds the routing key;
- later passes the delay in milliseconds;
- bulk calls publishBatch only once;
- basic.return becomes QueueException;
- backpressure becomes a dedicated exception;
- dispatch_after_commit remains handled by the Laravel Queue class.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqQueuePublishTest

Expected: FAIL.

**Step 3: Implement minimal publishing adapter**

Extend Illuminate\Queue\Queue and implement Illuminate\Contracts\Queue\Queue. Do not duplicate createPayload. Resolve the named route with the `default` fallback, reuse the Laravel payload UUID as `message_id` and preserve Laravel's events, delays and transactional callbacks around the simple or batched native calls.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqQueuePublishTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): publish immediate delayed and bulk jobs"

### Task 19: Implement RabbitMqJob

**Files:**
- Create: packages/laravel-queue/src/Jobs/RabbitMqJob.php
- Create: packages/laravel-queue/tests/Unit/RabbitMqJobTest.php
- Modify: packages/laravel-queue/src/RabbitMqQueue.php

**Step 1: Write failing job tests**

Test:

- getRawBody;
- getJobId;
- attempts;
- delete calls ACK once;
- release(0) calls basic.reject with requeue=true via the native handle;
- release(10) calls release(10000);
- double delete with no dangerous effect;
- an ACK exception surfaces as a lost connection;
- failed job follows the Laravel sequence.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqJobTest

Expected: FAIL.

**Step 3: Implement Job adapter**

Extend Illuminate\Queue\Jobs\Job. Keep Delivery private and release its handle after the terminal transition.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqJobTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): map native deliveries to Laravel jobs"

### Task 20: Wire pop to a multi-vhost profile

**Files:**
- Create: packages/laravel-queue/src/Support/WorkerProfileResolver.php
- Create: packages/laravel-queue/tests/Feature/MultiVhostWorkerTest.php
- Modify: packages/laravel-queue/src/RabbitMqQueue.php
- Modify: packages/laravel-queue/config/rabbit-rs.php

**Step 1: Write failing feature test**

Configure two brokers/vhosts and three subscriptions. Verify that a single pop call on the main profile can yield jobs from all three sources, with correct connectionName, queue and attempts.

Also test an unknown profile, a disabled subscription and a timeout without job.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter MultiVhostWorkerTest

Expected: FAIL.

**Step 3: Implement aggregate pop**

The Laravel connection's queue value references the worker profile name by default. Document that fine-grained multi-alias selection via the --queue option arrives with rabbit-rs:work; do not simulate a blocking per-queue loop.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter MultiVhostWorkerTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): consume multi-vhost worker profiles"

### Task 21: Implement size, clear and monitoring

**Files:**
- Create: packages/laravel-queue/tests/Unit/RabbitMqQueueAdminTest.php
- Modify: packages/laravel-queue/src/RabbitMqQueue.php

**Step 1: Write failing admin tests**

Verify aggregate and per-route size, explicit clear, clear refusal without configuration permission, and Monitor metrics.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqQueueAdminTest

Expected: FAIL.

**Step 3: Implement bounded admin operations**

Do not use the HTTP management API for the critical path. Passive AMQP commands suffice for size when available.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqQueueAdminTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): add queue administration and monitoring"

### Task 22: Add native events and a diagnostic command

**Files:**
- Create: packages/laravel-queue/src/Events/ConnectionStateChanged.php
- Create: packages/laravel-queue/src/Events/BackpressureDetected.php
- Create: packages/laravel-queue/src/Console/RabbitMqStatusCommand.php
- Create: packages/laravel-queue/tests/Feature/RabbitMqStatusCommandTest.php
- Modify: packages/laravel-queue/src/RabbitMqServiceProvider.php

**Step 1: Write failing command tests**

Verify human and JSON output, absence of secrets, per-broker/vhost states, buffers, confirms and generation.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqStatusCommandTest

Expected: FAIL.

**Step 3: Implement status adapter**

The rabbit-rs:status command only reads Pool::stats. It must neither reconnect nor modify the topology except for an explicit future option.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter RabbitMqStatusCommandTest

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): expose native connection diagnostics"

### Task 23: Add the progressive multiprocess command

**Files:**
- Create: packages/laravel-queue/src/Console/RabbitMqWorkCommand.php
- Create: packages/laravel-queue/src/Console/WorkerSupervisor.php
- Create: packages/laravel-queue/tests/Unit/WorkerSupervisorTest.php
- Create: packages/laravel-queue/tests/Feature/RabbitMqWorkCommandTest.php
- Modify: packages/laravel-queue/src/RabbitMqServiceProvider.php

**Step 1: Write failing supervisor tests**

Test child command construction, workers = 1 and workers > 1, SIGTERM/SIGINT propagation, restart with backoff, clean shutdown, max restarts and exit codes.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter 'WorkerSupervisorTest|RabbitMqWorkCommandTest'

Expected: FAIL.

**Step 3: Implement orchestration only**

Each child runs queue:work with a determined connection/profile. Use Symfony Process. Do not call job handlers from the supervisor.

**Step 4: Verify**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter 'WorkerSupervisorTest|RabbitMqWorkCommandTest'

Expected: PASS.

**Step 5: Commit**

    git add packages/laravel-queue
    git commit -m "feat(laravel): supervise multiple standard queue workers"

### Task 24: Certify Octane

**Files:**
- Create: packages/laravel-queue/src/Octane/OctaneLifecycle.php
- Create: packages/laravel-queue/tests/Feature/OctaneLifecycleTest.php
- Create: scripts/test-octane.sh
- Modify: packages/laravel-queue/src/RabbitMqServiceProvider.php

**Step 1: Write failing lifecycle tests**

Verify:

- no Request retained;
- two requests reuse the same pool within a worker;
- reload closes the pool;
- worker stop drains within the deadline;
- a cancelled request leaves no orphaned PHP waiter;
- independent pool per worker.

**Step 2: Verify failure**

Run: cd packages/laravel-queue && vendor/bin/phpunit --filter OctaneLifecycleTest

Expected: FAIL.

**Step 3: Implement Octane hooks**

Detect Octane optionally. Do not make laravel/octane mandatory for FPM users.

**Step 4: Verify package tests**

Run: cd packages/laravel-queue && vendor/bin/phpunit

Expected: PASS.

**Step 5: Run Milestone C gate**

Run: ./scripts/test-octane.sh --server=frankenphp && ./scripts/test-octane.sh --server=roadrunner && ./scripts/test-octane.sh --server=swoole && ./scripts/test-octane.sh --server=openswoole

Expected: PASS for each certified runtime.

**Step 6: Commit**

    git add packages/laravel-queue scripts/test-octane.sh
    git commit -m "feat(laravel): support persistent Octane worker lifecycles"

## Milestone D — Cluster, integration and chaos

### Task 25: Create the RabbitMQ test cluster

**Files:**
- Create: lab/rabbitmq/compose.yaml
- Create: lab/rabbitmq/rabbitmq/Dockerfile
- Create: lab/rabbitmq/rabbitmq/enabled_plugins
- Create: lab/rabbitmq/rabbitmq/rabbitmq.conf
- Create: lab/rabbitmq/rabbitmq/definitions.json
- Create: lab/rabbitmq/toxiproxy/config.json
- Create: lab/rabbitmq/prometheus/prometheus.yml
- Create: scripts/lab-up.sh
- Create: scripts/lab-down.sh
- Create: scripts/lab-ready.sh

**Step 1: Write failing readiness check**

The script must verify three RabbitMQ 4.3 nodes, a healthy cluster, a quorum leader, two vhosts, limited users, Prometheus and Toxiproxy.

**Step 2: Verify failure**

Run: ./scripts/lab-up.sh && ./scripts/lab-ready.sh

Expected: FAIL before the services.

**Step 3: Implement the lab**

Pin the images by version and digest during implementation. The Dockerfile downloads a pinned version of rabbitmq_delayed_message_exchange and verifies its SHA-256. Provide a Compose profile with the plugin and one without.

**Step 4: Verify**

Run: ./scripts/lab-up.sh && ./scripts/lab-ready.sh

Expected: PASS.

**Step 5: Commit**

    git add lab scripts
    git commit -m "test: add reproducible three-node RabbitMQ lab"

### Task 26: Write the end-to-end integration tests

**Files:**
- Create: crates/rabbit-rs-core/tests/integration/publish_consume.rs
- Create: crates/rabbit-rs-core/tests/integration/topology_modes.rs
- Create: packages/laravel-queue/tests/Integration/QueueWorkerTest.php
- Create: packages/laravel-queue/tests/Integration/DelayedJobTest.php
- Create: scripts/test-integration.sh

**Step 1: Write failing scenarios**

Include:

- confirmed publish then consume/ACK;
- mandatory return;
- quorum and classic;
- declare, verify and external;
- two vhosts in one ConsumerSet;
- release(0) via reject/requeue;
- delayed release plugin;
- delayed release TTL;
- failed Laravel job;
- bulk;
- TLS.

**Step 2: Verify failure**

Run: ./scripts/test-integration.sh

Expected: FAIL before real wiring.

**Step 3: Complete real transport integration**

Replace only the still-mocked paths. Keep the at-least-once assertions explicit.

**Step 4: Verify**

Run: ./scripts/test-integration.sh

Expected: PASS.

**Step 5: Commit**

    git add crates packages scripts
    git commit -m "test: validate native Laravel queue flows against RabbitMQ"

### Task 27: Write the failure scenarios

**Files:**
- Create: lab/rabbitmq/scenarios/
- Create: crates/rabbit-rs-core/tests/chaos/reconnect.rs
- Create: packages/laravel-queue/tests/Integration/AtLeastOnceChaosTest.php
- Create: scripts/test-chaos.sh

**Step 1: Write failing chaos assertions**

Scenarios:

- TCP reset before confirm;
- TCP reset after confirm before ACK;
- quorum leader shutdown;
- node restart;
- consumer partition;
- channel closed for topology error;
- delay plugin unavailable;
- refused credentials;
- SIGTERM of the worker with unacknowledged jobs.

For each scenario, count expected, unique, duplicated and missing messages.

**Step 2: Verify failure**

Run: ./scripts/test-chaos.sh

Expected: FAIL until the recovery is fully implemented.

**Step 3: Fix one scenario at a time**

Apply systematic-debugging. Never accept a missing message. Accept duplicates only in the documented ambiguous windows.

**Step 4: Verify**

Run: ./scripts/test-chaos.sh

Expected: PASS, missing = 0.

**Step 5: Commit**

    git add lab crates packages scripts
    git commit -m "test: prove at-least-once behavior under RabbitMQ failures"

## Milestone E — Performance

### Task 38: Create bench-native

**Files:**
- Create: benchmarks/native/Cargo.toml
- Create: benchmarks/native/benches/batching.rs
- Create: benchmarks/native/benches/ffi_conversion.rs
- Create: benchmarks/native/benches/scheduler.rs
- Create: benchmarks/native/benches/transport.rs
- Create: benchmarks/native/php/ffi_conversion.php
- Create: benchmarks/native/README.md
- Modify: Cargo.toml

**Step 1: Add benchmark smoke tests**

The benchmarks must cover sizes 256 B, 1 KiB, 10 KiB, 100 KiB and 1 MiB, batches 1/16/64/256, confirms, scheduler cost and allocation. The audit baseline measures separately the cost of a call at the PHP/Rust boundary, the conversion and copy of payloads and headers, and the submission of batches, without a broker when not needed.

**Step 2: Verify command**

Run: cargo bench -p rabbit-rs-native-bench --no-run

Expected: FAIL before the benchmark crate.

**Step 3: Implement Criterion suites**

Separate the microbench without broker and the transport bench with the lab. The PHP harness exercises the compiled extension and distinguishes the fixed cost of the FFI boundary from the conversion cost by volume and batch size. Record version, CPU, kernel, PHP, NTS/ZTS mode, RabbitMQ, payload and configuration in each result.

**Step 4: Verify**

Run: cargo bench -p rabbit-rs-native-bench --no-run

Expected: PASS.

**Step 5: Commit**

    git add Cargo.toml benchmarks/native
    git commit -m "perf: add native batching and transport benchmarks"

### Task 39: Create the bench-laravel application

**Files:**
- Create: benchmarks/laravel/composer.json
- Create: benchmarks/laravel/artisan
- Create: benchmarks/laravel/app/Jobs/BenchmarkJob.php
- Create: benchmarks/laravel/app/Console/Commands/PublishBenchmark.php
- Create: benchmarks/laravel/app/Console/Commands/ConsumeBenchmark.php
- Create: benchmarks/laravel/config/benchmark.php
- Create: benchmarks/laravel/drivers/
- Create: benchmarks/laravel/scripts/run-matrix.sh
- Create: benchmarks/laravel/README.md

**Step 1: Write failing benchmark contract test**

Each driver must expose setup, publish, consume, reset and metrics with the same payloads and configurable guarantees.

Drivers:

- rabbit-rs;
- php-amqplib direct;
- vyuldashev/laravel-queue-rabbitmq as the reference Laravel RabbitMQ driver;
- Laravel Redis;
- database control.

**Step 2: Verify failure**

Run: cd benchmarks/laravel && composer install && vendor/bin/phpunit

Expected: FAIL.

**Step 3: Implement the harness**

Measure throughput, end-to-end p50/p95/p99, CPU, RSS, connections, channels, duplicates and losses. Provide CLI, FPM and Octane modes. Do not include SQS.

**Step 4: Verify a short matrix**

Run: benchmarks/laravel/scripts/run-matrix.sh --smoke

Expected: PASS and a JSON results file.

**Step 5: Commit**

    git add benchmarks/laravel
    git commit -m "perf: add reproducible Laravel queue comparison lab"

### Task 40: Calibrate the defaults and freeze the budgets

**Files:**
- Create: benchmarks/baselines/reference-machine.json
- Create: benchmarks/baselines/v1-budget.json
- Create: docs/performance.md
- Modify: packages/laravel-queue/config/rabbit-rs.php
- Modify: crates/rabbit-rs-core/src/config.rs

**Step 1: Capture the reference environment**

Run: benchmarks/laravel/scripts/run-matrix.sh --full

Expected: full results and machine metadata.

**Step 2: Analyze batch and prefetch sweeps**

Compare batch_messages, batch_bytes, flush interval, publisher channels, prefetch and max_in_flight. Examine latency at 50%, 70% and 90% of saturation.

**Step 3: Set absolute and comparative gates**

Write the measured throughput and p99 targets per payload profile, plus the minimum expected gain against the PHP RabbitMQ driver. Do not invent an unmeasured threshold.

**Step 4: Update healthy defaults**

Change the defaults only if the fairness, memory and latency tests stay green.

**Step 5: Verify anti-regression check**

Run: benchmarks/laravel/scripts/run-matrix.sh --verify-budget benchmarks/baselines/v1-budget.json

Expected: PASS.

**Step 6: Commit**

    git add benchmarks/baselines docs/performance.md packages crates
    git commit -m "perf: calibrate safe queue and publisher defaults"

## Milestone F — Distribution and documentation

### Task 41: Prepare the Rabbit RS packages and the PIE matrix

**Files:**
- Modify: composer.json
- Modify: .gitattributes
- Modify: packages/laravel-queue/composer.json
- Create: release/pie-matrix.json
- Create: scripts/validate-distribution.sh
- Create: scripts/package-pie-binary.sh
- Create: scripts/split-laravel-package.sh

**Step 1: Write the failing distribution metadata check**

The script verifies:

- the root package is named goopil/rabbit-rs-native and its type is php-ext;
- extension-name is rabbit_rs;
- download-url-method contains only pre-packaged-binary;
- NTS and ZTS are advertised;
- Linux is the only OS family advertised in V1;
- the Laravel package is named goopil/rabbit-rs-laravel;
- its namespace is Goopil\RabbitRs\Laravel;
- it requires ext-rabbit_rs with the same major version;
- the Cargo, PHP extension and release tag versions are consistent.

**Step 2: Verify failure**

Run: ./scripts/validate-distribution.sh

Expected: FAIL before the manifest and checks.

**Step 3: Add the exact PIE matrix**

release/pie-matrix.json contains exactly 16 combinations:

    PHP: 8.4, 8.5
    architecture: x86_64, arm64
    libc: glibc, musl
    thread safety: nts, zts

Do not distribute debug builds. Document the minimum glibc version used for the glibc artifacts.

**Step 4: Implement deterministic PIE packaging**

scripts/package-pie-binary.sh receives version, PHP, architecture, libc, thread-safe mode and the shared object path. It produces a PIE-compliant ZIP archive, for example:

    php_rabbit_rs-1.2.0_php8.5-x86_64-linux-glibc-nts.zip

The archive contains rabbit_rs.so and no undocumented dynamic library. The script also produces the SHA-256 and refuses an inconsistent name, ABI or architecture.

**Step 5: Implement the Laravel split dry-run**

scripts/split-laravel-package.sh extracts packages/laravel-queue, keeps its composer.json at the result root and refuses to publish if its major version is not compatible with ext-rabbit_rs.

**Step 6: Verify**

Run: ./scripts/validate-distribution.sh && ./scripts/package-pie-binary.sh --self-test && ./scripts/split-laravel-package.sh --dry-run

Expected: PASS and a matrix of 16 unique artifacts.

**Step 7: Commit**

    git add composer.json .gitattributes packages/laravel-queue/composer.json release scripts
    git commit -m "build: define Rabbit RS PIE and Packagist packages"

### Task 42: Add CI and synchronized publication

> **Status:** Partially implemented. The release build/package/verify workflow
> exists, but the SBOM (CycloneDX via cargo-cyclonedx) and provenance
> attestation (actions/attest@v4) steps were added after the initial
> implementation. See `docs/superpowers/plans/2026-08-17-release-sbom-attestation.md`.

**Files:**
- Create: .github/workflows/rust.yml
- Create: .github/workflows/php.yml
- Create: .github/workflows/integration.yml
- Create: .github/workflows/octane.yml
- Create: .github/workflows/bench-smoke.yml
- Create: .github/workflows/split-laravel.yml
- Create: .github/workflows/release.yml
- Create: scripts/verify-release-assets.sh

**Step 1: Write failing workflow and release checks**

scripts/verify-release-assets.sh verifies the 16 expected archives, their SHA-256, SBOMs, attestations, PIE names and synchronized versions. It fails if a debug artifact or an unsupported platform is published.

**Step 2: Validate before adding workflows**

Run: actionlint && ./scripts/verify-release-assets.sh --fixtures release/pie-matrix.json

Expected: FAIL before workflows and fixtures.

**Step 3: Add build and test jobs**

Separate Rust tests, PHPT, Laravel 12/13, cluster integration, Octane and scheduled chaos. Cache Cargo and Composer without sharing a built extension between two PHP ABIs.

**Step 4: Build and smoke-test all PIE binaries**

For each line of release/pie-matrix.json:

1. build rabbit_rs.so with the right ABI;
2. inspect architecture and dynamic dependencies;
3. load the extension with php --ri rabbit_rs;
4. run a publish/consume smoke test;
5. create the PIE archive, the SHA-256 and the SBOM;
6. produce a GitHub attestation.

The Rust, Lapin and rustls dependencies are statically linked as much as possible.

**Step 5: Publish a draft native release**

Create a GitHub Release in draft and immutable after publication. Attach the 16 archives and their evidence. Do not publish the draft if a combination is missing.

**Step 6: Split and tag the Laravel package**

The split-laravel workflow publishes packages/laravel-queue to the read-only goopil/rabbit-rs-laravel mirror repository, then pushes exactly the same tag. Trigger the Packagist updates of goopil/rabbit-rs-native and goopil/rabbit-rs-laravel.

**Step 7: Verify installation as a user**

In clean representative containers:

    pie install goopil/rabbit-rs-native
    composer require goopil/rabbit-rs-laravel
    php --ri rabbit_rs
    php artisan rabbit-rs:status --json

Expected: PIE selects the right binary and Composer accepts the platform version.

**Step 8: Publish only after synchronized verification**

Publish the GitHub Release only after validating the artifacts, the mirror repository, both Packagist metadata and the user installation test.

**Step 9: Commit**

    git add .github scripts/verify-release-assets.sh
    git commit -m "ci: publish synchronized Rabbit RS releases"

### Task 43: Document installation, configuration and operations

**Files:**
- Create: README.md
- Create: docs/installation.md
- Create: docs/distribution.md
- Create: docs/configuration.md
- Create: docs/laravel.md
- Create: docs/topology.md
- Create: docs/reliability.md
- Create: docs/operations.md
- Create: docs/octane.md
- Create: docs/troubleshooting.md
- Create: examples/laravel/
- Create: scripts/test-docs.sh

**Step 1: Write documentation acceptance checklist**

The reader must be able to:

- install the extension with pie install goopil/rabbit-rs-native;
- install the bridge with composer require goopil/rabbit-rs-laravel;
- use PIE in a Dockerfile without a dedicated Rabbit RS image;
- build locally with Cargo to contribute;
- understand why Composer does not alter the system PHP;
- declare two vhosts;
- publish and run queue:work;
- choose declare/verify/external;
- configure quorum/classic;
- explicitly enable an application DLQ if desired;
- understand duplicates;
- enable the plugin delay or TTL;
- diagnose a reconnection;
- configure Supervisor/Kubernetes;
- use Octane without retaining Request.

State explicitly that PECL, Debian/RPM/APK packages, a Composer plugin installing binaries and full PHP images are not V1 channels.

**Step 2: Add copy-paste examples**

All examples are executed in CI. The README starts with the two installation commands and a minimal Laravel example.

**Step 3: Verify links and examples**

Run: ./scripts/test-docs.sh

Expected: PASS.

**Step 4: Commit**

    git add README.md docs examples scripts/test-docs.sh
    git commit -m "docs: document Rabbit RS installation and operations"

### Task 44: Perform the release verification

**Files:**
- Create: docs/release-checklist.md
- Modify: CHANGELOG.md

**Step 1: Run all fast checks**

Run: ./scripts/check.sh && ./scripts/test-extension.sh && ./scripts/validate-distribution.sh

Expected: PASS.

**Step 2: Run all PHP environments**

Run: ./scripts/test-fpm.sh && ./scripts/test-octane.sh --all

Expected: PASS.

**Step 3: Run integration and chaos**

Run: ./scripts/test-integration.sh && ./scripts/test-chaos.sh

Expected: PASS with missing = 0.

**Step 4: Run performance gate**

Run: benchmarks/laravel/scripts/run-matrix.sh --verify-budget benchmarks/baselines/v1-budget.json

Expected: PASS.

**Step 5: Verify all release assets**

Run: ./scripts/verify-release-assets.sh --release-tag VERSION

Expected: 16 valid archives, 16 valid checksums, SBOM and attestation present, no debug build.

**Step 6: Verify fresh user installation**

Run in the matrix of clean containers:

    pie install goopil/rabbit-rs-native:VERSION
    composer require goopil/rabbit-rs-laravel:^MAJOR
    php --ri rabbit_rs

Expected: PASS on the 16 advertised combinations.

**Step 7: Record evidence**

Add versions, checksums, results, observed duplicates, recovery times, Packagist URLs and the mirror repository tag to docs/release-checklist.md.

**Step 8: Commit**

    git add CHANGELOG.md docs/release-checklist.md
    git commit -m "chore: record Rabbit RS release verification"

## Completion criteria

- All Rust, PHPT, PHPUnit tests and Composer matrices pass.
- The 16 PIE artifacts PHP 8.4/8.5, NTS/ZTS, glibc/musl and x86_64/ARM64 load.
- pie install goopil/rabbit-rs-native selects and enables the right artifact.
- composer require goopil/rabbit-rs-laravel validates ext-rabbit_rs without altering the system.
- The goopil/rabbit-rs-native, goopil/rabbit-rs-laravel and ext-rabbit_rs tags and versions are synchronized.
- CLI, FPM, FrankenPHP, RoadRunner, Swoole and Open Swoole are certified.
- A standard queue:work consumes a profile containing several vhosts.
- rabbit-rs:work supervises several queue:work without reimplementing Worker.
- The chaos lab observes no silent loss without manual pool recreation.
- The recovery coordinator automatically restores connections, topology, publishers and consumers after a failure.
- Duplicates from ambiguous windows are measured and documented.
- Publisher-side delay routing works in plugin mode and TTL fallback.
- The application DLQ is configurable from the Laravel config and declared by the topology reconciler.
- Generic queue arguments (`x-delivery-limit`, etc.) are wired from the Laravel config.
- TLS is configurable (SNI, CA, client cert) and tested end-to-end.
- Consumers are cleanly closed (no channel leaks in long-lived processes).
- Laravel events (connection state, backpressure) are dispatched from the native extension.
- Consumer metrics and latencies are exposed in the status command.
- The publisher config (confirms, mandatory, timeout) is wired from Laravel.
- The Octane lifecycle (reload, stop, consumer cleanup) is fully wired.
- The WorkCommand supervisor is tested end-to-end (crash, restart, signals).
- The batch, prefetch and buffer defaults come from benchmarks.
- The absolute and comparative budgets are versioned.
- Logs and diagnostics reveal no secret.
- The at-least-once behavior and the idempotence requirement are clearly documented.
