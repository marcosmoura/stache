//! CPU usage and temperature (legacy `cpu.rs`).
//!
//! * **Usage** comes from `sysinfo` `refresh_cpu_usage` / `global_cpu_usage`,
//!   the legacy source (`host_processor_info` underneath): one aggregate
//!   percentage over the interval since the previous read. The priming read
//!   has no interval, so it is discarded and the first sample waits
//!   [`CPU_WARMUP`](super::CPU_WARMUP).
//! * **Temperature** is the native SMC average from
//!   [`stache_macos::system::SmcBackend`] (`smctemp` style: per-chip key lists,
//!   bounded retry, bounded last-valid; see that module). No `ismc`/`smctemp`
//!   subprocess and no other tool: when the SMC cannot supply a valid value
//!   the temperature is unknown, never fabricated.
//!
//! # Differences from legacy
//!
//! * Every temperature must be finite and `0 < t < 150`.
//! * Legacy fell back to `ismc` and `smctemp` subprocesses; the approved
//!   decision (2026-11-10) removed them.

use std::sync::Arc;
use std::time::Duration;

pub use stache_macos::system::smc::{TEMP_MAX, TEMP_MIN, is_valid_temp};
use stache_macos::system::{CpuUsageBackend, SmcBackend};
use stache_sdk::CancellationToken;

use super::{CpuReading, ProviderKind, ProviderParts, ProviderUpdate, Sampled, Sampler};

/// The CPU provider.
pub struct CpuSampler {
    usage: Arc<dyn CpuUsageBackend>,
    smc: Arc<dyn SmcBackend>,
    period: Duration,
    warmup: Duration,
}

impl CpuSampler {
    /// A sampler over `parts`.
    #[must_use]
    pub fn new(parts: &ProviderParts) -> Self {
        Self {
            usage: Arc::clone(&parts.cpu),
            smc: Arc::clone(&parts.smc),
            period: parts.timing.cpu,
            warmup: parts.timing.cpu_warmup,
        }
    }
}

/// Rounds a usage percentage to 0 to 100; `None` when it is not finite.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=100
pub fn round_usage(usage: f32) -> Option<u8> {
    usage.is_finite().then(|| usage.round().clamp(0.0, 100.0) as u8)
}

/// Rounds a valid temperature to whole degrees.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0 < t < 150
pub fn round_temperature(temp: f64) -> Option<u8> {
    is_valid_temp(temp).then(|| (temp as f32).round().clamp(0.0, 255.0) as u8)
}

impl Sampler for CpuSampler {
    fn kind(&self) -> ProviderKind { ProviderKind::Cpu }

    fn prime(&mut self) {
        // The first measurement has no interval to measure; discard it.
        let _ = self.usage.usage();
    }

    fn first_delay(&self) -> Duration { self.warmup }

    fn sample(&mut self, cancel: &CancellationToken) -> Sampled {
        let usage = self.usage.usage().ok().and_then(round_usage);
        let Some(usage) = usage else {
            return Sampled::Done {
                update: None,
                diagnostic: Some("the CPU usage could not be read".to_owned()),
            };
        };
        if cancel.is_cancelled() {
            return Sampled::Cancelled;
        }
        let (temperature, diagnostic) = self.smc.cpu_temperature().map_or_else(
            |_| (None, Some("the CPU temperature is unavailable".to_owned())),
            |temp| (temp.and_then(round_temperature), None),
        );
        Sampled::Done {
            update: Some(ProviderUpdate::Cpu(CpuReading { usage, temperature })),
            diagnostic,
        }
    }

    fn period(&self) -> Duration { self.period }
}
