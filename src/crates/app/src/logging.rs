//! Host logging to standard error.
//!
//! The logger is injected into the registry; there is no process-wide logging
//! facade. The shared [`LogFilter`] is updated by accepted `global.toml`
//! reloads, so a level edit takes effect without restarting the host.

use std::io::Write;
use std::sync::Arc;

use stache_sdk::{LogFilter, LogRecord, Logger};

/// Writes filtered records to standard error.
#[derive(Debug, Clone)]
pub struct StderrLogger {
    filter: Arc<LogFilter>,
}

impl StderrLogger {
    /// Creates a logger using the shared live filter.
    #[must_use]
    pub const fn new(filter: Arc<LogFilter>) -> Self { Self { filter } }
}

impl Logger for StderrLogger {
    fn log(&self, record: LogRecord) {
        if !self.filter.allows(record.level) {
            return;
        }
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(
            stderr,
            "stache: {} {}.{}: {}",
            record.level, record.module, record.operation, record.message
        );
    }
}
