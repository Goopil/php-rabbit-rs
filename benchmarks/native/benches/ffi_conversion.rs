use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rabbit_rs_core::{
    config::{
        BrokerConfig, Config, Credentials, DelayConfig, Endpoint, PublisherConfigSection,
        TlsConfig, TopologyMode,
    },
    publisher::{Destination, MessageProperties, PublishRequest},
    transport::{HeaderFloat, HeaderValue, PublishHeaders},
};
use tokio::time::Instant;

const PAYLOAD_SIZES: [(usize, &str); 5] = [
    (256, "256B"),
    (1024, "1KiB"),
    (10 * 1024, "10KiB"),
    (100 * 1024, "100KiB"),
    (1024 * 1024, "1MiB"),
];

const HEADER_COUNTS: [usize; 4] = [0, 8, 32, 128];

fn payload(size: usize) -> Vec<u8> {
    vec![0x42; size]
}

fn make_headers(count: usize) -> PublishHeaders {
    let mut headers = PublishHeaders::new();
    for index in 0..count {
        let key = format!("x-header-{index}");
        let value = match index % 4 {
            0 => HeaderValue::Boolean(true),
            1 => HeaderValue::Integer(i64::try_from(index).unwrap_or(i64::MAX)),
            2 => HeaderValue::Double(
                HeaderFloat::new(f64::from(u16::try_from(index).unwrap_or(u16::MAX)))
                    .unwrap_or_else(|| HeaderFloat::new(1.0).expect("finite fallback")),
            ),
            _ => HeaderValue::Binary(Bytes::copy_from_slice(format!("value-{index}").as_bytes())),
        };
        headers.insert(key, value);
    }
    headers
}

fn broker_config() -> BrokerConfig {
    BrokerConfig {
        name: "primary".to_owned(),
        hosts: vec![Endpoint::new("localhost", 5672)],
        vhost: "/".to_owned(),
        credentials: Credentials::new("guest", "guest"),
        tls: TlsConfig::disabled(),
        heartbeat: std::time::Duration::from_secs(30),
    }
}

fn config_for_validation() -> Config {
    Config {
        brokers: vec![broker_config()],
        workers: vec![],
        topology_mode: TopologyMode::External,
        delay: DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
        publisher: PublisherConfigSection::default(),
    }
}

fn bench_config_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("ffi_conversion/config_validation");

    group.bench_function("validate", |b| {
        let config = config_for_validation();
        b.iter(|| {
            config.clone().validate().expect("valid config");
        });
    });

    group.finish();
}

fn bench_message_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("ffi_conversion/message_construction");

    for &(payload_size, size_name) in &PAYLOAD_SIZES {
        for &header_count in &HEADER_COUNTS {
            let bench_id = BenchmarkId::new(size_name, format!("h{header_count}"));
            let total_bytes = payload_size;
            group.throughput(Throughput::Bytes(total_bytes as u64));

            group.bench_with_input(bench_id, &payload_size, |b, &payload_size| {
                let headers = make_headers(header_count);
                let payload_bytes = payload(payload_size);

                b.iter(|| {
                    let mut properties = MessageProperties::new("msg-bench");
                    properties.content_type = Some("application/octet-stream".to_owned());
                    properties.correlation_id = Some("corr-bench".to_owned());
                    properties.headers = headers.clone();

                    let _request = PublishRequest::new(
                        Destination::new("jobs", "high"),
                        Bytes::copy_from_slice(&payload_bytes),
                        properties.clone(),
                        Instant::now() + std::time::Duration::from_secs(30),
                    );
                });
            });
        }
    }

    group.finish();
}

fn bench_header_conversion(c: &mut Criterion) {
    let mut group = c.benchmark_group("ffi_conversion/header_conversion");

    for &header_count in &HEADER_COUNTS {
        let bench_id = BenchmarkId::new("headers", header_count);
        group.bench_with_input(bench_id, &header_count, |b, &header_count| {
            let template: Vec<(String, HeaderValue)> = (0..header_count)
                .map(|index| {
                    let key = format!("x-header-{index}");
                    let value = match index % 4 {
                        0 => HeaderValue::Boolean(true),
                        1 => HeaderValue::Integer(i64::try_from(index).unwrap_or(i64::MAX)),
                        2 => HeaderValue::Double(
                            HeaderFloat::new(f64::from(u16::try_from(index).unwrap_or(u16::MAX)))
                                .unwrap_or_else(|| HeaderFloat::new(1.0).expect("finite")),
                        ),
                        _ => HeaderValue::Binary(Bytes::copy_from_slice(
                            format!("value-{index}").as_bytes(),
                        )),
                    };
                    (key, value)
                })
                .collect();

            b.iter(|| {
                let mut headers = PublishHeaders::new();
                for (key, value) in &template {
                    headers.insert(key.clone(), value.clone());
                }
                let _ = headers;
            });
        });
    }

    group.finish();
}

fn bench_header_path_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("ffi_conversion/header_path_allocation");

    for &header_count in &HEADER_COUNTS {
        let bench_id = BenchmarkId::new("headers", header_count);
        group.throughput(Throughput::Elements(header_count as u64));

        group.bench_with_input(bench_id, &header_count, |b, &header_count| {
            let keys: Vec<String> = (0..header_count).map(|i| format!("h{i}")).collect();

            b.iter(|| {
                let path = "messages[0]";
                let mut total_len = 0usize;
                for key in &keys {
                    let value_path = format!("{path}.headers.{key}");
                    total_len += value_path.len();
                }
                let _ = total_len;
            });
        });
    }

    group.finish();
}

fn bench_ffi_conversion(c: &mut Criterion) {
    bench_config_validation(c);
    bench_message_construction(c);
    bench_header_conversion(c);
    bench_header_path_allocation(c);
}

criterion_group!(ffi_conversion_group, bench_ffi_conversion);
criterion_main!(ffi_conversion_group);
