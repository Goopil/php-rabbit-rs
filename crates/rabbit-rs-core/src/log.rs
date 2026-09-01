//! Pluggable diagnostics facade for the core.
//!
//! The core is runtime-independent: it never writes to stderr and depends on
//! no logging framework. Instead, every diagnostic event flows through this
//! module, and the embedder installs a [`Sink`] to receive records. Without
//! an installed sink the core is silent, so an embedder that installs
//! nothing observes no behavior change.
//!
//! Install the sink at startup, before spawning pools: records emitted
//! before the first [`install`] are dropped.
//!
//! # Redaction contract
//!
//! Core call sites only ever log broker names, connection generations, and
//! transport error messages — never credentials, complete broker URIs, or
//! private certificate material. Sink implementations must preserve that
//! contract when forwarding records.
//!
//! # Implementation contract
//!
//! Records are emitted from Tokio runtime worker threads: implementations
//! must be thread-safe, must never panic, must not block, and must not call
//! back into the core.

use std::{
    fmt,
    sync::{Arc, OnceLock},
};

/// Severity of a diagnostic record, ordered from least to most severe so
/// sinks can filter with `record.level >= Level::Warn`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum Level {
    /// Normal lifecycle event worth surfacing in debug mode.
    Info,
    /// Degradation the core recovers from on its own (retryable connect
    /// failure, failed recovery generation that will be retried).
    Warn,
    /// Condition the embedder must act on (permanent connection failure).
    Error,
}

impl Level {
    /// Lowercase name of the level, stable across releases.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// One diagnostic record emitted by the core.
///
/// The message is already redacted; see the module documentation for the
/// redaction contract.
#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
    /// Severity of the event.
    pub level: Level,
    /// Logical emitter inside the core (for example `connection_actor`).
    pub target: &'static str,
    /// Redacted, single-line description of the event.
    pub message: &'a str,
}

/// Receives diagnostic records emitted by the core.
pub trait Sink: Send + Sync {
    /// Handles one record. Must not panic, block, or call back into the core.
    fn log(&self, record: Record<'_>);
}

static SINK: OnceLock<Arc<dyn Sink>> = OnceLock::new();

/// Installs the process-wide sink. The first installation wins; later calls
/// are ignored and return `false`, which keeps repeated initializations and
/// forked children deterministic.
///
/// Returns `true` when this call installed the sink.
pub fn install(sink: Arc<dyn Sink>) -> bool {
    SINK.set(sink).is_ok()
}

/// Emits a record at [`Level::Error`].
pub fn error(target: &'static str, message: impl fmt::Display) {
    emit(Level::Error, target, message);
}

/// Emits a record at [`Level::Warn`].
pub fn warn(target: &'static str, message: impl fmt::Display) {
    emit(Level::Warn, target, message);
}

/// Emits a record at [`Level::Info`].
pub fn info(target: &'static str, message: impl fmt::Display) {
    emit(Level::Info, target, message);
}

/// Formats and emits one record through the installed sink.
///
/// Records emitted before the first [`install`] are dropped. Call sites are
/// lifecycle events, never the hot path, so the one allocation for the
/// formatted message is acceptable.
fn emit(level: Level, target: &'static str, message: impl fmt::Display) {
    let Some(sink) = SINK.get() else {
        return;
    };
    let message = message.to_string();
    sink.log(Record {
        level,
        target,
        message: &message,
    });
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct CountingSink(AtomicUsize);

    impl Sink for CountingSink {
        fn log(&self, _record: Record<'_>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn emit_without_install_is_silent_and_never_panics() {
        emit(Level::Error, "test", "dropped before any install");
    }

    #[test]
    fn installed_sink_receives_records_exactly_once() {
        let sink = Arc::new(CountingSink::default());
        assert!(install(sink.clone()), "the first install must win");
        emit(Level::Warn, "test", "counted");
        assert_eq!(sink.0.load(Ordering::Relaxed), 1);
    }
}
