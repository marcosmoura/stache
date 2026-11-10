//! The battery, through `starship-battery` (the legacy source,
//! `battery.rs` `get_battery_info`).

use starship_battery::units::ratio::percent;
use starship_battery::{Manager, State};

use super::SystemReadError;

/// The charge state, mirroring legacy `BatteryState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatteryPowerState {
    /// The state is not known.
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

/// One battery reading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatteryReadout {
    /// The state of charge in percent; not clamped here.
    pub percent: f32,
    /// The charge state.
    pub state: BatteryPowerState,
}

/// Reads the first battery.
pub trait BatteryBackend: Send + Sync {
    /// The first battery, or `None` when the machine has none.
    ///
    /// # Errors
    /// Why the battery list or the battery could not be read.
    fn read(&self) -> Result<Option<BatteryReadout>, SystemReadError>;
}

/// The production battery source.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemBattery;

impl BatteryBackend for SystemBattery {
    fn read(&self) -> Result<Option<BatteryReadout>, SystemReadError> {
        let manager = Manager::new()
            .map_err(|error| SystemReadError::new(format!("battery manager failed: {error}")))?;
        let mut batteries = manager
            .batteries()
            .map_err(|error| SystemReadError::new(format!("battery list failed: {error}")))?;
        let Some(battery) = batteries.next() else {
            return Ok(None);
        };
        let battery = battery
            .map_err(|error| SystemReadError::new(format!("battery read failed: {error}")))?;
        let state = match battery.state() {
            State::Charging => BatteryPowerState::Charging,
            State::Discharging => BatteryPowerState::Discharging,
            State::Empty => BatteryPowerState::Empty,
            State::Full => BatteryPowerState::Full,
            State::Unknown => BatteryPowerState::Unknown,
        };
        Ok(Some(BatteryReadout {
            percent: battery.state_of_charge().get::<percent>(),
            state,
        }))
    }
}
