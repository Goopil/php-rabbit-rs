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
//!    never opens an extra connection, no matter how many times it runs —
//!    it rides the coordinator's single connection (issue #77).
//! 3. Re-fetching the consumer after a purge returns the established set
//!    (no handle eviction) with an unchanged connection generation.
//!
//! A re-establishment storm (a new channel + `QoS` + consume per round, or a
//! fresh connection per purge) is the mechanism that would degrade pops;
//! these tests fail if it ever appears.
//!
//! The post-audit stabilization plan (Task 16) adds the `clear_route`
//! contract on the same pool: a purge of the main queue alone left deferred
//! jobs sitting in the TTL delay bucket queues the compiled plan synthesizes
//! for the route's destinations, where they still executed after the clear.
//! `clear_route` sweeps those buckets too — a bucket the GC already took
//! must not fail the clear, while a real purge failure still must.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use rabbit_rs_core::{
    client::{ClientErrorKind, ClientPool},
    config::{
        BrokerConfig, Config, ConsumerConfigSection, Credentials, DelayConfig, DelayMode, Endpoint,
        PrefetchConfig, PublisherConfigSection, RouteConfig, SchedulerConfig, SubscriptionConfig,
        TlsConfig, TopologyMode, WorkerProfile,
    },
    publisher::{Destination, MessageProperties, PublishOutcome, PublishRequest},
    topology::delay::DelayStrategy,
    transport::{
        Delivery as TransportDelivery, PublishConfirmation, QueueKind, TransportError,
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
                prefetch: PrefetchConfig::Fixed(8),
                max_buffered_bytes: 64 * 1024 * 1024,
                early_ack: false,
                no_ack: false,
            }],
            scheduler: SchedulerConfig::weighted_fair(),
        }],
        topology_mode: TopologyMode::Declare,
        routes: BTreeMap::new(),
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

/// Every delivery tag the wire acks settle: a cumulative
/// `Ack { multiple: true }` covers the whole contiguous range up to its tag.
fn settled_tags(transport: &MockTransport) -> BTreeSet<u64> {
    let mut settled = BTreeSet::new();
    let mut acked_upto = 0_u64;
    for operation in transport.operations() {
        if let TransportOperation::Ack {
            delivery_tag,
            multiple,
        } = operation
        {
            if multiple {
                settled.extend(acked_upto + 1..=delivery_tag);
                acked_upto = acked_upto.max(delivery_tag);
            } else {
                settled.insert(delivery_tag);
            }
        }
    }
    settled
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
        settled_tags(&transport),
        BTreeSet::from([1, 2, 3, 4, 5, 6, 7, 8, 9]),
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

    // No re-establishment storm: the purge rides the coordinator's single
    // connection; the consumer channel was configured and registered once.
    assert_eq!(
        connect_count(&transport),
        1,
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
async fn repeated_purges_reuse_the_coordinator_connection() {
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
        1,
        "repeated purges must reuse the coordinator's single connection"
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
        settled_tags(&transport),
        BTreeSet::from([1, 2, 3, 4, 5, 6]),
        "acks after a purge must settle without stale-generation errors"
    );
}

// ---------------------------------------------------------------------------
// Task 16 (post-audit stabilization) — `clear()` covers TTL delay buckets.
//
// `queue:clear` purging only the main queue left deferred jobs sitting in
// the `rabbit-rs.delay.*` bucket queues, where they still dead-lettered
// back into the cleared queue and executed after the clear. `clear_route`
// sweeps the compiled plan's bucket queues for the route's destinations,
// tolerates buckets the GC already took (missing-queue verdict), and still
// fails on real purge failures.
// ---------------------------------------------------------------------------

/// A consumer-shaped config publishing through a TTL delay plan: the route
/// `default` publishes `jobs` through the `jobs` exchange, and delayed
/// dispatches land in the destination's synthesized bucket queues.
fn ttl_delay_config() -> rabbit_rs_core::config::ValidatedConfig {
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
                prefetch: PrefetchConfig::Fixed(8),
                max_buffered_bytes: 64 * 1024 * 1024,
                early_ack: false,
                no_ack: false,
            }],
            scheduler: SchedulerConfig::weighted_fair(),
        }],
        topology_mode: TopologyMode::Declare,
        routes: BTreeMap::from([(
            "default".to_owned(),
            RouteConfig {
                broker: "default".to_owned(),
                exchange: "jobs".to_owned(),
                routing_key: "{queue}".to_owned(),
            },
        )]),
        delay: DelayConfig {
            mode: DelayMode::Ttl,
            ..DelayConfig::default()
        },
        dead_letter: None,
        delivery_limit: None,
        publisher: PublisherConfigSection::default(),
        consumer: ConsumerConfigSection::default(),
        queue_type: QueueKind::Classic,
        queue_durable: true,
    }
    .validate()
    .expect("valid ttl delay config")
}

/// The bucket queue names the compiled plan produces for the route
/// destination the delayed publish used (`(jobs, jobs)` — the route exchange
/// with the `{queue}` template resolved).
fn planned_bucket_names(config: &rabbit_rs_core::config::ValidatedConfig) -> Vec<String> {
    let DelayStrategy::TtlBuckets(plan) = DelayStrategy::compile(config) else {
        panic!("the ttl delay config must compile to the bucket strategy");
    };

    plan.expected_queue_names(&Destination::new("jobs", "jobs"))
}

/// The queue names every recorded purge targeted, in wire order.
fn purged_queues(transport: &MockTransport) -> Vec<String> {
    transport
        .operations()
        .iter()
        .filter_map(|operation| match operation {
            TransportOperation::PurgeQueue { queue } => Some(queue.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn clear_route_purges_the_delay_bucket_holding_the_deferred_job() {
    let transport = Arc::new(MockTransport::default());
    let pool = ClientPool::new(Arc::new(ttl_delay_config()), transport.clone());

    // A delayed dispatch through the route destination: the delay router
    // publishes it into the destination's synthesized bucket queue (default
    // exchange, bucket queue name as the routing key), declared lazily.
    let mut properties = MessageProperties::new("deferred-1");
    properties.delay_ms = Some(1_000);
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let outcomes = pool
        .publish_batch(vec![(
            "default".into(),
            PublishRequest::new(
                Destination::new("jobs", "jobs"),
                Bytes::from_static(b"deferred"),
                properties,
                tokio::time::Instant::now() + Duration::from_secs(30),
            ),
        )])
        .await
        .expect("delayed publish accepted");
    assert!(
        matches!(&outcomes[..], [PublishOutcome::Confirmed { .. }]),
        "the deferred publication must be confirmed: {outcomes:?}"
    );

    // The bucket the deferred job actually landed in: the wire publish
    // carries the synthesized queue name as its routing key.
    let bucket = transport
        .operations()
        .iter()
        .find_map(|operation| match operation {
            TransportOperation::Publish(request)
                if request.routing_key.starts_with("rabbit-rs.delay.") =>
            {
                Some(request.routing_key.to_string())
            }
            _ => None,
        })
        .expect("the delayed publish must have been routed into a bucket queue");

    pool.clear_route("default", "jobs")
        .await
        .expect("clear route");

    let purged = purged_queues(&transport);
    assert!(
        purged.contains(&"jobs".to_owned()),
        "the main queue must still be purged, got {purged:?}"
    );
    assert!(
        purged.contains(&bucket),
        "clear must purge the delay bucket '{bucket}' holding the deferred job, got {purged:?}"
    );

    pool.close().await.expect("close pool");
}

#[tokio::test(start_paused = true)]
async fn clear_route_tolerates_buckets_already_swept_by_the_gc() {
    let transport = Arc::new(MockTransport::default());
    let pool = ClientPool::new(Arc::new(ttl_delay_config()), transport.clone());

    // Warm the coordinator first: its startup topology reconcile consumes
    // scripted operation results, and the script below targets the clear's
    // own purge operations.
    pool.purge_queue("default", "jobs").await.expect("warm-up");

    // The main queue purges fine; the first bucket is gone already (swept,
    // GC'd by its x-expires, or never declared): the broker answers the
    // purge with the missing-queue verdict.
    transport.push_operation_result(Ok(()));
    transport.push_operation_result(Err(TransportError::protocol(
        "NOT_FOUND - no queue 'rabbit-rs.delay.x' in vhost '/'",
    )));

    pool.clear_route("default", "jobs")
        .await
        .expect("a missing bucket must not fail the clear");

    // The sweep continued past the missing bucket: every planned bucket was
    // purged, in the plan's deterministic (ascending) bucket order.
    let expected_buckets = planned_bucket_names(&ttl_delay_config());
    let purged_buckets: Vec<String> = purged_queues(&transport)
        .into_iter()
        .filter(|queue| queue.starts_with("rabbit-rs.delay."))
        .collect();
    assert_eq!(
        purged_buckets, expected_buckets,
        "every planned bucket must be purged despite the missing first bucket"
    );

    pool.close().await.expect("close pool");
}

#[tokio::test(start_paused = true)]
async fn clear_route_propagates_real_bucket_purge_failures() {
    let transport = Arc::new(MockTransport::default());
    let pool = ClientPool::new(Arc::new(ttl_delay_config()), transport.clone());

    // Warm the coordinator first (see the swept-bucket test above): the
    // script below must reach the clear's own purge operations.
    pool.purge_queue("default", "jobs").await.expect("warm-up");

    // The main queue purges fine; the first bucket purge fails for a real
    // reason (connection loss): the clear must fail, never silently swallow
    // the record.
    transport.push_operation_result(Ok(()));
    transport.push_operation_result(Err(TransportError::connection("socket reset mid-purge")));

    let error = pool
        .clear_route("default", "jobs")
        .await
        .expect_err("a real bucket purge failure must fail the clear");
    assert_eq!(
        error.kind(),
        ClientErrorKind::Transport,
        "the transport failure must surface as a typed client error: {error}"
    );
    // The failure happened at the first bucket purge: it was attempted (the
    // main purge succeeded before it) and the sweep stopped there.
    let purged = purged_queues(&transport);
    let attempted_buckets = purged
        .iter()
        .filter(|queue| queue.starts_with("rabbit-rs.delay."))
        .count();
    assert_eq!(
        attempted_buckets, 1,
        "the sweep must stop at the failed bucket purge, got {purged:?}"
    );

    pool.close().await.expect("close pool");
}
