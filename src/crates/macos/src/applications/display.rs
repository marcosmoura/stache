//! Best-effort display hint for an application, from public geometry only.
//!
//! The hint comes from `CGWindowListCopyWindowInfo` restricted to on-screen,
//! layer-0 windows of the owning process and reads only the owner PID, layer
//! and bounds. It never reads window titles, never needs Screen Recording
//! permission for those fields and uses no Accessibility API. Bounds are in the
//! CoreGraphics global space (origin top-left of the primary display, y down);
//! `NSScreen.frame` is in the `AppKit` global space (origin bottom-left of the
//! primary display, y up). [`appkit_rect_from_cg`] normalizes between them in
//! pure code and [`select_display`] picks the screen with the largest
//! intersection.
//!
//! The PID is only a nondestructive placement hint: it selects windows to
//! measure and never addresses a termination. Missing or filtered geometry is an
//! explicit `None` with a [`DisplaySource`] explaining why, never a guessed
//! display, so the HUD keeps its own fallback and can report it.
//!
//! # Limitations
//!
//! Only windows on the active Space are listed, so an application whose windows
//! are all on another Space (or minimized, or full-screen on a different Space)
//! reports [`DisplaySource::NoWindow`]. The front-most layer-0 window may not be
//! the one the user is looking at on a multi-window layout. Both are accepted:
//! the hint is advisory.

use std::ffi::c_void;

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_graphics::window::{
    copy_window_info, kCGNullWindowID, kCGWindowBounds, kCGWindowLayer,
    kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly, kCGWindowOwnerPID,
};

use super::frontmost::{FrontmostPreflight, preflight_on_main};
use super::{ApplicationError, ApplicationId, ApplicationObservation};
use crate::desktop::DisplayId;

/// An axis-aligned rectangle in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayRect {
    /// Left edge.
    pub x: f64,
    /// Origin edge on the vertical axis (space dependent).
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl DisplayRect {
    /// Creates a rectangle.
    #[must_use]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { x, y, width, height }
    }

    /// Whether every component is finite and the area is positive.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width > 0.0
            && self.height > 0.0
    }

    /// The area of the intersection with `other`; `0.0` when disjoint.
    #[must_use]
    pub fn intersection_area(&self, other: &Self) -> f64 {
        let left = self.x.max(other.x);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = self.y.max(other.y);
        let top = (self.y + self.height).min(other.y + other.height);
        let (width, height) = (right - left, top - bottom);
        if width > 0.0 && height > 0.0 {
            width * height
        } else {
            0.0
        }
    }
}

/// One window as `CGWindowListCopyWindowInfo` reported it, front to back.
///
/// Every field is optional because a filtered or malformed dictionary may omit
/// it; the selection treats a missing field as "unusable", never as a default.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowRecord {
    /// `kCGWindowOwnerPID`.
    pub pid: Option<i32>,
    /// `kCGWindowLayer`.
    pub layer: Option<i32>,
    /// `kCGWindowBounds`, in the `CoreGraphics` global space (top-left origin).
    pub bounds: Option<DisplayRect>,
}

/// One attached screen: its identity and its `AppKit` global frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenRecord {
    /// `NSScreen` display identity (the `NSScreenNumber` value).
    pub id: DisplayId,
    /// `NSScreen.frame`, `AppKit` global space (bottom-left origin).
    pub frame: DisplayRect,
}

/// Why a hint is present or absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplaySource {
    /// The display was chosen by largest intersection with the front-most
    /// on-screen layer-0 window of the process.
    WindowGeometry,
    /// The window list was unavailable (`CGWindowListCopyWindowInfo` returned
    /// null).
    ListUnavailable,
    /// No on-screen layer-0 window belongs to the process.
    NoWindow,
    /// Windows exist but none exposes usable bounds (filtered or malformed).
    GeometryFiltered,
    /// The window bounds intersect no attached screen (a detached display).
    NoIntersection,
    /// No screens are attached.
    NoScreens,
    /// The exact-identity/current-frontmost validation did not pass, so no
    /// geometry was read.
    NotValidated,
}

/// A best-effort display hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayHint {
    /// The chosen display; `None` means the caller keeps its own fallback.
    pub display: Option<DisplayId>,
    /// Why.
    pub source: DisplaySource,
}

impl DisplayHint {
    /// A hint without a display.
    #[must_use]
    pub const fn none(source: DisplaySource) -> Self { Self { display: None, source } }

    /// The display to pass to the HUD request (`HudRequest::with_display`), when
    /// geometry produced one.
    #[must_use]
    pub const fn display(&self) -> Option<DisplayId> { self.display }
}

/// Converts a `CoreGraphics` global rectangle (top-left origin, y down) into the
/// `AppKit` global space (bottom-left origin, y up).
///
/// `primary_height` is the height of the primary screen (`NSScreen.screens[0]`),
/// whose `AppKit` frame has origin `(0, 0)` and whose top-left is the
/// `CoreGraphics` origin. Pure.
#[must_use]
pub fn appkit_rect_from_cg(rect: DisplayRect, primary_height: f64) -> DisplayRect {
    DisplayRect {
        x: rect.x,
        y: primary_height - (rect.y + rect.height),
        width: rect.width,
        height: rect.height,
    }
}

/// Chooses the display for `pid` from a window list and the attached screens.
///
/// `windows` is the list front to back, or `None` when the list was
/// unavailable. The first on-screen layer-0 window of `pid` with usable bounds
/// is used; the screen with the largest positive intersection wins and the
/// earlier screen wins a tie. `screens[0]` must be the primary screen. Pure.
#[must_use]
pub fn select_display(
    windows: Option<&[WindowRecord]>,
    pid: i32,
    screens: &[ScreenRecord],
) -> DisplayHint {
    let Some(primary) = screens.first() else {
        return DisplayHint::none(DisplaySource::NoScreens);
    };
    let Some(windows) = windows else {
        return DisplayHint::none(DisplaySource::ListUnavailable);
    };
    let mut saw_layer_zero = false;
    let mut chosen = None;
    for window in windows {
        if window.pid != Some(pid) || window.layer != Some(0) {
            continue;
        }
        saw_layer_zero = true;
        if let Some(bounds) = window.bounds.filter(DisplayRect::is_usable) {
            chosen = Some(bounds);
            break;
        }
    }
    let Some(bounds) = chosen else {
        return DisplayHint::none(if saw_layer_zero {
            DisplaySource::GeometryFiltered
        } else {
            DisplaySource::NoWindow
        });
    };
    let bounds = appkit_rect_from_cg(bounds, primary.frame.height);
    let mut best: Option<(DisplayId, f64)> = None;
    for screen in screens {
        let area = bounds.intersection_area(&screen.frame);
        if area > 0.0 && best.is_none_or(|(_, current)| area > current) {
            best = Some((screen.id, area));
        }
    }
    best.map_or_else(
        || DisplayHint::none(DisplaySource::NoIntersection),
        |(id, _)| DisplayHint {
            display: Some(id),
            source: DisplaySource::WindowGeometry,
        },
    )
}

// ---------------------------------------------------------------------------
// Native collection (public CoreGraphics API only).
// ---------------------------------------------------------------------------

/// Reads the on-screen window list, front to back.
///
/// Only the owner PID, layer and bounds are read. `None` means the system
/// returned no list.
#[must_use]
pub(super) fn read_window_records() -> Option<Vec<WindowRecord>> {
    let options = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements;
    // `copy_window_info` takes ownership of the returned (create-rule) array.
    let array: CFArray = copy_window_info(options, kCGNullWindowID)?;
    let mut records = Vec::new();
    for item in array.iter() {
        let raw: *const c_void = *item;
        if raw.is_null() {
            continue;
        }
        // SAFETY: the documented element type of the window-info array is
        // `CFDictionary`; the array outlives this loop and the get rule retains
        // the dictionary for the wrapper's lifetime.
        let dictionary: CFDictionary<*const c_void, *const c_void> =
            unsafe { CFDictionary::wrap_under_get_rule(raw as CFDictionaryRef) };
        records.push(window_record(&dictionary));
    }
    Some(records)
}

fn window_record(dictionary: &CFDictionary<*const c_void, *const c_void>) -> WindowRecord {
    // SAFETY: these are immutable `CFString` constants exported by
    // CoreGraphics.
    let (pid_key, layer_key, bounds_key) = unsafe {
        (
            kCGWindowOwnerPID.cast::<c_void>(),
            kCGWindowLayer.cast::<c_void>(),
            kCGWindowBounds.cast::<c_void>(),
        )
    };
    let pid = number(dictionary, pid_key).and_then(|value| i32::try_from(value).ok());
    let layer = number(dictionary, layer_key).and_then(|value| i32::try_from(value).ok());
    let bounds = dictionary.find(bounds_key).and_then(|value| {
        let raw: *const c_void = *value;
        if raw.is_null() {
            return None;
        }
        // SAFETY: `kCGWindowBounds` maps to a `CFDictionary` per the
        // CGWindow documentation; the get rule retains it for the wrapper.
        let inner: CFDictionary<*const c_void, *const c_void> =
            unsafe { CFDictionary::wrap_under_get_rule(raw as CFDictionaryRef) };
        let coordinate = |name: &'static str| {
            let key = CFString::from_static_string(name);
            float(&inner, key.as_concrete_TypeRef().cast::<c_void>())
        };
        Some(DisplayRect::new(
            coordinate("X")?,
            coordinate("Y")?,
            coordinate("Width")?,
            coordinate("Height")?,
        ))
    });
    WindowRecord { pid, layer, bounds }
}

fn number(
    dictionary: &CFDictionary<*const c_void, *const c_void>,
    key: *const c_void,
) -> Option<i64> {
    let value = dictionary.find(key)?;
    let raw: *const c_void = *value;
    if raw.is_null() {
        return None;
    }
    // SAFETY: CGWindow stores these values as `CFNumber`; type is verified
    // before reading.
    let value = unsafe { CFType::wrap_under_get_rule(raw.cast()) };
    value.downcast::<CFNumber>().and_then(|number| number.to_i64())
}

fn float(
    dictionary: &CFDictionary<*const c_void, *const c_void>,
    key: *const c_void,
) -> Option<f64> {
    let value = dictionary.find(key)?;
    let raw: *const c_void = *value;
    if raw.is_null() {
        return None;
    }
    // SAFETY: the bounds dictionary stores `CFNumber` values; type is verified
    // before reading.
    let value = unsafe { CFType::wrap_under_get_rule(raw.cast()) };
    value.downcast::<CFNumber>().and_then(|number| number.to_f64())
}

impl ApplicationObservation {
    /// A best-effort display hint for the exact application instance.
    ///
    /// On main it first revalidates exact identity and the current frontmost
    /// application (running, frontmost, not Finder, not this process), then reads
    /// public window geometry. A failed validation is
    /// [`DisplaySource::NotValidated`] with no display and no geometry read.
    /// Missing or filtered geometry is an explicit `None` plus the reason; the
    /// caller keeps its own placement fallback and can report it.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotInstalled`] for a stopped generation, a
    /// dispatch rejection, a query failure or a timeout.
    pub fn display_for(&self, application: ApplicationId) -> Result<DisplayHint, ApplicationError> {
        if !self.owns(application) {
            return Ok(DisplayHint::none(DisplaySource::NotValidated));
        }
        self.query(
            move |backend, handle| match preflight_on_main(backend, handle, application)? {
                FrontmostPreflight::Eligible(_) => backend.display_hint(handle, application),
                FrontmostPreflight::NotRunning | FrontmostPreflight::Rejected(_) => {
                    Ok(DisplayHint::none(DisplaySource::NotValidated))
                }
            },
        )
    }
}
