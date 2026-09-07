# Runtime Worker Profile Synthesis Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `pop($queue)` work for any queue with no `workers.*` entry by synthesizing `__auto__.{queue}` worker profiles in the Rust core at first pop, with the topology plan computed at reconcile time (declare-on-use).

**Architecture:** `Client.requested_profiles` becomes a payload-carrying `BTreeMap<String, WorkerProfile>` (entries only enter via `consumer()` resolution). Unknown `__auto__.`-prefixed names synthesize a default profile (`ValidatedConfig::synthesize_auto_profile`); the frozen per-coordinator `TopologyPlan` is deleted and recomputed at each reconcile from config + requested extras. Laravel changes are test-only: `pop()` already routes `__auto__.` names to `pool->consumer()`.

**Tech Stack:** Rust 1.96 / edition 2024 (workspace `crates/rabbit-rs-core`), Pest (Laravel package), tokio with paused time + `MockTransport` for tests.

**Spec:** `docs/superpowers/specs/2026-09-06-runtime-worker-profiles-design.md` (shape S2)

## Global Constraints

- Rust pinned to 1.96.0, edition 2024. `#![forbid(unsafe_code)]` stays.
- TDD: every task starts with a failing test, observed failing, then minimal implementation.
- Tests use `MockTransport` + `#[tokio::test(start_paused = true)]`. No real sleeps.
- Deterministic recovery order preserved; #49 (requested-vs-dormant) preserved: map entries enter **only** through `consumer()`.
- All repo artifacts in English. Conventional commits (subject ≤ 70 chars), footer `Refs #167`.
- Quality gate before final claim: `rtk ./scripts/check.sh` (fmt + clippy `-D warnings` + nextest + composer validate).
- Run Rust checks with the `rtk` prefix; run `rtk cargo fmt --all` after each Rust edit.
- Work happens in worktree `.worktrees/feat-runtime-worker-profiles` (branch `feat/runtime-worker-profiles`).

---

### Task 1: `ValidatedConfig::synthesize_auto_profile` (core config)

**Files:**
- Modify: `crates/rabbit-rs-core/src/config.rs` (add `pub const AUTO_PROFILE_PREFIX`, `synthesize_auto_profile` in the `ValidatedConfig` impl near `worker()` at line ~845)
- Test: `crates/rabbit-rs-core/src/config.rs` (`mod tests` at file bottom)

**Interfaces:**
- Consumes: private `validate_worker(worker: &WorkerProfile, broker_names: &HashSet<&str>) -> Result<(), ConfigError>` (config.rs:695), private `default_max_buffered_bytes()`.
- Produces: `pub const AUTO_PROFILE_PREFIX: &str = "__auto__.";` and
  `impl ValidatedConfig { pub fn synthesize_auto_profile(&self, profile: &str) -> Result<WorkerProfile, ConfigError> }`.
  Task 3 consumes both.

- [ ] **Step 1: Write the failing tests** — append inside the existing `mod tests` in `config.rs` (reuse the module's existing helpers for building a validated config; if none exist for multi-broker, build `Config { brokers: vec![...], workers: vec![], .. }` directly like `tests/consumer.rs:57-70` does):

```rust
#[test]
fn synthesizes_default_profile_for_auto_name() {
    let config = Config {
        brokers: vec![broker_config("main")],
        workers: vec![],
        topology_mode: TopologyMode::Declare,
        ..Default::default() // follow the module's existing pattern for the remaining fields
    }
    .validate()
    .expect("valid config");

    let worker = config
        .synthesize_auto_profile("__auto__.emails")
        .expect("synthesis succeeds");

    assert_eq!(worker.name, "__auto__.emails");
    assert_eq!(worker.scheduler, SchedulerConfig::weighted_fair());
    let [subscription] = worker.subscriptions.as_slice() else {
        panic!("expected exactly one subscription");
    };
    assert_eq!(subscription.name, "auto");
    assert_eq!(subscription.queue, "emails");
    assert_eq!(subscription.broker, "main");
    assert_eq!(subscription.weight, 1);
    assert_eq!(subscription.priority_class, 0);
    assert_eq!(subscription.prefetch, PrefetchConfig::Fixed(64));
    assert_eq!(subscription.starvation_after, Duration::from_secs(30));
    assert!(!subscription.early_ack);
    assert!(!subscription.no_ack);
}

#[test]
fn synthesizes_only_for_auto_prefix() {
    let config = single_broker_config(); // same helper as above, factored locally
    let error = config
        .synthesize_auto_profile("orders")
        .expect_err("plain names are not synthesizable");
    assert_eq!(error.to_string(), "workers.orders: unknown worker profile");
}

#[test]
fn synthesizes_rejects_empty_queue_part() {
    let config = single_broker_config();
    let error = config
        .synthesize_auto_profile("__auto__.")
        .expect_err("empty queue part must be rejected");
    assert!(error.to_string().contains("must carry a queue"));
}

#[test]
fn synthesizes_rejects_multi_broker() {
    let config = Config {
        brokers: vec![broker_config("one"), broker_config("two")],
        workers: vec![],
        topology_mode: TopologyMode::Declare,
        ..Default::default()
    }
    .validate()
    .expect("valid config");

    let error = config
        .synthesize_auto_profile("__auto__.emails")
        .expect_err("multi-broker synthesis must fail");

    assert!(error.to_string().contains("single configured broker"));
}
```

Adapt constructor noise to how `config.rs` tests already build `Config` (the module has tests; mirror their helper functions instead of inventing new shapes).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `rtk cargo test -p rabbit-rs-core config::tests`
Expected: FAIL — `synthesize_auto_profile` / `AUTO_PROFILE_PREFIX` not defined.

- [ ] **Step 3: Implement** — in `config.rs`, next to the `ValidatedConfig` accessors (`worker()` at :845):

```rust
/// Prefix marking a worker profile name synthesized on first use (the
/// Laravel `auto_subscribe` contract). Synthesized profiles carry exactly
/// one subscription named `auto`, mirroring the defaults the Laravel
/// compiler emits for a fixed-prefetch connection.
pub const AUTO_PROFILE_PREFIX: &str = "__auto__.";
```

and inside `impl ValidatedConfig`:

```rust
/// Synthesizes the default worker profile for an `__auto__.name` profile.
///
/// The auto path resolves queues that no configured profile covers: the
/// synthesized profile subscribes to the queue named after the prefix on
/// the single configured broker, with the same defaults the Laravel
/// compiler emits (weight 1, priority class 0, fixed prefetch 64,
/// 30 s starvation, acknowledgements on). The profile is validated with
/// the same rules as configured profiles, so every bound applies.
///
/// # Errors
///
/// Returns a [`ConfigError`] when the name lacks the `__auto__.` prefix,
/// carries an empty queue part, more than one broker is configured, or the
/// synthesized profile fails validation.
pub fn synthesize_auto_profile(&self, profile: &str) -> Result<WorkerProfile, ConfigError> {
    const SUBSCRIPTION_NAME: &str = "auto";
    let Some(queue) = profile.strip_prefix(AUTO_PROFILE_PREFIX) else {
        return Err(ConfigError::new(
            format!("workers.{profile}"),
            "unknown worker profile",
        ));
    };
    if queue.is_empty() {
        return Err(ConfigError::new(
            format!("workers.{profile}"),
            "automatic profile names must carry a queue after __auto__.",
        ));
    }
    if self.brokers.len() != 1 {
        return Err(ConfigError::new(
            format!("workers.{profile}.subscriptions.{SUBSCRIPTION_NAME}.broker"),
            "automatic profiles require a single configured broker; \
             declare this profile under workers.*",
        ));
    }
    let worker = WorkerProfile {
        name: profile.to_owned(),
        subscriptions: vec![SubscriptionConfig {
            name: SUBSCRIPTION_NAME.to_owned(),
            broker: self.brokers[0].name.clone(),
            queue: queue.to_owned(),
            weight: 1,
            priority_class: 0,
            prefetch: PrefetchConfig::Fixed(64),
            starvation_after: Duration::from_secs(30),
            max_buffered_bytes: default_max_buffered_bytes(),
            early_ack: false,
            no_ack: false,
        }],
        scheduler: SchedulerConfig::weighted_fair(),
    };
    let broker_names: HashSet<&str> =
        self.brokers.iter().map(|b| b.name.as_str()).collect();
    validate_worker(&worker, &broker_names)?;
    Ok(worker)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `rtk cargo test -p rabbit-rs-core config::tests`
Expected: PASS (all, including pre-existing).

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core/src/config.rs
git commit -m "feat(core): synthesize default profiles for __auto__ names

Runtime queue resolution starts here: ValidatedConfig builds the default
one-subscription profile for __auto__.{queue} names with compiler-level
defaults and validates it like any configured profile.

Refs #167"
```

---

### Task 2: `TopologyPlan::from_config_and` (topology plan)

**Files:**
- Modify: `crates/rabbit-rs-core/src/topology/plan.rs` (`from_config` at :268)
- Test: `crates/rabbit-rs-core/src/topology/plan.rs` (existing `mod tests` if present, else add at bottom; mirror existing test style)

**Interfaces:**
- Produces: `pub fn from_config_and(config: &ValidatedConfig, extra: &[WorkerProfile]) -> Self`; `from_config(config)` remains and behaves identically to today. Task 3 consumes `from_config_and`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn from_config_and_appends_runtime_queues() {
    let config = declare_mode_config_with_worker("orders"); // helper: 1 broker, 1 worker on queue "orders"
    let extra = auto_worker("__auto__.emails", "emails");   // helper building WorkerProfile { subscriptions: [queue "emails"] }

    let plan = TopologyPlan::from_config_and(&config, std::slice::from_ref(&extra));

    let queue_names: Vec<&str> = plan.queues().iter().map(|queue| queue.name.as_str()).collect();
    assert!(queue_names.contains(&"orders"));
    assert!(queue_names.contains(&"emails"));

    let without_extra = TopologyPlan::from_config(&config);
    let base_names: Vec<&str> = without_extra.queues().iter().map(|queue| queue.name.as_str()).collect();
    assert!(!base_names.contains(&"emails"));
}
```

Build the helpers with the same struct-literal style `tests/consumer.rs:37-54` uses (`SubscriptionConfig` needs `name`, `broker`, `queue`, `weight: 1`, `priority_class: 0`, `prefetch: PrefetchConfig::Fixed(8)`, `starvation_after: Duration::from_secs(30)`, `max_buffered_bytes: 64 * 1024 * 1024`, `early_ack: false`, `no_ack: false`, `scheduler: SchedulerConfig::weighted_fair()`). Place the test in `plan.rs`'s test module; import `WorkerProfile`, `SchedulerConfig`, `PrefetchConfig` there.

- [ ] **Step 2: Run to verify failure**

Run: `rtk cargo test -p rabbit-rs-core topology::plan`
Expected: FAIL — `from_config_and` not defined.

- [ ] **Step 3: Implement** — refactor `from_config` to route through a sources-based private builder, then add the public variant:

```rust
#[must_use]
pub fn from_config(config: &ValidatedConfig) -> Self {
    Self::from_profiles(config, config.worker_profiles().iter())
}

/// Builds the plan from the configured profiles plus runtime-requested
/// extras whose names are not in config (the auto_subscribe path).
///
/// # Panics
///
/// Never panics: the external-mode fallback compiles by construction.
#[must_use]
pub fn from_config_and(config: &ValidatedConfig, extra: &[WorkerProfile]) -> Self {
    Self::from_profiles(config, config.worker_profiles().iter().chain(extra))
}

fn from_profiles<'a>(
    config: &ValidatedConfig,
    profiles: impl Iterator<Item = &'a WorkerProfile>,
) -> Self {
    // The existing body of from_config, with the `subscriptions` binding
    // replaced by:
    let subscriptions: Vec<_> = profiles.flat_map(|worker| &worker.subscriptions).collect();
    // ...everything from `let queues = ...` down stays exactly as it is today.
}
```

Keep the existing `from_config` doc comment and the `#[must_use]` attributes; document `from_config_and` with a `///` note that extras must already be name-filtered by the caller (the coordinator filters, see Task 3).

- [ ] **Step 4: Run to verify pass**

Run: `rtk cargo test -p rabbit-rs-core topology::plan`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core/src/topology/plan.rs
git commit -m "feat(topology): extend plan compilation with runtime profiles

from_config_and appends requested runtime profiles to the configured
ones so declare-on-use can reconcile queues that no configured profile
covers.

Refs #167"
```

---

### Task 3: Core wiring — payload map, synthesis gate, plan at reconcile

**Files:**
- Modify: `crates/rabbit-rs-core/src/client.rs` (field :65, init :111, `consumer()` :326-344, `coordinator()` :738-756)
- Modify: `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs` (alias :33, config struct :117-134, context build :162-170, context struct :194-206, `recover_generation` :507-591, reconcile sites :532 and :703, `establish_requested_profile` :605-721, helpers :723-747)
- Modify: `crates/rabbit-rs-core/src/transport/mock.rs` (declare recorder)
- Test: `crates/rabbit-rs-core/tests/auto_profiles.rs` (create, mirroring the harness style of `tests/consumer.rs` and the recovery-scripting patterns from `tests/recovery.rs`)

**Interfaces:**
- Consumes: `ValidatedConfig::synthesize_auto_profile` (Task 1), `AUTO_PROFILE_PREFIX` (Task 1), `TopologyPlan::from_config_and` (Task 2).
- Produces: `type RequestedProfiles = Arc<StdMutex<BTreeMap<String, WorkerProfile>>>;` (recovery_coordinator.rs:33, re-exported or path-imported by client.rs). `ClientPool::consumer()` resolves `__auto__.` names. `MockTransport::declared_queues() -> Vec<String>`.

- [ ] **Step 1: Write the failing cross-module tests** — create `crates/rabbit-rs-core/tests/auto_profiles.rs`:

```rust
use std::{sync::Arc, time::Duration};

use rabbit_rs_core::{
    client::{ClientErrorKind, ClientPool},
    config::{Config, TopologyMode},
    transport::mock::MockTransport,
};

mod common;

fn single_worker_config(topology: TopologyMode, worker: WorkerProfile) -> Arc<ValidatedConfig> {
    Arc::new(
        Config {
            brokers: vec![common_broker("main")],
            workers: vec![worker],
            topology_mode: topology,
            ..empty_sections() // same Config literal pattern as tests/consumer.rs:57-70
        }
        .validate()
        .expect("valid config"),
    )
}

#[tokio::test(start_paused = true)]
async fn synthesizes_and_declares_auto_queue() {
    let transport = Arc::new(MockTransport::default());
    let config = single_worker_config(TopologyMode::Declare, worker_on("orders"));
    let pool = ClientPool::new(config, transport.clone());

    let consumer = pool.consumer("__auto__.emails").await.expect("auto profile resolves");

    assert!(!transport.declared_queues().is_empty());
    assert!(transport
        .declared_queues()
        .iter()
        .any(|queue| queue == "emails"));
    // The consumer is usable: a scripted delivery surfaces (script the
    // mock to deliver one message on queue "emails", then assert next()
    // returns it — mirror the delivery-scripting calls from
    // tests/consumer.rs, e.g. transport.push_delivery(...) style helpers
    // used there).
    drop(consumer);
}

#[tokio::test(start_paused = true)]
async fn plain_unknown_names_still_error() {
    let transport = Arc::new(MockTransport::default());
    let config = single_worker_config(TopologyMode::Declare, worker_on("orders"));
    let pool = ClientPool::new(config, transport);

    let error = pool.consumer("orders-typo").await.expect_err("unknown profile");

    assert!(matches!(error.kind(), ClientErrorKind::Configuration));
    assert!(error.to_string().contains("unknown worker profile"));
}

#[tokio::test(start_paused = true)]
async fn synthesis_bound_is_enforced() {
    let transport = Arc::new(MockTransport::default());
    let config = single_worker_config(TopologyMode::Declare, worker_on("orders"));
    let pool = ClientPool::new(config, transport);

    // 64 distinct auto names succeed; the 65th is rejected before any
    // broker contact. Pop-order errors surface on consumer().
    for index in 0..64 {
        let name = format!("__auto__.queue-{index}");
        // Establish and drop each consumer; the mock accepts everything.
        drop(pool.consumer(&name).await.expect("within bound"));
    }
    let error = pool.consumer("__auto__.queue-64").await.expect_err("over bound");
    assert!(error.to_string().contains("profile registry is full"));
}
```

After the first RED run, add the two recovery-focused tests, reusing the disconnect-scripting helpers from `tests/recovery.rs` (copy the scenario setup — mock connection-drop scripting + paused-time advancement — from the closest existing test there):

```rust
#[tokio::test(start_paused = true)]
async fn recovery_re_establishes_synthesized_consumer() {
    // Setup like synthesizes_and_declares_auto_queue, then script a
    // connection drop + reconnect as tests/recovery.rs does, advance
    // paused time until the coordinator completes the recovery
    // generation, and assert the consumer is still usable and the queue
    // was declared again (declared_queues contains "emails" at least
    // twice).
}

#[tokio::test(start_paused = true)]
async fn auto_profile_declared_when_popped_after_publisher_use() {
    // 1. Publish one message through the pool (publisher-only) so the
    //    coordinator for "main" spawns with a plan that has no runtime
    //    queues.
    // 2. pool.consumer("__auto__.emails") — the establish path must
    //    reconcile a FRESH plan (from_config_and) and declare "emails".
    // 3. Assert transport.declared_queues() contains "emails".
}
```

- [ ] **Step 2: Run to verify failure**

Run: `rtk cargo test -p rabbit-rs-core --test auto_profiles`
Expected: FAIL — `declared_queues` missing on the mock and `consumer("__auto__.emails")` rejected as unknown profile.

- [ ] **Step 3: Add the mock declare recorder** — in `transport/mock.rs`:

Add to the shared mock state (the struct holding `declare_queue_gates`, :63-66):

```rust
declared_queues: StdMutex<Vec<String>>,
```

(initialize it wherever the other state fields are constructed) and on `MockTransport` (next to `push_declare_queue_gate`, :151-157):

```rust
/// Queue names passed to `queue_declare` on any channel, in call order.
pub fn declared_queues(&self) -> Vec<String> {
    self.state()
        .declared_queues
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}
```

Inside the `mock_declare` macro's `declare_queue` arm (:426), record before the gate logic:

```rust
self.state()
    .declared_queues
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .push(spec.name.clone());
```

- [ ] **Step 4: Make the consumer() gate synthesize** — in `client.rs`:

Change the field (:65) and its doc comment:

```rust
/// Worker profiles explicitly requested through [`ClientPool::consumer`],
/// carrying their payload: entries enter only through `consumer()`
/// resolution, so requested and established stay the same set. Shared with
/// every coordinator so recovery only establishes requested consumers;
/// declared-but-unrequested profiles stay dormant.
requested_profiles: Arc<StdMutex<BTreeMap<String, WorkerProfile>>>,
```

Update the initializer (:111) to `Arc::new(StdMutex::new(BTreeMap::new()))` and import `BTreeMap`. Raise the constant near the field:

```rust
/// Upper bound on runtime-synthesized profiles per process (the
/// `__auto__.` auto_subscribe path). Config profiles are not counted.
const MAX_SYNTHESIZED_PROFILES: usize = 64;
```

Replace the resolution + recording block in `consumer()` (:327-338):

```rust
let generation = self.open_generation()?;
let worker = match self.config.worker(profile) {
    Some(worker) => worker.clone(),
    None if profile.starts_with(crate::config::AUTO_PROFILE_PREFIX) => {
        self.synthesized_worker(profile)?
    }
    None => {
        return Err(ClientError::new(
            ClientErrorKind::Configuration,
            format!("workers.{profile}: unknown worker profile"),
        ));
    }
};

// Record the request before any coordinator is triggered so that the
// current or next recovery generation establishes this profile's
// consumer channels (see `recover_generation`).
lock(&self.requested_profiles)
    .entry(profile.to_owned())
    .or_insert_with(|| worker.clone());
```

and add the private helper next to `consumer()`:

```rust
/// Synthesizes and records the payload for an `__auto__.` profile,
/// enforcing the process-wide bound on synthesized profiles.
fn synthesized_worker(&self, profile: &str) -> Result<WorkerProfile, ClientError> {
    let worker = self.config.synthesize_auto_profile(profile).map_err(|error| {
        ClientError::new(ClientErrorKind::Configuration, error.to_string())
    })?;
    let mut requested = lock(&self.requested_profiles);
    let synthesized = requested
        .values()
        .filter(|worker| self.config.worker(&worker.name).is_none())
        .count();
    if !requested.contains_key(profile) && synthesized >= MAX_SYNTHESIZED_PROFILES {
        return Err(ClientError::new(
            ClientErrorKind::Configuration,
            format!(
                "workers.{profile}: profile registry is full \
                 ({MAX_SYNTHESIZED_PROFILES} synthesized profiles)"
            ),
        ));
    }
    requested
        .entry(profile.to_owned())
        .or_insert_with(|| worker.clone());
    Ok(worker)
}
```

Delete the now-dead plan computation in `coordinator()` (:742 `let topology_plan = TopologyPlan::from_config(&self.config);`) and the `topology_plan` field from the `RecoveryCoordinatorConfig` literal; remove the `TopologyPlan` import if it becomes unused (`rtk cargo clippy` will name it).

- [ ] **Step 5: Make the coordinator consume the payload map** — in `recovery_coordinator.rs`:

Alias (:33):

```rust
type RequestedProfiles = Arc<StdMutex<BTreeMap<String, WorkerProfile>>>;
```

Remove `pub topology_plan: TopologyPlan` from `RecoveryCoordinatorConfig` (:122-123) and from `CoordinatorContext` (:196) plus its initializer (:164); add `use std::collections::BTreeMap;` and the `WorkerProfile` import.

Replace `recover_generation`'s consumer loop (:573-588):

```rust
let requested = requested_snapshot(context);
for worker in context.config.worker_profiles() {
    if !requested.contains(&worker.name) {
        continue;
    }
    establish_requested_profile(
        actor,
        context,
        publisher,
        consumers,
        establish_lock,
        &worker.name,
        generation,
    )
    .await?;
}
// Runtime-synthesized profiles, name-sorted for determinism.
for name in requested_extras(context) {
    establish_requested_profile(
        actor,
        context,
        publisher,
        consumers,
        establish_lock,
        &name,
        generation,
    )
    .await?;
}
```

In `establish_requested_profile`, replace the payload lookup (:627-629):

```rust
let worker = match context.config.worker(profile) {
    Some(worker) => worker.clone(),
    None => match lock_requested(context).get(profile).cloned() {
        Some(worker) => worker,
        None => return Ok(()),
    },
};
```

Replace both reconcile sites to compute the plan fresh — a context helper next to `is_requested`:

```rust
/// The plan for this generation: configured profiles plus requested
/// runtime extras (names not in config). Computed per reconcile so a
/// profile requested after the coordinator spawned still gets its queue
/// declared (declare-on-use).
fn generation_plan(context: &CoordinatorContext) -> TopologyPlan {
    let extras: Vec<WorkerProfile> = lock_requested(context)
        .values()
        .filter(|worker| context.config.worker(&worker.name).is_none())
        .cloned()
        .collect();
    TopologyPlan::from_config_and(&context.config, &extras)
}
```

At :526-536 and :697-708, call `.reconcile(..., &generation_plan(context), generation)` (compute the plan once into a local before the await to avoid holding the map lock across it). Update `is_requested` (:723-729) and `requested_snapshot` (:731+) to work on the map — `contains_key` / keys collected into a `HashSet<String>` — and add:

```rust
fn lock_requested(
    context: &CoordinatorContext,
) -> std::sync::MutexGuard<'_, BTreeMap<String, WorkerProfile>> {
    context
        .requested_profiles
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Requested runtime profiles that are not in config, name-sorted.
fn requested_extras(context: &CoordinatorContext) -> Vec<String> {
    lock_requested(context)
        .keys()
        .filter(|name| context.config.worker(name).is_none())
        .cloned()
        .collect()
}
```

- [ ] **Step 6: Format, lint, and run the new tests**

```bash
rtk cargo fmt --all
rtk cargo test -p rabbit-rs-core --test auto_profiles
```

Expected: PASS. If the recovery-scripting helpers need adjusting to the exact mock scenario API, mirror `tests/recovery.rs` — the assertions above are the contract.

- [ ] **Step 7: Run the full core suite for regressions**

Run: `rtk cargo test -p rabbit-rs-core`
Expected: PASS — especially existing consumer/recovery/topology suites.

- [ ] **Step 8: Commit**

```bash
git add crates/rabbit-rs-core/src/client.rs crates/rabbit-rs-core/src/pool/recovery_coordinator.rs crates/rabbit-rs-core/src/transport/mock.rs crates/rabbit-rs-core/tests/auto_profiles.rs
git commit -m "feat(core): resolve auto profiles at pop time with fresh plans

requested_profiles becomes a payload-carrying map fed only by
consumer() resolution, __auto__.-prefixed unknown names synthesize
their default profile (bounded to 64), and the frozen per-coordinator
plan is replaced by generation_plan() at both reconcile sites — the
driver's declare-on-use. Registration and consumption stay the same
act, preserving #49 dormant semantics.

Refs #167"
```

---

### Task 4: Laravel integration test (real extension, real broker)

**Files:**
- Test: `packages/laravel-queue/tests/Integration/AutoSubscribeTest.php` (create; mirror the harness of `tests/Integration/PoisonDeliveryTest.php` — connection setup, unique queue names, Pest `it()` style)

**Interfaces:**
- Consumes: the extension built by `scripts/test-integration.sh` (ext-rabbit_rs loaded from `target/debug/`), a `__auto__.{queue}` profile name produced by `WorkerProfileResolver::registerAutoProfile`.
- Produces: end-to-end proof that an undeclared queue pops, acks, and its queue was declared.

- [ ] **Step 1: Write the failing integration test** — create a connection config with `auto_subscribe => true` and a queue name that appears in no `workers.*` entry (use a unique name like `'auto-it-'.uniqid()` to dodge broker state), push one job through the native publisher (`Queue::push` or the pool's publish API as the other Integration tests do), then `pop` it by queue name and assert it marshals into `RabbitMqJob` and acks. Assert the resolver-backed queue mapping works by checking `$job->getQueue()` equals the pushed queue. Follow `tests/Integration/PoisonDeliveryTest.php` for connection bootstrap and teardown (delete queues afterwards where the harness does).

- [ ] **Step 2: Run to verify failure**

Run: `./scripts/test-integration.sh` (requires the RabbitMQ lab running, same as CI)
Expected: FAIL — the pop path cannot resolve the undeclared queue (unknown worker profile).

- [ ] **Step 3: Verify it passes after Task 3's core is built into the extension artifact**

Run: `./scripts/test-integration.sh`
Expected: PASS. If the extension artifact is stale, rebuild it first (`ext_ensure_built` in `scripts/lib-extension.sh` handles debug builds).

- [ ] **Step 4: Commit**

```bash
git add packages/laravel-queue/tests/Integration/AutoSubscribeTest.php
git commit -m "test(laravel): cover end-to-end auto_subscribe on an undeclared queue

pop() on a queue no workers.* entry covers must synthesize, declare,
and consume through the native pool.

Refs #167"
```

---

### Task 5: Docs, changelog, full gate

**Files:**
- Modify: `packages/laravel-queue/docs/laravel.md` (auto_subscribe section)
- Modify: `README.md`
- Modify: `CHANGELOG.md` (Unreleased)
- Modify: `packages/laravel-queue/tests/Feature/AutoSubscribeTest.php` (comment at :31-33 only — the seeded fake still mirrors reality; update the wording to "the core synthesizes `__auto__.` profiles at first pop")

- [ ] **Step 1: Update `laravel.md`** — rewrite the `auto_subscribe` section to state: unknown queues pop with zero config; the core synthesizes a default profile (one subscription, weight 1, fixed prefetch 64, acks on) named `__auto__.{queue}`; tune per-queue via `workers.*`; caveats — `topology_mode: external` never declares the auto queue (broker 404, like `declare => false` in other drivers), and multiple brokers per config require explicit `workers.*` entries.
- [ ] **Step 2: README one-liner** — in the Laravel section, add: unknown queues are consumed automatically (`auto_subscribe`), no `workers.*` entry required.
- [ ] **Step 3: CHANGELOG (Unreleased)** — add: "Core synthesizes default worker profiles for `__auto__.{queue}` names at first pop; the auto-subscribe path no longer depends on config mutation and works after pool creation."
- [ ] **Step 4: Run the full quality gate**

```bash
rtk ./scripts/check.sh
```

Expected: fmt clean, clippy `-D warnings` clean, nextest green, `composer validate --strict` OK.

- [ ] **Step 5: Run the Laravel suites**

```bash
./scripts/test-laravel.sh
```

Expected: Unit + Feature green without the extension, no notices (the Feature AutoSubscribeTest asserts stay valid — the seeded-pool mirror still models core-side synthesis).

- [ ] **Step 6: Commit**

```bash
git add packages/laravel-queue/docs/laravel.md README.md CHANGELOG.md packages/laravel-queue/tests/Feature/AutoSubscribeTest.php
git commit -m "docs: document core-side auto profile synthesis

Zero-config pop, __auto__. contract, external-mode and multi-broker
caveats.

Refs #167"
```

---

## Verification (after Task 5)

1. `rtk ./scripts/check.sh` — full gate green.
2. `./scripts/test-integration.sh` — integration green (when the RabbitMQ lab is up).
3. Focused: `rtk cargo test -p rabbit-rs-core --test auto_profiles` and `rtk cargo test -p rabbit-rs-core config::tests`.
4. Spec coverage walk: every Goal in the spec maps to a test above (synthesis ✓ Task 1, plan-at-reconcile ✓ Tasks 2-3, pop parity ✓ Task 4, bounds ✓ Task 3, #49 dormancy ✓ Task 3 recovery test).
