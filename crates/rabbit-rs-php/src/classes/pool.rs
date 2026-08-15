use std::sync::Arc;

use super::{
    consumer::Consumer,
    exception::{client_exception, rabbit_exception},
};
use crate::conversion;
use ext_php_rs::{
    boxed::ZBox,
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
    types::ZendHashTable,
};
use rabbit_rs_core::{
    client::ClientPool,
    pool::{ConnectionHandle, ConnectionKey},
    publisher::PublishOutcome,
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
        })
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

fn publish_message_id(outcome: PublishOutcome) -> PhpResult<String> {
    match outcome {
        PublishOutcome::Confirmed { message_id } => Ok(message_id),
        PublishOutcome::Returned { message_id, reply } => rabbit_exception(format!(
            "message {message_id} was returned as unroutable (AMQP {})",
            reply.code
        )),
        PublishOutcome::Ambiguous { message_id } => rabbit_exception(format!(
            "message {message_id} has an ambiguous publication outcome"
        )),
    }
}

fn i64_from_counter(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

impl Pool {
    #[cfg(feature = "extension-tests")]
    pub(crate) fn for_testing(handle: Arc<ConnectionHandle>, client: Arc<ClientPool>) -> Self {
        Self {
            handle,
            client,
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
}
