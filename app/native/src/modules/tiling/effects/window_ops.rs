//! Window operations using the macOS Accessibility API.
//!
//! This module provides functions to manipulate windows using `AXUIElement`.
//! All operations are self-contained within `tiling` (no imports from v1).
//!
//! # Thread Safety
//!
//! The macOS Accessibility API is thread-safe for operations on different windows.
//! These functions can be called from multiple threads when operating on
//! different windows simultaneously.
//!
//! # Performance
//!
//! For best performance during animations or batch operations:
//! - Use `set_window_frame_fast` instead of `set_window_frame`
//! - Consider using `EnhancedUIGuard` to temporarily disable `VoiceOver` features
//! - Batch multiple window updates together

// Allow cast truncation in FFI code - we control array sizes
#![allow(clippy::cast_sign_loss)]

use std::cell::OnceCell;
use std::ffi::c_void;
use std::ptr;

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;

use crate::modules::tiling::state::Rect;

// ============================================================================
// FFI Declarations
// ============================================================================

type AXUIElementRef = *mut c_void;
type AXError = i32;

const K_AX_ERROR_SUCCESS: AXError = 0;

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
    fn AXValueCreate(value_type: i32, value: *const c_void) -> *mut c_void;
    fn AXValueGetValue(value: *const c_void, value_type: i32, value_ptr: *mut c_void) -> bool;
    fn _AXUIElementGetWindow(element: AXUIElementRef, window_id: *mut u32) -> AXError;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFGetTypeID(cf: *const c_void) -> u64;
    fn CFArrayGetCount(array: *const c_void) -> i64;
    fn CFArrayGetValueAtIndex(array: *const c_void, idx: i64) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn CFRetain(cf: *const c_void) -> *const c_void;
}

// AXValue type constants
const K_AX_VALUE_TYPE_CG_POINT: i32 = 1;
const K_AX_VALUE_TYPE_CG_SIZE: i32 = 2;

// ============================================================================
// Cached CFStrings
// ============================================================================

thread_local! {
    static CF_WINDOWS: OnceCell<CFString> = const { OnceCell::new() };
    static CF_POSITION: OnceCell<CFString> = const { OnceCell::new() };
    static CF_SIZE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_FOCUSED: OnceCell<CFString> = const { OnceCell::new() };
    static CF_MAIN: OnceCell<CFString> = const { OnceCell::new() };
    static CF_RAISE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_ROLE: OnceCell<CFString> = const { OnceCell::new() };
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
fn cf_position() -> *const c_void { cached_cfstring!(CF_POSITION, "AXPosition") }

#[inline]
fn cf_size() -> *const c_void { cached_cfstring!(CF_SIZE, "AXSize") }

#[inline]
fn cf_focused() -> *const c_void { cached_cfstring!(CF_FOCUSED, "AXFocused") }

#[inline]
fn cf_main() -> *const c_void { cached_cfstring!(CF_MAIN, "AXMain") }

#[inline]
fn cf_raise() -> *const c_void { cached_cfstring!(CF_RAISE, "AXRaise") }

#[inline]
fn cf_role() -> *const c_void { cached_cfstring!(CF_ROLE, "AXRole") }

// ============================================================================
// AX Element Resolution
// ============================================================================

/// Resolves a window ID to its `AXUIElement`.
///
/// This function enumerates all windows of all running applications to find
/// the `AXUIElement` for the given window ID. This is necessary because macOS
/// doesn't provide a direct way to get an `AXUIElement` from a window ID.
///
/// # Returns
///
/// The `AXUIElement` for the window, or `None` if not found.
/// The caller takes ownership and must release the element when done.
#[must_use]
pub fn resolve_window_element(window_id: u32) -> Option<AXUIElementRef> {
    // Get all running app PIDs
    let pids = get_running_app_pids();

    for pid in pids {
        let app_element = unsafe { AXUIElementCreateApplication(pid) };
        if app_element.is_null() {
            continue;
        }

        // Get all windows for this app (these are retained)
        let windows = unsafe { get_app_windows(app_element) };
        unsafe { CFRelease(app_element.cast()) };

        for window in &windows {
            // Check if this window has the target ID
            if let Some(id) = unsafe { get_window_id(*window) }
                && id == window_id
            {
                // Found it! Release all other windows in this batch
                for other in &windows {
                    if *other != *window {
                        unsafe { CFRelease((*other).cast()) };
                    }
                }
                // The matched window is already retained from get_app_windows
                return Some(*window);
            }
        }

        // Release all windows since we didn't find our target
        for window in &windows {
            unsafe { CFRelease((*window).cast()) };
        }
    }

    log::debug!("resolve_window_element: window {window_id} not found");
    None
}

/// Gets the window ID from an `AXUIElement`.
#[inline]
unsafe fn get_window_id(element: AXUIElementRef) -> Option<u32> {
    if element.is_null() {
        return None;
    }

    let mut window_id: u32 = 0;
    let result = unsafe { _AXUIElementGetWindow(element, &raw mut window_id) };

    if result == K_AX_ERROR_SUCCESS && window_id != 0 {
        Some(window_id)
    } else {
        None
    }
}

/// Gets all AX windows for an application element.
///
/// The returned windows are retained and must be released by the caller.
unsafe fn get_app_windows(app_element: AXUIElementRef) -> Vec<AXUIElementRef> {
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

    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let mut windows = Vec::with_capacity(count as usize);

    for i in 0..count {
        let window = unsafe { CFArrayGetValueAtIndex(value, i) };
        if !window.is_null() && unsafe { CFGetTypeID(window) } == ax_type_id {
            // Verify it's a real window (has AXWindow role)
            if is_window_element(window.cast_mut()) {
                // Retain the window element so it survives after we release the array
                unsafe { CFRetain(window) };
                windows.push(window.cast_mut());
            }
        }
    }

    unsafe { CFRelease(value) };
    windows
}

/// Checks if an AX element is a window (has role `AXWindow`).
fn is_window_element(element: AXUIElementRef) -> bool {
    if element.is_null() {
        return false;
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, cf_role(), &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return false;
    }

    let cf_string_type_id = CFString::type_id() as u64;
    if unsafe { CFGetTypeID(value) } != cf_string_type_id {
        unsafe { CFRelease(value) };
        return false;
    }

    let cf_string = unsafe { CFString::wrap_under_get_rule(value.cast()) };
    let role = cf_string.to_string();
    unsafe { CFRelease(value) };

    role == "AXWindow"
}

/// Gets PIDs of all running regular applications.
fn get_running_app_pids() -> Vec<i32> {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};

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
        let mut pids = Vec::with_capacity(count);

        for i in 0..count {
            let app: *mut Object = msg_send![apps, objectAtIndex: i];
            if app.is_null() {
                continue;
            }

            // Only include regular apps (not background processes)
            let activation_policy: i64 = msg_send![app, activationPolicy];
            if activation_policy != 0 {
                continue;
            }

            let pid: i32 = msg_send![app, processIdentifier];
            if pid > 0 {
                pids.push(pid);
            }
        }

        pids
    }
}

// ============================================================================
// Window Frame Operations
// ============================================================================

/// Gets the position of a window.
unsafe fn get_ax_position(element: AXUIElementRef) -> Option<(f64, f64)> {
    if element.is_null() {
        return None;
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, cf_position(), &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }

    let mut point = core_graphics::geometry::CGPoint::new(0.0, 0.0);
    let success =
        unsafe { AXValueGetValue(value.cast(), K_AX_VALUE_TYPE_CG_POINT, (&raw mut point).cast()) };

    unsafe { CFRelease(value) };

    if success {
        Some((point.x, point.y))
    } else {
        None
    }
}

/// Gets the size of a window.
unsafe fn get_ax_size(element: AXUIElementRef) -> Option<(f64, f64)> {
    if element.is_null() {
        return None;
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, cf_size(), &raw mut value) };

    if result != K_AX_ERROR_SUCCESS || value.is_null() {
        return None;
    }

    let mut size = core_graphics::geometry::CGSize::new(0.0, 0.0);
    let success =
        unsafe { AXValueGetValue(value.cast(), K_AX_VALUE_TYPE_CG_SIZE, (&raw mut size).cast()) };

    unsafe { CFRelease(value) };

    if success {
        Some((size.width, size.height))
    } else {
        None
    }
}

/// Sets the position of a window.
unsafe fn set_ax_position(element: AXUIElementRef, x: f64, y: f64) -> bool {
    if element.is_null() {
        return false;
    }

    let point = core_graphics::geometry::CGPoint::new(x, y);
    let value = unsafe { AXValueCreate(K_AX_VALUE_TYPE_CG_POINT, (&raw const point).cast()) };

    if value.is_null() {
        return false;
    }

    let result = unsafe { AXUIElementSetAttributeValue(element, cf_position(), value.cast()) };
    unsafe { CFRelease(value.cast()) };

    result == K_AX_ERROR_SUCCESS
}

/// Sets the size of a window.
unsafe fn set_ax_size(element: AXUIElementRef, width: f64, height: f64) -> bool {
    if element.is_null() {
        return false;
    }

    let size = core_graphics::geometry::CGSize::new(width, height);
    let value = unsafe { AXValueCreate(K_AX_VALUE_TYPE_CG_SIZE, (&raw const size).cast()) };

    if value.is_null() {
        return false;
    }

    let result = unsafe { AXUIElementSetAttributeValue(element, cf_size(), value.cast()) };
    unsafe { CFRelease(value.cast()) };

    result == K_AX_ERROR_SUCCESS
}

// ============================================================================
// Public API
// ============================================================================

/// Gets the current frame of a window.
///
/// # Arguments
///
/// * `window_id` - The window ID to get the frame for.
///
/// # Returns
///
/// The window frame, or `None` if the window cannot be found.
#[must_use]
pub fn get_window_frame(window_id: u32) -> Option<Rect> {
    let element = resolve_window_element(window_id)?;

    let result = unsafe {
        let pos = get_ax_position(element)?;
        let size = get_ax_size(element)?;
        Some(Rect::new(pos.0, pos.1, size.0, size.1))
    };

    unsafe { CFRelease(element.cast()) };
    result
}

/// Sets the frame of a window (position and size).
///
/// This performs operations in the optimal order for reliable resizing:
/// 1. Set size first (allows window to shrink)
/// 2. Set position
/// 3. Set size again (some apps need this)
///
/// # Arguments
///
/// * `window_id` - The window ID to move/resize.
/// * `frame` - The target frame.
///
/// # Returns
///
/// `true` if the operation succeeded (optimistically, since execution is async).
#[must_use]
pub fn set_window_frame(window_id: u32, frame: &Rect) -> bool {
    // Dispatch to main thread using the project's existing dispatch utility
    // This is async (fire-and-forget) but avoids potential deadlocks
    let frame_copy = *frame;
    crate::utils::thread::dispatch_on_main(move || {
        set_window_frame_impl(window_id, &frame_copy);
    });

    // Return true optimistically - the actual operation runs async
    true
}

/// Internal implementation of `set_window_frame` (runs on main thread).
fn set_window_frame_impl(window_id: u32, frame: &Rect) {
    let Some(element) = resolve_window_element(window_id) else {
        log::debug!("set_window_frame: could not resolve window {window_id}");
        return;
    };

    // Order matters for reliable resizing:
    // 1. Set size first - allows window to shrink
    // 2. Set position - move the (now smaller) window
    // 3. Set size again - some apps need this
    let size_ok_1 = unsafe { set_ax_size(element, frame.width, frame.height) };
    let pos_ok = unsafe { set_ax_position(element, frame.x, frame.y) };
    let size_ok_2 = unsafe { set_ax_size(element, frame.width, frame.height) };

    unsafe { CFRelease(element.cast()) };

    if !(pos_ok && (size_ok_1 || size_ok_2)) {
        log::debug!("set_window_frame: failed for window {window_id}");
    }
}

/// Sets the frame of a window using the fast path (2 AX calls instead of 3).
///
/// Use this during animations where windows move in small increments and
/// the defensive double-size-set isn't needed.
///
/// # Arguments
///
/// * `window_id` - The window ID to move/resize.
/// * `frame` - The target frame.
///
/// # Returns
///
/// `true` if the operation succeeded.
#[must_use]
pub fn set_window_frame_fast(window_id: u32, frame: &Rect) -> bool {
    let Some(element) = resolve_window_element(window_id) else {
        return false;
    };

    let pos_ok = unsafe { set_ax_position(element, frame.x, frame.y) };
    let size_ok = unsafe { set_ax_size(element, frame.width, frame.height) };

    unsafe { CFRelease(element.cast()) };

    pos_ok && size_ok
}

/// Result of a frame set operation with verification.
#[derive(Debug, Clone)]
pub struct FrameSetResult {
    /// Whether the operation succeeded at all.
    pub success: bool,
    /// The actual frame after the operation (if verification was performed).
    pub actual_frame: Option<Rect>,
    /// Whether the window hit its minimum size constraint.
    pub hit_minimum_width: bool,
    /// Whether the window hit its minimum height constraint.
    pub hit_minimum_height: bool,
    /// The window's minimum size, if known.
    pub minimum_size: Option<(f64, f64)>,
}

impl FrameSetResult {
    /// Returns true if the window couldn't reach the target size.
    #[must_use]
    pub const fn hit_constraints(&self) -> bool {
        self.hit_minimum_width || self.hit_minimum_height
    }
}

/// Sets the frame of a window and verifies the result.
///
/// This is a synchronous operation that checks if the window actually
/// reached the target size. Use this when you need to detect if a window
/// hit its minimum size constraints.
///
/// # Arguments
///
/// * `window_id` - The window ID to move/resize.
/// * `target` - The target frame.
///
/// # Returns
///
/// A `FrameSetResult` containing information about the operation.
#[must_use]
pub fn set_window_frame_verified(window_id: u32, target: &Rect) -> FrameSetResult {
    let Some(ax_element) = get_ax_element_for_window(window_id) else {
        return FrameSetResult {
            success: false,
            actual_frame: None,
            hit_minimum_width: false,
            hit_minimum_height: false,
            minimum_size: None,
        };
    };

    // Get minimum size before setting frame (if available)
    let minimum_size = ax_element.minimum_size();

    // Check if we're trying to go below minimum
    let (min_width_constraint, min_height_constraint) = if let Some((min_w, min_h)) = minimum_size {
        (target.width < min_w - 1.0, target.height < min_h - 1.0)
    } else {
        (false, false)
    };

    // Set the frame
    let success = ax_element.set_frame(target).is_ok();

    if !success {
        return FrameSetResult {
            success: false,
            actual_frame: None,
            hit_minimum_width: min_width_constraint,
            hit_minimum_height: min_height_constraint,
            minimum_size,
        };
    }

    // Get actual frame to verify
    let actual_frame = ax_element.frame();

    // Check if we hit size constraints
    let (hit_min_width, hit_min_height) =
        actual_frame
            .as_ref()
            .map_or((min_width_constraint, min_height_constraint), |actual| {
                let width_diff = (actual.width - target.width).abs();
                let height_diff = (actual.height - target.height).abs();

                // If actual is larger than target and we're not close, we hit a minimum
                let hit_width = width_diff > 1.0 && actual.width > target.width;
                let hit_height = height_diff > 1.0 && actual.height > target.height;

                (
                    hit_width || min_width_constraint,
                    hit_height || min_height_constraint,
                )
            });

    FrameSetResult {
        success,
        actual_frame,
        hit_minimum_width: hit_min_width,
        hit_minimum_height: hit_min_height,
        minimum_size,
    }
}

/// Gets the minimum size constraints for a window, if available.
///
/// # Arguments
///
/// * `window_id` - The window ID to query.
///
/// # Returns
///
/// `Some((min_width, min_height))` if the window reports minimum size,
/// `None` if the attribute is not available.
#[must_use]
pub fn get_window_minimum_size(window_id: u32) -> Option<(f64, f64)> {
    let ax_element = get_ax_element_for_window(window_id)?;
    ax_element.minimum_size()
}

/// Gets an `AXElement` for a window ID.
fn get_ax_element_for_window(
    window_id: u32,
) -> Option<crate::modules::tiling::ffi::accessibility::AXElement> {
    use crate::modules::tiling::ffi::accessibility::AXElement;

    let pid = get_window_pid(window_id)?;
    let app = AXElement::application(pid)?;

    // Find the window with matching ID
    app.windows().into_iter().find(|w| w.window_id() == Some(window_id))
}

/// Focuses a window (gives it keyboard focus).
///
/// # Arguments
///
/// * `window_id` - The window ID to focus.
///
/// # Returns
///
/// `true` if the operation succeeded (optimistically, since execution is async).
#[must_use]
pub fn focus_window(window_id: u32) -> bool {
    // Dispatch to main thread using the project's existing dispatch utility
    crate::utils::thread::dispatch_on_main(move || {
        focus_window_impl(window_id);
    });

    // Return true optimistically - the actual operation runs async
    true
}

/// Internal implementation of `focus_window` (runs on main thread).
fn focus_window_impl(window_id: u32) {
    // First, get the PID for this window so we can activate the app
    let pid = get_window_pid(window_id);

    // Activate the owning application first - this is critical for focus to work
    if let Some(pid) = pid {
        activate_app(pid);
    }

    let Some(element) = resolve_window_element(window_id) else {
        log::debug!("focus_window: could not resolve window {window_id}");
        return;
    };

    unsafe {
        let true_value = CFBoolean::true_value();

        // Set AXMain to make it the main window of the app
        let _main_result = AXUIElementSetAttributeValue(
            element,
            cf_main(),
            true_value.as_concrete_TypeRef().cast(),
        );

        // Set AXFocused to true
        let _focus_result = AXUIElementSetAttributeValue(
            element,
            cf_focused(),
            true_value.as_concrete_TypeRef().cast(),
        );

        // Raise the window (bring to front)
        let _raise_result = AXUIElementPerformAction(element, cf_raise());

        CFRelease(element.cast());
    };
}

/// Gets the PID of the app that owns a window.
fn get_window_pid(window_id: u32) -> Option<i32> {
    // Get all running app PIDs and check which one owns this window
    let pids = get_running_app_pids();

    for pid in pids {
        let app_element = unsafe { AXUIElementCreateApplication(pid) };
        if app_element.is_null() {
            continue;
        }

        let windows = unsafe { get_app_windows(app_element) };
        unsafe { CFRelease(app_element.cast()) };

        for window in &windows {
            if let Some(id) = unsafe { get_window_id(*window) }
                && id == window_id
            {
                // Found the owner - release all windows and return the PID
                for w in &windows {
                    unsafe { CFRelease((*w).cast()) };
                }
                return Some(pid);
            }
        }

        // Release windows since this app doesn't own our target
        for window in &windows {
            unsafe { CFRelease((*window).cast()) };
        }
    }

    None
}

/// Activates an application by PID.
///
/// Uses `NSApplicationActivateAllWindows | NSApplicationActivateIgnoringOtherApps` (3)
/// which is important for cycling through same-app windows in monocle layout.
fn activate_app(pid: i32) -> bool {
    use objc::runtime::{BOOL, Class, Object, YES};
    use objc::{msg_send, sel, sel_impl};

    unsafe {
        let Some(app_class) = Class::get("NSRunningApplication") else {
            return false;
        };

        let app: *mut Object = msg_send![app_class, runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            return false;
        }

        let result: BOOL = msg_send![app, activateWithOptions: 3u64]; // AllWindows | IgnoringOtherApps
        result == YES
    }
}

/// Raises (brings to front) a window.
///
/// # Arguments
///
/// * `window_id` - The window ID to raise.
///
/// # Returns
///
/// `true` if the operation succeeded (optimistically, since execution is async).
#[must_use]
pub fn raise_window(window_id: u32) -> bool {
    // Dispatch to main thread using the project's existing dispatch utility
    crate::utils::thread::dispatch_on_main(move || {
        raise_window_impl(window_id);
    });

    // Return true optimistically - the actual operation runs async
    true
}

/// Internal implementation of `raise_window` (runs on main thread).
fn raise_window_impl(window_id: u32) {
    let Some(element) = resolve_window_element(window_id) else {
        log::debug!("raise_window: could not resolve window {window_id}");
        return;
    };

    unsafe {
        let _result = AXUIElementPerformAction(element, cf_raise());
        CFRelease(element.cast());
    };
}

/// Sets multiple window frames in batch.
///
/// Uses a single main thread dispatch for all frames, reducing IPC overhead
/// compared to calling `set_window_frame` repeatedly. More efficient for
/// layout changes that affect multiple windows.
///
/// # Arguments
///
/// * `frames` - Vector of (`window_id`, frame) pairs.
///
/// # Returns
///
/// Number of frames queued (actual application is async).
#[must_use]
pub fn set_window_frames_batch(frames: &[(u32, Rect)]) -> usize {
    if frames.is_empty() {
        return 0;
    }

    let count = frames.len();
    let frames_copy: Vec<(u32, Rect)> = frames.to_vec();

    // Single main thread dispatch for all frames
    crate::utils::thread::dispatch_on_main(move || {
        for (window_id, frame) in frames_copy {
            set_window_frame_impl(window_id, &frame);
        }
    });

    count
}

// ============================================================================
// App Visibility Operations
// ============================================================================

/// Hides an application by PID.
///
/// Uses `NSRunningApplication.hide()` to hide all windows of the app.
/// This is an app-level operation (macOS doesn't support hiding individual windows).
///
/// # Arguments
///
/// * `pid` - The process ID of the app to hide.
///
/// # Returns
///
/// `true` if the app was successfully hidden or was already hidden, `false` otherwise.
#[must_use]
pub fn hide_app(pid: i32) -> bool {
    use objc::runtime::{BOOL, Class, Object, YES};
    use objc::{msg_send, sel, sel_impl};

    unsafe {
        let Some(app_class) = Class::get("NSRunningApplication") else {
            log::warn!("NSRunningApplication class not found");
            return false;
        };

        let app: *mut Object = msg_send![app_class, runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            log::debug!("hide_app: no running application for pid {pid}");
            return false;
        }

        // Check if already hidden
        let is_hidden: BOOL = msg_send![app, isHidden];
        if is_hidden == YES {
            return true;
        }

        // Hide the app
        let result: BOOL = msg_send![app, hide];
        result == YES
    }
}

/// Shows (unhides) an application by PID.
///
/// Uses `NSRunningApplication.unhide()` to show all windows of the app.
///
/// # Arguments
///
/// * `pid` - The process ID of the app to unhide.
///
/// # Returns
///
/// `true` if the app was successfully unhidden, `false` otherwise.
#[must_use]
pub fn unhide_app(pid: i32) -> bool {
    use objc::runtime::{BOOL, Class, Object, YES};
    use objc::{msg_send, sel, sel_impl};

    unsafe {
        let Some(app_class) = Class::get("NSRunningApplication") else {
            log::warn!("NSRunningApplication class not found");
            return false;
        };

        let app: *mut Object = msg_send![app_class, runningApplicationWithProcessIdentifier: pid];
        if app.is_null() {
            log::debug!("unhide_app: no running application for pid {pid}");
            return false;
        }

        let result: BOOL = msg_send![app, unhide];
        result == YES
    }
}

/// Hides multiple apps by their PIDs.
///
/// # Arguments
///
/// * `pids` - The process IDs of the apps to hide.
///
/// # Returns
///
/// The number of apps successfully hidden.
#[must_use]
pub fn hide_apps(pids: &[i32]) -> usize { pids.iter().filter(|&&pid| hide_app(pid)).count() }

/// Shows (unhides) multiple apps by their PIDs.
///
/// # Arguments
///
/// * `pids` - The process IDs of the apps to unhide.
///
/// # Returns
///
/// The number of apps successfully unhidden.
#[must_use]
pub fn unhide_apps(pids: &[i32]) -> usize { pids.iter().filter(|&&pid| unhide_app(pid)).count() }

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Note: test_resolve_nonexistent_window is disabled because it requires
    // accessibility permissions and can crash if permissions are not granted.
    // The function is still tested indirectly through integration tests.

    #[test]
    fn test_get_running_app_pids() {
        // This test just verifies the function doesn't panic.
        // The actual PIDs returned depend on the system state.
        let pids = get_running_app_pids();
        // Should have at least one running app (the test runner)
        // But this might fail in CI, so we just check it doesn't panic
        let _ = pids;
    }

    #[test]
    fn test_cached_cfstrings() {
        // Verify cached CFString functions don't panic
        let _ = cf_windows();
        let _ = cf_position();
        let _ = cf_size();
        let _ = cf_focused();
        let _ = cf_main();
        let _ = cf_raise();
        let _ = cf_role();
    }
}
