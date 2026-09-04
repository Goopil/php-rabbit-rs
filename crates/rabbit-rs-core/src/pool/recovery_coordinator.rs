use std::{
    borrow::Cow,
    collections::HashSet,
    error::Error,
    fmt,
    sync::{Arc, Mutex as StdMutex},
};

use tokio::{
    sync::{Mutex, mpsc, oneshot, watch},
    task::JoinHandle,
};

use crate::{
    config::{BrokerConfig, ValidatedConfig},
    consumer::{ConsumerError, ConsumerSet, ConsumerSetHandle, Subscription, SubscriptionPolicy},
    metrics::Metrics,
    metrics::MetricsSnapshot,
    publisher::{
        PublishError, PublisherActor, PublisherConfig, PublisherConnectionEvent, PublisherHandle,
    },
    recovery::{ConnectionState, RecoveryPolicy},
    topology::{TopologyPlan, TopologyReconcileError, TopologyReconciler},
    transport::{ConsumerChannel, PublisherChannel, Transport, TransportError, TransportErrorKind},
};

use super::connection_actor::{ConnectionActor, ConnectionActorClosed, ConnectionActorHandle};

type SharedPublisher = Arc<Mutex<Option<PublisherHandle>>>;
type SharedConsumers = Arc<Mutex<std::collections::HashMap<String, ConsumerSetHandle>>>;
/// Worker profiles explicitly requested through the client pool, shared
/// between the pool and every coordinator.
type RequestedProfiles = Arc<StdMutex<HashSet<String>>>;
/// Serializes consumer establishment between recovery generations and
/// on-demand acquisition so a profile is never established twice.
type EstablishLock = Arc<Mutex<()>>;

/// Orchestrates end-to-end recovery for one broker connection.
///
/// The coordinator spawns a [`ConnectionActor`], subscribes to its state
/// changes, and on each `Ready` generation runs the deterministic recovery
/// sequence: `connection` → `channels` → `topology` → `QoS` → `consumers` →
/// `publisher replay`.
pub struct RecoveryCoordinator;

/// Handle to a running recovery coordinator.
#[derive(Clone)]
pub struct RecoveryCoordinatorHandle {
    actor: ConnectionActorHandle,
    publisher: SharedPublisher,
    consumers: SharedConsumers,
    context: Arc<CoordinatorContext>,
    establish_lock: EstablishLock,
    state: watch::Receiver<ConnectionState>,
    close_tx: mpsc::Sender<CloseCommand>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
}

struct CloseCommand {
    completed: oneshot::Sender<()>,
}

/// Error returned by recovery coordinator operations.
///
/// Each failure carries its typed source so callers can classify it without
/// matching message strings.
#[derive(Debug)]
pub enum CoordinatorError {
    /// The topology plan could not be reconciled on the recovered channel.
    Topology(TopologyReconcileError),
    /// A connection or channel operation failed.
    Transport(TransportError),
    /// The publisher actor rejected the recovered connection event.
    Publisher(PublishError),
    /// A consumer set could not be established.
    Consumer(ConsumerError),
    /// Coordinator-internal condition that is not a transport failure.
    Internal(Cow<'static, str>),
}

impl CoordinatorError {
    fn internal(message: impl Into<Cow<'static, str>>) -> Self {
        Self::Internal(message.into())
    }
}

impl fmt::Display for CoordinatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // `TopologyReconcileError` already identifies the failed step.
            Self::Topology(error) => write!(formatter, "{error}"),
            Self::Transport(error) => write!(formatter, "{error}"),
            Self::Publisher(error) => write!(
                formatter,
                "publisher failed to adopt the recovered channel: {error}"
            ),
            Self::Consumer(error) => write!(formatter, "consumer spawn failed: {error}"),
            Self::Internal(message) => write!(formatter, "{message}"),
        }
    }
}

impl Error for CoordinatorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Topology(error) => Some(error),
            Self::Transport(error) => Some(error),
            Self::Publisher(error) => Some(error),
            Self::Consumer(error) => Some(error),
            Self::Internal(_) => None,
        }
    }
}

/// Configuration for spawning a [`RecoveryCoordinator`].
pub struct RecoveryCoordinatorConfig {
    /// The broker connection configuration.
    pub broker: BrokerConfig,
    /// The recovery backoff policy.
    pub policy: RecoveryPolicy,
    /// The topology plan to reconcile on each connection.
    pub topology_plan: TopologyPlan,
    /// Publisher actor configuration.
    pub publisher_config: PublisherConfig,
    /// The validated application configuration.
    pub config: Arc<ValidatedConfig>,
    /// Shared metrics registry.
    pub metrics: Metrics,
    /// Worker profiles explicitly requested through the client pool. Only
    /// these profiles get consumer channels and `basic_consume` on each
    /// recovery generation; declared-but-unrequested profiles stay dormant.
    pub requested_profiles: RequestedProfiles,
}

impl RecoveryCoordinator {
    /// Spawns a coordinator that manages one broker connection end-to-end.
    ///
    /// The coordinator starts the connection actor immediately and drives
    /// the full recovery sequence on each successful reconnection.
    #[must_use]
    pub fn spawn(
        transport: &Arc<dyn Transport>,
        config: RecoveryCoordinatorConfig,
    ) -> RecoveryCoordinatorHandle {
        let actor = ConnectionActor::spawn_with_dependencies_and_metrics(
            transport.clone(),
            config.broker.clone(),
            config.policy,
            Arc::new(crate::recovery::TokioClock),
            Arc::new(crate::recovery::EqualJitter),
            config.metrics.clone(),
        );

        let state = actor.subscribe();
        let (close_tx, close_rx) = mpsc::channel(1);

        let publisher: SharedPublisher = Arc::new(Mutex::new(None));
        let consumers: SharedConsumers = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let establish_lock: EstablishLock = Arc::new(Mutex::new(()));

        let context = Arc::new(CoordinatorContext {
            broker: config.broker,
            topology_plan: config.topology_plan,
            reconciler: Mutex::new(TopologyReconciler::new()),
            publisher_config: config.publisher_config,
            config: config.config,
            metrics: config.metrics,
            requested_profiles: config.requested_profiles,
        });

        let join = tokio::spawn(run_coordinator(
            actor.clone(),
            context.clone(),
            close_rx,
            publisher.clone(),
            consumers.clone(),
            establish_lock.clone(),
        ));

        RecoveryCoordinatorHandle {
            actor,
            publisher,
            consumers,
            context,
            establish_lock,
            state,
            close_tx,
            join: Arc::new(Mutex::new(Some(join))),
        }
    }
}

struct CoordinatorContext {
    broker: BrokerConfig,
    topology_plan: TopologyPlan,
    /// Shared topology reconciler: the recovery generation and the on-demand
    /// consumer establishment path both apply the plan through it, so
    /// whichever runs first declares the topology and the other observes the
    /// generation as applied (issue #95: declaration before subscription).
    reconciler: Mutex<TopologyReconciler>,
    publisher_config: PublisherConfig,
    config: Arc<ValidatedConfig>,
    metrics: Metrics,
    requested_profiles: RequestedProfiles,
}

impl RecoveryCoordinatorHandle {
    /// Returns the current connection state.
    ///
    /// Reports [`ConnectionState::Closed`] once the coordinator task has
    /// stopped, instead of a stale pre-stop value.
    #[must_use]
    pub fn state(&self) -> ConnectionState {
        let receiver = self.state.clone();
        if receiver.has_changed().is_err() {
            return ConnectionState::Closed;
        }
        receiver.borrow().clone()
    }

    /// Returns a non-blocking view of the shared metrics registry.
    #[must_use]
    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        self.actor.metrics_snapshot()
    }

    /// Waits for a connection state matching the predicate.
    ///
    /// When the coordinator task has stopped, no transition can ever match
    /// again; the wait resolves to [`ConnectionState::Closed`] so callers
    /// observe a terminal state instead of blocking or panicking.
    pub async fn wait_for_state(
        &self,
        predicate: impl Fn(&ConnectionState) -> bool,
    ) -> ConnectionState {
        let mut receiver = self.state.clone();
        loop {
            let current = receiver.borrow().clone();
            if predicate(&current) {
                return current;
            }
            if receiver.changed().await.is_err() {
                return ConnectionState::Closed;
            }
        }
    }

    /// Waits until the connection state leaves `observed` and returns the
    /// state it landed on.
    ///
    /// `observed` is the state the caller saw immediately before deciding to
    /// wait, so a transition that lands between that observation and this
    /// call wakes the caller instead of being lost.
    ///
    /// Returns `None` when the coordinator task has stopped, so callers can
    /// surface a clean closed-pool error instead of blocking forever. Unlike
    /// [`Self::wait_for_state`], this always awaits an actual state change,
    /// which keeps bounded waits (deadlines) enforceable.
    pub async fn wait_for_transition(&self, observed: &ConnectionState) -> Option<ConnectionState> {
        // `wait_for` checks the current value first: a transition that
        // already left `observed` resolves immediately instead of being
        // swallowed by a fresh receiver's mark, which would sleep past the
        // very transition being waited for (issue #111).
        let mut receiver = self.state.clone();
        let next = receiver.wait_for(|state| state != observed).await.ok()?;
        Some(next.clone())
    }

    /// Returns the publisher handle once the initial connection is ready.
    ///
    /// # Errors
    ///
    /// Returns an error if the publisher has not been initialized yet.
    pub async fn publisher(&self) -> Result<PublisherHandle, CoordinatorError> {
        self.publisher
            .lock()
            .await
            .clone()
            .ok_or_else(|| CoordinatorError::internal("publisher is not ready"))
    }

    /// Returns the per-broker consumer set for the given worker profile.
    ///
    /// The set contains only the profile's subscriptions that belong to this
    /// coordinator's broker. Callers that consume from several brokers must
    /// merge the per-broker sets (see `ClientPool::consumer`).
    ///
    /// Only requested profiles are ever established. A profile requested
    /// after the last recovery generation is established on demand by this
    /// call while the connection is ready, so late consumer acquisition does
    /// not wait for a reconnection. When on-demand establishment fails, the
    /// connection loss is reported to the actor so the re-establishment is
    /// retried through the deterministic recovery sequence with backoff,
    /// mirroring the recovery-generation failure path.
    ///
    /// # Errors
    ///
    /// Returns a typed consumer or coordinator error.
    pub async fn consumer(&self, profile: &str) -> Result<ConsumerSetHandle, CoordinatorError> {
        let not_ready =
            || CoordinatorError::internal(format!("consumer profile '{profile}' is not ready"));
        let ConnectionState::Ready { generation } = self.state() else {
            return Err(not_ready());
        };
        if let Some(handle) = self.consumers.lock().await.get(profile).cloned()
            && handle.generation() == generation
        {
            return Ok(handle);
        }
        if !is_requested(&self.context, profile) {
            return Err(not_ready());
        }
        match establish_requested_profile(
            &self.actor,
            &self.context,
            &self.publisher,
            &self.consumers,
            &self.establish_lock,
            profile,
            generation,
        )
        .await
        {
            Ok(()) => self
                .consumers
                .lock()
                .await
                .get(profile)
                .cloned()
                .ok_or_else(not_ready),
            Err(error) => {
                let _ = self
                    .actor
                    .connection_lost(TransportError::connection(format!(
                        "consumer establishment failed: {error}"
                    )))
                    .await;
                Err(error)
            }
        }
    }

    /// Reports connection loss to the underlying actor.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectionActorClosed`] if the actor stopped.
    pub async fn connection_lost(
        &self,
        error: TransportError,
    ) -> Result<(), ConnectionActorClosed> {
        self.actor.connection_lost(error).await
    }

    /// Opens a publisher channel on the actor's active connection for admin
    /// operations (`queue_size`, `purge_queue`).
    ///
    /// The channel is served on the single broker connection the actor owns,
    /// so it fails while the connection is down and works again as soon as
    /// recovery restores it — admin operations never open a second AMQP
    /// connection per vhost.
    ///
    /// # Errors
    ///
    /// Returns a typed transport error when the actor is stopped or the
    /// channel cannot be opened on the active connection.
    pub async fn admin_channel(&self) -> Result<Box<dyn PublisherChannel>, TransportError> {
        self.actor.open_admin_channel().await
    }

    /// Stops the coordinator and the connection actor.
    ///
    /// # Errors
    ///
    /// Returns an error if the coordinator or actor cannot be stopped.
    pub async fn close(&self) -> Result<(), CoordinatorError> {
        let (completed, completion) = oneshot::channel();
        self.close_tx
            .send(CloseCommand { completed })
            .await
            .map_err(|_| CoordinatorError::internal("coordinator is already closed"))?;
        let _ = completion.await;
        let _ = self.actor.close().await;
        if let Some(join) = self.join.lock().await.take() {
            let _ = join.await;
        }
        Ok(())
    }
}

async fn run_coordinator(
    actor: ConnectionActorHandle,
    context: Arc<CoordinatorContext>,
    mut close_rx: mpsc::Receiver<CloseCommand>,
    publisher: SharedPublisher,
    consumers: SharedConsumers,
    establish_lock: EstablishLock,
) {
    let mut state = actor.subscribe();
    if actor.start().await.is_err() {
        // The actor stopped before the start command landed (for example the
        // handle was closed during startup). Terminate the coordinator task
        // cleanly instead of panicking inside a spawned task.
        crate::log::error(
            "recovery_coordinator",
            "connection actor stopped before the coordinator could start it; terminating",
        );
        return;
    }

    let mut last_generation: u64 = 0;

    loop {
        tokio::select! {
            _ = state.changed() => {
                let current = state.borrow().clone();
                match current {
                    ConnectionState::Ready { generation } => {
                        if generation == last_generation {
                            continue;
                        }
                        last_generation = generation;
                        let result = tokio::select! {
                            r = recover_generation(
                                &actor,
                                &context,
                                generation,
                                &publisher,
                                &consumers,
                                &establish_lock,
                            ) => r,
                            close = close_rx.recv() => {
                                if let Some(CloseCommand { completed }) = close {
                                    if let Some(pub_handle) = publisher.lock().await.take() {
                                        let _ = pub_handle.close().await;
                                    }
                                    let consumers_map = std::mem::take(&mut *consumers.lock().await);
                                    for consumer in consumers_map.values() {
                                        let _ = consumer.close().await;
                                    }
                                    let _ = completed.send(());
                                }
                                return;
                            }
                        };
                        if let Err(error) = result {
                            context.metrics.record_recovery_failure();
                            crate::log::warn(
                                "recovery_coordinator",
                                format!("recovery generation {generation} failed: {error}"),
                            );
                            // Roll back so the next Ready re-attempts recovery.
                            last_generation = generation.saturating_sub(1);
                            // Drive the actor back to Recovering so the
                            // deterministic recovery order is re-attempted.
                            let _ = actor
                                .connection_lost(TransportError::connection(format!(
                                    "recovery failed: {error}"
                                )))
                                .await;
                        }
                    }
                    ConnectionState::Recovering { .. } | ConnectionState::Connecting { .. } => {
                        if let Some(pub_handle) = &*publisher.lock().await {
                            let _ = pub_handle
                                .connection_event(PublisherConnectionEvent::Recovering {
                                    generation: last_generation,
                                })
                                .await;
                        }
                    }
                    ConnectionState::FailedPermanent { kind, reason } => {
                        if let Some(pub_handle) = &*publisher.lock().await {
                            let error = transport_error_from_kind(kind, reason);
                            let _ = pub_handle
                                .connection_event(PublisherConnectionEvent::FailedPermanent {
                                    generation: last_generation,
                                    error,
                                })
                                .await;
                        }
                    }
                    ConnectionState::Disconnected | ConnectionState::Closed => {}
                }
            }
            close = close_rx.recv() => {
                if let Some(CloseCommand { completed }) = close {
                    if let Some(pub_handle) = publisher.lock().await.take() {
                        let _ = pub_handle.close().await;
                    }
                    let consumers_map = std::mem::take(&mut *consumers.lock().await);
                    for consumer in consumers_map.values() {
                        let _ = consumer.close().await;
                    }
                    let _ = completed.send(());
                }
                return;
            }
        }
    }
}

async fn recover_generation(
    actor: &ConnectionActorHandle,
    context: &CoordinatorContext,
    generation: u64,
    publisher: &SharedPublisher,
    consumers: &SharedConsumers,
    establish_lock: &EstablishLock,
) -> Result<(), CoordinatorError> {
    // Step 1: Open publisher channel.
    let publisher_channel: Arc<dyn PublisherChannel> =
        Arc::from(actor.open_publisher().await.map_err(|_| {
            CoordinatorError::Transport(TransportError::closed("failed to open publisher channel"))
        })?);

    // Step 2: Reconcile topology (exchanges → queues → bindings).
    //
    // The reconciler is shared with the on-demand consumer establishment
    // path: whichever runs first for this generation declares the topology,
    // so `basic.consume` is always issued after the plan is applied.
    context
        .reconciler
        .lock()
        .await
        .reconcile(
            publisher_channel.as_ref(),
            &context.topology_plan,
            generation,
        )
        .await
        .map_err(CoordinatorError::Topology)?;

    // Step 3: Initialize or update the publisher actor.
    let mut pub_guard = publisher.lock().await;
    if let Some(pub_handle) = pub_guard.as_ref() {
        // A failed adoption (e.g. transient `enable_confirms` rejection on
        // the fresh channel) must fail the generation: the coordinator rolls
        // it back and recovery re-runs on a new generation, instead of
        // leaving the publisher suspended with its generation consumed.
        pub_handle
            .connection_event(PublisherConnectionEvent::Ready {
                generation,
                channel: publisher_channel.clone(),
                topology_restored: true,
            })
            .await
            .map_err(CoordinatorError::Publisher)?;
    } else {
        let delay_strategy = crate::topology::delay::DelayStrategy::compile(&context.config);
        let handle = PublisherActor::spawn_with_delay_strategy_and_metrics(
            publisher_channel.clone(),
            context.publisher_config,
            context.metrics.clone(),
            Some(delay_strategy),
        );
        *pub_guard = Some(handle);
    }
    drop(pub_guard);

    // Step 4: Re-establish consumers (open channels → QoS → basic_consume → update_generation).
    //
    // Only profiles explicitly requested through the client pool are
    // established; declared-but-unrequested profiles stay dormant so a
    // publishing process never holds unacked messages on queues it does not
    // consume from (issue #49). The deterministic recovery order —
    // connection, channels, exchanges, queues, bindings, QoS, then
    // consumers — is preserved.
    let requested = requested_snapshot(context);
    for worker in context.config.worker_profiles() {
        if !requested.contains(&worker.name) {
            continue;
        }
        establish_requested_profile(
            actor,
            context,
            publisher,
            consumers,
            establish_lock,
            &worker.name,
            generation,
        )
        .await?;
    }

    Ok(())
}

/// Establishes one requested worker profile for the given generation.
///
/// The established consumer set handle is registered in `consumers`; callers
/// that need the handle fetch it from the map afterwards. Consumer set
/// handles close on drop, so this function never returns a redundant clone
/// that a caller could drop by accident.
///
/// Does nothing when the profile is not requested or declares no
/// subscriptions on this coordinator's broker. Concurrent establishment
/// attempts (recovery generation and on-demand acquisition) are serialized
/// by `establish_lock` and observe each other's registrations, so a profile
/// is never established twice for one generation.
async fn establish_requested_profile(
    actor: &ConnectionActorHandle,
    context: &CoordinatorContext,
    publisher: &SharedPublisher,
    consumers: &SharedConsumers,
    establish_lock: &EstablishLock,
    profile: &str,
    generation: u64,
) -> Result<(), CoordinatorError> {
    if !is_requested(context, profile) {
        return Ok(());
    }
    let _establishing = establish_lock.lock().await;
    if consumers
        .lock()
        .await
        .get(profile)
        .is_some_and(|handle| handle.generation() == generation)
    {
        return Ok(());
    }

    let Some(worker) = context.config.worker(profile).cloned() else {
        return Ok(());
    };
    let connection_key = crate::pool::ConnectionKey::from_config(&context.config);
    let delay_strategy = crate::topology::delay::DelayStrategy::compile(&context.config);
    let pub_handle = publisher.lock().await.clone();

    let mut subscriptions = Vec::with_capacity(worker.subscriptions.len());
    for (index, sub_config) in worker.subscriptions.iter().enumerate() {
        if sub_config.broker != context.broker.name {
            continue;
        }
        let consumer_channel: Arc<dyn ConsumerChannel> =
            Arc::from(actor.open_consumer().await.map_err(|_| {
                CoordinatorError::Transport(TransportError::closed(
                    "failed to open consumer channel",
                ))
            })?);
        let channel_id = u16::try_from(index.saturating_add(1)).unwrap_or(u16::MAX);
        let mut sub = Subscription::new(
            sub_config.name.clone(),
            connection_key,
            sub_config.queue.clone(),
            consumer_channel,
        )
        .generation(generation)
        .prefetch(sub_config.prefetch)
        .channel_id(channel_id)
        .policy(SubscriptionPolicy::new(
            sub_config.weight,
            sub_config.priority_class,
            sub_config.starvation_after,
        ))
        .early_ack(sub_config.early_ack)
        .no_ack(sub_config.no_ack)
        .max_buffered_bytes(sub_config.max_buffered_bytes)
        .max_attempts(
            context
                .config
                .consumer()
                .max_attempts
                .and_then(std::num::NonZeroU32::new),
        )
        .dead_letter(context.config.dead_letter().is_some())
        .delay_strategy(delay_strategy.clone());

        if let Some(publisher) = &pub_handle {
            sub = sub.delayed_publisher(
                publisher.clone(),
                crate::publisher::Destination::new(
                    sub_config.queue.clone(),
                    sub_config.queue.clone(),
                ),
            );
        }

        subscriptions.push(sub);
    }

    if subscriptions.is_empty() {
        return Ok(());
    }

    // Declaration before subscription (issue #95): this path can win the
    // establish lock while the recovery generation is still mid reconcile —
    // or before it even starts. A fresh quorum queue rejects `basic.consume`
    // with 404 until its `queue.declare` completes, so apply the topology
    // plan here when the generation has not been reconciled yet. The shared
    // reconciler makes the two paths idempotent: whoever runs first
    // declares, the other observes the generation as applied.
    let mut reconciler = context.reconciler.lock().await;
    if !reconciler.is_applied(generation) {
        let channel = actor.open_publisher().await.map_err(|_| {
            CoordinatorError::Transport(TransportError::closed("failed to open topology channel"))
        })?;
        let result = reconciler
            .reconcile(channel.as_ref(), &context.topology_plan, generation)
            .await;
        let _ = channel.close().await;
        result.map_err(CoordinatorError::Topology)?;
    }
    drop(reconciler);

    let consumer = ConsumerSet::spawn_with_metrics(subscriptions, context.metrics.clone())
        .await
        .map_err(CoordinatorError::Consumer)?;

    let mut guard = consumers.lock().await;
    if let Some(old) = guard.insert(profile.to_owned(), consumer) {
        let _ = old.close().await;
    }
    drop(guard);

    Ok(())
}

fn is_requested(context: &CoordinatorContext, profile: &str) -> bool {
    context
        .requested_profiles
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(profile)
}

fn requested_snapshot(context: &CoordinatorContext) -> HashSet<String> {
    context
        .requested_profiles
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn transport_error_from_kind(kind: TransportErrorKind, reason: String) -> TransportError {
    match kind {
        TransportErrorKind::Authentication => TransportError::authentication(reason),
        TransportErrorKind::Configuration => TransportError::config(reason),
        TransportErrorKind::Connection => TransportError::connection(reason),
        TransportErrorKind::Protocol => TransportError::protocol(reason),
        TransportErrorKind::Closed => TransportError::closed(reason),
    }
}
