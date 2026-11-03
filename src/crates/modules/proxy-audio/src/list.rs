//! Read-only device listing shared by the CLI and the socket.
//!
//! The JSON shape, ordering and table layout are the legacy contract; both the
//! `stache audio list` command and the `proxyAudio.list` method use this module
//! so they can never disagree.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_macos::audio::AudioBackend;

use crate::device::AudioDeviceType;

/// One audio device as shown by `stache audio list` and `proxyAudio.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AudioDeviceInfo {
    /// The human-readable device name.
    pub name: String,
    /// The detected device type (`airplay`, `bluetooth`, `usb`, `builtin`,
    /// `virtual` or `other`).
    #[serde(rename = "type")]
    pub device_type: String,
    /// Whether the device supports input.
    pub input: bool,
    /// Whether the device supports output.
    pub output: bool,
}

/// Which devices a listing includes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceFilter {
    /// Every device.
    #[default]
    All,
    /// Devices that support input.
    InputOnly,
    /// Devices that support output.
    OutputOnly,
}

impl DeviceFilter {
    /// Maps the two CLI flags to the legacy filter rule.
    ///
    /// Neither flag and both flags select every device.
    #[must_use]
    pub const fn from_flags(input: bool, output: bool) -> Self {
        match (input, output) {
            (true, false) => Self::InputOnly,
            (false, true) => Self::OutputOnly,
            (true, true) | (false, false) => Self::All,
        }
    }
}

/// Lists the devices matching `filter`, sorted by name.
#[must_use]
pub fn list_devices(backend: &dyn AudioBackend, filter: DeviceFilter) -> Vec<AudioDeviceInfo> {
    let mut devices: Vec<AudioDeviceInfo> = backend
        .devices()
        .into_iter()
        .filter(|device| match filter {
            DeviceFilter::InputOnly => device.input,
            DeviceFilter::OutputOnly => device.output,
            DeviceFilter::All => true,
        })
        .map(|device| AudioDeviceInfo {
            device_type: AudioDeviceType::detect(&device.name).as_str().to_owned(),
            name: device.name,
            input: device.input,
            output: device.output,
        })
        .collect();
    devices.sort_by(|left, right| left.name.cmp(&right.name));
    devices
}

/// Formats a human-readable table with the legacy columns and widths.
#[must_use]
pub fn format_devices_table(devices: &[AudioDeviceInfo]) -> String {
    if devices.is_empty() {
        return "No audio devices found.".to_owned();
    }
    let name_width = devices.iter().map(|device| device.name.len()).max().unwrap_or(20).max(4);
    let mut output = String::new();
    let _ = writeln!(
        output,
        "{:<name_width$}  {:<10}  {:<5}  {:<6}",
        "Name", "Type", "Input", "Output"
    );
    let _ = writeln!(
        output,
        "{:<name_width$}  {:<10}  {:<5}  {:<6}",
        "-".repeat(name_width),
        "-".repeat(10),
        "-".repeat(5),
        "-".repeat(6)
    );
    for device in devices {
        let input = if device.input { "Y" } else { "-" };
        let output_mark = if device.output { "Y" } else { "-" };
        let _ = writeln!(
            output,
            "{:<name_width$}  {:<10}  {:<5}  {:<6}",
            device.name, device.device_type, input, output_mark
        );
    }
    output
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use stache_macos::audio::{AudioBackend, AudioDevice, AudioDirection, AudioError};

    use super::*;

    struct Devices(Vec<AudioDevice>);

    impl AudioBackend for Devices {
        fn devices(&self) -> Vec<AudioDevice> { self.0.clone() }

        fn default_device(&self, _: AudioDirection) -> Option<AudioDevice> { None }

        fn set_default_device(&self, _: AudioDirection, _: u32) -> Result<(), AudioError> {
            Err(AudioError::new("set default audio device", -1))
        }

        fn is_hdmi(&self, _: u32) -> bool { false }

        fn subscribe(
            &self,
            _: stache_macos::audio::AudioWakeup,
        ) -> Result<Box<dyn stache_macos::audio::AudioChangeSubscription>, AudioError> {
            Err(AudioError::new("add audio property listener", -1))
        }
    }

    fn backend(devices: &[(&str, bool, bool)]) -> Arc<Devices> {
        Arc::new(Devices(
            devices
                .iter()
                .enumerate()
                .map(|(index, (name, input, output))| AudioDevice {
                    id: u32::try_from(index + 1).expect("small index"),
                    name: (*name).to_owned(),
                    input: *input,
                    output: *output,
                })
                .collect(),
        ))
    }

    #[test]
    fn audio_device_info_serializes_the_legacy_keys() {
        let device = AudioDeviceInfo {
            name: "Test Device".to_owned(),
            device_type: "usb".to_owned(),
            input: true,
            output: false,
        };
        let json = serde_json::to_string(&device).expect("serialize");
        assert!(json.contains("\"name\":\"Test Device\""));
        assert!(json.contains("\"type\":\"usb\""));
        assert!(json.contains("\"input\":true"));
        assert!(json.contains("\"output\":false"));
    }

    #[test]
    fn an_empty_listing_keeps_the_legacy_message() {
        assert_eq!(format_devices_table(&[]), "No audio devices found.");
    }

    #[test]
    fn listing_sorts_by_name_and_detects_types() {
        let backend = backend(&[
            ("USB Audio", true, true),
            ("AirPods Pro", false, true),
            ("MacBook Pro Speakers", false, true),
        ]);
        let devices = list_devices(backend.as_ref(), DeviceFilter::All);
        let names: Vec<&str> = devices.iter().map(|device| device.name.as_str()).collect();
        assert_eq!(names, ["AirPods Pro", "MacBook Pro Speakers", "USB Audio"]);
        assert_eq!(devices[0].device_type, "bluetooth");
        assert_eq!(devices[1].device_type, "builtin");
        assert_eq!(devices[2].device_type, "usb");
    }

    #[test]
    fn filters_use_the_scope_capabilities() {
        let backend = backend(&[
            ("USB Audio", true, true),
            ("Microphone", true, false),
            ("Speakers", false, true),
        ]);
        let input = list_devices(backend.as_ref(), DeviceFilter::InputOnly);
        assert_eq!(input.len(), 2);
        assert!(input.iter().all(|device| device.input));
        let output = list_devices(backend.as_ref(), DeviceFilter::OutputOnly);
        assert_eq!(output.len(), 2);
        assert!(output.iter().all(|device| device.output));
        assert_eq!(list_devices(backend.as_ref(), DeviceFilter::All).len(), 3);
    }

    #[test]
    fn flag_mapping_treats_none_and_both_as_all() {
        assert_eq!(DeviceFilter::from_flags(false, false), DeviceFilter::All);
        assert_eq!(DeviceFilter::from_flags(true, true), DeviceFilter::All);
        assert_eq!(DeviceFilter::from_flags(true, false), DeviceFilter::InputOnly);
        assert_eq!(DeviceFilter::from_flags(false, true), DeviceFilter::OutputOnly);
    }

    #[test]
    fn the_table_matches_the_legacy_layout() {
        let devices = vec![
            AudioDeviceInfo {
                name: "AirPods Pro".to_owned(),
                device_type: "bluetooth".to_owned(),
                input: false,
                output: true,
            },
            AudioDeviceInfo {
                name: "MacBook Pro Speakers".to_owned(),
                device_type: "builtin".to_owned(),
                input: false,
                output: true,
            },
            AudioDeviceInfo {
                name: "USB Audio".to_owned(),
                device_type: "usb".to_owned(),
                input: true,
                output: true,
            },
        ];
        let expected = concat!(
            "Name                  Type        Input  Output\n",
            "--------------------  ----------  -----  ------\n",
            "AirPods Pro           bluetooth   -      Y     \n",
            "MacBook Pro Speakers  builtin     -      Y     \n",
            "USB Audio             usb         Y      Y     \n",
        );
        assert_eq!(format_devices_table(&devices), expected);
    }

    #[test]
    fn a_short_name_still_pads_to_the_name_header() {
        let devices = vec![AudioDeviceInfo {
            name: "X".to_owned(),
            device_type: "usb".to_owned(),
            input: true,
            output: true,
        }];
        let expected = concat!(
            "Name  Type        Input  Output\n",
            "----  ----------  -----  ------\n",
            "X     usb         Y      Y     \n",
        );
        assert_eq!(format_devices_table(&devices), expected);
    }
}
