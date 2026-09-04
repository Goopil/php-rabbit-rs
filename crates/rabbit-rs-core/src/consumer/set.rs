use std::{
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::sync::{Notify, mpsc, oneshot, watch};

use super::{
    ConsumerError, Delivery, DeliveryTokenInner, SettlementError, SettlementErrorKind,
    SubscriptionId, SubscriptionPolicy,
    actor::{ConsumerCommand, run_actor},
    attempts::DEFAULT_MAX_ATTEMPTS_NON_ZERO,
};
use crate::{
    metrics::{Metrics, MetricsSnapshot},
    pool::ConnectionKey,
    publisher::{Destination, PublisherHandle},
    topology::delay::DelayStrategy,
    transport::{ConsumerChannel, ConsumerRequest, DeliveryStream, TransportError},
};

const COMMAND_CAPACITY: usize = 256;
const BUFFER_CAPACITY_MULTIPLIER: usize = 2;
/// Upper bound on retained settlement errors. When full, the actor drops the
/// oldest errors instead of blocking — the consumer loop must never stall
/// waiting for the embedder to call `drain_errors`.
const ERROR_CHANNEL_CAPACITY: usize = 256;

pub struct Subscription {
    pub(crate) id: SubscriptionId,
    pub(crate) connection_key: ConnectionKey,
    pub(crate) generation: u64,
    pub(crate) channel_id: u16,
    pub(crate) queue: String,
    pub(crate) prefetch: u16,
    pub(crate) policy: SubscriptionPolicy,
    pub(crate) early_ack: bool,
    pub(crate) no_ack: bool,
    pub(crate) max_buffered_bytes: u64,
    pub(crate) max_attempts: Option<NonZeroU32>,
    pub(crate) dead_letter: bool,
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
            no_ack: false,
            max_buffered_bytes: 64 * 1024 * 1024,
            max_attempts: Some(DEFAULT_MAX_ATTEMPTS_NON_ZERO),
            dead_letter: false,
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

    /// Enables or disables broker-side `no_ack` mode.
    ///
    /// When `true`, the broker auto-acks deliveries internally — no ack frames
    /// are sent from the consumer. Requires `early_ack=true` and
    /// `best_effort=true` at the configuration layer to preserve at-least-once
    /// semantics as an opt-in.
    #[must_use]
    pub const fn no_ack(mut self, no_ack: bool) -> Self {
        self.no_ack = no_ack;
        self
    }

    #[must_use]
    pub const fn max_buffered_bytes(mut self, max: u64) -> Self {
        self.max_buffered_bytes = max;
        self
    }

    /// Sets the maximum resolved delivery attempts per message. Deliveries
    /// above the cap are settled terminally instead of being dispatched.
    #[must_use]
    pub const fn max_attempts(mut self, max_attempts: Option<NonZeroU32>) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Declares whether the subscription's queue is bound to a dead-letter
    /// exchange. When `true`, poison deliveries are rejected with
    /// `requeue=false` so the broker routes them to the DLX; when `false`,
    /// the explicit ack-and-log policy applies.
    #[must_use]
    pub const fn dead_letter(mut self, dead_letter: bool) -> Self {
        self.dead_letter = dead_letter;
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
    /// Configures the consumer set with a metrics registry shared by its caller.
    ///
    /// # Errors
    ///
    /// Returns a typed transport error when `QoS` or consumer registration fails.
    pub async fn spawn_with_metrics(
        subscriptions: Vec<Subscription>,
        metrics: Metrics,
    ) -> Result<ConsumerSetHandle, ConsumerError> {
        let generation = subscriptions.first().map_or(1, |s| s.generation);
        Self::spawn_with_generation(subscriptions, metrics, generation).await
    }

    async fn spawn_with_generation(
        subscriptions: Vec<Subscription>,
        metrics: Metrics,
        generation: u64,
    ) -> Result<ConsumerSetHandle, ConsumerError> {
        let total_prefetch: u64 = subscriptions.iter().map(|s| u64::from(s.prefetch)).sum();
        // The command channel carries Incoming delivery commands from the
        // per-subscription pumps plus settlement commands. Size it from the
        // total prefetch so a large prefetch does not turn every delivery
        // handoff into pump backpressure.
        let channel_capacity =
            COMMAND_CAPACITY.max(usize::try_from(total_prefetch).unwrap_or(usize::MAX));
        let (commands, receiver) = mpsc::channel(channel_capacity);
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
                .consume(ConsumerRequest {
                    queue: subscription.queue.clone(),
                    consumer_tag: format!("rabbit-rs.{}", subscription.id.as_str()),
                    exclusive: false,
                    no_ack: subscription.no_ack,
                })
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

        // With prefetch >= 128 the flume holds >= 256, so `try_next_batch(256)`
        // can fill a complete batch in one call. The actor-side dispatch stops
        // when the flume is full, so this capacity is also the natural
        // handoff window between the actor and the consumer.
        let buffer_size =
            usize::try_from(total_prefetch).unwrap_or(usize::MAX) * BUFFER_CAPACITY_MULTIPLIER;
        let (buffer_tx, buffer_rx) =
            flume::bounded::<Result<Delivery, ConsumerError>>(buffer_size.max(1));
        let (error_tx, error_rx) = flume::bounded::<SettlementError>(ERROR_CHANNEL_CAPACITY);
        let dispatch_notify = Arc::new(Notify::new());
        // Drop-close and explicit `close()` both use a dedicated watch signal
        // instead of the shared command channel: a saturated command channel
        // must never be able to discard a close request and leak the actor
        // with its broker subscriptions. The explicit close registers its
        // completion in a slot the actor resolves after closing the channels,
        // so `close()` keeps reporting an actor that died before closing.
        let (close_tx, close_rx) = watch::channel(false);
        let close_completion: Arc<Mutex<Option<oneshot::Sender<()>>>> = Arc::new(Mutex::new(None));

        tokio::spawn(run_actor(
            subscriptions,
            receiver,
            commands.clone(),
            buffer_tx,
            error_tx,
            // The actor keeps its own receiver for drop-oldest; the handle
            // drains through the original receiver below.
            error_rx.clone(),
            metrics.clone(),
            dispatch_notify.clone(),
            close_rx,
            close_completion.clone(),
            channel_capacity,
        ));
        for (subscription, stream) in streams {
            spawn_source(subscription, stream, commands.clone());
        }

        Ok(ConsumerSetHandle {
            commands,
            buffer_rx,
            error_rx,
            metrics,
            closed: Arc::new(AtomicBool::new(false)),
            dispatch_notify,
            generation,
            pending_error: Arc::new(Mutex::new(None)),
            close_tx: Arc::new(close_tx),
            close_completion,
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
        // A terminated delivery stream means the subscription is dead
        // (connection lost, channel closed). Surface one terminal error so
        // `next()` unblocks instead of parking forever.
        let _ = commands
            .send(ConsumerCommand::Incoming {
                subscription,
                result: Err(TransportError::connection("consumer delivery stream ended")),
            })
            .await;
    });
}

async fn close_subscription_channels(subscriptions: &[Subscription]) {
    for subscription in subscriptions {
        let _ = subscription.channel.close().await;
    }
}

/// Maps a command-channel  failure to the settlement error kind
/// (shared by the set handle and the composite router).
pub(crate) fn map_try_send_error(
    error: &mpsc::error::TrySendError<ConsumerCommand>,
) -> SettlementErrorKind {
    match error {
        mpsc::error::TrySendError::Full(_) => SettlementErrorKind::ChannelFull,
        mpsc::error::TrySendError::Closed(_) => SettlementErrorKind::Closed,
    }
}

/// A handle to one per-broker consumer set.
///
/// Deliveries handed out by this handle carry tokens that route settlements
/// back to this set's actor, so acknowledgements always reach the broker
/// connection the delivery came from. Use [`super::ConsumerHandle`] to merge
/// several per-broker sets into one multi-broker consumer.
#[derive(Clone, Debug)]
pub struct ConsumerSetHandle {
    commands: mpsc::Sender<ConsumerCommand>,
    buffer_rx: flume::Receiver<Result<Delivery, ConsumerError>>,
    error_rx: flume::Receiver<SettlementError>,
    metrics: Metrics,
    closed: Arc<AtomicBool>,
    dispatch_notify: Arc<Notify>,
    generation: u64,
    pending_error: Arc<Mutex<Option<ConsumerError>>>,
    close_tx: Arc<watch::Sender<bool>>,
    close_completion: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl ConsumerSetHandle {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for ConsumerSetHandle {
    fn drop(&mut self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        // The watch signal is synchronous and cannot be discarded by command
        // backpressure, unlike a `try_send` of `ConsumerCommand::Close`.
        let _ = self.close_tx.send(true);
    }
}

impl ConsumerSetHandle {
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

    /// Fire-and-forget batch settlement via the actor's command channel.
    ///
    /// Enqueues a `SettleThrough` command with `try_send` and returns
    /// immediately. Does not perform the CAS — the caller is responsible for
    /// ensuring the delivery is not double-settled.
    ///
    /// # Errors
    ///
    /// Returns [`SettlementErrorKind::ChannelFull`] when the command channel is at
    /// capacity, or [`SettlementErrorKind::Closed`] when the actor has stopped.
    pub fn try_settle_through(
        &self,
        token: Arc<DeliveryTokenInner>,
    ) -> Result<(), SettlementErrorKind> {
        self.commands
            .try_send(ConsumerCommand::SettleThrough { token })
            .map_err(|e| map_try_send_error(&e))
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
    /// the buffer is empty. The effective maximum batch size equals the flume
    /// capacity (`total_prefetch × 2`), itself clamped by the `1..=256` clamp,
    /// so with prefetch ≥ 128 a full `256` batch can drain in one call. Each
    /// drained delivery wakes the actor, which refills the buffer from the
    /// transport.
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

    /// Closes the set and wakes all pending calls to [`Self::next`].
    ///
    /// The close request travels on a dedicated watch signal the actor
    /// selects on, so command-channel backpressure can never discard it.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the actor stops before closing the
    /// subscription channels.
    pub async fn close(&self) -> Result<(), ConsumerError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let (completed, completion) = oneshot::channel();
        *self
            .close_completion
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(completed);
        let _ = self.close_tx.send(true);
        completion.await.map_err(|_| ConsumerError::closed())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, time::Duration};

    use bytes::Bytes;
    use tokio::sync::Notify;

    use super::*;
    use crate::transport::Transport;
    use crate::{
        config::{BrokerConfig, Credentials, Endpoint, TlsConfig},
        transport::{Delivery as TransportDelivery, mock::MockTransport, mock::TransportOperation},
    };

    fn test_broker() -> BrokerConfig {
        BrokerConfig {
            name: "test".to_owned(),
            hosts: vec![Endpoint::new("localhost", 5672)],
            vhost: "/".to_owned(),
            credentials: Credentials::new("guest", "guest"),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        }
    }

    /// Dropping the handle is the only close path an embedder can forget to
    /// await, so its close signal must survive a saturated command channel:
    /// the actor and its broker subscriptions must not leak (audit F-16).
    #[tokio::test(start_paused = true)]
    async fn drop_with_saturated_command_channel_still_closes_the_actor() {
        let transport = MockTransport::default();
        let channel = transport
            .connect(&test_broker())
            .await
            .expect("connection")
            .open_consumer()
            .await
            .expect("consumer channel");

        let (commands, receiver) = mpsc::channel::<ConsumerCommand>(COMMAND_CAPACITY);
        let subscription = SubscriptionId::new("jobs");
        let delivery = || TransportDelivery {
            delivery_tag: 1,
            exchange: "jobs".to_owned(),
            routing_key: "jobs".to_owned(),
            redelivered: false,
            message_id: None,
            correlation_id: None,
            headers: Arc::new(BTreeMap::new()),
            payload: Bytes::from_static(b"payload"),
        };
        for _ in 0..COMMAND_CAPACITY {
            commands
                .try_send(ConsumerCommand::Incoming {
                    subscription: subscription.clone(),
                    result: Ok(delivery()),
                })
                .expect("channel saturated before the actor is polled");
        }

        let (buffer_tx, buffer_rx) = flume::bounded::<Result<Delivery, ConsumerError>>(1);
        let (error_tx, error_rx) = flume::bounded::<SettlementError>(16);
        let dispatch_notify = Arc::new(Notify::new());
        let (close_tx, close_rx) = watch::channel(false);
        let close_completion: Arc<Mutex<Option<oneshot::Sender<()>>>> = Arc::new(Mutex::new(None));
        let actor = tokio::spawn(run_actor(
            vec![Subscription::new(
                "jobs",
                crate::pool::ConnectionKey::from_bytes([7; 32]),
                "queue.jobs",
                Arc::from(channel),
            )],
            receiver,
            commands.clone(),
            buffer_tx,
            error_tx,
            error_rx.clone(),
            Metrics::default(),
            dispatch_notify.clone(),
            close_rx,
            close_completion.clone(),
            COMMAND_CAPACITY,
        ));

        let handle = ConsumerSetHandle {
            commands: commands.clone(),
            buffer_rx,
            error_rx,
            metrics: Metrics::default(),
            closed: Arc::new(AtomicBool::new(false)),
            dispatch_notify,
            generation: 1,
            pending_error: Arc::new(Mutex::new(None)),
            close_tx: Arc::new(close_tx),
            close_completion,
        };
        // In a current-thread runtime the actor is not polled until the test
        // awaits, so the channel is still saturated when `Drop` runs.
        drop(handle);

        let exited = tokio::time::timeout(Duration::from_secs(1), actor).await;
        assert!(
            exited.is_ok(),
            "dropping the handle with a saturated command channel must still stop the actor"
        );
        assert_eq!(
            transport
                .operations()
                .iter()
                .filter(|operation| matches!(operation, TransportOperation::CloseChannel))
                .count(),
            1,
            "drop-close must close the subscription channel"
        );
    }
}
