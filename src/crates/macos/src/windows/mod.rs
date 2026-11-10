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
//! | Consumer AX shards (this module; Tiling's are `stache-tiling-ax-<n>` and `stache-tiling-ax-overflow`) | One thread per tracked application, up to [`MAX_APPLICATION_SHARDS`], then one shared overflow shard: the AX API, that application's [`AxObserver`](crate::ax::AxObserver), the run loop that delivers its callbacks and its retained window elements. The [`WindowAdapter`] router owns no thread and makes no AX call. |
//! | `MenuAnywhere`'s `stache-ax-owner` | Its own AX state. Never shared; this module rejects that thread name. |
//!
//! A shard never waits on main, so there is no main -> owner -> main wait.
//! A shard starts on the first tracked application and ends on `untrack` or
//! stop, so an adapter with nothing tracked owns no thread. Commands cross a
//! bounded channel per shard plus a run-loop wakeup; replies and events
//! are pure values. No `unsafe impl Send`/`Sync` and no pointer-as-integer
//! passing exist here.
//!
//! # Identity
//!
//! A [`WindowInstance`] is `(adapter, epoch, exact application instance,
//! serial)`. Only the application's shard holds the AX element; serials come
//! from one per-epoch allocator shared by every shard. Before every action it
//! revalidates the epoch, the application instance, the retained element and
//! current `AXWindows` membership. The one exception is an
//! [`FrameStage::Intermediate`] frame, which re-checks the adapter, the epoch,
//! the application and the retained element (the serial is still held, so the
//! window is neither destroyed nor retired) but lists no `AXWindows`; the
//! first, final and direct frames and every action re-list it. A destroy
//! notification invalidates the token
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
//! Frame writes enter through a per-shard latest-frame-wins mailbox
//! ([`WindowAdapter::submit_frames`]): at most one pending frame per window,
//! bounded by [`FRAME_MAILBOX_BOUND`], its lock held only to insert or take.
//! The shard checks the gate (`admit`, then `commit` right before the send) for
//! every frame, so a sealed or superseded frame is inert; each frame's reply
//! runs exactly once. Only [`FrameStage::Direct`] reads the frame back.
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
mod frames;
mod identity;
mod issuance;
mod lease;
mod system;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod tests;

pub use core::{
    ENUMERATION_CHUNK_BUDGET, ENUMERATION_CHUNK_ELEMENTS, MAX_COMMANDS_PER_TURN,
    MAX_SUSPENDED_PER_APPLICATION, MAX_WINDOWS_PER_APPLICATION,
};

pub use adapter::{
    AdapterOptions, MAX_APPLICATION_SHARDS, OWNER_QUEUE_BOUND, ShardStats, SpawnError,
    WindowAdapter, hop_wait,
};
pub use commands::{
    ActionRequest, CleanupReport, CloseOutcome, FocusOutcome, FrameAck, FrameReply, FrameReplyFn,
    FrameRequest, FrameStage, HiddenOperation, HiddenOutcome, HiddenRequest, InstallError,
    InstallRequest, LeaseReport, LeaseRestoreOutcome, SnapshotOutcome, StopOutcome, TrackOutcome,
    TrackRequest, WindowAction,
};
pub use driver::{Driver, RunLoopDriver, Waker};
pub use events::{
    ApplicationChange, Capability, EnhancedUiNote, EnumerationAbort, EventBatch, EventQueue,
    SnapshotOrigin, TabBar, TabBarId, TabBarRead, TabInfo, TabMember, WindowChange, WindowEvent,
    WindowEventKind, WindowSnapshot,
};
pub use frames::FRAME_MAILBOX_BOUND;
pub use identity::{AdapterId, RuntimeEpoch, WindowInstance};
pub use issuance::{
    ApplicationIssuance, DispatchError, GenerationCell, IssuanceCancel, IssuedKind, NativeStatus,
    NotIssuedReason, StaleReason, WindowActionOutcome, WindowIssuance,
};
pub use lease::{LEASE_IDLE_GRACE, NoVoiceOver, SystemVoiceOverProbe, VoiceOverProbe};
pub use system::{SystemWindowClock, SystemWindows, WindowClock, WindowObserver, WindowSystem};
