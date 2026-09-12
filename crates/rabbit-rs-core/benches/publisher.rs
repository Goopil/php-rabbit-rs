//! Publisher pump benchmarks.
//!
//! Measures the pipelined publish hot path every PHP publish traverses
//! after conversion: mailbox hand-off, in-flight accounting, deadline
//! bookkeeping, wire write and confirmation resolution. The mock transport
//! answers immediately, so the loop is CPU-bound and deterministic.

use bytes::Bytes;
use rabbit_rs_core::config::{BrokerConfig, Credentials, Endpoint, SafetyMode, TlsConfig};
use rabbit_rs_core::metrics::Metrics;
use rabbit_rs_core::publisher::{
    Destination, MessageProperties, PublishRequest, PublisherActor, PublisherConfig,
};
use rabbit_rs_core::transport::mock::MockTransport;
use rabbit_rs_core::transport::{PublishConfirmation, Transport};
use std::time::Duration;

/// Publications flushed per measured iteration.
const BATCH: usize = 128;

/// Confirm-timeout budget: never reached, every confirmation is immediate.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

/// Payload size of a representative Laravel job body.
const PAYLOAD: Bytes = Bytes::from_static(b"x-door-bench-payload-0123456789abcdef");

struct Pump {
    transport: std::sync::Arc<MockTransport>,
    publisher: rabbit_rs_core::publisher::PublisherHandle,
    runtime: tokio::runtime::Runtime,
}

impl Pump {
    fn spawn() -> Self {
        let transport = std::sync::Arc::new(MockTransport::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("bench runtime");
        let publisher = runtime.block_on(async {
            let channel: std::sync::Arc<dyn rabbit_rs_core::transport::PublisherChannel> =
                std::sync::Arc::from(
                    transport
                        .connect(&broker())
                        .await
                        .expect("mock connect")
                        .open_publisher()
                        .await
                        .expect("publisher channel"),
                );
            PublisherActor::spawn_with_delay_strategy_and_metrics(
                channel,
                PublisherConfig::with_safety(4_096, CONFIRM_TIMEOUT, SafetyMode::Safe),
                Metrics::default(),
                None,
            )
        });
        Self {
            transport,
            publisher,
            runtime,
        }
    }

    /// One full pump cycle: `BATCH` pipelined publishes plus their
    /// confirmations, all awaited.
    fn run_batch(&self) {
        for _ in 0..BATCH {
            self.transport
                .push_confirmation(Ok(PublishConfirmation::Ack(None)));
        }
        self.runtime.block_on(async {
            let mut waiters = Vec::with_capacity(BATCH);
            for index in 0..BATCH {
                let waiter = self
                    .publisher
                    .try_publish(request(u64::try_from(index).unwrap_or(0)))
                    .expect("publication accepted");
                waiters.push(waiter);
            }
            for waiter in waiters {
                let outcome = waiter.wait().await.expect("confirmation resolved");
                divan::black_box(outcome);
            }
        });
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

fn request(index: u64) -> PublishRequest {
    let now = tokio::time::Instant::now();
    PublishRequest::new(
        Destination::new("jobs", "orders"),
        PAYLOAD,
        MessageProperties::new(format!("bench-{index}")),
        now + CONFIRM_TIMEOUT,
    )
}

fn main() {
    divan::main();
}

/// Pipelines a full 128-publication batch and awaits every confirmation.
#[divan::bench]
fn pump_batch_128(bencher: divan::Bencher) {
    let pump = Pump::spawn();

    bencher.bench(|| pump.run_batch());
}
