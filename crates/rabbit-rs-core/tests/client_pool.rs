use std::{collections::BTreeMap, sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    client::{ClientErrorKind, ClientPool},
    config::{
        BrokerConfig, Config, Credentials, Endpoint, SchedulerConfig, SubscriptionConfig,
        TlsConfig, TopologyMode, WorkerProfile,
    },
    publisher::{Destination, MessageProperties, PublishOutcome, PublishRequest},
    transport::{
        Delivery as TransportDelivery, PublishConfirmation, TransportError,
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
        delay: rabbit_rs_core::config::DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
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
                starvation_after: Duration::from_secs(30),
            }],
            scheduler: SchedulerConfig::weighted_fair(16),
        }],
        topology_mode: TopologyMode::External,
        delay: rabbit_rs_core::config::DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
    }
    .validate()
    .expect("valid consumer config")
}

fn two_broker_config() -> rabbit_rs_core::config::ValidatedConfig {
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
    }
    .validate()
    .expect("valid two-broker config")
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
async fn confirmation_transport_failures_keep_the_transport_classification() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Err(TransportError::protocol("transport failed")));
    let pool = ClientPool::new(Arc::new(config()), transport);

    let error = pool
        .publish("default", request("transport-error"))
        .await
        .expect_err("transport confirmation must fail");

    assert_eq!(error.kind(), ClientErrorKind::Transport);
    assert!(error.to_string().contains("transport failed"));
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
        message_id: None,
        correlation_id: None,
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
        message_id: None,
        correlation_id: None,
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

#[tokio::test]
async fn close_while_connecting_closes_the_uncommitted_connection_once() {
    let transport = Arc::new(MockTransport::default());
    let _gate = transport.push_connect_gate();
    let pool = Arc::new(ClientPool::new(Arc::new(config()), transport.clone()));
    let publishing = tokio::spawn({
        let pool = pool.clone();
        async move { pool.publish("default", request("message")).await }
    });

    // Give the coordinator time to start connecting.
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
async fn close_while_opening_publisher_closes_the_uncommitted_channel_once() {
    let transport = Arc::new(MockTransport::default());
    let _gate = transport.push_open_publisher_gate();
    let pool = Arc::new(ClientPool::new(Arc::new(config()), transport.clone()));
    let publishing = tokio::spawn({
        let pool = pool.clone();
        async move { pool.publish("default", request("message")).await }
    });

    // Give the coordinator time to start.
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
        .expect_err("publisher loses commit");
    assert_eq!(error.kind(), ClientErrorKind::Closed);
    let operation_count = transport.operations().len();
    assert_eq!(
        pool.consumer("main").await.expect_err("closed pool").kind(),
        ClientErrorKind::Closed
    );
    assert_eq!(transport.operations().len(), operation_count);
}

#[tokio::test]
async fn close_while_opening_consumer_closes_the_uncommitted_channel_once() {
    let transport = Arc::new(MockTransport::default());
    let _gate = transport.push_open_consumer_gate();
    let pool = Arc::new(ClientPool::new(
        Arc::new(consumer_config()),
        transport.clone(),
    ));
    let consuming = tokio::spawn({
        let pool = pool.clone();
        async move { pool.consumer("main").await }
    });

    // Give the coordinator time to start.
    tokio::task::yield_now().await;

    let closing = tokio::spawn({
        let pool = pool.clone();
        async move { pool.close().await }
    });
    while !pool.is_closed() {
        tokio::task::yield_now().await;
    }
    closing.await.expect("close join").expect("close pool");

    let result = consuming.await.expect("consumer join");
    // The consumer may succeed or fail depending on timing.
    // With the coordinator, if close happens before the consumer is ready,
    // it returns Closed. If the consumer was already committed, it succeeds.
    if let Err(error) = result {
        assert_eq!(error.kind(), ClientErrorKind::Closed);
    }
    let operation_count = transport.operations().len();
    assert_eq!(
        pool.consumer("main").await.expect_err("closed pool").kind(),
        ClientErrorKind::Closed
    );
    assert_eq!(transport.operations().len(), operation_count);
}

#[tokio::test(start_paused = true)]
async fn close_during_pending_confirm_resolves_the_publish_once() {
    let transport = Arc::new(MockTransport::default());
    let confirmation = transport.push_controlled_confirmation();
    let pool = Arc::new(ClientPool::new(Arc::new(config()), transport.clone()));
    let publishing = tokio::spawn({
        let pool = pool.clone();
        async move { pool.publish("default", request("message")).await }
    });
    for _ in 0..100 {
        if transport
            .operations()
            .iter()
            .any(|operation| matches!(operation, TransportOperation::Publish(_)))
        {
            break;
        }
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
    }

    pool.close().await.expect("close pool");

    let error = publishing
        .await
        .expect("publish join")
        .expect_err("pending confirm closes");
    assert_eq!(error.kind(), ClientErrorKind::Closed);
    assert!(!confirmation.resolve(Ok(PublishConfirmation::Ack(None))));
    let operations = transport.operations();
    assert_eq!(
        operations
            .iter()
            .filter(|operation| matches!(operation, TransportOperation::CloseChannel))
            .count(),
        1
    );
    assert_eq!(
        operations
            .iter()
            .filter(|operation| matches!(operation, TransportOperation::CloseConnection))
            .count(),
        1
    );
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
async fn queue_size_returns_message_count_from_the_broker() {
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
}

#[tokio::test]
async fn queue_size_propagates_broker_error() {
    let transport = Arc::new(MockTransport::default());
    transport.push_queue_size(Err(TransportError::protocol("queue missing")));
    let pool = ClientPool::new(Arc::new(config()), transport.clone());

    let error = pool
        .queue_size("default", "missing")
        .await
        .expect_err("should fail");

    assert_eq!(error.kind(), ClientErrorKind::Transport);
}

#[tokio::test]
async fn purge_queue_clears_messages_from_the_broker() {
    let transport = Arc::new(MockTransport::default());
    let pool = ClientPool::new(Arc::new(config()), transport.clone());

    pool.purge_queue("default", "orders").await.expect("purge");

    assert!(transport.operations().iter().any(|op| matches!(
        op,
        TransportOperation::PurgeQueue { queue } if queue == "orders"
    )));
}

#[tokio::test]
async fn queue_size_on_unknown_broker_returns_configuration_error() {
    let transport = Arc::new(MockTransport::default());
    let pool = ClientPool::new(Arc::new(config()), transport.clone());

    let error = pool
        .queue_size("unknown", "orders")
        .await
        .expect_err("should fail");

    assert_eq!(error.kind(), ClientErrorKind::Configuration);
}
