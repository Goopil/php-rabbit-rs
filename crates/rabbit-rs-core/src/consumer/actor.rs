use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::{mpsc, oneshot};

use super::{
    AttemptsResolver, ConsumerError, ConsumerErrorKind, Delivery, DeliveryState, MessageId,
    Scheduler, SubscriptionId, WeightedFairScheduler,
    delivery::{DeliveryIdentity, DeliveryToken, DeliveryTokenInner, Settlement},
    set::Subscription,
};
use crate::{
    metrics::Metrics,
    publisher::{
        Destination, MessageProperties, PublishOutcome, PublishRequest, delay::DelayRouter,
    },
    topology::delay::DelayStrategy,
    transport::{Delivery as TransportDelivery, TransportResult},
};

pub(crate) enum ConsumerCommand {
    Incoming {
        subscription: SubscriptionId,
        result: TransportResult<TransportDelivery>,
    },
    Next(oneshot::Sender<Result<Delivery, ConsumerError>>),
    Settle {
        token: Arc<DeliveryTokenInner>,
        settlement: Settlement,
        completed: oneshot::Sender<Result<DeliveryState, ConsumerError>>,
    },
    UpdateGeneration {
        subscription: SubscriptionId,
        generation: u64,
        completed: oneshot::Sender<Result<(), ConsumerError>>,
    },
    Close(oneshot::Sender<()>),
}

struct RuntimeSubscription {
    connection_key: crate::pool::ConnectionKey,
    generation: u64,
    channel_id: u16,
    channel: Arc<dyn crate::transport::ConsumerChannel>,
    publisher: Option<crate::publisher::PublisherHandle>,
    destination: Option<crate::publisher::Destination>,
    delay_strategy: Option<DelayStrategy>,
}

struct ActorState {
    subscriptions: HashMap<SubscriptionId, RuntimeSubscription>,
    buffers: HashMap<SubscriptionId, VecDeque<TransportDelivery>>,
    source_errors: VecDeque<ConsumerError>,
    scheduler: WeightedFairScheduler,
    waiting: VecDeque<oneshot::Sender<Result<Delivery, ConsumerError>>>,
    in_flight: usize,
    max_in_flight: usize,
    commands: mpsc::Sender<ConsumerCommand>,
    metrics: Metrics,
}

impl ActorState {
    fn new(
        subscriptions: Vec<Subscription>,
        max_in_flight: usize,
        commands: mpsc::Sender<ConsumerCommand>,
        metrics: Metrics,
    ) -> Self {
        let mut scheduler = WeightedFairScheduler::default();
        let mut runtime = HashMap::new();
        let mut buffers = HashMap::new();
        for subscription in subscriptions {
            scheduler.register(subscription.id.clone(), subscription.policy);
            buffers.insert(subscription.id.clone(), VecDeque::new());
            runtime.insert(
                subscription.id,
                RuntimeSubscription {
                    connection_key: subscription.connection_key,
                    generation: subscription.generation,
                    channel_id: subscription.channel_id,
                    channel: subscription.channel,
                    publisher: subscription.publisher,
                    destination: subscription.destination,
                    delay_strategy: subscription.delay_strategy,
                },
            );
        }

        Self {
            subscriptions: runtime,
            buffers,
            source_errors: VecDeque::new(),
            scheduler,
            waiting: VecDeque::new(),
            in_flight: 0,
            max_in_flight,
            commands,
            metrics,
        }
    }

    fn dispatch(&mut self) {
        while self.in_flight < self.max_in_flight {
            let Some(waiter) = self.waiting.pop_front() else {
                return;
            };
            if let Some(error) = self.source_errors.pop_front() {
                let _ = waiter.send(Err(error));
                continue;
            }
            let Some(subscription) = self.scheduler.next(Instant::now()) else {
                self.waiting.push_front(waiter);
                return;
            };
            let Some(delivery) = self
                .buffers
                .get_mut(&subscription)
                .and_then(VecDeque::pop_front)
            else {
                self.scheduler.mark_empty(&subscription);
                self.waiting.push_front(waiter);
                return;
            };
            if self
                .buffers
                .get(&subscription)
                .is_none_or(VecDeque::is_empty)
            {
                self.scheduler.mark_empty(&subscription);
            }
            let Some(runtime) = self.subscriptions.get(&subscription) else {
                let _ = waiter.send(Err(ConsumerError::new(
                    ConsumerErrorKind::InvalidSubscription,
                    "delivery references an unknown subscription",
                )));
                continue;
            };
            let message_id = MessageId::new(format!(
                "{}:{}:{}",
                runtime.generation, runtime.channel_id, delivery.delivery_tag
            ));
            let attempts = AttemptsResolver::default()
                .resolve(&delivery.headers, delivery.redelivered)
                .unwrap_or(if delivery.redelivered { 2 } else { 1 });
            let token = DeliveryToken::new(DeliveryTokenInner::pending(
                DeliveryIdentity {
                    subscription: subscription.clone(),
                    connection_key: runtime.connection_key,
                    generation: runtime.generation,
                    channel_id: runtime.channel_id,
                    delivery_tag: delivery.delivery_tag,
                },
                message_id.clone(),
                delivery.payload.clone(),
                delivery.headers.clone(),
                attempts,
                self.commands.clone(),
            ));
            let item = Delivery::new(
                message_id,
                subscription,
                delivery.payload,
                delivery.headers,
                attempts,
                token,
            );
            if waiter.send(Ok(item)).is_ok() {
                self.metrics.record_delivery();
                self.in_flight = self.in_flight.saturating_add(1);
            }
        }
    }

    fn release_budget(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
        self.dispatch();
    }
}

pub(crate) async fn run_actor(
    subscriptions: Vec<Subscription>,
    max_in_flight: usize,
    mut receiver: mpsc::Receiver<ConsumerCommand>,
    commands: mpsc::Sender<ConsumerCommand>,
    metrics: Metrics,
) {
    let mut state = ActorState::new(subscriptions, max_in_flight, commands, metrics);
    while let Some(command) = receiver.recv().await {
        match command {
            ConsumerCommand::Incoming {
                subscription,
                result,
            } => match result {
                Ok(delivery) => {
                    if let Some(buffer) = state.buffers.get_mut(&subscription) {
                        buffer.push_back(delivery);
                        state.scheduler.mark_ready(&subscription);
                    }
                    state.dispatch();
                }
                Err(error) => {
                    state.source_errors.push_back(ConsumerError::new(
                        ConsumerErrorKind::Transport,
                        error.to_string(),
                    ));
                    state.dispatch();
                }
            },
            ConsumerCommand::Next(waiter) => {
                state.waiting.push_back(waiter);
                state.dispatch();
            }
            ConsumerCommand::Settle {
                token,
                settlement,
                completed,
            } => {
                let result = settle(&state, &token, settlement).await;
                match result {
                    Ok(terminal) => {
                        match terminal {
                            DeliveryState::Acked => {
                                state.metrics.record_ack(token.reserved_at.elapsed());
                            }
                            DeliveryState::Rejected => {
                                state.metrics.record_reject(token.reserved_at.elapsed());
                            }
                            DeliveryState::Pending | DeliveryState::Lost => {}
                        }
                        state.release_budget();
                        let _ = completed.send(Ok(terminal));
                    }
                    Err(error) if error.kind() == ConsumerErrorKind::StaleGeneration => {
                        state.release_budget();
                        let _ = completed.send(Err(error));
                    }
                    Err(error) => {
                        let _ = completed.send(Err(error));
                    }
                }
            }
            ConsumerCommand::UpdateGeneration {
                subscription,
                generation,
                completed,
            } => {
                let result = state.subscriptions.get_mut(&subscription).map_or_else(
                    || {
                        Err(ConsumerError::new(
                            ConsumerErrorKind::InvalidSubscription,
                            "cannot update an unknown subscription",
                        ))
                    },
                    |runtime| {
                        runtime.generation = generation;
                        Ok(())
                    },
                );
                let _ = completed.send(result);
            }
            ConsumerCommand::Close(completed) => {
                let error = ConsumerError::closed();
                for waiter in state.waiting.drain(..) {
                    let _ = waiter.send(Err(error.clone()));
                }
                let _ = completed.send(());
                return;
            }
        }
    }
}

async fn settle(
    state: &ActorState,
    token: &DeliveryTokenInner,
    settlement: Settlement,
) -> Result<DeliveryState, ConsumerError> {
    let runtime = state
        .subscriptions
        .get(&token.subscription)
        .ok_or_else(|| {
            ConsumerError::new(
                ConsumerErrorKind::InvalidSubscription,
                "delivery references an unknown subscription",
            )
        })?;
    if runtime.connection_key != token.connection_key
        || runtime.generation != token.generation
        || runtime.channel_id != token.channel_id
    {
        return Err(ConsumerError::new(
            ConsumerErrorKind::StaleGeneration,
            "delivery belongs to a stale connection generation or channel",
        ));
    }

    match settlement {
        Settlement::Ack => {
            runtime
                .channel
                .ack(token.delivery_tag, false)
                .await
                .map_err(|error| transport_error(&error))?;
            Ok(DeliveryState::Acked)
        }
        Settlement::Release(delay) if delay.is_zero() => {
            runtime
                .channel
                .reject(token.delivery_tag, true)
                .await
                .map_err(|error| transport_error(&error))?;
            Ok(DeliveryState::Rejected)
        }
        Settlement::Release(delay) => {
            delayed_release(runtime, token, delay).await?;
            Ok(DeliveryState::Acked)
        }
    }
}

async fn delayed_release(
    runtime: &RuntimeSubscription,
    token: &DeliveryTokenInner,
    delay: Duration,
) -> Result<(), ConsumerError> {
    let publisher = runtime.publisher.as_ref().ok_or_else(|| {
        ConsumerError::new(
            ConsumerErrorKind::MissingPublisher,
            "delayed release requires a publisher",
        )
    })?;
    let destination = runtime.destination.as_ref().ok_or_else(|| {
        ConsumerError::new(
            ConsumerErrorKind::MissingPublisher,
            "delayed release requires a destination",
        )
    })?;
    let strategy = runtime.delay_strategy.as_ref().ok_or_else(|| {
        ConsumerError::new(
            ConsumerErrorKind::MissingPublisher,
            "delayed release requires a resolved delay strategy",
        )
    })?;
    let delay_ms = i64::try_from(delay.as_millis()).map_err(|_| {
        ConsumerError::new(ConsumerErrorKind::Publish, "delay exceeds supported range")
    })?;
    let route = DelayRouter::route(strategy, destination, delay_ms)
        .map_err(|error| ConsumerError::new(ConsumerErrorKind::Publish, error.to_string()))?;
    let mut properties = MessageProperties::new(token.message_id.as_str());
    properties.headers = AttemptsResolver::default()
        .delayed_headers(&token.headers, token.attempts)
        .map_err(|error| ConsumerError::new(ConsumerErrorKind::MaxAttempts, error.to_string()))?;
    if route.queue.is_none() {
        properties.delay_ms = Some(route.delay_ms);
    }
    let request = PublishRequest::new(
        Destination::new(route.exchange, route.routing_key),
        token.payload.clone(),
        properties,
        tokio::time::Instant::now() + Duration::from_secs(30),
    );
    let outcome = publisher
        .try_publish(request)
        .map_err(|error| publish_error(&error))?
        .wait()
        .await
        .map_err(|error| publish_error(&error))?;
    if !matches!(outcome, PublishOutcome::Confirmed { .. }) {
        return Err(ConsumerError::new(
            ConsumerErrorKind::Publish,
            "delayed release was not confirmed",
        ));
    }
    runtime
        .channel
        .ack(token.delivery_tag, false)
        .await
        .map_err(|error| transport_error(&error))
}

fn transport_error(error: &crate::transport::TransportError) -> ConsumerError {
    ConsumerError::new(ConsumerErrorKind::Transport, error.to_string())
}

fn publish_error(error: &crate::publisher::PublishError) -> ConsumerError {
    ConsumerError::new(ConsumerErrorKind::Publish, error.to_string())
}
