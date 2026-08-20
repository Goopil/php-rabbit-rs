pub mod actor;
pub mod batcher;
pub mod confirms;
pub mod delay;

use std::{error::Error, fmt, sync::Arc, time::Duration};

use bytes::Bytes;
use tokio::{
    sync::{OwnedSemaphorePermit, oneshot},
    time::Instant,
};

use crate::topology::delay::DelayStrategy;
use crate::transport::{
    PublishConfirmation, PublishHeaders, PublishProperties as TransportProperties,
    PublishRequest as TransportRequest, PublisherChannel, TransportError,
};

pub use actor::{PublisherActor, PublisherHandle};

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
    pub fn new(exchange: impl Into<Arc<str>>, routing_key: impl Into<Arc<str>>) -> Self {
        Self {
            exchange: exchange.into(),
            routing_key: routing_key.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageProperties {
    pub message_id: Arc<str>,
    pub content_type: Option<String>,
    pub correlation_id: Option<String>,
    pub delay_ms: Option<u64>,
    pub headers: PublishHeaders,
}

impl MessageProperties {
    #[must_use]
    pub fn new(message_id: impl Into<Arc<str>>) -> Self {
        Self {
            message_id: message_id.into(),
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
    pub max_messages: usize,
    pub max_bytes: usize,
    pub flush_interval: Duration,
    pub buffer_capacity: usize,
    pub confirm_timeout: Duration,
    pub confirms: bool,
    pub mandatory: bool,
}

impl PublisherConfig {
    #[must_use]
    pub const fn new(
        max_messages: usize,
        max_bytes: usize,
        flush_interval: Duration,
        buffer_capacity: usize,
        confirm_timeout: Duration,
    ) -> Self {
        Self {
            max_messages,
            max_bytes,
            flush_interval,
            buffer_capacity,
            confirm_timeout,
            confirms: true,
            mandatory: true,
        }
    }

    #[must_use]
    pub const fn with_flags(
        max_messages: usize,
        max_bytes: usize,
        flush_interval: Duration,
        buffer_capacity: usize,
        confirm_timeout: Duration,
        confirms: bool,
        mandatory: bool,
    ) -> Self {
        Self {
            max_messages,
            max_bytes,
            flush_interval,
            buffer_capacity,
            confirm_timeout,
            confirms,
            mandatory,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReturnInfo {
    pub code: u16,
    pub text: String,
    pub exchange: String,
    pub routing_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishOutcome {
    Confirmed {
        message_id: Arc<str>,
    },
    Returned {
        message_id: Arc<str>,
        reply: ReturnInfo,
    },
    Ambiguous {
        message_id: Arc<str>,
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
    source: WaitSource,
    /// Hot-path permit; held until the confirmation resolves so that
    /// `available_permits` reflects in-flight hot publishes.
    _permit: Option<OwnedSemaphorePermit>,
}

enum WaitSource {
    /// Cold path: the actor resolves the outcome via a oneshot channel.
    Channel(oneshot::Receiver<Result<PublishOutcome, PublishError>>),
    /// Hot path with immediate confirmation: the outcome is already resolved.
    Resolved(Result<PublishOutcome, PublishError>),
}

impl fmt::Debug for WaitSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Channel(_) => f.debug_tuple("Channel").finish(),
            Self::Resolved(r) => f.debug_tuple("Resolved").field(r).finish(),
        }
    }
}

impl PublishWaiter {
    pub(crate) fn new(receiver: oneshot::Receiver<Result<PublishOutcome, PublishError>>) -> Self {
        Self {
            source: WaitSource::Channel(receiver),
            _permit: None,
        }
    }

    pub(crate) fn resolved(outcome: PublishOutcome) -> Self {
        Self {
            source: WaitSource::Resolved(Ok(outcome)),
            _permit: None,
        }
    }

    /// Waits for the safe terminal outcome of one publish.
    ///
    /// # Errors
    ///
    /// Returns a typed publish failure or [`PublishErrorKind::Closed`] if the
    /// actor exits without resolving the command.
    pub async fn wait(self) -> Result<PublishOutcome, PublishError> {
        match self.source {
            WaitSource::Channel(receiver) => receiver.await.unwrap_or_else(|_| {
                Err(PublishError::new(
                    PublishErrorKind::Closed,
                    "publisher actor closed before resolving the command",
                ))
            }),
            WaitSource::Resolved(result) => result,
        }
    }
}

pub(crate) fn confirmation_to_outcome(
    confirmation: PublishConfirmation,
    message_id: Arc<str>,
) -> Result<PublishOutcome, PublishError> {
    match confirmation {
        PublishConfirmation::Ack(Some(returned)) | PublishConfirmation::Nack(Some(returned)) => {
            Ok(PublishOutcome::Returned {
                message_id,
                reply: ReturnInfo {
                    code: returned.reply_code,
                    text: returned.reply_text,
                    exchange: returned.exchange,
                    routing_key: returned.routing_key,
                },
            })
        }
        PublishConfirmation::Ack(None) => Ok(PublishOutcome::Confirmed { message_id }),
        PublishConfirmation::Nack(None) => Err(PublishError::new(
            PublishErrorKind::Nack,
            "broker negatively acknowledged the message",
        )),
        PublishConfirmation::NotRequested => Err(PublishError::new(
            PublishErrorKind::Unconfirmed,
            "publisher confirms were not enabled",
        )),
    }
}

/// Converts a publisher-level request into the transport-level request,
/// applying the delay strategy and mandatory flag.
pub(crate) fn into_transport_request(
    request: &PublishRequest,
    delay_strategy: Option<&DelayStrategy>,
    mandatory: bool,
) -> TransportRequest {
    let delay_ms = request.properties.delay_ms.unwrap_or(0);

    if delay_ms > 0
        && let Some(strategy) = delay_strategy
        && let Ok(route) = delay::DelayRouter::route(
            strategy,
            &request.destination,
            i64::try_from(delay_ms).unwrap_or(i64::MAX),
        )
    {
        let properties = TransportProperties {
            content_type: request.properties.content_type.clone(),
            correlation_id: request.properties.correlation_id.clone(),
            message_id: Some(request.properties.message_id.as_ref().to_owned()),
            delay_ms: route.queue.is_none().then_some(route.delay_ms),
            headers: request.properties.headers.clone(),
            persistent: true,
        };

        return TransportRequest {
            exchange: route.exchange,
            routing_key: route.routing_key,
            payload: request.payload.clone(),
            mandatory,
            properties,
        };
    }

    TransportRequest {
        exchange: request.destination.exchange.clone(),
        routing_key: request.destination.routing_key.clone(),
        payload: request.payload.clone(),
        mandatory,
        properties: TransportProperties {
            content_type: request.properties.content_type.clone(),
            correlation_id: request.properties.correlation_id.clone(),
            message_id: Some(request.properties.message_id.as_ref().to_owned()),
            delay_ms: request.properties.delay_ms,
            headers: request.properties.headers.clone(),
            persistent: true,
        },
    }
}
