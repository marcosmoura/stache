//! The issuance gate and the typed action outcomes.
//!
//! # Gate
//!
//! A [`WindowIssuance`] names one action: exact window token, the generation
//! and accepted config revision it was planned under, its plan sequence and its
//! deadline. The owner checks all of them against the runtime's
//! [`GenerationCell`] immediately before the native send, then commits the
//! one-shot issuance with a single compare-and-swap and sends. Nothing but
//! atomics and the immediate native call runs between the check and the send;
//! no module closure, dispatch, preflight or wait runs under the gate.
//!
//! Logical invalidation ([`GenerationCell::invalidate`],
//! [`WindowIssuance::cancel`]) is a lock-free atomic operation independent of
//! the owner thread: it returns while a native send is stalled. The
//! linearisation point is the commit CAS: an invalidation that wins it
//! guarantees nothing is sent; one that loses it reports
//! [`IssuanceCancel::AlreadyIssued`] because an issued send cannot be undone.
//!
//! One window is deliberately conservative: between the commit CAS and the
//! final runtime re-check the state is already `COMMITTED`, so a `cancel()`
//! racing that re-check reports `AlreadyIssued` even when the re-check then
//! refuses and nothing is ever sent. That is the safe side: callers treat
//! `AlreadyIssued` as `Uncertain`, never as "not applied".

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

use super::identity::WindowInstance;
use crate::applications::ApplicationId;
use crate::ax::{AxError, FrameRead, WriteStatus};

/// The runtime-wide atomic facts every issuance is checked against.
///
/// Generation `0` is reserved for "nothing live".
#[derive(Debug, Default)]
pub struct GenerationCell {
    live: AtomicU64,
    revision: AtomicU64,
    plan_floor: AtomicU64,
    sealed: AtomicBool,
}

impl GenerationCell {
    /// A cell with nothing live.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Publishes the live generation and its accepted config revision.
    pub fn publish(&self, generation: u64, revision: u64) {
        self.revision.store(revision, Ordering::SeqCst);
        self.live.store(generation, Ordering::SeqCst);
    }

    /// Replaces the accepted config revision without changing the generation.
    pub fn accept_revision(&self, revision: u64) {
        self.revision.store(revision, Ordering::SeqCst);
    }

    /// Invalidates `generation` if it is still live. Lock-free; never waits on
    /// the owner thread or on an in-flight send. Returns whether it was live.
    pub fn invalidate(&self, generation: u64) -> bool {
        generation != 0
            && self
                .live
                .compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
    }

    /// Refuses every plan with a sequence below `floor`.
    pub fn supersede_below(&self, floor: u64) {
        self.plan_floor.fetch_max(floor, Ordering::SeqCst);
    }

    /// The accepted config revision the gate currently holds.
    #[must_use]
    pub fn revision(&self) -> u64 { self.revision.load(Ordering::SeqCst) }

    /// Seals the runtime: no further issuance, ever.
    pub fn seal(&self) {
        self.sealed.store(true, Ordering::SeqCst);
        self.live.store(0, Ordering::SeqCst);
    }

    /// Whether the runtime is sealed.
    #[must_use]
    pub fn is_sealed(&self) -> bool { self.sealed.load(Ordering::SeqCst) }

    /// The live generation, `0` when none.
    #[must_use]
    pub fn live_generation(&self) -> u64 { self.live.load(Ordering::SeqCst) }

    /// Checks the runtime-wide facts only (seal, generation, revision, floor).
    pub(super) fn admit_runtime(&self, issuance: &WindowIssuance) -> Result<(), NotIssuedReason> {
        if self.is_sealed() {
            return Err(NotIssuedReason::Sealed);
        }
        let live = self.live.load(Ordering::SeqCst);
        if live == 0 || live != issuance.generation() {
            return Err(NotIssuedReason::GenerationInvalid);
        }
        if self.revision.load(Ordering::SeqCst) != issuance.revision() {
            return Err(NotIssuedReason::RevisionChanged);
        }
        if issuance.plan_sequence() < self.plan_floor.load(Ordering::SeqCst) {
            return Err(NotIssuedReason::Superseded);
        }
        Ok(())
    }

    /// Checks everything except the one-shot commit.
    pub(super) fn admit(&self, issuance: &WindowIssuance) -> Result<(), NotIssuedReason> {
        self.admit_runtime(issuance)?;
        match issuance.inner.state.load(Ordering::SeqCst) {
            CANCELLED => Err(NotIssuedReason::Cancelled),
            COMMITTED => Err(NotIssuedReason::AlreadyUsed),
            _ => Ok(()),
        }
    }
}

const ARMED: u8 = 0;
const CANCELLED: u8 = 1;
const COMMITTED: u8 = 2;

#[derive(Debug)]
struct IssuanceInner {
    window: WindowInstance,
    generation: u64,
    revision: u64,
    plan_sequence: u64,
    deadline: Instant,
    state: AtomicU8,
}

/// The result of [`WindowIssuance::cancel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssuanceCancel {
    /// No send can happen for this issuance.
    Cancelled,
    /// The send committed first and cannot be undone.
    AlreadyIssued,
}

/// A one-shot authorization for one window action. Cheap to clone;
/// `Send + Sync`; clones share one commit state.
#[derive(Clone, Debug)]
pub struct WindowIssuance {
    inner: Arc<IssuanceInner>,
}

impl WindowIssuance {
    /// Creates an armed issuance.
    #[must_use]
    pub fn new(
        window: WindowInstance,
        generation: u64,
        revision: u64,
        plan_sequence: u64,
        deadline: Instant,
    ) -> Self {
        Self {
            inner: Arc::new(IssuanceInner {
                window,
                generation,
                revision,
                plan_sequence,
                deadline,
                state: AtomicU8::new(ARMED),
            }),
        }
    }

    /// The exact window token.
    #[must_use]
    pub fn window(&self) -> WindowInstance { self.inner.window }

    /// The generation the action was planned under.
    #[must_use]
    pub fn generation(&self) -> u64 { self.inner.generation }

    /// The accepted config revision the action was planned under.
    #[must_use]
    pub fn revision(&self) -> u64 { self.inner.revision }

    /// The plan sequence.
    #[must_use]
    pub fn plan_sequence(&self) -> u64 { self.inner.plan_sequence }

    /// The instant after which nothing may be sent.
    #[must_use]
    pub fn deadline(&self) -> Instant { self.inner.deadline }

    /// Cancels without waiting on anything.
    #[must_use = "AlreadyIssued means the action cannot be undone"]
    pub fn cancel(&self) -> IssuanceCancel {
        match self.inner.state.compare_exchange(
            ARMED,
            CANCELLED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) | Err(CANCELLED) => IssuanceCancel::Cancelled,
            Err(_) => IssuanceCancel::AlreadyIssued,
        }
    }

    /// Whether it was cancelled before any send committed.
    #[must_use]
    pub fn is_cancelled(&self) -> bool { self.inner.state.load(Ordering::SeqCst) == CANCELLED }

    /// Whether the send committed.
    #[must_use]
    pub fn is_issued(&self) -> bool { self.inner.state.load(Ordering::SeqCst) == COMMITTED }

    /// The owner's commit point, called immediately before the native send
    /// (after the timeout is set). It re-checks every fact, then commits the
    /// one-shot compare-and-swap, then re-checks the runtime facts once more:
    /// an invalidation that landed before the final re-check wins and the
    /// issuance is cancelled instead of sent.
    pub(super) fn commit(&self, cell: &GenerationCell, now: Instant) -> Result<(), CommitRefusal> {
        cell.admit(self).map_err(CommitRefusal::NotIssued)?;
        if now >= self.inner.deadline {
            return Err(CommitRefusal::Expired);
        }
        if self
            .inner
            .state
            .compare_exchange(ARMED, COMMITTED, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(CommitRefusal::NotIssued(NotIssuedReason::Cancelled));
        }
        if let Err(reason) = cell.admit_runtime(self) {
            // The owner is the only writer out of COMMITTED, and nothing was sent.
            self.inner.state.store(CANCELLED, Ordering::SeqCst);
            return Err(CommitRefusal::NotIssued(reason));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ApplicationIssuanceInner {
    application: ApplicationId,
    deadline: Instant,
    state: AtomicU8,
}

/// A one-shot authorization for one application-level `AXHidden` write: the
/// AX fallback when the public hide or unhide returned `false`.
///
/// It is deliberately **not** checked against the [`GenerationCell`]: frame
/// writes are sealed at stop, while restoring a hidden application happens
/// after that seal and needs its own authority. The consumer owns the cancel:
/// cancelling before the send commits guarantees nothing is sent; after it,
/// [`IssuanceCancel::AlreadyIssued`] says the write cannot be undone.
#[derive(Clone, Debug)]
pub struct ApplicationIssuance {
    inner: Arc<ApplicationIssuanceInner>,
}

impl ApplicationIssuance {
    /// Creates an armed authorization for one exact application instance.
    #[must_use]
    pub fn new(application: ApplicationId, deadline: Instant) -> Self {
        Self {
            inner: Arc::new(ApplicationIssuanceInner {
                application,
                deadline,
                state: AtomicU8::new(ARMED),
            }),
        }
    }

    /// The exact application instance.
    #[must_use]
    pub fn application(&self) -> ApplicationId { self.inner.application }

    /// The instant after which nothing may be sent.
    #[must_use]
    pub fn deadline(&self) -> Instant { self.inner.deadline }

    /// Cancels without waiting on anything.
    #[must_use = "AlreadyIssued means the write cannot be undone"]
    pub fn cancel(&self) -> IssuanceCancel {
        match self.inner.state.compare_exchange(
            ARMED,
            CANCELLED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) | Err(CANCELLED) => IssuanceCancel::Cancelled,
            Err(_) => IssuanceCancel::AlreadyIssued,
        }
    }

    /// Whether it was cancelled before any send committed.
    #[must_use]
    pub fn is_cancelled(&self) -> bool { self.inner.state.load(Ordering::SeqCst) == CANCELLED }

    /// Whether the send committed.
    #[must_use]
    pub fn is_issued(&self) -> bool { self.inner.state.load(Ordering::SeqCst) == COMMITTED }

    /// The one-shot commit for a backend that is not the real owner (a fake):
    /// `true` exactly once and only while armed and unexpired.
    #[must_use]
    pub fn commit_send(&self, now: Instant) -> bool { self.commit(now).is_ok() }

    /// The owner's commit point, immediately before the native send.
    pub(super) fn commit(&self, now: Instant) -> Result<(), CommitRefusal> {
        if now >= self.inner.deadline {
            return Err(CommitRefusal::Expired);
        }
        self.inner
            .state
            .compare_exchange(ARMED, COMMITTED, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| ())
            .map_err(|state| {
                CommitRefusal::NotIssued(if state == COMMITTED {
                    NotIssuedReason::AlreadyUsed
                } else {
                    NotIssuedReason::Cancelled
                })
            })
    }
}

/// Why [`WindowIssuance::commit`] declined.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommitRefusal {
    /// A gate fact failed.
    NotIssued(NotIssuedReason),
    /// The deadline passed.
    Expired,
}

/// Why a window action provably did not reach the native send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotIssuedReason {
    /// The issuance (or the caller's cancellation) was cancelled first.
    Cancelled,
    /// The issuance was already used.
    AlreadyUsed,
    /// The planned generation is not the live one.
    GenerationInvalid,
    /// The accepted config revision changed.
    RevisionChanged,
    /// A newer plan superseded it.
    Superseded,
    /// The runtime is sealed or shutting down.
    Sealed,
    /// The owner thread is gone or poisoned by a contained panic.
    OwnerUnavailable,
    /// The request asked for nothing.
    EmptyRequest,
    /// Accessibility permission is missing; the capability is degraded.
    PermissionDenied,
    /// The messaging timeout could not be set, so nothing was sent.
    TimeoutSetFailed(AxError),
    /// Window membership could not be verified, so nothing was sent.
    MembershipUnverified(AxError),
    /// The command was not queued.
    Dispatch(DispatchError),
    /// The caller's wait ended and the unissued command was abandoned.
    AbandonedBeforeSend,
}

/// Why a command could not be queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchError {
    /// The bounded command queue is full.
    Full,
    /// The owner thread is stopped or never started.
    Closed,
}

/// Why a token no longer names a live window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaleReason {
    /// Minted by another adapter.
    WrongAdapter,
    /// Minted by another or an ended runtime epoch.
    WrongEpoch,
    /// The owner has no such token (never adopted, or already retired).
    UnknownWindow,
    /// The window was destroyed.
    Destroyed,
    /// The exact application instance is gone or not tracked.
    ApplicationGone,
    /// The element is no longer a member of the application's `AXWindows`.
    NotMember,
    /// The window left `AXWindows` but its element is alive (an inactive native
    /// tab): its token stays valid while it is unlisted, yet it cannot act.
    Suspended,
}

/// How an issued action ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssuedKind {
    /// Native acceptance of every requested part (not proof of the geometry).
    Applied,
    /// One requested half was accepted and the other failed.
    Partial,
    /// The send may have run but its result is not known (`CannotComplete`,
    /// lost or late acknowledgement). Never "not applied".
    Uncertain,
    /// The send was issued and the system definitively refused it.
    Rejected,
}

/// The native status of an issued action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeStatus {
    /// A frame write: per half.
    Frame {
        /// The position half.
        position: WriteStatus,
        /// The size half.
        size: WriteStatus,
    },
    /// A single action call (`AXRaise`).
    Action(Result<(), AxError>),
    /// The result was never observed (acknowledgement timeout after issue).
    Unobserved,
}

/// The truth about one window action.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowActionOutcome {
    /// Provably not sent.
    NotIssued(NotIssuedReason),
    /// The deadline passed before the send. Nothing was sent.
    ExpiredBeforeSend,
    /// The token no longer names a live window. Nothing was sent.
    Stale(StaleReason),
    /// The send was issued.
    Issued {
        /// How it ended.
        kind: IssuedKind,
        /// The native status.
        native: NativeStatus,
        /// The geometry read back after a frame write, when it could be read.
        observed: Option<FrameRead>,
    },
}

impl WindowActionOutcome {
    /// Whether this outcome proves nothing was sent.
    #[must_use]
    pub const fn proves_no_send(&self) -> bool {
        matches!(
            self,
            Self::NotIssued(_) | Self::ExpiredBeforeSend | Self::Stale(_)
        )
    }

    /// An issued-but-unobserved outcome.
    pub(super) const fn uncertain() -> Self {
        Self::Issued {
            kind: IssuedKind::Uncertain,
            native: NativeStatus::Unobserved,
            observed: None,
        }
    }
}
