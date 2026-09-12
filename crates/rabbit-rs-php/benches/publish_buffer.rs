//! Publish-buffer benchmark — the PHP-adjacent hot path.
//!
//! Every PHP `publish()` traverses this pure-Rust buffer before reaching
//! the core: conversion output lands via `enqueue`, the pipelined drain
//! flushes batches onto the wire and `quiesce` waits for their outcomes.
//! The mock transport answers immediately, so the cycle is CPU-bound.
//! No Zend runtime is touched: the bench drives the buffer exactly as the
//! pool class does, minus the PHP values.

use bytes::Bytes;
use rabbit_rs_core::client::ClientPool;
use rabbit_rs_core::publisher::{Destination, MessageProperties, PublishRequest};
use rabbit_rs_core::runtime::{PidProvider, RuntimeRegistry};
use rabbit_rs_core::transport::Transport;
use rabbit_rs_core::transport::mock::MockTransport;
use rabbit_rs_php::bench_api::{NativePublish, PublishBuffer};
// Provides the Zend symbol stubs so the binary links without a PHP engine
// (same trick the in-crate state-machine tests use). Nothing Zend runs here.
use std::sync::Arc;
use std::time::Duration;
use zend_link_stubs as _;

/// Publications flushed per measured iteration: the classic Laravel batch
/// shape (one web request dispatching a queue of jobs).
const BATCH: usize = 64;

/// Interval high enough that the age-flush timer can never fire mid-iteration
/// (the bench drives explicit flushes; `quiesce` aborts the armed timer).
const FLUSH_INTERVAL: Duration = Duration::from_mins(1);

/// Confirm-timeout budget: never reached, every mock send answers at once.
const HEALTHY_DEADLINE: Duration = Duration::from_secs(5);

/// Payload size of a representative Laravel job body.
const PAYLOAD: Bytes = Bytes::from_static(b"x-door-bench-payload-0123456789abcdef");

/// The production runtime shape: one background worker drives the actors
/// and drains spawned by the buffer, while the PHP (bench) thread blocks.
struct BackgroundRuntimeFactory;

impl rabbit_rs_core::runtime::RuntimeFactory for BackgroundRuntimeFactory {
    fn create(&self) -> std::io::Result<tokio::runtime::Runtime> {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
    }
}

struct FixedPid;

impl PidProvider for FixedPid {
    fn current_pid(&self) -> u32 {
        424_242
    }
}

/// One buffered publication as the conversion layer would hand it over.
fn publish(index: usize) -> NativePublish {
    NativePublish {
        broker: "bench".to_owned(),
        request: PublishRequest::new(
            Destination::new("jobs", "orders"),
            PAYLOAD,
            MessageProperties::new(format!("bench-{index}")),
            tokio::time::Instant::now() + HEALTHY_DEADLINE,
        ),
    }
}

struct BufferedPool {
    buffer: Arc<PublishBuffer>,
    /// Owns the runtime the connection handle borrows; declared last.
    _registry: RuntimeRegistry,
}

fn buffered_pool() -> BufferedPool {
    let transport = Arc::new(MockTransport::default());
    let registry =
        RuntimeRegistry::with_dependencies(Arc::new(FixedPid), Arc::new(BackgroundRuntimeFactory));
    let config = Arc::new(
        serde_json::from_value::<rabbit_rs_core::config::Config>(serde_json::json!({
            "brokers": [{
                "name": "bench",
                "hosts": [{"host": "rabbit.internal", "port": 5672}],
                "vhost": "/",
                "credentials": {"username": "guest", "password": "guest"},
                "tls": {"enabled": false},
                "heartbeat": 30,
            }],
            "workers": [],
            "topology_mode": "external",
            "queue_type": "quorum",
            "queue_durable": true,
        }))
        .expect("bench config deserializes")
        .validate()
        .expect("bench config validates"),
    );
    let handle = registry
        .acquire(rabbit_rs_core::pool::ConnectionKey::from_config(&config))
        .expect("connection handle");
    let client = Arc::new(ClientPool::new(
        Arc::clone(&config),
        Arc::clone(&transport) as Arc<dyn Transport>,
    ));
    BufferedPool {
        buffer: Arc::new(PublishBuffer::new(client, handle, FLUSH_INTERVAL)),
        _registry: registry,
    }
}

fn main() {
    divan::main();
}

/// One full buffered-publish cycle: 64 enqueues (the conversion output),
/// the buffer bookkeeping reads, a pipelined flush onto the wire and the
/// quiesce barrier awaiting the drain outcomes.
#[divan::bench]
fn publish_buffer_batch_64(bencher: divan::Bencher) {
    let BufferedPool { buffer, .. } = buffered_pool();

    bencher.bench(|| {
        for index in 0..BATCH {
            buffer.enqueue(publish(index));
        }
        divan::black_box(buffer.buffered_len());
        divan::black_box(buffer.buffered_bytes());
        divan::black_box(buffer.should_flush());
        divan::black_box(buffer.would_overflow(PAYLOAD.len()));
        buffer.flush_triggered().expect("pipelined flush");
        buffer.quiesce();
        divan::black_box(buffer.take_errors().len());
    });
}
