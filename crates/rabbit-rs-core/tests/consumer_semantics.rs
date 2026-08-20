use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

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
    publisher::{Destination, PublisherActor, PublisherConfig},
    topology::delay::DelayStrategy,
    transport::{
        Delivery as TransportDelivery, PublishConfirmation, Transport, TransportError,
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

fn delivery_with_properties(
    tag: u64,
    payload: &'static [u8],
    message_id: &str,
    correlation_id: &str,
) -> TransportDelivery {
    let mut delivery = delivery(tag, payload);
    delivery.message_id = Some(message_id.to_owned());
    delivery.correlation_id = Some(correlation_id.to_owned());
    delivery
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

#[tokio::test]
async fn publisher_handle_exposes_its_confirm_timeout() {
    let transport = MockTransport::default();
    let channel = transport
        .connect(&broker("publisher", "/"))
        .await
        .expect("connection")
        .open_publisher()
        .await
        .expect("publisher channel");
    let timeout = Duration::from_millis(17);
    let publisher = PublisherActor::spawn(
        Arc::from(channel),
        PublisherConfig::new(1, 1_024, Duration::from_millis(1), 8, timeout),
    );

    assert_eq!(publisher.confirm_timeout(), timeout);
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

#[tokio::test(start_paused = true)]
async fn expired_next_waiter_does_not_consume_the_following_delivery() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"first")));
    transport.push_delivery(Ok(delivery(2, b"second")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");
    let first = consumer.next().await.expect("first delivery");

    let expired = tokio::time::timeout(Duration::from_millis(1), consumer.next()).await;
    assert!(expired.is_err());
    first.ack().await.expect("ACK releases budget");

    let second = tokio::time::timeout(Duration::from_millis(1), consumer.next())
        .await
        .expect("second waiter receives buffered delivery")
        .expect("second delivery");
    assert_eq!(second.payload, Bytes::from_static(b"second"));
}

#[tokio::test(start_paused = true)]
async fn multiple_expired_waiters_preserve_buffer_order_and_in_flight_budget() {
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
    let first = consumer.next().await.expect("first delivery");

    for _ in 0..2 {
        assert!(
            tokio::time::timeout(Duration::from_millis(1), consumer.next())
                .await
                .is_err()
        );
    }
    first.ack().await.expect("ACK releases budget");

    let second = consumer.next().await.expect("second delivery");
    assert_eq!(second.payload, Bytes::from_static(b"second"));
    second.ack().await.expect("ACK releases budget");
    let third = consumer.next().await.expect("third delivery");
    assert_eq!(third.payload, Bytes::from_static(b"third"));
}

#[tokio::test]
async fn consumer_tag_uses_the_raw_subscription_id() {
    let transport = MockTransport::default();

    let _consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    let request = transport
        .operations()
        .into_iter()
        .find_map(|operation| match operation {
            TransportOperation::Consume(request) => Some(request),
            _ => None,
        })
        .expect("consume request");
    assert_eq!(request.consumer_tag, "rabbit-rs.jobs");
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
async fn preserves_incoming_message_and_correlation_ids() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery_with_properties(
        42,
        b"job",
        "broker-message-id",
        "trace-id",
    )));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    let item = consumer.next().await.expect("delivery");

    assert_eq!(item.id.as_str(), "broker-message-id");
    assert_eq!(item.correlation_id.as_deref(), Some("trace-id"));
}

#[tokio::test]
async fn synthesizes_message_id_only_when_the_transport_property_is_absent() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(42, b"job")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        1,
    )
    .await
    .expect("consumer set");

    let item = consumer.next().await.expect("delivery");

    assert_eq!(item.id.as_str(), "1:4:42");
    assert_eq!(item.correlation_id, None);
}

#[tokio::test]
async fn partial_consumer_spawn_closes_all_open_channels() {
    let transport = MockTransport::default();
    let first = subscription(&transport, "first", connection_key("first", "/"), 4, 0).await;
    let second = subscription(&transport, "second", connection_key("second", "/"), 4, 0).await;
    transport.push_consumer_result(Ok(()));
    transport.push_consumer_result(Ok(()));
    transport.push_consumer_result(Ok(()));
    transport.push_consumer_result(Err(TransportError::connection("consume failed")));

    ConsumerSet::spawn(vec![first, second], 2)
        .await
        .expect_err("second consumer registration fails");

    assert_eq!(
        transport
            .operations()
            .iter()
            .filter(|operation| matches!(operation, TransportOperation::CloseChannel))
            .count(),
        2
    );
}

#[tokio::test]
async fn source_errors_are_bounded_so_a_delivery_cannot_be_starved() {
    let transport = MockTransport::default();
    for index in 0..100 {
        transport.push_delivery(Err(TransportError::connection(format!(
            "source failure {index}"
        ))));
    }
    transport.push_delivery(Ok(delivery(42, b"job")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 1, 0).await],
        1,
    )
    .await
    .expect("consumer set");
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }

    for _ in 0..64 {
        assert_eq!(
            consumer
                .next()
                .await
                .expect_err("bounded source error")
                .kind(),
            ConsumerErrorKind::Transport
        );
    }
    assert_eq!(consumer.next().await.expect("delivery").payload, b"job"[..]);
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
async fn transport_settlement_error_marks_the_delivery_lost() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(42, b"job")));
    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await;
    transport.push_consumer_result(Ok(()));
    transport.push_consumer_result(Ok(()));
    transport.push_consumer_result(Err(TransportError::connection("channel closed")));
    let consumer = ConsumerSet::spawn(vec![subscription], 1)
        .await
        .expect("consumer set");
    let item = consumer.next().await.expect("delivery");

    let error = item.ack().await.expect_err("transport ACK failure");

    assert_eq!(error.kind(), ConsumerErrorKind::Transport);
    assert_eq!(item.state(), DeliveryState::Lost);
    assert_eq!(
        item.ack()
            .await
            .expect_err("lost token remains terminal")
            .kind(),
        ConsumerErrorKind::AlreadySettled
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
    transport.push_delivery(Ok(delivery_with_properties(
        11,
        b"job",
        "broker-message-id",
        "trace-id",
    )));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let publisher = publisher(&transport).await;
    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0)
        .await
        .delayed_publisher(publisher, Destination::new("jobs", "high"))
        .delay_strategy(DelayStrategy::Plugin);
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
    assert_eq!(transport_request.exchange.as_ref(), "jobs.delayed");
    assert_eq!(
        transport_request.properties.message_id.as_deref(),
        Some("broker-message-id")
    );
    assert_eq!(
        transport_request.properties.correlation_id.as_deref(),
        Some("trace-id")
    );
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
        .delayed_publisher(publisher, Destination::new("jobs", "high"))
        .delay_strategy(DelayStrategy::Plugin);
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
async fn consumer_tag_uses_subscription_name_without_debug_wrapper() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"job")));
    let _consumer = ConsumerSet::spawn(
        vec![
            subscription(
                &transport,
                "orders_high",
                connection_key("orders_high", "/"),
                4,
                0,
            )
            .await,
        ],
        1,
    )
    .await
    .expect("consumer set");

    let consume_ops: Vec<_> = transport
        .operations()
        .into_iter()
        .filter(|op| matches!(op, TransportOperation::Consume(_)))
        .collect();
    assert!(!consume_ops.is_empty(), "consume was registered");

    if let TransportOperation::Consume(request) = &consume_ops[0] {
        assert!(
            request.consumer_tag.contains("orders_high"),
            "tag should contain the subscription name: {}",
            request.consumer_tag
        );
        assert!(
            !request.consumer_tag.contains("SubscriptionId"),
            "tag must not contain the Debug wrapper: {}",
            request.consumer_tag
        );
        assert!(
            !request.consumer_tag.contains('"'),
            "tag must not contain quotes from Debug: {}",
            request.consumer_tag
        );
    }
}

#[tokio::test]
async fn source_errors_are_bounded_so_deliveries_are_not_starved() {
    let transport = MockTransport::default();
    for _ in 0..200 {
        transport.push_delivery(Err(rabbit_rs_core::transport::TransportError::connection(
            "flapping",
        )));
    }
    transport.push_delivery(Ok(delivery(1, b"job")));
    let consumer = ConsumerSet::spawn(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await],
        64,
    )
    .await
    .expect("consumer set");
    let_sources_fill().await;

    let mut got_errors = 0;
    let mut got_delivery = false;
    for _ in 0..300 {
        match consumer.next().await {
            Ok(item) => {
                item.ack().await.expect("ACK");
                got_delivery = true;
                break;
            }
            Err(error) => {
                assert_eq!(error.kind(), ConsumerErrorKind::Transport);
                got_errors += 1;
            }
        }
    }
    assert!(
        got_delivery,
        "good delivery must surface after bounded errors"
    );
    assert!(
        got_errors <= 64,
        "source errors must be bounded by max_in_flight, got {got_errors}"
    );
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
