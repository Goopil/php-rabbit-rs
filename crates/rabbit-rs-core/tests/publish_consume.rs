//! Integration tests against a real `RabbitMQ` broker.
//!
//! These tests require a running `RabbitMQ` instance accessible at
//! `localhost:5672` with the `rabbit_rs`/`rabbit_rs_lab` user.
//! They are gated behind the `integration` feature flag.
#![cfg(feature = "integration")]

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    client::{ClientErrorKind, ClientPool},
    config::{
        BrokerConfig, Config, Credentials, Endpoint, PublisherConfigSection, SchedulerConfig,
        SubscriptionConfig, TlsConfig, TopologyMode, WorkerProfile,
    },
    consumer::DeliveryState,
    publisher::{Destination, MessageProperties, PublishOutcome, PublishRequest},
    topology::{QueueDefinition, TopologyDefinition, TopologyPlan, TopologyReconciler},
    transport::{Transport, lapin::LapinTransport},
};
use tokio::time::Instant;

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
                }],
                scheduler: SchedulerConfig::weighted_fair(16),
            }],
            topology_mode: TopologyMode::External,
            delay: rabbit_rs_core::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
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
                    },
                    SubscriptionConfig {
                        name: "billing-jobs".to_owned(),
                        broker: "billing".to_owned(),
                        queue: "rabbit-rs-it-billing".to_owned(),
                        weight: 1,
                        priority_class: 0,
                        prefetch: 8,
                        starvation_after: Duration::from_secs(30),
                    },
                ],
                scheduler: SchedulerConfig::weighted_fair(16),
            }],
            topology_mode: TopologyMode::External,
            delay: rabbit_rs_core::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
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
        Instant::now() + Duration::from_secs(30),
    )
}

/// Declare a quorum queue on the given vhost so tests can publish and consume.
async fn declare_queue(vhost: &str, queue: &str) {
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

/// Purge a queue, ignoring `NOT_FOUND` errors (queue not declared yet).
async fn purge_or_ignore(pool: &ClientPool, broker: &str, queue: &str) {
    match pool.purge_queue(broker, queue).await {
        Ok(()) => {}
        Err(e)
            if e.kind() == ClientErrorKind::Transport && format!("{e:?}").contains("NOT_FOUND") => {
        }
        Err(e) => panic!("unexpected purge error: {e:?}"),
    }
}

#[tokio::test]
async fn publish_confirm_then_consume_and_ack() {
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
