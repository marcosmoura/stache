//! Timestamped debug evidence for the native borders.
//!
//! The lines carry the same `[t+<ms>ms]` stamp as the rest of Tiling (one shared
//! [`LogOrigin`]) and go through the module's injected logger, so they sort with
//! the actor and visibility lines. A [`LogSlot`] is empty until the runtime
//! start sets it; an empty slot never runs the closure that builds the line, so
//! an unlogged run formats nothing. Never call it from a native callback: only
//! where facts are drained on the actor or an owner's own thread.

use std::sync::OnceLock;

use stache_sdk::ScopedLogger;

use crate::logclock::LogOrigin;

struct Sink {
    logger: ScopedLogger,
    origin: LogOrigin,
}

/// Where the borders write their debug lines; set once per run.
#[derive(Default)]
pub struct LogSlot(OnceLock<Sink>);

impl LogSlot {
    /// An empty (silent) slot.
    #[must_use]
    pub const fn new() -> Self { Self(OnceLock::new()) }

    /// Sets the logger and the shared clock (the first call wins; `None` keeps it silent).
    pub fn set(&self, logger: Option<ScopedLogger>, origin: LogOrigin) {
        if let Some(logger) = logger {
            let _ = self.0.set(Sink { logger, origin });
        }
    }

    /// Whether lines are written.
    #[must_use]
    pub fn enabled(&self) -> bool { self.0.get().is_some() }

    /// `[t+..ms] tiling borders: <line>` at debug level; `line` runs only when set.
    pub fn debug(&self, line: impl FnOnce() -> String) {
        if let Some(sink) = self.0.get() {
            sink.logger.debug(format!("{} tiling borders: {}", sink.origin.stamp(), line()));
        }
    }

    /// The same line at info level.
    pub fn info(&self, line: impl FnOnce() -> String) {
        if let Some(sink) = self.0.get() {
            sink.logger.info(format!("{} tiling borders: {}", sink.origin.stamp(), line()));
        }
    }
}
