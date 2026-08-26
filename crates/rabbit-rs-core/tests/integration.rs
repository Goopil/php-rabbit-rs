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
    consumer::{Scheduler, SubscriptionId, SubscriptionPolicy, WeightedFairScheduler},
    publisher::{
        Destination, MessageOutcome, MessageProperties, PublishErrorKind, PublishOutcome,
        PublishRequest,
    },
    transport::{
        Delivery as TransportDelivery, PublishConfirmation, QueueKind, ReturnedMessage,
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
                    max_message_bytes: None,
                    early_ack: false,
                    no_ack: false,
                }],
                scheduler: SchedulerConfig::weighted_fair(16),
            }],
            topology_mode: TopologyMode::External,
            delay: rabbit_rs_core::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
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
                        max_message_bytes: None,
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
                        max_message_bytes: None,
                        early_ack: false,
                        no_ack: false,
                    },
                ],
                scheduler: SchedulerConfig::weighted_fair(16),
            }],
            topology_mode: TopologyMode::External,
            delay: rabbit_rs_core::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
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
        .publish("default", request("first"))
        .await
        .expect("first publish");
    let second = pool
        .publish("default", request("second"))
        .await
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
        async move { pool.publish("default", request("message")).await }
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
        pool.publish("default", request("after-close"))
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
        async move { pool.publish("default", request("first")).await }
    });
    let second = tokio::spawn({
        let pool = pool.clone();
        async move { pool.publish("default", request("second")).await }
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
        async move { pool.publish("first", request("first")).await }
    });
    let second = tokio::spawn({
        let pool = pool.clone();
        async move { pool.publish("second", request("second")).await }
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

    pool.publish("default", request("first"))
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

// ---------------------------------------------------------------------------
// publish_batch_detailed: per-message indexed report
// ---------------------------------------------------------------------------

/// Verifies that `publish_batch_detailed` returns one `MessageOutcome` per
/// input request in input order, classifying confirmed messages correctly.
#[tokio::test(start_paused = true)]
async fn publish_batch_detailed_classifies_confirmed_messages() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let pool = ClientPool::new(Arc::new(config()), transport);

    let requests = vec![
        ("default".to_owned(), request("msgA")),
        ("default".to_owned(), request("msgB")),
    ];

    let outcome = pool
        .publish_batch_detailed(requests)
        .await
        .expect("detailed batch");
    assert_eq!(outcome.results.len(), 2);
    assert!(matches!(
        &outcome.results[0],
        MessageOutcome::Confirmed(PublishOutcome::Confirmed { message_id })
            if message_id.as_ref() == "msgA"
    ));
    assert!(matches!(
        &outcome.results[1],
        MessageOutcome::Confirmed(PublishOutcome::Confirmed { message_id })
            if message_id.as_ref() == "msgB"
    ));
}

/// Verifies that a mandatory return is surfaced as `MessageOutcome::Returned`
/// with the broker reply info, in input order.
#[tokio::test(start_paused = true)]
async fn publish_batch_detailed_classifies_returned_message() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(Some(ReturnedMessage {
        reply_code: 312,
        reply_text: "NO_ROUTE".to_owned(),
        exchange: "jobs".to_owned(),
        routing_key: "missing".to_owned(),
        payload: Bytes::from_static(b"payload"),
    }))));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let pool = ClientPool::new(Arc::new(config()), transport);

    let requests = vec![
        ("default".to_owned(), request("returned")),
        ("default".to_owned(), request("ok")),
    ];

    let outcome = pool
        .publish_batch_detailed(requests)
        .await
        .expect("detailed batch");
    assert_eq!(outcome.results.len(), 2);
    assert!(matches!(
        &outcome.results[0],
        MessageOutcome::Returned(info) if info.code == 312
    ));
    assert!(matches!(
        &outcome.results[1],
        MessageOutcome::Confirmed(PublishOutcome::Confirmed { message_id })
            if message_id.as_ref() == "ok"
    ));
}

/// Verifies that a NACK is surfaced as `MessageOutcome::Failed` with the
/// `Nack` error kind, in input order.
#[tokio::test(start_paused = true)]
async fn publish_batch_detailed_classifies_failed_message() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Nack(None)));
    let pool = ClientPool::new(Arc::new(config()), transport);

    let requests = vec![
        ("default".to_owned(), request("ok")),
        ("default".to_owned(), request("nacked")),
    ];

    let outcome = pool
        .publish_batch_detailed(requests)
        .await
        .expect("detailed batch");
    assert_eq!(outcome.results.len(), 2);
    assert!(matches!(
        &outcome.results[0],
        MessageOutcome::Confirmed(PublishOutcome::Confirmed { message_id })
            if message_id.as_ref() == "ok"
    ));
    assert!(matches!(
        &outcome.results[1],
        MessageOutcome::Failed(err) if err.kind() == PublishErrorKind::Nack
    ));
}

/// `publish_batch_detailed` returns a `BatchOutcome` whose `results` length
/// always matches the input length, even on an empty batch.
#[tokio::test(start_paused = true)]
async fn publish_batch_detailed_empty_batch_returns_empty_outcome() {
    let transport = Arc::new(MockTransport::default());
    let pool = ClientPool::new(Arc::new(config()), transport);

    let outcome = pool
        .publish_batch_detailed(Vec::new())
        .await
        .expect("empty detailed batch");
    assert!(outcome.results.is_empty());
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

// ---------------------------------------------------------------------------
// Integration tests (from publish_consume.rs — integration-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "integration")]
mod integration {
    use super::*;

    fn broker(name: &str, vhost: &str) -> BrokerConfig {
        BrokerConfig {
            name: name.to_owned(),
            hosts: vec![Endpoint::new("localhost", 5672)],
            vhost: vhost.to_owned(),
            credentials: Credentials::new("rabbit_rs", "rabbit_rs_lab"),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        }
    }

    fn config_single() -> Arc<rabbit_rs_core::config::ValidatedConfig> {
        Arc::new(
            Config {
                brokers: vec![broker("primary", "/orders-eu")],
                workers: vec![WorkerProfile {
                    name: "main".to_owned(),
                    subscriptions: vec![SubscriptionConfig {
                        name: "jobs".to_owned(),
                        broker: "primary".to_owned(),
                        queue: "rabbit-rs-it-publish-consume".to_owned(),
                        weight: 1,
                        priority_class: 0,
                        prefetch: 8,
                        starvation_after: Duration::from_secs(30),
                        max_buffered_bytes: 64 * 1024 * 1024,
                        max_message_bytes: None,
                        early_ack: false,
                        no_ack: false,
                    }],
                    scheduler: SchedulerConfig::weighted_fair(16),
                }],
                topology_mode: TopologyMode::External,
                delay: rabbit_rs_core::config::DelayConfig::default(),
                dead_letter: None,
                delivery_limit: None,
                publisher: PublisherConfigSection::default(),
                queue_type: QueueKind::Quorum,
                queue_durable: true,
            }
            .validate()
            .expect("valid config"),
        )
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
                            max_message_bytes: None,
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
                            max_message_bytes: None,
                            early_ack: false,
                            no_ack: false,
                        },
                    ],
                    scheduler: SchedulerConfig::weighted_fair(16),
                }],
                topology_mode: TopologyMode::External,
                delay: rabbit_rs_core::config::DelayConfig::default(),
                dead_letter: None,
                delivery_limit: None,
                publisher: PublisherConfigSection::default(),
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

        let queue = "rabbit-rs-it-publish-consume";
        declare_queue("/orders-eu", queue).await;

        let config = config_single();
        let pool = ClientPool::production(config);
        purge_or_ignore(&pool, "primary", queue).await;

        let outcome = pool
            .publish(
                "primary",
                publish_request("msg-confirm-1", queue, b"hello-confirm"),
            )
            .await
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

    #[tokio::test]
    async fn release_zero_requeues_and_redispatches() {
        use rabbit_rs_core::consumer::DeliveryState;

        let queue = "rabbit-rs-it-publish-consume";
        declare_queue("/orders-eu", queue).await;

        let config = config_single();
        let pool = ClientPool::production(config);
        purge_or_ignore(&pool, "primary", queue).await;

        pool.publish(
            "primary",
            publish_request("msg-release-0", queue, b"hello-release"),
        )
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

        pool.publish(
            "orders",
            publish_request("msg-orders-1", orders_queue, b"from-orders"),
        )
        .await
        .expect("publish orders");
        pool.publish(
            "billing",
            publish_request("msg-billing-1", billing_queue, b"from-billing"),
        )
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
        let queue = "rabbit-rs-it-publish-consume";
        declare_queue("/orders-eu", queue).await;

        let config = config_single();
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
}
