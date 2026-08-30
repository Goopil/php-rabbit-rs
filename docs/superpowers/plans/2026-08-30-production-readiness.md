# Production Readiness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the blocking defects identified by the August 30, 2026 audit and bring the Rabbit RS ecosystem (Rust core, PHP extension, Laravel package) to production-ready / v1.0 level.

**Architecture:** Three layers: `rabbit-rs-core` (independent Rust crate), `rabbit-rs-php` (ext-php-rs extension), `goopil/rabbit-rs-laravel` (Laravel driver). Each task respects layer separation: the core knows nothing about PHP, the extension only carries owned values, the Laravel package accesses the native layer only through the stubs API.

**Tech Stack:** Rust 1.96 (edition 2024, Tokio, Lapin 4.10, flume), ext-php-rs 0.15.15, PHP 8.4/8.5, Laravel 12/13, Pest, Orchestra Testbench, Docker Compose (3-node RabbitMQ lab).

**Source audit:** evaluation of August 30, 2026 (see `docs/plans/ROADMAP.md`, Round F, for the per-layer summary and maturity notes).

> **Reconciliation 2026-08-30 (after PR #35 / post-pump merge on main):** the
> multi-broker consumer composition landed on main (Phase D, commit `585c534`,
> composed `ConsumerHandle` in `consumer/composite.rs`, per-broker
> `ConsumerSetHandle` in `consumer/set.rs`) — the former Task 8 is marked delivered.
> Pump v2 changed the publisher paths (`publish_blind` now at
> `publisher/actor.rs:229`, budget at `publisher/mod.rs:245`). Task 1 serves as
> hypothesis #1 for the Round 2 P1 investigation (ack-pipeline stall,
> `docs/plans/2026-08-30-consumer-stall-and-reliability.md`). The agreed execution
> order: Round 2 (with Task 1) → Tasks 2-6 (P0) → Round C → Tasks 7-14 (P1) →
> Round E → Round D.

## Global Constraints

- Rust 1.96, edition 2024, `#![forbid(unsafe_code)]` — never unsafe, never weaken the workspace lints.
- TDD mandatory for any behavior change: test written first, observed failing, minimal implementation, re-run.
- No real sleeps in Rust tests: paused Tokio time (`#[tokio::test(start_paused = true)]`) + scriptable mock transport.
- No Zend value, PHP object, callback, or Laravel container state held in a Rust thread.
- At-least-once delivery: no silent loss; duplicates are permitted, identifiable, and measurable.
- Secrets (credentials, full URI, certificates) never leak into `Debug`, errors, metrics, or logs.
- Before closing each task: `rtk cargo fmt --all` then `rtk ./scripts/check.sh` green (full quality gate).
- PHP: Pest (not PHPUnit), `declare(strict_types=1)`, the Laravel package Unit/Feature tests run **without** the extension.
- One logical commit per green task, conventional message (`feat:`/`fix:`/`test:`/`docs:`/`ci:`/`chore:`).

---

## Milestone P0 — Production blockers (correctness and safety)

### Task 1: Make the consumer error channel non-blocking (drop-oldest)

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (11 sites: lines 461, 471, 492, 502, 513, 544, 653, 666, 744, 774, 998)
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs:218` (flume channel construction)
- Test: `crates/rabbit-rs-core/tests/consumer.rs`

**Interfaces:**
- Produces: `ActorState::record_settlement_error(&mut self, error: SettlementError)` — private internal method, no public API changes.

**Context:** `error_tx` is a `flume::bounded(256)` but the actor uses the **blocking** send (`state.error_tx.send(...)`). If PHP never calls `drain_errors()`, after 256 settlement errors the consumer actor blocks its thread: no more dispatch, no more settlement. The `drain_errors` doc (`set.rs:309-311`) claims a drop-oldest behavior that does not exist.

- [x] **Step 1: Write the failing test**

Add at the end of `crates/rabbit-rs-core/tests/consumer.rs` (reuses the module-level helpers `subscription`, `connection_key`, `delivery` already present, cf. `settlement_error_surfaces_via_drain_errors` line 1473):

```rust
#[tokio::test(start_paused = true)]
async fn settlement_errors_never_stall_the_actor_when_never_drained() {
    let transport = MockTransport::default();

    // 300 deliveries, each acknowledged with a failing ack. Each failure
    // produces a SettlementError; 300 > 256 (error channel capacity).
    for tag in 1..=300u64 {
        transport.push_delivery(Ok(delivery(tag, b"payload")));
        transport.push_consumer_result(Ok(())); // set_qos
        transport.push_consumer_result(Ok(())); // consume
        transport.push_consumer_result(Err(TransportError::connection("ack-failure")));
    }

    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await;
    let handle = ConsumerSet::spawn_with_metrics(vec![subscription], Metrics::default())
        .await
        .unwrap();

    // Consume and acknowledge the 300 messages without ever draining the errors.
    for tag in 1..=300u64 {
        let delivery = handle.next().await.expect("delivery must keep flowing");
        assert_eq!(delivery.inner_token().delivery_tag(), tag);
        handle
            .try_settle(delivery.inner_token().clone(), Settlement::Ack)
            .expect("settle enqueued");
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
    }

    // The actor did not stall: the error buffer is full but bounded.
    let errors = handle.drain_errors();
    assert_eq!(errors.len(), 256, "oldest errors dropped, newest kept");
    assert_eq!(errors.last().expect("last error").delivery_tag, 300);

    let _ = handle.close().await;
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer settlement_errors_never_stall`
Expected: FAIL (stall timeout or error length ≠ 256) — the blocking send freezes the actor.

- [x] **Step 3: Write minimal implementation**

In `crates/rabbit-rs-core/src/consumer/actor.rs`, add to `ActorState` a cloned receiver (flume allows cloning; the bounded capacity applies to queued messages, not to the receiver count) and the helper method:

```rust
/// Records a settlement error without ever blocking the actor.
///
/// The error channel is bounded (256). When full, the oldest error is
/// dropped to make room — the actor must never stall waiting for the PHP
/// side to drain, matching the documented contract of
/// `ConsumerHandle::drain_errors`.
fn record_settlement_error(&mut self, error: SettlementError) {
    if self.error_tx.is_full() {
        let _ = self.error_rx.try_recv();
    }
    let _ = self.error_tx.send(error);
}
```

Then replace the 11 occurrences of `let _ = state.error_tx.send(SettlementError { ... });` with `state.record_settlement_error(SettlementError { ... });`.

In the `ActorState` construction (same file), keep a clone of the `error_rx` used by `ConsumerHandle`:

```rust
let (error_tx, error_rx) = flume::bounded::<SettlementError>(ERROR_CHANNEL_CAPACITY);
// The actor keeps its own receiver for drop-oldest.
```

- [x] **Step 4: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core --test consumer`
Expected: PASS (all consumer tests, including the new one).

- [x] **Step 5: Run the full quality gate and commit**

Run: `rtk cargo fmt --all && rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add crates/rabbit-rs-core
git commit -m "fix(core): make consumer settlement error channel non-blocking with drop-oldest"
```

---

### Task 2: Bound the PHP extension publish buffer

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs` (constants lines 30-32, `publish_buffer` line 46, `publish()` lines 103-134, re-buffer lines 420-425)
- Modify: `crates/rabbit-rs-php/src/classes/exception.rs:35` (helper)
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` (`publish()` docblock)
- Test: `packages/` — no; extension Pest tests via `crates/rabbit-rs-php/tests/` (Pest, `extension-tests` feature)

**Interfaces:**
- Consumes: `conversion::NativePublish { broker: String, request: PublishRequest }` (`crates/rabbit-rs-php/src/conversion.rs:87-88`), payload accessible via `publish.request.payload.len()`.
- Produces: `Goopil\RabbitRs\BackpressureException` error when the buffer is full — contract documented in the stub.

**Context:** `publish_buffer: std::sync::Mutex<Vec<NativePublish>>` (`pool.rs:46`) grows without a ceiling: every failed flush re-buffers its messages (`pool.rs:420-425`). In a prolonged outage with sustained traffic, unbounded memory growth on the PHP process side (the core's 64 MiB budget does not bound this application buffer).

- [x] **Step 1: Write the failing test**

Add to the extension Pest suite (lifecycle tests file, cf. existing structure `crates/rabbit-rs-php/tests/` — the `testing_pool()` mock is injected via the `extension-tests` feature):

```php
<?php

use Goopil\RabbitRs\BackpressureException;
use Goopil\RabbitRs\testing_pool;

it('raises backpressure when the publish buffer is full and cannot flush', function () {
    $pool = testing_pool()->with_blocked_transport();

    // PUBLISH_BUFFER_MAX_MESSAGES = 4096; beyond that, publish() refuses.
    $message = ['broker' => 'default', 'exchange' => 'jobs', 'routing_key' => 'jobs',
        'payload' => str_repeat('x', 64)];

    $messageIds = [];
    for ($i = 0; $i < 4096; $i++) {
        $messageIds[] = $pool->publish($message);
    }

    expect(fn () => $pool->publish($message))
        ->toThrow(BackpressureException::class);
});
```

Adapt the mock helper name to the existing pattern of the extension Pest tests (the test pool exposes a blocked transport to force the flush failure — cf. `crates/rabbit-rs-php/src/testing.rs` for the actual mock API).

- [x] **Step 2: Run test to verify it fails**

Run: `rtk ./scripts/test-extension.sh`
Expected: FAIL — either the test has no blocked transport, or `publish()` never reaches `BackpressureException` (unbounded buffer).

- [x] **Step 3: Write minimal implementation**

In `crates/rabbit-rs-php/src/classes/exception.rs`, add next to `client_exception` (line 35):

```rust
pub(crate) fn backpressure_exception<T>(message: &str) -> PhpResult<T> {
    Err(PhpException::from_class::<BackpressureException>(
        message.to_owned(),
    ))
}
```

In `crates/rabbit-rs-php/src/classes/pool.rs`, add the constants:

```rust
/// Maximum number of buffered publish requests before flushing is forced.
const PUBLISH_BUFFER_MAX_MESSAGES: usize = 4096;
/// Maximum cumulative buffered payload bytes before flushing is forced.
const PUBLISH_BUFFER_MAX_BYTES: usize = 64 * 1024 * 1024;
```

Add to the `Pool` struct a bounded byte counter `publish_buffer_bytes: std::sync::Mutex<usize>` (initialized to 0 in `__construct`), maintained on every push/re-buffer/drain of the buffer.

In `publish()` (after conversion, before the push), check capacity:

```rust
let payload_bytes = publish.request.payload.len();
let mut buffer = self.publish_buffer.lock().expect("publish buffer mutex poisoned");

let at_capacity = buffer.len() >= PUBLISH_BUFFER_MAX_MESSAGES
    || *self.publish_buffer_bytes.lock().expect("publish buffer bytes mutex poisoned")
        + payload_bytes
        > PUBLISH_BUFFER_MAX_BYTES;

if at_capacity {
    drop(buffer);
    self.flush()?; // attempt to make room
    let mut buffer = self.publish_buffer.lock().expect("publish buffer mutex poisoned");
    let bytes = *self.publish_buffer_bytes.lock().expect("publish buffer bytes mutex poisoned");
    if buffer.len() >= PUBLISH_BUFFER_MAX_MESSAGES
        || bytes + payload_bytes > PUBLISH_BUFFER_MAX_BYTES
    {
        return backpressure_exception(&format!(
            "publish buffer is full ({} messages, {} buffered bytes); retry after flush",
            buffer.len(),
            bytes,
        ));
    }
}
buffer.push(publish);
```

Maintain the byte counter at both points where the buffer changes: `publish()` (push) and the failed-flush re-buffer (`pool.rs:420-425`). The re-buffering of **already accepted** messages is allowed to exceed capacity (they already received a `message_id` — dropping them would be a silent loss); in that case new `publish()` calls receive `BackpressureException` until the buffer drops back below the ceiling.

- [x] **Step 4: Update the stub docblock**

In `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`, section `publish()`:

```php
/**
 * Publishes one message, returning its stable message identifier.
 * ...
 * @throws \Goopil\RabbitRs\BackpressureException when the bounded publish
 *   buffer is full (outage with sustained traffic); retry with the same
 *   message later. Already-buffered messages are never dropped.
 */
```

- [x] **Step 5: Run tests and check benchmark non-regression**

Run: `rtk ./scripts/test-extension.sh && rtk ./scripts/check.sh`
Expected: PASS.

The buffer ceiling touches the publish hot path: run the publish scenario of the
driver-level bench (Phase E, blind + safe modes) and compare against the frozen budget
(`benchmarks/results/benchmark-results.json`, cf. initial Task 40 plan):

Run: `cd benchmarks/driver-bench && (see README § run) ./run.sh --smoke rabbit-rs`
Expected: throughput within the archives variance (`runs/phase-e/`) — no
regression > 5%.

- [x] **Step 6: Commit**

```bash
git add crates/rabbit-rs-php
git commit -m "fix(php-ext): bound the publish buffer with explicit backpressure"
```

---

### Task 3: Deadline and timeout on the consumer wait

**Files:**
- Modify: `crates/rabbit-rs-core/src/config.rs` (new `ConsumerConfigSection` section)
- Modify: `crates/rabbit-rs-core/src/client.rs:330-410` (wait loop in `consumer()`, now composed per broker)
- Modify: `crates/rabbit-rs-core/src/pool/key.rs` (fingerprint)
- Modify: `crates/rabbit-rs-php/src/conversion.rs` (config mapping) and `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`
- Modify: `packages/laravel-queue/config/rabbit-rs.php` + `packages/laravel-queue/src/Config/ConfigNormalizer.php`
- Test: `crates/rabbit-rs-core/tests/consumer.rs` + `packages/laravel-queue/tests/Unit/ConfigNormalizerTest.php`

**Interfaces:**
- Produces: `Config` gains `consumer: ConsumerConfigSection { wait_timeout: Duration }` (serde `consumer.wait_timeout`, default 30 s, validated bounded 1 s..=24 h); expiry → `ClientError` of kind `ClientErrorKind::Transport` — mapped to `ConnectionException` on the PHP side (cf. `client_exception` in `crates/rabbit-rs-php/src/classes/exception.rs:35`).

**Context:** `ClientPool::consumer()` (`client.rs:330+`) loops indefinitely when the coordinator never leaves `Connecting`↔`Recovering` (black-holed broker, no connect timeout): FPM workers can freeze with no escape hatch. Since the multi-broker composition (PR #35), the `wait_for_state` wait loop sits inside the per-broker composition loop (≈ lines 371-410) — the deadline must wrap the whole acquisition, across all sources.

- [ ] **Step 1: Write the failing test (core)**

In `crates/rabbit-rs-core/tests/consumer.rs` (or a new `consumer_wait_deadline.rs` file):

```rust
#[tokio::test(start_paused = true)]
async fn consumer_wait_deadline_expires_when_the_broker_never_becomes_ready() {
    let transport = MockTransport::default();
    // No connect result pushed: the connect gate stays closed forever.
    let _gate = transport.push_connect_gate();

    // Build a valid base config then inject the short timeout,
    // like the existing client.rs tests (cf. the worker profile construction
    // in the unit tests of crates/rabbit-rs-core/src/client.rs).
    let base = Config {
        brokers: vec![helper::broker("b", "/")],
        workers: vec![worker_profile_with_subscription("main", "b", "main.jobs")],
        topology_mode: TopologyMode::Declare,
        delay: rabbit_rs_core::config::DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
        publisher: PublisherConfigSection::default(),
        queue_type: QueueKind::Quorum,
        queue_durable: true,
        consumer: rabbit_rs_core::config::ConsumerConfigSection {
            wait_timeout: Duration::from_millis(500),
        },
    };

    let pool = rabbit_rs_core::pool::ClientPool::new(
        Arc::new(base.validate().expect("valid config")),
        Arc::new(MockTransport::default()),
    );

    let started = tokio::time::Instant::now();
    let result = pool.consumer("main").await;

    let error = result.expect_err("must not wait forever");
    assert!(
        matches!(error.kind(), rabbit_rs_core::pool::ClientErrorKind::Transport),
        "deadline expiry must surface as a typed transport error: {error:?}"
    );
    assert_eq!(started.elapsed(), Duration::from_millis(500));
}
```

Adapt the worker profile construction to the existing helper (the `client_pool` tests in `crates/rabbit-rs-core/src/client.rs` show the full construction).

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core consumer_wait_deadline`
Expected: FAIL — the `consumer` field does not exist (compilation) then the loop never terminates.

- [ ] **Step 3: Implement the config section**

In `crates/rabbit-rs-core/src/config.rs`:

```rust
/// Consumer acquisition settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub struct ConsumerConfigSection {
    /// Maximum wall-clock time PHP waits for a consumer handle to become
    /// ready (connection + topology + basic_consume) before a typed error
    /// is returned. Prevents unbounded blocking on black-holed brokers.
    #[serde(with = "humantime_serde")]
    pub wait_timeout: std::time::Duration,
}

impl Default for ConsumerConfigSection {
    fn default() -> Self {
        Self { wait_timeout: std::time::Duration::from_secs(30) }
    }
}
```

Add `pub consumer: ConsumerConfigSection` to the `Config` struct (with `#[serde(default)]`) and the matching field in `ValidatedConfig`. Validation: `wait_timeout` bounded `1 s..=24 h` with `ConfigError` at path `consumer.wait_timeout`. Update `ConnectionKey::from_config` / `ConfigFingerprint` to include the value.

Update the `Config { ... }` literals of test helpers (`tests/consumer.rs::helper::connection_key` and other sites) by adding `consumer: ConsumerConfigSection::default(),`.

- [ ] **Step 4: Bound the acquisition**

In `crates/rabbit-rs-core/src/client.rs::consumer()`, wrap the whole acquisition (the per-broker composition and its `wait_for_state` loops, ≈ lines 355-410):

```rust
let wait_timeout = self.config.consumer.wait_timeout;
let consumer = tokio::time::timeout(wait_timeout, async {
    // ... existing loop unchanged (coordinator.consumer / wait_for_state /
    // is_closed / FailedPermanent / Closed)
})
.await
.map_err(|_elapsed| {
    ClientError::transport(&TransportError::connection(format!(
        "consumer profile '{profile}' did not become ready within {wait_timeout:?}"
    )))
})??;
```

With paused Tokio time, `timeout` respects `advance()` — the test stays deterministic.

- [ ] **Step 5: Wire through PHP and Laravel**

- `crates/rabbit-rs-php/src/conversion.rs`: map the `consumer.wait_timeout` key (integer ms, optional) to the native config.
- `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`: document the config key.
- `packages/laravel-queue/config/rabbit-rs.php`: add `'wait_timeout' => 30_000` under a `consumers` section (ms).
- `packages/laravel-queue/src/Config/ConfigNormalizer.php`: validate (int > 0, ≤ 86 400 000) and map to `consumer.wait_timeout`.

- [ ] **Step 6: Write the failing Laravel test, then pass it**

In `packages/laravel-queue/tests/Unit/ConfigNormalizerTest.php`:

```php
it('maps consumer wait_timeout to the native config', function () {
    $config = validConfig(['consumers' => ['wait_timeout' => 5_000]]);
    $native = (new Goopil\RabbitRs\Laravel\Config\ConfigNormalizer)->normalize($config);

    expect($native['consumer']['wait_timeout'])->toBe(5_000);
});

it('rejects a consumer wait_timeout outside the 1s..24h bound', function () {
    validConfig(['consumers' => ['wait_timeout' => 0]]);
})->throws(Goopil\RabbitRs\Laravel\Exceptions\ConfigurationException::class);
```

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/ConfigNormalizerTest.php`
Expected: FAIL then PASS after the Step 5 implementation.

- [ ] **Step 7: Full gate and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add crates packages
git commit -m "feat(core): bound consumer acquisition wait with a configurable deadline"
```

---

### Task 4: Loud failures on unreadable TLS files

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs:351-372` (`build_tls_config`, `build_tls_identity`)
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs` (caller of `build_tls_config` in `connect()`)
- Test: `crates/rabbit-rs-core/tests/transport_tuning.rs` (or new `tls_errors.rs`)

**Context:** `fs::read_to_string(path).ok()` and `fs::read(path).ok()?` silently fall back when a CA cert or a client cert/key pair is unreadable: the connection starts without the intended CA (silently degraded security). `TlsVerify::None` and `server_name` (SNI) remain validated but unwired fields — deferred to Task 12 with real TLS integration; this task guarantees that no configured TLS file can be silently ignored.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn unreadable_tls_files_fail_loudly_instead_of_connecting_unprotected() {
    let mut broker = helper::broker("tls-b", "/");
    broker.tls = rabbit_rs_core::config::TlsConfig {
        enabled: true,
        server_name: None,
        ca_cert: Some(std::path::PathBuf::from("/nonexistent/ca.pem")),
        client_cert: None,
        client_key: None,
        verify: rabbit_rs_core::config::TlsVerify::Peer,
    };

    let transport = rabbit_rs_core::transport::lapin::LapinTransport::default();
    let error = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(transport.connect(&broker))
        .expect_err("unreadable CA cert must fail loudly");

    assert!(
        error.to_string().contains("/nonexistent/ca.pem"),
        "error must identify the exact file path: {error}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core unreadable_tls_files_fail_loudly`
Expected: FAIL — the current error is a network connection error (CA ignored), not an error identifying the file.

- [ ] **Step 3: Write minimal implementation**

In `crates/rabbit-rs-core/src/transport/lapin.rs`:

```rust
fn build_tls_config(config: &BrokerConfig) -> TransportResult<lapin::tcp::OwnedTLSConfig> {
    let tls = &config.tls;
    let identity = build_tls_identity(tls)?;
    let cert_chain = match tls.ca_cert() {
        Some(path) => Some(
            std::fs::read_to_string(path).map_err(|error| {
                TransportError::config(format!(
                    "tls.ca_cert: cannot read '{}': {error}",
                    path.display()
                ))
            })?,
        ),
        None => None,
    };

    Ok(lapin::tcp::OwnedTLSConfig { identity, cert_chain })
}

fn build_tls_identity(
    tls: &crate::config::TlsConfig,
) -> TransportResult<Option<lapin::tcp::OwnedIdentity>> {
    let (Some(cert_path), Some(key_path)) = (tls.client_cert(), tls.client_key()) else {
        return Ok(None);
    };

    let pem = std::fs::read(cert_path).map_err(|error| {
        TransportError::config(format!(
            "tls.client_cert: cannot read '{}': {error}",
            cert_path.display()
        ))
    })?;
    let key = std::fs::read(key_path).map_err(|error| {
        TransportError::config(format!(
            "tls.client_key: cannot read '{}': {error}",
            key_path.display()
        ))
    })?;

    Ok(Some(lapin::tcp::OwnedIdentity::PKCS8 { pem, key }))
}
```

Adapt the caller in `connect()` to propagate `TransportResult` (`?`). Verify that `TransportError` exposes a config variant (`config(...)`) — otherwise add a `Configuration { message }` variant in `transport.rs` following the existing typed error style.

- [ ] **Step 4: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core unreadable_tls_files_fail_loudly && rtk cargo test -p rabbit-rs-core`
Expected: PASS.

- [ ] **Step 5: Full gate and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add crates/rabbit-rs-core
git commit -m "fix(core): fail loudly on unreadable TLS certificate files"
```

---

### Task 5: Horizon — honor after-commit and wire bulk()

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php:261,289` (`prepareBatch` and `publishBatch`: `private` → `protected`)
- Modify: `packages/laravel-queue/src/Horizon/RabbitMqQueue.php` (push/later via `enqueueUsing`, override `prepareBatch`)
- Test: `packages/laravel-queue/tests/Feature/HorizonAfterCommitTest.php` (new)

**Context:** In Horizon mode, `push()`/`later()` bypass `enqueueUsing` (they call `createPayload` + `pushRaw`/`laterRawFromPayload` directly): `after_commit` is ignored — jobs get published while the SQL transaction is not yet committed (transactional job loss). `bulk()` is not overridden: bulk jobs without `JobPayload::prepare()` nor Horizon events, invisible to the dashboard.

- [ ] **Step 1: Write the failing test**

`packages/laravel-queue/tests/Feature/HorizonAfterCommitTest.php` (with the existing bootstrap Horizon fakes):

```php
<?php

use Goopil\RabbitRs\Laravel\Horizon\RabbitMqQueue;
use Illuminate\Support\Facades\DB;
use Laravel\Horizon\Events\JobPushed;

it('defers Horizon job publication until the transaction commits', function () {
    $queue = $this->app->make('queue')->connection('rabbit-rs-horizon');
    expect($queue)->toBeInstanceOf(RabbitMqQueue::class);

    $published = [];
    $queue->swapNativePool(function (array $message) use (&$published) {
        $published[] = $message['payload'];
        return $message;
    });

    DB::transaction(function () use ($queue) {
        dispatch(new Fixtures\CommitJob)->onConnection('rabbit-rs-horizon');
        expect($published)->toBeEmpty('job must not be published inside the transaction');
    });

    expect($published)->toHaveCount(1);
});

it('pushes Horizon bulk jobs with prepared payloads and events', function () {
    $queue = $this->app->make('queue')->connection('rabbit-rs-horizon');
    Event::fake([JobPushed::class]);

    $queue->bulk([new Fixtures\BulkJob, new Fixtures\BulkJob], '', 'bulk');

    Event::assertDispatchedTimes(JobPushed::class, 2);
});
```

(The `swapNativePool` mechanism is an example: use the existing native mock pattern from `tests/bootstrap.php` — `Pool`/`Consumer` fakes faithful to the contract.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Feature/HorizonAfterCommitTest.php`
Expected: FAIL — the job is published inside the transaction (after_commit ignored) and bulk does not trigger the events.

- [ ] **Step 3: Make the base batch helpers overridable**

In `packages/laravel-queue/src/RabbitMqQueue.php`, replace `private function prepareBatch(...)` (line 261) and `private function publishBatch(...)` (line 289) with `protected function`. No signature or logic change.

- [ ] **Step 4: Rewrite the Horizon push/later/bulk path**

In `packages/laravel-queue/src/Horizon/RabbitMqQueue.php`:

```php
public function push($job, $data = '', $queue = null)
{
    $queueName = $this->queueName($queue);

    return $this->enqueueUsing(
        $job,
        (new JobPayload($this->createPayload($job, $queueName, $data)))->prepare($job)->value,
        $queue,
        null,
        fn (string $payload, ?string $queue): string => $this->publishHorizonPayload($payload, $queue),
    );
}

public function later($delay, $job, $data = '', $queue = null)
{
    $queueName = $this->queueName($queue);

    return $this->enqueueUsing(
        $job,
        (new JobPayload($this->createPayload($job, $queueName, $data, $delay)))->prepare($job)->value,
        $queue,
        $delay,
        fn (string $payload, ?string $queue, mixed $delay): string => $this->publishHorizonPayload(
            $payload, $queue, $this->delayMilliseconds($delay),
        ),
    );
}

protected function prepareBatch(array $jobs, mixed $data, mixed $queue): array
{
    return array_map(function (array $prepared) use ($queue) {
        $payload = (new JobPayload($prepared['payload']))->prepare($prepared['job'])->value;
        $this->event($this->queueName($queue), new JobPending($payload));

        return [...$prepared, 'payload' => $payload];
    }, parent::prepareBatch($jobs, $data, $queue));
}

private function publishHorizonPayload(string $payload, ?string $queue, ?int $delayMs = null): string
{
    $queueName = $this->queueName($queue);

    $result = $delayMs === null
        ? $this->publish($payload, $queue, ['content_type' => self::CONTENT_TYPE_JSON])
        : $this->publish($payload, $queue, ['content_type' => self::CONTENT_TYPE_JSON], $delayMs);

    $this->event($queueName, new JobPushed($payload));

    return $result;
}
```

Remove the `$lastPushed` property and the old overridden `pushRaw` (the Horizon payload is now prepared at the `push`/`later`/`prepareBatch` level, before `enqueueUsing`, so the callback published at commit already carries the prepared payload).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Feature/HorizonAfterCommitTest.php && php vendor/bin/pest`
Expected: PASS (new test + no regression on the existing suite including Horizon tests H1-H6).

- [ ] **Step 6: Full gate and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add packages/laravel-queue
git commit -m "fix(laravel): honor after-commit and Horizon events for push, later and bulk"
```

---

### Task 6: Poison-message warning on permissive defaults

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqServiceProvider.php` (boot)
- Test: `packages/laravel-queue/tests/Feature/PoisonMessageWarningTest.php` (new)

**Context:** By default `topology.queue.delivery_limit => null` and `topology.dead_letter => null` (`config/rabbit-rs.php:329-331`): a message that crashes the worker before settlement is redelivered forever. The protection is opt-in with no signal. We do not impose a new default (breaking change) but we warn in production.

- [ ] **Step 1: Write the failing test**

```php
<?php

use Illuminate\Support\Facades\Log;

it('warns when delivery_limit and dead_letter are both unset in production', function () {
    config(['queue.connections.rabbit-rs.production_warning' => true]);
    Log::shouldReceive('warning')->once()->withArgs(
        fn (string $message) => str_contains($message, 'delivery_limit') && str_contains($message, 'dead_letter'),
    );

    $this->app->make('queue')->connection('rabbit-rs');
});

it('does not warn when delivery_limit is configured', function () {
    config([
        'queue.connections.rabbit-rs.production_warning' => true,
        'queue.connections.rabbit-rs.topology.queue.delivery_limit' => 20,
    ]);
    Log::shouldReceive('warning')->never();

    $this->app->make('queue')->connection('rabbit-rs');
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Feature/PoisonMessageWarningTest.php`
Expected: FAIL — no warning emitted.

- [ ] **Step 3: Write minimal implementation**

In `packages/laravel-queue/src/RabbitMqServiceProvider.php::boot()`, at the first resolution of a `rabbit-rs` connection (via the connector, once per config fingerprint):

```php
if (
    ($config['topology']['queue']['delivery_limit'] ?? null) === null
    && ($config['topology']['dead_letter'] ?? null) === null
    && (bool) ($config['production_warning'] ?? true)
    && $this->app->environment('production')
) {
    Log::warning(
        'rabbit-rs: delivery_limit and dead_letter are both unset for this connection. '
        .'A poison message (worker crash before settlement) will be redelivered forever. '
        .'Set topology.queue.delivery_limit with topology.dead_letter, or silence this '
        .'with production_warning => false.'
    );
}
```

Trigger the warning only once per process (static flag or shared connector property). The `production_warning` key (default `true`) is added to `config/rabbit-rs.php` with a comment.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Feature/PoisonMessageWarningTest.php && php vendor/bin/pest`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add packages/laravel-queue
git commit -m "feat(laravel): warn on unbounded redelivery defaults in production"
```

---

## Milestone P1 — Hardening

### Task 7: Lazy consumer establishment for requested profiles only

**Files:**
- Modify: `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:406` (`recover_generation`)
- Modify: `crates/rabbit-rs-core/src/client.rs` (registry of requested profiles)
- Test: `crates/rabbit-rs-core/tests/consumer.rs` (new test)

**Context:** `recover_generation` loops over **all** `worker_profiles()` of the config (`recovery_coordinator.rs:406`): a purely publishing process that declares worker profiles opens channels + `basic_consume` on all queues at each reconnection and holds unacked messages (up to prefetch per queue) — invisible blocking of queues and pointless redeliveries.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(start_paused = true)]
async fn only_requested_worker_profiles_are_consumed() {
    // Config with two worker profiles: "main" (queue main.jobs) and
    // "side" (queue side.jobs), on the same mock broker.
    let transport = MockTransport::default();
    // ... pool construction via the existing client.rs helpers ...

    // The process only requests the "main" profile.
    let _handle = pool.consumer("main").await.expect("main consumer");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let operations = transport.operations();
    let consumed_queues: Vec<&str> = operations.iter().filter_map(|operation| match operation {
        TransportOperation::Consume { queue, .. } => Some(queue.as_str()),
        _ => None,
    }).collect();

    assert!(consumed_queues.contains(&"main.jobs"), "requested profile consumed: {consumed_queues:?}");
    assert!(
        !consumed_queues.contains(&"side.jobs"),
        "unrequested profile must not be consumed: {consumed_queues:?}"
    );
}
```

(Verify the exact `TransportOperation` variant for `basic_consume` in `crates/rabbit-rs-core/src/transport/mock.rs` and adapt the pattern matching.)

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core only_requested_worker_profiles`
Expected: FAIL — `side.jobs` is consumed despite no request.

- [ ] **Step 3: Write minimal implementation**

1. In `crates/rabbit-rs-core/src/client.rs`, add `requested_profiles: std::sync::Mutex<std::collections::HashSet<String>>` to `ClientPool`. `consumer(profile)` inserts the profile into the set **before** triggering the coordinators.
2. Share the set with each coordinator (passed at construction, `Arc<Mutex<HashSet<String>>>`).
3. In `recover_generation` (`recovery_coordinator.rs:406`), filter `worker_profiles()`: only process the profiles present in the requested set. A profile added after a reconnection is established at the next `coordinator.consumer(profile)` call (the `client.consumer()` wait loop already retries).

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo test -p rabbit-rs-core && rtk ./scripts/check.sh`
Expected: PASS (watch out for existing tests that expected eager establishment — adapt them if their intent is preserved).

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core
git commit -m "feat(core): lazily establish consumers only for requested worker profiles"
```

---

### Task 8: Multi-broker consumer composition — DELIVERED ON MAIN

**Status: completed upstream.** Delivered by the post-pump Phase D (PR #35, commit
`585c534` "compose multi-broker consumers from all coordinators"):

- `crates/rabbit-rs-core/src/consumer/composite.rs` — `pub struct ConsumerHandle`:
  the composed handle that merges deliveries from multi-broker sources with fair
  selection and routes each settlement to its source broker.
- `crates/rabbit-rs-core/src/consumer/set.rs:284` — `pub struct ConsumerSetHandle`:
  the per-broker handle (renamed from the old `ConsumerHandle`).
- `ClientPool::consumer()` (`client.rs:330`) now returns the composed handle.
- Documented semantics: `docs/` — commit `39ced65` "document multi-broker
  consumer semantics".

**Adequacy check against the audit:** the identified gap ("only the 1st
broker is consumed, `client.rs:414-417`") no longer exists. The multi-broker test
planned by this task remains relevant as a non-regression: if a similar scenario
is desired, write it on top of `composite.rs` and the existing tests
(`tests/consumer.rs`, composite section). No code to write for this task.

---

### Task 9: Measure delivery duplicates

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (dispatch path, where `attempts` is resolved)
- Modify: `crates/rabbit-rs-core/src/metrics.rs:145-147` (no signature change — wiring only)
- Test: `crates/rabbit-rs-core/tests/consumer.rs` (new test)

**Context:** `record_duplicate()` (`metrics.rs:145`) is never called: `duplicate_count` is always 0 while the project contract requires duplicates to be "identifiable and measurable". The snapshot exposes a dead counter, misleading for operations.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test(start_paused = true)]
async fn redelivered_messages_are_counted_as_duplicates() {
    let transport = MockTransport::default();
    let mut redelivered = helper::delivery(1, b"payload");
    redelivered.redelivered = true;
    transport.push_delivery(Ok(redelivered));
    transport.push_delivery(Ok(delivery(2, b"fresh")));

    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await;
    let handle = ConsumerSet::spawn_with_metrics(vec![subscription], Metrics::default())
        .await
        .unwrap();

    let _ = handle.next().await.unwrap();
    let _ = handle.next().await.unwrap();

    let snapshot = handle.metrics_snapshot();
    assert_eq!(snapshot.duplicate_count, 1, "one redelivery counted");
    assert_eq!(snapshot.deliveries_total, 2, "both deliveries counted");

    let _ = handle.close().await;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core redelivered_messages_are_counted`
Expected: FAIL — `duplicate_count == 0`.

- [ ] **Step 3: Write minimal implementation**

In the dispatch path of `consumer/actor.rs` (where `attempts` is resolved via `AttemptsResolver`), after resolution: if `attempts > 1` (redelivered flag, `x-acquired-count`, or `x-delivery-count` > 1 — the exact source is already centralized in `consumer/attempts.rs`), call `self.metrics.record_duplicate()` (the actor's shared `Metrics`). One call per redelivered delivery.

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo test -p rabbit-rs-core && rtk ./scripts/check.sh`
Expected: PASS. Complete the test assertions (`duplicate_count == 1`, `deliveries_total == 2`).

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core
git commit -m "feat(core): count redelivered messages as duplicates in metrics"
```

---

### Task 10: Drain native events from publish() and next()

**Files:**
- Create: `crates/rabbit-rs-php/src/classes/bridge.rs`
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs` (move callbacks/states to the bridge)
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs` (trigger the bridge in `next()`/`tryNext()`/`nextBatch()`)
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php` + README (drain on pop)
- Test: `crates/rabbit-rs-php/tests/` (Pest) + `packages/laravel-queue/tests/Feature/NativeEventDispatchTest.php`

**Context:** The `onConnectionState`/`onBackpressure` callbacks are only invoked during `stats()` (`pool.rs:263-264`) — yet the driver never calls `stats()` in normal operation. `ConnectionStateChanged`/`BackpressureDetected` are ineffective in production while the README (`packages/laravel-queue/README.md:17`) and `docs/operations.md:231` promise the opposite.

- [ ] **Step 1: Write the failing test (extension)**

```php
it('invokes connection state callbacks during publish and consume without stats()', function () {
    $pool = testing_pool()->with_failing_transport(); // transport that dies

    $states = [];
    $pool->onConnectionState(function (string $broker, string $state, int $generation) use (&$states) {
        $states[] = [$broker, $state, $generation];
    });

    $pool->publish([...]);
    expect($states)->not->toBeEmpty('callback must fire on publish, not only on stats()');
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk ./scripts/test-extension.sh`
Expected: FAIL — `$states` empty without `stats()`.

- [ ] **Step 3: Extract an EventBridge**

`crates/rabbit-rs-php/src/classes/bridge.rs`:

```rust
/// Shared event bridge: owns the PHP callbacks and last-seen state so both
/// `Pool` (publish path) and `Consumer` (pop path) can drain native events
/// on the PHP thread. Callbacks are invoked only on the PHP thread, never
/// from a Rust thread; mutexes are released before invocation.
pub(crate) struct EventBridge {
    connection_state_callback: CallbackSlot,
    backpressure_callback: CallbackSlot,
    last_connection_states: std::sync::Mutex<HashMap<String, (String, i64)>>,
    last_backpressure_total: std::sync::Mutex<u64>,
    client: std::sync::Weak<ClientPool>,
}
```

Move `invoke_connection_state_callbacks` and `invoke_backpressure_callbacks` (currently `Pool` methods, `pool.rs:443-505`) to `EventBridge` as `Arc<EventBridge>` implementations. `Pool` and `Consumer` hold `Arc<EventBridge>` (the `Consumer::new` constructor gains a `bridge: Arc<EventBridge>` parameter — update all call sites). Invariant preserved: mutexes released before callback invocation (anti-deadlock, cf. `callbacks.rs:1-24` and `CallbackDeadlockTest.php`).

Trigger `bridge.drain(...)`:
- in `Pool::publish()` and `Pool::publishBatch()` (after flush),
- in `Consumer::next()`/`tryNext()`/`nextBatch()` (before blocking on the wait),
- still in `stats()` (existing behavior).

- [ ] **Step 4: Wire the Laravel driver**

In `packages/laravel-queue/src/RabbitMqQueue.php::pop()`, before `next()`: no PHP-side change needed (the extension drains natively); however **fix the docs**: `README.md:17` and `docs/operations.md:231` become accurate with this behavior — verify and adjust the wording ("events fire during publish and consume operations").

- [ ] **Step 5: Run tests to verify they pass**

Run: `rtk ./scripts/test-extension.sh && cd packages/laravel-queue && php vendor/bin/pest`
Expected: PASS (including `CallbackDeadlockTest` and `NativeEventDispatchTest`).

- [ ] **Step 6: Full gate and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add crates/rabbit-rs-php packages/laravel-queue
git commit -m "feat(php-ext): drain native events on publish and consume paths"
```

---

### Task 11: Bound the Blind mode in bytes

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:229` (`publish_blind`, post-pump v2)
- Modify: `crates/rabbit-rs-core/src/publisher/pump.rs` (blind pump intake)
- Test: `crates/rabbit-rs-core/tests/blind_pump.rs`

**Context:** `publish_blind` acquires neither semaphore nor byte budget: the memory bound is a message count (1024 intake + 2048 in-flight). 1024 payloads of 50 MB pass — inconsistent with Safe/Unsafe which bound both count and bytes. The `with_byte_budget` builder already exists (`publisher/mod.rs:245`) for confirmed modes — reuse it.

- [ ] **Step 1: Write the failing test**

In `crates/rabbit-rs-core/tests/blind_pump.rs`:

```rust
#[tokio::test(start_paused = true)]
async fn blind_publish_respects_the_byte_budget() {
    // Pump capacity reduced for the test; payload size deliberately
    // > budget_bytes / capacity.
    let pump = BlindPump::spawn_with_budget(/* budget_bytes: */ 1024 * 1024, /* capacity: */ 4);

    let oversized = vec![0u8; 512 * 1024];
    // 3 x 512 KiB = 1.5 MiB > 1 MiB: the 4th publish must be rejected.
    for _ in 0..3 {
        pump.try_publish_blind(/* request with oversized payload */).expect("within budget");
    }
    let error = pump.try_publish_blind(/* request */).expect_err("over byte budget");
    assert!(matches!(error, PublishError::Backpressure { .. }));
}
```

(Adapt to the actual constructors of `publisher/pump.rs` and the `PublishRequest`/`PublishError` types.)

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core blind_publish_respects_the_byte_budget`
Expected: FAIL — the 4th publish is accepted.

- [ ] **Step 3: Write minimal implementation**

Add an atomic byte budget to the blind pump (same semantics as the confirmed-mode budget — reuse the `with_byte_budget` builder from `publisher/mod.rs:245`): incremented at intake (`checked_add`, overflow → `Backpressure`), decremented at transport exit. Apply in `publish_blind` (`publisher/actor.rs:229`) BEFORE inserting into the intake flume.

- [ ] **Step 4: Run tests and check benchmark non-regression**

Run: `rtk cargo test -p rabbit-rs-core && rtk ./scripts/check.sh`
Expected: PASS.

Blind mode is the fastest publish path — verify non-regression on the fire-and-forget
scenario of the driver bench (compare against the `runs/phase-e/` archives, 5%
tolerance).

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core
git commit -m "fix(core): enforce byte budget on blind publish pump"
```

---

### Task 12: Integration TLS — SNI and verify, TLS lab

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs` (`server_name` → SNI, `verify` → verification mode)
- Modify: `lab/rabbitmq/` (TLS profile: self-signed certificates, amqps ports 5675+)
- Modify: `crates/rabbit-rs-core/tests/` (new `tls_integration.rs`, `integration` feature)
- Modify: `crates/rabbit-rs-core/src/config.rs` (`TlsVerify`/`server_name` docblock: documented contract)

**Context:** `TlsVerify::None` and `effective_server_name()` (`config.rs:194`) are validated and hashed into the fingerprint but never read by the transport (`lapin.rs:351-372`). Changing `verify`/`server_name` changes the fingerprint → new pool → unchanged behavior. No real TLS test exists.

**API decision:** Lapin 4.10 with rustls only consumes `OwnedTLSConfig { identity, cert_chain }`; SNI and verification disabling require a custom TLS connector (`lapin::tcp::TLSBackend` / `ConnectionProperties::with_ssl` or injected rustls connector). Verify the exact lapin 4.10 API in `~/.cargo/registry/src/*/lapin-4.10*/src/tcp/` before implementing; if lapin does not allow injecting a custom `rustls::ClientConfig`, implement:
1. `verify = Peer` (default): default rustls behavior (hostname verification = `effective_server_name()`). **Explicitly document in the stub and `config.rs` that verification uses the first host's name when `server_name` is absent.**
2. `verify = None`: not supported in V1 → explicit `ConfigError` "tls.verify: 'none' requires a custom TLS connector, not yet supported" (instead of being silently ignored). Remove from the exposed Laravel surface if unwired.

- [ ] **Step 1: Write the failing integration tests**

`crates/rabbit-rs-core/tests/tls_integration.rs` (marked `#[cfg(feature = "integration")]`, lab required):

```rust
#[tokio::test]
async fn tls_handshake_succeeds_against_the_lab_certificate() {
    // Broker configured in amqps with the TLS profile self-signed CA.
    // Connect, open a publisher channel, publish with confirms enabled.
    // Assert: Confirmed.
}

#[tokio::test]
async fn tls_handshake_fails_against_an_untrusted_ca() {
    // Same broker but different CA (wrong-trust CA file).
    // Assert: typed connection error, no cleartext message.
}

#[tokio::test]
async fn server_name_overrides_sni() {
    // Certificate issued for 'rabbit.internal'; hosts = ['127.0.0.1'];
    // tls.server_name = 'rabbit.internal'. Assert: handshake succeeds.
    // Reverse variant: server_name = 'wrong.host' → handshake fails.
}
```

- [ ] **Step 2: Add the TLS profile to the lab**

- `lab/rabbitmq/compose.yaml`: `with-tls` profile — ports `5675-5677` in amqps, certificate volumes.
- Generate the certificates (self-signed CA + server cert SAN `rabbit.internal`, `127.0.0.1`) via a `lab/rabbitmq/tls/generate.sh` script (openssl, pinned by image digest or local openssl), keep generated PEMs `.gitignore`d out of the repo (generated at `lab-up`).
- `scripts/lab-ready.sh`: verify amqps listening.
- `scripts/test-integration.sh`: include the TLS profile when `--with-tls`.

- [ ] **Step 3: Implement and verify**

Implement SNI/verify per the API decision above (and the explicit `ConfigError` for unsupported `verify: none`). Run: `rtk cargo test -p rabbit-rs-core --features integration --test tls_integration && ./scripts/test-integration.sh --with-tls`
Expected: PASS.

- [ ] **Step 4: Full gate and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add crates lab scripts
git commit -m "feat(core): wire TLS SNI and verify with lab integration tests"
```

---

### Task 13: End-to-end PIE validation

**Files:**
- Modify: `scripts/package-pie-binary.sh:273` (naming)
- Modify: `.github/workflows/release.yml:161` (naming — single source of truth)
- Create: `.github/workflows/verify-pie.yml` or a job in `release.yml`
- Test: run CI on a draft release

**Context:** Naming inconsistency: `package-pie-binary.sh` produces `...-linux-glibc-nts.zip` (with `-nts` suffix) while `release.yml` produces `...-linux-glibc.zip` (without suffix — this is what is published in v0.0.7). PIE asset resolution depends on the naming pattern; the chain has never been validated by a real `pie install`.

- [ ] **Step 1: Determine the PIE-expected naming empirically**

On a local machine with PIE 1.5+ and PHP 8.4:

```bash
pie download goopil/rabbit-rs-native@0.0.7 --dry-run 2>&1 || true
# then manually download the v0.0.7 asset and install locally:
gh release download v0.0.7 -p 'php_rabbit_rs-v0.0.7_php8.4-x86_64-linux-glibc.zip' -D /tmp/pie-test
pie install /tmp/pie-test/php_rabbit_rs-v0.0.7_php8.4-x86_64-linux-glibc.zip
php -m | grep rabbit_rs
```

If `pie install` resolves the expected suffix (via the documented PIE naming convention: `php_rabbit_rs-1.2.0_php8.5-x86_64-linux-glibc-nts.zip`), document the mandatory pattern. Is the name **without** the `-nts` suffix resolved by PIE? If yes, the current v0.0.7 pattern is correct; if not, fix it.

- [ ] **Step 2: Unify the naming**

Based on the Step 1 result, fix `scripts/package-pie-binary.sh` OR `.github/workflows/release.yml` so that **both produce exactly the same pattern**, documented in `docs/distribution.md` with a convention test (script `scripts/verify-pie-naming.sh` verifying that every asset of the matrix matches the pattern expected by PIE).

- [ ] **Step 3: Add the CI verification job**

In `release.yml` (after the packaging job, before publication):

```yaml
verify-pie-install:
  name: PIE install end-to-end (glibc NTS x86_64 PHP 8.4)
  runs-on: ubuntu-latest
  needs: [build]
  steps:
    - uses: actions/checkout@v4
    - uses: php/pie-setup-action@v1
    - name: Download the drafted NTS asset
      run: gh release download "${{ needs.build.outputs.tag }}" -p '*php8.4-x86_64-linux-glibc*.zip' -D ./pie-assets
      env:
        GH_TOKEN: ${{ github.token }}
    - name: Install via PIE and smoke-test
      run: |
        pie install ./pie-assets/*.zip
        php -m | grep rabbit_rs
        php -r "echo phpversion('rabbit_rs'), PHP_EOL;"
```

(Extend the job to also test musl and ZTS if PIE allows it on the runner — at minimum NTS glibc blocking.)

- [ ] **Step 4: Verify on a draft release**

Tag `v0.0.8` (or similar), run the full release workflow, verify the `verify-pie-install` job is green, then `pie install` locally from the published release.

- [ ] **Step 5: Commit**

```bash
git add scripts .github docs/distribution.md
git commit -m "ci: validate PIE asset resolution end-to-end with unified naming"
```

---

### Task 14: Complete the Laravel contracts (ClearableQueue, auto-subscribe)

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php:31` (implements)
- Modify: `packages/laravel-queue/src/RabbitMqQueue.php:359-371` (pop fallback)
- Modify: `packages/laravel-queue/src/Support/WorkerProfileResolver.php`
- Modify: `packages/laravel-queue/config/rabbit-rs.php`
- Test: `packages/laravel-queue/tests/Unit/RabbitMqQueueAdminTest.php` + new `AutoSubscribeTest.php`

**Context:** (1) `queue:clear rabbit-rs` fails because `ClearableQueue` is not declared although `clear()` exists. (2) `pop()` throws if the requested queue is not a subscription of a worker profile — major deviation from the Laravel convention (`queue:work --queue=emails`).

- [ ] **Step 1: Write the failing tests**

```php
// tests/Unit/ClearableQueueTest.php
it('implements ClearableQueue so queue:clear works', function () {
    expect($this->app->make('queue')->connection('rabbit-rs'))
        ->toBeInstanceOf(Illuminate\Contracts\Queue\ClearableQueue::class);
});

// tests/Feature/AutoSubscribeTest.php
it('pops a plain queue by auto-subscribing when enabled', function () {
    config(['queue.connections.rabbit-rs.auto_subscribe' => true]);
    // bootstrap native fakes: pop('emails') must create/reuse an
    // implicit profile containing the 'emails' subscription
    $job = $this->app->make('queue')->connection('rabbit-rs')->pop('emails');
    // assert: the native consumer was requested with a dedicated 'emails' profile
});

it('rejects a plain queue without auto_subscribe', function () {
    config(['queue.connections.rabbit-rs.auto_subscribe' => false]);
    $this->app->make('queue')->connection('rabbit-rs')->pop('emails');
})->throws(RuntimeException::class);
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/ClearableQueueTest.php tests/Feature/AutoSubscribeTest.php`
Expected: FAIL.

- [ ] **Step 3: Write minimal implementation**

1. `class RabbitMqQueue extends Queue implements QueueContract, ClearableQueue` (`Illuminate\Contracts\Queue\ClearableQueue`).
2. `pop($queue, $index = 0)`: if the `queue` value is neither a known profile nor a subscription, and `auto_subscribe => true`: build on the fly an implicit profile `{name: "__auto__", subscriptions: [{broker: default, queue: $queue, weight: 1, prefetch: default}]}` (process-local cache per queue name, reused on subsequent pops) and continue the existing path. If `auto_subscribe => false`: keep the current error with an improved message ("configure workers.*.subscriptions.*.queue=emails or enable auto_subscribe").
3. `config/rabbit-rs.php`: `'auto_subscribe' => false` (opt-in, documented).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd packages/laravel-queue && php vendor/bin/pest && rtk ./scripts/check.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add packages/laravel-queue
git commit -m "feat(laravel): implement ClearableQueue and optional auto-subscribe pop"
```

---

## Milestone P2 — Ecosystem and DX

### Task 15: Log facade, typed errors, panic audit

**Files:**
- Modify: `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:47-66` (typed `CoordinatorError`) and `:256,295` (expect/eprintln)
- Modify: `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:169-187` (`wait_for_state` panic → error)
- Modify: `crates/rabbit-rs-core/Cargo.toml` (`log` dependency)
- Modify: `crates/rabbit-rs-php/src/lib.rs` (init of a minimal logger toward `error_log` when `debug` config)
- Test: `crates/rabbit-rs-core/tests/recovery.rs` (adaptation) + documented manual audit

**Context:** Non-capturable `eprintln!` in prod, `CoordinatorError = String`, `.expect("connection actor started")` in a spawned task and a documented panic in `wait_for_state`: in a PHP FFI context, an uncaught unwind is a process abort.

- [ ] **Step 1: Typify CoordinatorError**

Replace `pub type CoordinatorError = String;` with:

```rust
#[derive(Debug)]
pub enum CoordinatorError {
    Topology(crate::topology::TopologyPlanError),
    Transport(TransportError),
    Internal(&'static str),
}
```

Adapt the construction sites. The reason `String`s become structured messages carried by the variants.

- [ ] **Step 2: Remove panics reachable from PHP**

- `run_coordinator:256`: `.expect("connection actor started")` → error propagation (`CoordinatorError::Transport`) and clean task termination with a log.
- `wait_for_state:169-187`: return `Result<(), CoordinatorError>` when the watch channel dies instead of panicking; callers handle the error.
- `eprintln!(recovery_coordinator.rs:295)`: replace with `log::warn!("recovery generation {generation} failed: {error}")`.

- [ ] **Step 3: Full audit of reachable panics**

Run: `rtk cargo test --workspace 2>&1 | true; rg -n 'unwrap\(\)|expect\(|panic!\(todo' crates/rabbit-rs-core/src crates/rabbit-rs-php/src --type rust`
For each `expect`/`unwrap` reachable from a PHP operation (FFI boundary): either documented justification in a comment (proven invariant), or conversion to a typed error. Document the list in `docs/reliability.md` ("Panic policy" section).

- [ ] **Step 4: Wire the log facade to PHP**

Add `log = "0.4"` to the core (no subscriber — the core imposes no backend). In the extension, `MINIT`/first use: install a minimal logger (crate `env_logger` or custom writer) that routes to PHP `error_log()` when the `debug => true` key is present in the Pool config; otherwise no-op. No zval captured in the logger.

- [ ] **Step 5: Verify and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add crates
git commit -m "refactor(core): typed coordinator errors, no reachable panics, log facade"
```

---

### Task 16: Align versions and introduce the CHANGELOG

**Files:**
- Modify: `packages/laravel-queue/src/RabbitMqServiceProvider.php:123` ("^1.0" → real constraint)
- Modify: `packages/laravel-queue/composer.json` + root `composer.json` (synchronization)
- Modify: `docs/installation.md:42` (displayed version)
- Create: `CHANGELOG.md` (root) + `packages/laravel-queue/CHANGELOG.md`
- Test: `packages/laravel-queue/tests/Unit/ExtensionVersionTest.php` (new)

**Context:** composer requires `ext-rabbit_rs: ^0.0`, the exception talks about "^1.0", the doc displays 1.0.0, the workspace is 0.0.7. No CHANGELOG or upgrade notes.

- [ ] **Step 1: Write the failing test**

```php
it('states the same extension version constraint everywhere', function () {
    $composer = json_decode(file_get_contents(__DIR__.'/../../composer.json'), true);
    $constraint = $composer['require']['ext-rabbit_rs'];

    expect($constraint)->toBe('^0.0'); // aligned to workspace 0.0.x until 1.0
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Unit/ExtensionVersionTest.php`
Expected: FAIL or PASS depending on state — the goal is to lock in consistency.

- [ ] **Step 3: Align and document**

1. Decision: the `ext-rabbit_rs` constraint follows the workspace version (`^0.0` until 1.0). Fix the message of `RabbitMqServiceProvider.php:123` to reflect the real constraint.
2. Fix `docs/installation.md` (version => 0.0.7, or make the example generic `php -i | grep rabbit_rs`).
3. Create `CHANGELOG.md` (Keep a Changelog, Added/Changed/Fixed sections for v0.0.3..v0.0.7 from git tags) and `packages/laravel-queue/CHANGELOG.md` (simplified mirror).
4. Add a version consistency check to the release job (already present: `release.yml` checks tag↔Cargo.toml — extend it to the Laravel package ext constraint).

- [ ] **Step 4: Verify and commit**

Run: `rtk ./scripts/check.sh && rtk composer validate --strict`
Expected: PASS.

```bash
git add CHANGELOG.md packages/laravel-queue docs
git commit -m "chore: align extension version constraints and add changelogs"
```

---

### Task 17: Reduce installation friction and add PHP static analysis

**Files:**
- Modify: `packages/laravel-queue/composer.json:9` (`require` → `suggest` + `conflict`?)
- Modify: `packages/laravel-queue/src/RabbitMqServiceProvider.php:51` (runtime validation)
- Modify: `scripts/check.sh` (Pint + PHPStan)
- Create: `packages/laravel-queue/pint.json` + `phpstan.neon`
- Test: run Pint/PHPStan — iterative fix

**Context:** `ext-rabbit_rs` in hard `require` makes every `composer install` fail (CI, artifact builds, dev without the extension) although the runtime check already exists (`extension_loaded`, line 51). No PHP static analysis in the quality gate.

- [ ] **Step 1: Decide and apply the dependency policy**

Move `ext-rabbit_rs` from `require` to `suggest` with an explicit message, AND add a runtime validation **blocking at first use** (connection resolution) with an actionable message ("install the extension via `pie install goopil/rabbit-rs-native`"). Verify that `RabbitMqServiceProviderTest` ("missing extension" assertion) stays green — adapt if necessary. Caution: the Unit/Feature tests run without the extension per the AGENTS.md contract — the validation must not run at boot but at driver resolution.

- [ ] **Step 2: Add Pint and PHPStan to the gate**

```bash
cd packages/laravel-queue
composer require --dev laravel/pint phpstan/phpstan larastan/larastan --with-all-dependencies
```

`packages/laravel-queue/pint.json`: laravel preset. `packages/laravel-queue/phpstan.neon`: level 6, analyze `src/`.

In `scripts/check.sh`, after `composer validate`:

```bash
(cd packages/laravel-queue && vendor/bin/pint --test)
(cd packages/laravel-queue && vendor/bin/phpstan analyse --no-progress --error-format=table)
```

- [ ] **Step 3: Fix all reported issues iteratively**

Run: `(cd packages/laravel-queue && vendor/bin/pint -v) && (cd packages/laravel-queue && vendor/bin/phpstan analyse)`
Fix each violation (separate commits per category if large). Loop until 0 errors.

- [ ] **Step 4: Full gate and commit**

Run: `rtk ./scripts/check.sh`
Expected: PASS.

```bash
git add packages/laravel-queue scripts/check.sh
git commit -m "chore(laravel): soft-depend on the extension and add pint/phpstan to the gate"
```

---

### Task 18: Harden the worker supervisor

**Files:**
- Modify: `packages/laravel-queue/src/Console/WorkerSupervisor.php:125-178`
- Test: `packages/laravel-queue/tests/Feature/WorkerSupervisorIntegrationTest.php` (extension)

**Context:** `run()` requires pcntl even with `--workers=1` (the error message "Install it or run with --workers=1" is wrong); the `sleep($backoff)` (line 168) blocks the supervision loop of **all** children during a single child's backoff.

- [x] **Step 1: Write the failing tests**

```php
it('runs a single worker inline without pcntl', function () {
    $supervisor = new WorkerSupervisor(workers: 1, options: [...]);
    // simulate the absence of pcntl (the class already exposes the hook)
    $supervisor->shouldReceive('hasPcntl')->andReturn(false); // or a test subclass
    // assert: the worker runs in the foreground (proc_open artisan queue:work)
    // without a SupervisorException
});

it('keeps supervising other children while one is in backoff', function () {
    // 2 children; child 0 crashes; during its N s backoff,
    // child 1 must be supervised (non-blocking poll)
});
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cd packages/laravel-queue && php vendor/bin/pest tests/Feature/WorkerSupervisorIntegrationTest.php`
Expected: FAIL.

- [x] **Step 3: Write minimal implementation**

1. `--workers=1` without pcntl: direct path `proc_open('php artisan queue:work ...')` + wait + exit codes, without fork. Fix the pcntl error message ("ext-pcntl is required for --workers>1").
2. Non-blocking backoff: replace `sleep($this->backoffSeconds(...))` with a `restartAt[$index] = microtime(true) + $backoff` table; the supervision loop consults `restartAt` and skips restart attempts while `microtime(true) < restartAt[$index]` (the rest of the loop continues: the existing `usleep(100_000)` at line 178 already provides the polling).
3. Fix the defects identified in the evaluation: `--sleep` propagation, `--stop-when-empty`, child log supervision (`--log-children` option or stdout mux) — depending on the surface already present in `RabbitMqWorkCommand`.

- [x] **Step 4: Run tests to verify they pass**

Run: `cd packages/laravel-queue && php vendor/bin/pest && rtk ./scripts/check.sh`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add packages/laravel-queue
git commit -m "fix(laravel): pcntl-free single worker path and non-blocking restart backoff"
```

---

### Task 19: Align documentation and the stats() stub

**Files:**
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php:82` (`@return` of `stats()`: document the 17 real keys)
- Modify: `packages/laravel-queue/README.md` (prefetch 16 vs 64 lines 113/220; test suites; events)
- Modify: `docs/laravel.md:40` (remove `dispatchBatch`)
- Modify: `packages/laravel-queue/tests/Pest.php:13-37` (remove the obsolete `validConfig()` helper if unused, otherwise migrate it to the new schema)
- Modify: `docs/operations.md` (prometheus/OTel: clarify "adapters coming")
- Test: existing PHPT reflection

**Context:** Docs/code inconsistencies identified by the audit: the `stats()` stub documents 8 keys for 17 real ones (`pool.rs:211-261`); prefetch advertised 16, default 64; the README references non-existent PHPUnit suites ("Rabbit RS Laravel") while the project uses Pest (`default` suite); `docs/laravel.md` documents a non-existent API.

- [ ] **Step 1: Align the stats() stub**

Document each returned key (17) with its type: `closed`, `pid`, `handle`, `publishes_total`, `confirmations_total`, `returns_total`, `backpressure_total`, `reconnects_total`, `deliveries_total`, `acks_total`, `rejects_total`, `confirmation_latency_p50|p95|p99`, `settlement_latency_p50|p95|p99` (verify the exact list from `pool.rs:204-261` and `insert_percentile`).

- [ ] **Step 2: Fix the package README and docs**

1. Unify prefetch: either fix the README to 64 (actual default `config/rabbit-rs.php:208`), or fix the default to 16 (product decision — config default wins, README follows).
2. Testing section: reference `php vendor/bin/pest` and the actual suites (`default`, Integration).
3. `docs/laravel.md:40`: replace the `ProcessOrder::dispatchBatch($jobs)` example with `$queue->bulk([...])` or `Bus::batch`.
4. `docs/operations.md:231`: after Task 10, events fire on publish/consume — reword precisely.
5. `tests/Pest.php`: remove `validConfig()` if no test uses it (`rg validConfig packages/laravel-queue/tests`), otherwise migrate it.

- [ ] **Step 3: Verify**

Run: `rtk ./scripts/check.sh && rtk ./scripts/test-extension.sh && cd packages/laravel-queue && php vendor/bin/pest`
Expected: PASS (the reflection PHPT validates the stub — `php -l` on the stub).

- [ ] **Step 4: Commit**

```bash
git add crates/rabbit-rs-php/stubs packages/laravel-queue docs
git commit -m "docs: align stats stub, README, guides with the implementation"
```

---

### Task 20: Settle and apply the ZTS decision

**Files:**
- Modify: `composer.json` (root, PIE meta: `support-zts`)
- Modify: `release/pie-matrix.json` (16 → 8 combinations)
- Modify: `.github/workflows/release.yml` (build matrix `ts: ["nts", "zts"]` → `["nts"]`)
- Modify: `docs/distribution.md` + `docs/installation.md`
- Modify: `.github/workflows/ci.yml:192-196` (ZTS advisory job — remove or make blocking depending on option)

**Context:** `support-zts: true` is advertised without proof: the global `RuntimeRegistry` is shared between PHP threads under ZTS (potential race on the Zend refcount via `shallow_clone()` of callbacks), the CI ZTS job is advisory-only (`continue-on-error: true`), and the 8 ZTS release artifacts only get an `extension_loaded` smoke test. Shipping functionally untested ZTS binaries is the most concrete memory risk of the project.

**Decision (recommended — Option A):** remove ZTS from the V1 scope and reintroduce it in V2 with per-thread isolation + blocking CI + real concurrency tests.

- [ ] **Step 1: Write the failing consistency check**

Add to `scripts/verify-pie-naming.sh` (created by Task 13) a check that the matrix declared in `release/pie-matrix.json` contains no `zts` entry as long as `support-zts` is removed:

```bash
# fail if any zts entry remains while support-zts is false
if grep -qi 'zts' release/pie-matrix.json; then
  echo "ERROR: zts entries found in pie-matrix.json after ZTS removal (Task 20)"; exit 1
fi
```

- [ ] **Step 2: Apply Option A**

1. Root `composer.json`: `"support-zts": false` in the `php-ext` section.
2. `release/pie-matrix.json`: remove the 8 ZTS entries.
3. `.github/workflows/release.yml`: `ts: ["nts"]` (and simplify the conditional `TS_SUFFIX` logic lines 158-159).
4. `.github/workflows/ci.yml`: remove the advisory ZTS job.
5. `docs/distribution.md` + `docs/installation.md`: document "NTS only in V1; ZTS planned for V2" with the justification (process-global registry, TSRM isolation not implemented).

- [ ] **Step 3: Verify the release matrix**

Run: `./scripts/verify-pie-naming.sh && rtk composer validate --strict`
Expected: PASS. Also check `release/validate-distribution.sh` if it references ZTS.

- [ ] **Step 4: Commit**

```bash
git add composer.json release .github docs
git commit -m "build: drop unproven ZTS from the V1 release matrix (revisit in V2)"
```

> **Option B (rejected for V1, to document in the PR):** implement per-thread
> isolation (TSRM-aware registry), make the CI ZTS job blocking and add concurrency
> tests — estimated cost several weeks, deferred.

---

## Exit criteria toward 1.0

- [ ] All P0 tasks delivered and verified in CI.
- [ ] All P1 tasks delivered; the `pie install` chain validated on a real release (Task 13).
- [ ] Task 12 (TLS) validated on the 3-node lab with handshake, untrusted CA, and SNI.
- [ ] `./scripts/check.sh` green + Pint/PHPStan 0 errors + non-regressed coverage (Codecov).
- [ ] ZTS: decision settled and applied (Task 20 — Option A by default).
- [ ] 1.0 CHANGELOG written, version constraints aligned, docs consistent.
- [ ] Certification of CLI, FPM, and the 4 advertised Octane servers (overflow: `scripts/test-octane.sh` on each runtime).
- [ ] Round 2 (stall/pre-fill/clear) root-caused and re-bench compared to the Phase E archives.
