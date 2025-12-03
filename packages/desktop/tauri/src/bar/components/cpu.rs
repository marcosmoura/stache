//! CPU monitoring component.
//!
//! Provides synchronous helpers that read CPU metrics on demand using sysinfo
//! and platform utilities.

use std::sync::{LazyLock, Mutex};

use serde::Serialize;
use sysinfo::System;
use tauri_plugin_shell::ShellExt;

/// CPU metrics payload.
#[derive(Debug, Clone, Serialize)]
pub struct CpuInfo {
    /// CPU usage percentage (0-100).
    usage: f32,
    /// CPU temperature in Celsius.
    temperature: f32,
}

/// Global sysinfo instance to track CPU usage over time.
static SYS: LazyLock<Mutex<System>> = LazyLock::new(|| Mutex::new(System::new_all()));

/// Fetch current CPU metrics (usage and temperature) on demand.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_cpu_info(app: tauri::AppHandle) -> CpuInfo {
    let mut runner = |program: &str, args: &[&str]| run_shell_command(&app, program, args);
    get_cpu_info_with_runner(&mut runner)
}

fn get_cpu_info_with_runner<R>(runner: &mut R) -> CpuInfo
where R: FnMut(&str, &[&str]) -> Result<String, String> {
    let usage = get_cpu_usage().round();
    let temperature = get_cpu_temperature_with_runner(runner).round();

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

fn get_cpu_temperature_with_runner<R>(runner: &mut R) -> f32
where R: FnMut(&str, &[&str]) -> Result<String, String> {
    if let Ok(output) = runner("ismc", &["temp", "-o", "json"])
        && let Ok(temps) = parse_ismc_cpu_temps(&output)
        && !temps.is_empty()
    {
        #[allow(clippy::cast_precision_loss)]
        let avg = temps.iter().sum::<f32>() / temps.len() as f32;
        return avg;
    }

    if let Ok(output) = runner("smctemp", &["-c", "-i20", "-f", "-n5"])
        && let Ok(temp) = output.trim().parse::<f32>()
    {
        return temp;
    }

    50.0
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
fn parse_ismc_cpu_temps(json_str: &str) -> Result<Vec<f32>, serde_json::Error> {
    use serde_json::Value;

    let data: Value = serde_json::from_str(json_str)?;
    let mut temps = Vec::new();

    if let Some(obj) = data.as_object() {
        for (key, value) in obj {
            // Match CPU-related temperature sensors
            if key.starts_with("CPU")
                && let Some(quantity) = value.get("quantity").and_then(serde_json::Value::as_f64)
            {
                #[allow(clippy::cast_possible_truncation)]
                temps.push(quantity as f32);
            }
        }
    }

    Ok(temps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_info_creation() {
        let info = CpuInfo { usage: 45.5, temperature: 65.2 };

        assert!((info.usage - 45.5).abs() < f32::EPSILON);
        assert!((info.temperature - 65.2).abs() < f32::EPSILON);
    }

    #[test]
    fn test_cpu_info_clone() {
        let info = CpuInfo { usage: 45.5, temperature: 65.2 };
        let cloned = info.clone();

        assert!((info.usage - cloned.usage).abs() < f32::EPSILON);
        assert!((info.temperature - cloned.temperature).abs() < f32::EPSILON);
    }

    #[test]
    fn test_get_cpu_temperature_returns_valid_value() {
        let mut runner =
            |_cmd: &str, _args: &[&str]| -> Result<String, String> { Err("missing".to_string()) };
        let temp = get_cpu_temperature_with_runner(&mut runner);
        assert!(temp > 0.0 && temp < 150.0, "Temperature should be reasonable");
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
    fn test_get_cpu_info_returns_reasonable_values() {
        let mut runner =
            |_cmd: &str, _args: &[&str]| -> Result<String, String> { Err("missing".to_string()) };
        let info = get_cpu_info_with_runner(&mut runner);

        assert!((0.0..=100.0).contains(&info.usage));
        assert!(info.temperature > 0.0 && info.temperature < 150.0);
    }

    #[test]
    fn test_parse_ismc_cpu_temps() {
        let json = r#"{"CPU Efficiency Core 1":{"key":"Tp09","type":"flt","value":"52.1 °C","quantity":52.1,"unit":"°C"},"CPU Performance Core 1":{"key":"Tp01","type":"flt","value":"54.5 °C","quantity":54.5,"unit":"°C"},"GPU 1":{"key":"Tg05","type":"flt","value":"55.4 °C","quantity":55.4,"unit":"°C"}}"#;

        let temps = parse_ismc_cpu_temps(json).unwrap();
        assert_eq!(temps.len(), 2);
        assert!((temps[0] - 52.1).abs() < f32::EPSILON);
        assert!((temps[1] - 54.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_parse_ismc_cpu_temps_empty() {
        let json = r#"{"GPU 1":{"key":"Tg05","type":"flt","value":"55.4 °C","quantity":55.4,"unit":"°C"}}"#;

        let temps = parse_ismc_cpu_temps(json).unwrap();
        assert_eq!(temps.len(), 0);
    }
}
