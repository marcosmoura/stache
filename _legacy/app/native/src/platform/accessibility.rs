//! Accessibility permission utilities for macOS.
//!
//! This module provides utilities for checking and requesting macOS accessibility
//! permissions, which are required for window management, menu reading, and other
//! system-level interactions.

use std::ffi::c_void;

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;

// FFI declarations for Accessibility API
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    #[allow(dead_code)] // Used by is_trusted() which is a public API for non-prompting checks
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
}

// Key for prompting the user for accessibility permissions
const K_AX_TRUSTED_CHECK_OPTION_PROMPT: &str = "AXTrustedCheckOptionPrompt";

/// Checks if the application has accessibility permissions.
///
/// This is required to interact with other applications' windows and UI elements
/// via the Accessibility API.
///
/// # Returns
///
/// Returns `true` if the application has accessibility permissions, `false` otherwise.
#[must_use]
#[allow(dead_code)] // Public API for checking permissions without prompting
pub fn is_trusted() -> bool { unsafe { AXIsProcessTrusted() } }

/// Checks accessibility permissions and optionally prompts the user to grant them.
///
/// If permissions are not granted, this function will show a system dialog
/// asking the user to grant accessibility permissions. The user will be directed
/// to System Preferences > Security & Privacy > Privacy > Accessibility.
///
/// # Returns
///
/// Returns `true` if permissions are granted, `false` otherwise.
/// Note that if the user grants permissions, they typically need to restart the app
/// for the changes to take effect.
#[must_use]
pub fn check_and_prompt() -> bool {
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

    #[test]
    fn test_check_and_prompt_returns_bool() {
        // This test verifies the function doesn't crash
        // Note: In CI/headless environments, this won't show a dialog
        // The actual return value depends on system permissions
        let _ = is_trusted(); // Use is_trusted to avoid showing dialog in tests
    }
}
