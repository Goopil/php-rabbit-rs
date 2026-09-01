#![expect(
    non_snake_case,
    reason = "ext-php-rs preserves parameter identifiers for PHP named arguments"
)]

use std::sync::atomic::{AtomicBool, Ordering};

use super::{
    bridge::EventBridge,
    delivery::Delivery,
    exception::{
        consumer_exception, consumer_exception_message, rabbit_exception, rabbit_exception_message,
    },
};
use ext_php_rs::{
    boxed::ZBox,
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
    types::{ZendClassObject, ZendHashTable, Zval},
};
use rabbit_rs_core::consumer::{ConsumerHandle, Delivery as NativeDelivery};
use tokio::{runtime::Handle, time};

/// Native consumer for an aggregated subscription profile.
#[php_class]
#[php(name = "Goopil\\RabbitRs\\Consumer")]
#[php(flags = ClassFlags::Final)]
pub struct Consumer {
    handle: ConsumerHandle,
    runtime: Handle,
    pid: u32,
    closed: AtomicBool,
    bridge: std::sync::Arc<EventBridge>,
    publish_buffer: std::sync::Arc<super::publish_buffer::PublishBuffer>,
}

#[php_impl]
impl Consumer {
    /// Returns the next delivery within the requested timeout.
    ///
    /// The fast path checks the lock-free buffer without crossing into the
    /// async runtime. The slow path blocks on the async runtime with the
    /// specified timeout.
    pub fn next(&self, timeoutMs: i64) -> PhpResult<Option<Delivery>> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::next")?;
        self.drain_publish_buffer()?;
        // Drain before the fast path too: a delivery being immediately
        // available must not starve the state/backpressure callbacks (audit
        // F-21). The drain is cheap and idempotent.
        self.bridge.drain();

        // Fast path: check the flume buffer without block_on.
        if let Some(delivery) = self
            .handle
            .try_next()
            .map_err(|error| consumer_exception_message(&error))?
        {
            return Ok(Some(self.wrap_delivery(delivery)));
        }

        // Slow path: block on the async runtime with timeout.
        Ok(self
            .await_delivery(timeoutMs)?
            .map(|d| self.wrap_delivery(d)))
    }

    /// Attempts to return the next delivery without blocking.
    ///
    /// Returns `Some(Delivery)` when one is available in the buffer,
    /// or `None` when the buffer is empty. No timeout, no async wait.
    pub fn tryNext(&self) -> PhpResult<Option<Delivery>> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::tryNext")?;
        self.drain_publish_buffer()?;
        self.bridge.drain();
        match self.handle.try_next() {
            Ok(Some(delivery)) => Ok(Some(self.wrap_delivery(delivery))),
            Ok(None) => Ok(None),
            Err(error) => consumer_exception(&error),
        }
    }

    /// Drains up to `max` deliveries from the buffer in one call.
    ///
    /// The fast path checks the lock-free buffer without crossing into the
    /// async runtime. When the buffer is empty, the slow path blocks on the
    /// async runtime with the specified timeout, then drains whatever is
    /// available. `max` is clamped to `1..=256`.
    pub fn nextBatch(&self, max: i64, timeoutMs: i64) -> PhpResult<Vec<Delivery>> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::nextBatch")?;
        self.drain_publish_buffer()?;
        // Drain before the fast path too, mirroring next() (audit F-21).
        self.bridge.drain();

        let max = usize::try_from(max).map_err(|_| {
            rabbit_exception_message("max must be a non-negative integer".to_owned())
        })?;

        // Fast path: drain the flume buffer without block_on.
        let batch = self
            .handle
            .try_next_batch(max)
            .map_err(|error| consumer_exception_message(&error))?;
        if !batch.is_empty() {
            return Ok(batch
                .into_iter()
                .map(|delivery| self.wrap_delivery(delivery))
                .collect());
        }

        // Slow path: block on one delivery, then drain whatever else is
        // available up to the remaining batch size.
        let Some(delivery) = self.await_delivery(timeoutMs)? else {
            return Ok(Vec::new());
        };
        let mut deliveries = vec![self.wrap_delivery(delivery)];
        if max > 1 {
            let more = self
                .handle
                .try_next_batch(max.saturating_sub(1))
                .map_err(|error| consumer_exception_message(&error))?;
            deliveries.extend(more.into_iter().map(|d| self.wrap_delivery(d)));
        }
        Ok(deliveries)
    }

    /// Acknowledges a contiguous prefix of deliveries up to and including the
    /// given delivery using a single AMQP `basic.ack` with `multiple=true`.
    ///
    /// Fire-and-forget: enqueues the command and returns immediately.
    pub fn ackThrough(&self, delivery: &Delivery) -> PhpResult<()> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::ackThrough")?;
        match self
            .handle
            .try_settle_through(delivery.inner.inner_token().clone())
        {
            Ok(()) => Ok(()),
            Err(rabbit_rs_core::consumer::SettlementErrorKind::ChannelFull) => {
                for _ in 0..64 {
                    std::thread::yield_now();
                    if self
                        .handle
                        .try_settle_through(delivery.inner.inner_token().clone())
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                rabbit_exception("settlement channel full after backpressure timeout")
            }
            Err(rabbit_rs_core::consumer::SettlementErrorKind::Closed) => {
                rabbit_exception("consumer set is closed")
            }
            Err(rabbit_rs_core::consumer::SettlementErrorKind::AlreadySettled) => {
                rabbit_exception("delivery is already settled")
            }
        }
    }

    /// Acknowledges a batch of deliveries across potentially different channels.
    ///
    /// Fire-and-forget: enqueues each settlement command without blocking.
    /// Bounded to 256 deliveries per call.
    pub fn ackBatch(&self, deliveries: &ZendHashTable) -> PhpResult<()> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::ackBatch")?;

        for (count, (_, value)) in deliveries.into_iter().enumerate() {
            if count >= 256 {
                return rabbit_exception("ackBatch: maximum 256 deliveries per call");
            }
            let delivery = delivery_from_zval(value.dereference())?;
            delivery.settle_with_backpressure(rabbit_rs_core::consumer::Delivery::try_ack)?;
        }
        Ok(())
    }

    /// Drains settlement errors that have surfaced asynchronously since the
    /// last call. Returns an array of error hashes, each containing
    /// `delivery_tag`, `subscription`, `error_kind`, and `message`.
    pub fn drainErrors(&self) -> PhpResult<ZBox<ZendHashTable>> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::drainErrors")?;
        let errors = self.handle.drain_errors();
        let mut table = ZendHashTable::new();
        for (i, error) in errors.iter().enumerate() {
            let mut entry = ZendHashTable::new();
            entry.insert(
                "delivery_tag",
                i64::try_from(error.delivery_tag).unwrap_or(i64::MAX),
            )?;
            entry.insert("subscription", error.subscription.as_str())?;
            entry.insert("error_kind", format!("{:?}", error.kind))?;
            entry.insert("message", error.message.clone())?;
            table.insert(i, entry)?;
        }
        Ok(table)
    }

    /// Closes this consumer handle.
    pub fn close(&self) -> PhpResult<()> {
        if self.pid != std::process::id() {
            return rabbit_exception("cannot close a consumer inherited across fork");
        }
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.runtime
            .block_on(self.handle.close())
            .map_err(|error| rabbit_exception_message(error.to_string()))
    }

    /// Closes the consumer handle when PHP garbage-collects the object.
    ///
    /// This is a best-effort safety net that prevents AMQP channel leaks in
    /// long-lived processes (Octane, daemons) when `close()` is never called
    /// explicitly. The underlying `ConsumerHandle::Drop` also sends `Close` to
    /// the actor so channels are closed even if PHP never calls `close()`.
    pub fn __destruct(&self) {
        if self.pid != std::process::id() {
            return;
        }
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.runtime.block_on(self.handle.close());
    }
}

impl Consumer {
    pub(crate) fn new(
        handle: ConsumerHandle,
        runtime: Handle,
        pid: u32,
        bridge: std::sync::Arc<EventBridge>,
        publish_buffer: std::sync::Arc<super::publish_buffer::PublishBuffer>,
    ) -> Self {
        Self {
            handle,
            runtime,
            pid,
            closed: AtomicBool::new(false),
            bridge,
            publish_buffer,
        }
    }

    /// Drains the shared publish buffer before the consumer observes broker
    /// state, so publications accepted earlier are never trapped in process
    /// memory while the consumer waits (see [`super::publish_buffer`]).
    fn drain_publish_buffer(&self) -> PhpResult<()> {
        self.publish_buffer.flush_nonempty()
    }

    fn ensure_open(&self, operation: &str) -> PhpResult<()> {
        if self.pid != std::process::id() {
            return rabbit_exception(format!(
                "{operation} cannot use a consumer inherited across fork"
            ));
        }
        if self.closed.load(Ordering::Acquire) {
            return rabbit_exception(format!("{operation} cannot use a closed consumer"));
        }
        Ok(())
    }

    /// Wraps a native delivery into the PHP-facing type with this handle's pid.
    fn wrap_delivery(&self, delivery: NativeDelivery) -> Delivery {
        Delivery::new(delivery, self.pid)
    }

    /// Drains native events, then blocks on the async runtime for the next
    /// delivery with the given timeout in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns a PHP exception when the consumer reports a consumer error or
    /// `timeoutMs` is negative.
    fn await_delivery(&self, timeout_ms: i64) -> PhpResult<Option<NativeDelivery>> {
        self.bridge.drain();
        let timeout = u64::try_from(timeout_ms).map_err(|_| {
            rabbit_exception_message("timeoutMs must be a non-negative integer".to_owned())
        })?;
        match self.runtime.block_on(async {
            time::timeout(
                std::time::Duration::from_millis(timeout),
                self.handle.next(),
            )
            .await
        }) {
            Ok(Ok(delivery)) => Ok(Some(delivery)),
            Ok(Err(error)) => Err(consumer_exception_message(&error)),
            Err(_) => Ok(None),
        }
    }
}

/// Extracts a [`Delivery`] reference from a value in a delivery array.
///
/// # Errors
///
/// Returns a PHP exception when the value is not a constructed `Delivery`
/// object.
fn delivery_from_zval(value: &Zval) -> PhpResult<&Delivery> {
    let object = value.dereference().object().ok_or_else(|| {
        rabbit_exception_message("ackBatch expects an array of Delivery objects".to_owned())
    })?;
    let class_obj: &ZendClassObject<Delivery> = object.extract().map_err(|_| {
        rabbit_exception_message("ackBatch expects an array of Delivery objects".to_owned())
    })?;
    class_obj.obj.as_ref().ok_or_else(|| {
        rabbit_exception_message("ackBatch encountered an uninitialized Delivery object".to_owned())
    })
}
