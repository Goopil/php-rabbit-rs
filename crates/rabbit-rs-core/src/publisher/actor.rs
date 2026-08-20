use std::{
    collections::{HashSet, VecDeque},
    future,
    sync::Arc,
    time::{Duration, Instant},
};

use arc_swap::ArcSwapOption;
use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    time,
};

use crate::{
    metrics::{Metrics, MetricsSnapshot},
    topology::delay::DelayStrategy,
    transport::{
        PublishConfirmation, PublishRequest as TransportRequest, PublisherChannel, TransportError,
        TransportResult,
    },
};

use super::{
    PublishError, PublishErrorKind, PublishOutcome, PublishRequest, PublishWaiter, PublisherConfig,
    PublisherConnectionEvent, ReturnInfo, batcher::Batcher, confirms::ConfirmLedger,
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
        let (commands, receiver) = mpsc::channel(config.buffer_capacity.max(1));
        let hot_channel: Arc<ArcSwapOption<HotChannel>> =
            Arc::new(ArcSwapOption::from_pointee(HotChannel(channel.clone())));
        let delay_strategy_arc = delay_strategy.map(Arc::new);

        // In Blind mode, create a fire-and-forget pump that bypasses the actor
        // entirely: messages go directly to the transport channel without
        // waiting for confirmations.
        let pump = if matches!(config.safety, crate::config::SafetyMode::Blind) {
            Some(Arc::new(super::pump::PublishPump::spawn(
                channel.clone(),
                config.buffer_capacity,
            )))
        } else {
            None
        };

        tokio::spawn(run_actor(
            channel,
            config,
            receiver,
            metrics.clone(),
            delay_strategy_arc.clone(),
            hot_channel.clone(),
        ));
        PublisherHandle {
            commands,
            capacity,
            metrics,
            confirm_timeout: config.confirm_timeout,
            mandatory: config.mandatory_flag(),
            hot_channel,
            delay_strategy: delay_strategy_arc,
            pump,
        }
    }
}

/// Sized wrapper around `Arc<dyn PublisherChannel>` so it can be stored in
/// `ArcSwapOption` (which requires `Sized` types for its `RefCnt` impls).
#[derive(Clone)]
struct HotChannel(Arc<dyn PublisherChannel>);

impl std::fmt::Debug for HotChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("HotChannel").finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct PublisherHandle {
    commands: mpsc::Sender<Command>,
    capacity: Arc<Semaphore>,
    metrics: Metrics,
    confirm_timeout: Duration,
    mandatory: bool,
    hot_channel: Arc<ArcSwapOption<HotChannel>>,
    delay_strategy: Option<Arc<DelayStrategy>>,
    /// When `Some`, blind-mode publishes go directly to the pump instead of the actor.
    pump: Option<Arc<super::pump::PublishPump>>,
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
        let permit = self.capacity.clone().try_acquire_owned().map_err(|_| {
            self.metrics.record_backpressure();
            PublishError::new(
                PublishErrorKind::Backpressure,
                "publisher global capacity is exhausted",
            )
        })?;
        let (completion, receiver) = oneshot::channel();
        let command = Command::Publish(RetainedPublish {
            request,
            completion,
            accepted_at: Instant::now(),
            _permit: permit,
        });

        match self.commands.try_send(command) {
            Ok(()) => {
                self.metrics.record_publish();
                Ok(PublishWaiter::new(receiver))
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.metrics.record_backpressure();
                Err(PublishError::new(
                    PublishErrorKind::Backpressure,
                    "publisher command buffer is full",
                ))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(PublishError::new(
                PublishErrorKind::Closed,
                "publisher actor is closed",
            )),
        }
    }

    /// Hot path: publishes directly to the transport channel via `now_or_never`,
    /// bypassing the actor mpsc crossing entirely when the channel is ready and
    /// the socket buffer has capacity.
    ///
    /// Falls back to the cold actor path ([`try_publish`](Self::try_publish)) when
    /// the hot channel is unavailable (connection recovery), the immediate
    /// publish is not ready (`Pending`), or the channel returns an error.
    ///
    /// # Errors
    ///
    /// Returns the same typed errors as [`try_publish`](Self::try_publish) on the
    /// cold path fallback.
    /// Hot path: attempts to publish directly to the transport channel via
    /// `now_or_never`, bypassing the actor mpsc crossing when the channel is
    /// ready and the confirmation completes immediately.
    ///
    /// If the channel is unavailable (connection recovery), the publish is not
    /// ready, or the confirmation is pending, falls back to the cold actor path
    /// ([`try_publish`](Self::try_publish)) which provides full lifecycle
    /// management (timeouts, recovery, close).
    ///
    /// # Errors
    ///
    /// Returns the same typed errors as [`try_publish`](Self::try_publish).
    pub fn try_publish_hot(&self, request: PublishRequest) -> Result<PublishWaiter, PublishError> {
        if let Some(hot) = self.hot_channel.load_full()
            && let Ok(permit) = self.capacity.clone().try_acquire_owned()
        {
            let channel = &hot.0;
            let message_id = request.properties.message_id.clone();
            let transport_request = super::into_transport_request(
                &request,
                self.delay_strategy.as_deref(),
                self.mandatory,
            );
            // Hot path: publish and try to get the confirmation immediately.
            // If both publish and confirmation complete without yielding,
            // bypass the actor entirely. If the confirmation is pending,
            // fall back to the cold path so the actor's timeout and
            // lifecycle management apply.
            if let Some(Ok(receipt)) = channel.publish(transport_request).now_or_never() {
                let wait_fut = receipt.wait();
                if let Some(confirmation) = wait_fut.now_or_never() {
                    self.metrics.record_publish();
                    let outcome = super::confirmation_to_outcome(
                        confirmation.map_err(|error| transport_publish_error(&error))?,
                        message_id,
                    )?;
                    drop(permit);
                    return Ok(PublishWaiter::resolved(outcome));
                }
                // Confirmation not ready — fall through to the cold path.
            }
            // Publish not ready or confirmation pending — fall through.
            drop(permit);
        }
        self.try_publish(request)
    }

    /// Blind-mode publish: enqueues to the background pump and returns immediately.
    ///
    /// When `SafetyMode::Blind` is configured, the handle owns a [`PublishPump`]
    /// that publishes to the transport channel in a background task without
    /// waiting for confirmations. This method converts the [`PublishRequest`]
    /// to a [`TransportRequest`](crate::transport::PublishRequest) (with
    /// `mandatory=false`) and enqueues it to the pump.
    ///
    /// The returned [`PublishWaiter`] is already resolved with a synthetic
    /// `Confirmed` outcome — no confirmation is ever received in blind mode.
    ///
    /// Falls back to [`try_publish`](Self::try_publish) when no pump is
    /// configured (non-blind safety mode).
    ///
    /// # Errors
    ///
    /// Returns [`PublishErrorKind::Backpressure`] when the pump channel is
    /// full or disconnected.
    pub fn try_publish_blind(
        &self,
        request: PublishRequest,
    ) -> Result<PublishWaiter, PublishError> {
        let Some(pump) = &self.pump else {
            return self.try_publish(request);
        };
        let message_id = request.properties.message_id.clone();
        let transport_request =
            super::into_transport_request(&request, self.delay_strategy.as_deref(), false);
        if pump.try_publish(transport_request) {
            self.metrics.record_publish();
            Ok(PublishWaiter::resolved(PublishOutcome::Confirmed {
                message_id,
            }))
        } else {
            self.metrics.record_backpressure();
            Err(PublishError::new(
                PublishErrorKind::Backpressure,
                "blind publish pump is full or disconnected",
            ))
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
    Publish(RetainedPublish),
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
}

struct InFlightPublish {
    retained: RetainedPublish,
    generation: u64,
}

enum ConfirmationResult {
    Completed(TransportResult<PublishConfirmation>),
    TimedOut,
}

type ConfirmationFuture = BoxFuture<'static, (u64, u64, ConfirmationResult)>;

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
    batch: Batcher<RetainedPublish>,
    replay: VecDeque<RetainedPublish>,
    ledger: ConfirmLedger<InFlightPublish>,
    confirmations: FuturesUnordered<ConfirmationFuture>,
    sequence: u64,
    flush_deadline: Option<time::Instant>,
    permanent_error: Option<PublishError>,
    metrics: Metrics,
    delay_strategy: Option<Arc<DelayStrategy>>,
    declared_ttl_queues: HashSet<Arc<str>>,
    hot_channel: Arc<ArcSwapOption<HotChannel>>,
}

impl ActorState {
    fn new(
        channel: Arc<dyn PublisherChannel>,
        config: PublisherConfig,
        metrics: Metrics,
        delay_strategy: Option<Arc<DelayStrategy>>,
        hot_channel: Arc<ArcSwapOption<HotChannel>>,
    ) -> Self {
        Self {
            config,
            phase: Phase::Ready,
            generation: 1,
            channel: Some(channel),
            batch: Batcher::new(config.max_messages, config.max_bytes),
            replay: VecDeque::new(),
            ledger: ConfirmLedger::with_capacity(config.max_messages),
            confirmations: FuturesUnordered::new(),
            sequence: 0,
            flush_deadline: None,
            permanent_error: None,
            metrics,
            delay_strategy,
            declared_ttl_queues: HashSet::new(),
            hot_channel,
        }
    }

    fn flush_interval(&self) -> Duration {
        if self.config.flush_interval.is_zero() {
            Duration::from_nanos(1)
        } else {
            self.config.flush_interval
        }
    }

    fn next_deadline(&self) -> Option<time::Instant> {
        match self.phase {
            Phase::Ready => self.flush_deadline,
            Phase::Suspended => self
                .replay
                .iter()
                .map(|pending| pending.request.deadline)
                .min(),
            Phase::FailedPermanent => None,
        }
    }

    fn suspend(&mut self, generation: u64) {
        if generation > 0 {
            self.generation = self.generation.max(generation);
        }
        self.phase = Phase::Suspended;
        self.channel = None;
        self.flush_deadline = None;
        self.replay.extend(self.batch.take());
        self.replay
            .extend(self.ledger.drain().map(|in_flight| in_flight.retained));
        self.confirmations = FuturesUnordered::new();
        self.hot_channel.store(None);
    }

    fn fail_all(&mut self, error: &PublishError) {
        for retained in self.batch.take() {
            complete_error(retained, error.clone());
        }
        for retained in self.replay.drain(..) {
            complete_error(retained, error.clone());
        }
        for in_flight in self.ledger.drain() {
            complete_error(in_flight.retained, error.clone());
        }
        self.confirmations = FuturesUnordered::new();
        self.flush_deadline = None;
    }

    fn expire_replay(&mut self) {
        let now = time::Instant::now();
        let mut retained = VecDeque::new();
        while let Some(pending) = self.replay.pop_front() {
            if pending.request.deadline <= now {
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
    }
}

async fn run_actor(
    initial_channel: Arc<dyn PublisherChannel>,
    config: PublisherConfig,
    mut commands: mpsc::Receiver<Command>,
    metrics: Metrics,
    delay_strategy: Option<Arc<DelayStrategy>>,
    hot_channel: Arc<ArcSwapOption<HotChannel>>,
) {
    let mut state = ActorState::new(
        initial_channel,
        config,
        metrics,
        delay_strategy,
        hot_channel.clone(),
    );
    if state.config.enables_confirms()
        && let Some(channel) = &state.channel
        && let Err(error) = channel.enable_confirms().await
    {
        let error = transport_publish_error(&error);
        state.phase = Phase::FailedPermanent;
        state.permanent_error = Some(error);
        hot_channel.store(None);
    }

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Publish(retained)) => {
                    accept_publish(&mut state, retained).await;
                    if !state.batch.is_empty() && commands.is_empty() {
                        flush_batch(&mut state).await;
                        state.flush_deadline = None;
                    }
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
                    state.fail_all(&error);
                    if let Some(channel) = state.channel.take() {
                        let _ = channel.close().await;
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
                    Phase::Ready => {
                        flush_batch(&mut state).await;
                        state.flush_deadline = None;
                    }
                    Phase::Suspended => state.expire_replay(),
                    Phase::FailedPermanent => {}
                }
            }
            confirmation = state.confirmations.next(), if !state.confirmations.is_empty() => {
                if let Some((sequence, generation, result)) = confirmation {
                    resolve_confirmation(&mut state, sequence, generation, result);
                }
            }
        }
    }
}

async fn accept_publish(state: &mut ActorState, retained: RetainedPublish) {
    match state.phase {
        Phase::Ready => {
            let payload_len = retained.request.payload.len();
            if state.batch.is_empty() {
                state.flush_deadline = Some(time::Instant::now() + state.flush_interval());
            }
            if state.batch.push(retained, payload_len) {
                flush_batch(state).await;
                state.flush_deadline = None;
            }
        }
        Phase::Suspended => state.replay.push_back(retained),
        Phase::FailedPermanent => complete_error(
            retained,
            state.permanent_error.clone().unwrap_or_else(|| {
                PublishError::new(
                    PublishErrorKind::Transport,
                    "publisher connection failed permanently",
                )
            }),
        ),
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
            if state.config.enables_confirms() {
                channel
                    .enable_confirms()
                    .await
                    .map_err(|error| transport_publish_error(&error))?;
            }
            state.generation = generation;
            state.channel = Some(channel.clone());
            state.phase = Phase::Ready;
            state.permanent_error = None;
            state.hot_channel.store(Some(Arc::new(HotChannel(channel))));
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
            state.hot_channel.store(None);
            Ok(())
        }
    }
}

async fn flush_batch(state: &mut ActorState) {
    let pending = VecDeque::from(state.batch.take());
    publish_queue(state, pending).await;
}

async fn flush_replay(state: &mut ActorState) {
    state.expire_replay();
    let pending = std::mem::take(&mut state.replay);
    publish_queue(state, pending).await;
}

async fn publish_queue(state: &mut ActorState, mut pending: VecDeque<RetainedPublish>) {
    let Some(channel) = state.channel.clone() else {
        state.replay.extend(pending);
        return;
    };

    // Phase 1: Pre-process all messages — expire deadlines, ensure delay
    // topology, assign sequence numbers, and convert to transport requests.
    // If delay topology suspends mid-batch, replay everything collected so
    // far plus the remaining messages without sending anything.
    let mut batch: Vec<(u64, u64, time::Instant, RetainedPublish)> = Vec::new();
    let mut transport_requests: Vec<TransportRequest> = Vec::new();
    let mut suspended = false;

    while let Some(retained) = pending.pop_front() {
        if retained.request.deadline <= time::Instant::now() {
            complete_error(
                retained,
                PublishError::new(PublishErrorKind::Timeout, "publish deadline expired"),
            );
            continue;
        }

        match ensure_delay_topology(state, &channel, &retained).await {
            DelayTopologyOutcome::Ready => {}
            DelayTopologyOutcome::Suspend => {
                for (_, _, _, r) in batch.drain(..) {
                    state.replay.push_back(r);
                }
                state.replay.push_back(retained);
                state.replay.extend(pending);
                state.suspend(state.generation);
                suspended = true;
                break;
            }
            DelayTopologyOutcome::Failed(error) => {
                complete_error(retained, error);
                continue;
            }
        }

        state.sequence = state.sequence.saturating_add(1);
        let sequence = state.sequence;
        let generation = state.generation;
        let deadline = retained
            .request
            .deadline
            .min(time::Instant::now() + state.config.confirm_timeout);
        let request = super::into_transport_request(
            &retained.request,
            state.delay_strategy.as_deref(),
            state.config.mandatory_flag(),
        );

        batch.push((sequence, generation, deadline, retained));
        transport_requests.push(request);
    }

    if suspended || transport_requests.is_empty() {
        return;
    }

    // Phase 2: Send all frames in one batch call.
    match channel.publish_batch(transport_requests).await {
        Ok(receipts) => {
            for ((sequence, generation, deadline, retained), receipt) in
                batch.into_iter().zip(receipts)
            {
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
            }
        }
        Err(error) if error.is_recoverable() => {
            for (_, _, _, retained) in batch {
                state.replay.push_back(retained);
            }
            state.suspend(state.generation);
        }
        Err(error) => {
            for (_, _, _, retained) in batch {
                complete_error(retained, transport_publish_error(&error));
            }
        }
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
        ConfirmationResult::TimedOut => complete_error(
            in_flight.retained,
            PublishError::new(
                PublishErrorKind::Timeout,
                "publisher confirmation timed out",
            ),
        ),
        ConfirmationResult::Completed(Err(error)) if error.is_recoverable() => {
            state.replay.push_back(in_flight.retained);
            state.suspend(generation);
        }
        ConfirmationResult::Completed(Err(error)) => {
            complete_error(in_flight.retained, transport_publish_error(&error));
        }
        ConfirmationResult::Completed(Ok(
            PublishConfirmation::Ack(Some(returned)) | PublishConfirmation::Nack(Some(returned)),
        )) => {
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
            let message_id = in_flight.retained.request.properties.message_id.clone();
            complete_outcome(in_flight.retained, PublishOutcome::Confirmed { message_id });
        }
        ConfirmationResult::Completed(Ok(PublishConfirmation::Nack(None))) => complete_error(
            in_flight.retained,
            PublishError::new(
                PublishErrorKind::Nack,
                "broker negatively acknowledged the message",
            ),
        ),
        ConfirmationResult::Completed(Ok(PublishConfirmation::NotRequested)) => complete_error(
            in_flight.retained,
            PublishError::new(
                PublishErrorKind::Unconfirmed,
                "publisher confirms were not enabled",
            ),
        ),
    }
}

fn complete_outcome(retained: RetainedPublish, outcome: PublishOutcome) {
    let _ = retained.completion.send(Ok(outcome));
}

fn complete_error(retained: RetainedPublish, error: PublishError) {
    let _ = retained.completion.send(Err(error));
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
