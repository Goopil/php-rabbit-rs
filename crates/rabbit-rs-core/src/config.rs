use std::{collections::HashSet, fmt, path::PathBuf, str::FromStr, time::Duration};

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};

use crate::error::ConfigError;
use crate::transport::QueueKind;

/// A `RabbitMQ` network endpoint.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    host: String,
    port: u16,
}

impl Endpoint {
    #[must_use]
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }
}

/// Authentication material for a broker connection.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    username: String,
    password: SecretString,
}

impl Credentials {
    #[must_use]
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        let password: String = password.into();

        Self {
            username: username.into(),
            password: SecretString::from(password),
        }
    }

    pub(crate) fn username(&self) -> &str {
        &self.username
    }

    pub(crate) fn password(&self) -> &str {
        self.password.expose_secret()
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// How the transport verifies the broker's TLS certificate.
///
/// - `Peer` (default): full rustls verification against the platform trust
///   store plus any `ca_cert` chain. The verified server name is always the
///   AMQP connection host: the underlying AMQP transport (lapin 4.10) derives
///   TLS SNI from the URI host and exposes no override.
/// - `None`: rejected at validation and by the transport with a typed
///   [`ConfigError`]. lapin 4.10 does not allow disabling certificate
///   verification, so accepting the value would silently do nothing.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TlsVerify {
    #[default]
    Peer,
    None,
}

/// TLS parameters that are safe to retain in normalized configuration.
///
/// Contract enforced by [`Config::validate`] and the transport:
///
/// - `verify` is `Peer` (default) or an explicit validation error (see
///   [`TlsVerify`]).
/// - `server_name` is an explicit assertion of the TLS server name. The
///   transport always uses the AMQP connection host (its first endpoint) as
///   SNI, so a `server_name` different from the first host is rejected with a
///   typed [`ConfigError`] instead of being silently ignored. A matching
///   value is accepted as a no-op.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct TlsConfig {
    enabled: bool,
    ca_cert: Option<PathBuf>,
    client_cert: Option<PathBuf>,
    client_key: Option<PathBuf>,
    verify: TlsVerify,
    server_name: Option<String>,
}

impl TlsConfig {
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            ca_cert: None,
            client_cert: None,
            client_key: None,
            verify: TlsVerify::Peer,
            server_name: None,
        }
    }

    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn ca_cert(&self) -> Option<&PathBuf> {
        self.ca_cert.as_ref()
    }

    #[must_use]
    pub fn client_cert(&self) -> Option<&PathBuf> {
        self.client_cert.as_ref()
    }

    #[must_use]
    pub fn client_key(&self) -> Option<&PathBuf> {
        self.client_key.as_ref()
    }

    #[must_use]
    pub const fn verify(&self) -> TlsVerify {
        self.verify
    }

    #[must_use]
    pub fn server_name(&self) -> Option<&str> {
        self.server_name.as_deref()
    }
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

/// A named broker connection configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerConfig {
    pub name: String,
    pub hosts: Vec<Endpoint>,
    pub vhost: String,
    pub credentials: Credentials,
    pub tls: TlsConfig,
    #[serde(deserialize_with = "deserialize_duration_seconds")]
    pub heartbeat: Duration,
}

impl BrokerConfig {
    #[must_use]
    pub fn hosts(&self) -> &[Endpoint] {
        &self.hosts
    }
}

/// Per-subscription scheduling and flow-control parameters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionConfig {
    pub name: String,
    pub broker: String,
    pub queue: String,
    pub weight: u16,
    pub priority_class: i16,
    pub prefetch: PrefetchConfig,
    #[serde(
        default = "default_starvation_after",
        deserialize_with = "deserialize_duration_seconds"
    )]
    pub starvation_after: Duration,
    #[serde(default = "default_max_buffered_bytes")]
    pub max_buffered_bytes: u64,
    /// Best-effort mode: ACK the delivery to the broker before dispatch to PHP.
    ///
    /// When `true`, the consumer auto-acks each delivery immediately and
    /// presents it with [`DeliveryState::AutoAcked`]. Settlement calls
    /// (`ack`, `release`, `reject`) on such a delivery return
    /// [`ConsumerErrorKind::AlreadySettled`].
    #[serde(default)]
    pub early_ack: bool,

    /// Broker-side auto-ack: `RabbitMQ` auto-acks each delivery at the protocol level,
    /// eliminating all ack frames. Requires `early_ack = true` (enforced by
    /// `Config::validate`); best-effort opt-in is additionally gated by the
    /// Laravel `best_effort` flag.
    #[serde(default)]
    pub no_ack: bool,
}

/// Scheduler algorithms supported by the stable configuration format.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerStrategy {
    WeightedFair,
}

/// Per-subscription prefetch policy: a fixed `QoS` value or an adaptive
/// controller driven by observed job duration.
///
/// Wire forms accepted: a plain integer (`16`), `{"mode": "fixed", "value": N}`,
/// or `{"mode": "adaptive", "initial": N, "min": N, "max": N,
/// "target_buffer_seconds": S}`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrefetchConfig {
    Fixed(u16),
    Adaptive {
        initial: u16,
        min: u16,
        max: u16,
        target_buffer: Duration,
    },
}

impl PrefetchConfig {
    /// The prefetch applied when the subscription starts.
    #[must_use]
    pub const fn initial_value(&self) -> u16 {
        match self {
            Self::Fixed(value) => *value,
            Self::Adaptive { initial, .. } => *initial,
        }
    }

    /// The highest prefetch the policy can reach; bounds spawn buffer capacity.
    #[must_use]
    pub const fn ceiling(&self) -> u16 {
        match self {
            Self::Fixed(value) => *value,
            Self::Adaptive { max, .. } => *max,
        }
    }
}

impl<'de> Deserialize<'de> for PrefetchConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de;

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Plain(u16),
            Mapped(PrefetchMap),
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PrefetchMap {
            mode: PrefetchMode,
            value: Option<u16>,
            initial: Option<u16>,
            min: Option<u16>,
            max: Option<u16>,
            #[serde(default, deserialize_with = "deserialize_duration_seconds_opt")]
            target_buffer_seconds: Option<Duration>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum PrefetchMode {
            Fixed,
            Adaptive,
        }

        match Wire::deserialize(deserializer)? {
            Wire::Plain(value) => Ok(Self::Fixed(value)),
            Wire::Mapped(map) => match map.mode {
                PrefetchMode::Fixed => {
                    let value = map.value.ok_or_else(|| de::Error::missing_field("value"))?;
                    Ok(Self::Fixed(value))
                }
                PrefetchMode::Adaptive => Ok(Self::Adaptive {
                    initial: map
                        .initial
                        .ok_or_else(|| de::Error::missing_field("initial"))?,
                    min: map.min.ok_or_else(|| de::Error::missing_field("min"))?,
                    max: map.max.ok_or_else(|| de::Error::missing_field("max"))?,
                    target_buffer: map
                        .target_buffer_seconds
                        .ok_or_else(|| de::Error::missing_field("target_buffer_seconds"))?,
                }),
            },
        }
    }
}

/// Worker-level scheduler parameters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SchedulerConfig {
    pub strategy: SchedulerStrategy,
    /// Deserialized for wire compatibility with existing hand-written configs,
    /// then ignored: broker `QoS` structurally bounds un-acked deliveries per
    /// channel, so a worker-level dispatch budget would be dead weight.
    #[serde(default)]
    #[allow(dead_code)]
    max_in_flight: Option<u16>,
}

impl SchedulerConfig {
    #[must_use]
    pub const fn weighted_fair() -> Self {
        Self {
            strategy: SchedulerStrategy::WeightedFair,
            max_in_flight: None,
        }
    }
}

/// A set of subscriptions consumed by one Laravel worker profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkerProfile {
    pub name: String,
    pub subscriptions: Vec<SubscriptionConfig>,
    pub scheduler: SchedulerConfig,
}

/// Controls whether Rabbit RS mutates or only observes broker topology.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TopologyMode {
    Declare,
    Verify,
    External,
}

/// Preferred delayed-delivery backend.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DelayMode {
    Auto,
    Plugin,
    Ttl,
}

/// Bounded delayed-delivery configuration shared by topology and publishers.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct DelayConfig {
    pub mode: DelayMode,
    #[serde(deserialize_with = "deserialize_duration_seconds_vec")]
    pub buckets: Vec<Duration>,
    pub max_buckets: usize,
    #[serde(deserialize_with = "deserialize_duration_seconds")]
    pub queue_expiry_margin: Duration,
}

impl Default for DelayConfig {
    fn default() -> Self {
        Self {
            mode: DelayMode::Auto,
            buckets: vec![
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(30),
                Duration::from_mins(2),
            ],
            max_buckets: 8,
            queue_expiry_margin: Duration::from_mins(1),
        }
    }
}

impl FromStr for TopologyMode {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "declare" => Ok(Self::Declare),
            "verify" => Ok(Self::Verify),
            "external" => Ok(Self::External),
            _ => Err(ConfigError::new(
                "topology.mode",
                format!("unsupported topology mode '{value}'"),
            )),
        }
    }
}

/// Dead-letter configuration attached to the application topology.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeadLetterConfig {
    pub enabled: bool,
    pub exchange: String,
    pub queue: String,
    pub routing_key: Option<String>,
}

/// Publisher safety mode determining the delivery guarantee level.
///
/// Only [`SafetyMode::Unsafe`] and [`SafetyMode::Safe`] provide
/// at-least-once delivery: publications are retained in bounded process
/// memory and replayed with their original `message_id` across connection
/// recovery. [`SafetyMode::Blind`] is an explicit fire-and-forget contract
/// with no replay.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SafetyMode {
    /// Fire-and-forget: publishing hands the message to a bounded background
    /// pump (backpressure by blocking) and returns without waiting for any
    /// transport outcome. A transport failure after the hand-off — including
    /// a channel cleared during recovery — is a silent loss: no confirmation,
    /// no mandatory return, no replay. A flush barrier
    /// ([`PublisherHandle::flush_blind`](crate::publisher::PublisherHandle::flush_blind))
    /// is the only completion point.
    Blind,
    /// Synchronous socket write, no confirms. Message reached kernel socket buffer.
    Unsafe,
    /// Confirm mode + mandatory routing. At-least-once delivery guarantee.
    #[default]
    Safe,
}

/// Publisher configuration section controlling confirms, mandatory routing,
/// and confirmation timeout.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct PublisherConfigSection {
    pub safety: SafetyMode,
    /// Deprecated: use `safety = "safe"` instead. Defaults to true for backward compat.
    pub confirms: bool,
    /// Deprecated: must be `true` (or omitted). `false` is rejected at validation —
    /// mandatory routing is part of the safe delivery guarantee; opt out with
    /// `safety = "unsafe"` or `"blind"`.
    pub mandatory: bool,
    #[serde(deserialize_with = "deserialize_duration_millis")]
    pub confirm_timeout: Duration,
}

impl PublisherConfigSection {
    /// Returns the effective safety mode, deriving from legacy `confirms`/`mandatory`
    /// flags when `safety` was not explicitly set.
    ///
    /// - `safety != Safe` → returned as-is (explicitly chosen).
    /// - `safety == Safe` (default) + `confirms=false` → `Unsafe`.
    /// - `safety == Safe` (default) + `confirms=true` → `Safe`.
    #[must_use]
    pub fn effective_safety(&self) -> SafetyMode {
        if !matches!(self.safety, SafetyMode::Safe) {
            return self.safety;
        }
        if self.confirms {
            SafetyMode::Safe
        } else {
            SafetyMode::Unsafe
        }
    }
}

impl Default for PublisherConfigSection {
    fn default() -> Self {
        Self {
            safety: SafetyMode::Safe,
            confirms: true,
            mandatory: true,
            confirm_timeout: Duration::from_secs(30),
        }
    }
}

/// Consumer acquisition settings.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ConsumerConfigSection {
    /// Maximum wall-clock time a caller waits for a consumer handle to become
    /// ready (connection + topology + `basic_consume`) before a typed
    /// transport error is returned. Prevents unbounded blocking on
    /// black-holed brokers.
    #[serde(deserialize_with = "deserialize_duration_millis")]
    pub wait_timeout: Duration,
    /// Inclusive cap on resolved delivery attempts per message. Deliveries
    /// above the cap are settled terminally (dead-lettered, or explicitly
    /// acknowledged and logged when no dead-letter exchange is bound) instead
    /// of being dispatched to the caller.
    pub max_attempts: Option<u32>,
}

/// Serde default for the optional `max_attempts` key: the wrap is intentional
/// (an explicit JSON `null` must disable the cap, the omitted key defaults to
/// the documented limit).
#[allow(clippy::unnecessary_wraps)]
fn default_max_attempts() -> Option<u32> {
    Some(crate::consumer::DEFAULT_MAX_ATTEMPTS)
}

impl Default for ConsumerConfigSection {
    fn default() -> Self {
        Self {
            wait_timeout: Duration::from_secs(30),
            max_attempts: default_max_attempts(),
        }
    }
}

/// Unvalidated user configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub brokers: Vec<BrokerConfig>,
    pub workers: Vec<WorkerProfile>,
    pub topology_mode: TopologyMode,
    #[serde(default)]
    pub delay: DelayConfig,
    #[serde(default)]
    pub dead_letter: Option<DeadLetterConfig>,
    #[serde(default)]
    pub delivery_limit: Option<u32>,
    #[serde(default)]
    pub publisher: PublisherConfigSection,
    #[serde(default)]
    pub consumer: ConsumerConfigSection,
    #[serde(default = "default_queue_type")]
    pub queue_type: QueueKind,
    #[serde(default = "default_true")]
    pub queue_durable: bool,
}

fn default_queue_type() -> QueueKind {
    QueueKind::Quorum
}

fn default_true() -> bool {
    true
}

impl Config {
    /// Validates and canonicalizes configuration before any connection is opened.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] with the exact invalid configuration path.
    pub fn validate(mut self) -> Result<ValidatedConfig, ConfigError> {
        if self.brokers.is_empty() {
            return Err(ConfigError::new(
                "brokers",
                "at least one broker is required",
            ));
        }
        for broker in &mut self.brokers {
            if broker.hosts.is_empty() {
                return Err(ConfigError::new(
                    format!("brokers.{}.hosts", broker.name),
                    "at least one host is required",
                ));
            }
            if broker.heartbeat < Duration::from_secs(1)
                || broker.heartbeat > Duration::from_secs(u64::from(u16::MAX))
            {
                return Err(ConfigError::new(
                    format!("brokers.{}.heartbeat", broker.name),
                    "heartbeat must be between 1 and 65535 seconds (AMQP wire limit)",
                ));
            }

            broker.hosts.sort_unstable();
            Self::validate_broker_tls(broker)?;
        }
        self.brokers
            .sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let broker_names: HashSet<_> = self
            .brokers
            .iter()
            .map(|broker| broker.name.as_str())
            .collect();

        for worker in &mut self.workers {
            Self::validate_worker(worker, &broker_names)?;

            worker
                .subscriptions
                .sort_unstable_by(|left, right| left.name.cmp(&right.name));
        }
        self.workers
            .sort_unstable_by(|left, right| left.name.cmp(&right.name));

        Self::validate_delay(&self.delay)?;

        if self.consumer.wait_timeout < Duration::from_secs(1)
            || self.consumer.wait_timeout > Duration::from_hours(24)
        {
            return Err(ConfigError::new(
                "consumer.wait_timeout",
                "wait_timeout must be between 1 second and 24 hours",
            ));
        }

        if self.consumer.max_attempts == Some(0) {
            return Err(ConfigError::new(
                "consumer.max_attempts",
                "max_attempts must be a positive integer",
            ));
        }

        if !self.publisher.mandatory {
            return Err(ConfigError::new(
                "publisher.mandatory",
                "mandatory=false is no longer supported: mandatory routing is part of the safe \
                 delivery guarantee; opt out with publisher.safety = \"unsafe\" or \"blind\" \
                 instead",
            ));
        }

        if self.publisher.confirm_timeout < Duration::from_secs(1) {
            return Err(ConfigError::new(
                "publisher.confirm_timeout",
                "confirm_timeout must be at least 1 second",
            ));
        }

        let fingerprint = ConfigFingerprint::calculate(&self);

        Ok(ValidatedConfig {
            brokers: self.brokers,
            workers: self.workers,
            topology_mode: self.topology_mode,
            delay: self.delay,
            dead_letter: self.dead_letter,
            delivery_limit: self.delivery_limit,
            publisher: self.publisher,
            consumer: self.consumer,
            queue_type: self.queue_type,
            queue_durable: self.queue_durable,
            fingerprint,
        })
    }

    /// Enforces the TLS contract documented on [`TlsConfig`]: `verify = none`
    /// and a `server_name` differing from the first (sorted) host are rejected
    /// instead of being silently ignored by the transport.
    fn validate_broker_tls(broker: &BrokerConfig) -> Result<(), ConfigError> {
        let tls = &broker.tls;
        if !tls.enabled {
            return Ok(());
        }
        if tls.verify == TlsVerify::None {
            return Err(ConfigError::new(
                format!("brokers.{}.tls.verify", broker.name),
                "'none' requires a custom TLS connector, which the AMQP transport (lapin 4.10) \
                 does not support; use 'peer' or disable tls.enabled",
            ));
        }
        let first_host = broker
            .hosts
            .first()
            .map(|endpoint| endpoint.host.as_str())
            .unwrap_or_default();
        if let Some(server_name) = &tls.server_name
            && server_name != first_host
        {
            return Err(ConfigError::new(
                format!("brokers.{}.tls.server_name", broker.name),
                format!(
                    "'{server_name}' overrides the TLS server name, which the AMQP transport \
                     (lapin 4.10) cannot do: SNI is always the connection host; supported \
                     value: '{first_host}'"
                ),
            ));
        }
        Ok(())
    }

    fn validate_worker(
        worker: &WorkerProfile,
        broker_names: &HashSet<&str>,
    ) -> Result<(), ConfigError> {
        let worker_name = worker.name.as_str();
        if worker.subscriptions.is_empty() {
            return Err(ConfigError::new(
                format!("workers.{worker_name}.subscriptions"),
                "at least one subscription is required",
            ));
        }
        let mut subscription_names: HashSet<&str> = HashSet::new();
        for subscription in &worker.subscriptions {
            let path = format!("workers.{worker_name}.subscriptions.{}", subscription.name);
            if !subscription_names.insert(subscription.name.as_str()) {
                return Err(ConfigError::new(
                    path,
                    "subscription name must be unique within a worker profile",
                ));
            }
            if !broker_names.contains(subscription.broker.as_str()) {
                return Err(ConfigError::new(
                    path + ".broker",
                    "references an unknown broker",
                ));
            }
            if subscription.weight == 0 {
                return Err(ConfigError::new(
                    path + ".weight",
                    "weight must be greater than zero",
                ));
            }
            let prefetch_base = format!(
                "workers.{worker_name}.subscriptions.{}.prefetch",
                subscription.name
            );
            match subscription.prefetch {
                PrefetchConfig::Fixed(0) => {
                    return Err(ConfigError::new(
                        prefetch_base,
                        "prefetch must be greater than zero",
                    ));
                }
                PrefetchConfig::Fixed(_) => {}
                PrefetchConfig::Adaptive {
                    initial,
                    min,
                    max,
                    target_buffer,
                } => {
                    if min == 0 {
                        return Err(ConfigError::new(
                            format!("{prefetch_base}.min"),
                            "min must be greater than zero",
                        ));
                    }
                    if max < min {
                        return Err(ConfigError::new(
                            format!("{prefetch_base}.max"),
                            "max must be greater than or equal to min",
                        ));
                    }
                    if initial < min || initial > max {
                        return Err(ConfigError::new(
                            format!("{prefetch_base}.initial"),
                            "initial must be within [min, max]",
                        ));
                    }
                    if target_buffer.is_zero() {
                        return Err(ConfigError::new(
                            format!("{prefetch_base}.target_buffer_seconds"),
                            "target_buffer_seconds must be greater than zero",
                        ));
                    }
                    if subscription.early_ack || subscription.no_ack {
                        return Err(ConfigError::new(
                            format!("{prefetch_base}.mode"),
                            "adaptive prefetch requires consumer acknowledgements: \
                             early_ack and no_ack must be false",
                        ));
                    }
                }
            }
            if subscription.starvation_after.is_zero() {
                return Err(ConfigError::new(
                    path + ".starvation_after",
                    "starvation_after must be greater than zero",
                ));
            }
            if subscription.no_ack && !subscription.early_ack {
                return Err(ConfigError::new(
                    path + ".no_ack",
                    "no_ack requires early_ack: broker-side auto-ack bypasses prefetch, so an \
                     unattended consumer would buffer deliveries without any bound",
                ));
            }
        }
        Ok(())
    }

    fn validate_delay(delay: &DelayConfig) -> Result<(), ConfigError> {
        if delay.buckets.is_empty() {
            return Err(ConfigError::new(
                "delay.buckets",
                "at least one TTL bucket is required",
            ));
        }
        if delay.buckets.len() > delay.max_buckets {
            return Err(ConfigError::new(
                "delay.buckets",
                format!(
                    "TTL bucket count {} exceeds configured maximum {}",
                    delay.buckets.len(),
                    delay.max_buckets
                ),
            ));
        }
        if delay.buckets.contains(&Duration::ZERO) {
            return Err(ConfigError::new(
                "delay.buckets",
                "TTL buckets must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Canonical configuration accepted by the runtime registry.
#[derive(Clone, Debug)]
pub struct ValidatedConfig {
    brokers: Vec<BrokerConfig>,
    workers: Vec<WorkerProfile>,
    topology_mode: TopologyMode,
    delay: DelayConfig,
    dead_letter: Option<DeadLetterConfig>,
    delivery_limit: Option<u32>,
    publisher: PublisherConfigSection,
    consumer: ConsumerConfigSection,
    queue_type: QueueKind,
    queue_durable: bool,
    fingerprint: ConfigFingerprint,
}

/// Prefix marking a worker profile name synthesized on first use (the
/// Laravel `auto_subscribe` contract). Synthesized profiles carry exactly
/// one subscription named `auto`, mirroring the defaults the Laravel
/// compiler emits for a fixed-prefetch connection.
pub const AUTO_PROFILE_PREFIX: &str = "__auto__.";

impl ValidatedConfig {
    #[must_use]
    pub fn broker(&self, name: &str) -> Option<&BrokerConfig> {
        self.brokers.iter().find(|broker| broker.name == name)
    }

    #[must_use]
    pub fn worker(&self, name: &str) -> Option<&WorkerProfile> {
        self.workers.iter().find(|worker| worker.name == name)
    }

    /// Synthesizes the default worker profile for an `__auto__.name` profile.
    ///
    /// The auto path resolves queues that no configured profile covers: the
    /// synthesized profile subscribes to the queue named after the prefix on
    /// the single configured broker, with the same defaults the Laravel
    /// compiler emits (weight 1, priority class 0, fixed prefetch 64,
    /// 30 s starvation, acknowledgements on). The profile is validated with
    /// the same rules as configured profiles, so every bound applies.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] when the name lacks the `__auto__.` prefix,
    /// carries an empty queue part, more than one broker is configured, or the
    /// synthesized profile fails validation.
    pub fn synthesize_auto_profile(&self, profile: &str) -> Result<WorkerProfile, ConfigError> {
        const SUBSCRIPTION_NAME: &str = "auto";
        let Some(queue) = profile.strip_prefix(AUTO_PROFILE_PREFIX) else {
            return Err(ConfigError::new(
                format!("workers.{profile}"),
                "unknown worker profile",
            ));
        };
        if queue.is_empty() {
            return Err(ConfigError::new(
                format!("workers.{profile}"),
                "automatic profile names must carry a queue after __auto__.",
            ));
        }
        if self.brokers.len() != 1 {
            return Err(ConfigError::new(
                format!("workers.{profile}.subscriptions.{SUBSCRIPTION_NAME}.broker"),
                "automatic profiles require a single configured broker; \
                 declare this profile under workers.*",
            ));
        }
        let worker = WorkerProfile {
            name: profile.to_owned(),
            subscriptions: vec![SubscriptionConfig {
                name: SUBSCRIPTION_NAME.to_owned(),
                broker: self.brokers[0].name.clone(),
                queue: queue.to_owned(),
                weight: 1,
                priority_class: 0,
                prefetch: PrefetchConfig::Fixed(64),
                starvation_after: Duration::from_secs(30),
                max_buffered_bytes: default_max_buffered_bytes(),
                early_ack: false,
                no_ack: false,
            }],
            scheduler: SchedulerConfig::weighted_fair(),
        };
        let broker_names: HashSet<&str> = self.brokers.iter().map(|b| b.name.as_str()).collect();
        Config::validate_worker(&worker, &broker_names)?;
        Ok(worker)
    }

    /// Returns all worker profiles in canonical order.
    #[must_use]
    pub fn worker_profiles(&self) -> &[WorkerProfile] {
        &self.workers
    }

    #[must_use]
    pub const fn topology_mode(&self) -> TopologyMode {
        self.topology_mode
    }

    #[must_use]
    pub const fn delay(&self) -> &DelayConfig {
        &self.delay
    }

    #[must_use]
    pub const fn dead_letter(&self) -> Option<&DeadLetterConfig> {
        self.dead_letter.as_ref()
    }

    #[must_use]
    pub const fn delivery_limit(&self) -> Option<u32> {
        self.delivery_limit
    }

    #[must_use]
    pub const fn publisher(&self) -> PublisherConfigSection {
        self.publisher
    }

    #[must_use]
    pub const fn consumer(&self) -> ConsumerConfigSection {
        self.consumer
    }

    #[must_use]
    pub const fn queue_type(&self) -> QueueKind {
        self.queue_type
    }

    #[must_use]
    pub const fn queue_durable(&self) -> bool {
        self.queue_durable
    }

    #[must_use]
    pub const fn fingerprint(&self) -> &ConfigFingerprint {
        &self.fingerprint
    }
}

/// A non-reversible identity for normalized configuration, including credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigFingerprint([u8; 32]);

impl ConfigFingerprint {
    pub(crate) const fn into_bytes(self) -> [u8; 32] {
        self.0
    }

    fn calculate(config: &Config) -> Self {
        let mut digest = Sha256::new();

        hash_value(&mut digest, topology_mode_name(config.topology_mode));
        for broker in &config.brokers {
            hash_broker(&mut digest, broker);
        }
        for worker in &config.workers {
            hash_value(&mut digest, &worker.name);
            hash_value(&mut digest, scheduler_name(worker.scheduler.strategy));
            for subscription in &worker.subscriptions {
                hash_value(&mut digest, &subscription.name);
                hash_value(&mut digest, &subscription.broker);
                hash_value(&mut digest, &subscription.queue);
                digest.update(subscription.weight.to_be_bytes());
                digest.update(subscription.priority_class.to_be_bytes());
                match subscription.prefetch {
                    PrefetchConfig::Fixed(value) => {
                        hash_value(&mut digest, "prefetch:fixed");
                        digest.update(value.to_be_bytes());
                    }
                    PrefetchConfig::Adaptive {
                        initial,
                        min,
                        max,
                        target_buffer,
                    } => {
                        hash_value(&mut digest, "prefetch:adaptive");
                        digest.update(initial.to_be_bytes());
                        digest.update(min.to_be_bytes());
                        digest.update(max.to_be_bytes());
                        digest.update(
                            u64::try_from(target_buffer.as_millis())
                                .unwrap_or(u64::MAX)
                                .to_be_bytes(),
                        );
                    }
                }
                digest.update(subscription.starvation_after.as_secs().to_be_bytes());
                digest.update(subscription.max_buffered_bytes.to_be_bytes());
                hash_value(
                    &mut digest,
                    if subscription.early_ack {
                        "early_ack"
                    } else {
                        "no_early_ack"
                    },
                );
                hash_value(
                    &mut digest,
                    if subscription.no_ack { "no_ack" } else { "ack" },
                );
            }
        }

        hash_value(&mut digest, delay_mode_name(config.delay.mode));
        hash_value(&mut digest, &format!("{:?}", config.delay.buckets));
        digest.update(config.delay.max_buckets.to_be_bytes());
        digest.update(config.delay.queue_expiry_margin.as_secs().to_be_bytes());

        if let Some(dl) = &config.dead_letter {
            hash_value(&mut digest, "dead_letter");
            hash_value(&mut digest, if dl.enabled { "1" } else { "0" });
            hash_value(&mut digest, &dl.exchange);
            hash_value(&mut digest, &dl.queue);
            hash_value(&mut digest, dl.routing_key.as_deref().unwrap_or_default());
        } else {
            hash_value(&mut digest, "no_dead_letter");
        }
        if let Some(limit) = config.delivery_limit {
            hash_value(&mut digest, "delivery_limit");
            digest.update(limit.to_be_bytes());
        } else {
            hash_value(&mut digest, "no_delivery_limit");
        }

        hash_publisher(&mut digest, &config.publisher);
        hash_consumer(&mut digest, &config.consumer);

        hash_value(
            &mut digest,
            match config.queue_type {
                QueueKind::Classic => "queue_type:classic",
                QueueKind::Quorum => "queue_type:quorum",
            },
        );
        hash_value(
            &mut digest,
            if config.queue_durable {
                "queue_durable:true"
            } else {
                "queue_durable:false"
            },
        );

        Self(digest.finalize().into())
    }
}

fn hash_broker(digest: &mut Sha256, broker: &BrokerConfig) {
    hash_value(digest, &broker.name);
    hash_value(digest, &broker.vhost);
    hash_value(digest, &broker.credentials.username);
    hash_value(digest, broker.credentials.password.expose_secret());
    hash_value(digest, if broker.tls.enabled { "tls" } else { "plain" });
    hash_value(digest, tls_verify_name(broker.tls.verify));
    hash_value(
        digest,
        broker.tls.server_name.as_deref().unwrap_or_default(),
    );
    hash_value(
        digest,
        broker
            .tls
            .ca_cert
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .as_deref()
            .unwrap_or_default(),
    );
    hash_value(
        digest,
        broker
            .tls
            .client_cert
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .as_deref()
            .unwrap_or_default(),
    );
    hash_value(
        digest,
        broker
            .tls
            .client_key
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .as_deref()
            .unwrap_or_default(),
    );
    digest.update(broker.heartbeat.as_secs().to_be_bytes());
    for endpoint in &broker.hosts {
        hash_value(digest, &endpoint.host);
        digest.update(endpoint.port.to_be_bytes());
    }
}

fn hash_publisher(digest: &mut Sha256, publisher: &PublisherConfigSection) {
    hash_value(digest, "publisher");
    hash_value(digest, safety_mode_name(publisher.safety));
    hash_value(
        digest,
        if publisher.confirms {
            "confirms"
        } else {
            "no_confirms"
        },
    );
    // `publisher.mandatory` is validated to `true` before fingerprinting, so a
    // dead value must not split pools.
    digest.update(publisher.confirm_timeout.as_millis().to_be_bytes());
}

fn hash_consumer(digest: &mut Sha256, consumer: &ConsumerConfigSection) {
    hash_value(digest, "consumer");
    digest.update(consumer.wait_timeout.as_millis().to_be_bytes());
    digest.update(consumer.max_attempts.unwrap_or(0).to_be_bytes());
}

fn hash_value(digest: &mut Sha256, value: &str) {
    digest.update(value.len().to_be_bytes());
    digest.update(value.as_bytes());
}

const fn tls_verify_name(verify: TlsVerify) -> &'static str {
    match verify {
        TlsVerify::Peer => "peer",
        TlsVerify::None => "none",
    }
}

const fn topology_mode_name(mode: TopologyMode) -> &'static str {
    match mode {
        TopologyMode::Declare => "declare",
        TopologyMode::Verify => "verify",
        TopologyMode::External => "external",
    }
}

const fn scheduler_name(strategy: SchedulerStrategy) -> &'static str {
    match strategy {
        SchedulerStrategy::WeightedFair => "weighted_fair",
    }
}

const fn delay_mode_name(mode: DelayMode) -> &'static str {
    match mode {
        DelayMode::Auto => "auto",
        DelayMode::Plugin => "plugin",
        DelayMode::Ttl => "ttl",
    }
}

const fn safety_mode_name(mode: SafetyMode) -> &'static str {
    match mode {
        SafetyMode::Blind => "blind",
        SafetyMode::Unsafe => "unsafe",
        SafetyMode::Safe => "safe",
    }
}

fn deserialize_duration_seconds<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    u64::deserialize(deserializer).map(Duration::from_secs)
}

fn deserialize_duration_seconds_opt<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<u64>::deserialize(deserializer).map(|seconds| seconds.map(Duration::from_secs))
}

fn deserialize_duration_millis<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    u64::deserialize(deserializer).map(Duration::from_millis)
}

fn deserialize_duration_seconds_vec<'de, D>(deserializer: D) -> Result<Vec<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<u64>::deserialize(deserializer)
        .map(|secs| secs.into_iter().map(Duration::from_secs).collect())
}

fn default_starvation_after() -> Duration {
    Duration::from_secs(30)
}

fn default_max_buffered_bytes() -> u64 {
    64 * 1024 * 1024
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use serde_json::json;

    use super::{
        BrokerConfig, Config, ConfigFingerprint, ConsumerConfigSection, Credentials, DelayConfig,
        Endpoint, PrefetchConfig, PublisherConfigSection, SafetyMode, SchedulerConfig,
        SchedulerStrategy, SubscriptionConfig, TlsConfig, TlsVerify, TopologyMode, ValidatedConfig,
        WorkerProfile,
    };
    use crate::transport::QueueKind;
    use crate::transport::lapin::connection_uri;

    fn broker(hosts: Vec<Endpoint>) -> BrokerConfig {
        BrokerConfig {
            name: "default".to_owned(),
            hosts,
            vhost: "/".to_owned(),
            credentials: Credentials::new("guest", "super-secret"),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        }
    }

    fn subscription(prefetch: u16) -> SubscriptionConfig {
        SubscriptionConfig {
            name: "default".to_owned(),
            broker: "default".to_owned(),
            queue: "jobs".to_owned(),
            weight: 1,
            priority_class: 0,
            prefetch: PrefetchConfig::Fixed(prefetch),
            starvation_after: Duration::from_secs(30),
            max_buffered_bytes: 64 * 1024 * 1024,
            early_ack: false,
            no_ack: false,
        }
    }

    fn worker(prefetch: u16) -> WorkerProfile {
        WorkerProfile {
            name: "main".to_owned(),
            subscriptions: vec![subscription(prefetch)],
            scheduler: SchedulerConfig::weighted_fair(),
        }
    }

    fn config(hosts: Vec<Endpoint>) -> Config {
        Config {
            brokers: vec![broker(hosts)],
            workers: vec![worker(16)],
            topology_mode: TopologyMode::Declare,
            delay: DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
            consumer: ConsumerConfigSection::default(),
            queue_type: QueueKind::Quorum,
            queue_durable: true,
        }
    }

    #[test]
    fn rejects_broker_without_host() {
        let error = config(Vec::new()).validate().unwrap_err();

        assert_eq!(error.path(), "brokers.default.hosts");
    }

    #[test]
    fn rejects_zero_prefetch() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.workers = vec![worker(0)];

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "workers.main.subscriptions.default.prefetch");
    }

    fn prefetch_candidate(prefetch: &serde_json::Value) -> Result<Config, serde_json::Error> {
        serde_json::from_value(json!({
            "brokers": [{
                "name": "default",
                "hosts": [{"host": "rabbit.local", "port": 5672}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {"enabled": false, "server_name": null},
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "default",
                    "broker": "default",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": prefetch
                }],
                "scheduler": {"strategy": "weighted_fair"}
            }],
            "topology_mode": "external"
        }))
    }

    #[test]
    fn parses_plain_integer_prefetch_as_fixed() {
        let candidate = prefetch_candidate(&json!(16)).expect("plain integer prefetch parses");

        assert!(matches!(
            candidate.workers[0].subscriptions[0].prefetch,
            PrefetchConfig::Fixed(16)
        ));
    }

    #[test]
    fn parses_fixed_union_prefetch() {
        let candidate = prefetch_candidate(&json!({"mode": "fixed", "value": 8}))
            .expect("fixed union prefetch parses");

        assert!(matches!(
            candidate.workers[0].subscriptions[0].prefetch,
            PrefetchConfig::Fixed(8)
        ));
    }

    #[test]
    fn parses_adaptive_union_prefetch() {
        let candidate = prefetch_candidate(&json!({
            "mode": "adaptive",
            "initial": 64,
            "min": 1,
            "max": 256,
            "target_buffer_seconds": 5
        }))
        .expect("adaptive union prefetch parses");

        assert_eq!(
            candidate.workers[0].subscriptions[0].prefetch,
            PrefetchConfig::Adaptive {
                initial: 64,
                min: 1,
                max: 256,
                target_buffer: Duration::from_secs(5),
            }
        );
    }

    #[test]
    fn rejects_unknown_prefetch_mode() {
        let result = prefetch_candidate(&json!({"mode": "dynamic", "value": 8}));

        assert!(result.is_err(), "unknown prefetch mode must fail to parse");
    }

    #[test]
    fn rejects_adaptive_union_missing_field() {
        let result = prefetch_candidate(&json!({"mode": "adaptive", "initial": 16, "max": 256}));

        assert!(
            result.is_err(),
            "adaptive union missing `min` must fail to parse"
        );
    }

    fn subscription_with(prefetch: PrefetchConfig) -> SubscriptionConfig {
        SubscriptionConfig {
            name: "default".to_owned(),
            broker: "default".to_owned(),
            queue: "jobs".to_owned(),
            weight: 1,
            priority_class: 0,
            prefetch,
            starvation_after: Duration::from_secs(30),
            max_buffered_bytes: 64 * 1024 * 1024,
            early_ack: false,
            no_ack: false,
        }
    }

    fn worker_with(prefetch: PrefetchConfig) -> WorkerProfile {
        WorkerProfile {
            name: "main".to_owned(),
            subscriptions: vec![subscription_with(prefetch)],
            scheduler: SchedulerConfig::weighted_fair(),
        }
    }

    fn config_with(prefetch: PrefetchConfig) -> Config {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.workers = vec![worker_with(prefetch)];
        candidate
    }

    fn adaptive(initial: u16, min: u16, max: u16, target_buffer: Duration) -> PrefetchConfig {
        PrefetchConfig::Adaptive {
            initial,
            min,
            max,
            target_buffer,
        }
    }

    #[test]
    fn rejects_adaptive_min_zero() {
        let error = config_with(adaptive(16, 0, 256, Duration::from_secs(5)))
            .validate()
            .unwrap_err();
        assert_eq!(
            error.path(),
            "workers.main.subscriptions.default.prefetch.min"
        );
    }

    #[test]
    fn rejects_adaptive_max_below_min() {
        let error = config_with(adaptive(16, 8, 4, Duration::from_secs(5)))
            .validate()
            .unwrap_err();
        assert_eq!(
            error.path(),
            "workers.main.subscriptions.default.prefetch.max"
        );
    }

    #[test]
    fn rejects_adaptive_initial_outside_bounds() {
        let error = config_with(adaptive(512, 1, 256, Duration::from_secs(5)))
            .validate()
            .unwrap_err();
        assert_eq!(
            error.path(),
            "workers.main.subscriptions.default.prefetch.initial"
        );
    }

    #[test]
    fn rejects_adaptive_zero_target_buffer() {
        let error = config_with(adaptive(16, 1, 256, Duration::ZERO))
            .validate()
            .unwrap_err();
        assert_eq!(
            error.path(),
            "workers.main.subscriptions.default.prefetch.target_buffer_seconds"
        );
    }

    #[test]
    fn rejects_adaptive_with_early_ack() {
        let mut candidate = config_with(adaptive(16, 1, 256, Duration::from_secs(5)));
        candidate.workers[0].subscriptions[0].early_ack = true;
        let error = candidate.validate().unwrap_err();
        assert_eq!(
            error.path(),
            "workers.main.subscriptions.default.prefetch.mode"
        );
        assert!(
            error.to_string().contains("acknowledgements"),
            "error must explain the acknowledgement requirement, got: {error}"
        );
    }

    #[test]
    fn rejects_adaptive_with_no_ack() {
        let mut candidate = config_with(adaptive(16, 1, 256, Duration::from_secs(5)));
        candidate.workers[0].subscriptions[0].no_ack = true;
        let error = candidate.validate().unwrap_err();
        assert_eq!(
            error.path(),
            "workers.main.subscriptions.default.prefetch.mode"
        );
    }

    #[test]
    fn accepts_valid_adaptive_prefetch() {
        config_with(adaptive(16, 1, 256, Duration::from_secs(5)))
            .validate()
            .expect("valid adaptive prefetch");
    }

    #[test]
    fn fingerprint_distinguishes_prefetch_policies() {
        let fixed = config_with(PrefetchConfig::Fixed(16)).validate().unwrap();
        let adaptive = config_with(adaptive(16, 1, 256, Duration::from_secs(5)))
            .validate()
            .unwrap();
        let other_fixed = config_with(PrefetchConfig::Fixed(32)).validate().unwrap();

        assert_ne!(fixed.fingerprint(), adaptive.fingerprint());
        assert_ne!(fixed.fingerprint(), other_fixed.fingerprint());
        assert_eq!(
            fixed.fingerprint(),
            config_with(PrefetchConfig::Fixed(16))
                .validate()
                .unwrap()
                .fingerprint()
        );
    }

    #[test]
    fn rejects_duplicate_subscription_names_within_a_worker() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        let mut duplicated = worker(16);
        duplicated.subscriptions.push(SubscriptionConfig {
            name: "default".to_owned(),
            broker: "default".to_owned(),
            queue: "jobs.other".to_owned(),
            ..subscription(16)
        });
        candidate.workers = vec![duplicated];

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "workers.main.subscriptions.default");
        assert!(
            error
                .to_string()
                .contains("subscription name must be unique within a worker profile")
        );
    }

    #[test]
    fn accepts_config_without_scheduler_max_in_flight() {
        let candidate = serde_json::from_value::<Config>(json!({
            "brokers": [{
                "name": "default",
                "hosts": [{"host": "rabbit.local", "port": 5672}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {"enabled": false},
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "default",
                    "broker": "default",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": 16
                }],
                "scheduler": {
                    "strategy": "weighted_fair"
                }
            }],
            "topology_mode": "external"
        }))
        .expect("scheduler.max_in_flight is optional");

        candidate
            .validate()
            .expect("config without scheduler.max_in_flight is valid");
    }

    #[test]
    fn ignores_scheduler_max_in_flight_value() {
        let candidate = serde_json::from_value::<Config>(json!({
            "brokers": [{
                "name": "default",
                "hosts": [{"host": "rabbit.local", "port": 5672}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {"enabled": false},
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "default",
                    "broker": "default",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": 16
                }],
                "scheduler": {
                    "strategy": "weighted_fair",
                    "max_in_flight": 8
                }
            }],
            "topology_mode": "external"
        }))
        .expect("scheduler.max_in_flight is deserialized but ignored");

        candidate
            .validate()
            .expect("max_in_flight below prefetch must not be validated");
    }

    #[test]
    fn rejects_zero_starvation_after_with_the_subscription_path() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        let mut profile = worker(16);
        profile.subscriptions[0].starvation_after = Duration::ZERO;
        candidate.workers = vec![profile];

        let error = candidate.validate().unwrap_err();

        assert_eq!(
            error.path(),
            "workers.main.subscriptions.default.starvation_after"
        );
    }

    #[test]
    fn accepts_scheduler_budget_at_the_canonical_path() {
        let candidate = serde_json::from_value::<Config>(json!({
            "brokers": [{
                "name": "default",
                "hosts": [{"host": "rabbit.local", "port": 5672}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {"enabled": false},
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "default",
                    "broker": "default",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": 16
                }],
                "scheduler": {
                    "strategy": "weighted_fair",
                    "max_in_flight": 64
                }
            }],
            "topology_mode": "external"
        }))
        .expect("scheduler.max_in_flight is canonical");

        let validated = candidate
            .validate()
            .expect("canonical worker configuration");
        let worker = validated.worker("main").expect("worker");
        assert_eq!(worker.scheduler.strategy, SchedulerStrategy::WeightedFair);
        assert_eq!(
            worker.subscriptions[0].starvation_after,
            Duration::from_secs(30)
        );
    }

    #[test]
    fn rejects_the_legacy_worker_budget_with_an_actionable_path() {
        let error = serde_json::from_value::<Config>(json!({
            "brokers": [{
                "name": "default",
                "hosts": [{"host": "rabbit.local", "port": 5672}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {"enabled": false},
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "default",
                    "broker": "default",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": 16
                }],
                "max_in_flight": 64,
                "scheduler": {
                    "strategy": "weighted_fair"
                }
            }],
            "topology_mode": "external"
        }))
        .expect_err("legacy worker-level max_in_flight must be rejected");

        assert!(error.to_string().contains("unknown field"));
        assert!(error.to_string().contains("max_in_flight"));
    }

    #[test]
    fn rejects_unknown_topology_mode() {
        let error = "automatic".parse::<TopologyMode>().unwrap_err();

        assert_eq!(error.path(), "topology.mode");
    }

    #[test]
    fn normalizes_host_order() {
        let validated = config(vec![
            Endpoint::new("rabbit-b.local", 5672),
            Endpoint::new("rabbit-a.local", 5672),
        ])
        .validate()
        .unwrap();

        let hosts = validated.broker("default").unwrap().hosts();

        assert_eq!(hosts[0].host(), "rabbit-a.local");
        assert_eq!(hosts[1].host(), "rabbit-b.local");
    }

    #[test]
    fn debug_output_masks_credentials() {
        let candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);

        let debug = format!("{candidate:?}");

        assert!(!debug.contains("super-secret"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn equivalent_configurations_have_the_same_fingerprint() {
        let first = config(vec![
            Endpoint::new("rabbit-b.local", 5672),
            Endpoint::new("rabbit-a.local", 5672),
        ])
        .validate()
        .unwrap();
        let second = config(vec![
            Endpoint::new("rabbit-a.local", 5672),
            Endpoint::new("rabbit-b.local", 5672),
        ])
        .validate()
        .unwrap();

        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn credentials_remain_part_of_the_internal_fingerprint() {
        let first = config(vec![Endpoint::new("rabbit.local", 5672)])
            .validate()
            .unwrap();
        let mut second = config(vec![Endpoint::new("rabbit.local", 5672)]);
        second.brokers[0].credentials = Credentials::new("guest", "different-secret");
        let second = second.validate().unwrap();

        assert_ne!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn starvation_is_part_of_the_fingerprint() {
        let mut base = config(vec![Endpoint::new("rabbit.local", 5672)]);
        base.workers = vec![worker(16)];
        let mut starvation_changed = base.clone();
        starvation_changed.workers[0].subscriptions[0].starvation_after = Duration::from_secs(31);

        let base = base.validate().unwrap();
        let starvation_changed = starvation_changed.validate().unwrap();

        assert_ne!(base.fingerprint(), starvation_changed.fingerprint());
    }

    #[test]
    fn retains_validated_worker_profiles() {
        let validated = config(vec![Endpoint::new("rabbit.local", 5672)])
            .validate()
            .unwrap();

        let worker = validated.worker("main").unwrap();
        assert_eq!(worker.name, "main");
        assert_eq!(worker.subscriptions.len(), 1);
    }

    #[test]
    fn retains_validated_topology_mode() {
        let validated = config(vec![Endpoint::new("rabbit.local", 5672)])
            .validate()
            .unwrap();

        assert_eq!(validated.topology_mode(), TopologyMode::Declare);
    }

    #[test]
    fn rejects_zero_subscription_weight() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.workers[0].subscriptions[0].weight = 0;

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "workers.main.subscriptions.default.weight");
    }

    #[test]
    fn rejects_subscription_with_unknown_broker() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.workers[0].subscriptions[0].broker = "missing".to_owned();

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "workers.main.subscriptions.default.broker");
    }

    #[test]
    fn rejects_no_ack_without_early_ack() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.workers[0].subscriptions[0].no_ack = true;

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "workers.main.subscriptions.default.no_ack");
        assert!(
            error.to_string().contains("no_ack requires early_ack"),
            "error must explain the required combination, got: {error}"
        );
    }

    #[test]
    fn accepts_no_ack_with_early_ack() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.workers[0].subscriptions[0].early_ack = true;
        candidate.workers[0].subscriptions[0].no_ack = true;

        candidate
            .validate()
            .expect("no_ack with early_ack is the documented opt-in combination");
    }

    #[test]
    fn rejects_worker_without_subscriptions() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.workers[0].subscriptions.clear();

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "workers.main.subscriptions");
    }

    #[test]
    fn publisher_section_defaults_to_safe_values() {
        let validated = config(vec![Endpoint::new("rabbit.local", 5672)])
            .validate()
            .unwrap();

        let publisher = validated.publisher();
        assert!(publisher.confirms);
        assert!(publisher.mandatory);
        assert_eq!(publisher.confirm_timeout, Duration::from_secs(30));
    }

    #[test]
    fn consumer_section_defaults_to_thirty_seconds() {
        let validated = config(vec![Endpoint::new("rabbit.local", 5672)])
            .validate()
            .unwrap();

        assert_eq!(validated.consumer().wait_timeout, Duration::from_secs(30));
    }

    #[test]
    fn deserializes_consumer_wait_timeout_from_milliseconds() {
        let candidate = serde_json::from_value::<Config>(json!({
            "brokers": [{
                "name": "default",
                "hosts": [{"host": "rabbit.local", "port": 5672}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {"enabled": false},
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "default",
                    "broker": "default",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": 16
                }],
                "scheduler": {
                    "strategy": "weighted_fair",
                    "max_in_flight": 64
                }
            }],
            "topology_mode": "external",
            "consumer": {
                "wait_timeout": 5000
            }
        }))
        .expect("consumer section deserializes");

        let validated = candidate.validate().expect("valid config");
        assert_eq!(validated.consumer().wait_timeout, Duration::from_secs(5));
    }

    #[test]
    fn rejects_consumer_wait_timeout_below_one_second() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.consumer.wait_timeout = Duration::from_millis(999);

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "consumer.wait_timeout");
    }

    #[test]
    fn rejects_consumer_wait_timeout_above_twenty_four_hours() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.consumer.wait_timeout = Duration::from_secs(24 * 60 * 60 + 1);

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "consumer.wait_timeout");
    }

    #[test]
    fn consumer_section_is_part_of_the_fingerprint() {
        let base = config(vec![Endpoint::new("rabbit.local", 5672)])
            .validate()
            .unwrap();
        let mut changed = config(vec![Endpoint::new("rabbit.local", 5672)]);
        changed.consumer.wait_timeout = Duration::from_secs(31);
        let changed = changed.validate().unwrap();

        assert_ne!(base.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn consumer_max_attempts_defaults_to_twenty() {
        let validated = config(vec![Endpoint::new("rabbit.local", 5672)])
            .validate()
            .unwrap();

        assert_eq!(validated.consumer().max_attempts, Some(20));
    }

    #[test]
    fn deserializes_consumer_max_attempts_from_native_config() {
        let candidate = serde_json::from_value::<Config>(json!({
            "brokers": [{
                "name": "default",
                "hosts": [{"host": "rabbit.local", "port": 5672}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {"enabled": false},
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "default",
                    "broker": "default",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": 16
                }],
                "scheduler": {
                    "strategy": "weighted_fair",
                    "max_in_flight": 64
                }
            }],
            "topology_mode": "external",
            "consumer": {
                "max_attempts": 30
            }
        }))
        .expect("consumer section deserializes");

        let validated = candidate.validate().expect("valid config");
        assert_eq!(validated.consumer().max_attempts, Some(30));
    }

    #[test]
    fn rejects_zero_consumer_max_attempts() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.consumer.max_attempts = Some(0);

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "consumer.max_attempts");
    }

    #[test]
    fn consumer_max_attempts_is_part_of_the_fingerprint() {
        let base = config(vec![Endpoint::new("rabbit.local", 5672)])
            .validate()
            .unwrap();
        let mut changed = config(vec![Endpoint::new("rabbit.local", 5672)]);
        changed.consumer.max_attempts = Some(30);
        let changed = changed.validate().unwrap();

        assert_ne!(base.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn rejects_deprecated_mandatory_false_with_safety_guidance() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.publisher.mandatory = false;

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "publisher.mandatory");
        assert!(
            error.to_string().contains("publisher.safety"),
            "error must point at the safety replacement, got: {error}"
        );
    }

    #[test]
    fn rejects_zero_confirm_timeout() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.publisher.confirm_timeout = Duration::ZERO;

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "publisher.confirm_timeout");
    }

    #[test]
    fn accepts_confirm_timeout_of_one_second() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.publisher.confirm_timeout = Duration::from_secs(1);

        candidate
            .validate()
            .expect("confirm_timeout at the 1s lower bound is valid");
    }

    #[test]
    fn rejects_zero_heartbeat_with_the_broker_path() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.brokers[0].heartbeat = Duration::ZERO;

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "brokers.default.heartbeat");
    }

    #[test]
    fn rejects_heartbeat_above_the_amqp_wire_limit() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.brokers[0].heartbeat = Duration::from_secs(u64::from(u16::MAX) + 1);

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "brokers.default.heartbeat");
    }

    #[test]
    fn accepts_heartbeat_at_the_amqp_wire_limit() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.brokers[0].heartbeat = Duration::from_secs(u64::from(u16::MAX));

        candidate
            .validate()
            .expect("65535s is the AMQP wire limit and must be accepted");
    }

    #[test]
    fn mandatory_no_longer_splits_the_fingerprint() {
        let base = config(vec![Endpoint::new("rabbit.local", 5672)]);
        let mut changed = config(vec![Endpoint::new("rabbit.local", 5672)]);
        changed.publisher.mandatory = false;

        assert_eq!(
            ConfigFingerprint::calculate(&base),
            ConfigFingerprint::calculate(&changed),
            "the deprecated mandatory flag must not influence the fingerprint"
        );
    }

    #[test]
    fn deserializes_publisher_section_from_milliseconds() {
        let candidate = serde_json::from_value::<Config>(json!({
            "brokers": [{
                "name": "default",
                "hosts": [{"host": "rabbit.local", "port": 5672}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {"enabled": false},
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "default",
                    "broker": "default",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": 16
                }],
                "scheduler": {
                    "strategy": "weighted_fair",
                    "max_in_flight": 64
                }
            }],
            "topology_mode": "external",
            "publisher": {
                "confirms": false,
                "mandatory": true,
                "confirm_timeout": 5000
            }
        }))
        .expect("publisher section deserializes");

        let validated = candidate.validate().expect("valid config");
        let publisher = validated.publisher();
        assert!(!publisher.confirms);
        assert!(publisher.mandatory);
        assert_eq!(publisher.confirm_timeout, Duration::from_secs(5));
    }

    #[test]
    fn publisher_section_omitted_uses_defaults() {
        let candidate = serde_json::from_value::<Config>(json!({
            "brokers": [{
                "name": "default",
                "hosts": [{"host": "rabbit.local", "port": 5672}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {"enabled": false},
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "default",
                    "broker": "default",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": 16
                }],
                "scheduler": {
                    "strategy": "weighted_fair",
                    "max_in_flight": 64
                }
            }],
            "topology_mode": "external"
        }))
        .expect("config without publisher section");

        let validated = candidate.validate().expect("valid config");
        let publisher = validated.publisher();
        assert!(publisher.confirms);
        assert!(publisher.mandatory);
        assert_eq!(publisher.confirm_timeout, Duration::from_secs(30));
    }

    #[test]
    fn publisher_section_is_part_of_the_fingerprint() {
        let base = config(vec![Endpoint::new("rabbit.local", 5672)])
            .validate()
            .unwrap();
        let mut changed = config(vec![Endpoint::new("rabbit.local", 5672)]);
        changed.publisher.confirms = false;
        let changed = changed.validate().unwrap();

        assert_ne!(base.fingerprint(), changed.fingerprint());
    }

    fn broker_with_tls(tls: TlsConfig) -> BrokerConfig {
        BrokerConfig {
            name: "primary".to_owned(),
            hosts: vec![Endpoint::new("rabbit.example.com", 5671)],
            vhost: "/".to_owned(),
            credentials: Credentials::new("guest", "guest"),
            tls,
            heartbeat: Duration::from_secs(30),
        }
    }

    fn config_with_broker_tls(tls: TlsConfig) -> Config {
        Config {
            brokers: vec![broker_with_tls(tls)],
            workers: vec![WorkerProfile {
                name: "main".to_owned(),
                subscriptions: vec![SubscriptionConfig {
                    name: "jobs".to_owned(),
                    broker: "primary".to_owned(),
                    ..subscription(8)
                }],
                scheduler: SchedulerConfig::weighted_fair(),
            }],
            topology_mode: TopologyMode::Declare,
            delay: DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
            consumer: ConsumerConfigSection::default(),
            queue_type: QueueKind::Quorum,
            queue_durable: true,
        }
    }

    #[test]
    fn tls_enabled_uses_amqps_scheme() {
        let enabled: TlsConfig =
            serde_json::from_value(serde_json::json!({"enabled": true})).expect("valid TLS config");
        let broker = broker_with_tls(enabled);
        let uri = connection_uri(&broker, &broker.hosts()[0]).expect("valid URI");
        assert_eq!(uri.scheme(), "amqps");
    }

    #[test]
    fn tls_disabled_uses_amqp_scheme() {
        let broker = broker_with_tls(TlsConfig::disabled());
        let uri = connection_uri(&broker, &broker.hosts()[0]).expect("valid URI");
        assert_eq!(uri.scheme(), "amqp");
    }

    #[test]
    fn tls_config_deserializes_ca_and_client_certs() {
        let tls: TlsConfig = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "ca_cert": "/etc/ssl/certs/ca.pem",
            "client_cert": "/etc/ssl/client/cert.pem",
            "client_key": "/etc/ssl/client/key.pem"
        }))
        .expect("valid TLS config");

        assert!(tls.is_enabled());
        assert_eq!(tls.ca_cert(), Some(&PathBuf::from("/etc/ssl/certs/ca.pem")));
        assert_eq!(
            tls.client_cert(),
            Some(&PathBuf::from("/etc/ssl/client/cert.pem"))
        );
        assert_eq!(
            tls.client_key(),
            Some(&PathBuf::from("/etc/ssl/client/key.pem"))
        );
    }

    #[test]
    fn tls_config_without_certs_defaults_to_none() {
        let tls: TlsConfig = serde_json::from_value(serde_json::json!({
            "enabled": true
        }))
        .expect("valid TLS config");

        assert!(tls.ca_cert().is_none());
        assert!(tls.client_cert().is_none());
        assert!(tls.client_key().is_none());
    }

    #[test]
    fn tls_changes_affect_config_fingerprint() {
        let validated_with = serde_json::from_value::<Config>(serde_json::json!({
            "brokers": [{
                "name": "primary",
                "hosts": [{"host": "rabbit.example.com", "port": 5671}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {
                    "enabled": true,
                    "ca_cert": "/etc/ssl/certs/ca.pem"
                },
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "jobs",
                    "broker": "primary",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": 8,
                    "starvation_after": 30
                }],
                "scheduler": {"strategy": "weighted_fair", "max_in_flight": 16}
            }],
            "topology_mode": "declare"
        }))
        .expect("valid config with CA")
        .validate()
        .expect("valid");

        let validated_without = serde_json::from_value::<Config>(serde_json::json!({
            "brokers": [{
                "name": "primary",
                "hosts": [{"host": "rabbit.example.com", "port": 5671}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {
                    "enabled": true
                },
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "jobs",
                    "broker": "primary",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": 8,
                    "starvation_after": 30
                }],
                "scheduler": {"strategy": "weighted_fair", "max_in_flight": 16}
            }],
            "topology_mode": "declare"
        }))
        .expect("valid config without CA")
        .validate()
        .expect("valid");

        assert_ne!(
            validated_with.fingerprint(),
            validated_without.fingerprint(),
            "different TLS CA cert paths must produce different fingerprints"
        );
    }

    #[test]
    fn tls_verify_none_is_rejected_at_validation() {
        let tls: TlsConfig = serde_json::from_value(json!({
            "enabled": true,
            "verify": "none"
        }))
        .expect("valid TLS config");
        let config = config_with_broker_tls(tls);

        let error = config.validate().expect_err("verify none must be rejected");

        assert_eq!(error.path(), "brokers.primary.tls.verify");
        assert!(
            error.to_string().contains("custom TLS connector"),
            "error must explain the capability gap: {error}"
        );
    }

    #[test]
    fn tls_server_name_mismatch_is_rejected_at_validation() {
        let tls: TlsConfig = serde_json::from_value(json!({
            "enabled": true,
            "server_name": "other.example.com"
        }))
        .expect("valid TLS config");
        let config = config_with_broker_tls(tls);

        let error = config
            .validate()
            .expect_err("SNI mismatch must be rejected");

        assert_eq!(error.path(), "brokers.primary.tls.server_name");
        assert!(
            error.to_string().contains("rabbit.example.com"),
            "error must name the supported server name: {error}"
        );
    }

    #[test]
    fn tls_server_name_matching_first_host_is_accepted() {
        let tls: TlsConfig = serde_json::from_value(json!({
            "enabled": true,
            "server_name": "rabbit.example.com"
        }))
        .expect("valid TLS config");
        let config = config_with_broker_tls(tls);

        config.validate().expect("matching server name is valid");
    }

    #[test]
    fn tls_verify_defaults_to_peer() {
        let tls: TlsConfig =
            serde_json::from_value(json!({"enabled": true})).expect("valid TLS config");

        assert_eq!(tls.verify(), TlsVerify::Peer);
        assert_eq!(tls.server_name(), None);
    }

    #[test]
    fn tls_verify_and_server_name_changes_affect_fingerprint() {
        let base: TlsConfig =
            serde_json::from_value(json!({"enabled": true})).expect("valid TLS config");
        let renamed: TlsConfig = serde_json::from_value(json!({
            "enabled": true,
            "server_name": "rabbit.example.com"
        }))
        .expect("valid TLS config");

        let base = config_with_broker_tls(base).validate().expect("valid");
        let renamed = config_with_broker_tls(renamed).validate().expect("valid");

        assert_ne!(base.fingerprint(), renamed.fingerprint());
    }

    #[test]
    fn safety_mode_defaults_to_safe() {
        assert_eq!(SafetyMode::default(), SafetyMode::Safe);
    }

    #[test]
    fn publisher_section_defaults_safety_to_safe() {
        let publisher = PublisherConfigSection::default();
        assert_eq!(publisher.safety, SafetyMode::Safe);
    }

    #[test]
    fn effective_safety_returns_explicit_non_safe_mode() {
        let publisher = PublisherConfigSection {
            safety: SafetyMode::Blind,
            ..PublisherConfigSection::default()
        };
        assert_eq!(publisher.effective_safety(), SafetyMode::Blind);

        let publisher = PublisherConfigSection {
            safety: SafetyMode::Unsafe,
            ..PublisherConfigSection::default()
        };
        assert_eq!(publisher.effective_safety(), SafetyMode::Unsafe);
    }

    #[test]
    fn effective_safety_derives_from_legacy_confirms_when_safe() {
        let publisher = PublisherConfigSection {
            confirms: false,
            ..PublisherConfigSection::default()
        };
        assert_eq!(publisher.effective_safety(), SafetyMode::Unsafe);

        let publisher = PublisherConfigSection {
            confirms: true,
            ..PublisherConfigSection::default()
        };
        assert_eq!(publisher.effective_safety(), SafetyMode::Safe);
    }

    #[test]
    fn deserializes_safety_blind() {
        let candidate = serde_json::from_value::<Config>(serde_json::json!({
            "brokers": [{
                "name": "default",
                "hosts": [{"host": "rabbit.local", "port": 5672}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "secret"},
                "tls": {"enabled": false},
                "heartbeat": 30
            }],
            "workers": [{
                "name": "main",
                "subscriptions": [{
                    "name": "default",
                    "broker": "default",
                    "queue": "jobs",
                    "weight": 1,
                    "priority_class": 0,
                    "prefetch": 16
                }],
                "scheduler": {
                    "strategy": "weighted_fair",
                    "max_in_flight": 64
                }
            }],
            "topology_mode": "external",
            "publisher": {
                "safety": "blind",
                "confirm_timeout": 5000
            }
        }))
        .expect("publisher section with safety=blind deserializes");

        let validated = candidate.validate().expect("valid config");
        let publisher = validated.publisher();
        assert_eq!(publisher.safety, SafetyMode::Blind);
        assert_eq!(publisher.effective_safety(), SafetyMode::Blind);
    }

    fn broker_config(name: &str) -> BrokerConfig {
        BrokerConfig {
            name: name.to_owned(),
            hosts: vec![Endpoint::new("rabbit.local", 5672)],
            vhost: "/".to_owned(),
            credentials: Credentials::new("guest", "super-secret"),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        }
    }

    fn single_broker_config() -> ValidatedConfig {
        Config {
            brokers: vec![broker_config("main")],
            workers: vec![],
            topology_mode: TopologyMode::Declare,
            delay: DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
            consumer: ConsumerConfigSection::default(),
            queue_type: QueueKind::Quorum,
            queue_durable: true,
        }
        .validate()
        .expect("valid config")
    }

    #[test]
    fn synthesizes_default_profile_for_auto_name() {
        let config = single_broker_config();

        let worker = config
            .synthesize_auto_profile("__auto__.emails")
            .expect("synthesis succeeds");

        assert_eq!(worker.name, "__auto__.emails");
        assert_eq!(worker.scheduler, SchedulerConfig::weighted_fair());
        let [subscription] = worker.subscriptions.as_slice() else {
            panic!("expected exactly one subscription");
        };
        assert_eq!(subscription.name, "auto");
        assert_eq!(subscription.queue, "emails");
        assert_eq!(subscription.broker, "main");
        assert_eq!(subscription.weight, 1);
        assert_eq!(subscription.priority_class, 0);
        assert_eq!(subscription.prefetch, PrefetchConfig::Fixed(64));
        assert_eq!(subscription.starvation_after, Duration::from_secs(30));
        assert!(!subscription.early_ack);
        assert!(!subscription.no_ack);
    }

    #[test]
    fn synthesizes_only_for_auto_prefix() {
        let config = single_broker_config();

        let error = config
            .synthesize_auto_profile("orders")
            .expect_err("plain names are not synthesizable");

        assert_eq!(error.to_string(), "workers.orders: unknown worker profile");
    }

    #[test]
    fn synthesizes_rejects_empty_queue_part() {
        let config = single_broker_config();

        let error = config
            .synthesize_auto_profile("__auto__.")
            .expect_err("empty queue part must be rejected");

        assert!(error.to_string().contains("must carry a queue"));
    }

    #[test]
    fn synthesizes_rejects_multi_broker() {
        let config = Config {
            brokers: vec![broker_config("one"), broker_config("two")],
            workers: vec![],
            topology_mode: TopologyMode::Declare,
            delay: DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
            consumer: ConsumerConfigSection::default(),
            queue_type: QueueKind::Quorum,
            queue_durable: true,
        }
        .validate()
        .expect("valid config");

        let error = config
            .synthesize_auto_profile("__auto__.emails")
            .expect_err("multi-broker synthesis must fail");

        assert!(error.to_string().contains("single configured broker"));
    }
}
