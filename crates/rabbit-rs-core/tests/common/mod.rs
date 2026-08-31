//! Shared scaffolding for the consolidated integration tests.
//!
//! Every test binary pulls this in with `mod common;` and uses the helpers
//! instead of re-declaring local `broker`/`config`/wait-loop copies.
#![allow(dead_code)]

use std::{sync::Arc, time::Duration};

use rabbit_rs_core::{
    config::{
        BrokerConfig, Config, ConsumerConfigSection, Credentials, DelayConfig, Endpoint,
        PublisherConfigSection, SchedulerConfig, SubscriptionConfig, TlsConfig, TopologyMode,
        ValidatedConfig, WorkerProfile,
    },
    transport::{
        PublishRequest as TransportRequest, QueueKind,
        mock::{MockTransport, TransportOperation},
    },
};

/// A localhost broker with guest credentials, matching the shapes shared by
/// every test file.
pub fn broker(name: &str, vhost: &str, password: &str) -> BrokerConfig {
    BrokerConfig {
        name: name.to_owned(),
        hosts: vec![Endpoint::new("localhost", 5672)],
        vhost: vhost.to_owned(),
        credentials: Credentials::new("guest", password),
        tls: TlsConfig::disabled(),
        heartbeat: Duration::from_secs(30),
    }
}

/// One worker consuming a single queue with unit weight and default limits.
pub fn worker_profile(name: &str, broker_name: &str, queue: &str, prefetch: u16) -> WorkerProfile {
    WorkerProfile {
        name: name.to_owned(),
        subscriptions: vec![SubscriptionConfig {
            name: queue.to_owned(),
            broker: broker_name.to_owned(),
            queue: queue.to_owned(),
            weight: 1,
            priority_class: 0,
            prefetch,
            starvation_after: Duration::from_secs(30),
            max_buffered_bytes: 64 * 1024 * 1024,
            early_ack: false,
            no_ack: false,
        }],
        scheduler: SchedulerConfig::weighted_fair(),
    }
}

/// A validated configuration with the default declare/long-lived-quorum
/// topology and no dead-letter, delay, or delivery-limit overrides.
pub fn config(brokers: Vec<BrokerConfig>, workers: Vec<WorkerProfile>) -> Arc<ValidatedConfig> {
    Arc::new(
        Config {
            brokers,
            workers,
            topology_mode: TopologyMode::Declare,
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

/// Publish operations recorded by the mock transport, in order.
pub fn publish_requests(transport: &MockTransport) -> Vec<TransportRequest> {
    transport
        .operations()
        .into_iter()
        .filter_map(|operation| match operation {
            TransportOperation::Publish(request) => Some(request),
            _ => None,
        })
        .collect()
}

/// The first publish recorded by the mock transport.
pub fn find_publish(transport: &MockTransport) -> TransportRequest {
    transport
        .operations()
        .iter()
        .find_map(|operation| match operation {
            TransportOperation::Publish(request) => Some(request.clone()),
            _ => None,
        })
        .expect("at least one publish")
}

/// Yields until the transport recorded exactly `expected` publishes.
pub async fn wait_for_publish_count(transport: &MockTransport, expected: usize) {
    for _ in 0..100 {
        if publish_requests(transport).len() == expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("publisher did not emit {expected} messages");
}

/// Like [`wait_for_publish_count`] but advances paused time, for pipelines
/// that only emit on timer ticks.
pub async fn wait_for_publish_count_delay(transport: &MockTransport, expected: usize) {
    for _ in 0..200 {
        if publish_requests(transport).len() == expected {
            return;
        }
        tokio::time::advance(Duration::from_millis(2)).await;
        tokio::task::yield_now().await;
    }
    panic!("publisher did not emit {expected} messages");
}
