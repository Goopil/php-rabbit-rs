//! Transport tuning tests: `frame_max`, `worker_threads`, and connection URI parameters.

use rabbit_rs_core::config::{BrokerConfig, Credentials, Endpoint, TlsConfig};
use rabbit_rs_core::transport::lapin::connection_uri;

fn test_broker() -> BrokerConfig {
    BrokerConfig {
        name: "test".to_string(),
        hosts: vec![Endpoint::new("rabbit.local", 5672)],
        vhost: "/".to_string(),
        credentials: Credentials::new("guest", "guest"),
        tls: TlsConfig::disabled(),
        heartbeat: std::time::Duration::from_secs(30),
    }
}

#[test]
fn connection_uri_includes_frame_max_1mb() {
    let broker = test_broker();
    let endpoint = &broker.hosts[0];
    let uri = connection_uri(&broker, endpoint).expect("URI construction should succeed");
    assert!(
        uri.query().unwrap_or("").contains("frame_max=1048576"),
        "URI should contain frame_max=1048576, got: {:?}",
        uri.query()
    );
}
