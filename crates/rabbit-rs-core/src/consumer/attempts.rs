use std::{error::Error, fmt, num::NonZeroU32};

use super::Headers;
use crate::transport::HeaderValue;

pub const APPLICATION_ATTEMPTS_HEADER: &str = "x-rabbit-rs-attempts";

/// Default inclusive cap on resolved delivery attempts. Deliveries above the
/// cap are settled terminally by the consumer actor.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 20;

/// Const-evaluated non-zero form of [`DEFAULT_MAX_ATTEMPTS`]; the `match`
/// panics at compile time if the constant is ever set to zero.
pub(crate) const DEFAULT_MAX_ATTEMPTS_NON_ZERO: NonZeroU32 =
    match NonZeroU32::new(DEFAULT_MAX_ATTEMPTS) {
        Some(value) => value,
        None => panic!("DEFAULT_MAX_ATTEMPTS must be non-zero"),
    };

const ACQUIRED_COUNT_HEADER: &str = "x-acquired-count";
const DELIVERY_COUNT_HEADER: &str = "x-delivery-count";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttemptsResolver {
    max_attempts: Option<NonZeroU32>,
}

impl Default for AttemptsResolver {
    fn default() -> Self {
        Self {
            max_attempts: NonZeroU32::new(DEFAULT_MAX_ATTEMPTS),
        }
    }
}

impl AttemptsResolver {
    /// Overrides the inclusive attempts cap. `None` disables the cap.
    #[must_use]
    pub const fn with_max_attempts(mut self, max_attempts: Option<NonZeroU32>) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Resolves Laravel-compatible attempts from broker and application headers.
    ///
    /// `RabbitMQ` 4.3 `x-acquired-count` is already acquisition-based, while
    /// `x-delivery-count` counts failed deliveries and therefore needs one added
    /// for the delivery currently being acquired. Classic queues without either
    /// counter can only distinguish a first delivery from a redelivery.
    ///
    /// # Errors
    ///
    /// Returns [`AttemptsErrorKind::MaxAttempts`] when the resolved attempt is
    /// above the configured inclusive limit.
    pub fn resolve(&self, headers: &Headers, redelivered: bool) -> Result<u32, AttemptsError> {
        let acquired = header_count(headers, ACQUIRED_COUNT_HEADER).map(|value| value.max(1));
        let failed_deliveries =
            header_count(headers, DELIVERY_COUNT_HEADER).map(|value| value.saturating_add(1));
        let broker_attempts =
            acquired
                .or(failed_deliveries)
                .unwrap_or(if redelivered { 2 } else { 1 });
        let application_attempts = header_count(headers, APPLICATION_ATTEMPTS_HEADER).unwrap_or(1);
        let attempts = broker_attempts.max(application_attempts).max(1);

        (*self).validate(attempts)
    }

    pub(crate) fn delayed_headers(
        self,
        headers: &Headers,
        attempts: u32,
    ) -> Result<Headers, AttemptsError> {
        let next_attempt = self.validate(attempts.saturating_add(1))?;
        let mut next_headers = headers.clone();
        next_headers.remove(ACQUIRED_COUNT_HEADER);
        next_headers.remove(DELIVERY_COUNT_HEADER);
        next_headers.insert(
            APPLICATION_ATTEMPTS_HEADER.to_owned(),
            HeaderValue::Integer(i64::from(next_attempt)),
        );
        Ok(next_headers)
    }

    fn validate(self, attempts: u32) -> Result<u32, AttemptsError> {
        if self
            .max_attempts
            .is_some_and(|maximum| attempts > maximum.get())
        {
            Err(AttemptsError {
                kind: AttemptsErrorKind::MaxAttempts,
                attempts,
                max_attempts: self.max_attempts,
            })
        } else {
            Ok(attempts)
        }
    }
}

fn header_count(headers: &Headers, name: &str) -> Option<u32> {
    match headers.get(name)? {
        HeaderValue::Integer(value) => u32::try_from(*value).ok(),
        HeaderValue::Binary(value) => std::str::from_utf8(value).ok()?.parse().ok(),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptsErrorKind {
    MaxAttempts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttemptsError {
    kind: AttemptsErrorKind,
    attempts: u32,
    max_attempts: Option<NonZeroU32>,
}

impl AttemptsError {
    #[must_use]
    pub const fn kind(&self) -> AttemptsErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }

    #[must_use]
    pub const fn max_attempts(&self) -> Option<u32> {
        match self.max_attempts {
            Some(maximum) => Some(maximum.get()),
            None => None,
        }
    }
}

impl fmt::Display for AttemptsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.max_attempts {
            Some(maximum) => write!(
                formatter,
                "delivery attempt {} exceeds the configured maximum of {}",
                self.attempts, maximum
            ),
            None => formatter.write_str("delivery attempts could not be resolved"),
        }
    }
}

impl Error for AttemptsError {}
