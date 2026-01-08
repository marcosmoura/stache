use serde::Serialize;
use starship_battery::units::electric_potential::volt;
use starship_battery::units::energy::watt_hour;
use starship_battery::units::power::watt;
use starship_battery::units::ratio::percent;
use starship_battery::units::thermodynamic_temperature::degree_celsius;
use starship_battery::units::time::second;
use starship_battery::{Battery, Manager, State, Technology};

use crate::error::StacheError;

#[derive(Debug, Clone, Default, Serialize)]
pub struct BatteryInfo {
    /// Battery charge percentage (0-100)
    pub percentage: u8,
    /// Current battery state (charging, discharging, etc.)
    pub state: BatteryState,
    /// Battery health percentage (0-100)
    pub health: u8,
    /// Battery technology type
    pub technology: BatteryTechnology,
    /// Current energy in watt-hours
    pub energy: f32,
    /// Energy when fully charged in watt-hours
    pub energy_full: f32,
    /// Design energy capacity in watt-hours
    pub energy_full_design: f32,
    /// Current power draw/charge rate in watts
    pub energy_rate: f32,
    /// Current voltage in volts
    pub voltage: f32,
    /// Battery temperature in celsius (if available)
    pub temperature: Option<f32>,
    /// Number of charge cycles (if available)
    pub cycle_count: Option<u32>,
    /// Time until fully charged in seconds (if charging)
    pub time_to_full: Option<u64>,
    /// Time until empty in seconds (if discharging)
    pub time_to_empty: Option<u64>,
    /// Battery vendor (if available)
    pub vendor: Option<String>,
    /// Battery model (if available)
    pub model: Option<String>,
    /// Battery serial number (if available)
    pub serial_number: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub enum BatteryState {
    #[default]
    Unknown,
    Charging,
    Discharging,
    Empty,
    Full,
}

#[derive(Debug, Clone, Default, Serialize)]
pub enum BatteryTechnology {
    #[default]
    Unknown,
    LithiumIon,
    LeadAcid,
    LithiumPolymer,
    NickelMetalHydride,
    NickelCadmium,
    NickelZinc,
    LithiumIronPhosphate,
    RechargeableAlkalineManganese,
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

impl From<Technology> for BatteryTechnology {
    fn from(tech: Technology) -> Self {
        match tech {
            Technology::LithiumIon => Self::LithiumIon,
            Technology::LeadAcid => Self::LeadAcid,
            Technology::LithiumPolymer => Self::LithiumPolymer,
            Technology::NickelMetalHydride => Self::NickelMetalHydride,
            Technology::NickelCadmium => Self::NickelCadmium,
            Technology::NickelZinc => Self::NickelZinc,
            Technology::LithiumIronPhosphate => Self::LithiumIronPhosphate,
            Technology::RechargeableAlkalineManganese => Self::RechargeableAlkalineManganese,
            _ => Self::Unknown,
        }
    }
}

impl From<Battery> for BatteryInfo {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn from(battery: Battery) -> Self {
        Self {
            percentage: percentage_from_ratio(battery.state_of_charge().get::<percent>()),
            state: battery.state().into(),
            health: percentage_from_ratio(battery.state_of_health().get::<percent>()),
            technology: battery.technology().into(),
            energy: battery.energy().get::<watt_hour>(),
            energy_full: battery.energy_full().get::<watt_hour>(),
            energy_full_design: battery.energy_full_design().get::<watt_hour>(),
            energy_rate: battery.energy_rate().get::<watt>(),
            voltage: battery.voltage().get::<volt>(),
            temperature: battery.temperature().map(|t| t.get::<degree_celsius>()),
            cycle_count: battery.cycle_count(),
            time_to_full: battery.time_to_full().map(|t| t.get::<second>() as u64),
            time_to_empty: battery.time_to_empty().map(|t| t.get::<second>() as u64),
            vendor: battery.vendor().map(String::from),
            model: battery.model().map(String::from),
            serial_number: battery.serial_number().map(String::from),
        }
    }
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_battery_info(app_handle: tauri::AppHandle) -> Result<BatteryInfo, StacheError> {
    let _ = app_handle; // App handle kept for future event wiring

    let manager = Manager::new()
        .map_err(|e| StacheError::BatteryError(format!("Manager init failed: {e}")))?;
    let mut batteries = manager
        .batteries()
        .map_err(|e| StacheError::BatteryError(format!("Failed to list batteries: {e}")))?;

    let battery = batteries
        .next()
        .ok_or_else(|| StacheError::BatteryError("No battery detected on this system".to_string()))?
        .map_err(|e| StacheError::BatteryError(format!("Failed to read battery: {e}")))?;

    Ok(BatteryInfo::from(battery))
}

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
    fn battery_technology_from_technology_matches_variants() {
        assert!(matches!(
            BatteryTechnology::from(Technology::LithiumIon),
            BatteryTechnology::LithiumIon
        ));
        assert!(matches!(
            BatteryTechnology::from(Technology::LeadAcid),
            BatteryTechnology::LeadAcid
        ));
        assert!(matches!(
            BatteryTechnology::from(Technology::LithiumPolymer),
            BatteryTechnology::LithiumPolymer
        ));
        assert!(matches!(
            BatteryTechnology::from(Technology::NickelMetalHydride),
            BatteryTechnology::NickelMetalHydride
        ));
        assert!(matches!(
            BatteryTechnology::from(Technology::NickelCadmium),
            BatteryTechnology::NickelCadmium
        ));
        assert!(matches!(
            BatteryTechnology::from(Technology::NickelZinc),
            BatteryTechnology::NickelZinc
        ));
        assert!(matches!(
            BatteryTechnology::from(Technology::LithiumIronPhosphate),
            BatteryTechnology::LithiumIronPhosphate
        ));
        assert!(matches!(
            BatteryTechnology::from(Technology::RechargeableAlkalineManganese),
            BatteryTechnology::RechargeableAlkalineManganese
        ));
        assert!(matches!(
            BatteryTechnology::from(Technology::Unknown),
            BatteryTechnology::Unknown
        ));
    }

    #[test]
    fn battery_state_default_is_unknown() {
        assert!(matches!(BatteryState::default(), BatteryState::Unknown));
    }

    #[test]
    fn battery_technology_default_is_unknown() {
        assert!(matches!(
            BatteryTechnology::default(),
            BatteryTechnology::Unknown
        ));
    }

    #[test]
    fn battery_info_default() {
        let info = BatteryInfo::default();
        assert_eq!(info.percentage, 0);
        assert!(matches!(info.state, BatteryState::Unknown));
        assert_eq!(info.health, 0);
        assert!(matches!(info.technology, BatteryTechnology::Unknown));
        assert_eq!(info.energy, 0.0);
        assert_eq!(info.energy_full, 0.0);
        assert_eq!(info.energy_full_design, 0.0);
        assert_eq!(info.energy_rate, 0.0);
        assert_eq!(info.voltage, 0.0);
        assert!(info.temperature.is_none());
        assert!(info.cycle_count.is_none());
        assert!(info.time_to_full.is_none());
        assert!(info.time_to_empty.is_none());
        assert!(info.vendor.is_none());
        assert!(info.model.is_none());
        assert!(info.serial_number.is_none());
    }
}
