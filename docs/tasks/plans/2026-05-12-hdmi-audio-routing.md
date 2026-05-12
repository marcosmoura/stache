# HDMI Audio Auto-Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route audio output through HDMI when a user plugs in an HDMI display and the current output is built-in speakers.

**Architecture:** CoreAudio `kAudioDeviceTransportTypeHDMI` detects HDMI devices via transport type (with name fallback). A new priority rule in `resolve_output_device()` selects HDMI only when current output is `BuiltIn`. Input routing and user-selected higher-priority devices (AirPlay, Bluetooth, configured priorities) are unaffected.

**Tech Stack:** Rust, coreaudio-rs 0.14, objc2-core-audio 0.3

---

### Task 1: Add Hdmi variant and detection to `device.rs`

**Files:**

- Modify: `app/native/src/modules/audio/device.rs`
- No additional files needed for this task

- [ ] **Step 1: Add `Hdmi` variant to `AudioDeviceType` enum**

```rust
pub enum AudioDeviceType {
    AirPlay,
    Bluetooth,
    Virtual,
    Usb,
    BuiltIn,
    Hdmi,
    Other,
}
```

- [ ] **Step 2: Add `get_device_transport_type` to imports**

```rust
use coreaudio::audio_unit::macos_helpers::{
    get_audio_device_ids, get_audio_device_supports_scope, get_default_device_id, get_device_name,
    get_device_transport_type,
};
```

- [ ] **Step 3: Update `AudioDeviceType::as_str()`**

```rust
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
```

- [ ] **Step 4: Add `is_hdmi()` and `is_builtin()` methods to `AudioDevice`**

```rust
use objc2_core_audio::kAudioDeviceTransportTypeHDMI;

impl AudioDevice {
    pub fn is_hdmi(&self) -> bool {
        if let Ok(transport) = get_device_transport_type(self.id) {
            if transport == kAudioDeviceTransportTypeHDMI {
                return true;
            }
        }
        self.name_contains("hdmi")
    }

    pub fn is_builtin(&self) -> bool {
        self.device_type() == AudioDeviceType::BuiltIn
    }
}
```

- [ ] **Step 5: Write failing tests for HDMI detection**

```rust
#[test]
fn audio_device_type_detect_hdmi_by_name() {
    assert_eq!(
        AudioDeviceType::detect("HDMI Output"),
        AudioDeviceType::Other  // Name-only detect can't detect HDMI without transport type
    );
}

#[test]
fn audio_device_is_hdmi_by_name_fallback() {
    let device = AudioDevice {
        id: 99999, // fake ID, transport check will fail, tests fallback to name
        name: "HDMI Output".to_string(),
    };
    assert!(device.is_hdmi());
}

#[test]
fn audio_device_is_hdmi_returns_false_for_non_hdmi() {
    let device = AudioDevice {
        id: 99999,
        name: "MacBook Pro Speakers".to_string(),
    };
    assert!(!device.is_hdmi());
}

#[test]
fn audio_device_is_builtin_true_for_builtin() {
    let device = AudioDevice {
        id: 1,
        name: "MacBook Pro Speakers".to_string(),
    };
    assert!(device.is_builtin());
}

#[test]
fn audio_device_is_builtin_false_for_external() {
    let device = AudioDevice {
        id: 99999,
        name: "External Speakers".to_string(),
    };
    assert!(!device.is_builtin());
}
```

- [ ] **Step 6: Run tests to verify they fail as constructed**

Run: `cargo test -p stache --lib modules::audio::device::tests -- --show-output`
Expected: `audio_device_is_hdmi_*` and `audio_device_is_builtin_*` tests should compile and pass/fail as designed. `audio_device_type_detect_hdmi_by_name` should show that name-only detect returns `Other` (expected behavior).

- [ ] **Step 7: Run all tests to verify nothing is broken**

Run: `cargo test -p stache --lib modules::audio -- --show-output`
Expected: All tests pass including existing tests.

---

### Task 2: Add Hdmi audio type formatting for `as_str`

This was completed in Task 1 Step 3 (`as_str` returns `"hdmi"`). List/display is handled automatically since `list.rs` uses `AudioDeviceInfo.device_type` which derives from `AudioDeviceType::detect()` and `as_str()`. No changes needed to `list.rs`.

---

### Task 3: Add HDMI priority rule to `priority.rs`

**Files:**

- Modify: `app/native/src/modules/audio/priority.rs`

- [ ] **Step 1: Write failing test for built-in → HDMI routing**

```rust
#[test]
fn output_selects_hdmi_when_current_is_builtin_and_hdmi_available() {
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
            name: "HDMI Output".to_string(),
        },
        AudioDevice {
            id: 3,
            name: "AirPods Pro".to_string(),
        },
    ];

    // Built-in current + HDMI available → should select HDMI (name fallback matches "HDMI Output")
    let target = resolve_output_device(&current, &devices, &config, false);
    assert!(target.is_some());
    assert_eq!(target.unwrap().id, 2);
}
```

```rust
#[test]
fn output_keeps_airplay_even_when_hdmi_available() {
    let config = create_test_config();
    let current = AudioDevice {
        id: 3,
        name: "Living Room AirPlay".to_string(),
    };

    let devices = vec![
        AudioDevice {
            id: 1,
            name: "MacBook Pro Speakers".to_string(),
        },
        AudioDevice {
            id: 2,
            name: "HDMI Output".to_string(),
        },
        AudioDevice {
            id: 3,
            name: "Living Room AirPlay".to_string(),
        },
    ];

    // AirPlay current + HDMI available → should NOT switch to HDMI
    let target = resolve_output_device(&current, &devices, &config, false);
    assert!(target.is_some());
    assert_eq!(target.unwrap().id, 3);
}
```

```rust
#[test]
fn output_stays_on_builtin_when_no_hdmi_available() {
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
            id: 3,
            name: "AirPods Pro".to_string(),
        },
    ];

    // Built-in current + no HDMI → falls through to config priority (AirPods Pro)
    let target = resolve_output_device(&current, &devices, &config, false);
    assert!(target.is_some());
    assert_eq!(target.unwrap().id, 3);
}
```

```rust
#[test]
fn output_keeps_configured_device_when_not_builtin_even_with_hdmi() {
    let config = create_test_config();
    let current = AudioDevice {
        id: 2,
        name: "AirPods Pro".to_string(),
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
            name: "HDMI Output".to_string(),
        },
    ];

    // USB/AirPods current + HDMI available → should NOT switch, keep configured priority
    let target = resolve_output_device(&current, &devices, &config, false);
    assert!(target.is_some());
    assert_eq!(target.unwrap().id, 2);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p stache --lib modules::audio::priority::tests -- --show-output`
Expected: The new tests fail because `resolve_output_device` doesn't yet have the HDMI rule.

- [ ] **Step 3: Implement the HDMI rule in `resolve_output_device`**

Add a new rule between steps 2 and 3 (between "don't switch away from AirPlay" and "config priority"):

```rust
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

    // 3. When current output is built-in, switch to HDMI if available
    if current.is_builtin() {
        if let Some(hdmi) = devices.iter().find(|d| d.is_hdmi()) {
            return Some(hdmi);
        }
    }

    // 4. Check devices in config priority order
    for priority_device in &config.output {
        if let Some(device) = find_device_by_priority(devices, priority_device) {
            return Some(device);
        }
    }

    // 5. Fallback to MacBook Pro speakers
    find_device_by_name(devices, "MacBook Pro")
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p stache --lib modules::audio::priority::tests -- --show-output`
Expected: All 11 priority tests pass (7 existing + 4 new).

- [ ] **Step 5: Run the full audio test suite**

Run: `cargo test -p stache --lib modules::audio -- --show-output`
Expected: All audio module tests pass.
