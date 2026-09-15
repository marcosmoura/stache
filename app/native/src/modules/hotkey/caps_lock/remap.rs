//! HID-level Caps Lock → F18 remapping backed by `hidutil`.
//!
//! A `CGEventTap` cannot reliably suppress Caps Lock's native toggle/LED
//! behavior, so Stache remaps the physical Caps Lock key to F18 at the HID
//! layer (via `hidutil property --set UserKeyMapping`). macOS then never sees
//! Caps Lock: the LED stays off and capitalization never toggles. The event tap
//! treats the incoming F18 events as the `CapsLock` pseudo-modifier.
//!
//! The remapping is applied only while Stache owns Caps Lock bindings and is
//! restored on orderly shutdown. Because the mapping is global and survives
//! until reboot if Stache dies uncleanly, ownership state is persisted under
//! the Stache config directory so a later launch can recover it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// HID usage for the physical Caps Lock key.
pub(super) const HID_USAGE_CAPS_LOCK: u64 = 0x7_0000_0039;
/// HID usage for F18, Stache's internal Caps Lock surrogate.
pub(super) const HID_USAGE_F18: u64 = 0x7_0000_006D;

/// One entry in the `UserKeyMapping` array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HidMapping {
    src: u64,
    dst: u64,
}

/// Persisted ownership state, written after the remap is applied.
#[derive(Debug, Serialize, Deserialize)]
struct RemapState {
    /// Destination Stache replaced for the Caps Lock source (if any).
    previous_caps_usage: Option<u64>,
}

/// Applies Stache's Caps Lock → F18 remap, preserving unrelated mappings.
///
/// Recovers from a previous unclean shutdown first, then snapshots any existing
/// Caps Lock mapping before replacing it. Returns `false` if the mapping could
/// not be applied (in which case "owning" Caps Lock is not safe).
pub(super) fn apply() -> bool {
    recover_stale_remap();

    let Some(current) = read_current_mappings() else {
        tracing::warn!("CapsLock remap unavailable because hidutil could not read UserKeyMapping");
        return false;
    };

    let (mappings, previous_caps_usage) = applied_mappings(current);
    if !write_mappings(&mappings) {
        tracing::warn!("CapsLock remap failed to apply - caps lock is not owned by Stache");
        return false;
    }

    persist_state(previous_caps_usage);
    tracing::info!("Caps Lock remapped to F18 for CapsLock+<key> bindings");
    true
}

/// Restores the pre-Stache Caps Lock mapping after an orderly shutdown.
///
/// Only touches the mapping if Stache's F18 mapping is still active; an
/// externally changed Caps Lock mapping is left untouched.
pub(super) fn restore() {
    let Some(state_file) = state_file_path() else {
        return;
    };
    let Some(state) = load_state(&state_file) else {
        return;
    };
    // Remove the ownership file first so an interrupted restore is not retried
    // pointlessly on a later launch.
    let _ = fs::remove_file(&state_file);

    let Some(current) = read_current_mappings() else {
        return;
    };
    let Some(mappings) = restored_mappings(current, state.previous_caps_usage, true) else {
        tracing::debug!("CapsLock remap changed externally; leaving current mapping untouched");
        return;
    };

    if write_mappings(&mappings) {
        tracing::info!("Caps Lock mapping restored");
    } else {
        tracing::warn!("failed to restore Caps Lock mapping");
    }
}

/// Reclaims a mapping left behind by an unclean shutdown.
///
/// Runs before applying a fresh remap: if the persisted state matches the
/// currently active mapping, the original mapping is restored first.
fn recover_stale_remap() {
    let Some(state_file) = state_file_path() else {
        return;
    };
    if !state_file.exists() {
        return;
    }
    let Some(state) = load_state(&state_file) else {
        return;
    };
    let _ = fs::remove_file(&state_file);

    let Some(current) = read_current_mappings() else {
        return;
    };
    let ours_present =
        current.iter().any(|m| m.src == HID_USAGE_CAPS_LOCK && m.dst == HID_USAGE_F18);
    if !ours_present {
        tracing::debug!("stale CapsLock remap state found but mapping is no longer ours; ignoring");
        return;
    }

    let Some(mappings) = restored_mappings(current, state.previous_caps_usage, true) else {
        return;
    };
    if write_mappings(&mappings) {
        tracing::info!("restored Caps Lock mapping after unclean shutdown");
    } else {
        tracing::warn!("failed to restore Caps Lock mapping after unclean shutdown");
    }
}

/// Returns the mappings to apply, and the destination Stache replaced for Caps
/// Lock (so it can be restored later).
fn applied_mappings(current: Vec<HidMapping>) -> (Vec<HidMapping>, Option<u64>) {
    let previous_caps_usage = current.iter().find(|m| m.src == HID_USAGE_CAPS_LOCK).map(|m| m.dst);

    let mut mappings: Vec<HidMapping> =
        current.into_iter().filter(|m| m.src != HID_USAGE_CAPS_LOCK).collect();
    mappings.push(HidMapping {
        src: HID_USAGE_CAPS_LOCK,
        dst: HID_USAGE_F18,
    });

    (mappings, previous_caps_usage)
}

/// Returns the mappings to restore, or `None` when the current mapping was not
/// applied by Stache (so nothing should be touched).
fn restored_mappings(
    current: Vec<HidMapping>,
    previous_caps_usage: Option<u64>,
    ours_present: bool,
) -> Option<Vec<HidMapping>> {
    if !ours_present {
        return None;
    }

    let mut mappings: Vec<HidMapping> = current
        .into_iter()
        .filter(|m| !(m.src == HID_USAGE_CAPS_LOCK && m.dst == HID_USAGE_F18))
        .collect();
    if let Some(previous_dst) = previous_caps_usage
        && !mappings.iter().any(|m| m.src == HID_USAGE_CAPS_LOCK)
    {
        mappings.push(HidMapping {
            src: HID_USAGE_CAPS_LOCK,
            dst: previous_dst,
        });
    }
    Some(mappings)
}

fn read_current_mappings() -> Option<Vec<HidMapping>> {
    let output = Command::new("/usr/bin/hidutil")
        .args(["property", "--get", "UserKeyMapping"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(parse_mappings(&text))
}

fn write_mappings(mappings: &[HidMapping]) -> bool {
    let entries: Vec<serde_json::Value> = mappings
        .iter()
        .map(|m| {
            serde_json::json!({
                "HIDKeyboardModifierMappingSrc": m.src,
                "HIDKeyboardModifierMappingDst": m.dst,
            })
        })
        .collect();
    let payload = serde_json::json!({ "UserKeyMapping": entries }).to_string();

    match Command::new("/usr/bin/hidutil").args(["property", "--set", &payload]).output() {
        Ok(output) => {
            if !output.status.success() {
                tracing::warn!(
                    stderr = %String::from_utf8_lossy(&output.stderr),
                    "hidutil property --set failed"
                );
            }
            output.status.success()
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to run hidutil property --set");
            false
        }
    }
}

/// Parses `hidutil property --get UserKeyMapping` output (`OpenStep` plist text).
fn parse_mappings(text: &str) -> Vec<HidMapping> {
    if text.contains("(null)") {
        return Vec::new();
    }

    let pattern = Regex::new(
        r#"HIDKeyboardModifierMappingSrc\s*=\s*"?(\d+)"?[^}]*?HIDKeyboardModifierMappingDst\s*=\s*"?(\d+)"?"#,
    )
    .expect("valid hidutil mapping regex");
    pattern
        .captures_iter(text)
        .filter_map(|captures| {
            let src = captures.get(1)?.as_str().parse::<u64>().ok()?;
            let dst = captures.get(2)?.as_str().parse::<u64>().ok()?;
            Some(HidMapping { src, dst })
        })
        .collect()
}

fn state_file_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("stache").join("caps_lock_remap.json"))
}

fn persist_state(previous_caps_usage: Option<u64>) {
    let Some(path) = state_file_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(err) = fs::create_dir_all(parent) {
        tracing::warn!(error = %err, "failed to create state directory for CapsLock remap");
        return;
    }

    match serde_json::to_vec_pretty(&RemapState { previous_caps_usage }) {
        Ok(bytes) => {
            if let Err(err) = fs::write(&path, bytes) {
                tracing::warn!(
                    error = %err,
                    path = %path.display(),
                    "failed to persist CapsLock remap state"
                );
            }
        }
        Err(err) => tracing::warn!(error = %err, "failed to serialize CapsLock remap state"),
    }
}

fn load_state(path: &Path) -> Option<RemapState> {
    let bytes = fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(state) => Some(state),
        Err(err) => {
            tracing::warn!(error = %err, "ignoring unreadable CapsLock remap state");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC_CAPS: u64 = 0x7_0000_0039;
    const DST_F18: u64 = 0x7_0000_006D;

    fn caps_to(dst: u64) -> HidMapping { HidMapping { src: SRC_CAPS, dst } }

    #[test]
    fn parse_mappings_handles_null_output() {
        assert!(parse_mappings("(null)").is_empty());
    }

    #[test]
    fn parse_mappings_reads_openstep_plist_output() {
        let output = r"{
    UserKeyMapping =     (
                {
            HIDKeyboardModifierMappingSrc = 30064771129;
            HIDKeyboardModifierMappingDst = 30064771181;
        },
                {
            HIDKeyboardModifierMappingSrc = 30064771136;
            HIDKeyboardModifierMappingDst = 30064771137;
        }
    );
}
";

        let mappings = parse_mappings(output);

        assert_eq!(mappings, vec![
            HidMapping {
                src: 30_064_771_129,
                dst: 30_064_771_181,
            },
            HidMapping {
                src: 30_064_771_136,
                dst: 30_064_771_137,
            },
        ]);
    }

    #[test]
    fn parse_mappings_handles_quoted_numbers() {
        let output = r#"{
    UserKeyMapping =     (
                {
            HIDKeyboardModifierMappingSrc = "30064771129";
            HIDKeyboardModifierMappingDst = "30064771181";
        }
    );
}
"#;

        assert_eq!(parse_mappings(output), vec![HidMapping {
            src: 30_064_771_129,
            dst: 30_064_771_181,
        }]);
    }

    #[test]
    fn applied_mappings_preserves_unrelated_and_captures_previous_caps() {
        let unrelated = HidMapping { src: 100, dst: 200 };
        let existing_caps = caps_to(300);

        let (mappings, previous) = applied_mappings(vec![unrelated, existing_caps]);

        assert_eq!(previous, Some(300));
        assert_eq!(mappings, vec![unrelated, HidMapping {
            src: SRC_CAPS,
            dst: DST_F18,
        }]);
    }

    #[test]
    fn applied_mappings_without_previous_caps_mapping() {
        let unrelated = HidMapping { src: 100, dst: 200 };

        let (mappings, previous) = applied_mappings(vec![unrelated]);

        assert_eq!(previous, None);
        assert_eq!(mappings, vec![unrelated, HidMapping {
            src: SRC_CAPS,
            dst: DST_F18,
        }]);
    }

    #[test]
    fn restored_mappings_restores_previous_caps_mapping() {
        let unrelated = HidMapping { src: 100, dst: 200 };
        let current = vec![unrelated, caps_to(DST_F18)];

        let mappings = restored_mappings(current, Some(300), true).expect("ours present");

        assert_eq!(mappings, vec![unrelated, HidMapping { src: SRC_CAPS, dst: 300 }]);
    }

    #[test]
    fn restored_mappings_without_previous_caps_mapping_removes_ours() {
        let unrelated = HidMapping { src: 100, dst: 200 };
        let current = vec![unrelated, caps_to(DST_F18)];

        let mappings = restored_mappings(current, None, true).expect("ours present");

        assert_eq!(mappings, vec![unrelated]);
    }

    #[test]
    fn restored_mappings_preserves_external_caps_change() {
        let external_caps = caps_to(400);
        let current = vec![caps_to(DST_F18), external_caps];

        let mappings = restored_mappings(current, None, true).expect("ours present");

        assert_eq!(mappings, vec![external_caps]);
    }

    #[test]
    fn restored_mappings_refuses_to_touch_non_owned_mapping() {
        let current = vec![caps_to(400)];

        assert_eq!(restored_mappings(current, None, false), None);
    }
}
