//! Priority-based device selection.
//!
//! This module implements the priority-based audio device selection algorithm
//! that determines which device should be used based on the configuration.

use super::device::{AudioDevice, find_device_by_name, find_device_by_priority};
use crate::config::ProxyAudioConfig;
use crate::platform::display::is_screen_mirroring_active;

/// Determines the target output device based on priority rules from config.
///
/// Priority order:
/// 1. `AirPlay` device when screen mirroring is active (always highest priority)
/// 2. Keep current `AirPlay` device (don't switch away from `AirPlay`)
/// 3. Devices in the config priority list (in order)
/// 4. Fallback to `MacBook` Pro speakers
///
/// # Arguments
///
/// * `current` - The currently selected output device
/// * `devices` - All available output devices
/// * `config` - The proxy audio configuration
///
/// # Returns
///
/// The device that should be set as the default output device.
#[must_use]
pub fn get_target_output_device<'a>(
    current: &AudioDevice,
    devices: &'a [AudioDevice],
    config: &ProxyAudioConfig,
) -> Option<&'a AudioDevice> {
    resolve_output_device(current, devices, config, is_screen_mirroring_active())
}

/// Inner implementation that accepts screen mirroring state for testability.
fn resolve_output_device<'a>(
    current: &AudioDevice,
    devices: &'a [AudioDevice],
    config: &ProxyAudioConfig,
    screen_mirroring_active: bool,
) -> Option<&'a AudioDevice> {
    // 1. When screen mirroring is active, AirPlay always gets highest priority
    if screen_mirroring_active && let Some(airplay) = devices.iter().find(|d| d.is_airplay()) {
        return Some(airplay);
    }

    // 2. Don't switch away from AirPlay - keep it if it's the current device
    if current.is_airplay() {
        return devices.iter().find(|d| d.id == current.id);
    }

    // 3. Check devices in config priority order
    for priority_device in &config.output {
        if let Some(device) = find_device_by_priority(devices, priority_device) {
            return Some(device);
        }
    }

    // 4. Fallback to MacBook Pro speakers
    find_device_by_name(devices, "MacBook Pro")
}

/// Determines the target input device based on priority rules from config.
///
/// Priority order:
/// 1. `AirPlay` device when screen mirroring is active (always highest priority)
/// 2. Keep current `AirPlay` device (don't switch away from `AirPlay`)
/// 3. Devices in the config priority list (in order)
/// 4. Fallback to `MacBook` Pro microphone
/// 5. Keep current if nothing else matches
///
/// # Arguments
///
/// * `current` - The currently selected input device
/// * `devices` - All available input devices
/// * `config` - The proxy audio configuration
///
/// # Returns
///
/// The device that should be set as the default input device.
#[must_use]
pub fn get_target_input_device<'a>(
    current: &AudioDevice,
    devices: &'a [AudioDevice],
    config: &ProxyAudioConfig,
) -> Option<&'a AudioDevice> {
    resolve_input_device(current, devices, config, is_screen_mirroring_active())
}

/// Inner implementation that accepts screen mirroring state for testability.
fn resolve_input_device<'a>(
    current: &AudioDevice,
    devices: &'a [AudioDevice],
    config: &ProxyAudioConfig,
    screen_mirroring_active: bool,
) -> Option<&'a AudioDevice> {
    // 1. When screen mirroring is active, AirPlay always gets highest priority
    if screen_mirroring_active && let Some(airplay) = devices.iter().find(|d| d.is_airplay()) {
        return Some(airplay);
    }

    // 2. Don't switch away from AirPlay - keep it if it's the current device
    if current.is_airplay() {
        return devices.iter().find(|d| d.id == current.id);
    }

    // 3. Check devices in config priority order
    for priority_device in &config.input {
        if let Some(device) = find_device_by_priority(devices, priority_device) {
            return Some(device);
        }
    }

    // 4. Fallback to MacBook Pro microphone
    if let Some(macbook) = find_device_by_name(devices, "MacBook Pro") {
        return Some(macbook);
    }

    // 5. Keep current if nothing else matches
    devices.iter().find(|d| d.id == current.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AudioDevicePriority, MatchStrategy};

    fn create_test_config() -> ProxyAudioConfig {
        ProxyAudioConfig {
            enabled: true,
            input: vec![
                AudioDevicePriority {
                    name: "AirPods Pro".to_string(),
                    strategy: MatchStrategy::Contains,
                    depends_on: None,
                },
                AudioDevicePriority {
                    name: "AT2020USB".to_string(),
                    strategy: MatchStrategy::Contains,
                    depends_on: None,
                },
                AudioDevicePriority {
                    name: "MacBook Pro".to_string(),
                    strategy: MatchStrategy::Contains,
                    depends_on: None,
                },
            ],
            output: vec![
                AudioDevicePriority {
                    name: "AirPods Pro".to_string(),
                    strategy: MatchStrategy::Contains,
                    depends_on: None,
                },
                AudioDevicePriority {
                    name: "External Speakers".to_string(),
                    strategy: MatchStrategy::Contains,
                    depends_on: None,
                },
                AudioDevicePriority {
                    name: "MacBook Pro".to_string(),
                    strategy: MatchStrategy::Contains,
                    depends_on: None,
                },
            ],
        }
    }

    #[test]
    fn output_follows_config_priority_without_mirroring() {
        let config = create_test_config();
        let current = AudioDevice {
            id: 1,
            name: "MacBook Pro Speakers".to_string(),
        };

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
                name: "Living Room AirPlay".to_string(),
            },
        ];

        // Without screen mirroring, config priority wins over AirPlay
        let target = resolve_output_device(&current, &devices, &config, false);
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, 2);
    }

    #[test]
    fn output_selects_airplay_when_screen_mirroring_active() {
        let config = create_test_config();
        let current = AudioDevice {
            id: 1,
            name: "MacBook Pro Speakers".to_string(),
        };

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
                name: "Living Room AirPlay".to_string(),
            },
        ];

        // With screen mirroring, AirPlay wins over config priority
        let target = resolve_output_device(&current, &devices, &config, true);
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, 3);
    }

    #[test]
    fn output_uses_config_priority_when_mirroring_but_no_airplay_device() {
        let config = create_test_config();
        let current = AudioDevice {
            id: 1,
            name: "MacBook Pro Speakers".to_string(),
        };

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

        // Screen mirroring is active but no AirPlay audio device exists,
        // so fall through to config priority
        let target = resolve_output_device(&current, &devices, &config, true);
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, 2);
    }

    #[test]
    fn output_uses_config_priority_when_no_airplay() {
        let config = create_test_config();
        let current = AudioDevice {
            id: 1,
            name: "MacBook Pro Speakers".to_string(),
        };

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

        let target = resolve_output_device(&current, &devices, &config, false);
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, 2);
    }

    #[test]
    fn output_keeps_airplay_when_current() {
        let config = create_test_config();
        let current = AudioDevice {
            id: 3,
            name: "Kitchen AirPlay".to_string(),
        };

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
                name: "Kitchen AirPlay".to_string(),
            },
        ];

        // Even without screen mirroring, don't switch away from AirPlay
        let target = resolve_output_device(&current, &devices, &config, false);
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, 3);
    }

    #[test]
    fn input_selects_airplay_when_screen_mirroring_active() {
        let config = create_test_config();
        let current = AudioDevice {
            id: 1,
            name: "MacBook Pro Microphone".to_string(),
        };

        let devices = vec![
            AudioDevice {
                id: 1,
                name: "MacBook Pro Microphone".to_string(),
            },
            AudioDevice {
                id: 2,
                name: "AT2020USB+".to_string(),
            },
            AudioDevice {
                id: 3,
                name: "TV AirPlay".to_string(),
            },
        ];

        let target = resolve_input_device(&current, &devices, &config, true);
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, 3);
    }

    #[test]
    fn input_uses_config_priority() {
        let config = create_test_config();
        let current = AudioDevice {
            id: 1,
            name: "MacBook Pro Microphone".to_string(),
        };

        let devices = vec![
            AudioDevice {
                id: 1,
                name: "MacBook Pro Microphone".to_string(),
            },
            AudioDevice {
                id: 2,
                name: "AT2020USB+".to_string(),
            },
        ];

        let target = resolve_input_device(&current, &devices, &config, false);
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, 2);
    }

    #[test]
    fn output_respects_dependency_when_satisfied() {
        use crate::config::AudioDeviceDependency;

        let config = ProxyAudioConfig {
            enabled: true,
            input: Vec::new(),
            output: vec![
                AudioDevicePriority {
                    name: "External Speakers".to_string(),
                    strategy: MatchStrategy::Contains,
                    depends_on: Some(AudioDeviceDependency {
                        name: "MiniFuse".to_string(),
                        strategy: MatchStrategy::StartsWith,
                    }),
                },
                AudioDevicePriority {
                    name: "MacBook Pro".to_string(),
                    strategy: MatchStrategy::Contains,
                    depends_on: None,
                },
            ],
        };

        let current = AudioDevice {
            id: 1,
            name: "MacBook Pro Speakers".to_string(),
        };

        let devices = vec![
            AudioDevice {
                id: 1,
                name: "MacBook Pro Speakers".to_string(),
            },
            AudioDevice {
                id: 2,
                name: "External Speakers".to_string(),
            },
            AudioDevice {
                id: 3,
                name: "MiniFuse 2".to_string(),
            },
        ];

        let target = resolve_output_device(&current, &devices, &config, false);
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, 2);
    }

    #[test]
    fn output_skips_device_when_dependency_not_satisfied() {
        use crate::config::AudioDeviceDependency;

        let config = ProxyAudioConfig {
            enabled: true,
            input: Vec::new(),
            output: vec![
                AudioDevicePriority {
                    name: "External Speakers".to_string(),
                    strategy: MatchStrategy::Contains,
                    depends_on: Some(AudioDeviceDependency {
                        name: "MiniFuse".to_string(),
                        strategy: MatchStrategy::StartsWith,
                    }),
                },
                AudioDevicePriority {
                    name: "MacBook Pro".to_string(),
                    strategy: MatchStrategy::Contains,
                    depends_on: None,
                },
            ],
        };

        let current = AudioDevice {
            id: 1,
            name: "MacBook Pro Speakers".to_string(),
        };

        let devices = vec![
            AudioDevice {
                id: 1,
                name: "MacBook Pro Speakers".to_string(),
            },
            AudioDevice {
                id: 2,
                name: "External Speakers".to_string(),
            },
        ];

        let target = resolve_output_device(&current, &devices, &config, false);
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, 1);
    }

    #[test]
    fn dependency_device_is_never_selected() {
        use crate::config::AudioDeviceDependency;

        let config = ProxyAudioConfig {
            enabled: true,
            input: Vec::new(),
            output: vec![AudioDevicePriority {
                name: "External Speakers".to_string(),
                strategy: MatchStrategy::Contains,
                depends_on: Some(AudioDeviceDependency {
                    name: "MiniFuse".to_string(),
                    strategy: MatchStrategy::StartsWith,
                }),
            }],
        };

        let current = AudioDevice {
            id: 3,
            name: "MiniFuse 2".to_string(),
        };

        let devices = vec![AudioDevice {
            id: 3,
            name: "MiniFuse 2".to_string(),
        }];

        let target = resolve_output_device(&current, &devices, &config, false);
        assert!(target.is_none());
    }
}
