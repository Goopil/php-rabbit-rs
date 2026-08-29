use std::{future::Future, pin::Pin, sync::Arc};

use arc_swap::ArcSwapOption;
use bytes::Bytes;
use flume::{Receiver, Sender};
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use tokio::sync::oneshot;

use super::{PublishError, PublishErrorKind};
use crate::transport::{PublishRequest as TransportRequest, PublisherChannel};

/// Boxed publish future tracked in the pump's in-flight set.
type PublishFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A background pump that pipelines blind (fire-and-forget) publishes.
///
/// The pump owns a bounded `flume` intake queue. A background tokio task
/// continuously pulls jobs from the queue and pushes their publish future into
/// a `FuturesUnordered` in-flight set, so intake overlaps with the drain of
/// pending transport writes instead of serializing on them (one lapin publish
/// at a time). The in-flight set is bounded: intake pauses while
/// [`buffer_capacity.saturating_mul(2).max(128)`](Self::spawn) publishes are
/// pending on the transport.
///
/// The transport channel is stored in an [`ArcSwapOption`] so the actor can
/// hot-swap it after connection recovery. When the channel is `None`
/// (suspended during recovery), jobs are silently dropped.
///
/// # Delivery semantics
///
/// Blind mode is an explicit fire-and-forget contract: once a job has been
/// accepted by the pump, a transport-level error, a cleared channel during
/// recovery, or the pump being closed can silently lose the message. No
/// replay, no waiter, no loud logging. Backpressure is expressed by blocking
/// in [`send`](Self::send) — never by an error — so the earliest observable
/// failure for a caller is a closed pump returning
/// [`PublishErrorKind::Closed`].
pub struct PublishPump {
    tx: Sender<PumpJob>,
    channel: Arc<ArcSwapOption<PumpChannel>>,
}

/// Sized wrapper around `Arc<dyn PublisherChannel>` so it can be stored in
/// `ArcSwapOption` (which requires `Sized` types for its `RefCnt` impls).
#[derive(Clone)]
struct PumpChannel(Arc<dyn PublisherChannel>);

impl std::fmt::Debug for PumpChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PumpChannel").finish_non_exhaustive()
    }
}

struct PumpJob {
    request: TransportRequest,
    barrier_tx: Option<oneshot::Sender<()>>,
}

impl PublishPump {
    /// Spawns a background pump task that pipelines publishes to the transport.
    ///
    /// The intake queue is bounded by `buffer_capacity` (minimum 1) and the
    /// in-flight set by `buffer_capacity.saturating_mul(2).max(128)` — with
    /// the default capacity of 1024 this yields a queue of 1024 and up to
    /// 2048 publishes pending on the transport.
    ///
    /// # Panics
    ///
    /// Never panics. The pump task exits cleanly when the sender is dropped,
    /// after draining its remaining in-flight publishes.
    #[must_use]
    pub fn spawn(channel: Arc<dyn PublisherChannel>, buffer_capacity: usize) -> Self {
        let (tx, rx) = flume::bounded(buffer_capacity.max(1));
        let inflight_cap = buffer_capacity.saturating_mul(2).max(128);
        let channel_slot: Arc<ArcSwapOption<PumpChannel>> =
            Arc::new(ArcSwapOption::from_pointee(PumpChannel(channel)));
        tokio::spawn(pump_loop(channel_slot.clone(), rx, inflight_cap));
        Self {
            tx,
            channel: channel_slot,
        }
    }

    /// Hot-swaps the transport channel used by the background pump.
    pub fn update_channel(&self, channel: Arc<dyn PublisherChannel>) {
        self.channel.store(Some(Arc::new(PumpChannel(channel))));
    }

    /// Clears the transport channel, causing the pump to drop messages until
    /// a new channel is provided via [`update_channel`](Self::update_channel).
    pub fn clear_channel(&self) {
        self.channel.store(None);
    }

    /// Enqueues a publish job, applying backpressure by blocking the caller
    /// while the intake queue is full.
    ///
    /// # Errors
    ///
    /// Returns [`PublishErrorKind::Closed`] when the pump is closed.
    pub async fn send(&self, request: TransportRequest) -> Result<(), PublishError> {
        self.tx
            .send_async(PumpJob {
                request,
                barrier_tx: None,
            })
            .await
            .map_err(|_| PublishError::new(PublishErrorKind::Closed, "publish pump is closed"))
    }

    /// Flush barrier: resolves once every job enqueued before this call has
    /// been handed to the transport (or dropped for lack of a channel).
    ///
    /// # Errors
    ///
    /// Returns [`PublishErrorKind::Closed`] when the pump is closed before
    /// the barrier could be processed.
    pub async fn flush(&self) -> Result<(), PublishError> {
        let (barrier_tx, barrier_rx) = oneshot::channel();
        self.tx
            .send_async(PumpJob {
                request: TransportRequest::new(
                    Arc::<str>::from(""),
                    Arc::<str>::from(""),
                    Bytes::new(),
                ),
                barrier_tx: Some(barrier_tx),
            })
            .await
            .map_err(|_| PublishError::new(PublishErrorKind::Closed, "publish pump is closed"))?;
        barrier_rx.await.map_err(|_| {
            PublishError::new(
                PublishErrorKind::Closed,
                "publish pump closed before the flush barrier completed",
            )
        })
    }

    /// Returns the number of queued jobs waiting to be pumped.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tx.len()
    }

    /// Returns `true` if no jobs are queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tx.is_empty()
    }
}

impl std::fmt::Debug for PublishPump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishPump")
            .field("queued", &self.len())
            .finish_non_exhaustive()
    }
}

async fn pump_loop(
    channel: Arc<ArcSwapOption<PumpChannel>>,
    rx: Receiver<PumpJob>,
    inflight_cap: usize,
) {
    let mut inflight: FuturesUnordered<PublishFuture> = FuturesUnordered::new();
    loop {
        tokio::select! {
            biased;

            // 1) Drain completed publishes first so in-flight slots free up
            //    and the transport keeps making progress.
            Some(()) = inflight.next(), if !inflight.is_empty() => {}

            // 2) Intake while under the in-flight cap: overlap enqueueing with
            //    the drain of publishes still pending on the transport.
            maybe_job = rx.recv_async(), if inflight.len() < inflight_cap => {
                let Ok(job) = maybe_job else { break };

                if let Some(barrier_tx) = job.barrier_tx {
                    // Flush barrier: every job enqueued before the barrier is
                    // already in `inflight` (or was dropped for lack of a
                    // channel) — drain them all, then resolve the barrier.
                    while inflight.next().await.is_some() {}
                    let _ = barrier_tx.send(());
                    continue;
                }

                // With a channel, the publish error is a silent loss (blind
                // semantics); without one (recovery in progress) the job is
                // dropped silently.
                if let Some(ch) = channel.load_full() {
                    inflight.push(Box::pin(async move {
                        let _ = ch.0.publish(job.request).await;
                    }));
                }

                // Advance futures that already completed without blocking.
                while inflight.next().now_or_never().flatten().is_some() {}
            }

            // 3) Sender dropped and nothing in flight: exit.
            else => break,
        }
    }
    // Final drain so in-flight publishes are not cancelled on shutdown.
    while inflight.next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;
    use tokio::time::timeout;

    use super::*;
    use crate::config::{BrokerConfig, Credentials, Endpoint, TlsConfig};
    use crate::transport::{
        PublishProperties, Transport,
        mock::{MockPublishGate, MockTransport, TransportOperation},
    };

    fn broker() -> BrokerConfig {
        BrokerConfig {
            name: "primary".to_owned(),
            hosts: vec![Endpoint::new("localhost", 5672)],
            vhost: "/".to_owned(),
            credentials: Credentials::new("guest", "guest"),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        }
    }

    async fn mock_channel(transport: &MockTransport) -> Arc<dyn PublisherChannel> {
        Arc::from(
            transport
                .connect(&broker())
                .await
                .expect("connection")
                .open_publisher()
                .await
                .expect("publisher"),
        )
    }

    fn request(message_id: &str) -> TransportRequest {
        TransportRequest {
            exchange: Arc::from("jobs"),
            routing_key: Arc::from("high"),
            payload: Bytes::from_static(b"payload"),
            mandatory: false,
            properties: PublishProperties {
                message_id: Some(message_id.to_owned()),
                ..PublishProperties::default()
            },
        }
    }

    fn publish_requests(transport: &MockTransport) -> Vec<TransportRequest> {
        transport
            .operations()
            .into_iter()
            .filter_map(|operation| match operation {
                TransportOperation::Publish(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    /// Yields until the pump task has drained its intake queue.
    async fn settle(pump: &PublishPump) {
        for _ in 0..200 {
            if pump.is_empty() {
                tokio::task::yield_now().await;
                tokio::task::yield_now().await;
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("pump did not drain its intake queue");
    }

    /// Yields until the transport recorded `expected` publishes.
    async fn wait_for_publishes(transport: &MockTransport, expected: usize) {
        for _ in 0..200 {
            if publish_requests(transport).len() == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("transport did not record {expected} publishes");
    }

    #[tokio::test(start_paused = true)]
    async fn send_does_not_block_while_publishes_are_pending() {
        let transport = MockTransport::default();
        let channel = mock_channel(&transport).await;
        // Intake queue of 4, in-flight cap well above 8: all eight sends must
        // be accepted while every publish is held pending by its gate.
        let gates: Vec<MockPublishGate> = (0..8).map(|_| transport.push_publish_gate()).collect();
        let pump = PublishPump::spawn(channel, 4);

        for index in 0..8 {
            let send = pump.send(request(&index.to_string()));
            timeout(Duration::from_secs(1), send)
                .await
                .expect("send must not block while in-flight publishes are pending")
                .expect("send accepted while the pump is alive");
        }

        assert!(
            publish_requests(&transport).is_empty(),
            "gated publishes must not be recorded before their gate is released"
        );

        for gate in &gates {
            gate.release();
        }
        wait_for_publishes(&transport, 8).await;
    }

    #[tokio::test(start_paused = true)]
    async fn send_blocks_at_inflight_cap_and_unblocks_on_completion() {
        let transport = MockTransport::default();
        let channel = mock_channel(&transport).await;
        // buffer_capacity = 2 → intake queue of 2, in-flight cap 128.
        let gates: Vec<MockPublishGate> = (0..131).map(|_| transport.push_publish_gate()).collect();
        let pump = PublishPump::spawn(channel, 2);

        // Fill the in-flight cap while every publish is gated.
        for index in 0..128 {
            let send = pump.send(request(&index.to_string()));
            timeout(Duration::from_secs(1), send)
                .await
                .expect("pipelined intake keeps accepting sends below the cap")
                .expect("send accepted");
        }
        settle(&pump).await;

        // The intake queue absorbs exactly two more jobs...
        pump.send(request("128")).await.expect("queue slot 1");
        pump.send(request("129")).await.expect("queue slot 2");

        // ...then backpressure blocks the next send.
        let mut blocked = Box::pin(pump.send(request("130")));
        assert!(
            timeout(Duration::from_millis(50), blocked.as_mut())
                .await
                .is_err(),
            "send must block once the in-flight cap and the intake queue are full"
        );

        // Completing one publish frees a slot: the blocked send must proceed.
        assert!(gates[0].release(), "gate released");
        timeout(Duration::from_secs(1), blocked.as_mut())
            .await
            .expect("blocked send must unblock once a publish completes")
            .expect("send accepted");

        for gate in &gates {
            gate.release();
        }
        wait_for_publishes(&transport, 131).await;
    }

    #[tokio::test(start_paused = true)]
    async fn flush_resolves_only_after_enqueued_publishes_reach_the_transport() {
        let transport = MockTransport::default();
        let channel = mock_channel(&transport).await;
        let gate = transport.push_publish_gate();
        let pump = Arc::new(PublishPump::spawn(channel, 4));

        pump.send(request("gated")).await.expect("send accepted");
        settle(&pump).await;

        let flush_pump = Arc::clone(&pump);
        let flush = tokio::spawn(async move { flush_pump.flush().await });
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        assert!(
            !flush.is_finished(),
            "flush must wait for pending publishes to be handed to the transport"
        );
        assert!(publish_requests(&transport).is_empty());

        assert!(gate.release(), "gate released");
        wait_for_publishes(&transport, 1).await;

        let result = timeout(Duration::from_secs(1), flush)
            .await
            .expect("flush must not hang once the transport accepted everything")
            .expect("flush task joined");
        assert!(result.is_ok(), "flush succeeded");
    }

    #[tokio::test(start_paused = true)]
    async fn flush_without_channel_resolves_after_drain_without_error() {
        let transport = MockTransport::default();
        let channel = mock_channel(&transport).await;
        let pump = PublishPump::spawn(channel, 4);

        pump.clear_channel();
        pump.send(request("dropped")).await.expect("send accepted");
        timeout(Duration::from_secs(1), pump.flush())
            .await
            .expect("flush must not hang without a channel")
            .expect("flush must succeed after draining, even without a channel");

        assert!(publish_requests(&transport).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn cleared_channel_drops_jobs_and_updated_channel_resumes_publishing() {
        let transport = MockTransport::default();
        let channel = mock_channel(&transport).await;
        let pump = PublishPump::spawn(channel, 4);

        pump.clear_channel();
        pump.send(request("lost"))
            .await
            .expect("send stays accepted while the pump is alive");
        settle(&pump).await;
        assert!(
            publish_requests(&transport).is_empty(),
            "jobs enqueued without a channel are dropped silently"
        );

        pump.update_channel(mock_channel(&transport).await);
        pump.send(request("resumed"))
            .await
            .expect("send after update");
        wait_for_publishes(&transport, 1).await;
        assert_eq!(
            publish_requests(&transport)[0]
                .properties
                .message_id
                .as_deref(),
            Some("resumed"),
            "publishes resume on the updated channel"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn send_and_flush_fail_without_hanging_once_the_pump_is_closed() {
        let transport = MockTransport::default();
        let channel = mock_channel(&transport).await;

        // Closed-pump state: the background task is gone (receiver dropped).
        let (tx, rx) = flume::bounded::<PumpJob>(4);
        drop(rx);
        let pump = PublishPump {
            tx,
            channel: Arc::new(ArcSwapOption::from_pointee(PumpChannel(channel))),
        };

        let outcome = timeout(Duration::from_secs(1), pump.send(request("x")))
            .await
            .expect("send must not hang on a closed pump");
        assert!(
            matches!(outcome, Err(ref error) if error.kind() == super::super::PublishErrorKind::Closed),
            "send on a closed pump must return Closed, got {outcome:?}"
        );

        let outcome = timeout(Duration::from_secs(1), pump.flush())
            .await
            .expect("flush must not hang on a closed pump");
        assert!(
            matches!(outcome, Err(ref error) if error.kind() == super::super::PublishErrorKind::Closed),
            "flush on a closed pump must return Closed, got {outcome:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_pump_task_resolves_pending_barriers_with_closed() {
        let transport = MockTransport::default();
        let channel = mock_channel(&transport).await;
        let slot = Arc::new(ArcSwapOption::from_pointee(PumpChannel(channel)));
        let (tx, rx) = flume::bounded::<PumpJob>(4);
        let task = tokio::spawn(pump_loop(slot, rx, 128));

        let gate = transport.push_publish_gate();
        tx.send_async(PumpJob {
            request: request("pending"),
            barrier_tx: None,
        })
        .await
        .expect("enqueue publish");
        gate.wait_entered().await;

        let (barrier_tx, barrier_rx) = oneshot::channel();
        tx.send_async(PumpJob {
            request: TransportRequest::new(
                Arc::<str>::from(""),
                Arc::<str>::from(""),
                Bytes::new(),
            ),
            barrier_tx: Some(barrier_tx),
        })
        .await
        .expect("enqueue barrier");
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }

        // Cancelling the pump task (runtime teardown) drops the barrier
        // sender without replying: the waiter must observe `Closed`, not hang.
        task.abort();
        let result = timeout(Duration::from_secs(1), barrier_rx)
            .await
            .expect("barrier must resolve promptly once the pump task is cancelled");
        assert!(
            result.is_err(),
            "barrier oneshot must be dropped without a reply, got {result:?}"
        );
    }
}
