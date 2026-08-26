use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::{self, Future},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    time,
};

use crate::{
    metrics::{Metrics, MetricsSnapshot},
    topology::delay::DelayStrategy,
    transport::{
        PublishConfirmation, PublishProperties as TransportProperties, PublishReceipt,
        PublishRequest as TransportRequest, PublisherChannel, TransportError, TransportResult,
    },
};

use super::{
    ByteBudget, PublishError, PublishErrorKind, PublishOutcome, PublishRequest, PublishWaiter,
    PublisherConfig, PublisherConnectionEvent, ReturnInfo, confirms::ConfirmLedger,
    delay::DelayRouter,
};

pub struct PublisherActor;

impl PublisherActor {
    #[must_use]
    pub fn spawn(channel: Arc<dyn PublisherChannel>, config: PublisherConfig) -> PublisherHandle {
        Self::spawn_with_metrics(channel, config, Metrics::default())
    }

    #[must_use]
    pub fn spawn_with_metrics(
        channel: Arc<dyn PublisherChannel>,
        config: PublisherConfig,
        metrics: Metrics,
    ) -> PublisherHandle {
        Self::spawn_inner(channel, config, metrics, None)
    }

    /// Spawns the actor with delay routing enabled.
    ///
    /// When `delay_strategy` is `Some`, the actor routes messages with `delay_ms > 0`
    /// through the `DelayRouter` before publishing.
    #[must_use]
    pub fn spawn_with_delay_strategy(
        channel: Arc<dyn PublisherChannel>,
        config: PublisherConfig,
        delay_strategy: DelayStrategy,
    ) -> PublisherHandle {
        Self::spawn_inner(channel, config, Metrics::default(), Some(delay_strategy))
    }

    /// Spawns the actor with delay routing and shared metrics.
    #[must_use]
    pub fn spawn_with_delay_strategy_and_metrics(
        channel: Arc<dyn PublisherChannel>,
        config: PublisherConfig,
        metrics: Metrics,
        delay_strategy: DelayStrategy,
    ) -> PublisherHandle {
        Self::spawn_inner(channel, config, metrics, Some(delay_strategy))
    }

    #[must_use]
    fn spawn_inner(
        channel: Arc<dyn PublisherChannel>,
        config: PublisherConfig,
        metrics: Metrics,
        delay_strategy: Option<DelayStrategy>,
    ) -> PublisherHandle {
        let capacity = Arc::new(Semaphore::new(config.buffer_capacity.max(1)));
        let byte_budget = Arc::new(ByteBudget::new(config.max_buffered_bytes));
        let (commands, receiver) = mpsc::channel(config.buffer_capacity.max(1));
        tokio::spawn(run_actor(
            channel,
            config,
            receiver,
            metrics.clone(),
            delay_strategy,
            byte_budget.clone(),
        ));
        PublisherHandle {
            commands,
            capacity,
            byte_budget,
            metrics,
            confirm_timeout: config.confirm_timeout,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PublisherHandle {
    commands: mpsc::Sender<Command>,
    capacity: Arc<Semaphore>,
    byte_budget: Arc<ByteBudget>,
    metrics: Metrics,
    confirm_timeout: Duration,
}

impl PublisherHandle {
    #[must_use]
    pub const fn confirm_timeout(&self) -> Duration {
        self.confirm_timeout
    }

    /// Returns the number of available publisher capacity permits.
    ///
    /// Combined with the configured `buffer_capacity`, this yields the current
    /// in-flight publication count: `buffer_capacity - available_permits()`.
    #[must_use]
    pub fn available_permits(&self) -> usize {
        self.capacity.available_permits()
    }

    /// Enqueues a publish while retaining one global capacity permit until its terminal outcome.
    ///
    /// # Errors
    ///
    /// Returns [`PublishErrorKind::Backpressure`] when all global permits are
    /// retained or [`PublishErrorKind::Closed`] when the actor has stopped.
    pub fn try_publish(&self, request: PublishRequest) -> Result<PublishWaiter, PublishError> {
        let payload_bytes = u64::try_from(request.payload.len()).unwrap_or(u64::MAX);

        if !self.byte_budget.try_reserve(payload_bytes) {
            self.metrics.record_backpressure();
            self.metrics
                .record_backpressure_duration(Duration::from_millis(2));
            return Err(PublishError::new(
                PublishErrorKind::Backpressure,
                "publisher byte budget is exhausted",
            ));
        }

        let permit = self.capacity.clone().try_acquire_owned().map_err(|_| {
            self.byte_budget.release(payload_bytes);
            self.metrics.record_backpressure();
            self.metrics
                .record_backpressure_duration(Duration::from_millis(2));
            PublishError::new(
                PublishErrorKind::Backpressure,
                "publisher global capacity is exhausted",
            )
        })?;
        let (completion, receiver) = oneshot::channel();
        let command = Command::Publish(Box::new(RetainedPublish {
            request,
            completion,
            accepted_at: Instant::now(),
            _permit: permit,
            sequence: 0,
            payload_bytes,
        }));

        match self.commands.try_send(command) {
            Ok(()) => {
                self.metrics.record_publish();
                Ok(PublishWaiter::new(receiver))
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.byte_budget.release(payload_bytes);
                self.metrics.record_backpressure();
                self.metrics
                    .record_backpressure_duration(Duration::from_millis(2));
                Err(PublishError::new(
                    PublishErrorKind::Backpressure,
                    "publisher command buffer is full",
                ))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.byte_budget.release(payload_bytes);
                Err(PublishError::new(
                    PublishErrorKind::Closed,
                    "publisher actor is closed",
                ))
            }
        }
    }

    #[must_use]
    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Delivers an ordered connection lifecycle event to the publisher actor.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a closed actor, stale generation, missing
    /// topology reconciliation or channel initialization failure.
    pub async fn connection_event(
        &self,
        event: PublisherConnectionEvent,
    ) -> Result<(), PublishError> {
        let (completed, completion) = oneshot::channel();
        self.commands
            .send(Command::ConnectionEvent(event, completed))
            .await
            .map_err(|_| {
                PublishError::new(PublishErrorKind::Closed, "publisher actor is closed")
            })?;
        completion.await.unwrap_or_else(|_| {
            Err(PublishError::new(
                PublishErrorKind::Closed,
                "publisher actor stopped while handling a connection event",
            ))
        })
    }

    /// Suspends the current generation without resolving ambiguous publishes.
    ///
    /// # Errors
    ///
    /// Returns [`PublishErrorKind::Closed`] if the actor is no longer running.
    pub async fn connection_lost(&self) -> Result<(), PublishError> {
        self.connection_event(PublisherConnectionEvent::Recovering { generation: 0 })
            .await
    }

    /// Stops the actor and wakes every retained waiter.
    ///
    /// # Errors
    ///
    /// Returns [`PublishErrorKind::Closed`] if the actor already stopped.
    pub async fn close(&self) -> Result<(), PublishError> {
        let (completed, completion) = oneshot::channel();
        self.commands
            .send(Command::Close(completed))
            .await
            .map_err(|_| {
                PublishError::new(PublishErrorKind::Closed, "publisher actor is closed")
            })?;
        completion.await.map_err(|_| {
            PublishError::new(
                PublishErrorKind::Closed,
                "publisher actor stopped during shutdown",
            )
        })
    }
}

enum Command {
    Publish(Box<RetainedPublish>),
    ConnectionEvent(
        PublisherConnectionEvent,
        oneshot::Sender<Result<(), PublishError>>,
    ),
    Close(oneshot::Sender<()>),
}

struct RetainedPublish {
    request: PublishRequest,
    completion: oneshot::Sender<Result<PublishOutcome, PublishError>>,
    accepted_at: Instant,
    _permit: OwnedSemaphorePermit,
    sequence: u64,
    payload_bytes: u64,
}

struct InFlightPublish {
    retained: RetainedPublish,
    generation: u64,
}

enum ConfirmationResult {
    Completed(TransportResult<PublishConfirmation>),
    TimedOut,
}

type ConfirmationFuture = Pin<Box<dyn Future<Output = (u64, u64, ConfirmationResult)> + Send>>;

/// The boxed future returned by [`PublisherChannel::publish`].
type PublishResultFuture =
    Pin<Box<dyn Future<Output = TransportResult<Box<dyn PublishReceipt>>> + Send>>;

/// Wraps a publish future and attaches its sequence number, separating the
/// sequence-tagging concern from the future boxing.
struct TaggedFuture {
    fut: PublishResultFuture,
    sequence: u64,
}

impl Future for TaggedFuture {
    type Output = (u64, TransportResult<Box<dyn PublishReceipt>>);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.fut.as_mut().poll(cx) {
            Poll::Ready(result) => Poll::Ready((self.sequence, result)),
            Poll::Pending => Poll::Pending,
        }
    }
}

type PublishFuture = TaggedFuture;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Ready,
    Suspended,
    FailedPermanent,
}

struct ActorState {
    config: PublisherConfig,
    phase: Phase,
    generation: u64,
    channel: Option<Arc<dyn PublisherChannel>>,
    replay: VecDeque<RetainedPublish>,
    publishing: HashMap<u64, RetainedPublish>,
    ledger: ConfirmLedger<InFlightPublish>,
    confirmations: FuturesUnordered<ConfirmationFuture>,
    publish_in_flight: FuturesUnordered<PublishFuture>,
    sequence: u64,
    permanent_error: Option<PublishError>,
    metrics: Metrics,
    delay_strategy: Option<DelayStrategy>,
    declared_ttl_queues: HashSet<Arc<str>>,
    byte_budget: Arc<ByteBudget>,
}

impl ActorState {
    fn new(
        channel: Arc<dyn PublisherChannel>,
        config: PublisherConfig,
        metrics: Metrics,
        delay_strategy: Option<DelayStrategy>,
        byte_budget: Arc<ByteBudget>,
    ) -> Self {
        Self {
            config,
            phase: Phase::Ready,
            generation: 1,
            channel: Some(channel),
            replay: VecDeque::new(),
            publishing: HashMap::new(),
            ledger: ConfirmLedger::with_capacity(config.buffer_capacity),
            confirmations: FuturesUnordered::new(),
            publish_in_flight: FuturesUnordered::new(),
            sequence: 0,
            permanent_error: None,
            metrics,
            delay_strategy,
            declared_ttl_queues: HashSet::new(),
            byte_budget,
        }
    }

    fn next_deadline(&self) -> Option<time::Instant> {
        match self.phase {
            Phase::Ready | Phase::FailedPermanent => None,
            Phase::Suspended => self
                .replay
                .iter()
                .map(|pending| pending.request.deadline)
                .min(),
        }
    }

    fn suspend(&mut self, generation: u64) {
        if generation > 0 {
            self.generation = self.generation.max(generation);
        }
        self.phase = Phase::Suspended;
        self.channel = None;
        let mut all: Vec<RetainedPublish> = std::mem::take(&mut self.replay).into_iter().collect();
        all.extend(self.publishing.drain().map(|(_, retained)| retained));
        all.extend(self.ledger.drain().map(|in_flight| in_flight.retained));
        all.sort_by_key(|retained| retained.sequence);
        let replay_count = all.len();
        self.replay = all.into_iter().collect();
        for _ in 0..replay_count {
            self.metrics.record_replay();
        }
        self.metrics
            .record_replay_depth(u64::try_from(self.replay.len()).unwrap_or(u64::MAX));
        self.confirmations = FuturesUnordered::new();
        self.publish_in_flight = FuturesUnordered::new();
    }

    fn fail_all(&mut self, error: &PublishError) {
        for retained in self.replay.drain(..) {
            self.byte_budget.release(retained.payload_bytes);
            complete_error(retained, error.clone());
        }
        for in_flight in self.ledger.drain() {
            self.byte_budget.release(in_flight.retained.payload_bytes);
            complete_error(in_flight.retained, error.clone());
        }
        for (_, retained) in self.publishing.drain() {
            self.byte_budget.release(retained.payload_bytes);
            complete_error(retained, error.clone());
        }
        self.confirmations = FuturesUnordered::new();
        self.publish_in_flight = FuturesUnordered::new();
        record_publisher_metrics(self);
    }

    fn expire_replay(&mut self) {
        let now = time::Instant::now();
        let mut retained = VecDeque::new();
        while let Some(pending) = self.replay.pop_front() {
            if pending.request.deadline <= now {
                self.byte_budget.release(pending.payload_bytes);
                complete_error(
                    pending,
                    PublishError::new(
                        PublishErrorKind::Timeout,
                        "publish deadline expired during connection recovery",
                    ),
                );
            } else {
                retained.push_back(pending);
            }
        }
        self.replay = retained;
        self.metrics
            .record_replay_depth(u64::try_from(self.replay.len()).unwrap_or(u64::MAX));
    }
}

async fn run_actor(
    initial_channel: Arc<dyn PublisherChannel>,
    config: PublisherConfig,
    mut commands: mpsc::Receiver<Command>,
    metrics: Metrics,
    delay_strategy: Option<DelayStrategy>,
    byte_budget: Arc<ByteBudget>,
) {
    let mut state = ActorState::new(
        initial_channel,
        config,
        metrics,
        delay_strategy,
        byte_budget,
    );
    if state.config.confirms
        && let Some(channel) = &state.channel
        && let Err(error) = channel.enable_confirms().await
    {
        let error = transport_publish_error(&error);
        state.phase = Phase::FailedPermanent;
        state.permanent_error = Some(error);
    }

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Publish(retained)) => {
                    accept_publish(&mut state, *retained).await;
                }
                Some(Command::ConnectionEvent(event, completed)) => {
                    let result = handle_connection_event(&mut state, event).await;
                    let _ = completed.send(result);
                }
                Some(Command::Close(completed)) => {
                    let error = PublishError::new(
                        PublishErrorKind::Closed,
                        "publisher actor was explicitly closed",
                    );
                    // Drain the publishing registry so pending confirmations
                    // are resolved with a terminal error before closing.
                    for (_, retained) in state.publishing.drain() {
                        state.byte_budget.release(retained.payload_bytes);
                        complete_error(retained, error.clone());
                    }
                    state.fail_all(&error);
                    state.publish_in_flight = FuturesUnordered::new();
                    if let Some(channel) = state.channel.take() {
                        let _ = tokio::time::timeout(
                            Duration::from_secs(2),
                            channel.close(),
                        )
                        .await;
                    }
                    let _ = completed.send(());
                    return;
                }
                None => {
                    let error = PublishError::new(
                        PublishErrorKind::Closed,
                        "all publisher handles were dropped",
                    );
                    state.fail_all(&error);
                    return;
                }
            },
            () = wait_for_deadline(state.next_deadline()) => {
                match state.phase {
                    Phase::Ready | Phase::FailedPermanent => {}
                    Phase::Suspended => state.expire_replay(),
                }
            }
            confirmation = state.confirmations.next(), if !state.confirmations.is_empty() => {
                if let Some((sequence, generation, result)) = confirmation {
                    resolve_confirmation(&mut state, sequence, generation, result);
                }
            }
            Some((sequence, result)) = state.publish_in_flight.next(), if !state.publish_in_flight.is_empty() => {
                handle_publish_completion(&mut state, sequence, result);
            }
        }
    }
}

async fn accept_publish(state: &mut ActorState, retained: RetainedPublish) {
    match state.phase {
        Phase::Ready => {
            let pending = VecDeque::from([retained]);
            publish_queue(state, pending).await;
        }
        Phase::Suspended => {
            state.replay.push_back(retained);
            state
                .metrics
                .record_replay_depth(u64::try_from(state.replay.len()).unwrap_or(u64::MAX));
        }
        Phase::FailedPermanent => {
            state.byte_budget.release(retained.payload_bytes);
            complete_error(
                retained,
                state.permanent_error.clone().unwrap_or_else(|| {
                    PublishError::new(
                        PublishErrorKind::Transport,
                        "publisher connection failed permanently",
                    )
                }),
            );
        }
    }
}

async fn handle_connection_event(
    state: &mut ActorState,
    event: PublisherConnectionEvent,
) -> Result<(), PublishError> {
    match event {
        PublisherConnectionEvent::Recovering { generation } => {
            state.suspend(generation);
            Ok(())
        }
        PublisherConnectionEvent::Ready {
            generation,
            channel,
            topology_restored,
        } => {
            if !topology_restored {
                return Err(PublishError::new(
                    PublishErrorKind::Transport,
                    "publisher topology is not restored",
                ));
            }
            if generation < state.generation {
                return Err(PublishError::new(
                    PublishErrorKind::Transport,
                    "publisher recovery generation is stale",
                ));
            }
            if state.config.confirms {
                channel
                    .enable_confirms()
                    .await
                    .map_err(|error| transport_publish_error(&error))?;
            }
            state.generation = generation;
            state.channel = Some(channel);
            state.phase = Phase::Ready;
            state.permanent_error = None;
            flush_replay(state).await;
            Ok(())
        }
        PublisherConnectionEvent::FailedPermanent { generation, error } => {
            state.generation = state.generation.max(generation);
            let error = transport_publish_error(&error);
            state.fail_all(&error);
            state.phase = Phase::FailedPermanent;
            state.channel = None;
            state.permanent_error = Some(error);
            Ok(())
        }
    }
}

async fn flush_replay(state: &mut ActorState) {
    state.expire_replay();
    let pending = std::mem::take(&mut state.replay);
    publish_queue(state, pending).await;
}

async fn publish_queue(state: &mut ActorState, mut pending: VecDeque<RetainedPublish>) {
    while let Some(mut retained) = pending.pop_front() {
        if retained.request.deadline <= time::Instant::now() {
            state.byte_budget.release(retained.payload_bytes);
            complete_error(
                retained,
                PublishError::new(PublishErrorKind::Timeout, "publish deadline expired"),
            );
            continue;
        }

        let Some(channel) = state.channel.clone() else {
            state.replay.push_back(retained);
            state.replay.extend(pending);
            state
                .metrics
                .record_replay_depth(u64::try_from(state.replay.len()).unwrap_or(u64::MAX));
            record_publisher_metrics(state);
            return;
        };

        match ensure_delay_topology(state, &channel, &retained).await {
            DelayTopologyOutcome::Ready => {}
            DelayTopologyOutcome::Suspend => {
                state.replay.push_back(retained);
                state.replay.extend(pending);
                state.suspend(state.generation);
                return;
            }
            DelayTopologyOutcome::Failed(error) => {
                state.byte_budget.release(retained.payload_bytes);
                complete_error(retained, error);
                continue;
            }
        }

        state.sequence = state.sequence.saturating_add(1);
        let sequence = state.sequence;
        retained.sequence = sequence;

        let request = into_transport_request(
            &retained.request,
            state.delay_strategy.as_ref(),
            state.config.mandatory,
        );

        state.publishing.insert(sequence, retained);

        record_publisher_metrics(state);

        let channel_for_pub = Arc::clone(&channel);
        let mut tagged = TaggedFuture {
            fut: Box::pin(async move { channel_for_pub.publish(request).await }),
            sequence,
        };

        match Pin::new(&mut tagged).now_or_never() {
            Some((seq, result)) => {
                drop(tagged);
                handle_publish_completion(state, seq, result);
                if matches!(state.phase, Phase::Suspended) {
                    state.replay.extend(pending);
                    return;
                }
            }
            None => {
                state.publish_in_flight.push(tagged);
            }
        }
    }
}

fn handle_publish_completion(
    state: &mut ActorState,
    sequence: u64,
    result: TransportResult<Box<dyn PublishReceipt>>,
) {
    let Some(retained) = state.publishing.remove(&sequence) else {
        return;
    };

    record_publisher_metrics(state);

    match result {
        Ok(receipt) => {
            if state.config.confirms {
                let generation = state.generation;
                let deadline = retained
                    .request
                    .deadline
                    .min(time::Instant::now() + state.config.confirm_timeout);
                state.ledger.insert(
                    sequence,
                    InFlightPublish {
                        retained,
                        generation,
                    },
                );
                state.confirmations.push(Box::pin(async move {
                    let result = match time::timeout_at(deadline, receipt.wait()).await {
                        Ok(result) => ConfirmationResult::Completed(result),
                        Err(_) => ConfirmationResult::TimedOut,
                    };
                    (sequence, generation, result)
                }));
            } else {
                state.byte_budget.release(retained.payload_bytes);
                record_publisher_metrics(state);
                let _ = retained.completion.send(Ok(PublishOutcome::Confirmed {
                    message_id: retained.request.properties.message_id.clone(),
                }));
            }
        }
        Err(error) if error.is_recoverable() => {
            state.replay.push_back(retained);
            state.publish_in_flight.clear();
            state.metrics.record_replay();
            state
                .metrics
                .record_replay_depth(u64::try_from(state.replay.len()).unwrap_or(u64::MAX));
            state.suspend(state.generation);
        }
        Err(error) => {
            state.byte_budget.release(retained.payload_bytes);
            record_publisher_metrics(state);
            complete_error(retained, transport_publish_error(&error));
        }
    }
}

fn into_transport_request(
    request: &PublishRequest,
    delay_strategy: Option<&DelayStrategy>,
    mandatory: bool,
) -> TransportRequest {
    let delay_ms = request.properties.delay_ms.unwrap_or(0);

    if delay_ms > 0
        && let Some(strategy) = delay_strategy
        && let Ok(route) = DelayRouter::route(
            strategy,
            &request.destination,
            i64::try_from(delay_ms).unwrap_or(i64::MAX),
        )
    {
        let properties = TransportProperties {
            content_type: request
                .properties
                .content_type
                .as_ref()
                .map(|ct| ct.as_ref().to_owned()),
            correlation_id: request
                .properties
                .correlation_id
                .as_ref()
                .map(|ci| ci.as_ref().to_owned()),
            message_id: Some(request.properties.message_id.as_ref().to_owned()),
            delay_ms: route.queue.is_none().then_some(route.delay_ms),
            headers: request.properties.headers.clone(),
            persistent: true,
        };

        return TransportRequest {
            exchange: route.exchange,
            routing_key: route.routing_key,
            payload: request.payload.clone(),
            mandatory,
            properties,
        };
    }

    TransportRequest {
        exchange: request.destination.exchange.clone(),
        routing_key: request.destination.routing_key.clone(),
        payload: request.payload.clone(),
        mandatory,
        properties: TransportProperties {
            content_type: request
                .properties
                .content_type
                .as_ref()
                .map(|ct| ct.as_ref().to_owned()),
            correlation_id: request
                .properties
                .correlation_id
                .as_ref()
                .map(|ci| ci.as_ref().to_owned()),
            message_id: Some(request.properties.message_id.as_ref().to_owned()),
            delay_ms: request.properties.delay_ms,
            headers: request.properties.headers.clone(),
            persistent: true,
        },
    }
}

fn resolve_confirmation(
    state: &mut ActorState,
    sequence: u64,
    generation: u64,
    result: ConfirmationResult,
) {
    let Some(in_flight) = state.ledger.remove(sequence) else {
        return;
    };
    if in_flight.generation != generation {
        state.ledger.insert(sequence, in_flight);
        return;
    }

    if matches!(
        &result,
        ConfirmationResult::Completed(Ok(
            PublishConfirmation::Ack(_) | PublishConfirmation::Nack(_)
        ))
    ) {
        state
            .metrics
            .record_confirmation(in_flight.retained.accepted_at.elapsed());
    }

    match result {
        ConfirmationResult::TimedOut => {
            state.byte_budget.release(in_flight.retained.payload_bytes);
            record_publisher_metrics(state);
            complete_error(
                in_flight.retained,
                PublishError::new(
                    PublishErrorKind::Timeout,
                    "publisher confirmation timed out",
                ),
            );
        }
        ConfirmationResult::Completed(Err(error)) if error.is_recoverable() => {
            state.replay.push_back(in_flight.retained);
            state.metrics.record_replay();
            state
                .metrics
                .record_replay_depth(u64::try_from(state.replay.len()).unwrap_or(u64::MAX));
            state.suspend(generation);
        }
        ConfirmationResult::Completed(Err(error)) => {
            state.byte_budget.release(in_flight.retained.payload_bytes);
            record_publisher_metrics(state);
            complete_error(in_flight.retained, transport_publish_error(&error));
        }
        ConfirmationResult::Completed(Ok(
            PublishConfirmation::Ack(Some(returned)) | PublishConfirmation::Nack(Some(returned)),
        )) => {
            state.byte_budget.release(in_flight.retained.payload_bytes);
            record_publisher_metrics(state);
            state.metrics.record_return();
            let message_id = in_flight.retained.request.properties.message_id.clone();
            complete_outcome(
                in_flight.retained,
                PublishOutcome::Returned {
                    message_id,
                    reply: ReturnInfo {
                        code: returned.reply_code,
                        text: returned.reply_text,
                        exchange: returned.exchange,
                        routing_key: returned.routing_key,
                    },
                },
            );
        }
        ConfirmationResult::Completed(Ok(PublishConfirmation::Ack(None))) => {
            state.byte_budget.release(in_flight.retained.payload_bytes);
            record_publisher_metrics(state);
            let message_id = in_flight.retained.request.properties.message_id.clone();
            complete_outcome(in_flight.retained, PublishOutcome::Confirmed { message_id });
        }
        ConfirmationResult::Completed(Ok(PublishConfirmation::Nack(None))) => {
            state.byte_budget.release(in_flight.retained.payload_bytes);
            record_publisher_metrics(state);
            complete_error(
                in_flight.retained,
                PublishError::new(
                    PublishErrorKind::Nack,
                    "broker negatively acknowledged the message",
                ),
            );
        }
        ConfirmationResult::Completed(Ok(PublishConfirmation::NotRequested)) => {
            state.byte_budget.release(in_flight.retained.payload_bytes);
            record_publisher_metrics(state);
            complete_error(
                in_flight.retained,
                PublishError::new(
                    PublishErrorKind::Unconfirmed,
                    "publisher confirms were not enabled",
                ),
            );
        }
    }
}

fn complete_outcome(retained: RetainedPublish, outcome: PublishOutcome) {
    let _ = retained.completion.send(Ok(outcome));
}

fn complete_error(retained: RetainedPublish, error: PublishError) {
    let _ = retained.completion.send(Err(error));
}

fn record_publisher_metrics(state: &ActorState) {
    let publishing_depth =
        u64::try_from(state.publishing.len() + state.ledger.len() + state.replay.len())
            .unwrap_or(u64::MAX);
    state.metrics.record_publishing_depth(publishing_depth);

    let total_bytes = state.byte_budget.current();
    state.metrics.record_publishing_bytes(total_bytes);

    state
        .metrics
        .record_replay_depth(u64::try_from(state.replay.len()).unwrap_or(u64::MAX));
}

fn transport_publish_error(error: &TransportError) -> PublishError {
    PublishError::new(PublishErrorKind::Transport, error.to_string())
}

enum DelayTopologyOutcome {
    Ready,
    Suspend,
    Failed(PublishError),
}

/// Lazily declares delayed exchanges (plugin mode) or TTL delay queues (TTL mode)
/// before the first delayed publish. Idempotent via the `declared_ttl_queues` cache.
async fn ensure_delay_topology(
    state: &mut ActorState,
    channel: &Arc<dyn PublisherChannel>,
    retained: &RetainedPublish,
) -> DelayTopologyOutcome {
    let Some(strategy) = &state.delay_strategy else {
        return DelayTopologyOutcome::Ready;
    };
    let Some(delay_ms) = retained.request.properties.delay_ms else {
        return DelayTopologyOutcome::Ready;
    };
    if delay_ms == 0 {
        return DelayTopologyOutcome::Ready;
    }

    let Ok(route) = DelayRouter::route(
        strategy,
        &retained.request.destination,
        i64::try_from(delay_ms).unwrap_or(i64::MAX),
    ) else {
        return DelayTopologyOutcome::Ready;
    };

    if route.queue.is_none() && !state.declared_ttl_queues.contains(&route.exchange) {
        let spec = crate::transport::ExchangeSpec {
            name: route.exchange.as_ref().to_owned(),
            kind: crate::transport::ExchangeKind::Delayed(Box::new(
                crate::transport::ExchangeKind::Direct,
            )),
            durable: true,
            auto_delete: false,
            internal: false,
            arguments: crate::transport::Headers::new(),
        };
        match channel.declare_exchange(&spec).await {
            Ok(()) => {
                state.declared_ttl_queues.insert(route.exchange.clone());
            }
            Err(error) if error.is_recoverable() => return DelayTopologyOutcome::Suspend,
            Err(error) => {
                return DelayTopologyOutcome::Failed(transport_publish_error(&error));
            }
        }
    }

    if let Some(queue_spec) = &route.queue
        && !state.declared_ttl_queues.contains(queue_spec.name.as_str())
    {
        match channel.declare_queue(queue_spec).await {
            Ok(()) => {
                state
                    .declared_ttl_queues
                    .insert(Arc::from(queue_spec.name.as_str()));
            }
            Err(error) if error.is_recoverable() => return DelayTopologyOutcome::Suspend,
            Err(error) => {
                return DelayTopologyOutcome::Failed(transport_publish_error(&error));
            }
        }
    }

    DelayTopologyOutcome::Ready
}

async fn wait_for_deadline(deadline: Option<time::Instant>) {
    if let Some(deadline) = deadline {
        time::sleep_until(deadline).await;
    } else {
        future::pending::<()>().await;
    }
}
