use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    config::{BrokerConfig, Credentials, Endpoint, TlsConfig},
    publisher::{
        Destination, MessageProperties, PublishErrorKind, PublishOutcome, PublishRequest,
        PublisherActor, PublisherConfig, PublisherConnectionEvent,
    },
    transport::{
        PublishConfirmation, ReturnedMessage, Transport,
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

fn config(max_messages: usize, max_bytes: usize) -> PublisherConfig {
    PublisherConfig::new(
        max_messages,
        max_bytes,
        Duration::from_millis(1),
        32,
        Duration::from_secs(5),
    )
}

fn request(message_id: &str, payload: &'static [u8]) -> PublishRequest {
    PublishRequest::new(
        Destination::new("jobs", "high"),
        Bytes::from_static(payload),
        MessageProperties::new(message_id),
        Instant::now() + Duration::from_secs(30),
    )
}

async fn actor(
    transport: &MockTransport,
    config: PublisherConfig,
) -> rabbit_rs_core::publisher::PublisherHandle {
    let publisher = transport
        .connect(&broker())
        .await
        .expect("connection")
        .open_publisher()
        .await
        .expect("publisher");
    PublisherActor::spawn(Arc::from(publisher), config)
}

async fn wait_for_publish_count(transport: &MockTransport, expected: usize) {
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

#[tokio::test(start_paused = true)]
async fn flushes_when_max_messages_is_reached() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let actor = actor(&transport, config(2, 1_024)).await;

    let first = actor.try_publish(request("one", b"a")).expect("first");
    let second = actor.try_publish(request("two", b"b")).expect("second");
    wait_for_publish_count(&transport, 2).await;

    assert!(matches!(
        first.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));
    assert!(matches!(
        second.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn flushes_when_max_bytes_is_reached() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let actor = actor(&transport, config(10, 5)).await;

    let _first = actor.try_publish(request("one", b"123")).expect("first");
    let _second = actor.try_publish(request("two", b"45")).expect("second");

    wait_for_publish_count(&transport, 2).await;
}

#[tokio::test(start_paused = true)]
async fn flushes_on_the_configured_timer() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let actor = actor(&transport, config(10, 1_024)).await;

    let waiter = actor.try_publish(request("one", b"a")).expect("publish");
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1)).await;
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
    let actor = actor(&transport, config(2, 1_024)).await;

    let first = actor.try_publish(request("one", b"a")).expect("first");
    let second = actor.try_publish(request("two", b"b")).expect("second");

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
    let actor = actor(&transport, config(2, 1_024)).await;

    let first = actor.try_publish(request("one", b"a")).expect("first");
    let second = actor.try_publish(request("two", b"b")).expect("second");

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
    let actor = actor(&transport, config(1, 1_024)).await;

    let waiter = actor
        .try_publish(request("returned", b"job"))
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
    let actor = actor(
        &transport,
        PublisherConfig::new(
            1,
            1_024,
            Duration::from_millis(1),
            32,
            Duration::from_millis(10),
        ),
    )
    .await;
    let waiter = actor.try_publish(request("slow", b"job")).expect("publish");
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
    let actor = actor(
        &transport,
        PublisherConfig::new(
            256,
            1_048_576,
            Duration::from_secs(1),
            1,
            Duration::from_secs(5),
        ),
    )
    .await;

    let _first = actor.try_publish(request("one", b"a")).expect("first slot");
    let error = actor
        .try_publish(request("two", b"b"))
        .expect_err("buffer must be full");

    assert_eq!(error.kind(), PublishErrorKind::Backpressure);
}

#[tokio::test(start_paused = true)]
async fn connection_loss_before_confirm_is_replayed() {
    let transport = MockTransport::default();
    transport.push_pending_confirmation();
    let actor = actor(&transport, config(1, 1_024)).await;
    let waiter = actor
        .try_publish(request("uncertain", b"job"))
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
    let original = request("stable-id", b"job");

    let retry = original.republish(Instant::now() + Duration::from_secs(30));

    assert_eq!(retry.properties.message_id, "stable-id");
    assert_eq!(retry.payload, original.payload);
}
