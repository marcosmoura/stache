//! CPU monitoring component.
//!
//! Provides synchronous helpers that read CPU metrics on demand using sysinfo
//! and direct SMC access for accurate temperature readings.

use std::sync::{LazyLock, Mutex, PoisonError};

use serde::Serialize;
use sysinfo::System;
use tauri_plugin_shell::ShellExt;

/// Minimum valid temperature in Celsius.
const TEMP_MIN: f64 = 0.0;
/// Maximum valid temperature in Celsius (sanity check).
const TEMP_MAX: f64 = 150.0;

/// CPU metrics payload.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CpuInfo {
    /// CPU usage percentage (0-100).
    usage: f32,
    /// CPU temperature in Celsius (None if unavailable).
    temperature: Option<f32>,
}

/// Global sysinfo instance to track CPU usage over time.
static SYS: LazyLock<Mutex<System>> = LazyLock::new(|| Mutex::new(System::new_all()));

/// Fetch current CPU metrics (usage and temperature) on demand.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_cpu_info(app: tauri::AppHandle) -> CpuInfo {
    let usage = get_cpu_usage().round();
    let temperature = get_cpu_temperature(&app).map(f32::round);

    CpuInfo { usage, temperature }
}

/// Get current CPU usage percentage.
fn get_cpu_usage() -> f32 {
    let mut sys = SYS.lock().unwrap_or_else(PoisonError::into_inner);

    // Refresh CPU info
    sys.refresh_cpu_all();

    // Get global CPU usage
    sys.global_cpu_usage()
}

/// Get CPU temperature using multiple methods in order of preference:
/// 1. Direct SMC access via smc crate (most accurate, requires proper entitlements)
/// 2. External tools (ismc or smctemp) if installed via Homebrew
fn get_cpu_temperature(app: &tauri::AppHandle) -> Option<f32> {
    // Try direct SMC access first
    if let Some(temp) = get_smc_cpu_temperature() {
        return Some(temp);
    }

    // Fall back to external tools
    get_shell_cpu_temperature(app)
}

/// Check if a temperature reading is within valid range.
#[inline]
const fn is_valid_temp(temp: f64) -> bool { temp > TEMP_MIN && temp < TEMP_MAX }

/// Calculate average of a slice of f64 values.
#[inline]
#[allow(clippy::cast_precision_loss)]
fn average_temps(temps: &[f64]) -> f64 { temps.iter().sum::<f64>() / temps.len() as f64 }

/// Read CPU temperature directly from SMC using the smc crate.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn get_smc_cpu_temperature() -> Option<f32> {
    let smc = smc::SMC::new().ok()?;

    // Try the built-in cpus_temperature method
    if let Ok(temps) = smc.cpus_temperature() {
        let valid_temps: Vec<f64> = temps.into_iter().filter(|&t| is_valid_temp(t)).collect();

        if !valid_temps.is_empty() {
            return Some(average_temps(&valid_temps) as f32);
        }
    }

    // Fallback: try all temperature sensors and filter for CPU-related ones
    if let Ok(all_temps) = smc.all_temperature_sensors() {
        let cpu_temps: Vec<f64> = all_temps
            .iter()
            .filter(|(key, _)| {
                let key_str = format!("{key:?}");
                // CPU-related keys: TC (Intel) or Tp/Te/Tf (Apple Silicon)
                key_str.contains("TC")
                    || key_str.contains("Tp")
                    || key_str.contains("Te")
                    || key_str.contains("Tf")
            })
            .map(|(_, &temp)| temp)
            .filter(|&t| is_valid_temp(t))
            .collect();

        if !cpu_temps.is_empty() {
            return Some(average_temps(&cpu_temps) as f32);
        }
    }

    None
}

/// Get CPU temperature using external CLI tools (ismc or smctemp).
/// These must be installed by the user via Homebrew.
#[allow(clippy::cast_possible_truncation)]
fn get_shell_cpu_temperature(app: &tauri::AppHandle) -> Option<f32> {
    // Try ismc first (outputs JSON with detailed sensor data)
    if let Ok(output) = run_shell_command(app, "ismc", &["temp", "-o", "json"])
        && let Some(temp) = parse_ismc_cpu_temps(&output)
    {
        return Some(temp);
    }

    // Fall back to smctemp (outputs just the temperature value)
    if let Ok(output) = run_shell_command(app, "smctemp", &["-c"])
        && let Ok(temp) = output.trim().parse::<f32>()
        && is_valid_temp(f64::from(temp))
    {
        return Some(temp);
    }

    None
}

fn run_shell_command(
    app: &tauri::AppHandle,
    program: &str,
    args: &[&str],
) -> Result<String, String> {
    tauri::async_runtime::block_on(async { app.shell().command(program).args(args).output().await })
        .map_err(|err| format!("Failed to run {program}: {err}"))
        .and_then(|output| {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!(
                    "{program} exited with status {:?}: {}",
                    output.status.code(),
                    stderr.trim()
                ));
            }

            String::from_utf8(output.stdout)
                .map_err(|err| format!("{program} returned invalid UTF-8: {err}"))
        })
}

/// Parse CPU temperature readings from ismc JSON output.
/// Returns the average temperature of all CPU-related sensors.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn parse_ismc_cpu_temps(json_str: &str) -> Option<f32> {
    use serde_json::Value;

    let data: Value = serde_json::from_str(json_str).ok()?;
    let mut temps = Vec::new();

    if let Some(obj) = data.as_object() {
        for (key, value) in obj {
            // Match CPU-related temperature sensors
            if key.starts_with("CPU")
                && let Some(quantity) = value.get("quantity").and_then(serde_json::Value::as_f64)
            {
                temps.push(quantity as f32);
            }
        }
    }

    if temps.is_empty() {
        return None;
    }

    let avg = temps.iter().sum::<f32>() / temps.len() as f32;
    Some(avg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_info_creation() {
        let info = CpuInfo {
            usage: 45.5,
            temperature: Some(65.2),
        };

        assert!((info.usage - 45.5).abs() < f32::EPSILON);
        assert!((info.temperature.unwrap() - 65.2).abs() < f32::EPSILON);
    }

    #[test]
    fn test_cpu_info_clone() {
        let info = CpuInfo {
            usage: 45.5,
            temperature: Some(65.2),
        };
        let cloned = info.clone();

        assert!((info.usage - cloned.usage).abs() < f32::EPSILON);
        assert!((info.temperature.unwrap() - cloned.temperature.unwrap()).abs() < f32::EPSILON);
    }

    #[test]
    fn test_cpu_info_with_no_temperature() {
        let info = CpuInfo { usage: 45.5, temperature: None };

        assert!((info.usage - 45.5).abs() < f32::EPSILON);
        assert!(info.temperature.is_none());
    }

    #[test]
    fn test_get_cpu_usage() {
        let usage = get_cpu_usage();
        assert!(
            (0.0..=100.0).contains(&usage),
            "CPU usage should be between 0 and 100"
        );
    }

    #[test]
    fn test_get_smc_cpu_temperature() {
        // SMC temperature may or may not be available depending on privileges
        let temp = get_smc_cpu_temperature();
        if let Some(t) = temp {
            assert!(t > 0.0 && t < 150.0, "Temperature should be reasonable");
        }
    }

    #[test]
    fn test_parse_ismc_cpu_temps() {
        let json = r#"{"CPU Efficiency Core 1":{"key":"Tp09","type":"flt","value":"52.1 °C","quantity":52.1,"unit":"°C"},"CPU Performance Core 1":{"key":"Tp01","type":"flt","value":"54.5 °C","quantity":54.5,"unit":"°C"},"GPU 1":{"key":"Tg05","type":"flt","value":"55.4 °C","quantity":55.4,"unit":"°C"}}"#;

        let avg = parse_ismc_cpu_temps(json).unwrap();
        // Average of 52.1 and 54.5 = 53.3
        assert!((avg - 53.3).abs() < 0.1);
    }

    #[test]
    fn test_parse_ismc_cpu_temps_no_cpu_sensors() {
        let json = r#"{"GPU 1":{"key":"Tg05","type":"flt","value":"55.4 °C","quantity":55.4,"unit":"°C"}}"#;

        let result = parse_ismc_cpu_temps(json);
        assert!(result.is_none());
    }

    #[test]
    fn test_is_valid_temp() {
        // Valid temperatures
        assert!(is_valid_temp(50.0));
        assert!(is_valid_temp(0.1));
        assert!(is_valid_temp(149.9));

        // Invalid temperatures
        assert!(!is_valid_temp(0.0)); // Exactly 0 is invalid
        assert!(!is_valid_temp(-10.0));
        assert!(!is_valid_temp(150.0)); // Exactly 150 is invalid
        assert!(!is_valid_temp(200.0));
    }

    #[test]
    fn test_average_temps() {
        let temps = vec![50.0, 60.0, 70.0];
        let avg = average_temps(&temps);
        assert!((avg - 60.0).abs() < f64::EPSILON);

        let single = vec![42.5];
        assert!((average_temps(&single) - 42.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_cpu_info_default() {
        let info = CpuInfo::default();
        assert!((info.usage - 0.0).abs() < f32::EPSILON);
        assert!(info.temperature.is_none());
    }

    // ========================================================================
    // Additional edge case tests
    // ========================================================================

    #[test]
    fn test_parse_ismc_cpu_temps_invalid_json() {
        let result = parse_ismc_cpu_temps("not valid json");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_ismc_cpu_temps_empty_json() {
        let result = parse_ismc_cpu_temps("{}");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_ismc_cpu_temps_array_json() {
        // JSON array instead of object
        let result = parse_ismc_cpu_temps("[]");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_ismc_cpu_temps_missing_quantity() {
        // CPU sensor without quantity field
        let json = r#"{"CPU Core 1":{"key":"Tp01","type":"flt","value":"54.5 °C","unit":"°C"}}"#;
        let result = parse_ismc_cpu_temps(json);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_ismc_cpu_temps_quantity_not_number() {
        // CPU sensor with non-numeric quantity
        let json = r#"{"CPU Core 1":{"key":"Tp01","type":"flt","value":"54.5 °C","quantity":"fifty","unit":"°C"}}"#;
        let result = parse_ismc_cpu_temps(json);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_ismc_cpu_temps_single_cpu_sensor() {
        let json = r#"{"CPU Core 1":{"key":"Tp01","type":"flt","value":"45.0 °C","quantity":45.0,"unit":"°C"}}"#;
        let result = parse_ismc_cpu_temps(json).unwrap();
        assert!((result - 45.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_parse_ismc_cpu_temps_multiple_cpu_sensors() {
        let json = r#"{"CPU Core 1":{"quantity":40.0},"CPU Core 2":{"quantity":50.0},"CPU Core 3":{"quantity":60.0},"CPU Core 4":{"quantity":70.0}}"#;
        let result = parse_ismc_cpu_temps(json).unwrap();
        // Average of 40, 50, 60, 70 = 55
        assert!((result - 55.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_is_valid_temp_boundary_values() {
        // Just above minimum
        assert!(is_valid_temp(0.001));
        // Just below maximum
        assert!(is_valid_temp(149.999));
        // Negative zero
        assert!(!is_valid_temp(-0.0));
        // Very high temperature
        assert!(!is_valid_temp(1000.0));
        // Very low temperature
        assert!(!is_valid_temp(-273.15));
    }

    #[test]
    fn test_average_temps_large_values() {
        let temps = vec![100.0, 100.0, 100.0, 100.0];
        let avg = average_temps(&temps);
        assert!((avg - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_average_temps_varying_values() {
        let temps = vec![10.0, 20.0, 30.0, 40.0, 50.0];
        let avg = average_temps(&temps);
        assert!((avg - 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_cpu_info_debug() {
        let info = CpuInfo {
            usage: 50.0,
            temperature: Some(70.0),
        };
        let debug_str = format!("{:?}", info);
        assert!(debug_str.contains("CpuInfo"));
        assert!(debug_str.contains("50"));
        assert!(debug_str.contains("70"));
    }

    #[test]
    fn test_cpu_info_serialization() {
        let info = CpuInfo {
            usage: 45.5,
            temperature: Some(65.2),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("usage"));
        assert!(json.contains("temperature"));
        assert!(json.contains("45.5"));
        assert!(json.contains("65.2"));
    }

    #[test]
    fn test_cpu_info_serialization_no_temperature() {
        let info = CpuInfo { usage: 45.5, temperature: None };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("null") || json.contains("temperature"));
    }

    #[test]
    fn test_temp_constants() {
        assert!((TEMP_MIN - 0.0).abs() < f64::EPSILON);
        assert!((TEMP_MAX - 150.0).abs() < f64::EPSILON);
    }
}
