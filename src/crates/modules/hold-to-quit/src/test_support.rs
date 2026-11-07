//! Fakes for the Hold-to-Quit runtime.
//!
//! Everything here is in memory: the input tap is the shared service's fake
//! backend, applications are [`FakeApplicationBackend`], the HUD is
//! [`FakeHudBackend`], main-thread work is a [`ManualExecutor`] (or a pump
//! thread) and time is [`FakeHoldClock`]. Nothing installs a real tap, shows a
//! real panel, terminates a real application or prompts, so default tests never
//! touch the machine they run on.

#![allow(clippy::must_use_candidate)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::Thread;
use std::time::{Duration, Instant};

use stache_macos::hold_hud::testing::{FakeHudBackend, FakeHudCall};
use stache_macos::hold_hud::{HudHandle, HudPoint, HudRect, HudScreen, HudScreens};
use stache_macos::{
    ApplicationBackend, ApplicationControl, DisplayId, FakeAppKey, FakeApplicationBackend,
    ManualExecutor,
};
use stache_sdk::input::testing::{FakeBackend, FakeClock as FakeInputClock};
use stache_sdk::input::{
    Disposition, ExemptionFlags, FrontmostApp, InputBackend, InputClock, InputEventKind,
    InputPriority, InputService, KeyCode, ModifierFlags, NoWork, PressStream, RawInput, TapKind,
};
use stache_sdk::{
    CancellationToken, EventBus, LogLevel, LogRecord, Logger, MainThreadExecutor, ModuleHandle,
    ModuleStatus, Permission, PermissionError, PermissionProvider, PermissionStatus, Registry,
    RegistryError, ThreadError, UnavailableWindows,
};

use crate::config::{CommandQuitConfig, HudAnimation, HudPosition};
use crate::policy::{HoldClock, KEY_Q};
use crate::runtime::{FrontmostLeaser, HoldToQuitParts, HoldToQuitRuntime, HudHost, LeaseHandle};

/// A poisoned-lock tolerant lock.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Polls `condition` until it holds, panicking after ten seconds.
///
/// # Panics
/// When the condition never holds.
pub fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A logger that keeps every record in order.
#[derive(Default)]
pub struct CaptureLogger {
    records: Mutex<Vec<LogRecord>>,
}

impl Logger for CaptureLogger {
    fn log(&self, record: LogRecord) { lock(&self.records).push(record); }
}

impl CaptureLogger {
    /// Every record so far.
    pub fn records(&self) -> Vec<LogRecord> { lock(&self.records).clone() }

    /// The warning messages so far.
    pub fn warnings(&self) -> Vec<String> {
        self.records()
            .into_iter()
            .filter(|record| record.level == LogLevel::Warn)
            .map(|record| record.message)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

struct Parked {
    thread: Thread,
    deadline: Option<Instant>,
}

struct ClockState {
    now: Instant,
    waits: Vec<Option<Duration>>,
    parked: Option<Parked>,
    on_now: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Fake module time. `park` blocks the worker until [`FakeHoldClock::advance`]
/// reaches its timeout or the worker is unparked, and records every requested
/// wait, so a test proves an idle worker never polls.
pub struct FakeHoldClock {
    state: Mutex<ClockState>,
}

impl FakeHoldClock {
    /// A clock at an arbitrary fixed origin.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ClockState {
                now: Instant::now(),
                waits: Vec::new(),
                parked: None,
                on_now: None,
            }),
        })
    }

    /// Advances fake time and wakes the parked worker when its timeout is due.
    pub fn advance(&self, by: Duration) {
        let due = {
            let mut state = lock(&self.state);
            state.now += by;
            let now = state.now;
            state
                .parked
                .as_ref()
                .filter(|parked| parked.deadline.is_some_and(|deadline| deadline <= now))
                .map(|parked| parked.thread.clone())
        };
        if let Some(thread) = due {
            thread.unpark();
        }
    }

    /// Advances fake time without waking the worker, as if the worker had not
    /// yet been scheduled.
    pub fn advance_quietly(&self, by: Duration) { lock(&self.state).now += by; }

    /// Unparks the worker, whatever it waits for.
    pub fn wake(&self) {
        let parked = lock(&self.state).parked.as_ref().map(|parked| parked.thread.clone());
        if let Some(thread) = parked {
            thread.unpark();
        }
    }

    /// Every wait the worker requested, oldest first. `None` is indefinite.
    pub fn waits(&self) -> Vec<Option<Duration>> { lock(&self.state).waits.clone() }

    /// Whether the worker is parked right now.
    pub fn is_parked(&self) -> bool { lock(&self.state).parked.is_some() }

    /// The remaining timeout of the wait the worker is parked in, if parked.
    pub fn parked_timeout(&self) -> Option<Option<Duration>> {
        let state = lock(&self.state);
        state.parked.as_ref().map(|parked| {
            parked.deadline.map(|deadline| deadline.saturating_duration_since(state.now))
        })
    }

    /// Runs `hook` inside every [`HoldClock::now`] call (a slow or panicking
    /// callback).
    pub fn set_on_now(&self, hook: Option<Arc<dyn Fn() + Send + Sync>>) {
        lock(&self.state).on_now = hook;
    }

    /// The current fake instant.
    pub fn current(&self) -> Instant { lock(&self.state).now }
}

impl HoldClock for FakeHoldClock {
    fn now(&self) -> Instant {
        let (now, hook) = {
            let state = lock(&self.state);
            (state.now, state.on_now.clone())
        };
        if let Some(hook) = hook {
            hook();
        }
        now
    }

    fn park(&self, timeout: Option<Duration>) {
        {
            let mut state = lock(&self.state);
            state.waits.push(timeout);
            if timeout.is_some_and(|timeout| timeout.is_zero()) {
                return;
            }
            let deadline = timeout.map(|timeout| state.now + timeout);
            state.parked = Some(Parked {
                thread: std::thread::current(),
                deadline,
            });
        }
        std::thread::park();
        lock(&self.state).parked = None;
    }
}

// ---------------------------------------------------------------------------
// Host pieces
// ---------------------------------------------------------------------------

type MainJob = Box<dyn FnOnce() + Send>;

/// A real thread that plays the main thread: every job runs on it, in order, so
/// a worker waits for an acknowledgement exactly as it does in production.
pub struct PumpMain {
    sender: Mutex<Option<std::sync::mpsc::Sender<MainJob>>>,
    thread: std::thread::ThreadId,
}

impl PumpMain {
    /// Starts the pump thread.
    ///
    /// # Panics
    /// When the thread cannot be created.
    pub fn new() -> Arc<Self> {
        let (sender, receiver) = std::sync::mpsc::channel::<MainJob>();
        let handle = std::thread::Builder::new()
            .name("hold-to-quit-test-main".to_owned())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    job();
                }
            })
            .expect("spawn the fake main thread");
        Arc::new(Self {
            sender: Mutex::new(Some(sender)),
            thread: handle.thread().id(),
        })
    }
}

impl MainThreadExecutor for PumpMain {
    fn is_main_thread(&self) -> bool { std::thread::current().id() == self.thread }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        lock(&self.sender)
            .as_ref()
            .ok_or(ThreadError::GroupStopped)
            .and_then(|sender| sender.send(job).map_err(|_| ThreadError::GroupStopped))
    }
}

/// A scripted frontmost-context leaser.
#[derive(Default)]
pub struct FakeLeaser {
    leases: AtomicUsize,
    releases: AtomicUsize,
    fail_lease: AtomicBool,
    fail_release: AtomicBool,
}

impl FakeLeaser {
    /// Makes the next lease fail.
    pub fn fail_next_lease(&self) { self.fail_lease.store(true, Ordering::SeqCst); }

    /// Makes the next release report an unacknowledged stop.
    pub fn fail_next_release(&self) { self.fail_release.store(true, Ordering::SeqCst); }

    /// Leases taken.
    pub fn leases(&self) -> usize { self.leases.load(Ordering::SeqCst) }

    /// Leases released.
    pub fn releases(&self) -> usize { self.releases.load(Ordering::SeqCst) }

    /// Leases currently held.
    pub fn held(&self) -> usize { self.leases().saturating_sub(self.releases()) }
}

struct FakeLease(Arc<FakeLeaser>);

impl LeaseHandle for FakeLease {
    fn release(self: Box<Self>) -> Result<(), String> {
        self.0.releases.fetch_add(1, Ordering::SeqCst);
        if self.0.fail_release.swap(false, Ordering::SeqCst) {
            Err("the stop was not acknowledged".to_owned())
        } else {
            Ok(())
        }
    }
}

/// Adapter so tests can lease through an `Arc<FakeLeaser>`.
pub struct LeaserHandle(pub Arc<FakeLeaser>);

impl FrontmostLeaser for LeaserHandle {
    fn lease(&self, _cancel: &CancellationToken) -> Result<Box<dyn LeaseHandle>, String> {
        if self.0.fail_lease.swap(false, Ordering::SeqCst) {
            return Err("the frontmost observation cannot start".to_owned());
        }
        self.0.leases.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeLease(Arc::clone(&self.0))))
    }
}

/// A permission provider whose status a test flips and whose prompts it counts.
pub struct Trust {
    trusted: AtomicBool,
    requests: AtomicUsize,
}

impl Trust {
    /// A provider that starts granted or denied.
    pub fn new(trusted: bool) -> Arc<Self> {
        Arc::new(Self {
            trusted: AtomicBool::new(trusted),
            requests: AtomicUsize::new(0),
        })
    }

    /// Grants or revokes the permission.
    pub fn set(&self, trusted: bool) { self.trusted.store(trusted, Ordering::SeqCst); }

    /// How many prompts were requested.
    pub fn requests(&self) -> usize { self.requests.load(Ordering::SeqCst) }
}

impl PermissionProvider for Trust {
    fn status(&self, _: Permission) -> PermissionStatus {
        if self.trusted.load(Ordering::SeqCst) {
            PermissionStatus::Granted
        } else {
            PermissionStatus::Denied
        }
    }

    fn request(&self, permission: Permission) -> Result<PermissionStatus, PermissionError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        Ok(self.status(permission))
    }
}

// ---------------------------------------------------------------------------
// The fakes and the rig
// ---------------------------------------------------------------------------

fn one_screen() -> HudScreens {
    let id = DisplayId::new(1);
    HudScreens {
        screens: vec![HudScreen {
            id,
            frame: HudRect::new(0.0, 0.0, 1440.0, 900.0),
            visible: HudRect::new(0.0, 0.0, 1440.0, 875.0),
        }],
        mouse: Some(HudPoint { x: 10.0, y: 10.0 }),
        main: Some(id),
    }
}

/// The priority the rig subscribes with, as the host's `HOLD_TO_QUIT`.
pub const PRIORITY: InputPriority = InputPriority { level: 300, tie_break: 0 };

/// An enabled configuration with `hold` milliseconds and no release linger,
/// so an early release hides the HUD at once.
pub const fn config(hold: u64) -> CommandQuitConfig { config_with_dismiss(hold, 0) }

/// An enabled configuration with `hold` and `dismiss` milliseconds.
pub const fn config_with_dismiss(hold: u64, dismiss: u64) -> CommandQuitConfig {
    CommandQuitConfig {
        enabled: true,
        hold_duration: hold,
        dismiss_delay: dismiss,
        position: HudPosition::Bottom,
        animation: HudAnimation::Fade,
    }
}

/// Every fake behind one Hold-to-Quit runtime, before any registry exists.
///
/// The host tests register [`Fakes::runtime`] through the app factory and attach
/// [`Fakes::input`] to their own driver; [`Rig`] registers it in a plain
/// registry.
pub struct Fakes {
    /// The main thread the application control and registry use.
    pub app_main: Arc<dyn MainThreadExecutor>,
    /// The manual executor behind `app_main` unless a pump replaced it.
    pub apps_executor: Arc<ManualExecutor>,
    /// The executor behind the HUD.
    pub hud_executor: Arc<ManualExecutor>,
    /// The fake application backend.
    pub apps: Arc<FakeApplicationBackend>,
    /// The application control over it.
    pub control: ApplicationControl,
    /// The fake HUD backend.
    pub hud_backend: FakeHudBackend,
    /// The HUD owner.
    pub hud: Arc<HudHost>,
    /// The module clock.
    pub clock: Arc<FakeHoldClock>,
    /// The frontmost-context leaser.
    pub leaser: Arc<FakeLeaser>,
    /// The service clock.
    pub input_clock: Arc<FakeInputClock>,
    /// The fake tap owner.
    pub tap: Arc<FakeBackend>,
    /// The shared input service.
    pub input: Arc<InputService>,
    /// The runtime service over all of them.
    pub runtime: HoldToQuitRuntime,
}

impl Default for Fakes {
    fn default() -> Self { Self::new() }
}

impl Fakes {
    /// Inline executors and a short acknowledgement budget.
    pub fn new() -> Self { Self::build(Duration::from_millis(50), true, None) }

    fn build(budget: Duration, main_runs: bool, pump: Option<Arc<PumpMain>>) -> Self {
        let apps_executor = Arc::new(ManualExecutor::new(main_runs));
        let hud_executor = Arc::new(ManualExecutor::new(main_runs));
        let apps = Arc::new(FakeApplicationBackend::new());
        let app_main: Arc<dyn MainThreadExecutor> = pump.map_or_else(
            || Arc::clone(&apps_executor) as Arc<dyn MainThreadExecutor>,
            |pump| pump as Arc<dyn MainThreadExecutor>,
        );
        let control = ApplicationControl::with_acknowledgement_budget(
            Arc::clone(&app_main),
            Arc::clone(&apps) as Arc<dyn ApplicationBackend>,
            budget,
        );
        let hud_backend = FakeHudBackend::with_screens(one_screen());
        let hud = Arc::new(
            HudHost::new(
                Arc::clone(&hud_executor) as Arc<dyn MainThreadExecutor>,
                Arc::new(hud_backend.clone()),
            )
            .with_acknowledgement_budget(budget),
        );
        let clock = FakeHoldClock::new();
        let leaser = Arc::new(FakeLeaser::default());
        let input_clock = FakeInputClock::new();
        let tap = FakeBackend::new(&input_clock);
        let input = InputService::with_clock(
            Arc::clone(&tap) as Arc<dyn InputBackend>,
            Arc::new(NoWork),
            Arc::clone(&input_clock) as Arc<dyn InputClock>,
        );
        let runtime = HoldToQuitRuntime::new(HoldToQuitParts {
            control: control.clone(),
            hud: Arc::clone(&hud),
            frontmost: Arc::new(LeaserHandle(Arc::clone(&leaser))),
            clock: Arc::clone(&clock) as Arc<dyn HoldClock>,
            priority: PRIORITY,
        });
        Self {
            app_main,
            apps_executor,
            hud_executor,
            apps,
            control,
            hud_backend,
            hud,
            clock,
            leaser,
            input_clock,
            tap,
            input,
            runtime,
        }
    }
}

/// A fake application and what the SDK context says about it.
#[derive(Clone)]
pub struct App {
    /// The fake native instance.
    pub key: FakeAppKey,
    /// Its bundle identifier.
    pub bundle: Arc<str>,
    /// The exemption bits the host would publish.
    pub exemptions: ExemptionFlags,
}

/// A fully faked Hold-to-Quit runtime behind a real registry.
pub struct Rig {
    /// The runtime service under test.
    pub runtime: HoldToQuitRuntime,
    /// The registry that owns the module.
    pub registry: Arc<Registry>,
    /// The module handle.
    pub handle: ModuleHandle<CommandQuitConfig>,
    /// The shared input service.
    pub input: Arc<InputService>,
    /// The fake tap owner.
    pub tap: Arc<FakeBackend>,
    /// The fake application backend.
    pub apps: Arc<FakeApplicationBackend>,
    /// The application control over it.
    pub control: ApplicationControl,
    /// The executor behind the application control and the registry.
    pub apps_executor: Arc<ManualExecutor>,
    /// The executor behind the HUD.
    pub hud_executor: Arc<ManualExecutor>,
    /// The fake HUD backend.
    pub hud_backend: FakeHudBackend,
    /// The HUD owner.
    pub hud: Arc<HudHost>,
    /// The module clock.
    pub clock: Arc<FakeHoldClock>,
    /// The frontmost-context leaser.
    pub leaser: Arc<FakeLeaser>,
    /// The permission provider.
    pub trust: Arc<Trust>,
    /// The captured log.
    pub logger: Arc<CaptureLogger>,
    /// The service clock.
    pub input_clock: Arc<FakeInputClock>,
}

impl Rig {
    /// A rig with inline executors and a short acknowledgement budget.
    pub fn new(config: CommandQuitConfig) -> Self {
        Self::with(config, Duration::from_millis(50), true)
    }

    /// A rig with an explicit acknowledgement budget and a main thread that
    /// either runs jobs inline or never drains.
    pub fn with(config: CommandQuitConfig, budget: Duration, main_runs: bool) -> Self {
        Self::assemble(config, Fakes::build(budget, main_runs, None))
    }

    /// A rig whose application main thread is a real pump thread, so a worker
    /// waits for acknowledgements across threads as it does in production.
    pub fn pumped(config: CommandQuitConfig, budget: Duration) -> Self {
        Self::assemble(config, Fakes::build(budget, true, Some(PumpMain::new())))
    }

    fn assemble(config: CommandQuitConfig, parts: Fakes) -> Self {
        let runtime = parts.runtime.clone();
        let logger = Arc::new(CaptureLogger::default());
        let trust = Trust::new(true);
        let registry = Arc::new(
            Registry::new(
                Arc::clone(&logger) as Arc<dyn Logger>,
                Arc::new(EventBus::new()),
                Arc::clone(&parts.app_main),
                Arc::clone(&trust) as Arc<dyn PermissionProvider>,
                Arc::new(UnavailableWindows),
            )
            .with_input_service(Arc::clone(&parts.input)),
        );
        let handle = registry.register(runtime.module(), config).expect("register");
        runtime.install_accepted_watch(registry.accepted_watch(handle).expect("accepted watch"));
        Self {
            runtime,
            registry,
            handle,
            input: parts.input,
            tap: parts.tap,
            apps: parts.apps,
            control: parts.control,
            apps_executor: parts.apps_executor,
            hud_executor: parts.hud_executor,
            hud_backend: parts.hud_backend,
            hud: parts.hud,
            clock: parts.clock,
            leaser: parts.leaser,
            trust,
            logger,
            input_clock: parts.input_clock,
        }
    }

    /// Starts every module.
    ///
    /// # Errors
    /// The registry's start error.
    pub fn start(&self) -> Result<(), RegistryError> { self.registry.start_all() }

    /// A rig that started successfully and whose worker is parked.
    ///
    /// # Panics
    /// When the start fails.
    pub fn started(config: CommandQuitConfig) -> Self {
        let rig = Self::new(config);
        rig.start().expect("start");
        rig.wait_parked();
        rig
    }

    /// The module's lifecycle status.
    ///
    /// # Panics
    /// When the handle is stale.
    pub fn status(&self) -> ModuleStatus {
        self.registry.snapshot(self.handle).expect("snapshot").status
    }

    /// Waits until the worker is parked.
    pub fn wait_parked(&self) { wait_until("the worker to park", || self.clock.is_parked()); }

    /// Waits until the worker is parked in a wait whose remaining timeout
    /// satisfies `timeout`.
    pub fn wait_parked_for(&self, what: &str, timeout: impl Fn(Option<Duration>) -> bool) {
        wait_until(what, || self.clock.parked_timeout().is_some_and(&timeout));
    }

    /// Adds a running application.
    pub fn add_app(&self, bundle: &str) -> App {
        let key = self.apps.add_running(bundle);
        self.apps.set_name(key, Some(bundle));
        App {
            key,
            bundle: Arc::from(bundle),
            exemptions: ExemptionFlags(0),
        }
    }

    /// Makes `app` frontmost: the OS answer, the activation notification and the
    /// SDK context the host would publish.
    pub fn focus(&self, app: &App) {
        let _ = self.apps.activate(app.key);
        self.input.context_publisher().publish_frontmost(FrontmostApp {
            bundle_id: Arc::clone(&app.bundle),
            exemptions: app.exemptions,
        });
    }

    /// Routes one raw event through the suppressing tap.
    pub fn raw(
        &self,
        kind: InputEventKind,
        key: u16,
        flags: ModifierFlags,
        repeat: bool,
    ) -> Disposition {
        self.input.dispatch(TapKind::Suppressing, RawInput {
            kind,
            stream: PressStream::Key(KeyCode(key)),
            flags,
            repeat,
            timestamp: Duration::ZERO,
        })
    }

    /// A Command-only `Q` down.
    pub fn q_down(&self) -> Disposition { self.q_down_with(ModifierFlags::COMMAND) }

    /// A `Q` down with `flags`.
    pub fn q_down_with(&self, flags: ModifierFlags) -> Disposition {
        self.raw(InputEventKind::KeyDown, KEY_Q, flags, false)
    }

    /// A Command-only `Q` auto-repeat.
    pub fn q_repeat(&self) -> Disposition {
        self.raw(InputEventKind::KeyDown, KEY_Q, ModifierFlags::COMMAND, true)
    }

    /// A Command-only `Q` up.
    pub fn q_up(&self) -> Disposition {
        self.raw(InputEventKind::KeyUp, KEY_Q, ModifierFlags::COMMAND, false)
    }

    /// The left Command key changing the flags to `flags`.
    pub fn command_flags(&self, flags: ModifierFlags) -> Disposition {
        self.raw(InputEventKind::FlagsChanged, 55, flags, false)
    }

    /// The handle of the HUD owner the module installed.
    ///
    /// # Panics
    /// When nothing was installed yet.
    pub fn hud_handle(&self) -> HudHandle {
        self.hud_backend
            .calls()
            .iter()
            .find_map(|call| match call {
                FakeHudCall::Install(handle) => Some(*handle),
                _ => None,
            })
            .expect("the HUD was installed")
    }

    /// Counts the HUD `show` calls the backend received.
    pub fn shows(&self) -> usize {
        self.hud_backend
            .calls()
            .iter()
            .filter(|call| matches!(call, FakeHudCall::Show { .. }))
            .count()
    }
}
