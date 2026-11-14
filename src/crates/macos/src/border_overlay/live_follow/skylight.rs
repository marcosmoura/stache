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
//! - Fast reorder (optional, not part of the registration): `SLSOrderWindow`,
//!   looked up lazily on the first fast reorder, signature assumed (see
//!   [`SkyLightWindowNotify::order_above`]). Public equivalent: `AppKit`'s
//!   `orderWindow:relativeTo:`, main thread only, which is the slower path the
//!   fast reorder was meant to shorten. A missing symbol or a non-zero result
//!   disables the fast path for the run (one diagnostic); the main-thread
//!   `Order` stays. **Measured natively on 2026-11-14: the call returns 0 and
//!   the window server posts a reorder notification for the panel, but the
//!   panel does not move above its target** (with the border itself, and in a
//!   two-process test with an `AppKit` panel: 25 calls, the panel under its
//!   target after every one). Only the main-thread `Order` re-stacks the
//!   panel, at the next display frame. This is why the focused window's
//!   `above` border is placed one window level higher instead
//!   ([`crate::border_overlay::BorderPlacement::Top`]).
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
//! # Event ids: assumptions and what a native probe saw
//!
//! The constants below started as **candidates**. A read-only native probe
//! (2026-11-14: a second process registered for ids 800..=830 on its own
//! connection and timed them against the on-screen window list) saw these:
//!
//! - **808** for a window that was re-stacked: the first notification of a
//!   click in a window, and the panel after `SLSOrderWindow`.
//! - **815** for a window that was ordered in: the panel when it was shown and
//!   after `AppKit`'s `orderWindow:relativeTo:`, and a visible window its
//!   application orders front. A click in a window posts 808 and, with it or a
//!   few milliseconds later, 815; it is from the 815 on that the window is
//!   above an `above` panel.
//! - **816** for a window that was ordered out: the panel when it was hidden,
//!   and an application's windows when Tiling hid it.
//! - **807** for the panel shown at a new size.
//!
//! 815 and 816 were first assumed the other way round. The probe did not
//! exercise the move and close ids or the thread the callback runs on.

use std::ffi::{c_int, c_void};
use std::sync::{Arc, Mutex, PoisonError};

use super::{EventSlots, FastOrderResult, LiveUnavailable, WindowEventKind};
use crate::border_overlay::OverlayRect;
use crate::border_overlay::private_skylight::{SkyLightSymbols, system_lookup};

/// Candidate event id: a window moved. **Unverified until the part 06 native check.**
pub const EVENT_WINDOW_MOVE: u32 = 806;
/// Event id: a window was resized (seen for the panel shown at a new size).
pub const EVENT_WINDOW_RESIZE: u32 = 807;
/// Event id: a window was re-stacked (seen on a click and after `SLSOrderWindow`).
pub const EVENT_WINDOW_REORDER: u32 = 808;
/// Event id: a window was ordered out (seen for a hidden panel and for the
/// windows of an application Tiling hid).
pub const EVENT_WINDOW_HIDE: u32 = 816;
/// Event id: a window was ordered in. Also posted when an application orders
/// an already visible window front (seen on a click).
pub const EVENT_WINDOW_UNHIDE: u32 = 815;
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
pub(crate) const SYM_BOUNDS: &std::ffi::CStr = c"SLSGetWindowBounds";
/// The private order call of the fast reorder (see [`SkyLightWindowNotify::order_above`]).
pub const SYM_ORDER: &std::ffi::CStr = c"SLSOrderWindow";
/// `SLSOrderWindow` mode: directly above the relative window (assumed `1`,
/// like the public `kCGSOrderAbove`/`NSWindowAbove`; `-1` is below and `0`
/// out, never used here).
pub const ORDER_ABOVE: c_int = 1;

type RequestFn = unsafe extern "C" fn(c_int, *const u32, c_int) -> c_int;
pub(crate) type BoundsFn = unsafe extern "C" fn(c_int, u32, *mut CgRect) -> c_int;
type OrderFn = unsafe extern "C" fn(c_int, u32, c_int, u32) -> c_int;

/// `CGRect` as the window server writes it (`CGFloat` is `f64` on 64-bit).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct CgRect {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

/// Extra candidate ids registered **for logging only** (unverified).
///
/// Never acted on: every id of 800..=830 not in [`WINDOW_EVENTS`]. Their events go only to
/// the raw log the owner drains ([`EventSlots::on_window_event`]), so a native
/// run shows which ids the window server really sends on a click or a raise. A
/// refused logging-only registration is skipped, never an error.
#[must_use]
pub fn logging_only_events() -> Vec<u32> {
    (800..=830).filter(|id| kind_of_event(*id).is_none()).collect()
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

/// The notify proc. Copies `(event, window id)` into the slots (raw log and,
/// for a known id, the kind bit): allocates
/// nothing, reads no AX state, and holds the slot lock only to clone the `Arc`.
unsafe extern "C" fn window_event(
    event: u32,
    data: *mut c_void,
    length: usize,
    _context: *mut c_void,
    _connection: c_int,
) {
    let _ = std::panic::catch_unwind(|| {
        // SAFETY: the window server passes a payload of `length` bytes (or null).
        let Some(id) = (unsafe { window_id_from_payload(data.cast_const(), length) }) else {
            return;
        };
        let slots = ACTIVE_SLOTS.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(slots) = slots {
            // Raw log (any registered id) plus the kind's slot bit.
            slots.on_window_event(event, id);
        }
    });
}

/// The system registration, subscription and bounds read.
pub struct SkyLightWindowNotify {
    symbols: WindowSymbols,
    connection: Option<c_int>,
    registered: Vec<u32>,
    /// `SLSOrderWindow`, looked up on the first fast reorder (`None` before,
    /// empty inner value when missing). Never needed by live follow itself.
    order: std::cell::OnceCell<Option<OrderFn>>,
    lookup: fn(&std::ffi::CStr) -> *mut c_void,
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
            order: std::cell::OnceCell::new(),
            lookup: system_lookup,
        }
    }

    /// Over injected symbols and an injected lookup for `SLSOrderWindow`
    /// (tests: a missing symbol).
    #[cfg(test)]
    pub(crate) const fn with_order_lookup(
        symbols: WindowSymbols,
        lookup: fn(&std::ffi::CStr) -> *mut c_void,
    ) -> Self {
        Self {
            symbols,
            connection: None,
            registered: Vec::new(),
            order: std::cell::OnceCell::new(),
            lookup,
        }
    }

    /// The fast reorder: orders our overlay panel `panel` directly above
    /// `target` on our main connection (the connection that owns the panel),
    /// only while the main thread records the panel as visible above that
    /// target (`panel_registry`, lock held across the call). Never orders a
    /// panel in.
    ///
    /// Assumed, **unverified** signature (written from the author's
    /// recollection of the C shape, not from any border tool or yabai header):
    /// `CGError SLSOrderWindow(int connection, uint32_t window, int mode,
    /// uint32_t relative_to_window)` with mode above = 1, below = -1, out = 0;
    /// a window of another connection is accepted as `relative_to_window`.
    ///
    /// Measured natively on 2026-11-14: the call is accepted (0, and a reorder
    /// notification for the panel follows) but the panel stays where it was
    /// (see the module documentation).
    pub fn order_above(&mut self, panel: u32, target: u32) -> FastOrderResult {
        let Some(connection) = self.connection else {
            return FastOrderResult::Missing("SLSMainConnectionID");
        };
        let lookup = self.lookup;
        let order = *self.order.get_or_init(|| {
            let address = lookup(SYM_ORDER);
            // SAFETY: the `dlsym` address of the exact named symbol, with the
            // documented (assumed) shape; data and function pointers have the
            // same size on every supported target.
            (!address.is_null())
                .then(|| unsafe { std::mem::transmute::<*mut c_void, OrderFn>(address) })
        });
        let Some(order) = order else {
            return FastOrderResult::Missing("SLSOrderWindow");
        };
        crate::border_overlay::panel_registry::with_eligible(panel, target, || {
            // SAFETY: resolved symbol with the documented shape; plain values.
            unsafe { order(connection, panel, ORDER_ABOVE, target) }
        })
        .map_or(FastOrderResult::NotShown, |code| {
            if code == 0 {
                FastOrderResult::Ordered
            } else {
                FastOrderResult::Refused(code)
            }
        })
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
        // Logging-only candidates: best effort, a refusal is not an error.
        for event in logging_only_events() {
            if self.symbols.base.register_proc(connection, window_event, event) == Some(0) {
                self.registered.push(event);
            }
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
