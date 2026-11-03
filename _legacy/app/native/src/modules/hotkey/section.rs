//! §-key (ISO section key) shortcut handling.
//!
//! `tauri-plugin-global-shortcut` (via `global-hotkey`) cannot express the `§`
//! key: its parser rejects the literal `§` character and its macOS scan-code
//! map lacks the ISO section key (keycode `0x0A`). This module registers `§`
//! shortcuts directly through the Carbon hotkey API (`RegisterEventHotKey`),
//! the same mechanism the plugin uses internally.
//!
//! The physical key that produces `§` is resolved from the active keyboard
//! layout at registration time so bindings work on layouts without a dedicated
//! section key (e.g. ANSI layouts where `§` shares the `6` key) and on layouts
//! where `§` requires a modifier (e.g. Shift on German layouts). When the
//! layout cannot be inspected, the ISO section key is used as a fallback.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use core_foundation::base::{CFType, TCFType};
use core_foundation::data::CFData;
use core_foundation::string::CFString;

use crate::config::ShortcutCommands;
use crate::modules::hotkey::execute_shortcut_commands;

type OSStatus = i32;
type EventParamName = u32;
type EventParamType = u32;
type EventHandlerCallRef = *mut c_void;
type EventRef = *const c_void;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHotKeyRef = *mut c_void;

/// Carbon modifier masks, shared by `RegisterEventHotKey` and `UCKeyTranslate`.
pub(super) const MOD_CMD: u32 = 0x0100;
pub(super) const MOD_SHIFT: u32 = 0x0200;
pub(super) const MOD_OPTION: u32 = 0x0800;
pub(super) const MOD_CONTROL: u32 = 0x1000;

/// Virtual keycode of the ISO section key, the dedicated `§` key on ISO and
/// ABNT2 layouts. Used as the fallback when the layout cannot be inspected.
pub(super) const KEY_ISO_SECTION: u16 = 0x0A;

/// `§` (U+00A7 SECTION SIGN) as produced by `UCKeyTranslate`.
const SECTION_CHAR: u16 = 0x00A7;
/// `kUCKeyActionDown` from `HIToolbox`.
const K_UC_KEY_ACTION_DOWN: u16 = 0;
/// `kUCKeyTranslateNoDeadKeysMask` from `HIToolbox`.
const K_UC_KEY_TRANSLATE_NO_DEAD_KEYS: u32 = 1;
/// Keycodes are 7-bit values (0-127).
const SECTION_KEYCODE_RANGE_END: u16 = 128;
/// `kTISPropertyUnicodeKeyLayoutData` from `HIToolbox`.
const SECTION_KEY_LAYOUT_DATA_PROPERTY: &str = "TISPropertyUnicodeKeyLayoutData";

/// Builds a Carbon `FourCharCode` from its ASCII bytes.
const fn four_char_code(bytes: [u8; 4]) -> u32 { u32::from_be_bytes(bytes) }

const K_EVENT_PARAM_DIRECT_OBJECT: u32 = four_char_code(*b"----");
const TYPE_EVENT_HOT_KEY_ID: u32 = four_char_code(*b"hkid");
const K_EVENT_CLASS_KEYBOARD: u32 = four_char_code(*b"keyb");
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
const K_EVENT_HOT_KEY_RELEASED: u32 = 6;
/// Signature Stache uses to recognise its own Carbon hotkeys.
const SECTION_HOTKEY_SIGNATURE: u32 = four_char_code(*b"stac");
const NO_ERR: i32 = 0;

#[repr(C, packed(2))]
#[derive(Debug, Clone, Copy)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

#[repr(C, packed(2))]
#[derive(Clone, Copy)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetEventParameter(
        event: EventRef,
        name: EventParamName,
        desired_type: EventParamType,
        actual_type: *mut EventParamType,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> OSStatus;
    fn CallNextEventHandler(next_handler: EventHandlerCallRef, event: EventRef) -> OSStatus;
    fn GetEventKind(event: EventRef) -> u32;
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: Option<
            unsafe extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus,
        >,
        num_types: usize,
        types: *const EventTypeSpec,
        user_data: *mut c_void,
        handler_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        hot_key_id: EventHotKeyId,
        target: EventTargetRef,
        options: u32,
        hot_key_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(
        input_source: *const c_void,
        property_key: *const c_void,
    ) -> *mut c_void;
    fn UCKeyTranslate(
        key_layout: *const c_void,
        virtual_key_code: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        translate_options: u32,
        dead_key_state: *mut u32,
        max_string_length: usize,
        actual_string_length: *mut usize,
        unicode_string: *mut u16,
    ) -> OSStatus;
    fn LMGetKbdType() -> u8;
}

pub(super) type SectionBindings = HashMap<u32, SectionBinding>;

#[derive(Debug, Clone)]
pub(super) struct SectionBinding {
    pub raw_shortcut: String,
    pub commands: ShortcutCommands,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SectionShortcut {
    NotSection,
    Binding(u32),
    Invalid(SectionShortcutError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SectionShortcutError {
    UnsupportedShape,
    UnknownModifier(String),
}

impl std::fmt::Display for SectionShortcutError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedShape => formatter.write_str("§ must be the last key in the shortcut"),
            Self::UnknownModifier(modifier) => {
                write!(formatter, "unknown modifier before § key: {modifier}")
            }
        }
    }
}

static BINDINGS: Mutex<Option<SectionBindings>> = Mutex::new(None);
static INITIALIZED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionEventAction {
    Execute,
    Consume,
    Forward,
}

/// Parses a normalized shortcut string and detects `§`-key bindings.
///
/// `CapsLock` shortcuts never reach this parser: the `CapsLock` module handles
/// them first, so `§` is the only supported key here.
pub(super) fn parse_section_shortcut(shortcut: &str) -> SectionShortcut {
    let parts: Vec<&str> = shortcut.split('+').collect();
    let Some((&key, modifiers)) = parts.split_last() else {
        return SectionShortcut::NotSection;
    };

    if key == "§" {
        let mut mods = 0u32;
        for modifier in modifiers {
            match *modifier {
                "Command" => mods |= MOD_CMD,
                "Shift" => mods |= MOD_SHIFT,
                "Option" => mods |= MOD_OPTION,
                "Control" => mods |= MOD_CONTROL,
                _ => {
                    return SectionShortcut::Invalid(SectionShortcutError::UnknownModifier(
                        modifier.to_string(),
                    ));
                }
            }
        }
        return SectionShortcut::Binding(mods);
    }

    if parts.contains(&"§") {
        return SectionShortcut::Invalid(SectionShortcutError::UnsupportedShape);
    }

    SectionShortcut::NotSection
}

/// Registers the given `§` bindings through Carbon hotkeys.
///
/// Fails (returning `false`) when the event handler cannot be installed; a
/// single unavailable shortcut never aborts the remaining registrations.
pub(super) fn start(bindings: SectionBindings) -> bool {
    if bindings.is_empty() {
        return false;
    }

    if let Ok(mut stored_bindings) = BINDINGS.lock() {
        *stored_bindings = Some(bindings);
    } else {
        tracing::warn!("§ keybindings unavailable because binding state is poisoned");
        return false;
    }

    if INITIALIZED.swap(true, Ordering::SeqCst) {
        tracing::debug!("§ keybinding event handler already initialized; bindings updated");
        return true;
    }

    if !install_event_handler() {
        INITIALIZED.store(false, Ordering::SeqCst);
        return false;
    }

    register_bindings();
    true
}

fn install_event_handler() -> bool {
    let event_types = [
        EventTypeSpec {
            event_class: K_EVENT_CLASS_KEYBOARD,
            event_kind: K_EVENT_HOT_KEY_PRESSED,
        },
        EventTypeSpec {
            event_class: K_EVENT_CLASS_KEYBOARD,
            event_kind: K_EVENT_HOT_KEY_RELEASED,
        },
    ];

    let mut handler_ref: EventHandlerRef = unsafe { std::mem::zeroed() };
    let result = unsafe {
        InstallEventHandler(
            GetApplicationEventTarget(),
            Some(section_hotkey_handler),
            event_types.len(),
            event_types.as_ptr(),
            ptr::null_mut(),
            &raw mut handler_ref,
        )
    };

    if result != NO_ERR {
        tracing::warn!("failed to install § keybinding event handler");
        return false;
    }

    tracing::debug!("§ keybinding event handler installed");
    true
}

fn register_bindings() {
    let (keycode, layout_mods) = current_layout_data().map_or_else(
        || {
            tracing::warn!(
                "could not inspect keyboard layout; using ISO section key for § bindings"
            );
            (KEY_ISO_SECTION, 0)
        },
        |layout| resolve_section_key(&layout),
    );

    let Ok(bindings) = BINDINGS.lock() else {
        tracing::warn!("§ keybindings unavailable because binding state is poisoned");
        return;
    };
    let Some(bindings) = bindings.as_ref() else {
        return;
    };

    let mut sorted_bindings: Vec<_> = bindings.iter().collect();
    sorted_bindings.sort_by_key(|(mods, _)| *mods);

    let mut registered = 0usize;
    let mut failed = 0usize;

    for (&mods, binding) in sorted_bindings {
        let hotkey_id = EventHotKeyId {
            signature: SECTION_HOTKEY_SIGNATURE,
            id: mods,
        };
        let mut hotkey_ref: EventHotKeyRef = unsafe { std::mem::zeroed() };
        let result = unsafe {
            RegisterEventHotKey(
                u32::from(keycode),
                mods | layout_mods,
                hotkey_id,
                GetApplicationEventTarget(),
                0,
                &raw mut hotkey_ref,
            )
        };

        if result == NO_ERR {
            registered += 1;
            tracing::debug!(
                shortcut = %binding.raw_shortcut,
                keycode,
                "registered § shortcut"
            );
        } else {
            failed += 1;
            tracing::warn!(
                shortcut = %binding.raw_shortcut,
                keycode,
                error = result,
                "failed to register § shortcut"
            );
        }
    }

    tracing::info!(registered, failed, "finished registering § shortcuts");
}

unsafe extern "C" fn section_hotkey_handler(
    next_handler: EventHandlerCallRef,
    event: EventRef,
    _user_data: *mut c_void,
) -> OSStatus {
    let event_kind = unsafe { GetEventKind(event) };
    let mut hotkey_id: EventHotKeyId = unsafe { std::mem::zeroed() };
    let result = unsafe {
        GetEventParameter(
            event,
            K_EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOT_KEY_ID,
            ptr::null_mut(),
            std::mem::size_of::<EventHotKeyId>(),
            ptr::null_mut(),
            (&raw mut hotkey_id).cast(),
        )
    };

    match section_event_action(event_kind, result, hotkey_id.signature) {
        SectionEventAction::Forward => {
            return unsafe { CallNextEventHandler(next_handler, event) };
        }
        SectionEventAction::Consume => return NO_ERR,
        SectionEventAction::Execute => {}
    }

    let id = hotkey_id.id;
    let Ok(bindings) = BINDINGS.lock() else {
        tracing::warn!("§ keybindings unavailable because binding state is poisoned");
        return NO_ERR;
    };
    let Some(commands) = bindings.as_ref().and_then(|bindings| bindings.get(&id)) else {
        return NO_ERR;
    };

    execute_shortcut_commands(&commands.commands);
    NO_ERR
}

const fn section_event_action(
    event_kind: u32,
    parameter_result: OSStatus,
    signature: u32,
) -> SectionEventAction {
    if parameter_result != NO_ERR || signature != SECTION_HOTKEY_SIGNATURE {
        SectionEventAction::Forward
    } else if event_kind == K_EVENT_HOT_KEY_PRESSED {
        SectionEventAction::Execute
    } else {
        SectionEventAction::Consume
    }
}

/// Copies the `UnicodeKeyLayoutData` of the current keyboard layout.
///
/// The returned layout is used to resolve the physical `§` key; it is copied
/// because the input source only guarantees the data for its own lifetime.
fn current_layout_data() -> Option<Vec<u8>> {
    let source = unsafe { TISCopyCurrentKeyboardLayoutInputSource() };
    if source.is_null() {
        return None;
    }
    let source = unsafe { CFType::wrap_under_create_rule(source.cast()) };

    let property_key = CFString::new(SECTION_KEY_LAYOUT_DATA_PROPERTY);
    let data = unsafe {
        TISGetInputSourceProperty(
            source.as_concrete_TypeRef().cast(),
            property_key.as_concrete_TypeRef().cast(),
        )
    };
    if data.is_null() {
        return None;
    }

    // The layout data is owned by the input source (get rule): copy the bytes
    // before the source can go away.
    let data = unsafe { CFData::wrap_under_get_rule(data.cast()) };
    Some(data.bytes().to_vec())
}

/// Resolves the physical key that produces `§` in the given keyboard layout.
///
/// Returns the virtual keycode and the layout-required modifier bits (e.g.
/// Shift where `§` shares the `3` key, Option on ANSI layouts where it shares
/// `6`). Falls back to the ISO section key with no modifiers when no mapping
/// is found.
fn resolve_section_key(layout: &[u8]) -> (u16, u32) {
    let keyboard_type = u32::from(unsafe { LMGetKbdType() });

    let mut candidates = Vec::new();
    for keycode in 0..SECTION_KEYCODE_RANGE_END {
        for &mods in &SECTION_MODIFIER_COMBOS {
            if translates_to_section(keycode, mods, keyboard_type, layout) {
                candidates.push((keycode, mods));
            }
        }
    }

    select_section_candidate(&candidates).unwrap_or((KEY_ISO_SECTION, 0))
}

/// Modifier combinations probed when resolving the `§` key.
const SECTION_MODIFIER_COMBOS: [u32; 8] = [
    0,
    MOD_SHIFT,
    MOD_OPTION,
    MOD_SHIFT | MOD_OPTION,
    MOD_CONTROL,
    MOD_CONTROL | MOD_SHIFT,
    MOD_CONTROL | MOD_OPTION,
    MOD_CONTROL | MOD_SHIFT | MOD_OPTION,
];

/// Picks the candidate requiring the fewest modifier keys, breaking ties
/// deterministically by modifier bits then keycode.
fn select_section_candidate(candidates: &[(u16, u32)]) -> Option<(u16, u32)> {
    candidates
        .iter()
        .copied()
        .min_by_key(|&(keycode, mods)| (mods.count_ones(), mods, keycode))
}

fn translates_to_section(keycode: u16, mods: u32, keyboard_type: u32, layout: &[u8]) -> bool {
    let mut dead_key_state = 0u32;
    let mut chars = [0u16; 4];
    let mut length = 0usize;

    let status = unsafe {
        UCKeyTranslate(
            layout.as_ptr().cast(),
            keycode,
            K_UC_KEY_ACTION_DOWN,
            // `UCKeyTranslate` reads the modifier state from the high byte of
            // the Carbon event-modifiers word, so the masks shared with
            // `RegisterEventHotKey` (e.g. `MOD_SHIFT` = 0x0200) must be
            // shifted right by 8 bits here.
            mods >> 8,
            keyboard_type,
            K_UC_KEY_TRANSLATE_NO_DEAD_KEYS,
            &raw mut dead_key_state,
            chars.len(),
            &raw mut length,
            chars.as_mut_ptr(),
        )
    };

    status == NO_ERR && length > 0 && chars[0] == SECTION_CHAR
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_section_key() {
        assert_eq!(parse_section_shortcut("§"), SectionShortcut::Binding(0));
    }

    #[test]
    fn parse_section_shortcut_with_modifiers() {
        assert_eq!(
            parse_section_shortcut("Command+§"),
            SectionShortcut::Binding(MOD_CMD)
        );
        assert_eq!(
            parse_section_shortcut("Control+Option+§"),
            SectionShortcut::Binding(MOD_CONTROL | MOD_OPTION)
        );
        assert_eq!(
            parse_section_shortcut("Shift+Control+Option+Command+§"),
            SectionShortcut::Binding(MOD_SHIFT | MOD_CONTROL | MOD_OPTION | MOD_CMD)
        );
    }

    #[test]
    fn parse_non_section_shortcut() {
        assert_eq!(
            parse_section_shortcut("Command+Control+S"),
            SectionShortcut::NotSection
        );
        assert_eq!(parse_section_shortcut("Command+S"), SectionShortcut::NotSection);
        assert_eq!(parse_section_shortcut(""), SectionShortcut::NotSection);
    }

    #[test]
    fn reject_section_key_not_last() {
        assert_eq!(
            parse_section_shortcut("§+Command"),
            SectionShortcut::Invalid(SectionShortcutError::UnsupportedShape)
        );
        assert_eq!(
            parse_section_shortcut("Command+§+Shift"),
            SectionShortcut::Invalid(SectionShortcutError::UnsupportedShape)
        );
    }

    #[test]
    fn reject_unknown_section_modifier() {
        assert_eq!(
            parse_section_shortcut("Fn+§"),
            SectionShortcut::Invalid(SectionShortcutError::UnknownModifier("Fn".to_string()))
        );
    }

    #[test]
    fn select_section_candidate_prefers_fewest_modifiers() {
        let candidates = vec![(10, MOD_SHIFT), (10, 0), (22, MOD_OPTION)];
        assert_eq!(select_section_candidate(&candidates), Some((10, 0)));
    }

    #[test]
    fn select_section_candidate_tiebreak_prefers_lower_mods_and_keycode() {
        let candidates = vec![(22, MOD_OPTION), (10, MOD_OPTION), (10, MOD_SHIFT)];
        assert_eq!(select_section_candidate(&candidates), Some((10, MOD_SHIFT)));
    }

    #[test]
    fn select_section_candidate_empty_has_no_candidate() {
        assert_eq!(select_section_candidate(&[]), None);
    }

    #[test]
    fn foreign_carbon_hotkeys_are_forwarded() {
        assert_eq!(
            section_event_action(K_EVENT_HOT_KEY_PRESSED, NO_ERR, four_char_code(*b"htrs")),
            SectionEventAction::Forward
        );
        assert_eq!(
            section_event_action(K_EVENT_HOT_KEY_RELEASED, NO_ERR, four_char_code(*b"htrs")),
            SectionEventAction::Forward
        );
    }

    #[test]
    fn section_carbon_hotkeys_are_owned() {
        assert_eq!(
            section_event_action(K_EVENT_HOT_KEY_PRESSED, NO_ERR, SECTION_HOTKEY_SIGNATURE),
            SectionEventAction::Execute
        );
        assert_eq!(
            section_event_action(K_EVENT_HOT_KEY_RELEASED, NO_ERR, SECTION_HOTKEY_SIGNATURE),
            SectionEventAction::Consume
        );
    }

    #[test]
    fn malformed_carbon_hotkeys_are_forwarded() {
        assert_eq!(
            section_event_action(K_EVENT_HOT_KEY_PRESSED, -1, SECTION_HOTKEY_SIGNATURE),
            SectionEventAction::Forward
        );
    }
}
