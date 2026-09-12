//! Topology and delay-routing benchmarks.
//!
//! The topology plan is recompiled on every connection recovery, and the
//! delay router runs once per delayed publication, so both sit on hot paths
//! that no broker round-trip hides.

mod support;

use std::time::Duration;

use rabbit_rs_core::{
    publisher::{Destination, delay::DelayRouter},
    topology::{
        TopologyPlan,
        delay::{DelayStrategy, TtlBucketPlan, delayed_exchange_name},
    },
};
use support::{CONFIG_SHAPES, ConfigShape, validated_config};

fn main() {
    divan::main();
}

/// Compiles the exchanges, queues and bindings a pool declares on connect and
/// re-declares on every recovery generation.
#[divan::bench(args = CONFIG_SHAPES)]
fn compile_plan(bencher: divan::Bencher, shape: &str) {
    let config = validated_config(ConfigShape::from_label(shape));

    bencher.bench(|| divan::black_box(TopologyPlan::from_config(divan::black_box(&config))));
}

/// Resolves the delayed-delivery backend from configuration, including TTL
/// bucket sorting and deduplication.
#[divan::bench]
fn compile_delay_strategy(bencher: divan::Bencher) {
    let config = validated_config(ConfigShape::LARGE);

    bencher.bench(|| divan::black_box(DelayStrategy::compile(divan::black_box(&config))));
}

/// Names the delayed exchange a destination publishes through in plugin mode.
#[divan::bench(args = ["", "orders.exchange"])]
fn delayed_exchange(bencher: divan::Bencher, exchange: &str) {
    bencher.bench(|| divan::black_box(delayed_exchange_name(divan::black_box(exchange))));
}

/// Builds the durable TTL delay queue of a destination: bucket selection plus
/// the SHA-256 argument fingerprint that makes the queue name stable.
#[divan::bench(args = [50_u64, 4_000, 90_000])]
fn ttl_queue_for(bencher: divan::Bencher, delay_ms: u64) {
    let plan = ttl_plan();
    let destination = Destination::new("orders.exchange", "orders.created");
    let delay = Duration::from_millis(delay_ms);

    bencher.bench(|| {
        divan::black_box(
            plan.queue_for(divan::black_box(&destination), divan::black_box(delay))
                .expect("routable delay"),
        )
    });
}

/// Lists the live delay queues of a destination, the reference set the
/// orphan sweeper compares broker state against.
#[divan::bench]
fn ttl_expected_queue_names(bencher: divan::Bencher) {
    let plan = ttl_plan();
    let destination = Destination::new("orders.exchange", "orders.created");

    bencher.bench(|| divan::black_box(plan.expected_queue_names(divan::black_box(&destination))));
}

/// Routes a delayed publication through the plugin backend.
#[divan::bench]
fn route_delay_plugin(bencher: divan::Bencher) {
    let strategy = DelayStrategy::Plugin;
    let destination = Destination::new("orders.exchange", "orders.created");

    bencher.bench(|| {
        divan::black_box(
            DelayRouter::route(
                divan::black_box(&strategy),
                divan::black_box(&destination),
                divan::black_box(5_000),
            )
            .expect("routable delay"),
        )
    });
}

/// Routes a delayed publication through the TTL bucket backend.
#[divan::bench]
fn route_delay_ttl_buckets(bencher: divan::Bencher) {
    let strategy = DelayStrategy::TtlBuckets(ttl_plan());
    let destination = Destination::new("orders.exchange", "orders.created");

    bencher.bench(|| {
        divan::black_box(
            DelayRouter::route(
                divan::black_box(&strategy),
                divan::black_box(&destination),
                divan::black_box(5_000),
            )
            .expect("routable delay"),
        )
    });
}

fn ttl_plan() -> TtlBucketPlan {
    let config = validated_config(ConfigShape::LARGE);
    TtlBucketPlan::compile(config.delay()).expect("valid TTL bucket plan")
}
