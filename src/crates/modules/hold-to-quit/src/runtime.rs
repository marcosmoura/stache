//! The Hold-to-Quit runtime: lifecycle, worker and teardown.
//!
//! [`HoldToQuitRuntime`] is the host-lifetime service the composition root builds
//! once over one [`ApplicationControl`], the [`HudHost`], the host's
//! frontmost-context lease and a clock. Building it is inert. An enabled
//! [`HoldToQuitModule`] then subscribes one suppressing handler to the shared
//! input service (see [`crate::policy`] for the callback decision) and runs one
//! worker.
//!
//! # State machine
//!
//! The worker is the only thread that shows, hides and issues. Its phases are:
//!
//! ```text
//! Idle ──claimed down──▶ AwaitingShow ──matching Shown──▶ Tracking ──deadline──▶ Idle
//!   ▲                        │                               │
//!   └──── cancel / Stale / Unavailable / Failed / epoch / release ◀─────────────┘
//! ```
//!
//! * **Idle** parks with no timeout: an idle runtime has no polling or tick. It
//!   is woken by the callback (a claimed down), the accepted-configuration
//!   change listener, a lifecycle cancel, or the run's cancellation.
//! * **`AwaitingShow`** follows the claimed down's checks (armed revision, epoch,
//!   `preflight_frontmost`, optional `display_for`) and the non-waiting
//!   `HudControl::show`. A show's acknowledgement is only expired by polling
//!   [`HudEvents`], so the worker parks in `CANCELLATION_QUANTUM` slices and
//!   drains events after each wake, which is bounded by the HUD's own
//!   acknowledgement budget. Only a `Shown` event for the live press creates the
//!   one-shot [`PressAuthorization`] and moves to Tracking. `Stale` cancels the
//!   press; `Unavailable` and `Failed` cancel it and enter HUD-degraded mode.
//! * **Tracking** parks until the checked deadline (`down + holdDuration`; an
//!   unrepresentable deadline is a cancellable indefinite wait). At the deadline
//!   the worker drains the inbox and HUD events once, so a release or cancel
//!   delivered at or before the deadline wins, then rechecks the live press,
//!   armed revision and matching `Shown` and calls `hide` followed by
//!   `terminate_gracefully` exactly once. It never forces, retries or retargets.
//!
//! * **Linger.** When the user ends a press early (a release or Cmd/Q cancel seen
//!   by `step`, the frontmost activation listener's cancel, or a cancel found at
//!   the deadline) the worker cancels and clears the authorization at once, then
//!   calls `HudControl::freeze` and keeps one `Linger { press, until }` beside
//!   `Idle` for `dismissDelay` (read from the armed configuration) before the
//!   one `hide`. `dismissDelay` 0 hides at once with no freeze. While idle with a
//!   linger the worker parks for the remaining linger only; with none it parks
//!   with no timeout. A new claimed down takes the linger and, right after
//!   enqueuing its show, hides the old press (FIFO main queue: a no-op when the
//!   new press is displayed, an order-out when its show was refused); stop, an armed-revision change and degrade hide
//!   it at once. A `Shown` for the lingering press is frozen again, never
//!   tracked; `Stale`/`Unavailable`/`Failed` for it clear it without a second
//!   hide. Every other ending (a fire, stop, revision change, `accepting`
//!   cleared, epoch mismatch, HUD failure, degrade, supersession) hides at once.
//!   A tap-disable cancel and the older press cancelled by a failed push of a newer
//!   claim leave `accepting` and the revision unchanged, so they linger too.
//!   A lingering press holds no authorization and can never issue.
//!
//! Who wakes whom: the tap callback unparks the worker after a claimed down or a
//! cancel; the activation listener cancels the claimed press; the accepted
//! change listener unparks the worker; the run's cancellation (registry stop,
//! invalidation) unparks it. Cancellation is a monotone atomic mark plus the
//! one-shot authorization, so no cancel is ever lost to queue capacity.
//!
//! # Reconfigure
//!
//! `reconfigure` ends with the only fallible step, `ApplicationObservation::
//! accept`; a `TimedOut` rejects the candidate and leaves the old token,
//! configuration, readiness and press untouched. On success it clears
//! `accepting`, cancels the press and stages the candidate. The input
//! subscription and ledger tombstones stay. The worker re-arms only after
//! [`AcceptedWatch::read`] shows the staged value at a newer revision, and the
//! callback compares the armed revision with the revision the change listener
//! observed, never the watch.
//!
//! # Start and stop
//!
//! Start runs off-main in this order: generation, inbox and cancel state, the
//! host frontmost lease, `ApplicationControl::install` and the observed frontmost
//! watch, `HudControl::install` (hidden panel acknowledged), `accept()`, the
//! worker, and the input subscription last. Any failure rolls back in reverse,
//! keeps unacknowledged owners for retry and returns a typed [`ModuleError`], so
//! the registry reports `Failed(Start, ...)`. Stop clears `accepting`, cancels
//! every press and the authorization, drops the subscription (the shared service
//! keeps a suppressing tap while a tombstone lives), releases the HUD, stops the
//! application observation, releases the lease and returns for the registry's
//! worker join. Nothing waits on main for a worker that waits on main.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::hold_hud::{
    HudAnchor, HudAuthorization, HudBackend, HudControl, HudEvent, HudEvents, HudHandle, HudMotion,
    HudOutcome, HudReleaseOutcome, HudRequest, HudRetained, SystemHudBackend,
};
use stache_macos::{
    ACKNOWLEDGEMENT_BUDGET, AcceptedActionToken, ApplicationControl, ApplicationId,
    ApplicationObservation, CANCELLATION_QUANTUM, FrontmostPreflight, FrontmostWatch,
    GracefulOutcome, PressAuthorization,
};
use stache_sdk::config::AcceptedSubscription;
use stache_sdk::input::{
    HandlerSpec, InputError, InputPriority, NativeCleanup, Subscription, WorkRequirement,
};
use stache_sdk::threading::CancelListener;
use stache_sdk::{
    AcceptedWatch, CancellationToken, ErrorCategory, MainThreadExecutor, Module, ModuleContext,
    ModuleDescriptor, ModuleError, ModuleId, ModuleResult, Operation, Permission,
    PermissionRequirement, ScopedLogger,
};

use crate::config::{CommandQuitConfig, HudAnimation, HudPosition};
use crate::policy::{
    ActivationCancel, Down, HoldClock, HoldHandler, Shared, Staged, deadline, filter,
    hud_predicate, hud_press,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How many issue reports the runtime keeps.
const REPORT_CAPACITY: usize = 16;

fn error(operation: Operation, category: ErrorCategory, message: impl Into<String>) -> ModuleError {
    ModuleError::new(CommandQuitConfig::ID, operation, category, message)
}

/// Maps the configured position onto the HUD adapter's anchor.
const fn hud_anchor(position: HudPosition) -> HudAnchor {
    match position {
        HudPosition::Bottom => HudAnchor::Bottom,
        HudPosition::Top => HudAnchor::Top,
        HudPosition::Left => HudAnchor::Left,
        HudPosition::Right => HudAnchor::Right,
        HudPosition::Center => HudAnchor::Center,
    }
}

/// Maps the configured animation onto the HUD adapter's motion.
const fn hud_motion(animation: HudAnimation) -> HudMotion {
    match animation {
        HudAnimation::Fade => HudMotion::Fade,
        HudAnimation::Slide => HudMotion::Slide,
    }
}

// ---------------------------------------------------------------------------
// Host-supplied pieces
// ---------------------------------------------------------------------------

/// One held lease on the host's frontmost-context publisher.
///
/// The module never sees the host publisher itself. Releasing reports whether
/// the host acknowledged; a failed acknowledgement stays owned and inert in the
/// host, which retries it before a successor lease and at coordinated teardown.
pub trait LeaseHandle: Send {
    /// Releases the lease.
    ///
    /// # Errors
    /// Returns the host's text diagnostic when the last release could not be
    /// acknowledged.
    fn release(self: Box<Self>) -> Result<(), String>;
}

/// The host's frontmost-context lease, injected so this crate stays free of the
/// app crate.
pub trait FrontmostLeaser: Send + Sync {
    /// Takes one lease. The first lease installs the host observation. Called
    /// off-main.
    ///
    /// # Errors
    /// Returns the host's text diagnostic; no lease is held after an error.
    fn lease(&self, cancel: &CancellationToken) -> Result<Box<dyn LeaseHandle>, String>;
}

/// Why a HUD could not be created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HudHostError {
    /// A predecessor's HUD cleanup is still retained, so a second panel is not
    /// created.
    Retained(HudRetained),
}

impl fmt::Display for HudHostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Retained(retained) => {
                write!(f, "an earlier HUD is still retained ({:?})", retained.reason)
            }
        }
    }
}

impl std::error::Error for HudHostError {}

/// The host-lifetime HUD owner.
///
/// It owns the HUD backend and executor. Each start builds a fresh
/// [`HudControl`]/[`HudEvents`] pair (a released control is retired). A control
/// whose release was not acknowledged is *retained* here: the next
/// [`HudHost::create`] and the teardown owner retry it first, and while it stays
/// retained no second panel is created.
pub struct HudHost {
    executor: Arc<dyn MainThreadExecutor>,
    backend: Arc<dyn HudBackend>,
    budget: Duration,
    retained: Mutex<Option<HudControl>>,
}

impl HudHost {
    /// A host over `backend`, with the default acknowledgement budget.
    #[must_use]
    pub fn new(executor: Arc<dyn MainThreadExecutor>, backend: Arc<dyn HudBackend>) -> Self {
        Self {
            executor,
            backend,
            budget: ACKNOWLEDGEMENT_BUDGET,
            retained: Mutex::new(None),
        }
    }

    /// A host over the system backend. Creating it shows and installs nothing.
    #[must_use]
    pub fn system(executor: Arc<dyn MainThreadExecutor>) -> Self {
        Self::new(executor, Arc::new(SystemHudBackend))
    }

    /// Sets the per-hop acknowledgement budget of every control it creates.
    #[must_use]
    pub const fn with_acknowledgement_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// Whether a control whose release was not acknowledged is retained.
    #[must_use]
    pub fn has_retained(&self) -> bool { lock(&self.retained).is_some() }

    /// Like [`Self::has_retained`] but never waits: `None` while a settle holds
    /// the lock across a release.
    #[must_use]
    pub fn try_has_retained(&self) -> Option<bool> {
        match self.retained.try_lock() {
            Ok(retained) => Some(retained.is_some()),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                Some(poisoned.into_inner().is_some())
            }
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    /// Keeps `control` until its release is acknowledged.
    pub fn retain(&self, control: HudControl) { *lock(&self.retained) = Some(control); }

    /// Retries a retained control's release.
    ///
    /// Nothing retained is success. The wait is bounded by the control's
    /// acknowledgement budget and `cancel`.
    ///
    /// # Errors
    /// Returns the still-retained ownership.
    #[allow(clippy::significant_drop_tightening)] // The held guard is the critical section.
    pub fn settle(&self, cancel: &CancellationToken) -> Result<(), HudHostError> {
        let mut retained = lock(&self.retained);
        let Some(control) = retained.as_ref() else {
            return Ok(());
        };
        match control.release(cancel) {
            HudReleaseOutcome::Released => {
                *retained = None;
                Ok(())
            }
            HudReleaseOutcome::Retained(detail) => Err(HudHostError::Retained(detail)),
        }
    }

    /// Creates a fresh control after settling any retained predecessor.
    ///
    /// # Errors
    /// Returns [`HudHostError::Retained`] while the predecessor is retained.
    pub fn create(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(HudControl, HudEvents), HudHostError> {
        self.settle(cancel)?;
        Ok(HudControl::with_acknowledgement_budget(
            Arc::clone(&self.executor),
            Arc::clone(&self.backend),
            self.budget,
        ))
    }
}

/// What the composition root assembles.
pub struct HoldToQuitParts {
    /// The one application control (observation, preflight, graceful terminate).
    pub control: ApplicationControl,
    /// The HUD owner.
    pub hud: Arc<HudHost>,
    /// The host's frontmost-context lease.
    pub frontmost: Arc<dyn FrontmostLeaser>,
    /// The module's time source and wait point.
    pub clock: Arc<dyn HoldClock>,
    /// The priority of the input subscription.
    pub priority: InputPriority,
}

/// What the worker reported after one graceful attempt.
///
/// `Requested` is an accepted request, not proof the application exited, and
/// `Uncertain` is never `Refused`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueReport {
    /// The run that issued it.
    pub generation: u64,
    /// The claimed press.
    pub press: u64,
    /// The truthful graceful outcome.
    pub outcome: GracefulOutcome,
}

struct Inner {
    parts: HoldToQuitParts,
    watch: OnceLock<AcceptedWatch<CommandQuitConfig>>,
    next_generation: AtomicU64,
    active: AtomicU64,
    degraded: Mutex<Option<String>>,
    reports: Mutex<VecDeque<IssueReport>>,
    #[cfg(any(test, feature = "testing"))]
    reconfigured: Mutex<Option<ReconfigureHook>>,
    #[cfg(any(test, feature = "testing"))]
    adopting: Mutex<Option<AdoptHook>>,
}

/// A test seam run by the worker inside `adopt`: `false` after it decided to
/// adopt a candidate and before it applies it, `true` once it has applied it.
#[cfg(any(test, feature = "testing"))]
pub type AdoptHook = Arc<dyn Fn(bool) + Send + Sync>;

/// A test seam run at the end of a successful `reconfigure`, before the
/// registry publishes the candidate.
#[cfg(any(test, feature = "testing"))]
pub type ReconfigureHook = Arc<dyn Fn() + Send + Sync>;

impl Inner {
    /// Marks HUD-degraded mode for `generation`, once. A late result from an
    /// older run is inert.
    fn degrade(&self, generation: u64, detail: String) -> bool {
        let mut degraded = lock(&self.degraded);
        if self.active.load(Ordering::SeqCst) != generation || degraded.is_some() {
            return false;
        }
        *degraded = Some(detail);
        true
    }

    fn report(&self, report: IssueReport) {
        if self.active.load(Ordering::SeqCst) != report.generation {
            return;
        }
        let mut reports = lock(&self.reports);
        if reports.len() >= REPORT_CAPACITY {
            reports.pop_front();
        }
        reports.push_back(report);
    }
}

/// The host-lifetime Hold-to-Quit service. Cloning shares it.
#[derive(Clone)]
pub struct HoldToQuitRuntime {
    inner: Arc<Inner>,
}

impl HoldToQuitRuntime {
    /// The module identifier.
    pub const ID: ModuleId = CommandQuitConfig::ID;

    /// Builds the service. Inert: nothing native is created.
    #[must_use]
    pub fn new(parts: HoldToQuitParts) -> Self {
        Self {
            inner: Arc::new(Inner {
                parts,
                watch: OnceLock::new(),
                next_generation: AtomicU64::new(0),
                active: AtomicU64::new(0),
                degraded: Mutex::new(None),
                reports: Mutex::new(VecDeque::new()),
                #[cfg(any(test, feature = "testing"))]
                reconfigured: Mutex::new(None),
                #[cfg(any(test, feature = "testing"))]
                adopting: Mutex::new(None),
            }),
        }
    }

    /// A new module over this service. The registry owns it.
    #[must_use]
    pub fn module(&self) -> HoldToQuitModule {
        HoldToQuitModule {
            inner: Arc::clone(&self.inner),
            config: None,
            run: None,
        }
    }

    /// Installs the module's typed accepted-configuration watch. Call it before
    /// the registry starts modules. A second install is ignored.
    pub fn install_accepted_watch(&self, watch: AcceptedWatch<CommandQuitConfig>) {
        let _ = self.inner.watch.set(watch);
    }

    /// The HUD-degraded detail while the runtime is running without a
    /// subscription, else `None`. Published data only: a short lock, no native
    /// work.
    #[must_use]
    pub fn degraded_detail(&self) -> Option<String> { lock(&self.inner.degraded).clone() }

    /// Installs a seam run at the end of every successful `reconfigure`, after
    /// the candidate is staged and before the registry publishes it, so a test
    /// can hold the reconfigure-to-publication window open.
    #[cfg(any(test, feature = "testing"))]
    pub fn set_reconfigure_hook(&self, hook: Option<ReconfigureHook>) {
        *lock(&self.inner.reconfigured) = hook;
    }

    /// Installs a seam run by the worker around each adoption of a staged
    /// candidate (see [`AdoptHook`]).
    #[cfg(any(test, feature = "testing"))]
    pub fn set_adopt_hook(&self, hook: Option<AdoptHook>) { *lock(&self.inner.adopting) = hook; }

    /// The most recent graceful attempts of the active run, oldest first.
    #[must_use]
    pub fn reports(&self) -> Vec<IssueReport> {
        lock(&self.inner.reports).iter().cloned().collect()
    }
}

// ---------------------------------------------------------------------------
// Run state
// ---------------------------------------------------------------------------

/// The owners a run holds, each cleared only when its cleanup is acknowledged.
#[derive(Default)]
struct Resources {
    lease: Option<Box<dyn LeaseHandle>>,
    observation: Option<Arc<ApplicationObservation>>,
    hud: Option<HudControl>,
}

struct Run {
    generation: u64,
    token: CancellationToken,
    shared: Arc<Shared>,
    resources: Mutex<Resources>,
    subscription: Arc<Mutex<Option<Subscription>>>,
    guards: Mutex<Vec<CancelListener>>,
}

/// Hold-to-Quit as a registry module.
pub struct HoldToQuitModule {
    inner: Arc<Inner>,
    config: Option<CommandQuitConfig>,
    run: Option<Arc<Run>>,
}

impl HoldToQuitModule {
    /// Reverse rollback and stop in one idempotent routine. Returns what is
    /// still owed; a retained owner stays in `run.resources` for the retry.
    fn teardown(&self, run: &Run, logger: &ScopedLogger) -> Vec<String> {
        let parts = &self.inner.parts;
        let shared = &run.shared;
        // 1. Logical invalidation first: nothing can start, show or issue.
        shared.accepting.store(false, Ordering::SeqCst);
        shared.hud_ready.store(false, Ordering::SeqCst);
        shared.cancel_all();
        run.token.cancel();
        shared.wake();
        let mut owed = Vec::new();
        // 2. Drop the subscription. The shared service keeps a suppressing tap
        // alive while a tombstone for a claimed press lives.
        let subscription = lock(&run.subscription).take();
        if let Some(subscription) = subscription {
            let report = subscription.unsubscribe();
            if let NativeCleanup::Retained(retained) = report.native {
                logger.warn(format!(
                    "the input tap could not be closed and stays retained: {retained:?}"
                ));
            }
        }
        // 3. Hide and release the HUD with a fresh token: the run token is
        // already cancelled and cleanup runs regardless.
        let mut resources = lock(&run.resources);
        if let Some(hud) = resources.hud.take() {
            match hud.release(&CancellationToken::new()) {
                HudReleaseOutcome::Released => {
                    // A predecessor copy retained by the host is acknowledged too.
                    if let Err(failure) = parts.hud.settle(&CancellationToken::new()) {
                        owed.push(format!("an earlier HUD is still retained: {failure}"));
                    }
                }
                HudReleaseOutcome::Retained(detail) => {
                    parts.hud.retain(hud.clone());
                    resources.hud = Some(hud);
                    owed.push(format!("HUD cleanup is retained ({:?})", detail.reason));
                }
            }
        }
        // 4. Stop the application observation.
        if let Some(observation) = resources.observation.clone() {
            match observation.stop() {
                Ok(()) => resources.observation = None,
                Err(failure) => owed.push(format!(
                    "the application observation could not be stopped: {failure}"
                )),
            }
        }
        // 5. Release the host frontmost-context lease.
        if let Some(lease) = resources.lease.take()
            && let Err(failure) = lease.release()
        {
            owed.push(format!(
                "the frontmost context lease could not be released: {failure}"
            ));
        }
        drop(resources);
        if owed.is_empty() {
            lock(&self.inner.degraded).take();
            let _ = self.inner.active.compare_exchange(
                run.generation,
                0,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
        }
        owed
    }

    /// Rolls a failed start back and builds its typed error.
    fn fail_start(
        &mut self,
        run: &Arc<Run>,
        ctx: &ModuleContext,
        category: ErrorCategory,
        reason: String,
    ) -> ModuleError {
        let owed = self.teardown(run, ctx.logger());
        if owed.is_empty() {
            self.run = None;
        }
        let message = if owed.is_empty() {
            reason
        } else {
            format!("{reason}; cleanup also pending: {}", owed.join("; "))
        };
        ctx.logger().warn(message.clone());
        error(Operation::Start, category, message)
    }
}

impl Module for HoldToQuitModule {
    type Config = CommandQuitConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: CommandQuitConfig::ID,
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
        }
    }

    fn required_permissions(&self, config: &CommandQuitConfig) -> Vec<PermissionRequirement> {
        if config.enabled {
            vec![PermissionRequirement::shared_input(
                Permission::Accessibility,
            )]
        } else {
            Vec::new()
        }
    }

    fn configure(&mut self, config: &CommandQuitConfig, _ctx: &ModuleContext) -> ModuleResult<()> {
        self.config = Some(config.clone());
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // One ordered acquisition with a rollback at every step.
    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        let inner = Arc::clone(&self.inner);
        if let Some(previous) = self.run.clone() {
            // A failed stop left a run behind; resolve it before a successor.
            let owed = self.teardown(&previous, ctx.logger());
            if !owed.is_empty() {
                return Err(error(
                    Operation::Start,
                    ErrorCategory::Platform,
                    format!("earlier cleanup is still pending: {}", owed.join("; ")),
                ));
            }
            self.run = None;
        }
        let Some(config) = self.config.clone() else {
            return Err(error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "not configured",
            ));
        };
        let Some(accepted) = inner.watch.get().cloned() else {
            return Err(error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "the accepted configuration watch is not installed",
            ));
        };
        let Some(pair) = accepted.read().filter(|pair| *pair.config == config) else {
            return Err(error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "the configured value is not the accepted one",
            ));
        };
        let parts = &inner.parts;

        // 1. Generation, inbox and cancellation state.
        let generation = inner.next_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let token = CancellationToken::new();
        let shared = Arc::new(Shared::new());
        let run = Arc::new(Run {
            generation,
            token: token.clone(),
            shared: Arc::clone(&shared),
            resources: Mutex::new(Resources::default()),
            subscription: Arc::new(Mutex::new(None)),
            guards: Mutex::new(Vec::new()),
        });
        // Runtime cancellation and host invalidation both end this run's waits
        // and make every press inert before any lock-bound work.
        {
            let on_cancel = token.clone();
            let wake = Arc::clone(&shared);
            lock(&run.guards).push(ctx.cancellation().on_cancel(Arc::new(move || {
                on_cancel.cancel();
                wake.wake();
            })));
            let on_invalidate = token.clone();
            let invalidated = Arc::clone(&shared);
            ctx.invalidation().on_invalidate(Arc::new(move || {
                invalidated.accepting.store(false, Ordering::SeqCst);
                invalidated.cancel_all();
                on_invalidate.cancel();
            }));
        }
        inner.active.store(generation, Ordering::SeqCst);
        lock(&inner.degraded).take();
        self.run = Some(Arc::clone(&run));

        // 2. The host frontmost-context lease.
        match parts.frontmost.lease(&token) {
            Ok(lease) => lock(&run.resources).lease = Some(lease),
            Err(failure) => {
                return Err(self.fail_start(
                    &run,
                    ctx,
                    ErrorCategory::Platform,
                    format!("the frontmost context is unavailable: {failure}"),
                ));
            }
        }

        // 3. Application observation, then the frontmost watch (observation
        // before the first confirmed snapshot).
        let observation = match parts.control.install(&token, |_| {}) {
            Ok(observation) => Arc::new(observation),
            Err(failure) => {
                // The failed install already invalidated its generation and the
                // control keeps the exact cleanup owned; acknowledge it here
                // because no observation handle exists to stop.
                let cleanup = match parts.control.stop() {
                    Ok(()) => String::new(),
                    Err(cleanup) => format!("; observation cleanup also failed: {cleanup}"),
                };
                return Err(self.fail_start(
                    &run,
                    ctx,
                    ErrorCategory::Platform,
                    format!("failed to install the application observer: {failure}{cleanup}"),
                ));
            }
        };
        lock(&run.resources).observation = Some(Arc::clone(&observation));
        let watch = match observation
            .watch_frontmost_observed(&token, Arc::new(ActivationCancel(Arc::clone(&shared))))
        {
            Ok(watch) => watch,
            Err(failure) => {
                return Err(self.fail_start(
                    &run,
                    ctx,
                    ErrorCategory::Platform,
                    format!("failed to watch the frontmost application: {failure}"),
                ));
            }
        };

        // 4. The HUD: Running only after the acknowledged hidden panel.
        let (hud, events) = match parts.hud.create(&token) {
            Ok(pair) => pair,
            Err(failure) => {
                return Err(self.fail_start(
                    &run,
                    ctx,
                    ErrorCategory::Platform,
                    format!("the HUD cannot be created: {failure}"),
                ));
            }
        };
        lock(&run.resources).hud = Some(hud.clone());
        let handle = match hud.install(&token) {
            Ok(handle) => handle,
            Err(failure) => {
                return Err(self.fail_start(
                    &run,
                    ctx,
                    ErrorCategory::Platform,
                    format!("failed to install the HUD: {failure}"),
                ));
            }
        };

        // 5. Accept the local policy revision for the accepted configuration.
        let accepted_token = match observation.accept() {
            Ok(accepted_token) => accepted_token,
            Err(failure) => {
                return Err(self.fail_start(
                    &run,
                    ctx,
                    ErrorCategory::Platform,
                    format!("failed to accept the initial policy revision: {failure}"),
                ));
            }
        };

        // 6. The worker. The change listener is registered first so a
        // publication between the read above and the worker is never lost.
        shared.observed_revision.store(pair.revision, Ordering::SeqCst);
        let listener_shared = Arc::clone(&shared);
        let listener_watch = accepted.clone();
        let changes = accepted.on_change(
            pair.revision,
            Arc::new(move || {
                if let Some(pair) = listener_watch.read() {
                    listener_shared.observed_revision.store(pair.revision, Ordering::SeqCst);
                }
                listener_shared.wake();
            }),
        );
        let worker = Worker {
            inner: Arc::clone(&inner),
            generation,
            shared: Arc::clone(&shared),
            observation: Arc::clone(&observation),
            watch: watch.clone(),
            accepted,
            hud,
            handle,
            events,
            subscription: Arc::clone(&run.subscription),
            logger: ctx.logger().clone(),
            token,
            clock: Arc::clone(&parts.clock),
            armed: Armed {
                config,
                token: accepted_token,
                revision: pair.revision,
            },
            phase: Phase::Idle,
            linger: None,
            _changes: changes,
        };
        if let Err(failure) = ctx.spawn_worker("hold-to-quit", move |group| worker.run(&group)) {
            return Err(self.fail_start(
                &run,
                ctx,
                ErrorCategory::Platform,
                format!("failed to spawn the Hold-to-Quit worker: {failure}"),
            ));
        }

        // 7. Subscribe the handler last, once everything it needs is live.
        shared.armed_revision.store(pair.revision, Ordering::SeqCst);
        shared.hud_ready.store(true, Ordering::SeqCst);
        shared.accepting.store(true, Ordering::SeqCst);
        let handler = Arc::new(HoldHandler::new(
            Arc::clone(&shared),
            watch,
            Arc::clone(&parts.clock),
        ));
        let spec = HandlerSpec {
            filter: filter(),
            priority: parts.priority,
            work: WorkRequirement::None,
        };
        match ctx.input().subscribe_handler(spec, handler) {
            Ok(subscription) => *lock(&run.subscription) = Some(subscription),
            Err(failure) => {
                let category = match failure {
                    InputError::Invalidated | InputError::Cancelled => ErrorCategory::Lifecycle,
                    _ => ErrorCategory::Platform,
                };
                return Err(self.fail_start(
                    &run,
                    ctx,
                    category,
                    format!("failed to subscribe to the shared input service: {failure}"),
                ));
            }
        }
        if ctx.cancellation().is_cancelled() {
            return Err(self.fail_start(
                &run,
                ctx,
                ErrorCategory::Lifecycle,
                "start was cancelled while the runtime was starting".to_owned(),
            ));
        }
        // A HUD failure that raced the subscription already degraded the run.
        let degraded = lock(&inner.degraded).is_some();
        let late = if degraded {
            lock(&run.subscription).take()
        } else {
            None
        };
        if let Some(subscription) = late {
            let _ = subscription.unsubscribe();
        }
        ctx.logger().debug(format!("Hold-to-Quit generation {generation} started"));
        Ok(())
    }

    fn reconfigure(&mut self, config: &CommandQuitConfig, ctx: &ModuleContext) -> ModuleResult<()> {
        if self.config.as_ref() == Some(config) {
            return Ok(());
        }
        if let Some(run) = self.run.clone() {
            let observation = lock(&run.resources).observation.clone();
            let Some(observation) = observation else {
                return Err(error(
                    Operation::Reconfigure,
                    ErrorCategory::Lifecycle,
                    "the application observation is gone",
                ));
            };
            // The last fallible step: a rejected acceptance leaves the old token,
            // configuration, readiness and press untouched.
            let token = observation.accept().map_err(|failure| {
                error(
                    Operation::Reconfigure,
                    ErrorCategory::Platform,
                    format!("cannot accept the new policy revision: {failure}"),
                )
                .with_source(failure)
            })?;
            {
                // Cleared under the staged lock so an `adopt` of an older
                // candidate cannot re-open acceptance while this one is staged.
                let mut staged = lock(&run.shared.staged);
                run.shared.accepting.store(false, Ordering::SeqCst);
                *staged = Some(Staged { config: config.clone(), token });
            }
            run.shared.cancel_all();
            run.shared.wake();
            ctx.logger().debug(format!(
                "Hold-to-Quit generation {} staged a new configuration",
                run.generation
            ));
            #[cfg(any(test, feature = "testing"))]
            {
                let hook = lock(&self.inner.reconfigured).clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
        }
        // A paused or stopped module only stages the next-start snapshot.
        self.config = Some(config.clone());
        Ok(())
    }

    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        let Some(run) = self.run.clone() else {
            return Ok(());
        };
        let owed = self.teardown(&run, ctx.logger());
        if owed.is_empty() {
            self.run = None;
            ctx.logger()
                .debug(format!("Hold-to-Quit generation {} stopped", run.generation));
            Ok(())
        } else {
            let message = owed.join("; ");
            ctx.logger().error(message.clone());
            Err(error(Operation::Stop, ErrorCategory::Platform, message))
        }
    }
}

impl Drop for HoldToQuitModule {
    /// Best-effort inertness when the module is dropped without a lifecycle
    /// stop. The registry always stops it first.
    fn drop(&mut self) {
        if let Some(run) = self.run.take() {
            run.shared.accepting.store(false, Ordering::SeqCst);
            run.shared.cancel_all();
            run.token.cancel();
            run.shared.wake();
        }
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

/// The configuration, native revision token and published revision the worker
/// armed.
struct Armed {
    config: CommandQuitConfig,
    token: AcceptedActionToken,
    revision: u64,
}

/// One live press.
struct Active {
    press: u64,
    application: ApplicationId,
    epoch: u64,
    deadline: Option<std::time::Instant>,
    authorization: Option<PressAuthorization>,
}

/// A released press whose HUD stays visible, frozen, until `until`.
///
/// The press is no longer live: it holds no authorization and can never issue.
/// `until` is `None` when `now + dismissDelay` is unrepresentable; the linger
/// then lasts until the next show, stop, revision change or degrade.
struct Linger {
    press: u64,
    until: Option<Instant>,
}

/// How an ended press leaves the HUD.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dismissal {
    /// Hide now.
    Immediate,
    /// Freeze the bar and hide after `dismissDelay`.
    Linger,
}

enum Phase {
    Idle,
    AwaitingShow(Active),
    Tracking(Active),
}

struct Worker {
    inner: Arc<Inner>,
    generation: u64,
    shared: Arc<Shared>,
    observation: Arc<ApplicationObservation>,
    watch: FrontmostWatch,
    accepted: AcceptedWatch<CommandQuitConfig>,
    hud: HudControl,
    handle: HudHandle,
    events: HudEvents,
    subscription: Arc<Mutex<Option<Subscription>>>,
    logger: ScopedLogger,
    token: CancellationToken,
    clock: Arc<dyn HoldClock>,
    armed: Armed,
    phase: Phase,
    /// The released press whose HUD is still on screen, if any.
    linger: Option<Linger>,
    /// Keeps the change listener registered for the worker's lifetime.
    _changes: AcceptedSubscription,
}

impl Worker {
    fn run(mut self, group: &CancellationToken) {
        // Registered before anything else so wakes reach this thread.
        self.shared.set_worker(std::thread::current());
        let wake = Arc::clone(&self.shared);
        let _group_wake = group.on_cancel(Arc::new(move || wake.wake()));
        loop {
            if self.token.is_cancelled() || group.is_cancelled() {
                break;
            }
            self.adopt();
            self.drain_inbox();
            self.drain_events();
            self.step();
            self.expire_linger();
            let timeout = self.timeout();
            self.clock.park(timeout);
        }
        self.abandon("the runtime is stopping");
        self.end_linger();
    }

    /// Hides a pending linger now and clears it.
    fn end_linger(&mut self) {
        if let Some(linger) = self.linger.take() {
            self.hud.hide(self.handle, hud_press(linger.press));
            self.logger.debug(format!("press {} HUD dismissed", linger.press));
        }
    }

    /// Ends the linger once its time has passed.
    fn expire_linger(&mut self) {
        let due = self
            .linger
            .as_ref()
            .and_then(|linger| linger.until)
            .is_some_and(|until| self.clock.now() >= until);
        if due {
            self.end_linger();
        }
    }

    /// Whether a cancelled press ended because the user (or the activation
    /// listener) ended it, rather than because the runtime or its policy did.
    fn ended_by_user(&self) -> bool {
        let shared = &self.shared;
        shared.accepting.load(Ordering::SeqCst)
            && shared.armed_revision.load(Ordering::SeqCst) == self.armed.revision
            && !self.token.is_cancelled()
    }

    /// The single wait: idle has none, a pending show waits in
    /// `CANCELLATION_QUANTUM` slices, tracking waits for the checked deadline.
    fn timeout(&self) -> Option<Duration> {
        match &self.phase {
            Phase::Idle => self
                .linger
                .as_ref()
                .and_then(|linger| linger.until)
                .map(|until| until.saturating_duration_since(self.clock.now())),
            Phase::AwaitingShow(_) => Some(CANCELLATION_QUANTUM),
            Phase::Tracking(active) => active
                .deadline
                .map(|deadline| deadline.saturating_duration_since(self.clock.now())),
        }
    }

    /// Re-arms after the staged candidate's accepted publication: the typed
    /// value must be the staged one at a newer revision than the armed one.
    fn adopt(&mut self) {
        let Some(staged) = lock(&self.shared.staged).clone() else {
            return;
        };
        let Some(pair) = self.accepted.read() else {
            return;
        };
        if *pair.config != staged.config || pair.revision == self.armed.revision {
            return;
        }
        #[cfg(any(test, feature = "testing"))]
        let hook = lock(&self.inner.adopting).clone();
        #[cfg(any(test, feature = "testing"))]
        if let Some(hook) = &hook {
            hook(false);
        }
        if self.live_press().is_some() {
            // A press claimed under the old revision must not outlive it.
            self.abandon("the policy revision changed");
        }
        self.end_linger();
        self.armed = Armed {
            config: staged.config,
            token: staged.token,
            revision: pair.revision,
        };
        self.shared.observed_revision.fetch_max(pair.revision, Ordering::SeqCst);
        self.shared.armed_revision.store(pair.revision, Ordering::SeqCst);
        // Acceptance re-opens only under the staged lock and only when no newer
        // candidate is staged; `reconfigure` closes it under the same lock.
        let mut slot = lock(&self.shared.staged);
        if slot.as_ref().is_some_and(|current| current.token == staged.token) {
            *slot = None;
        }
        if slot.is_none() && self.shared.hud_ready.load(Ordering::SeqCst) {
            self.shared.accepting.store(true, Ordering::SeqCst);
            // A stop that raced the store leaves nothing accepting.
            if self.token.is_cancelled() {
                self.shared.accepting.store(false, Ordering::SeqCst);
            }
        }
        drop(slot);
        #[cfg(any(test, feature = "testing"))]
        if let Some(hook) = &hook {
            hook(true);
        }
    }

    fn drain_inbox(&mut self) {
        while let Some(down) = self.shared.pop() {
            self.begin(down);
        }
    }

    fn drain_events(&mut self) {
        while let Some(event) = self.events.try_recv() {
            self.on_event(event);
        }
    }

    const fn live_press(&self) -> Option<u64> {
        match &self.phase {
            Phase::Idle => None,
            Phase::AwaitingShow(active) | Phase::Tracking(active) => Some(active.press),
        }
    }

    /// Starts a claimed down: validates it and sends the non-waiting show.
    fn begin(&mut self, down: Down) {
        if self.live_press().is_some() {
            self.abandon("superseded by a newer press");
        }
        let shared = &self.shared;
        if shared.is_cancelled(down.press) {
            return;
        }
        if down.revision != self.armed.revision
            || !shared.accepting.load(Ordering::SeqCst)
            || !self.watch.is_epoch_current(down.epoch)
        {
            shared.cancel(down.press);
            return;
        }
        // Main revalidates the exact instance before anything is shown.
        let front = match self.observation.preflight_frontmost(down.application) {
            Ok(FrontmostPreflight::Eligible(front)) => front,
            Ok(other) => {
                self.logger
                    .debug(format!("press {} cancelled by preflight: {other:?}", down.press));
                shared.cancel(down.press);
                return;
            }
            Err(failure) => {
                self.logger.debug(format!(
                    "press {} cancelled: preflight failed: {failure}",
                    down.press
                ));
                shared.cancel(down.press);
                return;
            }
        };
        if shared.is_cancelled(down.press) {
            return;
        }
        // A failure keeps the HUD's own placement fallback.
        let display = self
            .observation
            .display_for(down.application)
            .ok()
            .and_then(|hint| hint.display);
        let hold = self.armed.config.hold_duration;
        let elapsed = self.clock.now().saturating_duration_since(down.at);
        let mut request = HudRequest::new(self.handle, hud_press(down.press), hold)
            .with_anchor(hud_anchor(self.armed.config.position))
            .with_motion(hud_motion(self.armed.config.animation))
            .with_elapsed_ms(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
            .with_authorization(HudAuthorization::new(hud_predicate(
                shared,
                &self.watch,
                down.press,
                down.epoch,
            )));
        if let Some(name) = front.name {
            request = request.with_label(name);
        }
        if let Some(display) = display {
            request = request.with_display(display);
        }
        let superseded = self.linger.take();
        self.phase = Phase::AwaitingShow(Active {
            press: down.press,
            application: down.application,
            epoch: down.epoch,
            deadline: deadline(down.at, hold),
            authorization: None,
        });
        self.hud.show(request);
        // The new show supersedes the lingering panel. The hide of the old press
        // is queued right after the new show (FIFO main queue): the press check in
        // the HUD makes it a no-op when the new press is displayed, and it orders
        // the old panel out when the new show was refused before touching it.
        if let Some(linger) = superseded {
            self.hud.hide(self.handle, hud_press(linger.press));
            self.logger.debug(format!("press {} HUD superseded", linger.press));
        }
    }

    fn on_event(&mut self, event: HudEvent) {
        let live = self.live_press().map(hud_press);
        let is_live = event.handle == self.handle && live == Some(event.press);
        match event.outcome {
            HudOutcome::Shown(_) => {
                if self.is_lingering(&event) {
                    // Shown after the release: frozen too, neither tracked nor hidden.
                    self.hud.freeze(event.handle, event.press);
                } else if is_live && matches!(self.phase, Phase::AwaitingShow(_)) {
                    self.track();
                } else if !is_live {
                    // A late show for a dead press: ignored and hidden.
                    self.hud.hide(event.handle, event.press);
                }
            }
            HudOutcome::Stale | HudOutcome::Unavailable(_) | HudOutcome::Failed(_)
                if self.is_lingering(&event) =>
            {
                // The HUD already dropped it: no second hide.
                self.linger = None;
            }
            HudOutcome::Stale => {
                if is_live {
                    self.abandon("the HUD reported the press stale");
                }
            }
            HudOutcome::Unavailable(reason) => {
                if is_live {
                    self.abandon("the HUD is unavailable");
                    self.degrade(&format!("the HUD is unavailable ({reason:?})"));
                }
            }
            HudOutcome::Failed(message) => {
                if is_live {
                    self.abandon("the HUD failed");
                    self.degrade(&format!("the HUD failed: {message}"));
                }
            }
        }
    }

    fn is_lingering(&self, event: &HudEvent) -> bool {
        event.handle == self.handle
            && self
                .linger
                .as_ref()
                .is_some_and(|linger| hud_press(linger.press) == event.press)
    }

    /// A matching `Shown` arms the press: creates the one-shot authorization
    /// and publishes it so the callback can cancel it.
    fn track(&mut self) {
        let Phase::AwaitingShow(mut active) = std::mem::replace(&mut self.phase, Phase::Idle)
        else {
            return;
        };
        let authorization =
            PressAuthorization::new(hud_press(active.press), active.application, self.armed.token);
        self.shared.publish_authorization(active.press, &authorization);
        active.authorization = Some(authorization);
        self.phase = Phase::Tracking(active);
    }

    /// Cancels or fires the live press if its state says so.
    fn step(&mut self) {
        let (press, epoch, deadline_passed) = match &self.phase {
            Phase::Idle => return,
            Phase::AwaitingShow(active) => (active.press, active.epoch, false),
            Phase::Tracking(active) => (
                active.press,
                active.epoch,
                active.deadline.is_some_and(|deadline| self.clock.now() >= deadline),
            ),
        };
        let shared = &self.shared;
        if shared.is_cancelled(press) {
            let dismissal = if self.ended_by_user() {
                Dismissal::Linger
            } else {
                Dismissal::Immediate
            };
            self.end_press("the press was released or cancelled", dismissal);
        } else if !shared.accepting.load(Ordering::SeqCst) {
            self.abandon("the runtime stopped accepting");
        } else if !self.watch.is_epoch_current(epoch) {
            self.abandon("the frontmost application changed");
        } else if deadline_passed {
            self.fire();
        }
    }

    /// The deadline: drain, recheck once, hide, then ask for a graceful quit.
    fn fire(&mut self) {
        let Some(press) = self.live_press() else {
            return;
        };
        // A release or cancel delivered at or before the deadline wins. Input
        // not yet delivered cannot be known.
        self.drain_inbox();
        self.drain_events();
        let Phase::Tracking(active) = &self.phase else {
            return;
        };
        if active.press != press {
            return;
        }
        let shared = &self.shared;
        let cancelled = shared.is_cancelled(press);
        if cancelled
            || !shared.accepting.load(Ordering::SeqCst)
            || shared.armed_revision.load(Ordering::SeqCst) != self.armed.revision
            || !self.watch.is_epoch_current(active.epoch)
            || self.token.is_cancelled()
        {
            let dismissal = if cancelled && self.ended_by_user() {
                Dismissal::Linger
            } else {
                Dismissal::Immediate
            };
            self.end_press("the press ended at the deadline", dismissal);
            return;
        }
        let Phase::Tracking(active) = std::mem::replace(&mut self.phase, Phase::Idle) else {
            return;
        };
        let Some(authorization) = active.authorization.as_ref() else {
            return;
        };
        self.hud.hide(self.handle, hud_press(active.press));
        let outcome = self.observation.terminate_gracefully(
            self.armed.token,
            active.application,
            authorization,
        );
        // The press is spent; the callback only keeps pairing it.
        shared.cancel(active.press);
        shared.clear_authorization(active.press);
        self.report(active.press, outcome);
    }

    fn report(&self, press: u64, outcome: GracefulOutcome) {
        let generation = self.generation;
        match &outcome {
            GracefulOutcome::Requested => self.logger.info(format!(
                "Hold-to-Quit asked the application to quit (press {press}); this is a request, not proof of exit"
            )),
            GracefulOutcome::Refused => self
                .logger
                .info(format!("the application refused the quit request (press {press})")),
            GracefulOutcome::Uncertain(failure) => self.logger.warn(format!(
                "the quit request may or may not have been sent (press {press}): {failure}"
            )),
            other => self
                .logger
                .debug(format!("no quit request was sent (press {press}): {other:?}")),
        }
        self.inner.report(IssueReport { generation, press, outcome });
    }

    /// Ends the live press without issuing: cancel, cancel the authorization,
    /// hide.
    fn abandon(&mut self, reason: &str) { self.end_press(reason, Dismissal::Immediate); }

    /// Ends the live press without issuing. The cancel, authorization cancel and
    /// `clear_authorization` always happen now; a lingering dismissal only
    /// defers the hide, after freezing the bar.
    fn end_press(&mut self, reason: &str, dismissal: Dismissal) {
        let (Phase::AwaitingShow(active) | Phase::Tracking(active)) =
            std::mem::replace(&mut self.phase, Phase::Idle)
        else {
            return;
        };
        self.shared.cancel(active.press);
        if let Some(authorization) = &active.authorization {
            let _ = authorization.cancel();
        }
        self.shared.clear_authorization(active.press);
        let delay = self.armed.config.dismiss_delay;
        if dismissal == Dismissal::Linger && delay > 0 {
            self.hud.freeze(self.handle, hud_press(active.press));
            self.linger = Some(Linger {
                press: active.press,
                until: self.clock.now().checked_add(Duration::from_millis(delay)),
            });
        } else {
            self.hud.hide(self.handle, hud_press(active.press));
        }
        self.logger.debug(format!("press {} ended: {reason}", active.press));
    }

    /// HUD-degraded mode: stay Running, drop the subscription so every press
    /// passes, warn once and publish the detail. Recovery is Pause then Resume.
    fn degrade(&mut self, reason: &str) {
        // Defensive: degrade fires for a live press, which never coexists with a
        // linger, but a linger must never outlive degraded mode.
        self.end_linger();
        let detail =
            format!("{reason}; Cmd+Q passes through. Pause and resume Hold-to-Quit to recover");
        if !self.inner.degrade(self.generation, detail.clone()) {
            return;
        }
        self.shared.accepting.store(false, Ordering::SeqCst);
        self.shared.hud_ready.store(false, Ordering::SeqCst);
        self.shared.cancel_all();
        self.logger.warn(detail);
        let subscription = lock(&self.subscription).take();
        if let Some(subscription) = subscription {
            let _ = subscription.unsubscribe();
        }
    }
}
