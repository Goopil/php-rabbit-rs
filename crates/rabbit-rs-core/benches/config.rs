//! Configuration benchmarks.
//!
//! Every PHP process that opens a connection deserializes, validates and
//! fingerprints its configuration document before any socket is touched, so
//! this path gates the cold start of the extension.

mod support;

use rabbit_rs_core::{
    config::{Config, Endpoint},
    transport::lapin::connection_uri,
};
use support::{CONFIG_SHAPES, ConfigShape, config_json, parse_config, validated_config};

fn main() {
    divan::main();
}

/// Deserializes the JSON document handed over from PHP.
#[divan::bench(args = CONFIG_SHAPES)]
fn deserialize(bencher: divan::Bencher, shape: &str) {
    let json = config_json(ConfigShape::from_label(shape));

    bencher.bench(|| divan::black_box(parse_config(divan::black_box(&json))));
}

/// Validates and canonicalizes a parsed configuration: bound checks, host and
/// profile sorting, then the SHA-256 fingerprint used as the pool identity.
#[divan::bench(args = CONFIG_SHAPES)]
fn validate(bencher: divan::Bencher, shape: &str) {
    let config = parse_config(&config_json(ConfigShape::from_label(shape)));

    bencher
        .with_inputs(|| config.clone())
        .bench_values(|config: Config| divan::black_box(config.validate().expect("valid")));
}

/// The complete cold-start path: raw JSON in, canonical configuration out.
#[divan::bench(args = CONFIG_SHAPES)]
fn deserialize_and_validate(bencher: divan::Bencher, shape: &str) {
    let json = config_json(ConfigShape::from_label(shape));

    bencher.bench(|| {
        divan::black_box(
            parse_config(divan::black_box(&json))
                .validate()
                .expect("valid"),
        )
    });
}

/// Synthesizes the implicit worker profile of the Laravel `auto_subscribe`
/// path, which runs once per unknown queue name.
#[divan::bench]
fn synthesize_auto_profile(bencher: divan::Bencher) {
    let config = validated_config(ConfigShape::SMALL);

    bencher.bench(|| {
        divan::black_box(
            config
                .synthesize_auto_profile("__auto__.orders")
                .expect("synthesizable profile"),
        )
    });
}

/// Builds the AMQP URI of an endpoint, including credentials and tuning
/// parameters. Runs for every connection attempt and every recovery round.
#[divan::bench]
fn build_connection_uri(bencher: divan::Bencher) {
    let config = validated_config(ConfigShape::SMALL);
    let broker = config
        .broker("broker-0")
        .expect("configured broker")
        .clone();
    let endpoint = Endpoint::new("rabbit-0.internal", 5672);

    bencher.bench(|| {
        divan::black_box(
            connection_uri(divan::black_box(&broker), divan::black_box(&endpoint))
                .expect("valid broker URI"),
        )
    });
}
