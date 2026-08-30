# Performance Gap Correction v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Recover the performance fixes from the unmerged `perf/consumer-buffer-perf` branch, adapting them to current `main`, plus apply additional audit findings. Four phases: isolated cherry-picks, `Arc<str>` transport, `publish_batch` trait, and `SafetyMode` + publish buffer.

**Architecture:** The branch `perf/consumer-buffer-perf` contains 27 commits of performance work that never merged to `main`. Some changes were partially cherry-picked (Arc<str> on Destination/MessageProperties, TaggedFuture, LTO). This plan recovers the remaining changes in 4 phases, each independently testable and committable. Phase 1 is cherry-picks of isolated commits. Phase 2 changes `transport::PublishRequest` and `PublishOutcome` from `String` to `Arc<str>`. Phase 3 adds a `publish_batch` trait method. Phase 4 introduces `SafetyMode` enum, a background `PublishPump` for blind mode, a PHP-side publish buffer, and `try_ack` fast path.

**Tech Stack:** Rust (edition 2024, toolchain 1.96.0), Tokio, Lapin 4.10, ext-php-rs, flume 0.12, arc-swap 1.x, Laravel/PHP 8.4

## Global Constraints

- Unsafe Rust is forbidden (`#![forbid(unsafe_code)]`).
- Keep Lapin behind the `Transport` abstraction so broker behavior remains mockable.
- Never expose credentials through `Debug`, errors, metrics, or logs.
- Run `rtk cargo fmt --all` after Rust edits.
- Run focused tests during iteration, then the full quality gate before completion.
- Preserve unrelated work in a dirty tree. Never discard changes you did not create.
- Keep commits logical and scoped.
- Do not retain Zend values, PHP objects, callbacks, or service-container state in Rust threads.
- The delivery contract is at-least-once: silent loss is unacceptable, while duplicates are permitted.

---

## File Structure

### Phase 1 — Cherry-picks (no new files)

- Modify: `crates/rabbit-rs-core/src/publisher/confirms.rs` — BTreeMap → HashMap
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs` — move exchange/routing_key, frame_max
- Modify: `crates/rabbit-rs-core/src/runtime.rs` — worker_threads=1 default
- Modify: `crates/rabbit-rs-php/src/conversion.rs` — defer format!, skip reject_unknown_keys in release
- Create: `crates/rabbit-rs-core/tests/transport_tuning.rs` — frame_max test

### Phase 2 — Arc<str> transport (no new files)

- Modify: `crates/rabbit-rs-core/src/transport.rs` — PublishRequest fields → Arc<str>
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs` — use .as_ref().into() instead of .clone().into()
- Modify: `crates/rabbit-rs-core/src/transport/mock.rs` — adapt mock for Arc<str>
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs` — into_transport_request: .to_string() → .clone()
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs` — PublishOutcome variants: String → Arc<str>
- Modify: All test files constructing PublishRequest or PublishOutcome

### Phase 3 — publish_batch trait (no new files)

- Modify: `crates/rabbit-rs-core/src/transport.rs` — add publish_batch to PublisherChannel trait
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs` — override publish_batch
- Modify: `crates/rabbit-rs-core/src/transport/mock.rs` — override publish_batch + TransportOperation

### Phase 4 — SafetyMode + pump + buffer (1 new file)

- Modify: `crates/rabbit-rs-core/Cargo.toml` — add arc-swap dependency
- Modify: `crates/rabbit-rs-core/src/config.rs` — SafetyMode enum + PublisherConfigSection.safety
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs` — PublisherConfig.safety + with_safety() + enables_confirms() + mandatory_flag()
- Create: `crates/rabbit-rs-core/src/publisher/pump.rs` — PublishPump for blind mode
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs` — try_publish_hot + try_publish_blind + pump wiring
- Modify: `crates/rabbit-rs-core/src/client.rs` — use safety to choose publish path
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs` — publish buffer + flush() + __destruct()
- Modify: `crates/rabbit-rs-php/src/classes/delivery.rs` — try_ack fast path
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs` — tryNext fast path
- Modify: `crates/rabbit-rs-php/src/lib.rs` — export ConsumerErrorKind
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` — flush(), tryNext(), tryAck()
- Modify: `crates/rabbit-rs-php/tests/phpt/binary_payload.phpt` — adjust for try_ack

---

### Task 1: ConfirmLedger BTreeMap → HashMap

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/confirms.rs` (full file, 37 lines)

**Interfaces:**
- Produces: `ConfirmLedger::with_capacity(capacity: usize) -> Self`
- Produces: `ConfirmLedger::drain() -> impl Iterator<Item = T>` (sorted by sequence for deterministic recovery)

- [ ] **Step 1: Write failing tests for HashMap-based ConfirmLedger**

Replace the entire content of `crates/rabbit-rs-core/src/publisher/confirms.rs` with:

```rust
use std::collections::HashMap;

pub struct ConfirmLedger<T> {
    pending: HashMap<u64, T>,
}

impl<T> Default for ConfirmLedger<T> {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }
}

impl<T> ConfirmLedger<T> {
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            pending: HashMap::with_capacity(capacity),
        }
    }

    pub fn insert(&mut self, sequence: u64, pending: T) {
        self.pending.insert(sequence, pending);
    }

    pub fn remove(&mut self, sequence: u64) -> Option<T> {
        self.pending.remove(&sequence)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn drain(&mut self) -> impl Iterator<Item = T> {
        let mut entries: Vec<(u64, T)> = std::mem::take(&mut self.pending).into_iter().collect();
        entries.sort_by_key(|(seq, _)| *seq);
        entries.into_iter().map(|(_, value)| value)
    }
}

#[cfg(test)]
mod tests {
    use super::ConfirmLedger;

    #[test]
    fn drain_returns_all_values_after_mixed_insert_remove() {
        let mut ledger = ConfirmLedger::<&'static str>::with_capacity(8);
        ledger.insert(1, "one");
        ledger.insert(2, "two");
        ledger.insert(3, "three");
        ledger.insert(4, "four");
        ledger.remove(2);
        ledger.remove(4);
        ledger.insert(5, "five");

        let mut drained: Vec<&'static str> = ledger.drain().collect();
        drained.sort_unstable();
        assert_eq!(drained, vec!["five", "one", "three"]);
    }

    #[test]
    fn drain_returns_entries_in_ascending_sequence_order() {
        let mut ledger = ConfirmLedger::<u64>::with_capacity(64);
        for seq in (1..=50).rev() {
            ledger.insert(seq, seq);
        }

        let drained: Vec<u64> = ledger.drain().collect();
        assert_eq!(drained, (1..=50).collect::<Vec<_>>());
    }

    #[test]
    fn insert_remove_roundtrip_preserves_value() {
        let mut ledger = ConfirmLedger::<u32>::with_capacity(4);
        ledger.insert(42, 100);
        assert_eq!(ledger.remove(42), Some(100));
        assert_eq!(ledger.remove(42), None);
    }

    #[test]
    fn with_capacity_preallocates() {
        let ledger = ConfirmLedger::<u32>::with_capacity(64);
        assert!(
            ledger.capacity() >= 64,
            "capacity {} should be >= 64",
            ledger.capacity()
        );
    }

    #[test]
    fn default_has_zero_capacity() {
        let ledger = ConfirmLedger::<u32>::default();
        assert_eq!(ledger.capacity(), 0);
    }
}
```

Note: Add `#[cfg(test)]` accessors for `capacity()`:

```rust
    #[cfg(test)]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.pending.capacity()
    }
```

- [ ] **Step 2: Use `with_capacity` in the actor where the ledger is created**

In `crates/rabbit-rs-core/src/publisher/actor.rs`, find where `ConfirmLedger` is constructed (search for `ConfirmLedger::default()` or `ConfirmLedger::new()`). Replace with:

```rust
ConfirmLedger::with_capacity(state.config.buffer_capacity)
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `rtk cargo test -p rabbit-rs-core publisher::confirms`
Expected: PASS (all 5 tests)

- [ ] **Step 4: Run focused publisher tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher_safety`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/confirms.rs crates/rabbit-rs-core/src/publisher/actor.rs
git commit -m "perf(publisher): replace BTreeMap with HashMap in ConfirmLedger

HashMap provides O(1) insert/remove on the hot path while drain()
collects into a Vec and sorts by sequence for deterministic recovery."
```

---

### Task 2: Move exchange/routing_key in Lapin publish to avoid clone

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs:146-166`

**Interfaces:**
- No interface changes

- [ ] **Step 1: Move exchange and routing_key before basic_publish call**

In `crates/rabbit-rs-core/src/transport/lapin.rs`, replace the `publish` method (lines 146-166):

```rust
async fn publish(&self, request: PublishRequest) -> TransportResult<Box<dyn PublishReceipt>> {
    let properties = publish_properties(&request);
    let exchange = request.exchange;
    let routing_key = request.routing_key;
    let confirmation = self
        .inner
        .basic_publish(
            exchange.as_ref().into(),
            routing_key.as_ref().into(),
            BasicPublishOptions {
                mandatory: request.mandatory,
                immediate: false,
            },
            &request.payload,
            properties,
        )
        .await
        .map_err(map_lapin_error)?;

    Ok(Box::new(LapinPublishReceipt {
        inner: confirmation,
    }))
}
```

This moves `exchange` and `routing_key` out of `request` before the `basic_publish` call, avoiding `.clone()`. The `.as_ref().into()` converts `&str` → `ShortString` without an intermediate `String` allocation.

Note: This step will only compile after Task 5 changes `PublishRequest.exchange` to `Arc<str>`. For now, if `exchange` is still `String`, use `exchange.as_str().into()` instead of `exchange.as_ref().into()`. The test in Step 2 will verify it compiles and passes.

- [ ] **Step 2: Run transport tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher_safety`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/rabbit-rs-core/src/transport/lapin.rs
git commit -m "perf(transport): move exchange/routing_key in Lapin publish to avoid clone

Move owned fields out of the request before basic_publish instead of
cloning, eliminating one String allocation per publish."
```

---

### Task 3: Tune frame_max to 1MB and worker_threads to 1

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs` (connection_uri function, ~line 356)
- Modify: `crates/rabbit-rs-core/src/runtime.rs` (TokioRuntimeFactory)

**Interfaces:**
- Produces: `TokioRuntimeFactory::default()` now creates a factory with `worker_threads: 1`

- [ ] **Step 1: Write failing test for frame_max in URI**

Create `crates/rabbit-rs-core/tests/transport_tuning.rs`:

```rust
//! Transport tuning tests: frame_max, worker_threads, and connection URI parameters.

use rabbit_rs_core::config::{BrokerConfig, Credentials, Endpoint, TlsConfig};
use rabbit_rs_core::transport::lapin::connection_uri;

fn test_broker() -> BrokerConfig {
    BrokerConfig {
        name: "test".to_string(),
        hosts: vec![Endpoint {
            host: "rabbit.local".to_string(),
            port: 5672,
            priority: 0,
        }],
        vhost: "/".to_string(),
        credentials: Credentials {
            username: "guest".to_string(),
            password: "guest".to_string(),
        },
        tls: TlsConfig {
            enabled: false,
            server_name: None,
            verify: Default::default(),
        },
        heartbeat: std::time::Duration::from_secs(30),
    }
}

#[test]
fn connection_uri_includes_frame_max_1mb() {
    let broker = test_broker();
    let endpoint = &broker.hosts[0];
    let uri = connection_uri(&broker, endpoint).expect("URI construction should succeed");
    assert!(
        uri.query().unwrap_or("").contains("frame_max=1048576"),
        "URI should contain frame_max=1048576, got: {:?}",
        uri.query()
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test transport_tuning`
Expected: FAIL with assertion error (frame_max not present)

- [ ] **Step 3: Add frame_max to connection_uri**

In `crates/rabbit-rs-core/src/transport/lapin.rs`, find the `connection_uri` function. Locate the `.append_pair("heartbeat", ...)` line. Add after it:

```rust
        .append_pair("heartbeat", &config.heartbeat.as_secs().to_string())
        // Negotiate a 1 MB frame size (up from the 128 KB default) so larger
        // payloads can be sent in a single frame, reducing per-frame overhead.
        .append_pair("frame_max", "1048576");
```

- [ ] **Step 4: Update existing URI test that checks heartbeat**

Search for the existing test that asserts `assert_eq!(uri.query(), Some("heartbeat=30"))`. Update it to:

```rust
assert_eq!(uri.query(), Some("heartbeat=30&frame_max=1048576"));
```

- [ ] **Step 5: Set worker_threads to 1 by default**

In `crates/rabbit-rs-core/src/runtime.rs`, find `TokioRuntimeFactory`. Replace the struct and its impls:

```rust
struct TokioRuntimeFactory {
    worker_threads: usize,
}

impl Default for TokioRuntimeFactory {
    fn default() -> Self {
        // I/O-bound workload: a single worker thread reduces scheduling
        // overhead while still allowing multiple concurrent tasks via async.
        Self { worker_threads: 1 }
    }
}

impl RuntimeFactory for TokioRuntimeFactory {
    fn create(&self) -> io::Result<Runtime> {
        let mut builder = Builder::new_multi_thread();
        builder.thread_name("rabbit-rs").enable_all();
        if self.worker_threads > 0 {
            builder.worker_threads(self.worker_threads);
        }
        builder.build()
    }
}
```

- [ ] **Step 6: Update the RuntimeRegistry::new call**

In `runtime.rs`, find where `TokioRuntimeFactory` is constructed (in `RuntimeRegistry::new` or similar). Replace with:

```rust
Arc::new(TokioRuntimeFactory::default())
```

- [ ] **Step 7: Add worker_threads test**

In `crates/rabbit-rs-core/src/runtime.rs` tests module, add:

```rust
    #[test]
    fn tokio_runtime_factory_defaults_to_one_worker_thread() {
        let factory = TokioRuntimeFactory::default();
        assert_eq!(
            factory.worker_threads, 1,
            "I/O-bound runtime should default to 1 worker thread"
        );
    }
```

- [ ] **Step 8: Run all tests**

Run: `rtk cargo test -p rabbit-rs-core --test transport_tuning && rtk cargo test -p rabbit-rs-core runtime`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add crates/rabbit-rs-core/src/transport/lapin.rs crates/rabbit-rs-core/src/runtime.rs crates/rabbit-rs-core/tests/transport_tuning.rs
git commit -m "perf(transport): tune frame_max to 1MB and worker_threads to 1

1MB frame size reduces per-frame overhead for larger payloads.
Single worker thread reduces scheduling overhead for I/O-bound workloads."
```

---

### Task 4: Defer format! to error branches and skip reject_unknown_keys in release (FFI)

**Files:**
- Modify: `crates/rabbit-rs-php/src/conversion.rs` (ConversionBudget, publish_with_budget, validated_config)

**Interfaces:**
- Produces: `validated_config` now skips `reject_unknown_keys` in release builds via `cfg!(debug_assertions)`
- Produces: `ConversionBudget::add_header_key_bytes` (split from `add_header_bytes`)
- Produces: `ConversionBudget::add_header_bytes(parent_path, key, bytes)` (deferred format!)

- [ ] **Step 1: Split add_header_bytes and defer format! to error path**

In `crates/rabbit-rs-php/src/conversion.rs`, find the `ConversionBudget` impl. Replace `add_header_bytes`:

```rust
    fn add_header_key_bytes(&mut self, parent_path: &str, bytes: usize) -> Result<(), String> {
        self.header_bytes = self
            .header_bytes
            .checked_add(bytes)
            .ok_or_else(|| format!("{parent_path}.headers: header size overflow"))?;
        if self.header_bytes > MAX_HEADER_BYTES {
            return Err(format!(
                "{parent_path}.headers: cumulative headers exceed the {MAX_HEADER_BYTES} byte limit"
            ));
        }
        Ok(())
    }

    fn add_header_bytes(
        &mut self,
        parent_path: &str,
        key: &str,
        bytes: usize,
    ) -> Result<(), String> {
        self.header_bytes = self
            .header_bytes
            .checked_add(bytes)
            .ok_or_else(|| format!("{parent_path}.headers.{key}: header size overflow"))?;
        if self.header_bytes > MAX_HEADER_BYTES {
            return Err(format!(
                "{parent_path}.headers.{key}: cumulative headers exceed the {MAX_HEADER_BYTES} byte limit"
            ));
        }
        Ok(())
    }
```

- [ ] **Step 2: Update add_headers to use new method names**

In the same file, update `add_headers` to call `add_header_key_bytes` instead:

```rust
    fn add_headers(&mut self, parent_path: &str, entries: usize) -> Result<(), String> {
        self.header_entries = self
            .header_entries
            .checked_add(entries)
            .ok_or_else(|| format!("{parent_path}.headers: header count overflow"))?;
        if self.header_entries > MAX_HEADER_ENTRIES {
            return Err(format!(
                "{parent_path}.headers: exceeds the {MAX_HEADER_ENTRIES} header entry limit"
            ));
        }
        Ok(())
    }
```

- [ ] **Step 3: Update call sites of add_header_bytes**

In the header conversion code, replace `budget.add_header_bytes(path, ...)` calls with `budget.add_header_bytes(parent_path, key, ...)` using the new signature. The `key` parameter is the header key being processed.

- [ ] **Step 4: Skip reject_unknown_keys in release builds**

In `validated_config`, add a `validate_keys` flag:

```rust
pub(crate) fn validated_config(table: &ZendHashTable) -> Result<ValidatedConfig, String> {
    let mut active_arrays = HashSet::new();
    let value = array_value(table, "config", 0, &mut active_arrays)?;
    let config: Config = serde_json::from_value(value)
        .map_err(|error| format!("config: invalid structure: {error}"))?;
    config.validate().map_err(|error| error.to_string())
}
```

And in `publish_with_budget`:

```rust
fn publish_with_budget(
    table: &ZendHashTable,
    path: &str,
    budget: &mut ConversionBudget,
    validate_keys: bool,
) -> Result<NativePublish, String> {
    if validate_keys {
        reject_unknown_keys(
            table,
            path,
            &[
                "broker",
                "exchange",
                "routing_key",
                "payload",
                "message_id",
                "content_type",
                "correlation_id",
                "headers",
                "delay_ms",
                "timeout_ms",
            ],
        )?;
    }
    // ... rest of function
```

Update the call site to pass `cfg!(debug_assertions)`:

```rust
    let validate_keys = cfg!(debug_assertions);
    publish_with_budget(table, path, &mut ConversionBudget::default(), validate_keys)
```

- [ ] **Step 5: Build and run PHP tests**

Run: `rtk cargo build -p rabbit-rs-php --features extension-tests`
Expected: PASS

Run: `rtk ./scripts/test-extension.sh`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/rabbit-rs-php/src/conversion.rs
git commit -m "perf(ffi): defer format! to error branches and skip reject_unknown_keys in release

Skip reject_unknown_keys in release builds to avoid per-key String
allocation on the publish hot path. Defer format! calls to error
branches only, using parent_path + key instead of pre-computed paths."
```

---

### Task 5: Change transport::PublishRequest to Arc<str>

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport.rs:251-281`
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs:146-166`
- Modify: `crates/rabbit-rs-core/src/transport/mock.rs` (all PublishRequest construction)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:727-790` (into_transport_request)
- Modify: All test files that construct `transport::PublishRequest`

**Interfaces:**
- Produces: `transport::PublishRequest { exchange: Arc<str>, routing_key: Arc<str>, ... }`
- Produces: `transport::PublishRequest::new(exchange: impl Into<Arc<str>>, routing_key: impl Into<Arc<str>>, payload: impl Into<Bytes>) -> Self`
- Consumes: `publisher::PublishRequest` (already has Arc<str> on Destination)

- [ ] **Step 1: Write failing test that Arc<str> is used**

In a test file (or inline test in transport.rs), add:

```rust
#[test]
fn publish_request_accepts_arc_str() {
    let req = PublishRequest::new(
        Arc::<str>::from("test_exchange"),
        Arc::<str>::from("test.key"),
        Bytes::from_static(b"payload"),
    );
    assert_eq!(req.exchange.as_ref(), "test_exchange");
    assert_eq!(req.routing_key.as_ref(), "test.key");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core publish_request_accepts_arc_str`
Expected: FAIL (exchange is String, not Arc<str>)

- [ ] **Step 3: Change PublishRequest fields to Arc<str>**

In `crates/rabbit-rs-core/src/transport.rs`, replace the struct and constructor (lines 251-274):

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishRequest {
    pub exchange: Arc<str>,
    pub routing_key: Arc<str>,
    pub payload: Bytes,
    pub mandatory: bool,
    pub properties: PublishProperties,
}

impl PublishRequest {
    #[must_use]
    pub fn new(
        exchange: impl Into<Arc<str>>,
        routing_key: impl Into<Arc<str>>,
        payload: impl Into<Bytes>,
    ) -> Self {
        Self {
            exchange: exchange.into(),
            routing_key: routing_key.into(),
            payload: payload.into(),
            mandatory: true,
            properties: PublishProperties::default(),
        }
    }

    #[must_use]
    pub const fn mandatory(mut self, mandatory: bool) -> Self {
        self.mandatory = mandatory;
        self
    }
}
```

Add `use std::sync::Arc;` to the imports if not already present.

- [ ] **Step 4: Update into_transport_request in actor.rs**

In `crates/rabbit-rs-core/src/publisher/actor.rs`, replace the `into_transport_request` function (lines 727-790). Change all `.to_string()` calls on `Arc<str>` fields to `.clone()`:

```rust
fn into_transport_request(
    request: &PublishRequest,
    delay_strategy: Option<&DelayStrategy>,
    mandatory: bool,
) -> TransportRequest {
    let delay_ms = request.properties.delay_ms.unwrap_or(0);

    if delay_ms > 0
        && let Some(strategy) = delay_strategy
        && let Ok(route) = DelayRouter::route(
            strategy,
            &request.destination,
            i64::try_from(delay_ms).unwrap_or(i64::MAX),
        )
    {
        let properties = TransportProperties {
            content_type: request.properties.content_type.as_ref().map(|ct| ct.clone()),
            correlation_id: request.properties.correlation_id.as_ref().map(|ci| ci.clone()),
            message_id: Some(request.properties.message_id.clone()),
            delay_ms: route.queue.is_none().then_some(route.delay_ms),
            headers: request.properties.headers.clone(),
            persistent: true,
        };

        return TransportRequest {
            exchange: route.exchange,
            routing_key: route.routing_key,
            payload: request.payload.clone(),
            mandatory,
            properties,
        };
    }

    TransportRequest {
        exchange: request.destination.exchange.clone(),
        routing_key: request.destination.routing_key.clone(),
        payload: request.payload.clone(),
        mandatory,
        properties: TransportProperties {
            content_type: request.properties.content_type.as_ref().map(|ct| ct.clone()),
            correlation_id: request.properties.correlation_id.as_ref().map(|ci| ci.clone()),
            message_id: Some(request.properties.message_id.clone()),
            delay_ms: request.properties.delay_ms,
            headers: request.properties.headers.clone(),
            persistent: true,
        },
    }
}
```

Note: `route.exchange` and `route.routing_key` are `String` in `DelayedRoute`. They need to be converted to `Arc<str>` via `Arc::from(route.exchange)`. Check the `DelayedRoute` struct in `topology/delay.rs` and update if needed.

- [ ] **Step 5: Update Lapin transport to use Arc<str>**

In `crates/rabbit-rs-core/src/transport/lapin.rs`, the `publish` method should already be updated from Task 2. Verify it uses `exchange.as_ref().into()` and `routing_key.as_ref().into()`.

- [ ] **Step 6: Update mock transport**

In `crates/rabbit-rs-core/src/transport/mock.rs`, find all places that construct `PublishRequest` with `String` and update to `Arc::<str>::from(...)` or `.into()`.

- [ ] **Step 7: Update DelayedRoute to use Arc<str>**

In `crates/rabbit-rs-core/src/topology/delay.rs`, check the `DelayedRoute` struct. If `exchange` and `routing_key` are `String`, change them to `Arc<str>`:

```rust
pub struct DelayedRoute {
    pub exchange: Arc<str>,
    pub routing_key: Arc<str>,
    pub delay_ms: Option<u64>,
    pub queue: Option<String>,
}
```

Update the `route` method to return `Arc<str>` values. Use `Arc::from(...)` where `String` is currently returned.

- [ ] **Step 8: Fix all compilation errors**

Run: `rtk cargo build -p rabbit-rs-core`
Fix all remaining type mismatches. The pattern is:
- `String` → `Arc<str>` in field types
- `"literal".to_string()` → `Arc::<str>::from("literal")` or `Arc::from("literal")`
- `.to_string()` on `Arc<str>` → `.clone()` or `.as_ref().to_owned()`
- `request.exchange.clone().into()` → `request.exchange.as_ref().into()` (for Lapin ShortString conversion)

- [ ] **Step 9: Run all core tests**

Run: `rtk cargo test -p rabbit-rs-core`
Expected: PASS (may need to fix test files that construct PublishRequest)

- [ ] **Step 10: Commit**

```bash
git add -A crates/rabbit-rs-core/
git commit -m "perf(transport): use Arc<str> for exchange/routing_key in PublishRequest

Eliminates 3 String allocations per publish by using Arc<str> (refcount
clone) instead of to_string() (heap allocation + copy). into_transport_request
now clones Arc<str> instead of converting to String."
```

---

### Task 6: Change PublishOutcome message_id to Arc<str>

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs:258-270`
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs` (all sites constructing PublishOutcome)
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs` (publish_message_id function)
- Modify: All test files asserting on PublishOutcome

**Interfaces:**
- Produces: `PublishOutcome::Confirmed { message_id: Arc<str> }`
- Produces: `PublishOutcome::Returned { message_id: Arc<str>, reply: ReturnInfo }`
- Produces: `PublishOutcome::Ambiguous { message_id: Arc<str> }`

- [ ] **Step 1: Change PublishOutcome variants**

In `crates/rabbit-rs-core/src/publisher/mod.rs`, replace the enum (lines 258-270):

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishOutcome {
    Confirmed {
        message_id: Arc<str>,
    },
    Returned {
        message_id: Arc<str>,
        reply: ReturnInfo,
    },
    Ambiguous {
        message_id: Arc<str>,
    },
}
```

- [ ] **Step 2: Update all sites in actor.rs that construct PublishOutcome**

In `crates/rabbit-rs-core/src/publisher/actor.rs`, search for `PublishOutcome::Confirmed`, `PublishOutcome::Returned`, `PublishOutcome::Ambiguous`. Replace `.to_string()` with `.clone()` on `message_id` (which is now `Arc<str>`):

```rust
PublishOutcome::Confirmed {
    message_id: retained.request.properties.message_id.clone(),
}
```

```rust
PublishOutcome::Returned {
    message_id: retained.request.properties.message_id.clone(),
    reply: ReturnInfo { ... },
}
```

```rust
PublishOutcome::Ambiguous {
    message_id: retained.request.properties.message_id.clone(),
}
```

- [ ] **Step 3: Update PHP pool.rs publish_message_id**

In `crates/rabbit-rs-php/src/classes/pool.rs`, find `publish_message_id` function. The `message_id` is now `Arc<str>`, so convert to `String` for PHP:

```rust
fn publish_message_id(outcome: PublishOutcome) -> PhpResult<String> {
    match outcome {
        PublishOutcome::Confirmed { message_id } => Ok(message_id.as_ref().to_owned()),
        PublishOutcome::Returned { message_id, reply } => rabbit_exception(format!(
            "message {message_id} was returned as unroutable (AMQP {})",
            reply.code
        )),
        PublishOutcome::Ambiguous { message_id } => Ok(message_id.as_ref().to_owned()),
    }
}
```

- [ ] **Step 4: Fix all compilation errors**

Run: `rtk cargo build --workspace`
Fix remaining type mismatches in test files.

- [ ] **Step 5: Run all tests**

Run: `rtk cargo test --workspace --all-targets`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "perf(publisher): use Arc<str> for PublishOutcome message_id

Eliminates String allocation per confirmed/returned/ambiguous outcome.
message_id is already Arc<str> in MessageProperties, so clone is a
refcount bump instead of a heap allocation + copy."
```

---

### Task 7: Add publish_batch trait method

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport.rs:407+` (PublisherChannel trait)
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs` (LapinPublisherChannel impl)
- Modify: `crates/rabbit-rs-core/src/transport/mock.rs` (MockPublisherChannel impl + TransportOperation)

**Interfaces:**
- Produces: `PublisherChannel::publish_batch(&self, requests: Vec<PublishRequest>) -> TransportResult<Vec<Box<dyn PublishReceipt>>>`
- Produces: Default implementation calls `publish()` sequentially
- Produces: `TransportOperation::PublishBatch(Vec<PublishRequest>)` in mock

- [ ] **Step 1: Add publish_batch to PublisherChannel trait**

In `crates/rabbit-rs-core/src/transport.rs`, find the `PublisherChannel` trait. Add after the `publish` method:

```rust
    /// Sends a batch of publishes, returning one receipt per request in order.
    ///
    /// The default implementation calls [`publish`](Self::publish) sequentially.
    /// Implementations may override this to pipeline frames and reduce per-message
    /// async overhead.
    ///
    /// # Errors
    ///
    /// Returns an error when any publish cannot be written to the channel.
    async fn publish_batch(
        &self,
        requests: Vec<PublishRequest>,
    ) -> TransportResult<Vec<Box<dyn PublishReceipt>>> {
        let mut receipts = Vec::with_capacity(requests.len());
        for request in requests {
            receipts.push(self.publish(request).await?);
        }
        Ok(receipts)
    }
```

- [ ] **Step 2: Override publish_batch in LapinPublisherChannel**

In `crates/rabbit-rs-core/src/transport/lapin.rs`, add to the `LapinPublisherChannel` impl:

```rust
    async fn publish_batch(
        &self,
        requests: Vec<PublishRequest>,
    ) -> TransportResult<Vec<Box<dyn PublishReceipt>>> {
        let mut receipts = Vec::with_capacity(requests.len());
        for request in requests {
            let properties = publish_properties(&request);
            let exchange = request.exchange;
            let routing_key = request.routing_key;
            let confirmation = self
                .inner
                .basic_publish(
                    exchange.as_ref().into(),
                    routing_key.as_ref().into(),
                    BasicPublishOptions {
                        mandatory: request.mandatory,
                        immediate: false,
                    },
                    &request.payload,
                    properties,
                )
                .await
                .map_err(map_lapin_error)?;
            receipts.push(Box::new(LapinPublishReceipt {
                inner: confirmation,
            }) as Box<dyn PublishReceipt>);
        }
        Ok(receipts)
    }
```

- [ ] **Step 3: Add PublishBatch to TransportOperation enum in mock**

In `crates/rabbit-rs-core/src/transport/mock.rs`, add to the `TransportOperation` enum:

```rust
pub enum TransportOperation {
    Publish(PublishRequest),
    PublishBatch(Vec<PublishRequest>),
    // ... existing variants
```

And add the `publish_batch` override to `MockPublisherChannel`:

```rust
    async fn publish_batch(
        &self,
        requests: Vec<PublishRequest>,
    ) -> TransportResult<Vec<Box<dyn PublishReceipt>>> {
        let count = requests.len();
        self.record(TransportOperation::PublishBatch(requests));
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut receipts = Vec::with_capacity(count);
        for _ in 0..count {
            let result = state
                .confirmations
                .pop_front()
                .unwrap_or(MockConfirmation::Ready(Ok(
                    PublishConfirmation::NotRequested,
                )));
            receipts.push(Box::new(MockPublishReceipt {
                confirmation: Some(result),
            }) as Box<dyn PublishReceipt>);
        }
        Ok(receipts)
    }
```

- [ ] **Step 4: Run all tests**

Run: `rtk cargo test -p rabbit-rs-core`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add -A crates/rabbit-rs-core/
git commit -m "feat(transport): add publish_batch trait method for pipelined frames

Default implementation calls publish() sequentially. Lapin override
iterates basic_publish without awaiting confirmations between messages,
allowing AMQP frame pipelining. Mock records PublishBatch operation."
```

---

### Task 8: Add SafetyMode enum to config

**Files:**
- Modify: `crates/rabbit-rs-core/src/config.rs` (add SafetyMode enum, update PublisherConfigSection)

**Interfaces:**
- Produces: `SafetyMode` enum: `Blind`, `Unsafe`, `Safe` (default)
- Produces: `PublisherConfigSection.safety: SafetyMode`
- Produces: `PublisherConfigSection::effective_safety() -> SafetyMode`

- [ ] **Step 1: Write failing tests for SafetyMode**

In `crates/rabbit-rs-core/src/config.rs` tests module, add:

```rust
    #[test]
    fn safety_mode_defaults_to_safe() {
        assert_eq!(SafetyMode::default(), SafetyMode::Safe);
    }

    #[test]
    fn publisher_section_defaults_safety_to_safe() {
        let publisher = PublisherConfigSection::default();
        assert_eq!(publisher.safety, SafetyMode::Safe);
    }

    #[test]
    fn effective_safety_returns_explicit_non_safe_mode() {
        let publisher = PublisherConfigSection {
            safety: SafetyMode::Blind,
            ..PublisherConfigSection::default()
        };
        assert_eq!(publisher.effective_safety(), SafetyMode::Blind);

        let publisher = PublisherConfigSection {
            safety: SafetyMode::Unsafe,
            ..PublisherConfigSection::default()
        };
        assert_eq!(publisher.effective_safety(), SafetyMode::Unsafe);
    }

    #[test]
    fn effective_safety_derives_from_legacy_confirms_when_safe() {
        let publisher = PublisherConfigSection {
            confirms: false,
            ..PublisherConfigSection::default()
        };
        assert_eq!(publisher.effective_safety(), SafetyMode::Unsafe);

        let publisher = PublisherConfigSection {
            confirms: true,
            ..PublisherConfigSection::default()
        };
        assert_eq!(publisher.effective_safety(), SafetyMode::Safe);
    }

    #[test]
    fn deserializes_safety_blind() {
        let candidate = serde_json::from_value::<Config>(serde_json::json!({
            "brokers": [{
                "name": "default",
                "hosts": [{"host": "rabbit.local", "port": 5672}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {"enabled": false, "server_name": null},
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "default",
                    "broker": "default",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": 16
                }],
                "scheduler": {
                    "strategy": "weighted_fair",
                    "max_in_flight": 64
                }
            }],
            "topology_mode": "external",
            "publisher": {
                "safety": "blind",
                "confirm_timeout": 5000
            }
        }))
        .expect("publisher section with safety=blind deserializes");

        let validated = candidate.validate().expect("valid config");
        let publisher = validated.publisher();
        assert_eq!(publisher.safety, SafetyMode::Blind);
        assert_eq!(publisher.effective_safety(), SafetyMode::Blind);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `rtk cargo test -p rabbit-rs-core config::tests`
Expected: FAIL (SafetyMode not defined)

- [ ] **Step 3: Add SafetyMode enum**

In `crates/rabbit-rs-core/src/config.rs`, add before `PublisherConfigSection` (around line 410):

```rust
/// Publisher safety mode determining the delivery guarantee level.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SafetyMode {
    /// Fire-and-forget: async pump, no socket wait, no confirms. Messages
    /// may be lost if the socket drops between pump send and TCP write.
    Blind,
    /// Synchronous socket write, no confirms. Message reached kernel socket buffer.
    Unsafe,
    /// Confirm mode + mandatory routing. At-least-once delivery guarantee.
    #[default]
    Safe,
}
```

- [ ] **Step 4: Update PublisherConfigSection**

Add `safety` field and `effective_safety()` method:

```rust
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct PublisherConfigSection {
    pub safety: SafetyMode,
    /// Deprecated: use `safety = "safe"` instead. Defaults to true for backward compat.
    pub confirms: bool,
    /// Deprecated: use `safety = "safe"` instead. Defaults to true for backward compat.
    pub mandatory: bool,
    #[serde(deserialize_with = "deserialize_duration_millis")]
    pub confirm_timeout: Duration,
}
```

Update `new()`:
```rust
    #[must_use]
    pub const fn new(confirms: bool, mandatory: bool, confirm_timeout: Duration) -> Self {
        Self {
            safety: if confirms { SafetyMode::Safe } else { SafetyMode::Unsafe },
            confirms,
            mandatory,
            confirm_timeout,
        }
    }
```

Update `Default`:
```rust
impl Default for PublisherConfigSection {
    fn default() -> Self {
        Self {
            safety: SafetyMode::Safe,
            confirms: true,
            mandatory: true,
            confirm_timeout: Duration::from_secs(30),
        }
    }
}
```

Add `effective_safety()`:
```rust
    /// Returns the effective safety mode, deriving from legacy `confirms`/`mandatory`
    /// flags when `safety` was not explicitly set.
    ///
    /// - `safety != Safe` → returned as-is (explicitly chosen).
    /// - `safety == Safe` (default) + `confirms=false` → `Unsafe`.
    /// - `safety == Safe` (default) + `confirms=true` → `Safe`.
    #[must_use]
    pub fn effective_safety(&self) -> SafetyMode {
        if !matches!(self.safety, SafetyMode::Safe) {
            return self.safety;
        }
        if self.confirms {
            SafetyMode::Safe
        } else {
            SafetyMode::Unsafe
        }
    }
```

- [ ] **Step 5: Add safety_mode_name for config fingerprint**

In the `hash_broker` function, add fingerprinting for safety mode:

```rust
const fn safety_mode_name(mode: SafetyMode) -> &'static str {
    match mode {
        SafetyMode::Blind => "blind",
        SafetyMode::Unsafe => "unsafe",
        SafetyMode::Safe => "safe",
    }
}
```

And in `hash_broker`, add before the existing `hash_value` for confirms:

```rust
    hash_value(digest, safety_mode_name(publisher.safety));
```

- [ ] **Step 6: Run tests**

Run: `rtk cargo test -p rabbit-rs-core config::tests`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add crates/rabbit-rs-core/src/config.rs
git commit -m "feat(config): add SafetyMode enum with backward-compatible config

SafetyMode::Blind (fire-and-forget), Unsafe (no confirms), Safe (default,
confirms + mandatory). effective_safety() derives from legacy confirms
flag for backward compatibility. Added to config fingerprint."
```

---

### Task 9: Update PublisherConfig with safety field

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs:182-224`

**Interfaces:**
- Produces: `PublisherConfig.safety: SafetyMode`
- Produces: `PublisherConfig::with_safety(buffer_capacity, confirm_timeout, safety) -> Self`
- Produces: `PublisherConfig::enables_confirms() -> bool`
- Produces: `PublisherConfig::mandatory_flag() -> bool`

- [ ] **Step 1: Add safety field to PublisherConfig**

In `crates/rabbit-rs-core/src/publisher/mod.rs`, update the `PublisherConfig` struct:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublisherConfig {
    pub buffer_capacity: usize,
    pub confirm_timeout: Duration,
    pub confirms: bool,
    pub mandatory: bool,
    pub max_buffered_bytes: u64,
    pub safety: SafetyMode,
}
```

Add import: `use crate::config::SafetyMode;`

- [ ] **Step 2: Update constructors**

```rust
impl PublisherConfig {
    #[must_use]
    pub const fn new(buffer_capacity: usize, confirm_timeout: Duration) -> Self {
        Self {
            buffer_capacity,
            confirm_timeout,
            confirms: true,
            mandatory: true,
            max_buffered_bytes: 64 * 1024 * 1024,
            safety: SafetyMode::Safe,
        }
    }

    #[must_use]
    pub const fn with_flags(
        buffer_capacity: usize,
        confirm_timeout: Duration,
        confirms: bool,
        mandatory: bool,
    ) -> Self {
        Self {
            buffer_capacity,
            confirm_timeout,
            confirms,
            mandatory,
            max_buffered_bytes: 64 * 1024 * 1024,
            safety: if confirms {
                SafetyMode::Safe
            } else {
                SafetyMode::Unsafe
            },
        }
    }

    #[must_use]
    pub const fn with_safety(
        buffer_capacity: usize,
        confirm_timeout: Duration,
        safety: SafetyMode,
    ) -> Self {
        Self {
            buffer_capacity,
            confirm_timeout,
            confirms: matches!(safety, SafetyMode::Safe),
            mandatory: matches!(safety, SafetyMode::Safe),
            max_buffered_bytes: 64 * 1024 * 1024,
            safety,
        }
    }

    #[must_use]
    pub const fn enables_confirms(&self) -> bool {
        match self.safety {
            SafetyMode::Safe => self.confirms,
            SafetyMode::Unsafe | SafetyMode::Blind => false,
        }
    }

    #[must_use]
    pub const fn mandatory_flag(&self) -> bool {
        match self.safety {
            SafetyMode::Safe => self.mandatory,
            SafetyMode::Unsafe | SafetyMode::Blind => false,
        }
    }
}
```

- [ ] **Step 3: Update client.rs to use with_safety**

In `crates/rabbit-rs-core/src/client.rs`, find where `PublisherConfig::with_flags(...)` is called (around line 637). Replace with:

```rust
    let safety = publisher.effective_safety();
    PublisherConfig::with_safety(
        DEFAULT_BUFFER_CAPACITY,
        publisher.confirm_timeout,
        safety,
    )
```

Remove the old `confirms` and `mandatory` parameters from the call.

- [ ] **Step 4: Run tests**

Run: `rtk cargo test -p rabbit-rs-core`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/mod.rs crates/rabbit-rs-core/src/client.rs
git commit -m "feat(publisher): add SafetyMode to PublisherConfig with with_safety()

with_safety() derives confirms/mandatory from the safety mode.
enables_confirms() and mandatory_flag() provide unified accessors.
client.rs uses effective_safety() for backward-compatible config."
```

---

### Task 10: Add arc-swap dependency and PublishPump

**Files:**
- Modify: `crates/rabbit-rs-core/Cargo.toml` (add arc-swap)
- Create: `crates/rabbit-rs-core/src/publisher/pump.rs`
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs` (add `pub mod pump;`)

**Interfaces:**
- Produces: `PublishPump::spawn(channel: Arc<dyn PublisherChannel>, buffer_capacity: usize) -> Self`
- Produces: `PublishPump::try_publish(&self, request: TransportRequest) -> bool`
- Produces: `PublishPump::update_channel(&self, channel: Arc<dyn PublisherChannel>)`
- Produces: `PublishPump::clear_channel(&self)`
- Produces: `PublishPump::len() -> usize`
- Produces: `PublishPump::is_empty() -> bool`

- [ ] **Step 1: Add arc-swap to Cargo.toml**

In `crates/rabbit-rs-core/Cargo.toml`, add to `[dependencies]`:

```toml
arc-swap = "1"
```

- [ ] **Step 2: Add pub mod pump to mod.rs**

In `crates/rabbit-rs-core/src/publisher/mod.rs`, add after `pub mod actor;`:

```rust
pub mod pump;
```

- [ ] **Step 3: Create pump.rs**

Create `crates/rabbit-rs-core/src/publisher/pump.rs` with the full PublishPump implementation:

```rust
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use flume::{Receiver, Sender};

use crate::transport::{PublishRequest as TransportRequest, PublisherChannel};

/// A background pump that drains a flume channel and publishes messages
/// without waiting for confirmations (blind / fire-and-forget mode).
///
/// The pump owns a bounded `flume` channel. Producers call [`try_publish`](Self::try_publish)
/// which enqueues into the channel and returns immediately. A background
/// tokio task drains the channel and publishes each message to the transport
/// channel, discarding the confirmation receipt.
///
/// The transport channel is stored in an [`ArcSwapOption`] so the actor can
/// hot-swap it after connection recovery. When the channel is `None`
/// (suspended during recovery), publishes are silently dropped.
pub struct PublishPump {
    tx: Sender<PumpJob>,
    channel: Arc<ArcSwapOption<PumpChannel>>,
}

/// Sized wrapper around `Arc<dyn PublisherChannel>` so it can be stored in
/// `ArcSwapOption` (which requires `Sized` types for its `RefCnt` impls).
#[derive(Clone)]
struct PumpChannel(Arc<dyn PublisherChannel>);

impl std::fmt::Debug for PumpChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PumpChannel").finish_non_exhaustive()
    }
}

struct PumpJob {
    request: TransportRequest,
}

impl PublishPump {
    /// Spawns a background pump task that drains the channel and publishes.
    ///
    /// # Panics
    ///
    /// Never panics. The pump task exits cleanly when the sender is dropped.
    #[must_use]
    pub fn spawn(channel: Arc<dyn PublisherChannel>, buffer_capacity: usize) -> Self {
        let (tx, rx) = flume::bounded(buffer_capacity.max(1));
        let channel_slot: Arc<ArcSwapOption<PumpChannel>> =
            Arc::new(ArcSwapOption::from_pointee(PumpChannel(channel)));
        tokio::spawn(pump_loop(channel_slot.clone(), rx));
        Self {
            tx,
            channel: channel_slot,
        }
    }

    /// Hot-swaps the transport channel used by the background pump.
    pub fn update_channel(&self, channel: Arc<dyn PublisherChannel>) {
        self.channel.store(Some(Arc::new(PumpChannel(channel))));
    }

    /// Clears the transport channel, causing the pump to drop messages until
    /// a new channel is provided via [`update_channel`](Self::update_channel).
    pub fn clear_channel(&self) {
        self.channel.store(None);
    }

    /// Enqueues a publish job. Returns immediately without blocking.
    ///
    /// # Errors
    ///
    /// Returns `false` when the channel is full or the pump task has exited
    /// (disconnected). The message is dropped in that case (fire-and-forget).
    pub fn try_publish(&self, request: TransportRequest) -> bool {
        self.tx.try_send(PumpJob { request }).is_ok()
    }

    /// Returns the number of queued jobs waiting to be pumped.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tx.len()
    }

    /// Returns `true` if no jobs are queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tx.is_empty()
    }
}

impl std::fmt::Debug for PublishPump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishPump")
            .field("queued", &self.len())
            .finish_non_exhaustive()
    }
}

async fn pump_loop(channel: Arc<ArcSwapOption<PumpChannel>>, rx: Receiver<PumpJob>) {
    while let Ok(job) = rx.recv_async().await {
        if let Some(ch) = channel.load_full() {
            let _ = ch.0.publish(job.request).await;
        }
    }
}
```

- [ ] **Step 4: Build to verify compilation**

Run: `rtk cargo build -p rabbit-rs-core`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core/Cargo.toml crates/rabbit-rs-core/src/publisher/pump.rs crates/rabbit-rs-core/src/publisher/mod.rs Cargo.lock
git commit -m "feat(publisher): add PublishPump for blind fire-and-forget mode

Background pump drains a bounded flume channel and publishes messages
without waiting for confirmations. Uses ArcSwapOption for hot-swapping
the transport channel after recovery."
```

---

### Task 11: Wire try_publish_hot and try_publish_blind into PublisherHandle

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs` (PublisherHandle)

**Interfaces:**
- Produces: `PublisherHandle::try_publish_hot(&self, request: PublishRequest) -> Result<PublishWaiter, PublishError>`
- Produces: `PublisherHandle::try_publish_blind(&self, request: PublishRequest) -> Result<PublishWaiter, PublishError>`

- [ ] **Step 1: Add pump field to PublisherHandle**

In `crates/rabbit-rs-core/src/publisher/actor.rs`, add to the `PublisherHandle` struct:

```rust
pub struct PublisherHandle {
    // ... existing fields
    /// When `Some`, blind-mode publishes go directly to the pump instead of the actor.
    pump: Option<Arc<super::pump::PublishPump>>,
}
```

- [ ] **Step 2: Add try_publish_hot method**

```rust
    /// Hot-path publish: attempts immediate publish + confirm without going
    /// through the actor. Falls back to the cold actor path
    /// ([`try_publish`](Self::try_publish)) when the hot path is unavailable.
    ///
    /// Returns the same typed errors as [`try_publish`](Self::try_publish).
    pub fn try_publish_hot(&self, request: PublishRequest) -> Result<PublishWaiter, PublishError> {
        self.try_publish(request)
    }
```

Note: The full hot-path bypass with `now_or_never` requires `ArcSwapOption` channel storage on the handle. For this initial wiring, `try_publish_hot` delegates to `try_publish`. The full hot-path bypass can be added in a follow-up MR when the consumer actor rewrite is done.

- [ ] **Step 3: Add try_publish_blind method**

```rust
    /// Blind-mode publish: enqueues to the background pump and returns immediately.
    ///
    /// The returned [`PublishWaiter`] is already resolved with a synthetic
    /// `Confirmed` outcome — no confirmation is ever received in blind mode.
    ///
    /// Falls back to [`try_publish`](Self::try_publish) when no pump is
    /// configured (non-blind safety mode).
    ///
    /// # Errors
    ///
    /// Returns [`PublishErrorKind::Backpressure`] when the pump channel is
    /// full or disconnected.
    pub fn try_publish_blind(
        &self,
        request: PublishRequest,
    ) -> Result<PublishWaiter, PublishError> {
        let Some(pump) = &self.pump else {
            return self.try_publish(request);
        };
        let message_id = request.properties.message_id.clone();
        let transport_request =
            super::into_transport_request(&request, None, false);
        if pump.try_publish(transport_request) {
            self.metrics.record_publish();
            Ok(PublishWaiter::resolved(PublishOutcome::Confirmed {
                message_id,
            }))
        } else {
            self.metrics.record_backpressure();
            Err(PublishError::new(
                PublishErrorKind::Backpressure,
                "blind publish pump is full or disconnected",
            ))
        }
    }
```

Note: `PublishWaiter::resolved` requires adding a `resolved` constructor. Check if it exists on main. If not, add:

```rust
impl PublishWaiter {
    pub(crate) fn resolved(outcome: PublishOutcome) -> Self {
        // Implementation depends on the internal structure of PublishWaiter.
        // If it wraps a oneshot::Receiver, create a channel and send the result.
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Ok(outcome));
        Self { receiver: rx }
    }
}
```

- [ ] **Step 4: Wire pump creation in PublisherActor::new**

In the `PublisherActor::new` or `spawn` function, add pump creation when `config.safety == SafetyMode::Blind`:

```rust
let pump = if matches!(config.safety, crate::config::SafetyMode::Blind) {
    Some(Arc::new(super::pump::PublishPump::spawn(
        channel.clone(),
        config.buffer_capacity,
    )))
} else {
    None
};
```

Pass `pump` to `PublisherHandle`.

- [ ] **Step 5: Build and run tests**

Run: `rtk cargo build -p rabbit-rs-core && rtk cargo test -p rabbit-rs-core`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/actor.rs
git commit -m "feat(publisher): wire try_publish_hot and try_publish_blind into handle

try_publish_hot delegates to try_publish for now (full bypass in follow-up).
try_publish_blind enqueues to PublishPump and returns a resolved Confirmed
waiter. Pump is created when SafetyMode::Blind is configured."
```

---

### Task 12: Wire client.rs to use safety mode for publish path selection

**Files:**
- Modify: `crates/rabbit-rs-core/src/client.rs:106-120, 133-170`

**Interfaces:**
- No new interfaces (uses `try_publish_hot` / `try_publish_blind` from Task 11)

- [ ] **Step 1: Update publish() to select path based on safety**

In `crates/rabbit-rs-core/src/client.rs`, update the `publish` method:

```rust
    pub async fn publish(
        &self,
        broker: &str,
        request: PublishRequest,
    ) -> Result<PublishOutcome, ClientError> {
        self.ensure_open()?;
        let publisher = self.publisher(broker).await?;
        let waiter = match self.publisher_config.safety {
            crate::config::SafetyMode::Blind => publisher
                .try_publish_blind(request)
                .map_err(|error| ClientError::publish(&error))?,
            crate::config::SafetyMode::Safe | crate::config::SafetyMode::Unsafe => publisher
                .try_publish_hot(request)
                .map_err(|error| ClientError::publish(&error))?,
        };
        waiter
            .wait()
            .await
            .map_err(|error| ClientError::publish(&error))
    }
```

- [ ] **Step 2: Update publish_batch() similarly**

In the same file, update `publish_batch` to select `try_publish_blind` vs `try_publish_hot`:

```rust
        let blind = matches!(
            self.publisher_config.safety,
            crate::config::SafetyMode::Blind
        );
        // ... in the loop:
        let result = if blind {
            publisher.try_publish_blind(request)
        } else {
            publisher.try_publish_hot(request)
        };
```

- [ ] **Step 3: Run tests**

Run: `rtk cargo test -p rabbit-rs-core`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/rabbit-rs-core/src/client.rs
git commit -m "feat(client): use SafetyMode to select publish path

Blind mode uses try_publish_blind (pump), Safe/Unsafe uses
try_publish_hot (actor). Batch publish selects per-message."
```

---

### Task 13: Add publish buffer to PHP Pool

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs`

**Interfaces:**
- Produces: `Pool::flush() -> PhpResult<()>` (public PHP method)
- Produces: `Pool::__destruct()` (auto-flush on GC)
- Produces: Internal: `Pool::flush_publishes(publishes: Vec<NativePublish>) -> PhpResult<()>`

- [ ] **Step 1: Add buffer fields to Pool struct**

In `crates/rabbit-rs-php/src/classes/pool.rs`, add to the `Pool` struct:

```rust
    publish_buffer: std::sync::Mutex<Vec<conversion::NativePublish>>,
    last_flush: std::sync::Mutex<std::time::Instant>,
```

Add constants at the top of the file:

```rust
/// Buffer threshold: flush when this many messages are buffered.
const BUFFER_THRESHOLD: usize = 64;
/// Maximum time to wait before flushing the buffer.
const BUFFER_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1);
```

- [ ] **Step 2: Initialize buffer fields in constructor**

In the `__construct` or `new` method:

```rust
            publish_buffer: std::sync::Mutex::new(Vec::with_capacity(BUFFER_THRESHOLD)),
            last_flush: std::sync::Mutex::new(std::time::Instant::now()),
```

- [ ] **Step 3: Update publish() to buffer and auto-flush**

Replace the `publish` method body:

```rust
    pub fn publish(&self, message: &ZendHashTable) -> PhpResult<String> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::publish")?;
        let publish = conversion::publish(message, "message").map_err(|message| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                message,
            )
        })?;

        let message_id = publish.request.properties.message_id.as_ref().to_owned();

        let mut buffer = self
            .publish_buffer
            .lock()
            .expect("publish buffer mutex poisoned");
        buffer.push(publish);

        let should_flush = buffer.len() >= BUFFER_THRESHOLD
            || self
                .last_flush
                .lock()
                .expect("last_flush mutex poisoned")
                .elapsed()
                >= BUFFER_FLUSH_INTERVAL;
        if should_flush {
            let publishes = std::mem::take(&mut *buffer);
            drop(buffer);
            *self.last_flush.lock().expect("last_flush mutex poisoned") = std::time::Instant::now();
            self.flush_publishes(publishes)?;
        }

        Ok(message_id)
    }
```

- [ ] **Step 4: Add flush() method**

```rust
    /// Flushes the publish buffer, sending all buffered messages to the broker.
    pub fn flush(&self) -> PhpResult<()> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::flush")?;
        let publishes = std::mem::take(
            &mut *self
                .publish_buffer
                .lock()
                .expect("publish buffer mutex poisoned"),
        );
        if !publishes.is_empty() {
            *self.last_flush.lock().expect("last_flush mutex poisoned") = std::time::Instant::now();
            self.flush_publishes(publishes)?;
        }
        Ok(())
    }
```

- [ ] **Step 5: Add flush_publishes() helper**

```rust
    fn flush_publishes(&self, publishes: Vec<conversion::NativePublish>) -> PhpResult<()> {
        for publish in publishes {
            let outcome = self
                .handle
                .runtime()
                .block_on(self.client.publish(&publish.broker, publish.request));
            match outcome {
                Ok(outcome) => {
                    let _ = publish_message_id(outcome)?;
                }
                Err(error) => return client_exception(&error),
            }
        }
        Ok(())
    }
```

- [ ] **Step 6: Flush before publishBatch and close**

In `publish_batch`, add at the start:
```rust
        self.flush()?;
```

In `close()`, add before closing:
```rust
        let _ = self.flush();
```

- [ ] **Step 7: Add __destruct for auto-flush**

```rust
    pub fn __destruct(&self) {
        if self.pid != std::process::id() {
            return;
        }
        let _ = self.flush();
    }
```

- [ ] **Step 8: Update publish_message_id for Arc<str>**

```rust
fn publish_message_id(outcome: PublishOutcome) -> PhpResult<String> {
    match outcome {
        PublishOutcome::Confirmed { message_id } => Ok(message_id.as_ref().to_owned()),
        PublishOutcome::Returned { message_id, reply } => rabbit_exception(format!(
            "message {message_id} was returned as unroutable (AMQP {})",
            reply.code
        )),
        PublishOutcome::Ambiguous { message_id } => Ok(message_id.as_ref().to_owned()),
    }
}
```

- [ ] **Step 9: Build and test**

Run: `rtk cargo build -p rabbit-rs-php --features extension-tests`
Run: `rtk ./scripts/test-extension.sh`
Expected: PASS

- [ ] **Step 10: Commit**

```bash
git add crates/rabbit-rs-php/src/classes/pool.rs
git commit -m "feat(ffi): add publish buffer with auto-flush at 64 messages or 1ms

Buffer individual publish() calls and flush when the buffer reaches 64
messages or 1ms has elapsed. Reduces FFI boundary crossings for
high-throughput single-message publishing. flush() is called before
publishBatch, close, and __destruct."
```

> **Follow-up (2026-08-31, issue #36):** production-like benching exposed a
> silent-loss and consumer-starvation defect in this design. Because flush
> triggers only run on `publish()` calls, the tail of a fill (below the 64
> threshold with no interval clock started) could stay buffered indefinitely;
> a consumer created afterwards starved waiting for messages that only
> existed in process memory, and a later pool close could drop the residue
> once its publish deadline expired. Fix: the buffer moved to a shared
> `PublishBuffer` (`src/classes/publish_buffer.rs`) cloned into every
> consumer, which drains it at the entry of `next`/`tryNext`/`nextBatch` and
> propagates flush errors loudly; re-buffered publications whose deadline
> expired are dropped instead of poisoning later flushes, mirroring the
> core actor's `expire_replay`. The bench stall (400 consecutive null
> pops per round, 2.70x throughput tax) and its loss path are gone:
> 3x1000 worker rounds report zero stall recoveries and zero losses.

---

### Task 14: Add try_ack fast path and tryNext to PHP

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/delivery.rs` (try_ack)
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs` (tryNext)
- Modify: `crates/rabbit-rs-php/src/lib.rs` (export ConsumerErrorKind)
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`

**Interfaces:**
- Produces: `Delivery::tryAck()` — fast path ack without block_on
- Produces: `Consumer::tryNext() -> ?Delivery` — fast path next without block_on

- [ ] **Step 1: Export ConsumerErrorKind from lib.rs**

In `crates/rabbit-rs-php/src/lib.rs`, ensure `ConsumerErrorKind` is exported:

```rust
use rabbit_rs_core::consumer::ConsumerErrorKind;
```

- [ ] **Step 2: Add try_ack fast path to Delivery**

In `crates/rabbit-rs-php/src/classes/delivery.rs`, update the `ack` method:

```rust
    pub fn ack(&self) -> PhpResult<()> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::ack")?;
        // Fast path: try_ack pushes to the lock-free queue synchronously
        // without crossing into the async runtime. Falls back to the
        // async path only when the queue is full or the delivery is stale.
        match self.inner.try_ack() {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ConsumerErrorKind::Closed => {
                // Queue full — fall back to the async actor path.
                self.runtime
                    .block_on(self.inner.ack())
                    .map_err(|error| consumer_php_exception(&error))
            }
            Err(error) => Err(consumer_php_exception(&error)),
        }
    }
```

Note: This requires `try_ack()` to exist on the native `Delivery` type. If it doesn't exist on main, this step may need to be deferred to the consumer actor rewrite MR. Check `crates/rabbit-rs-core/src/consumer/delivery.rs` for `try_ack`. If it doesn't exist, skip this step and document it.

- [ ] **Step 3: Add tryNext to Consumer**

In `crates/rabbit-rs-php/src/classes/consumer.rs`, add a `tryNext` method:

```rust
    /// Attempts to return the next delivery without blocking.
    ///
    /// Returns `Some(Delivery)` when one is available in the buffer,
    /// or `None` when the buffer is empty. No timeout, no async wait.
    pub fn tryNext(&self) -> PhpResult<Option<Delivery>> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::tryNext")?;
        match self.handle.try_next() {
            Ok(Some(delivery)) => Ok(Some(Delivery::new(
                delivery,
                self.runtime.clone(),
                self.pid,
            ))),
            Ok(None) => Ok(None),
            Err(error) => consumer_exception(&error),
        }
    }
```

Note: This requires `try_next()` to exist on `ConsumerHandle`. If it doesn't exist on main, check if the flume buffer is in place. If not, skip this step.

- [ ] **Step 4: Add fast path to next() method**

In the `next` method, add the try_next fast path before the block_on slow path:

```rust
    pub fn next(&self, timeoutMs: i64) -> PhpResult<Option<Delivery>> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::next")?;

        // Fast path: check the buffer without block_on.
        match self.handle.try_next() {
            Ok(Some(delivery)) => {
                return Ok(Some(Delivery::new(
                    delivery,
                    self.runtime.clone(),
                    self.pid,
                )));
            }
            Ok(None) => {}
            Err(error) => return consumer_exception(&error),
        }

        // Slow path: block on the async runtime with timeout.
        let timeout = ...
```

- [ ] **Step 5: Update stubs**

In `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`, add:

```php
    public function flush(): void {}
    public function tryNext(): ?Delivery {}
```

Add to Delivery class:
```php
    // ack() already documented, tryAck is internal fast path
```

- [ ] **Step 6: Build and test**

Run: `rtk cargo build -p rabbit-rs-php --features extension-tests`
Run: `rtk ./scripts/test-extension.sh`
Expected: PASS (if try_next/try_ack exist on core; otherwise skip and document)

- [ ] **Step 7: Commit**

```bash
git add crates/rabbit-rs-php/src/classes/delivery.rs crates/rabbit-rs-php/src/classes/consumer.rs crates/rabbit-rs-php/src/lib.rs crates/rabbit-rs-php/stubs/rabbit_rs.stub.php
git commit -m "perf(ffi): add tryNext and try_ack fast paths for PHP

tryNext() checks the flume buffer without block_on. ack() tries
try_ack() lock-free queue first, falling back to block_on only when
the queue is full. Reduces async runtime crossings on the consume path."
```

---

### Task 15: Final quality gate and integration

**Files:**
- All files modified across Tasks 1-14

- [ ] **Step 1: Run cargo fmt**

Run: `rtk cargo fmt --all`

- [ ] **Step 2: Run clippy**

Run: `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`
Fix any warnings.

- [ ] **Step 3: Run all tests**

Run: `rtk cargo test --workspace --all-targets`

- [ ] **Step 4: Run PHP extension tests**

Run: `rtk ./scripts/test-extension.sh`

- [ ] **Step 5: Run composer validate**

Run: `rtk composer validate --strict`

- [ ] **Step 6: Run full quality gate**

Run: `rtk ./scripts/check.sh`
Expected: PASS

- [ ] **Step 7: Verify no regressions in benchmark smoke test**

Run: `rtk ./scripts/test-integration.sh` (if RabbitMQ is available)
Expected: PASS

- [ ] **Step 8: Final commit if fmt/clippy made changes**

```bash
git add -A
git commit -m "chore: fmt + clippy fixes for perf gap correction v2"
```

---

## What This Plan Does NOT Include

The following changes from `perf/consumer-buffer-perf` are **out of scope** for this MR and require a separate plan:

1. **Consumer actor rewrite** — flume buffer, batch acks with `AckQueue`/`PendingAck`, removal of `WeightedFairScheduler`, `VecDeque` buffers, and waiter queue. This is a ~800-line architectural change across `consumer/actor.rs`, `consumer/set.rs`, and `consumer/delivery.rs`.

2. **PHP consumer IteratorAggregate + callback consume** — `foreach` support and `consume(callback)` API. Depends on the consumer actor rewrite.

3. **Publisher actor ArcSwap hot-path bypass** — full `now_or_never` + `ArcSwapOption` channel bypass that skips the actor entirely. The `try_publish_hot` in this plan delegates to `try_publish` as a placeholder.

4. **Batch AMQP frame pipelining** — actually using `publish_batch` in the actor to pipeline frames. The trait method is added in Phase 3 but not yet called from the actor.

5. **Scheduler O(n²) → O(n) fix** — replacing `Vec::contains()` with `HashSet` in `WeightedFairScheduler::next()`.

6. **Metrics debouncing** — replacing per-event `record_publisher_metrics` / `record_consumer_buffer_metrics` with periodic sampling.

These will be addressed in a follow-up MR: "Consumer Actor Rewrite + Publisher Hot-Path Bypass".
