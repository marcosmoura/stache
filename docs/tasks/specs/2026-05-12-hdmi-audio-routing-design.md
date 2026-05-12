# HDMI Audio Auto-Routing

## Problem

When a user plugs in an HDMI display with audio capabilities (e.g. a monitor or TV with speakers), Stache should automatically route audio output through the HDMI device. This should only happen when the current output is the built-in speakers — higher-priority user selections (AirPlay, AirPods, configured devices) should be preserved.

## Design

### HDMI Device Detection

HDMI audio devices are detected using CoreAudio's `kAudioDeviceTransportTypeHDMI` via the `get_device_transport_type()` helper from `coreaudio-rs`. A name-based fallback is used for testability and edge cases.

### Changes to `device.rs`

- Add `AudioDeviceType::Hdmi` variant.
- Add `is_hdmi()` method to `AudioDevice` and `AudioDeviceType::detect()` to recognise HDMI transport via CoreAudio.
- Add `is_builtin()` method to `AudioDevice`.
- Extend `AudioDeviceType::as_str()` to return `"hdmi"`.

### Changes to `priority.rs`

Update `resolve_output_device()` to add a new rule between the AirPlay rules and the config priority check:

1. _AirPlay when screen mirroring is active_ — unchanged
2. _Don't switch away from AirPlay_ — unchanged
3. **NEW: If current output is `BuiltIn` and any HDMI output device exists, select the first available HDMI device.**
4. _Config priority list_ — unchanged
5. _Fallback to MacBook Pro speakers_ — unchanged

### Changes to `list.rs`

Update `AudioDeviceInfo` display and formatting to handle the new `hdmi` type.

### Input Routing

No changes. HDMI is output-only, and input routing should not be affected.

### CLI

`stache audio list` will show HDMI devices with type `hdmi`.

### Tests

- Unit tests for `AudioDeviceType::detect()` with HDMI transport.
- Unit tests for HDMI name fallback detection.
- Unit tests for `resolve_output_device()` with:
  - Built-in current → HDMI available → selects HDMI.
  - AirPlay current → HDMI available → keeps AirPlay (no switch).
  - USB/AirPods current → HDMI available → keeps configured priority device.
  - Built-in current → no HDMI → falls through to config priority.
