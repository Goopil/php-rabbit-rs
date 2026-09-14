//! Rust-only consume-throughput instrument against the real lab broker.
//!
//! Measures the Rust pipeline alone (lapin transport, delivery hand-off
//! chain, consumer set) without the PHP extension, so the per-message cost
//! can be attributed: the delta against the current-thread mock benches is
//! the cross-thread wake chain plus lapin's share. Findings feed #282.
//!
//! Usage (lab up first: `./scripts/lab-up.sh`):
//!
//! ```text
//! cargo run -p rabbit-rs-core --example lab_consume -- --messages 100000
//! cargo run -p rabbit-rs-core --example lab_consume -- --messages 100000 --no-ack
//! cargo run -p rabbit-rs-core --example lab_consume -- --messages 100000 --nack
//! ```
//!
//! Not part of CI or the tracked benchmark suite: a local measurement instrument.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rabbit_rs_core::client::ClientPool;
use rabbit_rs_core::config::{
    BrokerConfig, Config, Credentials, Endpoint, PrefetchConfig, SchedulerConfig,
    SubscriptionConfig, TlsConfig, TopologyMode, WorkerProfile,
};
use rabbit_rs_core::publisher::{Destination, MessageProperties, PublishRequest};
use rabbit_rs_core::topology::{
    QueueDefinition, TopologyDefinition, TopologyPlan, TopologyReconciler,
};
use rabbit_rs_core::transport::{QueueKind, Transport, lapin::LapinTransport};

const BROKER_NAME: &str = "primary";
const QUEUE: &str = "bench.goopil.lab-consume";
const PAYLOAD: Bytes = Bytes::from_static(b"lab-consume-instrument-payload-0123456789abcdef");
const PUBLISH_CHUNK: usize = 1_000;
const PREFETCH: u16 = 64;

struct LabEndpoint {
    host: String,
    port: u16,
    user: String,
    password: String,
    vhost: String,
}

fn main() {
    let mut messages: usize = 100_000;
    let mut seconds: f64 = 30.0;
    let mut no_ack = false;
    let mut nack = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--messages" => {
                messages = args
                    .next()
                    .expect("--messages needs a value")
                    .parse()
                    .expect("--messages must be a number");
            }
            "--seconds" => {
                seconds = args
                    .next()
                    .expect("--seconds needs a value")
                    .parse()
                    .expect("--seconds must be a number");
            }
            "--no-ack" => no_ack = true,
            "--nack" => nack = true,
            other => panic!(
                "unknown argument '{other}' (expected --messages, --seconds, --no-ack, --nack)"
            ),
        }
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(run(
            messages,
            Duration::from_secs_f64(seconds),
            no_ack,
            nack,
        ));
}

async fn run(messages: usize, seconds: Duration, no_ack: bool, nack: bool) {
    let lab = lab_endpoint();
    let pool = ClientPool::production(config(&lab, no_ack));

    declare_queue(&lab).await;
    pool.purge_queue(BROKER_NAME, QUEUE)
        .await
        .expect("purge queue");

    let publish_start = Instant::now();
    for chunk_start in (0..messages).step_by(PUBLISH_CHUNK) {
        let requests: Vec<(Arc<str>, PublishRequest)> = (chunk_start
            ..(chunk_start + PUBLISH_CHUNK).min(messages))
            .map(|index| {
                (
                    BROKER_NAME.into(),
                    PublishRequest::new(
                        Destination::new("", QUEUE),
                        PAYLOAD.clone(),
                        MessageProperties::new(format!("lab-{index}")),
                        tokio::time::Instant::now() + Duration::from_mins(1),
                    ),
                )
            })
            .collect();
        pool.publish_batch(requests).await.expect("publish chunk");
    }
    let publish_elapsed = publish_start.elapsed();

    let consumer = pool.consumer("main").await.expect("consumer");

    let mut latencies_us = Vec::with_capacity(messages);
    let consume_start = Instant::now();
    while latencies_us.len() < messages {
        if consume_start.elapsed() >= seconds {
            break;
        }
        let t0 = Instant::now();
        let delivery = consumer.next().await.expect("delivery");
        latencies_us.push(u64::try_from(t0.elapsed().as_micros()).unwrap_or(u64::MAX));
        if nack {
            // requeue=false: the queue drains, so the run stays bounded —
            // measures the negative-settlement wire path per message.
            delivery.try_reject(false).expect("nack accepted");
        } else if !no_ack {
            delivery.try_ack().expect("ack accepted");
        }
    }
    let consume_elapsed = consume_start.elapsed();
    let delivered = latencies_us.len();
    assert!(delivered > 0, "no deliveries consumed");

    let (p50_us, p99_us) = percentiles(&mut latencies_us);
    let throughput = f64::from(u32::try_from(delivered).expect("delivery count fits u32"))
        / consume_elapsed.as_secs_f64();
    let ack_mode = if no_ack {
        "no_ack"
    } else if nack {
        "nack"
    } else {
        "manual"
    };
    println!(
        "{{\"messages\": {delivered}, \"seconds\": {:.3}, \"msg_per_s\": {:.0}, \"p50_us\": {p50_us}, \"p99_us\": {p99_us}, \"ack_mode\": \"{ack_mode}\"}}",
        consume_elapsed.as_secs_f64(),
        throughput,
    );
    eprintln!(
        "publish phase: {messages} messages in {:.2}s (unmeasured)",
        publish_elapsed.as_secs_f64()
    );

    pool.close().await.expect("close pool");
}

/// Lab defaults mirror the PHP driver-bench environment (vhost `/`, user
/// `rabbit_rs`, `bench.` configure grant, classic durable queue, prefetch 64).
fn lab_endpoint() -> LabEndpoint {
    let mut lab = LabEndpoint {
        host: "127.0.0.1".to_owned(),
        port: 5672,
        user: "rabbit_rs".to_owned(),
        password: "rabbit_rs_lab".to_owned(),
        vhost: "/".to_owned(),
    };
    if let Ok(uri) = std::env::var("LAB_AMQP_URI") {
        lab = parse_uri(&uri);
    }
    lab
}

/// Parses `amqp://user:password@host:port/vhost`.
fn parse_uri(uri: &str) -> LabEndpoint {
    let rest = uri
        .strip_prefix("amqp://")
        .unwrap_or_else(|| panic!("LAB_AMQP_URI must start with amqp://, got '{uri}'"));
    let (authority, vhost) = rest
        .split_once('/')
        .unwrap_or_else(|| panic!("LAB_AMQP_URI needs a vhost: '{uri}'"));
    let (userinfo, hostport) = authority
        .split_once('@')
        .unwrap_or_else(|| panic!("LAB_AMQP_URI needs user:password@host:port: '{uri}'"));
    let (user, password) = userinfo
        .split_once(':')
        .unwrap_or_else(|| panic!("LAB_AMQP_URI needs user:password: '{uri}'"));
    let (host, port) = hostport
        .split_once(':')
        .unwrap_or_else(|| panic!("LAB_AMQP_URI needs host:port: '{uri}'"));
    LabEndpoint {
        host: host.to_owned(),
        port: port
            .parse()
            .unwrap_or_else(|_| panic!("invalid port in '{uri}'")),
        user: user.to_owned(),
        password: password.to_owned(),
        vhost: format!("/{vhost}"),
    }
}

fn config(lab: &LabEndpoint, no_ack: bool) -> Arc<rabbit_rs_core::config::ValidatedConfig> {
    let document = Config {
        brokers: vec![BrokerConfig {
            name: BROKER_NAME.into(),
            hosts: vec![Endpoint::new(&lab.host, lab.port)],
            vhost: lab.vhost.clone(),
            credentials: Credentials::new(&lab.user, &lab.password),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        }],
        workers: vec![WorkerProfile {
            name: "main".to_owned(),
            subscriptions: vec![SubscriptionConfig {
                name: "lab-consume".to_owned(),
                broker: BROKER_NAME.into(),
                queue: QUEUE.to_owned(),
                weight: 1,
                prefetch: PrefetchConfig::Fixed(PREFETCH),
                max_buffered_bytes: 64 * 1024 * 1024,
                early_ack: no_ack,
                no_ack,
            }],
            scheduler: SchedulerConfig::weighted_fair(),
        }],
        topology_mode: TopologyMode::External,
        routes: std::collections::BTreeMap::new(),
        delay: rabbit_rs_core::config::DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
        publisher: rabbit_rs_core::config::PublisherConfigSection::default(),
        consumer: rabbit_rs_core::config::ConsumerConfigSection::default(),
        queue_type: QueueKind::Classic,
        queue_durable: true,
    }
    .validate()
    .expect("lab config validates");
    Arc::new(document)
}

/// Declares the classic durable bench queue through the public topology API.
async fn declare_queue(lab: &LabEndpoint) {
    let broker_config = BrokerConfig {
        name: BROKER_NAME.into(),
        hosts: vec![Endpoint::new(&lab.host, lab.port)],
        vhost: lab.vhost.clone(),
        credentials: Credentials::new(&lab.user, &lab.password),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(30),
    };
    let connection = LapinTransport
        .connect(&broker_config)
        .await
        .expect("connect for topology");
    let channel = connection
        .open_publisher()
        .await
        .expect("publisher channel");

    let plan = TopologyPlan::compile(
        TopologyMode::Declare,
        TopologyDefinition::new(vec![], vec![QueueDefinition::new(QUEUE).classic()], vec![]),
    )
    .expect("compile topology plan");

    TopologyReconciler::new()
        .reconcile(channel.as_ref(), &plan, 1)
        .await
        .expect("declare queue");

    channel.close().await.expect("close topology channel");
    connection.close().await.expect("close topology connection");
}

fn percentiles(latencies_us: &mut [u64]) -> (u64, u64) {
    latencies_us.sort_unstable();
    let p = |percentile: usize| -> u64 {
        let index = (latencies_us.len() * percentile / 100).min(latencies_us.len() - 1);
        latencies_us[index]
    };
    (p(50), p(99))
}
