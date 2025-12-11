//! macOS Accessibility API bindings.
//!
//! This module provides safe Rust wrappers around the macOS Accessibility API
//! for window manipulation and observation.

use std::ffi::c_void;
use std::ptr;

use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::display::CGPoint;
use core_graphics::geometry::CGSize;

use super::error::TilingError;
use super::state::WindowFrame;

/// Result type for accessibility operations.
pub type AXResult<T> = Result<T, TilingError>;

// Foreign function declarations for Accessibility API
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> i32;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> i32;
    fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> i32;
    fn AXIsProcessTrusted() -> bool;
    fn AXValueCreate(value_type: u32, value: *const c_void) -> CFTypeRef;
    fn AXValueGetValue(value: CFTypeRef, value_type: u32, value_out: *mut c_void) -> bool;
    fn CFRelease(cf: CFTypeRef);
}

// Opaque types
type AXUIElementRef = *mut c_void;
type CFTypeRef = *mut c_void;

// AXValue types
const AX_VALUE_TYPE_CGPOINT: u32 = 1;
const AX_VALUE_TYPE_CGSIZE: u32 = 2;

// AX error codes
const AX_ERROR_SUCCESS: i32 = 0;
const AX_ERROR_FAILURE: i32 = -25200;
const AX_ERROR_ILLEGAL_ARGUMENT: i32 = -25201;
const AX_ERROR_INVALID_UIELEMENT: i32 = -25202;
const AX_ERROR_INVALID_UIELEMENT_OBSERVER: i32 = -25203;
const AX_ERROR_CANNOT_COMPLETE: i32 = -25204;
const AX_ERROR_ATTRIBUTE_UNSUPPORTED: i32 = -25205;
const AX_ERROR_ACTION_UNSUPPORTED: i32 = -25206;
const AX_ERROR_NOTIFICATION_UNSUPPORTED: i32 = -25207;
const AX_ERROR_NOT_IMPLEMENTED: i32 = -25208;
const AX_ERROR_NOTIFICATION_ALREADY_REGISTERED: i32 = -25209;
const AX_ERROR_NOTIFICATION_NOT_REGISTERED: i32 = -25210;
const AX_ERROR_API_DISABLED: i32 = -25211;
const AX_ERROR_NO_VALUE: i32 = -25212;

/// Attribute names for accessibility elements.
pub mod attributes {
    pub const WINDOWS: &str = "AXWindows";
    pub const FOCUSED_WINDOW: &str = "AXFocusedWindow";
    pub const SUBROLE: &str = "AXSubrole";
    pub const POSITION: &str = "AXPosition";
    pub const SIZE: &str = "AXSize";
    pub const MAIN: &str = "AXMain";
    pub const TITLE: &str = "AXTitle";
    /// Application hidden state (like Cmd+H).
    pub const HIDDEN: &str = "AXHidden";
    /// System-wide focused application.
    pub const FOCUSED_APPLICATION: &str = "AXFocusedApplication";
}

/// Action names for accessibility elements.
pub mod actions {
    pub const RAISE: &str = "AXRaise";
}

/// Cached `CFString` constants for common AX attributes and actions.
///
/// These are lazily initialized on first use and reused thereafter,
/// avoiding repeated allocations in hot paths.
///
/// # Thread Safety
///
/// Core Foundation strings are immutable after creation and thread-safe.
/// We use a wrapper type to safely store the raw `CFStringRef` in statics.
pub mod cf_strings {
    use std::sync::LazyLock;

    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};

    use super::{actions, attributes};

    /// A thread-safe wrapper around `CFStringRef`.
    ///
    /// # Safety
    ///
    /// This is safe because:
    /// 1. `CFString` is immutable after creation
    /// 2. Core Foundation objects are reference-counted and thread-safe
    /// 3. We never mutate the string after initialization
    pub struct SyncCFString(CFStringRef);

    // SAFETY: CFString is immutable and Core Foundation types are thread-safe
    unsafe impl Send for SyncCFString {}
    unsafe impl Sync for SyncCFString {}

    impl SyncCFString {
        /// Creates a new `SyncCFString` from a string literal.
        fn new(s: &str) -> Self {
            let cf = CFString::new(s);
            // Retain the string so it lives forever in the static
            let ptr = cf.as_concrete_TypeRef();
            std::mem::forget(cf); // Prevent drop from releasing
            Self(ptr)
        }

        /// Returns the underlying `CFStringRef` for use with AX APIs.
        #[inline]
        pub const fn as_ref(&self) -> CFStringRef { self.0 }
    }

    // Attribute CFStrings
    pub static AX_WINDOWS: LazyLock<SyncCFString> =
        LazyLock::new(|| SyncCFString::new(attributes::WINDOWS));
    pub static AX_FOCUSED_WINDOW: LazyLock<SyncCFString> =
        LazyLock::new(|| SyncCFString::new(attributes::FOCUSED_WINDOW));
    pub static AX_SUBROLE: LazyLock<SyncCFString> =
        LazyLock::new(|| SyncCFString::new(attributes::SUBROLE));
    pub static AX_POSITION: LazyLock<SyncCFString> =
        LazyLock::new(|| SyncCFString::new(attributes::POSITION));
    pub static AX_SIZE: LazyLock<SyncCFString> =
        LazyLock::new(|| SyncCFString::new(attributes::SIZE));
    pub static AX_MAIN: LazyLock<SyncCFString> =
        LazyLock::new(|| SyncCFString::new(attributes::MAIN));
    pub static AX_TITLE: LazyLock<SyncCFString> =
        LazyLock::new(|| SyncCFString::new(attributes::TITLE));
    pub static AX_HIDDEN: LazyLock<SyncCFString> =
        LazyLock::new(|| SyncCFString::new(attributes::HIDDEN));
    pub static AX_FOCUSED_APPLICATION: LazyLock<SyncCFString> =
        LazyLock::new(|| SyncCFString::new(attributes::FOCUSED_APPLICATION));

    // Action CFStrings
    pub static AX_RAISE: LazyLock<SyncCFString> =
        LazyLock::new(|| SyncCFString::new(actions::RAISE));
}

/// Converts an AX error code to a `TilingError`.
fn ax_error_to_result(code: i32) -> AXResult<()> {
    match code {
        AX_ERROR_SUCCESS => Ok(()),
        AX_ERROR_API_DISABLED => Err(TilingError::AccessibilityNotAuthorized),
        AX_ERROR_INVALID_UIELEMENT | AX_ERROR_INVALID_UIELEMENT_OBSERVER => {
            Err(TilingError::WindowNotFound(0))
        }
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE => Err(TilingError::OperationFailed(
            "Attribute not supported".to_string(),
        )),
        AX_ERROR_ACTION_UNSUPPORTED => {
            Err(TilingError::OperationFailed("Action not supported".to_string()))
        }
        AX_ERROR_CANNOT_COMPLETE => Err(TilingError::OperationFailed(
            "Cannot complete operation".to_string(),
        )),
        AX_ERROR_FAILURE => Err(TilingError::OperationFailed("General AX failure".to_string())),
        AX_ERROR_ILLEGAL_ARGUMENT => {
            Err(TilingError::OperationFailed("Illegal argument".to_string()))
        }
        AX_ERROR_NOTIFICATION_UNSUPPORTED
        | AX_ERROR_NOT_IMPLEMENTED
        | AX_ERROR_NOTIFICATION_ALREADY_REGISTERED
        | AX_ERROR_NOTIFICATION_NOT_REGISTERED => {
            Err(TilingError::OperationFailed("Notification error".to_string()))
        }
        _ => Err(TilingError::OperationFailed(format!("Unknown AX error: {code}"))),
    }
}

/// Checks if the application has accessibility permissions.
#[must_use]
pub fn is_accessibility_enabled() -> bool { unsafe { AXIsProcessTrusted() } }

/// A wrapper around an `AXUIElement`.
#[derive(Debug)]
pub struct AccessibilityElement {
    element: AXUIElementRef,
}

impl Drop for AccessibilityElement {
    fn drop(&mut self) {
        if !self.element.is_null() {
            unsafe { CFRelease(self.element.cast()) };
        }
    }
}

// SAFETY: AXUIElementRef is thread-safe according to Apple documentation.
// The Accessibility API can be called from any thread.
unsafe impl Send for AccessibilityElement {}
unsafe impl Sync for AccessibilityElement {}

impl AccessibilityElement {
    /// Creates a system-wide accessibility element.
    #[must_use]
    pub fn system_wide() -> Self {
        Self {
            element: unsafe { AXUIElementCreateSystemWide() },
        }
    }

    /// Creates an accessibility element for an application.
    #[must_use]
    pub fn application(pid: i32) -> Self {
        Self {
            element: unsafe { AXUIElementCreateApplication(pid) },
        }
    }

    /// Creates from a raw element reference (takes ownership).
    ///
    /// # Safety
    /// The caller must ensure that `element` is a valid `AXUIElementRef`.
    #[must_use]
    pub const unsafe fn from_raw(element: AXUIElementRef) -> Self { Self { element } }

    /// Returns the raw `AXUIElementRef` pointer.
    ///
    /// This is useful for caching the element. The returned pointer is
    /// only valid as long as this `AccessibilityElement` is alive.
    #[must_use]
    pub const fn as_raw(&self) -> AXUIElementRef { self.element }

    /// Gets the process ID of this element's application.
    pub fn pid(&self) -> AXResult<i32> {
        let mut pid: i32 = 0;
        let result = unsafe { AXUIElementGetPid(self.element, &raw mut pid) };
        ax_error_to_result(result)?;
        Ok(pid)
    }

    /// Gets a string attribute value.
    pub fn get_string_attribute(&self, attribute: &str) -> AXResult<String> {
        let attr_cf = CFString::new(attribute);
        let mut value: CFTypeRef = ptr::null_mut();

        let result = unsafe {
            AXUIElementCopyAttributeValue(
                self.element,
                attr_cf.as_concrete_TypeRef(),
                &raw mut value,
            )
        };

        ax_error_to_result(result)?;

        if value.is_null() {
            return Err(TilingError::OperationFailed("Null value returned".to_string()));
        }

        // SAFETY: We checked that value is not null and the API guarantees it's a CFString
        let cf_string: CFString = unsafe { CFString::wrap_under_get_rule(value.cast()) };
        Ok(cf_string.to_string())
    }

    /// Gets a string attribute value using a pre-cached `CFStringRef`.
    ///
    /// This is an optimization for hot paths where the attribute string is known at compile time.
    fn get_string_attribute_cached(&self, attr_ref: CFStringRef) -> AXResult<String> {
        let mut value: CFTypeRef = ptr::null_mut();

        let result =
            unsafe { AXUIElementCopyAttributeValue(self.element, attr_ref, &raw mut value) };

        ax_error_to_result(result)?;

        if value.is_null() {
            return Err(TilingError::OperationFailed("Null value returned".to_string()));
        }

        let cf_string: CFString = unsafe { CFString::wrap_under_get_rule(value.cast()) };
        Ok(cf_string.to_string())
    }

    /// Gets the position of this element.
    pub fn get_position(&self) -> AXResult<(f64, f64)> {
        let mut value: CFTypeRef = ptr::null_mut();

        let result = unsafe {
            AXUIElementCopyAttributeValue(
                self.element,
                cf_strings::AX_POSITION.as_ref(),
                &raw mut value,
            )
        };

        ax_error_to_result(result)?;

        if value.is_null() {
            return Err(TilingError::OperationFailed("Null value returned".to_string()));
        }

        let mut point = CGPoint::new(0.0, 0.0);
        let success = unsafe {
            AXValueGetValue(value, AX_VALUE_TYPE_CGPOINT, ptr::from_mut(&mut point).cast())
        };

        unsafe { CFRelease(value) };

        if success {
            Ok((point.x, point.y))
        } else {
            Err(TilingError::OperationFailed(
                "Failed to extract position".to_string(),
            ))
        }
    }

    /// Gets the size of this element.
    pub fn get_size(&self) -> AXResult<(f64, f64)> {
        let mut value: CFTypeRef = ptr::null_mut();

        let result = unsafe {
            AXUIElementCopyAttributeValue(
                self.element,
                cf_strings::AX_SIZE.as_ref(),
                &raw mut value,
            )
        };

        ax_error_to_result(result)?;

        if value.is_null() {
            return Err(TilingError::OperationFailed("Null value returned".to_string()));
        }

        let mut size = CGSize::new(0.0, 0.0);
        let success = unsafe {
            AXValueGetValue(value, AX_VALUE_TYPE_CGSIZE, ptr::from_mut(&mut size).cast())
        };

        unsafe { CFRelease(value) };

        if success {
            Ok((size.width, size.height))
        } else {
            Err(TilingError::OperationFailed(
                "Failed to extract size".to_string(),
            ))
        }
    }

    /// Gets the frame (position and size) of this element.
    pub fn get_frame(&self) -> AXResult<WindowFrame> {
        let (x, y) = self.get_position()?;
        let (width, height) = self.get_size()?;

        // Convert to i32/u32, clamping to valid ranges
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok(WindowFrame {
            x: x as i32,
            y: y as i32,
            width: width.max(0.0) as u32,
            height: height.max(0.0) as u32,
        })
    }

    /// Sets the position of this element.
    pub fn set_position(&self, x: f64, y: f64) -> AXResult<()> {
        let point = CGPoint::new(x, y);
        let value = unsafe { AXValueCreate(AX_VALUE_TYPE_CGPOINT, ptr::from_ref(&point).cast()) };

        if value.is_null() {
            return Err(TilingError::OperationFailed(
                "Failed to create position value".to_string(),
            ));
        }

        let result = unsafe {
            AXUIElementSetAttributeValue(self.element, cf_strings::AX_POSITION.as_ref(), value)
        };

        unsafe { CFRelease(value) };

        ax_error_to_result(result)
    }

    /// Sets the size of this element.
    pub fn set_size(&self, width: f64, height: f64) -> AXResult<()> {
        let size = CGSize::new(width, height);
        let value = unsafe { AXValueCreate(AX_VALUE_TYPE_CGSIZE, ptr::from_ref(&size).cast()) };

        if value.is_null() {
            return Err(TilingError::OperationFailed(
                "Failed to create size value".to_string(),
            ));
        }

        let result = unsafe {
            AXUIElementSetAttributeValue(self.element, cf_strings::AX_SIZE.as_ref(), value)
        };

        unsafe { CFRelease(value) };

        ax_error_to_result(result)
    }

    /// Sets the frame (position and size) of this element.
    ///
    /// This method is optimized to minimize redundant AX calls:
    /// - Skips entirely if already at target frame
    /// - Only sets position if position changed
    /// - Only sets size if size changed
    /// - Re-applies size after position only when needed (for constrained windows)
    pub fn set_frame(&self, frame: &WindowFrame) -> AXResult<()> {
        use crate::tiling::window::{STRICT_FRAME_TOLERANCE, positions_match, sizes_match};

        // Get current frame to determine what needs to change
        let current = self.get_frame()?;

        let position_matches = positions_match(&current, frame, STRICT_FRAME_TOLERANCE);
        let size_matches = sizes_match(&current, frame, STRICT_FRAME_TOLERANCE);

        // Already at target - nothing to do
        if position_matches && size_matches {
            return Ok(());
        }

        // Only position changed
        if size_matches {
            return self.set_position(f64::from(frame.x), f64::from(frame.y));
        }

        // Only size changed
        if position_matches {
            return self.set_size(f64::from(frame.width), f64::from(frame.height));
        }

        // Both changed - use the full sequence:
        // 1. Set size first (reducing size ensures window can fit at target position)
        // 2. Set position
        // 3. Re-apply size (in case window was constrained during move)
        self.set_size(f64::from(frame.width), f64::from(frame.height))?;
        self.set_position(f64::from(frame.x), f64::from(frame.y))?;

        // Only re-apply size if the window might have been constrained
        // (i.e., we're moving to a different position that might have different constraints)
        if let Some(ref after) = self.get_frame().ok()
            && !sizes_match(after, frame, STRICT_FRAME_TOLERANCE)
        {
            self.set_size(f64::from(frame.width), f64::from(frame.height))?;
        }

        Ok(())
    }

    /// Gets a boolean attribute value.
    ///
    /// Returns `false` if the attribute is not set or cannot be read.
    pub fn get_bool_attribute(&self, attribute: &str) -> AXResult<bool> {
        let attr_cf = CFString::new(attribute);
        let mut value: CFTypeRef = ptr::null_mut();

        let result = unsafe {
            AXUIElementCopyAttributeValue(
                self.element,
                attr_cf.as_concrete_TypeRef(),
                &raw mut value,
            )
        };

        ax_error_to_result(result)?;

        if value.is_null() {
            return Ok(false);
        }

        // Extract boolean value from CFBoolean
        let cf_bool = unsafe { CFBoolean::wrap_under_get_rule(value.cast()) };
        Ok(cf_bool.into())
    }

    /// Sets a boolean attribute value.
    ///
    /// For frequently-used attributes, prefer the specialized methods like `set_hidden()`
    /// which use cached `CFString` constants for better performance.
    #[allow(dead_code)]
    pub fn set_bool_attribute(&self, attribute: &str, value: bool) -> AXResult<()> {
        let attr_cf = CFString::new(attribute);
        let cf_bool = if value {
            CFBoolean::true_value()
        } else {
            CFBoolean::false_value()
        };

        let result = unsafe {
            AXUIElementSetAttributeValue(
                self.element,
                attr_cf.as_concrete_TypeRef(),
                cf_bool.as_concrete_TypeRef().cast::<c_void>().cast_mut(),
            )
        };

        ax_error_to_result(result)
    }

    /// Sets a boolean attribute using a pre-cached `CFStringRef`.
    ///
    /// This is an optimization for hot paths where the attribute string is known at compile time.
    pub fn set_bool_attribute_cached(&self, attr_ref: CFStringRef, value: bool) -> AXResult<()> {
        let cf_bool = if value {
            CFBoolean::true_value()
        } else {
            CFBoolean::false_value()
        };

        let result = unsafe {
            AXUIElementSetAttributeValue(
                self.element,
                attr_ref,
                cf_bool.as_concrete_TypeRef().cast::<c_void>().cast_mut(),
            )
        };

        ax_error_to_result(result)
    }

    /// Sets the hidden state of this application element.
    ///
    /// Equivalent to pressing Cmd+H to hide an app.
    pub fn set_hidden(&self, hidden: bool) -> AXResult<()> {
        self.set_bool_attribute_cached(cf_strings::AX_HIDDEN.as_ref(), hidden)
    }

    /// Performs an action on this element.
    pub fn perform_action(&self, action: &str) -> AXResult<()> {
        let action_cf = CFString::new(action);
        let result =
            unsafe { AXUIElementPerformAction(self.element, action_cf.as_concrete_TypeRef()) };
        ax_error_to_result(result)
    }

    /// Raises this window to the front.
    pub fn raise(&self) -> AXResult<()> {
        let result =
            unsafe { AXUIElementPerformAction(self.element, cf_strings::AX_RAISE.as_ref()) };
        ax_error_to_result(result)
    }

    /// Focuses this window.
    pub fn focus(&self) -> AXResult<()> {
        self.set_bool_attribute_cached(cf_strings::AX_MAIN.as_ref(), true)?;
        self.raise()
    }

    /// Gets an element attribute (like focused application).
    pub fn get_element_attribute(&self, attribute: &str) -> AXResult<Self> {
        let attr_cf = CFString::new(attribute);
        let mut value: CFTypeRef = ptr::null_mut();

        let result = unsafe {
            AXUIElementCopyAttributeValue(
                self.element,
                attr_cf.as_concrete_TypeRef(),
                &raw mut value,
            )
        };

        ax_error_to_result(result)?;

        if value.is_null() {
            return Err(TilingError::OperationFailed("Null element returned".to_string()));
        }

        // SAFETY: We take ownership of the returned element
        Ok(unsafe { Self::from_raw(value) })
    }

    /// Gets an element attribute using a pre-cached `CFStringRef`.
    ///
    /// This is an optimization for hot paths where the attribute string is known at compile time.
    pub fn get_element_attribute_cached(&self, attr_ref: CFStringRef) -> AXResult<Self> {
        let mut value: CFTypeRef = ptr::null_mut();

        let result =
            unsafe { AXUIElementCopyAttributeValue(self.element, attr_ref, &raw mut value) };

        ax_error_to_result(result)?;

        if value.is_null() {
            return Err(TilingError::OperationFailed("Null element returned".to_string()));
        }

        Ok(unsafe { Self::from_raw(value) })
    }

    /// Gets the focused application element from the system-wide element.
    ///
    /// This should be called on a system-wide element.
    pub fn get_focused_application(&self) -> AXResult<Self> {
        self.get_element_attribute_cached(cf_strings::AX_FOCUSED_APPLICATION.as_ref())
    }

    /// Gets the focused window of an application element.
    pub fn get_focused_window(&self) -> AXResult<Self> {
        self.get_element_attribute_cached(cf_strings::AX_FOCUSED_WINDOW.as_ref())
            .map_err(|_| TilingError::WindowNotFound(0))
    }

    /// Gets all windows of an application element.
    pub fn get_windows(&self) -> AXResult<Vec<Self>> {
        let mut value: CFTypeRef = ptr::null_mut();

        let result = unsafe {
            AXUIElementCopyAttributeValue(
                self.element,
                cf_strings::AX_WINDOWS.as_ref(),
                &raw mut value,
            )
        };

        ax_error_to_result(result)?;

        if value.is_null() {
            return Ok(Vec::new());
        }

        // The value is a CFArray of AXUIElements
        // We need to iterate through it and create AccessibilityElement for each
        let array: core_foundation::array::CFArray<CFType> =
            unsafe { core_foundation::array::CFArray::wrap_under_get_rule(value.cast()) };

        #[allow(clippy::cast_sign_loss)]
        let mut windows = Vec::with_capacity(array.len() as usize);

        for i in 0..array.len() {
            if let Some(elem) = array.get(i) {
                // Retain the element since we're taking ownership
                let raw: *mut c_void = elem.as_concrete_TypeRef().cast::<c_void>().cast_mut();
                unsafe { core_foundation::base::CFRetain(raw.cast()) };
                windows.push(unsafe { Self::from_raw(raw) });
            }
        }

        Ok(windows)
    }

    /// Gets the subrole of this element (e.g., `AXDialog`, `AXFloatingWindow`, `AXSheet`).
    pub fn get_subrole(&self) -> Option<String> {
        self.get_string_attribute_cached(cf_strings::AX_SUBROLE.as_ref()).ok()
    }

    /// Gets the title of this element.
    pub fn get_title(&self) -> Option<String> {
        self.get_string_attribute_cached(cf_strings::AX_TITLE.as_ref()).ok()
    }

    /// Checks if this element is a dialog, sheet, or other non-tileable window type.
    ///
    /// Returns `true` for:
    /// - Dialogs (`AXDialog`)
    /// - Sheets (`AXSheet`) - slide-down panels attached to windows
    /// - System dialogs (`AXSystemDialog`)
    /// - Floating windows (`AXFloatingWindow`) - palettes, inspectors
    #[must_use]
    pub fn is_dialog_or_sheet(&self) -> bool {
        // Window subroles that should NOT be tiled
        const NON_TILEABLE_SUBROLES: &[&str] =
            &["AXDialog", "AXSheet", "AXSystemDialog", "AXFloatingWindow"];

        self.get_subrole()
            .is_some_and(|subrole| NON_TILEABLE_SUBROLES.iter().any(|&s| subrole == s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cf_strings_are_valid() {
        // Test that static CFStrings can be accessed without panicking
        // This verifies the lazy initialization works correctly
        let position_ref = cf_strings::AX_POSITION.as_ref();
        assert!(!position_ref.is_null());

        let size_ref = cf_strings::AX_SIZE.as_ref();
        assert!(!size_ref.is_null());

        let windows_ref = cf_strings::AX_WINDOWS.as_ref();
        assert!(!windows_ref.is_null());

        let raise_ref = cf_strings::AX_RAISE.as_ref();
        assert!(!raise_ref.is_null());
    }

    #[test]
    fn test_cf_strings_are_consistent() {
        // Test that multiple accesses return the same pointer (proving caching works)
        let first = cf_strings::AX_POSITION.as_ref();
        let second = cf_strings::AX_POSITION.as_ref();
        assert_eq!(
            first, second,
            "CFString should return same pointer on multiple accesses"
        );
    }

    #[test]
    fn test_all_attribute_cfstrings_initialized() {
        // Verify all attribute CFStrings can be initialized
        assert!(!cf_strings::AX_WINDOWS.as_ref().is_null());
        assert!(!cf_strings::AX_FOCUSED_WINDOW.as_ref().is_null());
        assert!(!cf_strings::AX_SUBROLE.as_ref().is_null());
        assert!(!cf_strings::AX_POSITION.as_ref().is_null());
        assert!(!cf_strings::AX_SIZE.as_ref().is_null());
        assert!(!cf_strings::AX_MAIN.as_ref().is_null());
        assert!(!cf_strings::AX_TITLE.as_ref().is_null());
        assert!(!cf_strings::AX_HIDDEN.as_ref().is_null());
        assert!(!cf_strings::AX_FOCUSED_APPLICATION.as_ref().is_null());
    }

    #[test]
    fn test_action_cfstrings_initialized() {
        // Verify action CFStrings can be initialized
        assert!(!cf_strings::AX_RAISE.as_ref().is_null());
    }
}
