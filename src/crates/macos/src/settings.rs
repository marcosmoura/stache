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

/// The Battery pane, opened by the Bar battery click.
pub const BATTERY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.Battery-Settings.extension";

/// The Wi-Fi pane, opened by the Bar Wi-Fi click.
pub const WIFI_SETTINGS_URL: &str = "x-apple.systempreferences:com.apple.wifi-settings-extension";

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

fn open_fixed(url: &'static str) -> Result<(), SettingsError> {
    let _main = MainThreadMarker::new().ok_or(SettingsError::NotMainThread)?;
    let url = NSURL::URLWithString(&NSString::from_str(url)).ok_or(SettingsError::InvalidUrl)?;
    if NSWorkspace::sharedWorkspace().openURL(&url) {
        Ok(())
    } else {
        Err(SettingsError::Rejected)
    }
}

/// Opens the Accessibility pane of System Settings.
///
/// Must be called on the main thread.
///
/// # Errors
/// Returns [`SettingsError`] when called off the main thread, when the URL is
/// invalid or when `AppKit` answers `false`.
pub fn open_accessibility_settings() -> Result<(), SettingsError> {
    open_fixed(ACCESSIBILITY_SETTINGS_URL)
}

/// Opens the Battery pane of System Settings. The URL is a fixed constant.
///
/// Must be called on the main thread.
///
/// # Errors
/// As [`open_accessibility_settings`].
pub fn open_battery_settings() -> Result<(), SettingsError> { open_fixed(BATTERY_SETTINGS_URL) }

/// Opens the Wi-Fi pane of System Settings. The URL is a fixed constant.
///
/// Must be called on the main thread.
///
/// # Errors
/// As [`open_accessibility_settings`].
pub fn open_wifi_settings() -> Result<(), SettingsError> { open_fixed(WIFI_SETTINGS_URL) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_off_main_call_is_refused_without_opening_anything() {
        let result = std::thread::spawn(open_accessibility_settings).join().expect("thread");
        assert_eq!(result, Err(SettingsError::NotMainThread));
    }

    #[test]
    fn the_bar_pane_calls_are_refused_off_main_and_the_urls_are_fixed() {
        assert_eq!(
            std::thread::spawn(open_battery_settings).join().expect("thread"),
            Err(SettingsError::NotMainThread)
        );
        assert_eq!(
            std::thread::spawn(open_wifi_settings).join().expect("thread"),
            Err(SettingsError::NotMainThread)
        );
        assert_eq!(
            BATTERY_SETTINGS_URL,
            "x-apple.systempreferences:com.apple.Battery-Settings.extension"
        );
        assert_eq!(
            WIFI_SETTINGS_URL,
            "x-apple.systempreferences:com.apple.wifi-settings-extension"
        );
    }
}
