//! The battery (legacy `battery.rs` and `BatteryStore.ts`).
//!
//! The reading is the percentage (rounded, clamped to 0 to 100) and the state.
//! A machine without a battery yields `None`, which hides the component. A
//! failed read keeps the last reading and records a diagnostic.
//!
//! With [`IoKitBatterySignals`] installed (the `IOKit` power-source `notify`
//! keys, see `stache_macos::system::battery_events`), a plug, unplug or
//! percentage change wakes the schedule and samples at once (bursts coalesce,
//! at least [`super::ProviderTiming::battery_min_gap`] apart), with a safety
//! sample every 120 s in every state. Without signals the period is 30 s while
//! the last reading is charging and 120 s otherwise (legacy `BatteryStore.ts`);
//! when the source could not be installed that fallback is reported.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_macos::system::{
    BatteryBackend, BatteryEventMonitor, BatteryPowerState, BatterySignalSink,
};
use stache_sdk::CancellationToken;

use super::wifi::SignalState;
use super::{BatteryReading, ProviderKind, ProviderParts, ProviderUpdate, Sampled, Sampler};

/// Native power-source change signals. A signal only wakes the schedule; the
/// sample reads the battery as usual.
pub trait BatterySignals: Send + Sync {
    /// Subscribes `notify` (cheap and non-blocking) until the returned guard is
    /// dropped.
    ///
    /// # Errors
    /// Why the subscription failed; the schedule then polls as without signals.
    fn watch(&self, notify: BatterySignalSink) -> Result<Box<dyn Send>, String>;
}

/// The production source: `IOKit` power-source notifications.
pub struct IoKitBatterySignals;

impl BatterySignals for IoKitBatterySignals {
    fn watch(&self, notify: BatterySignalSink) -> Result<Box<dyn Send>, String> {
        BatteryEventMonitor::start(notify)
            .map(|monitor| Box::new(monitor) as Box<dyn Send>)
            .map_err(|error| error.to_string())
    }
}

/// The status line while the change signal is unavailable.
pub const BATTERY_SIGNALS_UNAVAILABLE: &str = "Battery change notifications are unavailable, so \
     the battery is checked every 30 seconds while charging and every 2 minutes otherwise";

/// The charge state, spelled as legacy `BatteryState` (the UI compares these
/// exact strings).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum BatteryChargeState {
    /// Not known.
    #[default]
    Unknown,
    /// Charging.
    Charging,
    /// Discharging.
    Discharging,
    /// Empty.
    Empty,
    /// Full.
    Full,
}

impl From<BatteryPowerState> for BatteryChargeState {
    fn from(state: BatteryPowerState) -> Self {
        match state {
            BatteryPowerState::Unknown => Self::Unknown,
            BatteryPowerState::Charging => Self::Charging,
            BatteryPowerState::Discharging => Self::Discharging,
            BatteryPowerState::Empty => Self::Empty,
            BatteryPowerState::Full => Self::Full,
        }
    }
}

/// Rounds and clamps a charge percentage; `None` when it is not finite.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 0..=100
pub fn percentage(value: f32) -> Option<u8> {
    value.is_finite().then(|| value.round().clamp(0.0, 100.0) as u8)
}

/// The battery provider.
pub struct BatterySampler {
    backend: Arc<dyn BatteryBackend>,
    charging: Duration,
    idle: Duration,
    state: BatteryChargeState,
    signals: SignalState,
}

impl BatterySampler {
    /// A sampler over `parts`.
    #[must_use]
    pub fn new(parts: &ProviderParts) -> Self {
        Self {
            backend: Arc::clone(&parts.battery),
            charging: parts.timing.battery_charging,
            idle: parts.timing.battery_idle,
            state: BatteryChargeState::Unknown,
            signals: SignalState::None,
        }
    }

    /// Records whether the change signal is in use.
    pub const fn set_signals(&mut self, signals: SignalState) { self.signals = signals; }

    fn note(&self) -> Option<String> {
        (self.signals == SignalState::Unavailable).then(|| BATTERY_SIGNALS_UNAVAILABLE.to_owned())
    }

    fn with_note(&self, diagnostic: Option<String>) -> Option<String> {
        match (diagnostic, self.note()) {
            (Some(first), Some(note)) => Some(format!("{first}; {note}")),
            (first, note) => first.or(note),
        }
    }
}

impl Sampler for BatterySampler {
    fn kind(&self) -> ProviderKind { ProviderKind::Battery }

    fn sample(&mut self, _cancel: &CancellationToken) -> Sampled {
        match self.read() {
            Sampled::Done { update, diagnostic } => Sampled::Done {
                update,
                diagnostic: self.with_note(diagnostic),
            },
            Sampled::Cancelled => Sampled::Cancelled,
        }
    }

    fn period(&self) -> Duration {
        if self.signals != SignalState::Installed && self.state == BatteryChargeState::Charging {
            self.charging
        } else {
            self.idle
        }
    }
}

impl BatterySampler {
    fn read(&mut self) -> Sampled {
        match self.backend.read() {
            Ok(None) => {
                self.state = BatteryChargeState::Unknown;
                Sampled::Done {
                    update: Some(ProviderUpdate::Battery(None)),
                    diagnostic: None,
                }
            }
            Ok(Some(readout)) => match percentage(readout.percent) {
                Some(percentage) => {
                    let state = BatteryChargeState::from(readout.state);
                    self.state = state;
                    Sampled::Done {
                        update: Some(ProviderUpdate::Battery(Some(BatteryReading {
                            percentage,
                            state,
                        }))),
                        diagnostic: None,
                    }
                }
                None => Sampled::Done {
                    update: None,
                    diagnostic: Some("the battery charge was not a number".to_owned()),
                },
            },
            Err(_) => Sampled::Done {
                update: None,
                diagnostic: Some("the battery could not be read".to_owned()),
            },
        }
    }
}
