use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::sync::{mpsc, oneshot};

use super::{
    ConsumerError, Delivery, SubscriptionId, SubscriptionPolicy,
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

        tokio::spawn(run_actor(
            subscriptions,
            max_in_flight.max(1),
            receiver,
            commands.clone(),
            metrics.clone(),
        ));
        for (subscription, stream) in streams {
            spawn_source(subscription, stream, commands.clone());
        }

        Ok(ConsumerHandle {
            commands,
            metrics,
            closed: Arc::new(AtomicBool::new(false)),
            next_waiter_id: Arc::new(AtomicU64::new(1)),
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
    metrics: Metrics,
    closed: Arc<AtomicBool>,
    next_waiter_id: Arc<AtomicU64>,
}

struct WaiterCancellation {
    waiter_id: u64,
    commands: mpsc::Sender<ConsumerCommand>,
    armed: bool,
}

impl Drop for WaiterCancellation {
    fn drop(&mut self) {
        if self.armed {
            let _ = self
                .commands
                .try_send(ConsumerCommand::CancelWaiter(self.waiter_id));
        }
    }
}

impl ConsumerHandle {
    #[must_use]
    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Waits for the next scheduled delivery while respecting max in-flight.
    ///
    /// # Errors
    ///
    /// Returns a typed source, transport, or closed-consumer error.
    pub async fn next(&self) -> Result<Delivery, ConsumerError> {
        let waiter_id = self.next_waiter_id.fetch_add(1, Ordering::Relaxed);
        let (completed, completion) = oneshot::channel();
        self.commands
            .send(ConsumerCommand::Next {
                waiter_id,
                completed,
            })
            .await
            .map_err(|_| ConsumerError::closed())?;
        let mut cancellation = WaiterCancellation {
            waiter_id,
            commands: self.commands.clone(),
            armed: true,
        };
        let result = completion.await.map_err(|_| ConsumerError::closed())?;
        cancellation.armed = false;
        result
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
