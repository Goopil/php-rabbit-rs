//! Consumer delivery-loop benchmarks.
//!
//! Measures the per-message consume path over the mock transport: delivery
//! stream hand-off, subscription scheduling, payload/headers move into the
//! `Delivery` token and the synchronous acknowledgement round-trip. This is
//! the loop a PHP worker repeats for every job.

use bytes::Bytes;
use rabbit_rs_core::config::{BrokerConfig, Credentials, Endpoint, TlsConfig, ValidatedConfig};
use rabbit_rs_core::consumer::{ConsumerSet, Subscription, SubscriptionPolicy};
use rabbit_rs_core::metrics::Metrics;
use rabbit_rs_core::pool::ConnectionKey;
use rabbit_rs_core::transport::mock::MockTransport;
use rabbit_rs_core::transport::{Delivery as TransportDelivery, Headers, Transport};
use std::sync::Arc;
use std::time::Duration;

/// Payload size of a representative Laravel job body.
const PAYLOAD: Bytes = Bytes::from_static(b"x-door-bench-payload-0123456789abcdef");

struct DeliveryLoop {
    transport: Arc<MockTransport>,
    consumer: rabbit_rs_core::consumer::ConsumerSetHandle,
    runtime: tokio::runtime::Runtime,
    next_tag: std::sync::atomic::AtomicU64,
}

impl DeliveryLoop {
    fn spawn() -> Self {
        let transport = Arc::new(MockTransport::default());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("bench runtime");
        let config = validated_config();
        let consumer = runtime.block_on(async {
            // The delivery stream must stay open so `push_delivery` feeds the
            // consumer task after spawn.
            transport.keep_delivery_stream_open();
            let channel: Arc<dyn rabbit_rs_core::transport::ConsumerChannel> = Arc::from(
                transport
                    .connect(&broker())
                    .await
                    .expect("mock connect")
                    .open_consumer()
                    .await
                    .expect("consumer channel"),
            );
            let subscription = Subscription::new(
                "bench",
                ConnectionKey::from_config(&config),
                "orders",
                channel,
            )
            .prefetch(256)
            .channel_id(1)
            .policy(SubscriptionPolicy::new(1));
            ConsumerSet::spawn_with_metrics(vec![subscription], Metrics::default())
                .await
                .expect("consumer set spawned")
        });
        Self {
            transport,
            consumer,
            runtime,
            next_tag: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// One full consume cycle: one delivered job fetched and acknowledged.
    fn run_one(&self) {
        let tag = self
            .next_tag
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.transport.push_delivery(Ok(delivery(tag)));

        let delivery = self
            .runtime
            .block_on(self.consumer.next())
            .expect("consumer healthy");
        divan::black_box(&delivery);
        delivery.try_ack().expect("ack accepted");
    }
}

fn broker() -> BrokerConfig {
    BrokerConfig {
        name: "bench".to_owned(),
        hosts: vec![Endpoint::new("rabbit.internal", 5672)],
        vhost: "/".to_owned(),
        credentials: Credentials::new("guest", "guest"),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(30),
    }
}

fn validated_config() -> Arc<ValidatedConfig> {
    let document = serde_json::json!({
        "brokers": [{
            "name": "bench",
            "hosts": [{"host": "rabbit.internal", "port": 5672}],
            "vhost": "/",
            "credentials": {"username": "guest", "password": "guest"},
            "tls": {"enabled": false},
            "heartbeat": 30,
        }],
        "workers": [],
        "topology_mode": "external",
        "queue_type": "quorum",
        "queue_durable": true,
    });
    Arc::new(
        serde_json::from_value::<rabbit_rs_core::config::Config>(document)
            .expect("bench config deserializes")
            .validate()
            .expect("bench config validates"),
    )
}

fn delivery(tag: u64) -> TransportDelivery {
    let mut headers = Headers::new();
    headers.insert(
        "x-delivery-count".to_owned(),
        rabbit_rs_core::transport::HeaderValue::Integer(1),
    );
    TransportDelivery {
        delivery_tag: tag,
        exchange: "jobs".to_owned(),
        routing_key: "orders".to_owned(),
        redelivered: false,
        message_id: Some(format!("bench-{tag}")),
        correlation_id: None,
        headers: Arc::new(headers),
        payload: PAYLOAD,
    }
}

fn main() {
    divan::main();
}

/// Fetches one delivered job and acknowledges it — the per-job consumer cost.
#[divan::bench]
fn delivery_ack_round_trip(bencher: divan::Bencher) {
    let consumer = DeliveryLoop::spawn();

    bencher.bench(|| consumer.run_one());
}

/// Fetches and acknowledges a burst of jobs, exercising the pipelined
/// buffer between the delivery stream and the consumer.
#[divan::bench(args = [16_usize, 64])]
fn delivery_ack_burst(bencher: divan::Bencher, burst: usize) {
    let consumer = DeliveryLoop::spawn();

    bencher.bench(|| {
        for _ in 0..burst {
            consumer.run_one();
        }
    });
}
