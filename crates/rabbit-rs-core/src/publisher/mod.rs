pub mod actor;
pub mod batcher;
pub mod confirms;
pub mod delay;

use std::{error::Error, fmt, time::Duration};

use bytes::Bytes;
use tokio::{sync::oneshot, time::Instant};

pub use actor::{PublisherActor, PublisherHandle};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Destination {
    pub exchange: String,
    pub routing_key: String,
}

impl Destination {
    #[must_use]
    pub fn new(exchange: impl Into<String>, routing_key: impl Into<String>) -> Self {
        Self {
            exchange: exchange.into(),
            routing_key: routing_key.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageProperties {
    pub message_id: String,
    pub content_type: Option<String>,
    pub correlation_id: Option<String>,
}

impl MessageProperties {
    #[must_use]
    pub fn new(message_id: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            content_type: None,
            correlation_id: None,
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
