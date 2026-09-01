//! Log facade behavior tests (issue #56).
//!
//! The facade installs one process-wide sink per test binary (first
//! installation wins by design), so every test asserts on unique message
//! markers and parallel tests never observe each other's records.

use std::{
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use rabbit_rs_core::{
    config::SafetyMode,
    log::{self, Level, Record, Sink},
    pool::recovery_coordinator::{RecoveryCoordinator, RecoveryCoordinatorConfig},
    publisher::PublisherConfig,
    recovery::ConnectionState,
    topology::{QueueDefinition, TopologyDefinition, TopologyPlan},
    transport::{Transport, TransportError, mock::MockTransport},
};

mod common;

mod helper {
    use super::*;

    pub use crate::common::{broker, config, worker_profile};

    use rabbit_rs_core::config::TopologyMode;

    pub fn publisher_config() -> PublisherConfig {
        PublisherConfig::with_safety(8, Duration::from_secs(5), SafetyMode::Safe)
    }

    pub fn topology_plan() -> TopologyPlan {
        TopologyPlan::compile(
            TopologyMode::Declare,
            TopologyDefinition::new(vec![], vec![QueueDefinition::new("jobs")], vec![]),
        )
        .expect("topology plan")
    }

    pub fn coordinator_config(
        config: Arc<rabbit_rs_core::config::ValidatedConfig>,
    ) -> RecoveryCoordinatorConfig {
        RecoveryCoordinatorConfig {
            broker: broker("primary", "/", "guest"),
            policy: rabbit_rs_core::recovery::RecoveryPolicy::default(),
            topology_plan: topology_plan(),
            publisher_config: publisher_config(),
            config,
            metrics: rabbit_rs_core::metrics::Metrics::default(),
            requested_profiles: Arc::new(std::sync::Mutex::new(
                ["main".to_owned()].into_iter().collect(),
            )),
        }
    }

    pub fn dyn_transport(transport: &Arc<MockTransport>) -> Arc<dyn Transport> {
        transport.clone() as Arc<dyn Transport>
    }

    pub async fn wait_for_state(
        handle: &rabbit_rs_core::pool::recovery_coordinator::RecoveryCoordinatorHandle,
        predicate: impl Fn(&ConnectionState) -> bool,
    ) -> ConnectionState {
        handle.wait_for_state(predicate).await
    }
}

use helper::*;

// ---------------------------------------------------------------------------
// Recorder sink shared by every test in this binary.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct CapturedRecord {
    level: Level,
    target: &'static str,
    message: String,
}

#[derive(Default)]
struct Recorder {
    records: Mutex<Vec<CapturedRecord>>,
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

static RECORDER: OnceLock<Arc<Recorder>> = OnceLock::new();

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
    for _ in 0..500 {
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
        .map(|record| {
            format!(
                "[{}] {}: {}",
                record.target,
                record.level.as_str(),
                record.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    panic!("no log record containing {marker:?}; records so far:\n{collected}");
}

// ---------------------------------------------------------------------------
// Facade mechanics.
// ---------------------------------------------------------------------------

#[test]
fn facade_delivers_records_to_the_installed_sink() {
    recorder();
    log::error("log_facade_test", "facade-error-record-marker");
    log::warn("log_facade_test", "facade-warn-record-marker");
    log::info("log_facade_test", "facade-info-record-marker");

    let error = find_record("facade-error-record-marker").expect("error record");
    assert_eq!(error.level, Level::Error);
    assert_eq!(error.target, "log_facade_test");

    let warn = find_record("facade-warn-record-marker").expect("warn record");
    assert_eq!(warn.level, Level::Warn);

    let info = find_record("facade-info-record-marker").expect("info record");
    assert_eq!(info.level, Level::Info);
}

#[test]
fn install_is_first_wins() {
    let first = recorder();

    assert!(
        !log::install(Arc::new(Recorder::default())),
        "the second install must be rejected"
    );

    log::info("log_facade_test", "first-wins-marker");
    assert!(
        first
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|record| record.message.contains("first-wins-marker")),
        "records must keep flowing to the first-installed sink"
    );
}

#[test]
fn levels_order_from_info_to_error() {
    assert!(Level::Info < Level::Warn);
    assert!(Level::Warn < Level::Error);
}

#[test]
fn level_strings_are_stable() {
    assert_eq!(Level::Info.as_str(), "info");
    assert_eq!(Level::Warn.as_str(), "warn");
    assert_eq!(Level::Error.as_str(), "error");
}

// ---------------------------------------------------------------------------
// Connection actor diagnostics.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn successful_connection_logs_info_with_the_broker_name_and_generation() {
    // Install the recorder before spawning: records emitted during startup
    // must be captured.
    recorder();
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Ok(()));
    let cfg = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );
    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(cfg));

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    let record = wait_for_record("connected").await;
    assert_eq!(record.level, Level::Info);
    assert_eq!(record.target, "connection_actor");
    assert!(
        record.message.contains("primary") && record.message.contains("generation 1"),
        "info record must identify the broker and generation: {}",
        record.message
    );

    coordinator.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn recoverable_connect_failure_logs_a_warning() {
    // Install the recorder before spawning: records emitted during startup
    // must be captured.
    recorder();
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Err(TransportError::connection("unreachable-marker")));
    let cfg = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );
    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(cfg));

    let record = wait_for_record("unreachable-marker").await;
    assert_eq!(record.level, Level::Warn);
    assert_eq!(record.target, "connection_actor");
    assert!(
        record.message.contains("primary"),
        "warn record must identify the broker: {}",
        record.message
    );

    coordinator.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn permanent_failure_logs_an_error() {
    // Install the recorder before spawning: records emitted during startup
    // must be captured.
    recorder();
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Err(TransportError::authentication(
        "credentials rejected by the broker",
    )));
    let cfg = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );
    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(cfg));

    let record = wait_for_record("failed permanently").await;
    assert_eq!(record.level, Level::Error);
    assert_eq!(record.target, "connection_actor");
    assert!(
        record.message.contains("primary"),
        "error record must identify the broker: {}",
        record.message
    );

    coordinator.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// Recovery coordinator diagnostics.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn failed_recovery_generation_logs_a_warning() {
    // Install the recorder before spawning: records emitted during startup
    // must be captured.
    recorder();
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Ok(()));
    transport.push_consumer_result(Err(TransportError::connection("test failure")));
    transport.push_connect_result(Ok(()));
    transport.push_consumer_result(Ok(()));

    let cfg = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );
    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(cfg));

    wait_for_state(&coordinator, |s| matches!(s, ConnectionState::Ready { .. })).await;
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::timeout(
        Duration::from_secs(10),
        wait_for_state(
            &coordinator,
            |s| matches!(s, ConnectionState::Ready { generation: g } if *g >= 2),
        ),
    )
    .await
    .expect("coordinator should reach the retry generation");

    let record = wait_for_record("recovery generation 1 failed").await;
    assert_eq!(record.level, Level::Warn);
    assert_eq!(record.target, "recovery_coordinator");
    assert!(
        record.message.contains("consumer spawn failed"),
        "the warn record must carry the typed error context: {}",
        record.message
    );

    coordinator.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// Panic audit: waiting on a stopped coordinator must not panic.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn wait_for_state_returns_closed_when_the_coordinator_stops() {
    // Install the recorder before spawning: records emitted during startup
    // must be captured.
    recorder();
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Ok(()));
    let cfg = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );
    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(cfg));

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    coordinator.close().await.expect("close");

    // The coordinator task has stopped: no transition can ever match this
    // predicate again, so the wait must resolve to the terminal state
    // instead of panicking.
    let state = coordinator
        .wait_for_state(|s| matches!(s, ConnectionState::Ready { generation: 999 }))
        .await;
    assert_eq!(state, ConnectionState::Closed);
}

#[tokio::test(start_paused = true)]
async fn state_reports_closed_after_the_coordinator_stops() {
    // Install the recorder before spawning: records emitted during startup
    // must be captured.
    recorder();
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Ok(()));
    let cfg = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );
    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(cfg));

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;
    coordinator.close().await.expect("close");

    assert_eq!(coordinator.state(), ConnectionState::Closed);
}

// ---------------------------------------------------------------------------
// Redaction: no log record may carry the endpoint (host or port) or any
// credential material — only broker names, generations, and transport error
// messages are ever logged.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn log_records_never_leak_endpoints_or_credentials() {
    // Install the recorder before spawning: records emitted during startup
    // must be captured.
    recorder();
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Ok(()));
    transport.push_consumer_result(Err(TransportError::connection("test failure")));
    transport.push_connect_result(Ok(()));
    transport.push_consumer_result(Ok(()));
    transport.push_connect_result(Err(TransportError::authentication(
        "credentials rejected by the broker",
    )));

    let cfg = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );
    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(cfg));

    // Drive one successful generation, one failed recovery, and one permanent
    // failure so every log site emits at least once.
    wait_for_state(&coordinator, |s| matches!(s, ConnectionState::Ready { .. })).await;
    tokio::time::advance(Duration::from_secs(2)).await;
    for _ in 0..200 {
        if matches!(coordinator.state(), ConnectionState::FailedPermanent { .. }) {
            break;
        }
        tokio::task::yield_now().await;
    }
    coordinator.close().await.expect("close");

    let records = recorder()
        .records
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(
        !records.is_empty(),
        "this scenario must produce diagnostic records"
    );
    for record in records {
        assert!(
            !record.message.contains("localhost") && !record.message.contains("5672"),
            "record must not contain the endpoint: {}",
            record.message
        );
        assert!(
            !record.message.contains("secret") && !record.message.contains("guest"),
            "record must not contain credentials: {}",
            record.message
        );
    }
}
