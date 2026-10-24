#![expect(
    non_snake_case,
    clippy::doc_markdown,
    reason = "ext-php-rs preserves parameter identifiers for PHP named arguments, and PHP docblock array shapes keep snake_case keys"
)]

use std::sync::atomic::{AtomicBool, Ordering};

use super::exception::{rabbit_exception, rabbit_exception_message};
use ext_php_rs::{
    boxed::ZBox,
    convert::IntoZval,
    error::Result,
    flags::{ClassFlags, DataType},
    prelude::{PhpResult, php_class, php_impl},
    types::{ArrayKey, ZendHashTable, ZendStr, Zval},
};
use rabbit_rs_core::consumer::{Delivery as NativeDelivery, DeliveryState, SettlementErrorKind};
use rabbit_rs_core::transport::HeaderValue;

/// Native delivery and its acknowledgement token.
///
/// Obtained via `Consumer::next()`, `Consumer::tryNext()`, or
/// `Consumer::nextBatch()`; not constructible from PHP.
#[php_class]
#[php(name = "Goopil\\RabbitRs\\Delivery")]
#[php(flags = ClassFlags::Final)]
pub struct Delivery {
    pub(crate) inner: NativeDelivery,
    pid: u32,
}

#[php_impl]
impl Delivery {
    /// Returns the binary-safe delivery payload.
    pub fn payload(&self) -> PhpResult<PhpString> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::payload")?;
        Ok(PhpString::from_bytes(self.inner.payload.as_ref()))
    }

    /// Returns delivery metadata as a PHP array.
    ///
    /// @return array{message_id: string, correlation_id?: string,
    ///   subscription: string, attempts: int, state: string,
    ///   headers: array<string, bool|int|float|string|array|null>}
    ///
    /// Nested broker structures (e.g. dead-letter `x-death` tables and field
    /// arrays) round-trip as nested PHP arrays. Binary header values become
    /// byte strings, and AMQP decimal values are dropped with a PHP notice
    /// (PHP has no decimal scalar).
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
            insert_header(&mut headers, key.as_str(), value)?;
        }
        metadata.insert("headers", headers)?;
        Ok(metadata)
    }

    /// Returns the AMQP delivery tag.
    pub fn deliveryTag(&self) -> PhpResult<i64> {
        self.ensure_current_process("Goopil\\RabbitRs\\Delivery::deliveryTag")?;
        i64::try_from(self.inner.delivery_tag())
            .map_err(|_| rabbit_exception_message("delivery tag exceeds i64 range".to_owned()))
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
            rabbit_exception_message("delayMs must be a non-negative integer".to_owned())
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
    pub(crate) fn new(inner: NativeDelivery, pid: u32) -> Self {
        Self { inner, pid }
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
                spin_settle(|| try_settle(&self.inner).is_ok())
            }
        }
    }
}

/// PHP string view of Rust bytes: exactly one memcpy.
///
/// `Binary<u8>` costs two copies (Bytes→Vec, then Vec→zend_string). This newtype
/// builds the `zend_string` directly from the byte slice and hands ownership to
/// the zval, so the bytes are copied once into the final ZendMM allocation.
/// `IntoZval::TYPE` must stay `String`/non-nullable: the `#[php_impl]` macro
/// writes it into the runtime arginfo, and a `Zval` return would downgrade the
/// generated signature to `mixed`.
///
/// `pub` only because `#[php_impl]` exposes `Delivery::payload` publicly;
/// the field and constructor stay crate-private.
pub struct PhpString(ZBox<ZendStr>);

impl PhpString {
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        Self(ZendStr::new(bytes, false))
    }
}

/// `set_zval` ignores the `persistent` flag and always produces a
/// request-bound string (ZendMM request heap). Every current caller passes
/// `false`; a future persistent-heap caller must not use this type.
impl IntoZval for PhpString {
    const TYPE: DataType = DataType::String;
    const NULLABLE: bool = false;

    fn set_zval(self, zval: &mut Zval, _persistent: bool) -> Result<()> {
        zval.set_zend_string(self.0);
        Ok(())
    }
}

/// Bounded spin for a full settlement command channel: yields the PHP thread
/// up to 64 times while the actor drains, then raises. Shared by the
/// delivery settlements and `Consumer::ackThrough`.
pub(crate) fn spin_settle(mut retry: impl FnMut() -> bool) -> PhpResult<()> {
    for _ in 0..64 {
        std::thread::yield_now();
        if retry() {
            return Ok(());
        }
    }
    rabbit_exception("settlement channel full after backpressure timeout")
}

/// Inserts one header entry, exposing AMQP field arrays/tables as nested PHP
/// arrays so dead-letter metadata such as `x-death` stays visible to PHP
/// (audit F-22).
fn insert_header<'k, K>(table: &mut ZendHashTable, key: K, value: &HeaderValue) -> PhpResult<()>
where
    K: Into<ArrayKey<'k>> + Copy,
{
    match value {
        HeaderValue::Void => table.insert(key, Zval::null())?,
        HeaderValue::Boolean(value) => table.insert(key, *value)?,
        HeaderValue::Integer(value) => table.insert(key, *value)?,
        HeaderValue::Double(value) => table.insert(key, value.get())?,
        HeaderValue::Binary(value) => table.insert(key, PhpString::from_bytes(value.as_ref()))?,
        HeaderValue::Array(values) => {
            let mut nested = ZendHashTable::new();
            for (index, value) in values.iter().enumerate() {
                insert_header(&mut nested, i64::try_from(index).unwrap_or(i64::MAX), value)?;
            }
            table.insert(key, nested)?;
        }
        HeaderValue::Table(values) => {
            let mut nested = ZendHashTable::new();
            for (name, value) in values {
                insert_header(&mut nested, name.as_str(), value)?;
            }
            table.insert(key, nested)?;
        }
        HeaderValue::Decimal { .. } => notice_dropped_decimal(),
    }
    Ok(())
}

/// Emits a once-per-process PHP notice when a decimal header is dropped:
/// PHP has no decimal scalar, so the value cannot be represented faithfully.
fn notice_dropped_decimal() {
    static DECIMAL_NOTICE_SENT: AtomicBool = AtomicBool::new(false);
    if !DECIMAL_NOTICE_SENT.swap(true, Ordering::AcqRel) {
        ext_php_rs::error::php_error(
            &ext_php_rs::flags::ErrorType::Notice,
            "rabbit_rs: AMQP decimal header values are not supported by PHP and were dropped",
        );
    }
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
