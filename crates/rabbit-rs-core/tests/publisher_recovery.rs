use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    config::{BrokerConfig, Credentials, Endpoint, TlsConfig},
    publisher::{
        Destination, MessageProperties, PublishErrorKind, PublishOutcome, PublishRequest,
        PublisherActor, PublisherConfig, PublisherConnectionEvent, PublisherHandle,
    },
    transport::{
        PublishConfirmation, PublisherChannel, Transport, TransportError,
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

fn config(capacity: usize) -> PublisherConfig {
    PublisherConfig::new(
        1,
        1_024,
        Duration::from_millis(1),
        capacity,
        Duration::from_secs(5),
    )
}

fn request(message_id: &str, deadline: Instant) -> PublishRequest {
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

async fn new_channel(transport: &MockTransport) -> Arc<dyn PublisherChannel> {
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

async fn actor(transport: &MockTransport, capacity: usize) -> PublisherHandle {
    PublisherActor::spawn(new_channel(transport).await, config(capacity))
}

fn publish_operations(transport: &MockTransport) -> Vec<rabbit_rs_core::transport::PublishRequest> {
    transport
        .operations()
        .into_iter()
        .filter_map(|operation| match operation {
            TransportOperation::Publish(request) => Some(request),
            _ => None,
        })
        .collect()
}

async fn wait_for_publish_count(transport: &MockTransport, expected: usize) {
    for _ in 0..100 {
        if publish_operations(transport).len() == expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("publisher did not emit {expected} messages");
}

async fn suspend(actor: &PublisherHandle) {
    actor
        .connection_event(PublisherConnectionEvent::Recovering { generation: 1 })
        .await
        .expect("publisher suspended");
}

#[tokio::test(start_paused = true)]
async fn publication_accepted_during_recovery_is_sent_after_ready() {
    let transport = MockTransport::default();
    let actor = actor(&transport, 8).await;
    suspend(&actor).await;
    let waiter = actor
        .try_publish(request(
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
    let actor = actor(&transport, 8).await;
    suspend(&actor).await;
    let waiter = actor
        .try_publish(request(
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
    let actor = actor(&transport, 8).await;
    let waiter = actor
        .try_publish(request(
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
async fn message_still_in_batch_is_retained_across_connection_loss() {
    let transport = MockTransport::default();
    let actor = PublisherActor::spawn(
        new_channel(&transport).await,
        PublisherConfig::new(10, 1_024, Duration::from_secs(1), 8, Duration::from_secs(5)),
    );
    let waiter = actor
        .try_publish(request("batched", Instant::now() + Duration::from_secs(30)))
        .expect("batched publish");
    tokio::task::yield_now().await;
    assert!(publish_operations(&transport).is_empty());

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
    assert_eq!(publish_operations(&transport).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn late_confirm_from_old_generation_cannot_resolve_the_waiter() {
    let transport = MockTransport::default();
    let old_confirm = transport.push_controlled_confirmation();
    let actor = actor(&transport, 8).await;
    let waiter = actor
        .try_publish(request(
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
    let actor = actor(&transport, 8).await;
    let waiter = actor
        .try_publish(request("nacked", Instant::now() + Duration::from_secs(30)))
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
    let actor = actor(&transport, 8).await;
    suspend(&actor).await;
    let _waiter = actor
        .try_publish(request("gated", Instant::now() + Duration::from_secs(30)))
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
    let actor = actor(&transport, 8).await;
    let waiter = actor
        .try_publish(request("once", Instant::now() + Duration::from_secs(30)))
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
    let actor = actor(&transport, 1).await;
    suspend(&actor).await;
    let _first = actor
        .try_publish(request(
            "retained",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect("capacity slot");
    tokio::task::yield_now().await;

    let error = actor
        .try_publish(request(
            "overflow",
            Instant::now() + Duration::from_secs(30),
        ))
        .expect_err("global capacity exhausted");

    assert_eq!(error.kind(), PublishErrorKind::Backpressure);
}

#[tokio::test(start_paused = true)]
async fn deadline_expiring_during_outage_prevents_replay() {
    let transport = MockTransport::default();
    let actor = actor(&transport, 8).await;
    suspend(&actor).await;
    let waiter = actor
        .try_publish(request(
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
    let actor = actor(&transport, 8).await;
    suspend(&actor).await;
    let waiter = actor
        .try_publish(request("denied", Instant::now() + Duration::from_secs(30)))
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
    let actor = actor(&transport, 8).await;
    actor
        .connection_event(PublisherConnectionEvent::Recovering { generation: 3 })
        .await
        .expect("publisher suspended");
    let waiter = actor
        .try_publish(request(
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
    let actor = actor(&transport, 8).await;
    suspend(&actor).await;
    let waiter = actor
        .try_publish(request("closing", Instant::now() + Duration::from_secs(30)))
        .expect("queued");
    tokio::task::yield_now().await;

    actor.close().await.expect("close");

    assert_eq!(
        waiter.wait().await.expect_err("closed").kind(),
        PublishErrorKind::Closed
    );
}
