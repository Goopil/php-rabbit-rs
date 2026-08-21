use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use bytes::Bytes;
use rabbit_rs_core::{
    config::{BrokerConfig, Config, Credentials, DelayConfig, DelayMode, Endpoint, TlsConfig},
    publisher::{
        Destination, MessageProperties, PublishOutcome, PublishRequest, PublisherActor,
        PublisherConfig,
    },
    topology::delay::{DelayPluginProbe, DelayStrategy, DelayStrategyResolver, TtlBucketPlan},
    transport::{
        ExchangeKind, ExchangeSpec, PublishConfirmation, PublishRequest as TransportRequest,
        QueueSpec, Transport, TransportResult,
        mock::{MockTransport, TransportOperation},
    },
};
use tokio::time::Instant;

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

fn publisher_config() -> PublisherConfig {
    PublisherConfig::new(32, Duration::from_secs(30))
}

fn delayed_request(message_id: &str, delay_ms: u64) -> PublishRequest {
    let mut properties = MessageProperties::new(message_id);
    properties.delay_ms = Some(delay_ms);
    PublishRequest::new(
        Destination::new("jobs", "high"),
        Bytes::from_static(b"job"),
        properties,
        Instant::now() + Duration::from_secs(30),
    )
}

fn immediate_request(message_id: &str) -> PublishRequest {
    PublishRequest::new(
        Destination::new("jobs", "high"),
        Bytes::from_static(b"job"),
        MessageProperties::new(message_id),
        Instant::now() + Duration::from_secs(30),
    )
}

async fn spawn_actor(
    transport: &MockTransport,
    config: PublisherConfig,
    delay_strategy: DelayStrategy,
) -> rabbit_rs_core::publisher::PublisherHandle {
    let publisher = transport
        .connect(&broker())
        .await
        .expect("connection")
        .open_publisher()
        .await
        .expect("publisher");
    PublisherActor::spawn_with_delay_strategy(Arc::from(publisher), config, delay_strategy)
}

async fn wait_for_publish_count(transport: &MockTransport, expected: usize) {
    for _ in 0..200 {
        let count = transport
            .operations()
            .iter()
            .filter(|operation| matches!(operation, TransportOperation::Publish(_)))
            .count();
        if count == expected {
            return;
        }
        tokio::time::advance(Duration::from_millis(2)).await;
        tokio::task::yield_now().await;
    }
    panic!("publisher did not emit {expected} messages");
}

fn find_publish(transport: &MockTransport) -> TransportRequest {
    transport
        .operations()
        .iter()
        .find_map(|operation| match operation {
            TransportOperation::Publish(request) => Some(request.clone()),
            _ => None,
        })
        .expect("at least one publish")
}

fn ttl_config() -> DelayConfig {
    DelayConfig::new(
        DelayMode::Ttl,
        vec![
            Duration::from_secs(1),
            Duration::from_secs(5),
            Duration::from_secs(30),
        ],
        8,
        Duration::from_mins(1),
        Duration::from_millis(50),
    )
}

#[tokio::test(start_paused = true)]
async fn plugin_mode_publishes_on_delayed_exchange_with_x_delay_header() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let strategy = DelayStrategy::Plugin;
    let actor = spawn_actor(&transport, publisher_config(), strategy).await;

    let waiter = actor
        .try_publish(delayed_request("delayed-job", 5_000))
        .expect("publish");

    wait_for_publish_count(&transport, 1).await;

    let request = find_publish(&transport);

    assert_ne!(
        request.exchange, "jobs",
        "plugin mode must publish on a delayed exchange, not the original"
    );
    assert!(
        request.exchange.ends_with(".delayed"),
        "exchange name should be the delayed variant, got: {}",
        request.exchange
    );
    assert_eq!(request.routing_key, "high");
    assert_eq!(
        request.properties.delay_ms,
        Some(5_000),
        "x-delay header must be set"
    );

    assert!(matches!(
        waiter.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn ttl_mode_publishes_on_ttl_queue_with_dead_letter_to_original() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_operation_result(Ok(()));
    let plan = TtlBucketPlan::compile(&ttl_config()).expect("TTL plan");
    let strategy = DelayStrategy::TtlBuckets(plan);
    let actor = spawn_actor(&transport, publisher_config(), strategy).await;

    let waiter = actor
        .try_publish(delayed_request("ttl-job", 5_000))
        .expect("publish");

    wait_for_publish_count(&transport, 1).await;

    let operations = transport.operations();

    let queue_declared = operations.iter().any(|op| {
        matches!(
            op,
            TransportOperation::DeclareQueue(QueueSpec {
                dead_letter_exchange: Some(dlx),
                message_ttl: Some(ttl),
                ..
            }) if dlx == "jobs" && *ttl == Duration::from_secs(5)
        )
    });
    assert!(
        queue_declared,
        "TTL mode must lazily declare a queue with x-message-ttl and dead-letter-exchange"
    );

    let request = find_publish(&transport);

    assert_eq!(
        request.exchange, "",
        "TTL mode must publish on the default exchange"
    );
    assert!(
        request.routing_key.starts_with("rabbit-rs.delay."),
        "TTL mode must publish to a stable delay queue, got: {}",
        request.routing_key
    );
    assert_eq!(
        request.properties.delay_ms, None,
        "TTL mode must not set x-delay header"
    );

    assert!(matches!(
        waiter.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn zero_delay_does_not_change_routing() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let strategy = DelayStrategy::Plugin;
    let actor = spawn_actor(&transport, publisher_config(), strategy).await;

    let waiter = actor
        .try_publish(immediate_request("immediate-job"))
        .expect("publish");

    wait_for_publish_count(&transport, 1).await;

    let request = find_publish(&transport);

    assert_eq!(
        request.exchange, "jobs",
        "zero delay must publish on the original exchange"
    );
    assert_eq!(request.routing_key, "high");
    assert_eq!(
        request.properties.delay_ms, None,
        "zero delay must not set x-delay header"
    );

    assert!(matches!(
        waiter.wait().await,
        Ok(PublishOutcome::Confirmed { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn plugin_mode_topology_includes_delayed_exchange_declaration() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    let strategy = DelayStrategy::Plugin;
    let actor = spawn_actor(&transport, publisher_config(), strategy).await;

    let _waiter = actor
        .try_publish(delayed_request("delayed-job", 5_000))
        .expect("publish");

    wait_for_publish_count(&transport, 1).await;

    let operations = transport.operations();
    let has_delayed_exchange = operations.iter().any(|op| {
        matches!(
            op,
            TransportOperation::DeclareExchange(ExchangeSpec {
                kind: ExchangeKind::Delayed(_),
                ..
            })
        )
    });
    assert!(
        has_delayed_exchange,
        "plugin mode must declare a delayed exchange"
    );
}

#[tokio::test(start_paused = true)]
async fn ttl_mode_declares_delay_queue_lazily_on_first_delayed_publish() {
    let transport = MockTransport::default();
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    transport.push_operation_result(Ok(()));
    transport.push_operation_result(Ok(()));
    let plan = TtlBucketPlan::compile(&ttl_config()).expect("TTL plan");
    let strategy = DelayStrategy::TtlBuckets(plan);
    let actor = spawn_actor(&transport, publisher_config(), strategy).await;

    let _first = actor
        .try_publish(delayed_request("first", 5_000))
        .expect("first publish");
    wait_for_publish_count(&transport, 1).await;

    let _second = actor
        .try_publish(delayed_request("second", 5_000))
        .expect("second publish");
    wait_for_publish_count(&transport, 2).await;

    let declare_count = transport
        .operations()
        .iter()
        .filter(|op| matches!(op, TransportOperation::DeclareQueue(_)))
        .count();
    assert_eq!(
        declare_count, 1,
        "TTL queue must be declared lazily and only once per bucket"
    );
}

#[test]
fn delay_config_is_validated_and_deserialized_from_config() {
    let json = serde_json::json!({
        "brokers": [{
            "name": "default",
            "hosts": [{"host": "rabbit.local", "port": 5672}],
            "vhost": "/",
            "credentials": {"username": "guest", "password": "secret"},
            "tls": {"enabled": false, "server_name": null},
            "heartbeat": 30
        }],
        "workers": [{
            "name": "main",
            "subscriptions": [{
                "name": "default",
                "broker": "default",
                "queue": "jobs",
                "weight": 1,
                "priority_class": 0,
                "prefetch": 16
            }],
            "scheduler": {
                "strategy": "weighted_fair",
                "max_in_flight": 64
            }
        }],
        "topology_mode": "declare",
        "delay": {
            "mode": "ttl",
            "buckets": [1, 5, 30],
            "max_buckets": 8,
            "queue_expiry_margin": 60,
            "detection_timeout": 5
        }
    });

    let config: Config = serde_json::from_value(json).expect("valid config with delay section");
    let validated = config.validate().expect("valid configuration");

    let delay = validated.delay();
    assert_eq!(delay.mode, DelayMode::Ttl);
    assert_eq!(
        delay.buckets,
        vec![
            Duration::from_secs(1),
            Duration::from_secs(5),
            Duration::from_secs(30)
        ]
    );
    assert_eq!(delay.max_buckets, 8);
    assert_eq!(delay.queue_expiry_margin, Duration::from_mins(1));
    assert_eq!(delay.detection_timeout, Duration::from_secs(5));
}

#[test]
fn delay_config_rejects_empty_buckets() {
    let json = serde_json::json!({
        "brokers": [{
            "name": "default",
            "hosts": [{"host": "rabbit.local", "port": 5672}],
            "vhost": "/",
            "credentials": {"username": "guest", "password": "secret"},
            "tls": {"enabled": false, "server_name": null},
            "heartbeat": 30
        }],
        "workers": [{
            "name": "main",
            "subscriptions": [{
                "name": "default",
                "broker": "default",
                "queue": "jobs",
                "weight": 1,
                "priority_class": 0,
                "prefetch": 16
            }],
            "scheduler": {
                "strategy": "weighted_fair",
                "max_in_flight": 64
            }
        }],
        "topology_mode": "declare",
        "delay": {
            "mode": "ttl",
            "buckets": [],
            "max_buckets": 8,
            "queue_expiry_margin": 60,
            "detection_timeout": 5
        }
    });

    let config: Config = serde_json::from_value(json).expect("parse");
    let error = config
        .validate()
        .expect_err("empty buckets must be rejected");

    assert!(error.path().contains("delay.buckets"));
}

#[tokio::test(start_paused = true)]
async fn auto_mode_detects_plugin_and_falls_back_to_ttl_if_absent() {
    struct NeverAvailable;

    #[async_trait]
    impl DelayPluginProbe for NeverAvailable {
        async fn is_available(&self) -> TransportResult<bool> {
            Ok(false)
        }
    }

    let mut resolver = DelayStrategyResolver::new();
    let config = DelayConfig::new(
        DelayMode::Auto,
        vec![Duration::from_secs(5)],
        8,
        Duration::from_mins(1),
        Duration::from_millis(50),
    );

    let strategy = resolver
        .resolve(&config, 1, Arc::new(NeverAvailable))
        .await
        .expect("fallback strategy");

    assert!(
        matches!(strategy, DelayStrategy::TtlBuckets(_)),
        "Auto mode must fall back to TTL when plugin is absent"
    );
}
