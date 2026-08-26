use std::{error::Error, fmt, sync::Arc, time::Duration};

use crate::{publisher::Destination, topology::delay::DelayStrategy, transport::QueueSpec};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelayedRoute {
    pub exchange: Arc<str>,
    pub routing_key: Arc<str>,
    pub delay_ms: u64,
    pub queue: Option<QueueSpec>,
}

pub struct DelayRouter;

impl DelayRouter {
    /// Routes a non-negative delay through the selected backend.
    ///
    /// # Errors
    ///
    /// Returns an error for a negative delay or an exhausted TTL bucket plan.
    pub fn route(
        strategy: &DelayStrategy,
        destination: &Destination,
        delay_ms: i64,
    ) -> Result<DelayedRoute, DelayRoutingError> {
        let delay_ms = u64::try_from(delay_ms)
            .map_err(|_| DelayRoutingError::new("delay cannot be negative"))?;
        let delay = Duration::from_millis(delay_ms);

        match strategy {
            DelayStrategy::Plugin => Ok(DelayedRoute {
                exchange: Arc::from(delayed_exchange_name(&destination.exchange)),
                routing_key: destination.routing_key.clone(),
                delay_ms,
                queue: None,
            }),
            DelayStrategy::TtlBuckets(plan) => {
                let queue = plan
                    .queue_for(destination, delay)
                    .map_err(|error| DelayRoutingError::new(error.to_string()))?;
                Ok(DelayedRoute {
                    exchange: Arc::from(""),
                    routing_key: Arc::from(queue.name.as_str()),
                    delay_ms,
                    queue: Some(queue),
                })
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelayRoutingError {
    message: String,
}

impl DelayRoutingError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for DelayRoutingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for DelayRoutingError {}

fn delayed_exchange_name(exchange: &str) -> String {
    if exchange.is_empty() {
        "rabbit-rs.delayed".to_owned()
    } else {
        format!("{exchange}.delayed")
    }
}
