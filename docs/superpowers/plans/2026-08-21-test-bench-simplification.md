# Test & Benchmark Simplification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consolidate Rust integration tests (17→6 files), migrate PHPT and PHPUnit to Pest with parallelism, decommission Criterion benchmarks, and restructure PHP benchmarks with the AbstractBenchmark pattern from the reference repo.

**Architecture:** Five sequential phases — (1) remove Criterion benches and Cargo config, (2) consolidate Rust integration tests, (3) create Pest extension tests from PHPT, (4) migrate Laravel PHPUnit to Pest, (5) restructure PHP benchmarks. Each phase ends with a green test run and commit.

**Tech Stack:** Rust 1.96 / edition 2024, PHP 8.4+, Pest 3.x, Orchestra Testbench 10/11, ext-php-rs, MockTransport, Tokio paused time

## Global Constraints

- Rust pinned to 1.96.0, edition 2024, `#![forbid(unsafe_code)]`
- Unsafe Rust is forbidden; do not weaken the workspace lint configuration
- Keep Lapin behind the Transport abstraction so broker behavior remains mockable
- Never expose credentials, complete broker URIs, or private certificate material through Debug, errors, metrics, or logs
- Do not add real sleeps to unit tests; use paused Tokio time and MockTransport
- Preserve unrelated work in a dirty tree; never discard changes you did not create
- Run `cargo fmt --all` after Rust edits
- The `bench` feature in `crates/rabbit-rs-core/Cargo.toml` is removed entirely
- The `[[test]] chaos_reconnect` entry in `Cargo.toml` is KEPT unchanged
- `scripts/check.sh` content: `cargo fmt --all -- --check` + `cargo clippy --workspace --all-targets --all-features -- -D warnings` + `cargo test --workspace --all-targets` + `composer validate --strict`

---

## File Structure

### Phase 1: Criterion Decommission (Tasks 1–2)

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/rabbit-rs-core/benches/*.rs` (7 files) | Delete | Remove all Criterion benchmarks |
| `crates/rabbit-rs-core/Cargo.toml` | Modify | Remove criterion dev-dep, all `[[bench]]` sections, `bench` feature |
| `crates/rabbit-rs-core/src/transport/lapin.rs:536-540` | Modify | Remove `publish_properties_bench` pub fn |
| `Cargo.toml` (workspace root) | Modify | Remove `[profile.bench]` section |

### Phase 2: Rust Test Consolidation (Tasks 3–9)

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/rabbit-rs-core/tests/publisher.rs` | Create | Merged publisher tests (safety + recovery + delay + routing) |
| `crates/rabbit-rs-core/tests/consumer.rs` | Create | Merged consumer tests (semantics + cleanup) |
| `crates/rabbit-rs-core/tests/recovery.rs` | Create | Merged recovery tests (coordinator + state machine) |
| `crates/rabbit-rs-core/tests/topology.rs` | Create | Merged topology tests (recovery + modes + dlq + attempts) |
| `crates/rabbit-rs-core/tests/metrics.rs` | Create | Renamed from metrics_snapshot.rs |
| `crates/rabbit-rs-core/tests/integration.rs` | Create | Merged integration tests (publish_consume + scheduler + client_pool pruned) |
| 11 original test files | Delete | Replaced by the 6 consolidated files |
| `crates/rabbit-rs-core/tests/tls.rs` | Delete | TLS tests migrate to inline `src/config.rs` |
| `crates/rabbit-rs-core/src/config.rs` | Modify | Add TLS test cases to existing `#[cfg(test)]` module |

### Phase 3: Pest Extension Tests (Tasks 10–13)

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/rabbit-rs-php/composer.json` | Create/Modify | Add pestphp/pest dev-dependency |
| `crates/rabbit-rs-php/tests/Pest.php` | Create | Pest bootstrap with testingPool() helper |
| 10 Pest test files in `tests/Extension/`, `tests/Pool/`, `tests/Publisher/`, `tests/Consumer/`, `tests/Config/`, `tests/Reflection/`, `tests/BinaryPayload/` | Create | Migrated from PHPT |
| 10 PHPT files in `tests/phpt/` (all except `extension_metadata.phpt`) | Delete | Migrated to Pest |
| `scripts/test-extension.sh` | Modify | Run Pest + 1 PHPT |

### Phase 4: Laravel Pest Migration (Tasks 14–16)

| File | Action | Responsibility |
|------|--------|---------------|
| `packages/laravel-queue/composer.json` | Modify | Replace phpunit with pest + pest-plugin-laravel |
| `packages/laravel-queue/tests/Pest.php` | Create | Pest bootstrap with Testbench integration |
| 22 Pest test files (Unit + Feature + Integration) | Create | Migrated from PHPUnit |
| `packages/laravel-queue/phpunit.xml` | Delete | Replaced by Pest.php |
| 26 PHPUnit test files | Delete | Replaced by Pest equivalents |

### Phase 5: PHP Benchmark Restructuring (Tasks 17–21)

| File | Action | Responsibility |
|------|--------|---------------|
| `benchmarks/src/AbstractBenchmark.php` | Create | Base class with runBenchmark + calculateStats |
| `benchmarks/src/Config.php` | Create | Centralized constants |
| `benchmarks/src/Budget.php` | Create | Moved from lib/Budget.php |
| `benchmarks/src/Driver.php` | Create | Moved from drivers/Driver.php |
| `benchmarks/src/Drivers/*.php` (4 files) | Create | 4 drivers adapted to AbstractBenchmark pattern |
| `benchmarks/src/Scenarios/*.php` (3 files) | Create | fire-and-forget, batch-confirm, auto-ack scenarios |
| `benchmarks/src/run-benchmarks.php` | Create | Unified runner with --scenario and --driver filters |
| `benchmarks/laravel/*.php` (2 files) | Create | Laravel smoke + compare benchmarks |
| `benchmarks/docker-compose.yml` | Create | Single-node RabbitMQ |
| `benchmarks/run-benchmarks.sh` | Create | Wrapper script |
| `benchmarks/composer.json` | Modify | Add bunny/bunny, restructure autoload |
| Old benchmark files (smoke.php, compare.php, laravel-*.php, drivers/, lib/) | Delete | Replaced by new structure |

---

## Phase 1: Criterion Decommission

### Task 1: Remove Criterion Benchmarks and Cargo Configuration

**Files:**
- Delete: `crates/rabbit-rs-core/benches/batching.rs`
- Delete: `crates/rabbit-rs-core/benches/confirm_ledger.rs`
- Delete: `crates/rabbit-rs-core/benches/ffi_conversion.rs`
- Delete: `crates/rabbit-rs-core/benches/lapin_properties.rs`
- Delete: `crates/rabbit-rs-core/benches/publisher_actor.rs`
- Delete: `crates/rabbit-rs-core/benches/scheduler.rs`
- Delete: `crates/rabbit-rs-core/benches/transport.rs`
- Delete: `crates/rabbit-rs-core/benches/` (directory, now empty)
- Modify: `crates/rabbit-rs-core/Cargo.toml`
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs:536-540`

**Interfaces:**
- Consumes: none
- Produces: Cargo workspace with no Criterion dependency, no `bench` feature, no `[[bench]]` sections, no `[profile.bench]`, no `publish_properties_bench` fn

- [ ] **Step 1: Delete the 7 Criterion benchmark files**

```bash
rm crates/rabbit-rs-core/benches/batching.rs
rm crates/rabbit-rs-core/benches/confirm_ledger.rs
rm crates/rabbit-rs-core/benches/ffi_conversion.rs
rm crates/rabbit-rs-core/benches/lapin_properties.rs
rm crates/rabbit-rs-core/benches/publisher_actor.rs
rm crates/rabbit-rs-core/benches/scheduler.rs
rm crates/rabbit-rs-core/benches/transport.rs
rmdir crates/rabbit-rs-core/benches
```

- [ ] **Step 2: Remove Criterion dev-dependency, all [[bench]] sections, and bench feature from `crates/rabbit-rs-core/Cargo.toml`**

Remove from `[features]` section (line 12): `bench = []`

Remove from `[dev-dependencies]` section (line 28): `criterion = { version = "0.5", features = ["html_reports"] }`

Remove all 7 `[[bench]]` blocks (lines 34–68):

```toml
[[bench]]
name = "batching"
path = "benches/batching.rs"
harness = false

[[bench]]
name = "confirm_ledger"
path = "benches/confirm_ledger.rs"
harness = false

[[bench]]
name = "ffi_conversion"
path = "benches/ffi_conversion.rs"
harness = false

[[bench]]
name = "lapin_properties"
path = "benches/lapin_properties.rs"
harness = false
required-features = ["bench"]

[[bench]]
name = "publisher_actor"
path = "benches/publisher_actor.rs"
harness = false

[[bench]]
name = "scheduler"
path = "benches/scheduler.rs"
harness = false

[[bench]]
name = "transport"
path = "benches/transport.rs"
harness = false
```

Keep the `[[test]]` section for chaos_reconnect (lines 70–73).

The resulting `[features]` section should be:
```toml
[features]
test-support = []
integration = []
```

- [ ] **Step 3: Remove `[profile.bench]` from workspace root `Cargo.toml`**

Remove lines 33–35:
```toml
[profile.bench]
lto = "fat"
codegen-units = 1
```

- [ ] **Step 4: Remove `publish_properties_bench` from `src/transport/lapin.rs`**

Remove lines 536–540:
```rust
#[cfg(feature = "bench")]
#[doc(hidden)]
pub fn publish_properties_bench(request: &PublishRequest) -> BasicProperties {
    publish_properties(request)
}
```

- [ ] **Step 5: Verify Rust builds and tests pass**

Run: `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings && rtk cargo test --workspace --all-targets`
Expected: PASS (no compilation errors, all existing tests still pass)

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: decommission Criterion benchmarks and bench feature

Remove all 7 Criterion benchmark files, the criterion dev-dependency,
all [[bench]] Cargo.toml sections, the bench feature, [profile.bench],
and the publish_properties_bench pub fn. Rust performance is validated
through the PHP benchmark suite."
```

---

## Phase 2: Rust Test Consolidation

### Task 2: Create `publisher.rs` — Consolidated Publisher Tests

**Files:**
- Create: `crates/rabbit-rs-core/tests/publisher.rs`
- Delete: `crates/rabbit-rs-core/tests/publisher_safety.rs`
- Delete: `crates/rabbit-rs-core/tests/publisher_recovery.rs`
- Delete: `crates/rabbit-rs-core/tests/publisher_delay.rs`
- Delete: `crates/rabbit-rs-core/tests/delay_routing.rs`

**Interfaces:**
- Consumes: `rabbit_rs_core::{config::*, publisher::*, transport::mock::*, topology::delay::*}` from existing tests
- Produces: `publisher.rs` with shared `helper` mod containing `broker()`, `config()`, `request()`, `actor()`, `wait_for_publish_count()`

**Consolidation approach:** Merge all tests from the 4 source files into a single file. Extract the common helpers (`broker()`, `config()`, `request()`, `actor()`, `wait_for_publish_count()`) into a `mod helper` at the top. De-duplicate identical helper functions that appear in multiple files (each file currently has its own `broker()`, `config()`, `request()`, `wait_for_publish_count()`). Resolve import conflicts by merging import blocks — most files import the same types from `rabbit_rs_core`.

The 4 source files use different test attributes:
- `publisher_safety.rs`: mostly `#[tokio::test(start_paused = true)]` + one `#[test]`
- `publisher_recovery.rs`: all `#[tokio::test(start_paused = true)]`
- `publisher_delay.rs`: mostly `#[tokio::test(start_paused = true)]` + two `#[test]`
- `delay_routing.rs`: mixed `#[tokio::test]`, `#[test]`, one `#[tokio::test(start_paused = true)]`

Preserve each test's original attribute — do not change timing semantics.

- [ ] **Step 1: Create `publisher.rs` with merged imports and shared helper module**

Read all 4 source files fully. Merge their imports into a single `use` block. Create a `mod helper` containing the unified versions of:
- `fn broker() -> BrokerConfig` (from publisher_safety — the simplest version)
- `fn config(max_messages: usize, max_bytes: usize) -> PublisherConfig` (from publisher_safety)
- `fn request(message_id: &str, payload: &'static [u8]) -> PublishRequest` (from publisher_safety)
- `fn delayed_request(message_id: &str, delay_ms: u64) -> PublishRequest` (from publisher_delay)
- `fn immediate_request(message_id: &str) -> PublishRequest` (from publisher_delay)
- `fn ttl_config() -> DelayConfig` (from publisher_delay)
- `async fn actor(transport: &MockTransport, config: PublisherConfig) -> PublisherHandle` (from publisher_safety)
- `async fn spawn_actor(transport: &MockTransport, config: PublisherConfig, delay_strategy: DelayStrategy) -> PublisherHandle` (from publisher_delay)
- `async fn wait_for_publish_count(transport: &MockTransport, expected: usize)` (from publisher_safety — without the `tokio::time::advance` variant; tests that need advance call it inline)
- `fn find_publish(transport: &MockTransport) -> TransportRequest` (from publisher_delay)
- `fn publish_operations(transport: &MockTransport) -> Vec<...>` (from publisher_recovery)
- `async fn suspend(actor: &PublisherHandle)` (from publisher_recovery)
- `struct FixedProbe`, `struct PendingProbe` + their `DelayPluginProbe` impls (from delay_routing)
- `fn config_delay(mode: DelayMode) -> DelayConfig` (from delay_routing, renamed to avoid conflict with `config()`)

- [ ] **Step 2: Copy all test functions from the 4 source files**

Copy every `#[tokio::test(...)]` / `#[test]` function from:
1. `publisher_safety.rs` — all 16 tests
2. `publisher_recovery.rs` — all 13 tests
3. `publisher_delay.rs` — all 8 tests
4. `delay_routing.rs` — all 8 tests

If two tests have the same name across files, prefix the later one with a qualifier (e.g., `delay_routing_auto_selects...`). Check for name collisions before copying.

- [ ] **Step 3: Delete the 4 source files**

```bash
rm crates/rabbit-rs-core/tests/publisher_safety.rs
rm crates/rabbit-rs-core/tests/publisher_recovery.rs
rm crates/rabbit-rs-core/tests/publisher_delay.rs
rm crates/rabbit-rs-core/tests/delay_routing.rs
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo fmt --all && rtk cargo test -p rabbit-rs-core --test publisher`
Expected: PASS — all 45 tests pass

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: consolidate publisher tests into single file

Merge publisher_safety, publisher_recovery, publisher_delay, and
delay_routing into a single publisher.rs with shared helper module.
De-duplicates broker(), config(), request(), and wait_for_publish_count()
helpers that were copied across 4 files."
```

---

### Task 3: Create `consumer.rs` — Consolidated Consumer Tests

**Files:**
- Create: `crates/rabbit-rs-core/tests/consumer.rs`
- Delete: `crates/rabbit-rs-core/tests/consumer_semantics.rs`
- Delete: `crates/rabbit-rs-core/tests/consumer_cleanup.rs`

**Interfaces:**
- Consumes: `rabbit_rs_core::{config::*, consumer::*, pool::*, publisher::*, transport::mock::*}`
- Produces: `consumer.rs` with shared helpers from `consumer_semantics.rs`

**Consolidation approach:** `consumer_semantics.rs` (695 lines, 18 tests) is the larger file. `consumer_cleanup.rs` (230 lines, 6 tests) uses the same `MockTransport` pattern. Merge the helpers from both into a shared `mod helper` at the top.

`consumer_semantics.rs` uses `#[tokio::test]` (no start_paused) for most tests, with `#[tokio::test(start_paused = true)]` on a couple. `consumer_cleanup.rs` uses `#[tokio::test]` — preserve original attributes.

- [ ] **Step 1: Create `consumer.rs` with merged imports and shared helpers**

Read both source files fully. Merge imports. Create a `mod helper` containing:
- `fn broker(name: &str, vhost: &str) -> BrokerConfig` (from consumer_semantics — parameterized version)
- `fn connection_key(name: &str, vhost: &str) -> ConnectionKey` (from consumer_semantics)
- `fn delivery(tag: u64, payload: &'static [u8]) -> TransportDelivery` (from consumer_semantics)
- `fn delivery_with_properties(tag: u64, payload: &'static [u8], message_id: &str, correlation_id: &str) -> TransportDelivery` (from consumer_semantics)
- `async fn subscription(transport: &MockTransport, id: &str, key: ConnectionKey, prefetch: u16, priority: i16) -> Subscription` (from consumer_semantics)
- `async fn publisher(transport: &MockTransport) -> PublisherHandle` (from consumer_semantics)

`consumer_cleanup.rs` likely has its own helpers — merge them into the same `mod helper`.

- [ ] **Step 2: Copy all test functions from both source files**

Copy every test from:
1. `consumer_semantics.rs` — all 18 tests
2. `consumer_cleanup.rs` — all 6 tests

Check for name collisions before copying.

- [ ] **Step 3: Delete the 2 source files**

```bash
rm crates/rabbit-rs-core/tests/consumer_semantics.rs
rm crates/rabbit-rs-core/tests/consumer_cleanup.rs
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo fmt --all && rtk cargo test -p rabbit-rs-core --test consumer`
Expected: PASS — all 24 tests pass

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: consolidate consumer tests into single file

Merge consumer_semantics and consumer_cleanup into a single consumer.rs
with shared helper module."
```

---

### Task 4: Create `recovery.rs` — Consolidated Recovery Tests

**Files:**
- Create: `crates/rabbit-rs-core/tests/recovery.rs`
- Delete: `crates/rabbit-rs-core/tests/recovery_coordinator.rs`
- Delete: `crates/rabbit-rs-core/tests/recovery_state_machine.rs`

**Interfaces:**
- Consumes: `rabbit_rs_core::{config::*, pool::recovery_coordinator::*, publisher::*, recovery::*, topology::*, transport::mock::*}`
- Produces: `recovery.rs` with shared helpers

**Consolidation approach:** Both files test the same recovery state machine. `recovery_coordinator.rs` (354 lines, 5 tests) uses `#[tokio::test(start_paused = true)]` exclusively. `recovery_state_machine.rs` (297 lines, 7 tests) uses `#[tokio::test(start_paused = true)]` for most tests.

- [ ] **Step 1: Create `recovery.rs` with merged imports and shared helpers**

Read both source files fully. Merge imports. Create a `mod helper` containing:
- `fn broker() -> BrokerConfig` (from recovery_coordinator)
- `fn config() -> Arc<ValidatedConfig>` (from recovery_coordinator)
- `fn publisher_config() -> PublisherConfig` (from recovery_coordinator)
- `fn publish_request(message_id: &str, deadline: Instant) -> PublishRequest` (from recovery_coordinator)
- `fn topology_plan() -> TopologyPlan` (from recovery_coordinator)
- `fn coordinator_config(config: Arc<ValidatedConfig>) -> RecoveryCoordinatorConfig` (from recovery_coordinator)
- `fn dyn_transport(transport: &Arc<MockTransport>) -> Arc<dyn Transport>` (from recovery_coordinator)
- `async fn wait_for_state(handle: &RecoveryCoordinatorHandle, predicate: impl Fn(&ConnectionState) -> bool) -> ConnectionState` (from recovery_coordinator)
- Any helpers from `recovery_state_machine.rs` not already covered

- [ ] **Step 2: Copy all test functions from both source files**

1. `recovery_coordinator.rs` — all 5 tests
2. `recovery_state_machine.rs` — all 7 tests

Check for name collisions.

- [ ] **Step 3: Delete the 2 source files**

```bash
rm crates/rabbit-rs-core/tests/recovery_coordinator.rs
rm crates/rabbit-rs-core/tests/recovery_state_machine.rs
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo fmt --all && rtk cargo test -p rabbit-rs-core --test recovery`
Expected: PASS — all 12 tests pass

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: consolidate recovery tests into single file

Merge recovery_coordinator and recovery_state_machine into a single
recovery.rs with shared helper module."
```

---

### Task 5: Create `topology.rs` — Consolidated Topology Tests

**Files:**
- Create: `crates/rabbit-rs-core/tests/topology.rs`
- Delete: `crates/rabbit-rs-core/tests/topology_recovery.rs`
- Delete: `crates/rabbit-rs-core/tests/topology_modes.rs`
- Delete: `crates/rabbit-rs-core/tests/dlq_topology.rs`
- Delete: `crates/rabbit-rs-core/tests/delivery_attempts.rs`

**Interfaces:**
- Consumes: `rabbit_rs_core::{config::*, topology::*, transport::mock::*}` and types from each source file
- Produces: `topology.rs` with shared helpers

**Consolidation approach:** Four files merged. `topology_modes.rs` is integration-gated (`#![cfg(feature = "integration")]`) — preserve the `#[cfg(feature = "integration")]` attribute on those tests. The other three are mock-transport tests. `delivery_attempts.rs` tests `AttemptsResolver` (pure logic, `#[test]`), `dlq_topology.rs` tests config validation and reconciler, `topology_recovery.rs` tests declare ordering and idempotency.

- [ ] **Step 1: Create `topology.rs` with merged imports and shared helpers**

Read all 4 source files fully. Merge imports. Create a `mod helper` containing:
- `fn broker() -> BrokerConfig` (from topology_recovery)
- `fn exchange(name: &str) -> ExchangeSpec` (from topology_recovery)
- `fn binding(queue: &str, exchange: &str, routing_key: &str) -> BindingSpec` (from topology_recovery)
- `fn definition() -> TopologyDefinition` (from topology_recovery)
- `async fn topology_channel(transport: &MockTransport) -> Box<dyn ConsumerChannel>` (from topology_recovery)
- Any helpers from `dlq_topology.rs` and `delivery_attempts.rs`

Note: `topology_modes.rs` has `#![cfg(feature = "integration")]` at the top — do NOT put this at the file level. Instead, gate individual test functions with `#[cfg(feature = "integration")]` and `#[tokio::test]`.

- [ ] **Step 2: Copy all test functions from the 4 source files**

1. `topology_recovery.rs` — all 10 tests (mixed `#[test]` and `#[tokio::test]`)
2. `topology_modes.rs` — all 4 tests, each with `#[cfg(feature = "integration")]` + `#[tokio::test]`
3. `dlq_topology.rs` — all 11 tests
4. `delivery_attempts.rs` — all 9 tests (all `#[test]`)

Check for name collisions.

- [ ] **Step 3: Delete the 4 source files**

```bash
rm crates/rabbit-rs-core/tests/topology_recovery.rs
rm crates/rabbit-rs-core/tests/topology_modes.rs
rm crates/rabbit-rs-core/tests/dlq_topology.rs
rm crates/rabbit-rs-core/tests/delivery_attempts.rs
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo fmt --all && rtk cargo test -p rabbit-rs-core --test topology`
Expected: PASS — all mock-transport tests pass (integration-gated tests are compiled out without `--features integration`)

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: consolidate topology tests into single file

Merge topology_recovery, topology_modes, dlq_topology, and
delivery_attempts into a single topology.rs with shared helper module.
Integration-gated tests preserved with per-function cfg attribute."
```

---

### Task 6: Create `metrics.rs` — Renamed from `metrics_snapshot.rs`

**Files:**
- Rename: `crates/rabbit-rs-core/tests/metrics_snapshot.rs` → `crates/rabbit-rs-core/tests/metrics.rs`

**Interfaces:**
- Consumes: none (standalone file, unchanged content)
- Produces: `metrics.rs` test target

- [ ] **Step 1: Rename the file**

```bash
mv crates/rabbit-rs-core/tests/metrics_snapshot.rs crates/rabbit-rs-core/tests/metrics.rs
```

No content changes needed — the file is self-contained.

- [ ] **Step 2: Run tests to verify they pass**

Run: `rtk cargo fmt --all && rtk cargo test -p rabbit-rs-core --test metrics`
Expected: PASS — all 5 tests pass

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "refactor: rename metrics_snapshot test to metrics"
```

---

### Task 7: Create `integration.rs` — Consolidated Integration + Scheduler + Pool Tests

**Files:**
- Create: `crates/rabbit-rs-core/tests/integration.rs`
- Delete: `crates/rabbit-rs-core/tests/publish_consume.rs`
- Delete: `crates/rabbit-rs-core/tests/scheduler_fairness.rs`
- Delete: `crates/rabbit-rs-core/tests/client_pool.rs`

**Interfaces:**
- Consumes: `rabbit_rs_core::{client::*, config::*, consumer::*, publisher::*, transport::mock::*}`
- Produces: `integration.rs` with shared helpers

**Consolidation approach:** Three files merged. `publish_consume.rs` (298 lines, 4 tests) is `#![cfg(feature = "integration")]` — gate those tests with `#[cfg(feature = "integration")]`. `scheduler_fairness.rs` (128 lines, 6 tests) is pure `#[test]` — no Tokio. `client_pool.rs` (645 lines, 16 tests) — prune to ~8 most representative tests (drop tests that overlap with publisher/consumer/recovery coverage).

**Pruning `client_pool.rs`:** Keep these 8 tests (most representative):
1. `reuses_one_connection_and_publisher_for_confirmed_messages` — connection reuse core behavior
2. One batch publish test — batch behavior
3. One consumer profile creation test — consumer setup
4. `close_during_connect_does_not_panic` — edge case not covered elsewhere
5. One parallel broker initialization test — concurrency
6. One queue size/purge test — admin operations
7. One connection state reporting test — metrics integration
8. One publisher utilization test — backpressure

Drop tests that are pure edge-case duplicates of publisher/consumer/recovery behavior (e.g., specific NACK handling, specific replay scenarios already covered in publisher.rs/recovery.rs).

- [ ] **Step 1: Create `integration.rs` with merged imports and shared helpers**

Read all 3 source files fully. Merge imports. Create a `mod helper` containing:
- `fn config() -> ValidatedConfig` (from client_pool)
- `fn consumer_config() -> ValidatedConfig` (from client_pool)
- `fn two_broker_config() -> ValidatedConfig` (from client_pool)
- `fn request(message_id: &str) -> PublishRequest` (from client_pool)
- `fn id(value: &str) -> SubscriptionId` (from scheduler_fairness)
- `fn policy(weight: u16, priority_class: i16) -> SubscriptionPolicy` (from scheduler_fairness)
- Any helpers from `publish_consume.rs`

- [ ] **Step 2: Copy tests from the 3 source files**

1. `publish_consume.rs` — all 4 tests, each with `#[cfg(feature = "integration")]` + `#[tokio::test]`
2. `scheduler_fairness.rs` — all 6 tests (all `#[test]`)
3. `client_pool.rs` — 8 selected tests (pruned from 16)

Check for name collisions.

- [ ] **Step 3: Delete the 3 source files**

```bash
rm crates/rabbit-rs-core/tests/publish_consume.rs
rm crates/rabbit-rs-core/tests/scheduler_fairness.rs
rm crates/rabbit-rs-core/tests/client_pool.rs
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo fmt --all && rtk cargo test -p rabbit-rs-core --test integration`
Expected: PASS — all non-integration tests pass (integration-gated tests compiled out)

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: consolidate integration, scheduler, and pool tests

Merge publish_consume (integration-gated), scheduler_fairness, and
pruned client_pool into a single integration.rs with shared helper
module. client_pool pruned from 16 to 8 representative tests."
```

---

### Task 8: Migrate TLS Tests to Inline `src/config.rs`

**Files:**
- Delete: `crates/rabbit-rs-core/tests/tls.rs`
- Modify: `crates/rabbit-rs-core/src/config.rs` (existing `#[cfg(test)]` module)

**Interfaces:**
- Consumes: existing `#[cfg(test)]` module in `src/config.rs`
- Produces: inline TLS config tests in `src/config.rs`

**Approach:** `tls.rs` (229 lines, 9 tests) tests pure config parsing — `connection_uri()` and TLS config deserialization. These are `#[test]` (synchronous, no Tokio). The existing `#[cfg(test)]` module in `src/config.rs` (starting at line 838) already tests config parsing. Add the TLS-specific tests there.

- [ ] **Step 1: Read `tls.rs` fully to understand all 9 tests**

Read the full file. Each test is a `#[test]` function that builds a `BrokerConfig` with TLS config and calls `connection_uri()`. Import the `BrokerConfig`, `Credentials`, `Endpoint`, `TlsConfig`, `TlsVerify` types and the `connection_uri` function.

- [ ] **Step 2: Read the existing `#[cfg(test)]` module in `src/config.rs`**

Read `src/config.rs` from line 838 to end. Understand the existing test imports and patterns.

- [ ] **Step 3: Add TLS tests to the existing `#[cfg(test)]` module**

Copy the 9 test functions from `tls.rs` into the `#[cfg(test)]` module in `src/config.rs`. Add any missing imports (`TlsVerify`, `connection_uri` from `transport::lapin`). Ensure test names don't collide with existing tests in `src/config.rs`. If any do, prefix them with `tls_`.

- [ ] **Step 4: Delete `tls.rs`**

```bash
rm crates/rabbit-rs-core/tests/tls.rs
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `rtk cargo fmt --all && rtk cargo test -p rabbit-rs-core config::tests`
Expected: PASS — all config tests including migrated TLS tests pass

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: migrate TLS tests to inline config module

Move 9 TLS config tests from tests/tls.rs into the existing #[cfg(test)]
module in src/config.rs, eliminating a standalone test file for pure
config parsing tests."
```

---

### Task 9: Full Rust Quality Gate

- [ ] **Step 1: Run full quality gate**

Run: `rtk ./scripts/check.sh`
Expected: PASS — `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace --all-targets`, `composer validate --strict` all pass

- [ ] **Step 2: Verify test count is reasonable**

Run: `rtk cargo test --workspace --all-targets 2>&1 | tail -5`
Expected: Test count should reflect the consolidation (fewer test binaries, similar test count — tests were merged not removed, except ~8 pruned from client_pool)

---

## Phase 3: Pest Extension Tests

### Task 10: Set Up Pest Infrastructure for PHP Extension

**Files:**
- Create: `crates/rabbit-rs-php/composer.json`
- Create: `crates/rabbit-rs-php/tests/Pest.php`

**Interfaces:**
- Consumes: `Goopil\RabbitRs\testing_pool()` from the PHP extension (built with `--features extension-tests`)
- Produces: `testingPool()` and `defaultConfig()` helper functions available to all Pest test files

- [ ] **Step 1: Create `composer.json` for the PHP extension crate**

```json
{
    "name": "rabbit-rs/php-extension",
    "description": "Native PHP extension for rabbit-rs",
    "type": "extension",
    "license": "MIT",
    "require": {
        "php": ">=8.4"
    },
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

- [ ] **Step 2: Run `composer install` to install Pest**

```bash
cd crates/rabbit-rs-php && composer install
```

- [ ] **Step 3: Create `tests/Pest.php` bootstrap**

```php
<?php

declare(strict_types=1);

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

function pubMessage(string $payload = 'test payload'): array
{
    return [
        'broker' => 'testing',
        'exchange' => '',
        'routing_key' => 'test',
        'payload' => $payload,
        'message_id' => uniqid('', true),
    ];
}
```

- [ ] **Step 4: Verify Pest runs (empty, extension not loaded)**

Run: `cd crates/rabbit-rs-php && vendor/bin/pest`
Expected: PASS (0 tests or all skipped — extension not loaded in dev)

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: add Pest infrastructure for PHP extension tests

Create composer.json with pestphp/pest dev-dependency and Pest.php
bootstrap with testingPool(), defaultConfig(), and pubMessage() helpers."
```

---

### Task 11: Migrate PHPT Tests to Pest (Part 1 — Extension, Pool, Publisher)

**Files:**
- Create: `crates/rabbit-rs-php/tests/Extension/ExtensionTest.php`
- Create: `crates/rabbit-rs-php/tests/Extension/SecretsTest.php`
- Create: `crates/rabbit-rs-php/tests/Pool/PoolRegistryTest.php`
- Create: `crates/rabbit-rs-php/tests/Pool/ForkInvalidationTest.php`
- Create: `crates/rabbit-rs-php/tests/Publisher/PublisherOutcomesTest.php`
- Create: `crates/rabbit-rs-php/tests/Publisher/BackpressureTest.php`
- Create: `crates/rabbit-rs-php/tests/Publisher/BoundaryLimitsTest.php`
- Delete: `crates/rabbit-rs-php/tests/phpt/extension_metadata.phpt` (keep — only the other 10 PHPT files are deleted)
- Delete: `crates/rabbit-rs-php/tests/phpt/secrets.phpt`
- Delete: `crates/rabbit-rs-php/tests/phpt/pid_registry.phpt`
- Delete: `crates/rabbit-rs-php/tests/phpt/fork_invalidation.phpt`
- Delete: `crates/rabbit-rs-php/tests/phpt/publisher_outcomes.phpt`
- Delete: `crates/rabbit-rs-php/tests/phpt/backpressure.phpt`
- Delete: `crates/rabbit-rs-php/tests/phpt/boundary_limits.phpt`

**Interfaces:**
- Consumes: `testingPool()`, `defaultConfig()`, `pubMessage()` from `Pest.php`
- Produces: 7 Pest test files covering extension metadata, secrets, pool registry, fork invalidation, publisher outcomes, backpressure, boundary limits

**Approach:** For each PHPT file, read the full content, understand what it tests, and rewrite as Pest. The PHPT files use `testing_pool()` with scenario hashes — the Pest versions use the `testingPool()` helper from `Pest.php`. Use Pest `dataset()` for parametric tests (e.g., boundary limits, publisher outcomes).

- [ ] **Step 1: Read all 7 PHPT source files**

Read each PHPT file fully to understand the test logic, assertions, and scenarios used.

- [ ] **Step 2: Create `ExtensionTest.php`**

Migrate `extension_metadata.phpt` content. Test:
- `extension_loaded('rabbit_rs')` is true
- Version matches expected (from `RABBIT_RS_EXPECTED_VERSION` env or constant)
- Exception hierarchy: `BackpressureException` and `ConnectionException` extend `Exception`
- Classes `Pool`, `Consumer`, `Delivery` exist and are `final`

- [ ] **Step 3: Create `SecretsTest.php`**

Migrate `secrets.phpt` content. Test:
- Config with password in URI throws exception, message does NOT contain the password
- Config with private key throws exception, message does NOT contain key material

- [ ] **Step 4: Create `PoolRegistryTest.php`**

Migrate `pid_registry.phpt` content. Test:
- Equivalent configs produce same handle (process-local)
- Different vhosts produce distinct handles
- Close invalidates handles
- Handle replacement after close

- [ ] **Step 5: Create `ForkInvalidationTest.php`**

Migrate `fork_invalidation.phpt` content. Test:
- After `pcntl_fork()`, inherited pools are invalidated in child
- Child creates its own registry with distinct handle
- Parent handle remains valid

Use `->group('isolation')` to run sequentially.

- [ ] **Step 6: Create `PublisherOutcomesTest.php`**

Migrate `publisher_outcomes.phpt` content. Use Pest `dataset()` for the 4 outcomes:

```php
dataset('outcomes', [
    'ack'           => ['ack', null, 'confirmed'],
    'returned'      => ['returned', ConnectionException::class, '312'],
    'pending'       => ['pending', ConnectionException::class, 'timeout'],
    'transport_error' => ['transport_error', ConnectionException::class, 'transport'],
]);
```

- [ ] **Step 7: Create `BackpressureTest.php`**

Migrate `backpressure.phpt` content. Test:
- `testingPool()` with `publisher_capacity=1, pending_confirmations=1`
- Publishing beyond capacity throws `BackpressureException`
- Backpressure metric increment in `stats()`
- Pool close terminates active consumer

- [ ] **Step 8: Create `BoundaryLimitsTest.php`**

Migrate `boundary_limits.phpt` content. Use Pest `dataset()` for:
- Batch limit: 256 messages max
- Cumulative payload: 1 MiB max
- Header count: 128 max
- Header byte size: 64 KiB max
- Header key size: 64 KiB max
- Timeout range: 1–86,400,000 ms

- [ ] **Step 9: Delete the 7 migrated PHPT files**

```bash
rm crates/rabbit-rs-php/tests/phpt/secrets.phpt
rm crates/rabbit-rs-php/tests/phpt/pid_registry.phpt
rm crates/rabbit-rs-php/tests/phpt/fork_invalidation.phpt
rm crates/rabbit-rs-php/tests/phpt/publisher_outcomes.phpt
rm crates/rabbit-rs-php/tests/phpt/backpressure.phpt
rm crates/rabbit-rs-php/tests/phpt/boundary_limits.phpt
```

Keep `extension_metadata.phpt` — it stays for ZTS-level verification.

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "feat: migrate extension, pool, and publisher PHPT tests to Pest

Migrate 7 PHPT files (secrets, pid_registry, fork_invalidation,
publisher_outcomes, backpressure, boundary_limits) to Pest with datasets.
extension_metadata.phpt retained for ZTS-level loading verification."
```

---

### Task 12: Migrate PHPT Tests to Pest (Part 2 — Consumer, Config, Reflection, Binary)

**Files:**
- Create: `crates/rabbit-rs-php/tests/Consumer/ConsumerTest.php`
- Create: `crates/rabbit-rs-php/tests/Config/ConfigValidationTest.php`
- Create: `crates/rabbit-rs-php/tests/Reflection/ReflectionTest.php`
- Create: `crates/rabbit-rs-php/tests/BinaryPayload/BinaryPayloadTest.php`
- Delete: `crates/rabbit-rs-php/tests/phpt/delivery_terminal_state.phpt`
- Delete: `crates/rabbit-rs-php/tests/phpt/config_validation.phpt`
- Delete: `crates/rabbit-rs-php/tests/phpt/reflection.phpt`
- Delete: `crates/rabbit-rs-php/tests/phpt/binary_payload.phpt`

- [ ] **Step 1: Read all 4 PHPT source files**

Read each PHPT file fully.

- [ ] **Step 2: Create `ConsumerTest.php`**

Migrate `delivery_terminal_state.phpt`. Test:
- Binary-safe payload delivery
- Metadata: message_id, correlation_id, attempts, headers (bool/int/float/null, nested headers omitted)
- ACK terminal state + double-ACK error
- Release/reject terminal state
- Consumer close error

Use `testingPool()` with 4 deliveries in the scenario.

- [ ] **Step 3: Create `ConfigValidationTest.php`**

Migrate `config_validation.phpt`. Test:
- Pool stats (pid, handle, no key/credentials exposed)
- Zero prefetch rejected with exact path
- Legacy max_in_flight rejected
- Recursive config rejected
- Resource config rejected
- Close idempotency

- [ ] **Step 4: Create `ReflectionTest.php`**

Migrate `reflection.phpt`. Test:
- `ReflectionClass` on `Pool`, `Consumer`, `Delivery`
- All classes are `final`
- Method signatures: parameter types, optionality, defaults
- Direct construction rejection

- [ ] **Step 5: Create `BinaryPayloadTest.php`**

Migrate `binary_payload.phpt`. Test:
- NUL bytes in payload pass through
- Oversized payload (1 MiB + 1) rejected
- Resource/object headers rejected
- Recursive headers rejected
- Invalid batch item rejected

- [ ] **Step 6: Delete the 4 migrated PHPT files**

```bash
rm crates/rabbit-rs-php/tests/phpt/delivery_terminal_state.phpt
rm crates/rabbit-rs-php/tests/phpt/config_validation.phpt
rm crates/rabbit-rs-php/tests/phpt/reflection.phpt
rm crates/rabbit-rs-php/tests/phpt/binary_payload.phpt
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat: migrate consumer, config, reflection, binary PHPT tests to Pest

Migrate 4 remaining PHPT files (delivery_terminal_state, config_validation,
reflection, binary_payload) to Pest. Only extension_metadata.phpt remains."
```

---

### Task 13: Update `scripts/test-extension.sh` for Pest

**Files:**
- Modify: `scripts/test-extension.sh`

**Interfaces:**
- Consumes: `vendor/bin/pest` from composer install
- Produces: Script that runs Pest tests + the single remaining PHPT

- [ ] **Step 1: Read the current `scripts/test-extension.sh`**

Read the full script to understand its structure: how it resolves PHP, builds the extension, and runs `run-tests.php`.

- [ ] **Step 2: Update the script to run Pest before PHPT**

After the extension is built (the existing build step), add:
1. Run `composer install --no-interaction` in `crates/rabbit-rs-php/` if `vendor/` doesn't exist
2. Run `vendor/bin/pest --parallel` from `crates/rabbit-rs-php/`
3. Then run the existing `php run-tests.php` step, but only for `tests/phpt/extension_metadata.phpt`

Update the PHPT file list/glob to only include `tests/phpt/extension_metadata.phpt` instead of all `*.phpt` files.

- [ ] **Step 3: Verify the script syntax is valid**

Run: `bash -n scripts/test-extension.sh`
Expected: No syntax errors

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: update test-extension.sh to run Pest + single PHPT

Script now runs vendor/bin/pest --parallel for the migrated Pest tests,
then php run-tests.php for the retained extension_metadata.phpt only."
```

---

## Phase 4: Laravel Pest Migration

### Task 14: Set Up Pest Infrastructure for Laravel Package

**Files:**
- Modify: `packages/laravel-queue/composer.json`
- Create: `packages/laravel-queue/tests/Pest.php`
- Delete: `packages/laravel-queue/phpunit.xml`

**Interfaces:**
- Consumes: `Orchestra\Testbench\TestCase`, `RabbitMqServiceProvider`
- Produces: `Pest.php` with Testbench integration and shared helpers

- [ ] **Step 1: Update `composer.json`**

Replace `phpunit/phpunit` with Pest dependencies:

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

Remove the `"scripts": {"test": "phpunit"}` entry or change it to `"test": "pest"`.

- [ ] **Step 2: Run `composer update`**

```bash
cd packages/laravel-queue && composer update
```

- [ ] **Step 3: Create `tests/Pest.php`**

```php
<?php

declare(strict_types=1);

use Goopil\RabbitRs\Laravel\Tests\TestCase;
use Goopil\RabbitRs\Laravel\RabbitMqServiceProvider;

uses(TestCase::class)->in(__DIR__);

// Helpers available to all Pest tests
function validConfig(): array
{
    return [
        'brokers' => [
            'orders_eu' => [
                'uri' => 'amqp://rabbit_rs:rabbit_rs_lab@127.0.0.1:5672/orders-eu',
                'vhosts' => ['/'],
            ],
        ],
        'routes' => [
            'default' => ['broker' => 'orders_eu'],
        ],
        'workers' => [
            'default' => [
                'broker' => 'orders_eu',
                'queue' => 'test-queue',
                'prefetch' => 16,
            ],
        ],
        'publishers' => [
            'default' => ['broker' => 'orders_eu'],
        ],
        'topology' => 'declare',
    ];
}
```

Note: The `TestCase` class extends `Orchestra\Testbench\TestCase` and registers `RabbitMqServiceProvider`. The `bootstrap.php` file that defines fake `Pool`/`Consumer`/`Delivery` classes must still be loaded — Pest autoloads it via composer's `autoload-dev`.

- [ ] **Step 4: Delete `phpunit.xml`**

```bash
rm packages/laravel-queue/phpunit.xml
```

- [ ] **Step 5: Verify Pest runs**

Run: `cd packages/laravel-queue && vendor/bin/pest`
Expected: PASS (0 tests or all skipped if no test files yet)

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: add Pest infrastructure for Laravel package

Replace phpunit/phpunit with pestphp/pest + pest-plugin-laravel.
Create Pest.php bootstrap with TestCase binding and validConfig() helper."
```

---

### Task 15: Migrate Unit + Feature PHPUnit Tests to Pest

**Files:**
- Create: 11 Pest files in `tests/Unit/` (replacing 11 PHPUnit files + `TestCase.php`)
- Create: 7 Pest files in `tests/Feature/` (replacing 7 PHPUnit files)
- Delete: 12 original Unit files (11 tests + `TestCase.php`)
- Delete: 7 original Feature files

**Interfaces:**
- Consumes: `TestCase::class` via `uses()` in `Pest.php`, fake `Pool`/`Consumer`/`Delivery` from `bootstrap.php`
- Produces: 18 Pest test files with `it()` / `describe()` syntax

**Migration approach per file:**
1. Read the PHPUnit test class
2. Convert `testFoo` methods to `it('foo', ...)`
3. Convert `setUp`/`tearDown` to `beforeEach`/`afterEach`
4. Convert `@dataProvider` / `#[DataProvider]` to `dataset()`
5. Convert `$this->` assertions to `expect()` or keep `PHPUnit\Framework\TestCase` methods via `uses()`
6. Convert `self::assertSame` to `expect()->toBe()` where natural
7. Keep the same test logic — no simplification

**Note on `TestCase.php`:** The `TestCase` class extends `Orchestra\Testbench\TestCase` and registers `RabbitMqServiceProvider`. In Pest, this is handled by `uses(TestCase::class)->in(__DIR__)` in `Pest.php`. The `TestCase.php` file itself is deleted — Pest uses the `uses()` binding instead.

**Note on `bootstrap.php`:** The `bootstrap.php` file defines fake `Goopil\RabbitRs\Pool`, `Consumer`, `Delivery` classes. It must remain — it's loaded via composer `autoload-dev`. Do NOT delete it.

- [ ] **Step 1: Read all 12 Unit test files (including `TestCase.php`)**

Read each file to understand its tests, helpers, and patterns.

- [ ] **Step 2: Read all 7 Feature test files**

Read each file.

- [ ] **Step 3: Migrate Unit tests — create 11 Pest files**

For each of the 11 Unit test files (excluding `TestCase.php`), create a Pest file in `tests/Unit/`:
- `ConfigNormalizerTest.php` — 449 lines, has `#[DataProvider]` → `dataset()`
- `RabbitMqQueuePublishTest.php` — 298 lines
- `RabbitMqJobTest.php` — 206 lines
- `WorkerSupervisorTest.php` — 154 lines
- `RabbitMqQueueCleanupTest.php` — 154 lines
- `RabbitMqQueueAdminTest.php` — 151 lines
- `RabbitMqConnectorTest.php` — 141 lines
- `RabbitMqServiceProviderTest.php` — 102 lines
- `MessageMapperTest.php` — 87 lines
- `RabbitMqQueuePopTest.php` — 113 lines
- `WorkerProfileResolverTest.php` — 70 lines

Keep file names identical — Pest discovers `*.php` files in the directory.

- [ ] **Step 4: Migrate Feature tests — create 7 Pest files**

- `MultiVhostWorkerTest.php` — 296 lines
- `RabbitMqWorkCommandTest.php` — 279 lines (uses Mockery)
- `WorkerSupervisorIntegrationTest.php` — 237 lines
- `OctaneLifecycleTest.php` — 211 lines
- `NativeEventDispatchTest.php` — 143 lines (uses `Event::fake()`)
- `OctaneLifecycleHooksTest.php` — 95 lines
- `RabbitMqStatusCommandTest.php` — 87 lines

- [ ] **Step 5: Delete original Unit files**

```bash
rm packages/laravel-queue/tests/Unit/TestCase.php
rm packages/laravel-queue/tests/Unit/ConfigNormalizerTest.php
rm packages/laravel-queue/tests/Unit/RabbitMqQueuePublishTest.php
rm packages/laravel-queue/tests/Unit/RabbitMqJobTest.php
rm packages/laravel-queue/tests/Unit/WorkerSupervisorTest.php
rm packages/laravel-queue/tests/Unit/RabbitMqQueueCleanupTest.php
rm packages/laravel-queue/tests/Unit/RabbitMqQueueAdminTest.php
rm packages/laravel-queue/tests/Unit/RabbitMqConnectorTest.php
rm packages/laravel-queue/tests/Unit/RabbitMqServiceProviderTest.php
rm packages/laravel-queue/tests/Unit/MessageMapperTest.php
rm packages/laravel-queue/tests/Unit/RabbitMqQueuePopTest.php
rm packages/laravel-queue/tests/Unit/WorkerProfileResolverTest.php
```

- [ ] **Step 6: Delete original Feature files**

```bash
rm packages/laravel-queue/tests/Feature/MultiVhostWorkerTest.php
rm packages/laravel-queue/tests/Feature/RabbitMqWorkCommandTest.php
rm packages/laravel-queue/tests/Feature/WorkerSupervisorIntegrationTest.php
rm packages/laravel-queue/tests/Feature/OctaneLifecycleTest.php
rm packages/laravel-queue/tests/Feature/NativeEventDispatchTest.php
rm packages/laravel-queue/tests/Feature/OctaneLifecycleHooksTest.php
rm packages/laravel-queue/tests/Feature/RabbitMqStatusCommandTest.php
```

- [ ] **Step 7: Run Pest tests in parallel**

Run: `cd packages/laravel-queue && vendor/bin/pest tests/Unit tests/Feature --parallel`
Expected: PASS — all migrated tests pass

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat: migrate Laravel Unit and Feature tests to Pest

Convert 18 PHPUnit test files (11 Unit + 7 Feature) to Pest DSL.
Tests run in parallel via --parallel. No test logic simplified."
```

---

### Task 16: Migrate Integration PHPUnit Tests to Pest

**Files:**
- Create: 3 Pest files in `tests/Integration/` (replacing 3 PHPUnit files + `IntegrationTestCase.php`)
- Delete: 4 original Integration files (3 tests + `IntegrationTestCase.php`)

**Interfaces:**
- Consumes: `TestCase::class`, real `Pool` from extension, RabbitMQ management API
- Produces: 3 Pest integration test files (sequential, no `--parallel`)

- [ ] **Step 1: Read all 4 Integration test files**

Read `IntegrationTestCase.php`, `QueueWorkerTest.php`, `DelayedJobTest.php`, `AtLeastOnceChaosTest.php`.

- [ ] **Step 2: Create `tests/Integration/Pest.php` or add to main `Pest.php`**

Add integration-specific helpers to the main `Pest.php`:
- `liveConfig(string $queueName): array` — from `IntegrationTestCase`
- `uniqueQueue(string $prefix): string` — from `IntegrationTestCase`
- `declareQueue(string $queueName): void` — from `IntegrationTestCase`
- `deleteQueue(string $queueName): void` — from `IntegrationTestCase`

Use `uses(TestCase::class)->in('tests/Integration')` and `beforeEach` for setup/teardown.

- [ ] **Step 3: Migrate 3 Integration test files to Pest**

- `QueueWorkerTest.php` — 140 lines, uses real Pool, setUp/tearDown with queue lifecycle
- `DelayedJobTest.php` — 93 lines, delay plugin routing + TTL bucket fallback
- `AtLeastOnceChaosTest.php` — 567 lines, chaos tests with Toxiproxy

These run sequentially (no `--parallel`).

- [ ] **Step 4: Delete original Integration files**

```bash
rm packages/laravel-queue/tests/Integration/IntegrationTestCase.php
rm packages/laravel-queue/tests/Integration/QueueWorkerTest.php
rm packages/laravel-queue/tests/Integration/DelayedJobTest.php
rm packages/laravel-queue/tests/Integration/AtLeastOnceChaosTest.php
```

- [ ] **Step 5: Run Pest integration tests (requires lab)**

Run: `cd packages/laravel-queue && vendor/bin/pest tests/Integration`
Expected: PASS if lab is running; SKIP if extension not loaded

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: migrate Laravel Integration tests to Pest

Convert 3 PHPUnit integration test files + IntegrationTestCase to Pest.
Integration tests run sequentially (real broker, shared queues)."
```

---

## Phase 5: PHP Benchmark Restructuring

### Task 17: Create AbstractBenchmark, Config, and Budget

**Files:**
- Create: `benchmarks/src/AbstractBenchmark.php`
- Create: `benchmarks/src/Config.php`
- Create: `benchmarks/src/Budget.php` (moved from `lib/Budget.php`)
- Create: `benchmarks/src/Driver.php` (moved from `drivers/Driver.php`)

**Interfaces:**
- Consumes: existing `benchmarks/lib/Budget.php` and `benchmarks/drivers/Driver.php`
- Produces: `AbstractBenchmark` base class, `Config` constants, `Budget` checker, `Driver` interface

- [ ] **Step 1: Create `benchmarks/src/Config.php`**

```php
<?php

declare(strict_types=1);

namespace Bench;

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
    public const EXCHANGE_DURABLE = true;
    public const QUEUE_NAME = 'benchmark_queue';
    public const QUEUE_DURABLE = true;
    public const ROUTING_KEY = 'benchmark.key';
}
```

- [ ] **Step 2: Create `benchmarks/src/AbstractBenchmark.php`**

Adapt from the reference repo. Include the `runBenchmark()` and `calculateStats()` methods. Also include the `createMessage()` helper and the stats column calculation. Merge the latency recording from the existing `Metrics` trait.

```php
<?php

declare(strict_types=1);

namespace Bench;

abstract class AbstractBenchmark
{
    protected array $latencies = [];

    abstract public function getName(): string;
    abstract public function setUp(): void;
    abstract public function tearDown(): void;
    abstract public function publishMessages(int $count): void;
    abstract public function consumeMessages(int $count): void;

    protected function createMessage(string $body): string
    {
        return json_encode([
            'id' => uniqid('', true),
            'timestamp' => microtime(true),
            'data' => $body,
            'payload' => str_repeat('x', Config::MESSAGE_PAYLOAD_BYTES),
        ]);
    }

    public function runBenchmark(): array
    {
        $results = [];
        for ($i = 0; $i < Config::BENCHMARK_ROUNDS; $i++) {
            $this->latencies = [];

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
                'p50' => $this->percentile(0.50),
                'p95' => $this->percentile(0.95),
                'p99' => $this->percentile(0.99),
            ];
        }
        return $this->calculateStats($results);
    }

    protected function recordLatency(float $ms): void
    {
        $this->latencies[] = $ms;
    }

    protected function percentile(float $p): float
    {
        if (empty($this->latencies)) {
            return 0.0;
        }
        $sorted = $this->latencies;
        sort($sorted);
        $index = (int) floor($p * count($sorted));
        return $sorted[min($index, count($sorted) - 1)];
    }

    private function calculateStats(array $results): array
    {
        $get = fn(string $key) => array_column($results, $key);
        $avg = fn(array $vals) => array_sum($vals) / count($vals);

        $publishTimes = $get('publish_time');
        $consumeTimes = $get('consume_time');
        $publishRates = $get('publish_rate');
        $consumeRates = $get('consume_rate');

        return [
            'name' => $this->getName(),
            'publish' => [
                'avg_time' => $avg($publishTimes),
                'min_time' => min($publishTimes),
                'max_time' => max($publishTimes),
                'avg_rate' => $avg($publishRates),
                'min_rate' => min($publishRates),
                'max_rate' => max($publishRates),
                'p99' => $avg($get('p99')),
            ],
            'consume' => [
                'avg_time' => $avg($consumeTimes),
                'min_time' => min($consumeTimes),
                'max_time' => max($consumeTimes),
                'avg_rate' => $avg($consumeRates),
                'min_rate' => min($consumeRates),
                'max_rate' => max($consumeRates),
                'p50' => $avg($get('p50')),
                'p95' => $avg($get('p95')),
            ],
        ];
    }
}
```

- [ ] **Step 3: Move `Budget.php` to `src/Budget.php`**

Read `benchmarks/lib/Budget.php`, update namespace to `Bench`, move to `benchmarks/src/Budget.php`.

- [ ] **Step 4: Move `Driver.php` to `src/Driver.php`**

Read `benchmarks/drivers/Driver.php`, update namespace to `Bench`, move to `benchmarks/src/Driver.php`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: create AbstractBenchmark, Config, and move Budget/Driver

Create AbstractBenchmark base class with runBenchmark + stats.
Create Config with centralized constants. Move Budget and Driver to
src/ namespace."
```

---

### Task 18: Create Benchmark Drivers (4 drivers)

**Files:**
- Create: `benchmarks/src/Drivers/RabbitRsDriver.php`
- Create: `benchmarks/src/Drivers/AmqplibDriver.php`
- Create: `benchmarks/src/Drivers/AmqpExtDriver.php`
- Create: `benchmarks/src/Drivers/BunnyDriver.php`
- Delete: `benchmarks/drivers/RabbitRsDriver.php`
- Delete: `benchmarks/drivers/PhpAmqplibDriver.php`
- Delete: `benchmarks/drivers/AmqpExtDriver.php`
- Delete: `benchmarks/drivers/` (directory)

**Interfaces:**
- Consumes: `AbstractBenchmark`, `Config`, existing driver implementations
- Produces: 4 driver classes extending `AbstractBenchmark`

**Approach:** The existing drivers implement the `Driver` interface with `setup/publish/consume/reset/teardown/metrics/name`. The new pattern extends `AbstractBenchmark` with `setUp/tearDown/publishMessages/consumeMessages/getName`. Adapt each driver to the new base class. The existing `Metrics` trait methods (`recordLatency`, `percentile`) are now in `AbstractBenchmark` — drivers call `$this->recordLatency()`.

- [ ] **Step 1: Read existing 3 driver files fully**

Read `RabbitRsDriver.php`, `PhpAmqplibDriver.php`, `AmqpExtDriver.php` to understand their setup, publish, consume, and metrics logic.

- [ ] **Step 2: Create `RabbitRsDriver.php` extending `AbstractBenchmark`**

Adapt the existing driver:
- `setup()` → `setUp()`: create `Pool` with config, declare exchange/queue, purge queue
- `publish(array $messages, $safety)` → `publishMessages(int $count)`: build messages with `hrtime(true)` timestamp, batch publish up to 256, flush
- `consume(int $count)` → `consumeMessages(int $count)`: `Consumer::next(1000)` loop, record latency, ACK, break after 3 nulls
- `teardown()` → `tearDown()`: close pool
- `name()` → `getName()`: return `'rabbit-rs'`

Use the `safest` mode (confirms + mandatory) for the `batch-confirm` scenario. The scenario class selects the mode.

- [ ] **Step 3: Create `AmqplibDriver.php` extending `AbstractBenchmark`**

Adapt `PhpAmqplibDriver.php`:
- Two connections (publisher + consumer)
- `confirm_select()` for batch-confirm scenario
- `wait_for_pending_acks()` after publishing
- `basic_consume` + `wait()` loop for consuming

- [ ] **Step 4: Create `AmqpExtDriver.php` extending `AbstractBenchmark`**

Adapt the existing `AmqpExtDriver.php`:
- Single `\AMQPConnection`
- `confirmSelect()` for batch-confirm
- `$queue->get()` poll loop for consuming

- [ ] **Step 5: Create `BunnyDriver.php` extending `AbstractBenchmark`**

New driver, based on the reference repo's pattern. Use `Bunny\Client`:
- `setUp()`: create `Client`, connect, create channel, declare exchange/queue, bind, purge
- `publishMessages()`: `$channel->publish()` loop with timestamp prepend
- `consumeMessages()`: `$channel->consume()` callback, record latency, ACK
- `tearDown()`: close client

- [ ] **Step 6: Delete old driver files and directory**

```bash
rm benchmarks/drivers/RabbitRsDriver.php
rm benchmarks/drivers/PhpAmqplibDriver.php
rm benchmarks/drivers/AmqpExtDriver.php
rmdir benchmarks/drivers
```

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat: create 4 benchmark drivers extending AbstractBenchmark

Adapt RabbitRs, Amqplib, AmqpExt drivers to AbstractBenchmark pattern.
Add new BunnyDriver. Remove old Driver-interface-based drivers."
```

---

### Task 19: Create Benchmark Scenarios and Runner

**Files:**
- Create: `benchmarks/src/Scenarios/FireAndForgetBenchmark.php`
- Create: `benchmarks/src/Scenarios/BatchConfirmBenchmark.php`
- Create: `benchmarks/src/Scenarios/AutoAckBenchmark.php`
- Create: `benchmarks/src/run-benchmarks.php`

**Interfaces:**
- Consumes: `AbstractBenchmark`, `Config`, driver classes
- Produces: 3 scenario classes + unified runner with `--scenario=` and `--driver=` filters

**Approach:** Each scenario class extends `AbstractBenchmark` and delegates to a driver. The driver provides the raw publish/consume implementation; the scenario configures the safety mode (fire-and-forget = no confirms, batch-confirm = confirms + waitForConfirms, auto-ack = no_ack consumer).

The runner auto-detects available drivers (`extension_loaded('rabbit_rs')`, `class_exists(AMQPStreamConnection::class)`, `extension_loaded('amqp')`, `class_exists(Bunny\Client::class)`) and skips unavailable ones.

- [ ] **Step 1: Create `FireAndForgetBenchmark.php`**

This scenario delegates to a driver with no confirms and no mandatory flag. For RabbitRs, uses 100ms timeout.

- [ ] **Step 2: Create `BatchConfirmBenchmark.php`**

This scenario uses confirms + waitForConfirms. For RabbitRs, uses `publishBatch()` + flush. For Amqplib, uses `confirm_select()` + `wait_for_pending_acks()`.

- [ ] **Step 3: Create `AutoAckBenchmark.php`**

This scenario uses `no_ack=true` consumer. For RabbitRs, the consumer doesn't ACK (the extension auto-acks based on the consumer profile config).

- [ ] **Step 4: Create `run-benchmarks.php`**

```php
#!/usr/bin/env php
<?php

declare(strict_types=1);

require_once __DIR__ . '/../../vendor/autoload.php';

use Bench\Config;
use Bench\Budget;
use Bench\Drivers;

// Parse args
$scenarioFilter = null;
$driverFilter = null;
foreach (array_slice($argv, 1) as $arg) {
    if (str_starts_with($arg, '--scenario=')) {
        $scenarioFilter = substr($arg, strlen('--scenario='));
    }
    if (str_starts_with($arg, '--driver=')) {
        $driverFilter = substr($arg, strlen('--driver='));
    }
}

// Detect available drivers
$drivers = [];
if (extension_loaded('rabbit_rs')) {
    $drivers['rabbit-rs'] = Drivers\RabbitRsDriver::class;
}
if (class_exists(\PhpAmqpLib\Connection\AMQPStreamConnection::class)) {
    $drivers['amqplib'] = Drivers\AmqplibDriver::class;
}
if (extension_loaded('amqp')) {
    $drivers['amqp-ext'] = Drivers\AmqpExtDriver::class;
}
if (class_exists(\Bunny\Client::class)) {
    $drivers['bunny'] = Drivers\BunnyDriver::class;
}

// ... runner logic: iterate scenarios × drivers, run benchmarks, print table, write JSON
```

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: create benchmark scenarios and unified runner

3 scenarios (fire-and-forget, batch-confirm, auto-ack) + run-benchmarks.php
with --scenario= and --driver= filters. Auto-detects available drivers."
```

---

### Task 20: Create Laravel Benchmarks and Docker Compose

**Files:**
- Create: `benchmarks/laravel/LaravelSmokeBenchmark.php`
- Create: `benchmarks/laravel/LaravelCompareBenchmark.php`
- Create: `benchmarks/docker-compose.yml`
- Create: `benchmarks/run-benchmarks.sh`

**Interfaces:**
- Consumes: `Goopil\RabbitRs\Laravel\*` classes, `AbstractBenchmark`
- Produces: Laravel-level benchmark scripts + Docker Compose for standalone benchmark runs

- [ ] **Step 1: Create `docker-compose.yml`**

```yaml
services:
  rabbitmq:
    image: rabbitmq:3.13-management
    container_name: rabbitmq-benchmark
    hostname: rabbitmq-benchmark
    ports:
      - "5672:5672"
      - "15672:15672"
    environment:
      RABBITMQ_DEFAULT_USER: guest
      RABBITMQ_DEFAULT_PASS: guest
      RABBITMQ_DEFAULT_VHOST: /
      RABBITMQ_SERVER_ADDITIONAL_ERL_ARGS: "-rabbit loopback_users []"
    volumes:
      - rabbitmq_data:/var/lib/rabbitmq
    healthcheck:
      test: ["CMD", "rabbitmq-diagnostics", "-q", "ping"]
      interval: 2s
      timeout: 2s
      retries: 30
      start_period: 5s

volumes:
  rabbitmq_data: {}
```

- [ ] **Step 2: Create `LaravelSmokeBenchmark.php`**

Adapt the existing `laravel-smoke.php` (182 lines) into a class extending `AbstractBenchmark`:
- `setUp()`: build config, normalize, create Pool, RabbitMqConnector, queue
- `publishMessages(int $count)`: `$queue->push('stdClass', ['index' => $i], $queueName)`
- `consumeMessages(int $count)`: `$queue->pop($queueName)` loop, ACK job
- `tearDown()`: close pool

- [ ] **Step 3: Create `LaravelCompareBenchmark.php`**

Adapt the existing `laravel-compare.php` (365 lines) into a class that benchmarks through the Laravel queue layer across 3 drivers (rabbit-rs, php-amqplib, vyuldashev).

- [ ] **Step 4: Create `run-benchmarks.sh`**

```bash
#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

# Start RabbitMQ if not running
if ! docker compose ps --status running | grep -q rabbitmq-benchmark; then
    echo "Starting RabbitMQ..."
    docker compose up -d --wait
fi

# Install deps if needed
if [ ! -d vendor ]; then
    composer install --no-interaction
fi

# Run benchmarks
php src/run-benchmarks.php "$@"
```

- [ ] **Step 5: Delete old benchmark scripts**

```bash
rm benchmarks/smoke.php
rm benchmarks/compare.php
rm benchmarks/laravel-smoke.php
rm benchmarks/laravel-compare.php
rm benchmarks/lib/Metrics.php
rm benchmarks/lib/Budget.php
rmdir benchmarks/lib
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: create Laravel benchmarks, Docker Compose, and runner script

LaravelSmokeBenchmark and LaravelCompareBenchmark extend AbstractBenchmark.
Add single-node docker-compose.yml for standalone benchmark runs."
```

---

### Task 21: Update Benchmark composer.json and CI Scripts

**Files:**
- Modify: `benchmarks/composer.json`
- Modify: `scripts/test-integration.sh`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: new benchmark structure
- Produces: updated composer.json, CI integration

- [ ] **Step 1: Update `benchmarks/composer.json`**

```json
{
    "name": "rabbit-rs/benchmarks",
    "description": "Standalone PHP benchmarks for rabbit-rs",
    "license": "MIT",
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

- [ ] **Step 2: Update `scripts/test-integration.sh`**

Find the line that invokes `php benchmarks/smoke.php` and replace with:
```bash
php benchmarks/src/run-benchmarks.php --scenario=fire-and-forget --driver=rabbit-rs
```

Find the line that invokes `php benchmarks/laravel-smoke.php` and replace with:
```bash
php benchmarks/laravel/LaravelSmokeBenchmark.php
```

- [ ] **Step 3: Update `.github/workflows/ci.yml`**

In the `php` job, replace `phpunit --testsuite="Rabbit RS Laravel"` with:
```yaml
run: cd packages/laravel-queue && vendor/bin/pest tests/Unit tests/Feature --parallel
```

In the `phpt` job, the `./scripts/test-extension.sh` call is unchanged (the script itself was updated in Task 13).

In the `integration` job, update the benchmark invocation to use the new runner.

- [ ] **Step 4: Verify `composer validate --strict` passes**

Run: `rtk composer validate --strict`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: update benchmark composer.json and CI scripts

Restructure autoload to Bench\\ namespace, add bunny/bunny dep.
Update test-integration.sh and ci.yml for new benchmark runner path."
```

---

## Final Verification

- [ ] **Step 1: Run full quality gate**

Run: `rtk ./scripts/check.sh`
Expected: PASS — `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace --all-targets`, `composer validate --strict`

- [ ] **Step 2: Verify Rust test consolidation**

Run: `rtk cargo test --workspace --all-targets 2>&1 | rg "Running|test result"`
Expected: 6 consolidated test targets (publisher, consumer, recovery, topology, metrics, integration) + chaos (integration-gated) + inline tests

- [ ] **Step 3: Verify no Criterion references remain**

Run: `rg criterion crates/`
Expected: No matches

- [ ] **Step 4: Verify no old benchmark files remain**

Run: `ls benchmarks/smoke.php benchmarks/compare.php benchmarks/laravel-smoke.php benchmarks/laravel-compare.php benchmarks/drivers/ benchmarks/lib/ 2>&1`
Expected: "No such file or directory" for all

- [ ] **Step 5: Verify PHPT count**

Run: `ls crates/rabbit-rs-php/tests/phpt/*.phpt`
Expected: Only `extension_metadata.phpt`

- [ ] **Step 6: Final commit (if any cleanup needed)**

```bash
git add -A
git commit -m "chore: final cleanup after test and benchmark simplification"
```

---

## Self-Review Checklist

**1. Spec coverage:**
- [x] Part 1: Rust consolidation 17→6 — Tasks 2–8
- [x] Part 2: PHPT → Pest — Tasks 10–13
- [x] Part 3: Laravel PHPUnit → Pest — Tasks 14–16
- [x] Part 4: Criterion decommission — Task 1
- [x] Part 5: PHP benchmark restructuring — Tasks 17–21
- [x] CI changes — Task 21
- [x] Scripts changes — Tasks 13, 21

**2. Placeholder scan:** No TBD, TODO, or vague steps found.

**3. Type consistency:** `testingPool()` signature consistent across Pest.php and all test files. `AbstractBenchmark` method names (`setUp`, `tearDown`, `publishMessages`, `consumeMessages`, `getName`) consistent across all driver and scenario classes.
