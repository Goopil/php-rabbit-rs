use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rabbit_rs_core::{
    config::{BrokerConfig, Credentials, Endpoint, TlsConfig},
    transport::{PublishConfirmation, PublishRequest, Transport, mock::MockTransport},
};
use tokio::runtime::Runtime;

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

fn request(payload: Vec<u8>) -> PublishRequest {
    PublishRequest::new("", "jobs", payload)
}

const PAYLOAD_SIZES: [(usize, &str); 5] = [
    (256, "256B"),
    (1024, "1KiB"),
    (10 * 1024, "10KiB"),
    (100 * 1024, "100KiB"),
    (1024 * 1024, "1MiB"),
];

const BATCH_SIZES: [usize; 4] = [1, 16, 64, 256];

fn bench_transport_publish(c: &mut Criterion) {
    let runtime = Runtime::new().expect("runtime");

    let mut group = c.benchmark_group("transport/publish_confirm");
    group.sample_size(20);

    for &batch_size in &BATCH_SIZES {
        for &(payload_size, size_name) in &PAYLOAD_SIZES {
            let bench_id = BenchmarkId::new(format!("batch_{batch_size}"), size_name);
            let total_bytes = batch_size * payload_size;
            group.throughput(Throughput::Bytes(total_bytes as u64));

            group.bench_with_input(bench_id, &payload_size, |b, &payload_size| {
                let payloads: Vec<Vec<u8>> =
                    (0..batch_size).map(|_| payload(payload_size)).collect();

                b.iter(|| {
                    runtime.block_on(async {
                        let transport = MockTransport::default();

                        for _ in 0..batch_size {
                            transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
                        }

                        let connection = transport.connect(&broker()).await.expect("connection");
                        let publisher = connection.open_publisher().await.expect("publisher");

                        let mut receipts = Vec::with_capacity(batch_size);
                        for payload in &payloads {
                            let req = request(payload.clone());
                            let receipt = publisher.publish(req).await.expect("publish");
                            receipts.push(receipt);
                        }

                        for receipt in receipts {
                            let confirmation = receipt.wait().await.expect("confirmation");
                            assert!(matches!(confirmation, PublishConfirmation::Ack(None)));
                        }

                        publisher.close().await.expect("close channel");
                        connection.close().await.expect("close connection");
                    });
                });
            });
        }
    }

    group.finish();
}

fn bench_transport_connect(c: &mut Criterion) {
    let runtime = Runtime::new().expect("runtime");

    let mut group = c.benchmark_group("transport/connect_open_publisher");
    group.sample_size(50);

    group.bench_function("connect_open_close", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let transport = MockTransport::default();
                let connection = transport.connect(&broker()).await.expect("connection");
                let publisher = connection.open_publisher().await.expect("publisher");
                publisher.close().await.expect("close channel");
                connection.close().await.expect("close connection");
            });
        });
    });

    group.finish();
}

fn bench_transport(c: &mut Criterion) {
    bench_transport_connect(c);
    bench_transport_publish(c);
}

criterion_group!(transport_group, bench_transport);
criterion_main!(transport_group);
