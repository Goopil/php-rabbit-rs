use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::{mpsc, oneshot};

use super::{
    AttemptsResolver, ConsumerError, ConsumerErrorKind, DeliveryState,
    delivery::{DeliveryTokenInner, Settlement},
    set::Subscription,
};
use crate::{
    metrics::Metrics,
    publisher::{
        Destination, MessageProperties, PublishOutcome, PublishRequest, delay::DelayRouter,
    },
    topology::delay::DelayStrategy,
};

pub(crate) enum ConsumerCommand {
    Settle {
        token: Arc<DeliveryTokenInner>,
        settlement: Settlement,
        completed: oneshot::Sender<Result<DeliveryState, ConsumerError>>,
    },
    UpdateGeneration {
        subscription: super::SubscriptionId,
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
    subscriptions: HashMap<super::SubscriptionId, RuntimeSubscription>,
    metrics: Metrics,
}

impl ActorState {
    fn new(subscriptions: Vec<Subscription>, metrics: Metrics) -> Self {
        let mut runtime = HashMap::new();
        for subscription in subscriptions {
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
            metrics,
        }
    }
}

pub(crate) async fn run_actor(
    subscriptions: Vec<Subscription>,
    _max_in_flight: usize,
    mut receiver: mpsc::Receiver<ConsumerCommand>,
    _commands: mpsc::Sender<ConsumerCommand>,
    metrics: Metrics,
) {
    let mut state = ActorState::new(subscriptions, metrics);
    while let Some(command) = receiver.recv().await {
        match command {
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
                        let _ = completed.send(Ok(terminal));
                    }
                    Err(error) if error.kind() == ConsumerErrorKind::StaleGeneration => {
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
                for runtime in state.subscriptions.values() {
                    let _ = runtime.channel.close().await;
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
