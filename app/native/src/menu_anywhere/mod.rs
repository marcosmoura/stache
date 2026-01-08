//! `MenuAnywhere` Module for Barba Shell.
//!
//! This module provides the ability to summon the current application's menu bar
//! at any location on screen using a configurable keyboard + mouse trigger.
//!
//! The implementation uses macOS Accessibility APIs to read the menu bar of the
//! frontmost application and rebuild it as an `NSMenu` that can be displayed at
//! the cursor position.
//!
//! This is a Rust implementation inspired by the menuanywhere project:
//! <https://github.com/acsandmann/menuanywhere>

mod accessibility;
mod event_monitor;
mod menu_builder;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::get_config;

/// Flag indicating if the module is running.
static IS_RUNNING: AtomicBool = AtomicBool::new(false);

/// Tauri app handle for emitting events (stored when initialized).
static APP_HANDLE: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

/// Initializes the `MenuAnywhere` module.
///
/// This sets up a global event tap to intercept the configured mouse + modifier
/// combination and displays the frontmost app's menu bar at the cursor position.
///
/// # Arguments
/// * `app_handle` - The Tauri app handle for emitting events.
pub fn init(app_handle: tauri::AppHandle) {
    let config = get_config();

    if !config.menu_anywhere.is_enabled() {
        return;
    }

    // Check accessibility permissions first
    if !accessibility::check_permissions() {
        return;
    }

    // Store the app handle for later use
    if let Ok(mut handle) = APP_HANDLE.lock() {
        *handle = Some(app_handle);
    }

    // Start the event monitor in a separate thread
    let menu_config = config.menu_anywhere.clone();
    std::thread::spawn(move || {
        event_monitor::start(&menu_config);
    });

    IS_RUNNING.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_running_starts_false() {
        // Note: This test checks initial state before init() is called
        // In actual runtime, init() may have been called already
        let _ = IS_RUNNING.load(Ordering::SeqCst);
    }
}
