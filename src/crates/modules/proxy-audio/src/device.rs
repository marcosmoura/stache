//! Device identity, type detection and name matching.
//!
//! The policy layer never compares device identifiers across events: macOS
//! reassigns them when a device reconnects, so every rule matches a name and an
//! identifier is used only within one enumeration.

use regex::Regex;
use stache_macos::audio::{AudioBackend, AudioDevice as NativeAudioDevice, AudioDirection};

use crate::config::{AudioDeviceDependency, AudioDevicePriority, MatchStrategy};

/// One device offered to the priority rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDevice {
    /// The session-scoped `CoreAudio` identifier.
    pub id: u32,
    /// The human-readable device name.
    pub name: String,
    /// Whether `CoreAudio` reported the HDMI transport for this device.
    pub hdmi: bool,
}

/// The type of audio device inferred from its transport name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDeviceType {
    /// `AirPlay` streaming device.
    AirPlay,
    /// Bluetooth device, such as `AirPods` or Beats headphones.
    Bluetooth,
    /// Virtual or aggregate device.
    Virtual,
    /// USB audio interface.
    Usb,
    /// Built-in speakers or microphone.
    BuiltIn,
    /// HDMI audio output.
    Hdmi,
    /// Any other device.
    Other,
}

impl AudioDeviceType {
    /// Detects the type from the device name.
    ///
    /// This matches the legacy heuristic. It never returns [`Self::Hdmi`]:
    /// HDMI is detected from the `CoreAudio` transport in
    /// [`AudioDevice::is_hdmi`].
    #[must_use]
    pub fn detect(name: &str) -> Self {
        let name = name.to_lowercase();
        if name.contains("airplay") {
            return Self::AirPlay;
        }
        if name.contains("airpods") || name.contains("beats") || name.contains("bluetooth") {
            return Self::Bluetooth;
        }
        if name.contains("virtual")
            || name.contains("proxy")
            || name.contains("aggregate")
            || name.contains("multi-output")
            || name.contains("blackhole")
            || name.contains("soundflower")
            || name.contains("loopback")
        {
            return Self::Virtual;
        }
        if name.contains("usb") {
            return Self::Usb;
        }
        if name.contains("macbook") || name.contains("built-in") {
            return Self::BuiltIn;
        }
        Self::Other
    }

    /// The lowercase spelling used by CLI and JSON output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AirPlay => "airplay",
            Self::Bluetooth => "bluetooth",
            Self::Virtual => "virtual",
            Self::Usb => "usb",
            Self::BuiltIn => "builtin",
            Self::Hdmi => "hdmi",
            Self::Other => "other",
        }
    }
}

impl AudioDevice {
    /// Creates a device without transport information.
    #[must_use]
    pub fn new(id: u32, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            hdmi: false,
        }
    }

    /// Marks the device as reporting the HDMI transport.
    #[must_use]
    pub const fn with_hdmi(mut self, hdmi: bool) -> Self {
        self.hdmi = hdmi;
        self
    }

    /// Projects one backend device, asking the backend about its transport.
    #[must_use]
    pub fn from_native(device: &NativeAudioDevice, backend: &dyn AudioBackend) -> Self {
        Self {
            id: device.id,
            name: device.name.clone(),
            hdmi: backend.is_hdmi(device.id),
        }
    }

    /// Checks the device name for a substring, ignoring case.
    #[must_use]
    pub fn name_contains(&self, substring: &str) -> bool {
        self.name.to_lowercase().contains(&substring.to_lowercase())
    }

    /// The device type inferred from the name.
    #[must_use]
    pub fn device_type(&self) -> AudioDeviceType { AudioDeviceType::detect(&self.name) }

    /// Whether this is an `AirPlay` device.
    #[must_use]
    pub fn is_airplay(&self) -> bool { self.device_type() == AudioDeviceType::AirPlay }

    /// Whether this is a virtual or aggregate device.
    #[must_use]
    pub fn is_virtual(&self) -> bool { self.device_type() == AudioDeviceType::Virtual }

    /// Whether this is a Bluetooth device.
    #[must_use]
    pub fn is_bluetooth(&self) -> bool { self.device_type() == AudioDeviceType::Bluetooth }

    /// Whether this is an HDMI output.
    ///
    /// The `CoreAudio` transport is authoritative; a name check keeps the
    /// legacy fallback for devices that report an unknown transport.
    #[must_use]
    pub fn is_hdmi(&self) -> bool { self.hdmi || self.name_contains("hdmi") }

    /// Whether this is a built-in device.
    #[must_use]
    pub fn is_builtin(&self) -> bool { self.device_type() == AudioDeviceType::BuiltIn }
}

/// Every device supporting `direction`, in `CoreAudio` order.
#[must_use]
pub fn devices_for(backend: &dyn AudioBackend, direction: AudioDirection) -> Vec<AudioDevice> {
    backend
        .devices()
        .into_iter()
        .filter(|device| match direction {
            AudioDirection::Input => device.input,
            AudioDirection::Output => device.output,
        })
        .map(|device| AudioDevice::from_native(&device, backend))
        .collect()
}

/// Finds a device by a case-insensitive name substring.
#[must_use]
pub fn find_device_by_name<'a>(devices: &'a [AudioDevice], name: &str) -> Option<&'a AudioDevice> {
    devices.iter().find(|device| device.name_contains(name))
}

/// Checks whether a device matches a configured name pattern.
#[must_use]
pub fn matches_pattern(device: &AudioDevice, pattern: &str, strategy: MatchStrategy) -> bool {
    let device_name = device.name.to_lowercase();
    let pattern_lower = pattern.to_lowercase();
    match strategy {
        MatchStrategy::Exact => device_name == pattern_lower,
        MatchStrategy::Contains => device_name.contains(&pattern_lower),
        MatchStrategy::StartsWith => device_name.starts_with(&pattern_lower),
        MatchStrategy::Regex => Regex::new(pattern).is_ok_and(|regex| regex.is_match(&device.name)),
    }
}

/// Whether the optional dependency device is present in the same list.
///
/// The dependency is only a condition; it is never selected.
#[must_use]
pub fn is_dependency_satisfied(
    all_devices: &[AudioDevice],
    dependency: Option<&AudioDeviceDependency>,
) -> bool {
    dependency.is_none_or(|dependency| {
        all_devices
            .iter()
            .any(|device| matches_pattern(device, &dependency.name, dependency.strategy))
    })
}

/// Finds the first device matching one priority entry.
///
/// Returns `None` when the dependency is not satisfied, even if the target
/// device itself is present.
#[must_use]
pub fn find_device_by_priority<'a>(
    devices: &'a [AudioDevice],
    priority: &AudioDevicePriority,
) -> Option<&'a AudioDevice> {
    if !is_dependency_satisfied(devices, priority.depends_on.as_ref()) {
        return None;
    }
    devices
        .iter()
        .find(|device| matches_pattern(device, &priority.name, priority.strategy))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: u32, name: &str) -> AudioDevice { AudioDevice::new(id, name) }

    fn priority(name: &str, strategy: MatchStrategy) -> AudioDevicePriority {
        AudioDevicePriority {
            name: name.to_owned(),
            strategy,
            depends_on: None,
        }
    }

    #[test]
    fn audio_device_type_detect_airplay() {
        assert_eq!(
            AudioDeviceType::detect("Living Room AirPlay"),
            AudioDeviceType::AirPlay
        );
        assert_eq!(
            AudioDeviceType::detect("AIRPLAY Speaker"),
            AudioDeviceType::AirPlay
        );
    }

    #[test]
    fn audio_device_type_detect_bluetooth() {
        assert_eq!(
            AudioDeviceType::detect("AirPods Pro"),
            AudioDeviceType::Bluetooth
        );
        assert_eq!(AudioDeviceType::detect("Beats Solo"), AudioDeviceType::Bluetooth);
        assert_eq!(
            AudioDeviceType::detect("Bluetooth Headphones"),
            AudioDeviceType::Bluetooth
        );
    }

    #[test]
    fn audio_device_type_detect_virtual() {
        assert_eq!(
            AudioDeviceType::detect("BlackHole 2ch"),
            AudioDeviceType::Virtual
        );
        assert_eq!(
            AudioDeviceType::detect("Multi-Output Device"),
            AudioDeviceType::Virtual
        );
        assert_eq!(
            AudioDeviceType::detect("Soundflower (2ch)"),
            AudioDeviceType::Virtual
        );
        assert_eq!(
            AudioDeviceType::detect("Loopback Audio"),
            AudioDeviceType::Virtual
        );
        assert_eq!(
            AudioDeviceType::detect("Aggregate Device"),
            AudioDeviceType::Virtual
        );
        assert_eq!(AudioDeviceType::detect("Proxy Audio"), AudioDeviceType::Virtual);
    }

    #[test]
    fn audio_device_type_detect_usb() {
        assert_eq!(AudioDeviceType::detect("usb"), AudioDeviceType::Usb);
    }

    #[test]
    fn audio_device_type_detect_builtin() {
        assert_eq!(
            AudioDeviceType::detect("MacBook Pro Speakers"),
            AudioDeviceType::BuiltIn
        );
        assert_eq!(
            AudioDeviceType::detect("Built-in Output"),
            AudioDeviceType::BuiltIn
        );
    }

    #[test]
    fn audio_device_type_detect_other() {
        assert_eq!(AudioDeviceType::detect("Unknown Device"), AudioDeviceType::Other);
        assert_eq!(AudioDeviceType::detect("Random Speaker"), AudioDeviceType::Other);
    }

    #[test]
    fn audio_device_type_as_str() {
        assert_eq!(AudioDeviceType::AirPlay.as_str(), "airplay");
        assert_eq!(AudioDeviceType::Bluetooth.as_str(), "bluetooth");
        assert_eq!(AudioDeviceType::Virtual.as_str(), "virtual");
        assert_eq!(AudioDeviceType::Usb.as_str(), "usb");
        assert_eq!(AudioDeviceType::BuiltIn.as_str(), "builtin");
        assert_eq!(AudioDeviceType::Hdmi.as_str(), "hdmi");
        assert_eq!(AudioDeviceType::Other.as_str(), "other");
    }

    #[test]
    fn audio_device_name_contains_case_insensitive() {
        let device = device(1, "AirPods Pro");
        assert!(device.name_contains("airpods"));
        assert!(device.name_contains("AIRPODS"));
        assert!(device.name_contains("AirPods"));
        assert!(!device.name_contains("macbook"));
    }

    #[test]
    fn device_helpers_read_the_name() {
        let airplay = device(1, "Living Room AirPlay");
        assert!(airplay.is_airplay());
        assert!(!airplay.is_virtual());
        assert!(!airplay.is_bluetooth());
        assert!(!airplay.is_builtin());

        let airpods = device(2, "AirPods Pro");
        assert!(airpods.is_bluetooth());

        let virtual_device = device(3, "BlackHole 2ch");
        assert!(virtual_device.is_virtual());

        let builtin = device(4, "MacBook Pro Speakers");
        assert!(builtin.is_builtin());
    }

    #[test]
    fn hdmi_uses_the_transport_then_the_name() {
        assert!(device(1, "HDMI Output").is_hdmi());
        assert!(!device(2, "MacBook Pro Speakers").is_hdmi());
        assert!(
            device(3, "Studio Display").with_hdmi(true).is_hdmi(),
            "the transport wins for a device whose name hides HDMI"
        );
    }

    #[test]
    fn find_device_by_name_returns_matching_device() {
        let devices = vec![
            device(1, "MacBook Pro Speakers"),
            device(2, "AirPods Pro"),
            device(3, "External Speakers"),
        ];
        let found = find_device_by_name(&devices, "airpods").expect("matching device");
        assert_eq!(found.id, 2);
        assert!(find_device_by_name(&devices, "minifuse").is_none());
    }

    #[test]
    fn find_device_by_priority_exact_strategy() {
        let devices = vec![device(1, "MacBook Pro Speakers"), device(2, "AirPods Pro")];
        let found =
            find_device_by_priority(&devices, &priority("airpods pro", MatchStrategy::Exact))
                .expect("exact, case-insensitive");
        assert_eq!(found.id, 2);
        assert!(
            find_device_by_priority(&devices, &priority("AirPods", MatchStrategy::Exact)).is_none()
        );
    }

    #[test]
    fn find_device_by_priority_contains_strategy() {
        let devices = vec![
            device(1, "MacBook Pro Speakers"),
            device(2, "AirPods Pro 3"),
        ];
        let found =
            find_device_by_priority(&devices, &priority("AirPods", MatchStrategy::Contains))
                .expect("contains");
        assert_eq!(found.id, 2);
        let found =
            find_device_by_priority(&devices, &priority("macbook", MatchStrategy::Contains))
                .expect("contains, case-insensitive");
        assert_eq!(found.id, 1);
    }

    #[test]
    fn find_device_by_priority_starts_with_strategy() {
        let devices = vec![device(1, "MacBook Pro Speakers"), device(2, "AirPods Pro")];
        let found =
            find_device_by_priority(&devices, &priority("MacBook", MatchStrategy::StartsWith))
                .expect("prefix");
        assert_eq!(found.id, 1);
        assert!(
            find_device_by_priority(&devices, &priority("Pro", MatchStrategy::StartsWith))
                .is_none()
        );
    }

    #[test]
    fn find_device_by_priority_regex_strategy() {
        let devices = vec![
            device(1, "AT2020USB-X"),
            device(2, "AT2020USB+"),
            device(3, "MiniFuse 2"),
        ];
        let found =
            find_device_by_priority(&devices, &priority(r"AT2020USB[+-]?", MatchStrategy::Regex))
                .expect("regex");
        assert_eq!(found.id, 1, "the first match in device order wins");
        assert!(
            find_device_by_priority(&devices, &priority("[invalid", MatchStrategy::Regex))
                .is_none()
        );
    }

    #[test]
    fn regex_matching_is_case_sensitive() {
        let devices = vec![device(1, "AT2020USB-X")];
        assert!(
            find_device_by_priority(&devices, &priority("at2020", MatchStrategy::Regex)).is_none()
        );
        assert!(
            find_device_by_priority(&devices, &priority("AT2020", MatchStrategy::Regex)).is_some()
        );
    }

    #[test]
    fn find_device_by_priority_default_strategy_is_exact() {
        let devices = vec![device(1, "Test Device")];
        let entry = AudioDevicePriority {
            name: "Test Device".to_owned(),
            ..AudioDevicePriority::default()
        };
        assert_eq!(entry.strategy, MatchStrategy::Exact);
        assert!(find_device_by_priority(&devices, &entry).is_some());
    }

    #[test]
    fn find_device_by_priority_with_dependency_satisfied() {
        let devices = vec![
            device(1, "External Speakers"),
            device(2, "MiniFuse 2"),
            device(3, "MacBook Pro Speakers"),
        ];
        let entry = AudioDevicePriority {
            name: "External Speakers".to_owned(),
            strategy: MatchStrategy::Exact,
            depends_on: Some(AudioDeviceDependency {
                name: "MiniFuse".to_owned(),
                strategy: MatchStrategy::StartsWith,
            }),
        };
        let found = find_device_by_priority(&devices, &entry).expect("dependency present");
        assert_eq!(found.id, 1);
    }

    #[test]
    fn find_device_by_priority_with_dependency_not_satisfied() {
        let devices = vec![
            device(1, "External Speakers"),
            device(3, "MacBook Pro Speakers"),
        ];
        let entry = AudioDevicePriority {
            name: "External Speakers".to_owned(),
            strategy: MatchStrategy::Exact,
            depends_on: Some(AudioDeviceDependency {
                name: "MiniFuse".to_owned(),
                strategy: MatchStrategy::StartsWith,
            }),
        };
        assert!(find_device_by_priority(&devices, &entry).is_none());
    }

    #[test]
    fn is_dependency_satisfied_with_none() {
        assert!(is_dependency_satisfied(&[device(1, "Test Device")], None));
    }

    #[test]
    fn is_dependency_satisfied_uses_the_matching_strategy() {
        let devices = vec![device(1, "MiniFuse 2")];
        let exact = AudioDeviceDependency {
            name: "MiniFuse".to_owned(),
            strategy: MatchStrategy::Exact,
        };
        assert!(!is_dependency_satisfied(&devices, Some(&exact)));
        let contains = AudioDeviceDependency {
            name: "MiniFuse".to_owned(),
            strategy: MatchStrategy::Contains,
        };
        assert!(is_dependency_satisfied(&devices, Some(&contains)));
        let starts_with = AudioDeviceDependency {
            name: "MiniFuse".to_owned(),
            strategy: MatchStrategy::StartsWith,
        };
        assert!(is_dependency_satisfied(&devices, Some(&starts_with)));
    }

    #[test]
    fn is_dependency_satisfied_with_missing_device() {
        let devices = vec![device(1, "MacBook Pro")];
        let dependency = AudioDeviceDependency {
            name: "MiniFuse".to_owned(),
            strategy: MatchStrategy::StartsWith,
        };
        assert!(!is_dependency_satisfied(&devices, Some(&dependency)));
    }
}
