//! Safe native window ownership for Tiling (and any future window consumer).
//!
//! No consumer exists yet and native behavior is unverified: this module is
//! compile- and fake-proven only. Nothing starts at construction; an adapter
//! sends no AX message until a consumer installs an epoch and tracks an
//! application.
//!
//! # Owner map
//!
//! | Thread | Owns |
//! | --- | --- |
//! | Main | `AppKit` applications and `NSScreen` only. Never executes an AX query, enumeration chunk or action. |
//! | Consumer AX owner (this module; Tiling's is `stache-tiling-ax-owner`) | The AX API, per-application [`AxObserver`](crate::ax::AxObserver)s, the run loop that delivers their callbacks and every retained window element. |
//! | `MenuAnywhere`'s `stache-ax-owner` | Its own AX state. Never shared; this module rejects that thread name. |
//!
//! The owner never waits on main, so there is no main -> owner -> main wait.
//! Commands cross a bounded channel plus a run-loop wakeup; replies and events
//! are pure values. No `unsafe impl Send`/`Sync` and no pointer-as-integer
//! passing exist here.
//!
//! # Identity
//!
//! A [`WindowInstance`] is `(adapter, epoch, exact application instance,
//! serial)`. Only the owner holds the AX element. Before every action it
//! revalidates the epoch, the application instance, the retained element and
//! current `AXWindows` membership. A destroy notification invalidates the token
//! before any queued command is examined. `CGWindowID` is metadata:
//! [`WindowAdapter::resolve_window_id`] returns only a currently live token.
//!
//! # Gate
//!
//! [`WindowIssuance`] is checked against the [`GenerationCell`] (accepted
//! revision, live generation, exact token, plan sequence, deadline) at the
//! instant of send, with a one-shot compare-and-swap committed after the
//! messaging timeout is set and immediately before the native call. Nothing
//! else runs between commit and send. Invalidation is lock-free and returns
//! while a send is stalled. A timeout after the commit is `Uncertain`.
//!
//! # Focus
//!
//! Focusing a window is **not** a second activation path: callers combine
//! `ApplicationObservation::activate_exact` (the only activation path) with
//! [`WindowAction::Raise`]. This module never calls `NSRunningApplication`
//! activation.

mod adapter;
mod commands;
mod core;
mod driver;
mod events;
mod identity;
mod issuance;
mod system;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod tests;

pub use core::{
    ENUMERATION_CHUNK_BUDGET, ENUMERATION_CHUNK_ELEMENTS, MAX_COMMANDS_PER_TURN,
    MAX_SUSPENDED_PER_APPLICATION, MAX_WINDOWS_PER_APPLICATION,
};

pub use adapter::{AdapterOptions, OWNER_QUEUE_BOUND, SpawnError, WindowAdapter, hop_wait};
pub use commands::{
    ActionRequest, CleanupReport, CloseOutcome, FocusOutcome, HiddenOperation, HiddenOutcome,
    HiddenRequest, InstallError, InstallRequest, SnapshotOutcome, StopOutcome, TrackOutcome,
    TrackRequest, WindowAction,
};
pub use driver::{Driver, RunLoopDriver, Waker};
pub use events::{
    ApplicationChange, Capability, EnumerationAbort, EventBatch, EventQueue, SnapshotOrigin,
    TabBar, TabBarId, TabBarRead, TabInfo, TabMember, WindowChange, WindowEvent, WindowEventKind,
    WindowSnapshot,
};
pub use identity::{AdapterId, RuntimeEpoch, WindowInstance};
pub use issuance::{
    ApplicationIssuance, DispatchError, GenerationCell, IssuanceCancel, IssuedKind, NativeStatus,
    NotIssuedReason, StaleReason, WindowActionOutcome, WindowIssuance,
};
pub use system::{SystemWindowClock, SystemWindows, WindowClock, WindowObserver, WindowSystem};
