//! P3 — `Pool::clear()` × pre-existing consumer (issue #38).
//!
//! The Phase E benchmark matrix observed pops degraded ~25× when
//! `Pool::clear()` (a queue purge) runs on a pool that already owns a
//! consumer: every measured round after the first purges the queue through
//! the driver API while the consumer created by the previous round stays
//! attached. These tests pin the core contract of that combination:
//!
//! 1. Deliveries keep flowing through the pre-existing consumer after a
//!    purge; settlements keep reaching the broker channel.
//! 2. A purge never re-establishes the consumer (no QoS/consume storm) and
//!    never opens more than one extra connection, no matter how many times
//!    it runs.
//! 3. Re-fetching the consumer after a purge returns the established set
//!    (no handle eviction) with an unchanged connection generation.
//!
//! A re-establishment storm (a new channel + `QoS` + consume per round, or a
//! fresh connection per purge) is the mechanism that would degrade pops;
//! these tests fail if it ever appears.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    client::ClientPool,
    config::{
        BrokerConfig, Config, ConsumerConfigSection, Credentials, Endpoint, PublisherConfigSection,
        SchedulerConfig, SubscriptionConfig, TlsConfig, TopologyMode, WorkerProfile,
    },
    transport::{
        Delivery as TransportDelivery, QueueKind,
        mock::{MockTransport, TransportOperation},
    },
};

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
                max_buffered_bytes: 64 * 1024 * 1024,
                early_ack: false,
                no_ack: false,
            }],
            scheduler: SchedulerConfig::weighted_fair(),
        }],
        topology_mode: TopologyMode::Declare,
        delay: rabbit_rs_core::config::DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
        publisher: PublisherConfigSection::default(),
        consumer: ConsumerConfigSection::default(),
        queue_type: QueueKind::Classic,
        queue_durable: true,
    }
    .validate()
    .expect("valid consumer config")
}

fn delivery(tag: u64) -> TransportDelivery {
    TransportDelivery {
        delivery_tag: tag,
        exchange: String::new(),
        routing_key: "jobs".to_owned(),
        redelivered: false,
        message_id: None,
        correlation_id: None,
        headers: Arc::new(BTreeMap::new()),
        payload: Bytes::from(format!("round-{tag}")),
    }
}

/// Fills the queue with the given delivery tags (the benchmark's fill phase).
fn fill(transport: &MockTransport, tags: std::ops::Range<u64>) {
    for tag in tags {
        transport.push_delivery(Ok(delivery(tag)));
    }
}

/// Pops `expected` deliveries through the consumer and acknowledges them,
/// mirroring the measured unit pop+ack drain. Every pop is bounded so a
/// stalled pipeline fails the test instead of hanging it.
async fn drain_and_ack(consumer: &rabbit_rs_core::consumer::ConsumerHandle, expected: usize) {
    for index in 0..expected {
        let delivery = tokio::time::timeout(Duration::from_millis(200), consumer.next())
            .await
            .unwrap_or_else(|_| panic!("pop {index} stalled out of {expected}"))
            .unwrap_or_else(|error| panic!("pop {index} errored: {error}"));
        delivery.ack().await.expect("ack enqueued");
    }
    // Let the actor run the queued settlements.
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;
}

fn count(transport: &MockTransport, predicate: impl Fn(&TransportOperation) -> bool) -> usize {
    transport
        .operations()
        .iter()
        .filter(|op| predicate(op))
        .count()
}

fn connect_count(transport: &MockTransport) -> usize {
    count(transport, |op| {
        matches!(op, TransportOperation::Connect { .. })
    })
}

#[tokio::test(start_paused = true)]
async fn purge_between_rounds_keeps_a_pre_existing_consumer_delivering() {
    let transport = Arc::new(MockTransport::default());
    // A live subscription never ends: without this the mock stream returns
    // `None` once its queue drains and the per-subscription pump exits, so
    // later fills would have no pump to ride on.
    transport.keep_delivery_stream_open();
    let pool = ClientPool::new(Arc::new(consumer_config()), transport.clone());

    // Round 0: the fill lands, then the consumer is created by the first pop.
    fill(&transport, 1..4);
    let consumer = pool.consumer("main").await.expect("consumer");
    drain_and_ack(&consumer, 3).await;

    // Rounds 1 and 2: Pool::clear() runs while the consumer is attached,
    // then the next fill must still surface through the same consumer.
    for round in 1..=2_u64 {
        pool.purge_queue("default", "jobs")
            .await
            .unwrap_or_else(|error| panic!("purge round {round} failed: {error}"));
        fill(&transport, (round * 3 + 1)..(round * 3 + 4));
        drain_and_ack(&consumer, 3).await;
    }

    // Every popped message was settled on its broker channel, and the
    // consumer never surfaced a stale-generation error.
    assert_eq!(
        count(&transport, |op| matches!(
            op,
            TransportOperation::Ack { .. }
        )),
        9,
        "all nine deliveries must be acknowledged"
    );
    assert!(
        consumer.drain_errors().is_empty(),
        "settlement errors must not appear across purges"
    );
    assert_eq!(
        consumer.generation(),
        1,
        "a purge must not bump the connection generation"
    );

    // No re-establishment storm: one connection for the coordinator, one for
    // the purge path; the consumer channel was configured and registered once.
    assert_eq!(
        connect_count(&transport),
        2,
        "a purge must not open a new connection per round"
    );
    assert_eq!(
        count(&transport, |op| matches!(
            op,
            TransportOperation::Qos { .. }
        )),
        1,
        "a purge must not re-run QoS on the consumer channel"
    );
    assert_eq!(
        count(&transport, |op| matches!(
            op,
            TransportOperation::Consume(_)
        )),
        1,
        "a purge must not re-register the consumer"
    );
}

#[tokio::test(start_paused = true)]
async fn repeated_purges_reuse_one_cached_connection() {
    let transport = Arc::new(MockTransport::default());
    transport.keep_delivery_stream_open();
    let pool = ClientPool::new(Arc::new(consumer_config()), transport.clone());

    // A pre-existing consumer establishes the coordinator connection.
    let consumer = pool.consumer("main").await.expect("consumer");

    for _ in 0..5 {
        pool.purge_queue("default", "jobs")
            .await
            .expect("repeated purge");
    }

    assert_eq!(
        connect_count(&transport),
        2,
        "repeated purges must reuse one raw connection next to the coordinator's"
    );
    assert_eq!(
        count(&transport, |op| matches!(
            op,
            TransportOperation::PurgeQueue { .. }
        )),
        5
    );

    consumer.close().await.expect("close consumer");
    pool.close().await.expect("close pool");
}

#[tokio::test(start_paused = true)]
async fn refetching_the_consumer_after_a_purge_reuses_the_established_set() {
    let transport = Arc::new(MockTransport::default());
    transport.keep_delivery_stream_open();
    fill(&transport, 1..4);
    let pool = ClientPool::new(Arc::new(consumer_config()), transport.clone());

    let consumer = pool.consumer("main").await.expect("first consumer");
    drain_and_ack(&consumer, 3).await;

    pool.purge_queue("default", "jobs").await.expect("purge");

    // Re-fetching (what a fresh Laravel queue instance does) must return the
    // established set instead of rebuilding it.
    let refetched = pool.consumer("main").await.expect("refetched consumer");
    assert_eq!(
        refetched.generation(),
        consumer.generation(),
        "a purge must not evict the consumer handle"
    );
    assert_eq!(
        count(&transport, |op| matches!(
            op,
            TransportOperation::Consume(_)
        )),
        1,
        "the consumer must not be re-registered after a purge"
    );

    // The refetched handle keeps delivering.
    fill(&transport, 4..7);
    drain_and_ack(&refetched, 3).await;
}

/// Mirrors the round-boundary shape of the benchmark: the consumer stays
/// attached while a purge and a fresh fill happen, and every delivery that
/// surfaces still settles on the pre-existing connection generation
/// (stale-ACK rejection stays meaningful, at-least-once preserved).
#[tokio::test(start_paused = true)]
async fn deliveries_after_a_purge_carry_the_pre_existing_generation() {
    let transport = Arc::new(MockTransport::default());
    transport.keep_delivery_stream_open();
    fill(&transport, 1..4);
    let pool = ClientPool::new(Arc::new(consumer_config()), transport.clone());

    let consumer = pool.consumer("main").await.expect("consumer");
    drain_and_ack(&consumer, 3).await;

    pool.purge_queue("default", "jobs").await.expect("purge");
    fill(&transport, 4..7);

    let mut payloads = Vec::new();
    for _ in 0..3 {
        let delivery = tokio::time::timeout(Duration::from_millis(200), consumer.next())
            .await
            .expect("pop must not stall after a purge")
            .expect("delivery after purge");
        payloads.push(delivery.payload.clone());
        delivery.ack().await.expect("ack enqueued");
    }
    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let expected: Vec<Bytes> = (4..=6)
        .map(|tag| Bytes::from(format!("round-{tag}")))
        .collect();
    assert_eq!(
        payloads, expected,
        "deliveries filled after a purge must surface through the pre-existing consumer"
    );
    assert!(
        consumer.drain_errors().is_empty(),
        "acks after a purge must settle without stale-generation errors"
    );
    assert_eq!(
        count(&transport, |op| matches!(
            op,
            TransportOperation::Ack { .. }
        )),
        6
    );
}
