use std::{
    error::Error,
    fmt::{self, Write as _},
    time::Duration,
};

use sha2::{Digest, Sha256};

use crate::{
    config::DelayConfig,
    publisher::Destination,
    transport::{QueueKind, QueueSpec},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DelayStrategy {
    Plugin,
    TtlBuckets(TtlBucketPlan),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TtlBucketPlan {
    buckets: Vec<Duration>,
    expiry_margin: Duration,
}

impl TtlBucketPlan {
    /// Validates, sorts and freezes the configured TTL buckets.
    ///
    /// # Errors
    ///
    /// Returns a permanent error for an empty, zero or oversized bucket set.
    pub fn compile(config: &DelayConfig) -> Result<Self, DelayError> {
        if config.buckets.is_empty() {
            return Err(DelayError::new("at least one TTL bucket is required"));
        }
        if config.buckets.len() > config.max_buckets {
            return Err(DelayError::new(format!(
                "TTL bucket count {} exceeds configured maximum {}",
                config.buckets.len(),
                config.max_buckets
            )));
        }
        if config.buckets.contains(&Duration::ZERO) {
            return Err(DelayError::new("TTL buckets must be greater than zero"));
        }

        let mut buckets = config.buckets.clone();
        buckets.sort_unstable();
        buckets.dedup();
        Ok(Self {
            buckets,
            expiry_margin: config.queue_expiry_margin.max(Duration::from_millis(1)),
        })
    }

    /// Selects the first bucket that cannot deliver before the requested delay.
    ///
    /// # Errors
    ///
    /// Returns an error when the delay exceeds the largest configured bucket.
    pub fn bucket_for(&self, delay: Duration) -> Result<Duration, DelayError> {
        self.buckets
            .iter()
            .copied()
            .find(|bucket| *bucket >= delay)
            .ok_or_else(|| DelayError::new("delay exceeds the largest configured TTL bucket"))
    }

    /// Builds the durable delay queue for a destination and rounded bucket.
    ///
    /// # Errors
    ///
    /// Returns an error when no bucket can contain the requested delay.
    pub fn queue_for(
        &self,
        destination: &Destination,
        delay: Duration,
    ) -> Result<QueueSpec, DelayError> {
        let bucket = self.bucket_for(delay)?;
        let name = stable_queue_name(destination, bucket);
        Ok(QueueSpec {
            name,
            durable: true,
            exclusive: false,
            auto_delete: false,
            kind: QueueKind::Quorum,
            dead_letter_exchange: Some(destination.exchange.to_string()),
            dead_letter_routing_key: Some(destination.routing_key.to_string()),
            message_ttl: Some(bucket),
            expires: Some(bucket.saturating_add(self.expiry_margin)),
            delivery_limit: None,
            arguments: crate::transport::Headers::new(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelayError {
    message: String,
}

impl DelayError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn is_permanent(&self) -> bool {
        true
    }
}

impl fmt::Display for DelayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for DelayError {}

fn stable_queue_name(destination: &Destination, bucket: Duration) -> String {
    let mut digest = Sha256::new();
    digest.update(destination.exchange.as_bytes());
    digest.update([0]);
    digest.update(destination.routing_key.as_bytes());
    let hash: String = digest
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut acc, b| {
            write!(acc, "{b:02x}").expect("writing to String is infallible");
            acc
        });
    format!("rabbit-rs.delay.{}.{}", &hash[..16], bucket.as_millis())
}
