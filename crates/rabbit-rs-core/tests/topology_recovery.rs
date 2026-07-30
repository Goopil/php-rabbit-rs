use std::time::Duration;

use rabbit_rs_core::{
    config::{BrokerConfig, Credentials, Endpoint, TlsConfig, TopologyMode},
    topology::{
        DeadLetterDefinition, QueueDefinition, TopologyDefinition, TopologyPlan, TopologyReconciler,
    },
    transport::{
        BindingSpec, ExchangeKind, ExchangeSpec, QueueKind, Transport, TransportError,
        mock::{MockTransport, TransportOperation},
    },
};

fn broker() -> BrokerConfig {
    BrokerConfig {
        name: "primary".to_owned(),
        hosts: vec![Endpoint::new("localhost", 5672)],
        vhost: "/".to_owned(),
        credentials: Credentials::new("guest", "guest"),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(30),
    }
}

fn exchange(name: &str) -> ExchangeSpec {
    ExchangeSpec {
        name: name.to_owned(),
        kind: ExchangeKind::Direct,
        durable: true,
        auto_delete: false,
        internal: false,
    }
}

fn binding(queue: &str, exchange: &str, routing_key: &str) -> BindingSpec {
    BindingSpec {
        queue: queue.to_owned(),
        exchange: exchange.to_owned(),
        routing_key: routing_key.to_owned(),
    }
}

fn definition() -> TopologyDefinition {
    TopologyDefinition::new(
        vec![exchange("jobs")],
        vec![QueueDefinition::new("jobs.high")],
        vec![binding("jobs.high", "jobs", "high")],
    )
}

async fn topology_channel(
    transport: &MockTransport,
) -> Box<dyn rabbit_rs_core::transport::ConsumerChannel> {
    transport
        .connect(&broker())
        .await
        .expect("connection")
        .open_consumer()
        .await
        .expect("consumer channel")
}

#[test]
fn queues_are_durable_quorum_by_default_and_classic_when_explicit() {
    let default_plan = TopologyPlan::compile(TopologyMode::Declare, definition()).expect("plan");
    let classic_plan = TopologyPlan::compile(
        TopologyMode::Declare,
        TopologyDefinition::new(
            vec![],
            vec![QueueDefinition::new("legacy").classic()],
            vec![],
        ),
    )
    .expect("classic plan");

    assert_eq!(default_plan.queues()[0].kind, QueueKind::Quorum);
    assert!(default_plan.queues()[0].durable);
    assert!(!default_plan.queues()[0].exclusive);
    assert!(!default_plan.queues()[0].auto_delete);
    assert_eq!(classic_plan.queues()[0].kind, QueueKind::Classic);
}

#[test]
fn quorum_rejects_exclusive_or_auto_delete_combinations() {
    let exclusive = TopologyPlan::compile(
        TopologyMode::Declare,
        TopologyDefinition::new(
            vec![],
            vec![QueueDefinition::new("invalid").exclusive(true)],
            vec![],
        ),
    );
    let auto_delete = TopologyPlan::compile(
        TopologyMode::Declare,
        TopologyDefinition::new(
            vec![],
            vec![QueueDefinition::new("invalid").auto_delete(true)],
            vec![],
        ),
    );

    assert!(
        exclusive
            .expect_err("exclusive quorum must fail")
            .is_permanent()
    );
    assert!(
        auto_delete
            .expect_err("auto-delete quorum must fail")
            .is_permanent()
    );
}

#[tokio::test]
async fn declare_orders_exchanges_then_queues_then_bindings() {
    let transport = MockTransport::default();
    let channel = topology_channel(&transport).await;
    let plan = TopologyPlan::compile(TopologyMode::Declare, definition()).expect("plan");
    let mut reconciler = TopologyReconciler::new();

    reconciler
        .reconcile(&*channel, &plan, 1)
        .await
        .expect("reconciliation");

    let topology_operations: Vec<_> = transport
        .operations()
        .into_iter()
        .filter(|operation| {
            matches!(
                operation,
                TransportOperation::DeclareExchange(_)
                    | TransportOperation::DeclareQueue(_)
                    | TransportOperation::BindQueue(_)
            )
        })
        .collect();
    assert!(matches!(
        topology_operations.as_slice(),
        [
            TransportOperation::DeclareExchange(_),
            TransportOperation::DeclareQueue(_),
            TransportOperation::BindQueue(_)
        ]
    ));
}

#[tokio::test]
async fn declare_is_idempotent_within_one_connection_generation() {
    let transport = MockTransport::default();
    let channel = topology_channel(&transport).await;
    let plan = TopologyPlan::compile(TopologyMode::Declare, definition()).expect("plan");
    let mut reconciler = TopologyReconciler::new();

    reconciler
        .reconcile(&*channel, &plan, 7)
        .await
        .expect("first");
    let after_first = transport.operations();
    reconciler
        .reconcile(&*channel, &plan, 7)
        .await
        .expect("second");

    assert_eq!(transport.operations(), after_first);
}

#[tokio::test]
async fn verify_uses_passive_checks_without_creating_bindings() {
    let transport = MockTransport::default();
    let channel = topology_channel(&transport).await;
    let plan = TopologyPlan::compile(TopologyMode::Verify, definition()).expect("plan");
    let mut reconciler = TopologyReconciler::new();

    reconciler
        .reconcile(&*channel, &plan, 1)
        .await
        .expect("verification");
    let operations = transport.operations();

    assert!(
        operations
            .iter()
            .any(|operation| matches!(operation, TransportOperation::VerifyExchange(_)))
    );
    assert!(
        operations
            .iter()
            .any(|operation| matches!(operation, TransportOperation::VerifyQueue(_)))
    );
    assert!(!operations.iter().any(|operation| matches!(
        operation,
        TransportOperation::DeclareExchange(_)
            | TransportOperation::DeclareQueue(_)
            | TransportOperation::BindQueue(_)
    )));
}

#[tokio::test]
async fn external_mode_emits_no_topology_command() {
    let transport = MockTransport::default();
    let channel = topology_channel(&transport).await;
    let plan = TopologyPlan::compile(TopologyMode::External, definition()).expect("plan");
    let mut reconciler = TopologyReconciler::new();

    reconciler
        .reconcile(&*channel, &plan, 1)
        .await
        .expect("external mode");

    assert_eq!(transport.operations().len(), 2);
}

#[test]
fn application_dead_letter_topology_is_absent_by_default() {
    let plan = TopologyPlan::compile(TopologyMode::Declare, definition()).expect("plan");

    assert_eq!(plan.exchanges().len(), 1);
    assert_eq!(plan.queues().len(), 1);
    assert_eq!(plan.bindings().len(), 1);
    assert!(plan.queues()[0].dead_letter_exchange.is_none());
}

#[test]
fn application_dead_letter_topology_is_compiled_only_when_enabled() {
    let topology = definition().with_dead_letter(DeadLetterDefinition::new(
        "jobs.high",
        "jobs.dlx",
        "jobs.failed",
        "failed",
    ));
    let plan = TopologyPlan::compile(TopologyMode::Declare, topology).expect("plan");

    assert_eq!(plan.exchanges().len(), 2);
    assert_eq!(plan.queues().len(), 2);
    assert_eq!(plan.bindings().len(), 2);
    assert_eq!(
        plan.queues()[0].dead_letter_exchange.as_deref(),
        Some("jobs.dlx")
    );
}

#[tokio::test]
async fn topology_incompatibility_is_reported_as_permanent() {
    let transport = MockTransport::default();
    transport.push_operation_result(Err(TransportError::protocol(
        "PRECONDITION_FAILED inequivalent arg x-queue-type",
    )));
    let channel = topology_channel(&transport).await;
    let plan = TopologyPlan::compile(TopologyMode::Declare, definition()).expect("plan");
    let mut reconciler = TopologyReconciler::new();

    let error = reconciler
        .reconcile(&*channel, &plan, 1)
        .await
        .expect_err("incompatible topology");

    assert!(error.is_permanent());
}

#[tokio::test]
async fn a_new_connection_generation_replays_the_full_plan() {
    let transport = MockTransport::default();
    let channel = topology_channel(&transport).await;
    let plan = TopologyPlan::compile(TopologyMode::Declare, definition()).expect("plan");
    let mut reconciler = TopologyReconciler::new();

    reconciler
        .reconcile(&*channel, &plan, 1)
        .await
        .expect("generation one");
    reconciler
        .reconcile(&*channel, &plan, 2)
        .await
        .expect("generation two");

    let declarations = transport
        .operations()
        .into_iter()
        .filter(|operation| {
            matches!(
                operation,
                TransportOperation::DeclareExchange(_)
                    | TransportOperation::DeclareQueue(_)
                    | TransportOperation::BindQueue(_)
            )
        })
        .count();
    assert_eq!(declarations, 6);
}
