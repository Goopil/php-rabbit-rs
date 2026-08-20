#![expect(
    non_snake_case,
    reason = "ext-php-rs preserves parameter identifiers for PHP named arguments"
)]

use std::sync::atomic::{AtomicBool, Ordering};

use super::{
    delivery::Delivery,
    exception::{consumer_exception, rabbit_exception},
};
use ext_php_rs::{
    convert::IntoZval,
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
    types::{ZendCallable, Zval},
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
}

#[php_impl]
impl Consumer {
    /// Returns the next delivery within the requested timeout.
    pub fn next(&self, timeoutMs: i64) -> PhpResult<Option<Delivery>> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::next")?;
        // Fast path: check the flume buffer without block_on.
        match self.handle.try_next() {
            Ok(Some(delivery)) => {
                return Ok(Some(Delivery::new(
                    delivery,
                    self.runtime.clone(),
                    self.pid,
                )));
            }
            Ok(None) => {}
            Err(error) => return consumer_exception(&error),
        }
        // Slow path: block on the async runtime with timeout.
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
        match self.handle.try_next() {
            Ok(Some(delivery)) => Ok(Some(Delivery::new(
                delivery,
                self.runtime.clone(),
                self.pid,
            ))),
            Ok(None) => Ok(None),
            Err(error) => consumer_exception(&error),
        }
    }

    /// Processes messages by calling the given callback for each delivery.
    ///
    /// Returns the number of messages processed. The loop stops early when:
    /// - `count` messages have been processed (if `count > 0`)
    /// - The callback returns `false`
    /// - The total `timeoutMs` deadline elapses
    ///
    /// The fast path uses `try_next()` (sub-microsecond, no async crossing).
    /// The slow path blocks on `next()` with the remaining time budget.
    #[php(defaults(count = 0, timeoutMs = 1000))]
    pub fn consume(&self, handler: &Zval, count: i64, timeoutMs: i64) -> PhpResult<i64> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::consume")?;

        let max_count = if count <= 0 {
            None
        } else {
            Some(u64::try_from(count).unwrap_or(u64::MAX))
        };
        let timeout_ms = u64::try_from(timeoutMs).map_err(|_| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                "timeoutMs must be a non-negative integer".to_owned(),
            )
        })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

        let callable = ZendCallable::new(handler).map_err(|_| {
            ext_php_rs::prelude::PhpException::from_class::<super::exception::RabbitRsException>(
                "handler must be a callable PHP value".to_owned(),
            )
        })?;

        let mut processed: u64 = 0;

        while max_count.is_none_or(|max| processed < max) {
            if std::time::Instant::now() >= deadline {
                break;
            }

            // Fast path: check the flume buffer without block_on.
            let delivery = match self.handle.try_next() {
                Ok(Some(delivery)) => Some(delivery),
                Ok(None) => None,
                Err(error) => return consumer_exception(&error),
            };

            let delivery = if let Some(d) = delivery {
                d
            } else {
                // Slow path: wait for one delivery with remaining timeout.
                let remaining = deadline.duration_since(std::time::Instant::now());
                match self
                    .runtime
                    .block_on(async { time::timeout(remaining, self.handle.next()).await })
                {
                    Ok(Ok(d)) => d,
                    Ok(Err(error)) => return consumer_exception(&error),
                    Err(_) => break, // timeout elapsed
                }
            };

            let php_delivery = Delivery::new(delivery, self.runtime.clone(), self.pid);
            let zval =
                php_delivery.into_zval(false).map_err(|error| {
                    ext_php_rs::prelude::PhpException::from_class::<
                        super::exception::RabbitRsException,
                    >(error.to_string())
                })?;
            let result =
                callable.try_call(vec![&zval]).map_err(|error| {
                    ext_php_rs::prelude::PhpException::from_class::<
                        super::exception::RabbitRsException,
                    >(error.to_string())
                })?;
            processed += 1;

            if let Some(false) = result.bool() {
                break;
            }
        }

        Ok(i64::try_from(processed).unwrap_or(i64::MAX))
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
    pub(crate) fn new(handle: ConsumerHandle, runtime: Handle, pid: u32) -> Self {
        Self {
            handle,
            runtime,
            pid,
            closed: AtomicBool::new(false),
        }
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
