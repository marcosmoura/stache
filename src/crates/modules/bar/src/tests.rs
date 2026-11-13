//! Runtime tests over fakes: a registry-owned window backend that behaves like
//! the host (async created/loaded, close acknowledged later), a scripted
//! display feed that applies topologies to the real registry, and the real
//! module. No native object, thread of the OS or window is involved.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use stache_macos::applications::DisplayRect;
use stache_macos::desktop::DisplayId as NativeId;
use stache_macos::menu_bar::testing::{hidden_sample, visible_sample};
use stache_macos::{DisplayInfo, DisplayLayout, FakeMenuBarBackend, MenuBarControl, MenuBarSample};
use stache_sdk::{
    CancellationToken, DiscardLogger, DisplayId, EventBus, MainThreadExecutor, ModuleHandle,
    ModuleId, ModuleStatus, Permission, PermissionError, PermissionProvider, PermissionStatus,
    Registry, ThreadError, TopologyGeneration, WindowBackend, WindowError, WindowFrame, WindowId,
    WindowPhase, WindowPhases, WindowRequest,
};

use crate::{
    BarConfig, BarPanelState, BarParts, BarRuntime, BarRuntimeState, BarTiming, MenuBarState,
    ProviderParts, TopologyLease, TopologyReading, TopologySource, TopologyWait,
};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// ------------------------------------------------------------------ windows

#[derive(Clone, Copy, PartialEq, Eq)]
enum Native {
    /// Created and loaded as soon as the open is accepted.
    Auto,
    /// The test advances phases itself.
    Manual,
}

struct Windows {
    phases: Arc<WindowPhases>,
    mode: Mutex<Native>,
    next: Mutex<u64>,
    opened: Mutex<Vec<(WindowId, WindowRequest)>>,
    moved: Mutex<Vec<(WindowId, WindowFrame)>>,
    closed: Mutex<Vec<WindowId>>,
    live: Mutex<BTreeMap<WindowId, ModuleId>>,
}

impl Windows {
    fn new(mode: Native) -> Arc<Self> {
        Arc::new(Self {
            phases: WindowPhases::new(),
            mode: Mutex::new(mode),
            next: Mutex::new(0),
            opened: Mutex::default(),
            moved: Mutex::default(),
            closed: Mutex::default(),
            live: Mutex::default(),
        })
    }

    fn set_mode(&self, mode: Native) { *lock(&self.mode) = mode; }

    fn opened(&self) -> usize { lock(&self.opened).len() }

    fn last(&self) -> WindowId { lock(&self.opened).last().expect("an opened window").0 }

    fn live(&self) -> usize { lock(&self.live).len() }

    fn created(&self, id: WindowId) { self.phases.advance(id, WindowPhase::Created); }

    fn loaded(&self, id: WindowId) { self.phases.advance(id, WindowPhase::Loaded); }

    fn failed(&self, id: WindowId, reason: &str) {
        self.phases.advance(id, WindowPhase::Failed(reason.to_owned()));
        lock(&self.live).remove(&id);
    }

    /// The host's `Destroyed` acknowledgement.
    fn destroyed(&self, id: WindowId) {
        self.phases.advance(id, WindowPhase::Closed);
        lock(&self.live).remove(&id);
    }

    fn wait_opened(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.opened() < count {
            assert!(Instant::now() < deadline, "no window was opened");
            thread::sleep(Duration::from_millis(2));
        }
    }
}

impl WindowBackend for Windows {
    fn open(&self, owner: ModuleId, request: &WindowRequest) -> Result<WindowId, WindowError> {
        let id = {
            let mut next = lock(&self.next);
            *next += 1;
            WindowId::new(*next)
        };
        self.phases.begin(owner, id);
        lock(&self.live).insert(id, owner);
        lock(&self.opened).push((id, request.clone()));
        if *lock(&self.mode) == Native::Auto {
            self.created(id);
            self.loaded(id);
        }
        Ok(id)
    }

    fn focus(&self, _: WindowId) -> Result<(), WindowError> { Ok(()) }

    fn close(&self, window: WindowId) {
        lock(&self.closed).push(window);
        self.phases.advance(window, WindowPhase::Closing);
    }

    fn reposition(
        &self,
        _: ModuleId,
        window: WindowId,
        frame: WindowFrame,
    ) -> Result<(), WindowError> {
        lock(&self.moved).push((window, frame));
        Ok(())
    }

    fn phases(&self) -> Option<Arc<WindowPhases>> { Some(Arc::clone(&self.phases)) }

    fn release_owner(&self, owner: ModuleId) {
        let ids: Vec<WindowId> = lock(&self.live)
            .iter()
            .filter(|(_, o)| **o == owner)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if !lock(&self.closed).contains(&id) {
                self.close(id);
            }
        }
    }
}

// ----------------------------------------------------------------- displays

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

fn builtin() -> DisplayInfo {
    display(1, DisplayRect::new(0.0, 0.0, 1512.0, 982.0), 2.0, true, true)
}

fn external() -> DisplayInfo {
    display(
        2,
        DisplayRect::new(-2560.0, 0.0, 2560.0, 1440.0),
        1.0,
        false,
        false,
    )
}

fn layout(displays: Vec<DisplayInfo>) -> DisplayLayout { DisplayLayout::new(displays) }

struct Feed {
    state: Mutex<FeedState>,
    changed: std::sync::Condvar,
}

struct FeedState {
    reading: TopologyReading,
    /// The next `stale_reads` reads answer with the previous generation.
    stale: Option<TopologyReading>,
    stale_reads: usize,
}

/// The scripted lease: applies to the real registry like the app feed does.
struct FakeLease {
    registry: Arc<Registry>,
    feed: Feed,
    ends: AtomicUsize,
    end_ok: AtomicBool,
    generation: Mutex<u64>,
}

impl FakeLease {
    fn publish(&self, layout: Option<DisplayLayout>) {
        let mut state = lock(&self.feed.state);
        let mut generation = lock(&self.generation);
        let ids: Vec<DisplayId> = layout
            .iter()
            .flat_map(|layout| layout.displays().iter().map(|d| DisplayId::new(d.id.raw())))
            .collect();
        let changed_ids = {
            let snapshot = self.registry.observe_screen_reservations().read();
            snapshot.displays.iter().map(|d| d.display).collect::<Vec<_>>() != ids
        };
        if changed_ids {
            *generation += 1;
            self.registry
                .apply_screen_topology(TopologyGeneration::new(*generation), &ids)
                .expect("topology");
        }
        let primary = layout
            .as_ref()
            .and_then(|layout| layout.primary().map(|d| DisplayId::new(d.id.raw())));
        let previous = state.reading.clone();
        state.reading = TopologyReading {
            revision: previous.revision + 1,
            generation: TopologyGeneration::new(*generation),
            layout: layout.map(Arc::new),
            primary,
            error: None,
        };
        drop(generation);
        drop(state);
        self.feed.changed.notify_all();
    }

    /// Applies the next topology to the registry but keeps answering with the
    /// previous reading for one read, like a worker between apply and publish.
    fn publish_with_one_stale_read(&self, layout: DisplayLayout) {
        let previous = lock(&self.feed.state).reading.clone();
        self.publish(Some(layout));
        let mut state = lock(&self.feed.state);
        state.stale = Some(previous);
        state.stale_reads = 1;
    }
}

impl TopologyLease for FakeLease {
    fn read(&self) -> TopologyReading {
        let mut state = lock(&self.feed.state);
        if state.stale_reads > 0 {
            state.stale_reads -= 1;
            if let Some(stale) = state.stale.clone() {
                return stale;
            }
        }
        state.reading.clone()
    }

    fn wait_change(
        &self,
        after: u64,
        timeout: Duration,
        cancel: &[&CancellationToken],
    ) -> TopologyWait {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.iter().any(|token| token.is_cancelled()) {
                return TopologyWait::Cancelled;
            }
            let state = lock(&self.feed.state);
            if state.reading.revision != after {
                return TopologyWait::Changed(state.reading.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return TopologyWait::TimedOut;
            }
            let _ = self
                .feed
                .changed
                .wait_timeout(state, (deadline - now).min(Duration::from_millis(5)))
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn end(&self) -> bool {
        self.ends.fetch_add(1, Ordering::SeqCst);
        if !self.registry.observe_screen_reservations().read().displays.is_empty() {
            let next = {
                let mut generation = lock(&self.generation);
                *generation += 1;
                *generation
            };
            let _ = self.registry.apply_screen_topology(TopologyGeneration::new(next), &[]);
        }
        self.end_ok.load(Ordering::SeqCst)
    }
}

struct FakeSource {
    registry: Arc<Registry>,
    initial: Mutex<Option<DisplayLayout>>,
    begins: AtomicUsize,
    fail: AtomicBool,
    lease: Mutex<Option<Arc<FakeLease>>>,
    /// Shared across runs so generations stay monotonic like the app's counter.
    generation: Arc<Mutex<u64>>,
}

impl FakeSource {
    fn lease(&self) -> Arc<FakeLease> { lock(&self.lease).clone().expect("a lease") }
}

impl TopologySource for FakeSource {
    fn begin(&self) -> Result<Arc<dyn TopologyLease>, String> {
        self.begins.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err("the display query failed".to_owned());
        }
        let lease = Arc::new(FakeLease {
            registry: Arc::clone(&self.registry),
            feed: Feed {
                state: Mutex::new(FeedState {
                    reading: TopologyReading {
                        revision: 0,
                        generation: TopologyGeneration::INITIAL,
                        layout: None,
                        primary: None,
                        error: None,
                    },
                    stale: None,
                    stale_reads: 0,
                }),
                changed: std::sync::Condvar::new(),
            },
            ends: AtomicUsize::new(0),
            end_ok: AtomicBool::new(true),
            generation: Mutex::new(*lock(&self.generation)),
        });
        // Continue the shared generation sequence.
        let current = self.registry.observe_screen_reservations().read().topology.get();
        *lock(&lease.generation) = current;
        lease.publish(lock(&self.initial).clone());
        *lock(&self.generation) = *lock(&lease.generation);
        *lock(&self.lease) = Some(Arc::clone(&lease));
        Ok(lease)
    }
}

// ------------------------------------------------------------------ harness

struct Inline;

impl MainThreadExecutor for Inline {
    fn is_main_thread(&self) -> bool { false }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        job();
        Ok(())
    }
}

struct Granted;

impl PermissionProvider for Granted {
    fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

    fn request(&self, permission: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(self.status(permission))
    }
}

struct Harness {
    menu: Option<FakeMenuBarBackend>,
    registry: Arc<Registry>,
    windows: Arc<Windows>,
    source: Arc<FakeSource>,
    runtime: BarRuntime,
    handle: ModuleHandle<BarConfig>,
}

fn config(height: u16, padding: u16) -> BarConfig {
    BarConfig {
        enabled: true,
        height,
        padding,
        ..BarConfig::default()
    }
}

impl Harness {
    fn new(mode: Native, config: BarConfig, displays: Vec<DisplayInfo>) -> Self {
        Self::with_timing(mode, config, displays, BarTiming::default())
    }

    fn with_timing(
        mode: Native,
        config: BarConfig,
        displays: Vec<DisplayInfo>,
        timing: BarTiming,
    ) -> Self {
        Self::build(mode, config, displays, timing, None, None)
    }

    /// A harness whose Bar observes the menu bar through a fake adapter, with
    /// a fast poll so tests do not wait for production cadences.
    fn observing(config: BarConfig, displays: Vec<DisplayInfo>, sample: MenuBarSample) -> Self {
        let backend = FakeMenuBarBackend::new(sample);
        let timing = BarTiming {
            menu_poll: Duration::from_millis(3),
            menu_retry: Duration::from_millis(3),
            menu_join: Duration::from_millis(500),
            ..BarTiming::default()
        };
        Self::build(Native::Auto, config, displays, timing, Some(backend), None)
    }

    fn build(
        mode: Native,
        config: BarConfig,
        displays: Vec<DisplayInfo>,
        timing: BarTiming,
        menu: Option<FakeMenuBarBackend>,
        providers: Option<ProviderParts>,
    ) -> Self {
        let windows = Windows::new(mode);
        let registry = Arc::new(Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(Inline),
            Arc::new(Granted),
            Arc::clone(&windows) as Arc<dyn WindowBackend>,
        ));
        let source = Arc::new(FakeSource {
            registry: Arc::clone(&registry),
            initial: Mutex::new(Some(layout(displays))),
            begins: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            lease: Mutex::new(None),
            generation: Arc::new(Mutex::new(0)),
        });
        let mut parts = BarParts::new(Arc::clone(&source) as Arc<dyn TopologySource>);
        parts.timing = timing;
        if let Some(backend) = &menu {
            parts = parts.with_menu(MenuBarControl::with_budget(
                Arc::new(Inline),
                Arc::new(backend.clone()),
                Duration::from_millis(200),
            ));
        }
        if let Some(providers) = providers {
            parts = parts.with_providers(providers);
        }
        let runtime = BarRuntime::new(parts);
        let handle = registry.register(runtime.module(), config).expect("register");
        runtime.install_accepted_watch(registry.accepted_watch(handle).expect("watch"));
        Self {
            menu,
            registry,
            windows,
            source,
            runtime,
            handle,
        }
    }

    fn backend(&self) -> &FakeMenuBarBackend { self.menu.as_ref().expect("a menu backend") }

    fn menu_state(&self) -> MenuBarState { self.runtime.status().published().menu.state }

    fn sequence(&self) -> u64 { self.runtime.status().published().sequence }

    fn screen_revision(&self) -> u64 { self.registry.observe_screen_reservations().read().revision }

    fn start(&self) { let _ = self.registry.start_all(); }

    fn status(&self) -> ModuleStatus {
        self.registry.snapshot(self.handle).expect("snapshot").status
    }

    fn reload(&self, config: BarConfig) {
        self.registry.reload(self.handle, config).expect("reload");
    }

    fn reservation(&self, display: u32) -> f64 {
        self.registry
            .observe_screen_reservations()
            .read()
            .insets(DisplayId::new(display))
            .top()
    }

    fn displays(&self) -> Vec<u32> {
        self.registry
            .observe_screen_reservations()
            .read()
            .displays
            .iter()
            .map(|d| d.display.raw())
            .collect()
    }

    fn wait_until(&self, what: &str, condition: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(2));
        }
    }
}

// -------------------------------------------------------------------- tests

#[test]
fn construction_and_a_disabled_module_own_nothing() {
    let off = BarConfig {
        enabled: false,
        ..config(28, 14)
    };
    let h = Harness::new(Native::Auto, off, vec![builtin()]);
    h.start();
    assert_eq!(h.windows.opened(), 0);
    assert_eq!(h.source.begins.load(Ordering::SeqCst), 0);
    assert!(h.displays().is_empty());
    assert_eq!(h.runtime.status().read().state, BarRuntimeState::Inactive);
}

#[test]
fn bar_observes_keep_awake_and_tiling_optionally_and_never_depends_on_their_crates() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin()]);
    let descriptors = h.registry.descriptors();
    let descriptor = descriptors
        .iter()
        .find(|descriptor| descriptor.id == BarConfig::ID)
        .expect("bar descriptor");
    assert_eq!(descriptor.observes, &[
        crate::KEEP_AWAKE_PROVIDER,
        crate::TILING_PROVIDER
    ]);
    assert_eq!(crate::KEEP_AWAKE_PROVIDER.as_str(), "keepAwake");
    assert_eq!(crate::TILING_PROVIDER.as_str(), "tiling");
    assert!(
        descriptor.requires.is_empty(),
        "Keep Awake and Tiling are optional, never a hard requirement"
    );
    // Absent Keep Awake: Bar still starts and runs.
    h.start();
    assert_eq!(h.status(), ModuleStatus::Running);

    // Module-to-module independence: the provider crate is not a dependency.
    let manifest = include_str!("../Cargo.toml");
    assert!(
        !manifest.contains("keep-awake"),
        "stache-bar must not depend on stache-keep-awake"
    );
    assert!(
        !manifest.contains("tiling"),
        "stache-bar must not depend on stache-tiling"
    );
    let tiling = include_str!("../../tiling/Cargo.toml");
    assert!(
        !tiling.contains("stache-bar") && !tiling.contains("modules/bar"),
        "stache-tiling must not depend on stache-bar"
    );
}

#[test]
fn start_opens_the_panel_stages_the_reservation_and_reports_loaded() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin(), external()]);
    h.start();
    assert_eq!(h.status(), ModuleStatus::Running);
    let (_, request) = lock(&h.windows.opened).last().cloned().expect("opened");
    assert_eq!(request.kind, stache_sdk::WindowKind::NonActivatingPanel);
    assert_eq!(request.surface, crate::BAR_SURFACE);
    assert_eq!(request.origin, Some(stache_sdk::WindowOrigin { x: 14, y: 14 }));
    assert_eq!((request.size.width, request.size.height), (1484, 28));
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON);
    assert!(h.reservation(2).abs() < f64::EPSILON);
    let status = h.runtime.status().read();
    assert_eq!(status.state, BarRuntimeState::Running);
    assert_eq!(status.reservation.map(|r| (r.display, r.top)), Some((1, 42)));
    h.wait_until("loaded", |h| {
        h.runtime.status().read().panel == BarPanelState::Loaded
    });
    assert_eq!(status.config_revision, "1");
}

fn timing(load_ms: u64, backoff_ms: [u64; 3]) -> BarTiming {
    BarTiming {
        load_deadline: Duration::from_millis(load_ms),
        retry_backoff: backoff_ms.map(Duration::from_millis),
        ..BarTiming::default()
    }
}

/// Never retries within a test.
const NEVER: [u64; 3] = [3_600_000; 3];

#[test]
fn a_native_create_failure_after_start_keeps_bar_running_with_a_typed_failed_panel() {
    let h = Harness::with_timing(
        Native::Manual,
        config(28, 14),
        vec![builtin(), external()],
        timing(10_000, NEVER),
    );
    h.start();
    assert_eq!(
        h.status(),
        ModuleStatus::Running,
        "start never waits for the page"
    );
    h.windows.wait_opened(1);
    let first = h.windows.last();
    h.windows.failed(first, "no native window");
    h.wait_until("the failed panel", |h| {
        h.runtime.status().read().panel == BarPanelState::Failed
    });
    assert_eq!(h.status(), ModuleStatus::Running);
    let status = h.runtime.status().read();
    assert_eq!(status.state, BarRuntimeState::Running);
    assert!(status.detail.as_deref().is_some_and(|d| d.contains("no native window")));
    assert!(status.reservation.is_none() && status.frame.is_none());
    assert_eq!(h.displays(), vec![1, 2], "the topology feed stays applied");
    assert!(h.reservation(1).abs() < f64::EPSILON);
    assert_eq!(lock(&h.windows.closed).clone(), vec![first]);
    assert_eq!(h.source.lease().ends.load(Ordering::SeqCst), 0);
}

#[test]
fn a_load_timeout_clears_the_reservation_closes_the_exact_window_and_stays_running() {
    let h = Harness::with_timing(
        Native::Manual,
        config(28, 14),
        vec![builtin()],
        timing(1120, NEVER),
    );
    let begun = Instant::now();
    h.start();
    assert!(
        begun.elapsed() < Duration::from_millis(600),
        "start returned promptly"
    );
    assert_eq!(h.status(), ModuleStatus::Running);
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON, "staged at open");
    assert_eq!(h.runtime.status().read().panel, BarPanelState::Requested);
    h.windows.created(h.windows.last());
    h.wait_until("the timeout", |h| {
        h.runtime.status().read().panel == BarPanelState::Failed
    });
    let status = h.runtime.status().read();
    assert!(status.detail.as_deref().is_some_and(|d| d.contains("page never loaded")));
    assert_eq!(lock(&h.windows.closed).clone(), vec![h.windows.last()]);
    assert!(h.reservation(1).abs() < f64::EPSILON);
    assert_eq!(h.displays(), vec![1]);
    assert!(status.reservation.is_none());
    assert_eq!(h.status(), ModuleStatus::Running);
}

#[test]
fn a_failed_panel_retries_with_backoff_each_time_a_fresh_exact_window() {
    let h = Harness::with_timing(
        Native::Manual,
        config(28, 14),
        vec![builtin()],
        timing(40, [30, 30, 30]),
    );
    h.start();
    // Never loads: initial open + three retries, then it stays failed.
    h.wait_until("four opens", |h| h.windows.opened() >= 4);
    h.wait_until("failed for good", |h| {
        h.runtime.status().read().panel == BarPanelState::Failed
            && h.reservation(1).abs() < f64::EPSILON
            && lock(&h.windows.closed).len() == 4
    });
    thread::sleep(Duration::from_millis(150));
    assert_eq!(h.windows.opened(), 4, "bounded: no further retry");
    let ids: Vec<_> = lock(&h.windows.opened).iter().map(|(id, _)| *id).collect();
    assert_eq!(ids.len(), 4);
    assert!(ids.windows(2).all(|pair| pair[0] != pair[1]));
    assert_eq!(h.status(), ModuleStatus::Running);
}

#[test]
fn a_backoff_retry_recovers_when_the_host_starts_loading() {
    let h = Harness::with_timing(
        Native::Manual,
        config(28, 14),
        vec![builtin(), external()],
        timing(40, [60, 60, 60]),
    );
    h.start();
    h.wait_until("the failure", |h| {
        h.runtime.status().read().panel == BarPanelState::Failed
    });
    h.windows.set_mode(Native::Auto);
    h.wait_until("the recovery", |h| {
        h.runtime.status().read().panel == BarPanelState::Loaded
    });
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON);
    assert_eq!(h.displays(), vec![1, 2]);
    assert_eq!(h.status(), ModuleStatus::Running);
    assert!(h.runtime.status().read().detail.is_none());
}

#[test]
fn a_convergence_trigger_retries_a_failed_panel_immediately() {
    let h = Harness::with_timing(
        Native::Manual,
        config(28, 14),
        vec![builtin()],
        timing(40, NEVER),
    );
    h.start();
    h.wait_until("the failure", |h| {
        h.runtime.status().read().panel == BarPanelState::Failed
    });
    h.windows.set_mode(Native::Auto);
    h.reload(config(30, 14));
    h.wait_until("the retry", |h| {
        h.runtime.status().read().panel == BarPanelState::Loaded
    });
    assert!((h.reservation(1) - 44.0).abs() < f64::EPSILON);
    assert_eq!(h.windows.opened(), 2);
}

#[test]
fn pause_and_resume_retry_a_failed_panel_immediately() {
    let h = Harness::with_timing(
        Native::Manual,
        config(28, 14),
        vec![builtin()],
        timing(40, NEVER),
    );
    h.start();
    h.wait_until("the failure", |h| {
        h.runtime.status().read().panel == BarPanelState::Failed
    });
    h.windows.set_mode(Native::Auto);
    h.registry.pause(h.handle).expect("pause");
    h.registry.resume(h.handle).expect("resume");
    assert_eq!(h.status(), ModuleStatus::Running);
    h.wait_until("loaded", |h| {
        h.runtime.status().read().panel == BarPanelState::Loaded
    });
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON);
}

#[test]
fn stopping_during_a_pending_load_wait_returns_promptly_and_a_late_load_resurrects_nothing() {
    let h = Harness::with_timing(
        Native::Manual,
        config(28, 14),
        vec![builtin()],
        timing(10_000, NEVER),
    );
    h.start();
    let id = h.windows.last();
    let begun = Instant::now();
    h.registry.set_enabled(h.handle, false).expect("disable");
    assert!(
        begun.elapsed() < Duration::from_secs(2),
        "stop did not wait for the load"
    );
    h.windows.created(id);
    h.windows.loaded(id);
    thread::sleep(Duration::from_millis(50));
    let status = h.runtime.status().read();
    assert_eq!(status.panel, BarPanelState::None);
    assert!(status.reservation.is_none());
    assert!(h.displays().is_empty());
    assert_eq!(h.windows.opened(), 1);
}

#[test]
fn shutdown_during_a_pending_load_wait_is_prompt() {
    let h = Harness::with_timing(
        Native::Manual,
        config(28, 14),
        vec![builtin()],
        timing(10_000, NEVER),
    );
    h.start();
    let begun = Instant::now();
    h.runtime.begin_shutdown();
    let _ = h.registry.shutdown();
    assert!(begun.elapsed() < Duration::from_secs(2));
    assert!(h.displays().is_empty());
    assert!(h.reservation(1).abs() < f64::EPSILON);
}

#[test]
fn zero_height_runs_with_no_panel_no_window_and_no_reservation() {
    let h = Harness::new(Native::Auto, config(0, 14), vec![builtin()]);
    h.start();
    assert_eq!(h.status(), ModuleStatus::Running);
    assert_eq!(h.windows.opened(), 0);
    assert!(h.reservation(1).abs() < f64::EPSILON);
    let status = h.runtime.status().read();
    assert_eq!(status.panel, BarPanelState::NoPanelZeroSize);
    assert!(status.frame.is_none() && status.reservation.is_none());
    assert_eq!(h.displays(), vec![1], "the topology still lists the display");
}

#[test]
fn reconfigure_moves_between_no_panel_and_panel_in_place() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin()]);
    h.start();
    let first = h.windows.last();
    // Height and padding change: reposition + replaced reservation, same window.
    h.reload(config(30, 10));
    assert_eq!(h.windows.opened(), 1);
    assert_eq!(lock(&h.windows.moved).len(), 1);
    assert_eq!(lock(&h.windows.moved)[0].0, first);
    assert!((h.reservation(1) - 40.0).abs() < f64::EPSILON);
    // Enter zero size: the window closes and the reservation goes.
    h.reload(config(0, 10));
    assert_eq!(lock(&h.windows.closed).clone(), vec![first]);
    assert!(h.reservation(1).abs() < f64::EPSILON);
    assert_eq!(h.runtime.status().read().panel, BarPanelState::NoPanelZeroSize);
    assert_eq!(h.status(), ModuleStatus::Running);
    assert_eq!(h.displays(), vec![1], "topology still applied while Bar runs");
    // Leave zero size: a new exact window, reservation back.
    h.reload(config(28, 14));
    assert_eq!(h.windows.opened(), 2);
    assert_ne!(h.windows.last(), first);
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON);
}

#[test]
fn an_unchanged_reload_and_a_same_shape_reconfigure_do_nothing() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin()]);
    h.start();
    h.reload(config(28, 14));
    let weather = BarConfig {
        weather: crate::WeatherConfig {
            default_location: "Berlin".to_owned(),
            ..crate::WeatherConfig::default()
        },
        ..config(28, 14)
    };
    h.reload(weather);
    assert_eq!(h.windows.opened(), 1);
    assert!(lock(&h.windows.moved).is_empty());
    assert!(lock(&h.windows.closed).is_empty());
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON);
}

#[test]
fn a_rejected_reconfigure_keeps_the_old_frame_and_reservation() {
    let wide = display(1, DisplayRect::new(0.0, 0.0, 9000.0, 900.0), 1.0, true, true);
    let h = Harness::new(Native::Auto, config(28, 500), vec![wide]);
    h.start();
    assert_eq!(h.status(), ModuleStatus::Running);
    // 9000 - 2*400 = 8200 > 8192: the frame cannot be expressed.
    let rejected = h.registry.reload(h.handle, config(28, 400));
    assert!(rejected.is_err());
    let status = h.runtime.status().read();
    assert_eq!(status.frame.map(|f| (f.x, f.width)), Some((500, 8000)));
    assert!((h.reservation(1) - 528.0).abs() < f64::EPSILON);
    assert_eq!(h.windows.opened(), 1);
    assert!(lock(&h.windows.moved).is_empty());
}

#[test]
fn entering_a_panel_that_fails_to_load_accepts_the_reload_and_follows_the_failed_path() {
    let h = Harness::with_timing(Native::Auto, config(0, 14), vec![builtin()], timing(80, NEVER));
    h.start();
    h.windows.set_mode(Native::Manual);
    h.registry.reload(h.handle, config(28, 14)).expect("accepted");
    assert_eq!(h.status(), ModuleStatus::Running);
    h.wait_until("the failure", |h| {
        h.runtime.status().read().panel == BarPanelState::Failed
    });
    assert!(h.reservation(1).abs() < f64::EPSILON);
    assert_eq!(
        lock(&h.windows.closed).len(),
        1,
        "the half-open window was closed"
    );
    assert_eq!(h.displays(), vec![1]);
    assert_eq!(h.status(), ModuleStatus::Running);
}

#[test]
fn a_stop_clears_the_reservation_and_ends_the_feed_with_an_empty_topology() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin(), external()]);
    h.start();
    h.registry.set_enabled(h.handle, false).expect("disable");
    assert!(h.displays().is_empty());
    assert!(h.reservation(1).abs() < f64::EPSILON);
    assert_eq!(h.source.lease().ends.load(Ordering::SeqCst), 1);
    assert_eq!(h.runtime.status().read().state, BarRuntimeState::CleanupPending);
    h.windows.destroyed(h.windows.last());
    assert_eq!(h.runtime.status().read().state, BarRuntimeState::Inactive);
}

#[test]
fn pause_and_resume_follow_the_ordinary_stop_and_start() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin()]);
    h.start();
    h.registry.pause(h.handle).expect("pause");
    assert!(h.displays().is_empty());
    assert!(h.reservation(1).abs() < f64::EPSILON);
    h.registry.resume(h.handle).expect("resume");
    assert_eq!(h.displays(), vec![1]);
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON);
    assert_eq!(h.windows.opened(), 2);
}

#[test]
fn a_late_close_acknowledgement_cannot_touch_the_successor() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin()]);
    h.start();
    let first = h.windows.last();
    h.registry.set_enabled(h.handle, false).expect("disable");
    // The old close is not acknowledged; the retained identity is reported.
    assert_eq!(h.runtime.status().read().cleanup_pending, vec![
        first.to_string()
    ]);
    h.registry.set_enabled(h.handle, true).expect("enable");
    let second = h.windows.last();
    assert_ne!(first, second);
    h.wait_until("loaded", |h| {
        h.runtime.status().read().panel == BarPanelState::Loaded
    });
    h.windows.destroyed(first);
    let status = h.runtime.status().read();
    assert_eq!(status.panel, BarPanelState::Loaded, "the successor is untouched");
    assert!(status.cleanup_pending.is_empty());
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON);
    assert!(h.windows.live() >= 1);
}

#[test]
fn a_stuck_feed_end_reports_cleanup_pending_and_retries_the_exact_lease() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin()]);
    h.start();
    let lease = h.source.lease();
    lease.end_ok.store(false, Ordering::SeqCst);
    let _ = h.registry.set_enabled(h.handle, false);
    assert!(matches!(h.status(), ModuleStatus::Failed(_)));
    assert!(
        h.displays().is_empty(),
        "the empty topology is applied before the join"
    );
    assert_eq!(h.runtime.status().read().state, BarRuntimeState::CleanupPending);
    lease.end_ok.store(true, Ordering::SeqCst);
    let _ = h.registry.set_enabled(h.handle, false);
    h.windows.destroyed(h.windows.last());
    assert_eq!(h.runtime.status().read().state, BarRuntimeState::Inactive);
    assert_eq!(
        lease.ends.load(Ordering::SeqCst),
        2,
        "the same lease was retried"
    );
}

#[test]
fn a_feed_that_cannot_start_fails_the_start_and_owns_nothing() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin()]);
    h.source.fail.store(true, Ordering::SeqCst);
    h.start();
    assert!(matches!(h.status(), ModuleStatus::Failed(_)));
    assert_eq!(h.windows.opened(), 0);
    assert!(h.displays().is_empty());
}

#[test]
fn a_stale_topology_generation_is_retried_once_against_a_fresher_reading() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin()]);
    h.start();
    let lease = h.source.lease();
    // A display appears: the registry moves to a new generation while the
    // feed still answers its previous reading once.
    lease.publish_with_one_stale_read(layout(vec![builtin(), external()]));
    h.wait_until("the reservation to follow the new generation", |h| {
        h.registry.observe_screen_reservations().read().topology.get() >= 2
            && h.runtime.status().read().topology_generation.as_deref() == Some("2")
    });
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON);
    assert_eq!(h.windows.opened(), 1);
}

#[test]
fn no_primary_is_unavailable_with_no_panel_and_no_reservation_then_recovers() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin()]);
    h.start();
    let lease = h.source.lease();
    lease.publish(Some(layout(Vec::new())));
    h.wait_until("no-primary", |h| {
        h.runtime.status().read().panel == BarPanelState::NoPrimaryDisplay
    });
    assert!(h.reservation(1).abs() < f64::EPSILON);
    assert_eq!(h.status(), ModuleStatus::Running);
    assert_eq!(lock(&h.windows.closed).len(), 1);
    lease.publish(Some(layout(vec![builtin()])));
    h.wait_until("recovery", |h| {
        h.runtime.status().read().panel == BarPanelState::Loaded
    });
    assert_eq!(h.windows.opened(), 2);
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON);
}

#[test]
fn a_primary_swap_moves_the_panel_and_the_reservation_together() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin(), external()]);
    h.start();
    let lease = h.source.lease();
    // The external display becomes screens[0] (origin 0,0); the built-in moves.
    let new_primary = display(2, DisplayRect::new(0.0, 0.0, 2560.0, 1440.0), 1.0, true, true);
    let old = display(
        1,
        DisplayRect::new(2560.0, 0.0, 1512.0, 982.0),
        2.0,
        false,
        false,
    );
    lease.publish(Some(layout(vec![new_primary, old])));
    h.wait_until("the reservation to move", |h| h.reservation(2) > 0.0);
    assert!(h.reservation(1).abs() < f64::EPSILON);
    assert!((h.reservation(2) - 42.0).abs() < f64::EPSILON);
    assert_eq!(h.windows.opened(), 1, "the panel moved, it was not reopened");
    let moved = lock(&h.windows.moved).last().copied().expect("moved");
    assert_eq!((moved.1.size.width, moved.1.origin.x), (2532, 14));
}

fn external_primary() -> DisplayInfo {
    display(2, DisplayRect::new(0.0, 0.0, 2560.0, 1440.0), 1.0, true, true)
}

fn builtin_secondary() -> DisplayInfo {
    display(
        1,
        DisplayRect::new(2560.0, 0.0, 1512.0, 982.0),
        2.0,
        false,
        false,
    )
}

fn moves(h: &Harness) -> Vec<(i32, i32, u32, u32)> {
    lock(&h.windows.moved)
        .iter()
        .map(|(_, f)| (f.origin.x, f.origin.y, f.size.width, f.size.height))
        .collect()
}

// The planned frame changes here, so this scenario also passed before the
// panel fix: the Bar module uses a fake window capability. The fix itself (the
// panel takes the requested frame, never the stale Tauri window frame) is
// covered by `stache-app`
// `windows::tests::repositioning_places_the_panel_at_the_requested_frame_not_the_tauri_window`.
#[test]
fn disconnecting_the_primary_external_display_moves_the_panel_to_the_builtin_and_back() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![
        external_primary(),
        builtin_secondary(),
    ]);
    h.start();
    let first = h.windows.last();
    assert!((h.reservation(2) - 42.0).abs() < f64::EPSILON);
    let lease = h.source.lease();
    // Disconnect: the built-in display becomes screens[0] at the origin.
    lease.publish(Some(layout(vec![builtin()])));
    h.wait_until("the panel to move to the built-in", |h| !moves(h).is_empty());
    assert_eq!(moves(&h), [(14, 14, 1484, 28)]);
    assert_eq!(h.windows.last(), first, "moved, not reopened");
    assert_eq!(h.displays(), vec![1]);
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON);
    let status = h.runtime.status().read();
    assert_eq!(status.frame.map(|f| (f.x, f.y, f.width)), Some((14, 14, 1484)));
    // Reconnect: back on the external display.
    lease.publish(Some(layout(vec![external_primary(), builtin_secondary()])));
    h.wait_until("the panel to move back", |h| moves(h).len() == 2);
    assert_eq!(moves(&h)[1], (14, 14, 2532, 28));
    h.wait_until("the reservation to move back", |h| h.reservation(2) > 0.0);
    assert!(h.reservation(1).abs() < f64::EPSILON);
    assert_eq!(h.windows.opened(), 1);
}

#[test]
fn a_same_size_primary_replacement_still_reasserts_the_panel_frame() {
    let twin = |id| display(id, DisplayRect::new(0.0, 0.0, 2560.0, 1440.0), 1.0, true, true);
    let h = Harness::new(Native::Auto, config(28, 14), vec![twin(2)]);
    h.start();
    let lease = h.source.lease();
    // A different display of the same size becomes primary: the planned frame
    // is numerically identical, yet the system moved the panel off the
    // removed display, so it must be placed again.
    lease.publish(Some(layout(vec![twin(3)])));
    h.wait_until("the panel to be re-asserted", |h| !moves(h).is_empty());
    assert_eq!(moves(&h), [(14, 14, 2532, 28)]);
    h.wait_until("the reservation to follow", |h| h.reservation(3) > 0.0);
    assert_eq!(h.windows.opened(), 1);
}

#[test]
fn an_arrangement_only_change_reasserts_the_panel_frame() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin(), external()]);
    h.start();
    let generation = h.registry.observe_screen_reservations().read().topology;
    let lease = h.source.lease();
    let above = display(
        2,
        DisplayRect::new(0.0, -1440.0, 2560.0, 1440.0),
        1.0,
        false,
        false,
    );
    lease.publish(Some(layout(vec![builtin(), above])));
    h.wait_until("the panel to be re-asserted", |h| !moves(h).is_empty());
    assert_eq!(moves(&h), [(14, 14, 1484, 28)]);
    assert_eq!(
        h.registry.observe_screen_reservations().read().topology,
        generation
    );
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON);
}

#[test]
fn a_briefly_absent_primary_reopens_the_panel_at_the_new_primary_frame() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![
        external_primary(),
        builtin_secondary(),
    ]);
    h.start();
    let lease = h.source.lease();
    lease.publish(Some(layout(Vec::new())));
    h.wait_until("no-primary", |h| {
        h.runtime.status().read().panel == BarPanelState::NoPrimaryDisplay
    });
    assert_eq!(lock(&h.windows.closed).len(), 1);
    lease.publish(Some(layout(vec![builtin()])));
    h.wait_until("recovery", |h| {
        h.runtime.status().read().panel == BarPanelState::Loaded
    });
    assert_eq!(h.windows.opened(), 2);
    let request = lock(&h.windows.opened).last().expect("opened").1.clone();
    assert_eq!(request.size.width, 1484);
    assert_eq!(request.origin.map(|o| (o.x, o.y)), Some((14, 14)));
    assert!((h.reservation(1) - 42.0).abs() < f64::EPSILON);
}

#[test]
fn late_native_callbacks_after_a_stop_do_not_resurrect_anything() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin()]);
    h.start();
    let id = h.windows.last();
    h.registry.set_enabled(h.handle, false).expect("disable");
    h.windows.created(id);
    h.windows.loaded(id);
    let status = h.runtime.status().read();
    assert_eq!(status.panel, BarPanelState::None);
    assert!(status.reservation.is_none());
    assert!(h.displays().is_empty());
}

#[test]
fn begin_shutdown_refuses_starts_and_finish_reports_nothing_owed() {
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin()]);
    h.runtime.begin_shutdown();
    h.start();
    assert!(matches!(h.status(), ModuleStatus::Failed(_)));
    assert_eq!(h.windows.opened(), 0);
    assert!(h.runtime.finish_shutdown());
}

// ---------------------------------------------------------------- menu bar

fn hidden_with(auto_hide: Option<bool>) -> MenuBarSample {
    MenuBarSample { auto_hide, ..hidden_sample() }
}

#[test]
fn no_menu_observer_worker_or_sample_exists_without_a_panel() {
    // Disabled.
    let off = BarConfig {
        enabled: false,
        ..config(28, 14)
    };
    let h = Harness::observing(off, vec![builtin()], hidden_sample());
    h.start();
    assert_eq!(
        (h.backend().installs(), h.backend().samples()),
        (0, 0),
        "disabled"
    );

    // Zero size.
    let zero = Harness::observing(config(0, 14), vec![builtin()], hidden_sample());
    zero.start();
    assert_eq!(zero.status(), ModuleStatus::Running);
    assert_eq!(
        (zero.backend().installs(), zero.backend().samples()),
        (0, 0),
        "zero size"
    );

    // No primary display.
    let none = Harness::observing(config(28, 14), vec![], hidden_sample());
    none.start();
    assert_eq!(
        (none.backend().installs(), none.backend().samples()),
        (0, 0),
        "no primary"
    );
    assert_eq!(none.menu_state(), MenuBarState::Unknown);

    // Paused: an observer exists while running and none after the pause.
    let paused = Harness::observing(config(28, 14), vec![builtin()], hidden_sample());
    paused.start();
    paused.wait_until("the observer", |h| h.backend().observers() == 1);
    paused.registry.pause(paused.handle).expect("pause");
    assert_eq!(paused.backend().observers(), 0);
    assert_eq!(paused.menu_state(), MenuBarState::Unknown);
}

#[test]
fn the_observer_is_installed_before_the_first_sample_and_removed_exactly() {
    let h = Harness::observing(config(28, 14), vec![builtin()], hidden_with(Some(false)));
    h.start();
    h.wait_until("the seed", |h| h.menu_state() == MenuBarState::Hidden);
    let log = h.backend().log();
    assert_eq!(
        log.first(),
        Some(&"observe"),
        "subscribe before the first refresh: {log:?}"
    );
    assert!(log.contains(&"sample"));
    h.registry.set_enabled(h.handle, false).expect("disable");
    assert_eq!(h.backend().observers(), 0);
    assert_eq!(h.backend().installs(), 1);
    assert_eq!(h.backend().removals(), 1, "removed exactly once");
    assert_eq!(h.menu_state(), MenuBarState::Unknown);
    h.windows.destroyed(h.windows.last());
    assert_eq!(h.runtime.status().read().state, BarRuntimeState::Inactive);
}

#[test]
fn signals_drive_the_state_and_the_reservation_never_moves() {
    let h = Harness::observing(config(28, 14), vec![builtin()], hidden_with(Some(false)));
    h.start();
    h.wait_until("hidden", |h| h.menu_state() == MenuBarState::Hidden);
    let revision = h.screen_revision();
    assert_eq!(h.reservation(1), 42.0);
    let mut sequence = h.sequence();
    for step in 0..20 {
        let visible = step % 2 == 0;
        h.backend().set_sample(MenuBarSample {
            menu_bar_visible: Some(visible),
            ..hidden_with(Some(false))
        });
        assert_eq!(h.backend().signal(), 1);
        let want = if visible {
            MenuBarState::Visible
        } else {
            MenuBarState::Hidden
        };
        h.wait_until("the toggle", |h| h.menu_state() == want);
        let next = h.sequence();
        assert!(
            next > sequence,
            "the sequence only increases ({sequence} -> {next})"
        );
        sequence = next;
        assert_eq!(h.reservation(1), 42.0, "constant in every menu state");
        assert_eq!(h.screen_revision(), revision, "the registry saw no replacement");
    }
    // Unknown is a state too, and the reservation still holds.
    h.backend().fail_samples(true);
    h.backend().signal();
    h.wait_until("held", |h| {
        h.runtime
            .status()
            .published()
            .menu
            .reason
            .is_some_and(|r| r.starts_with("held"))
    });
    assert_eq!(h.reservation(1), 42.0);
    assert_eq!(h.screen_revision(), revision);
}

#[test]
fn a_dropped_signal_is_reconciled_by_the_poll_only_while_auto_hide_needs_it() {
    let h = Harness::observing(config(28, 14), vec![builtin()], hidden_with(Some(true)));
    h.start();
    h.wait_until("hidden", |h| h.menu_state() == MenuBarState::Hidden);
    // The pointer enters the menu-bar row: macOS posts nothing. The poll finds it.
    h.backend().set_sample(MenuBarSample {
        pointer_in_row: Some(true),
        ..hidden_with(Some(true))
    });
    h.wait_until("the poll", |h| h.menu_state() == MenuBarState::Visible);
    // Auto-hide turns off: the next sample ends the poll.
    h.backend().set_sample(visible_sample());
    h.wait_until("the last poll", |h| h.menu_state() == MenuBarState::Visible);
    thread::sleep(Duration::from_millis(40));
    let settled = h.backend().samples();
    thread::sleep(Duration::from_millis(60));
    assert_eq!(
        h.backend().samples(),
        settled,
        "no sample once auto-hide is known off"
    );
    // A signal restarts the refresh, and it stays event driven.
    h.backend().set_sample(hidden_with(Some(false)));
    h.backend().signal();
    h.wait_until("the hint", |h| h.menu_state() == MenuBarState::Hidden);
    let after = h.backend().samples();
    thread::sleep(Duration::from_millis(60));
    assert_eq!(h.backend().samples(), after, "still no poll");
}

#[test]
fn the_poll_cadence_is_bounded_while_needed_and_zero_when_not() {
    let backend = FakeMenuBarBackend::new(hidden_with(Some(true)));
    let timing = BarTiming {
        menu_poll: Duration::from_millis(50),
        menu_retry: Duration::from_millis(50),
        menu_join: Duration::from_millis(500),
        ..BarTiming::default()
    };
    let h = Harness::build(
        Native::Auto,
        config(28, 14),
        vec![builtin()],
        timing,
        Some(backend),
        None,
    );
    h.start();
    h.wait_until("hidden", |h| h.menu_state() == MenuBarState::Hidden);
    let begun = h.backend().samples();
    thread::sleep(Duration::from_millis(400));
    let polled = h.backend().samples() - begun;
    // 400 ms at a 50 ms cadence is about 8 samples; never a busy loop.
    assert!((2..=12).contains(&polled), "{polled} polls in 400 ms at 50 ms");
    // Auto-hide known off: zero samples over the same window.
    h.backend().set_sample(hidden_with(Some(false)));
    h.backend().signal();
    h.wait_until("the last refresh", |h| {
        let before = h.backend().samples();
        thread::sleep(Duration::from_millis(120));
        h.backend().samples() == before
    });
    let idle = h.backend().samples();
    thread::sleep(Duration::from_millis(300));
    assert_eq!(h.backend().samples(), idle, "no poll while auto-hide is off");
}

#[test]
fn bursts_of_hints_coalesce_into_bounded_samples() {
    let h = Harness::observing(config(28, 14), vec![builtin()], visible_sample());
    h.start();
    h.wait_until("visible", |h| h.menu_state() == MenuBarState::Visible);
    thread::sleep(Duration::from_millis(30));
    let before = h.backend().samples();
    for _ in 0..200 {
        h.backend().signal();
    }
    thread::sleep(Duration::from_millis(60));
    let taken = h.backend().samples() - before;
    assert!(taken >= 1, "the burst caused a refresh");
    assert!(taken <= 200, "never more samples than hints ({taken})");
}

#[test]
fn an_unreadable_state_is_explicit_deduplicated_and_recovers() {
    let h = Harness::observing(config(28, 14), vec![builtin()], hidden_sample());
    h.backend().fail_samples(true);
    h.start();
    h.wait_until("unknown", |h| {
        h.runtime
            .status()
            .published()
            .menu
            .reason
            .is_some_and(|r| r.contains("could not be read"))
    });
    assert_eq!(h.menu_state(), MenuBarState::Unknown);
    // The page-load phase is published by the panel worker; let it settle so
    // only the retries are measured.
    h.wait_until("loaded", |h| {
        h.runtime.status().read().panel == BarPanelState::Loaded
    });
    let sequence = h.sequence();
    let samples = h.backend().samples();
    h.wait_until("several retries", |h| h.backend().samples() >= samples + 5);
    assert_eq!(h.sequence(), sequence, "one diagnostic, not one per retry");
    h.backend().fail_samples(false);
    h.wait_until("recovered", |h| h.menu_state() == MenuBarState::Hidden);
    assert!(h.runtime.status().published().menu.reason.is_none());
}

#[test]
fn a_failed_registration_degrades_to_a_labelled_poll() {
    let h = Harness::observing(config(28, 14), vec![builtin()], visible_sample());
    h.backend().fail_observes(1_000);
    h.start();
    h.wait_until("the degraded reading", |h| {
        h.runtime
            .status()
            .published()
            .menu
            .reason
            .is_some_and(|r| r.contains("polling"))
    });
    let menu = h.runtime.status().published().menu;
    assert_eq!(menu.state, MenuBarState::Visible);
    assert!(menu.reason.is_some_and(|reason| reason.contains("polling")));
    // Even with auto-hide off the blind worker keeps polling and retries the
    // registration; once it succeeds the label goes away.
    h.backend().fail_observes(0);
    h.wait_until("the observer", |h| h.backend().observers() == 1);
    h.wait_until("the label clears", |h| {
        h.runtime.status().published().menu.reason.is_none()
    });
}

#[test]
fn a_stuck_observer_removal_is_cleanup_pending_and_retried_and_late_callbacks_are_ignored() {
    let h = Harness::observing(config(28, 14), vec![builtin()], hidden_with(Some(false)));
    h.start();
    h.wait_until("hidden", |h| h.menu_state() == MenuBarState::Hidden);
    // The worker's own removal and the stop's immediate retry both fail.
    h.backend().fail_unobserves(2);
    let _ = h.registry.set_enabled(h.handle, false);
    assert!(matches!(h.status(), ModuleStatus::Failed(_)));
    assert_eq!(h.runtime.status().read().state, BarRuntimeState::CleanupPending);
    assert_eq!(h.backend().observers(), 1, "the registration stays owned");
    // A late native callback after the stop publishes nothing.
    let sequence = h.sequence();
    h.backend().set_sample(MenuBarSample {
        menu_bar_visible: Some(true),
        ..hidden_with(Some(false))
    });
    assert_eq!(h.backend().signal(), 1, "the native block still fires");
    thread::sleep(Duration::from_millis(30));
    assert_eq!(h.sequence(), sequence);
    assert_eq!(h.menu_state(), MenuBarState::Unknown);
    // The exact retry settles it.
    let _ = h.registry.set_enabled(h.handle, false);
    assert_eq!(h.backend().observers(), 0);
    assert_eq!(h.backend().removals(), 1);
    h.windows.destroyed(h.windows.last());
    assert_eq!(h.runtime.status().read().state, BarRuntimeState::Inactive);
}

#[test]
fn leaving_and_re_entering_zero_size_stops_and_restarts_the_observation() {
    let h = Harness::observing(config(28, 14), vec![builtin()], hidden_sample());
    h.start();
    h.wait_until("the observer", |h| h.backend().observers() == 1);
    // Reconfigure to zero size: the panel goes, so does the observation.
    h.reload(config(0, 14));
    h.wait_until("no observer", |h| h.backend().observers() == 0);
    assert_eq!(h.menu_state(), MenuBarState::Unknown);
    // And back: a fresh observation seeds again.
    h.reload(config(28, 14));
    h.wait_until("seeded again", |h| {
        h.backend().observers() == 1 && h.menu_state() == MenuBarState::Hidden
    });
    assert_eq!(h.backend().installs(), 2);
}

#[test]
fn the_published_view_carries_exact_identity_and_the_accepted_config() {
    let h = Harness::observing(config(28, 14), vec![builtin()], hidden_sample());
    assert_eq!(h.runtime.status().published().generation, 0);
    h.start();
    h.wait_until("hidden", |h| h.menu_state() == MenuBarState::Hidden);
    let first = h.runtime.status().published();
    assert_eq!((first.generation, first.height, first.padding), (1, 28, 14));
    assert_eq!(first.state, BarRuntimeState::Running);
    assert!(!first.config_revision.is_empty());
    h.reload(config(30, 10));
    let second = h.runtime.status().published();
    assert!(second.sequence > first.sequence);
    assert_eq!((second.height, second.padding), (30, 10));
    h.registry.pause(h.handle).expect("pause");
    h.registry.resume(h.handle).expect("resume");
    let third = h.runtime.status().published();
    assert_eq!(third.generation, 2, "a new run is a new generation");
    assert!(third.sequence > second.sequence, "the sequence never resets");
}

#[test]
fn an_accepted_layout_reload_publishes_the_new_layout_without_touching_the_panel() {
    use crate::{BarLayoutConfig, BarLayoutEntry, BarLayoutEntryTable, BarWidgetKind as K};
    let h = Harness::new(Native::Auto, config(28, 14), vec![builtin()]);
    h.start();
    let first = h.runtime.status().published();
    assert_eq!(first.layout, BarLayoutConfig::default().normalized());
    assert_eq!(
        first.layout.end.last().map(crate::BarLayoutItem::id),
        Some("clock")
    );

    let layout = BarLayoutConfig {
        start: vec![BarLayoutEntry::Widget(K::Clock)],
        center: vec![],
        end: vec![
            BarLayoutEntry::Widget(K::Cpu),
            BarLayoutEntry::Table(BarLayoutEntryTable::new(K::Cpu, Some("cpu_b".to_owned()))),
        ],
    };
    h.reload(BarConfig {
        layout: layout.clone(),
        ..config(28, 14)
    });
    let second = h.runtime.status().published();
    assert!(second.sequence > first.sequence);
    assert_eq!(second.layout, layout.normalized());
    assert_eq!(second.generation, first.generation, "no restart");
    assert_eq!(second.state, BarRuntimeState::Running);
    assert_eq!(h.windows.opened(), 1, "the panel is not recreated");
    assert!(lock(&h.windows.moved).is_empty());
    assert!(lock(&h.windows.closed).is_empty());

    // A rejected file (duplicate explicit id) changes nothing.
    let duplicate = BarLayoutConfig {
        start: vec![BarLayoutEntry::Table(BarLayoutEntryTable::new(
            K::Wifi,
            Some("x".to_owned()),
        ))],
        center: vec![BarLayoutEntry::Table(BarLayoutEntryTable::new(
            K::Media,
            Some("x".to_owned()),
        ))],
        end: vec![],
    };
    let rejected = h.registry.reload(h.handle, BarConfig {
        layout: duplicate,
        ..config(28, 14)
    });
    assert!(rejected.is_err());
    let third = h.runtime.status().published();
    // The sequence is not compared: a provider reading may publish meanwhile.
    assert_eq!(third.layout, second.layout);
    assert_eq!(third.generation, second.generation);
}

#[test]
fn an_options_reload_publishes_resolved_options_and_a_rejected_one_publishes_nothing() {
    use crate::{
        BarEntryFields, BarLayoutConfig, BarLayoutEntry, BarLayoutEntryTable, BarLayoutItem,
        BarWidgetKind as K, ClockOptions, CpuOptions, HourCycle,
    };
    let base = BarLayoutConfig {
        start: vec![],
        center: vec![],
        end: vec![
            BarLayoutEntry::Widget(K::Cpu),
            BarLayoutEntry::Widget(K::Clock),
        ],
    };
    let h = Harness::new(Native::Auto, BarConfig { layout: base, ..config(28, 14) }, vec![
        builtin(),
    ]);
    h.start();
    let first = h.runtime.status().published();
    let with = |alert: u8| BarConfig {
        layout: BarLayoutConfig {
            start: vec![],
            center: vec![],
            end: vec![
                BarLayoutEntry::Table(BarLayoutEntryTable::Cpu(BarEntryFields {
                    id: None,
                    options: CpuOptions { temperature_alert: alert },
                })),
                BarLayoutEntry::Table(BarLayoutEntryTable::Clock(BarEntryFields {
                    id: None,
                    options: ClockOptions {
                        show_seconds: false,
                        hour_cycle: HourCycle::H12,
                        ..ClockOptions::default()
                    },
                })),
            ],
        },
        ..config(28, 14)
    };
    h.reload(with(95));
    let second = h.runtime.status().published();
    assert!(second.sequence > first.sequence);
    assert_eq!(second.layout.end, [
        BarLayoutItem::Cpu {
            id: "cpu".into(),
            options: CpuOptions { temperature_alert: 95 }
        },
        BarLayoutItem::Clock {
            id: "clock".into(),
            options: ClockOptions {
                show_date: true,
                show_seconds: false,
                hour_cycle: HourCycle::H12,
            },
        },
    ]);
    assert_eq!(second.generation, first.generation, "no restart");
    assert_eq!(h.windows.opened(), 1, "the panel is not recreated");
    assert!(lock(&h.windows.closed).is_empty());

    let rejected = h.registry.reload(h.handle, with(0));
    assert!(rejected.is_err(), "an out-of-range option is rejected");
    let third = h.runtime.status().published();
    // The sequence is not compared: a provider reading may publish meanwhile.
    assert_eq!(third.layout, second.layout);
    assert_eq!(third.generation, second.generation);
}

#[test]
fn a_stalled_main_queue_bounds_the_menu_worker_end_and_keeps_it_owned() {
    struct Stalled(AtomicBool);
    impl MainThreadExecutor for Stalled {
        fn is_main_thread(&self) -> bool { false }

        fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            if !self.0.load(Ordering::SeqCst) {
                job();
            }
            Ok(())
        }
    }
    let backend = FakeMenuBarBackend::new(hidden_sample());
    let main = Arc::new(Stalled(AtomicBool::new(true)));
    let control = MenuBarControl::with_budget(
        Arc::clone(&main) as Arc<dyn MainThreadExecutor>,
        Arc::new(backend.clone()),
        Duration::from_millis(80),
    );
    let published = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&published);
    let monitor = crate::MenuMonitor::start(
        control,
        Arc::new(move |observation| lock(&sink).push(observation)),
        Duration::from_millis(3),
        Duration::from_millis(3),
    )
    .expect("start");
    let begun = Instant::now();
    assert!(monitor.end(Duration::from_millis(500)) || begun.elapsed() < Duration::from_secs(2));
    assert!(begun.elapsed() < Duration::from_secs(2), "end is bounded");
    assert_eq!(
        backend.observers(),
        0,
        "nothing registered while main is stalled"
    );
    main.0.store(false, Ordering::SeqCst);
    assert!(monitor.end(Duration::from_millis(500)));
    assert!(lock(&published).is_empty() || lock(&published).iter().all(|o| o.reason.is_some()));
}

// ---------------------------------------------------------------- providers

mod provider_runtime {
    use std::io;

    use stache_macos::system::testing::{FakeBattery, FakeCpuUsage, FakeSmc, FakeWifi, Script};
    use stache_macos::system::{
        BatteryPowerState, BatteryReadout, InterfaceName, SystemReadError, WifiInterfaceReading,
        WifiPower,
    };
    use stache_sdk::commands::{
        ChildProcess, Clock, CommandEnvironment, CommandExecutor, ProcessFactory, SpawnSpec,
        SystemClock,
    };

    use super::*;
    use crate::{BatteryChargeState, ProviderTiming, SsidGate, WifiStatus};

    struct NoSpawn(AtomicUsize);

    impl ProcessFactory for NoSpawn {
        fn spawn(&self, _: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::other("tests never spawn"))
        }
    }

    struct Rig {
        parts: ProviderParts,
        usage: Arc<Script<Result<f32, SystemReadError>>>,
        battery: Arc<Script<Result<Option<BatteryReadout>, SystemReadError>>>,
        wifi: Arc<Script<Result<Vec<WifiInterfaceReading>, SystemReadError>>>,
        spawns: Arc<NoSpawn>,
    }

    fn fast() -> ProviderTiming {
        let tick = Duration::from_millis(4);
        ProviderTiming {
            cpu: tick,
            cpu_warmup: Duration::from_millis(1),
            battery_charging: tick,
            battery_idle: tick,
            wifi: tick,
            wifi_min_gap: Duration::from_millis(1),
            tool_deadline: Duration::from_millis(200),
            join: Duration::from_millis(150),
        }
    }

    fn rig() -> Rig {
        let cpu = FakeCpuUsage::new(10.0);
        let usage = Arc::clone(&cpu.0);
        let battery = Script::new(Ok(Some(BatteryReadout {
            percent: 80.0,
            state: BatteryPowerState::Discharging,
        })));
        let wifi = Script::new(Ok(vec![WifiInterfaceReading {
            name: InterfaceName::parse("en0").expect("name"),
            power: WifiPower::On,
            rssi: Some(-55),
        }]));
        let spawns = Arc::new(NoSpawn(AtomicUsize::new(0)));
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let executor = Arc::new(CommandExecutor::with_parts(
            Arc::clone(&spawns) as Arc<dyn ProcessFactory>,
            Arc::clone(&clock),
            CommandEnvironment::default(),
        ));
        let parts = ProviderParts {
            cpu: Arc::new(cpu),
            smc: Arc::new(FakeSmc::new(Some(48.0))),
            battery: Arc::new(FakeBattery(Arc::clone(&battery))),
            wifi: Arc::new(FakeWifi(Arc::clone(&wifi))),
            executor,
            clock,
            ipconfig: std::path::PathBuf::from("/usr/sbin/ipconfig"),
            ssid: SsidGate::Off,
            signals: None,
            timing: fast(),
            media: None,
        };
        Rig {
            parts,
            usage,
            battery,
            wifi,
            spawns,
        }
    }

    fn harness(config: BarConfig, displays: Vec<DisplayInfo>, rig: &Rig) -> Harness {
        Harness::build(
            Native::Auto,
            config,
            displays,
            BarTiming::default(),
            None,
            Some(rig.parts.clone()),
        )
    }

    fn calls(rig: &Rig) -> usize { rig.usage.calls() + rig.battery.calls() + rig.wifi.calls() }

    #[test]
    fn disabled_paused_zero_size_and_no_primary_own_no_provider_work() {
        let r = rig();
        let off = BarConfig {
            enabled: false,
            ..config(28, 14)
        };
        let h = harness(off, vec![builtin()], &r);
        h.start();
        let zero = harness(config(0, 14), vec![builtin()], &r);
        zero.start();
        assert_eq!(zero.status(), ModuleStatus::Running);
        let none = harness(config(28, 14), vec![], &r);
        none.start();
        thread::sleep(Duration::from_millis(60));
        assert_eq!(calls(&r), 0, "no read happened in any of the three states");
        assert!(zero.runtime.status().published().cpu.is_none());

        // Paused: reads happen while running and stop at the pause.
        let paused = harness(config(28, 14), vec![builtin()], &r);
        paused.start();
        paused.wait_until("a sample", |h| h.runtime.status().published().cpu.is_some());
        paused.registry.pause(paused.handle).expect("pause");
        let after = calls(&r);
        thread::sleep(Duration::from_millis(60));
        assert_eq!(calls(&r), after, "a paused Bar reads nothing");
        let published = paused.runtime.status().published();
        assert!(published.cpu.is_none() && published.battery.is_none() && published.wifi.is_none());
        assert_eq!(r.spawns.0.load(Ordering::SeqCst), 0, "no child in any state");
    }

    #[test]
    fn samples_flow_through_the_published_cell_and_unchanged_samples_do_not_emit() {
        let r = rig();
        let h = harness(config(28, 14), vec![builtin()], &r);
        h.start();
        h.wait_until("all three readings", |h| {
            let view = h.runtime.status().published();
            view.cpu.is_some() && view.battery.is_some() && view.wifi.is_some()
        });
        let view = h.runtime.status().published();
        assert_eq!(view.cpu.map(|cpu| cpu.usage), Some(10));
        assert_eq!(
            view.battery.map(|b| (b.percentage, b.state)),
            Some((80, BatteryChargeState::Discharging))
        );
        assert_eq!(view.wifi.as_ref().map(|w| w.status), Some(WifiStatus::Connected));
        // Steady samples bump nothing.
        h.wait_until("steady state", |h| {
            let first = h.sequence();
            thread::sleep(Duration::from_millis(30));
            h.sequence() == first
        });
        let steady = h.sequence();
        let reads = calls(&r);
        thread::sleep(Duration::from_millis(40));
        assert!(calls(&r) > reads, "the providers kept sampling");
        assert_eq!(h.sequence(), steady, "unchanged samples emit nothing");
        // A changed sample bumps the sequence once per change.
        r.usage.set(Ok(55.0));
        h.wait_until("a new CPU value", |h| {
            h.runtime.status().published().cpu.map(|cpu| cpu.usage) == Some(55)
        });
        assert!(h.sequence() > steady);
    }

    #[test]
    fn a_hung_provider_never_delays_the_others_or_a_bridge_read() {
        let r = rig();
        let h = harness(config(28, 14), vec![builtin()], &r);
        r.usage.hold();
        h.start();
        assert!(
            r.usage.wait_inside(1, Duration::from_secs(5)),
            "the CPU read is parked"
        );
        h.wait_until("battery and wifi", |h| {
            let view = h.runtime.status().published();
            view.battery.is_some() && view.wifi.is_some()
        });
        r.battery.set(Ok(Some(BatteryReadout {
            percent: 20.0,
            state: BatteryPowerState::Empty,
        })));
        h.wait_until("a new battery value", |h| {
            h.runtime.status().published().battery.map(|b| b.percentage) == Some(20)
        });
        let begun = Instant::now();
        for _ in 0..100 {
            let _ = h.runtime.status().published();
            let _ = h.runtime.status().read();
        }
        assert!(
            begun.elapsed() < Duration::from_secs(2),
            "reads never wait on a provider"
        );
        assert!(
            h.runtime.status().published().cpu.is_none(),
            "nothing was invented"
        );
        // Pausing with the read still parked is bounded and the owner is retained.
        let begun = Instant::now();
        let _ = h.registry.pause(h.handle);
        assert!(begun.elapsed() < Duration::from_secs(2), "stop is bounded");
        assert!(h.runtime.status().published().battery.is_none());
        r.usage.release();
        h.wait_until("the owed worker to settle", |h| h.runtime.finish_shutdown());
        let after = r.usage.calls();
        thread::sleep(Duration::from_millis(40));
        assert_eq!(r.usage.calls(), after, "a stopped worker never samples again");
    }

    #[test]
    fn a_late_completion_cannot_republish_after_the_stop() {
        let r = rig();
        let h = harness(config(28, 14), vec![builtin()], &r);
        h.start();
        h.wait_until("a battery reading", |h| {
            h.runtime.status().published().battery.is_some()
        });
        r.battery.hold();
        r.battery.set(Ok(Some(BatteryReadout {
            percent: 5.0,
            state: BatteryPowerState::Empty,
        })));
        assert!(r.battery.wait_inside(1, Duration::from_secs(5)));
        // The parked read cannot be joined within the bound: the pause reports the
        // owed worker, which stays owned.
        let _ = h.registry.pause(h.handle);
        let sequence = h.sequence();
        r.battery.release();
        thread::sleep(Duration::from_millis(60));
        let published = h.runtime.status().published();
        assert!(
            published.battery.is_none(),
            "the late value never reached the cell"
        );
        assert_eq!(h.sequence(), sequence);
    }

    #[test]
    fn resume_restarts_the_providers_with_fresh_readings() {
        let r = rig();
        let h = harness(config(28, 14), vec![builtin()], &r);
        h.start();
        h.wait_until("a reading", |h| h.runtime.status().published().cpu.is_some());
        h.registry.pause(h.handle).expect("pause");
        assert!(h.runtime.status().published().cpu.is_none());
        let before = calls(&r);
        h.registry.resume(h.handle).expect("resume");
        h.wait_until("readings again", |h| {
            h.runtime.status().published().cpu.is_some()
        });
        assert!(calls(&r) > before);
    }

    #[test]
    fn unreadable_providers_publish_typed_conditions_and_keep_the_visuals_unresolved() {
        let r = rig();
        r.battery.set(Err(SystemReadError::new("iokit")));
        let h = harness(config(28, 14), vec![builtin()], &r);
        h.start();
        h.wait_until("a battery condition", |h| {
            h.runtime
                .status()
                .read()
                .providers
                .iter()
                .any(|note| note.provider == crate::ProviderKind::Battery)
        });
        assert!(
            h.runtime.status().published().battery.is_none(),
            "no charge was invented"
        );
        let notes = h.runtime.status().read().providers;
        assert_eq!(
            notes
                .iter()
                .filter(|note| note.provider == crate::ProviderKind::Battery)
                .count(),
            1,
            "deduplicated"
        );
    }
}

#[path = "weather/tests.rs"]
mod weather_tests;
