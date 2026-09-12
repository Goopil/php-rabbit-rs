//! Per-delivery consumer benchmarks.
//!
//! Attempt resolution and scheduling run once for every message a worker
//! pulls, so their cost is multiplied by the delivery rate.

use rabbit_rs_core::{
    consumer::{
        AttemptsResolver, Headers, SubscriptionId, SubscriptionPolicy, WeightedFairScheduler,
    },
    metrics::{HistogramSnapshot, Metrics},
    transport::HeaderValue,
};

fn main() {
    divan::main();
}

/// Resolves Laravel-compatible attempts from the broker counters a quorum
/// queue stamps on each delivery.
#[divan::bench(args = ["quorum", "delivery_count", "application", "classic"])]
fn resolve_attempts(bencher: divan::Bencher, source: &str) {
    let resolver = AttemptsResolver::default();
    let headers = headers_for(source);
    let redelivered = source == "classic";

    bencher.bench(|| {
        divan::black_box(
            resolver
                .resolve(divan::black_box(&headers), divan::black_box(redelivered))
                .expect("attempts below the cap"),
        )
    });
}

/// Registers a worker's subscriptions and drains a full weighted round, the
/// decision the consumer set takes between two deliveries.
#[divan::bench(args = [4_usize, 32])]
fn weighted_fair_round(bencher: divan::Bencher, subscriptions: usize) {
    let ids: Vec<SubscriptionId> = (0..subscriptions)
        .map(|index| SubscriptionId::new(format!("subscription-{index}")))
        .collect();

    bencher
        .with_inputs(|| {
            let mut scheduler = WeightedFairScheduler::default();
            for (index, id) in ids.iter().enumerate() {
                let weight = u16::try_from(index % 8 + 1).unwrap_or(1);
                scheduler.register(id.clone(), SubscriptionPolicy::new(weight));
                scheduler.mark_ready(id);
            }
            scheduler
        })
        .bench_values(|mut scheduler: WeightedFairScheduler| {
            for _ in 0..subscriptions {
                divan::black_box(scheduler.pick());
            }
        });
}

/// Takes the lock-free metrics snapshot the extension exposes to PHP.
#[divan::bench]
fn metrics_snapshot(bencher: divan::Bencher) {
    let metrics = Metrics::default();

    bencher.bench(|| divan::black_box(metrics.snapshot()));
}

/// Estimates a latency percentile from the fixed histogram buckets.
#[divan::bench(args = [50.0_f64, 99.0])]
fn latency_percentile(bencher: divan::Bencher, percentile: f64) {
    let histogram = histogram();

    bencher.bench(|| divan::black_box(histogram.percentile_ns(divan::black_box(percentile))));
}

fn headers_for(source: &str) -> Headers {
    let mut headers = Headers::new();
    headers.insert(
        "x-rabbit-rs-queue".to_owned(),
        HeaderValue::Binary(bytes::Bytes::from_static(b"orders")),
    );
    match source {
        "quorum" => {
            headers.insert("x-acquired-count".to_owned(), HeaderValue::Integer(3));
        }
        "delivery_count" => {
            headers.insert("x-delivery-count".to_owned(), HeaderValue::Integer(2));
        }
        "application" => {
            headers.insert(
                "x-rabbit-rs-attempts".to_owned(),
                HeaderValue::Binary(bytes::Bytes::from_static(b"4")),
            );
        }
        "classic" => {}
        other => panic!("unknown attempts source '{other}'"),
    }
    headers
}

fn histogram() -> HistogramSnapshot {
    let bounds_ns: [u64; 10] = [
        100_000,
        500_000,
        1_000_000,
        5_000_000,
        10_000_000,
        50_000_000,
        100_000_000,
        500_000_000,
        1_000_000_000,
        5_000_000_000,
    ];
    let buckets: [u64; 11] = [12, 48, 130, 640, 210, 90, 33, 11, 4, 1, 0];
    let samples = buckets.iter().sum();

    HistogramSnapshot {
        bounds_ns,
        buckets,
        samples,
        sum_ns: 1_234_567_890,
    }
}
