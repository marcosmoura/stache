//! The read-only tracking harness.
//!
//! [`TrackingRuntime`] wires a [`WindowBackend`] to the actor: it installs one
//! backend epoch, starts the actor thread and a bounded read-only effect worker,
//! and exposes lifecycle/topology/config submission plus atomic snapshots. It is
//! not a production factory: nothing registers it. It moves no window until its
//! owner calls [`TrackingRuntime::authorize`], which publishes the action
//! authority; [`TrackingRuntime::seal`] withdraws it atomically and for good.
//! Only a runtime owner that supplied application actions can hide, unhide,
//! activate or raise a window, and only through the visibility control.

use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use stache_macos::applications::{LifecycleEvent, LifecycleKind, LifecycleSink};
use stache_macos::windows::{
    CleanupReport, EventQueue, GenerationCell, InstallError, RuntimeEpoch, StopOutcome,
};
use stache_macos::{DisplayLayout, TopologyHint};
use stache_sdk::CancellationToken;

use crate::actor::{
    AppDirectory, Clock, DrainOutcome, EffectEnv, EffectRunner, Engine, FrameControl,
    RUNNER_CAPACITY, SnapshotCell, SystemClock, ThreadRunner, join_within, spawn_actor,
};
use crate::animation::AnimationControl;
use crate::backend::WindowBackend;
use crate::borders::{BordersControl, BordersParts, FaultSink};
use crate::config::TilingConfig;
use crate::events::{Ingress, Pipeline, SubmitError};
use crate::state::{AppInfo, Health, Input, Stamp, TilingSnapshot, TilingState};
use crate::visibility::{ApplicationActions, Exec, RestoreOutcome, VisibilityControl};

/// Backend sink capacity.
pub const SINK_CAPACITY: usize = 1024;

/// One acquisition stage of a start, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StartStage {
    /// The backend epoch install.
    Install,
    /// The effect runner thread.
    Runner,
    /// The actor thread.
    Actor,
    /// The host reservation watcher (owned by the module).
    Watcher,
    /// The native display observer and worker (owned by the module).
    Displays,
    /// Queuing the initial enumeration.
    Enumeration,
    /// Publishing the action authority.
    Authorize,
}

/// A failpoint: returns `true` to make `stage` fail before it acquires anything.
pub type StageProbe = Arc<dyn Fn(StartStage) -> bool + Send + Sync>;

/// Harness options.
#[derive(Clone)]
pub struct TrackingOptions {
    /// Stache's own bundle identifier (never managed). Empty disables the check.
    pub self_bundle_id: String,
    /// The registry generation of this start; a restart passes a larger one.
    pub registry: u64,
    /// Backend sink capacity.
    pub sink_capacity: usize,
    /// Read-only effect queue capacity.
    pub runner_capacity: usize,
    /// Leave `Input::Start` (the initial enumeration) to [`TrackingRuntime::begin`].
    pub defer_begin: bool,
    /// An optional failpoint probed before each acquisition stage.
    pub probe: Option<StageProbe>,
    /// Native hide, unhide and activation for this run. `None` hides nothing
    /// and cannot activate a window for focus.
    pub actions: Option<Arc<ApplicationActions>>,
    /// The borders authority the host composed. `None` means an enabled
    /// configuration only reports a diagnostic and nothing is sent.
    pub borders: Option<BordersParts>,
    /// Receives the borders delivery fault (`None` clears it).
    pub borders_sink: Option<FaultSink>,
    /// The host logger, scoped to the module. `None` is silent.
    pub logger: Option<stache_sdk::ScopedLogger>,
}

impl std::fmt::Debug for TrackingOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackingOptions")
            .field("self_bundle_id", &self.self_bundle_id)
            .field("registry", &self.registry)
            .field("sink_capacity", &self.sink_capacity)
            .field("runner_capacity", &self.runner_capacity)
            .field("defer_begin", &self.defer_begin)
            .finish_non_exhaustive()
    }
}

impl TrackingOptions {
    /// Production bounds for `self_bundle_id`.
    #[must_use]
    pub fn new(self_bundle_id: impl Into<String>) -> Self {
        Self {
            self_bundle_id: self_bundle_id.into(),
            registry: 1,
            sink_capacity: SINK_CAPACITY,
            runner_capacity: RUNNER_CAPACITY,
            defer_begin: false,
            probe: None,
            actions: None,
            borders: None,
            borders_sink: None,
            logger: None,
        }
    }

    fn fails(&self, stage: StartStage) -> bool { self.probe.as_ref().is_some_and(|p| p(stage)) }
}

/// Why the harness could not start.
#[derive(Debug)]
pub enum StartError {
    /// The backend refused to install an epoch.
    Install(InstallError),
    /// A thread could not start.
    Thread(std::io::Error),
    /// A failpoint refused the stage.
    Injected(StartStage),
    /// The mailbox was already closed.
    Closed,
    /// The start failed and unwinding the backend epoch is incomplete: the
    /// epoch stays quarantined in the backend for `retry_cleanup`.
    Incomplete {
        /// The original failure.
        cause: Box<Self>,
        /// How the epoch stop ended.
        stop: StopOutcome,
    },
}

/// What the runtime has published to the issuance gate.
#[derive(Clone, Copy, Debug, Default)]
struct Authority {
    /// The gate is live for this runtime.
    published: bool,
    /// The newest accepted revision the gate holds.
    revision: u64,
}

/// A started harness.
pub struct TrackingRuntime {
    backend: Arc<dyn WindowBackend>,
    directory: Arc<dyn AppDirectory>,
    ingress: Arc<Ingress>,
    cell: Arc<SnapshotCell>,
    cancel: CancellationToken,
    runner: Arc<ThreadRunner>,
    gate: Arc<GenerationCell>,
    frames: Arc<FrameControl>,
    visibility: Arc<VisibilityControl>,
    animation: Arc<AnimationControl>,
    borders: Arc<BordersControl>,
    actions: Option<Arc<ApplicationActions>>,
    authority: Mutex<Authority>,
    epoch: RuntimeEpoch,
    stamp: Stamp,
    actor: std::sync::Mutex<Option<JoinHandle<()>>>,
    last_topology: std::sync::atomic::AtomicU64,
    probe: Option<StageProbe>,
    /// The activation watch; dropped with the runtime.
    frontmost: Mutex<Option<stache_macos::FrontmostWatch>>,
}

/// Resolves metadata for a launch and submits one lifecycle input. Runs on the
/// main thread, where the directory query is inline.
fn deliver_lifecycle(ingress: &Ingress, directory: &dyn AppDirectory, event: &LifecycleEvent) {
    // Only a launch needs metadata (a full application listing on the main
    // thread); the other kinds are keyed by the exact instance.
    let described = (event.kind == LifecycleKind::Launched)
        .then(|| directory.describe(event.application))
        .flatten();
    // A launch nobody can describe (an agent or accessory application, or one
    // that already exited) is not tracked.
    if described.is_none() && event.kind == LifecycleKind::Launched {
        return;
    }
    let info = described.unwrap_or_else(|| AppInfo {
        application: event.application,
        pid: event.pid.unwrap_or(0),
        bundle_id: String::new(),
        name: String::new(),
    });
    let _ = ingress.submit(Input::Lifecycle { kind: event.kind, info });
}

/// Feeds the frontmost application to the actor as a pure input.
struct ActivationFeed {
    ingress: Arc<Ingress>,
}

impl stache_macos::FrontmostListener for ActivationFeed {
    fn invalidated(&self, _epoch: u64) {}

    fn published(&self, snapshot: &stache_macos::FrontmostSnapshot) {
        if snapshot.is_confirmed()
            && !snapshot.is_current_process
            && let Some(application) = snapshot.application
        {
            let _ = self.ingress.submit(Input::Activated(application));
        }
    }
}

/// One acknowledgement hop (the finite bound of every cleanup wait).
pub const HOP: Duration = Duration::from_secs(2);

impl TrackingRuntime {
    /// Installs one backend epoch and starts the actor.
    ///
    /// # Errors
    ///
    /// [`StartError`] when the backend refuses or a thread cannot start.
    pub fn start(
        backend: Arc<dyn WindowBackend>,
        directory: Arc<dyn AppDirectory>,
        options: &TrackingOptions,
    ) -> Result<Self, StartError> {
        Self::start_with_clock(backend, directory, options, Arc::new(SystemClock), None)
    }

    /// [`Self::start`] with an explicit clock and an optional per-turn hook.
    ///
    /// # Errors
    ///
    /// [`StartError`] when the backend refuses or a thread cannot start.
    #[allow(clippy::too_many_lines)] // one linear staged start
    pub fn start_with_clock(
        backend: Arc<dyn WindowBackend>,
        directory: Arc<dyn AppDirectory>,
        options: &TrackingOptions,
        clock: Arc<dyn Clock>,
        hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<Self, StartError> {
        let ingress = Arc::new(Ingress::new(Pipeline::new()));
        let notify = Arc::clone(&ingress);
        let sink =
            EventQueue::with_notify(options.sink_capacity, Arc::new(move || notify.notify_sink()));
        // Not published until `authorize`: an unpublished gate accepts no
        // issuance, so nothing can move a window before the owner says Running.
        let gate = Arc::new(GenerationCell::new());
        let frames = Arc::new(FrameControl::new(Arc::clone(&gate), Arc::clone(&clock)));
        if options.fails(StartStage::Install) {
            return Err(StartError::Injected(StartStage::Install));
        }
        let epoch = backend
            .install(Arc::clone(&sink), Arc::clone(&gate), None)
            .map_err(StartError::Install)?;
        let stamp = Stamp {
            epoch,
            registry: options.registry,
        };
        let cancel = CancellationToken::new();
        // Exact unwind of every stage acquired so far: cancel and wake, close
        // the runner (idle, so the join is immediate) and stop the epoch. A
        // stop that is not clean leaves the epoch quarantined in the backend.
        let unwind = |cause: StartError, runner: Option<&ThreadRunner>| {
            cancel.cancel();
            ingress.shutdown();
            if let Some(runner) = runner {
                let _ = runner.close_within(HOP);
            }
            match backend.stop(epoch, &gate, Some(Instant::now() + HOP)) {
                StopOutcome::Clean { .. } | StopOutcome::NotCurrent => cause,
                stop => StartError::Incomplete { cause: Box::new(cause), stop },
            }
        };
        let visibility = Arc::new(VisibilityControl::new());
        let origin = crate::logclock::LogOrigin::new(Instant::now());
        // Inert: no worker, thread or timer until an enabled configuration.
        let animation = Arc::new(AnimationControl::live(
            Arc::clone(&backend),
            Arc::clone(&ingress),
            Arc::clone(&frames),
            Arc::clone(&clock),
            cancel.clone(),
        ));
        // Inert: no thread, port or child until an enabled configuration.
        let borders = Arc::new(BordersControl::new(
            options.borders.clone().map(|mut parts| {
                parts.logger.clone_from(&options.logger);
                parts.origin = origin;
                parts
            }),
            Arc::clone(&clock),
            options.borders_sink.clone().unwrap_or_else(|| Arc::new(|_| {})),
        ));
        let env = EffectEnv {
            backend: Arc::clone(&backend),
            directory: Arc::clone(&directory),
            ingress: Arc::clone(&ingress),
            cancel: cancel.clone(),
            frames: Arc::clone(&frames),
            visibility: Arc::clone(&visibility),
            actions: options.actions.clone(),
            animation: Arc::clone(&animation),
        };
        if options.fails(StartStage::Runner) {
            return Err(unwind(StartError::Injected(StartStage::Runner), None));
        }
        let runner = match ThreadRunner::spawn(env, options.runner_capacity) {
            Ok(runner) => Arc::new(runner),
            Err(error) => return Err(unwind(StartError::Thread(error), None)),
        };
        let cell = Arc::new(SnapshotCell::default());
        let mut initial = TilingState::new(options.self_bundle_id.clone());
        initial.set_tab_reads(backend.reads_tabs());
        let mut engine = Engine::new(
            initial,
            Arc::clone(&ingress),
            Arc::clone(&cell),
            Arc::clone(&runner) as Arc<dyn EffectRunner>,
            sink,
            clock,
        );
        engine.set_frame_control(Arc::clone(&frames));
        engine.set_visibility_control(Arc::clone(&visibility));
        engine.set_logger(options.logger.clone(), origin);
        visibility.set_logger(options.logger.clone(), origin);
        engine.set_borders_control(Arc::clone(&borders));
        if let Some(hook) = hook {
            engine.set_turn_hook(hook);
        }
        if options.fails(StartStage::Actor) {
            return Err(unwind(StartError::Injected(StartStage::Actor), Some(&runner)));
        }
        let actor = match spawn_actor(engine, &cancel) {
            Ok(actor) => actor,
            Err(error) => return Err(unwind(StartError::Thread(error), Some(&runner))),
        };
        let runtime = Self {
            backend,
            directory,
            ingress,
            cell,
            cancel,
            runner,
            gate,
            frames,
            visibility,
            animation,
            borders,
            actions: options.actions.clone(),
            authority: Mutex::new(Authority::default()),
            epoch,
            stamp,
            actor: std::sync::Mutex::new(Some(actor)),
            last_topology: std::sync::atomic::AtomicU64::new(0),
            probe: options.probe.clone(),
            frontmost: Mutex::new(None),
        };
        if !options.defer_begin {
            let _ = runtime.ingress.submit(Input::Start(stamp));
        }
        Ok(runtime)
    }

    /// Queues the initial enumeration (`Input::Start`). Only meaningful with
    /// [`TrackingOptions::defer_begin`]; it returns immediately and waits for
    /// no window.
    ///
    /// # Errors
    ///
    /// A failpoint refused the stage or the mailbox is closed.
    pub fn begin(&self) -> Result<(), StartError> {
        if self.probe.as_ref().is_some_and(|p| p(StartStage::Enumeration)) {
            return Err(StartError::Injected(StartStage::Enumeration));
        }
        self.ingress.submit(Input::Start(self.stamp)).map_err(|_| StartError::Closed)
    }

    /// Starts feeding application lifecycle (launch, terminate, hide, unhide)
    /// and the frontmost application to the actor. Call after [`Self::begin`]
    /// so a registry restart cannot discard the first activation. Without
    /// application actions there is nothing to watch.
    ///
    /// # Errors
    ///
    /// Which watch could not be installed. The runtime keeps working with the
    /// window events alone (new applications and Cmd-Tab are then not seen).
    pub fn watch_applications(&self) -> Result<(), String> {
        let Some(actions) = &self.actions else {
            return Ok(());
        };
        // Weak: the native observer keeps this sink for as long as the
        // observation lives, and the directory owns that observation.
        let ingress = Arc::clone(&self.ingress);
        let directory = Arc::downgrade(&self.directory);
        let lifecycle = actions
            .watch_lifecycle(&self.cancel, move |event| {
                if let Some(directory) = directory.upgrade() {
                    deliver_lifecycle(&ingress, &*directory, &event);
                }
            })
            .map_err(|reason| format!("application lifecycle: {reason}"));
        let feed = Arc::new(ActivationFeed {
            ingress: Arc::clone(&self.ingress),
        });
        let frontmost = actions
            .watch_frontmost(&self.cancel, feed)
            .map(|watch| {
                *self.frontmost.lock().unwrap_or_else(PoisonError::into_inner) = Some(watch);
            })
            .map_err(|reason| format!("frontmost application: {reason}"));
        // Anything launched between `begin` (the first list) and the lifecycle
        // watch being live was never reported: list once more now.
        let _ = self.ingress.submit(Input::Relist);
        match (lifecycle, frontmost) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(a), Ok(())) | (Ok(()), Err(a)) => Err(a),
            (Err(a), Err(b)) => Err(format!("{a}; {b}")),
        }
    }

    /// The registry stamp events and completions must carry.
    #[must_use]
    pub const fn stamp(&self) -> Stamp { self.stamp }

    /// The backend epoch.
    #[must_use]
    pub const fn epoch(&self) -> RuntimeEpoch { self.epoch }

    /// One atomic snapshot of everything.
    #[must_use]
    pub fn snapshot(&self) -> Arc<TilingSnapshot> { self.cell.load() }

    /// Health from the last published snapshot.
    #[must_use]
    pub fn health(&self) -> Health { self.cell.load().health }

    /// The mailbox (tests and later parts submit through it).
    #[must_use]
    pub const fn ingress(&self) -> &Arc<Ingress> { &self.ingress }

    /// A nonblocking lifecycle sink for the applications adapter.
    #[must_use]
    pub fn lifecycle_sink(&self) -> LifecycleSink {
        let ingress = Arc::clone(&self.ingress);
        let directory = Arc::clone(&self.directory);
        Arc::new(move |event: LifecycleEvent| deliver_lifecycle(&ingress, &*directory, &event))
    }

    /// Submits a display layout answering a topology hint. A hint older than one
    /// already applied is dropped (`Ok(false)`).
    ///
    /// # Errors
    ///
    /// [`SubmitError::Closed`] after the actor stopped.
    pub fn submit_displays(
        &self,
        hint: Option<TopologyHint>,
        layout: DisplayLayout,
    ) -> Result<bool, SubmitError> {
        use std::sync::atomic::Ordering;
        if let Some(hint) = hint
            && self.last_topology.fetch_max(hint.sequence, Ordering::SeqCst) >= hint.sequence
        {
            return Ok(false);
        }
        self.ingress.submit(Input::Displays(Arc::new(layout))).map(|()| true)
    }

    /// Why animations are unavailable for the adopted configuration, if their
    /// worker could not be spawned.
    #[must_use]
    pub fn animation_failure(&self) -> Option<String> { self.animation.spawn_failure() }

    /// Submits an accepted configuration revision.
    ///
    /// While the authority is published the gate adopts the revision first, so
    /// a write planned under an older revision is refused at the native send
    /// before the actor even sees the new configuration.
    ///
    /// # Errors
    ///
    /// [`SubmitError::Closed`] after the actor stopped.
    pub fn submit_config(
        &self,
        revision: u64,
        config: Arc<TilingConfig>,
    ) -> Result<(), SubmitError> {
        // The animation worker follows the accepted configuration: spawned for
        // an enabled one, retired for a disabled one. A spawn failure leaves
        // writes direct (never a lost frame) and is reported through
        // `animation_failure`.
        if !self.gate.is_sealed() {
            let _ = self.animation.configure(&config.animations);
            self.borders.configure(&config);
        }
        {
            let mut authority = self.authority.lock().unwrap_or_else(PoisonError::into_inner);
            if authority.published && revision > authority.revision {
                authority.revision = revision;
                drop(authority);
                self.gate.accept_revision(revision);
            }
        }
        self.ingress.submit(Input::Config { revision, config })
    }

    /// Publishes the action authority for `revision` and lets the actor plan
    /// frame writes. Call it only while the module is Running, after the
    /// matching [`Self::submit_config`].
    ///
    /// Returns `false` (and publishes nothing) once the runtime is sealed.
    pub fn authorize(&self, revision: u64) -> bool {
        if self.probe.as_ref().is_some_and(|p| p(StartStage::Authorize)) {
            return false;
        }
        let mut authority = self.authority.lock().unwrap_or_else(PoisonError::into_inner);
        if self.gate.is_sealed() {
            return false;
        }
        authority.published = true;
        authority.revision = revision;
        self.gate.publish(self.stamp.registry, revision);
        drop(authority);
        self.ingress.submit(Input::FrameWrites(true)).is_ok()
    }

    /// Withdraws the authority for good: one atomic store, independent of any
    /// stalled native send. Queued frame writes become inert at once; a send
    /// already issued cannot be undone and is accounted by [`Self::drain_frames`].
    pub fn seal(&self) {
        self.gate.seal();
        self.visibility.seal();
        self.authority.lock().unwrap_or_else(PoisonError::into_inner).published = false;
        let _ = self.ingress.submit(Input::FrameWrites(false));
    }

    /// Whether the authority is published and not sealed.
    #[must_use]
    pub fn is_authorized(&self) -> bool {
        !self.gate.is_sealed()
            && self.authority.lock().unwrap_or_else(PoisonError::into_inner).published
    }

    /// Frame writes queued or running.
    #[must_use]
    pub fn outstanding_frames(&self) -> usize { self.frames.outstanding() }

    /// Waits at most `timeout` for outstanding frame writes. Cancellation of
    /// `cancel` wakes it. Never holds a lock across the wait.
    #[must_use]
    pub fn drain_frames(&self, timeout: Duration, cancel: &CancellationToken) -> DrainOutcome {
        self.frames.drain(timeout, cancel)
    }

    /// The animation worker handle.
    #[must_use]
    pub const fn animation(&self) -> &Arc<AnimationControl> { &self.animation }

    /// Closes the animation worker for good and joins it within `hop`; a worker
    /// stuck in a native send keeps its handle for a retry. `true` when joined.
    /// Runs after the frame drain and before the restore, so no animated frame
    /// can follow a restoration.
    #[must_use]
    pub fn stop_animation(&self, hop: Duration) -> bool {
        self.animation.stop();
        self.animation.join_within(hop)
    }

    /// The borders runner handle.
    #[must_use]
    pub const fn borders(&self) -> &Arc<BordersControl> { &self.borders }

    /// Closes the borders runner for good (it sends nothing and the service
    /// keeps its last settings) and joins it within `hop`, reaping any helper
    /// child it spawned. A runner blocked in a send keeps its handle for a
    /// retry. `true` when nothing is left.
    #[must_use]
    pub fn stop_borders(&self, hop: Duration) -> bool {
        self.borders.stop();
        self.borders.join_within(hop)
    }

    /// The visibility control: the ledger and the restore.
    #[must_use]
    pub const fn visibility(&self) -> &Arc<VisibilityControl> { &self.visibility }

    /// The live applications as this run's observation sees them. Empty when
    /// there are no application actions or the query failed, which callers must
    /// treat as unknown rather than as "nothing is running".
    #[must_use]
    pub fn running_applications(&self) -> Vec<stache_macos::FrontmostApplication> {
        self.actions
            .as_deref()
            .map(ApplicationActions::running_details)
            .unwrap_or_default()
    }

    /// Restores what the runtime hid, after the frame drain. Uses a fresh
    /// cancellation token and its own authorizations, because the runtime's
    /// token and the frame gate are already sealed. Retrying re-runs only the
    /// records that remain.
    #[must_use]
    pub fn restore_visibility(&self, hop: Duration) -> RestoreOutcome {
        let cancel = CancellationToken::new();
        self.visibility.restore(
            &Exec {
                actions: self.actions.as_deref(),
                backend: &*self.backend,
                epoch: self.epoch,
                cancel: &cancel,
            },
            hop,
        )
    }

    /// Seals the authority, cancels the actor and effect worker and wakes the
    /// mailbox. Waits for nothing; idempotent.
    pub fn quiesce(&self) {
        self.seal();
        self.borders.stop();
        self.cancel.cancel();
        self.ingress.shutdown();
    }

    /// Stops the backend epoch with acknowledgement, at most `hop` long.
    pub fn stop_epoch(&self, hop: Duration) -> StopOutcome {
        self.backend.stop(self.epoch, &self.gate, Instant::now().checked_add(hop))
    }

    /// Retries quarantined backend removals exactly once each, at most `hop`
    /// long under a fresh token. `None` when the owner is unavailable.
    pub fn retry_epoch_cleanup(&self, hop: Duration) -> Option<CleanupReport> {
        self.backend
            .retry_cleanup(&CancellationToken::new(), Instant::now().checked_add(hop))
    }

    /// Joins the actor if it ends within `hop`; its handle is kept otherwise.
    #[must_use]
    pub fn join_actor(&self, hop: Duration) -> bool { join_within(&self.actor, hop) }

    /// Closes the effect runner, joining its worker within `hop`; a worker
    /// stuck in a native send keeps its handle for a retry.
    #[must_use]
    pub fn close_runner(&self, hop: Duration) -> bool { self.runner.close_within(hop) }

    /// The published snapshot cell (a cheap, lock-light read source).
    #[must_use]
    pub fn snapshot_cell(&self) -> Arc<SnapshotCell> { Arc::clone(&self.cell) }

    /// Cancels the actor and effect worker, joins them and stops the epoch.
    pub fn shutdown(&self) -> StopOutcome {
        self.seal();
        self.cancel.cancel();
        self.ingress.shutdown();
        let actor = self.actor.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(actor) = actor {
            let _ = actor.join();
        }
        self.animation.stop();
        let _ = self.animation.join_within(HOP);
        let _ = self.stop_borders(crate::borders::JOIN_BOUND);
        self.runner.close();
        self.backend.stop(self.epoch, &self.gate, None)
    }
}
