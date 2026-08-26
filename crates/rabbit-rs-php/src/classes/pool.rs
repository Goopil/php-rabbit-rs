#![expect(
    non_snake_case,
    reason = "ext-php-rs preserves parameter identifiers for PHP named arguments"
)]

use std::{collections::HashMap, sync::Arc};

use super::{
    consumer::Consumer,
    exception::{client_exception, rabbit_exception},
};
use crate::callbacks::CallbackSlot;
use crate::conversion;
use ext_php_rs::{
    boxed::ZBox,
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
    types::{ZendHashTable, Zval},
};
use rabbit_rs_core::{
    client::ClientPool,
    pool::{ConnectionHandle, ConnectionKey},
    publisher::PublishOutcome,
    recovery::ConnectionState,
    runtime::RuntimeRegistry,
};

/// Native `RabbitMQ` connection and operation pool.
#[php_class]
#[php(name = "Goopil\\RabbitRs\\Pool")]
#[php(flags = ClassFlags::Final)]
pub struct Pool {
    handle: Arc<ConnectionHandle>,
    client: Arc<ClientPool>,
    pid: u32,
    connection_state_callback: CallbackSlot,
    backpressure_callback: CallbackSlot,
    last_connection_states: std::sync::Mutex<HashMap<String, (String, i64)>>,
    last_backpressure_total: std::sync::Mutex<u64>,
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

        Ok(Self {
            handle,
            client,
            pid: std::process::id(),
            connection_state_callback: CallbackSlot::new(),
            backpressure_callback: CallbackSlot::new(),
            last_connection_states: std::sync::Mutex::new(HashMap::new()),
            last_backpressure_total: std::sync::Mutex::new(0),
        })
    }

    /// Registers a PHP callback invoked when a broker connection state changes.
    ///
    /// The callback receives `(string $broker, string $state, int $generation)`.
    /// It is invoked synchronously on the PHP thread during `stats()`.
    pub fn onConnectionState(&self, callback: &Zval) -> PhpResult<()> {
        self.connection_state_callback
            .set(callback.shallow_clone())?;
        Ok(())
    }

    /// Registers a PHP callback invoked when publisher backpressure is detected.
    ///
    /// The callback receives `(string $broker, int $inFlight, int $capacity)`.
    /// It is invoked synchronously on the PHP thread during `stats()`.
    pub fn onBackpressure(&self, callback: &Zval) -> PhpResult<()> {
        self.backpressure_callback.set(callback.shallow_clone())?;
        Ok(())
    }

    /// Publishes one message and returns its stable message identifier.
    pub fn publish(&self, message: &ZendHashTable) -> PhpResult<String> {
        self.ensure_open("Goopil\\RabbitRs\\Pool::publish")?;
        let publish = conversion::publish(message, "message").map_err(|message| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                message,
            )
        })?;
        let outcome = self
            .handle
            .runtime()
            .block_on(self.client.publish(&publish.broker, publish.request));
        match outcome {
            Ok(outcome) => publish_message_id(outcome),
            Err(error) => client_exception(&error),
        }
    }

    /// Publishes multiple messages in one boundary crossing.
    pub fn publish_batch(&self, messages: &ZendHashTable) -> PhpResult<Vec<String>> {
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
            Ok(outcomes) => outcomes.into_iter().map(publish_message_id).collect(),
            Err(error) => client_exception(&error),
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

        self.invoke_connection_state_callbacks();
        self.invoke_backpressure_callback(metrics.backpressure_total);

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
        if !self.handle.is_closed()
            && let Err(error) = self.handle.runtime().block_on(self.client.close())
        {
            self.handle.close();
            return client_exception(&error);
        }
        self.handle.close();
        Ok(())
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
            client,
            pid: std::process::id(),
            connection_state_callback: CallbackSlot::new(),
            backpressure_callback: CallbackSlot::new(),
            last_connection_states: std::sync::Mutex::new(HashMap::new()),
            last_backpressure_total: std::sync::Mutex::new(0),
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

    fn invoke_connection_state_callbacks(&self) {
        let states = self.client.connection_states();

        // Collect all changed states under the lock, then release before invoking
        // callbacks to prevent deadlock when a callback re-enters stats().
        let pending: Vec<(String, String, i64, (String, i64))> = {
            let mut last_states = self
                .last_connection_states
                .lock()
                .expect("connection state mutex poisoned");
            states
                .iter()
                .filter_map(|(broker, state)| {
                    let (state_name, generation) = connection_state_parts(state);
                    let current = (state_name.clone(), generation);
                    let changed = last_states
                        .get(broker)
                        .is_none_or(|previous| *previous != current);
                    if changed {
                        last_states.insert(broker.clone(), current.clone());
                        Some((broker.clone(), state_name, generation, current))
                    } else {
                        None
                    }
                })
                .collect()
        }; // Lock released here

        for (broker, state_name, generation, _) in pending {
            let _ = self.connection_state_callback.invoke_unlocked(vec![
                &broker.as_str(),
                &state_name,
                &generation,
            ]);
        }
    }

    fn invoke_backpressure_callback(&self, current_backpressure: u64) {
        // Determine under lock whether the backpressure metric changed, then
        // release the lock before invoking the callback to prevent deadlock
        // when the callback re-enters stats().
        let should_invoke = {
            let mut last = self
                .last_backpressure_total
                .lock()
                .expect("backpressure mutex poisoned");
            if current_backpressure > *last {
                *last = current_backpressure;
                true
            } else {
                false
            }
        }; // Lock released here

        if should_invoke {
            let (in_flight, capacity) = self.client.publisher_utilization();
            let _ = self.backpressure_callback.invoke_unlocked(vec![
                &"global".to_string(),
                &i64::try_from(in_flight).unwrap_or(i64::MAX),
                &i64::try_from(capacity).unwrap_or(i64::MAX),
            ]);
        }
    }
}

fn connection_state_parts(state: &ConnectionState) -> (String, i64) {
    match state {
        ConnectionState::Disconnected => ("disconnected".to_string(), 0),
        ConnectionState::Connecting { attempt } => ("connecting".to_string(), i64::from(*attempt)),
        ConnectionState::Ready { generation } => (
            "ready".to_string(),
            i64::try_from(*generation).unwrap_or(i64::MAX),
        ),
        ConnectionState::Recovering {
            attempt,
            retry_in: _,
            reason: _,
        } => ("recovering".to_string(), i64::from(*attempt)),
        ConnectionState::FailedPermanent { kind: _, reason: _ } => {
            ("failed_permanent".to_string(), 0)
        }
        ConnectionState::Closed => ("closed".to_string(), 0),
    }
}
