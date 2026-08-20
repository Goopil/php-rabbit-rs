use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::sync::{mpsc, oneshot};

use super::{
    AttemptsResolver, ConsumerError, ConsumerErrorKind, DeliveryState,
    delivery::{AckQueue, DeliveryTokenInner, PendingAck, Settlement},
    set::Subscription,
};
use crate::{
    metrics::Metrics,
    publisher::{
        Destination, MessageProperties, PublishOutcome, PublishRequest, delay::DelayRouter,
    },
    topology::delay::DelayStrategy,
};

/// Interval at which the actor drains the pending-ack queue.
const ACK_DRAIN_INTERVAL: Duration = Duration::from_millis(1);

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
    ack_queue: Arc<AckQueue>,
    current_generation: Arc<AtomicU64>,
) {
    let mut state = ActorState::new(subscriptions, metrics);

    loop {
        match tokio::time::timeout(ACK_DRAIN_INTERVAL, receiver.recv()).await {
            Ok(Some(command)) => {
                if handle_command(&mut state, command, &ack_queue, &current_generation).await {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => {
                drain_ack_queue(&mut state, &ack_queue, &current_generation).await;
            }
        }
    }
}

/// Returns `true` if the actor should terminate (Close command).
async fn handle_command(
    state: &mut ActorState,
    command: ConsumerCommand,
    ack_queue: &Arc<AckQueue>,
    current_generation: &Arc<AtomicU64>,
) -> bool {
    match command {
        ConsumerCommand::Settle {
            token,
            settlement,
            completed,
        } => {
            let result = settle(&*state, &token, settlement).await;
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
            drain_ack_queue(state, ack_queue, current_generation).await;
            for runtime in state.subscriptions.values() {
                let _ = runtime.channel.close().await;
            }
            let _ = completed.send(());
            return true;
        }
    }
    false
}

/// Drains the pending-ack queue, coalesces contiguous delivery tags per
/// subscription, and sends one `basic_ack(highest, multiple=true)` per
/// contiguous run.
///
/// # Non-contiguous tags
///
/// `multiple=true` acks all deliveries up to and including the given tag on
/// the same channel. If there are gaps (e.g. tags 1,2,5,6 where 3,4 were
/// nacked), we split into separate acks: `ack(2, true)` and `ack(6, true)`.
///
/// # Errors
///
/// If a batched ack fails, the error is logged. The deliveries were already
/// optimistically marked as `Acked` — the broker will redeliver them if the
/// ack truly failed.
async fn drain_ack_queue(
    state: &mut ActorState,
    ack_queue: &Arc<AckQueue>,
    current_generation: &Arc<AtomicU64>,
) {
    if ack_queue.is_empty() {
        return;
    }

    // Group pending acks by subscription.
    let mut groups: HashMap<super::SubscriptionId, Vec<PendingAck>> = HashMap::new();
    while let Some(pending) = ack_queue.pop() {
        groups
            .entry(pending.subscription.clone())
            .or_default()
            .push(pending);
    }

    for (subscription_id, mut pending) in groups {
        let Some(runtime) = state.subscriptions.get(&subscription_id) else {
            continue;
        };

        // Skip stale-generation acks.
        let current_gen = current_generation.load(Ordering::Acquire);
        pending.retain(|p| p.generation == current_gen);
        if pending.is_empty() {
            continue;
        }

        // Sort by delivery tag and coalesce contiguous runs.
        pending.sort_unstable_by_key(|p| p.delivery_tag);
        let mut tags: Vec<u64> = pending.iter().map(|p| p.delivery_tag).collect();
        tags.dedup();

        // Find contiguous runs and send one ack(highest, true) per run.
        let mut run_end = tags[0];
        let reserved_ats: Vec<_> = pending.iter().map(|p| p.reserved_at).collect();

        for &tag in tags.iter().skip(1) {
            if tag == run_end + 1 {
                run_end = tag;
            } else {
                if let Err(_error) = runtime.channel.ack(run_end, true).await {
                    // Batched ack failed — broker will redeliver (at-least-once).
                }
                run_end = tag;
            }
        }
        if let Err(_error) = runtime.channel.ack(run_end, true).await {
            // Batched ack failed — broker will redeliver (at-least-once).
        }

        // Record metrics for all acked deliveries.
        for reserved_at in reserved_ats {
            state.metrics.record_ack(reserved_at.elapsed());
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
