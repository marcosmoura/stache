use serde::Serialize;
use starship_battery::units::ratio::percent;
use starship_battery::{Battery, Manager, State};

#[derive(Debug, Clone, Serialize)]
pub struct BatteryInfo {
    pub percentage: u8,
    pub state: BatteryState,
}

#[derive(Debug, Clone, Serialize)]
pub enum BatteryState {
    Unknown,
    Charging,
    Discharging,
    Empty,
    Full,
}

impl From<State> for BatteryState {
    fn from(state: State) -> Self {
        match state {
            State::Charging => Self::Charging,
            State::Discharging => Self::Discharging,
            State::Empty => Self::Empty,
            State::Full => Self::Full,
            State::Unknown => Self::Unknown,
        }
    }
}

impl From<Battery> for BatteryInfo {
    fn from(battery: Battery) -> Self {
        Self {
            percentage: percentage_from_ratio(battery.state_of_charge().get::<percent>()),
            state: battery.state().into(),
        }
    }
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_battery_info(app_handle: tauri::AppHandle) -> Result<BatteryInfo, String> {
    let _ = app_handle; // App handle kept for future event wiring

    let manager = Manager::new().map_err(stringify_error)?;
    let mut batteries = manager.batteries().map_err(stringify_error)?;

    let battery = batteries
        .next()
        .ok_or_else(|| "No battery detected on this system".to_string())?
        .map_err(stringify_error)?;

    Ok(BatteryInfo::from(battery))
}

fn stringify_error(err: impl std::fmt::Display) -> String { err.to_string() }

// Value is clamped to 0..=100, so casting is safe for pedantic clippy settings.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
const fn percentage_from_ratio(value: f32) -> u8 { value.round().clamp(0.0, 100.0) as u8 }

#[cfg(test)]
mod tests {
    use starship_battery::State;

    use super::*;

    #[test]
    fn percentage_from_ratio_clamps_and_rounds() {
        assert_eq!(percentage_from_ratio(-10.0), 0);
        assert_eq!(percentage_from_ratio(0.0), 0);
        assert_eq!(percentage_from_ratio(49.4), 49);
        assert_eq!(percentage_from_ratio(49.5), 50);
        assert_eq!(percentage_from_ratio(150.0), 100);
    }

    #[test]
    fn battery_state_from_state_matches_variants() {
        assert!(matches!(
            BatteryState::from(State::Charging),
            BatteryState::Charging
        ));
        assert!(matches!(
            BatteryState::from(State::Discharging),
            BatteryState::Discharging
        ));
        assert!(matches!(BatteryState::from(State::Empty), BatteryState::Empty));
        assert!(matches!(BatteryState::from(State::Full), BatteryState::Full));
        assert!(matches!(
            BatteryState::from(State::Unknown),
            BatteryState::Unknown
        ));
    }

    #[test]
    fn stringify_error_returns_display_message() {
        use std::io::Error;

        let err = Error::other("boom");
        assert_eq!(stringify_error(err), "boom");
    }
}
