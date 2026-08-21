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
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
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
    ///
    /// The fast path checks the lock-free buffer without crossing into the
    /// async runtime. The slow path blocks on the async runtime with the
    /// specified timeout.
    pub fn next(&self, timeoutMs: i64) -> PhpResult<Option<Delivery>> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::next")?;

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

fn consumer_php_exception(
    error: &rabbit_rs_core::consumer::ConsumerError,
) -> ext_php_rs::prelude::PhpException {
    match consumer_exception::<()>(error) {
        Err(error) => error,
        Ok(()) => unreachable!("consumer_exception always returns an error"),
    }
}
