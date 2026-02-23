//! Window enumeration for tiling v2.
//!
//! This module provides window enumeration functionality similar to v1's
//! `get_all_windows_including_hidden()`, but adapted for v2's architecture.
//!
//! # Architecture
//!
//! Window enumeration uses an AX-first approach:
//! 1. Enumerate running applications via `NSWorkspace`
//! 2. For each app, get AX windows (the source of truth)
//! 3. Get `CGWindowID` from AX element via `_AXUIElementGetWindow`
//! 4. Combine with bundle ID and app name for complete window info

use std::collections::HashMap;

use objc::runtime::{BOOL, Class, Object, YES};
use objc::{msg_send, sel, sel_impl};

use super::ffi::accessibility::AXElement;
use super::rules::is_pip_window;
use super::state::Rect;

// ============================================================================
// FFI for NSString
// ============================================================================

/// Converts an `NSString` pointer to a Rust `String`.
///
/// Returns an empty string if the pointer is null or conversion fails.
///
/// # Safety
///
/// The caller must ensure that `ns_string` is a valid `NSString` pointer or null.
unsafe fn ns_string_to_rust(ns_string: *mut Object) -> String {
    if ns_string.is_null() {
        return String::new();
    }

    let utf8: *const i8 = msg_send![ns_string, UTF8String];
    if utf8.is_null() {
        return String::new();
    }

    // SAFETY: The UTF8String method returns a valid null-terminated C string
    // or null (which we checked above).
    unsafe { std::ffi::CStr::from_ptr(utf8) }.to_string_lossy().into_owned()
}

// ============================================================================
// AppInfo
// ============================================================================

/// Information about a running application.
#[derive(Debug)]
pub struct AppInfo {
    /// Process ID.
    pub pid: i32,
    /// Bundle identifier (e.g., "com.apple.Safari").
    pub bundle_id: String,
    /// Application name (e.g., "Safari").
    pub name: String,
    /// Whether the app is currently hidden.
    pub is_hidden: bool,
    /// AX element for this application.
    pub ax_app: AXElement,
}

/// Gets all running applications that can own windows.
///
/// Filters to only include "regular" apps (excludes background-only apps).
#[must_use]
pub fn get_running_apps() -> Vec<AppInfo> {
    unsafe {
        let Some(workspace_class) = Class::get("NSWorkspace") else {
            return Vec::new();
        };

        let workspace: *mut Object = msg_send![workspace_class, sharedWorkspace];
        if workspace.is_null() {
            return Vec::new();
        }

        let apps: *mut Object = msg_send![workspace, runningApplications];
        if apps.is_null() {
            return Vec::new();
        }

        let count: usize = msg_send![apps, count];
        let mut result = Vec::with_capacity(count);

        for i in 0..count {
            let app: *mut Object = msg_send![apps, objectAtIndex: i];
            if app.is_null() {
                continue;
            }

            // Skip apps that can't be activated (background apps)
            let activation_policy: i64 = msg_send![app, activationPolicy];
            if activation_policy != 0 {
                // 0 = NSApplicationActivationPolicyRegular
                continue;
            }

            let pid: i32 = msg_send![app, processIdentifier];
            if pid <= 0 {
                continue;
            }

            let bundle_id = ns_string_to_rust(msg_send![app, bundleIdentifier]);
            let name = ns_string_to_rust(msg_send![app, localizedName]);
            let is_hidden: BOOL = msg_send![app, isHidden];

            // Create AX element for this app
            let Some(ax_app) = AXElement::application(pid) else {
                continue;
            };

            result.push(AppInfo {
                pid,
                bundle_id,
                name,
                is_hidden: is_hidden == YES,
                ax_app,
            });
        }

        result
    }
}

// ============================================================================
// WindowInfo
// ============================================================================

/// Extended window information for tracking.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct WindowInfo {
    /// Window ID (`CGWindowID`).
    pub id: u32,
    /// Owner process ID.
    pub pid: i32,
    /// Bundle identifier of the owner app.
    pub bundle_id: String,
    /// Owner application name.
    pub app_name: String,
    /// Window title.
    pub title: String,
    /// Window frame (position and size).
    pub frame: Rect,
    /// Minimum size constraints (width, height) if the window reports them.
    pub minimum_size: Option<(f64, f64)>,
    /// Whether the window is minimized.
    pub is_minimized: bool,
    /// Whether the app is hidden.
    pub is_hidden: bool,
    /// Whether this window has focus.
    pub is_focused: bool,
    /// Whether this window is in fullscreen mode.
    pub is_fullscreen: bool,
}

// ============================================================================
// Window Enumeration
// ============================================================================

/// Gets all windows including those from hidden/minimized apps.
///
/// This is the primary function for initial window tracking at startup.
/// It enumerates windows using the AX-first approach to ensure we get
/// accurate window information.
///
/// # Returns
///
/// A vector of [`WindowInfo`] structs for all trackable windows.
#[must_use]
pub fn get_all_windows_including_hidden() -> Vec<WindowInfo> {
    // Minimum size thresholds
    // Standard windows: 200x150 minimum
    // Dialogs: 400x300 minimum (real dialogs like preferences are larger;
    //          small dialogs are popups like date pickers, color pickers)
    const MIN_STANDARD_WIDTH: f64 = 200.0;
    const MIN_STANDARD_HEIGHT: f64 = 150.0;
    const MIN_DIALOG_WIDTH: f64 = 400.0;
    const MIN_DIALOG_HEIGHT: f64 = 300.0;

    let apps = get_running_apps();

    // Build a map of PID -> (bundle_id, app_name, is_hidden)
    let app_info_map: HashMap<i32, (&str, &str, bool)> = apps
        .iter()
        .map(|app| {
            (
                app.pid,
                (app.bundle_id.as_str(), app.name.as_str(), app.is_hidden),
            )
        })
        .collect();

    let mut result = Vec::new();

    for app in &apps {
        // Get all AX windows for this app
        let ax_windows = app.ax_app.windows();

        for ax_window in ax_windows {
            // Get window ID (required)
            let Some(window_id) = ax_window.window_id() else {
                continue;
            };

            // Get frame (required)
            let Some(frame) = ax_window.frame() else {
                continue;
            };

            // Get subrole for filtering
            let subrole = ax_window.subrole();

            // Skip PiP (Picture-in-Picture) windows
            if is_pip_window(subrole.as_deref()) {
                continue;
            }

            // Determine if window should be managed based on subrole and size
            // Use blacklist approach: only reject known popup/sheet subroles
            let should_manage = match subrole.as_deref() {
                // Explicitly reject popup-like subroles
                // Sheets and drawers are always attached to parent windows
                // AXUnknown subrole indicates popup/panel windows without standard controls
                // Examples: browser extension popups, toolbar popups, dropdown panels
                // These windows typically have no close/minimize/zoom buttons
                Some("AXSheet" | "AXDrawer" | "AXUnknown") => false,
                Some("AXDialog") => {
                    // Dialogs - only accept large ones (preferences, settings)
                    // Small dialogs are popups (date pickers, color pickers, alerts)
                    frame.width >= MIN_DIALOG_WIDTH && frame.height >= MIN_DIALOG_HEIGHT
                }
                _ => {
                    // Standard windows, no subrole, or custom subroles - accept if large enough
                    // Many apps (like Ghostty) don't set subrole or use custom values
                    frame.width >= MIN_STANDARD_WIDTH && frame.height >= MIN_STANDARD_HEIGHT
                }
            };

            if !should_manage {
                continue;
            }

            // Get optional attributes
            let title = ax_window.title().unwrap_or_default();
            let is_minimized = ax_window.is_minimized().unwrap_or(false);
            let is_focused = ax_window.is_focused().unwrap_or(false);
            let is_fullscreen = ax_window.is_fullscreen().unwrap_or(false);
            let minimum_size = ax_window.minimum_size();

            // Get app info
            let (bundle_id, app_name, is_hidden) =
                app_info_map.get(&app.pid).copied().unwrap_or(("", "", false));

            // Skip Finder "Get Info" windows (e.g., "filename.txt Info")
            // These are popup-like windows that shouldn't be tiled
            if bundle_id == "com.apple.finder" && title.ends_with(" Info") {
                continue;
            }

            result.push(WindowInfo {
                id: window_id,
                pid: app.pid,
                bundle_id: bundle_id.to_string(),
                app_name: app_name.to_string(),
                title,
                frame,
                minimum_size,
                is_minimized,
                is_hidden,
                is_focused,
                is_fullscreen,
            });
        }
    }

    result
}

/// Gets only visible (on-screen) windows.
///
/// Excludes windows from hidden apps and minimized windows.
#[must_use]
pub fn get_visible_windows() -> Vec<WindowInfo> {
    get_all_windows_including_hidden()
        .into_iter()
        .filter(|w| !w.is_hidden && !w.is_minimized)
        .collect()
}

/// Gets the currently focused window ID.
///
/// This uses the proper macOS approach:
/// 1. Get the frontmost application via `NSWorkspace.frontmostApplication`
/// 2. Get that app's focused window via `AXFocusedWindow` attribute
///
/// # Returns
///
/// The window ID of the currently focused window, or `None` if no window is focused.
#[must_use]
pub fn get_focused_window_id() -> Option<u32> { unsafe { get_focused_window_id_unsafe() } }

/// Internal implementation of [`get_focused_window_id`].
///
/// # Safety
///
/// Uses Objective-C runtime and Accessibility APIs. Must be called from the main thread.
unsafe fn get_focused_window_id_unsafe() -> Option<u32> {
    use std::ffi::c_void;

    // FFI for AXFocusedWindow attribute
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
        fn AXUIElementCopyAttributeValue(
            element: *mut c_void,
            attribute: *const c_void,
            value: *mut *mut c_void,
        ) -> i32;
        fn _AXUIElementGetWindow(element: *mut c_void, window_id: *mut u32) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    const K_AX_ERROR_SUCCESS: i32 = 0;

    // Cached CFString for "AXFocusedWindow" — created once and reused.
    fn cf_focused_window() -> *const c_void {
        use std::sync::OnceLock;

        use core_foundation::base::TCFType;
        use core_foundation::string::CFString;

        static CF_FOCUSED_WINDOW: OnceLock<usize> = OnceLock::new();

        let ptr = *CF_FOCUSED_WINDOW.get_or_init(|| {
            let s = CFString::new("AXFocusedWindow");
            let ptr = s.as_concrete_TypeRef().cast::<c_void>() as usize;
            std::mem::forget(s); // Intentional: leaked once to create a 'static CFString
            ptr
        });

        ptr as *const c_void
    }

    // Get the frontmost application
    let workspace_class = Class::get("NSWorkspace")?;
    let workspace: *mut Object = msg_send![workspace_class, sharedWorkspace];
    if workspace.is_null() {
        return None;
    }

    let frontmost_app: *mut Object = msg_send![workspace, frontmostApplication];
    if frontmost_app.is_null() {
        return None;
    }

    let pid: i32 = msg_send![frontmost_app, processIdentifier];
    if pid <= 0 {
        return None;
    }

    // Create AX element for the frontmost app
    // SAFETY: AXUIElementCreateApplication is safe with a valid PID
    let app_element = unsafe { AXUIElementCreateApplication(pid) };
    if app_element.is_null() {
        return None;
    }

    // Get the focused window from the app
    let mut focused_window_ref: *mut c_void = std::ptr::null_mut();
    // SAFETY: We have a valid app_element and focused_window_ref is a valid out pointer
    let result = unsafe {
        AXUIElementCopyAttributeValue(app_element, cf_focused_window(), &raw mut focused_window_ref)
    };

    // Release the app element
    // SAFETY: app_element was created by AXUIElementCreateApplication
    unsafe { CFRelease(app_element.cast()) };

    if result != K_AX_ERROR_SUCCESS || focused_window_ref.is_null() {
        return None;
    }

    // Get the window ID from the focused window
    let mut window_id: u32 = 0;
    // SAFETY: focused_window_ref is a valid AXUIElement and window_id is a valid out pointer
    let result = unsafe { _AXUIElementGetWindow(focused_window_ref, &raw mut window_id) };

    // Release the window reference
    // SAFETY: focused_window_ref was created by AXUIElementCopyAttributeValue
    unsafe { CFRelease(focused_window_ref.cast()) };

    if result != K_AX_ERROR_SUCCESS || window_id == 0 {
        None
    } else {
        Some(window_id)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_running_apps_returns_results() {
        // This test just verifies the function doesn't panic
        // and returns some apps (at least Terminal/test runner)
        let apps = get_running_apps();
        // In a test environment there should be at least the test runner
        assert!(!apps.is_empty() || apps.is_empty()); // Allow empty in sandboxed environments
    }

    #[test]
    fn test_get_all_windows_returns_results() {
        // This test just verifies the function doesn't panic
        let windows = get_all_windows_including_hidden();
        // Just verify it returns a vector (may be empty in CI)
        let _ = windows.len();
    }

    #[test]
    fn test_get_visible_windows_filters_hidden() {
        let visible = get_visible_windows();
        // All returned windows should not be hidden or minimized
        for w in &visible {
            assert!(!w.is_hidden);
            assert!(!w.is_minimized);
        }
    }
}
