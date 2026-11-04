//! Keep Awake host and socket integration over deterministic native fakes.
//!
//! Every test drives the real factory, registry, configuration watcher and
//! `IpcServer`. The owner, lock probe and observation backend are the
//! `stache-macos` testing doubles, so no IOPM assertion is created, no screen
//! lock is read for real and no distributed observer is registered.

#![allow(clippy::significant_drop_tightening)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use stache_app::bindings::{HostStatus, KeepAwakeStatus, ModuleState};
use stache_app::host::{
    HostDriver, HostHandle, HostServices, HostSettings, HostUi, KeepAwakeFactory, ModuleFactories,
    new_host,
};
use stache_app::keep_awake::PowerAssertionOwner;
use stache_app::permissions::PermissionPoll;
use stache_app::shutdown::{
    ExitProcess, InstanceOwner, NativeSlot, RecordDelivery, SystemClock, TEARDOWN_TOTAL, Teardown,
    TeardownOutcome, TeardownRecordChannel, TeardownResult, TerminalAction, TerminalDriver,
    TerminalProgress,
};
use stache_app::startup::CommandRunner;
use stache_keep_awake::{
    AssertionState, KeepAwakeConditionKind, KeepAwakeConfig, KeepAwakeRuntimeStatus,
    LockObservation, LockState as RuntimeLockState,
};
use stache_macos::{
    AssertionId, AssertionKind, AssertionOwner, FakeAssertionBackend, FakeLockObservationBackend,
    FakeLockProbe, FakeNativeBarrier, LockObservationInstaller, LockObserverHandle, LockProbe,
    LockState as NativeLockState, LockUnknown, OwnedAssertion,
};
use stache_sdk::ipc::{IpcClient, Request, Response};
use stache_sdk::{
    AppEnvironment, AppPaths, ConfigPaths, DiscardLogger, EventBus, LogFilter, MainThreadExecutor,
    Permission, PermissionError, PermissionProvider, PermissionStatus, ThreadError,
    UnavailableWindows,
};

const WAIT: Duration = Duration::from_secs(10);
const BARRIER: Duration = Duration::from_secs(5);
const FAILURE_CODE: i32 = -536_870_000;
const GOOD: &str = "{ \"enabled\": true }\n";

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("root");
    let app = AppPaths::resolve(&AppEnvironment {
        home: Some(root.path().to_path_buf()),
        temporary: Some(root.path().to_path_buf()),
    })
    .expect("app paths");
    let config = ConfigPaths::new(root.path().join("config"));
    let catalog = stache_app::catalog::catalog().expect("catalog");
    Fixture {
        _root: root,
        config,
        app,
        catalog,
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    config: ConfigPaths,
    app: AppPaths,
    catalog: stache_sdk::ConfigCatalog,
}

impl Fixture {
    fn file(&self) -> PathBuf { self.config.module_file(KeepAwakeConfig::ID) }

    fn write(&self, text: &str) {
        fs::create_dir_all(self.config.directory()).expect("config dir");
        fs::write(self.file(), text).expect("write keepAwake");
    }

    fn remove(&self) { fs::remove_file(self.file()).expect("remove keepAwake"); }

    fn read(&self) -> String { fs::read_to_string(self.file()).expect("read keepAwake") }

    fn stamp(&self) -> (Vec<u8>, SystemTime) {
        let metadata = fs::metadata(self.file()).expect("keepAwake metadata");
        (
            fs::read(self.file()).expect("keepAwake bytes"),
            metadata.modified().expect("mtime"),
        )
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

struct AllowAll;

impl PermissionProvider for AllowAll {
    fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

    fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(PermissionStatus::Granted)
    }
}

struct NoCommands;

impl CommandRunner for NoCommands {
    fn run(&self, _: &[String], _: &stache_sdk::CancellationToken) -> Result<(), String> { Ok(()) }
}

struct QuietUi;

impl HostUi for QuietUi {
    fn status(&self, _: &stache_app::bindings::HostStatus) {}
}

#[derive(Default)]
struct RecordingRecords {
    lines: Mutex<Vec<String>>,
}

impl TeardownRecordChannel for RecordingRecords {
    fn record(&self, line: &str, _budget: Duration) -> RecordDelivery {
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(line.to_owned());
        RecordDelivery::Delivered
    }
}

/// Records the one terminal action and reports it delivered immediately.
#[derive(Default)]
struct FakeTerminal {
    actions: Mutex<Vec<TerminalAction>>,
}

impl FakeTerminal {
    fn actions(&self) -> Vec<TerminalAction> {
        self.actions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

impl TerminalDriver for FakeTerminal {
    fn dispatch(&self, action: TerminalAction) -> TerminalProgress {
        self.actions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(action);
        let progress = TerminalProgress::new();
        let _ = progress.claim(Instant::now());
        progress.mark_delivered();
        progress
    }
}

/// The fake forced exit: no test can end the test process.
#[derive(Default)]
struct FakeExit {
    codes: Mutex<Vec<i32>>,
}

impl FakeExit {
    fn codes(&self) -> Vec<i32> {
        self.codes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

impl ExitProcess for FakeExit {
    fn exit(&self, code: i32) {
        self.codes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(code);
    }
}

#[derive(Default)]
struct FakeInstance;

impl InstanceOwner for FakeInstance {
    fn release(&self) {}
}

struct TeardownParts {
    teardown: Arc<Teardown>,
    terminal: Arc<FakeTerminal>,
    exit: Arc<FakeExit>,
}

/// The production clock, so a hung owner is abandoned at its real bound.
fn teardown_parts() -> TeardownParts {
    let records = Arc::new(RecordingRecords::default());
    let terminal = Arc::new(FakeTerminal::default());
    let exit = Arc::new(FakeExit::default());
    let teardown = Teardown::new(
        Arc::new(SystemClock),
        records as Arc<dyn TeardownRecordChannel>,
        Arc::new(FakeInstance) as Arc<dyn InstanceOwner>,
        Arc::new(DiscardLogger),
    );
    teardown.install_terminal(terminal.clone() as Arc<dyn TerminalDriver>);
    teardown.install_exit(exit.clone() as Arc<dyn ExitProcess>);
    TeardownParts { teardown, terminal, exit }
}

struct Harness {
    handle: HostHandle,
    driver: Option<HostDriver>,
    client: IpcClient,
    teardown: Arc<Teardown>,
    terminal: Arc<FakeTerminal>,
    exit: Arc<FakeExit>,
    assertion: FakeAssertionBackend,
    probe: TestProbe,
    observation: FakeLockObservationBackend,
}

impl Harness {
    fn new(fixture: &Fixture, probe: NativeLockState) -> Self {
        let parts = teardown_parts();
        let (handle, driver) = new_host(Arc::clone(&parts.teardown));
        let assertion = FakeAssertionBackend::new();
        assertion.set_barrier_timeout(BARRIER);
        let observation = FakeLockObservationBackend::new();
        observation.set_barrier_timeout(BARRIER);
        Self {
            handle,
            driver: Some(driver),
            client: IpcClient::new(fixture.app.socket_path().expect("socket")),
            teardown: parts.teardown,
            terminal: parts.terminal,
            exit: parts.exit,
            assertion,
            probe: TestProbe::new(probe),
            observation,
        }
    }

    fn start(&mut self, fixture: &Fixture) {
        // The module and the teardown slot must share one owner, so the ledger
        // the adapter reads is the one the module releases.
        let owner = AssertionOwner::new(Arc::new(self.assertion.clone()));
        self.teardown.register_owner(
            NativeSlot::PowerAssertion,
            Arc::new(PowerAssertionOwner::new(owner.clone())),
        );
        let factory = KeepAwakeFactory::new(
            owner,
            Arc::new(self.probe.clone()),
            LockObservationInstaller::new(Arc::new(InlineMain), Arc::new(self.observation.clone())),
        );
        let settings = HostSettings {
            app: fixture.app.clone(),
            config: fixture.config.clone(),
            catalog: fixture.catalog.clone(),
            factories: ModuleFactories::new().with(factory),
        };
        let services = HostServices {
            logger: Arc::new(DiscardLogger),
            log_filter: Arc::new(LogFilter::default()),
            events: Arc::new(EventBus::new()),
            main_thread: Arc::new(InlineMain),
            permissions: Arc::new(AllowAll),
            windows: Arc::new(UnavailableWindows),
            commands: Arc::new(NoCommands),
            poll: PermissionPoll {
                waiting: Duration::from_millis(5),
                backoff_start: Duration::from_millis(5),
                backoff_max: Duration::from_millis(5),
            },
        };
        let driver = self.driver.take().expect("driver");
        driver.start(settings, services, Arc::new(QuietUi)).expect("start host");
        self.teardown.start().expect("start the teardown coordinator");
    }

    fn wait_ready(&self) { wait_for(&self.handle, "ready", |status| status.ready); }

    fn status(&self) -> KeepAwakeStatus { query(&self.client) }

    fn wait_status(
        &self,
        label: &str,
        predicate: impl Fn(&KeepAwakeStatus) -> bool,
    ) -> KeepAwakeStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            let status = self.status();
            if predicate(&status) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {label}: {status:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn shutdown(&self) -> TeardownOutcome {
        self.handle.request_shutdown();
        let outcome =
            self.teardown.wait_for_outcome(WAIT).expect("the coordinator stored an outcome");
        assert!(
            self.teardown.wait_for_terminal(WAIT),
            "the coordinator dispatched its terminal action"
        );
        assert!(
            self.handle.wait_for_exit(WAIT),
            "the host did not finish shutting down"
        );
        outcome
    }
}

fn wait_for(handle: &HostHandle, label: &str, predicate: impl Fn(&HostStatus) -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        if predicate(&handle.status()) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_until(label: &str, predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    loop {
        if predicate() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn query(client: &IpcClient) -> KeepAwakeStatus {
    match client
        .request(&Request::without_params("keepAwake", "status"))
        .expect("status request")
    {
        Response::Ok { data } => serde_json::from_value(data).expect("KeepAwakeStatus"),
        Response::Error { error } => panic!("keepAwake.status failed: {error}"),
    }
}

fn has_condition(status: &KeepAwakeStatus, kind: KeepAwakeConditionKind) -> bool {
    status.runtime.conditions.iter().any(|condition| condition.kind == kind)
}

fn snapshot_diagnostic(harness: &Harness) -> Option<String> {
    harness
        .handle
        .status()
        .modules
        .into_iter()
        .find(|module| module.id == "keepAwake")
        .and_then(|module| module.diagnostic)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A counting lock probe that can park candidate validation.
///
/// It delegates to the deterministic fake so the runtime sees ordinary lock
/// results. The optional barrier waits until all three IDs exist, which only the
/// post-creation validation probe in one pass does.
struct ProbeState {
    calls: AtomicUsize,
    validation: Mutex<Option<(FakeAssertionBackend, Arc<FakeNativeBarrier>)>>,
}

#[derive(Clone)]
struct TestProbe {
    fake: FakeLockProbe,
    state: Arc<ProbeState>,
}

impl TestProbe {
    fn new(state: NativeLockState) -> Self {
        Self {
            fake: FakeLockProbe::new(state),
            state: Arc::new(ProbeState {
                calls: AtomicUsize::new(0),
                validation: Mutex::new(None),
            }),
        }
    }

    fn set(&self, state: NativeLockState) { self.fake.set(state); }

    fn calls(&self) -> usize { self.state.calls.load(Ordering::SeqCst) }

    fn block_validation(&self, backend: FakeAssertionBackend) -> Arc<FakeNativeBarrier> {
        let barrier = Arc::new(FakeNativeBarrier::default());
        *lock(&self.state.validation) = Some((backend, Arc::clone(&barrier)));
        barrier
    }
}

impl LockProbe for TestProbe {
    fn probe(&self) -> NativeLockState {
        self.state.calls.fetch_add(1, Ordering::SeqCst);
        let plan = {
            let mut validation = lock(&self.state.validation);
            let ready =
                validation.as_ref().is_some_and(|(backend, _)| backend.created().len() >= 3);
            if ready { validation.take() } else { None }
        };
        if let Some((_, barrier)) = plan {
            let _ = barrier.park(None, BARRIER);
        }
        self.fake.probe()
    }
}

fn disabled_case(source: Option<&str>) {
    let fixture = fixture();
    if let Some(text) = source {
        fixture.write(text);
    }
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.start(&fixture);
    harness.wait_ready();

    let status = harness.status();
    assert_eq!(status.module.id, "keepAwake");
    assert_eq!(status.module.state, ModuleState::Disabled);
    assert!(!status.module.enabled);
    assert!(!status.module.paused);
    assert_eq!(status.module.revision, "1");
    assert!(status.module.diagnostic.is_none());
    assert_eq!(status.runtime, KeepAwakeRuntimeStatus::default());

    assert!(harness.assertion.created().is_empty());
    assert!(harness.assertion.create_attempts().is_empty());
    assert!(harness.assertion.release_attempts().is_empty());
    assert_eq!(
        harness.probe.calls(),
        0,
        "a disabled module must not probe the lock"
    );
    assert!(harness.observation.install_calls().is_empty());
    assert!(harness.observation.remove_calls().is_empty());
    if source.is_none() {
        assert!(
            !fixture.file().exists(),
            "registration must not write the configuration file"
        );
    }

    for _ in 0..5 {
        let _ = harness.status();
    }
    assert_eq!(harness.probe.calls(), 0, "status queries must not probe");
    harness.shutdown();
}

#[test]
fn a_disabled_module_answers_status_without_native_activity() {
    disabled_case(None);
    disabled_case(Some("{ \"enabled\": false }\n"));
}

#[test]
fn an_invalid_initial_file_is_diagnosed_and_a_valid_rewrite_recovers() {
    let fixture = fixture();
    fixture.write("{ \"enabled\": ");
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.start(&fixture);
    harness.wait_ready();

    let status = harness.status();
    assert_eq!(status.module.state, ModuleState::Stopped);
    assert!(!status.module.enabled);
    assert_eq!(status.module.revision, "0");
    assert!(
        status
            .module
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("keepAwake.jsonc")),
        "the held file diagnostic is visible: {status:?}"
    );
    assert_eq!(status.runtime, KeepAwakeRuntimeStatus::default());
    assert_eq!(
        harness.probe.calls(),
        0,
        "a rejected file never starts the runtime"
    );
    assert!(harness.assertion.created().is_empty());
    assert!(harness.observation.install_calls().is_empty());

    fixture.write(GOOD);
    let status = harness.wait_status("recovered after a valid rewrite", |status| {
        status.module.state == ModuleState::Running
            && status.runtime.assertion_state == AssertionState::Active
    });
    assert!(status.module.diagnostic.is_none());
    assert_eq!(status.module.revision, "1");
    assert_eq!(harness.assertion.created().len(), 3);
    harness.shutdown();
}

#[test]
fn a_failed_start_answers_status_and_generic_retry_recovers() {
    let fixture = fixture();
    fixture.write(GOOD);
    let unknown = NativeLockState::Unknown(LockUnknown::SessionDictionaryUnavailable);
    let mut harness = Harness::new(&fixture, unknown);
    harness.start(&fixture);
    harness.wait_ready();

    let status = harness.wait_status("failed start", |status| {
        status.module.state == ModuleState::Failed
    });
    assert_eq!(status.runtime, KeepAwakeRuntimeStatus::default());
    assert!(
        status
            .module
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("lock state")),
        "{status:?}"
    );
    assert!(harness.assertion.created().is_empty());
    assert_eq!(harness.observation.install_calls().len(), 1);
    assert_eq!(harness.observation.remove_calls().len(), 1);
    let calls = harness.probe.calls();
    assert!(calls >= 1);

    harness.probe.set(NativeLockState::Unlocked);
    harness.handle.retry("keepAwake");
    let status = harness.wait_status("retry recovered", |status| {
        status.module.state == ModuleState::Running
            && status.runtime.assertion_state == AssertionState::Active
    });
    assert!(status.module.detail.is_none());
    assert!(status.module.diagnostic.is_none());
    assert!(harness.probe.calls() > calls, "retry probes the lock again");
    assert_eq!(harness.observation.install_calls().len(), 2);
    assert_eq!(harness.assertion.created().len(), 3);
    harness.shutdown();
}

#[test]
fn pause_and_resume_are_session_only_and_reprobe_without_writing() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.start(&fixture);
    harness.wait_ready();
    harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });
    let before = fixture.stamp();
    let calls = harness.probe.calls();

    harness.handle.pause("keepAwake");
    let status = harness.wait_status("paused", |status| {
        status.module.state == ModuleState::Paused
            && status.runtime.assertion_state == AssertionState::Inactive
    });
    assert!(status.module.enabled);
    assert!(status.module.paused);
    assert_eq!(status.runtime, KeepAwakeRuntimeStatus::default());
    assert_eq!(fixture.stamp(), before, "pause must not write the configuration");
    wait_until("pause released the lease", || {
        harness.assertion.release_attempts().len() == 3
    });

    let installed = harness.observation.install_calls().len();
    harness.handle.resume("keepAwake");
    harness.wait_status("resumed", |status| {
        status.module.state == ModuleState::Running
            && status.runtime.assertion_state == AssertionState::Active
    });
    assert!(harness.probe.calls() > calls, "resume probes the lock again");
    assert_eq!(harness.observation.install_calls().len(), installed + 1);
    assert_eq!(
        fixture.stamp(),
        before,
        "resume must not write the configuration"
    );
    harness.shutdown();
}

#[test]
fn a_locked_start_runs_without_acquiring() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Locked);
    harness.start(&fixture);
    harness.wait_ready();

    let status = harness.wait_status("running while locked", |status| {
        status.module.state == ModuleState::Running
    });
    assert_eq!(status.runtime.assertion_state, AssertionState::Inactive);
    assert_eq!(status.runtime.lock_state, RuntimeLockState::Locked);
    assert_eq!(
        status.runtime.observation,
        LockObservation::NotificationsAndPolling
    );
    assert!(status.runtime.conditions.is_empty());
    assert!(harness.assertion.created().is_empty());
    assert!(harness.assertion.create_attempts().is_empty());
    assert!(harness.probe.calls() >= 1);
    assert_eq!(harness.observation.install_calls().len(), 1);
    harness.shutdown();
}

#[test]
fn a_blocked_native_create_is_acquiring_on_the_wire() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Locked);
    harness.start(&fixture);
    harness.wait_ready();
    harness.wait_status("running while locked", |status| {
        status.module.state == ModuleState::Running
    });

    let barrier = harness.assertion.block_next_create();
    harness.probe.set(NativeLockState::Unlocked);
    assert!(
        harness.observation.deliver(),
        "the live observer must be installed"
    );
    assert!(barrier.wait_entered(WAIT), "the native create must park");

    let started = Instant::now();
    for _ in 0..5 {
        let status = harness.status();
        assert_eq!(status.runtime.assertion_state, AssertionState::Acquiring);
        assert!(!has_condition(
            &status,
            KeepAwakeConditionKind::AssertionReleaseFailed
        ));
        assert_eq!(status.module.state, ModuleState::Running);
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "status must answer promptly"
    );
    assert!(
        harness.assertion.created().is_empty(),
        "no ID is recorded before the native create returns"
    );

    assert!(barrier.release());
    harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });
    assert_eq!(harness.assertion.created().len(), 3);
    harness.shutdown();
}

#[test]
fn a_blocked_candidate_validation_stays_acquiring_on_the_wire() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Locked);
    harness.start(&fixture);
    harness.wait_ready();
    harness.wait_status("running while locked", |status| {
        status.module.state == ModuleState::Running
    });

    let barrier = harness.probe.block_validation(harness.assertion.clone());
    harness.probe.set(NativeLockState::Unlocked);
    assert!(harness.observation.deliver());
    assert!(barrier.wait_entered(WAIT), "candidate validation must park");

    for _ in 0..5 {
        let status = harness.status();
        assert_eq!(status.runtime.assertion_state, AssertionState::Acquiring);
        assert!(!has_condition(
            &status,
            KeepAwakeConditionKind::AssertionReleaseFailed
        ));
    }
    assert_eq!(
        harness.assertion.created().len(),
        3,
        "all IDs exist but none is adopted"
    );
    assert!(harness.assertion.release_attempts().is_empty());

    assert!(barrier.release());
    harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });
    harness.shutdown();
}

#[test]
fn an_active_status_reports_the_complete_three_id_lease() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.assertion.set_ids([11, 12, 13]);
    harness.start(&fixture);
    harness.wait_ready();

    let status = harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });
    assert_eq!(status.runtime.lock_state, RuntimeLockState::Unlocked);
    assert_eq!(
        status.runtime.observation,
        LockObservation::NotificationsAndPolling
    );
    assert!(status.runtime.conditions.is_empty());
    assert_eq!(harness.assertion.created(), [
        OwnedAssertion {
            kind: AssertionKind::Display,
            id: AssertionId::new(11)
        },
        OwnedAssertion {
            kind: AssertionKind::Idle,
            id: AssertionId::new(12)
        },
        OwnedAssertion {
            kind: AssertionKind::System,
            id: AssertionId::new(13)
        },
    ]);
    assert!(harness.assertion.release_attempts().is_empty());
    harness.shutdown();
}

#[test]
fn a_normal_lock_release_is_release_pending_until_confirmed() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.start(&fixture);
    harness.wait_ready();
    harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });

    let barrier = harness.assertion.block_next_release();
    harness.probe.set(NativeLockState::Locked);
    assert!(harness.observation.deliver());
    assert!(barrier.wait_entered(WAIT), "the native release must park");

    for _ in 0..3 {
        let status = harness.status();
        assert_eq!(status.runtime.assertion_state, AssertionState::ReleasePending);
        assert_eq!(status.runtime.lock_state, RuntimeLockState::Locked);
        assert!(!has_condition(
            &status,
            KeepAwakeConditionKind::AssertionReleaseFailed
        ));
        assert_eq!(status.module.state, ModuleState::Running);
    }
    assert_eq!(harness.assertion.release_attempts().len(), 1);

    assert!(barrier.release());
    let status = harness.wait_status("released", |status| {
        status.runtime.assertion_state == AssertionState::Inactive
    });
    assert!(status.runtime.conditions.is_empty());
    assert_eq!(harness.assertion.release_attempts().len(), 3);
    assert_eq!(harness.assertion.created().len(), 3);
    harness.shutdown();
}

#[test]
fn a_failed_creation_with_a_prefix_is_release_pending_without_a_false_failure() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Locked);
    harness.start(&fixture);
    harness.wait_ready();
    harness.wait_status("running while locked", |status| {
        status.module.state == ModuleState::Running
    });

    harness.assertion.fail_next_create(AssertionKind::Idle, FAILURE_CODE);
    let barrier = harness.assertion.block_next_release();
    harness.probe.set(NativeLockState::Unlocked);
    assert!(harness.observation.deliver());
    assert!(barrier.wait_entered(WAIT), "the rollback release must park");

    for _ in 0..3 {
        let status = harness.status();
        assert_eq!(status.runtime.assertion_state, AssertionState::ReleasePending);
        assert!(!has_condition(
            &status,
            KeepAwakeConditionKind::AssertionReleaseFailed
        ));
    }
    assert_eq!(harness.assertion.created().len(), 1, "only the prefix exists");
    assert_eq!(harness.assertion.release_attempts().len(), 1);

    // Keep the next scheduled pass inert so it cannot acquire again.
    harness.probe.set(NativeLockState::Locked);
    assert!(barrier.release());
    let status = harness.wait_status("rollback settled", |status| {
        status.runtime.assertion_state == AssertionState::Inactive
            && has_condition(status, KeepAwakeConditionKind::AssertionAcquireFailed)
    });
    assert!(!has_condition(
        &status,
        KeepAwakeConditionKind::AssertionReleaseFailed
    ));
    assert_eq!(status.module.state, ModuleState::Running);
    harness.shutdown();
}

#[test]
fn an_actual_release_failure_is_reported_and_recovers() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.start(&fixture);
    harness.wait_ready();
    harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });

    harness.assertion.fail_release_always(AssertionKind::Display, FAILURE_CODE);
    harness.probe.set(NativeLockState::Locked);
    assert!(harness.observation.deliver());
    let status = harness.wait_status("release failed", |status| {
        status.runtime.assertion_state == AssertionState::ReleasePending
            && has_condition(status, KeepAwakeConditionKind::AssertionReleaseFailed)
    });
    assert_eq!(status.runtime.lock_state, RuntimeLockState::Locked);
    assert_eq!(status.module.state, ModuleState::Running);

    harness.assertion.clear_failures();
    assert!(harness.observation.deliver());
    let status = harness.wait_status("recovered after cleanup", |status| {
        status.runtime.assertion_state == AssertionState::Inactive
    });
    assert!(status.runtime.conditions.is_empty());
    assert_eq!(status.module.state, ModuleState::Running);
    harness.shutdown();
}

#[test]
fn retry_resolves_a_failed_disable_and_a_failed_pause_without_starting() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.start(&fixture);
    harness.wait_ready();
    harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });

    harness.assertion.fail_release_always(AssertionKind::Display, FAILURE_CODE);
    harness.handle.pause("keepAwake");
    harness.wait_status("failed pause", |status| {
        status.module.state == ModuleState::Failed
            && status.runtime.assertion_state == AssertionState::ReleasePending
    });
    let before = fixture.stamp();
    harness.assertion.clear_failures();
    harness.handle.retry("keepAwake");
    harness.wait_status("pause retry", |status| {
        status.module.state == ModuleState::Paused
            && status.runtime.assertion_state == AssertionState::Inactive
    });
    assert_eq!(fixture.stamp(), before, "retry must not write the configuration");
    assert_eq!(
        harness.assertion.created().len(),
        3,
        "a paused retry starts nothing"
    );

    harness.handle.resume("keepAwake");
    harness.wait_status("resumed", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });
    harness.assertion.fail_release_always(AssertionKind::Display, FAILURE_CODE);
    harness.handle.set_enabled("keepAwake", false);
    let status = harness.wait_status("failed disable", |status| {
        status.module.state == ModuleState::Failed
            && !status.module.enabled
            && has_condition(status, KeepAwakeConditionKind::AssertionReleaseFailed)
    });
    assert_eq!(status.runtime.assertion_state, AssertionState::ReleasePending);

    let created = harness.assertion.created().len();
    harness.assertion.clear_failures();
    harness.handle.retry("keepAwake");
    let status = harness.wait_status("disable retry", |status| {
        status.module.state == ModuleState::Disabled
            && status.runtime.assertion_state == AssertionState::Inactive
    });
    assert!(status.runtime.conditions.is_empty());
    assert_eq!(
        harness.assertion.created().len(),
        created,
        "a disabled retry starts nothing"
    );
    harness.shutdown();
}

#[derive(Debug, PartialEq, Eq)]
struct Observed {
    created: Vec<OwnedAssertion>,
    create_attempts: Vec<AssertionKind>,
    release_attempts: Vec<(AssertionKind, AssertionId)>,
    probes: usize,
    installs: Vec<(LockObserverHandle, u64)>,
    removes: Vec<(LockObserverHandle, u64)>,
    file: (Vec<u8>, SystemTime),
}

impl Harness {
    fn observe(&self, fixture: &Fixture) -> Observed {
        Observed {
            created: self.assertion.created(),
            create_attempts: self.assertion.create_attempts(),
            release_attempts: self.assertion.release_attempts(),
            probes: self.probe.calls(),
            installs: self.observation.install_calls(),
            removes: self.observation.remove_calls(),
            file: fixture.stamp(),
        }
    }
}

#[test]
fn a_query_while_the_worker_is_parked_changes_nothing() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.start(&fixture);
    harness.wait_ready();
    harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });

    let barrier = harness.assertion.block_next_release();
    harness.probe.set(NativeLockState::Locked);
    assert!(harness.observation.deliver());
    assert!(barrier.wait_entered(WAIT), "the native release must park");

    let before = harness.observe(&fixture);
    for _ in 0..50 {
        let status = harness.status();
        assert_eq!(status.runtime.assertion_state, AssertionState::ReleasePending);
    }
    assert_eq!(
        harness.observe(&fixture),
        before,
        "a query changed no cell or file"
    );

    assert!(barrier.release());
    harness.wait_status("released", |status| {
        status.runtime.assertion_state == AssertionState::Inactive
    });
    harness.shutdown();
}

#[test]
fn a_running_lease_survives_rejected_files_and_identical_good_bytes_repair() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.start(&fixture);
    harness.wait_ready();
    let active = harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });
    let revision = active.module.revision;
    let created = harness.assertion.created().len();
    let releases = harness.assertion.release_attempts().len();

    for (label, text, fragment) in [
        ("malformed JSON", "{ \"enabled\": ", "keepAwake.jsonc"),
        (
            "wrong value type",
            "{ \"enabled\": \"yes\" }\n",
            "expected a boolean",
        ),
    ] {
        fixture.write(text);
        let status = harness.wait_status(label, |status| {
            status
                .module
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| diagnostic.contains(fragment))
        });
        assert!(
            status
                .module
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| diagnostic.contains(fragment)),
            "{label}: {status:?}"
        );
        assert_eq!(
            status.module.revision, revision,
            "{label}: the revision is unchanged"
        );
        assert_eq!(status.module.state, ModuleState::Running, "{label}");
        assert_eq!(status.runtime.assertion_state, AssertionState::Active, "{label}");
        assert_eq!(status.runtime.lock_state, RuntimeLockState::Unlocked, "{label}");
        assert_eq!(harness.assertion.created().len(), created, "{label}");
        assert_eq!(harness.assertion.release_attempts().len(), releases, "{label}");
        assert!(
            snapshot_diagnostic(&harness).is_none(),
            "{label}: no snapshot diagnostic"
        );
    }

    fixture.remove();
    let status = harness.wait_status("deleted file", |status| {
        status
            .module
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("file was deleted"))
    });
    assert_eq!(status.module.revision, revision);
    assert_eq!(status.runtime.assertion_state, AssertionState::Active);
    assert_eq!(harness.assertion.created().len(), created);
    assert_eq!(harness.assertion.release_attempts().len(), releases);
    assert!(snapshot_diagnostic(&harness).is_none());

    fixture.write(GOOD);
    let status = harness.wait_status("identical good bytes", |status| {
        status.module.diagnostic.is_none()
    });
    assert_eq!(
        status.module.revision, revision,
        "a repair does not bump the revision"
    );
    assert_eq!(status.module.state, ModuleState::Running);
    assert_eq!(status.runtime.assertion_state, AssertionState::Active);
    assert!(status.runtime.conditions.is_empty());
    assert_eq!(
        harness.assertion.created().len(),
        created,
        "no replacement lease"
    );
    assert_eq!(
        harness.assertion.release_attempts().len(),
        releases,
        "no lease churn"
    );
    assert!(
        snapshot_diagnostic(&harness).is_none(),
        "the snapshot diagnostic clears too"
    );
    harness.shutdown();
}

#[test]
fn host_enable_and_disable_edit_jsonc_and_an_echo_creates_no_new_resources() {
    let source = "{\n  // Keep this comment.\n  \"enabled\": false\n}\n";
    let fixture = fixture();
    fixture.write(source);
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.start(&fixture);
    harness.wait_ready();

    let status = harness.status();
    assert_eq!(status.module.state, ModuleState::Disabled);
    assert!(!status.module.enabled);

    harness.handle.set_enabled("keepAwake", true);
    harness.wait_status("enabled", |status| {
        status.module.state == ModuleState::Running
            && status.runtime.assertion_state == AssertionState::Active
    });
    let edited = fixture.read();
    assert!(edited.contains("// Keep this comment."), "{edited}");
    assert!(edited.contains("\"enabled\": true"), "{edited}");
    let created = harness.assertion.created().len();
    let installs = harness.observation.install_calls().len();
    assert_eq!(created, 3);
    assert_eq!(installs, 1);

    // Rewriting the accepted bytes produces a watcher echo for an unchanged
    // file. It must not install a second observer or create another lease.
    fixture.write(&edited);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        harness.assertion.created().len(),
        created,
        "the echo created no lease"
    );
    assert_eq!(
        harness.assertion.create_attempts().len(),
        3,
        "the echo attempted no native create"
    );
    assert_eq!(
        harness.observation.install_calls().len(),
        installs,
        "the echo installed no second observer"
    );

    harness.handle.set_enabled("keepAwake", false);
    harness.wait_status("disabled again", |status| {
        status.module.state == ModuleState::Disabled
            && status.runtime.assertion_state == AssertionState::Inactive
    });
    let edited = fixture.read();
    assert!(edited.contains("// Keep this comment."), "{edited}");
    assert!(edited.contains("\"enabled\": false"), "{edited}");
    wait_until("disable released the lease", || {
        harness.assertion.release_attempts().len() == 3
    });
    harness.shutdown();
}

#[test]
fn shutdown_releases_the_lease_removes_observation_and_ends_clean() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.assertion.set_ids([7, 8, 9]);
    harness.start(&fixture);
    harness.wait_ready();
    harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });

    let outcome = harness.shutdown();
    assert_eq!(outcome.result, TeardownResult::Clean, "{outcome}");
    assert!(outcome.failed.is_empty(), "{outcome}");
    assert!(outcome.unacknowledged.is_empty(), "{outcome}");
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(0)]);
    assert!(harness.exit.codes().is_empty(), "no forced exit was needed");
    assert!(harness.handle.is_finished());

    let created = harness.assertion.created();
    assert_eq!(created.len(), 3);
    let released = harness.assertion.release_attempts();
    for owned in &created {
        assert!(
            released.contains(&(owned.kind, owned.id)),
            "shutdown released {owned:?}: {released:?}"
        );
    }
    assert!(
        !harness.observation.is_installed(),
        "shutdown removed the observation"
    );
    assert_eq!(harness.observation.remove_calls().len(), 1);
}

#[test]
fn a_persistent_release_failure_is_partial_and_never_restarts() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.start(&fixture);
    harness.wait_ready();
    harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });
    harness.assertion.fail_release_always(AssertionKind::Display, FAILURE_CODE);

    harness.handle.request_restart();
    let outcome = harness
        .teardown
        .wait_for_outcome(WAIT)
        .expect("the coordinator stored an outcome");
    assert!(harness.teardown.wait_for_terminal(WAIT));
    assert!(harness.handle.wait_for_exit(WAIT));
    assert_eq!(outcome.result, TeardownResult::Partial, "{outcome}");
    assert_eq!(
        outcome.failed,
        ["module:keepAwake", "power-assertion"],
        "the module owns cleanup and the slot only acknowledges it: {outcome}"
    );
    assert!(outcome.unacknowledged.is_empty(), "{outcome}");
    assert_eq!(outcome.recorded_code, 1);
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(1)]);
    assert!(
        !harness.terminal.actions().contains(&TerminalAction::Restart),
        "a partial outcome never relaunches"
    );
    assert!(harness.exit.codes().is_empty(), "no forced exit was needed");
    assert!(
        harness
            .assertion
            .release_attempts()
            .iter()
            .any(|(kind, _)| *kind == AssertionKind::Display),
        "the module attempted the owned release"
    );
}

#[test]
fn a_barrier_hung_release_is_uncertain_without_a_false_clean() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.start(&fixture);
    harness.wait_ready();
    harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });

    let barrier = harness.assertion.block_next_release();
    harness.handle.request_shutdown();
    assert!(barrier.wait_entered(WAIT), "the native release must park");

    let outcome = harness
        .teardown
        .wait_for_outcome(WAIT)
        .expect("the coordinator stored an outcome");
    assert!(harness.teardown.wait_for_terminal(WAIT));
    assert_eq!(outcome.result, TeardownResult::Uncertain, "{outcome}");
    assert!(
        outcome.unacknowledged.contains(&"module:keepAwake".to_owned()),
        "{outcome}"
    );
    assert!(
        outcome.unacknowledged.contains(&"power-assertion".to_owned()),
        "an in-flight release is never an acknowledgement: {outcome}"
    );
    assert!(outcome.failed.is_empty(), "{outcome}");
    assert!(
        outcome.elapsed <= TEARDOWN_TOTAL + Duration::from_secs(1),
        "the coordinator stays bounded: {outcome}"
    );
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(1)]);
    assert!(!harness.terminal.actions().contains(&TerminalAction::Restart));
    assert!(
        harness.assertion.release_attempts().len() < 3,
        "the blocked pass released nothing"
    );

    assert!(barrier.release());
    assert!(harness.handle.wait_for_exit(WAIT));
}

#[test]
fn a_barrier_hung_observation_removal_is_unacknowledged() {
    let fixture = fixture();
    fixture.write(GOOD);
    let mut harness = Harness::new(&fixture, NativeLockState::Unlocked);
    harness.start(&fixture);
    harness.wait_ready();
    harness.wait_status("active", |status| {
        status.runtime.assertion_state == AssertionState::Active
    });

    let barrier = harness.observation.block_next_remove();
    harness.handle.request_shutdown();

    let outcome = harness
        .teardown
        .wait_for_outcome(WAIT)
        .expect("the coordinator stored an outcome");
    assert!(harness.teardown.wait_for_terminal(WAIT));
    assert_eq!(outcome.result, TeardownResult::Uncertain, "{outcome}");
    // Monitor threads poll in real time and may pass their bound on a loaded
    // machine; the module owner is what this scenario proves.
    let owners: Vec<&str> = outcome
        .unacknowledged
        .iter()
        .map(String::as_str)
        .filter(|owner| !matches!(*owner, "watcher" | "permission-ticker" | "startup-job"))
        .collect();
    assert_eq!(
        owners,
        ["module:keepAwake"],
        "the module owns observation removal: {outcome}"
    );
    assert!(
        !outcome.unacknowledged.contains(&"power-assertion".to_owned()),
        "the assertion was released before observation removal: {outcome}"
    );
    assert!(outcome.failed.is_empty(), "{outcome}");
    assert!(
        outcome.elapsed <= TEARDOWN_TOTAL + Duration::from_secs(1),
        "the coordinator stays bounded: {outcome}"
    );
    assert_eq!(harness.terminal.actions(), vec![TerminalAction::Exit(1)]);
    assert_eq!(harness.observation.remove_calls().len(), 1);

    assert!(barrier.release());
    assert!(harness.handle.wait_for_exit(WAIT));
}

#[test]
fn keep_awake_adds_no_cli_command_or_completion_entry() {
    let command = stache_cli::commands::compose(&stache_app::commands::catalog());
    let mut audio = false;
    for subcommand in command.get_subcommands() {
        let name = subcommand.get_name();
        assert_ne!(name, "keepAwake");
        assert_ne!(name, "keep-awake");
        audio |= name == "audio";
    }
    assert!(audio, "the existing audio command stays contributed");
}
