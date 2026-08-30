#![expect(
    non_snake_case,
    reason = "ext-php-rs preserves parameter identifiers for PHP named arguments"
)]

use std::sync::atomic::{AtomicBool, Ordering};

use super::{
    bridge::EventBridge,
    delivery::Delivery,
    exception::{consumer_exception, rabbit_exception},
};
use ext_php_rs::{
    boxed::ZBox,
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
    types::{ZendClassObject, ZendHashTable},
};
use rabbit_rs_core::consumer::ConsumerHandle;
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

        // Fast path: check the flume buffer without block_on.
        if let Some(delivery) = self
            .handle
            .try_next()
            .map_err(|error| consumer_php_exception(&error))?
        {
            return Ok(Some(Delivery::new(
                delivery,
                self.runtime.clone(),
                self.pid,
            )));
        }

        // Slow path: drain native events (connection state, backpressure)
        // before blocking on the async runtime with timeout.
        self.bridge.drain();
        let timeout = u64::try_from(timeoutMs).map_err(|_| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                "timeoutMs must be a non-negative integer".to_owned(),
            )
        })?;
        match self.runtime.block_on(async {
            time::timeout(
                std::time::Duration::from_millis(timeout),
                self.handle.next(),
            )
            .await
        }) {
            Ok(Ok(delivery)) => Ok(Some(Delivery::new(
                delivery,
                self.runtime.clone(),
                self.pid,
            ))),
            Ok(Err(error)) => consumer_exception(&error),
            Err(_) => Ok(None),
        }
    }

    /// Attempts to return the next delivery without blocking.
    ///
    /// Returns `Some(Delivery)` when one is available in the buffer,
    /// or `None` when the buffer is empty. No timeout, no async wait.
    pub fn tryNext(&self) -> PhpResult<Option<Delivery>> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::tryNext")?;
        self.drain_publish_buffer()?;
        match self.handle.try_next() {
            Ok(Some(delivery)) => Ok(Some(Delivery::new(
                delivery,
                self.runtime.clone(),
                self.pid,
            ))),
            Ok(None) => {
                // Buffer empty: drain native events before returning, mirroring
                // the drain performed before the blocking wait in next().
                self.bridge.drain();
                Ok(None)
            }
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

        let max = usize::try_from(max).map_err(|_| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                "max must be a non-negative integer".to_owned(),
            )
        })?;

        // Fast path: drain the flume buffer without block_on.
        let batch = self
            .handle
            .try_next_batch(max)
            .map_err(|error| consumer_php_exception(&error))?;
        if !batch.is_empty() {
            return batch
                .into_iter()
                .map(|delivery| Ok(Delivery::new(delivery, self.runtime.clone(), self.pid)))
                .collect();
        }

        // Slow path: drain native events (connection state, backpressure)
        // before blocking on the async runtime with timeout, then drain
        // whatever is available.
        self.bridge.drain();
        let timeout = u64::try_from(timeoutMs).map_err(|_| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                "max must be a non-negative integer".to_owned(),
            )
        })?;
        match self.runtime.block_on(async {
            time::timeout(
                std::time::Duration::from_millis(timeout),
                self.handle.next(),
            )
            .await
        }) {
            Ok(Ok(delivery)) => {
                let mut deliveries = vec![Delivery::new(delivery, self.runtime.clone(), self.pid)];
                let more = if max > 1 {
                    self.handle
                        .try_next_batch(max.saturating_sub(1))
                        .map_err(|error| consumer_php_exception(&error))?
                } else {
                    Vec::new()
                };
                deliveries.extend(
                    more.into_iter()
                        .map(|d| Delivery::new(d, self.runtime.clone(), self.pid)),
                );
                Ok(deliveries)
            }
            Ok(Err(error)) => consumer_exception(&error),
            Err(_) => Ok(Vec::new()),
        }
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
            Err(rabbit_rs_core::consumer::SettleError::ChannelFull) => {
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
            Err(rabbit_rs_core::consumer::SettleError::Closed) => {
                rabbit_exception("consumer set is closed")
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
            let zval = value.dereference();
            let object =
                zval.object().ok_or_else(|| {
                    ext_php_rs::prelude::PhpException::from_class::<
                        super::exception::RabbitRsException,
                    >("ackBatch expects an array of Delivery objects".to_owned())
                })?;
            let class_obj: &ZendClassObject<Delivery> =
                object.extract().map_err(|_| {
                    ext_php_rs::prelude::PhpException::from_class::<
                        super::exception::RabbitRsException,
                    >("ackBatch expects an array of Delivery objects".to_owned())
                })?;
            let delivery =
                class_obj.obj.as_ref().ok_or_else(|| {
                    ext_php_rs::prelude::PhpException::from_class::<
                        super::exception::RabbitRsException,
                    >(
                        "ackBatch encountered an uninitialized Delivery object".to_owned()
                    )
                })?;
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
        self.runtime.block_on(self.handle.close()).map_err(|error| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                error.to_string(),
            )
        })
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
}

fn consumer_php_exception(
    error: &rabbit_rs_core::consumer::ConsumerError,
) -> ext_php_rs::prelude::PhpException {
    match consumer_exception::<()>(error) {
        Err(error) => error,
        Ok(()) => unreachable!("consumer_exception always returns an error"),
    }
}
