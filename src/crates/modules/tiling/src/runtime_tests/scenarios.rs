//! Whole-desktop scenarios: the real actor, transition, ledger, frame control
//! and application actions wired together over fakes, driven like a desktop
//! (~10 applications, one built-in display, the user's workspace rules). Only
//! the native edges are fake: the window backend, the application backend and
//! the display layout. Nothing touches a real window or application.

use std::sync::Mutex;

use serde_json::json;
use stache_macos::applications::LifecycleKind;
use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead};
use stache_macos::windows::SnapshotOrigin;
use stache_macos::{FakeAppKey, VisibilityOperation};

use super::*;
use crate::actor::{EffectRunner, RunnerError};
use crate::config::{LayoutType, TilingConfig};
use crate::geometry::Rect;
use crate::intent::{Intent, SwapDirection};
use crate::state::{AppInfo, Effect, Scope};
use crate::test_support::{Feed, one_display, snapshot};
use crate::visibility::{ActionsDirectory, VisibilityControl};
use crate::visibility_tests::Native;

const GHOSTTY: usize = 0;
const CODE: usize = 1;
const SPOTIFY: usize = 2;
const WHATSAPP: usize = 3;
const CHROME: usize = 4;
const MENUBAR: usize = 5;
const BETTERDISPLAY: usize = 6;
const ZOOM: usize = 7;
const FINDER: usize = 8;
const DEVICE_HUB: usize = 9;

const APPS: [(&str, &str); 10] = [
    ("com.mitchellh.ghostty", "Ghostty"),
    ("com.microsoft.VSCode", "Code"),
    ("com.spotify.client", "Spotify"),
    ("net.whatsapp.WhatsApp", "WhatsApp"),
    ("com.google.Chrome", "Google Chrome"),
    ("com.apple.MenuBarAgent", "MenuBarAgent"),
    ("pro.betterdisplay.BetterDisplay", "BetterDisplay"),
    ("us.zoom.xos", "zoom.us"),
    ("com.apple.finder", "Finder"),
    ("com.example.DeviceHub", "Device Hub"),
];

/// How the system reports the hides and unhides Stache caused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Delivery {
    /// Each notification arrives right away.
    Immediate,
    /// Each arrives twice (the workspace and the AX channel both report).
    Twice,
    /// Each arrives after the next user action.
    Late,
    /// None ever arrives.
    Never,
}

fn user_config(animations: bool) -> Arc<TilingConfig> {
    Arc::new(
        serde_json::from_value(json!({
            "enabled": true,
            "animations": { "enabled": animations },
            "master": { "position": "left", "ratio": 75 },
            "ignore": [
                { "app_id": "com.apple.finder", "title": "Copy" },
                { "app_name": "Raycast" },
            ],
            "workspaces": [
                { "name": "terminal", "layout": "master", "screen": "main",
                  "rules": [{ "app_id": "com.mitchellh.ghostty" },
                            { "app_id": "com.example.DeviceHub" }] },
                { "name": "coding", "layout": "monocle", "screen": "main",
                  "rules": [{ "app_id": "com.microsoft.VSCode" }, { "app_id": "dev.zed.Zed" }] },
                { "name": "browser", "layout": "monocle", "screen": "secondary",
                  "rules": [{ "app_id": "com.google.Chrome" }] },
                { "name": "music", "layout": "monocle", "screen": "main",
                  "rules": [{ "app_id": "com.spotify.client" }] },
                { "name": "communication", "layout": "floating", "screen": "main",
                  "rules": [{ "app_id": "net.whatsapp.WhatsApp" }, { "app_id": "com.hnc.Discord" }] },
                { "name": "misc", "layout": "floating", "screen": "main" },
                { "name": "guitar", "layout": "floating", "screen": "main" },
                { "name": "files", "layout": "grid", "screen": "main",
                  "rules": [{ "app_id": "com.apple.finder" }] },
                { "name": "tasks", "layout": "grid", "screen": "main" },
            ],
        }))
        .expect("the user's config"),
    )
}

/// Records every frame write the actor queues, then runs it.
struct RecordingRunner {
    inner: InlineRunner,
    writes: Arc<Mutex<Vec<Write>>>,
    /// Application lists requested.
    lists: Arc<std::sync::atomic::AtomicUsize>,
}

/// One queued frame write.
#[derive(Clone, Copy, Debug)]
struct Write {
    window: WindowInstance,
    target: Rect,
    animate: bool,
}

impl EffectRunner for RecordingRunner {
    fn submit(&self, effect: Effect) -> Result<(), RunnerError> {
        if let Effect::WriteFrame { window, target, animate, .. } = effect {
            self.writes.lock().expect("writes").push(Write { window, target, animate });
        }
        if matches!(effect, Effect::ListApplications { .. }) {
            self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        self.inner.submit(effect)
    }

    fn close(&self) { self.inner.close(); }
}

struct Desktop {
    writes: Arc<Mutex<Vec<Write>>>,
    lists: Arc<std::sync::atomic::AtomicUsize>,
    /// Whether a Device Hub window joins Ghostty on the terminal workspace.
    hub: bool,
    animations: bool,
    rig: Rig,
    native: Arc<Native>,
    keys: Vec<FakeAppKey>,
    ids: Vec<ApplicationId>,
    delivery: Delivery,
    /// Requests already turned into notifications.
    seen: usize,
    late: Vec<(usize, LifecycleKind)>,
    /// Whether each application is visible, as the system would see it.
    shown: Vec<bool>,
    serial: u64,
    /// Applications whose focused window the fake system already reports.
    focus_scripted: std::collections::BTreeSet<usize>,
    /// The frontmost application, as the system sees it.
    frontmost: usize,
    /// The application the system activates when the frontmost one is hidden
    /// (macOS picks the next one in its own order), if any.
    successor: Option<usize>,
    /// Whether Code has a second window, the one it reports as focused.
    second_code: bool,
}

/// The order the startup inputs reach the actor in (see [`Desktop::boot_in`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Order {
    /// Everything is submitted before the first window is listed.
    Listed,
    /// As [`Order::Listed`], and Code's focused window is its second one.
    SecondWindowFocused,
    /// The display layout arrives after the windows.
    DisplaysLast,
    /// Frame writes are enabled after the windows are listed.
    WritesLast,
    /// The frontmost application is reported after the windows are listed.
    ActivationLast,
}

impl Desktop {
    /// A desktop whose frontmost application is `frontmost`, with every
    /// application launched, tracked and its windows enumerated.
    fn boot(delivery: Delivery, frontmost: usize) -> Self {
        Self::boot_with(delivery, frontmost, false, false)
    }

    fn boot_with(delivery: Delivery, frontmost: usize, hub: bool, animations: bool) -> Self {
        Self::boot_ordered(delivery, frontmost, hub, animations, None)
    }

    fn boot_ordered(
        delivery: Delivery,
        frontmost: usize,
        hub: bool,
        animations: bool,
        order: Option<Order>,
    ) -> Self {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let lists = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let native = Arc::new(Native::new(Duration::from_millis(500)));
        let bundles: Vec<&str> = APPS.iter().map(|(bundle, _)| *bundle).collect();
        let (actions, pairs) = native.install(&bundles);
        let (ids, keys): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
        for (key, (_, name)) in keys.iter().zip(APPS) {
            native.backend.set_name(*key, Some(name));
        }
        native.backend.set_regular(keys[MENUBAR], false);
        let mut rig = Rig::new("", Vec::new(), 256, 256);
        rig.backend.script_echo_frames(true);
        rig.backend.script_track_for(ids[ZOOM], TrackOutcome::Unacknowledged);
        let vis = Arc::new(VisibilityControl::new());
        let actions = Arc::new(actions);
        let env = EffectEnv {
            backend: Arc::clone(&rig.backend) as Arc<dyn WindowBackend>,
            directory: Arc::new(ActionsDirectory::new(Arc::clone(&actions))),
            ingress: Arc::clone(&rig.ingress),
            cancel: CancellationToken::new(),
            frames: Arc::clone(&rig.frames),
            visibility: Arc::clone(&vis),
            actions: Some(actions),
            animation: Arc::clone(&rig.animation),
        };
        let mut engine = Engine::new(
            TilingState::new("com.stache.test"),
            Arc::clone(&rig.ingress),
            Arc::clone(&rig.cell),
            Arc::new(RecordingRunner {
                inner: InlineRunner::new(env),
                writes: Arc::clone(&writes),
                lists: Arc::clone(&lists),
            }),
            Arc::clone(&rig.sink),
            Arc::clone(&rig.clock) as Arc<dyn crate::actor::Clock>,
        );
        engine.set_frame_control(Arc::clone(&rig.frames));
        engine.set_visibility_control(Arc::clone(&vis));
        rig.engine = engine;
        let mut desktop = Self {
            writes,
            lists,
            hub,
            animations,
            rig,
            native,
            keys,
            ids,
            delivery,
            seen: 0,
            late: Vec::new(),
            shown: vec![true; APPS.len()],
            serial: 0,
            focus_scripted: std::collections::BTreeSet::new(),
            frontmost,
            successor: None,
            second_code: false,
        };
        if let Some(order) = order {
            desktop.boot_in(order);
            return desktop;
        }
        desktop.rig.start();
        // The activation watch reports the frontmost application at once.
        desktop.submit(Input::Activated(desktop.ids[frontmost]));
        desktop
            .rig
            .ingress
            .submit(Input::Config {
                revision: 1,
                config: user_config(desktop.animations),
            })
            .expect("config");
        desktop.enumerate();
        desktop.rig.gate.publish(desktop.rig.feed.stamp.registry, 1);
        desktop.submit(Input::FrameWrites(true));
        desktop.settle();
        desktop
    }

    /// Starts in one of the orders the real runtime can deliver its inputs in.
    /// The runtime starts listing, then installs the application watches (which
    /// report the frontmost application), then submits the configuration and
    /// enables frame writes, while applications are tracked and their windows
    /// listed on other threads: only the relative order of those inputs is fixed.
    fn boot_in(&mut self, order: Order) {
        let front = self.ids[self.frontmost];
        let config = Input::Config {
            revision: 1,
            config: user_config(self.animations),
        };
        let displays = Input::Displays(Arc::new(super::one_display()));
        let stamp = self.rig.feed.stamp;
        self.rig.gate.publish(stamp.registry, 1);
        // Whatever Stache hides, the system activates something else.
        self.successor = Some(if self.frontmost == GHOSTTY {
            CHROME
        } else {
            GHOSTTY
        });
        let queue = |desk: &Self, input: Input| desk.rig.ingress.submit(input).expect("input");
        queue(self, Input::Start(stamp));
        if order != Order::DisplaysLast {
            queue(self, displays.clone());
        }
        match order {
            Order::Listed | Order::SecondWindowFocused => {
                queue(self, Input::Activated(front));
                queue(self, config);
                queue(self, Input::FrameWrites(true));
                self.second_code = order == Order::SecondWindowFocused;
                self.enumerate();
            }
            Order::DisplaysLast => {
                queue(self, Input::Activated(front));
                queue(self, config);
                queue(self, Input::FrameWrites(true));
                self.enumerate();
                queue(self, displays);
            }
            Order::WritesLast => {
                queue(self, Input::Activated(front));
                queue(self, config);
                self.enumerate();
                queue(self, Input::FrameWrites(true));
            }
            Order::ActivationLast => {
                queue(self, config);
                self.enumerate();
                // Queued together: the focus read answers after the writes are on.
                queue(self, Input::Activated(front));
                queue(self, Input::FrameWrites(true));
            }
        }
        self.settle();
    }

    fn submit(&mut self, input: Input) {
        self.rig.ingress.submit(input).expect("input");
        self.rig.pump();
    }

    fn token(&self, app: usize, serial: u64) -> WindowInstance {
        WindowInstance::for_testing(
            self.rig.feed.adapter,
            self.rig.feed.epoch,
            self.ids[app],
            serial,
        )
    }

    fn snapshot_of(
        &self,
        token: WindowInstance,
        title: &str,
        subrole: &str,
        size: (f64, f64),
    ) -> stache_macos::windows::WindowSnapshot {
        let mut snapshot = snapshot(token, title, 1000 + u32::try_from(self.serial).unwrap_or(0));
        snapshot.subrole = AxReading::Value(subrole.to_owned());
        snapshot.frame = FrameRead {
            position: AxReading::Value(AxPoint { x: 40.0, y: 60.0 }),
            size: AxReading::Value(AxSize { width: size.0, height: size.1 }),
        };
        snapshot
    }

    /// One ordinary window enumerated at startup.
    fn window(&mut self, app: usize, title: &str) -> WindowInstance {
        self.window_with(
            app,
            title,
            "AXStandardWindow",
            (1200.0, 800.0),
            SnapshotOrigin::Enumeration,
        )
    }

    fn window_with(
        &mut self,
        app: usize,
        title: &str,
        subrole: &str,
        size: (f64, f64),
        origin: SnapshotOrigin,
    ) -> WindowInstance {
        self.serial += 1;
        let token = self.token(app, self.serial);
        // The system reports an application's first window as its focused one
        // until a scenario says otherwise (`app_focuses`).
        if self.focus_scripted.insert(app) {
            self.rig.backend.script_focused(self.ids[app], token);
        }
        let snapshot = self.snapshot_of(token, title, subrole, size);
        self.rig.push(WindowEventKind::Snapshot { snapshot, origin });
        self.rig.pump();
        token
    }

    /// Enumerates the windows the user's desktop has.
    fn enumerate(&mut self) {
        self.rig.pump();
        self.window(GHOSTTY, "~");
        self.window(CODE, "stache");
        self.window(SPOTIFY, "Spotify");
        self.window(WHATSAPP, "WhatsApp");
        self.window(CHROME, "New Tab");
        if self.hub {
            self.window(DEVICE_HUB, "Device Hub");
        }
        if self.second_code {
            // The window the user is really on is not the first one listed.
            let second = self.window(CODE, "notes");
            self.app_focuses(CODE, second);
        }
        // Not tileable: a 1x1 utility window, a Finder info popup, a sheet.
        self.window_with(
            BETTERDISPLAY,
            "",
            "AXStandardWindow",
            (1.0, 1.0),
            SnapshotOrigin::Enumeration,
        );
        self.window_with(
            FINDER,
            "notes.txt Info",
            "AXStandardWindow",
            (400.0, 500.0),
            SnapshotOrigin::Enumeration,
        );
        self.window_with(CODE, "", "AXSheet", (600.0, 300.0), SnapshotOrigin::Enumeration);
        for app in [
            GHOSTTY,
            CODE,
            SPOTIFY,
            WHATSAPP,
            CHROME,
            BETTERDISPLAY,
            FINDER,
            DEVICE_HUB,
        ] {
            self.rig.complete(self.ids[app]);
        }
        self.rig.pump();
    }

    /// Runs the engine until quiet, turning every hide and unhide Stache sent
    /// into the notifications the system would deliver.
    fn settle(&mut self) {
        loop {
            self.rig.pump();
            let requests = self.native.backend.visibility_requests();
            if requests.len() == self.seen {
                return;
            }
            let fresh: Vec<_> = requests[self.seen..].to_vec();
            self.seen = requests.len();
            // Hiding the frontmost application makes the system activate
            // another one right away, before the other hides of the switch have
            // landed and been reported. Nobody asked for it.
            let hides_frontmost = fresh.iter().any(|(key, operation)| {
                *operation == VisibilityOperation::Hide && self.keys[self.frontmost] == *key
            });
            if let (true, Some(next)) = (hides_frontmost, self.successor) {
                self.frontmost = next;
                self.submit(Input::Activated(self.ids[next]));
            }
            for (key, operation) in fresh {
                let index = self.keys.iter().position(|k| *k == key).expect("known app");
                let kind = if operation == VisibilityOperation::Hide {
                    LifecycleKind::Hidden
                } else {
                    LifecycleKind::Unhidden
                };
                self.shown[index] = kind == LifecycleKind::Unhidden;
                match self.delivery {
                    Delivery::Immediate => self.notify(index, kind),
                    Delivery::Twice => {
                        self.notify(index, kind);
                        self.notify(index, kind);
                    }
                    Delivery::Late => self.late.push((index, kind)),
                    Delivery::Never => {}
                }
            }
        }
    }

    fn notify(&mut self, app: usize, kind: LifecycleKind) {
        let info = AppInfo {
            application: self.ids[app],
            pid: 100 + i32::try_from(app).unwrap_or_default(),
            bundle_id: APPS[app].0.to_owned(),
            name: APPS[app].1.to_owned(),
        };
        self.submit(Input::Lifecycle { kind, info });
    }

    /// Delivers the notifications held back by [`Delivery::Late`].
    fn release_late(&mut self) {
        for (app, kind) in std::mem::take(&mut self.late) {
            self.notify(app, kind);
        }
        self.settle();
    }

    /// `stache tiling workspace --focus NAME`, as the IPC handler submits it.
    fn focus_workspace(&mut self, name: &str) {
        self.release_late();
        self.submit(Input::Intent(Intent::FocusWorkspace(name.to_owned())));
        self.settle();
    }

    /// The user brings `app` forward with Cmd-Tab: the system unhides it,
    /// activates it and (for a window that changes) reports the focus.
    fn cmd_tab(&mut self, app: usize) {
        // Delayed notices still arrive in order, before the user's next action.
        self.release_late();
        if !self.shown[app] {
            self.shown[app] = true;
            self.notify(app, LifecycleKind::Unhidden);
        }
        self.frontmost = app;
        self.submit(Input::Activated(self.ids[app]));
        self.settle();
    }

    /// The fake system now reports `window` as `app`'s focused window.
    fn app_focuses(&mut self, app: usize, window: WindowInstance) {
        self.focus_scripted.insert(app);
        self.rig.backend.script_focused(self.ids[app], window);
    }

    fn snap(&self) -> Arc<TilingSnapshot> { self.rig.snap() }

    fn visible_workspace(&self) -> String {
        let snap = self.snap();
        let visible: Vec<_> = snap.workspaces.iter().filter(|ws| ws.visible).collect();
        assert_eq!(
            visible.len(),
            1,
            "one display: one visible workspace: {visible:?}"
        );
        visible[0].name.clone()
    }

    fn workspace_of(&self, window: WindowInstance) -> String {
        let snap = self.snap();
        let id = snap.window(window).and_then(|w| w.desired.workspace).expect("assigned");
        snap.workspaces.iter().find(|ws| ws.id == id).expect("workspace").name.clone()
    }

    fn shown_set(&self) -> Vec<&'static str> {
        APPS.iter()
            .enumerate()
            .filter(|(index, _)| self.shown[*index])
            .map(|(_, (_, name))| *name)
            .collect()
    }

    /// Frame writes queued so far.
    fn mark(&self) -> usize { self.writes.lock().expect("writes").len() }

    /// Frame writes queued since `mark`.
    fn writes_since(&self, mark: usize) -> Vec<Write> {
        self.writes.lock().expect("writes")[mark..].to_vec()
    }

    fn desired_of(&self, window: WindowInstance) -> Rect {
        let snap = self.snap();
        snap.frames
            .iter()
            .find(|status| status.window == window)
            .and_then(|status| status.desired)
            .expect("desired frame")
    }

    fn actual_of(&self, window: WindowInstance) -> Rect {
        let snap = self.snap();
        snap.frames
            .iter()
            .find(|status| status.window == window)
            .and_then(|status| status.actual)
            .expect("actual frame")
    }

    fn written(&self, window: WindowInstance) -> bool {
        self.rig.backend.written_frames().iter().any(|(token, _)| *token == window)
    }
}

// ---------------------------------------------------------------- bug 1: layout

#[test]
fn one_app_that_cannot_be_tracked_never_blocks_the_layout_of_the_others() {
    let desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    let snap = desk.snap();
    assert_eq!(
        snap.phase,
        Phase::Complete,
        "a failed app does not make the phase incomplete"
    );
    assert_eq!(snap.failed_apps.len(), 1);
    assert_eq!(snap.failed_apps[0].application, desk.ids[ZOOM]);
    assert!(
        snap.failed_apps[0].reason.contains("Unacknowledged"),
        "{:?}",
        snap.failed_apps
    );
    let ghostty = desk.token(GHOSTTY, 1);
    assert!(
        desk.written(ghostty),
        "the terminal window was laid out: {:?}",
        desk.rig.calls()
    );
    // It filled the usable area (master layout, one window).
    let frame = snap.frames.iter().find(|s| s.window == ghostty).expect("frame status");
    assert!(frame.actual.is_some_and(|f| f.width > 1000.0), "{frame:?}");
}

#[test]
fn a_reconciling_app_holds_back_only_its_own_windows() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    // Code starts a refresh that has not finished.
    desk.submit(Input::Unconfirmed(
        Scope::App(desk.ids[CODE]),
        crate::state::Cause::ObserverOverflow,
    ));
    assert_eq!(desk.snap().phase, Phase::Reconciling);
    // A new terminal window still gets its frame.
    let second = desk.window_with(
        GHOSTTY,
        "second",
        "AXStandardWindow",
        (900.0, 600.0),
        SnapshotOrigin::Created,
    );
    desk.settle();
    assert!(
        desk.written(second),
        "other apps keep being laid out while one reconciles"
    );
    // The unconfirmed app's own window was not written meanwhile.
    let code = desk.token(CODE, 2);
    assert!(!desk.written(code));
}

#[test]
fn failed_apps_are_retried_on_activation_a_bounded_number_of_times() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    let zoom = desk.ids[ZOOM];
    let tracks = |desk: &Desktop| {
        desk.rig
            .calls()
            .iter()
            .filter(|call| matches!(call, FakeBackendCall::Track(app, _) if *app == zoom))
            .count()
    };
    assert_eq!(tracks(&desk), 1);
    for _ in 0..6 {
        desk.rig.backend.script_track_for(zoom, TrackOutcome::Unacknowledged);
        desk.submit(Input::Activated(zoom));
        desk.submit(Input::Activated(desk.ids[GHOSTTY]));
    }
    assert_eq!(
        tracks(&desk),
        1 + usize::from(crate::state::MAX_TRACK_RETRIES),
        "bounded"
    );
    // Once it can be tracked it recovers.
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    let zoom = desk.ids[ZOOM];
    desk.submit(Input::Activated(zoom));
    desk.rig.complete(zoom);
    desk.rig.pump();
    assert!(
        desk.snap().failed_apps.is_empty(),
        "{:?} {:?}",
        desk.snap().failed_apps,
        desk.rig.calls()
    );
}

// ------------------------------------------------------- bug 3: visibility

fn round_trips(delivery: Delivery) {
    let mut desk = Desktop::boot(delivery, GHOSTTY);
    desk.release_late();
    assert_eq!(desk.visible_workspace(), "terminal");
    assert_eq!(
        desk.shown_set(),
        [
            "Ghostty",
            "MenuBarAgent",
            "BetterDisplay",
            "zoom.us",
            "Finder",
            "Device Hub"
        ],
        "{delivery:?}: everything on other workspaces starts hidden"
    );
    for round in 0..3 {
        desk.focus_workspace("music");
        assert_eq!(desk.visible_workspace(), "music", "{delivery:?} round {round}");
        assert!(
            desk.shown[SPOTIFY],
            "{delivery:?} round {round}: Spotify is shown"
        );
        assert!(
            !desk.shown[GHOSTTY],
            "{delivery:?} round {round}: Ghostty is hidden"
        );
        assert!(
            !desk.shown[CODE],
            "{delivery:?} round {round}: Code stays hidden"
        );
        desk.cmd_tab(GHOSTTY);
        assert_eq!(
            desk.visible_workspace(),
            "terminal",
            "{delivery:?} round {round}"
        );
        assert!(
            desk.shown[GHOSTTY],
            "{delivery:?} round {round}: Ghostty is shown by Cmd-Tab"
        );
        assert!(
            !desk.shown[SPOTIFY],
            "{delivery:?} round {round}: Spotify is hidden again"
        );
        desk.release_late();
        assert!(
            !desk.shown[SPOTIFY] && desk.shown[GHOSTTY],
            "{delivery:?} round {round} after late notices"
        );
        assert_eq!(desk.visible_workspace(), "terminal");
    }
    // Bounded: a few requests per switch, no oscillation.
    let requests = desk.native.backend.visibility_requests().len();
    assert!(requests <= 8 + 3 * 8, "{delivery:?}: {requests} requests");
}

#[test]
fn workspace_switches_hide_and_unhide_correctly_when_notifications_arrive_promptly() {
    round_trips(Delivery::Immediate);
}

#[test]
fn workspace_switches_survive_duplicate_notifications() { round_trips(Delivery::Twice); }

#[test]
fn workspace_switches_survive_notifications_arriving_late() { round_trips(Delivery::Late); }

#[test]
fn workspace_switches_survive_notifications_that_never_arrive() { round_trips(Delivery::Never); }

#[test]
fn switching_by_intent_between_four_workspaces_keeps_exactly_one_group_visible() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    for name in [
        "coding",
        "music",
        "terminal",
        "communication",
        "coding",
        "terminal",
    ] {
        desk.focus_workspace(name);
        assert_eq!(desk.visible_workspace(), name);
        let wanted: &[usize] = match name {
            "coding" => &[CODE],
            "music" => &[SPOTIFY],
            "terminal" => &[GHOSTTY],
            _ => &[WHATSAPP],
        };
        for app in [GHOSTTY, CODE, SPOTIFY, WHATSAPP, CHROME] {
            assert_eq!(desk.shown[app], wanted.contains(&app), "{name}: {}", APPS[app].1);
        }
    }
}

// ---------------------------------------------------------------- bug 6: focus

#[test]
fn the_frontmost_app_at_startup_is_the_focused_window_and_its_workspace_is_visible() {
    let desk = Desktop::boot(Delivery::Immediate, CODE);
    let snap = desk.snap();
    let code = desk.token(CODE, 2);
    assert_eq!(snap.focus.window, Some(code));
    assert_eq!(desk.visible_workspace(), "coding");
    assert!(desk.shown[CODE] && !desk.shown[GHOSTTY] && !desk.shown[SPOTIFY]);
    assert!(desk.written(code), "the visible workspace was laid out");
}

#[test]
fn cmd_tab_between_apps_on_different_workspaces_switches_and_hides() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    assert_eq!(desk.snap().focus.window, Some(desk.token(GHOSTTY, 1)));
    desk.cmd_tab(SPOTIFY);
    assert_eq!(desk.visible_workspace(), "music");
    assert_eq!(desk.snap().focus.window, Some(desk.token(SPOTIFY, 3)));
    assert!(!desk.shown[GHOSTTY]);
    desk.cmd_tab(CODE);
    assert_eq!(desk.visible_workspace(), "coding");
    assert!(!desk.shown[SPOTIFY] && desk.shown[CODE]);
    desk.cmd_tab(CHROME);
    assert_eq!(desk.visible_workspace(), "browser");
    assert_eq!(
        desk.workspace_of(desk.token(CHROME, 5)),
        "browser",
        "no secondary display: main stands in"
    );
}

#[test]
fn focus_moves_between_windows_of_one_app() {
    let mut desk = Desktop::boot(Delivery::Immediate, CODE);
    let first = desk.token(CODE, 2);
    let second = desk.window_with(
        CODE,
        "other",
        "AXStandardWindow",
        (900.0, 700.0),
        SnapshotOrigin::Created,
    );
    desk.rig.push(WindowEventKind::Window {
        window: second,
        change: WindowChange::Focused,
    });
    desk.settle();
    assert_eq!(desk.snap().focus.window, Some(second));
    desk.rig.push(WindowEventKind::Window {
        window: first,
        change: WindowChange::Focused,
    });
    desk.settle();
    assert_eq!(desk.snap().focus.window, Some(first));
    assert_eq!(desk.visible_workspace(), "coding");
}

// ------------------------------------------------------ bug 5: eligibility

#[test]
fn agents_tiny_utility_windows_and_popups_are_never_tracked() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    // The agent's 1920x30 strip: its app is not a regular one, so it is never
    // listed, tracked or given a workspace.
    let strip = desk.window_with(
        MENUBAR,
        "",
        "AXStandardWindow",
        (1920.0, 30.0),
        SnapshotOrigin::Created,
    );
    let snap = desk.snap();
    assert!(snap.window(strip).is_none());
    assert!(!snap.apps.iter().any(|app| app.info.bundle_id == "com.apple.MenuBarAgent"));
    assert!(
        !snap
            .apps
            .iter()
            .any(|app| app.info.bundle_id == "pro.betterdisplay.BetterDisplay"
                && !snap.windows.is_empty()
                && snap.windows.iter().any(|w| w.application == app.info.application)),
        "BetterDisplay's 1x1 window is not tracked"
    );
    // Only the real windows are tracked, and the terminal workspace has one.
    let tracked: Vec<_> = snap
        .windows
        .iter()
        .map(|w| {
            snap.apps
                .iter()
                .find(|a| a.info.application == w.application)
                .expect("app")
                .info
                .name
                .clone()
        })
        .collect();
    assert_eq!(tracked.len(), 5, "{tracked:?}");
    let terminal = snap.workspace("terminal").expect("terminal");
    assert_eq!(
        terminal.order.len(),
        1,
        "one Ghostty window, no ghosts: {:?}",
        terminal.order
    );
    // A small dialog and the Finder info window are excluded with a reason.
    let status = crate::ipc::status(&snap);
    let reasons: Vec<&str> = status.excluded_windows.iter().map(|w| w.reason.as_str()).collect();
    assert!(reasons.contains(&"window below 200x150"), "{reasons:?}");
    assert!(reasons.contains(&"finder info window"), "{reasons:?}");
    assert!(
        reasons.contains(&"sheet, drawer or unknown-subrole panel"),
        "{reasons:?}"
    );
    let dialog = desk.window_with(
        GHOSTTY,
        "alert",
        "AXDialog",
        (300.0, 200.0),
        SnapshotOrigin::Created,
    );
    assert!(
        desk.snap().window(dialog).is_none(),
        "a small dialog is not tiled"
    );
    let preferences = desk.window_with(
        GHOSTTY,
        "settings",
        "AXDialog",
        (700.0, 500.0),
        SnapshotOrigin::Created,
    );
    assert!(desk.snap().window(preferences).is_some(), "a large dialog is");
    // The status answer lists every tracked window with its facts.
    let status = crate::ipc::status(&desk.snap());
    assert!(status.tracked_windows.iter().all(|w| w.subrole.is_some() && w.frame.is_some()));
    assert!(
        status
            .tracked_windows
            .iter()
            .any(|w| w.app_name == "Ghostty" && w.workspace.as_deref() == Some("terminal"))
    );
}

#[test]
fn the_status_answer_explains_a_failed_app_and_the_phase() {
    let desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    let status = crate::ipc::status(&desk.snap());
    assert_eq!(status.phase, "complete");
    assert!(status.faults.is_empty());
    assert!(status.layout_active);
    assert_eq!(status.failed_apps.len(), 1);
    assert_eq!(status.failed_apps[0].bundle_id, "us.zoom.xos");
    assert!(status.failed_apps[0].reason.contains("Unacknowledged"));
    assert!(
        status.hidden_apps.iter().any(|a| a.bundle_id == "com.spotify.client"),
        "{:?}",
        status.hidden_apps
    );
    let table = crate::cli::render::status(&status, false);
    assert!(table.contains("failed app: zoom.us"), "{table}");
    assert!(table.contains("phase: complete"), "{table}");
}

// ----------------------------------------- the running runtime's app watches

/// The real runtime (actor and effect threads) with the application actions:
/// hide, unhide, launch and Cmd-Tab reach the actor through the watches the
/// module installs, not through hand-made inputs.
#[test]
fn the_runtime_watches_application_lifecycle_and_the_frontmost_application() {
    let native = Arc::new(Native::new(Duration::from_millis(500)));
    let (actions, pairs) = native.install(&["com.a", "com.b"]);
    let (ids, keys): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
    native.backend.set_name(keys[0], Some("A"));
    native.backend.set_name(keys[1], Some("B"));
    let actions = Arc::new(actions);
    let backend = Arc::new(FakeWindowBackend::default());
    let mut options = TrackingOptions::new("");
    options.actions = Some(Arc::clone(&actions));
    options.defer_begin = true;
    let runtime = TrackingRuntime::start(
        Arc::clone(&backend) as Arc<dyn WindowBackend>,
        Arc::new(ActionsDirectory::new(actions)),
        &options,
    )
    .expect("start");
    let mut feed = Feed::new(runtime.epoch().value());
    feed.adapter = backend.adapter();
    runtime.submit_displays(None, one_display()).expect("displays");
    runtime.begin().expect("begin");
    runtime.watch_applications().expect("both watches install");
    let windows: Vec<_> = ids
        .iter()
        .enumerate()
        .map(|(n, id)| WindowInstance::for_testing(feed.adapter, feed.epoch, *id, n as u64 + 1))
        .collect();
    wait_until("tracked", || {
        backend
            .calls()
            .iter()
            .filter(|c| matches!(c, FakeBackendCall::Track(..)))
            .count()
            == 2
    });
    for (n, window) in windows.iter().enumerate() {
        let sink = backend.sink().expect("sink");
        assert!(sink.push(feed.event(WindowEventKind::Snapshot {
            snapshot: snapshot(*window, "w", 10 + n as u32),
            origin: SnapshotOrigin::Enumeration,
        })));
    }
    for id in &ids {
        assert!(backend.sink().expect("sink").push(feed.event(
            WindowEventKind::EnumerationComplete {
                application: *id,
                windows: 1,
                truncated: false,
            }
        )));
    }
    // Cmd-Tab to A, then to B: the focus follows the activation.
    assert!(
        native.backend.activate(keys[0]) > 0,
        "the activation watch is installed"
    );
    wait_until("A focused", || {
        runtime.snapshot().focus.window == Some(windows[0])
    });
    assert!(native.backend.activate(keys[1]) > 0);
    wait_until("B focused", || {
        runtime.snapshot().focus.window == Some(windows[1])
    });
    // The system hides A: the actor learns it without any window event.
    assert!(
        native.backend.deliver_lifecycle(LifecycleKind::Hidden, keys[0]) > 0,
        "lifecycle watch"
    );
    wait_until("A hidden", || {
        runtime.snapshot().apps.iter().any(|a| a.info.application == ids[0] && a.hidden)
    });
    runtime.shutdown();
}

// ------------------------------------------- switching back to a split workspace

const HUB_SERIAL: u64 = 6;

/// Boots the terminal split (Ghostty master 75 + Device Hub) with animations on.
fn split_desktop(delivery: Delivery) -> (Desktop, WindowInstance, WindowInstance) {
    let mut desk = Desktop::boot_with(delivery, GHOSTTY, true, true);
    desk.release_late();
    let ghostty = desk.token(GHOSTTY, 1);
    let hub = desk.token(DEVICE_HUB, HUB_SERIAL);
    assert_eq!(desk.visible_workspace(), "terminal");
    let (master, stack) = (desk.desired_of(ghostty), desk.desired_of(hub));
    assert!(
        master.width > stack.width * 2.0,
        "75/25 master split: {master:?} {stack:?}"
    );
    (desk, ghostty, hub)
}

/// What a switch back to the split workspace must (not) do.
fn assert_clean_switch_back(
    desk: &Desktop,
    since: usize,
    ghostty: WindowInstance,
    hub: WindowInstance,
    full: f64,
) {
    for write in desk.writes_since(since) {
        assert!(!write.animate, "a switch never animates: {write:?}");
        if write.window != ghostty && write.window != hub {
            continue;
        }
        assert!(
            write.window != ghostty,
            "Ghostty already sits at its slot, it is never rewritten: {write:?}"
        );
        assert!(
            write.target.width < full,
            "no full-width intermediate target: {write:?}"
        );
    }
    assert!(
        desk.writes_since(since).iter().filter(|write| write.window == hub).count() <= 1,
        "Device Hub is written at most once: {:?}",
        desk.writes_since(since)
    );
    for window in [ghostty, hub] {
        let (want, got) = (desk.desired_of(window), desk.actual_of(window));
        assert!(
            (want.x - got.x).abs() < 1.5
                && (want.width - got.width).abs() < 1.5
                && (want.height - got.height).abs() < 1.5,
            "final frame is the master split: want {want:?} got {got:?}"
        );
    }
}

#[test]
fn startup_writes_are_never_animated() {
    let desk = Desktop::boot_with(Delivery::Late, GHOSTTY, true, true);
    let writes = desk.writes_since(0);
    assert!(!writes.is_empty());
    assert!(writes.iter().all(|write| !write.animate), "{writes:?}");
}

#[test]
fn switching_back_by_intent_keeps_the_slot_of_the_app_still_being_unhidden() {
    for delivery in [Delivery::Late, Delivery::Immediate, Delivery::Twice] {
        let (mut desk, ghostty, hub) = split_desktop(delivery);
        let full = desk.desired_of(ghostty).width + desk.desired_of(hub).width;
        for round in 0..3 {
            desk.focus_workspace("music");
            assert_eq!(desk.visible_workspace(), "music", "{delivery:?} {round}");
            let since = desk.mark();
            // `Late`: Device Hub still reads hidden while the layout is planned.
            desk.focus_workspace("terminal");
            assert_eq!(desk.visible_workspace(), "terminal");
            desk.release_late();
            assert_clean_switch_back(&desk, since, ghostty, hub, full);
        }
    }
}

#[test]
fn switching_back_by_native_focus_keeps_the_slot_of_the_app_still_being_unhidden() {
    for delivery in [Delivery::Late, Delivery::Immediate] {
        let (mut desk, ghostty, hub) = split_desktop(delivery);
        let full = desk.desired_of(ghostty).width + desk.desired_of(hub).width;
        for _ in 0..3 {
            desk.cmd_tab(SPOTIFY);
            assert_eq!(desk.visible_workspace(), "music");
            let since = desk.mark();
            // Cmd-Tab shows Ghostty only; Device Hub's unhide lands later.
            desk.cmd_tab(GHOSTTY);
            assert_eq!(desk.visible_workspace(), "terminal");
            desk.release_late();
            assert_clean_switch_back(&desk, since, ghostty, hub, full);
        }
    }
}

#[test]
fn the_window_unhidden_late_is_written_directly_not_animated() {
    let (mut desk, ghostty, hub) = split_desktop(Delivery::Late);
    // Move Device Hub away from its slot while its workspace is hidden: it will
    // have to be written once it reads visible again.
    desk.focus_workspace("music");
    desk.release_late();
    desk.rig.backend.script_echo_frames(true);
    let stale = Rect::new(300.0, 300.0, 500.0, 400.0);
    desk.rig.push(WindowEventKind::Snapshot {
        snapshot: {
            let mut moved = desk.snapshot_of(hub, "Device Hub", "AXStandardWindow", (500.0, 400.0));
            moved.frame = FrameRead {
                position: AxReading::Value(AxPoint { x: stale.x, y: stale.y }),
                size: AxReading::Value(AxSize {
                    width: stale.width,
                    height: stale.height,
                }),
            };
            moved
        },
        origin: SnapshotOrigin::Created,
    });
    desk.settle();
    let since = desk.mark();
    desk.focus_workspace("terminal");
    desk.release_late();
    let writes = desk.writes_since(since);
    assert!(writes.iter().any(|write| write.window == hub), "{writes:?}");
    assert!(writes.iter().all(|write| !write.animate), "{writes:?}");
    assert!(writes.iter().all(|write| write.window != ghostty), "{writes:?}");
}

#[test]
fn an_externally_hidden_app_on_the_visible_workspace_is_excluded_and_never_unhidden() {
    let mut desk = Desktop::boot_with(Delivery::Immediate, GHOSTTY, true, true);
    let ghostty = desk.token(GHOSTTY, 1);
    let hub = desk.token(DEVICE_HUB, HUB_SERIAL);
    let split = desk.desired_of(ghostty).width;
    let requests = desk.native.backend.visibility_requests().len();
    // The user hides Device Hub: Stache never owned that hide.
    desk.shown[DEVICE_HUB] = false;
    desk.notify(DEVICE_HUB, LifecycleKind::Hidden);
    desk.settle();
    assert!(
        desk.desired_of(ghostty).width > split * 1.2,
        "Ghostty takes the whole area: {:?}",
        desk.desired_of(ghostty)
    );
    assert_eq!(
        desk.native.backend.visibility_requests().len(),
        requests,
        "nothing was hidden or unhidden by Stache"
    );
    // Further relayouts keep it that way.
    desk.submit(Input::Relayout);
    desk.settle();
    assert_eq!(desk.native.backend.visibility_requests().len(), requests);
    assert!(
        desk.writes_since(0)
            .iter()
            .all(|write| write.window != hub || write.target.width < split * 1.2)
    );
    // The user shows it again: it rejoins the split.
    desk.shown[DEVICE_HUB] = true;
    desk.notify(DEVICE_HUB, LifecycleKind::Unhidden);
    desk.settle();
    assert!((desk.desired_of(ghostty).width - split).abs() < 1.0);
}

#[test]
fn layout_changes_inside_a_visible_workspace_still_animate_when_enabled() {
    let (mut desk, ghostty, hub) = split_desktop(Delivery::Immediate);
    let since = desk.mark();
    desk.submit(Input::Intent(Intent::SetLayout(LayoutType::Monocle)));
    desk.settle();
    let writes = desk.writes_since(since);
    assert!(
        writes.iter().any(|write| write.window == hub && write.animate),
        "{writes:?}"
    );
    assert!(
        writes.iter().all(|write| write.window == ghostty || write.window == hub),
        "{writes:?}"
    );
}

#[test]
fn nothing_animates_with_animations_disabled() {
    let mut desk = Desktop::boot_with(Delivery::Immediate, GHOSTTY, true, false);
    desk.submit(Input::Intent(Intent::SetLayout(LayoutType::Monocle)));
    desk.settle();
    assert!(desk.writes_since(0).iter().all(|write| !write.animate));
}

// ------------------------------------------- review follow-ups: focus and listing

#[test]
fn hiding_the_frontmost_app_does_not_ping_pong_the_workspace() {
    for delivery in [
        Delivery::Immediate,
        Delivery::Twice,
        Delivery::Late,
        Delivery::Never,
    ] {
        let mut desk = Desktop::boot_with(delivery, GHOSTTY, true, false);
        desk.release_late();
        // The system activates Device Hub when Ghostty is hidden, although Device
        // Hub is on the workspace being left and is being hidden too.
        desk.successor = Some(DEVICE_HUB);
        desk.focus_workspace("music");
        desk.release_late();
        assert_eq!(
            desk.visible_workspace(),
            "music",
            "{delivery:?}: CLI switch settles"
        );
        assert!(desk.shown[SPOTIFY] && !desk.shown[GHOSTTY], "{delivery:?}");
        let requests = desk.native.backend.visibility_requests().len();
        assert!(
            requests <= 8,
            "{delivery:?}: {requests} requests, no oscillation"
        );
        // A native switch back settles on the intended workspace as well.
        desk.frontmost = SPOTIFY;
        desk.cmd_tab(GHOSTTY);
        desk.release_late();
        assert_eq!(
            desk.visible_workspace(),
            "terminal",
            "{delivery:?}: Cmd-Tab settles"
        );
        assert!(desk.shown[GHOSTTY] && !desk.shown[SPOTIFY], "{delivery:?}");
    }
}

#[test]
fn cmd_tab_to_an_app_with_windows_on_two_workspaces_follows_its_real_focused_window() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    // Code has a window on "coding" (enumerated) and one the user's rules send
    // elsewhere is not available, so move a second Code window by intent: a
    // window created later lands on the focused workspace.
    let coding = desk.token(CODE, 2);
    let second = desk.window_with(
        CODE,
        "second",
        "AXStandardWindow",
        (900.0, 700.0),
        SnapshotOrigin::Created,
    );
    desk.settle();
    let first_workspace = desk.workspace_of(coding);
    let second_workspace = desk.workspace_of(second);
    // Both belong to Code's rule workspace; force a second workspace by moving
    // one window explicitly.
    assert_eq!(first_workspace, second_workspace);
    desk.submit(Input::Intent(Intent::SendWindowToWorkspace {
        window: Some(second),
        workspace: "misc".to_owned(),
    }));
    desk.settle();
    assert_eq!(desk.workspace_of(second), "misc");
    assert_eq!(desk.visible_workspace(), "terminal");
    // The history says "coding", but the application's real focus is the other.
    desk.app_focuses(CODE, second);
    desk.cmd_tab(CODE);
    assert_eq!(desk.snap().focus.window, Some(second));
    assert_eq!(desk.visible_workspace(), "misc");
}

#[test]
fn activation_never_switches_a_workspace_on_a_guess() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    // The system cannot say which window of Code is focused.
    desk.rig.backend.script_unfocused(desk.ids[CODE]);
    desk.cmd_tab(CODE);
    assert_eq!(
        desk.visible_workspace(),
        "terminal",
        "an unresolved read leaves the focus"
    );
    assert!(
        desk.rig.calls().iter().any(
            |call| matches!(call, FakeBackendCall::FocusedWindow(app) if *app == desk.ids[CODE])
        ),
        "the real focused window was asked for"
    );
}

#[test]
fn an_application_missed_at_listing_is_tracked_after_cmd_tab_to_it() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    // MenuBarAgent was not regular at listing time, so it was never listed.
    assert!(desk.snap().apps.iter().all(|app| app.info.bundle_id != APPS[MENUBAR].0));
    desk.native.backend.set_regular(desk.keys[MENUBAR], true);
    desk.cmd_tab(MENUBAR);
    desk.settle();
    assert!(
        desk.snap().apps.iter().any(|app| app.info.bundle_id == APPS[MENUBAR].0),
        "the activation asked for a new list and the app is tracked now"
    );
    // A permanently unlisted application asks only once.
    let lists = |desk: &Desktop| desk.lists.load(std::sync::atomic::Ordering::SeqCst);
    let before = lists(&desk);
    desk.native.backend.set_regular(desk.keys[MENUBAR], false);
    for _ in 0..3 {
        desk.cmd_tab(GHOSTTY);
        desk.cmd_tab(MENUBAR);
    }
    assert!(
        lists(&desk) <= before + 1,
        "bounded: {} -> {}",
        before,
        lists(&desk)
    );
}

#[test]
fn a_lost_hidden_notice_resolves_on_the_actor_clock_on_an_idle_desktop() {
    let mut desk = Desktop::boot(Delivery::Never, GHOSTTY);
    desk.focus_workspace("music");
    let requests = |desk: &Desktop| desk.native.backend.visibility_requests().len();
    let sent = requests(&desk);
    // No turns pass the time: an idle desktop never re-sends on turns alone.
    for _ in 0..100 {
        desk.rig.engine.turn();
    }
    assert_eq!(requests(&desk), sent, "waiting is time, not actor turns");
    // The actor asks to be woken for the confirmation deadline.
    let due = desk.rig.engine.turn().next_deadline.expect("a wake for the lost notice");
    let now = crate::actor::Clock::now(&*desk.rig.clock);
    assert!(
        due > now && due <= now + crate::visibility::CONFIRMATION_TIMEOUT,
        "{due:?}"
    );
    desk.rig.clock.advance(crate::visibility::CONFIRMATION_TIMEOUT);
    desk.settle();
    assert!(
        requests(&desk) > sent,
        "the lost notice was resolved by re-sending the hide"
    );
    // Bounded: it does not wake or re-send forever.
    desk.rig.clock.advance(Duration::from_secs(60));
    desk.settle();
    assert!(requests(&desk) <= sent + 3 * 4);
}

/// The real runtime: an application launched between the first list and the
/// moment the lifecycle watch is live is picked up by the list the watch asks for.
#[test]
fn a_launch_during_the_watch_install_gap_is_tracked() {
    let native = Arc::new(Native::new(Duration::from_millis(500)));
    let (actions, pairs) = native.install(&["com.a", "com.b"]);
    let (ids, keys): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
    native.backend.set_name(keys[0], Some("A"));
    native.backend.set_name(keys[1], Some("B"));
    // B is still launching when the first list is taken.
    native.backend.set_regular(keys[1], false);
    let actions = Arc::new(actions);
    let backend = Arc::new(FakeWindowBackend::default());
    let mut options = TrackingOptions::new("");
    options.actions = Some(Arc::clone(&actions));
    options.defer_begin = true;
    let runtime = TrackingRuntime::start(
        Arc::clone(&backend) as Arc<dyn WindowBackend>,
        Arc::new(ActionsDirectory::new(actions)),
        &options,
    )
    .expect("start");
    runtime.submit_displays(None, one_display()).expect("displays");
    runtime.begin().expect("begin");
    let tracked = |application| {
        backend
            .calls()
            .iter()
            .any(|c| matches!(c, FakeBackendCall::Track(app, _) if *app == application))
    };
    wait_until("A tracked from the first list", || tracked(ids[0]));
    // B finishes launching inside the gap: no launch event reaches the runtime.
    native.backend.set_regular(keys[1], true);
    runtime.watch_applications().expect("watches");
    wait_until("B tracked from the list the watch asked for", || tracked(ids[1]));
    runtime.shutdown();
}

// ------------------------------------------- new window focus and swap

/// How the system tells Tiling that a new window took the focus.
#[derive(Clone, Copy, Debug)]
enum FocusNotice {
    /// The window-level notification arrives before the creation.
    BeforeAdoption,
    /// The window-level notification arrives after the creation.
    AfterAdoption,
    /// Only the application-level notification arrives, after the creation.
    ApplicationOnly,
    /// No notification at all: only the application's focused window can tell.
    None,
}

/// The user's Finder: its `files` grid workspace holds one window, focused.
fn finder_with_one_window() -> (Desktop, WindowInstance) {
    let mut desk = Desktop::boot(Delivery::Immediate, FINDER);
    let first = desk.window_with(
        FINDER,
        "Documents",
        "AXStandardWindow",
        (1200.0, 800.0),
        SnapshotOrigin::Created,
    );
    desk.app_focuses(FINDER, first);
    desk.rig.push(WindowEventKind::Window {
        window: first,
        change: WindowChange::Focused,
    });
    desk.settle();
    assert_eq!(desk.visible_workspace(), "files");
    assert_eq!(desk.snap().focus.window, Some(first));
    (desk, first)
}

/// Cmd-N: a second Finder window appears, macOS focuses it.
fn finder_new_window(desk: &mut Desktop, notice: FocusNotice) -> WindowInstance {
    let second = desk.token(FINDER, desk.serial + 1);
    desk.app_focuses(FINDER, second);
    let focused = WindowEventKind::Window {
        window: second,
        change: WindowChange::Focused,
    };
    if matches!(notice, FocusNotice::BeforeAdoption) {
        // The owner knows the window it minted, so a read of it answers.
        desk.serial += 1;
        let read = desk.snapshot_of(second, "Downloads", "AXStandardWindow", (1200.0, 800.0));
        desk.serial -= 1;
        desk.rig.backend.script_snapshot(
            second,
            stache_macos::windows::SnapshotOutcome::Snapshot(Box::new(read)),
        );
        desk.rig.push(focused.clone());
    }
    let created = desk.window_with(
        FINDER,
        "Downloads",
        "AXStandardWindow",
        (1200.0, 800.0),
        SnapshotOrigin::Created,
    );
    assert_eq!(created, second);
    match notice {
        FocusNotice::AfterAdoption => {
            desk.rig.push(focused);
        }
        FocusNotice::ApplicationOnly => {
            desk.rig.push(WindowEventKind::Application {
                application: desk.ids[FINDER],
                change: stache_macos::windows::ApplicationChange::FocusedWindowChanged,
            });
        }
        FocusNotice::BeforeAdoption | FocusNotice::None => {}
    }
    desk.settle();
    second
}

/// One user swap. The fake owner answers a position-only write (equal sizes)
/// as stale, so the windows report their new frames the way the system would.
fn swap(desk: &mut Desktop, direction: SwapDirection) {
    desk.submit(Input::Intent(Intent::SwapWindow(direction)));
    desk.settle();
    for window in finder_order(desk) {
        let snap = desk.snap();
        let held = snap.window(window).expect("tracked");
        let target = desk.desired_of(window);
        let mut read = desk.snapshot_of(window, "Finder", "AXStandardWindow", (1.0, 1.0));
        read.cg_window_id = held.cg_window_id;
        read.frame = FrameRead {
            position: AxReading::Value(AxPoint { x: target.x, y: target.y }),
            size: AxReading::Value(AxSize {
                width: target.width,
                height: target.height,
            }),
        };
        desk.rig.backend.script_snapshot(
            window,
            stache_macos::windows::SnapshotOutcome::Snapshot(Box::new(read)),
        );
        desk.rig.push(WindowEventKind::Window {
            window,
            change: WindowChange::Moved,
        });
    }
    desk.rig.clock.advance(Duration::from_secs(1));
    desk.settle();
}

fn finder_order(desk: &Desktop) -> Vec<WindowInstance> {
    let snap = desk.snap();
    snap.workspaces
        .iter()
        .find(|ws| ws.name == "files")
        .expect("files")
        .order
        .clone()
}

#[test]
fn a_new_finder_window_takes_the_focus_whichever_way_it_is_reported() {
    for notice in [
        FocusNotice::BeforeAdoption,
        FocusNotice::AfterAdoption,
        FocusNotice::ApplicationOnly,
        FocusNotice::None,
    ] {
        let (mut desk, first) = finder_with_one_window();
        let second = finder_new_window(&mut desk, notice);
        assert_eq!(desk.snap().focus.window, Some(second), "{notice:?}");
        assert_eq!(finder_order(&desk), vec![first, second], "{notice:?}");
        assert!(
            desk.desired_of(first).x < desk.desired_of(second).x,
            "{notice:?}"
        );
        // Swap left moves the NEW window (the focused one) to the left.
        swap(&mut desk, SwapDirection::Left);
        assert_eq!(finder_order(&desk), vec![second, first], "{notice:?}");
        assert!(
            desk.desired_of(second).x < desk.desired_of(first).x,
            "{notice:?}"
        );
        assert_eq!(desk.snap().focus.window, Some(second), "{notice:?}");
        // Now leftmost: another swap left changes nothing (legacy), right swaps back.
        swap(&mut desk, SwapDirection::Left);
        assert_eq!(finder_order(&desk), vec![second, first], "{notice:?}");
        swap(&mut desk, SwapDirection::Right);
        assert_eq!(finder_order(&desk), vec![first, second], "{notice:?}");
    }
}

#[test]
fn a_focus_for_a_window_that_is_never_adopted_is_dropped() {
    let (mut desk, first) = finder_with_one_window();
    let ghost = desk.token(FINDER, 999);
    desk.rig.push(WindowEventKind::Window {
        window: ghost,
        change: WindowChange::Focused,
    });
    desk.settle();
    assert_eq!(desk.snap().focus.window, Some(first));
    assert_eq!(finder_order(&desk), vec![first]);
}

// ------------------------------------------------ startup keeps the user's focus

/// The window focused when Stache starts stays focused: Stache never hides the
/// frontmost application while it works out which workspace is visible (the
/// system would activate another application, and an unhide never gives the
/// focus back), in whatever order the startup inputs arrive.
#[test]
fn startup_never_takes_the_focus_from_the_frontmost_app() {
    let workspaces = [
        (GHOSTTY, "terminal", 1),
        (CODE, "coding", 2),
        (SPOTIFY, "music", 3),
        (CHROME, "browser", 5),
    ];
    for order in [
        Order::Listed,
        Order::DisplaysLast,
        Order::WritesLast,
        Order::ActivationLast,
    ] {
        for (front, workspace, serial) in workspaces {
            let desk = Desktop::boot_ordered(Delivery::Immediate, front, true, false, Some(order));
            let case = format!("{order:?}, {} frontmost", APPS[front].1);
            let hidden =
                desk.native.backend.visibility_requests().iter().any(|(key, operation)| {
                    *operation == VisibilityOperation::Hide && *key == desk.keys[front]
                });
            assert!(!hidden, "{case}: the frontmost application was hidden");
            assert_eq!(
                desk.frontmost, front,
                "{case}: the system activated another application"
            );
            assert!(desk.shown[front], "{case}");
            assert_eq!(desk.visible_workspace(), workspace, "{case}");
            assert_eq!(
                desk.snap().focus.window,
                Some(desk.token(front, serial)),
                "{case}"
            );
            assert!(desk.written(desk.token(front, serial)), "{case}: laid out");
            // The other workspaces are still hidden once the focus is decided.
            let other = if front == SPOTIFY { CODE } else { SPOTIFY };
            assert!(!desk.shown[other], "{case}: {} is hidden", APPS[other].1);
        }
    }
}

#[test]
fn startup_focuses_the_window_the_frontmost_app_reports_not_the_first_listed() {
    let desk = Desktop::boot_ordered(
        Delivery::Immediate,
        CODE,
        false,
        false,
        Some(Order::SecondWindowFocused),
    );
    let second = desk.token(CODE, 6);
    assert_eq!(desk.workspace_of(second), "coding");
    assert_eq!(desk.snap().focus.window, Some(second));
    assert_eq!(desk.frontmost, CODE);
}
