//! Pool handle claims (issue #221): `Pool::close()` tears the shared
//! `ConnectionHandle` down with no use count, so every sibling pool built
//! from the same configuration fingerprint (the transient doctor/topology
//! probe pools) dies with it. The contract under test:
//!
//! 1. Two pools sharing one fingerprint acquire one handle, each holding a
//!    claim; closing the probe's claim leaves the live pool publishing.
//! 2. Closing the last claim tears the shared connection down and retires
//!    the handle (the registry replaces it on the next acquire).
//! 3. Releasing the last claim without an explicit close (the PHP
//!    destructor path) keeps the connection for registry reuse.

mod common;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    client::ClientPool,
    config::{
        BrokerConfig, Config, Credentials, Endpoint, PublisherConfigSection, TlsConfig,
        TopologyMode,
    },
    pool::{ConnectionHandle, ConnectionKey},
    publisher::{Destination, MessageProperties, PublishOutcome, PublishRequest},
    runtime::RuntimeRegistry,
    transport::{
        PublishConfirmation, QueueKind,
        mock::{MockTransport, TransportOperation},
    },
};

fn config(broker: &str) -> Arc<rabbit_rs_core::config::ValidatedConfig> {
    Arc::new(
        Config {
            brokers: vec![BrokerConfig {
                name: broker.to_owned(),
                hosts: vec![Endpoint::new("rabbit.local", 5672)],
                vhost: "/".to_owned(),
                credentials: Credentials::new("guest", "secret"),
                tls: TlsConfig::disabled(),
                heartbeat: Duration::from_secs(30),
            }],
            workers: Vec::new(),
            topology_mode: TopologyMode::External,
            routes: BTreeMap::new(),
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

fn request(message_id: &str) -> PublishRequest {
    PublishRequest::new(
        Destination::new("jobs", "default"),
        Bytes::from_static(b"payload"),
        MessageProperties::new(message_id),
        tokio::time::Instant::now() + Duration::from_secs(30),
    )
}

fn confirmed(outcomes: &[PublishOutcome], message_id: &str) -> bool {
    matches!(
        outcomes,
        [PublishOutcome::Confirmed { message_id: id }] if id.as_ref() == message_id
    )
}

#[tokio::test(start_paused = true)]
async fn a_live_pool_keeps_working_after_a_probe_pool_closes() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let registry = RuntimeRegistry::new();
    let key = ConnectionKey::from_config(config("default").as_ref());

    // Two pools sharing one fingerprint acquire one shared handle.
    let live = registry.acquire(key).expect("live pool handle");
    let probe = registry.acquire(key).expect("probe pool handle");
    assert!(
        Arc::ptr_eq(&live, &probe),
        "one fingerprint must share one handle"
    );

    let client = Arc::new(ClientPool::new(config("default"), transport.clone()));

    // The live pool publishes through the shared connection.
    let first = client
        .publish_batch(vec![("default".into(), request("live"))])
        .await
        .expect("live publish");
    assert!(confirmed(&first, "live"));

    // The probe pool closes. Its close must not tear the shared connection
    // down under the live pool.
    probe
        .close_claim(client.as_ref())
        .await
        .expect("probe close");

    let second = client
        .publish_batch(vec![("default".into(), request("after-probe"))])
        .await
        .expect("live pool must keep working after the probe pool closed");
    assert!(confirmed(&second, "after-probe"));
    assert!(
        !client.is_closed(),
        "the shared client must stay open while another claim lives"
    );
    assert!(!live.is_closed());

    // Closing the last claim tears the shared connection down and retires
    // the handle.
    live.close_claim(client.as_ref()).await.expect("live close");
    assert!(client.is_closed());
    assert!(live.is_closed());
    assert!(
        transport
            .operations()
            .iter()
            .any(|operation| matches!(operation, TransportOperation::CloseConnection)),
        "the last close must tear the shared connection down"
    );

    // The registry replaces the retired handle on the next acquire.
    let fresh = registry.acquire(key).expect("fresh handle");
    assert!(
        !Arc::ptr_eq(&fresh, &live),
        "the retired handle is replaced"
    );

    // The registry owns a real runtime that must not be dropped inside this
    // async context; the process reclaims it at exit.
    std::mem::forget(registry);
}

#[tokio::test(start_paused = true)]
async fn releasing_the_last_claim_without_a_close_keeps_the_connection_for_reuse() {
    let transport = Arc::new(MockTransport::default());
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let registry = RuntimeRegistry::new();
    let key = ConnectionKey::from_config(config("default").as_ref());

    let handle = registry.acquire(key).expect("pool handle");
    let client = Arc::new(ClientPool::new(config("default"), transport.clone()));
    let outcomes = client
        .publish_batch(vec![("default".into(), request("warm"))])
        .await
        .expect("warm publish");
    assert!(confirmed(&outcomes, "warm"));

    // The PHP destructor path: the claim is released without an explicit
    // close. The shared connection must stay available for reuse.
    assert!(handle.release_claim());
    assert!(
        !client.is_closed(),
        "a claim release must not tear the connection down"
    );
    assert!(!handle.is_closed());

    // The registry hands the same handle out again.
    let again = registry.acquire(key).expect("reacquire");
    assert!(Arc::ptr_eq(&again, &handle), "the handle is reused");

    // See the sibling test: the registry's runtime must outlive this context.
    std::mem::forget(registry);
}

/// A worker building pools for rotating fingerprints drops each pool object
/// without an explicit close (zero live claims, the PHP destructor path).
/// The registry must not accumulate those pools forever: past its cap, the
/// oldest zero-claim pool is evicted and closed through the regular close
/// path, and the registry stays bounded.
#[tokio::test(start_paused = true)]
async fn registry_eviction_bounds_the_process_pool_count() {
    let registry = RuntimeRegistry::new();

    // 17 distinct fingerprints, each released without a close (zero live
    // claims), then one more acquire: the registry must evict the oldest
    // idle pools instead of growing past its cap.
    let handles: Vec<Arc<ConnectionHandle>> = (0..18)
        .map(|index| {
            let key = ConnectionKey::from_config(config(&format!("pool-{index}")).as_ref());
            let handle = registry.acquire(key).expect("pool handle");
            assert!(handle.release_claim(), "the test owns exactly one claim");
            handle
        })
        .collect();

    // The two oldest idle pools were evicted and closed; every pool within
    // the cap stays open.
    assert!(
        handles[0].is_closed(),
        "the oldest idle pool must be evicted"
    );
    assert!(
        handles[1].is_closed(),
        "the next acquire must evict the next-oldest idle pool"
    );
    for (index, handle) in handles.iter().enumerate().skip(2) {
        assert!(
            !handle.is_closed(),
            "idle pool {index} is within the cap and must stay open"
        );
    }

    // The evicted fingerprint gets a fresh handle on the next acquire.
    let fresh = registry
        .acquire(ConnectionKey::from_config(config("pool-0").as_ref()))
        .expect("handle for the evicted fingerprint");
    assert!(!fresh.is_closed());
    assert!(
        !Arc::ptr_eq(&fresh, &handles[0]),
        "the evicted handle must be replaced"
    );

    // See the sibling tests: the registry's runtime must outlive this
    // context; the process reclaims it at exit.
    std::mem::forget(registry);
}
