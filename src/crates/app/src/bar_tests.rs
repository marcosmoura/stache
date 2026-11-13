//! End-to-end Bar scenarios over fakes, for the real user setup.
//!
//! These build a real [`Registry`] and [`ConfigStore`], the real Bar module and
//! [`BarFactory`], the real topology feed over a fake display backend, the real
//! [`NativeWindows`] bookkeeping over a fake window host that behaves like
//! macOS (asynchronous created/loaded, close acknowledged later by a
//! `Destroyed` event, creation may fail, loads may never finish) and a consumer
//! that reads reservations exactly the way Tiling does
//! ([`HostReservations`], [`usable_area`]). They assert what Tiling would see
//! and which windows exist, not call counts.
//!
//! The setup is the user's: `bar.toml` (`enabled = true`, `height = 28`, `padding = 14`)
//! (top reservation 42), a built-in primary 1512x982 at scale 2 and an
//! external 2560x1440 at scale 1 positioned left of it (negative x).
//!
//! No real window, process, network or main thread is involved: the "main
//! thread" is a dedicated worker the tests can stall.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use stache_bar::{
    BarConfig, BarPanelState, BarRuntimeState, BarRuntimeStatus, BarTiming, BarWeatherLocation,
    BarWeatherProvider, LocationDiagnostic, MenuBarState, ProviderParts, ProviderTiming, SsidGate,
    WeatherParts,
};
use stache_macos::applications::DisplayRect;
use stache_macos::desktop::DisplayId as NativeId;
use stache_macos::menu_bar::testing::hidden_sample;
use stache_macos::{
    ApplicationBackend, ApplicationControl, DisplayControl, DisplayInfo, DisplayLayout,
    FakeApplicationBackend, FakeDisplayBackend, FakeLocationSource, FakeMenuBarBackend,
    LocationFix, LocationOutcome, LocationRequest, LocationSource, LocationUnavailable,
    MenuBarControl, MenuBarSample,
};
use stache_sdk::commands::{
    ChildProcess, Clock, CommandEnvironment, CommandExecutor, ProcessFactory, SpawnSpec,
    SystemClock,
};
use stache_sdk::{
    ConfigPaths, ConfigStore, DiscardLogger, EventBus, LogFilter, LogRecord, Logger,
    MainThreadExecutor, ModuleStatus, Permission, PermissionError, PermissionProvider,
    PermissionRequirement, PermissionStatus, Registry, ThreadError, WindowFrame,
};
use stache_tiling::{DisplayProjection, HostLookup, HostReservations, Insets, Rect, usable_area};

use super::BarFactory;
use crate::access::{PaneGuard, PaneOpen, SettingsOpener, SettingsPane};
use crate::bar_actions::{
    ACTIVITY_MONITOR, BarAction, BarActionOutcome, BarActions, CALENDAR, WEATHER,
};
use crate::bar_bridge::{
    BarBridge, BarChanged, BarEventSink, BarSnapshotOutcome, BarUnavailableReason, BarView,
    BarWeatherOutcome, COALESCE_LIMIT, ModuleStates, is_bar_label, sequence_is_newer,
};
use crate::bindings::ModuleState;
use crate::display::sdk_display_id;
use crate::host::{ModuleControl, ModuleFactory};
use crate::windows::{CloseOutcome, NativeWindowSpec, NativeWindows, WindowEvents, WindowHost};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ------------------------------------------------------------- the fake main

type Job = Box<dyn FnOnce() + Send + 'static>;

/// A dedicated thread standing in for the `AppKit` main thread. Jobs run in
/// order; `stall` parks it so queued work (and every hop that waits for it)
/// sits unanswered, like a blocked main run loop.
struct MainLoop {
    sender: Mutex<Option<Sender<Job>>>,
    id: OnceLock<ThreadId>,
    stalled: Arc<AtomicBool>,
}

impl MainLoop {
    fn new() -> Arc<Self> {
        let (sender, receiver) = channel::<Job>();
        let stalled = Arc::new(AtomicBool::new(false));
        let main = Arc::new(Self {
            sender: Mutex::new(Some(sender)),
            id: OnceLock::new(),
            stalled: Arc::clone(&stalled),
        });
        let worker = Arc::clone(&main);
        thread::Builder::new()
            .name("fake-main".to_owned())
            .spawn(move || {
                let _ = worker.id.set(thread::current().id());
                for job in receiver {
                    while worker.stalled.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(1));
                    }
                    job();
                }
            })
            .expect("main loop");
        main
    }

    fn stall(&self, stalled: bool) { self.stalled.store(stalled, Ordering::SeqCst); }
}

impl MainThreadExecutor for MainLoop {
    fn is_main_thread(&self) -> bool { self.id.get() == Some(&thread::current().id()) }

    fn execute(&self, job: Job) -> Result<(), ThreadError> {
        if self.is_main_thread() {
            job();
            return Ok(());
        }
        lock(&self.sender)
            .as_ref()
            .ok_or(ThreadError::GroupStopped)?
            .send(job)
            .map_err(|_| ThreadError::GroupStopped)
    }
}

// -------------------------------------------------------- the fake window host

#[derive(Clone, Copy, PartialEq, Eq)]
enum HostMode {
    /// Created, then the page loads shortly after.
    Loads,
    /// Created, but the page never finishes loading.
    NeverLoads,
    /// Created, then the page fails to load.
    PageFails,
    /// The next creation fails after the queue accepted it.
    FailsToCreate,
}

struct PanelHost {
    main: Arc<MainLoop>,
    events: Mutex<Option<Weak<dyn WindowEvents>>>,
    windows: OnceLock<Weak<NativeWindows>>,
    mode: Mutex<HostMode>,
    opened: Mutex<Vec<NativeWindowSpec>>,
    moved: Mutex<Vec<(String, WindowFrame)>>,
    closed: Mutex<Vec<String>>,
    live: Mutex<BTreeSet<String>>,
    /// A close is requested but its `Destroyed` event never arrives.
    hold_close: AtomicBool,
}

impl PanelHost {
    fn new(main: Arc<MainLoop>) -> Arc<Self> {
        Arc::new(Self {
            main,
            events: Mutex::new(None),
            windows: OnceLock::new(),
            mode: Mutex::new(HostMode::Loads),
            opened: Mutex::default(),
            moved: Mutex::default(),
            closed: Mutex::default(),
            live: Mutex::default(),
            hold_close: AtomicBool::new(false),
        })
    }

    fn set_mode(&self, mode: HostMode) { *lock(&self.mode) = mode; }

    fn opened(&self) -> usize { lock(&self.opened).len() }

    fn live(&self) -> usize { lock(&self.live).len() }

    fn closed(&self) -> Vec<String> { lock(&self.closed).clone() }

    fn events(&self) -> Option<Arc<dyn WindowEvents>> {
        lock(&self.events).as_ref().and_then(Weak::upgrade)
    }
}

impl WindowHost for PanelHost {
    fn attach_events(&self, events: Weak<dyn WindowEvents>) { *lock(&self.events) = Some(events); }

    fn open(&self, spec: &NativeWindowSpec) -> Result<(), String> {
        lock(&self.opened).push(spec.clone());
        let mode = *lock(&self.mode);
        if mode == HostMode::FailsToCreate {
            self.set_mode(HostMode::Loads);
            return Err("native creation failed".to_owned());
        }
        lock(&self.live).insert(spec.label.clone());
        let label = spec.label.clone();
        let events = self.events();
        // The page-load event arrives later on the main thread, not inline.
        let _ = self.main.execute(Box::new(move || {
            let Some(events) = events else { return };
            match mode {
                HostMode::Loads => events.page_loaded(&label),
                HostMode::PageFails => events.page_failed(&label, "navigation failed".to_owned()),
                HostMode::NeverLoads | HostMode::FailsToCreate => {}
            }
        }));
        Ok(())
    }

    fn focus(&self, _: &str) {}

    fn reposition(&self, label: &str, frame: WindowFrame) {
        lock(&self.moved).push((label.to_owned(), frame));
    }

    fn close(&self, label: &str) -> CloseOutcome {
        lock(&self.closed).push(label.to_owned());
        if !self.hold_close.load(Ordering::SeqCst) {
            let windows = self.windows.get().and_then(Weak::upgrade);
            let label = label.to_owned();
            let _ = self.main.execute(Box::new(move || {
                if let Some(windows) = windows {
                    windows.window_closed(&label);
                }
            }));
            lock(&self.live).remove(&label_of(&self.closed));
        }
        CloseOutcome::Closing
    }
}

fn label_of(closed: &Mutex<Vec<String>>) -> String {
    lock(closed).last().cloned().unwrap_or_default()
}

// ------------------------------------------------------------------- the rig

struct Trust(AtomicBool);

impl PermissionProvider for Trust {
    fn status(&self, _: Permission) -> PermissionStatus {
        if self.0.load(Ordering::SeqCst) {
            PermissionStatus::Granted
        } else {
            PermissionStatus::Denied
        }
    }

    fn request(&self, permission: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(self.status(permission))
    }
}

fn display(id: u32, rect: DisplayRect, scale: f64, primary: bool, main: bool) -> DisplayInfo {
    DisplayInfo {
        id: NativeId::new(id),
        frame: rect,
        visible_frame: rect,
        scale,
        refresh_hz: None,
        name: None,
        is_primary: primary,
        is_main: main,
        is_builtin: primary,
    }
}

/// The built-in primary: `screens[0]`, 1512x982 at scale 2.
fn builtin() -> DisplayInfo {
    display(1, DisplayRect::new(0.0, 0.0, 1512.0, 982.0), 2.0, true, false)
}

/// The external display, left of the primary (negative x), and the key-window
/// `mainScreen` so the primary is not the main display.
fn external() -> DisplayInfo {
    display(
        2,
        DisplayRect::new(-2560.0, 0.0, 2560.0, 1440.0),
        1.0,
        false,
        true,
    )
}

fn both() -> DisplayLayout { DisplayLayout::new(vec![builtin(), external()]) }

const BAR: &str = "enabled = true\nheight = 28\npadding = 14\n";

/// A tight load deadline so failure scenarios do not wait for the production
/// ten seconds.
const SHORT_LOAD: Duration = Duration::from_millis(1300);
/// Automatic panel retries that never fire within a test.
const NEVER_RETRY: [Duration; 3] = [Duration::from_secs(3600); 3];
/// The display control's per-hop budget in these tests.
const BUDGET: Duration = Duration::from_millis(250);

/// Module states read from the registry, standing in for the host status.
struct RegistryStates(Arc<Registry>);

impl ModuleStates for RegistryStates {
    fn module_state(&self, id: &str) -> Option<ModuleState> {
        self.0
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.id.as_str() == id)
            .map(|snapshot| ModuleState::from(&snapshot.status))
    }
}

/// Records every event with its target label; `hold` parks the emitting worker
/// inside `emit` (a slow webview) and `fail` makes emission error.
#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<(String, BarChanged)>>,
    fail: AtomicBool,
    hold: Mutex<bool>,
    released: std::sync::Condvar,
}

impl RecordingSink {
    fn events(&self) -> Vec<(String, BarChanged)> { lock(&self.events).clone() }

    fn hold(&self, hold: bool) {
        *lock(&self.hold) = hold;
        self.released.notify_all();
    }
}

impl BarEventSink for RecordingSink {
    fn emit(&self, label: &str, event: &BarChanged) -> Result<(), String> {
        {
            let mut hold = lock(&self.hold);
            while *hold {
                hold = self.released.wait(hold).unwrap_or_else(PoisonError::into_inner);
            }
            drop(hold);
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err("the webview refused the event".to_owned());
        }
        lock(&self.events).push((label.to_owned(), event.clone()));
        Ok(())
    }
}

type Reply<T> =
    Arc<stache_macos::system::testing::Script<Result<T, stache_macos::system::SystemReadError>>>;

/// A child that never exits until it is killed.
struct Immortal {
    killed: bool,
    kills: Arc<std::sync::atomic::AtomicUsize>,
}

impl ChildProcess for Immortal {
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        use std::os::unix::process::ExitStatusExt;
        Ok(self.killed.then(|| std::process::ExitStatus::from_raw(9)))
    }

    fn kill(&mut self) -> std::io::Result<()> {
        self.killed = true;
        self.kills.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn read_stdout(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        if self.killed {
            Ok(0)
        } else {
            Err(std::io::ErrorKind::WouldBlock.into())
        }
    }

    fn read_stderr(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.read_stdout(buffer)
    }
}

struct StallingTools {
    spawns: Arc<std::sync::atomic::AtomicUsize>,
    kills: Arc<std::sync::atomic::AtomicUsize>,
}

impl ProcessFactory for StallingTools {
    fn spawn(&self, _: &SpawnSpec) -> std::io::Result<Box<dyn ChildProcess>> {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Immortal {
            killed: false,
            kills: Arc::clone(&self.kills),
        }))
    }
}

/// The fake system readers behind the Bar providers.
struct ProviderFakes {
    usage: Reply<f32>,
    smc: Reply<Option<f64>>,
    battery: Reply<Option<stache_macos::system::BatteryReadout>>,
    wifi: Reply<Vec<stache_macos::system::WifiInterfaceReading>>,
    spawns: Arc<std::sync::atomic::AtomicUsize>,
    kills: Arc<std::sync::atomic::AtomicUsize>,
}

impl ProviderFakes {
    fn reads(&self) -> usize { self.usage.calls() + self.battery.calls() + self.wifi.calls() }
}

fn fake_providers() -> (ProviderParts, ProviderFakes) {
    use stache_macos::system::testing::{FakeBattery, FakeCpuUsage, FakeSmc, FakeWifi, Script};
    use stache_macos::system::{
        BatteryPowerState, BatteryReadout, InterfaceName, WifiInterfaceReading, WifiPower,
    };

    let cpu = FakeCpuUsage::new(23.0);
    let usage = Arc::clone(&cpu.0);
    let smc = FakeSmc::new(Some(48.0));
    let smc_script = Arc::clone(&smc.0);
    let battery = Script::new(Ok(Some(BatteryReadout {
        percent: 80.0,
        state: BatteryPowerState::Discharging,
    })));
    let wifi = Script::new(Ok(vec![WifiInterfaceReading {
        name: InterfaceName::parse("en0").expect("name"),
        power: WifiPower::On,
        rssi: Some(-55),
    }]));
    let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let kills = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
    let executor = Arc::new(CommandExecutor::with_parts(
        Arc::new(StallingTools {
            spawns: Arc::clone(&spawns),
            kills: Arc::clone(&kills),
        }),
        Arc::clone(&clock),
        CommandEnvironment::default(),
    ));
    let tick = Duration::from_millis(4);
    let parts = ProviderParts {
        cpu: Arc::new(cpu),
        smc: Arc::new(smc),
        battery: Arc::new(FakeBattery(Arc::clone(&battery))),
        wifi: Arc::new(FakeWifi(Arc::clone(&wifi))),
        executor,
        clock,
        ipconfig: std::path::PathBuf::from("/usr/sbin/ipconfig"),
        ssid: SsidGate::Off,
        signals: None,
        timing: ProviderTiming {
            cpu: tick,
            cpu_warmup: Duration::from_millis(1),
            battery_charging: tick,
            battery_idle: tick,
            wifi: tick,
            wifi_min_gap: Duration::from_millis(1),
            tool_deadline: Duration::from_secs(30),
            join: Duration::from_secs(1),
        },
        media: None,
    };
    (parts, ProviderFakes {
        usage,
        smc: smc_script,
        battery,
        wifi,
        spawns,
        kills,
    })
}

/// Records the panes the Bar actions opened; nothing reaches `NSWorkspace`.
#[derive(Default)]
struct PaneRecorder {
    opened: Mutex<Vec<SettingsPane>>,
    next: Mutex<Option<PaneOpen>>,
}

impl SettingsOpener for PaneRecorder {
    fn open_accessibility_settings(&self) -> Result<(), String> { Err("unused".to_owned()) }

    fn open_pane(&self, pane: SettingsPane, current: PaneGuard) -> PaneOpen {
        if !current() {
            return PaneOpen::NotIssued;
        }
        let next = lock(&self.next).take();
        if let Some(result) = next {
            return result;
        }
        lock(&self.opened).push(pane);
        PaneOpen::Opened
    }
}

/// The executor for the Bar actions' application control: one thread per job,
/// so a parked fake native call never freezes the fake main loop the rest of
/// Bar runs on. `stall` parks jobs until `flush`, like a hung main queue.
#[derive(Default)]
struct AppQueue {
    stalled: AtomicBool,
    parked: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

impl AppQueue {
    fn stall(&self, stalled: bool) { self.stalled.store(stalled, Ordering::SeqCst); }

    fn flush(&self) {
        let jobs: Vec<_> = lock(&self.parked).drain(..).collect();
        for job in jobs {
            thread::spawn(job);
        }
    }
}

impl MainThreadExecutor for AppQueue {
    fn is_main_thread(&self) -> bool { false }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        if self.stalled.load(Ordering::SeqCst) {
            lock(&self.parked).push(job);
        } else {
            thread::spawn(job);
        }
        Ok(())
    }
}

/// Records every log record, so a test can prove nothing sensitive was logged.
#[derive(Default)]
struct CapturedLogs(Mutex<Vec<LogRecord>>);

impl Logger for CapturedLogs {
    fn log(&self, record: LogRecord) { lock(&self.0).push(record); }
}

/// A scripted location source that can park inside a read, like a pending
/// consent prompt. It records every request through the shared fake.
struct ScriptedLocation {
    fake: FakeLocationSource,
    held: Mutex<bool>,
    changed: std::sync::Condvar,
    entered: std::sync::atomic::AtomicUsize,
}

impl ScriptedLocation {
    fn new(outcome: LocationOutcome) -> Arc<Self> {
        Arc::new(Self {
            fake: FakeLocationSource::new(outcome),
            held: Mutex::new(false),
            changed: std::sync::Condvar::new(),
            entered: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn hold(&self, hold: bool) {
        *lock(&self.held) = hold;
        self.changed.notify_all();
    }

    fn requests(&self) -> Vec<LocationRequest> { self.fake.requests() }

    fn wait_entered(&self, count: usize) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.entered.load(Ordering::SeqCst) < count {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(2));
        }
        true
    }
}

impl LocationSource for ScriptedLocation {
    fn read(&self, request: LocationRequest) -> LocationOutcome {
        let outcome = self.fake.read(request);
        self.park(outcome)
    }

    fn read_watched(
        &self,
        request: LocationRequest,
        on_decided: stache_macos::ConsentNotifier,
    ) -> LocationOutcome {
        let outcome = self.fake.read_watched(request, on_decided);
        self.park(outcome)
    }

    fn cancel_pending(&self) { self.fake.cancel_pending(); }
}

impl ScriptedLocation {
    fn park(&self, outcome: LocationOutcome) -> LocationOutcome {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let mut held = lock(&self.held);
        while *held {
            held = self.changed.wait(held).unwrap_or_else(PoisonError::into_inner);
        }
        drop(held);
        outcome
    }
}

const SENTINEL_KEY: &str = "SENTINEL-KEY-7f3a91c2";
const SENTINEL_LATITUDE: f64 = 12.345_678;
const SENTINEL_LONGITUDE: f64 = -76.543_21;

fn sentinel_fix() -> LocationOutcome {
    LocationOutcome::Located(LocationFix {
        latitude: SENTINEL_LATITUDE,
        longitude: SENTINEL_LONGITUDE,
        accuracy_meters: 1_000.0,
        reduced_accuracy: true,
    })
}

struct Rig {
    location: Arc<ScriptedLocation>,
    logs: Arc<CapturedLogs>,
    app_queue: Arc<AppQueue>,
    apps: FakeApplicationBackend,
    panes: Arc<PaneRecorder>,
    providers: ProviderFakes,
    windows: Arc<NativeWindows>,
    menu: FakeMenuBarBackend,
    bridge: Arc<BarBridge>,
    sink: Arc<RecordingSink>,
    registry: Arc<Registry>,
    store: Arc<ConfigStore>,
    factory: BarFactory,
    control: Arc<dyn ModuleControl>,
    host: Arc<PanelHost>,
    backend: FakeDisplayBackend,
    main: Arc<MainLoop>,
    trust: Arc<Trust>,
    config: ConfigPaths,
    _root: tempfile::TempDir,
}

impl Rig {
    fn new(file: Option<&str>) -> Self { Self::over(file, both()) }

    fn over(file: Option<&str>, layout: DisplayLayout) -> Self {
        Self::build(file, layout, SHORT_LOAD, NEVER_RETRY)
    }

    /// A rig whose registry carries `input` as the shared input service.
    fn with_input(file: Option<&str>, input: Arc<stache_sdk::InputService>) -> Self {
        Self::build_with(file, both(), SHORT_LOAD, NEVER_RETRY, None, Some(input))
    }

    /// A rig whose failed panels retry after `backoff`.
    fn retrying(file: Option<&str>, backoff: [Duration; 3]) -> Self {
        Self::build(file, both(), SHORT_LOAD, backoff)
    }

    /// A rig whose page load deadline is `load`.
    fn loading_for(file: Option<&str>, load: Duration) -> Self {
        Self::build(file, both(), load, NEVER_RETRY)
    }

    fn build(
        file: Option<&str>,
        layout: DisplayLayout,
        load: Duration,
        backoff: [Duration; 3],
    ) -> Self {
        Self::build_with(file, layout, load, backoff, None, None)
    }

    #[allow(clippy::too_many_lines)] // One linear wiring of the whole host rig.
    fn build_with(
        file: Option<&str>,
        layout: DisplayLayout,
        load: Duration,
        backoff: [Duration; 3],
        media: Option<&Arc<MediaFakes>>,
        input: Option<Arc<stache_sdk::InputService>>,
    ) -> Self {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join("config")).expect("config directory");
        let config = ConfigPaths::new(root.path().join("config"));
        let main = MainLoop::new();
        let host = PanelHost::new(Arc::clone(&main));
        let windows = Arc::new(NativeWindows::new(
            Arc::clone(&main) as Arc<dyn MainThreadExecutor>,
            Arc::clone(&host) as Arc<dyn WindowHost>,
            Arc::new(DiscardLogger),
        ));
        let registry_windows = Arc::clone(&windows);
        let _ = host.windows.set(Arc::downgrade(&windows));
        let trust = Arc::new(Trust(AtomicBool::new(true)));
        let logs = Arc::new(CapturedLogs::default());
        let mut registry = Registry::new(
            Arc::clone(&logs) as Arc<dyn Logger>,
            Arc::new(EventBus::new()),
            Arc::clone(&main) as Arc<dyn MainThreadExecutor>,
            Arc::clone(&trust) as Arc<dyn PermissionProvider>,
            registry_windows,
        )
        .with_baseline_requirements(vec![PermissionRequirement::user_only(
            Permission::Accessibility,
        )])
        .expect("baseline")
        .with_state_schemas(&crate::published_state::state_schemas())
        .expect("the production schemas register before any module");
        if let Some(input) = input {
            registry = registry.with_input_service(input);
        }
        let registry = Arc::new(registry);
        let store = Arc::new(ConfigStore::new(
            Arc::clone(&registry),
            crate::catalog::catalog().expect("catalog"),
            config.clone(),
            Arc::new(LogFilter::default()),
        ));
        if let Some(file) = file {
            std::fs::write(config.module_file(BarConfig::ID), file).expect("bar.toml");
        }
        let backend = FakeDisplayBackend::new(layout);
        let control = DisplayControl::with_budget(
            Arc::clone(&main) as Arc<dyn MainThreadExecutor>,
            Arc::new(backend.clone()),
            BUDGET,
        );
        let menu = FakeMenuBarBackend::new(hidden_sample());
        let (mut provider_parts, providers) = fake_providers();
        if let Some(media) = media {
            provider_parts.media = Some(media.parts(&root.path().join("cache")));
        }
        let location = ScriptedLocation::new(sentinel_fix());
        // Weather resolves its key file against the rig's configuration
        // directory and the rig's own home, never the real ones.
        let weather = WeatherParts::new(
            Arc::clone(&location) as Arc<dyn LocationSource>,
            Some(root.path().join("home")),
            config.directory().to_path_buf(),
        );
        let factory = BarFactory::with_parts(control, |mut parts| {
            parts.timing = BarTiming {
                load_deadline: load,
                retry_backoff: backoff,
                menu_poll: Duration::from_millis(3),
                menu_retry: Duration::from_millis(3),
                menu_join: Duration::from_secs(1),
            };
            parts
                .with_menu(MenuBarControl::with_budget(
                    Arc::clone(&main) as Arc<dyn MainThreadExecutor>,
                    Arc::new(menu.clone()),
                    BUDGET,
                ))
                .with_providers(provider_parts)
        })
        .with_weather(weather);
        let sink = Arc::new(RecordingSink::default());
        let bridge = BarBridge::new(
            Arc::clone(&windows),
            Arc::new(RegistryStates(Arc::clone(&registry))),
            Arc::clone(&sink) as Arc<dyn BarEventSink>,
        );
        let apps = FakeApplicationBackend::new();
        apps.set_application_path(ACTIVITY_MONITOR, Some("/fake/Activity Monitor.app".into()));
        apps.set_application_path(CALENDAR, Some("/fake/Calendar.app".into()));
        apps.set_application_path(WEATHER, Some("/fake/Weather.app".into()));
        apps.complete_opens_synchronously();
        let panes = Arc::new(PaneRecorder::default());
        let app_queue = Arc::new(AppQueue::default());
        bridge.set_actions(Arc::new(BarActions::with_completion_wait(
            ApplicationControl::with_acknowledgement_budget(
                Arc::clone(&app_queue) as Arc<dyn MainThreadExecutor>,
                Arc::new(apps.clone()) as Arc<dyn ApplicationBackend>,
                BUDGET,
            ),
            Arc::clone(&panes) as Arc<dyn SettingsOpener>,
            Duration::from_millis(300),
        )));
        factory.attach_bridge(&bridge);
        factory.bind_registry(&registry);
        let handle = factory.register(&registry, &store).expect("register");
        Self {
            location,
            logs,
            app_queue,
            apps,
            panes,
            providers,
            windows,
            menu,
            bridge,
            sink,
            registry,
            store,
            factory,
            control: handle,
            host,
            backend,
            main,
            trust,
            config,
            _root: root,
        }
    }

    fn start(&self) { let _ = self.registry.start_all(); }

    fn status(&self) -> ModuleStatus {
        self.registry
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.id == BarConfig::ID)
            .expect("bar snapshot")
            .status
    }

    fn bar(&self) -> BarRuntimeStatus { self.factory.status().read() }

    /// The page load is observed by Bar's own worker after `start` returned.
    fn wait_loaded(&self) {
        self.wait("the panel to load", |rig| {
            rig.bar().panel == BarPanelState::Loaded
        });
    }

    /// Opens reach the fake host asynchronously after `start` returned.
    fn wait_opened(&self, count: usize) {
        self.wait("the host to see the open", |rig| rig.host.opened() >= count);
    }

    fn wait_failed(&self) {
        self.wait("the panel to fail", |rig| {
            rig.bar().panel == BarPanelState::Failed
        });
    }

    fn write(&self, text: &str) {
        std::fs::write(self.config.module_file(BarConfig::ID), text).expect("write");
        self.store.reload_one(BarConfig::ID).expect("reload");
    }

    /// The reservations exactly the way Tiling reads them.
    fn tiling(&self) -> HostReservations {
        HostReservations::from_snapshot(
            &self.registry.observe_screen_reservations().read(),
            &DisplayProjection::new(sdk_display_id),
        )
    }

    fn lookup(&self, id: u32) -> HostLookup { self.tiling().lookup(NativeId::new(id)) }

    /// The top inset Tiling sees for `id`; `None` when the lookup is not
    /// `Present`.
    fn top(&self, id: u32) -> Option<f64> {
        match self.lookup(id) {
            HostLookup::Present(display) => Some(display.insets.top),
            HostLookup::NoHostTopology | HostLookup::Absent => None,
        }
    }

    fn topology(&self) -> Vec<u32> {
        self.registry
            .observe_screen_reservations()
            .read()
            .displays
            .iter()
            .map(|display| display.display.raw())
            .collect()
    }

    /// Every attached display must be `Present` once a topology exists.
    fn assert_none_absent(&self, attached: &[u32]) {
        for id in attached {
            assert!(
                matches!(self.lookup(*id), HostLookup::Present(_)),
                "display {id} must be present, got {:?}",
                self.lookup(*id)
            );
        }
    }

    /// The live Bar panel label.
    fn label(&self) -> String { self.windows.live_label(BarConfig::ID).expect("a live Bar window") }

    /// Opens the Accessibility guide, a real non-Bar host window, and returns
    /// its live label.
    fn open_guide(&self) -> String {
        use stache_sdk::WindowBackend as _;

        let request = crate::windows::access_window_request().expect("a guide request");
        self.windows.open(crate::windows::HOST_OWNER, &request).expect("a guide window");
        let label = self
            .windows
            .live_label(crate::windows::HOST_OWNER)
            .expect("a live guide window");
        assert!(label.starts_with("stache-access-"), "{label}");
        label
    }

    /// `bar_snapshot` as the live Bar window.
    fn snapshot(&self) -> BarSnapshotOutcome { self.bridge.snapshot(&self.label()) }

    fn snapshot_sequence(&self) -> u64 { self.factory.status().published().sequence }

    fn available(&self) -> BarView {
        match self.snapshot() {
            BarSnapshotOutcome::Available { view } => view,
            other => panic!("expected an available view, got {other:?}"),
        }
    }

    /// Sets what the fake menu bar shows and signals the native observers.
    fn toggle_menu(&self, visible: bool) {
        self.menu.set_sample(MenuBarSample {
            menu_bar_visible: Some(visible),
            ..hidden_sample()
        });
        assert_eq!(self.menu.signal(), 1, "the menu observer is registered");
    }

    fn wait_menu(&self, state: MenuBarState) {
        self.wait("the menu state", |rig| {
            matches!(rig.snapshot(), BarSnapshotOutcome::Available { view } if view.menu.state == state)
        });
    }

    fn wait(&self, what: &str, condition: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Fires a native display reconfiguration with `layout` as the new truth
    /// and waits until the feed applied it.
    fn reconfigure(&self, layout: DisplayLayout) {
        let ids: Vec<u32> = layout.displays().iter().map(|d| d.id.raw()).collect();
        self.backend.set_layout(layout);
        assert!(self.backend.reconfigure() > 0, "an observer must be registered");
        self.wait("the topology to follow the displays", |rig| {
            rig.topology() == sorted(&ids)
        });
    }
}

fn sorted(ids: &[u32]) -> Vec<u32> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids
}

/// Tiling's usable area of a display, with the host reservation from the
/// registry and an outer gap, the way its geometry combines them.
fn usable_top(rig: &Rig, id: u32, bounds_height: f64, native_top: f64, outer: f64) -> f64 {
    let host = match rig.lookup(id) {
        HostLookup::Present(display) => display.insets,
        HostLookup::NoHostTopology => Insets::ZERO,
        HostLookup::Absent => panic!("display {id} is Absent"),
    };
    let gaps = stache_tiling::Gaps::uniform(0.0, outer);
    usable_area(
        Rect::new(0.0, 0.0, 1000.0, bounds_height),
        Insets::new(native_top, 0.0, 0.0, 0.0),
        host,
        &gaps,
    )
    .expect("usable area")
    .y
}

// ------------------------------------------------------------------ scenarios

/// (a) Bar enabled at startup.
#[test]
fn bar_enabled_at_startup_lists_both_displays_and_reserves_the_primary_top() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(rig.topology(), vec![1, 2]);
    // Tiling: Present for both, never Absent; 42 on the primary, zero host
    // inset on the secondary.
    assert_eq!(rig.top(1), Some(42.0));
    assert_eq!(rig.top(2), Some(0.0));
    rig.assert_none_absent(&[1, 2]);
    // The one panel: non-activating, on the primary, inset by the padding.
    rig.wait_opened(1);
    assert_eq!(rig.host.opened(), 1);
    let spec = lock(&rig.host.opened).first().cloned().expect("opened");
    assert_eq!(spec.kind, stache_sdk::WindowKind::NonActivatingPanel);
    assert_eq!(spec.surface.as_str(), "bar");
    assert_eq!((spec.width, spec.height), (1484, 28));
    assert_eq!(spec.origin, Some(stache_sdk::WindowOrigin { x: 14, y: 14 }));
    rig.wait_loaded();
    let bar = rig.bar();
    assert_eq!(bar.state, BarRuntimeState::Running);
    assert_eq!(bar.panel, BarPanelState::Loaded);
    assert_eq!(bar.reservation.map(|r| (r.display, r.top)), Some((1, 42)));
    // Tiling's gaps: outer 14 on the main display, 10 on the secondary. The
    // host reservation and a native menu-bar inset combine by max, then add
    // the outer gap once: max(38, 42) + 14 = 56 on the primary, 0 + 10 on the
    // external.
    assert!((usable_top(&rig, 1, 982.0, 38.0, 14.0) - 56.0).abs() < f64::EPSILON);
    assert!((usable_top(&rig, 2, 1440.0, 0.0, 10.0) - 10.0).abs() < f64::EPSILON);
}

/// (b) Bar disabled (default config) and Bar paused.
#[test]
fn a_disabled_bar_leaves_tiling_at_no_host_topology_and_opens_nothing() {
    let rig = Rig::new(None);
    rig.start();
    assert_eq!(rig.status(), ModuleStatus::Disabled);
    assert!(rig.topology().is_empty());
    assert_eq!(rig.lookup(1), HostLookup::NoHostTopology);
    assert_eq!(rig.lookup(2), HostLookup::NoHostTopology);
    assert_eq!(rig.host.opened(), 0);
    assert_eq!(rig.backend.observers(), 0, "no display observation while off");
    assert_eq!(rig.backend.layout_reads(), 0);
    assert_eq!(rig.bar().state, BarRuntimeState::Inactive);
}

/// A disabled Bar owns no webview, no panel, no topology feed and no bridge
/// worker, and every Bar-shaped caller gets a typed `unavailable`.
#[test]
fn a_disabled_bar_owns_no_webview_and_no_bridge_worker() {
    let rig = Rig::new(None);
    rig.start();
    assert_eq!(rig.status(), ModuleStatus::Disabled);
    assert_eq!(rig.host.opened(), 0, "no window was ever requested");
    assert_eq!(rig.host.live(), 0, "no webview exists");
    assert!(!rig.bridge.worker_alive(), "no bridge worker exists");
    assert_eq!(rig.backend.observers(), 0);
    assert_eq!(
        unavailable(&rig.bridge.snapshot("stache-bar-1")),
        Some(BarUnavailableReason::Disabled),
        "the bridge answers a Bar-shaped label with a typed unavailable"
    );
    assert!(lock(&rig.sink.events).is_empty(), "nothing was emitted");
}

#[test]
fn a_paused_bar_returns_tiling_to_no_host_topology_and_resume_restores_it() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    assert_eq!(rig.top(1), Some(42.0));
    rig.control.pause(&rig.registry).expect("pause");
    assert!(rig.topology().is_empty());
    assert_eq!(rig.lookup(1), HostLookup::NoHostTopology);
    assert_eq!(rig.lookup(2), HostLookup::NoHostTopology);
    assert_eq!(rig.backend.observers(), 0);
    rig.wait("the panel to be closed", |rig| rig.host.live() == 0);
    rig.control.resume(&rig.registry).expect("resume");
    assert_eq!(rig.topology(), vec![1, 2]);
    assert_eq!(rig.top(1), Some(42.0));
    rig.wait_opened(2);
    assert_eq!(rig.host.opened(), 2);
}

/// (c) Hotplug.
#[test]
fn hotplug_never_leaves_an_attached_display_absent() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    // The external display is unplugged...
    rig.reconfigure(DisplayLayout::new(vec![builtin()]));
    assert_eq!(
        rig.lookup(1),
        HostLookup::Present(match rig.lookup(1) {
            HostLookup::Present(display) => display,
            other => panic!("expected present, got {other:?}"),
        })
    );
    assert_eq!(rig.top(1), Some(42.0));
    rig.assert_none_absent(&[1]);
    // ...and replugged under the same numeric id: a new presence.
    let before = match rig.lookup(1) {
        HostLookup::Present(display) => display.since,
        other => panic!("{other:?}"),
    };
    rig.reconfigure(both());
    rig.assert_none_absent(&[1, 2]);
    let HostLookup::Present(returned) = rig.lookup(2) else {
        panic!("external must be present")
    };
    assert!(returned.since > before, "a returning id is a new presence");
    // The numeric id is reused for a different display at another position.
    let reused = display(
        2,
        DisplayRect::new(1512.0, 0.0, 1920.0, 1080.0),
        2.0,
        false,
        true,
    );
    rig.reconfigure(DisplayLayout::new(vec![builtin()]));
    rig.reconfigure(DisplayLayout::new(vec![builtin(), reused]));
    rig.assert_none_absent(&[1, 2]);
    // A third display is added.
    let third = display(
        3,
        DisplayRect::new(3432.0, 0.0, 1920.0, 1080.0),
        1.0,
        false,
        false,
    );
    rig.reconfigure(DisplayLayout::new(vec![builtin(), external(), third]));
    rig.assert_none_absent(&[1, 2, 3]);
    assert_eq!(rig.top(1), Some(42.0));
    assert_eq!(rig.top(2), Some(0.0));
    assert_eq!(rig.top(3), Some(0.0));
    rig.wait_opened(1);
    assert_eq!(rig.host.opened(), 1, "hotplug never reopens the panel");
    assert_eq!(rig.status(), ModuleStatus::Running);
}

#[test]
fn a_primary_swap_moves_the_panel_and_the_reservation_atomically() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    // `screens[0]` becomes the external display; the built-in moves right.
    let new_primary = display(2, DisplayRect::new(0.0, 0.0, 2560.0, 1440.0), 1.0, true, true);
    let old = display(
        1,
        DisplayRect::new(2560.0, 0.0, 1512.0, 982.0),
        2.0,
        false,
        false,
    );
    rig.backend.set_layout(DisplayLayout::new(vec![new_primary, old]));
    assert!(rig.backend.reconfigure() > 0);
    rig.wait("the reservation to move", |rig| rig.top(2) == Some(42.0));
    // The old primary lost it in the same replacement.
    assert_eq!(rig.top(1), Some(0.0));
    rig.assert_none_absent(&[1, 2]);
    rig.wait_opened(1);
    assert_eq!(rig.host.opened(), 1, "the panel moved; it was not reopened");
    rig.wait("the panel to move", |rig| !lock(&rig.host.moved).is_empty());
    let (_, frame) = lock(&rig.host.moved).last().cloned().expect("moved");
    assert_eq!((frame.origin.x, frame.origin.y), (14, 14));
    assert_eq!(frame.size.width, 2532);
    assert_eq!(rig.bar().reservation.map(|r| r.display), Some(2));
}

// (d) Reconfigure.
#[test]
fn reconfigure_to_zero_height_and_back_runs_in_place() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    // A height/padding change moves the same window and replaces the inset.
    rig.write("enabled = true\nheight = 30\npadding = 10\n");
    rig.wait_opened(1);
    assert_eq!(rig.host.opened(), 1);
    rig.wait("the reposition", |rig| lock(&rig.host.moved).len() == 1);
    assert_eq!(rig.top(1), Some(40.0));
    // Zero height: no panel, no reservation; the topology stays applied.
    rig.write("enabled = true\nheight = 0\npadding = 10\n");
    assert_eq!(rig.status(), ModuleStatus::Running);
    rig.wait("the window to close", |rig| rig.host.live() == 0);
    assert_eq!(rig.top(1), Some(0.0));
    assert_eq!(
        rig.topology(),
        vec![1, 2],
        "topology still applied while Bar runs"
    );
    rig.assert_none_absent(&[1, 2]);
    assert_eq!(rig.bar().panel, BarPanelState::NoPanelZeroSize);
    // Back to 28/14: a fresh panel and the 42 reservation, still Running.
    rig.write(BAR);
    assert_eq!(rig.status(), ModuleStatus::Running);
    rig.wait_opened(2);
    assert_eq!(rig.host.opened(), 2);
    assert_eq!(rig.top(1), Some(42.0));
    rig.wait_loaded();
}

#[test]
fn an_unchanged_reload_creates_no_second_window_or_reservation() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    let revision = rig.registry.observe_screen_reservations().read().revision;
    rig.write(BAR);
    rig.write(
        "enabled = true\nheight = 28\npadding = 14\n[weather]\ndefault_location = \"Berlin\"\n",
    );
    rig.wait_opened(1);
    assert_eq!(rig.host.opened(), 1);
    assert!(lock(&rig.host.moved).is_empty());
    assert!(rig.host.closed().is_empty());
    assert_eq!(
        rig.registry.observe_screen_reservations().read().revision,
        revision
    );
}

// (e) Panel failures: Bar keeps running, the panel fails typed, the reservation
// goes, Tiling keeps a Present display with zero host inset.

/// What Tiling sees after the panel failed: Bar still runs, every display is
/// listed, and no constant top inset remains.
fn assert_failed_panel_state(rig: &Rig) {
    assert_eq!(rig.status(), ModuleStatus::Running);
    let bar = rig.bar();
    assert_eq!(bar.state, BarRuntimeState::Running);
    assert_eq!(bar.panel, BarPanelState::Failed);
    assert!(bar.reservation.is_none() && bar.frame.is_none());
    assert_eq!(rig.topology(), vec![1, 2]);
    assert_eq!(rig.top(1), Some(0.0));
    assert_eq!(rig.top(2), Some(0.0));
    rig.assert_none_absent(&[1, 2]);
}

#[test]
fn a_page_that_never_loads_fails_typed_without_stopping_bar_and_clears_the_inset() {
    let rig = Rig::new(Some(BAR));
    rig.host.set_mode(HostMode::NeverLoads);
    let begun = Instant::now();
    rig.start();
    assert!(
        begun.elapsed() < Duration::from_millis(800),
        "start does not wait for the page"
    );
    assert_eq!(rig.status(), ModuleStatus::Running);
    // While loading the reservation is staged at open.
    assert_eq!(rig.top(1), Some(42.0));
    rig.wait_failed();
    assert!(rig.bar().detail.as_deref().is_some_and(|d| d.contains("did not load")));
    assert_failed_panel_state(&rig);
    // The exact window was closed.
    rig.wait("the failed window to close", |rig| rig.host.live() == 0);
    assert_eq!(rig.host.closed().len(), 1);
    assert_eq!(rig.host.opened(), 1, "no automatic retry within the test");
}

#[test]
fn a_native_create_failure_after_start_fails_typed_and_keeps_bar_running() {
    let rig = Rig::new(Some(BAR));
    rig.host.set_mode(HostMode::FailsToCreate);
    rig.start();
    assert_eq!(rig.status(), ModuleStatus::Running);
    rig.wait_failed();
    assert!(
        rig.bar()
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("native creation failed"))
    );
    assert_failed_panel_state(&rig);
    assert_eq!(rig.host.live(), 0);
}

#[test]
fn a_page_load_failure_is_typed_and_clears_the_reservation() {
    let rig = Rig::new(Some(BAR));
    rig.host.set_mode(HostMode::PageFails);
    rig.start();
    rig.wait_failed();
    assert!(rig.bar().detail.as_deref().is_some_and(|d| d.contains("navigation failed")));
    assert_failed_panel_state(&rig);
}

#[test]
fn the_backoff_retry_recovers_once_the_host_starts_loading() {
    let rig = Rig::retrying(Some(BAR), [Duration::from_millis(150); 3]);
    rig.host.set_mode(HostMode::NeverLoads);
    rig.start();
    rig.wait_failed();
    assert_failed_panel_state(&rig);
    rig.host.set_mode(HostMode::Loads);
    rig.wait_loaded();
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(rig.top(1), Some(42.0));
    assert_eq!(rig.top(2), Some(0.0));
    assert_eq!(rig.topology(), vec![1, 2]);
    assert!(rig.host.opened() >= 2, "a fresh exact window");
}

#[test]
fn the_backoff_is_bounded_and_then_the_panel_stays_failed() {
    let rig = Rig::retrying(Some(BAR), [Duration::from_millis(40); 3]);
    rig.host.set_mode(HostMode::PageFails);
    rig.start();
    rig.wait("the initial open and three retries", |rig| {
        rig.host.opened() >= 4
    });
    rig.wait_failed();
    thread::sleep(Duration::from_millis(300));
    assert_eq!(rig.host.opened(), 4, "no further automatic retry");
    assert_failed_panel_state(&rig);
}

#[test]
fn pause_and_resume_retry_a_failed_panel_immediately() {
    let rig = Rig::new(Some(BAR));
    rig.host.set_mode(HostMode::NeverLoads);
    rig.start();
    rig.wait_failed();
    rig.host.set_mode(HostMode::Loads);
    rig.control.pause(&rig.registry).expect("pause");
    assert!(rig.topology().is_empty());
    rig.control.resume(&rig.registry).expect("resume");
    assert_eq!(rig.status(), ModuleStatus::Running);
    rig.wait_loaded();
    assert_eq!(rig.top(1), Some(42.0));
}

#[test]
fn an_accepted_config_change_retries_a_failed_panel_immediately() {
    let rig = Rig::new(Some(BAR));
    rig.host.set_mode(HostMode::NeverLoads);
    rig.start();
    rig.wait_failed();
    rig.host.set_mode(HostMode::Loads);
    rig.write("enabled = true\nheight = 30\npadding = 14\n");
    rig.wait_loaded();
    assert_eq!(rig.top(1), Some(44.0));
}

#[test]
fn a_hotplug_retries_a_failed_panel_immediately() {
    let rig = Rig::new(Some(BAR));
    rig.host.set_mode(HostMode::NeverLoads);
    rig.start();
    rig.wait_failed();
    rig.host.set_mode(HostMode::Loads);
    rig.reconfigure(DisplayLayout::new(vec![builtin()]));
    rig.wait_loaded();
    assert_eq!(rig.top(1), Some(42.0));
}

// (e2) A page that never loads cannot freeze any global gate.

#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(default, rename_all = "camelCase")]
struct SecondSettings {
    enabled: bool,
}

impl stache_sdk::ModuleConfig for SecondSettings {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), stache_sdk::ConfigError> { Ok(()) }
}

struct SecondModule;

impl stache_sdk::Module for SecondModule {
    type Config = SecondSettings;

    fn descriptor(&self) -> stache_sdk::ModuleDescriptor {
        stache_sdk::ModuleDescriptor {
            id: stache_sdk::ModuleId::new("second-fake"),
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
            observes: &[],
            reserves_screen: false,
        }
    }

    fn configure(
        &mut self,
        _: &SecondSettings,
        _: &stache_sdk::ModuleContext,
    ) -> stache_sdk::ModuleResult<()> {
        Ok(())
    }

    fn start(&mut self, _: &stache_sdk::ModuleContext) -> stache_sdk::ModuleResult<()> { Ok(()) }

    fn reconfigure(
        &mut self,
        _: &SecondSettings,
        _: &stache_sdk::ModuleContext,
    ) -> stache_sdk::ModuleResult<()> {
        Ok(())
    }

    fn stop(&mut self, _: &stache_sdk::ModuleContext) -> stache_sdk::ModuleResult<()> { Ok(()) }
}

#[test]
fn while_bars_page_never_loads_other_modules_lifecycle_and_permissions_stay_prompt() {
    let rig = Rig::loading_for(Some(BAR), Duration::from_secs(10));
    let second = rig
        .registry
        .register(SecondModule, SecondSettings { enabled: true })
        .expect("second module");
    rig.host.set_mode(HostMode::NeverLoads);
    let begun = Instant::now();
    rig.start();
    assert!(
        begun.elapsed() < Duration::from_secs(2),
        "start_all returned promptly"
    );
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert!(matches!(
        rig.bar().panel,
        BarPanelState::Requested | BarPanelState::Created
    ));
    let op = Instant::now();
    rig.registry.pause(second).expect("pause");
    rig.registry.resume(second).expect("resume");
    rig.registry.set_enabled(second, false).expect("disable");
    rig.registry.set_enabled(second, true).expect("enable");
    assert!(
        op.elapsed() < Duration::from_secs(2),
        "lifecycle ops stay prompt"
    );
    // The permission change also stops Bar mid-load, promptly.
    rig.trust.0.store(false, Ordering::SeqCst);
    let op = Instant::now();
    rig.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
        .expect("revoke");
    assert!(
        op.elapsed() < Duration::from_secs(2),
        "permission handling stays prompt"
    );
    assert!(rig.topology().is_empty());
    assert_eq!(rig.top(1), None);
}

#[test]
fn stopping_during_a_pending_load_is_prompt_and_a_late_load_resurrects_nothing() {
    let rig = Rig::loading_for(Some(BAR), Duration::from_secs(10));
    rig.host.set_mode(HostMode::NeverLoads);
    rig.start();
    rig.wait_opened(1);
    let label = lock(&rig.host.opened).first().cloned().expect("opened").label;
    let stopping = Instant::now();
    rig.control.set_enabled(&rig.registry, false).expect("disable");
    assert!(stopping.elapsed() < Duration::from_secs(2));
    assert!(rig.topology().is_empty());
    assert_eq!(rig.top(1), None);
    // The page finally "loads" after the stop.
    if let Some(events) = rig.host.events() {
        events.page_loaded(&label);
    }
    thread::sleep(Duration::from_millis(60));
    let bar = rig.bar();
    assert_ne!(bar.panel, BarPanelState::Loaded);
    assert!(bar.reservation.is_none());
    assert!(rig.topology().is_empty());
    rig.wait_opened(1);
    assert_eq!(rig.host.opened(), 1);
}

// (f) Permission revocation.
#[test]
fn a_permission_revocation_tears_down_the_panel_the_reservation_and_the_topology() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    assert_eq!(rig.top(1), Some(42.0));
    rig.trust.0.store(false, Ordering::SeqCst);
    rig.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
        .expect("revoke");
    assert!(matches!(rig.status(), ModuleStatus::WaitingForPermission(_)));
    assert!(rig.topology().is_empty());
    assert_eq!(rig.lookup(1), HostLookup::NoHostTopology);
    rig.wait("the panel to close", |rig| rig.host.live() == 0);
    assert_eq!(rig.backend.observers(), 0);
    // A grant starts a fresh generation.
    rig.trust.0.store(true, Ordering::SeqCst);
    rig.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
        .expect("grant");
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(rig.top(1), Some(42.0));
    rig.wait_opened(2);
    assert_eq!(rig.host.opened(), 2);
}

// (g) One bad input cannot freeze a global gate.
#[test]
fn a_hung_display_query_fails_only_bars_start_within_the_bound() {
    let rig = Rig::new(Some(BAR));
    rig.main.stall(true);
    let begun = Instant::now();
    let registry = Arc::clone(&rig.registry);
    let starter = thread::spawn(move || {
        let _ = registry.start_all();
    });
    // While Bar's start waits on the hung main queue, host reads stay prompt.
    thread::sleep(Duration::from_millis(60));
    let read = Instant::now();
    let _ = rig.registry.snapshots();
    let _ = rig.registry.observe_screen_reservations().read();
    assert!(read.elapsed() < Duration::from_millis(200));
    starter.join().expect("starter");
    assert!(begun.elapsed() < Duration::from_secs(3), "start is bounded");
    assert!(matches!(rig.status(), ModuleStatus::Failed(_)));
    assert!(rig.topology().is_empty());
    assert_eq!(rig.host.opened(), 0);
    // The queue recovers; a retry works.
    rig.main.stall(false);
    rig.control.set_enabled(&rig.registry, true).expect("retry");
    assert_eq!(rig.status(), ModuleStatus::Running);
}

#[test]
fn a_never_acknowledged_close_neither_blocks_stop_nor_harms_the_successor() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.host.hold_close.store(true, Ordering::SeqCst);
    let stopping = Instant::now();
    rig.control.set_enabled(&rig.registry, false).expect("disable");
    assert!(stopping.elapsed() < Duration::from_secs(2));
    assert!(rig.topology().is_empty());
    assert_eq!(rig.top(1), None);
    // The unacknowledged close is reported, with the exact identity retained.
    rig.wait("cleanup pending", |rig| {
        rig.bar().state == BarRuntimeState::CleanupPending
    });
    assert_eq!(rig.bar().cleanup_pending.len(), 1);
    // A successor generation is unharmed by the retained cleanup.
    rig.host.hold_close.store(false, Ordering::SeqCst);
    rig.control.set_enabled(&rig.registry, true).expect("enable");
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(rig.top(1), Some(42.0));
    rig.wait_loaded();
    // Shutdown also stays prompt with the old close still unacknowledged.
    let shutting = Instant::now();
    let _ = rig.registry.shutdown();
    assert!(shutting.elapsed() < Duration::from_secs(3));
    assert!(rig.topology().is_empty());
}

#[test]
fn a_stalled_main_queue_does_not_block_stop_beyond_the_bound_and_clears_first() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.main.stall(true);
    let stopping = Instant::now();
    let _ = rig.control.set_enabled(&rig.registry, false);
    // Observer removal needs the main thread: bounded by the budget, then the
    // lease reports that it still owes cleanup.
    assert!(stopping.elapsed() < Duration::from_secs(4), "stop is bounded");
    // The empty topology was applied before anything waited on main.
    assert!(rig.topology().is_empty());
    assert_eq!(rig.lookup(1), HostLookup::NoHostTopology);
    assert_eq!(rig.top(1), None);
    assert_eq!(rig.bar().state, BarRuntimeState::CleanupPending);
    // The queue recovers; the exact lease is retried and finishes.
    rig.main.stall(false);
    rig.control.set_enabled(&rig.registry, false).expect("retry the stop");
    rig.wait("cleanup to finish", |rig| {
        rig.bar().state == BarRuntimeState::Inactive
    });
    assert_eq!(rig.backend.observers(), 0);
}

struct Wait;

impl stache_sdk::ShutdownWait for Wait {
    fn now(&self) -> Instant { Instant::now() }

    fn deadline(&self) -> Instant { Instant::now() + Duration::from_secs(2) }

    fn wait(&self, timeout: Duration) { thread::sleep(timeout.min(Duration::from_millis(5))); }
}

#[test]
fn begin_shutdown_and_finish_shutdown_leave_nothing_owned() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.factory.begin_shutdown();
    let _ = rig.registry.shutdown();
    assert_eq!(rig.factory.finish_shutdown(&Wait), Ok(true));
    assert!(rig.topology().is_empty());
    assert_eq!(rig.backend.observers(), 0);
}

// ------------------------------------------------------- the host data bridge

fn unavailable(outcome: &BarSnapshotOutcome) -> Option<BarUnavailableReason> {
    match outcome {
        BarSnapshotOutcome::Unavailable { reason, .. } => Some(*reason),
        BarSnapshotOutcome::Available { .. } | BarSnapshotOutcome::Unauthorized => None,
    }
}

fn sequence_of(view: &BarView) -> u64 { view.sequence.parse().expect("a decimal sequence") }

#[test]
fn only_the_live_bar_window_is_authorized_and_everything_else_is_typed() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    let label = rig.label();
    assert!(is_bar_label(&label), "{label}");
    assert!(matches!(
        rig.bridge.snapshot(&label),
        BarSnapshotOutcome::Available { .. }
    ));
    // The live guide window, forged host and shell labels and forged Bar labels.
    let guide = rig.open_guide();
    for foreign in [
        guide.as_str(),
        "stache-test-1",
        "stache-access-1",
        "main",
        "shell",
        "",
        "stache-bar-",
        "stache-bar-x",
        "stache-bar-1-2",
        "stache-bar-999",
        "STACHE-BAR-1",
    ] {
        if foreign == label {
            continue;
        }
        assert_eq!(
            rig.bridge.snapshot(foreign),
            BarSnapshotOutcome::Unauthorized,
            "{foreign:?}"
        );
    }
}

#[test]
fn a_stale_bar_label_after_a_panel_reopen_is_unauthorized() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    let old = rig.label();
    rig.control.pause(&rig.registry).expect("pause");
    rig.control.resume(&rig.registry).expect("resume");
    rig.wait_opened(2);
    rig.wait("the new panel", |rig| {
        rig.windows.live_label(BarConfig::ID).is_some_and(|label| label != old)
    });
    let new = rig.label();
    assert_ne!(old, new, "a label is never reused");
    assert_eq!(rig.bridge.snapshot(&old), BarSnapshotOutcome::Unauthorized);
    assert!(matches!(
        rig.bridge.snapshot(&new),
        BarSnapshotOutcome::Available { .. }
    ));
}

#[test]
fn the_bridge_is_unavailable_with_a_reason_while_bar_is_not_running() {
    // Disabled.
    let disabled = Rig::new(Some("enabled = false\n"));
    disabled.start();
    assert_eq!(
        unavailable(&disabled.bridge.snapshot("stache-bar-1")),
        Some(BarUnavailableReason::Disabled)
    );
    assert!(
        !disabled.bridge.worker_alive(),
        "a disabled Bar owns no bridge worker"
    );

    // Paused, then resumed.
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    let label = rig.label();
    rig.control.pause(&rig.registry).expect("pause");
    let paused = rig.bridge.snapshot(&label);
    assert_eq!(
        unavailable(&paused),
        Some(BarUnavailableReason::Paused),
        "{paused:?}"
    );
    if let BarSnapshotOutcome::Unavailable { generation, sequence, .. } = &paused {
        assert_eq!(generation, "1");
        assert!(sequence.parse::<u64>().is_ok());
    }
    assert_eq!(rig.menu.observers(), 0);
    rig.control.resume(&rig.registry).expect("resume");
    rig.wait_opened(2);
    rig.wait("the new panel", |rig| {
        rig.windows.live_label(BarConfig::ID).is_some_and(|new| new != label)
    });
    assert!(matches!(rig.snapshot(), BarSnapshotOutcome::Available { .. }));

    // Held by the Accessibility gate.
    let gated = Rig::new(Some(BAR));
    gated.trust.0.store(false, Ordering::SeqCst);
    gated.start();
    let outcome = gated.bridge.snapshot("stache-bar-1");
    assert_eq!(
        unavailable(&outcome),
        Some(BarUnavailableReason::WaitingForPermission)
    );
    assert_eq!(gated.host.opened(), 0, "a gated Bar owns no window");

    // Failed.
    let failed = Rig::new(Some(BAR));
    failed.factory.begin_shutdown();
    failed.start();
    assert!(matches!(failed.status(), ModuleStatus::Failed(_)));
    assert_eq!(
        unavailable(&failed.bridge.snapshot("stache-bar-1")),
        Some(BarUnavailableReason::Failed)
    );
}

#[test]
fn a_registry_revocation_makes_the_bridge_unavailable_promptly() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    let label = rig.label();
    rig.trust.0.store(false, Ordering::SeqCst);
    rig.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
        .expect("revoke");
    assert_eq!(
        unavailable(&rig.bridge.snapshot(&label)),
        Some(BarUnavailableReason::WaitingForPermission)
    );
}

#[test]
fn a_stalled_main_queue_or_a_pending_load_never_blocks_the_command() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    let label = rig.label();
    rig.main.stall(true);
    // A refresh now hangs in the menu worker's bounded hop.
    rig.menu.signal();
    thread::sleep(Duration::from_millis(30));
    for _ in 0..20 {
        let begun = Instant::now();
        assert!(matches!(
            rig.bridge.snapshot(&label),
            BarSnapshotOutcome::Available { .. }
        ));
        assert!(
            begun.elapsed() < Duration::from_millis(200),
            "{:?}",
            begun.elapsed()
        );
    }
    rig.main.stall(false);
}

#[test]
fn a_page_that_never_loads_still_answers_the_bridge_promptly() {
    let rig = Rig::loading_for(Some(BAR), Duration::from_secs(5));
    rig.host.set_mode(HostMode::NeverLoads);
    rig.start();
    rig.wait_opened(1);
    let begun = Instant::now();
    let outcome = rig.bridge.snapshot(&rig.label());
    assert!(begun.elapsed() < Duration::from_secs(2));
    assert!(matches!(outcome, BarSnapshotOutcome::Available { .. }));
}

#[test]
fn the_dtos_serialize_with_exact_strings_and_stable_tags() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    rig.wait_menu(MenuBarState::Hidden);
    let outcome = rig.snapshot();
    let json = serde_json::to_value(&outcome).expect("serialize");
    assert_eq!(json["kind"], "available");
    let view = &json["view"];
    assert!(view["generation"].is_string());
    assert!(view["sequence"].is_string());
    assert!(view["configRevision"].is_string());
    assert_eq!(view["menu"]["state"], "hidden");
    assert_eq!(view["height"], 28);
    assert_eq!(view["padding"], 14);
    assert_eq!(view["panel"], "loaded");
    assert_eq!(view["frame"]["x"], 14);
    let back: BarSnapshotOutcome = serde_json::from_value(json).expect("round trip");
    assert_eq!(back, outcome);
    // Unavailable and the event.
    let off = serde_json::to_value(BarSnapshotOutcome::Unavailable {
        reason: BarUnavailableReason::WaitingForPermission,
        detail: None,
        generation: u64::MAX.to_string(),
        sequence: "9007199254740993".to_owned(),
    })
    .expect("serialize");
    assert_eq!(off["kind"], "unavailable");
    assert_eq!(off["reason"], "waitingForPermission");
    assert_eq!(off["sequence"], "9007199254740993", "exact above 2^53");
    assert_eq!(
        serde_json::to_value(BarSnapshotOutcome::Unauthorized).expect("serialize"),
        serde_json::json!({"kind": "unauthorized"})
    );
    let event = serde_json::to_value(BarChanged {
        generation: "1".to_owned(),
        sequence: "7".to_owned(),
        resnapshot: true,
        outcome: None,
    })
    .expect("serialize");
    assert_eq!(event["resnapshot"], true);
    assert!(event["outcome"].is_null());
}

#[test]
fn the_ordering_rule_rejects_a_stale_snapshot_after_a_newer_event() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    rig.wait_menu(MenuBarState::Hidden);
    // The UI subscribed first; a snapshot read now is S0.
    let stale = rig.available();
    let label = rig.label();
    rig.toggle_menu(true);
    rig.wait("the event", |rig| {
        rig.sink.events().iter().any(|(_, event)| {
            matches!(&event.outcome, Some(BarSnapshotOutcome::Available { view }) if view.menu.state == MenuBarState::Visible)
        })
    });
    let event = rig
        .sink
        .events()
        .into_iter()
        .rev()
        .find(|(_, event)| matches!(&event.outcome, Some(BarSnapshotOutcome::Available { view }) if view.menu.state == MenuBarState::Visible))
        .expect("the visible event")
        .1;
    assert!(sequence_is_newer(&stale.sequence, &event.sequence));
    // The slow initial snapshot (stale) arrives AFTER the event: it is dropped.
    assert!(!sequence_is_newer(&event.sequence, &stale.sequence));
    assert!(
        !sequence_is_newer(&event.sequence, &event.sequence),
        "equal is not newer"
    );
    // Numeric, not lexicographic: "9" < "10".
    assert!(sequence_is_newer("9", "10"));
    assert!(!sequence_is_newer("10", "9"));
    assert!(!sequence_is_newer("1", "not a number"));
    assert!(sequence_is_newer("", "1"));
    // Events went to the Bar label only.
    assert!(rig.sink.events().iter().all(|(target, _)| *target == label));
}

#[test]
fn label_shape_and_the_ordering_rule_hold_for_edge_values() {
    for good in ["stache-bar-1", "stache-bar-42", "stache-bar-0"] {
        assert!(is_bar_label(good), "{good}");
    }
    for bad in [
        "",
        "stache-bar-",
        "stache-bar-x",
        "stache-bar-1 ",
        "stache-bar-1/..",
        "stache-test-1",
        "stache-access-1",
        "Stache-bar-1",
        " stache-bar-1",
        "stache-bar--1",
        "stache-bar-\u{661}",
        "main",
    ] {
        assert!(!is_bar_label(bad), "{bad:?}");
    }
    let max = u64::MAX.to_string();
    let below = (u64::MAX - 1).to_string();
    assert!(sequence_is_newer(&below, &max));
    assert!(!sequence_is_newer(&max, &below));
    assert!(!sequence_is_newer(&max, &max));
    // Above 2^53 a JavaScript number would collapse these; the strings do not.
    assert!(sequence_is_newer("9007199254740992", "9007199254740993"));
    assert!(sequence_is_newer("0", "1"));
    // Out of range or malformed text is never newer.
    assert!(!sequence_is_newer("1", "18446744073709551616"));
    assert!(!sequence_is_newer("1", "-2"));
    assert!(!sequence_is_newer("1", "+3"));
    assert!(!sequence_is_newer("+1", "3"));
}

#[test]
fn events_follow_the_menu_with_increasing_sequences_and_only_to_the_bar_window() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    rig.wait_menu(MenuBarState::Hidden);
    rig.open_guide();
    let mut held = sequence_of(&rig.available());
    for step in 0..6 {
        let visible = step % 2 == 0;
        rig.toggle_menu(visible);
        rig.wait_menu(if visible {
            MenuBarState::Visible
        } else {
            MenuBarState::Hidden
        });
        let now = sequence_of(&rig.available());
        assert!(now > held, "{now} > {held}");
        held = now;
        assert_eq!(rig.top(1), Some(42.0), "Tiling keeps the constant top");
    }
    rig.wait("the last event", |rig| {
        rig.sink.events().iter().any(|(_, event)| event.sequence == held.to_string())
    });
    let events = rig.sink.events();
    let mut previous = 0;
    for (target, event) in &events {
        assert_eq!(*target, rig.label(), "never broadcast");
        let sequence: u64 = event.sequence.parse().expect("decimal");
        assert!(sequence > previous, "events are strictly increasing");
        previous = sequence;
    }
}

#[test]
fn a_failed_emission_is_followed_by_a_resnapshot_hint() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    rig.wait_menu(MenuBarState::Hidden);
    rig.wait("a delivered event", |rig| !rig.sink.events().is_empty());
    rig.sink.fail.store(true, Ordering::SeqCst);
    rig.toggle_menu(true);
    rig.wait_menu(MenuBarState::Visible);
    thread::sleep(Duration::from_millis(40));
    let delivered = rig.sink.events().len();
    rig.sink.fail.store(false, Ordering::SeqCst);
    rig.toggle_menu(false);
    rig.wait_menu(MenuBarState::Hidden);
    rig.wait("the recovery event", |rig| rig.sink.events().len() > delivered);
    let recovery = rig.sink.events().pop().expect("an event").1;
    assert!(recovery.resnapshot, "the UI missed one and must take a snapshot");
    assert!(recovery.outcome.is_none());
}

#[test]
fn coalescing_beyond_the_limit_becomes_a_resnapshot_hint() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    rig.wait_menu(MenuBarState::Hidden);
    rig.wait("a delivered event", |rig| !rig.sink.events().is_empty());
    // A slow webview: the worker is parked inside one emission while the menu
    // flips many more times than the bridge will coalesce.
    rig.sink.hold(true);
    rig.toggle_menu(true);
    rig.wait("the worker to park", |rig| {
        rig.snapshot_sequence() > 0 && rig.menu.samples() > 0
    });
    thread::sleep(Duration::from_millis(30));
    let before = rig.snapshot_sequence();
    for step in 0..=(COALESCE_LIMIT + 6) {
        rig.toggle_menu(step % 2 == 1);
        let want = if step % 2 == 1 {
            MenuBarState::Visible
        } else {
            MenuBarState::Hidden
        };
        rig.wait_menu(want);
    }
    assert!(rig.snapshot_sequence() > before + COALESCE_LIMIT);
    rig.sink.hold(false);
    rig.wait("the coalesced event", |rig| {
        rig.sink.events().last().is_some_and(|(_, event)| event.resnapshot)
    });
    // Then it is normal again.
    let seen = rig.sink.events().len();
    rig.toggle_menu(true);
    rig.wait_menu(MenuBarState::Visible);
    rig.wait("a plain event", |rig| {
        rig.sink.events().len() > seen
            && rig.sink.events().last().is_some_and(|(_, event)| !event.resnapshot)
    });
}

#[test]
fn shutting_down_while_changes_arrive_leaves_no_bridge_worker() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    assert!(rig.bridge.worker_alive());
    let bridge = Arc::clone(&rig.bridge);
    let churn = {
        let menu = rig.menu.clone();
        thread::spawn(move || {
            for _ in 0..200 {
                menu.signal();
            }
        })
    };
    rig.factory.begin_shutdown();
    let _ = rig.registry.shutdown();
    assert_eq!(rig.factory.finish_shutdown(&Wait), Ok(true));
    churn.join().expect("churn");
    assert!(!bridge.worker_alive());
    assert!(!bridge.worker_alive());
    assert_eq!(rig.menu.observers(), 0);
}

#[test]
fn a_bridge_that_began_shutdown_before_the_subscription_installs_spawns_nothing() {
    let rig = Rig::new(Some(BAR));
    let late = BarBridge::new(
        Arc::clone(&rig.windows),
        Arc::new(RegistryStates(Arc::clone(&rig.registry))),
        Arc::clone(&rig.sink) as Arc<dyn BarEventSink>,
    );
    late.begin_shutdown();
    late.attach(rig.factory.status());
    rig.start();
    rig.wait_loaded();
    rig.wait_menu(MenuBarState::Hidden);
    assert!(!late.worker_alive(), "a closed bridge never spawns a worker");
    assert!(late.finish_shutdown());
    // The live bridge was unaffected.
    assert!(rig.bridge.worker_alive());
}

#[test]
fn an_unattached_bridge_is_unavailable_and_owns_no_worker() {
    let rig = Rig::new(Some(BAR));
    let detached = BarBridge::new(
        Arc::clone(&rig.windows),
        Arc::new(RegistryStates(Arc::clone(&rig.registry))),
        Arc::clone(&rig.sink) as Arc<dyn BarEventSink>,
    );
    assert_eq!(
        unavailable(&detached.snapshot("stache-bar-1")),
        Some(BarUnavailableReason::Stopped)
    );
    assert_eq!(
        detached.snapshot("stache-test-1"),
        BarSnapshotOutcome::Unauthorized
    );
    assert!(!detached.worker_alive());
    assert!(detached.finish_shutdown());
}

// ------------------------------------------------------------- the e2e scenario

/// The user's setup (28/14, a built-in primary and a left external): Bar
/// running, the menu bar toggling, the bridge following, Tiling constant,
/// then pause and resume.
#[test]
fn e2e_menu_toggles_drive_the_bridge_while_tiling_keeps_a_constant_top() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    rig.wait_menu(MenuBarState::Hidden);
    assert_eq!(rig.top(1), Some(42.0));
    assert_eq!(rig.top(2), Some(0.0));
    let revision = rig.registry.observe_screen_reservations().read().revision;
    let first = rig.available();
    assert_eq!((first.height, first.padding), (28, 14));
    assert_eq!(first.generation, "1");
    let mut held = sequence_of(&first);
    for step in 0..8 {
        let visible = step % 2 == 0;
        rig.toggle_menu(visible);
        rig.wait_menu(if visible {
            MenuBarState::Visible
        } else {
            MenuBarState::Hidden
        });
        let next = sequence_of(&rig.available());
        assert!(next > held);
        held = next;
        assert_eq!(rig.top(1), Some(42.0));
        assert_eq!(
            rig.registry.observe_screen_reservations().read().revision,
            revision,
            "the registry never saw an inset update"
        );
    }
    rig.wait("the events caught up", |rig| {
        rig.sink
            .events()
            .last()
            .is_some_and(|(_, event)| event.sequence == held.to_string())
    });

    // Pause: unavailable, no observer, Tiling back to no host topology.
    let label = rig.label();
    rig.control.pause(&rig.registry).expect("pause");
    assert_eq!(
        unavailable(&rig.bridge.snapshot(&label)),
        Some(BarUnavailableReason::Paused)
    );
    assert_eq!(rig.menu.observers(), 0);
    assert_eq!(rig.lookup(1), HostLookup::NoHostTopology);
    rig.wait("the bridge worker to idle", |rig| !rig.bridge.worker_alive());

    // Resume: a new generation and a new label, the sequence keeps rising.
    rig.control.resume(&rig.registry).expect("resume");
    rig.wait_opened(2);
    rig.wait("the new panel", |rig| {
        rig.windows.live_label(BarConfig::ID).is_some_and(|new| new != label)
    });
    rig.wait_menu(MenuBarState::Hidden);
    let again = rig.available();
    assert_eq!(again.generation, "2");
    assert!(sequence_of(&again) > held);
    assert_eq!(rig.top(1), Some(42.0));
    assert_eq!(rig.menu.observers(), 1);
    assert_eq!(rig.bridge.snapshot(&label), BarSnapshotOutcome::Unauthorized);
}

// ---------------------------------------------------------------- providers

#[test]
fn provider_samples_flow_through_the_bridge_with_increasing_sequences() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    rig.wait("all three readings", |rig| {
        let view = rig.available();
        view.cpu.is_some() && view.battery.is_some() && view.wifi.is_some()
    });
    let view = rig.available();
    assert_eq!(
        view.cpu.map(|cpu| (cpu.usage, cpu.temperature)),
        Some((23, Some(48)))
    );
    assert_eq!(
        view.battery.map(|battery| (battery.percentage, battery.state)),
        Some((80, stache_bar::BatteryChargeState::Discharging))
    );
    let wifi = view.wifi.expect("wifi");
    assert_eq!(wifi.status, stache_bar::WifiStatus::Connected);
    assert_eq!((wifi.network_name, wifi.signal_strength), (None, Some(-55)));

    // A changed sample is a new event with a greater sequence; steady samples
    // add none.
    rig.providers.usage.set(Ok(61.0));
    rig.wait("the new CPU value", |rig| {
        rig.available().cpu.map(|cpu| cpu.usage) == Some(61)
    });
    rig.wait("the bridge to deliver", |rig| {
        lock(&rig.sink.events).iter().any(|(_, event)| {
            matches!(&event.outcome, Some(BarSnapshotOutcome::Available { view })
                if view.cpu.map(|cpu| cpu.usage) == Some(61))
        })
    });
    let delivered: Vec<u64> = lock(&rig.sink.events)
        .iter()
        .filter_map(|(_, event)| event.sequence.parse().ok())
        .collect();
    assert!(
        delivered.windows(2).all(|pair| pair[0] < pair[1]),
        "{delivered:?}"
    );
    let steady = rig.snapshot_sequence();
    thread::sleep(Duration::from_millis(60));
    assert_eq!(rig.snapshot_sequence(), steady, "unchanged samples emit nothing");
}

#[test]
fn a_hung_smc_read_delays_neither_battery_wifi_nor_bridge_reads() {
    let rig = Rig::new(Some(BAR));
    // The native SMC read never returns.
    rig.providers.smc.hold();
    rig.start();
    rig.wait_loaded();
    assert!(
        rig.providers.smc.wait_inside(1, Duration::from_secs(5)),
        "the SMC read is parked"
    );
    rig.wait("battery and wifi", |rig| {
        let view = rig.available();
        view.battery.is_some() && view.wifi.is_some()
    });
    assert!(
        rig.available().cpu.is_none(),
        "the hung CPU sample published nothing"
    );
    // A battery change still arrives while the tool hangs.
    rig.providers.battery.set(Ok(Some(stache_macos::system::BatteryReadout {
        percent: 12.0,
        state: stache_macos::system::BatteryPowerState::Charging,
    })));
    let begun = Instant::now();
    rig.wait("the new battery value", |rig| {
        rig.available().battery.map(|battery| battery.percentage) == Some(12)
    });
    assert!(begun.elapsed() < Duration::from_secs(2), "{:?}", begun.elapsed());
    let begun = Instant::now();
    for _ in 0..200 {
        let _ = rig.snapshot();
        let _ = rig.bar();
    }
    assert!(
        begun.elapsed() < Duration::from_secs(2),
        "bridge reads never wait on a provider"
    );
    // Pausing with the read still parked is bounded; releasing it lets the
    // owned worker drain, and no process was ever spawned.
    let begun = Instant::now();
    let _ = rig.control.pause(&rig.registry);
    assert!(begun.elapsed() < Duration::from_secs(3), "stop is bounded");
    rig.providers.smc.release();
    assert_eq!(rig.providers.spawns.load(Ordering::SeqCst), 0);
    assert_eq!(rig.providers.kills.load(Ordering::SeqCst), 0);
    assert!(matches!(
        rig.bridge.snapshot("stache-bar-1"),
        BarSnapshotOutcome::Unavailable { .. }
    ));
}

#[test]
fn pause_stops_every_provider_and_resume_restarts_them() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    rig.wait("readings", |rig| rig.available().cpu.is_some());
    rig.control.pause(&rig.registry).expect("pause");
    let after = rig.providers.reads();
    thread::sleep(Duration::from_millis(60));
    assert_eq!(rig.providers.reads(), after, "a paused Bar reads nothing");
    assert!(
        rig.factory.status().published().cpu.is_none(),
        "no stale reading survives the pause"
    );
    rig.control.resume(&rig.registry).expect("resume");
    rig.wait_loaded();
    rig.wait("readings again", |rig| {
        let view = rig.available();
        view.cpu.is_some() && view.battery.is_some() && view.wifi.is_some()
    });
    assert!(rig.providers.reads() > after);
}

#[test]
fn a_disabled_or_zero_size_bar_reads_nothing() {
    let rig = Rig::new(None);
    rig.start();
    thread::sleep(Duration::from_millis(40));
    assert_eq!(rig.providers.reads(), 0, "disabled");
    let zero = Rig::new(Some("enabled = true\nheight = 0\npadding = 14\n"));
    zero.start();
    thread::sleep(Duration::from_millis(40));
    assert_eq!(zero.providers.reads(), 0, "zero size owns no provider");
}

// ------------------------------------------------------- the fixed click actions

#[test]
fn e2e_a_click_on_each_component_reaches_the_fake_adapter_with_the_fixed_target() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    let label = rig.label();
    assert_eq!(
        rig.bridge.action(&label, BarAction::Cpu),
        BarActionOutcome::Completed
    );
    assert_eq!(
        rig.bridge.action(&label, BarAction::Clock),
        BarActionOutcome::Completed
    );
    assert_eq!(
        rig.bridge.action(&label, BarAction::Battery),
        BarActionOutcome::Completed
    );
    assert_eq!(
        rig.bridge.action(&label, BarAction::Wifi),
        BarActionOutcome::Completed
    );
    assert_eq!(
        rig.bridge.action(&label, BarAction::Weather),
        BarActionOutcome::Completed
    );
    assert_eq!(rig.apps.open_requests(), [
        std::path::PathBuf::from("/fake/Activity Monitor.app"),
        std::path::PathBuf::from("/fake/Calendar.app"),
        std::path::PathBuf::from("/fake/Weather.app"),
    ]);
    assert_eq!(*lock(&rig.panes.opened), [
        SettingsPane::Battery,
        SettingsPane::Wifi
    ]);
    assert_eq!(
        rig.apps.observations(),
        0,
        "no click leaves an application observer"
    );
    // The Bar keeps serving after the clicks.
    assert!(matches!(rig.snapshot(), BarSnapshotOutcome::Available { .. }));
}

#[test]
fn e2e_a_missing_app_or_a_rejected_url_is_a_typed_outcome() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    let label = rig.label();
    rig.apps.set_application_path(CALENDAR, None);
    assert_eq!(
        rig.bridge.action(&label, BarAction::Clock),
        BarActionOutcome::NotInstalled
    );
    *lock(&rig.panes.next) = Some(PaneOpen::Refused("rejected".to_owned()));
    assert_eq!(
        rig.bridge.action(&label, BarAction::Wifi),
        BarActionOutcome::Refused { detail: "rejected".to_owned() }
    );
    assert!(rig.apps.open_requests().is_empty());
}

#[test]
fn only_the_live_bar_window_may_click_and_everything_else_is_unauthorized() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    let label = rig.label();
    let guide = rig.open_guide();
    for foreign in [
        guide.as_str(),
        "stache-test-1",
        "stache-access-1",
        "main",
        "shell",
        "",
        "stache-bar-",
        "stache-bar-x",
        "stache-bar-999",
        "STACHE-BAR-1",
    ] {
        if foreign == label {
            continue;
        }
        for action in [
            BarAction::Cpu,
            BarAction::Clock,
            BarAction::Battery,
            BarAction::Wifi,
            BarAction::Weather,
        ] {
            assert_eq!(
                rig.bridge.action(foreign, action),
                BarActionOutcome::Unauthorized,
                "{foreign:?}"
            );
        }
    }
    // A stale label after a panel reopen.
    rig.control.pause(&rig.registry).expect("pause");
    rig.control.resume(&rig.registry).expect("resume");
    rig.wait_opened(2);
    rig.wait("the new panel", |rig| {
        rig.windows.live_label(BarConfig::ID).is_some_and(|new| new != label)
    });
    assert_eq!(
        rig.bridge.action(&label, BarAction::Cpu),
        BarActionOutcome::Unauthorized
    );
    assert!(rig.apps.open_requests().is_empty());
    assert!(lock(&rig.panes.opened).is_empty());
    assert_eq!(
        rig.bridge.action(&rig.label(), BarAction::Cpu),
        BarActionOutcome::Completed
    );
}

#[test]
fn clicks_are_unavailable_while_bar_is_disabled_paused_or_failed() {
    let unavailable = |outcome: BarActionOutcome| match outcome {
        BarActionOutcome::Unavailable { reason, .. } => Some(reason),
        _ => None,
    };
    let disabled = Rig::new(Some("enabled = false\n"));
    disabled.start();
    assert_eq!(
        unavailable(disabled.bridge.action("stache-bar-1", BarAction::Cpu)),
        Some(BarUnavailableReason::Disabled)
    );
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    let label = rig.label();
    rig.control.pause(&rig.registry).expect("pause");
    assert_eq!(
        unavailable(rig.bridge.action(&label, BarAction::Battery)),
        Some(BarUnavailableReason::Paused)
    );
    let failed = Rig::new(Some(BAR));
    failed.factory.begin_shutdown();
    failed.start();
    assert_eq!(
        unavailable(failed.bridge.action("stache-bar-1", BarAction::Wifi)),
        Some(BarUnavailableReason::Failed)
    );
    for rig in [&disabled, &rig, &failed] {
        assert!(rig.apps.open_requests().is_empty());
        assert!(lock(&rig.panes.opened).is_empty());
    }
}

#[test]
fn stopping_bar_mid_click_sends_nothing_and_a_restart_inherits_no_effects() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    let label = rig.label();
    let barrier = rig.apps.block_next_application_path();
    let bridge = Arc::clone(&rig.bridge);
    let click = thread::spawn(move || bridge.action(&label, BarAction::Cpu));
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    rig.control.pause(&rig.registry).expect("pause");
    assert!(barrier.release());
    let outcome = click.join().expect("click");
    assert!(
        matches!(
            outcome,
            BarActionOutcome::Stale | BarActionOutcome::TimedOutBeforeIssuance
        ),
        "{outcome:?}"
    );
    rig.control.resume(&rig.registry).expect("resume");
    rig.wait_opened(2);
    thread::sleep(Duration::from_millis(80));
    assert!(
        rig.apps.open_requests().is_empty(),
        "the stopped click never sends"
    );
    assert_eq!(rig.apps.observations(), 0);
}

#[test]
fn a_hung_adapter_never_blocks_bridge_reads_or_shutdown_and_rapid_clicks_are_busy() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    let label = rig.label();
    let barrier = rig.apps.block_next_application_path();
    let bridge = Arc::clone(&rig.bridge);
    let clicked = label.clone();
    let click = thread::spawn(move || bridge.action(&clicked, BarAction::Cpu));
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    for _ in 0..20 {
        let begun = Instant::now();
        assert!(matches!(
            rig.bridge.snapshot(&label),
            BarSnapshotOutcome::Available { .. }
        ));
        assert!(begun.elapsed() < Duration::from_secs(2), "{:?}", begun.elapsed());
        assert_eq!(
            rig.bridge.action(&label, BarAction::Wifi),
            BarActionOutcome::Busy
        );
    }
    let begun = Instant::now();
    rig.factory.begin_shutdown();
    let _ = rig.registry.shutdown();
    assert_eq!(rig.factory.finish_shutdown(&Wait), Ok(true));
    assert!(begun.elapsed() < Duration::from_secs(3), "{:?}", begun.elapsed());
    assert!(barrier.release());
    let outcome = click.join().expect("click");
    assert!(
        matches!(
            outcome,
            BarActionOutcome::Stale | BarActionOutcome::TimedOutBeforeIssuance
        ),
        "{outcome:?}"
    );
    assert!(rig.apps.open_requests().is_empty());
}

#[test]
fn a_stalled_main_queue_times_out_before_issuance_without_blocking_reads() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    let label = rig.label();
    rig.app_queue.stall(true);
    let bridge = Arc::clone(&rig.bridge);
    let clicked = label.clone();
    let click = thread::spawn(move || bridge.action(&clicked, BarAction::Clock));
    for _ in 0..10 {
        let begun = Instant::now();
        assert!(matches!(
            rig.bridge.snapshot(&label),
            BarSnapshotOutcome::Available { .. }
        ));
        assert!(begun.elapsed() < Duration::from_millis(200));
    }
    assert_eq!(
        click.join().expect("click"),
        BarActionOutcome::TimedOutBeforeIssuance
    );
    rig.app_queue.stall(false);
    rig.app_queue.flush();
    thread::sleep(Duration::from_millis(80));
    assert!(rig.apps.open_requests().is_empty(), "the late job never sends");
}

// ------------------------------------------------------------------- weather

/// The user's Bar with weather: provider `auto`, an env file for the key and a
/// default location. Height 28 and padding 14 reserve 42 at the top.
const WEATHER_BAR: &str = "enabled = true\nheight = 28\npadding = 14\n\n[weather]\nprovider = \"auto\"\napi_keys = \"keys.env\"\ndefault_location = \"Lisbon\"\n";

impl Rig {
    fn write_keys(&self, text: &str) {
        std::fs::write(self.config.directory().join("keys.env"), text).expect("keys.env");
    }

    /// `bar_weather` as the live Bar window.
    fn weather(&self) -> BarWeatherOutcome { self.bridge.weather(&self.label()) }

    fn weather_rig() -> Self {
        let rig = Self::new(Some(WEATHER_BAR));
        rig.write_keys(&format!("VISUAL_CROSSING_API_KEY={SENTINEL_KEY}\n"));
        rig.start();
        rig.wait_loaded();
        rig
    }
}

fn weather_of(outcome: BarWeatherOutcome) -> stache_bar::BarWeather {
    match outcome {
        BarWeatherOutcome::Ready { weather } => weather,
        other => panic!("expected ready weather, got {other:?}"),
    }
}

fn unavailable_weather(outcome: &BarWeatherOutcome) -> Option<BarUnavailableReason> {
    match outcome {
        BarWeatherOutcome::Unavailable { reason, .. } => Some(*reason),
        _ => None,
    }
}

#[test]
fn e2e_weather_through_the_real_bridge_returns_the_key_and_coordinates_without_touching_the_bar() {
    let rig = Rig::weather_rig();
    assert_eq!(rig.top(1), Some(42.0));
    let before = rig.available();
    let weather = weather_of(rig.weather());
    assert_eq!(weather.config_revision, before.config_revision);
    assert_eq!(weather.provider, BarWeatherProvider::Auto);
    assert_eq!(weather.api_key.as_deref(), Some(SENTINEL_KEY));
    assert_eq!(weather.refresh_interval_minutes, 20, "the default cadence");
    assert_eq!(weather.location, BarWeatherLocation::CoreLocation {
        latitude: SENTINEL_LATITUDE,
        longitude: SENTINEL_LONGITUDE,
        reduced_accuracy: true,
    });
    assert_eq!(rig.location.requests().len(), 1);
    assert!(!rig.location.requests()[0].precise, "the default is approximate");
    // The typed diagnostic is in the Bar status, and nothing visual changed.
    assert_eq!(rig.bar().weather.location, Some(LocationDiagnostic::Located));
    thread::sleep(Duration::from_millis(60));
    // The CPU provider keeps sampling, so compare what weather could touch.
    let after = rig.available();
    assert_eq!(
        (
            &after.panel,
            &after.menu,
            after.frame,
            after.height,
            after.padding,
            &after.detail
        ),
        (
            &before.panel,
            &before.menu,
            before.frame,
            before.height,
            before.padding,
            &before.detail
        ),
    );
    assert_eq!(
        (&after.generation, &after.config_revision),
        (&before.generation, &before.config_revision)
    );
    assert_eq!(rig.top(1), Some(42.0));
    rig.assert_none_absent(&[1, 2]);
}

#[test]
fn e2e_denied_location_falls_back_to_the_default_location_with_a_typed_diagnostic() {
    let rig = Rig::weather_rig();
    for (index, (reason, expected)) in [
        (LocationUnavailable::Denied, LocationDiagnostic::Denied),
        (
            LocationUnavailable::NotDeterminedTimeout,
            LocationDiagnostic::NotDeterminedTimeout,
        ),
        (
            LocationUnavailable::NoUsageDescription,
            LocationDiagnostic::NoUsageDescription,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        rig.location.fake.set(LocationOutcome::Unavailable(reason));
        // A new accepted revision each time, so the earlier outcome is not
        // reused.
        rig.write(&format!("{WEATHER_BAR}precise_location = {}\n", index % 2 == 0));
        let weather = weather_of(rig.weather());
        assert_eq!(weather.location, BarWeatherLocation::DefaultLocation {
            query: "Lisbon".into()
        });
        assert_eq!(rig.bar().weather.location, Some(expected));
    }
}

#[test]
fn precise_location_reaches_the_request_and_applies_at_the_next_read_without_a_restart() {
    let rig = Rig::weather_rig();
    let _ = weather_of(rig.weather());
    let generation = rig.available().generation;
    rig.write(&format!(
        "{WEATHER_BAR}precise_location = true\nrefresh_interval = 45\n"
    ));
    let weather = weather_of(rig.weather());
    assert_eq!(weather.refresh_interval_minutes, 45, "applies on the next read");
    assert_eq!(rig.available().generation, generation, "Bar was not restarted");
    let precise: Vec<bool> = rig.location.requests().iter().map(|r| r.precise).collect();
    assert_eq!(precise, [false, true]);
}

#[test]
fn weather_is_unavailable_with_zero_reads_while_bar_is_disabled_paused_failed_or_zero_size() {
    let disabled = Rig::new(Some("enabled = false\n"));
    disabled.start();
    assert_eq!(
        unavailable_weather(&disabled.bridge.weather("stache-bar-1")),
        Some(BarUnavailableReason::Disabled)
    );

    let paused = Rig::weather_rig();
    let label = paused.label();
    paused.control.pause(&paused.registry).expect("pause");
    assert_eq!(
        unavailable_weather(&paused.bridge.weather(&label)),
        Some(BarUnavailableReason::Paused)
    );

    let gated = Rig::new(Some(WEATHER_BAR));
    gated.trust.0.store(false, Ordering::SeqCst);
    gated.start();
    assert_eq!(
        unavailable_weather(&gated.bridge.weather("stache-bar-1")),
        Some(BarUnavailableReason::WaitingForPermission)
    );

    let failed = Rig::new(Some(WEATHER_BAR));
    failed.factory.begin_shutdown();
    failed.start();
    assert_eq!(
        unavailable_weather(&failed.bridge.weather("stache-bar-1")),
        Some(BarUnavailableReason::Failed)
    );

    // A panel that failed after start: Bar keeps running with no live window.
    let page = Rig::new(Some(WEATHER_BAR));
    page.host.set_mode(HostMode::NeverLoads);
    page.start();
    page.wait_failed();
    assert_eq!(
        unavailable_weather(&page.bridge.weather("stache-bar-1")),
        Some(BarUnavailableReason::Failed)
    );

    let zero = Rig::new(Some("enabled = true\nheight = 0\npadding = 14\n"));
    zero.start();
    zero.wait("the zero-size state", |rig| {
        rig.bar().panel == BarPanelState::NoPanelZeroSize
    });
    assert_eq!(
        unavailable_weather(&zero.bridge.weather("stache-bar-1")),
        Some(BarUnavailableReason::Unavailable)
    );

    for rig in [&disabled, &paused, &gated, &failed, &page, &zero] {
        assert!(
            rig.location.requests().is_empty(),
            "no read outside a running panel"
        );
    }
}

#[test]
fn only_the_live_bar_window_may_ask_for_weather() {
    let rig = Rig::weather_rig();
    let label = rig.label();
    let guide = rig.open_guide();
    for foreign in [
        guide.as_str(),
        "stache-test-1",
        "stache-access-1",
        "main",
        "shell",
        "",
        "stache-bar-",
        "stache-bar-x",
        "stache-bar-999",
        "STACHE-BAR-1",
    ] {
        if foreign == label {
            continue;
        }
        assert_eq!(
            rig.bridge.weather(foreign),
            BarWeatherOutcome::Unauthorized,
            "{foreign:?}"
        );
    }
    // A stale label after a panel reopen.
    rig.control.pause(&rig.registry).expect("pause");
    rig.control.resume(&rig.registry).expect("resume");
    rig.wait_opened(2);
    rig.wait("the new panel", |rig| {
        rig.windows.live_label(BarConfig::ID).is_some_and(|new| new != label)
    });
    assert_eq!(rig.bridge.weather(&label), BarWeatherOutcome::Unauthorized);
    assert!(
        rig.location.requests().is_empty(),
        "no unauthorized call read anything"
    );
    assert!(matches!(rig.weather(), BarWeatherOutcome::Ready { .. }));
}

#[test]
fn stopping_bar_during_a_blocked_read_is_stale_and_has_no_later_effect() {
    let rig = Rig::weather_rig();
    let label = rig.label();
    rig.location.hold(true);
    let bridge = Arc::clone(&rig.bridge);
    let asked = label.clone();
    let call = thread::spawn(move || bridge.weather(&asked));
    assert!(rig.location.wait_entered(1));
    rig.control.pause(&rig.registry).expect("pause");
    rig.location.hold(false);
    assert_eq!(call.join().expect("call"), BarWeatherOutcome::Stale);
    assert_eq!(
        rig.bar().weather,
        stache_bar::WeatherStatus::default(),
        "nothing was recorded"
    );
    assert_eq!(
        unavailable_weather(&rig.bridge.weather(&label)),
        Some(BarUnavailableReason::Paused)
    );
    // A restart inherits nothing: the next call reads afresh.
    rig.control.resume(&rig.registry).expect("resume");
    rig.wait_opened(2);
    rig.wait("the new panel", |rig| {
        rig.windows.live_label(BarConfig::ID).is_some_and(|new| new != label)
    });
    assert_eq!(rig.bar().weather, stache_bar::WeatherStatus::default());
    let reads = rig.location.requests().len();
    let _ = weather_of(rig.weather());
    assert_eq!(rig.location.requests().len(), reads + 1);
}

#[test]
fn a_hung_location_read_never_blocks_bridge_reads_or_shutdown() {
    let rig = Rig::weather_rig();
    let label = rig.label();
    rig.location.hold(true);
    let bridge = Arc::clone(&rig.bridge);
    let asked = label.clone();
    let call = thread::spawn(move || bridge.weather(&asked));
    assert!(rig.location.wait_entered(1));
    for _ in 0..20 {
        let begun = Instant::now();
        assert!(matches!(
            rig.bridge.snapshot(&label),
            BarSnapshotOutcome::Available { .. }
        ));
        assert!(begun.elapsed() < Duration::from_secs(2), "{:?}", begun.elapsed());
    }
    let begun = Instant::now();
    rig.factory.begin_shutdown();
    let _ = rig.registry.shutdown();
    assert_eq!(rig.factory.finish_shutdown(&Wait), Ok(true));
    assert!(begun.elapsed() < Duration::from_secs(3), "{:?}", begun.elapsed());
    rig.location.hold(false);
    assert_eq!(call.join().expect("call"), BarWeatherOutcome::Stale);
    assert_eq!(rig.location.requests().len(), 1);
}

#[test]
fn a_sentinel_key_and_sentinel_coordinates_appear_only_in_the_weather_response() {
    let rig = Rig::weather_rig();
    let outcome = rig.weather();
    let response = serde_json::to_string(&outcome).expect("response");
    // The positive control: the response is where they live.
    assert!(response.contains(SENTINEL_KEY), "{response}");
    assert!(
        response.contains("12.345678") && response.contains("-76.54321"),
        "{response}"
    );
    // Let every observer run, with a registry reload on top.
    rig.write(&format!("{WEATHER_BAR}precise_location = true\n"));
    let _ = rig.weather();
    rig.toggle_menu(true);
    rig.wait_menu(MenuBarState::Visible);
    thread::sleep(Duration::from_millis(60));

    let view = bar_module_view(&rig);
    let host = crate::bindings::HostStatus {
        modules: vec![view.clone()],
        ..crate::bindings::HostStatus::default()
    };
    let public: Vec<(&str, String)> = vec![
        ("BarView", serde_json::to_string(&rig.available()).expect("view")),
        (
            "BarSnapshotOutcome",
            serde_json::to_string(&rig.snapshot()).expect("snapshot"),
        ),
        (
            "BarChanged",
            serde_json::to_string(
                &rig.sink.events().into_iter().map(|(_, e)| e).collect::<Vec<_>>(),
            )
            .expect("events"),
        ),
        (
            "BarRuntimeStatus",
            serde_json::to_string(&rig.bar()).expect("status"),
        ),
        ("BarRuntimeStatus debug", format!("{:?}", rig.bar())),
        (
            "BarPublished debug",
            format!("{:?}", rig.factory.status().published()),
        ),
        (
            "ModuleStatusView",
            serde_json::to_string(&view).expect("module view"),
        ),
        (
            "StatusChanged",
            serde_json::to_string(&crate::bindings::StatusChanged { status: host }).expect("event"),
        ),
        ("outcome debug", format!("{outcome:?}")),
        ("weather debug", format!("{:?}", weather_of(rig.weather()))),
        ("logs", format!("{:?}", lock(&rig.logs.0))),
    ];
    for (name, text) in &public {
        for secret in [SENTINEL_KEY, "12.345678", "76.54321", "12.3456", "7f3a91c2"] {
            assert!(!text.contains(secret), "{name} leaked {secret}: {text}");
        }
    }
}

/// The overlay `host_status` publishes for Bar, from the real factory.
fn bar_module_view(rig: &Rig) -> crate::bindings::ModuleStatusView {
    let mut view = crate::bindings::ModuleStatusView {
        id: BarConfig::ID.as_str().to_owned(),
        state: ModuleState::Running,
        enabled: true,
        paused: false,
        revision: "1".to_owned(),
        detail: None,
        diagnostic: None,
        keybindings: None,
        tiling: None,
        bar: None,
    };
    assert!(rig.factory.project_status(&mut view));
    assert!(view.bar.as_ref().is_some_and(|bar| bar.weather.location.is_some()));
    view
}

#[test]
fn an_unreadable_key_file_is_absent_with_a_redacted_diagnostic_and_open_meteo_reads_none() {
    let rig = Rig::weather_rig();
    std::fs::remove_file(rig.config.directory().join("keys.env")).expect("remove");
    let weather = weather_of(rig.weather());
    assert_eq!(weather.api_key, None);
    let note = rig.bar().weather.api_key.expect("a diagnostic");
    assert_eq!(note.problem, stache_bar::ApiKeyProblem::Missing);
    assert!(note.path.is_some_and(|path| path.ends_with("keys.env")));
    rig.write_keys(&format!("VISUAL_CROSSING_API_KEY={SENTINEL_KEY}\n"));
    rig.write(&WEATHER_BAR.replace("auto", "open_meteo"));
    let weather = weather_of(rig.weather());
    assert_eq!(weather.provider, BarWeatherProvider::OpenMeteo);
    assert_eq!(weather.api_key, None, "Open Meteo never receives the key");
}

impl Rig {
    /// The bumped epoch carried by an event emitted after `skip` events, if
    /// one arrived (either in the carried view or by a resnapshot request).
    fn epoch_event_after(&self, skip: usize) -> bool {
        let events = self.sink.events();
        events.iter().skip(skip).any(|(_, event)| match &event.outcome {
            Some(BarSnapshotOutcome::Available { view }) => view.weather_epoch != "0",
            _ => event.resnapshot,
        })
    }
}

#[test]
fn e2e_a_consent_decided_after_the_timeout_refreshes_the_weather_with_the_real_location() {
    let rig = Rig::weather_rig();
    assert_eq!(rig.top(1), Some(42.0));
    rig.location.fake.set(LocationOutcome::Unavailable(
        LocationUnavailable::NotDeterminedTimeout,
    ));
    let first = weather_of(rig.weather());
    assert_eq!(first.location, BarWeatherLocation::DefaultLocation {
        query: "Lisbon".into()
    });
    assert!(
        rig.location.fake.watcher_pending(),
        "one manager waits for the decision"
    );
    let epoch = rig.available().weather_epoch;
    assert_eq!(epoch, "0");
    let events = rig.sink.events().len();

    // The user answers the prompt: the real location is now available.
    rig.location.fake.set(sentinel_fix());
    assert!(rig.location.fake.decide_consent());
    rig.wait("a BarChanged with the bumped epoch", |rig| {
        rig.epoch_event_after(events)
    });
    let view = rig.available();
    assert_eq!(view.weather_epoch, "1");
    assert!(!rig.location.fake.watcher_pending(), "the manager is released");

    let second = weather_of(rig.weather());
    assert_eq!(second.location, BarWeatherLocation::CoreLocation {
        latitude: SENTINEL_LATITUDE,
        longitude: SENTINEL_LONGITUDE,
        reduced_accuracy: true,
    });
    assert_eq!(
        rig.location.requests().len(),
        2,
        "no repeated request for the answer"
    );
    assert_eq!(rig.location.fake.watchers_created(), 1);
    // Bar itself and Tiling's view are untouched.
    assert_eq!(rig.top(1), Some(42.0));
    rig.assert_none_absent(&[1, 2]);
}

#[test]
fn e2e_stopping_bar_with_a_pending_consent_releases_the_manager_and_emits_nothing() {
    let rig = Rig::weather_rig();
    rig.location.fake.set(LocationOutcome::Unavailable(
        LocationUnavailable::NotDeterminedTimeout,
    ));
    let _ = weather_of(rig.weather());
    assert!(rig.location.fake.watcher_pending());
    rig.control.pause(&rig.registry).expect("pause");
    assert!(
        !rig.location.fake.watcher_pending(),
        "zero managers when Bar is not running"
    );
    assert_eq!(rig.location.fake.watchers_cancelled(), 1);
    thread::sleep(Duration::from_millis(40));
    let events = rig.sink.events().len();
    let epoch = rig.factory.status().published().weather_epoch;
    // A late decision changes nothing.
    assert!(!rig.location.fake.decide_consent());
    thread::sleep(Duration::from_millis(60));
    assert_eq!(rig.sink.events().len(), events, "no republish after stop");
    assert_eq!(rig.factory.status().published().weather_epoch, epoch);
    assert_eq!(rig.location.fake.notifications(), 0);
}

#[test]
fn the_weather_outcome_has_exact_wire_forms() {
    let ready = BarWeatherOutcome::Ready {
        weather: stache_bar::BarWeather {
            config_revision: "3".to_owned(),
            provider: BarWeatherProvider::OpenMeteo,
            api_key: None,
            location: BarWeatherLocation::DefaultLocation { query: "Oslo".to_owned() },
            refresh_interval_minutes: 45,
        },
    };
    for (outcome, json) in [
        (
            ready,
            r#"{"kind":"ready","weather":{"configRevision":"3","provider":"openMeteo","apiKey":null,"location":{"source":"defaultLocation","query":"Oslo"},"refreshIntervalMinutes":45}}"#,
        ),
        (
            BarWeatherOutcome::Unavailable {
                reason: BarUnavailableReason::Paused,
                detail: None,
            },
            r#"{"kind":"unavailable","reason":"paused","detail":null}"#,
        ),
        (BarWeatherOutcome::Unauthorized, r#"{"kind":"unauthorized"}"#),
        (BarWeatherOutcome::Stale, r#"{"kind":"stale"}"#),
    ] {
        assert_eq!(serde_json::to_string(&outcome).expect("serialize"), json);
        assert_eq!(
            serde_json::from_str::<BarWeatherOutcome>(json).expect("round trip"),
            outcome
        );
    }
}

#[test]
fn a_bridge_without_a_weather_service_is_unavailable_and_unauthorized_for_foreign_labels() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    // The rig always installs weather; a bare bridge does not.
    let bare = BarBridge::new(
        Arc::clone(&rig.windows),
        Arc::new(RegistryStates(Arc::clone(&rig.registry))),
        Arc::new(RecordingSink::default()) as Arc<dyn BarEventSink>,
    );
    assert_eq!(
        unavailable_weather(&bare.weather("stache-bar-1")),
        Some(BarUnavailableReason::Stopped),
        "not attached"
    );
    assert_eq!(bare.weather("stache-test-1"), BarWeatherOutcome::Unauthorized);
    let attached = BarBridge::new(
        Arc::clone(&rig.windows),
        Arc::new(RegistryStates(Arc::clone(&rig.registry))),
        Arc::new(RecordingSink::default()) as Arc<dyn BarEventSink>,
    );
    attached.attach(rig.factory.status());
    assert_eq!(
        unavailable_weather(&attached.weather(&rig.label())),
        Some(BarUnavailableReason::Unavailable)
    );
}

// ------------------------------------------------------------------ media

use std::collections::VecDeque;

use base64::Engine as _;
use stache_bar::{MediaParts, MediaPaths, MediaResolver, MediaRunState, MediaTiming};

use crate::bar_actions::{EDGE_DEV, FEISHIN, SPOTIFY, TIDAL};
use crate::bar_bridge::BarArtworkOutcome;

/// One scripted adapter run.
#[derive(Default)]
struct AdapterRun {
    stdout: Vec<u8>,
    /// Streams `stdout` and then waits for a signal; otherwise it exits with
    /// `exit` once the output is read.
    stalls: bool,
    exit: i32,
}

#[derive(Default)]
struct AdapterState {
    stdout: VecDeque<u8>,
    stalls: bool,
    exit: i32,
    ended: Option<i32>,
}

struct AdapterChild {
    state: Arc<Mutex<AdapterState>>,
    signals: Arc<Mutex<Vec<&'static str>>>,
}

impl ChildProcess for AdapterChild {
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        use std::os::unix::process::ExitStatusExt;
        let mut state = lock(&self.state);
        if state.ended.is_none() && !state.stalls && state.stdout.is_empty() {
            state.ended = Some(state.exit << 8);
        }
        Ok(state.ended.map(std::process::ExitStatus::from_raw))
    }

    fn kill(&mut self) -> std::io::Result<()> {
        lock(&self.signals).push("kill");
        lock(&self.state).ended = Some(9);
        Ok(())
    }

    fn terminate(&mut self) -> std::io::Result<()> {
        lock(&self.signals).push("term");
        lock(&self.state).ended = Some(15);
        Ok(())
    }

    #[allow(clippy::significant_drop_tightening)] // one short guard over the scripted state
    fn read_stdout(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let mut state = lock(&self.state);
        if matches!(state.ended, Some(9 | 15)) {
            return Ok(0);
        }
        if state.stdout.is_empty() {
            return if state.stalls {
                Err(std::io::ErrorKind::WouldBlock.into())
            } else {
                Ok(0)
            };
        }
        let count = state.stdout.len().min(buffer.len());
        for slot in &mut buffer[..count] {
            *slot = state.stdout.pop_front().unwrap_or_default();
        }
        Ok(count)
    }

    fn read_stderr(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}

/// The scripted adapter factory: it never spawns perl.
#[derive(Default)]
struct AdapterFactory {
    runs: Mutex<VecDeque<AdapterRun>>,
    spawns: std::sync::atomic::AtomicUsize,
    children: Mutex<Vec<Arc<Mutex<AdapterState>>>>,
    signals: Arc<Mutex<Vec<&'static str>>>,
    programs: Mutex<Vec<SpawnSpec>>,
}

impl ProcessFactory for AdapterFactory {
    fn spawn(&self, spec: &SpawnSpec) -> std::io::Result<Box<dyn ChildProcess>> {
        self.spawns.fetch_add(1, Ordering::SeqCst);
        lock(&self.programs).push(spec.clone());
        let run = lock(&self.runs).pop_front().unwrap_or_else(|| AdapterRun {
            exit: 1,
            ..AdapterRun::default()
        });
        let state = Arc::new(Mutex::new(AdapterState {
            stdout: run.stdout.into(),
            stalls: run.stalls,
            exit: run.exit,
            ended: None,
        }));
        lock(&self.children).push(Arc::clone(&state));
        Ok(Box::new(AdapterChild {
            state,
            signals: Arc::clone(&self.signals),
        }))
    }
}

struct MediaFakes {
    factory: Arc<AdapterFactory>,
    resolver: Mutex<MediaResolver>,
}

fn adapter_paths() -> MediaPaths {
    MediaPaths {
        script: "/fake/media-control/mediaremote-adapter.pl".into(),
        framework: "/fake/media-control/MediaRemoteAdapter.framework".into(),
        test_client: "/fake/media-control/MediaRemoteAdapterTestClient".into(),
    }
}

impl MediaFakes {
    fn new(runs: Vec<AdapterRun>) -> Arc<Self> {
        let factory = Arc::new(AdapterFactory {
            runs: Mutex::new(runs.into()),
            ..AdapterFactory::default()
        });
        let resolver: MediaResolver = Arc::new(|| Ok(adapter_paths()));
        Arc::new(Self {
            factory,
            resolver: Mutex::new(resolver),
        })
    }

    fn unavailable(reason: &'static str) -> Arc<Self> {
        let fakes = Self::new(Vec::new());
        *lock(&fakes.resolver) = Arc::new(move || Err(reason.to_owned()));
        fakes
    }

    fn parts(&self, cache: &std::path::Path) -> MediaParts {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let executor = Arc::new(CommandExecutor::with_parts(
            Arc::clone(&self.factory) as Arc<dyn ProcessFactory>,
            Arc::clone(&clock),
            CommandEnvironment::default(),
        ));
        MediaParts {
            executor,
            clock,
            perl: "/usr/bin/perl".into(),
            resolver: Arc::clone(&lock(&self.resolver)),
            store: Arc::new(stache_bar::ArtworkStore::new(Some(cache.join("media-artwork")))),
            timing: MediaTiming {
                backoff: [Duration::from_millis(15); 6],
                term_grace: Duration::from_millis(200),
                reap_retries: 3,
                reap_retry_wait: Duration::from_millis(10),
                ..MediaTiming::default()
            },
        }
    }

    fn spawns(&self) -> usize { self.factory.spawns.load(Ordering::SeqCst) }

    /// Every child the fake ever created has ended (was reaped by the owner).
    fn all_ended(&self) -> bool {
        lock(&self.factory.children).iter().all(|child| lock(child).ended.is_some())
    }
}

fn adapter_line(payload: &serde_json::Value) -> Vec<u8> {
    let mut line = serde_json::json!({"type": "data", "diff": false, "payload": payload})
        .to_string()
        .into_bytes();
    line.push(b'\n');
    line
}

fn track(bundle: &str, title: &str) -> serde_json::Value {
    serde_json::json!({
        "title": title, "artist": "Band", "album": "LP", "bundleIdentifier": bundle,
        "playing": true, "playbackRate": 1.0, "elapsedTime": 3.5,
    })
}

fn artwork_png() -> Vec<u8> {
    let image = image::RgbaImage::from_fn(200, 100, |x, _| {
        if x < 100 {
            image::Rgba([255, 0, 0, 255])
        } else {
            image::Rgba([0, 0, 255, 255])
        }
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("png");
    out
}

fn playing(bundle: &str, with_art: bool) -> AdapterRun {
    let mut stdout = adapter_line(&serde_json::json!({}));
    stdout.extend(adapter_line(&track(bundle, "Song")));
    if with_art {
        let mut payload = track(bundle, "Song");
        payload["artworkData"] =
            serde_json::json!(base64::engine::general_purpose::STANDARD.encode(artwork_png()));
        payload["artworkMimeType"] = serde_json::json!("image/png");
        stdout.extend(adapter_line(&payload));
    }
    AdapterRun { stdout, stalls: true, exit: 0 }
}

impl Rig {
    fn with_media(file: Option<&str>, fakes: &Arc<MediaFakes>) -> Self {
        Self::build_with(file, both(), SHORT_LOAD, NEVER_RETRY, Some(fakes), None)
    }

    fn media_state(&self) -> Option<MediaRunState> { self.bar().media.map(|media| media.state) }

    fn media_view(&self) -> Option<stache_bar::MediaReading> { self.available().media }
}

#[test]
fn e2e_spotify_with_artwork_reaches_the_view_and_the_artwork_command() {
    let fakes = MediaFakes::new(vec![playing(SPOTIFY, true)]);
    let rig = Rig::with_media(Some(BAR), &fakes);
    rig.start();
    rig.wait_loaded();
    rig.wait("media with artwork", |rig| {
        rig.media_view().is_some_and(|media| media.artwork_id.is_some())
    });
    // bar.toml 28/14 on two displays: Tiling still sees the constant top.
    assert_eq!(rig.top(1), Some(42.0));
    rig.assert_none_absent(&[1, 2]);

    let view = rig.available();
    let media = view.media.expect("media");
    assert_eq!(media.title, "Song");
    assert_eq!(media.artist, "Band");
    assert_eq!(media.album.as_deref(), Some("LP"));
    assert_eq!(media.bundle_identifier, SPOTIFY);
    assert!(media.playing);
    let id = media.artwork_id.clone().expect("artwork id");
    assert_eq!(id.len(), 64);

    // The wire form is pinned.
    assert_eq!(
        serde_json::to_value(&media).expect("media"),
        serde_json::json!({
            "title": "Song", "artist": "Band", "album": "LP", "bundleIdentifier": SPOTIFY,
            "mediaType": null, "playing": true, "artworkId": id,
        })
    );

    // The artwork command returns the 128x128 PNG.
    let BarArtworkOutcome::Ready { artwork } = rig.bridge.media_artwork(&rig.label(), &id) else {
        panic!("expected ready artwork")
    };
    let png = base64::engine::general_purpose::STANDARD.decode(&artwork).expect("base64");
    let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .expect("png")
        .to_rgba8();
    assert_eq!(decoded.dimensions(), (128, 128));
    assert_eq!(
        rig.bridge.media_artwork(&rig.label(), &"0".repeat(64)),
        BarArtworkOutcome::Missing
    );

    // The adapter command: absolute paths, perl, stream, no-diff.
    let spec = lock(&fakes.factory.programs)[0].clone();
    assert_eq!(spec.program, std::path::PathBuf::from("/usr/bin/perl"));
    assert_eq!(spec.arguments.last().map(String::as_str), Some("--no-diff"));
    assert!(spec.arguments.iter().take(3).all(|argument| argument.starts_with('/')));

    // The artwork bytes are in no public payload.
    thread::sleep(Duration::from_millis(40));
    let source = base64::engine::general_purpose::STANDARD.encode(artwork_png());
    let public = [
        serde_json::to_string(&rig.available()).expect("view"),
        serde_json::to_string(&rig.snapshot()).expect("snapshot"),
        serde_json::to_string(&rig.sink.events().into_iter().map(|(_, e)| e).collect::<Vec<_>>())
            .expect("events"),
        serde_json::to_string(&rig.bar()).expect("status"),
        format!("{:?}", rig.bar()),
        format!("{:?}", rig.factory.status().published()),
        serde_json::to_string(&bar_module_view_for_media(&rig)).expect("module"),
        format!("{:?}", lock(&rig.logs.0)),
    ];
    for text in &public {
        assert!(!text.contains(&artwork), "the 128x128 artwork leaked");
        assert!(!text.contains(&source[..64]), "the source artwork leaked");
    }
    // The positive control: the command response is where the bytes live.
    assert!(
        serde_json::to_string(&rig.bridge.media_artwork(&rig.label(), &id))
            .expect("response")
            .contains(&artwork)
    );
    assert_eq!(rig.bar().media.expect("status").state, MediaRunState::Running);
}

fn bar_module_view_for_media(rig: &Rig) -> crate::bindings::ModuleStatusView {
    let mut view = crate::bindings::ModuleStatusView {
        id: BarConfig::ID.as_str().to_owned(),
        state: ModuleState::Running,
        enabled: true,
        paused: false,
        revision: "1".to_owned(),
        detail: None,
        diagnostic: None,
        keybindings: None,
        tiling: None,
        bar: None,
    };
    assert!(rig.factory.project_status(&mut view));
    view
}

#[test]
fn e2e_stopping_bar_terminates_and_reaps_the_exact_child_and_hides_the_media() {
    let fakes = MediaFakes::new(vec![playing(SPOTIFY, true), playing(TIDAL, false)]);
    let rig = Rig::with_media(Some(BAR), &fakes);
    rig.start();
    rig.wait_loaded();
    rig.wait("media", |rig| {
        rig.media_view().is_some_and(|m| m.artwork_id.is_some())
    });
    let stale = rig.label();
    let id = rig.media_view().and_then(|m| m.artwork_id).expect("id");

    rig.control.pause(&rig.registry).expect("pause");
    assert!(
        rig.factory.status().published().media.is_none(),
        "invalidated first"
    );
    assert!(rig.bar().media.is_none());
    rig.wait("the child to be reaped", |_| fakes.all_ended());
    assert_eq!(
        *lock(&fakes.factory.signals),
        ["term"],
        "the exact child, SIGTERM only"
    );
    assert_eq!(fakes.spawns(), 1);
    let sequence = rig.snapshot_sequence();
    thread::sleep(Duration::from_millis(80));
    assert_eq!(
        rig.snapshot_sequence(),
        sequence,
        "no late publish after the stop"
    );
    assert!(rig.factory.status().published().media.is_none());
    assert!(matches!(
        rig.bridge.media_artwork(&stale, &id),
        BarArtworkOutcome::Unavailable { .. }
    ));

    // Resume: a fresh child, and nothing of the old run leaks into it.
    rig.control.resume(&rig.registry).expect("resume");
    rig.wait_loaded();
    rig.wait("the successor's media", |rig| {
        rig.media_view().is_some_and(|m| m.bundle_identifier == TIDAL)
    });
    assert_eq!(fakes.spawns(), 2);
    assert_eq!(rig.media_view().expect("media").artwork_id, None);
    assert_eq!(
        rig.bridge.media_artwork(&stale, &id),
        BarArtworkOutcome::Unauthorized
    );
}

#[test]
fn e2e_a_crash_restarts_the_adapter_and_the_media_recovers() {
    let fakes = MediaFakes::new(vec![
        AdapterRun {
            exit: 1,
            ..AdapterRun::default()
        },
        playing(SPOTIFY, false),
    ]);
    let rig = Rig::with_media(Some(BAR), &fakes);
    rig.start();
    rig.wait_loaded();
    rig.wait("recovery", |rig| rig.media_view().is_some());
    assert_eq!(fakes.spawns(), 2);
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(rig.media_state(), Some(MediaRunState::Running));
    assert!(rig.available().cpu.is_some() || rig.snapshot_sequence() > 0);
}

#[test]
fn e2e_repeated_crashes_report_unavailable_and_hide_media_while_bar_keeps_running() {
    let fakes = MediaFakes::new(Vec::new());
    let rig = Rig::with_media(Some(BAR), &fakes);
    rig.start();
    rig.wait_loaded();
    rig.wait("unavailable", |rig| {
        rig.media_state() == Some(MediaRunState::Unavailable)
    });
    assert!(rig.media_view().is_none(), "Media is hidden");
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(rig.bar().panel, BarPanelState::Loaded);
    assert_eq!(rig.bar().state, BarRuntimeState::Running);
    let status = rig.bar().media.expect("status");
    assert!(status.reason.is_some_and(|reason| reason.contains("exited with code 1")));
    rig.wait("providers still sampling", |rig| rig.available().cpu.is_some());
    // The retry loop is paced by the backoff, not a spin.
    let spawns = fakes.spawns();
    thread::sleep(Duration::from_millis(100));
    assert!(fakes.spawns() - spawns <= 12, "{} -> {}", spawns, fakes.spawns());
}

#[test]
fn e2e_unavailable_resources_spawn_nothing_and_say_why() {
    let fakes = MediaFakes::unavailable("the media adapter supports arm64 only");
    let rig = Rig::with_media(Some(BAR), &fakes);
    rig.start();
    rig.wait_loaded();
    rig.wait("the diagnostic", |rig| {
        rig.media_state() == Some(MediaRunState::Unavailable)
    });
    thread::sleep(Duration::from_millis(60));
    assert_eq!(fakes.spawns(), 0);
    assert!(rig.media_view().is_none());
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(
        rig.bar().media.and_then(|media| media.reason).as_deref(),
        Some("the media adapter supports arm64 only")
    );
}

#[test]
fn a_disabled_zero_size_or_gated_bar_owns_no_media_worker_or_child() {
    for file in [None, Some("enabled = true\nheight = 0\npadding = 14\n")] {
        let fakes = MediaFakes::new(vec![playing(SPOTIFY, false)]);
        let rig = Rig::with_media(file, &fakes);
        rig.start();
        thread::sleep(Duration::from_millis(80));
        assert_eq!(fakes.spawns(), 0, "{file:?}");
        assert!(rig.bar().media.is_none(), "{file:?}");
    }
    // Permission revoked: the panel and the media go away together.
    let fakes = MediaFakes::new(vec![playing(SPOTIFY, false)]);
    let rig = Rig::with_media(Some(BAR), &fakes);
    rig.start();
    rig.wait_loaded();
    rig.wait("media", |rig| rig.media_view().is_some());
    rig.trust.0.store(false, Ordering::SeqCst);
    rig.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
        .expect("revoke");
    rig.wait("the child to be reaped", |_| fakes.all_ended());
    assert!(rig.factory.status().published().media.is_none());
}

#[test]
fn e2e_a_media_click_opens_the_fixed_player_and_unknown_or_absent_media_is_inert() {
    for (bundle, path) in [
        (SPOTIFY, "/fake/Spotify.app"),
        (EDGE_DEV, "/fake/Microsoft Edge Dev.app"),
        (TIDAL, "/fake/TIDAL.app"),
        (FEISHIN, "/fake/Feishin.app"),
    ] {
        let fakes = MediaFakes::new(vec![playing(bundle, false)]);
        let rig = Rig::with_media(Some(BAR), &fakes);
        rig.apps.set_application_path(bundle, Some(path.into()));
        rig.start();
        rig.wait_loaded();
        rig.wait("media", |rig| rig.media_view().is_some());
        assert_eq!(
            rig.bridge.action(&rig.label(), BarAction::Media),
            BarActionOutcome::Completed,
            "{bundle}"
        );
        assert_eq!(
            rig.apps.open_requests(),
            [std::path::PathBuf::from(path)],
            "{bundle}"
        );
    }
    for (name, run) in [
        ("unknown player", playing("com.apple.Music", false)),
        ("nothing playing", AdapterRun {
            stdout: adapter_line(&serde_json::json!({})),
            stalls: true,
            exit: 0,
        }),
    ] {
        let fakes = MediaFakes::new(vec![run]);
        let rig = Rig::with_media(Some(BAR), &fakes);
        rig.apps.set_application_path("com.apple.Music", Some("/fake/Music.app".into()));
        rig.start();
        rig.wait_loaded();
        rig.wait("the adapter", |_| fakes.spawns() == 1);
        thread::sleep(Duration::from_millis(40));
        assert_eq!(
            rig.bridge.action(&rig.label(), BarAction::Media),
            BarActionOutcome::NotAttempted,
            "{name}"
        );
        assert!(rig.apps.open_requests().is_empty(), "{name}");
    }
}

#[test]
fn only_the_live_bar_window_may_fetch_artwork_and_bad_ids_are_missing() {
    let fakes = MediaFakes::new(vec![playing(SPOTIFY, true)]);
    let rig = Rig::with_media(Some(BAR), &fakes);
    rig.start();
    rig.wait_loaded();
    rig.wait("artwork", |rig| {
        rig.media_view().is_some_and(|m| m.artwork_id.is_some())
    });
    let id = rig.media_view().and_then(|m| m.artwork_id).expect("id");
    let label = rig.label();
    let guide = rig.open_guide();
    for foreign in [
        guide.as_str(),
        "stache-test-1",
        "stache-access-1",
        "main",
        "shell",
        "",
        "stache-bar-",
        "stache-bar-x",
        "stache-bar-999",
        "STACHE-BAR-1",
    ] {
        if foreign == label {
            continue;
        }
        assert_eq!(
            rig.bridge.media_artwork(foreign, &id),
            BarArtworkOutcome::Unauthorized,
            "{foreign:?}"
        );
    }
    for bad in [
        "",
        "../secret",
        "..",
        "/etc/passwd",
        &id.to_uppercase(),
        &id[..63],
        &format!("{id}0"),
        &format!("{}/", &id[..63]),
        "zz",
    ] {
        assert_eq!(
            rig.bridge.media_artwork(&label, bad),
            BarArtworkOutcome::Missing,
            "{bad:?}"
        );
    }
    assert!(matches!(
        rig.bridge.media_artwork(&label, &id),
        BarArtworkOutcome::Ready { .. }
    ));
}

#[test]
fn a_bridge_without_a_media_store_has_no_artwork() {
    let rig = Rig::new(Some(BAR));
    rig.start();
    rig.wait_loaded();
    assert!(matches!(
        rig.bridge.media_artwork(&rig.label(), &"a".repeat(64)),
        BarArtworkOutcome::Unavailable { .. }
    ));
    assert_eq!(rig.bar().media, None, "no media worker, no media status");
    assert_eq!(rig.available().media, None);
}

#[test]
fn the_artwork_outcome_has_exact_wire_forms() {
    for (outcome, json) in [
        (
            BarArtworkOutcome::Ready { artwork: "QUJD".to_owned() },
            r#"{"kind":"ready","artwork":"QUJD"}"#,
        ),
        (BarArtworkOutcome::Missing, r#"{"kind":"missing"}"#),
        (BarArtworkOutcome::Unauthorized, r#"{"kind":"unauthorized"}"#),
        (
            BarArtworkOutcome::Unavailable {
                reason: BarUnavailableReason::Paused,
                detail: None,
            },
            r#"{"kind":"unavailable","reason":"paused","detail":null}"#,
        ),
    ] {
        assert_eq!(serde_json::to_string(&outcome).expect("serialize"), json);
        assert_eq!(
            serde_json::from_str::<BarArtworkOutcome>(json).expect("round trip"),
            outcome
        );
    }
}

#[test]
fn production_media_parts_use_the_next_cache_directory_and_the_vendored_resources() {
    let temp = tempfile::tempdir().expect("temp");
    let parts = crate::media_resources::media_parts(None, Some(temp.path()));
    let directory = parts.store.directory().expect("directory");
    assert_eq!(directory, temp.path().join("media-artwork"));
    assert_ne!(
        directory.file_name().and_then(|name| name.to_str()),
        Some("media_artwork"),
        "the legacy cache directory is never shared"
    );
    assert!(!directory.exists(), "constructing the parts creates nothing");
    assert_eq!(parts.perl, std::path::PathBuf::from("/usr/bin/perl"));
    let resolved = (parts.resolver)();
    if cfg!(target_arch = "aarch64") {
        let paths = resolved.expect("the vendored resources resolve in a debug build");
        for path in [&paths.script, &paths.framework, &paths.test_client] {
            assert!(
                path.is_absolute() && !path.to_string_lossy().contains("_legacy"),
                "{path:?}"
            );
        }
    } else {
        assert!(resolved.is_err());
    }
    // No cache directory: memory only, nothing on disk.
    assert_eq!(
        crate::media_resources::media_parts(None, None).store.directory(),
        None
    );
}

// -------------------------------------------------------------- Keep Awake
//
// Keep Awake runs for real (the production module and factory) over the native
// fakes: no power assertion is ever created and no lock state is ever changed.

use stache_keep_awake::KeepAwakeConfig;
use stache_macos::{
    AssertionOwner, FakeAssertionBackend, FakeLockObservationBackend, FakeLockProbe,
    LockObservationInstaller, LockState as NativeLockState,
};

use crate::bar_keep_awake::KeepAwakeIndicator;
use crate::host::KeepAwakeFactory;
use crate::session_action::{
    self, SessionAction, SessionActionOutcome, SessionControl, SessionUnavailable,
};

const KEEP_AWAKE_ON: &str = "enabled = true\n";

struct KeepAwakeKit {
    control: Arc<dyn ModuleControl>,
    assertion: FakeAssertionBackend,
    probe: FakeLockProbe,
    observation: FakeLockObservationBackend,
}

/// Stands in for the host runner: it serializes requests, executes them with
/// the production `session_action::execute` over the same module controls and
/// then signals the host status sink like the runner's publication does.
struct RigHost {
    registry: Arc<Registry>,
    controls: BTreeMap<String, Arc<dyn ModuleControl>>,
    bridge: Weak<BarBridge>,
    runner: Mutex<()>,
}

impl SessionControl for RigHost {
    fn session_action(&self, module: &str, action: SessionAction) -> SessionActionOutcome {
        let _serial = lock(&self.runner);
        let outcome = session_action::execute(&self.registry, &self.controls, module, action);
        if let Some(bridge) = self.bridge.upgrade() {
            bridge.host_changed();
        }
        outcome
    }
}

const fn indicator(active: bool, paused: bool, enabled: bool) -> KeepAwakeIndicator {
    KeepAwakeIndicator { active, paused, enabled }
}

impl Rig {
    /// Registers the production Keep Awake module over fakes, locked at first.
    fn add_keep_awake(&self, file: Option<&str>) -> KeepAwakeKit {
        if let Some(file) = file {
            std::fs::write(self.config.module_file(KeepAwakeConfig::ID), file).expect("keepAwake");
        }
        let assertion = FakeAssertionBackend::new();
        let probe = FakeLockProbe::new(NativeLockState::Locked);
        let observation = FakeLockObservationBackend::new();
        let factory = KeepAwakeFactory::new(
            AssertionOwner::new(Arc::new(assertion.clone())),
            Arc::new(probe.clone()),
            LockObservationInstaller::new(
                Arc::clone(&self.main) as Arc<dyn MainThreadExecutor>,
                Arc::new(observation.clone()),
            ),
        );
        let control = factory.register(&self.registry, &self.store).expect("register keepAwake");
        self.bridge.set_session(Arc::new(RigHost {
            registry: Arc::clone(&self.registry),
            controls: BTreeMap::from([(
                KeepAwakeConfig::ID.as_str().to_owned(),
                Arc::clone(&control),
            )]),
            bridge: Arc::downgrade(&self.bridge),
            runner: Mutex::new(()),
        }));
        KeepAwakeKit {
            control,
            assertion,
            probe,
            observation,
        }
    }

    fn keep_awake(&self) -> Option<KeepAwakeIndicator> { self.available().keep_awake }

    fn wait_keep_awake(&self, expected: KeepAwakeIndicator) {
        self.wait(&format!("keepAwake {expected:?}"), |rig| {
            rig.keep_awake() == Some(expected)
        });
    }

    fn keep_awake_snapshot(&self) -> stache_sdk::ModuleSnapshot {
        self.registry
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.id == KeepAwakeConfig::ID)
            .expect("keepAwake snapshot")
    }
}

/// The user's setup (bar.toml 28/14, two displays, Tiling gaps 14/10) with
/// Keep Awake running on fakes: the view follows the fake assertion.
#[test]
fn e2e_keep_awake_follows_adoption_and_release_through_the_bridge() {
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    // Registered and enabled but not started: the legacy loading branch.
    assert_eq!(rig.bridge.keep_awake_view().indicator, None);
    rig.start();
    rig.wait_loaded();
    assert_eq!(rig.status(), ModuleStatus::Running);

    // Locked at start: running, unlocked-lease absent, inactive.
    rig.wait_keep_awake(indicator(false, false, true));
    assert!(
        kit.assertion.created().is_empty(),
        "locked: nothing was acquired"
    );
    let mut held = sequence_of(&rig.available());

    // The fake assertion is adopted.
    kit.probe.set(NativeLockState::Unlocked);
    assert!(kit.observation.deliver());
    rig.wait_keep_awake(indicator(true, false, true));
    assert_eq!(kit.assertion.created().len(), 3);
    let active = sequence_of(&rig.available());
    assert!(active > held, "a Keep Awake change is a newer sequence");
    held = active;

    // Release is in flight: never green while releasePending.
    let release = kit.assertion.block_next_release();
    kit.probe.set(NativeLockState::Locked);
    assert!(kit.observation.deliver());
    assert!(release.wait_entered(Duration::from_secs(5)));
    rig.wait_keep_awake(indicator(false, false, true));
    assert!(release.release());
    rig.wait("the lease to be released", |_| {
        kit.assertion.release_attempts().len() == 3
    });
    assert!(sequence_of(&rig.available()) > held);

    // Acquisition in flight: never green while acquiring.
    kit.probe.set(NativeLockState::Unlocked);
    let create = kit.assertion.block_next_create();
    assert!(kit.observation.deliver());
    assert!(create.wait_entered(Duration::from_secs(5)));
    assert_eq!(rig.keep_awake(), Some(indicator(false, false, true)));
    assert!(create.release());
    rig.wait_keep_awake(indicator(true, false, true));

    // The events carried both states, strictly increasing sequences.
    rig.wait("the active event", |rig| {
        rig.sink.events().iter().any(|(_, event)| {
            matches!(&event.outcome, Some(BarSnapshotOutcome::Available { view })
                if view.keep_awake == Some(indicator(true, false, true)))
        })
    });
    let sequences: Vec<u64> = rig
        .sink
        .events()
        .iter()
        .filter_map(|(_, event)| event.sequence.parse().ok())
        .collect();
    assert!(
        sequences.windows(2).all(|pair| pair[0] < pair[1]),
        "{sequences:?}"
    );

    // Tiling is untouched: 28/14 on the primary, gaps 14/10.
    assert_eq!(rig.top(1), Some(42.0));
    rig.assert_none_absent(&[1, 2]);
    assert!((usable_top(&rig, 1, 982.0, 38.0, 14.0) - 56.0).abs() < f64::EPSILON);
    assert!((usable_top(&rig, 2, 1440.0, 0.0, 10.0) - 10.0).abs() < f64::EPSILON);
}

#[test]
fn e2e_pausing_and_disabling_keep_awake_report_host_session_facts() {
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    kit.probe.set(NativeLockState::Unlocked);
    rig.start();
    rig.wait_loaded();
    rig.wait_keep_awake(indicator(true, false, true));
    let before = sequence_of(&rig.available());

    kit.control.pause(&rig.registry).expect("pause");
    rig.bridge.host_changed();
    rig.wait_keep_awake(indicator(false, true, true));
    assert_eq!(rig.status(), ModuleStatus::Running, "Bar keeps running");
    assert!(sequence_of(&rig.available()) > before);

    kit.control.resume(&rig.registry).expect("resume");
    rig.bridge.host_changed();
    rig.wait_keep_awake(indicator(true, false, true));

    kit.control.set_enabled(&rig.registry, false).expect("disable");
    rig.bridge.host_changed();
    rig.wait_keep_awake(indicator(false, false, false));
    assert_eq!(rig.status(), ModuleStatus::Running, "Bar keeps running");
    assert!(kit.assertion.release_attempts().len() >= 3);
}

#[test]
fn e2e_an_absent_or_disabled_keep_awake_never_stops_bar() {
    // Never registered: resolved to inactive, inert.
    let absent = Rig::new(Some(BAR));
    absent.start();
    absent.wait_loaded();
    assert_eq!(absent.status(), ModuleStatus::Running);
    assert_eq!(absent.keep_awake(), Some(indicator(false, false, false)));

    // Registered but disabled by default.
    let disabled = Rig::new(Some(BAR));
    let _kit = disabled.add_keep_awake(None);
    disabled.start();
    disabled.wait_loaded();
    assert_eq!(disabled.status(), ModuleStatus::Running);
    assert_eq!(disabled.keep_awake(), Some(indicator(false, false, false)));
}

#[test]
fn e2e_a_failed_keep_awake_start_is_inactive_and_bar_keeps_running() {
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    kit.probe.set(NativeLockState::Unknown(
        stache_macos::LockUnknown::SessionDictionaryUnavailable,
    ));
    rig.start();
    rig.wait_loaded();
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert!(matches!(
        rig.keep_awake_snapshot().status,
        ModuleStatus::Failed(_)
    ));
    rig.wait_keep_awake(indicator(false, false, true));
    assert!(kit.assertion.created().is_empty());
}

/// Malformed TOML, a wrong boolean, deletion and an identical-good repair
/// through the normal reload keep the last accepted state and revision.
#[test]
fn e2e_rejected_keep_awake_files_retain_the_accepted_state_and_revision() {
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    kit.probe.set(NativeLockState::Unlocked);
    rig.start();
    rig.wait_loaded();
    rig.wait_keep_awake(indicator(true, false, true));
    let revision = rig.keep_awake_snapshot().revision;
    let projection = rig.bridge.keep_awake_view();
    let file = rig.config.module_file(KeepAwakeConfig::ID);

    // The host sink would signal after a reload; nothing may change anyway.
    let settle = || {
        rig.bridge.host_changed();
        thread::sleep(Duration::from_millis(150));
    };
    for bad in ["enabled = \n[", "enabled = \"yes\"\n"] {
        std::fs::write(&file, bad).expect("write");
        let _ = rig.store.reload_one(KeepAwakeConfig::ID);
        settle();
        assert_eq!(rig.keep_awake(), Some(indicator(true, false, true)), "{bad}");
        assert_eq!(rig.keep_awake_snapshot().revision, revision);
        assert!(rig.keep_awake_snapshot().enabled);
        assert_eq!(rig.bridge.keep_awake_view(), projection, "nothing changed");
    }
    std::fs::remove_file(&file).expect("delete");
    let _ = rig.store.reload_one(KeepAwakeConfig::ID);
    settle();
    assert_eq!(rig.keep_awake(), Some(indicator(true, false, true)));
    assert_eq!(rig.keep_awake_snapshot().revision, revision);
    assert_eq!(rig.bridge.keep_awake_view(), projection);

    // The identical good bytes repair the file without a new revision.
    std::fs::write(&file, KEEP_AWAKE_ON).expect("repair");
    let _ = rig.store.reload_one(KeepAwakeConfig::ID);
    settle();
    assert_eq!(rig.keep_awake(), Some(indicator(true, false, true)));
    assert_eq!(rig.keep_awake_snapshot().revision, revision);
    assert_eq!(rig.bridge.keep_awake_view(), projection);
    assert_eq!(rig.status(), ModuleStatus::Running);
}

#[test]
fn e2e_the_keep_awake_watcher_starts_with_bar_and_joins_at_shutdown() {
    // A disabled Bar never starts the watcher thread.
    let off = Rig::new(None);
    let _kit = off.add_keep_awake(Some(KEEP_AWAKE_ON));
    off.start();
    assert!(off.bridge.finish_shutdown());

    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    rig.start();
    rig.wait_loaded();
    rig.wait_keep_awake(indicator(false, false, true));
    kit.probe.set(NativeLockState::Unlocked);
    assert!(kit.observation.deliver());
    rig.wait_keep_awake(indicator(true, false, true));
    assert!(
        rig.bridge.finish_shutdown(),
        "the watcher joins within its bound"
    );
}

// ------------------------------------------------- Keep Awake session click

const KEEP_AWAKE_OFF: &str = "enabled = false\n";

const fn unavailable_session(reason: SessionUnavailable) -> SessionActionOutcome {
    SessionActionOutcome::Unavailable { reason }
}

impl Rig {
    /// The click the Bar sends: the real bridge authorization, then the host.
    fn session(&self, action: SessionAction) -> SessionActionOutcome {
        self.bridge.session_action(&self.label(), KeepAwakeConfig::ID.as_str(), action)
    }

    /// A delivered `BarChanged` already carries `expected` (the push path, not
    /// a re-read of the snapshot).
    fn wait_keep_awake_event(&self, expected: KeepAwakeIndicator) {
        self.wait(&format!("a keepAwake event {expected:?}"), |rig| {
            rig.sink.events().iter().any(|(_, event)| {
                matches!(&event.outcome, Some(BarSnapshotOutcome::Available { view })
                    if view.keep_awake == Some(expected))
            })
        });
    }
}

/// The user's setup (bar.toml 28/14, two displays, Tiling gaps 14/10): a click
/// pauses Keep Awake, the Bar redraws from the pushed event, a second click
/// resumes, and the Bar never stops.
#[test]
fn e2e_clicking_keep_awake_pauses_then_resumes_through_the_pushed_view() {
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    kit.probe.set(NativeLockState::Unlocked);
    rig.start();
    rig.wait_loaded();
    rig.wait_keep_awake_event(indicator(true, false, true));
    let released = kit.assertion.release_attempts().len();

    assert_eq!(rig.session(SessionAction::Pause), SessionActionOutcome::Applied);
    rig.wait_keep_awake_event(indicator(false, true, true));
    assert!(
        kit.assertion.release_attempts().len() > released,
        "pausing released the lease"
    );
    assert!(rig.keep_awake_snapshot().paused);
    assert!(rig.keep_awake_snapshot().enabled, "no enable/disable");
    assert_eq!(rig.status(), ModuleStatus::Running, "Bar never stops");

    assert_eq!(rig.session(SessionAction::Resume), SessionActionOutcome::Applied);
    rig.wait("the resumed active event", |rig| {
        rig.sink.events().last().is_some_and(|(_, event)| {
            matches!(&event.outcome, Some(BarSnapshotOutcome::Available { view })
                if view.keep_awake == Some(indicator(true, false, true)))
        })
    });
    assert!(!rig.keep_awake_snapshot().paused);
    assert_eq!(rig.status(), ModuleStatus::Running);

    assert_eq!(rig.top(1), Some(42.0));
    rig.assert_none_absent(&[1, 2]);
    assert!((usable_top(&rig, 1, 982.0, 38.0, 14.0) - 56.0).abs() < f64::EPSILON);
    assert!((usable_top(&rig, 2, 1440.0, 0.0, 10.0) - 10.0).abs() < f64::EPSILON);
}

#[test]
fn a_running_but_locked_keep_awake_still_pauses_and_resumes() {
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    rig.start();
    rig.wait_loaded();
    rig.wait_keep_awake_event(indicator(false, false, true));
    assert_eq!(rig.session(SessionAction::Pause), SessionActionOutcome::Applied);
    rig.wait_keep_awake_event(indicator(false, true, true));
    assert_eq!(rig.session(SessionAction::Resume), SessionActionOutcome::Applied);
    assert!(!rig.keep_awake_snapshot().paused);
    assert!(
        kit.assertion.created().is_empty(),
        "locked: nothing was ever acquired"
    );
}

#[test]
fn a_disabled_or_absent_keep_awake_click_is_inert_and_never_enables() {
    // Disabled by an explicit file: the bytes, revision and flag never move.
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_OFF));
    kit.probe.set(NativeLockState::Unlocked);
    rig.start();
    rig.wait_loaded();
    let file = rig.config.module_file(KeepAwakeConfig::ID);
    let bytes = std::fs::read(&file).expect("file");
    let revision = rig.keep_awake_snapshot().revision;
    for action in [
        SessionAction::Pause,
        SessionAction::Resume,
        SessionAction::Pause,
    ] {
        assert_eq!(rig.session(action), SessionActionOutcome::Disabled);
    }
    assert_eq!(std::fs::read(&file).expect("file"), bytes, "zero config writes");
    let snapshot = rig.keep_awake_snapshot();
    assert_eq!(
        (snapshot.enabled, snapshot.paused, snapshot.revision),
        (false, false, revision)
    );
    assert!(kit.assertion.created().is_empty());
    assert_eq!(rig.status(), ModuleStatus::Running);

    // Disabled by default (no file at all): still no file is created.
    let none = Rig::new(Some(BAR));
    let _kit = none.add_keep_awake(None);
    none.start();
    none.wait_loaded();
    assert_eq!(
        none.session(SessionAction::Pause),
        SessionActionOutcome::Disabled
    );
    assert_eq!(
        none.session(SessionAction::Resume),
        SessionActionOutcome::Disabled
    );
    assert!(!none.config.module_file(KeepAwakeConfig::ID).exists());
    assert!(!none.keep_awake_snapshot().enabled);

    // Absent: no Keep Awake host at all.
    let absent = Rig::new(Some(BAR));
    absent.start();
    absent.wait_loaded();
    assert_eq!(
        absent.session(SessionAction::Pause),
        unavailable_session(SessionUnavailable::NotRunning)
    );
    assert_eq!(absent.status(), ModuleStatus::Running);
}

#[test]
fn a_failed_keep_awake_returns_a_typed_non_success_and_acquires_nothing() {
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    kit.probe.set(NativeLockState::Unknown(
        stache_macos::LockUnknown::SessionDictionaryUnavailable,
    ));
    rig.start();
    rig.wait_loaded();
    assert!(matches!(
        rig.keep_awake_snapshot().status,
        ModuleStatus::Failed(_)
    ));
    for action in [SessionAction::Pause, SessionAction::Resume] {
        let outcome = rig.session(action);
        // Resume of a module that is not paused is stale; Pause of a failed one
        // is a typed non-success with no retry.
        match action {
            SessionAction::Pause => {
                assert_eq!(outcome, unavailable_session(SessionUnavailable::Failed));
            }
            SessionAction::Resume => assert_eq!(outcome, SessionActionOutcome::Stale),
        }
    }
    assert!(kit.assertion.created().is_empty(), "no new acquisition");
    assert!(matches!(
        rig.keep_awake_snapshot().status,
        ModuleStatus::Failed(_)
    ));
}

#[test]
fn a_release_pending_keep_awake_is_not_paused_or_retried() {
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    kit.probe.set(NativeLockState::Unlocked);
    rig.start();
    rig.wait_loaded();
    rig.wait_keep_awake_event(indicator(true, false, true));

    let release = kit.assertion.block_next_release();
    kit.probe.set(NativeLockState::Locked);
    assert!(kit.observation.deliver());
    assert!(release.wait_entered(Duration::from_secs(5)));
    rig.wait_keep_awake_event(indicator(false, false, true));
    // Wait for the published release-pending state to be visible to the host.
    rig.wait("releasePending", |rig| {
        match rig.registry.observe_state(KeepAwakeConfig::ID).read().state {
            stache_sdk::StateReading::Available(envelope) => envelope
                .decode::<stache_keep_awake::KeepAwakeRuntimeStatus>(KeepAwakeConfig::ID)
                .is_ok_and(|status| {
                    status.assertion_state == stache_keep_awake::AssertionState::ReleasePending
                }),
            _ => false,
        }
    });
    let attempts = kit.assertion.release_attempts().len();
    assert_eq!(
        rig.session(SessionAction::Pause),
        unavailable_session(SessionUnavailable::ReleasePending)
    );
    assert!(!rig.keep_awake_snapshot().paused);
    assert_eq!(kit.assertion.release_attempts().len(), attempts, "no retry");
    assert!(release.release());
}

#[test]
fn repeated_and_stale_clicks_never_invert_a_newer_decision() {
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    kit.probe.set(NativeLockState::Unlocked);
    rig.start();
    rig.wait_loaded();
    rig.wait_keep_awake_event(indicator(true, false, true));

    // A stale resume against a running module changes nothing.
    assert_eq!(rig.session(SessionAction::Resume), SessionActionOutcome::Stale);
    assert!(!rig.keep_awake_snapshot().paused);

    assert_eq!(rig.session(SessionAction::Pause), SessionActionOutcome::Applied);
    // Rapid duplicates of the same observed click do not resume it again.
    for _ in 0..5 {
        assert_eq!(rig.session(SessionAction::Pause), SessionActionOutcome::Stale);
    }
    assert!(rig.keep_awake_snapshot().paused, "the newer decision stands");

    // An external (tray) resume wins over a click based on the old state.
    kit.control.resume(&rig.registry).expect("tray resume");
    assert_eq!(rig.session(SessionAction::Resume), SessionActionOutcome::Stale);
    assert!(!rig.keep_awake_snapshot().paused);
    // And an external pause is not inverted by a click that saw "running".
    kit.control.pause(&rig.registry).expect("tray pause");
    assert_eq!(rig.session(SessionAction::Pause), SessionActionOutcome::Stale);
    assert!(rig.keep_awake_snapshot().paused);
    assert_eq!(rig.session(SessionAction::Resume), SessionActionOutcome::Applied);
}

#[test]
fn the_session_action_authorizes_every_invalid_caller_module_and_state() {
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    kit.probe.set(NativeLockState::Unlocked);
    rig.start();
    rig.wait_loaded();
    rig.wait_keep_awake_event(indicator(true, false, true));
    let label = rig.label();
    let before = rig.keep_awake_snapshot();

    for foreign in [
        "stache-test-1",
        "stache-access-1",
        "stache-shell-1",
        "main",
        "",
        "stache-bar-",
        "stache-bar-x",
        "stache-bar-999",
        "STACHE-BAR-1",
    ] {
        for action in [SessionAction::Pause, SessionAction::Resume] {
            assert_eq!(
                rig.bridge.session_action(foreign, "keepAwake", action),
                SessionActionOutcome::Unauthorized,
                "{foreign:?}"
            );
        }
    }
    for module in [
        "tiling",
        "bar",
        "keybindings",
        "holdToQuit",
        "keepawake",
        "",
        "keepAwake ",
    ] {
        for action in [SessionAction::Pause, SessionAction::Resume] {
            assert_eq!(
                rig.bridge.session_action(&label, module, action),
                SessionActionOutcome::Unauthorized,
                "{module:?}"
            );
        }
    }
    assert_eq!(
        rig.keep_awake_snapshot(),
        before,
        "nothing was paused or changed"
    );

    // A stale label after a panel reopen.
    rig.control.pause(&rig.registry).expect("pause");
    rig.control.resume(&rig.registry).expect("resume");
    rig.wait_opened(2);
    rig.wait("the new panel", |rig| {
        rig.windows.live_label(BarConfig::ID).is_some_and(|new| new != label)
    });
    assert_eq!(
        rig.bridge.session_action(&label, "keepAwake", SessionAction::Pause),
        SessionActionOutcome::Unauthorized
    );
    assert!(!rig.keep_awake_snapshot().paused);
    assert_eq!(rig.session(SessionAction::Pause), SessionActionOutcome::Applied);
}

#[test]
fn a_session_action_while_bar_is_not_running_is_unavailable_not_applied() {
    let rig = Rig::new(None);
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    kit.probe.set(NativeLockState::Unlocked);
    rig.start();
    assert_eq!(
        rig.bridge.session_action("stache-bar-1", "keepAwake", SessionAction::Pause),
        unavailable_session(SessionUnavailable::NotRunning)
    );
    assert!(!rig.keep_awake_snapshot().paused);
}

#[test]
fn a_session_action_during_registry_shutdown_is_typed_and_never_hangs() {
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    rig.start();
    rig.wait_loaded();
    let controls = BTreeMap::from([(KeepAwakeConfig::ID.as_str().to_owned(), kit.control)]);
    let _ = rig.registry.shutdown();
    let started = Instant::now();
    let outcome =
        session_action::execute(&rig.registry, &controls, "keepAwake", SessionAction::Pause);
    assert_eq!(outcome, unavailable_session(SessionUnavailable::ShuttingDown));
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// The projection has no timer: an idle Bar never wakes the watcher, and a host
/// status change (which does not move the Keep Awake cell) wakes the bridge.
#[test]
fn the_keep_awake_watcher_has_no_timer_and_wakes_on_host_changes() {
    let rig = Rig::new(Some(BAR));
    // Disabled: pausing it later changes only host session facts, never the cell.
    let kit = rig.add_keep_awake(Some(KEEP_AWAKE_OFF));
    rig.start();
    rig.wait_loaded();
    rig.wait_keep_awake_event(indicator(false, false, false));
    thread::sleep(Duration::from_millis(100));
    let wakeups = rig.bridge.keep_awake_wakeups();
    let events = rig.sink.events().len();
    thread::sleep(Duration::from_millis(1300));
    assert_eq!(rig.bridge.keep_awake_wakeups(), wakeups, "zero idle wakeups");
    assert_eq!(rig.sink.events().len(), events, "no idle events");

    // A pause flag of the disabled module does not touch the cell; the host
    // status sink is the only wake-up.
    kit.control.pause(&rig.registry).expect("pause");
    rig.bridge.host_changed();
    rig.wait_keep_awake_event(indicator(false, true, false));
    assert_eq!(rig.bridge.keep_awake_wakeups(), wakeups, "the cell did not move");
    kit.control.resume(&rig.registry).expect("resume");
    rig.bridge.host_changed();
    rig.wait("the resumed event", |rig| {
        rig.sink.events().last().is_some_and(|(_, event)| {
            matches!(&event.outcome, Some(BarSnapshotOutcome::Available { view })
                if view.keep_awake == Some(indicator(false, false, false)))
        })
    });
    assert!(rig.bridge.finish_shutdown());
}

#[path = "bar_tiling_e2e.rs"]
mod tiling_e2e;
