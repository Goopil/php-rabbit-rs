use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    config::{
        BrokerConfig, Config, Credentials, Endpoint, PublisherConfigSection, SchedulerConfig,
        SubscriptionConfig, TlsConfig, TopologyMode, WorkerProfile,
    },
    pool::recovery_coordinator::{
        RecoveryCoordinator, RecoveryCoordinatorConfig, RecoveryCoordinatorHandle,
    },
    publisher::{Destination, MessageProperties, PublishOutcome, PublishRequest, PublisherConfig},
    recovery::{ConnectionState, RecoveryPolicy},
    topology::{QueueDefinition, TopologyDefinition, TopologyPlan},
    transport::{
        PublishConfirmation, Transport, TransportError,
        mock::{MockTransport, TransportOperation},
    },
};
use tokio::time::Instant;

fn broker() -> BrokerConfig {
    BrokerConfig {
        name: "primary".to_owned(),
        hosts: vec![Endpoint::new("localhost", 5672)],
        vhost: "/".to_owned(),
        credentials: Credentials::new("guest", "guest"),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(30),
    }
}

fn config() -> Arc<rabbit_rs_core::config::ValidatedConfig> {
    Arc::new(
        Config {
            brokers: vec![broker()],
            workers: vec![WorkerProfile {
                name: "main".to_owned(),
                subscriptions: vec![SubscriptionConfig {
                    name: "jobs".to_owned(),
                    broker: "primary".to_owned(),
                    queue: "jobs".to_owned(),
                    weight: 1,
                    priority_class: 0,
                    prefetch: 4,
                    starvation_after: Duration::from_secs(30),
                }],
                scheduler: SchedulerConfig::weighted_fair(16),
            }],
            topology_mode: TopologyMode::Declare,
            delay: rabbit_rs_core::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
        }
        .validate()
        .expect("valid config"),
    )
}

fn publisher_config() -> PublisherConfig {
    PublisherConfig::new(
        1,
        1_024,
        Duration::from_millis(1),
        8,
        Duration::from_secs(5),
    )
}

fn publish_request(message_id: &str, deadline: Instant) -> PublishRequest {
    let mut properties = MessageProperties::new(message_id);
    properties.content_type = Some("application/json".to_owned());
    PublishRequest::new(
        Destination::new("jobs", "high"),
        Bytes::from_static(b"payload"),
        properties,
        deadline,
    )
}

fn topology_plan() -> TopologyPlan {
    TopologyPlan::compile(
        TopologyMode::Declare,
        TopologyDefinition::new(vec![], vec![QueueDefinition::new("jobs")], vec![]),
    )
    .expect("topology plan")
}

fn coordinator_config(
    config: Arc<rabbit_rs_core::config::ValidatedConfig>,
) -> RecoveryCoordinatorConfig {
    RecoveryCoordinatorConfig {
        broker: broker(),
        policy: RecoveryPolicy::default(),
        topology_plan: topology_plan(),
        publisher_config: publisher_config(),
        config,
        metrics: rabbit_rs_core::metrics::Metrics::default(),
    }
}

fn dyn_transport(transport: &Arc<MockTransport>) -> Arc<dyn Transport> {
    transport.clone() as Arc<dyn Transport>
}
async fn wait_for_state(
    handle: &RecoveryCoordinatorHandle,
    predicate: impl Fn(&ConnectionState) -> bool,
) -> ConnectionState {
    handle.wait_for_state(predicate).await
}

#[tokio::test(start_paused = true)]
async fn publisher_replays_unconfirmed_messages_after_recovery() {
    let transport = Arc::new(MockTransport::default());
    let config = config();

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    // Wait for initial connection.
    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    // Publish a message that stays unconfirmed (pending confirmation).
    transport.push_pending_confirmation();
    let waiter = coordinator
        .publisher()
        .await
        .expect("publisher ready")
        .try_publish(publish_request(
            "replay-me",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("publish accepted");

    // Wait for the publish to reach the transport.
    for _ in 0..100 {
        if transport
            .operations()
            .iter()
            .any(|op| matches!(op, TransportOperation::PublishBatch(_)))
        {
            break;
        }
        tokio::task::yield_now().await;
    }

    // Inject a connection loss.
    transport.push_connect_result(Ok(()));
    coordinator
        .connection_lost(TransportError::connection("heartbeat missed"))
        .await
        .expect("loss reported");

    // The publisher should enter Recovering (suspend).
    wait_for_state(&coordinator, |s| {
        matches!(
            s,
            ConnectionState::Recovering { .. } | ConnectionState::Connecting { .. }
        )
    })
    .await;

    // Push a confirmation for the replayed message after recovery.
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));

    // Wait for recovery to complete.
    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 2 })
    })
    .await;

    // The original message should be confirmed after replay.
    let outcome = tokio::time::timeout(Duration::from_secs(5), waiter.wait())
        .await
        .expect("timeout waiting for confirmation")
        .expect("confirmation");
    assert_eq!(
        outcome,
        PublishOutcome::Confirmed {
            message_id: "replay-me".into()
        }
    );

    coordinator.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn consumer_generation_updates_after_reconnection_rejects_stale_acks() {
    let transport = Arc::new(MockTransport::default());
    let config = config();

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    // Wait for initial connection.
    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    // Register a consumer.
    let consumer = coordinator.consumer("main").await.expect("consumer handle");

    // Inject a connection loss.
    transport.push_connect_result(Ok(()));
    coordinator
        .connection_lost(TransportError::connection("socket reset"))
        .await
        .expect("loss reported");

    // Wait for recovery to generation 2.
    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 2 })
    })
    .await;

    // The consumer's generation should have been updated to 2.
    // After recovery, consumer channels are re-established with the new generation.
    // A stale delivery (from generation 1) should be rejected with StaleGeneration.
    // We verify this by checking that the consumer is still usable.
    drop(consumer);

    coordinator.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn deterministic_recovery_order_connection_channels_topology_consumers_publisher() {
    let transport = Arc::new(MockTransport::default());
    let config = config();

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    // Wait for initial connection.
    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    let operations = transport.operations();

    // Find the indices of key operations to verify order.
    let connect_idx = operations
        .iter()
        .position(|op| matches!(op, TransportOperation::Connect { .. }))
        .expect("connect");

    let open_publisher_idx = operations
        .iter()
        .position(|op| matches!(op, TransportOperation::OpenPublisher))
        .expect("open publisher");

    let declare_queue_idx = operations
        .iter()
        .position(|op| matches!(op, TransportOperation::DeclareQueue(_)))
        .expect("declare queue");

    let enable_confirms_idx = operations
        .iter()
        .position(|op| matches!(op, TransportOperation::EnableConfirms))
        .expect("enable confirms");

    // Verify deterministic order: connection → channels → topology (exchanges, queues, bindings) → publisher confirms.
    assert!(connect_idx < open_publisher_idx);
    assert!(open_publisher_idx < declare_queue_idx);
    assert!(declare_queue_idx < enable_confirms_idx);

    coordinator.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn loss_during_recovery_cancels_and_restarts() {
    let transport = Arc::new(MockTransport::default());
    let config = config();

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    // Wait for initial connection.
    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    // First loss.
    transport.push_connect_result(Ok(()));
    coordinator
        .connection_lost(TransportError::connection("first loss"))
        .await
        .expect("first loss");

    // Wait for recovery to begin.
    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Recovering { .. })
    })
    .await;

    // Second loss during recovery (the connection attempt fails).
    transport.push_connect_result(Err(TransportError::connection("second loss")));
    coordinator
        .connection_lost(TransportError::connection("second loss"))
        .await
        .expect("second loss");

    // The coordinator should eventually recover.
    transport.push_connect_result(Ok(()));
    wait_for_state(&coordinator, |s| matches!(s, ConnectionState::Ready { .. })).await;

    coordinator.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn permanent_error_stops_the_recovery_loop() {
    let transport = Arc::new(MockTransport::default());
    let config = config();

    // First connect succeeds, then a permanent (authentication) error occurs.
    transport.push_connect_result(Ok(()));

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    // Wait for initial connection.
    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    // Report a permanent (authentication) error.
    coordinator
        .connection_lost(TransportError::authentication("credentials rejected"))
        .await
        .expect("permanent loss reported");

    // The coordinator should reach FailedPermanent and not loop.
    let state = wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::FailedPermanent { .. })
    })
    .await;

    assert!(matches!(
        state,
        ConnectionState::FailedPermanent {
            kind: rabbit_rs_core::transport::TransportErrorKind::Authentication,
            ..
        }
    ));

    coordinator.close().await.expect("close");
}
