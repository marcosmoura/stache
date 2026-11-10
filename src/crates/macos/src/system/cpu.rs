//! CPU usage through `sysinfo`, the legacy source (`cpu.rs` `get_cpu_usage`).

use std::sync::{Mutex, PoisonError};

use sysinfo::System;

use super::SystemReadError;

/// Reads the global CPU usage.
pub trait CpuUsageBackend: Send + Sync {
    /// The global usage in percent since the previous call. The first call
    /// after the backend is created only primes the measurement (the OS needs
    /// two samples at least [`sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`] apart), so
    /// callers discard it.
    ///
    /// # Errors
    /// Why the usage could not be read.
    fn usage(&self) -> Result<f32, SystemReadError>;
}

/// The production CPU usage source.
pub struct SystemCpuUsage {
    system: Mutex<System>,
}

impl SystemCpuUsage {
    /// A source with a fresh measurement.
    #[must_use]
    pub fn new() -> Self {
        Self {
            system: Mutex::new(System::new()),
        }
    }
}

impl Default for SystemCpuUsage {
    fn default() -> Self { Self::new() }
}

impl CpuUsageBackend for SystemCpuUsage {
    fn usage(&self) -> Result<f32, SystemReadError> {
        let mut system = self.system.lock().unwrap_or_else(PoisonError::into_inner);
        system.refresh_cpu_usage();
        Ok(system.global_cpu_usage())
    }
}
