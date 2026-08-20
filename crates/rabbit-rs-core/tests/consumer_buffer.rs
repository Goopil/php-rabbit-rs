use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    config::{
        BrokerConfig, Config, Credentials, Endpoint, PublisherConfigSection, TlsConfig,
        TopologyMode,
    },
    consumer::{
        ConsumerErrorKind, ConsumerSet, DeliveryState, Subscription, SubscriptionId,
        SubscriptionPolicy,
    },
    pool::ConnectionKey,
    transport::{
        Delivery as TransportDelivery, Transport,
        mock::{MockTransport, TransportOperation},
    },
};
use std::collections::BTreeMap;

fn broker(name: &str, vhost: &str) -> BrokerConfig {
    BrokerConfig {
        name: name.to_owned(),
        hosts: vec![Endpoint::new("localhost", 5672)],
        vhost: vhost.to_owned(),
        credentials: Credentials::new("guest", "guest"),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(30),
    }
}

fn connection_key(name: &str, vhost: &str) -> ConnectionKey {
    let config = Config {
        brokers: vec![broker(name, vhost)],
        workers: vec![],
        topology_mode: TopologyMode::External,
        delay: rabbit_rs_core::config::DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
        publisher: PublisherConfigSection::default(),
    }
    .validate()
    .expect("valid config");
    ConnectionKey::from_config(&config)
}

fn delivery(tag: u64, payload: &'static [u8]) -> TransportDelivery {
    TransportDelivery {
        delivery_tag: tag,
        exchange: "jobs".to_owned(),
        routing_key: "high".to_owned(),
        redelivered: false,
        message_id: None,
        correlation_id: None,
        headers: BTreeMap::new(),
        payload: Bytes::from_static(payload),
    }
}

async fn subscription(
    transport: &MockTransport,
    id: &str,
    key: ConnectionKey,
    prefetch: u16,
    priority: i16,
) -> Subscription {
    let channel = transport
        .connect(&broker(id, "/"))
        .await
        .expect("connection")
        .open_consumer()
        .await
        .expect("consumer channel");
    Subscription::new(id, key, format!("queue.{id}"), Arc::from(channel))
        .prefetch(prefetch)
        .channel_id(prefetch)
        .policy(SubscriptionPolicy::new(1, priority, Duration::from_secs(1)))
}

#[tokio::test]
async fn buffered_next_returns_delivery_from_flume_buffer() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg-0")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    let delivery = consumer.next().await.expect("delivery");
    assert_eq!(delivery.payload, Bytes::from_static(b"msg-0"));
}

#[tokio::test]
async fn buffered_next_returns_multiple_deliveries_in_order() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"first")));
    transport.push_delivery(Ok(delivery(2, b"second")));
    transport.push_delivery(Ok(delivery(3, b"third")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    let first = consumer.next().await.expect("first");
    let second = consumer.next().await.expect("second");
    let third = consumer.next().await.expect("third");

    assert_eq!(first.payload, Bytes::from_static(b"first"));
    assert_eq!(second.payload, Bytes::from_static(b"second"));
    assert_eq!(third.payload, Bytes::from_static(b"third"));
}

#[tokio::test]
async fn buffered_next_returns_error_from_source_failure() {
    let transport = MockTransport::default();
    transport.push_delivery(Err(rabbit_rs_core::transport::TransportError::connection(
        "source failure",
    )));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    let error = consumer.next().await.expect_err("source error");
    assert_eq!(error.kind(), ConsumerErrorKind::Transport);
}

#[tokio::test]
async fn buffered_consumer_supports_ack_and_close() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"job")));
    transport.push_consumer_result(Ok(()));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    let delivery = consumer.next().await.expect("delivery");
    delivery.ack().await.expect("ACK");
    consumer.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn batched_ack_coalesces_multiple_deliveries_into_one_ack_with_multiple_true() {
    let transport = MockTransport::default();
    for tag in 1..=16 {
        transport.push_delivery(Ok(delivery(tag, b"job")));
    }
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 16, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    let mut tags = Vec::new();
    for _ in 0..16 {
        let item = consumer.next().await.expect("delivery");
        item.ack().await.expect("ACK");
        tags.push(item.id.as_str().to_owned());
    }

    // Advance time to let the background drain fire.
    tokio::time::advance(Duration::from_millis(2)).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    let ack_calls: Vec<_> = transport
        .operations()
        .into_iter()
        .filter_map(|op| match op {
            TransportOperation::Ack {
                delivery_tag,
                multiple,
            } => Some((delivery_tag, multiple)),
            _ => None,
        })
        .collect();

    // A single batched ack with multiple=true and the highest delivery tag.
    assert_eq!(
        ack_calls.len(),
        1,
        "expected one batched ack, got {ack_calls:?}"
    );
    assert!(ack_calls[0].1, "multiple flag must be true");
    assert_eq!(ack_calls[0].0, 16, "delivery tag must be the highest (16)");
}

#[tokio::test(start_paused = true)]
async fn batched_ack_returns_immediately_without_awaiting_transport() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"job")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    let item = consumer.next().await.expect("delivery");

    // ack() should return immediately without any transport round-trip.
    // The transport ack only fires after the drain interval.
    item.ack().await.expect("ACK");

    assert_eq!(item.state(), DeliveryState::Acked);
    // No ack should have been sent to the transport yet.
    assert!(
        !transport
            .operations()
            .iter()
            .any(|op| matches!(op, TransportOperation::Ack { .. })),
        "no ack should be sent before the drain interval fires"
    );

    tokio::time::advance(Duration::from_millis(2)).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    assert!(
        transport
            .operations()
            .iter()
            .any(|op| matches!(op, TransportOperation::Ack { multiple: true, .. }))
    );
}

#[tokio::test(start_paused = true)]
async fn batched_ack_drains_on_close() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"job")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    let item = consumer.next().await.expect("delivery");
    item.ack().await.expect("ACK");

    // Close should flush any pending acks.
    consumer.close().await.expect("close");

    assert!(
        transport
            .operations()
            .iter()
            .any(|op| matches!(op, TransportOperation::Ack { multiple: true, .. }))
    );
}

#[tokio::test]
async fn try_next_returns_none_on_empty_buffer() {
    let transport = MockTransport::default();
    transport.keep_delivery_stream_open();
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    let result = consumer.try_next().expect("try_next ok");
    assert!(result.is_none(), "empty buffer should return None");
}

#[tokio::test]
async fn try_next_returns_some_delivery_from_filled_buffer() {
    let transport = MockTransport::default();
    transport.keep_delivery_stream_open();
    transport.push_delivery(Ok(delivery(1, b"msg-0")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    let delivery = consumer
        .try_next()
        .expect("try_next ok")
        .expect("delivery available");
    assert_eq!(delivery.payload, Bytes::from_static(b"msg-0"));

    let result = consumer.try_next().expect("try_next ok");
    assert!(result.is_none(), "buffer should now be empty");
}

#[tokio::test]
async fn try_next_returns_multiple_deliveries_in_order() {
    let transport = MockTransport::default();
    transport.keep_delivery_stream_open();
    transport.push_delivery(Ok(delivery(1, b"first")));
    transport.push_delivery(Ok(delivery(2, b"second")));
    transport.push_delivery(Ok(delivery(3, b"third")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    let first = consumer.try_next().expect("try_next ok").expect("first");
    let second = consumer.try_next().expect("try_next ok").expect("second");
    let third = consumer.try_next().expect("try_next ok").expect("third");

    assert_eq!(first.payload, Bytes::from_static(b"first"));
    assert_eq!(second.payload, Bytes::from_static(b"second"));
    assert_eq!(third.payload, Bytes::from_static(b"third"));

    let result = consumer.try_next().expect("try_next ok");
    assert!(result.is_none(), "buffer should be empty after three");
}

#[tokio::test]
async fn try_next_returns_error_from_source_failure() {
    let transport = MockTransport::default();
    transport.keep_delivery_stream_open();
    transport.push_delivery(Err(rabbit_rs_core::transport::TransportError::connection(
        "source failure",
    )));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    let error = consumer.try_next().expect_err("source error");
    assert_eq!(error.kind(), ConsumerErrorKind::Transport);
}

#[tokio::test]
async fn try_next_returns_error_after_close() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    // Give the pump time to exit (no deliveries, stream returns None, buffer
    // disconnects). Then close and verify try_next reports the closed state.
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    consumer.close().await.expect("close");

    let error = consumer.try_next().expect_err("closed error");
    assert_eq!(error.kind(), ConsumerErrorKind::Closed);
}

#[tokio::test]
async fn try_next_discards_stale_generation_deliveries() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"stale")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    // Bump the generation — deliveries buffered under generation 1 are now stale.
    consumer
        .update_generation(SubscriptionId::new("jobs"), 2)
        .await
        .expect("new generation");

    // try_next should discard the stale delivery (generation 1 != current 2)
    // and return None since the buffer is now empty.
    let result = consumer.try_next();
    assert!(result.is_ok(), "try_next should succeed, got: {result:?}");
    assert!(
        result.unwrap().is_none(),
        "stale delivery must be discarded, buffer empty"
    );
}
