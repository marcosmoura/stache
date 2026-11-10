//! The Bar runtime: one panel, one reservation, one display feed lease.
//!
//! All state lives in [`Core`] behind one short mutex that is held only across
//! logical bookkeeping and queue-only SDK calls (open, reposition, close,
//! reservation replace); it is never held across a wait, a join or a main
//! thread hop. The panel load wait runs outside it and rechecks the exact
//! runtime and window identity afterwards, so a late callback can neither
//! resurrect nor close a successor.
//!
//! # Convergence
//!
//! Start, reconfigure and every display change run the same idempotent
//! [`Inner::converge`]: compute the one desired state from the configuration
//! and the latest [`TopologyReading`], then do the smallest change that makes
//! the panel and the reservation match. An unchanged reload therefore does
//! nothing, a height or padding change repositions in place and replaces the
//! reservation, and only entering or leaving the zero-size / no-primary states
//! opens or closes a window.
//!
//! # Time bounds
//!
//! `start` and `reconfigure` never wait for a page: they queue the open, stage
//! the reservation and return, so a page that never loads cannot hold the
//! registry's lifecycle gate. Bar's own panel worker observes the load off the
//! main thread with no lock held, bounded by [`PANEL_LOAD_DEADLINE`], and woken
//! by cancellation or by any new convergence. A failed panel is closed, its
//! reservation cleared, and it is retried with bounded backoff
//! ([`PANEL_RETRY_BACKOFF`]) and immediately on the next convergence trigger.
//!
//! The idle waits of the two workers are woken by any cancellation; the slice
//! only bounds a missed wake.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::MenuBarControl;
use stache_sdk::{
    CancellationToken, DisplayId, EdgeInsets, OwnedWindow, PhaseWait, ScreenReservationError,
    ScreenReservations, SurfaceId, TopologyGeneration, WindowCapability, WindowFrame, WindowKey,
    WindowKind, WindowOrigin, WindowPhase, WindowRequest, WindowSize, WindowWatch,
};

use crate::config::BarConfig;
use crate::menubar::{MENU_JOIN, MENU_POLL, MENU_RETRY, MenuMonitor};
use crate::providers::{ProviderMonitor, ProviderParts, PublishMedia, PublishSample};
use crate::status::{BarFrameView, BarPanelState, BarReservationView, BarStatusSource};
use crate::topology::{TopologyLease, TopologyReading, TopologySource, TopologyWait};
use crate::window::{Desired, PanelFrame, plan};

/// The surface the panel loads.
pub const BAR_SURFACE: SurfaceId = SurfaceId::new("bar");

/// The longest wait for a requested panel to become created and then loaded.
///
/// The wait runs off the main thread. Expiry clears the reservation, closes the
/// exact window and exposes a typed failure.
pub const PANEL_LOAD_DEADLINE: Duration = Duration::from_secs(10);

/// The delays before the automatic retries of a failed panel: three attempts,
/// each a fresh exact window. After the last one the panel stays failed until
/// the next convergence trigger (accepted config change, topology or primary
/// change) or a pause/resume.
pub const PANEL_RETRY_BACKOFF: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

/// The idle wait of the display-follow worker between wake-ups. Cancellation
/// wakes it at once; the slice only bounds a missed wake.
const FOLLOW_IDLE: Duration = Duration::from_secs(30);

/// Bounds and seams the host injects.
#[derive(Clone)]
pub struct BarParts {
    /// The Bar-scoped display feed.
    pub topology: Arc<dyn TopologySource>,
    /// The menu-bar adapter. `None` leaves the menu state unobserved
    /// (`unknown`); the production composition always supplies one.
    pub menu: Option<MenuBarControl>,
    /// The CPU, battery and Wi-Fi providers. `None` runs none (the default,
    /// so a runtime built without them owns no provider thread).
    pub providers: Option<ProviderParts>,
    /// Time bounds.
    pub timing: BarTiming,
}

impl BarParts {
    /// Parts with the production time bounds and no menu-bar adapter.
    #[must_use]
    pub fn new(topology: Arc<dyn TopologySource>) -> Self {
        Self {
            topology,
            menu: None,
            providers: None,
            timing: BarTiming::default(),
        }
    }

    /// The same parts running the system providers with `providers`.
    #[must_use]
    pub fn with_providers(mut self, providers: ProviderParts) -> Self {
        self.providers = Some(providers);
        self
    }

    /// The same parts observing the menu bar through `menu`.
    #[must_use]
    pub fn with_menu(mut self, menu: MenuBarControl) -> Self {
        self.menu = Some(menu);
        self
    }
}

/// Time bounds, injectable so tests do not wait for production deadlines.
#[derive(Clone, Copy, Debug)]
pub struct BarTiming {
    /// Open to page-loaded.
    pub load_deadline: Duration,
    /// Delay before each automatic retry of a failed panel.
    pub retry_backoff: [Duration; 3],
    /// The menu-bar poll period, used only while auto-hide is on or unknown.
    pub menu_poll: Duration,
    /// The delay before a failed menu-bar sample is retried.
    pub menu_retry: Duration,
    /// The longest the menu-bar worker is joined at stop.
    pub menu_join: Duration,
}

impl Default for BarTiming {
    fn default() -> Self {
        Self {
            load_deadline: PANEL_LOAD_DEADLINE,
            retry_backoff: PANEL_RETRY_BACKOFF,
            menu_poll: MENU_POLL,
            menu_retry: MENU_RETRY,
            menu_join: MENU_JOIN,
        }
    }
}

pub struct Panel {
    window: OwnedWindow,
    watch: WindowWatch,
    frame: PanelFrame,
    opened: Instant,
    loaded: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Staged {
    display: DisplayId,
    top: u32,
    topology: TopologyGeneration,
}

/// Everything one started runtime owns.
pub struct Run {
    pub id: u64,
    pub windows: WindowCapability,
    pub reservations: ScreenReservations,
    pub lease: Arc<dyn TopologyLease>,
    panel: Option<Panel>,
    staged: Option<Staged>,
    /// Consecutive failed panels since the last convergence trigger.
    failures: usize,
    /// When the panel worker retries the failed panel; `None` once exhausted.
    retry_due: Option<Instant>,
    /// Set by the panel worker around its own retry so it keeps the count.
    auto_retry: bool,
    /// Wakes the panel worker's current wait.
    wake: Option<CancellationToken>,
}

#[derive(Default)]
pub struct Core {
    pub config: Option<BarConfig>,
    pub run: Option<Run>,
    /// A lease whose `end` did not finish: retained, retried by stop.
    pub lingering: Option<Arc<dyn TopologyLease>>,
}

/// What the panel worker does next.
enum Next {
    /// Observe the exact window until loaded, failed or `deadline`.
    Load {
        watch: WindowWatch,
        deadline: Instant,
    },
    /// Wait until `due`, then retry the failed panel.
    Retry { due: Instant },
    /// Nothing is pending.
    Idle,
}

pub enum ConvergeError {
    /// The staging named a topology other than the registry's. Re-read and
    /// retry once per topology change.
    Stale,
    Failed(String),
}

/// The menu-bar monitors. Locked after `core`, never the other way round, and
/// never across a join: only bookkeeping happens under it.
#[derive(Default)]
pub struct Menus {
    /// The monitor of the live panel, with its runtime generation.
    active: Option<(u64, Arc<MenuMonitor>)>,
    /// Monitors asked to stop (or whose end is still owed).
    retiring: Vec<Arc<MenuMonitor>>,
}

/// The provider monitors, bookkeeping only under the lock (same rules as
/// [`Menus`]).
#[derive(Default)]
pub struct Providers {
    /// The monitor of the live panel, with its runtime generation.
    active: Option<(u64, Arc<ProviderMonitor>)>,
    /// Monitors asked to stop (or whose end is still owed).
    retiring: Vec<Arc<ProviderMonitor>>,
}

pub struct Inner {
    pub parts: BarParts,
    pub core: Mutex<Core>,
    pub menus: Mutex<Menus>,
    pub providers: Mutex<Providers>,
    pub status: BarStatusSource,
    pub shutdown: CancellationToken,
    pub closing: AtomicBool,
    next_run: AtomicU64,
    /// Identifies provider monitors (not runtime generations).
    next_epoch: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

const fn view(frame: PanelFrame) -> BarFrameView {
    BarFrameView {
        x: frame.x,
        y: frame.y,
        width: frame.width,
        height: frame.height,
    }
}

fn window_frame(frame: PanelFrame) -> Result<WindowFrame, String> {
    let size = WindowSize::new(frame.width, frame.height).map_err(|error| error.to_string())?;
    Ok(WindowFrame {
        origin: WindowOrigin { x: frame.x, y: frame.y },
        size,
    })
}

impl Inner {
    pub(crate) fn new(parts: BarParts, status: BarStatusSource) -> Self {
        Self {
            parts,
            core: Mutex::new(Core::default()),
            menus: Mutex::new(Menus::default()),
            providers: Mutex::new(Providers::default()),
            status,
            shutdown: CancellationToken::new(),
            closing: AtomicBool::new(false),
            next_run: AtomicU64::new(1),
            next_epoch: AtomicU64::new(1),
        }
    }

    pub(crate) fn next_run(&self) -> u64 { self.next_run.fetch_add(1, Ordering::SeqCst) }

    /// Removes the reservation and releases the panel lease (queue-only). The
    /// exact window identity stays observable until the host acknowledges it.
    fn retire_panel(&self, run: &mut Run) {
        run.reservations.remove();
        run.staged = None;
        self.status.reservation(None);
        self.want_menu(run.id, false);
        self.want_providers(run.id, false);
        if let Some(panel) = run.panel.take() {
            self.status.closing(panel.watch.clone());
            drop(panel.window);
        }
    }

    /// Starts or stops the menu-bar observation to match "Bar runs with a
    /// panel". Never blocks: starting only spawns the worker (it registers and
    /// seeds off this thread); stopping only asks it to stop. The join and any
    /// owed native removal belong to [`teardown`](Self::teardown).
    fn want_menu(&self, run_id: u64, wanted: bool) {
        let Some(control) = self.parts.menu.clone() else {
            return;
        };
        let mut menus = lock(&self.menus);
        menus.retiring.retain(|monitor| !monitor.reap());
        if wanted {
            if menus.active.as_ref().is_some_and(|(id, _)| *id == run_id)
                || self.closing.load(Ordering::SeqCst)
            {
                return;
            }
            let status = self.status.clone();
            let publish = Arc::new(move |observation| status.menu(run_id, observation));
            let timing = self.parts.timing;
            match MenuMonitor::start(control, publish, timing.menu_poll, timing.menu_retry) {
                Ok(monitor) => menus.active = Some((run_id, Arc::new(monitor))),
                Err(reason) => {
                    drop(menus);
                    self.status.menu(run_id, crate::status::MenuBarObservation {
                        state: crate::status::MenuBarState::Unknown,
                        reason: Some(reason),
                    });
                }
            }
        } else if let Some((_, monitor)) = menus.active.take() {
            monitor.request_stop();
            menus.retiring.push(monitor);
            drop(menus);
            self.status.menu_off();
        }
    }

    /// Starts or stops the CPU, battery and Wi-Fi providers to match "Bar runs
    /// with a panel". Never blocks: starting only spawns the workers (every
    /// read happens on them); stopping only asks them to stop and detaches
    /// their publication. The join belongs to [`teardown`](Self::teardown).
    fn want_providers(&self, run_id: u64, wanted: bool) {
        let Some(parts) = self.parts.providers.clone() else {
            return;
        };
        let mut slots = lock(&self.providers);
        slots.retiring.retain(|monitor| !monitor.reap());
        if wanted {
            if slots.active.as_ref().is_some_and(|(id, _)| *id == run_id)
                || self.closing.load(Ordering::SeqCst)
            {
                return;
            }
            let epoch = self.next_epoch.fetch_add(1, Ordering::SeqCst);
            if !self.status.providers_on(epoch) {
                return;
            }
            let status = self.status.clone();
            let publish: PublishSample = Arc::new(move |kind, update, diagnostic| {
                status.provider_sample(epoch, kind, update, diagnostic);
            });
            let status = self.status.clone();
            let publish_media: PublishMedia =
                Arc::new(move |update| status.provider_media(epoch, update));
            match ProviderMonitor::start(&parts, &publish, &publish_media) {
                Ok(monitor) => slots.active = Some((run_id, Arc::new(monitor))),
                Err(reason) => {
                    drop(slots);
                    self.status.providers_failed(&reason);
                }
            }
        } else if let Some((_, monitor)) = slots.active.take() {
            // Detaches publication first, then asks the workers to stop:
            // nothing a worker (or the media child it is about to kill) finishes
            // later can reach the cell. The status lock is a leaf, so holding
            // the slots lock across it cannot deadlock.
            self.status.providers_off();
            monitor.request_stop();
            slots.retiring.push(monitor);
        }
    }

    /// Ends every provider monitor, joining within the bound. Returns whether
    /// nothing is owed; what is owed stays for an exact retry.
    fn end_providers(&self) -> bool {
        self.status.providers_off();
        let monitors: Vec<Arc<ProviderMonitor>> = {
            let mut slots = lock(&self.providers);
            slots
                .active
                .take()
                .into_iter()
                .map(|(_, monitor)| monitor)
                .chain(slots.retiring.drain(..))
                .collect()
        };
        let join = self.parts.providers.as_ref().map_or(Duration::ZERO, |parts| parts.timing.join);
        let owed: Vec<Arc<ProviderMonitor>> =
            monitors.into_iter().filter(|monitor| !monitor.end(join)).collect();
        let finished = owed.is_empty();
        if !finished {
            lock(&self.providers).retiring.extend(owed);
        }
        finished
    }

    /// Whether a provider monitor's end is still owed.
    pub(crate) fn providers_owed(&self) -> bool { !lock(&self.providers).retiring.is_empty() }

    /// Ends every menu monitor, joining within the bound. Returns whether
    /// nothing is owed; what is owed stays for an exact retry.
    fn end_menus(&self) -> bool {
        let monitors: Vec<Arc<MenuMonitor>> = {
            let mut menus = lock(&self.menus);
            menus
                .active
                .take()
                .into_iter()
                .map(|(_, monitor)| monitor)
                .chain(menus.retiring.drain(..))
                .collect()
        };
        let join = self.parts.timing.menu_join;
        let owed: Vec<Arc<MenuMonitor>> =
            monitors.into_iter().filter(|monitor| !monitor.end(join)).collect();
        let finished = owed.is_empty();
        if !finished {
            lock(&self.menus).retiring.extend(owed);
        }
        self.status.menu_off();
        finished
    }

    /// Whether a menu monitor's end is still owed.
    pub(crate) fn menus_owed(&self) -> bool { !lock(&self.menus).retiring.is_empty() }

    /// Makes the panel and the reservation match `config` on `reading`.
    ///
    /// Never waits: a newly opened panel is observed by the panel worker.
    pub(crate) fn converge(
        &self,
        run: &mut Run,
        config: &BarConfig,
        reading: &TopologyReading,
    ) -> Result<(), ConvergeError> {
        let topology = reading.generation.get();
        match plan(config, reading.layout.as_deref(), reading.primary) {
            Desired::ZeroSize => {
                self.retire_panel(run);
                self.status.no_panel(BarPanelState::NoPanelZeroSize, None, topology);
                Ok(())
            }
            Desired::NoPrimary => {
                self.retire_panel(run);
                self.status.no_panel(
                    BarPanelState::NoPrimaryDisplay,
                    reading.error.clone(),
                    topology,
                );
                Ok(())
            }
            Desired::Invalid(reason) => {
                self.retire_panel(run);
                self.status.panel_failed(reason.clone());
                Err(ConvergeError::Failed(reason))
            }
            Desired::Panel {
                display,
                frame,
                reservation_top,
            } => {
                if let Some(panel) = run.panel.as_mut() {
                    if panel.frame != frame {
                        let moved = window_frame(frame).and_then(|target| {
                            panel.window.reposition(target).map_err(|error| error.to_string())
                        });
                        if let Err(reason) = moved {
                            return Err(ConvergeError::Failed(format!(
                                "the panel could not be moved: {reason}"
                            )));
                        }
                        panel.frame = frame;
                    }
                } else {
                    // A convergence trigger restarts the retry budget; the panel
                    // worker's own retry keeps counting.
                    if !std::mem::take(&mut run.auto_retry) {
                        run.failures = 0;
                    }
                    run.retry_due = None;
                    let panel = Self::open(&run.windows, frame).map_err(ConvergeError::Failed)?;
                    run.panel = Some(panel);
                    run.kick();
                }
                if let Some(panel) = run.panel.as_ref() {
                    self.status.panel_placed(panel.watch.id().get(), view(frame), topology);
                    self.want_menu(run.id, true);
                    self.want_providers(run.id, true);
                }
                self.stage(run, display, reservation_top, reading.generation)?;
                Ok(())
            }
        }
    }

    fn open(windows: &WindowCapability, frame: PanelFrame) -> Result<Panel, String> {
        let key = WindowKey::new("bar").map_err(|error| error.to_string())?;
        let size = WindowSize::new(frame.width, frame.height).map_err(|error| error.to_string())?;
        let request = WindowRequest::new(key, BAR_SURFACE, size, WindowKind::NonActivatingPanel)
            .with_origin(WindowOrigin { x: frame.x, y: frame.y });
        let window = windows
            .request(&request)
            .map_err(|error| format!("the panel could not be opened: {error}"))?;
        let watch = window.watch();
        Ok(Panel {
            window,
            watch,
            frame,
            opened: Instant::now(),
            loaded: false,
        })
    }

    fn stage(
        &self,
        run: &mut Run,
        display: DisplayId,
        top: f64,
        generation: TopologyGeneration,
    ) -> Result<(), ConvergeError> {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // sum of two u16
        let rounded = top as u32;
        let wanted = Staged {
            display,
            top: rounded,
            topology: generation,
        };
        if run.staged == Some(wanted) {
            return Ok(());
        }
        let insets = EdgeInsets::new(top, 0.0, 0.0, 0.0)
            .map_err(|error| ConvergeError::Failed(error.to_string()))?;
        match run.reservations.replace(generation, &[(display, insets)]) {
            Ok(_) => {
                run.staged = Some(wanted);
                self.status.reservation(Some(BarReservationView {
                    display: display.raw(),
                    top: rounded,
                }));
                Ok(())
            }
            Err(
                ScreenReservationError::StaleTopology { .. }
                | ScreenReservationError::UnknownDisplay { .. },
            ) => Err(ConvergeError::Stale),
            Err(error) => Err(ConvergeError::Failed(format!(
                "the screen reservation was refused: {error}"
            ))),
        }
    }

    /// [`converge`](Self::converge), retried once against a fresher reading
    /// when the staging named a stale topology. Never loops: one retry per
    /// call, and only when the feed really moved on.
    pub(crate) fn converge_fresh(
        &self,
        run: &mut Run,
        config: &BarConfig,
        reading: &TopologyReading,
    ) -> Result<(), String> {
        match self.converge(run, config, reading) {
            Ok(()) => Ok(()),
            Err(ConvergeError::Failed(reason)) => Err(reason),
            Err(ConvergeError::Stale) => {
                let fresh = run.lease.read();
                if fresh.revision == reading.revision {
                    return Err("the screen reservation named a stale topology and the \
                                display feed has no newer one"
                        .to_owned());
                }
                match self.converge(run, config, &fresh) {
                    Ok(()) => Ok(()),
                    Err(ConvergeError::Failed(reason)) => Err(reason),
                    Err(ConvergeError::Stale) => {
                        Err("the screen reservation named a stale topology again".to_owned())
                    }
                }
            }
        }
    }

    /// Waits, off the main thread and outside every lock, until the exact
    /// window is page-loaded, failed, closed, past `deadline` or `wake` fires.
    fn await_loaded(
        &self,
        watch: &WindowWatch,
        deadline: Instant,
        wake: &CancellationToken,
    ) -> Result<(), String> {
        let mut snapshot = watch.phase();
        loop {
            self.status.phase(watch.id().get(), &snapshot.phase);
            match &snapshot.phase {
                WindowPhase::Loaded => return Ok(()),
                WindowPhase::Failed(reason) => {
                    return Err(format!("the panel failed: {reason}"));
                }
                WindowPhase::Closing | WindowPhase::Closed => {
                    return Err("the panel closed before it loaded".to_owned());
                }
                WindowPhase::Requested | WindowPhase::Created => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(format!(
                    "the panel did not load within {} seconds ({})",
                    self.parts.timing.load_deadline.as_secs(),
                    match snapshot.phase {
                        WindowPhase::Requested => "never created",
                        _ => "created, page never loaded",
                    }
                ));
            }
            match watch.wait_change(snapshot.revision, deadline - now, &[wake]) {
                PhaseWait::Changed(next) => snapshot = next,
                // The loop head rechecks the latest phase against the deadline.
                PhaseWait::TimedOut(_) => snapshot = watch.phase(),
                PhaseWait::Cancelled => return Err(CANCELLED.to_owned()),
            }
        }
    }

    /// Records a failed panel: it is gone (exact window closed, reservation
    /// cleared), the reason is typed, and the next automatic retry is due.
    /// The display feed stays applied: Bar is still running.
    fn note_failure(&self, run: &mut Run, reason: String) {
        self.retire_panel(run);
        self.status.panel_failed(reason);
        run.failures += 1;
        run.retry_due = run
            .failures
            .checked_sub(1)
            .and_then(|index| self.parts.timing.retry_backoff.get(index))
            .map(|delay| Instant::now() + *delay);
        run.kick();
    }

    /// Commits the outcome of a load wait for the exact run and window.
    ///
    /// A failure clears the reservation, releases (closes) the exact window and
    /// exposes the typed reason. A superseded run or window changes nothing.
    #[allow(clippy::significant_drop_tightening)] // the guard covers the identity check
    fn settle(&self, run_id: u64, window: &WindowWatch, outcome: Result<(), String>) {
        let mut core = lock(&self.core);
        let Some(run) = core.run.as_mut().filter(|run| run.id == run_id) else {
            return;
        };
        let Some(panel) = run.panel.as_mut().filter(|panel| panel.watch.id() == window.id()) else {
            return;
        };
        match outcome {
            Ok(()) => {
                panel.loaded = true;
                self.status.phase(window.id().get(), &WindowPhase::Loaded);
            }
            Err(reason) if reason == CANCELLED => {}
            Err(reason) => self.note_failure(run, reason),
        }
    }

    #[allow(clippy::significant_drop_tightening)] // the guard covers the identity check
    /// Decides the panel worker's next step, storing a fresh wake token for
    /// anything that changes the decision.
    fn next_step(&self, run_id: u64, wake: &CancellationToken) -> Option<Next> {
        let mut core = lock(&self.core);
        let run = core.run.as_mut().filter(|run| run.id == run_id)?;
        run.wake = Some(wake.clone());
        Some(match (&run.panel, run.retry_due) {
            (Some(panel), _) if !panel.loaded => Next::Load {
                watch: panel.watch.clone(),
                deadline: panel.opened + self.parts.timing.load_deadline,
            },
            (None, Some(due)) => Next::Retry { due },
            _ => Next::Idle,
        })
    }

    #[allow(clippy::significant_drop_tightening)] // the guard covers the identity check
    /// Retries a failed panel with a fresh exact window, against the latest
    /// reading and the current accepted configuration.
    fn retry_panel(&self, run_id: u64, due: Instant) {
        let mut core = lock(&self.core);
        let config = core.config.clone();
        let Some(run) = core.run.as_mut().filter(|run| run.id == run_id) else {
            return;
        };
        if run.panel.is_some() || run.retry_due != Some(due) {
            return;
        }
        let Some(config) = config else { return };
        run.retry_due = None;
        run.auto_retry = true;
        let reading = run.lease.read();
        let result = self.converge_fresh(run, &config, &reading);
        run.auto_retry = false;
        if let Err(reason) = result
            && run.panel.is_none()
        {
            self.note_failure(run, reason);
        }
    }

    /// The panel worker body for one runtime: observes loads, publishes
    /// failures and retries with backoff. Holds no lock while waiting.
    pub(crate) fn watch_panel(&self, run_id: u64, token: &CancellationToken) {
        loop {
            if token.is_cancelled() || self.shutdown.is_cancelled() {
                return;
            }
            let wake = CancellationToken::new();
            let _from_stop = {
                let wake = wake.clone();
                token.on_cancel(Arc::new(move || wake.cancel()))
            };
            let _from_shutdown = {
                let wake = wake.clone();
                self.shutdown.on_cancel(Arc::new(move || wake.cancel()))
            };
            let Some(next) = self.next_step(run_id, &wake) else {
                return;
            };
            match next {
                Next::Idle => {
                    let _ = wake.wait(FOLLOW_IDLE);
                }
                Next::Retry { due } => {
                    let now = Instant::now();
                    if due > now && wake.wait(due - now) {
                        continue;
                    }
                    self.retry_panel(run_id, due);
                }
                Next::Load { watch, deadline } => {
                    let outcome = self.await_loaded(&watch, deadline, &wake);
                    self.settle(run_id, &watch, outcome);
                }
            }
        }
    }

    /// Ends a runtime: reservation, panel lease, then the display feed.
    ///
    /// Idempotent. Returns whether the feed finished ending; `false` keeps the
    /// lease for an exact retry.
    pub(crate) fn teardown(&self) -> bool {
        let (run, lingering) = {
            let mut core = lock(&self.core);
            (core.run.take(), core.lingering.take())
        };
        let mut owed: Option<Arc<dyn TopologyLease>> = None;
        if let Some(mut run) = run {
            self.retire_panel(&mut run);
            let lease = Arc::clone(&run.lease);
            drop(run);
            // The empty topology is applied first inside `end`, before any join.
            if !lease.end() {
                owed = Some(lease);
            }
        }
        if let Some(lease) = lingering.filter(|lease| !lease.end()) {
            owed = Some(lease);
        }
        let menus_done = self.end_menus();
        let providers_done = self.end_providers();
        let finished = owed.is_none() && menus_done && providers_done;
        if let Some(lease) = owed {
            lock(&self.core).lingering = Some(lease);
        }
        self.status.feed_owed(!finished);
        finished
    }

    #[allow(clippy::significant_drop_tightening)] // the guard covers the identity check
    /// The display-follow worker body for one runtime.
    pub(crate) fn follow(&self, run_id: u64, mut seen: u64, token: &CancellationToken) {
        let lease = {
            let core = lock(&self.core);
            match core.run.as_ref().filter(|run| run.id == run_id) {
                Some(run) => Arc::clone(&run.lease),
                None => return,
            }
        };
        loop {
            match lease.wait_change(seen, FOLLOW_IDLE, &[token, &self.shutdown]) {
                TopologyWait::Cancelled => return,
                TopologyWait::TimedOut => {}
                TopologyWait::Changed(reading) => {
                    seen = reading.revision;
                    let mut core = lock(&self.core);
                    let config = core.config.clone();
                    let (Some(run), Some(config)) =
                        (core.run.as_mut().filter(|run| run.id == run_id), config)
                    else {
                        return;
                    };
                    if let Err(reason) = self.converge_fresh(run, &config, &reading) {
                        if run.panel.is_none() {
                            // No panel exists: same failed path as a load failure.
                            self.note_failure(run, reason);
                        } else {
                            self.status.detail(Some(reason));
                        }
                    }
                }
            }
        }
    }
}

/// The sentinel reason of a cancelled wait, never reported as a failure.
pub const CANCELLED: &str = "cancelled";

impl Run {
    pub(crate) fn new(
        id: u64,
        windows: WindowCapability,
        reservations: ScreenReservations,
        lease: Arc<dyn TopologyLease>,
    ) -> Self {
        Self {
            id,
            windows,
            reservations,
            lease,
            panel: None,
            staged: None,
            failures: 0,
            retry_due: None,
            auto_retry: false,
            wake: None,
        }
    }

    fn kick(&self) {
        if let Some(wake) = &self.wake {
            wake.cancel();
        }
    }
}
