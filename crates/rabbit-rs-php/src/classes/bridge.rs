//! Shared event bridge between the native [`Pool`](super::pool::Pool) and
//! [`Consumer`](super::consumer::Consumer) classes.
//!
//! The bridge owns the PHP callbacks and the last-seen event state so both
//! classes can drain native events on the PHP thread. Callbacks are invoked
//! only on the PHP thread, never from a Rust thread; state mutexes are always
//! released before invocation to prevent deadlock when a callback re-enters
//! the pool (see `tests/Pool/CallbackDeadlockTest.php`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use ext_php_rs::{
    prelude::{PhpException, PhpResult},
    types::Zval,
};
use rabbit_rs_core::{client::ClientPool, recovery::ConnectionState};

use crate::callbacks::CallbackRegistry;

/// Shared event bridge: owns the PHP callbacks and last-seen state so both
/// `Pool` (publish path) and `Consumer` (pop path) can drain native events
/// on the PHP thread. Callbacks are invoked only on the PHP thread, never
/// from a Rust thread; mutexes are released before invocation.
pub(crate) struct EventBridge {
    connection_state_callbacks: CallbackRegistry,
    backpressure_callbacks: CallbackRegistry,
    last_connection_states: Mutex<HashMap<String, (String, i64)>>,
    last_backpressure_total: Mutex<u64>,
    client: Weak<ClientPool>,
}

impl EventBridge {
    /// Creates a bridge bound to the given client pool, wrapped for sharing
    /// between the PHP `Pool` and `Consumer` class instances.
    ///
    /// The pool is held weakly: the bridge never extends the client's
    /// lifetime, and draining after the client was dropped is a no-op.
    #[expect(
        clippy::arc_with_non_send_sync,
        reason = "the bridge is confined to the PHP thread: both owning classes are PHP objects and Zend values are never sent across Rust threads"
    )]
    pub(crate) fn shared(client: &Arc<ClientPool>) -> Arc<Self> {
        Arc::new(Self {
            connection_state_callbacks: CallbackRegistry::new(),
            backpressure_callbacks: CallbackRegistry::new(),
            last_connection_states: Mutex::new(HashMap::new()),
            last_backpressure_total: Mutex::new(0),
            client: Arc::downgrade(client),
        })
    }

    /// Registers a PHP callback invoked when a broker connection state changes.
    ///
    /// Multiple callbacks can be registered (connections sharing one native
    /// pool each register their own); they are cleared together via
    /// [`EventBridge::clear_event_callbacks`].
    ///
    /// # Errors
    ///
    /// Returns a PHP exception if the given value is not callable.
    pub(crate) fn set_connection_state_callback(&self, callback: Zval) -> PhpResult<()> {
        self.connection_state_callbacks.set(callback)
    }

    /// Registers a PHP callback invoked when publisher backpressure is detected.
    ///
    /// Multiple callbacks can be registered; see
    /// [`EventBridge::set_connection_state_callback`].
    ///
    /// # Errors
    ///
    /// Returns a PHP exception if the given value is not callable.
    pub(crate) fn set_backpressure_callback(&self, callback: Zval) -> PhpResult<()> {
        self.backpressure_callbacks.set(callback)
    }

    /// Removes every registered event callback, returning how many were
    /// removed (connection-state and backpressure combined).
    pub(crate) fn clear_event_callbacks(&self) -> usize {
        self.connection_state_callbacks.clear() + self.backpressure_callbacks.clear()
    }

    /// Drains pending native events, invoking the registered PHP callbacks on
    /// the PHP thread.
    ///
    /// Connection-state callbacks fire for every broker whose state changed
    /// since the previous drain; the backpressure callback fires when the
    /// backpressure metric increased. Without a live client (already dropped)
    /// this is a no-op.
    ///
    /// Every registered callback is invoked even when an earlier one throws;
    /// the first thrown exception is rethrown as-is once the drain loop
    /// finishes so the enclosing operation surfaces it instead of silently
    /// destroying it (audit F-17).
    pub(crate) fn drain(&self) {
        let Some(client) = self.client.upgrade() else {
            return;
        };
        let state_error = self.invoke_connection_state_callbacks(&client);
        let backpressure_total = client.metrics_snapshot().backpressure_total;
        let backpressure_error = self.invoke_backpressure_callback(&client, backpressure_total);

        if let Some(exception) = state_error.or(backpressure_error) {
            // The exception object is preserved end-to-end; throwing a real
            // object cannot fail (only abstract/interface classes can).
            let _ = exception.throw();
        }
    }

    fn invoke_connection_state_callbacks(&self, client: &ClientPool) -> Option<PhpException> {
        let states = client.connection_states();

        // Collect all changed states under the lock, then release before invoking
        // callbacks to prevent deadlock when a callback re-enters the pool.
        let pending: Vec<(String, String, i64)> = {
            let mut last_states = self
                .last_connection_states
                .lock()
                .expect("connection state mutex poisoned");
            states
                .iter()
                .filter_map(|(broker, state)| {
                    let (state_name, generation) = connection_state_parts(state);
                    let current = (state_name.clone(), generation);
                    let changed = last_states
                        .get(broker)
                        .is_none_or(|previous| *previous != current);
                    if changed {
                        last_states.insert(broker.clone(), current);
                        Some((broker.clone(), state_name, generation))
                    } else {
                        None
                    }
                })
                .collect()
        }; // Lock released here

        let mut error = None;
        for (broker, state_name, generation) in pending {
            if let Err(callback_error) = self.connection_state_callbacks.invoke_unlocked(&[
                &broker.as_str(),
                &state_name,
                &generation,
            ]) {
                error.get_or_insert(callback_error);
            }
        }
        error
    }

    fn invoke_backpressure_callback(
        &self,
        client: &ClientPool,
        current_backpressure: u64,
    ) -> Option<PhpException> {
        // Determine under lock whether the backpressure metric changed, then
        // release the lock before invoking the callback to prevent deadlock
        // when the callback re-enters the pool.
        let should_invoke = {
            let mut last = self
                .last_backpressure_total
                .lock()
                .expect("backpressure mutex poisoned");
            if current_backpressure > *last {
                *last = current_backpressure;
                true
            } else {
                false
            }
        }; // Lock released here

        if !should_invoke {
            return None;
        }
        let (in_flight, capacity) = client.publisher_utilization();
        let error = self.backpressure_callbacks.invoke_unlocked(&[
            &"global".to_string(),
            &i64::try_from(in_flight).unwrap_or(i64::MAX),
            &i64::try_from(capacity).unwrap_or(i64::MAX),
        ]);
        error.err()
    }
}

fn connection_state_parts(state: &ConnectionState) -> (String, i64) {
    match state {
        ConnectionState::Disconnected => ("disconnected".to_string(), 0),
        ConnectionState::Connecting { attempt } => ("connecting".to_string(), i64::from(*attempt)),
        ConnectionState::Ready { generation } => (
            "ready".to_string(),
            i64::try_from(*generation).unwrap_or(i64::MAX),
        ),
        ConnectionState::Recovering {
            attempt,
            retry_in: _,
            reason: _,
        } => ("recovering".to_string(), i64::from(*attempt)),
        ConnectionState::FailedPermanent { kind: _, reason: _ } => {
            ("failed_permanent".to_string(), 0)
        }
        ConnectionState::Closed => ("closed".to_string(), 0),
    }
}
