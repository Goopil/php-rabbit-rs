use std::sync::Arc;

use arc_swap::ArcSwapOption;
use flume::{Receiver, Sender};

use crate::transport::{PublishRequest as TransportRequest, PublisherChannel};

/// A background pump that drains a flume channel and publishes messages
/// without waiting for confirmations (blind / fire-and-forget mode).
///
/// The pump owns a bounded `flume` channel. Producers call [`try_publish`](Self::try_publish)
/// which enqueues into the channel and returns immediately. A background
/// tokio task drains the channel and publishes each message to the transport
/// channel, discarding the confirmation receipt.
///
/// The transport channel is stored in an [`ArcSwapOption`] so the actor can
/// hot-swap it after connection recovery. When the channel is `None`
/// (suspended during recovery), publishes are silently dropped.
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
}

impl PublishPump {
    /// Spawns a background pump task that drains the channel and publishes.
    ///
    /// The pump converts each [`PublishRequest`] into a [`TransportRequest`]
    /// using `mandatory=false` (blind mode never sets the mandatory flag) and
    /// publishes it to the channel. Confirmation receipts are discarded.
    ///
    /// The transport channel is wrapped in an [`ArcSwapOption`] so the caller
    /// can update it via [`update_channel`](Self::update_channel) after
    /// connection recovery.
    ///
    /// # Panics
    ///
    /// Never panics. The pump task exits cleanly when the sender is dropped.
    #[must_use]
    pub fn spawn(channel: Arc<dyn PublisherChannel>, buffer_capacity: usize) -> Self {
        let (tx, rx) = flume::bounded(buffer_capacity.max(1));
        let channel_slot: Arc<ArcSwapOption<PumpChannel>> =
            Arc::new(ArcSwapOption::from_pointee(PumpChannel(channel)));
        tokio::spawn(pump_loop(channel_slot.clone(), rx));
        Self {
            tx,
            channel: channel_slot,
        }
    }

    /// Hot-swaps the transport channel used by the background pump.
    ///
    /// Call this after connection recovery to ensure the pump publishes to the
    /// new channel instead of the stale one.
    pub fn update_channel(&self, channel: Arc<dyn PublisherChannel>) {
        self.channel.store(Some(Arc::new(PumpChannel(channel))));
    }

    /// Clears the transport channel, causing the pump to drop messages until
    /// a new channel is provided via [`update_channel`](Self::update_channel).
    pub fn clear_channel(&self) {
        self.channel.store(None);
    }

    /// Enqueues a publish job. Returns immediately without blocking.
    ///
    /// # Errors
    ///
    /// Returns `false` when the channel is full or the pump task has exited
    /// (disconnected). The message is dropped in that case (fire-and-forget).
    pub fn try_publish(&self, request: TransportRequest) -> bool {
        self.tx.try_send(PumpJob { request }).is_ok()
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

async fn pump_loop(channel: Arc<ArcSwapOption<PumpChannel>>, rx: Receiver<PumpJob>) {
    while let Ok(job) = rx.recv_async().await {
        if let Some(ch) = channel.load_full() {
            let _ = ch.0.publish(job.request).await;
        }
    }
}
