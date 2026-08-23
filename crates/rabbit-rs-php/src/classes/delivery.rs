#![expect(
    non_snake_case,
    reason = "ext-php-rs preserves parameter identifiers for PHP named arguments"
)]

use super::exception::rabbit_exception;
use ext_php_rs::{
    binary::Binary,
    boxed::ZBox,
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
    types::{ZendHashTable, Zval},
};
use rabbit_rs_core::consumer::{Delivery as NativeDelivery, DeliveryState, SettlementErrorKind};
use rabbit_rs_core::transport::HeaderValue;
use tokio::runtime::Handle;

/// Native delivery and its acknowledgement token.
#[php_class]
#[php(name = "Goopil\\RabbitRs\\Delivery")]
#[php(flags = ClassFlags::Final)]
pub struct Delivery {
    pub(crate) inner: NativeDelivery,
    #[allow(dead_code)]
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

    /// Returns the AMQP delivery tag.
    pub fn deliveryTag(&self) -> PhpResult<i64> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::deliveryTag")?;
        i64::try_from(self.inner.delivery_tag()).map_err(|_| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                "delivery tag exceeds i64 range".to_owned(),
            )
        })
    }

    /// Acknowledges the delivery (fire-and-forget with bounded backpressure).
    pub fn ack(&self) -> PhpResult<()> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::ack")?;
        if self.inner.state() == DeliveryState::AutoAcked {
            return rabbit_exception("cannot ack an auto-acked delivery");
        }
        self.settle_with_backpressure(NativeDelivery::try_ack)
    }

    /// Releases the delivery immediately or after a delay (fire-and-forget).
    #[php(defaults(delayMs = 0))]
    pub fn release(&self, delayMs: i64) -> PhpResult<()> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::release")?;
        if self.inner.state() == DeliveryState::AutoAcked {
            return rabbit_exception("cannot release an auto-acked delivery");
        }
        let delay = u64::try_from(delayMs).map_err(|_| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                "delayMs must be a non-negative integer".to_owned(),
            )
        })?;
        self.settle_with_backpressure(|del| {
            del.try_release(std::time::Duration::from_millis(delay))
        })
    }

    /// Rejects the delivery with optional requeueing (fire-and-forget).
    #[php(defaults(requeue = false))]
    pub fn reject(&self, requeue: bool) -> PhpResult<()> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::reject")?;
        if self.inner.state() == DeliveryState::AutoAcked {
            return rabbit_exception("cannot reject an auto-acked delivery");
        }
        self.settle_with_backpressure(|del| del.try_reject(requeue))
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

    /// Fire-and-forget settlement with bounded backpressure.
    ///
    /// Fast path: `try_settle` uses `try_send` and returns immediately.
    /// When the command channel is full, spin-yield up to 64 times.
    /// If all spin-yields fail, returns a PHP exception.
    pub(crate) fn settle_with_backpressure(
        &self,
        try_settle: impl Fn(&NativeDelivery) -> Result<(), SettlementErrorKind>,
    ) -> PhpResult<()> {
        match try_settle(&self.inner) {
            Ok(()) => Ok(()),
            Err(SettlementErrorKind::AlreadySettled) => {
                rabbit_exception("delivery token is already terminal or transitioning")
            }
            Err(SettlementErrorKind::Closed) => rabbit_exception("consumer set is closed"),
            Err(SettlementErrorKind::ChannelFull) => {
                for _ in 0..64 {
                    std::thread::yield_now();
                    if try_settle(&self.inner).is_ok() {
                        return Ok(());
                    }
                }
                rabbit_exception("settlement channel full after backpressure timeout")
            }
        }
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

const fn state_name(state: DeliveryState) -> &'static str {
    match state {
        DeliveryState::Pending => "pending",
        DeliveryState::Acked => "acked",
        DeliveryState::Rejected => "rejected",
        DeliveryState::Lost => "lost",
        DeliveryState::AutoAcked => "auto_acked",
    }
}
