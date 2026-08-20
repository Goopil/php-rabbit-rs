use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    config::{
        BrokerConfig, Config, Credentials, Endpoint, PublisherConfigSection, TlsConfig,
        TopologyMode,
    },
    consumer::{ConsumerErrorKind, ConsumerSet, Subscription, SubscriptionPolicy},
    pool::ConnectionKey,
    transport::{Delivery as TransportDelivery, Transport, mock::MockTransport},
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
