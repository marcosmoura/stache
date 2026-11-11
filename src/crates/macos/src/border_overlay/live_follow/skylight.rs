//! The private `SkyLight` window notifications, subscription and bounds read,
//! resolved only by runtime symbol lookup (decision 4; same policy as
//! `ax/window_id.rs` and part 05's Space notification).
//!
//! The connection resolver and the register/remove helper are shared with part
//! 05 (`private_skylight`); this file adds the three window specifics.
//!
//! # Inventory (private API)
//!
//! - Symbols (all exported by the `SkyLight` image, none declared in a public
//!   SDK header): the shared `SLSMainConnectionID`,
//!   `SLSRegisterConnectionNotifyProc`, `SLSRemoveConnectionNotifyProc`, plus
//!   `SLSRequestNotificationsForWindows` (per-window subscription) and
//!   `SLSGetWindowBounds` (bounds of any window by `CGWindowID`). **Whether the
//!   two new symbols resolve on macOS 26 is not yet observed** (the part 05
//!   lookup covered only the shared three); a missing one makes live follow
//!   unavailable by name.
//! - Missing public equivalent: no public API reports another process's window
//!   moves, and `CGWindowListCopyWindowInfo` (public) is a polled, allocating
//!   list that is too costly per display frame; the Accessibility observers
//!   Tiling already has are coalesced per display period and arrive after the
//!   fact, which is the lag this part fixes.
//! - Minimum need: notifications for move, resize, reorder, hide, unhide and
//!   close of the bordered windows, and one bounds read per window per tick.
//!
//! # Signatures: assumptions (none observed)
//!
//! Written from the author's recollection of the C shapes, independently of any
//! border tool or yabai header:
//!
//! - `CGError SLSRequestNotificationsForWindows(int connection, const uint32_t
//!   *windows, int count)`; unverified whether a call replaces or adds to the
//!   subscription. The full current set is passed every time, so both behave
//!   the same; entries for windows that are no longer bordered are dropped by
//!   the slots (`EventSlots::watch`).
//! - `CGError SLSGetWindowBounds(int connection, uint32_t window, CGRect *out)`,
//!   global top-left points (the Quartz convention Tiling's frames use).
//! - The notify callback shape is the shared one; **the window id is assumed to
//!   be the first four bytes of `data`** when `length >= 4`. A wrong layout
//!   yields ids that match no watched window: events are dropped (counted as
//!   unwatched), nothing moves wrongly, and the Tiling fallback hides the border
//!   while the window moves.
//!
//! # Event ids: assumptions
//!
//! The constants below are **candidates, unverified until the part 06 native
//! check**: neither the numbers, nor whether `Hide`/`Unhide` mean "ordered
//! out/in" or "application hidden/unhidden", nor the thread the callback runs
//! on are known.

use std::ffi::{c_int, c_void};
use std::sync::{Arc, Mutex, PoisonError};

use super::{EventSlots, LiveUnavailable, WindowEventKind};
use crate::border_overlay::OverlayRect;
use crate::border_overlay::private_skylight::{SkyLightSymbols, system_lookup};

/// Candidate event id: a window moved. **Unverified until the part 06 native check.**
pub const EVENT_WINDOW_MOVE: u32 = 806;
/// Candidate event id: a window was resized. **Unverified until the part 06 native check.**
pub const EVENT_WINDOW_RESIZE: u32 = 807;
/// Candidate event id: a window was re-ordered. **Unverified until the part 06 native check.**
pub const EVENT_WINDOW_REORDER: u32 = 808;
/// Candidate event id: a window was hidden. **Unverified until the part 06 native check.**
pub const EVENT_WINDOW_HIDE: u32 = 815;
/// Candidate event id: a window was unhidden. **Unverified until the part 06 native check.**
pub const EVENT_WINDOW_UNHIDE: u32 = 816;
/// Candidate event id: a window was destroyed. **Unverified until the part 06 native check.**
pub const EVENT_WINDOW_CLOSE: u32 = 804;

/// The registered events and the kind each one reports.
pub const WINDOW_EVENTS: [(u32, WindowEventKind); 6] = [
    (EVENT_WINDOW_MOVE, WindowEventKind::Move),
    (EVENT_WINDOW_RESIZE, WindowEventKind::Resize),
    (EVENT_WINDOW_REORDER, WindowEventKind::Reorder),
    (EVENT_WINDOW_HIDE, WindowEventKind::Hide),
    (EVENT_WINDOW_UNHIDE, WindowEventKind::Unhide),
    (EVENT_WINDOW_CLOSE, WindowEventKind::Close),
];

const SYM_REQUEST: &std::ffi::CStr = c"SLSRequestNotificationsForWindows";
const SYM_BOUNDS: &std::ffi::CStr = c"SLSGetWindowBounds";

type RequestFn = unsafe extern "C" fn(c_int, *const u32, c_int) -> c_int;
type BoundsFn = unsafe extern "C" fn(c_int, u32, *mut CgRect) -> c_int;

/// `CGRect` as the window server writes it (`CGFloat` is `f64` on 64-bit).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CgRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// The kind a registered event id reports.
#[must_use]
pub fn kind_of_event(event: u32) -> Option<WindowEventKind> {
    WINDOW_EVENTS.iter().find(|(id, _)| *id == event).map(|(_, kind)| *kind)
}

/// The window id assumed to start the payload; `None` for a null or short
/// payload. See the module documentation (unverified layout).
///
/// # Safety
///
/// A non-null `data` must point to at least `length` readable bytes.
#[must_use]
pub const unsafe fn window_id_from_payload(data: *const c_void, length: usize) -> Option<u32> {
    if data.is_null() || length < std::mem::size_of::<u32>() {
        return None;
    }
    // SAFETY: at least four readable bytes (function contract); unaligned read.
    Some(unsafe { data.cast::<u32>().read_unaligned() })
}

/// The resolved (or missing) window symbols.
#[derive(Clone, Copy, Debug)]
pub struct WindowSymbols {
    base: SkyLightSymbols,
    request: Option<RequestFn>,
    bounds: Option<BoundsFn>,
}

impl WindowSymbols {
    /// Resolves the symbols on the `SkyLight` image. Registers nothing.
    #[must_use]
    pub fn system() -> Self {
        // SAFETY: the lookup returns only the `dlsym` address of the exact
        // named symbol, or null.
        unsafe { Self::from_lookup(system_lookup) }
    }

    /// Every symbol missing.
    #[must_use]
    pub const fn missing() -> Self {
        Self {
            base: SkyLightSymbols::missing(),
            request: None,
            bounds: None,
        }
    }

    /// Resolves through an injected lookup (tests inject missing symbols).
    ///
    /// # Safety
    ///
    /// A non-null address returned for a name must be the address of a function
    /// with the signature documented for that name.
    pub(crate) unsafe fn from_lookup(lookup: impl Fn(&std::ffi::CStr) -> *mut c_void) -> Self {
        let request = lookup(SYM_REQUEST);
        let bounds = lookup(SYM_BOUNDS);
        // SAFETY: see the function contract; data and function pointers have
        // the same size on every supported target.
        unsafe {
            Self {
                base: SkyLightSymbols::from_lookup(&lookup),
                request: (!request.is_null())
                    .then(|| std::mem::transmute::<*mut c_void, RequestFn>(request)),
                bounds: (!bounds.is_null())
                    .then(|| std::mem::transmute::<*mut c_void, BoundsFn>(bounds)),
            }
        }
    }

    /// The name of the first missing symbol.
    #[must_use]
    pub const fn missing_symbol(&self) -> Option<&'static str> {
        if let Some(name) = self.base.missing_symbol() {
            Some(name)
        } else if self.request.is_none() {
            Some("SLSRequestNotificationsForWindows")
        } else if self.bounds.is_none() {
            Some("SLSGetWindowBounds")
        } else {
            None
        }
    }
}

/// The slots the callback records into; empty after `unregister`. A process
/// slot, because the private callback receives no context we read.
static ACTIVE_SLOTS: Mutex<Option<Arc<EventSlots>>> = Mutex::new(None);

/// The notify proc. Copies `(event, window id)` into the slots: allocates
/// nothing, reads no AX state, and holds the slot lock only to clone the `Arc`.
unsafe extern "C" fn window_event(
    event: u32,
    data: *mut c_void,
    length: usize,
    _context: *mut c_void,
    _connection: c_int,
) {
    let _ = std::panic::catch_unwind(|| {
        let Some(kind) = kind_of_event(event) else { return };
        // SAFETY: the window server passes a payload of `length` bytes (or null).
        let Some(id) = (unsafe { window_id_from_payload(data.cast_const(), length) }) else {
            return;
        };
        let slots = ACTIVE_SLOTS.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(slots) = slots {
            slots.record(kind, id);
        }
    });
}

/// The system registration, subscription and bounds read.
pub struct SkyLightWindowNotify {
    symbols: WindowSymbols,
    connection: Option<c_int>,
    registered: Vec<u32>,
}

impl SkyLightWindowNotify {
    /// Resolves the symbols; registers nothing.
    #[must_use]
    pub fn system() -> Self { Self::with_symbols(WindowSymbols::system()) }

    /// Over injected symbols.
    #[must_use]
    pub const fn with_symbols(symbols: WindowSymbols) -> Self {
        Self {
            symbols,
            connection: None,
            registered: Vec::new(),
        }
    }

    /// Registers every window event. Total: a missing symbol or any refusal is
    /// an error and leaves nothing registered.
    ///
    /// # Errors
    ///
    /// Why live follow is unavailable.
    pub fn register(&mut self, slots: Arc<EventSlots>) -> Result<(), LiveUnavailable> {
        if self.connection.is_some() {
            return Ok(());
        }
        if let Some(name) = self.symbols.missing_symbol() {
            return Err(LiveUnavailable::missing_symbol(name));
        }
        let Some(connection) = self.symbols.base.connection() else {
            return Err(LiveUnavailable::missing_symbol("SLSMainConnectionID"));
        };
        *ACTIVE_SLOTS.lock().unwrap_or_else(PoisonError::into_inner) = Some(slots);
        for (event, _) in WINDOW_EVENTS {
            let code = self.symbols.base.register_proc(connection, window_event, event);
            if code != Some(0) {
                self.connection = Some(connection);
                self.unregister();
                return Err(LiveUnavailable::new(format!(
                    "registering window event {event} failed (code {})",
                    code.map_or_else(|| "none".to_owned(), |code| code.to_string())
                )));
            }
            self.registered.push(event);
        }
        self.connection = Some(connection);
        Ok(())
    }

    /// Subscribes to the notifications of `ids` (the full current set).
    ///
    /// # Errors
    ///
    /// The subscription was refused or is not registered.
    pub fn subscribe(&self, ids: &[u32]) -> Result<(), LiveUnavailable> {
        let (Some(connection), Some(request)) = (self.connection, self.symbols.request) else {
            return Err(LiveUnavailable::new("window notifications are not registered"));
        };
        let count = c_int::try_from(ids.len()).unwrap_or(c_int::MAX);
        // SAFETY: resolved symbol with the documented shape; `ids` is a valid
        // array of `count` entries for the call.
        let code = unsafe { request(connection, ids.as_ptr(), count) };
        if code == 0 {
            Ok(())
        } else {
            Err(LiveUnavailable::new(format!(
                "window notification subscription refused (code {code})"
            )))
        }
    }

    /// One bounds read.
    #[must_use]
    pub fn bounds(&self, cg_window_id: u32) -> Option<OverlayRect> {
        let (Some(connection), Some(bounds)) = (self.connection, self.symbols.bounds) else {
            return None;
        };
        let mut out = CgRect::default();
        // SAFETY: resolved symbol with the documented shape; `out` is a valid
        // out pointer for one rectangle.
        let code = unsafe { bounds(connection, cg_window_id, &raw mut out) };
        (code == 0 && out.width > 0.0 && out.height > 0.0)
            .then(|| OverlayRect::new(out.x, out.y, out.width, out.height))
    }

    /// Removes every registration (idempotent). The slot is cleared first: a
    /// callback in flight finds nothing.
    pub fn unregister(&mut self) {
        *ACTIVE_SLOTS.lock().unwrap_or_else(PoisonError::into_inner) = None;
        if let Some(connection) = self.connection.take() {
            for event in self.registered.drain(..) {
                // Best effort: the slot is already empty.
                let _ = self.symbols.base.remove_proc(connection, window_event, event);
            }
        }
    }
}

impl Drop for SkyLightWindowNotify {
    fn drop(&mut self) { self.unregister(); }
}
