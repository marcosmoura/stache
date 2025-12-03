//! Application-wide constants.

/// The app bundle identifier from `tauri.conf.json`, read at compile time.
pub const APP_BUNDLE_ID: &str = env!("TAURI_APP_IDENTIFIER");
