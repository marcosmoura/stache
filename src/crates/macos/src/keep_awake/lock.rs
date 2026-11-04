//! Read-only session lock probe.
//!
//! `CGSessionCopyCurrentDictionary` is public CoreGraphics API. It returns a
//! nullable, caller-owned dictionary; the installed `objc2-core-graphics 0.3.2`
//! wrapper takes that ownership as `Option<CFRetained<CFDictionary>>`, so every
//! path here drops the copy and never calls `CFRelease` manually.
//!
//! `CGSSessionScreenIsLocked` is not documented in Apple's header. The mapping
//! is approved legacy compatibility, not a public guarantee:
//!
//! - valid dictionary and no key -> [`LockState::Unlocked`];
//! - present `CFBoolean` true -> [`LockState::Locked`];
//! - present `CFBoolean` false -> [`LockState::Unlocked`];
//! - missing dictionary or present non-boolean value -> [`LockState::Unknown`].
//!
//! Absence is an answer, not a query failure. The value type is validated before
//! any boolean read. `NSWorkspace` session activity, console presence and login
//! completion are deliberately not substituted for a lock test.

use std::ffi::c_void;
use std::ptr;

use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFGetTypeID, CFRetained, CFString, CFType, ConcreteType,
};
use objc2_core_graphics::CGSessionCopyCurrentDictionary;

/// The legacy session-dictionary key.
///
/// This key is not documented by Apple. Treat it as compatibility evidence, not
/// as a stable public contract.
pub const SCREEN_LOCKED_KEY: &str = "CGSSessionScreenIsLocked";

/// The legacy locked notification name.
///
/// This is a refresh hint only. Registration does not prove delivery and a
/// callback is not an authoritative lock state.
pub const SCREEN_LOCK_NOTIFICATION: &str = "com.apple.screenIsLocked";

/// The legacy unlocked notification name.
///
/// This is a refresh hint only. Registration does not prove delivery and a
/// callback is not an authoritative lock state.
pub const SCREEN_UNLOCK_NOTIFICATION: &str = "com.apple.screenIsUnlocked";

/// A lock result with typed uncertainty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockState {
    /// The session reports a locked screen.
    Locked,
    /// The session reports an unlocked screen, including the approved
    /// "key absent in a valid dictionary" compatibility case.
    Unlocked,
    /// The state cannot be read safely; acquisition must be suppressed.
    Unknown(LockUnknown),
}

/// Why a lock probe could not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockUnknown {
    /// The public API returned no dictionary, or it could not be read.
    SessionDictionaryUnavailable,
    /// The key was present but its value was not a `CFBoolean`.
    ///
    /// `type_id` is the observed CoreFoundation type identifier; zero means the
    /// dictionary contained a null value.
    ScreenIsLockedValueIsNotBoolean {
        /// The observed `CFTypeID`.
        type_id: u64,
    },
}

impl LockState {
    /// Whether the screen is known to be locked.
    #[must_use]
    pub const fn is_locked(self) -> bool { matches!(self, Self::Locked) }

    /// Whether the screen is known to be unlocked.
    #[must_use]
    pub const fn is_unlocked(self) -> bool { matches!(self, Self::Unlocked) }
}

/// The injectable lock probe.
///
/// The runtime probes before acquisition and on every refresh hint. Unknown
/// results must suppress acquisition rather than assume unlocked.
pub trait LockProbe: Send + Sync {
    /// Reads the current best-effort lock state.
    fn probe(&self) -> LockState;
}

/// The real CoreGraphics probe.
///
/// This unit value is `Send + Sync` and owns nothing. Each call copies the
/// current session dictionary and releases it on return.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemLockProbe;

impl LockProbe for SystemLockProbe {
    fn probe(&self) -> LockState {
        // The generated wrapper returns an owned copy; `probe_with_copy` drops
        // exactly once on every return path. No raw pointer crosses this
        // function.
        probe_with_copy(CGSessionCopyCurrentDictionary())
    }
}

// Consuming the owned copy is the point: this helper proves every return path
// releases exactly one `CGSessionCopyCurrentDictionary` retain.
#[allow(clippy::needless_pass_by_value)]
fn probe_with_copy(copy: Option<CFRetained<CFDictionary>>) -> LockState {
    classify_session_dictionary(copy.as_deref())
}

fn classify_session_dictionary(dictionary: Option<&CFDictionary>) -> LockState {
    let Some(dictionary) = dictionary else {
        return LockState::Unknown(LockUnknown::SessionDictionaryUnavailable);
    };
    let key = CFString::from_static_str(SCREEN_LOCKED_KEY);
    let key = ptr::from_ref::<CFString>(&key).cast::<c_void>();
    let mut value: *const c_void = ptr::null();
    // SAFETY: the dictionary is a live CFDictionary copy, the key is a live
    // CFString, and both out-pointers are valid. The dictionary retains its
    // values, so the returned pointer stays valid until this function returns.
    let present = unsafe { dictionary.value_if_present(key, ptr::from_mut(&mut value)) };
    if !present {
        return LockState::Unlocked;
    }
    // SAFETY: a present, non-null value is a live CoreFoundation object owned by
    // the dictionary and valid for this borrow. Null is passed as `None` so the
    // classifier never dereferences it.
    let value = if value.is_null() {
        None
    } else {
        Some(unsafe { &*value.cast::<CFType>() })
    };
    classify_present_value(value)
}

fn classify_present_value(value: Option<&CFType>) -> LockState {
    let Some(value) = value else {
        return LockState::Unknown(LockUnknown::ScreenIsLockedValueIsNotBoolean { type_id: 0 });
    };
    // CFGetTypeID accepts any live CFType reference and does not consume it.
    let type_id = CFGetTypeID(Some(value));
    if type_id != CFBoolean::type_id() {
        return LockState::Unknown(LockUnknown::ScreenIsLockedValueIsNotBoolean {
            type_id: type_id as u64,
        });
    }
    // The type identifier proved this is a CFBoolean instance, so the downcast
    // is exact. The value is borrowed from the live dictionary.
    let Some(boolean) = value.downcast_ref::<CFBoolean>() else {
        return LockState::Unknown(LockUnknown::ScreenIsLockedValueIsNotBoolean {
            type_id: type_id as u64,
        });
    };
    if boolean.value() {
        LockState::Locked
    } else {
        LockState::Unlocked
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CFRetained, CFString, kCFBooleanFalse, kCFBooleanTrue};

    use super::*;

    fn boolean_dictionary(value: &CFBoolean) -> CFRetained<CFDictionary<CFString, CFBoolean>> {
        let key = CFString::from_static_str(SCREEN_LOCKED_KEY);
        CFDictionary::from_slices(&[&key], &[value])
    }

    #[test]
    fn absent_key_is_the_approved_unlocked_compatibility_case() {
        let dictionary = CFDictionary::<CFString, CFBoolean>::empty();
        assert_eq!(
            classify_session_dictionary(Some(dictionary.as_opaque())),
            LockState::Unlocked
        );
    }

    #[test]
    fn present_true_is_locked() {
        let dictionary = boolean_dictionary(
            // SAFETY: `kCFBooleanTrue` is an immutable CoreFoundation constant
            // with static lifetime.
            unsafe { kCFBooleanTrue }.expect("constant boolean"),
        );
        assert_eq!(
            classify_session_dictionary(Some(dictionary.as_opaque())),
            LockState::Locked
        );
    }

    #[test]
    fn present_false_is_unlocked() {
        let dictionary = boolean_dictionary(
            // SAFETY: `kCFBooleanFalse` is an immutable CoreFoundation constant
            // with static lifetime.
            unsafe { kCFBooleanFalse }.expect("constant boolean"),
        );
        assert_eq!(
            classify_session_dictionary(Some(dictionary.as_opaque())),
            LockState::Unlocked
        );
    }

    #[test]
    fn a_missing_dictionary_is_typed_unknown() {
        assert_eq!(
            classify_session_dictionary(None),
            LockState::Unknown(LockUnknown::SessionDictionaryUnavailable)
        );
    }

    #[test]
    fn a_present_non_boolean_is_typed_unknown_without_reading_it() {
        let key = CFString::from_static_str(SCREEN_LOCKED_KEY);
        let value = CFString::from_static_str("not a boolean");
        let dictionary = CFDictionary::<CFString, CFString>::from_slices(&[&key], &[&value]);
        assert_eq!(
            classify_session_dictionary(Some(dictionary.as_opaque())),
            LockState::Unknown(LockUnknown::ScreenIsLockedValueIsNotBoolean {
                type_id: CFString::type_id() as u64,
            })
        );
    }

    #[test]
    fn unrelated_entries_do_not_change_absence() {
        let key = CFString::from_static_str("kCGSSessionOnConsoleKey");
        let value = CFString::from_static_str("unrelated");
        let dictionary = CFDictionary::<CFString, CFString>::from_slices(&[&key], &[&value]);
        assert_eq!(
            classify_session_dictionary(Some(dictionary.as_opaque())),
            LockState::Unlocked
        );
    }

    #[test]
    fn a_present_null_value_is_typed_unknown_without_dereferencing_it() {
        assert_eq!(
            classify_present_value(None),
            LockState::Unknown(LockUnknown::ScreenIsLockedValueIsNotBoolean { type_id: 0 })
        );
    }

    #[test]
    fn every_probe_outcome_releases_its_owned_dictionary_copy() {
        fn opaque_boolean(
            dictionary: CFRetained<CFDictionary<CFString, CFBoolean>>,
        ) -> CFRetained<CFDictionary> {
            // SAFETY: `CFDictionary<CFString, CFBoolean>` is the same opaque
            // CFDictionary object with a narrower static type.
            unsafe { CFRetained::cast_unchecked(dictionary) }
        }

        let cases = [
            (
                opaque_boolean(CFDictionary::<CFString, CFBoolean>::empty()),
                LockState::Unlocked,
            ),
            (
                opaque_boolean(boolean_dictionary(
                    // SAFETY: `kCFBooleanTrue` is an immutable static constant.
                    unsafe { kCFBooleanTrue }.expect("constant boolean"),
                )),
                LockState::Locked,
            ),
            (
                opaque_boolean(boolean_dictionary(
                    // SAFETY: `kCFBooleanFalse` is an immutable static constant.
                    unsafe { kCFBooleanFalse }.expect("constant boolean"),
                )),
                LockState::Unlocked,
            ),
        ];
        for (copy, expected) in cases {
            let kept = copy.clone();
            let before = kept.retain_count();
            assert_eq!(probe_with_copy(Some(copy)), expected);
            // Exactly the probe's one owned copy was released; the retained
            // checker still owns a live dictionary on every outcome.
            assert_eq!(kept.retain_count(), before - 1);
        }

        let key = CFString::from_static_str(SCREEN_LOCKED_KEY);
        let value = CFString::from_static_str("not a boolean");
        let typed = CFDictionary::<CFString, CFString>::from_slices(&[&key], &[&value]);
        // SAFETY: same opaque object with a narrower value type.
        let copy: CFRetained<CFDictionary> = unsafe { CFRetained::cast_unchecked(typed) };
        let kept = copy.clone();
        let before = kept.retain_count();
        assert!(matches!(
            probe_with_copy(Some(copy)),
            LockState::Unknown(LockUnknown::ScreenIsLockedValueIsNotBoolean { .. })
        ));
        assert_eq!(kept.retain_count(), before - 1);

        assert_eq!(
            probe_with_copy(None),
            LockState::Unknown(LockUnknown::SessionDictionaryUnavailable)
        );
    }

    #[test]
    fn the_probe_is_send_and_sync_without_a_native_object() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SystemLockProbe>();
    }
}
