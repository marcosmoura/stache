//! The battery (legacy `battery.rs` and `BatteryStore.ts`).
//!
//! The reading is the percentage (rounded, clamped to 0 to 100) and the state.
//! A machine without a battery yields `None`, which hides the component. A
//! failed read keeps the last reading and records a diagnostic. The period is
//! 30 s while the last reading is charging and 120 s otherwise (legacy
//! `BatteryStore.ts`).

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_macos::system::{BatteryBackend, BatteryPowerState};
use stache_sdk::CancellationToken;

use super::{BatteryReading, ProviderKind, ProviderParts, ProviderUpdate, Sampled, Sampler};

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
        }
    }
}

impl Sampler for BatterySampler {
    fn kind(&self) -> ProviderKind { ProviderKind::Battery }

    fn sample(&mut self, _cancel: &CancellationToken) -> Sampled {
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

    fn period(&self) -> Duration {
        if self.state == BatteryChargeState::Charging {
            self.charging
        } else {
            self.idle
        }
    }
}
