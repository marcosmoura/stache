//! Raw `ApplicationServices` declarations added for the window adapters.
//!
//! Declarations match the macOS SDK `HIServices.framework` headers:
//!
//! - `AXUIElement.h` `AXUIElementSetAttributeValue` and
//!   `AXUIElementIsAttributeSettable` (the settable flag is a `Boolean`, one
//!   byte).
//! - `AXValue.h` `AXValueGetTypeID`, `AXValueCreate` (Create rule: the caller
//!   owns the result, `NULL` for invalid input), `AXValueGetType`,
//!   `AXValueGetValue` (returns `Boolean`). `kAXValueTypeCGPoint` is `1` and
//!   `kAXValueTypeCGSize` is `2`.
//! - `AXObserver.h` `AXObserverCreate` (Create rule), `AXObserverAddNotification`,
//!   `AXObserverRemoveNotification` and `AXObserverGetRunLoopSource` (Get rule:
//!   the source is borrowed from the observer).
//!
//! Safe wrappers with their `SAFETY` proofs live next to their callers.

use std::ffi::c_void;

use core_foundation::base::CFTypeRef;
use core_foundation::string::CFStringRef;

use super::api::{AxUiElementRef, RawAxError};

/// `AXValueRef`, a `CFTypeRef` of type `AXValue`.
pub(super) type AxValueRef = *const c_void;
/// `AXObserverRef`.
pub(super) type AxObserverRef = *mut c_void;
/// `CFRunLoopSourceRef` as a raw borrowed pointer.
pub(super) type RawRunLoopSource = *mut c_void;

/// `AXObserverCallback`.
pub(super) type AxObserverCallback = unsafe extern "C" fn(
    observer: AxObserverRef,
    element: AxUiElementRef,
    notification: CFStringRef,
    refcon: *mut c_void,
);

/// `kAXValueTypeCGPoint`.
pub(super) const VALUE_TYPE_CG_POINT: u32 = 1;
/// `kAXValueTypeCGSize`.
pub(super) const VALUE_TYPE_CG_SIZE: u32 = 2;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    pub(super) fn AXUIElementSetAttributeValue(
        element: AxUiElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> RawAxError;
    pub(super) fn AXUIElementIsAttributeSettable(
        element: AxUiElementRef,
        attribute: CFStringRef,
        settable: *mut u8,
    ) -> RawAxError;
    pub(super) fn AXValueGetTypeID() -> usize;
    pub(super) fn AXValueCreate(value_type: u32, value: *const c_void) -> AxValueRef;
    pub(super) fn AXValueGetType(value: AxValueRef) -> u32;
    pub(super) fn AXValueGetValue(value: AxValueRef, value_type: u32, out: *mut c_void) -> u8;
    pub(super) fn AXObserverCreate(
        application: i32,
        callback: AxObserverCallback,
        out: *mut AxObserverRef,
    ) -> RawAxError;
    pub(super) fn AXObserverAddNotification(
        observer: AxObserverRef,
        element: AxUiElementRef,
        notification: CFStringRef,
        refcon: *mut c_void,
    ) -> RawAxError;
    pub(super) fn AXObserverRemoveNotification(
        observer: AxObserverRef,
        element: AxUiElementRef,
        notification: CFStringRef,
    ) -> RawAxError;
    pub(super) fn AXObserverGetRunLoopSource(observer: AxObserverRef) -> RawRunLoopSource;
}
