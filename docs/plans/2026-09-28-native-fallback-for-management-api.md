# Native Fallback for Management-API Features — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every feature that currently requires the RabbitMQ management HTTP API keeps working without it, by falling back to native AMQP operations; the features that are protocol-impossible without the API are documented as such.

**Architecture:** Hybrid strategy, decided per feature at runtime: when a connection configures `management_url`, the existing HTTP path is used unchanged (primary, zero broker mutation, cross-process truth); when it is absent, the feature falls back to native AMQP operations exposed through the core `Transport`/`ClientPool` and the PHP `Pool` surface. Three features get native fallbacks (delay-plugin detection, doctor dead-letter canary, topology exchange existence); three gaps stay API-only because AMQP cannot express them (cumulative broker counters, binding enumeration, unacked count) and get documentation instead.

**Tech Stack:** Rust 1.98.1 / edition 2024 (crates/rabbit-rs-core, crates/rabbit-rs-php via ext-php-rs), Laravel 11 package (PHP 8.3), Pest, cargo-nextest, RabbitMQ lab (docker compose, with- and without-plugin brokers).

## Global Constraints

- `#![forbid(unsafe_code)]` holds; do not weaken workspace lint configuration.
- No real sleeps in Rust unit tests; paused Tokio time + scripted mock transport only.
- Public APIs get `///` docs and `#[must_use]` where applicable; config failures identify their exact input path.
- Never expose credentials, full broker URIs, or certificate material through `Debug`, errors, metrics, or logs.
- PHP methods on `Pool` follow the existing `size()`/`clear()` pattern: `ensure_open`, flush publish buffer, surface publish errors, `block_on` client call, `client_exception` on error.
- Laravel Unit/Feature tests must run WITHOUT the extension (missing-extension assertion in `RabbitMqServiceProviderTest`); integration tests load it via `scripts/test-extension.sh` / `scripts/test-integration.sh`.
- All repository artifacts (comments, docs, commit messages) in English.
- Clippy is `-D warnings`; run `rtk cargo fmt --all` after Rust edits.
- Before claiming completion: `rtk ./scripts/check.sh`.

## Current behavior (verified 2026-09-28)

| Feature | File | Today without `management_url` |
|---|---|---|
| Depth scaler + drain check | `packages/laravel-queue/src/Support/QueueDepthSampler.php` | Native fallback already built-in (`Pool::size()`) |
| Delay plugin detection | `src/Support/DelayPluginGuard.php:129` (`/api/overview`) | `auto` degrades to `ttl`; plugin mode unguarded |
| Status counters | `src/Console/RabbitMqStatusCommand.php:134` | Connection skipped (API-only by protocol) |
| Doctor unroutable stats | `src/Console/RabbitMqDoctorCommand.php:282` | Check skipped (API-only by protocol) |
| Doctor DLQ canary | `src/Console/DoctorProbe.php:172` (declare/bind/purge/delete/fetch via API) | Canary refused: "no management_url configured" |
| Topology verification | `src/Console/RabbitMqTopologyCommand.php:136` | Warn + pass (queues already native) |

Core transport surface today (`crates/rabbit-rs-core/src/transport.rs:315`): `declare_exchange`, `verify_exchange`, `declare_queue`, `verify_queue`, `bind_queue`, `queue_size`, `purge_queue`, `delete_queue`. Missing for native parity: `delete_exchange`, `basic.get` (`get_message`). `ClientPool` (client.rs:511) exposes only `queue_size`/`purge_queue` as admin ops, all riding `admin_channel(broker)` (fresh publisher channel per call on the coordinator's connection + recovery machinery — a channel killed by a failed declare contaminates nothing).

---

### Task 1: Core — `delete_exchange` + `get_message` on the transport

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport.rs` (trait + new `FetchedMessage` struct)
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs` (lapin impls)
- Modify: `crates/rabbit-rs-core/src/transport/mock.rs` (scriptable impls)

**Interfaces (produced):**
```rust
// transport.rs, next to Delivery
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchedMessage {
    pub delivery_tag: u64,
    pub exchange: String,
    pub routing_key: String,
    pub redelivered: bool,
    pub message_id: Option<String>,
    pub headers: Arc<Headers>,
    pub payload: Bytes,
}

// TopologyChannel additions (transport.rs, after delete_queue):
/// Deletes an exchange. A missing exchange resolves successfully (idempotent deletion).
///
/// # Errors
///
/// Returns an error when the broker rejects the deletion request.
async fn delete_exchange(&self, exchange: &str) -> TransportResult<()>;

/// Fetches one message with `basic.get` and settles it on this channel:
/// `requeue = true` inspects without consuming (`basic.reject` requeue),
/// `requeue = false` acknowledges it away. Returns `None` when the queue
/// is empty (get-empty).
///
/// # Errors
///
/// Returns an error when the queue does not exist or the broker rejects
/// the fetch or the settlement.
async fn get_message(&self, queue: &str, requeue: bool) -> TransportResult<Option<FetchedMessage>>;
```

- [x] **Step 1: Add `FetchedMessage` + the two trait methods** exactly as above. `delete_queue`'s doc comment (idempotency rationale) is the model for `delete_exchange`.
- [x] **Step 2: Lapin impl** (`transport/lapin.rs`, `impl TopologyChannel for LapinChannel`):
```rust
async fn delete_exchange(&self, exchange: &str) -> TransportResult<()> {
    delete_exchange(&self.inner, exchange).await
}

async fn get_message(&self, queue: &str, requeue: bool) -> TransportResult<Option<FetchedMessage>> {
    match self
        .inner
        .basic_get(queue.to_owned().into(), BasicGetOptions::default())
        .await
        .map_err(map_lapin_error)?
    {
        lapin::message::GetResult::Ok(delivery) => {
            let fetched = FetchedMessage {
                delivery_tag: delivery.delivery_tag(),
                exchange: delivery.exchange().to_string(),
                routing_key: delivery.routing_key().to_string(),
                redelivered: delivery.redelivered(),
                message_id: delivery
                    .properties()
                    .message_id()
                    .as_ref()
                    .map(ToString::to_string),
                headers: Arc::new(headers_from_lapin(delivery.properties().headers())),
                payload: Bytes::copy_from_slice(delivery.data()),
            };
            if requeue {
                self.inner
                    .basic_reject(fetched.delivery_tag, BasicRejectOptions { requeue: true, ..Default::default() })
                    .await
                    .map_err(map_lapin_error)?;
            } else {
                self.inner
                    .basic_ack(fetched.delivery_tag, BasicAckOptions::default())
                    .await
                    .map_err(map_lapin_error)?;
            }
            Ok(Some(fetched))
        }
        lapin::message::GetResult::Empty => Ok(None),
    }
}
```
  Free helpers at the bottom of lapin.rs, mirroring the existing `delete_queue` helper style:
  - `delete_exchange`: `exchange_delete` with `ExchangeDeleteOptions::default()`; a missing exchange (404) resolves Ok — mirror how `delete_queue` (lapin.rs:564) neutralizes NOT_FOUND (read that helper first and reuse its detection).
  - `headers_from_lapin`: reuse the existing lapin→core header conversion used by the consumer delivery mapping in this file (find the conversion used by `DeliveryStream`; extract/reuse, do not duplicate).
- [x] **Step 3: Mock impl** (`transport/mock.rs`):
  - `TransportOperation::DeleteExchange { exchange: String }` → `record_topology` (generic result queue, like `DeleteQueue`).
  - `TransportOperation::GetMessage { queue: String, requeue: bool }` recorded, result popped from a new scripted queue `get_messages: VecDeque<TransportResult<Option<FetchedMessage>>>` defaulting to `Ok(None)` — same shape as `queue_sizes`.
  - `MockTransport::push_get_message_result(...)` (or reuse the existing scripted-queue naming convention in the file — check `queue_sizes` push site).
- [x] **Step 4: Compile + mock tests.** Add to the existing mock tests (same file or `tests/`): a scripted `get_message` returns the scripted message / `None` when unscripted; `delete_exchange` records the operation and scripts success/failure.
  Run: `rtk cargo test -p rabbit-rs-core transport::mock`
- [x] **Step 5: Focused suite green:** `rtk cargo test -p rabbit-rs-core` then `rtk cargo fmt --all && rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`.

### Task 2: Core — `ClientPool` admin + probe operations

**Files:**
- Modify: `crates/rabbit-rs-core/src/client.rs`
- Test: `crates/rabbit-rs-core/src/client.rs` (tests module, mock-driven, following the existing test style in that file)

**Interfaces (produced):**
```rust
impl ClientPool {
    pub async fn declare_queue(&self, broker: &str, spec: &QueueSpec) -> Result<(), ClientError>;
    pub async fn bind_queue(&self, broker: &str, spec: &BindingSpec) -> Result<(), ClientError>;
    pub async fn delete_queue(&self, broker: &str, queue: &str) -> Result<(), ClientError>;
    pub async fn delete_exchange(&self, broker: &str, exchange: &str) -> Result<(), ClientError>;
    pub async fn verify_exchange(&self, broker: &str, exchange: &str) -> Result<(), ClientError>; // passive declare probe
    pub async fn get_message(&self, broker: &str, queue: &str, requeue: bool) -> Result<Option<FetchedMessage>, ClientError>;
    /// Returns whether the broker supports the delayed-message plugin, probed
    /// with a throwaway `x-delayed-message` exchange declare.
    /// Ok(false) = plugin provably absent; Err = probe inconclusive (broker
    /// unreachable, permissions, unexpected error).
    pub async fn probe_delay_plugin(&self, broker: &str) -> Result<bool, ClientError>;
}
```
All but `probe_delay_plugin` are one-line `admin_channel(broker)` + channel call (exact pattern of `queue_size`, client.rs:511). `verify_exchange` builds `ExchangeSpec { name, kind: ExchangeKind::Direct, durable: false, auto_delete: false, internal: false, arguments: Headers::new() }` and calls `channel.verify_exchange(&spec)` (passive — kind/flags ignored by the broker).

- [x] **Step 1: Write failing mock-driven tests** in client.rs tests: `declare_queue` records `DeclareQueue` on the mock; `get_message` returns the scripted message and `None` when unscripted; `verify_exchange` scripts Ok/Err; `probe_delay_plugin` → Ok(true) when declare succeeds, Ok(false) when the mocked declare error message contains `NOT-IMPLEMENTED`, Err on any other mocked declare error, and the probe exchange is deleted after a successful declare (assert `DeleteExchange` recorded).
- [x] **Step 2: Run** `rtk cargo test -p rabbit-rs-core client` — expect compile failure (methods missing).
- [x] **Step 3: Implement** all seven methods + the delay probe:
```rust
const DELAY_PROBE_EXCHANGE: &str = "rabbit-rs.probe.delayed";

pub async fn probe_delay_plugin(&self, broker: &str) -> Result<bool, ClientError> {
    self.ensure_open()?;
    // Delete any stale probe exchange first: a leftover exchange of a
    // different type would turn the declare below into a false negative
    // (PRECONDITION_FAILED instead of NOT_IMPLEMENTED).
    let _ = self.delete_exchange(broker, DELAY_PROBE_EXCHANGE).await;

    // A fresh channel per attempt: an unknown exchange type is a channel-
    // closing protocol error, and the admin channel factory hands out a
    // new channel on every call, so the failed probe poisons nothing.
    let channel = self.admin_channel(broker).await?;
    let spec = ExchangeSpec {
        name: DELAY_PROBE_EXCHANGE.to_owned(),
        kind: ExchangeKind::Delayed(Box::new(ExchangeKind::Direct)),
        durable: true,
        auto_delete: false,
        internal: false,
        arguments: Headers::new(),
    };
    match channel.declare_exchange(&spec).await {
        Ok(()) => {
            let _ = channel.delete_exchange(DELAY_PROBE_EXCHANGE).await;
            Ok(true)
        }
        Err(error) if error.message.contains("NOT-IMPLEMENTED") => Ok(false),
        Err(error) => Err(ClientError::transport(&error)),
    }
}
```
  Error-message matching on `NOT-IMPLEMENTED` mirrors the existing convention where the PHP layer matches `NOT-FOUND` on mapped transport errors; verify the mapped lapin text during Task 8 (lab has a no-plugin broker) and extend the matcher if lapin capitalizes differently (e.g. also match `NOT_IMPLEMENTED`).
- [x] **Step 4: Run focused tests** — green; then `rtk cargo test -p rabbit-rs-core`, fmt, clippy.

### Task 3: PHP extension — `Pool` surface for native topology/probe ops

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs` (6 methods, following `size()`/`clear()`)
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` (regenerated, never hand-edited)
- Test: `crates/rabbit-rs-php/tests/Pool/` (Pest, integration — extension + lab required)

**Interfaces (produced, PHP):**
```php
Goopil\RabbitRs\Pool:
  /** @throws ConnectionException|ClientException */
  public function declareQueue(string $broker, string $queue, string $kind = 'quorum', bool $durable = true): void;
  public function bindQueue(string $broker, string $exchange, string $queue, string $routingKey): void;
  public function deleteQueue(string $broker, string $queue): void;
  public function verifyExchange(string $broker, string $exchange): void; // throws on missing/incompatible
  public function probeDelayPlugin(string $broker): bool;                 // false = provably absent
  /** @return array{message_id: string, payload: string}|null null = queue empty */
  public function getMessage(string $broker, string $queue, bool $requeue = true): ?array;
```
Every method: `ensure_open("Goopil\\RabbitRs\\Pool::<name>")` → `publish_buffer.flush_all()` (getMessage too: a buffered publish must land before a fetch can see it) → `surface_publish_errors()` → `block_on(client.<op>(...))` → `client_exception(&error)` on Err. `kind` maps `'quorum'`/`'classic'` → `QueueKind` (reject anything else with a clear argument error naming the PHP call). `declareQueue` builds `QueueSpec { name, durable, exclusive: false, auto_delete: false, kind, dead_letter_exchange: None, dead_letter_routing_key: None, message_ttl: None, expires: None, delivery_limit: None, arguments: Headers::new() }`. `getMessage` maps `FetchedMessage` → hash table with `message_id` (or `''`) and `payload` (UTF-8 lossy? no — payload is bytes: base64-encode? Check how the consumer Delivery exposes payload to PHP in `delivery.rs` and reuse that exact encoding). `None` maps to PHP `null`.

- [x] **Step 1: Pest integration test first** (`tests/Pool/TopologyOpsTest.php`): against the lab broker — declare quorum queue, bind, size, publish via pool, getMessage sees message_id + requeues it (size unchanged), getMessage(requeue: false) consumes it, deleteQueue removes it (subsequent size throws NOT-FOUND), verifyExchange ok on the route exchange / throws on a missing one, probeDelayPlugin true on the plugin broker. A second test class targets the **no-plugin** broker for `probeDelayPlugin === false`.
- [x] **Step 2: Run** `./scripts/test-extension.sh` (or the focused Pest filter) — expect failure (methods undefined).
- [x] **Step 3: Implement** the 6 Rust methods; regenerate stubs: `./scripts/stubs.sh --out crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`; `php -l` the stub.
- [x] **Step 4: Run** extension Pest suite + PHPT: `./scripts/test-extension.sh`.

#### Discovered while executing Task 3 (pre-existing core bug, fixed)

Rapid sequential pool lifecycles (`acquire → admin op → close` on the same
process) intermittently failed admin ops with `did not become ready ... within
30s: connection is not ready`. Reproduced in pure Rust (`MockTransport`, no
PHP): the admin-channel readiness wait observed a failed `OpenPublisher` (the
command had been queued while the actor was momentarily `Connecting`) *after*
the state watch already reported `Ready`, then parked on
`wait_for_transition(&Ready)` — waiting for a healthy, stable connection to
leave `Ready` — until the deadline. `admin_channel` was the only
`wait_for_coordinator_ready` caller with `spin_on_ready: false`; consumer
acquisition already spun on `Ready` for exactly this reason.

- Fix: `crates/rabbit-rs-core/src/client.rs` — `admin_channel` passes
  `spin_on_ready: true` (deadline stays bounded by the configured
  `consumer.wait_timeout`).
- Regression test: `crates/rabbit-rs-core/tests/integration.rs`
  `admin_ops_stay_ready_across_repeated_pool_lifecycles` — 24
  acquire/size/close cycles on a shared `RuntimeRegistry` with a 1s readiness
  wait; fails in ~1s per racy cycle with the old code, passes instantly with
  the fix.

### Task 4: Laravel — `DelayPluginGuard` hybrid probe

**Files:**
- Modify: `packages/laravel-queue/src/Support/DelayPluginGuard.php`
- Test: `tests/Feature/DelayPluginGuardTest.php` (Http::fake path unchanged + new native paths)

**Behavior:** verdict resolution order per connection: (1) `management_url` set → HTTP `/api/overview` probe (existing code, extracted as `probeManagementApi`); (2) no `management_url` → native AMQP probe (`probeNative`): extension loaded? config compiles? `Pool::probeDelayPlugin(broker)`; any failure → `null` (unverifiable). Caching, `resolveAutoMode`, `assertPluginEnabled` semantics unchanged. Add a test seam: `public static ?Closure $nativeProbe = null` (set/cleared via `reset()`).

```php
private static function probeNative(string $connection, array $config): ?bool
{
    if (self::$nativeProbe !== null) {
        return (self::$nativeProbe)($connection);
    }
    if (! extension_loaded('rabbit_rs')) {
        return null;
    }
    try {
        $compiled = ConnectionCompiler::compile($connection, $config, RabbitRsConnections::packageDefaults());
    } catch (\Throwable) {
        return null;
    }
    $broker = (string) ($compiled['native']['brokers'][0]['name'] ?? 'default');
    try {
        $pool = new Pool($compiled['native']);
        try {
            return $pool->probeDelayPlugin($broker);
        } finally {
            $pool->close();
        }
    } catch (\Throwable) {
        return null;
    }
}
```

- [x] **Step 1: Failing Feature tests** (no extension in Feature env): connection without `management_url` + seam returns true → `resolveAutoMode` keeps `auto`; seam returns false → degrades to `ttl`; seam returns null → `ttl`; `assertPluginEnabled` with seam false → `DelayPluginMissingException`; without extension and no seam → warning logged once, publish passes through.
- [x] **Step 2: Run** `./scripts/test-laravel.sh` (Unit + Feature) — new tests fail.
- [x] **Step 3: Implement** the hybrid probe (split existing `probeBroker` body into `probeManagementApi($config, $url)`; add `probeNative`; extend `reset()` to clear `$nativeProbe`).
- [x] **Step 4: Run** `./scripts/test-laravel.sh` — green. Existing Http::fake tests must pass unchanged (primary path untouched).

### Task 5: Laravel — `DoctorProbe` canary hybrid

**Files:**
- Modify: `packages/laravel-queue/src/Console/DoctorProbe.php`
- Modify: `packages/laravel-queue/src/Console/RabbitMqDoctorCommand.php` (canary no longer gated on `$managementUsable`)
- Test: `tests/Integration/DoctorDlxCanaryTest.php` (native-path variant) + existing API-path test unchanged

**Behavior:** `deadLetterCanary()` keeps its signature. `$useApi = management_url present`. Branches:
- Declare canary DLQ + bind: API path = existing `declareCanaryDlq`; native = `$pool->declareQueue($broker, $canaryDlq, 'quorum', true)` + `$pool->bindQueue($broker, $dlx, $canaryDlq, $deadLetterRoutingKey)`.
- Verification after reject: API = existing `assertCanaryOnDlq`; native = new `assertCanaryOnDlqNative(Pool $pool, string $broker, string $messageId, string $configuredDlq, string $canaryDlq)` — tier 1: up to `CANARY_DLQ_ATTEMPTS` rounds, each scanning up to `CANARY_DLQ_SCAN_WINDOW` messages via `$pool->getMessage($broker, $canaryDlq, true)` (`null` → empty → sleep `CANARY_DLQ_POLL_MS` µs between rounds; canary `message_id` found → tier 1 pass; exhaustion → the same hard-failure message as the API tier 1); tier 2: one window of up to `CANARY_DLQ_SCAN_WINDOW` `getMessage(..., requeue: true)` on the configured DLQ — canary found → ok, exhausted/deeper → `CanaryInconclusiveException` with the foreign count (same wording as the API tier 2).
- Teardown in `finally`: API = `$pool->close()` then existing `teardownCanaryDlq` (HTTP); native = best-effort `$pool->clear($broker, $canaryDlq)` + `$pool->deleteQueue($broker, $canaryDlq)` each swallowed, **then** `$pool->close()`.
- The early return `no management_url configured — the canary cannot verify DLQ delivery` is deleted.

`RabbitMqDoctorCommand::checkDeadLetterCanary`: drop the `$managementUsable` gate on the canary call (keep the gate only for `checkPublishOutcomes`, which is protocol-API-only).

- [x] **Step 1: Failing integration test**: duplicate the existing canary scenario with a connection config **without** `management_url` (lab): canary lands on the configured DLQ → check passes; broken DLX variant → hard failure message present.
- [x] **Step 2: Run** `./scripts/test-integration.sh` (Laravel Integration part) — new test fails ("no management_url configured").
- [x] **Step 3: Implement** the branching + `assertCanaryOnDlqNative` + native teardown; update the doctor gate.
- [x] **Step 4: Run** integration tests — both canary paths green; feature suite (fake `DoctorProbe`) still green.

### Task 6: Laravel — topology command native exchange checks

**Files:**
- Modify: `packages/laravel-queue/src/Console/RabbitMqTopologyCommand.php`
- Modify: `packages/laravel-queue/src/Console/DoctorProbe.php` (add `exchangeExists`)
- Test: `tests/Feature/RabbitMqTopologyCommandTest.php` (fake `DoctorProbe` — it is container-resolved)

**Behavior:** in `verifyManagement` (rename optional; keep name, adjust docblock), when `management_url` is absent: instead of warn+pass, run native exchange-existence checks for every route exchange and the dead-letter exchange (`$probe->exchangeExists($compiled['native'], $broker, $exchange)`; `NOT-FOUND` in error → fail line mirroring the API wording "exchange '...' is missing"; `null` → ok; other error → warn) and emit one warn that bindings and queue arguments were not verified. API-present path unchanged.

`DoctorProbe::exchangeExists` mirrors `queueSize`:
```php
public function exchangeExists(array $nativeConfig, string $broker, string $exchange): ?string
{
    return $this->probePool($nativeConfig, function (Pool $pool) use ($broker, $exchange): void {
        $pool->verifyExchange($broker, $exchange);
    });
}
```

- [x] **Step 1: Failing Feature test**: topology command, no `management_url`, fake probe whose `exchangeExists` returns a `NOT-FOUND` error → command fails with "exchange '...' is missing"; returns null → ok line; other error → warn only.
- [x] **Step 2: Run** `./scripts/test-laravel.sh` — fail.
- [x] **Step 3: Implement.**
- [x] **Step 4: Run** — green, including existing `--fix` tests.

### Task 7: Documentation — parity matrix + reference updates

**Files:**
- Create: `packages/laravel-queue/docs/management-api.md`
- Modify: `packages/laravel-queue/docs/reference.md` (`management_url` section, ~line 823)
- Modify: `docs/operations/reference.md` (`management_url` mention, ~line 180)

**Content of `management-api.md`:**
- Behavior matrix per feature with/without `management_url` after this work (scaler: native since #272; delay guard: HTTP then native probe; canary: HTTP then native; topology: API then native exchanges; status counters: API-only; unroutable stats: API-only).
- The three protocol-impossible gaps, with one-line rationale each: AMQP exposes no cumulative broker counters (status `deliver_get`/`ack`/`redeliver`, doctor `return_unroutable`), no binding enumeration (topology binding verification), and no unacked gauge (drain check counts ready only through the native path — after workers exit, unacked messages are redelivered and become ready, so the drain reading is approximate with a redelivery window).
- Operational note: the native delay probe declares and deletes one throwaway exchange (`rabbit-rs.probe.delayed`) per probe, once per process (verdict cached); the canary declares/purges/deletes its own DLQ either way.

- [x] **Step 1: Write the doc; link it from both reference.md `management_url` sections; update those sections to describe hybrid behavior.**

### Task 8: Full gate + lab integration

- [x] **Step 1:** `rtk ./scripts/check.sh` (fmt, clippy, nextest, composer validate).
- [x] **Step 2:** `./scripts/test-integration.sh` (Rust integration + Laravel Integration against the lab) — verify the `NOT-IMPLEMENTED` matcher text against the real no-plugin broker and fix Task 2's matcher if needed.
- [x] **Step 3:** `./scripts/test-fpm.sh` — pool-surface changes must not disturb fork isolation.
- [x] **Step 4:** Update this plan's checkboxes; leave a one-paragraph completion note with any deviation.

## Deliberately out of scope

- Behavioral route-canary (publish + consume) as a native substitute for binding verification — the DLX canary already proves wiring behaviorally; route checks ride normal traffic.
- Native live unroutable check via `mandatory` publish in the doctor — `Pool::stats()` `returns_total` remains the per-process signal; cross-process truth stays API-only.
- Exposing arbitrary topology arguments (`x-*`) through the PHP `declareQueue` — the canary DLQ needs only kind+durable; widen when a real need appears.

## Completion note

All eight tasks landed as planned, with three deviations worth recording:

1. **Task 8 verified the delay-probe matcher against the real no-plugin lab broker and Task 2's matcher was wrong.** A real RabbitMQ 4.x broker without `rabbitmq_delayed_message_exchange` does not answer the probe declare with `NOT-IMPLEMENTED`; it closes the channel with `PRECONDITION_FAILED - unknown exchange type 'x-delayed-message'`. The matcher in `ClientPool::probe_delay_plugin` now accepts both phrasings (mock test added for the real text), and the verification was performed live against the lab's `without-plugin` profile: `probeDelayPlugin` returns `false`, `DelayPluginGuard` degrades `auto` to `ttl`, and `assertPluginEnabled` throws `DelayPluginMissingException` with the actionable message.
2. **A pre-existing core race surfaced while executing Task 3** (admin-channel readiness), diagnosed and fixed under its own heading above; the regression test lives in `crates/rabbit-rs-core/tests/integration.rs`.
3. **The stub regeneration dropped leading backslashes on exception `extends` clauses** (cargo-php quirk), which crashed PHPStan's bootstrap; the committed stub keeps the hand-restored `\Goopil\RabbitRs\...` forms — do not blindly re-run `./scripts/stubs.sh` without re-applying them.

Two phpstan-era slip-ups were caught by the gate itself and fixed: a `checkTopology` call accidentally removed from the doctor flow during the Task 5 edit (restored), and a redundant loop condition PHPStan proved constant (removed).

The `docs/operations/reference.md` path named in Task 7 does not exist in the repository; the operational `management_url` mention lives in `docs/reference.md` and was updated there instead. The real-broker matcher verification ran as a one-off Pest test against the no-plugin lab broker and was deleted afterwards — the durable coverage is the mock test for both error phrasings.
