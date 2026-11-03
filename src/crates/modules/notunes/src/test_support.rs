//! Shared fakes for the noTunes runtime suites.
//!
//! Nothing here observes, terminates or opens a real application: the module
//! runs over [`stache_macos::FakeApplicationBackend`], and every replacement
//! path and identifier is registered in that fake, so tests never depend on
//! installed players.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use stache_sdk::{
    EventBus, LogLevel, LogRecord, Logger, MainThreadExecutor, Permission, PermissionError,
    PermissionProvider, PermissionStatus, Registry, ThreadError,
};

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A logger that keeps every record in order.
#[derive(Default)]
pub struct CaptureLogger {
    records: Mutex<Vec<LogRecord>>,
}

impl Logger for CaptureLogger {
    fn log(&self, record: LogRecord) { lock(&self.records).push(record); }
}

impl CaptureLogger {
    pub fn records(&self) -> Vec<LogRecord> { lock(&self.records).clone() }

    pub fn messages(&self) -> Vec<String> {
        self.records().into_iter().map(|record| record.message).collect()
    }

    pub fn warnings(&self) -> Vec<String> {
        self.records()
            .into_iter()
            .filter(|record| record.level == LogLevel::Warn)
            .map(|record| record.message)
            .collect()
    }
}

pub struct AllowAll;

impl PermissionProvider for AllowAll {
    fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

    fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(PermissionStatus::Granted)
    }
}

/// A registry executor that never claims to be the main thread.
pub struct NoMainThread;

impl MainThreadExecutor for NoMainThread {
    fn is_main_thread(&self) -> bool { false }

    fn execute(&self, _: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> { Ok(()) }
}

/// A registry with permissive host services and no windows.
#[must_use]
pub fn registry(logger: Arc<dyn Logger>) -> Registry {
    Registry::new(
        logger,
        Arc::new(EventBus::new()),
        Arc::new(NoMainThread),
        Arc::new(AllowAll),
        Arc::new(stache_sdk::UnavailableWindows),
    )
}
