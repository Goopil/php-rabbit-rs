use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    num::NonZeroU32,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot};

use super::{
    AttemptsResolver, ConsumerError, ConsumerErrorKind, Delivery, DeliveryState, MessageId,
    SubscriptionId, WeightedFairScheduler,
    delivery::{DeliveryIdentity, DeliveryToken, DeliveryTokenInner, Settlement, SettlementError},
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

type ChannelKey = (SubscriptionId, u16, u64);

/// Upper bound on retained source errors so a flapping transport can neither
/// grow the deque without bound nor starve good deliveries behind errors.
const SOURCE_ERROR_CAPACITY: usize = 64;

struct ChannelLedgerEntry {
    state: DeliveryState,
    token: Option<Arc<DeliveryTokenInner>>,
}

#[derive(Default)]
struct ChannelLedger {
    pending: std::collections::BTreeMap<u64, ChannelLedgerEntry>,
    acked_prefix: u64,
}

struct SettleParams {
    token: Arc<DeliveryTokenInner>,
    settlement: Settlement,
}

struct SettlementResult {
    channel_key: ChannelKey,
    token: Arc<DeliveryTokenInner>,
    result: Result<DeliveryState, ConsumerError>,
}

type SettlementFuture = Pin<Box<dyn Future<Output = SettlementResult> + Send>>;

struct SettleThroughParams {
    token: Arc<DeliveryTokenInner>,
    affected_tokens: Vec<Arc<DeliveryTokenInner>>,
}

struct SettleThroughResult {
    channel_key: ChannelKey,
    target_tag: u64,
    affected_tokens: Vec<Arc<DeliveryTokenInner>>,
    result: Result<DeliveryState, ConsumerError>,
}

type SettleThroughFuture = Pin<Box<dyn Future<Output = SettleThroughResult> + Send>>;

pub(crate) enum ConsumerCommand {
    Incoming {
        subscription: SubscriptionId,
        result: TransportResult<TransportDelivery>,
    },
    Settle {
        token: Arc<DeliveryTokenInner>,
        settlement: Settlement,
    },
    SettleThrough {
        token: Arc<DeliveryTokenInner>,
    },
}

struct RuntimeSubscription {
    connection_key: crate::pool::ConnectionKey,
    generation: u64,
    channel_id: u16,
    channel: Arc<dyn crate::transport::ConsumerChannel>,
    publisher: Option<crate::publisher::PublisherHandle>,
    destination: Option<crate::publisher::Destination>,
    delay_strategy: Option<DelayStrategy>,
    early_ack: bool,
    no_ack: bool,
    max_attempts: Option<NonZeroU32>,
    has_dead_letter: bool,
}

struct ActorState {
    subscriptions: HashMap<SubscriptionId, RuntimeSubscription>,
    buffers: HashMap<SubscriptionId, VecDeque<TransportDelivery>>,
    buffered_bytes: HashMap<SubscriptionId, u64>,
    max_buffered_bytes: HashMap<SubscriptionId, u64>,
    channel_ledgers: HashMap<ChannelKey, ChannelLedger>,
    /// Over-budget deliveries waiting for the byte budget to free up. Count
    /// bounded by `pending_capacity`: in `no_ack` mode the broker auto-acks,
    /// so broker `QoS` does not bound delivery and this deque would otherwise
    /// grow without limit (audit F-04).
    pending_incoming: VecDeque<(SubscriptionId, TransportDelivery)>,
    pending_capacity: usize,
    pending_settlements: futures_util::stream::FuturesUnordered<SettlementFuture>,
    pending_settle_throughs: futures_util::stream::FuturesUnordered<SettleThroughFuture>,
    settlement_in_flight: HashSet<ChannelKey>,
    settlement_queues: HashMap<ChannelKey, VecDeque<SettleParams>>,
    settle_through_queues: HashMap<ChannelKey, VecDeque<SettleThroughParams>>,
    source_errors: VecDeque<ConsumerError>,
    scheduler: WeightedFairScheduler,
    commands: mpsc::Sender<ConsumerCommand>,
    buffer_tx: flume::Sender<Result<Delivery, ConsumerError>>,
    error_tx: flume::Sender<SettlementError>,
    error_rx: flume::Receiver<SettlementError>,
    close_completion: Arc<std::sync::Mutex<Option<oneshot::Sender<()>>>>,
    metrics: Metrics,
}

impl ActorState {
    #[allow(clippy::too_many_arguments)]
    fn new(
        subscriptions: Vec<Subscription>,
        commands: mpsc::Sender<ConsumerCommand>,
        buffer_tx: flume::Sender<Result<Delivery, ConsumerError>>,
        error_tx: flume::Sender<SettlementError>,
        error_rx: flume::Receiver<SettlementError>,
        close_completion: Arc<std::sync::Mutex<Option<oneshot::Sender<()>>>>,
        metrics: Metrics,
        pending_capacity: usize,
    ) -> Self {
        let mut scheduler = WeightedFairScheduler::default();
        let mut runtime = HashMap::new();
        let mut buffers = HashMap::new();
        let mut buffered_bytes = HashMap::new();
        let mut max_buffered_bytes = HashMap::new();
        let mut channel_ledgers = HashMap::new();
        for subscription in subscriptions {
            scheduler.register(subscription.id.clone(), subscription.policy);
            buffers.insert(subscription.id.clone(), VecDeque::new());
            buffered_bytes.insert(subscription.id.clone(), 0);
            max_buffered_bytes.insert(subscription.id.clone(), subscription.max_buffered_bytes);
            let channel_key = (
                subscription.id.clone(),
                subscription.channel_id,
                subscription.generation,
            );
            channel_ledgers.insert(channel_key, ChannelLedger::default());
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
                    early_ack: subscription.early_ack,
                    no_ack: subscription.no_ack,
                    max_attempts: subscription.max_attempts,
                    has_dead_letter: subscription.dead_letter,
                },
            );
        }

        Self {
            subscriptions: runtime,
            buffers,
            buffered_bytes,
            max_buffered_bytes,
            channel_ledgers,
            pending_incoming: VecDeque::new(),
            pending_capacity,
            pending_settlements: futures_util::stream::FuturesUnordered::new(),
            pending_settle_throughs: futures_util::stream::FuturesUnordered::new(),
            settlement_in_flight: HashSet::new(),
            settlement_queues: HashMap::new(),
            settle_through_queues: HashMap::new(),
            source_errors: VecDeque::new(),
            scheduler,
            commands,
            buffer_tx,
            error_tx,
            error_rx,
            close_completion,
            metrics,
        }
    }

    fn channel_key_for(&self, subscription: &SubscriptionId) -> Option<ChannelKey> {
        self.subscriptions
            .get(subscription)
            .map(|runtime| (subscription.clone(), runtime.channel_id, runtime.generation))
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch(&mut self) {
        self.drain_pending();
        loop {
            if let Some(error) = self.source_errors.front() {
                if self.buffer_tx.try_send(Err(error.clone())).is_err() {
                    break;
                }
                self.source_errors.pop_front();
                continue;
            }
            let Some(subscription) = self.scheduler.next(Instant::now()) else {
                break;
            };
            let Some(delivery) = self
                .buffers
                .get_mut(&subscription)
                .and_then(VecDeque::pop_front)
            else {
                self.scheduler.mark_empty(&subscription);
                break;
            };
            let Some(runtime) = self.subscriptions.get(&subscription) else {
                self.buffers
                    .entry(subscription.clone())
                    .or_default()
                    .push_front(delivery);
                self.scheduler.mark_ready(&subscription);
                continue;
            };
            let generation = runtime.generation;
            let channel_id = runtime.channel_id;
            let connection_key = runtime.connection_key;
            let early_ack = runtime.early_ack;
            let no_ack = runtime.no_ack;
            let max_attempts = runtime.max_attempts;
            let has_dead_letter = runtime.has_dead_letter;
            let channel = Arc::clone(&runtime.channel);
            let message_id = delivery.message_id.as_ref().map_or_else(
                || {
                    MessageId::new(format!(
                        "{generation}:{channel_id}:{}",
                        delivery.delivery_tag
                    ))
                },
                |message_id| MessageId::new(message_id.clone()),
            );
            let resolved = AttemptsResolver::default()
                .with_max_attempts(max_attempts)
                .resolve(&delivery.headers, delivery.redelivered);
            let attempts = match resolved {
                Ok(attempts) => attempts,
                // Best-effort mode auto-acks the delivery anyway: surface the
                // resolve error and dispatch with the true attempt count so
                // embedders can still fail the job on attempts.
                Err(error) if early_ack => {
                    self.record_settlement_error(SettlementError {
                        delivery_tag: delivery.delivery_tag,
                        subscription: subscription.clone(),
                        kind: ConsumerErrorKind::MaxAttempts,
                        message: error.to_string(),
                    });
                    error.attempts()
                }
                Err(error) => {
                    let payload_bytes = u64::try_from(delivery.payload.len()).unwrap_or(u64::MAX);
                    self.settle_poison(
                        &subscription,
                        has_dead_letter,
                        delivery.delivery_tag,
                        &message_id,
                        payload_bytes,
                        &error.to_string(),
                        Duration::ZERO,
                    );
                    continue;
                }
            };
            let headers = Arc::clone(&delivery.headers);

            if early_ack {
                let delivery_bytes = u64::try_from(delivery.payload.len()).unwrap_or(u64::MAX);
                if !no_ack {
                    let tag = delivery.delivery_tag;
                    tokio::spawn(async move {
                        let _ = channel.ack(tag, false).await;
                    });
                }
                let item = Delivery::new_auto_acked(
                    DeliveryIdentity {
                        subscription: subscription.clone(),
                        connection_key,
                        generation,
                        channel_id,
                        delivery_tag: delivery.delivery_tag,
                    },
                    message_id,
                    delivery.correlation_id.clone(),
                    delivery.payload.clone(),
                    headers,
                    attempts,
                );
                if self.buffer_tx.try_send(Ok(item)).is_err() {
                    self.buffers
                        .entry(subscription.clone())
                        .or_default()
                        .push_front(delivery);
                    self.scheduler.mark_ready(&subscription);
                    break;
                }
                if let Some(buffer) = self.buffers.get_mut(&subscription)
                    && buffer.is_empty()
                {
                    self.scheduler.mark_empty(&subscription);
                }
                if let Some(bytes) = self.buffered_bytes.get_mut(&subscription) {
                    *bytes = bytes.saturating_sub(delivery_bytes);
                }
                self.metrics.record_delivery();
                self.metrics.record_ack(Duration::ZERO);
                continue;
            }

            let token = DeliveryToken::new(DeliveryTokenInner::pending(
                DeliveryIdentity {
                    subscription: subscription.clone(),
                    connection_key,
                    generation,
                    channel_id,
                    delivery_tag: delivery.delivery_tag,
                },
                message_id.clone(),
                delivery.correlation_id.clone(),
                delivery.payload.clone(),
                headers.clone(),
                attempts,
                self.commands.clone(),
            ));
            if let Some(channel_key) = self.channel_key_for(&subscription)
                && let Some(ledger) = self.channel_ledgers.get_mut(&channel_key)
                && let Some(entry) = ledger.pending.get_mut(&delivery.delivery_tag)
            {
                entry.token = Some(token.inner().clone());
            }
            let item = Delivery::new(
                message_id,
                delivery.correlation_id.clone(),
                subscription.clone(),
                delivery.payload.clone(),
                headers,
                attempts,
                token,
            );
            if self.buffer_tx.try_send(Ok(item)).is_err() {
                self.buffers
                    .entry(subscription.clone())
                    .or_default()
                    .push_front(delivery);
                self.scheduler.mark_ready(&subscription);
                break;
            }
            if let Some(buffer) = self.buffers.get_mut(&subscription)
                && buffer.is_empty()
            {
                self.scheduler.mark_empty(&subscription);
            }
            self.metrics.record_delivery();
        }
    }

    fn record_source_error(&mut self, error: ConsumerError) {
        if self.source_errors.len() >= SOURCE_ERROR_CAPACITY {
            self.source_errors.pop_front();
        }
        self.source_errors.push_back(error);
    }

    /// Records a settlement error without ever blocking the actor.
    ///
    /// The error channel is bounded (`ERROR_CHANNEL_CAPACITY`). When full, the
    /// oldest error is dropped to make room — the actor must never stall
    /// waiting for the embedder to drain, matching the documented contract of
    /// `ConsumerHandle::drain_errors`.
    fn record_settlement_error(&mut self, error: SettlementError) {
        if self.error_tx.is_full() {
            let _ = self.error_rx.try_recv();
        }
        let _ = self.error_tx.send(error);
    }

    /// Settles a poison delivery — attempts above the configured maximum —
    /// terminally, never requeueing it. With a bound dead-letter exchange the
    /// delivery is rejected with `requeue=false` so the broker routes it to
    /// the DLX; without one, the documented policy is an explicit acknowledge
    /// recorded as a `MaxAttempts` settlement error (ack-and-log).
    ///
    /// The channel operation is fire-and-forget: a transient transport
    /// failure redelivers the message, which re-enters this path and retries.
    #[allow(clippy::too_many_arguments)]
    fn settle_poison(
        &mut self,
        subscription: &SubscriptionId,
        has_dead_letter: bool,
        delivery_tag: u64,
        message_id: &MessageId,
        payload_bytes: u64,
        detail: &str,
        settled_for: Duration,
    ) {
        let channel = self
            .subscriptions
            .get(subscription)
            .map(|runtime| Arc::clone(&runtime.channel));
        if let Some(channel) = channel {
            spawn_poison_settlement(channel, has_dead_letter, delivery_tag);
        }
        // An unknown subscription has nothing to settle against the broker;
        // the recorded error below still surfaces the poison condition.
        self.record_poison_metrics(has_dead_letter, settled_for);
        if let Some(bytes) = self.buffered_bytes.get_mut(subscription) {
            *bytes = bytes.saturating_sub(payload_bytes);
        }
        if let Some(channel_key) = self.channel_key_for(subscription)
            && let Some(ledger) = self.channel_ledgers.get_mut(&channel_key)
        {
            ledger.pending.remove(&delivery_tag);
        }
        self.record_settlement_error(SettlementError {
            delivery_tag,
            subscription: subscription.clone(),
            kind: ConsumerErrorKind::MaxAttempts,
            message: poison_settlement_message(detail, message_id, has_dead_letter),
        });
    }

    fn record_poison_metrics(&mut self, has_dead_letter: bool, settled_for: Duration) {
        if has_dead_letter {
            self.metrics.record_reject(settled_for);
        } else {
            self.metrics.record_ack(settled_for);
        }
    }

    fn drain_pending(&mut self) {
        while let Some((subscription, delivery)) = self.pending_incoming.front() {
            let delivery_bytes = u64::try_from(delivery.payload.len()).unwrap_or(u64::MAX);
            let over_budget = if let Some(max) = self.max_buffered_bytes.get(subscription) {
                let current = self.buffered_bytes.get(subscription).copied().unwrap_or(0);
                current.saturating_add(delivery_bytes) > *max
            } else {
                false
            };
            if over_budget {
                break;
            }
            let (subscription, delivery) = self
                .pending_incoming
                .pop_front()
                .expect("front checked above");
            if let Some(buffer) = self.buffers.get_mut(&subscription) {
                buffer.push_back(delivery);
                self.scheduler.mark_ready(&subscription);
            }
            if let Some(bytes) = self.buffered_bytes.get_mut(&subscription) {
                *bytes = bytes.saturating_add(delivery_bytes);
            }
        }
    }

    fn try_drain_pending(&mut self) {
        self.drain_pending();
        self.dispatch();
    }
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_actor(
    subscriptions: Vec<Subscription>,
    mut receiver: mpsc::Receiver<ConsumerCommand>,
    commands: mpsc::Sender<ConsumerCommand>,
    buffer_tx: flume::Sender<Result<Delivery, ConsumerError>>,
    error_tx: flume::Sender<SettlementError>,
    error_rx: flume::Receiver<SettlementError>,
    metrics: Metrics,
    dispatch_notify: Arc<tokio::sync::Notify>,
    mut close_rx: tokio::sync::watch::Receiver<bool>,
    close_completion: Arc<std::sync::Mutex<Option<oneshot::Sender<()>>>>,
    pending_capacity: usize,
) {
    let mut state = ActorState::new(
        subscriptions,
        commands,
        buffer_tx,
        error_tx,
        error_rx,
        close_completion,
        metrics,
        pending_capacity,
    );
    // Allow pumps to push deliveries before the first dispatch.
    tokio::task::yield_now().await;
    loop {
        tokio::select! {
            command = receiver.recv(),
                if state.pending_incoming.len() < state.pending_capacity =>
            match command {
                Some(ConsumerCommand::Incoming {
                    subscription,
                    result,
                }) => match result {
                    Ok(delivery) => {
                        let delivery_bytes = u64::try_from(delivery.payload.len()).unwrap_or(u64::MAX);
                        if let Some(channel_key) = state.channel_key_for(&subscription) {
                            state.channel_ledgers
                                .entry(channel_key)
                                .or_default()
                                .pending
                                .insert(delivery.delivery_tag, ChannelLedgerEntry {
                                    state: DeliveryState::Pending,
                                    token: None,
                                });
                        }
                        let over_budget = if let Some(max) = state.max_buffered_bytes.get(&subscription) {
                            let current = state.buffered_bytes.get(&subscription).copied().unwrap_or(0);
                            current.saturating_add(delivery_bytes) > *max
                        } else {
                            false
                        };
                        if over_budget {
                            state.pending_incoming.push_back((subscription.clone(), delivery));
                            state.metrics.record_backpressure();
                        } else if state.pending_incoming.is_empty() {
                            if let Some(buffer) = state.buffers.get_mut(&subscription) {
                                buffer.push_back(delivery);
                                state.scheduler.mark_ready(&subscription);
                            }
                            if let Some(bytes) = state.buffered_bytes.get_mut(&subscription) {
                                *bytes = bytes.saturating_add(delivery_bytes);
                            }
                            state.dispatch();
                        } else {
                            state.pending_incoming.push_back((subscription.clone(), delivery));
                            state.drain_pending();
                            state.dispatch();
                        }
                    }
                    Err(error) => {
                        state.record_source_error(ConsumerError::new(
                            ConsumerErrorKind::Transport,
                            error.to_string(),
                        ));
                        // Surface retained errors without waiting for an
                        // unrelated wake-up: a terminal error arriving after
                        // the embedder is already parked in `next()` must
                        // still reach it.
                        state.dispatch();
                    }
                },
                Some(ConsumerCommand::Settle {
                    token,
                    settlement,
                }) => {
                    let Some(channel_key) = state.channel_key_for(&token.subscription) else {
                        token.state.store(DeliveryState::Lost as u8, std::sync::atomic::Ordering::Release);
                        state
                            .record_settlement_error(settlement_error(
                                &token,
                                ConsumerErrorKind::InvalidSubscription,
                                "delivery references an unknown subscription",
                            ));
                        continue;
                    };
                    if token.settling.compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire).is_err() {
                        state
                            .record_settlement_error(settlement_error(
                                &token,
                                ConsumerErrorKind::AlreadySettling,
                                "delivery is already being settled",
                            ));
                        continue;
                    }
                    let params = SettleParams { token, settlement };
                    if state.settlement_in_flight.contains(&channel_key) {
                        state.settlement_queues.entry(channel_key).or_default().push_back(params);
                    } else {
                        launch_settlement(&mut state, channel_key, params);
                    }
                }
                Some(ConsumerCommand::SettleThrough { token }) => {
                    let Some(channel_key) = state.channel_key_for(&token.subscription) else {
                        token.state.store(DeliveryState::Lost as u8, std::sync::atomic::Ordering::Release);
                        state
                            .record_settlement_error(settlement_error(
                                &token,
                                ConsumerErrorKind::InvalidSubscription,
                                "delivery references an unknown subscription",
                            ));
                        continue;
                    };
                    if token.settling.compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire).is_err() {
                        state
                            .record_settlement_error(settlement_error(
                                &token,
                                ConsumerErrorKind::AlreadySettling,
                                "delivery is already being settled",
                            ));
                        continue;
                    }
                    let Some(ledger) = state.channel_ledgers.get(&channel_key) else {
                        token.settling.store(false, std::sync::atomic::Ordering::Release);
                        state
                            .record_settlement_error(settlement_error(
                                &token,
                                ConsumerErrorKind::Transport,
                                "channel ledger not found",
                            ));
                        continue;
                    };
                    match validate_contiguous_prefix(ledger, token.delivery_tag) {
                        Ok(affected_tokens) => {
                            for affected in &affected_tokens {
                                affected.settling.store(
                                    true,
                                    std::sync::atomic::Ordering::Release,
                                );
                            }
                            let params = SettleThroughParams {
                                token,
                                affected_tokens,
                            };
                            if state.settlement_in_flight.contains(&channel_key) {
                                state.settle_through_queues.entry(channel_key).or_default().push_back(params);
                            } else {
                                launch_settle_through(&mut state, channel_key, params);
                            }
                        }
                        Err(error) => {
                            token.settling.store(false, std::sync::atomic::Ordering::Release);
                            state
                                .record_settlement_error(settlement_error(
                                    &token,
                                    error.kind(),
                                    error.to_string(),
                                ));
                        }
                    }
                }
                None => return,
            },
            _ = close_rx.changed() => {
                // Close signal from `close()` or `Drop`: independent of the
                // command channel, so backpressure can never discard it. Only
                // `true` is ever sent (or the sender is dropped after
                // sending), and either way the set must close.
                close_set(&mut state).await;
                return;
            }
            () = dispatch_notify.notified() => {
                state.dispatch();
            }
            Some(settlement_result) = state.pending_settlements.next(),
                if !state.pending_settlements.is_empty() => {
                let channel_key = settlement_result.channel_key;
                state.settlement_in_flight.remove(&channel_key);

                let delivery_bytes = u64::try_from(settlement_result.token.payload.len()).unwrap_or(u64::MAX);
                let is_terminal = match &settlement_result.result {
                    Ok(_) => true,
                    Err(error) => matches!(
                        error.kind(),
                        ConsumerErrorKind::StaleGeneration
                            | ConsumerErrorKind::Transport
                            | ConsumerErrorKind::MaxAttempts
                    ),
                };

                if is_terminal {
                    if let Ok(terminal) = &settlement_result.result {
                        match terminal {
                            DeliveryState::Acked => {
                                state.metrics.record_ack(settlement_result.token.reserved_at.elapsed());
                            }
                            DeliveryState::Rejected => {
                                state.metrics.record_reject(settlement_result.token.reserved_at.elapsed());
                            }
                            DeliveryState::Pending | DeliveryState::Lost | DeliveryState::AutoAcked => {}
                        }
                    }
                    if let Some(bytes) = state.buffered_bytes.get_mut(&settlement_result.token.subscription) {
                        *bytes = bytes.saturating_sub(delivery_bytes);
                    }
                    state.try_drain_pending();
                    if let Some(ledger) = state.channel_ledgers.get_mut(&channel_key) {
                        ledger.pending.remove(&settlement_result.token.delivery_tag);
                    }
                }

                settlement_result.token.settling.store(false, std::sync::atomic::Ordering::Release);

                match &settlement_result.result {
                    Ok(terminal) => {
                        settlement_result
                            .token
                            .state
                            .store(*terminal as u8, std::sync::atomic::Ordering::Release);
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            ConsumerErrorKind::StaleGeneration | ConsumerErrorKind::Transport
                        ) =>
                    {
                        settlement_result
                            .token
                            .state
                            .store(DeliveryState::Lost as u8, std::sync::atomic::Ordering::Release);
                        state
                            .record_settlement_error(settlement_error(
                                &settlement_result.token,
                                error.kind(),
                                error.to_string(),
                            ));
                    }
                    // A poison delivery (attempts above the configured maximum
                    // surfaced by a capped delayed release) must never return
                    // to Pending: settle it terminally per the documented
                    // policy instead of hot-requeueing it.
                    Err(error) if error.kind() == ConsumerErrorKind::MaxAttempts => {
                        let subscription = settlement_result.token.subscription.clone();
                        let runtime = state
                            .subscriptions
                            .get(&subscription)
                            .map(|runtime| (Arc::clone(&runtime.channel), runtime.has_dead_letter));
                        if let Some((channel, has_dead_letter)) = runtime {
                            let delivery_tag = settlement_result.token.delivery_tag;
                            let terminal =
                                spawn_poison_settlement(channel, has_dead_letter, delivery_tag);
                            let settled_for = settlement_result.token.reserved_at.elapsed();
                            if terminal == DeliveryState::Acked {
                                state.metrics.record_ack(settled_for);
                            } else {
                                state.metrics.record_reject(settled_for);
                            }
                            settlement_result
                                .token
                                .state
                                .store(terminal as u8, std::sync::atomic::Ordering::Release);
                            state.record_settlement_error(SettlementError {
                                delivery_tag,
                                subscription,
                                kind: ConsumerErrorKind::MaxAttempts,
                                message: poison_settlement_message(
                                    &error.to_string(),
                                    &settlement_result.token.message_id,
                                    has_dead_letter,
                                ),
                            });
                        } else {
                            settlement_result
                                .token
                                .state
                                .store(DeliveryState::Lost as u8, std::sync::atomic::Ordering::Release);
                            state
                                .record_settlement_error(settlement_error(
                                    &settlement_result.token,
                                    ConsumerErrorKind::MaxAttempts,
                                    error.to_string(),
                                ));
                        }
                    }
                    Err(error) => {
                        settlement_result
                            .token
                            .state
                            .store(DeliveryState::Pending as u8, std::sync::atomic::Ordering::Release);
                        state
                            .record_settlement_error(settlement_error(
                                &settlement_result.token,
                                error.kind(),
                                error.to_string(),
                            ));
                    }
                }

                drain_settlement_queue(&mut state, channel_key);
            }
            Some(settle_through_result) = state.pending_settle_throughs.next(),
                if !state.pending_settle_throughs.is_empty() => {
                let channel_key = settle_through_result.channel_key;
                state.settlement_in_flight.remove(&channel_key);

                let target_tag = settle_through_result.target_tag;

                let is_terminal = match &settle_through_result.result {
                    Ok(_) => true,
                    Err(error) => matches!(
                        error.kind(),
                        ConsumerErrorKind::StaleGeneration | ConsumerErrorKind::Transport
                    ),
                };

                if is_terminal {
                    if let Ok(DeliveryState::Acked) = &settle_through_result.result {
                        for token in &settle_through_result.affected_tokens {
                            let bytes = u64::try_from(token.payload.len()).unwrap_or(u64::MAX);
                            if let Some(buf_bytes) = state.buffered_bytes.get_mut(&token.subscription) {
                                *buf_bytes = buf_bytes.saturating_sub(bytes);
                            }
                        }
                        state.metrics.record_ack(
                            settle_through_result
                                .affected_tokens
                                .last()
                                .unwrap()
                                .reserved_at
                                .elapsed(),
                        );
                    } else {
                        for token in &settle_through_result.affected_tokens {
                            let bytes = u64::try_from(token.payload.len()).unwrap_or(u64::MAX);
                            if let Some(buf_bytes) = state.buffered_bytes.get_mut(&token.subscription) {
                                *buf_bytes = buf_bytes.saturating_sub(bytes);
                            }
                        }
                    }
                    state.try_drain_pending();
                    if let Some(ledger) = state.channel_ledgers.get_mut(&channel_key) {
                        for tag in (ledger.acked_prefix + 1)..=target_tag {
                            ledger.pending.remove(&tag);
                        }
                        if matches!(settle_through_result.result, Ok(DeliveryState::Acked)) {
                            ledger.acked_prefix = target_tag;
                        }
                    }
                }

                for token in &settle_through_result.affected_tokens {
                    let final_state = match &settle_through_result.result {
                        Ok(state) => *state,
                        Err(error) if matches!(error.kind(), ConsumerErrorKind::StaleGeneration | ConsumerErrorKind::Transport) => {
                            DeliveryState::Lost
                        }
                        Err(_) => DeliveryState::Pending,
                    };
                    token.state.store(final_state as u8, std::sync::atomic::Ordering::Release);
                    token.settling.store(false, std::sync::atomic::Ordering::Release);
                }

                if let Err(error) = &settle_through_result.result {
                    state.record_settlement_error(SettlementError {
                        delivery_tag: settle_through_result.target_tag,
                        subscription: settle_through_result
                            .affected_tokens
                            .last()
                            .map_or_else(
                                || SubscriptionId::new("unknown"),
                                |t| t.subscription.clone(),
                            ),
                        kind: error.kind(),
                        message: error.to_string(),
                                            });
                }

                drain_settlement_queue(&mut state, channel_key);
            }
        }
    }
}

/// Builds a settlement error for a token whose asynchronous settlement failed.
fn settlement_error(
    token: &DeliveryTokenInner,
    kind: ConsumerErrorKind,
    message: impl Into<String>,
) -> SettlementError {
    SettlementError {
        delivery_tag: token.delivery_tag,
        subscription: token.subscription.clone(),
        kind,
        message: message.into(),
    }
}

/// Shuts the consumer set down: cancels pending settlements, closes every
/// subscription channel (bounded by a deadline so a stalled broker cannot
/// block close), and resolves any awaiting `close()` caller.
async fn close_set(state: &mut ActorState) {
    // Cancel pending settlements and clear queued work so close
    // cannot block on in-flight broker operations.
    state.pending_settlements = futures_util::stream::FuturesUnordered::new();
    state.pending_settle_throughs = futures_util::stream::FuturesUnordered::new();
    state.settlement_queues.clear();
    state.settle_through_queues.clear();
    state.settlement_in_flight.clear();
    for runtime in state.subscriptions.values() {
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(2), runtime.channel.close()).await;
    }
    if let Some(completed) = state
        .close_completion
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        let _ = completed.send(());
    }
}

#[allow(clippy::needless_pass_by_value)]
fn launch_settlement(state: &mut ActorState, channel_key: ChannelKey, params: SettleParams) {
    state.settlement_in_flight.insert(channel_key.clone());
    let Some(runtime) = state.subscriptions.get(&params.token.subscription) else {
        state.settlement_in_flight.remove(&channel_key);
        params.token.state.store(
            DeliveryState::Lost as u8,
            std::sync::atomic::Ordering::Release,
        );
        state.record_settlement_error(settlement_error(
            &params.token,
            ConsumerErrorKind::InvalidSubscription,
            "delivery references an unknown subscription",
        ));
        return;
    };
    let channel = runtime.channel.clone();
    let connection_key = runtime.connection_key;
    let generation = runtime.generation;
    let channel_id = runtime.channel_id;
    let publisher = runtime.publisher.clone();
    let destination = runtime.destination.clone();
    let delay_strategy = runtime.delay_strategy.clone();
    let delivery_tag = params.token.delivery_tag;
    let settlement = params.settlement;
    let token = params.token.clone();

    state.pending_settlements.push(Box::pin(async move {
        let result = execute_settlement(
            &channel,
            connection_key,
            generation,
            channel_id,
            delivery_tag,
            settlement,
            &token,
            publisher.as_ref(),
            destination.as_ref(),
            delay_strategy.as_ref(),
        )
        .await;
        SettlementResult {
            channel_key,
            token,
            result,
        }
    }));
}

#[allow(clippy::too_many_arguments)]
async fn execute_settlement(
    channel: &Arc<dyn crate::transport::ConsumerChannel>,
    connection_key: crate::pool::ConnectionKey,
    generation: u64,
    channel_id: u16,
    delivery_tag: u64,
    settlement: Settlement,
    token: &DeliveryTokenInner,
    publisher: Option<&crate::publisher::PublisherHandle>,
    destination: Option<&crate::publisher::Destination>,
    delay_strategy: Option<&DelayStrategy>,
) -> Result<DeliveryState, ConsumerError> {
    if connection_key != token.connection_key
        || generation != token.generation
        || channel_id != token.channel_id
    {
        return Err(ConsumerError::new(
            ConsumerErrorKind::StaleGeneration,
            "delivery belongs to a stale connection generation or channel",
        ));
    }

    match settlement {
        Settlement::Ack => {
            channel
                .ack(delivery_tag, false)
                .await
                .map_err(|e| transport_error(&e))?;
            Ok(DeliveryState::Acked)
        }
        Settlement::Release(delay) if delay.is_zero() => {
            channel
                .reject(delivery_tag, true)
                .await
                .map_err(|e| transport_error(&e))?;
            Ok(DeliveryState::Rejected)
        }
        Settlement::Release(delay) => {
            delayed_release(
                channel,
                delivery_tag,
                token,
                delay,
                publisher,
                destination,
                delay_strategy,
            )
            .await?;
            Ok(DeliveryState::Acked)
        }
        Settlement::Reject(requeue) => {
            channel
                .reject(delivery_tag, requeue)
                .await
                .map_err(|e| transport_error(&e))?;
            Ok(DeliveryState::Rejected)
        }
    }
}

async fn delayed_release(
    channel: &Arc<dyn crate::transport::ConsumerChannel>,
    delivery_tag: u64,
    token: &DeliveryTokenInner,
    delay: std::time::Duration,
    publisher: Option<&crate::publisher::PublisherHandle>,
    destination: Option<&crate::publisher::Destination>,
    delay_strategy: Option<&DelayStrategy>,
) -> Result<(), ConsumerError> {
    let publisher = publisher.ok_or_else(|| {
        ConsumerError::new(
            ConsumerErrorKind::MissingPublisher,
            "delayed release requires a publisher",
        )
    })?;
    let destination = destination.ok_or_else(|| {
        ConsumerError::new(
            ConsumerErrorKind::MissingPublisher,
            "delayed release requires a destination",
        )
    })?;
    let strategy = delay_strategy.ok_or_else(|| {
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
    properties.correlation_id = token.correlation_id.as_ref().map(|s| Arc::from(s.as_str()));
    properties.headers = AttemptsResolver::default()
        .delayed_headers(&token.headers, token.attempts)
        .map_err(|error| ConsumerError::new(ConsumerErrorKind::MaxAttempts, error.to_string()))?;
    if route.queue.is_none() {
        properties.delay_ms = Some(route.delay_ms);
    }
    let request = PublishRequest::new(
        Destination::new(route.exchange.as_ref(), route.routing_key.as_ref()),
        token.payload.clone(),
        properties,
        tokio::time::Instant::now() + publisher.confirm_timeout(),
    );
    let outcome = publisher
        .try_publish(request)
        .map_err(|e| publish_error(&e))?
        .wait()
        .await
        .map_err(|e| publish_error(&e))?;
    if !matches!(outcome, PublishOutcome::Confirmed { .. }) {
        return Err(ConsumerError::new(
            ConsumerErrorKind::Publish,
            "delayed release was not confirmed",
        ));
    }
    channel
        .ack(delivery_tag, false)
        .await
        .map_err(|e| transport_error(&e))
}

fn transport_error(error: &crate::transport::TransportError) -> ConsumerError {
    ConsumerError::new(ConsumerErrorKind::Transport, error.to_string())
}

/// Fires the terminal poison channel operation: `reject(requeue=false)` so the
/// broker dead-letters the delivery when a DLX is bound, otherwise an explicit
/// `ack` implementing the documented ack-and-log policy. Returns the terminal
/// delivery state the token must carry.
fn spawn_poison_settlement(
    channel: Arc<dyn crate::transport::ConsumerChannel>,
    has_dead_letter: bool,
    delivery_tag: u64,
) -> DeliveryState {
    if has_dead_letter {
        tokio::spawn(async move {
            let _ = channel.reject(delivery_tag, false).await;
        });
        DeliveryState::Rejected
    } else {
        tokio::spawn(async move {
            let _ = channel.ack(delivery_tag, false).await;
        });
        DeliveryState::Acked
    }
}

fn poison_settlement_message(
    detail: &str,
    message_id: &MessageId,
    has_dead_letter: bool,
) -> String {
    if has_dead_letter {
        format!(
            "{detail}; message {} rejected with requeue=false toward the dead-letter exchange",
            message_id.as_str()
        )
    } else {
        format!(
            "{detail}; message {} acknowledged and dropped after exceeding max attempts (no dead-letter exchange configured)",
            message_id.as_str()
        )
    }
}

fn publish_error(error: &crate::publisher::PublishError) -> ConsumerError {
    ConsumerError::new(ConsumerErrorKind::Publish, error.to_string())
}

fn validate_contiguous_prefix(
    ledger: &ChannelLedger,
    target_tag: u64,
) -> Result<Vec<Arc<DeliveryTokenInner>>, ConsumerError> {
    let mut tokens = Vec::new();
    let mut expected = ledger.acked_prefix + 1;
    for (&tag, entry) in ledger.pending.range(ledger.acked_prefix + 1..=target_tag) {
        if tag != expected {
            return Err(ConsumerError::new(
                ConsumerErrorKind::Transport,
                "non-contiguous delivery prefix — gap in delivery tags",
            ));
        }
        if entry.state != DeliveryState::Pending {
            return Err(ConsumerError::new(
                ConsumerErrorKind::AlreadySettled,
                "delivery in prefix is already terminal",
            ));
        }
        let Some(token) = &entry.token else {
            return Err(ConsumerError::new(
                ConsumerErrorKind::Transport,
                "delivery in prefix has no token — undelivered message in ledger",
            ));
        };
        tokens.push(token.clone());
        expected += 1;
    }
    if expected <= target_tag {
        return Err(ConsumerError::new(
            ConsumerErrorKind::Transport,
            "delivery tag not found in ledger",
        ));
    }
    Ok(tokens)
}

fn launch_settle_through(
    state: &mut ActorState,
    channel_key: ChannelKey,
    params: SettleThroughParams,
) {
    state.settlement_in_flight.insert(channel_key.clone());
    let Some(runtime) = state.subscriptions.get(&params.token.subscription) else {
        state.settlement_in_flight.remove(&channel_key);
        params.token.state.store(
            DeliveryState::Lost as u8,
            std::sync::atomic::Ordering::Release,
        );
        state.record_settlement_error(settlement_error(
            &params.token,
            ConsumerErrorKind::InvalidSubscription,
            "delivery references an unknown subscription",
        ));
        return;
    };
    let channel = runtime.channel.clone();
    let connection_key = runtime.connection_key;
    let generation = runtime.generation;
    let channel_id = runtime.channel_id;
    let target_tag = params.token.delivery_tag;
    let token = params.token.clone();
    let affected_tokens = params.affected_tokens;

    state.pending_settle_throughs.push(Box::pin(async move {
        let result = execute_settle_through(
            &channel,
            connection_key,
            generation,
            channel_id,
            target_tag,
            &token,
        )
        .await;
        SettleThroughResult {
            channel_key,
            target_tag,
            affected_tokens,
            result,
        }
    }));
}

async fn execute_settle_through(
    channel: &Arc<dyn crate::transport::ConsumerChannel>,
    connection_key: crate::pool::ConnectionKey,
    generation: u64,
    channel_id: u16,
    target_tag: u64,
    token: &DeliveryTokenInner,
) -> Result<DeliveryState, ConsumerError> {
    if connection_key != token.connection_key
        || generation != token.generation
        || channel_id != token.channel_id
    {
        return Err(ConsumerError::new(
            ConsumerErrorKind::StaleGeneration,
            "delivery belongs to a stale connection generation or channel",
        ));
    }

    channel
        .ack(target_tag, true)
        .await
        .map_err(|e| transport_error(&e))?;
    Ok(DeliveryState::Acked)
}

fn drain_settlement_queue(state: &mut ActorState, channel_key: ChannelKey) {
    // Check the regular settlement queue first, then the settle-through queue.
    if let Some(queue) = state.settlement_queues.get_mut(&channel_key)
        && let Some(next) = queue.pop_front()
    {
        launch_settlement(state, channel_key, next);
        return;
    }
    state.settlement_queues.remove(&channel_key);
    if let Some(queue) = state.settle_through_queues.get_mut(&channel_key)
        && let Some(next) = queue.pop_front()
    {
        launch_settle_through(state, channel_key, next);
        return;
    }
    state.settle_through_queues.remove(&channel_key);
}
