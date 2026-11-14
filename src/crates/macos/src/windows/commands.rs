//! Requests, outcomes and the owner command protocol.

use std::sync::Arc;
use std::time::Instant;

use stache_sdk::CancellationToken;

use super::core::EpochCounters;
use super::events::{EventQueue, WindowSnapshot};
use super::identity::{RuntimeEpoch, WindowInstance};
use super::issuance::{
    ApplicationIssuance, DispatchError, GenerationCell, IssuedKind, NotIssuedReason, StaleReason,
    WindowActionOutcome, WindowIssuance,
};
use crate::applications::ApplicationId;
use crate::ax::{AxError, AxPoint, AxReading, AxSize, FrameRead};

/// A reply callback. It runs on the owner thread and must not block.
pub type Reply<T> = Box<dyn FnOnce(T) + Send>;

/// What one install attaches to a new runtime epoch.
#[derive(Clone)]
pub struct InstallRequest {
    /// The bounded sink events leave through.
    pub queue: Arc<EventQueue>,
    /// The generation cell every action is checked against. Stopping the
    /// runtime seals it.
    pub gate: Arc<GenerationCell>,
}

/// Why an install did not start an epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallError {
    /// An earlier cleanup is incomplete; retry it first.
    Quarantined {
        /// Applications still held inert.
        applications: usize,
        /// Registrations still to remove.
        registrations: usize,
    },
    /// An epoch is already running.
    AlreadyInstalled(RuntimeEpoch),
    /// The command was not queued.
    Dispatch(DispatchError),
    /// No acknowledgement within the hop budget. The command stays queued and
    /// may still run; call [`super::WindowAdapter::retry_cleanup`] or stop to
    /// observe the truth.
    Unacknowledged,
    /// The owner thread is gone.
    OwnerUnavailable,
}

/// Observation request for one application instance.
#[derive(Clone)]
pub struct TrackRequest {
    /// The exact application instance.
    pub application: ApplicationId,
    /// Its process id: diagnostic and the AX handle, never identity.
    pub pid: i32,
    /// Cancels the initial enumeration.
    pub cancel: CancellationToken,
    /// The initial enumeration's deadline.
    pub deadline: Option<Instant>,
}

/// The result of a track request. Enumeration continues asynchronously.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackOutcome {
    /// Observation is installed and enumeration is queued.
    Tracking,
    /// This application instance is already observed.
    AlreadyTracked,
    /// No such running epoch.
    WrongEpoch,
    /// Accessibility permission is missing; the capability is degraded.
    PermissionDenied,
    /// Observation could not be installed.
    Failed(AxError),
    /// The command was not queued.
    Dispatch(DispatchError),
    /// No acknowledgement within the hop budget.
    Unacknowledged,
    /// The owner thread is gone.
    OwnerUnavailable,
}

/// A window action.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowAction {
    /// Writes position and size as separate AX calls (position first).
    SetFrame {
        /// The new position, when requested.
        position: Option<AxPoint>,
        /// The new size, when requested.
        size: Option<AxSize>,
    },
    /// `AXRaise`. It does not activate the application: combine it with
    /// `ApplicationObservation::activate_exact`.
    Raise,
}

/// One gated action.
#[derive(Clone, Debug)]
pub struct ActionRequest {
    /// The gate and the exact window token.
    pub issuance: WindowIssuance,
    /// What to do.
    pub action: WindowAction,
}

/// Where one frame sits in an animated move. It decides how much the shard
/// verifies and reads, nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameStage {
    /// The first write of an animation: membership is re-listed, no readback.
    First,
    /// A write between the first and the final one: the retained element and
    /// the issuance gate only (no `AXWindows` listing), no readback.
    Intermediate,
    /// The exact target of an animation: membership is re-listed, no readback
    /// (the settle readback is a separate snapshot).
    Final,
    /// A non-animated write: membership is re-listed and the frame is read
    /// back once, exactly like [`WindowAction::SetFrame`].
    Direct,
}

/// One gated frame write for the per-shard latest-frame-wins mailbox.
#[derive(Clone, Debug)]
pub struct FrameRequest {
    /// The gate and the exact window token. Its plan sequence orders frames.
    pub issuance: WindowIssuance,
    /// The new position, when requested.
    pub position: Option<AxPoint>,
    /// The new size, when requested.
    pub size: Option<AxSize>,
    /// How much the shard verifies and reads.
    pub stage: FrameStage,
}

/// The callback one frame is answered through, exactly once.
pub type FrameReplyFn = Box<dyn FnOnce(FrameReply) + Send>;

/// What the shard knows about one frame it answered.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameAck {
    /// The truth about the write.
    pub outcome: WindowActionOutcome,
    /// The shard clock just before the native send; `None` when nothing was
    /// sent.
    pub sent_at: Option<Instant>,
}

/// The one answer a frame gets, exactly once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameReply {
    /// The shard decided the frame.
    Done(FrameAck),
    /// A newer frame of the same plan sequence replaced this pending one: a
    /// dropped frame that was never written.
    Replaced,
}

/// What an application-level `AXHidden` request does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HiddenOperation {
    /// Reads `AXHidden`.
    Read,
    /// Writes `AXHidden`. Needs an [`ApplicationIssuance`].
    Set(bool),
}

/// One application-level `AXHidden` request.
///
/// The fallback when the public hide or unhide returned `false`.
/// Exact-instance scoped; it is checked against the epoch and the tracked
/// application, and a write against its own [`ApplicationIssuance`] (not the
/// frame gate, which stop seals).
#[derive(Clone, Debug)]
pub struct HiddenRequest {
    /// The runtime epoch the application is tracked in.
    pub epoch: RuntimeEpoch,
    /// The exact application instance.
    pub application: ApplicationId,
    /// Read or write.
    pub operation: HiddenOperation,
    /// The one-shot authorization of a write.
    pub issuance: Option<ApplicationIssuance>,
}

/// The truth about one `AXHidden` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HiddenOutcome {
    /// A read.
    Read(AxReading<bool>),
    /// A write was issued.
    Written {
        /// How it ended.
        kind: IssuedKind,
        /// The native result.
        status: Result<(), AxError>,
        /// `AXHidden` read back afterwards.
        observed: AxReading<bool>,
    },
    /// A write was issued but its result was not observed (acknowledgement lost
    /// after the send committed). Never "not applied".
    Unobserved,
    /// Provably not sent.
    NotIssued(NotIssuedReason),
    /// The deadline passed before the send.
    ExpiredBeforeSend,
    /// The application is not tracked in this epoch.
    Stale(StaleReason),
}

impl HiddenOutcome {
    /// Whether this outcome proves nothing was sent.
    #[must_use]
    pub const fn proves_no_send(&self) -> bool {
        matches!(
            self,
            Self::NotIssued(_) | Self::ExpiredBeforeSend | Self::Stale(_) | Self::Read(_)
        )
    }
}

/// The result of a snapshot query.
#[derive(Clone, Debug, PartialEq)]
pub enum SnapshotOutcome {
    /// The snapshot.
    Snapshot(Box<WindowSnapshot>),
    /// The token no longer names a live window.
    Stale(StaleReason),
    /// Membership could not be verified.
    Unverified(AxError),
    /// The command was not queued.
    Dispatch(DispatchError),
    /// No acknowledgement within the hop budget.
    Unacknowledged,
    /// The owner thread is gone.
    OwnerUnavailable,
}

/// The result of a frame-only read: `AXPosition` and `AXSize` of one tracked
/// window after the light validation (no `AXWindows` listing).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameReadOutcome {
    /// Both halves as read; either may be unknown.
    Frame(FrameRead),
    /// The token no longer names a live window, or it is suspended.
    Stale(StaleReason),
}

/// The result of reading an application's real focused window. It is a pure
/// answer: the consumer decides what to do with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusOutcome {
    /// The application's focused window, a tracked one.
    Resolved(WindowInstance),
    /// The application has no focused window, or it is not one this epoch
    /// tracks. Nothing was emitted.
    Unresolved,
    /// The read failed.
    Failed(AxError),
    /// No such running epoch or tracked application.
    WrongEpoch,
    /// The command was not queued.
    Dispatch(DispatchError),
    /// No acknowledgement within the hop budget.
    Unacknowledged,
    /// The owner thread is gone.
    OwnerUnavailable,
}

/// The result of stopping an epoch or retrying cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopOutcome {
    /// Every registration was removed and every element released.
    Clean {
        /// Applications released.
        applications: usize,
    },
    /// Some registrations could not be removed. Observers and elements stay
    /// inert in the owner-held quarantine for [`super::WindowAdapter::retry_cleanup`].
    Quarantined {
        /// Applications held.
        applications: usize,
        /// Registrations still to remove.
        registrations: usize,
    },
    /// The epoch is not the running one; nothing was touched.
    NotCurrent,
    /// The command was not queued. Issuance is sealed regardless.
    Dispatch(DispatchError),
    /// No acknowledgement within the hop budget: cleanup is pending, not done.
    Unacknowledged,
    /// The owner thread is gone.
    OwnerUnavailable,
}

/// How a retry ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CleanupReport {
    /// Applications fully released by this attempt.
    pub cleaned_applications: usize,
    /// Applications still held.
    pub remaining_applications: usize,
    /// Registrations still to remove.
    pub remaining_registrations: usize,
}

/// How [`super::WindowAdapter::close`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    /// The thread stopped and was joined.
    Closed,
    /// Quarantined resources remain, so the thread keeps running to hold them.
    Quarantined {
        /// Registrations still to remove.
        registrations: usize,
    },
    /// The thread did not finish in time and was left detached.
    TimedOut,
    /// The command was not queued.
    Dispatch(DispatchError),
    /// Already closed.
    AlreadyClosed,
}

/// What one shard knows about a `CGWindowID`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Resolution {
    /// No tracked window claims the id.
    NoMatch,
    /// Several tracked windows of this shard claim the id.
    Ambiguous,
    /// Exactly one claims it; `Some` when it is still a live window.
    Single(Option<WindowInstance>),
}

/// What one shard found when it restored its open leases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseReport {
    /// Leases closed: written back to on, or closed without a write because a
    /// third party had already turned it on or the application is gone.
    pub restored: usize,
    /// Applications whose lease could not be restored and is still owed.
    pub owed: Vec<ApplicationId>,
}

/// The result of restoring every open `AXEnhancedUserInterface` lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseRestoreOutcome {
    /// Every lease is closed.
    Clean {
        /// Leases closed by this call.
        restored: usize,
    },
    /// Some leases failed and stay owed (retried by the stop and by
    /// `retry_cleanup`).
    Owed {
        /// The applications that still owe the attribute.
        applications: Vec<ApplicationId>,
    },
    /// A shard did not acknowledge in time: assume leases may still be open.
    Unacknowledged,
}

/// The owner command protocol. Pure data plus `Send` reply callbacks.
pub(super) enum Command {
    /// A standalone install that mints its own epoch (core tests only; the
    /// router installs with [`Command::InstallShard`]).
    #[cfg(test)]
    Install {
        request: InstallRequest,
        reply: Reply<Result<RuntimeEpoch, InstallError>>,
    },
    /// Installs an epoch the shard router minted, with counters shared by
    /// every shard of the adapter.
    InstallShard {
        request: InstallRequest,
        epoch: RuntimeEpoch,
        counters: EpochCounters,
        reply: Reply<Result<RuntimeEpoch, InstallError>>,
    },
    Track {
        epoch: RuntimeEpoch,
        request: TrackRequest,
        reply: Reply<TrackOutcome>,
    },
    Untrack {
        epoch: RuntimeEpoch,
        application: ApplicationId,
        reply: Reply<StopOutcome>,
    },
    Refresh {
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
        reply: Reply<TrackOutcome>,
    },
    FocusedWindow {
        epoch: RuntimeEpoch,
        application: ApplicationId,
        reply: Reply<FocusOutcome>,
    },
    Act {
        request: ActionRequest,
        reply: Reply<WindowActionOutcome>,
    },
    Hidden {
        request: HiddenRequest,
        reply: Reply<HiddenOutcome>,
    },
    Snapshot {
        window: WindowInstance,
        reply: Reply<SnapshotOutcome>,
    },
    /// A frame-only read of one tracked window.
    ReadFrame {
        window: WindowInstance,
        reply: Reply<FrameReadOutcome>,
    },
    #[cfg(test)]
    Resolve {
        cg_window_id: u32,
        reply: Reply<Option<WindowInstance>>,
    },
    /// Like `Resolve`, but reports how many windows claimed the id so the
    /// router can detect an id claimed on two shards.
    ResolveRaw {
        cg_window_id: u32,
        reply: Reply<Resolution>,
    },
    Stop {
        epoch: RuntimeEpoch,
        reply: Reply<StopOutcome>,
    },
    Retry {
        reply: Reply<CleanupReport>,
    },
    RestoreLeases {
        reply: Reply<LeaseReport>,
    },
    Terminate {
        reply: Reply<CloseOutcome>,
    },
}
