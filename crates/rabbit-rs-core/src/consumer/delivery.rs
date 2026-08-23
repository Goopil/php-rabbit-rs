use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use tokio::sync::mpsc;

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
    /// Best-effort: the delivery was auto-acked before dispatch.
    /// Settlement calls return [`ConsumerErrorKind::AlreadySettled`].
    AutoAcked = 5,
}

const TRANSITIONING: u8 = 1;

pub struct Delivery {
    pub id: MessageId,
    pub correlation_id: Option<String>,
    pub subscription: SubscriptionId,
    pub payload: Bytes,
    pub headers: Arc<Headers>,
    pub attempts: u32,
    token: DeliveryToken,
}

impl fmt::Debug for Delivery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Delivery")
            .field("id", &self.id)
            .field("correlation_id", &self.correlation_id)
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
        correlation_id: Option<String>,
        subscription: SubscriptionId,
        payload: Bytes,
        headers: Arc<Headers>,
        attempts: u32,
        token: DeliveryToken,
    ) -> Self {
        Self {
            id,
            correlation_id,
            subscription,
            payload,
            headers,
            attempts,
            token,
        }
    }

    /// Creates a delivery that was already auto-acked to the broker.
    ///
    /// The token is in a terminal [`DeliveryState::AutoAcked`] state; any
    /// settlement call returns [`ConsumerErrorKind::AlreadySettled`].
    #[must_use]
    pub(crate) fn new_auto_acked(
        identity: DeliveryIdentity,
        id: MessageId,
        correlation_id: Option<String>,
        payload: Bytes,
        headers: Arc<Headers>,
        attempts: u32,
    ) -> Self {
        let token = DeliveryToken::new(DeliveryTokenInner::auto_acked(
            identity.clone(),
            id.clone(),
            correlation_id.clone(),
            payload.clone(),
            headers.clone(),
            attempts,
        ));
        Self::new(
            id,
            correlation_id,
            identity.subscription,
            payload,
            headers,
            attempts,
            token,
        )
    }

    #[must_use]
    pub fn state(&self) -> DeliveryState {
        self.token.state()
    }

    /// Returns the AMQP delivery tag for this delivery.
    #[must_use]
    pub fn delivery_tag(&self) -> u64 {
        self.token.inner.delivery_tag
    }

    /// Returns the inner token for batch settlement operations.
    #[must_use]
    pub fn inner_token(&self) -> &Arc<DeliveryTokenInner> {
        self.token.inner()
    }

    /// Acknowledges this delivery exactly once.
    ///
    /// Fire-and-forget: enqueues the settlement command and returns
    /// immediately. The final state is updated asynchronously by the actor.
    /// Settlement errors surface via [`ConsumerHandle::drain_errors`].
    ///
    /// # Errors
    ///
    /// Returns a typed error if the delivery was already settled, the
    /// command channel is full, or the consumer is closed.
    #[allow(clippy::unused_async)]
    pub async fn ack(&self) -> Result<(), ConsumerError> {
        self.token
            .try_settle(Settlement::Ack)
            .map_err(map_settle_error)
    }

    /// Releases this delivery immediately or through its delayed publisher.
    ///
    /// Fire-and-forget: enqueues the settlement command and returns
    /// immediately. The final state is updated asynchronously by the actor.
    /// Settlement errors surface via [`ConsumerHandle::drain_errors`].
    ///
    /// # Errors
    ///
    /// Returns a typed error when the delivery was already settled, the
    /// command channel is full, or the consumer is closed.
    #[allow(clippy::unused_async)]
    pub async fn release(&self, delay: Duration) -> Result<(), ConsumerError> {
        self.token
            .try_settle(Settlement::Release(delay))
            .map_err(map_settle_error)
    }

    /// Rejects this delivery exactly once with the requested requeue policy.
    ///
    /// Fire-and-forget: enqueues the settlement command and returns
    /// immediately. The final state is updated asynchronously by the actor.
    /// Settlement errors surface via [`ConsumerHandle::drain_errors`].
    ///
    /// # Errors
    ///
    /// Returns a typed error for already settled deliveries, a full command
    /// channel, or a closed consumer.
    #[allow(clippy::unused_async)]
    pub async fn reject(&self, requeue: bool) -> Result<(), ConsumerError> {
        self.token
            .try_settle(Settlement::Reject(requeue))
            .map_err(map_settle_error)
    }

    /// Synchronous fire-and-forget acknowledgement.
    ///
    /// Same as [`Self::ack`] but without the async wrapper, suitable for
    /// synchronous FFI callers (e.g. PHP extension) that cannot drive a
    /// future without a runtime. Returns [`SettlementErrorKind`] so the
    /// caller can distinguish backpressure (`ChannelFull`) from terminal
    /// errors and retry accordingly.
    ///
    /// # Errors
    ///
    /// Returns [`SettlementErrorKind::AlreadySettled`], [`SettlementErrorKind::ChannelFull`],
    /// or [`SettlementErrorKind::Closed`].
    pub fn try_ack(&self) -> Result<(), SettlementErrorKind> {
        self.token.try_settle(Settlement::Ack)
    }

    /// Synchronous fire-and-forget release.
    ///
    /// See [`Self::try_ack`] for semantics.
    ///
    /// # Errors
    ///
    /// Returns [`SettlementErrorKind::AlreadySettled`], [`SettlementErrorKind::ChannelFull`],
    /// or [`SettlementErrorKind::Closed`].
    pub fn try_release(&self, delay: Duration) -> Result<(), SettlementErrorKind> {
        self.token.try_settle(Settlement::Release(delay))
    }

    /// Synchronous fire-and-forget reject.
    ///
    /// See [`Self::try_ack`] for semantics.
    ///
    /// # Errors
    ///
    /// Returns [`SettlementErrorKind::AlreadySettled`], [`SettlementErrorKind::ChannelFull`],
    /// or [`SettlementErrorKind::Closed`].
    pub fn try_reject(&self, requeue: bool) -> Result<(), SettlementErrorKind> {
        self.token.try_settle(Settlement::Reject(requeue))
    }
}

/// Maps [`SettlementErrorKind`] to a [`ConsumerError`] for the public API.
fn map_settle_error(kind: SettlementErrorKind) -> ConsumerError {
    match kind {
        SettlementErrorKind::AlreadySettled => ConsumerError::already_settled(),
        SettlementErrorKind::ChannelFull => ConsumerError::new(
            ConsumerErrorKind::SettlementInProgress,
            "settlement command channel is full",
        ),
        SettlementErrorKind::Closed => ConsumerError::closed(),
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

    pub(crate) fn inner(&self) -> &Arc<DeliveryTokenInner> {
        &self.inner
    }

    fn state(&self) -> DeliveryState {
        match self.inner.state.load(Ordering::Acquire) {
            value if value == DeliveryState::Acked as u8 => DeliveryState::Acked,
            value if value == DeliveryState::Rejected as u8 => DeliveryState::Rejected,
            value if value == DeliveryState::Lost as u8 => DeliveryState::Lost,
            value if value == DeliveryState::AutoAcked as u8 => DeliveryState::AutoAcked,
            _ => DeliveryState::Pending,
        }
    }

    /// Fire-and-forget settlement.
    ///
    /// Performs the `Pending → Transitioning` CAS, then enqueues the
    /// settlement command via `try_send`. Returns immediately without
    /// waiting for the actor to process the settlement.
    ///
    /// # Errors
    ///
    /// Returns [`SettlementErrorKind::AlreadySettled`] if the CAS fails,
    /// [`SettlementErrorKind::ChannelFull`] if the command channel is full
    /// (state reverts to `Pending` so the caller can retry), or
    /// [`SettlementErrorKind::Closed`] if the command channel is closed
    /// (state becomes `Lost`).
    pub(crate) fn try_settle(&self, settlement: Settlement) -> Result<(), SettlementErrorKind> {
        self.inner
            .state
            .compare_exchange(
                DeliveryState::Pending as u8,
                TRANSITIONING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| SettlementErrorKind::AlreadySettled)?;

        match self.inner.commands.try_send(ConsumerCommand::Settle {
            token: self.inner.clone(),
            settlement,
        }) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.inner
                    .state
                    .store(DeliveryState::Pending as u8, Ordering::Release);
                Err(SettlementErrorKind::ChannelFull)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.inner
                    .state
                    .store(DeliveryState::Lost as u8, Ordering::Release);
                Err(SettlementErrorKind::Closed)
            }
        }
    }
}

pub struct DeliveryTokenInner {
    pub(crate) subscription: SubscriptionId,
    pub(crate) connection_key: ConnectionKey,
    pub(crate) generation: u64,
    pub(crate) channel_id: u16,
    pub(crate) delivery_tag: u64,
    pub(crate) message_id: MessageId,
    pub(crate) correlation_id: Option<String>,
    pub(crate) payload: Bytes,
    pub(crate) headers: Arc<Headers>,
    pub(crate) attempts: u32,
    pub(crate) reserved_at: Instant,
    pub(crate) commands: mpsc::Sender<ConsumerCommand>,
    pub(crate) state: AtomicU8,
    pub(crate) settling: AtomicBool,
}

#[derive(Clone)]
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
        correlation_id: Option<String>,
        payload: Bytes,
        headers: Arc<Headers>,
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
            correlation_id,
            payload,
            headers,
            attempts,
            reserved_at: Instant::now(),
            commands,
            state: AtomicU8::new(DeliveryState::Pending as u8),
            settling: AtomicBool::new(false),
        }
    }

    /// Creates a terminal token for an auto-acked delivery.
    ///
    /// The token starts in [`DeliveryState::AutoAcked`]; any settlement
    /// attempt returns [`ConsumerErrorKind::AlreadySettled`] because the
    /// `Pending → Transitioning` compare-exchange in `settle` fails.
    pub(crate) fn auto_acked(
        identity: DeliveryIdentity,
        message_id: MessageId,
        correlation_id: Option<String>,
        payload: Bytes,
        headers: Arc<Headers>,
        attempts: u32,
    ) -> Self {
        Self {
            subscription: identity.subscription,
            connection_key: identity.connection_key,
            generation: identity.generation,
            channel_id: identity.channel_id,
            delivery_tag: identity.delivery_tag,
            message_id,
            correlation_id,
            payload,
            headers,
            attempts,
            reserved_at: Instant::now(),
            commands: mpsc::channel(1).0,
            state: AtomicU8::new(DeliveryState::AutoAcked as u8),
            settling: AtomicBool::new(false),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Settlement {
    Ack,
    Release(Duration),
    Reject(bool),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsumerErrorKind {
    Closed,
    StaleGeneration,
    AlreadySettled,
    AlreadySettling,
    SettlementInProgress,
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

    #[allow(dead_code)]
    pub(crate) fn already_settling() -> Self {
        Self::new(
            ConsumerErrorKind::AlreadySettling,
            "delivery is already being settled",
        )
    }

    #[allow(dead_code)]
    pub(crate) fn settlement_in_progress() -> Self {
        Self::new(
            ConsumerErrorKind::SettlementInProgress,
            "a settlement is already in progress on this channel",
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

/// Error returned by `try_settle` when the fire-and-forget send fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettleError {
    /// The actor's command channel is full (256 capacity).
    ChannelFull,
    /// The actor's command channel is closed.
    Closed,
}

/// Classification of a fire-and-forget settlement failure at the token level.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettlementErrorKind {
    /// The delivery was already settled (CAS failed).
    AlreadySettled,
    /// The actor's command channel is full.
    ChannelFull,
    /// The actor's command channel is closed.
    Closed,
}

/// Error recorded by the actor when a settlement fails asynchronously.
#[derive(Clone, Debug)]
pub struct SettlementError {
    /// The AMQP delivery tag that failed to settle.
    pub delivery_tag: u64,
    /// The subscription that owns the delivery.
    pub subscription: SubscriptionId,
    /// The kind of consumer error that caused the settlement failure.
    pub kind: ConsumerErrorKind,
    /// Human-readable error message.
    pub message: String,
    /// When the error was recorded.
    pub timestamp: Instant,
}
