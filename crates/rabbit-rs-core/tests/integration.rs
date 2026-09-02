mod common;

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use rabbit_rs_core::{
    client::{ClientErrorKind, ClientPool},
    config::{
        BrokerConfig, Config, Credentials, Endpoint, PublisherConfigSection, SchedulerConfig,
        SubscriptionConfig, TlsConfig, TopologyMode, WorkerProfile,
    },
    consumer::{SubscriptionId, SubscriptionPolicy, WeightedFairScheduler},
    publisher::{Destination, MessageProperties, PublishOutcome, PublishRequest},
    transport::{
        Delivery as TransportDelivery, PublishConfirmation, QueueKind, TransportError,
        mock::{MockTransport, TransportOperation},
    },
};

mod helper {
    use super::*;

    pub fn config() -> rabbit_rs_core::config::ValidatedConfig {
        Config {
            brokers: vec![BrokerConfig {
                name: "default".to_owned(),
                hosts: vec![Endpoint::new("rabbit.local", 5672)],
                vhost: "/".to_owned(),
                credentials: Credentials::new("guest", "secret"),
                tls: TlsConfig::disabled(),
                heartbeat: Duration::from_secs(30),
            }],
            workers: Vec::new(),
            topology_mode: TopologyMode::External,
            delay: rabbit_rs_core::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
            consumer: rabbit_rs_core::config::ConsumerConfigSection::default(),
            queue_type: QueueKind::Quorum,
            queue_durable: true,
        }
        .validate()
        .expect("valid config")
    }

    pub fn consumer_config() -> rabbit_rs_core::config::ValidatedConfig {
        Config {
            brokers: vec![BrokerConfig {
                name: "default".to_owned(),
                hosts: vec![Endpoint::new("rabbit.local", 5672)],
                vhost: "/".to_owned(),
                credentials: Credentials::new("guest", "secret"),
                tls: TlsConfig::disabled(),
                heartbeat: Duration::from_secs(30),
            }],
            workers: vec![WorkerProfile {
                name: "main".to_owned(),
                subscriptions: vec![SubscriptionConfig {
                    name: "jobs".to_owned(),
                    broker: "default".to_owned(),
                    queue: "jobs".to_owned(),
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
            delay: rabbit_rs_core::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
            consumer: rabbit_rs_core::config::ConsumerConfigSection::default(),
            queue_type: QueueKind::Quorum,
            queue_durable: true,
        }
        .validate()
        .expect("valid consumer config")
    }

    pub fn two_broker_config() -> rabbit_rs_core::config::ValidatedConfig {
        Config {
            brokers: ["first", "second"]
                .into_iter()
                .map(|name| BrokerConfig {
                    name: name.to_owned(),
                    hosts: vec![Endpoint::new(format!("{name}.rabbit.local"), 5672)],
                    vhost: "/".to_owned(),
                    credentials: Credentials::new("guest", "secret"),
                    tls: TlsConfig::disabled(),
                    heartbeat: Duration::from_secs(30),
                })
                .collect(),
            workers: Vec::new(),
            topology_mode: TopologyMode::External,
            delay: rabbit_rs_core::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
            consumer: rabbit_rs_core::config::ConsumerConfigSection::default(),
            queue_type: QueueKind::Quorum,
            queue_durable: true,
        }
        .validate()
        .expect("valid two-broker config")
    }

    pub fn multi_broker_config() -> rabbit_rs_core::config::ValidatedConfig {
        Config {
            brokers: ["first", "second"]
                .into_iter()
                .map(|name| BrokerConfig {
                    name: name.to_owned(),
                    hosts: vec![Endpoint::new(format!("{name}.rabbit.local"), 5672)],
                    vhost: "/".to_owned(),
                    credentials: Credentials::new("guest", "secret"),
                    tls: TlsConfig::disabled(),
                    heartbeat: Duration::from_secs(30),
                })
                .collect(),
            workers: vec![WorkerProfile {
                name: "multi-broker-worker".to_owned(),
                subscriptions: vec![
                    SubscriptionConfig {
                        name: "jobs-first".to_owned(),
                        broker: "first".to_owned(),
                        queue: "jobs-first".to_owned(),
                        weight: 1,
                        priority_class: 0,
                        prefetch: 8,
                        starvation_after: Duration::from_secs(30),
                        max_buffered_bytes: 64 * 1024 * 1024,
                        early_ack: false,
                        no_ack: false,
                    },
                    SubscriptionConfig {
                        name: "jobs-second".to_owned(),
                        broker: "second".to_owned(),
                        queue: "jobs-second".to_owned(),
                        weight: 1,
                        priority_class: 0,
                        prefetch: 8,
                        starvation_after: Duration::from_secs(30),
                        max_buffered_bytes: 64 * 1024 * 1024,
                        early_ack: false,
                        no_ack: false,
                    },
                ],
                scheduler: SchedulerConfig::weighted_fair(),
            }],
            topology_mode: TopologyMode::External,
            delay: rabbit_rs_core::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
            consumer: rabbit_rs_core::config::ConsumerConfigSection::default(),
            queue_type: rabbit_rs_core::transport::QueueKind::Quorum,
            queue_durable: true,
        }
        .validate()
        .expect("valid multi-broker consumer config")
    }

    pub fn request(message_id: &str) -> PublishRequest {
        PublishRequest::new(
            Destination::new("jobs", "default"),
            Bytes::from_static(b"payload"),
            MessageProperties::new(message_id),
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
    }

    pub fn sched_id(value: &str) -> SubscriptionId {
        SubscriptionId::new(value)
    }

    pub fn policy(weight: u16, priority_class: i16) -> SubscriptionPolicy {
        SubscriptionPolicy::new(weight, priority_class, Duration::from_secs(1))
    }
}

use helper::*;

// ---------------------------------------------------------------------------
// Scheduler fairness tests (from scheduler_fairness.rs)
// ---------------------------------------------------------------------------

#[test]
fn selects_the_only_ready_subscription() {
    let now = Instant::now();
    let mut scheduler = WeightedFairScheduler::default();
    scheduler.register(sched_id("only"), policy(1, 0));
    scheduler.mark_ready(&sched_id("only"));

    assert_eq!(scheduler.next(now), Some(sched_id("only")));
}

#[test]
fn follows_configured_weight_ratio() {
    let now = Instant::now();
    let mut scheduler = WeightedFairScheduler::default();
    scheduler.register(sched_id("high-weight"), policy(8, 0));
    scheduler.register(sched_id("low-weight"), policy(2, 0));
    scheduler.mark_ready(&sched_id("high-weight"));
    scheduler.mark_ready(&sched_id("low-weight"));

    let mut counts = BTreeMap::new();
    for _ in 0..10_000 {
        *counts.entry(scheduler.next(now).unwrap()).or_insert(0_u32) += 1;
    }

    assert_eq!(counts[&sched_id("high-weight")], 8_000);
    assert_eq!(counts[&sched_id("low-weight")], 2_000);
}

#[test]
fn empty_subscription_does_not_accumulate_credit() {
    let now = Instant::now();
    let mut scheduler = WeightedFairScheduler::default();
    scheduler.register(sched_id("temporarily-empty"), policy(1, 0));
    scheduler.register(sched_id("always-ready"), policy(1, 0));
    scheduler.mark_ready(&sched_id("temporarily-empty"));
    scheduler.mark_ready(&sched_id("always-ready"));

    let first = scheduler.next(now).unwrap();
    scheduler.mark_empty(&sched_id("temporarily-empty"));
    for _ in 0..100 {
        assert_eq!(scheduler.next(now), Some(sched_id("always-ready")));
    }

    scheduler.mark_ready(&sched_id("temporarily-empty"));
    let resumed = [scheduler.next(now).unwrap(), scheduler.next(now).unwrap()];

    assert!(resumed.contains(&sched_id("temporarily-empty")));
    assert!(resumed.contains(&sched_id("always-ready")));
    assert!(first == sched_id("temporarily-empty") || first == sched_id("always-ready"));
}

#[test]
fn subscription_can_return_after_being_empty() {
    let now = Instant::now();
    let mut scheduler = WeightedFairScheduler::default();
    scheduler.register(sched_id("queue"), policy(1, 0));
    scheduler.mark_ready(&sched_id("queue"));
    scheduler.mark_empty(&sched_id("queue"));

    assert_eq!(scheduler.next(now), None);

    scheduler.mark_ready(&sched_id("queue"));

    assert_eq!(scheduler.next(now), Some(sched_id("queue")));
}

#[test]
fn aging_prevents_lower_priority_starvation() {
    let start = Instant::now();
    let mut scheduler = WeightedFairScheduler::default();
    scheduler.register(sched_id("high"), policy(1, 3));
    scheduler.register(sched_id("low"), policy(1, 0));
    scheduler.mark_ready(&sched_id("high"));
    scheduler.mark_ready(&sched_id("low"));

    let selected = (0..=6)
        .map(|seconds| {
            scheduler
                .next(start + Duration::from_secs(seconds))
                .unwrap()
        })
        .collect::<Vec<_>>();

    assert_eq!(selected[0], sched_id("high"));
    assert!(selected.contains(&sched_id("low")));
}

#[test]
fn produces_a_deterministic_sequence() {
    let start = Instant::now();
    let mut first = WeightedFairScheduler::default();
    let mut second = WeightedFairScheduler::default();

    for scheduler in [&mut first, &mut second] {
        scheduler.register(sched_id("a"), policy(5, 0));
        scheduler.register(sched_id("b"), policy(3, 0));
        scheduler.register(sched_id("c"), policy(1, 0));
        scheduler.mark_ready(&sched_id("a"));
        scheduler.mark_ready(&sched_id("b"));
        scheduler.mark_ready(&sched_id("c"));
    }

    let first_sequence = (0..100)
        .map(|tick| first.next(start + Duration::from_millis(tick)).unwrap())
        .collect::<Vec<_>>();
    let second_sequence = (0..100)
        .map(|tick| second.next(start + Duration::from_millis(tick)).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(first_sequence, second_sequence);
}

// ---------------------------------------------------------------------------
// Client pool tests (from client_pool.rs — pruned to ~8 representative tests)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reuses_one_connection_and_publisher_for_confirmed_messages() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let pool = ClientPool::new(Arc::new(config()), transport.clone());

    let first = pool
        .publish_batch(vec![("default".to_owned(), request("first"))])
        .await
        .expect("first publish")
        .pop()
        .expect("first publish");
    let second = pool
        .publish_batch(vec![("default".to_owned(), request("second"))])
        .await
        .expect("second publish")
        .pop()
        .expect("second publish");

    assert_eq!(
        first,
        PublishOutcome::Confirmed {
            message_id: "first".into()
        }
    );
    assert_eq!(
        second,
        PublishOutcome::Confirmed {
            message_id: "second".into()
        }
    );
    let operations = transport.operations();
    assert_eq!(
        operations
            .iter()
            .filter(|operation| matches!(operation, TransportOperation::Connect { .. }))
            .count(),
        1
    );
    assert_eq!(
        operations
            .iter()
            .filter(|operation| matches!(operation, TransportOperation::OpenPublisher))
            .count(),
        1
    );
    assert_eq!(pool.metrics_snapshot().publishes_total, 2);

    pool.close().await.expect("close pool");
    assert!(pool.is_closed());
}

#[tokio::test(start_paused = true)]
async fn batch_enqueues_all_messages_before_waiting_for_confirms() {
    let transport = Arc::new(MockTransport::default());
    let first = transport.push_controlled_confirmation();
    let second = transport.push_controlled_confirmation();
    let pool = Arc::new(ClientPool::new(Arc::new(config()), transport.clone()));
    let publishing = tokio::spawn({
        let pool = pool.clone();
        async move {
            pool.publish_batch(vec![
                ("default".to_owned(), request("first")),
                ("default".to_owned(), request("second")),
            ])
            .await
        }
    });

    for _ in 0..100 {
        if pool.metrics_snapshot().publishes_total == 2 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(pool.metrics_snapshot().publishes_total, 2);
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        transport
            .operations()
            .iter()
            .filter(|operation| matches!(operation, TransportOperation::Publish(_)))
            .count(),
        2
    );
    assert!(first.resolve(Ok(PublishConfirmation::Ack(None))));
    assert!(second.resolve(Ok(PublishConfirmation::Ack(None))));

    let outcomes = publishing.await.expect("join").expect("batch");
    assert_eq!(outcomes.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn opens_a_profile_consumer_on_the_reused_broker_connection() {
    let transport = Arc::new(MockTransport::default());
    transport.push_delivery(Ok(TransportDelivery {
        delivery_tag: 42,
        exchange: "jobs".to_owned(),
        routing_key: "jobs".to_owned(),
        redelivered: false,
        message_id: None,
        correlation_id: None,
        headers: Arc::new(BTreeMap::default()),
        payload: Bytes::from_static(b"job-payload"),
    }));
    let pool = ClientPool::new(Arc::new(consumer_config()), transport.clone());

    let consumer = pool.consumer("main").await.expect("consumer");
    let delivery = consumer.next().await.expect("delivery");
    assert_eq!(delivery.payload, Bytes::from_static(b"job-payload"));
    delivery.ack().await.expect("ack enqueued");

    tokio::time::advance(std::time::Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let operations = transport.operations();
    assert!(
        operations
            .iter()
            .any(|operation| matches!(operation, TransportOperation::Qos { prefetch: 8 }))
    );
    assert!(operations.iter().any(|operation| matches!(
        operation,
        TransportOperation::Ack {
            delivery_tag: 42,
            multiple: false
        }
    )));
}

#[tokio::test]
async fn close_while_connecting_closes_the_uncommitted_connection_once() {
    let transport = Arc::new(MockTransport::default());
    let _gate = transport.push_connect_gate();
    let pool = Arc::new(ClientPool::new(Arc::new(config()), transport.clone()));
    let publishing = tokio::spawn({
        let pool = pool.clone();
        async move {
            pool.publish_batch(vec![("default".to_owned(), request("message"))])
                .await
        }
    });

    tokio::task::yield_now().await;

    let closing = tokio::spawn({
        let pool = pool.clone();
        async move { pool.close().await }
    });
    while !pool.is_closed() {
        tokio::task::yield_now().await;
    }
    closing.await.expect("close join").expect("close pool");

    let error = publishing
        .await
        .expect("publish join")
        .expect_err("connect loses commit");
    assert_eq!(error.kind(), ClientErrorKind::Closed);
    let operations = transport.operations();
    let operation_count = operations.len();
    assert!(
        !operations
            .iter()
            .any(|operation| matches!(operation, TransportOperation::OpenPublisher))
    );
    assert_eq!(
        pool.publish_batch(vec![("default".to_owned(), request("after-close"))])
            .await
            .expect_err("closed pool")
            .kind(),
        ClientErrorKind::Closed
    );
    assert_eq!(transport.operations().len(), operation_count);
}

#[tokio::test]
async fn concurrent_same_broker_initialization_is_deduplicated() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let gate = transport.push_connect_gate();
    let pool = Arc::new(ClientPool::new(Arc::new(config()), transport.clone()));
    let first = tokio::spawn({
        let pool = pool.clone();
        async move {
            pool.publish_batch(vec![("default".to_owned(), request("first"))])
                .await
        }
    });
    let second = tokio::spawn({
        let pool = pool.clone();
        async move {
            pool.publish_batch(vec![("default".to_owned(), request("second"))])
                .await
        }
    });
    gate.wait_entered().await;
    tokio::task::yield_now().await;
    assert!(gate.release());

    first.await.expect("first join").expect("first publish");
    second.await.expect("second join").expect("second publish");

    let operations = transport.operations();
    assert_eq!(
        operations
            .iter()
            .filter(|operation| matches!(operation, TransportOperation::Connect { .. }))
            .count(),
        1
    );
    assert_eq!(
        operations
            .iter()
            .filter(|operation| matches!(operation, TransportOperation::OpenPublisher))
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn independent_brokers_initialize_in_parallel() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let first_gate = transport.push_connect_gate();
    let second_gate = transport.push_connect_gate();
    let pool = Arc::new(ClientPool::new(
        Arc::new(two_broker_config()),
        transport.clone(),
    ));
    let first = tokio::spawn({
        let pool = pool.clone();
        async move {
            pool.publish_batch(vec![("first".to_owned(), request("first"))])
                .await
        }
    });
    let second = tokio::spawn({
        let pool = pool.clone();
        async move {
            pool.publish_batch(vec![("second".to_owned(), request("second"))])
                .await
        }
    });
    first_gate.wait_entered().await;
    tokio::time::timeout(Duration::from_millis(10), second_gate.wait_entered())
        .await
        .expect("second broker enters while first remains blocked");
    assert!(first_gate.release());
    assert!(second_gate.release());

    first.await.expect("first join").expect("first publish");
    second.await.expect("second join").expect("second publish");
}

#[tokio::test]
async fn queue_size_and_purge_operations() {
    let transport = Arc::new(MockTransport::default());
    transport.push_queue_size(Ok(42));
    let pool = ClientPool::new(Arc::new(config()), transport.clone());

    let size = pool
        .queue_size("default", "orders")
        .await
        .expect("queue size");

    assert_eq!(size, 42);
    assert!(transport.operations().iter().any(|op| matches!(
        op,
        TransportOperation::QueueSize { queue, .. } if queue == "orders"
    )));

    pool.purge_queue("default", "orders").await.expect("purge");

    assert!(transport.operations().iter().any(|op| matches!(
        op,
        TransportOperation::PurgeQueue { queue } if queue == "orders"
    )));
}

#[tokio::test]
async fn connection_states_reports_known_brokers_after_initialization() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let pool = ClientPool::new(Arc::new(config()), transport);

    pool.publish_batch(vec![("default".to_owned(), request("first"))])
        .await
        .expect("publish");

    let states = pool.connection_states();
    assert!(states.contains_key("default"));
}

// ---------------------------------------------------------------------------
// publish_batch: broker handle caching and result ordering
// ---------------------------------------------------------------------------

/// Verifies that `publish_batch` preserves the input order of outcomes even
/// when the requests are grouped by broker internally. With a single broker
/// the order must match the input sequence exactly.
#[tokio::test(start_paused = true)]
async fn publish_batch_preserves_order_after_broker_grouping() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let pool = ClientPool::new(Arc::new(config()), transport.clone());

    let requests = vec![
        ("default".to_owned(), request("msgA")),
        ("default".to_owned(), request("msgB")),
        ("default".to_owned(), request("msgC")),
    ];

    let outcomes = pool.publish_batch(requests).await.expect("batch");
    assert_eq!(outcomes.len(), 3);
    assert_eq!(
        outcomes[0],
        PublishOutcome::Confirmed {
            message_id: "msgA".into()
        }
    );
    assert_eq!(
        outcomes[1],
        PublishOutcome::Confirmed {
            message_id: "msgB".into()
        }
    );
    assert_eq!(
        outcomes[2],
        PublishOutcome::Confirmed {
            message_id: "msgC".into()
        }
    );
}

/// Verifies that `publish_batch` caches the publisher handle per broker,
/// opening only one publisher channel for repeated brokers.
#[tokio::test(start_paused = true)]
async fn publish_batch_caches_publisher_handle_per_broker() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let pool = ClientPool::new(Arc::new(config()), transport.clone());

    let requests = vec![
        ("default".to_owned(), request("msgA")),
        ("default".to_owned(), request("msgB")),
        ("default".to_owned(), request("msgC")),
    ];

    let outcomes = pool.publish_batch(requests).await.expect("batch");
    assert_eq!(outcomes.len(), 3);

    let operations = transport.operations();
    let connect_count = operations
        .iter()
        .filter(|op| matches!(op, TransportOperation::Connect { .. }))
        .count();
    let publisher_count = operations
        .iter()
        .filter(|op| matches!(op, TransportOperation::OpenPublisher))
        .count();
    assert_eq!(connect_count, 1, "one connection for one broker");
    assert_eq!(
        publisher_count, 1,
        "publisher handle cached across batch messages"
    );
}

/// Verifies that `publish_batch` preserves input order when messages are
/// spread across two brokers (interleaved). The outcomes must appear in the
/// original input order regardless of which broker confirmed first.
#[tokio::test(start_paused = true)]
async fn publish_batch_preserves_order_across_two_brokers() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let pool = ClientPool::new(Arc::new(two_broker_config()), transport.clone());

    let requests = vec![
        ("first".to_owned(), request("msgA")),
        ("second".to_owned(), request("msgB")),
        ("first".to_owned(), request("msgC")),
        ("second".to_owned(), request("msgD")),
    ];

    let outcomes = pool.publish_batch(requests).await.expect("batch");
    assert_eq!(outcomes.len(), 4);
    assert_eq!(
        outcomes[0],
        PublishOutcome::Confirmed {
            message_id: "msgA".into()
        }
    );
    assert_eq!(
        outcomes[1],
        PublishOutcome::Confirmed {
            message_id: "msgB".into()
        }
    );
    assert_eq!(
        outcomes[2],
        PublishOutcome::Confirmed {
            message_id: "msgC".into()
        }
    );
    assert_eq!(
        outcomes[3],
        PublishOutcome::Confirmed {
            message_id: "msgD".into()
        }
    );

    let operations = transport.operations();
    let publisher_count = operations
        .iter()
        .filter(|op| matches!(op, TransportOperation::OpenPublisher))
        .count();
    assert_eq!(publisher_count, 2, "one publisher per broker");
}

/// Issue #83: `publish_batch` must resolve the publications already accepted
/// by an actor before reporting a terminal acquisition failure. When one
/// broker's publisher acquisition fails permanently, the batch must still
/// await the waiters collected for the brokers that were acquired.
#[tokio::test(start_paused = true)]
async fn publish_batch_resolves_accepted_publications_when_a_broker_acquisition_fails() {
    let transport = Arc::new(MockTransport::default());
    // Warm-up publish: caches the first broker's coordinator and publisher so
    // the failing batch consumes the scripted connect failure only on the
    // second broker, regardless of the internal grouping order.
    transport.push_connect_result(Ok(()));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let pool = Arc::new(ClientPool::new(
        Arc::new(two_broker_config()),
        transport.clone(),
    ));
    pool.publish_batch(vec![("first".to_owned(), request("warm"))])
        .await
        .expect("warm-up publish");

    // The second broker's connect fails permanently while the first broker's
    // confirmation stays under the test's control.
    transport.push_connect_result(Err(TransportError::authentication(
        "credentials rejected by the broker",
    )));
    let confirmation = transport.push_controlled_confirmation();

    let publishing = tokio::spawn({
        let pool = Arc::clone(&pool);
        async move {
            pool.publish_batch(vec![
                ("first".to_owned(), request("accepted")),
                ("second".to_owned(), request("discarded")),
            ])
            .await
        }
    });

    // The first broker's publication is handed to its actor...
    common::wait_for_publish_count(&transport, 2).await;

    // ...and the batch must stay in-flight until that waiter resolves, even
    // though the second broker's acquisition already failed.
    for _ in 0..100 {
        assert!(
            !publishing.is_finished(),
            "publish_batch must await the accepted waiter before reporting the acquisition failure"
        );
        tokio::task::yield_now().await;
    }

    assert!(confirmation.resolve(Ok(PublishConfirmation::Ack(None))));
    let error = publishing
        .await
        .expect("join")
        .expect_err("the batch must still report the acquisition failure");
    assert_eq!(error.kind(), ClientErrorKind::Transport);

    // The accepted publication was resolved exactly once and the failed
    // broker's message was never published.
    let published_ids: Vec<String> = common::publish_requests(&transport)
        .iter()
        .filter_map(|request| request.properties.message_id.clone())
        .collect();
    assert!(
        published_ids.contains(&"accepted".to_owned()),
        "the accepted publication must be resolved: {published_ids:?}"
    );
    assert!(
        !published_ids.contains(&"discarded".to_owned()),
        "the failed broker's message must not be published: {published_ids:?}"
    );

    pool.close().await.expect("close pool");
}

// ---------------------------------------------------------------------------
// Multi-broker worker profile: all coordinators must be started
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn multi_broker_profile_starts_all_coordinators() {
    let transport = Arc::new(MockTransport::default());
    transport.keep_delivery_stream_open();
    let pool = ClientPool::new(Arc::new(helper::multi_broker_config()), transport.clone());

    let _consumer = pool
        .consumer("multi-broker-worker")
        .await
        .expect("consumer");

    // Both brokers' coordinators must have been started; each coordinator
    // connects its broker, so both "first" and "second" must appear in the
    // transport's Connect operations.
    let operations = transport.operations();
    let connected_brokers: Vec<String> = operations
        .iter()
        .filter_map(|op| {
            if let TransportOperation::Connect { broker } = op {
                Some(broker.clone())
            } else {
                None
            }
        })
        .collect();
    assert!(
        connected_brokers.contains(&"first".to_owned()),
        "first broker coordinator should be started, got {connected_brokers:?}"
    );
    assert!(
        connected_brokers.contains(&"second".to_owned()),
        "second broker coordinator should be started, got {connected_brokers:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn closing_a_multi_broker_consumer_closes_every_brokers_channels() {
    let transport = Arc::new(MockTransport::default());
    transport.keep_delivery_stream_open();
    let pool = ClientPool::new(Arc::new(helper::multi_broker_config()), transport.clone());

    let consumer = pool
        .consumer("multi-broker-worker")
        .await
        .expect("consumer");
    consumer.close().await.expect("close");

    tokio::time::advance(std::time::Duration::from_millis(20)).await;
    tokio::task::yield_now().await;

    let close_channels = transport
        .operations()
        .iter()
        .filter(|operation| matches!(operation, TransportOperation::CloseChannel))
        .count();
    assert_eq!(
        close_channels, 2,
        "closing the profile consumer must fan out to both brokers' channels"
    );
}

// ---------------------------------------------------------------------------
// Integration tests (from publish_consume.rs — integration-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "integration")]
mod integration {
    use super::*;

    /// Real-broker helper: the lab provisions the `rabbit_rs` user (not
    /// `guest`) on `localhost:5672` with per-vhost permissions.
    fn broker(name: &str, vhost: &str) -> BrokerConfig {
        BrokerConfig {
            name: name.to_owned(),
            hosts: vec![rabbit_rs_core::config::Endpoint::new("localhost", 5672)],
            vhost: vhost.to_owned(),
            credentials: rabbit_rs_core::config::Credentials::new("rabbit_rs", "rabbit_rs_lab"),
            tls: rabbit_rs_core::config::TlsConfig::disabled(),
            heartbeat: std::time::Duration::from_secs(30),
        }
    }

    fn config_single(queue: &str) -> Arc<rabbit_rs_core::config::ValidatedConfig> {
        config_with_mode(queue, TopologyMode::External)
    }

    fn config_with_mode(
        queue: &str,
        topology_mode: TopologyMode,
    ) -> Arc<rabbit_rs_core::config::ValidatedConfig> {
        Arc::new(
            Config {
                brokers: vec![broker("primary", "/orders-eu")],
                workers: vec![WorkerProfile {
                    name: "main".to_owned(),
                    subscriptions: vec![SubscriptionConfig {
                        name: "jobs".to_owned(),
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
                topology_mode,
                delay: rabbit_rs_core::config::DelayConfig::default(),
                dead_letter: None,
                delivery_limit: None,
                publisher: PublisherConfigSection::default(),
                consumer: rabbit_rs_core::config::ConsumerConfigSection::default(),
                queue_type: QueueKind::Quorum,
                queue_durable: true,
            }
            .validate()
            .expect("valid config"),
        )
    }

    /// Unique per run so parallel lab runs and reruns never collide.
    fn unique_suffix() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos()
            .to_string()
    }

    fn config_two_vhosts() -> Arc<rabbit_rs_core::config::ValidatedConfig> {
        Arc::new(
            Config {
                brokers: vec![
                    broker("orders", "/orders-eu"),
                    broker("billing", "/billing"),
                ],
                workers: vec![WorkerProfile {
                    name: "main".to_owned(),
                    subscriptions: vec![
                        SubscriptionConfig {
                            name: "orders-jobs".to_owned(),
                            broker: "orders".to_owned(),
                            queue: "rabbit-rs-it-orders".to_owned(),
                            weight: 1,
                            priority_class: 0,
                            prefetch: 8,
                            starvation_after: Duration::from_secs(30),
                            max_buffered_bytes: 64 * 1024 * 1024,
                            early_ack: false,
                            no_ack: false,
                        },
                        SubscriptionConfig {
                            name: "billing-jobs".to_owned(),
                            broker: "billing".to_owned(),
                            queue: "rabbit-rs-it-billing".to_owned(),
                            weight: 1,
                            priority_class: 0,
                            prefetch: 8,
                            starvation_after: Duration::from_secs(30),
                            max_buffered_bytes: 64 * 1024 * 1024,
                            early_ack: false,
                            no_ack: false,
                        },
                    ],
                    scheduler: SchedulerConfig::weighted_fair(),
                }],
                topology_mode: TopologyMode::External,
                delay: rabbit_rs_core::config::DelayConfig::default(),
                dead_letter: None,
                delivery_limit: None,
                publisher: PublisherConfigSection::default(),
                consumer: rabbit_rs_core::config::ConsumerConfigSection::default(),
                queue_type: QueueKind::Quorum,
                queue_durable: true,
            }
            .validate()
            .expect("valid config"),
        )
    }

    fn publish_request(msg_id: &str, routing_key: &str, payload: &[u8]) -> PublishRequest {
        PublishRequest::new(
            Destination::new("", routing_key),
            Bytes::copy_from_slice(payload),
            MessageProperties::new(msg_id),
            tokio::time::Instant::now() + Duration::from_secs(30),
        )
    }

    async fn declare_queue(vhost: &str, queue: &str) {
        use rabbit_rs_core::{
            topology::{QueueDefinition, TopologyDefinition, TopologyPlan, TopologyReconciler},
            transport::{Transport, lapin::LapinTransport},
        };

        let broker_config = broker("primary", vhost);
        let conn = LapinTransport
            .connect(&broker_config)
            .await
            .expect("connect for topology");
        let channel = conn.open_publisher().await.expect("publisher channel");

        let plan = TopologyPlan::compile(
            TopologyMode::Declare,
            TopologyDefinition::new(vec![], vec![QueueDefinition::new(queue)], vec![]),
        )
        .expect("compile plan");

        let mut reconciler = TopologyReconciler::new();
        reconciler
            .reconcile(channel.as_ref(), &plan, 1)
            .await
            .expect("declare queue");

        channel.close().await.expect("close channel");
        conn.close().await.expect("close connection");
    }

    async fn purge_or_ignore(pool: &ClientPool, broker: &str, queue: &str) {
        match pool.purge_queue(broker, queue).await {
            Ok(()) => {}
            Err(e)
                if e.kind() == ClientErrorKind::Transport
                    && format!("{e:?}").contains("NOT_FOUND") => {}
            Err(e) => panic!("unexpected purge error: {e:?}"),
        }
    }

    #[tokio::test]
    async fn publish_confirm_then_consume_and_ack() {
        use rabbit_rs_core::consumer::DeliveryState;

        // Unique queue per test: these tests run in parallel and must not
        // consume each other's messages.
        let queue = "rabbit-rs-it-confirm";
        declare_queue("/orders-eu", queue).await;

        let config = config_single(queue);
        let pool = ClientPool::production(config);
        purge_or_ignore(&pool, "primary", queue).await;

        let outcome = pool
            .publish_batch(vec![(
                "primary".to_owned(),
                publish_request("msg-confirm-1", queue, b"hello-confirm"),
            )])
            .await
            .expect("publish")
            .pop()
            .expect("publish");

        assert_eq!(
            outcome,
            PublishOutcome::Confirmed {
                message_id: "msg-confirm-1".into(),
            }
        );

        let consumer = pool.consumer("main").await.expect("consumer");
        let delivery = consumer.next().await.expect("delivery");
        assert_eq!(delivery.payload, Bytes::from_static(b"hello-confirm"));
        assert_eq!(delivery.id.as_str(), "msg-confirm-1");

        delivery.ack().await.expect("ack");
        assert_eq!(delivery.state(), DeliveryState::Acked);

        pool.close().await.expect("close");
    }

    /// Issue #66 / audit F-01: restart the broker container and the pool must
    /// self-recover — reconnect, re-establish consumers, and confirm new
    /// publishes — with no `connection_lost` report anywhere in the test.
    #[tokio::test]
    async fn broker_restart_recovers_publish_and_consume() {
        use rabbit_rs_core::consumer::DeliveryState;

        let queue = "rabbit-rs-it-restart";
        declare_queue("/orders-eu", queue).await;

        let config = config_single(queue);
        let pool = ClientPool::production(config);
        purge_or_ignore(&pool, "primary", queue).await;

        // Sanity: everything works before the kill.
        let outcome = pool
            .publish_batch(vec![(
                "primary".to_owned(),
                publish_request("msg-restart-1", queue, b"before-restart"),
            )])
            .await
            .expect("publish")
            .pop()
            .expect("publish");
        assert_eq!(
            outcome,
            PublishOutcome::Confirmed {
                message_id: "msg-restart-1".into(),
            }
        );

        let consumer = pool.consumer("main").await.expect("consumer");
        let delivery = consumer.next().await.expect("delivery");
        assert_eq!(delivery.payload, Bytes::from_static(b"before-restart"));
        delivery.ack().await.expect("ack");

        // Kill the broker: no connection_lost report, no recovery trigger —
        // only the transport itself can detect this.
        let container = lab_rabbitmq_container();
        let restarted = std::process::Command::new("docker")
            .args(["restart", &container])
            .output()
            .expect("docker restart must run in the lab environment");
        assert!(
            restarted.status.success(),
            "docker restart failed: {}",
            String::from_utf8_lossy(&restarted.stderr)
        );

        // The boot re-imports the lab definitions, which resets the
        // rabbit-rs.* configure permission the recovery reconcile needs.
        restore_configure_permission();

        // The pool must observe the loss on its own and reconnect.
        tokio::time::timeout(Duration::from_mins(2), async {
            while pool.metrics_snapshot().reconnects_total == 0 {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .expect("the pool must self-recover after the broker restart");

        // Publishing recovers on the fresh generation.
        let outcome = pool
            .publish_batch(vec![(
                "primary".to_owned(),
                publish_request("msg-restart-2", queue, b"after-restart"),
            )])
            .await
            .expect("publish after recovery")
            .pop()
            .expect("publish");
        assert_eq!(
            outcome,
            PublishOutcome::Confirmed {
                message_id: "msg-restart-2".into(),
            }
        );

        // Consuming recovers: the stale pre-restart handle is evicted and a
        // fresh consumer set delivers and settles. At-least-once allows the
        // killed generation's unacked delivery to be redelivered first — its
        // acknowledgement died with the connection.
        let consumer = pool.consumer("main").await.expect("fresh consumer");
        let mut delivery = consumer.next().await.expect("delivery after recovery");
        for _ in 0..10 {
            if delivery.id.as_str() == "msg-restart-2" {
                break;
            }
            delivery.ack().await.expect("ack redelivered duplicate");
            delivery = consumer.next().await.expect("next delivery");
        }
        assert_eq!(delivery.payload, Bytes::from_static(b"after-restart"));
        assert_eq!(delivery.id.as_str(), "msg-restart-2");
        delivery.ack().await.expect("ack");
        assert_eq!(delivery.state(), DeliveryState::Acked);

        assert!(pool.metrics_snapshot().reconnects_total >= 1);

        pool.close().await.expect("close");
    }

    /// Issue #77 / audit F-13: after a broker restart, `queue_size` must
    /// succeed again without a process restart — admin operations ride the
    /// coordinator's connection and its recovery machinery instead of a
    /// cached raw connection that died with the broker process.
    #[tokio::test]
    async fn broker_restart_recovers_admin_operations() {
        let queue = "rabbit-rs-it-admin-restart";
        declare_queue("/orders-eu", queue).await;

        let config = config_single(queue);
        let pool = ClientPool::production(config);
        purge_or_ignore(&pool, "primary", queue).await;

        // Sanity: the admin operation works before the restart (and triggers
        // coordinator establishment, which owns the single connection).
        let before = pool
            .queue_size("primary", queue)
            .await
            .expect("size before restart");
        assert_eq!(before, 0);

        // Restart the broker: only the transport itself can detect this; no
        // connection_lost report is issued anywhere in the test.
        let container = lab_rabbitmq_container();
        let restarted = std::process::Command::new("docker")
            .args(["restart", &container])
            .output()
            .expect("docker restart must run in the lab environment");
        assert!(
            restarted.status.success(),
            "docker restart failed: {}",
            String::from_utf8_lossy(&restarted.stderr)
        );

        // The boot re-imports the lab definitions, which resets the
        // rabbit-rs.* configure permission the recovery reconcile needs.
        restore_configure_permission();

        // The pool must observe the loss on its own and reconnect.
        tokio::time::timeout(Duration::from_mins(2), async {
            while pool.metrics_snapshot().reconnects_total == 0 {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .expect("the pool must self-recover after the broker restart");

        // The success criterion: size() succeeds again without process restart.
        let after = tokio::time::timeout(Duration::from_mins(1), pool.queue_size("primary", queue))
            .await
            .expect("queue_size must not hang after recovery")
            .expect("queue size must succeed after broker restart");
        assert_eq!(after, 0);

        pool.close().await.expect("close");
    }

    /// Finds the lab container backing `localhost:5672` (the compose project
    /// names it `rabbitrs-rabbitmq-1`, with profile-dependent suffixes).
    fn lab_rabbitmq_container() -> String {
        let output = std::process::Command::new("docker")
            .args(["ps", "--format", "{{.Names}}"])
            .output()
            .expect("docker must be available for the lab integration tests");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|name| name.starts_with("rabbitrs-rabbitmq-1"))
            .max_by_key(|name| name.len())
            .expect(
                "lab RabbitMQ node 1 must be running; start the lab with scripts/lab-up.sh with-plugin",
            )
            .to_owned()
    }

    /// Restores the `rabbit_rs` configure permission after a node restart.
    ///
    /// The lab re-imports its stored definitions.json on every node boot,
    /// which resets the permission to the stored narrow pattern; declare-mode
    /// pools re-declare the rabbit-rs.delayed exchange (issue #97), so every
    /// recovery generation after the restart fails topology reconciliation
    /// until the widened pattern is granted again. rabbitmqctl blocks until
    /// the node accepts commands, so polling its exit status also waits out
    /// the boot.
    fn restore_configure_permission() {
        use std::time::{Duration, Instant};

        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let output = std::process::Command::new("docker")
                .args([
                    "exec",
                    &lab_rabbitmq_container(),
                    "rabbitmqctl",
                    "set_permissions",
                    "-p",
                    "/orders-eu",
                    "rabbit_rs",
                    "^(amq\\.|rabbit-rs-it-|rabbit-rs\\.)",
                    ".*",
                    ".*",
                ])
                .output()
                .expect("docker exec must run in the lab environment");
            if output.status.success() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the lab never restored the rabbit-rs configure permission after the node restart: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    #[tokio::test]
    async fn release_zero_requeues_and_redispatches() {
        use rabbit_rs_core::consumer::DeliveryState;

        let queue = "rabbit-rs-it-release";
        declare_queue("/orders-eu", queue).await;

        let config = config_single(queue);
        let pool = ClientPool::production(config);
        purge_or_ignore(&pool, "primary", queue).await;

        pool.publish_batch(vec![(
            "primary".to_owned(),
            publish_request("msg-release-0", queue, b"hello-release"),
        )])
        .await
        .expect("publish");

        let consumer = pool.consumer("main").await.expect("consumer");
        let delivery = consumer.next().await.expect("delivery");

        delivery.release(Duration::ZERO).await.expect("release");
        assert_eq!(delivery.state(), DeliveryState::Rejected);

        let redelivered = consumer.next().await.expect("redelivered");
        assert_eq!(redelivered.payload, Bytes::from_static(b"hello-release"));
        redelivered.ack().await.expect("ack");

        pool.close().await.expect("close");
    }

    #[tokio::test]
    async fn two_vhosts_in_one_consumer_set() {
        let orders_queue = "rabbit-rs-it-orders";
        let billing_queue = "rabbit-rs-it-billing";
        declare_queue("/orders-eu", orders_queue).await;
        declare_queue("/billing", billing_queue).await;

        let config = config_two_vhosts();
        let pool = ClientPool::production(config);
        purge_or_ignore(&pool, "orders", orders_queue).await;
        purge_or_ignore(&pool, "billing", billing_queue).await;

        pool.publish_batch(vec![(
            "orders".to_owned(),
            publish_request("msg-orders-1", orders_queue, b"from-orders"),
        )])
        .await
        .expect("publish orders");
        pool.publish_batch(vec![(
            "billing".to_owned(),
            publish_request("msg-billing-1", billing_queue, b"from-billing"),
        )])
        .await
        .expect("publish billing");

        let consumer = pool.consumer("main").await.expect("consumer");

        let mut received = Vec::new();
        for _ in 0..2 {
            let delivery = consumer.next().await.expect("delivery");
            received.push(delivery.payload.clone());
            delivery.ack().await.expect("ack");
        }

        assert!(received.contains(&Bytes::from_static(b"from-orders")));
        assert!(received.contains(&Bytes::from_static(b"from-billing")));

        pool.close().await.expect("close");
    }

    #[tokio::test]
    async fn bulk_publish_then_consume_all() {
        let queue = "rabbit-rs-it-bulk";
        declare_queue("/orders-eu", queue).await;

        let config = config_single(queue);
        let pool = ClientPool::production(config);
        purge_or_ignore(&pool, "primary", queue).await;

        let requests: Vec<(String, PublishRequest)> = (0..5)
            .map(|i| {
                let id = format!("msg-bulk-{i}");
                let req = publish_request(&id, queue, format!("bulk-{i}").as_bytes());
                ("primary".to_owned(), req)
            })
            .collect();

        let outcomes = pool.publish_batch(requests).await.expect("publish batch");

        assert_eq!(outcomes.len(), 5);
        for (i, outcome) in outcomes.iter().enumerate() {
            assert_eq!(
                outcome,
                &PublishOutcome::Confirmed {
                    message_id: format!("msg-bulk-{i}").into()
                }
            );
        }

        let consumer = pool.consumer("main").await.expect("consumer");

        let mut received = Vec::new();
        for _ in 0..5 {
            let delivery = consumer.next().await.expect("delivery");
            received.push(delivery.payload.clone());
            delivery.ack().await.expect("ack");
        }

        assert_eq!(received.len(), 5);
        for i in 0..5 {
            assert!(received.contains(&Bytes::copy_from_slice(format!("bulk-{i}").as_bytes())));
        }

        pool.close().await.expect("close");
    }

    /// Issue #95: consuming a NEVER-pre-declared quorum queue must not race
    /// the topology reconcile — `basic.consume` must be issued only after the
    /// queue's `queue.declare` completes. `RabbitMQ` rejects the consume with
    /// 404 on quorum queues otherwise, burning a recovery generation. Every
    /// other lab test pre-declares its queue, which masked this race, so this
    /// scenario runs several rounds against fresh queues and requires the
    /// consumer to be ready with zero reconnects (no generation burned).
    #[tokio::test]
    async fn fresh_quorum_queue_consumer_does_not_race_declaration() {
        for iteration in 0..5 {
            let queue = format!("rabbit-rs-it-95-{}-{iteration}", unique_suffix());
            // Declare mode: the coordinator's plan owns the fresh queue; the
            // test itself never pre-declares it.
            let config = config_with_mode(&queue, TopologyMode::Declare);
            let pool = ClientPool::production(config);

            let consumer = pool
                .consumer("main")
                .await
                .expect("consumer on a never-declared quorum queue");
            assert_eq!(
                pool.metrics_snapshot().reconnects_total,
                0,
                "acquiring a consumer on a fresh quorum queue must not burn a recovery generation"
            );

            let message_id = format!("msg-95-{iteration}");
            let outcome = pool
                .publish_batch(vec![(
                    "primary".to_owned(),
                    publish_request(&message_id, &queue, b"fresh-quorum"),
                )])
                .await
                .expect("publish to the fresh queue")
                .pop()
                .expect("publish");
            assert_eq!(
                outcome,
                PublishOutcome::Confirmed {
                    message_id: message_id.clone().into(),
                }
            );

            let delivery = consumer.next().await.expect("delivery");
            assert_eq!(delivery.id.as_str(), message_id);
            delivery.ack().await.expect("ack");

            pool.close().await.expect("close");
        }
    }
}
