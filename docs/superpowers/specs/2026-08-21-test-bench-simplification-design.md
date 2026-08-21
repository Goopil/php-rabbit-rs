# Test & Benchmark Simplification Design

**Date:** August 21, 2026
**Status:** Approved
**Supersedes:** `2026-08-18-benchmark-simplification-design.md` (partially — extends its benchmark decisions to cover Rust Criterion removal and test consolidation)

---

## Problem

The project carries three layers of test and benchmark complexity that are disproportionate to what they verify:

1. **Rust integration tests** — 17 files totaling ~5,300 lines in `crates/rabbit-rs-core/tests/`, many with overlapping setup and redundant coverage. The user finds them excessively long and loaded for what they actually verify.

2. **Dual PHP test format** — 11 PHPT files in `crates/rabbit-rs-php/tests/phpt/` duplicate what could be expressed more maintainably in a modern PHP test framework. The PHPT format lacks data providers, parallelism, and expressive assertions.

3. **Criterion microbenchmarks** — 7 Rust Criterion benchmark files (~820 lines) in `crates/rabbit-rs-core/benches/` measure subsystem internals (confirm ledger data structures, FFI conversion, scheduler logic) that are not directly exploitable. What matters is user-facing performance at the PHP level.

The user wants to focus on PHP-level integration and e2e testing — how the library and its integration behave from PHP's perspective — and wants benchmarks that are practical and comparable across PHP RabbitMQ drivers.

## Decision

### 1. Consolidate Rust integration tests: 17 → 6 files

Merge the 17 test files into 6 domain-focused files, eliminating redundant setup and overlapping coverage. All patterns (mock transport, paused Tokio time, integration feature gate) are preserved.

### 2. Migrate PHPT → Pest PHP tests

Replace the 11 PHPT files with a Pest test suite in `crates/rabbit-rs-php/tests/`. Pest provides expressive DSL, native `--parallel` support, and data providers. One PHPT file (`extension_metadata.phpt`) is retained for ZTS-level extension loading verification.

### 3. Migrate Laravel PHPUnit → Pest

Migrate the 26 PHPUnit test files in `packages/laravel-queue/tests/` to Pest. Unit and Feature suites run in parallel via `--parallel`. Integration suite stays sequential (real broker, shared queues).

### 4. Decommission all Criterion benchmarks

Remove all 7 Criterion bench files, the `criterion` dev-dependency, all `[[bench]]` Cargo.toml entries, the `bench` feature, and `[profile.bench]`. Rust performance is validated through the PHP benchmark suite.

### 5. Restructure PHP benchmarks with AbstractBenchmark pattern

Adopt the `AbstractBenchmark` pattern from the reference repo (Goopil/php-ext-rabbit-rs). Four drivers (RabbitRs, php-amqplib, amqp-ext, Bunny) across three scenarios (fire-and-forget, batch-confirm, auto-ack). Laravel benchmarks included.

---

## Architecture

### Part 1: Rust Integration Test Consolidation

#### Current state (17 files, ~5,300 lines)

```
crates/rabbit-rs-core/tests/
├── client_pool.rs              (645 lines, 16 tests)
├── consumer_semantics.rs       (695 lines, 18 tests)
├── consumer_cleanup.rs         (230 lines, 6 tests)
├── publisher_safety.rs         (466 lines, 16 tests)
├── publisher_recovery.rs       (477 lines, 13 tests)
├── publisher_delay.rs          (427 lines, 8 tests)
├── delay_routing.rs            (190 lines, 8 tests)
├── recovery_coordinator.rs     (354 lines, 5 tests)
├── recovery_state_machine.rs   (297 lines, 7 tests)
├── topology_recovery.rs        (291 lines, 10 tests)
├── topology_modes.rs            (157 lines, 4 tests, integration-gated)
├── dlq_topology.rs             (377 lines, 11 tests)
├── delivery_attempts.rs        (293 lines, 9 tests)
├── metrics_snapshot.rs         (351 lines, 5 tests)
├── publish_consume.rs          (298 lines, 4 tests, integration-gated)
├── scheduler_fairness.rs        (128 lines, 6 tests)
├── tls.rs                      (229 lines, 9 tests)
└── chaos/
    ├── reconnect.rs            (944 lines, 9 tests, integration-gated)
    └── toxiproxy.rs            (187 lines, helper)
```

#### Target state (6 files + chaos, ~2,200 lines)

```
crates/rabbit-rs-core/tests/
├── publisher.rs                (~500 lines)  — publisher_safety + publisher_recovery + publisher_delay + delay_routing
├── consumer.rs                 (~400 lines)  — consumer_semantics + consumer_cleanup
├── recovery.rs                 (~300 lines)  — recovery_coordinator + recovery_state_machine
├── topology.rs                 (~400 lines)  — topology_recovery + topology_modes + dlq_topology + delivery_attempts
├── metrics.rs                  (~200 lines)  — metrics_snapshot (renamed)
├── integration.rs              (~400 lines)  — publish_consume (integration-gated) + scheduler_fairness + client_pool (pruned)
└── chaos/
    ├── reconnect.rs            (unchanged, 944 lines, integration-gated)
    └── toxiproxy.rs            (unchanged, 187 lines, helper)
```

#### Consolidation details

**`publisher.rs`** — merges `publisher_safety.rs`, `publisher_recovery.rs`, `publisher_delay.rs`, `delay_routing.rs`:
- Batching flush triggers (count, bytes, timer) — from publisher_safety
- ACK/NACK resolution, mandatory return precedence — from publisher_safety
- Confirmation timeout typing, backpressure on full buffer — from publisher_safety
- Connection-loss replay, generation-aware replay, batch retention — from publisher_recovery
- Late-confirm rejection, NACK terminal-after-replay — from publisher_recovery
- Deadline expiry during outage, permanent failure — from publisher_recovery
- Delay plugin mode (delayed exchange + x-delay header) — from publisher_delay
- TTL bucket mode (lazy queue, dead-letter) — from publisher_delay
- DelayRouter: auto-selects plugin, TTL fallback — from delay_routing
- Config-based confirm timeout, confirms/mandatory flag toggling — from publisher_safety

Shared `MockTransport` setup extracted to a `pub mod helper` at the top of the file. Tests that exercised the same setup in 4 separate files now share it.

**`consumer.rs`** — merges `consumer_semantics.rs`, `consumer_cleanup.rs`:
- Consumer multiplexing, prefetch enforcement, priority scheduling — from consumer_semantics
- ACK/reject/release semantics, stale-generation ACK rejection — from consumer_semantics
- Bounded source errors, delayed release republishing — from consumer_semantics
- Consumer tag naming, close behavior — from consumer_semantics
- Drop closes subscription channels, multi-subscription cleanup — from consumer_cleanup
- No double-close after explicit close, single close across clones — from consumer_cleanup
- Next-after-drop returns typed Closed error — from consumer_cleanup
- Drop with pending delivery still closes — from consumer_cleanup

**`recovery.rs`** — merges `recovery_coordinator.rs`, `recovery_state_machine.rs`:
- Connection state transitions (Disconnected→Connecting→Ready) — from recovery_state_machine
- Exponential backoff (100/200/400ms, capped 30s), jitter — from recovery_state_machine
- Authentication failure permanence, ready-connection loss — from recovery_state_machine
- Close interrupting backoff, generation increment — from recovery_state_machine
- Publisher replay after recovery via RecoveryCoordinator — from recovery_coordinator
- Consumer generation updates, deterministic recovery order — from recovery_coordinator
- Loss-during-recovery cancellation, permanent error stops loop — from recovery_coordinator

**`topology.rs`** — merges `topology_recovery.rs`, `topology_modes.rs`, `dlq_topology.rs`, `delivery_attempts.rs`:
- Quorum-by-default queue kind, quorum rejects exclusive/auto-delete — from topology_recovery
- Declare ordering (exchanges→queues→bindings), idempotency — from topology_recovery
- Verify passive checks, external mode no-op — from topology_recovery
- Dead-letter topology, topology incompatibility — from topology_recovery
- New-generation full replay — from topology_recovery
- Declare quorum/classic queue, passive verify (integration-gated) — from topology_modes
- Dead-letter config validation, DLX/DLQ/binding compilation — from dlq_topology
- Reconciler declaration of DLX topology, delivery_limit — from dlq_topology
- Generic queue argument preservation, disabled DL config — from dlq_topology
- AttemptsResolver: first acquisition, acquired-count precedence — from delivery_attempts
- Quorum delivery-count conversion, application count survival — from delivery_attempts
- Max-attempts typed error, default limit (20) — from delivery_attempts
- Classic-queue redelivery fallback, broker message_id preservation — from delivery_attempts
- Synthetic id fallback, delayed release incrementing — from delivery_attempts

**`metrics.rs`** — renamed from `metrics_snapshot.rs`, unchanged content:
- Publisher metrics (publishes, confirmations, returns, backpressure, latency) — from metrics_snapshot
- NACK/unconfirmed classification, consumer metrics — from metrics_snapshot
- Connection reconnect counting, snapshot secret redaction — from metrics_snapshot
- Synchronous snapshot, concurrent snapshots — from metrics_snapshot

**`integration.rs`** — merges `publish_consume.rs`, `scheduler_fairness.rs`, pruned `client_pool.rs`:
- Publish-confirm-consume-ACK, release-zero requeue (integration-gated) — from publish_consume
- Two-vhost consumer set, bulk publish then consume (integration-gated) — from publish_consume
- WeightedFairScheduler: single selection, weight ratio — from scheduler_fairness
- Empty subscription no credit accumulation, aging prevents starvation — from scheduler_fairness
- Connection reuse/deduplication, batch publish — from client_pool (pruned to ~8 tests)
- Consumer profile creation, queue size/purge — from client_pool (pruned)
- Close-during-connect, parallel broker initialization — from client_pool (pruned)
- Connection state reporting — from client_pool (pruned)

#### What is deleted entirely

- **`tls.rs`** (229 lines) — pure config tests. Migrates to inline `#[cfg(test)]` in `src/config.rs` where TLS config parsing already has partial coverage.
- **`client_pool.rs`** — 16 tests pruned to ~8 most representative. Tests covering edge cases already covered by publisher/consumer/recovery tests are dropped.

#### Cargo.toml changes

```toml
# REMOVED from [dev-dependencies]:
# criterion = { version = "0.5", features = ["html_reports"] }

# REMOVED: all 7 [[bench]] sections
# REMOVED: bench feature from [features]
# REMOVED: [profile.bench] from workspace Cargo.toml
# KEPT: [[test]] chaos_reconnect entry (unchanged)
```

#### Source code changes

- Remove `publish_properties_bench` pub fn from `src/transport/lapin.rs` (gated behind `#[cfg(feature = "bench")]`)
- The `bench` feature in `Cargo.toml` `[features]` is removed

#### Impact summary

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Test files | 17 + 2 chaos | 6 + 2 chaos | -11 |
| Test lines | ~5,300 | ~2,200 | -3,100 |
| Bench files | 7 | 0 | -7 |
| Bench lines | 820 | 0 | -820 |
| Cargo.toml bench config | 15 lines | 0 | -15 |

---

### Part 2: PHPT → Pest Migration

#### Target structure

```
crates/rabbit-rs-php/
  tests/
    Pest.php                      — Pest bootstrap, extension-loaded guard
    Extension/
      ExtensionTest.php           — extension loaded, version, exception hierarchy
      SecretsTest.php             — credential leak prevention
    Pool/
      PoolRegistryTest.php        — handle reuse, distinct vhosts, close invalidation
      ForkInvalidationTest.php     — pcntl_fork handle isolation (@group isolation)
    Publisher/
      PublisherOutcomesTest.php    — ack, returned, pending, transport_error scenarios
      BackpressureTest.php         — bounded saturation, BackpressureException
      BoundaryLimitsTest.php       — batch/payload/header/timeout limits (dataset-driven)
    Consumer/
      ConsumerTest.php             — delivery metadata, ACK/reject/release terminal states
    Config/
      ConfigValidationTest.php     — zero prefetch, legacy max_in_flight, recursive config
    Reflection/
      ReflectionTest.php           — class/method/parameter reflection contract
    BinaryPayload/
      BinaryPayloadTest.php        — NUL bytes, oversized, resource/object headers
    phpt/
      extension_metadata.phpt      — RETAINED: ZTS-level extension loading verification
  composer.json                    — adds pestphp/pest dev-dependency
  phpunit.xml                      — removed, replaced by Pest.php
```

#### Test base: Pest.php

```php
<?php
// tests/Pest.php

uses(\PHPUnit\Framework\TestCase::class)->in(__DIR__);

beforeAll(function () {
    if (!extension_loaded('rabbit_rs')) {
        test('extension loaded', fn () => true)->markTestSkipped(
            'rabbit_rs extension not loaded'
        );
        return;
    }
});

function testingPool(array $config, array $scenario): \Goopil\RabbitRs\Pool
{
    return \Goopil\RabbitRs\testing_pool($config, $scenario);
}

function defaultConfig(): array
{
    return [
        'brokers' => [
            'testing' => [
                'uri' => 'amqp://guest:guest@localhost:5672/testing',
                'vhosts' => ['/'],
            ],
        ],
        'publishers' => [
            'testing' => ['broker' => 'testing'],
        ],
        'consumers' => [
            'testing' => ['broker' => 'testing', 'queue' => 'test'],
        ],
    ];
}
```

#### PHPT → Pest mapping

| PHPT source | Pest destination | Key adaptations |
|---|---|---|
| `extension_metadata.phpt` | `ExtensionTest.php` | `extension_loaded()` + `assertEquals(VERSION)`. ZTS check stays in PHPT. |
| `secrets.phpt` | `SecretsTest.php` | `expectException()` on config with credentials, assertion on message |
| `pid_registry.phpt` | `PoolRegistryTest.php` | `testingPool()` for handles, assertions on `Pool::stats()` |
| `fork_invalidation.phpt` | `ForkInvalidationTest.php` | `pcntl_fork()` + assertions. `->group('isolation')` |
| `publisher_outcomes.phpt` | `PublisherOutcomesTest.php` | `testingPool()` with `publication_outcomes` scenario, datasets for ack/returned/pending/transport_error |
| `backpressure.phpt` | `BackpressureTest.php` | `testingPool()` with `publisher_capacity=1, pending_confirmations=1` |
| `boundary_limits.phpt` | `BoundaryLimitsTest.php` | Pest datasets: batch 256, payload 1MiB, headers 128, timeout range |
| `delivery_terminal_state.phpt` | `ConsumerTest.php` | `testingPool()` with 4 deliveries, assertions on metadata + terminal ACK |
| `config_validation.phpt` | `ConfigValidationTest.php` | Pool stats, zero prefetch, legacy, recursive config, close idempotency |
| `reflection.phpt` | `ReflectionTest.php` | `ReflectionClass` on Pool/Consumer/Delivery, method signatures |
| `binary_payload.phpt` | `BinaryPayloadTest.php` | NUL bytes, oversized, resource/object headers, recursive headers |

#### Pest test example (PublisherOutcomesTest.php)

```php
<?php

use Goopil\RabbitRs\Pool;
use Goopil\RabbitRs\BackpressureException;
use Goopil\RabbitRs\ConnectionException;

describe('publisher outcomes', function () {
    dataset('outcomes', [
        'ack' => ['ack', null, 'confirmed'],
        'returned' => ['returned', ConnectionException::class, '312'],
        'pending' => ['pending', ConnectionException::class, 'timeout'],
        'transport_error' => ['transport_error', ConnectionException::class, 'transport'],
    ]);

    it('resolves each outcome correctly', function (string $outcome, ?string $exception, string $expectation) {
        $pool = testingPool(defaultConfig(), [
            'publication_outcomes' => [$outcome],
        ]);

        if ($exception) {
            expect(fn () => $pool->publish(pubMessage()))
                ->toThrow($exception);
        } else {
            $id = $pool->publish(pubMessage());
            expect($id)->toBeString();
        }
    })->with('outcomes');
});
```

#### composer.json changes (crates/rabbit-rs-php/)

```json
{
    "require-dev": {
        "pestphp/pest": "^3.0"
    },
    "config": {
        "allow-plugins": {
            "pestphp/pest-plugin": true
        }
    }
}
```

#### What stays in PHPT

- `extension_metadata.phpt` — verifies extension loading at the Zend engine level (ZTS, final class checks, exception hierarchy). This is the natural domain of `run-tests.php`. One file, kept in `tests/phpt/`.

#### scripts/test-extension.sh changes

The script currently:
1. Builds the extension with `--features extension-tests`
2. Runs all `.phpt` files via `php run-tests.php`

Updated:
1. Builds the extension (unchanged)
2. Runs `composer install` in `crates/rabbit-rs-php/` if needed
3. Runs `vendor/bin/pest --parallel` for Pest tests
4. Runs `php run-tests.php tests/phpt/extension_metadata.phpt` for the single PHPT

---

### Part 3: Laravel PHPUnit → Pest Migration

#### Target structure

```
packages/laravel-queue/
  tests/
    Pest.php                          — Pest bootstrap, Testbench integration
    Unit/
      ConfigNormalizerTest.php        — Pest, from PHPUnit
      RabbitMqQueuePublishTest.php     — Pest
      RabbitMqJobTest.php              — Pest
      WorkerSupervisorTest.php         — Pest
      RabbitMqQueueCleanupTest.php     — Pest
      RabbitMqQueueAdminTest.php       — Pest
      RabbitMqConnectorTest.php        — Pest
      RabbitMqServiceProviderTest.php   — Pest
      MessageMapperTest.php            — Pest
      RabbitMqQueuePopTest.php         — Pest
      WorkerProfileResolverTest.php    — Pest
    Feature/
      MultiVhostWorkerTest.php         — Pest
      RabbitMqWorkCommandTest.php      — Pest
      WorkerSupervisorIntegrationTest.php — Pest
      OctaneLifecycleTest.php          — Pest
      NativeEventDispatchTest.php      — Pest
      OctaneLifecycleHooksTest.php     — Pest
      RabbitMqStatusCommandTest.php    — Pest
    Integration/
      AtLeastOnceChaosTest.php         — Pest (sequential, real broker)
      QueueWorkerTest.php              — Pest (sequential, real broker)
      DelayedJobTest.php              — Pest (sequential, real broker)
    Fixture/
      worker_stub.php                  — unchanged
      worker_stub_functions.php         — unchanged
  composer.json                        — pestphp/pest replaces phpunit/phpunit
  phpunit.xml                          — removed, replaced by Pest.php
```

#### Parallelism

| Suite | Execution | Command |
|------|-----------|---------|
| Unit (12 files) | Parallel | `vendor/bin/pest tests/Unit --parallel` |
| Feature (7 files) | Parallel | `vendor/bin/pest tests/Feature --parallel` |
| Integration (3 files) | Sequential | `vendor/bin/pest tests/Integration` |
| `@group isolation` | Sequential | `vendor/bin/pest --group isolation` |

#### composer.json changes (packages/laravel-queue/)

```json
{
    "require-dev": {
        "orchestra/testbench": "^10.0 || ^11.0",
        "pestphp/pest": "^3.0",
        "pestphp/pest-plugin-laravel": "^3.0"
    },
    "config": {
        "allow-plugins": {
            "pestphp/pest-plugin": true
        }
    }
}
```

Removes `phpunit/phpunit` from `require-dev`.

#### Pest.php bootstrap

```php
<?php
// tests/Pest.php

uses(\Orchestra\Testbench\Concerns\WithTestbench::class)->in(__DIR__);
uses(\Illuminate\Foundation\Testing\TestCase::class)->in(__DIR__);
```

#### Migration approach

Each PHPUnit test class is converted to Pest's `describe/it` syntax:
- `testFoo` → `it('foo', ...)`
- `setUp`/`tearDown` → `beforeEach`/`afterEach`
- `@dataProvider` → `dataset()`
- `$this->` → `expect()` / function calls
- Test class hierarchy → Pest `uses()` trait binding

No test logic is simplified during migration — the focus is on format conversion + parallelism, not on reducing test count.

---

### Part 4: Decommission Criterion Benchmarks

#### Files deleted

| File | Lines | Reason |
|------|-------|--------|
| `benches/batching.rs` | 136 | Covered by PHP `batch-confirm` benchmark |
| `benches/confirm_ledger.rs` | 88 | Microbenchmark of data structure internals |
| `benches/ffi_conversion.rs` | 184 | FFI conversion covered indirectly by PHP benchmarks |
| `benches/lapin_properties.rs` | 58 | Lapin property conversion not visible at PHP level |
| `benches/publisher_actor.rs` | 103 | Covered by PHP `batch-confirm` benchmark |
| `benches/scheduler.rs` | 131 | Covered by PHP `auto-ack` benchmark |
| `benches/transport.rs` | 120 | Covered by PHP `fire-and-forget` benchmark |

#### Cargo.toml changes (crates/rabbit-rs-core/)

Removed:
- `criterion` from `[dev-dependencies]`
- All 7 `[[bench]]` sections
- `bench` from `[features]`

#### Workspace Cargo.toml changes

Removed:
- `[profile.bench]` section (if present)

#### Source code changes

- `src/transport/lapin.rs`: remove `publish_properties_bench` pub fn (gated by `#[cfg(feature = "bench")]`)

---

### Part 5: PHP Benchmark Restructuring

#### Architecture (inspired by Goopil/php-ext-rabbit-rs reference repo)

```
benchmarks/
  src/
    AbstractBenchmark.php           — base class: setUp/tearDown/publish/consume + stats
    Config.php                       — centralized constants (host, port, message count, etc.)
    Driver.php                       — interface (existing, unchanged)
    Drivers/
      RabbitRsDriver.php             — uses Goopil\RabbitRs\Pool API
      AmqplibDriver.php              — php-amqplib/php-amqplib
      AmqpExtDriver.php               — pecl amqp extension
      BunnyDriver.php                 — NEW: bunny/bunny driver
    Scenarios/
      FireAndForgetBenchmark.php     — no confirms, no mandatory
      BatchConfirmBenchmark.php      — confirmSelectMode + waitForConfirms
      AutoAckBenchmark.php            — no_ack=true consumer
    Budget.php                      — anti-regression budget checker
    run-benchmarks.php               — unified runner with --scenario= and --driver= filters
  laravel/
    LaravelSmokeBenchmark.php         — 100 msgs through RabbitMqQueue::push()/pop()
    LaravelCompareBenchmark.php       — rabbit-rs vs php-amqplib vs vyuldashev through Laravel
  baselines/
    smoke-budget.json                 — anti-regression thresholds (unchanged)
  docker-compose.yml                  — single-node RabbitMQ for benchmarks
  composer.json                       — php-amqplib + bunny + laravel deps
  run-benchmarks.sh                   — wrapper: docker-compose up + composer install + run
  results/
    .gitkeep                          — output directory for JSON results
```

#### AbstractBenchmark pattern

```php
<?php
// src/AbstractBenchmark.php

abstract class AbstractBenchmark
{
    abstract public function getName(): string;
    abstract public function setUp(): void;
    abstract public function tearDown(): void;
    abstract public function publishMessages(int $count): void;
    abstract public function consumeMessages(int $count): void;

    public function runBenchmark(): array
    {
        $results = [];
        for ($i = 0; $i < Config::BENCHMARK_ROUNDS; $i++) {
            $start = microtime(true);
            $this->publishMessages(Config::MESSAGE_COUNT);
            $publishTime = microtime(true) - $start;

            $start = microtime(true);
            $this->consumeMessages(Config::MESSAGE_COUNT);
            $consumeTime = microtime(true) - $start;

            $results[] = [
                'publish_time' => $publishTime,
                'consume_time' => $consumeTime,
                'publish_rate' => Config::MESSAGE_COUNT / $publishTime,
                'consume_rate' => Config::MESSAGE_COUNT / $consumeTime,
            ];
        }
        return $this->calculateStats($results);
    }

    private function calculateStats(array $results): array
    {
        return [
            'name' => $this->getName(),
            'publish' => $this->statsColumn($results, 'publish_time', 'publish_rate'),
            'consume' => $this->statsColumn($results, 'consume_time', 'consume_rate'),
        ];
    }
}
```

#### Config

```php
<?php
// src/Config.php

class Config
{
    public const RABBITMQ_HOST = '127.0.0.1';
    public const RABBITMQ_PORT = 5672;
    public const RABBITMQ_USER = 'guest';
    public const RABBITMQ_PASSWORD = 'guest';
    public const RABBITMQ_VHOST = '/';

    public const MESSAGE_COUNT = 10000;
    public const BENCHMARK_ROUNDS = 10;
    public const MESSAGE_PAYLOAD_BYTES = 256;
    public const PREFETCH_COUNT = 500;

    public const EXCHANGE_NAME = 'benchmark_exchange';
    public const EXCHANGE_TYPE = 'direct';
    public const QUEUE_NAME = 'benchmark_queue';
    public const ROUTING_KEY = 'benchmark.key';
}
```

#### Scenarios

| Scenario | Publisher | Consumer | RabbitRs adaptation |
|----------|-----------|----------|-------------------|
| `fire-and-forget` | No confirms | ACK manual | `Pool::publish()` with 100ms timeout |
| `batch-confirm` | Batch + waitForConfirms | ACK manual | `Pool::publishBatch()` + flush |
| `auto-ack` | No confirms | no_ack=true | `Consumer::next()` with auto-ack |

Each scenario is a class extending `AbstractBenchmark`. The RabbitRs driver adapts the `Pool`/`Consumer` API to the scenario's contract. Other drivers use their native APIs.

#### Runner (run-benchmarks.php)

```php
<?php
// Filters: --scenario=<name> --driver=<name>
// Auto-detects available drivers (extension_loaded, class_exists)
// Skips unavailable drivers, continues with remaining
// Outputs: comparison table + JSON results in results/
```

#### Docker Compose (single-node, lightweight)

```yaml
services:
  rabbitmq:
    image: rabbitmq:3.13-management
    ports: ["5672:5672", "15672:15672"]
    environment:
      RABBITMQ_DEFAULT_USER: guest
      RABBITMQ_DEFAULT_PASS: guest
    healthcheck:
      test: ["CMD", "rabbitmq-diagnostics", "-q", "ping"]
      interval: 2s
      timeout: 2s
      retries: 30
```

#### Budget system (unchanged)

`baselines/smoke-budget.json` remains the CI anti-regression threshold. The runner checks results against the budget, exit 1 on regression.

#### Existing benchmark files replaced

| Current file | Disposition |
|---|---|
| `benchmarks/smoke.php` | Replaced by `run-benchmarks.php --scenario=fire-and-forget --driver=rabbit-rs` |
| `benchmarks/compare.php` | Replaced by `run-benchmarks.php` (all drivers, all scenarios) |
| `benchmarks/laravel-smoke.php` | Replaced by `laravel/LaravelSmokeBenchmark.php` |
| `benchmarks/laravel-compare.php` | Replaced by `laravel/LaravelCompareBenchmark.php` |
| `benchmarks/drivers/Driver.php` | Moved to `src/Driver.php` |
| `benchmarks/drivers/RabbitRsDriver.php` | Moved to `src/Drivers/RabbitRsDriver.php`, adapted to AbstractBenchmark |
| `benchmarks/drivers/PhpAmqplibDriver.php` | Moved to `src/Drivers/AmqplibDriver.php`, adapted |
| `benchmarks/drivers/AmqpExtDriver.php` | Moved to `src/Drivers/AmqpExtDriver.php`, adapted |
| `benchmarks/lib/Metrics.php` | Merged into AbstractBenchmark |
| `benchmarks/lib/Budget.php` | Moved to `src/Budget.php` |
| `benchmarks/composer.json` | Updated: add `bunny/bunny`, restructure autoload |
| `benchmarks/README.md` | Updated for new architecture |

#### composer.json (benchmarks/)

```json
{
    "name": "rabbit-rs/benchmarks",
    "require": {
        "php": ">=8.4",
        "php-amqplib/php-amqplib": "^3.7",
        "bunny/bunny": "^0.5",
        "vladimir-yuldashev/laravel-queue-rabbitmq": "^13.0",
        "illuminate/container": "^12.0",
        "illuminate/events": "^12.0",
        "illuminate/queue": "^12.0",
        "illuminate/config": "^12.0"
    },
    "repositories": [
        {
            "type": "path",
            "url": "../packages/laravel-queue",
            "options": {"symlink": true}
        }
    ],
    "autoload": {
        "psr-4": {
            "Bench\\": "src/"
        }
    }
}
```

---

## CI Changes

### .github/workflows/ci.yml

| Job | Before | After |
|-----|--------|-------|
| `rust` | `cargo test --workspace --all-targets` | Unchanged (benches removed from targets automatically) |
| `php` | `phpunit --testsuite="Rabbit RS Laravel"` | `vendor/bin/pest tests/Unit tests/Feature --parallel` |
| `phpt` | `./scripts/test-extension.sh` (PHPT only) | `./scripts/test-extension.sh` (Pest + 1 PHPT) |
| `integration` | `cargo test --features integration` + `smoke.php` + `laravel-smoke.php` | `cargo test --features integration` + `php benchmarks/src/run-benchmarks.php --scenario=fire-and-forget --driver=rabbit-rs` |
| `chaos` | Unchanged | Unchanged |

### scripts/check.sh

Unchanged — `cargo test --workspace --all-targets` covers the consolidated tests. No `cargo bench` was ever in check.sh.

### scripts/test-extension.sh

Updated to run `vendor/bin/pest --parallel` then `php run-tests.php tests/phpt/extension_metadata.phpt`.

### scripts/test-integration.sh

Updated benchmark invocation from `php benchmarks/smoke.php` to `php benchmarks/src/run-benchmarks.php --scenario=fire-and-forget --driver=rabbit-rs`.

---

## Files Summary

### Deleted

| Path | Lines | Reason |
|------|-------|--------|
| `crates/rabbit-rs-core/benches/*.rs` (7 files) | 820 | Criterion decommissioned |
| `crates/rabbit-rs-core/tests/client_pool.rs` | 645 | Pruned into integration.rs |
| `crates/rabbit-rs-core/tests/consumer_semantics.rs` | 695 | Merged into consumer.rs |
| `crates/rabbit-rs-core/tests/consumer_cleanup.rs` | 230 | Merged into consumer.rs |
| `crates/rabbit-rs-core/tests/publisher_safety.rs` | 466 | Merged into publisher.rs |
| `crates/rabbit-rs-core/tests/publisher_recovery.rs` | 477 | Merged into publisher.rs |
| `crates/rabbit-rs-core/tests/publisher_delay.rs` | 427 | Merged into publisher.rs |
| `crates/rabbit-rs-core/tests/delay_routing.rs` | 190 | Merged into publisher.rs |
| `crates/rabbit-rs-core/tests/recovery_coordinator.rs` | 354 | Merged into recovery.rs |
| `crates/rabbit-rs-core/tests/recovery_state_machine.rs` | 297 | Merged into recovery.rs |
| `crates/rabbit-rs-core/tests/topology_recovery.rs` | 291 | Merged into topology.rs |
| `crates/rabbit-rs-core/tests/topology_modes.rs` | 157 | Merged into topology.rs |
| `crates/rabbit-rs-core/tests/dlq_topology.rs` | 377 | Merged into topology.rs |
| `crates/rabbit-rs-core/tests/delivery_attempts.rs` | 293 | Merged into topology.rs |
| `crates/rabbit-rs-core/tests/metrics_snapshot.rs` | 351 | Renamed to metrics.rs |
| `crates/rabbit-rs-core/tests/publish_consume.rs` | 298 | Merged into integration.rs |
| `crates/rabbit-rs-core/tests/scheduler_fairness.rs` | 128 | Merged into integration.rs |
| `crates/rabbit-rs-core/tests/tls.rs` | 229 | Migrated to src/config.rs inline tests |
| `crates/rabbit-rs-php/tests/phpt/*.phpt` (10 files) | ~840 | Migrated to Pest |
| `packages/laravel-queue/phpunit.xml` | 21 | Replaced by Pest.php |
| `benchmarks/smoke.php` | 210 | Replaced by run-benchmarks.php |
| `benchmarks/compare.php` | 286 | Replaced by run-benchmarks.php |
| `benchmarks/laravel-smoke.php` | 182 | Replaced by LaravelSmokeBenchmark.php |
| `benchmarks/laravel-compare.php` | 365 | Replaced by LaravelCompareBenchmark.php |
| `benchmarks/drivers/*.php` (3 files) | 571 | Moved + adapted |
| `benchmarks/lib/Metrics.php` | 119 | Merged into AbstractBenchmark |
| `benchmarks/lib/Budget.php` | 99 | Moved to src/Budget.php |

### Created

| Path | Lines est. | Reason |
|------|-----------|--------|
| `crates/rabbit-rs-core/tests/publisher.rs` | 500 | Consolidated publisher tests |
| `crates/rabbit-rs-core/tests/consumer.rs` | 400 | Consolidated consumer tests |
| `crates/rabbit-rs-core/tests/recovery.rs` | 300 | Consolidated recovery tests |
| `crates/rabbit-rs-core/tests/topology.rs` | 400 | Consolidated topology tests |
| `crates/rabbit-rs-core/tests/metrics.rs` | 200 | Renamed from metrics_snapshot |
| `crates/rabbit-rs-core/tests/integration.rs` | 400 | Consolidated integration tests |
| `crates/rabbit-rs-php/tests/Pest.php` | 30 | Pest bootstrap |
| `crates/rabbit-rs-php/tests/Extension/*` (2 files) | 80 | Extension + secrets tests |
| `crates/rabbit-rs-php/tests/Pool/*` (2 files) | 100 | Pool registry + fork tests |
| `crates/rabbit-rs-php/tests/Publisher/*` (3 files) | 150 | Publisher outcomes, backpressure, boundaries |
| `crates/rabbit-rs-php/tests/Consumer/*` (1 file) | 80 | Consumer terminal state |
| `crates/rabbit-rs-php/tests/Config/*` (1 file) | 60 | Config validation |
| `crates/rabbit-rs-php/tests/Reflection/*` (1 file) | 100 | Reflection contract |
| `crates/rabbit-rs-php/tests/BinaryPayload/*` (1 file) | 70 | Binary payload |
| `crates/rabbit-rs-php/composer.json` | updated | Adds pest dev-dep |
| `benchmarks/src/AbstractBenchmark.php` | 80 | Base class |
| `benchmarks/src/Config.php` | 30 | Constants |
| `benchmarks/src/Drivers/BunnyDriver.php` | 190 | New 4th driver |
| `benchmarks/src/Scenarios/*.php` (3 files) | 200 | Scenario implementations |
| `benchmarks/src/run-benchmarks.php` | 120 | Unified runner |
| `benchmarks/laravel/LaravelSmokeBenchmark.php` | 100 | Laravel smoke |
| `benchmarks/laravel/LaravelCompareBenchmark.php` | 150 | Laravel compare |
| `benchmarks/docker-compose.yml` | 25 | Single-node RabbitMQ |
| `benchmarks/run-benchmarks.sh` | 30 | Wrapper script |

### Net impact

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Rust integration test files | 17 | 6 | -11 |
| Rust integration test lines | ~5,300 | ~2,200 | -3,100 |
| Criterion bench files | 7 | 0 | -7 |
| Criterion bench lines | 820 | 0 | -820 |
| PHPT files | 11 | 1 | -10 |
| PHPUnit Laravel files | 26 | 0 (→ Pest) | -26 |
| Pest files (extension) | 0 | 10 | +10 |
| Pest files (Laravel) | 0 | 22 | +22 |
| Benchmark PHP files | 8 | ~12 | +4 |
| **Net lines** | | | **~-2,350** |

---

## Verification

After implementation:

1. `cargo fmt --all -- --check` passes
2. `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
3. `cargo test --workspace --all-targets` — all consolidated Rust tests pass
4. `vendor/bin/pest --parallel` in `crates/rabbit-rs-php/` — all extension Pest tests pass
5. `php run-tests.php tests/phpt/extension_metadata.phpt` — PHPT passes
6. `vendor/bin/pest tests/Unit tests/Feature --parallel` in `packages/laravel-queue/` — Laravel tests pass
7. `vendor/bin/pest tests/Integration` — integration tests pass (requires lab)
8. `php benchmarks/src/run-benchmarks.php --scenario=fire-and-forget` — benchmark runs
9. `./scripts/check.sh` — full quality gate passes
