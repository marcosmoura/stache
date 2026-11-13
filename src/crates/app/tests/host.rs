//! Composition-root behavior with fake native and module services.
//!
//! These tests never touch the real home, configuration directory, runtime
//! socket, tray, Accessibility prompt or legacy process.

use std::ffi::OsString;
use std::fs;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stache_app::access::SettingsOpener;
use stache_app::bindings::{AccessState, HostStatus, ModuleState, ModuleStatusView};
use stache_app::caps_recovery::KeybindingsCapsRecovery;
use stache_app::hold_to_quit::{HoldToQuitFactory, HudOwner};
use stache_app::host::{
    FactoryContext, HostDriver, HostHandle, HostServices, HostSettings, HostUi, KeepAwakeFactory,
    MenuAnywhereFactory, ModuleFactories, ModuleFactory, NoTunesFactory, ProxyAudioFactory,
    StartupRecovery, new_host, register_module,
};
use stache_app::permissions::PermissionPoll;
use stache_app::shutdown::{
    BoundedRecordChannel, ExitProcess, InstanceOwner, MainHopOwner, NativeOwner, NativeSlot,
    OwnerAnswer, RecordDelivery, SystemClock, TEARDOWN_TOTAL, Teardown, TeardownCause,
    TeardownClock, TeardownRecordChannel, TeardownRecordSink, TeardownResult, TerminalAction,
    TerminalDriver, TerminalProgress, Wake, report_terminate_install_failure,
};
use stache_app::startup::CommandRunner;
use stache_app::wallpapers::WallpapersFactory;
use stache_app::windows::{
    CloseOutcome, NativeWindowSpec, NativeWindows, WindowHost, WindowsOwner,
};
use stache_cli::CliEnvironment;
use stache_hold_to_quit::test_support::Fakes;
use stache_macos::audio::{
    AudioBackend, AudioChangeSubscription, AudioDevice, AudioDirection, AudioError, AudioWakeup,
};
use stache_macos::{
    ApplicationControl, AssertionOwner, DesktopBackend, DesktopError, DesktopOperation,
    DisplayEntry, DisplayFrame, DisplayId, DisplaySnapshot, FakeApplicationBackend,
    FakeAssertionBackend, FakeDesktop, FakeLockObservationBackend, FakeLockProbe,
    GuardedDesktopBackend, LockObservationInstaller, LockState as NativeLockState, ManualExecutor,
};
use stache_menu_anywhere::test_support::Fakes as MenuFakes;
use stache_sdk::input::{
    Disposition, InputEventKind, InputService, KeyCode, ModifierFlags, PressStream, RawInput,
    TapKind,
};
use stache_sdk::ipc::{HandlerResult, IpcClient, IpcHandler, Request, Response, RetryPolicy};
use stache_sdk::shutdown::ShutdownWait;
use stache_sdk::{
    AppEnvironment, AppPaths, CommandCatalog, ConfigCatalog, ConfigEnvironment, ConfigError,
    ConfigPaths, DiscardLogger, EventBus, LogFilter, MainThreadExecutor, Module, ModuleConfig,
    ModuleContext, ModuleDescriptor, ModuleId, ModuleResult, Permission, PermissionError,
    PermissionProvider, PermissionRequirement, PermissionStatus, SurfaceId, ThreadError,
    WindowBackend, WindowError, WindowId, WindowRequest,
};
use stache_wallpapers::cli::{
    IpcTransport, SetTransport, WallpapersCliEnv, contribution as wallpaper_contribution,
    never_cancelled,
};
use stache_wallpapers::{
    Processor, RealProcessor, RuntimeClock, ServiceConfig, SourceContext, SystemRandom,
    SystemRuntimeClock, WALLPAPERS_CLIENT_BUDGET, WallpaperScreen, WallpaperSelection,
    WallpapersOutcomeCause, WallpapersOutcomeStatus, WallpapersService, WallpapersSetOutcome,
    WallpapersSetParams, WallpapersTargetFailure, WallpapersTargetOutcome, WallpapersTargetResult,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
struct TestConfig {
    enabled: bool,
    value: u32,
}

impl TestConfig {
    const ID: ModuleId = ModuleId::new("testModule");
}

impl ModuleConfig for TestConfig {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
}

#[derive(Default)]
struct Counters {
    configure: AtomicUsize,
    start: AtomicUsize,
    stop: AtomicUsize,
    reconfigure: AtomicUsize,
    fail_next_start: AtomicBool,
    fail_next_stop: AtomicBool,
}

struct Gate {
    open: Mutex<bool>,
    signal: Condvar,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            open: Mutex::new(false),
            signal: Condvar::new(),
        })
    }

    #[allow(clippy::significant_drop_tightening)] // The guard is the loop condition.
    fn wait(&self) {
        let mut open = self.open.lock().expect("gate");
        while !*open {
            open = self.signal.wait(open).expect("gate");
        }
    }

    fn open(&self) {
        *self.open.lock().expect("gate") = true;
        self.signal.notify_all();
    }
}

struct TestModule {
    counters: Arc<Counters>,
    permission: bool,
    gate: Option<Arc<Gate>>,
    configure_gate: Option<Arc<Gate>>,
}

static ACCESS: [Permission; 1] = [Permission::Accessibility];
static SURFACES: [SurfaceId; 1] = [SurfaceId::new("test")];

impl Module for TestModule {
    type Config = TestConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: TestConfig::ID,
            provides: &[],
            requires: &[],
            permissions: if self.permission { &ACCESS } else { &[] },
            executables: &[],
            surfaces: &SURFACES,
            observes: &[],
            reserves_screen: false,
        }
    }

    fn configure(&mut self, _: &TestConfig, _: &ModuleContext) -> ModuleResult<()> {
        self.counters.configure.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.configure_gate {
            gate.wait();
        }
        Ok(())
    }

    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        self.counters.start.fetch_add(1, Ordering::SeqCst);
        if self.counters.fail_next_start.swap(false, Ordering::SeqCst) {
            return Err(stache_sdk::ModuleError::new(
                ctx.id(),
                stache_sdk::Operation::Start,
                stache_sdk::ErrorCategory::Lifecycle,
                "injected start failure",
            ));
        }
        if let Some(gate) = &self.gate {
            gate.wait();
        }
        Ok(())
    }

    fn reconfigure(&mut self, _: &TestConfig, _: &ModuleContext) -> ModuleResult<()> {
        self.counters.reconfigure.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        self.counters.stop.fetch_add(1, Ordering::SeqCst);
        if self.counters.fail_next_stop.swap(false, Ordering::SeqCst) {
            return Err(stache_sdk::ModuleError::new(
                ctx.id(),
                stache_sdk::Operation::Stop,
                stache_sdk::ErrorCategory::Lifecycle,
                "injected stop failure",
            ));
        }
        Ok(())
    }
}

struct TestFactory {
    counters: Arc<Counters>,
    permission: bool,
    gate: Option<Arc<Gate>>,
    configure_gate: Option<Arc<Gate>>,
}

impl ModuleFactory for TestFactory {
    fn id(&self) -> ModuleId { TestConfig::ID }

    fn register(
        &self,
        registry: &stache_sdk::Registry,
        store: &stache_sdk::ConfigStore,
    ) -> Result<Arc<dyn stache_app::host::ModuleControl>, stache_app::host::HostError> {
        register_module(registry, store, TestModule {
            counters: Arc::clone(&self.counters),
            permission: self.permission,
            gate: self.gate.clone(),
            configure_gate: self.configure_gate.clone(),
        })
    }
}

struct FakePermissions {
    status: Mutex<PermissionStatus>,
    requests: AtomicUsize,
    grant_on_request: Mutex<bool>,
}

impl FakePermissions {
    fn new(status: PermissionStatus) -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(status),
            requests: AtomicUsize::new(0),
            grant_on_request: Mutex::new(false),
        })
    }

    fn set(&self, status: PermissionStatus) { *self.status.lock().expect("status") = status; }
}

impl PermissionProvider for FakePermissions {
    fn status(&self, _: Permission) -> PermissionStatus { *self.status.lock().expect("status") }

    fn request(&self, permission: Permission) -> Result<PermissionStatus, PermissionError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        if *self.grant_on_request.lock().expect("grant") {
            self.set(PermissionStatus::Granted);
        }
        Ok(self.status(permission))
    }
}

/// Records System Settings opens and can fail on demand.
#[derive(Default)]
struct RecordingSettings {
    calls: AtomicUsize,
    fail: AtomicBool,
}

impl SettingsOpener for RecordingSettings {
    fn open_accessibility_settings(&self) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            Err("injected open failure".to_owned())
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
struct FakeCommands {
    runs: Mutex<Vec<Vec<String>>>,
}

impl CommandRunner for FakeCommands {
    fn run(&self, commands: &[String], _: &stache_sdk::CancellationToken) -> Result<(), String> {
        self.runs.lock().expect("runs").push(commands.to_vec());
        Ok(())
    }
}

#[derive(Default)]
struct FailingCommands {
    runs: Mutex<Vec<Vec<String>>>,
}

impl CommandRunner for FailingCommands {
    fn run(&self, commands: &[String], _: &stache_sdk::CancellationToken) -> Result<(), String> {
        self.runs.lock().expect("runs").push(commands.to_vec());
        Err("injected startup failure".to_owned())
    }
}

#[derive(Default)]
struct FakeWorld {
    statuses: Mutex<Vec<HostStatus>>,
}

impl HostUi for FakeWorld {
    fn status(&self, status: &HostStatus) {
        self.statuses.lock().expect("statuses").push(status.clone());
    }

    fn config_changed(&self, _: &[String]) {}
}

#[derive(Default)]
struct FakeInstance {
    released: AtomicBool,
}

impl InstanceOwner for FakeInstance {
    fn release(&self) { self.released.store(true, Ordering::SeqCst); }
}

#[derive(Default)]
struct RecordingRecords {
    lines: Mutex<Vec<String>>,
}

impl TeardownRecordChannel for RecordingRecords {
    fn record(&self, line: &str, _budget: Duration) -> RecordDelivery {
        self.lines.lock().expect("records").push(line.to_owned());
        RecordDelivery::Delivered
    }
}

#[derive(Default)]
struct FakeTerminal {
    actions: Mutex<Vec<TerminalAction>>,
    /// When set, the closure never claims or delivers, so the coordinator is
    /// forced to use its deadline path.
    stall: AtomicBool,
}

impl FakeTerminal {
    fn actions(&self) -> Vec<TerminalAction> { self.actions.lock().expect("actions").clone() }

    fn stall(&self) { self.stall.store(true, Ordering::SeqCst); }
}

impl TerminalDriver for FakeTerminal {
    fn dispatch(&self, action: TerminalAction) -> TerminalProgress {
        self.actions.lock().expect("actions").push(action);
        let progress = TerminalProgress::new();
        if !self.stall.load(Ordering::SeqCst) {
            let _ = progress.claim(std::time::Instant::now());
            progress.mark_delivered();
        }
        progress
    }
}

/// The fake forced exit: no test can end the test process.
#[derive(Default)]
struct FakeExit {
    codes: Mutex<Vec<i32>>,
}

impl ExitProcess for FakeExit {
    fn exit(&self, code: i32) { self.codes.lock().expect("exit codes").push(code); }
}

impl FakeExit {
    fn codes(&self) -> Vec<i32> { self.codes.lock().expect("exit codes").clone() }
}

/// A guarded desktop whose native setter waits on a test barrier.
struct BlockingDesktop {
    inner: FakeDesktop,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl BlockingDesktop {
    const fn new(
        inner: FakeDesktop,
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    ) -> Self {
        Self {
            inner,
            entered: Mutex::new(Some(entered)),
            release: Mutex::new(Some(release)),
        }
    }

    fn block(&self) {
        let entered = self.entered.lock().expect("entered").take();
        if let Some(entered) = entered {
            entered.send(()).expect("the test waits for the native call");
        }
        let release = self.release.lock().expect("release").take();
        if let Some(release) = release {
            release.recv().expect("the test releases the native call");
        }
    }
}

impl DesktopBackend for BlockingDesktop {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> { self.inner.snapshot() }

    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &std::path::Path,
    ) -> Result<(), DesktopError> {
        self.inner.set_image(display, expected, image)
    }
}

impl GuardedDesktopBackend for BlockingDesktop {
    fn snapshot_guarded(
        &self,
        operation: &DesktopOperation,
    ) -> Result<DisplaySnapshot, DesktopError> {
        self.inner.snapshot_guarded(operation)
    }

    fn set_image_guarded(
        &self,
        operation: &DesktopOperation,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &std::path::Path,
    ) -> Result<(), DesktopError> {
        operation.check("desktop image set")?;
        self.block();
        self.inner.set_image_guarded(operation, display, expected, image)
    }
}

/// A guarded desktop that signals each successful native set.
struct SignalingDesktop {
    inner: FakeDesktop,
    applied: mpsc::Sender<()>,
}

impl SignalingDesktop {
    const fn new(inner: FakeDesktop, applied: mpsc::Sender<()>) -> Self { Self { inner, applied } }
}

impl DesktopBackend for SignalingDesktop {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> { self.inner.snapshot() }

    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &std::path::Path,
    ) -> Result<(), DesktopError> {
        self.inner.set_image(display, expected, image)
    }
}

impl GuardedDesktopBackend for SignalingDesktop {
    fn snapshot_guarded(
        &self,
        operation: &DesktopOperation,
    ) -> Result<DisplaySnapshot, DesktopError> {
        self.inner.snapshot_guarded(operation)
    }

    fn set_image_guarded(
        &self,
        operation: &DesktopOperation,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &std::path::Path,
    ) -> Result<(), DesktopError> {
        let outcome = self.inner.set_image_guarded(operation, display, expected, image);
        if outcome.is_ok() {
            let _ = self.applied.send(());
        }
        outcome
    }
}

/// A shutdown wait that advances instantly, so a blocked owner is abandoned
/// without sleeping through the real two-second budget.
struct AdvancingWait {
    start: Instant,
    offset: Mutex<Duration>,
    budget: Duration,
}

impl AdvancingWait {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
            budget: Duration::from_secs(2),
        }
    }
}

impl ShutdownWait for AdvancingWait {
    fn now(&self) -> Instant { self.start + *self.offset.lock().expect("offset") }

    fn deadline(&self) -> Instant { self.start + self.budget }

    fn wait(&self, timeout: Duration) { *self.offset.lock().expect("offset") += timeout; }
}

fn wallpaper_snapshot() -> DisplaySnapshot {
    DisplaySnapshot::new(vec![DisplayEntry {
        id: DisplayId::new(1),
        frame: DisplayFrame { width: 1600.0, height: 900.0 },
        scale: 1.0,
    }])
}

fn write_wallpaper_png(path: &std::path::Path) {
    let pixels = image::RgbImage::from_fn(8, 8, |x, y| {
        #[allow(clippy::cast_possible_truncation)]
        image::Rgb([(x * 32) as u8, (y * 32) as u8, 64])
    });
    let file = fs::File::create(path).expect("image file");
    pixels
        .write_with_encoder(image::codecs::png::PngEncoder::new(file))
        .expect("encode png");
}

fn wallpapers_service(
    fixture: &Fixture,
    backend: Arc<dyn GuardedDesktopBackend>,
) -> Arc<WallpapersService> {
    WallpapersService::new(ServiceConfig {
        paths: fixture.app.clone(),
        source: SourceContext::new(fixture.paths.directory(), Some(fixture.root().to_path_buf())),
        backend,
        clock: Arc::new(SystemRuntimeClock),
        random: Arc::new(SystemRandom),
        processor: Arc::new(RealProcessor),
    })
    .expect("wallpaper service")
}

/// A clock a test jumps explicitly while owner waits sleep real, bounded
/// slices.
///
/// The bounded wait still makes real progress: a healthy owner finishes inside
/// its poll interval and needs no artificial tick. A hung owner is abandoned
/// only when the test advances the clock, which is the same bound production
/// observes against the system clock.
struct TestClock {
    started: Instant,
    offset: Mutex<Duration>,
    wake: Mutex<Option<Wake>>,
}

impl TestClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
            wake: Mutex::new(None),
        })
    }

    fn advance(&self, delta: Duration) {
        *self.offset.lock().expect("offset") += delta;
        let wake = self.wake.lock().expect("wake").clone();
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}

impl TeardownClock for TestClock {
    fn now(&self) -> Instant { self.started + *self.offset.lock().expect("offset") }

    fn pause(&self, remaining: Duration) -> Duration { remaining }

    fn attach(&self, wake: Wake) { *self.wake.lock().expect("wake") = Some(wake); }
}

struct NeverDrainMain {
    jobs: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

impl NeverDrainMain {
    fn new() -> Arc<Self> { Arc::new(Self { jobs: Mutex::new(Vec::new()) }) }

    fn len(&self) -> usize { self.jobs.lock().expect("jobs").len() }
}

impl MainThreadExecutor for NeverDrainMain {
    fn is_main_thread(&self) -> bool { false }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        self.jobs.lock().expect("jobs").push(job);
        Ok(())
    }
}

struct InlineMain;

impl MainThreadExecutor for InlineMain {
    fn is_main_thread(&self) -> bool { true }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        job();
        Ok(())
    }
}

struct QueuedMain {
    jobs: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

impl QueuedMain {
    fn new() -> Arc<Self> { Arc::new(Self { jobs: Mutex::new(Vec::new()) }) }

    fn drain(&self) -> usize {
        let jobs = std::mem::take(&mut *self.jobs.lock().expect("jobs"));
        let count = jobs.len();
        for job in jobs {
            job();
        }
        count
    }
}

impl MainThreadExecutor for QueuedMain {
    fn is_main_thread(&self) -> bool { false }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        self.jobs.lock().expect("jobs").push(job);
        Ok(())
    }
}

#[derive(Default)]
struct FakeWindowHost {
    opened: Mutex<Vec<NativeWindowSpec>>,
    focused: Mutex<Vec<String>>,
    closed: Mutex<Vec<String>>,
    appearances: Mutex<Vec<stache_sdk::Appearance>>,
}

impl WindowHost for FakeWindowHost {
    fn apply_appearance(&self, appearance: stache_sdk::Appearance) {
        self.appearances.lock().expect("appearances").push(appearance);
    }

    fn open(&self, spec: &NativeWindowSpec) -> Result<(), String> {
        self.opened.lock().expect("opened").push(spec.clone());
        Ok(())
    }

    fn focus(&self, label: &str) { self.focused.lock().expect("focused").push(label.to_owned()); }

    fn close(&self, label: &str) -> CloseOutcome {
        self.closed.lock().expect("closed").push(label.to_owned());
        CloseOutcome::Closing
    }
}

#[derive(Default)]
struct RecordingWindows {
    opened: AtomicUsize,
}

impl WindowBackend for RecordingWindows {
    fn open(&self, _: ModuleId, _: &WindowRequest) -> Result<WindowId, WindowError> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        Ok(WindowId::new(
            u64::try_from(self.opened.load(Ordering::SeqCst)).expect("id"),
        ))
    }

    fn focus(&self, _: WindowId) -> Result<(), WindowError> { Ok(()) }

    fn close(&self, _: WindowId) {}

    fn release_owner(&self, _: ModuleId) {}
}

struct Fixture {
    root: tempfile::TempDir,
    paths: ConfigPaths,
    app: AppPaths,
    catalog: ConfigCatalog,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("root");
    let app = AppPaths::resolve(&AppEnvironment {
        home: Some(root.path().to_path_buf()),
        temporary: Some(root.path().to_path_buf()),
    })
    .expect("app paths");
    let paths = ConfigPaths::new(root.path().join("config"));
    let mut catalog = stache_app::catalog::catalog().expect("catalog");
    catalog.register::<TestConfig>(TestConfig::ID).expect("test config");
    Fixture { root, paths, app, catalog }
}

impl Fixture {
    fn root(&self) -> &std::path::Path { self.root.path() }

    fn write(&self, file: &str, text: &str) {
        fs::create_dir_all(self.paths.directory()).expect("config dir");
        fs::write(self.paths.directory().join(file), text).expect("write config");
    }

    fn read(&self, file: &str) -> String {
        fs::read_to_string(self.paths.directory().join(file)).expect("read config")
    }
}

struct Harness {
    handle: HostHandle,
    driver: Option<HostDriver>,
    teardown: Arc<Teardown>,
    clock: Option<Arc<TestClock>>,
    records: Arc<RecordingRecords>,
    instance: Arc<FakeInstance>,
    terminal: Arc<FakeTerminal>,
    exit: Arc<FakeExit>,
    configure_gate: Option<Arc<Gate>>,
    world: Arc<FakeWorld>,
    counters: Arc<Counters>,
    permissions: Arc<FakePermissions>,
    commands: Arc<dyn CommandRunner>,
    main: Arc<dyn MainThreadExecutor>,
    windows: Arc<dyn WindowBackend>,
    recovery: Option<Arc<dyn StartupRecovery>>,
    input: Option<Arc<InputService>>,
    /// Builds the host with the Accessibility baseline (the production gate).
    gated: bool,
    settings: Arc<RecordingSettings>,
    poll: PermissionPoll,
}

impl Harness {
    fn new(
        permissions: Arc<FakePermissions>,
        main: Arc<dyn MainThreadExecutor>,
        commands: Arc<dyn CommandRunner>,
        windows: Arc<dyn WindowBackend>,
    ) -> Self {
        Self::with_teardown(permissions, main, commands, windows, default_teardown())
    }

    fn with_teardown(
        permissions: Arc<FakePermissions>,
        main: Arc<dyn MainThreadExecutor>,
        commands: Arc<dyn CommandRunner>,
        windows: Arc<dyn WindowBackend>,
        parts: TeardownParts,
    ) -> Self {
        let (handle, driver) = new_host(Arc::clone(&parts.teardown));
        Self {
            handle,
            driver: Some(driver),
            teardown: parts.teardown,
            clock: parts.clock,
            records: parts.records,
            instance: parts.instance,
            terminal: parts.terminal,
            exit: parts.exit,
            configure_gate: None,
            world: Arc::new(FakeWorld::default()),
            counters: Arc::new(Counters::default()),
            permissions,
            commands,
            main,
            windows,
            recovery: None,
            input: None,
            gated: false,
            settings: Arc::new(RecordingSettings::default()),
            poll: PermissionPoll {
                waiting: Duration::from_millis(5),
                backoff_start: Duration::from_millis(5),
                backoff_max: Duration::from_millis(5),
            },
        }
    }

    fn start(&mut self, fixture: &Fixture, permission: bool, gate: Option<Arc<Gate>>) {
        self.start_with(fixture, permission, gate, None);
    }

    fn start_with(
        &mut self,
        fixture: &Fixture,
        permission: bool,
        gate: Option<Arc<Gate>>,
        no_tunes: Option<ApplicationControl>,
    ) {
        let mut factories = ModuleFactories::new().with(TestFactory {
            counters: Arc::clone(&self.counters),
            permission,
            gate,
            configure_gate: self.configure_gate.clone(),
        });
        if let Some(control) = no_tunes {
            factories = factories.with(NoTunesFactory::new(control));
        }
        self.start_with_factories(fixture, factories);
    }

    fn start_with_factories(&mut self, fixture: &Fixture, factories: ModuleFactories) {
        let mut driver = self.driver.take().expect("driver");
        if let Some(recovery) = self.recovery.take() {
            driver = driver.with_startup_recovery(recovery);
        }
        if let Some(input) = self.input.take() {
            driver = driver.with_input_service(input);
        }
        let permissions: Arc<dyn PermissionProvider> = self.permissions.clone();
        let commands: Arc<dyn CommandRunner> = self.commands.clone();
        let services = HostServices {
            logger: Arc::new(DiscardLogger),
            log_filter: Arc::new(LogFilter::default()),
            events: Arc::new(EventBus::new()),
            main_thread: Arc::clone(&self.main),
            permissions,
            windows: Arc::clone(&self.windows),
            commands,
            poll: self.poll,
            baseline: if self.gated {
                vec![PermissionRequirement::user_only(Permission::Accessibility)]
            } else {
                Vec::new()
            },
            settings: self.settings.clone(),
        };
        let settings = HostSettings {
            app: fixture.app.clone(),
            config: fixture.paths.clone(),
            catalog: fixture.catalog.clone(),
            factories,
        };
        driver.start(settings, services, self.world.clone()).expect("start host");
        self.teardown.start().expect("start the teardown coordinator");
    }

    fn wait_ready(&self) { wait_for(&self.handle, "ready", |status| status.ready); }

    fn wait_exit(&self) -> bool { self.handle.wait_for_exit(Duration::from_secs(10)) }

    fn wait_teardown(&self) -> stache_app::shutdown::TeardownOutcome {
        self.teardown
            .wait_for_outcome(Duration::from_secs(10))
            .expect("stored teardown outcome")
    }

    fn wait_terminal(&self) {
        assert!(self.teardown.wait_for_terminal(Duration::from_secs(10)));
    }

    fn records(&self) -> Vec<String> { self.records.lines.lock().expect("records").clone() }
}

struct TeardownParts {
    teardown: Arc<Teardown>,
    /// Present only for the stepped fake clock; real-time scenarios leave it
    /// `None` and advance nothing.
    clock: Option<Arc<TestClock>>,
    records: Arc<RecordingRecords>,
    instance: Arc<FakeInstance>,
    terminal: Arc<FakeTerminal>,
    exit: Arc<FakeExit>,
}

fn default_teardown() -> TeardownParts {
    let clock = TestClock::new();
    let records = Arc::new(RecordingRecords::default());
    let instance = Arc::new(FakeInstance::default());
    let terminal = Arc::new(FakeTerminal::default());
    let exit = Arc::new(FakeExit::default());
    let teardown = Teardown::new(
        clock.clone() as Arc<dyn TeardownClock>,
        records.clone() as Arc<dyn TeardownRecordChannel>,
        instance.clone() as Arc<dyn InstanceOwner>,
        Arc::new(DiscardLogger),
    );
    teardown.install_terminal(terminal.clone() as Arc<dyn TerminalDriver>);
    teardown.install_exit(exit.clone() as Arc<dyn ExitProcess>);
    TeardownParts {
        teardown,
        clock: Some(clock),
        records,
        instance,
        terminal,
        exit,
    }
}

/// The production clock: every owner wait, deadline and record budget is real.
///
/// `TEARDOWN-04-01` exercises the assembled coordinator this way; only the
/// blocked record sink builds its own channel, because that channel is the
/// subject rather than the host composition.
fn real_teardown() -> TeardownParts {
    let records = Arc::new(RecordingRecords::default());
    let instance = Arc::new(FakeInstance::default());
    let terminal = Arc::new(FakeTerminal::default());
    let exit = Arc::new(FakeExit::default());
    let teardown = Teardown::new(
        Arc::new(SystemClock) as Arc<dyn TeardownClock>,
        records.clone() as Arc<dyn TeardownRecordChannel>,
        instance.clone() as Arc<dyn InstanceOwner>,
        Arc::new(DiscardLogger),
    );
    teardown.install_terminal(terminal.clone() as Arc<dyn TerminalDriver>);
    teardown.install_exit(exit.clone() as Arc<dyn ExitProcess>);
    TeardownParts {
        teardown,
        clock: None,
        records,
        instance,
        terminal,
        exit,
    }
}

fn wait_for(handle: &HostHandle, label: &str, predicate: impl Fn(&HostStatus) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if predicate(&handle.status()) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Advances a frozen test clock until the blocked owner hop expires.
///
/// Real polling still lets a healthy owner finish; only a hung owner needs the
/// clock to move past its bound.
fn expire_hop(harness: &Harness) {
    let clock = harness.clock.as_ref().expect("a stepped test clock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while harness.teardown.outcome().is_none() {
        assert!(Instant::now() < deadline, "teardown did not reach an outcome");
        std::thread::sleep(Duration::from_millis(10));
        clock.advance(Duration::from_millis(500));
    }
}

/// The blocked runner and the module it holds are both named, in stage order.
///
/// The fake clock can pass a monitor's bound while the monitor is still inside
/// its real-time poll, so a monitor may be named too. No other owner may be.
fn assert_blocked_command_owners(outcome: &stache_app::shutdown::TeardownOutcome) {
    let owners: Vec<&str> = outcome
        .unacknowledged
        .iter()
        .map(String::as_str)
        .filter(|owner| !matches!(*owner, "watcher" | "permission-ticker" | "startup-job"))
        .collect();
    assert_eq!(owners, ["host-worker", "module:testModule"], "{outcome}");
}

fn wait_for_publish(world: &FakeWorld, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if world.statuses.lock().expect("statuses").len() >= count {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {count} published statuses"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn plain_harness() -> Harness {
    Harness::new(
        FakePermissions::new(PermissionStatus::Granted),
        Arc::new(InlineMain),
        Arc::new(FakeCommands::default()),
        Arc::new(RecordingWindows::default()),
    )
}

#[test]
fn a_catalogued_wallpapers_file_stays_unbound_and_inactive() {
    use stache_app::bindings::ConfigFileState;

    let fixture = fixture();
    fixture.write("wallpapers.toml", "enabled = true\npath = \"images\"\n");
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    let status = harness.handle.status();
    let wallpapers = status
        .config
        .iter()
        .find(|file| file.file == "wallpapers.toml")
        .expect("wallpapers is a catalog file in core status");
    assert_eq!(wallpapers.state, ConfigFileState::Valid);
    assert!(
        !wallpapers.active,
        "catalog support alone must not bind a runtime"
    );
    assert!(wallpapers.diagnostic.is_none());
    assert!(status.error.is_none(), "{:?}", status.error);
    assert!(
        !status.modules.iter().any(|module| module.id == "wallpapers"),
        "this harness supplies only TestFactory, so catalog bytes alone register no runtime"
    );
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
    harness.wait_teardown();
    harness.wait_terminal();
}

#[test]
fn a_factory_can_read_the_accepted_config_through_the_factory_context() {
    use stache_sdk::{
        ConfigStore, DiscardLogger, EventBus, Initial, LogFilter, ModuleHandle, ReloadOutcome,
        UnavailableWindows,
    };

    struct WatchFactory {
        counters: Arc<Counters>,
        handle: Mutex<Option<ModuleHandle<TestConfig>>>,
        watch: Mutex<Option<stache_sdk::AcceptedWatch<TestConfig>>>,
    }

    impl ModuleFactory for WatchFactory {
        fn id(&self) -> ModuleId { TestConfig::ID }

        fn register(
            &self,
            registry: &stache_sdk::Registry,
            store: &ConfigStore,
        ) -> Result<Arc<dyn stache_app::host::ModuleControl>, stache_app::host::HostError> {
            let module = TestModule {
                counters: Arc::clone(&self.counters),
                permission: false,
                gate: None,
                configure_gate: None,
            };
            let handle = match store.initial::<TestConfig>(TestConfig::ID)? {
                Initial::Default(config) | Initial::Loaded(config) => {
                    registry.register(module, config)?
                }
                Initial::Invalid(diagnostic) => registry.register_rejected(
                    module,
                    stache_sdk::ConfigDiagnostic::new(diagnostic.message),
                )?,
            };
            store.bind(handle)?;
            *self.handle.lock().expect("handle") = Some(handle);
            Ok(Arc::new(handle))
        }

        fn ipc_handlers(
            &self,
            context: &FactoryContext,
        ) -> Vec<(&'static str, Arc<dyn stache_sdk::ipc::IpcHandler>)> {
            let handle = self.handle.lock().expect("handle").expect("registered");
            let watch = context.accepted_watch(handle).expect("accepted watch");
            *self.watch.lock().expect("watch") = Some(watch);
            Vec::new()
        }
    }

    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = false\nvalue = 1\n");
    let registry = Arc::new(stache_sdk::Registry::new(
        Arc::new(DiscardLogger),
        Arc::new(EventBus::new()),
        Arc::new(InlineMain),
        FakePermissions::new(PermissionStatus::Granted),
        Arc::new(UnavailableWindows),
    ));
    let store = Arc::new(ConfigStore::new(
        Arc::clone(&registry),
        fixture.catalog.clone(),
        fixture.paths.clone(),
        Arc::new(LogFilter::default()),
    ));
    let factory = WatchFactory {
        counters: Arc::new(Counters::default()),
        handle: Mutex::new(None),
        watch: Mutex::new(None),
    };
    factory.register(&registry, &store).expect("register");
    let context = FactoryContext::new(Arc::clone(&registry), Arc::clone(&store));
    factory.ipc_handlers(&context);
    let watch = factory.watch.lock().expect("watch").clone().expect("handler obtained a watch");

    let accepted = watch.read().expect("accepted at registration");
    assert_eq!(accepted.revision, 1);
    assert_eq!(accepted.config.value, 1);

    fixture.write("testModule.toml", "enabled = true\nvalue = 2\n");
    assert_eq!(
        store.reload_one(TestConfig::ID).expect("reload"),
        ReloadOutcome::Applied { revision: 2 }
    );
    let accepted = watch.read().expect("accepted after reload");
    assert_eq!(accepted.revision, 2);
    assert_eq!(accepted.config.value, 2);
    assert!(accepted.config.enabled);
}

#[test]
fn early_shutdown_before_setup_starts_nothing_and_exits_once() {
    let fixture = fixture();
    let mut harness = plain_harness();
    harness.handle.request_shutdown();
    harness.start(&fixture, false, None);
    assert!(harness.wait_exit(), "runner must finish");
    let outcome = harness.wait_teardown();
    assert_eq!(outcome.result, TeardownResult::Clean);
    assert_eq!(outcome.recorded_code, 0);
    harness.wait_terminal();
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(0)]);
    assert_eq!(harness.records().len(), 1);
    assert_eq!(harness.counters.start.load(Ordering::SeqCst), 0);
    assert_eq!(harness.counters.configure.load(Ordering::SeqCst), 0);
    assert!(!harness.handle.status().ready);
    assert!(!fixture.paths.directory().exists());
}

#[test]
fn a_host_startup_failure_reports_a_failure_exit_code() {
    let fixture = fixture();
    let mut first = plain_harness();
    first.start(&fixture, false, None);
    first.wait_ready();

    let mut second = plain_harness();
    second.start(&fixture, false, None);
    assert!(second.wait_exit(), "the failed host must finish");
    let outcome = second.wait_teardown();
    assert_eq!(outcome.result, TeardownResult::Partial);
    second.wait_terminal();
    assert_eq!(
        second.terminal.actions(),
        vec![TerminalAction::Exit(1)],
        "a host startup failure must not be converted to success"
    );
    assert_eq!(outcome.recorded_code, 1);
    assert!(second.handle.status().error.is_some());

    first.handle.request_shutdown();
    assert!(first.wait_exit());
}

#[test]
fn repeated_shutdown_requests_converge_on_one_exit() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = true\n");
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    assert_eq!(harness.counters.start.load(Ordering::SeqCst), 1);
    harness.handle.request_shutdown();
    harness.handle.request_shutdown();
    harness.handle.request_permission();
    assert!(harness.wait_exit(), "runner must finish");
    harness.wait_teardown();
    harness.wait_terminal();
    harness.handle.request_shutdown();
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(0)]);
    assert_eq!(harness.records().len(), 1, "exactly one outcome record");
    assert_eq!(harness.counters.stop.load(Ordering::SeqCst), 1);
    assert!(!harness.handle.status().ready);
}

#[test]
fn a_blocking_startup_command_does_not_delay_ready_controls_or_shutdown() {
    let fixture = fixture();
    let pid_file = fixture.root().join("startup.pid");
    fixture.write(
        "global.toml",
        &format!(
            "exec_on_startup = [\"/bin/sh -c 'echo $$ > {}; exec /bin/sleep 60'\"]\n",
            pid_file.display()
        ),
    );
    let mut harness = Harness::new(
        FakePermissions::new(PermissionStatus::Granted),
        Arc::new(InlineMain),
        Arc::new(stache_app::startup::ProcessCommands::new()),
        Arc::new(RecordingWindows::default()),
    );
    let started = Instant::now();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a blocking startup command delayed host readiness"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while !pid_file.exists() {
        assert!(Instant::now() < deadline, "startup child did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = fs::read_to_string(&pid_file).expect("pid").trim().to_owned();
    // A runner-processed control returns its outcome while startup still runs;
    // an unprocessed one would time out as `Unavailable`.
    let outcome = harness.handle.set_enabled_outcome("missing", true);
    assert!(
        !matches!(
            outcome,
            stache_app::popover_actions::ModuleEnableOutcome::Unavailable { .. }
        ),
        "controls must run while startup is still executing: {outcome:?}"
    );
    assert!(
        fs::read_to_string(&pid_file).is_ok(),
        "startup child still running"
    );
    harness.handle.request_shutdown();
    assert!(harness.wait_exit(), "shutdown must not wait for the child");
    harness.wait_teardown();
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "shutdown waited for the long-lived startup command"
    );
    let status = std::process::Command::new("/bin/ps")
        .args(["-p", &pid, "-o", "stat="])
        .output()
        .expect("ps");
    assert!(
        !status.status.success(),
        "the owned startup child was not reaped: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn startup_commands_run_once_and_reload_never_reruns_them() {
    let fixture = fixture();
    fixture.write("global.toml", "exec_on_startup = [\"open -a Terminal\"]\n");
    fixture.write("testModule.toml", "enabled = false\n");
    let commands = Arc::new(FakeCommands::default());
    let mut harness = Harness::new(
        FakePermissions::new(PermissionStatus::Granted),
        Arc::new(InlineMain),
        commands.clone(),
        Arc::new(RecordingWindows::default()),
    );
    harness.start(&fixture, false, None);
    harness.wait_ready();
    let deadline = Instant::now() + Duration::from_secs(10);
    while commands.runs.lock().expect("runs").is_empty() {
        assert!(Instant::now() < deadline, "startup command did not run");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(commands.runs.lock().expect("runs").len(), 1);
    assert_eq!(commands.runs.lock().expect("runs")[0], vec![
        "open -a Terminal".to_owned()
    ]);

    fixture.write("testModule.toml", "enabled = true\n");
    wait_for(&harness.handle, "reload", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });
    assert_eq!(commands.runs.lock().expect("runs").len(), 1);
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn enable_edits_preserve_comments_and_apply_once() {
    let fixture = fixture();
    let source = "# Keep this comment.\nenabled = false # note\nvalue = 7\n";
    fixture.write("testModule.toml", source);
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    assert_eq!(
        harness.counters.configure.load(Ordering::SeqCst),
        0,
        "disabled module is not configured"
    );

    harness.handle.set_enabled("testModule", true);
    wait_for(&harness.handle, "running", |status| {
        status
            .modules
            .iter()
            .any(|module| module.id == "testModule" && module.state == ModuleState::Running)
    });
    let edited = fixture.read("testModule.toml");
    assert_eq!(
        edited,
        "# Keep this comment.\nenabled = true # note\nvalue = 7\n"
    );
    // The store applies the committed bytes; the later watcher event is an
    // unchanged no-op rather than a second reconfigure.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(harness.counters.configure.load(Ordering::SeqCst), 1);
    assert_eq!(harness.counters.reconfigure.load(Ordering::SeqCst), 0);
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn pause_is_session_only_and_disabled_cannot_resume() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = true\n");
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    harness.handle.pause("testModule");
    wait_for(&harness.handle, "paused", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Paused)
    });
    assert_eq!(fixture.read("testModule.toml"), "enabled = true\n");

    harness.handle.resume("testModule");
    wait_for(&harness.handle, "resumed", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });

    harness.handle.set_enabled("testModule", false);
    wait_for(&harness.handle, "disabled", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Disabled)
    });
    harness.handle.resume("testModule");
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        harness
            .handle
            .status()
            .modules
            .iter()
            .any(|module| module.state == ModuleState::Disabled),
        "a disabled module cannot resume"
    );
    assert!(fixture.read("testModule.toml").contains("enabled = false"));
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn an_invalid_disk_file_is_reported_and_never_overwritten() {
    let fixture = fixture();
    let invalid = "enabled = ";
    fixture.write("testModule.toml", invalid);
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    let status = harness.handle.status();
    assert!(status.config.iter().any(|file| file.file == "testModule.toml"
        && file.state == stache_app::bindings::ConfigFileState::Invalid));
    assert!(
        status
            .modules
            .iter()
            .any(|module| module.id == "testModule" && module.diagnostic.is_some()),
        "the module snapshot keeps its rejected-configuration diagnostic"
    );
    harness.handle.set_enabled("testModule", true);
    wait_for(&harness.handle, "action error", |status| status.error.is_some());
    assert_eq!(
        fixture.read("testModule.toml"),
        invalid,
        "invalid bytes must stay"
    );
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn a_valid_external_edit_is_applied_once_by_the_watcher() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = false\n");
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    fixture.write("testModule.toml", "enabled = true\n");
    wait_for(&harness.handle, "started by watcher", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });
    assert_eq!(harness.counters.configure.load(Ordering::SeqCst), 1);
    fixture.write("testModule.toml", "enabled = true\n");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(harness.counters.configure.load(Ordering::SeqCst), 1);
    assert_eq!(harness.counters.reconfigure.load(Ordering::SeqCst), 0);
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn a_grant_starts_waiting_modules_without_restart_and_prompts_once() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = true\n");
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    *permissions.grant_on_request.lock().expect("grant") = true;
    let mut harness = Harness::new(
        permissions.clone(),
        Arc::new(InlineMain),
        Arc::new(FakeCommands::default()),
        Arc::new(RecordingWindows::default()),
    );
    harness.start(&fixture, true, None);
    wait_for(&harness.handle, "granted start", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });
    assert_eq!(permissions.requests.load(Ordering::SeqCst), 1);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        permissions.requests.load(Ordering::SeqCst),
        1,
        "only one automatic prompt"
    );
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn revocation_stops_the_module_and_a_new_grant_recovers_without_restart() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = true\n");
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    *permissions.grant_on_request.lock().expect("grant") = true;
    let mut harness = Harness::new(
        permissions.clone(),
        Arc::new(InlineMain),
        Arc::new(FakeCommands::default()),
        Arc::new(RecordingWindows::default()),
    );
    harness.start(&fixture, true, None);
    wait_for(&harness.handle, "running", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });
    permissions.set(PermissionStatus::Denied);
    wait_for(&harness.handle, "revoked", |status| {
        status
            .modules
            .iter()
            .any(|module| module.state == ModuleState::WaitingForPermission)
    });
    assert_eq!(harness.counters.stop.load(Ordering::SeqCst), 1);
    assert_eq!(
        permissions.requests.load(Ordering::SeqCst),
        1,
        "revocation must not prompt"
    );

    permissions.set(PermissionStatus::Granted);
    wait_for(&harness.handle, "recovered", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });
    assert_eq!(harness.counters.start.load(Ordering::SeqCst), 2);
    assert_eq!(permissions.requests.load(Ordering::SeqCst), 1);
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn a_cli_reload_publishes_the_new_module_state() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = false\n");
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    fixture.write("testModule.toml", "enabled = true\n");
    let client = IpcClient::new(fixture.app.socket_path().expect("socket"));
    let report = client.reload().expect("core reload");
    assert!(!report.has_failures(), "{report:?}");
    wait_for(&harness.handle, "published after CLI reload", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn an_unchanged_enable_retries_a_failed_start_instead_of_only_clearing_the_error() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = true\n");
    let mut harness = plain_harness();
    harness.counters.fail_next_start.store(true, Ordering::SeqCst);
    harness.start(&fixture, false, None);
    harness.wait_ready();
    wait_for(&harness.handle, "failed", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Failed)
    });
    assert_eq!(harness.counters.start.load(Ordering::SeqCst), 1);
    harness.handle.set_enabled("testModule", true);
    wait_for(&harness.handle, "retried", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });
    assert_eq!(harness.counters.start.load(Ordering::SeqCst), 2);
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn retry_restarts_a_failed_module() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = true\n");
    let mut harness = plain_harness();
    harness.counters.fail_next_start.store(true, Ordering::SeqCst);
    harness.start(&fixture, false, None);
    harness.wait_ready();
    wait_for(&harness.handle, "failed", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Failed)
    });
    assert_eq!(harness.counters.start.load(Ordering::SeqCst), 1);
    harness.handle.retry("testModule");
    wait_for(&harness.handle, "retried", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });
    assert_eq!(harness.counters.start.load(Ordering::SeqCst), 2);
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn a_startup_command_failure_survives_a_later_successful_action() {
    let fixture = fixture();
    fixture.write("global.toml", "exec_on_startup = [\"false\"]\n");
    fixture.write("testModule.toml", "enabled = true\n");
    let mut harness = Harness::new(
        FakePermissions::new(PermissionStatus::Granted),
        Arc::new(InlineMain),
        Arc::new(FailingCommands::default()),
        Arc::new(RecordingWindows::default()),
    );
    harness.start(&fixture, false, None);
    harness.wait_ready();
    wait_for(&harness.handle, "startup failure", |status| {
        status
            .error
            .as_deref()
            .is_some_and(|error| error.contains("exec_on_startup failed"))
    });

    harness.handle.pause("testModule");
    wait_for(&harness.handle, "paused", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Paused)
    });
    harness.handle.resume("testModule");
    wait_for(&harness.handle, "resumed", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });
    assert!(
        harness
            .handle
            .status()
            .error
            .as_deref()
            .is_some_and(|error| error.contains("exec_on_startup failed")),
        "a later successful action must not hide a failed startup command"
    );
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn a_successful_enable_clears_a_stale_action_error() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = false\n");
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();

    harness.handle.set_enabled("missing", true);
    wait_for(&harness.handle, "action error", |status| {
        status
            .error
            .as_deref()
            .is_some_and(|error| error.contains("no module is registered"))
    });
    harness.handle.set_enabled("testModule", true);
    wait_for(&harness.handle, "enabled", |status| {
        status
            .modules
            .iter()
            .any(|module| module.id == "testModule" && module.state == ModuleState::Running)
    });
    assert_eq!(
        harness.handle.status().error,
        None,
        "a successful enable must clear the previous action error"
    );
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn shutdown_during_a_blocked_start_stops_once_without_deadlock() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = true\n");
    let gate = Gate::new();
    let mut harness = plain_harness();
    harness.start(&fixture, false, Some(gate.clone()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while harness.counters.start.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "module start did not begin");
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.handle.request_shutdown();
    gate.open();
    assert!(
        harness.wait_exit(),
        "shutdown must finish after the start returns"
    );
    harness.wait_teardown();
    assert_eq!(harness.counters.stop.load(Ordering::SeqCst), 1);
    harness.wait_terminal();
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(0)]);
}

#[test]
fn queued_main_thread_work_is_drained_after_shutdown_without_waiting_on_it() {
    let fixture = fixture();
    let queued = QueuedMain::new();
    let host = Arc::new(FakeWindowHost::default());
    let windows = Arc::new(NativeWindows::new(
        queued.clone(),
        host.clone(),
        Arc::new(DiscardLogger),
    ));
    // A gated, denied host opens the Accessibility guide on its own.
    let mut harness = Harness::new(
        FakePermissions::new(PermissionStatus::Denied),
        queued.clone(),
        Arc::new(FakeCommands::default()),
        windows.clone(),
    );
    harness.gated = true;
    harness.start(&fixture, false, None);
    harness.wait_ready();
    harness
        .teardown
        .register_owner(NativeSlot::Windows, Arc::new(WindowsOwner::new(windows.clone())));
    let deadline = Instant::now() + Duration::from_secs(10);
    while windows.is_empty() {
        assert!(Instant::now() < deadline, "the guide window was not requested");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(host.opened.lock().expect("opened").is_empty());
    // The reservation is visible just before its job is queued.
    while queued.drain() == 0 {
        assert!(Instant::now() < deadline, "the open job was not queued");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(host.opened.lock().expect("opened").len(), 1);

    harness.handle.request_shutdown();
    assert!(
        harness.wait_exit(),
        "shutdown must not wait for main-thread jobs"
    );
    assert!(host.closed.lock().expect("closed").is_empty());
    // The registered owner queued one close hop; draining it is the only thing
    // that can acknowledge the window.
    let mut drained = 0;
    while drained == 0 {
        assert!(Instant::now() < deadline, "the close job was not queued");
        drained = queued.drain();
        if drained == 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert_eq!(drained, 1);
    assert_eq!(host.closed.lock().expect("closed").len(), 1);
    let label = host.closed.lock().expect("closed")[0].clone();
    windows.window_closed(&label);
    assert_eq!(harness.wait_teardown().result, TeardownResult::Clean);
    harness.wait_terminal();
}

#[test]
fn an_off_main_executor_that_never_drains_still_reaches_the_outcome() {
    let fixture = fixture();
    let main = NeverDrainMain::new();
    let windows = Arc::new(NativeWindows::new(
        main.clone(),
        Arc::new(FakeWindowHost::default()),
        Arc::new(DiscardLogger),
    ));
    // A gated, denied host opens the Accessibility guide on its own.
    let mut harness = Harness::new(
        FakePermissions::new(PermissionStatus::Denied),
        main.clone(),
        Arc::new(FakeCommands::default()),
        windows,
    );
    harness.gated = true;
    harness.start(&fixture, false, None);
    harness.wait_ready();
    let deadline = Instant::now() + Duration::from_secs(10);
    while main.len() == 0 {
        assert!(Instant::now() < deadline, "the open job was not queued");
        std::thread::sleep(Duration::from_millis(5));
    }

    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
    let outcome = harness.wait_teardown();
    harness.wait_terminal();
    assert_eq!(outcome.result, TeardownResult::Clean);
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(0)]);
    assert!(
        main.len() >= 1,
        "the executor never drained, so teardown cannot have waited on it"
    );
    assert!(harness.exit.codes().is_empty());
}

/// A host harness whose production-composed blocker runs over fake native
/// services: no workspace observation, termination or launch ever reaches
/// `AppKit`.
struct NotunesHarness {
    harness: Harness,
    backend: Arc<FakeApplicationBackend>,
    windows: Arc<RecordingWindows>,
    executor: Arc<ManualExecutor>,
}

impl NotunesHarness {
    fn new(permissions: Arc<FakePermissions>) -> Self {
        let backend = Arc::new(FakeApplicationBackend::new());
        let windows = Arc::new(RecordingWindows::default());
        let executor = Arc::new(ManualExecutor::new(true));
        let harness = Harness::new(
            permissions,
            executor.clone(),
            Arc::new(FakeCommands::default()),
            windows.clone(),
        );
        Self {
            harness,
            backend,
            windows,
            executor,
        }
    }

    fn start(&mut self, fixture: &Fixture) {
        let control = ApplicationControl::new(self.executor.clone(), self.backend.clone());
        self.harness.start_with(fixture, false, None, Some(control));
    }

    fn state(&self) -> Option<ModuleState> {
        self.harness
            .handle
            .status()
            .modules
            .into_iter()
            .find(|module| module.id == "notunes")
            .map(|module| module.state)
    }
}

/// The status row the popover's Overview reads for one module.
fn module_view(status: &HostStatus, id: &str) -> ModuleStatusView {
    status
        .modules
        .iter()
        .find(|module| module.id == id)
        .cloned()
        .unwrap_or_else(|| panic!("no module `{id}`"))
}

/// Enabled, running and not paused: what the Overview shows as a checked
/// enable switch and a Pause action.
fn assert_enabled_running(status: &HostStatus, id: &str) {
    let view = module_view(status, id);
    assert!(view.enabled && !view.paused, "{view:?}");
    assert_eq!(view.state, ModuleState::Running, "{view:?}");
}

#[test]
fn notunes_start_publishes_generic_status_without_opening_anything() {
    let fixture = fixture();
    fixture.write("notunes.toml", "enabled = true\n");
    let mut notunes = NotunesHarness::new(FakePermissions::new(PermissionStatus::Granted));
    let music = notunes.backend.add_running("com.apple.Music");
    notunes.start(&fixture);
    notunes.harness.wait_ready();
    wait_for(&notunes.harness.handle, "notunes running", |status| {
        status
            .modules
            .iter()
            .any(|module| module.id == "notunes" && module.state == ModuleState::Running)
    });

    let status = notunes.harness.handle.status();
    let notunes_view = status
        .modules
        .iter()
        .find(|module| module.id == "notunes")
        .expect("generic status lists the blocker");
    assert!(notunes_view.enabled);
    assert!(notunes_view.diagnostic.is_none());
    assert!(
        status.modules.iter().any(|module| module.id == "testModule"),
        "the blocker shares the generic status snapshot with every module"
    );
    assert!(status.error.is_none(), "{:?}", status.error);

    assert_enabled_running(&status, "notunes");
    let first = status
        .modules
        .iter()
        .position(|module| module.id == "testModule")
        .expect("test module");
    let blocker = status
        .modules
        .iter()
        .position(|module| module.id == "notunes")
        .expect("blocker");
    assert!(first < blocker, "status keeps registration order");

    assert_eq!(
        notunes.backend.termination_requests(),
        vec![music],
        "the initial sweep force-terminates the already-running player"
    );
    assert_eq!(
        notunes.backend.list_requests(),
        vec!["com.apple.Music".to_owned(), "com.apple.iTunes".to_owned()],
        "the sweep classifies both exact bundle identifiers"
    );
    assert!(
        notunes.backend.open_requests().is_empty(),
        "the initial sweep never launches a replacement"
    );
    assert_eq!(
        notunes.windows.opened.load(Ordering::SeqCst),
        0,
        "enabling the blocker creates no window or webview"
    );

    notunes.harness.handle.request_shutdown();
    assert!(notunes.harness.wait_exit());
}

#[test]
fn an_invalid_notunes_file_is_diagnosed_and_recovers_after_a_valid_rewrite() {
    let fixture = fixture();
    fixture.write("notunes.toml", "target_app = \"winamp\"\n");
    let mut notunes = NotunesHarness::new(FakePermissions::new(PermissionStatus::Granted));
    notunes.start(&fixture);
    notunes.harness.wait_ready();

    let status = notunes.harness.handle.status();
    let view = status
        .config
        .iter()
        .find(|file| file.file == "notunes.toml")
        .expect("generic config diagnostics list the file");
    assert_eq!(
        view.state,
        stache_app::bindings::ConfigFileState::Invalid,
        "{view:?}"
    );
    assert!(
        view.diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("target_app")),
        "{view:?}"
    );
    let module = status
        .modules
        .iter()
        .find(|module| module.id == "notunes")
        .expect("blocker snapshot");
    assert_eq!(
        module.state,
        ModuleState::Stopped,
        "a rejected file registers without starting the module"
    );
    assert!(
        module.diagnostic.is_some(),
        "the rejection stays on the module snapshot"
    );
    assert!(status.error.is_none(), "{:?}", status.error);
    assert_eq!(
        notunes.backend.observations(),
        0,
        "a rejected configuration installs no observer"
    );

    fixture.write(
        "notunes.toml",
        "enabled = true\ntarget_app = \"com.example.player\"\n",
    );
    wait_for(&notunes.harness.handle, "recovered blocker", |status| {
        status
            .modules
            .iter()
            .any(|module| module.id == "notunes" && module.state == ModuleState::Running)
    });
    let status = notunes.harness.handle.status();
    let view = status
        .config
        .iter()
        .find(|file| file.file == "notunes.toml")
        .expect("config entry");
    assert_eq!(view.state, stache_app::bindings::ConfigFileState::Valid);
    let module = status
        .modules
        .iter()
        .find(|module| module.id == "notunes")
        .expect("blocker snapshot");
    assert!(
        module.diagnostic.is_none(),
        "an accepted reload clears the rejection"
    );
    assert!(status.error.is_none(), "{:?}", status.error);
    assert_eq!(notunes.backend.observations(), 1);

    notunes.harness.handle.request_shutdown();
    assert!(notunes.harness.wait_exit());
}

#[test]
fn notunes_follows_enable_pause_resume_disable_and_releases_observation() {
    let fixture = fixture();
    fixture.write("notunes.toml", "enabled = false\n");
    let mut notunes = NotunesHarness::new(FakePermissions::new(PermissionStatus::Granted));
    notunes.start(&fixture);
    notunes.harness.wait_ready();
    assert_eq!(notunes.state(), Some(ModuleState::Disabled));
    assert_eq!(
        notunes.backend.observations(),
        0,
        "a disabled blocker holds no observation"
    );

    notunes.harness.handle.set_enabled("notunes", true);
    wait_for(&notunes.harness.handle, "enabled blocker", |status| {
        status
            .modules
            .iter()
            .any(|module| module.id == "notunes" && module.state == ModuleState::Running)
    });
    assert_eq!(notunes.backend.observations(), 1);

    notunes.harness.handle.pause("notunes");
    wait_for(&notunes.harness.handle, "paused blocker", |status| {
        status
            .modules
            .iter()
            .any(|module| module.id == "notunes" && module.state == ModuleState::Paused)
    });
    assert_eq!(notunes.backend.observations(), 0, "pause removes the observer");

    notunes.harness.handle.resume("notunes");
    wait_for(&notunes.harness.handle, "resumed blocker", |status| {
        status
            .modules
            .iter()
            .any(|module| module.id == "notunes" && module.state == ModuleState::Running)
    });
    assert_eq!(notunes.backend.observations(), 1);

    notunes.harness.handle.set_enabled("notunes", false);
    wait_for(&notunes.harness.handle, "disabled blocker", |status| {
        status
            .modules
            .iter()
            .any(|module| module.id == "notunes" && module.state == ModuleState::Disabled)
    });
    assert_eq!(notunes.backend.observations(), 0, "disable removes the observer");

    notunes.harness.handle.request_shutdown();
    assert!(notunes.harness.wait_exit());
    notunes.harness.wait_teardown();
    assert_eq!(
        notunes.backend.observations(),
        0,
        "shutdown leaves no live observer"
    );
}

#[test]
fn a_denied_permission_neither_blocks_notunes_nor_prompts() {
    let fixture = fixture();
    fixture.write("notunes.toml", "enabled = true\n");
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut notunes = NotunesHarness::new(permissions.clone());
    notunes.start(&fixture);
    notunes.harness.wait_ready();
    wait_for(&notunes.harness.handle, "notunes running", |status| {
        status
            .modules
            .iter()
            .any(|module| module.id == "notunes" && module.state == ModuleState::Running)
    });

    notunes.harness.handle.refresh_permissions();
    notunes.harness.handle.request_permission();
    // Both commands publish even when nothing changed, so the recorded status
    // count is the deterministic "they were processed" signal; a trivially true
    // predicate on the running module would not prove the refresh ran.
    let published = notunes.harness.world.statuses.lock().expect("statuses").len();
    wait_for_publish(&notunes.harness.world, published + 2);
    assert_eq!(
        permissions.requests.load(Ordering::SeqCst),
        0,
        "the blocker declares no permission, so nothing may prompt"
    );
    let status = notunes.harness.handle.status();
    assert!(
        !status
            .modules
            .iter()
            .any(|module| module.state == ModuleState::WaitingForPermission),
        "no module waits for a permission nobody declared"
    );
    assert!(status.error.is_none(), "{:?}", status.error);

    notunes.harness.handle.request_shutdown();
    assert!(notunes.harness.wait_exit());
}

#[test]
fn a_blocked_configuration_commit_is_uncertain_named_and_still_dispatches() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = false\n");
    let gate = Gate::new();
    let mut harness = plain_harness();
    harness.configure_gate = Some(gate.clone());
    harness.start(&fixture, false, None);
    harness.wait_ready();

    harness.handle.set_enabled("testModule", true);
    let deadline = Instant::now() + Duration::from_secs(10);
    while harness.counters.configure.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "the configuration commit never blocked"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        fixture.read("testModule.toml").contains("enabled = true"),
        "the configuration commit must already be on disk"
    );

    let requested = Instant::now();
    harness.handle.request_shutdown();
    // Let the polls reap first, then expire the blocked module's hop.
    std::thread::sleep(Duration::from_millis(300));
    expire_hop(&harness);
    let outcome = harness.wait_teardown();
    assert_eq!(outcome.result, TeardownResult::Uncertain);
    assert_eq!(outcome.recorded_code, 1);
    assert!(
        outcome.failed.is_empty(),
        "a blocked command did not report a failure"
    );
    assert_blocked_command_owners(&outcome);
    harness.wait_terminal();
    assert_eq!(
        harness.terminal.actions(),
        vec![TerminalAction::Exit(1)],
        "the terminal action must not wait for the blocked command"
    );
    assert!(
        requested.elapsed() < Duration::from_secs(5),
        "the fake clock must not sleep through the budget"
    );
    assert_eq!(harness.records().len(), 1);
    let published = harness.world.statuses.lock().expect("statuses").len();

    // Let the abandoned command finish; its late answer must not publish, start
    // the module, upgrade the outcome or write a second record.
    gate.open();
    assert!(harness.wait_exit());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(harness.teardown.outcome(), Some(outcome));
    assert_eq!(harness.records().len(), 1);
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(1)]);
    assert_eq!(
        harness.world.statuses.lock().expect("statuses").len(),
        published,
        "a late command completion must not publish"
    );
}

#[test]
fn a_clean_restart_relaunches_once_after_releasing_the_claim() {
    let fixture = fixture();
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();

    harness.handle.request_restart();
    assert!(harness.wait_exit());
    let outcome = harness.wait_teardown();
    assert_eq!(outcome.result, TeardownResult::Clean);
    assert_eq!(outcome.cause, TeardownCause::Restart);
    assert_eq!(outcome.recorded_code, 0);
    harness.wait_terminal();
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Restart]);
    assert!(
        harness.instance.released.load(Ordering::SeqCst),
        "a relaunch must not race the instance lock"
    );
    assert_eq!(harness.records().len(), 1);
    assert!(harness.records()[0].contains("cause=restart"));
}

#[test]
fn a_partial_restart_finishes_without_relaunching() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = true\n");
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    wait_for(&harness.handle, "running", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });

    harness.counters.fail_next_stop.store(true, Ordering::SeqCst);
    harness.handle.request_restart();
    assert!(harness.wait_exit());
    let outcome = harness.wait_teardown();
    assert_eq!(outcome.result, TeardownResult::Partial);
    assert_eq!(outcome.recorded_code, 1);
    assert_eq!(outcome.failed, vec!["module:testModule".to_owned()]);
    harness.wait_terminal();
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(1)]);
    assert!(!harness.instance.released.load(Ordering::SeqCst));
    assert_eq!(harness.records().len(), 1);
    assert!(harness.records()[0].contains("cause=restart"));
}

#[test]
fn an_uncertain_restart_finishes_without_relaunching() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = false\n");
    let gate = Gate::new();
    let mut harness = plain_harness();
    harness.configure_gate = Some(gate.clone());
    harness.start(&fixture, false, None);
    harness.wait_ready();

    harness.handle.set_enabled("testModule", true);
    let deadline = Instant::now() + Duration::from_secs(10);
    while harness.counters.configure.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "the configuration commit never blocked"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    harness.handle.request_restart();
    // Let the polls reap first, then expire the blocked module's hop.
    std::thread::sleep(Duration::from_millis(300));
    expire_hop(&harness);
    let outcome = harness.wait_teardown();
    assert_eq!(outcome.result, TeardownResult::Uncertain);
    assert_blocked_command_owners(&outcome);
    harness.wait_terminal();
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(1)]);
    assert!(!harness.instance.released.load(Ordering::SeqCst));
    assert_eq!(harness.records().len(), 1);
    assert!(harness.records()[0].contains("cause=restart"));

    gate.open();
    assert!(harness.wait_exit());
}

#[test]
fn a_restart_never_relaunches_while_an_appkit_reply_is_pending() {
    let fixture = fixture();
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();

    harness.handle.request_restart();
    assert_eq!(
        harness.teardown.begin_terminate(),
        stache_app::shutdown::TeardownAnswer::Later
    );
    assert!(harness.wait_exit());
    let outcome = harness.wait_teardown();
    assert_eq!(
        outcome.result,
        TeardownResult::Clean,
        "the host answered; only the reply pending state blocks relaunch"
    );
    harness.wait_terminal();
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Reply]);
    assert!(!harness.instance.released.load(Ordering::SeqCst));
}

#[test]
fn only_the_first_non_zero_exit_code_is_recorded() {
    let fixture = fixture();
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();

    harness.handle.request_shutdown_with_code(Some(0));
    harness.handle.request_shutdown_with_code(Some(7));
    harness.handle.request_shutdown_with_code(Some(3));
    assert!(harness.wait_exit());
    let outcome = harness.wait_teardown();
    assert_eq!(outcome.result, TeardownResult::Clean);
    assert_eq!(outcome.recorded_code, 7);
    harness.wait_terminal();
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(7)]);
}

#[test]
fn a_terminate_install_failure_stays_a_status_condition() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = false\n");
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();

    report_terminate_install_failure(&harness.handle, &DiscardLogger, "rejected selector");
    wait_for(&harness.handle, "terminate condition", |status| {
        status
            .error
            .as_deref()
            .is_some_and(|error| error.contains("terminate observation is unavailable"))
    });

    fixture.write("testModule.toml", "enabled = true\n");
    wait_for(&harness.handle, "later success", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });
    assert!(
        harness
            .handle
            .status()
            .error
            .as_deref()
            .is_some_and(|error| error.contains("terminate observation is unavailable")),
        "a later successful action must not hide the installation failure"
    );

    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

/// How a real-thread owner answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnerMode {
    Acknowledge,
    Fail,
    Hang,
}

/// A fake native owner whose `release` runs on the coordinator's real thread.
struct RealOwner {
    name: &'static str,
    on_main: bool,
    mode: OwnerMode,
    releases: AtomicUsize,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    pending: Mutex<Option<OwnerAnswer>>,
}

impl RealOwner {
    fn new(name: &'static str, on_main: bool, mode: OwnerMode) -> Arc<Self> {
        Arc::new(Self {
            name,
            on_main,
            mode,
            releases: AtomicUsize::new(0),
            entered: Mutex::new(None),
            pending: Mutex::new(None),
        })
    }

    /// The same owner with a one-shot signal fired from `release`.
    fn with_signal(
        name: &'static str,
        on_main: bool,
        mode: OwnerMode,
    ) -> (Arc<Self>, mpsc::Receiver<()>) {
        let (sender, receiver) = mpsc::channel();
        let owner = Arc::new(Self {
            name,
            on_main,
            mode,
            releases: AtomicUsize::new(0),
            entered: Mutex::new(Some(sender)),
            pending: Mutex::new(None),
        });
        (owner, receiver)
    }

    fn releases(&self) -> usize { self.releases.load(Ordering::SeqCst) }
}

impl NativeOwner for RealOwner {
    fn name(&self) -> &'static str { self.name }

    fn on_main(&self) -> bool { self.on_main }

    fn release(&self) -> OwnerAnswer {
        self.releases.fetch_add(1, Ordering::SeqCst);
        let entered = self.entered.lock().expect("entered").take();
        if let Some(entered) = entered {
            let _ = entered.send(());
        }
        let answer = OwnerAnswer::new();
        match self.mode {
            OwnerMode::Acknowledge => answer.succeed(),
            OwnerMode::Fail => answer.fail(),
            OwnerMode::Hang => {
                *self.pending.lock().expect("pending") = Some(answer.clone());
            }
        }
        answer
    }
}

/// A real thread that drains `ManualExecutor`, so main-hop owners can answer.
struct DrainThread {
    executor: Arc<ManualExecutor>,
    stop: Arc<AtomicBool>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl DrainThread {
    fn new() -> Arc<Self> {
        let executor = Arc::new(ManualExecutor::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&executor);
        let stop_worker = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("test-main-drain".to_owned())
            .spawn(move || {
                while !stop_worker.load(Ordering::SeqCst) {
                    if worker.drain() == 0 {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }
                worker.drain();
            })
            .expect("drain thread");
        Arc::new(Self {
            executor,
            stop,
            handle: Mutex::new(Some(handle)),
        })
    }

    fn main_executor(&self) -> Arc<dyn MainThreadExecutor> { self.executor.clone() }

    fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let handle = self.handle.lock().expect("handle").take();
        if let Some(handle) = handle {
            handle.join().expect("drain thread");
        }
    }
}

impl Drop for DrainThread {
    fn drop(&mut self) { self.stop(); }
}

fn real_harness(main: Arc<dyn MainThreadExecutor>) -> Harness {
    Harness::with_teardown(
        FakePermissions::new(PermissionStatus::Granted),
        main,
        Arc::new(FakeCommands::default()),
        Arc::new(RecordingWindows::default()),
        real_teardown(),
    )
}

/// `TEARDOWN-04-01`: real threads, real clock, every owner acknowledges.
///
/// The direct owner answers itself; the main-hop owner is answered only by the
/// closure the draining thread runs, so the queue-draining executor is part of
/// the proof rather than an inline shortcut.
#[test]
fn real_threads_all_owners_acknowledge_clean() {
    let fixture = fixture();
    let drain = DrainThread::new();
    let mut harness = real_harness(drain.main_executor());
    harness.start(&fixture, false, None);
    harness.wait_ready();

    let direct = RealOwner::new("ack-direct", false, OwnerMode::Acknowledge);
    let hop_calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&hop_calls);
    let hop = Arc::new(MainHopOwner::new("ack-main", drain.main_executor(), move || {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    harness.teardown.register_owner(NativeSlot::Hud, direct.clone());
    harness.teardown.register_owner(NativeSlot::Tray, hop);

    harness.handle.request_shutdown();
    assert!(harness.wait_exit(), "the host runner must finish");
    let outcome = harness.wait_teardown();
    harness.wait_terminal();

    assert_eq!(outcome.result, TeardownResult::Clean, "{outcome}");
    assert!(!outcome.escalated, "{outcome}");
    assert_eq!(outcome.recorded_code, 0);
    assert!(outcome.failed.is_empty(), "{outcome}");
    assert!(outcome.unacknowledged.is_empty(), "{outcome}");
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(0)]);
    assert_eq!(harness.records().len(), 1, "exactly one outcome record");
    assert_eq!(direct.releases(), 1, "the owner released exactly once");
    assert_eq!(
        hop_calls.load(Ordering::SeqCst),
        1,
        "the draining executor ran the hop"
    );
    drain.stop();
    assert!(
        harness.handle.is_finished(),
        "the runner finished before the test ended"
    );
}

/// `TEARDOWN-04-01`: a failing owner makes the outcome `Partial`.
#[test]
fn real_threads_a_failing_owner_is_partial() {
    let fixture = fixture();
    let drain = DrainThread::new();
    let mut harness = real_harness(drain.main_executor());
    harness.start(&fixture, false, None);
    harness.wait_ready();

    let owner = RealOwner::new("fail-owner", false, OwnerMode::Fail);
    harness.teardown.register_owner(NativeSlot::Hud, owner.clone());

    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
    let outcome = harness.wait_teardown();
    harness.wait_terminal();

    assert_eq!(outcome.result, TeardownResult::Partial, "{outcome}");
    assert_eq!(outcome.failed, vec!["fail-owner".to_owned()], "{outcome}");
    assert!(outcome.unacknowledged.is_empty(), "{outcome}");
    assert_eq!(outcome.recorded_code, 1);
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(1)]);
    assert_eq!(harness.records().len(), 1);
    assert_eq!(owner.releases(), 1);
    drain.stop();
}

/// `TEARDOWN-04-01`: a hung owner is abandoned at its real bound, not at the
/// total deadline, and is named in the outcome.
#[test]
fn real_threads_a_hung_owner_is_uncertain_by_the_deadline() {
    let fixture = fixture();
    let drain = DrainThread::new();
    let mut harness = real_harness(drain.main_executor());
    harness.start(&fixture, false, None);
    harness.wait_ready();

    let (owner, entered) = RealOwner::with_signal("hang-owner", false, OwnerMode::Hang);
    harness.teardown.register_owner(NativeSlot::Hud, owner.clone());

    harness.handle.request_shutdown();
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("the hung owner was asked to release");
    assert!(harness.wait_exit());
    let outcome = harness.wait_teardown();
    harness.wait_terminal();

    assert_eq!(outcome.result, TeardownResult::Uncertain, "{outcome}");
    assert_eq!(
        outcome.unacknowledged,
        vec!["hang-owner".to_owned()],
        "{outcome}"
    );
    assert!(outcome.failed.is_empty(), "{outcome}");
    assert_eq!(outcome.recorded_code, 1);
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(1)]);
    assert_eq!(harness.records().len(), 1);
    assert_eq!(owner.releases(), 1);
    assert!(
        outcome.elapsed >= stache_sdk::shutdown::OWNER_WAIT_BUDGET,
        "the hung owner must be given its full bound: {outcome}"
    );
    assert!(
        outcome.elapsed <= TEARDOWN_TOTAL + Duration::from_millis(500),
        "the outcome must stay at the deadline despite scheduling noise: {outcome}"
    );
    drain.stop();
}

/// `TEARDOWN-04-01`: a runner stuck in a configuration commit is named even
/// though every registered owner acknowledged.
#[test]
fn real_threads_a_blocked_host_worker_is_uncertain_despite_acknowledged_owners() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = false\n");
    let gate = Gate::new();
    let drain = DrainThread::new();
    let mut harness = real_harness(drain.main_executor());
    harness.configure_gate = Some(gate.clone());
    harness.start(&fixture, false, None);
    harness.wait_ready();

    let direct = RealOwner::new("ack-direct", false, OwnerMode::Acknowledge);
    harness.teardown.register_owner(NativeSlot::Hud, direct.clone());
    harness.handle.set_enabled("testModule", true);
    let deadline = Instant::now() + Duration::from_secs(10);
    while harness.counters.configure.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "the configuration commit never blocked"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let requested = Instant::now();
    harness.handle.request_shutdown();
    let outcome = harness.wait_teardown();
    harness.wait_terminal();

    assert_eq!(outcome.result, TeardownResult::Uncertain, "{outcome}");
    assert!(
        outcome.unacknowledged.iter().any(|owner| owner == "host-worker"),
        "the blocked runner must be named: {outcome}"
    );
    assert!(
        !outcome.unacknowledged.iter().any(|owner| owner == "ack-direct"),
        "an acknowledged owner must not be named: {outcome}"
    );
    assert!(outcome.failed.is_empty(), "{outcome}");
    assert_eq!(outcome.recorded_code, 1);
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(1)]);
    assert_eq!(harness.records().len(), 1);
    assert_eq!(direct.releases(), 1);
    assert!(
        requested.elapsed() <= TEARDOWN_TOTAL + Duration::from_secs(1),
        "the blocked runner must not extend the total bound: {:?}",
        requested.elapsed()
    );

    gate.open();
    assert!(harness.wait_exit());
    drain.stop();
}

/// `TEARDOWN-04-01`: a second request during a hang shortens the deadline and
/// ends the run inside the escalation bound.
#[test]
fn real_threads_escalation_ends_a_hung_owner_within_the_escalation_bound() {
    let fixture = fixture();
    let drain = DrainThread::new();
    let mut harness = real_harness(drain.main_executor());
    harness.start(&fixture, false, None);
    harness.wait_ready();

    let (owner, entered) = RealOwner::with_signal("hang-owner", false, OwnerMode::Hang);
    harness.teardown.register_owner(NativeSlot::Hud, owner);

    harness.handle.request_shutdown();
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("the hung owner was asked to release");
    // Let the first wait enter its bounded loop before the escalation lands.
    std::thread::sleep(Duration::from_millis(50));
    let second = Instant::now();
    harness.teardown.request(TeardownCause::Request, None);

    let outcome = harness.wait_teardown();
    let reacted = second.elapsed();
    harness.wait_terminal();

    assert!(outcome.escalated, "{outcome}");
    assert_eq!(outcome.result, TeardownResult::Uncertain, "{outcome}");
    assert!(
        outcome.unacknowledged.iter().any(|owner| owner == "hang-owner"),
        "the abandoned owner must be named: {outcome}"
    );
    assert_eq!(outcome.recorded_code, 1);
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(1)]);
    assert_eq!(harness.records().len(), 1);
    assert!(
        reacted < Duration::from_millis(750),
        "the second request must end the hang within 250 ms plus scheduling allowance, took {reacted:?}"
    );
    assert!(harness.wait_exit());
    drain.stop();
}

/// `TEARDOWN-04-01`: concurrent requests from real threads converge on one run.
#[test]
fn real_threads_concurrent_requests_produce_one_run_and_one_outcome() {
    let fixture = fixture();
    let drain = DrainThread::new();
    let mut harness = real_harness(drain.main_executor());
    harness.start(&fixture, false, None);
    harness.wait_ready();

    let mut threads = Vec::new();
    for _ in 0..8 {
        let teardown = Arc::clone(&harness.teardown);
        threads.push(std::thread::spawn(move || {
            teardown.request(TeardownCause::Request, None);
        }));
    }
    for thread in threads {
        thread.join().expect("request thread");
    }
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
    let outcome = harness.wait_teardown();
    harness.wait_terminal();

    assert_eq!(harness.teardown.outcome(), Some(outcome.clone()));
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(
        outcome.recorded_code
    )]);
    assert_eq!(harness.records().len(), 1, "exactly one outcome record");
    harness.teardown.request(TeardownCause::Restart, None);
    assert_eq!(
        harness.terminal.actions().len(),
        1,
        "a later request changes nothing"
    );
    drain.stop();
}

/// A sink that records its line, signals, then holds its writer thread until
/// the test releases it.
struct HeldSink {
    lines: Mutex<Vec<String>>,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl TeardownRecordSink for HeldSink {
    fn write(&self, line: &str) {
        self.lines.lock().expect("lines").push(line.to_owned());
        let entered = self.entered.lock().expect("entered").take();
        if let Some(entered) = entered {
            let _ = entered.send(());
        }
        let release = self.release.lock().expect("release").take();
        if let Some(release) = release {
            let _ = release.recv();
        }
    }
}

/// `TEARDOWN-04-01`: a blocked record sink keeps the in-memory slot readable,
/// delays dispatch only by its bounded attempt, and leaves the forced exit on
/// its own deadline.
#[test]
fn a_blocked_record_sink_keeps_the_slot_readable_and_bounds_the_forced_exit() {
    let fixture = fixture();
    let (entered, began) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let sink = Arc::new(HeldSink {
        lines: Mutex::new(Vec::new()),
        entered: Mutex::new(Some(entered)),
        release: Mutex::new(Some(released)),
    });
    let lines = Arc::clone(&sink);
    let terminal = Arc::new(FakeTerminal::default());
    terminal.stall();
    let exit = Arc::new(FakeExit::default());
    let instance = Arc::new(FakeInstance::default());
    let teardown = Teardown::new(
        Arc::new(SystemClock) as Arc<dyn TeardownClock>,
        Arc::new(BoundedRecordChannel::new(sink as Arc<dyn TeardownRecordSink>))
            as Arc<dyn TeardownRecordChannel>,
        instance.clone() as Arc<dyn InstanceOwner>,
        Arc::new(DiscardLogger),
    );
    teardown.install_terminal(terminal.clone() as Arc<dyn TerminalDriver>);
    teardown.install_exit(exit.clone() as Arc<dyn ExitProcess>);
    let mut harness = Harness::with_teardown(
        FakePermissions::new(PermissionStatus::Granted),
        Arc::new(InlineMain),
        Arc::new(FakeCommands::default()),
        Arc::new(RecordingWindows::default()),
        TeardownParts {
            teardown,
            clock: None,
            records: Arc::new(RecordingRecords::default()),
            instance,
            terminal,
            exit,
        },
    );
    harness.start(&fixture, false, None);
    harness.wait_ready();

    let requested = Instant::now();
    harness.handle.request_shutdown();
    assert!(harness.wait_exit(), "the host runner must finish");
    began
        .recv_timeout(Duration::from_secs(2))
        .expect("the sink received the record attempt");
    let sink_attempted = Instant::now();
    let outcome = harness.teardown.outcome().expect("the slot stays readable");
    assert_eq!(outcome.result, TeardownResult::Clean, "{outcome}");
    assert!(
        harness.teardown.wait_for_terminal(Duration::from_secs(2)),
        "dispatch must not wait for the blocked sink"
    );
    let dispatched = Instant::now();
    assert_eq!(harness.teardown.delivery(), RecordDelivery::Unavailable);
    assert!(
        dispatched.duration_since(sink_attempted) < Duration::from_millis(500),
        "the sink may add only its 50 ms attempt before dispatch, took {:?}",
        dispatched.duration_since(sink_attempted)
    );

    let deadline = Instant::now() + TEARDOWN_TOTAL + Duration::from_secs(2);
    while harness.exit.codes().is_empty() {
        assert!(Instant::now() < deadline, "the forced exit did not happen");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(harness.exit.codes(), vec![0]);
    assert!(
        requested.elapsed() <= TEARDOWN_TOTAL + Duration::from_secs(1),
        "the forced exit must stay on its bound, took {:?}",
        requested.elapsed()
    );
    assert_eq!(
        lines.lines.lock().expect("lines").len(),
        1,
        "one outcome record reached the sink"
    );
    release.send(()).expect("release the sink");
}

fn event_log(events: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    events.lock().expect("events").clone()
}

fn wait_for_event(events: &Arc<Mutex<Vec<String>>>, event: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !event_log(events).iter().any(|known| known == event) {
        assert!(Instant::now() < deadline, "timed out waiting for `{event}`");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// One factory that either registers the test module or fails before it does.
struct StartupFactory {
    id: &'static str,
    fail: bool,
    events: Arc<Mutex<Vec<String>>>,
}

impl StartupFactory {
    fn push(&self, event: &str) {
        self.events.lock().expect("events").push(format!("{event}:{}", self.id));
    }
}

impl ModuleFactory for StartupFactory {
    fn id(&self) -> ModuleId { ModuleId::new(self.id) }

    fn register(
        &self,
        registry: &stache_sdk::Registry,
        store: &stache_sdk::ConfigStore,
    ) -> Result<Arc<dyn stache_app::host::ModuleControl>, stache_app::host::HostError> {
        if self.fail {
            return Err(stache_app::host::HostError::Thread(
                "injected registration failure".to_owned(),
            ));
        }
        register_module(registry, store, TestModule {
            counters: Arc::new(Counters::default()),
            permission: false,
            gate: None,
            configure_gate: None,
        })
    }

    fn begin_shutdown(&self) { self.push("begin"); }

    fn finish_shutdown(&self, _: &dyn stache_sdk::shutdown::ShutdownWait) -> Result<bool, String> {
        self.push("finish");
        Ok(true)
    }
}

#[test]
fn a_partial_factory_startup_still_closes_every_begin_and_finish_hook() {
    let fixture = fixture();
    let events = Arc::new(Mutex::new(Vec::new()));
    let factories = ModuleFactories::new()
        .with(StartupFactory {
            id: "missing",
            fail: true,
            events: Arc::clone(&events),
        })
        .with(StartupFactory {
            id: "testModule",
            fail: false,
            events: Arc::clone(&events),
        });
    let mut harness = plain_harness();
    harness.start_with_factories(&fixture, factories);

    assert!(harness.wait_exit(), "the failed runner must finish");
    let outcome = harness.wait_teardown();
    assert_eq!(outcome.result, TeardownResult::Partial, "{outcome}");
    assert_eq!(event_log(&events), [
        "begin:missing",
        "begin:testModule",
        "finish:testModule",
        "finish:missing",
    ]);
}

struct ServiceMarker;

/// A factory whose drain blocks until a test releases it.
struct BlockedFinishFactory {
    events: Arc<Mutex<Vec<String>>>,
    gate: Arc<Gate>,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    _service: Arc<ServiceMarker>,
}

impl ModuleFactory for BlockedFinishFactory {
    fn id(&self) -> ModuleId { TestConfig::ID }

    fn register(
        &self,
        registry: &stache_sdk::Registry,
        store: &stache_sdk::ConfigStore,
    ) -> Result<Arc<dyn stache_app::host::ModuleControl>, stache_app::host::HostError> {
        register_module(registry, store, TestModule {
            counters: Arc::new(Counters::default()),
            permission: false,
            gate: None,
            configure_gate: None,
        })
    }

    fn begin_shutdown(&self) { self.events.lock().expect("events").push("begin".to_owned()); }

    fn finish_shutdown(&self, _: &dyn stache_sdk::shutdown::ShutdownWait) -> Result<bool, String> {
        let sender = self.entered.lock().expect("entered").take();
        if let Some(sender) = sender {
            let _ = sender.send(());
        }
        self.gate.wait();
        self.events.lock().expect("events").push("finish".to_owned());
        Ok(true)
    }
}

#[test]
fn factory_state_outlives_the_runner_until_its_drain_returns() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = false\n");
    let events = Arc::new(Mutex::new(Vec::new()));
    let gate = Gate::new();
    let (sender, entered) = mpsc::channel();
    let service = Arc::new(ServiceMarker);
    let retained = Arc::downgrade(&service);
    let factories = ModuleFactories::new().with(BlockedFinishFactory {
        events: Arc::clone(&events),
        gate: Arc::clone(&gate),
        entered: Mutex::new(Some(sender)),
        _service: service,
    });
    let mut harness = plain_harness();
    harness.start_with_factories(&fixture, factories);
    harness.wait_ready();

    harness.handle.request_shutdown();
    assert!(harness.wait_exit(), "the runner must finish first");
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("the factory drain runs after the runner finished");
    assert!(
        retained.upgrade().is_some(),
        "the factory service is still owned while its drain is blocked"
    );

    gate.open();
    let outcome = harness.wait_teardown();
    assert_eq!(outcome.result, TeardownResult::Clean, "{outcome}");
    assert!(event_log(&events).iter().any(|event| event == "finish"));
}

struct SocketHandler {
    events: Arc<Mutex<Vec<String>>>,
    gate: Arc<Gate>,
}

impl IpcHandler for SocketHandler {
    fn handle(&self, _: &str, _: serde_json::Value) -> HandlerResult<serde_json::Value> {
        self.events.lock().expect("events").push("handler-entered".to_owned());
        self.gate.wait();
        self.events.lock().expect("events").push("handler-done".to_owned());
        Ok(serde_json::Value::Null)
    }
}

struct SocketOrderFactory {
    events: Arc<Mutex<Vec<String>>>,
    gate: Arc<Gate>,
}

impl ModuleFactory for SocketOrderFactory {
    fn id(&self) -> ModuleId { TestConfig::ID }

    fn register(
        &self,
        registry: &stache_sdk::Registry,
        store: &stache_sdk::ConfigStore,
    ) -> Result<Arc<dyn stache_app::host::ModuleControl>, stache_app::host::HostError> {
        register_module(registry, store, TestModule {
            counters: Arc::new(Counters::default()),
            permission: false,
            gate: None,
            configure_gate: None,
        })
    }

    fn ipc_handlers(&self, _: &FactoryContext) -> Vec<(&'static str, Arc<dyn IpcHandler>)> {
        vec![(
            TestConfig::ID.as_str(),
            Arc::new(SocketHandler {
                events: Arc::clone(&self.events),
                gate: Arc::clone(&self.gate),
            }),
        )]
    }

    fn begin_shutdown(&self) {
        self.events.lock().expect("events").push("begin".to_owned());
        self.gate.open();
    }

    fn finish_shutdown(&self, _: &dyn stache_sdk::shutdown::ShutdownWait) -> Result<bool, String> {
        self.events.lock().expect("events").push("finish".to_owned());
        Ok(true)
    }
}

#[test]
fn begin_shutdown_wakes_a_running_socket_handler_before_its_join() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = false\n");
    let events = Arc::new(Mutex::new(Vec::new()));
    let gate = Gate::new();
    let factories = ModuleFactories::new().with(SocketOrderFactory {
        events: Arc::clone(&events),
        gate: Arc::clone(&gate),
    });
    let mut harness = plain_harness();
    harness.start_with_factories(&fixture, factories);
    harness.wait_ready();

    let client = IpcClient::new(fixture.app.socket_path().expect("socket"));
    let request = Request {
        module: TestConfig::ID.as_str().to_owned(),
        method: "hold".to_owned(),
        params: serde_json::Value::Null,
    };
    let caller = std::thread::spawn(move || client.request(&request));
    wait_for_event(&events, "handler-entered");

    harness.handle.request_shutdown();
    assert!(harness.wait_exit(), "the runner must finish");
    let outcome = harness.wait_teardown();
    assert_eq!(outcome.result, TeardownResult::Clean, "{outcome}");
    let _ = caller.join().expect("the client thread");

    let events = event_log(&events);
    let entered = events.iter().position(|event| event == "handler-entered").expect("entered");
    let begin = events.iter().position(|event| event == "begin").expect("begin");
    let done = events.iter().position(|event| event == "handler-done").expect("done");
    assert!(entered < begin, "{events:?}");
    assert!(
        begin < done,
        "begin-shutdown must wake the handler before its join: {events:?}"
    );
}

#[test]
fn a_blocked_wallpapers_native_call_is_unacknowledged_until_released() {
    let fixture = fixture();
    fixture.write("wallpapers.toml", "enabled = false\nlist = [\"wall.png\"]\n");
    let image = fixture.paths.directory().join("wall.png");
    write_wallpaper_png(&image);

    let desktop = FakeDesktop::with_snapshot(wallpaper_snapshot());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let backend: Arc<dyn GuardedDesktopBackend> =
        Arc::new(BlockingDesktop::new(desktop, entered_tx, release_rx));
    let service = wallpapers_service(&fixture, backend);
    let factory = WallpapersFactory::with_service(Arc::clone(&service));

    let mut harness = Harness::with_teardown(
        FakePermissions::new(PermissionStatus::Granted),
        Arc::new(InlineMain),
        Arc::new(FakeCommands::default()),
        Arc::new(RecordingWindows::default()),
        real_teardown(),
    );
    harness.start_with_factories(&fixture, ModuleFactories::new().with(factory));
    harness.wait_ready();
    assert!(
        harness
            .handle
            .status()
            .modules
            .iter()
            .any(|module| module.id == "wallpapers" && module.state == ModuleState::Disabled),
        "the disabled module is registered without starting rotation"
    );

    let params = WallpapersSetParams {
        config_directory: fixture.paths.directory().display().to_string(),
        selection: WallpaperSelection::File {
            path: image.display().to_string(),
        },
        screen: WallpaperScreen::All,
    };
    let caller = {
        let service = Arc::clone(&service);
        std::thread::spawn(move || service.set_with_budget(&params, Duration::from_secs(30)))
    };
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the blocked native call was entered");

    harness.handle.request_shutdown();
    assert!(harness.wait_exit(), "the runner must still finish");
    let outcome = harness.wait_teardown();
    assert!(
        outcome.unacknowledged.iter().any(|owner| owner == "factory:wallpapers"),
        "a live native call must be named instead of pretending teardown finished: {outcome}"
    );
    harness.wait_terminal();

    release_tx.send(()).expect("release the native call");
    let _ = caller.join().expect("the wallpaper caller thread");

    // Once the issued call and the coordinator return, the same factory drain
    // acknowledges. The wait advances instantly, so no test sleeps through the
    // real owner budget.
    let wait = AdvancingWait::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if service.finish_shutdown(&wait) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the wallpaper service never acknowledged after release"
        );
        std::thread::yield_now();
    }
}

#[test]
fn an_enabled_wallpapers_module_applies_one_initial_job_without_a_timer() {
    let fixture = fixture();
    fixture.write("wallpapers.toml", "enabled = true\nlist = [\"wall.png\"]\n");
    let image = fixture.paths.directory().join("wall.png");
    write_wallpaper_png(&image);

    let desktop = FakeDesktop::with_snapshot(wallpaper_snapshot());
    let (applied_tx, applied_rx) = mpsc::channel();
    let backend: Arc<dyn GuardedDesktopBackend> =
        Arc::new(SignalingDesktop::new(desktop, applied_tx));
    let factory = WallpapersFactory::with_service(wallpapers_service(&fixture, backend));
    let mut harness = plain_harness();
    harness.start_with_factories(&fixture, ModuleFactories::new().with(factory));
    harness.wait_ready();
    assert!(
        harness
            .handle
            .status()
            .modules
            .iter()
            .any(|module| module.id == "wallpapers" && module.state == ModuleState::Running),
        "an accepted enabled file starts the rotation module"
    );
    applied_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the first rotation job applied the wallpaper");

    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
    harness.wait_teardown();
    assert!(
        applied_rx.try_recv().is_err(),
        "interval zero has one initial job and no timer retry"
    );
}

#[test]
fn the_host_socket_merges_the_wallpapers_route() {
    let fixture = fixture();
    fixture.write("wallpapers.toml", "enabled = false\nlist = [\"wall.png\"]\n");
    let image = fixture.paths.directory().join("wall.png");
    write_wallpaper_png(&image);

    let desktop = FakeDesktop::with_snapshot(wallpaper_snapshot());
    let backend: Arc<dyn GuardedDesktopBackend> = Arc::new(desktop.clone());
    let factory = WallpapersFactory::with_service(wallpapers_service(&fixture, backend));
    let mut harness = plain_harness();
    harness.start_with_factories(&fixture, ModuleFactories::new().with(factory));
    harness.wait_ready();

    let client = IpcClient::with_options(
        fixture.app.socket_path().expect("socket"),
        RetryPolicy::single(),
        Duration::from_secs(30),
    );
    let params = WallpapersSetParams {
        config_directory: fixture.paths.directory().display().to_string(),
        selection: WallpaperSelection::File {
            path: image.display().to_string(),
        },
        screen: WallpaperScreen::All,
    };
    let request = Request::new("wallpapers", "set", &params).expect("request");
    let outcome = match client.request(&request).expect("wallpapers route") {
        Response::Ok { data } => {
            serde_json::from_value::<WallpapersSetOutcome>(data).expect("typed outcome")
        }
        Response::Error { error } => panic!("unexpected wire error: {error:?}"),
    };
    assert_eq!(outcome.status, WallpapersOutcomeStatus::FullSuccess);
    assert_eq!(desktop.calls().len(), 1);

    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
    harness.wait_teardown();
}

// ---------------------------------------------------------------------------
// `wallpapers.set` wire round trips through the real server and real client
// ---------------------------------------------------------------------------

fn two_display_snapshot() -> DisplaySnapshot {
    DisplaySnapshot::new(vec![
        DisplayEntry {
            id: DisplayId::new(1),
            frame: DisplayFrame { width: 160.0, height: 90.0 },
            scale: 1.0,
        },
        DisplayEntry {
            id: DisplayId::new(2),
            frame: DisplayFrame { width: 120.0, height: 80.0 },
            scale: 2.0,
        },
    ])
}

/// A running host with the real wallpapers route over a temporary socket.
struct WallpaperWire {
    fixture: Fixture,
    harness: Harness,
    desktop: FakeDesktop,
    service: Arc<WallpapersService>,
    client: IpcClient,
    image: std::path::PathBuf,
}

impl WallpaperWire {
    fn start(config: &str) -> Self {
        let fixture = fixture();
        fixture.write("wallpapers.toml", config);
        let image = fixture.paths.directory().join("wall.png");
        write_wallpaper_png(&image);
        let desktop = FakeDesktop::with_snapshot(two_display_snapshot());
        let backend: Arc<dyn GuardedDesktopBackend> = Arc::new(desktop.clone());
        let service = wallpapers_service(&fixture, backend);
        let factory = WallpapersFactory::with_service(Arc::clone(&service));
        let mut harness = plain_harness();
        harness.start_with_factories(&fixture, ModuleFactories::new().with(factory));
        harness.wait_ready();
        // The exported budget must be usable through the real client.
        let client = IpcClient::with_options(
            fixture.app.socket_path().expect("socket"),
            RetryPolicy::legacy(),
            WALLPAPERS_CLIENT_BUDGET,
        );
        Self {
            fixture,
            harness,
            desktop,
            service,
            client,
            image,
        }
    }

    fn disabled() -> Self { Self::start("enabled = false\nlist = [\"wall.png\"]\n") }

    fn params(&self, selection: WallpaperSelection) -> WallpapersSetParams {
        WallpapersSetParams {
            config_directory: self.fixture.paths.directory().display().to_string(),
            selection,
            screen: WallpaperScreen::All,
        }
    }

    fn file(&self) -> WallpaperSelection {
        WallpaperSelection::File {
            path: self.image.display().to_string(),
        }
    }

    /// Sends one request through the socket; transport must hand back `Ok`
    /// data, never a wire error.
    fn wire(&self, params: &WallpapersSetParams) -> WallpapersSetOutcome {
        let request = Request::new("wallpapers", "set", params).expect("request");
        match self.client.request(&request).expect("transport") {
            Response::Ok { data } => {
                assert!(
                    data.get("error").is_none(),
                    "an outcome must not smuggle a wire error: {data}"
                );
                serde_json::from_value(data).expect("typed outcome")
            }
            Response::Error { error } => panic!("a handled outcome became a wire error: {error:?}"),
        }
    }

    /// The service-produced outcome, computed directly on the same service
    /// before the wire call, with the same scripted fake and inputs.
    fn expected(&self, params: &WallpapersSetParams) -> WallpapersSetOutcome {
        self.service.set(params)
    }

    fn roundtrip(
        &self,
        params: &WallpapersSetParams,
    ) -> (WallpapersSetOutcome, WallpapersSetOutcome) {
        let expected = self.expected(params);
        let actual = self.wire(params);
        (actual, expected)
    }

    fn finish(self) {
        self.harness.handle.request_shutdown();
        assert!(self.harness.wait_exit());
        self.harness.wait_teardown();
    }
}

const fn applied(display: u32) -> WallpapersTargetOutcome {
    WallpapersTargetOutcome {
        display,
        target: WallpapersTargetResult::Applied,
    }
}

#[test]
fn wire_roundtrip_full_success_over_the_exported_client_budget() {
    let wire = WallpaperWire::disabled();
    let params = wire.params(wire.file());
    let (actual, expected) = wire.roundtrip(&params);
    assert_eq!(actual, expected);
    assert_eq!(actual.status, WallpapersOutcomeStatus::FullSuccess);
    assert_eq!(actual.cause, None);
    assert_eq!(actual.applied, [1, 2]);
    assert!(
        actual.failed.is_empty() && actual.not_attempted.is_empty() && actual.unknown.is_empty()
    );
    assert_eq!(actual.targets, [applied(1), applied(2)]);
    assert_eq!(
        wire.desktop.calls().len(),
        4,
        "two direct and two wire native sets"
    );
    wire.finish();
}

#[test]
fn wire_roundtrip_partial_with_one_known_native_failure() {
    let wire = WallpaperWire::disabled();
    wire.desktop.set_outcome(
        DisplayId::new(2),
        Err(DesktopError::Native {
            display: DisplayId::new(2),
            message: "rejected".to_owned(),
        }),
    );
    let params = wire.params(wire.file());
    let (actual, expected) = wire.roundtrip(&params);
    assert_eq!(actual, expected);
    assert_eq!(actual.status, WallpapersOutcomeStatus::Partial);
    assert!(
        matches!(actual.cause, Some(WallpapersOutcomeCause::Native { .. })),
        "{actual:?}"
    );
    assert_eq!(actual.applied, [1]);
    assert_eq!(actual.failed, [2]);
    assert!(actual.not_attempted.is_empty() && actual.unknown.is_empty());
    assert_eq!(actual.targets.len(), 2);
    assert_eq!(actual.targets[0], applied(1));
    assert_eq!(actual.targets[1].display, 2);
    assert!(
        matches!(
            &actual.targets[1].target,
            WallpapersTargetResult::Failed { reason: WallpapersTargetFailure::Native, message }
                if message.contains("rejected")
        ),
        "{:?}",
        actual.targets[1]
    );
    wire.finish();
}

#[test]
fn wire_roundtrip_unknown_target_after_an_issued_set() {
    let wire = WallpaperWire::disabled();
    wire.desktop.set_outcome(
        DisplayId::new(2),
        Err(DesktopError::Unknown { operation: "desktop image set" }),
    );
    let params = wire.params(wire.file());
    let (actual, expected) = wire.roundtrip(&params);
    assert_eq!(actual, expected);
    assert_eq!(actual.status, WallpapersOutcomeStatus::Uncertain);
    assert_eq!(actual.applied, [1]);
    assert_eq!(actual.unknown, [2]);
    assert!(actual.failed.is_empty() && actual.not_attempted.is_empty());
    assert_eq!(actual.targets[0], applied(1));
    assert_eq!(actual.targets[1].display, 2);
    assert!(
        matches!(actual.targets[1].target, WallpapersTargetResult::Unknown {
            reason: WallpapersTargetFailure::Unknown,
            ..
        }),
        "{:?}",
        actual.targets[1]
    );
    wire.finish();
}

#[test]
fn wire_roundtrip_cancelled_before_any_native_call() {
    let wire = WallpaperWire::disabled();
    // Host cancellation closes admission while the socket keeps serving.
    wire.service.begin_shutdown();
    let params = wire.params(wire.file());
    let (actual, expected) = wire.roundtrip(&params);
    assert_eq!(actual, expected);
    assert_eq!(actual.status, WallpapersOutcomeStatus::Cancelled);
    assert_eq!(actual.cause, Some(WallpapersOutcomeCause::Cancelled));
    assert!(actual.targets.is_empty() && actual.applied.is_empty());
    assert!(wire.desktop.calls().is_empty(), "no native call was issued");
    wire.finish();
}

#[test]
fn wire_roundtrip_different_config_directory_is_refused() {
    let wire = WallpaperWire::disabled();
    let other = tempfile::tempdir().expect("other config directory");
    let mut params = wire.params(wire.file());
    params.config_directory = other.path().display().to_string();
    let (actual, expected) = wire.roundtrip(&params);
    assert_eq!(actual, expected);
    assert_eq!(actual.status, WallpapersOutcomeStatus::Failure);
    match &actual.cause {
        Some(WallpapersOutcomeCause::DifferentConfig { host, requested }) => {
            assert_ne!(host, requested);
        }
        other => panic!("expected DifferentConfig, got {other:?}"),
    }
    assert!(actual.targets.is_empty());
    assert!(wire.desktop.calls().is_empty());
    wire.finish();
}

#[test]
fn wire_roundtrip_no_accepted_configuration() {
    let wire = WallpaperWire::start("enabled = ");
    let params = wire.params(WallpaperSelection::Random);
    let (actual, expected) = wire.roundtrip(&params);
    assert_eq!(actual, expected);
    assert_eq!(actual.status, WallpapersOutcomeStatus::Failure);
    assert_eq!(actual.cause, Some(WallpapersOutcomeCause::NoAcceptedConfig));
    assert!(actual.targets.is_empty());
    assert!(wire.desktop.calls().is_empty());
    wire.finish();
}

#[test]
fn wire_roundtrip_invalid_explicit_selection_and_source() {
    let wire = WallpaperWire::disabled();
    let out_of_range = wire.params(WallpaperSelection::Collection { index: 9 });
    let (actual, expected) = wire.roundtrip(&out_of_range);
    assert_eq!(actual, expected);
    assert_eq!(actual.status, WallpapersOutcomeStatus::Failure);
    assert!(
        matches!(
            actual.cause,
            Some(WallpapersOutcomeCause::InvalidSelection { .. })
        ),
        "{actual:?}"
    );

    let relative = wire.params(WallpaperSelection::File {
        path: "relative.png".to_owned(),
    });
    let (actual, expected) = wire.roundtrip(&relative);
    assert_eq!(actual, expected);
    assert_eq!(actual.status, WallpapersOutcomeStatus::Failure);
    assert!(actual.cause.is_some() && actual.targets.is_empty());

    let missing = wire.params(WallpaperSelection::File {
        path: wire.fixture.root().join("absent.png").display().to_string(),
    });
    let (actual, expected) = wire.roundtrip(&missing);
    assert_eq!(actual, expected);
    assert_eq!(actual.status, WallpapersOutcomeStatus::Failure);
    assert!(actual.cause.is_some() && actual.targets.is_empty());
    assert!(wire.desktop.calls().is_empty(), "no refusal reached the desktop");
    wire.finish();
}

#[test]
fn wallpaper_outcomes_leave_the_sdk_wire_error_shape_unchanged() {
    use stache_sdk::ipc::WireErrorKind;

    let wire = WallpaperWire::disabled();
    // A genuine wire error from the same route stays `{kind, message}` only.
    let bad =
        Request::new("wallpapers", "set", serde_json::json!({ "bogus": 1 })).expect("request");
    match wire.client.request(&bad).expect("transport") {
        Response::Error { error } => {
            assert_eq!(error.kind, WireErrorKind::InvalidParams);
            let value = serde_json::to_value(&error).expect("wire error");
            let mut keys: Vec<&str> =
                value.as_object().expect("object").keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(keys, ["kind", "message"]);
        }
        Response::Ok { data } => panic!("invalid params must be a wire error, got {data}"),
    }
    // A handled refusal is Ok data whose top-level keys are the outcome fields.
    let refused = wire.params(WallpaperSelection::Collection { index: 9 });
    let request = Request::new("wallpapers", "set", &refused).expect("request");
    match wire.client.request(&request).expect("transport") {
        Response::Ok { data } => {
            let mut keys: Vec<&str> =
                data.as_object().expect("object").keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(keys, [
                "applied",
                "cause",
                "failed",
                "notAttempted",
                "status",
                "targets",
                "unknown"
            ]);
        }
        Response::Error { error } => panic!("a handled refusal became a wire error: {error:?}"),
    }
    wire.finish();
}

#[test]
fn the_builtin_five_second_client_default_is_unchanged_and_below_the_wallpaper_budget() {
    assert_eq!(stache_sdk::ipc::IO_TIMEOUT, Duration::from_secs(5));
    assert!(WALLPAPERS_CLIENT_BUDGET > stache_sdk::ipc::IO_TIMEOUT);
}

// ---------------------------------------------------------------------------
// CLI executor against the real socket, real server and real service
// ---------------------------------------------------------------------------

fn socket_transport() -> Arc<dyn SetTransport> {
    // One attempt, no retry sleep; the response wait is generous but never
    // reached because every scenario answers or closes promptly.
    Arc::new(IpcTransport::with_options(
        RetryPolicy::single(),
        Duration::from_secs(30),
    ))
}

/// Runs the composed `stache` CLI with the wallpaper contribution.
///
/// `local` is the fake desktop a standalone fallback would use; the real
/// native adapter is never constructed.
fn run_wallpaper_cli(
    root: &std::path::Path,
    catalog: &ConfigCatalog,
    config_dir: &std::path::Path,
    local: &FakeDesktop,
    transport: Arc<dyn SetTransport>,
    cwd: &std::path::Path,
    args: &[&str],
) -> (ExitCode, String, String) {
    let config_env = ConfigEnvironment {
        xdg_config_home: None,
        home: Some(root.to_path_buf()),
    };
    let app_env = AppEnvironment {
        home: Some(root.to_path_buf()),
        temporary: Some(root.to_path_buf()),
    };
    let desktop: Arc<dyn GuardedDesktopBackend> = Arc::new(local.clone());
    let mut env = WallpapersCliEnv::new(
        config_env.clone(),
        app_env.clone(),
        desktop,
        Arc::new(|json: &str, _ansi: bool| json.to_owned()),
        never_cancelled(),
    );
    env.transport = transport;
    let cwd = cwd.to_path_buf();
    env.current_dir = Arc::new(move || Ok(cwd.clone()));
    let mut commands = CommandCatalog::new();
    commands.add(wallpaper_contribution(env)).expect("wallpaper command");
    let mut command_line: Vec<OsString> = vec!["stache".into(), "-c".into(), config_dir.into()];
    command_line.push("wallpaper".into());
    command_line.extend(args.iter().map(OsString::from));
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = stache_cli::run_with_io(
        command_line,
        catalog,
        &commands,
        &CliEnvironment::new(config_env, app_env),
        false,
        &mut out,
        &mut err,
    );
    (
        code,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

impl WallpaperWire {
    /// Runs the CLI against this host with a fresh local fake desktop.
    fn cli(&self, args: &[&str]) -> (ExitCode, String, String, FakeDesktop) {
        let local = FakeDesktop::with_snapshot(two_display_snapshot());
        let (code, out, err) = run_wallpaper_cli(
            self.fixture.root(),
            &self.fixture.catalog,
            self.fixture.paths.directory(),
            &local,
            socket_transport(),
            self.fixture.paths.directory(),
            args,
        );
        (code, out, err, local)
    }

    /// Stops this host and hands back its fixture (and socket path).
    fn stop(self) -> Fixture {
        self.harness.handle.request_shutdown();
        assert!(self.harness.wait_exit());
        self.harness.wait_teardown();
        self.fixture
    }
}

const SUCCESS_LINE: &str = "Wallpaper set successfully.\n";

#[test]
fn cli_set_succeeds_over_the_real_socket_while_the_module_is_disabled() {
    let wire = WallpaperWire::disabled();
    let image = wire.image.display().to_string();
    let (code, out, err, local) = wire.cli(&["set", &image]);
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (ExitCode::SUCCESS, SUCCESS_LINE, "")
    );
    assert_eq!(wire.desktop.calls().len(), 2, "the host applied both displays");
    assert!(local.calls().is_empty(), "no local application");
    // A bare existing filename in the invocation cwd (the config directory
    // here) is the actual file and is sent absolute.
    let (code, out, _, local) = wire.cli(&["set", "wall.png", "--screen", "main"]);
    assert_eq!((code, out.as_str()), (ExitCode::SUCCESS, SUCCESS_LINE));
    assert_eq!(wire.desktop.calls().len(), 3);
    assert!(local.calls().is_empty());
    let status = wire.harness.handle.status();
    assert!(
        status
            .modules
            .iter()
            .any(|module| module.id == "wallpapers" && module.state == ModuleState::Disabled),
        "set never enables the module"
    );
    let service = wire.service.status();
    assert!(
        service.session.is_none(),
        "no rotation session, timer or deadline"
    );
    wire.finish();
}

#[test]
fn cli_set_works_for_a_paused_module_without_lifecycle_changes() {
    let wire = WallpaperWire::start("enabled = true\nlist = [\"wall.png\"]\n");
    // The enabled interval-zero module applies once on start.
    wait_for(&wire.harness.handle, "wallpapers running", |status| {
        status
            .modules
            .iter()
            .any(|module| module.id == "wallpapers" && module.state == ModuleState::Running)
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while wire.desktop.calls().len() < 2 {
        assert!(Instant::now() < deadline, "the initial job never applied");
        std::thread::sleep(Duration::from_millis(5));
    }
    wire.harness.handle.pause("wallpapers");
    wait_for(&wire.harness.handle, "wallpapers paused", |status| {
        status
            .modules
            .iter()
            .any(|module| module.id == "wallpapers" && module.state == ModuleState::Paused)
    });
    let before = wire.desktop.calls().len();
    let (code, out, err, local) = wire.cli(&["set", "--random", "--screen", "main"]);
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (ExitCode::SUCCESS, SUCCESS_LINE, "")
    );
    assert_eq!(wire.desktop.calls().len(), before + 1);
    assert!(local.calls().is_empty());
    let status = wire.harness.handle.status();
    assert!(
        status
            .modules
            .iter()
            .any(|module| module.id == "wallpapers" && module.state == ModuleState::Paused),
        "set neither resumes nor restarts the module"
    );
    assert!(
        wire.service.status().session.is_none(),
        "no session, timer or deadline"
    );
    wire.finish();
}

#[test]
fn cli_refuses_a_different_config_directory_and_accepts_an_alias() {
    let wire = WallpaperWire::disabled();
    let image = wire.image.display().to_string();
    let other = wire.fixture.root().join("other-config");
    fs::create_dir_all(&other).expect("other config");
    for directory in [other.clone(), other.join("not-created-yet")] {
        let local = FakeDesktop::with_snapshot(two_display_snapshot());
        let (code, out, err) = run_wallpaper_cli(
            wire.fixture.root(),
            &wire.fixture.catalog,
            &directory,
            &local,
            socket_transport(),
            wire.fixture.root(),
            &["set", &image],
        );
        assert_eq!(code, ExitCode::FAILURE);
        assert_eq!(out, "");
        assert!(
            err.starts_with("stache: Wallpaper error: Running host uses config directory \""),
            "{err}"
        );
        assert!(
            err.ends_with(". Use the same config directory or stop that host.\n"),
            "{err}"
        );
        assert!(
            local.calls().is_empty(),
            "a live different-config host never falls back"
        );
    }
    assert!(wire.desktop.calls().is_empty(), "the refusal changed nothing");

    // A symlink alias of the same directory is the same host.
    let alias = wire.fixture.root().join("alias");
    std::os::unix::fs::symlink(wire.fixture.paths.directory(), &alias).expect("alias");
    let local = FakeDesktop::with_snapshot(two_display_snapshot());
    let (code, out, err) = run_wallpaper_cli(
        wire.fixture.root(),
        &wire.fixture.catalog,
        &alias,
        &local,
        socket_transport(),
        wire.fixture.root(),
        &["set", &image],
    );
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (ExitCode::SUCCESS, SUCCESS_LINE, "")
    );
    assert_eq!(wire.desktop.calls().len(), 2);
    assert!(local.calls().is_empty());
    wire.finish();
}

#[test]
fn cli_reports_no_accepted_config_then_works_after_a_fix_without_restart() {
    let wire = WallpaperWire::start("enabled = ");
    let (code, out, err, local) = wire.cli(&["set", "--random"]);
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(out, "");
    assert_eq!(
        err,
        "stache: Wallpaper error: No accepted Wallpapers configuration is available. Fix wallpapers.toml and reload, then retry.\n"
    );
    assert!(local.calls().is_empty() && wire.desktop.calls().is_empty());

    wire.fixture
        .write("wallpapers.toml", "enabled = false\nlist = [\"wall.png\"]\n");
    let report = wire.client.reload().expect("reload");
    assert!(!report.has_failures(), "{report:?}");
    let (code, out, err, local) = wire.cli(&["set", "WALL"]);
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (ExitCode::SUCCESS, SUCCESS_LINE, "")
    );
    assert_eq!(wire.desktop.calls().len(), 2);
    assert!(local.calls().is_empty());
    wire.finish();
}

#[test]
fn cli_name_resolves_against_the_last_good_accepted_pool_after_a_rejected_reload() {
    let wire = WallpaperWire::start("enabled = false\nlist = [\"wall.png\", \"second.png\"]\n");
    write_wallpaper_png(&wire.fixture.paths.directory().join("second.png"));
    // Disk is now malformed; the host keeps its last accepted pool.
    wire.fixture.write("wallpapers.toml", "list = [");
    let report = wire.client.reload().expect("reload");
    assert!(report.has_failures(), "the malformed file is rejected");
    // Nothing is a bare file in the cwd called `second`, so it is a name.
    let (code, out, err, local) = wire.cli(&["set", "Second"]);
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (ExitCode::SUCCESS, SUCCESS_LINE, "")
    );
    assert_eq!(wire.desktop.calls().len(), 2);
    assert!(
        local.calls().is_empty(),
        "the CLI never reread the malformed disk file"
    );

    let (code, _, err, _) = wire.cli(&["set", "absent-name"]);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("(invalid-selection)"), "{err}");
    wire.finish();
}

#[test]
fn cli_renders_a_partial_and_an_uncertain_host_outcome() {
    let wire = WallpaperWire::disabled();
    let image = wire.image.display().to_string();
    wire.desktop.set_outcome(
        DisplayId::new(2),
        Err(DesktopError::Native {
            display: DisplayId::new(2),
            message: "rejected".to_owned(),
        }),
    );
    let (code, out, err, local) = wire.cli(&["set", &image]);
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(out, "", "never the success line");
    assert_eq!(
        err,
        "stache: Wallpaper error: Wallpaper application partial: applied=[1]; failed=[2]; not-attempted=[]; unknown=[]. Check the desktop before retrying; no automatic retry was attempted.\n"
    );
    assert!(local.calls().is_empty());

    wire.desktop.set_outcome(
        DisplayId::new(2),
        Err(DesktopError::Unknown { operation: "desktop image set" }),
    );
    let (code, out, err, _) = wire.cli(&["set", &image]);
    assert_eq!((code, out.as_str()), (ExitCode::FAILURE, ""));
    assert_eq!(
        err,
        "stache: Wallpaper error: Wallpaper application uncertain: applied=[1]; failed=[]; not-attempted=[]; unknown=[2]. Check the desktop before retrying; no automatic retry was attempted.\n"
    );
    wire.finish();
}

#[test]
fn cli_never_falls_back_after_a_host_restart_with_a_different_config() {
    let wire = WallpaperWire::disabled();
    let image = wire.image.display().to_string();
    let first_config = wire.fixture.paths.directory().to_path_buf();
    let (code, out, _, _) = wire.cli(&["set", &image]);
    assert_eq!((code, out.as_str()), (ExitCode::SUCCESS, SUCCESS_LINE));

    // Host A stops and host B starts on the same socket with another config
    // directory, between the first request and the second.
    let mut fixture = wire.stop();
    let second_config = fixture.root().join("config-b");
    fixture.paths = ConfigPaths::new(&second_config);
    fixture.write("wallpapers.toml", "enabled = false\n");
    let desktop = FakeDesktop::with_snapshot(two_display_snapshot());
    let backend: Arc<dyn GuardedDesktopBackend> = Arc::new(desktop.clone());
    let factory = WallpapersFactory::with_service(wallpapers_service(&fixture, backend));
    let mut harness = plain_harness();
    harness.start_with_factories(&fixture, ModuleFactories::new().with(factory));
    harness.wait_ready();

    let local = FakeDesktop::with_snapshot(two_display_snapshot());
    let (code, out, err) = run_wallpaper_cli(
        fixture.root(),
        &fixture.catalog,
        &first_config,
        &local,
        socket_transport(),
        fixture.root(),
        &["set", &image],
    );
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(out, "");
    assert!(err.contains("Running host uses config directory"), "{err}");
    assert!(
        desktop.calls().is_empty() && local.calls().is_empty(),
        "zero application anywhere"
    );
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
    harness.wait_teardown();
}

/// A fake peer that accepts exactly one connection and answers with `reply`.
fn raw_peer(
    fixture: &Fixture,
    reply: Option<&'static [u8]>,
) -> (std::thread::JoinHandle<()>, std::os::unix::net::UnixListener) {
    use std::io::{BufRead, BufReader, Write};

    let socket = fixture.app.socket_path().expect("socket");
    fs::create_dir_all(socket.parent().expect("runtime directory")).expect("runtime directory");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind fake peer");
    let observer = listener.try_clone().expect("observer");
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("one connection");
        let mut line = String::new();
        BufReader::new(&mut stream).read_line(&mut line).expect("request line");
        assert!(line.ends_with('\n'), "a complete request was sent");
        if let Some(reply) = reply {
            stream.write_all(reply).expect("reply");
        }
        // Dropping the stream closes it: a dropped or truncated response.
    });
    (handle, observer)
}

#[test]
fn cli_treats_a_dropped_partial_or_malformed_response_as_uncertain_with_no_fallback() {
    const UNCERTAIN: &str = "stache: Wallpaper error: Wallpaper outcome is unknown after a send or response failure. Check the desktop before retrying; no automatic retry was attempted.\n";
    let replies: [Option<&'static [u8]>; 4] = [
        None,
        Some(br#"{"status":"ok","da"#),
        Some(b"{\"status\":\"ok\",\"data\":{\"x\":1}}\n"),
        Some(b"not json at all\n"),
    ];
    for reply in replies {
        let fixture = fixture();
        write_wallpaper_png(&fixture.root().join("w.png"));
        let (peer, observer) = raw_peer(&fixture, reply);
        let local = FakeDesktop::with_snapshot(two_display_snapshot());
        let (code, out, err) = run_wallpaper_cli(
            fixture.root(),
            &fixture.catalog,
            fixture.paths.directory(),
            &local,
            socket_transport(),
            fixture.root(),
            &["set", "w.png"],
        );
        peer.join().expect("fake peer");
        assert_eq!(code, ExitCode::FAILURE, "{reply:?}");
        assert_eq!(out, "");
        assert_eq!(err, UNCERTAIN, "{reply:?}");
        assert!(local.calls().is_empty(), "zero local apply after {reply:?}");
        assert_eq!(local.guarded_snapshot_reads(), 0);
        // Exactly one connection: a second attempt would be accepted here.
        observer.set_nonblocking(true).expect("nonblocking");
        assert!(
            matches!(observer.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "no retry connection"
        );
    }
}

#[test]
fn cli_absent_socket_applies_once_locally_and_a_later_host_initial_job_may_override_it() {
    // The documented race: no socket exists while a desktop instance is still
    // starting, so the CLI applies once locally. The host then runs its normal
    // initial job and its image is the last one set. No coordination exists.
    let fixture = fixture();
    fixture.write("wallpapers.toml", "enabled = true\nlist = [\"pool.png\"]\n");
    let pool = fixture.paths.directory().join("pool.png");
    write_wallpaper_png(&pool);
    // A distinct source image so the two cache outputs differ.
    let cli_image = fixture.root().join("cli.png");
    let pixels = image::RgbImage::from_fn(8, 8, |_, _| image::Rgb([200, 10, 10]));
    pixels
        .write_with_encoder(image::codecs::png::PngEncoder::new(
            fs::File::create(&cli_image).expect("cli image"),
        ))
        .expect("encode");

    let shared = FakeDesktop::with_snapshot(wallpaper_snapshot());
    let (code, out, err) = run_wallpaper_cli(
        fixture.root(),
        &fixture.catalog,
        fixture.paths.directory(),
        &shared,
        socket_transport(),
        fixture.root(),
        &["set", "cli.png"],
    );
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (ExitCode::SUCCESS, SUCCESS_LINE, "")
    );
    let local_calls = shared.calls();
    assert_eq!(local_calls.len(), 1, "exactly one local mutation");

    let backend: Arc<dyn GuardedDesktopBackend> = Arc::new(shared.clone());
    let factory = WallpapersFactory::with_service(wallpapers_service(&fixture, backend));
    let mut harness = plain_harness();
    harness.start_with_factories(&fixture, ModuleFactories::new().with(factory));
    harness.wait_ready();
    let deadline = Instant::now() + Duration::from_secs(10);
    while shared.calls().len() < 2 {
        assert!(Instant::now() < deadline, "the host's initial job never ran");
        std::thread::sleep(Duration::from_millis(5));
    }
    let calls = shared.calls();
    assert_eq!(calls.len(), 2);
    assert_ne!(
        calls[0].image, calls[1].image,
        "the host initial image overrode the CLI image"
    );
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
    harness.wait_teardown();
}

struct ManualRuntimeClock(Mutex<Instant>);

impl ManualRuntimeClock {
    fn advance(&self, by: Duration) { *self.0.lock().expect("clock") += by; }
}

impl RuntimeClock for ManualRuntimeClock {
    fn now(&self) -> Instant { *self.0.lock().expect("clock") }
}

/// Blocks its first `prepare`, then behaves like the real processor.
struct FirstCallBlocks {
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Processor for FirstCallBlocks {
    fn prepare(
        &self,
        plan: &[stache_wallpapers::PlannedTarget],
        paths: &AppPaths,
        cancel: &stache_sdk::CancellationToken,
    ) -> Result<stache_wallpapers::Prepared, stache_wallpapers::PrepareError> {
        let entered = self.entered.lock().expect("entered").take();
        if let Some(entered) = entered {
            let _ = entered.send(());
            let release = self.release.lock().expect("release").take();
            if let Some(release) = release {
                let _ = release.recv();
            }
        }
        RealProcessor.prepare(plan, paths, cancel)
    }
}

#[test]
fn cli_deadline_expiry_while_queued_round_trips_over_the_real_socket_with_fake_time() {
    let fixture = fixture();
    fixture.write("wallpapers.toml", "enabled = false\nlist = [\"wall.png\"]\n");
    let image = fixture.paths.directory().join("wall.png");
    write_wallpaper_png(&image);
    let desktop = FakeDesktop::with_snapshot(two_display_snapshot());
    let backend: Arc<dyn GuardedDesktopBackend> = Arc::new(desktop.clone());
    let clock = Arc::new(ManualRuntimeClock(Mutex::new(Instant::now())));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let service = WallpapersService::new(ServiceConfig {
        paths: fixture.app.clone(),
        source: SourceContext::new(fixture.paths.directory(), Some(fixture.root().to_path_buf())),
        backend,
        clock: Arc::<ManualRuntimeClock>::clone(&clock),
        random: Arc::new(SystemRandom),
        processor: Arc::new(FirstCallBlocks {
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(Some(release_rx)),
        }),
    })
    .expect("service");
    let factory = WallpapersFactory::with_service(Arc::clone(&service));
    let mut harness = plain_harness();
    harness.start_with_factories(&fixture, ModuleFactories::new().with(factory));
    harness.wait_ready();

    // Job one occupies the coordinator inside the processor.
    let params = WallpapersSetParams {
        config_directory: fixture.paths.directory().display().to_string(),
        selection: WallpaperSelection::File {
            path: image.display().to_string(),
        },
        screen: WallpaperScreen::All,
    };
    let first = {
        let service = Arc::clone(&service);
        std::thread::spawn(move || service.set(&params))
    };
    entered_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("job one entered the processor");

    // The CLI request queues behind it.
    let cli = {
        let local = FakeDesktop::with_snapshot(two_display_snapshot());
        let local_check = local.clone();
        let config = fixture.paths.directory().to_path_buf();
        let root = fixture.root().to_path_buf();
        let catalog = fixture.catalog.clone();
        let handle = std::thread::spawn(move || {
            run_wallpaper_cli(&root, &catalog, &config, &local, socket_transport(), &root, &[
                "set", "--random",
            ])
        });
        (handle, local_check)
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !service.status().queued {
        assert!(Instant::now() < deadline, "the CLI request never queued");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Jump past the server budget on the fake clock, then let job one finish.
    clock.advance(stache_wallpapers::WALLPAPERS_SERVER_BUDGET + Duration::from_secs(1));
    release_tx.send(()).expect("release job one");
    let (code, out, err) = cli.0.join().expect("cli thread");
    let _ = first.join().expect("job one");
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(out, "");
    assert_eq!(
        err,
        "stache: Wallpaper error: Wallpaper deadline expired before native application; no native change was issued. Retry when ready.\n"
    );
    assert!(cli.1.calls().is_empty(), "no fallback");
    assert!(desktop.calls().is_empty(), "no native issuance after expiry");
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
    harness.wait_teardown();
}

/// Advances the fake clock inside the processor, as a slow cold job would.
struct SlowProcessor {
    clock: Arc<ManualRuntimeClock>,
    by: Duration,
}

impl Processor for SlowProcessor {
    fn prepare(
        &self,
        plan: &[stache_wallpapers::PlannedTarget],
        paths: &AppPaths,
        cancel: &stache_sdk::CancellationToken,
    ) -> Result<stache_wallpapers::Prepared, stache_wallpapers::PrepareError> {
        self.clock.advance(self.by);
        RealProcessor.prepare(plan, paths, cancel)
    }
}

/// Advances the fake clock during each native set, then defers to the fake.
struct ClockAdvancingDesktop {
    inner: FakeDesktop,
    clock: Arc<ManualRuntimeClock>,
    by: Duration,
}

impl DesktopBackend for ClockAdvancingDesktop {
    fn snapshot(&self) -> Result<DisplaySnapshot, DesktopError> { self.inner.snapshot() }

    fn set_image(
        &self,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &std::path::Path,
    ) -> Result<(), DesktopError> {
        self.inner.set_image(display, expected, image)
    }
}

impl GuardedDesktopBackend for ClockAdvancingDesktop {
    fn snapshot_guarded(
        &self,
        operation: &DesktopOperation,
    ) -> Result<DisplaySnapshot, DesktopError> {
        self.inner.snapshot_guarded(operation)
    }

    fn set_image_guarded(
        &self,
        operation: &DesktopOperation,
        display: DisplayId,
        expected: &DisplayEntry,
        image: &std::path::Path,
    ) -> Result<(), DesktopError> {
        // The call is issued first; the deadline passes while it completes,
        // so the next display's guard observes the expiry.
        let result = self.inner.set_image_guarded(operation, display, expected, image);
        self.clock.advance(self.by);
        result
    }
}

/// Runs `set <image>` over the real socket against a host whose service uses
/// a fake clock, a custom processor and a custom desktop.
fn cli_set_with_fake_time(
    make: impl FnOnce(
        &Arc<ManualRuntimeClock>,
        &FakeDesktop,
    ) -> (Arc<dyn Processor>, Arc<dyn GuardedDesktopBackend>),
) -> (ExitCode, String, String, FakeDesktop, FakeDesktop) {
    let fixture = fixture();
    fixture.write("wallpapers.toml", "enabled = false\nlist = [\"wall.png\"]\n");
    let image = fixture.paths.directory().join("wall.png");
    write_wallpaper_png(&image);
    let desktop = FakeDesktop::with_snapshot(two_display_snapshot());
    let clock = Arc::new(ManualRuntimeClock(Mutex::new(Instant::now())));
    let (processor, backend) = make(&clock, &desktop);
    let service = WallpapersService::new(ServiceConfig {
        paths: fixture.app.clone(),
        source: SourceContext::new(fixture.paths.directory(), Some(fixture.root().to_path_buf())),
        backend,
        clock: Arc::<ManualRuntimeClock>::clone(&clock),
        random: Arc::new(SystemRandom),
        processor,
    })
    .expect("service");
    let factory = WallpapersFactory::with_service(Arc::clone(&service));
    let mut harness = plain_harness();
    harness.start_with_factories(&fixture, ModuleFactories::new().with(factory));
    harness.wait_ready();
    let local = FakeDesktop::with_snapshot(two_display_snapshot());
    let image = image.display().to_string();
    let (code, out, err) = run_wallpaper_cli(
        fixture.root(),
        &fixture.catalog,
        fixture.paths.directory(),
        &local,
        socket_transport(),
        fixture.paths.directory(),
        &["set", &image],
    );
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
    harness.wait_teardown();
    (code, out, err, local, desktop)
}

#[test]
fn cli_cold_job_beyond_five_seconds_succeeds_over_the_real_socket() {
    let (code, out, err, local, desktop) = cli_set_with_fake_time(|clock, desktop| {
        let processor: Arc<dyn Processor> = Arc::new(SlowProcessor {
            clock: Arc::clone(clock),
            by: Duration::from_secs(30),
        });
        let backend: Arc<dyn GuardedDesktopBackend> = Arc::new(desktop.clone());
        (processor, backend)
    });
    assert_eq!(
        (code, out.as_str(), err.as_str()),
        (ExitCode::SUCCESS, SUCCESS_LINE, "")
    );
    assert_eq!(desktop.calls().len(), 2, "the host applied both displays");
    assert!(local.calls().is_empty(), "no local application");
}

#[test]
fn cli_deadline_passing_during_native_issuance_is_not_reported_as_a_clean_success() {
    let (code, out, err, local, desktop) = cli_set_with_fake_time(|clock, desktop| {
        let processor: Arc<dyn Processor> = Arc::new(RealProcessor);
        let backend: Arc<dyn GuardedDesktopBackend> = Arc::new(ClockAdvancingDesktop {
            inner: desktop.clone(),
            clock: Arc::clone(clock),
            by: stache_wallpapers::WALLPAPERS_SERVER_BUDGET + Duration::from_secs(1),
        });
        (processor, backend)
    });
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(out, "");
    // Display 1 was issued and applied; the deadline passed during that call,
    // so display 2 was refused before issuance.
    assert_eq!(
        err,
        "stache: Wallpaper error: Wallpaper application partial: applied=[1]; failed=[]; \
         not-attempted=[2]; unknown=[]. Check the desktop before retrying; no automatic retry \
         was attempted.\n"
    );
    assert_eq!(
        desktop.calls().len(),
        1,
        "exactly one native call was issued on the host"
    );
    assert!(local.calls().is_empty(), "never a local fallback");
}

// ---- KB-04-07: host startup recovery of a stale Caps remap ----

mod caps_recovery {
    use stache_keybindings::caps::CapsRecoveryStep;
    use stache_macos::caps_remap::testing::{SetFault, World};
    use stache_macos::caps_remap::{CAPS_LOCK_USAGE, Mapping, OWNED};

    use super::*;

    const KEY_A: Mapping = Mapping { src: 100, dst: 200 };

    /// Counts runs so a retry loop would be visible.
    struct Counting {
        inner: KeybindingsCapsRecovery,
        runs: Arc<AtomicUsize>,
    }

    impl StartupRecovery for Counting {
        fn name(&self) -> &'static str { self.inner.name() }

        fn run(&self, cancel: &stache_sdk::CancellationToken) -> Option<String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            self.inner.run(cancel)
        }
    }

    fn stale(initial: Vec<Mapping>) -> World {
        let world = World::new(initial);
        world.remap().apply(&stache_sdk::CancellationToken::new()).expect("apply");
        world.restart();
        world
    }

    fn harness_with(world: &World) -> (Harness, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut harness = plain_harness();
        harness.recovery = Some(Arc::new(Counting {
            inner: KeybindingsCapsRecovery::new(CapsRecoveryStep::new(Arc::new(world.remap()))),
            runs: Arc::clone(&runs),
        }));
        (harness, runs)
    }

    fn wait_restored(world: &World) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while world.journal().is_some() {
            assert!(Instant::now() < deadline, "recovery never retired the journal");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_stale_journal_is_restored_once_while_the_keybindings_module_is_disabled() {
        let fixture = fixture();
        fixture.write("keybindings.toml", "enabled = false\nbindings = {}\n");
        let world = stale(vec![KEY_A]);
        let (mut harness, runs) = harness_with(&world);
        harness.start(&fixture, false, None);
        harness.wait_ready();
        wait_restored(&world);
        assert_eq!(world.mappings(), vec![KEY_A]);
        assert_eq!(world.sets(), 2, "the stale apply plus exactly one restore");
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(
            harness.handle.status().error,
            None,
            "a successful recovery is quiet"
        );
        harness.handle.request_shutdown();
        assert!(harness.wait_exit());
    }

    #[test]
    fn an_invalid_keybindings_config_does_not_block_recovery_or_startup() {
        let fixture = fixture();
        fixture.write("keybindings.toml", "enabled = tru");
        let world = stale(vec![KEY_A]);
        let (mut harness, _runs) = harness_with(&world);
        harness.start(&fixture, false, None);
        harness.wait_ready();
        wait_restored(&world);
        assert_eq!(world.mappings(), vec![KEY_A]);
        harness.handle.request_shutdown();
        assert!(harness.wait_exit());
    }

    #[test]
    fn without_a_journal_recovery_is_a_no_op_that_touches_nothing() {
        for existing in [vec![KEY_A], vec![OWNED], vec![Mapping {
            src: CAPS_LOCK_USAGE,
            dst: 0x7_0000_00E0,
        }]] {
            // A fresh identity per host: one launch per fixture, as in production.
            let fixture = fixture();
            let world = World::new(existing.clone());
            let (mut harness, runs) = harness_with(&world);
            harness.start(&fixture, false, None);
            harness.wait_ready();
            let deadline = Instant::now() + Duration::from_secs(10);
            while runs.load(Ordering::SeqCst) == 0 {
                assert!(Instant::now() < deadline, "the step never ran");
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!((world.acquisitions(), world.gets(), world.sets()), (0, 0, 0));
            assert_eq!(world.mappings(), existing);
            assert_eq!(harness.handle.status().error, None);
            harness.handle.request_shutdown();
            assert!(harness.wait_exit());
        }
    }

    #[test]
    fn a_foreign_mapping_under_our_journal_is_refused_with_a_diagnostic() {
        let fixture = fixture();
        let world = stale(vec![KEY_A]);
        let foreign = Mapping {
            src: CAPS_LOCK_USAGE,
            dst: 0x7_0000_00E0,
        };
        world.external_mappings(vec![KEY_A, foreign]);
        let (mut harness, _runs) = harness_with(&world);
        harness.start(&fixture, false, None);
        harness.wait_ready();
        wait_for(&harness.handle, "refusal diagnostic", |status| {
            status.error.as_deref().is_some_and(|error| error.contains("left untouched"))
        });
        assert_eq!(world.mappings(), vec![KEY_A, foreign]);
        harness.handle.request_shutdown();
        assert!(harness.wait_exit());
    }

    #[test]
    fn a_failed_recovery_is_reported_once_without_blocking_startup_or_retrying() {
        let fixture = fixture();
        let world = stale(vec![KEY_A]);
        world.script_set(SetFault::Exited);
        let sets_before = world.sets();
        let (mut harness, runs) = harness_with(&world);
        harness.start(&fixture, false, None);
        harness.wait_ready();
        wait_for(&harness.handle, "failure report", |status| {
            status.error.as_deref().is_some_and(|error| error.contains("not retried"))
        });
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(runs.load(Ordering::SeqCst), 1, "no retry loop");
        assert_eq!(
            world.sets(),
            sets_before,
            "the refused write did not apply and was not repeated"
        );
        assert!(
            world.journal().is_some(),
            "the journal stays for the next launch"
        );
        let error = harness.handle.status().error.expect("error");
        assert_eq!(error.matches("not retried").count(), 1, "reported once: {error}");
        harness.handle.request_shutdown();
        assert!(harness.wait_exit());
    }

    #[test]
    fn a_busy_lease_during_recovery_does_not_block_ready_or_shutdown() {
        let fixture = fixture();
        let world = stale(vec![KEY_A]);
        // Another cooperating process holds the lease: recovery waits on it.
        world.hold_lease(true);
        let (mut harness, _runs) = harness_with(&world);
        harness.start(&fixture, false, None);
        harness.wait_ready();
        harness.handle.request_shutdown();
        assert!(harness.wait_exit());
        assert_eq!(
            world.mappings(),
            vec![KEY_A, OWNED],
            "nothing was mutated without the lease"
        );
    }
}

// ---------------------------------------------------------------------------
// Hold-to-Quit (HQ-04-01): the production factory over fake native services
// ---------------------------------------------------------------------------

/// A host harness whose production-composed Hold-to-Quit runs over fakes: the
/// shared input service has a fake tap, applications and the HUD are fakes and
/// no tap, panel, observer or termination ever reaches the machine.
struct HoldHarness {
    harness: Harness,
    fakes: Fakes,
}

impl HoldHarness {
    fn new(permissions: Arc<FakePermissions>) -> Self {
        let fakes = Fakes::new();
        let mut harness = Harness::new(
            permissions,
            fakes.app_main.clone(),
            Arc::new(FakeCommands::default()),
            Arc::new(RecordingWindows::default()),
        );
        harness.input = Some(Arc::clone(&fakes.input));
        Self { harness, fakes }
    }

    fn start(&mut self, fixture: &Fixture) {
        let factories = ModuleFactories::new()
            .with(TestFactory {
                counters: Arc::clone(&self.harness.counters),
                permission: false,
                gate: None,
                configure_gate: None,
            })
            .with(HoldToQuitFactory::new(self.fakes.runtime.clone()));
        self.harness.start_with_factories(fixture, factories);
    }

    fn view(&self) -> Option<stache_app::bindings::ModuleStatusView> {
        self.harness
            .handle
            .status()
            .modules
            .into_iter()
            .find(|module| module.id == "commandQuit")
    }

    fn state(&self) -> Option<ModuleState> { self.view().map(|module| module.state) }

    fn wait_state(&self, state: ModuleState) {
        wait_for(
            &self.harness.handle,
            &format!("commandQuit {state:?}"),
            |status| status.modules.iter().any(|m| m.id == "commandQuit" && m.state == state),
        );
    }

    fn q_down(&self) -> Disposition {
        self.fakes.input.dispatch(TapKind::Suppressing, RawInput {
            kind: InputEventKind::KeyDown,
            stream: PressStream::Key(KeyCode(12)),
            flags: ModifierFlags::COMMAND,
            repeat: false,
            timestamp: Duration::ZERO,
            location: None,
        })
    }

    fn shutdown(&self) {
        self.harness.handle.request_shutdown();
        assert!(self.harness.wait_exit());
    }
}

#[test]
fn enable_writes_toml_and_pause_writes_nothing() {
    let fixture = fixture();
    let source = "# Keep this comment.\nenabled = false # note\nhold_duration = 900\n";
    fixture.write("commandQuit.toml", source);
    let mut hold = HoldHarness::new(FakePermissions::new(PermissionStatus::Granted));
    hold.start(&fixture);
    hold.harness.wait_ready();
    // Default-disabled and inert: no lease, observer, panel or tap exists.
    assert_eq!(hold.state(), Some(ModuleState::Disabled));
    assert_eq!(hold.fakes.leaser.leases(), 0);
    assert_eq!(hold.fakes.apps.observations(), 0);
    assert_eq!(hold.fakes.hud_backend.panels_created(), 0);
    assert!(hold.fakes.tap.submitted().is_empty());

    hold.harness.handle.set_enabled("commandQuit", true);
    hold.wait_state(ModuleState::Running);
    let edited = fixture.read("commandQuit.toml");
    assert_eq!(
        edited,
        "# Keep this comment.\nenabled = true # note\nhold_duration = 900\n"
    );
    assert_eq!(hold.fakes.leaser.held(), 1);
    assert_eq!(hold.fakes.hud_backend.panels_live(), 1);

    assert_enabled_running(&hold.harness.handle.status(), "commandQuit");

    hold.harness.handle.pause("commandQuit");
    hold.wait_state(ModuleState::Paused);
    assert_eq!(fixture.read("commandQuit.toml"), edited, "pause is session-only");
    assert_eq!(hold.fakes.leaser.held(), 0);
    assert_eq!(hold.q_down(), Disposition::Pass);
    hold.harness.handle.resume("commandQuit");
    hold.wait_state(ModuleState::Running);
    assert_eq!(hold.fakes.leaser.held(), 1);
    hold.shutdown();
}

#[test]
fn a_malformed_or_deleted_file_keeps_the_last_good_runtime_with_a_diagnostic() {
    let fixture = fixture();
    fixture.write("commandQuit.toml", "enabled = true\n");
    let mut hold = HoldHarness::new(FakePermissions::new(PermissionStatus::Granted));
    hold.start(&fixture);
    hold.harness.wait_ready();
    hold.wait_state(ModuleState::Running);
    fixture.write("commandQuit.toml", "enabled = ");
    wait_for(&hold.harness.handle, "a diagnostic", |status| {
        status.modules.iter().any(|m| m.id == "commandQuit" && m.diagnostic.is_some())
    });
    assert_eq!(
        hold.state(),
        Some(ModuleState::Running),
        "the last good state keeps running"
    );
    assert_eq!(hold.fakes.leaser.held(), 1);
    std::fs::remove_file(fixture.paths.directory().join("commandQuit.toml")).expect("delete");
    wait_for(&hold.harness.handle, "a deletion diagnostic", |status| {
        status.modules.iter().any(|m| m.id == "commandQuit" && m.diagnostic.is_some())
    });
    assert_eq!(hold.state(), Some(ModuleState::Running));
    assert_eq!(hold.fakes.hud_backend.panels_live(), 1);
    hold.shutdown();
}

#[test]
fn permission_denied_then_granted_then_revoked_never_prompts_automatically() {
    let fixture = fixture();
    fixture.write("commandQuit.toml", "enabled = true\n");
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut hold = HoldHarness::new(permissions.clone());
    hold.start(&fixture);
    hold.harness.wait_ready();
    hold.wait_state(ModuleState::WaitingForPermission);
    assert_eq!(hold.fakes.leaser.leases(), 0, "nothing starts while waiting");
    assert_eq!(hold.fakes.hud_backend.panels_created(), 0);

    permissions.set(PermissionStatus::Granted);
    hold.wait_state(ModuleState::Running);
    permissions.set(PermissionStatus::Denied);
    hold.wait_state(ModuleState::WaitingForPermission);
    assert_eq!(hold.fakes.leaser.held(), 0, "revocation releases everything");
    assert_eq!(hold.fakes.hud_backend.panels_live(), 0);
    permissions.set(PermissionStatus::Granted);
    hold.wait_state(ModuleState::Running);
    assert_eq!(
        permissions.requests.load(Ordering::SeqCst),
        0,
        "startup, polling, activation and recovery only check; only the tray prompts"
    );
    hold.shutdown();
}

#[test]
fn a_hud_install_failure_is_failed_start_and_retry_recovers() {
    let fixture = fixture();
    fixture.write("commandQuit.toml", "enabled = true\n");
    let mut hold = HoldHarness::new(FakePermissions::new(PermissionStatus::Granted));
    hold.fakes
        .hud_backend
        .fail_next_install(stache_macos::hold_hud::HudInstallError::NoScreen);
    hold.start(&fixture);
    hold.harness.wait_ready();
    hold.wait_state(ModuleState::Failed);
    let view = hold.view().expect("view");
    assert!(
        view.detail.as_deref().is_some_and(|d| d.contains("start")),
        "{view:?}"
    );
    assert_eq!(hold.fakes.leaser.held(), 0, "full rollback");
    assert_eq!(hold.fakes.apps.activation_observers(), 0);
    assert_eq!(
        module_view(&hold.harness.handle.status(), "commandQuit").state,
        ModuleState::Failed,
        "a failed module is eligible for Retry"
    );
    assert_eq!(hold.q_down(), Disposition::Pass, "callbacks are inert");
    hold.harness.handle.retry("commandQuit");
    hold.wait_state(ModuleState::Running);
    assert_eq!(hold.fakes.leaser.held(), 1);
    hold.shutdown();
}

#[test]
fn a_runtime_hud_failure_is_degraded_pass_through_with_a_detail() {
    let fixture = fixture();
    fixture.write("commandQuit.toml", "enabled = true\n");
    let mut hold = HoldHarness::new(FakePermissions::new(PermissionStatus::Granted));
    hold.start(&fixture);
    hold.harness.wait_ready();
    hold.wait_state(ModuleState::Running);
    // Put Finder-free eligible state in place, then break the next show.
    let editor = hold.fakes.apps.add_running("com.example.editor");
    hold.fakes.apps.set_name(editor, Some("Editor"));
    let _ = hold.fakes.apps.activate(editor);
    hold.fakes
        .input
        .context_publisher()
        .publish_frontmost(stache_sdk::input::FrontmostApp {
            bundle_id: Arc::from("com.example.editor"),
            exemptions: stache_sdk::input::ExemptionFlags(0),
        });
    hold.fakes
        .hud_backend
        .fail_next_show(stache_macos::hold_hud::HudShowFailure::NotInstalled);
    assert_eq!(hold.q_down(), Disposition::Suppress);
    // The detail is read from the runtime at each host publication, so the next
    // publication (any command, a permission tick or an activation refresh)
    // carries it.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        hold.harness.handle.refresh_permissions();
        if hold
            .view()
            .is_some_and(|m| m.detail.as_deref().is_some_and(|d| d.contains("passes through")))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the degraded detail never reached the status"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        hold.state(),
        Some(ModuleState::Running),
        "degraded mode stays Running"
    );
    assert_eq!(hold.fakes.leaser.held(), 1);
    // No subscription remains: a new press passes.
    assert_eq!(hold.q_down(), Disposition::Pass);
    // Pause then Resume recovers and clears the detail.
    hold.harness.handle.pause("commandQuit");
    hold.wait_state(ModuleState::Paused);
    hold.harness.handle.resume("commandQuit");
    hold.wait_state(ModuleState::Running);
    assert!(hold.view().expect("view").detail.is_none());
    hold.shutdown();
}

#[test]
fn unrelated_modules_keep_working_and_no_extra_surface_exists() {
    let fixture = fixture();
    fixture.write("commandQuit.toml", "enabled = true\n");
    fixture.write("testModule.toml", "enabled = true\n");
    let mut hold = HoldHarness::new(FakePermissions::new(PermissionStatus::Granted));
    hold.fakes.leaser.fail_next_lease();
    hold.start(&fixture);
    hold.harness.wait_ready();
    hold.wait_state(ModuleState::Failed);
    wait_for(&hold.harness.handle, "the unrelated module", |status| {
        status
            .modules
            .iter()
            .any(|m| m.id == "testModule" && m.state == ModuleState::Running)
    });
    // The module owns no window and registers no socket handler of its own.
    let status = hold.harness.handle.status();
    assert!(status.error.is_none(), "{:?}", status.error);
    hold.shutdown();
}

#[test]
fn the_hud_owner_acknowledges_a_retained_hud_and_is_idle_otherwise() {
    use stache_app::shutdown::OwnerState;
    let fakes = Fakes::new();
    let owner = HudOwner::new(Arc::clone(&fakes.hud));
    assert_eq!(owner.name(), "hold-to-quit-hud");
    assert!(!owner.on_main());
    assert_eq!(
        owner.release().state(),
        OwnerState::Succeeded,
        "nothing retained"
    );

    // Retain a real fake HUD whose release timed out.
    let (control, _events) = stache_macos::hold_hud::HudControl::with_acknowledgement_budget(
        fakes.hud_executor.clone(),
        Arc::new(fakes.hud_backend.clone()),
        Duration::from_millis(30),
    );
    fakes.hud_executor.set_on_main(true);
    control.install(&stache_sdk::CancellationToken::new()).expect("install");
    fakes.hud_executor.set_on_main(false);
    let outcome = control.release(&stache_sdk::CancellationToken::new());
    assert!(matches!(
        outcome,
        stache_macos::hold_hud::HudReleaseOutcome::Retained(_)
    ));
    fakes.hud.retain(control);
    assert!(fakes.hud.has_retained());
    // The main queue drains, then the owner's retry is acknowledged.
    fakes.hud_executor.set_on_main(true);
    fakes.hud_executor.drain();
    let answer = owner.release();
    let deadline = Instant::now() + Duration::from_secs(5);
    while answer.state() == OwnerState::Pending {
        assert!(Instant::now() < deadline, "the owner never answered");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(answer.state(), OwnerState::Succeeded);
    assert!(!fakes.hud.has_retained());
}

#[test]
fn the_hud_owner_release_never_waits_on_a_settle_holding_the_retained_lock() {
    use stache_app::shutdown::OwnerState;
    let fakes = Fakes::new();
    let owner = HudOwner::new(Arc::clone(&fakes.hud));
    let (control, _events) = stache_macos::hold_hud::HudControl::with_acknowledgement_budget(
        fakes.hud_executor.clone(),
        Arc::new(fakes.hud_backend.clone()),
        Duration::from_millis(1_500),
    );
    fakes.hud_executor.set_on_main(true);
    control.install(&stache_sdk::CancellationToken::new()).expect("install");
    fakes.hud_executor.set_on_main(false);
    fakes.hud.retain(control);
    // The executor never drains, so this settle holds the retained lock for the
    // whole acknowledgement budget.
    let hud = Arc::clone(&fakes.hud);
    let settling = std::thread::spawn(move || {
        let _ = hud.settle(&stache_sdk::CancellationToken::new());
    });
    std::thread::sleep(Duration::from_millis(200));
    let started = Instant::now();
    let answer = owner.release();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "release waited on the retained lock: {:?}",
        started.elapsed()
    );
    assert_eq!(answer.state(), OwnerState::Pending);
    settling.join().expect("settle");
    let deadline = Instant::now() + Duration::from_secs(10);
    while answer.state() == OwnerState::Pending {
        assert!(Instant::now() < deadline, "the owner never answered");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------------------
// Accessibility launch gate (spec 2026-11-07-accessibility-gate, part 02)
// ---------------------------------------------------------------------------

static ORDER_SERVICE: [stache_sdk::ServiceId; 1] = [stache_sdk::ServiceId::new("gateOrder")];

/// Defines one recording module, its config and its factory.
macro_rules! order_module {
    ($config:ident, $module:ident, $factory:ident, $id:literal, $provides:expr, $requires:expr) => {
        #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
        #[serde(default, rename_all = "camelCase")]
        struct $config {
            enabled: bool,
        }

        impl $config {
            const ID: ModuleId = ModuleId::new($id);
        }

        impl ModuleConfig for $config {
            fn enabled(&self) -> bool { self.enabled }

            fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
        }

        struct $module {
            events: Arc<Mutex<Vec<String>>>,
        }

        impl Module for $module {
            type Config = $config;

            fn descriptor(&self) -> ModuleDescriptor {
                ModuleDescriptor {
                    id: $config::ID,
                    provides: $provides,
                    requires: $requires,
                    permissions: &[],
                    executables: &[],
                    surfaces: &[],
                    observes: &[],
                    reserves_screen: false,
                }
            }

            fn configure(&mut self, _: &$config, _: &ModuleContext) -> ModuleResult<()> {
                self.events.lock().expect("events").push(format!("configure:{}", $id));
                Ok(())
            }

            fn start(&mut self, _: &ModuleContext) -> ModuleResult<()> {
                self.events.lock().expect("events").push(format!("start:{}", $id));
                Ok(())
            }

            fn reconfigure(&mut self, _: &$config, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

            fn stop(&mut self, _: &ModuleContext) -> ModuleResult<()> {
                self.events.lock().expect("events").push(format!("stop:{}", $id));
                Ok(())
            }
        }

        struct $factory {
            events: Arc<Mutex<Vec<String>>>,
        }

        impl ModuleFactory for $factory {
            fn id(&self) -> ModuleId { $config::ID }

            fn register(
                &self,
                registry: &stache_sdk::Registry,
                store: &stache_sdk::ConfigStore,
            ) -> Result<Arc<dyn stache_app::host::ModuleControl>, stache_app::host::HostError> {
                register_module(registry, store, $module {
                    events: Arc::clone(&self.events),
                })
            }
        }
    };
}

order_module!(
    AlphaConfig,
    AlphaModule,
    AlphaFactory,
    "gateAlpha",
    &ORDER_SERVICE,
    &[]
);
order_module!(
    BetaConfig,
    BetaModule,
    BetaFactory,
    "gateBeta",
    &[],
    &ORDER_SERVICE
);

/// A gated host over two recording modules where `gateBeta` needs `gateAlpha`.
struct GateHarness {
    harness: Harness,
    fixture: Fixture,
    events: Arc<Mutex<Vec<String>>>,
    commands: Arc<FakeCommands>,
    permissions: Arc<FakePermissions>,
}

impl GateHarness {
    fn new(permissions: Arc<FakePermissions>) -> Self {
        let mut fixture = fixture();
        fixture.catalog.register::<AlphaConfig>(AlphaConfig::ID).expect("alpha");
        fixture.catalog.register::<BetaConfig>(BetaConfig::ID).expect("beta");
        fixture.write("gateAlpha.toml", "enabled = true\n");
        fixture.write("gateBeta.toml", "enabled = true\n");
        fixture.write("global.toml", "exec_on_startup = [\"open -a Terminal\"]\n");
        let commands = Arc::new(FakeCommands::default());
        let mut harness = Harness::new(
            permissions.clone(),
            Arc::new(InlineMain),
            commands.clone(),
            Arc::new(RecordingWindows::default()),
        );
        harness.gated = true;
        Self {
            harness,
            fixture,
            events: Arc::new(Mutex::new(Vec::new())),
            commands,
            permissions,
        }
    }

    fn start(&mut self) {
        let factories = ModuleFactories::new()
            .with(AlphaFactory {
                events: Arc::clone(&self.events),
            })
            .with(BetaFactory {
                events: Arc::clone(&self.events),
            });
        self.harness.start_with_factories(&self.fixture, factories);
        self.harness.wait_ready();
    }

    fn lifecycle(&self) -> Vec<String> { event_log(&self.events) }

    fn access(&self) -> stache_app::bindings::AccessView { self.harness.handle.status().access }

    fn wait_access(&self, state: AccessState) {
        wait_for(&self.harness.handle, &format!("access {state:?}"), |status| {
            status.access.state == state
        });
    }

    fn wait_running(&self) {
        wait_for(&self.harness.handle, "both modules running", |status| {
            status.modules.len() == 2
                && status.modules.iter().all(|module| module.state == ModuleState::Running)
        });
    }

    fn startup_runs(&self) -> usize { self.commands.runs.lock().expect("runs").len() }

    /// The distinct consecutive `(state, revoked)` pairs the UI was shown.
    fn access_sequence(&self) -> Vec<(AccessState, bool)> {
        let mut sequence: Vec<(AccessState, bool)> = Vec::new();
        for status in self.harness.world.statuses.lock().expect("statuses").iter() {
            let pair = (status.access.state, status.access.revoked);
            if sequence.last() != Some(&pair) {
                sequence.push(pair);
            }
        }
        sequence
    }

    /// How many publishes changed the access state against the publish before
    /// them (the first publish counts against no access state).
    fn access_changing_publishes(&self) -> usize {
        let statuses = self.harness.world.statuses.lock().expect("statuses");
        let mut previous = None;
        let mut changes = 0;
        for status in statuses.iter() {
            let pair = (status.access.state, status.access.revoked);
            if previous != Some(pair) {
                changes += 1;
            }
            previous = Some(pair);
        }
        drop(statuses);
        changes
    }

    fn processed(&self, publishes: usize) {
        let before = self.harness.world.statuses.lock().expect("statuses").len();
        wait_for_publish(&self.harness.world, before + publishes);
    }

    fn shutdown(&self) {
        self.harness.handle.request_shutdown();
        assert!(self.harness.wait_exit());
    }
}

#[test]
fn an_untrusted_launch_blocks_every_module_before_any_lifecycle_call() {
    let mut gate = GateHarness::new(FakePermissions::new(PermissionStatus::Denied));
    gate.start();
    let first_ready = gate
        .harness
        .world
        .statuses
        .lock()
        .expect("statuses")
        .iter()
        .find(|status| status.ready)
        .cloned()
        .expect("a ready status");
    assert_eq!(first_ready.access.state, AccessState::Blocked);
    assert!(!first_ready.access.revoked);
    assert_eq!(first_ready.modules.len(), 2);
    assert!(
        first_ready
            .modules
            .iter()
            .all(|module| module.state == ModuleState::WaitingForPermission),
        "{:?}",
        first_ready.modules
    );
    assert!(first_ready.error.is_none(), "{:?}", first_ready.error);
    // Let several 5 ms ticks pass: nothing starts and nothing is requested.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(gate.lifecycle(), Vec::<String>::new());
    assert_eq!(gate.permissions.requests.load(Ordering::SeqCst), 0);
    assert_eq!(
        gate.startup_runs(),
        0,
        "exec_on_startup waits for the first grant"
    );
    gate.shutdown();
}

#[test]
fn a_gated_test_module_never_configures_or_starts() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = true\n");
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut harness = Harness::new(
        permissions.clone(),
        Arc::new(InlineMain),
        Arc::new(FakeCommands::default()),
        Arc::new(RecordingWindows::default()),
    );
    harness.gated = true;
    // The module itself declares no permission: the baseline alone gates it.
    harness.start(&fixture, false, None);
    harness.wait_ready();
    wait_for(&harness.handle, "waiting", |status| {
        status
            .modules
            .iter()
            .any(|module| module.state == ModuleState::WaitingForPermission)
    });
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(harness.counters.configure.load(Ordering::SeqCst), 0);
    assert_eq!(harness.counters.start.load(Ordering::SeqCst), 0);
    assert_eq!(permissions.requests.load(Ordering::SeqCst), 0);
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn with_every_module_disabled_the_gate_still_blocks_and_polls_fast() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = false\n");
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut harness = Harness::new(
        permissions.clone(),
        Arc::new(InlineMain),
        Arc::new(FakeCommands::default()),
        Arc::new(RecordingWindows::default()),
    );
    harness.gated = true;
    harness.start(&fixture, false, None);
    harness.wait_ready();
    assert_eq!(harness.handle.status().access.state, AccessState::Blocked);
    // The gate polls without any module waiting: a grant opens it unprompted.
    permissions.set(PermissionStatus::Granted);
    wait_for(&harness.handle, "open", |status| {
        status.access.state == AccessState::Open
    });
    assert_eq!(permissions.requests.load(Ordering::SeqCst), 0);
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn denied_denied_granted_opens_without_restart_in_dependency_order() {
    // The registry's own per-slot checks also read the provider, so a queue
    // consumed per call would be order-fragile: the status is changed between
    // observed ticks instead (Denied at launch, Denied on ticks, then Granted).
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut gate = GateHarness::new(permissions.clone());
    gate.start();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(gate.access().state, AccessState::Blocked);
    assert_eq!(gate.lifecycle(), Vec::<String>::new());
    assert_eq!(gate.startup_runs(), 0);
    permissions.set(PermissionStatus::Granted);
    gate.wait_running();
    assert_eq!(gate.lifecycle(), [
        "configure:gateAlpha",
        "start:gateAlpha",
        "configure:gateBeta",
        "start:gateBeta"
    ]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while gate.startup_runs() == 0 {
        assert!(
            Instant::now() < deadline,
            "exec_on_startup did not run after the grant"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(gate.startup_runs(), 1, "exactly once");
    assert_eq!(gate.access_sequence(), [
        (AccessState::Blocked, false),
        (AccessState::Open, false)
    ]);
    // One access-changing publish per transition: Blocked once, Open once.
    assert_eq!(gate.access_changing_publishes(), 2);
    assert_eq!(gate.permissions.requests.load(Ordering::SeqCst), 0);
    gate.shutdown();
}

#[test]
fn an_activation_refresh_opens_the_gate_before_the_next_tick() {
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut gate = GateHarness::new(permissions.clone());
    // Ticks are a minute apart, so only the activation refresh can open it.
    gate.harness.poll = PermissionPoll {
        waiting: Duration::from_secs(60),
        backoff_start: Duration::from_secs(60),
        backoff_max: Duration::from_secs(60),
    };
    gate.start();
    assert_eq!(gate.access().state, AccessState::Blocked);
    permissions.set(PermissionStatus::Granted);
    gate.harness.handle.refresh_permissions();
    gate.wait_access(AccessState::Open);
    gate.wait_running();
    gate.shutdown();
}

#[test]
fn revocation_stops_in_reverse_order_and_a_second_grant_restarts_without_startup_commands() {
    let permissions = FakePermissions::new(PermissionStatus::Granted);
    let mut gate = GateHarness::new(permissions.clone());
    gate.start();
    gate.wait_running();
    let deadline = Instant::now() + Duration::from_secs(10);
    while gate.startup_runs() == 0 {
        assert!(Instant::now() < deadline, "exec_on_startup did not run");
        std::thread::sleep(Duration::from_millis(5));
    }

    permissions.set(PermissionStatus::Denied);
    wait_for(&gate.harness.handle, "revoked", |status| {
        status.access.state == AccessState::Blocked && status.access.revoked
    });
    assert_eq!(&gate.lifecycle()[4..], ["stop:gateBeta", "stop:gateAlpha"]);
    assert!(
        gate.harness
            .handle
            .status()
            .modules
            .iter()
            .all(|module| module.state == ModuleState::WaitingForPermission)
    );

    permissions.set(PermissionStatus::Granted);
    gate.wait_running();
    assert_eq!(&gate.lifecycle()[6..], [
        "configure:gateAlpha",
        "start:gateAlpha",
        "configure:gateBeta",
        "start:gateBeta"
    ]);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(gate.startup_runs(), 1, "reopening never reruns exec_on_startup");
    assert_eq!(gate.access_sequence(), [
        (AccessState::Open, false),
        (AccessState::Blocked, true),
        (AccessState::Open, false)
    ]);
    assert_eq!(gate.permissions.requests.load(Ordering::SeqCst), 0);
    gate.shutdown();
}

#[test]
fn unknown_behaves_as_denied() {
    let permissions = FakePermissions::new(PermissionStatus::Unknown);
    let mut gate = GateHarness::new(permissions.clone());
    gate.start();
    assert_eq!(gate.access().state, AccessState::Blocked);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(gate.lifecycle(), Vec::<String>::new());

    permissions.set(PermissionStatus::Granted);
    gate.wait_running();
    permissions.set(PermissionStatus::Unknown);
    wait_for(&gate.harness.handle, "revoked", |status| {
        status.access.state == AccessState::Blocked && status.access.revoked
    });
    assert_eq!(&gate.lifecycle()[4..], ["stop:gateBeta", "stop:gateAlpha"]);
    gate.shutdown();
}

#[test]
fn only_an_explicit_request_while_blocked_calls_request() {
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut gate = GateHarness::new(permissions.clone());
    gate.start();
    let requests = || permissions.requests.load(Ordering::SeqCst);
    // Activation, enable (a reload of one file), config apply: zero requests.
    gate.harness.handle.refresh_permissions();
    gate.harness.handle.set_enabled("gateAlpha", false);
    gate.harness.handle.set_enabled("gateAlpha", true);
    gate.fixture.write("gateBeta.toml", "enabled = true\n# reload\n");
    gate.processed(3);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        requests(),
        0,
        "startup, ticks, activation, enable and reload never request"
    );

    gate.harness.handle.request_permission();
    gate.processed(1);
    assert_eq!(requests(), 1);
    gate.harness.handle.request_permission();
    gate.processed(1);
    assert_eq!(requests(), 2, "one request per explicit action");

    permissions.set(PermissionStatus::Granted);
    gate.wait_access(AccessState::Open);
    gate.harness.handle.request_permission();
    gate.processed(1);
    assert_eq!(requests(), 2, "an explicit request while open requests nothing");
    gate.shutdown();
}

#[test]
fn the_settings_opener_runs_only_while_blocked_and_failures_are_cleared_by_success() {
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut gate = GateHarness::new(permissions.clone());
    gate.start();
    let calls = || gate.harness.settings.calls.load(Ordering::SeqCst);

    gate.harness.handle.open_accessibility_settings();
    gate.processed(1);
    assert_eq!(calls(), 1);
    assert!(gate.harness.handle.status().error.is_none());

    gate.harness.settings.fail.store(true, Ordering::SeqCst);
    gate.harness.handle.open_accessibility_settings();
    gate.processed(1);
    assert_eq!(calls(), 2);
    let error = gate.harness.handle.status().error.expect("the opener failure is reported");
    assert!(error.contains("injected open failure"), "{error}");

    gate.harness.settings.fail.store(false, Ordering::SeqCst);
    gate.harness.handle.open_accessibility_settings();
    gate.processed(1);
    assert_eq!(calls(), 3);
    assert!(
        gate.harness.handle.status().error.is_none(),
        "a successful action clears it"
    );

    permissions.set(PermissionStatus::Granted);
    gate.wait_access(AccessState::Open);
    gate.harness.handle.open_accessibility_settings();
    // Commands run in order: this publishing refresh proves the open was handled.
    gate.harness.handle.refresh_permissions();
    gate.processed(1);
    assert_eq!(calls(), 3, "the opener is not called while open");
    gate.shutdown();
}

#[test]
fn a_shutdown_while_blocked_never_requests_or_opens_settings_afterwards() {
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut gate = GateHarness::new(permissions.clone());
    gate.start();
    gate.harness.handle.request_shutdown();
    gate.harness.handle.request_permission();
    gate.harness.handle.open_accessibility_settings();
    gate.harness.handle.refresh_permissions();
    assert!(gate.harness.wait_exit());
    let outcome = gate.harness.wait_teardown();
    gate.harness.wait_terminal();
    assert_eq!(outcome.result, TeardownResult::Clean, "{outcome}");
    assert_eq!(permissions.requests.load(Ordering::SeqCst), 0);
    assert_eq!(gate.harness.settings.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gate.lifecycle(), Vec::<String>::new());
    assert!(!gate.harness.handle.status().ready);
}

#[test]
fn a_gated_host_is_ready_with_a_blocked_access_view() {
    let mut gate = GateHarness::new(FakePermissions::new(PermissionStatus::Denied));
    gate.start();
    let status = gate.harness.handle.status();
    assert!(status.ready);
    assert_eq!(status.access.state, AccessState::Blocked);
    gate.shutdown();
}

// ---------------------------------------------------------------------------
// AG-03: the socket while the Accessibility gate is closed
// ---------------------------------------------------------------------------

const GATE_MESSAGE: &str = "Stache Next is waiting for Accessibility permission. Grant it in System Settings > Privacy & Security > Accessibility.";

struct EmptyAudio;

impl AudioBackend for EmptyAudio {
    fn devices(&self) -> Vec<AudioDevice> { Vec::new() }

    fn default_device(&self, _: AudioDirection) -> Option<AudioDevice> { None }

    fn set_default_device(&self, _: AudioDirection, _: u32) -> Result<(), AudioError> {
        Err(AudioError::new("set default audio device", -1))
    }

    fn is_hdmi(&self, _: u32) -> bool { false }

    fn subscribe(&self, _: AudioWakeup) -> Result<Box<dyn AudioChangeSubscription>, AudioError> {
        Err(AudioError::new("add audio property listener", -1))
    }
}

fn socket_client(fixture: &Fixture) -> IpcClient {
    IpcClient::with_options(
        fixture.app.socket_path().expect("socket"),
        RetryPolicy::single(),
        Duration::from_secs(5),
    )
}

fn assert_gate_refusal(response: Response) {
    match response {
        Response::Error { error } => {
            assert_eq!(error.kind, stache_sdk::ipc::WireErrorKind::Unavailable);
            assert_eq!(error.message, GATE_MESSAGE);
        }
        Response::Ok { data } => panic!("a gated method answered ok: {data}"),
    }
}

fn gated_harness(permissions: Arc<FakePermissions>) -> Harness {
    let mut harness = Harness::new(
        permissions,
        Arc::new(InlineMain),
        Arc::new(FakeCommands::default()),
        Arc::new(RecordingWindows::default()),
    );
    harness.gated = true;
    harness
}

#[test]
fn module_methods_answer_unavailable_through_a_real_router_while_blocked() {
    let fixture = fixture();
    fixture.write("wallpapers.toml", "enabled = false\nlist = [\"wall.png\"]\n");
    let image = fixture.paths.directory().join("wall.png");
    write_wallpaper_png(&image);
    let desktop = FakeDesktop::with_snapshot(wallpaper_snapshot());
    let backend: Arc<dyn GuardedDesktopBackend> = Arc::new(desktop.clone());
    let wallpapers = WallpapersFactory::with_service(wallpapers_service(&fixture, backend));
    let keep_awake = KeepAwakeFactory::new(
        AssertionOwner::new(Arc::new(FakeAssertionBackend::new())),
        Arc::new(FakeLockProbe::new(NativeLockState::Unlocked)),
        LockObservationInstaller::new(
            Arc::new(InlineMain),
            Arc::new(FakeLockObservationBackend::new()),
        ),
    );
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut harness = gated_harness(permissions.clone());
    harness.start_with_factories(
        &fixture,
        ModuleFactories::new()
            .with(keep_awake)
            .with(ProxyAudioFactory::new(Arc::new(EmptyAudio)))
            .with(wallpapers),
    );
    harness.wait_ready();
    let client = socket_client(&fixture);

    let params = WallpapersSetParams {
        config_directory: fixture.paths.directory().display().to_string(),
        selection: WallpaperSelection::File {
            path: image.display().to_string(),
        },
        screen: WallpaperScreen::All,
    };
    let requests = [
        Request::without_params("keepAwake", "status"),
        Request::without_params("proxyAudio", "list"),
        Request::new("wallpapers", "set", &params).expect("request"),
    ];
    for request in &requests {
        assert_gate_refusal(client.request(request).expect("answered"));
    }
    assert!(
        desktop.calls().is_empty(),
        "the refused set never reaches the desktop"
    );

    // core calls keep working and report the gate.
    let status = client.status().expect("core.status");
    let access = status.access.expect("host reports access");
    assert_eq!(access.state, stache_sdk::ipc::CoreAccessState::Blocked);
    assert!(!access.revoked);

    // Once trust arrives the same router delegates unchanged.
    permissions.set(PermissionStatus::Granted);
    wait_for(&harness.handle, "open", |status| {
        status.access.state == AccessState::Open
    });
    assert_eq!(
        client.status().expect("core.status").access.map(|access| access.state),
        Some(stache_sdk::ipc::CoreAccessState::Open)
    );
    match client
        .request(&Request::without_params("proxyAudio", "list"))
        .expect("answered")
    {
        Response::Ok { data } => assert_eq!(data, serde_json::json!([])),
        Response::Error { error } => panic!("an open gate refused: {error:?}"),
    }
    let outcome = match client.request(&requests[2]).expect("answered") {
        Response::Ok { data } => {
            serde_json::from_value::<WallpapersSetOutcome>(data).expect("typed outcome")
        }
        Response::Error { error } => panic!("an open gate refused: {error:?}"),
    };
    assert_eq!(outcome.status, WallpapersOutcomeStatus::FullSuccess);

    // Revocation closes the cell again, with the revoked flag.
    permissions.set(PermissionStatus::Denied);
    wait_for(&harness.handle, "revoked", |status| {
        status.access.state == AccessState::Blocked
    });
    assert_gate_refusal(client.request(&requests[1]).expect("answered"));
    let access = client.reload().expect("core.reload").access.expect("access");
    assert_eq!(access.state, stache_sdk::ipc::CoreAccessState::Blocked);
    assert!(access.revoked);

    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

struct ProbeHandler {
    calls: Arc<AtomicUsize>,
}

impl IpcHandler for ProbeHandler {
    fn handle(&self, _: &str, _: serde_json::Value) -> HandlerResult<serde_json::Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(serde_json::Value::Null)
    }
}

/// A module whose start blocks (holding its slot and lifecycle work) plus a
/// counting socket handler.
struct BusyStartFactory {
    counters: Arc<Counters>,
    start_gate: Arc<Gate>,
    calls: Arc<AtomicUsize>,
}

impl ModuleFactory for BusyStartFactory {
    fn id(&self) -> ModuleId { TestConfig::ID }

    fn register(
        &self,
        registry: &stache_sdk::Registry,
        store: &stache_sdk::ConfigStore,
    ) -> Result<Arc<dyn stache_app::host::ModuleControl>, stache_app::host::HostError> {
        register_module(registry, store, TestModule {
            counters: Arc::clone(&self.counters),
            permission: false,
            gate: Some(Arc::clone(&self.start_gate)),
            configure_gate: None,
        })
    }

    fn ipc_handlers(&self, _: &FactoryContext) -> Vec<(&'static str, Arc<dyn IpcHandler>)> {
        vec![(
            TestConfig::ID.as_str(),
            Arc::new(ProbeHandler { calls: Arc::clone(&self.calls) }),
        )]
    }
}

#[test]
fn a_gated_request_answers_while_the_lifecycle_lock_and_host_worker_are_busy() {
    let fixture = fixture();
    fixture.write("testModule.toml", "enabled = true\n");
    let counters = Arc::new(Counters::default());
    let start_gate = Gate::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut harness = gated_harness(permissions.clone());
    harness.start_with_factories(
        &fixture,
        ModuleFactories::new().with(BusyStartFactory {
            counters: Arc::clone(&counters),
            start_gate: Arc::clone(&start_gate),
            calls: Arc::clone(&calls),
        }),
    );
    harness.wait_ready();

    // Granting makes the worker start the module, and the start blocks inside
    // the registry with its slot held while the gate cell still reads closed.
    permissions.set(PermissionStatus::Granted);
    let deadline = Instant::now() + Duration::from_secs(10);
    while counters.start.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "the module never entered start");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_ne!(harness.handle.status().access.state, AccessState::Open);

    let client = IpcClient::with_options(
        fixture.app.socket_path().expect("socket"),
        RetryPolicy::single(),
        Duration::from_secs(2),
    );
    let request = Request::without_params(TestConfig::ID.as_str(), "anything");
    let started = Instant::now();
    let response = client.request(&request).expect("answered while the worker is busy");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_gate_refusal(response);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "the module handler was never called"
    );

    start_gate.open();
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

/// A gated host over the real window bookkeeping and a recording native host.
fn guide_harness(
    permissions: Arc<FakePermissions>,
    gated: bool,
) -> (Harness, Arc<NativeWindows>, Arc<FakeWindowHost>) {
    let host = Arc::new(FakeWindowHost::default());
    let windows = Arc::new(NativeWindows::new(
        Arc::new(InlineMain),
        host.clone(),
        Arc::new(DiscardLogger),
    ));
    let mut harness = Harness::new(
        permissions,
        Arc::new(InlineMain),
        Arc::new(FakeCommands::default()),
        windows.clone(),
    );
    harness.gated = gated;
    (harness, windows, host)
}

fn access_labels(host: &FakeWindowHost) -> Vec<String> {
    host.opened
        .lock()
        .expect("opened")
        .iter()
        .filter(|spec| spec.surface == stache_app::windows::ACCESS_SURFACE)
        .map(|spec| spec.label.clone())
        .collect()
}

fn wait_until(label: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn ui_appearance_is_applied_app_wide_at_startup_and_on_every_accepted_reload() {
    use stache_sdk::Appearance;
    let fixture = fixture();
    fixture.write("global.toml", "[ui]\nappearance = \"dark\"\n");
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let (mut harness, windows, host) = guide_harness(permissions, true);
    harness.start(&fixture, false, None);
    harness.wait_ready();
    wait_until("the guidance window", || access_labels(&host).len() == 1);
    assert_eq!(
        *host.appearances.lock().expect("appearances"),
        [Appearance::Dark],
        "the first accepted value is applied to the application"
    );
    let client = IpcClient::new(fixture.app.socket_path().expect("socket"));

    fixture.write("global.toml", "[ui]\nappearance = \"light\"\n");
    let report = client.reload().expect("reload");
    assert!(!report.has_failures(), "{report:?}");
    wait_until("the light re-apply", || {
        host.appearances.lock().expect("appearances").contains(&Appearance::Light)
    });

    fixture.write("global.toml", "[ui]\nappearance = \"sepia\"\n");
    let report = client.reload().expect("rejected reload");
    assert!(report.has_failures(), "{report:?}");

    // A later accepted change that is neither the default nor the current
    // value: once it is applied, the rejected reload was handled before it
    // and must have applied nothing (not even a fallback to the default).
    fixture.write("global.toml", "[ui]\nappearance = \"dark\"\n");
    let report = client.reload().expect("reload");
    assert!(!report.has_failures(), "{report:?}");
    wait_until("the dark re-apply", || {
        host.appearances.lock().expect("appearances").last() == Some(&Appearance::Dark)
            && host.appearances.lock().expect("appearances").len() > 2
    });
    assert_eq!(
        *host.appearances.lock().expect("appearances"),
        [Appearance::Dark, Appearance::Light, Appearance::Dark],
        "a rejected reload changes nothing"
    );
    assert_eq!(
        host.opened.lock().expect("opened").len(),
        1,
        "nothing is recreated"
    );
    assert_eq!(windows.len(), 1);
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn blocked_opens_one_window_a_grant_closes_it_and_a_revocation_opens_one_again() {
    let fixture = fixture();
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let (mut harness, windows, host) = guide_harness(permissions.clone(), true);
    harness.start(&fixture, false, None);
    harness.wait_ready();
    wait_until("the guidance window", || access_labels(&host).len() == 1);
    // Many more blocked ticks open nothing else.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(access_labels(&host).len(), 1);
    assert_eq!(windows.len(), 1);
    let first = access_labels(&host)[0].clone();
    assert!(first.starts_with("stache-access-"), "{first}");

    permissions.set(PermissionStatus::Granted);
    wait_until("the grant to close the window", || {
        host.closed.lock().expect("closed").contains(&first)
    });
    windows.window_closed(&first);
    assert!(windows.is_empty());

    permissions.set(PermissionStatus::Denied);
    wait_until("the revocation window", || access_labels(&host).len() == 2);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(access_labels(&host).len(), 2, "one window per episode");
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn a_user_close_is_not_reopened_by_more_blocked_ticks() {
    let fixture = fixture();
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let (mut harness, windows, host) = guide_harness(permissions, true);
    harness.start(&fixture, false, None);
    harness.wait_ready();
    wait_until("the guidance window", || access_labels(&host).len() == 1);
    let label = access_labels(&host)[0].clone();
    windows.window_closed(&label);
    assert!(windows.is_empty());
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(access_labels(&host).len(), 1, "same episode: no reopen");
    assert!(windows.is_empty());
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn the_tray_command_reopens_after_a_user_close_and_focuses_an_open_window() {
    let fixture = fixture();
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let (mut harness, windows, host) = guide_harness(permissions.clone(), true);
    harness.start(&fixture, false, None);
    harness.wait_ready();
    wait_until("the guidance window", || access_labels(&host).len() == 1);

    harness.handle.open_access_guide();
    wait_until("the focus", || host.focused.lock().expect("focused").len() == 1);
    assert_eq!(
        access_labels(&host).len(),
        1,
        "an open window is focused, not duplicated"
    );

    let label = access_labels(&host)[0].clone();
    windows.window_closed(&label);
    harness.handle.open_access_guide();
    wait_until("the reopened window", || access_labels(&host).len() == 2);

    // The reopened window is the one a grant closes.
    let second = access_labels(&host)[1].clone();
    permissions.set(PermissionStatus::Granted);
    wait_until("the grant to close the reopened window", || {
        host.closed.lock().expect("closed").contains(&second)
    });
    // Open: the row is absent and a late command opens nothing.
    harness.handle.open_access_guide();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(access_labels(&host).len(), 2);
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn a_trusted_launch_and_an_ungated_host_open_no_window() {
    for gated in [true, false] {
        let fixture = fixture();
        let permissions = FakePermissions::new(PermissionStatus::Granted);
        let (mut harness, windows, host) = guide_harness(permissions, gated);
        harness.start(&fixture, false, None);
        harness.wait_ready();
        std::thread::sleep(Duration::from_millis(100));
        assert!(host.opened.lock().expect("opened").is_empty(), "gated={gated}");
        assert!(host.closed.lock().expect("closed").is_empty());
        assert!(windows.is_empty());
        harness.handle.request_shutdown();
        assert!(harness.wait_exit());
    }
}

#[test]
fn an_ungated_host_with_a_missing_permission_still_opens_no_window() {
    let fixture = fixture();
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let (mut harness, _windows, host) = guide_harness(permissions, false);
    harness.start(&fixture, false, None);
    harness.wait_ready();
    std::thread::sleep(Duration::from_millis(100));
    assert!(host.opened.lock().expect("opened").is_empty());
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

// ---------------------------------------------------------------------------
// MenuAnywhere (MA-03-01/06/08): the production factory over fake native services
// ---------------------------------------------------------------------------

/// A host harness whose production-composed `MenuAnywhere` runs over fakes: the
/// shared input service has a fake tap, applications are fakes, the menu stack
/// is the real control over the Part 02 fakes and nothing reaches the machine.
struct MenuHarness {
    harness: Harness,
    fakes: MenuFakes,
}

impl MenuHarness {
    fn new(permissions: Arc<FakePermissions>) -> Self {
        let fakes = MenuFakes::new();
        let mut harness = Harness::new(
            permissions,
            fakes.apps_executor.clone(),
            Arc::new(FakeCommands::default()),
            Arc::new(RecordingWindows::default()),
        );
        harness.input = Some(Arc::clone(&fakes.input));
        Self { harness, fakes }
    }

    fn start(&mut self, fixture: &Fixture) {
        let factories = ModuleFactories::new()
            .with(TestFactory {
                counters: Arc::clone(&self.harness.counters),
                permission: false,
                gate: None,
                configure_gate: None,
            })
            .with(MenuAnywhereFactory::new(self.fakes.runtime.clone()));
        self.harness.start_with_factories(fixture, factories);
    }

    fn view(&self) -> Option<stache_app::bindings::ModuleStatusView> {
        self.harness
            .handle
            .status()
            .modules
            .into_iter()
            .find(|module| module.id == "menuAnywhere")
    }

    fn state(&self) -> Option<ModuleState> { self.view().map(|module| module.state) }

    fn wait_state(&self, state: ModuleState) {
        wait_for(
            &self.harness.handle,
            &format!("menuAnywhere {state:?}"),
            |status| status.modules.iter().any(|m| m.id == "menuAnywhere" && m.state == state),
        );
    }

    fn chord(&self, kind: InputEventKind) -> Disposition {
        self.fakes.input.dispatch(TapKind::Suppressing, RawInput {
            kind,
            stream: PressStream::Button(stache_sdk::input::MouseButton(1)),
            flags: ModifierFlags::CONTROL | ModifierFlags::COMMAND,
            repeat: false,
            timestamp: Duration::ZERO,
            location: Some(stache_sdk::input::InputPoint { x: 100.0, y: 100.0 }),
        })
    }

    fn shutdown(&self) {
        self.harness.handle.request_shutdown();
        assert!(self.harness.wait_exit());
    }
}

#[test]
fn enable_writes_toml_runs_the_trigger_and_pause_writes_nothing() {
    let fixture = fixture();
    let source =
        "# Keep this comment.\nenabled = false # note\nmodifiers = [\"control\", \"command\"]\n";
    fixture.write("menuAnywhere.toml", source);
    let mut menu = MenuHarness::new(FakePermissions::new(PermissionStatus::Granted));
    menu.start(&fixture);
    menu.harness.wait_ready();
    // Default-disabled and inert: no observer, thread, tap or menu exists.
    assert_eq!(menu.state(), Some(ModuleState::Disabled));
    assert_eq!(menu.fakes.apps.observations(), 0);
    assert_eq!(menu.fakes.menu.opens(), 0);
    assert!(menu.fakes.tap.submitted().is_empty());
    assert_eq!(menu.chord(InputEventKind::MouseDown), Disposition::Pass);

    menu.harness.handle.set_enabled("menuAnywhere", true);
    menu.wait_state(ModuleState::Running);
    let edited = fixture.read("menuAnywhere.toml");
    assert_eq!(
        edited,
        "# Keep this comment.\nenabled = true # note\nmodifiers = [\"control\", \"command\"]\n"
    );
    assert_eq!(menu.fakes.apps.activation_observers(), 1);
    assert_eq!(menu.fakes.menu.opens(), 1);
    assert_eq!(menu.fakes.menu.ax.call_count(), 0, "no read until a trigger");

    assert_enabled_running(&menu.harness.handle.status(), "menuAnywhere");

    menu.harness.handle.pause("menuAnywhere");
    menu.wait_state(ModuleState::Paused);
    assert_eq!(
        fixture.read("menuAnywhere.toml"),
        edited,
        "pause is session-only"
    );
    assert_eq!(menu.fakes.apps.activation_observers(), 0);
    assert_eq!(menu.chord(InputEventKind::MouseDown), Disposition::Pass);
    menu.harness.handle.resume("menuAnywhere");
    menu.wait_state(ModuleState::Running);
    menu.shutdown();
}

#[test]
fn permission_only_checks_and_only_the_explicit_tray_request_prompts() {
    let fixture = fixture();
    fixture.write("menuAnywhere.toml", "enabled = true\n");
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut menu = MenuHarness::new(permissions.clone());
    menu.start(&fixture);
    menu.harness.wait_ready();
    menu.wait_state(ModuleState::WaitingForPermission);
    assert_eq!(menu.fakes.menu.opens(), 0, "nothing starts while waiting");
    assert_eq!(menu.fakes.apps.observations(), 0);
    assert_eq!(
        permissions.requests.load(Ordering::SeqCst),
        0,
        "startup only checks"
    );

    // Reload, activation-style refresh and polling only check.
    fixture.write("menuAnywhere.toml", "enabled = true\nmodifiers = [\"shift\"]\n");
    let revision = menu.view().expect("view").revision;
    wait_for(&menu.harness.handle, "the reloaded revision", |status| {
        status.modules.iter().any(|m| m.id == "menuAnywhere" && m.revision != revision)
    });
    menu.harness.handle.refresh_permissions();
    assert_eq!(menu.state(), Some(ModuleState::WaitingForPermission));
    assert_eq!(permissions.requests.load(Ordering::SeqCst), 0);

    // Only the explicit tray request reaches the provider's `request`.
    menu.harness.handle.request_permission();
    wait_until_true("the explicit request", || {
        permissions.requests.load(Ordering::SeqCst) == 1
    });

    permissions.set(PermissionStatus::Granted);
    menu.wait_state(ModuleState::Running);
    permissions.set(PermissionStatus::Denied);
    menu.wait_state(ModuleState::WaitingForPermission);
    assert_eq!(
        menu.fakes.apps.activation_observers(),
        0,
        "revocation releases everything"
    );
    permissions.set(PermissionStatus::Granted);
    menu.wait_state(ModuleState::Running);
    assert_eq!(
        permissions.requests.load(Ordering::SeqCst),
        1,
        "enable, reload, polling, activation, revocation and recovery only check"
    );
    menu.shutdown();
}

#[test]
fn a_disabled_to_enabled_file_edit_only_checks_permission() {
    let fixture = fixture();
    fixture.write("menuAnywhere.toml", "enabled = false\n");
    let permissions = FakePermissions::new(PermissionStatus::Denied);
    let mut menu = MenuHarness::new(permissions.clone());
    menu.start(&fixture);
    menu.harness.wait_ready();
    assert_eq!(menu.state(), Some(ModuleState::Disabled));
    let revision = menu.view().expect("view").revision;

    fixture.write("menuAnywhere.toml", "enabled = true\n");
    wait_for(&menu.harness.handle, "the enabled revision", |status| {
        status.modules.iter().any(|m| m.id == "menuAnywhere" && m.revision != revision)
    });
    menu.wait_state(ModuleState::WaitingForPermission);
    assert_eq!(
        permissions.requests.load(Ordering::SeqCst),
        0,
        "enabling through the file only checks and never prompts"
    );
    assert_eq!(menu.fakes.menu.opens(), 0, "nothing starts while waiting");
    menu.shutdown();
}

fn wait_until_true(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_malformed_deleted_then_repaired_file_keeps_the_runtime_and_clears_the_diagnostic() {
    let fixture = fixture();
    fixture.write("menuAnywhere.toml", "enabled = true\n");
    let mut menu = MenuHarness::new(FakePermissions::new(PermissionStatus::Granted));
    menu.start(&fixture);
    menu.harness.wait_ready();
    menu.wait_state(ModuleState::Running);
    let revision = menu.view().expect("view").revision;

    fixture.write("menuAnywhere.toml", "enabled = ");
    wait_for(&menu.harness.handle, "a diagnostic", |status| {
        status.modules.iter().any(|m| m.id == "menuAnywhere" && m.diagnostic.is_some())
    });
    assert_eq!(
        menu.state(),
        Some(ModuleState::Running),
        "the last good state keeps running"
    );
    assert_eq!(
        menu.view().expect("view").revision,
        revision,
        "the accepted revision is retained"
    );
    assert_eq!(menu.fakes.apps.activation_observers(), 1);

    std::fs::remove_file(fixture.paths.directory().join("menuAnywhere.toml")).expect("delete");
    wait_for(&menu.harness.handle, "a deletion diagnostic", |status| {
        status.modules.iter().any(|m| m.id == "menuAnywhere" && m.diagnostic.is_some())
    });
    assert_eq!(menu.state(), Some(ModuleState::Running));

    fixture.write("menuAnywhere.toml", "enabled = true\nmodifiers = [\"shift\"]\n");
    wait_for(&menu.harness.handle, "the repaired file", |status| {
        status
            .modules
            .iter()
            .any(|m| m.id == "menuAnywhere" && m.diagnostic.is_none() && m.revision != revision)
    });
    assert_eq!(menu.state(), Some(ModuleState::Running));
    menu.shutdown();
}

#[test]
fn a_start_failure_is_failed_with_full_rollback_and_retry_recovers() {
    let fixture = fixture();
    fixture.write("menuAnywhere.toml", "enabled = true\n");
    let mut menu = MenuHarness::new(FakePermissions::new(PermissionStatus::Granted));
    menu.fakes.menu.fail_next_open();
    menu.start(&fixture);
    menu.harness.wait_ready();
    menu.wait_state(ModuleState::Failed);
    let view = menu.view().expect("view");
    assert!(
        view.detail.as_deref().is_some_and(|d| d.contains("start")),
        "{view:?}"
    );
    assert_eq!(menu.fakes.apps.activation_observers(), 0, "full rollback");
    assert!(!menu.fakes.tap.is_live(TapKind::Suppressing));
    assert_eq!(
        module_view(&menu.harness.handle.status(), "menuAnywhere").state,
        ModuleState::Failed
    );
    menu.harness.handle.retry("menuAnywhere");
    menu.wait_state(ModuleState::Running);
    menu.shutdown();
}

#[test]
fn an_unhealthy_running_module_is_projected_unavailable_with_its_reason() {
    let fixture = fixture();
    fixture.write("menuAnywhere.toml", "enabled = true\n");
    let mut menu = MenuHarness::new(FakePermissions::new(PermissionStatus::Granted));
    menu.start(&fixture);
    menu.harness.wait_ready();
    menu.wait_state(ModuleState::Running);
    menu.fakes.runtime.mark_degraded("the input subscription became inert");
    // The detail is read from the runtime at each host publication, so the next
    // publication carries it.
    wait_until_true("the degraded projection", || {
        menu.harness.handle.refresh_permissions();
        menu.state() == Some(ModuleState::Unavailable)
    });
    let view = menu.view().expect("view");
    assert!(
        view.detail.as_deref().is_some_and(|d| d.contains("inert")),
        "{view:?}"
    );
    menu.shutdown();
}

#[test]
fn supported_host_shutdown_with_an_open_menu_cancels_it_and_exits() {
    let fixture = fixture();
    fixture.write("menuAnywhere.toml", "enabled = true\n");
    let mut menu = MenuHarness::new(FakePermissions::new(PermissionStatus::Granted));
    menu.start(&fixture);
    menu.harness.wait_ready();
    menu.wait_state(ModuleState::Running);
    let app = {
        let key = menu.fakes.apps.add_running_with_pid("com.example.editor", 4242);
        menu.fakes.apps.set_name(key, Some("com.example.editor"));
        let _ = menu.fakes.apps.activate(key);
        key
    };
    let _ = app;
    assert_eq!(menu.chord(InputEventKind::MouseDown), Disposition::Suppress);
    assert_eq!(menu.chord(InputEventKind::MouseUp), Disposition::Suppress);
    wait_until_true("a tracking menu", || {
        menu.fakes
            .menu
            .presenter
            .tracked()
            .last()
            .is_some_and(|id| menu.fakes.menu.presenter.is_tracking(*id))
    });
    let id = *menu.fakes.menu.presenter.tracked().last().expect("tracked");
    let started = Instant::now();
    menu.shutdown();
    assert!(started.elapsed() < Duration::from_secs(8), "responsive shutdown");
    wait_until_true("the modal menu to be removed", || {
        menu.fakes.menu.presenter.removed().contains(&id)
    });
    wait_until_true("retained elements to be released", || {
        menu.fakes.menu.ax.counts().live() == 0
    });
    wait_until_true("the observer to be removed", || {
        menu.fakes.apps.activation_observers() == 0
    });
}

#[test]
fn a_missing_shared_input_service_is_projected_unavailable_not_failed() {
    let fixture = fixture();
    fixture.write("menuAnywhere.toml", "enabled = true\n");
    let mut menu = MenuHarness::new(FakePermissions::new(PermissionStatus::Granted));
    menu.harness.input = None;
    menu.start(&fixture);
    menu.harness.wait_ready();
    menu.wait_state(ModuleState::Unavailable);
    let view = menu.view().expect("view");
    assert!(
        view.detail.as_deref().is_some_and(|d| d.contains("shared input service")),
        "{view:?}"
    );
    assert_eq!(menu.fakes.menu.opens(), 0, "nothing was acquired");
    assert_eq!(menu.fakes.apps.observations(), 0);
    menu.shutdown();
}
