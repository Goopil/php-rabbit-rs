use std::path::PathBuf;

#[derive(Clone, Default, Debug)]
pub struct SslOptions {
    pub cafile: Option<PathBuf>,
    pub capath: Option<PathBuf>,
    pub verify_peer: Option<bool>,
    pub verify_peer_name: Option<bool>,
    pub peer_name: Option<String>,
    pub allow_self_signed: Option<bool>,
    pub certfile: Option<PathBuf>,
    pub keyfile: Option<PathBuf>,
    pub passphrase: Option<String>,
    pub pkcs12: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ReconnectOptions {
    pub enabled: bool,
    pub max_retries: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub total_timeout_ms: u64,
    pub jitter: f64,
}

impl Default for ReconnectOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 5,
            initial_delay_ms: 200,
            max_delay_ms: 2_000,
            total_timeout_ms: 10_000,
            jitter: 0.2,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConnectionOptions {
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub password: String,
    pub vhost: String,
    pub ssl: Option<SslOptions>,
    pub reconnect: Option<ReconnectOptions>,
    /// Heartbeat interval in seconds (default: negotiated with server, usually ~60s)
    /// Passed to broker via URI query parameter: ?heartbeat=30
    /// Set to 0 to disable heartbeats.
    pub heartbeat: Option<u16>,
    /// Maximum frame size in bytes (default: negotiated with server, usually 128KB)
    /// Passed to broker via URI query parameter: ?frame_max=131072
    pub frame_max: Option<u32>,
    /// Connection timeout in milliseconds (default: client library default)
    /// Passed to broker via URI query parameter as seconds: ?connection_timeout=60
    pub connection_timeout_ms: Option<u64>,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: None,
            user: "guest".into(),
            password: "guest".into(),
            vhost: "/".into(),
            ssl: None,
            reconnect: None,
            heartbeat: None,
            frame_max: None,
            connection_timeout_ms: None,
        }
    }
}
