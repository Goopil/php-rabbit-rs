use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use rabbit_rs_core::{
    config::{DelayConfig, DelayMode},
    publisher::{Destination, delay::DelayRouter},
    topology::delay::{DelayPluginProbe, DelayStrategy, DelayStrategyResolver, TtlBucketPlan},
    transport::TransportResult,
};

struct FixedProbe {
    available: bool,
    calls: AtomicUsize,
}

impl FixedProbe {
    const fn new(available: bool) -> Self {
        Self {
            available,
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl DelayPluginProbe for FixedProbe {
    async fn is_available(&self) -> TransportResult<bool> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.available)
    }
}

struct PendingProbe;

#[async_trait]
impl DelayPluginProbe for PendingProbe {
    async fn is_available(&self) -> TransportResult<bool> {
        std::future::pending().await
    }
}

fn config(mode: DelayMode) -> DelayConfig {
    DelayConfig::new(
        mode,
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

#[tokio::test]
async fn auto_selects_delayed_exchange_when_plugin_is_available() {
    let probe = Arc::new(FixedProbe::new(true));
    let mut resolver = DelayStrategyResolver::new();

    let strategy = resolver
        .resolve(&config(DelayMode::Auto), 1, probe)
        .await
        .expect("plugin strategy");

    assert_eq!(strategy, DelayStrategy::Plugin);
}

#[tokio::test]
async fn auto_falls_back_to_ttl_when_plugin_is_absent() {
    let probe = Arc::new(FixedProbe::new(false));
    let mut resolver = DelayStrategyResolver::new();

    let strategy = resolver
        .resolve(&config(DelayMode::Auto), 1, probe)
        .await
        .expect("TTL fallback");

    assert!(matches!(strategy, DelayStrategy::TtlBuckets(_)));
}

#[tokio::test]
async fn required_plugin_fails_permanently_when_absent() {
    let probe = Arc::new(FixedProbe::new(false));
    let mut resolver = DelayStrategyResolver::new();

    let error = resolver
        .resolve(&config(DelayMode::Plugin), 1, probe)
        .await
        .expect_err("plugin is required");

    assert!(error.is_permanent());
}

#[test]
fn ttl_rounds_up_to_the_next_bucket() {
    let plan = TtlBucketPlan::compile(&config(DelayMode::Ttl)).expect("TTL plan");

    assert_eq!(
        plan.bucket_for(Duration::from_millis(1_001))
            .expect("bucket"),
        Duration::from_secs(5)
    );
}

#[test]
fn ttl_rejects_more_than_the_configured_maximum_buckets() {
    let invalid = DelayConfig::new(
        DelayMode::Ttl,
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(3),
        ],
        2,
        Duration::from_mins(1),
        Duration::from_millis(50),
    );

    assert!(TtlBucketPlan::compile(&invalid).is_err());
}

#[test]
fn ttl_queue_name_is_stable_and_x_expires_exceeds_message_ttl() {
    let plan = TtlBucketPlan::compile(&config(DelayMode::Ttl)).expect("TTL plan");
    let destination = Destination::new("jobs", "high");

    let first = plan
        .queue_for(&destination, Duration::from_secs(5))
        .expect("queue");
    let second = plan
        .queue_for(&destination, Duration::from_secs(5))
        .expect("queue");

    assert_eq!(first.name, second.name);
    assert_eq!(first.message_ttl, Some(Duration::from_secs(5)));
    assert!(first.expires > first.message_ttl);
}

#[test]
fn negative_delay_is_rejected() {
    let strategy = DelayStrategy::TtlBuckets(
        TtlBucketPlan::compile(&config(DelayMode::Ttl)).expect("TTL plan"),
    );

    assert!(DelayRouter::route(&strategy, &Destination::new("jobs", "high"), -1).is_err());
}

#[tokio::test]
async fn plugin_detection_is_cached_per_connection_generation() {
    let probe = Arc::new(FixedProbe::new(true));
    let mut resolver = DelayStrategyResolver::new();

    resolver
        .resolve(&config(DelayMode::Auto), 1, probe.clone())
        .await
        .expect("first resolution");
    resolver
        .resolve(&config(DelayMode::Auto), 1, probe.clone())
        .await
        .expect("cached resolution");
    resolver
        .resolve(&config(DelayMode::Auto), 2, probe.clone())
        .await
        .expect("new generation");

    assert_eq!(probe.calls(), 2);
}

#[tokio::test(start_paused = true)]
async fn auto_detection_timeout_is_bounded_and_falls_back_to_ttl() {
    let mut resolver = DelayStrategyResolver::new();

    let strategy = resolver
        .resolve(&config(DelayMode::Auto), 1, Arc::new(PendingProbe))
        .await
        .expect("bounded TTL fallback");

    assert!(matches!(strategy, DelayStrategy::TtlBuckets(_)));
}
