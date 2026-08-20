use std::time::Duration;

use rabbit_rs_core::{
    config::{BrokerConfig, Credentials, Endpoint, TlsConfig},
    transport::lapin::connection_uri,
};

fn broker() -> BrokerConfig {
    BrokerConfig {
        name: "primary".to_owned(),
        hosts: vec![Endpoint::new("rabbit.internal", 5671)],
        vhost: "/".to_owned(),
        credentials: Credentials::new("guest", "guest"),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(30),
    }
}

#[test]
fn connection_uri_includes_frame_max_of_one_megabyte() {
    let config = broker();
    let uri = connection_uri(&config, &config.hosts[0]).expect("valid URI");

    let query = uri.query().expect("URI must have a query string");
    assert!(
        query.contains("frame_max=1048576"),
        "URI query '{query}' must contain frame_max=1048576 (1 MB)"
    );
}

#[test]
fn connection_uri_preserves_heartbeat_alongside_frame_max() {
    let config = broker();
    let uri = connection_uri(&config, &config.hosts[0]).expect("valid URI");

    let query = uri.query().expect("URI must have a query string");
    assert!(
        query.contains("heartbeat=30"),
        "URI query '{query}' must still contain heartbeat=30"
    );
}
