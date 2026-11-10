//! TI-05-01/03/04/05: the Tiling module over the real registry and fakes.
//!
//! Nothing native: the backend is [`FakeWindowBackend`], reservations come from
//! a scripted source, time is the system clock only for waits, and the
//! "main thread" is an inline executor.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead, WriteStatus};
use stache_macos::windows::{
    CleanupReport, IssuedKind, NativeStatus, SnapshotOrigin, StopOutcome, WindowActionOutcome,
    WindowEventKind, WindowInstance,
};
use stache_macos::{ApplicationId, DisplayControl, FakeDisplayBackend};
use stache_sdk::{
    CancellationToken, ConfigError, DiscardLogger, DisplayId as SdkDisplayId, EventBus, Module,
    ModuleConfig, ModuleContext, ModuleDescriptor, ModuleHandle, ModuleId, ModuleResult,
    ModuleStatus, Permission, PermissionError, PermissionProvider, PermissionStatus, PromptPolicy,
    Registry, ReloadStatus, ScreenReservationSnapshot, ScreenReservationWait, ThreadError,
    TopologyGeneration, UnavailableWindows,
};

use crate::actor::AppDirectory;
use crate::backend::{FakeBackendCall, FakeWindowBackend, WindowBackend};
use crate::config::{LayoutType, TilingConfig};
use crate::geometry::Rect;
use crate::module::{Teardown, TilingParts, TilingRuntime, TilingTiming};
use crate::reservations::{DisplayProjection, ReservationSource};
use crate::state::{AppInfo, Phase};
use crate::status::TilingRuntimeState;
use crate::test_support::{FakeAppDirectory, Feed, app, info, one_display, snapshot};
use crate::tracking::StartStage;

const HOP: Duration = Duration::from_millis(60);

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ----------------------------------------------------------------- fakes

struct Inline;

impl stache_sdk::MainThreadExecutor for Inline {
    fn is_main_thread(&self) -> bool { false }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        job();
        Ok(())
    }
}

struct Trust {
    trusted: AtomicBool,
    requests: AtomicUsize,
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

/// A scripted reservation source that counts reads and live waits.
struct Source {
    state: Mutex<Arc<ScreenReservationSnapshot>>,
    reads: AtomicUsize,
    waiting: AtomicUsize,
}

impl Source {
    fn new(displays: &[u32]) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Arc::new(Self::snapshot(1, displays))),
            reads: AtomicUsize::new(0),
            waiting: AtomicUsize::new(0),
        })
    }

    fn snapshot(revision: u64, displays: &[u32]) -> ScreenReservationSnapshot {
        ScreenReservationSnapshot {
            revision,
            topology: TopologyGeneration::new(1),
            displays: displays
                .iter()
                .map(|id| stache_sdk::DisplayReservations {
                    display: SdkDisplayId::new(*id),
                    since: TopologyGeneration::new(1),
                    insets: stache_sdk::EdgeInsets::new(40.0, 0.0, 0.0, 0.0).expect("insets"),
                    contributions: Vec::new(),
                })
                .collect(),
        }
    }

    fn reads(&self) -> usize { self.reads.load(Ordering::SeqCst) }
}

impl ReservationSource for Source {
    fn read(&self) -> Arc<ScreenReservationSnapshot> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Arc::clone(&lock(&self.state))
    }

    fn wait_for_change(
        &self,
        seen: u64,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> ScreenReservationWait {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let started = Instant::now();
        let outcome = loop {
            if cancel.is_cancelled() {
                break ScreenReservationWait::Cancelled;
            }
            let current = Arc::clone(&lock(&self.state));
            if current.revision != seen {
                break ScreenReservationWait::Changed(current);
            }
            if started.elapsed() >= timeout {
                break ScreenReservationWait::TimedOut;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        outcome
    }
}

/// A directory whose enumeration can be stalled.
struct GatedDirectory {
    inner: FakeAppDirectory,
    open: Mutex<bool>,
    changed: Condvar,
    entered: AtomicUsize,
}

impl GatedDirectory {
    fn new(apps: Vec<AppInfo>, open: bool) -> Arc<Self> {
        let directory = Self {
            inner: FakeAppDirectory::default(),
            open: Mutex::new(open),
            changed: Condvar::new(),
            entered: AtomicUsize::new(0),
        };
        *lock(&directory.inner.apps) = apps;
        Arc::new(directory)
    }

    fn open(&self) {
        *lock(&self.open) = true;
        self.changed.notify_all();
    }
}

impl AppDirectory for GatedDirectory {
    fn running(&self) -> Vec<AppInfo> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let guard = lock(&self.open);
        drop(
            self.changed
                .wait_while(guard, |open| !*open)
                .unwrap_or_else(PoisonError::into_inner),
        );
        self.inner.running()
    }

    fn describe(&self, application: ApplicationId) -> Option<AppInfo> {
        self.inner.describe(application)
    }
}

// ----------------------------------------------------------------- host

/// The shared input service over a fake native owner. No real tap exists.
struct FakeInput {
    backend: Arc<stache_sdk::input::testing::FakeBackend>,
    service: Arc<stache_sdk::InputService>,
}

impl FakeInput {
    fn new() -> Self {
        use stache_sdk::input::testing::{FakeBackend, FakeClock};
        let clock = FakeClock::new();
        let backend = FakeBackend::new(&clock);
        let service = stache_sdk::InputService::with_clock(
            Arc::clone(&backend) as Arc<dyn stache_sdk::input::InputBackend>,
            Arc::new(stache_sdk::input::NoWork),
            Arc::clone(&clock) as Arc<dyn stache_sdk::input::InputClock>,
        );
        Self { backend, service }
    }
}

struct Host {
    input: Option<FakeInput>,
    backend: Arc<FakeWindowBackend>,
    source: Arc<Source>,
    directory: Arc<GatedDirectory>,
    runtime: TilingRuntime,
    registry: Arc<Registry>,
    handle: ModuleHandle<TilingConfig>,
    trust: Arc<Trust>,
    trace: Arc<Mutex<Vec<Teardown>>>,
    projections: Arc<AtomicUsize>,
}

// Independent test switches, not a state machine.
#[allow(clippy::struct_excessive_bools)]
struct Setup {
    trusted: bool,
    directory_open: bool,
    timing: TilingTiming,
    displays: Vec<u32>,
    display: Option<FakeDisplayBackend>,
    applications: Option<Arc<dyn crate::visibility::ApplicationActionsSource>>,
    /// Attach the fake shared input service (the production composition always
    /// has one). `false` leaves the inert placeholder.
    input: bool,
    /// The fake owner refuses to install the listen-only tap (Input Monitoring
    /// missing).
    listen_only_refused: bool,
    /// The `JankyBorders` authority (fakes only).
    borders: Option<crate::borders::BordersParts>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            trusted: true,
            directory_open: true,
            timing: TilingTiming {
                poll: Duration::from_millis(5),
                hop: HOP,
                borders_join: HOP,
                ..TilingTiming::default()
            },
            displays: Vec::new(),
            display: None,
            applications: None,
            input: true,
            listen_only_refused: false,
            borders: None,
        }
    }
}

fn enabled() -> TilingConfig {
    TilingConfig {
        enabled: true,
        ..TilingConfig::default()
    }
}

fn workspace(name: &str) -> crate::config::WorkspaceConfig {
    crate::config::WorkspaceConfig {
        name: name.to_owned(),
        layout: None,
        screen: "main".to_owned(),
        rules: Vec::new(),
        preset_on_open: None,
    }
}

fn with_layout(layout: LayoutType) -> TilingConfig {
    TilingConfig {
        default_layout: layout,
        ..enabled()
    }
}

impl Host {
    fn new() -> Self { Self::with(Setup::default()) }

    fn with(setup: Setup) -> Self {
        let backend = Arc::new(FakeWindowBackend::default());
        let source = Source::new(&setup.displays);
        let directory = GatedDirectory::new(vec![info(1, "com.a", "A")], setup.directory_open);
        let trace = Arc::new(Mutex::new(Vec::new()));
        let projections = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&projections);
        let mut parts = TilingParts::new(
            Arc::clone(&backend) as Arc<dyn WindowBackend>,
            Arc::clone(&directory) as Arc<dyn AppDirectory>,
            DisplayProjection::new(move |native| {
                counted.fetch_add(1, Ordering::SeqCst);
                SdkDisplayId::new(native.raw())
            }),
            "",
        );
        parts.reservations = Some(Arc::clone(&source) as Arc<dyn ReservationSource>);
        parts.applications = setup.applications.clone();
        parts.borders = setup.borders.clone();
        parts.displays = setup
            .display
            .clone()
            .map(|fake| DisplayControl::new(Arc::new(Inline), Arc::new(fake)));
        let recorder = Arc::clone(&trace);
        parts.timing = setup.timing;
        parts.timing.trace = Some(Arc::new(move |step| lock(&recorder).push(step)));
        let runtime = TilingRuntime::new(parts);
        let trust = Arc::new(Trust {
            trusted: AtomicBool::new(setup.trusted),
            requests: AtomicUsize::new(0),
        });
        let input = setup.input.then(FakeInput::new);
        if let (Some(input), true) = (&input, setup.listen_only_refused) {
            input.backend.script(
                stache_sdk::input::TapKind::ListenOnly,
                stache_sdk::input::ControlAction::Install,
                stache_sdk::input::testing::FakeResponse::Fail(
                    "input monitoring denied".to_owned(),
                ),
            );
        }
        let mut registry = Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(Inline),
            Arc::clone(&trust) as Arc<dyn PermissionProvider>,
            Arc::new(UnavailableWindows),
        );
        if let Some(input) = &input {
            registry = registry.with_input_service(Arc::clone(&input.service));
        }
        let registry = Arc::new(registry);
        let handle = registry.register(runtime.module(), enabled()).expect("register");
        runtime.install_accepted_watch(registry.accepted_watch(handle).expect("watch"));
        Self {
            input,
            backend,
            source,
            directory,
            runtime,
            registry,
            handle,
            trust,
            trace,
            projections,
        }
    }

    fn started() -> Self {
        let host = Self::new();
        host.registry.start_all().expect("start");
        host
    }

    fn status(&self) -> ModuleStatus {
        self.registry.snapshot(self.handle).expect("snapshot").status
    }

    fn revision(&self) -> u64 { self.registry.snapshot(self.handle).expect("snapshot").revision }

    fn runtime_revision(&self) -> String { self.runtime.status().read().config_revision }

    fn calls(&self, wanted: fn(&FakeBackendCall) -> bool) -> usize {
        self.backend.calls().iter().filter(|call| wanted(call)).count()
    }

    fn installs(&self) -> usize { self.calls(|c| matches!(c, FakeBackendCall::Install)) }

    fn stops(&self) -> usize { self.calls(|c| matches!(c, FakeBackendCall::Stop(_))) }

    fn trace(&self) -> Vec<Teardown> { lock(&self.trace).clone() }

    fn state(&self) -> TilingRuntimeState { self.runtime.status().read().state }

    fn reload(&self, config: TilingConfig) -> stache_sdk::RegistryResult<stache_sdk::ReloadReport> {
        self.registry.reload(self.handle, config)
    }

    /// Stops the module (the registry calls `Module::stop`).
    fn disable(&self) { let _ = self.registry.set_enabled(self.handle, false); }

    fn enable(&self) { let _ = self.registry.set_enabled(self.handle, true); }
}

const TARGET: Rect = Rect::new(0.0, 25.0, 1920.0, 1055.0);

fn applied() -> WindowActionOutcome {
    WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        native: NativeStatus::Frame {
            position: WriteStatus::Written,
            size: WriteStatus::Written,
        },
        observed: Some(FrameRead {
            position: AxReading::Value(AxPoint { x: TARGET.x, y: TARGET.y }),
            size: AxReading::Value(AxSize {
                width: TARGET.width,
                height: TARGET.height,
            }),
        }),
    }
}

/// Feeds one tracked window to a started host and waits for the phase to
/// complete. Returns the window.
fn feed_window(host: &Host) -> WindowInstance {
    let runtime = host.runtime.tracking().expect("running");
    let mut feed = Feed::new(runtime.epoch().value());
    feed.adapter = host.backend.adapter();
    runtime.submit_displays(None, one_display()).expect("displays");
    wait_until("tracked", || {
        host.backend
            .calls()
            .iter()
            .any(|call| matches!(call, FakeBackendCall::Track(..)))
    });
    let sink = host.backend.sink().expect("sink");
    let token = feed.token(1, 1);
    assert!(sink.push(feed.event(WindowEventKind::Snapshot {
        snapshot: snapshot(token, "w", 1),
        origin: SnapshotOrigin::Created,
    })));
    assert!(sink.push(feed.event(WindowEventKind::EnumerationComplete {
        application: app(1),
        windows: 1,
        truncated: false,
    })));
    wait_until("complete", || runtime.snapshot().phase == Phase::Complete);
    token
}

/// Drives one window to a held native send on a started host.
fn hold_one_send(host: &Host) -> Arc<crate::backend::SendHold> {
    let hold = host.backend.hold_sends();
    host.backend.script_action(applied());
    feed_window(host);
    assert!(hold.wait_entered(1, Duration::from_secs(10)), "a send is held");
    hold
}

// ------------------------------------------------------------- TI-05-01

#[test]
fn construction_and_registration_are_inert() {
    let host = Host::new();
    assert!(host.backend.calls().is_empty(), "no native call before start");
    assert_eq!(host.source.reads(), 0);
    assert_eq!(host.projections.load(Ordering::SeqCst), 0);
    assert_eq!(host.state(), TilingRuntimeState::Inactive);
    assert!(host.runtime.tracking().is_none());
    assert_eq!(host.status(), ModuleStatus::Stopped);
}

#[test]
fn start_returns_running_before_the_enumeration_completes() {
    let host = Host::with(Setup {
        directory_open: false,
        ..Setup::default()
    });
    host.registry.start_all().expect("start");
    assert_eq!(host.status(), ModuleStatus::Running);
    assert_eq!(host.state(), TilingRuntimeState::Running);
    assert_eq!(host.installs(), 1);
    // Enumeration is queued and stalled inside the directory; start did not wait.
    wait_until("enumeration entered", || {
        host.directory.entered.load(Ordering::SeqCst) == 1
    });
    assert_eq!(host.calls(|c| matches!(c, FakeBackendCall::Track(..))), 0);
    assert!(host.runtime.tracking().expect("running").is_authorized());
    host.directory.open();
    wait_until("tracked", || {
        host.calls(|c| matches!(c, FakeBackendCall::Track(..))) == 1
    });
    host.disable();
}

#[test]
fn every_start_stage_failure_unwinds_exactly() {
    for stage in [
        StartStage::Install,
        StartStage::Runner,
        StartStage::Actor,
        StartStage::Watcher,
        StartStage::Displays,
        StartStage::Enumeration,
        StartStage::Authorize,
    ] {
        let fail = Arc::new(AtomicBool::new(true));
        let armed = Arc::clone(&fail);
        let fake = FakeDisplayBackend::new(one_display());
        let mut setup = Setup {
            display: Some(fake.clone()),
            ..Setup::default()
        };
        setup.timing.probe = Some(Arc::new(move |at| at == stage && armed.load(Ordering::SeqCst)));
        let host = Host::with(setup);
        let _ = host.registry.start_all();
        assert!(
            matches!(host.status(), ModuleStatus::Failed(_)),
            "{stage:?}: {:?}",
            host.status()
        );
        // Exact rollback: every install is matched by a stop; nothing is owed.
        assert_eq!(host.installs(), host.stops(), "{stage:?}: installs match stops");
        assert_eq!(host.state(), TilingRuntimeState::Inactive, "{stage:?}");
        assert!(host.runtime.tracking().is_none(), "{stage:?}");
        wait_until("watcher gone", || {
            host.source.waiting.load(Ordering::SeqCst) == 0
        });
        assert_eq!(fake.observers(), 0, "{stage:?}: the display observer is removed");
        let installed = stage != StartStage::Install;
        assert_eq!(host.installs(), usize::from(installed), "{stage:?}");
        // A restart after the fault is cleared succeeds from a clean slate.
        fail.store(false, Ordering::SeqCst);
        host.reload(with_layout(LayoutType::Floating)).expect("reload restarts");
        assert_eq!(host.status(), ModuleStatus::Running, "{stage:?}");
        assert_eq!(host.state(), TilingRuntimeState::Running);
        assert_eq!(fake.observers(), 1, "{stage:?}: the restart observes again");
        host.disable();
        assert_eq!(fake.observers(), 0, "{stage:?}: stop removes the observer");
        assert_eq!(host.installs(), host.stops(), "{stage:?}: final");
    }
}

#[test]
fn a_backend_refusal_installs_nothing() {
    let host = Host::new();
    host.backend
        .script_install_error(stache_macos::windows::InstallError::Quarantined {
            applications: 1,
            registrations: 1,
        });
    let _ = host.registry.start_all();
    assert!(matches!(host.status(), ModuleStatus::Failed(_)));
    assert_eq!(host.stops(), 0);
    assert_eq!(
        host.source.reads(),
        0,
        "reservations are not touched before the epoch"
    );
    assert_eq!(host.state(), TilingRuntimeState::Inactive);
}

#[test]
fn missing_accessibility_waits_without_acquiring_or_prompting() {
    let host = Host::with(Setup {
        trusted: false,
        ..Setup::default()
    });
    host.registry.start_all().expect("start");
    assert_eq!(
        host.status(),
        ModuleStatus::WaitingForPermission(Permission::Accessibility)
    );
    assert!(host.backend.calls().is_empty());
    assert_eq!(host.source.reads(), 0);
    assert_eq!(
        host.trust.requests.load(Ordering::SeqCst),
        0,
        "startup only checks"
    );
    let module = host.runtime.module();
    let descriptor: ModuleDescriptor = module.descriptor();
    assert_eq!(descriptor.permissions, &[Permission::Accessibility]);
    assert!(
        !descriptor.reserves_screen,
        "Tiling never owns host reservations"
    );
    let requirements = module.required_permissions(&enabled());
    assert_eq!(requirements.len(), 1);
    assert_eq!(requirements[0].permission(), Permission::Accessibility);
    assert_eq!(requirements[0].prompt(), PromptPolicy::UserOnly);
}

#[test]
fn the_status_projects_the_actors_own_revision() {
    let host = Host::started();
    let status = host.runtime.status().read();
    assert_eq!(status.state, TilingRuntimeState::Running);
    assert!(status.healthy);
    wait_until("the actor adopted the revision", || {
        host.runtime_revision() == host.revision().to_string()
    });
    host.disable();
    let status = host.runtime.status().read();
    assert_eq!(status.state, TilingRuntimeState::Inactive);
    assert!(status.owed.is_empty());
}

// ------------------------------------------------------------- TI-05-03

#[test]
fn staged_resources_flip_only_after_the_same_revision_is_observed() {
    let (entered_tx, entered) = std::sync::mpsc::channel::<u64>();
    let (release, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let armed = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&armed);
    let staged_calls = Arc::new(AtomicUsize::new(0));
    let staged = Arc::clone(&staged_calls);
    let mut setup = Setup::default();
    setup.timing.observed = Some(Arc::new(move |revision| {
        if gate.load(Ordering::SeqCst) {
            let _ = entered_tx.send(revision);
            let _ = release_rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
        }
    }));
    setup.timing.stage = Some(Arc::new(move |_| {
        staged.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    let host = Host::with(setup);
    host.registry.start_all().expect("start");
    wait_until("initial adoption", || {
        host.runtime_revision() == host.revision().to_string()
    });
    let before = host.revision();

    armed.store(true, Ordering::SeqCst);
    let report = host.reload(with_layout(LayoutType::Floating)).expect("reload");
    let ReloadStatus::Applied { revision } = report.status else {
        panic!("applied")
    };
    assert_eq!(revision, before + 1);
    assert_eq!(
        staged_calls.load(Ordering::SeqCst),
        1,
        "the candidate was staged once"
    );
    // The coordinator observed exactly the published revision and is held
    // before the flip: the runtime still runs the old revision.
    assert_eq!(
        entered.recv_timeout(Duration::from_secs(10)).expect("observed"),
        revision
    );
    assert_eq!(
        host.runtime_revision(),
        before.to_string(),
        "nothing flipped before the flip"
    );
    release.send(()).expect("release");
    wait_until("flip", || host.runtime_revision() == revision.to_string());
    armed.store(false, Ordering::SeqCst);
    host.disable();
}

#[test]
fn a_rejected_candidate_keeps_the_runtime_and_coherent_revisions() {
    let mut setup = Setup::default();
    setup.timing.stage = Some(Arc::new(|config| {
        if config.default_layout == LayoutType::Grid {
            Err("grid is refused".to_owned())
        } else {
            Ok(())
        }
    }));
    let host = Host::with(setup);
    host.registry.start_all().expect("start");
    wait_until("adopted", || {
        host.runtime_revision() == host.revision().to_string()
    });
    let revision = host.revision();
    let tracking = host.runtime.tracking().expect("running");
    let epoch = tracking.epoch();

    assert!(
        host.reload(with_layout(LayoutType::Grid)).is_err(),
        "the stage refused"
    );
    assert_eq!(host.revision(), revision, "no revision was published");
    let watched = host.registry.accepted_watch(host.handle).expect("watch").read().expect("pair");
    assert_eq!(watched.revision, revision);
    assert_eq!(watched.config.default_layout, enabled().default_layout);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(
        host.runtime_revision(),
        revision.to_string(),
        "runtime untouched"
    );
    assert_eq!(host.status(), ModuleStatus::Running);
    assert!(tracking.is_authorized(), "authority unchanged");
    assert_eq!(tracking.epoch(), epoch);
    assert_eq!(host.installs(), 1, "no restart happened");

    // A later valid candidate is still adopted.
    host.reload(with_layout(LayoutType::Floating)).expect("reload");
    wait_until("adopted", || {
        host.runtime_revision() == host.revision().to_string()
    });
    host.disable();
}

#[test]
fn a_structural_reload_is_prepared_against_the_live_state_before_acceptance() {
    let host = Host::started();
    wait_until("adopted", || {
        host.runtime_revision() == host.revision().to_string()
    });
    let structural = |names: &[&str]| TilingConfig {
        workspaces: names.iter().map(|name| workspace(name)).collect(),
        ..enabled()
    };
    let before = host.revision();
    host.reload(structural(&["a", "b"])).expect("add workspaces");
    host.reload(structural(&["b"])).expect("remove a workspace");
    wait_until("adopted", || {
        host.runtime_revision() == host.revision().to_string()
    });
    assert_eq!(host.revision(), before + 2);
    assert_eq!(host.status(), ModuleStatus::Running);
    host.disable();
}

#[test]
fn a_b_a_revisions_are_distinct_and_each_is_observed() {
    let host = Host::started();
    wait_until("adopted", || {
        host.runtime_revision() == host.revision().to_string()
    });
    let first = host.revision();
    host.reload(with_layout(LayoutType::Floating)).expect("b");
    wait_until("b", || host.runtime_revision() == (first + 1).to_string());
    host.reload(enabled()).expect("a again");
    wait_until("a again", || host.runtime_revision() == (first + 2).to_string());
    assert_eq!(host.revision(), first + 2, "the same value is a new revision");
    host.disable();
}

#[test]
fn an_invalid_reload_is_held_and_repaired_independently_of_cleanup() {
    let host = Host::started();
    let revision = host.revision();
    let invalid = TilingConfig {
        workspaces: vec![workspace("a"), workspace("a")],
        ..enabled()
    };
    assert!(host.reload(invalid).is_err());
    let snapshot = host.registry.snapshot(host.handle).expect("snapshot");
    assert!(snapshot.diagnostic.is_some());
    assert_eq!(snapshot.revision, revision);
    assert_eq!(
        host.status(),
        ModuleStatus::Running,
        "the running module is not disguised"
    );
    host.reload(with_layout(LayoutType::Floating)).expect("repair");
    assert!(host.registry.snapshot(host.handle).expect("snapshot").diagnostic.is_none());
    host.disable();
}

// ------------------------------------------------------------- TA-04-03

#[test]
fn the_lease_restore_follows_the_drain_and_precedes_the_hidden_restore() {
    let host = Host::started();
    host.disable();
    let trace = host.trace();
    let position = |step| trace.iter().position(|s| *s == step).expect("traced");
    assert!(position(Teardown::Drained) < position(Teardown::Animation));
    assert!(position(Teardown::Animation) < position(Teardown::EnhancedUi));
    assert!(position(Teardown::EnhancedUi) < position(Teardown::Restore));
    assert!(position(Teardown::Restore) < position(Teardown::Epoch));
    assert!(
        host.backend.calls().contains(&FakeBackendCall::RestoreLeases),
        "the backend was asked to restore its leases"
    );
}

#[test]
fn a_failed_lease_restore_stays_owed_and_the_retry_restores_it() {
    let host = Host::started();
    host.backend
        .script_lease_restore(stache_macos::windows::LeaseRestoreOutcome::Owed {
            applications: vec![crate::test_support::app(1)],
        });
    host.disable();
    let status = host.runtime.status().read();
    assert_eq!(status.state, TilingRuntimeState::Quarantined, "{status:?}");
    assert_eq!(status.owed, ["enhanced ui restore"], "{status:?}");
    assert!(
        status
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("AXEnhancedUserInterface")),
        "{status:?}"
    );
    assert!(!host.trace().contains(&Teardown::EnhancedUi));
    // The rest of the stop ran: the visibility restore does not wait for it.
    assert!(host.trace().contains(&Teardown::Restore));
    assert!(host.trace().contains(&Teardown::Epoch));

    host.enable();
    assert_eq!(
        host.status(),
        ModuleStatus::Running,
        "{:?}",
        host.runtime.status().read()
    );
    assert_eq!(
        host.trace().iter().filter(|s| **s == Teardown::EnhancedUi).count(),
        1,
        "the lease restore completes once"
    );
    assert_eq!(host.stops(), 1, "the epoch was not stopped twice");
    host.disable();
}

// ------------------------------------------------------------- TI-05-04

#[test]
fn a_clean_stop_drains_before_restore_before_observer_removal() {
    let host = Host::started();
    host.disable();
    assert_eq!(host.status(), ModuleStatus::Disabled);
    assert_eq!(host.trace(), [
        Teardown::Sealed,
        Teardown::Drag,
        Teardown::Drained,
        Teardown::Animation,
        Teardown::EnhancedUi,
        Teardown::Restore,
        Teardown::Watcher,
        Teardown::Epoch,
        Teardown::Actor,
        Teardown::Runner,
    ]);
    assert_eq!(host.stops(), 1);
    assert_eq!(host.state(), TilingRuntimeState::Inactive);
}

#[test]
fn stop_seals_while_a_send_is_held_then_quarantines_truthfully() {
    let host = Host::started();
    let hold = hold_one_send(&host);
    let tracking = host.runtime.tracking().expect("running");
    assert!(tracking.is_authorized());

    let started = Instant::now();
    host.disable();
    assert!(started.elapsed() < Duration::from_secs(5), "stop is finite");
    assert!(!tracking.is_authorized(), "sealed");
    let status = host.runtime.status().read();
    assert_eq!(status.state, TilingRuntimeState::Quarantined);
    assert!(status.owed.contains(&"frame drain".to_owned()), "{status:?}");
    assert!(status.owed.contains(&"restore".to_owned()), "{status:?}");
    assert!(
        status.owed.contains(&"enhanced ui restore".to_owned()),
        "no lease is restored while a send could still take one: {status:?}"
    );
    assert!(!host.trace().contains(&Teardown::EnhancedUi));
    assert!(
        !status.owed.contains(&"effect worker".to_owned()),
        "frame writes are asynchronous: the effect worker is not stuck: {status:?}"
    );
    assert!(!status.owed.contains(&"backend epoch".to_owned()));
    assert!(matches!(host.status(), ModuleStatus::Failed(_)));
    // Restore never ran while a send could still move a window.
    assert!(!host.trace().contains(&Teardown::Restore));
    assert!(host.runtime.tracking().is_none());

    // No restart while quarantined: the registry retries the cleanup before it
    // would start, and while it is incomplete the start is never reached.
    host.enable();
    let ModuleStatus::Failed(failure) = host.status() else {
        panic!("failed")
    };
    assert!(
        failure.message.contains("cleanup is incomplete"),
        "{}",
        failure.message
    );
    assert_eq!(host.state(), TilingRuntimeState::Quarantined);
    assert_eq!(host.installs(), 1, "nothing was installed over the quarantine");

    // The old send completes late; the retry runs only the owed stages and the
    // restart proceeds from a clean slate.
    hold.release();
    host.enable();
    assert_eq!(
        host.status(),
        ModuleStatus::Running,
        "{:?}",
        host.runtime.status().read()
    );
    assert_eq!(host.stops(), 1, "the epoch was not stopped twice");
    let trace = host.trace();
    let restore = trace.iter().position(|s| *s == Teardown::Restore).expect("restore ran");
    let drained = trace.iter().position(|s| *s == Teardown::Drained).expect("drained");
    assert!(drained < restore);
    for step in [
        Teardown::Watcher,
        Teardown::Epoch,
        Teardown::Actor,
        Teardown::Runner,
    ] {
        assert_eq!(
            trace.iter().filter(|s| **s == step).count(),
            1,
            "{step:?} ran once"
        );
    }

    // A successor is not affected by the late old completion.
    let successor = host.runtime.tracking().expect("successor");
    assert_ne!(successor.stamp(), tracking.stamp());
    assert_eq!(successor.outstanding_frames(), 0);
    std::thread::sleep(Duration::from_millis(30));
    let snap = successor.snapshot();
    assert!(
        snap.windows.is_empty() && snap.frames.is_empty(),
        "no old state leaked"
    );
    host.disable();
}

#[test]
fn an_incomplete_epoch_stop_is_retried_without_repeating_other_stages() {
    let host = Host::started();
    host.backend.script_stop(StopOutcome::Quarantined {
        applications: 1,
        registrations: 2,
    });
    host.disable();
    let status = host.runtime.status().read();
    assert_eq!(status.state, TilingRuntimeState::Quarantined);
    assert_eq!(status.owed, ["backend epoch"]);
    // The restart retries only the owed backend removal, then starts again.
    host.enable();
    assert_eq!(
        host.status(),
        ModuleStatus::Running,
        "{:?}",
        host.runtime.status().read()
    );
    assert_eq!(host.stops(), 1);
    assert_eq!(host.calls(|c| matches!(c, FakeBackendCall::Retry)), 1);
    assert_eq!(host.installs(), 2);
    let trace = host.trace();
    assert_eq!(trace.iter().filter(|s| **s == Teardown::Drained).count(), 1);
    assert_eq!(trace.iter().filter(|s| **s == Teardown::Watcher).count(), 1);
    assert_eq!(trace.iter().filter(|s| **s == Teardown::Epoch).count(), 1);
    host.disable();
}

#[test]
fn a_failed_start_with_an_incomplete_unwind_is_quarantined_and_retried() {
    let fail = Arc::new(AtomicBool::new(true));
    let armed = Arc::clone(&fail);
    let mut setup = Setup::default();
    setup.timing.probe = Some(Arc::new(move |at| {
        at == StartStage::Actor && armed.load(Ordering::SeqCst)
    }));
    let host = Host::with(setup);
    host.backend.script_stop(StopOutcome::Quarantined {
        applications: 1,
        registrations: 1,
    });
    // The failed start's own retry and the registry's cleanup-after-failure
    // retry both find the registrations still pending.
    for _ in 0..2 {
        host.backend.script_retry(Some(CleanupReport {
            cleaned_applications: 0,
            remaining_applications: 1,
            remaining_registrations: 1,
        }));
    }
    let _ = host.registry.start_all();
    let status = host.runtime.status().read();
    assert_eq!(status.state, TilingRuntimeState::Quarantined, "{status:?}");
    assert_eq!(status.owed, ["backend epoch"]);
    assert!(host.runtime.tracking().is_none());
    fail.store(false, Ordering::SeqCst);
    host.enable();
    assert_eq!(
        host.status(),
        ModuleStatus::Running,
        "{:?}",
        host.runtime.status().read()
    );
    assert_eq!(host.calls(|c| matches!(c, FakeBackendCall::Retry)), 3);
    host.disable();
}

#[test]
fn an_unacknowledged_epoch_stop_is_stopped_again_on_retry() {
    let host = Host::started();
    host.backend.script_stop(StopOutcome::Unacknowledged);
    host.disable();
    assert_eq!(host.state(), TilingRuntimeState::Quarantined);
    host.disable();
    assert_eq!(host.state(), TilingRuntimeState::Inactive);
    assert_eq!(
        host.stops(),
        2,
        "an unacknowledged stop is re-issued, not assumed"
    );
}

#[test]
fn waits_wake_before_joins() {
    // A long coordinator poll and a long reservation wait must not delay a stop.
    let mut setup = Setup::default();
    setup.timing.poll = Duration::from_secs(60);
    setup.timing.hop = Duration::from_secs(2);
    let host = Host::with(setup);
    host.registry.start_all().expect("start");
    let started = Instant::now();
    host.disable();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(host.state(), TilingRuntimeState::Inactive);
    assert_eq!(host.source.waiting.load(Ordering::SeqCst), 0);
}

// ------------------------------------------------------------- TI-05-05

#[test]
fn reservations_and_projection_are_consumed_before_the_first_authorize() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let host_cell: Arc<Mutex<Option<Arc<Source>>>> = Arc::new(Mutex::new(None));
    let probe_cell = Arc::clone(&host_cell);
    let mut setup = Setup {
        displays: vec![7],
        ..Setup::default()
    };
    setup.timing.probe = Some(Arc::new(move |stage| {
        if stage == StartStage::Authorize
            && let Some(source) = lock(&probe_cell).as_ref()
        {
            lock(&log).push(source.reads());
        }
        false
    }));
    let host = Host::with(setup);
    *lock(&host_cell) = Some(Arc::clone(&host.source));
    host.registry.start_all().expect("start");
    let observed = lock(&seen).clone();
    assert_eq!(observed.len(), 1);
    assert!(observed[0] >= 1, "the snapshot was read before authorize");
    let tracking = host.runtime.tracking().expect("running");
    wait_until("reservations reached the actor", || {
        tracking.snapshot().host_revision == 1
    });
    host.disable();
}

struct OtherConfig;

impl Clone for OtherConfig {
    fn clone(&self) -> Self { Self }
}

impl PartialEq for OtherConfig {
    fn eq(&self, _: &Self) -> bool { true }
}

impl ModuleConfig for OtherConfig {
    fn enabled(&self) -> bool { true }

    fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
}

struct Other;

impl Module for Other {
    type Config = OtherConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: ModuleId::new("independent"),
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
            observes: &[],
            reserves_screen: false,
        }
    }

    fn configure(&mut self, _: &OtherConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

    fn start(&mut self, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

    fn reconfigure(&mut self, _: &OtherConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

    fn stop(&mut self, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }
}

#[test]
fn a_stopped_or_quarantined_tiling_does_not_affect_independent_modules() {
    let host = Host::new();
    let other = host.registry.register(Other, OtherConfig).expect("other");
    host.registry.start_all().expect("start");
    let status = |h| host.registry.snapshot(h).expect("snapshot").status;
    assert_eq!(status(other), ModuleStatus::Running);
    let hold = hold_one_send(&host);
    host.disable();
    assert_eq!(host.state(), TilingRuntimeState::Quarantined);
    assert_eq!(
        status(other),
        ModuleStatus::Running,
        "the other module keeps running"
    );
    hold.release();
    host.disable();
    assert_eq!(status(other), ModuleStatus::Running);
}

// ------------------------------------------------------------- TI-06-02/03

mod visibility {
    use stache_macos::VisibilityOperation;

    use super::*;
    use crate::visibility::Ownership;
    use crate::visibility_tests::{FixtureSource, Native};

    struct Rig {
        host: Host,
        native: Arc<Native>,
        source: Arc<FixtureSource>,
    }

    fn rig() -> Rig {
        let native = Arc::new(Native::new(Duration::from_millis(500)));
        let source = FixtureSource::new(&native, &["com.a", "com.b"]);
        let host = Host::with(Setup {
            applications: Some(
                Arc::clone(&source) as Arc<dyn crate::visibility::ApplicationActionsSource>
            ),
            ..Setup::default()
        });
        host.registry.start_all().expect("start");
        // The actor must know the instances, or it would clear their records
        // as gone: launch them through the lifecycle input.
        let runtime = host.runtime.tracking().expect("running");
        for (n, id) in source.ids().into_iter().enumerate() {
            runtime
                .ingress()
                .submit(crate::state::Input::Lifecycle {
                    kind: stache_macos::applications::LifecycleKind::Launched,
                    info: AppInfo {
                        application: id,
                        pid: 200 + i32::try_from(n).unwrap_or_default(),
                        bundle_id: format!("com.fixture.{n}"),
                        name: format!("Fixture {n}"),
                    },
                })
                .expect("launch");
        }
        wait_until("fixture applications tracked", || {
            let tracked = host
                .backend
                .calls()
                .iter()
                .filter(|call| matches!(call, FakeBackendCall::Track(application, _) if source.ids().contains(application)))
                .count();
            tracked == source.ids().len()
        });
        Rig { host, native, source }
    }

    impl Rig {
        fn control(&self) -> Arc<crate::visibility::VisibilityControl> {
            Arc::clone(self.host.runtime.tracking().expect("running").visibility())
        }
    }

    #[test]
    fn the_started_module_watches_application_lifecycle_and_activation() {
        let rig = rig();
        let keys = rig.source.keys();
        let ids = rig.source.ids();
        let runtime = rig.host.runtime.tracking().expect("running");
        assert!(
            rig.native
                .backend
                .deliver_lifecycle(stache_macos::applications::LifecycleKind::Hidden, keys[0])
                > 0,
            "the lifecycle watch is installed by the module"
        );
        wait_until("hidden observed", || {
            runtime.snapshot().apps.iter().any(|a| a.info.application == ids[0] && a.hidden)
        });
        assert!(
            rig.native.backend.activate(keys[1]) > 0,
            "the activation watch is installed"
        );
    }

    #[test]
    fn stop_restores_owned_hides_after_the_drain_and_before_observer_removal() {
        let rig = rig();
        let a = rig.source.ids()[0];
        let keys = rig.source.keys();
        // A is ours (hidden by the runtime); B is not in the ledger at all.
        rig.control().seed(a, Ownership::Confirmed);
        rig.host.disable();
        assert_eq!(rig.host.trace(), [
            Teardown::Sealed,
            Teardown::Drag,
            Teardown::Drained,
            Teardown::Animation,
            Teardown::EnhancedUi,
            Teardown::Restore,
            Teardown::Watcher,
            Teardown::Epoch,
            Teardown::Actor,
            Teardown::Runner,
        ]);
        assert_eq!(rig.native.backend.visibility_requests(), vec![(
            keys[0],
            VisibilityOperation::Unhide
        )]);
        assert_eq!(rig.host.state(), TilingRuntimeState::Inactive);
        assert!(rig.host.runtime.ambiguous_visibility().is_empty());
    }

    #[test]
    fn a_refused_restore_quarantines_and_the_retry_restores_only_what_remains() {
        let rig = rig();
        let ids = rig.source.ids();
        let keys = rig.source.keys();
        let control = rig.control();
        control.seed(ids[0], Ownership::Confirmed);
        control.seed(ids[1], Ownership::PendingPossible);
        rig.native.backend.refuse_visibility();
        rig.host.disable();
        let status = rig.host.runtime.status().read();
        assert_eq!(status.state, TilingRuntimeState::Quarantined, "{status:?}");
        assert_eq!(status.owed, ["restore"], "{status:?}");
        let detail = status.detail.expect("detail");
        assert!(detail.contains("still owes an unhide"), "{detail}");
        assert!(
            !rig.host.trace().contains(&Teardown::Restore),
            "restore did not finish"
        );
        // The rest of the stop did run, in order, after the failed restore.
        assert!(rig.host.trace().contains(&Teardown::Epoch));
        let attempts = rig.native.backend.visibility_requests().len();
        assert_eq!(attempts, 2, "both were tried once");
        // Only the instance that was restored leaves the ledger; make the
        // second attempt succeed and watch exactly the remaining ones retry.
        rig.native.backend.allow_visibility();
        rig.host.enable();
        let after = rig.native.backend.visibility_requests();
        assert!(after.len() >= 4, "{after:?}");
        assert_eq!(
            after
                .iter()
                .filter(|(key, op)| *key == keys[0] && *op == VisibilityOperation::Unhide)
                .count(),
            2,
            "retried once more"
        );
        assert_eq!(
            rig.host.trace().iter().filter(|s| **s == Teardown::Restore).count(),
            1,
            "restore completes once"
        );
    }

    #[test]
    fn ambiguous_records_are_retained_reported_and_never_unhidden() {
        let rig = rig();
        rig.control().seed(rig.source.ids()[0], Ownership::Ambiguous);
        rig.host.disable();
        assert_eq!(
            rig.host.state(),
            TilingRuntimeState::Inactive,
            "ambiguity never wedges the stop"
        );
        assert!(
            rig.native.backend.visibility_requests().is_empty(),
            "never unhidden"
        );
        assert_eq!(rig.host.runtime.ambiguous_visibility().len(), 1);
        let detail = rig.host.runtime.status().read().detail.expect("reported in status");
        assert!(detail.contains("explicit approval"), "{detail}");
    }

    #[test]
    fn an_ambiguous_record_survives_stop_start_stop_and_stays_reported() {
        let rig = rig();
        rig.control().seed(rig.source.ids()[0], Ownership::Ambiguous);
        rig.host.disable();
        assert_eq!(rig.host.runtime.ambiguous_visibility().len(), 1);
        let first = rig.host.runtime.status().read().ambiguous_visibility;
        assert_eq!(first.len(), 1, "{first:?}");
        // A new run must not forget it, and must report it while Running.
        rig.host.enable();
        assert_eq!(rig.host.state(), TilingRuntimeState::Running);
        assert_eq!(
            rig.host.runtime.ambiguous_visibility().len(),
            1,
            "kept across start"
        );
        let running = rig.host.runtime.status().read();
        assert_eq!(running.state, TilingRuntimeState::Running);
        assert!(
            running.ambiguous_visibility.iter().any(|line| first.contains(line)),
            "still reported while running: {running:?}"
        );
        rig.host.disable();
        assert_eq!(
            rig.host.runtime.ambiguous_visibility().len(),
            1,
            "kept across the second stop"
        );
        assert_eq!(rig.host.runtime.status().read().ambiguous_visibility.len(), 1);
        assert!(
            rig.native.backend.visibility_requests().is_empty(),
            "a retained record is never seeded as owned or unhidden"
        );
    }

    #[test]
    fn a_terminated_instance_clears_its_retained_record() {
        let rig = rig();
        rig.control().seed(rig.source.ids()[0], Ownership::Ambiguous);
        rig.host.disable();
        assert_eq!(rig.host.runtime.ambiguous_visibility().len(), 1);
        rig.host.enable();
        assert_eq!(rig.host.runtime.ambiguous_visibility().len(), 1);
        rig.native.backend.terminate(rig.source.keys()[0]);
        rig.host.disable();
        assert!(
            rig.host.runtime.ambiguous_visibility().is_empty(),
            "terminal: proven gone"
        );
        assert!(rig.host.runtime.status().read().ambiguous_visibility.is_empty());
    }

    #[test]
    fn a_stopped_runtime_is_not_kept_alive_by_the_invalidation_listener() {
        let rig = rig();
        let weak = Arc::downgrade(&rig.host.runtime.tracking().expect("running"));
        rig.host.disable();
        assert!(weak.upgrade().is_none(), "the stopped runtime was dropped");
    }
}

// ------------------------------------------------------------- TI-06-04

mod production_surface {
    use stache_macos::ax::AxError;

    use super::*;
    use crate::test_support::display;

    fn fed() -> (FakeDisplayBackend, Setup) {
        let fake = FakeDisplayBackend::new(one_display());
        let setup = Setup {
            display: Some(fake.clone()),
            ..Setup::default()
        };
        (fake, setup)
    }

    #[test]
    fn the_initial_layout_reaches_the_actor_before_the_authority_is_published() {
        let (fake, mut setup) = fed();
        let reads = Arc::new(AtomicUsize::new(usize::MAX));
        let seen = Arc::clone(&reads);
        let probe_fake = fake.clone();
        setup.timing.probe = Some(Arc::new(move |stage| {
            if stage == StartStage::Authorize {
                seen.store(probe_fake.layout_reads(), Ordering::SeqCst);
            }
            false
        }));
        let host = Host::with(setup);
        host.registry.start_all().expect("start");
        assert!(
            reads.load(Ordering::SeqCst) >= 1,
            "the layout was read before authorize"
        );
        assert_eq!(fake.observers(), 1);
        let tracking = host.runtime.tracking().expect("running");
        wait_until("the native display reached the actor", || {
            tracking.snapshot().screens.len() == 1 && !tracking.snapshot().no_screens
        });
        host.disable();
    }

    #[test]
    fn a_display_change_is_queried_off_the_callback_and_submitted() {
        let (fake, setup) = fed();
        let host = Host::with(setup);
        host.registry.start_all().expect("start");
        let tracking = host.runtime.tracking().expect("running");
        wait_until("one display", || tracking.snapshot().screens.len() == 1);
        fake.set_layout(stache_macos::DisplayLayout::new(vec![
            display(1, true, Some(60.0), 0.0),
            display(2, false, Some(60.0), 1920.0),
        ]));
        assert_eq!(fake.reconfigure(), 1);
        wait_until("two displays", || tracking.snapshot().screens.len() == 2);
        host.disable();
    }

    #[test]
    fn stop_removes_the_observer_after_the_watcher_and_before_the_epoch() {
        let (fake, setup) = fed();
        let host = Host::with(setup);
        host.registry.start_all().expect("start");
        host.disable();
        assert_eq!(fake.observers(), 0);
        assert_eq!(host.trace(), [
            Teardown::Sealed,
            Teardown::Drag,
            Teardown::Drained,
            Teardown::Animation,
            Teardown::EnhancedUi,
            Teardown::Restore,
            Teardown::Watcher,
            Teardown::Displays,
            Teardown::Epoch,
            Teardown::Actor,
            Teardown::Runner,
        ]);
    }

    #[test]
    fn a_failed_observer_removal_is_quarantined_and_retried() {
        let (fake, setup) = fed();
        let host = Host::with(setup);
        host.registry.start_all().expect("start");
        fake.fail_unobserves(1);
        host.disable();
        let status = host.runtime.status().read();
        assert_eq!(status.state, TilingRuntimeState::Quarantined, "{status:?}");
        assert_eq!(status.owed, ["display observer"]);
        assert_eq!(fake.observers(), 1, "never force-cleared");
        host.disable();
        assert_eq!(host.state(), TilingRuntimeState::Inactive);
        assert_eq!(fake.observers(), 0);
    }

    #[test]
    fn a_refused_observer_fails_the_start_and_leaves_nothing_acquired() {
        let (fake, setup) = fed();
        fake.fail_next_observe();
        let host = Host::with(setup);
        let _ = host.registry.start_all();
        assert!(matches!(host.status(), ModuleStatus::Failed(_)));
        assert_eq!(fake.observers(), 0);
        assert_eq!(host.installs(), host.stops());
        assert_eq!(host.state(), TilingRuntimeState::Inactive);
    }

    #[test]
    fn requested_borders_without_an_authority_degrade_borders_only_and_animation_is_not() {
        let host = Host::new();
        let mut config = enabled();
        config.animations.enabled = true;
        config.borders.enabled = true;
        host.reload(config.clone()).expect("reload");
        host.registry.start_all().expect("start");
        wait_until("adopted", || {
            host.runtime_revision() == host.revision().to_string()
        });
        let status = host.runtime.status().read();
        assert_eq!(status.state, TilingRuntimeState::Running);
        assert!(status.healthy, "an unsupported feature is not a failure");
        assert_eq!(status.diagnostics.len(), 2, "{status:?}");
        assert!(
            !status.diagnostics.iter().any(|d| d.contains("animations")),
            "animation is implemented: no unsupported diagnostic"
        );
        assert!(status.diagnostics[0].contains("no JankyBorders authority"));
        assert!(status.diagnostics[1].contains("native tabs are not read"));
        // The accepted configuration still holds the request.
        assert_eq!(
            host.registry.snapshot(host.handle).expect("snapshot").revision,
            host.revision()
        );
        // Turning them off clears the diagnostics once adopted.
        config.animations.enabled = false;
        config.borders.enabled = false;
        host.reload(config).expect("reload");
        wait_until("requested-feature diagnostics cleared", || {
            host.runtime.status().read().diagnostics.len() == 1
        });
        assert!(host.runtime.status().read().diagnostics[0].contains("native tabs"));
        host.disable();
    }

    #[test]
    fn the_default_configuration_reports_only_the_tab_capability() {
        let host = Host::started();
        let diagnostics = host.runtime.status().read().diagnostics;
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].contains("native tabs are not read"));
        host.disable();
    }

    #[test]
    fn a_partial_physical_outcome_is_counted_in_the_status() {
        let (_fake, setup) = fed();
        let host = Host::with(setup);
        host.registry.start_all().expect("start");
        host.backend.script_action(WindowActionOutcome::Issued {
            kind: IssuedKind::Partial,
            native: NativeStatus::Frame {
                position: WriteStatus::Written,
                size: WriteStatus::Failed(AxError::ActionUnsupported),
            },
            observed: Some(FrameRead {
                position: AxReading::Value(AxPoint { x: TARGET.x, y: TARGET.y }),
                size: AxReading::Value(AxSize { width: 1000.0, height: 700.0 }),
            }),
        });
        let runtime = host.runtime.tracking().expect("running");
        let mut feed = Feed::new(runtime.epoch().value());
        feed.adapter = host.backend.adapter();
        wait_until("tracked", || {
            host.calls(|c| matches!(c, FakeBackendCall::Track(..))) == 1
        });
        let sink = host.backend.sink().expect("sink");
        let token = feed.token(1, 1);
        assert!(sink.push(feed.event(WindowEventKind::Snapshot {
            snapshot: snapshot(token, "w", 1),
            origin: SnapshotOrigin::Created,
        })));
        assert!(sink.push(feed.event(WindowEventKind::EnumerationComplete {
            application: app(1),
            windows: 1,
            truncated: false,
        })));
        wait_until("partial outcome surfaced", || {
            host.runtime.status().read().partial_outcomes == 1
        });
        let status = host.runtime.status().read();
        assert_eq!(status.state, TilingRuntimeState::Running);
        assert!(status.healthy, "a partial placement is not a runtime failure");
        host.disable();
    }
}

// ------------------------------------------------------------- TI-08-02

mod drag_input {
    use std::sync::atomic::AtomicUsize;

    use stache_sdk::input::{
        CallbackScope, ControlAction, Disposition, HandlerSpec, InputEvent, InputEventKind,
        InputFilter, InputHandler, InputObserver, InputPoint, InputPriority, KindSet,
        ModifierFlags, MouseButton, ObserverSpec, PressStream, RawInput, TapKind, WorkRequirement,
    };

    use super::*;
    use crate::drag::LostCause;
    use crate::state::DragOutcome;
    use crate::test_support::{one_display, snapshot};

    fn raw(kind: InputEventKind, button: u32) -> RawInput {
        RawInput {
            kind,
            stream: PressStream::Button(MouseButton(button)),
            flags: ModifierFlags::NONE,
            repeat: false,
            timestamp: Duration::from_millis(1),
            location: Some(InputPoint { x: 120.0, y: 80.0 }),
        }
    }

    fn down() -> RawInput { raw(InputEventKind::MouseDown, 0) }

    fn up() -> RawInput { raw(InputEventKind::MouseUp, 0) }

    fn mouse_filter() -> InputFilter {
        InputFilter::new(KindSet::of(InputEventKind::MouseDown).with(InputEventKind::MouseUp))
            .with_buttons([MouseButton(0)])
    }

    fn input(host: &Host) -> &FakeInput { host.input.as_ref().expect("fake input") }

    fn listen_only_live(host: &Host) -> bool { input(host).backend.is_live(TapKind::ListenOnly) }

    /// A tracked, enumerated window with frame writes published.
    fn ready_window(host: &Host) -> (Arc<crate::tracking::TrackingRuntime>, crate::TilingSnapshot) {
        host.backend.script_action(applied());
        let runtime = host.runtime.tracking().expect("running");
        let mut feed = Feed::new(runtime.epoch().value());
        feed.adapter = host.backend.adapter();
        runtime.submit_displays(None, one_display()).expect("displays");
        wait_until("tracked", || {
            host.backend
                .calls()
                .iter()
                .any(|call| matches!(call, FakeBackendCall::Track(..)))
        });
        let sink = host.backend.sink().expect("sink");
        let token = feed.token(1, 1);
        assert!(sink.push(feed.event(WindowEventKind::Snapshot {
            snapshot: snapshot(token, "w", 1),
            origin: SnapshotOrigin::Created,
        })));
        assert!(sink.push(feed.event(WindowEventKind::EnumerationComplete {
            application: app(1),
            windows: 1,
            truncated: false,
        })));
        wait_until("complete", || runtime.snapshot().phase == Phase::Complete);
        wait_until("placed", || {
            runtime.snapshot().frames.iter().any(|frame| frame.actual == Some(TARGET))
        });
        let snapshot = (*runtime.snapshot()).clone();
        (runtime, snapshot)
    }

    fn refreshes(host: &Host) -> usize {
        host.calls(|call| matches!(call, FakeBackendCall::Refresh(_)))
    }

    struct Suppressor(AtomicUsize);

    impl InputHandler for Suppressor {
        fn on_event(&self, _: &InputEvent, _: &mut CallbackScope) -> Disposition {
            self.0.fetch_add(1, Ordering::SeqCst);
            Disposition::Suppress
        }
    }

    #[derive(Default)]
    struct Bystander(AtomicUsize);

    impl InputObserver for Bystander {
        fn on_event(&self, _: &InputEvent) { self.0.fetch_add(1, Ordering::SeqCst); }
    }

    #[test]
    fn start_subscribes_only_the_listen_only_tap_and_reports_no_drag_problem() {
        let host = Host::started();
        assert!(listen_only_live(&host));
        assert!(!input(&host).backend.is_live(TapKind::Suppressing));
        assert_eq!(
            input(&host).backend.count(TapKind::Suppressing, ControlAction::Install),
            0
        );
        assert_eq!(
            input(&host).backend.count(TapKind::ListenOnly, ControlAction::Install),
            1
        );
        let diagnostics = host.runtime.status().read().diagnostics;
        assert!(
            diagnostics.iter().all(|d| !d.contains("drag")),
            "{diagnostics:?}"
        );
        host.disable();
    }

    #[test]
    fn stop_removes_the_observer_first_and_the_tap_closes_with_the_last_subscriber() {
        let host = Host::started();
        assert!(listen_only_live(&host));
        host.disable();
        assert_eq!(&host.trace()[..2], [Teardown::Sealed, Teardown::Drag]);
        assert!(
            !listen_only_live(&host),
            "the tap closed with the last passive subscriber"
        );
        assert_eq!(
            input(&host).backend.count(TapKind::ListenOnly, ControlAction::Remove),
            1
        );
        assert_eq!(host.state(), TilingRuntimeState::Inactive);
        // A second run subscribes again.
        host.enable();
        wait_until("running", || host.state() == TilingRuntimeState::Running);
        assert!(listen_only_live(&host));
        host.disable();
    }

    #[test]
    fn the_tap_outlives_tiling_while_another_passive_subscriber_remains() {
        let host = Host::started();
        let bystander = Arc::new(Bystander::default());
        let other = input(&host)
            .service
            .subscribe_observer(
                ObserverSpec { filter: mouse_filter() },
                Arc::clone(&bystander) as Arc<dyn InputObserver>,
                &CancellationToken::new(),
            )
            .expect("second passive subscriber");
        host.disable();
        assert!(
            listen_only_live(&host),
            "another passive subscriber still needs the tap"
        );
        input(&host).service.dispatch(TapKind::ListenOnly, down());
        assert_eq!(bystander.0.load(Ordering::SeqCst), 1);
        drop(other);
        assert!(!listen_only_live(&host));
    }

    #[test]
    fn overlapping_suppressors_cannot_hide_a_press_from_tiling_and_tiling_changes_nothing() {
        let host = Host::started();
        let service = &input(&host).service;
        let handlers: Vec<Arc<Suppressor>> =
            (0..3).map(|_| Arc::new(Suppressor(AtomicUsize::new(0)))).collect();
        // Three consumers at descending priority, standing for HQ > MA > KB.
        let subscriptions: Vec<_> = handlers
            .iter()
            .zip([3_u16, 2, 1])
            .map(|(handler, level)| {
                service
                    .subscribe_handler(
                        HandlerSpec {
                            filter: mouse_filter(),
                            priority: InputPriority { level, tie_break: 0 },
                            work: WorkRequirement::None,
                        },
                        Arc::clone(handler) as Arc<dyn InputHandler>,
                        &CancellationToken::new(),
                    )
                    .expect("handler")
            })
            .collect();
        let (runtime, _) = ready_window(&host);
        let before = runtime.snapshot().admission.drag_accepted;

        // One physical press: the suppressing tap consumes it, the listen-only
        // tap still sees it, and it always passes.
        for event in [down(), up()] {
            assert_eq!(
                service.dispatch(TapKind::Suppressing, event),
                Disposition::Suppress
            );
            assert_eq!(service.dispatch(TapKind::ListenOnly, event), Disposition::Pass);
        }
        wait_until("both signals reached the actor", || {
            runtime.snapshot().admission.drag_accepted == before + 2
        });
        // Only the highest-priority consumer ran; Tiling changed no priority.
        let calls: Vec<usize> = handlers.iter().map(|h| h.0.load(Ordering::SeqCst)).collect();
        assert_eq!(calls, [2, 0, 0]);
        // Tiling's observer is on the listen-only tap only.
        assert_eq!(
            input(&host).backend.count(TapKind::Suppressing, ControlAction::Install),
            1
        );
        drop(subscriptions);
        host.disable();
    }

    #[test]
    fn a_press_reaches_the_actor_as_a_gesture_without_any_window_read_in_the_callback() {
        let host = Host::started();
        let (runtime, tiled) = ready_window(&host);
        // The user moved the window a little; the final read reports it there.
        let mut moved = snapshot(tiled.windows[0].token, "w", 1);
        moved.frame = FrameRead {
            position: AxReading::Value(AxPoint {
                x: TARGET.x + 20.0,
                y: TARGET.y + 10.0,
            }),
            size: AxReading::Value(AxSize {
                width: TARGET.width,
                height: TARGET.height,
            }),
        };
        host.backend.script_snapshot(
            tiled.windows[0].token,
            stache_macos::windows::SnapshotOutcome::Snapshot(Box::new(moved)),
        );
        host.backend.script_action(applied());
        let snapshots_before = host.calls(|c| matches!(c, FakeBackendCall::Snapshot(_)));
        input(&host).service.dispatch(TapKind::ListenOnly, down());
        wait_until("gesture", || runtime.snapshot().drag.active);
        // Only the up asks for fresh reads, and they happen on the effect worker.
        assert_eq!(
            host.calls(|c| matches!(c, FakeBackendCall::Snapshot(_))),
            snapshots_before
        );
        input(&host).service.dispatch(TapKind::ListenOnly, up());
        wait_until("committed", || !runtime.snapshot().drag.active);
        assert_eq!(
            runtime.snapshot().drag.last.as_ref().map(|r| r.outcome.clone()),
            Some(DragOutcome::SnappedBack)
        );
        host.disable();
    }

    #[test]
    fn a_disabled_listen_only_tap_cancels_the_gesture_and_reconciles_once() {
        let host = Host::started();
        let (runtime, _) = ready_window(&host);
        input(&host).service.dispatch(TapKind::ListenOnly, down());
        wait_until("gesture", || runtime.snapshot().drag.active);
        let before = refreshes(&host);

        input(&host).service.tap_disabled(TapKind::ListenOnly);
        wait_until("cancelled", || !runtime.snapshot().drag.active);
        let last = runtime.snapshot().drag.last.clone().expect("record");
        assert_eq!(last.outcome, DragOutcome::Cancelled(LostCause::Discontinuity));
        wait_until("one reconcile", || refreshes(&host) == before + 1);

        // The up that follows is no gesture: nothing is decided or replayed.
        input(&host).service.dispatch(TapKind::ListenOnly, up());
        wait_until("up admitted", || runtime.snapshot().admission.drag_accepted >= 3);
        let snap = runtime.snapshot();
        assert_eq!(snap.diagnostics.drag_swaps + snap.diagnostics.drag_snap_backs, 0);
        assert_eq!(refreshes(&host), before + 1, "reconciled once, not per signal");
        host.disable();
    }

    #[test]
    fn an_input_epoch_change_between_down_and_up_cancels_the_gesture() {
        let host = Host::started();
        let (runtime, _) = ready_window(&host);
        input(&host).service.dispatch(TapKind::ListenOnly, down());
        wait_until("gesture", || runtime.snapshot().drag.active);
        // The suppressing tap was disabled: the shared epoch advances, so the up
        // below carries a different epoch than the down.
        input(&host).service.tap_disabled(TapKind::Suppressing);
        input(&host).service.dispatch(TapKind::ListenOnly, up());
        wait_until("cancelled", || !runtime.snapshot().drag.active);
        assert_eq!(
            runtime.snapshot().drag.last.as_ref().map(|r| r.outcome.clone()),
            Some(DragOutcome::Cancelled(LostCause::Discontinuity))
        );
        host.disable();
    }

    #[test]
    fn a_refused_listen_only_tap_degrades_drag_only() {
        let host = Host::with(Setup {
            listen_only_refused: true,
            ..Setup::default()
        });
        host.registry.start_all().expect("start");
        let status = host.runtime.status().read();
        assert_eq!(status.state, TilingRuntimeState::Running);
        assert!(status.healthy);
        assert!(
            status
                .diagnostics
                .iter()
                .any(|d| d.contains("drag") && d.contains("Input Monitoring")),
            "{:?}",
            status.diagnostics
        );
        assert!(!listen_only_live(&host), "nothing is retained");
        // Ordinary tiling is unaffected.
        let (runtime, _) = ready_window(&host);
        assert_eq!(runtime.snapshot().health, crate::state::Health::Healthy);
        host.disable();
        assert!(!host.trace().contains(&Teardown::Drag), "nothing was subscribed");
        assert_eq!(host.state(), TilingRuntimeState::Inactive);
    }

    #[test]
    fn an_unattached_input_service_degrades_drag_only() {
        let host = Host::with(Setup {
            input: false,
            ..Setup::default()
        });
        host.registry.start_all().expect("start");
        let status = host.runtime.status().read();
        assert_eq!(status.state, TilingRuntimeState::Running);
        assert!(status.healthy);
        assert!(
            status.diagnostics.iter().any(|d| d.contains("drag is unavailable")),
            "{:?}",
            status.diagnostics
        );
        let (runtime, _) = ready_window(&host);
        assert_eq!(runtime.snapshot().phase, Phase::Complete);
        host.disable();
    }

    #[test]
    fn the_observer_never_changes_a_decision_and_a_stopped_feed_goes_quiet() {
        // The observer trait returns nothing: it cannot suppress. A removed feed
        // enqueues nothing even if the service still dispatches to it.
        let host = Host::started();
        let (runtime, _) = ready_window(&host);
        let bystander = Arc::new(Bystander::default());
        let _keep = input(&host)
            .service
            .subscribe_observer(
                ObserverSpec { filter: mouse_filter() },
                Arc::clone(&bystander) as Arc<dyn InputObserver>,
                &CancellationToken::new(),
            )
            .expect("bystander");
        let before = runtime.snapshot().admission.drag_accepted;
        host.disable();
        input(&host).service.dispatch(TapKind::ListenOnly, down());
        assert_eq!(bystander.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            runtime.snapshot().admission.drag_accepted,
            before,
            "nothing after stop"
        );
    }
}

// ------------------------------------------------------------- TI-08-04

/// Opt-in animation over the real registry, coordinator, tracking runtime and a
/// real animation worker thread, with the fake backend as the only native
/// writer. Nothing here moves a window.
mod animation {
    use super::*;

    fn animated() -> TilingConfig {
        let mut config = enabled();
        config.animations.enabled = true;
        config.animations.duration = 200;
        config
    }

    /// A host with animation enabled before it starts, the fake echoing every
    /// frame write exactly.
    fn animated_host() -> Host {
        let host = Host::new();
        host.reload(animated()).expect("reload");
        host.registry.start_all().expect("start");
        host.backend.script_echo_frames(true);
        wait_until("adopted", || {
            host.runtime_revision() == host.revision().to_string()
        });
        host
    }

    /// The animated configuration with an outer gap, so the window has to move.
    fn animated_with_gap() -> TilingConfig {
        serde_json::from_value(serde_json::json!({
            "enabled": true,
            "gaps": { "inner": 0, "outer": 40 },
            "animations": { "enabled": true, "duration": 200 },
        }))
        .expect("config")
    }

    fn running(host: &Host) -> bool {
        host.runtime.tracking().is_some_and(|rt| rt.animation().is_running())
    }

    fn written(host: &Host) -> usize { host.backend.written_frames().len() }

    #[test]
    fn the_default_module_owns_no_animation_worker() {
        let host = Host::started();
        let tracking = host.runtime.tracking().expect("running");
        assert!(
            !tracking.animation().is_running(),
            "default off: no worker, no timer"
        );
        assert_eq!(tracking.animation().retired(), 0);
        feed_window(&host);
        host.backend.script_action(applied());
        wait_until("a direct write", || host.backend.action_count() >= 1);
        assert!(!tracking.animation().is_running());
        host.disable();
    }

    #[test]
    fn a_reload_that_turns_animations_off_leaves_leases_to_the_grace() {
        // Disabling animations is a configuration change, not a stop: the worker
        // retires, the epoch keeps running, and the leases expire on their own
        // after the idle grace; nothing is restored early.
        let host = animated_host();
        wait_until("the worker started", || running(&host));
        let stops = host.stops();
        let mut off = animated();
        off.animations.enabled = false;
        host.reload(off).expect("turn animations off");
        wait_until("the worker retired", || !running(&host));
        wait_until("adopted", || {
            host.runtime_revision() == host.revision().to_string()
        });
        assert!(
            !host.backend.calls().contains(&FakeBackendCall::RestoreLeases),
            "a reload does not restore leases"
        );
        assert_eq!(host.stops(), stops, "a reload does not stop the epoch");
        assert_eq!(host.state(), TilingRuntimeState::Running);
        assert!(!host.trace().contains(&Teardown::EnhancedUi));
        host.disable();
    }

    #[test]
    fn a_reload_starts_and_stops_the_worker_live() {
        let host = Host::started();
        assert!(!running(&host));
        host.reload(animated()).expect("enable");
        wait_until("the worker started", || running(&host));
        // Duration and easing changes keep the one worker.
        let mut spring = animated();
        spring.animations.easing = crate::config::EasingType::Spring;
        host.reload(spring).expect("retune");
        wait_until("retuned", || {
            host.runtime_revision() == host.revision().to_string()
        });
        assert!(running(&host));
        host.reload(enabled()).expect("disable");
        wait_until("the worker retired", || !running(&host));
        assert!(
            host.runtime
                .status()
                .read()
                .diagnostics
                .iter()
                .all(|d| !d.contains("animation")),
            "no unsupported-animation diagnostic at any point"
        );
        host.disable();
        assert_eq!(host.status(), ModuleStatus::Disabled);
    }

    /// Feeds the window; its first plan reveals the workspace and is written
    /// directly, never animated. Returns the window once that write settled.
    fn feed_and_settle(host: &Host) -> WindowInstance {
        let tracking = host.runtime.tracking().expect("running");
        let window = feed_window(host);
        wait_until("the direct write settled", || {
            tracking
                .snapshot()
                .frames
                .iter()
                .any(|s| s.window == window && s.outcome.is_some())
        });
        window
    }

    #[test]
    fn the_first_plan_of_a_revealed_workspace_is_one_direct_write_even_when_animated() {
        let host = animated_host();
        let window = feed_and_settle(&host);
        wait_until("nothing outstanding", || {
            host.runtime.tracking().is_some_and(|rt| rt.outstanding_frames() == 0)
        });
        let frames = host.backend.written_frames();
        assert_eq!(frames, [(window, TARGET)], "one jump, no intermediate frames");
        host.disable();
    }

    #[test]
    fn an_animated_plan_writes_gated_frames_and_acknowledges_the_exact_plan() {
        let host = animated_host();
        let tracking = host.runtime.tracking().expect("running");
        assert!(running(&host));
        let window = feed_and_settle(&host);
        wait_until("nothing outstanding", || tracking.outstanding_frames() == 0);
        let before = written(&host);
        // A layout change inside the visible workspace animates.
        host.reload(animated_with_gap()).expect("reload");
        wait_until("the animation settled", || {
            tracking.snapshot().frames.iter().any(|s| {
                s.window == window
                    && s.outcome.is_some()
                    && s.desired.is_some_and(|d| s.actual == Some(d) && d != TARGET)
            })
        });
        let frames = host.backend.written_frames();
        let animated = &frames[before..];
        assert!(
            animated.len() >= 4,
            "intermediate frames, not one jump: {}",
            animated.len()
        );
        assert!(animated.iter().all(|(w, _)| *w == window));
        let snapshot = tracking.snapshot();
        let status = snapshot.frames.iter().find(|s| s.window == window).expect("status");
        let desired = status.desired.expect("desired");
        assert_eq!(
            animated.last().expect("last").1,
            desired,
            "the exact final frame"
        );
        assert!(
            matches!(status.outcome, Some(crate::OperationOutcome::Applied { .. })),
            "{status:?}"
        );
        assert_eq!(status.actual, Some(desired));
        wait_until("nothing outstanding", || tracking.outstanding_frames() == 0);
        // The window's own echoes settle without a second plan.
        let count = written(&host);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(written(&host), count, "no write after the settle");
        host.disable();
        assert_eq!(host.status(), ModuleStatus::Disabled);
    }

    /// Settles the direct first write, then starts an animated layout change.
    fn start_animating(host: &Host) -> WindowInstance {
        let window = feed_and_settle(host);
        let before = written(host);
        host.reload(animated_with_gap()).expect("reload");
        wait_until("the first animated frame", || written(host) > before);
        window
    }

    #[test]
    fn stop_mid_animation_joins_the_worker_before_the_restore_and_writes_no_late_frame() {
        let host = animated_host();
        start_animating(&host);
        host.disable();
        assert_eq!(host.status(), ModuleStatus::Disabled);
        let trace = host.trace();
        assert_eq!(trace, [
            Teardown::Sealed,
            Teardown::Drag,
            Teardown::Drained,
            Teardown::Animation,
            Teardown::EnhancedUi,
            Teardown::Restore,
            Teardown::Watcher,
            Teardown::Epoch,
            Teardown::Actor,
            Teardown::Runner,
        ]);
        let after = written(&host);
        assert!(
            host.backend.written_frames().last().is_some_and(|(_, f)| *f != TARGET),
            "the animation was cut short, not completed"
        );
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(written(&host), after, "no frame after the stop");
        assert_eq!(host.state(), TilingRuntimeState::Inactive);
    }

    #[test]
    fn a_worker_stuck_in_a_native_send_quarantines_and_a_retry_drains_before_the_restore() {
        let host = animated_host();
        // The first plan is a direct write; hold the sends of the animated one.
        feed_and_settle(&host);
        let hold = host.backend.hold_sends();
        host.reload(animated_with_gap()).expect("reload");
        assert!(
            hold.wait_entered(1, Duration::from_secs(10)),
            "a frame send is held"
        );
        let held_frames = written(&host);

        host.disable();
        let status = host.runtime.status().read();
        assert_eq!(status.state, TilingRuntimeState::Quarantined, "{status:?}");
        assert!(status.owed.contains(&"frame drain".to_owned()), "{status:?}");
        assert!(
            status.owed.contains(&"animation worker".to_owned()),
            "{status:?}"
        );
        assert!(status.owed.contains(&"restore".to_owned()), "{status:?}");
        assert!(
            !host.trace().contains(&Teardown::Restore),
            "no restore under a live worker"
        );
        assert!(!host.trace().contains(&Teardown::Animation));
        assert!(host.runtime.tracking().is_none());

        // The send completes late; the retry runs only the owed stages.
        hold.release();
        host.enable();
        assert_eq!(
            host.status(),
            ModuleStatus::Running,
            "{:?}",
            host.runtime.status().read()
        );
        let trace = host.trace();
        let at = |step: Teardown| trace.iter().position(|s| *s == step).expect("ran");
        assert!(at(Teardown::Drained) < at(Teardown::Animation));
        assert!(at(Teardown::Animation) < at(Teardown::Restore));
        assert_eq!(trace.iter().filter(|s| **s == Teardown::Animation).count(), 1);
        assert_eq!(host.stops(), 1, "the epoch was not stopped twice");
        assert_eq!(
            written(&host),
            held_frames,
            "no frame was written after the seal, not even by the late worker"
        );
    }
}

mod borders {
    use super::*;
    use crate::borders::BordersParts;
    use crate::borders::fakes::{Cli, Net};

    struct Rig {
        host: Host,
        net: Arc<Net>,
        cli: Arc<Cli>,
    }

    fn rig(present: bool) -> Rig {
        let net = Net::new(present);
        let cli = Cli::new();
        let host = Host::with(Setup {
            borders: Some(BordersParts::new(net.factory(), cli.factory())),
            ..Setup::default()
        });
        Rig { host, net, cli }
    }

    fn with_borders() -> TilingConfig {
        let mut config = enabled();
        config.borders.enabled = true;
        config
    }

    fn running(host: &Host) -> bool {
        host.runtime.tracking().is_some_and(|rt| rt.borders().is_running())
    }

    #[test]
    fn disabled_borders_own_no_runner_port_or_child() {
        let rig = rig(true);
        rig.host.registry.start_all().expect("start");
        let tracking = rig.host.runtime.tracking().expect("running");
        assert!(!tracking.borders().is_running());
        assert!(!tracking.borders().owes_cleanup());
        feed_window(&rig.host);
        assert_eq!(
            (rig.net.created(), rig.net.lookups(), rig.net.sends().len()),
            (0, 0, 0)
        );
        assert!(rig.cli.spawned().is_empty());
        rig.host.disable();
        assert!(
            !rig.host.trace().contains(&Teardown::Borders),
            "no borders stage was owed"
        );
        let status = rig.host.runtime.status().read();
        assert!(status.detail.is_none(), "{status:?}");
    }

    #[test]
    fn an_accepted_enable_starts_the_runner_and_a_reload_to_disabled_stops_it_silently() {
        let rig = rig(true);
        rig.host.registry.start_all().expect("start");
        rig.host.reload(with_borders()).expect("enable");
        wait_until("the runner", || running(&rig.host));
        assert!(rig.net.wait_sends(1, Duration::from_secs(10)));
        assert_eq!(rig.net.sends()[0][0], "width=4");
        assert!(rig.net.sends()[0].last().expect("blacklist").starts_with("blacklist="));
        let status = rig.host.runtime.status().read();
        assert!(
            status.diagnostics.iter().all(|d| !d.contains("borders")),
            "{status:?}"
        );
        assert!(status.healthy);

        let before = rig.net.sends();
        rig.host.reload(enabled()).expect("disable");
        wait_until("the runner closed", || !running(&rig.host));
        let tracking = rig.host.runtime.tracking().expect("running");
        assert!(tracking.borders().join_within(Duration::from_secs(10)));
        assert_eq!(rig.net.dropped(), 1, "the connection was released");
        assert_eq!(rig.net.sends(), before, "disabling sent nothing");
        rig.host.disable();
    }

    #[test]
    fn stop_closes_the_runner_before_the_drain_and_leaves_the_last_settings() {
        let rig = rig(true);
        rig.host.reload(with_borders()).expect("config");
        rig.host.registry.start_all().expect("start");
        assert!(rig.net.wait_sends(1, Duration::from_secs(10)));
        let before = rig.net.sends();
        rig.host.disable();
        assert_eq!(rig.host.state(), TilingRuntimeState::Inactive);
        let trace = rig.host.trace();
        let borders = trace.iter().position(|s| *s == Teardown::Borders).expect("borders ran");
        let drained = trace.iter().position(|s| *s == Teardown::Drained).expect("drained");
        assert!(borders < drained);
        assert_eq!(rig.net.dropped(), 1);
        assert_eq!(rig.net.sends(), before, "stop sent no mutation");
        assert!(rig.net.sends().iter().flatten().all(|arg| arg != "width=0"));
        let detail = rig.host.runtime.status().read().detail.expect("detail");
        assert!(detail.contains("keeps the last border settings"), "{detail}");
    }

    #[test]
    fn a_missing_service_degrades_borders_only() {
        let rig = rig(false);
        rig.host.reload(with_borders()).expect("config");
        rig.host.registry.start_all().expect("start");
        wait_until("the diagnostic", || {
            rig.host
                .runtime
                .status()
                .read()
                .diagnostics
                .iter()
                .any(|d| d.contains("git.felix.borders"))
        });
        let status = rig.host.runtime.status().read();
        assert_eq!(status.state, TilingRuntimeState::Running);
        assert!(status.healthy, "borders alone are degraded");
        assert!(rig.net.sends().is_empty());
        assert!(rig.cli.spawned().is_empty(), "no CLI without a service");
        assert_eq!(rig.host.status(), ModuleStatus::Running);
        rig.host.disable();
        assert!(
            rig.host.runtime.status().read().detail.is_none(),
            "nothing was left behind"
        );
    }

    #[test]
    fn the_actor_feeds_the_focused_window_layout_to_the_runner() {
        let rig = rig(true);
        let mut config = with_borders();
        config.default_layout = LayoutType::Monocle;
        rig.host.reload(config).expect("config");
        rig.host.registry.start_all().expect("start");
        assert!(rig.net.wait_sends(1, Duration::from_secs(10)));
        let runtime = rig.host.runtime.tracking().expect("running");
        let mut feed = Feed::new(runtime.epoch().value());
        feed.adapter = rig.host.backend.adapter();
        runtime.submit_displays(None, one_display()).expect("displays");
        wait_until("tracked", || {
            rig.host
                .backend
                .calls()
                .iter()
                .any(|call| matches!(call, FakeBackendCall::Track(..)))
        });
        let sink = rig.host.backend.sink().expect("sink");
        let token = feed.token(1, 1);
        assert!(sink.push(feed.event(WindowEventKind::Snapshot {
            snapshot: snapshot(token, "w", 1),
            origin: SnapshotOrigin::Created,
        })));
        assert!(sink.push(feed.event(WindowEventKind::EnumerationComplete {
            application: app(1),
            windows: 1,
            truncated: false,
        })));
        assert!(sink.push(feed.event(WindowEventKind::Window {
            window: token,
            change: stache_macos::windows::WindowChange::Focused,
        })));
        // The monocle workspace's state colors the active border: a pure value
        // derived from the published snapshot, no AX read.
        wait_until("the monocle focus command", || {
            rig.net.sends().iter().any(|args| {
                args.first().is_some_and(|a| a == "width=4")
                    && args.get(1).is_some_and(|a| a == "active_color=0xFFCBA6F7")
            })
        });
        rig.host.disable();
    }

    #[test]
    fn a_runner_blocked_in_a_send_quarantines_the_stop_and_the_retry_completes_it() {
        let rig = rig(true);
        rig.net.hold();
        rig.host.reload(with_borders()).expect("config");
        rig.host.registry.start_all().expect("start");
        assert!(rig.net.wait_entered(1, Duration::from_secs(10)));
        let started = Instant::now();
        rig.host.disable();
        assert!(started.elapsed() < Duration::from_secs(5), "the stop is finite");
        let status = rig.host.runtime.status().read();
        assert_eq!(status.state, TilingRuntimeState::Quarantined);
        assert!(status.owed.contains(&"borders runner".to_owned()), "{status:?}");
        assert!(matches!(rig.host.status(), ModuleStatus::Failed(_)));
        // The core stages that do not depend on the runner still ran.
        assert!(rig.host.trace().contains(&Teardown::Runner));

        rig.net.release();
        rig.host.enable();
        assert_eq!(
            rig.host.status(),
            ModuleStatus::Running,
            "{:?}",
            rig.host.runtime.status().read()
        );
        assert_eq!(rig.net.dropped(), 1, "the blocked connection was released");
        assert_eq!(
            rig.host.trace().iter().filter(|s| **s == Teardown::Borders).count(),
            1
        );
        rig.host.disable();
    }
}
