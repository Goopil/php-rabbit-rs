//! Storage and invocation of PHP callbacks for native event signaling.
//!
//! Callbacks are stored as `Zval` values containing PHP callables. They are
//! only ever accessed on the PHP thread during synchronous operations (e.g.,
//! `Pool::stats()`). They are never sent across async boundaries or Rust
//! threads, satisfying the constraint that Zend values must not be retained
//! in Rust threads.

use std::sync::Mutex;

use ext_php_rs::{
    convert::IntoZvalDyn,
    prelude::{PhpException, PhpResult},
    types::{ZendCallable, Zval},
};

use crate::classes::exception::RabbitRsException;

/// Container for an optional PHP callable, protected by a mutex.
///
/// The callable is stored as an owned `Zval` and converted to a `ZendCallable`
/// at invocation time. The mutex is only held briefly during registration and
/// invocation, never across `block_on` or async boundaries.
pub struct CallbackSlot(Mutex<Option<Zval>>);

impl CallbackSlot {
    /// Creates an empty callback slot.
    #[must_use]
    pub fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// Stores a PHP callable, replacing any previously registered callback.
    ///
    /// # Errors
    ///
    /// Returns a PHP exception if the given value is not callable.
    pub fn set(&self, callable: Zval) -> PhpResult<()> {
        if !callable.is_callable() {
            return Err(PhpException::from_class::<RabbitRsException>(
                "callback must be a callable PHP value".to_owned(),
            ));
        }
        let mut slot = self.0.lock().expect("callback mutex poisoned");
        *slot = Some(callable);
        Ok(())
    }

    /// Invokes the stored callback with the given arguments if one is registered.
    ///
    /// This method is called on the PHP thread. It does not send the callable
    /// across any async boundary. The mutex is held for the duration of the
    /// PHP callback invocation, which is safe because the callback runs on the
    /// same thread and cannot re-enter `invoke` recursively.
    pub fn invoke(&self, params: Vec<&dyn IntoZvalDyn>) -> PhpResult<()> {
        let slot = self.0.lock().expect("callback mutex poisoned");
        let Some(ref callable) = *slot else {
            return Ok(());
        };
        let callback = ZendCallable::new(callable).map_err(|_| {
            PhpException::from_class::<RabbitRsException>(
                "stored callback is no longer callable".to_owned(),
            )
        })?;
        callback
            .try_call(params)
            .map(|_| ())
            .map_err(|error| PhpException::from_class::<RabbitRsException>(error.to_string()))
    }
}

impl Default for CallbackSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CallbackSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CallbackSlot")
            .field("set", &self.0.lock().is_ok_and(|slot| slot.is_some()))
            .finish_non_exhaustive()
    }
}
