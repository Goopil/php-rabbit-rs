#![forbid(unsafe_code)]

//! Runtime-independent `RabbitMQ` primitives for Rabbit RS.

pub mod config;
pub mod consumer;
pub mod error;
pub mod metrics;
pub mod pool;
pub mod publisher;
pub mod recovery;
pub mod runtime;
pub mod topology;
pub mod transport;
