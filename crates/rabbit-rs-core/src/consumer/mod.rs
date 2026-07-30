//! Consumer multiplexing and scheduling primitives.

pub mod actor;
pub mod delivery;
pub mod set;

mod scheduler;

pub use delivery::{ConsumerError, ConsumerErrorKind, Delivery, DeliveryState, Headers, MessageId};
pub use scheduler::{Scheduler, SubscriptionId, SubscriptionPolicy, WeightedFairScheduler};
pub use set::{ConsumerHandle, ConsumerSet, Subscription};
