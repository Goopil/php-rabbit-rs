use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rabbit_rs_core::transport::{
    HeaderValue, PublishHeaders, PublishProperties, PublishRequest, lapin::publish_properties_bench,
};

const HEADER_COUNTS: [usize; 4] = [0, 8, 32, 128];

fn make_headers(count: usize) -> PublishHeaders {
    let mut headers = PublishHeaders::new();
    for index in 0..count {
        let key = format!("h{index}");
        headers.insert(
            key,
            HeaderValue::Integer(i64::try_from(index).unwrap_or(i64::MAX)),
        );
    }
    headers
}

fn make_request(header_count: usize) -> PublishRequest {
    let properties = PublishProperties {
        persistent: true,
        headers: make_headers(header_count),
        ..PublishProperties::default()
    };

    PublishRequest {
        exchange: "jobs".into(),
        routing_key: "high".into(),
        payload: Bytes::from(vec![0x42; 256]),
        mandatory: true,
        properties,
    }
}

fn bench_lapin_publish_properties(c: &mut Criterion) {
    let mut group = c.benchmark_group("lapin/publish_properties");
    group.sample_size(20);

    for &header_count in &HEADER_COUNTS {
        let bench_id = BenchmarkId::new("headers", header_count);
        group.throughput(Throughput::Elements(header_count as u64));

        group.bench_with_input(bench_id, &header_count, |b, &header_count| {
            let request = make_request(header_count);

            b.iter(|| {
                let _properties = publish_properties_bench(&request);
            });
        });
    }

    group.finish();
}

criterion_group!(lapin_properties_group, bench_lapin_publish_properties);
criterion_main!(lapin_properties_group);
