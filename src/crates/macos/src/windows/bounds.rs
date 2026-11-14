//! A window's frame from the window server, by `CGWindowID` (private API).
//!
//! Tiling uses it for two questions that only need a frame, and nothing else
//! may: where a window is while a Stache frame write for it is in flight (the
//! echo of its own animation), and where a retargeted animation starts. It
//! never decides a write outcome, a minimum size or a fullscreen state; the
//! Accessibility reads stay the source for those, and they run as before
//! whenever this reader answers nothing.
//!
//! # Inventory (private API)
//!
//! - Symbols (exported by the `SkyLight` image, not declared in a public SDK
//!   header): the shared `SLSMainConnectionID` (resolved by
//!   `border_overlay::private_skylight`) and `SLSGetWindowBounds`, the same
//!   symbol borders' live follow reads (`border_overlay::live_follow::skylight`).
//!   Both are resolved only by runtime `dlsym` lookup through that shared
//!   resolver; nothing is linked or declared a second way.
//! - Observed on 2026-11-13 (Apple M1 Pro, macOS 27.3, a probe outside
//!   Stache): 67 µs per call, equal to the public bounds for 8 of 8 on-screen
//!   windows.
//! - Missing public equivalent: `CGWindowListCopyWindowInfo` for one window
//!   (about 650 µs, allocating) or an Accessibility read on the application's
//!   own thread, which is the round trip this reader avoids.
//! - Minimum need: one read per in-flight echo and per retarget.
//!
//! # Signatures: assumptions
//!
//! Same as live follow, written from the author's recollection of the C
//! shapes:
//!
//! - `int SLSMainConnectionID(void)`;
//! - `CGError SLSGetWindowBounds(int connection, uint32_t window, CGRect
//!   *out)`, global top-left points (the Quartz convention of `AXPosition`).
//!
//! A missing symbol, a non-zero result or an empty rectangle answers nothing.

use std::ffi::{CStr, c_int, c_void};
use std::sync::OnceLock;

use crate::ax::{AxFrame, AxPoint, AxSize};
use crate::border_overlay::live_follow::skylight::{BoundsFn, CgRect, SYM_BOUNDS};
use crate::border_overlay::private_skylight::{SkyLightSymbols, system_lookup};

/// Reads a window's frame from the window server.
pub trait WindowBounds: Send + Sync {
    /// Whether the symbols resolved. `false` means every read answers nothing.
    fn available(&self) -> bool;

    /// The window's frame in global top-left points; `None` when a symbol is
    /// missing, the call fails or the rectangle is empty.
    fn bounds(&self, cg_window_id: u32) -> Option<AxFrame>;
}

/// The system reader. Resolving the symbols calls nothing; the connection is
/// fetched on the first read.
pub struct SystemWindowBounds {
    base: SkyLightSymbols,
    bounds: Option<BoundsFn>,
    connection: OnceLock<Option<c_int>>,
}

impl SystemWindowBounds {
    /// Resolves the symbols on the `SkyLight` image.
    #[must_use]
    pub fn system() -> Self {
        // SAFETY: the lookup returns only the `dlsym` address of the exact
        // named symbol, or null.
        unsafe { Self::from_lookup(system_lookup) }
    }

    /// Resolves through an injected lookup (tests inject missing symbols).
    ///
    /// # Safety
    ///
    /// A non-null address returned for a name must be the address of a function
    /// with the signature documented for that name in the module documentation.
    pub(crate) unsafe fn from_lookup(lookup: impl Fn(&CStr) -> *mut c_void) -> Self {
        let bounds = lookup(SYM_BOUNDS);
        Self {
            // SAFETY: the same contract as this function.
            base: unsafe { SkyLightSymbols::from_lookup(&lookup) },
            // SAFETY: see the function contract; data and function pointers
            // have the same size on every supported target.
            bounds: (!bounds.is_null())
                .then(|| unsafe { std::mem::transmute::<*mut c_void, BoundsFn>(bounds) }),
            connection: OnceLock::new(),
        }
    }

    fn connection(&self) -> Option<c_int> {
        *self.connection.get_or_init(|| self.base.connection())
    }
}

impl WindowBounds for SystemWindowBounds {
    fn available(&self) -> bool { self.bounds.is_some() && self.base.has_main_connection() }

    fn bounds(&self, cg_window_id: u32) -> Option<AxFrame> {
        let bounds = self.bounds?;
        let connection = self.connection()?;
        let mut out = CgRect::default();
        // SAFETY: resolved symbol with the documented shape; `out` is a valid
        // out pointer for one rectangle.
        let code = unsafe { bounds(connection, cg_window_id, &raw mut out) };
        (code == 0 && out.width > 0.0 && out.height > 0.0).then_some(AxFrame {
            origin: AxPoint { x: out.x, y: out.y },
            size: AxSize {
                width: out.width,
                height: out.height,
            },
        })
    }
}

/// A reader with no symbols: every read answers nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct MissingWindowBounds;

impl WindowBounds for MissingWindowBounds {
    fn available(&self) -> bool { false }

    fn bounds(&self, _cg_window_id: u32) -> Option<AxFrame> { None }
}

/// A scripted reader for tests: answers from a closure and counts reads.
#[cfg(any(test, feature = "testing"))]
pub struct FakeWindowBounds {
    available: bool,
    answer: Box<dyn Fn(u32) -> Option<AxFrame> + Send + Sync>,
    reads: std::sync::atomic::AtomicUsize,
}

#[cfg(any(test, feature = "testing"))]
impl FakeWindowBounds {
    /// Answers each read with `answer`.
    #[must_use]
    pub fn new(answer: impl Fn(u32) -> Option<AxFrame> + Send + Sync + 'static) -> Self {
        Self {
            available: true,
            answer: Box::new(answer),
            reads: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The symbols are missing: unavailable, every read answers nothing.
    #[must_use]
    pub fn missing() -> Self {
        Self {
            available: false,
            ..Self::new(|_| None)
        }
    }

    /// How many reads were made.
    #[must_use]
    pub fn reads(&self) -> usize { self.reads.load(std::sync::atomic::Ordering::SeqCst) }
}

#[cfg(any(test, feature = "testing"))]
impl WindowBounds for FakeWindowBounds {
    fn available(&self) -> bool { self.available }

    fn bounds(&self, cg_window_id: u32) -> Option<AxFrame> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if !self.available {
            return None;
        }
        (self.answer)(cg_window_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn main_connection() -> c_int { 7 }

    /// Window 1: a frame; 2: refused; 3: an empty rectangle.
    unsafe extern "C" fn get_bounds(connection: c_int, window: u32, out: *mut CgRect) -> c_int {
        assert_eq!(connection, 7);
        let rect = match window {
            1 => CgRect {
                x: 10.0,
                y: 20.0,
                width: 300.0,
                height: 200.0,
            },
            2 => return 1001,
            _ => CgRect::default(),
        };
        // SAFETY: the reader passes a valid out pointer.
        unsafe { out.write(rect) };
        0
    }

    fn lookup(name: &CStr) -> *mut c_void {
        if name == c"SLSMainConnectionID" {
            main_connection as *mut c_void
        } else if name == SYM_BOUNDS {
            get_bounds as *mut c_void
        } else {
            std::ptr::null_mut()
        }
    }

    #[test]
    fn present_symbols_answer_the_window_server_frame() {
        // SAFETY: the injected addresses have the documented shapes.
        let reader = unsafe { SystemWindowBounds::from_lookup(lookup) };
        assert!(reader.available());
        assert_eq!(
            reader.bounds(1),
            Some(AxFrame {
                origin: AxPoint { x: 10.0, y: 20.0 },
                size: AxSize { width: 300.0, height: 200.0 },
            })
        );
    }

    #[test]
    fn a_refused_call_or_an_empty_rectangle_answers_nothing() {
        // SAFETY: the injected addresses have the documented shapes.
        let reader = unsafe { SystemWindowBounds::from_lookup(lookup) };
        assert_eq!(reader.bounds(2), None);
        assert_eq!(reader.bounds(3), None);
    }

    #[test]
    fn a_missing_symbol_answers_nothing() {
        // SAFETY: null for every name.
        let none = unsafe { SystemWindowBounds::from_lookup(|_| std::ptr::null_mut()) };
        assert!(!none.available());
        assert_eq!(none.bounds(1), None);
        // Only the bounds call is missing.
        // SAFETY: the injected addresses have the documented shapes.
        let no_bounds = unsafe {
            SystemWindowBounds::from_lookup(|name| {
                if name == SYM_BOUNDS {
                    std::ptr::null_mut()
                } else {
                    lookup(name)
                }
            })
        };
        assert!(!no_bounds.available());
        assert_eq!(no_bounds.bounds(1), None);
        // Only the connection is missing.
        // SAFETY: the injected addresses have the documented shapes.
        let no_connection = unsafe {
            SystemWindowBounds::from_lookup(|name| {
                if name == SYM_BOUNDS {
                    lookup(name)
                } else {
                    std::ptr::null_mut()
                }
            })
        };
        assert!(!no_connection.available());
        assert_eq!(no_connection.bounds(1), None);
        assert!(!MissingWindowBounds.available());
    }
}
