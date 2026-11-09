//! Host-owned, injectable logging.
//!
//! There is no global logger and no facade crate. The registry receives one
//! [`Logger`] at construction and hands modules a [`ScopedLogger`] that already
//! knows the module and the lifecycle operation. Two registries in one process
//! can therefore use different loggers without initialization races.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::error::Operation;
use crate::module::ModuleId;

/// Severity of one log record.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    Type,
)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    /// Development detail.
    Debug,
    /// Normal lifecycle information.
    Info,
    /// A recoverable problem.
    Warn,
    /// A failure.
    Error,
}

impl LogLevel {
    const fn byte(self) -> u8 {
        match self {
            Self::Debug => 0,
            Self::Info => 1,
            Self::Warn => 2,
            Self::Error => 3,
        }
    }

    const fn from_byte(byte: u8) -> Self {
        match byte {
            0 => Self::Debug,
            2 => Self::Warn,
            3 => Self::Error,
            _ => Self::Info,
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        })
    }
}

/// One structured log entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// Severity.
    pub level: LogLevel,
    /// The module that produced the record.
    pub module: ModuleId,
    /// The lifecycle operation in progress.
    pub operation: Operation,
    /// The human-readable message.
    pub message: String,
}

/// A sink for structured records.
///
/// Implementations decide level filtering and formatting; the SDK never
/// installs a process-wide logger.
pub trait Logger: Send + Sync {
    /// Records one entry. Must not block lifecycle operations.
    fn log(&self, record: LogRecord);
}

/// A logger that discards every record, useful for tests and CLI paths.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscardLogger;

impl Logger for DiscardLogger {
    fn log(&self, _record: LogRecord) {}
}

/// A shared, atomically updated minimum log level.
///
/// The configuration store updates it when `global.toml` is accepted, and an
/// injected [`Logger`] can consult [`LogFilter::allows`] before formatting a
/// record. This keeps level changes live without a process-wide logger.
#[derive(Debug)]
pub struct LogFilter {
    level: AtomicU8,
}

impl LogFilter {
    /// Creates a filter at the given minimum level.
    #[must_use]
    pub const fn new(level: LogLevel) -> Self {
        Self {
            level: AtomicU8::new(level.byte()),
        }
    }

    /// The current minimum level.
    #[must_use]
    pub fn level(&self) -> LogLevel { LogLevel::from_byte(self.level.load(Ordering::Relaxed)) }

    /// Replaces the minimum level.
    pub fn set_level(&self, level: LogLevel) { self.level.store(level.byte(), Ordering::Relaxed); }

    /// Whether a record at `level` passes the filter.
    #[must_use]
    pub fn allows(&self, level: LogLevel) -> bool { level >= self.level() }
}

impl Default for LogFilter {
    fn default() -> Self { Self::new(LogLevel::Info) }
}

/// A logger bound to one module and lifecycle operation.
#[derive(Clone)]
pub struct ScopedLogger {
    logger: Arc<dyn Logger>,
    module: ModuleId,
    operation: Operation,
}

impl ScopedLogger {
    /// Binds a logger to a module and operation.
    #[must_use]
    pub fn new(logger: Arc<dyn Logger>, module: ModuleId, operation: Operation) -> Self {
        Self { logger, module, operation }
    }

    /// The module this scope belongs to.
    #[must_use]
    pub const fn module(&self) -> ModuleId { self.module }

    /// The operation this scope belongs to.
    #[must_use]
    pub const fn operation(&self) -> Operation { self.operation }

    /// Returns the same logger bound to another operation.
    #[must_use]
    pub fn with_operation(&self, operation: Operation) -> Self {
        Self {
            logger: Arc::clone(&self.logger),
            module: self.module,
            operation,
        }
    }

    /// Records a message at the given level.
    pub fn log(&self, level: LogLevel, message: impl Into<String>) {
        self.logger.log(LogRecord {
            level,
            module: self.module,
            operation: self.operation,
            message: message.into(),
        });
    }

    /// Records a debug message.
    pub fn debug(&self, message: impl Into<String>) { self.log(LogLevel::Debug, message); }

    /// Records an informational message.
    pub fn info(&self, message: impl Into<String>) { self.log(LogLevel::Info, message); }

    /// Records a warning.
    pub fn warn(&self, message: impl Into<String>) { self.log(LogLevel::Warn, message); }

    /// Records an error.
    pub fn error(&self, message: impl Into<String>) { self.log(LogLevel::Error, message); }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::lock;

    #[derive(Default)]
    struct Capture {
        records: Mutex<Vec<LogRecord>>,
    }

    impl Logger for Capture {
        fn log(&self, record: LogRecord) { lock(&self.records).push(record); }
    }

    #[test]
    fn scoped_records_carry_module_and_operation() {
        let capture = Arc::new(Capture::default());
        let logger =
            ScopedLogger::new(capture.clone(), ModuleId::new("proxy-audio"), Operation::Start);
        logger.info("starting");
        logger.warn("degraded");
        let records = lock(&capture.records);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].level, LogLevel::Info);
        assert_eq!(records[0].module, ModuleId::new("proxy-audio"));
        assert_eq!(records[0].operation, Operation::Start);
        assert_eq!(records[0].message, "starting");
        assert_eq!(records[1].level, LogLevel::Warn);
    }

    #[test]
    fn with_operation_keeps_the_module_and_sink() {
        let capture = Arc::new(Capture::default());
        let logger =
            ScopedLogger::new(capture.clone(), ModuleId::new("audio"), Operation::Configure);
        logger.with_operation(Operation::Stop).error("failed");
        let records = lock(&capture.records);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].operation, Operation::Stop);
        assert_eq!(records[0].module, ModuleId::new("audio"));
        assert_eq!(records[0].level, LogLevel::Error);
    }

    #[test]
    fn discard_logger_accepts_records() {
        let logger = DiscardLogger;
        logger.log(LogRecord {
            level: LogLevel::Debug,
            module: ModuleId::new("m"),
            operation: Operation::Register,
            message: "ignored".to_owned(),
        });
    }

    #[test]
    fn log_filter_honours_the_minimum_level() {
        let filter = LogFilter::new(LogLevel::Warn);
        assert!(!filter.allows(LogLevel::Debug));
        assert!(!filter.allows(LogLevel::Info));
        assert!(filter.allows(LogLevel::Warn));
        assert!(filter.allows(LogLevel::Error));
        filter.set_level(LogLevel::Debug);
        assert_eq!(filter.level(), LogLevel::Debug);
        assert!(filter.allows(LogLevel::Debug));
    }
}
