use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use rabbit_rs_core::{
    config::{
        BrokerConfig, Config, ConsumerConfigSection, DeadLetterConfig, DelayConfig,
        PublisherConfigSection, SafetyMode, SchedulerConfig, SubscriptionConfig, TopologyMode,
        ValidatedConfig, WorkerProfile,
    },
    consumer::{
        APPLICATION_ATTEMPTS_HEADER, AttemptsErrorKind, AttemptsResolver, ConsumerSet, Headers,
        Subscription,
    },
    metrics::Metrics,
    pool::ConnectionKey,
    publisher::{Destination, PublisherActor, PublisherConfig},
    topology::{
        DeadLetterDefinition, QueueDefinition, TopologyDefinition, TopologyPlan, TopologyReconciler,
    },
    transport::{
        BindingSpec, ExchangeKind, ExchangeSpec, HeaderValue, Headers as TransportHeaders,
        QueueKind, QueueSpec, Transport, TransportError,
        mock::{MockTransport, TransportOperation},
    },
};

mod common;

mod helper {
    use super::*;

    pub use crate::common::broker;

    #[must_use]
    pub fn broker_default() -> BrokerConfig {
        broker("default", "/", "guest")
    }

    pub fn exchange(name: &str) -> ExchangeSpec {
        ExchangeSpec {
            name: name.to_owned(),
            kind: ExchangeKind::Direct,
            durable: true,
            auto_delete: false,
            internal: false,
            arguments: rabbit_rs_core::transport::Headers::new(),
        }
    }

    pub fn binding(queue: &str, exchange: &str, routing_key: &str) -> BindingSpec {
        BindingSpec {
            queue: queue.to_owned(),
            exchange: exchange.to_owned(),
            routing_key: routing_key.to_owned(),
        }
    }

    pub fn definition() -> TopologyDefinition {
        TopologyDefinition::new(
            vec![exchange("jobs")],
            vec![QueueDefinition::new("jobs.high")],
            vec![binding("jobs.high", "jobs", "high")],
        )
    }

    pub async fn topology_channel(
        transport: &MockTransport,
    ) -> Box<dyn rabbit_rs_core::transport::ConsumerChannel> {
        transport
            .connect(&broker("primary", "/", "guest"))
            .await
            .expect("connection")
            .open_consumer()
            .await
            .expect("consumer channel")
    }

    pub fn connection_key() -> ConnectionKey {
        ConnectionKey::from_config(
            &Config {
                brokers: vec![broker_default()],
                workers: vec![],
                topology_mode: TopologyMode::External,
                delay: DelayConfig::default(),
                dead_letter: None,
                delivery_limit: None,
                publisher: PublisherConfigSection::default(),
                consumer: ConsumerConfigSection::default(),
                queue_type: QueueKind::Quorum,
                queue_durable: true,
            }
            .validate()
            .expect("valid config"),
        )
    }

    pub fn subscription(queue: &str) -> SubscriptionConfig {
        SubscriptionConfig {
            name: queue.to_owned(),
            broker: "primary".to_owned(),
            queue: queue.to_owned(),
            weight: 1,
            priority_class: 0,
            prefetch: 8,
            starvation_after: Duration::from_secs(30),
            max_buffered_bytes: 64 * 1024 * 1024,
            early_ack: false,
            no_ack: false,
        }
    }

    pub fn base_config(queue: &str) -> Config {
        Config {
            brokers: vec![broker("primary", "/", "guest")],
            workers: vec![WorkerProfile {
                name: "main".to_owned(),
                subscriptions: vec![subscription(queue)],
                scheduler: SchedulerConfig::weighted_fair(),
            }],
            topology_mode: TopologyMode::Declare,
            delay: DelayConfig::default(),
            dead_letter: None,
            delivery_limit: None,
            publisher: PublisherConfigSection::default(),
            consumer: ConsumerConfigSection::default(),
            queue_type: QueueKind::Quorum,
            queue_durable: true,
        }
    }

    pub fn config_with_dead_letter(queue: &str) -> Config {
        let mut config = base_config(queue);
        config.dead_letter = Some(DeadLetterConfig {
            enabled: true,
            exchange: "jobs.dlx".to_owned(),
            queue: "jobs.failed".to_owned(),
            routing_key: Some("failed".to_owned()),
        });
        config
    }

    pub fn config_with_delivery_limit(queue: &str, limit: u32) -> Config {
        let mut config = base_config(queue);
        config.delivery_limit = Some(limit);
        config
    }

    pub fn config_with_queue_type(queue: &str, kind: QueueKind) -> Config {
        let mut config = base_config(queue);
        config.queue_type = kind;
        config
    }

    pub fn config_with_queue_durable(queue: &str, durable: bool) -> Config {
        let mut config = base_config(queue);
        config.queue_durable = durable;
        config
    }

    pub fn build_plan_from_config(config: &ValidatedConfig) -> TopologyPlan {
        let queue_type = config.queue_type();
        let queue_durable = config.queue_durable();
        let queues: Vec<_> = config
            .worker_profiles()
            .iter()
            .flat_map(|worker| &worker.subscriptions)
            .map(|sub| {
                let mut qd = QueueDefinition::new(&sub.queue)
                    .kind(queue_type)
                    .durable(queue_durable);
                if let Some(limit) = config.delivery_limit() {
                    qd = qd.delivery_limit(limit);
                }
                qd
            })
            .collect();

        let mut topology = TopologyDefinition::new(vec![], queues, vec![]);
        if let Some(dl) = config.dead_letter()
            && dl.enabled
        {
            for sub in config
                .worker_profiles()
                .iter()
                .flat_map(|w| &w.subscriptions)
            {
                let routing_key = dl.routing_key.clone().unwrap_or_else(|| sub.queue.clone());
                topology = topology.with_dead_letter(DeadLetterDefinition::new(
                    sub.queue.clone(),
                    dl.exchange.clone(),
                    dl.queue.clone(),
                    routing_key,
                ));
            }
        }

        TopologyPlan::compile(config.topology_mode(), topology).unwrap_or_else(|_error| {
            TopologyPlan::compile(
                TopologyMode::External,
                TopologyDefinition::new(vec![], vec![], vec![]),
            )
            .expect("external mode always compiles")
        })
    }

    pub fn attempt_headers(values: &[(&str, &str)]) -> Headers {
        values
            .iter()
            .map(|(name, value)| {
                (
                    (*name).to_owned(),
                    HeaderValue::Binary(Bytes::copy_from_slice(value.as_bytes())),
                )
            })
            .collect()
    }
}

use helper::*;

// ---------------------------------------------------------------------------
// Topology recovery tests (from topology_recovery.rs)
// ---------------------------------------------------------------------------

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

    exclusive.expect_err("exclusive quorum must fail");
    auto_delete.expect_err("auto-delete quorum must fail");
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

#[test]
fn two_subscriptions_sharing_a_dead_letter_queue_produce_one_binding_per_routing_key() {
    let mut config = base_config("jobs.one");
    config.dead_letter = Some(DeadLetterConfig {
        enabled: true,
        exchange: "jobs.dlx".to_owned(),
        queue: "jobs.failed".to_owned(),
        routing_key: None, // per-source defaults apply: routing key = sub queue
    });
    // Second subscription sharing the same dead-letter queue.
    config.workers[0]
        .subscriptions
        .push(subscription("jobs.two"));

    let plan = build_plan_from_config(&config.validate().expect("valid config"));

    // The shared DLQ is still declared exactly once.
    assert_eq!(
        plan.queues()
            .iter()
            .filter(|queue| queue.name == "jobs.failed")
            .count(),
        1,
        "the shared dead-letter queue must be declared exactly once",
    );

    // One binding per (dlq, routing_key) pair. The DLX republish is not
    // mandatory, so a missing binding silently drops dead-lettered messages
    // (audit F-05).
    let dlq_bindings: Vec<_> = plan
        .bindings()
        .iter()
        .filter(|binding| binding.queue == "jobs.failed")
        .collect();
    assert_eq!(
        dlq_bindings.len(),
        2,
        "both subscriptions must have a binding into the shared DLQ",
    );
    let routing_keys: Vec<_> = dlq_bindings
        .iter()
        .map(|binding| binding.routing_key.as_str())
        .collect();
    assert!(routing_keys.contains(&"jobs.one"));
    assert!(routing_keys.contains(&"jobs.two"));
}

#[tokio::test]
async fn an_incompatible_topology_fails_reconciliation() {
    let transport = MockTransport::default();
    transport.push_operation_result(Err(TransportError::protocol(
        "PRECONDITION_FAILED inequivalent arg x-queue-type",
    )));
    let channel = topology_channel(&transport).await;
    let plan = TopologyPlan::compile(TopologyMode::Declare, definition()).expect("plan");
    let mut reconciler = TopologyReconciler::new();

    reconciler
        .reconcile(&*channel, &plan, 1)
        .await
        .expect_err("incompatible topology");
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

// ---------------------------------------------------------------------------
// Topology modes tests (from topology_modes.rs — integration-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "integration")]
async fn integration_connect() -> Box<dyn rabbit_rs_core::transport::TransportConnection> {
    use rabbit_rs_core::config::{Credentials, Endpoint, TlsConfig};
    use rabbit_rs_core::transport::lapin::LapinTransport;

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

#[cfg(feature = "integration")]
#[tokio::test]
async fn declare_quorum_queue_succeeds() {
    let conn = integration_connect().await;
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
                arguments: rabbit_rs_core::transport::Headers::new(),
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

#[cfg(feature = "integration")]
#[tokio::test]
async fn declare_classic_queue_succeeds() {
    let conn = integration_connect().await;
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

#[cfg(feature = "integration")]
#[tokio::test]
async fn verify_passive_does_not_create() {
    let conn = integration_connect().await;
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
                arguments: rabbit_rs_core::transport::Headers::new(),
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
                arguments: rabbit_rs_core::transport::Headers::new(),
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

#[cfg(feature = "integration")]
#[tokio::test]
async fn external_mode_emits_no_commands() {
    let conn = integration_connect().await;
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

// ---------------------------------------------------------------------------
// DLQ topology tests (from dlq_topology.rs)
// ---------------------------------------------------------------------------

#[test]
fn validated_config_preserves_dead_letter_config() {
    let config = config_with_dead_letter("jobs.high");
    let validated = config.validate().expect("valid config");

    let dl = validated.dead_letter().expect("dead letter config present");
    assert!(dl.enabled);
    assert_eq!(dl.exchange, "jobs.dlx");
    assert_eq!(dl.queue, "jobs.failed");
    assert_eq!(dl.routing_key.as_deref(), Some("failed"));
}

#[test]
fn validated_config_preserves_delivery_limit() {
    let config = config_with_delivery_limit("jobs.high", 20);
    let validated = config.validate().expect("valid config");

    assert_eq!(validated.delivery_limit(), Some(20));
}

#[test]
fn config_without_dead_letter_has_no_dlq() {
    let config = base_config("jobs.high");
    let validated = config.validate().expect("valid config");

    assert!(validated.dead_letter().is_none());
}

#[test]
fn dead_letter_config_compiles_to_dlx_dlq_and_binding() {
    let config = config_with_dead_letter("jobs.high");
    let validated = config.validate().expect("valid config");
    let plan = build_plan_from_config(&validated);

    assert!(
        plan.exchanges().iter().any(|e| e.name == "jobs.dlx"),
        "plan should declare the DLX exchange"
    );
    assert!(
        plan.queues().iter().any(|q| q.name == "jobs.failed"),
        "plan should declare the DLQ"
    );
    assert!(
        plan.queues()
            .iter()
            .any(|q| q.dead_letter_exchange.as_deref() == Some("jobs.dlx")),
        "source queue should have dead_letter_exchange set"
    );
    assert!(
        plan.bindings()
            .iter()
            .any(|b| b.exchange == "jobs.dlx" && b.queue == "jobs.failed"),
        "plan should bind DLQ to DLX"
    );
}

#[tokio::test]
async fn reconciler_declares_dlx_dlq_and_binding_for_dead_letter_config() {
    let config = config_with_dead_letter("jobs.high");
    let validated = config.validate().expect("valid config");
    let plan = build_plan_from_config(&validated);

    let transport = MockTransport::default();
    let channel = topology_channel(&transport).await;
    let mut reconciler = TopologyReconciler::new();

    reconciler
        .reconcile(&*channel, &plan, 1)
        .await
        .expect("reconciliation");

    let operations = transport.operations();
    assert!(
        operations.iter().any(|op| matches!(
            op,
            TransportOperation::DeclareExchange(e) if e.name == "jobs.dlx"
        )),
        "should declare DLX exchange"
    );
    assert!(
        operations.iter().any(|op| matches!(
            op,
            TransportOperation::DeclareQueue(q) if q.name == "jobs.failed"
        )),
        "should declare DLQ"
    );
    assert!(
        operations.iter().any(|op| matches!(
            op,
            TransportOperation::BindQueue(b)
                if b.exchange == "jobs.dlx" && b.queue == "jobs.failed"
        )),
        "should bind DLQ to DLX"
    );
}

#[test]
fn delivery_limit_emits_x_delivery_limit_on_queue_spec() {
    let config = config_with_delivery_limit("jobs.high", 20);
    let validated = config.validate().expect("valid config");
    let plan = build_plan_from_config(&validated);

    let queue = plan
        .queues()
        .iter()
        .find(|q| q.name == "jobs.high")
        .expect("source queue in plan");
    assert_eq!(
        queue.delivery_limit,
        Some(20),
        "QueueSpec should carry delivery_limit"
    );
}

#[tokio::test]
async fn reconciler_declares_queue_with_delivery_limit() {
    let config = config_with_delivery_limit("jobs.high", 20);
    let validated = config.validate().expect("valid config");
    let plan = build_plan_from_config(&validated);

    let transport = MockTransport::default();
    let channel = topology_channel(&transport).await;
    let mut reconciler = TopologyReconciler::new();

    reconciler
        .reconcile(&*channel, &plan, 1)
        .await
        .expect("reconciliation");

    let operations = transport.operations();
    assert!(
        operations.iter().any(|op| matches!(
            op,
            TransportOperation::DeclareQueue(q)
                if q.name == "jobs.high" && q.delivery_limit == Some(20)
        )),
        "should declare queue with delivery_limit"
    );
}

#[test]
fn config_without_dead_letter_produces_no_dlx() {
    let config = base_config("jobs.high");
    let validated = config.validate().expect("valid config");
    let plan = build_plan_from_config(&validated);

    assert!(
        plan.exchanges().iter().all(|e| e.name != "jobs.dlx"),
        "no DLX when dead_letter is absent"
    );
    assert!(
        plan.queues().iter().all(|q| q.name != "jobs.failed"),
        "no DLQ when dead_letter is absent"
    );
    assert!(
        plan.queues()
            .iter()
            .all(|q| q.dead_letter_exchange.is_none()),
        "no dead_letter_exchange when dead_letter is absent"
    );
}

#[test]
fn disabled_dead_letter_config_produces_no_dlx() {
    let mut config = base_config("jobs.high");
    config.dead_letter = Some(DeadLetterConfig {
        enabled: false,
        exchange: "jobs.dlx".to_owned(),
        queue: "jobs.failed".to_owned(),
        routing_key: Some("failed".to_owned()),
    });
    let validated = config.validate().expect("valid config");
    let plan = build_plan_from_config(&validated);

    assert!(
        plan.exchanges().iter().all(|e| e.name != "jobs.dlx"),
        "no DLX when dead_letter is disabled"
    );
}

#[test]
fn generic_queue_arguments_are_preserved_in_queue_spec() {
    let mut arguments = TransportHeaders::new();
    arguments.insert("x-max-priority".to_owned(), HeaderValue::Integer(10));
    let spec = QueueSpec {
        name: "priority.jobs".to_owned(),
        durable: true,
        exclusive: false,
        auto_delete: false,
        kind: QueueKind::Quorum,
        dead_letter_exchange: None,
        dead_letter_routing_key: None,
        message_ttl: None,
        expires: None,
        delivery_limit: None,
        arguments,
    };

    assert_eq!(
        spec.arguments.get("x-max-priority"),
        Some(&HeaderValue::Integer(10))
    );
}

#[test]
fn dead_letter_applies_to_all_worker_queues() {
    let mut config = base_config("orders");
    config.workers = vec![WorkerProfile {
        name: "main".to_owned(),
        subscriptions: vec![subscription("orders"), subscription("billing")],
        scheduler: SchedulerConfig::weighted_fair(),
    }];
    config.dead_letter = Some(DeadLetterConfig {
        enabled: true,
        exchange: "global.dlx".to_owned(),
        queue: "global.failed".to_owned(),
        routing_key: Some("dead".to_owned()),
    });
    let validated = config.validate().expect("valid config");
    let plan = build_plan_from_config(&validated);

    assert!(
        plan.queues()
            .iter()
            .any(|q| q.name == "orders" && q.dead_letter_exchange.as_deref() == Some("global.dlx")),
        "orders queue should dead-letter to global.dlx"
    );
    assert!(
        plan.queues()
            .iter()
            .any(|q| q.name == "billing" && q.dead_letter_exchange.as_deref() == Some("global.dlx")),
        "billing queue should dead-letter to global.dlx"
    );
    assert_eq!(
        plan.exchanges()
            .iter()
            .filter(|e| e.name == "global.dlx")
            .count(),
        1,
        "DLX should be declared exactly once even with multiple source queues"
    );
    assert_eq!(
        plan.queues()
            .iter()
            .filter(|q| q.name == "global.failed")
            .count(),
        1,
        "DLQ should be declared exactly once"
    );
}

// ---------------------------------------------------------------------------
// Queue type and durable from config tests
// ---------------------------------------------------------------------------

#[test]
fn queue_type_classic_from_config() {
    let config = config_with_queue_type("jobs.high", QueueKind::Classic);
    let validated = config.validate().expect("valid config");
    let plan = build_plan_from_config(&validated);

    assert_eq!(
        plan.queues()[0].kind,
        QueueKind::Classic,
        "queue kind should come from config.queue_type"
    );
}

#[test]
fn queue_type_quorum_from_config() {
    let config = config_with_queue_type("jobs.high", QueueKind::Quorum);
    let validated = config.validate().expect("valid config");
    let plan = build_plan_from_config(&validated);

    assert_eq!(
        plan.queues()[0].kind,
        QueueKind::Quorum,
        "quorum queue kind should be preserved from config"
    );
}

#[test]
fn queue_durable_false_from_config() {
    let config = config_with_queue_durable("jobs.high", false);
    let validated = config.validate().expect("valid config");
    let plan = build_plan_from_config(&validated);

    assert!(
        !plan.queues()[0].durable,
        "durable flag should come from config.queue_durable"
    );
}

#[test]
fn queue_durable_true_from_config() {
    let config = config_with_queue_durable("jobs.high", true);
    let validated = config.validate().expect("valid config");
    let plan = build_plan_from_config(&validated);

    assert!(
        plan.queues()[0].durable,
        "durable=true should be preserved from config"
    );
}

#[test]
fn queue_defaults_to_quorum_and_durable() {
    let config = base_config("jobs.high");
    let validated = config.validate().expect("valid config");
    let plan = build_plan_from_config(&validated);

    assert_eq!(plan.queues()[0].kind, QueueKind::Quorum);
    assert!(plan.queues()[0].durable);
}

// ---------------------------------------------------------------------------
// Delivery attempts tests (from delivery_attempts.rs)
// ---------------------------------------------------------------------------

#[test]
fn first_acquisition_is_attempt_one() {
    let attempts = AttemptsResolver::default()
        .resolve(&Headers::new(), false)
        .expect("first acquisition");

    assert_eq!(attempts, 1);
}

#[test]
fn acquired_count_takes_precedence_over_redelivered_flag() {
    let attempts = AttemptsResolver::default()
        .resolve(&attempt_headers(&[("x-acquired-count", "7")]), true)
        .expect("RabbitMQ 4.3 acquired count");

    assert_eq!(attempts, 7);
}

#[test]
fn quorum_delivery_count_is_converted_from_failures_to_current_attempt() {
    let attempts = AttemptsResolver::default()
        .resolve(&attempt_headers(&[("x-delivery-count", "3")]), true)
        .expect("quorum delivery count");

    assert_eq!(attempts, 4);
}

#[test]
fn application_count_survives_a_fresh_broker_delivery() {
    let attempts = AttemptsResolver::default()
        .resolve(
            &attempt_headers(&[(APPLICATION_ATTEMPTS_HEADER, "5")]),
            false,
        )
        .expect("application retry count");

    assert_eq!(attempts, 5);
}

#[test]
fn exceeding_the_configured_limit_is_a_typed_max_attempts_error() {
    let resolver = AttemptsResolver::default();

    let error = resolver
        .resolve(
            &attempt_headers(&[(APPLICATION_ATTEMPTS_HEADER, "21")]),
            false,
        )
        .expect_err("twenty-first attempt exceeds the default limit of twenty");

    assert_eq!(error.kind(), AttemptsErrorKind::MaxAttempts);
    assert_eq!(error.attempts(), 21);
    assert_eq!(error.max_attempts(), Some(20));
}

#[test]
fn default_attempt_limit_is_inclusive_at_twenty() {
    let resolver = AttemptsResolver::default();

    assert_eq!(
        resolver
            .resolve(
                &attempt_headers(&[(APPLICATION_ATTEMPTS_HEADER, "20")]),
                false
            )
            .expect("twentieth attempt is accepted"),
        20
    );
    let error = resolver
        .resolve(
            &attempt_headers(&[(APPLICATION_ATTEMPTS_HEADER, "21")]),
            false,
        )
        .expect_err("twenty-first attempt exceeds the default limit");
    assert_eq!(error.kind(), AttemptsErrorKind::MaxAttempts);
    assert_eq!(error.max_attempts(), Some(20));
}

#[test]
fn classic_queue_without_counters_uses_the_documented_redelivery_fallback() {
    let resolver = AttemptsResolver::default();

    assert_eq!(
        resolver
            .resolve(&Headers::new(), false)
            .expect("fresh classic delivery"),
        1
    );
    assert_eq!(
        resolver
            .resolve(&Headers::new(), true)
            .expect("classic redelivery"),
        2
    );
}

#[tokio::test]
async fn broker_message_id_is_preserved_as_delivery_id() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(rabbit_rs_core::transport::Delivery {
        delivery_tag: 42,
        exchange: "jobs".to_owned(),
        routing_key: "high".to_owned(),
        redelivered: false,
        headers: Arc::new(Headers::new()),
        payload: Bytes::from_static(b"job"),
        message_id: Some("uuid-stable-job-id".to_owned()),
        correlation_id: Some("corr-1".to_owned()),
    }));
    let connection = transport
        .connect(&broker_default())
        .await
        .expect("connection");
    let consumer_channel = connection.open_consumer().await.expect("consumer channel");
    let subscription = Subscription::new(
        "jobs",
        connection_key(),
        "jobs",
        Arc::from(consumer_channel),
    );
    let consumer = ConsumerSet::spawn_with_metrics(vec![subscription], Metrics::default())
        .await
        .expect("consumer set");
    let delivery = consumer.next().await.expect("delivery");

    assert_eq!(
        delivery.id.as_str(),
        "uuid-stable-job-id",
        "delivery id must use the broker message_id, not a synthetic tag"
    );
}

#[tokio::test]
async fn missing_broker_message_id_falls_back_to_synthetic_id() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(rabbit_rs_core::transport::Delivery {
        delivery_tag: 42,
        exchange: "jobs".to_owned(),
        routing_key: "high".to_owned(),
        redelivered: false,
        headers: Arc::new(Headers::new()),
        payload: Bytes::from_static(b"job"),
        message_id: None,
        correlation_id: None,
    }));
    let connection = transport
        .connect(&broker_default())
        .await
        .expect("connection");
    let consumer_channel = connection.open_consumer().await.expect("consumer channel");
    let subscription = Subscription::new(
        "jobs",
        connection_key(),
        "jobs",
        Arc::from(consumer_channel),
    );
    let consumer = ConsumerSet::spawn_with_metrics(vec![subscription], Metrics::default())
        .await
        .expect("consumer set");
    let delivery = consumer.next().await.expect("delivery");

    assert!(
        !delivery.id.as_str().is_empty(),
        "delivery id must have a fallback synthetic id"
    );
    assert!(
        delivery.id.as_str().contains('1'),
        "synthetic id should contain the generation: {}",
        delivery.id.as_str()
    );
}

#[tokio::test(start_paused = true)]
async fn delayed_release_increments_the_application_attempt_header() {
    let transport = MockTransport::default();
    transport.push_delivery(Ok(rabbit_rs_core::transport::Delivery {
        delivery_tag: 8,
        exchange: "jobs".to_owned(),
        routing_key: "high".to_owned(),
        redelivered: false,
        message_id: None,
        correlation_id: None,
        headers: Arc::new(attempt_headers(&[
            (APPLICATION_ATTEMPTS_HEADER, "2"),
            ("trace-id", "trace-42"),
            ("x-delivery-count", "1"),
        ])),
        payload: Bytes::from_static(b"job"),
    }));
    transport.push_confirmation(Ok(rabbit_rs_core::transport::PublishConfirmation::Ack(
        None,
    )));
    let connection = transport
        .connect(&broker_default())
        .await
        .expect("connection");
    let consumer_channel = connection.open_consumer().await.expect("consumer channel");
    let publisher_channel = connection
        .open_publisher()
        .await
        .expect("publisher channel");
    let publisher = PublisherActor::spawn_with_delay_strategy_and_metrics(
        Arc::from(publisher_channel),
        PublisherConfig::with_safety(8, Duration::from_secs(5), SafetyMode::Safe),
        Metrics::default(),
        None,
    );
    let subscription = Subscription::new(
        "jobs",
        connection_key(),
        "jobs",
        Arc::from(consumer_channel),
    )
    .delayed_publisher(publisher, Destination::new("jobs", "high"))
    .delay_strategy(rabbit_rs_core::topology::delay::DelayStrategy::Plugin);
    let consumer = ConsumerSet::spawn_with_metrics(vec![subscription], Metrics::default())
        .await
        .expect("consumer set");
    let delivery = consumer.next().await.expect("delivery");

    assert_eq!(delivery.attempts, 2);
    delivery
        .release(Duration::from_secs(5))
        .await
        .expect("delayed release enqueued");

    tokio::time::advance(Duration::from_millis(10)).await;
    tokio::task::yield_now().await;

    let published_request = transport
        .operations()
        .into_iter()
        .find_map(|operation| match operation {
            TransportOperation::Publish(request) => Some(request),
            _ => None,
        })
        .expect("republished message");
    assert_eq!(
        published_request
            .properties
            .headers
            .get(APPLICATION_ATTEMPTS_HEADER),
        Some(&HeaderValue::Integer(3))
    );
    assert_eq!(
        published_request.properties.headers.get("trace-id"),
        Some(&HeaderValue::Binary(Bytes::from_static(b"trace-42")))
    );
    assert!(
        !published_request
            .properties
            .headers
            .contains_key("x-delivery-count")
    );
}

// ---------------------------------------------------------------------------
// Delay queue identity (issue #79): the queue name must bind the arguments
// so two configs with different margins never fight over one declaration.
// ---------------------------------------------------------------------------

fn ttl_plan_with_margin(margin: Duration) -> rabbit_rs_core::topology::delay::TtlBucketPlan {
    rabbit_rs_core::topology::delay::TtlBucketPlan::compile(&DelayConfig {
        mode: rabbit_rs_core::config::DelayMode::Ttl,
        buckets: vec![
            Duration::from_secs(1),
            Duration::from_secs(5),
            Duration::from_secs(30),
        ],
        max_buckets: 8,
        queue_expiry_margin: margin,
    })
    .expect("valid TTL plan")
}

#[test]
fn delay_queues_with_different_arguments_get_different_names() {
    let short_margin = ttl_plan_with_margin(Duration::from_mins(1));
    let long_margin = ttl_plan_with_margin(Duration::from_mins(2));
    let destination = Destination::new("jobs", "high");

    let short = short_margin
        .queue_for(&destination, Duration::from_secs(5))
        .expect("queue for short margin");
    let long = long_margin
        .queue_for(&destination, Duration::from_secs(5))
        .expect("queue for long margin");

    assert_ne!(
        short.name, long.name,
        "distinct delay arguments must not declare the same queue name: \
         rolling deploys would fight over one declaration (406 storms)"
    );
    assert!(short.name.starts_with("rabbit-rs.delay."));
    assert!(long.name.starts_with("rabbit-rs.delay."));
    assert!(
        short.name.ends_with(".5000") && long.name.ends_with(".5000"),
        "the bucket stays readable in the name: {} vs {}",
        short.name,
        long.name
    );
    assert_eq!(short.expires, Some(Duration::from_secs(65)));
    assert_eq!(long.expires, Some(Duration::from_secs(125)));
}

#[test]
fn identical_delay_arguments_reuse_the_same_queue_name() {
    let plan = ttl_plan_with_margin(Duration::from_mins(1));
    let destination = Destination::new("jobs", "high");

    let first = plan
        .queue_for(&destination, Duration::from_secs(5))
        .expect("first queue");
    let second = plan
        .queue_for(&destination, Duration::from_secs(5))
        .expect("second queue");

    assert_eq!(first.name, second.name);
}

// ---------------------------------------------------------------------------
// Synthesized delay-queue GC (issue #79): a conservative maintenance sweep.
// ---------------------------------------------------------------------------

use rabbit_rs_core::topology::delay::{
    SweepKeptReason, legacy_delay_queue_name, sweep_delay_queues,
};

#[tokio::test]
async fn delay_queue_sweep_deletes_orphans_and_keeps_live_queues() {
    let transport = MockTransport::default();
    let connection = transport
        .connect(&broker_default())
        .await
        .expect("connection");
    let channel = connection
        .open_publisher()
        .await
        .expect("publisher channel");

    let plan = ttl_plan_with_margin(Duration::from_mins(1));
    let destination = Destination::new("jobs", "high");
    let live = plan
        .queue_for(&destination, Duration::from_secs(5))
        .expect("live queue");
    let orphan_1s = legacy_delay_queue_name(&destination, Duration::from_secs(1));
    let orphan_5s = legacy_delay_queue_name(&destination, Duration::from_secs(5));
    let orphan_30s = legacy_delay_queue_name(&destination, Duration::from_secs(30));

    // Scripted broker (candidates probed in sorted order: 1 s, 30 s, 5 s):
    // the 1 s and 5 s legacy orphans are empty and deletable, the 30 s
    // legacy orphan still drains in-flight messages. The live queue must be
    // recognized without touching the broker.
    transport.push_queue_size(Ok(0));
    transport.push_queue_size(Ok(3));
    transport.push_queue_size(Ok(0));
    transport.push_operation_result(Ok(()));
    transport.push_operation_result(Ok(()));

    let sweep = sweep_delay_queues(
        channel.as_ref(),
        &plan,
        std::slice::from_ref(&destination),
        std::slice::from_ref(&live.name),
    )
    .await;

    assert_eq!(
        sweep.deleted,
        vec![orphan_1s.clone(), orphan_5s.clone()],
        "every empty legacy orphan of a known destination must be deleted"
    );
    assert_eq!(sweep.absent, Vec::<String>::new());
    assert!(
        sweep
            .kept
            .iter()
            .any(|kept| kept.name == orphan_30s && kept.reason == SweepKeptReason::HasMessages),
        "an orphan still draining messages must be kept: {:?}",
        sweep.kept
    );
    assert!(
        sweep
            .kept
            .iter()
            .any(|kept| kept.name == live.name && kept.reason == SweepKeptReason::InCurrentPlan),
        "a queue the current plan still produces must be kept: {:?}",
        sweep.kept
    );

    let operations = transport.operations();
    assert!(
        operations.contains(&TransportOperation::DeleteQueue { queue: orphan_1s })
            && operations.contains(&TransportOperation::DeleteQueue { queue: orphan_5s }),
        "the empty orphans must be deleted: {operations:?}",
    );
    assert!(
        !operations
            .iter()
            .any(|operation| matches!(operation, TransportOperation::DeleteQueue { queue } if *queue == live.name)),
        "the live queue must never be deleted"
    );
    assert!(
        !operations
            .iter()
            .any(|operation| matches!(operation, TransportOperation::DeleteQueue { queue } if *queue == orphan_30s)),
        "the draining orphan must never be deleted"
    );
}

#[tokio::test]
async fn delay_queue_sweep_never_touches_unknown_or_foreign_queues() {
    let transport = MockTransport::default();
    let connection = transport
        .connect(&broker_default())
        .await
        .expect("connection");
    let channel = connection
        .open_publisher()
        .await
        .expect("publisher channel");

    let plan = ttl_plan_with_margin(Duration::from_mins(1));
    let destination = Destination::new("jobs", "high");

    // The three derived legacy orphans of the known destination are already
    // gone (probes fail); the foreign and unknown-destination candidates
    // must be kept without ever reaching the broker.
    transport.push_queue_size(Err(TransportError::protocol("NOT_FOUND")));
    transport.push_queue_size(Err(TransportError::protocol("NOT_FOUND")));
    transport.push_queue_size(Err(TransportError::protocol("NOT_FOUND")));

    let sweep = sweep_delay_queues(
        channel.as_ref(),
        &plan,
        std::slice::from_ref(&destination),
        &[
            "jobs".to_owned(),
            "rabbit-rs.delay.0123456789abcdef.9999".to_owned(),
        ],
    )
    .await;

    assert!(sweep.deleted.is_empty());
    assert_eq!(sweep.absent.len(), 3);
    assert!(
        sweep
            .kept
            .iter()
            .any(|kept| kept.name == "jobs" && kept.reason == SweepKeptReason::NotSynthesized)
    );
    assert!(
        sweep
            .kept
            .iter()
            .any(|kept| kept.name == "rabbit-rs.delay.0123456789abcdef.9999"
                && kept.reason == SweepKeptReason::UnknownDestination),
        "queues of unknown destinations must never be swept: {:?}",
        sweep.kept
    );
    assert!(
        !transport
            .operations()
            .iter()
            .any(|operation| matches!(operation, TransportOperation::DeleteQueue { .. })),
        "nothing may be deleted when every candidate is kept or absent"
    );
}

#[tokio::test]
async fn delay_queue_sweep_reports_absent_queues_without_deleting() {
    let transport = MockTransport::default();
    let connection = transport
        .connect(&broker_default())
        .await
        .expect("connection");
    let channel = connection
        .open_publisher()
        .await
        .expect("publisher channel");

    let plan = rabbit_rs_core::topology::delay::TtlBucketPlan::compile(&DelayConfig {
        mode: rabbit_rs_core::config::DelayMode::Ttl,
        buckets: vec![Duration::from_secs(1)],
        max_buckets: 8,
        queue_expiry_margin: Duration::from_mins(1),
    })
    .expect("valid TTL plan");
    let destination = Destination::new("jobs", "high");
    let orphan = legacy_delay_queue_name(&destination, Duration::from_secs(1));

    transport.push_queue_size(Err(TransportError::protocol(
        "NOT_FOUND - no queue 'rabbit-rs.delay.abc' in vhost '/'",
    )));

    let sweep = sweep_delay_queues(channel.as_ref(), &plan, &[destination], &[]).await;

    assert!(sweep.deleted.is_empty());
    assert_eq!(sweep.absent, vec![orphan]);
}

#[tokio::test]
async fn delay_queue_sweep_keeps_queues_when_the_delete_is_refused() {
    let transport = MockTransport::default();
    let connection = transport
        .connect(&broker_default())
        .await
        .expect("connection");
    let channel = connection
        .open_publisher()
        .await
        .expect("publisher channel");

    let plan = rabbit_rs_core::topology::delay::TtlBucketPlan::compile(&DelayConfig {
        mode: rabbit_rs_core::config::DelayMode::Ttl,
        buckets: vec![Duration::from_secs(1)],
        max_buckets: 8,
        queue_expiry_margin: Duration::from_mins(1),
    })
    .expect("valid TTL plan");
    let destination = Destination::new("jobs", "high");
    let orphan = legacy_delay_queue_name(&destination, Duration::from_secs(1));

    transport.push_queue_size(Ok(0));
    transport.push_operation_result(Err(TransportError::connection("broker refused the delete")));

    let sweep = sweep_delay_queues(channel.as_ref(), &plan, &[destination], &[]).await;

    assert!(sweep.deleted.is_empty());
    assert!(
        sweep
            .kept
            .iter()
            .any(|kept| kept.name == orphan && kept.reason == SweepKeptReason::DeleteRefused),
        "a refused delete must be reported as kept: {:?}",
        sweep.kept
    );
}

// ---------------------------------------------------------------------------
// Lab-gated: the GC against a real broker (run with --features integration).
// ---------------------------------------------------------------------------

#[cfg(feature = "integration")]
mod delay_gc_lab {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use bytes::Bytes;

    use rabbit_rs_core::{
        config::{BrokerConfig, Credentials, DelayConfig, Endpoint, TlsConfig},
        publisher::Destination,
        topology::delay::{
            SweepKeptReason, TtlBucketPlan, legacy_delay_queue_name, sweep_delay_queues,
        },
        transport::{
            Headers, PublishProperties, PublishRequest, PublisherChannel, QueueKind, QueueSpec,
            Transport, lapin::LapinTransport,
        },
    };

    fn lab_broker() -> BrokerConfig {
        BrokerConfig {
            name: "lab".to_owned(),
            hosts: vec![Endpoint::new("localhost", 5672)],
            vhost: "/orders-eu".to_owned(),
            // The lab `rabbit_rs` user is deliberately restricted to
            // `^(amq\.|rabbit-rs-it-)` for configure operations; declaring
            // and deleting the reserved `rabbit-rs.delay.*` namespace needs
            // the lab admin, mirroring the operator's admin channel in
            // production.
            credentials: Credentials::new("admin", "admin_lab"),
            tls: TlsConfig::disabled(),
            heartbeat: Duration::from_secs(30),
        }
    }

    /// Unique per run so parallel lab runs and reruns never collide.
    fn unique_suffix() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock must advance")
            .as_nanos();
        format!("{nanos:x}")
    }

    /// The queue shape the pre-identity scheme synthesized.
    fn legacy_spec(name: String, destination: &Destination, bucket: Duration) -> QueueSpec {
        QueueSpec {
            name,
            durable: true,
            exclusive: false,
            auto_delete: false,
            kind: QueueKind::Quorum,
            dead_letter_exchange: Some(destination.exchange.to_string()),
            dead_letter_routing_key: Some(destination.routing_key.to_string()),
            message_ttl: Some(bucket),
            expires: Some(bucket + Duration::from_mins(1)),
            delivery_limit: None,
            arguments: Headers::new(),
        }
    }

    /// Declares the lab fixtures: an empty legacy orphan, a legacy orphan
    /// still draining one message, the live queue of the current plan and a
    /// foreign queue.
    async fn declare_lab_fixtures(
        channel: &dyn PublisherChannel,
        plan: &TtlBucketPlan,
        destination: &Destination,
    ) -> (String, String, QueueSpec, String) {
        let suffix = unique_suffix();
        let empty_orphan = legacy_delay_queue_name(destination, Duration::from_secs(1));
        let draining_orphan = legacy_delay_queue_name(destination, Duration::from_secs(30));
        let live = plan
            .queue_for(destination, Duration::from_secs(1))
            .expect("live queue");
        let foreign = format!("rabbit-rs-it-79-{suffix}-keep-me");

        channel
            .declare_queue(&legacy_spec(
                empty_orphan.clone(),
                destination,
                Duration::from_secs(1),
            ))
            .await
            .expect("declare empty orphan");
        channel
            .declare_queue(&legacy_spec(
                draining_orphan.clone(),
                destination,
                Duration::from_secs(30),
            ))
            .await
            .expect("declare draining orphan");
        channel
            .declare_queue(&live)
            .await
            .expect("declare live queue");
        channel
            .declare_queue(&legacy_spec(
                foreign.clone(),
                destination,
                Duration::from_secs(1),
            ))
            .await
            .expect("declare foreign queue");
        channel
            .publish(PublishRequest {
                exchange: "".into(),
                routing_key: draining_orphan.clone().into(),
                payload: Bytes::from_static(b"in-flight"),
                mandatory: false,
                properties: PublishProperties::default(),
            })
            .await
            .expect("publish into the draining orphan");
        (empty_orphan, draining_orphan, live, foreign)
    }

    /// Best-effort destructive cleanup: purge then delete, retrying until
    /// the broker accepts everything.
    async fn cleanup_lab_queues(
        connection: &dyn rabbit_rs_core::transport::TransportConnection,
        queues: Vec<String>,
    ) {
        let cleanup_channel = connection.open_publisher().await.expect("cleanup channel");
        let mut survivors = queues;
        for _ in 0..3 {
            let mut pending = Vec::new();
            for queue in survivors {
                let _ = cleanup_channel.purge_queue(&queue).await;
                if cleanup_channel.delete_queue(&queue).await.is_err() {
                    pending.push(queue);
                }
            }
            if pending.is_empty() {
                break;
            }
            survivors = pending;
        }
        let _ = cleanup_channel.close().await;
    }

    #[tokio::test]
    async fn sweep_deletes_legacy_delay_orphans_on_the_real_broker() {
        let transport = LapinTransport;
        let connection = transport
            .connect(&lab_broker())
            .await
            .expect("lab connection");
        let channel = connection
            .open_publisher()
            .await
            .expect("publisher channel");

        let suffix = unique_suffix();
        let destination = Destination::new(format!("rabbit-rs-it-79-{suffix}"), "high");
        let plan = TtlBucketPlan::compile(&DelayConfig {
            mode: rabbit_rs_core::config::DelayMode::Ttl,
            buckets: vec![Duration::from_secs(1), Duration::from_secs(30)],
            max_buckets: 8,
            queue_expiry_margin: Duration::from_mins(1),
        })
        .expect("valid TTL plan");

        let (empty_orphan, draining_orphan, live, foreign) =
            declare_lab_fixtures(channel.as_ref(), &plan, &destination).await;

        let sweep = sweep_delay_queues(
            channel.as_ref(),
            &plan,
            std::slice::from_ref(&destination),
            &[foreign.clone(), live.name.clone()],
        )
        .await;

        assert_eq!(
            sweep.deleted,
            vec![empty_orphan.clone()],
            "the empty legacy orphan must be deleted: {sweep:?}",
        );
        assert_eq!(sweep.absent, Vec::<String>::new());
        assert!(
            sweep
                .kept
                .iter()
                .any(|kept| kept.name == draining_orphan
                    && kept.reason == SweepKeptReason::HasMessages),
            "the draining orphan must be kept: {:?}",
            sweep.kept
        );
        assert!(
            sweep
                .kept
                .iter()
                .any(|kept| kept.name == live.name && kept.reason == SweepKeptReason::InCurrentPlan),
            "the live queue must be kept without being probed: {:?}",
            sweep.kept
        );
        assert!(
            sweep
                .kept
                .iter()
                .any(|kept| kept.name == foreign && kept.reason == SweepKeptReason::NotSynthesized),
            "the foreign queue must be kept: {:?}",
            sweep.kept
        );

        // Broker state: the survivors first — a passive declare of a
        // missing queue is a hard NOT_FOUND that closes the channel, so the
        // absence check runs last.
        for survivor in [&draining_orphan, &live.name, &foreign] {
            channel
                .verify_queue(&legacy_spec(survivor.clone(), &destination, Duration::ZERO))
                .await
                .unwrap_or_else(|error| panic!("queue '{survivor}' must survive: {error}"));
        }
        assert!(
            channel
                .verify_queue(&legacy_spec(
                    empty_orphan.clone(),
                    &destination,
                    Duration::ZERO
                ))
                .await
                .is_err(),
            "the empty orphan must be gone from the broker"
        );

        cleanup_lab_queues(
            connection.as_ref(),
            vec![draining_orphan, live.name.clone(), foreign],
        )
        .await;
        connection.close().await.expect("close lab connection");
    }
}

#[test]
fn legacy_delay_queue_names_match_the_pre_identity_scheme() {
    // The pre-#79 name was hash16(exchange, 0, routing_key) + "." + bucket
    // ms. Pinning the exact digest freezes the GC's migration contract:
    // legacy names must keep matching the queues the previous version
    // synthesized, or the sweep would miss them.
    let destination = Destination::new("jobs", "high");
    assert_eq!(
        legacy_delay_queue_name(&destination, Duration::from_secs(1)),
        "rabbit-rs.delay.52b5affb8584cc92.1000"
    );
}
