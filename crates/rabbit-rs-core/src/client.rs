use std::{
    collections::HashMap,
    error::Error,
    fmt,
    sync::{Arc, Mutex as StdMutex, MutexGuard},
    time::Duration,
};

use tokio::sync::Mutex as AsyncMutex;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PoolLifecycle {
    Open { generation: u64 },
    Closing,
    Closed,
}

type Initializers = StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>;

/// A process-local, lazily connected pool shared by the PHP boundary.
pub struct ClientPool {
    config: Arc<ValidatedConfig>,
    transport: Arc<dyn Transport>,
    publisher_config: PublisherConfig,
    lifecycle: StdMutex<PoolLifecycle>,
    connections: StdMutex<HashMap<String, Arc<dyn TransportConnection>>>,
    connection_initializers: Initializers,
    publishers: StdMutex<HashMap<String, PublisherHandle>>,
    publisher_initializers: Initializers,
    consumers: StdMutex<HashMap<String, ConsumerHandle>>,
    consumer_initializers: Initializers,
    metrics: Metrics,
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
            lifecycle: StdMutex::new(PoolLifecycle::Open { generation: 1 }),
            connections: StdMutex::new(HashMap::new()),
            connection_initializers: StdMutex::new(HashMap::new()),
            publishers: StdMutex::new(HashMap::new()),
            publisher_initializers: StdMutex::new(HashMap::new()),
            consumers: StdMutex::new(HashMap::new()),
            consumer_initializers: StdMutex::new(HashMap::new()),
            metrics: Metrics::default(),
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
        let generation = self.open_generation()?;
        let worker = self.config.worker(profile).cloned().ok_or_else(|| {
            ClientError::new(
                ClientErrorKind::Configuration,
                format!("workers.{profile}: unknown worker profile"),
            )
        })?;
        if let Some(consumer) = self.ready(generation, &self.consumers, profile)? {
            return Ok(consumer);
        }
        let initializer = initializer(&self.consumer_initializers, profile);
        let _initializing = initializer.lock().await;
        if let Some(consumer) = self.ready(generation, &self.consumers, profile)? {
            return Ok(consumer);
        }

        let key = ConnectionKey::from_config(&self.config);
        let mut subscriptions = Vec::with_capacity(worker.subscriptions.len());
        for (index, subscription) in worker.subscriptions.into_iter().enumerate() {
            let connection = match self.connection(&subscription.broker).await {
                Ok(connection) => connection,
                Err(error) => {
                    close_subscription_channels(&subscriptions).await;
                    return Err(error);
                }
            };
            let channel = match connection.open_consumer().await {
                Ok(channel) => channel,
                Err(error) => {
                    close_subscription_channels(&subscriptions).await;
                    return Err(ClientError::transport(&error));
                }
            };
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
                    subscription.starvation_after,
                )),
            );
        }

        let consumer = ConsumerSet::spawn_with_metrics(
            subscriptions,
            usize::from(worker.scheduler.max_in_flight),
            self.metrics.clone(),
        )
        .await
        .map_err(|error| ClientError::consumer(&error))?;
        if self.commit(generation, &self.consumers, profile, consumer.clone()) {
            Ok(consumer)
        } else {
            let _ = consumer.close().await;
            Err(ClientError::closed())
        }
    }

    /// Returns a lock-free metrics snapshot shared by all actors in this pool.
    #[must_use]
    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Returns the number of pending messages in a queue on the given broker.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unknown broker, connection failure, or
    /// channel failure.
    pub async fn queue_size(&self, broker: &str, queue: &str) -> Result<u32, ClientError> {
        self.ensure_open()?;
        let connection = self.connection(broker).await?;
        let channel = connection
            .open_publisher()
            .await
            .map_err(|error| ClientError::transport(&error))?;
        channel
            .queue_size(queue)
            .await
            .map_err(|error| ClientError::transport(&error))
    }

    /// Purges all messages from a queue on the given broker.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unknown broker, connection failure, or
    /// channel failure.
    pub async fn purge_queue(&self, broker: &str, queue: &str) -> Result<(), ClientError> {
        self.ensure_open()?;
        let connection = self.connection(broker).await?;
        let channel = connection
            .open_publisher()
            .await
            .map_err(|error| ClientError::transport(&error))?;
        channel
            .purge_queue(queue)
            .await
            .map_err(|error| ClientError::transport(&error))
    }

    #[cfg(test)]
    pub(crate) async fn initialize_connection_for_tests(
        &self,
        broker: &str,
    ) -> Result<(), ClientError> {
        self.connection(broker).await.map(drop)
    }

    /// Returns whether this pool has entered its terminal closed state.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        !matches!(*lock(&self.lifecycle), PoolLifecycle::Open { .. })
    }

    /// Closes publisher actors and broker connections exactly once.
    ///
    /// # Errors
    ///
    /// Returns the first actor or transport shutdown failure.
    pub async fn close(&self) -> Result<(), ClientError> {
        {
            let mut lifecycle = lock(&self.lifecycle);
            match *lifecycle {
                PoolLifecycle::Open { .. } => *lifecycle = PoolLifecycle::Closing,
                PoolLifecycle::Closing | PoolLifecycle::Closed => return Ok(()),
            }
        }

        let consumers = std::mem::take(&mut *lock(&self.consumers));
        let mut first_error = None;
        for consumer in consumers.into_values() {
            if let Err(error) = consumer.close().await
                && first_error.is_none()
            {
                first_error = Some(ClientError::consumer(&error));
            }
        }

        let publishers = std::mem::take(&mut *lock(&self.publishers));
        for publisher in publishers.into_values() {
            if let Err(error) = publisher.close().await
                && error.kind() != PublishErrorKind::Closed
                && first_error.is_none()
            {
                first_error = Some(ClientError::publish(&error));
            }
        }

        let connections = std::mem::take(&mut *lock(&self.connections));
        for connection in connections.into_values() {
            if let Err(error) = connection.close().await
                && first_error.is_none()
            {
                first_error = Some(ClientError::transport(&error));
            }
        }

        *lock(&self.lifecycle) = PoolLifecycle::Closed;
        first_error.map_or(Ok(()), Err)
    }

    async fn publisher(&self, broker: &str) -> Result<PublisherHandle, ClientError> {
        let generation = self.open_generation()?;
        if let Some(publisher) = self.ready(generation, &self.publishers, broker)? {
            return Ok(publisher);
        }
        let initializer = initializer(&self.publisher_initializers, broker);
        let _initializing = initializer.lock().await;
        if let Some(publisher) = self.ready(generation, &self.publishers, broker)? {
            return Ok(publisher);
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
        if self.commit(generation, &self.publishers, broker, publisher.clone()) {
            Ok(publisher)
        } else {
            let _ = publisher.close().await;
            Err(ClientError::closed())
        }
    }

    async fn connection(&self, broker: &str) -> Result<Arc<dyn TransportConnection>, ClientError> {
        let broker_config = self.config.broker(broker).cloned().ok_or_else(|| {
            ClientError::new(
                ClientErrorKind::Configuration,
                format!("brokers.{broker}: unknown broker"),
            )
        })?;
        let generation = self.open_generation()?;
        if let Some(connection) = self.ready(generation, &self.connections, broker)? {
            return Ok(connection);
        }
        let initializer = initializer(&self.connection_initializers, broker);
        let _initializing = initializer.lock().await;
        if let Some(connection) = self.ready(generation, &self.connections, broker)? {
            return Ok(connection);
        }
        let connection: Arc<dyn TransportConnection> = Arc::from(
            self.transport
                .connect(&broker_config)
                .await
                .map_err(|error| ClientError::transport(&error))?,
        );
        if self.commit(generation, &self.connections, broker, connection.clone()) {
            Ok(connection)
        } else {
            let _ = connection.close().await;
            Err(ClientError::closed())
        }
    }

    fn ensure_open(&self) -> Result<(), ClientError> {
        self.open_generation().map(|_| ())
    }

    fn open_generation(&self) -> Result<u64, ClientError> {
        match *lock(&self.lifecycle) {
            PoolLifecycle::Open { generation } => Ok(generation),
            PoolLifecycle::Closing | PoolLifecycle::Closed => Err(ClientError::closed()),
        }
    }

    fn ready<T: Clone>(
        &self,
        generation: u64,
        registry: &StdMutex<HashMap<String, T>>,
        key: &str,
    ) -> Result<Option<T>, ClientError> {
        let lifecycle = lock(&self.lifecycle);
        if !matches!(
            *lifecycle,
            PoolLifecycle::Open {
                generation: current
            } if current == generation
        ) {
            return Err(ClientError::closed());
        }
        Ok(lock(registry).get(key).cloned())
    }

    fn commit<T>(
        &self,
        generation: u64,
        registry: &StdMutex<HashMap<String, T>>,
        key: &str,
        resource: T,
    ) -> bool {
        let lifecycle = lock(&self.lifecycle);
        if !matches!(
            *lifecycle,
            PoolLifecycle::Open {
                generation: current
            } if current == generation
        ) {
            return false;
        }
        lock(registry).insert(key.to_owned(), resource);
        true
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn initializer(initializers: &Initializers, key: &str) -> Arc<AsyncMutex<()>> {
    lock(initializers)
        .entry(key.to_owned())
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone()
}

async fn close_subscription_channels(subscriptions: &[Subscription]) {
    for subscription in subscriptions {
        let _ = subscription.channel.close().await;
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

    fn closed() -> Self {
        Self::new(ClientErrorKind::Closed, "native client pool is closed")
    }

    fn publish(error: &PublishError) -> Self {
        let kind = match error.kind() {
            PublishErrorKind::Backpressure => ClientErrorKind::Backpressure,
            PublishErrorKind::Closed => ClientErrorKind::Closed,
            PublishErrorKind::Transport => ClientErrorKind::Transport,
            PublishErrorKind::Nack | PublishErrorKind::Timeout | PublishErrorKind::Unconfirmed => {
                ClientErrorKind::Publish
            }
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
