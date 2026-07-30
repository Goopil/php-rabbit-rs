use std::{error::Error, fmt, num::NonZeroU32};

use super::Headers;

pub const APPLICATION_ATTEMPTS_HEADER: &str = "x-rabbit-rs-attempts";

const ACQUIRED_COUNT_HEADER: &str = "x-acquired-count";
const DELIVERY_COUNT_HEADER: &str = "x-delivery-count";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AttemptsResolver {
    max_attempts: Option<NonZeroU32>,
}

impl AttemptsResolver {
    #[must_use]
    pub const fn new(max_attempts: Option<NonZeroU32>) -> Self {
        Self { max_attempts }
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
            next_attempt.to_string().into(),
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
    std::str::from_utf8(headers.get(name)?).ok()?.parse().ok()
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
