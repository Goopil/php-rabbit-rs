#![expect(
    non_snake_case,
    reason = "ext-php-rs preserves parameter identifiers for PHP named arguments"
)]

use std::sync::Arc;

use super::{
    bridge::EventBridge,
    consumer::Consumer,
    exception::{backpressure_exception, client_exception, rabbit_exception},
};
use crate::conversion;
use ext_php_rs::{
    boxed::ZBox,
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
    types::{ZendHashTable, Zval},
};
use rabbit_rs_core::{
    client::{ClientErrorKind, ClientPool},
    config::SafetyMode,
    pool::{ConnectionHandle, ConnectionKey},
    publisher::{PublishOutcome, PublishRequest},
    runtime::RuntimeRegistry,
};

/// Buffer threshold: flush when this many messages are buffered.
const BUFFER_THRESHOLD: usize = 64;
/// Maximum time to wait before flushing the buffer.
const BUFFER_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1);
/// Maximum number of buffered publish requests before flushing is forced.
const PUBLISH_BUFFER_MAX_MESSAGES: usize = 4096;
/// Maximum cumulative buffered payload bytes before flushing is forced.
const PUBLISH_BUFFER_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Native `RabbitMQ` connection and operation pool.
#[php_class]
#[php(name = "Goopil\\RabbitRs\\Pool")]
#[php(flags = ClassFlags::Final)]
pub struct Pool {
    handle: Arc<ConnectionHandle>,
    client: Arc<ClientPool>,
    pid: u32,
    bridge: Arc<EventBridge>,
    publish_buffer: std::sync::Mutex<Vec<conversion::NativePublish>>,
    publish_buffer_bytes: std::sync::Mutex<usize>,
    last_flush: std::sync::Mutex<std::time::Instant>,
}

#[php_impl]
impl Pool {
    /// Creates a native pool from its PHP configuration.
    pub fn __construct(config: &ZendHashTable) -> PhpResult<Self> {
        let config =
            Arc::new(
                conversion::validated_config(config).map_err(|message| {
                    ext_php_rs::prelude::PhpException::from_class::<
                        super::exception::RabbitRsException,
                    >(message)
                })?,
            );
        let key = ConnectionKey::from_config(&config);
        let handle = RuntimeRegistry::global().acquire(key).map_err(|error| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                error.to_string(),
            )
        })?;
        let client = handle.client(config.clone());
        let bridge = EventBridge::shared(&client);

        Ok(Self {
            handle,
            client,
            pid: std::process::id(),
            bridge,
            publish_buffer: std::sync::Mutex::new(Vec::with_capacity(BUFFER_THRESHOLD)),
            publish_buffer_bytes: std::sync::Mutex::new(0),
            last_flush: std::sync::Mutex::new(std::time::Instant::now()),
        })
    }

    /// Registers a PHP callback invoked when a broker connection state changes.
    ///
    /// The callback receives `(string $broker, string $state, int $generation)`.
    /// It is invoked synchronously on the PHP thread during publish, consume,
    /// and `stats()` operations.
    pub fn onConnectionState(&self, callback: &Zval) -> PhpResult<()> {
        self.bridge
            .set_connection_state_callback(callback.shallow_clone())
    }

    /// Registers a PHP callback invoked when publisher backpressure is detected.
    ///
    /// The callback receives `(string $broker, int $inFlight, int $capacity)`.
    /// It is invoked synchronously on the PHP thread during publish, consume,
    /// and `stats()` operations.
    pub fn onBackpressure(&self, callback: &Zval) -> PhpResult<()> {
        self.bridge
            .set_backpressure_callback(callback.shallow_clone())
    }

    /// Publishes one message and returns its stable message identifier.
    pub fn publish(&self, message: &ZendHashTable) -> PhpResult<String> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::publish")?;
        let publish = conversion::publish(message, "message").map_err(|message| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                message,
            )
        })?;

        let message_id = publish.request.properties.message_id.as_ref().to_owned();
        let payload_bytes = publish.request.payload.len();

        let mut buffer = self
            .publish_buffer
            .lock()
            .expect("publish buffer mutex poisoned");
        let mut buffered_bytes = *self
            .publish_buffer_bytes
            .lock()
            .expect("publish buffer bytes mutex poisoned");

        if buffer.len() >= PUBLISH_BUFFER_MAX_MESSAGES
            || buffered_bytes + payload_bytes > PUBLISH_BUFFER_MAX_BYTES
        {
            // Best-effort flush to make room. A failed flush re-buffers every
            // already-accepted message (they are never dropped); the re-check
            // below then surfaces explicit backpressure instead of leaking a
            // transport error the caller did not cause.
            drop(buffer);
            let _ = self.flush();
            buffer = self
                .publish_buffer
                .lock()
                .expect("publish buffer mutex poisoned");
            buffered_bytes = *self
                .publish_buffer_bytes
                .lock()
                .expect("publish buffer bytes mutex poisoned");
            if buffer.len() >= PUBLISH_BUFFER_MAX_MESSAGES
                || buffered_bytes + payload_bytes > PUBLISH_BUFFER_MAX_BYTES
            {
                return backpressure_exception(&format!(
                    "publish buffer is full ({} messages, {} buffered bytes); retry after flush",
                    buffer.len(),
                    buffered_bytes,
                ));
            }
        }

        buffer.push(publish);
        *self
            .publish_buffer_bytes
            .lock()
            .expect("publish buffer bytes mutex poisoned") += payload_bytes;

        let should_flush = buffer.len() >= BUFFER_THRESHOLD
            || self
                .last_flush
                .lock()
                .expect("last_flush mutex poisoned")
                .elapsed()
                >= BUFFER_FLUSH_INTERVAL;
        if should_flush {
            let publishes = std::mem::take(&mut *buffer);
            drop(buffer);
            *self
                .publish_buffer_bytes
                .lock()
                .expect("publish buffer bytes mutex poisoned") -=
                Self::buffered_payload_bytes(&publishes);
            *self.last_flush.lock().expect("last_flush mutex poisoned") = std::time::Instant::now();
            self.flush_publishes(publishes)?;
        }

        self.bridge.drain();
        Ok(message_id)
    }

    /// Flushes the publish buffer, sending all buffered messages to the broker.
    ///
    /// In blind mode this is a barrier: every request enqueued on the publish
    /// pump before this call — including buffered publications flushed just
    /// above and any earlier blind publish — has been handed to the transport
    /// (or dropped for lack of a channel during recovery) when `flush`
    /// returns. Hand-off is not delivery: per the blind fire-and-forget
    /// contract, a later transport failure is a silent loss.
    pub fn flush(&self) -> PhpResult<()> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::flush")?;
        let publishes = self.take_publish_buffer();
        if !publishes.is_empty() {
            *self.last_flush.lock().expect("last_flush mutex poisoned") = std::time::Instant::now();
            self.flush_publishes(publishes)?;
        }
        if matches!(self.client.safety_mode(), SafetyMode::Blind)
            && let Err(error) = self.handle.runtime().block_on(self.client.flush_blind())
        {
            return client_exception(&error);
        }
        self.bridge.drain();
        Ok(())
    }

    /// Publishes multiple messages in one boundary crossing.
    pub fn publish_batch(&self, messages: &ZendHashTable) -> PhpResult<Vec<String>> {
        self.flush()?;
        self.ensure_open("Goopil\\RabbitRs\\Pool::publishBatch")?;
        let publishes = conversion::publish_batch(messages).map_err(|message| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                message,
            )
        })?;
        let requests = publishes
            .into_iter()
            .map(|publish| (publish.broker, publish.request))
            .collect();
        match self
            .handle
            .runtime()
            .block_on(self.client.publish_batch(requests))
        {
            Ok(outcomes) => {
                self.bridge.drain();
                outcomes.into_iter().map(publish_message_id).collect()
            }
            Err(error) => {
                self.bridge.drain();
                client_exception(&error)
            }
        }
    }

    /// Opens a consumer for a configured profile.
    pub fn consumer(&self, profile: &str) -> PhpResult<Consumer> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::consumer")?;
        match self
            .handle
            .runtime()
            .block_on(self.client.consumer(profile))
        {
            Ok(handle) => Ok(Consumer::new(
                handle,
                self.handle.runtime().clone(),
                self.pid,
                Arc::clone(&self.bridge),
            )),
            Err(error) => client_exception(&error),
        }
    }

    /// Returns the current native metrics snapshot.
    pub fn stats(&self) -> PhpResult<ZBox<ZendHashTable>> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::stats")?;
        let mut stats = ZendHashTable::new();
        stats.insert("closed", self.handle.is_closed())?;
        stats.insert("pid", i64::from(self.pid))?;
        stats.insert("handle", self.handle.identifier())?;
        let metrics = self.client.metrics_snapshot();
        stats.insert("publishes_total", i64_from_counter(metrics.publishes_total))?;
        stats.insert(
            "confirmations_total",
            i64_from_counter(metrics.confirmations_total),
        )?;
        stats.insert("returns_total", i64_from_counter(metrics.returns_total))?;
        stats.insert(
            "backpressure_total",
            i64_from_counter(metrics.backpressure_total),
        )?;
        stats.insert(
            "reconnects_total",
            i64_from_counter(metrics.reconnects_total),
        )?;
        stats.insert(
            "deliveries_total",
            i64_from_counter(metrics.deliveries_total),
        )?;
        stats.insert("acks_total", i64_from_counter(metrics.acks_total))?;
        stats.insert("rejects_total", i64_from_counter(metrics.rejects_total))?;

        insert_percentile(
            &mut stats,
            "confirmation_latency_p50",
            metrics.confirmation_latency.percentile_ns(50.0),
        )?;
        insert_percentile(
            &mut stats,
            "confirmation_latency_p95",
            metrics.confirmation_latency.percentile_ns(95.0),
        )?;
        insert_percentile(
            &mut stats,
            "confirmation_latency_p99",
            metrics.confirmation_latency.percentile_ns(99.0),
        )?;
        insert_percentile(
            &mut stats,
            "settlement_latency_p50",
            metrics.settlement_latency.percentile_ns(50.0),
        )?;
        insert_percentile(
            &mut stats,
            "settlement_latency_p95",
            metrics.settlement_latency.percentile_ns(95.0),
        )?;
        insert_percentile(
            &mut stats,
            "settlement_latency_p99",
            metrics.settlement_latency.percentile_ns(99.0),
        )?;

        self.bridge.drain();

        Ok(stats)
    }

    /// Returns the number of pending messages in a queue on the given broker.
    pub fn size(&self, broker: &str, queue: &str) -> PhpResult<i64> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::size")?;
        match self
            .handle
            .runtime()
            .block_on(self.client.queue_size(broker, queue))
        {
            Ok(count) => Ok(i64::from(count)),
            Err(error) => client_exception(&error),
        }
    }

    /// Purges all messages from a queue on the given broker.
    pub fn clear(&self, broker: &str, queue: &str) -> PhpResult<()> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::clear")?;
        match self
            .handle
            .runtime()
            .block_on(self.client.purge_queue(broker, queue))
        {
            Ok(()) => Ok(()),
            Err(error) => client_exception(&error),
        }
    }

    /// Closes this pool handle.
    pub fn close(&self) -> PhpResult<()> {
        if self.pid != std::process::id() {
            return rabbit_exception("cannot close a pool inherited across fork");
        }
        // Flush before closing. If flush fails the error is deferred (swallowed)
        // so close always proceeds; unconfirmed publications were re-buffered by
        // flush_publishes (unroutable returns are definitive and dropped) and
        // will be lost when the handle drops. This is an accepted limitation of
        // deferred flush in close/destruct paths.
        let _ = self.flush();
        if !self.handle.is_closed()
            && let Err(error) = self.handle.runtime().block_on(self.client.close())
        {
            self.handle.close();
            return client_exception(&error);
        }
        self.handle.close();
        Ok(())
    }

    /// Auto-flushes buffered messages when the pool is garbage collected.
    pub fn __destruct(&self) {
        if self.pid != std::process::id() {
            return;
        }
        // Deferred error path: flush errors are swallowed during GC teardown.
        // Unconfirmed publications were re-buffered by flush_publishes but will
        // be lost when the pool is dropped. Callers that need delivery guarantees
        // should call flush() explicitly before the pool goes out of scope.
        let _ = self.flush();
    }
}

#[allow(
    clippy::match_same_arms,
    reason = "Confirmed and Ambiguous are semantically distinct outcomes that both return the message_id"
)]
fn publish_message_id(outcome: PublishOutcome) -> PhpResult<String> {
    match outcome {
        PublishOutcome::Confirmed { message_id } => Ok(message_id.as_ref().to_owned()),
        PublishOutcome::Returned { message_id, reply } => rabbit_exception(format!(
            "message {message_id} was returned as unroutable (AMQP {})",
            reply.code
        )),
        PublishOutcome::Ambiguous { message_id } => Ok(message_id.as_ref().to_owned()),
    }
}

fn i64_from_counter(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Inserts a latency percentile as integer milliseconds into the stats table.
/// A `None` percentile (no samples recorded) is stored as `0`.
fn insert_percentile(
    stats: &mut ZendHashTable,
    key: &str,
    percentile_ns: Option<u64>,
) -> PhpResult<()> {
    let millis = percentile_ns.map_or(0, |nanos| nanos / 1_000_000);
    stats.insert(key, i64::try_from(millis).unwrap_or(i64::MAX))?;
    Ok(())
}

impl Pool {
    #[cfg(feature = "extension-tests")]
    pub(crate) fn for_testing(handle: Arc<ConnectionHandle>, client: Arc<ClientPool>) -> Self {
        Self {
            handle,
            bridge: EventBridge::shared(&client),
            client,
            pid: std::process::id(),
            publish_buffer: std::sync::Mutex::new(Vec::with_capacity(BUFFER_THRESHOLD)),
            publish_buffer_bytes: std::sync::Mutex::new(0),
            last_flush: std::sync::Mutex::new(std::time::Instant::now()),
        }
    }

    /// Total payload bytes of the given buffered publications.
    fn buffered_payload_bytes(publishes: &[conversion::NativePublish]) -> usize {
        publishes
            .iter()
            .map(|publish| publish.request.payload.len())
            .sum()
    }

    /// Drains the publish buffer, keeping the byte counter in sync.
    fn take_publish_buffer(&self) -> Vec<conversion::NativePublish> {
        let mut buffer = self
            .publish_buffer
            .lock()
            .expect("publish buffer mutex poisoned");
        let publishes = std::mem::take(&mut *buffer);
        drop(buffer);
        *self
            .publish_buffer_bytes
            .lock()
            .expect("publish buffer bytes mutex poisoned") -=
            Self::buffered_payload_bytes(&publishes);
        publishes
    }

    /// Re-buffers publications whose flush failed, keeping the byte counter in
    /// sync. Re-buffered publications may exceed the buffer ceiling: they were
    /// already accepted (a `message_id` was returned) and are never dropped.
    fn rebuffer_publishes(&self, publishes: Vec<conversion::NativePublish>) {
        let bytes = Self::buffered_payload_bytes(&publishes);
        let mut buffer = self
            .publish_buffer
            .lock()
            .expect("publish buffer mutex poisoned");
        buffer.extend(publishes);
        *self
            .publish_buffer_bytes
            .lock()
            .expect("publish buffer bytes mutex poisoned") += bytes;
    }

    fn flush_publishes(&self, publishes: Vec<conversion::NativePublish>) -> PhpResult<()> {
        if publishes.is_empty() {
            return Ok(());
        }

        // Keep the original requests so a failed flush can re-buffer them.
        let requests: Vec<(String, PublishRequest)> = publishes
            .iter()
            .map(|publish| (publish.broker.clone(), publish.request.clone()))
            .collect();

        let batch = self
            .handle
            .runtime()
            .block_on(self.client.publish_batch(requests));

        match batch {
            Ok(outcomes) => {
                // Every outcome is inspected before anything is raised so a
                // failure never short-circuits the buffer decisions. With the
                // current `publish_batch` contract this arm only ever sees
                // `Confirmed`, `Ambiguous`, and `Returned` outcomes: per-message
                // failures such as backpressure or timeout are folded into the
                // batch-level `Err` below, which re-buffers every request.
                let mut first_error = None;
                for outcome in outcomes {
                    if let Err(error) = publish_message_id(outcome) {
                        // `Returned` is the only outcome that resolves to an
                        // error here. An unroutable message is definitive:
                        // re-buffering it would loop forever, so the error is
                        // recorded instead and raised once every outcome has
                        // been processed.
                        first_error.get_or_insert(error);
                    }
                }
                first_error.map_or(Ok(()), Err)
            }
            Err(error) => {
                // `publish_batch` discards per-message results after the first
                // terminal failure, so every request of this flush is
                // un-attempted or of unknown state. Conservatively re-buffer
                // all of them, oldest first, so the next flush retries them;
                // duplicates are permitted and identifiable via `message_id`.
                // A closing pool must not re-buffer.
                if !matches!(error.kind(), ClientErrorKind::Closed) {
                    self.rebuffer_publishes(publishes);
                }
                client_exception(&error)
            }
        }
    }

    fn ensure_open(&self, operation: &str) -> PhpResult<()> {
        if self.pid != std::process::id() {
            return rabbit_exception(format!(
                "{operation} cannot use a pool inherited across fork"
            ));
        }
        if self.handle.is_closed() {
            return rabbit_exception(format!("{operation} cannot use a closed pool"));
        }
        Ok(())
    }
}
