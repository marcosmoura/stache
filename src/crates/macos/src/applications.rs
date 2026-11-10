//! Application observation, query and control on the main thread.
//!
//! [`ApplicationControl`] is the safe, `Send`-facing handle a module keeps.
//! Every Cocoa object stays main-thread-owned: the observer registration lives
//! in a thread-local entry keyed by [`BackendHandle`], and the raw
//! `NSRunningApplication` instances are retained there too. Callers only ever
//! see the opaque [`ApplicationId`].
//!
//! Install, snapshot, termination and opening are dispatched through the host's
//! [`MainThreadExecutor`]. Off-main callers wait for an explicit acknowledgement
//! for at most [`ACKNOWLEDGEMENT_BUDGET`]; an inline main-thread call completes
//! directly because the dispatcher runs the job without waiting. `install` is
//! the one wait that also observes lifecycle cancellation: it rechecks the
//! [`CancellationToken`] at least every [`CANCELLATION_QUANTUM`], so a cancelled
//! start returns promptly even when the main queue is never drained.
//!
//! Identity is never bare PID or bundle equality. A token names one retained
//! instance, and an action revalidates that exact instance through `isEqual:`
//! against the current workspace list and its terminated flag before touching
//! it. A reused PID therefore cannot address an unrelated process. Every token
//! and instance also carries its [`BackendHandle`] and generation, so two
//! independent controls with the same generation number never address each
//! other's tokens. The token stays opaque; [`ApplicationObservation::bundle_identifier`]
//! classifies a notification without moving a Cocoa object off-main.
//!
//! Two more generation-scoped queries resolve a user-chosen replacement without
//! any module-side filesystem work: [`ApplicationObservation::bundle_identifier_at_path`]
//! reads the identifier of an application bundle and
//! [`ApplicationObservation::application_path`] looks up the installed path for
//! an identifier. Both answer `Ok(None)` for "no such bundle" or "not
//! installed", and only report an error when the query itself could not run.
//!
//! ## Frontmost, graceful quit and display hint
//!
//! Three additive capabilities extend the same generation and owner maps; the
//! will-launch observation and the force-terminating
//! [`ApplicationObservation::terminate`] are unchanged:
//!
//! - [`frontmost`]: workspace activation observation, a nonblocking published
//!   [`FrontmostSnapshot`] and the main [`FrontmostPreflight`].
//! - [`graceful`]: [`ApplicationObservation::terminate_gracefully`], which calls
//!   only `terminate` under the existing [`IssuancePermit`] plus a typed
//!   [`PressAuthorization`].
//! - [`display`]: [`ApplicationObservation::display_for`], a best-effort display
//!   hint from public window geometry.
//! - [`visibility`]: [`ApplicationObservation::hide_exact`] and
//!   [`ApplicationObservation::unhide_exact`], exact-instance hide/unhide under
//!   the same issuance model as [`ApplicationObservation::activate_exact`].
//! - [`lifecycle`]: [`ApplicationObservation::watch_lifecycle`], pure launch,
//!   terminate, hide and unhide events carrying the exact [`ApplicationId`].
//!
//! ## Linearization
//!
//! A narrow generation gate (`Mutex<GenerationState>`) is the commit point
//! shared by acceptance, generation invalidation, observer claim/cleanup and
//! irreversible native issuance. The gate is held only for a few field reads
//! and writes, plus one synchronous native send in [`IssuancePermit::issue`].
//! It is never held across a backend preflight, a user completion callback, a
//! dispatch or acknowledgement wait, observer install/removal or a worker wait,
//! so a native backend may deliver a completion synchronously without
//! deadlocking: the result is buffered and the user callback runs only after the
//! gate is released.
//!
//! Termination and opening are two-phase. The backend performs every
//! non-committing step first: instance identity, path and configuration checks,
//! and any test barrier. It then calls [`IssuancePermit::issue`] at the exact
//! native send site. `issue` revalidates the accepted handle, generation and
//! revision and the request deadline under the gate, and runs only the immediate
//! native call while holding it. Either the request is issued first and a later
//! stop/reconfigure cannot cancel it, or the newer decision commits first and
//! the request is never issued; there is no window in which a stale request is
//! sent after a completed stop.
//!
//! A request whose acknowledgement timed out is marked abandoned. If it had
//! already issued, it may finish after the timeout and its result is still
//! reported. If it had not, [`IssuancePermit::issue`] returns `None` and no
//! native call happens. A synchronous open completion that arrives while the
//! gate is held is buffered and delivered only after the gate is released.
//!
//! A will-launch notification can also be delivered synchronously from inside
//! the same native send that holds the gate. The system backend therefore
//! interns the retained instance on the spot and defers only the pure
//! [`ApplicationId`] and its cloned `Send` sink to a later main dispatch turn.
//! The sink rechecks the generation when the deferred delivery runs, so a
//! removed or superseded observation stays inert.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError, Weak};
use std::time::{Duration, Instant};

use block2::{DynBlock, RcBlock};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2_app_kit::{
    NSApplicationActivationOptions, NSApplicationActivationPolicy, NSRunningApplication, NSScreen,
    NSWorkspace, NSWorkspaceApplicationKey, NSWorkspaceDidActivateApplicationNotification,
    NSWorkspaceOpenConfiguration, NSWorkspaceWillLaunchApplicationNotification,
};
use objc2_foundation::{
    NSBundle, NSError, NSNotification, NSNotificationCenter, NSOperationQueue, NSString, NSURL,
};
use stache_sdk::{CancellationToken, MainThreadExecutor, ThreadError};

pub mod activation;
pub mod display;
pub mod frontmost;
pub mod graceful;
pub mod lifecycle;
mod pid;
pub mod visibility;

pub use activation::{ActivationAuthorization, ActivationCancel, ActivationOutcome};
pub use display::{
    DisplayHint, DisplayRect, DisplaySource, ScreenRecord, WindowRecord, appkit_rect_from_cg,
    select_display,
};
pub use frontmost::{
    ActivationSink, FINDER_BUNDLE_IDENTIFIER, FrontmostApplication, FrontmostClass,
    FrontmostListener, FrontmostPreflight, FrontmostSnapshot, FrontmostState, FrontmostWatch,
    PreflightRejection, RefreshTicket, Unconfirmed, classify_preflight,
};
pub use graceful::{GracefulOutcome, PressAuthorization, PressCancel};
pub use lifecycle::{LifecycleEvent, LifecycleKind, LifecycleSink};
pub use visibility::{VisibilityAuthorization, VisibilityOperation, VisibilityOutcome};

/// How long one dispatched native operation may wait for its acknowledgement.
///
/// The app's smoke harness waits up to five seconds for one main-thread hop
/// (`src/crates/app/src/smoke.rs`), so a single native acknowledgement must
/// finish well inside that budget. The main dispatch queue is FIFO and normally
/// acknowledges in microseconds. This is a per-hop bound, not a shutdown SLA:
/// the host has no global lifecycle timeout.
pub const ACKNOWLEDGEMENT_BUDGET: Duration = Duration::from_secs(2);

/// The longest an install wait sleeps before rechecking lifecycle cancellation.
///
/// [`CancellationToken`] has no wait/drain hook on native dispatch, so the wait
/// is polled at this quantum. A cancelled start therefore returns within at most
/// one quantum after cancellation, subject to scheduling. The value is small
/// enough for lifecycle responsiveness and large enough to avoid a busy loop.
pub const CANCELLATION_QUANTUM: Duration = Duration::from_millis(10);

/// How long one commit-gate acquisition sleeps before retrying.
///
/// The gate is only held for a few field updates, or for one immediate native
/// send, so an uncontended acquisition never sleeps. A control-side operation
/// that cannot take the gate within its budget (for example while a stalled
/// native call still holds it) reports [`ApplicationError::TimedOut`] instead of
/// waiting indefinitely.
const GATE_RETRY: Duration = Duration::from_millis(1);

/// One running application instance, valid only for the generation that
/// produced it.
///
/// The value is opaque and `Copy`; it carries no pointer and can safely cross
/// threads. The retained native object stays on the main thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ApplicationId {
    handle: BackendHandle,
    generation: u64,
    token: u64,
}

impl ApplicationId {
    /// The reserved id carried by `LifecycleKind::TerminatedUnresolved`.
    /// `intern` never allocates it, so it names no instance.
    pub(crate) const fn unresolved(handle: BackendHandle, generation: u64) -> Self {
        Self {
            handle,
            generation,
            token: u64::MAX,
        }
    }
}

#[cfg(any(test, feature = "testing"))]
impl ApplicationId {
    /// A synthetic id for fakes: equal tokens are equal ids, different tokens
    /// are different application instances.
    #[must_use]
    pub fn for_testing(token: u64) -> Self {
        static SEED: BackendHandle = BackendHandle(u64::MAX);
        Self {
            handle: SEED,
            generation: 1,
            token,
        }
    }
}

/// An opaque key a backend uses to keep main-thread-owned state per control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BackendHandle(u64);

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

impl BackendHandle {
    fn allocate() -> Self { Self(NEXT_HANDLE.fetch_add(1, Ordering::Relaxed)) }
}

/// The accepted-generation and accepted-configuration snapshot a queued action
/// must still match at its commit point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptedActionToken {
    handle: BackendHandle,
    generation: u64,
    revision: u64,
}

/// Whether an accepted action was still current when the native side ran it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenRequest {
    /// The open request was issued to `AppKit`. Completion still reports success
    /// or failure.
    Issued,
    /// The accepted token was stale, so nothing was issued.
    Stale,
}

/// The truth about a termination request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminateOutcome {
    /// `forceTerminate` returned `true`: the request was accepted, not that the
    /// process exited.
    Requested,
    /// The exact instance is no longer running, so nothing was sent. A foreign
    /// instance from another control also reports this: nothing was touched.
    NotRunning,
    /// `forceTerminate` returned `false`; macOS refused the request.
    Refused,
    /// The accepted token was stale, so nothing was sent.
    Stale,
}

/// A failed native application operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplicationError {
    /// A native operation was invoked off the main thread.
    NotMainThread,
    /// The host dispatcher rejected the job.
    Dispatch(String),
    /// The main run loop is unavailable.
    Unavailable,
    /// No acknowledgement arrived inside [`ACKNOWLEDGEMENT_BUDGET`], or the
    /// commit gate could not be observed inside that budget.
    TimedOut,
    /// The operation was cancelled before it could run.
    Cancelled,
    /// An observation is already installed on this control.
    AlreadyInstalled,
    /// The observation generation is no longer active.
    NotInstalled,
    /// Observation installation failed.
    ObservationFailed(String),
    /// Observation removal failed.
    RemovalFailed(String),
    /// Opening the replacement application failed.
    OpenFailed(String),
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMainThread => f.write_str("native application work requires the main thread"),
            Self::Dispatch(message) => write!(f, "cannot dispatch to the main thread: {message}"),
            Self::Unavailable => f.write_str("the main run loop is unavailable"),
            Self::TimedOut => f.write_str("timed out waiting for a native acknowledgement"),
            Self::Cancelled => f.write_str("the native operation was cancelled"),
            Self::AlreadyInstalled => {
                f.write_str("an application observation is already installed")
            }
            Self::NotInstalled => f.write_str("the application observation is no longer active"),
            Self::ObservationFailed(message) => {
                write!(f, "cannot install application observation: {message}")
            }
            Self::RemovalFailed(message) => {
                write!(f, "cannot remove application observation: {message}")
            }
            Self::OpenFailed(message) => write!(f, "cannot open application: {message}"),
        }
    }
}

impl std::error::Error for ApplicationError {}

/// A failed open request and whether it may already have been issued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenFailure {
    /// Why the request failed.
    pub error: ApplicationError,
    /// Whether the native request was issued before the failure. `false` means
    /// it can never be issued; `true` means it was and may still complete.
    pub issued: bool,
}

impl OpenFailure {
    fn new(error: ApplicationError, request: &RequestCommit) -> Self {
        Self {
            error,
            issued: !request.is_unissued(),
        }
    }
}

/// Identifies one open request for diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenCompletionContext {
    /// The control that issued the request.
    pub handle: BackendHandle,
    /// The observation generation that issued it.
    pub generation: u64,
    /// The accepted revision it was issued under.
    pub revision: u64,
    /// The per-generation request number.
    pub request: u64,
}

/// The delivered result of one open request.
///
/// The state is decided at delivery time, under the commit gate, not when the
/// native call was made. `Current` means the accepted revision and request were
/// still the newest for the live generation. `Stale` means a stop, successor
/// generation, newer accepted revision or newer open request has committed
/// since; the result is diagnostic only and has already changed no current
/// pending bookkeeping. Both variants carry the request context so a caller can
/// log the stale result and key its per-request pending bookkeeping.
#[derive(Debug, PartialEq, Eq)]
pub enum OpenCompletion {
    /// The request is still the current one for this observation.
    Current(OpenCompletionContext, Result<(), ApplicationError>),
    /// The request is no longer current; log or ignore it.
    Stale(OpenCompletionContext, Result<(), ApplicationError>),
}

/// The native completion passed to [`ApplicationBackend::open`].
///
/// It is delivered through the host dispatcher. If the dispatcher rejects the
/// delivery job the completion cannot be run and the result is lost; this crate
/// has no logging channel to report that separately.
pub type NativeOpenCompletion = Box<dyn FnOnce(Result<(), ApplicationError>) + Send + 'static>;

/// The one-shot user callback stored for an open request.
type OpenCompletionSlot = Box<dyn FnOnce(OpenCompletion) + Send>;

/// The injectable native operations.
///
/// Implementations are called on the main thread by [`ApplicationControl`] and
/// must keep every `!Send` native object there. The production implementation is
/// [`SystemApplicationBackend`]; `testing::FakeApplicationBackend` is the
/// deterministic test double.
pub trait ApplicationBackend: Send + Sync {
    /// Registers will-launch observation for one generation.
    ///
    /// # Callback contract
    ///
    /// `notify` is delivered on the main thread, but never synchronously from
    /// inside the native callback that observed the launch. A will-launch
    /// notification can arrive during a native send whose [`IssuancePermit`]
    /// still holds the commit gate, so a backend must hand the pure
    /// [`ApplicationId`] and its `Send` sink to a later main-thread turn
    /// instead of calling the sink inline. The system backend uses the main
    /// dispatch queue; a backend that delivers inline would re-enter the gate
    /// on the same thread and deadlock. The sink rechecks the generation when
    /// the deferred delivery finally runs.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main or a registration
    /// failure. The caller owns rollback.
    fn observe(
        &self,
        handle: BackendHandle,
        generation: u64,
        notify: Arc<dyn Fn(ApplicationId) + Send + Sync>,
    ) -> Result<(), ApplicationError>;

    /// Removes the observer for one generation. Idempotent, and a mismatched
    /// successor generation is left untouched.
    ///
    /// # Errors
    /// Returns a removal failure; the caller keeps the observation invalid and
    /// retries cleanup.
    fn unobserve(&self, handle: BackendHandle, generation: u64) -> Result<(), ApplicationError>;

    /// Running instances whose bundle identifier matches exactly.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main.
    fn running_applications(
        &self,
        handle: BackendHandle,
        bundle_id: &str,
    ) -> Result<Vec<ApplicationId>, ApplicationError>;

    /// Whether the exact instance is still alive.
    ///
    /// A foreign handle or generation is never running here.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main.
    fn is_running(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
    ) -> Result<bool, ApplicationError>;

    /// The exact bundle identifier of one instance.
    ///
    /// This is how a will-launch notification, which yields only an
    /// [`ApplicationId`], is classified without retaining Cocoa off-main. A
    /// foreign handle or generation reports `None`.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main.
    fn bundle_identifier(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
    ) -> Result<Option<String>, ApplicationError>;

    /// The bundle identifier of the application bundle at `path`.
    ///
    /// A path replacement target derives the identifier it needs for the
    /// already-running check here. `Ok(None)` means the path holds no
    /// application bundle, which is an answer, not a failure.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main.
    fn bundle_identifier_at_path(
        &self,
        handle: BackendHandle,
        path: &Path,
    ) -> Result<Option<String>, ApplicationError>;

    /// The path of the installed application that claims `bundle_id`.
    ///
    /// A bundle-identifier replacement target resolves the path it opens here.
    /// `Ok(None)` means no installed application claims the identifier, which
    /// is an answer, not a failure.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main.
    fn application_path(
        &self,
        handle: BackendHandle,
        bundle_id: &str,
    ) -> Result<Option<PathBuf>, ApplicationError>;

    /// Requests termination of the exact instance through `permit`.
    ///
    /// The backend performs identity preflight first. It must call
    /// [`IssuancePermit::issue`] at the exact `forceTerminate` site and return
    /// [`TerminateOutcome::Stale`] when that call returns `None`.
    ///
    /// A foreign handle or generation is never touched.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main.
    fn request_terminate(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
        permit: IssuancePermit<'_>,
    ) -> Result<TerminateOutcome, ApplicationError>;

    /// Registers workspace activation observation for one generation.
    ///
    /// The native callback must only call `notify` (plain-state invalidation) and
    /// enqueue the returned ticket's [`frontmost::RefreshTicket::run`] on a *later*
    /// main turn through its own main-queue deferral (as will-launch does), never
    /// inline. The default implementation reports [`ApplicationError::Unavailable`].
    ///
    /// # Errors
    /// Returns [`ApplicationError::AlreadyInstalled`] when already registered, a
    /// registration failure or [`ApplicationError::NotMainThread`].
    fn observe_activation(
        &self,
        _handle: BackendHandle,
        _generation: u64,
        _notify: ActivationSink,
    ) -> Result<(), ApplicationError> {
        Err(ApplicationError::Unavailable)
    }

    /// Removes only the activation observer for one generation. Idempotent; a
    /// superseded generation is left untouched. [`ApplicationBackend::unobserve`]
    /// also removes it.
    ///
    /// # Errors
    /// Returns a removal failure; ownership stays with the caller.
    fn unobserve_activation(
        &self,
        _handle: BackendHandle,
        _generation: u64,
    ) -> Result<(), ApplicationError> {
        Ok(())
    }

    /// Queries the actual current frontmost application.
    ///
    /// Main-thread only. The instance is interned into the generation's owner
    /// map. `Ok(None)` means the system reports no frontmost application.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main or a query failure.
    fn frontmost(
        &self,
        _handle: BackendHandle,
    ) -> Result<Option<FrontmostApplication>, ApplicationError> {
        Err(ApplicationError::Unavailable)
    }

    /// Every live running application with its metadata, interned as exact
    /// instances of this handle's generation. [`FrontmostApplication`] is the
    /// shared metadata record; `is_current_process` marks Stache itself.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main or a query failure.
    /// The default implementation reports [`ApplicationError::Unavailable`].
    fn running_details(
        &self,
        _handle: BackendHandle,
    ) -> Result<Vec<FrontmostApplication>, ApplicationError> {
        Err(ApplicationError::Unavailable)
    }

    /// Requests graceful termination of the exact instance through `permit`.
    ///
    /// The backend runs the main exact-identity/current-frontmost preflight
    /// first, then calls [`IssuancePermit::issue`] at the exact `terminate` site
    /// and, inside that closure, [`PressAuthorization::commit_send`]. It must
    /// return [`GracefulOutcome::Stale`] when the permit returns `None` and
    /// [`GracefulOutcome::Cancelled`] when the commit fails. It never calls
    /// `forceTerminate`.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main.
    fn request_graceful_terminate(
        &self,
        _handle: BackendHandle,
        _application: ApplicationId,
        _authorization: &PressAuthorization,
        _permit: IssuancePermit<'_>,
    ) -> Result<GracefulOutcome, ApplicationError> {
        Err(ApplicationError::Unavailable)
    }

    /// Activates the exact retained instance through `permit`.
    ///
    /// The backend resolves the retained instance and validates liveness and
    /// owner generation first (not running or foreign is
    /// [`ActivationOutcome::Stale`]). It must call [`IssuancePermit::issue`] at
    /// the exact native activation site and, inside that closure, check
    /// [`ActivationAuthorization::is_expired`] (returning
    /// [`ActivationOutcome::ExpiredBeforeSend`]) and then
    /// [`ActivationAuthorization::commit_send`] (returning
    /// [`ActivationOutcome::Cancelled`] on failure). It returns
    /// [`ActivationOutcome::Stale`] when the permit returns `None`. It never
    /// activates any other instance and never opens by path.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main.
    fn request_activate(
        &self,
        _handle: BackendHandle,
        _application: ApplicationId,
        _authorization: &ActivationAuthorization,
        _permit: IssuancePermit<'_>,
    ) -> Result<ActivationOutcome, ApplicationError> {
        Err(ApplicationError::Unavailable)
    }

    /// Hides or unhides the exact retained instance through `permit`.
    ///
    /// The backend resolves the retained instance and validates liveness and
    /// owner generation first (not running or foreign is
    /// [`VisibilityOutcome::Stale`]). It must call [`IssuancePermit::issue`] at
    /// the exact native `hide`/`unhide` site and, inside that closure, check
    /// [`VisibilityAuthorization::is_expired`] (returning
    /// [`VisibilityOutcome::ExpiredBeforeSend`]) and then
    /// [`VisibilityAuthorization::commit_send`] (returning
    /// [`VisibilityOutcome::Cancelled`] on failure). It returns
    /// [`VisibilityOutcome::Stale`] when the permit returns `None`. It never
    /// touches any other instance and never activates.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main.
    fn request_visibility(
        &self,
        _handle: BackendHandle,
        _application: ApplicationId,
        _operation: VisibilityOperation,
        _authorization: &VisibilityAuthorization,
        _permit: IssuancePermit<'_>,
    ) -> Result<VisibilityOutcome, ApplicationError> {
        Err(ApplicationError::Unavailable)
    }

    /// Registers launch, terminate, hide and unhide observation for one
    /// generation.
    ///
    /// The native callbacks must only intern or look up the instance and enqueue
    /// the pure [`LifecycleEvent`] on a *later* main turn (see
    /// [`lifecycle`]), never call `notify` inline. The default implementation
    /// reports [`ApplicationError::Unavailable`].
    ///
    /// # Errors
    /// Returns [`ApplicationError::AlreadyInstalled`] when already registered, a
    /// registration failure or [`ApplicationError::NotMainThread`].
    fn observe_lifecycle(
        &self,
        _handle: BackendHandle,
        _generation: u64,
        _notify: LifecycleSink,
    ) -> Result<(), ApplicationError> {
        Err(ApplicationError::Unavailable)
    }

    /// Removes only the lifecycle observers for one generation. Idempotent; a
    /// superseded generation is left untouched. [`ApplicationBackend::unobserve`]
    /// also removes them.
    ///
    /// # Errors
    /// Returns a removal failure; ownership stays with the caller.
    fn unobserve_lifecycle(
        &self,
        _handle: BackendHandle,
        _generation: u64,
    ) -> Result<(), ApplicationError> {
        Ok(())
    }

    /// Best-effort display hint for the exact instance from public geometry.
    ///
    /// The caller has already validated identity and frontmost state on main.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotMainThread`] off-main.
    fn display_hint(
        &self,
        _handle: BackendHandle,
        _application: ApplicationId,
    ) -> Result<DisplayHint, ApplicationError> {
        Err(ApplicationError::Unavailable)
    }

    /// Opens an application at an exact path through `permit`.
    ///
    /// The backend prepares the path, URL and configuration first. It must call
    /// [`IssuancePermit::issue`] at the exact `AppKit` send and return
    /// [`OpenRequest::Stale`] when that call returns `None`; the completion is
    /// then never invoked. `AppKit` reports the real result through `completion`.
    ///
    /// # Errors
    /// Returns an immediate preparation failure or [`ApplicationError::NotMainThread`].
    fn open(
        &self,
        handle: BackendHandle,
        path: &Path,
        completion: NativeOpenCompletion,
        permit: IssuancePermit<'_>,
    ) -> Result<OpenRequest, ApplicationError>;
}

/// The module-facing control handle.
///
/// It is `Send + Sync`, starts no work and owns no native object itself. Clone it
/// freely; all clones share one generation and one backend entry.
#[derive(Clone)]
pub struct ApplicationControl {
    state: Arc<ControlState>,
}

struct ControlState {
    handle: BackendHandle,
    executor: Arc<dyn MainThreadExecutor>,
    backend: Arc<dyn ApplicationBackend>,
    budget: Duration,
    next_generation: AtomicU64,
    /// The generation this control most recently made active.
    ///
    /// This mirror exists so a control-wide stop can invalidate its generation
    /// immediately, without waiting for a stalled commit gate.
    published: AtomicU64,
    /// The highest generation logically invalidated.
    ///
    /// Logical invalidation is separate from native removal: a generation at or
    /// below this value is inert for callbacks, queries and native issuance even
    /// when its guard is still owned and the main queue has not drained yet.
    /// Generations only increase, so a late cleanup of an old generation can
    /// never invalidate a successor.
    invalidated: AtomicU64,
    /// Observed frontmost watches, told once when their generation is invalidated.
    observed_watches: Mutex<Vec<Weak<frontmost::FrontmostShared>>>,
    gate: Mutex<GenerationState>,
}

/// The narrow commit gate: generations, accepted revisions and request order.
///
/// `owned` is the generation whose observer registration this control still
/// owns. It is cleared only after a matching removal is acknowledged, so a
/// failed install or stop is retried rather than leaking a live observer.
#[derive(Default)]
struct GenerationState {
    active: u64,
    revision: u64,
    next_request: u64,
    latest_request: u64,
    owned: Option<u64>,
}

impl ApplicationControl {
    /// Creates a control over the host dispatcher and the system `AppKit` backend.
    #[must_use]
    pub fn system(executor: Arc<dyn MainThreadExecutor>) -> Self {
        Self::new(executor, Arc::new(SystemApplicationBackend))
    }

    /// Creates a control over the host dispatcher and an injected backend.
    #[must_use]
    pub fn new(
        executor: Arc<dyn MainThreadExecutor>,
        backend: Arc<dyn ApplicationBackend>,
    ) -> Self {
        Self::with_acknowledgement_budget(executor, backend, ACKNOWLEDGEMENT_BUDGET)
    }

    /// Creates a control with an explicit per-hop acknowledgement budget.
    ///
    /// Only tests and hosts with their own main-hop budget should need this.
    #[must_use]
    pub fn with_acknowledgement_budget(
        executor: Arc<dyn MainThreadExecutor>,
        backend: Arc<dyn ApplicationBackend>,
        budget: Duration,
    ) -> Self {
        Self {
            state: Arc::new(ControlState {
                handle: BackendHandle::allocate(),
                executor,
                backend,
                budget,
                next_generation: AtomicU64::new(0),
                published: AtomicU64::new(0),
                invalidated: AtomicU64::new(0),
                observed_watches: Mutex::new(Vec::new()),
                gate: Mutex::new(GenerationState::default()),
            }),
        }
    }

    /// Installs will-launch observation and returns its live generation.
    ///
    /// The observer is registered on the workspace notification center with
    /// explicit main-queue delivery, and the call returns only after the native
    /// acknowledgement and a final cancellation check. A caller may therefore
    /// take the running-application snapshot immediately afterwards:
    /// subscription happens first. The cancellation token is checked before
    /// claiming a generation, immediately before registration, immediately
    /// after registration, and at least every [`CANCELLATION_QUANTUM`] while
    /// waiting. An off-main call waits at most the configured budget; an inline
    /// main-thread call completes directly.
    ///
    /// A failed, cancelled or timed-out install invalidates exactly its
    /// generation and keeps cleanup owned. The exact partial registration is
    /// rolled back immediately after `observe` returns if the generation is no
    /// longer current; otherwise an ordered cleanup job retries. A later install
    /// first cleans that exact generation (or reports its still-actionable
    /// cleanup error) instead of failing `AlreadyInstalled` forever.
    ///
    /// # Errors
    /// Returns [`ApplicationError::Cancelled`], a dispatch rejection, a native
    /// registration failure, a cleanup failure or [`ApplicationError::TimedOut`].
    pub fn install(
        &self,
        cancellation: &CancellationToken,
        notify: impl Fn(ApplicationId) + Send + Sync + 'static,
    ) -> Result<ApplicationObservation, ApplicationError> {
        if cancellation.is_cancelled() {
            return Err(ApplicationError::Cancelled);
        }
        let generation = self.claim_generation()?;
        if cancellation.is_cancelled() {
            return Err(self.abandon_install(generation, ApplicationError::Cancelled));
        }

        // The backend may outlive every external owner and is itself owned by
        // the control state, so the observer sink only holds a weak reference.
        // Otherwise backend -> sink -> control state -> backend is a cycle that
        // pins a failed control after cleanup failure.
        let notify_state = Arc::downgrade(&self.state);
        let sink: Arc<dyn Fn(ApplicationId) + Send + Sync> = Arc::new(move |application| {
            let Some(state) = notify_state.upgrade() else {
                return;
            };
            if !state.is_active(generation) {
                return;
            }
            let _ = catch_unwind(AssertUnwindSafe(|| notify(application)));
        });

        let ack = Arc::new(Ack::new());
        let job_ack = Arc::clone(&ack);
        let job_state = Arc::clone(&self.state);
        let token = cancellation.clone();
        let job = Box::new(move || {
            if token.is_cancelled() || !job_state.is_active(generation) {
                job_ack.complete(InstallAck::Aborted);
                return;
            }
            let backend = Arc::clone(&job_state.backend);
            match backend.observe(job_state.handle, generation, sink) {
                Ok(()) => {
                    // Cancellation may land while `observe` runs on main. Roll
                    // back exactly this partial generation before acknowledging.
                    if token.is_cancelled() || !job_state.is_active(generation) {
                        if backend.unobserve(job_state.handle, generation).is_ok() {
                            job_state.release_ownership(generation);
                        }
                        job_ack.complete(InstallAck::Aborted);
                    } else {
                        job_ack.complete(InstallAck::Done(Ok(())));
                    }
                }
                Err(error) => {
                    // A late install may have registered partially before the
                    // error or before cancellation landed; roll it back here.
                    if token.is_cancelled() || !job_state.is_active(generation) {
                        if backend.unobserve(job_state.handle, generation).is_ok() {
                            job_state.release_ownership(generation);
                        }
                        job_ack.complete(InstallAck::Aborted);
                    } else {
                        job_ack.complete(InstallAck::Done(Err(error)));
                    }
                }
            }
        });
        if let Err(error) = self.state.executor.execute(job) {
            return Err(
                self.abandon_install(generation, ApplicationError::Dispatch(error.to_string()))
            );
        }
        let outcome = ack.wait_cancellable(self.state.budget, cancellation, CANCELLATION_QUANTUM);
        self.finish_install(generation, outcome, cancellation)
    }

    fn finish_install(
        &self,
        generation: u64,
        outcome: AckWait<InstallAck>,
        cancellation: &CancellationToken,
    ) -> Result<ApplicationObservation, ApplicationError> {
        let failure = match outcome {
            AckWait::Ready(InstallAck::Done(Ok(()))) => {
                if cancellation.is_cancelled() || !self.state.is_active(generation) {
                    return Err(self.abandon_install(generation, ApplicationError::Cancelled));
                }
                return Ok(ApplicationObservation {
                    state: Arc::clone(&self.state),
                    generation,
                    removed: AtomicBool::new(false),
                    frontmost: Mutex::new(None),
                });
            }
            AckWait::Ready(InstallAck::Done(Err(error))) => error,
            AckWait::Ready(InstallAck::Aborted) | AckWait::Cancelled => ApplicationError::Cancelled,
            AckWait::TimedOut => ApplicationError::TimedOut,
        };
        Err(self.abandon_install(generation, failure))
    }

    /// Claims the next generation after cleaning up any failed predecessor.
    fn claim_generation(&self) -> Result<u64, ApplicationError> {
        loop {
            let pending = {
                let mut gate = self.state.gate_within()?;
                if gate.active != 0 {
                    return Err(ApplicationError::AlreadyInstalled);
                }
                if let Some(owned) = gate.owned {
                    Some(owned)
                } else {
                    let generation = self.state.next_generation.fetch_add(1, Ordering::SeqCst) + 1;
                    gate.active = generation;
                    gate.revision = 0;
                    gate.next_request = 0;
                    gate.latest_request = 0;
                    gate.owned = Some(generation);
                    self.state.published.store(generation, Ordering::SeqCst);
                    drop(gate);
                    return Ok(generation);
                }
            };
            if let Some(owned) = pending {
                self.cleanup_owned(owned)?;
            }
        }
    }

    /// Removes a failed generation's observer and reports an actionable failure.
    ///
    /// Ownership is retained when removal fails, so the next install retries
    /// exactly this generation instead of reporting `AlreadyInstalled`.
    fn cleanup_owned(&self, generation: u64) -> Result<(), ApplicationError> {
        let ack = Arc::new(Ack::new());
        let job_ack = Arc::clone(&ack);
        let state = Arc::clone(&self.state);
        let job = Box::new(move || {
            let result = state.backend.unobserve(state.handle, generation);
            if result.is_ok() {
                state.release_ownership(generation);
            }
            job_ack.complete(result);
        });
        if let Err(error) = self.state.executor.execute(job) {
            return Err(ApplicationError::Dispatch(error.to_string()));
        }
        match ack.wait(self.state.budget) {
            AckWait::Ready(result) => result,
            AckWait::TimedOut => Err(ApplicationError::TimedOut),
            AckWait::Cancelled => Err(ApplicationError::Cancelled),
        }
    }

    /// Invalidates and removes the observation generation this control still
    /// owns, without installing anything.
    ///
    /// This is the cleanup path for a lifecycle that failed before an
    /// [`ApplicationObservation`] was returned: [`ApplicationControl::install`]
    /// invalidates a failed generation and keeps its cleanup owned, but the
    /// caller has no observation handle to stop. This method rechecks the
    /// current generation under the commit gate, invalidates it, and
    /// acknowledges removal of exactly the owned generation. A late install job
    /// therefore observes an inactive generation and stays inert.
    ///
    /// It is idempotent and safe to call with nothing owned. The published
    /// generation is invalidated immediately, without waiting for the commit
    /// gate, so callbacks and queued actions are inert even when a stalled
    /// native send holds the gate. A staged native send may then hold the gate;
    /// removal then reports [`ApplicationError::TimedOut`] within the configured
    /// per-hop budget and the call can be retried. A failed native removal keeps
    /// ownership so the caller can retry instead of leaking a live observer.
    ///
    /// # Errors
    /// Returns [`ApplicationError::TimedOut`] when the commit gate cannot be
    /// observed, a dispatch rejection, or a native removal failure.
    pub fn stop(&self) -> Result<(), ApplicationError> {
        // Logical invalidation does not wait for the commit gate: a stalled
        // native send must not leave the live generation current. The native
        // send already in flight is irreversible; everything queued becomes
        // inert immediately.
        self.state.invalidate_published();
        let owned = {
            let mut gate = self.state.gate_within()?;
            // Any active generation is invalidated by a control-wide stop, so
            // queued install/termination/open work cannot issue afterwards.
            gate.active = 0;
            self.state.published.store(0, Ordering::SeqCst);
            gate.owned
        };
        let Some(generation) = owned else {
            return Ok(());
        };
        self.cleanup_owned(generation)
    }

    /// Invalidates exactly this generation and schedules owned cleanup.
    fn abandon_install(&self, generation: u64, error: ApplicationError) -> ApplicationError {
        // A failed, cancelled or timed-out install is inert immediately, even
        // when it completed natively but its acknowledgement was lost.
        self.state.invalidate(generation);
        // A busy gate (an issuance on the main thread) defers deactivation to
        // the cleanup job, which runs after that issuance and invalidates before
        // removing the observer.
        let _ = self.state.deactivate_within(generation);
        self.schedule_cleanup(generation);
        error
    }

    fn schedule_cleanup(&self, generation: u64) {
        let state = Arc::clone(&self.state);
        let job = Box::new(move || {
            // Idempotent: a failed install may already have invalidated it, and
            // a nonblocking path may have raced a native issuance.
            state.invalidate(generation);
            state.deactivate(generation);
            if state.backend.unobserve(state.handle, generation).is_ok() {
                state.release_ownership(generation);
            }
        });
        let _ = self.state.executor.execute(job);
    }
}

impl ControlState {
    /// Whether a generation has been logically invalidated.
    ///
    /// This is an atomic read with no commit-gate acquisition, so it stays
    /// truthful even while a stalled native send holds the gate.
    fn is_invalidated(&self, generation: u64) -> bool {
        generation != 0 && generation <= self.invalidated.load(Ordering::SeqCst)
    }

    /// Invalidates one generation immediately.
    ///
    /// `fetch_max` makes the flag monotonic: a late cleanup of an old
    /// generation can raise the floor only to its own value, never invalidate a
    /// successor.
    fn invalidate(&self, generation: u64) {
        if generation != 0 {
            self.invalidated.fetch_max(generation, Ordering::SeqCst);
            self.notify_invalidated_watches();
        }
    }

    /// Remembers a watch so invalidating its generation tells its listener.
    fn watch_frontmost_inactive_on_invalidation(&self, shared: &Arc<frontmost::FrontmostShared>) {
        let mut watches = lock(&self.observed_watches);
        watches.retain(|watch| watch.strong_count() > 0);
        watches.push(Arc::downgrade(shared));
    }

    /// Runs the one-shot `Inactive` notification for every invalidated watch.
    /// Listener code runs after the registry lock is released.
    fn notify_invalidated_watches(&self) {
        let due: Vec<Arc<frontmost::FrontmostShared>> = {
            let mut watches = lock(&self.observed_watches);
            if watches.is_empty() {
                return;
            }
            let mut due = Vec::new();
            watches.retain(|watch| {
                let Some(shared) = watch.upgrade() else {
                    return false;
                };
                if self.is_invalidated(shared.generation()) {
                    due.push(shared);
                    return false;
                }
                true
            });
            due
        };
        for shared in due {
            shared.notify_inactive();
        }
    }

    /// Invalidates whatever generation this control last published.
    fn invalidate_published(&self) { self.invalidate(self.published.load(Ordering::SeqCst)); }

    fn is_active(&self, generation: u64) -> bool {
        generation != 0 && !self.is_invalidated(generation) && lock(&self.gate).active == generation
    }

    /// Whether a generation is the control's published, non-invalidated one.
    ///
    /// This reads only `invalidated` and `published`: it never takes the commit
    /// gate, so it answers while a stalled native send holds the gate. It is
    /// advisory, because a concurrent stop or successor can still commit after
    /// the read; the issuance permit remains authoritative.
    fn is_published(&self, generation: u64) -> bool {
        generation != 0
            && !self.is_invalidated(generation)
            && self.published.load(Ordering::SeqCst) == generation
    }

    fn is_current(&self, generation: u64, accepted: AcceptedActionToken) -> bool {
        if self.is_invalidated(generation) {
            return false;
        }
        let gate = lock(&self.gate);
        accepted.handle == self.handle
            && accepted.generation == generation
            && gate.active == generation
            && gate.revision == accepted.revision
    }

    fn is_completion_current(
        &self,
        generation: u64,
        accepted: AcceptedActionToken,
        request: u64,
    ) -> bool {
        if self.is_invalidated(generation) {
            return false;
        }
        let gate = lock(&self.gate);
        request != 0
            && accepted.handle == self.handle
            && accepted.generation == generation
            && gate.active == generation
            && gate.revision == accepted.revision
            && gate.latest_request == request
    }

    /// Whether a generation is active, without waiting for an in-flight
    /// issuance. A busy gate is reported as active so the caller dispatches and
    /// lets the issuance permit decide.
    fn is_active_now(&self, generation: u64) -> bool {
        if self.is_invalidated(generation) {
            return false;
        }
        match self.gate.try_lock() {
            Ok(gate) => generation != 0 && gate.active == generation,
            Err(TryLockError::Poisoned(poisoned)) => {
                let gate = poisoned.into_inner();
                generation != 0 && gate.active == generation
            }
            Err(TryLockError::WouldBlock) => true,
        }
    }

    /// Acquires the commit gate within the configured per-hop budget.
    ///
    /// The only long critical section is one immediate native send. This bound
    /// keeps a control-side stop or acceptance from waiting indefinitely if that
    /// send stalls, without ever blocking the main thread on a worker.
    fn gate_within(&self) -> Result<MutexGuard<'_, GenerationState>, ApplicationError> {
        let deadline = Instant::now() + self.budget;
        loop {
            match self.gate.try_lock() {
                Ok(gate) => return Ok(gate),
                Err(TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => {}
            }
            if Instant::now() >= deadline {
                return Err(ApplicationError::TimedOut);
            }
            std::thread::sleep(GATE_RETRY);
        }
    }

    fn deactivate(&self, generation: u64) {
        let mut gate = lock(&self.gate);
        if gate.active == generation {
            gate.active = 0;
            drop(gate);
            self.published.store(0, Ordering::SeqCst);
        }
    }

    fn deactivate_within(&self, generation: u64) -> Result<(), ApplicationError> {
        let mut gate = self.gate_within()?;
        if gate.active == generation {
            gate.active = 0;
            self.published.store(0, Ordering::SeqCst);
        }
        drop(gate);
        Ok(())
    }

    fn release_ownership(&self, generation: u64) {
        let mut gate = lock(&self.gate);
        if gate.owned == Some(generation) {
            gate.owned = None;
        }
    }

    /// Accepts a new revision for the live generation.
    ///
    /// A stale observation returns a token carrying its dead generation without
    /// touching the live revision, so it cannot invalidate a successor. A
    /// logically invalidated generation returns a revision-zero token without
    /// bumping the revision. A gate acquisition timeout returns an error and
    /// leaves the previous accepted snapshot current; that boundary is explicit
    /// rather than a silent no-op.
    fn accept(&self, generation: u64) -> Result<AcceptedActionToken, ApplicationError> {
        let mut gate = self.gate_within()?;
        let revision = if gate.active == generation && !self.is_invalidated(generation) {
            gate.revision += 1;
            gate.revision
        } else {
            0
        };
        Ok(AcceptedActionToken {
            handle: self.handle,
            generation,
            revision,
        })
    }

    fn accepted(&self, generation: u64) -> AcceptedActionToken {
        let revision = if self.is_invalidated(generation) {
            0
        } else {
            let gate = lock(&self.gate);
            if gate.active == generation {
                gate.revision
            } else {
                0
            }
        };
        AcceptedActionToken {
            handle: self.handle,
            generation,
            revision,
        }
    }

    const fn issuance<'a>(
        &'a self,
        generation: u64,
        accepted: AcceptedActionToken,
        request: &'a RequestCommit,
        open: Option<&'a Arc<OpenDelivery>>,
    ) -> IssuancePermit<'a> {
        IssuancePermit {
            state: self,
            generation,
            accepted,
            request,
            open,
        }
    }
}

impl fmt::Debug for ApplicationControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApplicationControl").finish_non_exhaustive()
    }
}

/// One live observation generation.
///
/// Keep it for as long as observation is wanted. It is `Send`, but its native
/// state is not: dropping it removes the observer through the dispatcher and
/// waits for a bounded acknowledgement, even when the lifecycle token was
/// already cancelled.
pub struct ApplicationObservation {
    state: Arc<ControlState>,
    generation: u64,
    removed: AtomicBool,
    /// The frontmost publication cell, when [`ApplicationObservation::watch_frontmost`]
    /// installed one.
    frontmost: Mutex<Option<Arc<frontmost::FrontmostShared>>>,
}

impl ApplicationObservation {
    /// The exact generation this observation owns.
    #[must_use]
    pub const fn generation(&self) -> u64 { self.generation }

    /// Stages a new accepted-action snapshot for this generation.
    ///
    /// Queued work captures the returned token and rechecks it at the exact
    /// native issuance site. The newest token is current; any older token is
    /// stale. Calling this on a stopped, superseded or invalidated observation
    /// returns a stale token (revision zero) and never mutates the successor's
    /// revision.
    ///
    /// # Errors
    /// Returns [`ApplicationError::TimedOut`] when the commit gate cannot be
    /// observed inside the configured budget, for example while a stalled
    /// native issuance still holds it. The previous accepted snapshot stays
    /// current; nothing is silently accepted.
    pub fn accept(&self) -> Result<AcceptedActionToken, ApplicationError> {
        self.state.accept(self.generation)
    }

    /// The current accepted token without bumping the revision.
    ///
    /// This is a read under the commit gate: it returns after any in-flight
    /// native send completes. Use [`ApplicationObservation::accept`] for the
    /// bounded, fallible acceptance point.
    #[must_use]
    pub fn accepted(&self) -> AcceptedActionToken { self.state.accepted(self.generation) }

    /// Whether a token is still current for this observation.
    ///
    /// This is a read under the commit gate: it returns after any in-flight
    /// native send completes. The issuance permit, not this check, is the
    /// authoritative decision before a native request.
    #[must_use]
    pub fn is_current(&self, accepted: AcceptedActionToken) -> bool {
        self.state.is_current(self.generation, accepted)
    }

    /// Whether this generation is published and not invalidated.
    ///
    /// This is an advisory read of two atomics only: it never takes the commit
    /// gate, so it answers promptly even while a stalled native send holds the
    /// gate. It reports `false` for a stopped, invalidated, superseded or never
    /// installed generation, and `true` while the generation is live. It does
    /// not observe accepted revisions: [`ApplicationObservation::is_current`]
    /// and the issuance permit remain the authoritative decisions.
    #[must_use]
    pub fn is_active(&self) -> bool { self.state.is_published(self.generation) }

    /// Logically invalidates this generation for stage-1 teardown.
    ///
    /// This is the registry teardown path, not a cleanup path: it never takes
    /// the commit gate, never calls the native backend and never waits, so it
    /// returns while a stalled native send holds the gate and while a start or
    /// stop holds a slot lock. The generation is inert immediately: callbacks
    /// and queries observe an inactive generation, a later
    /// [`ApplicationObservation::accept`] returns a stale token, and queued
    /// native work refuses to issue.
    ///
    /// It is idempotent and safe from any thread, including after
    /// [`ApplicationObservation::stop`]. Already-issued native work still
    /// finishes inert; invalidation neither retries nor cancels it.
    /// [`ApplicationObservation::stop`] remains the method that also performs
    /// the acknowledged native removal.
    pub fn invalidate(&self) { self.state.invalidate(self.generation); }

    /// Running instances whose bundle identifier matches exactly.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotInstalled`] after cleanup, a dispatch
    /// rejection or a timeout.
    pub fn running_applications(
        &self,
        bundle_id: &str,
    ) -> Result<Vec<ApplicationId>, ApplicationError> {
        let bundle_id = bundle_id.to_owned();
        self.query(move |backend, handle| backend.running_applications(handle, &bundle_id))
    }

    /// Every live running application with metadata, as exact instances of this
    /// observation. Runs a main-thread query bounded by the acknowledgement
    /// budget.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotInstalled`] after cleanup, a dispatch
    /// rejection, a timeout or an unavailable backend.
    pub fn running_details(&self) -> Result<Vec<FrontmostApplication>, ApplicationError> {
        self.query(|backend, handle| backend.running_details(handle))
    }

    /// Whether the exact instance is still alive.
    ///
    /// An instance from another control is not running for this observation.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotInstalled`] after cleanup, a dispatch
    /// rejection or a timeout.
    pub fn is_running(&self, application: ApplicationId) -> Result<bool, ApplicationError> {
        if !self.owns(application) {
            return Ok(false);
        }
        self.query(move |backend, handle| backend.is_running(handle, application))
    }

    /// The exact bundle identifier of one instance.
    ///
    /// A will-launch callback receives only an [`ApplicationId`]; this query
    /// supplies the identifier needed to classify it. It also runs inline when
    /// called from an observer callback on the main thread. A foreign instance
    /// reports `None`.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotInstalled`] after cleanup, a dispatch
    /// rejection or a timeout.
    pub fn bundle_identifier(
        &self,
        application: ApplicationId,
    ) -> Result<Option<String>, ApplicationError> {
        if !self.owns(application) {
            return Ok(None);
        }
        self.query(move |backend, handle| backend.bundle_identifier(handle, application))
    }

    /// The bundle identifier of the application bundle at `path`.
    ///
    /// A path replacement target derives its identifier here, so the module
    /// never inspects the filesystem itself. `Ok(None)` means the path holds no
    /// application bundle.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotInstalled`] after cleanup, a dispatch
    /// rejection or a timeout.
    pub fn bundle_identifier_at_path(
        &self,
        path: &Path,
    ) -> Result<Option<String>, ApplicationError> {
        let path = path.to_path_buf();
        self.query(move |backend, handle| backend.bundle_identifier_at_path(handle, &path))
    }

    /// The path of the installed application that claims `bundle_id`.
    ///
    /// A bundle-identifier replacement target resolves the path it opens here.
    /// `Ok(None)` means no installed application claims the identifier.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotInstalled`] after cleanup, a dispatch
    /// rejection or a timeout.
    pub fn application_path(&self, bundle_id: &str) -> Result<Option<PathBuf>, ApplicationError> {
        let bundle_id = bundle_id.to_owned();
        self.query(move |backend, handle| backend.application_path(handle, &bundle_id))
    }

    /// Requests force-termination of the exact instance.
    ///
    /// The backend performs identity preflight first and the issuance permit
    /// rechecks the accepted token at the exact `forceTerminate` call. A token
    /// from another control is [`TerminateOutcome::Stale`]; an instance from
    /// another control reports [`TerminateOutcome::NotRunning`] and touches
    /// nothing. [`TerminateOutcome::Requested`] means only that macOS accepted
    /// the request.
    ///
    /// If the acknowledgement times out before the request issued, the queued
    /// job is marked abandoned and the permit refuses to send. A request that
    /// already issued may still reach macOS after the timeout.
    ///
    /// # Errors
    /// Returns a dispatch rejection or timeout.
    pub fn terminate(
        &self,
        accepted: AcceptedActionToken,
        application: ApplicationId,
    ) -> Result<TerminateOutcome, ApplicationError> {
        if !self.owns(application) {
            return Ok(TerminateOutcome::NotRunning);
        }
        // Classify an already-stopped generation without dispatching. This is
        // not the commit decision: a concurrent stop is caught by the permit.
        if !self.state.is_active_now(self.generation) {
            return Ok(TerminateOutcome::Stale);
        }
        let request = Arc::new(RequestCommit::new(Instant::now() + self.state.budget));
        let ack = Arc::new(Ack::new());
        let job_ack = Arc::clone(&ack);
        let job_request = Arc::clone(&request);
        let state = Arc::clone(&self.state);
        let generation = self.generation;
        let job = Box::new(move || {
            let permit = state.issuance(generation, accepted, &job_request, None);
            let backend = Arc::clone(&state.backend);
            job_ack.complete(backend.request_terminate(state.handle, application, permit));
        });
        self.state
            .executor
            .execute(job)
            .map_err(|error| ApplicationError::Dispatch(error.to_string()))?;
        match ack.wait(self.state.budget) {
            AckWait::Ready(result) => result,
            AckWait::TimedOut => {
                request.abandon();
                Err(ApplicationError::TimedOut)
            }
            AckWait::Cancelled => Err(ApplicationError::Cancelled),
        }
    }

    /// Opens the application at `path` when the accepted token is current.
    ///
    /// The backend prepares the native path, URL and configuration first; the
    /// issuance permit rechecks the accepted token at the exact `AppKit` send.
    /// The returned request state confirms only that `AppKit` accepted the
    /// dispatch; `completion` reports the real outcome on the host's dispatcher.
    /// Both completion variants carry the request context; [`OpenCompletion::Current`]
    /// marks the still-newest request for a live generation, while
    /// [`OpenCompletion::Stale`] means a stop, successor generation, newer
    /// accepted revision or newer request has committed since. A stale
    /// completion logs but cannot change current pending-open bookkeeping or
    /// queue follow-on work.
    ///
    /// If the acknowledgement times out before the request issued, the queued
    /// job is marked abandoned and nothing opens. An already issued open may
    /// still complete after the timeout; its result is delivered as above.
    ///
    /// If the host dispatcher rejects the completion delivery, the completion
    /// is dropped and its result is lost; see [`NativeOpenCompletion`].
    ///
    /// # Errors
    /// Returns a dispatch rejection or timeout. A stale token returns
    /// [`OpenRequest::Stale`] and no completion is invoked.
    pub fn open<F>(
        &self,
        accepted: AcceptedActionToken,
        path: &Path,
        completion: F,
    ) -> Result<OpenRequest, ApplicationError>
    where
        F: FnOnce(OpenCompletion) + Send + 'static,
    {
        self.open_tracked(accepted, path, completion).map_err(|failure| failure.error)
    }

    /// Like [`ApplicationObservation::open`], but a failure also reports whether
    /// the native request may already have been issued.
    ///
    /// A timed-out acknowledgement is ambiguous in [`ApplicationError`]: the job
    /// may never have issued, or may have issued without being acknowledged in
    /// time. This variant abandons the request first and then reads the commit
    /// state, so [`OpenFailure::issued`] is exact: `false` means the request can
    /// never issue, `true` means it did and may still complete.
    ///
    /// # Errors
    /// Returns an [`OpenFailure`] for a dispatch rejection, timeout, native
    /// preparation failure or cancellation.
    pub fn open_tracked<F>(
        &self,
        accepted: AcceptedActionToken,
        path: &Path,
        completion: F,
    ) -> Result<OpenRequest, OpenFailure>
    where
        F: FnOnce(OpenCompletion) + Send + 'static,
    {
        let request = Arc::new(RequestCommit::new(Instant::now() + self.state.budget));
        let ack = Arc::new(Ack::new());
        let job_ack = Arc::clone(&ack);
        let job_request = Arc::clone(&request);
        let state = Arc::clone(&self.state);
        let generation = self.generation;
        let handle = self.state.handle;
        let path = path.to_path_buf();
        let delivery = Arc::new(OpenDelivery {
            state: Arc::clone(&state),
            executor: Arc::clone(&state.executor),
            generation,
            accepted,
            request: AtomicU64::new(0),
            completion: Mutex::new(Some(Box::new(completion))),
            buffered: Mutex::new(None),
            issuing: AtomicBool::new(false),
        });
        let job_delivery = Arc::clone(&delivery);
        let completion_delivery = Arc::clone(&delivery);
        let job = Box::new(move || {
            let permit = state.issuance(generation, accepted, &job_request, Some(&job_delivery));
            let backend = Arc::clone(&state.backend);
            let native: NativeOpenCompletion =
                Box::new(move |result| completion_delivery.receive(result));
            job_ack.complete(backend.open(handle, &path, native, permit));
        });
        self.state.executor.execute(job).map_err(|error| {
            request.abandon();
            OpenFailure::new(ApplicationError::Dispatch(error.to_string()), &request)
        })?;
        match ack.wait(self.state.budget) {
            AckWait::Ready(result) => result.map_err(|error| OpenFailure::new(error, &request)),
            AckWait::TimedOut => {
                request.abandon();
                Err(OpenFailure::new(ApplicationError::TimedOut, &request))
            }
            AckWait::Cancelled => {
                request.abandon();
                Err(OpenFailure::new(ApplicationError::Cancelled, &request))
            }
        }
    }

    /// Removes observation and waits for a bounded acknowledgement.
    ///
    /// Logical invalidation happens first and never waits for the commit gate:
    /// callbacks, queries and queued native actions for this generation become
    /// inert immediately, even if a stalled native send still holds the gate and
    /// native removal later times out. Only a request already inside its
    /// immediate native send is irreversible. Successful cleanup still requires
    /// the acknowledged native removal under the gate; a failed or timed-out
    /// removal keeps ownership for a retry and is reported, never hidden.
    ///
    /// This method never consults the lifecycle cancellation token: stop must
    /// still remove the observer after that token was cancelled. Repeated calls
    /// and calls after a partial failure are safe.
    ///
    /// Native deactivation waits for the gate at most the configured budget. If
    /// a stalled native send still holds it, stop reports
    /// [`ApplicationError::TimedOut`] after the generation is already inert and
    /// can be retried; it never blocks indefinitely and never waits for a
    /// worker.
    ///
    /// # Errors
    /// Returns a dispatch rejection, native removal failure or a timeout. A
    /// failed removal leaves the generation invalid, keeps ownership and lets a
    /// retry or `Drop` clean it up.
    pub fn stop(&self) -> Result<(), ApplicationError> {
        if self.removed.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.state.invalidate(self.generation);
        self.state.deactivate_within(self.generation)?;
        let ack = Arc::new(Ack::new());
        let job_ack = Arc::clone(&ack);
        let state = Arc::clone(&self.state);
        let generation = self.generation;
        let job = Box::new(move || {
            let result = state.backend.unobserve(state.handle, generation);
            if result.is_ok() {
                state.release_ownership(generation);
            }
            job_ack.complete(result);
        });
        if let Err(error) = self.state.executor.execute(job) {
            return Err(ApplicationError::Dispatch(error.to_string()));
        }
        match ack.wait(self.state.budget) {
            AckWait::Ready(Ok(())) => {
                self.removed.store(true, Ordering::SeqCst);
                Ok(())
            }
            AckWait::Ready(Err(error)) => Err(error),
            AckWait::TimedOut => Err(ApplicationError::TimedOut),
            AckWait::Cancelled => Err(ApplicationError::Cancelled),
        }
    }

    fn owns(&self, application: ApplicationId) -> bool {
        application.handle == self.state.handle && application.generation == self.generation
    }

    fn query<T, F>(&self, operation: F) -> Result<T, ApplicationError>
    where
        T: Send + 'static,
        F: FnOnce(&dyn ApplicationBackend, BackendHandle) -> Result<T, ApplicationError>
            + Send
            + 'static, {
        let ack = Arc::new(Ack::new());
        let job_ack = Arc::clone(&ack);
        let state = Arc::clone(&self.state);
        let generation = self.generation;
        let job = Box::new(move || {
            if !state.is_active(generation) {
                job_ack.complete(QueryAck::Stale);
                return;
            }
            let backend = Arc::clone(&state.backend);
            job_ack.complete(QueryAck::Done(operation(backend.as_ref(), state.handle)));
        });
        self.state
            .executor
            .execute(job)
            .map_err(|error| ApplicationError::Dispatch(error.to_string()))?;
        match ack.wait(self.state.budget) {
            AckWait::Ready(QueryAck::Done(result)) => result,
            AckWait::Ready(QueryAck::Stale) => Err(ApplicationError::NotInstalled),
            AckWait::TimedOut => Err(ApplicationError::TimedOut),
            AckWait::Cancelled => Err(ApplicationError::Cancelled),
        }
    }
}

impl Drop for ApplicationObservation {
    fn drop(&mut self) { let _ = self.stop(); }
}

impl fmt::Debug for ApplicationObservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApplicationObservation")
            .field("generation", &self.generation)
            .field("removed", &self.removed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Drop for ControlState {
    /// Best-effort final cleanup once every external owner is gone.
    ///
    /// A failed install or failed stop may still own one observation generation.
    /// The job is dispatched without blocking and names exactly that generation;
    /// a stale or superseded generation is a no-op in the backend. If the
    /// dispatcher rejects the job or the main run loop is unavailable, the
    /// observer cannot be removed here and no success is claimed.
    fn drop(&mut self) {
        let owned = lock(&self.gate).owned;
        let Some(generation) = owned else {
            return;
        };
        let executor = Arc::clone(&self.executor);
        let backend = Arc::clone(&self.backend);
        let handle = self.handle;
        let job = Box::new(move || {
            let _ = backend.unobserve(handle, generation);
        });
        let _ = executor.execute(job);
    }
}

/// The delivery side of one open request.
///
/// The native completion may be invoked synchronously from inside the send for
/// which the issuance permit holds the commit gate. Delivering or scheduling the
/// user callback there would re-enter the gate, so the result is recorded while
/// `issuing` is true and the issuing permit flushes it after releasing the gate.
struct OpenDelivery {
    state: Arc<ControlState>,
    executor: Arc<dyn MainThreadExecutor>,
    generation: u64,
    accepted: AcceptedActionToken,
    request: AtomicU64,
    completion: Mutex<Option<OpenCompletionSlot>>,
    buffered: Mutex<Option<Result<(), ApplicationError>>>,
    issuing: AtomicBool,
}

impl OpenDelivery {
    /// Records a native completion, or schedules its delivery when the issuance
    /// gate is not held.
    fn receive(self: &Arc<Self>, result: Result<(), ApplicationError>) {
        let mut buffered = lock(&self.buffered);
        if self.issuing.load(Ordering::SeqCst) {
            // Holding the buffer lock across the check is what makes the
            // issuer's flush unable to miss a concurrently buffered result.
            *buffered = Some(result);
            drop(buffered);
            return;
        }
        drop(buffered);
        self.dispatch(result);
    }

    /// Delivers a result buffered during issuance, after the gate is released.
    fn flush(self: &Arc<Self>) {
        let buffered = lock(&self.buffered).take();
        if let Some(result) = buffered {
            self.dispatch(result);
        }
    }

    fn dispatch(self: &Arc<Self>, result: Result<(), ApplicationError>) {
        let executor = Arc::clone(&self.executor);
        let delivery = Arc::clone(self);
        let job = Box::new(move || delivery.deliver(result));
        let _ = executor.execute(job);
    }

    fn deliver(&self, result: Result<(), ApplicationError>) {
        let context = OpenCompletionContext {
            handle: self.accepted.handle,
            generation: self.generation,
            revision: self.accepted.revision,
            request: self.request.load(Ordering::SeqCst),
        };
        let event =
            if self
                .state
                .is_completion_current(self.generation, self.accepted, context.request)
            {
                OpenCompletion::Current(context, result)
            } else {
                OpenCompletion::Stale(context, result)
            };
        if let Some(completion) = { lock(&self.completion).take() } {
            let _ = catch_unwind(AssertUnwindSafe(|| completion(event)));
        }
    }
}

/// A one-shot permit for one irreversible native request.
///
/// The control creates it for a queued action and passes it to the backend. The
/// backend performs every non-committing step first (instance identity, path and
/// configuration checks, test barriers) and calls [`IssuancePermit::issue`] at
/// the exact native send site. `issue` revalidates the accepted handle,
/// generation and revision and the request deadline under the shared commit
/// gate, then runs the send while holding that gate. Acceptance and stop
/// invalidation therefore linearize against the native request itself: either
/// the request is issued first, or the newer decision is visible before issuance
/// and the request is dropped.
///
/// Nothing else runs under the gate. `issue` never waits for an acknowledgement,
/// never dispatches, never adds or removes observation and never calls user
/// code. A backend must not perform preparation inside `issue`; only the
/// immediate native call belongs there.
#[must_use = "the request is only issued through this permit"]
pub struct IssuancePermit<'a> {
    state: &'a ControlState,
    generation: u64,
    accepted: AcceptedActionToken,
    request: &'a RequestCommit,
    open: Option<&'a Arc<OpenDelivery>>,
}

impl IssuancePermit<'_> {
    /// Issues the native request when the accepted action is still current.
    ///
    /// Returns `None` when the accepted token is stale, the request was
    /// abandoned by an acknowledgement timeout, or the deadline passed before
    /// issuance. In those cases `send` never runs and no native action happens.
    /// `send` runs at most once and must perform only the immediate irreversible
    /// native call.
    pub fn issue<T>(&self, send: impl FnOnce() -> T) -> Option<T> {
        // A logically invalidated generation is inert immediately, even while a
        // stalled native send still holds the commit gate. There is nothing to
        // flush because this request never entered the send.
        if self.state.is_invalidated(self.generation) {
            return None;
        }
        if let Some(open) = self.open {
            open.issuing.store(true, Ordering::SeqCst);
        }
        let mut gate = lock(&self.state.gate);
        let current = self.accepted.handle == self.state.handle
            && self.accepted.generation == self.generation
            && gate.active == self.generation
            && gate.revision == self.accepted.revision
            && !self.state.is_invalidated(self.generation);
        if !current || !self.request.try_commit() {
            drop(gate);
            if let Some(open) = self.open {
                open.issuing.store(false, Ordering::SeqCst);
                open.flush();
            }
            return None;
        }
        if let Some(open) = self.open {
            gate.next_request += 1;
            gate.latest_request = gate.next_request;
            open.request.store(gate.next_request, Ordering::SeqCst);
        }
        let output = send();
        drop(gate);
        if let Some(open) = self.open {
            open.issuing.store(false, Ordering::SeqCst);
            open.flush();
        }
        Some(output)
    }
}

/// The commit state of one queued native action.
///
/// A request may commit only inside its acknowledgement window. A timeout
/// abandons a request that never committed, so it can never issue a native
/// action later. An already committed request cannot be undone and may finish
/// after the timeout.
struct RequestCommit {
    state: AtomicU8,
    deadline: Instant,
}

const REQUEST_PENDING: u8 = 0;
const REQUEST_COMMITTED: u8 = 1;
const REQUEST_ABANDONED: u8 = 2;

impl RequestCommit {
    const fn new(deadline: Instant) -> Self {
        Self {
            state: AtomicU8::new(REQUEST_PENDING),
            deadline,
        }
    }

    fn try_commit(&self) -> bool {
        if Instant::now() >= self.deadline {
            return false;
        }
        self.state
            .compare_exchange(
                REQUEST_PENDING,
                REQUEST_COMMITTED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    /// Abandons a pending request and reports whether it can never issue.
    fn abandon_confirmed(&self) -> bool {
        match self.state.compare_exchange(
            REQUEST_PENDING,
            REQUEST_ABANDONED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => true,
            Err(current) => current == REQUEST_ABANDONED,
        }
    }

    /// Whether the request has not committed to the native send.
    fn is_unissued(&self) -> bool { self.state.load(Ordering::SeqCst) != REQUEST_COMMITTED }

    fn abandon(&self) {
        let _ = self.state.compare_exchange(
            REQUEST_PENDING,
            REQUEST_ABANDONED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

enum AckState<T> {
    Pending,
    Ready(T),
    Closed,
}

struct Ack<T> {
    state: Mutex<AckState<T>>,
    signal: Condvar,
}

enum AckWait<T> {
    Ready(T),
    TimedOut,
    Cancelled,
}

impl<T> Ack<T> {
    const fn new() -> Self {
        Self {
            state: Mutex::new(AckState::Pending),
            signal: Condvar::new(),
        }
    }

    fn complete(&self, value: T) {
        let mut state = lock(&self.state);
        if matches!(&*state, AckState::Pending) {
            *state = AckState::Ready(value);
            drop(state);
            self.signal.notify_all();
        }
    }

    fn wait(&self, budget: Duration) -> AckWait<T> {
        let deadline = Instant::now() + budget;
        let mut state = lock(&self.state);
        loop {
            match &*state {
                AckState::Ready(_) => {
                    let AckState::Ready(value) = std::mem::replace(&mut *state, AckState::Closed)
                    else {
                        unreachable!("matched Ready");
                    };
                    return AckWait::Ready(value);
                }
                AckState::Closed => return AckWait::TimedOut,
                AckState::Pending => {}
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return AckWait::TimedOut;
            };
            let (next, _) = self
                .signal
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = next;
        }
    }

    /// Waits for the acknowledgement or lifecycle cancellation.
    ///
    /// The ack condvar is used with a bounded slice so an acknowledgement is
    /// still observed immediately, while cancellation is rechecked at least
    /// every `quantum`. Cancellation wins over a simultaneous success: the
    /// caller rolls back the exact generation.
    fn wait_cancellable(
        &self,
        budget: Duration,
        cancellation: &CancellationToken,
        quantum: Duration,
    ) -> AckWait<T> {
        let deadline = Instant::now() + budget;
        let mut state = lock(&self.state);
        loop {
            if cancellation.is_cancelled() {
                return AckWait::Cancelled;
            }
            match &*state {
                AckState::Ready(_) => {
                    let AckState::Ready(value) = std::mem::replace(&mut *state, AckState::Closed)
                    else {
                        unreachable!("matched Ready");
                    };
                    return AckWait::Ready(value);
                }
                AckState::Closed => return AckWait::TimedOut,
                AckState::Pending => {}
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return AckWait::TimedOut;
            };
            let slice = remaining.min(quantum);
            let (next, _) =
                self.signal.wait_timeout(state, slice).unwrap_or_else(PoisonError::into_inner);
            state = next;
        }
    }
}

enum InstallAck {
    Done(Result<(), ApplicationError>),
    Aborted,
}

enum QueryAck<T> {
    Done(T),
    Stale,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The real `AppKit` backend.
///
/// The value is stateless; its observer and instance state lives in a
/// main-thread-local map keyed by [`BackendHandle`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemApplicationBackend;

struct ObserverState {
    generation: u64,
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    /// The optional workspace activation observer of the same generation.
    activation: Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    /// The lifecycle observers (launch, terminate, hide, unhide); empty when not
    /// installed.
    lifecycle: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

/// Retained native instances for one control, owned by one generation.
///
/// Tokens are scoped by the generation that interned them: an [`ApplicationId`]
/// from an earlier generation never names a successor's instance because the
/// generation differs, even though a fresh store restarts its token counter.
/// Entries are pruned when an instance terminates, and the whole per-handle
/// record is dropped once removal is acknowledged.
#[derive(Default)]
struct InstanceStore {
    generation: u64,
    next: u64,
    entries: BTreeMap<u64, Retained<NSRunningApplication>>,
    /// Pruned terminated instances, oldest first, so a `DidTerminate` that runs
    /// after another application's intern still names the original id.
    tombstones: VecDeque<(u64, Retained<NSRunningApplication>)>,
    /// Pids resolved from the executable for instances macOS reports none for,
    /// so the process table is scanned once per instance, not on every launch
    /// or listing. Dropped with the instance when it is pruned.
    resolved_pids: BTreeMap<u64, i32>,
}

/// The most pruned terminated instances kept to resolve a late `DidTerminate`.
/// The oldest is evicted first; an evicted termination is reported as
/// `LifecycleKind::TerminatedUnresolved`.
const TERMINATED_TOMBSTONES: usize = 64;

impl InstanceStore {
    /// Drops terminated instances from the live set, moving them to the bounded
    /// tombstone list instead of forgetting their ids.
    fn prune(&mut self) {
        let dead: Vec<u64> = self
            .entries
            .iter()
            .filter(|(_, application)| application.isTerminated())
            .map(|(token, _)| *token)
            .collect();
        for token in dead {
            self.resolved_pids.remove(&token);
            if let Some(application) = self.entries.remove(&token) {
                self.tombstones.push_back((token, application));
                while self.tombstones.len() > TERMINATED_TOMBSTONES {
                    self.tombstones.pop_front();
                }
            }
        }
    }

    /// The pid to read `application` (interned as `id`) with: see
    /// [`application_pid`]. A resolved pid is cached per instance.
    fn pid_of(&mut self, id: ApplicationId, application: &NSRunningApplication) -> Option<i32> {
        let reported = application.processIdentifier();
        if reported > 0 {
            return Some(reported);
        }
        if let Some(pid) = self.resolved_pids.get(&id.token) {
            return Some(*pid);
        }
        let pid = application_pid(application)?;
        self.resolved_pids.insert(id.token, pid);
        Some(pid)
    }

    /// The token of an already interned equal instance, without pruning.
    ///
    /// A terminated instance must keep resolving to the id a consumer holds, so
    /// this never calls [`Self::prune`], never inserts and also checks the
    /// tombstones of pruned instances.
    fn find(&self, application: &NSRunningApplication) -> Option<u64> {
        let equal = |candidate: &Retained<NSRunningApplication>| {
            candidate.isEqual(Some(AsRef::<AnyObject>::as_ref(application)))
        };
        self.entries
            .iter()
            .find_map(|(token, candidate)| equal(candidate).then_some(*token))
            .or_else(|| {
                self.tombstones
                    .iter()
                    .find_map(|(token, candidate)| equal(candidate).then_some(*token))
            })
    }

    /// Resolves a termination and clears its tombstone: the id is delivered once.
    fn take_terminated(&mut self, application: &NSRunningApplication) -> Option<u64> {
        let token = self.find(application)?;
        self.tombstones.retain(|(held, _)| *held != token);
        Some(token)
    }

    fn intern(
        &mut self,
        handle: BackendHandle,
        generation: u64,
        application: &NSRunningApplication,
    ) -> ApplicationId {
        self.prune();
        // A pruned (terminated) instance keeps its id while tombstoned, so a
        // late notification for it never mints a second id.
        let existing = self.find(application);
        let token = existing.unwrap_or_else(|| {
            let token = self.next;
            self.next += 1;
            // SAFETY: `application` is a live Objective-C object owned by the
            // caller for this call; retaining it through its pointer is exactly
            // what `Retained::retain` is for, and the pointer is non-null.
            let retained = unsafe { Retained::retain(std::ptr::from_ref(application).cast_mut()) }
                .expect("a live NSRunningApplication cannot be null");
            self.entries.insert(token, retained);
            token
        });
        ApplicationId { handle, generation, token }
    }
}

thread_local! {
    static OBSERVERS: RefCell<BTreeMap<BackendHandle, ObserverState>> =
        const { RefCell::new(BTreeMap::new()) };
    static INSTANCES: RefCell<BTreeMap<BackendHandle, InstanceStore>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Whether a delivered notification still belongs to the installed observer.
///
/// This is the native ingestion gate: a queued block from a removed generation
/// must not intern a token or reach a successor generation's sink.
fn ingestion_is_current(installed: Option<u64>, delivered: u64) -> bool {
    delivered != 0 && installed == Some(delivered)
}

/// Queues the pure part of one will-launch delivery for a later main turn.
///
/// The native callback body can run synchronously inside a native send whose
/// [`IssuancePermit`] still holds the commit gate. Calling the observer sink
/// there would re-enter the gate on the same thread and deadlock, so only
/// interning the retained instance stays inside the callback. The
/// [`ApplicationId`] and the cloned `Send` sink travel to the queue instead;
/// the sink rechecks the generation when the delivery finally runs.
///
/// A rejected enqueue drops the delivery: the notification is lost, and it is
/// never run inline as a fallback, because inline delivery is exactly the
/// re-entry this function exists to prevent.
fn defer_notification(
    enqueue: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), ThreadError>,
    notify: Arc<dyn Fn(ApplicationId) + Send + Sync>,
    application: ApplicationId,
) {
    let delivery: Box<dyn FnOnce() + Send> = Box::new(move || notify(application));
    let _ = enqueue(delivery);
}

impl SystemApplicationBackend {
    /// The retained instance for an exact, current-generation id.
    fn retained_instance(
        handle: BackendHandle,
        application: ApplicationId,
    ) -> Option<Retained<NSRunningApplication>> {
        if application.handle != handle {
            return None;
        }
        INSTANCES.with(|cell| {
            cell.borrow().get(&handle).and_then(|store| {
                (store.generation == application.generation)
                    .then(|| store.entries.get(&application.token).cloned())
                    .flatten()
            })
        })
    }
}

fn require_main() -> Result<MainThreadMarker, ApplicationError> {
    MainThreadMarker::new().ok_or(ApplicationError::NotMainThread)
}

/// The pid of a live application for Accessibility reads: its reported
/// `processIdentifier`, or (when macOS reports none, as for Xcode's Device Hub)
/// the single live process running its executable. Never identity.
fn application_pid(application: &NSRunningApplication) -> Option<i32> {
    let executable = application
        .executableURL()
        .and_then(|url| url.path())
        .map(|path| path.to_string());
    pid::resolve(application.processIdentifier(), executable.as_deref())
}

impl ApplicationBackend for SystemApplicationBackend {
    fn observe(
        &self,
        handle: BackendHandle,
        generation: u64,
        notify: Arc<dyn Fn(ApplicationId) + Send + Sync>,
    ) -> Result<(), ApplicationError> {
        let _main = require_main()?;
        OBSERVERS.with(|cell| {
            let mut observers = cell.borrow_mut();
            if observers.contains_key(&handle) {
                return Err(ApplicationError::AlreadyInstalled);
            }
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            let queue = NSOperationQueue::mainQueue();
            let generation_for_block = generation;
            let block = RcBlock::new(move |notification: NonNull<NSNotification>| {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    // Gate on the installed generation before interning: a
                    // queued old block must not recreate or pollute a store.
                    let installed = OBSERVERS
                        .with(|cell| cell.borrow().get(&handle).map(|state| state.generation));
                    if !ingestion_is_current(installed, generation_for_block) {
                        return;
                    }
                    // SAFETY: the notification center invokes the block with a
                    // valid notification for the duration of the call.
                    let notification = unsafe { notification.as_ref() };
                    let Some(application) = launch_application(notification) else {
                        return;
                    };
                    let id = INSTANCES.with(|cell| {
                        let mut stores = cell.borrow_mut();
                        let store = stores.get_mut(&handle)?;
                        if store.generation != generation_for_block {
                            return None;
                        }
                        Some(store.intern(handle, generation_for_block, &application))
                    });
                    if let Some(id) = id {
                        // The native callback may be running inside a send
                        // whose permit still holds the commit gate. Interning
                        // above is pure main-thread state; the sink is the
                        // part that takes the gate, so it is deferred.
                        defer_notification(
                            crate::main_thread::run_on_main,
                            Arc::clone(&notify),
                            id,
                        );
                    }
                }));
            });
            let block: &DynBlock<dyn Fn(NonNull<NSNotification>)> = &block;
            // SAFETY: the notification name and application key are immutable
            // AppKit constants; the observer object is `None`; delivery is
            // explicitly on the main operation queue, and the center retains the
            // block until the token is removed in `unobserve`.
            let token = unsafe {
                let name = NSWorkspaceWillLaunchApplicationNotification;
                center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    None,
                    Some(&queue),
                    block,
                )
            };
            observers.insert(handle, ObserverState {
                generation,
                center,
                token,
                activation: None,
                lifecycle: Vec::new(),
            });
            INSTANCES.with(|cell| {
                let mut stores = cell.borrow_mut();
                let store = stores.entry(handle).or_default();
                if store.generation != generation {
                    store.entries.clear();
                }
                store.generation = generation;
                store.prune();
            });
            Ok(())
        })
    }

    fn unobserve(&self, handle: BackendHandle, generation: u64) -> Result<(), ApplicationError> {
        let _main = require_main()?;
        let removed = OBSERVERS.with(|cell| {
            let mut observers = cell.borrow_mut();
            match observers.get(&handle) {
                Some(state) if state.generation == generation => observers.remove(&handle),
                _ => None,
            }
        });
        let superseded = OBSERVERS.with(|cell| {
            cell.borrow().get(&handle).is_some_and(|state| state.generation != generation)
        });
        if superseded {
            return Ok(());
        }
        if let Some(state) = removed {
            // SAFETY: the tokens came from this center and have not been removed;
            // removal is idempotent for an installed observer. The activation
            // token leaves with its generation's will-launch observer.
            unsafe {
                state.center.removeObserver(AsRef::<AnyObject>::as_ref(&state.token));
                if let Some(activation) = &state.activation {
                    state.center.removeObserver(AsRef::<AnyObject>::as_ref(activation));
                }
                for token in &state.lifecycle {
                    state.center.removeObserver(AsRef::<AnyObject>::as_ref(token));
                }
            }
        }
        INSTANCES.with(|cell| {
            let mut stores = cell.borrow_mut();
            // Drop the record once removal is acknowledged; an empty per-handle
            // tombstone would only be needed to keep tokens monotonic, and
            // generation identity already isolates successors. A queued stale
            // notification block only looks up this map and never creates it.
            if stores.get(&handle).is_some_and(|store| store.generation == generation) {
                stores.remove(&handle);
            }
        });
        Ok(())
    }

    fn running_applications(
        &self,
        handle: BackendHandle,
        bundle_id: &str,
    ) -> Result<Vec<ApplicationId>, ApplicationError> {
        let _main = require_main()?;
        let workspace = NSWorkspace::sharedWorkspace();
        let running = workspace.runningApplications();
        let generation =
            OBSERVERS.with(|cell| cell.borrow().get(&handle).map(|state| state.generation));
        let Some(generation) = generation else {
            return Ok(Vec::new());
        };
        Ok(INSTANCES.with(|cell| {
            let mut stores = cell.borrow_mut();
            let Some(store) = stores.get_mut(&handle) else {
                return Vec::new();
            };
            if store.generation != generation {
                return Vec::new();
            }
            store.prune();
            let mut matches = Vec::new();
            for application in running.to_vec() {
                if application.isTerminated() {
                    continue;
                }
                let matches_id =
                    application.bundleIdentifier().is_some_and(|id| id.to_string() == bundle_id);
                if matches_id {
                    matches.push(store.intern(handle, generation, &application));
                }
            }
            matches
        }))
    }

    fn is_running(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
    ) -> Result<bool, ApplicationError> {
        let _main = require_main()?;
        if application.handle != handle {
            return Ok(false);
        }
        let generation =
            OBSERVERS.with(|cell| cell.borrow().get(&handle).map(|state| state.generation));
        if generation != Some(application.generation) {
            return Ok(false);
        }
        let current = INSTANCES.with(|cell| {
            cell.borrow().get(&handle).and_then(|store| {
                (store.generation == application.generation)
                    .then(|| store.entries.get(&application.token).cloned())
                    .flatten()
            })
        });
        let Some(current) = current else {
            return Ok(false);
        };
        if current.isTerminated() {
            return Ok(false);
        }
        let workspace = NSWorkspace::sharedWorkspace();
        let running = workspace.runningApplications();
        Ok(running
            .to_vec()
            .iter()
            .any(|candidate| candidate.isEqual(Some(AsRef::<AnyObject>::as_ref(&*current)))))
    }

    fn bundle_identifier(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
    ) -> Result<Option<String>, ApplicationError> {
        let _main = require_main()?;
        if application.handle != handle {
            return Ok(None);
        }
        let generation =
            OBSERVERS.with(|cell| cell.borrow().get(&handle).map(|state| state.generation));
        if generation != Some(application.generation) {
            return Ok(None);
        }
        let current = INSTANCES.with(|cell| {
            cell.borrow().get(&handle).and_then(|store| {
                (store.generation == application.generation)
                    .then(|| store.entries.get(&application.token).cloned())
                    .flatten()
            })
        });
        Ok(current.and_then(|current| current.bundleIdentifier()).map(|id| id.to_string()))
    }

    fn bundle_identifier_at_path(
        &self,
        _handle: BackendHandle,
        path: &Path,
    ) -> Result<Option<String>, ApplicationError> {
        let _main = require_main()?;
        // A non-UTF-8 path cannot be expressed as an `NSString` path, and a
        // directory that is not an application bundle has no identifier: both
        // are answers, not failed queries.
        let Some(path) = path.to_str() else {
            return Ok(None);
        };
        Ok(NSBundle::bundleWithPath(&NSString::from_str(path))
            .and_then(|bundle| bundle.bundleIdentifier())
            .map(|bundle_id| bundle_id.to_string()))
    }

    fn application_path(
        &self,
        _handle: BackendHandle,
        bundle_id: &str,
    ) -> Result<Option<PathBuf>, ApplicationError> {
        let _main = require_main()?;
        let workspace = NSWorkspace::sharedWorkspace();
        let url = workspace.URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id));
        Ok(url.and_then(|url| url.path()).map(|path| PathBuf::from(path.to_string())))
    }

    fn request_terminate(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
        permit: IssuancePermit<'_>,
    ) -> Result<TerminateOutcome, ApplicationError> {
        let _main = require_main()?;
        // Preflight is deliberately outside the gate: resolving and comparing
        // the retained instance is not the irreversible step.
        if !self.is_running(handle, application)? {
            return Ok(TerminateOutcome::NotRunning);
        }
        let current = INSTANCES.with(|cell| {
            cell.borrow().get(&handle).and_then(|store| {
                (store.generation == application.generation)
                    .then(|| store.entries.get(&application.token).cloned())
                    .flatten()
            })
        });
        let Some(current) = current else {
            return Ok(TerminateOutcome::NotRunning);
        };
        let Some(outcome) = permit.issue(|| {
            if current.forceTerminate() {
                TerminateOutcome::Requested
            } else {
                TerminateOutcome::Refused
            }
        }) else {
            return Ok(TerminateOutcome::Stale);
        };
        Ok(outcome)
    }

    fn observe_activation(
        &self,
        handle: BackendHandle,
        generation: u64,
        notify: ActivationSink,
    ) -> Result<(), ApplicationError> {
        let _main = require_main()?;
        OBSERVERS.with(|cell| {
            let mut observers = cell.borrow_mut();
            let Some(state) = observers.get_mut(&handle) else {
                return Err(ApplicationError::NotInstalled);
            };
            if state.generation != generation {
                return Err(ApplicationError::NotInstalled);
            }
            if state.activation.is_some() {
                return Err(ApplicationError::AlreadyInstalled);
            }
            let queue = NSOperationQueue::mainQueue();
            let block = RcBlock::new(move |_notification: NonNull<NSNotification>| {
                // Contain panics; only invalidate and enqueue. No query, wait or
                // policy runs here: the block may be running inside a native send.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    let installed = OBSERVERS
                        .with(|cell| cell.borrow().get(&handle).map(|state| state.generation));
                    if !ingestion_is_current(installed, generation) {
                        return;
                    }
                    frontmost::defer_activation(crate::main_thread::run_on_main, &notify);
                }));
            });
            let block: &DynBlock<dyn Fn(NonNull<NSNotification>)> = &block;
            // SAFETY: the notification name is an immutable AppKit constant; the
            // observer object is `None`; delivery is explicitly on the main
            // operation queue, and the center retains the block until the token
            // is removed in `unobserve`/`unobserve_activation`.
            let token = unsafe {
                let name = NSWorkspaceDidActivateApplicationNotification;
                state.center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    None,
                    Some(&queue),
                    block,
                )
            };
            state.activation = Some(token);
            Ok(())
        })
    }

    fn unobserve_activation(
        &self,
        handle: BackendHandle,
        generation: u64,
    ) -> Result<(), ApplicationError> {
        let _main = require_main()?;
        OBSERVERS.with(|cell| {
            let mut observers = cell.borrow_mut();
            let Some(state) = observers.get_mut(&handle) else {
                return;
            };
            if state.generation != generation {
                return;
            }
            if let Some(token) = state.activation.take() {
                // SAFETY: the token came from this center and has not been
                // removed; removal is idempotent for an installed observer.
                unsafe {
                    state.center.removeObserver(AsRef::<AnyObject>::as_ref(&token));
                }
            }
        });
        Ok(())
    }

    fn running_details(
        &self,
        handle: BackendHandle,
    ) -> Result<Vec<FrontmostApplication>, ApplicationError> {
        let _main = require_main()?;
        let generation = OBSERVERS
            .with(|cell| cell.borrow().get(&handle).map(|state| state.generation))
            .ok_or(ApplicationError::NotInstalled)?;
        let running = NSWorkspace::sharedWorkspace().runningApplications();
        let current = NSRunningApplication::currentApplication();
        INSTANCES
            .with(|cell| {
                let mut stores = cell.borrow_mut();
                let store = stores.get_mut(&handle)?;
                if store.generation != generation {
                    return None;
                }
                store.prune();
                let mut details = Vec::new();
                for application in running.to_vec() {
                    if application.isTerminated() {
                        continue;
                    }
                    let id = store.intern(handle, generation, &application);
                    details.push(FrontmostApplication {
                        id,
                        bundle_identifier: application.bundleIdentifier().map(|id| id.to_string()),
                        name: application.localizedName().map(|name| name.to_string()),
                        is_current_process: application
                            .isEqual(Some(AsRef::<AnyObject>::as_ref(&*current))),
                        pid: store.pid_of(id, &application),
                        regular: application.activationPolicy()
                            == NSApplicationActivationPolicy::Regular,
                    });
                }
                Some(details)
            })
            .ok_or(ApplicationError::NotInstalled)
    }

    fn frontmost(
        &self,
        handle: BackendHandle,
    ) -> Result<Option<FrontmostApplication>, ApplicationError> {
        let _main = require_main()?;
        let generation = OBSERVERS
            .with(|cell| cell.borrow().get(&handle).map(|state| state.generation))
            .ok_or(ApplicationError::NotInstalled)?;
        let Some(application) = NSWorkspace::sharedWorkspace().frontmostApplication() else {
            return Ok(None);
        };
        if application.isTerminated() {
            return Ok(None);
        }
        // Self comparison is by retained-instance equality against the current
        // process's own `NSRunningApplication`, never by executable name.
        let current = NSRunningApplication::currentApplication();
        let is_current_process = application.isEqual(Some(AsRef::<AnyObject>::as_ref(&*current)));
        let (id, pid) = INSTANCES
            .with(|cell| {
                let mut stores = cell.borrow_mut();
                let store = stores.get_mut(&handle)?;
                (store.generation == generation).then(|| {
                    let id = store.intern(handle, generation, &application);
                    (id, store.pid_of(id, &application))
                })
            })
            .ok_or(ApplicationError::NotInstalled)?;
        Ok(Some(FrontmostApplication {
            id,
            bundle_identifier: application.bundleIdentifier().map(|id| id.to_string()),
            name: application.localizedName().map(|name| name.to_string()),
            is_current_process,
            pid,
            regular: application.activationPolicy() == NSApplicationActivationPolicy::Regular,
        }))
    }

    fn request_graceful_terminate(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
        authorization: &PressAuthorization,
        permit: IssuancePermit<'_>,
    ) -> Result<GracefulOutcome, ApplicationError> {
        let _main = require_main()?;
        // Preflight is deliberately outside the gate and immediately before the
        // permit: exact identity running, current frontmost, not Finder/self.
        match frontmost::preflight_on_main(self, handle, application)? {
            FrontmostPreflight::Eligible(_) => {}
            FrontmostPreflight::NotRunning => return Ok(GracefulOutcome::NotRunning),
            FrontmostPreflight::Rejected(reason) => return Ok(GracefulOutcome::Rejected(reason)),
        }
        let Some(current) = Self::retained_instance(handle, application) else {
            return Ok(GracefulOutcome::NotRunning);
        };
        let Some(outcome) = permit.issue(|| {
            // One-shot ordering with a concurrent cancel: whichever CAS wins.
            if !authorization.commit_send() {
                return GracefulOutcome::Cancelled;
            }
            // `terminate` only; never `forceTerminate`.
            if current.terminate() {
                GracefulOutcome::Requested
            } else {
                GracefulOutcome::Refused
            }
        }) else {
            return Ok(GracefulOutcome::Stale);
        };
        Ok(outcome)
    }

    fn request_activate(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
        authorization: &ActivationAuthorization,
        permit: IssuancePermit<'_>,
    ) -> Result<ActivationOutcome, ApplicationError> {
        let _main = require_main()?;
        // Preflight is outside the gate and immediately before the permit: the
        // exact retained instance of the current generation must still be live.
        if !self.is_running(handle, application)? {
            return Ok(ActivationOutcome::Stale);
        }
        let Some(current) = Self::retained_instance(handle, application) else {
            return Ok(ActivationOutcome::Stale);
        };
        let Some(outcome) = permit.issue(|| {
            if authorization.is_expired() {
                return ActivationOutcome::ExpiredBeforeSend;
            }
            // One-shot ordering with a concurrent cancel: whichever CAS wins.
            if !authorization.commit_send() {
                return ActivationOutcome::Cancelled;
            }
            // Non-deprecated public activation of exactly this instance; see
            // the `activation` module documentation for the SDK evidence.
            if current.activateWithOptions(NSApplicationActivationOptions::empty()) {
                ActivationOutcome::Requested
            } else {
                ActivationOutcome::Refused
            }
        }) else {
            return Ok(ActivationOutcome::Stale);
        };
        Ok(outcome)
    }

    fn request_visibility(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
        operation: VisibilityOperation,
        authorization: &VisibilityAuthorization,
        permit: IssuancePermit<'_>,
    ) -> Result<VisibilityOutcome, ApplicationError> {
        let _main = require_main()?;
        // Preflight is outside the gate and immediately before the permit: the
        // exact retained instance of the current generation must still be live.
        if !self.is_running(handle, application)? {
            return Ok(VisibilityOutcome::Stale);
        }
        let Some(current) = Self::retained_instance(handle, application) else {
            return Ok(VisibilityOutcome::Stale);
        };
        let Some(outcome) = permit.issue(|| {
            if authorization.is_expired() {
                return VisibilityOutcome::ExpiredBeforeSend;
            }
            // One-shot ordering with a concurrent cancel: whichever CAS wins.
            if !authorization.commit_send() {
                return VisibilityOutcome::Cancelled;
            }
            // Public `-[NSRunningApplication hide]`/`unhide` of exactly this
            // instance; see the `visibility` module documentation.
            let accepted = match operation {
                VisibilityOperation::Hide => current.hide(),
                VisibilityOperation::Unhide => current.unhide(),
            };
            if accepted {
                VisibilityOutcome::Requested
            } else {
                VisibilityOutcome::Refused
            }
        }) else {
            return Ok(VisibilityOutcome::Stale);
        };
        Ok(outcome)
    }

    fn observe_lifecycle(
        &self,
        handle: BackendHandle,
        generation: u64,
        notify: LifecycleSink,
    ) -> Result<(), ApplicationError> {
        lifecycle::observe_system(handle, generation, &notify)
    }

    fn unobserve_lifecycle(
        &self,
        handle: BackendHandle,
        generation: u64,
    ) -> Result<(), ApplicationError> {
        lifecycle::unobserve_system(handle, generation)
    }

    fn display_hint(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
    ) -> Result<DisplayHint, ApplicationError> {
        let mtm = require_main()?;
        let Some(current) = Self::retained_instance(handle, application) else {
            return Ok(DisplayHint::none(DisplaySource::NotValidated));
        };
        let pid = current.processIdentifier();
        let screens: Vec<ScreenRecord> = NSScreen::screens(mtm)
            .to_vec()
            .iter()
            .map(|screen| {
                let frame = screen.frame();
                ScreenRecord {
                    id: crate::desktop::DisplayId::new(screen.CGDirectDisplayID()),
                    frame: DisplayRect::new(
                        frame.origin.x,
                        frame.origin.y,
                        frame.size.width,
                        frame.size.height,
                    ),
                }
            })
            .collect();
        let windows = display::read_window_records();
        Ok(select_display(windows.as_deref(), pid, &screens))
    }

    fn open(
        &self,
        _handle: BackendHandle,
        path: &Path,
        completion: NativeOpenCompletion,
        permit: IssuancePermit<'_>,
    ) -> Result<OpenRequest, ApplicationError> {
        let _main = require_main()?;
        // Path, URL and configuration preparation happen before the permit, not
        // under the commit gate.
        let path = path
            .to_str()
            .ok_or_else(|| ApplicationError::OpenFailed("path is not valid UTF-8".to_owned()))?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        let configuration = NSWorkspaceOpenConfiguration::configuration();
        let workspace = NSWorkspace::sharedWorkspace();
        let completion = Arc::new(Mutex::new(Some(completion)));
        let slot = Arc::clone(&completion);
        let block = RcBlock::new(
            move |_application: *mut NSRunningApplication, error: *mut NSError| {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    let Some(completion) = lock(&slot).take() else {
                        return;
                    };
                    let result = if error.is_null() {
                        Ok(())
                    } else {
                        // SAFETY: AppKit passes a valid error object for the
                        // duration of the completion handler.
                        let message = unsafe { &*error }.localizedDescription().to_string();
                        Err(ApplicationError::OpenFailed(message))
                    };
                    completion(result);
                }));
            },
        );
        let block: &DynBlock<dyn Fn(*mut NSRunningApplication, *mut NSError)> = &block;
        // SAFETY-free call: the generated binding is a safe method. The block is
        // copied by AppKit; the pointer arguments are only read inside it. Only
        // this immediate send runs under the issuance permit.
        let Some(()) = permit.issue(|| {
            workspace.openApplicationAtURL_configuration_completionHandler(
                &url,
                &configuration,
                Some(block),
            );
        }) else {
            return Ok(OpenRequest::Stale);
        };
        Ok(OpenRequest::Issued)
    }
}

fn launch_application(notification: &NSNotification) -> Option<Retained<NSRunningApplication>> {
    let info = notification.userInfo()?;
    // SAFETY: `NSWorkspaceApplicationKey` is an immutable AppKit constant.
    let key = unsafe { NSWorkspaceApplicationKey };
    let key: &AnyObject = AsRef::<AnyObject>::as_ref(key);
    let value = info.objectForKey(key)?;
    value.downcast::<NSRunningApplication>().ok()
}

#[cfg(any(test, feature = "testing"))]
pub mod testing;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod display_tests;

#[cfg(test)]
mod frontmost_tests;

#[cfg(test)]
mod graceful_tests;

#[cfg(test)]
mod activation_tests;

#[cfg(test)]
mod visibility_tests;

#[cfg(test)]
mod lifecycle_tests;
