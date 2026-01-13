//! Window operations for the tiling window manager.
//!
//! This module provides functions to enumerate, query, and manipulate windows
//! using the macOS Accessibility API (`AXUIElement`).

use std::cell::OnceCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc::runtime::{BOOL, Class, Object, YES};
use objc::{msg_send, sel, sel_impl};
use rayon::prelude::*;

use super::constants::offscreen;
use super::error::{TilingError, TilingResult};
use super::state::{Rect, TrackedWindow};

// ============================================================================
// Accessibility API Types and FFI
// ============================================================================

type AXUIElementRef = *mut c_void;
type AXError = i32;

const K_AX_ERROR_SUCCESS: AXError = 0;

/// Thread-safe wrapper for `AXUIElementRef`.
///
/// The macOS Accessibility API is thread-safe for operations on different
/// windows/elements. This wrapper allows us to use parallel iteration
/// when positioning multiple windows.
///
/// # Safety
///
/// Each `AXUIElementRef` should only be used with its own window.
/// Do not share the same element across threads for the same window.
#[derive(Clone, Copy)]
struct SendableAXElement(AXUIElementRef);

// SAFETY: AX API is thread-safe for different windows
unsafe impl Send for SendableAXElement {}
unsafe impl Sync for SendableAXElement {}

// ============================================================================
// CGS Private API (Window Server direct access)
// ============================================================================
//
// These are private APIs that communicate directly with the Window Server.
// They're faster than the Accessibility API but may break with macOS updates.
// Use `STACHE_USE_CGS=1` environment variable to enable (experimental).

// ============================================================================
// Accessibility API
// ============================================================================

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *mut *mut c_void,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: *const c_void,
        value: *const c_void,
    ) -> AXError;
    fn AXUIElementGetTypeID() -> u64;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: *const c_void) -> AXError;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFGetTypeID(cf: *const c_void) -> u64;
    fn CFArrayGetCount(array: *const c_void) -> i64;
    fn CFArrayGetValueAtIndex(array: *const c_void, idx: i64) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn CFRetain(cf: *const c_void) -> *const c_void;
}

use core_foundation::array::CFArrayRef;

// Note: This matches the declaration in bar/menubar.rs
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> CFArrayRef;
}

// CGWindowListOption flags
const K_CG_WINDOW_LIST_OPTION_ALL: u32 = 0;
const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1;
const K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;

// CGWindowInfo dictionary keys
const K_CG_WINDOW_NUMBER: &str = "kCGWindowNumber";
const K_CG_WINDOW_OWNER_PID: &str = "kCGWindowOwnerPID";
const K_CG_WINDOW_NAME: &str = "kCGWindowName";
const K_CG_WINDOW_OWNER_NAME: &str = "kCGWindowOwnerName";
const K_CG_WINDOW_BOUNDS: &str = "kCGWindowBounds";
const K_CG_WINDOW_LAYER: &str = "kCGWindowLayer";

// ============================================================================
// Cached CFStrings (thread-local for performance)
// ============================================================================

thread_local! {
    static CF_WINDOWS: OnceCell<CFString> = const { OnceCell::new() };
    static CF_TITLE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_ROLE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_SUBROLE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_POSITION: OnceCell<CFString> = const { OnceCell::new() };
    static CF_SIZE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_FOCUSED: OnceCell<CFString> = const { OnceCell::new() };
    static CF_FOCUSED_WINDOW: OnceCell<CFString> = const { OnceCell::new() };
    static CF_MINIMIZED: OnceCell<CFString> = const { OnceCell::new() };
    static CF_HIDDEN: OnceCell<CFString> = const { OnceCell::new() };
    static CF_MAIN: OnceCell<CFString> = const { OnceCell::new() };
    static CF_RAISE: OnceCell<CFString> = const { OnceCell::new() };
}

/// Gets or creates a cached `CFString`.
macro_rules! cached_cfstring {
    ($cell:expr, $value:expr) => {
        $cell.with(|cell| cell.get_or_init(|| CFString::new($value)).as_concrete_TypeRef().cast())
    };
}

#[inline]
fn cf_windows() -> *const c_void { cached_cfstring!(CF_WINDOWS, "AXWindows") }

#[inline]
fn cf_title() -> *const c_void { cached_cfstring!(CF_TITLE, "AXTitle") }

#[inline]
fn cf_role() -> *const c_void { cached_cfstring!(CF_ROLE, "AXRole") }

#[inline]
fn cf_subrole() -> *const c_void { cached_cfstring!(CF_SUBROLE, "AXSubrole") }

#[inline]
fn cf_position() -> *const c_void { cached_cfstring!(CF_POSITION, "AXPosition") }

#[inline]
fn cf_size() -> *const c_void { cached_cfstring!(CF_SIZE, "AXSize") }

#[inline]
fn cf_focused() -> *const c_void { cached_cfstring!(CF_FOCUSED, "AXFocused") }

#[inline]
fn cf_focused_window() -> *const c_void { cached_cfstring!(CF_FOCUSED_WINDOW, "AXFocusedWindow") }

#[inline]
fn cf_minimized() -> *const c_void { cached_cfstring!(CF_MINIMIZED, "AXMinimized") }

#[inline]
fn cf_hidden() -> *const c_void { cached_cfstring!(CF_HIDDEN, "AXHidden") }

#[inline]
fn cf_main() -> *const c_void { cached_cfstring!(CF_MAIN, "AXMain") }

#[inline]
fn cf_raise() -> *const c_void { cached_cfstring!(CF_RAISE, "AXRaise") }

// ============================================================================
// AXUIElement Attribute Helpers
// ============================================================================

/// Gets a string attribute from an `AXUIElement`.
#[inline]
unsafe fn get_ax_string(element: AXUIElementRef, attr: *const c_void) -> Option<String> {
    if element.is_null() {
        return None;
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, attr, &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }

    let cf_string_type_id = CFString::type_id() as u64;
    if unsafe { CFGetTypeID(value) } != cf_string_type_id {
        unsafe { CFRelease(value) };
        return None;
    }

    let cf_string = unsafe { CFString::wrap_under_get_rule(value.cast()) };
    let string = cf_string.to_string();
    unsafe { CFRelease(value) };

    Some(string)
}

/// Gets a boolean attribute from an `AXUIElement`.
#[inline]
unsafe fn get_ax_bool(element: AXUIElementRef, attr: *const c_void) -> Option<bool> {
    if element.is_null() {
        return None;
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, attr, &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }

    let bool_value = unsafe { CFBoolean::wrap_under_get_rule(value.cast()) };
    let result = bool_value.into();
    unsafe { CFRelease(value) };

    Some(result)
}

/// Gets the position (`AXPosition`) of an `AXUIElement` as (x, y).
#[inline]
unsafe fn get_ax_position(element: AXUIElementRef) -> Option<(f64, f64)> {
    if element.is_null() {
        return None;
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, cf_position(), &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }

    // AXPosition returns an AXValue of type kAXValueTypeCGPoint
    let mut point: core_graphics::geometry::CGPoint =
        core_graphics::geometry::CGPoint::new(0.0, 0.0);
    let success = unsafe {
        AXValueGetValue(
            value.cast(),
            1, // kAXValueTypeCGPoint
            (&raw mut point).cast(),
        )
    };

    unsafe { CFRelease(value) };

    if success {
        Some((point.x, point.y))
    } else {
        None
    }
}

/// Gets the size (`AXSize`) of an `AXUIElement` as (width, height).
#[inline]
unsafe fn get_ax_size(element: AXUIElementRef) -> Option<(f64, f64)> {
    if element.is_null() {
        return None;
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, cf_size(), &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }

    // AXSize returns an AXValue of type kAXValueTypeCGSize
    let mut size: core_graphics::geometry::CGSize = core_graphics::geometry::CGSize::new(0.0, 0.0);
    let success = unsafe {
        AXValueGetValue(
            value.cast(),
            2, // kAXValueTypeCGSize
            (&raw mut size).cast(),
        )
    };

    unsafe { CFRelease(value) };

    if success {
        Some((size.width, size.height))
    } else {
        None
    }
}

/// Gets the frame (position + size) of an `AXUIElement`.
#[inline]
unsafe fn get_ax_frame(element: AXUIElementRef) -> Option<Rect> {
    let (x, y) = unsafe { get_ax_position(element)? };
    let (width, height) = unsafe { get_ax_size(element)? };
    Some(Rect::new(x, y, width, height))
}

/// Sets the position of an `AXUIElement`.
#[inline]
unsafe fn set_ax_position(element: AXUIElementRef, x: f64, y: f64) -> bool {
    if element.is_null() {
        return false;
    }

    let point = core_graphics::geometry::CGPoint::new(x, y);
    let value = unsafe {
        AXValueCreate(
            1, // kAXValueTypeCGPoint
            (&raw const point).cast(),
        )
    };

    if value.is_null() {
        return false;
    }

    let result = unsafe { AXUIElementSetAttributeValue(element, cf_position(), value.cast()) };
    unsafe { CFRelease(value.cast()) };

    result == K_AX_ERROR_SUCCESS
}

/// Sets the size of an `AXUIElement`.
#[inline]
unsafe fn set_ax_size(element: AXUIElementRef, width: f64, height: f64) -> bool {
    if element.is_null() {
        return false;
    }

    let size = core_graphics::geometry::CGSize::new(width, height);
    let value = unsafe {
        AXValueCreate(
            2, // kAXValueTypeCGSize
            (&raw const size).cast(),
        )
    };

    if value.is_null() {
        return false;
    }

    let result = unsafe { AXUIElementSetAttributeValue(element, cf_size(), value.cast()) };
    unsafe { CFRelease(value.cast()) };

    result == K_AX_ERROR_SUCCESS
}

/// Sets the frame (position + size) of an `AXUIElement`.
#[inline]
unsafe fn set_ax_frame(element: AXUIElementRef, frame: &Rect) -> bool {
    // Order of operations matters for reliable resizing:
    // 1. Set size first - this ensures the window can shrink before moving
    // 2. Set position - move the (now smaller) window to target location
    // 3. Set size again - some apps need this to fully apply the size change
    //
    // This handles cases where:
    // - Window is larger than target and needs to shrink
    // - App has constraints that depend on position
    let size_ok_1 = unsafe { set_ax_size(element, frame.width, frame.height) };
    let pos_ok = unsafe { set_ax_position(element, frame.x, frame.y) };
    let size_ok_2 = unsafe { set_ax_size(element, frame.width, frame.height) };

    // Consider success if position and at least one size operation succeeded
    pos_ok && (size_ok_1 || size_ok_2)
}

/// Fast frame setter for animations - only 2 AX calls instead of 3.
///
/// During animations, windows move in small increments so we don't need
/// the defensive double-size-set that `set_ax_frame` uses. This reduces
/// AX API calls by 33%.
#[inline]
unsafe fn set_ax_frame_fast(element: AXUIElementRef, frame: &Rect) -> bool {
    // For animations: position first, then size (2 calls instead of 3)
    let pos_ok = unsafe { set_ax_position(element, frame.x, frame.y) };
    let size_ok = unsafe { set_ax_size(element, frame.width, frame.height) };
    pos_ok && size_ok
}

/// Minimum pixel change to trigger an AX update.
/// Changes smaller than this are imperceptible and skipped to reduce API calls.
const MIN_FRAME_DELTA: f64 = 0.5;

/// Smart frame setter that only updates changed properties.
///
/// Compares the new frame to the previous frame and:
/// - Skips position update if position hasn't changed significantly
/// - Skips size update if size hasn't changed significantly
/// - Returns early if nothing changed
///
/// This can reduce AX API calls by 50-100% for windows that are nearly stationary.
#[inline]
unsafe fn set_ax_frame_delta(element: AXUIElementRef, new_frame: &Rect, prev_frame: &Rect) -> bool {
    let pos_changed = (new_frame.x - prev_frame.x).abs() >= MIN_FRAME_DELTA
        || (new_frame.y - prev_frame.y).abs() >= MIN_FRAME_DELTA;

    let size_changed = (new_frame.width - prev_frame.width).abs() >= MIN_FRAME_DELTA
        || (new_frame.height - prev_frame.height).abs() >= MIN_FRAME_DELTA;

    // Skip if nothing changed
    if !pos_changed && !size_changed {
        return true; // Consider it successful - nothing to do
    }

    let mut success = true;

    if pos_changed {
        success &= unsafe { set_ax_position(element, new_frame.x, new_frame.y) };
    }

    if size_changed {
        success &= unsafe { set_ax_size(element, new_frame.width, new_frame.height) };
    }

    success
}

/// Raises (brings to front) an `AXUIElement` window.
#[inline]
unsafe fn raise_ax_window(element: AXUIElementRef) -> bool {
    if element.is_null() {
        return false;
    }
    let result = unsafe { AXUIElementPerformAction(element, cf_raise()) };
    result == K_AX_ERROR_SUCCESS
}

// AXValue FFI
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXValueCreate(value_type: i32, value: *const c_void) -> *mut c_void;
    fn AXValueGetValue(value: *const c_void, value_type: i32, value_ptr: *mut c_void) -> bool;
}

// ============================================================================
// Application and Window Enumeration
// ============================================================================

/// Information about a running application.
#[derive(Debug, Clone)]
pub struct AppInfo {
    /// Process ID.
    pub pid: i32,
    /// Bundle identifier (e.g., "com.apple.Safari").
    pub bundle_id: String,
    /// Application name (e.g., "Safari").
    pub name: String,
    /// Whether the app is currently hidden.
    pub is_hidden: bool,
    /// `AXUIElement` for this application (for internal use).
    ax_element: AXUIElementRef,
}

/// Gets all running applications that can own windows.
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

            let ax_element = AXUIElementCreateApplication(pid);

            result.push(AppInfo {
                pid,
                bundle_id,
                name,
                is_hidden: is_hidden == YES,
                ax_element,
            });
        }

        result
    }
}

/// Information about a window from `CGWindowList`.
#[derive(Debug, Clone)]
pub struct CGWindowInfo {
    /// Window ID (`CGWindowID`).
    pub id: u32,
    /// Owner process ID.
    pub pid: i32,
    /// Window title (may be empty).
    pub title: String,
    /// Owner application name.
    pub app_name: String,
    /// Window bounds.
    pub frame: Rect,
    /// Window layer (0 = normal windows).
    pub layer: i32,
}

/// Gets all on-screen windows using `CGWindowListCopyWindowInfo`.
///
/// This provides window IDs and basic info, but more details require
/// the Accessibility API (`AXUIElement`).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn get_cg_window_list() -> Vec<CGWindowInfo> { get_cg_window_list_internal(true) }

/// Gets ALL windows (including hidden/minimized) using `CGWindowListCopyWindowInfo`.
///
/// Use this for initial tracking to ensure hidden windows are also tracked.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn get_cg_window_list_all() -> Vec<CGWindowInfo> { get_cg_window_list_internal(false) }

/// Internal function to get window list with configurable options.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn get_cg_window_list_internal(on_screen_only: bool) -> Vec<CGWindowInfo> {
    unsafe {
        let options = if on_screen_only {
            K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY | K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS
        } else {
            K_CG_WINDOW_LIST_OPTION_ALL | K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS
        };
        let window_list = CGWindowListCopyWindowInfo(options, 0);

        if window_list.is_null() {
            return Vec::new();
        }

        let window_list_ptr: *const c_void = window_list.cast();
        let count = CFArrayGetCount(window_list_ptr);
        let mut result = Vec::new();

        for i in 0..count {
            let dict = CFArrayGetValueAtIndex(window_list_ptr, i);
            if dict.is_null() {
                continue;
            }

            // Get window number (ID)
            let id = get_cf_dict_number(dict, K_CG_WINDOW_NUMBER).unwrap_or(0) as u32;
            if id == 0 {
                continue;
            }

            // Get owner PID
            let pid = get_cf_dict_number(dict, K_CG_WINDOW_OWNER_PID).unwrap_or(0) as i32;
            if pid <= 0 {
                continue;
            }

            // Get window layer (skip non-normal windows like menubar, dock, etc.)
            let layer = get_cf_dict_number(dict, K_CG_WINDOW_LAYER).unwrap_or(0) as i32;
            if layer != 0 {
                continue;
            }

            // Get window title
            let title = get_cf_dict_string(dict, K_CG_WINDOW_NAME).unwrap_or_default();

            // Get owner name
            let app_name = get_cf_dict_string(dict, K_CG_WINDOW_OWNER_NAME).unwrap_or_default();

            // Get bounds
            let frame = get_cf_dict_rect(dict, K_CG_WINDOW_BOUNDS).unwrap_or_default();

            result.push(CGWindowInfo {
                id,
                pid,
                title,
                app_name,
                frame,
                layer,
            });
        }

        CFRelease(window_list_ptr);
        result
    }
}

/// Extended window information combining `CGWindowList` and Accessibility API data.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // All bools represent independent window states
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
    /// Whether the window is minimized.
    pub is_minimized: bool,
    /// Whether the app is hidden.
    pub is_hidden: bool,
    /// Whether this is the main window of its app.
    pub is_main: bool,
    /// Whether this window has focus.
    pub is_focused: bool,
    /// `AXUIElement` for this window (for operations).
    ax_element: Option<AXUIElementRef>,
}

/// Gets all visible windows with full details.
///
/// This combines `CGWindowList` (for window IDs) with Accessibility API
/// (for detailed attributes and control).
///
/// Note: This only returns on-screen windows. Use `get_all_windows_including_hidden()`
/// to also get windows from hidden/minimized apps.
pub fn get_all_windows() -> Vec<WindowInfo> { get_all_windows_internal(false) }

/// Gets ALL windows including those from hidden/minimized apps.
///
/// This should be used for initial window tracking at startup to ensure
/// all windows are tracked regardless of their visibility state.
pub fn get_all_windows_including_hidden() -> Vec<WindowInfo> { get_all_windows_internal(true) }

/// Internal function to get windows with configurable hidden app handling.
///
/// This uses an AX-first approach: we enumerate AX windows (the source of truth)
/// and match them to CG windows to get their IDs. This avoids issues with:
/// - Frame-based deduplication incorrectly merging distinct windows
/// - Stale `CGWindowList` data
fn get_all_windows_internal(include_hidden: bool) -> Vec<WindowInfo> {
    // Use the full CG window list for matching.
    // Some apps (like Ghostty) don't appear in ON_SCREEN_ONLY list even when visible.
    // We use the full list and filter by frame matching to get the right window.
    let cg_windows = get_cg_window_list_all();

    // Build a map of PID -> bundle_id from running apps
    let apps = get_running_apps();
    let pid_to_bundle: HashMap<i32, &str> =
        apps.iter().map(|app| (app.pid, app.bundle_id.as_str())).collect();
    let pid_to_hidden: HashMap<i32, bool> =
        apps.iter().map(|app| (app.pid, app.is_hidden)).collect();

    // Group CG windows by PID for matching with AX windows
    // Filter out non-standard windows (menus, tooltips, etc.) by layer or tiny size
    let mut pid_to_cg_windows: HashMap<i32, Vec<&CGWindowInfo>> = HashMap::new();
    for win in &cg_windows {
        // Skip tiny windows (likely menus, tooltips, etc.)
        if win.frame.width < 50.0 || win.frame.height < 50.0 {
            continue;
        }
        pid_to_cg_windows.entry(win.pid).or_default().push(win);
    }

    let mut result = Vec::new();

    // For each app, enumerate AX windows and match to CG windows
    for app in &apps {
        let ax_windows = unsafe { get_app_ax_windows(app.ax_element) };

        if ax_windows.is_empty() {
            continue;
        }

        // Get CG windows for this app (if any)
        let cg_wins = pid_to_cg_windows.get(&app.pid).map_or(&[][..], |v| v.as_slice());

        // Track which CG windows have been matched to avoid double-matching
        let mut matched_cg_ids: std::collections::HashSet<u32> = std::collections::HashSet::new();

        // Process each AX window (AX is the source of truth)
        for ax in &ax_windows {
            let ax_frame = unsafe { get_ax_frame(*ax) };
            let Some(frame) = ax_frame else {
                continue; // Skip windows without a valid frame
            };

            let title = unsafe { get_ax_string(*ax, cf_title()) }.unwrap_or_default();
            let is_minimized = unsafe { get_ax_bool(*ax, cf_minimized()) }.unwrap_or(false);
            let is_main = unsafe { get_ax_bool(*ax, cf_main()) }.unwrap_or(false);
            let is_focused = unsafe { get_ax_bool(*ax, cf_focused()) }.unwrap_or(false);

            // Find the best matching CG window for this AX window
            // For apps with native tabs (multiple CG windows at same frame), pick lowest ID
            let cg_match = find_best_cg_match(&frame, cg_wins, &matched_cg_ids);

            let (window_id, is_hidden) = if let Some(cg_win) = cg_match {
                matched_cg_ids.insert(cg_win.id);
                (cg_win.id, *pid_to_hidden.get(&app.pid).unwrap_or(&false))
            } else if include_hidden {
                // No CG match - generate a synthetic ID for hidden window
                let hidden_id = generate_hidden_window_id(app.pid, &title, &frame);
                (hidden_id, true)
            } else {
                continue; // Skip windows without CG match when not including hidden
            };

            result.push(WindowInfo {
                id: window_id,
                pid: app.pid,
                bundle_id: pid_to_bundle.get(&app.pid).unwrap_or(&"").to_string(),
                app_name: app.name.clone(),
                title,
                frame,
                is_minimized,
                is_hidden,
                is_main,
                is_focused,
                ax_element: Some(*ax),
            });
        }
    }

    result
}

/// Finds the best matching CG window for an AX window frame.
///
/// Returns the CG window with the closest frame match that hasn't been matched yet.
/// When multiple windows have the same frame (native tabs), prefers the lowest ID
/// for consistency (lower IDs tend to be more stable and positionable).
fn find_best_cg_match<'a>(
    ax_frame: &Rect,
    cg_windows: &[&'a CGWindowInfo],
    matched_ids: &std::collections::HashSet<u32>,
) -> Option<&'a CGWindowInfo> {
    let mut best_match: Option<&'a CGWindowInfo> = None;
    let mut best_distance = f64::MAX;
    let mut best_id = u32::MAX;

    for cg_win in cg_windows {
        // Skip already matched windows
        if matched_ids.contains(&cg_win.id) {
            continue;
        }

        // Calculate frame distance
        let dx = (ax_frame.x - cg_win.frame.x).abs();
        let dy = (ax_frame.y - cg_win.frame.y).abs();
        let dw = (ax_frame.width - cg_win.frame.width).abs();
        let dh = (ax_frame.height - cg_win.frame.height).abs();
        let distance = dx + dy + dw + dh;

        // Only consider matches within reasonable tolerance (10 pixels total)
        if distance < 10.0 {
            // Prefer closer matches, or lower ID on ties (more stable for native tabs)
            // Use small epsilon for float comparison (0.1 pixel)
            let is_tie = (distance - best_distance).abs() < 0.1;
            if distance < best_distance || (is_tie && cg_win.id < best_id) {
                best_distance = distance;
                best_id = cg_win.id;
                best_match = Some(cg_win);
            }
        }
    }

    best_match
}

/// Deduplicates windows that are native tabs of the same window.
///
/// macOS assigns unique `CGWindowID`s to each native tab, but they all share
/// the same frame (position and size). This function groups windows by their
/// frame and keeps only one representative window per frame.
///
/// The window with the highest ID is kept, as it's typically the most recently
/// active tab and macOS tends to give it the AX focus attributes.
#[allow(dead_code)] // Only used in tests
#[allow(clippy::cast_possible_truncation)] // Window coordinates won't exceed i32 range
fn deduplicate_tabbed_windows<'a>(windows: &[&'a CGWindowInfo]) -> Vec<&'a CGWindowInfo> {
    // Group windows by their frame (rounded to avoid floating point issues)
    let mut frame_groups: HashMap<(i32, i32, i32, i32), Vec<&'a CGWindowInfo>> = HashMap::new();

    for win in windows {
        let key = (
            win.frame.x.round() as i32,
            win.frame.y.round() as i32,
            win.frame.width.round() as i32,
            win.frame.height.round() as i32,
        );
        frame_groups.entry(key).or_default().push(win);
    }

    // For each group, keep only one window (the one with highest ID - typically the active tab)
    frame_groups
        .into_values()
        .filter_map(|group| group.into_iter().max_by_key(|w| w.id))
        .collect()
}

/// Generates a unique ID for hidden windows that don't have a `CGWindowID`.
///
/// Uses a hash of the app PID, title, and frame to create a consistent ID.
#[allow(clippy::cast_possible_truncation)] // Intentional truncation for hash
fn generate_hidden_window_id(pid: i32, title: &str, frame: &Rect) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    pid.hash(&mut hasher);
    title.hash(&mut hasher);
    // Round frame values to avoid floating point issues
    (frame.x as i32).hash(&mut hasher);
    (frame.y as i32).hash(&mut hasher);
    (frame.width as i32).hash(&mut hasher);
    (frame.height as i32).hash(&mut hasher);
    // Use high bits to avoid collision with real CGWindowIDs (which are typically low numbers)
    (hasher.finish() as u32) | 0x8000_0000
}

/// Gets AX windows for an application.
unsafe fn get_app_ax_windows(app_element: AXUIElementRef) -> Vec<AXUIElementRef> {
    if app_element.is_null() {
        return Vec::new();
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result =
        unsafe { AXUIElementCopyAttributeValue(app_element, cf_windows(), &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return Vec::new();
    }

    let count = unsafe { CFArrayGetCount(value) };
    if count <= 0 {
        unsafe { CFRelease(value) };
        return Vec::new();
    }

    let ax_type_id = unsafe { AXUIElementGetTypeID() };

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mut windows = Vec::with_capacity(count as usize);

    for i in 0..count {
        let window = unsafe { CFArrayGetValueAtIndex(value, i) };
        if !window.is_null() && unsafe { CFGetTypeID(window) } == ax_type_id {
            // Check if it's a real window (has AXWindow role)
            if let Some(role) = unsafe { get_ax_string(window.cast_mut(), cf_role()) }
                && role == "AXWindow"
            {
                unsafe { CFRetain(window) };
                windows.push(window.cast_mut());
            }
        }
    }

    unsafe { CFRelease(value) };
    windows
}

// ============================================================================
// Window Operations
// ============================================================================

/// Gets the currently focused window.
///
/// This function gets the focused window by:
/// 1. Getting the frontmost application
/// 2. Getting that app's focused window (`AXFocusedWindow`)
/// 3. Matching it with windows from `CGWindowList` to get the window ID
pub fn get_focused_window() -> Option<WindowInfo> { unsafe { get_focused_window_unsafe() } }

/// Internal implementation for getting the focused window.
///
/// # Safety
///
/// Uses Objective-C runtime and Accessibility API calls.
#[allow(clippy::too_many_lines)]
unsafe fn get_focused_window_unsafe() -> Option<WindowInfo> {
    // Get the frontmost application
    let workspace_class = Class::get("NSWorkspace")?;

    let shared_workspace: *mut Object = msg_send![workspace_class, sharedWorkspace];
    if shared_workspace.is_null() {
        return None;
    }

    let frontmost_app: *mut Object = msg_send![shared_workspace, frontmostApplication];
    if frontmost_app.is_null() {
        return None;
    }

    // Get the PID of the frontmost app
    let pid: i32 = msg_send![frontmost_app, processIdentifier];
    if pid <= 0 {
        return None;
    }

    // Get the bundle ID
    let bundle_id_ns: *mut Object = msg_send![frontmost_app, bundleIdentifier];
    let bundle_id = if bundle_id_ns.is_null() {
        String::new()
    } else {
        let utf8: *const i8 = msg_send![bundle_id_ns, UTF8String];
        if utf8.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(utf8) }.to_string_lossy().into_owned()
        }
    };

    // Get the app name
    let app_name_ns: *mut Object = msg_send![frontmost_app, localizedName];
    let app_name = if app_name_ns.is_null() {
        String::new()
    } else {
        let utf8: *const i8 = msg_send![app_name_ns, UTF8String];
        if utf8.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(utf8) }.to_string_lossy().into_owned()
        }
    };

    // Create AXUIElement for the app and get its focused window
    let app_element = unsafe { AXUIElementCreateApplication(pid) };
    if app_element.is_null() {
        return None;
    }

    // Get the focused window of this app
    let mut focused_window_ref: *mut c_void = ptr::null_mut();
    let result = unsafe {
        AXUIElementCopyAttributeValue(app_element, cf_focused_window(), &raw mut focused_window_ref)
    };

    unsafe { CFRelease(app_element.cast()) };

    if result != K_AX_ERROR_SUCCESS || focused_window_ref.is_null() {
        return None;
    }

    // Get the window's frame from AX
    let ax_frame = unsafe { get_ax_frame(focused_window_ref.cast()) }?;

    // Get window title
    let title = unsafe { get_ax_string(focused_window_ref.cast(), cf_title()) }.unwrap_or_default();

    // Get minimized state
    let is_minimized =
        unsafe { get_ax_bool(focused_window_ref.cast(), cf_minimized()) }.unwrap_or(false);

    // Check if app is hidden
    let is_hidden: BOOL = msg_send![frontmost_app, isHidden];

    // Now match with CGWindowList to get the window ID
    // Some apps (especially GPU-accelerated ones like Ghostty) may report slightly
    // different frames between AX and CG APIs, so we use multiple matching strategies.
    let cg_list = get_cg_window_list();
    let app_windows: Vec<&CGWindowInfo> = cg_list.iter().filter(|cg| cg.pid == pid).collect();

    // Strategy 1: Match by frame with tight tolerance (2px)
    let mut cg_match: Option<&CGWindowInfo> = app_windows.iter().copied().find(|cg| {
        (cg.frame.x - ax_frame.x).abs() < 2.0
            && (cg.frame.y - ax_frame.y).abs() < 2.0
            && (cg.frame.width - ax_frame.width).abs() < 2.0
            && (cg.frame.height - ax_frame.height).abs() < 2.0
    });

    // Strategy 2: Match by frame with looser tolerance (10px) - helps with GPU-rendered apps
    if cg_match.is_none() {
        cg_match = app_windows.iter().copied().find(|cg| {
            (cg.frame.x - ax_frame.x).abs() < 10.0
                && (cg.frame.y - ax_frame.y).abs() < 10.0
                && (cg.frame.width - ax_frame.width).abs() < 10.0
                && (cg.frame.height - ax_frame.height).abs() < 10.0
        });
    }

    // Strategy 3: Match by title if there's exactly one window with that title
    if cg_match.is_none() && !title.is_empty() {
        let title_matches: Vec<_> =
            app_windows.iter().copied().filter(|cg| cg.title == title).collect();
        if title_matches.len() == 1 {
            cg_match = Some(title_matches[0]);
        }
    }

    // Strategy 4: If there's only one window from this app, use it
    if cg_match.is_none() && app_windows.len() == 1 {
        cg_match = Some(app_windows[0]);
    }

    let window_id = cg_match.map_or(0, |cg| cg.id);

    unsafe { CFRelease(focused_window_ref) };

    Some(WindowInfo {
        id: window_id,
        pid,
        bundle_id,
        app_name,
        title,
        frame: ax_frame,
        is_minimized,
        is_hidden: is_hidden == YES,
        is_main: true, // The focused window is the main window
        is_focused: true,
        ax_element: None, // We already released it
    })
}

/// Sets the frame (position and size) of a window by ID.
///
/// # Errors
///
/// Returns `TilingError::WindowNotFound` if the window ID is not found.
/// Returns `TilingError::WindowOperation` if the AX element is unavailable or the frame cannot be set.
pub fn set_window_frame(window_id: u32, frame: &Rect) -> TilingResult<()> {
    let windows = get_all_windows();
    let window = windows
        .iter()
        .find(|w| w.id == window_id)
        .ok_or(TilingError::WindowNotFound(window_id))?;

    let ax_element = window
        .ax_element
        .ok_or_else(|| TilingError::window_op(format!("No AX element for window {window_id}")))?;

    if unsafe { set_ax_frame(ax_element, frame) } {
        Ok(())
    } else {
        Err(TilingError::window_op(format!(
            "Failed to set frame for window {window_id}"
        )))
    }
}

/// Sets the frame of a window using the app's PID and the window's current frame.
///
/// This is more reliable than `set_window_frame` for recently shown windows,
/// as it doesn't depend on `CGWindowList` which may have stale data.
///
/// # Arguments
///
/// * `pid` - Process ID of the app owning the window
/// * `current_frame` - The window's current/expected frame (used to identify the window)
/// * `new_frame` - The new frame to set
///
/// # Returns
///
/// `true` if the window was found and repositioned successfully.
pub fn set_window_frame_by_pid(pid: i32, current_frame: &Rect, new_frame: &Rect) -> bool {
    // For single-window operations, delegate to the batch function
    set_windows_for_pid(pid, &[(*current_frame, *new_frame)]) > 0
}

/// Sets frames for multiple windows belonging to the same app (PID).
///
/// This function matches all windows to their target frames BEFORE moving any,
/// preventing the issue where moving one window causes another to be mismatched.
///
/// # Arguments
///
/// * `pid` - Process ID of the app
/// * `window_frames` - Slice of (`current_frame`, `target_frame`) pairs
///
/// # Returns
///
/// Number of windows successfully repositioned.
pub fn set_windows_for_pid(pid: i32, window_frames: &[(Rect, Rect)]) -> usize {
    if window_frames.is_empty() {
        return 0;
    }

    unsafe {
        let app_element = AXUIElementCreateApplication(pid);
        if app_element.is_null() {
            eprintln!(
                "stache: tiling: set_windows_for_pid: failed to create AX element for pid {pid}"
            );
            return 0;
        }

        let ax_windows = get_app_ax_windows(app_element);
        CFRelease(app_element.cast());

        if ax_windows.is_empty() {
            return 0;
        }

        // Get current frames for all AX windows
        let ax_frames: Vec<Option<Rect>> = ax_windows.iter().map(|w| get_ax_frame(*w)).collect();

        // Match each target to an AX window using Hungarian-style greedy matching
        // This ensures each AX window is matched to at most one target
        let mut used_ax_indices: std::collections::HashSet<usize> =
            std::collections::HashSet::new();
        let mut matches: Vec<(usize, Rect)> = Vec::with_capacity(window_frames.len());

        for (current_frame, target_frame) in window_frames {
            let mut best_match: Option<(usize, f64)> = None;

            for (i, ax_frame) in ax_frames.iter().enumerate() {
                if used_ax_indices.contains(&i) {
                    continue; // Already matched
                }

                if let Some(frame) = ax_frame {
                    let distance = (frame.x - current_frame.x).abs()
                        + (frame.y - current_frame.y).abs()
                        + (frame.width - current_frame.width).abs()
                        + (frame.height - current_frame.height).abs();

                    if best_match.is_none() || distance < best_match.unwrap().1 {
                        best_match = Some((i, distance));
                    }
                }
            }

            if let Some((idx, _distance)) = best_match {
                used_ax_indices.insert(idx);
                matches.push((idx, *target_frame));
            }
        }

        // Now apply all the frames (after all matching is done)
        let mut success_count = 0;
        for (ax_idx, target_frame) in matches {
            if set_ax_frame(ax_windows[ax_idx], &target_frame) {
                success_count += 1;
            }
        }

        // Clean up AX references
        for w in ax_windows {
            CFRelease(w.cast());
        }

        success_count
    }
}

/// Sets the frame of a window with retry logic.
///
/// This is useful when windows were just unhidden and may take time to appear.
///
/// # Arguments
///
/// * `pid` - Process ID of the app owning the window
/// * `current_frame` - The window's current/expected frame
/// * `new_frame` - The new frame to set
/// * `max_attempts` - Maximum number of attempts
///
/// # Errors
///
/// Returns `TilingError::WindowOperation` if the window cannot be repositioned after all attempts.
pub fn set_window_frame_with_retry(
    pid: i32,
    current_frame: &Rect,
    new_frame: &Rect,
    max_attempts: u32,
) -> TilingResult<()> {
    for attempt in 0..max_attempts {
        if set_window_frame_by_pid(pid, current_frame, new_frame) {
            if attempt > 0 {
                eprintln!(
                    "stache: tiling: set_window_frame_with_retry: succeeded on attempt {}",
                    attempt + 1
                );
            }
            return Ok(());
        }

        if attempt < max_attempts - 1 {
            // Wait a bit before retrying - increase wait time with each attempt
            std::thread::sleep(std::time::Duration::from_millis(10 * u64::from(attempt + 1)));
        }
    }

    Err(TilingError::window_op(format!(
        "Failed to set window frame after {max_attempts} attempts for pid {pid}"
    )))
}

/// Sets frames for multiple windows by their IDs.
///
/// This is more reliable than frame-based matching as it uses window IDs directly.
/// Gets the window list once and positions all windows from it.
///
/// # Arguments
///
/// * `window_frames` - Pairs of (`window_id`, `new_frame`)
///
/// # Returns
///
/// Number of windows successfully positioned.
pub fn set_window_frames_by_id(window_frames: &[(u32, Rect)]) -> usize {
    if window_frames.is_empty() {
        return 0;
    }

    // Get all windows once
    let windows = get_all_windows();

    // Build a map of window_id -> ax_element
    let window_map: std::collections::HashMap<u32, _> =
        windows.iter().filter_map(|w| w.ax_element.map(|ax| (w.id, ax))).collect();

    eprintln!(
        "stache: tiling: set_window_frames_by_id: {} targets, {} available windows with AX",
        window_frames.len(),
        window_map.len()
    );

    // Check for duplicate window IDs in the input (should never happen)
    let mut seen_ids: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (window_id, _) in window_frames {
        if !seen_ids.insert(*window_id) {
            eprintln!("stache: tiling: BUG: duplicate window ID {window_id} in layout result!");
        }
    }

    let mut success_count = 0;
    let mut missing_ids = Vec::new();

    for (window_id, new_frame) in window_frames {
        if let Some(&ax_element) = window_map.get(window_id) {
            eprintln!(
                "stache: tiling: positioning window {} to ({:.0}, {:.0}, {:.0}x{:.0})",
                window_id, new_frame.x, new_frame.y, new_frame.width, new_frame.height
            );
            if unsafe { set_ax_frame(ax_element, new_frame) } {
                success_count += 1;
            }
        } else {
            missing_ids.push(*window_id);
        }
    }

    if !missing_ids.is_empty() {
        eprintln!(
            "stache: tiling: set_window_frames_by_id: {} window IDs not found: {:?}",
            missing_ids.len(),
            missing_ids
        );
        eprintln!(
            "stache: tiling: set_window_frames_by_id: available IDs: {:?}",
            window_map.keys().collect::<Vec<_>>()
        );
    }

    success_count
}

/// Resolves window IDs to their AX element references.
///
/// This is used to cache AX elements for animations, avoiding repeated
/// calls to `get_all_windows()` on every frame.
///
/// # Arguments
///
/// * `window_ids` - List of window IDs to resolve
///
/// # Returns
///
/// A map of `window_id` -> `AXUIElementRef` for windows that were found.
pub fn resolve_window_ax_elements(
    window_ids: &[u32],
) -> std::collections::HashMap<u32, AXUIElementRef> {
    let windows = get_all_windows();

    window_ids
        .iter()
        .filter_map(|&id| {
            windows
                .iter()
                .find(|w| w.id == id)
                .and_then(|w| w.ax_element.map(|ax| (id, ax)))
        })
        .collect()
}

/// Sets frames for multiple windows using cached AX element references.
///
/// This is the fast path for animations - it doesn't query the window list,
/// just directly positions windows using pre-resolved AX elements.
/// Uses `set_ax_frame_fast` which makes only 2 AX calls per window instead of 3.
///
/// Windows are positioned in parallel using rayon for maximum throughput.
///
/// # Arguments
///
/// * `frames` - Pairs of (`AXUIElementRef`, `new_frame`)
///
/// # Returns
///
/// Number of windows successfully positioned.
///
/// # Safety
///
/// The AX element references must be valid. They should be obtained from
/// `resolve_window_ax_elements` and used within the same animation sequence.
pub fn set_window_frames_direct(frames: &[(AXUIElementRef, Rect)]) -> usize {
    if frames.is_empty() {
        return 0;
    }

    // For single window, skip parallel overhead
    if frames.len() == 1 {
        let (ax_element, new_frame) = &frames[0];
        return usize::from(unsafe { set_ax_frame_fast(*ax_element, new_frame) });
    }

    // Wrap in sendable type for parallel iteration
    let sendable_frames: Vec<(SendableAXElement, Rect)> =
        frames.iter().map(|(ax, rect)| (SendableAXElement(*ax), *rect)).collect();

    // Position multiple windows in parallel
    let success_count = AtomicUsize::new(0);

    sendable_frames
        .par_iter()
        .for_each(|(SendableAXElement(ax_element), new_frame)| {
            if unsafe { set_ax_frame_fast(*ax_element, new_frame) } {
                success_count.fetch_add(1, Ordering::Relaxed);
            }
        });

    success_count.load(Ordering::Relaxed)
}

/// Sets frames for multiple windows with delta optimization.
///
/// Only updates properties (position/size) that have actually changed,
/// reducing AX API calls by up to 50-100% for windows that are nearly stationary.
///
/// # Arguments
///
/// * `frames` - Tuples of (`AXUIElementRef`, `new_frame`, `prev_frame`)
///
/// # Returns
///
/// Number of windows successfully positioned.
pub fn set_window_frames_delta(frames: &[(AXUIElementRef, Rect, Rect)]) -> usize {
    if frames.is_empty() {
        return 0;
    }

    // For single window, skip parallel overhead
    if frames.len() == 1 {
        let (ax_element, new_frame, prev_frame) = &frames[0];
        return usize::from(unsafe { set_ax_frame_delta(*ax_element, new_frame, prev_frame) });
    }

    // Wrap in sendable type for parallel iteration
    let sendable_frames: Vec<(SendableAXElement, Rect, Rect)> = frames
        .iter()
        .map(|(ax, new, prev)| (SendableAXElement(*ax), *new, *prev))
        .collect();

    // Position multiple windows in parallel
    let success_count = AtomicUsize::new(0);

    sendable_frames.par_iter().for_each(
        |(SendableAXElement(ax_element), new_frame, prev_frame)| {
            if unsafe { set_ax_frame_delta(*ax_element, new_frame, prev_frame) } {
                success_count.fetch_add(1, Ordering::Relaxed);
            }
        },
    );

    success_count.load(Ordering::Relaxed)
}

/// Sets only the position (not size) for multiple windows.
///
/// This is faster than `set_window_frames_delta` when you know the animation
/// only involves position changes (no resizing). It makes exactly 1 AX call
/// per window instead of 2.
///
/// # Arguments
///
/// * `frames` - Tuples of (`AXUIElementRef`, `new_x`, `new_y`)
///
/// # Returns
///
/// Number of windows successfully positioned.
pub fn set_window_positions_only(frames: &[(AXUIElementRef, f64, f64)]) -> usize {
    if frames.is_empty() {
        return 0;
    }

    // For single window, skip parallel overhead
    if frames.len() == 1 {
        let (ax_element, x, y) = &frames[0];
        return usize::from(unsafe { set_ax_position(*ax_element, *x, *y) });
    }

    // Wrap in sendable type for parallel iteration
    let sendable_frames: Vec<(SendableAXElement, f64, f64)> =
        frames.iter().map(|(ax, x, y)| (SendableAXElement(*ax), *x, *y)).collect();

    // Position multiple windows in parallel
    let success_count = AtomicUsize::new(0);

    sendable_frames.par_iter().for_each(|(SendableAXElement(ax_element), x, y)| {
        if unsafe { set_ax_position(*ax_element, *x, *y) } {
            success_count.fetch_add(1, Ordering::Relaxed);
        }
    });

    success_count.load(Ordering::Relaxed)
}

/// Sets frames for multiple windows using pre-resolved AX elements.
///
/// # Arguments
///
/// * `window_frames` - Pairs of (`window_id`, `new_frame`)
/// * `ax_elements` - Pre-resolved AX elements map
///
/// # Returns
///
/// Number of windows successfully positioned.
pub fn set_window_frames_auto(
    window_frames: &[(u32, Rect)],
    ax_elements: &std::collections::HashMap<u32, AXUIElementRef>,
) -> usize {
    if window_frames.is_empty() {
        return 0;
    }

    let ax_frames: Vec<(AXUIElementRef, Rect)> = window_frames
        .iter()
        .filter_map(|(id, frame)| ax_elements.get(id).map(|&ax| (ax, *frame)))
        .collect();
    set_window_frames_direct(&ax_frames)
}

/// Focuses a window by ID.
///
/// This raises the window and focuses it.
///
/// # Errors
///
/// Returns `TilingError::WindowNotFound` if the window ID is not found.
/// Returns `TilingError::WindowOperation` if the app cannot be activated or window cannot be raised.
pub fn focus_window(window_id: u32) -> TilingResult<()> {
    let windows = get_all_windows();
    let window = windows
        .iter()
        .find(|w| w.id == window_id)
        .ok_or(TilingError::WindowNotFound(window_id))?;

    // First activate the app
    if !activate_app(window.pid) {
        return Err(TilingError::window_op(format!(
            "Failed to activate app for window {window_id} (pid {})",
            window.pid
        )));
    }

    // Then raise the specific window
    if let Some(ax_element) = window.ax_element
        && !unsafe { raise_ax_window(ax_element) }
    {
        return Err(TilingError::window_op(format!(
            "Failed to raise window {window_id}"
        )));
    }

    Ok(())
}

/// Activates an application by PID.
fn activate_app(pid: i32) -> bool {
    unsafe {
        let Some(app_class) = Class::get("NSRunningApplication") else {
            return false;
        };

        let app: *mut Object = msg_send![app_class, runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            return false;
        }

        let result: BOOL = msg_send![app, activateWithOptions: 3u64]; // NSApplicationActivateIgnoringOtherApps
        result == YES
    }
}

/// Hides an application by PID.
///
/// This hides all windows of the app (like Cmd+H).
/// Includes retry logic to handle race conditions with macOS activation.
///
/// # Errors
///
/// Returns `TilingError::WindowOperation` if the app cannot be found or hidden.
pub fn hide_app(pid: i32) -> TilingResult<()> {
    unsafe {
        let app_class = Class::get("NSRunningApplication")
            .ok_or_else(|| TilingError::window_op("NSRunningApplication class not found"))?;

        let app: *mut Object = msg_send![app_class, runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            return Err(TilingError::window_op(format!(
                "No running application for pid {pid}"
            )));
        }

        // Check if already hidden
        let is_hidden: BOOL = msg_send![app, isHidden];
        if is_hidden == YES {
            return Ok(());
        }

        // Try to hide with retry logic
        // macOS activation can race with hide, so we retry a few times
        for attempt in 0..3 {
            let result: BOOL = msg_send![app, hide];
            if result == YES {
                // Verify it actually hid
                std::thread::sleep(std::time::Duration::from_millis(5));
                let is_hidden: BOOL = msg_send![app, isHidden];
                if is_hidden == YES {
                    return Ok(());
                }
                // Not hidden yet, retry after a brief delay
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            } else if attempt < 2 {
                // Hide call failed, retry after delay
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        // Final check
        let is_hidden: BOOL = msg_send![app, isHidden];
        if is_hidden == YES {
            Ok(())
        } else {
            Err(TilingError::window_op(format!(
                "Failed to hide app with pid {pid} after 3 attempts"
            )))
        }
    }
}

/// Shows (unhides) an application by PID.
///
/// # Errors
///
/// Returns `TilingError::WindowOperation` if the app cannot be found or unhidden.
pub fn unhide_app(pid: i32) -> TilingResult<()> {
    unsafe {
        let app_class = Class::get("NSRunningApplication")
            .ok_or_else(|| TilingError::window_op("NSRunningApplication class not found"))?;

        let app: *mut Object = msg_send![app_class, runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            return Err(TilingError::window_op(format!(
                "No running application for pid {pid}"
            )));
        }

        let result: BOOL = msg_send![app, unhide];
        if result == YES {
            Ok(())
        } else {
            Err(TilingError::window_op(format!(
                "Failed to unhide app with pid {pid}"
            )))
        }
    }
}

/// Hides a specific window by ID.
///
/// Note: macOS doesn't support hiding individual windows directly.
/// This hides the entire application.
///
/// # Errors
///
/// Returns `TilingError::WindowNotFound` if the window ID is not found.
/// Returns `TilingError::WindowOperation` if the app cannot be hidden.
pub fn hide_window(window_id: u32) -> TilingResult<()> {
    let windows = get_all_windows();
    let window = windows
        .iter()
        .find(|w| w.id == window_id)
        .ok_or(TilingError::WindowNotFound(window_id))?;

    hide_app(window.pid)
}

/// Shows a specific window by ID.
///
/// Note: This unhides the entire application and focuses the window.
///
/// # Errors
///
/// Returns `TilingError::WindowNotFound` if the window ID is not found.
/// Returns `TilingError::WindowOperation` if the app cannot be unhidden or window cannot be focused.
pub fn show_window(window_id: u32) -> TilingResult<()> {
    let windows = get_all_windows();
    let window = windows
        .iter()
        .find(|w| w.id == window_id)
        .ok_or(TilingError::WindowNotFound(window_id))?;

    unhide_app(window.pid)?;
    focus_window(window_id)
}

/// Moves a window off-screen to hide it without minimizing.
///
/// This is used for cross-workspace apps where we can't use app-level hiding
/// (because that would hide windows in the target workspace too) and we don't
/// want to minimize windows.
///
/// # Arguments
///
/// * `window_id` - The `CGWindowID` of the window to move off-screen
///
/// # Errors
///
/// Returns `TilingError::WindowNotFound` if the window ID is not found.
/// Returns `TilingError::WindowOperation` if the window cannot be moved.
pub fn move_window_offscreen(window_id: u32) -> TilingResult<()> {
    let windows = get_all_windows();
    let window = windows
        .iter()
        .find(|w| w.id == window_id)
        .ok_or(TilingError::WindowNotFound(window_id))?;

    // Move to off-screen position, keeping the same size
    let offscreen_frame = super::state::Rect {
        x: offscreen::X,
        y: offscreen::Y,
        width: window.frame.width,
        height: window.frame.height,
    };

    set_window_frame(window_id, &offscreen_frame)
}

/// Waits for windows from a specific app to become ready (have valid AX frames).
///
/// Instead of using a fixed delay, this polls the app's windows until they have
/// valid accessibility properties (position and size), or until the timeout is reached.
///
/// # Arguments
///
/// * `pid` - Process ID of the app
/// * `max_wait_ms` - Maximum time to wait in milliseconds
/// * `poll_interval_ms` - How often to check (in milliseconds)
///
/// # Returns
///
/// A vector of `WindowInfo` for all ready windows from the app.
pub fn wait_for_app_windows_ready(
    pid: i32,
    max_wait_ms: u64,
    poll_interval_ms: u64,
) -> Vec<WindowInfo> {
    use std::time::{Duration, Instant};

    let start = Instant::now();
    let max_wait = Duration::from_millis(max_wait_ms);
    let poll_interval = Duration::from_millis(poll_interval_ms);

    loop {
        // Get fresh window list for this app, including hidden windows.
        // This ensures we catch newly created windows that may not have CG IDs yet
        // (e.g., Ghostty native tabs which take time to get CG window entries).
        let windows = get_all_windows_including_hidden();
        let app_windows: Vec<WindowInfo> = windows.into_iter().filter(|w| w.pid == pid).collect();

        // Check if we found any windows
        if !app_windows.is_empty() {
            // All windows in get_all_windows_including_hidden() have valid frames
            // (windows without valid frames are skipped in get_all_windows_internal)
            return app_windows;
        }

        // Check timeout
        if start.elapsed() >= max_wait {
            eprintln!(
                "stache: tiling: wait_for_app_windows_ready: timeout after {max_wait_ms}ms for pid {pid}"
            );
            return Vec::new();
        }

        // Wait before next poll
        std::thread::sleep(poll_interval);
    }
}

/// Checks if a specific window is ready (has valid AX frame).
///
/// # Arguments
///
/// * `pid` - Process ID of the app owning the window
/// * `window_id` - The window ID to check
///
/// # Returns
///
/// `true` if the window exists and has a valid frame.
pub fn is_window_ready(pid: i32, window_id: u32) -> bool {
    let windows = get_all_windows();
    windows.iter().any(|w| w.pid == pid && w.id == window_id)
}

/// Moves a window to a specific screen.
///
/// The window is repositioned to maintain its relative position within the
/// screen bounds. If the window would be outside the target screen, it's
/// positioned at the top-left of the visible area.
///
/// # Arguments
///
/// * `window_id` - The window to move
/// * `target_screen` - The screen to move the window to
/// * `current_screen` - The screen the window is currently on (optional, for relative positioning)
///
/// # Returns
///
/// `true` if the window was successfully moved.
pub fn move_window_to_screen(
    window_id: u32,
    target_screen: &super::state::Screen,
    current_screen: Option<&super::state::Screen>,
) -> bool {
    let windows = get_all_windows();
    let Some(window) = windows.iter().find(|w| w.id == window_id) else {
        return false;
    };

    let Some(ax_element) = window.ax_element else {
        return false;
    };

    // Calculate new position
    let new_frame = calculate_frame_for_screen(&window.frame, target_screen, current_screen);

    unsafe { set_ax_frame(ax_element, &new_frame) }
}

/// Calculates a new frame for a window when moving it to a different screen.
///
/// Tries to preserve the relative position within the screen. If the window
/// is larger than the target screen, it's resized to fit.
#[allow(clippy::option_if_let_else)] // More readable with if let for this algorithm
fn calculate_frame_for_screen(
    current_frame: &Rect,
    target_screen: &super::state::Screen,
    current_screen: Option<&super::state::Screen>,
) -> Rect {
    let target = &target_screen.visible_frame;

    // If we know the current screen, try to preserve relative position
    if let Some(source) = current_screen {
        let source_frame = &source.visible_frame;

        // Calculate relative position (0.0 to 1.0)
        let rel_x = if source_frame.width > 0.0 {
            (current_frame.x - source_frame.x) / source_frame.width
        } else {
            0.0
        };
        let rel_y = if source_frame.height > 0.0 {
            (current_frame.y - source_frame.y) / source_frame.height
        } else {
            0.0
        };

        // Apply relative position to target screen
        let new_x = target.x + (rel_x * target.width);
        let new_y = target.y + (rel_y * target.height);

        // Ensure window fits within target screen
        let new_width = current_frame.width.min(target.width);
        let new_height = current_frame.height.min(target.height);

        // Clamp position to keep window visible
        let clamped_x = new_x.max(target.x).min(target.x + target.width - new_width);
        let clamped_y = new_y.max(target.y).min(target.y + target.height - new_height);

        Rect::new(clamped_x, clamped_y, new_width, new_height)
    } else {
        // No source screen info - position at top-left of target screen
        let new_width = current_frame.width.min(target.width);
        let new_height = current_frame.height.min(target.height);

        Rect::new(target.x, target.y, new_width, new_height)
    }
}

/// Determines which screen a point is on.
///
/// Returns the screen containing the point, or None if not found.
pub fn get_screen_for_point(
    x: f64,
    y: f64,
    screens: &[super::state::Screen],
) -> Option<&super::state::Screen> {
    screens.iter().find(|s| s.frame.contains(super::state::Point::new(x, y)))
}

/// Determines which screen a window is primarily on.
///
/// Uses the window's center point to determine the screen.
pub fn get_screen_for_window<'a>(
    window_frame: &Rect,
    screens: &'a [super::state::Screen],
) -> Option<&'a super::state::Screen> {
    let center = window_frame.center();
    get_screen_for_point(center.x, center.y, screens)
}

// ============================================================================
// CoreFoundation Dictionary Helpers
// ============================================================================

/// Gets a number value from a `CFDictionary`.
unsafe fn get_cf_dict_number(dict: *const c_void, key: &str) -> Option<i64> {
    let cf_key = CFString::new(key);
    let mut value: *const c_void = ptr::null();

    let found = unsafe {
        core_foundation::dictionary::CFDictionaryGetValueIfPresent(
            dict.cast(),
            cf_key.as_concrete_TypeRef().cast(),
            &raw mut value,
        )
    };

    if found == 0 || value.is_null() {
        return None;
    }

    let number = unsafe { CFNumber::wrap_under_get_rule(value.cast()) };
    number.to_i64()
}

/// Gets a string value from a `CFDictionary`.
unsafe fn get_cf_dict_string(dict: *const c_void, key: &str) -> Option<String> {
    let cf_key = CFString::new(key);
    let mut value: *const c_void = ptr::null();

    let found = unsafe {
        core_foundation::dictionary::CFDictionaryGetValueIfPresent(
            dict.cast(),
            cf_key.as_concrete_TypeRef().cast(),
            &raw mut value,
        )
    };

    if found == 0 || value.is_null() {
        return None;
    }

    // Check type
    let cf_string_type_id = CFString::type_id() as u64;
    if unsafe { CFGetTypeID(value) } != cf_string_type_id {
        return None;
    }

    let cf_string = unsafe { CFString::wrap_under_get_rule(value.cast()) };
    Some(cf_string.to_string())
}

/// Gets a `CGRect` value from a `CFDictionary` (stored as a bounds dictionary).
#[allow(clippy::cast_precision_loss)] // Window coordinates won't exceed f64 precision
unsafe fn get_cf_dict_rect(dict: *const c_void, key: &str) -> Option<Rect> {
    let cf_key = CFString::new(key);
    let mut value: *const c_void = ptr::null();

    let found = unsafe {
        core_foundation::dictionary::CFDictionaryGetValueIfPresent(
            dict.cast(),
            cf_key.as_concrete_TypeRef().cast(),
            &raw mut value,
        )
    };

    if found == 0 || value.is_null() {
        return None;
    }

    // The bounds is a CFDictionary with X, Y, Width, Height keys
    let x = unsafe { get_cf_dict_number(value, "X") }? as f64;
    let y = unsafe { get_cf_dict_number(value, "Y") }? as f64;
    let width = unsafe { get_cf_dict_number(value, "Width") }? as f64;
    let height = unsafe { get_cf_dict_number(value, "Height") }? as f64;

    Some(Rect::new(x, y, width, height))
}

/// Converts an `NSString` to a Rust String.
#[inline]
fn ns_string_to_rust(ns_string: *mut Object) -> String {
    if ns_string.is_null() {
        return String::new();
    }

    unsafe {
        let utf8: *const i8 = msg_send![ns_string, UTF8String];
        if utf8.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned()
    }
}

// ============================================================================
// Conversion to TrackedWindow
// ============================================================================

impl WindowInfo {
    /// Creates a new `WindowInfo` instance.
    ///
    /// This is primarily useful for testing. In normal operation, window info
    /// is obtained from `get_all_windows()`.
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        clippy::missing_const_for_fn,
        clippy::fn_params_excessive_bools
    )]
    pub fn new(
        id: u32,
        pid: i32,
        bundle_id: String,
        app_name: String,
        title: String,
        frame: Rect,
        is_minimized: bool,
        is_hidden: bool,
        is_main: bool,
        is_focused: bool,
    ) -> Self {
        Self {
            id,
            pid,
            bundle_id,
            app_name,
            title,
            frame,
            is_minimized,
            is_hidden,
            is_main,
            is_focused,
            ax_element: None,
        }
    }

    /// Returns whether this window has a valid AX element.
    ///
    /// Windows without AX elements are "phantom" windows that appear in
    /// `CGWindowList` but cannot be controlled via the Accessibility API.
    #[must_use]
    pub const fn has_ax_element(&self) -> bool { self.ax_element.is_some() }

    /// Converts this `WindowInfo` to a `TrackedWindow` for the tiling state.
    #[must_use]
    pub fn to_tracked_window(&self, workspace_name: &str) -> TrackedWindow {
        TrackedWindow::new(
            self.id,
            self.pid,
            self.bundle_id.clone(),
            self.app_name.clone(),
            self.title.clone(),
            self.frame,
            workspace_name.to_string(),
        )
    }

    /// Creates a minimal `WindowInfo` for testing purposes.
    ///
    /// All optional fields are set to reasonable defaults.
    #[cfg(test)]
    #[must_use]
    pub fn new_for_test(id: u32, pid: i32, frame: Rect) -> Self {
        Self {
            id,
            pid,
            bundle_id: String::new(),
            app_name: String::new(),
            title: String::new(),
            frame,
            is_minimized: false,
            is_hidden: false,
            is_main: false,
            is_focused: false,
            ax_element: None,
        }
    }

    /// Creates a `WindowInfo` for testing with app identification.
    ///
    /// Use this when you need to test rule matching based on bundle ID or app name.
    #[cfg(test)]
    #[must_use]
    pub fn new_for_test_with_app(
        id: u32,
        pid: i32,
        frame: Rect,
        bundle_id: &str,
        app_name: &str,
    ) -> Self {
        Self {
            id,
            pid,
            bundle_id: bundle_id.to_string(),
            app_name: app_name.to_string(),
            title: String::new(),
            frame,
            is_minimized: false,
            is_hidden: false,
            is_main: false,
            is_focused: false,
            ax_element: None,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cf_windows_returns_valid_pointer() {
        let ptr = cf_windows();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_title_returns_valid_pointer() {
        let ptr = cf_title();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_position_returns_valid_pointer() {
        let ptr = cf_position();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_size_returns_valid_pointer() {
        let ptr = cf_size();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_minimized_returns_valid_pointer() {
        let ptr = cf_minimized();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_cf_raise_returns_valid_pointer() {
        let ptr = cf_raise();
        assert!(!ptr.is_null());
    }

    #[test]
    fn test_get_ax_string_null_element() {
        unsafe {
            let result = get_ax_string(ptr::null_mut(), cf_title());
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_get_ax_bool_null_element() {
        unsafe {
            let result = get_ax_bool(ptr::null_mut(), cf_minimized());
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_get_ax_position_null_element() {
        unsafe {
            let result = get_ax_position(ptr::null_mut());
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_get_ax_size_null_element() {
        unsafe {
            let result = get_ax_size(ptr::null_mut());
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_get_ax_frame_null_element() {
        unsafe {
            let result = get_ax_frame(ptr::null_mut());
            assert!(result.is_none());
        }
    }

    #[test]
    fn test_set_ax_position_null_element() {
        unsafe {
            let result = set_ax_position(ptr::null_mut(), 0.0, 0.0);
            assert!(!result);
        }
    }

    #[test]
    fn test_set_ax_size_null_element() {
        unsafe {
            let result = set_ax_size(ptr::null_mut(), 100.0, 100.0);
            assert!(!result);
        }
    }

    #[test]
    fn test_raise_ax_window_null_element() {
        unsafe {
            let result = raise_ax_window(ptr::null_mut());
            assert!(!result);
        }
    }

    #[test]
    fn test_get_running_apps() {
        // This should return at least one app (the test runner)
        // May be empty in CI without display - just verify it doesn't crash
        let _ = get_running_apps();
    }

    #[test]
    fn test_get_cg_window_list() {
        // Just verify it doesn't crash
        let _ = get_cg_window_list();
    }

    #[test]
    fn test_ns_string_to_rust_null() {
        let result = ns_string_to_rust(ptr::null_mut());
        assert!(result.is_empty());
    }

    #[test]
    fn test_window_info_to_tracked_window() {
        let info = WindowInfo {
            id: 123,
            pid: 456,
            bundle_id: "com.example.app".to_string(),
            app_name: "Example".to_string(),
            title: "Window Title".to_string(),
            frame: Rect::new(100.0, 100.0, 800.0, 600.0),
            is_minimized: false,
            is_hidden: false,
            is_main: true,
            is_focused: true,
            ax_element: None,
        };

        let tracked = info.to_tracked_window("workspace1");

        assert_eq!(tracked.id, 123);
        assert_eq!(tracked.pid, 456);
        assert_eq!(tracked.app_id, "com.example.app");
        assert_eq!(tracked.app_name, "Example");
        assert_eq!(tracked.title, "Window Title");
        assert_eq!(tracked.workspace_name, "workspace1");
    }

    #[test]
    fn test_rect_comparison() {
        let r1 = Rect::new(100.0, 200.0, 800.0, 600.0);
        let r2 = Rect::new(100.0, 200.0, 800.0, 600.0);
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_cg_window_info_creation() {
        let info = CGWindowInfo {
            id: 1,
            pid: 100,
            title: "Test".to_string(),
            app_name: "TestApp".to_string(),
            frame: Rect::default(),
            layer: 0,
        };
        assert_eq!(info.id, 1);
        assert_eq!(info.pid, 100);
    }

    // ========================================================================
    // Tab Deduplication Tests
    // ========================================================================

    #[test]
    fn test_deduplicate_tabbed_windows_no_tabs() {
        // Two windows with different frames - no deduplication needed
        let win1 = CGWindowInfo {
            id: 100,
            pid: 1,
            title: "Window 1".to_string(),
            app_name: "App".to_string(),
            frame: Rect::new(0.0, 0.0, 800.0, 600.0),
            layer: 0,
        };
        let win2 = CGWindowInfo {
            id: 200,
            pid: 1,
            title: "Window 2".to_string(),
            app_name: "App".to_string(),
            frame: Rect::new(100.0, 100.0, 800.0, 600.0), // Different position
            layer: 0,
        };

        let windows: Vec<&CGWindowInfo> = vec![&win1, &win2];
        let result = deduplicate_tabbed_windows(&windows);

        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_deduplicate_tabbed_windows_with_tabs() {
        // Three windows with same frame - should deduplicate to one (like Ghostty with 3 tabs)
        let frame = Rect::new(12.0, 52.0, 2536.0, 1375.0);
        let win1 = CGWindowInfo {
            id: 27,
            pid: 1,
            title: "Tab 1".to_string(),
            app_name: "Ghostty".to_string(),
            frame,
            layer: 0,
        };
        let win2 = CGWindowInfo {
            id: 28,
            pid: 1,
            title: "Tab 2".to_string(),
            app_name: "Ghostty".to_string(),
            frame,
            layer: 0,
        };
        let win3 = CGWindowInfo {
            id: 29,
            pid: 1,
            title: "Tab 3".to_string(),
            app_name: "Ghostty".to_string(),
            frame,
            layer: 0,
        };

        let windows: Vec<&CGWindowInfo> = vec![&win1, &win2, &win3];
        let result = deduplicate_tabbed_windows(&windows);

        // Should keep only one window (the one with highest ID)
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, 29);
    }

    #[test]
    fn test_deduplicate_tabbed_windows_mixed() {
        // Simulates Finder with 2 windows (one with tabs, one without)
        // Window 1 at position (21, 121) - no tabs
        let win1 = CGWindowInfo {
            id: 33568,
            pid: 1,
            title: "Documents".to_string(),
            app_name: "Finder".to_string(),
            frame: Rect::new(21.0, 121.0, 1416.0, 1261.0),
            layer: 0,
        };
        // Window 2 at position (21, 179) - has 2 tabs (2 CGWindows)
        let win2 = CGWindowInfo {
            id: 33963,
            pid: 1,
            title: "Projects".to_string(),
            app_name: "Finder".to_string(),
            frame: Rect::new(21.0, 179.0, 1416.0, 1261.0),
            layer: 0,
        };
        let win3 = CGWindowInfo {
            id: 37874,
            pid: 1,
            title: "Projects Tab 2".to_string(),
            app_name: "Finder".to_string(),
            frame: Rect::new(21.0, 179.0, 1416.0, 1261.0), // Same frame as win2
            layer: 0,
        };

        let windows: Vec<&CGWindowInfo> = vec![&win1, &win2, &win3];
        let result = deduplicate_tabbed_windows(&windows);

        // Should have 2 windows: one at y=121, one at y=179 (deduped)
        assert_eq!(result.len(), 2);

        // Check we have both unique positions
        let y_positions: Vec<i32> = result.iter().map(|w| w.frame.y.round() as i32).collect();
        assert!(y_positions.contains(&121));
        assert!(y_positions.contains(&179));
    }

    #[test]
    fn test_deduplicate_tabbed_windows_empty() {
        let windows: Vec<&CGWindowInfo> = vec![];
        let result = deduplicate_tabbed_windows(&windows);
        assert!(result.is_empty());
    }

    #[test]
    fn test_deduplicate_tabbed_windows_single() {
        let win = CGWindowInfo {
            id: 1,
            pid: 1,
            title: "Single".to_string(),
            app_name: "App".to_string(),
            frame: Rect::new(0.0, 0.0, 800.0, 600.0),
            layer: 0,
        };

        let windows: Vec<&CGWindowInfo> = vec![&win];
        let result = deduplicate_tabbed_windows(&windows);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, 1);
    }
}
