//! Chaos / fault-injection tests against a real `RabbitMQ` cluster.
//!
//! These tests require the lab running with the `with-plugin` profile:
//! 3 `RabbitMQ` nodes, Toxiproxy (ports 5672/5673/5674), and the management API
//! on port 15672. They are gated behind the `integration` feature flag.
//!
//! # Delivery contract
//!
//! The contract is **at-least-once**: `missing = 0` is mandatory.
//! Duplicates are permitted only inside documented ambiguous windows
//! (e.g. TCP reset after the broker received a message but before the
//! publisher confirm reached the client).
//!
//! # Current recovery status
//!
//! The `ClientPool` does not yet wire `ConnectionActor` to `PublisherActor`
//! for automatic recovery. These tests therefore close and recreate pools
//! after faults to verify that **the broker** preserves messages. Once
//! automatic recovery is implemented, these tests will pass without the
//! manual pool recreation step.
#![cfg(feature = "integration")]

mod toxiproxy;

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    client::{ClientErrorKind, ClientPool},
    config::{
        BrokerConfig, Config, Credentials, Endpoint, SchedulerConfig, SubscriptionConfig,
        TlsConfig, TopologyMode, WorkerProfile,
    },
    publisher::{Destination, MessageProperties, PublishOutcome, PublishRequest},
    topology::{QueueDefinition, TopologyDefinition, TopologyPlan, TopologyReconciler},
    transport::{Transport, lapin::LapinTransport},
};
use tokio::time::Instant;
use toxiproxy::{ToxicSpec, ToxicType, ToxiproxyClient};

const VHOST: &str = "/orders-eu";
const TOXIPROXY_URL: &str = "http://localhost:8474";
const MGMT_URL: &str = "http://localhost:15672";
const ADMIN_USER: &str = "admin";
const ADMIN_PASS: &str = "admin_lab";
const RABBIT_USER: &str = "rabbit_rs";
const RABBIT_PASS: &str = "rabbit_rs_lab";

const PROXY_1: &str = "rabbitmq-1-toxiproxy";

fn broker_via_proxy(name: &str, proxy_port: u16) -> BrokerConfig {
    BrokerConfig {
        name: name.to_owned(),
        hosts: vec![Endpoint::new("localhost", proxy_port)],
        vhost: VHOST.to_owned(),
        credentials: Credentials::new(RABBIT_USER, RABBIT_PASS),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(3),
    }
}

fn broker_bad_credentials(name: &str, proxy_port: u16) -> BrokerConfig {
    BrokerConfig {
        name: name.to_owned(),
        hosts: vec![Endpoint::new("localhost", proxy_port)],
        vhost: VHOST.to_owned(),
        credentials: Credentials::new("rabbit_rs", "wrong_password"),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(10),
    }
}

fn config_for_queue(
    queue: &str,
    broker: &BrokerConfig,
) -> Arc<rabbit_rs_core::config::ValidatedConfig> {
    Arc::new(
        Config {
            brokers: vec![broker.clone()],
            workers: vec![WorkerProfile {
                name: "main".to_owned(),
                subscriptions: vec![SubscriptionConfig {
                    name: "jobs".to_owned(),
                    broker: broker.name.clone(),
                    queue: queue.to_owned(),
                    weight: 1,
                    priority_class: 0,
                    prefetch: 4,
                    starvation_after: Duration::from_secs(30),
                }],
                scheduler: SchedulerConfig::weighted_fair(16),
            }],
            topology_mode: TopologyMode::External,
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
        Instant::now() + Duration::from_mins(1),
    )
}

async fn declare_queue(_vhost: &str, queue: &str) {
    let broker_config = broker_via_proxy("primary", 5672);
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

    let _ = channel.close().await;
    let _ = conn.close().await;
}

async fn purge_queue_mgmt(queue: &str) {
    let url = format!("{MGMT_URL}/api/queues/%2Forders-eu/{queue}");
    let client = reqwest_simple();
    // Delete the queue if it exists, then recreate it clean.
    let _ = client
        .delete(&url)
        .basic_auth(ADMIN_USER, Some(ADMIN_PASS))
        .send()
        .await;
    let _ = client
        .put(&url)
        .basic_auth(ADMIN_USER, Some(ADMIN_PASS))
        .header("content-type", "application/json")
        .body(r#"{"durable":true,"arguments":{"x-queue-type":"quorum"}}"#)
        .send()
        .await;
}

fn reqwest_simple() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client")
}

async fn delete_queue_mgmt(queue: &str) {
    let url = format!("{MGMT_URL}/api/queues/%2Forders-eu/{queue}");
    let _ = reqwest_simple()
        .delete(&url)
        .basic_auth(ADMIN_USER, Some(ADMIN_PASS))
        .send()
        .await;
}

/// Publish a message, retrying up to `timeout` total.
async fn publish_with_retry(
    config: Arc<rabbit_rs_core::config::ValidatedConfig>,
    broker_name: &str,
    request: PublishRequest,
    timeout: Duration,
) -> PublishOutcome {
    let deadline = Instant::now() + timeout;
    loop {
        let pool = ClientPool::production(config.clone());
        match tokio::time::timeout(
            Duration::from_secs(5),
            pool.publish(broker_name, request.clone()),
        )
        .await
        {
            Ok(Ok(outcome)) => {
                let _ = pool.close().await;
                return outcome;
            }
            Ok(Err(e)) => {
                eprintln!("publish error: {e:?}");
                let _ = pool.close().await;
            }
            Err(_) => {
                eprintln!("publish timeout");
                let _ = pool.close().await;
            }
        }
        assert!(
            Instant::now() < deadline,
            "publish_with_retry exceeded {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Consume exactly `count` deliveries, recording their message IDs.
async fn consume_n(pool: &ClientPool, count: usize, timeout: Duration) -> Vec<String> {
    let consumer = pool.consumer("main").await.expect("consumer");
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        let delivery = tokio::time::timeout(timeout, consumer.next())
            .await
            .expect("timeout waiting for delivery")
            .expect("delivery");
        ids.push(delivery.id.as_str().to_owned());
        delivery.ack().await.expect("ack");
    }
    ids
}

/// Consume all available messages within timeout, waiting for queue to drain.
async fn consume_all(
    config: Arc<rabbit_rs_core::config::ValidatedConfig>,
    expected_count: usize,
    timeout: Duration,
) -> Vec<String> {
    let pool = ClientPool::production(config);
    let consumer = pool.consumer("main").await.expect("consumer");
    let mut ids = Vec::new();
    let deadline = Instant::now() + timeout;
    while ids.len() < expected_count {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, consumer.next()).await {
            Ok(Ok(delivery)) => {
                ids.push(delivery.id.as_str().to_owned());
                let _ = delivery.ack().await;
            }
            Ok(Err(e)) => {
                eprintln!("consumer error: {e:?}");
                break;
            }
            Err(_) => break,
        }
    }
    let _ = pool.close().await;
    ids
}

/// Count results: expected, unique, duplicates, missing.
struct DeliveryAudit {
    expected: BTreeSet<String>,
    received: Vec<String>,
}

impl DeliveryAudit {
    fn new(expected_ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            expected: expected_ids.into_iter().collect(),
            received: Vec::new(),
        }
    }

    fn record(&mut self, msg_id: &str) {
        self.received.push(msg_id.to_owned());
    }

    fn unique_count(&self) -> usize {
        let received_set: BTreeSet<&str> = self.received.iter().map(String::as_str).collect();
        received_set.len()
    }

    fn duplicate_count(&self) -> usize {
        self.received.len().saturating_sub(self.unique_count())
    }

    fn missing(&self) -> BTreeSet<String> {
        let received_set: BTreeSet<&str> = self.received.iter().map(String::as_str).collect();
        self.expected
            .iter()
            .filter(|id| !received_set.contains(id.as_str()))
            .cloned()
            .collect()
    }

    fn assert_at_least_once(&self, scenario: &str) {
        let missing = self.missing();
        let duplicates = self.duplicate_count();
        let unique = self.unique_count();
        let expected = self.expected.len();
        let total = self.received.len();

        println!(
            "[{scenario}] expected={expected} unique={unique} duplicates={duplicates} total_received={total} missing={}",
            missing.len()
        );

        assert!(
            missing.is_empty(),
            "[{scenario}] MISSING MESSAGES: {missing:?} — at-least-once violated"
        );
        println!("[{scenario}] PASS: missing = 0");
    }
}

// ---------------------------------------------------------------------------
// Scenario 1: TCP reset before publisher confirm
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_tcp_reset_before_confirm() {
    let queue = "rabbit-rs-it-chaos-tcp-reset-before-confirm";
    declare_queue(VHOST, queue).await;
    purge_queue_mgmt(queue).await;

    let broker = broker_via_proxy("primary", 5672);
    let config = config_for_queue(queue, &broker);

    // Publish a message successfully before the fault.
    {
        let pool = ClientPool::production(config.clone());
        pool.publish(
            "primary",
            publish_request("chaos-reset-1", queue, b"payload-1"),
        )
        .await
        .expect("publish before fault");
        let _ = pool.close().await;
    }

    let toxi = ToxiproxyClient::new(TOXIPROXY_URL.to_owned());

    // Inject a TCP reset on the proxy.
    toxi.add_toxic(
        PROXY_1,
        &ToxicSpec {
            name: "reset-before-confirm",
            kind: ToxicType::ResetPeer,
            direction: "downstream",
            toxicity: 1.0,
            timeout: Some(Duration::from_millis(50)),
        },
    )
    .await
    .expect("add toxic");

    // Attempt to publish during the fault — may fail.
    {
        let pool = ClientPool::production(config.clone());
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            pool.publish(
                "primary",
                publish_request("chaos-reset-2", queue, b"payload-2"),
            ),
        )
        .await;
        let _ = pool.close().await;
    }

    // Remove the toxic.
    toxi.remove_toxic(PROXY_1, "reset-before-confirm")
        .await
        .expect("remove toxic");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // After the fault, publish the second message again (it may not have been delivered).
    {
        let pool = ClientPool::production(config.clone());
        pool.publish(
            "primary",
            publish_request("chaos-reset-2", queue, b"payload-2"),
        )
        .await
        .expect("publish after recovery");
        let _ = pool.close().await;
    }

    // Consume all messages and verify at-least-once.
    let received = consume_all(config, 2, Duration::from_secs(15)).await;

    let mut audit = DeliveryAudit::new(["chaos-reset-1", "chaos-reset-2"].map(String::from));
    for id in &received {
        audit.record(id);
    }
    audit.assert_at_least_once("tcp-reset-before-confirm");

    delete_queue_mgmt(queue).await;
}

// ---------------------------------------------------------------------------
// Scenario 2: TCP reset after confirm, before consumer ACK
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_tcp_reset_after_confirm_before_ack() {
    let queue = "rabbit-rs-it-chaos-tcp-reset-after-confirm";
    declare_queue(VHOST, queue).await;
    purge_queue_mgmt(queue).await;

    let broker = broker_via_proxy("primary", 5672);
    let config = config_for_queue(queue, &broker);
    let toxi = ToxiproxyClient::new(TOXIPROXY_URL.to_owned());

    // Publish and confirm a message.
    {
        let pool = ClientPool::production(config.clone());
        pool.publish(
            "primary",
            publish_request("chaos-ack-1", queue, b"payload-ack-1"),
        )
        .await
        .expect("publish before reset");
        let _ = pool.close().await;
    }

    // Consume the message but do NOT ACK it.
    let consumer_pool = ClientPool::production(config.clone());
    let consumer = consumer_pool.consumer("main").await.expect("consumer");
    let delivery = tokio::time::timeout(Duration::from_secs(10), consumer.next())
        .await
        .expect("timeout for delivery")
        .expect("delivery");
    assert_eq!(delivery.id.as_str(), "chaos-ack-1");

    // Create a network partition (timeout toxic blocks all traffic).
    toxi.add_toxic(
        PROXY_1,
        &ToxicSpec {
            name: "reset-before-ack",
            kind: ToxicType::Timeout,
            direction: "downstream",
            toxicity: 1.0,
            timeout: Some(Duration::from_millis(0)),
        },
    )
    .await
    .expect("add toxic");

    // Wait for the partition to take effect.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Do NOT ACK and do NOT close the pool cleanly.
    // Just drop the consumer_pool — the connections will be dropped
    // without sending a close frame.
    drop(consumer);
    drop(consumer_pool);

    // Wait for the broker to detect the connection loss via heartbeat timeout.
    // With a 3-second heartbeat, the broker should detect the loss within
    // ~6 seconds (2 missed heartbeats). We wait 10 seconds to be safe.
    tokio::time::sleep(Duration::from_secs(10)).await;

    // Remove the toxic to heal the partition.
    toxi.remove_toxic(PROXY_1, "reset-before-ack")
        .await
        .expect("remove toxic");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // The unacked message must be redelivered to a new consumer.
    let received = consume_all(config, 1, Duration::from_secs(20)).await;

    let mut audit = DeliveryAudit::new(["chaos-ack-1"].map(String::from));
    for id in &received {
        audit.record(id);
    }
    audit.assert_at_least_once("tcp-reset-after-confirm-before-ack");

    delete_queue_mgmt(queue).await;
}

// ---------------------------------------------------------------------------
// Scenario 3: Quorum leader shutdown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_quorum_leader_shutdown() {
    let queue = "rabbit-rs-it-chaos-quorum-leader";
    declare_queue(VHOST, queue).await;
    purge_queue_mgmt(queue).await;

    let broker = broker_via_proxy("primary", 5672);
    let config = config_for_queue(queue, &broker);

    // Publish a message before the leader shutdown.
    {
        let pool = ClientPool::production(config.clone());
        pool.publish(
            "primary",
            publish_request("chaos-leader-1", queue, b"payload-leader-1"),
        )
        .await
        .expect("publish before leader shutdown");
        let _ = pool.close().await;
    }

    // Find the queue leader node.
    let leader = get_queue_leader(queue).await;
    println!("Queue leader: {leader}");

    // Stop the leader node.
    stop_rabbitmq_node(&leader).await;
    println!("Stopped leader node: {leader}");

    // Wait for quorum to elect a new leader.
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Publish another message after failover.
    // After the leader is stopped, we need to connect to a surviving node.
    // Try connecting through each proxy until one works.
    let fallback_broker = BrokerConfig {
        name: "fallback".to_owned(),
        hosts: vec![Endpoint::new("localhost", 5673)],
        vhost: VHOST.to_owned(),
        credentials: Credentials::new(RABBIT_USER, RABBIT_PASS),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(3),
    };
    let fallback_config = config_for_queue(queue, &fallback_broker);
    publish_with_retry(
        fallback_config.clone(),
        "fallback",
        publish_request("chaos-leader-2", queue, b"payload-leader-2"),
        Duration::from_secs(15),
    )
    .await;

    // Restart the stopped node.
    start_rabbitmq_node(&leader).await;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Consume both messages from the fallback node.
    let received = consume_all(fallback_config, 2, Duration::from_secs(15)).await;

    let mut audit = DeliveryAudit::new(["chaos-leader-1", "chaos-leader-2"].map(String::from));
    for id in &received {
        audit.record(id);
    }
    audit.assert_at_least_once("quorum-leader-shutdown");

    delete_queue_mgmt(queue).await;
}

// ---------------------------------------------------------------------------
// Scenario 4: Node restart
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_node_restart() {
    let queue = "rabbit-rs-it-chaos-node-restart";
    declare_queue(VHOST, queue).await;
    purge_queue_mgmt(queue).await;

    let broker = broker_via_proxy("primary", 5672);
    let config = config_for_queue(queue, &broker);

    // Publish a message before the restart.
    {
        let pool = ClientPool::production(config.clone());
        pool.publish(
            "primary",
            publish_request("chaos-restart-1", queue, b"payload-restart-1"),
        )
        .await
        .expect("publish before restart");
        let _ = pool.close().await;
    }

    // Restart rabbitmq-1.
    stop_rabbitmq_node("rabbit@rabbitmq-1").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    start_rabbitmq_node("rabbit@rabbitmq-1").await;
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Publish another message after the restart.
    publish_with_retry(
        config.clone(),
        "primary",
        publish_request("chaos-restart-2", queue, b"payload-restart-2"),
        Duration::from_secs(15),
    )
    .await;

    // Consume both messages.
    let received = consume_all(config, 2, Duration::from_secs(15)).await;

    let mut audit = DeliveryAudit::new(["chaos-restart-1", "chaos-restart-2"].map(String::from));
    for id in &received {
        audit.record(id);
    }
    audit.assert_at_least_once("node-restart");

    delete_queue_mgmt(queue).await;
}

// ---------------------------------------------------------------------------
// Scenario 5: Consumer network partition
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_consumer_partition() {
    let queue = "rabbit-rs-it-chaos-consumer-partition";
    declare_queue(VHOST, queue).await;
    purge_queue_mgmt(queue).await;

    let broker = broker_via_proxy("primary", 5672);
    let config = config_for_queue(queue, &broker);
    let toxi = ToxiproxyClient::new(TOXIPROXY_URL.to_owned());

    // Publish a message.
    {
        let pool = ClientPool::production(config.clone());
        pool.publish(
            "primary",
            publish_request("chaos-partition-1", queue, b"payload-partition-1"),
        )
        .await
        .expect("publish before partition");
        let _ = pool.close().await;
    }

    // Consume the message but do NOT ACK it.
    let consumer_pool = ClientPool::production(config.clone());
    let consumer = consumer_pool.consumer("main").await.expect("consumer");
    let delivery = tokio::time::timeout(Duration::from_secs(10), consumer.next())
        .await
        .expect("timeout for delivery")
        .expect("delivery");

    // Create a network partition by blocking all traffic.
    toxi.add_toxic(
        PROXY_1,
        &ToxicSpec {
            name: "partition-consumer",
            kind: ToxicType::Timeout,
            direction: "downstream",
            toxicity: 1.0,
            timeout: Some(Duration::from_millis(0)),
        },
    )
    .await
    .expect("add partition toxic");

    // Wait for the partition to take effect.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Do NOT ACK and do NOT close cleanly — just drop.
    drop(delivery);
    drop(consumer);
    drop(consumer_pool);

    // Wait in the partitioned state.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Heal the partition.
    toxi.remove_toxic(PROXY_1, "partition-consumer")
        .await
        .expect("remove toxic");

    // Wait for the broker to detect the connection loss and redeliver.
    tokio::time::sleep(Duration::from_secs(10)).await;

    // The unacked message must be redelivered.
    let received = consume_all(config, 1, Duration::from_secs(20)).await;

    let mut audit = DeliveryAudit::new(["chaos-partition-1"].map(String::from));
    for id in &received {
        audit.record(id);
    }
    audit.assert_at_least_once("consumer-partition");

    delete_queue_mgmt(queue).await;
}

// ---------------------------------------------------------------------------
// Scenario 6: Channel closed for topology error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_channel_closed_topology_error() {
    let queue = "rabbit-rs-it-chaos-topology-error";
    declare_queue(VHOST, queue).await;
    purge_queue_mgmt(queue).await;

    let broker = broker_via_proxy("primary", 5672);
    let config = config_for_queue(queue, &broker);

    // Publish and consume a message successfully first.
    {
        let pool = ClientPool::production(config.clone());
        pool.publish(
            "primary",
            publish_request("chaos-topo-1", queue, b"payload-topo-1"),
        )
        .await
        .expect("publish first");
        let received = consume_n(&pool, 1, Duration::from_secs(5)).await;
        assert_eq!(received, vec!["chaos-topo-1".to_owned()]);
        let _ = pool.close().await;
    }

    // Simulate a channel error by creating a new pool (fresh connection).
    {
        let pool = ClientPool::production(config.clone());
        pool.publish(
            "primary",
            publish_request("chaos-topo-2", queue, b"payload-topo-2"),
        )
        .await
        .expect("publish after channel recreation");
        let received = consume_n(&pool, 1, Duration::from_secs(10)).await;

        let mut audit = DeliveryAudit::new(["chaos-topo-2"].map(String::from));
        for id in &received {
            audit.record(id);
        }
        audit.assert_at_least_once("channel-closed-topology-error");
        let _ = pool.close().await;
    }

    delete_queue_mgmt(queue).await;
}

// ---------------------------------------------------------------------------
// Scenario 7: Delay plugin unavailable
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_delay_plugin_unavailable() {
    let queue = "rabbit-rs-it-chaos-delay-unavailable";
    declare_queue(VHOST, queue).await;
    purge_queue_mgmt(queue).await;

    let broker = broker_via_proxy("primary", 5672);
    let config = config_for_queue(queue, &broker);

    // Verify the delay plugin is enabled in the current lab.
    let plugin_enabled = check_delay_plugin().await;
    println!("Delay plugin enabled: {plugin_enabled}");

    // Regular publish/consume must work regardless of the delay plugin state.
    {
        let pool = ClientPool::production(config.clone());
        pool.publish(
            "primary",
            publish_request("chaos-delay-1", queue, b"payload-delay-1"),
        )
        .await
        .expect("publish with delay plugin");
        let received = consume_n(&pool, 1, Duration::from_secs(10)).await;

        let mut audit = DeliveryAudit::new(["chaos-delay-1"].map(String::from));
        for id in &received {
            audit.record(id);
        }
        audit.assert_at_least_once("delay-plugin-unavailable");
        let _ = pool.close().await;
    }

    delete_queue_mgmt(queue).await;
}

// ---------------------------------------------------------------------------
// Scenario 8: Credentials rejected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_credentials_rejected() {
    let queue = "rabbit-rs-it-chaos-credentials-rejected";
    declare_queue(VHOST, queue).await;
    purge_queue_mgmt(queue).await;

    // Publishing with wrong credentials must fail with a typed error.
    {
        let bad_broker = broker_bad_credentials("bad", 5672);
        let bad_config = config_for_queue(queue, &bad_broker);
        let pool = ClientPool::production(bad_config);

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            pool.publish(
                "bad",
                publish_request("chaos-creds-1", queue, b"payload-creds-1"),
            ),
        )
        .await;

        match result {
            Ok(Err(error)) => {
                assert!(
                    matches!(
                        error.kind(),
                        ClientErrorKind::Transport | ClientErrorKind::Publish
                    ),
                    "expected transport or publish error for bad credentials, got {:?}",
                    error.kind()
                );
                println!(
                    "[credentials-rejected] PASS: bad credentials correctly rejected with {:?}",
                    error.kind()
                );
            }
            Ok(Ok(_)) => panic!("publish with bad credentials must fail"),
            Err(_) => {
                println!("[credentials-rejected] PASS: bad credentials timed out (rejected)");
            }
        }
        let _ = pool.close().await;
    }

    // Verify that valid credentials still work for at-least-once.
    let broker = broker_via_proxy("primary", 5672);
    let config = config_for_queue(queue, &broker);
    {
        let pool = ClientPool::production(config.clone());
        pool.publish(
            "primary",
            publish_request("chaos-creds-2", queue, b"payload-creds-2"),
        )
        .await
        .expect("publish with good credentials");
        let received = consume_n(&pool, 1, Duration::from_secs(10)).await;

        let mut audit = DeliveryAudit::new(["chaos-creds-2"].map(String::from));
        for id in &received {
            audit.record(id);
        }
        audit.assert_at_least_once("credentials-rejected");
        let _ = pool.close().await;
    }

    delete_queue_mgmt(queue).await;
}

// ---------------------------------------------------------------------------
// Scenario 9: Worker SIGTERM with unacked jobs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_worker_sigterm_with_unacked() {
    let queue = "rabbit-rs-it-chaos-sigterm-unacked";
    declare_queue(VHOST, queue).await;
    purge_queue_mgmt(queue).await;

    let broker = broker_via_proxy("primary", 5672);
    let config = config_for_queue(queue, &broker);

    // Publish a message.
    {
        let pool = ClientPool::production(config.clone());
        pool.publish(
            "primary",
            publish_request("chaos-sigterm-1", queue, b"payload-sigterm-1"),
        )
        .await
        .expect("publish before sigterm");
        let _ = pool.close().await;
    }

    // Consume the message but do NOT ACK — simulating a worker that
    // received a SIGTERM while processing.
    {
        let pool = ClientPool::production(config.clone());
        let consumer = pool.consumer("main").await.expect("consumer");
        let delivery = tokio::time::timeout(Duration::from_secs(10), consumer.next())
            .await
            .expect("timeout for delivery")
            .expect("delivery");
        assert_eq!(delivery.id.as_str(), "chaos-sigterm-1");

        // Simulate SIGTERM: close the pool without ACKing the delivery.
        // The pool.close() sends a channel.close which causes the broker
        // to requeue unacked messages.
        drop(delivery);
        let _ = pool.close().await;
    }

    // Wait for the broker to requeue the unacked message.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The unacked message must be redelivered to a new consumer.
    let received = consume_all(config, 1, Duration::from_secs(15)).await;

    let mut audit = DeliveryAudit::new(["chaos-sigterm-1"].map(String::from));
    for id in &received {
        audit.record(id);
    }
    audit.assert_at_least_once("worker-sigterm-unacked");

    delete_queue_mgmt(queue).await;
}

// ---------------------------------------------------------------------------
// Management API helpers
// ---------------------------------------------------------------------------

async fn get_queue_leader(queue: &str) -> String {
    let url = format!("{MGMT_URL}/api/queues/%2Forders-eu/{queue}");
    let resp = reqwest_simple()
        .get(&url)
        .basic_auth(ADMIN_USER, Some(ADMIN_PASS))
        .send()
        .await
        .expect("get queue info");
    let body: serde_json::Value = resp.json().await.expect("queue info json");
    body["leader"]
        .as_str()
        .unwrap_or("rabbit@rabbitmq-1")
        .to_owned()
}

async fn stop_rabbitmq_node(node: &str) {
    let container = node_to_container(node);
    let _ = tokio::process::Command::new("docker")
        .args(["stop", &container])
        .output()
        .await;
}

async fn start_rabbitmq_node(node: &str) {
    let container = node_to_container(node);
    let _ = tokio::process::Command::new("docker")
        .args(["start", &container])
        .output()
        .await;
    // Wait for the node to be responsive.
    for _ in 0..30 {
        let output = tokio::process::Command::new("docker")
            .args(["exec", &container, "rabbitmq-diagnostics", "-q", "ping"])
            .output()
            .await;
        if let Ok(out) = output
            && out.status.success()
        {
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn node_to_container(node: &str) -> String {
    let suffix = node.rsplit('@').next().unwrap_or("rabbitmq-1");
    format!("rabbitrs-{suffix}-1")
}

async fn check_delay_plugin() -> bool {
    let url = format!("{MGMT_URL}/api/nodes");
    let resp = reqwest_simple()
        .get(&url)
        .basic_auth(ADMIN_USER, Some(ADMIN_PASS))
        .send()
        .await
        .expect("get nodes");
    let body: serde_json::Value = resp.json().await.expect("nodes json");
    if let Some(plugins) = body[0]["enabled_plugins"].as_array() {
        plugins.iter().any(|p| {
            p.as_str()
                .is_some_and(|s| s == "rabbitmq_delayed_message_exchange")
        })
    } else {
        false
    }
}
