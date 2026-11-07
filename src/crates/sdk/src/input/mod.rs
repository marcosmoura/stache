//! The shared input contract and lazy tap lifecycle.
//!
//! It covers values, filters, context snapshots, the dispatch router, the press
//! ledger, the reservation registry and the work-reservation contract over an
//! abstract native owner. This is the platform-independent half of the input
//! service. No real tap exists here; `stache-macos` supplies an [`InputBackend`] that owns one
//! dedicated thread and calls [`InputService::dispatch`] synchronously from the
//! tap callback.
//!
//! # Limits stated honestly
//!
//! - The macOS HID tap has no per-keyboard identity. A press is a key code or
//!   mouse button plus an owner [`Epoch`].
//! - The 1 ms callback target is soft: elapsed time is measured after a callback
//!   returns, then the subscriber is made inert. Arbitrary code cannot be
//!   preempted.
//! - Panics are contained with `catch_unwind` only in unwind-enabled builds. The
//!   release profile uses `panic = "abort"`, so production callbacks must be
//!   audited, bounded and non-panicking.
//! - Subscriber and context snapshots use a `RwLock<Arc<_>>` read with
//!   `try_read` (see `SnapshotCell`); `arc-swap` is not available offline. A busy
//!   read fails open and is counted, it never waits.

#![allow(clippy::redundant_pub_crate)]

mod capability;
mod context_snapshot;
mod control;
mod diagnostics;
mod event;
mod filter;
mod ledger;
mod reservations;
mod router;
mod service;
mod subscribers;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod tests;
mod work;

pub use capability::InputCapability;
pub use context_snapshot::{
    ContextPublisher, ContextSnapshot, ExemptionFlags, FrontmostApp, FrontmostState,
};
pub use control::{
    ACKNOWLEDGEMENT_BUDGET, AckSignal, CALLBACK_SOFT_TARGET, CANCELLATION_QUANTUM,
    CONTROL_QUEUE_CAPACITY, ControlAction, ControlQueue, ControlRejected, ControlRequest,
    FailureCause, InputBackend, InputClock, MAX_RETRY_ATTEMPTS, NativeError, SystemClock, TapKind,
};
pub use diagnostics::{DIAGNOSTIC_CAPACITY, InputCounters, InputDiagnostic};
pub use event::{
    Disposition, Epoch, InputEvent, InputEventKind, KeyCode, ModifierFlags, MouseButton, PressId,
    PressStream, RawInput,
};
pub use filter::{FilterError, InputFilter, KindSet, MAX_FILTER_ENTRIES, ModifierMatch, Selector};
pub use ledger::PRESS_LEDGER_CAPACITY;
pub use reservations::{
    Chord, Claim, ReservationBackend, ReservationConflict, ReservationError, ReservationId,
    ReservationOwner, ReservationRegistry,
};
pub use service::{
    InputError, InputService, NativeCleanup, Retained, Subscription, TapStatus, UnsubscribeReport,
};
pub use subscribers::{
    CallbackScope, HandlerSpec, InertReason, InputHandler, InputObserver, InputPriority,
    ObserverSpec, SubscriptionId, WorkRequirement,
};
pub use work::{NoWork, WorkRejected, WorkReservation, WorkReservations};
