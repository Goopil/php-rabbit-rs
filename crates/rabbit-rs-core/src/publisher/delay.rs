use std::{collections::HashSet, error::Error, fmt, sync::Arc, time::Duration};

use super::{PublishError, PublishErrorKind};
use crate::{
    publisher::Destination,
    topology::delay::{DelayStrategy, delayed_exchange_name},
    transport::{PublisherChannel, QueueSpec},
};

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

/// Routes a publication through the delay strategy before it reaches the wire.
///
/// A delay of zero (or absent) leaves the original destination untouched. A
/// positive delay with no strategy, or a delay the strategy cannot route, is
/// an error — the previous silent fallback published such messages to the
/// original exchange with an `x-delay` header a normal exchange ignores,
/// which executes the job immediately.
///
/// # Errors
///
/// Returns [`PublishErrorKind::InvalidRequest`] for a delay the compiled
/// strategy cannot route (e.g. beyond the largest TTL bucket).
pub(crate) fn route_transport_request(
    request: &crate::publisher::PublishRequest,
    strategy: Option<&DelayStrategy>,
    mandatory: bool,
) -> Result<crate::transport::PublishRequest, PublishError> {
    let requested_delay = request.properties.delay_ms.unwrap_or(0);
    let routed = if requested_delay == 0 {
        None
    } else {
        let strategy = strategy.ok_or_else(|| {
            PublishError::new(
                PublishErrorKind::InvalidRequest,
                "delay_ms is set but no delay strategy is configured",
            )
        })?;
        let route = DelayRouter::route(
            strategy,
            &request.destination,
            i64::try_from(requested_delay).unwrap_or(i64::MAX),
        )
        .map_err(|error| PublishError::new(PublishErrorKind::InvalidRequest, error.to_string()))?;
        Some((
            route.exchange,
            route.routing_key,
            route.queue.is_none().then_some(route.delay_ms),
        ))
    };

    let (exchange, routing_key, delay_ms, mandatory) = match routed {
        // The delayed-message plugin defers routing and cannot honour the
        // mandatory flag: every mandatory publish carrying an `x-delay` header
        // comes back as unroutable. Delayed publishes keep publisher confirms
        // — the documented confirms-without-mandatory case (issue #97).
        Some((exchange, routing_key, delay_ms)) => (
            exchange,
            routing_key,
            delay_ms,
            mandatory && delay_ms.is_none(),
        ),
        None => (
            request.destination.exchange.clone(),
            request.destination.routing_key.clone(),
            request.properties.delay_ms,
            mandatory,
        ),
    };

    Ok(crate::transport::PublishRequest {
        exchange,
        routing_key,
        payload: request.payload.clone(),
        mandatory,
        properties: crate::transport::PublishProperties {
            content_type: request
                .properties
                .content_type
                .as_ref()
                .map(|ct| ct.as_ref().to_owned()),
            correlation_id: request
                .properties
                .correlation_id
                .as_ref()
                .map(|ci| ci.as_ref().to_owned()),
            message_id: Some(request.properties.message_id.as_ref().to_owned()),
            delay_ms,
            headers: request.properties.headers.clone(),
            persistent: true,
        },
    })
}

/// Declares the delay infrastructure the compiled strategy routes a delay
/// through: the `x-delayed-message` exchange for plugin mode, or the
/// synthesized TTL delay queue for TTL mode. Idempotent per process through
/// the caller-owned `declared` cache.
///
/// # Errors
///
/// Returns a typed publish error when the routing fails or the broker refuses
/// the declaration (e.g. the delayed-message plugin is absent in plugin mode).
pub(crate) async fn ensure_delay_topology(
    channel: &Arc<dyn PublisherChannel>,
    strategy: &DelayStrategy,
    destination: &Destination,
    delay_ms: u64,
    declared: &mut HashSet<Arc<str>>,
) -> Result<(), PublishError> {
    let route = DelayRouter::route(
        strategy,
        destination,
        i64::try_from(delay_ms).unwrap_or(i64::MAX),
    )
    .map_err(|error| PublishError::new(PublishErrorKind::InvalidRequest, error.to_string()))?;

    if route.queue.is_none() && !declared.contains(&route.exchange) {
        let spec = crate::topology::delay::delayed_exchange_spec(&route.exchange);
        match channel.declare_exchange(&spec).await {
            Ok(()) => {
                declared.insert(route.exchange.clone());
            }
            // A topology failure for this message (e.g. 540 when the
            // delayed-message plugin is absent in `auto` mode) is never a
            // reason to suspend the publisher: fail the single message
            // terminally so the actor stays ready.
            Err(error) => {
                return Err(PublishError::new(
                    PublishErrorKind::Transport,
                    error.to_string(),
                ));
            }
        }
    }

    if let Some(queue_spec) = &route.queue
        && !declared.contains(queue_spec.name.as_str())
    {
        match channel.declare_queue(queue_spec).await {
            Ok(()) => {
                declared.insert(Arc::from(queue_spec.name.as_str()));
            }
            // Same contract as the delayed-exchange declare above.
            Err(error) => {
                return Err(PublishError::new(
                    PublishErrorKind::Transport,
                    error.to_string(),
                ));
            }
        }
    }

    Ok(())
}
