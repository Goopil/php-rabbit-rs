//! Shared fixtures for the `rabbit-rs-core` benchmark suite.
//!
//! Every benchmark works on plain, broker-free data: configuration documents,
//! compiled plans and header maps. Nothing here opens a socket, so results
//! stay deterministic under CPU simulation.

// Each benchmark binary only pulls in the fixtures it needs.
#![allow(dead_code)]

use rabbit_rs_core::config::{Config, ValidatedConfig};
use serde_json::{Value, json};

/// Shape of a generated configuration document.
#[derive(Clone, Copy, Debug)]
pub struct ConfigShape {
    pub brokers: usize,
    pub workers: usize,
    pub subscriptions: usize,
    pub routes: usize,
    pub dead_letter: bool,
}

impl ConfigShape {
    /// A single-broker, single-queue deployment: the Laravel default.
    pub const SMALL: Self = Self {
        brokers: 1,
        workers: 1,
        subscriptions: 1,
        routes: 1,
        dead_letter: false,
    };

    /// A multi-tenant deployment fanning several worker profiles over a few
    /// brokers, with dead-lettering enabled.
    pub const LARGE: Self = Self {
        brokers: 4,
        workers: 8,
        subscriptions: 16,
        routes: 4,
        dead_letter: true,
    };

    /// Resolves the shape benchmarked under the given argument label.
    ///
    /// # Panics
    ///
    /// Panics for an unknown label, which can only be a benchmark typo.
    #[must_use]
    pub fn from_label(label: &str) -> Self {
        match label {
            "small" => Self::SMALL,
            "large" => Self::LARGE,
            other => panic!("unknown configuration shape '{other}'"),
        }
    }
}

/// Labels of the configuration shapes every configuration benchmark covers.
pub const CONFIG_SHAPES: [&str; 2] = ["small", "large"];

/// Builds the JSON document a PHP caller hands to the runtime.
#[must_use]
pub fn config_document(shape: ConfigShape) -> Value {
    let brokers: Vec<Value> = (0..shape.brokers)
        .map(|index| {
            json!({
                "name": format!("broker-{index}"),
                "hosts": [
                    {"host": format!("rabbit-{index}.internal"), "port": 5672},
                    {"host": format!("rabbit-{index}.backup.internal"), "port": 5672},
                ],
                "vhost": "/production",
                "credentials": {"username": "app", "password": "s3cr3t"},
                "tls": {"enabled": false},
                "heartbeat": 30,
            })
        })
        .collect();

    let workers: Vec<Value> = (0..shape.workers)
        .map(|worker| {
            let subscriptions: Vec<Value> = (0..shape.subscriptions)
                .map(|subscription| {
                    json!({
                        "name": format!("subscription-{subscription}"),
                        "broker": format!("broker-{}", subscription % shape.brokers),
                        "queue": format!("queue-{worker}-{subscription}"),
                        "weight": u16::try_from(subscription % 8 + 1).unwrap_or(1),
                        "prefetch": {
                            "mode": "adaptive",
                            "initial": 16,
                            "min": 1,
                            "max": 256,
                            "target_buffer_seconds": 5,
                        },
                    })
                })
                .collect();

            json!({
                "name": format!("worker-{worker}"),
                "subscriptions": subscriptions,
                "scheduler": {"strategy": "weighted_fair", "max_in_flight": 64},
            })
        })
        .collect();

    let routes: serde_json::Map<String, Value> = (0..shape.routes)
        .map(|index| {
            (
                format!("route-{index}"),
                json!({
                    "broker": format!("broker-{}", index % shape.brokers),
                    "exchange": format!("rabbit-rs.route-{index}"),
                    "routing_key": "{queue}",
                }),
            )
        })
        .collect();

    let mut document = json!({
        "brokers": brokers,
        "workers": workers,
        "topology_mode": "declare",
        "routes": routes,
        "delay": {
            "mode": "ttl",
            "buckets": [1, 5, 30, 120, 600],
            "max_buckets": 8,
            "queue_expiry_margin": 60,
        },
        "queue_type": "quorum",
        "queue_durable": true,
    });

    if shape.dead_letter {
        document["dead_letter"] = json!({
            "enabled": true,
            "exchange": "rabbit-rs.dlx",
            "queue": "rabbit-rs.dlq",
            "routing_key": "failed",
        });
    }

    document
}

/// Serialized form of [`config_document`], as received over the FFI boundary.
#[must_use]
pub fn config_json(shape: ConfigShape) -> String {
    config_document(shape).to_string()
}

/// Parses a configuration document without validating it.
#[must_use]
pub fn parse_config(json: &str) -> Config {
    serde_json::from_str(json).expect("benchmark configuration must deserialize")
}

/// Parses and validates a configuration document.
#[must_use]
pub fn validated_config(shape: ConfigShape) -> ValidatedConfig {
    parse_config(&config_json(shape))
        .validate()
        .expect("benchmark configuration must validate")
}
