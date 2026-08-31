//! Fan-in composition of per-broker consumer sets.
//!
//! A worker profile may subscribe to queues on several brokers. Each broker's
//! coordinator spawns its own [`ConsumerSetHandle`] for the profile (see
//! `recover_generation`); [`ConsumerHandle`] merges those sets into a single
//! consumer so deliveries from every broker surface through one API.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use futures_util::stream::{FuturesUnordered, StreamExt};

use super::{
    ConsumerError, ConsumerErrorKind, Delivery, DeliveryTokenInner, SettleError, Settlement,
    SettlementError, SubscriptionId, actor::ConsumerCommand, set::ConsumerSetHandle,
};
use crate::metrics::MetricsSnapshot;

/// A consumer handle that merges deliveries from one or more per-broker
/// [`ConsumerSetHandle`]s.
///
/// # Semantics
///
/// - **Delivery merge**: [`Self::next`] polls every live source, rotating the
///   starting source on each call (round-robin), so no broker starves another.
///   No cross-broker ordering is guaranteed. The composite itself buffers
///   nothing: deliveries stay in each source's bounded buffer until received,
///   and per-source backpressure (prefetch, `max_buffered_bytes`) is
///   preserved.
/// - **Settlement routing**: every [`Delivery`] token carries the command
///   sender of the set actor that produced it, so `ack`, `release`, and
///   `reject` — including the batch variants — always reach the broker
///   connection the delivery came from, with its original generation.
/// - **Recovery**: each source keeps its own coordinator. A broker that is
///   recovering does not block consumption from the others: when one source
///   closes (its set was replaced by a newer generation or the pool closed),
///   it is retired from the rotation and the remaining sources keep
///   delivering. The retire surfaces a one-shot
///   [`ConsumerErrorKind::SourceReplaced`] signal (see [`Self::next`] and
///   [`Self::try_next`]): the caller should re-fetch the consumer from the
///   pool, which rebuilds the composite from every coordinator's current set
///   without duplicating subscriptions. Errors other than closure and the
///   retire signal surface to the caller as well. When the last source is
///   retired, calls fail with a closed-consumer error.
/// - **Close**: closing the composite fans out to every underlying set.
#[derive(Clone)]
pub struct ConsumerHandle {
    inner: Arc<CompositeInner>,
}

struct CompositeInner {
    sources: Vec<ConsumerSetHandle>,
    retired: Vec<AtomicBool>,
    round_robin: AtomicUsize,
    /// One-slot stash for errors that must surface once: mid-drain batch
    /// errors and the one-shot re-fetch signal pushed when a source is
    /// retired while others remain live.
    pending_error: Mutex<Option<ConsumerError>>,
    /// Set when [`ConsumerHandle::close`] was called by the owner: source
    /// closures observed afterwards are expected teardown, not a broker
    /// replacement, so no re-fetch signal is pushed.
    closed_by_caller: AtomicBool,
}

impl ConsumerHandle {
    /// Composes a multi-broker consumer from one handle per broker.
    ///
    /// The caller must supply at least one source; each source must come from
    /// a different broker's coordinator.
    #[must_use]
    pub(crate) fn from_sources(sources: Vec<ConsumerSetHandle>) -> Self {
        debug_assert!(
            !sources.is_empty(),
            "a composite consumer requires at least one source"
        );
        let retired = sources.iter().map(|_| AtomicBool::new(false)).collect();
        Self {
            inner: Arc::new(CompositeInner {
                sources,
                retired,
                round_robin: AtomicUsize::new(0),
                pending_error: Mutex::new(None),
                closed_by_caller: AtomicBool::new(false),
            }),
        }
    }

    /// Returns the connection generation of the first source.
    ///
    /// Use [`Self::source_generations`] for the per-broker generations.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.inner
            .sources
            .first()
            .map_or(1, ConsumerSetHandle::generation)
    }

    /// Returns the connection generation of each source, in source order.
    #[must_use]
    pub(crate) fn source_generations(&self) -> Vec<u64> {
        self.inner
            .sources
            .iter()
            .map(ConsumerSetHandle::generation)
            .collect()
    }

    #[must_use]
    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        // Every source shares the pool-level metrics registry, so any
        // source's snapshot is the pool-wide snapshot.
        self.inner.sources.first().map_or_else(
            || crate::metrics::Metrics::default().snapshot(),
            super::set::ConsumerSetHandle::metrics_snapshot,
        )
    }

    /// Drains all settlement errors recorded by every source since the last
    /// call. See [`ConsumerSetHandle::drain_errors`] for the error semantics.
    #[must_use]
    pub fn drain_errors(&self) -> Vec<SettlementError> {
        let mut errors = Vec::new();
        for source in &self.inner.sources {
            errors.extend(source.drain_errors());
        }
        errors
    }

    /// Fire-and-forget settlement routed to the set that produced the token.
    ///
    /// Unlike [`ConsumerSetHandle::try_settle`], which sends through its own
    /// actor, this reads the routing channel from the token itself so a
    /// settlement always reaches the originating broker regardless of which
    /// source delivered it. Does not perform the pending→terminal CAS — the
    /// caller is responsible for ensuring the delivery is not double-settled.
    ///
    /// # Errors
    ///
    /// Returns [`SettleError::ChannelFull`] when the originating actor's
    /// command channel is at capacity, or [`SettleError::Closed`] when it
    /// has stopped.
    pub fn try_settle(
        &self,
        token: std::sync::Arc<DeliveryTokenInner>,
        settlement: Settlement,
    ) -> Result<(), SettleError> {
        let commands = token.commands.clone();
        commands
            .try_send(ConsumerCommand::Settle { token, settlement })
            .map_err(|e| map_try_send_error(&e))
    }

    /// Fire-and-forget batch settlement routed to the set that produced the
    /// token. See [`Self::try_settle`] for the routing semantics.
    ///
    /// # Errors
    ///
    /// Returns [`SettleError::ChannelFull`] when the originating actor's
    /// command channel is at capacity, or [`SettleError::Closed`] when it
    /// has stopped.
    pub fn try_settle_through(
        &self,
        token: std::sync::Arc<DeliveryTokenInner>,
    ) -> Result<(), SettleError> {
        let commands = token.commands.clone();
        commands
            .try_send(ConsumerCommand::SettleThrough { token })
            .map_err(|e| map_try_send_error(&e))
    }

    /// Tries to receive the next delivery without blocking, scanning sources
    /// in round-robin order.
    ///
    /// Returns `Ok(Some(delivery))` when one is available, `Ok(None)` when
    /// every live source is empty, or `Err` when a source surfaces an error,
    /// a source was retired (one-shot re-fetch signal), or every source is
    /// retired.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the consumer is closed, a source error is
    /// encountered, or a source was retired while others remain live.
    pub fn try_next(&self) -> Result<Option<Delivery>, ConsumerError> {
        if self.inner.sources.len() == 1 {
            return self.inner.sources[0].try_next();
        }
        let Some(rotation) = self.rotation() else {
            return Err(ConsumerError::closed());
        };
        for index in rotation {
            if self.is_retired(index) {
                continue;
            }
            match self.inner.sources[index].try_next() {
                Ok(Some(delivery)) => return Ok(Some(delivery)),
                Ok(None) => {}
                Err(error) if error.kind() == ConsumerErrorKind::Closed => {
                    self.retire(index);
                }
                Err(error) => return Err(error),
            }
        }
        if self.all_retired() {
            return Err(ConsumerError::closed());
        }
        if let Some(error) = self.take_pending_error() {
            return Err(error);
        }
        Ok(None)
    }

    /// Drains up to `max` deliveries across all live sources in one call,
    /// scanning sources in round-robin order.
    ///
    /// The requested `max` is clamped to `1..=256`. When a source surfaces a
    /// non-closure error mid-drain, already-drained deliveries are returned
    /// and the error is stashed to surface on the next call with an empty
    /// batch (mirroring [`ConsumerSetHandle::try_next_batch`]).
    ///
    /// # Errors
    ///
    /// Returns a typed error when the consumer is closed or a source error
    /// is encountered with an empty batch.
    pub fn try_next_batch(&self, max: usize) -> Result<Vec<Delivery>, ConsumerError> {
        if self.inner.sources.len() == 1 {
            return self.inner.sources[0].try_next_batch(max);
        }
        let max = max.clamp(1, 256);
        let Some(rotation) = self.rotation() else {
            return Err(ConsumerError::closed());
        };
        let mut batch = Vec::new();
        for index in rotation {
            if batch.len() >= max {
                break;
            }
            if self.is_retired(index) {
                continue;
            }
            let remaining = max - batch.len();
            match self.inner.sources[index].try_next_batch(remaining) {
                Ok(part) => batch.extend(part),
                Err(error) if error.kind() == ConsumerErrorKind::Closed => {
                    self.retire(index);
                }
                Err(error) => {
                    if batch.is_empty() {
                        return Err(error);
                    }
                    *self
                        .inner
                        .pending_error
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                    break;
                }
            }
        }
        if batch.is_empty() {
            if self.all_retired() {
                return Err(ConsumerError::closed());
            }
            if let Some(error) = self.take_pending_error() {
                return Err(error);
            }
        }
        Ok(batch)
    }

    /// Waits for the next delivery from any live source.
    ///
    /// Sources are polled in round-robin order (the starting source rotates
    /// on every call), so a broker cannot starve another. When a source is
    /// closed it is retired and the remaining sources keep delivering; the
    /// retire surfaces a one-shot [`ConsumerErrorKind::SourceReplaced`]
    /// signal instead of leaving the caller parked on a degraded consumer —
    /// re-fetch the consumer from the pool to resume deliveries from the
    /// replaced broker. When every source is retired, this returns a
    /// closed-consumer error.
    ///
    /// # Errors
    ///
    /// Returns a typed source, transport, re-fetch-signal, or
    /// closed-consumer error.
    pub async fn next(&self) -> Result<Delivery, ConsumerError> {
        if self.inner.sources.len() == 1 {
            return self.inner.sources[0].next().await;
        }
        let Some(rotation) = self.rotation() else {
            return Err(ConsumerError::closed());
        };
        let mut futures = FuturesUnordered::new();
        for index in rotation {
            if self.is_retired(index) {
                continue;
            }
            let source = &self.inner.sources[index];
            futures.push(async move { (index, source.next().await) });
        }
        while let Some((index, result)) = futures.next().await {
            match result {
                Ok(delivery) => return Ok(delivery),
                Err(error) if error.kind() == ConsumerErrorKind::Closed => {
                    self.retire(index);
                    // Wake a caller parked on a degraded composite: if other
                    // sources remain live, surface the one-shot re-fetch
                    // signal instead of waiting on them indefinitely.
                    if !self.all_retired()
                        && let Some(pending) = self.take_pending_error()
                    {
                        return Err(pending);
                    }
                }
                Err(error) => return Err(error),
            }
        }
        // Every live source resolved without delivering — all were retired.
        if self.all_retired() {
            return Err(ConsumerError::closed());
        }
        Err(self
            .take_pending_error()
            .unwrap_or_else(ConsumerError::closed))
    }

    /// Acknowledges a contiguous prefix of deliveries up to and including the
    /// given delivery, routed to the set that produced it. See
    /// [`ConsumerSetHandle::ack_through`] for the prefix semantics.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the originating actor's command channel is
    /// full or closed.
    #[allow(clippy::unused_async)]
    pub async fn ack_through(&self, delivery: &Delivery) -> Result<(), ConsumerError> {
        self.try_settle_through(delivery.inner_token().clone())
            .map_err(|e| match e {
                SettleError::ChannelFull => ConsumerError::new(
                    ConsumerErrorKind::SettlementInProgress,
                    "settlement command channel is full",
                ),
                SettleError::Closed => ConsumerError::closed(),
            })
    }

    /// Records a new connection generation for one subscription on whichever
    /// source owns it.
    ///
    /// # Errors
    ///
    /// Returns a typed error when no source owns the subscription or an
    /// actor is unavailable.
    pub async fn update_generation(
        &self,
        subscription: SubscriptionId,
        generation: u64,
    ) -> Result<(), ConsumerError> {
        let mut first_error = None;
        for source in &self.inner.sources {
            match source
                .update_generation(subscription.clone(), generation)
                .await
            {
                Ok(()) => return Ok(()),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.unwrap_or_else(ConsumerError::closed))
    }

    /// Closes every underlying set and wakes all pending calls to
    /// [`Self::next`].
    ///
    /// Source closures observed after this call are expected teardown: no
    /// re-fetch signal is pushed and any pending signal is discarded.
    ///
    /// # Errors
    ///
    /// Returns the first typed error raised by an underlying set close.
    pub async fn close(&self) -> Result<(), ConsumerError> {
        self.inner.closed_by_caller.store(true, Ordering::Release);
        *self
            .inner
            .pending_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let mut first_error = None;
        for source in &self.inner.sources {
            if let Err(error) = source.close().await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Returns the source indices in poll order, starting at the current
    /// round-robin offset. Returns `None` when there is no source to poll.
    fn rotation(&self) -> Option<Vec<usize>> {
        let len = self.inner.sources.len();
        if len == 0 {
            return None;
        }
        let start = self.inner.round_robin.fetch_add(1, Ordering::Relaxed) % len;
        Some((0..len).map(|offset| (start + offset) % len).collect())
    }

    fn is_retired(&self, index: usize) -> bool {
        self.inner.retired[index].load(Ordering::Acquire)
    }

    /// Retires a source from the rotation. Idempotent.
    ///
    /// The first retire of a source — while other sources remain live and
    /// the owner has not closed the composite — pushes a one-shot
    /// [`ConsumerErrorKind::SourceReplaced`] signal onto the pending slot so
    /// the caller learns a broker's set was replaced (typically by a
    /// recovery generation) and can re-fetch the consumer. The signal is
    /// delivered once by [`Self::next`], [`Self::try_next`], or
    /// [`Self::try_next_batch`], then the composite goes quiet again.
    fn retire(&self, index: usize) {
        if self.inner.retired[index].swap(true, Ordering::AcqRel) {
            return;
        }
        if self.all_retired() || self.inner.closed_by_caller.load(Ordering::Acquire) {
            // Terminal state or expected teardown: the closed error is the
            // signal, not a degradation notice.
            return;
        }
        let mut pending = self
            .inner
            .pending_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.is_none() {
            *pending = Some(ConsumerError::new(
                ConsumerErrorKind::SourceReplaced,
                "broker source replaced by recovery; re-fetch consumer",
            ));
        }
    }

    fn all_retired(&self) -> bool {
        self.inner
            .retired
            .iter()
            .all(|retired| retired.load(Ordering::Acquire))
    }

    fn take_pending_error(&self) -> Option<ConsumerError> {
        self.inner
            .pending_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

fn map_try_send_error(
    error: &tokio::sync::mpsc::error::TrySendError<ConsumerCommand>,
) -> SettleError {
    match error {
        tokio::sync::mpsc::error::TrySendError::Full(_) => SettleError::ChannelFull,
        tokio::sync::mpsc::error::TrySendError::Closed(_) => SettleError::Closed,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc, time::Duration};

    use bytes::Bytes;

    use crate::{
        config::{
            BrokerConfig, Config, Credentials, Endpoint, PublisherConfigSection, TlsConfig,
            TopologyMode,
        },
        consumer::{
            ConsumerErrorKind, ConsumerSet, DeliveryState, Subscription, SubscriptionPolicy,
        },
        metrics::Metrics,
        pool::ConnectionKey,
        transport::{
            Delivery as TransportDelivery, QueueKind, Transport, TransportError,
            mock::{MockTransport, TransportOperation},
        },
    };

    fn broker(name: &str) -> BrokerConfig {
        BrokerConfig {
            name: name.to_owned(),
            hosts: vec![Endpoint::new("localhost", 5672)],
            vhost: "/".to_owned(),
            credentials: Credentials::new("guest", "guest"),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        }
    }

    fn connection_key(name: &str) -> ConnectionKey {
        let config = Config {
            brokers: vec![broker(name)],
            workers: vec![],
            topology_mode: TopologyMode::External,
            delay: crate::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
            consumer: crate::config::ConsumerConfigSection::default(),
            queue_type: QueueKind::Quorum,
            queue_durable: true,
        }
        .validate()
        .expect("valid config");
        ConnectionKey::from_config(&config)
    }

    fn delivery(tag: u64, payload: &'static [u8]) -> TransportDelivery {
        TransportDelivery {
            delivery_tag: tag,
            exchange: "jobs".to_owned(),
            routing_key: "high".to_owned(),
            redelivered: false,
            message_id: None,
            correlation_id: None,
            headers: Arc::new(BTreeMap::new()),
            payload: Bytes::from_static(payload),
        }
    }

    async fn subscription(transport: &MockTransport, id: &str, key: ConnectionKey) -> Subscription {
        let channel = transport
            .connect(&broker(id))
            .await
            .expect("connection")
            .open_consumer()
            .await
            .expect("consumer channel");
        Subscription::new(id, key, format!("queue.{id}"), Arc::from(channel))
            .prefetch(8)
            .channel_id(1)
            .policy(SubscriptionPolicy::new(1, 0, Duration::from_secs(1)))
    }

    /// Spawns one source per transport so each delivery lands in a
    /// deterministic source set. The per-broker handles are moved into the
    /// composite; tests that need to close a single source build their own
    /// setup (see `retiring_one_closed_source_keeps_the_others_delivering`).
    async fn two_source_composite() -> (super::ConsumerHandle, MockTransport, MockTransport) {
        let first = MockTransport::default();
        let second = MockTransport::default();
        first.push_delivery(Ok(delivery(1, b"from-first")));
        second.push_delivery(Ok(delivery(1, b"from-second")));

        let left = ConsumerSet::spawn_with_metrics(
            vec![subscription(&first, "jobs-first", connection_key("first")).await],
            Metrics::default(),
        )
        .await
        .expect("first source set");
        let right = ConsumerSet::spawn_with_metrics(
            vec![subscription(&second, "jobs-second", connection_key("second")).await],
            Metrics::default(),
        )
        .await
        .expect("second source set");

        (
            super::ConsumerHandle::from_sources(vec![left, right]),
            first,
            second,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn merges_deliveries_from_every_source() {
        let (consumer, ..) = two_source_composite().await;

        tokio::time::sleep(Duration::from_millis(50)).await;

        let first = consumer.next().await.expect("first delivery");
        let second = consumer.next().await.expect("second delivery");
        let payloads = [first.payload.clone(), second.payload.clone()];
        assert!(payloads.contains(&Bytes::from_static(b"from-first")));
        assert!(payloads.contains(&Bytes::from_static(b"from-second")));
        let subscriptions = [first.subscription.clone(), second.subscription.clone()];
        assert!(subscriptions.contains(&super::super::SubscriptionId::new("jobs-first")));
        assert!(subscriptions.contains(&super::super::SubscriptionId::new("jobs-second")));

        consumer.close().await.expect("close");
    }

    #[tokio::test(start_paused = true)]
    async fn settlements_route_to_the_origin_source() {
        let (consumer, first, second, ..) = two_source_composite().await;

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Receive both deliveries and ack them.
        let one = consumer.next().await.expect("first delivery");
        let two = consumer.next().await.expect("second delivery");
        one.ack().await.expect("ack one");
        two.ack().await.expect("ack two");

        tokio::time::advance(Duration::from_millis(20)).await;
        tokio::task::yield_now().await;

        let ack_count = |transport: &MockTransport| {
            transport
                .operations()
                .iter()
                .filter(|operation| matches!(operation, TransportOperation::Ack { .. }))
                .count()
        };
        assert_eq!(ack_count(&first), 1, "first source must ack its delivery");
        assert_eq!(ack_count(&second), 1, "second source must ack its delivery");

        assert_eq!(one.state(), DeliveryState::Acked);
        assert_eq!(two.state(), DeliveryState::Acked);

        consumer.close().await.expect("close");
    }

    #[tokio::test(start_paused = true)]
    async fn close_fans_out_to_every_source() {
        let (consumer, first, second, ..) = two_source_composite().await;

        consumer.close().await.expect("close");

        tokio::time::advance(Duration::from_millis(20)).await;
        tokio::task::yield_now().await;

        for (name, transport) in [("first", &first), ("second", &second)] {
            let closes = transport
                .operations()
                .iter()
                .filter(|operation| matches!(operation, TransportOperation::CloseChannel))
                .count();
            assert_eq!(closes, 1, "{name} source channel must be closed");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn try_next_batch_drains_across_sources() {
        let (consumer, ..) = two_source_composite().await;

        tokio::time::sleep(Duration::from_millis(50)).await;

        let batch = consumer.try_next_batch(8).expect("batch");
        assert_eq!(batch.len(), 2, "batch must drain both sources");
        let payloads: Vec<Bytes> = batch.iter().map(|d| d.payload.clone()).collect();
        assert!(payloads.contains(&Bytes::from_static(b"from-first")));
        assert!(payloads.contains(&Bytes::from_static(b"from-second")));

        consumer.close().await.expect("close");
    }

    #[tokio::test(start_paused = true)]
    async fn retiring_one_closed_source_keeps_the_others_delivering() {
        // All deliveries are pushed before the sets spawn: a mock delivery
        // stream parks on `pending()` once its queue is empty, so this is the
        // deterministic way to have items still buffered when a source closes.
        // Both per-broker handles stay alive so nothing drops and closes a
        // set ahead of the scenario.
        let first = MockTransport::default();
        let second = MockTransport::default();
        first.push_delivery(Ok(delivery(1, b"from-first-1")));
        first.push_delivery(Ok(delivery(2, b"from-first-2")));
        first.push_delivery(Ok(delivery(3, b"from-first-3")));
        second.push_delivery(Ok(delivery(1, b"from-second-1")));
        second.push_delivery(Ok(delivery(2, b"from-second-2")));

        let left = ConsumerSet::spawn_with_metrics(
            vec![subscription(&first, "jobs-first", connection_key("first")).await],
            Metrics::default(),
        )
        .await
        .expect("first source set");
        let right = ConsumerSet::spawn_with_metrics(
            vec![subscription(&second, "jobs-second", connection_key("second")).await],
            Metrics::default(),
        )
        .await
        .expect("second source set");
        let consumer = super::ConsumerHandle::from_sources(vec![left.clone(), right.clone()]);

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drain three of the five deliveries, leaving one buffered in each
        // source.
        let mut drained = Vec::new();
        while drained.len() < 3 {
            if let Some(delivery) = consumer.try_next().expect("initial drain") {
                drained.push(delivery.payload);
            }
        }
        assert!(drained.contains(&Bytes::from_static(b"from-first-1")));
        assert!(drained.contains(&Bytes::from_static(b"from-second-1")));
        assert!(drained.contains(&Bytes::from_static(b"from-first-2")));

        // Closing one source must not discard a delivery already buffered in
        // it: the composite drains it before retiring the source.
        right.close().await.expect("close second source");
        let drained_before_retire =
            tokio::time::timeout(Duration::from_millis(100), consumer.next())
                .await
                .expect("a closed source with a buffered delivery must not hang the composite")
                .expect("buffered delivery drains before retire");
        assert_eq!(
            drained_before_retire.payload,
            Bytes::from_static(b"from-second-2")
        );

        // The closed source is retired from the rotation while the remaining
        // source keeps delivering.
        let live = tokio::time::timeout(Duration::from_millis(100), consumer.next())
            .await
            .expect("no hang after retire")
            .expect("remaining source keeps delivering");
        assert_eq!(live.payload, Bytes::from_static(b"from-first-3"));

        // The retire surfaces a one-shot re-fetch signal: exactly one error,
        // then the composite goes quiet again.
        let refetch = tokio::time::timeout(Duration::from_millis(100), consumer.next())
            .await
            .expect("retire must not hang the composite")
            .expect_err("retire must surface a one-shot re-fetch signal");
        assert_eq!(refetch.kind(), ConsumerErrorKind::SourceReplaced);
        assert!(
            refetch.to_string().contains("re-fetch"),
            "signal must tell the caller to re-fetch, got: {refetch}"
        );

        // With the retired source gone and the remaining source idle, next()
        // parks waiting for work instead of failing closed (the retire is
        // stable: the closed source is never polled again, no panic).
        for _ in 0..2 {
            let idle = tokio::time::timeout(Duration::from_millis(100), consumer.next()).await;
            assert!(
                idle.is_err(),
                "an idle live source must keep the composite waiting, not closed"
            );
        }

        // Closing the remaining source fails the composite closed, and the
        // closed state is stable across repeated calls (idempotent retire,
        // no panic, no hang).
        left.close().await.expect("close first source");
        let closed = consumer
            .next()
            .await
            .expect_err("all sources retired must surface closed");
        assert_eq!(closed.kind(), ConsumerErrorKind::Closed);
        let again = consumer.next().await.expect_err("closed state is stable");
        assert_eq!(again.kind(), ConsumerErrorKind::Closed);
        assert!(
            matches!(consumer.try_next(), Err(error) if error.kind() == ConsumerErrorKind::Closed)
        );
        assert!(
            matches!(consumer.try_next_batch(4), Err(error) if error.kind() == ConsumerErrorKind::Closed)
        );

        // The composite handle stays usable: close fans out without error.
        consumer.close().await.expect("composite close");
    }

    #[tokio::test(start_paused = true)]
    async fn retiring_a_source_surfaces_exactly_one_refetch_signal() {
        let first = MockTransport::default();
        let second = MockTransport::default();
        first.push_delivery(Ok(delivery(1, b"from-first")));
        second.push_delivery(Ok(delivery(1, b"from-second")));

        let left = ConsumerSet::spawn_with_metrics(
            vec![subscription(&first, "jobs-first", connection_key("first")).await],
            Metrics::default(),
        )
        .await
        .expect("first source set");
        let right = ConsumerSet::spawn_with_metrics(
            vec![subscription(&second, "jobs-second", connection_key("second")).await],
            Metrics::default(),
        )
        .await
        .expect("second source set");
        let consumer = super::ConsumerHandle::from_sources(vec![left.clone(), right.clone()]);

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drain everything so the close is observed deterministically: the
        // remaining source has nothing buffered and its stream is parked, so
        // the retired source's closure is the first thing `next()` sees.
        while consumer.try_next().expect("drain").is_some() {}

        // A clean close via recovery (the coordinator replaced the broker's
        // set) retires the source: the composite must surface the re-fetch
        // signal instead of going quiet.
        right.close().await.expect("close second source");
        let signal = tokio::time::timeout(Duration::from_millis(100), consumer.next())
            .await
            .expect("retire must not hang the composite")
            .expect_err("retire must surface a one-shot re-fetch signal");
        assert_eq!(signal.kind(), ConsumerErrorKind::SourceReplaced);
        assert!(
            signal.to_string().contains("re-fetch"),
            "signal must tell the caller to re-fetch, got: {signal}"
        );

        // One-shot: subsequent observations are quiet — no spam, no spin, and
        // the composite is not closed while the other source remains live.
        assert!(
            matches!(consumer.try_next(), Ok(None)),
            "the signal must be delivered exactly once"
        );
        assert!(
            matches!(consumer.try_next_batch(4), Ok(batch) if batch.is_empty()),
            "the signal must not repeat through the batch API"
        );
        let idle = tokio::time::timeout(Duration::from_millis(100), consumer.next()).await;
        assert!(
            idle.is_err(),
            "the live source must keep the composite waiting after the signal"
        );

        consumer.close().await.expect("composite close");
    }

    #[tokio::test(start_paused = true)]
    async fn refetch_signal_never_masks_the_closed_terminal_state() {
        let first = MockTransport::default();
        let second = MockTransport::default();
        first.push_delivery(Ok(delivery(1, b"from-first")));
        second.push_delivery(Ok(delivery(1, b"from-second")));

        let left = ConsumerSet::spawn_with_metrics(
            vec![subscription(&first, "jobs-first", connection_key("first")).await],
            Metrics::default(),
        )
        .await
        .expect("first source set");
        let right = ConsumerSet::spawn_with_metrics(
            vec![subscription(&second, "jobs-second", connection_key("second")).await],
            Metrics::default(),
        )
        .await
        .expect("second source set");
        let consumer = super::ConsumerHandle::from_sources(vec![left.clone(), right.clone()]);

        tokio::time::sleep(Duration::from_millis(50)).await;
        while consumer.try_next().expect("drain").is_some() {}

        // Both sets close before the signal is consumed (e.g. every broker
        // recovered at once): the composite must still settle on Closed —
        // the re-fetch signal is a degradation notice, never the terminal
        // state.
        right.close().await.expect("close second source");
        left.close().await.expect("close first source");

        // The first async wake observes one closure while the other source is
        // not yet retired, so the signal surfaces once; the next call must
        // report the terminal Closed state.
        let signal = tokio::time::timeout(Duration::from_millis(100), consumer.next())
            .await
            .expect("closure must wake the waiter")
            .expect_err("closure wakes the waiter");
        assert_eq!(signal.kind(), ConsumerErrorKind::SourceReplaced);
        let closed = consumer
            .next()
            .await
            .expect_err("all sources retired must surface closed");
        assert_eq!(closed.kind(), ConsumerErrorKind::Closed);
        let stable = consumer.next().await.expect_err("closed state is stable");
        assert_eq!(stable.kind(), ConsumerErrorKind::Closed);

        // The synchronous observations see both closures in one scan: Closed
        // directly, the pending signal must not shadow the terminal state.
        assert!(
            matches!(consumer.try_next(), Err(error) if error.kind() == ConsumerErrorKind::Closed)
        );
        assert!(
            matches!(consumer.try_next_batch(4), Err(error) if error.kind() == ConsumerErrorKind::Closed)
        );

        consumer.close().await.expect("composite close");
    }

    #[tokio::test(start_paused = true)]
    async fn source_error_burst_is_bounded_and_does_not_starve_live_deliveries() {
        let first = MockTransport::default();
        let second = MockTransport::default();
        // A 200-error burst from one source: the actor's retained-error buffer
        // (SOURCE_ERROR_CAPACITY = 64 in consumer/actor.rs) must cap how many
        // of them can surface downstream.
        for _ in 0..200 {
            first.push_delivery(Err(TransportError::connection("burst error")));
        }
        first.push_delivery(Ok(delivery(1, b"from-first")));

        let left = ConsumerSet::spawn_with_metrics(
            vec![subscription(&first, "jobs-first", connection_key("first")).await],
            Metrics::default(),
        )
        .await
        .expect("first source set");
        let right = ConsumerSet::spawn_with_metrics(
            vec![subscription(&second, "jobs-second", connection_key("second")).await],
            Metrics::default(),
        )
        .await
        .expect("second source set");
        let consumer = super::ConsumerHandle::from_sources(vec![left, right]);

        // Let the pumps absorb the whole burst before any drain: no
        // intermediate dispatch runs during the burst, so the retained-error
        // buffer alone bounds what can ever surface.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut surfaced_errors = 0_usize;
        let mut saw_delivery = false;
        let mut quiet = 0_usize;
        for _ in 0..10_000 {
            match consumer.try_next() {
                Ok(Some(_)) => {
                    saw_delivery = true;
                    quiet = 0;
                }
                Ok(None) => {
                    quiet += 1;
                    tokio::time::advance(Duration::from_millis(1)).await;
                    tokio::task::yield_now().await;
                    if quiet >= 20 && saw_delivery {
                        break;
                    }
                }
                Err(_) => {
                    surfaced_errors += 1;
                    quiet = 0;
                }
            }
        }
        assert!(
            surfaced_errors > 0,
            "burst errors must surface to the caller"
        );
        assert!(
            surfaced_errors <= 64,
            "retained source errors must stay bounded (64), got {surfaced_errors}"
        );
        assert!(
            saw_delivery,
            "a live delivery must not starve behind the error burst"
        );

        consumer.close().await.expect("close");
    }
}
