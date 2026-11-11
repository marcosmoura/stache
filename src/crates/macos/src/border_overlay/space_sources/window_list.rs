//! The public on-screen window listing used to confirm a target is on the
//! active Space of its display (part 05).
//!
//! `CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly, kCGNullWindowID)`
//! lists the windows that are ordered in on the Spaces currently shown on the
//! displays; a window of an inactive Space, a minimized or a hidden window is
//! absent. Only `kCGWindowNumber` is read, which needs no Screen Recording
//! permission (window names and images would). A target is confirmed when its
//! number is listed.
//!
//! Assumptions, unverified until the native check: that the listing follows each
//! display's own active Space with "Displays have separate Spaces" on, and that
//! a freshly ordered-in window is listed by the time the border shows (the show
//! rule already waits one display frame after the window is visible).
//!
//! The listing is a trait so tests use a fake; the system implementation runs
//! on the actor thread, never on main, and only on show and resume.

use std::collections::BTreeSet;
use std::ffi::c_void;

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_graphics::window::{
    copy_window_info, kCGNullWindowID, kCGWindowListOptionOnScreenOnly, kCGWindowNumber,
};

use crate::windows::WindowInstance;

/// The listing could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowListError;

/// The ids of the windows currently on screen.
pub trait OnScreenWindows: Send {
    /// Replaces `ids` with the on-screen window numbers.
    ///
    /// # Errors
    /// [`WindowListError`] when the system returned no list.
    fn on_screen(&mut self, ids: &mut Vec<u32>) -> Result<(), WindowListError>;
}

/// The public Core Graphics listing.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemOnScreenWindows;

impl OnScreenWindows for SystemOnScreenWindows {
    fn on_screen(&mut self, ids: &mut Vec<u32>) -> Result<(), WindowListError> {
        ids.clear();
        // `copy_window_info` takes ownership of the returned (create-rule) array.
        let array: CFArray = copy_window_info(kCGWindowListOptionOnScreenOnly, kCGNullWindowID)
            .ok_or(WindowListError)?;
        // SAFETY: an immutable `CFString` constant exported by CoreGraphics.
        let key = unsafe { kCGWindowNumber }.cast::<c_void>();
        for item in array.iter() {
            let raw: *const c_void = *item;
            if raw.is_null() {
                continue;
            }
            // SAFETY: the documented element type of the window-info array is
            // `CFDictionary`; the array outlives this loop and the get rule
            // retains the dictionary for the wrapper's lifetime.
            let dictionary: CFDictionary<*const c_void, *const c_void> =
                unsafe { CFDictionary::wrap_under_get_rule(raw as CFDictionaryRef) };
            let Some(value) = dictionary.find(key) else {
                continue;
            };
            let value: *const c_void = *value;
            if value.is_null() {
                continue;
            }
            // SAFETY: CGWindow stores the number as a `CFNumber`; the type is
            // verified by the downcast before it is read.
            let value = unsafe { CFType::wrap_under_get_rule(value.cast()) };
            if let Some(id) = value
                .downcast::<CFNumber>()
                .and_then(|number| number.to_i64())
                .and_then(|number| u32::try_from(number).ok())
            {
                ids.push(id);
            }
        }
        Ok(())
    }
}

/// The subset of `windows` (`(window, cg_window_id)`) whose number is on
/// screen.
///
/// # Errors
/// [`WindowListError`] when the listing failed: nothing is confirmed.
pub fn confirm_on_screen<S: OnScreenWindows + ?Sized>(
    source: &mut S,
    windows: &[(WindowInstance, u32)],
) -> Result<BTreeSet<WindowInstance>, WindowListError> {
    if windows.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut ids = Vec::new();
    source.on_screen(&mut ids)?;
    let listed: BTreeSet<u32> = ids.into_iter().collect();
    Ok(windows
        .iter()
        .filter(|(_, id)| listed.contains(id))
        .map(|(window, _)| *window)
        .collect())
}
