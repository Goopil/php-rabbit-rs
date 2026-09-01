//! Embedder-side log sink: routes core diagnostics to stderr.
//!
//! PHP functions cannot be called from the Tokio runtime threads that emit
//! core diagnostics, so instead of bridging to `error_log()` the sink writes
//! to stderr (visible in CLI output and captured by FPM with
//! `catch_workers_output`).
//!
//! The sink is installed once at module init and filters on the
//! `RABBIT_RS_LOG` environment variable: `info`, `warn` (or `warning`), or
//! `error` enable records at that severity and above. Unset or unrecognized
//! values keep the extension silent (the core's default sink is silent too).

use std::sync::Arc;

use rabbit_rs_core::log::{self, Level, Record, Sink};

/// Installs the stderr sink at module init. The first installation wins, so
/// calling this more than once (or after an embedder installed its own sink)
/// never overrides an existing sink.
pub(crate) fn install_from_env() {
    log::install(Arc::new(StderrSink {
        min_level: level_from_env(),
    }));
}

/// Writes matching records to stderr, one line per record.
///
/// The line prefix (`rabbit-rs.<target>`) lets operators filter these
/// records out of a mixed error stream.
pub(crate) struct StderrSink {
    min_level: Option<Level>,
}

impl Sink for StderrSink {
    fn log(&self, record: Record<'_>) {
        if emits(self.min_level, record.level) {
            eprintln!(
                "[rabbit-rs.{}] {}: {}",
                record.target,
                record.level.as_str(),
                record.message
            );
        }
    }
}

/// Whether a record at `record_level` passes a sink configured with
/// `min_level` (no level configured means never emit).
fn emits(min_level: Option<Level>, record_level: Level) -> bool {
    min_level.is_some_and(|min| record_level >= min)
}

/// Parses the severity threshold from the `RABBIT_RS_LOG` environment
/// variable.
fn level_from_env() -> Option<Level> {
    match std::env::var("RABBIT_RS_LOG") {
        Ok(value) => level_from_str(&value),
        Err(_) => None,
    }
}

fn level_from_str(value: &str) -> Option<Level> {
    match value.trim().to_ascii_lowercase().as_str() {
        "info" => Some(Level::Info),
        "warn" | "warning" => Some(Level::Warn),
        "error" => Some(Level::Error),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_threshold_parses_severities() {
        assert_eq!(level_from_str("info"), Some(Level::Info));
        assert_eq!(level_from_str("WARN"), Some(Level::Warn));
        assert_eq!(level_from_str(" warning "), Some(Level::Warn));
        assert_eq!(level_from_str("error"), Some(Level::Error));
        assert_eq!(level_from_str("debug"), None);
        assert_eq!(level_from_str(""), None);
        assert_eq!(level_from_str("off"), None);
    }

    #[test]
    fn emission_filters_on_the_threshold() {
        assert!(!emits(None, Level::Error), "unset threshold emits nothing");
        assert!(emits(Some(Level::Error), Level::Error));
        assert!(!emits(Some(Level::Error), Level::Warn));
        assert!(emits(Some(Level::Warn), Level::Error));
        assert!(emits(Some(Level::Info), Level::Info));
        assert!(!emits(Some(Level::Warn), Level::Info));
    }
}
