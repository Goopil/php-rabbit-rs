//! Integration tests for topology modes against a real `RabbitMQ` broker.
#![cfg(feature = "integration")]

use std::time::Duration;

use rabbit_rs_core::{
    config::{BrokerConfig, Credentials, Endpoint, TlsConfig, TopologyMode},
    topology::{QueueDefinition, TopologyDefinition, TopologyPlan},
    transport::{ExchangeKind, ExchangeSpec, Transport, lapin::LapinTransport},
};

async fn connect() -> Box<dyn rabbit_rs_core::transport::TransportConnection> {
    let broker = BrokerConfig {
        name: "primary".to_owned(),
        hosts: vec![Endpoint::new("localhost", 5672)],
        vhost: "/orders-eu".to_owned(),
        credentials: Credentials::new("rabbit_rs", "rabbit_rs_lab"),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(30),
    };
    LapinTransport
        .connect(&broker)
        .await
        .expect("connect to broker")
}

#[tokio::test]
async fn declare_quorum_queue_succeeds() {
    let conn = connect().await;
    let channel = conn.open_publisher().await.expect("publisher channel");

    let plan = TopologyPlan::compile(
        TopologyMode::Declare,
        TopologyDefinition::new(
            vec![ExchangeSpec {
                name: "rabbit-rs-it-topo".to_owned(),
                kind: ExchangeKind::Direct,
                durable: true,
                auto_delete: false,
                internal: false,
            }],
            vec![QueueDefinition::new("rabbit-rs-it-quorum-queue")],
            vec![],
        ),
    )
    .expect("compile plan");

    let mut reconciler = rabbit_rs_core::topology::TopologyReconciler::new();
    reconciler
        .reconcile(channel.as_ref(), &plan, 1)
        .await
        .expect("reconcile declare");

    channel.close().await.expect("close channel");
    conn.close().await.expect("close connection");
}

#[tokio::test]
async fn declare_classic_queue_succeeds() {
    let conn = connect().await;
    let channel = conn.open_publisher().await.expect("publisher channel");

    let plan = TopologyPlan::compile(
        TopologyMode::Declare,
        TopologyDefinition::new(
            vec![],
            vec![QueueDefinition::new("rabbit-rs-it-classic-queue").classic()],
            vec![],
        ),
    )
    .expect("compile plan");

    let mut reconciler = rabbit_rs_core::topology::TopologyReconciler::new();
    reconciler
        .reconcile(channel.as_ref(), &plan, 1)
        .await
        .expect("reconcile declare classic");

    channel.close().await.expect("close channel");
    conn.close().await.expect("close connection");
}

#[tokio::test]
async fn verify_passive_does_not_create() {
    let conn = connect().await;
    let channel = conn.open_publisher().await.expect("publisher channel");

    let plan = TopologyPlan::compile(
        TopologyMode::Declare,
        TopologyDefinition::new(
            vec![ExchangeSpec {
                name: "rabbit-rs-it-verify-ex".to_owned(),
                kind: ExchangeKind::Direct,
                durable: true,
                auto_delete: false,
                internal: false,
            }],
            vec![QueueDefinition::new("rabbit-rs-it-verify-queue")],
            vec![],
        ),
    )
    .expect("compile declare plan");

    let mut reconciler = rabbit_rs_core::topology::TopologyReconciler::new();
    reconciler
        .reconcile(channel.as_ref(), &plan, 1)
        .await
        .expect("declare first");

    let verify_plan = TopologyPlan::compile(
        TopologyMode::Verify,
        TopologyDefinition::new(
            vec![ExchangeSpec {
                name: "rabbit-rs-it-verify-ex".to_owned(),
                kind: ExchangeKind::Direct,
                durable: true,
                auto_delete: false,
                internal: false,
            }],
            vec![QueueDefinition::new("rabbit-rs-it-verify-queue")],
            vec![],
        ),
    )
    .expect("compile verify plan");

    reconciler
        .reconcile(channel.as_ref(), &verify_plan, 2)
        .await
        .expect("verify should succeed on existing topology");

    channel.close().await.expect("close channel");
    conn.close().await.expect("close connection");
}

#[tokio::test]
async fn external_mode_emits_no_commands() {
    let conn = connect().await;
    let channel = conn.open_publisher().await.expect("publisher channel");

    let plan = TopologyPlan::compile(
        TopologyMode::External,
        TopologyDefinition::new(vec![], vec![], vec![]),
    )
    .expect("compile external plan");

    let mut reconciler = rabbit_rs_core::topology::TopologyReconciler::new();
    reconciler
        .reconcile(channel.as_ref(), &plan, 1)
        .await
        .expect("external mode should be no-op");

    channel.close().await.expect("close channel");
    conn.close().await.expect("close connection");
}
