//! Audio device management module.
//!
//! Automatically switches audio input and output devices based on connected hardware
//! and priority rules defined in the configuration file.
//!
//! # Features
//!
//! - **Automatic device switching**: Monitors device connections/disconnections and
//!   automatically switches to the highest priority available device.
//! - **Config-based priorities**: Device priorities can be configured in the barba
//!   config file under the `proxyAudio` section.
//! - **`AirPlay` priority**: `AirPlay` devices are always given highest priority, even
//!   if not explicitly listed in the configuration.
//! - **Legacy mode**: If no config is provided, uses hardcoded priority rules.
//!
//! # Configuration Example
//!
//! ```json
//! {
//!   "proxyAudio": {
//!     "enabled": true,
//!     "input": {
//!       "priority": [
//!         { "name": "AirPods Pro" },
//!         { "name": "AT2020USB" },
//!         { "name": "MacBook Pro" }
//!       ]
//!     },
//!     "output": {
//!       "bufferSize": 128,
//!       "priority": [
//!         { "name": "AirPods Pro" },
//!         { "name": "External Speakers" },
//!         { "name": "MacBook Pro" }
//!       ]
//!     }
//!   }
//! }
//! ```
//!
//! # Device Priority (Default/Legacy)
//!
//! When no configuration is provided, the following hardcoded rules apply:
//!
//! ## Output Priority
//! 1. `AirPods` (when connected)
//! 2. Current device if `AirPlay` (don't switch away from `AirPlay`)
//! 3. External Speakers (when audio interface connected)
//! 4. Microsoft Teams Audio (when in use and audio interface connected)
//! 5. `MacBook` Pro built-in speakers (fallback)
//!
//! ## Input Priority
//! 1. External USB microphone (AT2020USB)
//! 2. `AirPods` microphone
//! 3. `MacBook` Pro built-in microphone (fallback)

mod device;
mod priority;
mod watcher;

// Re-export commonly used types

use crate::config::get_config;

/// Initializes the audio module.
///
/// Sets up device watchers and applies initial device configuration.
/// Uses the proxy audio configuration from the barba config file if available.
pub fn init() {
    let config = get_config();

    // Only use proxy audio config if it's enabled
    let proxy_config = if config.proxy_audio.is_enabled() {
        Some(config.proxy_audio.clone())
    } else {
        None
    };

    // Start the audio device watcher
    watcher::start(proxy_config);
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
