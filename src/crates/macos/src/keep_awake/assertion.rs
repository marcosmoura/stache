//! Fallible IOPM assertion ownership.
//!
//! The old `keepawake` wrapper ignores every `IOPMAssertionRelease` return code,
//! so its `Drop` cannot prove cleanup. This adapter keeps explicit ownership:
//! creation records the exact IDs it owns, release reports every ID that the OS
//! did not confirm, and unresolved IDs stay in the owner ledger. No new lease can
//! be acquired while that ledger is non-empty.
//!
//! [`AssertionOwner::cleanup_pending`] performs one bounded pass. It is
//! intentionally manual: there is no reaper, detached thread or busy retry, so
//! the caller must invoke it on the next reconcile or lifecycle transition.
//!
//! # Final-owner retention
//!
//! Dropping the last [`AssertionOwner`] is not the process-exit boundary. If a
//! best-effort `Drop` pass still cannot confirm every ID, the exact backend and
//! unresolved generation evidence move into a passive process-lifetime recovery
//! bank keyed by [`AssertionOwnerId`]. The bank starts no thread, performs no
//! native call on construction and never scans; [`AssertionOwner::recover`]
//! takes back exactly one owner record. Process exit remains the ultimate OS
//! boundary, and no `Drop` path claims a successful release.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use objc2_core_foundation::CFString;
use objc2_io_kit::{
    IOPMAssertionCreateWithName, IOPMAssertionID, IOPMAssertionRelease, kIOPMAssertionLevelOn,
    kIOReturnSuccess,
};
use stache_sdk::CancellationToken;

/// The fixed macOS assertion reason.
///
/// `reason` applies on macOS; the legacy `app_name` and reverse domain were
/// Linux-only metadata and are not binding here.
pub const ASSERTION_REASON: &str = "Stache requested system wake lock";

static NEXT_OWNER_ID: AtomicU64 = AtomicU64::new(1);

/// An opaque process-unique identity for one [`AssertionOwner`].
///
/// It is safe to persist and `Send + Sync`; it is the only key accepted by
/// [`AssertionOwner::recover`]. The numeric value has no meaning outside the
/// recovery bank.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssertionOwnerId(u64);

impl AssertionOwnerId {
    fn allocate() -> Self { Self(NEXT_OWNER_ID.fetch_add(1, Ordering::Relaxed)) }
}

impl fmt::Display for AssertionOwnerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}", self.0) }
}

/// One kind of prevention the legacy `.display(true).idle(true).sleep(true)`
/// requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AssertionKind {
    /// Prevent the display from idling off.
    Display,
    /// Prevent the system from idling asleep.
    Idle,
    /// The legacy `PreventSystemSleep` assertion.
    System,
}

impl AssertionKind {
    /// The assertion type string passed to `IOPMAssertionCreateWithName`.
    ///
    /// The system kind deliberately preserves the deprecated legacy type: the
    /// rewrite keeps the old three-assertion behavior rather than silently
    /// changing policy.
    const fn assertion_type(self) -> &'static str {
        match self {
            Self::Display => "PreventUserIdleDisplaySleep",
            Self::Idle => "PreventUserIdleSystemSleep",
            Self::System => "PreventSystemSleep",
        }
    }

    /// The stable diagnostic spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Display => "display",
            Self::Idle => "idle",
            Self::System => "system",
        }
    }
}

impl fmt::Display for AssertionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
}

/// An opaque `IOPMAssertionID`.
///
/// The raw value is exposed for diagnostics and tests; it is not a portable
/// identity and the OS may reuse it after release.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssertionId(u32);

impl AssertionId {
    /// Wraps a raw `IOPMAssertionID`.
    #[must_use]
    pub const fn new(raw: u32) -> Self { Self(raw) }

    /// The raw `IOPMAssertionID`.
    #[must_use]
    pub const fn get(self) -> u32 { self.0 }
}

impl fmt::Display for AssertionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}", self.0) }
}

/// The request intent, separate from owned evidence.
///
/// The Keep Awake policy always requests all three legacy assertions, but the
/// request is a distinct value so a failed acquisition can be described without
/// pretending the IDs belong to a lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssertionRequest {
    kinds: [AssertionKind; 3],
}

impl AssertionRequest {
    /// The complete display, idle and system request.
    #[must_use]
    pub const fn system_wake_lock() -> Self {
        Self {
            kinds: [
                AssertionKind::Display,
                AssertionKind::Idle,
                AssertionKind::System,
            ],
        }
    }

    /// The requested kinds in creation order.
    #[must_use]
    pub const fn kinds(&self) -> &[AssertionKind; 3] { &self.kinds }
}

impl Default for AssertionRequest {
    fn default() -> Self { Self::system_wake_lock() }
}

/// One successfully created assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OwnedAssertion {
    /// Which prevention the ID represents.
    pub kind: AssertionKind,
    /// The exact OS assertion ID.
    pub id: AssertionId,
}

/// One assertion the OS did not confirm as released.
///
/// The generation keeps a predecessor's evidence from being mistaken for a
/// successor's, even when the OS reuses the numeric ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PendingAssertion {
    /// Which prevention the ID represents.
    pub kind: AssertionKind,
    /// The exact unresolved OS assertion ID.
    pub id: AssertionId,
    /// The lease generation that created it.
    pub generation: u64,
}

/// The truthfulness category of one [`ReleaseReport`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReleaseStatus {
    /// Nothing was owned and no native release was attempted.
    #[default]
    Inert,
    /// Every ID owned by this pass was confirmed released.
    Complete,
    /// This pass attempted every ID and some remain unresolved.
    Pending,
    /// Another release or cleanup pass owns the transition; nothing was attempted.
    InProgress,
    /// A live lease blocks cleanup; nothing was attempted.
    Busy,
}

/// The read-only release state of one [`AssertionOwner`] ledger.
///
/// This observes an already-owned cleanup; it never starts, retries or
/// completes one. [`AssertionOwner::release_state`] reads it under one ledger
/// lock and performs no native work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerReleaseState {
    /// No active or acquiring generation, no release pass in flight and no
    /// unresolved ID: every owned ID was confirmed released.
    Released,
    /// No active generation and no release pass in flight, but unresolved IDs
    /// remain: the last release failed and has not been retried.
    Failed,
    /// A generation is active or acquiring, or a release pass is in flight.
    /// This proves neither success nor failure.
    Unanswered,
}

/// The observable result of one explicit release or rollback pass.
///
/// An empty `pending` list alone does not prove completion: check
/// [`ReleaseReport::status`] or [`ReleaseReport::is_complete`]. A non-empty
/// `pending` list means those exact IDs remain owned by the owner ledger and
/// block any fresh acquisition. [`ReleaseStatus::InProgress`] and
/// [`ReleaseStatus::Busy`] are never complete, even though they carry no IDs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleaseReport {
    status: ReleaseStatus,
    released: Vec<AssertionId>,
    pending: Vec<PendingAssertion>,
}

impl ReleaseReport {
    fn inert() -> Self { Self::default() }

    const fn complete(released: Vec<AssertionId>) -> Self {
        Self {
            status: ReleaseStatus::Complete,
            released,
            pending: Vec::new(),
        }
    }

    const fn with_pending(released: Vec<AssertionId>, pending: Vec<PendingAssertion>) -> Self {
        Self {
            status: ReleaseStatus::Pending,
            released,
            pending,
        }
    }

    fn in_progress() -> Self {
        Self {
            status: ReleaseStatus::InProgress,
            ..Self::default()
        }
    }

    fn busy() -> Self {
        Self {
            status: ReleaseStatus::Busy,
            ..Self::default()
        }
    }

    /// The truthfulness category of this pass.
    #[must_use]
    pub const fn status(&self) -> ReleaseStatus { self.status }

    /// The IDs confirmed released by this pass, in attempt order.
    #[must_use]
    pub fn released(&self) -> &[AssertionId] { &self.released }

    /// The exact IDs still unresolved after this pass.
    #[must_use]
    pub fn pending(&self) -> &[PendingAssertion] { &self.pending }

    /// Whether this report proves every owned ID confirmed released.
    ///
    /// `Inert` is complete because there was nothing to release. `InProgress`
    /// and `Busy` are never complete.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        matches!(self.status, ReleaseStatus::Inert | ReleaseStatus::Complete)
    }
}

/// A failed assertion backend call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssertionError {
    /// The OS returned a non-success `IOReturn`.
    Native {
        /// The exact return code; never interpreted as success.
        code: i32,
    },
    /// The OS reported success but produced the null assertion ID.
    InvalidId,
    /// An injected test backend observed cancellation before its native call.
    Cancelled,
    /// A test barrier ended before the fake operation could complete.
    ///
    /// The system backend never produces this; the ID stays unresolved so a
    /// timeout cannot masquerade as a successful release.
    Interrupted,
}

impl fmt::Display for AssertionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native { code } => write!(f, "IOPM return code {code:#010x}"),
            Self::InvalidId => f.write_str("IOPM returned the null assertion ID"),
            Self::Cancelled => f.write_str("the assertion call was cancelled"),
            Self::Interrupted => f.write_str("a fake barrier interrupted the assertion call"),
        }
    }
}

impl std::error::Error for AssertionError {}

/// The injectable IOPM operations.
///
/// Implementations are called from the owning thread and must be thread-safe.
/// The production backend performs one synchronous immediate call. Test backends
/// may park in a finite cancellable barrier before returning.
pub trait AssertionBackend: Send + Sync {
    /// Creates one assertion.
    ///
    /// # Errors
    /// Returns the exact native failure. A non-success code never creates an
    /// owned ID.
    fn create(
        &self,
        kind: AssertionKind,
        cancellation: &CancellationToken,
    ) -> Result<AssertionId, AssertionError>;

    /// Releases one assertion.
    ///
    /// Any non-success return keeps the ID unresolved; the caller retains it.
    ///
    /// # Errors
    /// Returns the exact native failure.
    fn release(&self, kind: AssertionKind, id: AssertionId) -> Result<(), AssertionError>;
}

/// A failed acquisition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcquireError {
    /// Another lease is live or an acquisition is in flight.
    Busy,
    /// Unresolved IDs block acquisition until a bounded cleanup pass succeeds.
    PendingCleanup {
        /// The exact IDs still owned by the ledger.
        pending: Vec<PendingAssertion>,
    },
    /// Lifecycle cancellation arrived before a complete lease existed.
    Cancelled {
        /// The rollback evidence for IDs created before cancellation.
        rollback: ReleaseReport,
    },
    /// One creation failed after a prefix was created.
    CreationFailed {
        /// The kind that failed.
        kind: AssertionKind,
        /// The exact backend failure.
        error: AssertionError,
        /// The rollback evidence for the preceding IDs.
        rollback: ReleaseReport,
    },
}

impl fmt::Display for AcquireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("an assertion lease is already active"),
            Self::PendingCleanup { pending } => {
                write!(f, "{} assertion ID(s) remain unresolved", pending.len())
            }
            Self::Cancelled { rollback } => {
                write!(f, "acquisition cancelled ({} pending)", rollback.pending.len())
            }
            Self::CreationFailed { kind, error, .. } => {
                write!(f, "creating the {kind} assertion failed: {error}")
            }
        }
    }
}

impl std::error::Error for AcquireError {}

/// The observable lifecycle stage of one [`AssertionLease`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseState {
    /// The lease owns its exact IDs and no release has started.
    Live,
    /// One release pass currently owns the transition.
    Releasing,
    /// Every owned ID was confirmed released.
    Released,
    /// A release pass finished with unresolved IDs in the owner ledger.
    Pending,
}

/// The live ownership evidence for one complete acquisition.
///
/// The lease is `Send + Sync` and carries no native object. Dropping it attempts
/// one best-effort release pass, but that attempt is not proof: any unresolved ID
/// stays in the owner ledger and continues to block acquisition. Repeated or
/// concurrent calls never claim completion while a pass is in flight.
pub struct AssertionLease {
    owner: Arc<OwnerState>,
    generation: u64,
    evidence: Mutex<LeaseEvidence>,
}

enum LeaseEvidence {
    Live(Vec<OwnedAssertion>),
    Releasing(Vec<OwnedAssertion>),
    Released,
    Pending,
}

enum LeaseClaim {
    Ids(Vec<OwnedAssertion>),
    InProgress,
    Released,
    Pending,
}

impl AssertionLease {
    /// The generation that created these IDs.
    #[must_use]
    pub const fn generation(&self) -> u64 { self.generation }

    /// The current lifecycle stage.
    #[must_use]
    pub fn state(&self) -> LeaseState {
        match &*lock(&self.evidence) {
            LeaseEvidence::Live(_) => LeaseState::Live,
            LeaseEvidence::Releasing(_) => LeaseState::Releasing,
            LeaseEvidence::Released => LeaseState::Released,
            LeaseEvidence::Pending if self.owner.pending_report(self.generation).is_complete() => {
                LeaseState::Released
            }
            LeaseEvidence::Pending => LeaseState::Pending,
        }
    }

    /// The ownership evidence still held by this lease.
    ///
    /// The list stays visible while a release pass is in flight. It is empty
    /// after a confirmed or failed pass: unresolved IDs then live in the owner
    /// ledger so `Drop` cannot forget them.
    #[must_use]
    pub fn assertions(&self) -> Vec<OwnedAssertion> {
        match &*lock(&self.evidence) {
            LeaseEvidence::Live(items) | LeaseEvidence::Releasing(items) => items.clone(),
            LeaseEvidence::Released | LeaseEvidence::Pending => Vec::new(),
        }
    }

    /// Whether every owned ID was confirmed released by an earlier pass.
    ///
    /// This is not true merely because a release was attempted or is running.
    #[must_use]
    pub fn is_confirmed_released(&self) -> bool { self.state() == LeaseState::Released }

    /// Releases every owned ID exactly once and reports the outcome.
    ///
    /// A concurrent call reports [`ReleaseStatus::InProgress`] without touching
    /// the backend. A repeated call after a failed pass reports the exact
    /// unresolved IDs without re-releasing confirmed ones; retry through
    /// [`AssertionOwner::cleanup_pending`]. A confirmed release is inert.
    pub fn release(&self) -> ReleaseReport {
        let claimed = {
            let mut slot = lock(&self.evidence);
            let current = std::mem::replace(&mut *slot, LeaseEvidence::Released);
            match current {
                LeaseEvidence::Live(items) => {
                    *slot = LeaseEvidence::Releasing(items.clone());
                    LeaseClaim::Ids(items)
                }
                LeaseEvidence::Releasing(items) => {
                    *slot = LeaseEvidence::Releasing(items);
                    LeaseClaim::InProgress
                }
                LeaseEvidence::Released => LeaseClaim::Released,
                LeaseEvidence::Pending => {
                    *slot = LeaseEvidence::Pending;
                    LeaseClaim::Pending
                }
            }
        };
        match claimed {
            LeaseClaim::Ids(items) => {
                let report = self.owner.release(self.generation, &items);
                let mut slot = lock(&self.evidence);
                *slot = if report.is_complete() {
                    LeaseEvidence::Released
                } else {
                    LeaseEvidence::Pending
                };
                report
            }
            LeaseClaim::InProgress => ReleaseReport::in_progress(),
            LeaseClaim::Released => ReleaseReport::inert(),
            LeaseClaim::Pending => self.owner.pending_report(self.generation),
        }
    }
}

impl Drop for AssertionLease {
    /// Best-effort release, never a success claim.
    fn drop(&mut self) { let _ = self.release(); }
}

impl fmt::Debug for AssertionLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AssertionLease")
            .field("generation", &self.generation)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

/// The accountable owner of live and unresolved assertion IDs.
///
/// Clone it freely; every clone shares one ledger. Keep one clone for the
/// module lifetime. A dropped lease cannot silently discard its ledger because
/// the owner, not the lease, owns unresolved evidence.
#[derive(Clone)]
pub struct AssertionOwner {
    state: Arc<OwnerState>,
}

struct OwnerState {
    identity: AssertionOwnerId,
    backend: Arc<dyn AssertionBackend>,
    ledger: Mutex<Ledger>,
}

#[derive(Default)]
struct Ledger {
    next_generation: u64,
    active: Option<u64>,
    unresolved: Vec<PendingAssertion>,
    cleaning: bool,
}

/// One owner record retained after the last handle dropped with unresolved IDs.
struct RetainedOwner {
    backend: Arc<dyn AssertionBackend>,
    next_generation: u64,
    unresolved: Vec<PendingAssertion>,
}

/// Passive process-lifetime recovery bank.
///
/// It performs no work on construction and never scans. Entries appear only when
/// the final owner handle drops with unresolved native IDs, and only the matching
/// [`AssertionOwnerId`] can take one back. Process exit is the ultimate boundary.
static RECOVERY_BANK: Mutex<BTreeMap<AssertionOwnerId, RetainedOwner>> =
    Mutex::new(BTreeMap::new());

impl AssertionOwner {
    /// Creates an owner over an injected backend.
    #[must_use]
    pub fn new(backend: Arc<dyn AssertionBackend>) -> Self {
        Self {
            state: Arc::new(OwnerState {
                identity: AssertionOwnerId::allocate(),
                backend,
                ledger: Mutex::new(Ledger::default()),
            }),
        }
    }

    /// Creates an owner over the real IOPM backend.
    #[must_use]
    pub fn system() -> Self { Self::new(Arc::new(SystemAssertionBackend)) }

    /// This owner's opaque, process-unique identity.
    ///
    /// Persist it before dropping the last handle so
    /// [`AssertionOwner::recover`] can take back a final-owner failure.
    #[must_use]
    pub fn identity(&self) -> AssertionOwnerId { self.state.identity }

    /// Recovers exactly one retained owner and its unresolved evidence.
    ///
    /// Returns `None` when no final-owner failure was retained for `identity`,
    /// including when a previous recovery already consumed it. This constructs
    /// state only: it performs no native call, scan or thread start.
    #[must_use]
    pub fn recover(identity: AssertionOwnerId) -> Option<Self> {
        let retained = lock(&RECOVERY_BANK).remove(&identity)?;
        Some(Self {
            state: Arc::new(OwnerState {
                identity,
                backend: retained.backend,
                ledger: Mutex::new(Ledger {
                    next_generation: retained.next_generation,
                    active: None,
                    unresolved: retained.unresolved,
                    cleaning: false,
                }),
            }),
        })
    }

    /// Creates the complete legacy lease, or reports why it could not.
    ///
    /// The ledger records an in-flight generation before the first native call,
    /// so a concurrent acquisition is [`AcquireError::Busy`]. Cancellation is
    /// checked before every creation and once more after each success. Any
    /// prefix is rolled back immediately; rollback failures stay owned.
    ///
    /// This is the no-callback form of
    /// [`AssertionOwner::acquire_with_rollback`]: the exact IDs, generations
    /// and rollback evidence are identical.
    ///
    /// # Errors
    /// Returns a busy, pending-cleanup, cancellation or creation failure. A
    /// failure never creates a lease.
    pub fn acquire(
        &self,
        request: &AssertionRequest,
        cancellation: &CancellationToken,
    ) -> Result<AssertionLease, AcquireError> {
        self.acquire_with_rollback(request, cancellation, || {})
    }

    /// Creates the complete legacy lease, announcing cleanup before rollback.
    ///
    /// `before_rollback` runs at most once, and only when a partial acquisition
    /// or a cancellation has already created at least one ID and native release
    /// is about to start. It runs before the first `IOPMAssertionRelease`, outside
    /// the owner's ledger lock, so the caller can publish its own
    /// cleanup-in-progress state. It never runs for a complete lease, a creation
    /// failure at the first position or a cancellation that precedes the first
    /// creation. The callback cannot create, adopt or release IDs: ownership and
    /// native policy stay with this owner.
    ///
    /// # Errors
    /// Returns a busy, pending-cleanup, cancellation or creation failure with the
    /// same exact IDs, generations and rollback evidence as
    /// [`AssertionOwner::acquire`].
    ///
    /// # Panics
    /// If `before_rollback` panics, the rollback still runs before the panic
    /// resumes, so unwinding cannot strand created IDs. A release build aborts
    /// instead, and the kernel releases the assertions at process exit.
    pub fn acquire_with_rollback(
        &self,
        request: &AssertionRequest,
        cancellation: &CancellationToken,
        before_rollback: impl FnOnce(),
    ) -> Result<AssertionLease, AcquireError> {
        if cancellation.is_cancelled() {
            return Err(AcquireError::Cancelled {
                rollback: ReleaseReport::default(),
            });
        }
        let generation = self.begin_acquisition()?;
        let mut created = Vec::new();
        for &kind in request.kinds() {
            if cancellation.is_cancelled() {
                return Err(AcquireError::Cancelled {
                    rollback: self.rollback_after_hook(generation, &created, before_rollback),
                });
            }
            match self.state.backend.create(kind, cancellation) {
                Ok(id) => {
                    created.push(OwnedAssertion { kind, id });
                    if cancellation.is_cancelled() {
                        return Err(AcquireError::Cancelled {
                            rollback: self.rollback_after_hook(
                                generation,
                                &created,
                                before_rollback,
                            ),
                        });
                    }
                }
                Err(error) => {
                    return Err(AcquireError::CreationFailed {
                        kind,
                        error,
                        rollback: self.rollback_after_hook(generation, &created, before_rollback),
                    });
                }
            }
        }
        Ok(AssertionLease {
            owner: Arc::clone(&self.state),
            generation,
            evidence: Mutex::new(LeaseEvidence::Live(created)),
        })
    }

    /// One bounded cleanup pass for unresolved IDs.
    ///
    /// Every unresolved ID is attempted at most once per call. A concurrent pass
    /// reports [`ReleaseStatus::InProgress`]; a live lease reports
    /// [`ReleaseStatus::Busy`]; neither claims completion. Exact IDs stay in the
    /// ledger while the pass is stalled. Call this on the next reconcile or
    /// lifecycle transition; there is no background retry.
    #[must_use]
    pub fn cleanup_pending(&self) -> ReleaseReport {
        let items = {
            let mut ledger = lock(&self.state.ledger);
            if ledger.cleaning {
                return ReleaseReport::in_progress();
            }
            if ledger.active.is_some() {
                return ReleaseReport::busy();
            }
            if ledger.unresolved.is_empty() {
                return ReleaseReport::inert();
            }
            ledger.cleaning = true;
            ledger.unresolved.clone()
        };
        let report = self.state.release_all(&items);
        self.state.finish_release(&report);
        report
    }

    /// The unresolved IDs currently owned by the ledger.
    ///
    /// The list stays populated while a cleanup pass is in flight, so callers can
    /// observe the exact owned IDs before native confirmation.
    #[must_use]
    pub fn pending(&self) -> Vec<PendingAssertion> { lock(&self.state.ledger).unresolved.clone() }

    /// Reads the ledger's release state without performing native work.
    ///
    /// [`AssertionOwner::pending`] being empty is not proof: a live or acquiring
    /// generation is not listed there. This copies the coherent ledger state
    /// under one lock, so it reports [`OwnerReleaseState::Released`] only when
    /// there is no active or acquiring generation, no cleanup pass in flight and
    /// no unresolved ID.
    #[must_use]
    pub fn release_state(&self) -> OwnerReleaseState {
        let ledger = lock(&self.state.ledger);
        if ledger.active.is_some() || ledger.cleaning {
            return OwnerReleaseState::Unanswered;
        }
        if ledger.unresolved.is_empty() {
            OwnerReleaseState::Released
        } else {
            OwnerReleaseState::Failed
        }
    }

    fn begin_acquisition(&self) -> Result<u64, AcquireError> {
        let mut ledger = lock(&self.state.ledger);
        if ledger.active.is_some() {
            return Err(AcquireError::Busy);
        }
        if ledger.cleaning && ledger.unresolved.is_empty() {
            return Err(AcquireError::Busy);
        }
        if !ledger.unresolved.is_empty() {
            return Err(AcquireError::PendingCleanup {
                pending: ledger.unresolved.clone(),
            });
        }
        ledger.next_generation += 1;
        let generation = ledger.next_generation;
        ledger.active = Some(generation);
        drop(ledger);
        Ok(generation)
    }

    /// Transitions a failed acquisition to cleanup and performs one pass.
    fn rollback(&self, generation: u64, created: &[OwnedAssertion]) -> ReleaseReport {
        self.state.release(generation, created)
    }

    /// Announces cleanup once, then performs the rollback.
    ///
    /// An empty prefix releases nothing, so the caller has nothing to publish
    /// and the callback is dropped without being called. A panic from the
    /// callback is contained (unwind-enabled builds only; release aborts) so
    /// the rollback still runs; the panic resumes only afterwards.
    fn rollback_after_hook<F: FnOnce()>(
        &self,
        generation: u64,
        created: &[OwnedAssertion],
        before_rollback: F,
    ) -> ReleaseReport {
        if created.is_empty() {
            return self.rollback(generation, created);
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(before_rollback));
        let report = self.rollback(generation, created);
        if let Err(payload) = outcome {
            std::panic::resume_unwind(payload);
        }
        report
    }
}

enum ReleaseClaim {
    Evidence(Vec<PendingAssertion>),
    InProgress,
    Inert,
}

impl OwnerState {
    /// Moves exactly one active generation into cleanup.
    ///
    /// `Inert` means this generation is already complete or superseded and must
    /// never touch the ledger. An empty evidence vector is a real transition for
    /// a failed acquisition that created nothing.
    fn begin_release(&self, generation: u64, owned: &[OwnedAssertion]) -> ReleaseClaim {
        let mut ledger = lock(&self.ledger);
        if ledger.cleaning {
            return ReleaseClaim::InProgress;
        }
        if ledger.active != Some(generation) {
            return ReleaseClaim::Inert;
        }
        ledger.active = None;
        ledger.cleaning = true;
        let evidence: Vec<PendingAssertion> = owned
            .iter()
            .map(|owned| PendingAssertion {
                kind: owned.kind,
                id: owned.id,
                generation,
            })
            .collect();
        ledger.unresolved.clone_from(&evidence);
        drop(ledger);
        ReleaseClaim::Evidence(evidence)
    }

    fn release(&self, generation: u64, owned: &[OwnedAssertion]) -> ReleaseReport {
        match self.begin_release(generation, owned) {
            ReleaseClaim::Evidence(evidence) => {
                let report = self.release_all(&evidence);
                self.finish_release(&report);
                report
            }
            ReleaseClaim::InProgress => ReleaseReport::in_progress(),
            ReleaseClaim::Inert => ReleaseReport::inert(),
        }
    }

    /// Reports the exact unresolved evidence for one generation.
    fn pending_report(&self, generation: u64) -> ReleaseReport {
        let ledger = lock(&self.ledger);
        // A successor can only be acquired after this generation's cleanup was
        // confirmed. Its in-flight cleanup must not revive predecessor evidence.
        if ledger.next_generation > generation {
            return ReleaseReport::inert();
        }
        if ledger.cleaning {
            return ReleaseReport::in_progress();
        }
        let pending: Vec<PendingAssertion> = ledger
            .unresolved
            .iter()
            .filter(|item| item.generation == generation)
            .copied()
            .collect();
        drop(ledger);
        if pending.is_empty() {
            ReleaseReport::inert()
        } else {
            ReleaseReport::with_pending(Vec::new(), pending)
        }
    }

    /// Stores the unresolved remainder of the pass that set `cleaning`.
    fn finish_release(&self, report: &ReleaseReport) {
        let mut ledger = lock(&self.ledger);
        if ledger.cleaning {
            ledger.cleaning = false;
            ledger.unresolved.clone_from(&report.pending);
        }
    }

    fn release_all(&self, evidence: &[PendingAssertion]) -> ReleaseReport {
        release_all_with(self.backend.as_ref(), evidence)
    }
}

fn release_all_with(
    backend: &dyn AssertionBackend,
    evidence: &[PendingAssertion],
) -> ReleaseReport {
    let mut released = Vec::new();
    let mut pending = Vec::new();
    for item in evidence {
        match backend.release(item.kind, item.id) {
            Ok(()) => released.push(item.id),
            Err(_) => pending.push(*item),
        }
    }
    if pending.is_empty() {
        ReleaseReport::complete(released)
    } else {
        ReleaseReport::with_pending(released, pending)
    }
}

impl Drop for OwnerState {
    /// Last-resort cleanup once no module owner remains.
    ///
    /// This performs one bounded best-effort pass. If the pass cannot confirm
    /// every ID, the exact backend, generation and unresolved IDs are retained in
    /// [`RECOVERY_BANK`] for [`AssertionOwner::recover`]. Dropping a handle is not
    /// process exit, so this path never forgets evidence or claims success.
    fn drop(&mut self) {
        let (next_generation, unresolved, cleaning, backend) = {
            let ledger = lock(&self.ledger);
            (
                ledger.next_generation,
                ledger.unresolved.clone(),
                ledger.cleaning,
                Arc::clone(&self.backend),
            )
        };
        if unresolved.is_empty() {
            return;
        }
        // A concurrent pass can only exist while another handle keeps this state
        // alive. If one is nonetheless recorded, retain rather than duplicate a
        // native release.
        let report = if cleaning {
            ReleaseReport::with_pending(Vec::new(), unresolved)
        } else {
            release_all_with(backend.as_ref(), &unresolved)
        };
        if !report.pending.is_empty() {
            lock(&RECOVERY_BANK).insert(self.identity, RetainedOwner {
                backend,
                next_generation,
                unresolved: report.pending,
            });
        }
    }
}

impl fmt::Debug for AssertionOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ledger = lock(&self.state.ledger);
        f.debug_struct("AssertionOwner")
            .field("active", &ledger.active)
            .field("pending", &ledger.unresolved.len())
            .field("cleaning", &ledger.cleaning)
            .finish_non_exhaustive()
    }
}

/// The real IOPM backend.
///
/// Only `kIOReturnSuccess` proves either creation or release. The release
/// documentation does not assign success meanings to any other return, so every
/// nonzero value is retained as unresolved.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemAssertionBackend;

impl AssertionBackend for SystemAssertionBackend {
    fn create(
        &self,
        kind: AssertionKind,
        _cancellation: &CancellationToken,
    ) -> Result<AssertionId, AssertionError> {
        let assertion_type = CFString::from_static_str(kind.assertion_type());
        let assertion_name = CFString::from_static_str(ASSERTION_REASON);
        let mut id: IOPMAssertionID = 0;
        // SAFETY: both strings are live for the call, the level is the documented
        // "on" value, and the out-pointer refers to a writable local.
        let result = unsafe {
            IOPMAssertionCreateWithName(
                Some(&assertion_type),
                kIOPMAssertionLevelOn,
                Some(&assertion_name),
                std::ptr::from_mut(&mut id),
            )
        };
        if result != kIOReturnSuccess {
            return Err(AssertionError::Native { code: result });
        }
        if id == 0 {
            return Err(AssertionError::InvalidId);
        }
        Ok(AssertionId::new(id))
    }

    fn release(&self, _kind: AssertionKind, id: AssertionId) -> Result<(), AssertionError> {
        let result = IOPMAssertionRelease(id.get());
        if result == kIOReturnSuccess {
            Ok(())
        } else {
            Err(AssertionError::Native { code: result })
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::keep_awake::testing::FakeAssertionBackend;

    const KINDS: [AssertionKind; 3] = [
        AssertionKind::Display,
        AssertionKind::Idle,
        AssertionKind::System,
    ];

    fn owner() -> (AssertionOwner, FakeAssertionBackend) {
        let fake = FakeAssertionBackend::new();
        (AssertionOwner::new(Arc::new(fake.clone())), fake)
    }

    fn request() -> AssertionRequest { AssertionRequest::system_wake_lock() }

    #[test]
    fn creation_failure_at_each_position_rolls_back_preceding_ids() {
        let ids = [10, 11, 12];
        for (index, kind) in KINDS.into_iter().enumerate() {
            let (owner, fake) = owner();
            fake.set_ids(ids);
            fake.fail_next_create(kind, -536_870_212);
            let token = CancellationToken::new();

            let error = owner.acquire(&request(), &token).expect_err("creation must fail");
            let AcquireError::CreationFailed {
                kind: failed,
                error: AssertionError::Native { .. },
                rollback,
            } = error
            else {
                panic!("unexpected error: {error:?}");
            };
            assert_eq!(failed, kind);
            assert!(rollback.is_complete());
            let expected: Vec<AssertionId> =
                ids[..index].iter().map(|id| AssertionId::new(*id)).collect();
            assert_eq!(rollback.released(), expected);
            assert_eq!(fake.create_attempts().len(), index + 1);
            let expected_pairs: Vec<(AssertionKind, AssertionId)> =
                KINDS[..index].iter().copied().zip(expected.iter().copied()).collect();
            assert_eq!(fake.release_attempts(), expected_pairs);
            assert!(owner.pending().is_empty());
            let recovered = owner.acquire(&request(), &token).expect("clean rollback must recover");
            assert!(recovered.release().is_complete());
        }
    }

    #[test]
    fn release_failure_at_each_id_retains_exact_pending_and_recovers() {
        let ids = [20, 21, 22];
        for (index, kind) in KINDS.into_iter().enumerate() {
            let (owner, fake) = owner();
            fake.set_ids(ids);
            let token = CancellationToken::new();
            let lease = owner.acquire(&request(), &token).expect("complete acquisition");
            let generation = lease.generation();

            fake.fail_next_release(kind, -536_870_184);
            let report = lease.release();
            assert_eq!(
                report.pending(),
                vec![PendingAssertion {
                    kind,
                    id: AssertionId::new(ids[index]),
                    generation,
                }]
                .as_slice()
            );
            assert_eq!(report.released().len(), 2);
            assert_eq!(report.status(), ReleaseStatus::Pending);
            assert!(!lease.is_confirmed_released());
            assert_eq!(lease.state(), LeaseState::Pending);
            assert!(lease.assertions().is_empty());

            let blocked = owner.acquire(&request(), &token).expect_err("pending blocks");
            let AcquireError::PendingCleanup { pending } = blocked else {
                panic!("unexpected error: {blocked:?}");
            };
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].id, AssertionId::new(ids[index]));

            let recovered = owner.cleanup_pending();
            assert!(recovered.is_complete());
            assert_eq!(recovered.released(), [AssertionId::new(ids[index])]);
            assert!(owner.pending().is_empty());

            let second = owner.acquire(&request(), &token).expect("recovered acquisition");
            assert_eq!(second.generation(), generation + 1);
            assert!(second.release().is_complete());
        }
    }

    #[test]
    fn rollback_failure_keeps_the_exact_id_until_one_recovery_pass() {
        let (owner, fake) = owner();
        fake.set_ids([30, 31, 32]);
        fake.fail_next_create(AssertionKind::System, -536_870_212);
        fake.fail_next_release(AssertionKind::Display, -536_870_184);
        let token = CancellationToken::new();

        let error = owner.acquire(&request(), &token).expect_err("creation must fail");
        let AcquireError::CreationFailed {
            kind: AssertionKind::System,
            rollback,
            ..
        } = error
        else {
            panic!("unexpected error: {error:?}");
        };
        assert!(!rollback.is_complete());
        assert_eq!(
            rollback.pending(),
            vec![PendingAssertion {
                kind: AssertionKind::Display,
                id: AssertionId::new(30),
                generation: 1,
            }]
            .as_slice()
        );
        assert_eq!(owner.pending().len(), 1);

        let blocked = owner.acquire(&request(), &token).expect_err("pending blocks");
        assert!(matches!(blocked, AcquireError::PendingCleanup { .. }));

        let recovered = owner.cleanup_pending();
        assert!(recovered.is_complete());
        assert_eq!(recovered.released(), [AssertionId::new(30)]);
        assert!(owner.pending().is_empty());
        assert!(owner.acquire(&request(), &token).is_ok());
    }

    #[test]
    fn an_in_flight_creation_barrier_exposes_acquiring_and_cancellation() {
        let (owner, fake) = owner();
        fake.set_ids([40, 41, 42]);
        let barrier = fake.block_next_create_of(AssertionKind::Idle);
        let token = CancellationToken::new();
        let worker_owner = owner.clone();
        let worker_token = token.clone();
        let worker = thread::spawn(move || worker_owner.acquire(&request(), &worker_token));

        assert!(barrier.wait_entered(Duration::from_secs(2)));
        let busy = owner.acquire(&request(), &token).expect_err("in-flight blocks");
        assert!(matches!(busy, AcquireError::Busy));

        token.cancel();
        assert!(barrier.release());
        let result = worker.join().expect("worker must not panic");
        let AcquireError::CreationFailed {
            kind: AssertionKind::Idle,
            error: AssertionError::Cancelled,
            rollback,
        } = result.expect_err("cancellation must fail acquisition")
        else {
            panic!("unexpected result");
        };
        assert!(rollback.is_complete());
        assert_eq!(rollback.released(), [AssertionId::new(40)]);
        assert!(owner.pending().is_empty());
    }

    #[test]
    fn an_in_flight_rollback_barrier_exposes_pending_before_release_finishes() {
        let (owner, fake) = owner();
        fake.set_ids([50, 51, 52]);
        fake.fail_next_create(AssertionKind::System, -536_870_212);
        let barrier = fake.block_next_release();
        let token = CancellationToken::new();
        let worker_owner = owner.clone();
        let worker_token = token.clone();
        let worker = thread::spawn(move || worker_owner.acquire(&request(), &worker_token));

        assert!(barrier.wait_entered(Duration::from_secs(2)));
        assert_eq!(owner.pending().len(), 2);
        let blocked = owner.acquire(&request(), &token).expect_err("cleanup blocks");
        assert!(matches!(blocked, AcquireError::PendingCleanup { .. }));

        assert!(barrier.release());
        let result = worker.join().expect("worker must not panic");
        let AcquireError::CreationFailed { rollback, .. } = result.expect_err("creation fails")
        else {
            panic!("unexpected result");
        };
        assert!(rollback.is_complete());
        assert!(owner.pending().is_empty());
    }

    #[test]
    fn a_dropped_lease_attempts_release_but_never_claims_success() {
        let (owner, fake) = owner();
        fake.set_ids([60, 61, 62]);
        let token = CancellationToken::new();
        {
            let lease = owner.acquire(&request(), &token).expect("complete acquisition");
            fake.fail_next_release(AssertionKind::Idle, -536_870_184);
            drop(lease);
        }
        assert_eq!(fake.release_attempts().len(), 3);
        assert_eq!(
            owner.pending(),
            vec![PendingAssertion {
                kind: AssertionKind::Idle,
                id: AssertionId::new(61),
                generation: 1,
            }]
            .as_slice()
        );
        assert!(owner.cleanup_pending().is_complete());
        assert!(owner.pending().is_empty());
    }

    #[test]
    fn a_confirmed_release_is_exactly_once_and_inert_afterwards() {
        let (owner, fake) = owner();
        fake.set_ids([70, 71, 72]);
        let token = CancellationToken::new();
        let first = owner.acquire(&request(), &token).expect("first lease");
        let first_report = first.release();
        assert!(first_report.is_complete());
        assert_eq!(first_report.released().len(), 3);
        assert_eq!(fake.release_attempts().len(), 3);

        let second = owner.acquire(&request(), &token).expect("successor with reused ids");
        assert_eq!(second.generation(), first.generation() + 1);
        assert!(first.release().released().is_empty());
        assert!(first.release().pending().is_empty());
        assert_eq!(fake.release_attempts().len(), 3);

        assert!(second.release().is_complete());
        assert_eq!(fake.release_attempts().len(), 6);
        assert!(first.release().released().is_empty());
    }

    #[test]
    fn failed_cleanup_retries_only_the_unresolved_id_in_one_bounded_pass() {
        let (owner, fake) = owner();
        fake.set_ids([80, 81, 82]);
        let token = CancellationToken::new();
        let lease = owner.acquire(&request(), &token).expect("complete acquisition");
        fake.fail_next_release(AssertionKind::System, -536_870_184);
        assert!(!lease.release().is_complete());
        assert_eq!(fake.release_attempts().len(), 3);

        fake.fail_next_release(AssertionKind::System, -536_870_184);
        let failed = owner.cleanup_pending();
        assert!(failed.released().is_empty());
        assert_eq!(failed.pending().len(), 1);
        assert_eq!(fake.release_attempts().len(), 4);
        assert_eq!(
            fake.release_attempts()[3],
            (AssertionKind::System, AssertionId::new(82))
        );

        let recovered = owner.cleanup_pending();
        assert!(recovered.is_complete());
        assert_eq!(fake.release_attempts().len(), 5);
        assert_eq!(
            fake.release_attempts()[4],
            (AssertionKind::System, AssertionId::new(82))
        );
        assert!(owner.pending().is_empty());
    }

    #[test]
    fn rollback_failure_for_every_preceding_id_retains_exactly_that_id() {
        let ids = [200, 201, 202];
        for (fail_index, fail_kind) in [(1, AssertionKind::Idle), (2, AssertionKind::System)] {
            for release_index in 0..fail_index {
                let (owner, fake) = owner();
                fake.set_ids(ids);
                fake.fail_next_create(fail_kind, -536_870_212);
                fake.fail_next_release(KINDS[release_index], -536_870_184);
                let token = CancellationToken::new();

                let error = owner.acquire(&request(), &token).expect_err("creation must fail");
                let AcquireError::CreationFailed { rollback, .. } = error else {
                    panic!("unexpected error: {error:?}");
                };
                assert_eq!(rollback.status(), ReleaseStatus::Pending);
                assert_eq!(rollback.pending(), [PendingAssertion {
                    kind: KINDS[release_index],
                    id: AssertionId::new(ids[release_index]),
                    generation: 1,
                }]);
                assert_eq!(rollback.released().len(), fail_index - 1);
                let blocked = owner.acquire(&request(), &token).expect_err("pending blocks");
                assert!(matches!(blocked, AcquireError::PendingCleanup { .. }));

                let recovered = owner.cleanup_pending();
                assert!(recovered.is_complete());
                assert_eq!(recovered.released(), [AssertionId::new(ids[release_index])]);
                assert!(owner.pending().is_empty());
                assert!(owner.acquire(&request(), &token).is_ok());
            }
        }
    }

    #[test]
    fn cancellation_after_the_final_create_rolls_back_all_three_ids() {
        let (owner, fake) = owner();
        fake.set_ids([210, 211, 212]);
        fake.cancel_after_create(AssertionKind::System);
        let token = CancellationToken::new();

        let error = owner.acquire(&request(), &token).expect_err("cancellation must fail");
        let AcquireError::Cancelled { rollback } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert!(rollback.is_complete());
        assert_eq!(rollback.released(), [
            AssertionId::new(210),
            AssertionId::new(211),
            AssertionId::new(212),
        ]);
        assert_eq!(fake.release_attempts().len(), 3);
        assert!(owner.pending().is_empty());
    }

    #[test]
    fn cancellation_after_the_final_create_retains_a_failed_system_rollback() {
        let (owner, fake) = owner();
        fake.set_ids([220, 221, 222]);
        fake.cancel_after_create(AssertionKind::System);
        fake.fail_release_always(AssertionKind::System, -536_870_184);
        let token = CancellationToken::new();

        let error = owner.acquire(&request(), &token).expect_err("cancellation must fail");
        let AcquireError::Cancelled { rollback } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert_eq!(rollback.status(), ReleaseStatus::Pending);
        assert_eq!(rollback.pending(), [PendingAssertion {
            kind: AssertionKind::System,
            id: AssertionId::new(222),
            generation: 1,
        }]);
        assert_eq!(rollback.released().len(), 2);
        assert_eq!(owner.pending().len(), 1);

        fake.clear_failures();
        let recovered = owner.cleanup_pending();
        assert!(recovered.is_complete());
        assert_eq!(recovered.released(), [AssertionId::new(222)]);
        assert!(owner.pending().is_empty());
    }

    #[test]
    fn repeated_release_after_failure_reports_pending_without_new_native_calls() {
        let (owner, fake) = owner();
        fake.set_ids([230, 231, 232]);
        let token = CancellationToken::new();
        let lease = owner.acquire(&request(), &token).expect("complete acquisition");
        let generation = lease.generation();
        fake.fail_next_release(AssertionKind::Idle, -536_870_184);

        let first = lease.release();
        assert_eq!(first.status(), ReleaseStatus::Pending);
        assert_eq!(first.released().len(), 2);
        assert_eq!(fake.release_attempts().len(), 3);

        let repeated = lease.release();
        assert_eq!(repeated.status(), ReleaseStatus::Pending);
        assert!(!repeated.is_complete());
        assert!(repeated.released().is_empty());
        assert_eq!(repeated.pending(), [PendingAssertion {
            kind: AssertionKind::Idle,
            id: AssertionId::new(231),
            generation,
        }]);
        assert_eq!(lease.state(), LeaseState::Pending);
        assert_eq!(fake.release_attempts().len(), 3);

        let recovered = owner.cleanup_pending();
        assert!(recovered.is_complete());
        assert_eq!(recovered.released(), [AssertionId::new(231)]);
        assert_eq!(fake.release_attempts().len(), 4);
        assert_eq!(lease.state(), LeaseState::Released);
        assert!(lease.is_confirmed_released());

        let successor = owner.acquire(&request(), &token).expect("successor reuses the IDs");
        let barrier = fake.block_next_release();
        thread::scope(|scope| {
            let worker = scope.spawn(|| successor.release());
            assert!(barrier.wait_entered(Duration::from_secs(2)));
            assert!(lease.is_confirmed_released());
            assert_eq!(lease.release().status(), ReleaseStatus::Inert);
            assert_eq!(fake.release_attempts().len(), 5);
            assert!(barrier.release());
            assert!(worker.join().expect("successor release").is_complete());
        });
        assert_eq!(fake.release_attempts().len(), 7);
    }

    #[test]
    fn repeated_cleanup_failures_keep_the_exact_id_until_one_success() {
        let (owner, fake) = owner();
        fake.set_ids([240, 241, 242]);
        let token = CancellationToken::new();
        let lease = owner.acquire(&request(), &token).expect("complete acquisition");
        fake.fail_release_always(AssertionKind::System, -536_870_184);
        assert_eq!(lease.release().status(), ReleaseStatus::Pending);
        let expected = [PendingAssertion {
            kind: AssertionKind::System,
            id: AssertionId::new(242),
            generation: lease.generation(),
        }];

        for _ in 0..2 {
            let failed = owner.cleanup_pending();
            assert_eq!(failed.status(), ReleaseStatus::Pending);
            assert!(!failed.is_complete());
            assert_eq!(failed.pending(), expected);
            assert_eq!(owner.pending(), expected);
        }
        fake.clear_failures();
        let recovered = owner.cleanup_pending();
        assert!(recovered.is_complete());
        assert_eq!(recovered.released(), [AssertionId::new(242)]);
        assert!(owner.pending().is_empty());
    }

    #[test]
    fn concurrent_release_reports_in_progress_without_duplicate_native_calls() {
        let (owner, fake) = owner();
        fake.set_ids([250, 251, 252]);
        let token = CancellationToken::new();
        let lease = owner.acquire(&request(), &token).expect("complete acquisition");
        let barrier = fake.block_next_release();

        thread::scope(|scope| {
            let worker = scope.spawn(|| lease.release());
            assert!(barrier.wait_entered(Duration::from_secs(2)));

            let during = lease.release();
            assert_eq!(during.status(), ReleaseStatus::InProgress);
            assert!(!during.is_complete());
            assert_eq!(lease.state(), LeaseState::Releasing);
            assert_eq!(owner.pending().len(), 3);
            assert!(matches!(
                owner.acquire(&request(), &token),
                Err(AcquireError::PendingCleanup { .. })
            ));
            assert_eq!(fake.release_attempts().len(), 1);

            assert!(barrier.release());
            let report = worker.join().expect("release worker must not panic");
            assert!(report.is_complete());
            assert_eq!(report.released().len(), 3);
        });

        assert_eq!(lease.state(), LeaseState::Released);
        assert!(lease.is_confirmed_released());
        assert!(lease.release().is_complete());
        assert_eq!(fake.release_attempts().len(), 3);
    }

    #[test]
    fn stalled_cleanup_keeps_pending_evidence_and_blocks_acquisition() {
        let (owner, fake) = owner();
        fake.set_ids([260, 261, 262]);
        let token = CancellationToken::new();
        let lease = owner.acquire(&request(), &token).expect("complete acquisition");
        fake.fail_release_always(AssertionKind::System, -536_870_184);
        assert_eq!(lease.release().status(), ReleaseStatus::Pending);
        let expected = [PendingAssertion {
            kind: AssertionKind::System,
            id: AssertionId::new(262),
            generation: lease.generation(),
        }];
        let barrier = fake.block_next_release();

        thread::scope(|scope| {
            let worker = scope.spawn(|| owner.cleanup_pending());
            assert!(barrier.wait_entered(Duration::from_secs(2)));

            let during = owner.cleanup_pending();
            assert_eq!(during.status(), ReleaseStatus::InProgress);
            assert!(!during.is_complete());
            assert_eq!(owner.pending(), expected);
            assert!(matches!(
                owner.acquire(&request(), &token),
                Err(AcquireError::PendingCleanup { .. })
            ));

            assert!(barrier.release());
            let report = worker.join().expect("cleanup worker must not panic");
            assert_eq!(report.status(), ReleaseStatus::Pending);
            assert_eq!(report.pending(), expected);
        });

        fake.clear_failures();
        let recovered = owner.cleanup_pending();
        assert!(recovered.is_complete());
        assert_eq!(recovered.released(), [AssertionId::new(262)]);
        assert!(owner.pending().is_empty());
    }

    #[test]
    fn a_cancelled_release_barrier_keeps_the_exact_id_pending() {
        let (owner, fake) = owner();
        fake.set_ids([275, 276, 277]);
        let token = CancellationToken::new();
        let lease = owner.acquire(&request(), &token).expect("complete acquisition");
        let barrier = fake.block_next_release();

        thread::scope(|scope| {
            let worker = scope.spawn(|| lease.release());
            assert!(barrier.wait_entered(Duration::from_secs(2)));
            assert!(barrier.cancel());
            let report = worker.join().expect("release worker must not panic");
            assert_eq!(report.status(), ReleaseStatus::Pending);
            assert_eq!(report.pending(), [PendingAssertion {
                kind: AssertionKind::Display,
                id: AssertionId::new(275),
                generation: lease.generation(),
            }]);
        });
        assert_eq!(owner.pending().len(), 1);
        assert!(owner.cleanup_pending().is_complete());
        assert!(owner.pending().is_empty());
    }

    #[test]
    fn a_timed_out_release_barrier_never_looks_like_success() {
        let (owner, fake) = owner();
        fake.set_ids([278, 279, 280]);
        fake.set_barrier_timeout(Duration::from_millis(20));
        let token = CancellationToken::new();
        let lease = owner.acquire(&request(), &token).expect("complete acquisition");
        let _barrier = fake.block_next_release();

        let report = lease.release();
        assert_eq!(report.status(), ReleaseStatus::Pending);
        assert_eq!(report.pending()[0].id, AssertionId::new(278));
        assert!(!report.is_complete());
        assert_eq!(lease.state(), LeaseState::Pending);
        assert!(owner.cleanup_pending().is_complete());
    }

    #[test]
    fn an_inert_owner_reports_released_without_native_work() {
        let (owner, fake) = owner();
        assert_eq!(owner.release_state(), OwnerReleaseState::Released);
        assert!(fake.create_attempts().is_empty());
        assert!(fake.release_attempts().is_empty());
    }

    #[test]
    fn a_live_lease_is_unanswered_even_with_no_pending_ids() {
        let (owner, _fake) = owner();
        let token = CancellationToken::new();
        let lease = owner.acquire(&request(), &token).expect("complete acquisition");

        assert!(owner.pending().is_empty(), "a live lease is not pending cleanup");
        assert_eq!(owner.release_state(), OwnerReleaseState::Unanswered);

        assert!(lease.release().is_complete());
        assert_eq!(owner.release_state(), OwnerReleaseState::Released);
    }

    #[test]
    fn an_in_flight_acquisition_is_unanswered() {
        let (owner, fake) = owner();
        let barrier = fake.block_next_create();
        let token = CancellationToken::new();

        thread::scope(|scope| {
            let worker = scope.spawn(|| owner.acquire(&request(), &token));
            assert!(barrier.wait_entered(Duration::from_secs(2)));

            assert_eq!(owner.release_state(), OwnerReleaseState::Unanswered);

            assert!(barrier.release());
            let lease =
                worker.join().expect("acquisition worker must not panic").expect("complete");
            assert!(lease.release().is_complete());
        });

        assert_eq!(owner.release_state(), OwnerReleaseState::Released);
    }

    #[test]
    fn an_in_flight_release_is_unanswered() {
        let (owner, fake) = owner();
        let token = CancellationToken::new();
        let lease = owner.acquire(&request(), &token).expect("complete acquisition");
        let barrier = fake.block_next_release();

        thread::scope(|scope| {
            let worker = scope.spawn(|| lease.release());
            assert!(barrier.wait_entered(Duration::from_secs(2)));

            assert_eq!(owner.release_state(), OwnerReleaseState::Unanswered);

            assert!(barrier.release());
            assert!(worker.join().expect("release worker must not panic").is_complete());
        });

        assert_eq!(owner.release_state(), OwnerReleaseState::Released);
    }

    #[test]
    fn a_confirmed_unresolved_cleanup_failure_is_reported_as_failed() {
        let (owner, fake) = owner();
        let token = CancellationToken::new();
        let lease = owner.acquire(&request(), &token).expect("complete acquisition");
        fake.fail_release_always(AssertionKind::Display, -536_870_184);

        assert_eq!(lease.release().status(), ReleaseStatus::Pending);
        assert!(!owner.pending().is_empty());
        assert_eq!(owner.release_state(), OwnerReleaseState::Failed);

        fake.clear_failures();
        assert!(owner.cleanup_pending().is_complete());
        assert_eq!(owner.release_state(), OwnerReleaseState::Released);
    }

    #[test]
    fn cleanup_pending_reports_busy_while_a_lease_is_live() {
        let (owner, fake) = owner();
        fake.set_ids([270, 271, 272]);
        let token = CancellationToken::new();
        let lease = owner.acquire(&request(), &token).expect("complete acquisition");

        let report = owner.cleanup_pending();
        assert_eq!(report.status(), ReleaseStatus::Busy);
        assert!(!report.is_complete());
        assert!(owner.pending().is_empty());
        assert!(lease.release().is_complete());
    }

    #[test]
    fn final_owner_drop_retains_failed_release_for_targeted_recovery() {
        let (owner, fake) = owner();
        fake.set_ids([280, 281, 282]);
        fake.fail_release_always(AssertionKind::Idle, -536_870_184);
        let identity = owner.identity();
        let token = CancellationToken::new();
        let lease = owner.acquire(&request(), &token).expect("complete acquisition");
        assert_eq!(lease.release().status(), ReleaseStatus::Pending);
        drop(lease);
        assert_eq!(fake.release_attempts().len(), 3);

        drop(owner);
        // The final-owner Drop attempted the retained ID once more and kept the
        // failure accountable instead of freeing the backend.
        assert_eq!(fake.release_attempts().len(), 4);

        let recovered = AssertionOwner::recover(identity)
            .expect("failed final-owner evidence must be retained");
        assert_eq!(recovered.pending(), [PendingAssertion {
            kind: AssertionKind::Idle,
            id: AssertionId::new(281),
            generation: 1,
        }]);
        // Recovery is single-owner: a second lookup cannot touch the successor.
        assert!(AssertionOwner::recover(identity).is_none());

        fake.clear_failures();
        let cleanup = recovered.cleanup_pending();
        assert!(cleanup.is_complete());
        assert_eq!(cleanup.released(), [AssertionId::new(281)]);
        assert_eq!(fake.release_attempts().len(), 5);

        let successor = recovered.acquire(&request(), &token).expect("successor acquisition");
        assert_eq!(successor.generation(), 2);
        assert!(successor.assertions().iter().any(|owned| owned.id == AssertionId::new(281)));
        assert!(AssertionOwner::recover(identity).is_none());
        assert!(successor.release().is_complete());
    }

    #[test]
    fn handles_are_send_and_sync_without_a_native_object() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AssertionOwner>();
        assert_send_sync::<AssertionLease>();
        assert_send_sync::<SystemAssertionBackend>();
        assert_send_sync::<AssertionRequest>();
        assert_send_sync::<PendingAssertion>();
    }

    #[test]
    fn the_fixed_reason_and_three_assertion_request_are_stable() {
        assert_eq!(ASSERTION_REASON, "Stache requested system wake lock");
        assert_eq!(request().kinds(), &[
            AssertionKind::Display,
            AssertionKind::Idle,
            AssertionKind::System,
        ]);
    }

    #[test]
    fn cancellation_before_the_first_native_call_never_touches_the_backend() {
        let (owner, fake) = owner();
        let token = CancellationToken::new();
        token.cancel();
        let error = owner.acquire(&request(), &token).expect_err("cancelled");
        assert_eq!(error, AcquireError::Cancelled {
            rollback: ReleaseReport::default()
        });
        assert!(fake.create_attempts().is_empty());
        assert!(fake.release_attempts().is_empty());
    }

    #[test]
    fn a_partial_creation_failure_invokes_the_hook_once_before_any_release() {
        let ids = [300, 301, 302];
        for (fail_kind, prefix) in [
            (AssertionKind::Idle, 1_usize),
            (AssertionKind::System, 2_usize),
        ] {
            let (owner, fake) = owner();
            fake.set_ids(ids);
            fake.fail_next_create(fail_kind, -536_870_212);
            let token = CancellationToken::new();
            let calls = std::cell::Cell::new(0_u32);

            let error = owner
                .acquire_with_rollback(&request(), &token, || {
                    calls.set(calls.get() + 1);
                    assert_eq!(
                        fake.created().len(),
                        prefix,
                        "the hook must run after the partial prefix was created"
                    );
                    assert!(
                        fake.release_attempts().is_empty(),
                        "the hook must run before the first native release"
                    );
                })
                .expect_err("creation must fail");

            assert_eq!(calls.get(), 1);
            let AcquireError::CreationFailed { rollback, .. } = error else {
                panic!("unexpected error: {error:?}");
            };
            assert!(rollback.is_complete());
            assert_eq!(rollback.released().len(), prefix);
            assert_eq!(fake.release_attempts().len(), prefix);
            assert!(owner.pending().is_empty());
            assert!(owner.acquire(&request(), &token).is_ok());
        }
    }

    #[test]
    fn a_first_position_creation_failure_never_invokes_the_hook_or_releases() {
        let (owner, fake) = owner();
        fake.set_ids([303, 304, 305]);
        fake.fail_next_create(AssertionKind::Display, -536_870_212);
        let token = CancellationToken::new();
        let calls = std::cell::Cell::new(0_u32);

        let error = owner
            .acquire_with_rollback(&request(), &token, || calls.set(calls.get() + 1))
            .expect_err("creation must fail");

        let AcquireError::CreationFailed { kind, error, rollback } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert_eq!(kind, AssertionKind::Display);
        assert_eq!(error, AssertionError::Native { code: -536_870_212 });
        assert!(rollback.is_complete());
        assert!(rollback.released().is_empty());
        assert_eq!(calls.get(), 0);
        assert!(fake.release_attempts().is_empty());
        assert!(owner.pending().is_empty());
    }

    #[test]
    fn cancellation_before_the_first_create_never_invokes_the_hook() {
        let (owner, fake) = owner();
        let token = CancellationToken::new();
        token.cancel();
        let calls = std::cell::Cell::new(0_u32);

        let error = owner
            .acquire_with_rollback(&request(), &token, || calls.set(calls.get() + 1))
            .expect_err("cancelled");

        assert_eq!(error, AcquireError::Cancelled {
            rollback: ReleaseReport::default()
        });
        assert_eq!(calls.get(), 0);
        assert!(fake.create_attempts().is_empty());
        assert!(fake.release_attempts().is_empty());
    }

    #[test]
    fn a_cancelled_first_create_invokes_no_hook_and_releases_nothing() {
        let (owner, fake) = owner();
        fake.set_ids([306, 307, 308]);
        let barrier = fake.block_next_create_of(AssertionKind::Display);
        let token = CancellationToken::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_owner = owner.clone();
        let worker_token = token.clone();
        let worker_calls = Arc::clone(&calls);
        let worker = thread::spawn(move || {
            worker_owner.acquire_with_rollback(&request(), &worker_token, || {
                worker_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
        });

        assert!(barrier.wait_entered(Duration::from_secs(2)));
        token.cancel();
        assert!(barrier.release());
        let error = worker
            .join()
            .expect("worker must not panic")
            .expect_err("cancelled create must fail acquisition");

        let AcquireError::CreationFailed {
            kind: AssertionKind::Display,
            error: AssertionError::Cancelled,
            rollback,
        } = error
        else {
            panic!("unexpected error: {error:?}");
        };
        assert!(rollback.is_complete());
        assert!(rollback.released().is_empty());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(fake.release_attempts().is_empty());
        assert!(owner.pending().is_empty());
        let fresh = CancellationToken::new();
        assert!(owner.acquire(&request(), &fresh).is_ok());
    }

    #[test]
    fn cancellation_after_the_final_create_invokes_the_hook_once_before_rollback() {
        let (owner, fake) = owner();
        fake.set_ids([320, 321, 322]);
        fake.cancel_after_create(AssertionKind::System);
        let token = CancellationToken::new();
        let calls = std::cell::Cell::new(0_u32);

        let error = owner
            .acquire_with_rollback(&request(), &token, || {
                calls.set(calls.get() + 1);
                assert_eq!(fake.created().len(), 3);
                assert!(fake.release_attempts().is_empty());
            })
            .expect_err("cancellation must fail acquisition");

        assert_eq!(calls.get(), 1);
        let AcquireError::Cancelled { rollback } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert!(rollback.is_complete());
        assert_eq!(rollback.released(), [
            AssertionId::new(320),
            AssertionId::new(321),
            AssertionId::new(322),
        ]);
        assert_eq!(fake.release_attempts().len(), 3);
        assert!(owner.pending().is_empty());
    }

    #[test]
    fn a_complete_acquisition_never_invokes_the_hook() {
        let (owner, fake) = owner();
        fake.set_ids([330, 331, 332]);
        let token = CancellationToken::new();
        let calls = std::cell::Cell::new(0_u32);

        let lease = owner
            .acquire_with_rollback(&request(), &token, || calls.set(calls.get() + 1))
            .expect("complete acquisition");

        assert_eq!(calls.get(), 0);
        assert_eq!(lease.state(), LeaseState::Live);
        assert_eq!(lease.assertions().len(), 3);
        assert!(lease.release().is_complete());
    }

    #[test]
    fn a_panicking_hook_still_rolls_back_every_created_id() {
        let (owner, fake) = owner();
        fake.set_ids([340, 341, 342]);
        fake.fail_next_create(AssertionKind::System, -536_870_212);
        let token = CancellationToken::new();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            owner.acquire_with_rollback(&request(), &token, || panic!("publication failed"))
        }));
        assert!(result.is_err(), "the hook panic must resume after cleanup");

        assert_eq!(fake.release_attempts(), [
            (AssertionKind::Display, AssertionId::new(340)),
            (AssertionKind::Idle, AssertionId::new(341)),
        ]);
        assert!(owner.pending().is_empty());
        let recovered = owner.acquire(&request(), &token).expect("the ledger must be usable");
        assert!(recovered.release().is_complete());
    }

    #[test]
    fn the_hook_never_changes_rollback_failure_ownership() {
        let (owner, fake) = owner();
        fake.set_ids([350, 351, 352]);
        fake.fail_next_create(AssertionKind::System, -536_870_212);
        fake.fail_next_release(AssertionKind::Display, -536_870_184);
        let token = CancellationToken::new();
        let calls = std::cell::Cell::new(0_u32);

        let error = owner
            .acquire_with_rollback(&request(), &token, || calls.set(calls.get() + 1))
            .expect_err("creation must fail");

        assert_eq!(calls.get(), 1);
        let AcquireError::CreationFailed { rollback, .. } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert_eq!(rollback.status(), ReleaseStatus::Pending);
        assert_eq!(rollback.pending(), [PendingAssertion {
            kind: AssertionKind::Display,
            id: AssertionId::new(350),
            generation: 1,
        }]);
        assert_eq!(fake.release_attempts().len(), 2);
        assert!(matches!(
            owner.acquire(&request(), &token),
            Err(AcquireError::PendingCleanup { .. })
        ));

        let recovered = owner.cleanup_pending();
        assert!(recovered.is_complete());
        assert_eq!(recovered.released(), [AssertionId::new(350)]);
        assert!(owner.pending().is_empty());
    }
}
