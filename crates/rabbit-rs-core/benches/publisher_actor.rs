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

fn request(message_id: &str) -> PublishRequest {
    PublishRequest::new(
        Destination::new("jobs", "high"),
        Bytes::from(vec![0x42; 256]),
        MessageProperties::new(message_id),
        Instant::now() + std::time::Duration::from_secs(30),
    )
}

const BATCH_SIZES: [usize; 4] = [1, 16, 64, 256];

fn bench_publisher_actor(c: &mut Criterion) {
    let runtime = Runtime::new().expect("runtime");

    let mut group = c.benchmark_group("publisher_actor/try_publish_wait");
    group.sample_size(20);

    for &batch_size in &BATCH_SIZES {
        let bench_id = BenchmarkId::new("confirms", batch_size);
        group.throughput(Throughput::Elements(batch_size as u64));

        group.bench_with_input(bench_id, &batch_size, |b, &batch_size| {
            b.iter_batched(
                || {
                    runtime.block_on(async {
                        let transport = MockTransport::default();
                        for _ in 0..batch_size {
                            transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
                        }

                        let channel = transport
                            .connect(&broker())
                            .await
                            .expect("connection")
                            .open_publisher()
                            .await
                            .expect("publisher channel");

                        let config = PublisherConfig::with_flags(
                            1024,
                            std::time::Duration::from_secs(30),
                            true,
                            true,
                        );
                        PublisherActor::spawn(Arc::from(channel), config)
                    })
                },
                |publisher| {
                    runtime.block_on(async {
                        let mut waiters = Vec::with_capacity(batch_size);
                        for index in 0..batch_size {
                            let req = request(&format!("msg-{index}"));
                            let waiter = publisher.try_publish(req).expect("publish");
                            waiters.push(waiter);
                        }

                        for waiter in waiters {
                            let outcome = waiter.wait().await.expect("outcome");
                            assert!(matches!(outcome, PublishOutcome::Confirmed { .. }));
                        }

                        publisher.close().await.expect("close");
                    });
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }

    group.finish();
}

criterion_group!(publisher_actor_group, bench_publisher_actor);
criterion_main!(publisher_actor_group);
