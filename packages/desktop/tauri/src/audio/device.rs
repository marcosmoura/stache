//! Audio device representation and type detection.
//!
//! This module provides the `AudioDevice` struct and utility functions
//! for detecting device types (`AirPlay`, virtual, etc.).

use barba_shared::{AudioDevicePriority, MatchStrategy};
use coreaudio::audio_unit::Scope;
use coreaudio::audio_unit::macos_helpers::{
    get_audio_device_ids, get_audio_device_supports_scope, get_default_device_id, get_device_name,
};
use objc2_core_audio::AudioDeviceID;
use regex::Regex;

/// Represents an audio device with its ID and name.
#[derive(Debug, Clone)]
pub struct AudioDevice {
    /// The `CoreAudio` device ID.
    pub id: AudioDeviceID,
    /// The human-readable device name.
    pub name: String,
}

/// The type of audio device based on its transport mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDeviceType {
    /// `AirPlay` streaming device (highest priority by default).
    AirPlay,
    /// Bluetooth device (e.g., `AirPods`, headphones).
    Bluetooth,
    /// Virtual/aggregate device (e.g., proxy audio device).
    Virtual,
    /// USB audio interface.
    Usb,
    /// Built-in speakers or microphone.
    BuiltIn,
    /// Unknown or other device type.
    Other,
}

impl AudioDevice {
    /// Creates a new `AudioDevice` from a device ID.
    ///
    /// Returns `None` if the device name cannot be retrieved.
    #[must_use]
    pub fn from_id(id: AudioDeviceID) -> Option<Self> {
        get_device_name(id).ok().map(|name| Self { id, name })
    }

    /// Checks if the device name contains the given substring (case-insensitive).
    #[must_use]
    pub fn name_contains(&self, substring: &str) -> bool {
        self.name.to_lowercase().contains(&substring.to_lowercase())
    }

    /// Detects the device type based on its name and characteristics.
    #[must_use]
    pub fn device_type(&self) -> AudioDeviceType {
        let name_lower = self.name.to_lowercase();

        // AirPlay devices
        if name_lower.contains("airplay") {
            return AudioDeviceType::AirPlay;
        }

        // Bluetooth devices (AirPods, Beats, etc.)
        if name_lower.contains("airpods")
            || name_lower.contains("beats")
            || name_lower.contains("bluetooth")
        {
            return AudioDeviceType::Bluetooth;
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
            return AudioDeviceType::Virtual;
        }

        // USB devices
        if name_lower.contains("usb")
            || name_lower.contains("minifuse")
            || name_lower.contains("focusrite")
            || name_lower.contains("scarlett")
            || name_lower.contains("at2020")
        {
            return AudioDeviceType::Usb;
        }

        // Built-in devices
        if name_lower.contains("macbook") || name_lower.contains("built-in") {
            return AudioDeviceType::BuiltIn;
        }

        AudioDeviceType::Other
    }

    /// Returns whether this is an `AirPlay` device.
    #[must_use]
    pub fn is_airplay(&self) -> bool { self.device_type() == AudioDeviceType::AirPlay }

    /// Returns whether this is a virtual/aggregate device.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_virtual(&self) -> bool { self.device_type() == AudioDeviceType::Virtual }

    /// Returns whether this is a Bluetooth device.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_bluetooth(&self) -> bool { self.device_type() == AudioDeviceType::Bluetooth }
}

/// Gets all output audio devices.
#[must_use]
pub fn get_output_devices() -> Vec<AudioDevice> {
    get_audio_device_ids()
        .unwrap_or_default()
        .into_iter()
        .filter(|&id| get_audio_device_supports_scope(id, Scope::Output).unwrap_or(false))
        .filter_map(AudioDevice::from_id)
        .collect()
}

/// Gets all input audio devices.
#[must_use]
pub fn get_input_devices() -> Vec<AudioDevice> {
    get_audio_device_ids()
        .unwrap_or_default()
        .into_iter()
        .filter(|&id| get_audio_device_supports_scope(id, Scope::Input).unwrap_or(false))
        .filter_map(AudioDevice::from_id)
        .collect()
}

/// Gets the current default output device.
#[must_use]
pub fn get_default_output_device() -> Option<AudioDevice> {
    get_default_device_id(false).and_then(AudioDevice::from_id)
}

/// Gets the current default input device.
#[must_use]
pub fn get_default_input_device() -> Option<AudioDevice> {
    get_default_device_id(true).and_then(AudioDevice::from_id)
}

/// Finds a device by name (case-insensitive substring match).
#[must_use]
pub fn find_device_by_name<'a>(devices: &'a [AudioDevice], name: &str) -> Option<&'a AudioDevice> {
    devices.iter().find(|device| device.name_contains(name))
}

/// Finds a device matching the priority entry using the configured strategy.
///
/// # Arguments
///
/// * `devices` - The list of available devices
/// * `priority` - The priority entry containing the name and strategy
///
/// # Returns
///
/// The first device matching the priority entry, or `None` if no match.
#[must_use]
pub fn find_device_by_priority<'a>(
    devices: &'a [AudioDevice],
    priority: &AudioDevicePriority,
) -> Option<&'a AudioDevice> {
    let pattern = &priority.name;

    devices.iter().find(|device| {
        let device_name_lower = device.name.to_lowercase();
        let pattern_lower = pattern.to_lowercase();

        match priority.strategy {
            MatchStrategy::Exact => device_name_lower == pattern_lower,
            MatchStrategy::Contains => device_name_lower.contains(&pattern_lower),
            MatchStrategy::StartsWith => device_name_lower.starts_with(&pattern_lower),
            MatchStrategy::Regex => {
                Regex::new(pattern).ok().is_some_and(|re| re.is_match(&device.name))
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn audio_device_type_detection() {
        let airplay = AudioDevice {
            id: 1,
            name: "Living Room AirPlay".to_string(),
        };
        assert_eq!(airplay.device_type(), AudioDeviceType::AirPlay);

        let airpods = AudioDevice {
            id: 2,
            name: "AirPods Pro".to_string(),
        };
        assert_eq!(airpods.device_type(), AudioDeviceType::Bluetooth);

        let virtual_device = AudioDevice {
            id: 3,
            name: "BlackHole 2ch".to_string(),
        };
        assert_eq!(virtual_device.device_type(), AudioDeviceType::Virtual);

        let usb_device = AudioDevice {
            id: 4,
            name: "AT2020USB+".to_string(),
        };
        assert_eq!(usb_device.device_type(), AudioDeviceType::Usb);

        let builtin = AudioDevice {
            id: 5,
            name: "MacBook Pro Speakers".to_string(),
        };
        assert_eq!(builtin.device_type(), AudioDeviceType::BuiltIn);
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

    #[test]
    fn find_device_by_priority_exact_strategy() {
        let devices = vec![
            AudioDevice {
                id: 1,
                name: "MacBook Pro Speakers".to_string(),
            },
            AudioDevice {
                id: 2,
                name: "AirPods Pro".to_string(),
            },
        ];

        // Exact match (case-insensitive)
        let priority = AudioDevicePriority {
            name: "airpods pro".to_string(),
            strategy: MatchStrategy::Exact,
        };
        let found = find_device_by_priority(&devices, &priority);
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, 2);

        // Partial name should not match with exact strategy
        let priority_partial = AudioDevicePriority {
            name: "AirPods".to_string(),
            strategy: MatchStrategy::Exact,
        };
        let not_found = find_device_by_priority(&devices, &priority_partial);
        assert!(not_found.is_none());
    }

    #[test]
    fn find_device_by_priority_contains_strategy() {
        let devices = vec![
            AudioDevice {
                id: 1,
                name: "MacBook Pro Speakers".to_string(),
            },
            AudioDevice {
                id: 2,
                name: "AirPods Pro 3".to_string(),
            },
        ];

        // Contains match
        let priority = AudioDevicePriority {
            name: "AirPods".to_string(),
            strategy: MatchStrategy::Contains,
        };
        let found = find_device_by_priority(&devices, &priority);
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, 2);

        // Case-insensitive contains
        let priority_lower = AudioDevicePriority {
            name: "macbook".to_string(),
            strategy: MatchStrategy::Contains,
        };
        let found_lower = find_device_by_priority(&devices, &priority_lower);
        assert!(found_lower.is_some());
        assert_eq!(found_lower.unwrap().id, 1);
    }

    #[test]
    fn find_device_by_priority_starts_with_strategy() {
        let devices = vec![
            AudioDevice {
                id: 1,
                name: "MacBook Pro Speakers".to_string(),
            },
            AudioDevice {
                id: 2,
                name: "AirPods Pro".to_string(),
            },
        ];

        // Starts with match
        let priority = AudioDevicePriority {
            name: "MacBook".to_string(),
            strategy: MatchStrategy::StartsWith,
        };
        let found = find_device_by_priority(&devices, &priority);
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, 1);

        // "Pro" appears in the name but not at the start
        let priority_mid = AudioDevicePriority {
            name: "Pro".to_string(),
            strategy: MatchStrategy::StartsWith,
        };
        let not_found = find_device_by_priority(&devices, &priority_mid);
        assert!(not_found.is_none());
    }

    #[test]
    fn find_device_by_priority_regex_strategy() {
        let devices = vec![
            AudioDevice {
                id: 1,
                name: "AT2020USB-X".to_string(),
            },
            AudioDevice {
                id: 2,
                name: "AT2020USB+".to_string(),
            },
            AudioDevice {
                id: 3,
                name: "MiniFuse 2".to_string(),
            },
        ];

        // Regex match for AT2020USB variants
        let priority = AudioDevicePriority {
            name: r"AT2020USB[+-]?".to_string(),
            strategy: MatchStrategy::Regex,
        };
        let found = find_device_by_priority(&devices, &priority);
        assert!(found.is_some());
        // Should match the first one (AT2020USB-X)
        assert_eq!(found.unwrap().id, 1);

        // Invalid regex should not match anything
        let priority_invalid = AudioDevicePriority {
            name: r"[invalid".to_string(),
            strategy: MatchStrategy::Regex,
        };
        let not_found = find_device_by_priority(&devices, &priority_invalid);
        assert!(not_found.is_none());
    }

    #[test]
    fn find_device_by_priority_default_strategy_is_exact() {
        let devices = vec![AudioDevice {
            id: 1,
            name: "Test Device".to_string(),
        }];

        let priority = AudioDevicePriority {
            name: "Test Device".to_string(),
            ..Default::default()
        };

        // Default strategy should be Exact
        assert_eq!(priority.strategy, MatchStrategy::Exact);

        let found = find_device_by_priority(&devices, &priority);
        assert!(found.is_some());
    }
}
