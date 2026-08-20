use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use crossbeam_queue::ArrayQueue;
use tokio::sync::{mpsc, oneshot};

use super::{
    ConsumerError, Delivery, MessageId, SubscriptionId, SubscriptionPolicy,
    actor::{ConsumerCommand, run_actor},
    attempts::AttemptsResolver,
    delivery::{ACK_QUEUE_CAPACITY, AckQueue, DeliveryIdentity, DeliveryToken, DeliveryTokenInner},
};
use crate::{
    metrics::{Metrics, MetricsSnapshot},
    pool::ConnectionKey,
    publisher::{Destination, PublisherHandle},
    topology::delay::DelayStrategy,
    transport::{ConsumerChannel, ConsumerRequest, DeliveryStream},
};

const COMMAND_CAPACITY: usize = 256;

pub(crate) enum BufferedDelivery {
    Delivery { delivery: Delivery, generation: u64 },
    Error(ConsumerError),
}

#[derive(Clone)]
pub struct Subscription {
    pub(crate) id: SubscriptionId,
    pub(crate) connection_key: ConnectionKey,
    pub(crate) generation: u64,
    pub(crate) channel_id: u16,
    pub(crate) queue: String,
    pub(crate) prefetch: u16,
    pub(crate) policy: SubscriptionPolicy,
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
    pub const fn policy(mut self, policy: SubscriptionPolicy) -> Self {
        self.policy = policy;
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
    ///
    /// # Panics
    ///
    /// Panics if a stream cannot be matched back to its subscription (internal invariant).
    pub async fn spawn_with_metrics(
        subscriptions: Vec<Subscription>,
        max_in_flight: usize,
        metrics: Metrics,
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

        let buffer_size = subscriptions
            .iter()
            .map(|s| (s.prefetch as usize * 3 / 2).max(1))
            .max()
            .unwrap_or(1);
        let (buffer_tx, buffer_rx) = flume::bounded(buffer_size);
        let current_generation = Arc::new(AtomicU64::new(
            subscriptions
                .iter()
                .map(|s| s.generation)
                .max()
                .unwrap_or(1),
        ));
        let ack_queue: Arc<AckQueue> = Arc::new(ArrayQueue::new(ACK_QUEUE_CAPACITY));

        tokio::spawn(run_actor(
            subscriptions.clone(),
            max_in_flight.max(1),
            receiver,
            commands.clone(),
            metrics.clone(),
            ack_queue.clone(),
            current_generation.clone(),
        ));
        for (subscription_id, stream) in streams {
            let subscription = subscriptions
                .iter()
                .find(|s| s.id == subscription_id)
                .expect("subscription exists");
            spawn_pump(
                subscription.clone(),
                stream,
                buffer_tx.clone(),
                commands.clone(),
                metrics.clone(),
                ack_queue.clone(),
                current_generation.clone(),
            );
        }

        Ok(ConsumerHandle {
            commands,
            buffer_rx,
            metrics,
            closed: Arc::new(AtomicBool::new(false)),
            current_generation,
        })
    }
}

fn spawn_pump(
    subscription: Subscription,
    mut stream: Box<dyn DeliveryStream>,
    buffer_tx: flume::Sender<BufferedDelivery>,
    commands: mpsc::Sender<ConsumerCommand>,
    metrics: Metrics,
    ack_queue: Arc<AckQueue>,
    current_generation: Arc<AtomicU64>,
) {
    tokio::spawn(async move {
        let connection_key = subscription.connection_key;
        let generation = subscription.generation;
        let channel_id = subscription.channel_id;
        let subscription_id = subscription.id.clone();

        while let Some(result) = stream.next().await {
            let delivery = match result {
                Ok(delivery) => delivery,
                Err(error) => {
                    let consumer_error =
                        ConsumerError::new(super::ConsumerErrorKind::Transport, error.to_string());
                    if buffer_tx
                        .send_async(BufferedDelivery::Error(consumer_error))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
            };

            let message_id = delivery.message_id.as_ref().map_or_else(
                || {
                    MessageId::new(format!(
                        "{generation}:{channel_id}:{}",
                        delivery.delivery_tag
                    ))
                },
                |message_id| MessageId::new(message_id.clone()),
            );
            let attempts = AttemptsResolver::default()
                .resolve(&delivery.headers, delivery.redelivered)
                .unwrap_or(if delivery.redelivered { 2 } else { 1 });
            let token = DeliveryToken::new(DeliveryTokenInner::pending(
                DeliveryIdentity {
                    subscription: subscription_id.clone(),
                    connection_key,
                    generation,
                    channel_id,
                    delivery_tag: delivery.delivery_tag,
                },
                message_id.clone(),
                delivery.correlation_id.clone(),
                delivery.payload.clone(),
                delivery.headers.clone(),
                attempts,
                commands.clone(),
                ack_queue.clone(),
                current_generation.clone(),
            ));
            let item = Delivery::new(
                message_id,
                delivery.correlation_id,
                subscription_id.clone(),
                delivery.payload,
                delivery.headers,
                attempts,
                token,
            );
            metrics.record_delivery();
            if buffer_tx
                .send_async(BufferedDelivery::Delivery {
                    delivery: item,
                    generation,
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
    buffer_rx: flume::Receiver<BufferedDelivery>,
    metrics: Metrics,
    closed: Arc<AtomicBool>,
    current_generation: Arc<AtomicU64>,
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

    /// Waits for the next scheduled delivery.
    ///
    /// The fast path uses `try_recv()` from the bounded flume buffer (sub-microsecond,
    /// no async runtime crossing). The slow path awaits `recv_async()` when the buffer
    /// is empty. Stale deliveries from a previous connection generation are discarded
    /// (`RabbitMQ` will redeliver them on the new connection).
    ///
    /// # Errors
    ///
    /// Returns a typed source, transport, or closed-consumer error.
    pub async fn next(&self) -> Result<Delivery, ConsumerError> {
        let generation = self.current_generation.load(Ordering::Acquire);
        loop {
            match self.buffer_rx.try_recv() {
                Ok(BufferedDelivery::Delivery {
                    delivery,
                    generation: deliv_gen,
                }) if deliv_gen == generation => return Ok(delivery),
                Ok(BufferedDelivery::Error(error)) => return Err(error),
                Ok(BufferedDelivery::Delivery { .. }) => {
                    continue;
                }
                Err(flume::TryRecvError::Empty) => {}
                Err(flume::TryRecvError::Disconnected) => return Err(ConsumerError::closed()),
            }
            match self.buffer_rx.recv_async().await {
                Ok(BufferedDelivery::Delivery {
                    delivery,
                    generation: deliv_gen,
                }) if deliv_gen == generation => return Ok(delivery),
                Ok(BufferedDelivery::Error(error)) => return Err(error),
                Ok(BufferedDelivery::Delivery { .. }) => {}
                Err(flume::RecvError::Disconnected) => return Err(ConsumerError::closed()),
            }
        }
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
        self.current_generation.store(generation, Ordering::Release);
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
