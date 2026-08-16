use std::path::PathBuf;
use std::time::Duration;

use rabbit_rs_core::config::{BrokerConfig, Config, Credentials, Endpoint, TlsConfig, TlsVerify};
use rabbit_rs_core::transport::lapin::connection_uri;

fn broker_with_tls(tls: TlsConfig) -> BrokerConfig {
    BrokerConfig {
        name: "primary".to_owned(),
        hosts: vec![Endpoint::new("rabbit.example.com", 5671)],
        vhost: "/".to_owned(),
        credentials: Credentials::new("guest", "guest"),
        tls,
        heartbeat: Duration::from_secs(30),
    }
}

#[test]
fn tls_enabled_uses_amqps_scheme() {
    let broker = broker_with_tls(TlsConfig::enabled());
    let uri = connection_uri(&broker, &broker.hosts()[0]).expect("valid URI");
    assert_eq!(uri.scheme(), "amqps");
}

#[test]
fn tls_disabled_uses_amqp_scheme() {
    let broker = broker_with_tls(TlsConfig::disabled());
    let uri = connection_uri(&broker, &broker.hosts()[0]).expect("valid URI");
    assert_eq!(uri.scheme(), "amqp");
}

#[test]
fn tls_server_name_resolves_to_explicit_value() {
    let broker = broker_with_tls(TlsConfig::enabled().with_server_name("broker.internal"));
    assert_eq!(broker.effective_server_name(), "broker.internal");
}

#[test]
fn tls_server_name_falls_back_to_first_host() {
    let broker = broker_with_tls(TlsConfig::enabled());
    assert_eq!(broker.effective_server_name(), "rabbit.example.com");
}

#[test]
fn tls_config_deserializes_ca_and_client_certs() {
    let tls: TlsConfig = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "server_name": "broker.internal",
        "ca_cert": "/etc/ssl/certs/ca.pem",
        "client_cert": "/etc/ssl/client/cert.pem",
        "client_key": "/etc/ssl/client/key.pem",
        "verify": "peer"
    }))
    .expect("valid TLS config");

    assert!(tls.is_enabled());
    assert_eq!(tls.server_name(), Some("broker.internal"));
    assert_eq!(tls.ca_cert(), Some(&PathBuf::from("/etc/ssl/certs/ca.pem")));
    assert_eq!(
        tls.client_cert(),
        Some(&PathBuf::from("/etc/ssl/client/cert.pem"))
    );
    assert_eq!(
        tls.client_key(),
        Some(&PathBuf::from("/etc/ssl/client/key.pem"))
    );
    assert_eq!(tls.verify(), TlsVerify::Peer);
}

#[test]
fn tls_verify_defaults_to_peer() {
    let tls: TlsConfig = serde_json::from_value(serde_json::json!({
        "enabled": true
    }))
    .expect("valid TLS config");

    assert_eq!(tls.verify(), TlsVerify::Peer);
}

#[test]
fn tls_verify_can_be_set_to_none() {
    let tls: TlsConfig = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "verify": "none"
    }))
    .expect("valid TLS config");

    assert_eq!(tls.verify(), TlsVerify::None);
}

#[test]
fn tls_config_without_certs_defaults_to_none() {
    let tls: TlsConfig = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "server_name": "broker.internal"
    }))
    .expect("valid TLS config");

    assert!(tls.ca_cert().is_none());
    assert!(tls.client_cert().is_none());
    assert!(tls.client_key().is_none());
}

#[test]
fn tls_changes_affect_config_fingerprint() {
    let validated_with = serde_json::from_value::<Config>(serde_json::json!({
        "brokers": [{
            "name": "primary",
            "hosts": [{"host": "rabbit.example.com", "port": 5671}],
            "vhost": "/",
            "credentials": {"username": "guest", "password": "secret"},
            "tls": {
                "enabled": true,
                "server_name": "broker.internal",
                "ca_cert": "/etc/ssl/certs/ca.pem"
            },
            "heartbeat": 30
        }],
        "workers": [{
            "name": "main",
            "subscriptions": [{
                "name": "jobs",
                "broker": "primary",
                "queue": "jobs",
                "weight": 1,
                "priority_class": 0,
                "prefetch": 8,
                "starvation_after": 30
            }],
            "scheduler": {"strategy": "weighted_fair", "max_in_flight": 16}
        }],
        "topology_mode": "declare"
    }))
    .expect("valid config with CA")
    .validate()
    .expect("valid");

    let validated_without = serde_json::from_value::<Config>(serde_json::json!({
        "brokers": [{
            "name": "primary",
            "hosts": [{"host": "rabbit.example.com", "port": 5671}],
            "vhost": "/",
            "credentials": {"username": "guest", "password": "secret"},
            "tls": {
                "enabled": true,
                "server_name": "broker.internal"
            },
            "heartbeat": 30
        }],
        "workers": [{
            "name": "main",
            "subscriptions": [{
                "name": "jobs",
                "broker": "primary",
                "queue": "jobs",
                "weight": 1,
                "priority_class": 0,
                "prefetch": 8,
                "starvation_after": 30
            }],
            "scheduler": {"strategy": "weighted_fair", "max_in_flight": 16}
        }],
        "topology_mode": "declare"
    }))
    .expect("valid config without CA")
    .validate()
    .expect("valid");

    assert_ne!(
        validated_with.fingerprint(),
        validated_without.fingerprint(),
        "different TLS CA cert paths must produce different fingerprints"
    );
}

#[test]
fn tls_verify_change_affects_fingerprint() {
    let peer = serde_json::from_value::<Config>(serde_json::json!({
        "brokers": [{
            "name": "primary",
            "hosts": [{"host": "rabbit.example.com", "port": 5671}],
            "vhost": "/",
            "credentials": {"username": "guest", "password": "secret"},
            "tls": {"enabled": true, "verify": "peer"},
            "heartbeat": 30
        }],
        "workers": [{
            "name": "main",
            "subscriptions": [{
                "name": "jobs", "broker": "primary", "queue": "jobs",
                "weight": 1, "priority_class": 0, "prefetch": 8, "starvation_after": 30
            }],
            "scheduler": {"strategy": "weighted_fair", "max_in_flight": 16}
        }],
        "topology_mode": "declare"
    }))
    .unwrap()
    .validate()
    .unwrap();

    let none = serde_json::from_value::<Config>(serde_json::json!({
        "brokers": [{
            "name": "primary",
            "hosts": [{"host": "rabbit.example.com", "port": 5671}],
            "vhost": "/",
            "credentials": {"username": "guest", "password": "secret"},
            "tls": {"enabled": true, "verify": "none"},
            "heartbeat": 30
        }],
        "workers": [{
            "name": "main",
            "subscriptions": [{
                "name": "jobs", "broker": "primary", "queue": "jobs",
                "weight": 1, "priority_class": 0, "prefetch": 8, "starvation_after": 30
            }],
            "scheduler": {"strategy": "weighted_fair", "max_in_flight": 16}
        }],
        "topology_mode": "declare"
    }))
    .unwrap()
    .validate()
    .unwrap();

    assert_ne!(
        peer.fingerprint(),
        none.fingerprint(),
        "different verify modes must produce different fingerprints"
    );
}
