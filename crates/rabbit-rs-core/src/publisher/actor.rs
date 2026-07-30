use std::{
    collections::VecDeque,
    future,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    time,
};

use crate::{
    metrics::{Metrics, MetricsSnapshot},
    transport::{
        PublishConfirmation, PublishProperties as TransportProperties,
        PublishRequest as TransportRequest, PublisherChannel, TransportError, TransportResult,
    },
};

use super::{
    PublishError, PublishErrorKind, PublishOutcome, PublishRequest, PublishWaiter, PublisherConfig,
    PublisherConnectionEvent, ReturnInfo, batcher::Batcher, confirms::ConfirmLedger,
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
        let capacity = Arc::new(Semaphore::new(config.buffer_capacity.max(1)));
        let (commands, receiver) = mpsc::channel(config.buffer_capacity.max(1));
        tokio::spawn(run_actor(channel, config, receiver, metrics.clone()));
        PublisherHandle {
            commands,
            capacity,
            metrics,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PublisherHandle {
    commands: mpsc::Sender<Command>,
    capacity: Arc<Semaphore>,
    metrics: Metrics,
}

impl PublisherHandle {
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
}

impl ActorState {
    fn new(channel: Arc<dyn PublisherChannel>, config: PublisherConfig, metrics: Metrics) -> Self {
        Self {
            config,
            phase: Phase::Ready,
            generation: 1,
            channel: Some(channel),
            batch: Batcher::new(config.max_messages, config.max_bytes),
            replay: VecDeque::new(),
            ledger: ConfirmLedger::default(),
            confirmations: FuturesUnordered::new(),
            sequence: 0,
            flush_deadline: None,
            permanent_error: None,
            metrics,
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
) {
    let mut state = ActorState::new(initial_channel, config, metrics);
    if let Some(channel) = &state.channel
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
                    accept_publish(&mut state, retained).await;
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
            if generation <= state.generation {
                return Err(PublishError::new(
                    PublishErrorKind::Transport,
                    "publisher recovery generation is stale",
                ));
            }
            channel
                .enable_confirms()
                .await
                .map_err(|error| transport_publish_error(&error))?;
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
    while let Some(retained) = pending.pop_front() {
        if retained.request.deadline <= time::Instant::now() {
            complete_error(
                retained,
                PublishError::new(PublishErrorKind::Timeout, "publish deadline expired"),
            );
            continue;
        }

        let Some(channel) = state.channel.clone() else {
            state.replay.push_back(retained);
            state.replay.extend(pending);
            return;
        };
        state.sequence = state.sequence.saturating_add(1);
        let sequence = state.sequence;
        let generation = state.generation;
        let deadline = retained
            .request
            .deadline
            .min(time::Instant::now() + state.config.confirm_timeout);
        let request = into_transport_request(&retained.request);

        match channel.publish(request).await {
            Ok(receipt) => {
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
            Err(error) if error.is_recoverable() => {
                state.replay.push_back(retained);
                state.replay.extend(pending);
                state.suspend(generation);
                return;
            }
            Err(error) => complete_error(retained, transport_publish_error(&error)),
        }
    }
}

fn into_transport_request(request: &PublishRequest) -> TransportRequest {
    TransportRequest {
        exchange: request.destination.exchange.clone(),
        routing_key: request.destination.routing_key.clone(),
        payload: request.payload.clone(),
        mandatory: true,
        properties: TransportProperties {
            content_type: request.properties.content_type.clone(),
            correlation_id: request.properties.correlation_id.clone(),
            message_id: Some(request.properties.message_id.clone()),
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

async fn wait_for_deadline(deadline: Option<time::Instant>) {
    if let Some(deadline) = deadline {
        time::sleep_until(deadline).await;
    } else {
        future::pending::<()>().await;
    }
}
