//! Wi-Fi (legacy `wifi.rs` and `Wifi.state.ts`).
//!
//! # Sources
//!
//! 1. **Native, prompt-free** (`stache_macos::system::WifiBackend`): the
//!    `CoreWLAN` interface names, power and RSSI. See that module for the
//!    Location Services evidence: none of those three reads is Location-gated,
//!    and nothing here requests Location or uses a private Apple80211 API.
//! 2. **SSID**: only the audited `ipconfig getsummary <interface>`
//!    compatibility path can supply the network name, and only while the
//!    [`SsidGate`] is on. The gate is **on by default**: the user-approved
//!    macOS 26 feasibility run (2026-11-10) showed the name with no Location
//!    or other permission prompt. `STACHE_BAR_WIFI_SSID_SUMMARY=0` is a
//!    diagnostic off-switch; with the gate off no `ipconfig` child is ever
//!    spawned and the name is `None`.
//!
//! # Change signals and `ipconfig` cadence
//!
//! With [`CoreWlanSignals`] installed (`CoreWLAN` event monitoring, see
//! `stache_macos::system::wifi_events`), the sampler keeps the last `ipconfig`
//! summary of each interface and runs `ipconfig` only when there is no
//! summary yet, after an identity signal (power, SSID, link, or the Wi-Fi
//! daemon connection was interrupted or invalidated), when the native power or
//! association state differs from the one the summary was taken under, and on
//! the periodic safety sample. Otherwise it publishes the native reading with
//! the kept name (the native RSSI first, as it is the fresher value). When the
//! source cannot be installed, or is lost (its connection to the Wi-Fi daemon
//! was invalidated), every sample runs `ipconfig`, as without signals, and the
//! condition is reported.
//!
//! # Status
//!
//! The status keeps the legacy meaning of `Unknown` (nothing known; the
//! component is hidden), `Off`, `Disconnected` and `Connected`, with the legacy
//! precedence: a summary naming an SSID wins; otherwise any interface with the
//! radio on is connected when it reports an RSSI and disconnected when it does
//! not; otherwise any radio off is `Off`; interfaces with an unreadable power
//! state are `Disconnected`; no interface at all is `Unknown`. A missing or
//! redacted SSID never implies disconnected.
//!
//! # Differences from legacy
//!
//! Interface discovery uses the `CoreWLAN` names instead of
//! `networksetup -listallhardwareports` / `ifconfig -l` / `networksetup
//! -getairportpower`, which removes three child processes per sample. The RSSI
//! is read off the main thread (legacy hopped to it).
//!
//! # Untrusted output
//!
//! `ipconfig` output is bounded by the runner, the interface argument is a
//! validated [`InterfaceName`], and the SSID is stripped of quotes and control
//! characters, bounded to [`MAX_SSID_BYTES`] and discarded when it is the
//! `<redacted>` placeholder.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_macos::system::{
    InterfaceName, WifiBackend, WifiEventMonitor, WifiInterfaceReading, WifiPower, WifiSignalSink,
};
use stache_sdk::CancellationToken;
use stache_sdk::commands::{CaptureLimits, CaptureOutcome, CommandExecutor, PreparedCommand};

use super::{
    ProviderKind, ProviderParts, ProviderUpdate, Sampled, Sampler, Trigger, WIFI_SAFETY_PERIOD,
    WifiReading,
};

/// The diagnostic off-switch of the SSID summary path. Only the exact value `0`
/// turns it off; any other value, or none, leaves it on.
pub const SSID_GATE_ENV: &str = "STACHE_BAR_WIFI_SSID_SUMMARY";

/// The longest SSID kept, in bytes.
pub const MAX_SSID_BYTES: usize = 64;

/// The status, spelled as legacy `WifiStatus` (the UI compares these strings).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum WifiStatus {
    /// Nothing is known; the component is hidden.
    Unknown,
    /// The radio is off.
    Off,
    /// The radio is on without a network.
    Disconnected,
    /// Associated with a network.
    Connected,
}

/// Whether the `ipconfig getsummary` SSID path may run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SsidGate {
    /// The path never runs. Diagnostic only (`STACHE_BAR_WIFI_SSID_SUMMARY=0`).
    Off,
    /// The path runs. The default.
    #[default]
    On,
}

impl SsidGate {
    /// The gate for an environment value: off only for exactly `0`.
    #[must_use]
    pub fn from_value(value: Option<&str>) -> Self {
        if value == Some("0") {
            Self::Off
        } else {
            Self::On
        }
    }

    /// Reads [`SSID_GATE_ENV`] from the process environment.
    #[must_use]
    pub fn from_environment() -> Self {
        Self::from_value(std::env::var(SSID_GATE_ENV).ok().as_deref())
    }
}

/// Native Wi-Fi change signals. A signal only wakes the schedule; the sample
/// itself runs on the provider worker.
pub trait WifiSignals: Send + Sync {
    /// Subscribes `notify` (cheap and non-blocking) until the returned guard is
    /// dropped. Each call carries the signal's kind.
    ///
    /// # Errors
    /// Why the subscription failed; the schedule then polls as without signals.
    fn watch(&self, notify: WifiSignalSink) -> Result<Box<dyn Send>, String>;
}

/// The production signal source: `CoreWLAN` event monitoring on a client
/// Stache owns.
#[derive(Clone, Copy, Debug, Default)]
pub struct CoreWlanSignals;

impl WifiSignals for CoreWlanSignals {
    fn watch(&self, notify: WifiSignalSink) -> Result<Box<dyn Send>, String> {
        WifiEventMonitor::start(notify)
            .map(|monitor| Box::new(monitor) as Box<dyn Send>)
            .map_err(|error| error.to_string())
    }
}

/// The status line while the change signal is unavailable.
pub const SIGNALS_UNAVAILABLE: &str =
    "Wi-Fi change notifications are unavailable, so Wi-Fi is checked every 5 seconds";

/// Whether the change signal is in use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SignalState {
    /// No source is configured: poll, silently.
    #[default]
    None,
    /// The source is installed: sample on signals and every safety period.
    Installed,
    /// The source could not be installed, or was lost: poll and report it.
    Unavailable,
}

/// One kept `ipconfig` summary and the native state it was taken under.
struct Kept {
    name: InterfaceName,
    power: WifiPower,
    associated: bool,
    summary: IpconfigSummary,
}

/// What `ipconfig getsummary` said about one interface.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IpconfigSummary {
    /// The interface type is Wi-Fi.
    pub is_wifi: bool,
    /// The sanitized network name.
    pub ssid: Option<String>,
    /// The RSSI in dBm.
    pub signal_strength: Option<i32>,
}

fn key_value(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.trim().split_once(':')?;
    Some((key.trim(), value.trim()))
}

/// Sanitizes an SSID from untrusted output: strips quotes and control
/// characters, bounds it and drops the redaction placeholder.
#[must_use]
pub fn sanitize_ssid(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches('"').trim();
    let mut clean: String = trimmed.chars().filter(|ch| !ch.is_control()).collect();
    if clean.len() > MAX_SSID_BYTES {
        let mut end = MAX_SSID_BYTES;
        while !clean.is_char_boundary(end) {
            end -= 1;
        }
        clean.truncate(end);
    }
    let clean = clean.trim().to_owned();
    if clean.is_empty() || clean.eq_ignore_ascii_case("<redacted>") {
        None
    } else {
        Some(clean)
    }
}

/// An RSSI in dBm from `ipconfig` text: the first integer token in the
/// plausible range `-127..=-1`.
#[must_use]
pub fn parse_rssi(value: &str) -> Option<i32> {
    value
        .split(|ch: char| !(ch == '-' || ch.is_ascii_digit()))
        .filter(|part| !part.is_empty() && *part != "-")
        .find_map(|part| part.parse::<i32>().ok())
        .filter(|rssi| (-127..=-1).contains(rssi))
}

/// Parses `ipconfig getsummary` output (legacy `parse_ipconfig_summary`).
#[must_use]
pub fn parse_summary(output: &str) -> IpconfigSummary {
    let mut summary = IpconfigSummary::default();
    for line in output.lines().take(512) {
        let Some((key, value)) = key_value(line) else {
            continue;
        };
        if key.eq_ignore_ascii_case("InterfaceType") {
            summary.is_wifi = value.eq_ignore_ascii_case("WiFi")
                || value.eq_ignore_ascii_case("AirPort")
                || value.eq_ignore_ascii_case("IEEE80211");
        } else if key.eq_ignore_ascii_case("SSID") || key.eq_ignore_ascii_case("SSID_STR") {
            summary.ssid = sanitize_ssid(value);
        } else if key.to_ascii_lowercase().contains("rssi")
            && let Some(rssi) = parse_rssi(value)
        {
            summary.signal_strength = Some(rssi);
        }
    }
    if summary.ssid.is_some() {
        summary.is_wifi = true;
    }
    summary
}

/// The status and signal from the native readings alone (no SSID).
#[must_use]
pub fn native_reading(interfaces: &[WifiInterfaceReading]) -> WifiReading {
    if interfaces.is_empty() {
        return WifiReading::unknown();
    }
    if let Some(connected) = interfaces
        .iter()
        .find(|interface| interface.power == WifiPower::On && interface.rssi.is_some())
    {
        return WifiReading {
            status: WifiStatus::Connected,
            network_name: None,
            signal_strength: connected.rssi,
        };
    }
    let status = if interfaces.iter().any(|interface| interface.power == WifiPower::On) {
        WifiStatus::Disconnected
    } else if interfaces.iter().any(|interface| interface.power == WifiPower::Off) {
        WifiStatus::Off
    } else {
        WifiStatus::Disconnected
    };
    WifiReading {
        status,
        network_name: None,
        signal_strength: None,
    }
}

/// Runs `ipconfig getsummary <interface>`.
pub enum SummaryOutcome {
    /// The parsed summary.
    Summary(IpconfigSummary),
    /// The call failed; no summary.
    Failed(String),
    /// Cancellation stopped the call.
    Cancelled,
}

fn run_summary(
    executor: &CommandExecutor,
    ipconfig: &Path,
    interface: &InterfaceName,
    limits: CaptureLimits,
    cancel: &CancellationToken,
) -> SummaryOutcome {
    let prepared = PreparedCommand {
        program: ipconfig.to_path_buf(),
        arguments: vec!["getsummary".to_owned(), interface.as_str().to_owned()],
    };
    match executor.run_captured_prepared(&prepared, limits, cancel, cancel) {
        CaptureOutcome::Completed(output) if output.status.success() => {
            SummaryOutcome::Summary(parse_summary(&String::from_utf8_lossy(&output.stdout)))
        }
        CaptureOutcome::Completed(_) => {
            SummaryOutcome::Failed("ipconfig exited with a failure".into())
        }
        CaptureOutcome::Overflow { .. } => {
            SummaryOutcome::Failed("ipconfig printed too much".into())
        }
        CaptureOutcome::TimedOut { .. } => SummaryOutcome::Failed("ipconfig timed out".into()),
        CaptureOutcome::Cancelled(_) => SummaryOutcome::Cancelled,
        CaptureOutcome::Failed { .. } => SummaryOutcome::Failed("ipconfig could not run".into()),
    }
}

/// The Wi-Fi provider.
pub struct WifiSampler {
    backend: Arc<dyn WifiBackend>,
    executor: Arc<CommandExecutor>,
    ipconfig: PathBuf,
    gate: SsidGate,
    limits: CaptureLimits,
    period: Duration,
    signals: SignalState,
    kept: Vec<Kept>,
}

impl WifiSampler {
    /// A sampler over `parts`.
    #[must_use]
    pub fn new(parts: &ProviderParts) -> Self {
        Self {
            backend: Arc::clone(&parts.wifi),
            executor: Arc::clone(&parts.executor),
            ipconfig: parts.ipconfig.clone(),
            gate: parts.ssid,
            limits: parts.limits(),
            period: parts.timing.wifi,
            signals: SignalState::None,
            kept: Vec::new(),
        }
    }

    /// Records whether the change signal is in use.
    pub const fn set_signals(&mut self, signals: SignalState) { self.signals = signals; }

    fn diagnostic(&self, mut notes: Vec<String>) -> Option<String> {
        if self.signals == SignalState::Unavailable {
            notes.push(SIGNALS_UNAVAILABLE.to_owned());
        }
        (!notes.is_empty()).then(|| notes.join("; "))
    }

    /// Today's result from the kept summaries, without `ipconfig`: `None` when
    /// an interface has no summary taken under its current native state.
    fn reading_from_kept(&self, interfaces: &[WifiInterfaceReading]) -> Option<WifiReading> {
        for interface in interfaces {
            let kept = self.kept.iter().find(|kept| {
                kept.name == interface.name
                    && kept.power == interface.power
                    && kept.associated == interface.rssi.is_some()
            })?;
            if kept.summary.is_wifi && kept.summary.ssid.is_some() {
                return Some(WifiReading {
                    status: WifiStatus::Connected,
                    network_name: kept.summary.ssid.clone(),
                    signal_strength: interface.rssi.or(kept.summary.signal_strength),
                });
            }
        }
        Some(native_reading(interfaces))
    }
}

impl Sampler for WifiSampler {
    fn kind(&self) -> ProviderKind { ProviderKind::Wifi }

    fn sample(&mut self, cancel: &CancellationToken) -> Sampled {
        self.sample_for(Trigger::periodic(), cancel)
    }

    fn sample_for(&mut self, trigger: Trigger, cancel: &CancellationToken) -> Sampled {
        // A lost source ends the signal mode: poll as without signals from now.
        if trigger.lost && self.signals == SignalState::Installed {
            self.signals = SignalState::Unavailable;
        }
        let Ok(interfaces) = self.backend.interfaces() else {
            return Sampled::Done {
                update: Some(ProviderUpdate::Wifi(WifiReading::unknown())),
                diagnostic: self
                    .diagnostic(vec!["the Wi-Fi interfaces could not be read".to_owned()]),
            };
        };
        let mut notes: Vec<String> = Vec::new();
        if self.gate == SsidGate::On {
            let refresh =
                self.signals != SignalState::Installed || trigger.periodic || trigger.identity;
            if !refresh && let Some(reading) = self.reading_from_kept(&interfaces) {
                return Sampled::Done {
                    update: Some(ProviderUpdate::Wifi(reading)),
                    diagnostic: self.diagnostic(notes),
                };
            }
            self.kept.clear();
            for interface in &interfaces {
                match run_summary(
                    &self.executor,
                    &self.ipconfig,
                    &interface.name,
                    self.limits,
                    cancel,
                ) {
                    SummaryOutcome::Cancelled => return Sampled::Cancelled,
                    SummaryOutcome::Failed(reason) => notes.push(reason),
                    SummaryOutcome::Summary(summary) => {
                        let hit = summary.is_wifi && summary.ssid.is_some();
                        let reading = hit.then(|| WifiReading {
                            status: WifiStatus::Connected,
                            network_name: summary.ssid.clone(),
                            signal_strength: summary.signal_strength.or(interface.rssi),
                        });
                        self.kept.push(Kept {
                            name: interface.name.clone(),
                            power: interface.power,
                            associated: interface.rssi.is_some(),
                            summary,
                        });
                        if let Some(reading) = reading {
                            return Sampled::Done {
                                update: Some(ProviderUpdate::Wifi(reading)),
                                diagnostic: self.diagnostic(notes),
                            };
                        }
                    }
                }
            }
        }
        Sampled::Done {
            update: Some(ProviderUpdate::Wifi(native_reading(&interfaces))),
            diagnostic: self.diagnostic(notes),
        }
    }

    fn period(&self) -> Duration {
        if self.signals == SignalState::Installed {
            WIFI_SAFETY_PERIOD
        } else {
            self.period
        }
    }
}
