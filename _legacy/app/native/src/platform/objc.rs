//! Objective-C helper utilities for macOS integration.
//!
//! Provides common utilities for working with Objective-C objects,
//! particularly `NSString` conversions and application introspection.

use std::ffi::c_void;

use objc::runtime::Object;
use objc::{class, msg_send, sel, sel_impl};

/// Creates an `NSString` from a Rust string slice.
///
/// # Safety
///
/// This function is unsafe because it calls Objective-C methods via FFI.
/// The caller must ensure that the Objective-C runtime is properly initialized.
///
/// # Returns
///
/// A pointer to an `NSString` object. The returned object is autoreleased.
#[must_use]
pub unsafe fn nsstring(s: &str) -> *mut Object {
    let nsstring_class = class!(NSString);
    let bytes = s.as_ptr().cast::<c_void>();
    let len = s.len();
    let encoding: usize = 4; // NSUTF8StringEncoding

    msg_send![
        nsstring_class,
        stringWithBytes: bytes
        length: len
        encoding: encoding
    ]
}

/// Converts an `NSString` to a Rust `String`.
///
/// # Safety
///
/// This function is unsafe because it calls Objective-C methods via FFI.
/// The caller must ensure that `nsstring` is either null or a valid `NSString` pointer.
///
/// # Returns
///
/// A Rust `String` containing the UTF-8 representation of the `NSString`.
/// Returns an empty string if the input is null or conversion fails.
#[must_use]
pub unsafe fn nsstring_to_string(nsstring: *mut Object) -> String {
    if nsstring.is_null() {
        return String::new();
    }

    let c_str: *const i8 = msg_send![nsstring, UTF8String];
    if c_str.is_null() {
        return String::new();
    }

    // SAFETY: c_str is verified non-null above, and UTF8String returns a valid C string
    unsafe { std::ffi::CStr::from_ptr(c_str) }.to_string_lossy().into_owned()
}

/// Gets the bundle identifier of an `NSRunningApplication`.
///
/// # Safety
///
/// This function is unsafe because it calls Objective-C methods via FFI.
/// The caller must ensure that `app` is a valid `NSRunningApplication` pointer.
///
/// # Returns
///
/// The bundle identifier as a `String`, or `None` if the app is null or has no bundle ID.
#[must_use]
pub unsafe fn get_app_bundle_id(app: *mut Object) -> Option<String> {
    if app.is_null() {
        return None;
    }

    let bundle_id: *mut Object = msg_send![app, bundleIdentifier];
    if bundle_id.is_null() {
        return None;
    }

    // SAFETY: bundle_id is verified non-null above
    let bundle_str = unsafe { nsstring_to_string(bundle_id) };
    if bundle_str.is_empty() {
        None
    } else {
        Some(bundle_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nsstring_roundtrip() {
        unsafe {
            let original = "Hello, World!";
            let ns = nsstring(original);
            let result = nsstring_to_string(ns);
            assert_eq!(result, original);
        }
    }

    #[test]
    fn test_nsstring_to_string_null() {
        unsafe {
            let result = nsstring_to_string(std::ptr::null_mut());
            assert!(result.is_empty());
        }
    }

    #[test]
    fn test_nsstring_empty() {
        unsafe {
            let ns = nsstring("");
            let result = nsstring_to_string(ns);
            assert!(result.is_empty());
        }
    }

    #[test]
    fn test_nsstring_unicode() {
        unsafe {
            let original = "こんにちは 🎵";
            let ns = nsstring(original);
            let result = nsstring_to_string(ns);
            assert_eq!(result, original);
        }
    }

    #[test]
    fn test_get_app_bundle_id_null() {
        unsafe {
            let result = get_app_bundle_id(std::ptr::null_mut());
            assert!(result.is_none());
        }
    }
}
