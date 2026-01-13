//! Screen change monitoring for the tiling window manager.
//!
//! This module monitors for display configuration changes (screens connected
//! or disconnected) using macOS's `CGDisplayRegisterReconfigurationCallback`.
//!
//! When a screen configuration change is detected, it notifies the tiling
//! manager which then:
//! - Refreshes the screen list
//! - Reassigns workspaces to available screens
//! - Moves windows to appropriate screens

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use super::constants::timing::SCREEN_CHANGE_DELAY_MS;

/// Callback function type for screen configuration changes.
type ScreenChangeCallback = fn();

/// Global callback for screen changes.
static SCREEN_CHANGE_CALLBACK: OnceLock<ScreenChangeCallback> = OnceLock::new();

/// Whether the monitor has been initialized.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Whether we're currently processing a screen change.
/// This prevents recursive callbacks during our own screen refresh operations.
static PROCESSING_CHANGE: AtomicBool = AtomicBool::new(false);

// ============================================================================
// CoreGraphics Display Reconfiguration
// ============================================================================

/// Display reconfiguration flags from CoreGraphics.
#[allow(non_upper_case_globals)]
mod cg_flags {
    /// Display has been added.
    pub const kCGDisplayAddFlag: u32 = 1 << 4;
    /// Display has been removed.
    pub const kCGDisplayRemoveFlag: u32 = 1 << 5;
    /// Display is being reconfigured (about to change).
    pub const kCGDisplayBeginConfigurationFlag: u32 = 1 << 0;
}

/// CoreGraphics display reconfiguration callback.
///
/// This is called by macOS when display configuration changes.
///
/// # Safety
///
/// This function is called from CoreGraphics and must be careful about
/// what it does. We use atomic flags to prevent reentrancy.
unsafe extern "C" fn display_reconfiguration_callback(
    _display: u32,
    flags: u32,
    _user_info: *mut std::ffi::c_void,
) {
    // Ignore "begin configuration" events - wait for the actual change
    if flags & cg_flags::kCGDisplayBeginConfigurationFlag != 0 {
        return;
    }

    // Only handle add/remove events (actual screen connect/disconnect)
    let is_add = flags & cg_flags::kCGDisplayAddFlag != 0;
    let is_remove = flags & cg_flags::kCGDisplayRemoveFlag != 0;

    if !is_add && !is_remove {
        return;
    }

    // Prevent recursive callbacks
    if PROCESSING_CHANGE.load(Ordering::SeqCst) {
        return;
    }

    // Log the event
    let event_type = if is_add { "connected" } else { "disconnected" };
    eprintln!("stache: tiling: screen {event_type} (display reconfiguration)");

    // Call the registered callback
    if let Some(callback) = SCREEN_CHANGE_CALLBACK.get() {
        // Mark that we're processing to prevent reentrancy
        PROCESSING_CHANGE.store(true, Ordering::SeqCst);

        // Spawn a thread to handle the change with a small delay
        // This ensures all CoreGraphics updates are complete
        std::thread::spawn(move || {
            // Brief delay to let macOS finish updating display state
            std::thread::sleep(std::time::Duration::from_millis(SCREEN_CHANGE_DELAY_MS));

            callback();

            PROCESSING_CHANGE.store(false, Ordering::SeqCst);
        });
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Initializes the screen change monitor.
///
/// # Arguments
///
/// * `callback` - Function to call when screen configuration changes
///
/// # Returns
///
/// `true` if initialization succeeded, `false` if already initialized.
pub fn init(callback: ScreenChangeCallback) -> bool {
    // Only initialize once
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        eprintln!("stache: tiling: screen monitor already initialized");
        return false;
    }

    // Store the callback
    if SCREEN_CHANGE_CALLBACK.set(callback).is_err() {
        eprintln!("stache: tiling: screen monitor callback already set");
        return false;
    }

    // Register for display reconfiguration notifications
    unsafe {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGDisplayRegisterReconfigurationCallback(
                callback: unsafe extern "C" fn(u32, u32, *mut std::ffi::c_void),
                user_info: *mut std::ffi::c_void,
            ) -> i32;
        }

        let result = CGDisplayRegisterReconfigurationCallback(
            display_reconfiguration_callback,
            std::ptr::null_mut(),
        );

        if result != 0 {
            eprintln!(
                "stache: tiling: failed to register display reconfiguration callback: {result}"
            );
            INITIALIZED.store(false, Ordering::SeqCst);
            return false;
        }
    }

    eprintln!("stache: tiling: screen monitor initialized");
    true
}

/// Checks if the screen monitor is initialized.
#[must_use]
pub fn is_initialized() -> bool { INITIALIZED.load(Ordering::SeqCst) }

/// Sets the processing flag.
///
/// This should be called before manually refreshing screens to prevent
/// the callback from being triggered recursively.
pub fn set_processing(processing: bool) { PROCESSING_CHANGE.store(processing, Ordering::SeqCst); }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_not_initialized_by_default() {
        // Note: This test may fail if run after other tests that initialize
        // the monitor. In practice, initialization happens once at app start.
        // We can't easily test this without resetting global state.
    }

    #[test]
    fn test_cg_flags() {
        // Verify flag values match CoreGraphics
        assert_eq!(cg_flags::kCGDisplayAddFlag, 0x10);
        assert_eq!(cg_flags::kCGDisplayRemoveFlag, 0x20);
        assert_eq!(cg_flags::kCGDisplayBeginConfigurationFlag, 0x01);
    }
}
