use std::{error::Error, fmt};

use crate::{
    config::TopologyMode,
    transport::{TopologyChannel, TransportError},
};

use super::TopologyPlan;

#[derive(Debug, Default)]
pub struct TopologyReconciler {
    applied_generation: Option<u64>,
}

impl TopologyReconciler {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            applied_generation: None,
        }
    }

    /// Applies a plan at most once per connection generation.
    ///
    /// # Errors
    ///
    /// Returns the classified transport failure from the first failed operation.
    pub async fn reconcile<C: TopologyChannel + ?Sized>(
        &mut self,
        channel: &C,
        plan: &TopologyPlan,
        generation: u64,
    ) -> Result<(), TopologyReconcileError> {
        if self.applied_generation == Some(generation) {
            return Ok(());
        }

        match plan.mode() {
            TopologyMode::Declare => {
                for exchange in plan.exchanges() {
                    channel.declare_exchange(exchange).await?;
                }
                for queue in plan.queues() {
                    channel.declare_queue(queue).await?;
                }
                for binding in plan.bindings() {
                    channel.bind_queue(binding).await?;
                }
            }
            TopologyMode::Verify => {
                for exchange in plan.exchanges() {
                    channel.verify_exchange(exchange).await?;
                }
                for queue in plan.queues() {
                    channel.verify_queue(queue).await?;
                }
            }
            TopologyMode::External => {}
        }

        self.applied_generation = Some(generation);
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologyReconcileError {
    source: TransportError,
}

impl fmt::Display for TopologyReconcileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "topology reconciliation failed: {}", self.source)
    }
}

impl Error for TopologyReconcileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

impl From<TransportError> for TopologyReconcileError {
    fn from(source: TransportError) -> Self {
        Self { source }
    }
}
