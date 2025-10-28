use crate::core::connections::connection_options::ConnectionOptions;
use crate::core::connections::connection_pool::{
    get_connected_entry, lock_for, ConnKey, ConnectionEntry, CONNECTIONS, CONNECTION_LOCKS,
};
use crate::core::connections::connection_tls::build_tls;
use crate::core::runtime::RUNTIME;
use anyhow::Result;
use lapin::{tcp::OwnedTLSConfig, Connection, ConnectionProperties};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{runtime::Handle, sync::Mutex as AsyncMutex, task};
use crate::core::channels::channel::AmqpChannel;

pub struct AmqpClient {
    entry: Arc<ConnectionEntry>,
    released: AtomicBool,
}

impl Clone for AmqpClient {
    fn clone(&self) -> Self {
        self.entry.add_client();
        Self::new(self.entry.clone())
    }
}

impl Drop for AmqpClient {
    fn drop(&mut self) {
        let _ = self.release(false);
    }
}

impl AmqpClient {
    fn new(entry: Arc<ConnectionEntry>) -> Self {
        Self {
            entry,
            released: AtomicBool::new(false),
        }
    }

    fn reconnect_policy(opts: &ConnectionOptions) -> (bool, u32, u64, u64, u64, f64) {
        let r = opts.reconnect.clone().unwrap_or_default();
        (
            r.enabled,
            r.max_retries,
            r.initial_delay_ms,
            r.max_delay_ms,
            r.total_timeout_ms,
            r.jitter,
        )
    }

    #[inline]
    fn scheme_port(opts: &ConnectionOptions) -> (&'static str, u16, bool) {
        let want_tls = opts.ssl.is_some() || matches!(opts.port, Some(p) if p == 5671);
        let port = opts.port.unwrap_or(if want_tls { 5671 } else { 5672 });
        let scheme = if want_tls { "amqps" } else { "amqp" };
        (scheme, port, want_tls)
    }

    #[inline]
    fn next_backoff(mut delay_ms: u64, max_ms: u64, jitter: f64, rng: &mut u64) -> (u64, u64) {
        // LCG step for a tiny, deterministic RNG (keeps deps minimal)
        *rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);

        // Map upper bits to [0,1)
        let rand01 = ((*rng >> 33) as f64) / (u32::MAX as f64);
        let factor = if jitter > 0.0 {
            1.0 + rand01 * jitter
        } else {
            1.0
        };

        let sleep_ms = ((delay_ms as f64) * factor).round() as u64;
        let sleep_ms = sleep_ms.min(max_ms);

        // Exponential backoff with cap
        delay_ms = (delay_ms.saturating_mul(2)).min(max_ms);

        (sleep_ms, delay_ms)
    }

    pub fn id(&self) -> String {
        self.entry.id.clone()
    }

    pub fn connect(opts: ConnectionOptions) -> Result<Self> {
        RUNTIME.block_on(async move {
            let (scheme, port, _want_tls) = Self::scheme_port(&opts);

            let tls_tag: &'static str = match (_want_tls, &opts.ssl) {
                (false, _) => "none",
                (true, None) => "default",
                (true, Some(_)) => "custom",
            };
            let key = ConnKey {
                scheme,
                host: opts.host.clone(),
                port,
                vhost: opts.vhost.clone(),
                user: opts.user.clone(),
                tls_tag,
            };

            if let Some(entry) = get_connected_entry(&key) {
                entry.add_client();
                return Ok(Self::new(entry));
            }

            // Per-key async lock to avoid races
            let _guard = lock_for(&key).lock_owned().await;

            // Double-check after locking
            if let Some(entry) = get_connected_entry(&key) {
                entry.add_client();
                return Ok(Self::new(entry));
            }

            // Build URI & connect
            let conn = Self::connect_inner(scheme, port, &opts).await?;

            // Generate a simple local id (host:port/vhost + unique pointer addr)
            let id = format!("{scheme}://{}:{}/{}", &opts.host, port, &opts.vhost);

            let entry = Arc::new(ConnectionEntry::new(id, key.clone(), opts.clone(), conn));

            CONNECTIONS.lock().insert(key, Arc::downgrade(&entry));

            Ok(Self::new(entry))
        })
    }

    pub fn ensure_connected(&self) -> Result<()> {
        let conn = self.entry.connection();
        if conn.status().connected() {
            return Ok(());
        }

        let (enabled, _max_retries, _init_ms, _max_ms, _total_ms, _jitter) =
            Self::reconnect_policy(&self.entry.opts);

        if !enabled {
            anyhow::bail!("connection is closed and auto-reconnect is disabled");
        }

        self.reconnect_blocking()
    }

    pub fn reconnect_blocking(&self) -> Result<()> {
        let (enabled, max_retries, init_ms, max_ms, total_ms, jitter) =
            Self::reconnect_policy(&self.entry.opts);

        if !enabled {
            anyhow::bail!("connection is closed and auto-reconnect is disabled");
        }

        let key = self.entry.key.clone();
        RUNTIME.block_on(async {
            let lock_arc = {
                let mut m = CONNECTION_LOCKS.lock();
                m.entry(key.clone())
                    .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                    .clone()
            };
            let _guard = lock_arc.lock().await;

            // double-check after locking
            if self.entry.connection().status().connected() {
                return Ok(());
            }

            let (scheme, port, _want_tls) = Self::scheme_port(&self.entry.opts);
            let start = Instant::now();
            let mut delay = init_ms;
            let mut attempts: u32 = 0;
            let now_nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            let mut rng_seed: u64 = now_nanos ^ (port as u64) ^ (start.elapsed().as_nanos() as u64);

            loop {
                attempts += 1;
                match Self::connect_inner(scheme, port, &self.entry.opts).await {
                    Ok(new_conn) => {
                        self.entry.set_connection(new_conn);
                        return Ok(());
                    }
                    Err(err) => {
                        if attempts >= max_retries
                            || start.elapsed() >= Duration::from_millis(total_ms)
                        {
                            return Err(err);
                        }
                        let (sleep_ms, next_delay) =
                            Self::next_backoff(delay, max_ms, jitter, &mut rng_seed);
                        tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
                        delay = next_delay;
                    }
                }
            }
        })
    }

    pub fn close(&self) -> Result<()> {
        self.release(true)
    }

    pub fn close_if_last(&self) -> Result<()> {
        self.release(false)
    }

    fn release(&self, force: bool) -> Result<()> {
        if force {
            if !self.released.swap(true, Ordering::SeqCst) {
                let _ = self.entry.release_client();
            }
            self.close_connection()?;
            let key = self.entry.key.clone();
            CONNECTIONS.lock().remove(&key);
            CONNECTION_LOCKS.lock().remove(&key);
            return Ok(());
        }

        if self.released.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let remaining = self.entry.release_client();
        if remaining == 0 {
            self.close_connection()?;
            let key = self.entry.key.clone();
            CONNECTIONS.lock().remove(&key);
            CONNECTION_LOCKS.lock().remove(&key);
        }

        Ok(())
    }

    fn close_connection(&self) -> Result<()> {
        let conn = self.entry.connection();
        if Handle::try_current().is_err() {
            return RUNTIME.block_on(async move {
                if conn.status().connected() {
                    let _ = conn.close(200, "Bye").await; // ignore second-close errors
                }

                Ok(())
            });
        }

        task::block_in_place(|| {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            Handle::current().spawn(async move {
                if conn.status().connected() {
                    let _ = conn.close(200, "Bye").await;
                }
                let _ = tx.send(());
            });
            let _ = rx.recv();
        });

        Ok(())
    }

    pub fn ping(&self) -> Result<()> {
        self.ensure_connected()?;

        let first = RUNTIME.block_on(async {
            self.entry
                .connection()
                .create_channel()
                .await?
                .close(200, "ping")
                .await?;

            Result::<(), anyhow::Error>::Ok(())
        });

        match first {
            Ok(()) => Ok(()),
            Err(e) => {
                let (enabled, _, _, _, _, _) = Self::reconnect_policy(&self.entry.opts);
                if !enabled {
                    return Err(e);
                }
                self.reconnect_blocking()?;

                RUNTIME.block_on(async {
                    self.entry
                        .connection()
                        .create_channel()
                        .await?
                        .close(200, "ping")
                        .await?;

                    Ok(())
                })
            }
        }
    }

    pub fn open_channel(&self) -> anyhow::Result<AmqpChannel> {
        let entry = self.entry.clone();
        let ch = RUNTIME.block_on(async move { AmqpChannel::open(entry).await })?;

        Ok(ch)
    }

    /// Build query string for AMQP URI with heartbeat, frame_max, etc.
    fn build_query_string(opts: &ConnectionOptions) -> String {
        let mut params = Vec::new();

        if let Some(hb) = opts.heartbeat {
            params.push(format!("heartbeat={}", hb));
        }

        if let Some(fm) = opts.frame_max {
            params.push(format!("frame_max={}", fm));
        }

        if let Some(timeout_ms) = opts.connection_timeout_ms {
            // connection_timeout is in seconds for AMQP URI
            let timeout_s = (timeout_ms + 999) / 1000; // round up to seconds
            params.push(format!("connection_timeout={}", timeout_s));
        }

        if params.is_empty() {
            String::new()
        } else {
            format!("?{}", params.join("&"))
        }
    }

    async fn connect_inner(
        scheme: &str,
        port: u16,
        opts: &ConnectionOptions,
    ) -> Result<Connection> {
        // Build query string with heartbeat, frame_max, connection_timeout
        let query_string = Self::build_query_string(opts);

        let uri = format!(
            "{scheme}://{}:{}@{}:{}/{}{}",
            urlencoding::encode(&opts.user),
            urlencoding::encode(&opts.password),
            opts.host,
            port,
            urlencoding::encode(&opts.vhost),
            query_string
        );

        let props = ConnectionProperties::default();

        let want_tls = opts.ssl.is_some() || matches!(opts.port, Some(p) if p == 5671);
        if want_tls {
            let tls = if let Some(ref ssl) = opts.ssl {
                build_tls(ssl)?
            } else {
                OwnedTLSConfig::default()
            };
            Ok(Connection::connect_with_config(&uri, props, tls).await?)
        } else {
            Ok(Connection::connect(&uri, props).await?)
        }
    }
}
