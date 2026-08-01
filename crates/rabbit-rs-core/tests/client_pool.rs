use std::{collections::BTreeMap, sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    client::ClientPool,
    config::{
        BrokerConfig, Config, Credentials, Endpoint, SchedulerConfig, SubscriptionConfig,
        TlsConfig, TopologyMode, WorkerProfile,
    },
    publisher::{Destination, MessageProperties, PublishOutcome, PublishRequest},
    transport::{
        Delivery as TransportDelivery, PublishConfirmation,
        mock::{MockTransport, TransportOperation},
    },
};
use tokio::time::Instant;

fn config() -> rabbit_rs_core::config::ValidatedConfig {
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
    }
    .validate()
    .expect("valid config")
}

fn consumer_config() -> rabbit_rs_core::config::ValidatedConfig {
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
            }],
            max_in_flight: 16,
            scheduler: SchedulerConfig::weighted_fair(),
        }],
        topology_mode: TopologyMode::External,
    }
    .validate()
    .expect("valid consumer config")
}

fn request(message_id: &str) -> PublishRequest {
    PublishRequest::new(
        Destination::new("jobs", "default"),
        Bytes::from_static(b"payload"),
        MessageProperties::new(message_id),
        Instant::now() + Duration::from_secs(1),
    )
}

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
            message_id: "first".to_owned()
        }
    );
    assert_eq!(
        second,
        PublishOutcome::Confirmed {
            message_id: "second".to_owned()
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

#[tokio::test]
async fn rejects_unknown_broker_without_connecting() {
    let transport = Arc::new(MockTransport::default());
    let pool = ClientPool::new(Arc::new(config()), transport.clone());

    let error = pool
        .publish("missing", request("message"))
        .await
        .expect_err("unknown broker");

    assert!(error.to_string().contains("brokers.missing"));
    assert!(transport.operations().is_empty());
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

#[tokio::test]
async fn opens_a_profile_consumer_on_the_reused_broker_connection() {
    let transport = Arc::new(MockTransport::default());
    transport.push_delivery(Ok(TransportDelivery {
        delivery_tag: 42,
        exchange: "jobs".to_owned(),
        routing_key: "jobs".to_owned(),
        redelivered: false,
        headers: BTreeMap::default(),
        payload: Bytes::from_static(b"job-payload"),
    }));
    let pool = ClientPool::new(Arc::new(consumer_config()), transport.clone());

    let consumer = pool.consumer("main").await.expect("consumer");
    let delivery = consumer.next().await.expect("delivery");
    assert_eq!(delivery.payload, Bytes::from_static(b"job-payload"));
    delivery.ack().await.expect("ack");

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
async fn closing_pool_after_its_consumer_is_idempotent() {
    let transport = Arc::new(MockTransport::default());
    transport.keep_delivery_stream_open();
    let pool = ClientPool::new(Arc::new(consumer_config()), transport);
    let consumer = pool.consumer("main").await.expect("consumer");

    consumer.close().await.expect("close consumer");

    pool.close().await.expect("close pool");
    assert!(pool.is_closed());
}

#[tokio::test]
async fn delivery_reject_forwards_the_requested_requeue_policy() {
    let transport = Arc::new(MockTransport::default());
    transport.push_delivery(Ok(TransportDelivery {
        delivery_tag: 9,
        exchange: "jobs".to_owned(),
        routing_key: "jobs".to_owned(),
        redelivered: false,
        headers: BTreeMap::default(),
        payload: Bytes::from_static(b"job-payload"),
    }));
    let pool = ClientPool::new(Arc::new(consumer_config()), transport.clone());
    let consumer = pool.consumer("main").await.expect("consumer");
    let delivery = consumer.next().await.expect("delivery");

    delivery.reject(false).await.expect("reject");

    assert!(transport.operations().iter().any(|operation| matches!(
        operation,
        TransportOperation::Reject {
            delivery_tag: 9,
            requeue: false
        }
    )));
}
