//! Transport tuning tests: `frame_max`, connection URI parameters, and loud TLS file errors.

mod common;

use std::time::Duration;

use rabbit_rs_core::config::{BrokerConfig, Credentials, Endpoint, TlsConfig};
use rabbit_rs_core::transport::Transport;
use rabbit_rs_core::transport::lapin::connection_uri;

fn test_broker() -> BrokerConfig {
    BrokerConfig {
        name: "test".to_string(),
        hosts: vec![Endpoint::new("rabbit.local", 5672)],
        vhost: "/".to_string(),
        credentials: Credentials::new("guest", "guest"),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(30),
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

mod helper {
    use super::*;

    pub fn broker(name: &str, vhost: &str) -> BrokerConfig {
        crate::common::broker(name, vhost, "guest")
    }
}

/// Builds the broker with the given TLS configuration, connects through the
/// lapin transport, and asserts the failure names the offending file path.
///
/// `TlsConfig` fields are private, so the TLS configuration is built through
/// deserialization exactly as the Laravel normalizer would emit it.
fn assert_tls_loud_failure(tls_json: serde_json::Value, expected_path: &str) {
    let mut broker = helper::broker("tls-b", "/");
    broker.tls = serde_json::from_value(tls_json).expect("valid TLS configuration");

    let transport = rabbit_rs_core::transport::lapin::LapinTransport;
    let error = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(transport.connect(&broker))
        .err()
        .expect("unreadable TLS material must fail loudly");

    assert!(
        error.to_string().contains(expected_path),
        "error must identify the exact file path: {error}"
    );
}

#[test]
fn unreadable_tls_files_fail_loudly_instead_of_connecting_unprotected() {
    assert_tls_loud_failure(
        serde_json::json!({
            "enabled": true,
            "ca_cert": "/nonexistent/ca.pem"
        }),
        "/nonexistent/ca.pem",
    );
}

#[test]
fn unreadable_client_certificate_fails_loudly_instead_of_dropping_identity() {
    assert_tls_loud_failure(
        serde_json::json!({
            "enabled": true,
            "client_cert": "/nonexistent/client-cert.pem",
            "client_key": "/nonexistent/client-key.pem"
        }),
        "/nonexistent/client-cert.pem",
    );
}

#[test]
fn unreadable_client_key_fails_loudly_instead_of_dropping_identity() {
    let cert_path =
        std::env::temp_dir().join(format!("rabbit-rs-tls-loud-{}.pem", std::process::id()));
    std::fs::write(
        &cert_path,
        b"-----BEGIN CERTIFICATE-----\nZmFrZQ==\n-----END CERTIFICATE-----\n",
    )
    .expect("write temporary certificate fixture");

    let result = std::panic::catch_unwind(|| {
        assert_tls_loud_failure(
            serde_json::json!({
                "enabled": true,
                "client_cert": cert_path.display().to_string(),
                "client_key": "/nonexistent/client-key.pem"
            }),
            "/nonexistent/client-key.pem",
        );
    });

    let _ = std::fs::remove_file(&cert_path);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
