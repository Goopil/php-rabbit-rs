//! TLS integration tests against the lab's `with-tls` profile.
//!
//! Requires the TLS lab profile (`./scripts/lab-up.sh with-tls`, or
//! `./scripts/test-integration.sh --with-tls`): a standalone broker serving
//! AMQP over TLS on `localhost:5671`, its certificate signed by the lab CA in
//! `lab/rabbitmq/tls/generated/lab-ca.pem`, a deliberately untrusted CA in
//! `lab-other-ca.pem`, a client identity (`lab-client.pem` +
//! `lab-client-key.pem`) for the mTLS node on `127.0.0.1:5676`
//! (`fail_if_no_peer_cert = true`), and `wrong.internal` resolvable to
//! 127.0.0.1 via /etc/hosts for the SAN-negative test.

#![cfg(feature = "integration")]

use std::time::Duration;

use bytes::Bytes;
use rabbit_rs_core::{
    config::{BrokerConfig, Credentials, Endpoint, TlsVerify, TopologyMode},
    topology::{QueueDefinition, TopologyDefinition, TopologyPlan, TopologyReconciler},
    transport::{
        PublishConfirmation, PublishProperties, PublishRequest, Transport, TransportErrorKind,
        lapin::LapinTransport,
    },
};

const TLS_HOST: &str = "localhost";
const TLS_PORT: u16 = 5671;
const TLS_VHOST: &str = "/orders-eu";
/// Host port of the mTLS node's AMQPS listener (container port 5673; the
/// host side avoids the with-plugin cluster's rabbitmq-2 mapping on 5673).
const MTLS_HOST: &str = "127.0.0.1";
const MTLS_PORT: u16 = 5674;
/// Resolves to 127.0.0.1 via /etc/hosts but is absent from the server
/// certificate SANs, so hostname verification must fail.
const WRONG_SAN_HOST: &str = "wrong.internal";

/// Absolute paths resolved from the crate directory, so the tests run from
/// any working directory.
fn generated_cert(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../lab/rabbitmq/tls/generated")
        .join(name)
}

fn lab_ca() -> std::path::PathBuf {
    generated_cert("lab-ca.pem")
}

fn foreign_ca() -> std::path::PathBuf {
    generated_cert("lab-other-ca.pem")
}

fn client_cert() -> std::path::PathBuf {
    generated_cert("lab-client.pem")
}

fn client_key() -> std::path::PathBuf {
    generated_cert("lab-client.key")
}

/// A broker definition pointing at the TLS listener. `tls` is built from the
/// given JSON so tests can pick their CA and SNI assertion per scenario.
fn tls_broker_at(host: &str, port: u16, name: &str, tls: serde_json::Value) -> BrokerConfig {
    let tls: rabbit_rs_core::config::TlsConfig =
        serde_json::from_value(tls).expect("valid TLS config");
    BrokerConfig {
        name: name.to_owned(),
        hosts: vec![Endpoint::new(host, port)],
        vhost: TLS_VHOST.to_owned(),
        credentials: Credentials::new("rabbit_rs", "rabbit_rs_lab"),
        tls,
        heartbeat: Duration::from_secs(30),
    }
}

fn tls_broker(name: &str, tls: serde_json::Value) -> BrokerConfig {
    tls_broker_at(TLS_HOST, TLS_PORT, name, tls)
}

fn trusted_tls() -> serde_json::Value {
    serde_json::json!({"enabled": true, "ca_cert": lab_ca().to_string_lossy()})
}

/// Declares the per-run queue so the publish lands on a real binding, then
/// returns once the topology is in place.
async fn declare_queue(broker_config: &BrokerConfig, queue: &str) {
    let connection = LapinTransport
        .connect(broker_config)
        .await
        .expect("TLS connect for topology");
    let channel = connection
        .open_publisher()
        .await
        .expect("publisher channel");

    let plan = TopologyPlan::compile(
        TopologyMode::Declare,
        TopologyDefinition::new(vec![], vec![QueueDefinition::new(queue)], vec![]),
    )
    .expect("compile plan");

    let mut reconciler = TopologyReconciler::new();
    reconciler
        .reconcile(channel.as_ref(), &plan, 1)
        .await
        .expect("declare queue");

    channel.close().await.expect("close channel");
    connection.close().await.expect("close connection");
}

/// Publishes one message with publisher confirms enabled over the given
/// broker config and asserts the broker acknowledged it.
async fn publish_with_confirms(broker_config: &BrokerConfig, queue: &str, message_id: &str) {
    let connection = LapinTransport
        .connect(broker_config)
        .await
        .expect("TLS connect");
    let channel = connection
        .open_publisher()
        .await
        .expect("publisher channel");

    channel.enable_confirms().await.expect("enable confirms");
    let receipt = channel
        .publish(PublishRequest {
            exchange: String::new().into(),
            routing_key: queue.to_owned().into(),
            payload: Bytes::from_static(b"tls-hello"),
            mandatory: true,
            properties: PublishProperties {
                message_id: Some(message_id.to_owned()),
                ..PublishProperties::default()
            },
        })
        .await
        .expect("publish over TLS");

    let confirmation = receipt.wait().await.expect("confirm resolution");
    assert!(
        matches!(confirmation, PublishConfirmation::Ack(_)),
        "expected a publisher ACK over TLS, got {confirmation:?}"
    );

    channel.close().await.expect("close channel");
    connection.close().await.expect("close connection");
}

#[tokio::test]
async fn tls_handshake_succeeds_against_the_lab_certificate() {
    let broker_config = tls_broker("tls-primary", trusted_tls());
    let queue = "rabbit-rs-it-tls-trusted";

    declare_queue(&broker_config, queue).await;
    publish_with_confirms(&broker_config, queue, "msg-tls-trusted-1").await;
}

#[tokio::test]
async fn tls_handshake_fails_against_an_untrusted_ca() {
    let broker_config = tls_broker(
        "tls-untrusted",
        serde_json::json!({"enabled": true, "ca_cert": foreign_ca().to_string_lossy()}),
    );

    let Err(error) = LapinTransport.connect(&broker_config).await else {
        panic!("untrusted CA must fail the TLS handshake");
    };

    assert!(
        matches!(
            error.kind(),
            TransportErrorKind::Connection | TransportErrorKind::Protocol
        ),
        "expected a transport-level handshake failure, got {error:?}"
    );
}

#[tokio::test]
async fn tls_connects_with_server_name_matching_the_connection_host() {
    let broker_config = tls_broker(
        "tls-sni",
        serde_json::json!({
            "enabled": true,
            "ca_cert": lab_ca().to_string_lossy(),
            "server_name": TLS_HOST
        }),
    );
    let queue = "rabbit-rs-it-tls-sni";

    assert_eq!(broker_config.tls.verify(), TlsVerify::Peer);

    declare_queue(&broker_config, queue).await;
    publish_with_confirms(&broker_config, queue, "msg-tls-sni-1").await;
}

#[tokio::test]
async fn mtls_handshake_succeeds_with_client_identity() {
    let broker_config = tls_broker_at(
        MTLS_HOST,
        MTLS_PORT,
        "mtls-primary",
        serde_json::json!({
            "enabled": true,
            "ca_cert": lab_ca().to_string_lossy(),
            "client_cert": client_cert().to_string_lossy(),
            "client_key": client_key().to_string_lossy()
        }),
    );
    let queue = "rabbit-rs-it-mtls-client";

    declare_queue(&broker_config, queue).await;
    publish_with_confirms(&broker_config, queue, "msg-mtls-client-1").await;
}

#[tokio::test]
async fn mtls_handshake_fails_without_client_identity() {
    let broker_config = tls_broker_at(
        MTLS_HOST,
        MTLS_PORT,
        "mtls-anonymous",
        serde_json::json!({"enabled": true, "ca_cert": lab_ca().to_string_lossy()}),
    );

    let Err(error) = LapinTransport.connect(&broker_config).await else {
        panic!("a listener requiring client certificates must reject anonymous TLS clients");
    };

    assert!(
        matches!(
            error.kind(),
            TransportErrorKind::Connection | TransportErrorKind::Protocol
        ),
        "expected a typed transport-level handshake failure, got {error:?}"
    );
}

#[tokio::test]
async fn tls_handshake_fails_when_host_is_not_in_the_certificate_san() {
    let broker_config = tls_broker_at(WRONG_SAN_HOST, TLS_PORT, "tls-san-mismatch", trusted_tls());

    let Err(error) = LapinTransport.connect(&broker_config).await else {
        panic!("a host outside the certificate SANs must fail the TLS handshake");
    };

    assert!(
        matches!(
            error.kind(),
            TransportErrorKind::Connection | TransportErrorKind::Protocol
        ),
        "expected a typed transport-level handshake failure, got {error:?}"
    );
}
