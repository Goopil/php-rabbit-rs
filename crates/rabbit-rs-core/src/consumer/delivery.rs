use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use super::{SubscriptionId, actor::ConsumerCommand};
use crate::pool::ConnectionKey;

pub use crate::transport::Headers;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MessageId(String);

impl MessageId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DeliveryState {
    Pending = 0,
    Acked = 2,
    Rejected = 3,
    Lost = 4,
}

const TRANSITIONING: u8 = 1;

pub struct Delivery {
    pub id: MessageId,
    pub subscription: SubscriptionId,
    pub payload: Bytes,
    pub headers: Headers,
    pub attempts: u32,
    token: DeliveryToken,
}

impl fmt::Debug for Delivery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Delivery")
            .field("id", &self.id)
            .field("subscription", &self.subscription)
            .field("payload_len", &self.payload.len())
            .field("headers", &self.headers)
            .field("attempts", &self.attempts)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl Delivery {
    pub(crate) fn new(
        id: MessageId,
        subscription: SubscriptionId,
        payload: Bytes,
        headers: Headers,
        attempts: u32,
        token: DeliveryToken,
    ) -> Self {
        Self {
            id,
            subscription,
            payload,
            headers,
            attempts,
            token,
        }
    }

    #[must_use]
    pub fn state(&self) -> DeliveryState {
        self.token.state()
    }

    /// Acknowledges this delivery exactly once.
    ///
    /// # Errors
    ///
    /// Returns a typed error for stale generations, transport failures, a
    /// closed consumer, or an already terminal token.
    pub async fn ack(&self) -> Result<(), ConsumerError> {
        self.token.settle(Settlement::Ack).await
    }

    /// Releases this delivery immediately or through its delayed publisher.
    ///
    /// # Errors
    ///
    /// Returns a typed error when reject, delayed publish, confirm, or the
    /// final acknowledgement fails.
    pub async fn release(&self, delay: Duration) -> Result<(), ConsumerError> {
        self.token.settle(Settlement::Release(delay)).await
    }

    /// Rejects this delivery exactly once with the requested requeue policy.
    ///
    /// # Errors
    ///
    /// Returns a typed error for stale generations, transport failures, a
    /// closed consumer, or an already terminal token.
    pub async fn reject(&self, requeue: bool) -> Result<(), ConsumerError> {
        self.token.settle(Settlement::Reject(requeue)).await
    }
}

#[derive(Clone)]
pub(crate) struct DeliveryToken {
    inner: Arc<DeliveryTokenInner>,
}

impl DeliveryToken {
    pub(crate) fn new(inner: DeliveryTokenInner) -> Self {
        Self {
            inner: Arc::new(inner),
        }
    }

    fn state(&self) -> DeliveryState {
        match self.inner.state.load(Ordering::Acquire) {
            value if value == DeliveryState::Acked as u8 => DeliveryState::Acked,
            value if value == DeliveryState::Rejected as u8 => DeliveryState::Rejected,
            value if value == DeliveryState::Lost as u8 => DeliveryState::Lost,
            _ => DeliveryState::Pending,
        }
    }

    async fn settle(&self, settlement: Settlement) -> Result<(), ConsumerError> {
        self.inner
            .state
            .compare_exchange(
                DeliveryState::Pending as u8,
                TRANSITIONING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| ConsumerError::already_settled())?;
        let (completed, completion) = oneshot::channel();
        if self
            .inner
            .commands
            .send(ConsumerCommand::Settle {
                token: self.inner.clone(),
                settlement,
                completed,
            })
            .await
            .is_err()
        {
            self.inner
                .state
                .store(DeliveryState::Lost as u8, Ordering::Release);
            return Err(ConsumerError::closed());
        }

        match completion.await {
            Ok(Ok(terminal)) => {
                self.inner.state.store(terminal as u8, Ordering::Release);
                Ok(())
            }
            Ok(Err(error)) if error.kind == ConsumerErrorKind::StaleGeneration => {
                self.inner
                    .state
                    .store(DeliveryState::Lost as u8, Ordering::Release);
                Err(error)
            }
            Ok(Err(error)) => {
                self.inner
                    .state
                    .store(DeliveryState::Pending as u8, Ordering::Release);
                Err(error)
            }
            Err(_) => {
                self.inner
                    .state
                    .store(DeliveryState::Lost as u8, Ordering::Release);
                Err(ConsumerError::closed())
            }
        }
    }
}

pub(crate) struct DeliveryTokenInner {
    pub subscription: SubscriptionId,
    pub connection_key: ConnectionKey,
    pub generation: u64,
    pub channel_id: u16,
    pub delivery_tag: u64,
    pub message_id: MessageId,
    pub payload: Bytes,
    pub headers: Headers,
    pub attempts: u32,
    pub reserved_at: Instant,
    pub commands: mpsc::Sender<ConsumerCommand>,
    state: AtomicU8,
}

pub(crate) struct DeliveryIdentity {
    pub subscription: SubscriptionId,
    pub connection_key: ConnectionKey,
    pub generation: u64,
    pub channel_id: u16,
    pub delivery_tag: u64,
}

impl DeliveryTokenInner {
    pub(crate) fn pending(
        identity: DeliveryIdentity,
        message_id: MessageId,
        payload: Bytes,
        headers: Headers,
        attempts: u32,
        commands: mpsc::Sender<ConsumerCommand>,
    ) -> Self {
        Self {
            subscription: identity.subscription,
            connection_key: identity.connection_key,
            generation: identity.generation,
            channel_id: identity.channel_id,
            delivery_tag: identity.delivery_tag,
            message_id,
            payload,
            headers,
            attempts,
            reserved_at: Instant::now(),
            commands,
            state: AtomicU8::new(DeliveryState::Pending as u8),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Settlement {
    Ack,
    Release(Duration),
    Reject(bool),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsumerErrorKind {
    Closed,
    StaleGeneration,
    AlreadySettled,
    Transport,
    Publish,
    MissingPublisher,
    InvalidSubscription,
    MaxAttempts,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsumerError {
    kind: ConsumerErrorKind,
    message: String,
}

impl ConsumerError {
    pub(crate) fn new(kind: ConsumerErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn closed() -> Self {
        Self::new(ConsumerErrorKind::Closed, "consumer set is closed")
    }

    pub(crate) fn already_settled() -> Self {
        Self::new(
            ConsumerErrorKind::AlreadySettled,
            "delivery token is already terminal or transitioning",
        )
    }

    #[must_use]
    pub const fn kind(&self) -> ConsumerErrorKind {
        self.kind
    }
}

impl fmt::Display for ConsumerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ConsumerError {}
