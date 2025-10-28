use ext_php_rs::exception::PhpResult;
use ext_php_rs::types::Iterable;
use std::time::Instant;

#[derive(Clone)]
pub struct AutoQosConfig {
    enabled: bool,
    min: u16,
    max: u16,
    step_up: f64,
    step_down: f64,
    cooldown_ms: u64,
    // true = throughput, false = latency
    target_throughput: bool,
}

impl Default for AutoQosConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min: 10,
            max: 500,
            step_up: 1.5,
            step_down: 0.75,
            cooldown_ms: 2000,
            target_throughput: true, // default to throughput
        }
    }
}

impl AutoQosConfig {
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn initial(&self) -> u16 {
        let lo = self.min.max(1);
        let hi = self.max.max(lo);

        ((lo as u32 + hi as u32) / 2) as u16
    }

    pub fn from_iterable(opts: Option<Iterable>) -> Self {
        let mut auto_qos_config = Self::default();

        if let Some(Iterable::Array(arr)) = &opts {
            if let Some(v) = arr.get("auto_qos") {
                if let Some(cfg) = v.array() {
                    if let Some(v) = cfg.get("enabled") {
                        if let Some(b) = v.bool() {
                            auto_qos_config.enabled = b;
                        }
                    }
                    if let Some(v) = cfg.get("min") {
                        if let Some(i) = v.long() {
                            let i = i.clamp(1, i64::from(u16::MAX));
                            auto_qos_config.min = i as u16;
                        }
                    }
                    if let Some(v) = cfg.get("max") {
                        if let Some(i) = v.long() {
                            let i = i.clamp(1, i64::from(u16::MAX));
                            auto_qos_config.max = i as u16;
                        }
                    }
                    if let Some(v) = cfg.get("step_up") {
                        if let Some(f) = v.double() {
                            auto_qos_config.step_up = if f > 1.0 { f } else { 1.0 };
                        }
                    }
                    if let Some(v) = cfg.get("step_down") {
                        if let Some(f) = v.double() {
                            auto_qos_config.step_down =
                                if (0.0..1.0).contains(&f) { f } else { 0.75 };
                        }
                    }
                    if let Some(v) = cfg.get("cooldown_ms") {
                        if let Some(i) = v.long() {
                            auto_qos_config.cooldown_ms = i.max(250) as u64;
                        }
                    }
                    if let Some(v) = cfg.get("target") {
                        if let Some(s) = v.string() {
                            auto_qos_config.target_throughput = s.to_lowercase() != "latency";
                        }
                    }
                }
            }
        }

        auto_qos_config
    }
}

pub struct AutoQosState {
    cfg: AutoQosConfig,
    current: u16,
    last_adjust: Instant,
    acc_drained: usize,
}

impl AutoQosState {
    pub fn new(
        cfg: AutoQosConfig,
        apply_qos: impl FnOnce(u16) -> PhpResult<()>,
    ) -> PhpResult<Self> {
        let start = cfg.initial();
        apply_qos(start)?;
        Ok(Self {
            cfg,
            current: start,
            last_adjust: Instant::now(),
            acc_drained: 0,
        })
    }

    /// Record how many messages were drained in this poll and, if cooldown elapsed,
    /// compute a new QoS prefetch and apply it via the provided callback.
    /// The callback is responsible for applying the QoS to the underlying channel.
    pub fn record_and_maybe_adjust<F>(
        &mut self,
        drained_now: usize,
        mut apply_qos: F,
    ) -> PhpResult<()>
    where
        F: FnMut(u16) -> PhpResult<()>,
    {
        self.acc_drained = self.acc_drained.saturating_add(drained_now);
        if self.last_adjust.elapsed().as_millis() as u64 >= self.cfg.cooldown_ms {
            let cur = self.current as usize;
            let drained = self.acc_drained;
            let mut next = self.current;

            if self.cfg.target_throughput {
                // If we drain close to capacity, scale up; if very low, scale down
                if (drained as f64) >= 0.8 * (cur as f64) {
                    let up = ((self.current as f64) * self.cfg.step_up).round() as i64;
                    next = up.max(self.cfg.min as i64).min(self.cfg.max as i64) as u16;
                } else if (drained as f64) <= 0.2 * (cur as f64) {
                    let down = ((self.current as f64) * self.cfg.step_down).round() as i64;
                    next = down.max(self.cfg.min as i64).min(self.cfg.max as i64) as u16;
                }
            } else {
                // Latency target: conservative up, quicker down
                if (drained as f64) >= 0.6 * (cur as f64) {
                    let up = ((self.current as f64) * self.cfg.step_up.max(1.2)).round() as i64;
                    next = up.max(self.cfg.min as i64).min(self.cfg.max as i64) as u16;
                } else if (drained as f64) <= 0.4 * (cur as f64) {
                    let down = ((self.current as f64) * self.cfg.step_down).round() as i64;
                    next = down.max(self.cfg.min as i64).min(self.cfg.max as i64) as u16;
                }
            }

            if next != self.current {
                apply_qos(next)?;
                self.current = next;
            }

            self.acc_drained = 0;
            self.last_adjust = Instant::now();
        }
        Ok(())
    }
}
