//! Transport liveness (issue #66, audit F-01/F-23): connection loss must be
//! detected from the transport itself — an error event on the connection or a
//! terminated consumer delivery stream — and must trigger recovery without any
//! manual `connection_lost` report. Recovery-generation failures must surface
//! as a metric, not only as stderr noise.

mod common;

use std::{sync::Arc, time::Duration};

use rabbit_rs_core::{
    config::{SafetyMode, TopologyMode},
    consumer::ConsumerErrorKind,
    pool::recovery_coordinator::{RecoveryCoordinator, RecoveryCoordinatorConfig},
    publisher::PublisherConfig,
    recovery::{ConnectionState, RecoveryPolicy},
    topology::{QueueDefinition, TopologyDefinition, TopologyPlan},
    transport::{TransportError, mock::MockTransport},
};

mod helper {
    use super::*;

    pub use crate::common::{broker, config, worker_profile};

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
            policy: RecoveryPolicy::default(),
            topology_plan: topology_plan(),
            publisher_config: publisher_config(),
            config,
            metrics: rabbit_rs_core::metrics::Metrics::default(),
            requested_profiles: Arc::new(std::sync::Mutex::new(
                ["main".to_owned()].into_iter().collect(),
            )),
        }
    }

    pub fn dyn_transport(
        transport: &Arc<MockTransport>,
    ) -> Arc<dyn rabbit_rs_core::transport::Transport> {
        transport.clone() as Arc<dyn rabbit_rs_core::transport::Transport>
    }

    pub async fn wait_for_state(
        handle: &RecoveryCoordinatorHandle,
        predicate: impl Fn(&ConnectionState) -> bool,
    ) -> ConnectionState {
        handle.wait_for_state(predicate).await
    }
}

use helper::*;
use rabbit_rs_core::pool::recovery_coordinator::RecoveryCoordinatorHandle;

fn ready(generation: u64) -> impl Fn(&ConnectionState) -> bool {
    move |state| matches!(state, ConnectionState::Ready { generation: g } if *g == generation)
}

/// F-01: a connection-level error event from the transport (socket death,
/// heartbeat failure) must reach the connection actor and drive recovery —
/// no explicit `connection_lost` call anywhere in the test.
#[tokio::test(start_paused = true)]
async fn transport_error_event_triggers_recovery_without_manual_loss() {
    let transport = Arc::new(MockTransport::default());
    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    wait_for_state(&coordinator, ready(1)).await;

    // The next connection attempt succeeds; the transport then reports the
    // live connection dying on its own (heartbeat failure / socket reset).
    transport.push_connect_result(Ok(()));
    transport.push_connection_error(TransportError::connection("heartbeat timeout"));

    let recovered = tokio::time::timeout(Duration::from_secs(30), async {
        wait_for_state(&coordinator, ready(2)).await
    })
    .await;

    assert!(
        recovered.is_ok(),
        "a transport error event must trigger recovery without a manual connection_lost report"
    );

    let metrics = coordinator.metrics_snapshot();
    assert!(
        metrics.reconnects_total >= 1,
        "recovery must be observable in reconnects_total, got {metrics:?}"
    );

    coordinator.close().await.expect("close");
}

/// F-01 (consumer side): when a delivery stream terminates (broker died,
/// subscription cancelled), the consumer must surface a terminal error
/// instead of parking `next()` forever.
#[tokio::test(start_paused = true)]
async fn consumer_delivery_stream_termination_surfaces_a_terminal_error() {
    let transport = Arc::new(MockTransport::default());
    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    wait_for_state(&coordinator, ready(1)).await;

    let consumer = coordinator.consumer("main").await.expect("consumer handle");

    // No deliveries are scripted, so the mock delivery stream terminates
    // immediately — like a subscription whose connection just died.
    let error = tokio::time::timeout(Duration::from_secs(30), consumer.next())
        .await
        .expect("next() must not park forever on a terminated delivery stream")
        .expect_err("next() must surface a terminal error, not a delivery");

    assert_eq!(error.kind(), ConsumerErrorKind::Transport);
    assert!(
        error.to_string().contains("stream"),
        "terminal error must identify the terminated stream, got: {error}"
    );

    coordinator.close().await.expect("close");
}

/// F-23: recovery-generation failures must increment a dedicated counter
/// instead of only writing to stderr.
#[tokio::test(start_paused = true)]
async fn recovery_generation_failure_increments_recovery_failures_counter() {
    let transport = Arc::new(MockTransport::default());
    // Consumer establishment fails on the first generation (QoS rejected).
    transport.push_consumer_result(Err(TransportError::connection("qos rejected")));
    transport.push_connect_result(Ok(()));

    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    // The failed generation rolls back and a later generation succeeds.
    wait_for_state(&coordinator, ready(2)).await;

    let metrics = coordinator.metrics_snapshot();
    assert_eq!(
        metrics.recovery_failures_total, 1,
        "the failed recovery generation must be counted, got {metrics:?}"
    );

    coordinator.close().await.expect("close");
}

/// A connect attempt that never completes (silent network black hole) must
/// not block the recovery lifecycle: the actor bounds each attempt and falls
/// back to the backoff policy, so a later attempt can still connect.
#[tokio::test(start_paused = true)]
async fn wedged_connect_attempt_cannot_block_recovery() {
    use rabbit_rs_core::{
        metrics::Metrics,
        pool::connection_actor::ConnectionActor,
        recovery::{EqualJitter, TokioClock},
    };

    let transport = Arc::new(MockTransport::default());
    // The very first connect attempt wedges forever (accepted socket, no
    // handshake data — e.g. a load balancer or docker proxy in front of a
    // dead broker).
    let gate = transport.push_connect_gate();

    let handle = ConnectionActor::spawn_with_dependencies_and_metrics(
        dyn_transport(&transport),
        broker("primary", "/", "guest"),
        RecoveryPolicy::default(),
        Arc::new(TokioClock),
        Arc::new(EqualJitter),
        Metrics::default(),
    );
    handle.start().await.expect("actor started");
    gate.wait_entered().await;

    // The gate is never released. The next scripted attempt succeeds.
    transport.push_connect_result(Ok(()));
    let states = handle.subscribe();
    let mut ready = false;
    for _ in 0..2000 {
        if matches!(
            states.borrow().clone(),
            ConnectionState::Ready { generation: 1 }
        ) {
            ready = true;
            break;
        }
        tokio::time::advance(Duration::from_millis(50)).await;
        tokio::task::yield_now().await;
    }

    assert!(
        ready,
        "a wedged connect attempt must be bounded so recovery can proceed"
    );
    handle.close().await.expect("close");
}
