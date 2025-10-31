use crate::core::connections::connection_options::ConnectionOptions;
use crate::core::runtime::RUNTIME;
use lapin::Connection;
use once_cell::sync::Lazy;
use parking_lot::{Mutex, RwLock};
use std::{
    collections::HashMap,
    hash::Hash,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Weak,
    },
};
use tokio::{runtime::Handle, sync::Mutex as AsyncMutex, task};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ConnKey {
    pub scheme: &'static str,
    pub host: String,
    pub port: u16,
    pub vhost: String,
    pub user: String,
    pub tls_tag: &'static str,
}

pub struct ConnectionEntry {
    pub conn: RwLock<Arc<Connection>>, // swappable on reconnect
    pub id: String,                    // ex: amqp://host:port/vhost
    pub key: ConnKey,                  // used to look up the lock entry
    pub opts: ConnectionOptions,       // stored to rebuild the connection
    clients: AtomicUsize,              // number of attached clients
}

impl ConnectionEntry {
    #[inline]
    pub fn connection(&self) -> Arc<Connection> {
        self.conn.read().clone()
    }
    #[inline]
    pub fn set_connection(&self, new_conn: Connection) {
        *self.conn.write() = Arc::new(new_conn);
    }

    pub fn new(id: String, key: ConnKey, opts: ConnectionOptions, conn: Connection) -> Self {
        Self {
            conn: RwLock::new(Arc::new(conn)),
            id,
            key,
            opts,
            clients: AtomicUsize::new(1),
        }
    }

    #[inline]
    pub fn add_client(&self) -> usize {
        self.clients.fetch_add(1, Ordering::SeqCst) + 1
    }

    #[inline]
    pub fn release_client(&self) -> usize {
        let prev = self.clients.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(prev > 0, "release_client called with zero clients");
        prev.saturating_sub(1)
    }

    #[inline]
    pub fn client_count(&self) -> usize {
        self.clients.load(Ordering::SeqCst)
    }
}

impl Drop for ConnectionEntry {
    fn drop(&mut self) {
        let conn = self.conn.read().clone();
        if !conn.status().connected() {
            return;
        }

        // Attempt to close synchronously when we're outside of Tokio.
        if Handle::try_current().is_err() {
            RUNTIME.block_on(async move {
                if conn.status().connected() {
                    let _ = conn.close(200, "Connection pool drop").await;
                }
            });
            return;
        }

        // When already on a Tokio worker, offload the blocking wait to block_in_place.
        task::block_in_place(|| {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            Handle::current().spawn({
                let conn = conn.clone();
                async move {
                    if conn.status().connected() {
                        let _ = conn.close(200, "Connection pool drop").await;
                    }
                    let _ = tx.send(());
                }
            });
            let _ = rx.recv();
        });
    }
}

/// Process-wide cache: ensure a single live connection per `ConnKey`.
pub static CONNECTIONS: Lazy<Mutex<HashMap<ConnKey, Weak<ConnectionEntry>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Async locks keyed by `ConnKey` to serialise open/reconnect operations.
pub static CONNECTION_LOCKS: Lazy<Mutex<HashMap<ConnKey, Arc<AsyncMutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn get_connected_entry(key: &ConnKey) -> Option<Arc<ConnectionEntry>> {
    let mut pool = CONNECTIONS.lock();
    if let Some(weak) = pool.get(key).cloned() {
        if let Some(entry) = weak.upgrade() {
            if entry.connection().status().connected() {
                return Some(entry);
            }
        } else {
            // Remove expired weak references
            pool.remove(key);
        }
    }
    None
}

pub fn lock_for(key: &ConnKey) -> Arc<AsyncMutex<()>> {
    let mut locks = CONNECTION_LOCKS.lock();
    locks
        .entry(key.clone())
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone()
}

pub fn dispose_pool() {
    let mut pool = CONNECTIONS.lock();
    let entries: Vec<_> = pool.drain().map(|(_, w)| w).collect();
    drop(pool);

    for weak in entries {
        if let Some(entry) = weak.upgrade() {
            let conn = entry.connection();
            RUNTIME.block_on(async move {
                if conn.status().connected() {
                    let _ = conn.close(200, "Module shutdown").await;
                }
            });

            return;
        }
    }

    CONNECTION_LOCKS.lock().clear();
}
