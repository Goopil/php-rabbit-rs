use std::{
    fmt,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use tokio::runtime::Handle;

use crate::client::{ClientError, ClientPool};
use crate::config::ValidatedConfig;

pub mod connection_actor;
pub mod key;
pub mod recovery_coordinator;

pub use key::ConnectionKey;
pub use recovery_coordinator::{
    CoordinatorError, RecoveryCoordinator, RecoveryCoordinatorConfig, RecoveryCoordinatorHandle,
};

static NEXT_HANDLE_SERIAL: AtomicU64 = AtomicU64::new(1);

/// Process-local handle representing one reusable connection pool.
///
/// Every pool object sharing a configuration fingerprint holds one claim on
/// the shared handle. Closing a claim releases it; the shared connection is
/// torn down only when the last claim closes, so sibling pools survive one
/// pool's close (issue #221).
pub struct ConnectionHandle {
    identifier: String,
    closed: AtomicBool,
    claims: AtomicUsize,
    runtime: Handle,
    client: OnceLock<Arc<ClientPool>>,
}

impl fmt::Debug for ConnectionHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionHandle")
            .field("identifier", &self.identifier)
            .field("closed", &self.is_closed())
            .field("live_claims", &self.live_claims())
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
            claims: AtomicUsize::new(0),
            runtime,
            client: OnceLock::new(),
        }
    }

    /// Registers one claim on this handle. The registry calls this on every
    /// acquire; the claim lives exactly as long as the acquiring pool object.
    pub(crate) fn add_claim(&self) {
        self.claims.fetch_add(1, Ordering::AcqRel);
    }

    /// Returns the number of live claims on this handle.
    #[must_use]
    pub fn live_claims(&self) -> usize {
        self.claims.load(Ordering::Acquire)
    }

    /// Releases one claim, returning whether the caller held the last live
    /// claim.
    ///
    /// Releasing the last claim does not tear the shared connection down:
    /// the registry keeps the handle for reuse, mirroring a pool object
    /// being dropped without an explicit close. The explicit-close protocol
    /// is [`ConnectionHandle::close_claim`].
    pub fn release_claim(&self) -> bool {
        let previous = self.claims.fetch_sub(1, Ordering::AcqRel);
        if previous == 0 {
            // A release without a matching acquire is a boundary bug:
            // restore the count instead of wrapping around to usize::MAX.
            self.claims.fetch_add(1, Ordering::AcqRel);
            return false;
        }
        previous == 1
    }

    /// Closes this handle's claim the way an explicit `Pool::close()` does:
    /// release the claim, tear `client` down when it was the last live
    /// claim, and retire the handle so the registry replaces it on the next
    /// acquire. When other claims are still live, only the caller's claim
    /// is released, `client` is left running, and the shared connection
    /// stays up for its siblings.
    ///
    /// The caller passes the client it has been operating on — the shared
    /// handle client for production pools, the pool-owned client for the
    /// extension test path.
    ///
    /// # Errors
    ///
    /// Returns the client's shutdown failure; the handle is retired
    /// regardless.
    pub async fn close_claim(&self, client: &ClientPool) -> Result<(), ClientError> {
        if !self.release_claim() {
            return Ok(());
        }
        if !self.is_closed()
            && let Err(error) = client.close().await
        {
            self.close();
            return Err(error);
        }
        self.close();
        Ok(())
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
