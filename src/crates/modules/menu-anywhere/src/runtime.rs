//! The `MenuAnywhere` runtime: lifecycle, workers and teardown.
//!
//! [`MenuAnywhereRuntime`] is the host-lifetime service the composition root
//! builds once over one [`ApplicationControl`] and a [`MenuHost`]. Building it is
//! inert. An enabled [`MenuAnywhereModule`] then subscribes one suppressing
//! handler to the shared input service (see [`crate::trigger`] for the callback
//! decision) and runs two workers.
//!
//! # Threads
//!
//! ```text
//! tap callback ──try_send──▶ trigger worker ──▶ capture ▶ read ▶ show ▶ track ▶ invoke ▶ release
//!                                 ▲ owns the busy reservation of exactly one request
//! registry publication ──▶ config worker ──▶ publish the accepted matcher and revision
//! ```
//!
//! # Pipeline (one admitted trigger)
//!
//! 1. Reject a request older than the 2 s capture window.
//! 2. Capture the exact frontmost instance through the applications adapter and
//!    record trigger, capture-start and capture times separately. A capture hop
//!    over 2 s is rejected. This is the app frontmost at processing time, not a
//!    proven event-time snapshot, and it is never retargeted afterwards.
//! 3. Exclude this process, use the trigger event's Quartz point, read the menu on the AX owner
//!    thread with a validator bound to the captured instance and activation
//!    epoch.
//! 4. Recheck the instance and show the menu at the Quartz point.
//! 5. Wait for tracking to end. A selection is invoked against the captured
//!    instance with Part 02's exact-instance send guards, even if focus moved.
//! 6. Release the presentation (exact tracking removal, then AX release) with a
//!    fresh acknowledgement window and free the busy reservation exactly once.
//!
//! Every stage rechecks the presentation authority. The authority is scoped to
//! the run generation and the presentation: configuration changes never revoke
//! it, while pause, disable, permission or service loss, stop and shutdown do.
//!
//! # Acceptance coherence
//!
//! `reconfigure` only validates and stages the candidate. The config worker
//! observes the registry's published accepted pair through a bounded,
//! cancellation-wakeable [`AcceptedWatch`] wait; only after it observes the
//! revision does it replace the service filter and publish the one coherent
//! [`Published`] tuple the callback reads. A rejected candidate publishes
//! nothing, so the previous filter and revision stay.
//!
//! # Start and stop
//!
//! Start runs off-main in this order: application observation and its frontmost
//! watch, the accepted-action token, the menu stack, the workers, and the input
//! subscription last. The published tuple becomes eligible only after the
//! subscription is acknowledged. Any failure rolls back in reverse and returns a
//! typed [`ModuleError`]. Stop first revokes eligibility and every authority,
//! then unsubscribes, cancels exact tracking, asks the owner for cleanup with
//! fresh 2 s acknowledgements and stops the observation. A timeout is reported
//! as an error and the run is retained for a retry at the next start or at host
//! shutdown; nothing is called released merely because cancellation was
//! requested.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::menu::{
    CancelOutcome, CleanupError, CleanupOutcome, MenuClock, MenuControl, MenuEvent,
    MenuInvocationOutcome, MenuInvocationReport, MenuPresentationOutcome, MenuReadOutcome,
    MenuReleaseOutcome, MenuTarget, PresentationAuthority, PresentationId, ReadRequest,
    ShowRequest, TargetCheck, TargetValidator,
};
use stache_macos::{
    ACKNOWLEDGEMENT_BUDGET, AcceptedActionToken, ApplicationControl, ApplicationId,
    ApplicationObservation, CANCELLATION_QUANTUM, FrontmostSnapshot, FrontmostState,
    FrontmostWatch,
};
use stache_sdk::input::{HandlerSpec, InputError, InputPriority, Subscription, WorkRequirement};
use stache_sdk::threading::CancelListener;
use stache_sdk::{
    AcceptedConfig, AcceptedWait, AcceptedWatch, CancellationToken, ErrorCategory, Module,
    ModuleContext, ModuleDescriptor, ModuleError, ModuleId, ModuleResult, Operation, Permission,
    PermissionRequirement, ScopedLogger,
};

use crate::backend::MenuHost;
use crate::config::MenuAnywhereConfig;
use crate::trigger::{
    Admission, Counters, Matcher, Message, Outcome, Published, PublishedCell, Request,
    TriggerHandler,
};

/// How old an admitted request may be before the worker drops it, and the
/// longest an identity-capture hop may take. This is the adapter's 2 s
/// acknowledgement window.
pub const CAPTURE_WINDOW: Duration = ACKNOWLEDGEMENT_BUDGET;

/// The deadline of a selected action, measured from the selection.
pub const ACTION_BUDGET: Duration = Duration::from_secs(5);

/// The idle interval of the config worker's service-health check.
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(1);

/// How many trigger reports the runtime keeps.
const REPORT_CAPACITY: usize = 16;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn error(operation: Operation, category: ErrorCategory, message: impl Into<String>) -> ModuleError {
    ModuleError::new(MenuAnywhereConfig::ID, operation, category, message)
}

/// A point a test can hold the runtime at.
///
/// The worker and the config worker call the installed hook at each point, so a
/// barrier test can race another event against exactly that boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// The subscription was acknowledged and the tuple is still ineligible.
    Installed,
    /// The worker took an admitted request.
    Admitted,
    /// Before the frontmost capture.
    BeforeCapture,
    /// After the frontmost capture.
    Captured,
    /// Before the AX read is submitted.
    BeforeRead,
    /// After the read returned.
    Read,
    /// Before the menu is shown.
    BeforeShow,
    /// The menu is shown and tracking began.
    Shown,
    /// Before the selected action is invoked.
    BeforeAction,
    /// Before the presentation is released.
    Cleanup,
    /// The config worker observed a revision, before it publishes the tuple.
    AdoptBeforePublish,
    /// The accepted tuple was published.
    AdoptPublished,
}

/// A test seam called at each [`Stage`].
#[cfg(any(test, feature = "testing"))]
pub type StageHook = Arc<dyn Fn(Stage) + Send + Sync>;

/// What the composition root assembles.
pub struct MenuAnywhereParts {
    /// The one application control this runtime owns (observation, frontmost
    /// watch and exact activation).
    pub applications: ApplicationControl,
    /// The native menu stack.
    pub menu: Arc<dyn MenuHost>,
    /// The priority of the input subscription.
    pub priority: InputPriority,
    /// The idle interval of the service-health check.
    pub health_interval: Duration,
    /// The fresh acknowledgement window of every cleanup hop and of an
    /// abandoned read. The system value is [`ACKNOWLEDGEMENT_BUDGET`].
    pub acknowledgement: Duration,
}

/// The honest timing and result of one admitted trigger.
///
/// `captured_at` is when the frontmost identity was captured, not when the
/// click happened: a different application may have become frontmost in
/// between, and that application is the observed capture target.
#[derive(Clone, Debug)]
pub struct TriggerReport {
    /// The run that processed it.
    pub generation: u64,
    /// The accepted revision the trigger matched at.
    pub revision: u64,
    /// The trigger time on the menu clock.
    pub triggered_at: Instant,
    /// When the capture was requested.
    pub capture_started_at: Option<Instant>,
    /// When the capture returned.
    pub captured_at: Option<Instant>,
    /// The exact captured instance.
    pub target: Option<ApplicationId>,
    /// The last typed outcome of the pipeline.
    pub outcome: Outcome,
    /// What was issued for a selected action, if one was selected.
    pub action: Option<MenuInvocationReport>,
}

struct Inner {
    parts: MenuAnywhereParts,
    watch: OnceLock<AcceptedWatch<MenuAnywhereConfig>>,
    next_generation: AtomicU64,
    run: Mutex<Option<Arc<Run>>>,
    degraded: Mutex<Option<String>>,
    /// Set when the shared input service could not acknowledge removal of the
    /// tap; it outlives the run it was observed in.
    cleanup_pending: Mutex<Option<String>>,
    /// The last start failed because the shared input service was missing.
    service_missing: AtomicBool,
    reports: Mutex<VecDeque<TriggerReport>>,
    counters: Arc<Counters>,
    #[cfg(any(test, feature = "testing"))]
    hook: Mutex<Option<StageHook>>,
}

impl Inner {
    #[allow(clippy::unused_self, clippy::missing_const_for_fn)] // Only the testing build reads the hook.
    fn reach(&self, stage: Stage) {
        #[cfg(any(test, feature = "testing"))]
        {
            let hook = lock(&self.hook).clone();
            if let Some(hook) = hook {
                hook(stage);
            }
        }
        let _ = stage;
    }

    fn degrade(&self, generation: u64, detail: String) {
        let current = lock(&self.run).as_ref().map(|run| run.generation);
        if current == Some(generation) {
            lock(&self.degraded).get_or_insert(detail);
        }
    }

    fn report(&self, report: TriggerReport) {
        let current = lock(&self.run).as_ref().map(|run| run.generation);
        if current != Some(report.generation) {
            return;
        }
        let mut reports = lock(&self.reports);
        if reports.len() >= REPORT_CAPACITY {
            reports.pop_front();
        }
        reports.push_back(report);
    }
}

/// The host-lifetime `MenuAnywhere` service. Cloning shares it.
#[derive(Clone)]
pub struct MenuAnywhereRuntime {
    inner: Arc<Inner>,
}

impl MenuAnywhereRuntime {
    /// The module identifier.
    pub const ID: ModuleId = MenuAnywhereConfig::ID;

    /// Builds the service. Inert: no observer, thread, tap or native menu
    /// object exists until an enabled module starts.
    #[must_use]
    pub fn new(parts: MenuAnywhereParts) -> Self {
        Self {
            inner: Arc::new(Inner {
                parts,
                watch: OnceLock::new(),
                next_generation: AtomicU64::new(0),
                run: Mutex::new(None),
                degraded: Mutex::new(None),
                cleanup_pending: Mutex::new(None),
                service_missing: AtomicBool::new(false),
                reports: Mutex::new(VecDeque::new()),
                counters: Arc::new(Counters::default()),
                #[cfg(any(test, feature = "testing"))]
                hook: Mutex::new(None),
            }),
        }
    }

    /// A new module over this service. The registry owns it.
    #[must_use]
    pub fn module(&self) -> MenuAnywhereModule {
        MenuAnywhereModule {
            inner: Arc::clone(&self.inner),
            config: None,
            run: None,
        }
    }

    /// Installs the module's typed accepted-configuration watch. Call it before
    /// the registry starts modules. A second install is ignored.
    pub fn install_accepted_watch(&self, watch: AcceptedWatch<MenuAnywhereConfig>) {
        let _ = self.inner.watch.set(watch);
    }

    /// Why a running module is not healthy, else `None`. Published data only.
    #[must_use]
    pub fn health_detail(&self) -> Option<String> {
        lock(&self.inner.degraded)
            .clone()
            .or_else(|| lock(&self.inner.cleanup_pending).clone())
    }

    /// Why the module is unavailable because the shared input service is
    /// missing or failed, else `None`. It describes the last start only.
    #[must_use]
    pub fn missing_service_detail(&self) -> Option<String> {
        self.inner
            .service_missing
            .load(Ordering::SeqCst)
            .then(|| "missing service `input`: the shared input service is unavailable".to_owned())
    }

    /// A probe that reports whether the active run is still alive anywhere.
    ///
    /// It holds only a weak handle, so it never keeps the run alive itself.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn run_probe(&self) -> Option<Box<dyn Fn() -> bool + Send + Sync>> {
        let weak = Arc::downgrade(lock(&self.inner.run).as_ref()?);
        Some(Box::new(move || weak.upgrade().is_some()))
    }

    /// How often `outcome` happened across all runs.
    #[must_use]
    pub fn count(&self, outcome: Outcome) -> u64 { self.inner.counters.get(outcome) }

    /// The most recent trigger reports of the active run, oldest first.
    #[must_use]
    pub fn reports(&self) -> Vec<TriggerReport> {
        lock(&self.inner.reports).iter().cloned().collect()
    }

    /// The published tuple of the active run, if one exists.
    #[must_use]
    pub fn published(&self) -> Option<Published> {
        lock(&self.inner.run).as_ref().map(|run| run.cell.load())
    }

    /// Whether a busy reservation is held by the active run.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        lock(&self.inner.run).as_ref().is_some_and(|run| run.admission.is_busy())
    }

    /// Host teardown stage 1: revoke eligibility and every authority at once,
    /// without native work or waiting.
    pub fn begin_shutdown(&self) {
        let run = lock(&self.inner.run).clone();
        if let Some(run) = run {
            run.revoke();
        }
    }

    /// Host teardown stage 3: retry cleanup a stop could not acknowledge.
    ///
    /// Returns `true` when nothing is owed.
    #[must_use]
    pub fn finish_shutdown(&self) -> bool {
        let run = lock(&self.inner.run).clone();
        let mut clean = true;
        if let Some(run) = run {
            let owed = self.inner.teardown(&run);
            clean = owed.is_empty();
            if clean {
                self.inner.forget(&run);
            }
        }
        clean && self.inner.stop_applications()
    }

    /// Runs `inside` while the active run's published cell is held for writing,
    /// so a callback during it must fail open. Returns `None` without a run.
    #[cfg(any(test, feature = "testing"))]
    pub fn while_publishing<R>(&self, inside: impl FnOnce() -> R) -> Option<R> {
        let run = lock(&self.inner.run).clone()?;
        let mut result = None;
        run.cell.update(|_| result = Some(inside()));
        result
    }

    /// Marks the active run degraded, as a failed health check would.
    #[cfg(any(test, feature = "testing"))]
    pub fn mark_degraded(&self, detail: &str) {
        let generation = lock(&self.inner.run).as_ref().map(|run| run.generation);
        if let Some(generation) = generation {
            self.inner.degrade(generation, detail.to_owned());
        }
    }

    /// Installs a seam called at each [`Stage`].
    #[cfg(any(test, feature = "testing"))]
    pub fn set_stage_hook(&self, hook: Option<StageHook>) { *lock(&self.inner.hook) = hook; }
}

// ---------------------------------------------------------------------------
// Run state
// ---------------------------------------------------------------------------

/// The authority of one presentation: its run is live and it was not revoked.
/// A configuration change never touches it.
struct Authority {
    live: Arc<AtomicBool>,
    revoked: AtomicBool,
}

impl Authority {
    fn revoke(&self) { self.revoked.store(true, Ordering::SeqCst); }
}

impl PresentationAuthority for Authority {
    fn is_current(&self) -> bool {
        self.live.load(Ordering::SeqCst) && !self.revoked.load(Ordering::SeqCst)
    }
}

/// The exact instance and activation epoch the worker captured.
#[derive(Clone, Copy)]
struct Capture {
    application: ApplicationId,
    epoch: u64,
}

/// Validates reads and the show against the captured instance.
///
/// The instance is current only while no activation was ingested since the
/// capture (so A, B, A is stale too) and the exact instance is still running.
struct RunValidator {
    observation: Arc<ApplicationObservation>,
    watch: FrontmostWatch,
    capture: Mutex<Option<Capture>>,
}

impl RunValidator {
    fn set(&self, capture: Option<Capture>) { *lock(&self.capture) = capture; }

    fn check(&self, application: ApplicationId) -> TargetCheck {
        let Some(capture) = *lock(&self.capture) else {
            return TargetCheck::Stale;
        };
        if capture.application != application || !self.watch.is_epoch_current(capture.epoch) {
            return TargetCheck::Stale;
        }
        match self.observation.is_running(application) {
            Ok(true) => TargetCheck::Current,
            Ok(false) => TargetCheck::Stale,
            Err(_) => TargetCheck::Unavailable,
        }
    }
}

impl TargetValidator for RunValidator {
    fn validate(&self, target: &MenuTarget) -> TargetCheck { self.check(target.application) }
}

struct Live {
    id: PresentationId,
    authority: Arc<Authority>,
}

#[derive(Default)]
struct Tracking {
    current: Option<Live>,
    pending: Vec<PresentationId>,
}

/// The owners a run holds, each cleared only when its cleanup is acknowledged.
#[derive(Default)]
struct Resources {
    observation: Option<Arc<ApplicationObservation>>,
    control: Option<Arc<MenuControl>>,
}

struct Staged {
    config: MenuAnywhereConfig,
    matcher: Matcher,
}

struct Run {
    generation: u64,
    token: CancellationToken,
    live: Arc<AtomicBool>,
    cell: Arc<PublishedCell>,
    admission: Arc<Admission>,
    logger: ScopedLogger,
    resources: Mutex<Resources>,
    subscription: Mutex<Option<Subscription>>,
    tracking: Mutex<Tracking>,
    staged: Mutex<Option<Staged>>,
    guards: Mutex<Vec<CancelListener>>,
    owner_shutdown_pending: AtomicBool,
    owner_closed: AtomicBool,
}

impl Run {
    /// Immediate, nonblocking logical invalidation: callbacks stop matching,
    /// every authority ends and the workers wake. No native work, no waiting.
    fn revoke(&self) {
        self.live.store(false, Ordering::SeqCst);
        self.cell.update(|published| published.permission = false);
        self.token.cancel();
        self.admission.wake();
        let (current, control) = {
            let tracking = lock(&self.tracking);
            let current = tracking.current.as_ref().map(|live| {
                live.authority.revoke();
                live.id
            });
            drop(tracking);
            (current, lock(&self.resources).control.clone())
        };
        if let (Some(id), Some(control)) = (current, control) {
            control.invalidate(id);
        }
    }

    fn control(&self) -> Option<Arc<MenuControl>> { lock(&self.resources).control.clone() }

    fn observation(&self) -> Option<Arc<ApplicationObservation>> {
        lock(&self.resources).observation.clone()
    }
}

impl Inner {
    fn forget(&self, run: &Arc<Run>) {
        let mut slot = lock(&self.run);
        if slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, run)) {
            *slot = None;
            drop(slot);
            lock(&self.degraded).take();
            lock(&self.reports).clear();
        }
    }

    /// Stops the application control's own cleanup when no run holds it.
    fn stop_applications(&self) -> bool {
        if lock(&self.run).is_some() {
            return true;
        }
        self.parts.applications.stop().is_ok()
    }

    /// Reverse rollback and stop in one idempotent routine. Returns what is
    /// still owed; a retained owner stays in `run` for the retry.
    #[allow(clippy::too_many_lines)] // One ordered teardown with an owed entry per step.
    fn teardown(&self, run: &Run) -> Vec<String> {
        let mut owed = Vec::new();
        // 1. Logical invalidation first: nothing can start, show or issue.
        run.revoke();
        // 2. Drop the subscription. The shared service keeps its tap for a
        // tombstone of the click this run already consumed.
        let subscription = lock(&run.subscription).take();
        if let Some(subscription) = subscription {
            let report = subscription.unsubscribe();
            if let stache_sdk::input::NativeCleanup::Retained(retained) = report.native {
                // The shared input service owns the retained, inert native
                // cleanup and retries it; this module has no handle left to
                // retry with. Report it as pending instead of as released, in
                // the health detail and the pending-cleanup counter.
                let detail = format!(
                    "input tap removal is not acknowledged; the shared input service retains \
                     its inert cleanup (cleanupPending): {retained:?}"
                );
                run.logger.warn(detail.clone());
                self.counters.bump(Outcome::CleanupPending);
                *lock(&self.cleanup_pending) = Some(detail);
            }
        }
        let control = run.control();
        // 3. Cancel exact tracking, then release every known presentation with a
        // fresh acknowledgement window (the run token is already cancelled).
        if let Some(control) = control.as_ref() {
            let mut ids: Vec<PresentationId> = Vec::new();
            {
                let tracking = lock(&run.tracking);
                ids.extend(tracking.current.as_ref().map(|live| live.id));
                ids.extend(tracking.pending.iter().copied());
            }
            for id in &ids {
                match control.cancel(*id) {
                    CancelOutcome::Pending => {
                        owed.push("tracking cancellation was not acknowledged".to_owned());
                    }
                    CancelOutcome::DispatchFailure => {
                        owed.push("tracking cancellation could not be dispatched".to_owned());
                    }
                    CancelOutcome::Acknowledged
                    | CancelOutcome::NotTracking
                    | CancelOutcome::Unknown => {}
                }
            }
            // Once the owner thread was asked to shut down, its exit released
            // every retained element, so a later AX release has nothing to do.
            let owner_gone = run.owner_closed.load(Ordering::SeqCst)
                || run.owner_shutdown_pending.load(Ordering::SeqCst);
            for id in &ids {
                match control.release(*id) {
                    CleanupOutcome::Pending {
                        error: CleanupError::AxDispatch,
                        ..
                    } if owner_gone => {
                        lock(&run.tracking).pending.retain(|pending| pending != id);
                    }
                    CleanupOutcome::Pending { error, .. } => {
                        self.counters.bump(Outcome::CleanupPending);
                        owed.push(format!("presentation cleanup is pending ({error:?})"));
                        let mut tracking = lock(&run.tracking);
                        if !tracking.pending.contains(id) {
                            tracking.pending.push(*id);
                        }
                    }
                    CleanupOutcome::Complete { .. }
                    | CleanupOutcome::LeasedPending { .. }
                    | CleanupOutcome::Unknown => {
                        lock(&run.tracking).pending.retain(|pending| pending != id);
                    }
                }
            }
            // 4. The AX owner: join its thread.
            if !run.owner_closed.load(Ordering::SeqCst) {
                match control.owner().shutdown_within(self.parts.acknowledgement) {
                    MenuReleaseOutcome::Released { .. }
                    | MenuReleaseOutcome::Unknown
                    | MenuReleaseOutcome::Deferred { .. } => {
                        run.owner_closed.store(true, Ordering::SeqCst);
                    }
                    MenuReleaseOutcome::Pending => {
                        run.owner_shutdown_pending.store(true, Ordering::SeqCst);
                        owed.push("the AX owner did not acknowledge shutdown".to_owned());
                    }
                    MenuReleaseOutcome::DispatchFailure => {
                        // A previous pending shutdown that has since run leaves
                        // the owner closed; anything else is a real failure.
                        if run.owner_shutdown_pending.load(Ordering::SeqCst) {
                            run.owner_closed.store(true, Ordering::SeqCst);
                        } else {
                            owed.push("the AX owner could not be asked to shut down".to_owned());
                        }
                    }
                }
            }
        }
        // 5. Stop the application observation.
        if let Some(observation) = run.observation() {
            match observation.stop() {
                Ok(()) => lock(&run.resources).observation = None,
                Err(failure) => owed.push(format!(
                    "the application observation could not be stopped: {failure}"
                )),
            }
        }
        if owed.is_empty() {
            lock(&run.resources).control = None;
        }
        owed
    }
}

/// `MenuAnywhere` as a registry module.
pub struct MenuAnywhereModule {
    inner: Arc<Inner>,
    config: Option<MenuAnywhereConfig>,
    run: Option<Arc<Run>>,
}

impl MenuAnywhereModule {
    /// Rolls a failed start back and builds its typed error.
    fn fail_start(
        &mut self,
        run: &Arc<Run>,
        category: ErrorCategory,
        reason: String,
    ) -> ModuleError {
        let owed = self.inner.teardown(run);
        if owed.is_empty() {
            self.run = None;
            self.inner.forget(run);
        }
        let message = if owed.is_empty() {
            reason
        } else {
            format!("{reason}; cleanup also pending: {}", owed.join("; "))
        };
        run.logger.warn(message.clone());
        error(Operation::Start, category, message)
    }
}

impl Module for MenuAnywhereModule {
    type Config = MenuAnywhereConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: MenuAnywhereConfig::ID,
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
            observes: &[],
            reserves_screen: false,
        }
    }

    /// Accessibility, as a shared-input consumer: startup, enable, reload,
    /// tick and activation only check; only an explicit tray request prompts.
    fn required_permissions(&self, config: &MenuAnywhereConfig) -> Vec<PermissionRequirement> {
        if config.enabled {
            vec![PermissionRequirement::shared_input(
                Permission::Accessibility,
            )]
        } else {
            Vec::new()
        }
    }

    fn configure(&mut self, config: &MenuAnywhereConfig, _ctx: &ModuleContext) -> ModuleResult<()> {
        self.config = Some(config.clone());
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // One ordered acquisition with a rollback at every step.
    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        let inner = Arc::clone(&self.inner);
        if let Some(previous) = self.run.clone().or_else(|| lock(&inner.run).clone()) {
            // A failed stop left a run behind; resolve it before a successor.
            let owed = inner.teardown(&previous);
            if !owed.is_empty() {
                self.run = Some(previous);
                return Err(error(
                    Operation::Start,
                    ErrorCategory::Platform,
                    format!("earlier cleanup is still pending: {}", owed.join("; ")),
                ));
            }
            self.run = None;
            inner.forget(&previous);
        }
        inner.service_missing.store(false, Ordering::SeqCst);
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
        if !ctx.input().is_available() {
            inner.service_missing.store(true, Ordering::SeqCst);
            return Err(error(
                Operation::Start,
                ErrorCategory::Dependency,
                "the shared input service is unavailable",
            ));
        }
        let parts = &inner.parts;
        let matcher = Matcher::from_config(&config);
        if let Err(failure) = Matcher::prefilter().validate() {
            return Err(error(
                Operation::Start,
                ErrorCategory::Config,
                format!("the trigger filter is invalid: {failure}"),
            ));
        }

        // 1. Generation, authority root, published tuple and the one-slot queue.
        let generation = inner.next_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let token = CancellationToken::new();
        let (sender, receiver) = mpsc::sync_channel::<Message>(1);
        let admission = Arc::new(Admission::new(sender, Arc::clone(&inner.counters)));
        // Ineligible until the subscription is acknowledged.
        let cell = Arc::new(PublishedCell::new(Published {
            generation,
            revision: pair.revision,
            matcher,
            permission: false,
            service: false,
        }));
        let run = Arc::new(Run {
            generation,
            token: token.clone(),
            live: Arc::new(AtomicBool::new(true)),
            cell: Arc::clone(&cell),
            admission: Arc::clone(&admission),
            logger: ctx.logger().clone(),
            resources: Mutex::new(Resources::default()),
            subscription: Mutex::new(None),
            tracking: Mutex::new(Tracking::default()),
            staged: Mutex::new(None),
            guards: Mutex::new(Vec::new()),
            owner_shutdown_pending: AtomicBool::new(false),
            owner_closed: AtomicBool::new(false),
        });
        {
            // Weak: the invalidation registry never prunes listeners, so a
            // strong handle would keep every past run alive for the process
            // lifetime across pause/resume cycles.
            let on_cancel = Arc::downgrade(&run);
            lock(&run.guards).push(ctx.cancellation().on_cancel(Arc::new(move || {
                if let Some(run) = on_cancel.upgrade() {
                    run.token.cancel();
                    run.admission.wake();
                }
            })));
            let on_invalidate = Arc::downgrade(&run);
            ctx.invalidation().on_invalidate(Arc::new(move || {
                if let Some(run) = on_invalidate.upgrade() {
                    run.revoke();
                }
            }));
        }
        lock(&inner.degraded).take();
        lock(&inner.reports).clear();
        *lock(&inner.run) = Some(Arc::clone(&run));
        self.run = Some(Arc::clone(&run));

        // 2. Application observation, the frontmost watch and the accepted
        // action token. The token is taken once: a filter change never calls
        // `accept`, so it can never revoke an accepted live popup.
        let observation = match parts.applications.install(&token, |_| {}) {
            Ok(observation) => Arc::new(observation),
            Err(failure) => {
                // The failed install already invalidated its generation and the
                // control keeps the exact cleanup owned; acknowledge it here
                // because no observation handle exists to stop.
                let cleanup = match parts.applications.stop() {
                    Ok(()) => String::new(),
                    Err(cleanup) => format!("; observation cleanup also failed: {cleanup}"),
                };
                return Err(self.fail_start(
                    &run,
                    ErrorCategory::Platform,
                    format!("failed to install the application observer: {failure}{cleanup}"),
                ));
            }
        };
        lock(&run.resources).observation = Some(Arc::clone(&observation));
        let watch = match observation.watch_frontmost(&token) {
            Ok(watch) => watch,
            Err(failure) => {
                return Err(self.fail_start(
                    &run,
                    ErrorCategory::Platform,
                    format!("failed to watch the frontmost application: {failure}"),
                ));
            }
        };
        let accepted_action = match observation.accept() {
            Ok(accepted_action) => accepted_action,
            Err(failure) => {
                return Err(self.fail_start(
                    &run,
                    ErrorCategory::Platform,
                    format!("failed to accept the action revision: {failure}"),
                ));
            }
        };

        // 3. The menu stack (the AX owner thread). Nothing is read here.
        let validator = Arc::new(RunValidator {
            observation: Arc::clone(&observation),
            watch: watch.clone(),
            capture: Mutex::new(None),
        });
        let control = match parts.menu.open(Arc::clone(&validator) as Arc<dyn TargetValidator>) {
            Ok(control) => Arc::new(control),
            Err(failure) => {
                return Err(self.fail_start(
                    &run,
                    ErrorCategory::Platform,
                    format!("failed to open the menu stack: {failure}"),
                ));
            }
        };
        lock(&run.resources).control = Some(Arc::clone(&control));
        let Some(events) = control.take_events() else {
            return Err(self.fail_start(
                &run,
                ErrorCategory::Platform,
                "the menu event stream was already taken".to_owned(),
            ));
        };

        // 4. The workers.
        let clock = parts.menu.clock();
        let worker = Worker {
            inner: Arc::clone(&inner),
            run: Arc::clone(&run),
            queue: receiver,
            events,
            control,
            observation,
            validator,
            accepted_action,
            watch,
            clock: Arc::clone(&clock),
        };
        if let Err(failure) = ctx.spawn_worker("menu-anywhere", move |group| worker.run(&group)) {
            return Err(self.fail_start(
                &run,
                ErrorCategory::Platform,
                format!("failed to spawn the MenuAnywhere worker: {failure}"),
            ));
        }
        let config_worker = ConfigWorker {
            inner: Arc::clone(&inner),
            run: Arc::clone(&run),
            accepted,
            seen: pair.revision,
        };
        if let Err(failure) =
            ctx.spawn_worker("menu-anywhere-config", move |group| config_worker.run(&group))
        {
            return Err(self.fail_start(
                &run,
                ErrorCategory::Platform,
                format!("failed to spawn the MenuAnywhere config worker: {failure}"),
            ));
        }

        // 5. Subscribe the handler last, once everything it needs is live.
        let handler = Arc::new(TriggerHandler::new(
            Arc::clone(&cell),
            admission,
            generation,
            clock,
            Arc::clone(&inner.counters),
        ));
        let spec = HandlerSpec {
            filter: Matcher::prefilter(),
            priority: parts.priority,
            work: WorkRequirement::None,
        };
        match ctx.input().subscribe_handler(spec, handler) {
            Ok(subscription) => {
                *lock(&run.subscription) = Some(subscription);
                // A fresh acknowledged subscription supersedes an earlier
                // unacknowledged removal.
                lock(&inner.cleanup_pending).take();
            }
            Err(failure) => {
                if failure == InputError::Unavailable {
                    inner.service_missing.store(true, Ordering::SeqCst);
                }
                let category = match failure {
                    InputError::Invalidated | InputError::Cancelled => ErrorCategory::Lifecycle,
                    InputError::Unavailable => ErrorCategory::Dependency,
                    _ => ErrorCategory::Platform,
                };
                return Err(self.fail_start(
                    &run,
                    category,
                    format!("failed to subscribe to the shared input service: {failure}"),
                ));
            }
        }
        inner.reach(Stage::Installed);
        if ctx.cancellation().is_cancelled() || !run.live.load(Ordering::SeqCst) {
            return Err(self.fail_start(
                &run,
                ErrorCategory::Lifecycle,
                "start was cancelled while the runtime was starting".to_owned(),
            ));
        }
        // Eligible only now: acknowledged resources, granted permission (the
        // registry started the module) and a healthy service.
        cell.update(|published| {
            published.permission = true;
            published.service = true;
        });
        ctx.logger().debug(format!("MenuAnywhere generation {generation} started"));
        Ok(())
    }

    fn reconfigure(
        &mut self,
        config: &MenuAnywhereConfig,
        ctx: &ModuleContext,
    ) -> ModuleResult<()> {
        if self.config.as_ref() == Some(config) {
            return Ok(());
        }
        if let Some(run) = self.run.clone() {
            // Validate and stage only. Nothing about the live filter, tuple or
            // any in-flight presentation changes before the registry publishes
            // the revision and the config worker observes it.
            let matcher = Matcher::from_config(config);
            *lock(&run.staged) = Some(Staged {
                config: config.clone(),
                matcher,
            });
            ctx.logger().debug(format!(
                "MenuAnywhere generation {} staged a new configuration",
                run.generation
            ));
        }
        // A paused or stopped module only stages the next-start snapshot.
        self.config = Some(config.clone());
        Ok(())
    }

    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        let Some(run) = self.run.clone().or_else(|| lock(&self.inner.run).clone()) else {
            return Ok(());
        };
        let owed = self.inner.teardown(&run);
        if owed.is_empty() {
            self.run = None;
            self.inner.forget(&run);
            ctx.logger()
                .debug(format!("MenuAnywhere generation {} stopped", run.generation));
            Ok(())
        } else {
            self.run = Some(run);
            let message = owed.join("; ");
            ctx.logger().error(message.clone());
            Err(error(Operation::Stop, ErrorCategory::Platform, message))
        }
    }
}

impl Drop for MenuAnywhereModule {
    /// Best-effort inertness when the module is dropped without a lifecycle
    /// stop. The registry always stops it first.
    fn drop(&mut self) {
        if let Some(run) = self.run.take() {
            run.revoke();
        }
    }
}

// ---------------------------------------------------------------------------
// Config worker
// ---------------------------------------------------------------------------

struct ConfigWorker {
    inner: Arc<Inner>,
    run: Arc<Run>,
    accepted: AcceptedWatch<MenuAnywhereConfig>,
    seen: u64,
}

impl ConfigWorker {
    fn run(mut self, group: &CancellationToken) {
        let token = self.run.token.clone();
        let admission = Arc::clone(&self.run.admission);
        let _group = group.on_cancel({
            let token = token.clone();
            Arc::new(move || {
                token.cancel();
                admission.wake();
            })
        });
        loop {
            match self
                .accepted
                .wait_for_change(self.seen, self.inner.parts.health_interval, &token)
            {
                AcceptedWait::Changed(pair) => {
                    self.seen = pair.revision;
                    self.adopt(&pair);
                }
                AcceptedWait::TimedOut => self.check_health(),
                AcceptedWait::Cancelled | AcceptedWait::Closed => break,
            }
            if !self.run.live.load(Ordering::SeqCst) {
                break;
            }
        }
    }

    /// Applies one observed accepted pair by publishing the one coherent
    /// matcher-and-revision tuple. The shared-service prefilter never changes,
    /// so a callback before the publish compares against the previous complete
    /// tuple.
    fn adopt(&self, pair: &AcceptedConfig<MenuAnywhereConfig>) {
        let run = &self.run;
        if !pair.config.enabled {
            // A disabling reload publishes before the registry stops the
            // module. Eligibility ends the moment it is observed.
            run.cell.update(|published| {
                published.permission = false;
                published.revision = pair.revision;
            });
            return;
        }
        let matcher = lock(&run.staged)
            .take()
            .filter(|staged| staged.config == *pair.config)
            .map_or_else(|| Matcher::from_config(&pair.config), |staged| staged.matcher);
        self.inner.reach(Stage::AdoptBeforePublish);
        run.cell.update(|published| {
            published.revision = pair.revision;
            published.matcher = matcher;
        });
        self.inner.reach(Stage::AdoptPublished);
    }

    /// Withdraws eligibility when the subscription went inert or the worker
    /// queue disconnected. Neither rewrites the registry lifecycle.
    fn check_health(&self) {
        let run = &self.run;
        let inert = lock(&run.subscription).as_ref().and_then(Subscription::inert_reason);
        let detail = inert
            .map(|reason| format!("the input subscription became inert: {reason:?}"))
            .or_else(|| {
                (!run.admission.worker_available())
                    .then(|| "the MenuAnywhere worker is unavailable".to_owned())
            });
        if let Some(detail) = detail {
            run.cell.update(|published| published.service = false);
            if lock(&self.inner.degraded).is_none() {
                run.logger.warn(detail.clone());
            }
            self.inner.degrade(run.generation, detail);
        }
    }
}

// ---------------------------------------------------------------------------
// Trigger worker
// ---------------------------------------------------------------------------

/// Frees a busy reservation exactly once, by its own token, on every exit path.
struct Reservation {
    admission: Arc<Admission>,
    token: u64,
}

impl Drop for Reservation {
    fn drop(&mut self) { self.admission.release(self.token); }
}

struct Worker {
    inner: Arc<Inner>,
    run: Arc<Run>,
    queue: Receiver<Message>,
    events: Receiver<MenuEvent>,
    control: Arc<MenuControl>,
    observation: Arc<ApplicationObservation>,
    validator: Arc<RunValidator>,
    accepted_action: AcceptedActionToken,
    watch: FrontmostWatch,
    clock: Arc<dyn MenuClock>,
}

/// One pass over one request.
struct Pass {
    report: TriggerReport,
    authority: Arc<Authority>,
}

impl Worker {
    fn run(self, group: &CancellationToken) {
        let token = self.run.token.clone();
        let admission = Arc::clone(&self.run.admission);
        let _group = group.on_cancel(Arc::new(move || {
            token.cancel();
            admission.wake();
        }));
        loop {
            match self.queue.recv() {
                Ok(Message::Request(request)) => {
                    let _reservation = Reservation {
                        admission: Arc::clone(&self.run.admission),
                        token: request.reservation,
                    };
                    self.handle(request);
                }
                Ok(Message::Stop) => {}
                Err(_) => break,
            }
            if !self.run.live.load(Ordering::SeqCst) || self.run.token.is_cancelled() {
                break;
            }
        }
        // The queue receiver drops here, so a later callback sees a
        // disconnected worker and passes.
    }

    fn count(&self, outcome: Outcome) -> Outcome {
        self.inner.counters.bump(outcome);
        if matches!(
            outcome,
            Outcome::ReadTimeout
                | Outcome::ReadFailed
                | Outcome::ReadPermissionDenied
                | Outcome::CaptureFailed
                | Outcome::CaptureSlow
                | Outcome::ShowTimeout
                | Outcome::ShowRejected
                | Outcome::ActionUncertain
                | Outcome::ActionFailed
                | Outcome::ActionRefused
                | Outcome::CleanupPending
        ) {
            self.run.logger.warn(format!("MenuAnywhere: {}", outcome.label()));
        }
        outcome
    }

    fn handle(&self, request: Request) {
        let authority = Arc::new(Authority {
            live: Arc::clone(&self.run.live),
            revoked: AtomicBool::new(false),
        });
        let mut pass = Pass {
            report: TriggerReport {
                generation: request.generation,
                revision: request.revision,
                triggered_at: request.triggered_at,
                capture_started_at: None,
                captured_at: None,
                target: None,
                outcome: Outcome::Admitted,
                action: None,
            },
            authority,
        };
        self.inner.reach(Stage::Admitted);
        pass.report.outcome = self.process(&request, &mut pass);
        pass.authority.revoke();
        self.validator.set(None);
        self.inner.report(pass.report);
    }

    fn revoked(&self, pass: &Pass) -> bool {
        !pass.authority.is_current() || self.run.token.is_cancelled()
    }

    /// Runs the pipeline and returns its last typed outcome.
    #[allow(clippy::too_many_lines)] // One linear pipeline; each stage is an early exit.
    fn process(&self, request: &Request, pass: &mut Pass) -> Outcome {
        self.retry_pending();
        if self.revoked(pass) {
            return self.count(Outcome::Revoked);
        }
        if self.clock.now().saturating_duration_since(request.triggered_at) > CAPTURE_WINDOW {
            return self.count(Outcome::RequestStale);
        }

        // Identity capture, as early as the worker can after admission.
        self.inner.reach(Stage::BeforeCapture);
        let started = self.clock.now();
        pass.report.capture_started_at = Some(started);
        let snapshot = self.watch.refresh();
        let captured = self.clock.now();
        pass.report.captured_at = Some(captured);
        let Ok(snapshot) = snapshot else {
            return self.count(Outcome::CaptureFailed);
        };
        if captured.saturating_duration_since(started) > CAPTURE_WINDOW {
            return self.count(Outcome::CaptureSlow);
        }
        self.inner.reach(Stage::Captured);
        let (target, epoch) = match Self::target_of(&snapshot) {
            Ok(found) => found,
            Err(outcome) => return self.count(outcome),
        };
        pass.report.target = Some(target.application);
        let Some(anchor) = request.anchor else {
            return self.count(Outcome::PointerUnavailable);
        };
        if self.revoked(pass) {
            return self.count(Outcome::Revoked);
        }

        // Read.
        let presentation = PresentationId::allocate();
        self.validator.set(Some(Capture {
            application: target.application,
            epoch,
        }));
        lock(&self.run.tracking).current = Some(Live {
            id: presentation,
            authority: Arc::clone(&pass.authority),
        });
        self.inner.reach(Stage::BeforeRead);
        let outcome = self.read(target, presentation, &pass.authority);
        self.inner.reach(Stage::Read);
        let snapshot = match outcome {
            MenuReadOutcome::Snapshot(snapshot) => {
                self.count(Outcome::Read);
                snapshot
            }
            other => {
                let outcome = self.count(Self::read_outcome(&other));
                self.finish(presentation, false);
                return outcome;
            }
        };

        // Show.
        if self.revoked(pass) {
            self.finish(presentation, false);
            return self.count(Outcome::Revoked);
        }
        self.inner.reach(Stage::BeforeShow);
        // The last check before the show, immediately before the show call.
        if self.revoked(pass) {
            self.finish(presentation, false);
            return self.count(Outcome::Revoked);
        }
        if self.validator.check(target.application) != TargetCheck::Current {
            self.finish(presentation, false);
            return self.count(Outcome::StaleTarget);
        }
        let shown = self.control.show(ShowRequest {
            snapshot,
            anchor,
            expected_geometry: None,
            target,
            accepted: self.accepted_action,
            authority: Arc::clone(&pass.authority) as Arc<dyn PresentationAuthority>,
            deadline: self.clock.now() + self.inner.parts.acknowledgement,
        });
        let MenuPresentationOutcome::Shown { .. } = shown else {
            let outcome = self.count(Self::show_outcome(&shown));
            self.finish(presentation, true);
            return outcome;
        };
        let mut outcome = self.count(Outcome::Shown);
        self.inner.reach(Stage::Shown);

        // Track until the menu closes or a selection is invoked.
        outcome = self.track(presentation, pass, outcome);

        self.inner.reach(Stage::Cleanup);
        self.finish(presentation, true);
        outcome
    }

    /// The exact captured instance of a confirmed, non-self frontmost snapshot.
    const fn target_of(snapshot: &FrontmostSnapshot) -> Result<(MenuTarget, u64), Outcome> {
        if !snapshot.is_confirmed() {
            return Err(match snapshot.state {
                FrontmostState::Unconfirmed(stache_macos::Unconfirmed::NoFrontmost) => {
                    Outcome::NoFrontmost
                }
                _ => Outcome::CaptureFailed,
            });
        }
        if snapshot.is_current_process {
            return Err(Outcome::SelfTarget);
        }
        let (Some(application), Some(pid)) = (snapshot.application, snapshot.pid) else {
            return Err(Outcome::NoFrontmost);
        };
        Ok((MenuTarget { pid, application }, snapshot.epoch))
    }

    /// Submits the read to the AX owner thread and waits in quanta, observing
    /// revocation. A revoked read is cancelled and then given one fresh
    /// acknowledgement window before the wait is abandoned (the owner thread
    /// finishes the in-flight AX call under its own timeout).
    fn read(
        &self,
        target: MenuTarget,
        presentation: PresentationId,
        authority: &Arc<Authority>,
    ) -> MenuReadOutcome {
        let (reply, answer) = mpsc::channel();
        let cancel = CancellationToken::new();
        let submitted = self.control.owner().submit_read(
            ReadRequest {
                target: Some(target),
                presentation,
                cancel: cancel.clone(),
                deadline: None,
            },
            move |outcome| {
                let _ = reply.send(outcome);
            },
        );
        if submitted.is_err() {
            return MenuReadOutcome::DispatchFailure;
        }
        let mut revoked_at: Option<Instant> = None;
        loop {
            match answer.recv_timeout(CANCELLATION_QUANTUM) {
                Ok(outcome) => return outcome,
                Err(RecvTimeoutError::Disconnected) => return MenuReadOutcome::DispatchFailure,
                Err(RecvTimeoutError::Timeout) => {
                    if !authority.is_current() || self.run.token.is_cancelled() {
                        cancel.cancel();
                        let since = *revoked_at.get_or_insert_with(Instant::now);
                        if since.elapsed() >= self.inner.parts.acknowledgement {
                            let mut tracking = lock(&self.run.tracking);
                            if !tracking.pending.contains(&presentation) {
                                tracking.pending.push(presentation);
                            }
                            drop(tracking);
                            return MenuReadOutcome::Cancelled;
                        }
                    }
                }
            }
        }
    }

    /// Waits for tracking to end. A selection is invoked against the captured
    /// instance. Revocation cancels exact tracking.
    fn track(
        &self,
        presentation: PresentationId,
        pass: &mut Pass,
        mut outcome: Outcome,
    ) -> Outcome {
        loop {
            if !pass.authority.is_current() {
                let _ = self.control.cancel(presentation);
                return outcome;
            }
            match self.events.recv_timeout(CANCELLATION_QUANTUM) {
                Ok(MenuEvent::Selected(selection))
                    if selection.token().presentation() == presentation =>
                {
                    self.count(Outcome::Selected);
                    self.inner.reach(Stage::BeforeAction);
                    let report = self.control.invoke(
                        &selection,
                        &self.observation,
                        self.clock.now() + ACTION_BUDGET,
                    );
                    pass.report.action = Some(report);
                    outcome = self.count(Self::action_outcome(report.outcome));
                    return outcome;
                }
                Ok(MenuEvent::Closed(id)) if id == presentation => return outcome,
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return outcome,
            }
        }
    }

    /// Releases a presentation with a fresh acknowledgement window. A pending
    /// result is retained and retried; it never frees a successor's state.
    fn finish(&self, presentation: PresentationId, shown: bool) {
        let complete = if shown {
            match self.control.release(presentation) {
                CleanupOutcome::Complete { .. } | CleanupOutcome::LeasedPending { .. } => true,
                CleanupOutcome::Unknown => self.release_owner(presentation),
                CleanupOutcome::Pending { .. } => false,
            }
        } else {
            self.release_owner(presentation)
        };
        {
            let mut tracking = lock(&self.run.tracking);
            if tracking.current.as_ref().is_some_and(|live| live.id == presentation) {
                tracking.current = None;
            }
            if !complete && !tracking.pending.contains(&presentation) {
                tracking.pending.push(presentation);
            }
        }
        self.count(if complete {
            Outcome::CleanupComplete
        } else {
            Outcome::CleanupPending
        });
    }

    fn release_owner(&self, presentation: PresentationId) -> bool {
        match self
            .control
            .owner()
            .release_within(presentation, self.inner.parts.acknowledgement)
        {
            MenuReleaseOutcome::Released { .. }
            | MenuReleaseOutcome::Unknown
            | MenuReleaseOutcome::Deferred { .. } => true,
            // A stopped run shut the owner down, which released everything.
            MenuReleaseOutcome::DispatchFailure => !self.run.live.load(Ordering::SeqCst),
            MenuReleaseOutcome::Pending => false,
        }
    }

    /// Retries presentations an earlier pass could not clean up.
    fn retry_pending(&self) {
        let pending: Vec<PresentationId> = lock(&self.run.tracking).pending.clone();
        for id in pending {
            let done = match self.control.release(id) {
                CleanupOutcome::Complete { .. }
                | CleanupOutcome::LeasedPending { .. }
                | CleanupOutcome::Unknown => self.release_owner(id),
                CleanupOutcome::Pending { .. } => false,
            };
            if done {
                lock(&self.run.tracking).pending.retain(|pending| *pending != id);
            }
        }
    }

    const fn read_outcome(outcome: &MenuReadOutcome) -> Outcome {
        match outcome {
            MenuReadOutcome::Snapshot(_) => Outcome::Read,
            MenuReadOutcome::NoMenu => Outcome::NoMenu,
            MenuReadOutcome::PermissionDenied => Outcome::ReadPermissionDenied,
            MenuReadOutcome::DeadlineExceeded
            | MenuReadOutcome::ElementTimeout
            | MenuReadOutcome::TimeoutSetFailed => Outcome::ReadTimeout,
            MenuReadOutcome::Cancelled => Outcome::ReadCancelled,
            MenuReadOutcome::SelfTarget => Outcome::SelfTarget,
            MenuReadOutcome::NoFrontmost => Outcome::NoFrontmost,
            MenuReadOutcome::StaleTarget => Outcome::StaleTarget,
            MenuReadOutcome::Unsupported
            | MenuReadOutcome::Malformed
            | MenuReadOutcome::NodeLimit
            | MenuReadOutcome::DepthLimit
            | MenuReadOutcome::Cycle
            | MenuReadOutcome::Transport
            | MenuReadOutcome::DispatchFailure
            | MenuReadOutcome::OwnerPanic => Outcome::ReadFailed,
        }
    }

    const fn show_outcome(outcome: &MenuPresentationOutcome) -> Outcome {
        match outcome {
            MenuPresentationOutcome::Shown { .. } => Outcome::Shown,
            MenuPresentationOutcome::Stale | MenuPresentationOutcome::Cancelled => {
                Outcome::ShowStale
            }
            MenuPresentationOutcome::Expired
            | MenuPresentationOutcome::AckTimeout
            | MenuPresentationOutcome::DispatchFailure => Outcome::ShowTimeout,
            MenuPresentationOutcome::GeometryRejected(_)
            | MenuPresentationOutcome::PresenterFailure(_)
            | MenuPresentationOutcome::Busy => Outcome::ShowRejected,
        }
    }

    const fn action_outcome(outcome: MenuInvocationOutcome) -> Outcome {
        match outcome {
            MenuInvocationOutcome::Pressed => Outcome::ActionPressed,
            MenuInvocationOutcome::Stale
            | MenuInvocationOutcome::Cancelled
            | MenuInvocationOutcome::ExpiredBeforeSend
            | MenuInvocationOutcome::DispatchFailure => Outcome::ActionStale,
            MenuInvocationOutcome::ActivationRefused | MenuInvocationOutcome::Unsupported(_) => {
                Outcome::ActionRefused
            }
            MenuInvocationOutcome::Uncertain(_) => Outcome::ActionUncertain,
            MenuInvocationOutcome::PermissionDenied
            | MenuInvocationOutcome::TimeoutSetFailed(_)
            | MenuInvocationOutcome::Failed(_) => Outcome::ActionFailed,
        }
    }
}
