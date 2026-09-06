//! Pure adaptive prefetch controller.
//!
//! The controller keeps an EWMA of settlement latency (which includes the PHP
//! job duration for acknowledged deliveries) and derives the prefetch value
//! that keeps approximately `target_buffer` of ready work buffered. Changes
//! apply through a relative hysteresis so broker QoS is not thrashed.

use std::time::Duration;

/// EWMA smoothing factor applied to each observed settlement latency.
pub(crate) const EWMA_ALPHA: f64 = 0.25;
/// Interval between prefetch applications inside the consumer actor.
pub(crate) const PREFETCH_TICK: Duration = Duration::from_secs(1);
/// Settlement samples required before the first adjustment.
pub(crate) const MIN_SAMPLES: u64 = 3;
/// Relative hysteresis: a change applies when the target differs from the
/// current value by at least `current / HYSTERESIS_DIVISOR` (minimum 1).
const HYSTERESIS_DIVISOR: u64 = 4;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AdaptivePrefetch {
    min: u16,
    max: u16,
    target_buffer: Duration,
    current: u16,
    ewma_ns: f64,
    samples: u64,
}

impl AdaptivePrefetch {
    #[must_use]
    pub(crate) const fn new(min: u16, max: u16, initial: u16, target_buffer: Duration) -> Self {
        Self {
            min,
            max,
            target_buffer,
            current: initial,
            ewma_ns: 0.0,
            samples: 0,
        }
    }

    #[must_use]
    pub(crate) const fn current(&self) -> u16 {
        self.current
    }

    #[must_use]
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "EWMA nanoseconds are non-negative and fit far below u64::MAX"
    )]
    pub(crate) fn ewma(&self) -> Duration {
        if self.ewma_ns.is_finite() && self.ewma_ns > 0.0 {
            Duration::from_nanos(self.ewma_ns as u64)
        } else {
            Duration::ZERO
        }
    }

    /// Records one acknowledged settlement latency.
    pub(crate) fn observe(&mut self, latency: Duration) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "nanosecond durations fit in the 52-bit mantissa"
        )]
        let nanos = latency.as_nanos() as f64;
        self.ewma_ns = if self.samples == 0 {
            nanos
        } else {
            EWMA_ALPHA * nanos + (1.0 - EWMA_ALPHA) * self.ewma_ns
        };
        self.samples = self.samples.saturating_add(1);
    }

    /// Computes the next prefetch adjustment, if hysteresis allows one.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "nanosecond durations fit in the 52-bit mantissa"
    )]
    pub(crate) fn tick(&mut self) -> Option<u16> {
        if self.samples < MIN_SAMPLES {
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "nanosecond durations fit in the 52-bit mantissa"
        )]
        let target_nanos = self.target_buffer.as_nanos() as f64;
        let desired = if self.ewma_ns.is_finite() && self.ewma_ns > 0.0 {
            (target_nanos / self.ewma_ns).ceil()
        } else {
            f64::INFINITY
        };
        let desired = if desired.is_finite() {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "ceil() result is non-negative; saturated by try_from below"
            )]
            let as_u64 = desired as u64;
            u16::try_from(as_u64).unwrap_or(u16::MAX)
        } else {
            u16::MAX
        };
        let desired = desired.clamp(self.min, self.max);
        let threshold = u16::try_from(
            (u32::from(self.current) / u32::try_from(HYSTERESIS_DIVISOR).unwrap_or(1)).max(1),
        )
        .unwrap_or(u16::MAX);
        if desired.abs_diff(self.current) >= threshold {
            self.current = desired;
            Some(desired)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller(initial: u16, min: u16, max: u16, target: Duration) -> AdaptivePrefetch {
        AdaptivePrefetch::new(min, max, initial, target)
    }

    #[test]
    fn tick_requires_three_samples() {
        let mut candidate = controller(16, 1, 256, Duration::from_secs(5));
        candidate.observe(Duration::from_millis(250));
        candidate.observe(Duration::from_millis(250));
        assert_eq!(candidate.tick(), None);
    }

    #[test]
    fn tick_scales_prefetch_to_target_buffer_time() {
        let mut candidate = controller(16, 1, 256, Duration::from_secs(5));
        for _ in 0..3 {
            candidate.observe(Duration::from_millis(250));
        }
        // target 5s / 250ms = 20; |20 - 16| = 4 >= max(1, 16/4) = 4
        assert_eq!(candidate.tick(), Some(20));
    }

    #[test]
    fn tick_suppresses_changes_below_the_hysteresis_band() {
        let mut candidate = controller(16, 1, 256, Duration::from_secs(5));
        for _ in 0..3 {
            candidate.observe(Duration::from_millis(250));
        }
        assert_eq!(candidate.tick(), Some(20));
        // EWMA after a 227ms job: 0.25*227 + 0.75*250 = 244.25ms -> target 21
        candidate.observe(Duration::from_millis(227));
        assert_eq!(candidate.tick(), None, "diff 1 < threshold 5");
        // EWMA after a 100ms job: 0.25*100 + 0.75*244.25 = 208.1875ms -> target 25
        candidate.observe(Duration::from_millis(100));
        assert_eq!(candidate.tick(), Some(25), "diff 5 >= threshold 5");
    }

    #[test]
    fn tick_clamps_very_fast_jobs_to_max() {
        let mut candidate = controller(16, 1, 256, Duration::from_secs(5));
        for _ in 0..3 {
            candidate.observe(Duration::from_millis(1));
        }
        assert_eq!(candidate.tick(), Some(256));
    }

    #[test]
    fn tick_clamps_very_slow_jobs_to_min() {
        let mut candidate = controller(16, 1, 256, Duration::from_secs(5));
        for _ in 0..3 {
            candidate.observe(Duration::from_secs(30));
        }
        assert_eq!(candidate.tick(), Some(1));
    }

    #[test]
    fn tick_respects_a_narrow_fixed_band() {
        let mut candidate = controller(16, 16, 16, Duration::from_secs(5));
        for _ in 0..3 {
            candidate.observe(Duration::from_millis(1));
        }
        assert_eq!(
            candidate.tick(),
            None,
            "target clamps to the band; no change"
        );
    }

    #[test]
    fn ewma_is_zero_before_any_sample_and_equals_the_first_sample() {
        let mut candidate = controller(16, 1, 256, Duration::from_secs(5));
        assert_eq!(candidate.ewma(), Duration::ZERO);
        candidate.observe(Duration::from_millis(100));
        assert_eq!(candidate.ewma(), Duration::from_millis(100));
        assert_eq!(candidate.current(), 16);
    }
}
