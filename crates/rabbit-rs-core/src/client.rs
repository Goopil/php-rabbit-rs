use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    sync::{Arc, Mutex as StdMutex, MutexGuard},
};

use tokio::sync::Mutex as AsyncMutex;

use crate::{
    config::{SafetyMode, ValidatedConfig},
    consumer::{ConsumerError, ConsumerHandle},
    metrics::{Metrics, MetricsSnapshot},
    pool::{RecoveryCoordinator, RecoveryCoordinatorConfig, RecoveryCoordinatorHandle},
    publisher::{
        PublishError, PublishErrorKind, PublishOutcome, PublishRequest, PublishWaiter,
        PublisherConfig, PublisherHandle,
    },
    recovery::ConnectionState,
    topology::TopologyPlan,
    transport::{PublisherChannel, Transport, TransportError, lapin::LapinTransport},
};

#[cfg(any(test, feature = "test-support"))]
use crate::publisher::PublisherActor;

const DEFAULT_BUFFER_CAPACITY: usize = 1024;

/// Returns the distinct broker names of a worker profile's subscriptions, in
/// subscription order.
fn worker_brokers(worker: &crate::config::WorkerProfile) -> Vec<String> {
    let mut brokers: Vec<String> = Vec::with_capacity(worker.subscriptions.len());
    for subscription in &worker.subscriptions {
        if !brokers.contains(&subscription.broker) {
            brokers.push(subscription.broker.clone());
        }
    }
    brokers
}

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
    coordinators: StdMutex<HashMap<String, RecoveryCoordinatorHandle>>,
    coordinator_initializers: Initializers,
    publishers: StdMutex<HashMap<String, PublisherHandle>>,
    publisher_initializers: Initializers,
    consumers: StdMutex<HashMap<String, ConsumerHandle>>,
    consumer_initializers: Initializers,
    /// Worker profiles explicitly requested through [`ClientPool::consumer`].
    /// Shared with every coordinator so recovery only establishes requested
    /// consumers; declared-but-unrequested profiles stay dormant.
    requested_profiles: Arc<StdMutex<HashSet<String>>>,
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
            coordinators: StdMutex::new(HashMap::new()),
            coordinator_initializers: StdMutex::new(HashMap::new()),
            publishers: StdMutex::new(HashMap::new()),
            publisher_initializers: StdMutex::new(HashMap::new()),
            consumers: StdMutex::new(HashMap::new()),
            consumer_initializers: StdMutex::new(HashMap::new()),
            requested_profiles: Arc::new(StdMutex::new(HashSet::new())),
            metrics: Metrics::default(),
        }
    }

    /// Enqueues a complete batch through cached per-broker publishers,
    /// preserving input order in the returned outcomes.
    ///
    /// The publisher handle is cached per broker so that repeated brokers in
    /// the batch reuse a single actor instead of performing one lookup per
    /// message.
    ///
    /// In [`SafetyMode::Safe`] and [`SafetyMode::Unsafe`] modes the batch
    /// awaits every outcome before returning; outcomes are returned in the
    /// original input order regardless of how requests are grouped by broker
    /// internally.
    ///
    /// In [`SafetyMode::Blind`] mode the batch returns as soon as every
    /// request has been handed off to the bounded publish pump (backpressure
    /// by blocking); no transport outcome is awaited and every returned
    /// outcome is the synthetic `Confirmed` resolved at hand-off. A closed
    /// pump or an exhausted publisher byte budget fails the batch immediately,
    /// leaving the requests that were not yet enqueued with the caller —
    /// re-buffering callers therefore re-publish a conservative superset, and
    /// duplicates are permitted and identifiable through their `message_id`.
    ///
    /// # Errors
    ///
    /// Returns the first terminal failure after resolving every publication
    /// that was already accepted by an actor. In blind mode a byte-budget
    /// exhaustion (`Backpressure`) or a closed pump (`Closed`) fails the batch
    /// immediately.
    pub async fn publish_batch(
        &self,
        requests: Vec<(String, PublishRequest)>,
    ) -> Result<Vec<PublishOutcome>, ClientError> {
        self.ensure_open()?;
        let total = requests.len();
        let blind = matches!(self.publisher_config.safety, SafetyMode::Blind);
        let mut by_broker: HashMap<String, Vec<(usize, PublishRequest)>> = HashMap::new();
        for (i, (broker, request)) in requests.into_iter().enumerate() {
            by_broker.entry(broker).or_default().push((i, request));
        }

        let mut outcomes: Vec<Option<Result<PublishOutcome, ClientError>>> = vec![None; total];
        let mut waiters: Vec<(usize, PublishWaiter)> = Vec::new();
        let mut terminal_error = None;

        for (broker, msgs) in by_broker {
            let publisher = match self.publisher(&broker).await {
                Ok(publisher) => publisher,
                Err(error) => {
                    if blind {
                        // Same contract as a closed pump: fail the batch
                        // immediately, leaving the un-enqueued requests with
                        // the caller (Task 1 contract).
                        return Err(error);
                    }
                    // Publications already accepted by earlier brokers'
                    // actors must still be resolved (issue #83): record a
                    // terminal error for this broker's indices and keep
                    // collecting the remaining brokers.
                    terminal_error.get_or_insert_with(|| error.clone());
                    for (original_index, _request) in msgs {
                        outcomes[original_index] = Some(Err(error.clone()));
                    }
                    continue;
                }
            };
            for (original_index, request) in msgs {
                let result = if blind {
                    let message_id = Arc::clone(&request.properties.message_id);
                    publisher
                        .publish_blind(request)
                        .await
                        .map(|_| PublishOutcome::Confirmed { message_id })
                        .map_err(|error| ClientError::publish(&error))
                } else {
                    match publisher.try_publish(request) {
                        Ok(waiter) => {
                            waiters.push((original_index, waiter));
                            continue;
                        }
                        Err(error) => Err(ClientError::publish(&error)),
                    }
                };
                match result {
                    Ok(outcome) => outcomes[original_index] = Some(Ok(outcome)),
                    Err(client_err) if blind => {
                        // Only a closed pump can fail a blind hand-off. Fail
                        // the batch immediately: un-enqueued requests stay
                        // with the caller, which re-buffers a conservative
                        // superset (Task 1 contract).
                        return Err(client_err);
                    }
                    Err(client_err) => {
                        terminal_error.get_or_insert_with(|| client_err.clone());
                        outcomes[original_index] = Some(Err(client_err));
                    }
                }
            }
        }

        if !blind {
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

    /// Flush barrier for blind-mode publishing: resolves once every request
    /// enqueued before this call has been handed to the transport (or dropped
    /// for lack of a channel during recovery) on every cached publisher.
    ///
    /// Never fails when no publisher is cached yet, and resolves immediately
    /// for non-blind pools: those publishers have no pump and no outstanding
    /// hand-offs — their outcomes are resolved before `publish` and
    /// `publish_batch` return.
    ///
    /// # Errors
    ///
    /// Returns the first [`PublishErrorKind::Closed`] raised by a cached
    /// publisher's pump.
    pub async fn flush_blind(&self) -> Result<(), ClientError> {
        let publishers: Vec<PublisherHandle> = lock(&self.publishers).values().cloned().collect();
        let mut first_error = None;
        for publisher in publishers {
            if let Err(error) = publisher.flush_blind().await {
                first_error.get_or_insert(ClientError::publish(&error));
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Returns a cached consumer handle if every broker coordinator's
    /// current generation still matches the handle's per-source generations,
    /// evicting stale handles when any generation has advanced. Returns
    /// `Ok(None)` when no cached handle exists, it has been evicted, or a
    /// coordinator is not `Ready`.
    fn consumer_if_fresh(
        &self,
        generation: u64,
        worker: &crate::config::WorkerProfile,
        profile: &str,
    ) -> Result<Option<ConsumerHandle>, ClientError> {
        let Some(consumer) = self.ready(generation, &self.consumers, profile)? else {
            return Ok(None);
        };
        let brokers = worker_brokers(worker);
        let source_generations = consumer.source_generations();
        if source_generations.len() != brokers.len() {
            lock(&self.consumers).remove(profile);
            return Ok(None);
        }
        let stale = {
            let coordinators = lock(&self.coordinators);
            brokers
                .iter()
                .zip(&source_generations)
                .any(|(broker, source_generation)| {
                    coordinators
                        .get(broker)
                        .is_none_or(|coord| match coord.state() {
                            crate::recovery::ConnectionState::Ready { generation } => {
                                generation != *source_generation
                            }
                            _ => true,
                        })
                })
        };
        if stale {
            // Stale — evict and fall through to fetch a fresh handle.
            lock(&self.consumers).remove(profile);
            return Ok(None);
        }
        Ok(Some(consumer))
    }

    /// Opens or reuses the composed consumer for one worker profile.
    ///
    /// The returned handle merges the per-broker consumer sets of the
    /// profile: deliveries from every broker surface through one API, while
    /// each delivery's settlements route back to its originating broker and
    /// each broker keeps its own recovery coordinator.
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

        // Record the request before any coordinator is triggered so that the
        // current or next recovery generation establishes this profile's
        // consumer channels (see `recover_generation`).
        lock(&self.requested_profiles).insert(profile.to_owned());

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
        let wait_timeout = self.config.consumer().wait_timeout;
        let profile_owned = profile.to_owned();
        let acquisition = async {
            let brokers = worker_brokers(&worker);
            for broker in &brokers {
                self.coordinator(broker).await?;
            }

            // Compose the profile's consumer from every broker's coordinator:
            // each source multiplexes the subscriptions of one broker, and the
            // composite merges deliveries fairly while routing every
            // settlement back to the broker the delivery came from.
            let mut sources = Vec::with_capacity(brokers.len());
            for broker in &brokers {
                let coordinator = self.coordinator(broker).await?;
                let profile_ref = &profile_owned;
                let consumer = self
                    .wait_for_coordinator_ready(&coordinator, true, || async {
                        coordinator.consumer(profile_ref).await.ok()
                    })
                    .await?;
                sources.push(consumer);
            }

            Ok(ConsumerHandle::from_sources(sources))
        };

        let consumer = tokio::time::timeout(wait_timeout, acquisition)
            .await
            .map_err(|_elapsed| {
                ClientError::transport(&TransportError::connection(format!(
                    "consumer profile '{profile}' did not become ready within {wait_timeout:?}"
                )))
            })??;

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

    /// Returns the publisher safety mode this pool was configured with.
    #[must_use]
    pub fn safety_mode(&self) -> SafetyMode {
        self.publisher_config.safety
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
        let channel = self.admin_channel(broker).await?;
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
        let channel = self.admin_channel(broker).await?;
        channel
            .purge_queue(queue)
            .await
            .map_err(|error| ClientError::transport(&error))
    }

    /// Opens a publisher channel for admin operations on the broker
    /// coordinator's single connection, waiting for readiness.
    ///
    /// Admin operations ride the connection actor's recovery machinery
    /// instead of caching a second raw connection (issue #77, audit F-13):
    /// the channel fails while the connection is down and works again once
    /// recovery restores it, and no second AMQP connection per vhost is
    /// ever opened. The readiness wait is bounded by the configured
    /// consumer `wait_timeout` so a prolonged outage surfaces as a typed
    /// timeout instead of blocking the caller forever.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unknown broker, a permanently failed
    /// connection, a closed pool, or the readiness timeout elapsing.
    async fn admin_channel(&self, broker: &str) -> Result<Box<dyn PublisherChannel>, ClientError> {
        let coordinator = self.coordinator(broker).await?;
        let wait_timeout = self.config.consumer().wait_timeout;
        let acquisition = async {
            self.wait_for_coordinator_ready(&coordinator, false, || async {
                coordinator.admin_channel().await.ok()
            })
            .await
        };
        tokio::time::timeout(wait_timeout, acquisition)
            .await
            .map_err(|_elapsed| {
                ClientError::transport(&TransportError::connection(format!(
                    "broker '{broker}' did not become ready for the admin operation within {wait_timeout:?}"
                )))
            })?
    }

    /// Waits for a coordinator-owned resource to become available.
    ///
    /// Loops: bail when the client closed, try the operation, then handle the
    /// coordinator's state — `FailedPermanent` and `Closed` are terminal, on
    /// `Ready` the resource may appear at any moment (`spin_on_ready` retries
    /// with a yield so the acquisition deadline stays enforceable while a
    /// recovery generation completes), and any other state can only produce
    /// the resource through a transition: wait for the coordinator to leave
    /// what was just observed so the deadline — and other tasks — can make
    /// progress.
    async fn wait_for_coordinator_ready<T, F, Fut>(
        &self,
        coordinator: &RecoveryCoordinatorHandle,
        spin_on_ready: bool,
        try_op: F,
    ) -> Result<T, ClientError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Option<T>>,
    {
        loop {
            if self.is_closed() {
                return Err(ClientError::closed());
            }
            if let Some(value) = try_op().await {
                return Ok(value);
            }
            match coordinator.state() {
                crate::recovery::ConnectionState::FailedPermanent { .. } => {
                    return Err(ClientError::transport(&TransportError::connection(
                        "broker connection failed permanently",
                    )));
                }
                crate::recovery::ConnectionState::Closed => {
                    return Err(ClientError::closed());
                }
                crate::recovery::ConnectionState::Ready { .. } if spin_on_ready => {
                    tokio::task::yield_now().await;
                }
                observed => {
                    if coordinator.wait_for_transition(&observed).await.is_none() {
                        return Err(ClientError::closed());
                    }
                }
            }
        }
    }

    /// Spawns the broker coordinator (and its connection actor) without
    /// waiting for readiness, so tests can trigger connection establishment
    /// on a runtime and then observe the state off-runtime.
    #[cfg(test)]
    pub(crate) async fn establish_coordinator_for_tests(
        &self,
        broker: &str,
    ) -> Result<(), ClientError> {
        self.coordinator(broker).await.map(drop)
    }

    /// Replaces the cached publisher for `broker` with one whose blind
    /// publish pump is already closed (its intake receiver is gone), keeping
    /// the pool itself open so tests can pin the closed-pump contract of the
    /// blind publish paths.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the broker connection or channel fails.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub async fn install_closed_pump_publisher_for_tests(
        &self,
        broker: &str,
    ) -> Result<(), ClientError> {
        let channel = self.admin_channel(broker).await?;
        let publisher =
            PublisherActor::with_closed_pump_for_tests(channel.into(), self.publisher_config);
        lock(&self.publishers).insert(broker.to_owned(), publisher);
        Ok(())
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

        let topology_plan = TopologyPlan::from_config(&self.config);
        let coordinator_config = RecoveryCoordinatorConfig {
            broker: broker_config,
            policy: crate::recovery::RecoveryPolicy::default(),
            topology_plan,
            publisher_config: self.publisher_config,
            config: self.config.clone(),
            metrics: self.metrics.clone(),
            requested_profiles: self.requested_profiles.clone(),
        };
        let coordinator = RecoveryCoordinator::spawn(&self.transport, coordinator_config);
        if self.commit(generation, &self.coordinators, broker, coordinator.clone()) {
            Ok(coordinator)
        } else {
            let _ = coordinator.close().await;
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
            PublishErrorKind::InvalidRequest
            | PublishErrorKind::Nack
            | PublishErrorKind::Timeout
            | PublishErrorKind::Unconfirmed => ClientErrorKind::Publish,
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
