//! Poison-message terminal settlement (issue #70 — audit F-07 + F-11).
//!
//! Deliveries whose resolved attempts exceed the configured maximum must be
//! settled terminally instead of being dispatched with fabricated attempt
//! counts, and delayed releases at the cap must settle the original delivery
//! instead of leaving it pending in a hot redelivery loop.

use std::{collections::BTreeMap, num::NonZeroU32, sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    config::SafetyMode,
    consumer::{
        APPLICATION_ATTEMPTS_HEADER, ConsumerErrorKind, ConsumerSet, DeliveryState, Subscription,
        SubscriptionPolicy,
    },
    metrics::Metrics,
    pool::ConnectionKey,
    publisher::{Destination, PublisherActor, PublisherConfig, PublisherHandle},
    topology::delay::DelayStrategy,
    transport::{
        Delivery as TransportDelivery, HeaderValue, Transport,
        mock::{MockTransport, TransportOperation},
    },
};

mod common;

mod helper {
    use super::*;

    pub fn broker(name: &str, vhost: &str) -> rabbit_rs_core::config::BrokerConfig {
        crate::common::broker(name, vhost, "guest")
    }

    pub fn connection_key(name: &str, vhost: &str) -> ConnectionKey {
        use rabbit_rs_core::{
            config::{Config, ConsumerConfigSection, PublisherConfigSection, TopologyMode},
            transport::QueueKind,
        };
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

    pub fn delivery_with_attempts(tag: u64, attempts: &str) -> TransportDelivery {
        let mut headers = BTreeMap::new();
        headers.insert(
            APPLICATION_ATTEMPTS_HEADER.to_owned(),
            HeaderValue::Binary(Bytes::copy_from_slice(attempts.as_bytes())),
        );
        TransportDelivery {
            delivery_tag: tag,
            exchange: "jobs".to_owned(),
            routing_key: "high".to_owned(),
            redelivered: false,
            message_id: None,
            correlation_id: None,
            headers: Arc::new(headers),
            payload: Bytes::from_static(b"poison"),
        }
    }

    pub async fn subscription(
        transport: &MockTransport,
        id: &str,
        key: ConnectionKey,
    ) -> Subscription {
        let channel = transport
            .connect(&broker(id, "/"))
            .await
            .expect("connection")
            .open_consumer()
            .await
            .expect("consumer channel");
        Subscription::new(id, key, format!("queue.{id}"), Arc::from(channel))
            .prefetch(4)
            .channel_id(1)
            .policy(SubscriptionPolicy::new(1, 0, Duration::from_secs(1)))
    }

    pub async fn publisher(transport: &MockTransport) -> PublisherHandle {
        let channel = transport
            .connect(&broker("publisher", "/"))
            .await
            .expect("connection")
            .open_publisher()
            .await
            .expect("publisher channel");
        PublisherActor::spawn_with_delay_strategy_and_metrics(
            Arc::from(channel),
            PublisherConfig::with_safety(32, Duration::from_secs(5), SafetyMode::Safe),
            Metrics::default(),
            None,
        )
    }

    pub async fn let_actor_process() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    pub fn ack_operations(transport: &MockTransport, delivery_tag: u64) -> usize {
        transport
            .operations()
            .iter()
            .filter(|op| matches!(op, TransportOperation::Ack { delivery_tag: tag, .. } if *tag == delivery_tag))
            .count()
    }

    pub fn reject_operations(transport: &MockTransport, delivery_tag: u64, requeue: bool) -> usize {
        transport
            .operations()
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    TransportOperation::Reject { delivery_tag: tag, requeue: r }
                        if *tag == delivery_tag && *r == requeue
                )
            })
            .count()
    }

    pub fn publish_operations(transport: &MockTransport) -> usize {
        transport
            .operations()
            .iter()
            .filter(|op| matches!(op, TransportOperation::Publish(_)))
            .count()
    }
}

use helper::*;

// ---------------------------------------------------------------------------
// Dispatch-time attempts above the cap
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn poison_delivery_without_a_dlx_is_settled_with_an_explicit_documented_ack() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery_with_attempts(1, "25")));
    let consumer = ConsumerSet::spawn_with_metrics(
        vec![subscription(&transport, "jobs", connection_key("jobs", "/")).await],
        Metrics::default(),
    )
    .await
    .expect("consumer set");
    let_actor_process().await;

    assert!(
        consumer.try_next().expect("consumer open").is_none(),
        "a delivery above the attempt cap must never be dispatched"
    );
    assert_eq!(
        ack_operations(&transport, 1),
        1,
        "without a DLX the documented policy is an explicit ack-and-log"
    );
    let errors = consumer.drain_errors();
    assert_eq!(errors.len(), 1, "the resolve error must surface");
    assert_eq!(errors[0].kind, ConsumerErrorKind::MaxAttempts);
    assert!(
        errors[0]
            .message
            .contains("exceeds the configured maximum of 20"),
        "the error must carry the true attempt count, got: {}",
        errors[0].message
    );
    assert!(
        errors[0].message.contains("acknowledged"),
        "the explicit-loss action must be documented in the error, got: {}",
        errors[0].message
    );
}

#[tokio::test(start_paused = true)]
async fn poison_delivery_with_a_dlx_is_dead_lettered_with_requeue_false() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery_with_attempts(1, "25")));
    let consumer = ConsumerSet::spawn_with_metrics(
        vec![
            subscription(&transport, "jobs", connection_key("jobs", "/"))
                .await
                .dead_letter(true),
        ],
        Metrics::default(),
    )
    .await
    .expect("consumer set");
    let_actor_process().await;

    assert!(
        consumer.try_next().expect("consumer open").is_none(),
        "a delivery above the attempt cap must never be dispatched"
    );
    assert_eq!(
        reject_operations(&transport, 1, false),
        1,
        "with a DLX the delivery is rejected with requeue=false so the broker dead-letters it"
    );
    assert_eq!(
        ack_operations(&transport, 1),
        0,
        "no ack when dead-lettering"
    );
}

#[tokio::test(start_paused = true)]
async fn configured_max_attempts_dispatches_deliveries_beyond_the_default_cap() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery_with_attempts(1, "25")));
    let consumer = ConsumerSet::spawn_with_metrics(
        vec![
            subscription(&transport, "jobs", connection_key("jobs", "/"))
                .await
                .max_attempts(NonZeroU32::new(30)),
        ],
        Metrics::default(),
    )
    .await
    .expect("consumer set");
    let_actor_process().await;

    let item = consumer.next().await.expect("delivery dispatched");
    assert_eq!(
        item.attempts, 25,
        "the resolved attempts value is preserved"
    );
    assert_eq!(item.state(), DeliveryState::Pending);
}

// ---------------------------------------------------------------------------
// Delayed release at the cap
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn delayed_release_at_the_cap_settles_the_original_instead_of_requeueing_it() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery_with_attempts(7, "20")));
    let publisher = publisher(&transport).await;
    let consumer = ConsumerSet::spawn_with_metrics(
        vec![
            subscription(&transport, "jobs", connection_key("jobs", "/"))
                .await
                .delayed_publisher(publisher, Destination::new("jobs", "high"))
                .delay_strategy(DelayStrategy::Plugin),
        ],
        Metrics::default(),
    )
    .await
    .expect("consumer set");
    let item = consumer.next().await.expect("delivery");
    assert_eq!(item.attempts, 20);

    item.release(Duration::from_secs(5))
        .await
        .expect("release enqueued (fire-and-forget)");
    tokio::time::advance(Duration::from_millis(10)).await;
    let_actor_process().await;

    assert_eq!(
        item.state(),
        DeliveryState::Acked,
        "the capped delayed release must settle the original delivery terminally"
    );
    assert_eq!(
        publish_operations(&transport),
        0,
        "no republish may happen at the cap"
    );
    assert_eq!(
        ack_operations(&transport, 7),
        1,
        "without a DLX the documented policy is an explicit ack-and-log"
    );
    let errors = consumer.drain_errors();
    assert!(
        errors.iter().any(|error| {
            error.kind == ConsumerErrorKind::MaxAttempts
                && error
                    .message
                    .contains("exceeds the configured maximum of 20")
        }),
        "the capped release must surface the MaxAttempts error, got: {errors:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn delayed_release_at_the_cap_with_a_dlx_dead_letters_the_original() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(delivery_with_attempts(7, "20")));
    let publisher = publisher(&transport).await;
    let consumer = ConsumerSet::spawn_with_metrics(
        vec![
            subscription(&transport, "jobs", connection_key("jobs", "/"))
                .await
                .dead_letter(true)
                .delayed_publisher(publisher, Destination::new("jobs", "high"))
                .delay_strategy(DelayStrategy::Plugin),
        ],
        Metrics::default(),
    )
    .await
    .expect("consumer set");
    let item = consumer.next().await.expect("delivery");

    item.release(Duration::from_secs(5))
        .await
        .expect("release enqueued (fire-and-forget)");
    tokio::time::advance(Duration::from_millis(10)).await;
    let_actor_process().await;

    assert_eq!(item.state(), DeliveryState::Rejected);
    assert_eq!(
        reject_operations(&transport, 7, false),
        1,
        "with a DLX the capped release dead-letters the original delivery"
    );
    assert_eq!(
        ack_operations(&transport, 7),
        0,
        "no ack when dead-lettering"
    );
    assert_eq!(publish_operations(&transport), 0, "no republish at the cap");
}
