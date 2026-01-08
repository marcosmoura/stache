//! Accessibility permission utilities for `MenuAnywhere`.
//!
//! This module provides utilities for checking and requesting macOS accessibility
//! permissions, which are required for reading other applications' menu bars.

use std::ffi::c_void;

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;

// FFI declarations for Accessibility API
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
}

// Key for prompting the user for accessibility permissions
const K_AX_TRUSTED_CHECK_OPTION_PROMPT: &str = "AXTrustedCheckOptionPrompt";

/// Checks if the application has accessibility permissions.
///
/// This is required to read other applications' menu bars via the Accessibility API.
///
/// # Returns
///
/// Returns `true` if the application has accessibility permissions, `false` otherwise.
#[must_use]
#[allow(dead_code)]
pub fn is_trusted() -> bool { unsafe { AXIsProcessTrusted() } }

/// Checks accessibility permissions and optionally prompts the user to grant them.
///
/// If permissions are not granted, this function will show a system dialog
/// asking the user to grant accessibility permissions.
///
/// # Returns
///
/// Returns `true` if permissions are granted, `false` otherwise.
#[must_use]
pub fn check_permissions() -> bool {
    // Create the options dictionary to prompt for permissions
    let key = CFString::new(K_AX_TRUSTED_CHECK_OPTION_PROMPT);
    let value = CFBoolean::true_value();

    let pairs = [(key.as_CFType(), value.as_CFType())];
    let options = CFDictionary::from_CFType_pairs(&pairs);

    unsafe { AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef().cast()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_trusted_returns_bool() {
        // This test just verifies the function doesn't crash
        // The actual return value depends on system permissions
        let _ = is_trusted();
    }
}
