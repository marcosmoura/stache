//! Composition-root behavior with fake native and module services.
//!
//! These tests never touch the real home, configuration directory, runtime
//! socket, tray, Accessibility prompt or legacy process.

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use stache_app::bindings::{HostStatus, ModuleState};
use stache_app::host::{
    HostDriver, HostHandle, HostServices, HostSettings, HostUi, ModuleFactories, ModuleFactory,
    new_host, register_module,
};
use stache_app::permissions::PermissionPoll;
use stache_app::startup::CommandRunner;
use stache_app::windows::{NativeWindowSpec, NativeWindows, WindowHost};
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
    fail_next_start: std::sync::atomic::AtomicBool,
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

    fn stop(&mut self, _: &ModuleContext) -> ModuleResult<()> {
        self.counters.stop.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct TestFactory {
    counters: Arc<Counters>,
    permission: bool,
    gate: Option<Arc<Gate>>,
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
    exits: AtomicUsize,
    exit_codes: Mutex<Vec<i32>>,
}

impl HostUi for FakeWorld {
    fn status(&self, status: &HostStatus) {
        self.statuses.lock().expect("statuses").push(status.clone());
    }

    fn exit(&self, code: i32) {
        self.exits.fetch_add(1, Ordering::SeqCst);
        self.exit_codes.lock().expect("exit codes").push(code);
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
        let (handle, driver) = new_host();
        Self {
            handle,
            driver: Some(driver),
            world: Arc::new(FakeWorld::default()),
            counters: Arc::new(Counters::default()),
            permissions,
            commands,
            main,
            windows,
        }
    }

    fn start(&mut self, fixture: &Fixture, permission: bool, gate: Option<Arc<Gate>>) {
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
        let settings = HostSettings {
            app: fixture.app.clone(),
            config: fixture.paths.clone(),
            catalog: fixture.catalog.clone(),
            factories: ModuleFactories::new().with(TestFactory {
                counters: Arc::clone(&self.counters),
                permission,
                gate,
            }),
        };
        driver.start(settings, services, self.world.clone()).expect("start host");
    }

    fn wait_ready(&self) { wait_for(&self.handle, "ready", |status| status.ready); }

    fn wait_exit(&self) -> bool { self.handle.wait_for_exit(Duration::from_secs(10)) }
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
    assert_eq!(harness.world.exits.load(Ordering::SeqCst), 1);
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
    assert_eq!(
        *second.world.exit_codes.lock().expect("exit codes"),
        vec![1],
        "a host startup failure must not be converted to success"
    );
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
    harness.handle.request_shutdown();
    assert_eq!(harness.world.exits.load(Ordering::SeqCst), 1);
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
    assert_eq!(harness.world.exits.load(Ordering::SeqCst), 1);
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
    assert_eq!(queued.drain(), 1);
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
