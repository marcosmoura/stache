//! Safe wrappers for macOS `SkyLight` private framework APIs.
//!
#![allow(clippy::doc_markdown)] // Allow SkyLight, CGRect, etc. without backticks
//!
//! This module provides safe Rust wrappers around the `SkyLight` private framework,
//! which offers low-level window server operations. These APIs enable performance
//! optimizations that are not possible with the public Accessibility API.
//!
//! # Screen Update Batching
//!
//! The primary feature is screen update batching via `SLSDisableUpdate`/`SLSReenableUpdate`.
//! This prevents the window server from refreshing the screen during batch operations,
//! reducing visual tearing and improving frame timing consistency.
//!
//! ```rust,ignore
//! use stache::modules::tiling::ffi::skylight::UpdateGuard;
//!
//! // Updates are batched while the guard is held
//! let _guard = UpdateGuard::new();
//! // ... move multiple windows ...
//! // Screen refreshes when guard is dropped
//! ```
//!
//! # Safety
//!
//! These APIs are private and undocumented. While they work without SIP disabled,
//! they may change between macOS versions. All unsafe code is encapsulated in this
//! module with safe wrappers.
//!
//! # Thread Safety
//!
//! The SkyLight APIs are thread-safe. The connection ID is cached and can be
//! accessed from any thread. `UpdateGuard` is `Send` but not `Sync` (should
//! be created and dropped on the same thread as the window operations).

use std::ffi::c_void;
use std::sync::OnceLock;

use crate::modules::tiling::state::Rect;

// ============================================================================
// FFI Type Definitions
// ============================================================================

/// CGRect structure for window bounds.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

/// CGPoint structure for origin coordinates.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

/// CGSize structure for width and height.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

impl CGRect {
    /// Converts this CGRect to our internal Rect type.
    #[must_use]
    pub const fn to_rect(self) -> Rect {
        Rect {
            x: self.origin.x,
            y: self.origin.y,
            width: self.size.width,
            height: self.size.height,
        }
    }
}

// ============================================================================
// FFI Declarations
// ============================================================================

#[link(name = "SkyLight", kind = "framework")]
unsafe extern "C" {
    /// Returns the main connection ID to the window server.
    fn SLSMainConnectionID() -> u32;

    /// Disables screen updates for the given connection.
    fn SLSDisableUpdate(cid: u32) -> i32;

    /// Re-enables screen updates for the given connection.
    fn SLSReenableUpdate(cid: u32) -> i32;

    /// Gets the bounds of a window from the window server.
    fn CGSGetWindowBounds(cid: u32, wid: u32, bounds: *mut CGRect) -> i32;

    /// Moves a window to a new position (faster than AX API).
    ///
    /// This is significantly faster than `AXUIElementSetAttributeValue` for position:
    /// - `SLSMoveWindow`: ~0.1-0.3ms
    /// - AX position: ~2-5ms
    fn SLSMoveWindow(cid: u32, wid: u32, point: *const CGPoint) -> i32;
}

// Private Accessibility API to get the window server ID from an AXUIElement.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn _AXUIElementGetWindow(element: *mut c_void, wid: *mut u32) -> i32;
}

// ============================================================================
// Connection ID Cache
// ============================================================================

/// Cached connection ID to the window server.
static CONNECTION_ID: OnceLock<u32> = OnceLock::new();

/// Returns the cached window server connection ID.
///
/// The connection ID is obtained once and cached for the lifetime of the process.
/// Returns 0 if the connection could not be established.
#[inline]
#[must_use]
pub fn get_connection_id() -> u32 {
    *CONNECTION_ID.get_or_init(|| {
        let cid = unsafe { SLSMainConnectionID() };
        if cid == 0 {
            tracing::warn!("tiling: skylight: failed to get connection ID");
        }
        cid
    })
}

// ============================================================================
// Screen Update Control
// ============================================================================

/// Disables screen updates for batch window operations.
///
/// Returns `true` if updates were successfully disabled.
#[inline]
#[must_use]
pub fn disable_update() -> bool {
    let cid = get_connection_id();
    if cid == 0 {
        return false;
    }
    let result = unsafe { SLSDisableUpdate(cid) };
    result == 0
}

/// Re-enables screen updates after batch window operations.
///
/// Returns `true` if updates were successfully re-enabled.
#[inline]
#[must_use]
pub fn reenable_update() -> bool {
    let cid = get_connection_id();
    if cid == 0 {
        return false;
    }
    let result = unsafe { SLSReenableUpdate(cid) };
    result == 0
}

// ============================================================================
// Fast Window Bounds Query
// ============================================================================

/// Gets window bounds directly from the window server.
///
/// This is significantly faster than querying via the Accessibility API:
/// - CGS: ~0.1-0.5ms per call
/// - AX: ~2-8ms per call
#[must_use]
pub fn get_window_bounds_fast(window_id: u32) -> Option<Rect> {
    let cid = get_connection_id();
    if cid == 0 {
        return None;
    }

    let mut bounds = CGRect::default();
    let result = unsafe { CGSGetWindowBounds(cid, window_id, &raw mut bounds) };

    if result == 0 {
        Some(bounds.to_rect())
    } else {
        None
    }
}

/// Moves a window to a new position using the SkyLight API.
///
/// This is significantly faster than using the Accessibility API:
/// - `SLSMoveWindow`: ~0.1-0.3ms
/// - AX position: ~2-5ms
///
/// Returns `true` if the move was successful.
///
/// Note: This only moves the window, it does not resize it.
/// For resize operations, the Accessibility API must still be used.
#[must_use]
pub fn move_window_fast(window_id: u32, x: f64, y: f64) -> bool {
    let cid = get_connection_id();
    if cid == 0 {
        return false;
    }

    let point = CGPoint { x, y };
    let result = unsafe { SLSMoveWindow(cid, window_id, &raw const point) };
    result == 0
}

// ============================================================================
// AXUIElement to CGWindowID Mapping
// ============================================================================

/// Gets the CGWindowID from an AXUIElement reference.
#[must_use]
#[allow(clippy::not_unsafe_ptr_arg_deref)] // Null-checked before use
pub fn get_window_id_from_ax(ax_element: *mut c_void) -> Option<u32> {
    if ax_element.is_null() {
        return None;
    }

    let mut window_id: u32 = 0;
    let result = unsafe { _AXUIElementGetWindow(ax_element, &raw mut window_id) };

    // kAXErrorSuccess = 0
    if result == 0 && window_id != 0 {
        Some(window_id)
    } else {
        None
    }
}

// ============================================================================
// RAII Update Guard
// ============================================================================

/// RAII guard that disables screen updates while held.
///
/// Screen updates are disabled when the guard is created and automatically
/// re-enabled when the guard is dropped.
#[derive(Debug)]
pub struct UpdateGuard {
    /// Whether updates were successfully disabled.
    disabled: bool,
}

impl UpdateGuard {
    /// Creates a new update guard, disabling screen updates.
    #[must_use]
    pub fn new() -> Option<Self> {
        if disable_update() {
            Some(Self { disabled: true })
        } else {
            None
        }
    }

    /// Creates a guard that conditionally disables updates.
    #[must_use]
    pub fn new_if(enabled: bool) -> Option<Self> { if enabled { Self::new() } else { None } }
}

impl Drop for UpdateGuard {
    fn drop(&mut self) {
        if self.disabled {
            let _ = reenable_update();
        }
    }
}

// UpdateGuard is Send because SkyLight APIs are thread-safe
unsafe impl Send for UpdateGuard {}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::ptr;

    use super::*;

    #[test]
    fn connection_id_is_cached() {
        let cid1 = get_connection_id();
        let cid2 = get_connection_id();
        assert_eq!(cid1, cid2);
    }

    #[test]
    fn update_guard_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<UpdateGuard>();
    }

    #[test]
    fn update_guard_new_if_disabled_returns_none() {
        let guard = UpdateGuard::new_if(false);
        assert!(guard.is_none());
    }

    #[test]
    fn update_guard_drop_is_safe_when_disabled_false() {
        let guard = UpdateGuard { disabled: false };
        drop(guard);
    }

    #[test]
    fn cg_rect_to_rect_conversion() {
        let cg_rect = CGRect {
            origin: CGPoint { x: 10.0, y: 20.0 },
            size: CGSize { width: 100.0, height: 200.0 },
        };
        let rect = cg_rect.to_rect();

        assert!((rect.x - 10.0).abs() < f64::EPSILON);
        assert!((rect.y - 20.0).abs() < f64::EPSILON);
        assert!((rect.width - 100.0).abs() < f64::EPSILON);
        assert!((rect.height - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn get_window_id_from_ax_with_null_returns_none() {
        let result = get_window_id_from_ax(ptr::null_mut());
        assert!(result.is_none());
    }

    #[test]
    fn get_window_bounds_fast_with_invalid_id_returns_none() {
        let result = get_window_bounds_fast(0);
        assert!(result.is_none());

        let result = get_window_bounds_fast(u32::MAX);
        assert!(result.is_none());
    }

    #[test]
    fn move_window_fast_with_invalid_id_returns_false() {
        // Invalid window ID should fail gracefully
        let result = move_window_fast(0, 100.0, 100.0);
        assert!(!result);

        let result = move_window_fast(u32::MAX, 100.0, 100.0);
        assert!(!result);
    }
}
