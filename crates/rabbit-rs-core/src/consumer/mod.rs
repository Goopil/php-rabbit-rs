//! Consumer multiplexing and scheduling primitives.

pub mod actor;
pub mod attempts;
pub mod delivery;
pub mod set;

mod scheduler;

pub use attempts::{
    APPLICATION_ATTEMPTS_HEADER, AttemptsError, AttemptsErrorKind, AttemptsResolver,
};
pub use delivery::{ConsumerError, ConsumerErrorKind, Delivery, DeliveryState, Headers, MessageId};
pub use scheduler::{Scheduler, SubscriptionId, SubscriptionPolicy, WeightedFairScheduler};
pub use set::{ConsumerHandle, ConsumerSet, Subscription};
