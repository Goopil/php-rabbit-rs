use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::Serialize;

const LATENCY_BOUNDS_NS: [u64; 10] = [
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

/// Lock-free metrics registry that can be shared by all runtime actors.
#[derive(Clone, Default)]
pub struct Metrics {
    inner: Arc<MetricsInner>,
}

impl Metrics {
    /// Returns a non-blocking, best-effort view of the registry.
    ///
    /// Counters are loaded independently, so a snapshot taken while actors are
    /// recording events is not guaranteed to represent one atomic instant.
    #[must_use]
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            publishes_total: load(&self.inner.publishes_total),
            confirmations_total: load(&self.inner.confirmations_total),
            returns_total: load(&self.inner.returns_total),
            deliveries_total: load(&self.inner.deliveries_total),
            acks_total: load(&self.inner.acks_total),
            rejects_total: load(&self.inner.rejects_total),
            reconnects_total: load(&self.inner.reconnects_total),
            backpressure_total: load(&self.inner.backpressure_total),
            confirmation_latency: self.inner.confirmation_latency.snapshot(),
            settlement_latency: self.inner.settlement_latency.snapshot(),
        }
    }

    pub(crate) fn record_publish(&self) {
        increment(&self.inner.publishes_total);
    }

    pub(crate) fn record_confirmation(&self, latency: Duration) {
        increment(&self.inner.confirmations_total);
        self.inner.confirmation_latency.record(latency);
    }

    pub(crate) fn record_return(&self) {
        increment(&self.inner.returns_total);
    }

    pub(crate) fn record_delivery(&self) {
        increment(&self.inner.deliveries_total);
    }

    pub(crate) fn record_ack(&self, latency: Duration) {
        increment(&self.inner.acks_total);
        self.inner.settlement_latency.record(latency);
    }

    pub(crate) fn record_reject(&self, latency: Duration) {
        increment(&self.inner.rejects_total);
        self.inner.settlement_latency.record(latency);
    }

    pub(crate) fn record_reconnect(&self) {
        increment(&self.inner.reconnects_total);
    }

    pub(crate) fn record_backpressure(&self) {
        increment(&self.inner.backpressure_total);
    }
}

impl fmt::Debug for Metrics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.snapshot().fmt(formatter)
    }
}

#[derive(Debug, Default)]
struct MetricsInner {
    publishes_total: AtomicU64,
    confirmations_total: AtomicU64,
    returns_total: AtomicU64,
    deliveries_total: AtomicU64,
    acks_total: AtomicU64,
    rejects_total: AtomicU64,
    reconnects_total: AtomicU64,
    backpressure_total: AtomicU64,
    confirmation_latency: AtomicHistogram,
    settlement_latency: AtomicHistogram,
}

/// Serializable counters and latency distributions with no dynamic labels.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MetricsSnapshot {
    /// Logical publications accepted by a publisher, excluding internal replays.
    pub publishes_total: u64,
    /// `Ack` and `Nack` confirmation responses received from the broker.
    pub confirmations_total: u64,
    /// Mandatory publications returned as unroutable.
    pub returns_total: u64,
    /// Deliveries handed to a consumer caller.
    pub deliveries_total: u64,
    /// Successful acknowledgements, including confirmed delayed releases.
    pub acks_total: u64,
    /// Successful immediate releases using `basic.reject` with requeue.
    pub rejects_total: u64,
    /// Successful connections established after the initial connection.
    pub reconnects_total: u64,
    /// Publications rejected before acceptance because capacity was exhausted.
    pub backpressure_total: u64,
    /// End-to-end latency from publication acceptance to broker confirmation.
    pub confirmation_latency: HistogramSnapshot,
    /// End-to-end latency from delivery reservation to successful settlement.
    pub settlement_latency: HistogramSnapshot,
}

/// Fixed latency histogram whose buckets are non-cumulative.
///
/// `buckets` contains one count per bound followed by an overflow bucket.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HistogramSnapshot {
    pub bounds_ns: [u64; LATENCY_BOUNDS_NS.len()],
    pub buckets: [u64; LATENCY_BOUNDS_NS.len() + 1],
    pub samples: u64,
    pub sum_ns: u64,
}

impl HistogramSnapshot {
    /// Estimates the given percentile from the bucket counts using linear
    /// interpolation within the bucket that crosses the requested rank.
    ///
    /// Returns `None` when no samples have been recorded.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "latency bucket bounds and sample counts fit in 52-bit mantissa"
    )]
    pub fn percentile_ns(&self, percentile: f64) -> Option<u64> {
        if self.samples == 0 {
            return None;
        }

        let percentile = percentile.clamp(0.0, 100.0);
        let samples = self.samples as f64;
        let target = (percentile / 100.0) * samples;

        let mut cumulative: u64 = 0;
        for (index, count) in self.buckets.iter().enumerate() {
            let previous = cumulative;
            cumulative = cumulative.saturating_add(*count);

            if cumulative as f64 >= target {
                let bucket_count = cumulative.saturating_sub(previous) as f64;
                if bucket_count == 0.0 {
                    continue;
                }

                let lower = if index == 0 {
                    0
                } else {
                    self.bounds_ns[index - 1]
                };
                let upper = if index < self.bounds_ns.len() {
                    self.bounds_ns[index]
                } else {
                    self.bounds_ns[self.bounds_ns.len() - 1] * 2
                };

                let previous_f = previous as f64;
                let position_in_bucket = (target - previous_f) / bucket_count;
                let lower_f = lower as f64;
                let width = upper.saturating_sub(lower) as f64;
                let interpolated = lower_f + position_in_bucket * width;
                return Some(round_to_u64(interpolated));
            }
        }

        Some(self.bounds_ns[self.bounds_ns.len() - 1])
    }
}

#[derive(Debug)]
struct AtomicHistogram {
    buckets: [AtomicU64; LATENCY_BOUNDS_NS.len() + 1],
    samples: AtomicU64,
    sum_ns: AtomicU64,
}

impl AtomicHistogram {
    fn record(&self, duration: Duration) {
        let value = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        let bucket = LATENCY_BOUNDS_NS
            .iter()
            .position(|bound| value <= *bound)
            .unwrap_or(LATENCY_BOUNDS_NS.len());
        increment(&self.buckets[bucket]);
        increment(&self.samples);
        self.sum_ns.fetch_add(value, Ordering::Relaxed);
    }

    fn snapshot(&self) -> HistogramSnapshot {
        HistogramSnapshot {
            bounds_ns: LATENCY_BOUNDS_NS,
            buckets: std::array::from_fn(|index| load(&self.buckets[index])),
            samples: load(&self.samples),
            sum_ns: load(&self.sum_ns),
        }
    }
}

impl Default for AtomicHistogram {
    fn default() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            samples: AtomicU64::new(0),
            sum_ns: AtomicU64::new(0),
        }
    }
}

fn increment(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

fn load(counter: &AtomicU64) -> u64 {
    counter.load(Ordering::Relaxed)
}

/// Converts a non-negative `f64` nanosecond value to `u64`, clamping overflow.
///
/// The value is first clamped to `0.0..=i64::MAX` to guarantee a safe cast,
/// then rounded to the nearest integer.
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to i64 range first"
)]
#[expect(
    clippy::cast_precision_loss,
    reason = "latency buckets fit in 52-bit mantissa"
)]
fn round_to_u64(value: f64) -> u64 {
    let clamped = value.clamp(0.0, i64::MAX as f64);
    let rounded = clamped.round();
    u64::try_from(rounded as i64).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(buckets: [u64; LATENCY_BOUNDS_NS.len() + 1], samples: u64) -> HistogramSnapshot {
        HistogramSnapshot {
            bounds_ns: LATENCY_BOUNDS_NS,
            buckets,
            samples,
            sum_ns: 0,
        }
    }

    #[test]
    fn percentile_returns_none_for_empty_histogram() {
        let empty = snapshot([0; LATENCY_BOUNDS_NS.len() + 1], 0);
        assert!(empty.percentile_ns(50.0).is_none());
    }

    #[test]
    fn percentile_handles_single_sample() {
        let mut buckets = [0; LATENCY_BOUNDS_NS.len() + 1];
        buckets[0] = 1; // 1 sample in first bucket (0..=100us)
        let single = snapshot(buckets, 1);
        let p50 = single.percentile_ns(50.0).expect("single sample p50");
        assert!(p50 <= LATENCY_BOUNDS_NS[0]);
    }

    #[test]
    fn percentile_interpolates_within_bucket() {
        let mut buckets = [0; LATENCY_BOUNDS_NS.len() + 1];
        buckets[0] = 10; // 10 samples all in first bucket
        let histogram = snapshot(buckets, 10);

        let p50 = histogram.percentile_ns(50.0).expect("p50");
        let p95 = histogram.percentile_ns(95.0).expect("p95");
        let p99 = histogram.percentile_ns(99.0).expect("p99");

        assert_eq!(p50, LATENCY_BOUNDS_NS[0] / 2);
        assert!(p95 > p50, "p95 ({p95}) should exceed p50 ({p50})");
        assert!(p99 >= p95, "p99 ({p99}) should exceed p95 ({p95})");
    }

    #[test]
    fn percentile_crosses_buckets() {
        let mut buckets = [0; LATENCY_BOUNDS_NS.len() + 1];
        buckets[0] = 50; // 50 samples in first bucket
        buckets[1] = 50; // 50 samples in second bucket
        let histogram = snapshot(buckets, 100);

        let p50 = histogram.percentile_ns(50.0).expect("p50");
        let p95 = histogram.percentile_ns(95.0).expect("p95");

        assert!(p50 <= LATENCY_BOUNDS_NS[0], "p50 should be in first bucket");
        assert!(
            p95 > LATENCY_BOUNDS_NS[0],
            "p95 should cross into second bucket"
        );
    }

    #[test]
    fn percentile_handles_overflow_bucket() {
        let mut buckets = [0; LATENCY_BOUNDS_NS.len() + 1];
        buckets[LATENCY_BOUNDS_NS.len()] = 10; // all in overflow
        let histogram = snapshot(buckets, 10);

        let p99 = histogram.percentile_ns(99.0).expect("p99");
        assert!(
            p99 > 9_000_000_000,
            "p99 ({p99}) should be near upper bound for uniform overflow"
        );
    }
}
