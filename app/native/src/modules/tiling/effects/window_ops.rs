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
use objc::runtime::{BOOL, Class, Object, YES};
use objc::{msg_send, sel, sel_impl};

use crate::modules::tiling::ffi::accessibility::AXElement;
use crate::modules::tiling::identity::{AppIdentity, WindowTarget};
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
pub fn resolve_window_element(target: WindowTarget) -> Option<AXUIElementRef> {
    // Revalidate the exact identity before touching the app.
    if resolve_current_identity(target.identity) != Some(target.identity) {
        tracing::trace!("resolve_window_element: identity mismatch for {target:?}");
        return None;
    }

    let app_element = unsafe { AXUIElementCreateApplication(target.identity.pid) };
    if app_element.is_null() {
        return None;
    }

    // Get all windows for this app (these are retained)
    let windows = unsafe { get_app_windows(app_element) };
    unsafe { CFRelease(app_element.cast()) };

    for window in &windows {
        // Check if this window has the target ID
        if let Some(id) = unsafe { get_window_id(*window) }
            && id == target.window_id
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

    tracing::debug!("resolve_window_element: window {} not found", target.window_id);
    None
}

/// Re-resolves the exact identity for a PID from one local
/// `NSRunningApplication` object.
fn resolve_current_identity(identity: AppIdentity) -> Option<AppIdentity> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    objc::rc::autoreleasepool(|| unsafe {
        let app: *mut Object = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: identity.pid
        ];
        if app.is_null() {
            return None;
        }
        // `identity.pid` is authoritative: apps that report NSNotFound for
        // `processIdentifier` still re-resolve to the same exact identity.
        AppIdentity::from_ns_running_app_with_pid(app, identity.pid)
    })
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
pub fn get_window_frame(target: WindowTarget) -> Option<Rect> {
    let element = resolve_window_element(target)?;

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
pub fn set_window_frame(target: WindowTarget, frame: &Rect) -> bool {
    // Dispatch to main thread using the project's existing dispatch utility
    // This is async (fire-and-forget) but avoids potential deadlocks
    let frame_copy = *frame;
    crate::platform::thread::dispatch_on_main(move || {
        set_window_frame_impl(target, &frame_copy);
    });

    // Return true optimistically - the actual operation runs async
    true
}

/// Internal implementation of `set_window_frame` (runs on main thread).
fn set_window_frame_impl(target: WindowTarget, frame: &Rect) {
    let Some(element) = resolve_window_element(target) else {
        tracing::debug!("set_window_frame: could not resolve window {}", target.window_id);
        return;
    };

    // Position and size are separate AX attributes. Suppress presentation
    // until both are written so callers without the animation cache (for
    // example a preset with no known source frame) still produce one reflow.
    let _update_guard = crate::modules::tiling::ffi::skylight::UpdateGuard::new();

    // Order matters for reliable resizing:
    // 1. Set size first - allows window to shrink
    // 2. Set position - move the (now smaller) window
    // 3. Set size again - some apps need this
    let size_ok_1 = unsafe { set_ax_size(element, frame.width, frame.height) };
    let pos_ok = unsafe { set_ax_position(element, frame.x, frame.y) };
    let size_ok_2 = unsafe { set_ax_size(element, frame.width, frame.height) };

    unsafe { CFRelease(element.cast()) };

    if !(pos_ok && (size_ok_1 || size_ok_2)) {
        tracing::debug!("set_window_frame: failed for window {}", target.window_id);
    }
}

/// Sets the frame of a window using the fast path.
///
/// Uses `SLSMoveWindow` for position (~0.1ms) instead of AX API (~2-5ms),
/// falling back to AX only for size changes. This is optimized for animations
/// where windows move in small increments.
///
/// # Performance
///
/// - Position via `SkyLight`: ~0.1-0.3ms
/// - Size via AX: ~2-5ms
/// - Total: ~2-5ms (vs ~4-10ms with pure AX)
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
pub fn set_window_frame_fast(target: WindowTarget, frame: &Rect) -> bool {
    // Try SkyLight API for position first (much faster than AX)
    let skylight_pos_ok =
        crate::modules::tiling::ffi::skylight::move_window_fast(target.window_id, frame.x, frame.y);

    // Resolve AX element once - needed for size, and maybe position fallback
    let Some(element) = resolve_window_element(target) else {
        return skylight_pos_ok; // Can't do AX ops, return SkyLight result
    };

    // If SkyLight failed, fall back to AX for position
    let pos_ok = if skylight_pos_ok {
        true
    } else {
        unsafe { set_ax_position(element, frame.x, frame.y) }
    };

    // Always use AX for size (SkyLight doesn't support resize)
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
pub fn set_window_frame_verified(target: WindowTarget, frame: &Rect) -> FrameSetResult {
    let Some(ax_element) = get_ax_element_for_window(target) else {
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
        (frame.width < min_w - 1.0, frame.height < min_h - 1.0)
    } else {
        (false, false)
    };

    // Set the frame
    let success = ax_element.set_frame(frame).is_ok();

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
                let width_diff = (actual.width - frame.width).abs();
                let height_diff = (actual.height - frame.height).abs();

                // If actual is larger than target and we're not close, we hit a minimum
                let hit_width = width_diff > 1.0 && actual.width > frame.width;
                let hit_height = height_diff > 1.0 && actual.height > frame.height;

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
pub fn get_window_minimum_size(target: WindowTarget) -> Option<(f64, f64)> {
    let ax_element = get_ax_element_for_window(target)?;
    ax_element.minimum_size()
}

/// Gets an `AXElement` for an exact target.
fn get_ax_element_for_window(
    target: WindowTarget,
) -> Option<crate::modules::tiling::ffi::accessibility::AXElement> {
    use crate::modules::tiling::ffi::accessibility::AXElement;

    // Revalidate the exact identity before touching the app.
    if resolve_current_identity(target.identity) != Some(target.identity) {
        return None;
    }

    let app = AXElement::application(target.identity.pid)?;

    // Find the window with matching ID
    app.windows().into_iter().find(|w| w.window_id() == Some(target.window_id))
}

/// Focuses a window by its tracked identity — exact target only.
///
/// Resolves the stored `Window.identity` and builds the exact `WindowTarget`;
/// returns `false` (and logs) when the window has no stored identity.
#[must_use]
pub fn focus_stored_window(
    state: &crate::modules::tiling::state::TilingState,
    window_id: u32,
) -> bool {
    let Some(identity) = state.get_window(window_id).and_then(|w| w.identity) else {
        tracing::trace!("focus_stored_window: no identity for window {window_id}, skipping");
        return false;
    };
    focus_window(WindowTarget { identity, window_id })
}

/// Focuses a window (gives it keyboard focus).
///
/// # Arguments
///
/// * `target` - The exact target window to focus.
///
/// # Returns
///
/// `true` if the operation succeeded (optimistically, since execution is async).
#[must_use]
pub fn focus_window(target: WindowTarget) -> bool {
    // Dispatch to main thread using the project's existing dispatch utility
    crate::platform::thread::dispatch_on_main(move || {
        focus_window_impl(target);
    });

    // Return true optimistically - the actual operation runs async
    true
}

/// Focuses a window on the main thread and waits until the activation and AX
/// focus requests have been issued.
///
/// Workspace switches must show the destination application, activate it, and
/// only then hide applications from the previous workspace. The asynchronous
/// [`focus_window`] helper cannot provide that ordering guarantee.
#[must_use]
pub fn focus_window_sync(target: WindowTarget) -> bool {
    crate::platform::thread::dispatch_on_main_sync(move || focus_window_impl(target))
}

/// Focuses a tracked window synchronously.
#[must_use]
pub fn focus_stored_window_sync(
    state: &crate::modules::tiling::state::TilingState,
    window_id: u32,
) -> bool {
    let Some(identity) = state.get_window(window_id).and_then(|w| w.identity) else {
        tracing::trace!("focus_stored_window_sync: no identity for window {window_id}, skipping");
        return false;
    };
    focus_window_sync(WindowTarget { identity, window_id })
}

/// Internal implementation of `focus_window` (runs on main thread).
fn focus_window_impl(target: WindowTarget) -> bool {
    // Activate the owning application first - this is critical for focus to work
    if !activate_app(target.identity) {
        return false;
    }

    let Some(element) = resolve_window_element(target) else {
        tracing::debug!("focus_window: could not resolve window {}", target.window_id);
        return false;
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

    true
}

/// Returns whether a currently resolved application is the expected instance.
fn identity_matches_for_activation(expected: AppIdentity, actual: AppIdentity) -> bool {
    expected == actual
}

/// Activates an exact application instance.
///
/// Uses `NSApplicationActivateAllWindows | NSApplicationActivateIgnoringOtherApps` (3)
/// which is important for cycling through same-app windows in monocle layout.
fn activate_app(identity: AppIdentity) -> bool {
    use objc::runtime::{BOOL, Class, Object, YES};
    use objc::{msg_send, sel, sel_impl};

    objc::rc::autoreleasepool(|| unsafe {
        let Some(app_class) = Class::get("NSRunningApplication") else {
            return false;
        };

        let app: *mut Object =
            msg_send![app_class, runningApplicationWithProcessIdentifier: identity.pid];
        if app.is_null() {
            return false;
        }

        let Some(actual) = AppIdentity::from_ns_running_app_with_pid(app, identity.pid) else {
            return false;
        };
        if !identity_matches_for_activation(identity, actual) {
            tracing::debug!("focus_window: refusing activation for stale application identity");
            return false;
        }

        let result: BOOL = msg_send![app, activateWithOptions: 3u64]; // AllWindows | IgnoringOtherApps
        result == YES
    })
}

/// Raises (brings to front) a window.
///
/// # Arguments
///
/// * `target` - The exact target window to raise.
///
/// # Returns
///
/// `true` if the operation succeeded (optimistically, since execution is async).
#[must_use]
pub fn raise_window(target: WindowTarget) -> bool {
    // Dispatch to main thread using the project's existing dispatch utility
    crate::platform::thread::dispatch_on_main(move || {
        raise_window_impl(target);
    });

    // Return true optimistically - the actual operation runs async
    true
}

/// Internal implementation of `raise_window` (runs on main thread).
fn raise_window_impl(target: WindowTarget) {
    let Some(element) = resolve_window_element(target) else {
        tracing::debug!("raise_window: could not resolve window {}", target.window_id);
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
/// * `frames` - Vector of (exact target, frame) pairs.
///
/// # Returns
///
/// Number of frames queued (actual application is async).
#[must_use]
pub fn set_window_frames_batch(frames: &[(WindowTarget, Rect)]) -> usize {
    if frames.is_empty() {
        return 0;
    }

    let count = frames.len();
    let frames_copy: Vec<(WindowTarget, Rect)> = frames.to_vec();

    // Single main thread dispatch for all frames
    crate::platform::thread::dispatch_on_main(move || {
        for (target, frame) in frames_copy {
            set_window_frame_impl(target, &frame);
        }
    });

    count
}

// ============================================================================
// App Visibility Operations
// ============================================================================

/// Outcome of unhiding (showing) an application.
///
/// Distinguishes whether Stache actually performed a hidden→shown transition
/// (unhide succeeded and the app was hidden), the app was already shown,
/// or the operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnhideAppOutcome {
    /// App was actually hidden and Stache unhid it — Stache should
    /// expect an `AppShown` notification.
    UnhiddenByStache,
    /// App was already visible — no `AppShown` will be generated.
    AlreadyShown,
    /// Unhide operation failed (app not found, etc.).
    Failed,
}

impl UnhideAppOutcome {
    /// Returns `true` if the app is now in a shown state (either unhidden by us
    /// or was already shown).
    #[must_use]
    pub const fn succeeded(self) -> bool {
        matches!(self, Self::UnhiddenByStache | Self::AlreadyShown)
    }
}

/// Reads an application's accessibility `AXHidden` state.
///
/// `NSRunningApplication.isHidden` neither reflects `AXHidden` nor changes for
/// apps whose `hide()` is a no-op (e.g. Device Hub).
#[must_use]
pub(crate) fn ax_app_hidden(pid: i32) -> Option<bool> { AXElement::application(pid)?.is_hidden() }

/// Sets an application's accessibility `AXHidden` attribute.
pub(crate) fn set_ax_app_hidden(pid: i32, hidden: bool) -> bool {
    AXElement::application(pid).is_some_and(|app| app.set_hidden(hidden).is_ok())
}

/// Hides an application instance, validated by its exact identity.
///
/// Re-resolves `NSRunningApplication` for the identity's PID, captures the
/// actual identity from that same object, and refuses the hide on any
/// mismatch (PID reuse). Runs in its own autorelease pool (called from the
/// actor's task thread).
///
/// `NSRunningApplication.hide()` is a no-op for some applications, so the
/// accessibility `AXHidden` attribute is used as a fallback.
#[must_use]
pub fn hide_app_instance_with_outcome(identity: AppIdentity) -> HideAppOutcome {
    objc::rc::autoreleasepool(|| unsafe {
        let Some(app_class) = Class::get("NSRunningApplication") else {
            return HideAppOutcome::Failed;
        };
        let app: *mut Object = msg_send![
            app_class,
            runningApplicationWithProcessIdentifier: identity.pid
        ];
        if app.is_null() {
            return HideAppOutcome::Failed;
        }
        let Some(actual) = AppIdentity::from_ns_running_app_with_pid(app, identity.pid) else {
            return HideAppOutcome::Failed;
        };
        if actual != identity {
            // PID-reuse: a different process now owns this PID.
            return HideAppOutcome::Failed;
        }
        let is_hidden: BOOL = msg_send![app, isHidden];
        if is_hidden == YES || ax_app_hidden(identity.pid) == Some(true) {
            return HideAppOutcome::AlreadyHidden;
        }
        let result: BOOL = msg_send![app, hide];
        if result == YES || set_ax_app_hidden(identity.pid, true) {
            HideAppOutcome::HiddenByStache
        } else {
            HideAppOutcome::Failed
        }
    })
}

/// Unhides an application instance, validated by its exact identity.
///
/// Same exact-instance validation as [`hide_app_instance_with_outcome`].
#[must_use]
pub fn unhide_app_instance_with_outcome(identity: AppIdentity) -> UnhideAppOutcome {
    objc::rc::autoreleasepool(|| unsafe {
        let Some(app_class) = Class::get("NSRunningApplication") else {
            return UnhideAppOutcome::Failed;
        };
        let app: *mut Object = msg_send![
            app_class,
            runningApplicationWithProcessIdentifier: identity.pid
        ];
        if app.is_null() {
            return UnhideAppOutcome::Failed;
        }
        let Some(actual) = AppIdentity::from_ns_running_app_with_pid(app, identity.pid) else {
            return UnhideAppOutcome::Failed;
        };
        if actual != identity {
            return UnhideAppOutcome::Failed;
        }
        let ns_hidden: BOOL = msg_send![app, isHidden];
        let ax_hidden = ax_app_hidden(identity.pid) == Some(true);
        if ns_hidden != YES && !ax_hidden {
            return UnhideAppOutcome::AlreadyShown;
        }
        if ns_hidden == YES {
            let result: BOOL = msg_send![app, unhide];
            if result == YES {
                return UnhideAppOutcome::UnhiddenByStache;
            }
        }
        if ax_hidden && set_ax_app_hidden(identity.pid, false) {
            return UnhideAppOutcome::UnhiddenByStache;
        }
        UnhideAppOutcome::Failed
    })
}

/// OS hidden state for an exact identity, validated on the same local
/// `NSRunningApplication`. Read-only; lifecycle handlers never hide/unhide.
///
/// Creates its own autorelease pool (actor task thread).
#[must_use]
pub fn app_instance_is_hidden(identity: AppIdentity) -> Option<bool> {
    objc::rc::autoreleasepool(|| unsafe {
        let app_class = objc::runtime::Class::get("NSRunningApplication")?;
        let app: *mut objc::runtime::Object = msg_send![
            app_class,
            runningApplicationWithProcessIdentifier: identity.pid
        ];
        if app.is_null() {
            return None;
        }
        let actual = AppIdentity::from_ns_running_app_with_pid(app, identity.pid)?;
        if actual != identity {
            return None; // PID-reuse or process mismatch
        }
        let is_hidden: BOOL = msg_send![app, isHidden];
        if is_hidden == YES {
            return Some(true);
        }
        Some(ax_app_hidden(identity.pid) == Some(true))
    })
}

// ============================================================================
// Hide Outcome
// ============================================================================

/// Outcome of hiding an application.
///
/// Tracks whether the app was hidden by Stache (so we own the responsibility
/// to restore it), was already hidden, or the operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HideAppOutcome {
    /// App was successfully hidden by Stache - we own restoring it.
    HiddenByStache,
    /// App was already hidden - no ownership taken.
    AlreadyHidden,
    /// Hide operation failed.
    Failed,
}

impl HideAppOutcome {
    /// Returns `true` if the hide operation was functionally successful
    /// (either hidden by us or already hidden).
    #[must_use]
    #[allow(dead_code)] // public API kept for outcome consumers
    const fn succeeded(self) -> bool { matches!(self, Self::HiddenByStache | Self::AlreadyHidden) }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::identity::LaunchDateBits;

    // Note: test_resolve_nonexistent_window is disabled because it requires
    // accessibility permissions and can crash if permissions are not granted.
    // The function is still tested indirectly through integration tests.

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

    #[test]
    fn test_identity_mismatch_rejects_activation() {
        let expected = AppIdentity {
            pid: 1000,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(1.0).unwrap(),
        };
        let replacement = AppIdentity {
            pid: 1000,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(2.0).unwrap(),
        };

        assert!(!identity_matches_for_activation(expected, replacement));
    }
}
