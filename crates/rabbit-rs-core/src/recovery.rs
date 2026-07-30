use std::time::Duration;

use async_trait::async_trait;

use crate::transport::TransportErrorKind;

/// Observable lifecycle of a reusable broker connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting {
        attempt: u32,
    },
    Ready {
        generation: u64,
    },
    Recovering {
        attempt: u32,
        retry_in: Duration,
        reason: String,
    },
    FailedPermanent {
        kind: TransportErrorKind,
        reason: String,
    },
    Closed,
}

/// Exponential retry parameters applied before jitter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryPolicy {
    initial_delay: Duration,
    maximum_delay: Duration,
}

impl RecoveryPolicy {
    #[must_use]
    pub const fn new(initial_delay: Duration, maximum_delay: Duration) -> Self {
        Self {
            initial_delay,
            maximum_delay,
        }
    }

    #[must_use]
    pub fn delay_for_failure(self, consecutive_failures: u32) -> Duration {
        if consecutive_failures == 0 {
            return Duration::ZERO;
        }

        let exponent = consecutive_failures.saturating_sub(1).min(31);
        self.initial_delay
            .saturating_mul(1_u32 << exponent)
            .min(self.maximum_delay)
    }
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self::new(Duration::from_millis(100), Duration::from_secs(30))
    }
}

/// Abstract clock used to make recovery tests independent from wall time.
#[async_trait]
pub trait Clock: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

/// Tokio-backed production clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioClock;

#[async_trait]
impl Clock for TokioClock {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Mutates a capped exponential delay to spread reconnect attempts.
pub trait JitterSource: Send + Sync {
    fn apply(&self, delay: Duration) -> Duration;
}

/// Deterministic jitter used by tests and reproducible environments.
#[derive(Clone, Copy, Debug, Default)]
pub struct IdentityJitter;

impl JitterSource for IdentityJitter {
    fn apply(&self, delay: Duration) -> Duration {
        delay
    }
}

/// Production equal-jitter strategy, returning a delay between 50% and 100%.
#[derive(Clone, Copy, Debug, Default)]
pub struct EqualJitter;

impl JitterSource for EqualJitter {
    fn apply(&self, delay: Duration) -> Duration {
        let half = delay / 2;
        let random_nanos = fastrand::u64(0..=u64::try_from(half.as_nanos()).unwrap_or(u64::MAX));
        half.saturating_add(Duration::from_nanos(random_nanos))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::RecoveryPolicy;

    #[test]
    fn delay_is_zero_before_any_failure() {
        assert_eq!(
            RecoveryPolicy::default().delay_for_failure(0),
            Duration::ZERO
        );
    }
}
