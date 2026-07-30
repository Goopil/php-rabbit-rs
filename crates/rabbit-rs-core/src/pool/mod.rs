use std::sync::atomic::{AtomicBool, Ordering};

pub mod key;

pub use key::ConnectionKey;

/// Process-local handle representing one reusable connection pool.
#[derive(Debug)]
pub struct ConnectionHandle {
    key: ConnectionKey,
    closed: AtomicBool,
}

impl ConnectionHandle {
    pub(crate) const fn new(key: ConnectionKey) -> Self {
        Self {
            key,
            closed: AtomicBool::new(false),
        }
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
        let handle = ConnectionHandle::new(ConnectionKey::from_bytes([7; 32]));

        assert!(handle.close());
        assert!(!handle.close());
        assert!(handle.is_closed());
    }
}
