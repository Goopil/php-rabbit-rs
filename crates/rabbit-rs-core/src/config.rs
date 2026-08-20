use std::{collections::HashSet, fmt, path::PathBuf, str::FromStr, time::Duration};

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};

use crate::error::ConfigError;

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

/// TLS verification mode controlling certificate validation.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TlsVerify {
    /// Verify the server certificate chain against the configured CA.
    #[default]
    Peer,
    /// Skip certificate verification (insecure — use only in development).
    None,
}

/// TLS parameters that are safe to retain in normalized configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct TlsConfig {
    enabled: bool,
    server_name: Option<String>,
    ca_cert: Option<PathBuf>,
    client_cert: Option<PathBuf>,
    client_key: Option<PathBuf>,
    verify: TlsVerify,
}

impl TlsConfig {
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            server_name: None,
            ca_cert: None,
            client_cert: None,
            client_key: None,
            verify: TlsVerify::Peer,
        }
    }

    #[must_use]
    pub const fn enabled() -> Self {
        Self {
            enabled: true,
            server_name: None,
            ca_cert: None,
            client_cert: None,
            client_key: None,
            verify: TlsVerify::Peer,
        }
    }

    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn server_name(&self) -> Option<&str> {
        self.server_name.as_deref()
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
    pub fn with_server_name(mut self, server_name: &str) -> Self {
        self.server_name = Some(server_name.to_owned());
        self
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

    /// Returns the SNI server name to use for TLS connections.
    ///
    /// Falls back to the first broker host when `tls.server_name` is not set.
    ///
    /// # Panics
    ///
    /// Panics if the broker has no hosts. Validation guarantees at least one host.
    #[must_use]
    pub fn effective_server_name(&self) -> &str {
        self.tls
            .server_name
            .as_deref()
            .unwrap_or_else(|| self.hosts.first().expect("at least one host").host())
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
    pub prefetch: u16,
    #[serde(
        default = "default_starvation_after",
        deserialize_with = "deserialize_duration_seconds"
    )]
    pub starvation_after: Duration,
}

/// Scheduler algorithms supported by the stable configuration format.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerStrategy {
    WeightedFair,
}

/// Worker-level scheduler parameters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SchedulerConfig {
    pub strategy: SchedulerStrategy,
    pub max_in_flight: u16,
}

impl SchedulerConfig {
    #[must_use]
    pub const fn weighted_fair(max_in_flight: u16) -> Self {
        Self {
            strategy: SchedulerStrategy::WeightedFair,
            max_in_flight,
        }
    }
}

/// A set of subscriptions consumed by one Laravel worker profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerProfile {
    pub name: String,
    pub subscriptions: Vec<SubscriptionConfig>,
    pub scheduler: SchedulerConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerProfileWire {
    name: String,
    subscriptions: Vec<SubscriptionConfig>,
    scheduler: SchedulerConfigWire,
    #[serde(default)]
    max_in_flight: Option<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SchedulerConfigWire {
    strategy: SchedulerStrategy,
    #[serde(default)]
    max_in_flight: Option<u16>,
}

impl<'de> Deserialize<'de> for WorkerProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WorkerProfileWire::deserialize(deserializer)?;
        if wire.max_in_flight.is_some() {
            return Err(serde::de::Error::custom(format!(
                "workers.{}.max_in_flight moved to workers.{}.scheduler.max_in_flight",
                wire.name, wire.name
            )));
        }
        let max_in_flight = wire.scheduler.max_in_flight.ok_or_else(|| {
            serde::de::Error::custom(format!(
                "workers.{}.scheduler.max_in_flight is required",
                wire.name
            ))
        })?;

        Ok(Self {
            name: wire.name,
            subscriptions: wire.subscriptions,
            scheduler: SchedulerConfig {
                strategy: wire.scheduler.strategy,
                max_in_flight,
            },
        })
    }
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
    #[serde(deserialize_with = "deserialize_duration_seconds")]
    pub detection_timeout: Duration,
}

impl DelayConfig {
    #[must_use]
    pub const fn new(
        mode: DelayMode,
        buckets: Vec<Duration>,
        max_buckets: usize,
        queue_expiry_margin: Duration,
        detection_timeout: Duration,
    ) -> Self {
        Self {
            mode,
            buckets,
            max_buckets,
            queue_expiry_margin,
            detection_timeout,
        }
    }
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
            detection_timeout: Duration::from_secs(5),
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
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SafetyMode {
    /// Fire-and-forget: async pump, no socket wait, no confirms. Messages
    /// may be lost if the socket drops between pump send and TCP write.
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
    /// Deprecated: use `safety = "safe"` instead. Defaults to true for backward compat.
    pub mandatory: bool,
    #[serde(deserialize_with = "deserialize_duration_millis")]
    pub confirm_timeout: Duration,
}

impl PublisherConfigSection {
    #[must_use]
    pub const fn new(confirms: bool, mandatory: bool, confirm_timeout: Duration) -> Self {
        Self {
            safety: SafetyMode::Safe,
            confirms,
            mandatory,
            confirm_timeout,
        }
    }

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

            broker.hosts.sort_unstable();
        }
        self.brokers
            .sort_unstable_by(|left, right| left.name.cmp(&right.name));
        let broker_names: HashSet<_> = self
            .brokers
            .iter()
            .map(|broker| broker.name.as_str())
            .collect();

        for worker in &mut self.workers {
            if worker.subscriptions.is_empty() {
                return Err(ConfigError::new(
                    format!("workers.{}.subscriptions", worker.name),
                    "at least one subscription is required",
                ));
            }
            for subscription in &worker.subscriptions {
                if !broker_names.contains(subscription.broker.as_str()) {
                    return Err(ConfigError::new(
                        format!(
                            "workers.{}.subscriptions.{}.broker",
                            worker.name, subscription.name
                        ),
                        "references an unknown broker",
                    ));
                }
                if subscription.weight == 0 {
                    return Err(ConfigError::new(
                        format!(
                            "workers.{}.subscriptions.{}.weight",
                            worker.name, subscription.name
                        ),
                        "weight must be greater than zero",
                    ));
                }
                if subscription.prefetch == 0 {
                    return Err(ConfigError::new(
                        format!(
                            "workers.{}.subscriptions.{}.prefetch",
                            worker.name, subscription.name
                        ),
                        "prefetch must be greater than zero",
                    ));
                }

                if subscription.starvation_after.is_zero() {
                    return Err(ConfigError::new(
                        format!(
                            "workers.{}.subscriptions.{}.starvation_after",
                            worker.name, subscription.name
                        ),
                        "starvation_after must be greater than zero",
                    ));
                }

                if worker.scheduler.max_in_flight < subscription.prefetch {
                    return Err(ConfigError::new(
                        format!("workers.{}.scheduler.max_in_flight", worker.name),
                        "max_in_flight must be at least every subscription prefetch",
                    ));
                }
            }

            worker
                .subscriptions
                .sort_unstable_by(|left, right| left.name.cmp(&right.name));
        }
        self.workers
            .sort_unstable_by(|left, right| left.name.cmp(&right.name));

        Self::validate_delay(&self.delay)?;

        let fingerprint = ConfigFingerprint::calculate(&self);

        Ok(ValidatedConfig {
            brokers: self.brokers,
            workers: self.workers,
            topology_mode: self.topology_mode,
            delay: self.delay,
            dead_letter: self.dead_letter,
            delivery_limit: self.delivery_limit,
            publisher: self.publisher,
            fingerprint,
        })
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
        if delay.detection_timeout.is_zero() {
            return Err(ConfigError::new(
                "delay.detection_timeout",
                "detection_timeout must be greater than zero",
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
    fingerprint: ConfigFingerprint,
}

impl ValidatedConfig {
    #[must_use]
    pub fn broker(&self, name: &str) -> Option<&BrokerConfig> {
        self.brokers.iter().find(|broker| broker.name == name)
    }

    #[must_use]
    pub fn worker(&self, name: &str) -> Option<&WorkerProfile> {
        self.workers.iter().find(|worker| worker.name == name)
    }

    /// Returns all broker configurations in canonical order.
    #[must_use]
    pub fn brokers(&self) -> &[BrokerConfig] {
        &self.brokers
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
            digest.update(worker.scheduler.max_in_flight.to_be_bytes());
            for subscription in &worker.subscriptions {
                hash_value(&mut digest, &subscription.name);
                hash_value(&mut digest, &subscription.broker);
                hash_value(&mut digest, &subscription.queue);
                digest.update(subscription.weight.to_be_bytes());
                digest.update(subscription.priority_class.to_be_bytes());
                digest.update(subscription.prefetch.to_be_bytes());
                digest.update(subscription.starvation_after.as_secs().to_be_bytes());
            }
        }

        hash_value(&mut digest, delay_mode_name(config.delay.mode));
        hash_value(&mut digest, &format!("{:?}", config.delay.buckets));
        digest.update(config.delay.max_buckets.to_be_bytes());
        digest.update(config.delay.queue_expiry_margin.as_secs().to_be_bytes());
        digest.update(config.delay.detection_timeout.as_secs().to_be_bytes());

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

        Self(digest.finalize().into())
    }
}

fn hash_broker(digest: &mut Sha256, broker: &BrokerConfig) {
    hash_value(digest, &broker.name);
    hash_value(digest, &broker.vhost);
    hash_value(digest, &broker.credentials.username);
    hash_value(digest, broker.credentials.password.expose_secret());
    hash_value(digest, if broker.tls.enabled { "tls" } else { "plain" });
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
    hash_value(digest, tls_verify_name(broker.tls.verify));
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
    hash_value(
        digest,
        if publisher.mandatory {
            "mandatory"
        } else {
            "no_mandatory"
        },
    );
    digest.update(publisher.confirm_timeout.as_millis().to_be_bytes());
}

fn hash_value(digest: &mut Sha256, value: &str) {
    digest.update(value.len().to_be_bytes());
    digest.update(value.as_bytes());
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

const fn tls_verify_name(verify: TlsVerify) -> &'static str {
    match verify {
        TlsVerify::Peer => "peer",
        TlsVerify::None => "none",
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{
        BrokerConfig, Config, Credentials, DelayConfig, Endpoint, PublisherConfigSection,
        SafetyMode, SchedulerConfig, SubscriptionConfig, TlsConfig, TopologyMode, WorkerProfile,
    };

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
            prefetch,
            starvation_after: Duration::from_secs(30),
        }
    }

    fn worker(prefetch: u16, max_in_flight: u16) -> WorkerProfile {
        WorkerProfile {
            name: "main".to_owned(),
            subscriptions: vec![subscription(prefetch)],
            scheduler: SchedulerConfig::weighted_fair(max_in_flight),
        }
    }

    fn config(hosts: Vec<Endpoint>) -> Config {
        Config {
            brokers: vec![broker(hosts)],
            workers: vec![worker(16, 64)],
            topology_mode: TopologyMode::Declare,
            delay: DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
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
        candidate.workers = vec![worker(0, 64)];

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "workers.main.subscriptions.default.prefetch");
    }

    #[test]
    fn rejects_worker_budget_below_subscription_prefetch() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.workers = vec![worker(16, 8)];

        let error = candidate.validate().unwrap_err();

        assert_eq!(error.path(), "workers.main.scheduler.max_in_flight");
    }

    #[test]
    fn rejects_zero_starvation_after_with_the_subscription_path() {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        let mut profile = worker(16, 64);
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
        assert_eq!(worker.scheduler.max_in_flight, 64);
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

        assert!(error.to_string().contains("workers.main.max_in_flight"));
        assert!(
            error
                .to_string()
                .contains("workers.main.scheduler.max_in_flight")
        );
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
    fn scheduler_budget_and_starvation_are_part_of_the_fingerprint() {
        let mut base = config(vec![Endpoint::new("rabbit.local", 5672)]);
        base.workers = vec![worker(16, 64)];
        let mut scheduler_changed = base.clone();
        scheduler_changed.workers[0].scheduler.max_in_flight = 65;
        let mut starvation_changed = base.clone();
        starvation_changed.workers[0].subscriptions[0].starvation_after = Duration::from_secs(31);

        let base = base.validate().unwrap();
        let scheduler_changed = scheduler_changed.validate().unwrap();
        let starvation_changed = starvation_changed.validate().unwrap();

        assert_ne!(base.fingerprint(), scheduler_changed.fingerprint());
        assert_ne!(base.fingerprint(), starvation_changed.fingerprint());
    }

    #[test]
    fn retains_validated_worker_profiles() {
        let validated = config(vec![Endpoint::new("rabbit.local", 5672)])
            .validate()
            .unwrap();

        assert_eq!(
            validated.worker("main").unwrap().scheduler.max_in_flight,
            64
        );
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
    fn deserializes_publisher_section_from_milliseconds() {
        let candidate = serde_json::from_value::<Config>(json!({
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
                "mandatory": false,
                "confirm_timeout": 5000
            }
        }))
        .expect("publisher section deserializes");

        let validated = candidate.validate().expect("valid config");
        let publisher = validated.publisher();
        assert!(!publisher.confirms);
        assert!(!publisher.mandatory);
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
        let candidate = serde_json::from_value::<Config>(json!({
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

    #[test]
    fn deserializes_safety_unsafe() {
        let candidate = serde_json::from_value::<Config>(json!({
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
                    "prefetch": 16
                }],
                "scheduler": {
                    "strategy": "weighted_fair",
                    "max_in_flight": 64
                }
            }],
            "topology_mode": "external",
            "publisher": {
                "safety": "unsafe"
            }
        }))
        .expect("publisher section with safety=unsafe deserializes");

        let validated = candidate.validate().expect("valid config");
        let publisher = validated.publisher();
        assert_eq!(publisher.safety, SafetyMode::Unsafe);
        assert_eq!(publisher.effective_safety(), SafetyMode::Unsafe);
    }

    #[test]
    fn deserializes_safety_safe_explicit() {
        let candidate = serde_json::from_value::<Config>(json!({
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
                    "prefetch": 16
                }],
                "scheduler": {
                    "strategy": "weighted_fair",
                    "max_in_flight": 64
                }
            }],
            "topology_mode": "external",
            "publisher": {
                "safety": "safe"
            }
        }))
        .expect("publisher section with safety=safe deserializes");

        let validated = candidate.validate().expect("valid config");
        let publisher = validated.publisher();
        assert_eq!(publisher.safety, SafetyMode::Safe);
        assert_eq!(publisher.effective_safety(), SafetyMode::Safe);
    }

    #[test]
    fn safety_mode_is_part_of_the_fingerprint() {
        let base = config(vec![Endpoint::new("rabbit.local", 5672)])
            .validate()
            .unwrap();
        let mut changed = config(vec![Endpoint::new("rabbit.local", 5672)]);
        changed.publisher.safety = SafetyMode::Blind;
        let changed = changed.validate().unwrap();

        assert_ne!(base.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn legacy_confirms_false_still_works_with_default_safety() {
        let candidate = serde_json::from_value::<Config>(json!({
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
                "mandatory": false
            }
        }))
        .expect("legacy publisher section deserializes");

        let validated = candidate.validate().expect("valid config");
        let publisher = validated.publisher();
        assert_eq!(publisher.safety, SafetyMode::Safe);
        assert!(!publisher.confirms);
        assert_eq!(publisher.effective_safety(), SafetyMode::Unsafe);
    }
}
