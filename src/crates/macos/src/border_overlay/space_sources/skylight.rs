//! The private `SkyLight` Space-change notification, resolved only by runtime
//! symbol lookup (decision 4; same policy as `ax/window_id.rs`).
//!
//! The resolver and the registration helper are shared with part 06 and live in
//! `private_skylight`; this file keeps the Space-change specifics.
//!
//! # Inventory (private API)
//!
//! - Symbols, all exported by the `SkyLight` image
//!   (`/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight`) and not
//!   declared in any public SDK header: `SLSMainConnectionID`,
//!   `SLSRegisterConnectionNotifyProc`, `SLSRemoveConnectionNotifyProc`.
//!   **Observed:** all three resolve with `dlsym` on the macOS 26 development
//!   machine (checked once with a throwaway lookup outside Stache).
//! - Missing public equivalent: `NSWorkspaceActiveSpaceDidChangeNotification`
//!   fires after the change completes and carries no display; nothing public
//!   announces that a Space change *begins*. The border must be hidden before
//!   the first transition frame (decision 9).
//! - Minimum need: one connection-level notify registration for one event id.
//!
//! The functions are never linked. A missing symbol makes registration fail
//! with `UnknownCause::MissingSymbol(name)`; a refused registration fails with
//! `UnknownCause::RegistrationFailed`. Both fail closed (borders stay hidden).
//!
//! # Signatures and event id: assumptions
//!
//! The parameter types were **not observed** (a symbol lookup reveals no
//! types); they are the long-standing C shapes of these functions as the
//! author recalls them, written here independently of any border tool or yabai
//! header:
//!
//! - `int SLSMainConnectionID(void)`;
//! - `int SLSRegisterConnectionNotifyProc(int connection, Callback, uint32_t
//!   event, void *context)` and `SLSRemoveConnectionNotifyProc` with the same
//!   parameters, both returning a `CGError`;
//! - `Callback = void (*)(uint32_t event, void *data, <length>, void *context,
//!   int connection)`. The callback declared here **never reads any argument**
//!   (it takes the active hub from a process slot), so a wrong length type or
//!   arity cannot be misread.
//!
//! [`SPACE_CHANGE_EVENTS`] is **unverified**: each value is a candidate for the
//! Space-change notification, and it is not yet known which one is delivered
//! when the change *begins* (what the gate needs), on which thread, or whether
//! it carries a display. Native run 3 observed `1401` about 2 ms **before the
//! public completion**, so it is (close to) a completion event, not a begin
//! event. `1400` is a guess by numeric adjacency only (no source documents it):
//! it is registered so the next native run can show whether it fires earlier.
//! Every candidate is reported as `SpaceChangeBegan` and each delivery is also
//! reported as a `Diagnostic(PrivateEvent { event })` fact, so the log names
//! the id that fired. A candidate that also fires for something other than a
//! Space change would hide the borders until the next public completion (fail
//! closed); the native run must check for that. If no candidate fires early, a
//! Space change is detected only at completion and the native check fails the
//! part (overview OQ-B).

use std::ffi::c_int;
use std::sync::{Arc, Mutex, PoisonError};

use super::super::private_skylight::NotifyCallback;
pub use super::super::private_skylight::SkyLightSymbols;
use super::super::spaces::{SpaceDiagnostic, UnknownCause};
use super::hub::SourceHub;

/// The candidate event ids of the Space-change notification, in registration
/// order. **Unverified**, see the module documentation.
pub const SPACE_CHANGE_EVENTS: [u32; 2] = [1401, 1400];

/// One callback per candidate, so the id that fired is known without reading
/// any callback argument.
const CANDIDATES: [(u32, NotifyCallback); 2] = [
    (
        SPACE_CHANGE_EVENTS[0],
        space_changed::<{ SPACE_CHANGE_EVENTS[0] }>,
    ),
    (
        SPACE_CHANGE_EVENTS[1],
        space_changed::<{ SPACE_CHANGE_EVENTS[1] }>,
    ),
];

/// Registers and removes the Space-change notification. The trait lets tests
/// and part 06 substitute the mechanism.
pub trait SpaceChangeRegistrar {
    /// Registers; every Space-change event calls
    /// [`SourceHub::space_change_began`] on `hub`.
    ///
    /// # Errors
    /// The cause to report with `SourceLost`.
    fn register(&mut self, hub: Arc<SourceHub>) -> Result<(), UnknownCause>;
    /// Removes the registration and the hub (idempotent).
    fn unregister(&mut self);
}

/// The hub the callback pushes through; empty after `unregister`. A process
/// slot, because the private callback receives no context we read.
static ACTIVE_HUB: Mutex<Option<Arc<SourceHub>>> = Mutex::new(None);

/// The notify proc of candidate `EVENT`. Reads no argument, allocates
/// nothing, holds the slot lock only to clone the `Arc` and calls out after
/// releasing it.
unsafe extern "C" fn space_changed<const EVENT: u32>(
    _: u32,
    _: *mut std::ffi::c_void,
    _: usize,
    _: *mut std::ffi::c_void,
    _: c_int,
) {
    let _ = std::panic::catch_unwind(|| {
        let hub = ACTIVE_HUB.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(hub) = hub {
            hub.private_space_event(EVENT);
        }
    });
}

/// The system registration.
pub struct SkyLightSpaceNotify {
    symbols: SkyLightSymbols,
    /// The connection and the candidates registered on it.
    registered: Option<(c_int, Vec<(u32, NotifyCallback)>)>,
}

impl SkyLightSpaceNotify {
    /// Resolves the symbols; registers nothing.
    #[must_use]
    pub fn system() -> Self { Self::with_symbols(SkyLightSymbols::system()) }

    /// Over injected symbols.
    #[must_use]
    pub const fn with_symbols(symbols: SkyLightSymbols) -> Self {
        Self { symbols, registered: None }
    }
}

impl SpaceChangeRegistrar for SkyLightSpaceNotify {
    fn register(&mut self, hub: Arc<SourceHub>) -> Result<(), UnknownCause> {
        if self.registered.is_some() {
            return Ok(());
        }
        if let Some(name) = self.symbols.missing_symbol() {
            return Err(UnknownCause::MissingSymbol(name));
        }
        let Some(connection) = self.symbols.connection() else {
            return Err(UnknownCause::RegistrationFailed);
        };
        *ACTIVE_HUB.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&hub));
        // Each callback ignores every argument and the context is null. Every
        // result is reported for the log; one accepted candidate is enough.
        let mut accepted = Vec::new();
        for (event, callback) in CANDIDATES {
            let code = self.symbols.register_proc(connection, callback, event);
            hub.diagnostic(SpaceDiagnostic::PrivateRegistration { event, code });
            if code == Some(0) {
                accepted.push((event, callback));
            }
        }
        if accepted.is_empty() {
            *ACTIVE_HUB.lock().unwrap_or_else(PoisonError::into_inner) = None;
            return Err(UnknownCause::RegistrationFailed);
        }
        self.registered = Some((connection, accepted));
        Ok(())
    }

    fn unregister(&mut self) {
        // Clear the slot first: a callback in flight finds no hub.
        *ACTIVE_HUB.lock().unwrap_or_else(PoisonError::into_inner) = None;
        if let Some((connection, accepted)) = self.registered.take() {
            // Best effort: the slot is already empty.
            for (event, callback) in accepted {
                let _ = self.symbols.remove_proc(connection, callback, event);
            }
        }
    }
}

impl Drop for SkyLightSpaceNotify {
    fn drop(&mut self) { self.unregister(); }
}
