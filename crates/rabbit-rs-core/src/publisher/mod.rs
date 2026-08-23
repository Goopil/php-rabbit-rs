pub mod actor;
pub mod confirms;
pub mod delay;

use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use tokio::{sync::oneshot, time::Instant};

use crate::transport::{PublishHeaders, PublisherChannel, TransportError};

pub use actor::{PublisherActor, PublisherHandle};

/// Shared byte budget enforcing a cap on total buffered publisher bytes.
///
/// Uses an atomic counter so that `try_publish` can reserve bytes before
/// acquiring the semaphore permit. Bytes are released when the
/// `RetainedPublish` reaches a terminal outcome (confirmed, returned,
/// terminal error, or drained to replay — at which point replay bytes are
/// counted separately by the metrics layer).
#[derive(Debug)]
pub struct ByteBudget {
    current: AtomicU64,
    max: u64,
}

impl ByteBudget {
    #[must_use]
    pub const fn new(max: u64) -> Self {
        Self {
            current: AtomicU64::new(0),
            max,
        }
    }

    /// Tries to reserve `bytes` from the budget. Returns `true` on success.
    ///
    /// On failure the budget is left unchanged and the caller must not
    /// release any bytes.
    pub fn try_reserve(&self, bytes: u64) -> bool {
        loop {
            let current = self.current.load(Ordering::Relaxed);
            let new = current.saturating_add(bytes);
            if new > self.max {
                return false;
            }
            if self
                .current
                .compare_exchange(current, new, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Releases `bytes` back to the budget.
    pub fn release(&self, bytes: u64) {
        loop {
            let current = self.current.load(Ordering::Relaxed);
            let new = current.saturating_sub(bytes);
            if self
                .current
                .compare_exchange(current, new, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
    }

    /// Returns the current number of reserved bytes.
    #[must_use]
    pub fn current(&self) -> u64 {
        self.current.load(Ordering::Relaxed)
    }

    /// Returns the configured maximum.
    #[must_use]
    pub const fn max(&self) -> u64 {
        self.max
    }
}

pub enum PublisherConnectionEvent {
    Recovering {
        generation: u64,
    },
    Ready {
        generation: u64,
        channel: Arc<dyn PublisherChannel>,
        topology_restored: bool,
    },
    FailedPermanent {
        generation: u64,
        error: TransportError,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Destination {
    pub exchange: Arc<str>,
    pub routing_key: Arc<str>,
}

impl Destination {
    #[must_use]
    pub fn new(exchange: impl Into<String>, routing_key: impl Into<String>) -> Self {
        Self {
            exchange: Arc::from(exchange.into()),
            routing_key: Arc::from(routing_key.into()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageProperties {
    pub message_id: Arc<str>,
    pub content_type: Option<Arc<str>>,
    pub correlation_id: Option<Arc<str>>,
    pub delay_ms: Option<u64>,
    pub headers: PublishHeaders,
}

impl MessageProperties {
    #[must_use]
    pub fn new(message_id: impl Into<String>) -> Self {
        Self {
            message_id: Arc::from(message_id.into()),
            content_type: None,
            correlation_id: None,
            delay_ms: None,
            headers: PublishHeaders::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PublishRequest {
    pub destination: Destination,
    pub payload: Bytes,
    pub properties: MessageProperties,
    pub deadline: Instant,
}

impl PublishRequest {
    #[must_use]
    pub const fn new(
        destination: Destination,
        payload: Bytes,
        properties: MessageProperties,
        deadline: Instant,
    ) -> Self {
        Self {
            destination,
            payload,
            properties,
            deadline,
        }
    }

    #[must_use]
    pub fn republish(&self, deadline: Instant) -> Self {
        Self {
            destination: self.destination.clone(),
            payload: self.payload.clone(),
            properties: self.properties.clone(),
            deadline,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublisherConfig {
    pub buffer_capacity: usize,
    pub confirm_timeout: Duration,
    pub confirms: bool,
    pub mandatory: bool,
    pub max_buffered_bytes: u64,
}

impl PublisherConfig {
    #[must_use]
    pub const fn new(buffer_capacity: usize, confirm_timeout: Duration) -> Self {
        Self {
            buffer_capacity,
            confirm_timeout,
            confirms: true,
            mandatory: true,
            max_buffered_bytes: 64 * 1024 * 1024,
        }
    }

    #[must_use]
    pub const fn with_flags(
        buffer_capacity: usize,
        confirm_timeout: Duration,
        confirms: bool,
        mandatory: bool,
    ) -> Self {
        Self {
            buffer_capacity,
            confirm_timeout,
            confirms,
            mandatory,
            max_buffered_bytes: 64 * 1024 * 1024,
        }
    }

    #[must_use]
    pub const fn with_byte_budget(mut self, max_buffered_bytes: u64) -> Self {
        self.max_buffered_bytes = max_buffered_bytes;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReturnInfo {
    pub code: u16,
    pub text: String,
    pub exchange: String,
    pub routing_key: String,
}

/// A per-message indexed report produced by [`ClientPool::publish_batch_detailed`].
///
/// Each variant corresponds to the terminal resolution of one publish in the
/// batch, preserving the input order. `NotAccepted` covers publications that
/// were never accepted by an actor (e.g., the pool closed mid-batch).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MessageOutcome {
    /// The broker confirmed the message.
    Confirmed(PublishOutcome),
    /// The broker returned the message (mandatory routing failure).
    Returned(ReturnInfo),
    /// The publish failed with a typed error.
    Failed(PublishError),
    /// The publish was never accepted by an actor.
    NotAccepted(PublishError),
}

/// A full per-message indexed report for a batch publish.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchOutcome {
    /// One entry per input request, in input order.
    pub results: Vec<MessageOutcome>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishOutcome {
    Confirmed {
        message_id: String,
    },
    Returned {
        message_id: String,
        reply: ReturnInfo,
    },
    Ambiguous {
        message_id: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishErrorKind {
    Backpressure,
    Nack,
    Timeout,
    Unconfirmed,
    Transport,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishError {
    kind: PublishErrorKind,
    message: String,
}

impl PublishError {
    pub(crate) fn new(kind: PublishErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> PublishErrorKind {
        self.kind
    }
}

impl fmt::Display for PublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for PublishError {}

#[derive(Debug)]
pub struct PublishWaiter {
    receiver: oneshot::Receiver<Result<PublishOutcome, PublishError>>,
}

impl PublishWaiter {
    pub(crate) const fn new(
        receiver: oneshot::Receiver<Result<PublishOutcome, PublishError>>,
    ) -> Self {
        Self { receiver }
    }

    /// Waits for the safe terminal outcome of one publish.
    ///
    /// # Errors
    ///
    /// Returns a typed publish failure or [`PublishErrorKind::Closed`] if the
    /// actor exits without resolving the command.
    pub async fn wait(self) -> Result<PublishOutcome, PublishError> {
        self.receiver.await.unwrap_or_else(|_| {
            Err(PublishError::new(
                PublishErrorKind::Closed,
                "publisher actor closed before resolving the command",
            ))
        })
    }
}
