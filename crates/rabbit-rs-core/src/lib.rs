#![forbid(unsafe_code)]

//! Runtime-independent `RabbitMQ` primitives for Rabbit RS.

pub mod config;
pub mod consumer;
pub mod error;
pub mod pool;
pub mod runtime;
pub mod transport;
