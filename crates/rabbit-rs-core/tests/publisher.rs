use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    config::{
        BrokerConfig, Config, Credentials, DelayConfig, DelayMode, Endpoint, SafetyMode, TlsConfig,
    },
    metrics::Metrics,
    publisher::{
        Destination, MessageProperties, PublishErrorKind, PublishOutcome, PublishRequest,
        PublishWaiter, PublisherActor, PublisherConfig, PublisherConnectionEvent, PublisherHandle,
        delay::DelayRouter,
    },
    topology::delay::{DelayStrategy, TtlBucketPlan},
    transport::{
        ExchangeKind, ExchangeSpec, PublishConfirmation, PublishRequest as TransportRequest,
        PublisherChannel, QueueSpec, ReturnedMessage, Transport, TransportError,
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
        PublisherConfig::with_safety(32, Duration::from_secs(5), SafetyMode::Safe)
    }

    pub fn config_recovery(capacity: usize) -> PublisherConfig {
        PublisherConfig::with_safety(capacity, Duration::from_secs(5), SafetyMode::Safe)
    }

    pub fn publisher_config_delay() -> PublisherConfig {
        PublisherConfig::with_safety(32, Duration::from_secs(30), SafetyMode::Safe)
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
        properties.content_type = Some(Arc::from("application/json"));
        properties.correlation_id = Some(Arc::from("correlation"));
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
        PublisherActor::spawn_with_delay_strategy_and_metrics(
            Arc::from(publisher),
            config,
            Metrics::default(),
            None,
        )
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
        PublisherActor::spawn_with_delay_strategy_and_metrics(
            new_channel(transport).await,
            config_recovery(capacity),
            Metrics::default(),
            None,
        )
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
        PublisherActor::spawn_with_delay_strategy_and_metrics(
            Arc::from(publisher),
            config,
            Metrics::default(),
            Some(delay_strategy),
        )
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
        DelayConfig {
            mode: DelayMode::Ttl,
            buckets: vec![
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(30),
            ],
            max_buckets: 8,
            queue_expiry_margin: Duration::from_mins(1),
        }
    }

    pub fn delay_config(mode: DelayMode) -> DelayConfig {
        DelayConfig {
            mode,
            buckets: vec![
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(30),
            ],
            max_buckets: 8,
            queue_expiry_margin: Duration::from_mins(1),
        }
    }
}

use helper::*;

// ---------------------------------------------------------------------------
// TaggedFuture baseline test (Task 11)
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn tagged_future_publish_completes() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let actor = actor_safety(&transport, config_safety()).await;

    let waiter = actor
        .try_publish(request_safety("tagged-1", b"payload"))
        .expect("publish");
    wait_for_publish_count(&transport, 1).await;

    assert!(matches!(
        waiter.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));
}

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
            message_id: "one".into()
        }
    );
    assert_eq!(
        second.wait().await.expect("second ACK"),
        PublishOutcome::Confirmed {
            message_id: "two".into()
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
        PublisherConfig::with_safety(32, Duration::from_millis(10), SafetyMode::Safe),
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
    let actor = actor_safety(
        &transport,
        PublisherConfig::with_safety(1, Duration::from_secs(5), SafetyMode::Safe),
    )
    .await;

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
            message_id: "uncertain".into()
        }
    );
}

#[test]
fn republication_preserves_the_message_id() {
    let original = request_safety("stable-id", b"job");

    let retry = original.republish(Instant::now() + Duration::from_secs(30));

    assert_eq!(retry.properties.message_id.as_ref(), "stable-id");
    assert_eq!(retry.payload, original.payload);
}

#[test]
fn publish_request_clone_is_refcount_bump() {
    let request = request_safety("msg-1", b"job");
    let cloned = request.clone();
    assert!(std::ptr::eq(
        request.destination.exchange.as_ref(),
        cloned.destination.exchange.as_ref(),
    ));
    assert!(std::ptr::eq(
        request.destination.routing_key.as_ref(),
        cloned.destination.routing_key.as_ref(),
    ));
    assert!(std::ptr::eq(
        request.properties.message_id.as_ref(),
        cloned.properties.message_id.as_ref(),
    ));
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
    let config = PublisherConfig::with_safety(32, Duration::from_secs(5), SafetyMode::Unsafe);
    let actor = PublisherActor::spawn_with_delay_strategy_and_metrics(
        Arc::from(publisher),
        config,
        Metrics::default(),
        None,
    );

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
        "enable_confirms must not be called in Unsafe mode"
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
    let config = PublisherConfig::with_safety(32, Duration::from_secs(5), SafetyMode::Safe);
    let actor = PublisherActor::spawn_with_delay_strategy_and_metrics(
        Arc::from(publisher),
        config,
        Metrics::default(),
        None,
    );

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
    let config = PublisherConfig::with_safety(32, Duration::from_secs(5), SafetyMode::Safe);
    let actor = PublisherActor::spawn_with_delay_strategy_and_metrics(
        Arc::from(publisher),
        config,
        Metrics::default(),
        None,
    );

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
    let config = PublisherConfig::with_safety(32, Duration::from_secs(5), SafetyMode::Safe);
    let actor = PublisherActor::spawn_with_delay_strategy_and_metrics(
        Arc::from(publisher),
        config,
        Metrics::default(),
        None,
    );

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
    // First publish: succeeds, pending confirmation → stays in ledger.
    transport.push_pending_confirmation();
    // Second publish: recoverable error → triggers suspend.
    transport.push_confirmation(Err(TransportError::connection("connection lost")));
    // Recovery replays both messages; provide confirmations for the replays.
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));

    let actor = actor_safety(&transport, config_safety()).await;

    let w1 = actor
        .try_publish(request_safety("msg1", b"p1"))
        .expect("publish1");
    let w2 = actor
        .try_publish(request_safety("msg2", b"p2"))
        .expect("publish2");

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;

    // After the recoverable error, the actor should be suspended.
    // Recover with a new channel.
    actor
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("recovery");

    // Both messages should be confirmed after recovery.
    assert_eq!(
        w1.wait().await.expect("msg1 confirmed"),
        PublishOutcome::Confirmed {
            message_id: "msg1".into()
        }
    );
    assert_eq!(
        w2.wait().await.expect("msg2 confirmed"),
        PublishOutcome::Confirmed {
            message_id: "msg2".into()
        }
    );

    // Verify replay order: the first two publishes are the originals (seq 1, seq 2).
    // The next two are the replays after recovery. They must be in the same order.
    let publishes = publish_operations(&transport);
    assert_eq!(publishes.len(), 4, "2 original + 2 replay publishes");
    assert_eq!(
        publishes[2].properties.message_id.as_deref(),
        Some("msg1"),
        "replay must preserve sequence order: msg1 first"
    );
    assert_eq!(
        publishes[3].properties.message_id.as_deref(),
        Some("msg2"),
        "replay must preserve sequence order: msg2 second"
    );
}

#[tokio::test(start_paused = true)]
async fn pipeline_slow_path_completes_via_publish_in_flight() {
    let transport = MockTransport::default();
    // Gate the first publish so now_or_never() returns None → slow path.
    let gate = transport.push_publish_gate();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));

    let actor = actor_safety(&transport, config_safety()).await;

    let waiter = actor
        .try_publish(request_safety("slow-msg", b"payload"))
        .expect("publish");

    // Let the actor poll. The publish future is pending (gate not released).
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;

    // The publish was recorded (inside publish() after the gate), but the
    // future hasn't completed yet — the waiter should not be resolved.
    // Actually, the gate blocks before recording, so the publish op
    // should NOT be recorded yet.
    assert!(
        publish_operations(&transport).is_empty(),
        "gated publish must not be recorded until released"
    );

    // Release the gate so the slow-path future completes.
    assert!(gate.release(), "gate released");
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;

    // Now the publish should be recorded and confirmed.
    assert_eq!(
        publish_operations(&transport).len(),
        1,
        "gated publish completed"
    );
    assert!(matches!(
        waiter.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));
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
            message_id: "during-outage".into()
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
            message_id: "same-generation".into()
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
            message_id: "stable-id".into()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn unconfirmed_publish_is_retained_across_connection_loss() {
    let transport = MockTransport::default();
    transport.push_pending_confirmation();
    let actor = PublisherActor::spawn_with_delay_strategy_and_metrics(
        new_channel(&transport).await,
        PublisherConfig::with_safety(8, Duration::from_secs(5), SafetyMode::Safe),
        Metrics::default(),
        None,
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
            message_id: "generation-safe".into()
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
            message_id: "same-gen".into()
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

#[tokio::test(start_paused = true)]
async fn close_resolves_within_deadline_with_pending_confirmations() {
    let transport = MockTransport::default();
    transport.push_pending_confirmation();
    transport.push_pending_confirmation();
    // Gate the channel close so it would hang indefinitely without a deadline.
    let _close_gate = transport.push_close_channel_gate();

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

    let close_result = tokio::time::timeout(Duration::from_secs(10), actor.close()).await;
    assert!(
        close_result.is_ok(),
        "close should complete within deadline"
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
        request.exchange.as_ref(),
        "jobs",
        "plugin mode must publish on a delayed exchange, not the original"
    );
    assert!(
        request.exchange.ends_with(".delayed"),
        "exchange name should be the delayed variant, got: {}",
        request.exchange
    );
    assert_eq!(request.routing_key.as_ref(), "high");
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
        request.exchange.as_ref(),
        "",
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
        request.exchange.as_ref(),
        "jobs",
        "zero delay must publish on the original exchange"
    );
    assert_eq!(request.routing_key.as_ref(), "high");
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
            "tls": {"enabled": false},
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
            "queue_expiry_margin": 60
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
}

#[test]
fn delay_config_rejects_empty_buckets() {
    let json = serde_json::json!({
        "brokers": [{
            "name": "default",
            "hosts": [{"host": "rabbit.local", "port": 5672}],
            "vhost": "/",
            "credentials": {"username": "guest", "password": "secret"},
            "tls": {"enabled": false},
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
            "queue_expiry_margin": 60
        }
    });

    let config: Config = serde_json::from_value(json).expect("parse");
    let error = config
        .validate()
        .expect_err("empty buckets must be rejected");

    assert!(error.path().contains("delay.buckets"));
}

// ---------------------------------------------------------------------------
// Delay routing tests (from delay_routing.rs)
// ---------------------------------------------------------------------------

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
    let invalid = DelayConfig {
        mode: DelayMode::Ttl,
        buckets: vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(3),
        ],
        max_buckets: 2,
        queue_expiry_margin: Duration::from_mins(1),
    };

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

// ---------------------------------------------------------------------------
// Batch wait tests (Task 12 — wait_all)
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn wait_all_returns_results_in_original_order() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));

    let actor = actor_safety(&transport, config_safety()).await;

    let mut waiters = Vec::new();
    for i in 0..3u8 {
        let waiter = actor
            .try_publish(request_safety(&format!("msg-{i}"), b"payload"))
            .expect("publish");
        waiters.push((i as usize, waiter));
    }

    wait_for_publish_count(&transport, 3).await;

    let results = PublishWaiter::wait_all(waiters).await;
    assert_eq!(results.len(), 3);
    for (i, result) in &results {
        assert_eq!(*i, *i); // index preserved
        assert!(result.is_ok(), "result {i} should be confirmed");
    }
}

#[tokio::test(start_paused = true)]
async fn wait_all_preserves_order_regardless_of_completion_order() {
    let transport = MockTransport::default();
    // Three confirmations pushed in order; the actor resolves them in
    // sequence. wait_all must return results indexed 0, 1, 2 regardless.
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));

    let actor = actor_safety(&transport, config_safety()).await;

    let waiters: Vec<(usize, _)> = (0..3u8)
        .map(|i| {
            let waiter = actor
                .try_publish(request_safety(&format!("msg-{i}"), b"payload"))
                .expect("publish");
            (i as usize, waiter)
        })
        .collect();

    wait_for_publish_count(&transport, 3).await;

    let results = PublishWaiter::wait_all(waiters).await;
    let indices: Vec<usize> = results.iter().map(|(i, _)| *i).collect();
    assert_eq!(indices, vec![0, 1, 2], "results must be in original order");
    for (_, result) in &results {
        assert!(result.is_ok());
    }
}

#[tokio::test(start_paused = true)]
async fn wait_all_handles_empty_input() {
    let results = PublishWaiter::wait_all(Vec::new()).await;
    assert!(results.is_empty());
}

// ---------------------------------------------------------------------------
// Blind pump recovery wiring (connection_event → pump channel hot-swap)
// ---------------------------------------------------------------------------

fn request_blind(message_id: &str) -> PublishRequest {
    PublishRequest::new(
        Destination::new("jobs", "high"),
        Bytes::from_static(b"payload"),
        MessageProperties::new(message_id),
        Instant::now() + Duration::from_secs(30),
    )
}

#[tokio::test(start_paused = true)]
async fn connection_event_clears_then_restores_the_blind_pump_channel() {
    let transport = MockTransport::default();
    let channel = new_channel(&transport).await;
    let config = PublisherConfig::with_safety(4, Duration::from_secs(5), SafetyMode::Blind);
    let handle = PublisherActor::spawn_with_delay_strategy_and_metrics(
        channel,
        config,
        Metrics::default(),
        None,
    );

    // m1 is taken in by the pump and held pending on the gate.
    let gate = transport.push_publish_gate();
    let waiter = handle
        .publish_blind(request_blind("m1"))
        .await
        .expect("blind publish accepted");
    gate.wait_entered().await;

    // Recovering clears the pump channel: sends stay accepted, queued jobs
    // are dropped silently (blind semantics).
    handle
        .connection_event(PublisherConnectionEvent::Recovering { generation: 1 })
        .await
        .expect("publisher suspended");
    handle
        .publish_blind(request_blind("m2"))
        .await
        .expect("send stays accepted while recovering");
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert!(
        publish_operations(&transport).is_empty(),
        "m2 must be dropped while the pump channel is cleared"
    );

    // Ready installs the new channel before the actor resumes: publishes
    // restart without waiting for the still-gated m1.
    handle
        .connection_event(PublisherConnectionEvent::Ready {
            generation: 2,
            channel: new_channel(&transport).await,
            topology_restored: true,
        })
        .await
        .expect("publisher resumed");
    handle
        .publish_blind(request_blind("m3"))
        .await
        .expect("blind publish accepted after ready");
    wait_for_publish_count(&transport, 1).await;

    // The still in-flight m1 completes once its gate is released.
    assert!(gate.release(), "gate released");
    wait_for_publish_count(&transport, 2).await;

    let published_ids: Vec<Option<String>> = publish_operations(&transport)
        .into_iter()
        .map(|request| request.properties.message_id)
        .collect();
    assert!(published_ids.contains(&Some("m1".to_owned())));
    assert!(published_ids.contains(&Some("m3".to_owned())));
    assert!(
        !published_ids.contains(&Some("m2".to_owned())),
        "job queued while the channel was cleared must be dropped, got {published_ids:?}"
    );

    assert!(matches!(
        waiter.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));
}
