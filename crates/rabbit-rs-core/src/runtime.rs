use std::{
    collections::HashMap,
    error::Error,
    fmt, io,
    sync::{Arc, Mutex, OnceLock},
};

use tokio::runtime::{Builder, Runtime};

use crate::pool::{ConnectionHandle, ConnectionKey};

/// Supplies the current process identifier, and can be replaced in tests.
pub trait PidProvider: Send + Sync {
    fn current_pid(&self) -> u32;
}

/// Creates the asynchronous runtime lazily, and can be replaced in tests.
pub trait RuntimeFactory: Send + Sync {
    /// Creates a Tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the runtime or its worker threads cannot be created.
    fn create(&self) -> io::Result<Runtime>;
}

#[derive(Debug)]
struct ProcessPid;

impl PidProvider for ProcessPid {
    fn current_pid(&self) -> u32 {
        std::process::id()
    }
}

#[derive(Debug)]
struct TokioRuntimeFactory;

impl RuntimeFactory for TokioRuntimeFactory {
    fn create(&self) -> io::Result<Runtime> {
        Builder::new_multi_thread()
            .thread_name("rabbit-rs")
            .enable_all()
            .build()
    }
}

struct ProcessState {
    pid: u32,
    _runtime: Runtime,
    pools: HashMap<ConnectionKey, Arc<ConnectionHandle>>,
}

/// Lazily owns exactly one runtime and one set of pools per operating-system process.
pub struct RuntimeRegistry {
    pid_provider: Arc<dyn PidProvider>,
    runtime_factory: Arc<dyn RuntimeFactory>,
    state: Mutex<Option<ProcessState>>,
}

impl RuntimeRegistry {
    /// Creates an empty registry. No thread or socket is created by this call.
    #[must_use]
    pub fn new() -> Self {
        Self::with_dependencies(Arc::new(ProcessPid), Arc::new(TokioRuntimeFactory))
    }

    /// Returns the process-global registry without eagerly starting its runtime.
    #[must_use]
    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<RuntimeRegistry> = OnceLock::new();

        REGISTRY.get_or_init(Self::new)
    }

    /// Creates a registry with injectable process and runtime services.
    #[must_use]
    pub fn with_dependencies(
        pid_provider: Arc<dyn PidProvider>,
        runtime_factory: Arc<dyn RuntimeFactory>,
    ) -> Self {
        Self {
            pid_provider,
            runtime_factory,
            state: Mutex::new(None),
        }
    }

    /// Returns a reusable handle for this process and normalized configuration.
    ///
    /// If the PID changed, all inherited handles are invalidated before a new
    /// runtime and pool set are created.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeCreationError`] when the process-local Tokio runtime
    /// cannot be initialized.
    pub fn acquire(
        &self,
        key: ConnectionKey,
    ) -> Result<Arc<ConnectionHandle>, RuntimeCreationError> {
        let current_pid = self.pid_provider.current_pid();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pid_changed = state
            .as_ref()
            .is_some_and(|process| process.pid != current_pid);

        if pid_changed {
            Self::close_state(state.take());
        }

        if state.is_none() {
            let runtime = self
                .runtime_factory
                .create()
                .map_err(RuntimeCreationError::new)?;
            *state = Some(ProcessState {
                pid: current_pid,
                _runtime: runtime,
                pools: HashMap::new(),
            });
        }

        match state.as_mut() {
            Some(process) => Ok(process
                .pools
                .entry(key)
                .or_insert_with(|| Arc::new(ConnectionHandle::new(key)))
                .clone()),
            None => Err(RuntimeCreationError::new(io::Error::other(
                "runtime factory returned without initializing process state",
            ))),
        }
    }

    /// Closes all process-local handles. Repeated calls have no effect.
    pub fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::close_state(state.take());
    }

    fn close_state(state: Option<ProcessState>) {
        if let Some(process) = state {
            for handle in process.pools.values() {
                handle.close();
            }
        }
    }
}

impl Default for RuntimeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Failure to lazily initialize the process-local asynchronous runtime.
#[derive(Debug)]
pub struct RuntimeCreationError {
    source: io::Error,
}

impl RuntimeCreationError {
    const fn new(source: io::Error) -> Self {
        Self { source }
    }
}

impl fmt::Display for RuntimeCreationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to create Rabbit RS runtime: {}",
            self.source
        )
    }
}

impl Error for RuntimeCreationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    };

    use tokio::runtime::{Builder, Runtime};

    use super::{PidProvider, RuntimeFactory, RuntimeRegistry};
    use crate::pool::ConnectionKey;

    struct MutablePid(AtomicU32);

    impl MutablePid {
        const fn new(pid: u32) -> Self {
            Self(AtomicU32::new(pid))
        }

        fn set(&self, pid: u32) {
            self.0.store(pid, Ordering::SeqCst);
        }
    }

    impl PidProvider for MutablePid {
        fn current_pid(&self) -> u32 {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[derive(Default)]
    struct CountingRuntimeFactory(AtomicUsize);

    impl CountingRuntimeFactory {
        fn creation_count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    impl RuntimeFactory for CountingRuntimeFactory {
        fn create(&self) -> std::io::Result<Runtime> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Builder::new_current_thread().enable_all().build()
        }
    }

    fn registry() -> (
        RuntimeRegistry,
        Arc<MutablePid>,
        Arc<CountingRuntimeFactory>,
    ) {
        let pid = Arc::new(MutablePid::new(100));
        let factory = Arc::new(CountingRuntimeFactory::default());
        let registry = RuntimeRegistry::with_dependencies(pid.clone(), factory.clone());

        (registry, pid, factory)
    }

    #[test]
    fn runtime_creation_is_lazy() {
        let (registry, _pid, factory) = registry();

        assert_eq!(factory.creation_count(), 0);

        registry
            .acquire(ConnectionKey::from_bytes([1; 32]))
            .expect("runtime creation should succeed");

        assert_eq!(factory.creation_count(), 1);
    }

    #[test]
    fn same_pid_and_key_reuse_the_connection_handle() {
        let (registry, _pid, _factory) = registry();
        let key = ConnectionKey::from_bytes([1; 32]);

        let first = registry.acquire(key).expect("first acquisition");
        let second = registry.acquire(key).expect("second acquisition");

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn pid_change_invalidates_inherited_handles_and_runtime() {
        let (registry, pid, factory) = registry();
        let key = ConnectionKey::from_bytes([1; 32]);
        let inherited = registry.acquire(key).expect("parent acquisition");

        pid.set(101);
        let child = registry.acquire(key).expect("child acquisition");

        assert!(inherited.is_closed());
        assert!(!Arc::ptr_eq(&inherited, &child));
        assert_eq!(factory.creation_count(), 2);
    }

    #[test]
    fn distinct_configurations_do_not_share_a_pool() {
        let (registry, _pid, _factory) = registry();

        let first = registry
            .acquire(ConnectionKey::from_bytes([1; 32]))
            .expect("first configuration");
        let second = registry
            .acquire(ConnectionKey::from_bytes([2; 32]))
            .expect("second configuration");

        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn closing_the_registry_is_idempotent() {
        let (registry, _pid, _factory) = registry();
        let handle = registry
            .acquire(ConnectionKey::from_bytes([1; 32]))
            .expect("acquisition");

        registry.close();
        registry.close();

        assert!(handle.is_closed());
    }
}
