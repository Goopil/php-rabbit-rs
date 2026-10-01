# connection.blocked/unblocked signal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface AMQP `connection.blocked`/`connection.unblocked` (broker resource-alarm backpressure) as a first-class measured signal — transport events, a gauge + counter, log lines with the broker reason, and two `Pool::stats()` keys — without changing any publish failure semantics (issue #251).

**Architecture:** The lapin event stream already delivers blocked/unblocked events into the transport adapter, which discards them today. `TransportErrorStream` is generalized into `TransportEventStream` carrying `TransportEvent { Error, Blocked, Unblocked }`; the connection actor (the single consumer) records blocked state into `Metrics` and the log facade, and clears the gauge whenever the connection dies. Publishes stay fully deadline-bound.

**Tech Stack:** Rust 1.98.1 (edition 2024, `#![forbid(unsafe_code)]`), tokio (paused-time tests), mock transport scripting, PHP extension stats array (Zend), Pest.

**Spec:** `docs/superpowers/specs/2026-09-28-connection-blocked-design.md`

## Global Constraints

- No unsafe code; never weaken `#![forbid(unsafe_code)]` or workspace lints.
- Blocked/Unblocked events must NEVER trigger recovery and must NEVER change publish failure semantics: confirm timeouts and publication deadlines remain the only failure bounds.
- Broker-provided reason strings are truncated to 200 chars in logs; never credentials (facade redaction contract unchanged).
- Deterministic tests only: `#[tokio::test(start_paused = true)]` or pure `yield_now` polling loops; no real sleeps.
- Run `rtk cargo fmt --all` after every Rust edit batch.
- Focused iteration: `rtk cargo test -p rabbit-rs-core`; full gate at the end: `rtk ./scripts/check.sh`.
- Working directory for all tasks: the feature worktree (`.worktrees/connection-blocked-signal` on branch `feature/connection-blocked-signal`).

---

### Task 1: Transport event stream (trait + lapin mapping + mock knobs)

**Files:**
- Modify: `crates/rabbit-rs-core/src/transport.rs:288-312` (trait rename + new enum)
- Modify: `crates/rabbit-rs-core/src/transport/lapin.rs:74-117` (stream mapping) and `lapin.rs:740-855` (unit tests)
- Modify: `crates/rabbit-rs-core/src/transport/mock.rs:60-65,126-133,325-370` (state, knobs, stream)
- Modify: `crates/rabbit-rs-core/src/pool/connection_actor.rs:9-13,219,280,321,370-433` (mechanical rename only, behavior unchanged)

**Interfaces:**
- Produces (used by Task 2): `TransportEvent { Error(TransportError), Blocked(String), Unblocked }` (`Clone, Debug, Eq, PartialEq`), trait `TransportEventStream { async fn next(&mut self) -> Option<TransportEvent> }`, `TransportConnection::event_stream() -> Box<dyn TransportEventStream>`, `MockTransport::push_blocked(&str)`, `MockTransport::push_unblocked()`.

- [ ] **Step 1: Write the failing lapin unit tests**

In `crates/rabbit-rs-core/src/transport/lapin.rs`, inside `mod tests`, add (next to the existing stream tests):

```rust
#[tokio::test]
async fn blocked_events_surface_with_the_broker_reason() {
    use super::LapinEventStream;

    let mut stream = LapinEventStream {
        events: Box::pin(futures_util::stream::iter(vec![
            lapin::Event::ConnectionBlocked("memory alarm triggered".into()),
        ])),
        connection_alive: Box::new(|| true),
    };

    let event = stream.next().await.expect("a blocked event must surface");

    assert!(
        matches!(event, crate::transport::TransportEvent::Blocked(reason) if reason == "memory alarm triggered"),
        "blocked must carry the broker-provided reason, got {event:?}"
    );
}

#[tokio::test]
async fn unblocked_events_surface() {
    use super::LapinEventStream;

    let mut stream = LapinEventStream {
        events: Box::pin(futures_util::stream::iter(vec![lapin::Event::ConnectionUnblocked])),
        connection_alive: Box::new(|| false),
    };

    let event = stream
        .next()
        .await
        .expect("an unblocked event must surface");

    assert!(matches!(event, crate::transport::TransportEvent::Unblocked));
}
```

Also update the two existing tests (`channel_scoped_errors_do_not_kill_a_live_connection`, `connection_refusals_surface_once_the_connection_is_down`, `errors_surface_again_once_the_connection_dies_mid_stream`): the stream type becomes `LapinEventStream` and the asserted item is a `TransportEvent` — the `Error` assertions become `matches!(event, TransportEvent::Error(error) if error.kind() == ...)`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `rtk cargo test -p rabbit-rs-core lapin`
Expected: FAIL (type `LapinEventStream` and variant `TransportEvent` do not exist).

- [ ] **Step 3: Add `TransportEvent` and rename the stream trait in transport.rs**

Replace `TransportConnection::error_stream` (transport.rs:288) and the `TransportErrorStream` trait (transport.rs:306-312) with:

```rust
/// Connection-level events surfaced by an active connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportEvent {
    /// Connection-fatal error: the connection is lost and recovery must run.
    Error(TransportError),
    /// The broker applied backpressure (resource alarm) and stopped reading
    /// from publishers on this connection. Carries the broker-provided
    /// reason string.
    Blocked(String),
    /// The broker lifted backpressure; publications flow again.
    Unblocked,
}

#[async_trait]
pub trait TransportConnection: Send + Sync {
    /// Returns a stream of connection-level events that reports the liveness
    /// and backpressure state of this connection.
    ///
    /// The caller should create the stream once per connection and select
    /// over it: a [`TransportEvent::Error`] means the connection is lost and
    /// recovery must run. Stream termination (`None`) also means the
    /// connection is gone.
    fn event_stream(&self) -> Box<dyn TransportEventStream>;
    // ... existing open_publisher / open_consumer / close methods unchanged
}

/// Connection-level events, one connection per stream.
#[async_trait]
pub trait TransportEventStream: Send {
    /// Waits for the next connection event. Returns `None` when the source is
    /// gone (the underlying connection no longer exists).
    async fn next(&mut self) -> Option<TransportEvent>;
}
```

- [ ] **Step 4: Map lapin events in the adapter**

In `crates/rabbit-rs-core/src/transport/lapin.rs`, replace `LapinErrorStream` (lines 74-107) with:

```rust
/// Maps the lapin connection event stream onto transport events.
///
/// Connection-fatal errors are classified through the connection status (see
/// the comment in `next`); `ConnectionBlocked`/`ConnectionUnblocked` carry
/// the broker's resource-alarm backpressure signal and are informational
/// only — they must never affect the connection lifecycle.
struct LapinEventStream {
    events: Pin<Box<dyn futures_util::Stream<Item = lapin::Event> + Send>>,
    connection_alive: Box<dyn Fn() -> bool + Send>,
}

#[async_trait]
impl super::TransportEventStream for LapinEventStream {
    async fn next(&mut self) -> Option<TransportEvent> {
        loop {
            let event = self.events.as_mut().next().await?;
            match event {
                lapin::Event::ConnectionBlocked(reason) => {
                    return Some(TransportEvent::Blocked(reason));
                }
                lapin::Event::ConnectionUnblocked => return Some(TransportEvent::Unblocked),
                lapin::Event::Error(error) => {
                    // lapin surfaces channel-scoped failures (e.g. a failed
                    // passive declare on an admin channel) on the connection
                    // event stream while the connection itself stays
                    // `Connected`; it flips the connection state *before*
                    // emitting connection-fatal errors. The status is
                    // therefore read at event time: a live connection proves
                    // the failure was channel-scoped and must not tear the
                    // connection down — the owning channel already reports it
                    // to its caller, and `DelayKeepAlive` containment depends
                    // on this.
                    if (self.connection_alive)() {
                        continue;
                    }
                    return Some(TransportEvent::Error(map_lapin_error(error)));
                }
                // `Connected` and `SendFlow` carry no backpressure or
                // liveness signal relevant to this stream.
                _ => continue,
            }
        }
    }
}
```

In `impl TransportConnection for LapinConnection` (lapin.rs:109-117):

```rust
    fn event_stream(&self) -> Box<dyn super::TransportEventStream> {
        let status = self.inner.status().clone();
        Box::new(LapinEventStream {
            events: Box::pin(self.inner.events_listener()),
            connection_alive: Box::new(move || status.connected()),
        })
    }
```

Import `TransportEvent` in the `use super::{...}` block (lapin.rs:18-23).

- [ ] **Step 5: Script blocked/unblocked in the mock transport**

In `crates/rabbit-rs-core/src/transport/mock.rs`:

State (mock.rs:60-65) — rename and widen:

```rust
    /// Connection-level events armed on the event stream, mirroring a broker
    /// connection that dies (socket reset, heartbeat timeout) or applies
    /// backpressure (resource alarm).
    connection_events: VecDeque<TransportEvent>,
    /// Wakes event streams parked on an empty queue so an event pushed after
    /// a stream parked still surfaces. Always armed.
    error_notify: Arc<tokio::sync::Notify>,
```

`push_connection_error` (mock.rs:126-133) keeps its name and signature, now wrapping the error:

```rust
    /// Scripts a connection-level error: every `event_stream()` created by
    /// the current mock connection yields it once, like lapin's event
    /// listener reporting a socket reset or heartbeat failure.
    pub fn push_connection_error(&self, error: TransportError) {
        let mut state = self.state();
        state.connection_events.push_back(TransportEvent::Error(error));
        state.error_notify.notify_one();
    }

    /// Scripts a broker backpressure episode: every `event_stream()` created
    /// by the current mock connection yields it once, like lapin reporting
    /// `connection.blocked`.
    pub fn push_blocked(&self, reason: &str) {
        let mut state = self.state();
        state
            .connection_events
            .push_back(TransportEvent::Blocked(reason.to_owned()));
        state.error_notify.notify_one();
    }

    /// Scripts the end of a broker backpressure episode (`connection.unblocked`).
    pub fn push_unblocked(&self) {
        let mut state = self.state();
        state
            .connection_events
            .push_back(TransportEvent::Unblocked);
        state.error_notify.notify_one();
    }
```

Rename `MockErrorStream` → `MockEventStream` (mock.rs:325-354):

```rust
/// Pends like a live connection with no events until one is scripted.
struct MockEventStream {
    state: Arc<Mutex<MockState>>,
}

#[async_trait]
impl super::TransportEventStream for MockEventStream {
    async fn next(&mut self) -> Option<TransportEvent> {
        loop {
            let event = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.connection_events.pop_front()
            };
            if let Some(event) = event {
                return Some(event);
            }
            let notified = Arc::clone(
                &self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .error_notify,
            );
            notified.notified().await;
        }
    }
}
```

In `impl TransportConnection for MockConnection` (mock.rs:364-370):

```rust
    fn event_stream(&self) -> Box<dyn super::TransportEventStream> {
        Box::new(MockEventStream {
            state: self.state.clone(),
        })
    }
```

Import `TransportEvent` in mock.rs's transport imports.

- [ ] **Step 6: Mechanical rename in the connection actor (behavior unchanged)**

In `crates/rabbit-rs-core/src/pool/connection_actor.rs`:

- Import: `TransportEvent, TransportEventStream` (replace `TransportErrorStream`).
- `run_actor` local (line 219): `let mut events: Option<Box<dyn TransportEventStream>> = None;`
- `handle_connecting` signature (line 280) + body (line 321): `*events = Some(new_connection.event_stream());`
- `handle_ready` signature (line 370) and body: `next_transport_event` select arm routes only `Error`; the other variants are ignored for now (recording lands in Task 2):

```rust
async fn handle_ready(
    context: &mut ActorContext,
    connection: &mut Option<Box<dyn TransportConnection>>,
    events: &mut Option<Box<dyn TransportEventStream>>,
) -> Option<Phase> {
    // The event source lives for the whole Ready phase; every exit of this
    // loop is a phase change where the connection dies or is closed, so the
    // taken stream is simply dropped with the local binding.
    let mut events = events.take();
    loop {
        tokio::select! {
            // The transport itself reports the connection is dying (socket
            // reset, heartbeat failure): route it exactly like a reported
            // `Command::ConnectionLost`.
            event = next_transport_event(&mut events) => {
                match event {
                    TransportEvent::Error(error) => {
                        close_connection(connection).await;
                        return Some(route_loss(&context.states, error));
                    }
                    // Backpressure is informational; recording lands with the
                    // metrics work. It must never affect the lifecycle.
                    TransportEvent::Blocked(_) | TransportEvent::Unblocked => continue,
                }
            }
            command = context.commands.recv() => {
                // ... existing command arms unchanged ...
            }
        }
    }
}

/// Waits for the next event of the active connection. Pends forever when
/// there is no connection, leaving commands as the only wake-up source.
async fn next_transport_event(
    events: &mut Option<Box<dyn TransportEventStream>>,
) -> TransportEvent {
    match events {
        Some(stream) => stream.next().await.unwrap_or_else(|| {
            TransportEvent::Error(TransportError::connection("transport event stream ended"))
        }),
        None => std::future::pending().await,
    }
}
```

- [ ] **Step 7: Format and run the core suite**

Run: `rtk cargo fmt --all && rtk cargo test -p rabbit-rs-core`
Expected: PASS (including the two new lapin tests and all existing transport/actor tests).

- [ ] **Step 8: Commit**

```bash
git add crates/rabbit-rs-core/src/transport.rs crates/rabbit-rs-core/src/transport/lapin.rs crates/rabbit-rs-core/src/transport/mock.rs crates/rabbit-rs-core/src/pool/connection_actor.rs
git commit -m "feat(transport): Generalize the connection stream to TransportEvent

TransportErrorStream becomes TransportEventStream and carries
TransportEvent::{Error, Blocked, Unblocked}. The lapin adapter maps
connection.blocked/unblocked (currently discarded) and the mock gains
push_blocked/push_unblocked scripting. The connection actor routes
only Error; backpressure handling lands with the metrics task.

Refs #251"
```

---

### Task 2: Metrics + actor recording + log lines + integration tests

**Files:**
- Modify: `crates/rabbit-rs-core/src/metrics.rs` (2 atomics, snapshot fields, record methods)
- Modify: `crates/rabbit-rs-core/src/pool/connection_actor.rs` (record + log arms, gauge reset on every connection-death exit)
- Create: `crates/rabbit-rs-core/tests/connection_blocked.rs`

**Interfaces:**
- Consumes (from Task 1): `TransportEvent::{Error, Blocked, Unblocked}`, `TransportEventStream`, `MockTransport::push_blocked`, `push_unblocked`, `push_connection_error`.
- Produces (used by Task 3): `MetricsSnapshot { connection_blocked: u64, connection_blocked_total: u64 }` (serialized, snake_case names as-is).

- [ ] **Step 1: Write the failing integration tests**

Create `crates/rabbit-rs-core/tests/connection_blocked.rs`:

```rust
//! connection.blocked/unblocked (issue #251): broker backpressure must be a
//! first-class measured signal — a gauge, an episode counter, and a log line
//! carrying the broker-provided reason — without ever affecting the
//! connection lifecycle or publish failure semantics.

mod common;

use std::{sync::Arc, time::Duration};

use rabbit_rs_core::{
    log::{self, Level, Record, Sink},
    metrics::Metrics,
    pool::connection_actor::ConnectionActor,
    recovery::{ConnectionState, EqualJitter, RecoveryPolicy, TokioClock},
    transport::mock::MockTransport,
};

mod helper {
    use super::*;
    pub use crate::common::broker;

    pub async fn wait_for_state(
        states: &tokio::sync::watch::Receiver<ConnectionState>,
        predicate: impl Fn(&ConnectionState) -> bool,
    ) {
        for _ in 0..2000 {
            if predicate(&states.borrow().clone()) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("state never matched: {:?}", states.borrow().clone());
    }

    pub async fn wait_until(condition: impl Fn() -> bool, description: &str) {
        for _ in 0..2000 {
            if condition() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("condition never held: {description}");
    }

    pub fn ready(generation: u64) -> impl Fn(&ConnectionState) -> bool {
        move |state| {
            matches!(state, ConnectionState::Ready { generation: g } if *g == generation)
        }
    }

    pub fn spawn_actor(
        transport: Arc<MockTransport>,
        metrics: Metrics,
    ) -> rabbit_rs_core::pool::connection_actor::ConnectionActorHandle {
        ConnectionActor::spawn_with_dependencies_and_metrics(
            transport as Arc<dyn rabbit_rs_core::transport::Transport>,
            broker("primary", "/", "guest"),
            RecoveryPolicy::default(),
            Arc::new(TokioClock),
            Arc::new(EqualJitter),
            metrics,
        )
    }
}

use helper::*;

/// Blocked/unblocked transitions are measured (gauge + episode counter) while
/// the connection lifecycle is untouched: the state stays Ready and the
/// counter accumulates across episodes.
#[tokio::test(start_paused = true)]
async fn blocked_and_unblocked_are_measured_without_touching_the_lifecycle() {
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Ok(()));
    let metrics = Metrics::default();
    let actor = spawn_actor(transport.clone(), metrics.clone());
    actor.start().await.expect("actor started");
    let states = actor.subscribe();
    wait_for_state(&states, ready(1)).await;

    transport.push_blocked("memory alarm triggered");
    wait_until(
        || metrics.snapshot().connection_blocked == 1,
        "gauge set on connection.blocked",
    );
    assert_eq!(metrics.snapshot().connection_blocked_total, 1);

    transport.push_unblocked();
    wait_until(
        || metrics.snapshot().connection_blocked == 0,
        "gauge cleared on connection.unblocked",
    );
    assert_eq!(
        metrics.snapshot().connection_blocked_total, 1,
        "unblocked must not touch the episode counter"
    );

    // A second episode accumulates.
    transport.push_blocked("disk free limit");
    wait_until(
        || metrics.snapshot().connection_blocked_total == 2,
        "episode counter accumulates",
    );
    assert_eq!(metrics.snapshot().connection_blocked, 1);

    // The connection itself is untouched and remains Ready (generation 1).
    assert!(ready(1)(&states.borrow().clone()));

    actor.close().await.expect("close");
}

/// A connection that dies while blocked starts its successor unblocked: the
/// gauge resets, the episode counter survives, recovery proceeds.
#[tokio::test(start_paused = true)]
async fn connection_loss_while_blocked_resets_the_gauge_keeps_the_counter() {
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Ok(()));
    let metrics = Metrics::default();
    let actor = spawn_actor(transport.clone(), metrics.clone());
    actor.start().await.expect("actor started");
    let states = actor.subscribe();
    wait_for_state(&states, ready(1)).await;

    transport.push_blocked("memory alarm triggered");
    wait_until(
        || metrics.snapshot().connection_blocked == 1,
        "gauge set on connection.blocked",
    );

    // The connection dies mid-block; the next attempt succeeds.
    transport.push_connect_result(Ok(()));
    transport.push_connection_error(rabbit_rs_core::transport::TransportError::connection(
        "heartbeat timeout",
    ));
    wait_for_state(&states, ready(2)).await;

    wait_until(
        || metrics.snapshot().connection_blocked == 0,
        "gauge resets on connection loss",
    );
    assert_eq!(
        metrics.snapshot().connection_blocked_total, 1,
        "the episode counter survives the loss"
    );

    actor.close().await.expect("close");
}

/// The blocked log line carries the broker-provided reason (truncated), and
/// unblocked is logged at info level.
#[tokio::test(start_paused = true)]
async fn blocked_events_log_the_broker_reason() {
    recorder();
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Ok(()));
    let metrics = Metrics::default();
    let actor = spawn_actor(transport.clone(), metrics.clone());
    actor.start().await.expect("actor started");
    let states = actor.subscribe();
    wait_for_state(&states, ready(1)).await;

    transport.push_blocked("memory resource limit alarm set");
    wait_until(
        || metrics.snapshot().connection_blocked == 1,
        "gauge set before asserting the log line",
    );
    let record = wait_for_record("blocked: memory resource limit alarm set").await;
    assert_eq!(record.level, Level::Warn);
    assert_eq!(record.target, "connection_actor");

    transport.push_unblocked();
    wait_until(
        || metrics.snapshot().connection_blocked == 0,
        "gauge cleared before asserting the info line",
    );
    let unblocked = wait_for_record("unblocked").await;
    assert_eq!(unblocked.level, Level::Info);

    actor.close().await.expect("close");
}

/// A blocked reason longer than the log cap is truncated to 200 characters.
#[tokio::test(start_paused = true)]
async fn oversized_blocked_reasons_are_truncated() {
    recorder();
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Ok(()));
    let metrics = Metrics::default();
    let actor = spawn_actor(transport.clone(), metrics.clone());
    actor.start().await.expect("actor started");
    let states = actor.subscribe();
    wait_for_state(&states, ready(1)).await;

    let long_reason = "x".repeat(500);
    transport.push_blocked(&long_reason);
    wait_until(
        || metrics.snapshot().connection_blocked == 1,
        "gauge set before asserting truncation",
    );
    let record = wait_for_record("blocked: ").await;
    let reason = record
        .message
        .split("blocked: ")
        .nth(1)
        .unwrap_or_default();
    assert_eq!(reason.chars().count(), 200, "reason must be capped");

    actor.close().await.expect("close");
}
```

Append to the same file (shared log sink, mirroring `tests/log_facade.rs` — one process-wide sink per test binary, first installation wins):

```rust
// ---------------------------------------------------------------------------
// Recorder sink shared by every test in this binary (see tests/log_facade.rs).
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct CapturedRecord {
    level: Level,
    target: &'static str,
    message: String,
}

#[derive(Default)]
struct Recorder {
    records: std::sync::Mutex<Vec<CapturedRecord>>,
}

impl Sink for Recorder {
    fn log(&self, record: Record<'_>) {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(CapturedRecord {
                level: record.level,
                target: record.target,
                message: record.message.to_owned(),
            });
    }
}

static RECORDER: std::sync::OnceLock<Arc<Recorder>> = std::sync::OnceLock::new();

fn recorder() -> Arc<Recorder> {
    RECORDER
        .get_or_init(|| {
            let recorder = Arc::new(Recorder::default());
            assert!(
                log::install(recorder.clone()),
                "the first sink must install"
            );
            recorder
        })
        .clone()
}

fn find_record(marker: &str) -> Option<CapturedRecord> {
    recorder()
        .records
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|record| record.message.contains(marker))
        .cloned()
}

async fn wait_for_record(marker: &str) -> CapturedRecord {
    for _ in 0..2000 {
        if let Some(record) = find_record(marker) {
            return record;
        }
        tokio::task::yield_now().await;
    }
    let collected = recorder()
        .records
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|record| format!("[{}] {:#?}", record.target, record))
        .collect::<Vec<_>>()
        .join("\n");
    panic!("no log record containing {marker:?}; records so far:\n{collected}");
}
```

Note: the pause-based tests use `start_paused = true` only to forbid real-time drift; all waits are `yield_now` loops, so no clock advance is needed.

- [ ] **Step 2: Run tests to verify they fail**

Run: `rtk cargo test -p rabbit-rs-core --test connection_blocked`
Expected: FAIL — `Metrics`/snapshot lack `connection_blocked`/`connection_blocked_total` (compile error) and no log records appear.

- [ ] **Step 3: Add the metrics**

In `crates/rabbit-rs-core/src/metrics.rs`:

`MetricsInner` (after `publication_retries_total`, line ~122):

```rust
    /// Broker backpressure episodes (`connection.blocked`) across all
    /// connections of this client.
    connection_blocked_total: AtomicU64,
    /// Gauge: `1` while the broker is applying backpressure to any current
    /// connection, `0` otherwise.
    connection_blocked: AtomicU64,
```

`snapshot()` (after `publication_retries_total`, line ~49):

```rust
            connection_blocked_total: load(&self.inner.connection_blocked_total),
            connection_blocked: load(&self.inner.connection_blocked),
```

Recording methods (after `record_publication_retry`, line ~101):

```rust
    /// Records a broker backpressure episode (`connection.blocked`): the
    /// episode counter grows and the gauge flags backpressure.
    pub(crate) fn record_connection_blocked(&self) {
        increment(&self.inner.connection_blocked_total);
        self.inner.connection_blocked.store(1, Ordering::Relaxed);
    }

    /// Clears the backpressure gauge (on `connection.unblocked` or when the
    /// connection goes away — a successor starts unblocked by definition).
    /// The episode counter is never reset.
    pub(crate) fn clear_connection_blocked(&self) {
        self.inner.connection_blocked.store(0, Ordering::Relaxed);
    }
```

`MetricsSnapshot` fields (after `publication_retries_total`, line ~151):

```rust
    /// Broker backpressure episodes observed (`connection.blocked`), across
    /// all connections of this client. Never reset by recovery.
    pub connection_blocked_total: u64,
    /// Whether the broker is currently applying backpressure to any current
    /// connection (`1`) or not (`0`).
    pub connection_blocked: u64,
```

- [ ] **Step 4: Record and log in the connection actor**

In `crates/rabbit-rs-core/src/pool/connection_actor.rs`:

Replace the Task 1 placeholder arm in `handle_ready` with:

```rust
            event = next_transport_event(&mut events) => {
                match event {
                    TransportEvent::Error(error) => {
                        // A dead connection is never blocked: its successor
                        // starts unblocked. The episode counter survives.
                        context.metrics.clear_connection_blocked();
                        close_connection(connection).await;
                        return Some(route_loss(&context.states, error));
                    }
                    TransportEvent::Blocked(reason) => {
                        context.metrics.record_connection_blocked();
                        crate::log::warn(
                            "connection_actor",
                            format!(
                                "broker '{}' blocked: {}",
                                context.config.name,
                                truncate_reason(&reason),
                            ),
                        );
                    }
                    TransportEvent::Unblocked => {
                        context.metrics.clear_connection_blocked();
                        crate::log::info(
                            "connection_actor",
                            format!("broker '{}' unblocked", context.config.name),
                        );
                    }
                }
            }
```

Add the gauge clear to every other connection-death exit inside `handle_ready` (gauge-only, one line each, before the existing statements):

- `Some(Command::ConnectionLost(error))` arm: `context.metrics.clear_connection_blocked();`
- `Some(Command::Close(completed))` arm: `context.metrics.clear_connection_blocked();` (before `shutdown(...)`)
- `None` arm: `context.metrics.clear_connection_blocked();`

Add near the bottom of the file (module-private helpers):

```rust
/// Caps the broker-provided blocked reason in log output. The string is a
/// protocol-provided diagnostic, but it is still external input and must not
/// balloon a log line.
const BLOCKED_REASON_MAX_CHARS: usize = 200;

fn truncate_reason(reason: &str) -> String {
    reason.chars().take(BLOCKED_REASON_MAX_CHARS).collect()
}
```

- [ ] **Step 5: Run the new tests, then the core suite**

Run: `rtk cargo test -p rabbit-rs-core --test connection_blocked`
Expected: PASS (4 tests).

Run: `rtk cargo test -p rabbit-rs-core && rtk cargo clippy -p rabbit-rs-core --all-targets --all-features -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Format and commit**

```bash
rtk cargo fmt --all
git add crates/rabbit-rs-core/src/metrics.rs crates/rabbit-rs-core/src/pool/connection_actor.rs crates/rabbit-rs-core/tests/connection_blocked.rs
git commit -m "feat(pool): Measure broker backpressure on connection.blocked

The connection actor records connection.blocked/unblocked into a
gauge (connection_blocked) and an episode counter
(connection_blocked_total), and logs the broker-provided reason
(truncated to 200 chars) at warn level. The gauge resets whenever
the connection dies; the counter survives. Blocked events never
affect the connection lifecycle or publish failure semantics.

Refs #251"
```

---

### Task 3: PHP surface — `Pool::stats()` keys, docblock, stub, Pest test

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/pool.rs:284-321` (docblock contract + stats keys)
- Modify: `crates/rabbit-rs-php/tests/Pool/PoolRegistryTest.php:47-56` (quiet-pool key assertions)
- Regenerate: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` (via `./scripts/stubs.sh`)

**Interfaces:**
- Consumes (from Task 2): `MetricsSnapshot { connection_blocked: u64, connection_blocked_total: u64 }`.
- Produces: `Pool::stats()` array gains `connection_blocked` (0/1) and `connection_blocked_total` (int).

- [ ] **Step 1: Write the failing Pest test**

In `crates/rabbit-rs-php/tests/Pool/PoolRegistryTest.php`, extend the existing quiet-pool block (the test asserting `publish_buffered`, `publish_buffered_bytes`, `publication_retries_total` read zero on a quiet pool, lines ~47-56) with:

```php
        // Broker backpressure signal (#251): must exist on a quiet pool.
        expect($pool->stats()['connection_blocked'])->toBe(0);
        expect($pool->stats()['connection_blocked_total'])->toBe(0);
```

- [ ] **Step 2: Run to verify it fails**

Run: `./scripts/test-extension.sh` (requires the lab broker; if no lab is available, run the Pest suite as the script does and record that only broker-less failures are expected here)
Expected: FAIL — `connection_blocked` key missing.

- [ ] **Step 3: Expose the metrics in `Pool::stats()`**

In `crates/rabbit-rs-php/src/classes/pool.rs`:

Docblock (pool.rs:284-293) — add the two keys after `publication_retries_total`:

```php
    /// @return array{closed: bool, pid: int, handle: string,
    ///   publishes_total: int, confirmations_total: int, returns_total: int,
    ///   backpressure_total: int, publication_retries_total: int,
    ///   connection_blocked: int, connection_blocked_total: int,
    ///   reconnects_total: int, deliveries_total: int,
    ///   duplicates_total: int, acks_total: int, rejects_total: int,
    ///   dropped_publications_total: int, dropped_error_records_total: int,
    ///   publish_buffered: int, publish_buffered_bytes: int,
    ///   confirmation_latency_p50: int, confirmation_latency_p95: int,
    ///   confirmation_latency_p99: int, settlement_latency_p50: int,
    ///   settlement_latency_p95: int, settlement_latency_p99: int}
```

Counter loop (pool.rs:305-321) — insert after `publication_retries_total`:

```rust
            ("connection_blocked", metrics.connection_blocked),
            (
                "connection_blocked_total",
                metrics.connection_blocked_total,
            ),
```

- [ ] **Step 4: Regenerate the stub and validate**

Run: `./scripts/stubs.sh --out crates/rabbit-rs-php/stubs/rabbit_rs.stub.php && php -l crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`
Expected: stub regenerated; `php -l` reports no syntax errors. `git diff` on the stub must show only the new docblock keys for `stats`.

- [ ] **Step 5: Run the extension test suite**

Run: `./scripts/test-extension.sh` (lab broker required; skip the broker-dependent Pest groups only if no lab is running, but the `PoolRegistryTest` quiet-pool case must run)
Expected: PASS.

- [ ] **Step 6: Format and commit**

```bash
rtk cargo fmt --all
git add crates/rabbit-rs-php/src/classes/pool.rs crates/rabbit-rs-php/stubs/rabbit_rs.stub.php crates/rabbit-rs-php/tests/Pool/PoolRegistryTest.php
git commit -m "feat(php): Expose broker backpressure in Pool::stats

stats() gains connection_blocked (0/1 gauge) and
connection_blocked_total (episode counter); the docblock return
contract and the generated stubs are updated accordingly.

Refs #251"
```

---

### Task 4: Docs + full quality gate

**Files:**
- Modify: `docs/plans/2026-09-11-reliability-hardening.md:100` (backlog row)

- [ ] **Step 1: Update the backlog row**

Replace the `connection.blocked`/`unblocked handling` row in the backlog table with:

```markdown
| `connection.blocked`/`unblocked` handling | Implemented (2026-09-28, spec `docs/superpowers/specs/2026-09-28-connection-blocked-design.md`) | Observability only: `TransportEventStream` carries `Blocked`/`Unblocked`; the connection actor records `connection_blocked` (gauge) + `connection_blocked_total` (counter) and logs the broker reason; `Pool::stats()` exposes both. Deadlines remain the only publish failure bound. External outbox / Laravel DB spool stays future work. |
```

(Keep the row inside the existing table; if the table column widths differ, keep the two-column structure: Decision = "Implemented (2026-09-28, ...)", Notes = the observability summary above.)

- [ ] **Step 2: Run the full gate**

Run: `rtk ./scripts/check.sh`
Expected: fmt clean, clippy clean (`-D warnings`), all tests pass, `cargo deny` ok, composer valid.

- [ ] **Step 3: Commit**

```bash
git add docs/plans/2026-09-11-reliability-hardening.md
git commit -m "docs(plans): Mark connection.blocked/unblocked implemented

The backlog row now points to the design spec and summarizes the
shipped signal: gauge + episode counter + log line + stats keys,
with publish semantics unchanged.

Refs #251"
```

---

## Self-Review (completed at plan time)

- **Spec coverage:** transport enum (Task 1), lapin mapping (Task 1), mock knobs (Task 1), actor recording + log + gauge reset (Task 2), metrics + snapshot (Task 2), integration tests incl. log assertions (Task 2), PHP stats + docblock + stub + Pest (Task 3), backlog doc update (Task 4). Lab e2e stays optional/manual per spec.
- **Placeholders:** none — every step carries exact code or exact commands.
- **Type consistency:** `TransportEvent` variants and `MetricsSnapshot` field names (`connection_blocked`, `connection_blocked_total`) are identical across Tasks 1-3 and the spec.
