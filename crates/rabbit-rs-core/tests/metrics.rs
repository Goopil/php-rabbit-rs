use std::{collections::BTreeMap, sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    config::{
        BrokerConfig, Config, Credentials, Endpoint, PublisherConfigSection, TlsConfig,
        TopologyMode,
    },
    consumer::{ConsumerSet, Subscription},
    metrics::Metrics,
    pool::{ConnectionKey, connection_actor::ConnectionActor},
    publisher::{
        Destination, MessageProperties, PublishErrorKind, PublishOutcome, PublishRequest,
        PublisherActor, PublisherConfig,
    },
    recovery::{ConnectionState, IdentityJitter, RecoveryPolicy, TokioClock},
    transport::{
        Delivery as TransportDelivery, PublishConfirmation, ReturnedMessage, Transport,
        TransportError,
        mock::{MockTransport, TransportOperation},
    },
};
use tokio::{sync::watch, time::Instant};

fn broker(password: &str) -> BrokerConfig {
    BrokerConfig {
        name: "primary".to_owned(),
        hosts: vec![Endpoint::new("rabbit.internal", 5672)],
        vhost: "/tenant".to_owned(),
        credentials: Credentials::new("worker", password),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(30),
    }
}

fn connection_key() -> ConnectionKey {
    ConnectionKey::from_config(
        &Config {
            brokers: vec![broker("guest")],
            workers: vec![],
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

fn publish_request(message_id: &str) -> PublishRequest {
    PublishRequest::new(
        Destination::new("jobs", "high"),
        Bytes::from_static(b"job"),
        MessageProperties::new(message_id),
        Instant::now() + Duration::from_secs(30),
    )
}

async fn wait_for_publish_count(transport: &MockTransport, expected: usize) {
    for _ in 0..100 {
        let count = transport
            .operations()
            .iter()
            .filter(|operation| matches!(operation, TransportOperation::Publish(_)))
            .count();
        if count == expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("publisher did not emit {expected} messages");
}

#[tokio::test(start_paused = true)]
async fn publisher_records_accepts_confirmations_returns_and_backpressure() {
    let transport = MockTransport::default();
    let controlled = transport.push_controlled_confirmation();
    let channel = transport
        .connect(&broker("guest"))
        .await
        .expect("connection")
        .open_publisher()
        .await
        .expect("publisher channel");
    let metrics = Metrics::default();
    let publisher = PublisherActor::spawn_with_metrics(
        Arc::from(channel),
        PublisherConfig::new(1, Duration::from_secs(5)),
        metrics,
    );

    let first = publisher
        .try_publish(publish_request("confirmed"))
        .expect("first publish");
    wait_for_publish_count(&transport, 1).await;
    let error = publisher
        .try_publish(publish_request("backpressured"))
        .expect_err("capacity is retained until confirmation");
    assert_eq!(error.kind(), PublishErrorKind::Backpressure);
    assert!(controlled.resolve(Ok(PublishConfirmation::Ack(None))));
    assert!(matches!(
        first.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));

    transport.push_confirmation(Ok(PublishConfirmation::Ack(Some(ReturnedMessage {
        reply_code: 312,
        reply_text: "NO_ROUTE".to_owned(),
        exchange: "jobs".to_owned(),
        routing_key: "missing".to_owned(),
        payload: Bytes::from_static(b"job"),
    }))));
    let returned = publisher
        .try_publish(publish_request("returned"))
        .expect("second accepted publish");
    assert!(matches!(
        returned.wait().await,
        Ok(PublishOutcome::Returned { .. })
    ));

    let snapshot = publisher.metrics_snapshot();
    assert_eq!(snapshot.publishes_total, 2);
    assert_eq!(snapshot.confirmations_total, 2);
    assert_eq!(snapshot.returns_total, 1);
    assert_eq!(snapshot.backpressure_total, 1);
    assert_eq!(snapshot.confirmation_latency.samples, 2);
}

#[tokio::test(start_paused = true)]
async fn only_ack_and_nack_are_recorded_as_confirmations() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Nack(None)));
    let channel = transport
        .connect(&broker("guest"))
        .await
        .expect("connection")
        .open_publisher()
        .await
        .expect("publisher channel");
    let publisher = PublisherActor::spawn_with_metrics(
        Arc::from(channel),
        PublisherConfig::new(1, Duration::from_secs(5)),
        Metrics::default(),
    );

    let nack = publisher
        .try_publish(publish_request("nack"))
        .expect("NACK publication");
    assert_eq!(
        nack.wait().await.expect_err("NACK outcome").kind(),
        PublishErrorKind::Nack
    );
    let unconfirmed = publisher
        .try_publish(publish_request("unconfirmed"))
        .expect("unconfirmed publication");
    assert_eq!(
        unconfirmed
            .wait()
            .await
            .expect_err("confirms not requested")
            .kind(),
        PublishErrorKind::Unconfirmed
    );

    let snapshot = publisher.metrics_snapshot();
    assert_eq!(snapshot.publishes_total, 2);
    assert_eq!(snapshot.confirmations_total, 1);
    assert_eq!(snapshot.confirmation_latency.samples, 1);
}

fn transport_delivery(tag: u64) -> TransportDelivery {
    TransportDelivery {
        delivery_tag: tag,
        exchange: "jobs".to_owned(),
        routing_key: "high".to_owned(),
        redelivered: false,
        message_id: None,
        correlation_id: None,
        headers: Arc::new(BTreeMap::new()),
        payload: Bytes::from_static(b"job"),
    }
}

#[tokio::test]
async fn consumer_records_deliveries_acks_rejects_and_settlement_latency() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(transport_delivery(1)));
    transport.push_delivery(Ok(transport_delivery(2)));
    let channel = transport
        .connect(&broker("guest"))
        .await
        .expect("connection")
        .open_consumer()
        .await
        .expect("consumer channel");
    let subscription = Subscription::new("jobs", connection_key(), "jobs", Arc::from(channel));
    let metrics = Metrics::default();
    let consumer = ConsumerSet::spawn_with_metrics(vec![subscription], 2, metrics)
        .await
        .expect("consumer set");

    let acknowledged = consumer.next().await.expect("first delivery");
    acknowledged.ack().await.expect("ACK");
    let rejected = consumer.next().await.expect("second delivery");
    rejected.release(Duration::ZERO).await.expect("reject");

    let snapshot = consumer.metrics_snapshot();
    assert_eq!(snapshot.deliveries_total, 2);
    assert_eq!(snapshot.acks_total, 1);
    assert_eq!(snapshot.rejects_total, 1);
    assert_eq!(snapshot.settlement_latency.samples, 2);
}

async fn wait_for_state(
    states: &mut watch::Receiver<ConnectionState>,
    predicate: impl Fn(&ConnectionState) -> bool,
) {
    loop {
        if predicate(&states.borrow()) {
            return;
        }
        states.changed().await.expect("connection actor alive");
    }
}

#[tokio::test(start_paused = true)]
async fn connection_counts_only_successful_reconnections_and_snapshot_has_no_secrets() {
    let transport = Arc::new(MockTransport::default());
    let metrics = Metrics::default();
    let actor = ConnectionActor::spawn_with_dependencies_and_metrics(
        transport,
        broker("very-secret-password"),
        RecoveryPolicy::default(),
        Arc::new(TokioClock),
        Arc::new(IdentityJitter),
        metrics,
    );
    let mut states = actor.subscribe();
    actor.start().await.expect("start");
    wait_for_state(&mut states, |state| {
        matches!(state, ConnectionState::Ready { generation: 1 })
    })
    .await;
    assert_eq!(actor.metrics_snapshot().reconnects_total, 0);

    actor
        .connection_lost(TransportError::connection("socket reset"))
        .await
        .expect("connection loss");
    wait_for_state(&mut states, |state| {
        matches!(state, ConnectionState::Recovering { .. })
    })
    .await;
    tokio::time::advance(Duration::from_millis(100)).await;
    wait_for_state(&mut states, |state| {
        matches!(state, ConnectionState::Ready { generation: 2 })
    })
    .await;

    let snapshot = actor.metrics_snapshot();
    assert_eq!(snapshot.reconnects_total, 1);
    let serialized = serde_json::to_string(&snapshot).expect("serializable snapshot");
    assert!(!serialized.contains("very-secret-password"));
    assert!(!serialized.contains("rabbit.internal"));
    assert!(!serialized.contains("/tenant"));
}

#[test]
fn snapshot_is_synchronous_and_available_without_an_async_runtime() {
    let metrics = Metrics::default();

    for _ in 0..10_000 {
        std::hint::black_box(metrics.snapshot());
    }

    assert_eq!(metrics.snapshot().publishes_total, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_snapshots_do_not_prevent_publisher_progress() {
    const MESSAGE_COUNT: usize = 64;

    let transport = MockTransport::default();
    for _ in 0..MESSAGE_COUNT {
        transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    }
    let channel = transport
        .connect(&broker("guest"))
        .await
        .expect("connection")
        .open_publisher()
        .await
        .expect("publisher channel");
    let metrics = Metrics::default();
    let publisher = PublisherActor::spawn_with_metrics(
        Arc::from(channel),
        PublisherConfig::new(MESSAGE_COUNT, Duration::from_secs(5)),
        metrics.clone(),
    );
    let snapshot_reader = std::thread::spawn(move || {
        let mut latest = metrics.snapshot();
        for _ in 0..10_000 {
            latest = metrics.snapshot();
        }
        latest
    });

    let progress = tokio::time::timeout(Duration::from_secs(2), async {
        let mut waiters = Vec::new();
        for message in 0..MESSAGE_COUNT {
            waiters.push(
                publisher
                    .try_publish(publish_request(&format!("message-{message}")))
                    .expect("publication accepted"),
            );
        }
        for waiter in waiters {
            assert!(matches!(
                waiter.wait().await,
                Ok(PublishOutcome::Confirmed { .. })
            ));
        }
    })
    .await;
    let concurrent_snapshot = snapshot_reader.join().expect("snapshot reader");

    progress.expect("publisher progressed while snapshots were read");
    let expected = u64::try_from(MESSAGE_COUNT).expect("message count fits u64");
    assert_eq!(publisher.metrics_snapshot().confirmations_total, expected);
    assert!(concurrent_snapshot.confirmations_total <= expected);
}

#[tokio::test(start_paused = true)]
async fn depth_metrics_are_recorded() {
    let metrics = Metrics::default();
    metrics.record_publishing_depth(10);
    metrics.record_publishing_depth(5);
    metrics.record_publishing_bytes(1024);
    metrics.record_replay();
    metrics.record_consumer_buffer_depth(3);
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.publishing_depth, 5);
    assert_eq!(snapshot.publishing_depth_hwm, 10);
    assert_eq!(snapshot.publishing_bytes, 1024);
    assert_eq!(snapshot.replay_count, 1);
    assert_eq!(snapshot.replay_depth, 1);
    assert_eq!(snapshot.consumer_buffer_depth, 3);
}
