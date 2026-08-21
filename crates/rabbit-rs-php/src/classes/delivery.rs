#![expect(
    non_snake_case,
    reason = "ext-php-rs preserves parameter identifiers for PHP named arguments"
)]

use super::exception::{consumer_exception, rabbit_exception};
use ext_php_rs::{
    binary::Binary,
    boxed::ZBox,
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
    types::{ZendHashTable, Zval},
};
use rabbit_rs_core::consumer::{Delivery as NativeDelivery, DeliveryState};
use rabbit_rs_core::transport::HeaderValue;
use tokio::runtime::Handle;

/// Native delivery and its acknowledgement token.
#[php_class]
#[php(name = "Goopil\\RabbitRs\\Delivery")]
#[php(flags = ClassFlags::Final)]
pub struct Delivery {
    inner: NativeDelivery,
    runtime: Handle,
    pid: u32,
}

#[php_impl]
impl Delivery {
    /// Returns the binary-safe delivery payload.
    pub fn payload(&self) -> PhpResult<Binary<u8>> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::payload")?;
        Ok(Binary::new(self.inner.payload.to_vec()))
    }

    /// Returns delivery metadata as a PHP array.
    pub fn metadata(&self) -> PhpResult<ZBox<ZendHashTable>> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::metadata")?;
        let mut metadata = ZendHashTable::new();
        metadata.insert("message_id", self.inner.id.as_str())?;
        if let Some(correlation_id) = &self.inner.correlation_id {
            metadata.insert("correlation_id", correlation_id.as_str())?;
        }
        metadata.insert("subscription", self.inner.subscription.as_str())?;
        metadata.insert("attempts", i64::from(self.inner.attempts))?;
        metadata.insert("state", state_name(self.inner.state()))?;
        let mut headers = ZendHashTable::new();
        for (key, value) in self.inner.headers.iter() {
            insert_header(&mut headers, key, value)?;
        }
        metadata.insert("headers", headers)?;
        Ok(metadata)
    }

    /// Acknowledges the delivery.
    pub fn ack(&self) -> PhpResult<()> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::ack")?;
        self.runtime
            .block_on(self.inner.ack())
            .map_err(|error| consumer_php_exception(&error))
    }

    /// Releases the delivery immediately or after a delay.
    #[php(defaults(delayMs = 0))]
    pub fn release(&self, delayMs: i64) -> PhpResult<()> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::release")?;
        let delay = u64::try_from(delayMs).map_err(|_| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                "delayMs must be a non-negative integer".to_owned(),
            )
        })?;
        self.runtime
            .block_on(self.inner.release(std::time::Duration::from_millis(delay)))
            .map_err(|error| consumer_php_exception(&error))
    }

    /// Rejects the delivery with optional requeueing.
    #[php(defaults(requeue = false))]
    pub fn reject(&self, requeue: bool) -> PhpResult<()> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::reject")?;
        self.runtime
            .block_on(self.inner.reject(requeue))
            .map_err(|error| consumer_php_exception(&error))
    }
}

impl Delivery {
    pub(crate) fn new(inner: NativeDelivery, runtime: Handle, pid: u32) -> Self {
        Self {
            inner,
            runtime,
            pid,
        }
    }

    fn ensure_current_process(&self, operation: &str) -> PhpResult<()> {
        if self.pid != std::process::id() {
            return rabbit_exception(format!(
                "{operation} cannot use a delivery inherited across fork"
            ));
        }
        Ok(())
    }
}

fn insert_header(table: &mut ZendHashTable, key: &str, value: &HeaderValue) -> PhpResult<()> {
    match value {
        HeaderValue::Void => table.insert(key, Zval::null())?,
        HeaderValue::Boolean(value) => table.insert(key, *value)?,
        HeaderValue::Integer(value) => table.insert(key, *value)?,
        HeaderValue::Double(value) => table.insert(key, value.get())?,
        HeaderValue::Binary(value) => table.insert(key, Binary::new(value.to_vec()))?,
        HeaderValue::Array(_) | HeaderValue::Table(_) => {}
    }
    Ok(())
}

fn consumer_php_exception(
    error: &rabbit_rs_core::consumer::ConsumerError,
) -> ext_php_rs::prelude::PhpException {
    match consumer_exception::<()>(error) {
        Err(error) => error,
        Ok(()) => unreachable!("consumer_exception always returns an error"),
    }
}

const fn state_name(state: DeliveryState) -> &'static str {
    match state {
        DeliveryState::Pending => "pending",
        DeliveryState::Acked => "acked",
        DeliveryState::Rejected => "rejected",
        DeliveryState::Lost => "lost",
    }
}
