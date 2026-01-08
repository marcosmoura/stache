//! Audio device listing functionality for CLI.
//!
//! This module provides functions to list audio devices using `CoreAudio`.

use std::fmt::Write;

use coreaudio::audio_unit::Scope;
use coreaudio::audio_unit::macos_helpers::{
    get_audio_device_ids, get_audio_device_supports_scope, get_device_name,
};
use serde::Serialize;

/// Represents an audio device with its properties for CLI output.
#[derive(Debug, Clone, Serialize)]
pub struct AudioDeviceInfo {
    /// The human-readable device name.
    pub name: String,
    /// The device type (e.g., "airplay", "bluetooth", "usb", "builtin", "virtual", "other").
    #[serde(rename = "type")]
    pub device_type: String,
    /// Whether this device supports input.
    pub input: bool,
    /// Whether this device supports output.
    pub output: bool,
}

/// Filter for audio device listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceFilter {
    /// Show all devices.
    All,
    /// Show only input devices.
    InputOnly,
    /// Show only output devices.
    OutputOnly,
}

/// Detects the device type based on its name.
fn detect_device_type(name: &str) -> &'static str {
    let name_lower = name.to_lowercase();

    // AirPlay devices
    if name_lower.contains("airplay") {
        return "airplay";
    }

    // Bluetooth devices (AirPods, Beats, etc.)
    if name_lower.contains("airpods")
        || name_lower.contains("beats")
        || name_lower.contains("bluetooth")
    {
        return "bluetooth";
    }

    // Virtual/aggregate devices
    if name_lower.contains("virtual")
        || name_lower.contains("proxy")
        || name_lower.contains("aggregate")
        || name_lower.contains("multi-output")
        || name_lower.contains("blackhole")
        || name_lower.contains("soundflower")
        || name_lower.contains("loopback")
    {
        return "virtual";
    }

    // USB devices
    if name_lower.contains("usb")
        || name_lower.contains("minifuse")
        || name_lower.contains("focusrite")
        || name_lower.contains("scarlett")
        || name_lower.contains("at2020")
    {
        return "usb";
    }

    // Built-in devices
    if name_lower.contains("macbook") || name_lower.contains("built-in") {
        return "builtin";
    }

    "other"
}

/// Lists all audio devices matching the given filter.
#[must_use]
pub fn list_devices(filter: DeviceFilter) -> Vec<AudioDeviceInfo> {
    let device_ids = get_audio_device_ids().unwrap_or_default();

    let mut devices: Vec<AudioDeviceInfo> = device_ids
        .into_iter()
        .filter_map(|id| {
            let name = get_device_name(id).ok()?;
            let supports_input = get_audio_device_supports_scope(id, Scope::Input).unwrap_or(false);
            let supports_output =
                get_audio_device_supports_scope(id, Scope::Output).unwrap_or(false);

            // Apply filter
            match filter {
                DeviceFilter::InputOnly if !supports_input => return None,
                DeviceFilter::OutputOnly if !supports_output => return None,
                DeviceFilter::All | DeviceFilter::InputOnly | DeviceFilter::OutputOnly => {}
            }

            Some(AudioDeviceInfo {
                name: name.clone(),
                device_type: detect_device_type(&name).to_string(),
                input: supports_input,
                output: supports_output,
            })
        })
        .collect();

    // Sort by name for consistent output
    devices.sort_by(|a, b| a.name.cmp(&b.name));

    devices
}

/// Formats devices for human-readable output.
#[must_use]
pub fn format_devices_table(devices: &[AudioDeviceInfo]) -> String {
    if devices.is_empty() {
        return "No audio devices found.".to_string();
    }

    // Find max name length for alignment
    let max_name_len = devices.iter().map(|d| d.name.len()).max().unwrap_or(20);
    let name_col_width = max_name_len.max(4); // At least "Name" width

    let mut output = String::new();

    // Header
    let _ = writeln!(
        output,
        "{:<name_col_width$}  {:<10}  {:<5}  {:<6}",
        "Name", "Type", "Input", "Output"
    );
    let _ = writeln!(
        output,
        "{:<name_col_width$}  {:<10}  {:<5}  {:<6}",
        "-".repeat(name_col_width),
        "-".repeat(10),
        "-".repeat(5),
        "-".repeat(6)
    );

    // Rows
    for device in devices {
        let input_mark = if device.input { "Y" } else { "-" };
        let output_mark = if device.output { "Y" } else { "-" };

        let _ = writeln!(
            output,
            "{:<name_col_width$}  {:<10}  {:<5}  {:<6}",
            device.name, device.device_type, input_mark, output_mark
        );
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_device_type_airplay() {
        assert_eq!(detect_device_type("Living Room AirPlay"), "airplay");
    }

    #[test]
    fn test_detect_device_type_bluetooth() {
        assert_eq!(detect_device_type("AirPods Pro"), "bluetooth");
        assert_eq!(detect_device_type("Beats Solo"), "bluetooth");
    }

    #[test]
    fn test_detect_device_type_virtual() {
        assert_eq!(detect_device_type("BlackHole 2ch"), "virtual");
        assert_eq!(detect_device_type("Multi-Output Device"), "virtual");
    }

    #[test]
    fn test_detect_device_type_usb() {
        assert_eq!(detect_device_type("AT2020USB+"), "usb");
        assert_eq!(detect_device_type("MiniFuse 2"), "usb");
    }

    #[test]
    fn test_detect_device_type_builtin() {
        assert_eq!(detect_device_type("MacBook Pro Speakers"), "builtin");
    }

    #[test]
    fn test_detect_device_type_other() {
        assert_eq!(detect_device_type("Unknown Device"), "other");
    }

    #[test]
    fn test_audio_device_info_serialization() {
        let device = AudioDeviceInfo {
            name: "Test Device".to_string(),
            device_type: "usb".to_string(),
            input: true,
            output: false,
        };

        let json = serde_json::to_string(&device).unwrap();
        assert!(json.contains("\"name\":\"Test Device\""));
        assert!(json.contains("\"type\":\"usb\""));
        assert!(json.contains("\"input\":true"));
        assert!(json.contains("\"output\":false"));
    }

    #[test]
    fn test_format_devices_table_empty() {
        let devices: Vec<AudioDeviceInfo> = vec![];
        let output = format_devices_table(&devices);
        assert_eq!(output, "No audio devices found.");
    }

    #[test]
    fn test_format_devices_table_with_devices() {
        let devices = vec![AudioDeviceInfo {
            name: "Test".to_string(),
            device_type: "usb".to_string(),
            input: true,
            output: true,
        }];
        let output = format_devices_table(&devices);
        assert!(output.contains("Test"));
        assert!(output.contains("usb"));
        assert!(output.contains("Y"));
    }
}
