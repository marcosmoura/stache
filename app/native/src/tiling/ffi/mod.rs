//! FFI wrappers for macOS APIs used by the tiling window manager.
//!
//! This module provides safe Rust wrappers around macOS Accessibility API
//! and other system APIs. The goal is to encapsulate unsafe FFI code and
//! provide a safe, ergonomic interface for the rest of the tiling system.
//!
//! # Modules
//!
//! - [`accessibility`] - Safe wrappers for `AXUIElement` and related APIs
//!
//! # Macros
//!
//! - [`ffi_try!`] - Returns early with an error if a pointer is null
//! - [`ffi_try_opt!`] - Returns `None` if a pointer is null
//!
//! These macros are used in [`super::window`] to reduce null-check boilerplate
//! in functions like `get_ax_string()`, `get_ax_position()`, etc.

pub mod accessibility;

pub use accessibility::AXElement;

/// Returns early with an error if the given pointer is null.
///
/// This macro is useful for FFI code where null pointers indicate errors.
/// It reduces boilerplate when checking multiple pointer values.
///
/// # Examples
///
/// ```rust,ignore
/// use stache::tiling::ffi::ffi_try;
/// use stache::tiling::error::TilingError;
///
/// fn get_window_title(ptr: *mut c_void) -> Result<String, TilingError> {
///     ffi_try!(ptr, TilingError::window_op("Null window pointer"));
///     // ... continue with non-null ptr
/// }
/// ```
///
/// # Forms
///
/// - `ffi_try!(ptr)` - Returns `Err(TilingError::window_op("Null pointer"))`
/// - `ffi_try!(ptr, error)` - Returns `Err(error)` if null
#[macro_export]
macro_rules! ffi_try {
    ($ptr:expr) => {
        if $ptr.is_null() {
            return Err($crate::tiling::error::TilingError::window_op("Null pointer"));
        }
    };
    ($ptr:expr, $err:expr) => {
        if $ptr.is_null() {
            return Err($err);
        }
    };
}

/// Returns `None` early if the given pointer is null.
///
/// This macro is useful for FFI code in functions that return `Option<T>`.
/// It provides a concise way to handle null pointer checks.
///
/// # Examples
///
/// ```rust,ignore
/// use stache::tiling::ffi::ffi_try_opt;
///
/// fn get_optional_value(ptr: *mut c_void) -> Option<i32> {
///     ffi_try_opt!(ptr);
///     // ... continue with non-null ptr
///     Some(42)
/// }
/// ```
#[macro_export]
macro_rules! ffi_try_opt {
    ($ptr:expr) => {
        if $ptr.is_null() {
            return None;
        }
    };
}

// Re-export macros at the module level
pub use {ffi_try, ffi_try_opt};

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::ptr;

    use super::*;
    use crate::tiling::error::TilingError;

    #[test]
    fn test_ffi_try_with_null() {
        fn check_ptr(p: *const i32) -> Result<i32, TilingError> {
            ffi_try!(p, TilingError::window_op("test error"));
            Ok(42)
        }

        let result = check_ptr(ptr::null());
        assert!(result.is_err());
    }

    #[test]
    fn test_ffi_try_with_valid_ptr() {
        fn check_ptr(p: *const i32) -> Result<i32, TilingError> {
            ffi_try!(p, TilingError::window_op("test error"));
            Ok(unsafe { *p })
        }

        let value = 42;
        let result = check_ptr(&value);
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_ffi_try_default_error() {
        fn check_ptr(p: *const i32) -> Result<i32, TilingError> {
            ffi_try!(p);
            Ok(42)
        }

        let result = check_ptr(ptr::null());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Null pointer"));
    }

    #[test]
    fn test_ffi_try_opt_with_null() {
        fn check_ptr(p: *const i32) -> Option<i32> {
            ffi_try_opt!(p);
            Some(42)
        }

        let result = check_ptr(ptr::null());
        assert!(result.is_none());
    }

    #[test]
    fn test_ffi_try_opt_with_valid_ptr() {
        fn check_ptr(p: *const i32) -> Option<i32> {
            ffi_try_opt!(p);
            Some(unsafe { *p })
        }

        let value = 42;
        let result = check_ptr(&value);
        assert_eq!(result, Some(42));
    }
}
