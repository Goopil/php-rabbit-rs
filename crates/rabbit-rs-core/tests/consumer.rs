use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use rabbit_rs_core::{
    client::{ClientErrorKind, ClientPool},
    config::{
        BrokerConfig, Config, ConsumerConfigSection, Credentials, Endpoint, PublisherConfigSection,
        SchedulerConfig, SubscriptionConfig, TlsConfig, TopologyMode, WorkerProfile,
    },
    consumer::{
        ConsumerErrorKind, ConsumerSet, DeliveryState, Settlement, Subscription, SubscriptionId,
        SubscriptionPolicy,
    },
    metrics::Metrics,
    pool::ConnectionKey,
    publisher::{Destination, MessageProperties, PublishRequest, PublisherActor, PublisherConfig},
    topology::delay::DelayStrategy,
    transport::{
        Delivery as TransportDelivery, PublishConfirmation, QueueKind, Transport, TransportError,
        mock::{MockTransport, TransportOperation},
    },
};

mod helper {
    use super::*;

    pub fn broker(name: &str, vhost: &str) -> BrokerConfig {
        BrokerConfig {
            name: name.to_owned(),
            hosts: vec![Endpoint::new("localhost", 5672)],
            vhost: vhost.to_owned(),
            credentials: Credentials::new("guest", "guest"),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        }
    }

    pub fn worker_profile(name: &str, broker_name: &str, queue: &str) -> WorkerProfile {
        WorkerProfile {
            name: name.to_owned(),
            subscriptions: vec![SubscriptionConfig {
                name: queue.to_owned(),
                broker: broker_name.to_owned(),
                queue: queue.to_owned(),
                weight: 1,
                priority_class: 0,
                prefetch: 8,
                starvation_after: Duration::from_secs(30),
                max_buffered_bytes: 64 * 1024 * 1024,
                max_message_bytes: None,
                early_ack: false,
                no_ack: false,
            }],
            scheduler: SchedulerConfig::weighted_fair(),
        }
    }

    pub fn connection_key(name: &str, vhost: &str) -> ConnectionKey {
        let config = Config {
            brokers: vec![broker(name, vhost)],
            workers: vec![],
            topology_mode: TopologyMode::External,
            delay: rabbit_rs_core::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
            consumer: ConsumerConfigSection::default(),
            queue_type: QueueKind::Quorum,
            queue_durable: true,
        }
        .validate()
        .expect("valid config");
        ConnectionKey::from_config(&config)
    }

    pub fn delivery(tag: u64, payload: &'static [u8]) -> TransportDelivery {
        TransportDelivery {
            delivery_tag: tag,
            exchange: "jobs".to_owned(),
            routing_key: "high".to_owned(),
            redelivered: false,
            message_id: None,
            correlation_id: None,
            headers: Arc::new(BTreeMap::new()),
            payload: Bytes::from_static(payload),
        }
    }

    pub fn delivery_with_properties(
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

    pub async fn subscription(
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

    pub async fn publisher(
        transport: &MockTransport,
    ) -> rabbit_rs_core::publisher::PublisherHandle {
        let channel = transport
            .connect(&broker("publisher", "/"))
            .await
            .expect("connection")
            .open_publisher()
            .await
            .expect("publisher channel");
        PublisherActor::spawn(
            Arc::from(channel),
            PublisherConfig::new(32, Duration::from_secs(5)),
        )
    }

    pub async fn let_sources_fill() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    pub async fn let_actor_process() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    pub fn close_channel_count(transport: &MockTransport) -> usize {
        transport
            .operations()
            .iter()
            .filter(|op| matches!(op, TransportOperation::CloseChannel))
            .count()
    }
}

use helper::*;

// ---------------------------------------------------------------------------
// Consumer semantics tests (from consumer_semantics.rs)
// ---------------------------------------------------------------------------

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
    let publisher = PublisherActor::spawn(Arc::from(channel), PublisherConfig::new(8, timeout));

    assert_eq!(publisher.confirm_timeout(), timeout);
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
    let consumer = ConsumerSet::spawn(subscriptions)
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
    // Gate the low delivery so the low pump blocks until the high delivery
    // has been pushed. This ensures both deliveries are buffered before the
    // actor dispatches, so the scheduler can select by priority.
    let low_gate = low_transport.push_delivery_gate();
    let consumer = ConsumerSet::spawn(vec![
        subscription(&low_transport, "low", connection_key("low", "/"), 4, 0).await,
        subscription(&high_transport, "high", connection_key("high", "/"), 4, 10).await,
    ])
    .await
    .expect("consumer set");
    // Let the high pump push its delivery.
    let_sources_fill().await;
    // Release the low gate so both deliveries are now buffered.
    let _ = low_gate.release();
    let_sources_fill().await;

    let selected = consumer.next().await.expect("delivery");

    assert_eq!(selected.subscription, SubscriptionId::new("high"));
}

#[tokio::test]
async fn applies_broker_qos_per_subscription_and_streams_deliveries() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"first")));
    transport.push_delivery(Ok(delivery(2, b"second")));
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 7, 0).await,
    ])
    .await
    .expect("consumer set");
    let_sources_fill().await;

    // There is no worker-level dispatch budget: broker QoS (prefetch=7) is the
    // only structural bound on un-acked deliveries, so both deliveries stream
    // through without waiting for a settlement.
    let first = consumer.next().await.expect("first");
    let second = consumer.next().await.expect("second");

    assert!(
        transport
            .operations()
            .contains(&TransportOperation::Qos { prefetch: 7 })
    );
    assert_eq!(first.payload, Bytes::from_static(b"first"));
    assert_eq!(second.payload, Bytes::from_static(b"second"));
}

#[tokio::test(start_paused = true)]
async fn expired_next_waiter_does_not_consume_the_following_delivery() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"first")));
    transport.push_delivery(Ok(delivery(2, b"second")));
    // Each stream poll pops one gate: the first gate holds the first delivery
    // until released, the second gate holds the second delivery after the
    // first has been consumed.
    let first_gate = transport.push_delivery_gate();
    let second_gate = transport.push_delivery_gate();
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
    .await
    .expect("consumer set");

    let _ = first_gate.release();
    let first = consumer.next().await.expect("first delivery");

    // The second delivery is held at the pump — the waiter must time out
    // without consuming anything.
    let expired = tokio::time::timeout(Duration::from_millis(1), consumer.next()).await;
    assert!(expired.is_err());
    first.ack().await.expect("ACK");

    // The following delivery arrives after the waiter expired: it must still
    // be consumable.
    let _ = second_gate.release();
    let second = tokio::time::timeout(Duration::from_millis(1), consumer.next())
        .await
        .expect("second waiter receives buffered delivery")
        .expect("second delivery");
    assert_eq!(second.payload, Bytes::from_static(b"second"));
}

#[tokio::test(start_paused = true)]
async fn multiple_expired_waiters_preserve_buffer_order() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"first")));
    transport.push_delivery(Ok(delivery(2, b"second")));
    transport.push_delivery(Ok(delivery(3, b"third")));
    let first_gate = transport.push_delivery_gate();
    let second_gate = transport.push_delivery_gate();
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
    .await
    .expect("consumer set");

    let _ = first_gate.release();
    let _first = consumer.next().await.expect("first delivery");

    // Both the second and third waiters expire while the second delivery is
    // held at the pump.
    for _ in 0..2 {
        assert!(
            tokio::time::timeout(Duration::from_millis(1), consumer.next())
                .await
                .is_err()
        );
    }

    // Release the second delivery; the third flows freely behind it.
    let _ = second_gate.release();
    let second = consumer.next().await.expect("second delivery");
    assert_eq!(second.payload, Bytes::from_static(b"second"));
    let third = consumer.next().await.expect("third delivery");
    assert_eq!(third.payload, Bytes::from_static(b"third"));
}

#[tokio::test]
async fn consumer_tag_uses_the_raw_subscription_id() {
    let transport = MockTransport::default();

    let _consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
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

#[tokio::test(start_paused = true)]
async fn ack_uses_the_delivery_generation_and_channel() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(42, b"job")));
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
    .await
    .expect("consumer set");
    let item = consumer.next().await.expect("delivery");

    item.ack().await.expect("ACK");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

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
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
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
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
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

    ConsumerSet::spawn(vec![first, second])
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
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 1, 0).await,
    ])
    .await
    .expect("consumer set");

    // Let pumps push all deliveries and errors, and let the actor drain
    // source_errors into the flume buffer. With a buffer capacity of 2, the
    // actor dispatches a couple of items per notify/timer tick.
    // The source_errors deque is bounded to SOURCE_ERROR_CAPACITY (64), so at
    // most 64 errors are retained; the remaining 36 are dropped on the floor.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

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

#[tokio::test(start_paused = true)]
async fn stale_generation_ack_is_rejected_without_touching_the_new_channel() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(42, b"job")));
    let id = SubscriptionId::new("jobs");
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
    .await
    .expect("consumer set");
    let item = consumer.next().await.expect("delivery");
    consumer
        .update_generation(id, 2)
        .await
        .expect("new generation");

    item.ack().await.expect("ACK enqueued (fire-and-forget)");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let errors = consumer.drain_errors();
    assert!(
        !errors.is_empty(),
        "expected stale-generation settlement error"
    );
    assert_eq!(errors[0].kind, ConsumerErrorKind::StaleGeneration);
    assert_eq!(item.state(), DeliveryState::Lost);
    assert!(
        !transport
            .operations()
            .iter()
            .any(|operation| matches!(operation, TransportOperation::Ack { .. }))
    );
}

#[tokio::test(start_paused = true)]
async fn transport_settlement_error_marks_the_delivery_lost() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(42, b"job")));
    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await;
    transport.push_consumer_result(Ok(()));
    transport.push_consumer_result(Ok(()));
    transport.push_consumer_result(Err(TransportError::connection("channel closed")));
    let consumer = ConsumerSet::spawn(vec![subscription])
        .await
        .expect("consumer set");
    let item = consumer.next().await.expect("delivery");

    item.ack().await.expect("ACK enqueued (fire-and-forget)");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let errors = consumer.drain_errors();
    assert!(!errors.is_empty(), "expected transport settlement error");
    assert_eq!(errors[0].kind, ConsumerErrorKind::Transport);
    assert_eq!(item.state(), DeliveryState::Lost);
    assert_eq!(
        item.ack()
            .await
            .expect_err("lost token remains terminal")
            .kind(),
        ConsumerErrorKind::AlreadySettled
    );
}

#[tokio::test(start_paused = true)]
async fn release_zero_uses_basic_reject_with_requeue() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(9, b"job")));
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
    .await
    .expect("consumer set");
    let item = consumer.next().await.expect("delivery");

    item.release(Duration::ZERO)
        .await
        .expect("release enqueued");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

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

#[tokio::test(start_paused = true)]
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
    let consumer = ConsumerSet::spawn(vec![subscription])
        .await
        .expect("consumer set");
    let item = consumer.next().await.expect("delivery");

    item.release(Duration::from_secs(5))
        .await
        .expect("delayed release enqueued");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

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

#[tokio::test(start_paused = true)]
async fn failed_delayed_publish_does_not_ack_the_original() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(12, b"job")));
    transport.push_confirmation(Ok(PublishConfirmation::Nack(None)));
    let publisher = publisher(&transport).await;
    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0)
        .await
        .delayed_publisher(publisher, Destination::new("jobs", "high"))
        .delay_strategy(DelayStrategy::Plugin);
    let consumer = ConsumerSet::spawn(vec![subscription])
        .await
        .expect("consumer set");
    let item = consumer.next().await.expect("delivery");

    item.release(Duration::from_secs(5))
        .await
        .expect("release enqueued (fire-and-forget)");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    assert_eq!(item.state(), DeliveryState::Pending);
    assert!(!transport.operations().iter().any(|operation| matches!(
        operation,
        TransportOperation::Ack {
            delivery_tag: 12,
            ..
        }
    )));
    assert!(
        !consumer.drain_errors().is_empty(),
        "expected a settlement error for the failed publish"
    );
}

#[tokio::test(start_paused = true)]
async fn retryable_settlement_failure_preserves_ledger_and_allows_retry() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg1")));
    transport.push_delivery(Ok(delivery(2, b"msg2")));
    // The delayed release will fail with a Nack confirmation (retryable:
    // ConsumerErrorKind::Publish, NOT StaleGeneration/Transport).
    transport.push_confirmation(Ok(PublishConfirmation::Nack(None)));
    let publisher = publisher(&transport).await;
    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0)
        .await
        .delayed_publisher(publisher, Destination::new("jobs", "high"))
        .delay_strategy(DelayStrategy::Plugin);
    let consumer = ConsumerSet::spawn(vec![subscription])
        .await
        .expect("consumer set");

    let d1 = consumer.next().await.expect("delivery 1");
    let d2 = consumer.next().await.expect("delivery 2");

    // Attempt delayed release — fire-and-forget enqueues the command.
    d1.release(Duration::from_secs(5))
        .await
        .expect("release enqueued");

    // Let the actor process the settlement. The publish gets a Nack
    // confirmation, which is a retryable error (ConsumerErrorKind::Publish).
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let errors = consumer.drain_errors();
    assert!(!errors.is_empty(), "expected a retryable settlement error");
    assert!(
        !matches!(
            errors[0].kind,
            ConsumerErrorKind::StaleGeneration | ConsumerErrorKind::Transport
        ),
        "failure must be retryable, not terminal"
    );
    assert_eq!(
        d1.state(),
        DeliveryState::Pending,
        "delivery stays Pending for retry"
    );

    // Retry the release with a zero delay (immediate reject) — should succeed,
    // proving the ledger entry was preserved for retry.
    d1.release(Duration::ZERO)
        .await
        .expect("retry release enqueued");
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;
    assert_eq!(d1.state(), DeliveryState::Rejected);

    // The second delivery is unaffected and can still be settled.
    d2.ack().await.expect("ack d2 enqueued");
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;
    assert_eq!(d2.state(), DeliveryState::Acked);

    let _ = consumer.close().await;
}

#[tokio::test]
async fn consumer_tag_uses_subscription_name_without_debug_wrapper() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"job")));
    let _consumer = ConsumerSet::spawn(vec![
        subscription(
            &transport,
            "orders_high",
            connection_key("orders_high", "/"),
            4,
            0,
        )
        .await,
    ])
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
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
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
        "source errors must be bounded by SOURCE_ERROR_CAPACITY, got {got_errors}"
    );
}

#[tokio::test]
async fn close_wakes_pending_next_with_a_typed_error() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
    .await
    .expect("consumer set");
    let waiting_consumer = consumer.clone();
    let waiting = tokio::spawn(async move { waiting_consumer.next().await });
    tokio::task::yield_now().await;

    consumer.close().await.expect("close");
    let error = waiting.await.expect("join").expect_err("closed consumer");

    assert_eq!(error.kind(), ConsumerErrorKind::Closed);
}

// ---------------------------------------------------------------------------
// Consumer cleanup tests (from consumer_cleanup.rs)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn drop_closes_subscription_channels_without_explicit_close() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
    .await
    .expect("consumer set");

    drop(consumer);
    let_actor_process().await;

    assert_eq!(
        close_channel_count(&transport),
        1,
        "dropping ConsumerHandle must close all subscription channels"
    );
}

#[tokio::test]
async fn drop_closes_channels_for_multiple_subscriptions() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "first", connection_key("first", "/"), 4, 0).await,
        subscription(&transport, "second", connection_key("second", "/"), 4, 0).await,
    ])
    .await
    .expect("consumer set");

    drop(consumer);
    let_actor_process().await;

    assert_eq!(
        close_channel_count(&transport),
        2,
        "dropping ConsumerHandle must close every subscription channel"
    );
}

#[tokio::test]
async fn drop_does_not_double_close_when_close_was_already_called() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
    .await
    .expect("consumer set");

    consumer.close().await.expect("explicit close");
    let closes_after_close = close_channel_count(&transport);

    drop(consumer);
    let_actor_process().await;

    assert_eq!(
        close_channel_count(&transport),
        closes_after_close,
        "Drop must not send a second Close after explicit close()"
    );
}

#[tokio::test]
async fn drop_sends_close_only_once_across_clones() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
    .await
    .expect("consumer set");

    let clone = consumer.clone();
    drop(consumer);
    let_actor_process().await;
    let closes_after_first_drop = close_channel_count(&transport);

    drop(clone);
    let_actor_process().await;

    assert_eq!(
        close_channel_count(&transport),
        closes_after_first_drop,
        "Drop must not close channels twice when the last clone is dropped"
    );
}

#[tokio::test(start_paused = true)]
async fn next_after_drop_returns_typed_error_not_panic() {
    let transport = MockTransport::default();
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
    .await
    .expect("consumer set");

    let clone = consumer.clone();
    drop(consumer);
    let_actor_process().await;

    let result = tokio::time::timeout(Duration::from_millis(100), clone.next()).await;
    match result {
        Ok(Err(error)) => assert_eq!(
            error.kind(),
            ConsumerErrorKind::Closed,
            "next() after drop must return a Closed error"
        ),
        Ok(Ok(_)) => panic!("next() must not succeed after drop"),
        Err(elapsed) => panic!(
            "next() must not hang after drop — it should return a typed error (elapsed: {elapsed:?})"
        ),
    }
}

#[tokio::test]
async fn drop_with_pending_delivery_still_closes_channels() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"job")));
    let consumer = ConsumerSet::spawn(vec![
        subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await,
    ])
    .await
    .expect("consumer set");
    let_actor_process().await;
    let _item = consumer.next().await.expect("delivery");

    drop(consumer);
    let_actor_process().await;

    assert_eq!(
        close_channel_count(&transport),
        1,
        "channels must be closed even with an in-flight delivery"
    );
}

#[tokio::test]
async fn total_prefetch_does_not_overflow_u16() {
    let transport = MockTransport::default();
    let subs = vec![
        subscription(&transport, "a", connection_key("a", "/"), 60000, 0).await,
        subscription(&transport, "b", connection_key("b", "/"), 60000, 0).await,
    ];
    // 60000 + 60000 = 120000 — overflows u16 (max 65535)
    // Should not panic; buffer_size should be computed correctly
    let consumer = ConsumerSet::spawn(subs).await;
    assert!(consumer.is_ok());
    let consumer = consumer.unwrap();
    consumer.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn settle_through_acks_contiguous_prefix() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg1")));
    transport.push_delivery(Ok(delivery(2, b"msg2")));
    transport.push_delivery(Ok(delivery(3, b"msg3")));

    let sub = subscription(&transport, "s1", connection_key("b", "/"), 3, 0).await;
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    let d1 = consumer.next().await.expect("d1");
    let d2 = consumer.next().await.expect("d2");
    let d3 = consumer.next().await.expect("d3");

    // Ack through d3 — should ack 1, 2, 3 with multiple=true
    consumer
        .ack_through(&d3)
        .await
        .expect("ack through enqueued");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let ops = transport.operations();
    let acks: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, TransportOperation::Ack { multiple: true, .. }))
        .collect();
    assert_eq!(acks.len(), 1); // One ack call with multiple=true

    // All three deliveries should be acked
    assert_eq!(d1.state(), DeliveryState::Acked);
    assert_eq!(d2.state(), DeliveryState::Acked);
    assert_eq!(d3.state(), DeliveryState::Acked);

    consumer.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn settle_through_rejects_non_contiguous_prefix() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg1")));
    transport.push_delivery(Ok(delivery(3, b"msg3"))); // gap: tag 2 missing

    let sub = subscription(&transport, "s1", connection_key("b", "/"), 3, 0).await;
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    let _d1 = consumer.next().await.expect("d1"); // tag 1
    let d3 = consumer.next().await.expect("d3"); // tag 3

    consumer
        .ack_through(&d3)
        .await
        .expect("ack through enqueued (fire-and-forget)");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let errors = consumer.drain_errors();
    assert!(!errors.is_empty(), "expected non-contiguous prefix error");
    assert_eq!(errors[0].kind, ConsumerErrorKind::Transport);
    consumer.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn try_next_batch_drains_buffer() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg1")));
    transport.push_delivery(Ok(delivery(2, b"msg2")));
    transport.push_delivery(Ok(delivery(3, b"msg3")));

    let sub = subscription(&transport, "s1", connection_key("b", "/"), 3, 0).await;
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    let_sources_fill().await;

    let batch = consumer.try_next_batch(10).expect("batch");
    assert_eq!(batch.len(), 3);
    for d in batch {
        d.ack().await.expect("ack");
    }
    consumer.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn try_next_batch_returns_partial_batch_on_error() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg1")));
    transport.push_delivery(Ok(delivery(2, b"msg2")));
    // Push an error into the buffer (after the two deliveries)
    transport.push_delivery(Err(TransportError::connection("test error")));
    transport.push_delivery(Ok(delivery(3, b"msg3")));

    let sub = subscription(&transport, "s1", connection_key("b", "/"), 4, 0).await;
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    // Let deliveries flow into the buffer
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;

    // try_next_batch(10) should return 2 deliveries + stash the error
    let batch = consumer.try_next_batch(10).expect("partial batch");
    assert_eq!(
        batch.len(),
        2,
        "should return partial batch, not discard it"
    );
    assert_eq!(batch[0].delivery_tag(), 1);
    assert_eq!(batch[1].delivery_tag(), 2);

    // Next call should return delivery 3 (error was stashed, surfaced when batch is empty)
    let batch2 = consumer.try_next_batch(10).expect("batch with delivery 3");
    assert_eq!(batch2.len(), 1);
    assert_eq!(batch2[0].delivery_tag(), 3);

    // Now the stashed error should surface
    let result = consumer.try_next_batch(10);
    assert!(
        result.is_err(),
        "stashed error should surface on empty batch"
    );

    let _ = consumer.close().await;
}

#[tokio::test(start_paused = true)]
async fn flume_holds_two_prefetches_so_next_batch_fills_completely() {
    let transport = MockTransport::default();
    for tag in 1..=256_u64 {
        transport.push_delivery(Ok(delivery(tag, b"payload")));
    }

    let sub = subscription(&transport, "s1", connection_key("b", "/"), 128, 0).await;
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    // Let the pumps push everything and the actor fill the flume buffer to
    // capacity before any drain.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // With prefetch=128 the flume holds 2x128=256, so nextBatch(256) can fill
    // a complete batch in one call.
    let batch = consumer.try_next_batch(256).expect("full batch");
    assert_eq!(
        batch.len(),
        256,
        "nextBatch(256) must fill completely from the flume buffer"
    );

    let _ = consumer.close().await;
}

#[tokio::test(start_paused = true)]
async fn spawn_with_large_prefetch_delivers_beyond_the_legacy_command_capacity() {
    let transport = MockTransport::default();
    // More deliveries than the legacy fixed command capacity (256).
    for tag in 1..=600_u64 {
        transport.push_delivery(Ok(delivery(tag, b"payload")));
    }

    let sub = subscription(&transport, "s1", connection_key("b", "/"), 512, 0).await;
    let consumer = ConsumerSet::spawn_with_metrics(vec![sub], Metrics::default())
        .await
        .expect("consumer set with prefetch 512");

    // Let the pumps push everything and the actor fill the flume buffer.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let first = consumer.try_next_batch(256).expect("first batch");
    assert_eq!(first.len(), 256, "first full batch");
    let second = consumer.try_next_batch(256).expect("second batch");
    assert_eq!(second.len(), 256, "second full batch");

    let _ = consumer.close().await;
}

// ---------------------------------------------------------------------------
// Settlement lane and event-driven dispatch tests
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn slow_ack_does_not_block_incoming() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg1")));
    transport.push_delivery(Ok(delivery(2, b"msg2")));

    let sub = subscription(&transport, "s1", connection_key("b", "/"), 2, 0).await;
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    let d1 = consumer.next().await.expect("delivery 1");
    // Ack d1 — with default mock, ack is fast. But the key assertion is that
    // delivery 2 is available immediately after, without waiting for d1's ack.
    d1.ack().await.expect("ack1");

    let d2 = consumer.next().await.expect("delivery 2");
    assert_eq!(d2.payload.as_ref(), b"msg2");
    d2.ack().await.expect("ack2");
    consumer.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn settlements_on_same_channel_are_serialized() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg1")));
    transport.push_delivery(Ok(delivery(2, b"msg2")));
    // Gate the first ack so it blocks until we release it.
    let ack_gate = transport.push_ack_gate();
    let sub = subscription(&transport, "s1", connection_key("b", "/"), 2, 0).await;
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    let d1 = consumer.next().await.expect("d1");
    let d2 = consumer.next().await.expect("d2");

    // Start acking d1 — it will block at the gate.
    let ack1 = tokio::spawn(async move { d1.ack().await });
    // Let the settlement lane reach the gate.
    ack_gate.wait_entered().await;

    // The first Ack operation should already be recorded (the mock records
    // before applying the gate), but the second must NOT have been sent yet
    // because settlements on the same channel are serialized.
    let ops_before = transport.operations();
    let acks_before: Vec<_> = ops_before
        .iter()
        .filter(|op| matches!(op, TransportOperation::Ack { .. }))
        .collect();
    assert_eq!(
        acks_before.len(),
        1,
        "only the first ack should be in-flight on the same channel"
    );

    // Start acking d2 — it should queue behind d1's settlement, not execute.
    let ack2 = tokio::spawn(async move { d2.ack().await });

    // Give the actor a chance to process the second Settle command.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // The second ack must still not have been sent.
    let ops_still = transport.operations();
    let acks_still: Vec<_> = ops_still
        .iter()
        .filter(|op| matches!(op, TransportOperation::Ack { .. }))
        .collect();
    assert_eq!(
        acks_still.len(),
        1,
        "second ack must wait for the first to complete (same-channel serialization)"
    );

    // Release the gate — d1's ack completes, then d2's ack executes.
    let _ = ack_gate.release();
    ack1.await.expect("ack1 join").expect("ack1");
    ack2.await.expect("ack2 join").expect("ack2");

    // Let the second settlement complete.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    let ops_after = transport.operations();
    let acks_after: Vec<_> = ops_after
        .iter()
        .filter(|op| matches!(op, TransportOperation::Ack { .. }))
        .collect();
    assert_eq!(acks_after.len(), 2, "both acks must complete after release");
    consumer.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn close_works_with_pending_settlements() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg1")));
    let sub = subscription(&transport, "s1", connection_key("b", "/"), 1, 0).await;
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    let d1 = consumer.next().await.expect("d1");
    // Don't ack — just close
    drop(d1);
    consumer.close().await.expect("close should succeed");
}

#[tokio::test(start_paused = true)]
async fn close_resolves_within_deadline_with_hanging_channel() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg1")));
    // Gate the channel close so it would hang indefinitely without a deadline.
    let _close_gate = transport.push_close_channel_gate();
    let sub = subscription(&transport, "s1", connection_key("b", "/"), 1, 0).await;
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    let d1 = consumer.next().await.expect("d1");
    drop(d1);

    let close_result = tokio::time::timeout(Duration::from_secs(10), consumer.close()).await;
    assert!(
        close_result.is_ok(),
        "close should complete within deadline even if channel close hangs"
    );
}

// ---------------------------------------------------------------------------
// Early-ACK best-effort mode tests
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn early_ack_acks_before_dispatch_to_buffer() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg1")));

    let mut sub = subscription(&transport, "s1", connection_key("b", "/"), 1, 0).await;
    sub = sub.early_ack(true);
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    let_sources_fill().await;
    let d = consumer.next().await.expect("delivery");

    assert_eq!(d.state(), DeliveryState::AutoAcked);

    let ops = transport.operations();
    let acks: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, TransportOperation::Ack { .. }))
        .collect();
    assert_eq!(acks.len(), 1, "delivery should have been auto-acked");

    consumer.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn early_ack_does_not_block_subsequent_deliveries() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg1")));
    transport.push_delivery(Ok(delivery(2, b"msg2")));

    let mut sub = subscription(&transport, "s1", connection_key("b", "/"), 2, 0).await;
    sub = sub.early_ack(true);
    // Auto-acked deliveries must not hold back subsequent dispatches: both
    // deliveries flow through without any manual settlement.
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    let_sources_fill().await;
    let d1 = consumer.next().await.expect("delivery 1");
    let d2 = consumer.next().await.expect("delivery 2");

    assert_eq!(d1.state(), DeliveryState::AutoAcked);
    assert_eq!(d2.state(), DeliveryState::AutoAcked);

    consumer.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn early_ack_delivery_settle_returns_error() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"msg1")));

    let mut sub = subscription(&transport, "s1", connection_key("b", "/"), 1, 0).await;
    sub = sub.early_ack(true);
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    let_sources_fill().await;
    let d = consumer.next().await.expect("delivery");

    let err = d
        .ack()
        .await
        .expect_err("ack should fail on auto-acked delivery");
    assert_eq!(err.kind(), ConsumerErrorKind::AlreadySettled);

    let err = d.reject(false).await.expect_err("reject should fail");
    assert_eq!(err.kind(), ConsumerErrorKind::AlreadySettled);

    let err = d
        .release(Duration::ZERO)
        .await
        .expect_err("release should fail");
    assert_eq!(err.kind(), ConsumerErrorKind::AlreadySettled);

    consumer.close().await.expect("close");
}

#[tokio::test(start_paused = true)]
async fn early_ack_preserves_delivery_metadata() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery_with_properties(
        7,
        b"payload",
        "broker-msg-id",
        "trace-id",
    )));

    let mut sub = subscription(&transport, "s1", connection_key("b", "/"), 1, 0).await;
    sub = sub.early_ack(true);
    let consumer = ConsumerSet::spawn(vec![sub]).await.expect("consumer");

    let_sources_fill().await;
    let d = consumer.next().await.expect("delivery");

    assert_eq!(d.id.as_str(), "broker-msg-id");
    assert_eq!(d.correlation_id.as_deref(), Some("trace-id"));
    assert_eq!(d.subscription, SubscriptionId::new("s1"));
    assert_eq!(d.delivery_tag(), 7);

    consumer.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// OOM protection — hard gate tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn arc_headers_no_deep_clone() {
    use std::sync::Arc;
    let transport_delivery = helper::delivery(1, b"payload");
    let headers_arc = Arc::clone(&transport_delivery.headers);
    let _cloned = Arc::clone(&headers_arc);
}

// ---------------------------------------------------------------------------
// Fire-and-forget settlement tests
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn fire_and_forget_ack_returns_immediately() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"hello")));
    transport.push_delivery(Ok(delivery(2, b"world")));
    transport.push_consumer_result(Ok(()));

    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await;
    let handle = ConsumerSet::spawn_with_metrics(vec![subscription], Metrics::default())
        .await
        .unwrap();

    let delivery = handle.next().await.unwrap();

    let result = handle.try_settle(delivery.inner_token().clone(), Settlement::Ack);
    assert!(result.is_ok());

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let errors = handle.drain_errors();
    assert!(errors.is_empty());

    let _ = handle.close().await;
}

#[tokio::test(start_paused = true)]
async fn settlement_error_surfaces_via_drain_errors() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"hello")));
    transport.push_consumer_result(Ok(())); // set_qos
    transport.push_consumer_result(Ok(())); // consume
    transport.push_consumer_result(Err(TransportError::connection("test-stale-generation"))); // ack fails

    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await;
    let handle = ConsumerSet::spawn_with_metrics(vec![subscription], Metrics::default())
        .await
        .unwrap();

    let delivery = handle.next().await.unwrap();

    handle
        .try_settle(delivery.inner_token().clone(), Settlement::Ack)
        .unwrap();

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let errors = handle.drain_errors();
    assert!(!errors.is_empty(), "expected at least one settlement error");
    assert_eq!(errors[0].delivery_tag, 1);

    let _ = handle.close().await;
}

// ---------------------------------------------------------------------------
// no_ack mode tests
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn no_ack_propagates_to_transport_and_skips_ack_frames() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"hello")));

    let mut sub = subscription(&transport, "s1", connection_key("b", "/"), 1, 0).await;
    sub = sub.early_ack(true).no_ack(true);
    let handle = ConsumerSet::spawn_with_metrics(vec![sub], Metrics::default())
        .await
        .unwrap();

    let delivery = handle.next().await.unwrap();
    assert_eq!(delivery.state(), DeliveryState::AutoAcked);

    let ops = transport.operations();
    let consume_op = ops
        .iter()
        .find(|op| matches!(op, TransportOperation::Consume(_)));
    assert!(consume_op.is_some(), "expected Consume operation");
    if let Some(TransportOperation::Consume(request)) = consume_op {
        assert!(request.no_ack, "expected no_ack=true in ConsumerRequest");
    }

    let acks: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, TransportOperation::Ack { .. }))
        .collect();
    assert!(acks.is_empty(), "no_ack must not send ack frames");

    let _ = handle.close().await;
}

#[tokio::test(start_paused = true)]
async fn no_ack_defaults_to_false_in_consume_request() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery(1, b"hello")));

    let sub = subscription(&transport, "s1", connection_key("b", "/"), 1, 0).await;
    let handle = ConsumerSet::spawn_with_metrics(vec![sub], Metrics::default())
        .await
        .unwrap();

    let _ = handle.next().await;

    let ops = transport.operations();
    let consume_op = ops.iter().find_map(|op| match op {
        TransportOperation::Consume(req) => Some(req),
        _ => None,
    });
    assert!(consume_op.is_some(), "expected Consume operation");
    assert!(
        !consume_op.unwrap().no_ack,
        "no_ack should default to false"
    );

    let _ = handle.close().await;
}

#[tokio::test(start_paused = true)]
async fn settlement_errors_never_stall_the_actor_when_never_drained() {
    let transport = MockTransport::default();
    // 300 deliveries, each acknowledged with a failing ack. Each failure
    // produces a SettlementError; 300 > 256 (error channel capacity).
    transport.push_consumer_result(Ok(())); // set_qos
    transport.push_consumer_result(Ok(())); // consume
    for tag in 1..=300u64 {
        transport.push_delivery(Ok(delivery(tag, b"payload")));
        transport.push_consumer_result(Err(TransportError::connection("ack-failure")));
    }

    let subscription = subscription(&transport, "jobs", connection_key("jobs", "/"), 4, 0).await;
    let handle = ConsumerSet::spawn_with_metrics(vec![subscription], Metrics::default())
        .await
        .unwrap();

    // Consume and acknowledge the 300 messages without ever draining the
    // errors. The bounded error channel must never stall the actor loop.
    let consume_all = async {
        for tag in 1..=300u64 {
            let delivery = handle.next().await.expect("delivery must keep flowing");
            assert_eq!(delivery.delivery_tag(), tag);
            handle
                .try_settle(delivery.inner_token().clone(), Settlement::Ack)
                .expect("settle enqueued");
            tokio::time::advance(Duration::from_millis(1)).await;
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), consume_all)
        .await
        .expect("actor must not stall while settlement errors are never drained");

    // Let the last settlement result land before draining.
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    // The actor did not stall: the error buffer is full but bounded, with the
    // oldest errors dropped and the newest kept.
    let errors = handle.drain_errors();
    assert_eq!(errors.len(), 256, "oldest errors dropped, newest kept");
    assert_eq!(errors.last().expect("last error").delivery_tag, 300);

    let _ = handle.close().await;
}

// ---------------------------------------------------------------------------
// Consumer acquisition deadline (issue #45)
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn consumer_wait_deadline_expires_when_the_broker_never_becomes_ready() {
    let transport = Arc::new(MockTransport::default());
    // The connect gate is never released and no connect result is pushed: the
    // coordinator never leaves Connecting — a black-holed broker with no
    // connect timeout. The acquisition must fail at the deadline instead of
    // blocking forever.
    let _gate = transport.push_connect_gate();

    let config = Config {
        brokers: vec![broker("b", "/")],
        workers: vec![worker_profile("main", "b", "main.jobs")],
        topology_mode: TopologyMode::External,
        delay: rabbit_rs_core::config::DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
        publisher: PublisherConfigSection::default(),
        queue_type: QueueKind::Quorum,
        queue_durable: true,
        consumer: ConsumerConfigSection {
            // Minimum valid value (validation bounds wait_timeout to 1s..24h).
            wait_timeout: Duration::from_secs(1),
        },
    };

    let pool = ClientPool::new(
        Arc::new(config.validate().expect("valid config")),
        transport,
    );

    let started = tokio::time::Instant::now();
    let result = pool.consumer("main").await;

    let Err(error) = result else {
        panic!("acquisition must not succeed against a black-holed broker");
    };
    assert!(
        matches!(error.kind(), ClientErrorKind::Transport),
        "deadline expiry must surface as a typed transport error: {error:?}"
    );
    assert_eq!(started.elapsed(), Duration::from_secs(1));
}

// ---------------------------------------------------------------------------
// Lazy consumer establishment (issue #49)
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn only_requested_worker_profiles_are_consumed() {
    let transport = Arc::new(MockTransport::default());

    // Two declared worker profiles on the same broker: "main" (queue
    // main.jobs) and "side" (queue side.jobs).
    let config = Config {
        brokers: vec![broker("b", "/")],
        workers: vec![
            worker_profile("main", "b", "main.jobs"),
            worker_profile("side", "b", "side.jobs"),
        ],
        topology_mode: TopologyMode::External,
        delay: rabbit_rs_core::config::DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
        publisher: PublisherConfigSection::default(),
        consumer: ConsumerConfigSection::default(),
        queue_type: QueueKind::Quorum,
        queue_durable: true,
    };

    let pool = ClientPool::new(
        Arc::new(config.validate().expect("valid config")),
        transport.clone(),
    );

    // The process only requests the "main" profile.
    let _handle = pool.consumer("main").await.expect("main consumer");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let operations = transport.operations();
    let consumed_queues: Vec<&str> = operations
        .iter()
        .filter_map(|operation| match operation {
            TransportOperation::Consume(request) => Some(request.queue.as_str()),
            _ => None,
        })
        .collect();

    assert!(
        consumed_queues.contains(&"main.jobs"),
        "requested profile consumed: {consumed_queues:?}"
    );
    assert!(
        !consumed_queues.contains(&"side.jobs"),
        "unrequested profile must not be consumed: {consumed_queues:?}"
    );

    pool.close().await.expect("close pool");
}

#[tokio::test(start_paused = true)]
async fn profile_requested_after_a_publishing_phase_is_established_on_demand() {
    let transport = Arc::new(MockTransport::default());

    let config = Config {
        brokers: vec![broker("b", "/")],
        workers: vec![
            worker_profile("main", "b", "main.jobs"),
            worker_profile("side", "b", "side.jobs"),
        ],
        topology_mode: TopologyMode::External,
        delay: rabbit_rs_core::config::DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
        publisher: PublisherConfigSection::default(),
        consumer: ConsumerConfigSection::default(),
        queue_type: QueueKind::Quorum,
        queue_durable: true,
    };

    let pool = ClientPool::new(
        Arc::new(config.validate().expect("valid config")),
        transport.clone(),
    );

    // A publishing phase starts the coordinator: generation 1 runs with no
    // requested profile, so no queue is consumed.
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let properties = MessageProperties::new("msg-1");
    let request = PublishRequest::new(
        Destination::new("main.jobs", "main.jobs"),
        Bytes::from_static(b"payload"),
        properties,
        tokio::time::Instant::now() + Duration::from_secs(30),
    );
    pool.publish("b", request).await.expect("publish");
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let operations_before = transport.operations();
    assert!(
        !operations_before
            .iter()
            .any(|operation| matches!(operation, TransportOperation::Consume(_))),
        "no queue is consumed while no profile has been requested"
    );

    // The "main" profile is requested afterwards: it must be established
    // without waiting for a reconnection, and "side" must stay dormant.
    let _handle = pool.consumer("main").await.expect("main consumer");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let operations = transport.operations();
    let consumed_queues: Vec<&str> = operations
        .iter()
        .filter_map(|operation| match operation {
            TransportOperation::Consume(request) => Some(request.queue.as_str()),
            _ => None,
        })
        .collect();

    assert!(
        consumed_queues.contains(&"main.jobs"),
        "requested profile consumed: {consumed_queues:?}"
    );
    assert!(
        !consumed_queues.contains(&"side.jobs"),
        "unrequested profile must not be consumed: {consumed_queues:?}"
    );

    pool.close().await.expect("close pool");
}
