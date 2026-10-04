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

    pub type ConnectionActorHandle = rabbit_rs_core::pool::connection_actor::ConnectionActorHandle;

    pub async fn wait_for_state(
        states: &tokio::sync::watch::Receiver<ConnectionState>,
        predicate: impl Fn(&ConnectionState) -> bool,
    ) {
        for _ in 0..2000 {
            if predicate(&states.borrow().clone()) {
                return;
            }
            tokio::time::advance(Duration::from_millis(5)).await;
            tokio::task::yield_now().await;
        }
        panic!("state never matched: {:?}", states.borrow().clone());
    }

    pub async fn wait_until(condition: impl Fn() -> bool, description: &str) {
        for _ in 0..2000 {
            if condition() {
                return;
            }
            tokio::time::advance(Duration::from_millis(5)).await;
            tokio::task::yield_now().await;
        }
        panic!("condition never held: {description}");
    }

    pub fn ready(generation: u64) -> impl Fn(&ConnectionState) -> bool {
        move |state| matches!(state, ConnectionState::Ready { generation: g } if *g == generation)
    }

    pub fn spawn_actor(transport: Arc<MockTransport>, metrics: Metrics) -> ConnectionActorHandle {
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
    )
    .await;
    wait_until(
        || metrics.snapshot().connection_blocked_total == 1,
        "episode counter recorded",
    )
    .await;

    transport.push_unblocked();
    wait_until(
        || metrics.snapshot().connection_blocked == 0,
        "gauge cleared on connection.unblocked",
    )
    .await;
    assert_eq!(
        metrics.snapshot().connection_blocked_total,
        1,
        "unblocked must not touch the episode counter"
    );

    // A second episode accumulates.
    transport.push_blocked("disk free limit alarm");
    wait_until(
        || metrics.snapshot().connection_blocked_total == 2,
        "episode counter accumulates",
    )
    .await;
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
    )
    .await;
    wait_until(
        || metrics.snapshot().connection_blocked_total == 1,
        "episode counter recorded",
    )
    .await;

    // The connection dies mid-block; the next attempt succeeds.
    transport.push_connect_result(Ok(()));
    transport.push_connection_error(rabbit_rs_core::transport::TransportError::connection(
        "heartbeat timeout",
    ));
    wait_for_state(&states, ready(2)).await;

    wait_until(
        || metrics.snapshot().connection_blocked == 0,
        "gauge resets on connection loss",
    )
    .await;
    assert_eq!(
        metrics.snapshot().connection_blocked_total,
        1,
        "the episode counter survives the loss"
    );

    actor.close().await.expect("close");
}

/// The blocked log line carries the broker-provided reason, and unblocked is
/// logged at info level.
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
    )
    .await;
    let record = wait_for_record("blocked: memory resource limit alarm set").await;
    assert_eq!(record.level, Level::Warn);
    assert_eq!(record.target, "connection_actor");

    transport.push_unblocked();
    wait_until(
        || metrics.snapshot().connection_blocked == 0,
        "gauge cleared before asserting the info line",
    )
    .await;
    let unblocked = wait_for_record("unblocked").await;
    assert_eq!(unblocked.level, Level::Info);

    actor.close().await.expect("close");
}

/// When two brokers share the same Metrics instance, the `connection_blocked`
/// gauge must count how many brokers are currently blocked, not act as a
/// simple on/off flag. Broker B unblocking must not erase the fact that broker
/// A remains blocked.
#[tokio::test(start_paused = true)]
async fn multi_broker_blocked_gauge_counts_brokers() {
    // Two independent transports / brokers sharing the SAME metrics instance.
    let transport_a = Arc::new(MockTransport::default());
    transport_a.push_connect_result(Ok(()));
    let transport_b = Arc::new(MockTransport::default());
    transport_b.push_connect_result(Ok(()));

    let metrics = Metrics::default();

    let actor_a = ConnectionActor::spawn_with_dependencies_and_metrics(
        transport_a.clone() as Arc<dyn rabbit_rs_core::transport::Transport>,
        broker("broker_a", "/", "guest"),
        RecoveryPolicy::default(),
        Arc::new(TokioClock),
        Arc::new(EqualJitter),
        metrics.clone(),
    );
    actor_a.start().await.expect("actor A started");
    let states_a = actor_a.subscribe();
    wait_for_state(&states_a, ready(1)).await;

    let actor_b = ConnectionActor::spawn_with_dependencies_and_metrics(
        transport_b.clone() as Arc<dyn rabbit_rs_core::transport::Transport>,
        broker("broker_b", "/", "guest"),
        RecoveryPolicy::default(),
        Arc::new(TokioClock),
        Arc::new(EqualJitter),
        metrics.clone(),
    );
    actor_b.start().await.expect("actor B started");
    let states_b = actor_b.subscribe();
    wait_for_state(&states_b, ready(1)).await;

    // Both brokers report Ready at generation 1 with gauge == 0.
    assert_eq!(
        metrics.snapshot().connection_blocked,
        0,
        "gauge starts at zero"
    );

    // Broker A becomes blocked.
    transport_a.push_blocked("memory alarm triggered");
    wait_until(
        || metrics.snapshot().connection_blocked == 1,
        "gauge increments for A blocked",
    )
    .await;

    // Broker B also becomes blocked.
    transport_b.push_blocked("disk alarm triggered");
    wait_until(
        || metrics.snapshot().connection_blocked == 2,
        "gauge increments for B blocked (count 2)",
    )
    .await;
    assert_eq!(
        metrics.snapshot().connection_blocked_total,
        2,
        "episode counter recorded both episodes"
    );

    // Broker B unblocks — gauge must drop to 1 (A still blocked), NOT 0.
    transport_b.push_unblocked();
    wait_until(
        || metrics.snapshot().connection_blocked == 1,
        "gauge decrements for B unblocked (A still blocked)",
    )
    .await;
    assert_eq!(
        metrics.snapshot().connection_blocked_total,
        2,
        "unblocked must not touch the episode counter"
    );

    // Broker A unblocks — gauge should go back to 0.
    transport_a.push_unblocked();
    wait_until(
        || metrics.snapshot().connection_blocked == 0,
        "gauge decrements for A unblocked (all clear)",
    )
    .await;

    // Connections remain Ready at generation 1.
    assert!(ready(1)(&states_a.borrow().clone()));
    assert!(ready(1)(&states_b.borrow().clone()));

    actor_a.close().await.expect("close A");
    actor_b.close().await.expect("close B");
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
    )
    .await;
    let record = wait_for_record("blocked: xxxxx").await;
    let reason = record.message.split("blocked: ").nth(1).unwrap_or_default();
    assert_eq!(reason.chars().count(), 200, "reason must be capped");

    actor.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// Recorder sink shared by every test in this binary (see tests/log_facade.rs).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
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
        tokio::time::advance(Duration::from_millis(5)).await;
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
