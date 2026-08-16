use std::time::Duration;

use rabbit_rs_core::{
    config::{
        BrokerConfig, Config, Credentials, DeadLetterConfig, DelayConfig, Endpoint,
        SchedulerConfig, SubscriptionConfig, TlsConfig, TopologyMode, ValidatedConfig,
        WorkerProfile,
    },
    topology::{
        DeadLetterDefinition, QueueDefinition, TopologyDefinition, TopologyPlan, TopologyReconciler,
    },
    transport::{
        HeaderValue, Headers, QueueKind, QueueSpec, Transport,
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

fn subscription(queue: &str) -> SubscriptionConfig {
    SubscriptionConfig {
        name: "jobs".to_owned(),
        broker: "primary".to_owned(),
        queue: queue.to_owned(),
        weight: 1,
        priority_class: 0,
        prefetch: 8,
        starvation_after: Duration::from_secs(30),
    }
}

fn base_config(queue: &str) -> Config {
    Config {
        brokers: vec![broker()],
        workers: vec![WorkerProfile {
            name: "main".to_owned(),
            subscriptions: vec![subscription(queue)],
            scheduler: SchedulerConfig::weighted_fair(16),
        }],
        topology_mode: TopologyMode::Declare,
        delay: DelayConfig::default(),
        dead_letter: None,
        delivery_limit: None,
    }
}

fn config_with_dead_letter(queue: &str) -> Config {
    let mut config = base_config(queue);
    config.dead_letter = Some(DeadLetterConfig {
        enabled: true,
        exchange: "jobs.dlx".to_owned(),
        queue: "jobs.failed".to_owned(),
        routing_key: Some("failed".to_owned()),
    });
    config
}

fn config_with_delivery_limit(queue: &str, limit: u32) -> Config {
    let mut config = base_config(queue);
    config.delivery_limit = Some(limit);
    config
}

fn build_plan_from_config(config: &ValidatedConfig) -> TopologyPlan {
    let queues: Vec<_> = config
        .worker_profiles()
        .iter()
        .flat_map(|worker| &worker.subscriptions)
        .map(|sub| {
            let mut qd = QueueDefinition::new(&sub.queue);
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
    let mut arguments = Headers::new();
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
        scheduler: SchedulerConfig::weighted_fair(16),
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
