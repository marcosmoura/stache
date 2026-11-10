//! The Tiling [`Module`]: lifecycle, staged start with exact rollback,
//! observed-revision reconfigure and quarantined stop.
//!
//! Construction is inert. The factory owns a host-lifetime [`TilingRuntime`]
//! service (backend, application directory, display projection); the registry
//! owns the [`TilingModule`] it creates. Nothing native is installed until an
//! enabled module starts. The production composition registers the factory
//! default disabled.
//!
//! # Start stages (each unwound exactly, in reverse, on any failure)
//!
//! 1. backend epoch install
//! 2. effect runner thread
//! 3. actor thread
//! 4. host reservation watcher (primed: its first snapshot reached the actor)
//! 5. native display feed (observer and worker; the initial layout reached the actor)
//! 6. configuration coordinator worker
//! 7. initial enumeration queued, accepted configuration submitted
//! 8. action authority published for the accepted revision, then Running
//! 9. the passive drag observer on the shared listen-only tap (optional: a
//!    refusal degrades drag only, with a status diagnostic)
//!
//! The borders runner is not a numbered stage: it is spawned only while the
//! accepted configuration has `borders.enabled` (at stage 7 and live on every
//! accepted revision), owns its connection and helper child, and a failure to
//! start or deliver degrades borders only.
//!
//! The reservation watcher and the display feed are primed before the authority
//! is published, so reservations and native topology are consumed before the
//! first frame effect. `start` never waits
//! for a window to move.
//!
//! # Stop and quarantine
//!
//! Stop seals the authority immediately (one atomic store), publishes the
//! stopping status, then wakes every wait and runs the owed stages in order with
//! fresh finite waits: remove the drag observer, close the borders runner (it sends nothing; the service keeps its last settings), drain frame issuance, close and join the animation worker, the restore of runtime-hidden applications, the
//! reservation watcher, the display feed, the backend epoch, the actor and the effect worker.
//! A stage that does not finish stays *owed*. The exact resources are retained
//! in a quarantine; `start` refuses while any stage is owed and a later `stop`
//! re-runs only the owed stages. A blocked join is never detached.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

use stache_macos::DisplayControl;
use stache_macos::windows::StopOutcome;
use stache_sdk::{
    AcceptedWait, AcceptedWatch, CancelListener, CancellationToken, ErrorCategory, Module,
    ModuleContext, ModuleDescriptor, ModuleError, ModuleResult, Operation, Permission,
    PermissionRequirement,
};

use crate::actor::{AppDirectory, Clock, SystemClock};
use crate::backend::WindowBackend;
use crate::borders::{BordersParts, FaultSink, JOIN_BOUND};
use crate::config::TilingConfig;
use crate::displays::DisplayFeed;
use crate::drag::DragFeed;
use crate::reservations::{DisplayProjection, ReservationSource, ReservationWatcher};
use crate::status::{AppLabel, TilingStatusSource};
use crate::tracking::{HOP, StageProbe, StartError, StartStage, TrackingOptions, TrackingRuntime};
use crate::visibility::{ActionsDirectory, ApplicationActionsSource, OwnedApp};

/// A revision observer (tests hold the coordinator with it).
pub type RevisionHook = Arc<dyn Fn(u64) + Send + Sync>;
/// The fallible pure staging seam of `reconfigure`.
pub type StageHook = Arc<dyn Fn(&TilingConfig) -> Result<(), String> + Send + Sync>;
/// Observes each teardown stage as it runs.
pub type TeardownTrace = Arc<dyn Fn(Teardown) + Send + Sync>;

/// One teardown step, in execution order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Teardown {
    /// The authority was sealed and every wait was woken.
    Sealed,
    /// The drag observer was removed.
    Drag,
    /// The borders runner was closed and joined (only traced when one existed).
    Borders,
    /// Frame issuance was drained.
    Drained,
    /// The animation worker was closed and joined: no animated frame can follow.
    Animation,
    /// The open `AXEnhancedUserInterface` leases were restored.
    EnhancedUi,
    /// The runtime-hidden applications were restored.
    Restore,
    /// The reservation watcher was stopped.
    Watcher,
    /// The display observer was removed and its worker ended.
    Displays,
    /// The backend epoch was stopped.
    Epoch,
    /// The actor was joined.
    Actor,
    /// The effect worker was joined.
    Runner,
}

/// Wait bounds and test seams.
#[derive(Clone)]
pub struct TilingTiming {
    /// The coordinator's wake period.
    pub poll: Duration,
    /// One finite acknowledgement hop for every cleanup wait.
    pub hop: Duration,
    /// The longest a stop waits for the borders runner (one Mach send, one CLI
    /// child and the cancel/reap window).
    pub borders_join: Duration,
    /// Called with each observed revision before it is adopted.
    pub observed: Option<RevisionHook>,
    /// Fallible staging of a candidate configuration.
    pub stage: Option<StageHook>,
    /// Failpoints probed before each start stage.
    pub probe: Option<StageProbe>,
    /// Teardown order observer.
    pub trace: Option<TeardownTrace>,
}

impl Default for TilingTiming {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(100),
            hop: HOP,
            borders_join: JOIN_BOUND,
            observed: None,
            stage: None,
            probe: None,
            trace: None,
        }
    }
}

/// Everything the host-lifetime service is built from.
pub struct TilingParts {
    /// The owner-confined window backend. Closed at host shutdown.
    pub backend: Arc<dyn WindowBackend>,
    /// Read-only application facts. When `applications` installs actions, the
    /// run answers from that observation instead, so every instance is of the
    /// generation the hide, unhide and activation authorizations accept; this
    /// directory then only backs runs without actions.
    pub directory: Arc<dyn AppDirectory>,
    /// The injected native-to-SDK display identity projection.
    pub projection: DisplayProjection,
    /// Time source for plan deadlines.
    pub clock: Arc<dyn Clock>,
    /// Stache's own bundle identifier (never managed).
    pub self_bundle_id: String,
    /// The reservation source; `None` observes the host through the module
    /// context.
    pub reservations: Option<Arc<dyn ReservationSource>>,
    /// Wait bounds and test seams.
    pub timing: TilingTiming,
    /// Installs the exact-instance application actions (hide, unhide and the
    /// one activation path) for each run. `None` runs without them: nothing is
    /// ever hidden and focus cannot activate an application.
    pub applications: Option<Arc<dyn ApplicationActionsSource>>,
    /// Native display topology. `None` delivers no layout: Tiling then sees no
    /// screens and never guesses one.
    pub displays: Option<DisplayControl>,
    /// The `JankyBorders` authority: how to reach the service and the helper CLI.
    /// `None` composes none, so an enabled `borders.enabled` only reports a
    /// diagnostic and nothing is looked up or sent. Accepted `enabled: true` is
    /// the consent to overwrite the shared settings.
    pub borders: Option<BordersParts>,
}

impl TilingParts {
    /// Parts with the system clock, production waits and context-observed
    /// reservations.
    #[must_use]
    pub fn new(
        backend: Arc<dyn WindowBackend>,
        directory: Arc<dyn AppDirectory>,
        projection: DisplayProjection,
        self_bundle_id: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            directory,
            projection,
            clock: Arc::new(SystemClock),
            self_bundle_id: self_bundle_id.into(),
            reservations: None,
            timing: TilingTiming::default(),
            applications: None,
            displays: None,
            borders: None,
        }
    }
}

struct Inner {
    parts: TilingParts,
    status: TilingStatusSource,
    watch: OnceLock<AcceptedWatch<TilingConfig>>,
    generation: AtomicU64,
    closing: AtomicBool,
    /// Cancelled by [`TilingRuntime::begin_shutdown`]; every IPC handler wait
    /// observes it, so no request blocks the connection joins.
    shutdown: CancellationToken,
    current: Mutex<Weak<TrackingRuntime>>,
    /// Ambiguous visibility records kept past a stop: the ledger cannot tell
    /// our hide from an external one, so it never unhides them on its own.
    ambiguous: Mutex<Vec<Retained>>,
}

/// An ambiguous visibility record kept across stops, with the process id used
/// to prove the instance terminal. Ids are generation-scoped and cannot be
/// compared across runs; the process id is used only to *keep* a record while
/// that process is alive, never to authorize an action.
#[derive(Clone, Debug)]
struct Retained {
    app: OwnedApp,
    pid: Option<i32>,
    /// Display-only identity captured at merge time.
    label: AppLabel,
}

impl Inner {
    /// Drops records whose process is proven gone. An empty or failed query
    /// proves nothing and drops nothing.
    fn prune_retained(kept: &mut Vec<Retained>, live: &[stache_macos::FrontmostApplication]) {
        if live.is_empty() {
            return;
        }
        kept.retain(|record| {
            record.pid.is_none_or(|pid| live.iter().any(|app| app.pid == Some(pid)))
        });
    }

    /// Merges the ambiguous records a stop found into those already retained
    /// (deduplicated by exact application id or live process id), prunes the
    /// proven-terminal ones and republishes them.
    fn merge_ambiguous(&self, runtime: Option<&Arc<TrackingRuntime>>, fresh: Vec<OwnedApp>) {
        let live = runtime.map(|rt| rt.running_applications()).unwrap_or_default();
        let mut kept = lock(&self.ambiguous);
        Self::prune_retained(&mut kept, &live);
        for app in fresh {
            let found = live.iter().find(|candidate| candidate.id == app.application);
            if found.is_none() && !live.is_empty() {
                // The instance is gone: terminal, nothing left to protect.
                kept.retain(|record| record.app.application != app.application);
                continue;
            }
            let pid = found.and_then(|candidate| candidate.pid);
            kept.retain(|record| {
                record.app.application != app.application && (pid.is_none() || record.pid != pid)
            });
            let label = found.map(AppLabel::from_running).unwrap_or_default();
            kept.push(Retained { app, pid, label });
        }
        let apps: Vec<(OwnedApp, AppLabel)> =
            kept.iter().map(|record| (record.app.clone(), record.label.clone())).collect();
        drop(kept);
        self.status.retain_ambiguous(&apps);
    }

    /// At a start: drops retained records whose process ended while stopped.
    fn refresh_retained(&self, runtime: &Arc<TrackingRuntime>) {
        let live = runtime.running_applications();
        let mut kept = lock(&self.ambiguous);
        Self::prune_retained(&mut kept, &live);
        let apps: Vec<(OwnedApp, AppLabel)> =
            kept.iter().map(|record| (record.app.clone(), record.label.clone())).collect();
        drop(kept);
        self.status.retain_ambiguous(&apps);
    }
}

/// The host-lifetime Tiling service. Cloning shares it.
#[derive(Clone)]
pub struct TilingRuntime {
    inner: Arc<Inner>,
}

impl TilingRuntime {
    /// The module identifier.
    pub const ID: stache_sdk::ModuleId = TilingConfig::ID;

    /// Builds the service. Inert: nothing native is created.
    #[must_use]
    pub fn new(parts: TilingParts) -> Self {
        Self {
            inner: Arc::new(Inner {
                parts,
                status: TilingStatusSource::default(),
                watch: OnceLock::new(),
                generation: AtomicU64::new(0),
                closing: AtomicBool::new(false),
                shutdown: CancellationToken::new(),
                current: Mutex::new(Weak::new()),
                ambiguous: Mutex::new(Vec::new()),
            }),
        }
    }

    /// A new module over this service. The registry owns it.
    #[must_use]
    pub fn module(&self) -> TilingModule {
        TilingModule {
            inner: Arc::clone(&self.inner),
            config: None,
            slot: Slot::Idle,
        }
    }

    /// Installs the typed accepted-configuration watch. Call it before the
    /// registry starts modules. A second install is ignored.
    pub fn install_accepted_watch(&self, watch: AcceptedWatch<TilingConfig>) {
        let _ = self.inner.watch.set(watch);
    }

    /// The read-only status source.
    #[must_use]
    pub fn status(&self) -> TilingStatusSource { self.inner.status.clone() }

    /// The running tracking runtime, while Running. Later parts deliver display
    /// layouts and commands through it; it is `None` while stopped, stopping or
    /// quarantined.
    #[must_use]
    pub fn tracking(&self) -> Option<Arc<TrackingRuntime>> { lock(&self.inner.current).upgrade() }

    /// Applications whose hidden state the runtime cannot attribute (a show was
    /// observed while its own hide may have landed). Retained and reported;
    /// never unhidden automatically. Explicit recovery needs approval.
    #[must_use]
    pub fn ambiguous_visibility(&self) -> Vec<OwnedApp> {
        lock(&self.inner.ambiguous).iter().map(|record| record.app.clone()).collect()
    }

    /// Stage 1 of host shutdown: refuses further starts and wakes every IPC
    /// handler wait. Joins nothing and performs no I/O.
    pub fn begin_shutdown(&self) {
        self.inner.closing.store(true, Ordering::SeqCst);
        self.inner.shutdown.cancel();
    }

    /// The token [`Self::begin_shutdown`] cancels. IPC handlers wait on it.
    #[must_use]
    pub fn shutdown_token(&self) -> CancellationToken { self.inner.shutdown.clone() }

    /// Stage 3 of host shutdown, after the registry stopped the module: closes
    /// the backend owner thread. `false` when quarantined resources keep it
    /// alive or it did not finish (never detached to look clean).
    #[must_use]
    pub fn finish_shutdown(&self) -> bool {
        matches!(
            self.inner.parts.backend.close(),
            stache_macos::windows::CloseOutcome::Closed
        )
    }
}

// ---------------------------------------------------------------------------
// Resources and quarantine
// ---------------------------------------------------------------------------

/// How the backend epoch still owes cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EpochOwed {
    /// The stop was not acknowledged: stop again.
    Stop,
    /// Registrations were quarantined by the backend: retry their removal.
    Cleanup,
}

/// The stages a teardown still owes. Cleared one by one, never all at once.
// One flag per cleanup stage: each is cleared independently as its stage finishes.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Owed {
    drag: bool,
    drain: bool,
    borders: bool,
    animation: bool,
    frame_clock: bool,
    enhanced_ui: bool,
    restore: bool,
    watcher: bool,
    displays: bool,
    epoch: Option<EpochOwed>,
    actor: bool,
    runner: bool,
}

impl Owed {
    const fn any(self) -> bool {
        self.drag
            || self.borders
            || self.drain
            || self.animation
            || self.frame_clock
            || self.enhanced_ui
            || self.restore
            || self.watcher
            || self.displays
            || self.epoch.is_some()
            || self.actor
            || self.runner
    }

    fn describe(self) -> Vec<String> {
        let mut owed = Vec::new();
        for (flag, name) in [
            (self.drag, "drag observer"),
            (self.borders, "borders runner"),
            (self.drain, "frame drain"),
            (self.animation, "animation worker"),
            (self.frame_clock, "frame clock"),
            (self.enhanced_ui, "enhanced ui restore"),
            (self.restore, "restore"),
            (self.watcher, "reservation watcher"),
            (self.displays, "display observer"),
            (self.epoch.is_some(), "backend epoch"),
            (self.actor, "actor"),
            (self.runner, "effect worker"),
        ] {
            if flag {
                owed.push(name.to_owned());
            }
        }
        owed
    }
}

/// What the restore stage found.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Restore {
    /// Nothing is owed. `ambiguous` records are retained and reported.
    Done { ambiguous: Vec<OwnedApp> },
    /// Some records are still owed (failed or uncertain unhide, or requests
    /// that did not drain); a later stop retries exactly those.
    Owed {
        note: String,
        ambiguous: Vec<OwnedApp>,
    },
}

/// Restores what the runtime hid. It runs after frame issuance drained and
/// before any native observer is removed.
fn restore_owned_visibility(runtime: Option<&Arc<TrackingRuntime>>, hop: Duration) -> Restore {
    let Some(runtime) = runtime else {
        return Restore::Done { ambiguous: Vec::new() };
    };
    let outcome = runtime.restore_visibility(hop);
    if outcome.is_complete() {
        return Restore::Done { ambiguous: outcome.ambiguous };
    }
    let mut parts: Vec<String> = outcome.blocked.into_iter().collect();
    parts.extend(outcome.remaining.iter().map(|app| {
        format!(
            "{:?} still owes an unhide ({:?}: {})",
            app.application,
            app.ownership,
            app.reason.as_deref().unwrap_or("no reason recorded")
        )
    }));
    Restore::Owed {
        note: parts.join("; "),
        ambiguous: outcome.ambiguous,
    }
}

struct Staged {
    config: Arc<TilingConfig>,
}

struct RunShared {
    token: CancellationToken,
    runtime: Arc<TrackingRuntime>,
    staged: Mutex<Option<Staged>>,
}

/// The exact resources a teardown owes, retained while any stage is incomplete.
struct Held {
    runtime: Option<Arc<TrackingRuntime>>,
    watcher: Option<Arc<ReservationWatcher>>,
    displays: Option<Arc<DisplayFeed>>,
    drag: Option<Arc<DragFeed>>,
    owed: Owed,
    last: String,
    _guards: Vec<CancelListener>,
}

enum Slot {
    Idle,
    Running {
        run: Arc<RunShared>,
        watcher: Arc<ReservationWatcher>,
        displays: Option<Arc<DisplayFeed>>,
        drag: Option<Arc<DragFeed>>,
        guards: Vec<CancelListener>,
    },
    Quarantined(Box<Held>),
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn error(operation: Operation, category: ErrorCategory, message: impl Into<String>) -> ModuleError {
    ModuleError::new(TilingConfig::ID, operation, category, message)
}

impl Inner {
    fn trace(&self, step: Teardown) {
        if let Some(trace) = &self.parts.timing.trace {
            trace(step);
        }
    }

    /// Runs the owed stages in order. Seal and wake come first and never wait.
    /// Returns when every stage ran once; stages that did not finish stay owed.
    #[allow(clippy::too_many_lines)] // one linear sequence of owed stages
    fn teardown(&self, held: &mut Held) {
        let hop = self.parts.timing.hop;
        if let Some(runtime) = &held.runtime {
            runtime.quiesce();
        }
        self.trace(Teardown::Sealed);
        let mut notes = Vec::new();

        // The observer goes first: callbacks stop at once and the service closes
        // the listen-only tap when this was its last passive subscriber.
        if held.owed.drag {
            if let Some(note) = held.drag.as_ref().and_then(|feed| feed.stop()) {
                self.status.drag_failure(Some(note));
            }
            held.owed.drag = false;
            self.trace(Teardown::Drag);
        }

        // The borders runner is released next: it sends nothing, so nothing more
        // reaches the service, and its connection and helper child are freed.
        if held.owed.borders {
            if held
                .runtime
                .as_ref()
                .is_none_or(|rt| rt.stop_borders(self.parts.timing.borders_join))
            {
                held.owed.borders = false;
                self.trace(Teardown::Borders);
            } else {
                let retained =
                    held.runtime.as_ref().map(|rt| rt.borders().retained()).unwrap_or_default();
                notes.push(if retained.is_empty() {
                    "the borders runner is blocked in a send".to_owned()
                } else {
                    format!(
                        "the borders runner left helper child(ren): {}",
                        retained.join(", ")
                    )
                });
            }
        }

        if held.owed.drain {
            match held.runtime.as_ref().map(|rt| rt.drain_frames(hop, &CancellationToken::new())) {
                None | Some(crate::actor::DrainOutcome::Drained) => {
                    held.owed.drain = false;
                    self.trace(Teardown::Drained);
                }
                Some(other) => notes.push(format!("frame issuance did not drain: {other:?}")),
            }
        }
        // The animation worker is closed and joined after the drain and before
        // the restore: no animated frame may follow a restoration.
        if held.owed.animation && !held.owed.drain {
            if held.runtime.as_ref().is_none_or(|rt| rt.stop_animation(hop)) {
                held.owed.animation = false;
                self.trace(Teardown::Animation);
            } else {
                notes.push("the animation worker did not end".to_owned());
            }
        }
        // Frame clocks that did not stop with the worker are kept and retried.
        // Their callbacks never write, so the restore does not wait for them.
        if held.owed.frame_clock && !held.owed.animation {
            if held.runtime.as_ref().is_none_or(|rt| rt.stop_frame_clocks(hop)) {
                held.owed.frame_clock = false;
            } else {
                notes.push("a frame clock did not stop".to_owned());
            }
        }
        // The lease restore waits for the drain and the worker too: a frame
        // write after it would take a new lease. It runs after the seal (owned
        // cleanup) and before the runtime-hidden applications are restored.
        if held.owed.enhanced_ui && !held.owed.drain && !held.owed.animation {
            match held.runtime.as_ref().map(|rt| rt.restore_enhanced_ui(hop)) {
                None | Some(stache_macos::windows::LeaseRestoreOutcome::Clean { .. }) => {
                    held.owed.enhanced_ui = false;
                    self.trace(Teardown::EnhancedUi);
                }
                Some(stache_macos::windows::LeaseRestoreOutcome::Owed { applications }) => {
                    notes.push(format!(
                        "{} application(s) still owe AXEnhancedUserInterface",
                        applications.len()
                    ));
                }
                Some(stache_macos::windows::LeaseRestoreOutcome::Unacknowledged) => {
                    notes
                        .push("the enhanced user interface restore is not acknowledged".to_owned());
                }
            }
        }
        // Restore waits for the drain and the worker: nothing may still be
        // moving a window.
        if held.owed.restore && !held.owed.drain && !held.owed.animation {
            match restore_owned_visibility(held.runtime.as_ref(), hop) {
                Restore::Done { ambiguous } => {
                    held.owed.restore = false;
                    self.merge_ambiguous(held.runtime.as_ref(), ambiguous);
                    self.trace(Teardown::Restore);
                }
                Restore::Owed { note, ambiguous } => {
                    self.merge_ambiguous(held.runtime.as_ref(), ambiguous);
                    notes.push(format!("restore is incomplete: {note}"));
                }
            }
        }
        if held.owed.watcher {
            if held.watcher.as_ref().is_none_or(|watcher| watcher.stop(hop)) {
                held.owed.watcher = false;
                self.trace(Teardown::Watcher);
            } else {
                notes.push("the reservation watcher did not end".to_owned());
            }
        }
        if held.owed.displays {
            if held.displays.as_ref().is_none_or(|feed| feed.stop(hop)) {
                held.owed.displays = false;
                self.trace(Teardown::Displays);
            } else {
                notes.push("the display observer is not removed".to_owned());
            }
        }
        if let Some(owed) = held.owed.epoch {
            let remaining = match (owed, &held.runtime) {
                (EpochOwed::Stop, Some(runtime)) => match runtime.stop_epoch(hop) {
                    StopOutcome::Clean { .. } | StopOutcome::NotCurrent => None,
                    StopOutcome::Quarantined { .. } => Some(EpochOwed::Cleanup),
                    other => {
                        notes
                            .push(format!("the backend epoch stop is not acknowledged: {other:?}"));
                        Some(EpochOwed::Stop)
                    }
                },
                (_, runtime) => {
                    let report = runtime.as_ref().map_or_else(
                        || {
                            self.parts
                                .backend
                                .retry_cleanup(&CancellationToken::new(), deadline(hop))
                        },
                        |runtime| runtime.retry_epoch_cleanup(hop),
                    );
                    match report {
                        Some(report)
                            if report.remaining_applications == 0
                                && report.remaining_registrations == 0 =>
                        {
                            None
                        }
                        Some(report) => {
                            notes.push(format!(
                                "{} application(s) and {} registration(s) still owe removal",
                                report.remaining_applications, report.remaining_registrations
                            ));
                            Some(EpochOwed::Cleanup)
                        }
                        None => {
                            notes.push("the window owner is unavailable".to_owned());
                            Some(EpochOwed::Cleanup)
                        }
                    }
                }
            };
            held.owed.epoch = remaining;
            if remaining.is_none() {
                self.trace(Teardown::Epoch);
            }
        }
        if held.owed.actor {
            if held.runtime.as_ref().is_none_or(|rt| rt.join_actor(hop)) {
                held.owed.actor = false;
                self.trace(Teardown::Actor);
            } else {
                notes.push("the actor did not end".to_owned());
            }
        }
        if held.owed.runner {
            if held.runtime.as_ref().is_none_or(|rt| rt.close_runner(hop)) {
                held.owed.runner = false;
                self.trace(Teardown::Runner);
            } else {
                notes.push("the effect worker is blocked in a native send".to_owned());
            }
        }
        if !notes.is_empty() {
            held.last = notes.join("; ");
        }
    }
}

fn deadline(hop: Duration) -> Option<std::time::Instant> {
    std::time::Instant::now().checked_add(hop)
}

// ---------------------------------------------------------------------------
// The module
// ---------------------------------------------------------------------------

/// The Tiling module. Construction is inert.
pub struct TilingModule {
    inner: Arc<Inner>,
    config: Option<TilingConfig>,
    slot: Slot,
}

impl TilingModule {
    /// Runs the teardown of `held`. Idle on success; quarantined otherwise.
    fn settle(&mut self, mut held: Box<Held>, operation: Operation) -> ModuleResult<()> {
        let inner = Arc::clone(&self.inner);
        inner.teardown(&mut held);
        if held.owed.any() {
            let owed = held.owed.describe();
            let message = format!("cleanup is incomplete ({}): {}", owed.join(", "), held.last);
            inner.status.quarantined(owed, message.clone());
            self.slot = Slot::Quarantined(held);
            Err(error(operation, ErrorCategory::Platform, message))
        } else {
            let ambiguous = lock(&inner.ambiguous);
            let mut notes = Vec::new();
            if !ambiguous.is_empty() {
                notes.push(format!(
                    "{} application(s) hidden by an unattributable cause are retained and not \
                     unhidden automatically; recovery needs explicit approval",
                    ambiguous.len()
                ));
            }
            drop(ambiguous);
            if held.runtime.as_ref().is_some_and(|rt| rt.borders().left_settings()) {
                notes.push(
                    "JankyBorders keeps the last border settings Stache sent; nothing was reset \
                     or restored"
                        .to_owned(),
                );
            }
            let detail = (!notes.is_empty()).then(|| notes.join("; "));
            inner.status.inactive(detail);
            self.slot = Slot::Idle;
            Ok(())
        }
    }

    /// Unwinds a failed start: only what was acquired is owed.
    fn rollback(
        &mut self,
        cause: String,
        runtime: Option<Arc<TrackingRuntime>>,
        watcher: Option<Arc<ReservationWatcher>>,
        displays: Option<Arc<DisplayFeed>>,
        token: &CancellationToken,
        guards: Vec<CancelListener>,
    ) -> ModuleError {
        token.cancel();
        let owed = Owed {
            drag: false,
            drain: false,
            borders: runtime.as_ref().is_some_and(|rt| rt.borders().owes_cleanup()),
            animation: runtime.is_some(),
            frame_clock: runtime.is_some(),
            enhanced_ui: runtime.is_some(),
            restore: false,
            watcher: watcher.is_some(),
            displays: displays.is_some(),
            epoch: runtime.as_ref().map(|_| EpochOwed::Stop),
            actor: runtime.is_some(),
            runner: runtime.is_some(),
        };
        let held = Box::new(Held {
            runtime,
            watcher,
            displays,
            drag: None,
            owed,
            last: cause.clone(),
            _guards: guards,
        });
        match self.settle(held, Operation::Start) {
            Ok(()) => {
                self.inner.status.inactive(Some(cause.clone()));
                error(Operation::Start, ErrorCategory::Platform, cause)
            }
            Err(quarantined) => quarantined,
        }
    }

    /// Quarantines the backend epoch of a start that failed inside the runtime
    /// constructor, where no runtime object exists to retry through.
    fn quarantine_epoch(&mut self, cause: String) -> ModuleError {
        let held = Box::new(Held {
            runtime: None,
            watcher: None,
            displays: None,
            drag: None,
            owed: Owed {
                epoch: Some(EpochOwed::Cleanup),
                ..Owed::default()
            },
            last: cause.clone(),
            _guards: Vec::new(),
        });
        match self.settle(held, Operation::Start) {
            Ok(()) => error(Operation::Start, ErrorCategory::Platform, cause),
            Err(quarantined) => quarantined,
        }
    }

    fn probe(&self, stage: StartStage) -> bool {
        self.inner.parts.timing.probe.as_ref().is_some_and(|probe| probe(stage))
    }
}

impl Module for TilingModule {
    type Config = TilingConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: TilingConfig::ID,
            provides: &[],
            requires: &[],
            permissions: &[Permission::Accessibility],
            executables: &[],
            surfaces: &[],
            observes: &[],
            reserves_screen: false,
        }
    }

    /// Accessibility is the only permission; only an explicit host request
    /// may prompt and Tiling never prompts itself.
    fn required_permissions(&self, _config: &TilingConfig) -> Vec<PermissionRequirement> {
        vec![PermissionRequirement::user_only(Permission::Accessibility)]
    }

    fn configure(&mut self, config: &TilingConfig, _ctx: &ModuleContext) -> ModuleResult<()> {
        stache_sdk::ModuleConfig::validate(config).map_err(|rejection| {
            error(Operation::Configure, ErrorCategory::Config, rejection.message())
        })?;
        self.config = Some(config.clone());
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // one linear staged start
    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        let inner = Arc::clone(&self.inner);
        match &self.slot {
            Slot::Quarantined(held) => {
                return Err(error(
                    Operation::Start,
                    ErrorCategory::Lifecycle,
                    format!(
                        "quarantined: cleanup still owes {}; stop must complete it first",
                        held.owed.describe().join(", ")
                    ),
                ));
            }
            Slot::Running { .. } => {
                return Err(error(
                    Operation::Start,
                    ErrorCategory::Lifecycle,
                    "already started",
                ));
            }
            Slot::Idle => {}
        }
        if inner.closing.load(Ordering::SeqCst) {
            return Err(error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "the host is shutting down",
            ));
        }
        let Some(config) = self.config.clone() else {
            return Err(error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "not configured",
            ));
        };
        let Some(watch) = inner.watch.get().cloned() else {
            return Err(error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "the accepted configuration watch is not installed",
            ));
        };
        let Some(pair) = watch.read().filter(|pair| *pair.config == config) else {
            return Err(error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "the configured value is not the accepted one",
            ));
        };
        let parts = &inner.parts;
        let mut options = TrackingOptions::new(parts.self_bundle_id.clone());
        options.registry = inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        options.defer_begin = true;
        options.probe.clone_from(&parts.timing.probe);
        options.borders.clone_from(&parts.borders);
        options.logger = Some(ctx.logger().clone());
        inner.status.borders_failure(None);
        let status = inner.status.clone();
        let sink: FaultSink = Arc::new(move |fault| {
            status.borders_failure(fault.map(crate::borders::Fault::describe));
        });
        options.borders_sink = Some(sink);
        let mut directory = Arc::clone(&parts.directory);
        if let Some(source) = &parts.applications {
            match source.open(&ctx.cancellation()) {
                Ok(actions) => {
                    let actions = Arc::new(actions);
                    // The run's own observation answers directory queries, so
                    // every instance belongs to the generation its hide, unhide
                    // and activation authorizations accept.
                    directory = Arc::new(ActionsDirectory::new(Arc::clone(&actions)));
                    options.actions = Some(actions);
                }
                Err(reason) => {
                    inner.status.inactive(Some(reason.clone()));
                    return Err(error(
                        Operation::Start,
                        ErrorCategory::Platform,
                        format!("the application control could not be installed: {reason}"),
                    ));
                }
            }
        }

        // Stages 1-3: backend epoch, effect worker, actor.
        let runtime = match TrackingRuntime::start_with_clock(
            Arc::clone(&parts.backend),
            directory,
            &options,
            Arc::clone(&parts.clock),
            None,
        ) {
            Ok(runtime) => Arc::new(runtime),
            Err(StartError::Incomplete { cause, stop }) => {
                return Err(self.quarantine_epoch(format!("{cause:?}; epoch stop: {stop:?}")));
            }
            Err(failure) => {
                inner.status.inactive(Some(format!("{failure:?}")));
                return Err(error(
                    Operation::Start,
                    ErrorCategory::Platform,
                    format!("the window runtime could not start: {failure:?}"),
                ));
            }
        };

        let token = CancellationToken::new();
        let mut guards = Vec::new();
        // Runtime cancellation and host invalidation seal the authority and
        // wake every wait at once, before any cleanup wait.
        let on_cancel = token.clone();
        let sealed = Arc::clone(&runtime);
        guards.push(ctx.cancellation().on_cancel(Arc::new(move || {
            sealed.quiesce();
            on_cancel.cancel();
        })));
        // The registry keeps this listener for its lifetime: hold the runtime
        // weakly so a stopped runtime is not kept alive by every past start.
        let invalidated = Arc::downgrade(&runtime);
        let on_invalidate = token.clone();
        ctx.invalidation().on_invalidate(Arc::new(move || {
            if let Some(runtime) = invalidated.upgrade() {
                runtime.quiesce();
            }
            on_invalidate.cancel();
        }));

        // Stage 4: reservations, primed before any frame effect can exist.
        if self.probe(StartStage::Watcher) {
            return Err(self.rollback(
                "the reservation watcher was refused".to_owned(),
                Some(runtime),
                None,
                None,
                &token,
                guards,
            ));
        }
        let source: Arc<dyn ReservationSource> = parts
            .reservations
            .clone()
            .unwrap_or_else(|| Arc::new(ctx.observe_screen_reservations()));
        let watcher = match ReservationWatcher::spawn(
            source,
            parts.projection.clone(),
            Arc::clone(runtime.ingress()),
        ) {
            Ok(watcher) => Arc::new(watcher),
            Err(spawn) => {
                return Err(self.rollback(
                    format!("the reservation watcher could not start: {spawn}"),
                    Some(runtime),
                    None,
                    None,
                    &token,
                    guards,
                ));
            }
        };
        if !watcher.wait_primed(parts.timing.hop) {
            return Err(self.rollback(
                "the host reservations were not delivered".to_owned(),
                Some(runtime),
                Some(watcher),
                None,
                &token,
                guards,
            ));
        }

        // Stage 5: the native display feed. Its initial layout reaches the
        // actor before the authority is published.
        let displays = match &parts.displays {
            None => None,
            Some(control) => {
                if self.probe(StartStage::Displays) {
                    return Err(self.rollback(
                        "the display observer was refused".to_owned(),
                        Some(runtime),
                        Some(watcher),
                        None,
                        &token,
                        guards,
                    ));
                }
                let feed = match DisplayFeed::spawn(control.clone(), &runtime, inner.status.clone())
                {
                    Ok(feed) => feed,
                    Err(cause) => {
                        return Err(self.rollback(
                            cause,
                            Some(runtime),
                            Some(watcher),
                            None,
                            &token,
                            guards,
                        ));
                    }
                };
                if let Err(cause) = feed.prime() {
                    return Err(self.rollback(
                        format!("the display layout was not delivered: {cause}"),
                        Some(runtime),
                        Some(watcher),
                        Some(feed),
                        &token,
                        guards,
                    ));
                }
                Some(feed)
            }
        };

        // Stage 6: the configuration coordinator.
        let run = Arc::new(RunShared {
            token: token.clone(),
            runtime: Arc::clone(&runtime),
            staged: Mutex::new(None),
        });
        let worker_inner = Arc::clone(&inner);
        let worker_run = Arc::clone(&run);
        let seen = pair.revision;
        if let Err(spawn) = ctx.spawn_worker("tiling-coordinator", move |_| {
            coordinate(&worker_inner, &worker_run, &watch, seen);
        }) {
            return Err(self.rollback(
                format!("the coordinator could not start: {spawn}"),
                Some(runtime),
                Some(watcher),
                displays,
                &token,
                guards,
            ));
        }

        // Stage 7: initial enumeration and the accepted configuration. Returns
        // immediately; no window has to move for the module to be Running.
        if let Err(failure) = runtime.begin() {
            return Err(self.rollback(
                format!("the initial enumeration could not be queued: {failure:?}"),
                Some(runtime),
                Some(watcher),
                displays,
                &token,
                guards,
            ));
        }
        // Optional: without these watches new applications, hide/unhide and
        // Cmd-Tab are not seen, which the run reports but does not refuse.
        if let Err(reason) = runtime.watch_applications() {
            ctx.logger().warn(format!("application watches unavailable: {reason}"));
        }
        if runtime.submit_config(pair.revision, Arc::clone(&pair.config)).is_err() {
            return Err(self.rollback(
                "the accepted configuration could not be queued".to_owned(),
                Some(runtime),
                Some(watcher),
                displays,
                &token,
                guards,
            ));
        }

        // Stage 8: publish the authority for the accepted revision.
        if ctx.cancellation().is_cancelled() || !runtime.authorize(pair.revision) {
            return Err(self.rollback(
                "the action authority could not be published".to_owned(),
                Some(runtime),
                Some(watcher),
                displays,
                &token,
                guards,
            ));
        }
        *lock(&inner.current) = Arc::downgrade(&runtime);
        inner.refresh_retained(&runtime);
        inner.status.running(runtime.snapshot_cell());
        inner.status.set_features(feature_diagnostics(&runtime, &pair.config));
        let animation = Arc::downgrade(runtime.animation());
        inner.status.set_pacing_source(Some(Box::new(move || {
            animation
                .upgrade()
                .map(|control| control.pacing_diagnostics())
                .unwrap_or_default()
        })));
        let animation = Arc::downgrade(runtime.animation());
        inner.status.set_animation_source(Some(Box::new(move || {
            animation.upgrade().map(|control| control.report()).unwrap_or_default()
        })));

        // Stage 9: the passive drag observer. Optional: it cannot fail the
        // start, and ordinary tiling never depends on it.
        let drag = match DragFeed::subscribe(ctx.input(), Arc::clone(runtime.ingress())) {
            Ok(feed) => Some(Arc::new(feed)),
            Err(unavailable) => {
                inner.status.drag_failure(Some(unavailable.to_string()));
                None
            }
        };
        self.slot = Slot::Running {
            run,
            watcher,
            displays,
            drag,
            guards,
        };
        Ok(())
    }

    fn reconfigure(&mut self, config: &TilingConfig, _ctx: &ModuleContext) -> ModuleResult<()> {
        let Slot::Running { run, .. } = &self.slot else {
            return Err(error(
                Operation::Reconfigure,
                ErrorCategory::Lifecycle,
                "not started",
            ));
        };
        // Stage fallible pure work only. Nothing reaches the actor, the gate or
        // any native resource until the same revision is observed.
        if let Some(stage) = &self.inner.parts.timing.stage {
            stage(config)
                .map_err(|reason| error(Operation::Reconfigure, ErrorCategory::Config, reason))?;
        }
        // The pure workspace mapping is prepared against the published state
        // before acceptance; a refused mapping leaves the old runtime untouched.
        crate::workspaces::prepare_reload_from_snapshot(
            &run.runtime.snapshot_cell().load(),
            config,
        )
        .map_err(|reason| {
            error(Operation::Reconfigure, ErrorCategory::Config, reason.to_string())
        })?;
        *lock(&run.staged) = Some(Staged {
            config: Arc::new(config.clone()),
        });
        self.config = Some(config.clone());
        Ok(())
    }

    fn stop(&mut self, _ctx: &ModuleContext) -> ModuleResult<()> {
        match std::mem::replace(&mut self.slot, Slot::Idle) {
            Slot::Idle => Ok(()),
            Slot::Quarantined(held) => self.settle(held, Operation::Stop),
            Slot::Running {
                run,
                watcher,
                displays,
                drag,
                guards,
            } => {
                // Seal first: one atomic store, independent of any stalled send.
                run.runtime.quiesce();
                run.token.cancel();
                lock(&run.staged).take();
                *lock(&self.inner.current) = Weak::new();
                self.inner.status.stopping();
                let had_displays = displays.is_some();
                let had_drag = drag.is_some();
                let held = Box::new(Held {
                    runtime: Some(Arc::clone(&run.runtime)),
                    watcher: Some(watcher),
                    displays,
                    drag,
                    owed: Owed {
                        drag: had_drag,
                        borders: run.runtime.borders().owes_cleanup(),
                        drain: true,
                        animation: true,
                        frame_clock: true,
                        enhanced_ui: true,
                        restore: true,
                        watcher: true,
                        displays: had_displays,
                        epoch: Some(EpochOwed::Stop),
                        actor: true,
                        runner: true,
                    },
                    last: String::new(),
                    _guards: guards,
                });
                self.settle(held, Operation::Stop)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------

/// The optional-feature diagnostics of an adopted configuration: features not
/// implemented yet plus an animation worker that could not start.
fn feature_diagnostics(runtime: &TrackingRuntime, config: &TilingConfig) -> Vec<String> {
    let mut features = crate::status::unsupported_features(config);
    features.extend(runtime.animation_failure());
    features
}

/// Adopts accepted revisions only after observing them through the watch.
fn coordinate(inner: &Inner, run: &RunShared, watch: &AcceptedWatch<TilingConfig>, mut seen: u64) {
    loop {
        match watch.wait_for_change(seen, inner.parts.timing.poll, &run.token) {
            AcceptedWait::Changed(pair) => {
                seen = pair.revision;
                if let Some(observed) = &inner.parts.timing.observed {
                    observed(pair.revision);
                }
                // The staged candidate is consumed only by the very value that
                // was accepted; a newer one waits for its own revision.
                {
                    let mut staged = lock(&run.staged);
                    if staged.as_ref().is_some_and(|candidate| candidate.config == pair.config) {
                        staged.take();
                    }
                }
                if run.token.is_cancelled() {
                    return;
                }
                // The pair is coherent, so the revision is the registry's own.
                let _ = run.runtime.submit_config(pair.revision, Arc::clone(&pair.config));
                inner.status.set_features(feature_diagnostics(&run.runtime, &pair.config));
            }
            AcceptedWait::TimedOut => {}
            AcceptedWait::Cancelled | AcceptedWait::Closed => return,
        }
    }
}
