//! Opening System Settings at a privacy pane.
//!
//! The call uses `NSWorkspace openURL:` and must run on the main thread. This
//! module requires no unsafe code: the generated `objc2` bindings for
//! `NSWorkspace::openURL` and `NSURL::URLWithString` are safe, and the
//! main-thread precondition is enforced with a `MainThreadMarker` check that
//! returns [`SettingsError::NotMainThread`] instead of calling `AppKit` off the
//! main thread. Callers dispatch through the host main-thread executor.
//!
//! The `x-apple.systempreferences` URL scheme is undocumented by Apple; its
//! behavior on the running macOS release is verified manually.

use std::fmt;

use objc2::MainThreadMarker;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSString, NSURL};

/// The Accessibility privacy pane.
pub const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

/// Why System Settings could not be opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsError {
    /// The caller was not the main thread.
    NotMainThread,
    /// The URL could not be built.
    InvalidUrl,
    /// `AppKit` reported that it did not open the URL.
    Rejected,
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotMainThread => "System Settings must be opened from the main thread",
            Self::InvalidUrl => "the System Settings URL is invalid",
            Self::Rejected => "macOS did not open System Settings",
        })
    }
}

impl std::error::Error for SettingsError {}

/// Opens the Accessibility pane of System Settings.
///
/// Must be called on the main thread.
///
/// # Errors
/// Returns [`SettingsError`] when called off the main thread, when the URL is
/// invalid or when `AppKit` answers `false`.
pub fn open_accessibility_settings() -> Result<(), SettingsError> {
    let _main = MainThreadMarker::new().ok_or(SettingsError::NotMainThread)?;
    let url = NSURL::URLWithString(&NSString::from_str(ACCESSIBILITY_SETTINGS_URL))
        .ok_or(SettingsError::InvalidUrl)?;
    if NSWorkspace::sharedWorkspace().openURL(&url) {
        Ok(())
    } else {
        Err(SettingsError::Rejected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_off_main_call_is_refused_without_opening_anything() {
        let result = std::thread::spawn(open_accessibility_settings).join().expect("thread");
        assert_eq!(result, Err(SettingsError::NotMainThread));
    }
}
