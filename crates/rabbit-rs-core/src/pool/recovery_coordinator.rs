use std::{error::Error, fmt, sync::Arc};

use tokio::{
    sync::{Mutex, mpsc, oneshot, watch},
    task::JoinHandle,
};

use crate::{
    config::{BrokerConfig, ValidatedConfig},
    consumer::{ConsumerHandle, ConsumerSet, Subscription, SubscriptionPolicy},
    metrics::Metrics,
    publisher::{PublisherActor, PublisherConfig, PublisherConnectionEvent, PublisherHandle},
    recovery::{ConnectionState, RecoveryPolicy},
    topology::{TopologyPlan, TopologyReconciler},
    transport::{ConsumerChannel, PublisherChannel, Transport, TransportError, TransportErrorKind},
};

use super::connection_actor::{ConnectionActor, ConnectionActorClosed, ConnectionActorHandle};

type SharedPublisher = Arc<Mutex<Option<PublisherHandle>>>;
type SharedConsumers = Arc<Mutex<std::collections::HashMap<String, ConsumerHandle>>>;

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
    state: watch::Receiver<ConnectionState>,
    close_tx: mpsc::Sender<CloseCommand>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
}

struct CloseCommand {
    completed: oneshot::Sender<()>,
}

/// Error returned by recovery coordinator operations.
#[derive(Debug)]
pub struct CoordinatorError {
    message: String,
}

impl CoordinatorError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CoordinatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CoordinatorError {}

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
        Self::spawn_with_dependencies(
            transport,
            config,
            Arc::new(crate::recovery::TokioClock),
            Arc::new(crate::recovery::EqualJitter),
        )
    }

    /// Spawns a coordinator with deterministic time and jitter dependencies.
    #[must_use]
    pub fn spawn_with_dependencies(
        transport: &Arc<dyn Transport>,
        config: RecoveryCoordinatorConfig,
        clock: Arc<dyn crate::recovery::Clock>,
        jitter: Arc<dyn crate::recovery::JitterSource>,
    ) -> RecoveryCoordinatorHandle {
        let actor = ConnectionActor::spawn_with_dependencies_and_metrics(
            transport.clone(),
            config.broker.clone(),
            config.policy,
            clock,
            jitter,
            config.metrics.clone(),
        );

        let state = actor.subscribe();
        let (close_tx, close_rx) = mpsc::channel(1);

        let publisher: SharedPublisher = Arc::new(Mutex::new(None));
        let consumers: SharedConsumers = Arc::new(Mutex::new(std::collections::HashMap::new()));

        let context = CoordinatorContext {
            broker: config.broker,
            topology_plan: config.topology_plan,
            publisher_config: config.publisher_config,
            config: config.config,
            metrics: config.metrics,
        };

        let join = tokio::spawn(run_coordinator(
            actor.clone(),
            context,
            close_rx,
            publisher.clone(),
            consumers.clone(),
        ));

        RecoveryCoordinatorHandle {
            actor,
            publisher,
            consumers,
            state,
            close_tx,
            join: Arc::new(Mutex::new(Some(join))),
        }
    }
}

struct CoordinatorContext {
    broker: BrokerConfig,
    topology_plan: TopologyPlan,
    publisher_config: PublisherConfig,
    config: Arc<ValidatedConfig>,
    metrics: Metrics,
}

impl RecoveryCoordinatorHandle {
    /// Returns the current connection state.
    #[must_use]
    pub fn state(&self) -> ConnectionState {
        self.state.borrow().clone()
    }

    /// Waits for a connection state matching the predicate.
    ///
    /// # Panics
    ///
    /// Panics if the underlying coordinator task has stopped.
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
            receiver
                .changed()
                .await
                .expect("coordinator actor is alive");
        }
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
            .ok_or_else(|| CoordinatorError::new("publisher is not ready"))
    }

    /// Returns a consumer handle for the given worker profile.
    ///
    /// # Errors
    ///
    /// Returns a typed consumer or coordinator error.
    pub async fn consumer(&self, profile: &str) -> Result<ConsumerHandle, CoordinatorError> {
        if let Some(handle) = self.consumers.lock().await.get(profile).cloned() {
            return Ok(handle);
        }
        Err(CoordinatorError::new(format!(
            "consumer profile '{profile}' is not ready"
        )))
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
            .map_err(|_| CoordinatorError::new("coordinator is already closed"))?;
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
    context: CoordinatorContext,
    mut close_rx: mpsc::Receiver<CloseCommand>,
    publisher: SharedPublisher,
    consumers: SharedConsumers,
) {
    let mut state = actor.subscribe();
    actor.start().await.expect("connection actor started");

    let mut reconciler = TopologyReconciler::new();
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
                                &mut reconciler,
                                generation,
                                &publisher,
                                &consumers,
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
                            eprintln!("recovery generation {generation} failed: {error}");
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
    reconciler: &mut TopologyReconciler,
    generation: u64,
    publisher: &SharedPublisher,
    consumers: &SharedConsumers,
) -> Result<(), CoordinatorError> {
    // Step 1: Open publisher channel.
    let publisher_channel: Arc<dyn PublisherChannel> = Arc::from(
        actor
            .open_publisher()
            .await
            .map_err(|_| CoordinatorError::new("failed to open publisher channel"))?,
    );

    // Step 2: Reconcile topology (exchanges → queues → bindings).
    reconciler
        .reconcile(
            publisher_channel.as_ref(),
            &context.topology_plan,
            generation,
        )
        .await
        .map_err(|error| {
            CoordinatorError::new(format!("topology reconciliation failed: {error}"))
        })?;

    // Step 3: Initialize or update the publisher actor.
    let mut pub_guard = publisher.lock().await;
    if let Some(pub_handle) = pub_guard.as_ref() {
        let _ = pub_handle
            .connection_event(PublisherConnectionEvent::Ready {
                generation,
                channel: publisher_channel.clone(),
                topology_restored: true,
            })
            .await;
    } else {
        let delay_strategy = compile_delay_strategy(&context.config);
        let handle = PublisherActor::spawn_with_delay_strategy_and_metrics(
            publisher_channel.clone(),
            context.publisher_config,
            context.metrics.clone(),
            delay_strategy,
        );
        *pub_guard = Some(handle);
    }
    drop(pub_guard);

    // Step 4: Re-establish consumers (open channels → QoS → basic_consume → update_generation).
    let connection_key = crate::pool::ConnectionKey::from_config(&context.config);
    let delay_strategy = compile_delay_strategy(&context.config);

    let pub_handle = publisher.lock().await.clone();
    for worker in context.config.worker_profiles() {
        let mut subscriptions = Vec::with_capacity(worker.subscriptions.len());
        for (index, sub_config) in worker.subscriptions.iter().enumerate() {
            if sub_config.broker != context.broker.name {
                continue;
            }
            let consumer_channel: Arc<dyn ConsumerChannel> = Arc::from(
                actor
                    .open_consumer()
                    .await
                    .map_err(|_| CoordinatorError::new("failed to open consumer channel"))?,
            );
            let channel_id = u16::try_from(index.saturating_add(1)).unwrap_or(u16::MAX);
            let mut sub = Subscription::new(
                sub_config.name.clone(),
                connection_key,
                sub_config.queue.clone(),
                consumer_channel,
            )
            .prefetch(sub_config.prefetch)
            .channel_id(channel_id)
            .policy(SubscriptionPolicy::new(
                sub_config.weight,
                sub_config.priority_class,
                sub_config.starvation_after,
            ))
            .early_ack(sub_config.early_ack)
            .max_buffered_bytes(sub_config.max_buffered_bytes)
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
            continue;
        }

        let max_in_flight = usize::from(worker.scheduler.max_in_flight);
        let consumer =
            ConsumerSet::spawn_with_metrics(subscriptions, max_in_flight, context.metrics.clone())
                .await
                .map_err(|error| {
                    CoordinatorError::new(format!("consumer spawn failed: {error}"))
                })?;

        let mut guard = consumers.lock().await;
        if let Some(old) = guard.insert(worker.name.clone(), consumer) {
            let _ = old.close().await;
        }
        drop(guard);
    }

    Ok(())
}

fn transport_error_from_kind(kind: TransportErrorKind, reason: String) -> TransportError {
    match kind {
        TransportErrorKind::Authentication => TransportError::authentication(reason),
        TransportErrorKind::Connection => TransportError::connection(reason),
        TransportErrorKind::Protocol => TransportError::protocol(reason),
        TransportErrorKind::Closed => TransportError::closed(reason),
    }
}

/// Compiles a delay strategy from configuration for the publisher actor.
///
/// In plugin mode the strategy is `Plugin`; in TTL mode it is `TtlBuckets`.
/// In auto mode we default to `Plugin` for the publisher path — the consumer
/// path performs runtime detection via `DelayStrategyResolver`.
fn compile_delay_strategy(config: &ValidatedConfig) -> crate::topology::delay::DelayStrategy {
    use crate::{
        config::DelayMode,
        topology::delay::{DelayStrategy, TtlBucketPlan},
    };
    let delay = config.delay();
    match delay.mode {
        DelayMode::Plugin | DelayMode::Auto => DelayStrategy::Plugin,
        DelayMode::Ttl => {
            TtlBucketPlan::compile(delay).map_or(DelayStrategy::Plugin, DelayStrategy::TtlBuckets)
        }
    }
}
