//! The single-writer actor: it owns [`TilingState`], applies pure transitions
//! and publishes immutable snapshots.
//!
//! [`Engine::turn`] is the whole loop body and takes its time from a [`Clock`],
//! so tests drive it deterministically; [`spawn_actor`] wraps it in a std thread
//! that sleeps on the mailbox until work, a geometry deadline or cancellation.
//! Native reads happen in the [`EffectRunner`], never in the actor, and their
//! results come back as tagged completions. Frame writes are the one mutation:
//! [`FrameControl`] gates and tracks them.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use stache_macos::applications::LifecycleKind;
use stache_macos::windows::{ApplicationChange, EventQueue, WindowChange, WindowEventKind};
use stache_macos::{ApplicationId, DisplayId};
use stache_sdk::CancellationToken;

mod diag;
mod frames;

pub use diag::Diag;
pub use frames::{DRAIN_QUANTUM, DrainOutcome, FRAME_PLAN_DEADLINE, FrameControl, FrameJob};

use crate::animation::{AnimationControl, AnimationJob, AnimationSettings};
use crate::backend::WindowBackend;
use crate::borders::{BordersControl, FocusInput};
use crate::drag::{DragSignal, GESTURE_TIMEOUT, LostCause};
use crate::events::{Ingress, RETIME_EPSILON, Wake, flush_period};
use crate::state::{
    AppInfo, Cause, CompletionResult, Effect, Health, HealthFault, Input, Phase, Scope, Stamp,
    TabOutbox, TilingSnapshot, TilingState, transition,
};
use crate::visibility::{ApplicationActions, Exec, Observation, Planned, VisibilityControl};
use crate::workspaces::visibility_requirement;

/// Inputs processed before the loop re-checks cancellation and timers.
pub const MAX_INPUTS_PER_TURN: usize = 256;
/// Read-only effects queued for the runner.
pub const RUNNER_CAPACITY: usize = 64;
/// Wait budget for one backend hop.
pub const BACKEND_BUDGET: Duration = Duration::from_secs(5);
/// How long after a native tab group event the application is listed again, so
/// `AppKit`'s transient window states have passed.
pub const TAB_SETTLE_DELAY: Duration = Duration::from_millis(150);
/// Settle listings repeated for windows that were still in transition.
pub const MAX_TAB_SETTLE_RETRIES: u8 = 4;
/// The waits before each time-based retry of a transient track failure (an
/// application that has not finished launching). After the last one the
/// application is left to the activation and relaunch retries.
pub const TRACK_RETRY_BACKOFF: [Duration; 5] = [
    Duration::from_millis(100),
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];
/// The actor thread name.
pub const ACTOR_THREAD_NAME: &str = "stache-tiling-actor";

/// A monotonic time source.
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Instant;
}

/// The real clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant { Instant::now() }
}

/// A manually advanced clock for deterministic tests.
#[cfg(any(test, feature = "testing"))]
pub struct ManualClock {
    base: Instant,
    offset: Mutex<Duration>,
}

#[cfg(any(test, feature = "testing"))]
impl Default for ManualClock {
    fn default() -> Self {
        Self {
            base: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
        }
    }
}

#[cfg(any(test, feature = "testing"))]
impl ManualClock {
    /// Moves time forward.
    pub fn advance(&self, by: Duration) {
        *self.offset.lock().unwrap_or_else(PoisonError::into_inner) += by;
    }
}

#[cfg(any(test, feature = "testing"))]
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.base + *self.offset.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Read-only application facts (the production one wraps `NSWorkspace`).
pub trait AppDirectory: Send + Sync {
    /// Every running application with metadata.
    fn running(&self) -> Vec<AppInfo>;
    /// One application's metadata, when known.
    fn describe(&self, application: ApplicationId) -> Option<AppInfo>;
}

/// The latest published snapshot, swapped under a short lock.
pub struct SnapshotCell {
    current: Mutex<Arc<TilingSnapshot>>,
}

impl Default for SnapshotCell {
    fn default() -> Self {
        Self {
            current: Mutex::new(Arc::new(TilingSnapshot::initial())),
        }
    }
}

impl SnapshotCell {
    /// The atomic snapshot; never a field-by-field sample.
    #[must_use]
    pub fn load(&self) -> Arc<TilingSnapshot> {
        Arc::clone(&self.current.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub(crate) fn publish(&self, snapshot: TilingSnapshot) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(snapshot);
    }
}

// ------------------------------------------------------------------ runner

/// Why an effect could not be queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerError {
    /// The bounded queue is full.
    Full,
    /// The runner stopped.
    Closed,
}

/// Executes read-only effects away from the actor.
pub trait EffectRunner: Send + Sync {
    /// Queues an effect without blocking.
    ///
    /// # Errors
    ///
    /// [`RunnerError`]: nothing was queued, so no completion will follow.
    fn submit(&self, effect: Effect) -> Result<(), RunnerError>;

    /// Stops accepting and joins any worker.
    fn close(&self);
}

/// What an effect needs to run.
#[derive(Clone)]
pub struct EffectEnv {
    /// The backend (reads, plus gated frame writes through [`FrameControl`]).
    pub backend: Arc<dyn WindowBackend>,
    /// Application facts.
    pub directory: Arc<dyn AppDirectory>,
    /// Where completions go.
    pub ingress: Arc<Ingress>,
    /// Cancels in-flight reads.
    pub cancel: CancellationToken,
    /// Gates and tracks frame writes.
    pub frames: Arc<FrameControl>,
    /// The visibility ledger, seal and executors.
    pub visibility: Arc<VisibilityControl>,
    /// Native hide, unhide and activation; `None` without an application
    /// observation (nothing is hidden and focus cannot activate).
    pub actions: Option<Arc<ApplicationActions>>,
    /// The animation worker handle: inert until an enabled configuration.
    pub animation: Arc<AnimationControl>,
}

impl EffectEnv {
    fn deadline() -> Instant { Instant::now() + BACKEND_BUDGET }

    fn complete(&self, stamp: Stamp, result: CompletionResult) {
        // A closed mailbox means the actor is gone; nobody is waiting.
        let _ = self.ingress.submit(Input::Completion { stamp, result });
    }

    fn refresh(&self, stamp: Stamp, application: ApplicationId) {
        let outcome = self.backend.refresh(
            stamp.epoch,
            application,
            self.cancel.clone(),
            Some(Self::deadline()),
        );
        self.complete(stamp, CompletionResult::Refresh(application, outcome));
    }

    fn read_focus(&self, stamp: Stamp, application: ApplicationId) {
        let outcome = self.backend.focused_window(
            stamp.epoch,
            application,
            &self.cancel,
            Some(Self::deadline()),
        );
        self.complete(stamp, CompletionResult::Focus(application, outcome));
    }

    /// Runs one effect. Reads (`track`, `refresh`, `untrack`, `snapshot`, the
    /// application listing) and the gated frame write; the write is the only
    /// path that can move a window.
    #[allow(clippy::too_many_lines)] // one linear dispatch over every effect
    pub fn run(&self, effect: &Effect) {
        match *effect {
            Effect::Track { stamp, application, pid } => {
                let outcome = self.backend.track(
                    stamp.epoch,
                    application,
                    pid,
                    self.cancel.clone(),
                    Some(Self::deadline()),
                );
                self.complete(stamp, CompletionResult::Track(application, outcome));
            }
            Effect::Refresh { stamp, application } => self.refresh(stamp, application),
            Effect::ReadFocus { stamp, application } => self.read_focus(stamp, application),
            Effect::Snapshot { stamp, window } => {
                let outcome = self.backend.snapshot(window, &self.cancel, Some(Self::deadline()));
                self.complete(stamp, CompletionResult::Snapshot(window, outcome));
            }
            Effect::Untrack { stamp, application } => {
                let _ = self.backend.untrack(
                    stamp.epoch,
                    application,
                    &self.cancel,
                    Some(Self::deadline()),
                );
            }
            Effect::ListApplications { stamp } => {
                let apps = self.directory.running();
                let _ = self.ingress.submit(Input::Applications { stamp, apps });
            }
            Effect::WriteFrame {
                stamp,
                revision,
                plan_sequence,
                window,
                target,
                components,
                from,
                animate,
                refresh_hz,
                display,
                cross_display,
            } => {
                if animate {
                    let job = AnimationJob {
                        stamp,
                        revision,
                        plan_sequence,
                        window,
                        from,
                        target,
                        components,
                        period: flush_period(refresh_hz),
                        display,
                        cross_display,
                        settings: AnimationSettings::default(),
                    };
                    // Accepted: the worker acknowledges and finishes it. Refused
                    // (disabled, stopped, full): written directly below.
                    if self.animation.submit(job).is_ok() {
                        return;
                    }
                }
                // Asynchronous: the reply posts the completion and finishes the
                // write from the shard thread (or inline when the write is inert),
                // so this runner never waits on a native frame write.
                let ingress = Arc::clone(&self.ingress);
                let frames = Arc::clone(&self.frames);
                let job = FrameJob {
                    generation: stamp.registry,
                    revision,
                    plan_sequence,
                    window,
                    target,
                    components,
                };
                self.frames.submit_direct(&*self.backend, &self.cancel, &job, move |outcome| {
                    // A closed mailbox means the actor is gone; nobody waits.
                    let _ = ingress.submit(Input::Completion {
                        stamp,
                        result: CompletionResult::Frame {
                            window,
                            plan_sequence,
                            revision,
                            outcome,
                        },
                    });
                    frames.finish(window, plan_sequence);
                });
            }
            Effect::CancelFrame { window } => self.frames.cancel(window),
            Effect::HideApp { stamp, application } => {
                self.visibility.execute_hide(&self.exec(stamp), application);
                self.complete(stamp, CompletionResult::Visibility(application));
            }
            Effect::UnhideApp { stamp, application } => {
                self.visibility.execute_unhide(&self.exec(stamp), application);
                self.complete(stamp, CompletionResult::Visibility(application));
            }
            Effect::Focus { stamp, window } => {
                self.visibility.execute_focus(
                    &self.exec(stamp),
                    &self.frames,
                    stamp.registry,
                    window,
                );
                self.visibility.finish_focus();
                self.complete(stamp, CompletionResult::Visibility(window.application()));
            }
            Effect::Publish => {}
        }
    }

    fn exec(&self, stamp: Stamp) -> Exec<'_> {
        Exec {
            actions: self.actions.as_deref(),
            backend: &*self.backend,
            epoch: stamp.epoch,
            cancel: &self.cancel,
        }
    }

    /// An effect that will never run (the worker was cancelled).
    pub fn discard(&self, effect: &Effect) {
        match *effect {
            Effect::WriteFrame { window, plan_sequence, .. } => {
                self.frames.finish(window, plan_sequence);
            }
            Effect::HideApp { application, .. } => {
                self.visibility.cancel_planned(Planned::Hide(application));
            }
            Effect::UnhideApp { application, .. } => {
                self.visibility.cancel_planned(Planned::Unhide(application));
            }
            Effect::Focus { .. } => self.visibility.finish_focus(),
            _ => {}
        }
    }
}

/// Runs each effect inline on submit. Deterministic; used by tests.
pub struct InlineRunner {
    env: EffectEnv,
}

impl InlineRunner {
    /// An inline runner.
    #[must_use]
    pub const fn new(env: EffectEnv) -> Self { Self { env } }
}

impl EffectRunner for InlineRunner {
    fn submit(&self, effect: Effect) -> Result<(), RunnerError> {
        self.env.run(&effect);
        Ok(())
    }

    fn close(&self) {}
}

/// A bounded queue drained by one worker thread.
pub struct ThreadRunner {
    sender: Mutex<Option<SyncSender<Effect>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl ThreadRunner {
    /// Starts the worker with a queue of `capacity` effects.
    ///
    /// # Errors
    ///
    /// The thread could not be spawned.
    pub fn spawn(env: EffectEnv, capacity: usize) -> io::Result<Self> {
        let (sender, receiver) = sync_channel::<Effect>(capacity.max(1));
        let worker = std::thread::Builder::new().name("stache-tiling-effects".to_owned()).spawn(
            move || {
                while let Ok(effect) = receiver.recv() {
                    if env.cancel.is_cancelled() {
                        env.discard(&effect);
                        continue;
                    }
                    env.run(&effect);
                }
            },
        )?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
        })
    }
}

/// Joins the thread in `slot` if it finishes within `timeout`. A thread that
/// does not finish keeps its handle in `slot` (never detached) so a retry can
/// join it. `true` when nothing is left to join.
pub fn join_within(slot: &Mutex<Option<JoinHandle<()>>>, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now().checked_add(timeout);
    loop {
        {
            let mut guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
            match guard.as_ref() {
                None => return true,
                Some(handle) if handle.is_finished() => {
                    let handle = guard.take();
                    drop(guard);
                    if let Some(handle) = handle {
                        let _ = handle.join();
                    }
                    return true;
                }
                Some(_) => {}
            }
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

impl ThreadRunner {
    /// Closes admission and joins the worker if it ends within `timeout`.
    ///
    /// A worker stuck in a native send is not detached: its handle stays so a
    /// later call can join it. `true` when the worker is joined.
    #[must_use]
    pub fn close_within(&self, timeout: Duration) -> bool {
        drop(self.sender.lock().unwrap_or_else(PoisonError::into_inner).take());
        join_within(&self.worker, timeout)
    }
}

impl EffectRunner for ThreadRunner {
    fn submit(&self, effect: Effect) -> Result<(), RunnerError> {
        let sender = self.sender.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let Some(sender) = sender else {
            return Err(RunnerError::Closed);
        };
        sender.try_send(effect).map_err(|error| match error {
            TrySendError::Full(_) => RunnerError::Full,
            TrySendError::Disconnected(_) => RunnerError::Closed,
        })
    }

    fn close(&self) {
        let sender = self.sender.lock().unwrap_or_else(PoisonError::into_inner).take();
        drop(sender);
        let worker = self.worker.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

// ------------------------------------------------------------------- engine

/// What one turn did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Turn {
    /// Inputs applied.
    pub processed: usize,
    /// When the next geometry flush is due.
    pub next_deadline: Option<Instant>,
    /// The actor can no longer continue.
    pub halted: bool,
}

/// The actor loop body.
pub struct Engine {
    state: Option<TilingState>,
    ingress: Arc<Ingress>,
    cell: Arc<SnapshotCell>,
    runner: Arc<dyn EffectRunner>,
    sink: Arc<EventQueue>,
    clock: Arc<dyn Clock>,
    halted: bool,
    hook: Option<Arc<dyn Fn() + Send + Sync>>,
    rates: BTreeMap<DisplayId, Option<f64>>,
    frames: Option<Arc<FrameControl>>,
    visibility: Option<Arc<VisibilityControl>>,
    borders: Option<Arc<BordersControl>>,
    animation: Option<Arc<AnimationControl>>,
    last_admission: crate::events::AdmissionStats,
    /// The drag gesture being timed and when it is abandoned.
    gesture_watch: Option<(u64, Instant)>,
    /// Applications that were ours to unhide and still read hidden.
    released_hidden: BTreeSet<ApplicationId>,
    /// Native tab group settle listings: when each application is listed and
    /// how many times a listing already had to be repeated.
    settle: BTreeMap<ApplicationId, SettleEntry>,
    /// Backoff of transient track failures.
    track_retry: BTreeMap<ApplicationId, TrackRetry>,
    diag: Diag,
}

/// One application's time-based track retry.
#[derive(Clone, Copy, Debug, Default)]
struct TrackRetry {
    due: Option<Instant>,
    /// Retries issued so far.
    attempts: u8,
    /// A retry was submitted and the state has not consumed it yet.
    submitted: bool,
    /// The backoff schedule ran out.
    gave_up: bool,
}

/// One application's pending settle listing.
#[derive(Clone, Copy, Debug, Default)]
struct SettleEntry {
    due: Option<Instant>,
    attempts: u8,
}

const fn effect_stamp(effect: &Effect) -> Option<Stamp> {
    match effect {
        Effect::Track { stamp, .. }
        | Effect::Refresh { stamp, .. }
        | Effect::Snapshot { stamp, .. }
        | Effect::ReadFocus { stamp, .. }
        | Effect::Untrack { stamp, .. }
        | Effect::ListApplications { stamp }
        | Effect::HideApp { stamp, .. }
        | Effect::UnhideApp { stamp, .. }
        | Effect::Focus { stamp, .. }
        | Effect::WriteFrame { stamp, .. } => Some(*stamp),
        Effect::CancelFrame { .. } | Effect::Publish => None,
    }
}

impl Engine {
    /// An engine over `state`.
    #[must_use]
    pub fn new(
        state: TilingState,
        ingress: Arc<Ingress>,
        cell: Arc<SnapshotCell>,
        runner: Arc<dyn EffectRunner>,
        sink: Arc<EventQueue>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            state: Some(state),
            ingress,
            cell,
            runner,
            sink,
            clock,
            halted: false,
            hook: None,
            rates: BTreeMap::new(),
            frames: None,
            visibility: None,
            borders: None,
            animation: None,
            last_admission: crate::events::AdmissionStats::default(),
            gesture_watch: None,
            released_hidden: BTreeSet::new(),
            settle: BTreeMap::new(),
            track_retry: BTreeMap::new(),
            diag: Diag::default(),
        }
    }

    /// Reports phase changes, failed applications, missing frames, intents and
    /// visibility requests through the host logger.
    pub fn set_logger(
        &mut self,
        logger: Option<stache_sdk::ScopedLogger>,
        origin: crate::logclock::LogOrigin,
    ) {
        self.diag = Diag::new(logger, origin);
    }

    /// Calls `hook` at the start of every turn (tests use it to stall the actor).
    pub fn set_turn_hook(&mut self, hook: Arc<dyn Fn() + Send + Sync>) { self.hook = Some(hook); }

    /// Shares the animation control: the engine copies its instant-fallback
    /// applications into the state before each transition and reports lease
    /// problems and terminations to it.
    pub fn set_animation_control(&mut self, animation: Arc<AnimationControl>) {
        self.animation = Some(animation);
    }

    /// Shares the frame issuance control with the runner's environment.
    pub fn set_frame_control(&mut self, frames: Arc<FrameControl>) { self.frames = Some(frames); }

    /// Shares the visibility control with the runner's environment. The actor
    /// is its only planner and observer.
    pub fn set_visibility_control(&mut self, visibility: Arc<VisibilityControl>) {
        self.visibility = Some(visibility);
    }

    /// Shares the borders control. The actor feeds it the focus state as plain
    /// values after each published change; it never blocks on it.
    pub fn set_borders_control(&mut self, borders: Arc<BordersControl>) {
        self.borders = Some(borders);
    }

    /// The published snapshot cell.
    #[must_use]
    pub fn cell(&self) -> Arc<SnapshotCell> { Arc::clone(&self.cell) }

    fn route(&self, kind: &WindowEventKind) -> Option<DisplayId> {
        match kind {
            WindowEventKind::Window {
                window,
                change: WindowChange::Moved | WindowChange::Resized,
            } => self.state.as_ref().and_then(|state| state.display_of(*window)),
            _ => None,
        }
    }

    /// Drains the sink, admits, applies up to [`MAX_INPUTS_PER_TURN`] inputs and
    /// publishes.
    pub fn turn(&mut self) -> Turn {
        if self.halted {
            return Turn {
                processed: 0,
                next_deadline: None,
                halted: true,
            };
        }
        if let Some(hook) = &self.hook {
            hook();
        }
        let now = self.clock.now();
        self.ingress.take_sink_dirty();
        let batch = self.sink.drain();
        let routed: Vec<_> = batch
            .events
            .into_iter()
            .map(|event| {
                let route = self.route(&event.kind);
                (event, route)
            })
            .collect();
        let inputs = self.ingress.with(|pipeline| {
            pipeline.admit_sink_drop(batch.dropped);
            for (event, route) in routed {
                pipeline.admit_event(event, route, now);
            }
            let mut inputs = pipeline.take(MAX_INPUTS_PER_TURN);
            inputs.extend(pipeline.due_geometry(now));
            inputs
        });
        let mut processed = 0;
        for input in inputs {
            if !self.apply(input, now) {
                return Turn {
                    processed,
                    next_deadline: None,
                    halted: true,
                };
            }
            processed += 1;
        }
        let visibility_due = self.reconcile_visibility(now);
        self.publish_if_changed(processed);
        let gesture_due = self.watch_gesture(now);
        let settle_due = self.fire_settles(now);
        let retry_due = self.fire_track_retries(now);
        let geometry = self.ingress.with(|pipeline| pipeline.geometry().next_deadline());
        Turn {
            processed,
            next_deadline: [geometry, gesture_due, visibility_due, settle_due, retry_due]
                .into_iter()
                .flatten()
                .min(),
            halted: false,
        }
    }

    /// Takes what the transition asked of the clock: log the tab group
    /// handovers and schedule one coalesced settle listing per application.
    ///
    /// A fresh trigger arms the timer once (further triggers while it waits are
    /// coalesced into it). A retry, requested because a listing skipped windows
    /// that were still in transition, arms it again until the retries run out;
    /// then the windows still waiting for a successor are closed for real.
    fn note_tab_outbox(&mut self, state: &TilingState, outbox: TabOutbox, now: Instant) {
        for (old, new) in outbox.handovers {
            self.diag.handover(state, old, new);
        }
        for note in outbox.notes {
            self.diag.slot_note(state, note);
        }
        for (application, retry) in outbox.settle {
            let entry = self.settle.entry(application).or_default();
            if retry {
                entry.attempts = entry.attempts.saturating_add(1);
                let expired = entry.attempts > MAX_TAB_SETTLE_RETRIES;
                self.diag.settle_retry(state, application, entry.attempts, expired);
                if expired {
                    self.settle.remove(&application);
                    let _ = self.ingress.submit(Input::SettleExpired(application));
                    continue;
                }
            } else if entry.due.is_none() {
                entry.attempts = 0;
            }
            entry.due.get_or_insert(now + TAB_SETTLE_DELAY);
        }
    }

    /// Lists the applications whose settle delay passed (as a tab-settle
    /// unconfirmed marking, so the one reconcile path issues the refresh) and
    /// returns when the next one is due.
    fn fire_settles(&mut self, now: Instant) -> Option<Instant> {
        let due: Vec<ApplicationId> = self
            .settle
            .iter()
            .filter(|(_, entry)| entry.due.is_some_and(|due| due <= now))
            .map(|(application, _)| *application)
            .collect();
        for application in due {
            if let Some(entry) = self.settle.get_mut(&application) {
                entry.due = None;
                if entry.attempts == 0 {
                    self.settle.remove(&application);
                }
            }
            let _ = self
                .ingress
                .submit(Input::Unconfirmed(Scope::App(application), Cause::TabSettle));
        }
        self.settle.values().filter_map(|entry| entry.due).min()
    }

    /// Retries the applications whose track failed for a plausibly transient
    /// reason (typically one that is still launching), on the
    /// [`TRACK_RETRY_BACKOFF`] schedule, and returns when the next retry is due.
    /// The backoff is bounded; later attempts remain the activation and
    /// relaunch retries of the state.
    fn fire_track_retries(&mut self, now: Instant) -> Option<Instant> {
        let state = self.state.as_ref()?;
        self.track_retry.retain(|application, entry| {
            let live = state.startup.failed.contains(application)
                || state.startup.awaiting.contains(application)
                || state.startup.transient.contains(application);
            live && (entry.gave_up || state.apps.contains_key(application))
        });
        for application in &state.startup.transient {
            let entry = self.track_retry.entry(*application).or_default();
            if entry.submitted || entry.due.is_some() || entry.gave_up {
                continue;
            }
            let Some(delay) = TRACK_RETRY_BACKOFF.get(usize::from(entry.attempts)).copied() else {
                entry.gave_up = true;
                self.diag.track_gave_up(state, *application, entry.attempts);
                continue;
            };
            entry.due = Some(now + delay);
            self.diag.track_retry_armed(state, *application, entry.attempts + 1, delay);
        }
        // A submitted retry is consumed once the state no longer lists the failure.
        for (application, entry) in &mut self.track_retry {
            if entry.submitted && !state.startup.transient.contains(application) {
                entry.submitted = false;
            }
        }
        let mut next: Option<Instant> = None;
        for (application, entry) in &mut self.track_retry {
            let Some(due) = entry.due else { continue };
            if due <= now {
                entry.due = None;
                entry.attempts += 1;
                entry.submitted = true;
                let _ = self.ingress.submit(Input::RetryTrack(*application));
            } else {
                next = Some(next.map_or(due, |other| other.min(due)));
            }
        }
        next
    }

    /// Times the drag gesture in progress. One that outlives
    /// [`GESTURE_TIMEOUT`] is abandoned as a lost observation (no up is
    /// invented), so a missed up cannot hold the planner back forever. Returns
    /// when the actor must wake for it.
    fn watch_gesture(&mut self, now: Instant) -> Option<Instant> {
        let current = self.state.as_ref().and_then(TilingState::drag_gesture);
        let Some(id) = current else {
            self.gesture_watch = None;
            return None;
        };
        let due = match self.gesture_watch {
            Some((watched, due)) if watched == id => due,
            _ => now + GESTURE_TIMEOUT,
        };
        self.gesture_watch = Some((id, due));
        if now >= due {
            // The lost signal is applied by the next turn; stop timing meanwhile.
            let _ = self.ingress.submit(Input::Drag(DragSignal::Lost(LostCause::Timeout)));
            self.gesture_watch = None;
            return None;
        }
        Some(due)
    }

    /// Applies one input; `false` when the actor halted.
    fn apply(&mut self, input: Input, now: Instant) -> bool {
        let retime = matches!(input, Input::Displays(_));
        self.observe_visibility(&input);
        if let Some(state) = &self.state {
            self.diag.input(state, &input);
        }
        // Hiding the frontmost application makes the system activate another
        // one. When that one is being hidden by us too (its hide is in flight or
        // its hidden notice is still due), the activation is our own doing, not
        // the user's: acting on it would switch back and fight the switch.
        if let (Input::Activated(application), Some(visibility)) = (&input, &self.visibility)
            && visibility.hide_unsettled(*application)
        {
            return true;
        }
        let Some(mut state) = self.state.take() else {
            return false;
        };
        // The runtime-owned hidden applications are an input of the pure
        // transition: their windows keep their layout slot.
        if let Some(visibility) = &self.visibility {
            // Our unhide is done once the ledger releases the record, yet the
            // application still reads hidden until its shown observation: it
            // stays ours until then. Anything that reads visible is dropped.
            self.released_hidden
                .retain(|application| state.apps.get(application).is_some_and(|app| app.hidden));
            let mut owned = visibility.hidden_apps();
            owned.extend(self.released_hidden.iter().copied());
            self.released_hidden.clone_from(&owned);
            state.set_owned_hidden(owned);
        }
        // The same for the instant-fallback applications.
        if let Some(animation) = &self.animation {
            state.set_animation_fallback(animation.fallback_apps());
        }
        let lease = match &input {
            Input::Window(event) => match &event.kind {
                WindowEventKind::EnhancedUi { application, note } => Some((*application, *note)),
                _ => None,
            },
            _ => None,
        };
        let terminated = match &input {
            Input::Lifecycle {
                kind: LifecycleKind::Terminated,
                info,
            } => Some(info.application),
            _ => None,
        };
        let Ok((mut state, effects)) = catch_unwind(AssertUnwindSafe(|| transition(state, input)))
        else {
            self.fail_panicked();
            return false;
        };
        self.note_animation(&state, lease, terminated);
        self.diag.observe(&state);
        self.diag.effects(&state, &effects);
        let outbox = state.take_tab_outbox();
        self.note_tab_outbox(&state, outbox, now);
        self.state = Some(state);
        self.dispatch(effects);
        if retime {
            self.retime(now);
        }
        true
    }

    /// Tells the animation control what this turn changed for it: a
    /// terminated application is forgotten, and an application whose
    /// `AXEnhancedUserInterface` lease cannot be kept moves without animation
    /// (it turns the attribute back on in two consecutive batches, or it could
    /// not be turned off).
    fn note_animation(
        &self,
        state: &TilingState,
        lease: Option<(ApplicationId, stache_macos::windows::EnhancedUiNote)>,
        terminated: Option<ApplicationId>,
    ) {
        use stache_macos::windows::EnhancedUiNote;

        use crate::animation::FallbackReason;
        let Some(animation) = &self.animation else { return };
        if let Some(application) = terminated {
            animation.forget_application(application);
        }
        let Some((application, note)) = lease else { return };
        let Some(lease_stats) = state.enhanced_ui(application) else {
            return;
        };
        match note {
            EnhancedUiNote::ReEnabled { .. } if lease_stats.cannot_keep_up() => {
                animation.enter_fallback(application, FallbackReason::ReEnablesEnhancedUi);
            }
            EnhancedUiNote::SetFailed(_) if lease_stats.set_failed() => {
                animation.enter_fallback(application, FallbackReason::EnhancedUiStuck);
            }
            _ => {}
        }
    }

    /// Feeds the ledger what the actor just learned about an application's
    /// visibility, before the pure transition consumes the input.
    fn observe_visibility(&self, input: &Input) {
        let Some(visibility) = &self.visibility else { return };
        let observed = match input {
            Input::Lifecycle { kind, info } => match kind {
                LifecycleKind::Hidden => Some((info.application, Observation::Hidden)),
                LifecycleKind::Unhidden => Some((info.application, Observation::Shown)),
                LifecycleKind::Terminated => Some((info.application, Observation::Gone)),
                LifecycleKind::Launched | LifecycleKind::TerminatedUnresolved => None,
            },
            Input::Window(event) => match &event.kind {
                WindowEventKind::Application { application, change } => match change {
                    ApplicationChange::Hidden => Some((*application, Observation::Hidden)),
                    ApplicationChange::Shown => Some((*application, Observation::Shown)),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        };
        if let Some((application, observation)) = observed {
            visibility.observe(application, observation);
        }
    }

    /// Plans hides and unhides from the visibility requirement and queues them.
    /// Requirement-driven only: one request per application per turn, so a hide
    /// and an unhide of the same application never share a transition.
    /// Returns when the actor must wake to resolve a lost notification.
    fn reconcile_visibility(&self, now: Instant) -> Option<Instant> {
        let (Some(visibility), Some(state)) = (&self.visibility, &self.state) else {
            return None;
        };
        let stamp = state.stamp();
        let enabled = stamp.is_some() && state.frames.enabled;
        let requirement = visibility_requirement(state);
        let live = |application| state.apps.contains_key(&application);
        let planned = visibility.plan(&requirement, &live, enabled, now);
        let stamp = stamp?;
        self.diag.planned(state, &planned);
        for request in planned {
            let effect = match request {
                Planned::Hide(application) => Effect::HideApp { stamp, application },
                Planned::Unhide(application) => Effect::UnhideApp { stamp, application },
            };
            if let Err(error) = self.runner.submit(effect) {
                visibility.cancel_planned(request);
                if error == RunnerError::Closed {
                    let _ = self.ingress.submit(Input::Fault(HealthFault::Disconnected));
                }
            }
        }
        visibility.next_deadline(now)
    }

    fn dispatch(&self, effects: Vec<Effect>) {
        for effect in effects {
            if let Effect::CancelFrame { window } = effect {
                if let Some(frames) = &self.frames {
                    frames.cancel(window);
                }
                continue;
            }
            let Some(stamp) = effect_stamp(&effect) else { continue };
            if let (Effect::Focus { .. }, Some(visibility)) = (&effect, &self.visibility) {
                visibility.note_focus_dispatched();
            }
            let frame = match (&effect, &self.frames) {
                (Effect::WriteFrame { window, plan_sequence, .. }, Some(frames)) => {
                    frames.note_dispatched(*window, *plan_sequence);
                    Some((Arc::clone(frames), *window, *plan_sequence))
                }
                _ => None,
            };
            match self.runner.submit(effect.clone()) {
                Ok(()) => {}
                Err(error) => {
                    if let (Effect::Focus { .. }, Some(visibility)) = (&effect, &self.visibility) {
                        visibility.finish_focus();
                    }
                    if let Some((frames, window, plan)) = frame {
                        frames.note_not_queued(window, plan);
                    }
                    if error == RunnerError::Full {
                        // Reservation failure is a visible failure, never success.
                        let _ = self.ingress.submit(Input::Completion {
                            stamp,
                            result: CompletionResult::Rejected(effect),
                        });
                    } else {
                        let _ = self.ingress.submit(Input::Fault(HealthFault::Disconnected));
                    }
                }
            }
        }
    }

    fn retime(&mut self, now: Instant) {
        let Some(state) = &self.state else { return };
        let rates: BTreeMap<DisplayId, Option<f64>> =
            state.screens.iter().map(|screen| (screen.id, screen.refresh_hz)).collect();
        let same = rates.len() == self.rates.len()
            && rates.iter().all(|(id, hz)| {
                self.rates.get(id).is_some_and(|old| match (old, hz) {
                    (Some(a), Some(b)) => (a - b).abs() <= RETIME_EPSILON,
                    (a, b) => a == b,
                })
            });
        if same {
            return;
        }
        let periods: BTreeMap<DisplayId, Duration> =
            rates.iter().map(|(id, hz)| (*id, flush_period(*hz))).collect();
        self.rates = rates;
        self.ingress.with(|pipeline| {
            pipeline.geometry_mut().retime(periods, |token| state.display_of(token), now);
        });
    }

    fn publish_if_changed(&mut self, processed: usize) {
        let admission = self.ingress.stats();
        if processed == 0 && admission == self.last_admission {
            return;
        }
        let Some(state) = &self.state else { return };
        let mut snapshot = state.snapshot();
        snapshot.admission = admission.clone();
        if let Some(visibility) = &self.visibility {
            snapshot.hidden_apps = visibility.owned();
        }
        self.last_admission = admission;
        if let Some(borders) = &self.borders
            && let Some(focus) = border_focus(&snapshot)
        {
            borders.observe(focus);
        }
        self.cell.publish(snapshot);
    }

    /// After a panic the state is gone: keep the last safe snapshot, mark health
    /// failed and require a restart.
    fn fail_panicked(&mut self) {
        let mut snapshot = (*self.cell.load()).clone();
        snapshot.health = Health::Failed(HealthFault::Panicked);
        snapshot.phase = Phase::Incomplete;
        snapshot.actions_allowed = false;
        self.cell.publish(snapshot);
        self.halted = true;
        self.ingress.close();
    }
}

/// The pure border facts of a snapshot: the focused window's workspace layout
/// and whether the window floats. `None` without a focused tracked window.
fn border_focus(snapshot: &TilingSnapshot) -> Option<FocusInput> {
    let window = snapshot.focus.window?;
    let workspace = snapshot.focus.workspace?;
    let layout = snapshot.workspaces.iter().find(|candidate| candidate.id == workspace)?.layout;
    let floating = snapshot
        .windows
        .iter()
        .find(|candidate| candidate.token == window)
        .is_some_and(|candidate| candidate.desired.floating);
    Some(FocusInput { layout, floating })
}

/// Starts the actor thread. It exits on cancellation, when the engine halts, or
/// when the mailbox closes.
///
/// # Errors
///
/// The thread could not be spawned.
pub fn spawn_actor(mut engine: Engine, cancel: &CancellationToken) -> io::Result<JoinHandle<()>> {
    let ingress = Arc::clone(&engine.ingress);
    let guard_ingress = Arc::clone(&ingress);
    let cancel = cancel.clone();
    std::thread::Builder::new().name(ACTOR_THREAD_NAME.to_owned()).spawn(move || {
        // Cancellation sets the flag under the mailbox lock the waiter checks
        // before sleeping, so a cancel between check and sleep is not lost.
        let _listener = cancel.on_cancel(Arc::new(move || guard_ingress.shutdown()));
        loop {
            let turn = engine.turn();
            if turn.halted {
                break;
            }
            if ingress.wait(turn.next_deadline) == Wake::Shutdown {
                break;
            }
        }
        ingress.close();
    })
}
