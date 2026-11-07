//! Fakes for the `MenuAnywhere` runtime.
//!
//! Everything here is in memory. The input tap is the shared service's fake
//! backend, applications are [`FakeApplicationBackend`], the menu stack is the
//! real [`MenuControl`] over the Part 02 fakes (a fake AX tree, a fake
//! presenter, fake geometry and a thread standing in for main) and time is the
//! fake menu clock. Nothing installs a real tap, reads a real application,
//! shows a real menu, activates, presses or prompts, so default tests never
//! touch the machine they run on.

#![allow(clippy::must_use_candidate, clippy::missing_panics_doc)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::menu::testing::{FakeAx, FakeClock, Node};
use stache_macos::menu::testing_native::{FakeGeometry, FakeMainThread, FakePresenter};
use stache_macos::menu::{
    ActionToken, AxOwner, ControlConfig, DisplayGeometry, GeometrySnapshot, MainDispatcher,
    MenuClock, MenuControl, OwnerConfig, PlannedItem, PresentationId, TargetValidator,
};
use stache_macos::{
    ApplicationBackend, ApplicationControl, DisplayId, DisplayRect, FakeAppKey,
    FakeApplicationBackend, ManualExecutor,
};
use stache_sdk::input::testing::{FakeBackend, FakeClock as FakeInputClock};
use stache_sdk::input::{
    Disposition, InputBackend, InputClock, InputEventKind, InputPoint, InputPriority, InputService,
    ModifierFlags, MouseButton, NoWork, PressStream, RawInput, TapKind,
};
use stache_sdk::{
    EventBus, LogLevel, LogRecord, Logger, MainThreadExecutor, ModuleHandle, ModuleStatus,
    Permission, PermissionError, PermissionProvider, PermissionStatus, Registry, RegistryError,
    UnavailableWindows,
};

use crate::backend::MenuHost;
use crate::config::{MenuAnywhereConfig, MenuAnywhereModifier, MenuAnywhereMouseButton};
use crate::runtime::{MenuAnywhereParts, MenuAnywhereRuntime, Stage};
use crate::trigger::Outcome;

/// The process id the fake host reports as this process.
pub const OWN_PID: i32 = 7;

/// The priority the rig subscribes with, as the host's `MENU_ANYWHERE`.
pub const PRIORITY: InputPriority = InputPriority { level: 200, tie_break: 0 };

/// A short acknowledgement window so timeouts are quick.
pub const ACK: Duration = Duration::from_millis(250);

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

/// A permission provider whose status a test flips and whose calls it counts.
pub struct Trust {
    trusted: AtomicBool,
    statuses: AtomicUsize,
    requests: AtomicUsize,
}

impl Trust {
    /// A provider that starts granted or denied.
    pub fn new(trusted: bool) -> Arc<Self> {
        Arc::new(Self {
            trusted: AtomicBool::new(trusted),
            statuses: AtomicUsize::new(0),
            requests: AtomicUsize::new(0),
        })
    }

    /// Grants or revokes the permission.
    pub fn set(&self, trusted: bool) { self.trusted.store(trusted, Ordering::SeqCst); }

    /// How many non-prompting status checks happened.
    pub fn statuses(&self) -> usize { self.statuses.load(Ordering::SeqCst) }

    /// How many prompts were requested.
    pub fn requests(&self) -> usize { self.requests.load(Ordering::SeqCst) }

    fn current(&self) -> PermissionStatus {
        if self.trusted.load(Ordering::SeqCst) {
            PermissionStatus::Granted
        } else {
            PermissionStatus::Denied
        }
    }
}

impl PermissionProvider for Trust {
    fn status(&self, _: Permission) -> PermissionStatus {
        self.statuses.fetch_add(1, Ordering::SeqCst);
        self.current()
    }

    fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        Ok(self.current())
    }
}

/// One display, so every fake anchor lands inside it.
pub fn single_display() -> GeometrySnapshot {
    GeometrySnapshot {
        primary: DisplayId::new(1),
        displays: vec![DisplayGeometry {
            id: DisplayId::new(1),
            quartz: DisplayRect::new(0.0, 0.0, 1440.0, 900.0),
            appkit: DisplayRect::new(0.0, 0.0, 1440.0, 900.0),
            backing_scale: 2.0,
        }],
    }
}

/// The default fake menu bar: Apple, File (Open, Close) and a top-level item.
pub fn default_menu() -> Vec<Node> {
    vec![
        Node::submenu("Apple", vec![Node::item("About")]),
        Node::submenu("File", vec![Node::item("Open"), Node::item("Close")]),
        Node::item("Solo"),
    ]
}

/// The fake native menu stack.
///
/// [`MenuHost::open`] builds the real [`AxOwner`] and [`MenuControl`] over the
/// Part 02 fakes, so the worker code under test is the production code.
pub struct FakeMenuHost {
    /// The fake AX tree (its hook parks reads and presses).
    pub ax: FakeAx,
    /// The thread standing in for main.
    pub main: Arc<FakeMainThread>,
    /// The fake presenter.
    pub presenter: Arc<FakePresenter>,
    /// The fake geometry.
    pub geometry: Arc<FakeGeometry>,
    /// The fake monotonic clock.
    pub clock: Arc<FakeClock>,
    fail_open: AtomicBool,
    opens: AtomicUsize,
}

impl FakeMenuHost {
    /// A host over `ax`.
    pub fn new(ax: FakeAx) -> Arc<Self> {
        let main = FakeMainThread::start();
        let presenter = FakePresenter::new(Arc::clone(&main));
        Arc::new(Self {
            ax,
            presenter,
            main,
            geometry: FakeGeometry::new(single_display()),
            clock: Arc::new(FakeClock::default()),
            fail_open: AtomicBool::new(false),
            opens: AtomicUsize::new(0),
        })
    }

    /// Makes the next [`MenuHost::open`] fail.
    pub fn fail_next_open(&self) { self.fail_open.store(true, Ordering::SeqCst); }

    /// How many menu stacks were opened.
    pub fn opens(&self) -> usize { self.opens.load(Ordering::SeqCst) }
}

impl MenuHost for FakeMenuHost {
    fn open(&self, validator: Arc<dyn TargetValidator>) -> Result<MenuControl, String> {
        if self.fail_open.swap(false, Ordering::SeqCst) {
            return Err("the menu stack cannot be opened".to_owned());
        }
        self.opens.fetch_add(1, Ordering::SeqCst);
        let ax = self.ax.clone();
        let owner = Arc::new(AxOwner::new(move || ax, OwnerConfig {
            clock: Arc::clone(&self.clock) as Arc<dyn MenuClock>,
            validator,
            own_pid: OWN_PID,
        }));
        Ok(MenuControl::new(
            owner,
            Arc::clone(&self.presenter) as Arc<_>,
            Arc::clone(&self.geometry) as Arc<_>,
            Arc::clone(&self.main) as Arc<dyn MainDispatcher>,
            Arc::clone(&self.clock) as Arc<dyn MenuClock>,
            ControlConfig { budget: ACK },
        ))
    }

    fn own_pid(&self) -> i32 { OWN_PID }

    fn clock(&self) -> Arc<dyn MenuClock> { Arc::clone(&self.clock) as Arc<dyn MenuClock> }
}

type Gates = Arc<Mutex<Vec<(Stage, Arc<StageGate>)>>>;

/// A one-shot rendezvous a test holds a runtime stage open with.
#[derive(Default)]
pub struct StageGate {
    state: Mutex<(bool, bool)>,
    signal: std::sync::Condvar,
}

impl StageGate {
    /// Waits until the runtime reached the stage.
    pub fn wait_entered(&self) {
        let (state, result) = self
            .signal
            .wait_timeout_while(lock(&self.state), Duration::from_secs(10), |state| !state.0)
            .unwrap_or_else(PoisonError::into_inner);
        let reached = state.0 && !result.timed_out();
        drop(state);
        assert!(reached, "the stage was never reached");
    }

    /// Whether the runtime reached the stage.
    pub fn entered(&self) -> bool { lock(&self.state).0 }

    /// Lets the held stage continue.
    pub fn release(&self) {
        lock(&self.state).1 = true;
        self.signal.notify_all();
    }

    fn enter(&self) {
        let mut state = lock(&self.state);
        state.0 = true;
        self.signal.notify_all();
        let (_state, _) = self
            .signal
            .wait_timeout_while(state, Duration::from_secs(10), |state| !state.1)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// A parked fake AX call.
pub struct AxHold {
    entered: Mutex<std::sync::mpsc::Receiver<()>>,
    release: std::sync::mpsc::Sender<()>,
}

impl AxHold {
    /// Waits until the AX call parked.
    pub fn wait_entered(&self) {
        lock(&self.entered)
            .recv_timeout(Duration::from_secs(10))
            .expect("the AX call parked");
    }

    /// Lets the parked AX call continue.
    pub fn release(&self) { let _ = self.release.send(()); }
}

/// Parks the first AX call of `op` made after this is installed.
pub fn hold_ax(ax: &FakeAx, op: stache_macos::menu::testing::Op) -> AxHold {
    let (entered_tx, entered) = std::sync::mpsc::channel();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let entered_tx = Mutex::new(Some(entered_tx));
    let release_rx = Mutex::new(release_rx);
    ax.set_hook(move |call| {
        if call.op == op {
            let taken = lock(&entered_tx).take();
            if let Some(tx) = taken {
                let _ = tx.send(());
                let _ = lock(&release_rx).recv_timeout(Duration::from_secs(10));
            }
        }
    });
    AxHold {
        entered: Mutex::new(entered),
        release,
    }
}

/// An enabled configuration for `button` with exactly `modifiers`.
pub fn config(
    button: MenuAnywhereMouseButton,
    modifiers: &[MenuAnywhereModifier],
) -> MenuAnywhereConfig {
    MenuAnywhereConfig {
        enabled: true,
        modifiers: modifiers.to_vec(),
        mouse_button: button,
    }
}

/// The default enabled configuration: right click with Control and Command.
pub fn default_config() -> MenuAnywhereConfig {
    MenuAnywhereConfig {
        enabled: true,
        ..MenuAnywhereConfig::default()
    }
}

/// The service flags of the default chord.
pub const CHORD: ModifierFlags = ModifierFlags::CONTROL.union(ModifierFlags::COMMAND);

/// A fake application and its key.
#[derive(Clone, Copy)]
pub struct App {
    /// The fake native instance.
    pub key: FakeAppKey,
    /// Its process id.
    pub pid: i32,
}

/// Every fake behind one `MenuAnywhere` runtime, before any registry exists.
///
/// The host tests register [`Fakes::runtime`] through the app factory and attach
/// [`Fakes::input`] to their own driver; [`Rig`] registers it in a plain
/// registry.
pub struct Fakes {
    /// The executor behind the application control and the registry.
    pub apps_executor: Arc<ManualExecutor>,
    /// The fake application backend.
    pub apps: Arc<FakeApplicationBackend>,
    /// The application control over it.
    pub control: ApplicationControl,
    /// The fake menu host.
    pub menu: Arc<FakeMenuHost>,
    /// The service clock.
    pub input_clock: Arc<FakeInputClock>,
    /// The fake tap owner.
    pub tap: Arc<FakeBackend>,
    /// The shared input service.
    pub input: Arc<InputService>,
    /// The runtime service over all of them.
    pub runtime: MenuAnywhereRuntime,
}

impl Default for Fakes {
    fn default() -> Self { Self::new() }
}

impl Fakes {
    /// Inline executors and a short acknowledgement budget.
    pub fn new() -> Self { Self::with_menu(default_menu()) }

    /// Fakes over a specific fake menu bar.
    pub fn with_menu(menu: Vec<Node>) -> Self {
        let apps_executor = Arc::new(ManualExecutor::new(true));
        let apps = Arc::new(FakeApplicationBackend::new());
        let control = ApplicationControl::with_acknowledgement_budget(
            Arc::clone(&apps_executor) as Arc<dyn MainThreadExecutor>,
            Arc::clone(&apps) as Arc<dyn ApplicationBackend>,
            ACK,
        );
        let menu = FakeMenuHost::new(FakeAx::new(menu));
        let input_clock = FakeInputClock::new();
        let tap = FakeBackend::new(&input_clock);
        let input = InputService::with_clock(
            Arc::clone(&tap) as Arc<dyn InputBackend>,
            Arc::new(NoWork),
            Arc::clone(&input_clock) as Arc<dyn InputClock>,
        );
        let runtime = MenuAnywhereRuntime::new(MenuAnywhereParts {
            applications: control.clone(),
            menu: Arc::clone(&menu) as Arc<dyn MenuHost>,
            priority: PRIORITY,
            health_interval: Duration::from_millis(20),
            acknowledgement: ACK,
        });
        Self {
            apps_executor,
            apps,
            control,
            menu,
            input_clock,
            tap,
            input,
            runtime,
        }
    }
}

/// A fully faked `MenuAnywhere` runtime behind a real registry.
pub struct Rig {
    /// The runtime service under test.
    pub runtime: MenuAnywhereRuntime,
    /// The registry that owns the module.
    pub registry: Arc<Registry>,
    /// The module handle.
    pub handle: ModuleHandle<MenuAnywhereConfig>,
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
    /// The fake menu host.
    pub menu: Arc<FakeMenuHost>,
    /// The permission provider.
    pub trust: Arc<Trust>,
    /// The captured log.
    pub logger: Arc<CaptureLogger>,
    /// The service clock.
    pub input_clock: Arc<FakeInputClock>,
    gates: Gates,
}

impl Rig {
    /// A rig that is not yet started.
    pub fn new(config: MenuAnywhereConfig) -> Self { Self::with_menu(config, default_menu()) }

    /// A rig over a specific fake menu bar.
    pub fn with_menu(config: MenuAnywhereConfig, menu: Vec<Node>) -> Self {
        Self::assemble(config, menu, true)
    }

    /// A rig whose host has no shared input service.
    pub fn without_input_service(config: MenuAnywhereConfig) -> Self {
        Self::assemble(config, default_menu(), false)
    }

    fn assemble(config: MenuAnywhereConfig, menu: Vec<Node>, input_available: bool) -> Self {
        let fakes = Fakes::with_menu(menu);
        let logger = Arc::new(CaptureLogger::default());
        let trust = Trust::new(true);
        let registry = Registry::new(
            Arc::clone(&logger) as Arc<dyn Logger>,
            Arc::new(EventBus::new()),
            Arc::clone(&fakes.apps_executor) as Arc<dyn MainThreadExecutor>,
            Arc::clone(&trust) as Arc<dyn PermissionProvider>,
            Arc::new(UnavailableWindows),
        );
        let registry = Arc::new(if input_available {
            registry.with_input_service(Arc::clone(&fakes.input))
        } else {
            registry
        });
        let runtime = fakes.runtime.clone();
        let handle = registry.register(runtime.module(), config).expect("register");
        runtime.install_accepted_watch(registry.accepted_watch(handle).expect("accepted watch"));
        Self {
            runtime,
            registry,
            handle,
            input: fakes.input,
            tap: fakes.tap,
            apps: fakes.apps,
            control: fakes.control,
            apps_executor: fakes.apps_executor,
            menu: fakes.menu,
            trust,
            logger,
            input_clock: fakes.input_clock,
            gates: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A rig that started successfully with a frontmost application.
    ///
    /// # Panics
    /// When the start fails.
    pub fn started(config: MenuAnywhereConfig) -> (Self, App) {
        let rig = Self::new(config);
        let app = rig.add_app("com.example.editor", 4242);
        rig.registry.start_all().expect("start");
        rig.focus(app);
        (rig, app)
    }

    /// Starts every module.
    ///
    /// # Errors
    /// The registry's start error.
    pub fn start(&self) -> Result<(), RegistryError> { self.registry.start_all() }

    /// The module's lifecycle status.
    pub fn status(&self) -> ModuleStatus {
        self.registry.snapshot(self.handle).expect("snapshot").status
    }

    /// Adds a running application with a process id.
    pub fn add_app(&self, bundle: &str, pid: i32) -> App {
        let key = self.apps.add_running_with_pid(bundle, pid);
        self.apps.set_name(key, Some(bundle));
        App { key, pid }
    }

    /// Makes `app` frontmost and delivers the activation notification.
    pub fn focus(&self, app: App) { let _ = self.apps.activate(app.key); }

    /// Routes one raw mouse event at the default point (100, 100).
    pub fn mouse(&self, kind: InputEventKind, button: u32, flags: ModifierFlags) -> Disposition {
        self.mouse_at(kind, button, flags, Some(InputPoint { x: 100.0, y: 100.0 }))
    }

    /// Routes one raw mouse event carrying `location` through the suppressing tap.
    pub fn mouse_at(
        &self,
        kind: InputEventKind,
        button: u32,
        flags: ModifierFlags,
        location: Option<InputPoint>,
    ) -> Disposition {
        self.input.dispatch(TapKind::Suppressing, RawInput {
            kind,
            stream: PressStream::Button(MouseButton(button)),
            flags,
            repeat: false,
            timestamp: Duration::ZERO,
            location,
        })
    }

    /// A button down with `flags`.
    pub fn down(&self, button: u32, flags: ModifierFlags) -> Disposition {
        self.mouse(InputEventKind::MouseDown, button, flags)
    }

    /// A button up with `flags`.
    pub fn up(&self, button: u32, flags: ModifierFlags) -> Disposition {
        self.mouse(InputEventKind::MouseUp, button, flags)
    }

    /// A right-button down with the default chord.
    pub fn chord_down(&self) -> Disposition { self.down(1, CHORD) }

    /// A right-button up with the default chord.
    pub fn chord_up(&self) -> Disposition { self.up(1, CHORD) }

    /// Holds the next time the runtime reaches `stage` until released.
    pub fn hold(&self, stage: Stage) -> Arc<StageGate> {
        let gate = Arc::new(StageGate::default());
        let table = Arc::clone(&self.gates);
        let first = {
            let mut gates = lock(&table);
            gates.push((stage, Arc::clone(&gate)));
            gates.len() == 1
        };
        if first {
            self.runtime.set_stage_hook(Some(Arc::new(move |reached| {
                let found = {
                    let mut gates = lock(&table);
                    gates
                        .iter()
                        .position(|(held, _)| *held == reached)
                        .map(|index| gates.remove(index).1)
                };
                if let Some(gate) = found {
                    gate.enter();
                }
            })));
        }
        gate
    }

    /// How many times the runtime counted `outcome`.
    pub fn count(&self, outcome: Outcome) -> u64 { self.runtime.count(outcome) }

    /// Waits until `outcome` was counted at least `times`.
    pub fn wait_count(&self, outcome: Outcome, times: u64) {
        wait_until(outcome.label(), || self.count(outcome) >= times);
    }

    /// Waits until the busy reservation is free.
    pub fn wait_idle(&self) { wait_until("the reservation to free", || !self.runtime.is_busy()); }

    /// Waits until the menu is tracking and returns its presentation.
    pub fn wait_tracking(&self) -> PresentationId {
        wait_until("a tracking menu", || {
            self.menu
                .presenter
                .tracked()
                .last()
                .is_some_and(|id| self.menu.presenter.is_tracking(*id))
        });
        *self.menu.presenter.tracked().last().expect("tracked")
    }

    /// The action token of the leaf titled `title` in a presentation's menu.
    pub fn token(&self, id: PresentationId, title: &str) -> ActionToken {
        fn find(items: &[PlannedItem], title: &str, tokens: &[ActionToken]) -> Option<ActionToken> {
            items.iter().find_map(|item| match item {
                PlannedItem::Separator => None,
                PlannedItem::Item(entry) => {
                    if entry.title == title
                        && let Some(slot) = entry.action_slot
                    {
                        return tokens.get(slot as usize).copied();
                    }
                    entry.submenu.as_deref().and_then(|items| find(items, title, tokens))
                }
            })
        }
        let plan = self.menu.presenter.plan(id).expect("plan");
        find(&plan.items, title, &plan.tokens).expect("token")
    }

    /// Chooses `title` in the tracking menu.
    pub fn choose(&self, id: PresentationId, title: &str) {
        let token = self.token(id, title);
        self.menu.presenter.choose(id, token);
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.registry.shutdown();
        self.menu.main.shutdown();
    }
}
