//! Priority-based device selection.
//!
//! This module implements the priority-based audio device selection algorithm
//! that determines which device should be used based on the configuration.

use super::device::{AudioDevice, find_device_by_name, find_device_by_priority};
use crate::config::ProxyAudioConfig;

/// Determines the target output device based on priority rules from config.
///
/// Priority order:
/// 1. Keep current `AirPlay` device (don't switch away from `AirPlay`)
/// 2. Devices in the config priority list (in order)
/// 3. Fallback to `MacBook` Pro speakers
///
/// Note: This function does NOT automatically switch to `AirPlay`. The watcher
/// should call this when devices change, and `AirPlay` devices will be selected
/// when they appear in the device list and match the priority config.
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
    // 1. Don't switch away from AirPlay - keep it if it's the current device
    if current.is_airplay() {
        return devices.iter().find(|d| d.id == current.id);
    }

    // 2. Check devices in config priority order
    for priority_device in &config.output.priority {
        if let Some(device) = find_device_by_priority(devices, priority_device) {
            return Some(device);
        }
    }

    // 3. Fallback to MacBook Pro speakers
    find_device_by_name(devices, "MacBook Pro")
}

/// Determines the target input device based on priority rules from config.
///
/// Priority order:
/// 1. Keep current `AirPlay` device (don't switch away from `AirPlay`)
/// 2. Devices in the config priority list (in order)
/// 3. Fallback to `MacBook` Pro microphone
///
/// Note: This function does NOT automatically switch to `AirPlay`. The watcher
/// should call this when devices change, and `AirPlay` devices will be selected
/// when they appear in the device list and match the priority config.
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
    // 1. Don't switch away from AirPlay - keep it if it's the current device
    if current.is_airplay() {
        return devices.iter().find(|d| d.id == current.id);
    }

    // 2. Check devices in config priority order
    for priority_device in &config.input.priority {
        if let Some(device) = find_device_by_priority(devices, priority_device) {
            return Some(device);
        }
    }

    // 3. Fallback to MacBook Pro microphone
    if let Some(macbook) = find_device_by_name(devices, "MacBook Pro") {
        return Some(macbook);
    }

    // 4. Keep current if nothing else matches
    devices.iter().find(|d| d.id == current.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AudioDevicePriority, MatchStrategy, ProxyAudioInputConfig, ProxyAudioOutputConfig,
    };

    fn create_test_config() -> ProxyAudioConfig {
        ProxyAudioConfig {
            enabled: true,
            input: ProxyAudioInputConfig {
                name: "Virtual Input".to_string(),
                priority: vec![
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
            },
            output: ProxyAudioOutputConfig {
                name: "Virtual Output".to_string(),
                buffer_size: 128,
                priority: vec![
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
            },
        }
    }

    #[test]
    fn output_follows_config_priority() {
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

        let target = get_target_output_device(&current, &devices, &config);
        assert!(target.is_some());
        // AirPods Pro is first in config priority list (not AirPlay)
        // AirPlay is only kept if it's the current device
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

        let target = get_target_output_device(&current, &devices, &config);
        assert!(target.is_some());
        // AirPods Pro is first in config priority
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

        let target = get_target_output_device(&current, &devices, &config);
        assert!(target.is_some());
        // Should keep current AirPlay device
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

        let target = get_target_input_device(&current, &devices, &config);
        assert!(target.is_some());
        // AT2020USB is in config priority
        assert_eq!(target.unwrap().id, 2);
    }

    #[test]
    fn output_respects_dependency_when_satisfied() {
        use crate::config::AudioDeviceDependency;

        let config = ProxyAudioConfig {
            enabled: true,
            input: ProxyAudioInputConfig::default(),
            output: ProxyAudioOutputConfig {
                name: "Virtual Output".to_string(),
                buffer_size: 128,
                priority: vec![
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
            },
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

        // MiniFuse is present, so External Speakers should be selected
        let target = get_target_output_device(&current, &devices, &config);
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, 2);
    }

    #[test]
    fn output_skips_device_when_dependency_not_satisfied() {
        use crate::config::AudioDeviceDependency;

        let config = ProxyAudioConfig {
            enabled: true,
            input: ProxyAudioInputConfig::default(),
            output: ProxyAudioOutputConfig {
                name: "Virtual Output".to_string(),
                buffer_size: 128,
                priority: vec![
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
            },
        };

        let current = AudioDevice {
            id: 1,
            name: "MacBook Pro Speakers".to_string(),
        };

        // MiniFuse is NOT present
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

        // MiniFuse is NOT present, so External Speakers should be skipped
        // and MacBook Pro should be selected instead
        let target = get_target_output_device(&current, &devices, &config);
        assert!(target.is_some());
        assert_eq!(target.unwrap().id, 1); // MacBook Pro Speakers
    }

    #[test]
    fn dependency_device_is_never_selected() {
        use crate::config::AudioDeviceDependency;

        // Even if MiniFuse is in the device list, it should NEVER be
        // selected as the target - it's only a condition
        let config = ProxyAudioConfig {
            enabled: true,
            input: ProxyAudioInputConfig::default(),
            output: ProxyAudioOutputConfig {
                name: "Virtual Output".to_string(),
                buffer_size: 128,
                priority: vec![AudioDevicePriority {
                    name: "External Speakers".to_string(),
                    strategy: MatchStrategy::Contains,
                    depends_on: Some(AudioDeviceDependency {
                        name: "MiniFuse".to_string(),
                        strategy: MatchStrategy::StartsWith,
                    }),
                }],
            },
        };

        let current = AudioDevice {
            id: 3,
            name: "MiniFuse 2".to_string(),
        };

        // External Speakers is not present, only MiniFuse
        let devices = vec![AudioDevice {
            id: 3,
            name: "MiniFuse 2".to_string(),
        }];

        // External Speakers is not present, so nothing should be selected
        // MiniFuse should NOT be selected even though it's the only device
        let target = get_target_output_device(&current, &devices, &config);
        // Falls back to find_device_by_name for MacBook Pro, which is also not present
        assert!(target.is_none());
    }
}
