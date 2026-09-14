use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    num::NonZeroU32,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use futures_util::StreamExt;
use tokio::{
    sync::{mpsc, oneshot},
    time::MissedTickBehavior,
};

use super::{
    AttemptsResolver, ConsumerError, ConsumerErrorKind, Delivery, DeliveryState, MessageId,
    SubscriptionId, WeightedFairScheduler,
    delivery::{DeliveryIdentity, DeliveryToken, DeliveryTokenInner, Settlement, SettlementError},
    prefetch::{AdaptivePrefetch, PREFETCH_TICK, PrefetchStat},
    set::Subscription,
};
use crate::{
    config::PrefetchConfig,
    metrics::Metrics,
    publisher::{MessageProperties, PublishOutcome, PublishRequest, delay::DelayRouter},
    topology::delay::DelayStrategy,
    transport::{Delivery as TransportDelivery, TransportError, TransportResult},
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
}

/// Control-plane commands. These are never gated by backpressure: they are
/// what frees the byte budget and what the embedder uses to observe and stop
/// the consumer. They ride a dedicated channel so a saturated
/// `pending_incoming` cannot starve them (audit 2026-09-14 #1, same lesson as
/// the `close_rx` watch channel).
///
/// Liveness invariant: every control command accepted by the embedder is
/// eventually consumed, regardless of delivery backpressure.
pub(crate) enum ControlCommand {
    Settle {
        token: Arc<DeliveryTokenInner>,
        settlement: Settlement,
        /// Job latency measured embedder-side (pop stamp -> ack send): the
        /// adaptive controller's sample. Measured at send time because the
        /// actor-side record time is delayed by command queueing behind
        /// incoming floods, which grows with the prefetch window.
        job_latency: Duration,
    },
    SettleThrough {
        token: Arc<DeliveryTokenInner>,
        job_latency: Duration,
    },
    GetPrefetchStats {
        completed: oneshot::Sender<Vec<PrefetchStat>>,
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
    queue: String,
    prefetch: PrefetchConfig,
}

struct ActorState {
    subscriptions: HashMap<SubscriptionId, RuntimeSubscription>,
    adaptive_prefetch: HashMap<SubscriptionId, AdaptivePrefetch>,
    buffers: HashMap<SubscriptionId, VecDeque<TransportDelivery>>,
    buffered_bytes: HashMap<SubscriptionId, u64>,
    max_buffered_bytes: HashMap<SubscriptionId, u64>,
    channel_ledgers: HashMap<ChannelKey, ChannelLedger>,
    /// Delivery tags early-acked on the wire, per channel, in dispatch order.
    /// Bounded: a re-dispatch of an early-acked delivery only happens while
    /// the delivery is still held in actor memory (`self.buffers` re-push), so
    /// a ring sized to the in-flight bound (`pending_capacity`, total
    /// prefetch) is always sufficient. The real sufficiency proof is the
    /// shared `buffer_tx` coupling: a re-dispatchable tag only exists while
    /// the dispatch loop front is stuck on a failed handoff, which requires a
    /// full `buffer_tx`, and a full `buffer_tx` blocks every new same-channel
    /// ack from entering the ring. Guards against a second `basic_ack`
    /// for the same tag — `RabbitMQ` answers a duplicate ack with
    /// `PRECONDITION_FAILED` (406) and closes the channel (audit 2026-09-14 #3).
    /// Same lifecycle as `channel_ledgers`: entries live for the actor's
    /// lifetime; recovery spawns a fresh actor with fresh state.
    early_acked_tags: HashMap<ChannelKey, VecDeque<u64>>,
    early_acked_capacity: usize,
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
    /// Plain acks recorded but not yet flushed to the wire, per channel.
    /// Bounded by the ledger, which is bounded by the in-flight budget and
    /// prefetch. Drained by `flush_acked` whenever the dispatch stock is
    /// drained (and unconditionally at close).
    acked_batch: HashMap<ChannelKey, std::collections::BTreeMap<u64, Arc<DeliveryTokenInner>>>,
    /// Upper bound on source-error items currently sitting in the hand-off
    /// flume. Source errors never produce acknowledgements, so they must not
    /// hold the stock-aware flush back: the flush gate treats the flume as
    /// drained once `len` drops to this count. Over-permissive only after an
    /// embedder actually consumes an error item (rare; worst case is a
    /// slightly earlier flush, never a lost or duplicated ack).
    flume_error_items: usize,
    source_errors: VecDeque<ConsumerError>,
    scheduler: WeightedFairScheduler,
    control_tx: mpsc::Sender<ControlCommand>,
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
        control_tx: mpsc::Sender<ControlCommand>,
        buffer_tx: flume::Sender<Result<Delivery, ConsumerError>>,
        error_tx: flume::Sender<SettlementError>,
        error_rx: flume::Receiver<SettlementError>,
        close_completion: Arc<std::sync::Mutex<Option<oneshot::Sender<()>>>>,
        metrics: Metrics,
        pending_capacity: usize,
    ) -> Self {
        let mut scheduler = WeightedFairScheduler::default();
        let mut runtime = HashMap::new();
        let mut adaptive_prefetch = HashMap::new();
        let mut buffers = HashMap::new();
        let mut buffered_bytes = HashMap::new();
        let mut max_buffered_bytes = HashMap::new();
        let mut channel_ledgers = HashMap::new();
        let total_prefetch: usize = subscriptions
            .iter()
            .map(|subscription| usize::from(subscription.prefetch.ceiling()))
            .sum();
        let early_acked_capacity = pending_capacity.max(total_prefetch);
        for subscription in subscriptions {
            scheduler.register(subscription.id.clone(), subscription.policy);
            buffers.insert(subscription.id.clone(), VecDeque::new());
            buffered_bytes.insert(subscription.id.clone(), 0);
            max_buffered_bytes.insert(subscription.id.clone(), subscription.max_buffered_bytes);
            if let PrefetchConfig::Adaptive {
                initial,
                min,
                max,
                target_buffer,
            } = subscription.prefetch
            {
                adaptive_prefetch.insert(
                    subscription.id.clone(),
                    AdaptivePrefetch::new(min, max, initial, target_buffer),
                );
            }
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
                    queue: subscription.queue,
                    prefetch: subscription.prefetch,
                },
            );
        }

        Self {
            subscriptions: runtime,
            adaptive_prefetch,
            buffers,
            buffered_bytes,
            max_buffered_bytes,
            channel_ledgers,
            early_acked_tags: HashMap::new(),
            early_acked_capacity,
            pending_incoming: VecDeque::new(),
            pending_capacity,
            pending_settlements: futures_util::stream::FuturesUnordered::new(),
            pending_settle_throughs: futures_util::stream::FuturesUnordered::new(),
            settlement_in_flight: HashSet::new(),
            settlement_queues: HashMap::new(),
            settle_through_queues: HashMap::new(),
            acked_batch: HashMap::new(),
            flume_error_items: 0,
            source_errors: VecDeque::new(),
            scheduler,
            control_tx,
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

    /// Whether `tag` was already early-acked on the wire for this channel.
    /// Linear scan over a bounded ring.
    fn early_acked(&self, tag: u64, key: &ChannelKey) -> bool {
        self.early_acked_tags
            .get(key)
            .is_some_and(|ring| ring.contains(&tag))
    }

    /// Records `tag` as early-acked on the wire. Bounded ring: the oldest
    /// entry is dropped at capacity — safe because a re-dispatch of an
    /// early-acked delivery only happens while the delivery is still held in
    /// actor memory, so the ring never needs to outlive its oldest entry.
    fn remember_early_acked(&mut self, tag: u64, key: ChannelKey) {
        let ring = self.early_acked_tags.entry(key).or_default();
        if ring.len() >= self.early_acked_capacity {
            ring.pop_front();
        }
        ring.push_back(tag);
    }

    fn has_adaptive_prefetch(&self) -> bool {
        !self.adaptive_prefetch.is_empty()
    }

    /// Advances every adaptive controller and returns the `QoS` changes to apply.
    fn collect_prefetch_updates(
        &mut self,
    ) -> Vec<(
        SubscriptionId,
        Arc<dyn crate::transport::ConsumerChannel>,
        u16,
    )> {
        let mut updates = Vec::new();
        for (id, controller) in &mut self.adaptive_prefetch {
            if let Some(value) = controller.tick()
                && let Some(runtime) = self.subscriptions.get(id)
            {
                updates.push((id.clone(), Arc::clone(&runtime.channel), value));
            }
        }
        updates
    }

    /// Snapshot of per-subscription prefetch state (mode, applied value, EWMA).
    fn prefetch_stats(&self) -> Vec<PrefetchStat> {
        let mut stats = Vec::with_capacity(self.subscriptions.len());
        for (id, runtime) in &self.subscriptions {
            let (mode, mut current) = match runtime.prefetch {
                PrefetchConfig::Fixed(value) => ("fixed", value),
                PrefetchConfig::Adaptive { initial, .. } => ("adaptive", initial),
            };
            let mut ewma = Duration::ZERO;
            if let Some(controller) = self.adaptive_prefetch.get(id) {
                current = controller.current();
                ewma = controller.ewma();
            }
            stats.push(PrefetchStat {
                subscription: id.as_str().to_owned(),
                queue: runtime.queue.clone(),
                mode,
                current,
                ewma,
            });
        }
        stats.sort_by(|left, right| left.subscription.cmp(&right.subscription));
        stats
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch(&mut self) {
        self.drain_pending();
        loop {
            if let Some(error) = self.source_errors.front() {
                let err_sent = self.buffer_tx.try_send(Err(error.clone()));
                if err_sent.is_ok() {
                    self.flume_error_items = self.flume_error_items.saturating_add(1);
                }
                if err_sent.is_err() {
                    break;
                }
                self.source_errors.pop_front();
                continue;
            }
            let Some(subscription) = self.scheduler.pick() else {
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
                        Some((connection_key, generation, channel_id)),
                    );
                    continue;
                }
            };
            let headers = Arc::clone(&delivery.headers);

            if early_ack {
                let delivery_bytes = u64::try_from(delivery.payload.len()).unwrap_or(u64::MAX);
                if !no_ack {
                    // At most one wire ack per tag across re-dispatches: the
                    // re-pushed delivery must not be acked again (channel 406,
                    // audit 2026-09-14 #3).
                    let tag = delivery.delivery_tag;
                    let channel_key = (subscription.clone(), channel_id, generation);
                    if !self.early_acked(tag, &channel_key) {
                        self.remember_early_acked(tag, channel_key);
                        tokio::spawn(async move {
                            let _ = channel.ack(tag, false).await;
                        });
                    }
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
                let noack_sent = self.buffer_tx.try_send(Ok(item));
                if noack_sent.is_err() {
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
                if attempts > 1 {
                    self.metrics.record_duplicate();
                }
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
                self.control_tx.clone(),
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
            let sent = self.buffer_tx.try_send(Ok(item));
            if sent.is_err() {
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
            if attempts > 1 {
                self.metrics.record_duplicate();
            }
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
    ///
    /// The channel operation is fenced: it only fires when the subscription
    /// runtime still matches the generation identity the delivery arrived
    /// under (`Some`), passed by the caller from where the delivery was
    /// captured. After a reconnection the same numeric tag on the live
    /// channel belongs to a different delivery, so a stale settlement is
    /// skipped and the broker redelivers (audit 2026-09-14 #5). A delivery
    /// with no runtime identity (`None`) settles like an unknown
    /// subscription: no wire operation, typed error still recorded.
    ///
    /// In `no_ack` mode the broker auto-acks at delivery, so the wire op is
    /// skipped as well: any ack or reject for the tag would hit an unknown
    /// delivery tag (`PRECONDITION_FAILED` 406), close the channel, and
    /// redeliver the same message in a deterministic churn loop (audit
    /// 2026-09-14 #6). The typed error and metrics still record the terminal
    /// outcome.
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
        token_identity: Option<(crate::pool::ConnectionKey, u64, u16)>,
    ) {
        let channel = self
            .subscriptions
            .get(subscription)
            .filter(|runtime| {
                !runtime.no_ack
                    && token_identity.is_some_and(|(connection_key, generation, channel_id)| {
                        runtime.connection_key == connection_key
                            && runtime.generation == generation
                            && runtime.channel_id == channel_id
                    })
            })
            .map(|runtime| Arc::clone(&runtime.channel));
        if let Some(channel) = channel {
            spawn_poison_settlement(channel, has_dead_letter, delivery_tag);
        }
        // A stale generation or unknown subscription has nothing to settle
        // against the broker; the recorded error below still surfaces the
        // poison condition and the broker redelivers.
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

    /// Settles a delivery whose size alone exceeds the subscription's byte
    /// budget. Reuses the poison contract: `reject(requeue=false)` toward the
    /// dead-letter exchange when one is configured, otherwise an explicit
    /// acknowledge recorded as a typed settlement error (`MaxAttempts` kind,
    /// the oversized detail as discriminator). The delivery is parked in
    /// `pending_incoming` without being counted in `buffered_bytes`, so it is
    /// counted here before `settle_poison` releases it — keeping
    /// `buffered_bytes` equal to the sum of held sizes.
    fn settle_oversized(&mut self, subscription: &SubscriptionId, delivery: &TransportDelivery) {
        let payload_bytes = u64::try_from(delivery.payload.len()).unwrap_or(u64::MAX);
        // Count-then-release: `settle_poison` subtracts `payload_bytes` below,
        // and the delivery was not yet counted in `buffered_bytes`.
        if let Some(bytes) = self.buffered_bytes.get_mut(subscription) {
            *bytes = bytes.saturating_add(payload_bytes);
        }
        let runtime = self.subscriptions.get(subscription);
        let has_dead_letter = runtime.is_some_and(|runtime| runtime.has_dead_letter);
        let message_id = delivery.message_id.clone().map_or_else(
            || MessageId::new(delivery.delivery_tag.to_string()),
            MessageId::new,
        );
        // The oversized delivery arrived through this runtime's pumps, so its
        // identity is the runtime's own; `settle_poison` re-verifies it
        // against the live runtime before any wire operation.
        let token_identity = runtime.map(|runtime| {
            (
                runtime.connection_key,
                runtime.generation,
                runtime.channel_id,
            )
        });
        self.settle_poison(
            subscription,
            has_dead_letter,
            delivery.delivery_tag,
            &message_id,
            payload_bytes,
            "message size exceeds max_buffered_bytes",
            Duration::ZERO,
            token_identity,
        );
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
            let max = self.max_buffered_bytes.get(subscription).copied();
            let over_budget = max.is_some_and(|max| {
                let current = self.buffered_bytes.get(subscription).copied().unwrap_or(0);
                current.saturating_add(delivery_bytes) > max
            });
            if over_budget {
                if delivery_bytes > max.unwrap_or(u64::MAX) {
                    // Defensive: an oversized delivery must already have been
                    // settled at arrival in `handle_incoming`. If it ever
                    // reaches the head of the deque anyway, settle it
                    // terminally instead of blocking the pipeline forever.
                    let (subscription, delivery) = self
                        .pending_incoming
                        .pop_front()
                        .expect("front checked above");
                    self.settle_oversized(&subscription, &delivery);
                    continue;
                }
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

    /// True when nothing dispatchable remains: no backpressured incoming, no
    /// buffered deliveries, and the hand-off flume holds only source-error
    /// items (which never produce acknowledgements). The embedder has (or is
    /// about to run out of) work, so holding recorded acks back no longer
    /// buys coalescing — flushing now frees broker credit exactly when the
    /// next arrivals need it, and lets the acks recorded while the stock was
    /// stocked land as one cumulative wire ack.
    fn dispatch_stock_drained(&self) -> bool {
        if !self.pending_incoming.is_empty() {
            return false;
        }
        if self.buffers.values().any(|buffer| !buffer.is_empty()) {
            return false;
        }
        self.buffer_tx.len() <= self.flume_error_items
    }
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_actor(
    subscriptions: Vec<Subscription>,
    mut incoming_rx: mpsc::Receiver<ConsumerCommand>,
    mut control_rx: mpsc::Receiver<ControlCommand>,
    control_tx: mpsc::Sender<ControlCommand>,
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
        control_tx,
        buffer_tx,
        error_tx,
        error_rx,
        close_completion,
        metrics,
        pending_capacity,
    );
    // Allow pumps to push deliveries before the first dispatch.
    tokio::task::yield_now().await;
    // Conditional tick arm: no active interval when no subscription is
    // adaptive, so fixed-only sets keep their previous behavior exactly.
    let has_adaptive = state.has_adaptive_prefetch();
    let mut prefetch_interval = tokio::time::interval(PREFETCH_TICK);
    prefetch_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // The actor holds a control sender (cloned into delivery tokens), so the
    // control channel cannot close while it runs; the flag only guards the
    // defensive `None` path so a closed arm can never spin under `biased`.
    // Same guard for the incoming channel: its senders (pumps) can all end
    // while the set is still open, and the actor must keep serving control
    // commands and the close signal instead of exiting past `close_set`.
    let mut control_open = true;
    let mut incoming_open = true;
    loop {
        // Drain every ready command before flushing acks: settlements
        // recorded in one burst must coalesce into one wire ack, not leak
        // out one per select pass. Control commands drain ungated — they
        // free the byte budget and must progress even when `pending_incoming`
        // is saturated (audit 2026-09-14 #1); the incoming drain keeps the
        // same backpressure gate as its select arm.
        loop {
            match control_rx.try_recv() {
                Ok(ControlCommand::Settle {
                    token,
                    settlement,
                    job_latency,
                }) => {
                    handle_settle(&mut state, token, settlement, job_latency);
                }
                Ok(ControlCommand::SettleThrough { token, job_latency }) => {
                    handle_settle_through(&mut state, token, job_latency);
                }
                Ok(ControlCommand::GetPrefetchStats { completed }) => {
                    let _ = completed.send(state.prefetch_stats());
                }
                Err(_) => break,
            }
        }
        while state.pending_incoming.len() < state.pending_capacity {
            match incoming_rx.try_recv() {
                Ok(ConsumerCommand::Incoming {
                    subscription,
                    result,
                }) => {
                    handle_incoming(&mut state, subscription, result);
                }
                Err(_) => break,
            }
        }
        // Flush recorded acks only when the dispatch stock is drained: while
        // the embedder still has stocked deliveries, later acks can coalesce
        // into the same cumulative wire ack, so freeing broker credit now
        // would split the batch and keep the credit pipeline one-ack-per-RTT.
        if state.dispatch_stock_drained() {
            flush_acked(&mut state);
        }
        tokio::select! {
            biased;
            command = control_rx.recv(), if control_open => match command {
                Some(ControlCommand::Settle {
                    token,
                    settlement,
                    job_latency,
                }) => handle_settle(&mut state, token, settlement, job_latency),
                Some(ControlCommand::SettleThrough { token, job_latency }) => {
                    handle_settle_through(&mut state, token, job_latency);
                }
                Some(ControlCommand::GetPrefetchStats { completed }) => {
                    let _ = completed.send(state.prefetch_stats());
                }
                None => control_open = false,
            },
            command = incoming_rx.recv(),
                if incoming_open
                    && state.pending_incoming.len() < state.pending_capacity =>
            match command {
                Some(ConsumerCommand::Incoming {
                    subscription,
                    result,
                }) => handle_incoming(&mut state, subscription, result),
                None => incoming_open = false,
            },
            _ = close_rx.changed() => {
                // Close signal from `close()` or `Drop`: independent of the
                // command channel, so backpressure can never discard it. Only
                // `true` is ever sent (or the sender is dropped after
                // sending), and either way the set must close. Teardown runs
                // after the select loop so the command receiver is free for
                // the settlement drain.
                break;
            }
            () = dispatch_notify.notified() => {
                state.dispatch();
            }
            _ = prefetch_interval.tick(), if has_adaptive => {
                // The tick itself is pure; the network round trip of `set_qos`
                // runs in a detached task so it never blocks dispatch and
                // settlements during the RTT. Failures surface through the
                // bounded error channel; the actor keeps going.
                for (subscription, channel, value) in state.collect_prefetch_updates() {
                    let error_tx = state.error_tx.clone();
                    tokio::spawn(async move {
                        if let Err(error) = channel.set_qos(value).await {
                            let _ = error_tx.send(SettlementError {
                                delivery_tag: 0,
                                subscription,
                                kind: ConsumerErrorKind::Transport,
                                message: format!(
                                    "adaptive prefetch set_qos({value}) failed: {error}"
                                ),
                            });
                        }
                    });
                }
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
                            | ConsumerErrorKind::InvalidDelay
                    ),
                };

                if is_terminal {
                    if let Ok(terminal) = &settlement_result.result {
                        match terminal {
                            DeliveryState::Acked => {
                                state
                                    .metrics
                                    .record_ack(settlement_result.token.reserved_at.elapsed());
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
                    // surfaced by a capped delayed release, or a release delay
                    // no compiled strategy can honor) must never return to
                    // Pending: settle it terminally per the documented policy
                    // instead of hot-requeueing it.
                    Err(error)
                        if matches!(
                            error.kind(),
                            ConsumerErrorKind::MaxAttempts | ConsumerErrorKind::InvalidDelay
                        ) =>
                    {
                        let subscription = settlement_result.token.subscription.clone();
                        let runtime = state.subscriptions.get(&subscription).map(|runtime| {
                            (
                                Arc::clone(&runtime.channel),
                                runtime.has_dead_letter,
                                runtime.connection_key,
                                runtime.generation,
                                runtime.channel_id,
                            )
                        });
                        if let Some(
                            (channel, has_dead_letter, connection_key, generation, channel_id),
                        ) = runtime
                        {
                            let token = &settlement_result.token;
                            let delivery_tag = token.delivery_tag;
                            // Generation fencing (audit 2026-09-14 #5): a token
                            // from a superseded generation must never ack or
                            // reject on the live channel — the same numeric tag
                            // there belongs to a different delivery. Skip the
                            // wire op; the broker redelivers and the typed
                            // error below still surfaces the poison condition.
                            let terminal = if connection_key == token.connection_key
                                && generation == token.generation
                                && channel_id == token.channel_id
                            {
                                spawn_poison_settlement(channel, has_dead_letter, delivery_tag)
                            } else {
                                DeliveryState::Lost
                            };
                            let settled_for = token.reserved_at.elapsed();
                            if terminal == DeliveryState::Lost {
                                state.record_poison_metrics(has_dead_letter, settled_for);
                            } else if terminal == DeliveryState::Acked {
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
                                kind: error.kind(),
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
                                    error.kind(),
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

    close_set(&mut state, &mut control_rx).await;
}

/// Handles one incoming delivery command: ledger claim, byte-budget
/// backpressure, buffering and dispatch. Shared by the live select arm and
/// the loop-top command drain.
fn handle_incoming(
    state: &mut ActorState,
    subscription: SubscriptionId,
    result: Result<TransportDelivery, TransportError>,
) {
    match result {
        Ok(delivery) => {
            let delivery_bytes = u64::try_from(delivery.payload.len()).unwrap_or(u64::MAX);
            if let Some(channel_key) = state.channel_key_for(&subscription) {
                state
                    .channel_ledgers
                    .entry(channel_key)
                    .or_default()
                    .pending
                    .insert(
                        delivery.delivery_tag,
                        ChannelLedgerEntry {
                            state: DeliveryState::Pending,
                            token: None,
                        },
                    );
            }
            let max = state.max_buffered_bytes.get(&subscription).copied();
            let over_budget = max.is_some_and(|max| {
                let current = state
                    .buffered_bytes
                    .get(&subscription)
                    .copied()
                    .unwrap_or(0);
                current.saturating_add(delivery_bytes) > max
            });
            if over_budget {
                if delivery_bytes > max.unwrap_or(u64::MAX) {
                    // A delivery whose size alone exceeds the budget can never
                    // satisfy the capacity predicate: settle it terminally via
                    // the poison contract instead of parking it forever
                    // (audit 2026-09-14 #2).
                    state.settle_oversized(&subscription, &delivery);
                    return;
                }
                state.pending_incoming.push_back((subscription, delivery));
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
                state.pending_incoming.push_back((subscription, delivery));
                state.drain_pending();
                state.dispatch();
            }
        }
        Err(error) => {
            state.record_source_error(ConsumerError::new(
                ConsumerErrorKind::Transport,
                error.to_string(),
            ));
            // Surface retained errors without waiting for an unrelated
            // wake-up: a terminal error arriving after the embedder is
            // already parked in `next()` must still reach it.
            state.dispatch();
        }
    }
}

fn handle_settle(
    state: &mut ActorState,
    token: Arc<DeliveryTokenInner>,
    settlement: Settlement,
    job_latency: Duration,
) {
    let Some(channel_key) = claim_settlement(state, &token) else {
        return;
    };
    if matches!(settlement, Settlement::Ack) {
        // Plain acks coalesce: record only — `flush_acked` bursts the
        // contiguous prefix into one cumulative wire ack.
        //
        // The adaptive controller learns the embedder-side job latency
        // measured at ack-send time, not at settlement completion: the
        // completion latency folds in the coalescing delay and the command
        // queueing behind incoming floods, both of which grow with the
        // prefetch window and would make the controller shrink the very
        // window the coalescing feeds on.
        if let Some(controller) = state.adaptive_prefetch.get_mut(&token.subscription) {
            controller.observe(job_latency);
        }
        state
            .acked_batch
            .entry(channel_key)
            .or_default()
            .insert(token.delivery_tag, token);
        return;
    }
    let params = SettleParams { token, settlement };
    if state.settlement_in_flight.contains(&channel_key) {
        state
            .settlement_queues
            .entry(channel_key)
            .or_default()
            .push_back(params);
    } else {
        launch_settlement(state, channel_key, params);
    }
}

/// Enqueues a contiguous-prefix multi-ack. Shared by the live command loop
/// and the close-time sweep.
fn handle_settle_through(
    state: &mut ActorState,
    token: Arc<DeliveryTokenInner>,
    job_latency: Duration,
) {
    let Some(channel_key) = claim_settlement(state, &token) else {
        return;
    };
    let Some(ledger) = state.channel_ledgers.get(&channel_key) else {
        token
            .settling
            .store(false, std::sync::atomic::Ordering::Release);
        state.record_settlement_error(settlement_error(
            &token,
            ConsumerErrorKind::Transport,
            "channel ledger not found",
        ));
        return;
    };
    match validate_contiguous_prefix(ledger, token.delivery_tag) {
        Ok(affected_tokens) => {
            for affected in &affected_tokens {
                affected
                    .settling
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            // Embedder-side job latency sampling for the adaptive
            // controller: see the plain-ack branch in `handle_settle`.
            if let Some(controller) = state.adaptive_prefetch.get_mut(&token.subscription) {
                controller.observe(job_latency);
            }
            let params = SettleThroughParams {
                token,
                affected_tokens,
            };
            if state.settlement_in_flight.contains(&channel_key) {
                state
                    .settle_through_queues
                    .entry(channel_key)
                    .or_default()
                    .push_back(params);
            } else {
                launch_settle_through(state, channel_key, params);
            }
        }
        Err(error) => {
            token
                .settling
                .store(false, std::sync::atomic::Ordering::Release);
            state.record_settlement_error(settlement_error(
                &token,
                error.kind(),
                error.to_string(),
            ));
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

/// Flushes recorded plain acks to the wire, called at the top of every actor
/// pass after the command drain. The contiguous run of acked delivery tags
/// above the settled watermark lands as one cumulative
/// `ack(watermark, multiple=true)` through the existing settle-through
/// machinery; acks beyond a hole flush individually so a stalled tag never
/// delays downstream acknowledgements on the wire. Channels with a
/// settlement in flight keep their batch for the next pass. Tokens absent
/// from the ledger are already terminal (double acks) and drop without a
/// wire op.
fn flush_acked(state: &mut ActorState) {
    if state.acked_batch.is_empty() {
        return;
    }
    let channels: Vec<ChannelKey> = state.acked_batch.keys().cloned().collect();
    for channel_key in channels {
        if state.settlement_in_flight.contains(&channel_key) {
            continue;
        }
        let Some(batch) = state.acked_batch.get(&channel_key) else {
            continue;
        };
        let Some(ledger) = state.channel_ledgers.get(&channel_key) else {
            // The ledger dies with the channel generation; the broker
            // redelivers what was never acknowledged on the wire.
            state.acked_batch.remove(&channel_key);
            continue;
        };

        // Walk the contiguous run of recorded acks above the settled
        // watermark. The first unacked (or terminal) tag ends the run —
        // that hole does not invalidate the run before it.
        let mut prefix_tokens: Vec<Arc<DeliveryTokenInner>> = Vec::new();
        for (&tag, entry) in ledger.pending.range(ledger.acked_prefix + 1..) {
            match batch.get(&tag) {
                Some(token) if entry.state == DeliveryState::Pending => {
                    prefix_tokens.push(token.clone());
                }
                _ => break,
            }
        }
        let watermark = prefix_tokens.last().map(|token| token.delivery_tag);

        // Stragglers: acked tags beyond the hole (or the whole batch when
        // the run is empty). Already-terminal tags drop without a wire op.
        let scan_from = watermark.map_or(ledger.acked_prefix + 1, |w| w + 1);
        let mut stragglers: Vec<SettleParams> = Vec::new();
        for (tag, token) in batch.range(scan_from..) {
            if let Some(entry) = ledger.pending.get(tag)
                && entry.state == DeliveryState::Pending
            {
                stragglers.push(SettleParams {
                    token: token.clone(),
                    settlement: Settlement::Ack,
                });
            }
        }
        state.acked_batch.remove(&channel_key);
        if let Some(target_token) = prefix_tokens.pop() {
            prefix_tokens.push(target_token.clone());
            launch_settle_through(
                state,
                channel_key.clone(),
                SettleThroughParams {
                    token: target_token,
                    affected_tokens: prefix_tokens,
                },
            );
            // In flight now: stragglers queue behind the burst.
            for params in stragglers {
                state
                    .settlement_queues
                    .entry(channel_key.clone())
                    .or_default()
                    .push_back(params);
            }
        } else {
            for params in stragglers {
                if state.settlement_in_flight.contains(&channel_key) {
                    state
                        .settlement_queues
                        .entry(channel_key.clone())
                        .or_default()
                        .push_back(params);
                } else {
                    launch_settlement(state, channel_key.clone(), params);
                }
            }
        }
    }
}

/// Bounded budget for the close-time settlement flush (issue #233). Mirrors
/// the publish side's teardown flush budget: long enough to land queued
/// settlements on a healthy broker, short enough that a stalled transport
/// cannot hold a process exit hostage. Settlements still unacknowledged when
/// the budget expires are abandoned to the broker's redelivery — the
/// delivery contract stays at-least-once.
const CLOSE_SETTLEMENT_DRAIN_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// Shuts the consumer set down: flushes pending and queued settlements to
/// the transport within a bounded budget (a consumer that pops, acks, and
/// exits must not silently drop its acknowledgements), closes every
/// subscription channel (bounded by a deadline so a stalled broker cannot
/// block close), and resolves any awaiting `close()` caller.
async fn close_set(state: &mut ActorState, control_rx: &mut mpsc::Receiver<ControlCommand>) {
    // Sweep the control channel first: a settlement enqueued right before
    // close raced the actor's command loop and must not die with it.
    // Incoming deliveries are left for the broker to redeliver
    // (at-least-once); stats callers observe a closed error.
    while let Ok(command) = control_rx.try_recv() {
        match command {
            ControlCommand::Settle {
                token,
                settlement,
                job_latency,
            } => {
                handle_settle(state, token, settlement, job_latency);
            }
            ControlCommand::SettleThrough { token, job_latency } => {
                handle_settle_through(state, token, job_latency);
            }
            ControlCommand::GetPrefetchStats { .. } => {}
        }
    }
    // Recorded acks join the bounded drain like any queued settlement.
    flush_acked(state);

    // Drive in-flight and queued settlements to the transport within the
    // bounded budget — same sequencing as the actor's completion arms,
    // minus bookkeeping that only matters to a consumer that stays open
    // (metrics, prefetch observation, buffer accounting).
    let deadline = tokio::time::Instant::now() + CLOSE_SETTLEMENT_DRAIN_BUDGET;
    loop {
        let drain_settlements = !state.pending_settlements.is_empty();
        let drain_throughs = !state.pending_settle_throughs.is_empty();
        if !drain_settlements && !drain_throughs {
            break;
        }
        tokio::select! {
            result = state.pending_settlements.next(), if drain_settlements => {
                if let Some(result) = result {
                    state.settlement_in_flight.remove(&result.channel_key);
                    match &result.result {
                        Ok(terminal) => result.token.state.store(
                            *terminal as u8,
                            std::sync::atomic::Ordering::Release,
                        ),
                        Err(_) => result.token.state.store(
                            DeliveryState::Lost as u8,
                            std::sync::atomic::Ordering::Release,
                        ),
                    }
                    result.token.settling.store(false, std::sync::atomic::Ordering::Release);
                    if let Err(error) = &result.result {
                        state.record_settlement_error(settlement_error(
                            &result.token,
                            error.kind(),
                            error.to_string(),
                        ));
                    }
                    drain_settlement_queue(state, result.channel_key);
                }
            }
            result = state.pending_settle_throughs.next(), if drain_throughs => {
                if let Some(result) = result {
                    state.settlement_in_flight.remove(&result.channel_key);
                    let final_state = match &result.result {
                        Ok(terminal) => *terminal,
                        Err(_) => DeliveryState::Lost,
                    };
                    for token in &result.affected_tokens {
                        token.state.store(
                            final_state as u8,
                            std::sync::atomic::Ordering::Release,
                        );
                        token.settling.store(false, std::sync::atomic::Ordering::Release);
                    }
                    if let Err(error) = &result.result {
                        state.record_settlement_error(SettlementError {
                            delivery_tag: result.target_tag,
                            subscription: result.affected_tokens.last().map_or_else(
                                || SubscriptionId::new("unknown"),
                                |t| t.subscription.clone(),
                            ),
                            kind: error.kind(),
                            message: error.to_string(),
                        });
                    }
                    drain_settlement_queue(state, result.channel_key);
                }
            }
            () = tokio::time::sleep_until(deadline) => break,
        }
    }

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
/// Common settlement prologue: validates the token's subscription and flips
/// its `settling` flag. On failure the token is marked `Lost` (unknown
/// subscription) or left untouched (already settling), the error is recorded,
/// and `None` is returned so the caller proceeds to the next command.
fn claim_settlement(state: &mut ActorState, token: &Arc<DeliveryTokenInner>) -> Option<ChannelKey> {
    let Some(channel_key) = state.channel_key_for(&token.subscription) else {
        token.state.store(
            DeliveryState::Lost as u8,
            std::sync::atomic::Ordering::Release,
        );
        state.record_settlement_error(settlement_error(
            token,
            ConsumerErrorKind::InvalidSubscription,
            "delivery references an unknown subscription",
        ));
        return None;
    };
    if token
        .settling
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        state.record_settlement_error(settlement_error(
            token,
            ConsumerErrorKind::AlreadySettling,
            "delivery is already being settled",
        ));
        return None;
    }
    Some(channel_key)
}

/// Subscription-runtime context cloned out for a launch (the borrow must not
/// escape while the caller keeps mutating `ActorState`).
struct SettlementLaunch {
    channel: Arc<dyn crate::transport::ConsumerChannel>,
    connection_key: crate::pool::ConnectionKey,
    generation: u64,
    channel_id: u16,
    publisher: Option<crate::publisher::PublisherHandle>,
    destination: Option<crate::publisher::Destination>,
    delay_strategy: Option<DelayStrategy>,
}

/// Launch staging shared by `launch_settlement` and `launch_settle_through`:
/// marks the channel as having a settlement in flight, then looks up the
/// subscription runtime — rolling the marker back when the subscription is
/// already gone (token marked `Lost`, error recorded, `None` returned).
fn stage_settlement_launch(
    state: &mut ActorState,
    channel_key: &ChannelKey,
    token: &Arc<DeliveryTokenInner>,
) -> Option<SettlementLaunch> {
    state.settlement_in_flight.insert(channel_key.clone());
    let Some(runtime) = state.subscriptions.get(&token.subscription) else {
        state.settlement_in_flight.remove(channel_key);
        token.state.store(
            DeliveryState::Lost as u8,
            std::sync::atomic::Ordering::Release,
        );
        state.record_settlement_error(settlement_error(
            token,
            ConsumerErrorKind::InvalidSubscription,
            "delivery references an unknown subscription",
        ));
        return None;
    };
    Some(SettlementLaunch {
        channel: runtime.channel.clone(),
        connection_key: runtime.connection_key,
        generation: runtime.generation,
        channel_id: runtime.channel_id,
        publisher: runtime.publisher.clone(),
        destination: runtime.destination.clone(),
        delay_strategy: runtime.delay_strategy.clone(),
    })
}

/// Rejects a settlement whose token belongs to a stale connection generation
/// or channel (acknowledging it would settle the wrong delivery).
fn ensure_live_generation(
    connection_key: crate::pool::ConnectionKey,
    generation: u64,
    channel_id: u16,
    token: &DeliveryTokenInner,
) -> Result<(), ConsumerError> {
    if connection_key != token.connection_key
        || generation != token.generation
        || channel_id != token.channel_id
    {
        return Err(ConsumerError::new(
            ConsumerErrorKind::StaleGeneration,
            "delivery belongs to a stale connection generation or channel",
        ));
    }
    Ok(())
}

fn launch_settlement(state: &mut ActorState, channel_key: ChannelKey, params: SettleParams) {
    let Some(launch) = stage_settlement_launch(state, &channel_key, &params.token) else {
        return;
    };
    let delivery_tag = params.token.delivery_tag;
    let settlement = params.settlement;
    let token = params.token.clone();
    drop(params);

    state.pending_settlements.push(Box::pin(async move {
        let result = execute_settlement(
            &launch.channel,
            launch.connection_key,
            launch.generation,
            launch.channel_id,
            delivery_tag,
            settlement,
            &token,
            launch.publisher.as_ref(),
            launch.destination.as_ref(),
            launch.delay_strategy.as_ref(),
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
    ensure_live_generation(connection_key, generation, channel_id, token)?;

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
    // Validation only: the publication carries the ORIGINAL destination and
    // the raw delay, and the publisher actor performs the (single) delayed
    // routing plus its lazy infrastructure declaration. Pre-routing here and
    // publishing to the delayed exchange made the actor route a second time
    // (`{exchange}.delayed.delayed`, never declared or bound), and bypassed
    // the TTL delay-queue declaration entirely (issue #196).
    DelayRouter::route(strategy, destination, delay_ms).map_err(|error| {
        // A delay no compiled strategy can honor (e.g. beyond the largest
        // TTL bucket) is permanent: the caller must settle the original
        // delivery terminally instead of leaving it pending in a redelivery
        // loop.
        ConsumerError::new(ConsumerErrorKind::InvalidDelay, error.to_string())
    })?;
    let mut properties = MessageProperties::new(token.message_id.as_str());
    properties.correlation_id = token.correlation_id.as_ref().map(|s| Arc::from(s.as_str()));
    properties.headers = AttemptsResolver::default()
        .delayed_headers(&token.headers, token.attempts)
        .map_err(|error| ConsumerError::new(ConsumerErrorKind::MaxAttempts, error.to_string()))?;
    properties.delay_ms = Some(u64::try_from(delay_ms).unwrap_or(u64::MAX));
    let request = PublishRequest::new(
        destination.clone(),
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
            "{detail}; message {} acknowledged and dropped (no dead-letter exchange configured)",
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
    let Some(launch) = stage_settlement_launch(state, &channel_key, &params.token) else {
        return;
    };
    let target_tag = params.token.delivery_tag;
    let token = params.token.clone();
    let affected_tokens = params.affected_tokens;

    state.pending_settle_throughs.push(Box::pin(async move {
        let result = execute_settle_through(
            &launch.channel,
            launch.connection_key,
            launch.generation,
            launch.channel_id,
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
    ensure_live_generation(connection_key, generation, channel_id, token)?;

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
