use std::sync::Arc;

use flume::{Receiver, Sender};

use crate::transport::{PublishRequest as TransportRequest, PublisherChannel};

/// A background pump that drains a flume channel and publishes messages
/// without waiting for confirmations (blind / fire-and-forget mode).
///
/// The pump owns a bounded `flume` channel. Producers call [`try_publish`](Self::try_publish)
/// which enqueues into the channel and returns immediately. A background
/// tokio task drains the channel and publishes each message to the transport
/// channel, discarding the confirmation receipt.
pub struct PublishPump {
    tx: Sender<PumpJob>,
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
    /// # Panics
    ///
    /// Never panics. The pump task exits cleanly when the sender is dropped.
    #[must_use]
    pub fn spawn(channel: Arc<dyn PublisherChannel>, buffer_capacity: usize) -> Self {
        let (tx, rx) = flume::bounded(buffer_capacity.max(1));
        tokio::spawn(pump_loop(channel, rx));
        Self { tx }
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

async fn pump_loop(channel: Arc<dyn PublisherChannel>, rx: Receiver<PumpJob>) {
    while let Ok(job) = rx.recv_async().await {
        let _ = channel.publish(job.request).await;
    }
}
