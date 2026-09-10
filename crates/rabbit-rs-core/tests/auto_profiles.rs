use std::{collections::BTreeMap, sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    client::{ClientErrorKind, ClientPool},
    config::{
        BrokerConfig, Config, ConsumerConfigSection, DelayConfig, PublisherConfigSection,
        TopologyMode, ValidatedConfig, WorkerProfile,
    },
    publisher::{Destination, MessageProperties, PublishRequest},
    transport::{
        Delivery as TransportDelivery, PublishConfirmation, QueueKind, TransportError,
        mock::{MockTransport, TransportOperation},
    },
};

mod common;

fn broker(name: &str) -> BrokerConfig {
    common::broker(name, "/", "guest")
}

/// One configured worker profile: named after its single queue, subscribed
/// on the single broker "main".
fn worker_on(queue: &str) -> WorkerProfile {
    common::worker_profile(queue, "main", queue, 4)
}

fn single_worker_config(topology: TopologyMode, worker: WorkerProfile) -> Arc<ValidatedConfig> {
    Arc::new(
        Config {
            brokers: vec![broker("main")],
            workers: vec![worker],
            topology_mode: topology,
            routes: BTreeMap::new(),
            delay: DelayConfig::default(),
            dead_letter: None,
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

fn delivery(tag: u64) -> TransportDelivery {
    TransportDelivery {
        delivery_tag: tag,
        exchange: "jobs".to_owned(),
        routing_key: "emails".to_owned(),
        redelivered: false,
        message_id: None,
        correlation_id: None,
        headers: Arc::new(BTreeMap::new()),
        payload: Bytes::from_static(b"payload"),
    }
}

#[tokio::test(start_paused = true)]
async fn synthesizes_and_declares_auto_queue() {
    let transport = Arc::new(MockTransport::default());
    transport.keep_delivery_stream_open();
    transport.push_delivery(Ok(delivery(1)));
    let config = single_worker_config(TopologyMode::Declare, worker_on("orders"));
    let pool = ClientPool::new(config, transport.clone());

    let consumer = pool
        .consumer("__auto__.emails")
        .await
        .expect("auto profile resolves");

    assert!(!transport.declared_queues().is_empty());
    assert!(
        transport
            .declared_queues()
            .iter()
            .any(|queue| queue == "emails")
    );
    // The consumer is usable: the scripted delivery surfaces.
    let item = consumer.next().await.expect("scripted delivery surfaces");
    assert_eq!(item.payload, Bytes::from_static(b"payload"));

    drop(consumer);
}

#[tokio::test(start_paused = true)]
async fn plain_unknown_names_still_error() {
    let transport = Arc::new(MockTransport::default());
    let config = single_worker_config(TopologyMode::Declare, worker_on("orders"));
    let pool = ClientPool::new(config, transport);

    let Err(error) = pool.consumer("orders-typo").await else {
        panic!("unknown profile must not resolve");
    };

    assert!(matches!(error.kind(), ClientErrorKind::Configuration));
    assert!(error.to_string().contains("unknown worker profile"));
}

#[tokio::test(start_paused = true)]
async fn synthesis_bound_is_enforced() {
    let transport = Arc::new(MockTransport::default());
    let config = single_worker_config(TopologyMode::Declare, worker_on("orders"));
    let pool = ClientPool::new(config, transport.clone());

    // 64 distinct auto names succeed; the 65th is rejected before any
    // broker contact. Pop-order errors surface on consumer().
    for index in 0..64 {
        let name = format!("__auto__.queue-{index}");
        // Establish and drop each consumer; the mock accepts everything.
        drop(pool.consumer(&name).await.expect("within bound"));
    }
    let Err(error) = pool.consumer("__auto__.queue-64").await else {
        panic!("over bound must not resolve");
    };
    assert!(error.to_string().contains("profile registry is full"));
    assert!(
        transport
            .declared_queues()
            .iter()
            .all(|q| q != "__auto__.queue-64")
    );
}

#[tokio::test(start_paused = true)]
async fn verify_mode_does_not_declare_auto_queues() {
    let transport = Arc::new(MockTransport::default());
    let config = single_worker_config(TopologyMode::Verify, worker_on("orders"));
    let pool = ClientPool::new(config, transport.clone());

    let consumer = pool
        .consumer("__auto__.queue-0")
        .await
        .expect("auto profile resolves; the queue must exist externally");

    // The honest observable: the mock accepts `verify_queue`, so the leak
    // shows up as a VerifyQueue operation for the synthesized queue — a
    // passive declare on a queue the broker has never seen. The reconcile
    // of the FIRST generation must not mention it. The synthesized
    // subscription's queue is the plain name (the `__auto__.` prefix names
    // the profile, not the queue).
    let verified_auto = transport.operations().iter().any(|operation| {
        matches!(
            operation,
            TransportOperation::VerifyQueue(spec) if spec.name == "queue-0"
        )
    });
    assert!(
        !verified_auto,
        "synthesized queue must not join the verify plan: the passive verify 404s and stalls every recovery generation"
    );
    assert!(transport.declared_queues().is_empty());

    drop(consumer);
}

#[tokio::test(start_paused = true)]
async fn recovery_re_establishes_synthesized_consumer() {
    let transport = Arc::new(MockTransport::default());
    let config = single_worker_config(TopologyMode::Declare, worker_on("orders"));
    let pool = ClientPool::new(config, transport.clone());

    let first = pool
        .consumer("__auto__.emails")
        .await
        .expect("auto profile resolves");
    assert!(
        transport
            .declared_queues()
            .iter()
            .any(|queue| queue == "emails")
    );

    // Let the first generation's delivery pump reach its first empty poll:
    // no delivery stream is kept open here, so it exits and cannot consume
    // the delivery scripted for the re-established subscription below.
    // The fixed yield count is deterministic: the pump exits on its first
    // empty poll, and the delivery is only pushed after these yields, so
    // pump exit strictly precedes the push regardless of scheduling.
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    // Script the drop + reconnect as tests/recovery.rs does, then advance
    // paused time until the coordinator completes the recovery generation.
    pool.simulate_connection_loss_for_tests("main", TransportError::connection("socket reset"))
        .await
        .expect("loss reported");
    // The delivery is scripted before recovery so the fresh subscription's
    // stream pops it on its first poll. The yields are deterministic: each
    // advance only unblocks the recovery step whose timer fired, and the
    // delivery is already queued, so interleaving pump tasks between steps
    // cannot reorder an observable outcome.
    transport.push_delivery(Ok(delivery(2)));
    for _ in 0..5 {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }

    let second = tokio::time::timeout(Duration::from_secs(10), pool.consumer("__auto__.emails"))
        .await
        .expect("consumer re-established after recovery")
        .expect("consumer handle");
    assert_ne!(
        first.generation(),
        second.generation(),
        "stale handle must be evicted after recovery"
    );

    let redeclarations = transport
        .declared_queues()
        .iter()
        .filter(|queue| queue.as_str() == "emails")
        .count();
    assert!(
        redeclarations >= 2,
        "queue must be declared again during recovery, got {redeclarations}"
    );

    // The re-established consumer is usable.
    let item = second.next().await.expect("delivery after recovery");
    assert_eq!(item.payload, Bytes::from_static(b"payload"));

    drop(first);
    drop(second);
    pool.close().await.expect("close pool");
}

#[tokio::test(start_paused = true)]
async fn auto_profile_declared_when_popped_after_publisher_use() {
    let transport = Arc::new(MockTransport::default());
    let config = single_worker_config(TopologyMode::Declare, worker_on("orders"));
    let pool = ClientPool::new(config, transport.clone());

    // 1. Publish one message through the pool (publisher-only) so the
    //    coordinator for "main" spawns with a plan that has no runtime
    //    queues.
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let request = PublishRequest::new(
        Destination::new("orders", "orders"),
        Bytes::from_static(b"payload"),
        MessageProperties::new("published-before-pop"),
        tokio::time::Instant::now() + Duration::from_secs(30),
    );
    pool.publish_batch(vec![("main".to_owned(), request)])
        .await
        .expect("publish through the fresh pool");
    assert!(!transport.declared_queues().contains(&"emails".to_owned()));

    // 2. The first pop synthesizes the profile, and the establish path must
    //    reconcile a FRESH plan (from_config_and) so "emails" is declared
    //    even though the generation was already reconciled without it.
    let consumer = pool
        .consumer("__auto__.emails")
        .await
        .expect("auto profile resolves");

    // 3.
    assert!(
        transport
            .declared_queues()
            .iter()
            .any(|queue| queue == "emails")
    );

    drop(consumer);
    pool.close().await.expect("close pool");
}
