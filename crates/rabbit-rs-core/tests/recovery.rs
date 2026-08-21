use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use bytes::Bytes;
use rabbit_rs_core::{
    config::{
        BrokerConfig, Config, Credentials, Endpoint, PublisherConfigSection, SchedulerConfig,
        SubscriptionConfig, TlsConfig, TopologyMode, WorkerProfile,
    },
    pool::connection_actor::ConnectionActor,
    pool::recovery_coordinator::{
        RecoveryCoordinator, RecoveryCoordinatorConfig, RecoveryCoordinatorHandle,
    },
    publisher::{Destination, MessageProperties, PublishOutcome, PublishRequest, PublisherConfig},
    recovery::{Clock, ConnectionState, IdentityJitter, JitterSource, RecoveryPolicy},
    topology::{QueueDefinition, TopologyDefinition, TopologyPlan},
    transport::{
        PublishConfirmation, Transport, TransportError, TransportErrorKind,
        mock::{MockTransport, TransportOperation},
    },
};
use tokio::{sync::watch, time::Instant};

mod helper {
    use super::*;

    pub fn broker() -> BrokerConfig {
        BrokerConfig {
            name: "primary".to_owned(),
            hosts: vec![Endpoint::new("localhost", 5672)],
            vhost: "/".to_owned(),
            credentials: Credentials::new("guest", "guest"),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        }
    }

    pub fn config() -> Arc<rabbit_rs_core::config::ValidatedConfig> {
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

    pub fn publisher_config() -> PublisherConfig {
        PublisherConfig::new(
            1,
            1_024,
            Duration::from_millis(1),
            8,
            Duration::from_secs(5),
        )
    }

    pub fn publish_request(message_id: &str, deadline: Instant) -> PublishRequest {
        let mut properties = MessageProperties::new(message_id);
        properties.content_type = Some("application/json".to_owned());
        PublishRequest::new(
            Destination::new("jobs", "high"),
            Bytes::from_static(b"payload"),
            properties,
            deadline,
        )
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
            broker: broker(),
            policy: RecoveryPolicy::default(),
            topology_plan: topology_plan(),
            publisher_config: publisher_config(),
            config,
            metrics: rabbit_rs_core::metrics::Metrics::default(),
        }
    }

    pub fn dyn_transport(transport: &Arc<MockTransport>) -> Arc<dyn Transport> {
        transport.clone() as Arc<dyn Transport>
    }

    pub async fn wait_for_state(
        handle: &RecoveryCoordinatorHandle,
        predicate: impl Fn(&ConnectionState) -> bool,
    ) -> ConnectionState {
        handle.wait_for_state(predicate).await
    }

    pub async fn wait_for_actor(
        receiver: &mut watch::Receiver<ConnectionState>,
        predicate: impl Fn(&ConnectionState) -> bool,
    ) -> ConnectionState {
        loop {
            let current = receiver.borrow().clone();
            if predicate(&current) {
                return current;
            }
            receiver.changed().await.expect("connection actor alive");
        }
    }

    #[derive(Clone, Default)]
    pub struct RecordingClock {
        delays: Arc<Mutex<Vec<Duration>>>,
    }

    impl RecordingClock {
        pub fn delays(&self) -> Vec<Duration> {
            self.delays
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    #[async_trait]
    impl Clock for RecordingClock {
        async fn sleep(&self, duration: Duration) {
            self.delays
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(duration);
            tokio::time::sleep(duration).await;
        }
    }

    pub struct AdditiveJitter(pub Duration);

    impl JitterSource for AdditiveJitter {
        fn apply(&self, delay: Duration) -> Duration {
            delay.saturating_add(self.0)
        }
    }
}

use helper::*;

// ---------------------------------------------------------------------------
// Recovery coordinator tests (from recovery_coordinator.rs)
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn publisher_replays_unconfirmed_messages_after_recovery() {
    let transport = Arc::new(MockTransport::default());
    let config = config();

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

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

    for _ in 0..100 {
        if transport
            .operations()
            .iter()
            .any(|op| matches!(op, TransportOperation::Publish(_)))
        {
            break;
        }
        tokio::task::yield_now().await;
    }

    transport.push_connect_result(Ok(()));
    coordinator
        .connection_lost(TransportError::connection("heartbeat missed"))
        .await
        .expect("loss reported");

    wait_for_state(&coordinator, |s| {
        matches!(
            s,
            ConnectionState::Recovering { .. } | ConnectionState::Connecting { .. }
        )
    })
    .await;

    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 2 })
    })
    .await;

    let outcome = tokio::time::timeout(Duration::from_secs(5), waiter.wait())
        .await
        .expect("timeout waiting for confirmation")
        .expect("confirmation");
    assert_eq!(
        outcome,
        PublishOutcome::Confirmed {
            message_id: "replay-me".to_owned()
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

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    let consumer = coordinator.consumer("main").await.expect("consumer handle");

    transport.push_connect_result(Ok(()));
    coordinator
        .connection_lost(TransportError::connection("socket reset"))
        .await
        .expect("loss reported");

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 2 })
    })
    .await;

    drop(consumer);

    coordinator.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn deterministic_recovery_order_connection_channels_topology_consumers_publisher() {
    let transport = Arc::new(MockTransport::default());
    let config = config();

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    let operations = transport.operations();

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

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    transport.push_connect_result(Ok(()));
    coordinator
        .connection_lost(TransportError::connection("first loss"))
        .await
        .expect("first loss");

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Recovering { .. })
    })
    .await;

    transport.push_connect_result(Err(TransportError::connection("second loss")));
    coordinator
        .connection_lost(TransportError::connection("second loss"))
        .await
        .expect("second loss");

    transport.push_connect_result(Ok(()));
    wait_for_state(&coordinator, |s| matches!(s, ConnectionState::Ready { .. })).await;

    coordinator.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn permanent_error_stops_the_recovery_loop() {
    let transport = Arc::new(MockTransport::default());
    let config = config();

    transport.push_connect_result(Ok(()));

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    coordinator
        .connection_lost(TransportError::authentication("credentials rejected"))
        .await
        .expect("permanent loss reported");

    let state = wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::FailedPermanent { .. })
    })
    .await;

    assert!(matches!(
        state,
        ConnectionState::FailedPermanent {
            kind: TransportErrorKind::Authentication,
            ..
        }
    ));

    coordinator.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// Recovery state machine tests (from recovery_state_machine.rs)
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn transitions_from_disconnected_through_connecting_to_ready() {
    let transport = Arc::new(MockTransport::default());
    let actor = ConnectionActor::spawn(transport, broker(), RecoveryPolicy::default());
    let mut states = actor.subscribe();

    assert_eq!(*states.borrow(), ConnectionState::Disconnected);
    actor.start().await.expect("start command");
    wait_for_actor(&mut states, |state| {
        matches!(state, ConnectionState::Connecting { attempt: 1 })
    })
    .await;
    let ready = wait_for_actor(&mut states, |state| {
        matches!(state, ConnectionState::Ready { .. })
    })
    .await;

    assert_eq!(ready, ConnectionState::Ready { generation: 1 });
}

#[tokio::test(start_paused = true)]
async fn retries_with_100_200_and_400_millisecond_backoff() {
    let transport = Arc::new(MockTransport::default());
    for attempt in 1..=3 {
        transport.push_connect_result(Err(TransportError::connection(format!(
            "failure {attempt}"
        ))));
    }
    let clock = Arc::new(RecordingClock::default());
    let actor = ConnectionActor::spawn_with_dependencies(
        transport,
        broker(),
        RecoveryPolicy::default(),
        clock.clone(),
        Arc::new(IdentityJitter),
    );
    let mut states = actor.subscribe();

    actor.start().await.expect("start command");
    wait_for_actor(&mut states, |state| {
        matches!(
            state,
            ConnectionState::Recovering {
                retry_in,
                ..
            } if *retry_in == Duration::from_millis(100)
        )
    })
    .await;
    assert_eq!(clock.delays(), vec![Duration::from_millis(100)]);

    tokio::time::advance(Duration::from_millis(100)).await;
    wait_for_actor(&mut states, |state| {
        matches!(
            state,
            ConnectionState::Recovering {
                retry_in,
                ..
            } if *retry_in == Duration::from_millis(200)
        )
    })
    .await;

    tokio::time::advance(Duration::from_millis(200)).await;
    wait_for_actor(&mut states, |state| {
        matches!(
            state,
            ConnectionState::Recovering {
                retry_in,
                ..
            } if *retry_in == Duration::from_millis(400)
        )
    })
    .await;

    assert_eq!(
        clock.delays(),
        vec![
            Duration::from_millis(100),
            Duration::from_millis(200),
            Duration::from_millis(400),
        ]
    );
}

#[test]
fn exponential_backoff_is_capped_at_30_seconds() {
    let policy = RecoveryPolicy::default();

    assert_eq!(policy.delay_for_failure(20), Duration::from_secs(30));
}

#[tokio::test(start_paused = true)]
async fn injected_jitter_controls_the_observed_retry_delay() {
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Err(TransportError::connection("offline")));
    let actor = ConnectionActor::spawn_with_dependencies(
        transport,
        broker(),
        RecoveryPolicy::default(),
        Arc::new(RecordingClock::default()),
        Arc::new(AdditiveJitter(Duration::from_millis(25))),
    );
    let mut states = actor.subscribe();

    actor.start().await.expect("start command");
    let recovering = wait_for_actor(&mut states, |state| {
        matches!(state, ConnectionState::Recovering { .. })
    })
    .await;

    assert!(matches!(
        recovering,
        ConnectionState::Recovering { retry_in, .. }
            if retry_in == Duration::from_millis(125)
    ));
}

#[tokio::test(start_paused = true)]
async fn authentication_failure_is_permanent() {
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Err(TransportError::authentication("access refused")));
    let actor = ConnectionActor::spawn(transport, broker(), RecoveryPolicy::default());
    let mut states = actor.subscribe();

    actor.start().await.expect("start command");
    let failed = wait_for_actor(&mut states, |state| {
        matches!(state, ConnectionState::FailedPermanent { .. })
    })
    .await;

    assert!(matches!(
        failed,
        ConnectionState::FailedPermanent {
            kind: TransportErrorKind::Authentication,
            ..
        }
    ));
}

#[tokio::test(start_paused = true)]
async fn ready_connection_loss_enters_recovery() {
    let transport = Arc::new(MockTransport::default());
    let actor = ConnectionActor::spawn_with_dependencies(
        transport,
        broker(),
        RecoveryPolicy::default(),
        Arc::new(RecordingClock::default()),
        Arc::new(IdentityJitter),
    );
    let mut states = actor.subscribe();
    actor.start().await.expect("start command");
    wait_for_actor(&mut states, |state| {
        matches!(state, ConnectionState::Ready { generation: 1 })
    })
    .await;

    actor
        .connection_lost(TransportError::connection("heartbeat missed"))
        .await
        .expect("loss command");
    let recovering = wait_for_actor(&mut states, |state| {
        matches!(state, ConnectionState::Recovering { .. })
    })
    .await;

    assert!(matches!(
        recovering,
        ConnectionState::Recovering {
            attempt: 1,
            retry_in,
            ..
        } if retry_in == Duration::from_millis(100)
    ));
}

#[tokio::test(start_paused = true)]
async fn close_interrupts_an_active_backoff() {
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Err(TransportError::connection("offline")));
    let actor = ConnectionActor::spawn(transport, broker(), RecoveryPolicy::default());
    let mut states = actor.subscribe();
    actor.start().await.expect("start command");
    wait_for_actor(&mut states, |state| {
        matches!(state, ConnectionState::Recovering { .. })
    })
    .await;

    actor.close().await.expect("close during backoff");

    assert_eq!(*states.borrow(), ConnectionState::Closed);
}

#[tokio::test(start_paused = true)]
async fn generation_increments_after_successful_recovery() {
    let transport = Arc::new(MockTransport::default());
    let actor = ConnectionActor::spawn_with_dependencies(
        transport,
        broker(),
        RecoveryPolicy::default(),
        Arc::new(RecordingClock::default()),
        Arc::new(IdentityJitter),
    );
    let mut states = actor.subscribe();
    actor.start().await.expect("start command");
    wait_for_actor(&mut states, |state| {
        matches!(state, ConnectionState::Ready { generation: 1 })
    })
    .await;

    actor
        .connection_lost(TransportError::connection("socket reset"))
        .await
        .expect("loss command");
    wait_for_actor(&mut states, |state| {
        matches!(state, ConnectionState::Recovering { .. })
    })
    .await;
    tokio::time::advance(Duration::from_millis(100)).await;
    let ready = wait_for_actor(&mut states, |state| {
        matches!(state, ConnectionState::Ready { generation: 2 })
    })
    .await;

    assert_eq!(ready, ConnectionState::Ready { generation: 2 });
}
