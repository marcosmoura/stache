//! Safe wrappers for macOS Accessibility API (`AXUIElement`).
//!
//! This module provides a safe Rust interface to the macOS Accessibility API,
//! which is used to query and manipulate windows. The main type is [`AXElement`],
//! which wraps an `AXUIElementRef` and provides automatic memory management.
//!
//! # Example
//!
//! ```rust,ignore
//! use stache::tiling::ffi::AXElement;
//!
//! // Create an element for an application
//! if let Some(app) = AXElement::application(pid) {
//!     // Get all windows
//!     for window in app.windows() {
//!         if let Some(title) = window.title() {
//!             println!("Window: {}", title);
//!         }
//!     }
//! }
//! ```
//!
//! # Thread Safety
//!
//! The macOS Accessibility API is thread-safe for operations on different
//! elements. `AXElement` implements `Send` and `Sync` to allow use across
//! threads. However, users should avoid concurrent modifications to the
//! same window from multiple threads.
//!
//! # Memory Management
//!
//! `AXElement` uses RAII to manage the underlying `AXUIElementRef`. When an
//! `AXElement` is dropped, it automatically calls `CFRelease`. Cloning an
//! `AXElement` calls `CFRetain` to increment the reference count.

use std::cell::OnceCell;
use std::ffi::c_void;
use std::ptr;

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;

use crate::tiling::error::{TilingError, TilingResult};
use crate::tiling::state::Rect;

// ============================================================================
// FFI Declarations
// ============================================================================

type AXUIElementRef = *mut c_void;
type AXError = i32;

const K_AX_ERROR_SUCCESS: AXError = 0;
const K_AX_ERROR_INVALID_UI_ELEMENT: AXError = -25202;
const K_AX_ERROR_ATTRIBUTE_UNSUPPORTED: AXError = -25205;
const K_AX_ERROR_ACTION_UNSUPPORTED: AXError = -25206;
const K_AX_ERROR_NOT_IMPLEMENTED: AXError = -25208;
const K_AX_ERROR_CANNOT_COMPLETE: AXError = -25204;

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
// AXElement
// ============================================================================

/// A safe wrapper around `AXUIElementRef`.
///
/// This type provides automatic memory management (via `Drop`) and a safe
/// interface to the macOS Accessibility API. It can represent either an
/// application element or a window element.
///
/// # Safety
///
/// The underlying `AXUIElementRef` is a Core Foundation type that uses
/// reference counting. `AXElement` manages this automatically:
/// - Creating an `AXElement` takes ownership of the reference
/// - Cloning increments the reference count via `CFRetain`
/// - Dropping decrements the reference count via `CFRelease`
///
/// # Thread Safety
///
/// The Accessibility API is thread-safe for operations on different elements.
/// `AXElement` implements `Send` and `Sync` to allow use in parallel operations
/// (e.g., positioning multiple windows simultaneously).
pub struct AXElement {
    /// The underlying `AXUIElementRef`. Never null for a valid `AXElement`.
    raw: AXUIElementRef,
}

impl AXElement {
    // ========================================================================
    // Constructors
    // ========================================================================

    /// Creates an `AXElement` for an application by its process ID.
    ///
    /// Returns `None` if the application cannot be accessed (e.g., if the
    /// process doesn't exist or accessibility permissions are not granted).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if let Some(app) = AXElement::application(pid) {
    ///     // Work with the application
    /// }
    /// ```
    #[must_use]
    pub fn application(pid: i32) -> Option<Self> {
        let raw = unsafe { AXUIElementCreateApplication(pid) };
        if raw.is_null() {
            None
        } else {
            Some(Self { raw })
        }
    }

    /// Creates an `AXElement` from a raw pointer, taking ownership.
    ///
    /// Returns `None` if the pointer is null.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - The pointer is a valid `AXUIElementRef`
    /// - The caller transfers ownership (does not call `CFRelease` separately)
    #[must_use]
    pub const unsafe fn from_raw(raw: AXUIElementRef) -> Option<Self> {
        if raw.is_null() {
            None
        } else {
            Some(Self { raw })
        }
    }

    /// Creates an `AXElement` from a raw pointer, retaining it.
    ///
    /// Returns `None` if the pointer is null.
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - The pointer is a valid `AXUIElementRef`
    /// - The original reference is still valid and owned by someone else
    #[must_use]
    pub unsafe fn from_raw_retained(raw: AXUIElementRef) -> Option<Self> {
        if raw.is_null() {
            None
        } else {
            unsafe { CFRetain(raw.cast()) };
            Some(Self { raw })
        }
    }

    /// Returns the raw `AXUIElementRef` without transferring ownership.
    ///
    /// The returned pointer is valid as long as this `AXElement` is alive.
    #[must_use]
    pub const fn as_raw(&self) -> AXUIElementRef { self.raw }

    /// Consumes the `AXElement` and returns the raw pointer.
    ///
    /// The caller takes ownership and is responsible for calling `CFRelease`.
    #[must_use]
    pub const fn into_raw(self) -> AXUIElementRef {
        let raw = self.raw;
        std::mem::forget(self);
        raw
    }

    // ========================================================================
    // Window Enumeration
    // ========================================================================

    /// Gets all windows belonging to this application.
    ///
    /// Returns an empty vector if this is not an application element or if
    /// the application has no windows.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if let Some(app) = AXElement::application(pid) {
    ///     for window in app.windows() {
    ///         println!("Found window: {:?}", window.title());
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn windows(&self) -> Vec<Self> { unsafe { self.get_windows_internal() } }

    /// Gets the focused window of this application.
    ///
    /// Returns `None` if this is not an application element or if the
    /// application has no focused window.
    #[must_use]
    pub fn focused_window(&self) -> Option<Self> {
        let mut value: *mut c_void = ptr::null_mut();
        let result =
            unsafe { AXUIElementCopyAttributeValue(self.raw, cf_focused_window(), &raw mut value) };

        if result != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }

        // Transfer ownership to AXElement
        unsafe { Self::from_raw(value.cast()) }
    }

    /// Internal implementation for getting windows.
    unsafe fn get_windows_internal(&self) -> Vec<Self> {
        let mut value: *mut c_void = ptr::null_mut();
        let result =
            unsafe { AXUIElementCopyAttributeValue(self.raw, cf_windows(), &raw mut value) };

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
                let element_ref: AXUIElementRef = window.cast_mut();
                if let Some(role) = unsafe { get_string_attr(element_ref, cf_role()) }
                    && role == "AXWindow"
                {
                    // Retain since CFArrayGetValueAtIndex doesn't transfer ownership
                    unsafe { CFRetain(window) };
                    windows.push(Self { raw: window.cast_mut() });
                }
            }
        }

        unsafe { CFRelease(value) };
        windows
    }

    // ========================================================================
    // String Attributes
    // ========================================================================

    /// Gets the window title.
    ///
    /// Returns `None` if the element has no title attribute.
    #[must_use]
    pub fn title(&self) -> Option<String> { unsafe { get_string_attr(self.raw, cf_title()) } }

    /// Gets the element's role (e.g., "`AXWindow`", "`AXButton`").
    #[must_use]
    pub fn role(&self) -> Option<String> { unsafe { get_string_attr(self.raw, cf_role()) } }

    /// Gets the element's subrole (e.g., "`AXStandardWindow`").
    #[must_use]
    pub fn subrole(&self) -> Option<String> { unsafe { get_string_attr(self.raw, cf_subrole()) } }

    // ========================================================================
    // Boolean Attributes
    // ========================================================================

    /// Returns whether this element is focused.
    #[must_use]
    pub fn is_focused(&self) -> Option<bool> { unsafe { get_bool_attr(self.raw, cf_focused()) } }

    /// Returns whether this window is minimized.
    #[must_use]
    pub fn is_minimized(&self) -> Option<bool> {
        unsafe { get_bool_attr(self.raw, cf_minimized()) }
    }

    /// Returns whether this application is hidden.
    #[must_use]
    pub fn is_hidden(&self) -> Option<bool> { unsafe { get_bool_attr(self.raw, cf_hidden()) } }

    /// Returns whether this is the main window.
    #[must_use]
    pub fn is_main(&self) -> Option<bool> { unsafe { get_bool_attr(self.raw, cf_main()) } }

    // ========================================================================
    // Geometry
    // ========================================================================

    /// Gets the position of this element as (x, y).
    #[must_use]
    pub fn position(&self) -> Option<(f64, f64)> { unsafe { get_position_attr(self.raw) } }

    /// Gets the size of this element as (width, height).
    #[must_use]
    pub fn size(&self) -> Option<(f64, f64)> { unsafe { get_size_attr(self.raw) } }

    /// Gets the frame (position and size) of this element.
    #[must_use]
    pub fn frame(&self) -> Option<Rect> {
        let (x, y) = self.position()?;
        let (width, height) = self.size()?;
        Some(Rect::new(x, y, width, height))
    }

    /// Sets the position of this element.
    ///
    /// # Errors
    ///
    /// Returns an error if the position cannot be set.
    pub fn set_position(&self, x: f64, y: f64) -> TilingResult<()> {
        unsafe { set_position_attr(self.raw, x, y) }
    }

    /// Sets the size of this element.
    ///
    /// # Errors
    ///
    /// Returns an error if the size cannot be set.
    pub fn set_size(&self, width: f64, height: f64) -> TilingResult<()> {
        unsafe { set_size_attr(self.raw, width, height) }
    }

    /// Sets the frame (position and size) of this element.
    ///
    /// This performs the operations in the optimal order for reliable resizing:
    /// 1. Set size first (allows window to shrink)
    /// 2. Set position
    /// 3. Set size again (some apps need this)
    ///
    /// # Errors
    ///
    /// Returns an error if the frame cannot be set.
    pub fn set_frame(&self, frame: &Rect) -> TilingResult<()> {
        // Order matters for reliable resizing
        let size_1 = self.set_size(frame.width, frame.height);
        let pos = self.set_position(frame.x, frame.y);
        let size_2 = self.set_size(frame.width, frame.height);

        // Consider success if position and at least one size operation succeeded
        if pos.is_ok() && (size_1.is_ok() || size_2.is_ok()) {
            Ok(())
        } else {
            Err(TilingError::window_op("Failed to set window frame"))
        }
    }

    /// Sets the frame using the fast path (2 AX calls instead of 3).
    ///
    /// Use this during animations where windows move in small increments.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame cannot be set.
    pub fn set_frame_fast(&self, frame: &Rect) -> TilingResult<()> {
        self.set_position(frame.x, frame.y)?;
        self.set_size(frame.width, frame.height)?;
        Ok(())
    }

    // ========================================================================
    // Actions
    // ========================================================================

    /// Raises (brings to front) this window.
    ///
    /// # Errors
    ///
    /// Returns an error if the window cannot be raised.
    pub fn raise(&self) -> TilingResult<()> {
        let result = unsafe { AXUIElementPerformAction(self.raw, cf_raise()) };
        ax_result_to_tiling_result(result, "raise window")
    }
}

// ============================================================================
// Trait Implementations
// ============================================================================

impl Drop for AXElement {
    fn drop(&mut self) {
        // SAFETY: self.raw is guaranteed to be valid and non-null
        unsafe { CFRelease(self.raw.cast()) };
    }
}

impl Clone for AXElement {
    fn clone(&self) -> Self {
        // SAFETY: self.raw is guaranteed to be valid and non-null
        unsafe { CFRetain(self.raw.cast()) };
        Self { raw: self.raw }
    }
}

// SAFETY: The Accessibility API is thread-safe for operations on different elements.
// Each AXElement represents a unique UI element reference.
unsafe impl Send for AXElement {}

// SAFETY: Concurrent reads are safe. Concurrent writes to the same window
// may result in undefined behavior at the OS level, but won't cause memory
// unsafety in Rust.
unsafe impl Sync for AXElement {}

impl std::fmt::Debug for AXElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AXElement")
            .field("raw", &self.raw)
            .field("role", &self.role())
            .field("title", &self.title())
            .finish()
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Gets a string attribute from an element.
unsafe fn get_string_attr(element: AXUIElementRef, attr: *const c_void) -> Option<String> {
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

/// Gets a boolean attribute from an element.
unsafe fn get_bool_attr(element: AXUIElementRef, attr: *const c_void) -> Option<bool> {
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

/// Gets the position attribute from an element.
unsafe fn get_position_attr(element: AXUIElementRef) -> Option<(f64, f64)> {
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

/// Gets the size attribute from an element.
unsafe fn get_size_attr(element: AXUIElementRef) -> Option<(f64, f64)> {
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

/// Sets the position attribute on an element.
unsafe fn set_position_attr(element: AXUIElementRef, x: f64, y: f64) -> TilingResult<()> {
    if element.is_null() {
        return Err(TilingError::window_op("Null element"));
    }

    let point = core_graphics::geometry::CGPoint::new(x, y);
    let value = unsafe { AXValueCreate(K_AX_VALUE_TYPE_CG_POINT, (&raw const point).cast()) };

    if value.is_null() {
        return Err(TilingError::window_op("Failed to create AXValue for position"));
    }

    let result = unsafe { AXUIElementSetAttributeValue(element, cf_position(), value.cast()) };
    unsafe { CFRelease(value.cast()) };

    ax_result_to_tiling_result(result, "set position")
}

/// Sets the size attribute on an element.
unsafe fn set_size_attr(element: AXUIElementRef, width: f64, height: f64) -> TilingResult<()> {
    if element.is_null() {
        return Err(TilingError::window_op("Null element"));
    }

    let size = core_graphics::geometry::CGSize::new(width, height);
    let value = unsafe { AXValueCreate(K_AX_VALUE_TYPE_CG_SIZE, (&raw const size).cast()) };

    if value.is_null() {
        return Err(TilingError::window_op("Failed to create AXValue for size"));
    }

    let result = unsafe { AXUIElementSetAttributeValue(element, cf_size(), value.cast()) };
    unsafe { CFRelease(value.cast()) };

    ax_result_to_tiling_result(result, "set size")
}

/// Converts an AX error code to a `TilingResult`.
fn ax_result_to_tiling_result(result: AXError, operation: &str) -> TilingResult<()> {
    if result == K_AX_ERROR_SUCCESS {
        Ok(())
    } else {
        let message = match result {
            K_AX_ERROR_INVALID_UI_ELEMENT => "Invalid UI element",
            K_AX_ERROR_ATTRIBUTE_UNSUPPORTED => "Attribute unsupported",
            K_AX_ERROR_ACTION_UNSUPPORTED => "Action unsupported",
            K_AX_ERROR_NOT_IMPLEMENTED => "Not implemented",
            K_AX_ERROR_CANNOT_COMPLETE => "Cannot complete operation",
            _ => "Unknown error",
        };
        Err(TilingError::accessibility(
            result,
            format!("{operation}: {message}"),
        ))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ax_element_application_invalid_pid() {
        // PID 0 should return None (no such process)
        let result = AXElement::application(0);
        // Note: This might actually succeed on some systems, so we just check it doesn't panic
        drop(result);
    }

    #[test]
    fn test_ax_element_from_raw_null() {
        let result = unsafe { AXElement::from_raw(ptr::null_mut()) };
        assert!(result.is_none());
    }

    #[test]
    fn test_ax_element_from_raw_retained_null() {
        let result = unsafe { AXElement::from_raw_retained(ptr::null_mut()) };
        assert!(result.is_none());
    }

    #[test]
    fn test_ax_result_to_tiling_result_success() {
        let result = ax_result_to_tiling_result(K_AX_ERROR_SUCCESS, "test");
        assert!(result.is_ok());
    }

    #[test]
    fn test_ax_result_to_tiling_result_error() {
        let result = ax_result_to_tiling_result(K_AX_ERROR_INVALID_UI_ELEMENT, "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_ax_error_constants_are_negative() {
        // AX error codes are negative
        assert!(K_AX_ERROR_INVALID_UI_ELEMENT < 0);
        assert!(K_AX_ERROR_ATTRIBUTE_UNSUPPORTED < 0);
        assert!(K_AX_ERROR_ACTION_UNSUPPORTED < 0);
        assert!(K_AX_ERROR_NOT_IMPLEMENTED < 0);
        assert!(K_AX_ERROR_CANNOT_COMPLETE < 0);
    }

    #[test]
    fn test_cached_cfstring_thread_local() {
        // Just verify the cached CFString functions don't panic
        let _ = cf_windows();
        let _ = cf_title();
        let _ = cf_role();
        let _ = cf_position();
        let _ = cf_size();
    }
}
