//! Audio device management module.
//!
//! This module provides audio device listing, type detection, and automatic
//! device switching based on priority rules defined in the configuration file.
//!
//! # Features
//!
//! - **Device listing**: List all audio devices with their properties.
//! - **Automatic device switching**: Monitors device connections/disconnections and
//!   automatically switches to the highest priority available device.
//! - **Config-based priorities**: Device priorities can be configured in the barba
//!   config file under the `proxyAudio` section.
//! - **`AirPlay` priority**: `AirPlay` devices are always given highest priority, even
//!   if not explicitly listed in the configuration.

mod device;
mod list;
mod priority;
mod watcher;

// Re-export commonly used types
pub use device::{AudioDevice, AudioDeviceType};
pub use list::{AudioDeviceInfo, DeviceFilter, format_devices_table, list_devices};

use crate::config::get_config;

/// Initializes the audio module.
///\n/// Sets up device watchers and applies initial device configuration.
/// Only starts if proxy audio is enabled in the config.
pub fn init() {
    let config = get_config();

    // Only start if proxy audio is enabled
    if config.proxy_audio.is_enabled() {
        watcher::start(config.proxy_audio.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::device::*;

    #[test]
    fn audio_device_name_contains_case_insensitive() {
        let device = AudioDevice {
            id: 1,
            name: "AirPods Pro".to_string(),
        };

        assert!(device.name_contains("airpods"));
        assert!(device.name_contains("AIRPODS"));
        assert!(device.name_contains("AirPods"));
        assert!(!device.name_contains("macbook"));
    }

    #[test]
    fn find_device_by_name_returns_matching_device() {
        let devices = vec![
            AudioDevice {
                id: 1,
                name: "MacBook Pro Speakers".to_string(),
            },
            AudioDevice {
                id: 2,
                name: "AirPods Pro".to_string(),
            },
            AudioDevice {
                id: 3,
                name: "External Speakers".to_string(),
            },
        ];

        let found = find_device_by_name(&devices, "airpods");
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, 2);

        let not_found = find_device_by_name(&devices, "minifuse");
        assert!(not_found.is_none());
    }
}
