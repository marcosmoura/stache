//! The real layout source: Text Input Sources (TIS) and `UCKeyTranslate`.
//!
//! Main-thread only. The layout bytes are copied out of the input source, so the
//! [`TisLayout`] table owns its data for its whole lifetime and the input source
//! reference is released before the table is returned.

use std::collections::hash_map::DefaultHasher;
use std::ffi::c_void;
use std::hash::{Hash, Hasher};

use core_foundation::base::{CFType, TCFType};
use core_foundation::data::{CFData, CFDataRef};
use core_foundation::string::{CFString, CFStringRef};

use super::{KeyLayout, LayoutId, LayoutRead};
use crate::main_thread::is_main_thread;

type OsStatus = i32;

/// `kUCKeyActionDown`.
const KEY_ACTION_DOWN: u16 = 0;
/// `kUCKeyTranslateNoDeadKeysMask`.
const NO_DEAD_KEYS: u32 = 1;

#[allow(non_upper_case_globals, reason = "Apple constant names")]
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    /// `kTISPropertyUnicodeKeyLayoutData`: the `uchr` table as `CFData`.
    static kTISPropertyUnicodeKeyLayoutData: CFStringRef;
    /// `kTISPropertyInputSourceID`: the input source's stable identifier.
    static kTISPropertyInputSourceID: CFStringRef;

    /// Copies (create rule) the current keyboard-layout input source, or null.
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *const c_void;

    /// Reads a property (get rule) of an input source, or null.
    fn TISGetInputSourceProperty(input_source: *const c_void, key: CFStringRef) -> *const c_void;

    /// Translates a virtual key code through a `uchr` table.
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
    ) -> OsStatus;

    /// The current keyboard type constant.
    fn LMGetKbdType() -> u8;
}

/// A copied `uchr` layout table.
pub struct TisLayout {
    identity: LayoutId,
    data: Vec<u8>,
    keyboard_type: u32,
}

impl KeyLayout for TisLayout {
    fn identity(&self) -> LayoutId { self.identity }

    fn translate(&self, key_code: u16, carbon_modifiers: u32) -> Option<u16> {
        // Dead-key state is reset for every probe.
        let mut dead_key_state = 0u32;
        let mut chars = [0u16; 4];
        let mut length = 0usize;
        // SAFETY: `self.data` is an owned buffer that holds the copied `uchr`
        // table and outlives the call; `UCKeyTranslate` only reads it. The out
        // pointers name live locals sized as declared (`chars.len()` units). The
        // modifier state is the Carbon mask's high byte, as the API expects.
        let status = unsafe {
            UCKeyTranslate(
                self.data.as_ptr().cast(),
                key_code,
                KEY_ACTION_DOWN,
                carbon_modifiers >> 8,
                self.keyboard_type,
                NO_DEAD_KEYS,
                &raw mut dead_key_state,
                chars.len(),
                &raw mut length,
                chars.as_mut_ptr(),
            )
        };
        (status == 0 && length > 0).then_some(chars[0])
    }
}

/// Reads and copies the current keyboard layout. Must run on the main thread.
pub fn read_current_layout() -> LayoutRead {
    if !is_main_thread() {
        return LayoutRead::Unavailable("the layout was read off the main thread".to_owned());
    }
    // SAFETY: no arguments; returns an owned (create rule) reference or null.
    let source = unsafe { TISCopyCurrentKeyboardLayoutInputSource() };
    if source.is_null() {
        return LayoutRead::Unavailable("no current keyboard layout input source".to_owned());
    }
    // SAFETY: `source` is non-null and follows the create rule, so the wrapper
    // takes over the single retain and releases it on drop.
    let source = unsafe { CFType::wrap_under_create_rule(source) };
    let source_ref = source.as_concrete_TypeRef();

    // SAFETY: `source_ref` is live (held by `source`); the key is Apple's
    // immutable global constant.
    let data = unsafe { TISGetInputSourceProperty(source_ref, kTISPropertyUnicodeKeyLayoutData) };
    if data.is_null() {
        return LayoutRead::Unavailable(
            "the input source has no Unicode key layout data".to_owned(),
        );
    }
    // SAFETY: the get rule returns a borrowed `CFData` valid while `source`
    // lives; `wrap_under_get_rule` retains it for the copy below.
    let data = unsafe { CFData::wrap_under_get_rule(data.cast::<c_void>() as CFDataRef) };
    let bytes = data.bytes().to_vec();
    if bytes.is_empty() {
        return LayoutRead::Unavailable("the Unicode key layout data is empty".to_owned());
    }

    // SAFETY: as above, with the input-source-ID constant; the result is a
    // borrowed `CFString` (or null) valid while `source` lives.
    let id = unsafe { TISGetInputSourceProperty(source_ref, kTISPropertyInputSourceID) };

    let mut hasher = DefaultHasher::new();
    if !id.is_null() {
        // SAFETY: non-null borrowed `CFString`, retained by the wrapper.
        let id = unsafe { CFString::wrap_under_get_rule(id.cast::<c_void>() as CFStringRef) };
        id.to_string().hash(&mut hasher);
    }
    bytes.hash(&mut hasher);
    // SAFETY: no arguments, no side effects.
    let keyboard_type = u32::from(unsafe { LMGetKbdType() });
    LayoutRead::Available(Box::new(TisLayout {
        identity: LayoutId(hasher.finish()),
        data: bytes,
        keyboard_type,
    }))
}
