use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use rabbit_rs_core::{
    config::{BrokerConfig, Config, Credentials, Endpoint, TlsConfig, TopologyMode},
    consumer::{
        ConsumerErrorKind, ConsumerSet, DeliveryState, Subscription, SubscriptionId,
        SubscriptionPolicy,
    },
    pool::ConnectionKey,
    publisher::{Destination, PublisherActor, PublisherConfig},
    transport::{
        Delivery as TransportDelivery, PublishConfirmation, Transport,
        mock::{MockTransport, TransportOperation},
    },
};

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

async fn publisher(transport: &MockTransport) -> rabbit_rs_core::publisher::PublisherHandle {
    let channel = transport
        .connect(&broker("publisher", "/"))
        .await
        .expect("connection")
        .open_publisher()
        .await
        .expect("publisher channel");
    PublisherActor::spawn(
        Arc::from(channel),
        PublisherConfig::new(
            1,
            1_024,
            Duration::from_millis(1),
            32,
            Duration::from_secs(5),
        ),
    )
}

async fn let_sources_fill() {
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn multiplexes_subscriptions_across_two_connections() {
    let first_transport = MockTransport::default();
    let second_transport = MockTransport::default();
    first_transport.push_delivery(Ok(delivery(1, b"first")));
    second_transport.push_delivery(Ok(delivery(2, b"second")));
    let subscriptions = vec![
        subscription(
            &first_transport,
            "first",
            connection_key("first", "/one"),
            4,
            0,
        )
        .await,
        subscription(
            &second_transport,
            "second",
            connection_key("second", "/two"),
            8,
            0,
        )
        .await,
    ];
    let consumer = ConsumerSet::spawn(subscriptions, 2)
        .await
        .expect("consumer set");
    let_sources_fill().await;

    let first = consumer.next().await.expect("first delivery");
    let second = consumer.next().await.expect("second delivery");
    let ids = BTreeSet::from([first.subscription.clone(), second.subscription.clone()]);

    assert_eq!(
        ids,
        BTreeSet::from([SubscriptionId::new("first"), SubscriptionId::new("second")])
    );
}

#[tokio::test]
async fn scheduler_selects_the_highest_priority_ready_buffer() {
    let low_transport = MockTransport::default();
    let high_transport = MockTransport::default();
    low_transport.push_delivery(Ok(delivery(1, b"low")));
    high_transport.push_delivery(Ok(delivery(2, b"high")));
    let consumer = ConsumerSet::spawn(
        vec![
            subscription(&low_transport, "low", connection_key("low", "/"), 4, 0).await,
            subscription(&high_transport, "high", connection_key("high", "/"), 4, 10).await,
        ],
        2,
    )
    .await
    .expect("consumer set");
    let_sources_fill().await;

    let selected = consumer.next().await.expect("delivery");

    assert_eq!(selected.subscription, SubscriptionId::new("high"));
}

#[tokio::test]
async fn enforces_prefetch_per_subscription_and_global_in_flight_budget() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"first")));
    transport.push_delivery(Ok(delivery(2, b"second")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 7, 0).await],
        1,
    )
    .await
    .expect("consumer set");
    let_sources_fill().await;
    let first = consumer.next().await.expect("first");
    let waiting_consumer = consumer.clone();
    let second = tokio::spawn(async move { waiting_consumer.next().await });
    tokio::task::yield_now().await;

    assert!(!second.is_finished());
    assert!(
        transport
            .operations()
            .contains(&TransportOperation::Qos { prefetch: 7 })
    );

    first.ack().await.expect("ACK releases budget");
    assert!(second.await.expect("join").is_ok());
}

#[tokio::test]
async fn ack_uses_the_delivery_generation_and_channel() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(42, b"job")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");
    let item = consumer.next().await.expect("delivery");

    item.ack().await.expect("ACK");

    assert_eq!(item.state(), DeliveryState::Acked);
    assert!(transport.operations().contains(&TransportOperation::Ack {
        delivery_tag: 42,
        multiple: false,
    }));
}

#[tokio::test]
async fn stale_generation_ack_is_rejected_without_touching_the_new_channel() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(42, b"job")));
    let id = SubscriptionId::new("jobs");
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");
    let item = consumer.next().await.expect("delivery");
    consumer
        .update_generation(id, 2)
        .await
        .expect("new generation");

    let error = item.ack().await.expect_err("stale ACK");

    assert_eq!(error.kind(), ConsumerErrorKind::StaleGeneration);
    assert_eq!(item.state(), DeliveryState::Lost);
    assert!(
        !transport
            .operations()
            .iter()
            .any(|operation| matches!(operation, TransportOperation::Ack { .. }))
    );
}

#[tokio::test]
async fn release_zero_uses_basic_reject_with_requeue() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(9, b"job")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");
    let item = consumer.next().await.expect("delivery");

    item.release(Duration::ZERO).await.expect("release");

    assert_eq!(item.state(), DeliveryState::Rejected);
    assert!(
        transport
            .operations()
            .contains(&TransportOperation::Reject {
                delivery_tag: 9,
                requeue: true,
            })
    );
}

#[tokio::test]
async fn delayed_release_publishes_confirms_then_acks_original() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(11, b"job")));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let publisher = publisher(&transport).await;
    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0)
        .await
        .delayed_publisher(publisher, Destination::new("jobs", "high"));
    let consumer = ConsumerSet::spawn(vec![subscription], 1)
        .await
        .expect("consumer set");
    let item = consumer.next().await.expect("delivery");

    item.release(Duration::from_secs(5))
        .await
        .expect("delayed release");

    let operations = transport.operations();
    let publish = operations
        .iter()
        .position(|operation| matches!(operation, TransportOperation::Publish(_)))
        .expect("republish");
    let transport_request = operations
        .iter()
        .find_map(|operation| match operation {
            TransportOperation::Publish(request) => Some(request),
            _ => None,
        })
        .expect("published request");
    let ack = operations
        .iter()
        .position(|operation| {
            matches!(
                operation,
                TransportOperation::Ack {
                    delivery_tag: 11,
                    ..
                }
            )
        })
        .expect("ACK original");
    assert!(publish < ack);
    assert_eq!(transport_request.exchange, "jobs.delayed");
    assert_eq!(transport_request.properties.delay_ms, Some(5_000));
}

#[tokio::test]
async fn failed_delayed_publish_does_not_ack_the_original() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(12, b"job")));
    transport.push_confirmation(Ok(PublishConfirmation::Nack(None)));
    let publisher = publisher(&transport).await;
    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0)
        .await
        .delayed_publisher(publisher, Destination::new("jobs", "high"));
    let consumer = ConsumerSet::spawn(vec![subscription], 1)
        .await
        .expect("consumer set");
    let item = consumer.next().await.expect("delivery");

    assert!(item.release(Duration::from_secs(5)).await.is_err());
    assert_eq!(item.state(), DeliveryState::Pending);
    assert!(!transport.operations().iter().any(|operation| matches!(
        operation,
        TransportOperation::Ack {
            delivery_tag: 12,
            ..
        }
    )));
}

#[tokio::test]
async fn close_wakes_pending_next_with_a_typed_error() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");
    let waiting_consumer = consumer.clone();
    let waiting = tokio::spawn(async move { waiting_consumer.next().await });
    tokio::task::yield_now().await;

    consumer.close().await.expect("close");
    let error = waiting.await.expect("join").expect_err("closed consumer");

    assert_eq!(error.kind(), ConsumerErrorKind::Closed);
}
