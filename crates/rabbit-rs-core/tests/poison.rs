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
        // These tests exercise poison settlement, not subscription death:
        // keep the delivery stream open like a live broker subscription so
        // the scripted deliveries end without surfacing a terminal
        // stream-ended error (see tests/transport_liveness.rs).
        transport.keep_delivery_stream_open();
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

// ---------------------------------------------------------------------------
// Real-broker verification (requires the RabbitMQ lab)
// ---------------------------------------------------------------------------

#[cfg(feature = "integration")]
mod real_broker {
    use super::*;
    use rabbit_rs_core::{
        client::ClientPool,
        config::{
            ConsumerConfigSection, Credentials, DeadLetterConfig, DelayConfig, Endpoint,
            PublisherConfigSection, SchedulerConfig, SubscriptionConfig, TlsConfig, TopologyMode,
            WorkerProfile,
        },
        publisher::{MessageProperties, PublishRequest},
        topology::{
            DeadLetterDefinition, QueueDefinition, TopologyDefinition, TopologyPlan,
            TopologyReconciler,
        },
        transport::{QueueKind, lapin::LapinTransport},
    };

    fn lab_broker(name: &str, vhost: &str) -> rabbit_rs_core::config::BrokerConfig {
        rabbit_rs_core::config::BrokerConfig {
            name: name.to_owned(),
            hosts: vec![Endpoint::new("localhost", 5672)],
            vhost: vhost.to_owned(),
            credentials: Credentials::new("rabbit_rs", "rabbit_rs_lab"),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        }
    }

    fn unique_suffix() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos()
            .to_string()
    }

    async fn declare(vhost: &str, source: &str, dead_letter: Option<(&str, &str, &str)>) {
        let conn = LapinTransport
            .connect(&lab_broker("topology", vhost))
            .await
            .expect("connect to lab");
        let channel = conn.open_publisher().await.expect("publisher channel");

        let mut definition =
            TopologyDefinition::new(vec![], vec![QueueDefinition::new(source)], vec![]);
        if let Some((dlx, dlq, routing_key)) = dead_letter {
            definition = definition.with_dead_letter(DeadLetterDefinition::new(
                source,
                dlx,
                dlq,
                routing_key,
            ));
        }
        let plan = TopologyPlan::compile(TopologyMode::Declare, definition).expect("compile plan");
        TopologyReconciler::new()
            .reconcile(channel.as_ref(), &plan, 1)
            .await
            .expect("declare topology");

        channel.close().await.expect("close channel");
        conn.close().await.expect("close connection");
    }

    fn pool_config(
        queue: &str,
        dead_letter: Option<DeadLetterConfig>,
    ) -> Arc<rabbit_rs_core::config::ValidatedConfig> {
        Arc::new(
            rabbit_rs_core::config::Config {
                brokers: vec![lab_broker("primary", "/orders-eu")],
                workers: vec![WorkerProfile {
                    name: "main".to_owned(),
                    subscriptions: vec![SubscriptionConfig {
                        name: "poison".to_owned(),
                        broker: "primary".to_owned(),
                        queue: queue.to_owned(),
                        weight: 1,
                        priority_class: 0,
                        prefetch: 8,
                        starvation_after: Duration::from_secs(30),
                        max_buffered_bytes: 64 * 1024 * 1024,
                        early_ack: false,
                        no_ack: false,
                    }],
                    scheduler: SchedulerConfig::weighted_fair(),
                }],
                topology_mode: TopologyMode::External,
                delay: DelayConfig::default(),
                dead_letter,
                delivery_limit: None,
                publisher: PublisherConfigSection::default(),
                consumer: ConsumerConfigSection::default(),
                queue_type: QueueKind::Quorum,
                queue_durable: true,
            }
            .validate()
            .expect("valid config"),
        )
    }

    fn poison_request(message_id: &str, queue: &str) -> PublishRequest {
        let mut headers = rabbit_rs_core::transport::Headers::new();
        headers.insert(
            APPLICATION_ATTEMPTS_HEADER.to_owned(),
            HeaderValue::Binary(Bytes::from_static(b"25")),
        );
        let mut properties = MessageProperties::new(message_id);
        properties.headers = headers;
        PublishRequest::new(
            Destination::new("", queue),
            Bytes::from_static(b"poison"),
            properties,
            tokio::time::Instant::now() + Duration::from_secs(30),
        )
    }

    #[tokio::test]
    async fn real_broker_poison_with_a_dlx_dead_letters_and_never_dispatches() {
        let suffix = unique_suffix();
        let source = format!("rabbit-rs-it-poison-dlx-{suffix}");
        let dlx = format!("{source}.dlx");
        let dlq = format!("{source}.failed");
        declare("/orders-eu", &source, Some((&dlx, &dlq, ""))).await;

        let pool = ClientPool::production(pool_config(
            &source,
            Some(DeadLetterConfig {
                enabled: true,
                exchange: dlx.clone(),
                queue: dlq.clone(),
                routing_key: Some(String::new()),
            }),
        ));

        pool.publish_batch(vec![(
            "primary".to_owned(),
            poison_request(&format!("poison-dlx-{suffix}"), &source),
        )])
        .await
        .expect("publish poison");

        let consumer = pool.consumer("main").await.expect("consumer");
        let dispatched = tokio::time::timeout(Duration::from_secs(2), consumer.next()).await;
        assert!(
            dispatched.is_err(),
            "a poison delivery must never be dispatched to the application"
        );

        assert_eq!(
            pool.queue_size("primary", &source)
                .await
                .expect("source size"),
            0,
            "the poison delivery must be settled out of the source queue"
        );
        assert_eq!(
            pool.queue_size("primary", &dlq).await.expect("dlq size"),
            1,
            "with a DLX the poison delivery must be dead-lettered"
        );
        let errors = consumer.drain_errors();
        assert!(
            errors
                .iter()
                .any(|error| error.kind == ConsumerErrorKind::MaxAttempts),
            "the poison settlement must surface a MaxAttempts error, got: {errors:?}"
        );

        pool.close().await.expect("close pool");
    }

    #[tokio::test]
    async fn real_broker_poison_without_a_dlx_is_acked_and_reported() {
        let suffix = unique_suffix();
        let source = format!("rabbit-rs-it-poison-ack-{suffix}");
        declare("/orders-eu", &source, None).await;

        let pool = ClientPool::production(pool_config(&source, None));

        pool.publish_batch(vec![(
            "primary".to_owned(),
            poison_request(&format!("poison-ack-{suffix}"), &source),
        )])
        .await
        .expect("publish poison");

        let consumer = pool.consumer("main").await.expect("consumer");
        let dispatched = tokio::time::timeout(Duration::from_secs(2), consumer.next()).await;
        assert!(
            dispatched.is_err(),
            "a poison delivery must never be dispatched to the application"
        );

        assert_eq!(
            pool.queue_size("primary", &source)
                .await
                .expect("source size"),
            0,
            "without a DLX the documented policy is an explicit ack-and-log"
        );
        let errors = consumer.drain_errors();
        assert!(
            errors
                .iter()
                .any(|error| error.kind == ConsumerErrorKind::MaxAttempts),
            "the poison settlement must surface a MaxAttempts error, got: {errors:?}"
        );

        pool.close().await.expect("close pool");
    }

    #[tokio::test]
    async fn real_broker_shared_dead_letter_queue_routes_per_source_routing_keys() {
        // Two source queues share ONE dead-letter queue with per-source
        // default routing keys. Before the F-05 fix only the first
        // (dlq, routing_key) binding was declared, so a poison message from
        // the second subscription was silently dropped by the DLX (its
        // republish is not mandatory). This test dead-letters through the
        // SECOND binding end to end.
        let suffix = unique_suffix();
        let (source_one, source_two, dlx, dlq) = declare_shared_dlq_topology(&suffix).await;

        let pool = ClientPool::production(shared_dlq_pool_config(
            source_one.clone(),
            source_two.clone(),
            dlx.clone(),
            dlq.clone(),
        ));

        // Poison the SECOND subscription: it must be dead-lettered into the
        // shared DLQ through the second (dlq, routing_key) binding.
        pool.publish_batch(vec![(
            "primary".to_owned(),
            poison_request(&format!("dlq-share-{suffix}"), &source_two),
        )])
        .await
        .expect("publish poison to the second source");

        let consumer = pool.consumer("main").await.expect("consumer");
        let dispatched = tokio::time::timeout(Duration::from_secs(2), consumer.next()).await;
        assert!(
            dispatched.is_err(),
            "a poison delivery must never be dispatched to the application",
        );

        assert_eq!(
            pool.queue_size("primary", &source_two)
                .await
                .expect("source two size"),
            0,
            "the poison delivery must be settled out of the source queue",
        );
        assert_eq!(
            pool.queue_size("primary", &dlq).await.expect("dlq size"),
            1,
            "the second subscription's dead-letter routing key must reach the shared DLQ",
        );
        let errors = consumer.drain_errors();
        assert!(
            errors
                .iter()
                .any(|error| error.kind == ConsumerErrorKind::MaxAttempts),
            "the poison settlement must surface a MaxAttempts error, got: {errors:?}",
        );

        pool.close().await.expect("close pool");
    }

    /// Declares two source queues and a DLX/DLQ pair shared by both, one
    /// dead-letter binding per (DLQ, routing key) pair, against the lab.
    async fn declare_shared_dlq_topology(suffix: &str) -> (String, String, String, String) {
        let source_one = format!("rabbit-rs-it-dlq-share-a-{suffix}");
        let source_two = format!("rabbit-rs-it-dlq-share-b-{suffix}");
        let dlx = format!("rabbit-rs-it-dlq-share-{suffix}.dlx");
        let dlq = format!("rabbit-rs-it-dlq-share-{suffix}.failed");

        let conn = LapinTransport
            .connect(&lab_broker("topology", "/orders-eu"))
            .await
            .expect("connect to lab");
        let channel = conn.open_publisher().await.expect("publisher channel");

        let definition = TopologyDefinition::new(
            vec![],
            vec![
                QueueDefinition::new(&source_one),
                QueueDefinition::new(&source_two),
            ],
            vec![],
        )
        .with_dead_letter(DeadLetterDefinition::new(
            &source_one,
            &dlx,
            &dlq,
            &source_one,
        ))
        .with_dead_letter(DeadLetterDefinition::new(
            &source_two,
            &dlx,
            &dlq,
            &source_two,
        ));
        let plan = TopologyPlan::compile(TopologyMode::Declare, definition).expect("compile plan");
        assert_eq!(
            plan.bindings()
                .iter()
                .filter(|binding| binding.queue == dlq)
                .count(),
            2,
            "one binding per (dlq, routing_key) pair in the compiled plan",
        );

        TopologyReconciler::new()
            .reconcile(channel.as_ref(), &plan, 1)
            .await
            .expect("declare shared-dlq topology");

        channel.close().await.expect("close channel");
        conn.close().await.expect("close connection");

        (source_one, source_two, dlx, dlq)
    }

    /// Pool with one worker subscribing to both sources, dead-lettering into
    /// the shared DLQ (default per-source routing keys, `TopologyMode::External`).
    fn shared_dlq_pool_config(
        source_one: String,
        source_two: String,
        dlx: String,
        dlq: String,
    ) -> Arc<rabbit_rs_core::config::ValidatedConfig> {
        let subscription = |name: &str, queue: String| SubscriptionConfig {
            name: name.to_owned(),
            broker: "primary".to_owned(),
            queue,
            weight: 1,
            priority_class: 0,
            prefetch: 8,
            starvation_after: Duration::from_secs(30),
            max_buffered_bytes: 64 * 1024 * 1024,
            early_ack: false,
            no_ack: false,
        };

        Arc::new(
            rabbit_rs_core::config::Config {
                brokers: vec![lab_broker("primary", "/orders-eu")],
                workers: vec![WorkerProfile {
                    name: "main".to_owned(),
                    subscriptions: vec![
                        subscription("share-a", source_one),
                        subscription("share-b", source_two),
                    ],
                    scheduler: SchedulerConfig::weighted_fair(),
                }],
                topology_mode: TopologyMode::External,
                delay: DelayConfig::default(),
                dead_letter: Some(DeadLetterConfig {
                    enabled: true,
                    exchange: dlx,
                    queue: dlq,
                    routing_key: None,
                }),
                delivery_limit: None,
                publisher: PublisherConfigSection::default(),
                consumer: ConsumerConfigSection::default(),
                queue_type: QueueKind::Quorum,
                queue_durable: true,
            }
            .validate()
            .expect("valid config"),
        )
    }
}
