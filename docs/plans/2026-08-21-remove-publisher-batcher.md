# Remove Publisher Batcher Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the actor-internal batcher from the publisher, simplifying the publish path to immediate per-message publishing while keeping all guarantees (confirms, mandatory, replay, backpressure, publishBatch FFI).

**Architecture:** Delete `batcher.rs`, remove `max_messages`/`max_bytes`/`flush_interval` from `PublisherConfig`, simplify `accept_publish` to publish immediately in Ready phase, remove `flush_batch`/`flush_interval`/`flush_deadline` from the actor state machine. Update all 22 call sites across 11 files. No changes to the Laravel package.

**Tech Stack:** Rust (edition 2024, toolchain 1.96.0), Tokio, ext-php-rs, Laravel/PHP

## Global Constraints

- Unsafe Rust is forbidden (`#![forbid(unsafe_code)]`).
- Keep Lapin behind the `Transport` abstraction.
- Never expose credentials through Debug, errors, metrics, or logs.
- Run `rtk cargo fmt --all` after Rust edits.
- Run focused tests during iteration, then the full quality gate before completion.
- Preserve unrelated work in a dirty tree.
- Keep commits logical and scoped.

---

### Task 1: Remove `batcher.rs` and update `PublisherConfig`

**Files:**
- Delete: `crates/rabbit-rs-core/src/publisher/batcher.rs`
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs:2` (remove `pub mod batcher;`)
- Modify: `crates/rabbit-rs-core/src/publisher/mod.rs:103-154` (remove 3 fields + update constructors)

**Interfaces:**
- Produces: `PublisherConfig::new(buffer_capacity: usize, confirm_timeout: Duration) -> Self`
- Produces: `PublisherConfig::with_flags(buffer_capacity: usize, confirm_timeout: Duration, confirms: bool, mandatory: bool) -> Self`

- [ ] **Step 1: Delete `batcher.rs`**

Delete the file `crates/rabbit-rs-core/src/publisher/batcher.rs`.

- [ ] **Step 2: Remove `pub mod batcher;` from `mod.rs`**

In `crates/rabbit-rs-core/src/publisher/mod.rs`, remove line 2:

```rust
pub mod batcher;
```

- [ ] **Step 3: Remove batch fields from `PublisherConfig` struct**

In `crates/rabbit-rs-core/src/publisher/mod.rs`, replace the `PublisherConfig` struct (lines 103-112):

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublisherConfig {
    pub max_messages: usize,
    pub max_bytes: usize,
    pub flush_interval: Duration,
    pub buffer_capacity: usize,
    pub confirm_timeout: Duration,
    pub confirms: bool,
    pub mandatory: bool,
}
```

with:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublisherConfig {
    pub buffer_capacity: usize,
    pub confirm_timeout: Duration,
    pub confirms: bool,
    pub mandatory: bool,
}
```

- [ ] **Step 4: Update `PublisherConfig::new` constructor**

Replace the `new` method (lines 114-133):

```rust
impl PublisherConfig {
    #[must_use]
    pub const fn new(
        max_messages: usize,
        max_bytes: usize,
        flush_interval: Duration,
        buffer_capacity: usize,
        confirm_timeout: Duration,
    ) -> Self {
        Self {
            max_messages,
            max_bytes,
            flush_interval,
            buffer_capacity,
            confirm_timeout,
            confirms: true,
            mandatory: true,
        }
    }
```

with:

```rust
impl PublisherConfig {
    #[must_use]
    pub const fn new(buffer_capacity: usize, confirm_timeout: Duration) -> Self {
        Self {
            buffer_capacity,
            confirm_timeout,
            confirms: true,
            mandatory: true,
        }
    }
```

- [ ] **Step 5: Update `PublisherConfig::with_flags` constructor**

Replace the `with_flags` method (lines 134-154):

```rust
    #[must_use]
    pub const fn with_flags(
        max_messages: usize,
        max_bytes: usize,
        flush_interval: Duration,
        buffer_capacity: usize,
        confirm_timeout: Duration,
        confirms: bool,
        mandatory: bool,
    ) -> Self {
        Self {
            max_messages,
            max_bytes,
            flush_interval,
            buffer_capacity,
            confirm_timeout,
            confirms,
            mandatory,
        }
    }
}
```

with:

```rust
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
        }
    }
}
```

- [ ] **Step 6: Run `cargo fmt`**

Run: `rtk cargo fmt --all`
Expected: no formatting errors

- [ ] **Step 7: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/batcher.rs crates/rabbit-rs-core/src/publisher/mod.rs
git commit -m "refactor: remove batcher.rs and simplify PublisherConfig"
```

---

### Task 2: Simplify the publisher actor

**Files:**
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:25` (remove `batcher::Batcher` import)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:256-271` (ActorState fields)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:280-296` (ActorState::new)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:298-316` (remove flush_interval, simplify next_deadline)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:318-329` (suspend)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:331-343` (fail_all)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:413-421` (run_actor select! Ready branch)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:432-455` (accept_publish Ready branch)
- Modify: `crates/rabbit-rs-core/src/publisher/actor.rs:508-511` (delete flush_batch)

**Interfaces:**
- Consumes: `PublisherConfig` without `max_messages`/`max_bytes`/`flush_interval` from Task 1
- Produces: simplified `ActorState` without `batch` and `flush_deadline`

- [ ] **Step 1: Remove `Batcher` import**

In `crates/rabbit-rs-core/src/publisher/actor.rs`, the import is on line 25 inside the `super` block:

```rust
use super::{
    PublishError, PublishErrorKind, PublishOutcome, PublishRequest, PublishWaiter, PublisherConfig,
    PublisherConnectionEvent, ReturnInfo, batcher::Batcher, confirms::ConfirmLedger,
    delay::DelayRouter,
};
```

Replace with (remove `batcher::Batcher,`):

```rust
use super::{
    PublishError, PublishErrorKind, PublishOutcome, PublishRequest, PublishWaiter, PublisherConfig,
    PublisherConnectionEvent, ReturnInfo, confirms::ConfirmLedger, delay::DelayRouter,
};
```

- [ ] **Step 2: Remove `batch` and `flush_deadline` from `ActorState`**

Replace the `ActorState` struct (lines 256-271):

```rust
struct ActorState {
    config: PublisherConfig,
    phase: Phase,
    generation: u64,
    channel: Option<Arc<dyn PublisherChannel>>,
    batch: Batcher<RetainedPublish>,
    replay: VecDeque<RetainedPublish>,
    ledger: ConfirmLedger<InFlightPublish>,
    confirmations: FuturesUnordered<ConfirmationFuture>,
    sequence: u64,
    flush_deadline: Option<time::Instant>,
    permanent_error: Option<PublishError>,
    metrics: Metrics,
    delay_strategy: Option<DelayStrategy>,
    declared_ttl_queues: HashSet<String>,
}
```

with:

```rust
struct ActorState {
    config: PublisherConfig,
    phase: Phase,
    generation: u64,
    channel: Option<Arc<dyn PublisherChannel>>,
    replay: VecDeque<RetainedPublish>,
    ledger: ConfirmLedger<InFlightPublish>,
    confirmations: FuturesUnordered<ConfirmationFuture>,
    sequence: u64,
    permanent_error: Option<PublishError>,
    metrics: Metrics,
    delay_strategy: Option<DelayStrategy>,
    declared_ttl_queues: HashSet<String>,
}
```

- [ ] **Step 3: Update `ActorState::new`**

Replace the `new` method (lines 274-296):

```rust
    fn new(
        channel: Arc<dyn PublisherChannel>,
        config: PublisherConfig,
        metrics: Metrics,
        delay_strategy: Option<DelayStrategy>,
    ) -> Self {
        Self {
            config,
            phase: Phase::Ready,
            generation: 1,
            channel: Some(channel),
            batch: Batcher::new(config.max_messages, config.max_bytes),
            replay: VecDeque::new(),
            ledger: ConfirmLedger::default(),
            confirmations: FuturesUnordered::new(),
            sequence: 0,
            flush_deadline: None,
            permanent_error: None,
            metrics,
            delay_strategy,
            declared_ttl_queues: HashSet::new(),
        }
    }
```

with:

```rust
    fn new(
        channel: Arc<dyn PublisherChannel>,
        config: PublisherConfig,
        metrics: Metrics,
        delay_strategy: Option<DelayStrategy>,
    ) -> Self {
        Self {
            config,
            phase: Phase::Ready,
            generation: 1,
            channel: Some(channel),
            replay: VecDeque::new(),
            ledger: ConfirmLedger::default(),
            confirmations: FuturesUnordered::new(),
            sequence: 0,
            permanent_error: None,
            metrics,
            delay_strategy,
            declared_ttl_queues: HashSet::new(),
        }
    }
```

- [ ] **Step 4: Remove `flush_interval` and simplify `next_deadline`**

Replace the `flush_interval` and `next_deadline` methods (lines 298-316):

```rust
    fn flush_interval(&self) -> Duration {
        if self.config.flush_interval.is_zero() {
            Duration::from_nanos(1)
        } else {
            self.config.flush_interval
        }
    }

    fn next_deadline(&self) -> Option<time::Instant> {
        match self.phase {
            Phase::Ready => self.flush_deadline,
            Phase::Suspended => self
                .replay
                .iter()
                .map(|pending| pending.request.deadline)
                .min(),
            Phase::FailedPermanent => None,
        }
    }
```

with:

```rust
    fn next_deadline(&self) -> Option<time::Instant> {
        match self.phase {
            Phase::Ready => None,
            Phase::Suspended => self
                .replay
                .iter()
                .map(|pending| pending.request.deadline)
                .min(),
            Phase::FailedPermanent => None,
        }
    }
```

- [ ] **Step 5: Update `suspend` to remove batch drain**

Replace the `suspend` method (lines 318-329):

```rust
    fn suspend(&mut self, generation: u64) {
        if generation > 0 {
            self.generation = self.generation.max(generation);
        }
        self.phase = Phase::Suspended;
        self.channel = None;
        self.flush_deadline = None;
        self.replay.extend(self.batch.take());
        self.replay
            .extend(self.ledger.drain().map(|in_flight| in_flight.retained));
        self.confirmations = FuturesUnordered::new();
    }
```

with:

```rust
    fn suspend(&mut self, generation: u64) {
        if generation > 0 {
            self.generation = self.generation.max(generation);
        }
        self.phase = Phase::Suspended;
        self.channel = None;
        self.replay
            .extend(self.ledger.drain().map(|in_flight| in_flight.retained));
        self.confirmations = FuturesUnordered::new();
    }
```

- [ ] **Step 6: Update `fail_all` to remove batch drain**

Replace the `fail_all` method (lines 331-343):

```rust
    fn fail_all(&mut self, error: &PublishError) {
        for retained in self.batch.take() {
            complete_error(retained, error.clone());
        }
        for retained in self.replay.drain(..) {
            complete_error(retained, error.clone());
        }
        for in_flight in self.ledger.drain() {
            complete_error(in_flight.retained, error.clone());
        }
        self.confirmations = FuturesUnordered::new();
        self.flush_deadline = None;
    }
```

with:

```rust
    fn fail_all(&mut self, error: &PublishError) {
        for retained in self.replay.drain(..) {
            complete_error(retained, error.clone());
        }
        for in_flight in self.ledger.drain() {
            complete_error(in_flight.retained, error.clone());
        }
        self.confirmations = FuturesUnordered::new();
    }
```

- [ ] **Step 7: Update `run_actor` select! `wait_for_deadline` Ready branch**

Replace the `wait_for_deadline` match in the select! (lines 413-421):

```rust
            () = wait_for_deadline(state.next_deadline()) => {
                match state.phase {
                    Phase::Ready => {
                        flush_batch(&mut state).await;
                        state.flush_deadline = None;
                    }
                    Phase::Suspended => state.expire_replay(),
                    Phase::FailedPermanent => {}
                }
            }
```

with:

```rust
            () = wait_for_deadline(state.next_deadline()) => {
                match state.phase {
                    Phase::Ready => {}
                    Phase::Suspended => state.expire_replay(),
                    Phase::FailedPermanent => {}
                }
            }
```

- [ ] **Step 8: Update `accept_publish` Ready branch**

Replace the `accept_publish` function (lines 432-455):

```rust
async fn accept_publish(state: &mut ActorState, retained: RetainedPublish) {
    match state.phase {
        Phase::Ready => {
            let payload_len = retained.request.payload.len();
            if state.batch.is_empty() {
                state.flush_deadline = Some(time::Instant::now() + state.flush_interval());
            }
            if state.batch.push(retained, payload_len) {
                flush_batch(state).await;
                state.flush_deadline = None;
            }
        }
        Phase::Suspended => state.replay.push_back(retained),
        Phase::FailedPermanent => complete_error(
            retained,
            state.permanent_error.clone().unwrap_or_else(|| {
                PublishError::new(
                    PublishErrorKind::Transport,
                    "publisher connection failed permanently",
                )
            }),
        ),
    }
}
```

with:

```rust
async fn accept_publish(state: &mut ActorState, retained: RetainedPublish) {
    match state.phase {
        Phase::Ready => {
            let pending = VecDeque::from([retained]);
            publish_queue(state, pending).await;
        }
        Phase::Suspended => state.replay.push_back(retained),
        Phase::FailedPermanent => complete_error(
            retained,
            state.permanent_error.clone().unwrap_or_else(|| {
                PublishError::new(
                    PublishErrorKind::Transport,
                    "publisher connection failed permanently",
                )
            }),
        ),
    }
}
```

- [ ] **Step 9: Delete `flush_batch` function**

Delete the `flush_batch` function (lines 508-511):

```rust
async fn flush_batch(state: &mut ActorState) {
    let pending = VecDeque::from(state.batch.take());
    publish_queue(state, pending).await;
}
```

The `flush_replay` function immediately follows and remains unchanged.

- [ ] **Step 10: Run `cargo fmt`**

Run: `rtk cargo fmt --all`
Expected: no formatting errors

- [ ] **Step 11: Commit**

```bash
git add crates/rabbit-rs-core/src/publisher/actor.rs
git commit -m "refactor: simplify publisher actor — publish immediately in Ready phase"
```

---

### Task 3: Update `client.rs` production code

**Files:**
- Modify: `crates/rabbit-rs-core/src/client.rs:25-27` (remove constants)
- Modify: `crates/rabbit-rs-core/src/client.rs:619-630` (update `publisher_config()`)

- [ ] **Step 1: Remove `DEFAULT_MAX_MESSAGES` and `DEFAULT_MAX_BYTES` constants**

In `crates/rabbit-rs-core/src/client.rs`, remove lines 25-26:

```rust
const DEFAULT_MAX_MESSAGES: usize = 256;
const DEFAULT_MAX_BYTES: usize = 2 * 1024 * 1024;
```

Keep `DEFAULT_BUFFER_CAPACITY` on line 27.

- [ ] **Step 2: Update `publisher_config()` function**

Replace the `publisher_config` function (lines 619-630):

```rust
fn publisher_config(config: &ValidatedConfig) -> PublisherConfig {
    let publisher = config.publisher();
    PublisherConfig::with_flags(
        DEFAULT_MAX_MESSAGES,
        DEFAULT_MAX_BYTES,
        Duration::from_millis(1),
        DEFAULT_BUFFER_CAPACITY,
        publisher.confirm_timeout,
        publisher.confirms,
        publisher.mandatory,
    )
}
```

with:

```rust
fn publisher_config(config: &ValidatedConfig) -> PublisherConfig {
    let publisher = config.publisher();
    PublisherConfig::with_flags(
        DEFAULT_BUFFER_CAPACITY,
        publisher.confirm_timeout,
        publisher.confirms,
        publisher.mandatory,
    )
}
```

- [ ] **Step 3: Run `cargo fmt`**

Run: `rtk cargo fmt --all`
Expected: no formatting errors

- [ ] **Step 4: Commit**

```bash
git add crates/rabbit-rs-core/src/client.rs
git commit -m "refactor: update client.rs publisher_config for simplified PublisherConfig"
```

---

### Task 4: Update PHP extension testing harness

**Files:**
- Modify: `crates/rabbit-rs-php/src/testing.rs:105-111`

- [ ] **Step 1: Update `PublisherConfig::new` call**

In `crates/rabbit-rs-php/src/testing.rs`, replace lines 105-111:

```rust
    let publisher_config = PublisherConfig::new(
        1,
        1024 * 1024,
        Duration::from_millis(1),
        scenario.publisher_capacity,
        Duration::from_secs(30),
    );
```

with:

```rust
    let publisher_config = PublisherConfig::new(
        scenario.publisher_capacity,
        Duration::from_secs(30),
    );
```

- [ ] **Step 2: Run `cargo fmt`**

Run: `rtk cargo fmt --all`
Expected: no formatting errors

- [ ] **Step 3: Commit**

```bash
git add crates/rabbit-rs-php/src/testing.rs
git commit -m "refactor: update testing.rs PublisherConfig call for new signature"
```

---

### Task 5: Update `publisher_safety.rs` tests

**Files:**
- Modify: `crates/rabbit-rs-core/tests/publisher_safety.rs:28-36` (helper `config`)
- Delete: `crates/rabbit-rs-core/tests/publisher_safety.rs:76-108` (two batcher tests)
- Modify: `crates/rabbit-rs-core/tests/publisher_safety.rs:196-202` (inline `new` call)
- Modify: `crates/rabbit-rs-core/tests/publisher_safety.rs:221-227` (inline `new` call)
- Modify: `crates/rabbit-rs-core/tests/publisher_safety.rs:296-304` (inline `with_flags` call)
- Modify: `crates/rabbit-rs-core/tests/publisher_safety.rs:331-339` (inline `with_flags` call)
- Modify: `crates/rabbit-rs-core/tests/publisher_safety.rs:366-374` (inline `with_flags` call)
- Modify: `crates/rabbit-rs-core/tests/publisher_safety.rs:406-414` (inline `with_flags` call)
- Modify: `crates/rabbit-rs-core/tests/publisher_safety.rs:446-454` (inline `with_flags` call)

- [ ] **Step 1: Update helper `config` function**

Replace lines 28-36:

```rust
fn config(max_messages: usize, max_bytes: usize) -> PublisherConfig {
    PublisherConfig::new(
        max_messages,
        max_bytes,
        Duration::from_millis(1),
        32,
        Duration::from_secs(5),
    )
}
```

with:

```rust
fn config() -> PublisherConfig {
    PublisherConfig::new(32, Duration::from_secs(5))
}
```

- [ ] **Step 2: Delete the two batcher-specific tests**

Delete lines 76-108 (the `#[tokio::test(start_paused = true)]` attribute, `flushes_when_max_messages_is_reached`, the second `#[tokio::test(start_paused = true)]` attribute, and `flushes_when_max_bytes_is_reached`).

- [ ] **Step 3: Update all `config(...)` call sites**

Search for `config(2, 1_024)`, `config(10, 5)`, and any other calls to the old `config(max_messages, max_bytes)` helper. Replace each with `config()`.

The calls were in the deleted tests (already removed), so no remaining call sites should need this change. Verify with:

Run: `rtk rg 'config\(\d' crates/rabbit-rs-core/tests/publisher_safety.rs`
Expected: no matches

- [ ] **Step 4: Update inline `new` call at line 196**

Replace:

```rust
        PublisherConfig::new(
            1,
            1_024,
            Duration::from_millis(1),
            32,
            Duration::from_millis(10),
        ),
```

with:

```rust
        PublisherConfig::new(32, Duration::from_millis(10)),
```

- [ ] **Step 5: Update inline `new` call at line 221**

Replace:

```rust
        PublisherConfig::new(
            256,
            1_048_576,
            Duration::from_secs(1),
            1,
            Duration::from_secs(5),
        ),
```

with:

```rust
        PublisherConfig::new(1, Duration::from_secs(5)),
```

- [ ] **Step 6: Update `with_flags` call #1 (line 296)**

Replace:

```rust
    let config = PublisherConfig::with_flags(
        1,
        1_024,
        Duration::from_millis(1),
        32,
        Duration::from_secs(5),
        false,
        true,
    );
```

with:

```rust
    let config = PublisherConfig::with_flags(32, Duration::from_secs(5), false, true);
```

- [ ] **Step 7: Update `with_flags` call #2 (line 331)**

Replace:

```rust
    let config = PublisherConfig::with_flags(
        1,
        1_024,
        Duration::from_millis(1),
        32,
        Duration::from_secs(5),
        true,
        true,
    );
```

with:

```rust
    let config = PublisherConfig::with_flags(32, Duration::from_secs(5), true, true);
```

- [ ] **Step 8: Update `with_flags` call #3 (line 366)**

Replace:

```rust
    let config = PublisherConfig::with_flags(
        1,
        1_024,
        Duration::from_millis(1),
        32,
        Duration::from_secs(5),
        true,
        false,
    );
```

with:

```rust
    let config = PublisherConfig::with_flags(32, Duration::from_secs(5), true, false);
```

- [ ] **Step 9: Update `with_flags` call #4 (line 406)**

Replace:

```rust
    let config = PublisherConfig::with_flags(
        1,
        1_024,
        Duration::from_millis(1),
        32,
        Duration::from_secs(5),
        true,
        true,
    );
```

with:

```rust
    let config = PublisherConfig::with_flags(32, Duration::from_secs(5), true, true);
```

- [ ] **Step 10: Update `with_flags` call #5 (line 446)**

Replace:

```rust
    let config = PublisherConfig::with_flags(
        1,
        1_024,
        Duration::from_millis(1),
        32,
        Duration::from_secs(5),
        true,
        true,
    );
```

with:

```rust
    let config = PublisherConfig::with_flags(32, Duration::from_secs(5), true, true);
```

- [ ] **Step 11: Run `cargo fmt`**

Run: `rtk cargo fmt --all`
Expected: no formatting errors

- [ ] **Step 12: Run publisher_safety tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher_safety`
Expected: all tests pass

- [ ] **Step 13: Commit**

```bash
git add crates/rabbit-rs-core/tests/publisher_safety.rs
git commit -m "test: update publisher_safety tests for batcher removal"
```

---

### Task 6: Update `publisher_recovery.rs` tests

**Files:**
- Modify: `crates/rabbit-rs-core/tests/publisher_recovery.rs:28-36` (helper `config`)
- Modify: `crates/rabbit-rs-core/tests/publisher_recovery.rs:196` (inline `new` call)

- [ ] **Step 1: Update helper `config` function**

Replace lines 28-36:

```rust
fn config(capacity: usize) -> PublisherConfig {
    PublisherConfig::new(
        1,
        1_024,
        Duration::from_millis(1),
        capacity,
        Duration::from_secs(5),
    )
}
```

with:

```rust
fn config(capacity: usize) -> PublisherConfig {
    PublisherConfig::new(capacity, Duration::from_secs(5))
}
```

- [ ] **Step 2: Update inline `new` call at line 196**

Replace:

```rust
        PublisherConfig::new(10, 1_024, Duration::from_secs(1), 8, Duration::from_secs(5)),
```

with:

```rust
        PublisherConfig::new(8, Duration::from_secs(5)),
```

- [ ] **Step 3: Run `cargo fmt`**

Run: `rtk cargo fmt --all`
Expected: no formatting errors

- [ ] **Step 4: Run publisher_recovery tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher_recovery`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core/tests/publisher_recovery.rs
git commit -m "test: update publisher_recovery tests for batcher removal"
```

---

### Task 7: Update remaining test files

**Files:**
- Modify: `crates/rabbit-rs-core/tests/publisher_delay.rs:31-38`
- Modify: `crates/rabbit-rs-core/tests/recovery_coordinator.rs:61-69`
- Modify: `crates/rabbit-rs-core/tests/metrics_snapshot.rs:90-96,150-156,311-317`
- Modify: `crates/rabbit-rs-core/tests/delivery_attempts.rs:241-247`
- Modify: `crates/rabbit-rs-core/tests/consumer_semantics.rs:107-113,130`

- [ ] **Step 1: Update `publisher_delay.rs` helper**

Replace lines 31-38:

```rust
fn publisher_config() -> PublisherConfig {
    PublisherConfig::new(
        256,
        1_024 * 1_024,
        Duration::from_millis(1),
        32,
        Duration::from_secs(30),
    )
}
```

with:

```rust
fn publisher_config() -> PublisherConfig {
    PublisherConfig::new(32, Duration::from_secs(30))
}
```

- [ ] **Step 2: Update `recovery_coordinator.rs` helper**

Replace lines 61-69:

```rust
fn publisher_config() -> PublisherConfig {
    PublisherConfig::new(
        1,
        1_024,
        Duration::from_millis(1),
        8,
        Duration::from_secs(5),
    )
}
```

with:

```rust
fn publisher_config() -> PublisherConfig {
    PublisherConfig::new(8, Duration::from_secs(5))
}
```

- [ ] **Step 3: Update `metrics_snapshot.rs` call #1 (line 90)**

Replace:

```rust
        PublisherConfig::new(
            1,
            1_024,
            Duration::from_millis(1),
            1,
            Duration::from_secs(5),
        ),
```

with:

```rust
        PublisherConfig::new(1, Duration::from_secs(5)),
```

- [ ] **Step 4: Update `metrics_snapshot.rs` call #2 (line 150)**

Replace:

```rust
        PublisherConfig::new(
            1,
            1_024,
            Duration::from_millis(1),
            1,
            Duration::from_secs(5),
        ),
```

with:

```rust
        PublisherConfig::new(1, Duration::from_secs(5)),
```

- [ ] **Step 5: Update `metrics_snapshot.rs` call #3 (line 311)**

Replace:

```rust
        PublisherConfig::new(
            1,
            1_024,
            Duration::from_millis(1),
            MESSAGE_COUNT,
            Duration::from_secs(5),
        ),
```

with:

```rust
        PublisherConfig::new(MESSAGE_COUNT, Duration::from_secs(5)),
```

- [ ] **Step 6: Update `delivery_attempts.rs` call (line 241)**

Replace:

```rust
        PublisherConfig::new(
            1,
            1_024,
            Duration::from_millis(1),
            8,
            Duration::from_secs(5),
        ),
```

with:

```rust
        PublisherConfig::new(8, Duration::from_secs(5)),
```

- [ ] **Step 7: Update `consumer_semantics.rs` call #1 (line 107)**

Replace:

```rust
        PublisherConfig::new(
            1,
            1_024,
            Duration::from_millis(1),
            32,
            Duration::from_secs(5),
        ),
```

with:

```rust
        PublisherConfig::new(32, Duration::from_secs(5)),
```

- [ ] **Step 8: Update `consumer_semantics.rs` call #2 (line 130)**

Replace:

```rust
        PublisherConfig::new(1, 1_024, Duration::from_millis(1), 8, timeout),
```

with:

```rust
        PublisherConfig::new(8, timeout),
```

- [ ] **Step 9: Run `cargo fmt`**

Run: `rtk cargo fmt --all`
Expected: no formatting errors

- [ ] **Step 10: Run affected tests**

Run: `rtk cargo test -p rabbit-rs-core --test publisher_delay && rtk cargo test -p rabbit-rs-core --test recovery_coordinator && rtk cargo test -p rabbit-rs-core --test metrics_snapshot && rtk cargo test -p rabbit-rs-core --test delivery_attempts && rtk cargo test -p rabbit-rs-core --test consumer_semantics`
Expected: all tests pass

- [ ] **Step 11: Commit**

```bash
git add crates/rabbit-rs-core/tests/publisher_delay.rs crates/rabbit-rs-core/tests/recovery_coordinator.rs crates/rabbit-rs-core/tests/metrics_snapshot.rs crates/rabbit-rs-core/tests/delivery_attempts.rs crates/rabbit-rs-core/tests/consumer_semantics.rs
git commit -m "test: update remaining test files for batcher removal"
```

---

### Task 8: Update benchmarks

**Files:**
- Modify: `crates/rabbit-rs-core/benches/batching.rs:91-99`
- Modify: `crates/rabbit-rs-core/benches/publisher_actor.rs:65-73`

- [ ] **Step 1: Update `batching.rs` `with_flags` call**

Replace lines 91-99:

```rust
                                let config = PublisherConfig::with_flags(
                                    batch_size,
                                    2 * 1024 * 1024,
                                    std::time::Duration::from_millis(1),
                                    1024,
                                    std::time::Duration::from_secs(30),
                                    confirms,
                                    true,
                                );
```

with:

```rust
                                let config = PublisherConfig::with_flags(
                                    1024,
                                    std::time::Duration::from_secs(30),
                                    confirms,
                                    true,
                                );
```

- [ ] **Step 2: Update `publisher_actor.rs` `with_flags` call**

Replace lines 65-73:

```rust
                        let config = PublisherConfig::with_flags(
                            batch_size,
                            2 * 1024 * 1024,
                            std::time::Duration::from_millis(1),
                            1024,
                            std::time::Duration::from_secs(30),
                            true,
                            true,
                        );
```

with:

```rust
                        let config = PublisherConfig::with_flags(
                            1024,
                            std::time::Duration::from_secs(30),
                            true,
                            true,
                        );
```

- [ ] **Step 3: Run `cargo fmt`**

Run: `rtk cargo fmt --all`
Expected: no formatting errors

- [ ] **Step 4: Verify benchmarks compile**

Run: `rtk cargo bench -p rabbit-rs-core --no-run`
Expected: compiles without errors

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-core/benches/batching.rs crates/rabbit-rs-core/benches/publisher_actor.rs
git commit -m "bench: update benchmarks for batcher removal"
```

---

### Task 9: Update design documentation

**Files:**
- Modify: `docs/plans/2026-07-30-rabbitmq-native-design.md:102` (remove step 3)
- Modify: `docs/plans/2026-07-30-rabbitmq-native-design.md:108` (remove batching mention)
- Modify: `docs/plans/2026-07-30-rabbitmq-native-design.md:112` (remove "batches" mention)
- Modify: `docs/plans/2026-07-30-rabbitmq-native-design.md:240-241` (remove batch defaults)
- Modify: `docs/plans/2026-07-30-rabbitmq-native-design.md:246` (remove batch calibration line)

- [ ] **Step 1: Remove batching step from publisher description**

In `docs/plans/2026-07-30-rabbitmq-native-design.md`, line 102, remove:

```
3. groups commands by destination and channel;
```

Renumber subsequent steps (4→3, 5→4, 6→5, 7→6).

- [ ] **Step 2: Remove batching mention from paragraph**

Line 108, replace:

```
A reliable publish call waits for its confirmation before handing control back to PHP. The publishBatch method transmits a full array in a single FFI crossing and is the fast path for Laravel bulk.
```

with:

```
A reliable publish call waits for its confirmation before handing control back to PHP. The publishBatch method transmits a full array in a single FFI crossing and is the fast path for Laravel bulk.
```

- [ ] **Step 3: Remove "batches" mention from capacity paragraph**

Line 112, replace:

```
This capacity covers pending commands and in-flight confirms so that an actor draining its channel during a long outage cannot accumulate unbounded memory.
```

with:

```
This capacity covers pending commands and in-flight confirms so that an actor draining its channel during a long outage cannot accumulate unbounded memory.
```

- [ ] **Step 4: Remove batch defaults from healthy values**

Remove lines 240-241:

```
- maximum batch of 256 messages or 1 MiB;
- flush at 1 ms;
```

- [ ] **Step 5: Remove batch calibration line**

Line 246, remove:

```
Batch and prefetch values must be calibrated by benchmark before the stable V1.
```

Replace with:

```
Prefetch values must be calibrated by benchmark before the stable V1.
```

- [ ] **Step 6: Commit**

```bash
git add docs/plans/2026-07-30-rabbitmq-native-design.md
git commit -m "docs: remove batcher references from design document"
```

---

### Task 10: Full quality gate

- [ ] **Step 1: Run fmt check**

Run: `rtk cargo fmt --all -- --check`
Expected: no diff

- [ ] **Step 2: Run clippy**

Run: `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: no warnings

- [ ] **Step 3: Run all tests**

Run: `rtk cargo test --workspace --all-targets`
Expected: all tests pass

- [ ] **Step 4: Run full quality gate**

Run: `rtk ./scripts/check.sh`
Expected: all checks pass

- [ ] **Step 5: Verify no stale references**

Run: `rtk rg 'max_messages|max_bytes|flush_interval|flush_batch|flush_deadline|Batcher' crates/ --ignore='*.md'`
Expected: no matches in source files

- [ ] **Step 6: Final commit if any cleanup needed**

If step 5 found any stale references, fix them, fmt, and commit. If clean, no commit needed.
