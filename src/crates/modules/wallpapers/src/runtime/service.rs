//! The host-lifetime explicit-action and rotation coordinator.
//!
//! One lazily created thread owns every manual and rotation job. Admission is
//! bounded to one executing job plus at most one queued job; a caller may wait
//! for the queue slot only until its absolute deadline. All mutex metadata is
//! released before filesystem, processing or native work, and the coordinator
//! owns its synchronous stages until they return.
//!
//! The service is deliberately separate from a rotation session: registration
//! and a stopped module still expose explicit `wallpapers.set`, while
//! `Module.stop` invalidates only the session epoch, its timer and its queued
//! rotation work.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use stache_macos::{
    DesktopClock, DesktopError, DesktopOperation, DisplayId, GuardedDesktopBackend,
    NativeOperationTracker,
};
use stache_sdk::{
    AcceptedConfig, AcceptedSubscription, AcceptedWatch, AppPaths, CancelListener,
    CancellationToken, ScopedLogger, ShutdownWait, wait_for_finish,
};

use super::canonical::canonical_directory;
use super::clock::{
    RandomSource, RuntimeClock, RuntimeSignal, SystemRandom, SystemRuntimeClock, WaitOutcome,
};
use super::selection::{PoolIdentity, PoolSnapshot, SelectionState};
use super::session::SessionControl;
use super::types::{
    WallpaperScreen, WallpaperSelection, WallpapersOutcomeCause, WallpapersOutcomeStatus,
    WallpapersSetOutcome, WallpapersSetParams,
};
use crate::WallpapersConfig;
use crate::apply::{PlannedTarget, PrepareError, PreparePlanDeps, Prepared, prepare_plan_with};
use crate::cache::NoFailures;
use crate::config::WallpaperMode;
use crate::memo::PrepareMemo;
use crate::processing::Effects;
use crate::selection::{SelectedTarget, SelectionError, resolve_targets};
use crate::sources::{
    SourceContext, SourceError, SourcePool, find_pool_entry, resolve_explicit_file, resolve_pool,
};

/// The client's finite response wait for one cold `wallpapers.set`.
///
/// Part 04's client passes this through `IpcClient::with_options`. It is
/// provisional until the real cold multi-display benchmark in the integration
/// slice replaces it with measured evidence.
pub const WALLPAPERS_CLIENT_BUDGET: Duration = Duration::from_secs(120);

/// The server/coordinator budget for one request, including queue admission
/// and processing, leaving a delivery margin under [`WALLPAPERS_CLIENT_BUDGET`].
pub const WALLPAPERS_SERVER_BUDGET: Duration = Duration::from_secs(115);

/// The service construction failure.
#[derive(Debug)]
pub struct ServiceError {
    message: String,
}

impl ServiceError {
    fn new(message: impl Into<String>) -> Self { Self { message: message.into() } }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.message) }
}

impl std::error::Error for ServiceError {}

/// Prepares every addressed output before a native change is issued.
///
/// Production uses [`RealProcessor`]. Tests inject a fake that can block, fail
/// or advance the injected clock; the production path is not bypassed.
pub trait Processor: Send + Sync {
    /// Validates and prepares one caller-resolved plan.
    ///
    /// # Errors
    /// Returns the same typed failure [`crate::prepare_plan`] reports.
    fn prepare(
        &self,
        plan: &[PlannedTarget],
        paths: &AppPaths,
        cancel: &CancellationToken,
    ) -> Result<Prepared, PrepareError>;
}

/// The production processor over the shared cache and image pipeline.
///
/// [`RealProcessor::host`] owns one [`PrepareMemo`] for its lifetime, so a
/// cached switch skips the source read and the output decode while neither
/// file changed. [`RealProcessor::default`] has no memo and behaves like the
/// standalone commands.
#[derive(Clone, Debug, Default)]
pub struct RealProcessor {
    memo: Option<Arc<PrepareMemo>>,
}

impl RealProcessor {
    /// A processor that owns a fresh per-process memo.
    #[must_use]
    pub fn host() -> Self {
        Self {
            memo: Some(Arc::new(PrepareMemo::new())),
        }
    }

    /// The owned memo, if any.
    #[must_use]
    pub fn memo(&self) -> Option<&PrepareMemo> { self.memo.as_deref() }
}

impl Processor for RealProcessor {
    fn prepare(
        &self,
        plan: &[PlannedTarget],
        paths: &AppPaths,
        cancel: &CancellationToken,
    ) -> Result<Prepared, PrepareError> {
        let deps = PreparePlanDeps { paths, memo: self.memo() };
        prepare_plan_with(plan, &deps, cancel, None, &NoFailures)
    }
}

/// A one-shot startup admission latch.
///
/// The initial rotation job is queued before the timer exists, so it may start
/// executing while startup is still establishing resources. It waits on this
/// latch before any native call; `open` admits it and `fail` makes it inert
/// when a later startup stage fails and rolls back.
pub(crate) struct StartGate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GateState {
    Pending,
    Open,
    Failed,
}

impl StartGate {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(GateState::Pending),
            changed: Condvar::new(),
        }
    }

    pub(crate) fn open(&self) {
        *lock(&self.state) = GateState::Open;
        self.changed.notify_all();
    }

    pub(crate) fn fail(&self) {
        *lock(&self.state) = GateState::Failed;
        self.changed.notify_all();
    }

    /// Waits for admission, returning `false` when startup failed or the job
    /// was cancelled.
    pub(crate) fn wait(self: &Arc<Self>, cancel: &CancellationToken) -> bool {
        if cancel.is_cancelled() {
            return false;
        }
        let gate = Arc::clone(self);
        let _listener = cancel.on_cancel(Arc::new(move || gate.wake()));
        let mut state = lock(&self.state);
        loop {
            match *state {
                GateState::Open => return true,
                GateState::Failed => return false,
                GateState::Pending => {}
            }
            if cancel.is_cancelled() {
                return false;
            }
            state = self.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn wake(&self) {
        let _state = lock(&self.state);
        self.changed.notify_all();
    }
}

/// Injected dependencies for one host-lifetime service.
pub struct ServiceConfig {
    /// Identity cache and runtime paths.
    pub paths: AppPaths,
    /// Configuration directory and home used for configured sources.
    pub source: SourceContext,
    /// Guarded desktop boundary. Production passes the native adapter; tests
    /// pass `FakeDesktop`.
    pub backend: Arc<dyn GuardedDesktopBackend>,
    /// Monotonic clock for deadlines and timers.
    pub clock: Arc<dyn RuntimeClock>,
    /// Random draw source.
    pub random: Arc<dyn RandomSource>,
    /// Processing coordinator. Production passes [`RealProcessor::host`].
    pub processor: Arc<dyn Processor>,
}

impl ServiceConfig {
    /// Production dependencies with the system clock and random source.
    #[must_use]
    pub fn production(
        paths: AppPaths,
        source: SourceContext,
        backend: Arc<dyn GuardedDesktopBackend>,
    ) -> Self {
        Self {
            paths,
            source,
            backend,
            clock: Arc::new(SystemRuntimeClock),
            random: Arc::new(SystemRandom),
            processor: Arc::new(RealProcessor::host()),
        }
    }
}

/// Observable service state, without retaining any job payload.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ServiceStatus {
    /// Admission is closed.
    pub closed: bool,
    /// The coordinator thread exists (it may be finishing its last job).
    pub coordinator_running: bool,
    /// One job is executing.
    pub executing: bool,
    /// One rotation job is queued. Manual jobs use the same one slot.
    pub queued: bool,
    /// The current rotation session, when a module is running.
    pub session: Option<SessionStatus>,
}

/// Observable session state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionStatus {
    /// Session identity.
    pub id: u64,
    /// Accepted interval in seconds.
    pub interval: u64,
    /// Adopted accepted revision.
    pub revision: u64,
    /// Whether the timer worker currently exists.
    pub timer_spawned: bool,
    /// Whether a next deadline is armed.
    pub deadline_present: bool,
    /// Current accepted pool length.
    pub pool_len: usize,
    /// How many complete successes reset the interval.
    pub full_success_resets: u64,
}

/// One host-lifetime wallpaper service, independent from a module runtime.
pub struct WallpapersService {
    paths: AppPaths,
    source: SourceContext,
    host_directory: PathBuf,
    backend: Arc<dyn GuardedDesktopBackend>,
    clock: Arc<dyn RuntimeClock>,
    random: Arc<dyn RandomSource>,
    processor: Arc<dyn Processor>,
    native: NativeOperationTracker,
    accepted: Mutex<Option<AcceptedWatch<WallpapersConfig>>>,
    accepted_subscription: Mutex<Option<AcceptedSubscription>>,
    coordinator: Mutex<CoordinatorState>,
    coordinator_signal: Arc<RuntimeSignal>,
    session: Mutex<Option<Arc<SessionControl>>>,
    next_session: AtomicU64,
    mutation_epoch: AtomicU64,
    selection: Mutex<SelectionState>,
    shutdown: AtomicBool,
    host_cancel: CancellationToken,
    self_ref: OnceLock<Weak<Self>>,
    #[cfg(test)]
    commit_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    fail_timer_spawn: AtomicBool,
}

struct CoordinatorState {
    closed: bool,
    queued: Option<Job>,
    executing: bool,
    thread: Option<JoinHandle<()>>,
}

enum Job {
    Manual(ManualJob),
    Rotation(RotationJob),
}

struct ManualJob {
    selection: WallpaperSelection,
    screen: WallpaperScreen,
    deadline: Option<Instant>,
    cancel: CancellationToken,
    _host_guard: Option<CancelListener>,
    issued: Arc<AtomicBool>,
    reply: mpsc::Sender<WallpapersSetOutcome>,
    wake: Arc<RuntimeSignal>,
}

struct RotationJob {
    session_id: u64,
    revision: u64,
    epoch: u64,
    cancel: CancellationToken,
    logger: ScopedLogger,
    reply: Option<mpsc::Sender<RotationReport>>,
    gate: Option<Arc<StartGate>>,
}

/// What one rotation attempt achieved.
pub(crate) struct RotationReport {
    pub(crate) full_success: bool,
}

impl Job {
    fn cancel(&self) {
        match self {
            Self::Manual(job) => job.cancel.cancel(),
            Self::Rotation(job) => job.cancel.cancel(),
        }
    }

    const fn session_id(&self) -> Option<u64> {
        match self {
            Self::Manual(_) => None,
            Self::Rotation(job) => Some(job.session_id),
        }
    }
}

enum Commit {
    Cursor(Option<PoolIdentity>),
    External,
}

enum PlanMode {
    File(PathBuf),
    Collection(usize),
    Random,
    Sequential,
    RotationRandom,
}

pub(crate) enum PlanFailure {
    Source(SourceError),
    Invalid(String),
}

impl WallpapersService {
    /// Constructs an inert service. No thread, session, timer or native call is
    /// created here.
    ///
    /// # Errors
    /// Returns [`ServiceError`] when the configuration directory cannot be
    /// canonicalized.
    pub fn new(config: ServiceConfig) -> Result<Arc<Self>, ServiceError> {
        let host_directory =
            canonical_directory(config.source.config_directory()).map_err(ServiceError::new)?;
        let service = Arc::new(Self {
            paths: config.paths,
            source: config.source,
            host_directory,
            backend: config.backend,
            clock: config.clock,
            random: config.random,
            processor: config.processor,
            native: NativeOperationTracker::new(),
            accepted: Mutex::new(None),
            accepted_subscription: Mutex::new(None),
            coordinator: Mutex::new(CoordinatorState {
                closed: false,
                queued: None,
                executing: false,
                thread: None,
            }),
            coordinator_signal: Arc::new(RuntimeSignal::new()),
            session: Mutex::new(None),
            next_session: AtomicU64::new(0),
            mutation_epoch: AtomicU64::new(0),
            selection: Mutex::new(SelectionState::default()),
            shutdown: AtomicBool::new(false),
            host_cancel: CancellationToken::new(),
            self_ref: OnceLock::new(),
            #[cfg(test)]
            commit_hook: Mutex::new(None),
            #[cfg(test)]
            fail_timer_spawn: AtomicBool::new(false),
        });
        let _ = service.self_ref.set(Arc::downgrade(&service));
        Ok(service)
    }

    /// Production constructor with the system clock and random source.
    ///
    /// # Errors
    /// Returns [`ServiceError`] for an unusable configuration directory.
    pub fn production(
        paths: AppPaths,
        source: SourceContext,
        backend: Arc<dyn GuardedDesktopBackend>,
    ) -> Result<Arc<Self>, ServiceError> {
        Self::new(ServiceConfig::production(paths, source, backend))
    }

    /// The injected identity paths.
    #[must_use]
    pub const fn paths(&self) -> &AppPaths { &self.paths }

    /// The injected configured-source context.
    #[must_use]
    pub(crate) const fn source_context(&self) -> &SourceContext { &self.source }

    /// The service clock.
    #[must_use]
    pub(crate) fn clock_ref(&self) -> &dyn RuntimeClock { self.clock.as_ref() }

    /// The current instant on the service clock.
    #[must_use]
    pub(crate) fn now(&self) -> Instant { self.clock.now() }

    /// The canonical accepted configuration directory.
    #[must_use]
    pub fn host_directory(&self) -> &Path { &self.host_directory }

    fn arc(&self) -> Arc<Self> {
        self.self_ref
            .get()
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| panic!("the wallpaper service outlived its self reference"))
    }

    /// Installs the module's typed accepted watch and a wake-only listener.
    pub fn install_accepted_watch(&self, watch: AcceptedWatch<WallpapersConfig>) {
        let seen = watch.read().map_or(0, |pair| pair.revision);
        let weak = self.self_ref.get().cloned().unwrap_or_default();
        let subscription = watch.on_change(
            seen,
            Arc::new(move || {
                if let Some(service) = weak.upgrade() {
                    service.wake_runtime();
                }
            }),
        );
        *lock(&self.accepted) = Some(watch);
        *lock(&self.accepted_subscription) = Some(subscription);
        self.refresh_mutation_tag();
        self.coordinator_signal.notify();
    }

    /// The shared native-operation tracker used by explicit and rotation
    /// application.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn native_tracker(&self) -> &NativeOperationTracker { &self.native }

    /// Installs a one-shot hook between completion checking and the cursor
    /// commit. Test-only: lets a barrier publish or stop exactly at the
    /// check-to-write boundary.
    #[cfg(test)]
    pub(crate) fn set_commit_hook(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        *lock(&self.commit_hook) = hook;
    }

    /// Injects one timer-worker spawn failure for the startup rollback test.
    #[cfg(test)]
    pub(crate) fn fail_next_timer_spawn(&self) {
        self.fail_timer_spawn.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn take_timer_spawn_failure(&self) -> bool {
        self.fail_timer_spawn.swap(false, Ordering::SeqCst)
    }

    /// The session epoch a manual job freezes at admission.
    fn session_epoch(&self) -> u64 { self.mutation_epoch.load(Ordering::SeqCst) }

    /// A monotonic session transition, taken before the visible session change.
    fn begin_epoch_transition(&self) {
        self.mutation_epoch.fetch_add(1, Ordering::SeqCst);
        self.refresh_mutation_tag();
    }

    /// Re-tags the cursor state with the current accepted revision and session
    /// under the selection lock.
    ///
    /// Publication listeners and session transitions call this, so a commit
    /// that checks the tag while holding the same lock is linearized against
    /// them: an older completion can no longer write successor cursor state.
    fn refresh_mutation_tag(&self) {
        // Hold the selection lock while reading the values being tagged so this
        // write cannot interleave between a commit's check and its cursor
        // writes. Lock order is selection -> accepted/watch/session, matching
        // `commit_if_current`.
        let mut state = lock(&self.selection);
        let revision = self.accepted_pair().map_or(0, |pair| pair.revision);
        let session = self.session_handle().map(|session| session.id());
        state.set_tag(revision, self.session_epoch(), session);
        drop(state);
    }

    /// Constructs the rotation module bound to this service.
    #[must_use]
    pub fn module(&self) -> super::WallpapersModule { super::WallpapersModule::new(self.arc()) }

    /// Constructs the typed `wallpapers.set` handler bound to this service.
    #[must_use]
    pub fn handler(&self) -> super::WallpapersIpc { super::WallpapersIpc::new(self.arc()) }

    /// Runs one manual set with the production cold-request budget.
    #[must_use]
    pub fn set(&self, params: &WallpapersSetParams) -> WallpapersSetOutcome {
        self.set_with_budget(params, WALLPAPERS_SERVER_BUDGET)
    }

    /// Runs one manual set with an explicit budget, measured from this call.
    #[must_use]
    pub fn set_with_budget(
        &self,
        params: &WallpapersSetParams,
        budget: Duration,
    ) -> WallpapersSetOutcome {
        let now = self.clock.now();
        let deadline = now.checked_add(budget);
        self.set_with_deadline(params, deadline)
    }

    /// Runs one manual set with an absolute deadline on the service clock.
    #[must_use]
    pub fn set_with_deadline(
        &self,
        params: &WallpapersSetParams,
        deadline: Option<Instant>,
    ) -> WallpapersSetOutcome {
        if self.shutdown.load(Ordering::SeqCst) || self.host_cancel.is_cancelled() {
            return WallpapersSetOutcome::cancelled(WallpapersOutcomeCause::Cancelled);
        }
        let requested = match canonical_directory(Path::new(&params.config_directory)) {
            Ok(directory) => directory,
            Err(message) => {
                return WallpapersSetOutcome::refused(
                    WallpapersOutcomeStatus::Failure,
                    WallpapersOutcomeCause::InvalidSelection { message },
                );
            }
        };
        if requested != self.host_directory {
            return WallpapersSetOutcome::refused(
                WallpapersOutcomeStatus::Failure,
                WallpapersOutcomeCause::DifferentConfig {
                    host: self.host_directory.display().to_string(),
                    requested: requested.display().to_string(),
                },
            );
        }

        let cancel = CancellationToken::new();
        let wake = Arc::new(RuntimeSignal::new());
        let host_guard = {
            let token = cancel.clone();
            let wake = Arc::clone(&wake);
            Some(self.host_cancel.on_cancel(Arc::new(move || {
                token.cancel();
                wake.notify();
            })))
        };
        let (reply, receiver) = mpsc::channel();
        let issued = Arc::new(AtomicBool::new(false));
        let job = ManualJob {
            selection: params.selection.clone(),
            screen: params.screen.clone(),
            deadline,
            cancel: cancel.clone(),
            _host_guard: host_guard,
            issued: Arc::clone(&issued),
            reply,
            wake: Arc::clone(&wake),
        };
        if let Err(outcome) = self.admit_manual(job) {
            return outcome;
        }

        loop {
            // Snapshot the wake generation before checking the result, so a
            // completion or host cancellation between the check and the wait
            // still wakes this thread. The wait is lossless and bounded by the
            // request deadline on the injected clock.
            let seen = wake.generation();
            match receiver.try_recv() {
                // A completed job is authoritative: its own outcome knows
                // whether a native call was issued. The conservative caller
                // check below cannot distinguish a guarded refusal from an
                // issued call, so it must not override a delivered answer.
                Ok(outcome) => return outcome,
                Err(mpsc::TryRecvError::Disconnected) => {
                    cancel.cancel();
                    // A native call may already have been issued; then the
                    // result is unknown and no identity is invented.
                    let status = if issued.load(Ordering::SeqCst) {
                        WallpapersOutcomeStatus::Uncertain
                    } else {
                        WallpapersOutcomeStatus::Failure
                    };
                    return WallpapersSetOutcome::refused(
                        status,
                        WallpapersOutcomeCause::Unavailable {
                            message: "the wallpaper coordinator stopped before reporting"
                                .to_owned(),
                        },
                    );
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if let Some(outcome) = self.manual_wait_status(&cancel, &issued, deadline) {
                cancel.cancel();
                return outcome;
            }
            let _ = wake.wait_after(seen, deadline, self.clock.as_ref());
        }
    }

    fn manual_wait_status(
        &self,
        cancel: &CancellationToken,
        issued: &AtomicBool,
        deadline: Option<Instant>,
    ) -> Option<WallpapersSetOutcome> {
        let cancelled = self.shutdown.load(Ordering::SeqCst) || self.host_cancel.is_cancelled();
        let expired = deadline.is_some_and(|deadline| self.clock.now() >= deadline);
        if !cancelled && !expired && !cancel.is_cancelled() {
            return None;
        }
        let cause = if expired && !cancelled {
            WallpapersOutcomeCause::DeadlineExpired
        } else {
            WallpapersOutcomeCause::Cancelled
        };
        if issued.load(Ordering::SeqCst) {
            Some(WallpapersSetOutcome::refused(
                WallpapersOutcomeStatus::Uncertain,
                cause,
            ))
        } else {
            Some(WallpapersSetOutcome::cancelled(cause))
        }
    }

    #[allow(clippy::result_large_err)]
    fn admit_manual(&self, job: ManualJob) -> Result<(), WallpapersSetOutcome> {
        let deadline = job.deadline;
        let cancel = job.cancel.clone();
        let mut state = lock(&self.coordinator);
        loop {
            if state.closed
                || self.shutdown.load(Ordering::SeqCst)
                || self.host_cancel.is_cancelled()
                || cancel.is_cancelled()
            {
                return Err(WallpapersSetOutcome::cancelled(
                    WallpapersOutcomeCause::Cancelled,
                ));
            }
            if let Some(deadline) = deadline
                && self.clock.now() >= deadline
            {
                return Err(WallpapersSetOutcome::cancelled(
                    WallpapersOutcomeCause::DeadlineExpired,
                ));
            }
            if state.queued.is_none() {
                state.queued = Some(Job::Manual(job));
                if let Err(error) = self.ensure_thread(&mut state) {
                    state.queued = None;
                    return Err(WallpapersSetOutcome::refused(
                        WallpapersOutcomeStatus::Failure,
                        WallpapersOutcomeCause::Unavailable { message: error.to_string() },
                    ));
                }
                drop(state);
                self.coordinator_signal.notify();
                return Ok(());
            }

            if deadline.is_none() {
                return Err(WallpapersSetOutcome::refused(
                    WallpapersOutcomeStatus::Failure,
                    WallpapersOutcomeCause::QueueFull,
                ));
            }
            let seen = self.coordinator_signal.generation();
            drop(state);
            match self.coordinator_signal.wait_after(seen, deadline, self.clock.as_ref()) {
                WaitOutcome::Stopped => {
                    return Err(WallpapersSetOutcome::cancelled(
                        WallpapersOutcomeCause::Cancelled,
                    ));
                }
                WaitOutcome::Deadline => {
                    return Err(WallpapersSetOutcome::cancelled(
                        WallpapersOutcomeCause::DeadlineExpired,
                    ));
                }
                WaitOutcome::Wake => {}
            }
            state = lock(&self.coordinator);
        }
    }

    fn ensure_thread(&self, state: &mut CoordinatorState) -> std::io::Result<()> {
        if state.thread.is_some() {
            return Ok(());
        }
        let service = self.arc();
        let handle = thread::Builder::new()
            .name("wallpapers-coordinator".to_owned())
            .spawn(move || coordinator_loop(service))?;
        state.thread = Some(handle);
        Ok(())
    }

    fn try_admit(&self, job: Job) -> bool {
        if self.shutdown.load(Ordering::SeqCst) {
            return false;
        }
        let mut state = lock(&self.coordinator);
        if state.closed || state.queued.is_some() {
            return false;
        }
        state.queued = Some(job);
        if self.ensure_thread(&mut state).is_err() {
            state.queued = None;
            return false;
        }
        drop(state);
        self.coordinator_signal.notify();
        true
    }

    /// Submits one rotation attempt, never waiting for a queue slot.
    pub(crate) fn submit_rotation(
        &self,
        session: &Arc<SessionControl>,
        revision: u64,
        logger: ScopedLogger,
        reply: Option<mpsc::Sender<RotationReport>>,
    ) -> bool {
        if !session.is_live() {
            return false;
        }
        self.try_admit(Job::Rotation(RotationJob {
            session_id: session.id(),
            revision,
            epoch: self.session_epoch(),
            cancel: session.cancellation(),
            logger,
            reply,
            gate: None,
        }))
    }

    /// Submits the startup job, which is queued first but latched until every
    /// startup resource is established.
    pub(crate) fn submit_initial(
        &self,
        session: &Arc<SessionControl>,
        revision: u64,
        logger: ScopedLogger,
        gate: Arc<StartGate>,
    ) -> bool {
        if !session.is_live() {
            return false;
        }
        self.try_admit(Job::Rotation(RotationJob {
            session_id: session.id(),
            revision,
            epoch: self.session_epoch(),
            cancel: session.cancellation(),
            logger,
            reply: None,
            gate: Some(gate),
        }))
    }

    pub(crate) fn begin_session(
        &self,
        pair: &AcceptedConfig<WallpapersConfig>,
        pool: SourcePool,
        now: Instant,
    ) -> Arc<SessionControl> {
        let id = self.next_session.fetch_add(1, Ordering::SeqCst) + 1;
        let session = Arc::new(SessionControl::new(
            id,
            pair,
            pool,
            Arc::new(RuntimeSignal::new()),
            now,
        ));
        self.begin_epoch_transition();
        let previous = lock(&self.session).replace(Arc::clone(&session));
        if let Some(previous) = previous {
            previous.invalidate();
        }
        self.refresh_mutation_tag();
        session
    }

    /// Invalidates one module session, cancelling its timer and queued
    /// rotation work, without touching manual admission.
    pub(crate) fn end_session(&self, id: u64) {
        self.begin_epoch_transition();
        let session = {
            let mut guard = lock(&self.session);
            if guard.as_ref().is_some_and(|session| session.id() == id) {
                guard.take()
            } else {
                None
            }
        };
        if let Some(session) = session {
            session.invalidate();
        }
        let queued = {
            let mut state = lock(&self.coordinator);
            if state.queued.as_ref().is_some_and(|job| job.session_id() == Some(id)) {
                state.queued.take()
            } else {
                None
            }
        };
        if let Some(job) = queued {
            job.cancel();
            reply_cancelled(&job);
        }
        self.refresh_mutation_tag();
        self.coordinator_signal.notify();
    }

    pub(crate) fn session_handle(&self) -> Option<Arc<SessionControl>> {
        lock(&self.session).clone()
    }

    pub(crate) fn accepted_pair(&self) -> Option<AcceptedConfig<WallpapersConfig>> {
        let watch = lock(&self.accepted).clone();
        watch.as_ref().and_then(AcceptedWatch::read)
    }

    /// Closes admission, cancels every host-owned wait and invalidates the
    /// current rotation session. Retains the coordinator thread and service
    /// ownership until [`WallpapersService::finish_shutdown`] observes it
    /// finished.
    pub fn begin_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.host_cancel.cancel();
        let queued = {
            let mut state = lock(&self.coordinator);
            state.closed = true;
            state.queued.take()
        };
        if let Some(job) = queued {
            job.cancel();
            reply_cancelled(&job);
        }
        self.coordinator_signal.notify();
        self.begin_epoch_transition();
        let session = lock(&self.session).take();
        if let Some(session) = session {
            session.invalidate();
        }
        self.refresh_mutation_tag();
    }

    /// Waits boundedly for the coordinator to return. `false` means the service
    /// still owns the thread and its payload; the caller must report the gap
    /// rather than pretend completion.
    #[must_use]
    pub fn finish_shutdown(&self, wait: &dyn ShutdownWait) -> bool {
        let finished = wait_for_finish(wait, &|| {
            let coordinator_finished = lock(&self.coordinator)
                .thread
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished);
            // A late issued native closure still owns its keepalive and may
            // still be changing the desktop. Reporting completion before it
            // returns would overstate teardown.
            coordinator_finished && self.native.is_drained()
        });
        if !finished {
            return false;
        }
        let handle = lock(&self.coordinator).thread.take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        true
    }

    /// Snapshot of coordinator and session state.
    #[must_use]
    pub fn status(&self) -> ServiceStatus {
        let state = lock(&self.coordinator);
        let session = lock(&self.session).clone();
        ServiceStatus {
            closed: state.closed || self.shutdown.load(Ordering::SeqCst),
            coordinator_running: state.thread.is_some(),
            executing: state.executing,
            queued: state.queued.is_some(),
            session: session.map(|session| {
                let snapshot = session.snapshot();
                SessionStatus {
                    id: session.id(),
                    interval: snapshot.config.interval,
                    revision: snapshot.revision,
                    timer_spawned: session.timer_spawned(),
                    deadline_present: session.deadline().is_some(),
                    pool_len: snapshot.pool.len(),
                    full_success_resets: session.full_success_resets(),
                }
            }),
        }
    }

    /// Whether admission is closed.
    #[must_use]
    pub fn is_shutdown(&self) -> bool { self.shutdown.load(Ordering::SeqCst) }

    fn wake_runtime(&self) {
        self.refresh_mutation_tag();
        self.coordinator_signal.notify();
        let session = lock(&self.session).clone();
        if let Some(session) = session {
            session.signal().notify();
        }
    }

    /// Wakes one rotation session after its job finished, including when the
    /// job was obsolete and updated no state of its own.
    fn notify_session(&self, id: Option<u64>) {
        if let Some(id) = id
            && let Some(session) = self.session_handle()
            && session.id() == id
        {
            session.signal().notify();
        }
    }

    #[allow(clippy::too_many_lines)]
    fn execute_manual(&self, job: &ManualJob) -> WallpapersSetOutcome {
        if job.cancel.is_cancelled() {
            return WallpapersSetOutcome::cancelled(WallpapersOutcomeCause::Cancelled);
        }
        if job.deadline.is_some_and(|deadline| self.clock.now() >= deadline) {
            return WallpapersSetOutcome::cancelled(WallpapersOutcomeCause::DeadlineExpired);
        }
        let Some(pair) = self.accepted_pair() else {
            return WallpapersSetOutcome::refused(
                WallpapersOutcomeStatus::Failure,
                WallpapersOutcomeCause::NoAcceptedConfig,
            );
        };
        let effects = Effects {
            radius: pair.config.radius,
            blur: pair.config.blur,
        };
        let (mode, pool) = match self.resolve_manual_source(&job.selection, &pair.config) {
            Ok(value) => value,
            Err((status, cause)) => {
                return WallpapersSetOutcome::refused(status, cause);
            }
        };
        let revision = pair.revision;
        let epoch = self.session_epoch();
        let session_id = self.session_handle().map(|session| session.id());
        let current = self.current_closure(epoch, session_id, revision, job.cancel.clone());
        let snapshot = match self.snapshot(&job.cancel, current.clone(), job.deadline) {
            Ok(snapshot) => snapshot,
            Err(error) => return WallpapersSetOutcome::native_failure(&error),
        };
        let selected = match resolve_targets(&snapshot, &job.screen.as_target()) {
            Ok(selected) => selected,
            Err(error) => {
                return WallpapersSetOutcome::refused(
                    WallpapersOutcomeStatus::Failure,
                    WallpapersOutcomeCause::InvalidSelection { message: error.to_string() },
                );
            }
        };
        self.prune_selection(&snapshot);
        let state = lock(&self.selection);
        let pool_snapshot = pool.as_ref().map(PoolSnapshot::from_pool);
        let built = match self.build_plan(
            &mode,
            &selected,
            pool_snapshot.as_ref(),
            effects,
            Some(&state),
        ) {
            Ok(built) => built,
            Err(failure) => {
                return plan_failure(failure);
            }
        };
        drop(state);
        let order: Vec<DisplayId> = selected.iter().map(|target| target.entry.id).collect();

        let prepared = match self.processor.prepare(&built.plan, &self.paths, &job.cancel) {
            Ok(prepared) => prepared,
            Err(error) => return prepare_failure(&error),
        };
        job.issued.store(true, Ordering::SeqCst);
        let current = self.current_closure(epoch, session_id, revision, job.cancel.clone());
        let applied = prepared.apply_guarded_with_tracker(
            self.backend.as_ref(),
            &job.cancel,
            Arc::clone(&current),
            job.deadline,
            self.desktop_clock(),
            Some(self.native.clone()),
        );
        let applied = match applied {
            Ok(applied) => applied,
            Err(error) => {
                return WallpapersSetOutcome::refused(
                    WallpapersOutcomeStatus::Failure,
                    WallpapersOutcomeCause::Processing { message: error.to_string() },
                );
            }
        };
        let committed =
            self.commit_if_current(&applied, &built.commits, revision, epoch, session_id);
        if applied.is_complete()
            && committed
            && let Some(id) = session_id
            && let Some(session) = self.session_handle()
            && session.id() == id
        {
            session.note_full_success_if(self.clock.now(), revision, current.as_ref());
        }
        let mut outcome = WallpapersSetOutcome::from_apply_ordered(&applied, &order, None);
        if applied.applied.is_empty()
            && applied.failed.is_empty()
            && applied.unknown.is_empty()
            && !applied.not_attempted.is_empty()
        {
            let cause = if job.cancel.is_cancelled() {
                WallpapersOutcomeCause::Cancelled
            } else if job.deadline.is_some_and(|deadline| self.clock.now() >= deadline) {
                WallpapersOutcomeCause::DeadlineExpired
            } else {
                WallpapersOutcomeCause::Cancelled
            };
            outcome.status = WallpapersOutcomeStatus::Cancelled;
            outcome.cause = Some(cause);
        }
        outcome
    }

    #[allow(clippy::too_many_lines)]
    fn execute_rotation(&self, job: &RotationJob) -> RotationReport {
        let report = RotationReport { full_success: false };
        if job.cancel.is_cancelled() {
            return report;
        }
        if let Some(gate) = &job.gate
            && !gate.wait(&job.cancel)
        {
            return report;
        }
        let Some(session) = self.session_handle() else {
            return report;
        };
        if session.id() != job.session_id || !session.is_live() {
            return report;
        }
        let snapshot = session.snapshot();
        let Some(pair) = self.accepted_pair() else {
            job.logger.warn("wallpaper rotation skipped: no accepted configuration");
            return report;
        };
        if pair.revision != job.revision || snapshot.revision != job.revision {
            return report;
        }
        let mode = match snapshot.config.mode {
            WallpaperMode::Sequential => PlanMode::Sequential,
            WallpaperMode::Random => PlanMode::RotationRandom,
        };
        let pool = snapshot.pool;
        if pool.is_empty() {
            job.logger
                .warn("wallpaper rotation skipped (source): the wallpaper pool is empty");
            return report;
        }
        let effects = Effects {
            radius: snapshot.config.radius,
            blur: snapshot.config.blur,
        };
        let current =
            self.current_closure(job.epoch, Some(session.id()), job.revision, job.cancel.clone());
        let snapshot = match self.snapshot(&job.cancel, current.clone(), None) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                job.logger.warn(format!("wallpaper rotation skipped (native): {error}"));
                return report;
            }
        };
        let selected = match resolve_targets(&snapshot, &crate::selection::ScreenTarget::All) {
            Ok(selected) => selected,
            Err(error) => {
                job.logger.warn(format!("wallpaper rotation skipped (selection): {error}"));
                return report;
            }
        };
        self.prune_selection(&snapshot);
        let state = lock(&self.selection);
        let pool_snapshot = PoolSnapshot::from_pool(&pool);
        let built =
            match self.build_plan(&mode, &selected, Some(&pool_snapshot), effects, Some(&state)) {
                Ok(built) => built,
                Err(failure) => {
                    let outcome = plan_failure(failure);
                    job.logger.warn(format!(
                        "wallpaper rotation failed ({}): {}",
                        outcome.cause.as_ref().map_or("unknown", WallpapersOutcomeCause::tag),
                        outcome.cause.as_ref().map_or_else(String::new, cause_detail)
                    ));
                    return report;
                }
            };
        drop(state);
        let order: Vec<DisplayId> = selected.iter().map(|target| target.entry.id).collect();
        let prepared = match self.processor.prepare(&built.plan, &self.paths, &job.cancel) {
            Ok(prepared) => prepared,
            Err(error) => {
                let failure = prepare_failure(&error);
                job.logger.warn(format!(
                    "wallpaper rotation failed ({}): {}",
                    failure.cause.as_ref().map_or("unknown", WallpapersOutcomeCause::tag),
                    failure.cause.as_ref().map_or_else(String::new, cause_detail)
                ));
                return report;
            }
        };
        let current =
            self.current_closure(job.epoch, Some(session.id()), job.revision, job.cancel.clone());
        let applied = match prepared.apply_guarded_with_tracker(
            self.backend.as_ref(),
            &job.cancel,
            Arc::clone(&current),
            None,
            self.desktop_clock(),
            Some(self.native.clone()),
        ) {
            Ok(applied) => applied,
            Err(error) => {
                job.logger.warn(format!("wallpaper rotation failed (processing): {error}"));
                return report;
            }
        };
        let committed = self.commit_if_current(
            &applied,
            &built.commits,
            job.revision,
            job.epoch,
            Some(session.id()),
        );
        if committed {
            if applied.is_complete() {
                session.note_full_success_if(self.clock.now(), job.revision, current.as_ref());
            } else {
                session.note_attempt_finished_if(self.clock.now(), job.revision, current.as_ref());
            }
        }
        if !applied.is_complete() {
            let outcome = WallpapersSetOutcome::from_apply_ordered(&applied, &order, None);
            job.logger.warn(format!(
                "wallpaper rotation incomplete ({}): applied={}, failed={}, not-attempted={}, unknown={}",
                outcome.status_tag(),
                applied.applied.len(),
                applied.failed.len(),
                applied.not_attempted.len(),
                applied.unknown.len()
            ));
        }
        RotationReport {
            full_success: applied.is_complete(),
        }
    }

    fn resolve_manual_source(
        &self,
        selection: &WallpaperSelection,
        config: &WallpapersConfig,
    ) -> Result<(PlanMode, Option<SourcePool>), (WallpapersOutcomeStatus, WallpapersOutcomeCause)>
    {
        match selection {
            WallpaperSelection::File { path } => {
                let path = Path::new(path);
                if !path.is_absolute() {
                    return Err((
                        WallpapersOutcomeStatus::Failure,
                        WallpapersOutcomeCause::InvalidSelection {
                            message: "the explicit wallpaper path must be absolute".to_owned(),
                        },
                    ));
                }
                match resolve_explicit_file(path, Path::new("/")) {
                    Ok(path) => Ok((PlanMode::File(path), None)),
                    Err(error) => Err((
                        WallpapersOutcomeStatus::Failure,
                        WallpapersOutcomeCause::Source { message: error.to_string() },
                    )),
                }
            }
            WallpaperSelection::Collection { index } => {
                let pool = self.pool(config)?;
                let index = usize::try_from(*index).map_err(|_| {
                    (
                        WallpapersOutcomeStatus::Failure,
                        WallpapersOutcomeCause::InvalidSelection {
                            message: format!("collection index {index} does not fit this host"),
                        },
                    )
                })?;
                if index >= pool.len() {
                    return Err((
                        WallpapersOutcomeStatus::Failure,
                        WallpapersOutcomeCause::InvalidSelection {
                            message: format!(
                                "collection index {index} is out of range for a pool of {} entries",
                                pool.len()
                            ),
                        },
                    ));
                }
                Ok((PlanMode::Collection(index), Some(pool)))
            }
            WallpaperSelection::Name { name } => {
                let pool = self.pool(config)?;
                let Some(index) = find_pool_entry(&pool, name) else {
                    return Err((
                        WallpapersOutcomeStatus::Failure,
                        WallpapersOutcomeCause::InvalidSelection {
                            message: format!(
                                "no wallpaper named `{name}` is in the accepted pool of {} entries",
                                pool.len()
                            ),
                        },
                    ));
                };
                Ok((PlanMode::Collection(index), Some(pool)))
            }
            WallpaperSelection::Random => {
                let pool = self.pool(config)?;
                if pool.is_empty() {
                    return Err((
                        WallpapersOutcomeStatus::Failure,
                        WallpapersOutcomeCause::Source {
                            message: SourceError::EmptyPool.to_string(),
                        },
                    ));
                }
                Ok((PlanMode::Random, Some(pool)))
            }
        }
    }

    fn pool(
        &self,
        config: &WallpapersConfig,
    ) -> Result<SourcePool, (WallpapersOutcomeStatus, WallpapersOutcomeCause)> {
        resolve_pool(config, &self.source).map_err(|error| {
            (
                WallpapersOutcomeStatus::Failure,
                WallpapersOutcomeCause::Source { message: error.to_string() },
            )
        })
    }

    fn snapshot(
        &self,
        cancel: &CancellationToken,
        current: Arc<dyn Fn() -> bool + Send + Sync>,
        deadline: Option<Instant>,
    ) -> Result<stache_macos::DisplaySnapshot, DesktopError> {
        let mut operation =
            DesktopOperation::new(cancel.clone(), current).with_clock(self.desktop_clock());
        if let Some(deadline) = deadline {
            operation = operation.with_deadline(deadline);
        }
        self.backend.snapshot_guarded(&operation)
    }

    fn build_plan(
        &self,
        mode: &PlanMode,
        selected: &[SelectedTarget],
        pool: Option<&PoolSnapshot>,
        effects: Effects,
        state: Option<&SelectionState>,
    ) -> Result<BuiltPlan, PlanFailure> {
        let mut plan = Vec::with_capacity(selected.len());
        let mut commits = Vec::with_capacity(selected.len());
        for target in selected {
            let display = target.entry.id;
            match mode {
                PlanMode::File(path) => {
                    plan.push(PlannedTarget {
                        target: *target,
                        source: path.clone(),
                        effects,
                    });
                    commits.push((display, Commit::External));
                }
                PlanMode::Collection(index) => {
                    let snapshot = pool.ok_or_else(|| {
                        PlanFailure::Invalid("missing accepted source pool".to_owned())
                    })?;
                    let planned =
                        SelectionState::plan_collection(*index, snapshot).ok_or_else(|| {
                            PlanFailure::Invalid(format!(
                                "collection index {index} is out of range"
                            ))
                        })?;
                    plan.push(PlannedTarget {
                        target: *target,
                        source: planned.path,
                        effects,
                    });
                    commits.push((display, Commit::Cursor(planned.successor)));
                }
                PlanMode::Random | PlanMode::RotationRandom => {
                    let snapshot = pool.ok_or_else(|| {
                        PlanFailure::Invalid("missing accepted source pool".to_owned())
                    })?;
                    if snapshot.is_empty() {
                        return Err(PlanFailure::Source(SourceError::EmptyPool));
                    }
                    let index = self.random.index(snapshot.len());
                    let entry = snapshot
                        .entry(index)
                        .ok_or_else(|| PlanFailure::Invalid("random pool index".to_owned()))?;
                    plan.push(PlannedTarget {
                        target: *target,
                        source: entry.path.clone(),
                        effects,
                    });
                    commits.push((display, Commit::Cursor(None)));
                }
                PlanMode::Sequential => {
                    let state = state.ok_or_else(|| {
                        PlanFailure::Invalid("missing selection state".to_owned())
                    })?;
                    let snapshot = pool.ok_or_else(|| {
                        PlanFailure::Invalid("missing accepted source pool".to_owned())
                    })?;
                    if snapshot.is_empty() {
                        return Err(PlanFailure::Source(SourceError::EmptyPool));
                    }
                    let planned = state.plan_sequential(display, snapshot).ok_or_else(|| {
                        PlanFailure::Invalid("empty accepted source pool".to_owned())
                    })?;
                    plan.push(PlannedTarget {
                        target: *target,
                        source: planned.path,
                        effects,
                    });
                    commits.push((display, Commit::Cursor(planned.successor)));
                }
            }
        }
        Ok(BuiltPlan { plan, commits })
    }

    fn prune_selection(&self, snapshot: &stache_macos::DisplaySnapshot) {
        let ids: Vec<DisplayId> = snapshot.iter().map(|entry| entry.id).collect();
        lock(&self.selection).prune(&ids);
    }

    /// Commits cursor changes only while the frozen accepted revision and
    /// session epoch are still current.
    ///
    /// The tag check and the writes happen under the selection lock; the
    /// publication listener and session transitions update that same tag while
    /// holding it. A reload, pause, stop or new session therefore either
    /// linearizes before this commit or makes it inert, so an old completion can
    /// never overwrite successor cursor state.
    fn commit_if_current(
        &self,
        applied: &crate::apply::ApplyOutcome,
        commits: &[(DisplayId, Commit)],
        revision: u64,
        epoch: u64,
        session_id: Option<u64>,
    ) -> bool {
        #[cfg(test)]
        {
            let hook = lock(&self.commit_hook).take();
            if let Some(hook) = hook {
                hook();
            }
        }
        let mut state = lock(&self.selection);
        if !state.tagged(revision, epoch, session_id)
            || self.session_epoch() != epoch
            || self.accepted_pair().is_none_or(|pair| pair.revision != revision)
        {
            return false;
        }
        match session_id {
            Some(id) => {
                if !self
                    .session_handle()
                    .is_some_and(|session| session.id() == id && session.is_live())
                {
                    return false;
                }
            }
            None => {
                if self.session_handle().is_some() {
                    return false;
                }
            }
        }
        Self::commit_applied(&mut state, applied, commits);
        drop(state);
        true
    }

    fn commit_applied(
        state: &mut SelectionState,
        applied: &crate::apply::ApplyOutcome,
        commits: &[(DisplayId, Commit)],
    ) {
        for (display, commit) in commits {
            if !applied.applied.contains(display) {
                continue;
            }
            match commit {
                Commit::Cursor(successor) => state.commit(*display, successor.clone()),
                Commit::External => state.note_external(*display),
            }
        }
    }

    fn desktop_clock(&self) -> Arc<dyn DesktopClock> {
        Arc::new(RuntimeDesktopClock(Arc::clone(&self.clock)))
    }

    fn current_closure(
        &self,
        epoch: u64,
        session_id: Option<u64>,
        revision: u64,
        cancel: CancellationToken,
    ) -> Arc<dyn Fn() -> bool + Send + Sync> {
        let weak = self.self_ref.get().cloned().unwrap_or_default();
        let watch = lock(&self.accepted).clone();
        let host = self.host_cancel.clone();
        Arc::new(move || {
            if cancel.is_cancelled() || host.is_cancelled() {
                return false;
            }
            let Some(service) = weak.upgrade() else {
                return false;
            };
            if service.shutdown.load(Ordering::SeqCst) {
                return false;
            }
            if service.session_epoch() != epoch {
                return false;
            }
            if let Some(id) = session_id
                && !service
                    .session_handle()
                    .is_some_and(|session| session.id() == id && session.is_live())
            {
                return false;
            }
            watch
                .as_ref()
                .is_some_and(|watch| watch.read().is_some_and(|pair| pair.revision == revision))
        })
    }
}

struct BuiltPlan {
    plan: Vec<PlannedTarget>,
    commits: Vec<(DisplayId, Commit)>,
}

struct RuntimeDesktopClock(Arc<dyn RuntimeClock>);

impl DesktopClock for RuntimeDesktopClock {
    fn now(&self) -> Instant { self.0.now() }
}

#[allow(clippy::needless_pass_by_value)]
fn coordinator_loop(service: Arc<WallpapersService>) {
    loop {
        let job = {
            let mut state = lock(&service.coordinator);
            loop {
                if state.closed {
                    return;
                }
                if let Some(job) = state.queued.take() {
                    state.executing = true;
                    service.coordinator_signal.notify();
                    break job;
                }
                let seen = service.coordinator_signal.generation();
                drop(state);
                let _ = service.coordinator_signal.wait_after(seen, None, service.clock.as_ref());
                state = lock(&service.coordinator);
            }
        };
        let session_id = job.session_id();
        if job_is_cancelled(&service, &job) {
            reply_cancelled(&job);
        } else {
            match job {
                Job::Manual(job) => {
                    let outcome = service.execute_manual(&job);
                    let _ = job.reply.send(outcome);
                    job.wake.notify();
                }
                Job::Rotation(job) => {
                    let report = service.execute_rotation(&job);
                    if let Some(reply) = &job.reply {
                        let _ = reply.send(report);
                    }
                }
            }
        }
        // The timer waits on the session signal for either an accepted
        // publication or this completion. Waking even an obsolete job keeps
        // that wait lossless without polling the reply channel.
        service.notify_session(session_id);
        let mut state = lock(&service.coordinator);
        state.executing = false;
        drop(state);
        service.coordinator_signal.notify();
    }
}

fn job_is_cancelled(service: &WallpapersService, job: &Job) -> bool {
    if service.shutdown.load(Ordering::SeqCst) || service.host_cancel.is_cancelled() {
        return true;
    }
    match job {
        Job::Manual(job) => job.cancel.is_cancelled(),
        Job::Rotation(job) => job.cancel.is_cancelled(),
    }
}

fn reply_cancelled(job: &Job) {
    match job {
        Job::Manual(job) => {
            let _ = job.reply.send(WallpapersSetOutcome::cancelled(
                WallpapersOutcomeCause::Cancelled,
            ));
            job.wake.notify();
        }
        Job::Rotation(job) => {
            if let Some(reply) = &job.reply {
                let _ = reply.send(RotationReport { full_success: false });
            }
        }
    }
}

pub(crate) fn plan_failure(failure: PlanFailure) -> WallpapersSetOutcome {
    let cause = match failure {
        PlanFailure::Source(error) => WallpapersOutcomeCause::Source { message: error.to_string() },
        PlanFailure::Invalid(message) => WallpapersOutcomeCause::InvalidSelection { message },
    };
    WallpapersSetOutcome::refused(WallpapersOutcomeStatus::Failure, cause)
}

pub(crate) fn prepare_failure(error: &PrepareError) -> WallpapersSetOutcome {
    match error {
        PrepareError::Cancelled => {
            WallpapersSetOutcome::cancelled(WallpapersOutcomeCause::Cancelled)
        }
        PrepareError::Selection(error) => WallpapersSetOutcome::refused(
            WallpapersOutcomeStatus::Failure,
            WallpapersOutcomeCause::InvalidSelection {
                message: selection_message(error),
            },
        ),
        PrepareError::Snapshot(error) => WallpapersSetOutcome::native_failure(error),
        PrepareError::Lease(error) => WallpapersSetOutcome::refused(
            WallpapersOutcomeStatus::Failure,
            WallpapersOutcomeCause::Unavailable { message: error.to_string() },
        ),
        PrepareError::DuplicateDisplay(display) => WallpapersSetOutcome::refused(
            WallpapersOutcomeStatus::Failure,
            WallpapersOutcomeCause::InvalidSelection {
                message: format!("display {} appears more than once", display.raw()),
            },
        ),
        PrepareError::Resource(_) | PrepareError::Process(_) => WallpapersSetOutcome::refused(
            WallpapersOutcomeStatus::Failure,
            WallpapersOutcomeCause::Processing { message: error.to_string() },
        ),
    }
}

fn selection_message(error: &SelectionError) -> String { error.to_string() }

fn cause_detail(cause: &WallpapersOutcomeCause) -> String {
    match cause {
        WallpapersOutcomeCause::DifferentConfig { host, requested } => {
            format!("host {host}, requested {requested}")
        }
        WallpapersOutcomeCause::NoAcceptedConfig => "no accepted configuration".to_owned(),
        WallpapersOutcomeCause::QueueFull => "queue full".to_owned(),
        WallpapersOutcomeCause::DeadlineExpired => "deadline expired".to_owned(),
        WallpapersOutcomeCause::Cancelled => "cancelled".to_owned(),
        WallpapersOutcomeCause::InvalidSelection { message }
        | WallpapersOutcomeCause::Source { message }
        | WallpapersOutcomeCause::Processing { message }
        | WallpapersOutcomeCause::Native { message }
        | WallpapersOutcomeCause::Unavailable { message } => message.clone(),
    }
}

impl WallpapersSetOutcome {
    /// A compact status tag for logs.
    #[must_use]
    pub const fn status_tag(&self) -> &'static str {
        match self.status {
            WallpapersOutcomeStatus::FullSuccess => "full-success",
            WallpapersOutcomeStatus::Partial => "partial",
            WallpapersOutcomeStatus::Failure => "failure",
            WallpapersOutcomeStatus::Cancelled => "cancelled",
            WallpapersOutcomeStatus::Uncertain => "uncertain",
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
