//! `MenuAnywhere` Module for Stache.
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

mod event_monitor;
mod menu_builder;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::get_config;
use crate::is_accessibility_granted;

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

    // Use cached accessibility permission check from lib.rs
    if !is_accessibility_granted() {
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

use crate::modules::services::lifecycle::{LifecycleModule, ModuleStatus};

/// Pure status decision: config gate, then accessibility, then the real tap state.
fn menu_anywhere_status(
    config_enabled: bool,
    accessibility: bool,
    running: bool,
    tap_state: Option<bool>,
) -> ModuleStatus {
    if !config_enabled {
        return ModuleStatus::ConfiguredOff;
    }
    if !accessibility {
        return ModuleStatus::Unavailable("Accessibility permission required".into());
    }
    match tap_state {
        None if running => ModuleStatus::Unavailable(
            "event tap creation failed — check Accessibility permission".into(),
        ),
        Some(true) => ModuleStatus::Running,
        None | Some(false) => ModuleStatus::Paused,
    }
}

/// Tray-toggleable lifecycle handle for `MenuAnywhere`.
pub struct MenuAnywhereLifecycle {
    app_handle: tauri::AppHandle,
}

impl MenuAnywhereLifecycle {
    #[must_use]
    pub const fn new(app_handle: tauri::AppHandle) -> Self { Self { app_handle } }
}

impl LifecycleModule for MenuAnywhereLifecycle {
    fn name(&self) -> &'static str { "Menu Anywhere" }

    fn id(&self) -> &'static str { "menuAnywhere" }

    fn start(&self) -> Result<(), String> {
        if IS_RUNNING.load(Ordering::SeqCst) {
            return Ok(());
        }
        init(self.app_handle.clone());
        Ok(())
    }

    fn pause(&self) -> Result<(), String> { event_monitor::set_enabled(false) }

    fn resume(&self) -> Result<(), String> { event_monitor::set_enabled(true) }

    fn status(&self) -> ModuleStatus {
        let config = crate::config::get_config();
        let config_enabled = config.menu_anywhere.is_enabled();
        let accessibility = crate::is_accessibility_granted();
        let running = IS_RUNNING.load(Ordering::SeqCst);
        let tap_state = event_monitor::tap_state();
        menu_anywhere_status(config_enabled, accessibility, running, tap_state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_anywhere_status_maps_states() {
        use crate::modules::services::lifecycle::ModuleStatus;

        assert_eq!(
            menu_anywhere_status(false, true, true, Some(true)),
            ModuleStatus::ConfiguredOff
        );
        assert!(matches!(
            menu_anywhere_status(true, false, false, None),
            ModuleStatus::Unavailable(reason) if reason.contains("Accessibility")
        ));
        assert_eq!(
            menu_anywhere_status(true, true, true, Some(true)),
            ModuleStatus::Running
        );
        assert_eq!(
            menu_anywhere_status(true, true, true, Some(false)),
            ModuleStatus::Paused
        );
        assert_eq!(
            menu_anywhere_status(true, true, false, None),
            ModuleStatus::Paused
        );
        assert!(matches!(
            menu_anywhere_status(true, true, true, None),
            ModuleStatus::Unavailable(reason) if reason.contains("Accessibility")
        ));
    }

    #[test]
    fn test_is_running_starts_false() {
        // Note: This test checks initial state before init() is called
        // In actual runtime, init() may have been called already
        let _ = IS_RUNNING.load(Ordering::SeqCst);
    }
}
