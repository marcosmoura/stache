//! The pure `§` resolver.
//!
//! Ported from the archived hotkey module: probe key codes `0..128` against the
//! modifier combinations {none, Shift, Option, Shift+Option} each optionally with
//! Control, with dead-key state reset for every probe. Among the keys that type
//! `§` choose the fewest required modifiers, then the lowest Carbon modifier
//! bits, then the lowest key code. The required modifiers are OR-ed with the
//! configured ones. With no readable layout, or no candidate, the physical ISO
//! key `0x0A` is used and a diagnostic says that this does not guarantee `§`.

use stache_sdk::input::ModifierFlags;

use super::{KeyLayout, LayoutRead};

/// Carbon Command mask, shared by `RegisterEventHotKey` and `UCKeyTranslate`.
pub const CARBON_COMMAND: u32 = 0x0100;
/// Carbon Shift mask.
pub const CARBON_SHIFT: u32 = 0x0200;
/// Carbon Option mask.
pub const CARBON_OPTION: u32 = 0x0800;
/// Carbon Control mask.
pub const CARBON_CONTROL: u32 = 0x1000;

/// The physical ISO section key, used when the layout cannot answer.
pub const ISO_SECTION_KEY: u16 = 0x0A;

/// `§` (U+00A7) as typed by the layout.
const SECTION_CHARACTER: u16 = 0x00A7;

/// Key codes are 7-bit values.
const KEY_CODE_COUNT: u16 = 128;

/// Modifier combinations probed, in the archived order. Command is never probed.
const PROBES: [u32; 8] = [
    0,
    CARBON_SHIFT,
    CARBON_OPTION,
    CARBON_SHIFT | CARBON_OPTION,
    CARBON_CONTROL,
    CARBON_CONTROL | CARBON_SHIFT,
    CARBON_CONTROL | CARBON_OPTION,
    CARBON_CONTROL | CARBON_SHIFT | CARBON_OPTION,
];

/// Converts SDK modifier flags to a Carbon mask. Caps Lock is dropped (it never
/// takes part in a physical chord); Function cannot be expressed and gives `None`.
#[must_use]
pub fn carbon_modifiers(flags: ModifierFlags) -> Option<u32> {
    if flags.contains(ModifierFlags::FUNCTION) {
        return None;
    }
    let mut mask = 0;
    for (flag, carbon) in [
        (ModifierFlags::COMMAND, CARBON_COMMAND),
        (ModifierFlags::SHIFT, CARBON_SHIFT),
        (ModifierFlags::OPTION, CARBON_OPTION),
        (ModifierFlags::CONTROL, CARBON_CONTROL),
    ] {
        if flags.contains(flag) {
            mask |= carbon;
        }
    }
    Some(mask)
}

/// Converts a Carbon mask to SDK modifier flags; unknown bits are dropped.
#[must_use]
pub fn section_modifiers(carbon: u32) -> ModifierFlags {
    let mut flags = ModifierFlags::NONE;
    for (flag, mask) in [
        (ModifierFlags::COMMAND, CARBON_COMMAND),
        (ModifierFlags::SHIFT, CARBON_SHIFT),
        (ModifierFlags::OPTION, CARBON_OPTION),
        (ModifierFlags::CONTROL, CARBON_CONTROL),
    ] {
        if carbon & mask != 0 {
            flags = flags | flag;
        }
    }
    flags
}

/// Why the ISO fallback key was used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    /// The layout data was absent or unreadable.
    LayoutUnavailable(String),
    /// The layout was readable but no key types `§`.
    NoCandidate,
}

/// Where a resolved `§` chord came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionSource {
    /// The layout types `§` with this key; `required` are the extra modifiers.
    Layout {
        /// Modifiers the layout needs in addition to the configured ones.
        required: ModifierFlags,
    },
    /// The physical ISO key `0x0A` was used because the layout could not answer.
    Fallback(FallbackReason),
}

impl SectionSource {
    /// A user-facing note when this is not a real layout match.
    #[must_use]
    pub fn diagnostic(&self) -> Option<String> {
        let Self::Fallback(reason) = self else {
            return None;
        };
        let why = match reason {
            FallbackReason::LayoutUnavailable(detail) => {
                format!("keyboard layout data is unavailable ({detail})")
            }
            FallbackReason::NoCandidate => "no key on the current layout types `§`".to_owned(),
        };
        Some(format!(
            "{why}; using the physical ISO section key 0x0A, which does not guarantee `§` on ANSI or non-matching layouts"
        ))
    }
}

/// The layout's answer to "which key types `§`", independent of configured modifiers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutResolution {
    /// The virtual key code.
    pub key_code: u16,
    /// Where it came from.
    pub source: SectionSource,
    required: ModifierFlags,
}

impl LayoutResolution {
    /// The chord for a binding with `configured` modifiers: the required layout
    /// modifiers are OR-ed with the configured ones.
    #[must_use]
    pub fn chord(&self, configured: ModifierFlags) -> (u16, ModifierFlags) {
        (self.key_code, configured | self.required)
    }
}

/// Picks the candidate with the fewest modifiers, then lowest Carbon bits, then
/// lowest key code.
#[must_use]
pub fn select_candidate(candidates: &[(u16, u32)]) -> Option<(u16, u32)> {
    candidates
        .iter()
        .copied()
        .min_by_key(|&(key_code, mods)| (mods.count_ones(), mods, key_code))
}

/// Resolves the physical key that types `§` on `layout`.
#[must_use]
pub fn resolve(layout: &LayoutRead) -> LayoutResolution {
    let fallback = |reason| LayoutResolution {
        key_code: ISO_SECTION_KEY,
        source: SectionSource::Fallback(reason),
        required: ModifierFlags::NONE,
    };
    let table: &dyn KeyLayout = match layout {
        LayoutRead::Available(table) => table.as_ref(),
        LayoutRead::Unavailable(reason) => {
            return fallback(FallbackReason::LayoutUnavailable(reason.clone()));
        }
    };
    let mut candidates = Vec::new();
    for key_code in 0..KEY_CODE_COUNT {
        for &mods in &PROBES {
            if table.translate(key_code, mods) == Some(SECTION_CHARACTER) {
                candidates.push((key_code, mods));
            }
        }
    }
    select_candidate(&candidates).map_or_else(
        || fallback(FallbackReason::NoCandidate),
        |(key_code, mods)| {
            let required = section_modifiers(mods);
            LayoutResolution {
                key_code,
                source: SectionSource::Layout { required },
                required,
            }
        },
    )
}
