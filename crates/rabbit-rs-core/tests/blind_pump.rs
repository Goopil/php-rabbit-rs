//! Cross-module behavior tests for blind-mode routing through the pipelined
//! publish pump: `SafetyMode::Blind` publishes must hand off to the pump with
//! zero outcome-waiting at the caller, and `ClientPool::flush_blind` must act
//! as a barrier over everything enqueued before it.

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    client::{ClientErrorKind, ClientPool},
    config::{
        BrokerConfig, Config, Credentials, DelayConfig, Endpoint, PublisherConfigSection,
        SafetyMode, TlsConfig, TopologyMode, ValidatedConfig,
    },
    publisher::{Destination, MessageProperties, PublishOutcome, PublishRequest, PublisherConfig},
    transport::{
        PublishConfirmation, QueueKind,
        mock::{MockOperationGate, MockTransport},
    },
};

mod common;

use common::{publish_requests, wait_for_publish_count as wait_for_publishes};

fn config() -> Arc<ValidatedConfig> {
    Arc::new(
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
            delay: DelayConfig::default(),
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

fn blind_pool(transport: &Arc<MockTransport>, capacity: usize) -> ClientPool {
    ClientPool::new_for_tests(
        config(),
        transport.clone(),
        PublisherConfig::with_safety(capacity, Duration::from_secs(5), SafetyMode::Blind),
    )
}

fn blind_pool_with_byte_budget(
    transport: &Arc<MockTransport>,
    capacity: usize,
    max_buffered_bytes: u64,
) -> ClientPool {
    ClientPool::new_for_tests(
        config(),
        transport.clone(),
        PublisherConfig {
            max_buffered_bytes,
            ..PublisherConfig::with_safety(capacity, Duration::from_secs(5), SafetyMode::Blind)
        },
    )
}

fn oversized_request(message_id: &str, payload_bytes: usize) -> PublishRequest {
    PublishRequest::new(
        Destination::new("jobs", "default"),
        Bytes::from(vec![0u8; payload_bytes]),
        MessageProperties::new(message_id),
        tokio::time::Instant::now() + Duration::from_secs(30),
    )
}

fn request(message_id: &str) -> PublishRequest {
    PublishRequest::new(
        Destination::new("jobs", "default"),
        Bytes::from_static(b"payload"),
        MessageProperties::new(message_id),
        tokio::time::Instant::now() + Duration::from_secs(30),
    )
}

fn batch(message_ids: &[&str]) -> Vec<(String, PublishRequest)> {
    message_ids
        .iter()
        .map(|id| ("default".to_owned(), request(id)))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn blind_batch_returns_while_every_publish_is_still_pending() {
    let transport = Arc::new(MockTransport::default());
    let gates: Vec<MockOperationGate> = (0..4).map(|_| transport.push_publish_gate()).collect();
    let pool = Arc::new(blind_pool(&transport, 8));

    let batch = tokio::spawn({
        let pool = Arc::clone(&pool);
        async move { pool.publish_batch(batch(&["m0", "m1", "m2", "m3"])).await }
    });

    let outcomes = tokio::time::timeout(Duration::from_secs(1), batch)
        .await
        .expect("blind batch must return while every publish is still pending")
        .expect("batch task joins")
        .expect("batch succeeds");
    assert_eq!(
        outcomes,
        vec![
            PublishOutcome::Confirmed {
                message_id: "m0".into()
            },
            PublishOutcome::Confirmed {
                message_id: "m1".into()
            },
            PublishOutcome::Confirmed {
                message_id: "m2".into()
            },
            PublishOutcome::Confirmed {
                message_id: "m3".into()
            },
        ],
        "blind outcomes are the synthetic Confirmed resolved at hand-off"
    );
    assert!(
        publish_requests(&transport).is_empty(),
        "every publish must still be pending on its gate when the batch returns"
    );

    // Releasing the gates one at a time proves the pump preserves the intake
    // order on its way to the transport.
    for (index, gate) in gates.iter().enumerate() {
        assert!(gate.release(), "gate {index} released");
        wait_for_publishes(&transport, index + 1).await;
    }
    let ids: Vec<Option<String>> = publish_requests(&transport)
        .into_iter()
        .map(|request| request.properties.message_id)
        .collect();
    assert_eq!(
        ids,
        vec![
            Some("m0".into()),
            Some("m1".into()),
            Some("m2".into()),
            Some("m3".into())
        ],
        "enfilage order must be preserved at the transport entrance"
    );

    pool.close().await.expect("close pool");
}

#[tokio::test(start_paused = true)]
async fn blind_batch_bypasses_the_publisher_actor() {
    let transport = Arc::new(MockTransport::default());
    let gates: Vec<MockOperationGate> = (0..2).map(|_| transport.push_publish_gate()).collect();
    let pool = Arc::new(blind_pool(&transport, 8));

    pool.publish_batch(batch(&["m0", "m1"]))
        .await
        .expect("batch enqueued");
    gates[0].wait_entered().await;
    gates[1].wait_entered().await;

    // The actor path retains one capacity permit per pending publication; the
    // pump path does not. Zero in-flight permits while both publishes are
    // still pending proves no publish command ever reached the actor.
    let (in_flight, capacity) = pool.publisher_utilization();
    assert_eq!(
        in_flight, 0,
        "no actor permit may be retained in blind mode"
    );
    assert_eq!(capacity, 8);

    for gate in &gates {
        assert!(gate.release());
    }
    wait_for_publishes(&transport, 2).await;

    pool.close().await.expect("close pool");
}

#[tokio::test(start_paused = true)]
async fn blind_single_publish_resolves_confirmed_without_waiting_for_the_transport() {
    let transport = Arc::new(MockTransport::default());
    let gate = transport.push_publish_gate();
    let pool = Arc::new(blind_pool(&transport, 8));

    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        pool.publish_batch(vec![("default".to_owned(), request("m0"))]),
    )
    .await
    .expect("blind publish must resolve at hand-off, not on the transport")
    .expect("blind publish succeeds")
    .pop()
    .expect("blind publish succeeds");
    assert_eq!(
        outcome,
        PublishOutcome::Confirmed {
            message_id: "m0".into()
        }
    );
    assert!(
        publish_requests(&transport).is_empty(),
        "gated publish must not be recorded before its gate is released"
    );

    assert!(gate.release());
    wait_for_publishes(&transport, 1).await;
    assert_eq!(
        publish_requests(&transport)[0]
            .properties
            .message_id
            .as_deref(),
        Some("m0")
    );

    pool.close().await.expect("close pool");
}

#[tokio::test(start_paused = true)]
async fn flush_blind_resolves_only_after_pending_publishes_reach_the_transport() {
    let transport = Arc::new(MockTransport::default());
    let gate = transport.push_publish_gate();
    let pool = Arc::new(blind_pool(&transport, 8));

    pool.publish_batch(batch(&["m0"]))
        .await
        .expect("batch enqueued");
    gate.wait_entered().await;

    // A full simulated second must elapse without the flush resolving. The
    // timeout expiry is positive proof the flush barrier was enqueued behind
    // the gated publish and stayed parked — the assertion can no longer pass
    // merely because the flush has not been scheduled yet (D2 non-vacuous
    // assert, blind sibling of the pump-level test).
    let flush = tokio::spawn({
        let pool = Arc::clone(&pool);
        async move { pool.flush_blind().await }
    });
    tokio::time::timeout(Duration::from_secs(1), flush)
        .await
        .expect_err("flush must not resolve while the gated publish has not reached the transport");
    assert!(publish_requests(&transport).is_empty());

    // Releasing the gate hands the publish to the transport: a fresh flush
    // must then resolve promptly.
    assert!(gate.release());
    wait_for_publishes(&transport, 1).await;
    tokio::time::timeout(Duration::from_secs(1), pool.flush_blind())
        .await
        .expect("flush must not hang once the transport accepted everything")
        .expect("flush succeeds");

    pool.close().await.expect("close pool");
}

#[tokio::test(start_paused = true)]
async fn flush_blind_resolves_immediately_on_non_blind_clients() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let pool = ClientPool::new(config(), transport.clone());

    // No cached publisher yet: must succeed without touching the transport.
    tokio::time::timeout(Duration::from_secs(1), pool.flush_blind())
        .await
        .expect("flush must not hang on an empty pool")
        .expect("flush on an empty pool succeeds");

    pool.publish_batch(vec![("default".to_owned(), request("m0"))])
        .await
        .expect("safe publish");
    tokio::time::timeout(Duration::from_secs(1), pool.flush_blind())
        .await
        .expect("flush must not hang on a non-blind publisher")
        .expect("flush on a non-blind publisher succeeds");

    pool.close().await.expect("close pool");
}

#[tokio::test(start_paused = true)]
async fn blind_publish_respects_the_byte_budget() {
    let transport = Arc::new(MockTransport::default());
    // 1 MiB budget: one parked 600 KiB publish must reject a second one,
    // exactly like the confirmed path rejects on byte-budget exhaustion.
    let gates: Vec<MockOperationGate> = (0..2).map(|_| transport.push_publish_gate()).collect();
    let pool = Arc::new(blind_pool_with_byte_budget(&transport, 8, 1024 * 1024));

    pool.publish_batch(vec![(
        "default".to_owned(),
        oversized_request("m0", 600 * 1024),
    )])
    .await
    .expect("first publish fits the byte budget");
    gates[0].wait_entered().await;

    let error = pool
        .publish_batch(vec![(
            "default".to_owned(),
            oversized_request("m1", 600 * 1024),
        )])
        .await
        .expect_err("second concurrent publish exceeds the 1 MiB byte budget");
    assert_eq!(
        error.kind(),
        ClientErrorKind::Backpressure,
        "blind publishes must surface byte-budget exhaustion as Backpressure, got {error}"
    );
    assert!(
        publish_requests(&transport).is_empty(),
        "a byte-budget rejection must not enqueue anything on the transport"
    );

    // Completing the parked publish releases its reservation at transport
    // exit: the same request is then accepted again (bounded, not leaked).
    assert!(gates[0].release());
    wait_for_publishes(&transport, 1).await;
    pool.publish_batch(vec![(
        "default".to_owned(),
        oversized_request("m1", 600 * 1024),
    )])
    .await
    .expect("byte budget must be released at transport exit");
    assert!(gates[1].release());
    wait_for_publishes(&transport, 2).await;

    pool.close().await.expect("close pool");
}

#[tokio::test(start_paused = true)]
async fn blind_batch_on_a_closed_pump_fails_immediately_and_leaves_everything_with_the_caller() {
    let transport = Arc::new(MockTransport::default());
    let pool = Arc::new(blind_pool(&transport, 8));

    // Install a publisher whose blind pump is already closed while the pool
    // itself stays open.
    pool.install_closed_pump_publisher_for_tests("default")
        .await
        .expect("closed-pump publisher installed");

    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        pool.publish_batch(batch(&["m0", "m1", "m2"])),
    )
    .await
    .expect("batch on a closed pump must fail immediately, not hang");

    let error = outcome.expect_err("batch on a closed pump must fail");
    assert_eq!(
        error.kind(),
        ClientErrorKind::Closed,
        "the closed pump must surface a Closed client error, got {error}"
    );

    // Nothing was enqueued, so the caller re-buffers a conservative superset
    // (here: the whole batch) — no request may have reached the transport,
    // and no synthetic `Confirmed` outcome may be produced.
    assert!(
        publish_requests(&transport).is_empty(),
        "a batch rejected by a closed pump must not enqueue any publish"
    );

    pool.close().await.expect("close pool");
}

#[tokio::test(start_paused = true)]
async fn blind_batch_applies_backpressure_then_completes_without_error() {
    let transport = Arc::new(MockTransport::default());
    // buffer_capacity = 2 → intake queue of 2, in-flight cap 128.
    let gates: Vec<MockOperationGate> = (0..131).map(|_| transport.push_publish_gate()).collect();
    let pool = Arc::new(blind_pool(&transport, 2));

    let batch = tokio::spawn({
        let pool = Arc::clone(&pool);
        async move {
            let requests: Vec<(String, PublishRequest)> = (0..131)
                .map(|i| {
                    let id = i.to_string();
                    ("default".to_owned(), request(&id))
                })
                .collect();
            pool.publish_batch(requests).await
        }
    });

    // 128 publishes are held pending in the pump's in-flight set; the two
    // intake slots absorb the next two; the batch must then block on the
    // 131st hand-off (backpressure by blocking, not an error).
    for gate in &gates[..128] {
        gate.wait_entered().await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !batch.is_finished(),
        "publish_batch must block once the pump queue and in-flight cap are full"
    );

    // Completing one publish frees an in-flight slot: the batch drains.
    assert!(gates[0].release(), "gate released");
    let outcomes = tokio::time::timeout(Duration::from_secs(1), batch)
        .await
        .expect("blocked batch must complete once backpressure clears")
        .expect("batch task joins")
        .expect("batch completes without error");
    assert_eq!(outcomes.len(), 131);
    for (index, outcome) in outcomes.iter().enumerate() {
        assert_eq!(
            outcome,
            &PublishOutcome::Confirmed {
                message_id: index.to_string().into()
            }
        );
    }

    // gates[0] was already released above; re-releasing is a no-op that
    // returns false, so the recorded count below is the real verification.
    for gate in &gates {
        let _ = gate.release();
    }
    wait_for_publishes(&transport, 131).await;

    pool.close().await.expect("close pool");
}
