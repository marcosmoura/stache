//! Composition-root behavior with fake native and module services.
//!
//! These tests never touch the real home, configuration directory, runtime
//! socket, tray, Accessibility prompt or legacy process.

use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stache_app::bindings::{HostStatus, ModuleState};
use stache_app::host::{
    HostDriver, HostHandle, HostServices, HostSettings, HostUi, ModuleFactories, ModuleFactory,
    NoTunesFactory, new_host, register_module,
};
use stache_app::permissions::PermissionPoll;
use stache_app::shutdown::{
    InstanceOwner, ManualClock, RecordDelivery, Teardown, TeardownCause, TeardownRecordChannel,
    TeardownResult, TerminalAction, TerminalDriver, report_terminate_install_failure,
};
use stache_app::startup::CommandRunner;
use stache_app::tray::{TrayItem, TrayRow};
use stache_app::windows::{NativeWindowSpec, NativeWindows, WindowHost};
use stache_macos::{ApplicationControl, FakeApplicationBackend, ManualExecutor};
use stache_sdk::ipc::IpcClient;
use stache_sdk::{
    AppEnvironment, AppPaths, ConfigCatalog, ConfigError, ConfigPaths, DiscardLogger, EventBus,
    LogFilter, MainThreadExecutor, Module, ModuleConfig, ModuleContext, ModuleDescriptor, ModuleId,
    ModuleResult, Permission, PermissionError, PermissionProvider, PermissionStatus, SurfaceId,
    ThreadError, WindowBackend, WindowError, WindowId, WindowRequest,
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
}

impl FakeTerminal {
    fn actions(&self) -> Vec<TerminalAction> { self.actions.lock().expect("actions").clone() }
}

impl TerminalDriver for FakeTerminal {
    fn dispatch(&self, action: TerminalAction) {
        self.actions.lock().expect("actions").push(action);
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
}

impl WindowHost for FakeWindowHost {
    fn open(&self, spec: &NativeWindowSpec) -> Result<(), String> {
        self.opened.lock().expect("opened").push(spec.clone());
        Ok(())
    }

    fn focus(&self, label: &str) { self.focused.lock().expect("focused").push(label.to_owned()); }

    fn close(&self, label: &str) { self.closed.lock().expect("closed").push(label.to_owned()); }
}

#[derive(Default)]
struct RecordingWindows {
    opened: AtomicUsize,
    closed: AtomicUsize,
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

    fn close_all(&self) { self.closed.fetch_add(1, Ordering::SeqCst); }
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
    clock: Arc<ManualClock>,
    records: Arc<RecordingRecords>,
    instance: Arc<FakeInstance>,
    terminal: Arc<FakeTerminal>,
    configure_gate: Option<Arc<Gate>>,
    world: Arc<FakeWorld>,
    counters: Arc<Counters>,
    permissions: Arc<FakePermissions>,
    commands: Arc<dyn CommandRunner>,
    main: Arc<dyn MainThreadExecutor>,
    windows: Arc<dyn WindowBackend>,
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
            configure_gate: None,
            world: Arc::new(FakeWorld::default()),
            counters: Arc::new(Counters::default()),
            permissions,
            commands,
            main,
            windows,
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
        let driver = self.driver.take().expect("driver");
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
            poll: PermissionPoll {
                waiting: Duration::from_millis(5),
                backoff_start: Duration::from_millis(5),
                backoff_max: Duration::from_millis(5),
            },
        };
        let mut factories = ModuleFactories::new().with(TestFactory {
            counters: Arc::clone(&self.counters),
            permission,
            gate,
            configure_gate: self.configure_gate.clone(),
        });
        if let Some(control) = no_tunes {
            factories = factories.with(NoTunesFactory::new(control));
        }
        let settings = HostSettings {
            app: fixture.app.clone(),
            config: fixture.paths.clone(),
            catalog: fixture.catalog.clone(),
            factories,
        };
        driver.start(settings, services, self.world.clone()).expect("start host");
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
    clock: Arc<ManualClock>,
    records: Arc<RecordingRecords>,
    instance: Arc<FakeInstance>,
    terminal: Arc<FakeTerminal>,
}

fn default_teardown() -> TeardownParts {
    let clock = Arc::new(ManualClock::new());
    let records = Arc::new(RecordingRecords::default());
    let instance = Arc::new(FakeInstance::default());
    let terminal = Arc::new(FakeTerminal::default());
    let teardown = Teardown::new(
        clock.clone(),
        records.clone(),
        instance.clone(),
        Arc::new(DiscardLogger),
    );
    teardown.install_terminal(terminal.clone());
    teardown.start().expect("start the teardown coordinator");
    TeardownParts {
        teardown,
        clock,
        records,
        instance,
        terminal,
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
    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
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
        "global.jsonc",
        &format!(
            "{{ \"execOnStartup\": [\"/bin/sh -c 'echo $$ > {}; exec /bin/sleep 60'\"] }}\n",
            pid_file.display()
        ),
    );
    let windows = Arc::new(RecordingWindows::default());
    let mut harness = Harness::new(
        FakePermissions::new(PermissionStatus::Granted),
        Arc::new(InlineMain),
        Arc::new(stache_app::startup::ProcessCommands),
        windows.clone(),
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
    harness.handle.open_test_window();
    let deadline = Instant::now() + Duration::from_secs(10);
    while windows.opened.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < deadline,
            "tray window command was not processed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        windows.opened.load(Ordering::SeqCst),
        1,
        "tray commands must run while startup is still executing"
    );
    harness.handle.request_shutdown();
    assert!(harness.wait_exit(), "shutdown must not wait for the child");
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
    fixture.write("global.jsonc", "{ \"execOnStartup\": [\"open -a Terminal\"] }\n");
    fixture.write("testModule.jsonc", "{ \"enabled\": false }\n");
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

    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
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
    let source = "{\n  // Keep this comment.\n  \"enabled\": false,\n  \"value\": 7\n}\n";
    fixture.write("testModule.jsonc", source);
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
    let edited = fixture.read("testModule.jsonc");
    assert!(edited.contains("// Keep this comment."));
    assert!(edited.contains("\"value\": 7"));
    assert!(edited.contains("\"enabled\": true"));
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
    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    harness.handle.pause("testModule");
    wait_for(&harness.handle, "paused", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Paused)
    });
    assert_eq!(fixture.read("testModule.jsonc"), "{ \"enabled\": true }\n");

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
    assert!(fixture.read("testModule.jsonc").contains("\"enabled\": false"));
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn an_invalid_disk_file_is_reported_and_never_overwritten() {
    let fixture = fixture();
    let invalid = "{ \"enabled\": ";
    fixture.write("testModule.jsonc", invalid);
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    let status = harness.handle.status();
    assert!(status.config.iter().any(|file| file.file == "testModule.jsonc"
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
        fixture.read("testModule.jsonc"),
        invalid,
        "invalid bytes must stay"
    );
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn a_valid_external_edit_is_applied_once_by_the_watcher() {
    let fixture = fixture();
    fixture.write("testModule.jsonc", "{ \"enabled\": false }\n");
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
    wait_for(&harness.handle, "started by watcher", |status| {
        status.modules.iter().any(|module| module.state == ModuleState::Running)
    });
    assert_eq!(harness.counters.configure.load(Ordering::SeqCst), 1);
    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(harness.counters.configure.load(Ordering::SeqCst), 1);
    assert_eq!(harness.counters.reconfigure.load(Ordering::SeqCst), 0);
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn a_grant_starts_waiting_modules_without_restart_and_prompts_once() {
    let fixture = fixture();
    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
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
    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
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
    fixture.write("testModule.jsonc", "{ \"enabled\": false }\n");
    let mut harness = plain_harness();
    harness.start(&fixture, false, None);
    harness.wait_ready();
    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
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
    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
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
    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
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
    fixture.write("global.jsonc", "{ \"execOnStartup\": [\"false\"] }\n");
    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
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
            .is_some_and(|error| error.contains("execOnStartup failed"))
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
            .is_some_and(|error| error.contains("execOnStartup failed")),
        "a later successful action must not hide a failed startup command"
    );
    harness.handle.request_shutdown();
    assert!(harness.wait_exit());
}

#[test]
fn a_successful_enable_clears_a_stale_action_error() {
    let fixture = fixture();
    fixture.write("testModule.jsonc", "{ \"enabled\": false }\n");
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
    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
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
    assert_eq!(harness.counters.stop.load(Ordering::SeqCst), 1);
    harness.wait_teardown();
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
    let mut harness = Harness::new(
        FakePermissions::new(PermissionStatus::Granted),
        queued.clone(),
        Arc::new(FakeCommands::default()),
        windows.clone(),
    );
    harness.start(&fixture, false, None);
    harness.wait_ready();
    harness.handle.open_test_window();
    let deadline = Instant::now() + Duration::from_secs(10);
    while windows.is_empty() {
        assert!(Instant::now() < deadline, "test window was not requested");
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
    assert_eq!(queued.drain(), 1);
    assert_eq!(host.closed.lock().expect("closed").len(), 1);
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

fn tray_row<'a>(rows: &'a [TrayRow], id: &str) -> &'a TrayItem {
    rows.iter()
        .find_map(|row| match row {
            TrayRow::Item(item) if item.id == id => Some(item),
            TrayRow::Item(_) | TrayRow::Separator => None,
        })
        .unwrap_or_else(|| panic!("no tray row `{id}`"))
}

#[test]
fn notunes_start_publishes_generic_status_and_tray_rows_without_opening_anything() {
    let fixture = fixture();
    fixture.write("notunes.jsonc", "{ \"enabled\": true }\n");
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

    let rows = stache_app::tray::rows(&status);
    assert_eq!(tray_row(&rows, "enable:notunes").checked, Some(true));
    assert_eq!(tray_row(&rows, "state:notunes").label, "  notunes: running");
    assert_eq!(tray_row(&rows, "pause:notunes").label, "Pause notunes");
    let first = rows
        .iter()
        .position(|row| matches!(row, TrayRow::Item(item) if item.id == "state:testModule"))
        .expect("test module rows");
    let blocker = rows
        .iter()
        .position(|row| matches!(row, TrayRow::Item(item) if item.id == "state:notunes"))
        .expect("blocker rows");
    assert!(first < blocker, "tray rows keep registration order");

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
    fixture.write("notunes.jsonc", "{ \"targetApp\": \"winamp\" }\n");
    let mut notunes = NotunesHarness::new(FakePermissions::new(PermissionStatus::Granted));
    notunes.start(&fixture);
    notunes.harness.wait_ready();

    let status = notunes.harness.handle.status();
    let view = status
        .config
        .iter()
        .find(|file| file.file == "notunes.jsonc")
        .expect("generic config diagnostics list the file");
    assert_eq!(
        view.state,
        stache_app::bindings::ConfigFileState::Invalid,
        "{view:?}"
    );
    assert!(
        view.diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("targetApp")),
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
        "notunes.jsonc",
        "{ \"enabled\": true, \"targetApp\": \"com.example.player\" }\n",
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
        .find(|file| file.file == "notunes.jsonc")
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
    fixture.write("notunes.jsonc", "{ \"enabled\": false }\n");
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
    assert_eq!(
        notunes.backend.observations(),
        0,
        "shutdown leaves no live observer"
    );
}

#[test]
fn a_denied_permission_neither_blocks_notunes_nor_prompts() {
    let fixture = fixture();
    fixture.write("notunes.jsonc", "{ \"enabled\": true }\n");
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
    fixture.write("testModule.jsonc", "{ \"enabled\": false }\n");
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
        fixture.read("testModule.jsonc").contains("\"enabled\": true"),
        "the configuration commit must already be on disk"
    );

    let requested = Instant::now();
    harness.handle.request_shutdown();
    harness.clock.advance(stache_app::shutdown::TEARDOWN_TOTAL);
    let outcome = harness.wait_teardown();
    assert_eq!(outcome.result, TeardownResult::Uncertain);
    assert_eq!(outcome.recorded_code, 1);
    assert!(
        outcome.failed.is_empty(),
        "a blocked command did not report a failure"
    );
    assert_eq!(outcome.unacknowledged, vec!["host-worker"]);
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

    // Let the abandoned command finish; its late answer must not upgrade the
    // outcome or write a second record.
    gate.open();
    assert!(harness.wait_exit());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(harness.teardown.outcome(), Some(outcome));
    assert_eq!(harness.records().len(), 1);
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(1)]);
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
    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
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
    assert_eq!(outcome.failed, vec!["host-worker"]);
    harness.wait_terminal();
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(1)]);
    assert!(!harness.instance.released.load(Ordering::SeqCst));
    assert_eq!(harness.records().len(), 1);
    assert!(harness.records()[0].contains("cause=restart"));
}

#[test]
fn an_uncertain_restart_finishes_without_relaunching() {
    let fixture = fixture();
    fixture.write("testModule.jsonc", "{ \"enabled\": false }\n");
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
    harness.clock.advance(stache_app::shutdown::TEARDOWN_TOTAL);
    let outcome = harness.wait_teardown();
    assert_eq!(outcome.result, TeardownResult::Uncertain);
    assert_eq!(outcome.unacknowledged, vec!["host-worker"]);
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
    assert_eq!(
        harness.wait_teardown().result,
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
    fixture.write("testModule.jsonc", "{ \"enabled\": false }\n");
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

    fixture.write("testModule.jsonc", "{ \"enabled\": true }\n");
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
