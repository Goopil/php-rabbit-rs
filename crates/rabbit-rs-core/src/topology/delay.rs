use std::{
    error::Error,
    fmt::{self, Write as _},
    time::Duration,
};

use sha2::{Digest, Sha256};

use crate::{
    config::{DelayConfig, DelayMode, ValidatedConfig},
    publisher::Destination,
    transport::{QueueKind, QueueSpec, TopologyChannel},
};

/// Prefix reserved by Rabbit RS for the synthesized TTL delay queues.
pub const DELAY_QUEUE_PREFIX: &str = "rabbit-rs.delay.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DelayStrategy {
    Plugin,
    TtlBuckets(TtlBucketPlan),
}

impl DelayStrategy {
    /// Compiles the delay strategy a pool resolves from its configuration.
    ///
    /// Plugin and auto modes always resolve to [`DelayStrategy::Plugin`]; a TTL
    /// configuration whose bucket compilation fails falls back to the plugin
    /// strategy, matching the recovery coordinator's publisher fallback.
    #[must_use]
    pub fn compile(config: &ValidatedConfig) -> Self {
        let delay = config.delay();
        match delay.mode {
            DelayMode::Plugin | DelayMode::Auto => Self::Plugin,
            DelayMode::Ttl => TtlBucketPlan::compile(delay).map_or(Self::Plugin, Self::TtlBuckets),
        }
    }
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
    /// Returns an error naming the largest configured bucket when the delay
    /// exceeds it.
    pub fn bucket_for(&self, delay: Duration) -> Result<Duration, DelayError> {
        self.buckets
            .iter()
            .copied()
            .find(|bucket| *bucket >= delay)
            .ok_or_else(|| {
                DelayError::new(format!(
                    "delay exceeds the largest configured TTL bucket ({} ms)",
                    self.largest_bucket_ms()
                ))
            })
    }

    #[must_use]
    fn largest_bucket_ms(&self) -> u128 {
        self.buckets.last().map_or(0, Duration::as_millis)
    }

    /// Returns the frozen bucket set the plan routes delays through.
    #[must_use]
    pub fn buckets(&self) -> &[Duration] {
        &self.buckets
    }

    /// Builds the durable delay queue for a destination and rounded bucket.
    ///
    /// The queue name binds the destination **and** a fingerprint of every
    /// broker-visible argument (`x-message-ttl`, `x-expires`,
    /// `x-dead-letter-*`): two configurations with different arguments
    /// therefore declare two distinct queues instead of fighting over one
    /// name, which makes rolling deploys with changed margins or bucket
    /// lists safe (`PRECONDITION_FAILED` storms cannot happen).
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
        let mut spec = QueueSpec {
            name: String::new(),
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
        };
        spec.name = self.stable_queue_name(destination, bucket);
        Ok(spec)
    }

    /// Names the live delay queues the plan produces for a destination.
    ///
    /// Any name matching [`DELAY_QUEUE_PREFIX`] that is absent from this set
    /// for a known destination is an orphan the GC may delete.
    #[must_use]
    pub fn expected_queue_names(&self, destination: &Destination) -> Vec<String> {
        self.buckets
            .iter()
            .map(|bucket| self.stable_queue_name(destination, *bucket))
            .collect()
    }

    /// Deterministic queue name binding the destination and the declaring
    /// arguments: `rabbit-rs.delay.{destination}.{args}.{bucket_ms}`.
    fn stable_queue_name(&self, destination: &Destination, bucket: Duration) -> String {
        format!(
            "{DELAY_QUEUE_PREFIX}{}.{}.{}",
            destination_hash(destination),
            self.args_fingerprint(destination, bucket),
            bucket.as_millis()
        )
    }

    /// Fingerprint of the declaring arguments of a synthesized delay queue.
    ///
    /// Covers every broker-visible argument `queue_for` sets beyond the
    /// queue type: the bucket (`x-message-ttl`), the expiry margin
    /// (`x-expires = bucket + margin`) and the dead-letter target. Any new
    /// argument added to `queue_for` MUST join this digest, otherwise two
    /// argument variants would collide on one queue name.
    fn args_fingerprint(&self, destination: &Destination, bucket: Duration) -> String {
        let mut digest = Sha256::new();
        digest.update(bucket.as_millis().to_be_bytes());
        digest.update(self.expiry_margin.as_millis().to_be_bytes());
        hash_field(&mut digest, &destination.exchange);
        hash_field(&mut digest, &destination.routing_key);
        hex_prefix(&digest.finalize(), 8)
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
}

impl fmt::Display for DelayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for DelayError {}

/// Names the delay queues the pre-identity scheme synthesized (before the
/// declaring arguments joined the name). The GC derives these for the
/// current plan's destinations and buckets so a version upgrade can sweep
/// its own migration orphans.
#[must_use]
pub fn legacy_delay_queue_name(destination: &Destination, bucket: Duration) -> String {
    format!(
        "{DELAY_QUEUE_PREFIX}{}.{}",
        destination_hash(destination),
        bucket.as_millis()
    )
}

fn destination_hash(destination: &Destination) -> String {
    let mut digest = Sha256::new();
    digest.update(destination.exchange.as_bytes());
    digest.update([0]);
    digest.update(destination.routing_key.as_bytes());
    hex_prefix(&digest.finalize(), 16)
}

fn hash_field(digest: &mut Sha256, value: &str) {
    // Length-prefix so distinct field sequences cannot collide on the same
    // concatenated byte stream.
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}

fn hex_prefix(digest: &[u8], length: usize) -> String {
    digest
        .iter()
        .take(length.div_ceil(2))
        .fold(String::with_capacity(length), |mut acc, byte| {
            write!(acc, "{byte:02x}").expect("writing to String is infallible");
            acc
        })
        .chars()
        .take(length)
        .collect()
}

/// Why the GC kept a synthesized delay queue instead of deleting it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SweepKeptReason {
    /// The candidate is not a synthesized delay queue.
    NotSynthesized,
    /// The queue belongs to a destination the sweep does not know; it might
    /// still be live elsewhere.
    UnknownDestination,
    /// The current plan still produces this queue name.
    InCurrentPlan,
    /// The queue still holds messages that must drain through their DLX.
    HasMessages,
    /// The broker refused the deletion (raced traffic or transport failure).
    DeleteRefused,
}

/// A candidate the GC deliberately kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeptDelayQueue {
    pub name: String,
    pub reason: SweepKeptReason,
}

/// Outcome of a [`sweep_delay_queues`] run.
///
/// Every vector is bounded by the number of candidates derived from the
/// sweep inputs (destinations × buckets plus the explicit extras).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DelayQueueSweep {
    /// Queues the broker actually deleted (empty, unconsumed, orphaned).
    pub deleted: Vec<String>,
    /// Queues that no longer exist or whose probe failed; nothing was done.
    pub absent: Vec<String>,
    /// Queues the GC deliberately kept, with the reason.
    pub kept: Vec<KeptDelayQueue>,
}

/// Deletes orphaned synthesized delay queues through an admin channel.
///
/// Orphans appear when a configuration change retires buckets or shifts the
/// declaring arguments: the identity scheme gives them new names and the
/// old queues stop being declared. The sweep is a **maintenance command,
/// not a startup sweep**: run it after a rolling deploy fully completes.
/// While old-version publishers are still running they actively declare and
/// fill the legacy names, so deleting those queues mid-deploy would make
/// their in-flight delayed publishes fail (returned as unroutable).
///
/// Conservatism, in order:
///
/// 1. only [`DELAY_QUEUE_PREFIX`] names are considered; anything else is
///    kept as [`SweepKeptReason::NotSynthesized`];
/// 2. only queues whose destination hash matches one of `destinations` are
///    touched — queues of unknown destinations are never deleted;
/// 3. a name the current plan still produces (for a known destination) is
///    kept as [`SweepKeptReason::InCurrentPlan`];
/// 4. a queue that still holds messages is kept as
///    [`SweepKeptReason::HasMessages`] so in-flight delayed jobs can drain
///    through their DLX;
/// 5. the emptiness probe runs immediately before each delete and the
///    maintenance policy (run after the rolling deploy completes, when no
///    writer produces these names anymore) closes the remaining
///    probe/delete race; the broker cannot enforce emptiness because
///    quorum queues reject `if-unused`/`if-empty` deletes.
///
/// Queues the sweep never reaches (because their identity is unknowable
/// from the current configuration) are not stranded: their own `x-expires`
/// eventually deletes them from the broker. The sweep only accelerates the
/// cleanup it can prove safe.
pub async fn sweep_delay_queues(
    channel: &dyn TopologyChannel,
    plan: &TtlBucketPlan,
    destinations: &[Destination],
    extra_candidates: &[String],
) -> DelayQueueSweep {
    // (destination hash, live names) for every destination the caller vouches for.
    let known: Vec<(String, Vec<String>)> = destinations
        .iter()
        .map(|destination| {
            (
                destination_hash(destination),
                plan.expected_queue_names(destination),
            )
        })
        .collect();

    // Migration orphans for the current plan first, then the explicit
    // candidates (older bucket lists, older margins); deduplicated.
    let mut candidates: Vec<String> = destinations
        .iter()
        .flat_map(|destination| {
            plan.buckets()
                .iter()
                .map(|bucket| legacy_delay_queue_name(destination, *bucket))
        })
        .chain(extra_candidates.iter().cloned())
        .collect();
    candidates.sort_unstable();
    candidates.dedup();

    let mut sweep = DelayQueueSweep::default();
    for name in candidates {
        if let Some(reason) = classify_candidate(&name, &known) {
            sweep.kept.push(KeptDelayQueue { name, reason });
            continue;
        }
        match channel.queue_size(&name).await {
            // Emptiness is re-checked immediately before the delete; the
            // remaining probe/delete race is closed by the maintenance
            // policy documented above (no writers after the deploy).
            Ok(0) => match channel.delete_queue(&name).await {
                Ok(()) => sweep.deleted.push(name),
                Err(_) => sweep.kept.push(KeptDelayQueue {
                    name,
                    reason: SweepKeptReason::DeleteRefused,
                }),
            },
            Ok(_) => sweep.kept.push(KeptDelayQueue {
                name,
                reason: SweepKeptReason::HasMessages,
            }),
            // A missing queue (or a failed probe) is nothing to delete.
            Err(_) => sweep.absent.push(name),
        }
    }
    sweep
}

/// Returns the keep-reason for a candidate, or `None` when the candidate
/// may proceed to the broker probe.
fn classify_candidate(name: &str, known: &[(String, Vec<String>)]) -> Option<SweepKeptReason> {
    if !name.starts_with(DELAY_QUEUE_PREFIX) {
        return Some(SweepKeptReason::NotSynthesized);
    }
    let rest = &name[DELAY_QUEUE_PREFIX.len()..];
    let Some((destination_hash, _)) = rest.split_once('.') else {
        return Some(SweepKeptReason::NotSynthesized);
    };
    if destination_hash.is_empty() {
        return Some(SweepKeptReason::NotSynthesized);
    }
    let Some((_, expected)) = known.iter().find(|(hash, _)| hash == destination_hash) else {
        return Some(SweepKeptReason::UnknownDestination);
    };
    expected
        .iter()
        .any(|queue| queue == name)
        .then_some(SweepKeptReason::InCurrentPlan)
}
