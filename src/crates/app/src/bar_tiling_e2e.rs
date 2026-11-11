//! End-to-end Tiling strip scenarios on the user's setup, with the **real**
//! Tiling runtime (the production module, [`TilingFactory`] and its exact
//! focus path) next to the real Bar in one registry.
//!
//! Only the native edges are fake: [`FakeWindowBackend`] for windows (frame
//! writes echo back like macOS reports them), [`FakeApplicationBackend`] for
//! the running applications and exact activation, the rig's display backend
//! (a built-in primary 1512x982 and an external 2560x1440 left of it, with the
//! menu bar auto-hidden: no native top inset) and the fake panel host.
//!
//! `bar.toml` is height 28 / padding 14 (top reservation 42) and `tiling.toml`
//! has outer gaps 14 and inner gaps 10. The scenarios assert what Tiling
//! writes (frames) and what the Bar UI receives (`bar_snapshot`, `BarChanged`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead};
use stache_macos::windows::{
    RuntimeEpoch, SnapshotOrigin, TabBar, TabBarId, TabBarRead, TabInfo, WindowChange, WindowEvent,
    WindowEventKind, WindowInstance, WindowSnapshot,
};
use stache_macos::{ApplicationBackend, ApplicationControl, ApplicationId, FakeAppKey};
use stache_tiling::{
    FakeBackendCall, FakeWindowBackend, Rect, TilingConfig, TilingParts, TilingRuntime,
    WindowBackend as _,
};

use super::*;
use crate::bar_tiling::{
    BarTilingOutcome, BarTilingRequest, BarTilingTarget, BarTilingUnavailable, StripApp,
    StripWorkspace, TilingStripView,
};
use crate::host::TilingFactory;

/// The user's Tiling setup, reduced to three workspaces: two on the built-in
/// primary (`main`) and one on the external display (`secondary`).
const TILING: &str = r#"enabled = true

[gaps]
inner = 10
outer = 14

[[workspaces]]
name = "terminal"
layout = "monocle"
screen = "main"
rules = [{ app_id = "com.mitchellh.ghostty" }]

[[workspaces]]
name = "coding"
layout = "dwindle"
screen = "main"
rules = [{ app_id = "dev.zed.Zed" }, { app_id = "com.apple.finder" }]

[[workspaces]]
name = "browser"
layout = "monocle"
screen = "secondary"
rules = [{ app_id = "com.google.Chrome" }]
"#;

const GHOSTTY: &str = "com.mitchellh.ghostty";
const ZED: &str = "dev.zed.Zed";
const FINDER: &str = "com.apple.finder";
const CHROME: &str = "com.google.Chrome";

/// The real Tiling module over fakes, registered in the rig's registry.
struct TilingKit {
    windows: Arc<FakeWindowBackend>,
    runtime: TilingRuntime,
    factory: Arc<TilingFactory>,
    control: Arc<dyn ModuleControl>,
    keys: BTreeMap<&'static str, FakeAppKey>,
    sequence: Mutex<u64>,
}

fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(2));
    }
}

fn frame_read(rect: Rect) -> FrameRead {
    FrameRead {
        position: AxReading::Value(AxPoint { x: rect.x, y: rect.y }),
        size: AxReading::Value(AxSize {
            width: rect.width,
            height: rect.height,
        }),
    }
}

fn window_snapshot(token: WindowInstance, title: &str, cg: u32, at: Rect) -> WindowSnapshot {
    WindowSnapshot {
        window: token,
        cg_window_id: Some(cg),
        title: AxReading::Value(title.to_owned()),
        role: AxReading::Value("AXWindow".to_owned()),
        subrole: AxReading::Value("AXStandardWindow".to_owned()),
        frame: frame_read(at),
        minimized: AxReading::Value(false),
        fullscreen: AxReading::Value(false),
        resizable: AxReading::Value(true),
        tabs: TabInfo::None,
        tab_members: Vec::new(),
        tab_bar: TabBarRead::NotRead,
        observed: true,
    }
}

impl Rig {
    /// Registers the production Tiling module over fakes in this rig's
    /// registry and store, with `file` as `tiling.toml`, and gives the Bar
    /// bridge its exact focus path (as `lib.rs` composes them).
    fn add_tiling(&self, file: Option<&str>) -> TilingKit { self.add_tiling_with(file, None) }

    /// [`Self::add_tiling`] over `clock` (the actor's clock).
    fn add_tiling_with(
        &self,
        file: Option<&str>,
        clock: Option<Arc<dyn stache_tiling::Clock>>,
    ) -> TilingKit {
        if let Some(file) = file {
            std::fs::write(self.config.module_file(TilingConfig::ID), file).expect("tiling.toml");
        }
        let mut keys = BTreeMap::new();
        for (pid, (bundle, name)) in [
            (GHOSTTY, "Ghostty"),
            (ZED, "Zed"),
            (FINDER, "Finder"),
            (CHROME, "Google Chrome"),
        ]
        .into_iter()
        .enumerate()
        {
            let key =
                self.apps.add_running_with_pid(bundle, 500 + i32::try_from(pid).expect("pid"));
            self.apps.set_name(key, Some(name));
            keys.insert(bundle, key);
        }
        let windows = Arc::new(FakeWindowBackend::default());
        windows.script_echo_frames(true);
        let mut parts = TilingParts::new(
            Arc::clone(&windows) as Arc<dyn stache_tiling::WindowBackend>,
            Arc::new(NoTilingApps),
            stache_tiling::DisplayProjection::new(sdk_display_id),
            "com.example.stache",
        );
        parts.applications = Some(Arc::new(stache_tiling::ControlActionsSource::new(
            ApplicationControl::with_acknowledgement_budget(
                Arc::clone(&self.main) as Arc<dyn MainThreadExecutor>,
                Arc::new(self.apps.clone()) as Arc<dyn ApplicationBackend>,
                BUDGET,
            ),
        )));
        parts.displays = Some(DisplayControl::with_budget(
            Arc::clone(&self.main) as Arc<dyn MainThreadExecutor>,
            Arc::new(self.backend.clone()),
            BUDGET,
        ));
        if let Some(clock) = clock {
            parts.clock = clock;
        }
        parts.timing.hop = Duration::from_millis(250);
        parts.timing.poll = Duration::from_millis(3);
        let runtime = TilingRuntime::new(parts);
        let factory = Arc::new(TilingFactory::new(runtime.clone()));
        factory.bind_registry(&self.registry);
        let control = factory.register(&self.registry, &self.store).expect("register tiling");
        self.bridge.set_tiling_focus(factory.focus());
        TilingKit {
            windows,
            runtime,
            factory,
            control,
            keys,
            sequence: Mutex::new(0),
        }
    }

    fn tiling_strip(&self) -> Option<TilingStripView> { self.available().tiling }

    fn wait_strip(&self, what: &str, condition: impl Fn(&TilingStripView) -> bool) {
        self.wait(what, |rig| {
            rig.tiling_strip().is_some_and(|strip| condition(&strip))
        });
    }

    fn click(&self, action: BarTilingTarget) -> BarTilingOutcome {
        let Some(TilingStripView::Available { generation, sequence, .. }) = self.tiling_strip()
        else {
            panic!("the strip is not available")
        };
        self.bridge
            .tiling_action(&self.label(), &BarTilingRequest { generation, sequence, action })
    }

    fn tiling_snapshot(&self) -> stache_sdk::ModuleSnapshot {
        self.registry
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.id == TilingConfig::ID)
            .expect("tiling snapshot")
    }
}

/// The run's applications come from the application observation.
struct NoTilingApps;

impl stache_tiling::AppDirectory for NoTilingApps {
    fn running(&self) -> Vec<stache_tiling::AppInfo> { Vec::new() }

    fn describe(&self, _: ApplicationId) -> Option<stache_tiling::AppInfo> { None }
}

impl TilingKit {
    fn tracking(&self) -> Arc<stache_tiling::TrackingRuntime> {
        self.runtime.tracking().expect("tiling is running")
    }

    fn snapshot(&self) -> Arc<stache_tiling::TilingSnapshot> { self.tracking().snapshot() }

    fn epoch(&self) -> RuntimeEpoch { self.tracking().epoch() }

    /// The opaque id of the tracked application `bundle`.
    fn application(&self, bundle: &str) -> ApplicationId {
        let mut found = None;
        wait_for(&format!("{bundle} to be known"), || {
            found = self
                .snapshot()
                .apps
                .iter()
                .find(|app| app.info.bundle_id == bundle)
                .map(|app| app.info.application);
            found.is_some()
        });
        found.expect("application")
    }

    fn push(&self, kind: WindowEventKind) {
        let sink = self.windows.sink().expect("the backend sink");
        let sequence = {
            let mut sequence = lock(&self.sequence);
            *sequence += 1;
            *sequence
        };
        assert!(sink.push(WindowEvent {
            epoch: self.epoch(),
            sequence,
            kind,
        }));
    }

    /// Creates a window of `bundle` with a unique per-epoch `serial`.
    fn open(&self, bundle: &str, serial: u64, title: &str, cg: u32) -> WindowInstance {
        let application = self.application(bundle);
        let token =
            WindowInstance::for_testing(self.windows.adapter(), self.epoch(), application, serial);
        let snapshot = window_snapshot(token, title, cg, Rect::new(100.0, 100.0, 800.0, 600.0));
        // A live window answers a re-read like macOS does (the fake's default
        // is a stale token).
        self.windows.script_snapshot(
            token,
            stache_macos::windows::SnapshotOutcome::Snapshot(Box::new(snapshot.clone())),
        );
        self.push(WindowEventKind::Snapshot {
            snapshot,
            origin: SnapshotOrigin::Created,
        });
        token
    }

    /// Reports a window of `bundle` carrying `tab_bar` (a new tab or a listed
    /// group member, per `origin`) at `at`; a re-read answers the same.
    fn tabbed(
        &self,
        token: WindowInstance,
        title: &str,
        cg: u32,
        at: Rect,
        tab_bar: TabBarRead,
        origin: SnapshotOrigin,
    ) {
        let mut snapshot = window_snapshot(token, title, cg, at);
        snapshot.tab_bar = tab_bar;
        self.windows.script_snapshot(
            token,
            stache_macos::windows::SnapshotOutcome::Snapshot(Box::new(snapshot.clone())),
        );
        self.push(WindowEventKind::Snapshot { snapshot, origin });
    }

    fn enumerated(&self, bundle: &str, windows: usize) {
        let application = self.application(bundle);
        self.push(WindowEventKind::EnumerationComplete {
            application,
            windows,
            truncated: false,
        });
    }

    fn change(&self, window: WindowInstance, change: WindowChange) {
        self.push(WindowEventKind::Window { window, change });
    }

    /// The last frame Tiling wrote for `window`.
    fn frame(&self, window: WindowInstance) -> Option<Rect> {
        self.windows
            .written_frames()
            .into_iter()
            .rev()
            .find(|(written, _)| *written == window)
            .map(|(_, rect)| rect)
    }

    fn wait_frame(&self, window: WindowInstance, expected: Rect) {
        wait_for(&format!("the frame {expected:?}"), || {
            self.frame(window).is_some_and(|frame| close(frame, expected))
        });
    }

    fn status(&self) -> stache_tiling::TilingRuntimeStatus { self.runtime.status().read() }
}

fn close(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() < 0.5
        && (a.y - b.y).abs() < 0.5
        && (a.width - b.width).abs() < 0.5
        && (a.height - b.height).abs() < 0.5
}

/// Boots Bar and Tiling and opens the user's windows:
/// Ghostty on `terminal`, Zed + Finder on `coding`, Chrome on `browser`.
struct World {
    rig: Rig,
    kit: TilingKit,
    ghostty: WindowInstance,
    zed: WindowInstance,
    finder: WindowInstance,
    chrome: WindowInstance,
}

impl World {
    fn boot() -> Self { Self::boot_with(Some(BAR)) }

    fn boot_with(bar: Option<&str>) -> Self { Self::over(Rig::new(bar), None) }

    /// Registers Tiling in `rig`, starts both and opens the windows.
    fn over(rig: Rig, clock: Option<Arc<dyn stache_tiling::Clock>>) -> Self {
        let kit = rig.add_tiling_with(Some(TILING), clock);
        rig.start();
        assert_eq!(
            kit.runtime.status().read().state,
            stache_tiling::TilingRuntimeState::Running
        );
        wait_for("tiling tracks the applications", || {
            kit.windows
                .calls()
                .iter()
                .filter(|call| matches!(call, FakeBackendCall::Track(..)))
                .count()
                >= 4
        });
        let ghostty = kit.open(GHOSTTY, 1, "~/Projects", 11);
        let zed = kit.open(ZED, 2, "main.rs — stache", 21);
        let finder = kit.open(FINDER, 3, "Downloads", 31);
        let chrome = kit.open(CHROME, 4, "Docs", 41);
        for bundle in [GHOSTTY, ZED, FINDER, CHROME] {
            kit.enumerated(bundle, 1);
        }
        wait_for("enumeration completes", || {
            kit.snapshot().phase == stache_tiling::Phase::Complete
        });
        Self {
            rig,
            kit,
            ghostty,
            zed,
            finder,
            chrome,
        }
    }
}

// -------------------------------------------------------------- expectations

/// A single tiled window on the built-in primary under the Bar: the host
/// reservation (42 = height 28 + padding 14) plus the outer gap 14.
const PRIMARY: Rect = Rect::new(14.0, 56.0, 1484.0, 912.0);
/// The same window without a Bar reservation: only the outer gap.
const PRIMARY_NO_BAR: Rect = Rect::new(14.0, 14.0, 1484.0, 954.0);
/// The external display (2560x1440 left of the primary, bottom-aligned, so
/// its top is at y = 982 - 1440 = -458): outer gaps only, Bar reserves only
/// its own display.
const EXTERNAL: Rect = Rect::new(-2546.0, -444.0, 2532.0, 1412.0);
/// Two windows side by side on `coding` (dwindle) with the inner gap 10.
const LEFT: Rect = Rect::new(14.0, 56.0, 737.0, 912.0);
const RIGHT: Rect = Rect::new(761.0, 56.0, 737.0, 912.0);

/// The parts of an available strip.
struct Strip {
    generation: u64,
    sequence: u64,
    workspaces: Vec<StripWorkspace>,
    apps: Vec<StripApp>,
    focused_app: Option<StripApp>,
    focused_workspace: Option<String>,
}

fn strip(view: &TilingStripView) -> Option<Strip> {
    match view {
        TilingStripView::Available {
            generation,
            sequence,
            workspaces,
            apps,
            focused_app,
            focused_workspace,
        } => Some(Strip {
            generation: crate::bar_tiling::exact_decimal(generation).expect("a decimal generation"),
            sequence: crate::bar_tiling::exact_decimal(sequence).expect("a decimal sequence"),
            workspaces: workspaces.clone(),
            apps: apps.clone(),
            focused_app: focused_app.clone(),
            focused_workspace: focused_workspace.clone(),
        }),
        TilingStripView::Absent | TilingStripView::Loading => None,
    }
}

fn app(window: WindowInstance, name: &str, title: &str) -> StripApp {
    StripApp {
        key: stache_tiling::window_key(window),
        app_name: name.to_owned(),
        title: title.to_owned(),
    }
}

fn names(apps: &[StripApp]) -> Vec<&str> { apps.iter().map(|app| app.app_name.as_str()).collect() }

fn focus_workspace(name: &str) -> BarTilingTarget {
    BarTilingTarget::FocusWorkspace { name: name.to_owned() }
}

fn focus_window(window: WindowInstance) -> BarTilingTarget {
    BarTilingTarget::FocusWindow {
        window: stache_tiling::window_key(window),
    }
}

/// Every `BarChanged` sequence the Bar window received, in order.
fn event_sequences(rig: &Rig) -> Vec<u64> {
    rig.sink
        .events()
        .iter()
        .map(|(_, event)| event.sequence.parse::<u64>().expect("a decimal sequence"))
        .collect()
}

fn assert_strictly_increasing(sequences: &[u64]) {
    assert!(
        sequences.windows(2).all(|pair| pair[1] > pair[0]),
        "BarChanged sequences must strictly increase: {sequences:?}"
    );
}

/// The `tiling` field of every `BarChanged` view the Bar window received.
fn event_strips(rig: &Rig) -> Vec<Option<TilingStripView>> {
    rig.sink
        .events()
        .into_iter()
        .filter_map(|(_, event)| match event.outcome {
            Some(BarSnapshotOutcome::Available { view }) => Some(view.tiling),
            _ => None,
        })
        .collect()
}

fn is_absent(view: &TilingStripView) -> bool { *view == TilingStripView::Absent }

/// An actor clock that parks the Tiling actor thread while closed: a busy or
/// slow actor, without touching anything else.
struct GateClock {
    closed: Mutex<bool>,
    changed: std::sync::Condvar,
}

impl GateClock {
    fn new(closed: bool) -> Arc<Self> {
        Arc::new(Self {
            closed: Mutex::new(closed),
            changed: std::sync::Condvar::new(),
        })
    }

    fn set(&self, closed: bool) {
        *lock(&self.closed) = closed;
        self.changed.notify_all();
    }
}

impl stache_tiling::Clock for GateClock {
    fn now(&self) -> Instant {
        if thread::current().name() == Some(stache_tiling::ACTOR_THREAD_NAME) {
            let closed = lock(&self.closed);
            drop(
                self.changed
                    .wait_while(closed, |closed| *closed)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
        Instant::now()
    }
}

// --------------------------------------------------------------- scenarios

/// TI-10-01/TI-10-02: the user's setup. Tiling plans under the Bar on the
/// primary only, and the UI receives the strip through `bar_snapshot` and
/// `BarChanged`; clicks round-trip through the real runtime to the fakes.
#[test]
fn e2e_tiling_plans_under_the_bar_and_the_ui_receives_the_strip_and_clicks_round_trip() {
    let World {
        rig,
        kit,
        ghostty,
        zed,
        finder,
        chrome,
    } = World::boot();
    rig.wait_loaded();

    // What Tiling writes: the primary starts at y = 42 + 14, the external
    // display has no Bar inset, every frame is inset by the gaps.
    kit.wait_frame(ghostty, PRIMARY);
    kit.wait_frame(chrome, EXTERNAL);
    assert_eq!(rig.top(1), Some(42.0));
    assert_eq!(rig.top(2), Some(0.0));
    assert_eq!(kit.frame(zed), None, "coding is not visible yet");

    // What the UI receives.
    rig.wait_strip("the first published strip", |view| {
        strip(view).is_some_and(|strip| strip.focused_workspace.as_deref() == Some("terminal"))
    });
    let first = strip(&rig.tiling_strip().expect("resolved")).expect("available");
    assert_eq!(first.workspaces, vec![
        StripWorkspace {
            name: "terminal".to_owned(),
            focused: true
        },
        StripWorkspace {
            name: "coding".to_owned(),
            focused: false
        },
        StripWorkspace {
            name: "browser".to_owned(),
            focused: false
        },
    ]);
    assert_eq!(first.apps, vec![app(ghostty, "Ghostty", "~/Projects")]);
    assert_eq!(first.focused_app, None, "nothing reported focus yet");
    assert!(first.generation > 0 && first.sequence > 0);
    rig.wait("an event carrying the strip", |rig| {
        event_strips(rig).iter().flatten().any(|view| strip(view).is_some())
    });
    let json = serde_json::to_value(rig.available()).expect("encode");
    assert_eq!(json["tiling"]["kind"], "available");
    assert!(json["tiling"]["generation"].is_string() && json["tiling"]["sequence"].is_string());
    assert_eq!(json["tiling"]["apps"][0]["appName"], "Ghostty");

    // Click the `coding` workspace: Tiling switches, the strip follows.
    assert_eq!(rig.click(focus_workspace("coding")), BarTilingOutcome::Applied);
    rig.wait_strip("coding focused", |view| {
        strip(view).is_some_and(|strip| strip.focused_workspace.as_deref() == Some("coding"))
    });
    let coding = strip(&rig.tiling_strip().expect("resolved")).expect("available");
    assert_eq!(coding.apps, vec![
        app(zed, "Zed", "main.rs — stache"),
        app(finder, "Finder", "Downloads"),
    ]);
    assert!(coding.workspaces.iter().any(|ws| ws.name == "coding" && ws.focused));
    kit.wait_frame(zed, LEFT);
    kit.wait_frame(finder, RIGHT);
    assert_eq!(rig.click(focus_workspace("coding")), BarTilingOutcome::NoOp);

    // Click Chrome (another workspace on the other display): the exact
    // window is activated through the exact application instance.
    let activations = rig.apps.exact_activation_requests().len();
    assert_eq!(rig.click(focus_window(chrome)), BarTilingOutcome::Applied);
    wait_for("Chrome's exact activation", || {
        rig.apps.exact_activation_requests().len() > activations
    });
    assert_eq!(
        rig.apps.exact_activation_requests().last(),
        kit.keys.get(CHROME),
        "the exact Chrome instance was activated"
    );
    assert!(
        kit.windows.calls().contains(&FakeBackendCall::Action(chrome)),
        "the exact Chrome window was raised"
    );
    // macOS then reports the focus.
    kit.change(chrome, WindowChange::Focused);
    rig.wait_strip("Chrome focused", |view| {
        strip(view).is_some_and(|strip| {
            strip.focused_app == Some(app(chrome, "Google Chrome", "Docs"))
                && strip.focused_workspace.as_deref() == Some("browser")
                && names(&strip.apps) == ["Google Chrome"]
        })
    });
    kit.wait_frame(chrome, EXTERNAL);
    assert_eq!(rig.click(focus_window(chrome)), BarTilingOutcome::NoOp);

    // Every BarChanged sequence strictly increased.
    assert_strictly_increasing(&event_sequences(&rig));
    let _ = rig.registry.shutdown();
}

/// Minimized and hidden windows leave the strip; frame-only activity never
/// wakes the watcher or emits `BarChanged`.
#[test]
fn e2e_minimized_and_hidden_windows_leave_the_strip_and_frame_only_activity_is_silent() {
    let World { rig, kit, zed, finder, .. } = World::boot();
    rig.wait_loaded();
    assert_eq!(rig.click(focus_workspace("coding")), BarTilingOutcome::Applied);
    rig.wait_strip("coding with two apps", |view| {
        strip(view).is_some_and(|strip| names(&strip.apps) == ["Zed", "Finder"])
    });
    kit.wait_frame(finder, RIGHT);

    kit.change(finder, WindowChange::Miniaturized);
    rig.wait_strip("the minimized window left", |view| {
        strip(view).is_some_and(|strip| names(&strip.apps) == ["Zed"])
    });
    kit.change(finder, WindowChange::Deminiaturized);
    rig.wait_strip("the window is back", |view| {
        strip(view).is_some_and(|strip| names(&strip.apps) == ["Zed", "Finder"])
    });
    kit.push(WindowEventKind::Application {
        application: kit.application(ZED),
        change: stache_macos::windows::ApplicationChange::Hidden,
    });
    rig.wait_strip("the hidden application left", |view| {
        strip(view).is_some_and(|strip| names(&strip.apps) == ["Finder"])
    });
    kit.push(WindowEventKind::Application {
        application: kit.application(ZED),
        change: stache_macos::windows::ApplicationChange::Shown,
    });
    rig.wait_strip("the shown application is back", |view| {
        strip(view).is_some_and(|strip| names(&strip.apps) == ["Zed", "Finder"])
    });
    kit.wait_frame(zed, LEFT);

    // Idle: let everything settle, then count.
    thread::sleep(Duration::from_millis(150));
    let wakeups = rig.bridge.tiling_wakeups();
    let version = rig.bridge.tiling_view().version;
    let events = rig.sink.events().len();
    let writes = kit.windows.written_frames().len();
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        rig.bridge.tiling_wakeups(),
        wakeups,
        "no timer wakes the watcher"
    );
    assert_eq!(rig.sink.events().len(), events, "an idle Bar emits nothing");

    // Frame-only activity: the user drags Zed; Tiling re-asserts its frame.
    for _ in 0..5 {
        kit.change(zed, WindowChange::Moved);
        kit.change(zed, WindowChange::Resized);
    }
    wait_for("Tiling handled the moves", || {
        kit.windows.written_frames().len() > writes
    });
    thread::sleep(Duration::from_millis(150));
    kit.wait_frame(zed, LEFT);
    assert_eq!(
        rig.bridge.tiling_wakeups(),
        wakeups,
        "frames never publish state"
    );
    assert_eq!(rig.bridge.tiling_view().version, version);
    assert_eq!(
        rig.sink.events().len(),
        events,
        "frame-only activity emits no BarChanged"
    );
    let _ = rig.registry.shutdown();
}

/// Disabling and re-enabling Tiling: absent, then a new generation; Bar keeps
/// running and its `BarChanged` sequence only grows.
#[test]
fn e2e_a_tiling_disable_and_enable_keeps_the_bar_sequence_increasing() {
    let World { rig, kit, ghostty, .. } = World::boot();
    rig.wait_loaded();
    rig.wait_strip("available", |view| strip(view).is_some());
    let before = strip(&rig.tiling_strip().expect("resolved")).expect("available");

    kit.control.set_enabled(&rig.registry, false).expect("disable tiling");
    rig.wait_strip("absent", is_absent);
    assert_eq!(rig.status(), ModuleStatus::Running, "Bar keeps running");
    assert_eq!(rig.top(1), Some(42.0), "Bar keeps its reservation");
    rig.wait("an absent event", |rig| {
        event_strips(rig).iter().flatten().any(is_absent)
    });

    kit.control.set_enabled(&rig.registry, true).expect("enable tiling");
    wait_for("tiling tracks again", || kit.runtime.tracking().is_some());
    rig.wait_strip("a new generation", |view| {
        strip(view).is_some_and(|strip| strip.generation > before.generation)
    });
    let after = strip(&rig.tiling_strip().expect("resolved")).expect("available");
    assert!(
        after.sequence >= 1,
        "the SDK sequence restarted for the new generation"
    );
    assert!(
        !after.apps.iter().any(|app| app.key == stache_tiling::window_key(ghostty)),
        "a previous generation's key never reappears"
    );
    rig.wait("the new generation's event", |rig| {
        event_strips(rig)
            .iter()
            .flatten()
            .any(|view| strip(view).is_some_and(|strip| strip.generation == after.generation))
    });
    assert_strictly_increasing(&event_sequences(&rig));
    let _ = rig.registry.shutdown();
}

/// Tiling running before its first publication is `loading`; a paused Tiling
/// is `absent` and a click is `unavailable{paused}` without resuming it.
#[test]
fn e2e_loading_before_the_first_publication_and_a_paused_click_never_resumes() {
    let clock = GateClock::new(true);
    let rig = Rig::new(Some(BAR));
    let kit = rig.add_tiling_with(
        Some(TILING),
        Some(Arc::clone(&clock) as Arc<dyn stache_tiling::Clock>),
    );
    rig.start();
    rig.wait_loaded();
    assert_eq!(rig.tiling_snapshot().status, ModuleStatus::Running);
    rig.wait_strip("loading", |view| *view == TilingStripView::Loading);
    rig.wait("a loading event", |rig| {
        event_strips(rig).iter().flatten().any(|view| *view == TilingStripView::Loading)
    });
    clock.set(false);
    rig.wait_strip("available", |view| strip(view).is_some());
    let rendered = strip(&rig.tiling_strip().expect("resolved")).expect("available");

    kit.control.pause(&rig.registry).expect("pause tiling");
    rig.wait_strip("absent while paused", is_absent);
    let request = BarTilingRequest {
        generation: rendered.generation.to_string(),
        sequence: rendered.sequence.to_string(),
        action: focus_workspace("coding"),
    };
    let file = std::fs::read(rig.config.module_file(TilingConfig::ID)).expect("tiling.toml");
    assert_eq!(
        rig.bridge.tiling_action(&rig.label(), &request),
        BarTilingOutcome::Unavailable {
            reason: BarTilingUnavailable::Paused
        }
    );
    let snapshot = rig.tiling_snapshot();
    assert!(
        snapshot.paused && snapshot.enabled,
        "a click never resumes or edits"
    );
    assert_eq!(
        std::fs::read(rig.config.module_file(TilingConfig::ID)).expect("file"),
        file
    );
    assert!(kit.runtime.tracking().is_none(), "Tiling stays stopped");
    let _ = rig.registry.shutdown();
}

/// A failed or disabled Tiling is `absent` while Bar keeps running and Keep
/// Awake keeps updating.
#[test]
fn e2e_a_failed_or_disabled_tiling_is_absent_while_bar_and_keep_awake_keep_updating() {
    let rig = Rig::new(Some(BAR));
    let awake = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    let kit = rig.add_tiling(Some(TILING));
    kit.windows
        .script_install_error(stache_macos::windows::InstallError::OwnerUnavailable);
    rig.start();
    rig.wait_loaded();
    assert!(matches!(rig.tiling_snapshot().status, ModuleStatus::Failed(_)));
    assert_eq!(rig.status(), ModuleStatus::Running);
    rig.wait_strip("absent after a failed start", is_absent);
    rig.wait_keep_awake(indicator(false, false, true));
    awake.probe.set(NativeLockState::Unlocked);
    assert!(awake.observation.deliver());
    rig.wait_keep_awake(indicator(true, false, true));
    assert_eq!(rig.tiling_strip(), Some(TilingStripView::Absent));

    // Disabled: still absent, and Keep Awake still updates.
    kit.control.set_enabled(&rig.registry, false).expect("disable tiling");
    awake.probe.set(NativeLockState::Locked);
    assert!(awake.observation.deliver());
    rig.wait_keep_awake(indicator(false, false, true));
    assert_eq!(rig.tiling_strip(), Some(TilingStripView::Absent));
    assert_eq!(rig.top(1), Some(42.0));
    assert_strictly_increasing(&event_sequences(&rig));
    let _ = rig.registry.shutdown();
}

/// A disabled Bar: Tiling keeps managing windows without the inset, and no
/// Bar worker or watcher exists.
#[test]
fn e2e_a_disabled_bar_leaves_tiling_managing_windows_without_the_inset() {
    let World { rig, kit, ghostty, chrome, .. } = World::boot_with(None);
    assert_eq!(rig.status(), ModuleStatus::Disabled);
    kit.wait_frame(ghostty, PRIMARY_NO_BAR);
    kit.wait_frame(chrome, EXTERNAL);
    assert!(!rig.bridge.worker_alive());
    assert!(
        !rig.bridge.tiling_watcher_started(),
        "a disabled Bar owns no watcher"
    );
    assert_eq!(
        rig.bridge.tiling_action("stache-bar-1", &BarTilingRequest {
            generation: "1".to_owned(),
            sequence: "1".to_owned(),
            action: focus_workspace("coding"),
        }),
        BarTilingOutcome::Unavailable {
            reason: BarTilingUnavailable::BarNotRunning
        }
    );
    assert!(kit.status().healthy);
    let _ = rig.registry.shutdown();
}

/// TI-10-02: the constant inset while the Bar slides away for the menu bar.
#[test]
fn e2e_the_inset_stays_constant_while_the_bar_slides_hidden() {
    let World { rig, kit, ghostty, .. } = World::boot();
    rig.wait_loaded();
    rig.wait_menu(MenuBarState::Hidden);
    kit.wait_frame(ghostty, PRIMARY);
    thread::sleep(Duration::from_millis(100));
    let writes = kit.windows.written_frames().len();
    for visible in [true, false, true] {
        rig.toggle_menu(visible);
        rig.wait_menu(if visible {
            MenuBarState::Visible
        } else {
            MenuBarState::Hidden
        });
        assert_eq!(rig.top(1), Some(42.0));
    }
    thread::sleep(Duration::from_millis(100));
    assert_eq!(kit.frame(ghostty), Some(PRIMARY));
    assert_eq!(
        kit.windows.written_frames().len(),
        writes,
        "sliding the Bar never re-plans Tiling"
    );
    let _ = rig.registry.shutdown();
}

/// TI-10-02: the inset is staged at panel open and held while the page loads;
/// a failed panel clears it and Tiling re-plans to full height.
#[test]
fn e2e_the_inset_is_held_while_the_page_loads_and_a_failed_panel_releases_it() {
    let rig = Rig::new(Some(BAR));
    rig.host.set_mode(HostMode::NeverLoads);
    let World { rig, kit, ghostty, .. } = World::over(rig, None);
    assert_eq!(rig.bar().panel, BarPanelState::Created);
    assert_eq!(rig.top(1), Some(42.0), "staged at open, held while loading");
    kit.wait_frame(ghostty, PRIMARY);
    rig.wait_failed();
    assert_eq!(rig.status(), ModuleStatus::Running, "Bar keeps running");
    assert_eq!(rig.top(1), Some(0.0), "the failed panel released the inset");
    kit.wait_frame(ghostty, PRIMARY_NO_BAR);
    let _ = rig.registry.shutdown();
}

/// TI-10-02: an exact-generation tombstone on Bar pause, disable and
/// re-enable makes Tiling re-plan each time.
#[test]
fn e2e_bar_pause_and_disable_tombstones_make_tiling_replan() {
    let World { rig, kit, ghostty, chrome, .. } = World::boot();
    rig.wait_loaded();
    kit.wait_frame(ghostty, PRIMARY);
    rig.control.pause(&rig.registry).expect("pause bar");
    kit.wait_frame(ghostty, PRIMARY_NO_BAR);
    rig.control.resume(&rig.registry).expect("resume bar");
    kit.wait_frame(ghostty, PRIMARY);
    rig.write("enabled = false\nheight = 28\npadding = 14\n");
    assert_eq!(rig.status(), ModuleStatus::Disabled);
    kit.wait_frame(ghostty, PRIMARY_NO_BAR);
    rig.write(BAR);
    kit.wait_frame(ghostty, PRIMARY);
    assert_eq!(
        kit.frame(chrome),
        Some(EXTERNAL),
        "the external display never moved"
    );
    let _ = rig.registry.shutdown();
}

/// TI-10-02: a stale release from an earlier Bar generation cannot clear its
/// successor's inset.
#[test]
fn e2e_a_stale_release_cannot_clear_the_successor() {
    let World { rig, kit, ghostty, .. } = World::boot();
    rig.wait_loaded();
    let old = rig.label();
    rig.host.hold_close.store(true, Ordering::SeqCst);
    rig.control.set_enabled(&rig.registry, false).expect("disable bar");
    kit.wait_frame(ghostty, PRIMARY_NO_BAR);
    rig.host.hold_close.store(false, Ordering::SeqCst);
    rig.control.set_enabled(&rig.registry, true).expect("enable bar");
    rig.wait("the successor panel", |rig| {
        rig.windows.live_label(BarConfig::ID).is_some_and(|label| label != old)
    });
    kit.wait_frame(ghostty, PRIMARY);
    // The old panel's close finally arrives.
    rig.windows.window_closed(&old);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(rig.top(1), Some(42.0));
    assert_eq!(kit.frame(ghostty), Some(PRIMARY), "the successor's inset holds");
    let _ = rig.registry.shutdown();
}

/// TI-10-02: Tiling follows the registry reservation only. A Bar height change
/// that goes through the registry re-plans; a `bar.toml` the registry never
/// accepted changes nothing (Tiling never reads Bar configuration; neither
/// crate depends on the other, see the Bar manifest test).
#[test]
fn e2e_tiling_follows_the_registry_reservation_and_never_reads_bar_config() {
    let World { rig, kit, ghostty, .. } = World::boot();
    rig.wait_loaded();
    kit.wait_frame(ghostty, PRIMARY);
    rig.write("enabled = true\nheight = 40\npadding = 14\n");
    assert_eq!(rig.top(1), Some(54.0));
    let taller = Rect::new(14.0, 68.0, 1484.0, 900.0);
    kit.wait_frame(ghostty, taller);

    // A file the registry never accepted: Tiling re-plans a new window with
    // the registry's 54, not the file's 74.
    std::fs::write(
        rig.config.module_file(BarConfig::ID),
        "enabled = true\nheight = 60\npadding = 14\n",
    )
    .expect("write");
    let second = kit.open(GHOSTTY, 9, "second", 19);
    kit.wait_frame(second, taller);
    kit.wait_frame(ghostty, taller);
    assert_eq!(rig.top(1), Some(54.0));
    let _ = rig.registry.shutdown();
}

/// The click is authorized against the live Bar window, rejects malformed
/// decimals and stale rendered state, and nothing of it reaches Tiling.
#[test]
#[allow(clippy::too_many_lines)] // One linear walk through every rejection.
fn e2e_the_click_authorizes_validates_and_revalidates_before_tiling() {
    let World { rig, kit, finder, .. } = World::boot();
    rig.wait_loaded();
    rig.wait_strip("available", |view| strip(view).is_some());
    let rendered = strip(&rig.tiling_strip().expect("resolved")).expect("available");
    let request = |generation: String, sequence: String, action: BarTilingTarget| {
        BarTilingRequest { generation, sequence, action }
    };
    let good = |action| {
        request(
            rendered.generation.to_string(),
            rendered.sequence.to_string(),
            action,
        )
    };
    let recorded = |kit: &TilingKit| {
        let diagnostics = &kit.snapshot().diagnostics;
        diagnostics.intents_applied + diagnostics.intents_noop + diagnostics.intents_rejected
    };
    let intents = recorded(&kit);
    let activations = rig.apps.exact_activation_requests().len();

    for label in [
        "stache-test-1",
        "stache-access-1",
        "stache-shell-1",
        "stache-bar-999",
        "stache-bar-",
        "x",
    ] {
        assert_eq!(
            rig.bridge.tiling_action(label, &good(focus_workspace("coding"))),
            BarTilingOutcome::Unauthorized,
            "{label}"
        );
    }
    for (generation, sequence) in [
        ("", "1"),
        ("+1", "1"),
        ("01", "1"),
        ("1", "-1"),
        ("1", "18446744073709551616"),
        ("1.0", "1"),
    ] {
        assert_eq!(
            rig.bridge.tiling_action(
                &rig.label(),
                &request(
                    generation.to_owned(),
                    sequence.to_owned(),
                    focus_workspace("coding")
                )
            ),
            BarTilingOutcome::Invalid,
            "{generation:?}/{sequence:?}"
        );
    }
    let label = rig.label();
    for stale in [
        request(
            (rendered.generation + 1).to_string(),
            "1".to_owned(),
            focus_workspace("coding"),
        ),
        request(
            (rendered.generation - 1).to_string(),
            "1".to_owned(),
            focus_workspace("coding"),
        ),
        request(
            rendered.generation.to_string(),
            (rendered.sequence + 1).to_string(),
            focus_workspace("coding"),
        ),
        good(focus_workspace("nowhere")),
        good(focus_workspace("Coding")),
        good(BarTilingTarget::FocusWindow { window: "1".to_owned() }),
        good(BarTilingTarget::FocusWindow { window: "not a key".to_owned() }),
    ] {
        assert_eq!(
            rig.bridge.tiling_action(&label, &stale),
            BarTilingOutcome::Stale,
            "{stale:?}"
        );
    }

    // A window that closed after the strip rendered: its key is stale.
    kit.change(finder, WindowChange::Destroyed);
    wait_for("Finder is gone", || {
        !kit.snapshot().windows.iter().any(|window| window.token == finder)
    });
    let current = strip(&rig.tiling_strip().expect("resolved")).expect("available");
    assert_eq!(
        rig.bridge.tiling_action(
            &label,
            &request(
                current.generation.to_string(),
                current.sequence.to_string(),
                focus_window(finder)
            )
        ),
        BarTilingOutcome::Stale
    );
    assert_eq!(recorded(&kit), intents, "nothing reached the actor");
    assert_eq!(rig.apps.exact_activation_requests().len(), activations);

    // A stale label after a Bar reopen, and a Bar that is not running.
    rig.control.pause(&rig.registry).expect("pause bar");
    assert_eq!(
        rig.bridge.tiling_action(&label, &good(focus_workspace("coding"))),
        BarTilingOutcome::Unavailable {
            reason: BarTilingUnavailable::BarNotRunning
        }
    );
    rig.control.resume(&rig.registry).expect("resume bar");
    rig.wait("the reopened panel", |rig| {
        rig.windows.live_label(BarConfig::ID).is_some_and(|new| new != label)
    });
    assert_eq!(
        rig.bridge.tiling_action(&label, &good(focus_workspace("coding"))),
        BarTilingOutcome::Unauthorized
    );
    assert_eq!(recorded(&kit), intents);
    let _ = rig.registry.shutdown();
}

/// A click while another is in flight is `busy` and never queues; a stuck
/// click never stalls `BarChanged` or `bar_snapshot`; shutdown during the wait
/// is `uncertain` and later clicks are `unavailable{shuttingDown}`.
#[test]
fn e2e_a_stuck_click_is_busy_for_others_never_stalls_the_bar_and_ends_at_shutdown() {
    let clock = GateClock::new(false);
    let World { rig, kit, chrome, .. } = World::over(
        Rig::new(Some(BAR)),
        Some(Arc::clone(&clock) as Arc<dyn stache_tiling::Clock>),
    );
    rig.wait_loaded();
    rig.wait_strip("available", |view| strip(view).is_some());
    let rendered = strip(&rig.tiling_strip().expect("resolved")).expect("available");
    let request = BarTilingRequest {
        generation: rendered.generation.to_string(),
        sequence: rendered.sequence.to_string(),
        action: focus_window(chrome),
    };

    // The actor is busy: the first click waits for its record.
    clock.set(true);
    let bridge = Arc::clone(&rig.bridge);
    let label = rig.label();
    let first = {
        let request = request.clone();
        let label = label.clone();
        thread::spawn(move || bridge.tiling_action(&label, &request))
    };
    thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    assert_eq!(
        rig.bridge.tiling_action(&label, &request),
        BarTilingOutcome::Busy
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "busy never waits"
    );

    // The Bar keeps serving while the click is stuck.
    let started = Instant::now();
    let _ = rig.available();
    assert!(started.elapsed() < Duration::from_millis(500));
    let events = rig.sink.events().len();
    rig.toggle_menu(true);
    rig.wait_menu(MenuBarState::Visible);
    rig.wait("a BarChanged while the click is stuck", |rig| {
        rig.sink.events().len() > events
    });

    // Host shutdown begins: the wait ends promptly and nothing is retried.
    let stopping = Instant::now();
    kit.factory.begin_shutdown();
    assert_eq!(first.join().expect("join"), BarTilingOutcome::Uncertain);
    assert!(stopping.elapsed() < Duration::from_secs(1));
    assert!(
        matches!(
            rig.bridge.tiling_action(&label, &request),
            BarTilingOutcome::Unavailable {
                reason: BarTilingUnavailable::ShuttingDown | BarTilingUnavailable::BarNotRunning
            }
        ),
        "a late click is refused without reaching Tiling"
    );
    clock.set(false);
    let _ = rig.registry.shutdown();
}

/// The watcher starts with Bar, has no timer and joins at shutdown.
#[test]
fn e2e_the_tiling_watcher_starts_with_bar_and_joins_at_shutdown() {
    let World { rig, .. } = World::boot();
    rig.wait_loaded();
    rig.wait("the watcher", |rig| rig.bridge.tiling_watcher_started());
    rig.wait_strip("available", |view| strip(view).is_some());
    let _ = rig.registry.shutdown();
    assert!(rig.bridge.finish_shutdown(), "the watcher joined");
    assert!(!rig.bridge.tiling_watcher_started());
}

// ------------------------------------------------- final integration (10d)

fn workspace_names(rig: &Rig) -> Option<Vec<String>> {
    rig.tiling_strip()
        .as_ref()
        .and_then(strip)
        .map(|strip| strip.workspaces.into_iter().map(|workspace| workspace.name).collect())
}

/// Rejected `tiling.toml` files keep the last accepted state: the strip and
/// the revision do not move. Accepted A–B–A edits each advance the revision
/// and the strip follows them in order.
#[test]
fn e2e_rejected_tiling_files_keep_the_strip_and_accepted_edits_stay_ordered() {
    let World { rig, .. } = World::boot();
    rig.wait_loaded();
    rig.wait_strip("available", |view| strip(view).is_some());
    let original = vec![
        "terminal".to_owned(),
        "coding".to_owned(),
        "browser".to_owned(),
    ];
    assert_eq!(workspace_names(&rig), Some(original.clone()));
    let revision = rig.tiling_snapshot().revision;
    let file = rig.config.module_file(TilingConfig::ID);

    for bad in [
        "enabled = \n[",
        "enabled = true\n[gaps]\ninner = \"wide\"\n",
    ] {
        std::fs::write(&file, bad).expect("write");
        let _ = rig.store.reload_one(TilingConfig::ID);
        thread::sleep(Duration::from_millis(150));
        assert_eq!(rig.tiling_snapshot().revision, revision, "{bad}");
        assert_eq!(rig.tiling_snapshot().status, ModuleStatus::Running, "{bad}");
        assert_eq!(workspace_names(&rig), Some(original.clone()), "{bad}");
    }

    // A -> B -> A: each accepted edit is a new revision and the strip follows.
    let renamed = TILING.replace("\"coding\"", "\"code\"");
    let mut last = revision;
    for (text, expected) in [
        (renamed.as_str(), ["terminal", "code", "browser"]),
        (TILING, ["terminal", "coding", "browser"]),
    ] {
        std::fs::write(&file, text).expect("write");
        rig.store.reload_one(TilingConfig::ID).expect("accepted reload");
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_owned()).collect();
        rig.wait("the strip follows the accepted edit", |rig| {
            workspace_names(rig).as_ref() == Some(&expected)
        });
        let now = rig.tiling_snapshot().revision;
        assert!(now > last, "accepted revisions advance: {last} -> {now}");
        last = now;
    }
    assert_strictly_increasing(&event_sequences(&rig));
    let _ = rig.registry.shutdown();
}

/// After host shutdown a late click and a late Tiling change are inert: the
/// click is refused (`shuttingDown` or `barNotRunning`) and no `BarChanged` follows.
#[test]
fn e2e_after_shutdown_a_late_click_and_a_late_change_are_inert() {
    let World { rig, kit, chrome, .. } = World::boot();
    rig.wait_loaded();
    rig.wait_strip("available", |view| strip(view).is_some());
    let rendered = strip(&rig.tiling_strip().expect("resolved")).expect("available");
    let request = BarTilingRequest {
        generation: rendered.generation.to_string(),
        sequence: rendered.sequence.to_string(),
        action: focus_window(chrome),
    };
    let label = rig.label();
    kit.factory.begin_shutdown();
    let _ = rig.registry.shutdown();
    assert!(rig.bridge.finish_shutdown(), "the watcher and worker joined");
    let events = rig.sink.events().len();
    assert!(
        matches!(
            rig.bridge.tiling_action(&label, &request),
            BarTilingOutcome::Unavailable {
                reason: BarTilingUnavailable::ShuttingDown | BarTilingUnavailable::BarNotRunning
            }
        ),
        "a late click is refused without reaching Tiling"
    );
    std::fs::write(
        rig.config.module_file(TilingConfig::ID),
        TILING.replace("\"coding\"", "\"code\""),
    )
    .expect("write");
    let _ = rig.store.reload_one(TilingConfig::ID);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        rig.sink.events().len(),
        events,
        "nothing is emitted after shutdown"
    );
}

/// Hold-to-Quit > `MenuAnywhere` > Keybindings at the host priorities over the
/// shared input service, with Tiling running passively (listen-only) and the
/// Bar running: Tiling consumes nothing and the strip does not move.
#[test]
#[allow(clippy::too_many_lines)] // One linear input scenario over the whole rig.
fn e2e_shared_input_precedence_holds_with_a_passive_tiling_and_the_bar() {
    use stache_sdk::input::testing::{FakeBackend, FakeClock};
    use stache_sdk::input::{
        CallbackScope, ControlAction, Disposition, HandlerSpec, InputEvent, InputEventKind,
        InputFilter, InputHandler, InputPoint, KindSet, ModifierFlags, MouseButton, PressStream,
        RawInput, TapKind, WorkRequirement,
    };

    struct Recorder {
        name: &'static str,
        order: Arc<Mutex<Vec<&'static str>>>,
        consume: std::sync::atomic::AtomicBool,
    }
    impl InputHandler for Recorder {
        fn on_event(&self, _: &InputEvent, _: &mut CallbackScope) -> Disposition {
            lock(&self.order).push(self.name);
            if self.consume.load(Ordering::SeqCst) {
                Disposition::Suppress
            } else {
                Disposition::Pass
            }
        }
    }

    let clock = FakeClock::new();
    let backend = FakeBackend::new(&clock);
    let service = stache_sdk::InputService::with_clock(
        Arc::clone(&backend) as Arc<dyn stache_sdk::input::InputBackend>,
        Arc::new(stache_sdk::input::NoWork),
        Arc::clone(&clock) as Arc<dyn stache_sdk::input::InputClock>,
    );
    let rig = Rig::with_input(Some(BAR), Arc::clone(&service));
    let World { rig, kit, .. } = World::over(rig, None);
    rig.wait_loaded();
    rig.wait_strip("available", |view| strip(view).is_some());
    assert!(backend.is_live(TapKind::ListenOnly), "Tiling observes passively");
    assert!(
        !backend.is_live(TapKind::Suppressing),
        "Tiling suppresses nothing"
    );
    let before = strip(&rig.tiling_strip().expect("resolved")).expect("available");

    let order = Arc::new(Mutex::new(Vec::new()));
    let filter =
        InputFilter::new(KindSet::of(InputEventKind::MouseDown).with(InputEventKind::MouseUp))
            .with_buttons([MouseButton(0)]);
    let handlers: Vec<Arc<Recorder>> = ["holdToQuit", "menuAnywhere", "keybindings"]
        .into_iter()
        .map(|name| {
            Arc::new(Recorder {
                name,
                order: Arc::clone(&order),
                consume: std::sync::atomic::AtomicBool::new(false),
            })
        })
        .collect();
    let cancel = stache_sdk::CancellationToken::new();
    // Registered lowest first, so the order comes from priority alone.
    let _subscriptions: Vec<_> = handlers
        .iter()
        .zip([
            crate::host::input_priority::HOLD_TO_QUIT,
            crate::host::input_priority::MENU_ANYWHERE,
            crate::host::input_priority::KEYBINDINGS,
        ])
        .rev()
        .map(|(handler, priority)| {
            service
                .subscribe_handler(
                    HandlerSpec {
                        filter: filter.clone(),
                        priority,
                        work: WorkRequirement::None,
                    },
                    Arc::clone(handler) as Arc<dyn InputHandler>,
                    &cancel,
                )
                .expect("handler")
        })
        .collect();
    let press = |kind| RawInput {
        kind,
        stream: PressStream::Button(MouseButton(0)),
        flags: ModifierFlags::NONE,
        repeat: false,
        timestamp: Duration::from_millis(1),
        location: Some(InputPoint { x: 120.0, y: 80.0 }),
    };
    let drags = || kit.snapshot().admission.drag_accepted;

    // Everyone passes: all three in priority order; Tiling's listen-only tap
    // still sees the press and passes it.
    let seen = drags();
    assert_eq!(
        service.dispatch(TapKind::Suppressing, press(InputEventKind::MouseDown)),
        Disposition::Pass
    );
    assert_eq!(
        service.dispatch(TapKind::ListenOnly, press(InputEventKind::MouseDown)),
        Disposition::Pass
    );
    assert_eq!(*lock(&order), ["holdToQuit", "menuAnywhere", "keybindings"]);
    for tap in [TapKind::Suppressing, TapKind::ListenOnly] {
        assert_eq!(
            service.dispatch(tap, press(InputEventKind::MouseUp)),
            Disposition::Pass
        );
    }
    wait_for("Tiling observed the press", || drags() > seen);

    // Each consumer in turn ends the chain; nobody below it is asked.
    for (index, expected) in [
        vec!["holdToQuit"],
        vec!["holdToQuit", "menuAnywhere"],
        vec!["holdToQuit", "menuAnywhere", "keybindings"],
    ]
    .into_iter()
    .enumerate()
    {
        for (at, handler) in handlers.iter().enumerate() {
            handler.consume.store(at == index, Ordering::SeqCst);
        }
        lock(&order).clear();
        assert_eq!(
            service.dispatch(TapKind::Suppressing, press(InputEventKind::MouseDown)),
            Disposition::Suppress
        );
        assert_eq!(*lock(&order), expected);
        assert_eq!(
            service.dispatch(TapKind::Suppressing, press(InputEventKind::MouseUp)),
            Disposition::Suppress,
            "the claimed press's up stays with its owner"
        );
        for kind in [InputEventKind::MouseDown, InputEventKind::MouseUp] {
            assert_eq!(
                service.dispatch(TapKind::ListenOnly, press(kind)),
                Disposition::Pass,
                "the listen-only tap never suppresses"
            );
        }
    }
    assert_eq!(
        backend.count(TapKind::ListenOnly, ControlAction::Install),
        1,
        "Tiling installed the passive tap once"
    );
    let after = strip(&rig.tiling_strip().expect("resolved")).expect("available");
    assert_eq!(
        after.workspaces, before.workspaces,
        "input never moves the strip"
    );
    assert_eq!(after.apps, before.apps);
    assert_eq!(rig.status(), ModuleStatus::Running);
    let _ = rig.registry.shutdown();
}

/// noTunes runs its own application observation next to Tiling's: a Music
/// launch is terminated by noTunes only, Tiling's strip and windows do not
/// move, and disabling noTunes releases only its own observation.
#[test]
fn e2e_notunes_and_tiling_keep_separate_application_observations() {
    let World { rig, kit, ghostty, .. } = World::boot();
    rig.wait_loaded();
    rig.wait_strip("available", |view| strip(view).is_some());
    std::fs::write(
        rig.config.module_file(stache_notunes::NoTunesConfig::ID),
        "enabled = true\n",
    )
    .expect("notunes.toml");
    let tiling_observers = rig.apps.observations();
    let notunes =
        crate::host::NoTunesFactory::new(ApplicationControl::with_acknowledgement_budget(
            Arc::clone(&rig.main) as Arc<dyn MainThreadExecutor>,
            Arc::new(rig.apps.clone()) as Arc<dyn ApplicationBackend>,
            BUDGET,
        ))
        .register(&rig.registry, &rig.store)
        .expect("register notunes");
    rig.start();
    wait_for("noTunes observes", || {
        rig.apps.observations() == tiling_observers + 1
    });
    let before = strip(&rig.tiling_strip().expect("resolved")).expect("available");
    let ghostty_frame = kit.frame(ghostty);

    let music = rig.apps.add_running(stache_notunes::MUSIC_BUNDLE_ID);
    assert_eq!(
        rig.apps.launch(music),
        tiling_observers + 1,
        "every observer is told"
    );
    wait_for("noTunes terminates Music", || {
        rig.apps.termination_requests().contains(&music)
    });
    let after = strip(&rig.tiling_strip().expect("resolved")).expect("available");
    assert_eq!(after.workspaces, before.workspaces);
    assert_eq!(after.apps, before.apps, "Music never joins the strip");
    assert_eq!(kit.frame(ghostty), ghostty_frame, "Tiling moved nothing");
    assert!(
        !rig.apps
            .termination_requests()
            .iter()
            .any(|key| kit.keys.values().any(|own| own == key)),
        "noTunes never touches Tiling's applications"
    );

    notunes.set_enabled(&rig.registry, false).expect("disable notunes");
    wait_for("only noTunes' observer is released", || {
        rig.apps.observations() == tiling_observers
    });
    assert_eq!(kit.status().state, stache_tiling::TilingRuntimeState::Running);
    assert_eq!(rig.status(), ModuleStatus::Running);
    let _ = rig.registry.shutdown();
}

/// Keep Awake disabled and re-enabled, then the Bar paused and resumed:
/// Tiling keeps running, and the strip and Keep Awake both recover with a
/// strictly increasing `BarChanged` sequence.
#[test]
fn e2e_sibling_disable_and_pause_recover_the_strip_and_keep_awake() {
    let rig = Rig::new(Some(BAR));
    let awake = rig.add_keep_awake(Some(KEEP_AWAKE_ON));
    awake.probe.set(NativeLockState::Unlocked);
    let World { rig, kit, .. } = World::over(rig, None);
    rig.wait_loaded();
    rig.wait_strip("available", |view| strip(view).is_some());
    rig.wait_keep_awake(indicator(true, false, true));
    let rendered = strip(&rig.tiling_strip().expect("resolved")).expect("available");

    awake.control.set_enabled(&rig.registry, false).expect("disable keepAwake");
    rig.wait_keep_awake(indicator(false, false, false));
    assert_eq!(kit.status().state, stache_tiling::TilingRuntimeState::Running);
    assert!(
        rig.tiling_strip().as_ref().and_then(strip).is_some(),
        "the strip stays"
    );
    awake.control.set_enabled(&rig.registry, true).expect("enable keepAwake");
    rig.wait_keep_awake(indicator(true, false, true));

    rig.control.pause(&rig.registry).expect("pause bar");
    wait_for("bar paused", || {
        rig.registry.snapshots().iter().any(|s| s.id == BarConfig::ID && s.paused)
    });
    assert_eq!(
        kit.status().state,
        stache_tiling::TilingRuntimeState::Running,
        "Tiling keeps running"
    );
    rig.control.resume(&rig.registry).expect("resume bar");
    rig.wait_loaded();
    rig.wait_strip("the strip recovers", |view| {
        strip(view).is_some_and(|strip| {
            strip.generation == rendered.generation && strip.workspaces == rendered.workspaces
        })
    });
    rig.wait_keep_awake(indicator(true, false, true));
    assert_strictly_increasing(&event_sequences(&rig));
    let _ = rig.registry.shutdown();
}

/// A shared native tab bar of `count` tabs (`AppKit` shows one tab window).
const fn tab_bar(id: u64, count: usize) -> TabBarRead {
    TabBarRead::Bar(TabBar {
        id: TabBarId::for_testing(id),
        count,
        selected: Some(0),
    })
}

/// Native tabs: two Finder windows share one tab bar and one slot; the strip
/// lists only the active tab and follows a tab switch.
#[test]
fn e2e_the_strip_lists_only_the_active_native_tab_and_follows_a_switch() {
    let World { rig, kit, zed, finder, .. } = World::boot();
    rig.wait_loaded();
    assert_eq!(rig.click(focus_workspace("coding")), BarTilingOutcome::Applied);
    rig.wait_strip("coding with two apps", |view| {
        strip(view).is_some_and(|strip| names(&strip.apps) == ["Zed", "Finder"])
    });
    kit.wait_frame(finder, RIGHT);
    kit.change(finder, WindowChange::Focused);

    // Command-T in Finder: the owner reports the old window unlisted but
    // alive, then the new tab window carrying the shared bar on the same slot.
    kit.change(finder, WindowChange::Suspended);
    let application = kit.application(FINDER);
    let tab = WindowInstance::for_testing(kit.windows.adapter(), kit.epoch(), application, 5);
    kit.tabbed(
        tab,
        "Documents",
        32,
        RIGHT,
        tab_bar(7, 2),
        SnapshotOrigin::Created,
    );
    kit.change(tab, WindowChange::Focused);
    rig.wait_strip("only the new active tab is listed", |view| {
        strip(view).is_some_and(|strip| {
            strip.apps
                == [
                    app(zed, "Zed", "main.rs — stache"),
                    app(tab, "Finder", "Documents"),
                ]
                && strip.focused_app == Some(app(tab, "Finder", "Documents"))
        })
    });
    let suspended = kit.snapshot();
    assert!(
        suspended
            .windows
            .iter()
            .any(|window| window.token == finder && window.is_inactive_tab()),
        "the first tab is tracked as an inactive tab"
    );

    // Switch back to the first tab: it is listed again with the shared bar,
    // the new tab leaves the list alive, and macOS reports the focus.
    kit.tabbed(
        finder,
        "Downloads",
        31,
        RIGHT,
        tab_bar(7, 2),
        SnapshotOrigin::Enumeration,
    );
    kit.change(tab, WindowChange::Suspended);
    kit.enumerated(FINDER, 1);
    kit.change(finder, WindowChange::Focused);
    rig.wait_strip("the strip follows the switch", |view| {
        strip(view).is_some_and(|strip| {
            strip.apps
                == [
                    app(zed, "Zed", "main.rs — stache"),
                    app(finder, "Finder", "Downloads"),
                ]
                && strip.focused_app == Some(app(finder, "Finder", "Downloads"))
        })
    });
    kit.wait_frame(finder, RIGHT);
    kit.wait_frame(zed, LEFT);
    assert_strictly_increasing(&event_sequences(&rig));
    let _ = rig.registry.shutdown();
}
