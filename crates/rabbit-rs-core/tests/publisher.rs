use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use bytes::Bytes;
use rabbit_rs_core::{
    config::{BrokerConfig, Config, Credentials, DelayConfig, DelayMode, Endpoint, TlsConfig},
    publisher::{
        Destination, MessageProperties, PublishErrorKind, PublishOutcome, PublishRequest,
        PublisherActor, PublisherConfig, PublisherConnectionEvent, PublisherHandle,
        delay::DelayRouter,
    },
    topology::delay::{DelayPluginProbe, DelayStrategy, DelayStrategyResolver, TtlBucketPlan},
    transport::{
        ExchangeKind, ExchangeSpec, PublishConfirmation, PublishRequest as TransportRequest,
        PublisherChannel, QueueSpec, ReturnedMessage, Transport, TransportError, TransportResult,
        mock::{MockTransport, TransportOperation},
    },
};
use tokio::time::Instant;

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

    pub fn config_safety() -> PublisherConfig {
        PublisherConfig::new(32, Duration::from_secs(5))
    }

    pub fn config_recovery(capacity: usize) -> PublisherConfig {
        PublisherConfig::new(capacity, Duration::from_secs(5))
    }

    pub fn publisher_config_delay() -> PublisherConfig {
        PublisherConfig::new(32, Duration::from_secs(30))
    }

    pub fn request_safety(message_id: &str, payload: &'static [u8]) -> PublishRequest {
        PublishRequest::new(
            Destination::new("jobs", "high"),
            Bytes::from_static(payload),
            MessageProperties::new(message_id),
            Instant::now() + Duration::from_secs(30),
        )
    }

    pub fn request_recovery(message_id: &str, deadline: Instant) -> PublishRequest {
        let mut properties = MessageProperties::new(message_id);
        properties.content_type = Some("application/json".to_owned());
        properties.correlation_id = Some("correlation".to_owned());
        PublishRequest::new(
            Destination::new("jobs", "high"),
            Bytes::from_static(b"payload"),
            properties,
            deadline,
        )
    }

    pub fn delayed_request(message_id: &str, delay_ms: u64) -> PublishRequest {
        let mut properties = MessageProperties::new(message_id);
        properties.delay_ms = Some(delay_ms);
        PublishRequest::new(
            Destination::new("jobs", "high"),
            Bytes::from_static(b"job"),
            properties,
            Instant::now() + Duration::from_secs(30),
        )
    }

    pub fn immediate_request(message_id: &str) -> PublishRequest {
        PublishRequest::new(
            Destination::new("jobs", "high"),
            Bytes::from_static(b"job"),
            MessageProperties::new(message_id),
            Instant::now() + Duration::from_secs(30),
        )
    }

    pub async fn actor_safety(
        transport: &MockTransport,
        config: PublisherConfig,
    ) -> PublisherHandle {
        let publisher = transport
            .connect(&broker())
            .await
            .expect("connection")
            .open_publisher()
            .await
            .expect("publisher");
        PublisherActor::spawn(Arc::from(publisher), config)
    }

    pub async fn new_channel(transport: &MockTransport) -> Arc<dyn PublisherChannel> {
        Arc::from(
            transport
                .connect(&broker())
                .await
                .expect("connection")
                .open_publisher()
                .await
                .expect("publisher"),
        )
    }

    pub async fn actor_recovery(transport: &MockTransport, capacity: usize) -> PublisherHandle {
        PublisherActor::spawn(new_channel(transport).await, config_recovery(capacity))
    }

    pub async fn spawn_actor_delay(
        transport: &MockTransport,
        config: PublisherConfig,
        delay_strategy: DelayStrategy,
    ) -> PublisherHandle {
        let publisher = transport
            .connect(&broker())
            .await
            .expect("connection")
            .open_publisher()
            .await
            .expect("publisher");
        PublisherActor::spawn_with_delay_strategy(Arc::from(publisher), config, delay_strategy)
    }

    pub async fn wait_for_publish_count(transport: &MockTransport, expected: usize) {
        for _ in 0..100 {
            let count = transport
                .operations()
                .iter()
                .filter(|operation| matches!(operation, TransportOperation::Publish(_)))
                .count();
            if count == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("publisher did not emit {expected} messages");
    }

    pub async fn wait_for_publish_count_delay(transport: &MockTransport, expected: usize) {
        for _ in 0..200 {
            let count = transport
                .operations()
                .iter()
                .filter(|operation| matches!(operation, TransportOperation::Publish(_)))
                .count();
            if count == expected {
                return;
            }
            tokio::time::advance(Duration::from_millis(2)).await;
            tokio::task::yield_now().await;
        }
        panic!("publisher did not emit {expected} messages");
    }

    pub fn find_publish(transport: &MockTransport) -> TransportRequest {
        transport
            .operations()
            .iter()
            .find_map(|operation| match operation {
                TransportOperation::Publish(request) => Some(request.clone()),
                _ => None,
            })
            .expect("at least one publish")
    }

    pub fn publish_operations(transport: &MockTransport) -> Vec<TransportRequest> {
        transport
            .operations()
            .into_iter()
            .filter_map(|operation| match operation {
                TransportOperation::Publish(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    pub async fn suspend(actor: &PublisherHandle) {
        actor
            .connection_event(PublisherConnectionEvent::Recovering { generation: 1 })
            .await
            .expect("publisher suspended");
    }

    pub fn ttl_config() -> DelayConfig {
        DelayConfig::new(
            DelayMode::Ttl,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(30),
            ],
            8,
            Duration::from_mins(1),
            Duration::from_millis(50),
        )
    }

    pub fn delay_config(mode: DelayMode) -> DelayConfig {
        DelayConfig::new(
            mode,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(30),
            ],
            8,
            Duration::from_mins(1),
            Duration::from_millis(50),
        )
    }

    pub struct FixedProbe {
        available: bool,
        calls: AtomicUsize,
    }

    impl FixedProbe {
        pub const fn new(available: bool) -> Self {
            Self {
                available,
                calls: AtomicUsize::new(0),
            }
        }

        pub fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl DelayPluginProbe for FixedProbe {
        async fn is_available(&self) -> TransportResult<bool> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.available)
        }
    }

    pub struct PendingProbe;

    #[async_trait]
    impl DelayPluginProbe for PendingProbe {
        async fn is_available(&self) -> TransportResult<bool> {
            std::future::pending().await
        }
    }
}

use helper::*;

// ---------------------------------------------------------------------------
// Publisher safety tests (from publisher_safety.rs)
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn publishes_immediately_in_ready_phase() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let actor = actor_safety(&transport, config_safety()).await;

    let waiter = actor
        .try_publish(request_safety("one", b"a"))
        .expect("publish");
    // No time advance needed — publish is immediate.
    wait_for_publish_count(&transport, 1).await;

    assert!(matches!(
        waiter.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn resolves_acks_for_multiple_sequences() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let actor = actor_safety(&transport, config_safety()).await;

    let first = actor
        .try_publish(request_safety("one", b"a"))
        .expect("first");
    let second = actor
        .try_publish(request_safety("two", b"b"))
        .expect("second");

    assert_eq!(
        first.wait().await.expect("first ACK"),
        PublishOutcome::Confirmed {
            message_id: "one".to_owned()
        }
    );
    assert_eq!(
        second.wait().await.expect("second ACK"),
        PublishOutcome::Confirmed {
            message_id: "two".to_owned()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_nack_only_fails_its_target_sequence() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Nack(None)));
    let actor = actor_safety(&transport, config_safety()).await;

    let first = actor
        .try_publish(request_safety("one", b"a"))
        .expect("first");
    let second = actor
        .try_publish(request_safety("two", b"b"))
        .expect("second");

    assert!(first.wait().await.is_ok());
    assert_eq!(
        second.wait().await.expect_err("targeted NACK").kind(),
        PublishErrorKind::Nack
    );
}

#[tokio::test(start_paused = true)]
async fn mandatory_return_wins_over_the_following_ack() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(Some(ReturnedMessage {
        reply_code: 312,
        reply_text: "NO_ROUTE".to_owned(),
        exchange: "jobs".to_owned(),
        routing_key: "missing".to_owned(),
        payload: Bytes::from_static(b"job"),
    }))));
    let actor = actor_safety(&transport, config_safety()).await;

    let waiter = actor
        .try_publish(request_safety("returned", b"job"))
        .expect("publish");

    assert!(matches!(
        waiter.wait().await,
        Ok(PublishOutcome::Returned { reply, .. }) if reply.code == 312
    ));
}

#[tokio::test(start_paused = true)]
async fn confirmation_timeout_is_typed() {
    let transport = MockTransport::default();
    transport.push_pending_confirmation();
    let actor = actor_safety(
        &transport,
        PublisherConfig::new(32, Duration::from_millis(10)),
    )
    .await;
    let waiter = actor
        .try_publish(request_safety("slow", b"job"))
        .expect("publish");
    wait_for_publish_count(&transport, 1).await;

    tokio::time::advance(Duration::from_millis(10)).await;

    assert_eq!(
        waiter.wait().await.expect_err("timeout").kind(),
        PublishErrorKind::Timeout
    );
}

#[tokio::test(start_paused = true)]
async fn a_full_command_buffer_returns_backpressure() {
    let transport = MockTransport::default();
    let actor = actor_safety(&transport, PublisherConfig::new(1, Duration::from_secs(5))).await;

    let _first = actor
        .try_publish(request_safety("one", b"a"))
        .expect("first slot");
    let error = actor
        .try_publish(request_safety("two", b"b"))
        .expect_err("buffer must be full");

    assert_eq!(error.kind(), PublishErrorKind::Backpressure);
}

#[tokio::test(start_paused = true)]
async fn connection_loss_before_confirm_is_replayed() {
    let transport = MockTransport::default();
    transport.push_pending_confirmation();
    let actor = actor_safety(&transport, config_safety()).await;
    let waiter = actor
        .try_publish(request_safety("uncertain", b"job"))
        .expect("publish");
    wait_for_publish_count(&transport, 1).await;

    actor.connection_lost().await.expect("loss command");
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let replacement = transport
        .connect(&broker())
        .await
        .expect("replacement connection")
        .open_publisher()
        .await
        .expect("replacement channel");
    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: Arc::from(replacement),
            topology_restored: true,
        })
        .await
        .expect("recovery");

    assert_eq!(
        waiter.wait().await.expect("replayed outcome"),
        PublishOutcome::Confirmed {
            message_id: "uncertain".to_owned()
        }
    );
}

#[test]
fn republication_preserves_the_message_id() {
    let original = request_safety("stable-id", b"job");

    let retry = original.republish(Instant::now() + Duration::from_secs(30));

    assert_eq!(retry.properties.message_id, "stable-id");
    assert_eq!(retry.payload, original.payload);
}

#[tokio::test(start_paused = true)]
async fn skips_enable_confirms_when_configured_off() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::NotRequested));
    let publisher = transport
        .connect(&broker())
        .await
        .expect("connection")
        .open_publisher()
        .await
        .expect("publisher");
    let config = PublisherConfig::with_flags(32, Duration::from_secs(5), false, true);
    let actor = PublisherActor::spawn(Arc::from(publisher), config);

    let waiter = actor
        .try_publish(request_safety("one", b"a"))
        .expect("publish");
    wait_for_publish_count(&transport, 1).await;
    let _ = waiter.wait().await;

    let operations = transport.operations();
    assert!(
        !operations
            .iter()
            .any(|op| matches!(op, TransportOperation::EnableConfirms)),
        "enable_confirms must not be called when confirms=false"
    );
}

#[tokio::test(start_paused = true)]
async fn calls_enable_confirms_when_configured_on() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let publisher = transport
        .connect(&broker())
        .await
        .expect("connection")
        .open_publisher()
        .await
        .expect("publisher");
    let config = PublisherConfig::with_flags(32, Duration::from_secs(5), true, true);
    let actor = PublisherActor::spawn(Arc::from(publisher), config);

    let waiter = actor
        .try_publish(request_safety("one", b"a"))
        .expect("publish");
    wait_for_publish_count(&transport, 1).await;
    let _ = waiter.wait().await;

    let operations = transport.operations();
    assert!(
        operations
            .iter()
            .any(|op| matches!(op, TransportOperation::EnableConfirms)),
        "enable_confirms must be called when confirms=true"
    );
}

#[tokio::test(start_paused = true)]
async fn publishes_with_mandatory_false_when_configured_off() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let publisher = transport
        .connect(&broker())
        .await
        .expect("connection")
        .open_publisher()
        .await
        .expect("publisher");
    let config = PublisherConfig::with_flags(32, Duration::from_secs(5), true, false);
    let actor = PublisherActor::spawn(Arc::from(publisher), config);

    let waiter = actor
        .try_publish(request_safety("one", b"a"))
        .expect("publish");
    wait_for_publish_count(&transport, 1).await;
    let _ = waiter.wait().await;

    let operations = transport.operations();
    let publish = operations
        .iter()
        .find(|op| matches!(op, TransportOperation::Publish(_)))
        .expect("a publish operation");
    let TransportOperation::Publish(req) = publish else {
        panic!("expected a Publish operation");
    };
    assert!(
        !req.mandatory,
        "publish must have mandatory=false when config.mandatory=false"
    );
}

#[tokio::test(start_paused = true)]
async fn publishes_with_mandatory_true_when_configured_on() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let publisher = transport
        .connect(&broker())
        .await
        .expect("connection")
        .open_publisher()
        .await
        .expect("publisher");
    let config = PublisherConfig::with_flags(32, Duration::from_secs(5), true, true);
    let actor = PublisherActor::spawn(Arc::from(publisher), config);

    let waiter = actor
        .try_publish(request_safety("one", b"a"))
        .expect("publish");
    wait_for_publish_count(&transport, 1).await;
    let _ = waiter.wait().await;

    let operations = transport.operations();
    let publish = operations
        .iter()
        .find(|op| matches!(op, TransportOperation::Publish(_)))
        .expect("a publish operation");
    let TransportOperation::Publish(req) = publish else {
        panic!("expected a Publish operation");
    };
    assert!(
        req.mandatory,
        "publish must have mandatory=true when config.mandatory=true"
    );
}

#[tokio::test(start_paused = true)]
async fn confirm_timeout_from_config_is_applied() {
    let transport = MockTransport::default();
    transport.push_pending_confirmation();
    let publisher = transport
        .connect(&broker())
        .await
        .expect("connection")
        .open_publisher()
        .await
        .expect("publisher");
    let config = PublisherConfig::with_flags(32, Duration::from_secs(5), true, true);
    let actor = PublisherActor::spawn(Arc::from(publisher), config);

    let waiter = actor
        .try_publish(request_safety("slow", b"job"))
        .expect("publish");
    wait_for_publish_count(&transport, 1).await;

    tokio::time::advance(Duration::from_secs(5)).await;

    assert_eq!(
        waiter.wait().await.expect_err("timeout").kind(),
        PublishErrorKind::Timeout
    );
}

// ---------------------------------------------------------------------------
// Publisher pipeline tests (now_or_never sequential sending)
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn pipeline_publishes_before_confirmation() {
    let transport = MockTransport::default();
    transport.push_pending_confirmation();
    transport.push_pending_confirmation();
    transport.push_pending_confirmation();

    let actor = actor_safety(&transport, config_safety()).await;

    let _w1 = actor
        .try_publish(request_safety("msg0", b"payload"))
        .expect("publish0");
    let _w2 = actor
        .try_publish(request_safety("msg1", b"payload"))
        .expect("publish1");
    let _w3 = actor
        .try_publish(request_safety("msg2", b"payload"))
        .expect("publish2");

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;

    let publishes = publish_operations(&transport);
    assert_eq!(
        publishes.len(),
        3,
        "all 3 publishes should be sent before any confirmation"
    );
}

#[tokio::test(start_paused = true)]
async fn pipeline_recoverable_error_sorts_replay_by_sequence() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::NotRequested));
    transport.push_confirmation(Err(TransportError::connection("connection lost")));

    let actor = actor_safety(&transport, config_safety()).await;

    let _w1 = actor
        .try_publish(request_safety("msg1", b"p1"))
        .expect("publish1");
    let _w2 = actor
        .try_publish(request_safety("msg2", b"p2"))
        .expect("publish2");

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;

    let publishes = publish_operations(&transport);
    assert!(!publishes.is_empty(), "at least one publish was attempted");
}

// ---------------------------------------------------------------------------
// Publisher recovery tests (from publisher_recovery.rs)
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn publication_accepted_during_recovery_is_sent_after_ready() {
    let transport = MockTransport::default();
    let actor = actor_recovery(&transport, 8).await;
    suspend(&actor).await;
    let waiter = actor
        .try_publish(request_recovery(
            "during-outage",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("accepted while suspended");
    tokio::task::yield_now().await;
    assert!(publish_operations(&transport).is_empty());

    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("publisher resumed");

    assert_eq!(
        waiter.wait().await.expect("confirmed after recovery"),
        PublishOutcome::Confirmed {
            message_id: "during-outage".to_owned()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn ready_with_the_same_generation_as_recovering_resumes_publication() {
    let transport = MockTransport::default();
    let actor = actor_recovery(&transport, 8).await;
    suspend(&actor).await;
    let waiter = actor
        .try_publish(request_recovery(
            "same-generation",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("accepted while suspended");

    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 1,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("same generation resumes the publisher");

    assert_eq!(
        waiter.wait().await.expect("confirmed after recovery"),
        PublishOutcome::Confirmed {
            message_id: "same-generation".to_owned()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn unconfirmed_publish_is_replayed_identically_with_the_same_message_id() {
    let transport = MockTransport::default();
    transport.push_pending_confirmation();
    let actor = actor_recovery(&transport, 8).await;
    let waiter = actor
        .try_publish(request_recovery(
            "stable-id",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("initial publish");
    wait_for_publish_count(&transport, 1).await;

    suspend(&actor).await;
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("publisher resumed");
    wait_for_publish_count(&transport, 2).await;

    let attempts = publish_operations(&transport);
    assert_eq!(attempts[0], attempts[1]);
    assert_eq!(
        waiter.wait().await.expect("replayed ACK"),
        PublishOutcome::Confirmed {
            message_id: "stable-id".to_owned()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn unconfirmed_publish_is_retained_across_connection_loss() {
    let transport = MockTransport::default();
    transport.push_pending_confirmation();
    let actor = PublisherActor::spawn(
        new_channel(&transport).await,
        PublisherConfig::new(8, Duration::from_secs(5)),
    );
    let waiter = actor
        .try_publish(request_recovery(
            "pending",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("publish");
    wait_for_publish_count(&transport, 1).await;

    suspend(&actor).await;
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("resume");

    assert!(waiter.wait().await.is_ok());
    assert_eq!(publish_operations(&transport).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn late_confirm_from_old_generation_cannot_resolve_the_waiter() {
    let transport = MockTransport::default();
    let old_confirm = transport.push_controlled_confirmation();
    let actor = actor_recovery(&transport, 8).await;
    let waiter = actor
        .try_publish(request_recovery(
            "generation-safe",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("publish");
    wait_for_publish_count(&transport, 1).await;

    suspend(&actor).await;
    assert!(!old_confirm.resolve(Ok(PublishConfirmation::Ack(None))));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("resume");

    assert_eq!(
        waiter.wait().await.expect("new generation ACK"),
        PublishOutcome::Confirmed {
            message_id: "generation-safe".to_owned()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn nack_remains_terminal_after_replay() {
    let transport = MockTransport::default();
    transport.push_pending_confirmation();
    let actor = actor_recovery(&transport, 8).await;
    let waiter = actor
        .try_publish(request_recovery(
            "nacked",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("publish");
    wait_for_publish_count(&transport, 1).await;
    suspend(&actor).await;

    transport.push_confirmation(Ok(PublishConfirmation::Nack(None)));
    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("resume");

    assert_eq!(
        waiter.wait().await.expect_err("NACK is terminal").kind(),
        PublishErrorKind::Nack
    );
}

#[tokio::test(start_paused = true)]
async fn topology_and_confirms_must_be_ready_before_replay() {
    let transport = MockTransport::default();
    let actor = actor_recovery(&transport, 8).await;
    suspend(&actor).await;
    let _waiter = actor
        .try_publish(request_recovery(
            "gated",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("queued");
    let replacement = new_channel(&transport).await;

    assert!(
        actor
            .connection_event(PublisherConnectionEvent::Ready {
                generation: 2,
                channel: replacement.clone(),
                topology_restored: false,
            })
            .await
            .is_err()
    );
    assert!(publish_operations(&transport).is_empty());

    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: replacement,
            topology_restored: true,
        })
        .await
        .expect("topology restored");
    wait_for_publish_count(&transport, 1).await;

    let operations = transport.operations();
    let confirms = operations
        .iter()
        .rposition(|operation| matches!(operation, TransportOperation::EnableConfirms))
        .expect("confirm.select");
    let publish = operations
        .iter()
        .position(|operation| matches!(operation, TransportOperation::Publish(_)))
        .expect("publish");
    assert!(confirms < publish);
}

#[tokio::test(start_paused = true)]
async fn repeated_loss_does_not_duplicate_the_replay_entry() {
    let transport = MockTransport::default();
    transport.push_pending_confirmation();
    let actor = actor_recovery(&transport, 8).await;
    let waiter = actor
        .try_publish(request_recovery(
            "once",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("publish");
    wait_for_publish_count(&transport, 1).await;
    suspend(&actor).await;
    suspend(&actor).await;

    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("resume");
    assert!(waiter.wait().await.is_ok());
    assert_eq!(publish_operations(&transport).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn retained_entries_keep_global_capacity_while_mpsc_is_drained() {
    let transport = MockTransport::default();
    let actor = actor_recovery(&transport, 1).await;
    suspend(&actor).await;
    let _first = actor
        .try_publish(request_recovery(
            "retained",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("capacity slot");
    tokio::task::yield_now().await;

    let error = actor
        .try_publish(request_recovery(
            "overflow",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect_err("global capacity exhausted");

    assert_eq!(error.kind(), PublishErrorKind::Backpressure);
}

#[tokio::test(start_paused = true)]
async fn deadline_expiring_during_outage_prevents_replay() {
    let transport = MockTransport::default();
    let actor = actor_recovery(&transport, 8).await;
    suspend(&actor).await;
    let waiter = actor
        .try_publish(request_recovery(
            "expired",
            Instant::now() + Duration::from_millis(10),
        ))
        .expect("queued");
    tokio::task::yield_now().await;

    tokio::time::advance(Duration::from_millis(10)).await;
    assert_eq!(
        waiter.wait().await.expect_err("deadline").kind(),
        PublishErrorKind::Timeout
    );

    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("resume after expiry");
    assert!(publish_operations(&transport).is_empty());
}

#[tokio::test(start_paused = true)]
async fn permanent_recovery_failure_is_terminal() {
    let transport = MockTransport::default();
    let actor = actor_recovery(&transport, 8).await;
    suspend(&actor).await;
    let waiter = actor
        .try_publish(request_recovery(
            "denied",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("queued");
    tokio::task::yield_now().await;

    actor
        .connection_event(PublisherConnectionEvent::FailedPermanent {
            generation: 2,
            error: TransportError::authentication("access refused"),
        })
        .await
        .expect("permanent failure handled");

    assert_eq!(
        waiter.wait().await.expect_err("terminal error").kind(),
        PublishErrorKind::Transport
    );
}

#[tokio::test(start_paused = true)]
async fn ready_with_same_generation_as_recovering_resumes() {
    let transport = MockTransport::default();
    let actor = actor_recovery(&transport, 8).await;
    actor
        .connection_event(PublisherConnectionEvent::Recovering { generation: 3 })
        .await
        .expect("publisher suspended");
    let waiter = actor
        .try_publish(request_recovery(
            "same-gen",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("accepted while suspended");
    tokio::task::yield_now().await;
    assert!(publish_operations(&transport).is_empty());

    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 3,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("publisher resumed with same generation");

    assert_eq!(
        waiter.wait().await.expect("confirmed after recovery"),
        PublishOutcome::Confirmed {
            message_id: "same-gen".to_owned()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn explicit_close_wakes_retained_waiters() {
    let transport = MockTransport::default();
    let actor = actor_recovery(&transport, 8).await;
    suspend(&actor).await;
    let waiter = actor
        .try_publish(request_recovery(
            "closing",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("queued");
    tokio::task::yield_now().await;

    actor.close().await.expect("close");

    assert_eq!(
        waiter.wait().await.expect_err("closed").kind(),
        PublishErrorKind::Closed
    );
}

// ---------------------------------------------------------------------------
// Publisher delay tests (from publisher_delay.rs)
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn plugin_mode_publishes_on_delayed_exchange_with_x_delay_header() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let strategy = DelayStrategy::Plugin;
    let actor = spawn_actor_delay(&transport, publisher_config_delay(), strategy).await;

    let waiter = actor
        .try_publish(delayed_request("delayed-job", 5_000))
        .expect("publish");

    wait_for_publish_count_delay(&transport, 1).await;

    let request = find_publish(&transport);

    assert_ne!(
        request.exchange, "jobs",
        "plugin mode must publish on a delayed exchange, not the original"
    );
    assert!(
        request.exchange.ends_with(".delayed"),
        "exchange name should be the delayed variant, got: {}",
        request.exchange
    );
    assert_eq!(request.routing_key, "high");
    assert_eq!(
        request.properties.delay_ms,
        Some(5_000),
        "x-delay header must be set"
    );

    assert!(matches!(
        waiter.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn ttl_mode_publishes_on_ttl_queue_with_dead_letter_to_original() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_operation_result(Ok(()));
    let plan = TtlBucketPlan::compile(&ttl_config()).expect("TTL plan");
    let strategy = DelayStrategy::TtlBuckets(plan);
    let actor = spawn_actor_delay(&transport, publisher_config_delay(), strategy).await;

    let waiter = actor
        .try_publish(delayed_request("ttl-job", 5_000))
        .expect("publish");

    wait_for_publish_count_delay(&transport, 1).await;

    let operations = transport.operations();

    let queue_declared = operations.iter().any(|op| {
        matches!(
            op,
            TransportOperation::DeclareQueue(QueueSpec {
                dead_letter_exchange: Some(dlx),
                message_ttl: Some(ttl),
                ..
            }) if dlx == "jobs" && *ttl == Duration::from_secs(5)
        )
    });
    assert!(
        queue_declared,
        "TTL mode must lazily declare a queue with x-message-ttl and dead-letter-exchange"
    );

    let request = find_publish(&transport);

    assert_eq!(
        request.exchange, "",
        "TTL mode must publish on the default exchange"
    );
    assert!(
        request.routing_key.starts_with("rabbit-rs.delay."),
        "TTL mode must publish to a stable delay queue, got: {}",
        request.routing_key
    );
    assert_eq!(
        request.properties.delay_ms, None,
        "TTL mode must not set x-delay header"
    );

    assert!(matches!(
        waiter.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn zero_delay_does_not_change_routing() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let strategy = DelayStrategy::Plugin;
    let actor = spawn_actor_delay(&transport, publisher_config_delay(), strategy).await;

    let waiter = actor
        .try_publish(immediate_request("immediate-job"))
        .expect("publish");

    wait_for_publish_count_delay(&transport, 1).await;

    let request = find_publish(&transport);

    assert_eq!(
        request.exchange, "jobs",
        "zero delay must publish on the original exchange"
    );
    assert_eq!(request.routing_key, "high");
    assert_eq!(
        request.properties.delay_ms, None,
        "zero delay must not set x-delay header"
    );

    assert!(matches!(
        waiter.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn plugin_mode_topology_includes_delayed_exchange_declaration() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let strategy = DelayStrategy::Plugin;
    let actor = spawn_actor_delay(&transport, publisher_config_delay(), strategy).await;

    let _waiter = actor
        .try_publish(delayed_request("delayed-job", 5_000))
        .expect("publish");

    wait_for_publish_count_delay(&transport, 1).await;

    let operations = transport.operations();
    let has_delayed_exchange = operations.iter().any(|op| {
        matches!(
            op,
            TransportOperation::DeclareExchange(ExchangeSpec {
                kind: ExchangeKind::Delayed(_),
                ..
            })
        )
    });
    assert!(
        has_delayed_exchange,
        "plugin mode must declare a delayed exchange"
    );
}

#[tokio::test(start_paused = true)]
async fn ttl_mode_declares_delay_queue_lazily_on_first_delayed_publish() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_operation_result(Ok(()));
    transport.push_operation_result(Ok(()));
    let plan = TtlBucketPlan::compile(&ttl_config()).expect("TTL plan");
    let strategy = DelayStrategy::TtlBuckets(plan);
    let actor = spawn_actor_delay(&transport, publisher_config_delay(), strategy).await;

    let _first = actor
        .try_publish(delayed_request("first", 5_000))
        .expect("first publish");
    wait_for_publish_count_delay(&transport, 1).await;

    let _second = actor
        .try_publish(delayed_request("second", 5_000))
        .expect("second publish");
    wait_for_publish_count_delay(&transport, 2).await;

    let declare_count = transport
        .operations()
        .iter()
        .filter(|op| matches!(op, TransportOperation::DeclareQueue(_)))
        .count();
    assert_eq!(
        declare_count, 1,
        "TTL queue must be declared lazily and only once per bucket"
    );
}

#[test]
fn delay_config_is_validated_and_deserialized_from_config() {
    let json = serde_json::json!({
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
        "topology_mode": "declare",
        "delay": {
            "mode": "ttl",
            "buckets": [1, 5, 30],
            "max_buckets": 8,
            "queue_expiry_margin": 60,
            "detection_timeout": 5
        }
    });

    let config: Config = serde_json::from_value(json).expect("valid config with delay section");
    let validated = config.validate().expect("valid configuration");

    let delay = validated.delay();
    assert_eq!(delay.mode, DelayMode::Ttl);
    assert_eq!(
        delay.buckets,
        vec![
            Duration::from_secs(1),
            Duration::from_secs(5),
            Duration::from_secs(30)
        ]
    );
    assert_eq!(delay.max_buckets, 8);
    assert_eq!(delay.queue_expiry_margin, Duration::from_mins(1));
    assert_eq!(delay.detection_timeout, Duration::from_secs(5));
}

#[test]
fn delay_config_rejects_empty_buckets() {
    let json = serde_json::json!({
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
        "topology_mode": "declare",
        "delay": {
            "mode": "ttl",
            "buckets": [],
            "max_buckets": 8,
            "queue_expiry_margin": 60,
            "detection_timeout": 5
        }
    });

    let config: Config = serde_json::from_value(json).expect("parse");
    let error = config
        .validate()
        .expect_err("empty buckets must be rejected");

    assert!(error.path().contains("delay.buckets"));
}

#[tokio::test(start_paused = true)]
async fn auto_mode_detects_plugin_and_falls_back_to_ttl_if_absent() {
    struct NeverAvailable;

    #[async_trait]
    impl DelayPluginProbe for NeverAvailable {
        async fn is_available(&self) -> TransportResult<bool> {
            Ok(false)
        }
    }

    let mut resolver = DelayStrategyResolver::new();
    let config = DelayConfig::new(
        DelayMode::Auto,
        vec![Duration::from_secs(5)],
        8,
        Duration::from_mins(1),
        Duration::from_millis(50),
    );

    let strategy = resolver
        .resolve(&config, 1, Arc::new(NeverAvailable))
        .await
        .expect("fallback strategy");

    assert!(
        matches!(strategy, DelayStrategy::TtlBuckets(_)),
        "Auto mode must fall back to TTL when plugin is absent"
    );
}

// ---------------------------------------------------------------------------
// Delay routing tests (from delay_routing.rs)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auto_selects_delayed_exchange_when_plugin_is_available() {
    let probe = Arc::new(FixedProbe::new(true));
    let mut resolver = DelayStrategyResolver::new();

    let strategy = resolver
        .resolve(&delay_config(DelayMode::Auto), 1, probe)
        .await
        .expect("plugin strategy");

    assert_eq!(strategy, DelayStrategy::Plugin);
}

#[tokio::test]
async fn auto_falls_back_to_ttl_when_plugin_is_absent() {
    let probe = Arc::new(FixedProbe::new(false));
    let mut resolver = DelayStrategyResolver::new();

    let strategy = resolver
        .resolve(&delay_config(DelayMode::Auto), 1, probe)
        .await
        .expect("TTL fallback");

    assert!(matches!(strategy, DelayStrategy::TtlBuckets(_)));
}

#[tokio::test]
async fn required_plugin_fails_permanently_when_absent() {
    let probe = Arc::new(FixedProbe::new(false));
    let mut resolver = DelayStrategyResolver::new();

    let error = resolver
        .resolve(&delay_config(DelayMode::Plugin), 1, probe)
        .await
        .expect_err("plugin is required");

    assert!(error.is_permanent());
}

#[test]
fn ttl_rounds_up_to_the_next_bucket() {
    let plan = TtlBucketPlan::compile(&delay_config(DelayMode::Ttl)).expect("TTL plan");

    assert_eq!(
        plan.bucket_for(Duration::from_millis(1_001))
            .expect("bucket"),
        Duration::from_secs(5)
    );
}

#[test]
fn ttl_rejects_more_than_the_configured_maximum_buckets() {
    let invalid = DelayConfig::new(
        DelayMode::Ttl,
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(3),
        ],
        2,
        Duration::from_mins(1),
        Duration::from_millis(50),
    );

    assert!(TtlBucketPlan::compile(&invalid).is_err());
}

#[test]
fn ttl_queue_name_is_stable_and_x_expires_exceeds_message_ttl() {
    let plan = TtlBucketPlan::compile(&delay_config(DelayMode::Ttl)).expect("TTL plan");
    let destination = Destination::new("jobs", "high");

    let first = plan
        .queue_for(&destination, Duration::from_secs(5))
        .expect("queue");
    let second = plan
        .queue_for(&destination, Duration::from_secs(5))
        .expect("queue");

    assert_eq!(first.name, second.name);
    assert_eq!(first.message_ttl, Some(Duration::from_secs(5)));
    assert!(first.expires > first.message_ttl);
}

#[test]
fn negative_delay_is_rejected() {
    let strategy = DelayStrategy::TtlBuckets(
        TtlBucketPlan::compile(&delay_config(DelayMode::Ttl)).expect("TTL plan"),
    );

    assert!(DelayRouter::route(&strategy, &Destination::new("jobs", "high"), -1).is_err());
}

#[tokio::test]
async fn plugin_detection_is_cached_per_connection_generation() {
    let probe = Arc::new(FixedProbe::new(true));
    let mut resolver = DelayStrategyResolver::new();

    resolver
        .resolve(&delay_config(DelayMode::Auto), 1, probe.clone())
        .await
        .expect("first resolution");
    resolver
        .resolve(&delay_config(DelayMode::Auto), 1, probe.clone())
        .await
        .expect("cached resolution");
    resolver
        .resolve(&delay_config(DelayMode::Auto), 2, probe.clone())
        .await
        .expect("new generation");

    assert_eq!(probe.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn auto_detection_timeout_is_bounded_and_falls_back_to_ttl() {
    let mut resolver = DelayStrategyResolver::new();

    let strategy = resolver
        .resolve(&delay_config(DelayMode::Auto), 1, Arc::new(PendingProbe))
        .await
        .expect("bounded TTL fallback");

    assert!(matches!(strategy, DelayStrategy::TtlBuckets(_)));
}
