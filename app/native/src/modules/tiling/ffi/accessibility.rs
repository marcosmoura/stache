//! Safe wrappers for macOS Accessibility API (`AXUIElement`).
//!
//! This module provides a safe Rust interface to the macOS Accessibility API,
//! which is used to query and manipulate windows. The main type is [`AXElement`],
//! which wraps an `AXUIElementRef` and provides automatic memory management.
//!
//! # Thread Safety
//!
//! The macOS Accessibility API is thread-safe for operations on different
//! elements. `AXElement` implements `Send` and `Sync` to allow use across
//! threads.

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
    /// Private API to get CGWindowID from an AXUIElement.
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
    static CF_CHILDREN: OnceCell<CFString> = const { OnceCell::new() };
    static CF_TITLE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_ROLE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_SUBROLE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_POSITION: OnceCell<CFString> = const { OnceCell::new() };
    static CF_SIZE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_MINIMUM_SIZE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_FOCUSED: OnceCell<CFString> = const { OnceCell::new() };
    static CF_FOCUSED_WINDOW: OnceCell<CFString> = const { OnceCell::new() };
    static CF_MINIMIZED: OnceCell<CFString> = const { OnceCell::new() };
    static CF_HIDDEN: OnceCell<CFString> = const { OnceCell::new() };
    static CF_MAIN: OnceCell<CFString> = const { OnceCell::new() };
    static CF_RAISE: OnceCell<CFString> = const { OnceCell::new() };
    static CF_FULLSCREEN: OnceCell<CFString> = const { OnceCell::new() };
    static CF_TABS: OnceCell<CFString> = const { OnceCell::new() };
    static CF_TAB_GROUP_ROLE: OnceCell<CFString> = const { OnceCell::new() };
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
fn cf_children() -> *const c_void { cached_cfstring!(CF_CHILDREN, "AXChildren") }

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
fn cf_minimum_size() -> *const c_void { cached_cfstring!(CF_MINIMUM_SIZE, "AXMinimumSize") }

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

#[inline]
fn cf_fullscreen() -> *const c_void { cached_cfstring!(CF_FULLSCREEN, "AXFullScreen") }

#[inline]
fn cf_tabs() -> *const c_void { cached_cfstring!(CF_TABS, "AXTabs") }

#[inline]
fn cf_tab_group_role() -> *const c_void { cached_cfstring!(CF_TAB_GROUP_ROLE, "AXTabGroup") }

// ============================================================================
// AXElement
// ============================================================================

/// A safe wrapper around `AXUIElementRef`.
///
/// This type provides automatic memory management (via `Drop`) and a safe
/// interface to the macOS Accessibility API.
pub struct AXElement {
    /// The underlying `AXUIElementRef`. Never null for a valid `AXElement`.
    raw: AXUIElementRef,
}

impl AXElement {
    // ========================================================================
    // Constructors
    // ========================================================================

    /// Creates an `AXElement` for an application by its process ID.
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
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - The pointer is a valid `AXUIElementRef`
    /// - The caller transfers ownership
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
    /// # Safety
    ///
    /// The caller must ensure the pointer is valid.
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
    #[must_use]
    pub fn windows(&self) -> Vec<Self> { unsafe { self.get_windows_internal() } }

    /// Gets the focused window of this application.
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

    /// Returns whether this window is in fullscreen mode.
    #[must_use]
    pub fn is_fullscreen(&self) -> Option<bool> {
        unsafe { get_bool_attr(self.raw, cf_fullscreen()) }
    }

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

    /// Gets the minimum size of this element as (width, height).
    ///
    /// Not all windows support this attribute. Returns `None` if the
    /// attribute is not available or cannot be read.
    #[must_use]
    pub fn minimum_size(&self) -> Option<(f64, f64)> {
        unsafe { get_size_attr_with_key(self.raw, cf_minimum_size()) }
    }

    /// Sets the position of this element.
    ///
    /// # Errors
    ///
    /// Returns an error if the position cannot be set.
    pub fn set_position(&self, x: f64, y: f64) -> Result<(), String> {
        unsafe { set_position_attr(self.raw, x, y) }
    }

    /// Sets the size of this element.
    ///
    /// # Errors
    ///
    /// Returns an error if the size cannot be set.
    pub fn set_size(&self, width: f64, height: f64) -> Result<(), String> {
        unsafe { set_size_attr(self.raw, width, height) }
    }

    /// Sets the frame (position and size) of this element.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame cannot be set.
    pub fn set_frame(&self, frame: &Rect) -> Result<(), String> {
        // Order matters for reliable resizing
        let size_1 = self.set_size(frame.width, frame.height);
        let pos = self.set_position(frame.x, frame.y);
        let size_2 = self.set_size(frame.width, frame.height);

        // Consider success if position and at least one size operation succeeded
        if pos.is_ok() && (size_1.is_ok() || size_2.is_ok()) {
            Ok(())
        } else {
            Err("Failed to set window frame".to_string())
        }
    }

    /// Sets the frame using the fast path (2 AX calls instead of 3).
    ///
    /// # Errors
    ///
    /// Returns an error if the frame cannot be set.
    pub fn set_frame_fast(&self, frame: &Rect) -> Result<(), String> {
        self.set_position(frame.x, frame.y)?;
        self.set_size(frame.width, frame.height)?;
        Ok(())
    }

    /// Sets the frame and verifies it was applied correctly.
    ///
    /// Returns `Ok(Some(actual_frame))` if the window couldn't reach the target size
    /// (e.g., due to minimum size constraints), with the actual resulting frame.
    /// Returns `Ok(None)` if the frame was applied as requested.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame operation completely failed.
    pub fn set_frame_verified(&self, target: &Rect) -> Result<Option<Rect>, String> {
        self.set_frame(target)?;

        // Query actual frame to verify
        let Some(actual) = self.frame() else {
            return Ok(None); // Can't verify, assume success
        };

        // Check if size differs significantly (more than 1 pixel)
        let width_diff = (actual.width - target.width).abs();
        let height_diff = (actual.height - target.height).abs();

        if width_diff > 1.0 || height_diff > 1.0 {
            Ok(Some(actual))
        } else {
            Ok(None)
        }
    }

    /// Checks if the target size would violate this window's minimum size constraints.
    ///
    /// Returns `Some((min_width, min_height))` if the window has minimum size constraints
    /// and the target would violate them. Returns `None` if constraints are met or unknown.
    #[must_use]
    pub fn check_minimum_size(&self, target_width: f64, target_height: f64) -> Option<(f64, f64)> {
        let (min_w, min_h) = self.minimum_size()?;

        if target_width < min_w || target_height < min_h {
            Some((min_w, min_h))
        } else {
            None
        }
    }

    // ========================================================================
    // Actions
    // ========================================================================

    /// Raises (brings to front) this window.
    ///
    /// # Errors
    ///
    /// Returns an error if the window cannot be raised.
    pub fn raise(&self) -> Result<(), String> {
        let result = unsafe { AXUIElementPerformAction(self.raw, cf_raise()) };
        ax_result_to_error(result, "raise window")
    }

    // ========================================================================
    // Window ID
    // ========================================================================

    /// Gets the `CGWindowID` for this window element.
    ///
    /// This uses a private API (`_AXUIElementGetWindow`) to get the exact
    /// mapping from AX element to window ID.
    #[must_use]
    pub fn window_id(&self) -> Option<u32> {
        let mut window_id: u32 = 0;
        let result = unsafe { _AXUIElementGetWindow(self.raw, &raw mut window_id) };
        if result == K_AX_ERROR_SUCCESS && window_id != 0 {
            Some(window_id)
        } else {
            None
        }
    }

    // ========================================================================
    // Tab Detection
    // ========================================================================

    /// Gets the children of this element.
    #[must_use]
    pub fn children(&self) -> Vec<Self> {
        let mut value: *mut c_void = ptr::null_mut();
        let result =
            unsafe { AXUIElementCopyAttributeValue(self.raw, cf_children(), &raw mut value) };

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
        let mut children = Vec::with_capacity(count as usize);

        for i in 0..count {
            let child = unsafe { CFArrayGetValueAtIndex(value, i) };
            if !child.is_null() && unsafe { CFGetTypeID(child) } == ax_type_id {
                // Retain since CFArrayGetValueAtIndex doesn't transfer ownership
                unsafe { CFRetain(child) };
                children.push(Self { raw: child.cast_mut() });
            }
        }

        unsafe { CFRelease(value) };
        children
    }

    /// Finds a tab group child element if one exists.
    ///
    /// Tab groups are UI elements with role "AXTabGroup" that contain tabs.
    /// This is used for detecting native macOS tabs in applications like Finder.
    #[must_use]
    pub fn find_tab_group(&self) -> Option<Self> {
        let tab_group_role = "AXTabGroup";

        for child in self.children() {
            if child.role().is_some_and(|role| role == tab_group_role) {
                return Some(child);
            }
            // Also check in AXGroup children (Safari 14+ puts tabs in an AXGroup)
            if child.role().is_some_and(|role| role == "AXGroup") && child.has_tabs_attribute() {
                // Check if this group has an AXTabs attribute
                return Some(child);
            }
        }
        None
    }

    /// Checks if this element has an AXTabs attribute.
    #[must_use]
    fn has_tabs_attribute(&self) -> bool {
        let mut value: *mut c_void = ptr::null_mut();
        let result = unsafe { AXUIElementCopyAttributeValue(self.raw, cf_tabs(), &raw mut value) };

        if result == K_AX_ERROR_SUCCESS && !value.is_null() {
            unsafe { CFRelease(value) };
            true
        } else {
            false
        }
    }

    /// Gets the number of tabs in this tab group element.
    ///
    /// Returns 0 if this is not a tab group or has no tabs.
    #[must_use]
    pub fn tab_count(&self) -> usize {
        let mut value: *mut c_void = ptr::null_mut();
        let result = unsafe { AXUIElementCopyAttributeValue(self.raw, cf_tabs(), &raw mut value) };

        if result != K_AX_ERROR_SUCCESS || value.is_null() {
            return 0;
        }

        let count = unsafe { CFArrayGetCount(value) };
        unsafe { CFRelease(value) };

        #[allow(clippy::cast_sign_loss)]
        if count > 0 { count as usize } else { 0 }
    }

    /// Gets the tab count for this window (if it has tabs).
    ///
    /// This searches for a tab group child and returns its tab count.
    /// Returns 0 if the window has no tabs or 1 if it's a single tab.
    #[must_use]
    pub fn window_tab_count(&self) -> usize { self.find_tab_group().map_or(0, |tg| tg.tab_count()) }

    /// Checks if this window has multiple tabs.
    #[must_use]
    pub fn has_multiple_tabs(&self) -> bool { self.window_tab_count() > 1 }
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
unsafe impl Send for AXElement {}
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
    unsafe { get_size_attr_with_key(element, cf_size()) }
}

/// Gets a size attribute from an element using the specified attribute key.
unsafe fn get_size_attr_with_key(
    element: AXUIElementRef,
    attr_key: *const c_void,
) -> Option<(f64, f64)> {
    if element.is_null() {
        return None;
    }

    let mut value: *mut c_void = ptr::null_mut();
    let result = unsafe { AXUIElementCopyAttributeValue(element, attr_key, &raw mut value) };

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
unsafe fn set_position_attr(element: AXUIElementRef, x: f64, y: f64) -> Result<(), String> {
    if element.is_null() {
        return Err("Null element".to_string());
    }

    let point = core_graphics::geometry::CGPoint::new(x, y);
    let value = unsafe { AXValueCreate(K_AX_VALUE_TYPE_CG_POINT, (&raw const point).cast()) };

    if value.is_null() {
        return Err("Failed to create AXValue for position".to_string());
    }

    let result = unsafe { AXUIElementSetAttributeValue(element, cf_position(), value.cast()) };
    unsafe { CFRelease(value.cast()) };

    ax_result_to_error(result, "set position")
}

/// Sets the size attribute on an element.
unsafe fn set_size_attr(element: AXUIElementRef, width: f64, height: f64) -> Result<(), String> {
    if element.is_null() {
        return Err("Null element".to_string());
    }

    let size = core_graphics::geometry::CGSize::new(width, height);
    let value = unsafe { AXValueCreate(K_AX_VALUE_TYPE_CG_SIZE, (&raw const size).cast()) };

    if value.is_null() {
        return Err("Failed to create AXValue for size".to_string());
    }

    let result = unsafe { AXUIElementSetAttributeValue(element, cf_size(), value.cast()) };
    unsafe { CFRelease(value.cast()) };

    ax_result_to_error(result, "set size")
}

/// Converts an AX error code to a `Result`.
fn ax_result_to_error(result: AXError, operation: &str) -> Result<(), String> {
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
        Err(format!("{operation}: {message} (error {result})"))
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
        let result = AXElement::application(0);
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
    fn test_ax_result_to_error_success() {
        let result = ax_result_to_error(K_AX_ERROR_SUCCESS, "test");
        assert!(result.is_ok());
    }

    #[test]
    fn test_ax_result_to_error_error() {
        let result = ax_result_to_error(K_AX_ERROR_INVALID_UI_ELEMENT, "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_ax_error_constants_are_negative() {
        const { assert!(K_AX_ERROR_INVALID_UI_ELEMENT < 0) };
        const { assert!(K_AX_ERROR_ATTRIBUTE_UNSUPPORTED < 0) };
        const { assert!(K_AX_ERROR_ACTION_UNSUPPORTED < 0) };
        const { assert!(K_AX_ERROR_NOT_IMPLEMENTED < 0) };
        const { assert!(K_AX_ERROR_CANNOT_COMPLETE < 0) };
    }

    #[test]
    fn test_cached_cfstring_thread_local() {
        let _ = cf_windows();
        let _ = cf_title();
        let _ = cf_role();
        let _ = cf_position();
        let _ = cf_size();
    }
}
