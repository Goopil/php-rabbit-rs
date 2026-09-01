//! Storage and invocation of PHP callbacks for native event signaling.
//!
//! Callbacks are stored as `Zval` values containing PHP callables. They are
//! only ever accessed on the PHP thread during synchronous operations (e.g.,
//! `Pool::stats()`). They are never sent across async boundaries or Rust
//! threads, satisfying the constraint that Zend values must not be retained
//! in Rust threads.

use std::sync::Mutex;

use ext_php_rs::{
    convert::{IntoZval, IntoZvalDyn},
    error::Error,
    prelude::{PhpException, PhpResult},
    types::{ZendCallable, Zval},
};

/// Registry of PHP callables, protected by a mutex.
///
/// Multiple callables can be registered: connections sharing one native pool
/// (e.g. two Laravel connections with the same fingerprint) each register
/// their own callbacks and all of them fire (audit F-17). The mutex is only
/// held briefly during registration and invocation, never across `block_on`
/// or async boundaries.
pub struct CallbackRegistry(Mutex<Vec<Zval>>);

impl CallbackRegistry {
    /// Creates an empty callback registry.
    #[must_use]
    pub fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    /// Registers a PHP callable, keeping any previously registered callbacks.
    ///
    /// # Errors
    ///
    /// Returns a PHP exception if the given value is not callable.
    pub fn set(&self, callable: Zval) -> PhpResult<()> {
        if !callable.is_callable() {
            return Err(crate::classes::exception::rabbit_exception_message(
                "callback must be a callable PHP value".to_owned(),
            ));
        }
        self.0
            .lock()
            .expect("callback registry mutex poisoned")
            .push(callable);
        Ok(())
    }

    /// Removes every registered callback, returning how many were removed.
    pub fn clear(&self) -> usize {
        self.0
            .lock()
            .expect("callback registry mutex poisoned")
            .drain(..)
            .count()
    }

    /// Invokes every registered callback without holding the internal mutex.
    ///
    /// The callable `Zval`s are shallow-cloned under the mutex, the mutex is
    /// released, and only then are the PHP callbacks invoked. This prevents
    /// deadlocks when a callback re-enters the pool (e.g., calling `stats()`
    /// which needs to acquire other mutexes that the caller may hold).
    ///
    /// Every registered callback is invoked even when an earlier one fails;
    /// the first failure is returned so the caller can rethrow the original
    /// exception instead of silently destroying it (audit F-17).
    pub fn invoke_unlocked(&self, params: &[&dyn IntoZvalDyn]) -> PhpResult<()> {
        let callables = {
            let registry = self.0.lock().expect("callback registry mutex poisoned");
            registry.iter().map(Zval::shallow_clone).collect::<Vec<_>>()
        }; // Lock released here

        let mut error: Option<PhpException> = None;
        for callable_zval in callables {
            let Ok(callback) = ZendCallable::new(&callable_zval) else {
                error.get_or_insert_with(|| {
                    crate::classes::exception::rabbit_exception_message(
                        "stored callback is no longer callable".to_owned(),
                    )
                });
                continue;
            };
            match callback.try_call(params.to_vec()) {
                Ok(_) => {}
                // Preserve the thrown exception object so it is rethrown as-is
                // instead of being stringified and destroyed.
                Err(Error::Exception(exception)) => {
                    let mut object = Zval::new();
                    if exception.set_zval(&mut object, false).is_ok() {
                        let mut thrown = crate::classes::exception::rabbit_exception_message(
                            "registered callback threw an exception".to_owned(),
                        );
                        thrown.set_object(Some(object));
                        error.get_or_insert(thrown);
                    }
                }
                Err(other) => {
                    error.get_or_insert_with(|| {
                        crate::classes::exception::rabbit_exception_message(other.to_string())
                    });
                }
            }
        }
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl std::fmt::Debug for CallbackRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CallbackRegistry")
            .field(
                "registered",
                &self.0.lock().map_or(0, |registry| registry.len()),
            )
            .finish_non_exhaustive()
    }
}
