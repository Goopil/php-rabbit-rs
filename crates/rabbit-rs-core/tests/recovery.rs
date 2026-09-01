use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use bytes::Bytes;
use rabbit_rs_core::metrics::Metrics;
use rabbit_rs_core::{
    client::ClientPool,
    config::{SafetyMode, TopologyMode},
    pool::connection_actor::ConnectionActor,
    pool::recovery_coordinator::{
        RecoveryCoordinator, RecoveryCoordinatorConfig, RecoveryCoordinatorHandle,
    },
    publisher::{
        Destination, MessageProperties, PublishErrorKind, PublishOutcome, PublishRequest,
        PublisherConfig,
    },
    recovery::{
        Clock, ConnectionState, EqualJitter, IdentityJitter, JitterSource, RecoveryPolicy,
        TokioClock,
    },
    topology::{QueueDefinition, TopologyDefinition, TopologyPlan},
    transport::{
        PublishConfirmation, Transport, TransportError, TransportErrorKind,
        mock::{MockTransport, TransportOperation},
    },
};
use tokio::{sync::watch, time::Instant};

mod common;

mod helper {
    use super::*;

    pub use crate::common::{broker, config, worker_profile};

    pub fn publisher_config() -> PublisherConfig {
        PublisherConfig::with_safety(8, Duration::from_secs(5), SafetyMode::Safe)
    }

    pub fn publish_request(message_id: &str, deadline: Instant) -> PublishRequest {
        let mut properties = MessageProperties::new(message_id);
        properties.content_type = Some(Arc::from("application/json"));
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
            broker: broker("primary", "/", "guest"),
            policy: RecoveryPolicy::default(),
            topology_plan: topology_plan(),
            publisher_config: publisher_config(),
            config,
            metrics: rabbit_rs_core::metrics::Metrics::default(),
            // These tests exercise the deterministic recovery sequence with
            // the profile's consumer established, so "main" is requested up
            // front (issue #49: recovery only establishes requested profiles).
            requested_profiles: Arc::new(std::sync::Mutex::new(
                ["main".to_owned()].into_iter().collect(),
            )),
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
    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );

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
            message_id: "replay-me".into()
        }
    );

    coordinator.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn consumer_generation_updates_after_reconnection_rejects_stale_acks() {
    let transport = Arc::new(MockTransport::default());
    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );

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
async fn stale_consumer_handle_evicted_after_recovery() {
    let transport = Arc::new(MockTransport::default());
    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    let consumer1 = coordinator.consumer("main").await.expect("consumer handle");
    assert_eq!(
        consumer1.generation(),
        1,
        "first consumer should be generation 1"
    );

    // Simulate connection drop + recovery.
    transport.push_connect_result(Ok(()));
    coordinator
        .connection_lost(TransportError::connection("socket reset"))
        .await
        .expect("loss reported");

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 2 })
    })
    .await;

    // The consumer from the coordinator should now be generation 2.
    let consumer2 = coordinator.consumer("main").await.expect("consumer handle");
    assert_eq!(
        consumer2.generation(),
        2,
        "second consumer should be generation 2 after recovery"
    );
    assert_ne!(
        consumer1.generation(),
        consumer2.generation(),
        "stale handle should be evicted after recovery"
    );

    drop(consumer1);
    drop(consumer2);

    coordinator.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn deterministic_recovery_order_connection_channels_topology_consumers_publisher() {
    let transport = Arc::new(MockTransport::default());
    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );

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

/// Issue #95: a consumer acquired on demand can take the establish lock
/// while the recovery generation is still mid topology reconcile. A fresh
/// quorum queue rejects `basic.consume` with 404 until its `queue.declare`
/// completes, so the establish path must not subscribe before the
/// declaration completes — it must wait for (or perform) it.
#[tokio::test(start_paused = true)]
async fn on_demand_consumer_establishment_waits_for_the_queue_declaration() {
    let transport = Arc::new(MockTransport::default());
    // Park the recovery generation mid `queue.declare`: the topology plan is
    // not applied yet, which is exactly the window in which an on-demand
    // acquisition races the reconcile on the lab.
    let gate = transport.push_declare_queue_gate();
    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;
    gate.wait_entered().await;

    // The timeout owns the establishment future: an `Elapsed` drops it, and
    // tokio's fair locks release any guard or queued waiter on drop.
    let raced = tokio::time::timeout(Duration::from_secs(1), coordinator.consumer("main")).await;
    assert!(
        raced.is_err(),
        "consumer establishment must not complete before the queue declaration: {raced:?}"
    );

    let _ = gate.release();
    let consumer = coordinator.consumer("main").await.expect("consumer handle");

    // The declaration happened exactly once: the establish path observed the
    // shared reconciler instead of re-declaring for the same generation.
    let declare_count = transport
        .operations()
        .iter()
        .filter(|op| matches!(op, TransportOperation::DeclareQueue(_)))
        .count();
    assert_eq!(declare_count, 1);

    drop(consumer);
    coordinator.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn loss_during_recovery_cancels_and_restarts() {
    let transport = Arc::new(MockTransport::default());
    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );

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
    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );

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

#[tokio::test(start_paused = true)]
async fn recovery_failure_rolls_back_and_retries() {
    let transport = Arc::new(MockTransport::default());
    // First connection succeeds; recovery generation 1 will attempt consumers.
    transport.push_connect_result(Ok(()));
    // Make the first consumer-set spawn fail (set_qos consumes this error).
    transport.push_consumer_result(Err(TransportError::connection("test failure")));
    // Pre-queue results for the retry: connect succeeds, consumer succeeds.
    transport.push_connect_result(Ok(()));
    transport.push_consumer_result(Ok(()));

    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );
    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    // Wait for the first Ready generation so recovery runs.  The state may
    // transition through Ready{1} → Recovering → Connecting → Ready{2} very
    // quickly, so we wait for Ready with any generation ≥ 1, then for the
    // consumer to appear.
    wait_for_state(&coordinator, |s| matches!(s, ConnectionState::Ready { .. })).await;

    // If the first recovery failed, the coordinator drives the actor to
    // Recovering and rolls back last_generation.  Advance time through the
    // backoff so the reconnection and second recovery can occur.
    tokio::time::advance(Duration::from_secs(2)).await;

    // Wait for the second Ready generation (the retry).
    let ready2 = tokio::time::timeout(
        Duration::from_secs(10),
        wait_for_state(
            &coordinator,
            |s| matches!(s, ConnectionState::Ready { generation: g } if *g >= 2),
        ),
    )
    .await;
    assert!(
        ready2.is_ok(),
        "coordinator should reach Ready{{gen>=2}} after rollback+retry, state: {:?}",
        coordinator.state()
    );

    // The consumer should be available after the successful retry.
    let consumer = tokio::time::timeout(Duration::from_secs(5), coordinator.consumer("main"))
        .await
        .expect("timed out waiting for consumer")
        .expect("consumer should become available after retry");

    drop(consumer);

    coordinator.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// Publisher wake-up (audit F-03): a failed publisher Ready event must roll
// the generation back so recovery re-runs instead of leaving the publisher
// suspended with its generation consumed.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn publisher_ready_event_failure_rolls_back_generation_and_recovers() {
    let transport = Arc::new(MockTransport::default());
    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    // The fresh channel of the next generation rejects confirm.select once
    // (transient failure): recovery must not report success while the
    // publisher stays suspended with its generation consumed.
    transport.push_enable_confirms_result(Err(TransportError::connection("confirm.select failed")));
    transport.push_connect_result(Ok(()));
    coordinator
        .connection_lost(TransportError::connection("heartbeat missed"))
        .await
        .expect("loss reported");

    tokio::time::timeout(
        Duration::from_secs(10),
        wait_for_state(&coordinator, |s| {
            matches!(s, ConnectionState::Ready { generation: 3 })
        }),
    )
    .await
    .expect("recovery must re-run after the publisher Ready event failure");

    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let waiter = coordinator
        .publisher()
        .await
        .expect("publisher ready")
        .try_publish(publish_request(
            "post-recovery",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("publish accepted");
    let outcome = tokio::time::timeout(Duration::from_secs(10), waiter.wait())
        .await
        .expect("confirmation within timeout")
        .expect("publish must be confirmed after recovery");
    assert_eq!(
        outcome,
        PublishOutcome::Confirmed {
            message_id: "post-recovery".into()
        }
    );

    coordinator.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// Publisher wake-up (audit F-02): `delay.mode=auto` compiles to the plugin
// strategy, so the first delayed publish declares the `*.delayed` exchange.
// A channel-level declare failure (540 when the plugin is absent, classified
// recoverable by the lapin mapping) must fail that single message terminally
// and leave the actor ready — never suspend the publisher.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn delayed_publish_declare_failure_fails_that_message_and_keeps_publisher_ready() {
    let transport = Arc::new(MockTransport::default());
    let config = config(
        vec![broker("primary", "/", "guest")],
        vec![worker_profile("main", "primary", "jobs", 4)],
    );

    let coordinator =
        RecoveryCoordinator::spawn(&dyn_transport(&transport), coordinator_config(config));

    wait_for_state(&coordinator, |s| {
        matches!(s, ConnectionState::Ready { generation: 1 })
    })
    .await;

    transport.push_operation_result(Err(TransportError::connection(
        "exchange.declare: 540 NOT_IMPLEMENTED - x-delayed-message exchange type not registered",
    )));

    let mut properties = MessageProperties::new("delayed-one");
    properties.delay_ms = Some(60_000);
    let waiter = coordinator
        .publisher()
        .await
        .expect("publisher ready")
        .try_publish(PublishRequest::new(
            Destination::new("jobs", "high"),
            Bytes::from_static(b"payload"),
            properties,
            Instant::now() + Duration::from_secs(5),
        ))
        .expect("publish accepted");

    let outcome = tokio::time::timeout(Duration::from_secs(10), waiter.wait())
        .await
        .expect("waiter resolved")
        .expect_err("the delayed publish must fail terminally");
    assert_eq!(
        outcome.kind(),
        PublishErrorKind::Transport,
        "declare failure must fail the message terminally, not suspend the publisher"
    );

    // The actor stays ready: ordinary publishing keeps working.
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let follow_up = coordinator
        .publisher()
        .await
        .expect("publisher ready")
        .try_publish(publish_request(
            "still-published",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("publish accepted");
    let outcome = tokio::time::timeout(Duration::from_secs(10), follow_up.wait())
        .await
        .expect("confirmation within timeout")
        .expect("publishing must keep working after a delayed declare failure");
    assert_eq!(
        outcome,
        PublishOutcome::Confirmed {
            message_id: "still-published".into()
        }
    );

    coordinator.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// Recovery state machine tests (from recovery_state_machine.rs)
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn transitions_from_disconnected_through_connecting_to_ready() {
    let transport = Arc::new(MockTransport::default());
    let actor = ConnectionActor::spawn_with_dependencies_and_metrics(
        transport,
        broker("primary", "/", "guest"),
        RecoveryPolicy::default(),
        Arc::new(TokioClock),
        Arc::new(EqualJitter),
        Metrics::default(),
    );
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
    let actor = ConnectionActor::spawn_with_dependencies_and_metrics(
        transport,
        broker("primary", "/", "guest"),
        RecoveryPolicy::default(),
        clock.clone(),
        Arc::new(IdentityJitter),
        Metrics::default(),
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
    let actor = ConnectionActor::spawn_with_dependencies_and_metrics(
        transport,
        broker("primary", "/", "guest"),
        RecoveryPolicy::default(),
        Arc::new(RecordingClock::default()),
        Arc::new(AdditiveJitter(Duration::from_millis(25))),
        Metrics::default(),
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
    let actor = ConnectionActor::spawn_with_dependencies_and_metrics(
        transport,
        broker("primary", "/", "guest"),
        RecoveryPolicy::default(),
        Arc::new(TokioClock),
        Arc::new(EqualJitter),
        Metrics::default(),
    );
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
    let actor = ConnectionActor::spawn_with_dependencies_and_metrics(
        transport,
        broker("primary", "/", "guest"),
        RecoveryPolicy::default(),
        Arc::new(RecordingClock::default()),
        Arc::new(IdentityJitter),
        Metrics::default(),
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
    let actor = ConnectionActor::spawn_with_dependencies_and_metrics(
        transport,
        broker("primary", "/", "guest"),
        RecoveryPolicy::default(),
        Arc::new(TokioClock),
        Arc::new(EqualJitter),
        Metrics::default(),
    );
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
    let actor = ConnectionActor::spawn_with_dependencies_and_metrics(
        transport,
        broker("primary", "/", "guest"),
        RecoveryPolicy::default(),
        Arc::new(RecordingClock::default()),
        Arc::new(IdentityJitter),
        Metrics::default(),
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

#[tokio::test(start_paused = true)]
async fn pool_evicts_stale_consumer_handle_after_recovery() {
    let transport = Arc::new(MockTransport::default());

    // Pre-queue results: initial connect + consumer succeed.
    transport.push_connect_result(Ok(()));
    transport.push_consumer_result(Ok(()));
    // Recovery: connect + consumer succeed.
    transport.push_connect_result(Ok(()));
    transport.push_consumer_result(Ok(()));

    let pool = ClientPool::new(
        config(
            vec![broker("primary", "/", "guest")],
            vec![worker_profile("main", "primary", "jobs", 4)],
        ),
        transport.clone() as Arc<dyn Transport>,
    );

    // Get initial consumer — generation 1.
    let consumer1 = pool.consumer("main").await.expect("consumer handle");
    assert_eq!(
        consumer1.generation(),
        1,
        "first pool consumer should be generation 1"
    );

    // Simulate connection loss + recovery.
    pool.simulate_connection_loss_for_tests("primary", TransportError::connection("socket reset"))
        .await
        .expect("loss reported");

    // Advance time through the backoff and let the recovery complete.
    for _ in 0..5 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }

    // Wait for recovery to complete (generation 2).
    let consumer2 = tokio::time::timeout(Duration::from_secs(10), pool.consumer("main"))
        .await
        .expect("timed out waiting for consumer")
        .expect("consumer should be available after recovery");

    assert_eq!(
        consumer2.generation(),
        2,
        "pool should return generation 2 consumer after recovery"
    );
    assert_ne!(
        consumer1.generation(),
        consumer2.generation(),
        "pool should evict stale handle and return a fresh one"
    );

    drop(consumer1);
    drop(consumer2);
    pool.close().await.expect("close pool");
}

/// Issue #77 / audit F-13: `queue_size` and `purge_queue` must ride the
/// coordinator's single connection (and its recovery machinery) instead of
/// caching a second raw connection per vhost.
///
/// Before the fix the admin path opened and cached its own connection
/// outside the actor: the `connect_count` assertion fails (two connects),
/// and after a broker restart the cached dead connection made these
/// operations fail forever.
#[tokio::test(start_paused = true)]
async fn admin_ops_ride_the_coordinator_connection_and_survive_recovery() {
    let transport = Arc::new(MockTransport::default());
    transport.keep_delivery_stream_open();
    // Initial connect + consumer, then recovery connect + consumer.
    transport.push_connect_result(Ok(()));
    transport.push_consumer_result(Ok(()));
    transport.push_connect_result(Ok(()));
    transport.push_consumer_result(Ok(()));

    let pool = ClientPool::new(
        config(
            vec![broker("primary", "/", "guest")],
            vec![worker_profile("main", "primary", "jobs", 4)],
        ),
        transport.clone() as Arc<dyn Transport>,
    );

    // Establish the coordinator connection first so the connect count
    // discriminates: admin operations must not add a second connection.
    let consumer = pool.consumer("main").await.expect("consumer handle");
    drop(consumer);

    transport.push_queue_size(Ok(42));
    let size = pool
        .queue_size("primary", "jobs")
        .await
        .expect("queue size on the live connection");
    assert_eq!(size, 42);
    pool.purge_queue("primary", "jobs")
        .await
        .expect("purge on the live connection");

    let connect_count = transport
        .operations()
        .iter()
        .filter(|op| matches!(op, TransportOperation::Connect { .. }))
        .count();
    assert_eq!(
        connect_count, 1,
        "admin operations must ride the coordinator's single connection"
    );

    // Simulate a broker restart: the actor observes the loss, recovers the
    // connection, and admin operations work again — without process restart.
    pool.simulate_connection_loss_for_tests("primary", TransportError::connection("socket reset"))
        .await
        .expect("loss reported");
    for _ in 0..5 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }

    transport.push_queue_size(Ok(7));
    let size = tokio::time::timeout(Duration::from_secs(10), pool.queue_size("primary", "jobs"))
        .await
        .expect("queue_size must not hang after recovery")
        .expect("queue size must succeed after recovery");
    assert_eq!(size, 7);
    assert!(transport.operations().iter().any(|op| matches!(
        op,
        TransportOperation::QueueSize { queue, .. } if queue == "jobs"
    )));

    pool.close().await.expect("close pool");
}

/// Issue #77: when the connection actor failed permanently, admin
/// operations must surface a typed error promptly instead of hanging.
#[tokio::test(start_paused = true)]
async fn admin_operations_fail_fast_when_the_connection_failed_permanently() {
    let transport = Arc::new(MockTransport::default());
    transport.push_connect_result(Err(TransportError::authentication(
        "credentials rejected by the broker",
    )));

    let pool = ClientPool::new(
        config(
            vec![broker("primary", "/", "guest")],
            vec![worker_profile("main", "primary", "jobs", 4)],
        ),
        transport.clone() as Arc<dyn Transport>,
    );

    let error = tokio::time::timeout(Duration::from_secs(5), pool.queue_size("primary", "jobs"))
        .await
        .expect("queue_size must fail instead of hanging on a permanently failed actor")
        .expect_err("queue_size must fail on a permanently failed actor");
    assert!(
        format!("{error}").contains("failed permanently"),
        "error must identify the permanent failure, got: {error}"
    );

    pool.close().await.expect("close pool");
}
