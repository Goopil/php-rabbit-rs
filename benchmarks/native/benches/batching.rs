use std::sync::Arc;

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rabbit_rs_core::{
    config::{BrokerConfig, Credentials, Endpoint, TlsConfig},
    publisher::{
        Destination, MessageProperties, PublishOutcome, PublishRequest, PublisherActor,
        PublisherConfig,
    },
    transport::{PublishConfirmation, Transport, mock::MockTransport},
};
use tokio::runtime::Runtime;
use tokio::time::Instant;

fn broker() -> BrokerConfig {
    BrokerConfig {
        name: "primary".to_owned(),
        hosts: vec![Endpoint::new("localhost", 5672)],
        vhost: "/".to_owned(),
        credentials: Credentials::new("guest", "guest"),
        tls: TlsConfig::disabled(),
        heartbeat: std::time::Duration::from_secs(30),
    }
}

fn payload(size: usize) -> Vec<u8> {
    vec![0x42; size]
}

fn request(message_id: &str, payload: Vec<u8>) -> PublishRequest {
    PublishRequest::new(
        Destination::new("jobs", "high"),
        Bytes::from(payload),
        MessageProperties::new(message_id),
        Instant::now() + std::time::Duration::from_secs(30),
    )
}

const BATCH_SIZES: [usize; 4] = [1, 16, 64, 256];
const PAYLOAD_SIZES: [(usize, &str); 5] = [
    (256, "256B"),
    (1024, "1KiB"),
    (10 * 1024, "10KiB"),
    (100 * 1024, "100KiB"),
    (1024 * 1024, "1MiB"),
];

fn bench_batching(c: &mut Criterion) {
    let runtime = Runtime::new().expect("runtime");

    let mut group = c.benchmark_group("batching");
    group.sample_size(20);

    for confirms in [true, false] {
        let label = if confirms { "confirms" } else { "no-confirms" };
        for &batch_size in &BATCH_SIZES {
            for &(payload_size, size_name) in &PAYLOAD_SIZES {
                let bench_id = BenchmarkId::new(label, format!("{batch_size}/{size_name}"));
                let total_bytes = batch_size * payload_size;
                group.throughput(Throughput::Bytes(total_bytes as u64));

                group.bench_with_input(bench_id, &payload_size, |b, &payload_size| {
                    let payloads: Vec<Vec<u8>> =
                        (0..batch_size).map(|_| payload(payload_size)).collect();

                    b.iter(|| {
                        runtime.block_on(async {
                            let transport = MockTransport::default();

                            for _ in 0..batch_size {
                                if confirms {
                                    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
                                } else {
                                    transport
                                        .push_confirmation(Ok(PublishConfirmation::NotRequested));
                                }
                            }

                            let channel = transport
                                .connect(&broker())
                                .await
                                .expect("connection")
                                .open_publisher()
                                .await
                                .expect("publisher channel");

                            let config = PublisherConfig::with_flags(
                                batch_size,
                                2 * 1024 * 1024,
                                std::time::Duration::from_millis(1),
                                1024,
                                std::time::Duration::from_secs(30),
                                confirms,
                                true,
                            );
                            let publisher = PublisherActor::spawn(Arc::from(channel), config);

                            let mut waiters = Vec::with_capacity(batch_size);
                            for (index, payload) in payloads.iter().enumerate() {
                                let req = request(&format!("msg-{index}"), payload.clone());
                                let waiter = publisher.try_publish(req).expect("publish");
                                waiters.push(waiter);
                            }

                            for waiter in waiters {
                                let outcome = waiter.wait().await.expect("outcome");
                                assert!(matches!(outcome, PublishOutcome::Confirmed { .. }));
                            }

                            publisher.close().await.expect("close");
                        });
                    });
                });
            }
        }
    }

    group.finish();
}

criterion_group!(batching_group, bench_batching);
criterion_main!(batching_group);
