//! Consumer multiplexing and scheduling primitives.

mod scheduler;

pub use scheduler::{Scheduler, SubscriptionId, SubscriptionPolicy, WeightedFairScheduler};
