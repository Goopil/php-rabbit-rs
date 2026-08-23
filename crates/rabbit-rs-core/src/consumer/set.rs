use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::sync::{Notify, mpsc, oneshot};

use super::{
    ConsumerError, Delivery, DeliveryTokenInner, SettleError, Settlement, SettlementError,
    SubscriptionId, SubscriptionPolicy,
    actor::{ConsumerCommand, run_actor},
};
use crate::{
    metrics::{Metrics, MetricsSnapshot},
    pool::ConnectionKey,
    publisher::{Destination, PublisherHandle},
    topology::delay::DelayStrategy,
    transport::{ConsumerChannel, ConsumerRequest, DeliveryStream},
};

const COMMAND_CAPACITY: usize = 256;
const BUFFER_CAPACITY_FACTOR: usize = 3;

pub struct Subscription {
    pub(crate) id: SubscriptionId,
    pub(crate) connection_key: ConnectionKey,
    pub(crate) generation: u64,
    pub(crate) channel_id: u16,
    pub(crate) queue: String,
    pub(crate) prefetch: u16,
    pub(crate) policy: SubscriptionPolicy,
    pub(crate) early_ack: bool,
    pub(crate) max_buffered_bytes: u64,
    pub(crate) channel: Arc<dyn ConsumerChannel>,
    pub(crate) publisher: Option<PublisherHandle>,
    pub(crate) destination: Option<Destination>,
    pub(crate) delay_strategy: Option<DelayStrategy>,
}

impl Subscription {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        connection_key: ConnectionKey,
        queue: impl Into<String>,
        channel: Arc<dyn ConsumerChannel>,
    ) -> Self {
        Self {
            id: SubscriptionId::new(id),
            connection_key,
            generation: 1,
            channel_id: 1,
            queue: queue.into(),
            prefetch: 16,
            policy: SubscriptionPolicy::new(1, 0, Duration::from_secs(30)),
            early_ack: false,
            max_buffered_bytes: 64 * 1024 * 1024,
            channel,
            publisher: None,
            destination: None,
            delay_strategy: None,
        }
    }

    #[must_use]
    pub const fn prefetch(mut self, prefetch: u16) -> Self {
        self.prefetch = prefetch;
        self
    }

    #[must_use]
    pub const fn channel_id(mut self, channel_id: u16) -> Self {
        self.channel_id = channel_id;
        self
    }

    #[must_use]
    pub const fn generation(mut self, generation: u64) -> Self {
        self.generation = generation;
        self
    }

    #[must_use]
    pub const fn policy(mut self, policy: SubscriptionPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Enables or disables early-ACK best-effort mode.
    ///
    /// When `true`, deliveries are auto-acked to the broker before dispatch
    /// and presented with [`DeliveryState::AutoAcked`].
    #[must_use]
    pub const fn early_ack(mut self, early_ack: bool) -> Self {
        self.early_ack = early_ack;
        self
    }

    #[must_use]
    pub const fn max_buffered_bytes(mut self, max: u64) -> Self {
        self.max_buffered_bytes = max;
        self
    }

    #[must_use]
    pub fn delayed_publisher(
        mut self,
        publisher: PublisherHandle,
        destination: Destination,
    ) -> Self {
        self.publisher = Some(publisher);
        self.destination = Some(destination);
        self
    }

    #[must_use]
    pub fn delay_strategy(mut self, strategy: DelayStrategy) -> Self {
        self.delay_strategy = Some(strategy);
        self
    }
}

pub struct ConsumerSet;

impl ConsumerSet {
    /// Configures all channels and starts one multiplexing actor.
    ///
    /// # Errors
    ///
    /// Returns a typed transport error when `QoS` or consumer registration fails.
    pub async fn spawn(
        subscriptions: Vec<Subscription>,
        max_in_flight: usize,
    ) -> Result<ConsumerHandle, ConsumerError> {
        Self::spawn_with_metrics(subscriptions, max_in_flight, Metrics::default()).await
    }

    /// Configures the consumer set with a metrics registry shared by its caller.
    ///
    /// # Errors
    ///
    /// Returns a typed transport error when `QoS` or consumer registration fails.
    pub async fn spawn_with_metrics(
        subscriptions: Vec<Subscription>,
        max_in_flight: usize,
        metrics: Metrics,
    ) -> Result<ConsumerHandle, ConsumerError> {
        let generation = subscriptions.first().map_or(1, |s| s.generation);
        Self::spawn_with_generation(subscriptions, max_in_flight, metrics, generation).await
    }

    async fn spawn_with_generation(
        subscriptions: Vec<Subscription>,
        max_in_flight: usize,
        metrics: Metrics,
        generation: u64,
    ) -> Result<ConsumerHandle, ConsumerError> {
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let mut streams = Vec::with_capacity(subscriptions.len());

        for subscription in &subscriptions {
            if let Err(error) = subscription.channel.set_qos(subscription.prefetch).await {
                close_subscription_channels(&subscriptions).await;
                return Err(ConsumerError::new(
                    super::ConsumerErrorKind::Transport,
                    error.to_string(),
                ));
            }
            let stream = match subscription
                .channel
                .consume(ConsumerRequest::new(
                    subscription.queue.clone(),
                    format!("rabbit-rs.{}", subscription.id.as_str()),
                ))
                .await
            {
                Ok(stream) => stream,
                Err(error) => {
                    close_subscription_channels(&subscriptions).await;
                    return Err(ConsumerError::new(
                        super::ConsumerErrorKind::Transport,
                        error.to_string(),
                    ));
                }
            };
            streams.push((subscription.id.clone(), stream));
        }

        let total_prefetch: u64 = subscriptions.iter().map(|s| u64::from(s.prefetch)).sum();
        let buffer_size =
            usize::try_from(total_prefetch).unwrap_or(usize::MAX) * BUFFER_CAPACITY_FACTOR / 2;
        let (buffer_tx, buffer_rx) =
            flume::bounded::<Result<Delivery, ConsumerError>>(buffer_size.max(1));
        let (error_tx, error_rx) = flume::bounded::<SettlementError>(256);
        let dispatch_notify = Arc::new(Notify::new());

        tokio::spawn(run_actor(
            subscriptions,
            max_in_flight.max(1),
            receiver,
            commands.clone(),
            buffer_tx,
            error_tx,
            metrics.clone(),
            dispatch_notify.clone(),
        ));
        for (subscription, stream) in streams {
            spawn_source(subscription, stream, commands.clone());
        }

        Ok(ConsumerHandle {
            commands,
            buffer_rx,
            error_rx,
            metrics,
            closed: Arc::new(AtomicBool::new(false)),
            dispatch_notify,
            generation,
            pending_error: Arc::new(Mutex::new(None)),
        })
    }
}

fn spawn_source(
    subscription: SubscriptionId,
    mut stream: Box<dyn DeliveryStream>,
    commands: mpsc::Sender<ConsumerCommand>,
) {
    tokio::spawn(async move {
        while let Some(result) = stream.next().await {
            if commands
                .send(ConsumerCommand::Incoming {
                    subscription: subscription.clone(),
                    result,
                })
                .await
                .is_err()
            {
                return;
            }
        }
    });
}

async fn close_subscription_channels(subscriptions: &[Subscription]) {
    for subscription in subscriptions {
        let _ = subscription.channel.close().await;
    }
}

#[derive(Clone, Debug)]
pub struct ConsumerHandle {
    commands: mpsc::Sender<ConsumerCommand>,
    buffer_rx: flume::Receiver<Result<Delivery, ConsumerError>>,
    error_rx: flume::Receiver<SettlementError>,
    metrics: Metrics,
    closed: Arc<AtomicBool>,
    dispatch_notify: Arc<Notify>,
    generation: u64,
    pending_error: Arc<Mutex<Option<ConsumerError>>>,
}

impl ConsumerHandle {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for ConsumerHandle {
    fn drop(&mut self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let (sender, _) = oneshot::channel();
        let _ = self.commands.try_send(ConsumerCommand::Close(sender));
    }
}

impl ConsumerHandle {
    #[must_use]
    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Drains all settlement errors that the actor has recorded since the
    /// last call. The error buffer is bounded (256); if it fills, the actor
    /// drops the oldest errors.
    ///
    /// Settlement errors surface asynchronously because settlement is
    /// fire-and-forget: `ack()`, `release()`, and `reject()` enqueue the
    /// command and return immediately. Transport failures, stale-generation
    /// errors, and other settlement failures appear here, not in the return
    /// value of the settlement call.
    #[must_use]
    pub fn drain_errors(&self) -> Vec<SettlementError> {
        let mut errors = Vec::new();
        while let Ok(error) = self.error_rx.try_recv() {
            errors.push(error);
        }
        errors
    }

    /// Fire-and-forget settlement via the actor's command channel.
    ///
    /// Enqueues a `Settle` command with `try_send` and returns immediately.
    /// Does not perform the `Pending → Transitioning` CAS — the caller is
    /// responsible for ensuring the delivery is not double-settled.
    ///
    /// # Errors
    ///
    /// Returns [`SettleError::ChannelFull`] when the command channel is at
    /// capacity, or [`SettleError::Closed`] when the actor has stopped.
    pub fn try_settle(
        &self,
        token: Arc<DeliveryTokenInner>,
        settlement: Settlement,
    ) -> Result<(), SettleError> {
        self.commands
            .try_send(ConsumerCommand::Settle { token, settlement })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => SettleError::ChannelFull,
                mpsc::error::TrySendError::Closed(_) => SettleError::Closed,
            })
    }

    /// Fire-and-forget batch settlement via the actor's command channel.
    ///
    /// Enqueues a `SettleThrough` command with `try_send` and returns
    /// immediately. Does not perform the CAS — the caller is responsible for
    /// ensuring the delivery is not double-settled.
    ///
    /// # Errors
    ///
    /// Returns [`SettleError::ChannelFull`] when the command channel is at
    /// capacity, or [`SettleError::Closed`] when the actor has stopped.
    pub fn try_settle_through(&self, token: Arc<DeliveryTokenInner>) -> Result<(), SettleError> {
        self.commands
            .try_send(ConsumerCommand::SettleThrough { token })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => SettleError::ChannelFull,
                mpsc::error::TrySendError::Closed(_) => SettleError::Closed,
            })
    }

    /// Tries to receive the next delivery without blocking.
    ///
    /// Returns `Ok(Some(delivery))` when one is available in the buffer,
    /// `Ok(None)` when the buffer is empty, or `Err` when the consumer is closed.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the consumer is closed.
    pub fn try_next(&self) -> Result<Option<Delivery>, ConsumerError> {
        match self.buffer_rx.try_recv() {
            Ok(Ok(delivery)) => {
                self.dispatch_notify.notify_one();
                Ok(Some(delivery))
            }
            Ok(Err(error)) => {
                self.dispatch_notify.notify_one();
                Err(error)
            }
            Err(flume::TryRecvError::Empty) => {
                if let Some(error) = self
                    .pending_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    return Err(error);
                }
                Ok(None)
            }
            Err(flume::TryRecvError::Disconnected) => Err(ConsumerError::closed()),
        }
    }

    /// Drains up to `max` deliveries from the buffer in a single call.
    ///
    /// The requested `max` is clamped to `1..=256`. Returns an empty vector when
    /// the buffer is empty. Each drained delivery releases dispatch budget so
    /// the actor can pull more work from the transport.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the consumer is closed or a source error is
    /// encountered mid-drain.
    pub fn try_next_batch(&self, max: usize) -> Result<Vec<Delivery>, ConsumerError> {
        let max = max.clamp(1, 256);
        let mut batch = Vec::with_capacity(max);
        for _ in 0..max {
            match self.buffer_rx.try_recv() {
                Ok(Ok(delivery)) => batch.push(delivery),
                Ok(Err(error)) => {
                    self.dispatch_notify.notify_one();
                    if !batch.is_empty() {
                        // Stash error, return partial batch so deliveries are
                        // never discarded. The error surfaces on the next call
                        // when the buffer is empty.
                        *self
                            .pending_error
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                        return Ok(batch);
                    }
                    return Err(error);
                }
                Err(flume::TryRecvError::Empty) => break,
                Err(flume::TryRecvError::Disconnected) => {
                    self.dispatch_notify.notify_one();
                    if !batch.is_empty() {
                        *self
                            .pending_error
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            Some(ConsumerError::closed());
                        return Ok(batch);
                    }
                    return Err(ConsumerError::closed());
                }
            }
        }
        if !batch.is_empty() {
            self.dispatch_notify.notify_one();
            return Ok(batch);
        }
        // Buffer is empty — surface stashed error if any.
        if let Some(error) = self
            .pending_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            return Err(error);
        }
        Ok(batch)
    }

    /// Waits for the next scheduled delivery.
    ///
    /// # Errors
    ///
    /// Returns a typed source, transport, or closed-consumer error.
    pub async fn next(&self) -> Result<Delivery, ConsumerError> {
        self.dispatch_notify.notify_one();
        match self.buffer_rx.recv_async().await {
            Ok(Ok(delivery)) => {
                self.dispatch_notify.notify_one();
                Ok(delivery)
            }
            Ok(Err(error)) => {
                self.dispatch_notify.notify_one();
                Err(error)
            }
            Err(flume::RecvError::Disconnected) => Err(ConsumerError::closed()),
        }
    }

    /// Acknowledges a contiguous prefix of deliveries up to and including the
    /// given delivery, using a single AMQP `basic.ack` with `multiple=true`.
    ///
    /// The prefix must be contiguous starting from `acked_prefix + 1`.
    /// Non-contiguous prefixes or already-terminal deliveries in the range are
    /// rejected asynchronously by the actor and surface via
    /// [`Self::drain_errors`].
    ///
    /// Fire-and-forget: enqueues the command and returns immediately. The
    /// final state of each affected delivery is updated asynchronously by the
    /// actor.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the command channel is full or the consumer
    /// is closed.
    #[allow(clippy::unused_async)]
    pub async fn ack_through(&self, delivery: &Delivery) -> Result<(), ConsumerError> {
        self.try_settle_through(delivery.inner_token().clone())
            .map_err(|e| match e {
                SettleError::ChannelFull => ConsumerError::new(
                    super::ConsumerErrorKind::SettlementInProgress,
                    "settlement command channel is full",
                ),
                SettleError::Closed => ConsumerError::closed(),
            })
    }

    /// Records a new connection generation for one subscription.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the subscription or actor is unavailable.
    pub async fn update_generation(
        &self,
        subscription: SubscriptionId,
        generation: u64,
    ) -> Result<(), ConsumerError> {
        let (completed, completion) = oneshot::channel();
        self.commands
            .send(ConsumerCommand::UpdateGeneration {
                subscription,
                generation,
                completed,
            })
            .await
            .map_err(|_| ConsumerError::closed())?;
        completion.await.map_err(|_| ConsumerError::closed())?
    }

    /// Closes the set and wakes all pending calls to [`Self::next`].
    ///
    /// # Errors
    ///
    /// Returns a typed error when the actor stops before processing the first close request.
    pub async fn close(&self) -> Result<(), ConsumerError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let (completed, completion) = oneshot::channel();
        self.commands
            .send(ConsumerCommand::Close(completed))
            .await
            .map_err(|_| ConsumerError::closed())?;
        completion.await.map_err(|_| ConsumerError::closed())
    }
}
