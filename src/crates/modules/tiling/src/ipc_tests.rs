//! Socket-method tests: queries, ordered mutations, lifecycle, deadlines and
//! shutdown, against a fake actor that applies the real pure transitions.
//!
//! Maps to TI-07-02 (typed outcomes, admission semantics, documented
//! corrections) and TI-07-03 (disabled/paused/quarantine/absence, malformed
//! data and deadline/shutdown races). The CLI-facing goldens belong to slice B.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use stache_macos::DisplayLayout;
use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead};
use stache_macos::windows::{SnapshotOrigin, WindowChange, WindowEventKind, WindowInstance};
use stache_sdk::ipc::{
    CoreHandler, CoreReload, CoreStatus, HandlerErrorKind, HandlerResult, IpcClient, IpcHandler,
    IpcServer, Request, Response, RetryPolicy, Router, WireErrorKind,
};
use stache_sdk::{AppEnvironment, AppPaths, CancellationToken, ModuleSnapshot, ModuleStatus};

use crate::actor::SnapshotCell;
use crate::config::TilingConfig;
use crate::dto::{OperationResponse, QueryKind, QueryParams};
use crate::events::IntentSubmitError;
use crate::intent::{
    FocusDirection, FocusTarget, Intent, ResizeDimension, ScreenTarget, SwapDirection,
};
use crate::ipc::{
    ActorPort, IpcTiming, LifecycleControl, LifecycleFailure, SubmitFailure, TilingIpc, body,
};
use crate::state::{Input, TilingSnapshot, TilingState, transition};
use crate::status::TilingStatusSource;
use crate::test_support::{Feed, display, info, ready, snapshot, step};

// ------------------------------------------------------------------ world

const fn frame_read(x: f64, y: f64, w: f64, h: f64) -> FrameRead {
    FrameRead {
        position: AxReading::Value(AxPoint { x, y }),
        size: AxReading::Value(AxSize { width: w, height: h }),
    }
}

pub struct Built {
    pub state: TilingState,
    pub feed: Feed,
}

impl Built {
    pub fn new(config: Value) -> Self {
        let mut feed = Feed::new(1);
        let apps = [
            info(1, "com.editor", "Editor"),
            info(2, "com.browser", "Browser"),
            info(3, "com.apple.dock", "Dock"),
        ];
        let state = ready("com.stache.app", &apps, &mut feed);
        let mut built = Self { state, feed };
        built.send(Input::Displays(Arc::new(DisplayLayout::new(vec![
            display(1, true, Some(60.0), 0.0),
            display(2, false, Some(60.0), 1920.0),
        ]))));
        built.send(Input::Config {
            revision: 1,
            config: Arc::new(serde_json::from_value(config).expect("config")),
        });
        built.send(Input::FrameWrites(true));
        built
    }

    pub fn send(&mut self, input: Input) {
        let state = std::mem::replace(&mut self.state, TilingState::new(""));
        self.state = step(state, input).0;
    }

    pub fn add(&mut self, app: u64, serial: u64, title: &str, x: f64, cg: u32) -> WindowInstance {
        let token = self.feed.token(app, serial);
        let mut snap = snapshot(token, title, cg);
        snap.frame = frame_read(x + 10.0, 10.0, 800.0, 600.0);
        let input = self.feed.input(WindowEventKind::Snapshot {
            snapshot: snap,
            origin: SnapshotOrigin::Created,
        });
        self.send(input);
        token
    }

    pub fn focus(&mut self, token: WindowInstance) {
        let input = self.feed.change(token, WindowChange::Focused);
        self.send(input);
    }
}

pub fn config() -> Value {
    json!({"workspaces": [
        {"name": "a", "screen": "main"},
        {"name": "b", "screen": "main"},
        {"name": "g", "screen": "secondary", "layout": "grid"},
        {"name": "f", "screen": "secondary", "layout": "floating"},
    ]})
}

/// Editor window `101` on `a` (focused), browser window `201` on `g`.
pub fn standard() -> (Built, WindowInstance, WindowInstance) {
    let mut built = Built::new(config());
    let editor = built.add(1, 1, "Editor — main.rs", 0.0, 101);
    let browser = built.add(2, 1, "Browser \"quoted\" 日本語\n🙂", 1920.0, 201);
    built.focus(editor);
    (built, editor, browser)
}

/// The standard world with `b` inactive: the browser window moves to the
/// hidden `f` (the secondary display shows nothing), Finder is not running and
/// every application has a window, so nothing can host an empty desktop.
pub fn inactive_b() -> Built {
    let (mut built, _, browser) = standard();
    built.send(Input::Intent(Intent::SendWindowToWorkspace {
        window: Some(browser),
        workspace: "f".to_owned(),
    }));
    assert!(
        built
            .state
            .snapshot()
            .unreachable
            .contains(&built.state.workspaces.iter().find(|ws| ws.name == "b").expect("b").id)
    );
    built
}

// ------------------------------------------------------------- fake actor

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Apply each intent as soon as it is offered.
    Process,
    /// Apply each intent after a delay, on a worker thread.
    Delay(Duration),
    /// Accept intents and never record them.
    Swallow,
    /// Refuse every intent as saturated.
    Full,
    /// Refuse every intent as closed.
    Closed,
}

type AfterHook = Box<dyn Fn(&mut TilingState) + Send + Sync>;

pub struct FakeActor {
    state: Mutex<TilingState>,
    cell: Mutex<Arc<TilingSnapshot>>,
    mode: Mutex<Mode>,
    submitted: Mutex<Vec<Intent>>,
    after: Mutex<Option<AfterHook>>,
    alive: Mutex<bool>,
}

impl FakeActor {
    fn new(state: TilingState) -> Arc<Self> {
        let cell = Arc::new(state.snapshot());
        Arc::new(Self {
            state: Mutex::new(state),
            cell: Mutex::new(cell),
            mode: Mutex::new(Mode::Process),
            submitted: Mutex::new(Vec::new()),
            after: Mutex::new(None),
            alive: Mutex::new(true),
        })
    }

    pub fn set_mode(&self, mode: Mode) {
        *self.mode.lock().unwrap_or_else(PoisonError::into_inner) = mode;
    }

    pub fn submitted(&self) -> Vec<Intent> {
        self.submitted.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn apply(&self, intent: Intent) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let state = std::mem::replace(&mut *guard, TilingState::new(""));
        let (mut next, _) = transition(state, Input::Intent(intent));
        if let Some(hook) = &*self.after.lock().unwrap_or_else(PoisonError::into_inner) {
            hook(&mut next);
        }
        *self.cell.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(next.snapshot());
        *guard = next;
    }

    /// Replaces the published state without an intent (a world change).
    pub fn publish(&self, edit: impl FnOnce(&mut TilingState)) {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        edit(&mut guard);
        *self.cell.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(guard.snapshot());
    }

    pub fn kill(&self) { *self.alive.lock().unwrap_or_else(PoisonError::into_inner) = false; }
}

impl ActorPort for Arc<FakeActor> {
    fn snapshot(&self) -> Option<Arc<TilingSnapshot>> {
        if !*self.alive.lock().unwrap_or_else(PoisonError::into_inner) {
            return None;
        }
        Some(Arc::clone(
            &self.cell.lock().unwrap_or_else(PoisonError::into_inner),
        ))
    }

    fn submit(&self, intent: Intent) -> Result<(), SubmitFailure> {
        if !*self.alive.lock().unwrap_or_else(PoisonError::into_inner) {
            return Err(SubmitFailure::NotRunning);
        }
        let mode = *self.mode.lock().unwrap_or_else(PoisonError::into_inner);
        match mode {
            Mode::Full => return Err(SubmitFailure::Full),
            Mode::Closed => return Err(SubmitFailure::Closed),
            Mode::Process | Mode::Delay(_) | Mode::Swallow => {}
        }
        self.submitted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(intent.clone());
        match mode {
            Mode::Process => self.apply(intent),
            Mode::Delay(delay) => {
                let actor = Self::clone(self);
                std::thread::spawn(move || {
                    std::thread::sleep(delay);
                    actor.apply(intent);
                });
            }
            Mode::Swallow | Mode::Full | Mode::Closed => {}
        }
        Ok(())
    }
}

// ---------------------------------------------------------- fake lifecycle

type LifecycleHook = Arc<dyn Fn() -> Result<(), LifecycleFailure> + Send + Sync>;

pub struct FakeLifecycle {
    module: Mutex<Option<ModuleSnapshot>>,
    pause: Mutex<LifecycleHook>,
    resume: Mutex<LifecycleHook>,
    pub pauses: AtomicUsize,
    pub resumes: AtomicUsize,
}

pub fn module(status: ModuleStatus, enabled: bool, paused: bool) -> ModuleSnapshot {
    ModuleSnapshot {
        id: TilingConfig::ID,
        status,
        enabled,
        paused,
        revision: 1,
        diagnostic: None,
    }
}

impl FakeLifecycle {
    fn new() -> Arc<Self> {
        let ok: LifecycleHook = Arc::new(|| Ok(()));
        Arc::new(Self {
            module: Mutex::new(Some(module(ModuleStatus::Running, true, false))),
            pause: Mutex::new(Arc::clone(&ok)),
            resume: Mutex::new(ok),
            pauses: AtomicUsize::new(0),
            resumes: AtomicUsize::new(0),
        })
    }

    pub fn set_module(&self, value: Option<ModuleSnapshot>) {
        *self.module.lock().unwrap_or_else(PoisonError::into_inner) = value;
    }

    pub fn on_pause(
        &self,
        hook: impl Fn() -> Result<(), LifecycleFailure> + Send + Sync + 'static,
    ) {
        *self.pause.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(hook);
    }

    pub fn on_resume(
        &self,
        hook: impl Fn() -> Result<(), LifecycleFailure> + Send + Sync + 'static,
    ) {
        *self.resume.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(hook);
    }
}

impl LifecycleControl for Arc<FakeLifecycle> {
    fn module(&self) -> Option<ModuleSnapshot> {
        self.module.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn pause(&self) -> Result<(), LifecycleFailure> {
        self.pauses.fetch_add(1, Ordering::SeqCst);
        let hook = Arc::clone(&self.pause.lock().unwrap_or_else(PoisonError::into_inner));
        hook()
    }

    fn resume(&self) -> Result<(), LifecycleFailure> {
        self.resumes.fetch_add(1, Ordering::SeqCst);
        let hook = Arc::clone(&self.resume.lock().unwrap_or_else(PoisonError::into_inner));
        hook()
    }
}

// --------------------------------------------------------------------- rig

pub fn fast() -> IpcTiming {
    IpcTiming {
        budget: Duration::from_secs(3),
        hop: Duration::from_secs(1),
        poll: Duration::from_millis(1),
    }
}

pub struct Rig {
    pub actor: Arc<FakeActor>,
    pub life: Arc<FakeLifecycle>,
    pub status: TilingStatusSource,
    pub cancel: CancellationToken,
    pub ipc: Arc<TilingIpc>,
}

impl Rig {
    pub fn new(state: TilingState) -> Self { Self::timed(state, fast()) }

    pub fn timed(state: TilingState, timing: IpcTiming) -> Self {
        let actor = FakeActor::new(state);
        let life = FakeLifecycle::new();
        let status = TilingStatusSource::default();
        status.running(Arc::new(SnapshotCell::default()));
        let cancel = CancellationToken::new();
        let ipc = Arc::new(TilingIpc::with_timing(
            Arc::new(Arc::clone(&actor)),
            Arc::new(Arc::clone(&life)),
            status.clone(),
            cancel.clone(),
            timing,
        ));
        Self {
            actor,
            life,
            status,
            cancel,
            ipc,
        }
    }

    pub fn standard() -> (Self, WindowInstance, WindowInstance) {
        let (built, editor, browser) = standard();
        (Self::new(built.state), editor, browser)
    }

    fn call(&self, method: &str, params: Value) -> HandlerResult<Value> {
        self.ipc.handle(method, params)
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        self.call(method, params).unwrap_or_else(|error| panic!("{method}: {error}"))
    }

    /// The kind and body of a failed call.
    fn err(&self, method: &str, params: Value) -> (HandlerErrorKind, String) {
        let error = self.call(method, params).expect_err(&format!("{method} should fail"));
        (error.kind(), error.message().to_owned())
    }

    pub fn query(&self, params: Value) -> Value { self.ok("query", params) }
}

fn names(value: &Value, key: &str) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|row| row[key].as_str().expect("string").to_owned())
        .collect()
}

fn operation(value: Value) -> OperationResponse {
    serde_json::from_value(value).expect("operation response")
}

// ====================================================== wire shapes (queries)

#[test]
fn screens_use_the_legacy_field_set_in_actor_order() {
    let (rig, ..) = Rig::standard();
    let screens = rig.query(json!({"kind": "screens"}));
    assert_eq!(
        screens,
        json!([
            {
                "id": 1, "name": "Display 1", "isMain": true, "isBuiltin": true,
                "scaleFactor": 2.0,
                "frame": {"x": 0.0, "y": 0.0, "width": 1920.0, "height": 1080.0},
                "visibleFrame": {"x": 0.0, "y": 0.0, "width": 1920.0, "height": 1055.0},
            },
            {
                "id": 2, "name": "Display 2", "isMain": false, "isBuiltin": false,
                "scaleFactor": 2.0,
                "frame": {"x": 1920.0, "y": 0.0, "width": 1920.0, "height": 1080.0},
                "visibleFrame": {"x": 1920.0, "y": 0.0, "width": 1920.0, "height": 1055.0},
            },
        ])
    );
}

#[test]
fn workspaces_use_a_string_id_and_the_legacy_field_set() {
    let (rig, ..) = Rig::standard();
    let all = rig.query(json!({"kind": "workspaces"}));
    assert_eq!(
        names(&all, "name"),
        ["a", "b", "g", "f"],
        "actor order, no name sort"
    );
    let first = &all[0];
    let mut keys: Vec<&str> =
        first.as_object().expect("object").keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, [
        "id",
        "isFocused",
        "isVisible",
        "layout",
        "name",
        "screenName",
        "windowCount"
    ]);
    assert!(first["id"].is_string());
    assert_eq!(first["screenName"], "Display 1");
    assert_eq!(first["layout"], "dwindle");
    assert_eq!(first["windowCount"], 1);
    assert_eq!(first["isFocused"], true);
    assert_eq!(all[2]["layout"], "grid");
}

#[test]
fn workspace_screen_filters_are_exact_and_secondary_means_every_non_main() {
    let (rig, ..) = Rig::standard();
    for alias in ["main", "primary"] {
        let rows = rig.query(json!({"kind": "workspaces", "screen": alias}));
        assert_eq!(names(&rows, "name"), ["a", "b"], "{alias}");
    }
    let secondary = rig.query(json!({"kind": "workspaces", "screen": "secondary"}));
    assert_eq!(names(&secondary, "name"), ["g", "f"]);
    let named = rig.query(json!({"kind": "workspaces", "screen": "Display 2"}));
    assert_eq!(names(&named, "name"), ["g", "f"]);
    // Aliases and names are exact: case and substrings do not match.
    for miss in ["Main", "SECONDARY", "display 2", "Display", "nope"] {
        let rows = rig.query(json!({"kind": "workspaces", "screen": miss}));
        assert_eq!(rows, json!([]), "{miss}");
    }
}

#[test]
fn focused_screen_selects_the_focused_workspaces_screen_and_is_empty_without_focus() {
    let (rig, ..) = Rig::standard();
    let rows = rig.query(json!({"kind": "workspaces", "focusedScreen": true}));
    assert_eq!(names(&rows, "name"), ["a", "b"]);

    rig.actor.publish(|state| {
        state.focus = crate::state::FocusState::default();
        for workspace in &mut state.workspaces {
            workspace.focused = false;
        }
    });
    assert_eq!(
        rig.query(json!({"kind": "workspaces", "focusedScreen": true})),
        json!([])
    );
    assert_eq!(
        rig.query(json!({"kind": "windows", "focusedScreen": true})),
        json!([])
    );
    assert_eq!(
        rig.query(json!({"kind": "windows", "focusedWorkspace": true})),
        json!([])
    );
    // Unfiltered queries still list everything.
    assert_eq!(
        rig.query(json!({"kind": "windows"})).as_array().expect("array").len(),
        2
    );
}

#[test]
fn windows_carry_the_legacy_fields_and_escape_text_through_serde() {
    let (rig, ..) = Rig::standard();
    let windows = rig.query(json!({"kind": "windows"}));
    assert_eq!(
        windows[0],
        json!({
            "id": 101, "pid": 101, "appId": "com.editor", "appName": "Editor",
            "title": "Editor — main.rs", "workspace": "a",
            "frame": {"x": 10.0, "y": 10.0, "width": 800.0, "height": 600.0},
            "isMinimized": false, "isFullscreen": false, "isFloating": false, "isFocused": true,
        })
    );
    assert_eq!(windows[1]["title"], "Browser \"quoted\" 日本語\n🙂");
    assert_eq!(windows[1]["workspace"], "g");
    assert_eq!(windows[1]["isFocused"], false);
    // The serialized line keeps quotes and control characters escaped.
    let line = serde_json::to_string(&windows).expect("encode");
    assert!(line.contains(r#"\"quoted\""#) && line.contains(r"\n"), "{line}");
}

#[test]
fn window_filters_combine_and_stay_exact() {
    let (rig, ..) = Rig::standard();
    let q = |extra: Value| {
        let mut params = json!({"kind": "windows"});
        params
            .as_object_mut()
            .expect("object")
            .extend(extra.as_object().expect("object").clone());
        names(&rig.query(params), "workspace")
    };
    assert_eq!(q(json!({"screen": "main"})), ["a"]);
    assert_eq!(q(json!({"screen": "secondary"})), ["g"]);
    assert_eq!(q(json!({"workspace": "g"})), ["g"]);
    assert_eq!(
        q(json!({"screen": "main", "workspace": "g"})),
        Vec::<String>::new()
    );
    assert_eq!(q(json!({"screen": "Display 2", "workspace": "g"})), ["g"]);
    assert_eq!(q(json!({"focusedWorkspace": true})), ["a"]);
    assert_eq!(q(json!({"focusedScreen": true})), ["a"]);
    assert_eq!(q(json!({"workspace": "G"})), Vec::<String>::new());
}

#[test]
fn grid_overflow_reports_effective_floating_beyond_the_ten_tile_cap() {
    let mut built = Built::new(config());
    for serial in 0..12_u64 {
        built.add(
            2,
            serial + 1,
            &format!("tab {serial}"),
            1920.0,
            300 + u32::try_from(serial).unwrap_or(0),
        );
    }
    let rig = Rig::new(built.state);
    let rows = rig.query(json!({"kind": "windows", "workspace": "g"}));
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 12);
    let floating: Vec<bool> =
        rows.iter().map(|row| row["isFloating"].as_bool().expect("bool")).collect();
    assert_eq!(floating.iter().filter(|f| **f).count(), 2, "{floating:?}");
    assert!(floating[..10].iter().all(|f| !f) && floating[10..].iter().all(|f| *f));
}

#[test]
fn apps_come_from_the_catalog_and_skip_system_components() {
    let (rig, ..) = Rig::standard();
    let apps = rig.query(json!({"kind": "apps"}));
    assert_eq!(
        apps,
        json!([
            {"pid": 101, "name": "Editor", "bundleId": "com.editor", "isHidden": false},
            {"pid": 102, "name": "Browser", "bundleId": "com.browser", "isHidden": false},
        ]),
        "the Dock is a system component and is skipped"
    );
}

#[test]
fn the_status_query_answers_the_phase_and_takes_no_filters() {
    let (rig, ..) = Rig::standard();
    let status = rig.query(json!({"kind": "status"}));
    assert!(status["phase"].is_string(), "{status}");
    assert!(
        status["failedApps"].is_array() && status["noFrame"].is_array(),
        "{status}"
    );
    assert!(status["trackedWindows"].is_array() && status["excludedWindows"].is_array());
    let (kind, _) = rig.err("query", json!({"kind": "status", "screen": "main"}));
    assert_eq!(kind, HandlerErrorKind::InvalidParams);
}

#[test]
fn empty_state_answers_empty_arrays_not_errors() {
    let rig = Rig::new(Built::new(config()).state);
    assert_eq!(rig.query(json!({"kind": "windows"})), json!([]));
    assert_eq!(
        rig.query(json!({"kind": "windows", "screen": "secondary"})),
        json!([])
    );
    assert_eq!(
        rig.query(json!({"kind": "workspaces", "screen": "nowhere"})),
        json!([])
    );

    // No attached display is an unconfirmed topology, not a fabricated one.
    let mut state = Built::new(config()).state;
    state.layout = DisplayLayout::default();
    state.screens.clear();
    let rig = Rig::new(state);
    assert_eq!(rig.query(json!({"kind": "screens"})), json!([]));
}

// ------------------------------------------------------ query param guards

#[test]
fn malformed_unknown_and_conflicting_query_params_are_invalid_params() {
    let (rig, ..) = Rig::standard();
    let invalid = |params: Value| {
        let (kind, _) = rig.err("query", params);
        assert_eq!(kind, HandlerErrorKind::InvalidParams);
    };
    invalid(Value::Null);
    invalid(json!({}));
    invalid(json!({"kind": "nope"}));
    invalid(json!({"kind": "screens", "extra": 1}));
    invalid(json!({"kind": "screens", "detailed": true}));
    invalid(json!({"kind": "screens", "screen": "main"}));
    invalid(json!({"kind": "apps", "workspace": "a"}));
    invalid(json!({"kind": "workspaces", "workspace": "a"}));
    invalid(json!({"kind": "workspaces", "focusedScreen": true, "screen": "main"}));
    invalid(json!({"kind": "windows", "focusedScreen": true, "screen": "main"}));
    invalid(json!({"kind": "windows", "focusedScreen": true, "workspace": "a"}));
    invalid(json!({"kind": "windows", "focusedScreen": true, "focusedWorkspace": true}));
    invalid(json!({"kind": "windows", "focusedWorkspace": true, "screen": "main"}));
    invalid(json!({"kind": "windows", "focusedWorkspace": true, "workspace": "a"}));
    invalid(json!({"kind": "windows", "focusedScreen": "yes"}));
    let (kind, _) = rig.err("nope", json!({}));
    assert_eq!(kind, HandlerErrorKind::UnknownMethod);
}

// ------------------------------------------------------- unavailable bodies

#[test]
fn lifecycle_states_answer_typed_errors_for_queries_and_mutations() {
    let (rig, ..) = Rig::standard();
    let requests = [
        ("query", json!({"kind": "screens"})),
        ("window", json!({"toggleFloating": true})),
        ("workspace", json!({"balance": true})),
    ];
    let cases: [(Option<ModuleSnapshot>, &str, HandlerErrorKind); 4] = [
        (None, body::NOT_INITIALIZED, HandlerErrorKind::Unavailable),
        (
            Some(module(ModuleStatus::Disabled, false, false)),
            body::DISABLED,
            HandlerErrorKind::Disabled,
        ),
        (
            Some(module(ModuleStatus::Paused, true, true)),
            body::PAUSED,
            HandlerErrorKind::Unavailable,
        ),
        (
            Some(module(ModuleStatus::Stopped, true, false)),
            body::NOT_INITIALIZED,
            HandlerErrorKind::Unavailable,
        ),
    ];
    rig.status.inactive(None);
    for (snapshot, expected, kind) in cases {
        rig.life.set_module(snapshot);
        for (method, params) in &requests {
            let (got_kind, message) = rig.err(method, params.clone());
            assert_eq!((got_kind, message.as_str()), (kind, expected), "{method}");
        }
    }
    assert!(
        rig.actor.submitted().is_empty(),
        "nothing is sent while unavailable"
    );

    // Quarantined cleanup.
    rig.life.set_module(Some(module(ModuleStatus::Running, true, false)));
    rig.status.quarantined(vec!["actor".to_owned()], "stuck".to_owned());
    for (method, params) in &requests {
        let (kind, message) = rig.err(method, params.clone());
        assert_eq!(
            (kind, message.as_str()),
            (HandlerErrorKind::Unavailable, body::CLEANUP_PENDING)
        );
    }
}

#[test]
fn an_unpublished_runtime_an_unhealthy_actor_and_initial_enumeration_never_answer_empty() {
    let (rig, ..) = Rig::standard();
    rig.actor.kill();
    assert_eq!(
        rig.err("query", json!({"kind": "windows"})),
        (HandlerErrorKind::Unavailable, body::NOT_INITIALIZED.to_owned())
    );

    let (rig, ..) = Rig::standard();
    rig.actor.publish(|state| {
        state.health = crate::state::Health::Failed(crate::state::HealthFault::Disconnected);
    });
    assert_eq!(
        rig.err("query", json!({"kind": "windows"})),
        (
            HandlerErrorKind::Unavailable,
            body::HANDLE_UNAVAILABLE.to_owned()
        )
    );

    let rig = Rig::new(TilingState::new("com.stache.app"));
    assert_eq!(
        rig.err("query", json!({"kind": "windows"})),
        (HandlerErrorKind::Unavailable, body::NOT_INITIALIZED.to_owned()),
        "before the first enumeration there is no confirmed state"
    );
}

#[test]
fn availability_prefers_disabled_then_paused_then_cleanup() {
    use crate::ipc::{Availability, availability};
    use crate::status::TilingRuntimeState as R;
    let running = module(ModuleStatus::Running, true, false);
    assert_eq!(availability(None, R::Inactive), Availability::NotInitialized);
    assert_eq!(availability(Some(&running), R::Running), Availability::Running);
    assert_eq!(
        availability(Some(&running), R::Inactive),
        Availability::NotRunning
    );
    assert_eq!(
        availability(Some(&running), R::Stopping),
        Availability::CleanupPending
    );
    assert_eq!(
        availability(Some(&running), R::Quarantined),
        Availability::CleanupPending
    );
    let paused = module(ModuleStatus::Paused, true, true);
    assert_eq!(availability(Some(&paused), R::Inactive), Availability::Paused);
    assert_eq!(
        availability(Some(&paused), R::Quarantined),
        Availability::CleanupPending
    );
    let disabled = module(ModuleStatus::Disabled, false, true);
    assert_eq!(
        availability(Some(&disabled), R::Quarantined),
        Availability::Disabled
    );
}

// ------------------------------------------------------------ DTO decoding

#[test]
fn mutation_requests_deny_unknown_fields_and_bad_shapes() {
    let (rig, ..) = Rig::standard();
    for (method, params) in [
        ("window", json!({"focus": "next", "bogus": true})),
        (
            "window",
            json!({"resize": [{"dimension": "width", "amount": 5}]}),
        ),
        ("window", json!({"toggleFloating": "yes"})),
        ("window", Value::Null),
        ("workspace", json!({"layout": "spiral"})),
        ("workspace", json!({"layout": "Grid"})),
        // The socket takes the canonical snake_case spelling only; the kebab
        // spellings are CLI-only aliases.
        ("workspace", json!({"layout": "split-vertical"})),
        ("workspace", json!({"layout": "splitVertical"})),
        ("workspace", json!({"balance": 1})),
        ("pause", json!({"force": true})),
        ("resume", json!([])),
    ] {
        let (kind, _) = rig.err(method, params.clone());
        assert_eq!(kind, HandlerErrorKind::InvalidParams, "{method} {params}");
    }
    assert!(rig.actor.submitted().is_empty());
}

#[test]
fn request_and_response_dtos_round_trip_and_reject_unknown_fields() {
    let params = QueryParams::new(QueryKind::Windows);
    let value = serde_json::to_value(&params).expect("encode");
    assert_eq!(value, json!({"kind": "windows"}));
    assert_eq!(
        serde_json::from_value::<QueryParams>(value).expect("decode"),
        params
    );

    let (rig, ..) = Rig::standard();
    let mut response = rig.ok("window", json!({"toggleFloating": true}));
    let decoded: OperationResponse = serde_json::from_value(response.clone()).expect("decode");
    assert!(matches!(decoded, OperationResponse::Enqueued(_)));
    response["surprise"] = json!(1);
    assert!(serde_json::from_value::<OperationResponse>(response).is_err());
}

// ------------------------------------------------------ mutations: success

#[test]
fn an_admitted_action_answers_enqueued_never_a_success_flag() {
    let (rig, ..) = Rig::standard();
    let value = rig.ok("window", json!({"toggleFloating": true}));
    assert_eq!(
        value,
        json!({
            "outcome": "enqueued",
            "request": {
                "operation": "window",
                "actions": [{"action": "toggleFloating", "status": "admittedChanged"}],
            },
            "targets": [], "failed": [], "notAttempted": [], "unknown": [],
        })
    );
    let text = value.to_string();
    assert!(!text.contains("success") && !text.contains("applied"), "{text}");
}

#[test]
fn window_actions_run_in_the_fixed_order_whatever_the_request_order() {
    let (rig, editor, _) = Rig::standard();
    // The JSON object lists the fields in reverse of the fixed order.
    let params: Value = serde_json::from_str(
        r#"{"sendToWorkspace":"b","sendToScreen":"secondary",
            "resize":[{"dimension":"Height","amount":"-4"},{"dimension":"width","amount":"7"}],
            "preset":"center","toggleFloating":true,"swap":"next","focus":"next"}"#,
    )
    .expect("json");
    // Sending the window to the other screen leaves no focused window, so the
    // last action reports it; every earlier action was admitted in order.
    let result = rig.call("window", params);
    assert_eq!(
        result.expect_err("no focus after the screen move").message(),
        body::NO_FOCUSED_WINDOW
    );
    assert_eq!(rig.actor.submitted(), vec![
        Intent::FocusWindow(FocusTarget::Direction(FocusDirection::Next)),
        Intent::SwapWindow(SwapDirection::Next),
        Intent::ToggleFloating,
        Intent::Preset("center".to_owned()),
        Intent::Resize {
            dimension: ResizeDimension::Height,
            amount: -4
        },
        Intent::Resize {
            dimension: ResizeDimension::Width,
            amount: 7
        },
        Intent::SendWindowToScreen(ScreenTarget::Secondary),
        Intent::SendWindowToWorkspace {
            window: None,
            workspace: "b".to_owned()
        },
    ]);

    let (rig, ..) = Rig::standard();
    let value = rig.ok(
        "window",
        json!({"preset": "center", "toggleFloating": true, "swap": "next", "focus": "next",
               "resize": [{"dimension": "height", "amount": "1"}, {"dimension": "width", "amount": "2"}]}),
    );
    let actions: Vec<_> = operation(value)
        .evidence()
        .request
        .actions
        .iter()
        .map(|action| action.action.clone())
        .collect();
    assert_eq!(actions, [
        "focus",
        "swap",
        "toggleFloating",
        "preset",
        "resize",
        "resize"
    ]);
    let _ = editor;
}

#[test]
fn workspace_actions_run_focus_layout_balance_send_in_order() {
    let (rig, ..) = Rig::standard();
    rig.ok(
        "workspace",
        json!({"sendToScreen": "main", "balance": true, "layout": "grid", "focus": "b"}),
    );
    assert_eq!(rig.actor.submitted(), vec![
        Intent::FocusWorkspace("b".to_owned()),
        Intent::SetLayout(crate::config::LayoutType::Grid),
        Intent::Balance,
        Intent::SendWorkspaceToScreen(ScreenTarget::Main),
    ]);
}

#[test]
fn split_layouts_use_snake_case_on_the_socket_and_in_query_results() {
    let (rig, ..) = Rig::standard();
    rig.ok("workspace", json!({"layout": "split_horizontal"}));
    assert_eq!(rig.actor.submitted(), vec![Intent::SetLayout(
        crate::config::LayoutType::SplitHorizontal
    )]);
    assert_eq!(
        serde_json::to_value(crate::config::LayoutType::SplitVertical).expect("encode"),
        json!("split_vertical")
    );
    assert_eq!(
        crate::config::LayoutType::SplitVertical.as_str(),
        "split_vertical"
    );
}

#[test]
fn each_action_waits_for_its_record_before_the_next_selector_resolves() {
    let (rig, ..) = Rig::standard();
    rig.actor.set_mode(Mode::Delay(Duration::from_millis(30)));
    // Focusing workspace "b" changes which workspace is focused; the layout
    // that follows must see that, so the second intent is only offered after
    // the first was recorded.
    let started = Instant::now();
    rig.ok("workspace", json!({"focus": "b", "layout": "monocle"}));
    assert!(
        started.elapsed() >= Duration::from_millis(60),
        "two sequential waits"
    );
    let after = rig.query(json!({"kind": "workspaces"}));
    let b = &after.as_array().expect("array")[1];
    assert_eq!(
        (
            b["name"].as_str(),
            b["layout"].as_str(),
            b["isFocused"].as_bool()
        ),
        (Some("b"), Some("monocle"), Some(true))
    );
}

#[test]
fn directions_are_case_insensitive_and_swap_is_exact() {
    let (rig, ..) = Rig::standard();
    for (target, expected) in [
        ("UP", FocusDirection::Up),
        ("Prev", FocusDirection::Previous),
        ("previous", FocusDirection::Previous),
        ("NEXT", FocusDirection::Next),
    ] {
        rig.ok("window", json!({"focus": target}));
        assert_eq!(
            rig.actor.submitted().last(),
            Some(&Intent::FocusWindow(FocusTarget::Direction(expected))),
            "{target}"
        );
    }
    let before = rig.actor.submitted().len();
    for bad in ["Up", "prev", "NEXT", "", "diagonal"] {
        assert_eq!(
            rig.err("window", json!({"swap": bad})),
            (HandlerErrorKind::Handler, body::invalid_swap(bad)),
            "{bad:?}"
        );
    }
    assert_eq!(
        rig.err("window", json!({"focus": "sideways"})),
        (
            HandlerErrorKind::Handler,
            "Invalid focus direction: sideways".to_owned()
        )
    );
    assert_eq!(
        rig.actor.submitted().len(),
        before,
        "invalid values are never sent"
    );
    for good in ["up", "down", "left", "right", "next", "previous"] {
        rig.ok("window", json!({"swap": good}));
    }
}

// --------------------------------------------------- numeric focus selector

#[test]
fn a_numeric_focus_target_resolves_once_to_the_exact_window_token() {
    let (rig, _, browser) = Rig::standard();
    // After the focus intent is processed the id is no longer the browser's.
    rig.actor.after.lock().unwrap_or_else(PoisonError::into_inner).replace(Box::new(
        move |state| {
            if let Some(window) = state.windows.get_mut(&browser) {
                window.cg_window_id = Some(999);
            }
        },
    ));
    rig.ok("window", json!({"focus": "201", "sendToWorkspace": "a"}));
    let submitted = rig.actor.submitted();
    assert_eq!(
        submitted[0],
        Intent::FocusWindow(FocusTarget::Window(browser)),
        "the id resolved to the exact token once"
    );
    // The follow-up acts on the focused window logically (no id retarget).
    assert_eq!(submitted[1], Intent::SendWindowToWorkspace {
        window: None,
        workspace: "a".to_owned()
    });
    // The reused id no longer resolves; it is never silently retargeted.
    assert_eq!(
        rig.err("window", json!({"focus": "201"})),
        (HandlerErrorKind::Handler, body::window_not_found(201))
    );
    assert_eq!(rig.actor.submitted().len(), 2);
}

#[test]
fn unknown_and_ambiguous_numeric_targets_are_window_not_found() {
    let (rig, ..) = Rig::standard();
    assert_eq!(
        rig.err("window", json!({"focus": "424242"})),
        (HandlerErrorKind::Handler, "Window 424242 not found".to_owned())
    );
    rig.actor.publish(|state| {
        for window in state.windows.values_mut() {
            window.cg_window_id = Some(7);
        }
    });
    assert_eq!(
        rig.err("window", json!({"focus": "7"})),
        (HandlerErrorKind::Handler, "Window 7 not found".to_owned()),
        "two claimants are not a guess"
    );
    assert!(rig.actor.submitted().is_empty());
    // A signed or oversized number is not a window id; it is an invalid direction.
    assert_eq!(
        rig.err("window", json!({"focus": "-3"})),
        (HandlerErrorKind::Handler, body::invalid_focus("-3"))
    );
    assert_eq!(
        rig.err("window", json!({"focus": "99999999999999999999"})),
        (
            HandlerErrorKind::Handler,
            body::invalid_focus("99999999999999999999")
        )
    );
}

// ---------------------------------------------------- inactive workspaces

#[test]
fn focusing_an_inactive_workspace_is_the_workspace_is_inactive_error() {
    let rig = Rig::new(inactive_b().state);
    assert_eq!(
        rig.err("workspace", json!({"focus": "b"})),
        (HandlerErrorKind::Handler, body::WORKSPACE_INACTIVE.to_owned())
    );
    assert_eq!(body::WORKSPACE_INACTIVE, "Workspace is inactive");
    assert_eq!(rig.actor.submitted(), vec![Intent::FocusWorkspace(
        "b".to_owned()
    )]);
    let snapshot = rig.actor.snapshot().expect("snapshot");
    assert_eq!(snapshot.refused_focus.map(|refused| refused.serial), Some(1));
    assert_eq!(snapshot.focus.workspace, snapshot.workspace("a").map(|ws| ws.id));
}

#[test]
fn an_inactive_focus_stops_every_later_action() {
    let rig = Rig::new(inactive_b().state);
    let (kind, message) = rig.err(
        "workspace",
        json!({"focus": "b", "layout": "grid", "balance": true}),
    );
    assert_eq!(
        (kind, message.as_str()),
        (HandlerErrorKind::Handler, body::WORKSPACE_INACTIVE)
    );
    assert_eq!(rig.actor.submitted(), vec![Intent::FocusWorkspace(
        "b".to_owned()
    )]);
}

// ------------------------------------------- first / middle failure stops

#[test]
fn a_first_action_failure_stops_every_later_action() {
    let (rig, ..) = Rig::standard();
    let (kind, message) = rig.err(
        "workspace",
        json!({"focus": "missing", "layout": "grid", "balance": true}),
    );
    assert_eq!(
        (kind, message.as_str()),
        (HandlerErrorKind::Handler, body::WORKSPACE_NOT_FOUND)
    );
    assert_eq!(rig.actor.submitted(), vec![Intent::FocusWorkspace(
        "missing".to_owned()
    )]);
}

#[test]
fn a_middle_failure_keeps_earlier_admitted_actions_and_stops_later_ones() {
    let (rig, ..) = Rig::standard();
    let (kind, message) = rig.err(
        "window",
        json!({"toggleFloating": true, "sendToWorkspace": "missing", "preset": "x"}),
    );
    assert_eq!(
        (kind, message.as_str()),
        (HandlerErrorKind::Handler, body::WORKSPACE_NOT_FOUND)
    );
    let submitted = rig.actor.submitted();
    assert_eq!(
        submitted,
        vec![
            Intent::ToggleFloating,
            Intent::Preset("x".to_owned()),
            Intent::SendWindowToWorkspace {
                window: None,
                workspace: "missing".to_owned()
            },
        ],
        "no rollback of the toggle"
    );
    // The toggle stayed applied.
    let rows = rig.query(json!({"kind": "windows", "workspace": "a"}));
    assert_eq!(rows[0]["isFloating"], true);
}

#[test]
fn resize_pairs_validate_late_at_their_execution_point() {
    let (rig, ..) = Rig::standard();
    let (kind, message) = rig.err(
        "window",
        json!({
            "toggleFloating": true,
            "resize": [
                {"dimension": "width", "amount": "10"},
                {"dimension": "depth", "amount": "5"},
                {"dimension": "height", "amount": "5"},
            ],
            "sendToWorkspace": "b",
        }),
    );
    assert_eq!(
        (kind, message.as_str()),
        (
            HandlerErrorKind::Handler,
            "Invalid resize dimension 'depth'. Must be 'width' or 'height'."
        )
    );
    assert_eq!(
        rig.actor.submitted(),
        vec![Intent::ToggleFloating, Intent::Resize {
            dimension: ResizeDimension::Width,
            amount: 10
        },],
        "the toggle and the first pair were already admitted"
    );
    assert_eq!(
        rig.err(
            "window",
            json!({"resize": [{"dimension": "WIDTH", "amount": "ten"}]})
        ),
        (
            HandlerErrorKind::Handler,
            "Invalid resize amount 'ten'. Must be an integer.".to_owned()
        )
    );
    rig.ok(
        "window",
        json!({"resize": [{"dimension": "WIDTH", "amount": "-12"}]}),
    );
    assert_eq!(
        rig.actor.submitted().last(),
        Some(&Intent::Resize {
            dimension: ResizeDimension::Width,
            amount: -12
        })
    );
    for amount in ["", "1.5", "2147483648", " 3"] {
        let (kind, message) = rig.err(
            "window",
            json!({"resize": [{"dimension": "width", "amount": amount}]}),
        );
        assert_eq!(
            (kind, message),
            (HandlerErrorKind::Handler, body::invalid_amount(amount))
        );
    }
}

#[test]
fn empty_window_and_workspace_requests_name_the_missing_operation() {
    let (rig, ..) = Rig::standard();
    assert_eq!(
        rig.err("window", json!({})),
        (
            HandlerErrorKind::Handler,
            "No window operation specified. Use --help for available options.".to_owned()
        )
    );
    assert_eq!(
        rig.err("workspace", json!({})),
        (
            HandlerErrorKind::Handler,
            "No workspace operation specified. Use --help for available options.".to_owned()
        )
    );
    assert_eq!(
        rig.err("window", json!({"toggleFloating": false})),
        (HandlerErrorKind::Handler, body::NO_WINDOW_OPERATION.to_owned())
    );
}

#[test]
fn missing_focus_state_reports_the_legacy_bodies_only_where_legacy_did() {
    let (rig, ..) = Rig::standard();
    rig.actor.publish(|state| {
        state.focus = crate::state::FocusState::default();
        for workspace in &mut state.workspaces {
            workspace.focused = false;
        }
    });
    assert_eq!(
        rig.err("window", json!({"toggleFloating": true})),
        (HandlerErrorKind::Handler, body::NO_FOCUSED_WINDOW.to_owned())
    );
    assert_eq!(
        rig.err("workspace", json!({"layout": "grid"})),
        (HandlerErrorKind::Handler, body::NO_FOCUSED_WORKSPACE.to_owned())
    );
    assert_eq!(
        rig.err("workspace", json!({"balance": true})),
        (HandlerErrorKind::Handler, body::NO_FOCUSED_WORKSPACE.to_owned())
    );
    // Focus, swap, resize and preset stay silent admitted no-ops, as in legacy.
    let value = rig.ok(
        "window",
        json!({"focus": "next", "swap": "up", "preset": "center"}),
    );
    assert!(matches!(operation(value), OperationResponse::Enqueued(_)));
}

// ------------------------------------------------------------ preset no-op

#[test]
fn preset_outside_a_floating_workspace_is_a_silent_admitted_noop_with_no_applied_frame() {
    let (rig, editor, _) = Rig::standard();
    let before = rig.actor.snapshot().expect("snapshot");
    let value = rig.ok("window", json!({"preset": "center"}));
    let response = operation(value);
    let OperationResponse::Enqueued(evidence) = &response else {
        panic!("a no-op preset is an enqueued-class success: {response:?}");
    };
    assert_eq!(
        evidence.request.actions[0].status,
        crate::dto::ActionStatus::AdmittedUnchanged
    );
    assert!(evidence.targets.is_empty(), "no frame is reported");
    let after = rig.actor.snapshot().expect("snapshot");
    assert_eq!(
        after.window(editor).map(|w| w.observed.frame),
        before.window(editor).map(|w| w.observed.frame)
    );
    assert_eq!(
        after.diagnostics.intents_noop,
        before.diagnostics.intents_noop + 1
    );
}

// --------------------------------------------------------- actor failures

#[test]
fn saturation_and_a_closed_actor_are_proven_no_send_errors() {
    let (rig, ..) = Rig::standard();
    rig.actor.set_mode(Mode::Full);
    assert_eq!(
        rig.err("window", json!({"toggleFloating": true})),
        (
            HandlerErrorKind::Handler,
            "Actor overflow mailbox is full".to_owned()
        )
    );
    rig.actor.set_mode(Mode::Closed);
    assert_eq!(
        rig.err("workspace", json!({"balance": true})),
        (
            HandlerErrorKind::Handler,
            "Failed to send message to actor: channel closed".to_owned()
        )
    );
    assert!(rig.actor.submitted().is_empty());
}

#[test]
fn the_real_ingress_reports_a_full_queue_instead_of_dropping_the_intent() {
    use crate::events::{Ingress, Pipeline, SEMANTIC_CAPACITY};
    let ingress = Ingress::new(Pipeline::new());
    for _ in 0..SEMANTIC_CAPACITY {
        ingress.submit_intent(Intent::Balance).expect("room");
    }
    assert_eq!(
        ingress.submit_intent(Intent::Balance),
        Err(IntentSubmitError::Full)
    );
    assert_eq!(
        ingress.stats().semantic_depth,
        SEMANTIC_CAPACITY,
        "nothing grew past the bound"
    );
    ingress.shutdown();
    assert_eq!(
        ingress.submit_intent(Intent::Balance),
        Err(IntentSubmitError::Closed)
    );
}

// -------------------------------------------------- deadlines and shutdown

#[test]
fn an_unacknowledged_send_is_uncertain_never_retried_and_later_actions_are_not_attempted() {
    let timing = IpcTiming {
        budget: Duration::from_secs(3),
        hop: Duration::from_millis(60),
        poll: Duration::from_millis(1),
    };
    let (built, ..) = standard();
    let rig = Rig::timed(built.state, timing);
    rig.actor.set_mode(Mode::Swallow);
    let value = rig.ok(
        "window",
        json!({"focus": "next", "swap": "up", "sendToWorkspace": "b"}),
    );
    let OperationResponse::Uncertain(evidence) = operation(value) else {
        panic!("a sent but unacknowledged action is uncertain");
    };
    assert_eq!(evidence.unknown, vec![crate::dto::ReasonEvidence::new(
        "action:focus",
        "noAcknowledgement"
    )]);
    assert_eq!(evidence.not_attempted, vec![
        crate::dto::ReasonEvidence::new("action:swap", "notReached"),
        crate::dto::ReasonEvidence::new("action:sendToWorkspace", "notReached"),
    ]);
    assert_eq!(
        evidence.request.actions[0].status,
        crate::dto::ActionStatus::SentUnconfirmed
    );
    assert_eq!(rig.actor.submitted().len(), 1, "no retry and no later send");
}

#[test]
fn hops_share_the_request_budget() {
    let timing = IpcTiming {
        budget: Duration::from_millis(260),
        hop: Duration::from_millis(200),
        poll: Duration::from_millis(1),
    };
    let (built, ..) = standard();
    let rig = Rig::timed(built.state, timing);
    rig.actor.set_mode(Mode::Delay(Duration::from_millis(150)));
    let started = Instant::now();
    let value = rig.ok("window", json!({"toggleFloating": true, "preset": "x"}));
    let elapsed = started.elapsed();
    let OperationResponse::Uncertain(evidence) = operation(value) else {
        panic!("the second wait ran out of the shared budget");
    };
    assert_eq!(
        evidence.request.actions[0].status,
        crate::dto::ActionStatus::AdmittedChanged
    );
    assert_eq!(evidence.unknown[0].subject, "action:preset");
    assert!(
        elapsed < Duration::from_millis(260 + 150),
        "two hops of 200 ms were not spent: {elapsed:?}"
    );
}

#[test]
fn an_exhausted_budget_before_any_send_is_a_deterministic_timeout_error() {
    let timing = IpcTiming {
        budget: Duration::ZERO,
        hop: Duration::from_secs(1),
        poll: Duration::from_millis(1),
    };
    let (built, ..) = standard();
    let rig = Rig::timed(built.state, timing);
    assert_eq!(
        rig.err("window", json!({"toggleFloating": true})),
        (HandlerErrorKind::Handler, "Query timed out after 0ns".to_owned())
    );
    assert!(
        rig.actor.submitted().is_empty(),
        "abandoned work was never issued"
    );
}

#[test]
fn begin_shutdown_wakes_a_blocked_wait_before_it_would_time_out() {
    let timing = IpcTiming {
        budget: Duration::from_secs(30),
        hop: Duration::from_secs(30),
        poll: Duration::from_millis(5),
    };
    let (built, ..) = standard();
    let rig = Rig::timed(built.state, timing);
    rig.actor.set_mode(Mode::Swallow);
    let cancel = rig.cancel.clone();
    let (sender, receiver) = mpsc::channel();
    let ipc = Arc::clone(&rig.ipc);
    let worker = std::thread::spawn(move || {
        let _ = sender.send(ipc.handle("window", json!({"toggleFloating": true, "swap": "up"})));
    });
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        receiver.try_recv().is_err(),
        "the request is blocked on the actor"
    );
    let cancelled_at = Instant::now();
    cancel.cancel();
    let result = receiver.recv_timeout(Duration::from_secs(2)).expect("woken by shutdown");
    assert!(cancelled_at.elapsed() < Duration::from_secs(1));
    worker.join().expect("join");
    let OperationResponse::Uncertain(evidence) =
        operation(result.expect("uncertain, not an error"))
    else {
        panic!("a send that cannot be confirmed is uncertain");
    };
    assert_eq!(evidence.unknown[0].reason, "shutdown");
    assert_eq!(rig.actor.submitted().len(), 1);
    // Afterwards every new request is refused before anything is sent.
    assert_eq!(
        rig.err("window", json!({"toggleFloating": true})),
        (HandlerErrorKind::Unavailable, body::SHUTTING_DOWN.to_owned())
    );
    assert_eq!(
        rig.err("query", json!({"kind": "screens"})),
        (HandlerErrorKind::Unavailable, body::SHUTTING_DOWN.to_owned())
    );
}

#[test]
fn concurrent_requests_are_serialized_so_each_waits_for_its_own_record() {
    let (rig, ..) = Rig::standard();
    rig.actor.set_mode(Mode::Delay(Duration::from_millis(20)));
    let ipc = Arc::clone(&rig.ipc);
    let handles: Vec<_> = (0..4)
        .map(|n| {
            let ipc = Arc::clone(&ipc);
            std::thread::spawn(move || {
                ipc.handle(
                    "window",
                    json!({"resize": [{"dimension": "width", "amount": n.to_string()}]}),
                )
            })
        })
        .collect();
    for handle in handles {
        let value = handle.join().expect("join").expect("ok");
        assert!(matches!(operation(value), OperationResponse::Enqueued(_)));
    }
    assert_eq!(rig.actor.submitted().len(), 4);
}

// ------------------------------------------------------ pause and resume

pub fn quiet_pause(rig: &Rig) {
    let life = Arc::clone(&rig.life);
    let status = rig.status.clone();
    rig.life.on_pause(move || {
        life.set_module(Some(module(ModuleStatus::Paused, true, true)));
        status.inactive(None);
        Ok(())
    });
}

#[test]
fn pause_waits_for_teardown_and_reports_applied_only_when_acknowledged() {
    let (rig, ..) = Rig::standard();
    quiet_pause(&rig);
    let value = rig.ok("pause", Value::Null);
    let OperationResponse::Applied(evidence) = operation(value) else {
        panic!("an acknowledged teardown is applied");
    };
    assert_eq!(evidence.request.operation, crate::dto::OperationKind::Pause);
    assert_eq!(
        evidence.request.actions[0].status,
        crate::dto::ActionStatus::Acknowledged
    );
    assert_eq!(rig.life.pauses.load(Ordering::SeqCst), 1);
    // `{}` is the same zero-argument request.
    quiet_pause(&rig);
    rig.life.set_module(Some(module(ModuleStatus::Running, true, false)));
    assert!(matches!(
        operation(rig.ok("pause", json!({}))),
        OperationResponse::Applied(_)
    ));
}

#[test]
fn a_cleanup_failure_or_uncertainty_is_never_applied() {
    let (rig, ..) = Rig::standard();
    let status = rig.status.clone();
    rig.life.on_pause(move || {
        status.quarantined(
            vec!["actor".to_owned(), "backend epoch".to_owned()],
            "stuck".to_owned(),
        );
        Err(LifecycleFailure::Failed("stop failed".to_owned()))
    });
    let OperationResponse::Uncertain(evidence) = operation(rig.ok("pause", json!({}))) else {
        panic!("quarantined cleanup is uncertain");
    };
    assert_eq!(evidence.unknown, vec![
        crate::dto::ReasonEvidence::new("stage:actor", "cleanupOwed"),
        crate::dto::ReasonEvidence::new("stage:backend epoch", "cleanupOwed"),
    ]);
    // A repeated pause retries the owed cleanup through the registry.
    assert_eq!(rig.life.pauses.load(Ordering::SeqCst), 1);
    assert!(matches!(
        operation(rig.ok("pause", json!({}))),
        OperationResponse::Uncertain(_)
    ));
    assert_eq!(rig.life.pauses.load(Ordering::SeqCst), 2);
}

#[test]
fn pause_that_does_not_acknowledge_in_time_is_uncertain_and_single_flight() {
    let timing = IpcTiming {
        budget: Duration::from_millis(120),
        hop: Duration::from_millis(60),
        poll: Duration::from_millis(1),
    };
    let (built, ..) = standard();
    let rig = Rig::timed(built.state, timing);
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    rig.life.on_pause(move || {
        let _ = gate.lock().unwrap_or_else(PoisonError::into_inner).recv();
        Ok(())
    });
    let OperationResponse::Uncertain(evidence) = operation(rig.ok("pause", json!({}))) else {
        panic!("an unacknowledged teardown is uncertain");
    };
    assert_eq!(evidence.unknown, vec![crate::dto::ReasonEvidence::new(
        "stage:teardown",
        "noAcknowledgement"
    )]);
    // While the abandoned call still runs, a second pause is refused, not started.
    let OperationResponse::Uncertain(busy) = operation(rig.ok("pause", json!({}))) else {
        panic!("a concurrent lifecycle operation is a typed uncertain outcome");
    };
    assert_eq!(busy.unknown, vec![crate::dto::ReasonEvidence::new(
        "stage:lifecycle",
        "inProgress"
    )]);
    assert_eq!(
        busy.request.actions[0].status,
        crate::dto::ActionStatus::NotAttempted
    );
    assert_eq!(rig.life.pauses.load(Ordering::SeqCst), 1);
    // The abandoned worker is retained, never detached: a bounded join reports
    // it still running, then reaps it once the teardown returns.
    let thread = rig.ipc.lifecycle_thread();
    assert!(
        !thread.join_within(Duration::from_millis(20)),
        "still running and owned"
    );
    release.send(()).expect("release");
    assert!(
        thread.join_within(Duration::from_secs(2)),
        "joined after release"
    );
}

#[test]
fn shutdown_wakes_a_waiting_pause() {
    let timing = IpcTiming {
        budget: Duration::from_secs(30),
        hop: Duration::from_secs(30),
        poll: Duration::from_millis(5),
    };
    let (built, ..) = standard();
    let rig = Rig::timed(built.state, timing);
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    rig.life.on_pause(move || {
        let _ = gate.lock().unwrap_or_else(PoisonError::into_inner).recv();
        Ok(())
    });
    let (sender, receiver) = mpsc::channel();
    let ipc = Arc::clone(&rig.ipc);
    let worker = std::thread::spawn(move || {
        let _ = sender.send(ipc.handle("pause", Value::Null));
    });
    std::thread::sleep(Duration::from_millis(60));
    rig.cancel.cancel();
    let result = receiver.recv_timeout(Duration::from_secs(2)).expect("woken");
    worker.join().expect("join");
    let OperationResponse::Uncertain(evidence) = operation(result.expect("typed outcome")) else {
        panic!("uncertain");
    };
    assert_eq!(evidence.unknown[0].reason, "shutdown");
    release.send(()).expect("release");
}

#[test]
fn disabled_and_absent_modules_neither_pause_nor_resume() {
    let (rig, ..) = Rig::standard();
    rig.life.set_module(None);
    assert_eq!(
        rig.err("pause", json!({})),
        (HandlerErrorKind::Unavailable, body::NOT_INITIALIZED.to_owned())
    );
    rig.life.set_module(Some(module(ModuleStatus::Disabled, false, false)));
    assert_eq!(
        rig.err("pause", json!({})),
        (HandlerErrorKind::Disabled, body::DISABLED.to_owned())
    );
    assert_eq!(
        rig.err("resume", json!({})),
        (HandlerErrorKind::Disabled, body::DISABLED.to_owned())
    );
    assert_eq!(rig.life.pauses.load(Ordering::SeqCst), 0);
    assert_eq!(rig.life.resumes.load(Ordering::SeqCst), 0);
}

#[test]
fn resume_uses_registry_semantics_and_cannot_override_quarantine() {
    let (rig, ..) = Rig::standard();
    rig.life.set_module(Some(module(ModuleStatus::Paused, true, true)));
    rig.status.inactive(None);
    let life = Arc::clone(&rig.life);
    let status = rig.status.clone();
    rig.life.on_resume(move || {
        life.set_module(Some(module(ModuleStatus::Running, true, false)));
        status.running(Arc::new(SnapshotCell::default()));
        Ok(())
    });
    let OperationResponse::Applied(evidence) = operation(rig.ok("resume", Value::Null)) else {
        panic!("a started module is applied");
    };
    assert_eq!(evidence.request.operation, crate::dto::OperationKind::Resume);

    rig.status.quarantined(vec!["actor".to_owned()], "stuck".to_owned());
    let resumes = rig.life.resumes.load(Ordering::SeqCst);
    assert_eq!(
        rig.err("resume", json!({})),
        (HandlerErrorKind::Unavailable, body::CLEANUP_PENDING.to_owned())
    );
    assert_eq!(
        rig.life.resumes.load(Ordering::SeqCst),
        resumes,
        "the registry was not asked"
    );
}

#[test]
fn a_resume_whose_start_fails_reports_the_registry_failure() {
    let (rig, ..) = Rig::standard();
    rig.life.set_module(Some(module(ModuleStatus::Paused, true, true)));
    rig.status.inactive(None);
    rig.life.on_resume(|| Err(LifecycleFailure::Failed("start refused".to_owned())));
    assert_eq!(
        rig.err("resume", json!({})),
        (HandlerErrorKind::Handler, "start refused".to_owned())
    );
    rig.life.on_resume(|| Err(LifecycleFailure::ShuttingDown));
    assert_eq!(
        rig.err("resume", json!({})),
        (HandlerErrorKind::Unavailable, body::SHUTTING_DOWN.to_owned())
    );
}

// --------------------------------------------- native outcome -> wire (typed)

mod native {
    use stache_macos::ax::AxError;

    use super::*;
    use crate::dto::{ComponentDto, OperationKind, RequestEvidence, from_native};
    use crate::geometry::Rect;
    use crate::outcome::{
        ComponentEvidence, FailureReason, NotAttemptedReason, OperationOutcome, StaleCause,
        TargetEvidence,
    };

    fn request() -> RequestEvidence {
        RequestEvidence {
            operation: OperationKind::Window,
            actions: Vec::new(),
        }
    }

    fn target(
        window: WindowInstance,
        position: ComponentEvidence,
        size: ComponentEvidence,
    ) -> TargetEvidence {
        TargetEvidence {
            window,
            plan_sequence: 9,
            expected: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            position,
            size,
            actual: Some(Rect {
                x: 1.0,
                y: 2.0,
                width: 90.0,
                height: 80.0,
            }),
        }
    }

    #[test]
    fn uncertain_and_partial_native_results_stay_typed_ok_outcomes() {
        let (_, editor, _) = standard();
        let cg = |token: WindowInstance| (token == editor).then_some(101);

        let uncertain = from_native(
            request(),
            &OperationOutcome::Uncertain {
                targets: vec![target(
                    editor,
                    ComponentEvidence::Confirmed,
                    ComponentEvidence::Unobserved,
                )],
            },
            &cg,
        )
        .expect("ok");
        let OperationResponse::Uncertain(evidence) = &uncertain else {
            panic!("{uncertain:?}")
        };
        assert_eq!(evidence.targets[0].window, Some(101));
        assert_eq!(evidence.targets[0].size, ComponentDto::Unobserved);
        assert_eq!(evidence.unknown, vec![crate::dto::ReasonEvidence::new(
            "window:101:size",
            "unobserved"
        )]);
        assert!(uncertain.is_physical_failure());

        let partial = from_native(
            request(),
            &OperationOutcome::Partial {
                targets: vec![target(
                    editor,
                    ComponentEvidence::Confirmed,
                    ComponentEvidence::Refused(AxError::CannotComplete),
                )],
            },
            &cg,
        )
        .expect("ok");
        let OperationResponse::Partial(evidence) = &partial else {
            panic!("{partial:?}")
        };
        assert_eq!(evidence.failed, vec![crate::dto::ReasonEvidence::new(
            "window:101:size",
            "refused"
        )]);

        let enqueued = from_native(request(), &OperationOutcome::Enqueued, &cg).expect("ok");
        assert!(
            matches!(enqueued, OperationResponse::Enqueued(_)) && !enqueued.is_physical_failure()
        );
        let applied = from_native(
            request(),
            &OperationOutcome::Applied {
                targets: vec![target(
                    editor,
                    ComponentEvidence::Confirmed,
                    ComponentEvidence::Confirmed,
                )],
            },
            &cg,
        )
        .expect("ok");
        assert!(matches!(applied, OperationResponse::Applied(_)));
    }

    #[test]
    fn known_immediate_failures_are_typed_errors_not_ok_bodies() {
        let (_, editor, _) = standard();
        let cg = |_: WindowInstance| None;
        let refused = from_native(
            request(),
            &OperationOutcome::Failed {
                reason: FailureReason::Refused,
                targets: vec![target(
                    editor,
                    ComponentEvidence::Refused(AxError::CannotComplete),
                    ComponentEvidence::NotSent,
                )],
            },
            &cg,
        )
        .expect_err("refusal");
        assert_eq!(refused.message, "Tiling operation was refused by the window");
        let skipped = from_native(
            request(),
            &OperationOutcome::NotAttempted {
                reason: NotAttemptedReason::QueueFull,
                window: editor,
                plan_sequence: 1,
            },
            &cg,
        )
        .expect_err("no send");
        assert_eq!(skipped.message, "Tiling operation was not attempted (queueFull)");
        let stale = from_native(
            request(),
            &OperationOutcome::Stale {
                cause: StaleCause::Plan,
                window: editor,
                plan_sequence: 1,
            },
            &cg,
        )
        .expect_err("stale");
        assert_eq!(stale.message, "Tiling operation was superseded by a newer plan");
    }
}

// --------------------------------------------------- real socket round trips

pub struct NoCore;

impl CoreHandler for NoCore {
    fn reload(&self) -> HandlerResult<CoreReload> {
        Ok(CoreReload {
            files: Vec::new(),
            access: None,
        })
    }

    fn status(&self) -> HandlerResult<CoreStatus> {
        Ok(CoreStatus {
            directory: "/config".into(),
            files: Vec::new(),
            access: None,
        })
    }
}

struct Wire {
    _root: tempfile::TempDir,
    _server: IpcServer,
    client: IpcClient,
}

impl Wire {
    fn new(handler: Arc<TilingIpc>) -> Self {
        let root = tempfile::tempdir().expect("root");
        let paths = AppPaths::resolve(&AppEnvironment {
            home: Some(root.path().to_path_buf()),
            temporary: Some(root.path().to_path_buf()),
        })
        .expect("paths");
        let mut router = Router::new(Arc::new(NoCore));
        router.register("tiling", handler).expect("register");
        let server = IpcServer::start(&paths, Arc::new(router)).expect("server");
        let client = IpcClient::with_options(
            server.socket_path(),
            RetryPolicy::single(),
            Duration::from_secs(5),
        );
        Self {
            _root: root,
            _server: server,
            client,
        }
    }

    fn call(&self, method: &str, params: Value) -> Response {
        self.client
            .request(&Request {
                module: "tiling".to_owned(),
                method: method.to_owned(),
                params,
            })
            .expect("transport")
    }
}

#[test]
fn queries_and_mutations_round_trip_over_the_real_socket() {
    let (rig, ..) = Rig::standard();
    let wire = Wire::new(Arc::clone(&rig.ipc));

    let Response::Ok { data } = wire.call("query", json!({"kind": "windows", "workspace": "g"}))
    else {
        panic!("ok");
    };
    assert_eq!(data[0]["title"], "Browser \"quoted\" 日本語\n🙂");
    assert_eq!(data[0]["id"], 201);

    let Response::Ok { data } = wire.call("window", json!({"focus": "201"})) else {
        panic!("ok")
    };
    assert!(matches!(operation(data), OperationResponse::Enqueued(_)));

    let Response::Error { error } = wire.call("window", json!({"focus": "9999"})) else {
        panic!("not found");
    };
    assert_eq!(
        (error.kind, error.message.as_str()),
        (WireErrorKind::Handler, "Window 9999 not found")
    );

    let Response::Error { error } = wire.call("query", json!({"kind": "screens", "extra": 1}))
    else {
        panic!("invalid");
    };
    assert_eq!(error.kind, WireErrorKind::InvalidParams);

    rig.life.set_module(Some(module(ModuleStatus::Disabled, false, false)));
    let Response::Error { error } = wire.call("query", json!({"kind": "screens"})) else {
        panic!("disabled");
    };
    assert_eq!(
        (error.kind, error.message.as_str()),
        (WireErrorKind::Disabled, "Tiling is disabled")
    );
    let Response::Error { error } = wire.call("frobnicate", json!({})) else {
        panic!("unknown")
    };
    assert_eq!(error.kind, WireErrorKind::UnknownMethod);
}

#[test]
fn an_unconfirmed_send_and_a_cleanup_failure_are_ok_bodies_on_the_wire() {
    let timing = IpcTiming {
        budget: Duration::from_secs(3),
        hop: Duration::from_millis(50),
        poll: Duration::from_millis(1),
    };
    let (built, ..) = standard();
    let rig = Rig::timed(built.state, timing);
    rig.actor.set_mode(Mode::Swallow);
    let status = rig.status.clone();
    rig.life.on_pause(move || {
        status.quarantined(vec!["display observer".to_owned()], "stuck".to_owned());
        Err(LifecycleFailure::Failed("x".to_owned()))
    });
    let wire = Wire::new(Arc::clone(&rig.ipc));

    let Response::Ok { data } = wire.call("window", json!({"toggleFloating": true})) else {
        panic!("uncertain stays an ok body");
    };
    assert!(matches!(operation(data), OperationResponse::Uncertain(_)));
    let Response::Ok { data } = wire.call("pause", Value::Null) else {
        panic!("ok")
    };
    assert!(matches!(operation(data), OperationResponse::Uncertain(_)));
}

#[test]
fn shutdown_while_a_socket_request_waits_answers_before_the_client_budget() {
    let timing = IpcTiming {
        budget: Duration::from_secs(4),
        hop: Duration::from_secs(2),
        poll: Duration::from_millis(5),
    };
    let (built, ..) = standard();
    let rig = Rig::timed(built.state, timing);
    rig.actor.set_mode(Mode::Swallow);
    let wire = Wire::new(Arc::clone(&rig.ipc));
    let cancel = rig.cancel;
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        cancel.cancel();
    });
    let started = Instant::now();
    let Response::Ok { data } = wire.call("window", json!({"swap": "up"})) else {
        panic!("ok")
    };
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
    canceller.join().expect("join");
    assert!(matches!(operation(data), OperationResponse::Uncertain(_)));
}
