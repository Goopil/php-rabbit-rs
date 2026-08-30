# Adaptive Prefetch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Per-subscription adaptive prefetch in the Rust core: the consumer actor learns each queue's job duration (EWMA of ack latency, which includes PHP job processing) and adjusts broker QoS to keep ~`target_buffer_seconds` of ready work buffered, with config, observability, and Laravel wiring.

**Architecture:** A pure `AdaptivePrefetch` controller (EWMA + 25 % relative hysteresis) lives in the consumer actor, fed at the two ack-settlement completion sites. A conditional 1 s `tokio::select!` arm applies changes via `set_qos` in detached tasks (never on the actor critical path). Wire format stays backward compatible: plain int = fixed; union forms `{"mode": "fixed"|"adaptive", ...}` added. Fixed-mode behavior is byte-identical.

**Tech Stack:** Rust 1.96 (edition 2024, tokio, flume, serde), ext-php-rs (PHP 8.4 extension), Laravel 12/13 package, Pest, cargo-nextest.

**Spec:** `docs/superpowers/specs/2026-08-29-adaptive-prefetch-design.md` (read it first — decisions, defaults, and limits live there).

## Global Constraints

- Rust toolchain pinned 1.96.0; edition 2024; `#![forbid(unsafe_code)]` — never weaken it.
- Every Rust command runs through `rtk` (e.g. `rtk cargo test -p rabbit-rs-core config::tests`).
- No real sleeps in tests; use `#[tokio::test(start_paused = true)]` + `tokio::task::yield_now()` helpers.
- Config errors are typed with the exact input path (`workers.<name>.subscriptions.<name>.prefetch...`).
- PHP tests use Pest, never PHPUnit. Laravel Unit/Feature tests run WITHOUT the extension.
- After every Rust edit: `rtk cargo fmt --all`.
- Clippy is `-D warnings` — all code must be clippy-clean, using `#[expect(clippy::..., reason = "...")]` for justified casts (repo style, see `metrics.rs`).
- Final gate before claiming done: `rtk ./scripts/check.sh`.
- Never commit `.air/`, IDE metadata, or build artifacts.

---

### Task 1: `PrefetchConfig` type, wire parsing, validation, fingerprint, and plumbing

This task switches the prefetch representation end to end and restores compilation. Behavior of fixed mode is unchanged (existing tests are the safety net).

**Files:**
- Modify: `crates/rabbit-rs-core/src/config.rs` (field at line 211, validation at lines 576-584, fingerprint at line 759, helpers near line 940, tests at line 970+)
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs` (field line 33, builder lines 71-74, capacity line 170, initial QoS line 181)
- Modify: `crates/rabbit-rs-core/src/pool/recovery_coordinator.rs:422`

**Interfaces:**
- Consumes: existing `SubscriptionConfig`, `Config::validate()`, `ConfigFingerprint::calculate`.
- Produces: `PrefetchConfig` enum (derives `Clone, Copy, Debug, Eq, PartialEq`) with `initial_value() -> u16` and `ceiling() -> u16`; `Subscription::prefetch(u16)` (unchanged signature → `Fixed`), `Subscription::prefetch_config(PrefetchConfig)`, `Subscription::initial_prefetch() -> u16`; `deserialize_duration_seconds_opt` helper.

- [ ] **Step 1: Write failing wire-parsing tests**

In `config.rs` `mod tests`, add after the existing JSON tests (imports: add `PrefetchConfig` to the `use super::{...}` list):

```rust
    #[test]
    fn parses_plain_integer_prefetch_as_fixed() {
        let candidate = serde_json::from_value::<Config>(json!({
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
                "scheduler": {"strategy": "weighted_fair"}
            }],
            "topology_mode": "external"
        }))
        .expect("plain integer prefetch parses");

        assert!(matches!(
            candidate.workers[0].subscriptions[0].prefetch,
            PrefetchConfig::Fixed(16)
        ));
    }

    #[test]
    fn parses_fixed_union_prefetch() {
        let candidate = serde_json::from_value::<Config>(json!({
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
                    "prefetch": {"mode": "fixed", "value": 8}
                }],
                "scheduler": {"strategy": "weighted_fair"}
            }],
            "topology_mode": "external"
        }))
        .expect("fixed union prefetch parses");

        assert!(matches!(
            candidate.workers[0].subscriptions[0].prefetch,
            PrefetchConfig::Fixed(8)
        ));
    }

    #[test]
    fn parses_adaptive_union_prefetch() {
        let candidate = serde_json::from_value::<Config>(json!({
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
                    "prefetch": {
                        "mode": "adaptive",
                        "initial": 64,
                        "min": 1,
                        "max": 256,
                        "target_buffer_seconds": 5
                    }
                }],
                "scheduler": {"strategy": "weighted_fair"}
            }],
            "topology_mode": "external"
        }))
        .expect("adaptive union prefetch parses");

        assert_eq!(
            candidate.workers[0].subscriptions[0].prefetch,
            PrefetchConfig::Adaptive {
                initial: 64,
                min: 1,
                max: 256,
                target_buffer: Duration::from_secs(5),
            }
        );
    }

    #[test]
    fn rejects_unknown_prefetch_mode() {
        let result = serde_json::from_value::<Config>(json!({
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
                    "prefetch": {"mode": "dynamic", "value": 8}
                }],
                "scheduler": {"strategy": "weighted_fair"}
            }],
            "topology_mode": "external"
        }));

        assert!(result.is_err(), "unknown prefetch mode must fail to parse");
    }

    #[test]
    fn rejects_adaptive_union_missing_field() {
        let result = serde_json::from_value::<Config>(json!({
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
                    "prefetch": {"mode": "adaptive", "initial": 16, "max": 256}
                }],
                "scheduler": {"strategy": "weighted_fair"}
            }],
            "topology_mode": "external"
        }));

        assert!(result.is_err(), "adaptive union missing `min` must fail to parse");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `rtk cargo test -p rabbit-rs-core config::tests`
Expected: COMPILE ERROR — `PrefetchConfig` does not exist.

- [ ] **Step 3: Implement `PrefetchConfig` and its `Deserialize`**

In `config.rs`, right after the `SubscriptionConfig` struct (line ~234), add:

```rust
/// Per-subscription prefetch policy: a fixed QoS value or an adaptive
/// controller driven by observed job duration.
///
/// Wire forms accepted: a plain integer (`16`), `{"mode": "fixed", "value": N}`,
/// or `{"mode": "adaptive", "initial": N, "min": N, "max": N,
/// "target_buffer_seconds": S}`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrefetchConfig {
    Fixed(u16),
    Adaptive {
        initial: u16,
        min: u16,
        max: u16,
        target_buffer: Duration,
    },
}

impl PrefetchConfig {
    /// The prefetch applied when the subscription starts.
    #[must_use]
    pub const fn initial_value(&self) -> u16 {
        match self {
            Self::Fixed(value) => *value,
            Self::Adaptive { initial, .. } => *initial,
        }
    }

    /// The highest prefetch the policy can reach; bounds spawn buffer capacity.
    #[must_use]
    pub const fn ceiling(&self) -> u16 {
        match self {
            Self::Fixed(value) => *value,
            Self::Adaptive { max, .. } => *max,
        }
    }
}

impl<'de> Deserialize<'de> for PrefetchConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de;

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Plain(u16),
            Mapped(PrefetchMap),
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PrefetchMap {
            mode: PrefetchMode,
            value: Option<u16>,
            initial: Option<u16>,
            min: Option<u16>,
            max: Option<u16>,
            #[serde(default, deserialize_with = "deserialize_duration_seconds_opt")]
            target_buffer_seconds: Option<Duration>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum PrefetchMode {
            Fixed,
            Adaptive,
        }

        match Wire::deserialize(deserializer)? {
            Wire::Plain(value) => Ok(Self::Fixed(value)),
            Wire::Mapped(map) => match map.mode {
                PrefetchMode::Fixed => {
                    let value = map.value.ok_or_else(|| de::Error::missing_field("value"))?;
                    Ok(Self::Fixed(value))
                }
                PrefetchMode::Adaptive => Ok(Self::Adaptive {
                    initial: map.initial.ok_or_else(|| de::Error::missing_field("initial"))?,
                    min: map.min.ok_or_else(|| de::Error::missing_field("min"))?,
                    max: map.max.ok_or_else(|| de::Error::missing_field("max"))?,
                    target_buffer: map
                        .target_buffer_seconds
                        .ok_or_else(|| de::Error::missing_field("target_buffer_seconds"))?,
                }),
            },
        }
    }
}
```

Change the `SubscriptionConfig` field (line 211) from `pub prefetch: u16` to `pub prefetch: PrefetchConfig`.

Next to the other duration helpers (after `deserialize_duration_seconds`, line ~945), add:

```rust
fn deserialize_duration_seconds_opt<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<u64>::deserialize(deserializer).map(|seconds| seconds.map(Duration::from_secs))
}
```

Check the imports at the top of `config.rs` include `serde::{Deserialize, Deserializer}` (they already do for the existing helpers).

- [ ] **Step 4: Restore compilation — validation, fingerprint, set.rs, recovery_coordinator.rs**

Replace the prefetch validation block (`config.rs` lines 576-584) with:

```rust
                let prefetch_base =
                    format!("workers.{}.subscriptions.{}.prefetch", worker.name, subscription.name);
                match subscription.prefetch {
                    PrefetchConfig::Fixed(0) => {
                        return Err(ConfigError::new(
                            prefetch_base,
                            "prefetch must be greater than zero",
                        ));
                    }
                    PrefetchConfig::Fixed(_) => {}
                    PrefetchConfig::Adaptive {
                        initial,
                        min,
                        max,
                        target_buffer,
                    } => {
                        if min == 0 {
                            return Err(ConfigError::new(
                                format!("{prefetch_base}.min"),
                                "min must be greater than zero",
                            ));
                        }
                        if max < min {
                            return Err(ConfigError::new(
                                format!("{prefetch_base}.max"),
                                "max must be greater than or equal to min",
                            ));
                        }
                        if initial < min || initial > max {
                            return Err(ConfigError::new(
                                format!("{prefetch_base}.initial"),
                                "initial must be within [min, max]",
                            ));
                        }
                        if target_buffer.is_zero() {
                            return Err(ConfigError::new(
                                format!("{prefetch_base}.target_buffer_seconds"),
                                "target_buffer_seconds must be greater than zero",
                            ));
                        }
                        if subscription.early_ack || subscription.no_ack {
                            return Err(ConfigError::new(
                                format!("{prefetch_base}.mode"),
                                "adaptive prefetch requires consumer acknowledgements: \
                                 early_ack and no_ack must be false",
                            ));
                        }
                    }
                }
```

In `ConfigFingerprint::calculate`, replace `digest.update(subscription.prefetch.to_be_bytes());` (line 759) with:

```rust
                match subscription.prefetch {
                    PrefetchConfig::Fixed(value) => {
                        hash_value(&mut digest, "prefetch:fixed");
                        digest.update(value.to_be_bytes());
                    }
                    PrefetchConfig::Adaptive {
                        initial,
                        min,
                        max,
                        target_buffer,
                    } => {
                        hash_value(&mut digest, "prefetch:adaptive");
                        digest.update(initial.to_be_bytes());
                        digest.update(min.to_be_bytes());
                        digest.update(max.to_be_bytes());
                        digest.update(
                            u64::try_from(target_buffer.as_millis())
                                .unwrap_or(u64::MAX)
                                .to_be_bytes(),
                        );
                    }
                }
```

In `consumer/set.rs`:

- Change the `Subscription` field (line 33) to `pub(crate) prefetch: PrefetchConfig,` and the default in `Subscription::new` (line 58) to `prefetch: PrefetchConfig::Fixed(16),`.
- Replace the builder (lines 70-74) with:

```rust
    #[must_use]
    pub const fn prefetch(mut self, prefetch: u16) -> Self {
        self.prefetch = PrefetchConfig::Fixed(prefetch);
        self
    }

    /// Sets the full prefetch policy (fixed or adaptive).
    #[must_use]
    pub const fn prefetch_config(mut self, prefetch: PrefetchConfig) -> Self {
        self.prefetch = prefetch;
        self
    }

    /// The prefetch applied to the channel when the subscription starts.
    #[must_use]
    pub const fn initial_prefetch(&self) -> u16 {
        self.prefetch.initial_value()
    }
```

- Add the import `use crate::config::PrefetchConfig;` to the `use crate::{...}` block.
- Replace the capacity computation (line 170) with:

```rust
        let total_prefetch: u64 = subscriptions
            .iter()
            .map(|subscription| u64::from(subscription.prefetch.ceiling()))
            .sum();
```

- Replace the initial QoS call (line 181) with `subscription.channel.set_qos(subscription.prefetch.initial_value())`.

In `pool/recovery_coordinator.rs` line 422, change `.prefetch(sub_config.prefetch)` to `.prefetch_config(sub_config.prefetch)` (the config type is `Copy`, no clone needed).

- [ ] **Step 5: Write failing validation + fingerprint tests**

In `config.rs` `mod tests`, add helpers and tests (keep the existing `subscription(u16)` / `worker(u16)` helpers untouched — they keep compiling because they wrap `Fixed`):

```rust
    fn subscription_with(prefetch: PrefetchConfig) -> SubscriptionConfig {
        SubscriptionConfig {
            name: "default".to_owned(),
            broker: "default".to_owned(),
            queue: "jobs".to_owned(),
            weight: 1,
            priority_class: 0,
            prefetch,
            starvation_after: Duration::from_secs(30),
            max_buffered_bytes: 64 * 1024 * 1024,
            max_message_bytes: None,
            early_ack: false,
            no_ack: false,
        }
    }

    fn worker_with(prefetch: PrefetchConfig) -> WorkerProfile {
        WorkerProfile {
            name: "main".to_owned(),
            subscriptions: vec![subscription_with(prefetch)],
            scheduler: SchedulerConfig::weighted_fair(),
        }
    }

    fn config_with(prefetch: PrefetchConfig) -> Config {
        let mut candidate = config(vec![Endpoint::new("rabbit.local", 5672)]);
        candidate.workers = vec![worker_with(prefetch)];
        candidate
    }

    fn adaptive(
        initial: u16,
        min: u16,
        max: u16,
        target_buffer: Duration,
    ) -> PrefetchConfig {
        PrefetchConfig::Adaptive {
            initial,
            min,
            max,
            target_buffer,
        }
    }

    #[test]
    fn rejects_adaptive_min_zero() {
        let error = config_with(adaptive(16, 0, 256, Duration::from_secs(5)))
            .validate()
            .unwrap_err();
        assert_eq!(error.path(), "workers.main.subscriptions.default.prefetch.min");
    }

    #[test]
    fn rejects_adaptive_max_below_min() {
        let error = config_with(adaptive(16, 8, 4, Duration::from_secs(5)))
            .validate()
            .unwrap_err();
        assert_eq!(error.path(), "workers.main.subscriptions.default.prefetch.max");
    }

    #[test]
    fn rejects_adaptive_initial_outside_bounds() {
        let error = config_with(adaptive(512, 1, 256, Duration::from_secs(5)))
            .validate()
            .unwrap_err();
        assert_eq!(error.path(), "workers.main.subscriptions.default.prefetch.initial");
    }

    #[test]
    fn rejects_adaptive_zero_target_buffer() {
        let error = config_with(adaptive(16, 1, 256, Duration::ZERO))
            .validate()
            .unwrap_err();
        assert_eq!(
            error.path(),
            "workers.main.subscriptions.default.prefetch.target_buffer_seconds"
        );
    }

    #[test]
    fn rejects_adaptive_with_early_ack() {
        let mut candidate = config_with(adaptive(16, 1, 256, Duration::from_secs(5)));
        candidate.workers[0].subscriptions[0].early_ack = true;
        let error = candidate.validate().unwrap_err();
        assert_eq!(error.path(), "workers.main.subscriptions.default.prefetch.mode");
        assert!(error.to_string().contains("acknowledgements"));
    }

    #[test]
    fn rejects_adaptive_with_no_ack() {
        let mut candidate = config_with(adaptive(16, 1, 256, Duration::from_secs(5)));
        candidate.workers[0].subscriptions[0].no_ack = true;
        let error = candidate.validate().unwrap_err();
        assert_eq!(error.path(), "workers.main.subscriptions.default.prefetch.mode");
    }

    #[test]
    fn accepts_valid_adaptive_prefetch() {
        config_with(adaptive(16, 1, 256, Duration::from_secs(5)))
            .validate()
            .expect("valid adaptive prefetch");
    }

    #[test]
    fn fingerprint_distinguishes_prefetch_policies() {
        let fixed = config_with(PrefetchConfig::Fixed(16)).validate().unwrap();
        let adaptive = config_with(adaptive(16, 1, 256, Duration::from_secs(5)))
            .validate()
            .unwrap();
        let other_fixed = config_with(PrefetchConfig::Fixed(32)).validate().unwrap();

        assert_ne!(fixed.fingerprint(), adaptive.fingerprint());
        assert_ne!(fixed.fingerprint(), other_fixed.fingerprint());
        assert_eq!(fixed.fingerprint(), config_with(PrefetchConfig::Fixed(16)).validate().unwrap().fingerprint());
    }
```

Note: `ConfigError` exposes `path()` and `Display` (`"{path}: {message}"`) — use `to_string()` for message assertions.

- [ ] **Step 6: Run the config tests to verify they pass**

Run: `rtk cargo test -p rabbit-rs-core config::tests`
Expected: PASS (new + existing, including the unchanged `rejects_zero_prefetch` with path `workers.main.subscriptions.default.prefetch`).

- [ ] **Step 7: Add the policy-bound unit test**

Append at the end of `consumer/set.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::config::PrefetchConfig;

    #[test]
    fn prefetch_policy_bounds_follow_mode() {
        let fixed = PrefetchConfig::Fixed(16);
        let adaptive = PrefetchConfig::Adaptive {
            initial: 16,
            min: 1,
            max: 256,
            target_buffer: Duration::from_secs(5),
        };

        assert_eq!(fixed.ceiling(), 16);
        assert_eq!(fixed.initial_value(), 16);
        assert_eq!(adaptive.ceiling(), 256);
        assert_eq!(adaptive.initial_value(), 16);
    }
}
```

(The full capacity path is exercised by the integration tests in Task 3.)

- [ ] **Step 8: Run the whole core suite to confirm zero regression**

Run: `rtk cargo test -p rabbit-rs-core`
Expected: PASS — all existing tests unchanged in behavior.

- [ ] **Step 9: Format and commit**

```bash
rtk cargo fmt --all
git add crates/rabbit-rs-core/src/config.rs crates/rabbit-rs-core/src/consumer/set.rs crates/rabbit-rs-core/src/pool/recovery_coordinator.rs
git commit -m "feat(config): adaptive prefetch policy type with backward-compatible wire format"
```

---

### Task 2: Pure `AdaptivePrefetch` controller

**Files:**
- Create: `crates/rabbit-rs-core/src/consumer/prefetch.rs`
- Modify: `crates/rabbit-rs-core/src/consumer/mod.rs`

**Interfaces:**
- Produces: `pub(crate) struct AdaptivePrefetch` with `const fn new(min: u16, max: u16, initial: u16, target_buffer: Duration) -> Self`, `fn observe(&mut self, latency: Duration)`, `fn tick(&mut self) -> Option<u16>`, `fn current(&self) -> u16`, `fn ewma(&self) -> Duration`; constants `EWMA_ALPHA: f64 = 0.25`, `PREFETCH_TICK: Duration = 1s`, `MIN_SAMPLES: u64 = 3`. Task 4 adds `pub struct PrefetchStat` to this module.

- [ ] **Step 1: Write the failing unit tests**

Create `crates/rabbit-rs-core/src/consumer/prefetch.rs` containing only the module doc, imports, and this test module:

```rust
//! Pure adaptive prefetch controller.
//!
//! The controller keeps an EWMA of settlement latency (which includes the PHP
//! job duration for acknowledged deliveries) and derives the prefetch value
//! that keeps approximately `target_buffer` of ready work buffered. Changes
//! apply through a relative hysteresis so broker QoS is not thrashed.

use std::time::Duration;

#[cfg(test)]
mod tests {
    use super::*;

    fn controller(initial: u16, min: u16, max: u16, target: Duration) -> AdaptivePrefetch {
        AdaptivePrefetch::new(min, max, initial, target)
    }

    #[test]
    fn tick_requires_three_samples() {
        let mut candidate = controller(16, 1, 256, Duration::from_secs(5));
        candidate.observe(Duration::from_millis(250));
        candidate.observe(Duration::from_millis(250));
        assert_eq!(candidate.tick(), None);
    }

    #[test]
    fn tick_scales_prefetch_to_target_buffer_time() {
        let mut candidate = controller(16, 1, 256, Duration::from_secs(5));
        for _ in 0..3 {
            candidate.observe(Duration::from_millis(250));
        }
        // target 5s / 250ms = 20; |20 - 16| = 4 >= max(1, 16/4) = 4
        assert_eq!(candidate.tick(), Some(20));
 ETF   }

    #[test]
    fn tick_suppresses_changes_below_the_hysteresis_band() {
        let mut candidate = controller(16, 1, 256, Duration::from_secs(5));
        for _ in 0..3 {
            candidate.observe(Duration::from_millis(250));
        }
        assert_eq!(candidate.tick(), Some(20));
        // EWMA after a 227ms job: 0.25*227 + 0.75*250 = 244.25ms -> target 21
        candidate.observe(Duration::from_millis(227));
        assert_eq!(candidate.tick(), None, "diff 1 < threshold 5");
        // EWMA after a 100ms job: 0.25*100 + 0.75*244.25 = 208.1875ms -> target 25
        candidate.observe(Duration::from_millis(100));
        assert_eq!(candidate.tick(), Some(25), "diff 5 >= threshold 5");
    }

    #[test]
    fn tick_clamps_very_fast_jobs_to_max() {
        let mut candidate = controller(16, 1, 256, Duration::from_secs(5));
        for _ in 0..3 {
            candidate.observe(Duration::from_millis(1));
        }
        assert_eq!(candidate.tick(), Some(256));
    }

    #[test]
    fn tick_clamps_very_slow_jobs_to_min() {
        let mut candidate = controller(16, 1, 256, Duration::from_secs(5));
        for _ in 0..3 {
            candidate.observe(Duration::from_secs(30));
        }
        assert_eq!(candidate.tick(), Some(1));
    }

    #[test]
    fn tick_respects_a_narrow_fixed_band() {
        let mut candidate = controller(16, 16, 16, Duration::from_secs(5));
        for _ in 0..3 {
            candidate.observe(Duration::from_millis(1));
        }
        assert_eq!(candidate.tick(), None, "target clamps to the band; no change");
    }

    #[test]
    fn ewma_is_zero_before_any_sample_and_equals_the_first_sample() {
        let mut candidate = controller(16, 1, 256, Duration::from_secs(5));
        assert_eq!(candidate.ewma(), Duration::ZERO);
        candidate.observe(Duration::from_millis(100));
        assert_eq!(candidate.ewma(), Duration::from_millis(100));
ecult        assert_eq!(candidate.current(), 16);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `rtk cargo test -p rabbit-rs-core consumer::prefetch`
Expected: COMPILE ERROR — module has no `AdaptivePrefetch`.

- [ ] **Step 3: Implement the controller**

In the same file, above the test module, add:

```rust
/// EWMA smoothing factor applied to each observed settlement latency.
pub(crate) const EWMA_ALPHA: f64 = 0.25;
/// Interval between prefetch applications inside the consumer actor.
pub(crate) const PREFETCH_TICK: Duration = Duration::from_secs(1);
/// Settlement samples required before the first adjustment.
pub(crate) const MIN_SAMPLES: u64 = 3;
/// Relative hysteresis: a change applies when the target differs from the
/// current value by at least `current / HYSTERESIS_DIVISOR` (minimum 1).
const HYSTERESIS_DIVISOR: u64 = 4;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AdaptivePrefetch {
    min: u16,
    max: u16,
    target_buffer: Duration,
    current: u16,
    ewma_ns: f64,
    samples: u64,
}

impl AdaptivePrefetch {
    #[must_use]
    pub(crate) const fn new(min: u16, max: u16, initial: u16, target_buffer: Duration) -> Self {
        Self {
            min,
            max,
            target_buffer,
            current: initial,
            ewma_ns: 0.0,
            samples: 0,
        }
    }

    #[must_use]
    pub(crate) const fn current(&self) -> u16 {
        self.current
    }

    #[must_use]
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "EWMA nanoseconds are non-negative and fit far below u64::MAX"
    )]
    pub(crate) fn ewma(&self) -> Duration {
        if self.ewma_ns.is_finite() && self.ewma_ns > 0.0 {
            Duration::from_nanos(self.ewma_ns as u64)
        } else {
            Duration::ZERO
        }
    }

    /// Records one acknowledged settlement latency.
    pub(crate) fn observe(&mut self, latency: Duration) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "nanosecond durations fit in the 52-bit mantissa"
        )]
        let nanos = latency.as_nanos() as f64;
        self.ewma_ns = if self.samples == 0 {
            nanos
        } else {
            EWMA_ALPHA * nanos + (1.0 - EWMA_ALPHA) * self.ewma_ns
        };
        self.samples = self.samples.saturating_add(1);
    }

    /// Computes the next prefetch adjustment, if hysteresis allows one.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "nanosecond durations fit in the 52-bit mantissa"
    )]
    pub(crate) fn tick(&mut self) -> Option<u16> {
        if self.samples < MIN_SAMPLES {
            return None;
        }
        let target_nanos = self.target_buffer.as_nanos() as f64;
        let desired = if self.ewma_ns.is_finite() && self.ewma_ns > 0.0 {
            (target_nanos / self.ewma_ns).ceil()
        } else {
            f64::INFINITY
        };
        let desired = if desired.is_finite() {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "ceil() result is non-negative; saturated by try_from below"
            )]
            let as_u64 = desired as u64;
            u16::try_from(as_u64).unwrap_or(u16::MAX)
        } else {
            u16::MAX
        };
        let desired = desired.clamp(self.min, self.max);
        let threshold = u16::try_from(
            (u32::from(self.current) / u32::try_from(HYSTERESIS_DIVISOR).unwrap_or(1)).max(1),
        )
        .unwrap_or(u16::MAX);
        if desired.abs_diff(self.current) >= threshold {
            self.current = desired;
            Some(desired)
        } else {
            None
        }
    }
}
```

Export it in `consumer/mod.rs` — add `mod prefetch;` next to `mod scheduler;` (keep the module private; Task 4 adds `pub use prefetch::PrefetchStat;`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo test -p rabbit-rs-core consumer::prefetch`
Expected: PASS (7 tests).

- [ ] **Step 5: Format, clippy, and commit**

```bash
rtk cargo fmt --all
rtk cargo clippy -p rabbit-rs-core --all-targets --all-features -- -D warnings
git add crates/rabbit-rs-core/src/consumer/prefetch.rs crates/rabbit-rs-core/src/consumer/mod.rs
git commit -m "feat(consumer): pure adaptive prefetch controller with EWMA and hysteresis"
```

---

### Task 3: Actor integration — observation, tick, detached `set_qos`

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs`
- Modify: `crates/rabbit-rs-core/tests/consumer.rs`

**Interfaces:**
- Consumes: `AdaptivePrefetch`, `PREFETCH_TICK` from Task 2; `Subscription::prefetch: PrefetchConfig` from Task 1.
- Produces: `ActorState` field `adaptive_prefetch: HashMap<SubscriptionId, AdaptivePrefetch>`; `RuntimeSubscription` fields `queue: String`, `prefetch: PrefetchConfig`; `SettlementResult` field `is_plain_ack: bool`; methods `fn has_adaptive_prefetch(&self) -> bool`, `fn collect_prefetch_updates(&mut self) -> Vec<(SubscriptionId, Arc<dyn ConsumerChannel>, u16)>`.

- [ ] **Step 1: Write the failing integration tests**

In `crates/rabbit-rs-core/tests/consumer.rs`:

1. Add `PrefetchConfig` to the config import list (lines 9-12) so it reads `config::{BrokerConfig, Config, Credentials, Endpoint, PrefetchConfig, PublisherConfigSection, TlsConfig, TopologyMode}`.
2. Inside `mod helper`, after the `subscription` function, add:

```rust
    pub async fn adaptive_subscription(
        transport: &MockTransport,
        id: &str,
        key: ConnectionKey,
    ) -> Subscription {
        subscription(transport, id, key, 16, 0)
            .await
            .prefetch_config(PrefetchConfig::Adaptive {
                initial: 16,
                min: 1,
                max: 256,
                target_buffer: Duration::from_secs(5),
            })
    }
```

3. Append at the end of the file:

```rust
// ---------------------------------------------------------------------------
// Adaptive prefetch
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn adaptive_prefetch_grows_to_max_after_fast_jobs() {
    let transport = MockTransport::default();
    for tag in 1..=4 {
        transport.push_delivery(Ok(delivery(tag, b"job")));
    }
    let consumer = ConsumerSet::spawn(vec![
        adaptive_subscription(&transport, "adaptive", connection_key("adaptive", "/")).await,
    ])
    .await
    .expect("consumer set");
    let_sources_fill().await;

    for _ in 0..3 {
        let delivery = consumer.next().await.expect("delivery");
        consumer.ack_through(&delivery).await.expect("ack");
        let_actor_process().await;
    }
    // Deterministically fire the 1s controller tick under paused time, then
    // let the detached set_qos task record its operation.
    tokio::time::advance(Duration::from_secs(1)).await;
    let_actor_process().await;

    let qos_values: Vec<u16> = transport
        .operations()
        .iter()
        .filter_map(|operation| match operation {
            TransportOperation::Qos { prefetch } => Some(*prefetch),
            _ => None,
        })
        .collect();
    assert_eq!(qos_values, vec![16, 256]);
}

#[tokio::test(start_paused = true)]
async fn adaptive_prefetch_holds_when_hysteresis_band_not_crossed() {
    let transport = MockTransport::default();
    for tag in 1..=4 {
        transport.push_delivery(Ok(delivery(tag, b"job")));
    }
    let subscription = subscription(&transport, "adaptive", connection_key("adaptive", "/"), 16, 0)
        .await
        .prefetch_config(PrefetchConfig::Adaptive {
            initial: 16,
            min: 16,
            max: 16,
            target_buffer: Duration::from_secs(5),
        });
    let consumer = ConsumerSet::spawn(vec![subscription])
        .await
        .expect("consumer set");
    let_sources_fill().await;

    for _ in 0..3 {
        let delivery = consumer.next().await.expect("delivery");
        consumer.ack_through(&delivery).await.expect("ack");
        let_actor_process().await;
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    let_actor_process().await;

    let qos_values: Vec<u16> = transport
        .operations()
        .iter()
        .filter_map(|operation| match operation {
            TransportOperation::Qos { prefetch } => Some(*prefetch),
            _ => None,
        })
        .collect();
    assert_eq!(qos_values, vec![16], "target clamps to the band; no extra QoS");
}

#[tokio::test(start_paused = true)]
async fn adaptive_prefetch_set_qos_failure_surfaces_and_actor_survives() {
    let transport = MockTransport::default();
    for tag in 1..=5 {
        transport.push_delivery(Ok(delivery(tag, b"job")));
    }
    let consumer = ConsumerSet::spawn(vec![
        adaptive_subscription(&transport, "adaptive", connection_key("adaptive", "/")).await,
    ])
    .await
    .expect("consumer set");
    let_sources_fill().await;

    for _ in 0..3 {
        let delivery = consumer.next().await.expect("delivery");
        consumer.ack_through(&delivery).await.expect("ack");
        let_actor_process().await;
    }
    transport.push_consumer_result(Err(TransportError::connection("qos rejected")));
    tokio::time::advance(Duration::from_secs(1)).await;
    let_actor_process().await;

    let errors = consumer.drain_errors();
    assert!(
        errors.iter().any(|error| {
            error.message.contains("adaptive prefetch set_qos(256) failed")
        }),
        "expected the set_qos failure in drain_errors, got {errors:?}"
    );

    // The actor keeps consuming and settling after the failed adjustment.
    let fourth = consumer.next().await.expect("delivery after failure");
    consumer.ack_through(&fourth).await.expect("ack after failure");
    let_actor_process().await;
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `rtk cargo test -p rabbit-rs-core --test consumer adaptive`
Expected: FAIL/COMPILE ERROR — `Adaptive` variants never trigger QoS changes (first two tests see only `vec![16]`), and no `drain_errors` entry appears for the third.

- [ ] **Step 3: Implement the actor changes**

In `crates/rabbit-rs-core/src/consumer/actor.rs`:

1. Extend imports: change `use tokio::sync::{mpsc, oneshot};` to `use tokio::{sync::{mpsc, oneshot}, time::MissedTickBehavior};`, and add `use super::prefetch::{AdaptivePrefetch, PREFETCH_TICK};` plus `use crate::config::PrefetchConfig;` alongside the other `use crate::...` imports.

2. Extend `RuntimeSubscription` (line 91) with two fields:

```rust
struct RuntimeSubscription {
    connection_key: crate::pool::ConnectionKey,
    generation: u64,
    channel_id: u16,
    channel: Arc<dyn crate::transport::ConsumerChannel>,
    publisher: Option<crate::publisher::PublisherHandle>,
    destination: Option<crate::publisher::Destination>,
    delay_strategy: Option<DelayStrategy>,
    early_ack: bool,
    no_ack: bool,
    queue: String,
    prefetch: PrefetchConfig,
}
```

3. Extend `ActorState` (line 103) with:

```rust
    adaptive_prefetch: HashMap<SubscriptionId, AdaptivePrefetch>,
```

4. In `ActorState::new` (line 124): restructure the existing loop to build the adaptive map and populate the two new `RuntimeSubscription` fields:

```rust
        let mut adaptive_prefetch = HashMap::new();
        for subscription in subscriptions {
            scheduler.register(subscription.id.clone(), subscription.policy);
            buffers.insert(subscription.id.clone(), VecDeque::new());
            buffered_bytes.insert(subscription.id.clone(), 0);
            max_buffered_bytes.insert(subscription.id.clone(), subscription.max_buffered_bytes);
            if let PrefetchConfig::Adaptive {
                initial,
                min,
                max,
                target_buffer,
            } = subscription.prefetch
            {
                adaptive_prefetch.insert(
                    subscription.id.clone(),
                    AdaptivePrefetch::new(min, max, initial, target_buffer),
                );
            }
            let channel_key = (
                subscription.id.clone(),
                subscription.channel_id,
                subscription.generation,
            );
            channel_ledgers.insert(channel_key, ChannelLedger::default());
            runtime.insert(
                subscription.id.clone(),
                RuntimeSubscription {
                    connection_key: subscription.connection_key,
                    generation: subscription.generation,
                    channel_id: subscription.channel_id,
                    channel: subscription.channel,
                    publisher: subscription.publisher,
                    destination: subscription.destination,
                    delay_strategy: subscription.delay_strategy,
                    early_ack: subscription.early_ack,
                    no_ack: subscription.no_ack,
                    queue: subscription.queue,
                    prefetch: subscription.prefetch,
                },
            );
        }
```

and add `adaptive_prefetch` to the `Self { ... }` literal.

5. Add helper methods to `ActorState` (next to `channel_key_for`):

```rust
    fn has_adaptive_prefetch(&self) -> bool {
        !self.adaptive_prefetch.is_empty()
    }

    /// Advances every adaptive controller and returns the QoS changes to apply.
    fn collect_prefetch_updates(
        &mut self,
    ) -> Vec<(SubscriptionId, Arc<dyn crate::transport::ConsumerChannel>, u16)> {
        let mut updates = Vec::new();
        for (id, controller) in self.adaptive_prefetch.iter_mut() {
            if let Some(value) = controller.tick()
                && let Some(runtime) = self.subscriptions.get(id)
            {
                updates.push((id.clone(), Arc::clone(&runtime.channel), value));
            }
        }
        updates
    }
```

6. Observation site 1 — `Settle` ack completion (line 613).

   **Important:** delayed releases (`Settlement::Release(delay > 0)`) also complete
   as `DeliveryState::Acked` (republish → confirm → ack), and their latency includes
   the delay — observing them would corrupt the EWMA. Guard with the original
   settlement kind:

   a. Extend `SettlementResult` (line 49) with `is_plain_ack: bool`.
   b. In `launch_settlement` (line 766), compute it from the params and carry it
      through the future:

```rust
    let settlement = params.settlement;
    let is_plain_ack = matches!(settlement, Settlement::Ack);
```

   and in the `SettlementResult` construction at the end of the pushed future:

```rust
        SettlementResult {
            channel_key,
            token,
            is_plain_ack,
            result,
        }
```

   c. Change the `DeliveryState::Acked` arm (line 613) to:

```rust
                            DeliveryState::Acked => {
                                if settlement_result.is_plain_ack
                                    && let Some(controller) = state
                                        .adaptive_prefetch
                                        .get_mut(&settlement_result.token.subscription)
                                {
                                    controller.observe(settlement_result.token.reserved_at.elapsed());
                                }
                                state.metrics.record_ack(settlement_result.token.reserved_at.elapsed());
                            }
```

   `SettleThrough` (site 2) is always a genuine contiguous-prefix ack — no guard needed there.

7. Observation site 2 — `SettleThrough` ack completion (line 694). Inside the `if let Ok(DeliveryState::Acked)` block, hoist the elapsed computation and observe before recording:

```rust
                    if let Ok(DeliveryState::Acked) = &settle_through_result.result {
                        if let Some(controller) = state
                            .adaptive_prefetch
                            .get_mut(&settle_through_result.channel_key.0)
                        {
                            controller.observe(
                                settle_through_result
                                    .affected_tokens
                                    .last()
                                    .map_or(Duration::ZERO, |token| token.reserved_at.elapsed()),
                            );
                        }
                        for token in &settle_through_result.affected_tokens {
```

(the existing `record_ack(...)` call below uses `.last().unwrap()` — keep it, or refactor it to reuse the same `map_or` expression; either is acceptable as long as behavior is identical.)

8. Tick arm — in `run_actor` (line 392), after `let mut state = ActorState::new(...)` and before the `loop`, add:

```rust
    let has_adaptive = state.has_adaptive_prefetch();
    let mut prefetch_interval = tokio::time::interval(PREFETCH_TICK);
    prefetch_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
```

Then inside `tokio::select!`, after the `() = dispatch_notify.notified()` arm, add:

```rust
            _ = prefetch_interval.tick(), if has_adaptive => {
                for (subscription, channel, value) in state.collect_prefetch_updates() {
                    let error_tx = state.error_tx.clone();
                    tokio::spawn(async move {
                        if let Err(error) = channel.set_qos(value).await {
                            let _ = error_tx.send(SettlementError {
                                delivery_tag: 0,
                                subscription,
                                kind: ConsumerErrorKind::Transport,
                                message: format!("adaptive prefetch set_qos({value}) failed: {error}"),
                                timestamp: Instant::now(),
                            });
                        }
                    });
                }
            }
```

Note: the first tick of a `tokio::time::interval` completes immediately; the controllers have zero samples then, so `tick()` returns `None` — no spurious QoS.

- [ ] **Step 4: Run the adaptive tests to verify they pass**

Run: `rtk cargo test -p rabbit-rs-core --test consumer adaptive`
Expected: PASS (3 tests).

- [ ] **Step 5: Run the whole consumer suite to confirm zero regression**

Run: `rtk cargo test -p rabbit-rs-core --test consumer`
Expected: PASS — including the fixed-mode tests, which must not observe any QoS beyond the spawn value.

- [ ] **Step 6: Format, clippy, and commit**

```bash
rtk cargo fmt --all
rtk cargo clippy -p rabbit-rs-core --all-targets --all-features -- -D warnings
git add crates/rabbit-rs-core/src/consumer/actor.rs crates/rabbit-rs-core/tests/consumer.rs
git commit -m "feat(consumer): apply adaptive prefetch adjustments from the actor tick"
```

---

### Task 4: Observability — `GetPrefetchStats` and `ConsumerHandle::prefetch_stats`

**Files:**
- Modify: `crates/rabbit-rs-core/src/consumer/prefetch.rs`
- Modify: `crates/rabbit-rs-core/src/consumer/actor.rs` (command enum + handler + `prefetch_stats` state method)
- Modify: `crates/rabbit-rs-core/src/consumer/set.rs` (handle method)
- Modify: `crates/rabbit-rs-core/src/consumer/mod.rs` (export)

**Interfaces:**
- Produces: `pub struct PrefetchStat { pub subscription: String, pub queue: String, pub mode: &'static str, pub current: u16, pub ewma: Duration }` (derives `Clone, Debug, Eq, PartialEq`); `ConsumerCommand::GetPrefetchStats { completed: oneshot::Sender<Vec<PrefetchStat>> }`; `pub async fn ConsumerHandle::prefetch_stats(&self) -> Result<Vec<PrefetchStat>, ConsumerError>`; `fn ActorState::prefetch_stats(&self) -> Vec<PrefetchStat>`.

- [ ] **Step 1: Write the failing test**

Append to `crates/rabbit-rs-core/tests/consumer.rs`:

```rust
#[tokio::test]
async fn prefetch_stats_reports_fixed_and_adaptive_state() {
    let transport = MockTransport::default();
    let fixed_subscription = subscription(&transport, "fixed", connection_key("fixed", "/"), 16, 0).await;
    let adaptive_subscription = helper::adaptive_subscription(
        &transport,
        "adaptive",
        connection_key("adaptive", "/"),
    )
    .await;
    let consumer = ConsumerSet::spawn(vec![fixed_subscription, adaptive_subscription])
        .await
        .expect("consumer set");

    let stats = consumer.prefetch_stats().await.expect("stats");

    assert_eq!(stats.len(), 2);
    let adaptive = stats
        .iter()
        .find(|stat| stat.subscription == "adaptive")
        .expect("adaptive stat");
    assert_eq!(adaptive.mode, "adaptive");
    assert_eq!(adaptive.current, 16);
    assert_eq!(adaptive.ewma, Duration::ZERO);
    let fixed = stats
        .iter()
        .find(|stat| stat.subscription == "fixed")
        .expect("fixed stat");
    assert_eq!(fixed.mode, "fixed");
    assert_eq!(fixed.current, 16);
    assert_eq!(fixed.ewma, Duration::ZERO);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test -p rabbit-rs-core --test consumer prefetch_stats`
Expected: COMPILE ERROR — no `prefetch_stats` on `ConsumerHandle`.

- [ ] **Step 3: Implement**

In `consumer/prefetch.rs`, above `AdaptivePrefetch`, add the public snapshot type:

```rust
/// Per-subscription prefetch observability snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrefetchStat {
    /// Subscription identifier.
    pub subscription: String,
    /// Queue consumed by the subscription.
    pub queue: String,
    /// `"fixed"` or `"adaptive"`.
    pub mode: &'static str,
    /// Prefetch currently applied (spawn value for fixed subscriptions).
    pub current: u16,
    /// EWMA of acknowledged settlement latency; zero before any sample.
    pub ewma: Duration,
}
```

In `consumer/mod.rs`, add `pub use prefetch::PrefetchStat;` next to the other `pub use` lines.

In `consumer/actor.rs`:

1. Add to the `use super::{...}` list: `crate::consumer? no` — add `PrefetchStat` via `use super::prefetch::{AdaptivePrefetch, PrefetchStat, PREFETCH_TICK};` (extends Task 3's import).

2. Add the command variant to `ConsumerCommand` (line 71):

```rust
    GetPrefetchStats {
        completed: oneshot::Sender<Vec<PrefetchStat>>,
    },
```

3. Add the `select!` arm (place it right after the `UpdateGeneration` arm, before the `Close` arm):

```rust
                Some(ConsumerCommand::GetPrefetchStats { completed }) => {
                    let _ = completed.send(state.prefetch_stats());
                }
```

4. Add the state method next to `collect_prefetch_updates`:

```rust
    fn prefetch_stats(&self) -> Vec<PrefetchStat> {
        let mut stats = Vec::with_capacity(self.subscriptions.len());
        for (id, runtime) in &self.subscriptions {
            let (mode, mut current) = match runtime.prefetch {
                PrefetchConfig::Fixed(value) => ("fixed", value),
                PrefetchConfig::Adaptive { initial, .. } => ("adaptive", initial),
            };
            let mut ewma = Duration::ZERO;
            if let Some(controller) = self.adaptive_prefetch.get(id) {
                current = controller.current();
                ewma = controller.ewma();
            }
            stats.push(PrefetchStat {
                subscription: id.as_str().to_owned(),
                queue: runtime.queue.clone(),
                mode,
                current,
                ewma,
            });
        }
        stats.sort_by(|left, right| left.subscription.cmp(&right.subscription));
        stats
    }
```

5. In `consumer/set.rs`, add to `impl ConsumerHandle` (after `metrics_snapshot`):

```rust
    /// Snapshot of per-subscription prefetch state (mode, applied value, EWMA).
    ///
    /// # Errors
    ///
    /// Returns a typed error when the consumer is closed.
    pub async fn prefetch_stats(&self) -> Result<Vec<PrefetchStat>, ConsumerError> {
        let (completed, receiver) = oneshot::channel();
        self.commands
            .send(ConsumerCommand::GetPrefetchStats { completed })
            .await
            .map_err(|_| ConsumerError::closed())?;
        receiver.await.map_err(|_| ConsumerError::closed())
    }
```

and add `PrefetchStat` to the `use super::{...}` import list at the top of the file.

- [ ] **Step 3b: check `SubscriptionId::as_str` exists** — it is used already in `set.rs:192` (`format!("rabbit-rs.{}", subscription.id.as_str())`), so it exists; if the name differs, adjust.

- [ ] **Step 4: Run test to verify it passes**

Run: `rtk cargo test -p rabbit-rs-core --test consumer prefetch_stats`
Expected: PASS.

- [ ] **Step 5: Run the full core suite, format, and commit**

```bash
rtk cargo test -p rabbit-rs-core
rtk cargo fmt --all
git add crates/rabbit-rs-core/src/consumer/prefetch.rs crates/rabbit-rs-core/src/consumer/actor.rs crates/rabbit-rs-core/src/consumer/set.rs crates/rabbit-rs-core/src/consumer/mod.rs crates/rabbit-rs-core/tests/consumer.rs
git commit -m "feat(consumer): expose per-subscription prefetch stats"
```

---

### Task 5: PHP extension — `Consumer::getPrefetchStats()`

**Files:**
- Modify: `crates/rabbit-rs-php/src/classes/consumer.rs` (after `drainErrors`, line ~241)
- Modify: `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php` (after the `drainErrors` declaration, line ~223)
- Modify: `crates/rabbit-rs-php/tests/Reflection/ReflectionTest.php` (near the `Consumer` method expectations, line ~131)

**Interfaces:**
- Consumes: `ConsumerHandle::prefetch_stats() -> Result<Vec<PrefetchStat>, ConsumerError>` from Task 4.
- Produces: PHP method `Goopil\RabbitRs\Consumer::getPrefetchStats(): array` keyed by subscription name with entries `{ mode: string, prefetch: int, ewma_ms: int }`.

- [ ] **Step 1: Write the failing reflection test**

In `crates/rabbit-rs-php/tests/Reflection/ReflectionTest.php`, next to the existing `Consumer` method expectations (the `drainErrors` expectation), add:

```php
        expectMethod(\Goopil\RabbitRs\Consumer::class, 'getPrefetchStats', [], 'array');
```

- [ ] **Step 2: Run the extension test suite to verify it fails**

Run: `./scripts/test-extension.sh`
Expected: FAIL — `getPrefetchStats` is not declared on the class (reflection assertion).

- [ ] **Step 3: Implement the method**

In `crates/rabbit-rs-php/src/classes/consumer.rs`, inside the `#[php_impl] impl Consumer` block after `drainErrors`, add:

```rust
    /// Returns per-subscription prefetch state for observability.
    ///
    /// Returns an array keyed by subscription name; each entry contains
    /// `mode` (`"fixed"` or `"adaptive"`), `prefetch` (currently applied
    /// value), and `ewma_ms` (EWMA of acknowledged settlement latency in
    /// milliseconds, 0 before any acknowledged job).
    pub fn getPrefetchStats(&self) -> PhpResult<ZBox<ZendHashTable>> {
        self.ensure_open("Goopil\\RabbitRs\\Consumer::getPrefetchStats")?;
        let stats = self.runtime.block_on(self.handle.prefetch_stats()).map_err(
            |error| {
                ext_php_rs::prelude::PhpException::from_class::<
                    super::exception::RabbitRsException,
                >(error.to_string())
            },
        )?;
        let mut table = ZendHashTable::new();
        for stat in stats {
            let mut entry = ZendHashTable::new();
            entry.insert("mode", stat.mode)?;
            entry.insert("prefetch", i64::from(stat.current))?;
            entry.insert(
                "ewma_ms",
                i64::try_from(stat.ewma.as_millis()).unwrap_or(i64::MAX),
            )?;
            table.insert(stat.subscription.as_str(), entry)?;
        }
        Ok(table)
    }
```

In `crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`, directly after the `drainErrors` declaration, add (match the surrounding indentation exactly):

```php
  /**
   * Per-subscription prefetch state: mode ("fixed" or "adaptive"), currently
   * applied prefetch, and EWMA of settlement latency in milliseconds
   * (0 before any acknowledged job).
   */
  public function getPrefetchStats(): array {}
```

Validate the stub syntax: `php -l crates/rabbit-rs-php/stubs/rabbit_rs.stub.php`.

- [ ] **Step 4: Run the extension test suite to verify it passes**

Run: `./scripts/test-extension.sh`
Expected: PASS (reflection + Pest + PHPT).

- [ ] **Step 5: Commit**

```bash
git add crates/rabbit-rs-php/src/classes/consumer.rs crates/rabbit-rs-php/stubs/rabbit_rs.stub.php crates/rabbit-rs-php/tests/Reflection/ReflectionTest.php
git commit -m "feat(php): expose consumer prefetch stats to PHP"
```

---

### Task 6: Laravel layer — normalizer, config docs, README

**Files:**
- Modify: `packages/laravel-queue/src/Config/ConfigNormalizer.php` (prefetch at lines 421-431, call site at lines 342-345, docblock at line 325)
- Modify: `packages/laravel-queue/config/rabbit-rs.php` (doc block lines 175-190)
- Modify: `packages/laravel-queue/README.md` (prefetch section, around line 218-272)
- Test: `packages/laravel-queue/tests/Unit/ConfigNormalizerTest.php`
- Test: `packages/laravel-queue/tests/Feature/MultiVhostWorkerTest.php`

**Interfaces:**
- Consumes: core wire forms from Task 1.
- Produces: `ConfigNormalizer` emits a plain int for fixed mode (unchanged) and `array{mode: 'adaptive', initial: int, min: int, max: int, target_buffer_seconds: int}` for adaptive mode; rejects adaptive combined with `early_ack`/`no_ack`.

- [ ] **Step 1: Write the failing Pest tests**

In `packages/laravel-queue/tests/Unit/ConfigNormalizerTest.php`, add a new describe block (imports already present: `ConfigNormalizer`):

```php
describe('prefetch', function (): void {
    it('keeps emitting a plain integer for the fixed mode', function (): void {
        $config = configValidConfig();
        $config['workers']['main']['subscriptions']['orders']['prefetch'] = [
            'mode' => 'fixed',
            'value' => 8,
        ];

        $normalized = ConfigNormalizer::normalize($config);

        expect(8)->toBe($normalized['native']['workers'][0]['subscriptions'][0]['prefetch']);
    });

    it('accepts a plain integer prefetch as fixed', function (): void {
        $config = configValidConfig();
        $config['workers']['main']['subscriptions']['orders']['prefetch'] = 32;

        $normalized = ConfigNormalizer::normalize($config);

        expect(32)->toBe($normalized['native']['workers'][0]['subscriptions'][0]['prefetch']);
    });

    it('forwards an adaptive prefetch config to the native config', function (): void {
        $config = configValidConfig();
        $config['workers']['main']['subscriptions']['orders']['prefetch'] = [
            'mode' => 'adaptive',
            'initial' => 16,
            'min' => 1,
            'max' => 256,
            'target_buffer_seconds' => 5,
        ];

        $normalized = ConfigNormalizer::normalize($config);

        expect([
            'mode' => 'adaptive',
            'initial' => 16,
            'min' => 1,
            'max' => 256,
            'target_buffer_seconds' => 5,
        ])->toBe($normalized['native']['workers'][0]['subscriptions'][0]['prefetch']);
    });

    it('rejects an adaptive prefetch whose max is below min', function (): void {
        $config = configValidConfig();
        $config['workers']['main']['subscriptions']['orders']['prefetch'] = [
            'mode' => 'adaptive',
            'initial' => 16,
            'min' => 8,
            'max' => 4,
            'target_buffer_seconds' => 5,
        ];

        expect(fn () => ConfigNormalizer::normalize($config))->toThrow(
            InvalidArgumentException::class,
            'workers.main.subscriptions.orders.prefetch.max',
        );
    });

    it('rejects an adaptive prefetch whose initial is outside the bounds', function (): void {
        $config = configValidConfig();
        $config['workers']['main']['subscriptions']['orders']['prefetch'] = [
            'mode' => 'adaptive',
            'initial' => 512,
            'min' => 1,
            'max' => 256,
            'target_buffer_seconds' => 5,
        ];

        expect(fn () => ConfigNormalizer::normalize($config))->toThrow(
            InvalidArgumentException::class,
            'workers.main.subscriptions.orders.prefetch.initial',
        );
    });

    it('rejects a zero target buffer seconds', function (): void {
        $config = configValidConfig();
        $config['workers']['main']['subscriptions']['orders']['prefetch'] = [
            'mode' => 'adaptive',
            'initial' => 16,
            'min' => 1,
            'max' => 256,
            'target_buffer_seconds' => 0,
        ];

        expect(fn () => ConfigNormalizer::normalize($config))->toThrow(
            InvalidArgumentException::class,
            'workers.main.subscriptions.orders.prefetch.target_buffer_seconds',
        );
    });

    it('rejects an adaptive prefetch combined with early_ack', function (): void {
        $config = configValidConfig();
        $config['workers']['main']['subscriptions']['orders']['prefetch'] = [
            'mode' => 'adaptive',
            'initial' => 16,
            'min' => 1,
            'max' => 256,
            'target_buffer_seconds' => 5,
        ];
        $config['workers']['main']['subscriptions']['orders']['early_ack'] = true;
        $config['best_effort'] = true;

        expect(fn () => ConfigNormalizer::normalize($config))->toThrow(
            InvalidArgumentException::class,
            'workers.main.subscriptions.orders.prefetch.mode',
        );
    });
});
```

- [ ] **Step 2: Run the Laravel unit tests to verify they fail**

Run: `./scripts/test-laravel.sh`
Expected: FAIL — adaptive mode rejected with "must be fixed".

- [ ] **Step 3: Implement the normalizer changes**

In `ConfigNormalizer.php`, replace `prefetch()` (lines 421-431) with:

```php
    /**
     * @return int|array{mode: string, initial: int, min: int, max: int, target_buffer_seconds: int}
     */
    private static function prefetch(mixed $prefetch, string $path, bool $earlyAck, bool $noAck): int|array
    {
        if (is_int($prefetch)) {
            return self::positiveInt($prefetch, $path, 65535);
        }
        if (! is_array($prefetch)) {
            self::invalid($path, 'must be an integer or an array with a mode');
        }

        $mode = $prefetch['mode'] ?? null;
        if ($mode === 'fixed') {
            return self::positiveInt($prefetch['value'] ?? null, $path.'.value', 65535);
        }
        if ($mode === 'adaptive') {
            $min = self::positiveInt($prefetch['min'] ?? null, $path.'.min', 65535);
            $max = self::positiveInt($prefetch['max'] ?? null, $path.'.max', 65535);
            if ($max < $min) {
                self::invalid($path.'.max', 'must be greater than or equal to min');
            }
            $initial = self::positiveInt($prefetch['initial'] ?? null, $path.'.initial', 65535);
            if ($initial < $min || $initial > $max) {
                self::invalid($path.'.initial', 'must be within [min, max]');
            }
            $targetBufferSeconds = self::positiveInt(
                $prefetch['target_buffer_seconds'] ?? null,
                $path.'.target_buffer_seconds',
            );
            if ($earlyAck || $noAck) {
                self::invalid(
                    $path.'.mode',
                    'adaptive prefetch requires consumer acknowledgements: early_ack and no_ack must be false',
                );
            }

            return [
                'mode' => 'adaptive',
                'initial' => $initial,
                'min' => $min,
                'max' => $max,
                'target_buffer_seconds' => $targetBufferSeconds,
            ];
        }

        self::invalid($path.'.mode', 'must be fixed or adaptive');
    }
```

In `normalizeSubscription`, move the prefetch computation after the ack flags so it reads:

```php
        $earlyAck = self::boolean(
            $subscription['early_ack'] ?? false,
            $subscriptionPath.'.early_ack',
        );
        $noAck = self::validateAckFlags(
            $subscription['no_ack'] ?? false,
            $earlyAck,
            $bestEffort,
            $subscriptionName,
            $subscriptionPath,
        );

        $prefetch = self::prefetch(
            $subscription['prefetch'] ?? ['mode' => 'fixed', 'value' => 16],
            $subscriptionPath.'.prefetch',
            $earlyAck,
            $noAck,
        );
```

and update the method docblock at line 325 to `prefetch: int|array{mode: string, initial: int, min: int, max: int, target_buffer_seconds: int}`.

- [ ] **Step 3b: Add the feature test (provider → native pool with adaptive config)**

In `packages/laravel-queue/tests/Feature/MultiVhostWorkerTest.php`, inside the `describe('multi-vhost worker', ...)` block, add (the fake native `Pool` class is provided by the Feature bootstrap — no extension needed):

```php
    it('boots the native pool with an adaptive prefetch subscription', function () {
        $config = multiVhostConfig();
        $firstSubscription = array_key_first($config['workers']['main']['subscriptions']);
        $config['workers']['main']['subscriptions'][$firstSubscription]['prefetch'] = [
            'mode' => 'adaptive',
            'initial' => 16,
            'min' => 1,
            'max' => 256,
            'target_buffer_seconds' => 5,
        ];

        $normalized = ConfigNormalizer::normalize($config);
        $pool = new Pool($normalized['native']);

        $prefetches = array_column(
            $normalized['native']['workers'][0]['subscriptions'],
            'prefetch',
            'name',
        );
        expect($prefetches[$firstSubscription])->toBe([
            'mode' => 'adaptive',
            'initial' => 16,
            'min' => 1,
            'max' => 256,
            'target_buffer_seconds' => 5,
        ]);
        expect($pool->config)->toBe($normalized['native']);
    });
```

- [ ] **Step 4: Update config docs and README**

In `config/rabbit-rs.php`, replace lines 175-177 (the `prefetch.mode` / `prefetch.value` comments) with:

```php
    |   prefetch.mode:       "fixed" applies the constant prefetch.value.
    |                       "adaptive" keeps about target_buffer_seconds of
    |                       ready work buffered: the extension learns the job
    |                       duration (EWMA of ack latency) and adjusts prefetch
    |                       between min and max. Requires acknowledgements
    |                       (early_ack and no_ack must be false).
    |   prefetch.value:      QoS prefetch count (fixed mode). The broker delivers at most
    |                       this many unacked messages per consumer channel.
```

In `README.md`, in the prefetch explanation section (after the `prefetch.value` bullet around line 272), add:

```markdown
**`prefetch.mode: adaptive`** — instead of a constant value, the extension keeps
about `target_buffer_seconds` of ready work buffered per queue: it learns the job
duration (EWMA of ack latency) and adjusts the broker prefetch between `min` and
`max`, with a 25% hysteresis so QoS is not thrashed. Requires acknowledgements
(`early_ack`/`no_ack` must stay `false`).

```php
'prefetch' => [
    'mode' => 'adaptive',
    'initial' => 64,
    'min' => 1,
    'max' => 256,
    'target_buffer_seconds' => 5,
],
```
```

- [ ] **Step 5: Run the Laravel suite to verify everything passes**

Run: `./scripts/test-laravel.sh`
Expected: PASS (Unit + Feature, without the extension).

- [ ] **Step 6: Commit**

```bash
git add packages/laravel-queue/src/Config/ConfigNormalizer.php packages/laravel-queue/config/rabbit-rs.php packages/laravel-queue/README.md packages/laravel-queue/tests/Unit/ConfigNormalizerTest.php packages/laravel-queue/tests/Feature/MultiVhostWorkerTest.php
git commit -m "feat(laravel): adaptive prefetch configuration support"
```

---

### Task 7: Design-doc bookkeeping and full quality gate

**Files:**
- Modify: `docs/plans/2026-07-30-rabbitmq-native-design.md` (§ « Évolutions prévues », line ~344)

- [ ] **Step 1: Mark the roadmap item as implemented**

In `docs/plans/2026-07-30-rabbitmq-native-design.md`, change the line:

```
- prefetch adaptatif basé sur EWMA, target buffer time, hystérésis et pression mémoire ;
```

to:

```
- prefetch adaptatif basé sur EWMA et target buffer time — implémenté (spec
  `docs/superpowers/specs/2026-08-29-adaptive-prefetch-design.md`) ;
```

- [ ] **Step 2: Run the full quality gate**

Run: `rtk ./scripts/check.sh`
Expected: fmt + clippy (`-D warnings`) + nextest + composer validate all green.

- [ ] **Step 3: Commit**

```bash
git add docs/plans/2026-07-30-rabbitmq-native-design.md
git commit -m "docs: mark adaptive prefetch as implemented in the native design"
```

---

## Verification checklist (end of round)

- [ ] `rtk ./scripts/check.sh` green
- [ ] `rtk cargo test -p rabbit-rs-core --test consumer adaptive` — 3 deterministic paused-time tests
- [ ] `rtk cargo test -p rabbit-rs-core --test consumer prefetch_stats` — observability
- [ ] `./scripts/test-extension.sh` — reflection + Pest + PHPT green
- [ ] `./scripts/test-laravel.sh` — Unit + Feature green
- [ ] Fixed-mode behavior byte-identical (existing suites untouched and green)
