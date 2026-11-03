//! Accessibility trust checks behind safe functions.
//!
//! `AXIsProcessTrusted` and `AXIsProcessTrustedWithOptions` are documented as
//! callable from any thread; they do not require the main thread or a run loop.
//! Only [`request_trust`] may show a system prompt, and callers should invoke it
//! from a user action. Tests call [`is_trusted`], which never prompts.

use core_foundation::base::{Boolean, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};
use stache_sdk::{Permission, PermissionError, PermissionProvider, PermissionStatus};

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    /// Whether the current process is trusted for Accessibility.
    fn AXIsProcessTrusted() -> Boolean;

    /// Whether the process is trusted, optionally prompting the user.
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> Boolean;

    /// The option key that asks the system to show the trust prompt.
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

/// Returns the current Accessibility trust status without prompting.
#[must_use]
pub fn is_trusted() -> bool {
    // SAFETY: the function takes no arguments, transfers no ownership and is
    // safe to call from any thread.
    unsafe { AXIsProcessTrusted() != 0 }
}

/// Asks the system to grant Accessibility trust, prompting if needed.
///
/// Returns the trust status after the request. The call may show a system
/// dialog and is safe from any thread, but the host should only invoke it from a
/// user action.
#[must_use]
pub fn request_trust() -> bool {
    let prompt_key = {
        // SAFETY: kAXTrustedCheckOptionPrompt is an immutable process-wide
        // CFString constant. wrap_under_get_rule retains it, so the wrapper owns
        // a balanced reference.
        unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt) }
    };
    let options: CFDictionary<CFString, CFBoolean> =
        CFDictionary::from_CFType_pairs(&[(prompt_key, CFBoolean::true_value())]);
    // SAFETY: `options` is a valid, live CFDictionary for the whole call.
    // AXIsProcessTrustedWithOptions only reads it and does not retain it.
    unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) != 0 }
}

/// A [`PermissionProvider`] backed by the Accessibility APIs.
#[derive(Debug, Clone, Copy, Default)]
pub struct AccessibilityPermission;

impl PermissionProvider for AccessibilityPermission {
    fn status(&self, permission: Permission) -> PermissionStatus {
        match permission {
            Permission::Accessibility => {
                if is_trusted() {
                    PermissionStatus::Granted
                } else {
                    PermissionStatus::Denied
                }
            }
        }
    }

    fn request(&self, permission: Permission) -> Result<PermissionStatus, PermissionError> {
        match permission {
            Permission::Accessibility => Ok(if request_trust() {
                PermissionStatus::Granted
            } else {
                PermissionStatus::Denied
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_checks_are_repeatable_and_never_prompt() {
        assert_eq!(is_trusted(), is_trusted());
    }

    #[test]
    fn provider_reports_the_same_status_without_prompting() {
        let expected = if is_trusted() {
            PermissionStatus::Granted
        } else {
            PermissionStatus::Denied
        };
        assert_eq!(
            AccessibilityPermission.status(Permission::Accessibility),
            expected
        );
    }
}
