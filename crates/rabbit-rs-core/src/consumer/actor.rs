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
    in_flight: usize,
    max_in_flight: usize,
    commands: mpsc::Sender<ConsumerCommand>,
    buffer_tx: flume::Sender<Result<Delivery, ConsumerError>>,
    metrics: Metrics,
}

impl ActorState {
    fn new(
        subscriptions: Vec<Subscription>,
        max_in_flight: usize,
        commands: mpsc::Sender<ConsumerCommand>,
        buffer_tx: flume::Sender<Result<Delivery, ConsumerError>>,
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
            in_flight: 0,
            max_in_flight,
            commands,
            buffer_tx,
            metrics,
        }
    }

    fn dispatch(&mut self) {
        while self.in_flight < self.max_in_flight {
            if let Some(error) = self.source_errors.front() {
                if self.buffer_tx.try_send(Err(error.clone())).is_err() {
                    return;
                }
                self.source_errors.pop_front();
                continue;
            }
            let Some(subscription) = self.scheduler.next(Instant::now()) else {
                return;
            };
            let Some(delivery) = self
                .buffers
                .get(&subscription)
                .and_then(VecDeque::front)
                .cloned()
            else {
                self.scheduler.mark_empty(&subscription);
                return;
            };
            let Some(runtime) = self.subscriptions.get(&subscription) else {
                continue;
            };
            let message_id = delivery.message_id.as_ref().map_or_else(
                || {
                    MessageId::new(format!(
                        "{}:{}:{}",
                        runtime.generation, runtime.channel_id, delivery.delivery_tag
                    ))
                },
                |message_id| MessageId::new(message_id.clone()),
            );
            let attempts = AttemptsResolver::default()
                .resolve(&delivery.headers, delivery.redelivered)
                .unwrap_or(if delivery.redelivered { 2 } else { 1 });
            let headers = Arc::new(delivery.headers.clone());
            let token = DeliveryToken::new(DeliveryTokenInner::pending(
                DeliveryIdentity {
                    subscription: subscription.clone(),
                    connection_key: runtime.connection_key,
                    generation: runtime.generation,
                    channel_id: runtime.channel_id,
                    delivery_tag: delivery.delivery_tag,
                },
                message_id.clone(),
                delivery.correlation_id.clone(),
                delivery.payload.clone(),
                headers.clone(),
                attempts,
                self.commands.clone(),
            ));
            let item = Delivery::new(
                message_id,
                delivery.correlation_id,
                subscription.clone(),
                delivery.payload,
                headers,
                attempts,
                token,
            );
            if self.buffer_tx.try_send(Ok(item)).is_err() {
                self.scheduler.mark_ready(&subscription);
                return;
            }
            if let Some(buffer) = self.buffers.get_mut(&subscription) {
                buffer.pop_front();
                if buffer.is_empty() {
                    self.scheduler.mark_empty(&subscription);
                }
            }
            self.metrics.record_delivery();
            self.in_flight = self.in_flight.saturating_add(1);
        }
    }

    fn record_source_error(&mut self, error: ConsumerError) {
        if self.source_errors.len() >= self.max_in_flight.max(64) {
            self.source_errors.pop_front();
        }
        self.source_errors.push_back(error);
    }

    fn release_budget(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }
}

pub(crate) async fn run_actor(
    subscriptions: Vec<Subscription>,
    max_in_flight: usize,
    mut receiver: mpsc::Receiver<ConsumerCommand>,
    commands: mpsc::Sender<ConsumerCommand>,
    buffer_tx: flume::Sender<Result<Delivery, ConsumerError>>,
    metrics: Metrics,
    dispatch_notify: Arc<tokio::sync::Notify>,
) {
    let mut state = ActorState::new(subscriptions, max_in_flight, commands, buffer_tx, metrics);
    let mut dispatch_timer = tokio::time::interval(Duration::from_millis(1));
    dispatch_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Allow pumps to push deliveries before the first dispatch.
    tokio::task::yield_now().await;
    loop {
        tokio::select! {
            command = receiver.recv() => match command {
                Some(ConsumerCommand::Incoming {
                    subscription,
                    result,
                }) => match result {
                    Ok(delivery) => {
                        if let Some(buffer) = state.buffers.get_mut(&subscription) {
                            buffer.push_back(delivery);
                            state.scheduler.mark_ready(&subscription);
                        }
                    }
                    Err(error) => {
                        state.record_source_error(ConsumerError::new(
                            ConsumerErrorKind::Transport,
                            error.to_string(),
                        ));
                    }
                },
                Some(ConsumerCommand::Settle {
                    token,
                    settlement,
                    completed,
                }) => {
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
                            state.dispatch();
                            let _ = completed.send(Ok(terminal));
                        }
                        Err(error) if error.kind() == ConsumerErrorKind::StaleGeneration => {
                            state.release_budget();
                            state.dispatch();
                            let _ = completed.send(Err(error));
                        }
                        Err(error) => {
                            let _ = completed.send(Err(error));
                        }
                    }
                }
                Some(ConsumerCommand::UpdateGeneration {
                    subscription,
                    generation,
                    completed,
                }) => {
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
                Some(ConsumerCommand::Close(completed)) => {
                    for runtime in state.subscriptions.values() {
                        let _ = runtime.channel.close().await;
                    }
                    let _ = completed.send(());
                    return;
                }
                None => return,
            },
            () = dispatch_notify.notified() => {
                state.dispatch();
            }
            _ = dispatch_timer.tick() => {
                state.dispatch();
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
        Settlement::Reject(requeue) => {
            runtime
                .channel
                .reject(token.delivery_tag, requeue)
                .await
                .map_err(|error| transport_error(&error))?;
            Ok(DeliveryState::Rejected)
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
    properties.correlation_id.clone_from(&token.correlation_id);
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
        tokio::time::Instant::now() + publisher.confirm_timeout(),
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
