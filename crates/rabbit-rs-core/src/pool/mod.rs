use std::{
    fmt,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::runtime::Handle;

use crate::{client::ClientPool, config::ValidatedConfig};

pub mod connection_actor;
pub mod key;

pub use key::ConnectionKey;

/// Process-local handle representing one reusable connection pool.
pub struct ConnectionHandle {
    key: ConnectionKey,
    closed: AtomicBool,
    runtime: Handle,
    client: OnceLock<Arc<ClientPool>>,
}

impl fmt::Debug for ConnectionHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionHandle")
            .field("key", &self.key)
            .field("closed", &self.is_closed())
            .field("client_initialized", &self.client.get().is_some())
            .finish_non_exhaustive()
    }
}

impl ConnectionHandle {
    pub(crate) fn new(key: ConnectionKey, runtime: Handle) -> Self {
        Self {
            key,
            closed: AtomicBool::new(false),
            runtime,
            client: OnceLock::new(),
        }
    }

    /// Returns or initializes the production client attached to this shared handle.
    #[must_use]
    pub fn client(&self, config: Arc<ValidatedConfig>) -> Arc<ClientPool> {
        self.client
            .get_or_init(|| Arc::new(ClientPool::production(config)))
            .clone()
    }

    /// Returns the process-local Tokio runtime handle.
    #[must_use]
    pub const fn runtime(&self) -> &Handle {
        &self.runtime
    }

    pub(crate) fn initialized_client(&self) -> Option<&Arc<ClientPool>> {
        self.client.get()
    }

    /// Marks this handle closed, returning whether this call changed its state.
    pub fn close(&self) -> bool {
        !self.closed.swap(true, Ordering::AcqRel)
    }

    /// Returns whether this handle has been closed or invalidated after a fork.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Returns the normalized configuration key owned by this handle.
    #[must_use]
    pub const fn key(&self) -> ConnectionKey {
        self.key
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectionHandle, ConnectionKey};

    #[test]
    fn closing_a_handle_is_idempotent() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let handle =
            ConnectionHandle::new(ConnectionKey::from_bytes([7; 32]), runtime.handle().clone());

        assert!(handle.close());
        assert!(!handle.close());
        assert!(handle.is_closed());
    }
}
