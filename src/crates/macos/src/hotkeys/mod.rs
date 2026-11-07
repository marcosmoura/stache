//! The main-owned Carbon hotkey adapter.
//!
//! # Ownership
//!
//! ```text
//! HotkeyControl (Send + Sync)        off-main: ack waits, opaque values only
//!   └─ MainThreadExecutor ──────────────────────────────┐
//!                                                       ▼ main thread
//! thread-local Store (per control)
//!   ├─ Box<dyn Native>  (!Send)   EventHandlerRef, EventHotKeyRef map, layout observer
//!   ├─ Rc<RefCell<Routes>>        native ID -> token, reservation, live flag
//!   ├─ active registrations, owed-cleanup guards, desired requests
//!   └─ Rc dispatcher ───────────► Carbon callback (main) ──► HotkeySink::pressed
//! ```
//!
//! Every Carbon pointer lives in the main-thread store and is `!Send` by its real
//! type; there is no `unsafe impl Send` or `Sync`. Off-main callers only hold
//! [`HotkeyControl`] and plain values ([`BindingToken`], [`StagedId`],
//! [`CommitReport`], ...), scoped by control, generation and revision. Native
//! hotkey IDs come from a process-monotonic counter and are never reused, so a
//! queued event for a retired registration can never be read as a successor's.
//!
//! # What this module does
//!
//! - One Carbon handler, installed lazily by the first commit that registers a
//!   chord. A failed install is its own outcome ([`HandlerStatus::InstallFailed`]).
//! - Each chord is registered individually and reported individually: an occupied
//!   chord is [`BindingOutcome::Occupied`], never "all bindings active".
//! - [`HotkeyControl::prepare`] stages inert candidates, [`HotkeyControl::commit`]
//!   transfers unchanged registrations without re-registering them, registers new
//!   ones, publishes central Carbon reservations, flips routes at one boundary and
//!   only then unregisters removed chords. The last-good set is never unregistered
//!   before a candidate is accepted.
//! - Logical removal ([`HotkeyControl::stop`], drop) stops dispatch before any
//!   main-thread hop. Native cleanup then runs under the 2 s acknowledgement /
//!   10 ms quantum bounds, rechecks cancellation before mutating and keeps failed
//!   IDs as inert guards, reported as pending cleanup. Drop alone is not proof.
//! - The Carbon callback forwards foreign, malformed, unknown and stale events,
//!   ignores releases, skips removed or yielded routes and otherwise calls the
//!   non-blocking [`HotkeySink`]. It performs no native query, wait or spawn.
//! - A distributed-notification observer only calls
//!   [`HotkeyControl::request_layout_refresh`], which coalesces. The refresh
//!   re-resolves `§` through the staging path and rechecks generation, revision and
//!   layout identity before any native mutation or publication.
//!
//! Nothing registers a hotkey at runtime: no production code constructs a
//! [`HotkeyControl`]. Default tests use `testing::FakeLog` and never touch Carbon.

mod control;
mod dispatch;
pub(crate) mod ffi;
pub mod keymap;
pub mod native;
mod staging;
mod store;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod tests;
pub mod types;

pub use control::{HotkeyControl, Timing};
pub use native::{Disposition, Native, NativeFactory, NativeHotKeyId, RawEvent, RawEventKind};
pub use types::{
    ActiveBinding, BindingOutcome, BindingToken, CommitReport, HandlerStatus, HotkeyError,
    HotkeyRequest, HotkeySink, HotkeyTarget, ResolvedChord, Scope, StagedBinding, StagedId,
    StagedReport, StaleReason, Status, StopReport,
};
