#![expect(
    non_snake_case,
    reason = "ext-php-rs preserves parameter identifiers for PHP named arguments"
)]

use std::sync::Arc;
#[cfg(feature = "extension-tests")]
use std::time::Duration;

use super::{
    bridge::EventBridge,
    consumer::Consumer,
    exception::{
        backpressure_exception, client_exception, connection_exception, rabbit_exception,
        rabbit_exception_message,
    },
    publish_buffer::{PublishBuffer, publish_message_id},
};
use crate::conversion;
use ext_php_rs::{
    boxed::ZBox,
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
    types::{ZendHashTable, Zval},
};
use rabbit_rs_core::{
    client::ClientPool,
    config::SafetyMode,
    pool::{ConnectionHandle, ConnectionKey},
    runtime::RuntimeRegistry,
    topology::delay::DelayStrategy,
};

/// Native `RabbitMQ` connection and operation pool.
#[php_class]
#[php(name = "Goopil\\RabbitRs\\Pool")]
#[php(flags = ClassFlags::Final)]
pub struct Pool {
    handle: Arc<ConnectionHandle>,
    client: Arc<ClientPool>,
    delay_strategy: DelayStrategy,
    pid: u32,
    bridge: Arc<EventBridge>,
    publish_buffer: Arc<PublishBuffer>,
}

#[php_impl]
impl Pool {
    /// Creates a native pool from its PHP configuration.
    pub fn __construct(config: &ZendHashTable) -> PhpResult<Self> {
        let config =
            Arc::new(conversion::validated_config(config).map_err(rabbit_exception_message)?);
        let key = ConnectionKey::from_config(&config);
        let handle = RuntimeRegistry::global()
            .acquire(key)
            .map_err(|error| rabbit_exception_message(error.to_string()))?;
        let client = handle.client(config.clone());
        let bridge = EventBridge::shared(&client);

        Ok(Self {
            publish_buffer: Arc::new(PublishBuffer::new(Arc::clone(&client), Arc::clone(&handle))),
            handle,
            client,
            delay_strategy: DelayStrategy::compile(&config),
            pid: std::process::id(),
            bridge,
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

    /// Removes every registered event callback, returning how many were
    /// removed (connection-state and backpressure combined).
    ///
    /// Connections sharing one native pool each register their own callbacks;
    /// clearing allows a fresh registration to start from a clean slate.
    pub fn clearEventCallbacks(&self) -> i64 {
        i64::try_from(self.bridge.clear_event_callbacks()).unwrap_or(i64::MAX)
    }

    /// Publishes one message and returns its stable message identifier.
    pub fn publish(&self, message: &ZendHashTable) -> PhpResult<String> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::publish")?;
        let publish = conversion::publish(message, "message", &self.delay_strategy)
            .map_err(rabbit_exception_message)?;

        let message_id = publish.request.properties.message_id.as_ref().to_owned();
        let payload_bytes = publish.request.payload.len();

        if self.publish_buffer.would_overflow(payload_bytes) {
            // Best-effort flush to make room. A failed flush re-buffers every
            // already-accepted message (they are never dropped); the re-check
            // below then surfaces explicit backpressure instead of leaking a
            // transport error the caller did not cause.
            let _ = self.flush();
            if self.publish_buffer.would_overflow(payload_bytes) {
                return backpressure_exception(&format!(
                    "publish buffer is full ({} messages, {} buffered bytes); retry after flush",
                    self.publish_buffer.buffered_len(),
                    self.publish_buffer.buffered_bytes(),
                ));
            }
        }

        self.publish_buffer.enqueue(publish);

        if self.publish_buffer.should_flush() {
            // Pipelined flush (Round D): the batch is spawned on the runtime
            // and publish returns before confirmations resolve. Non-confirmed
            // outcomes surface at the next operation (see below).
            self.publish_buffer.flush_triggered()?;
        }

        self.bridge.drain();
        self.surface_publish_errors()?;
        Ok(message_id)
    }

    /// Flushes the publish buffer, sending all buffered messages to the broker.
    ///
    /// Outstanding pipelined drains are quiesced first (bounded by the fixed
    /// teardown budget) so their re-buffered publications are visible to this
    /// drain. The flush itself keeps full-deadline semantics: every buffered
    /// publication is confirmed — or its failure raised — when `flush`
    /// returns.
    ///
    /// In blind mode this is a barrier: every request enqueued on the publish
    /// pump before this call — including buffered publications flushed just
    /// above and any earlier blind publish — has been handed to the transport
    /// (or dropped for lack of a channel during recovery) when `flush`
    /// returns. Hand-off is not delivery: per the blind fire-and-forget
    /// contract, a later transport failure is a silent loss.
    pub fn flush(&self) -> PhpResult<()> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::flush")?;
        self.publish_buffer.flush_all()?;
        if matches!(self.client.safety_mode(), SafetyMode::Blind)
            && let Err(error) = self.handle.runtime().block_on(self.client.flush_blind())
        {
            return client_exception(&error);
        }
        self.bridge.drain();
        self.surface_publish_errors()
    }

    /// Publishes multiple messages in one boundary crossing.
    pub fn publish_batch(&self, messages: &ZendHashTable) -> PhpResult<Vec<String>> {
        self.flush()?;
        self.ensure_open("Goopil\\RabbitRs\\Pool::publishBatch")?;
        let publishes = conversion::publish_batch(messages, &self.delay_strategy)
            .map_err(rabbit_exception_message)?;
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
                Arc::clone(&self.publish_buffer),
            )),
            Err(error) => client_exception(&error),
        }
    }

    /// Returns the current native metrics snapshot.
    ///
    /// A pending pipelined publish failure surfaces here (thrown) before the
    /// snapshot is built, so a failed publish is observable at the next
    /// stats operation at the latest.
    pub fn stats(&self) -> PhpResult<ZBox<ZendHashTable>> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::stats")?;
        self.surface_publish_errors()?;
        let mut stats = ZendHashTable::new();
        stats.insert("closed", self.handle.is_closed())?;
        stats.insert("pid", i64::from(self.pid))?;
        stats.insert("handle", self.handle.identifier())?;
        let metrics = self.client.metrics_snapshot();
        for (key, value) in [
            ("publishes_total", metrics.publishes_total),
            ("confirmations_total", metrics.confirmations_total),
            ("returns_total", metrics.returns_total),
            ("backpressure_total", metrics.backpressure_total),
            (
                "publication_retries_total",
                metrics.publication_retries_total,
            ),
            ("reconnects_total", metrics.reconnects_total),
            ("deliveries_total", metrics.deliveries_total),
            ("duplicates_total", metrics.duplicates_total),
            ("acks_total", metrics.acks_total),
            ("rejects_total", metrics.rejects_total),
        ] {
            stats.insert(key, i64_from_counter(value))?;
        }
        stats.insert(
            "dropped_publications_total",
            i64_from_counter(self.publish_buffer.dropped_publications()),
        )?;
        stats.insert(
            "dropped_error_records_total",
            i64_from_counter(self.publish_buffer.dropped_error_records()),
        )?;
        // Publish buffer occupancy (Round K #143): the soak tripwire reads
        // this to catch a re-buffer leak path (buffer must quiesce to zero).
        stats.insert(
            "publish_buffered",
            i64::try_from(self.publish_buffer.buffered_len()).unwrap_or(i64::MAX),
        )?;
        stats.insert(
            "publish_buffered_bytes",
            i64::try_from(self.publish_buffer.buffered_bytes()).unwrap_or(i64::MAX),
        )?;

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
    ///
    /// Flushes the publish buffer first (quiescing outstanding pipelined
    /// drains) so publications accepted by this pool are counted.
    pub fn size(&self, broker: &str, queue: &str) -> PhpResult<i64> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::size")?;
        self.publish_buffer.flush_all()?;
        self.surface_publish_errors()?;
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
    ///
    /// Flushes the publish buffer first (quiescing outstanding pipelined
    /// drains) so buffered publications cannot repopulate the queue after
    /// the purge.
    pub fn clear(&self, broker: &str, queue: &str) -> PhpResult<()> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::clear")?;
        self.publish_buffer.flush_all()?;
        self.surface_publish_errors()?;
        match self
            .handle
            .runtime()
            .block_on(self.client.purge_queue(broker, queue))
        {
            Ok(()) => Ok(()),
            Err(error) => client_exception(&error),
        }
    }

    /// Drains non-confirmed publish outcomes recorded by the pipelined
    /// flush, returning one hash per record with `kind`, `message_id`, and
    /// `message`. The queue is cleared by this call; the same records would
    /// otherwise surface as exceptions at the next publish/flush/size/
    /// clear/stats operation.
    pub fn drainErrors(&self) -> PhpResult<ZBox<ZendHashTable>> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::drainErrors")?;
        self.bridge.drain();
        let errors = self.publish_buffer.take_errors();
        let mut table = ZendHashTable::new();
        for (i, error) in errors.iter().enumerate() {
            let mut entry = ZendHashTable::new();
            entry.insert("kind", error.kind.as_str())?;
            entry.insert("message_id", error.message_id.as_str())?;
            entry.insert("message", error.message.as_str())?;
            table.insert(i, entry)?;
        }
        Ok(table)
    }

    /// Closes this pool handle.
    pub fn close(&self) -> PhpResult<()> {
        if self.pid != std::process::id() {
            return rabbit_exception("cannot close a pool inherited across fork");
        }
        // Flush before closing under full-deadline semantics: an explicit
        // close() keeps the caller's timeout contract. If flush fails the
        // error is deferred (swallowed) so close always proceeds; retriable
        // publications were re-buffered by flush_batch and are counted as
        // dropped by the destructor's bounded flush; deadline-expired
        // publications are counted by rebuffer itself (audit F-18).
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
        // Fixed teardown budget (audit F-18): the destructor must not block
        // for up to the per-message timeout (30 s default, 24 h ceiling) at
        // FPM or request shutdown. Anything the budget could not confirm is
        // counted in `dropped_publications_total`; an explicit flush() or
        // close() keeps full-deadline semantics.
        self.publish_buffer.flush_teardown();
        if matches!(self.client.safety_mode(), SafetyMode::Blind) {
            let _ = self.handle.runtime().block_on(async {
                tokio::time::timeout(
                    super::publish_buffer::TEARDOWN_FLUSH_BUDGET,
                    self.client.flush_blind(),
                )
                .await
            });
        }
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
    pub(crate) fn for_testing(
        handle: Arc<ConnectionHandle>,
        client: Arc<ClientPool>,
        delay_strategy: DelayStrategy,
        flush_interval: Option<Duration>,
        flush_threshold: Option<usize>,
    ) -> Self {
        let publish_buffer = PublishBuffer::new(Arc::clone(&client), Arc::clone(&handle));
        let publish_buffer = match flush_interval {
            Some(interval) => publish_buffer.with_flush_interval(interval),
            None => publish_buffer,
        };
        let publish_buffer = match flush_threshold {
            Some(threshold) => publish_buffer.with_flush_threshold(threshold),
            None => publish_buffer,
        };
        Self {
            publish_buffer: Arc::new(publish_buffer),
            handle,
            bridge: EventBridge::shared(&client),
            client,
            delay_strategy,
            pid: std::process::id(),
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

    /// Surfaces the oldest pending publish error (recorded by the pipelined
    /// flush) as the exception its kind maps to, mirroring the sync
    /// `client_exception` mapping. The whole queue is cleared: every record
    /// has been processed, and only the first failure is raised — the same
    /// behavior as the sync flush raising its first error.
    fn surface_publish_errors(&self) -> PhpResult<()> {
        let errors = self.publish_buffer.take_errors();
        let Some(first) = errors.first() else {
            return Ok(());
        };
        match first.kind.as_str() {
            "Transport" => connection_exception(&first.message),
            "Backpressure" => backpressure_exception(&first.message),
            _ => rabbit_exception(&first.message),
        }
    }
}
