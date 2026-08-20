use std::{
    collections::HashMap,
    error::Error,
    fmt, io,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use futures_util::future::join_all;
use tokio::runtime::{Builder, Runtime};

use crate::pool::{ConnectionHandle, ConnectionKey};

const DEFAULT_SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

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
struct TokioRuntimeFactory {
    worker_threads: usize,
}

impl Default for TokioRuntimeFactory {
    fn default() -> Self {
        // I/O-bound workload: a single worker thread reduces scheduling
        // overhead while still allowing multiple concurrent tasks via async.
        Self { worker_threads: 1 }
    }
}

impl RuntimeFactory for TokioRuntimeFactory {
    fn create(&self) -> io::Result<Runtime> {
        let mut builder = Builder::new_multi_thread();
        builder.thread_name("rabbit-rs").enable_all();
        if self.worker_threads > 0 {
            builder.worker_threads(self.worker_threads);
        }
        builder.build()
    }
}

struct ProcessState {
    pid: u32,
    runtime: Runtime,
    pools: HashMap<ConnectionKey, Arc<ConnectionHandle>>,
}

/// Lazily owns exactly one runtime and one set of pools per operating-system process.
pub struct RuntimeRegistry {
    pid_provider: Arc<dyn PidProvider>,
    runtime_factory: Arc<dyn RuntimeFactory>,
    shutdown_budget: Duration,
    state: Mutex<Option<ProcessState>>,
}

impl RuntimeRegistry {
    /// Creates an empty registry. No thread or socket is created by this call.
    #[must_use]
    pub fn new() -> Self {
        Self::with_dependencies(
            Arc::new(ProcessPid),
            Arc::new(TokioRuntimeFactory::default()),
        )
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
        Self::with_dependencies_and_shutdown_budget(
            pid_provider,
            runtime_factory,
            DEFAULT_SHUTDOWN_BUDGET,
        )
    }

    fn with_dependencies_and_shutdown_budget(
        pid_provider: Arc<dyn PidProvider>,
        runtime_factory: Arc<dyn RuntimeFactory>,
        shutdown_budget: Duration,
    ) -> Self {
        Self {
            pid_provider,
            runtime_factory,
            shutdown_budget,
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
        let inherited = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .as_ref()
                .is_some_and(|process| process.pid != current_pid)
                .then(|| state.take())
                .flatten()
        };
        Self::invalidate_inherited_state(inherited);

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if state.is_none() {
            let runtime = self
                .runtime_factory
                .create()
                .map_err(RuntimeCreationError::new)?;
            *state = Some(ProcessState {
                pid: current_pid,
                runtime,
                pools: HashMap::new(),
            });
        }

        match state.as_mut() {
            Some(process) => {
                if let Some(handle) = process.pools.get(&key)
                    && !handle.is_closed()
                {
                    return Ok(handle.clone());
                }

                let handle = Arc::new(ConnectionHandle::new(process.runtime.handle().clone()));
                process.pools.insert(key, handle.clone());
                Ok(handle)
            }
            None => Err(RuntimeCreationError::new(io::Error::other(
                "runtime factory returned without initializing process state",
            ))),
        }
    }

    /// Closes all process-local handles. Repeated calls have no effect.
    pub fn close(&self) {
        let current_pid = self.pid_provider.current_pid();
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if state
            .as_ref()
            .is_some_and(|process| process.pid != current_pid)
        {
            Self::invalidate_inherited_state(state);
        } else {
            Self::close_state(state, self.shutdown_budget);
        }
    }

    fn close_state(state: Option<ProcessState>, budget: Duration) {
        let Some(process) = state else {
            return;
        };
        let deadline = Instant::now() + budget;
        for handle in process.pools.values() {
            handle.close();
        }
        let clients = process
            .pools
            .values()
            .filter_map(|handle| handle.initialized_client())
            .cloned()
            .collect::<Vec<_>>();
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !clients.is_empty() && !remaining.is_zero() {
            process.runtime.block_on(async move {
                let closes = clients.iter().map(|client| client.close());
                let _ = tokio::time::timeout(remaining, join_all(closes)).await;
            });
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        process.runtime.shutdown_timeout(remaining);
    }

    fn invalidate_inherited_state(state: Option<ProcessState>) {
        if let Some(process) = state {
            for handle in process.pools.values() {
                handle.close();
            }
            // Tokio worker threads do not survive fork, so the inherited runtime
            // must neither run shutdown futures nor wait for those vanished threads.
            std::mem::forget(process.runtime);
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
    use std::{
        future,
        sync::{
            Arc,
            atomic::{AtomicU32, AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    use tokio::runtime::{Builder, Runtime};

    use super::{PidProvider, RuntimeFactory, RuntimeRegistry, TokioRuntimeFactory};
    use crate::{
        client::ClientPool,
        config::{
            BrokerConfig, Config, Credentials, Endpoint, TlsConfig, TopologyMode, ValidatedConfig,
        },
        pool::{ConnectionHandle, ConnectionKey},
        transport::mock::{MockTransport, TransportOperation},
    };

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

    fn registry_with_budget(
        budget: Duration,
    ) -> (
        RuntimeRegistry,
        Arc<MutablePid>,
        Arc<CountingRuntimeFactory>,
    ) {
        let pid = Arc::new(MutablePid::new(100));
        let factory = Arc::new(CountingRuntimeFactory::default());
        let registry = RuntimeRegistry::with_dependencies_and_shutdown_budget(
            pid.clone(),
            factory.clone(),
            budget,
        );

        (registry, pid, factory)
    }

    fn client_config(name: &str) -> ValidatedConfig {
        Config {
            brokers: vec![BrokerConfig {
                name: name.to_owned(),
                hosts: vec![Endpoint::new("rabbit.local", 5672)],
                vhost: "/".to_owned(),
                credentials: Credentials::new("guest", "secret"),
                tls: TlsConfig::disabled(),
                heartbeat: Duration::from_secs(30),
            }],
            workers: Vec::new(),
            topology_mode: TopologyMode::External,
            delay: crate::config::DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: crate::config::PublisherConfigSection::default(),
        }
        .validate()
        .expect("valid client config")
    }

    fn install_connected_client(
        registry: &RuntimeRegistry,
        key: ConnectionKey,
        broker: &str,
        transport: Arc<MockTransport>,
    ) -> Arc<ConnectionHandle> {
        let handle = registry.acquire(key).expect("handle");
        let client = Arc::new(ClientPool::new(Arc::new(client_config(broker)), transport));
        handle
            .install_client(client.clone())
            .unwrap_or_else(|_| panic!("install client"));
        handle
            .runtime()
            .block_on(client.initialize_connection_for_tests(broker))
            .expect("publish initializes connection");
        handle
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
    fn pid_change_drops_old_runtime_outside_the_mutex() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct SlowDropFactory(Arc<AtomicBool>);
        impl RuntimeFactory for SlowDropFactory {
            fn create(&self) -> std::io::Result<Runtime> {
                let runtime = Builder::new_current_thread().enable_all().build()?;
                self.0.store(true, Ordering::SeqCst);
                Ok(runtime)
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(MutablePid::new(100));
        let registry = RuntimeRegistry::with_dependencies(
            pid.clone(),
            Arc::new(SlowDropFactory(dropped.clone())),
        );
        let key = ConnectionKey::from_bytes([1; 32]);
        let _inherited = registry.acquire(key).expect("parent acquisition");

        pid.set(101);
        let child = registry.acquire(key).expect("child acquisition");

        assert!(!child.is_closed());
        assert!(
            dropped.load(Ordering::SeqCst),
            "old runtime was replaced after PID change"
        );
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

    #[test]
    fn acquiring_a_closed_pool_replaces_its_handle() {
        let (registry, _pid, _factory) = registry();
        let key = ConnectionKey::from_bytes([3; 32]);
        let closed = registry.acquire(key).expect("first acquisition");
        closed.close();

        let replacement = registry.acquire(key).expect("replacement acquisition");

        assert!(!Arc::ptr_eq(&closed, &replacement));
        assert!(!replacement.is_closed());
    }

    #[test]
    fn blocked_network_close_respects_the_shared_shutdown_budget() {
        let budget = Duration::from_millis(25);
        let (registry, _pid, _factory) = registry_with_budget(budget);
        let transport = Arc::new(MockTransport::default());
        let handle = install_connected_client(
            &registry,
            ConnectionKey::from_bytes([4; 32]),
            "default",
            transport.clone(),
        );
        let _gate = transport.push_close_connection_gate();

        let started = Instant::now();
        registry.close();

        assert!(started.elapsed() <= Duration::from_millis(250));
        assert!(handle.is_closed());
        assert!(
            transport
                .operations()
                .contains(&TransportOperation::CloseConnection)
        );
    }

    #[test]
    fn multiple_blocked_clients_share_one_shutdown_budget() {
        let budget = Duration::from_millis(25);
        let (registry, _pid, _factory) = registry_with_budget(budget);
        let first_transport = Arc::new(MockTransport::default());
        let second_transport = Arc::new(MockTransport::default());
        let first = install_connected_client(
            &registry,
            ConnectionKey::from_bytes([5; 32]),
            "first",
            first_transport.clone(),
        );
        let second = install_connected_client(
            &registry,
            ConnectionKey::from_bytes([6; 32]),
            "second",
            second_transport.clone(),
        );
        let _first_gate = first_transport.push_close_connection_gate();
        let _second_gate = second_transport.push_close_connection_gate();

        let started = Instant::now();
        registry.close();

        assert!(started.elapsed() <= Duration::from_millis(250));
        assert!(first.is_closed());
        assert!(second.is_closed());
        assert!(
            first_transport
                .operations()
                .contains(&TransportOperation::CloseConnection)
        );
        assert!(
            second_transport
                .operations()
                .contains(&TransportOperation::CloseConnection)
        );
    }

    #[test]
    fn active_runtime_tasks_do_not_block_shutdown_past_the_budget() {
        let budget = Duration::from_millis(25);
        let (registry, _pid, _factory) = registry_with_budget(budget);
        let handle = registry
            .acquire(ConnectionKey::from_bytes([7; 32]))
            .expect("handle");
        let _task = handle.runtime().spawn(future::pending::<()>());

        let started = Instant::now();
        registry.close();

        assert!(started.elapsed() <= Duration::from_millis(250));
        assert!(handle.is_closed());
    }

    #[test]
    fn pid_invalidation_never_waits_for_client_network_close() {
        let budget = Duration::from_millis(25);
        let (registry, pid, factory) = registry_with_budget(budget);
        let transport = Arc::new(MockTransport::default());
        let inherited = install_connected_client(
            &registry,
            ConnectionKey::from_bytes([8; 32]),
            "default",
            transport.clone(),
        );
        let _gate = transport.push_close_connection_gate();

        pid.set(101);
        let started = Instant::now();
        let child = registry
            .acquire(ConnectionKey::from_bytes([8; 32]))
            .expect("child handle");

        assert!(started.elapsed() <= Duration::from_millis(10));
        assert!(inherited.is_closed());
        assert!(!child.is_closed());
        assert_eq!(factory.creation_count(), 2);
        assert!(
            !transport
                .operations()
                .contains(&TransportOperation::CloseConnection)
        );
    }

    #[test]
    fn close_after_pid_change_never_touches_the_inherited_runtime() {
        let (registry, pid, _) = registry_with_budget(Duration::from_millis(25));
        let transport = Arc::new(MockTransport::default());
        let handle = install_connected_client(
            &registry,
            ConnectionKey::from_bytes([12; 32]),
            "inherited",
            transport.clone(),
        );
        let _blocked_close = transport.push_close_connection_gate();
        pid.set(101);

        registry.close();

        assert!(handle.is_closed());
        assert!(
            !transport
                .operations()
                .contains(&TransportOperation::CloseConnection)
        );
    }

    #[test]
    fn tokio_runtime_factory_defaults_to_one_worker_thread() {
        let factory = TokioRuntimeFactory::default();
        assert_eq!(
            factory.worker_threads, 1,
            "I/O-bound runtime should default to 1 worker thread"
        );
    }
}
