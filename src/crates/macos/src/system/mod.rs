//! Read-only system readings for the Bar providers: CPU usage, the SMC CPU
//! temperature, the battery and the Wi-Fi interfaces.
//!
//! Every adapter is a small object-safe trait with a production
//! implementation and a scripted fake under the `testing` feature. A read is
//! **blocking** and may be slow; callers run it on their own worker thread and
//! never on the main thread. None of these adapters mutates anything, spawns a
//! process, prompts for a permission or touches the network.
//!
//! * [`CpuUsageBackend`]: `sysinfo` global CPU usage (the legacy source).
//! * [`SmcBackend`]: the CPU temperature from the SMC, `smctemp` style, through
//!   a small own adapter over public `IOKit` (see [`smc`]). No subprocess.
//! * [`BatteryBackend`]: `starship-battery` (legacy source).
//! * [`WifiBackend`]: `CoreWLAN` interface names, power and RSSI only. See
//!   [`wifi`] for the Location Services evidence.
//! * [`WifiEventMonitor`]: `CoreWLAN` event monitoring as a change signal for
//!   the Wi-Fi provider; see [`wifi_events`] for its Location evidence.

use std::fmt;

pub mod battery;
pub mod cpu;
pub mod smc;
mod smc_io;
pub mod wifi;
pub mod wifi_events;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use battery::{BatteryBackend, BatteryPowerState, BatteryReadout, SystemBattery};
pub use cpu::{CpuUsageBackend, SystemCpuUsage};
pub use smc::{
    ChipFamily, KeyPlan, SmcBackend, SmcConnection, SmcCpuTemperature, SmcKey, SmcOpener,
    SmcPolicy, SmcRaw, decode,
};
pub use wifi::{
    InterfaceName, InterfaceNameError, SystemWifi, WifiBackend, WifiInterfaceReading, WifiPower,
};
pub use wifi_events::{WifiEventMonitor, WifiSignalKind, WifiSignalSink};

/// Why a system read failed. The text is for diagnostics only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemReadError(String);

impl SystemReadError {
    /// A failure with `message`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self { Self(message.into()) }
}

impl fmt::Display for SystemReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

impl std::error::Error for SystemReadError {}
