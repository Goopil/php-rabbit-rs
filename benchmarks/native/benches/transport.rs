use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rabbit_rs_core::{
    config::{BrokerConfig, Credentials, Endpoint, TlsConfig},
    transport::{PublishConfirmation, PublishRequest, Transport, mock::MockTransport},
};
use tokio::runtime::Runtime;

const BROKER_URI_ENV: &str = "RABBIT_BENCH_BROKER_URI";

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

fn lab_transport() -> Option<Box<dyn Transport>> {
    let uri = std::env::var(BROKER_URI_ENV).ok()?;
    eprintln!("lab broker: {uri}");
    Some(Box::new(rabbit_rs_core::transport::lapin::LapinTransport))
}

fn broker_from_uri(uri: &str) -> BrokerConfig {
    let stripped = uri
        .strip_prefix("amqp://")
        .or_else(|| uri.strip_prefix("amqps://"))
        .unwrap_or(uri);
    let (authority, vhost) = stripped
        .split_once('/')
        .map_or((stripped, "/"), |(auth, path)| (auth, path));
    let (host_port, creds) = authority
        .rsplit_once('@')
        .map_or((authority, None), |(hp, cr)| (hp, Some(cr)));
    let (host, port) = host_port
        .rsplit_once(':')
        .map_or((host_port, 5672), |(h, p)| (h, p.parse().unwrap_or(5672)));
    let credentials = creds.map_or(Credentials::new("guest", "guest"), |c| {
        let (user, pass) = c.split_once(':').map_or((c, "guest"), |(u, p)| (u, p));
        Credentials::new(user, pass)
    });
    let vhost = if vhost.is_empty() { "/" } else { vhost };
    BrokerConfig {
        name: "primary".to_owned(),
        hosts: vec![Endpoint::new(host, port)],
        vhost: vhost.to_owned(),
        credentials,
        tls: TlsConfig::disabled(),
        heartbeat: std::time::Duration::from_secs(30),
    }
}

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
                let lab = lab_transport();
                let broker_cfg = lab.as_ref().map_or_else(broker, |_| {
                    broker_from_uri(&std::env::var(BROKER_URI_ENV).expect("checked"))
                });

                b.iter_batched(
                    || {
                        runtime.block_on(async {
                            let connection = if let Some(transport) = &lab {
                                transport.connect(&broker_cfg).await.expect("connection")
                            } else {
                                let transport = MockTransport::default();
                                for _ in 0..batch_size {
                                    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
                                }
                                transport.connect(&broker()).await.expect("connection")
                            };
                            let publisher = connection.open_publisher().await.expect("publisher");
                            (connection, publisher)
                        })
                    },
                    |(connection, publisher)| {
                        runtime.block_on(async {
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
                    },
                    criterion::BatchSize::LargeInput,
                );
            });
        }
    }

    group.finish();
}

fn bench_transport_connect(c: &mut Criterion) {
    let runtime = Runtime::new().expect("runtime");

    let mut group = c.benchmark_group("transport/connect_open_publisher");
    group.sample_size(50);

    let lab = lab_transport();
    let broker_cfg = lab.as_ref().map_or_else(broker, |_| {
        broker_from_uri(&std::env::var(BROKER_URI_ENV).expect("checked"))
    });

    group.bench_function("connect_open_close", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let connection = if let Some(transport) = &lab {
                    transport.connect(&broker_cfg).await.expect("connection")
                } else {
                    let transport = MockTransport::default();
                    transport.connect(&broker()).await.expect("connection")
                };
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
