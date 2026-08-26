use std::{
    collections::HashMap,
    error::Error,
    fmt,
    sync::{Arc, Mutex as StdMutex, MutexGuard},
};

use tokio::sync::Mutex as AsyncMutex;

use crate::{
    config::{TopologyMode, ValidatedConfig},
    consumer::{ConsumerError, ConsumerHandle},
    metrics::{Metrics, MetricsSnapshot},
    pool::{RecoveryCoordinator, RecoveryCoordinatorConfig, RecoveryCoordinatorHandle},
    publisher::{
        BatchOutcome, MessageOutcome, PublishError, PublishErrorKind, PublishOutcome,
        PublishRequest, PublishWaiter, PublisherConfig, PublisherHandle,
    },
    recovery::ConnectionState,
    topology::{DeadLetterDefinition, QueueDefinition, TopologyDefinition, TopologyPlan},
    transport::{Transport, TransportConnection, TransportError, lapin::LapinTransport},
};

const DEFAULT_BUFFER_CAPACITY: usize = 1024;

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
    coordinators: StdMutex<HashMap<String, RecoveryCoordinatorHandle>>,
    coordinator_initializers: Initializers,
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
        let pc = publisher_config(&config);
        Self::with_publisher_config(config, transport, pc)
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
            coordinators: StdMutex::new(HashMap::new()),
            coordinator_initializers: StdMutex::new(HashMap::new()),
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
        let waiter = match self.publisher_config.safety {
            crate::config::SafetyMode::Blind => publisher
                .try_publish_blind(request)
                .map_err(|error| ClientError::publish(&error))?,
            crate::config::SafetyMode::Safe | crate::config::SafetyMode::Unsafe => publisher
                .try_publish_hot(request)
                .map_err(|error| ClientError::publish(&error))?,
        };
        waiter
            .wait()
            .await
            .map_err(|error| ClientError::publish(&error))
    }

    /// Enqueues a complete batch before awaiting confirmations, preserving input order.
    ///
    /// The publisher handle is cached per broker so that repeated brokers in the
    /// batch reuse a single actor instead of performing one lookup per message.
    /// Outcomes are returned in the original input order regardless of how
    /// requests are grouped by broker internally.
    ///
    /// # Errors
    ///
    /// Returns the first terminal failure after resolving every publication that
    /// was already accepted by an actor.
    pub async fn publish_batch(
        &self,
        requests: Vec<(String, PublishRequest)>,
    ) -> Result<Vec<PublishOutcome>, ClientError> {
        self.ensure_open()?;
        let total = requests.len();
        let blind = matches!(
            self.publisher_config.safety,
            crate::config::SafetyMode::Blind
        );
        let mut by_broker: HashMap<String, Vec<(usize, PublishRequest)>> = HashMap::new();
        for (i, (broker, request)) in requests.into_iter().enumerate() {
            by_broker.entry(broker).or_default().push((i, request));
        }

        let mut outcomes: Vec<Option<Result<PublishOutcome, ClientError>>> = vec![None; total];
        let mut waiters: Vec<(usize, PublishWaiter)> = Vec::new();
        let mut immediate_error = None;

        for (broker, msgs) in &by_broker {
            let publisher = self.publisher(broker).await?;
            for (original_index, request) in msgs {
                let result = if blind {
                    publisher.try_publish_blind(request.clone())
                } else {
                    publisher.try_publish_hot(request.clone())
                };
                match result {
                    Ok(waiter) => waiters.push((*original_index, waiter)),
                    Err(error) => {
                        let client_err = ClientError::publish(&error);
                        immediate_error.get_or_insert_with(|| client_err.clone());
                        outcomes[*original_index] = Some(Err(client_err));
                    }
                }
            }
        }

        let mut terminal_error = immediate_error;
        let results = PublishWaiter::wait_all(waiters).await;
        for (index, result) in results {
            match result {
                Ok(outcome) => outcomes[index] = Some(Ok(outcome)),
                Err(error) => {
                    let client_err = ClientError::publish(&error);
                    terminal_error.get_or_insert_with(|| client_err.clone());
                    outcomes[index] = Some(Err(client_err));
                }
            }
        }

        let mut results = Vec::with_capacity(total);
        for outcome in outcomes {
            match outcome {
                Some(Ok(o)) => results.push(o),
                Some(Err(e)) => {
                    terminal_error.get_or_insert(e);
                }
                None => {
                    terminal_error.get_or_insert(ClientError::publish(&PublishError::new(
                        PublishErrorKind::Backpressure,
                        "publication was not accepted by the actor",
                    )));
                }
            }
        }

        terminal_error.map_or(Ok(results), Err)
    }

    /// Enqueues a complete batch and returns a per-message indexed report.
    ///
    /// Like [`publish_batch`](Self::publish_batch), the publisher handle is
    /// cached per broker and the results preserve the original input order.
    /// Unlike `publish_batch`, every input request yields exactly one
    /// [`MessageOutcome`] entry — including publications that were never
    /// accepted by an actor — so callers can correlate per-message status
    /// without inferring gaps.
    ///
    /// # Errors
    ///
    /// Returns a [`ClientError`] only if the pool is closed or an unknown
    /// broker is referenced before any publication is attempted. Per-message
    /// failures are reported inside the returned [`BatchOutcome`].
    pub async fn publish_batch_detailed(
        &self,
        requests: Vec<(String, PublishRequest)>,
    ) -> Result<BatchOutcome, ClientError> {
        self.ensure_open()?;
        let total = requests.len();
        let mut by_broker: HashMap<String, Vec<(usize, PublishRequest)>> = HashMap::new();
        for (i, (broker, request)) in requests.into_iter().enumerate() {
            by_broker.entry(broker).or_default().push((i, request));
        }

        let mut outcomes: Vec<Option<Result<PublishOutcome, PublishError>>> = vec![None; total];
        let mut waiters: Vec<(usize, PublishWaiter)> = Vec::new();

        for (broker, msgs) in &by_broker {
            let publisher = self.publisher(broker).await?;
            for (original_index, request) in msgs {
                match publisher.try_publish(request.clone()) {
                    Ok(waiter) => waiters.push((*original_index, waiter)),
                    Err(error) => {
                        outcomes[*original_index] = Some(Err(error));
                    }
                }
            }
        }

        let results = PublishWaiter::wait_all(waiters).await;
        for (index, result) in results {
            match result {
                Ok(outcome) => outcomes[index] = Some(Ok(outcome)),
                Err(error) => {
                    outcomes[index] = Some(Err(error));
                }
            }
        }

        let results = outcomes
            .into_iter()
            .map(|o| match o {
                Some(Ok(PublishOutcome::Confirmed { message_id })) => {
                    MessageOutcome::Confirmed(PublishOutcome::Confirmed { message_id })
                }
                Some(Ok(PublishOutcome::Returned { reply, .. })) => MessageOutcome::Returned(reply),
                Some(Ok(PublishOutcome::Ambiguous { .. })) => MessageOutcome::Failed(
                    PublishError::new(PublishErrorKind::Unconfirmed, "ambiguous confirmation"),
                ),
                Some(Err(error)) => MessageOutcome::Failed(error),
                None => MessageOutcome::NotAccepted(PublishError::new(
                    PublishErrorKind::Backpressure,
                    "not accepted",
                )),
            })
            .collect();

        Ok(BatchOutcome { results })
    }

    /// Returns a cached consumer handle if it matches the coordinator's
    /// current generation, evicting stale handles when the generation has
    /// advanced. Returns `Ok(None)` when no cached handle exists or it has
    /// been evicted.
    fn consumer_if_fresh(
        &self,
        generation: u64,
        worker: &crate::config::WorkerProfile,
        profile: &str,
    ) -> Result<Option<ConsumerHandle>, ClientError> {
        let Some(consumer) = self.ready(generation, &self.consumers, profile)? else {
            return Ok(None);
        };
        let broker = &worker.subscriptions[0].broker;
        let coord_gen =
            lock(&self.coordinators)
                .get(broker)
                .and_then(|coord| match coord.state() {
                    crate::recovery::ConnectionState::Ready { generation } => Some(generation),
                    _ => None,
                });
        match coord_gen {
            Some(coord_gen) if consumer.generation() == coord_gen => Ok(Some(consumer)),
            Some(_) => {
                // Stale — evict and fall through to fetch a fresh handle.
                lock(&self.consumers).remove(profile);
                Ok(None)
            }
            None => {
                // Coordinator not Ready (Recovering/Connecting/etc.) — don't
                // return a potentially stale cached handle; fall through so the
                // loop can wait for Ready and fetch a fresh consumer.
                Ok(None)
            }
        }
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

        // Check for a cached consumer handle. If the coordinator has moved to a
        // newer generation, the cached handle is stale and must be evicted.
        if let Some(consumer) = self.consumer_if_fresh(generation, &worker, profile)? {
            return Ok(consumer);
        }

        let initializer = initializer(&self.consumer_initializers, profile);
        let _initializing = initializer.lock().await;

        // Double-check after acquiring the initializer lock.
        if let Some(consumer) = self.consumer_if_fresh(generation, &worker, profile)? {
            return Ok(consumer);
        }

        // Ensure coordinators for all distinct brokers in the worker profile are
        // started so that every subscription's broker has a recovery coordinator
        // running. The coordinator for each broker only creates consumers for
        // subscriptions that belong to that broker (see `recover_generation`).
        let mut brokers: Vec<String> = worker
            .subscriptions
            .iter()
            .map(|s| s.broker.clone())
            .collect();
        brokers.dedup();
        for broker in &brokers {
            self.coordinator(broker).await?;
        }

        // The consumer is composed from all coordinators. For now, use the
        // first coordinator's handle for the primary consumer handle. A future
        // task may compose a multi-broker consumer that merges handles.
        let coordinator = self.coordinator(&brokers[0]).await?;
        let consumer = loop {
            if self.is_closed() {
                return Err(ClientError::closed());
            }
            if let Ok(consumer) = coordinator.consumer(profile).await {
                break consumer;
            }
            // Wait for any state transition (including Ready so we can retry
            // without blocking when the state hasn't left Ready yet).
            coordinator
                .wait_for_state(|state| {
                    matches!(
                        state,
                        crate::recovery::ConnectionState::Ready { .. }
                            | crate::recovery::ConnectionState::Recovering { .. }
                            | crate::recovery::ConnectionState::Connecting { .. }
                            | crate::recovery::ConnectionState::FailedPermanent { .. }
                            | crate::recovery::ConnectionState::Closed
                    )
                })
                .await;
            if self.is_closed() {
                return Err(ClientError::closed());
            }
            if matches!(
                coordinator.state(),
                crate::recovery::ConnectionState::FailedPermanent { .. }
            ) {
                return Err(ClientError::transport(&TransportError::connection(
                    "broker connection failed permanently",
                )));
            }
            if matches!(
                coordinator.state(),
                crate::recovery::ConnectionState::Closed
            ) {
                return Err(ClientError::closed());
            }
        };

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

    /// Returns the current connection state for each known broker coordinator.
    ///
    /// Brokers whose coordinator has not been started yet are absent from the
    /// returned map. The map is a point-in-time snapshot and may be stale by
    /// the time it is inspected.
    #[must_use]
    pub fn connection_states(&self) -> HashMap<String, ConnectionState> {
        lock(&self.coordinators)
            .iter()
            .map(|(broker, handle)| (broker.clone(), handle.state()))
            .collect()
    }

    /// Returns the aggregate publisher utilization across all known brokers.
    ///
    /// `in_flight` is the total number of retained capacity permits across
    /// every publisher actor (i.e., publications awaiting a terminal
    /// outcome). `capacity` is the total configured buffer capacity across
    /// all publishers. Brokers whose publisher has not been started yet
    /// contribute zero in-flight messages and zero capacity.
    #[must_use]
    pub fn publisher_utilization(&self) -> (usize, usize) {
        let publishers = lock(&self.publishers);
        let capacity_per_publisher = self.publisher_config.buffer_capacity.max(1);
        let publisher_count = publishers.len();
        let total_capacity = capacity_per_publisher.saturating_mul(publisher_count);
        let in_flight = publishers
            .values()
            .map(|handle| capacity_per_publisher.saturating_sub(handle.available_permits()))
            .sum::<usize>();
        (in_flight, total_capacity)
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

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub async fn simulate_connection_loss_for_tests(
        &self,
        broker: &str,
        error: TransportError,
    ) -> Result<(), ClientError> {
        let coord = lock(&self.coordinators)
            .get(broker)
            .cloned()
            .ok_or_else(|| {
                ClientError::new(ClientErrorKind::Configuration, "coordinator not found")
            })?;
        coord
            .connection_lost(error)
            .await
            .map_err(|_| ClientError::closed())
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

        let coordinators = std::mem::take(&mut *lock(&self.coordinators));
        for coordinator in coordinators.into_values() {
            if let Err(error) = coordinator.close().await
                && first_error.is_none()
            {
                first_error = Some(ClientError::new(
                    ClientErrorKind::Transport,
                    error.to_string(),
                ));
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

        let coordinator = self.coordinator(broker).await?;
        let publisher = loop {
            if self.is_closed() {
                return Err(ClientError::closed());
            }
            if let Ok(publisher) = coordinator.publisher().await {
                break publisher;
            }
            coordinator
                .wait_for_state(|state| {
                    matches!(
                        state,
                        crate::recovery::ConnectionState::Ready { .. }
                            | crate::recovery::ConnectionState::Recovering { .. }
                            | crate::recovery::ConnectionState::Connecting { .. }
                            | crate::recovery::ConnectionState::FailedPermanent { .. }
                            | crate::recovery::ConnectionState::Closed
                    )
                })
                .await;
            if self.is_closed() {
                return Err(ClientError::closed());
            }
            if matches!(
                coordinator.state(),
                crate::recovery::ConnectionState::FailedPermanent { .. }
            ) {
                return Err(ClientError::transport(&TransportError::connection(
                    "broker connection failed permanently",
                )));
            }
            if matches!(
                coordinator.state(),
                crate::recovery::ConnectionState::Closed
            ) {
                return Err(ClientError::closed());
            }
        };
        if self.commit(generation, &self.publishers, broker, publisher.clone()) {
            Ok(publisher)
        } else {
            let _ = publisher.close().await;
            Err(ClientError::closed())
        }
    }

    async fn coordinator(&self, broker: &str) -> Result<RecoveryCoordinatorHandle, ClientError> {
        let broker_config = self.config.broker(broker).cloned().ok_or_else(|| {
            ClientError::new(
                ClientErrorKind::Configuration,
                format!("brokers.{broker}: unknown broker"),
            )
        })?;
        let generation = self.open_generation()?;
        if let Some(coordinator) = self.ready(generation, &self.coordinators, broker)? {
            return Ok(coordinator);
        }
        let initializer = initializer(&self.coordinator_initializers, broker);
        let _initializing = initializer.lock().await;
        if let Some(coordinator) = self.ready(generation, &self.coordinators, broker)? {
            return Ok(coordinator);
        }

        let topology_plan = self.build_topology_plan();
        let coordinator_config = RecoveryCoordinatorConfig {
            broker: broker_config,
            policy: crate::recovery::RecoveryPolicy::default(),
            topology_plan,
            publisher_config: self.publisher_config,
            config: self.config.clone(),
            metrics: self.metrics.clone(),
        };
        let coordinator = RecoveryCoordinator::spawn(&self.transport, coordinator_config);
        if self.commit(generation, &self.coordinators, broker, coordinator.clone()) {
            Ok(coordinator)
        } else {
            let _ = coordinator.close().await;
            Err(ClientError::closed())
        }
    }

    fn build_topology_plan(&self) -> TopologyPlan {
        let queue_type = self.config.queue_type();
        let queue_durable = self.config.queue_durable();
        let queues: Vec<_> = self
            .config
            .worker_profiles()
            .iter()
            .flat_map(|worker| &worker.subscriptions)
            .map(|sub| {
                let mut qd = QueueDefinition::new(&sub.queue)
                    .kind(queue_type)
                    .durable(queue_durable);
                if let Some(limit) = self.config.delivery_limit() {
                    qd = qd.delivery_limit(limit);
                }
                qd
            })
            .collect();

        let mut topology = TopologyDefinition::new(vec![], queues, vec![]);
        if let Some(dl) = self.config.dead_letter()
            && dl.enabled
        {
            for sub in self
                .config
                .worker_profiles()
                .iter()
                .flat_map(|w| &w.subscriptions)
            {
                let routing_key = dl.routing_key.clone().unwrap_or_else(|| sub.queue.clone());
                topology = topology.with_dead_letter(DeadLetterDefinition::new(
                    sub.queue.clone(),
                    dl.exchange.clone(),
                    dl.queue.clone(),
                    routing_key,
                ));
            }
        }

        TopologyPlan::compile(self.config.topology_mode(), topology).unwrap_or_else(|_error| {
            TopologyPlan::compile(
                TopologyMode::External,
                TopologyDefinition::new(vec![], vec![], vec![]),
            )
            .expect("external mode always compiles")
        })
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

fn publisher_config(config: &ValidatedConfig) -> PublisherConfig {
    let publisher = config.publisher();
    let safety = publisher.effective_safety();
    PublisherConfig::with_safety(DEFAULT_BUFFER_CAPACITY, publisher.confirm_timeout, safety)
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
