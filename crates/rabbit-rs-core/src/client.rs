use std::{
    collections::HashMap,
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::sync::Mutex;

use crate::{
    config::ValidatedConfig,
    consumer::{ConsumerError, ConsumerHandle, ConsumerSet, Subscription, SubscriptionPolicy},
    metrics::{Metrics, MetricsSnapshot},
    pool::ConnectionKey,
    publisher::{
        PublishError, PublishErrorKind, PublishOutcome, PublishRequest, PublisherActor,
        PublisherConfig, PublisherHandle,
    },
    transport::{Transport, TransportConnection, TransportError, lapin::LapinTransport},
};

const DEFAULT_MAX_MESSAGES: usize = 256;
const DEFAULT_MAX_BYTES: usize = 1024 * 1024;
const DEFAULT_BUFFER_CAPACITY: usize = 8192;

/// A process-local, lazily connected pool shared by the PHP boundary.
pub struct ClientPool {
    config: Arc<ValidatedConfig>,
    transport: Arc<dyn Transport>,
    publisher_config: PublisherConfig,
    connections: Mutex<HashMap<String, Arc<dyn TransportConnection>>>,
    publishers: Mutex<HashMap<String, PublisherHandle>>,
    consumers: Mutex<HashMap<String, ConsumerHandle>>,
    metrics: Metrics,
    closed: AtomicBool,
}

impl ClientPool {
    /// Creates a pool backed by the production Lapin transport without opening sockets.
    #[must_use]
    pub fn production(config: Arc<ValidatedConfig>) -> Self {
        Self::new(config, Arc::new(LapinTransport))
    }

    /// Creates a pool with an injectable transport.
    #[must_use]
    pub fn new(config: Arc<ValidatedConfig>, transport: Arc<dyn Transport>) -> Self {
        Self::with_publisher_config(config, transport, publisher_config())
    }

    /// Creates a pool with an injectable transport and publisher limits for extension tests.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    #[must_use]
    pub fn new_for_tests(
        config: Arc<ValidatedConfig>,
        transport: Arc<dyn Transport>,
        publisher_config: PublisherConfig,
    ) -> Self {
        Self::with_publisher_config(config, transport, publisher_config)
    }

    fn with_publisher_config(
        config: Arc<ValidatedConfig>,
        transport: Arc<dyn Transport>,
        publisher_config: PublisherConfig,
    ) -> Self {
        Self {
            config,
            transport,
            publisher_config,
            connections: Mutex::new(HashMap::new()),
            publishers: Mutex::new(HashMap::new()),
            consumers: Mutex::new(HashMap::new()),
            metrics: Metrics::default(),
            closed: AtomicBool::new(false),
        }
    }

    /// Publishes one message through a lazily reused broker connection and publisher actor.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unknown broker, connection failure, backpressure,
    /// negative confirmation, mandatory return, timeout, or closed pool.
    pub async fn publish(
        &self,
        broker: &str,
        request: PublishRequest,
    ) -> Result<PublishOutcome, ClientError> {
        self.ensure_open()?;
        let publisher = self.publisher(broker).await?;
        let waiter = publisher
            .try_publish(request)
            .map_err(|error| ClientError::publish(&error))?;
        waiter
            .wait()
            .await
            .map_err(|error| ClientError::publish(&error))
    }

    /// Enqueues a complete batch before awaiting confirmations, preserving input order.
    ///
    /// # Errors
    ///
    /// Returns the first terminal failure after resolving every publication that was
    /// already accepted by an actor.
    pub async fn publish_batch(
        &self,
        requests: Vec<(String, PublishRequest)>,
    ) -> Result<Vec<PublishOutcome>, ClientError> {
        self.ensure_open()?;
        let mut waiters = Vec::with_capacity(requests.len());
        let mut immediate_error = None;

        for (broker, request) in requests {
            match self.publisher(&broker).await {
                Ok(publisher) => match publisher.try_publish(request) {
                    Ok(waiter) => waiters.push(waiter),
                    Err(error) => {
                        immediate_error.get_or_insert_with(|| ClientError::publish(&error));
                    }
                },
                Err(error) => {
                    immediate_error.get_or_insert(error);
                }
            }
        }

        let mut outcomes = Vec::with_capacity(waiters.len());
        let mut terminal_error = immediate_error;
        for waiter in waiters {
            match waiter.wait().await {
                Ok(outcome) => outcomes.push(outcome),
                Err(error) => {
                    terminal_error.get_or_insert_with(|| ClientError::publish(&error));
                }
            }
        }

        terminal_error.map_or(Ok(outcomes), Err)
    }

    /// Opens or reuses the multiplexed consumer actor for one worker profile.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unknown profile or broker, connection/channel
    /// failure, `QoS` failure, or consumer registration failure.
    pub async fn consumer(&self, profile: &str) -> Result<ConsumerHandle, ClientError> {
        self.ensure_open()?;
        let mut consumers = self.consumers.lock().await;
        if let Some(consumer) = consumers.get(profile) {
            return Ok(consumer.clone());
        }

        let worker = self.config.worker(profile).cloned().ok_or_else(|| {
            ClientError::new(
                ClientErrorKind::Configuration,
                format!("workers.{profile}: unknown worker profile"),
            )
        })?;
        let key = ConnectionKey::from_config(&self.config);
        let mut subscriptions = Vec::with_capacity(worker.subscriptions.len());
        for (index, subscription) in worker.subscriptions.into_iter().enumerate() {
            let connection = self.connection(&subscription.broker).await?;
            let channel = connection
                .open_consumer()
                .await
                .map_err(|error| ClientError::transport(&error))?;
            let channel_id = u16::try_from(index.saturating_add(1)).unwrap_or(u16::MAX);
            subscriptions.push(
                Subscription::new(
                    subscription.name,
                    key,
                    subscription.queue,
                    Arc::from(channel),
                )
                .prefetch(subscription.prefetch)
                .channel_id(channel_id)
                .policy(SubscriptionPolicy::new(
                    subscription.weight,
                    subscription.priority_class,
                    Duration::from_secs(30),
                )),
            );
        }

        let consumer = ConsumerSet::spawn_with_metrics(
            subscriptions,
            usize::from(worker.max_in_flight),
            self.metrics.clone(),
        )
        .await
        .map_err(|error| ClientError::consumer(&error))?;
        consumers.insert(profile.to_owned(), consumer.clone());
        Ok(consumer)
    }

    /// Returns a lock-free metrics snapshot shared by all actors in this pool.
    #[must_use]
    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Returns whether this pool has entered its terminal closed state.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Closes publisher actors and broker connections exactly once.
    ///
    /// # Errors
    ///
    /// Returns the first actor or transport shutdown failure.
    pub async fn close(&self) -> Result<(), ClientError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }

        let consumers = std::mem::take(&mut *self.consumers.lock().await);
        let mut first_error = None;
        for consumer in consumers.into_values() {
            if let Err(error) = consumer.close().await
                && first_error.is_none()
            {
                first_error = Some(ClientError::consumer(&error));
            }
        }

        let publishers = std::mem::take(&mut *self.publishers.lock().await);
        for publisher in publishers.into_values() {
            if let Err(error) = publisher.close().await
                && first_error.is_none()
            {
                first_error = Some(ClientError::publish(&error));
            }
        }

        let connections = std::mem::take(&mut *self.connections.lock().await);
        for connection in connections.into_values() {
            if let Err(error) = connection.close().await
                && first_error.is_none()
            {
                first_error = Some(ClientError::transport(&error));
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    async fn publisher(&self, broker: &str) -> Result<PublisherHandle, ClientError> {
        let mut publishers = self.publishers.lock().await;
        if let Some(publisher) = publishers.get(broker) {
            return Ok(publisher.clone());
        }

        let connection = self.connection(broker).await?;
        let channel = connection
            .open_publisher()
            .await
            .map_err(|error| ClientError::transport(&error))?;
        let publisher = PublisherActor::spawn_with_metrics(
            Arc::from(channel),
            self.publisher_config,
            self.metrics.clone(),
        );
        publishers.insert(broker.to_owned(), publisher.clone());
        Ok(publisher)
    }

    async fn connection(&self, broker: &str) -> Result<Arc<dyn TransportConnection>, ClientError> {
        let mut connections = self.connections.lock().await;
        if let Some(connection) = connections.get(broker) {
            return Ok(connection.clone());
        }

        let broker_config = self.config.broker(broker).ok_or_else(|| {
            ClientError::new(
                ClientErrorKind::Configuration,
                format!("brokers.{broker}: unknown broker"),
            )
        })?;
        let connection: Arc<dyn TransportConnection> = Arc::from(
            self.transport
                .connect(broker_config)
                .await
                .map_err(|error| ClientError::transport(&error))?,
        );
        connections.insert(broker.to_owned(), connection.clone());
        Ok(connection)
    }

    fn ensure_open(&self) -> Result<(), ClientError> {
        if self.is_closed() {
            Err(ClientError::new(
                ClientErrorKind::Closed,
                "native client pool is closed",
            ))
        } else {
            Ok(())
        }
    }
}

fn publisher_config() -> PublisherConfig {
    PublisherConfig::new(
        DEFAULT_MAX_MESSAGES,
        DEFAULT_MAX_BYTES,
        Duration::from_millis(1),
        DEFAULT_BUFFER_CAPACITY,
        Duration::from_secs(30),
    )
}

/// Stable classification for failures at the integrated client-pool boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientErrorKind {
    Configuration,
    Backpressure,
    Publish,
    Consumer,
    Transport,
    Closed,
}

/// Error returned by the integrated process-local client pool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientError {
    kind: ClientErrorKind,
    message: String,
}

impl ClientError {
    #[must_use]
    pub const fn kind(&self) -> ClientErrorKind {
        self.kind
    }

    fn new(kind: ClientErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn publish(error: &PublishError) -> Self {
        let kind = match error.kind() {
            PublishErrorKind::Backpressure => ClientErrorKind::Backpressure,
            PublishErrorKind::Closed => ClientErrorKind::Closed,
            PublishErrorKind::Nack
            | PublishErrorKind::Timeout
            | PublishErrorKind::Unconfirmed
            | PublishErrorKind::Transport => ClientErrorKind::Publish,
        };
        Self::new(kind, error.to_string())
    }

    fn transport(error: &TransportError) -> Self {
        Self::new(ClientErrorKind::Transport, error.to_string())
    }

    fn consumer(error: &ConsumerError) -> Self {
        Self::new(ClientErrorKind::Consumer, error.to_string())
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ClientError {}
