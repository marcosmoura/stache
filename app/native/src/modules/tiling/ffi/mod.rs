//! FFI wrappers for macOS APIs used by the tiling v2 window manager.
//!
//! This module provides safe Rust wrappers around macOS Accessibility API
//! and other system APIs. The goal is to encapsulate unsafe FFI code and
//! provide a safe, ergonomic interface for the rest of the tiling system.
//!
//! # Modules
//!
//! - [`accessibility`] - Safe wrappers for `AXUIElement` and related APIs
//! - [`skylight`] - Safe wrappers for `SkyLight` private framework (screen update batching)
//! - [`transaction`] - RAII wrapper for SkyLight transactions
//! - [`window_query`] - Fast window enumeration using SkyLight APIs
//!
//! # Macros
//!
//! - [`ffi_try!`] - Returns early with an error if a pointer is null
//! - [`ffi_try_opt!`] - Returns `None` if a pointer is null

pub mod accessibility;
pub mod skylight;
pub mod transaction;
pub mod window_query;

pub use accessibility::AXElement;
pub use skylight::{UpdateGuard, get_connection_id, get_window_bounds_fast, get_window_id_from_ax};
pub use transaction::Transaction;
pub use window_query::{WindowInfo, WindowQuery};

/// Returns early with an error if the given pointer is null.
///
/// This macro is useful for FFI code where null pointers indicate errors.
#[macro_export]
macro_rules! ffi_try_v2 {
    ($ptr:expr) => {
        if $ptr.is_null() {
            return Err("Null pointer".to_string());
        }
    };
    ($ptr:expr, $err:expr) => {
        if $ptr.is_null() {
            return Err($err);
        }
    };
}

/// Returns `None` early if the given pointer is null.
#[macro_export]
macro_rules! ffi_try_opt_v2 {
    ($ptr:expr) => {
        if $ptr.is_null() {
            return None;
        }
    };
}

pub use {ffi_try_opt_v2, ffi_try_v2};

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::ptr;

    #[test]
    fn test_ffi_try_opt_with_null() {
        fn check_ptr(p: *const i32) -> Option<i32> {
            ffi_try_opt_v2!(p);
            Some(42)
        }

        let result = check_ptr(ptr::null());
        assert!(result.is_none());
    }

    #[test]
    fn test_ffi_try_opt_with_valid_ptr() {
        fn check_ptr(p: *const i32) -> Option<i32> {
            ffi_try_opt_v2!(p);
            Some(unsafe { *p })
        }

        let value = 42;
        let result = check_ptr(&value);
        assert_eq!(result, Some(42));
    }
}
