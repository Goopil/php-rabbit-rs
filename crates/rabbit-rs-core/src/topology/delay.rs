use std::{error::Error, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::{
    config::{DelayConfig, DelayMode},
    publisher::Destination,
    transport::{QueueKind, QueueSpec, TransportResult},
};

#[async_trait]
pub trait DelayPluginProbe: Send + Sync {
    /// Checks whether `x-delayed-message` is installed and usable.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the capability check cannot complete.
    async fn is_available(&self) -> TransportResult<bool>;
}

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
            dead_letter_exchange: Some(destination.exchange.clone()),
            dead_letter_routing_key: Some(destination.routing_key.clone()),
            message_ttl: Some(bucket),
            expires: Some(bucket.saturating_add(self.expiry_margin)),
        })
    }
}

#[derive(Debug, Default)]
pub struct DelayStrategyResolver {
    cached_plugin: Option<(u64, bool)>,
}

impl DelayStrategyResolver {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cached_plugin: None,
        }
    }

    /// Resolves plugin or TTL mode with bounded, generation-scoped detection.
    ///
    /// # Errors
    ///
    /// Returns a permanent error for invalid TTL configuration or when plugin
    /// mode is mandatory but the capability is unavailable.
    pub async fn resolve(
        &mut self,
        config: &DelayConfig,
        generation: u64,
        probe: Arc<dyn DelayPluginProbe>,
    ) -> Result<DelayStrategy, DelayError> {
        if config.mode == DelayMode::Ttl {
            return TtlBucketPlan::compile(config).map(DelayStrategy::TtlBuckets);
        }

        let plugin_available = if let Some((cached_generation, available)) = self.cached_plugin {
            if cached_generation == generation {
                available
            } else {
                self.detect(config, generation, &probe).await?
            }
        } else {
            self.detect(config, generation, &probe).await?
        };

        if plugin_available {
            Ok(DelayStrategy::Plugin)
        } else if config.mode == DelayMode::Plugin {
            Err(DelayError::new(
                "x-delayed-message plugin is required but unavailable",
            ))
        } else {
            TtlBucketPlan::compile(config).map(DelayStrategy::TtlBuckets)
        }
    }

    async fn detect(
        &mut self,
        config: &DelayConfig,
        generation: u64,
        probe: &Arc<dyn DelayPluginProbe>,
    ) -> Result<bool, DelayError> {
        let detected = tokio::time::timeout(config.detection_timeout, probe.is_available()).await;
        let available = match detected {
            Ok(Ok(available)) => available,
            Ok(Err(error)) if config.mode == DelayMode::Plugin => {
                return Err(DelayError::new(error.to_string()));
            }
            Ok(Err(_)) | Err(_) => false,
        };
        self.cached_plugin = Some((generation, available));
        Ok(available)
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
    let hash = format!("{:x}", digest.finalize());
    format!("rabbit-rs.delay.{}.{}", &hash[..16], bucket.as_millis())
}
