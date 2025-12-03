//! CPU monitoring component.
//!
//! Provides synchronous helpers that read CPU metrics on demand using sysinfo
//! and direct SMC access for accurate temperature readings.

use std::sync::{LazyLock, Mutex};

use serde::Serialize;
use sysinfo::System;
use tauri_plugin_shell::ShellExt;

/// CPU metrics payload.
#[derive(Debug, Clone, Serialize)]
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
    let temperature = get_cpu_temperature(&app).map(|t| t.round());

    CpuInfo { usage, temperature }
}

/// Get current CPU usage percentage.
fn get_cpu_usage() -> f32 {
    let mut sys = SYS.lock().unwrap();

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

/// Read CPU temperature directly from SMC using the smc crate.
#[allow(clippy::cast_possible_truncation)]
fn get_smc_cpu_temperature() -> Option<f32> {
    let smc = smc::SMC::new().ok()?;

    // Try the built-in cpus_temperature method
    if let Ok(temps) = smc.cpus_temperature() {
        let valid_temps: Vec<f64> = temps.into_iter().filter(|&t| t > 0.0 && t < 150.0).collect();

        if !valid_temps.is_empty() {
            let avg = valid_temps.iter().sum::<f64>() / valid_temps.len() as f64;
            return Some(avg as f32);
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
            .filter(|&t| t > 0.0 && t < 150.0)
            .collect();

        if !cpu_temps.is_empty() {
            let avg = cpu_temps.iter().sum::<f64>() / cpu_temps.len() as f64;
            return Some(avg as f32);
        }
    }

    None
}

/// Get CPU temperature using external CLI tools (ismc or smctemp).
/// These must be installed by the user via Homebrew.
#[allow(clippy::cast_possible_truncation)]
fn get_shell_cpu_temperature(app: &tauri::AppHandle) -> Option<f32> {
    // Try ismc first (outputs JSON with detailed sensor data)
    if let Ok(output) = run_shell_command(app, "ismc", &["temp", "-o", "json"]) {
        if let Some(temp) = parse_ismc_cpu_temps(&output) {
            return Some(temp);
        }
    }

    // Fall back to smctemp (outputs just the temperature value)
    if let Ok(output) = run_shell_command(app, "smctemp", &["-c"]) {
        if let Ok(temp) = output.trim().parse::<f32>() {
            if temp > 0.0 && temp < 150.0 {
                return Some(temp);
            }
        }
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
#[allow(clippy::cast_possible_truncation)]
fn parse_ismc_cpu_temps(json_str: &str) -> Option<f32> {
    use serde_json::Value;

    let data: Value = serde_json::from_str(json_str).ok()?;
    let mut temps = Vec::new();

    if let Some(obj) = data.as_object() {
        for (key, value) in obj {
            // Match CPU-related temperature sensors
            if key.starts_with("CPU") {
                if let Some(quantity) = value.get("quantity").and_then(serde_json::Value::as_f64) {
                    temps.push(quantity as f32);
                }
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
}
