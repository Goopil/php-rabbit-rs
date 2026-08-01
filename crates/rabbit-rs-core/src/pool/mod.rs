use std::{
    fmt,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use tokio::runtime::Handle;

use crate::{client::ClientPool, config::ValidatedConfig};

pub mod connection_actor;
pub mod key;

pub use key::ConnectionKey;

static NEXT_HANDLE_SERIAL: AtomicU64 = AtomicU64::new(1);

/// Process-local handle representing one reusable connection pool.
pub struct ConnectionHandle {
    identifier: String,
    closed: AtomicBool,
    runtime: Handle,
    client: OnceLock<Arc<ClientPool>>,
}

impl fmt::Debug for ConnectionHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionHandle")
            .field("identifier", &self.identifier)
            .field("closed", &self.is_closed())
            .field("client_initialized", &self.client.get().is_some())
            .finish_non_exhaustive()
    }
}

impl ConnectionHandle {
    pub(crate) fn new(runtime: Handle) -> Self {
        let serial = NEXT_HANDLE_SERIAL.fetch_add(1, Ordering::Relaxed);
        Self {
            identifier: format!("{}:{serial}", std::process::id()),
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

    #[cfg(test)]
    pub(crate) fn install_client(&self, client: Arc<ClientPool>) -> Result<(), Arc<ClientPool>> {
        self.client.set(client)
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

    /// Returns the process-local identity of this handle instance.
    #[must_use]
    pub fn identifier(&self) -> &str {
        &self.identifier
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionHandle;

    #[test]
    fn closing_a_handle_is_idempotent() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let handle = ConnectionHandle::new(runtime.handle().clone());

        assert!(handle.close());
        assert!(!handle.close());
        assert!(handle.is_closed());
    }

    #[test]
    fn each_handle_has_a_distinct_process_local_identifier() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let first = ConnectionHandle::new(runtime.handle().clone());
        let second = ConnectionHandle::new(runtime.handle().clone());

        assert_ne!(first.identifier(), second.identifier());
        assert!(
            first
                .identifier()
                .starts_with(&format!("{}:", std::process::id()))
        );
        assert!(!first.identifier().contains(&"07".repeat(32)));
        assert!(!format!("{first:?}").contains(&"07".repeat(32)));
    }
}
