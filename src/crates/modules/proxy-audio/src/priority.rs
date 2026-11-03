//! Priority-based default-device selection.
//!
//! The rules are intentionally the legacy rules, in the legacy order. The
//! public entry points read the real mirroring state; the `resolve_*` variants
//! take it as an argument so the policy is testable without a display.

use stache_macos::display::is_screen_mirroring_active;

use crate::config::ProxyAudioConfig;
use crate::device::{AudioDevice, find_device_by_name, find_device_by_priority};

/// Chooses the target output device for the current state.
#[must_use]
pub fn get_target_output_device<'a>(
    current: &AudioDevice,
    devices: &'a [AudioDevice],
    config: &ProxyAudioConfig,
) -> Option<&'a AudioDevice> {
    resolve_output_device(current, devices, config, is_screen_mirroring_active())
}

/// Chooses the output device with an explicit mirroring flag.
///
/// Order:
/// 1. an `AirPlay` device while screen mirroring is active;
/// 2. keep the current device when it is `AirPlay`;
/// 3. an HDMI device when the current device is built in;
/// 4. the first eligible configured output entry;
/// 5. the first device whose name contains `MacBook Pro`.
#[must_use]
pub fn resolve_output_device<'a>(
    current: &AudioDevice,
    devices: &'a [AudioDevice],
    config: &ProxyAudioConfig,
    screen_mirroring_active: bool,
) -> Option<&'a AudioDevice> {
    if screen_mirroring_active
        && let Some(airplay) = devices.iter().find(|device| device.is_airplay())
    {
        return Some(airplay);
    }

    if current.is_airplay() {
        return devices.iter().find(|device| device.id == current.id);
    }

    if current.is_builtin()
        && let Some(hdmi) = devices.iter().find(|device| device.is_hdmi())
    {
        return Some(hdmi);
    }

    for priority in &config.output {
        if let Some(device) = find_device_by_priority(devices, priority) {
            return Some(device);
        }
    }

    find_device_by_name(devices, "MacBook Pro")
}

/// Chooses the target input device for the current state.
#[must_use]
pub fn get_target_input_device<'a>(
    current: &AudioDevice,
    devices: &'a [AudioDevice],
    config: &ProxyAudioConfig,
) -> Option<&'a AudioDevice> {
    resolve_input_device(current, devices, config, is_screen_mirroring_active())
}

/// Chooses the input device with an explicit mirroring flag.
///
/// Input omits the HDMI rule, falls back to a `MacBook Pro` microphone, and
/// finally keeps the current device.
#[must_use]
pub fn resolve_input_device<'a>(
    current: &AudioDevice,
    devices: &'a [AudioDevice],
    config: &ProxyAudioConfig,
    screen_mirroring_active: bool,
) -> Option<&'a AudioDevice> {
    if screen_mirroring_active
        && let Some(airplay) = devices.iter().find(|device| device.is_airplay())
    {
        return Some(airplay);
    }

    if current.is_airplay() {
        return devices.iter().find(|device| device.id == current.id);
    }

    for priority in &config.input {
        if let Some(device) = find_device_by_priority(devices, priority) {
            return Some(device);
        }
    }

    if let Some(macbook) = find_device_by_name(devices, "MacBook Pro") {
        return Some(macbook);
    }

    devices.iter().find(|device| device.id == current.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AudioDeviceDependency, AudioDevicePriority, MatchStrategy};
    use crate::device::AudioDevice;

    fn device(id: u32, name: &str) -> AudioDevice { AudioDevice::new(id, name) }

    fn entry(name: &str) -> AudioDevicePriority {
        AudioDevicePriority {
            name: name.to_owned(),
            strategy: MatchStrategy::Contains,
            depends_on: None,
        }
    }

    fn test_config() -> ProxyAudioConfig {
        ProxyAudioConfig {
            enabled: true,
            input: vec![
                entry("AirPods Pro"),
                entry("AT2020USB"),
                entry("MacBook Pro"),
            ],
            output: vec![
                entry("AirPods Pro"),
                entry("External Speakers"),
                entry("MacBook Pro"),
            ],
        }
    }

    #[test]
    fn output_follows_config_priority_without_mirroring() {
        let config = test_config();
        let current = device(1, "MacBook Pro Speakers");
        let devices = vec![
            device(1, "MacBook Pro Speakers"),
            device(2, "AirPods Pro"),
            device(3, "Living Room AirPlay"),
        ];
        let target = resolve_output_device(&current, &devices, &config, false);
        assert_eq!(target.map(|device| device.id), Some(2));
    }

    #[test]
    fn output_selects_airplay_when_screen_mirroring_active() {
        let config = test_config();
        let current = device(1, "MacBook Pro Speakers");
        let devices = vec![
            device(1, "MacBook Pro Speakers"),
            device(2, "AirPods Pro"),
            device(3, "Living Room AirPlay"),
        ];
        let target = resolve_output_device(&current, &devices, &config, true);
        assert_eq!(target.map(|device| device.id), Some(3));
    }

    #[test]
    fn output_uses_config_priority_when_mirroring_but_no_airplay_device() {
        let config = test_config();
        let current = device(1, "MacBook Pro Speakers");
        let devices = vec![device(1, "MacBook Pro Speakers"), device(2, "AirPods Pro")];
        let target = resolve_output_device(&current, &devices, &config, true);
        assert_eq!(target.map(|device| device.id), Some(2));
    }

    #[test]
    fn output_keeps_airplay_when_current() {
        let config = test_config();
        let current = device(3, "Kitchen AirPlay");
        let devices = vec![
            device(1, "MacBook Pro Speakers"),
            device(2, "AirPods Pro"),
            device(3, "Kitchen AirPlay"),
        ];
        let target = resolve_output_device(&current, &devices, &config, false);
        assert_eq!(target.map(|device| device.id), Some(3));
    }

    #[test]
    fn input_selects_airplay_when_screen_mirroring_active() {
        let config = test_config();
        let current = device(1, "MacBook Pro Microphone");
        let devices = vec![
            device(1, "MacBook Pro Microphone"),
            device(2, "AT2020USB+"),
            device(3, "TV AirPlay"),
        ];
        let target = resolve_input_device(&current, &devices, &config, true);
        assert_eq!(target.map(|device| device.id), Some(3));
    }

    #[test]
    fn input_uses_config_priority() {
        let config = test_config();
        let current = device(1, "MacBook Pro Microphone");
        let devices = vec![device(1, "MacBook Pro Microphone"), device(2, "AT2020USB+")];
        let target = resolve_input_device(&current, &devices, &config, false);
        assert_eq!(target.map(|device| device.id), Some(2));
    }

    #[test]
    fn output_respects_dependency_when_satisfied() {
        let config = ProxyAudioConfig {
            enabled: true,
            input: Vec::new(),
            output: vec![AudioDevicePriority {
                name: "External Speakers".to_owned(),
                strategy: MatchStrategy::Contains,
                depends_on: Some(AudioDeviceDependency {
                    name: "MiniFuse".to_owned(),
                    strategy: MatchStrategy::StartsWith,
                }),
            }],
        };
        let current = device(1, "MacBook Pro Speakers");
        let devices = vec![
            device(1, "MacBook Pro Speakers"),
            device(2, "External Speakers"),
            device(3, "MiniFuse 2"),
        ];
        let target = resolve_output_device(&current, &devices, &config, false);
        assert_eq!(target.map(|device| device.id), Some(2));
    }

    #[test]
    fn output_skips_device_when_dependency_not_satisfied() {
        let config = ProxyAudioConfig {
            enabled: true,
            input: Vec::new(),
            output: vec![
                AudioDevicePriority {
                    name: "External Speakers".to_owned(),
                    strategy: MatchStrategy::Contains,
                    depends_on: Some(AudioDeviceDependency {
                        name: "MiniFuse".to_owned(),
                        strategy: MatchStrategy::StartsWith,
                    }),
                },
                entry("MacBook Pro"),
            ],
        };
        let current = device(1, "MacBook Pro Speakers");
        let devices = vec![
            device(1, "MacBook Pro Speakers"),
            device(2, "External Speakers"),
        ];
        let target = resolve_output_device(&current, &devices, &config, false);
        assert_eq!(target.map(|device| device.id), Some(1));
    }

    #[test]
    fn dependency_device_is_never_selected() {
        let config = ProxyAudioConfig {
            enabled: true,
            input: Vec::new(),
            output: vec![AudioDevicePriority {
                name: "External Speakers".to_owned(),
                strategy: MatchStrategy::Contains,
                depends_on: Some(AudioDeviceDependency {
                    name: "MiniFuse".to_owned(),
                    strategy: MatchStrategy::StartsWith,
                }),
            }],
        };
        let current = device(3, "MiniFuse 2");
        let devices = vec![device(3, "MiniFuse 2")];
        assert!(resolve_output_device(&current, &devices, &config, false).is_none());
    }

    #[test]
    fn output_selects_hdmi_when_current_is_builtin_and_hdmi_available() {
        let config = test_config();
        let current = device(1, "MacBook Pro Speakers");
        let devices = vec![
            device(1, "MacBook Pro Speakers"),
            device(2, "HDMI Output"),
            device(3, "AirPods Pro"),
        ];
        let target = resolve_output_device(&current, &devices, &config, false);
        assert_eq!(target.map(|device| device.id), Some(2));
    }

    #[test]
    fn output_keeps_airplay_even_when_hdmi_available() {
        let config = test_config();
        let current = device(3, "Living Room AirPlay");
        let devices = vec![
            device(1, "MacBook Pro Speakers"),
            device(2, "HDMI Output"),
            device(3, "Living Room AirPlay"),
        ];
        let target = resolve_output_device(&current, &devices, &config, false);
        assert_eq!(target.map(|device| device.id), Some(3));
    }

    #[test]
    fn output_stays_on_builtin_when_no_hdmi_available() {
        let config = test_config();
        let current = device(1, "MacBook Pro Speakers");
        let devices = vec![device(1, "MacBook Pro Speakers"), device(3, "AirPods Pro")];
        let target = resolve_output_device(&current, &devices, &config, false);
        assert_eq!(target.map(|device| device.id), Some(3));
    }

    #[test]
    fn output_keeps_configured_device_when_not_builtin_even_with_hdmi() {
        let config = test_config();
        let current = device(2, "AirPods Pro");
        let devices = vec![
            device(1, "MacBook Pro Speakers"),
            device(2, "AirPods Pro"),
            device(3, "HDMI Output"),
        ];
        let target = resolve_output_device(&current, &devices, &config, false);
        assert_eq!(target.map(|device| device.id), Some(2));
    }

    #[test]
    fn output_falls_back_to_a_macbook_device() {
        let config = ProxyAudioConfig::default();
        let current = device(9, "Studio Display");
        let devices = vec![
            device(1, "MacBook Pro Speakers"),
            device(9, "Studio Display"),
        ];
        let target = resolve_output_device(&current, &devices, &config, false);
        assert_eq!(target.map(|device| device.id), Some(1));
    }

    #[test]
    fn input_falls_back_to_the_microphone_then_the_current_device() {
        let config = ProxyAudioConfig::default();
        let current = device(2, "USB Microphone");
        let devices = vec![
            device(1, "MacBook Pro Microphone"),
            device(2, "USB Microphone"),
        ];
        assert_eq!(
            resolve_input_device(&current, &devices, &config, false).map(|device| device.id),
            Some(1)
        );
        let devices = vec![device(2, "USB Microphone"), device(3, "Line In")];
        assert_eq!(
            resolve_input_device(&current, &devices, &config, false).map(|device| device.id),
            Some(2)
        );
    }

    #[test]
    fn input_keeps_airplay_when_current() {
        let config = test_config();
        let current = device(3, "Kitchen AirPlay");
        let devices = vec![device(1, "AT2020USB+"), device(3, "Kitchen AirPlay")];
        let target = resolve_input_device(&current, &devices, &config, false);
        assert_eq!(target.map(|device| device.id), Some(3));
    }
}
