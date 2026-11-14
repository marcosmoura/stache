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

pub(super) const APPS: [(&str, &str); 10] = [
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

std::thread_local! {
    /// Adjusts the next desktop before it starts (its native starting state).
    static BOOT_HOOK: std::cell::Cell<Option<fn(&mut Desktop)>> =
        const { std::cell::Cell::new(None) };
}

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

pub(super) fn user_config(animations: bool) -> Arc<TilingConfig> {
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
    /// Native focus requests queued (`Effect::Focus`).
    focuses: Arc<Mutex<Vec<WindowInstance>>>,
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
        if let Effect::Focus { window, .. } = effect {
            self.focuses.lock().expect("focuses").push(window);
        }
        if matches!(effect, Effect::ListApplications { .. }) {
            self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        self.inner.submit(effect)
    }

    fn close(&self) { self.inner.close(); }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent opt-in switches of the fake desktop"
)]
struct Desktop {
    writes: Arc<Mutex<Vec<Write>>>,
    focuses: Arc<Mutex<Vec<WindowInstance>>>,
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
    /// macOS as measured: a hide is not applied while it would leave no
    /// visible regular application (a windowless shown Finder counts), and
    /// hiding the frontmost application activates a remaining visible one.
    last_app_kept: bool,
    /// Running regular applications without any window that the fake system
    /// counts as visible while shown (with [`Self::last_app_kept`]).
    bare: Vec<usize>,
    /// The visibility control the runner and the actor share.
    vis: Arc<VisibilityControl>,
    /// The application actions the runner executes with.
    actions: Arc<crate::visibility::ApplicationActions>,
    /// Exact activation requests already turned into activations.
    activations_seen: usize,
    /// The fake window server answers bounds for every window by its id.
    bounds: bool,
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
        let focuses = Arc::new(Mutex::new(Vec::new()));
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
            actions: Some(Arc::clone(&actions)),
            animation: Arc::clone(&rig.animation),
            borders: None,
        };
        let mut engine = Engine::new(
            TilingState::new("com.stache.test"),
            Arc::clone(&rig.ingress),
            Arc::clone(&rig.cell),
            Arc::new(RecordingRunner {
                inner: InlineRunner::new(env),
                writes: Arc::clone(&writes),
                lists: Arc::clone(&lists),
                focuses: Arc::clone(&focuses),
            }),
            Arc::clone(&rig.sink),
            Arc::clone(&rig.clock) as Arc<dyn crate::actor::Clock>,
        );
        engine.set_frame_control(Arc::clone(&rig.frames));
        engine.set_visibility_control(Arc::clone(&vis));
        rig.engine = engine;
        let mut desktop = Self {
            writes,
            focuses,
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
            last_app_kept: false,
            bare: Vec::new(),
            vis: Arc::clone(&vis),
            actions,
            activations_seen: 0,
            bounds: false,
        };
        if let Some(hook) = BOOT_HOOK.take() {
            hook(&mut desktop);
        }
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
        if self.bounds
            && let Some(cg) = snapshot.cg_window_id
        {
            self.rig.backend.script_bounds(cg, token);
        }
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
                // An activation Stache requested with no hide or unhide (a
                // desktop host) still takes effect.
                if self.last_app_kept && self.follow_activations() {
                    continue;
                }
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
                if self.last_app_kept && operation == VisibilityOperation::Hide {
                    let Some(mut next) = self.other_visible(index) else {
                        // Accepted, never applied: it is the last visible one.
                        continue;
                    };
                    if index == self.frontmost
                        && self.bare.contains(&next)
                        && let Some(hidden) = self.hidden_with_windows(index)
                    {
                        // Measured: with only a windowless app left that is not
                        // frontmost, macOS shows and activates a hidden app
                        // with windows instead.
                        self.shown[hidden] = true;
                        self.notify(hidden, LifecycleKind::Unhidden);
                        next = hidden;
                    }
                    if index == self.frontmost {
                        self.frontmost = next;
                        self.submit(Input::Activated(self.ids[next]));
                    }
                }
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
            // Activations take effect after the hides sent with them: a hide
            // sent before its host's activation is seen meets the old
            // frontmost application.
            if self.last_app_kept {
                self.follow_activations();
            }
        }
    }

    /// Every activation Stache requested makes that application frontmost,
    /// and the system reports it.
    fn follow_activations(&mut self) -> bool {
        let exact = self.native.backend.exact_activation_requests();
        let fresh = exact.len() > self.activations_seen;
        for key in exact[self.activations_seen..].iter().copied() {
            let index = self.keys.iter().position(|k| *k == key).expect("known app");
            self.frontmost = index;
            self.submit(Input::Activated(self.ids[index]));
        }
        self.activations_seen = exact.len();
        fresh
    }

    /// A visible regular application other than `app`, Finder first: an app
    /// with a shown tracked window, or a shown Finder even without a window.
    /// (The agents, Zoom and a windowless Device Hub stand for the user's
    /// hidden applications.)
    fn other_visible(&self, app: usize) -> Option<usize> {
        if app != FINDER && self.shown[FINDER] {
            return Some(FINDER);
        }
        managed_shown(self)
            .iter()
            .filter_map(|name| APPS.iter().position(|(_, n)| n == name))
            .find(|index| *index != app)
            .or_else(|| self.bare.iter().copied().find(|bare| *bare != app && self.shown[*bare]))
    }

    /// A hidden application other than `app` with a tracked window.
    fn hidden_with_windows(&self, app: usize) -> Option<usize> {
        let snap = self.snap();
        snap.windows
            .iter()
            .filter_map(|window| self.ids.iter().position(|id| *id == window.application))
            .find(|index| *index != app && !self.shown[*index])
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

    /// `stache tiling window --send-to-workspace NAME` for an exact window.
    fn send_window(&mut self, window: WindowInstance, workspace: &str) {
        self.release_late();
        self.submit(Input::Intent(Intent::SendWindowToWorkspace {
            window: Some(window),
            workspace: workspace.to_owned(),
        }));
        self.settle();
    }

    /// Empties the focused `terminal` (Ghostty and Device Hub) by sending
    /// Device Hub's window to `misc`, then Ghostty's to `target`: an empty
    /// workspace is inactive when nothing can host the desktop, so the
    /// kept-last state is reached by this route instead of a switch.
    fn empty_terminal_into(&mut self, target: &str) {
        let hub = self
            .snap()
            .windows
            .iter()
            .find(|window| window.application == self.ids[DEVICE_HUB])
            .map(|window| window.token)
            .expect("a Device Hub window");
        self.send_window(hub, "misc");
        self.send_window(self.token(GHOSTTY, 1), target);
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

/// Stache's own focus of Code's window on `coding` runs on the effects thread
/// of a real [`crate::ThreadRunner`]: it activates Code, then raises the
/// window. Code's activation queues a focus read; until the raise, the system
/// still reports Code's window on `misc`. The read waits for the raise, reports
/// the raised window and the switch stays; the window it would have read
/// earlier would have switched the workspace back.
#[test]
#[allow(clippy::too_many_lines, reason = "one linear scenario")]
fn a_focus_read_after_stache_s_own_focus_reads_the_raised_window_and_the_switch_stays() {
    use stache_macos::windows::FocusOutcome;

    use crate::state::CompletionResult;
    let mut desk = Desktop::boot(Delivery::Immediate, CODE);
    let code = desk.token(CODE, 2);
    let notes = desk.window_with(
        CODE,
        "notes",
        "AXStandardWindow",
        (1200.0, 800.0),
        SnapshotOrigin::Created,
    );
    desk.send_window(notes, "misc");
    desk.focus_workspace("misc");
    desk.focus_workspace("coding");
    desk.submit(Input::Activated(desk.ids[CODE]));
    desk.settle();
    assert_eq!(desk.visible_workspace(), "coding");
    assert_eq!(desk.snap().focus.window, Some(code));

    // Before the raise lands, the system reports the window on `misc`.
    desk.app_focuses(CODE, notes);
    desk.rig.backend.script_raise_focuses(true);
    let ingress = Arc::new(Ingress::new(Pipeline::new()));
    let env = EffectEnv {
        backend: Arc::clone(&desk.rig.backend) as Arc<dyn WindowBackend>,
        directory: Arc::new(FakeAppDirectory::default()),
        ingress: Arc::clone(&ingress),
        cancel: CancellationToken::new(),
        frames: Arc::clone(&desk.rig.frames),
        visibility: Arc::clone(&desk.vis),
        actions: Some(Arc::clone(&desk.actions)),
        animation: Arc::clone(&desk.rig.animation),
        borders: None,
    };
    let runner = crate::ThreadRunner::spawn(env, crate::RUNNER_CAPACITY).expect("runner");
    let stamp = desk.rig.feed.stamp;
    let reads_before = desk
        .rig
        .backend
        .calls()
        .iter()
        .filter(|c| **c == FakeBackendCall::FocusedWindow(desk.ids[CODE]))
        .count();
    let raise = desk.rig.backend.hold_sends();
    let activations = desk.native.backend.exact_activation_requests().len();
    desk.vis.note_focus_dispatched();
    runner.submit(Effect::Focus { stamp, window: code }).expect("focus");
    assert!(
        raise.wait_entered(1, Duration::from_secs(10)),
        "held between activation and raise"
    );
    assert_eq!(
        desk.native.backend.exact_activation_requests().len(),
        activations + 1
    );
    runner
        .submit(Effect::ReadFocus {
            stamp,
            application: desk.ids[CODE],
        })
        .expect("read");
    // Another application's read runs meanwhile; Code's waits for the raise.
    let spotify = desk.token(SPOTIFY, 3);
    runner.submit(Effect::Snapshot { stamp, window: spotify }).expect("snapshot");
    let taken = std::sync::Mutex::new(Vec::new());
    let completions = || {
        let inputs = ingress.with(|pipeline| pipeline.take(usize::MAX));
        let mut taken = taken.lock().expect("taken");
        taken.extend(inputs.into_iter().filter_map(|input| match input {
            Input::Completion { result, .. } => Some(result),
            _ => None,
        }));
        taken.clone()
    };
    wait_until("the other application's read", || {
        completions()
            .iter()
            .any(|c| matches!(c, CompletionResult::Snapshot(w, _) if *w == spotify))
    });
    let reads = |desk: &Desktop| {
        desk.rig
            .backend
            .calls()
            .iter()
            .filter(|c| **c == FakeBackendCall::FocusedWindow(desk.ids[CODE]))
            .count()
    };
    assert_eq!(reads(&desk), reads_before, "the focus read has not started");
    raise.release();
    wait_until("Code's focus read", || {
        completions().iter().any(|c| matches!(c, CompletionResult::Focus(..)))
    });
    assert!(runner.close_within(Duration::from_secs(5)));
    let read = completions()
        .into_iter()
        .find_map(|c| match c {
            CompletionResult::Focus(application, outcome) => Some((application, outcome)),
            _ => None,
        })
        .expect("focus completion");
    assert_eq!(
        read,
        (desk.ids[CODE], FocusOutcome::Resolved(code)),
        "the raised window"
    );

    // The real transition folds the read: the switch stays.
    desk.submit(Input::Completion {
        stamp,
        result: CompletionResult::Focus(read.0, read.1),
    });
    assert_eq!(desk.visible_workspace(), "coding");
    assert_eq!(desk.snap().focus.window, Some(code));
    // The read the single thread's order prevented would have switched back.
    desk.submit(Input::Completion {
        stamp,
        result: CompletionResult::Focus(desk.ids[CODE], FocusOutcome::Resolved(notes)),
    });
    assert_eq!(desk.visible_workspace(), "misc", "why the read must wait");
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
    // B's window leaves `AXWindows` (native fullscreen): its slot is released.
    // The workspace's Space change then lists B again through the watch.
    assert!(
        backend.sink().expect("sink").push(feed.event(WindowEventKind::Window {
            window: windows[1],
            change: WindowChange::Suspended,
        }))
    );
    let refreshes = || {
        backend
            .calls()
            .iter()
            .filter(|c| matches!(c, FakeBackendCall::Refresh(a) if *a == ids[1]))
            .count()
    };
    wait_until("settle listing", || refreshes() == 1);
    assert!(
        backend
            .sink()
            .expect("sink")
            .push(feed.event(WindowEventKind::EnumerationComplete {
                application: ids[1],
                windows: 0,
                truncated: false,
            }))
    );
    wait_until("slot released", || {
        runtime.snapshot().diagnostics.tab_slots_released == 1
    });
    assert!(native.backend.deliver_space_change() > 0, "space watch");
    wait_until("listed again on the Space change", || refreshes() == 2);
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

/// The user's setup with animations, the layout changed back and forth while
/// animations run: with or without the window-server bounds it ends on the
/// same frames. Returns every window's desired and observed frame.
fn users_setup_final_layout(bounds: bool) -> Vec<(u64, Option<Rect>, Option<Rect>)> {
    if bounds {
        BOOT_HOOK.set(Some(|desk: &mut Desktop| desk.bounds = true));
    } else {
        BOOT_HOOK.set(Some(|desk: &mut Desktop| {
            desk.rig.backend.script_bounds_missing(true);
        }));
    }
    let (mut desk, _, _) = split_desktop(Delivery::Immediate);
    desk.rig
        .animation
        .configure(&crate::config::AnimationConfig {
            enabled: true,
            ..crate::config::AnimationConfig::default()
        })
        .expect("the animation worker");
    // A new plan retargets a job the worker already runs, reading where the
    // window is (from the window server with the bounds, else a frame-only
    // read); a job still in the worker's mailbox is replaced without a read.
    // So the first change runs with every send held (its jobs cannot end) and
    // the second waits until the worker took its jobs: it is certain to
    // retarget them.
    let retarget_reads = |desk: &Desktop| {
        desk.rig
            .calls()
            .iter()
            .filter(|c| matches!(c, FakeBackendCall::Bounds(_) | FakeBackendCall::FrameRead(_)))
            .count()
    };
    let held = desk.rig.backend.hold_sends();
    let since = desk.mark();
    desk.submit(Input::Intent(Intent::SetLayout(LayoutType::Monocle)));
    desk.settle();
    assert!(
        desk.writes_since(since).iter().any(|write| write.animate),
        "the change animates"
    );
    wait_until("the worker took the jobs", || {
        desk.rig.animation.pending_jobs() == 0
    });
    // Dwindle, not straight back to Master: the held windows still read the
    // Master frames, which a direct write would restore without a job.
    let reads = retarget_reads(&desk);
    let since = desk.mark();
    desk.submit(Input::Intent(Intent::SetLayout(LayoutType::Dwindle)));
    desk.settle();
    assert!(
        desk.writes_since(since).iter().any(|write| write.animate),
        "the second change animates"
    );
    wait_until("a running job retargeted", || retarget_reads(&desk) > reads);
    held.release();
    for layout in [LayoutType::Monocle, LayoutType::Master] {
        desk.submit(Input::Intent(Intent::SetLayout(layout)));
        desk.settle();
    }
    wait_until("every animation settled", || {
        desk.settle();
        std::thread::sleep(Duration::from_millis(2));
        desk.rig
            .snap()
            .frames
            .iter()
            .all(|f| !f.in_flight && (f.desired.is_none() || f.actual == f.desired))
    });
    // Each run has its own adapter: windows are compared by serial.
    let frames = desk
        .rig
        .snap()
        .frames
        .iter()
        .map(|f| (f.window.serial(), f.desired, f.actual))
        .collect();
    if bounds {
        assert!(
            desk.rig.calls().iter().any(|c| matches!(c, FakeBackendCall::Bounds(_))),
            "the window server was asked"
        );
    }
    frames
}

#[test]
fn the_users_setup_ends_on_the_same_layout_with_and_without_window_server_bounds() {
    let with = users_setup_final_layout(true);
    let without = users_setup_final_layout(false);
    assert!(with.iter().any(|(_, desired, _)| desired.is_some()));
    assert_eq!(with, without);
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

// ---------------------------------------------- native fullscreen enter and exit

/// What a read of the Ghostty window returns: in native fullscreen (the whole
/// display, on its own Space) or back at `frame`.
fn ghostty_read(
    desk: &Desktop,
    window: WindowInstance,
    fullscreen: bool,
    frame: Rect,
) -> stache_macos::windows::WindowSnapshot {
    let mut read = desk.snapshot_of(window, "~", "AXStandardWindow", (frame.width, frame.height));
    // The identity is the window's own; the read keeps the known `CGWindowID`.
    read.cg_window_id = None;
    read.fullscreen = AxReading::Value(fullscreen);
    read.frame.position = AxReading::Value(AxPoint { x: frame.x, y: frame.y });
    read
}

fn script_read(desk: &Desktop, read: stache_macos::windows::WindowSnapshot) {
    desk.rig.backend.script_snapshot(
        read.window,
        stache_macos::windows::SnapshotOutcome::Snapshot(Box::new(read)),
    );
}

/// The window's resize notification, flushed through the geometry coalescer.
fn resized(desk: &mut Desktop, window: WindowInstance) {
    desk.rig.push(WindowEventKind::Window {
        window,
        change: WindowChange::Resized,
    });
    desk.rig.pump();
    desk.rig.clock.advance(Duration::from_millis(20));
    desk.settle();
}

/// The split terminal with Ghostty in native fullscreen: the flag is observed,
/// the window is out of the layout and Device Hub takes the workspace.
fn ghostty_fullscreen() -> (Desktop, WindowInstance, WindowInstance, Rect) {
    let (mut desk, ghostty, hub) = split_desktop(Delivery::Immediate);
    let slot = desk.desired_of(ghostty);
    let full = Rect::new(0.0, 0.0, 1920.0, 1080.0);
    script_read(&desk, ghostty_read(&desk, ghostty, true, full));
    let mark = desk.mark();
    resized(&mut desk, ghostty);
    let window = desk.snap().window(ghostty).expect("ghostty").clone();
    assert!(window.observed.fullscreen, "the fullscreen flag is observed");
    let writes = desk.writes_since(mark);
    assert!(
        writes.iter().all(|write| write.window != ghostty),
        "a fullscreen window is never written: {writes:?}"
    );
    assert!(
        desk.desired_of(hub).width > slot.width,
        "Device Hub takes the workspace: {:?}",
        desk.desired_of(hub)
    );
    (desk, ghostty, hub, slot)
}

fn assert_laid_out_again(desk: &Desktop, mark: usize, window: WindowInstance, slot: Rect) {
    let state = desk.snap();
    assert!(
        !state.window(window).expect("window").observed.fullscreen,
        "the fullscreen flag cleared"
    );
    let writes: Vec<Write> = desk
        .writes_since(mark)
        .into_iter()
        .filter(|write| write.window == window)
        .collect();
    let last = writes.last().unwrap_or_else(|| panic!("the window is written again"));
    assert!(
        (last.target.x - slot.x).abs() < 1.5 && (last.target.width - slot.width).abs() < 1.5,
        "back in its master slot: {last:?} vs {slot:?}"
    );
}

#[test]
fn a_window_leaving_native_fullscreen_is_laid_out_again() {
    let (mut desk, ghostty, _hub, slot) = ghostty_fullscreen();
    let mark = desk.mark();
    script_read(
        &desk,
        ghostty_read(&desk, ghostty, false, Rect::new(40.0, 60.0, 1200.0, 800.0)),
    );
    resized(&mut desk, ghostty);
    assert_laid_out_again(&desk, mark, ghostty, slot);
}

/// The exit's resize arrives while `AXFullScreen` still reads `true` (the exit
/// happens on another Space and the flag flips at its end). A later move of the
/// window shows the exit.
#[test]
fn a_fullscreen_exit_seen_only_by_a_later_move_is_laid_out_again() {
    let (mut desk, ghostty, _hub, slot) = ghostty_fullscreen();
    let back = Rect::new(40.0, 60.0, 1200.0, 800.0);
    script_read(&desk, ghostty_read(&desk, ghostty, true, back));
    resized(&mut desk, ghostty);
    assert!(desk.snap().window(ghostty).expect("ghostty").observed.fullscreen);
    let mark = desk.mark();
    script_read(&desk, ghostty_read(&desk, ghostty, false, back));
    desk.rig.push(WindowEventKind::Window {
        window: ghostty,
        change: WindowChange::Moved,
    });
    desk.rig.pump();
    desk.rig.clock.advance(Duration::from_millis(20));
    desk.settle();
    assert_laid_out_again(&desk, mark, ghostty, slot);
}

/// The application re-creates the window on the exit: the fullscreen one is
/// destroyed and a new identity appears, which is tiled like any new window
/// (it joins the stack; the old identity's slot is gone with it).
#[test]
fn a_window_re_created_on_fullscreen_exit_is_laid_out() {
    let (mut desk, ghostty, hub, _slot) = ghostty_fullscreen();
    desk.rig.push(WindowEventKind::Window {
        window: ghostty,
        change: WindowChange::Destroyed,
    });
    desk.settle();
    let mark = desk.mark();
    let again = desk.window_with(
        GHOSTTY,
        "~",
        "AXStandardWindow",
        (1200.0, 800.0),
        SnapshotOrigin::Created,
    );
    desk.settle();
    assert!(
        desk.snap().window(ghostty).is_none(),
        "the fullscreen identity is gone"
    );
    let desired = desk.desired_of(again);
    let writes: Vec<Write> = desk
        .writes_since(mark)
        .into_iter()
        .filter(|write| write.window == again)
        .collect();
    let last = writes.last().unwrap_or_else(|| panic!("the new window is written"));
    assert_eq!(last.target, desired, "written to its tile");
    assert!(
        desk.desired_of(hub).width < 1920.0 * 0.9,
        "Device Hub shares the workspace again: {:?}",
        desk.desired_of(hub)
    );
}

// ------------------------------- a window unlisted by native fullscreen returns

/// The active Space changed (the workspace notification names no application).
fn space_changed(rig: &mut Rig) {
    rig.ingress
        .submit(Input::Lifecycle {
            kind: LifecycleKind::SpaceChanged,
            info: AppInfo {
                application: app(999),
                pid: 0,
                bundle_id: String::new(),
                name: String::new(),
            },
        })
        .expect("space change");
    rig.pump();
}

/// A one-window application whose window entered native fullscreen: the owner
/// reports it unlisted and the settled listing releases its slot.
fn unlisted_single() -> (super::tabs::Tabs, WindowInstance, WindowInstance, Rect) {
    let (mut tabs, single, other) = super::tabs::Tabs::single();
    let slot = tabs.slot(single);
    tabs.suspend(single);
    tabs.settled_listing(&[]);
    assert!(tabs.suspended_of(single));
    assert_eq!(tabs.order(), vec![other], "the slot was released");
    assert_eq!(tabs.rig.snap().diagnostics.tab_slots_released, 1);
    (tabs, single, other, slot)
}

fn written(tabs: &super::tabs::Tabs, window: WindowInstance) -> usize {
    tabs.rig
        .backend
        .written_frames()
        .iter()
        .filter(|(token, _)| *token == window)
        .count()
}

#[test]
fn a_space_change_relists_an_unlisted_window_and_returns_it_to_its_slot() {
    let (mut tabs, single, other, slot) = unlisted_single();
    // Still unlisted (fullscreen): one refresh per Space change, nothing else.
    let (refreshes, writes) = (tabs.refreshes(), tabs.writes());
    space_changed(&mut tabs.rig);
    assert_eq!(tabs.refreshes(), refreshes + 1, "one refresh");
    tabs.end_listing();
    assert!(tabs.suspended_of(single));
    assert_eq!(tabs.order(), vec![other]);
    assert_eq!(tabs.writes(), writes, "nothing moved");

    // Fullscreen exited: the window rejoined `AXWindows` with no notification.
    let before = written(&tabs, single);
    space_changed(&mut tabs.rig);
    assert_eq!(tabs.refreshes(), refreshes + 2);
    tabs.listed(single, stache_macos::windows::TabBarRead::None, slot);
    tabs.end_listing();
    assert!(!tabs.suspended_of(single), "listed again");
    assert!(tabs.order().contains(&single), "back in the layout");
    assert!(written(&tabs, single) > before, "written to its slot");

    // Nothing is unlisted any more: a Space change lists nothing.
    let refreshes = tabs.refreshes();
    space_changed(&mut tabs.rig);
    tabs.rig.pump();
    assert_eq!(tabs.refreshes(), refreshes);
}

/// The listing that finds a window back from native fullscreen, read with the
/// given fullscreen flag at the frame it had in fullscreen.
fn listed_back(tabs: &mut super::tabs::Tabs, window: WindowInstance, fullscreen: bool) {
    let mut snapshot = super::tabs::Tabs::snapshot(
        window,
        stache_macos::windows::TabBarRead::None,
        Some(Rect::new(0.0, 0.0, 1920.0, 1200.0)),
    );
    snapshot.fullscreen = AxReading::Value(fullscreen);
    tabs.rig.push(WindowEventKind::Snapshot {
        snapshot,
        origin: SnapshotOrigin::Enumeration,
    });
    tabs.rig.pump();
    tabs.end_listing();
}

/// Native run 6: Ghostty's single window is unlisted by native fullscreen, its
/// slot released; the exit's Space change lists it again (off its tile, at the
/// fullscreen frame) and it must be laid out and written to its tile again.
#[test]
fn native_run6_a_released_window_listed_again_by_the_space_change_is_written_to_its_tile() {
    let (mut tabs, single, other, _slot) = unlisted_single();
    space_changed(&mut tabs.rig);
    tabs.end_listing();
    assert!(tabs.suspended_of(single));
    let before = written(&tabs, single);
    space_changed(&mut tabs.rig);
    listed_back(&mut tabs, single, false);
    assert!(!tabs.suspended_of(single), "listed again");
    assert!(tabs.order().contains(&single), "back in the layout");
    assert!(tabs.order().contains(&other));
    assert!(written(&tabs, single) > before, "written to its tile");
    assert_ne!(tabs.slot(single), Rect::new(0.0, 0.0, 1920.0, 1200.0));
}

/// Native run 6, the read order macOS produces: the listing that finds the
/// window again still reads `AXFullScreen` true (the exit's animation is not
/// over and no later notification comes); a later read says false.
#[test]
fn native_run6_a_window_listed_again_while_still_fullscreen_is_read_again_and_laid_out() {
    let (mut tabs, single, _other, _slot) = unlisted_single();
    space_changed(&mut tabs.rig);
    tabs.end_listing();
    let before = written(&tabs, single);
    space_changed(&mut tabs.rig);
    listed_back(&mut tabs, single, true);
    assert!(!tabs.suspended_of(single), "listed again");
    assert!(tabs.order().contains(&single), "back in the layout");
    // Still fullscreen on the next read: it is read again later (bounded).
    let refreshes = tabs.refreshes();
    tabs.settle_delay();
    assert_eq!(tabs.refreshes(), refreshes + 1, "read again after the settle");
    listed_back(&mut tabs, single, true);
    assert_eq!(written(&tabs, single), before, "not written while fullscreen");
    tabs.settle_delay();
    assert_eq!(tabs.refreshes(), refreshes + 2, "read again once more");
    listed_back(&mut tabs, single, false);
    assert!(written(&tabs, single) > before, "written to its tile");
    // Settled: nothing is read again.
    let refreshes = tabs.refreshes();
    tabs.settle_delay();
    tabs.wait(Duration::from_secs(2));
    assert_eq!(tabs.refreshes(), refreshes);
}

/// A window that stays fullscreen after it is listed again is read again a
/// bounded number of times, then left to the next Space change.
#[test]
fn a_window_listed_again_that_stays_fullscreen_is_read_again_a_bounded_number_of_times() {
    let (mut tabs, single, _other, _slot) = unlisted_single();
    space_changed(&mut tabs.rig);
    listed_back(&mut tabs, single, true);
    let refreshes = tabs.refreshes();
    let max = usize::from(crate::state::MAX_FULLSCREEN_RECHECKS);
    for _ in 0..2 * max {
        tabs.settle_delay();
        listed_back(&mut tabs, single, true);
    }
    let reads = tabs.refreshes() - refreshes;
    assert!(reads > 0 && reads <= max, "bounded re-reads: {reads}");
}

#[test]
fn a_space_change_never_relists_an_inactive_native_tab() {
    let (mut tabs, single, other) = super::tabs::Tabs::single();
    let slot = tabs.slot(single);
    // Command-T: the old window is an inactive member of the new tab's slot.
    tabs.suspend(single);
    let fresh = tabs.token(1);
    tabs.created(fresh, super::tabs::bar(7, 2), slot);
    tabs.settled_listing(&[(fresh, super::tabs::bar(7, 2))]);
    assert!(tabs.suspended_of(single));
    assert_eq!(tabs.order(), vec![fresh, other]);
    let refreshes = tabs.refreshes();
    space_changed(&mut tabs.rig);
    tabs.wait(Duration::from_millis(500));
    assert_eq!(tabs.refreshes(), refreshes, "an inactive tab triggers nothing");
    assert!(tabs.suspended_of(single));
}

#[test]
fn activating_an_application_with_an_unlisted_window_relists_it() {
    let (mut tabs, single, _other, slot) = unlisted_single();
    let refreshes = tabs.refreshes();
    // The Space notification was missed; the user brings the application forward.
    tabs.rig.ingress.submit(Input::Activated(app(1))).expect("activated");
    tabs.rig.pump();
    assert_eq!(tabs.refreshes(), refreshes + 1);
    let before = written(&tabs, single);
    tabs.listed(single, stache_macos::windows::TabBarRead::None, slot);
    tabs.end_listing();
    assert!(!tabs.suspended_of(single));
    assert!(written(&tabs, single) > before);
    // Activating it again with nothing unlisted lists nothing.
    let refreshes = tabs.refreshes();
    tabs.rig.ingress.submit(Input::Activated(app(1))).expect("activated");
    tabs.rig.pump();
    assert_eq!(tabs.refreshes(), refreshes);
}

/// The fullscreen flag flips after the last notification of the exit; the
/// Space change of the exit re-reads the window. Nothing polls meanwhile.
#[test]
fn a_fullscreen_exit_without_any_later_notification_is_found_by_the_space_change() {
    let (mut desk, ghostty, _hub, slot) = ghostty_fullscreen();
    let back = Rect::new(40.0, 60.0, 1200.0, 800.0);
    script_read(&desk, ghostty_read(&desk, ghostty, true, back));
    resized(&mut desk, ghostty);
    let reads = |desk: &Desktop| {
        desk.rig
            .calls()
            .iter()
            .filter(|call| matches!(call, FakeBackendCall::Snapshot(w) if *w == ghostty))
            .count()
    };
    // Idle while fullscreen: no deadline, no re-read however long it waits.
    let before = reads(&desk);
    desk.rig.clock.advance(Duration::from_secs(5));
    let turn = desk.rig.engine.turn();
    assert_eq!(turn.next_deadline, None, "no periodic work while fullscreen");
    desk.settle();
    assert_eq!(reads(&desk), before, "nothing polls");
    // A Space change while the flag still reads `true`: one read.
    space_changed(&mut desk.rig);
    desk.settle();
    assert_eq!(reads(&desk), before + 1, "one re-read per Space change");
    assert!(desk.snap().window(ghostty).expect("ghostty").observed.fullscreen);
    let mark = desk.mark();
    script_read(&desk, ghostty_read(&desk, ghostty, false, back));
    space_changed(&mut desk.rig);
    desk.settle();
    assert_laid_out_again(&desk, mark, ghostty, slot);
    // Nothing is fullscreen any more: a Space change reads nothing.
    let after = reads(&desk);
    space_changed(&mut desk.rig);
    desk.settle();
    assert_eq!(reads(&desk), after);
}

/// Native run 7: the exit's Space change lists the application before the
/// window rejoined `AXWindows`. The application is listed again after the
/// settle delay until the window is back: found on the third retry (still
/// reading fullscreen, windowed on the next read), it is written to its tile.
#[test]
fn native_run7_a_window_missing_from_the_space_change_listing_is_found_by_a_later_listing() {
    let (mut tabs, single, other, _slot) = unlisted_single();
    space_changed(&mut tabs.rig);
    tabs.end_listing();
    let before = written(&tabs, single);
    let refreshes = tabs.refreshes();
    space_changed(&mut tabs.rig);
    assert_eq!(tabs.refreshes(), refreshes + 1);
    // The immediate listing and the first two retries still lack the window.
    tabs.end_listing();
    for retry in 1..=2 {
        tabs.settle_delay();
        assert_eq!(tabs.refreshes(), refreshes + 1 + retry, "retry {retry}");
        tabs.end_listing();
        assert!(tabs.suspended_of(single), "still suspended, not untracked");
        assert!(tabs.rig.snap().window(single).is_some());
    }
    tabs.settle_delay();
    assert_eq!(tabs.refreshes(), refreshes + 4, "third retry");
    listed_back(&mut tabs, single, true);
    assert!(!tabs.suspended_of(single), "listed again");
    tabs.settle_delay();
    listed_back(&mut tabs, single, false);
    assert!(tabs.order().contains(&single), "back in its slot");
    assert!(tabs.order().contains(&other));
    assert!(written(&tabs, single) > before, "written to its tile");
    // Settled: nothing is listed again.
    let refreshes = tabs.refreshes();
    tabs.settle_delay();
    tabs.wait(Duration::from_secs(5));
    assert_eq!(tabs.refreshes(), refreshes);
}

/// A window that never rejoins `AXWindows` is listed for at most the bound,
/// then nothing runs until the next trigger.
#[test]
fn a_window_that_never_returns_is_listed_again_a_bounded_number_of_times() {
    let (mut tabs, single, _other, _slot) = unlisted_single();
    space_changed(&mut tabs.rig);
    tabs.end_listing();
    let refreshes = tabs.refreshes();
    let max = usize::from(crate::state::MAX_FULLSCREEN_RECHECKS);
    for _ in 0..2 * max {
        tabs.settle_delay();
        tabs.end_listing();
    }
    let retries = tabs.refreshes() - refreshes;
    assert!(retries > 0 && retries <= max, "bounded listings: {retries}");
    assert!(tabs.suspended_of(single), "still suspended, not untracked");
    let refreshes = tabs.refreshes();
    tabs.wait(Duration::from_secs(5));
    let turn = tabs.rig.engine.turn();
    assert_eq!(turn.next_deadline, None, "no deadline while idle");
    assert_eq!(tabs.refreshes(), refreshes, "nothing lists while idle");
}

#[test]
fn a_space_change_never_retries_an_inactive_native_tab() {
    let (mut tabs, single, _other) = super::tabs::Tabs::single();
    let slot = tabs.slot(single);
    tabs.suspend(single);
    let fresh = tabs.token(1);
    tabs.created(fresh, super::tabs::bar(7, 2), slot);
    tabs.settled_listing(&[(fresh, super::tabs::bar(7, 2))]);
    let refreshes = tabs.refreshes();
    space_changed(&mut tabs.rig);
    for _ in 0..4 {
        tabs.settle_delay();
    }
    tabs.wait(Duration::from_secs(2));
    assert_eq!(tabs.refreshes(), refreshes, "no retries for an inactive tab");
    assert!(tabs.suspended_of(single));
}

/// Native run 8: the entry's Space change lists Ghostty's window again while it
/// reads fullscreen, then it leaves `AXWindows` again (the fullscreen window
/// flickers in the listings) and its slot is released. The exit's Space change
/// lists it windowed: it is back in its workspace order and written to its
/// tile.
#[test]
fn native_run8_a_window_that_flickers_while_fullscreen_is_laid_out_after_the_exit() {
    let (mut tabs, single, other) = super::tabs::Tabs::single();
    // Ghostty is frontmost and its window focused (no activation follows the
    // exit: the application never stopped being frontmost).
    tabs.rig.backend.script_focused(single.application(), single);
    tabs.rig
        .ingress
        .submit(Input::Activated(single.application()))
        .expect("activated");
    tabs.rig.pump();
    tabs.focus(single);
    assert_eq!(tabs.rig.snap().focus.window, Some(single));
    tabs.suspend(single);
    tabs.settled_listing(&[]);
    assert_eq!(tabs.order(), vec![other], "the slot was released");
    // Entry Space change: listed while it reads fullscreen.
    space_changed(&mut tabs.rig);
    listed_back(&mut tabs, single, true);
    for _ in 0..4 {
        tabs.settle_delay();
        listed_back(&mut tabs, single, true);
    }
    // It leaves `AXWindows` again; the settled listing releases the slot.
    tabs.suspend(single);
    tabs.settled_listing(&[]);
    assert!(tabs.suspended_of(single));
    for _ in 0..usize::from(crate::state::MAX_FULLSCREEN_RECHECKS) + 2 {
        tabs.settle_delay();
        tabs.end_listing();
    }
    let before = written(&tabs, single);
    // Exit Space change: listed windowed.
    space_changed(&mut tabs.rig);
    listed_back(&mut tabs, single, false);
    tabs.settle_delay();
    assert!(!tabs.suspended_of(single), "listed again");
    assert!(tabs.order().contains(&single), "back in its workspace order");
    assert!(tabs.order().contains(&other));
    assert!(written(&tabs, single) > before, "written to its tile");
    assert_eq!(
        tabs.rig.snap().focus.window,
        Some(single),
        "the frontmost application's returned window has the focus again"
    );
}

/// A window that left `AXWindows` because it is on another macOS Space (not
/// fullscreen) and never comes back: one bounded series runs; then each later
/// activation lists its application once, while each Space change runs a
/// bounded series again, then everything is quiet.
#[test]
fn a_window_on_another_space_is_listed_once_per_trigger_after_one_series() {
    let (mut tabs, single, _other, _slot) = unlisted_single();
    space_changed(&mut tabs.rig);
    tabs.end_listing();
    let max = usize::from(crate::state::MAX_FULLSCREEN_RECHECKS);
    for _ in 0..max + 2 {
        tabs.settle_delay();
        tabs.end_listing();
    }
    assert!(tabs.suspended_of(single));
    for trigger in 0..4 {
        let refreshes = tabs.refreshes();
        let activation = trigger % 2 == 0;
        if activation {
            tabs.rig.ingress.submit(Input::Activated(app(1))).expect("activated");
            tabs.rig.pump();
        } else {
            space_changed(&mut tabs.rig);
        }
        tabs.end_listing();
        for _ in 0..max + 4 {
            tabs.settle_delay();
            tabs.end_listing();
        }
        let listed = tabs.refreshes() - refreshes;
        if activation {
            assert_eq!(listed, 1, "one listing for activation {trigger}");
        } else {
            assert!(listed > 1 && listed <= max + 1, "a bounded series: {listed}");
        }
        assert!(tabs.suspended_of(single), "still suspended, not untracked");
        // No idle work.
        let refreshes = tabs.refreshes();
        tabs.wait(Duration::from_secs(5));
        assert_eq!(tabs.refreshes(), refreshes, "quiet after trigger {trigger}");
    }
}

/// Native runs 4-7 (Ghostty): fullscreen entry unlists the window while it
/// still reads windowed; the entry series runs out. On the exit Space change
/// the window rejoins `AXWindows` only on the third retry: a full series runs
/// again and finds it.
#[test]
fn a_spent_window_is_found_by_the_series_of_the_exit_space_change() {
    let (mut tabs, single, other, _slot) = unlisted_single();
    space_changed(&mut tabs.rig);
    tabs.end_listing();
    let max = usize::from(crate::state::MAX_FULLSCREEN_RECHECKS);
    for _ in 0..max + 4 {
        tabs.settle_delay();
        tabs.end_listing();
    }
    let refreshes = tabs.refreshes();
    tabs.settle_delay();
    assert_eq!(tabs.refreshes(), refreshes, "the entry series ran out");
    let before = written(&tabs, single);
    space_changed(&mut tabs.rig);
    assert_eq!(tabs.refreshes(), refreshes + 1);
    tabs.end_listing();
    for retry in 1..=2 {
        tabs.settle_delay();
        assert_eq!(tabs.refreshes(), refreshes + 1 + retry, "retry {retry}");
        tabs.end_listing();
        assert!(tabs.suspended_of(single));
    }
    tabs.settle_delay();
    assert_eq!(tabs.refreshes(), refreshes + 4, "third retry");
    listed_back(&mut tabs, single, false);
    tabs.settle_delay();
    assert!(!tabs.suspended_of(single), "listed again");
    assert!(tabs.order().contains(&single), "back in its slot");
    assert!(tabs.order().contains(&other));
    assert!(written(&tabs, single) > before, "written to its tile");
}

// ------------------------------------------------- reload re-evaluates windows

impl Desktop {
    /// An accepted `tiling.toml` reload, as the module submits it.
    fn reload(&mut self, revision: u64, value: serde_json::Value) {
        self.release_late();
        let config: TilingConfig = serde_json::from_value(value).expect("config");
        self.submit(Input::Config {
            revision,
            config: Arc::new(config),
        });
        self.settle();
    }

    /// The published `tilingState` strip: workspace names in order.
    fn strip(&self) -> Vec<String> {
        let published = crate::published::project(&self.snap());
        published.workspaces.iter().map(|ws| ws.name.clone()).collect()
    }

    fn published_workspace_of(&self, window: WindowInstance) -> Option<String> {
        let published = crate::published::project(&self.snap());
        let key = crate::published::window_key(window);
        published
            .windows
            .iter()
            .find(|w| w.key == key)
            .and_then(|w| w.workspace.clone())
    }

    fn focused_workspace(&self) -> Option<String> {
        crate::published::project(&self.snap()).focus.workspace
    }
}

fn ab(extra: &serde_json::Value) -> serde_json::Value {
    let mut value = json!({
        "enabled": true,
        "workspaces": [
            { "name": "a", "rules": [{ "app_id": "com.mitchellh.ghostty" }] },
            { "name": "b", "rules": [{ "app_id": "com.microsoft.VSCode" }] },
        ],
    });
    if let (Some(map), Some(more)) = (value.as_object_mut(), extra.as_object()) {
        for (key, item) in more {
            map.insert(key.clone(), item.clone());
        }
    }
    value
}

/// Workspaces `a` (rule: Ghostty = X) and `b` (rule: Code = Y); the unmatched
/// Spotify, `WhatsApp` and Chrome (Z) in `a`, which is focused and visible.
fn ab_desktop() -> Desktop {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    desk.reload(2, ab(&json!({})));
    assert_eq!(desk.strip(), ["a", "b"]);
    assert_eq!(desk.workspace_of(desk.token(GHOSTTY, 1)), "a");
    assert_eq!(desk.workspace_of(desk.token(CODE, 2)), "b");
    for (app, serial) in [(SPOTIFY, 3), (WHATSAPP, 4), (CHROME, 5)] {
        assert_eq!(
            desk.workspace_of(desk.token(app, serial)),
            "a",
            "{}",
            APPS[app].1
        );
    }
    assert_eq!(desk.visible_workspace(), "a");
    assert_eq!(desk.focused_workspace().as_deref(), Some("a"));
    assert!(!desk.shown[CODE] && desk.shown[GHOSTTY] && desk.shown[SPOTIFY]);
    desk
}

#[test]
fn reload_a_new_workspace_takes_an_existing_window_by_its_rule() {
    let mut desk = ab_desktop();
    let code = desk.token(CODE, 2);
    desk.reload(
        3,
        json!({
            "enabled": true,
            "workspaces": [
                { "name": "a", "rules": [{ "app_id": "com.mitchellh.ghostty" }] },
                { "name": "b" },
                { "name": "c", "rules": [{ "app_id": "com.microsoft.VSCode" }] },
            ],
        }),
    );
    assert_eq!(desk.strip(), ["a", "b", "c"]);
    assert_eq!(desk.workspace_of(code), "c");
    assert_eq!(desk.published_workspace_of(code).as_deref(), Some("c"));
    assert_eq!(desk.workspace_of(desk.token(SPOTIFY, 3)), "a");
    assert_eq!(desk.visible_workspace(), "a");
    assert_eq!(desk.focused_workspace().as_deref(), Some("a"));
    assert!(!desk.shown[CODE], "a window on a hidden workspace stays hidden");
    desk.focus_workspace("c");
    assert!(desk.shown[CODE] && !desk.shown[GHOSTTY]);
}

#[test]
fn reload_removing_the_visible_workspace_places_its_windows_and_lays_them_out() {
    let mut desk = ab_desktop();
    let mark = desk.mark();
    desk.reload(
        3,
        json!({
            "enabled": true,
            "workspaces": [{ "name": "b", "rules": [{ "app_id": "com.microsoft.VSCode" }] }],
        }),
    );
    assert_eq!(desk.strip(), ["b"]);
    for (app, serial) in [
        (GHOSTTY, 1),
        (CODE, 2),
        (SPOTIFY, 3),
        (WHATSAPP, 4),
        (CHROME, 5),
    ] {
        assert_eq!(
            desk.workspace_of(desk.token(app, serial)),
            "b",
            "{}",
            APPS[app].1
        );
        assert!(desk.shown[app], "{} is on the visible workspace", APPS[app].1);
    }
    assert_eq!(desk.visible_workspace(), "b");
    assert_eq!(desk.focused_workspace().as_deref(), Some("b"));
    let written: Vec<_> = desk.writes_since(mark).iter().map(|w| w.window).collect();
    assert!(
        written.contains(&desk.token(CODE, 2)),
        "the visible workspace is laid out"
    );
    assert!(written.contains(&desk.token(GHOSTTY, 1)));
}

#[test]
fn reload_reversing_the_order_moves_nothing_and_writes_nothing() {
    let mut desk = ab_desktop();
    let mark = desk.mark();
    let requests = desk.native.backend.visibility_requests().len();
    desk.reload(
        3,
        json!({
            "enabled": true,
            "workspaces": [
                { "name": "b", "rules": [{ "app_id": "com.microsoft.VSCode" }] },
                { "name": "a", "rules": [{ "app_id": "com.mitchellh.ghostty" }] },
            ],
        }),
    );
    assert_eq!(desk.strip(), ["b", "a"]);
    let snap = desk.snap();
    let names: Vec<_> = snap.workspaces.iter().map(|ws| ws.name.as_str()).collect();
    assert_eq!(names, ["b", "a"], "the query order is the configuration order");
    assert_eq!(desk.workspace_of(desk.token(CODE, 2)), "b");
    assert_eq!(desk.workspace_of(desk.token(SPOTIFY, 3)), "a");
    assert_eq!(desk.visible_workspace(), "a");
    assert_eq!(desk.focused_workspace().as_deref(), Some("a"));
    assert!(desk.writes_since(mark).is_empty());
    assert_eq!(desk.native.backend.visibility_requests().len(), requests);
}

#[test]
fn reload_renaming_a_workspace_takes_its_windows_by_rule() {
    let mut desk = ab_desktop();
    let code = desk.token(CODE, 2);
    desk.reload(
        3,
        json!({
            "enabled": true,
            "workspaces": [
                { "name": "a", "rules": [{ "app_id": "com.mitchellh.ghostty" }] },
                { "name": "b2", "rules": [{ "app_id": "com.microsoft.VSCode" }] },
            ],
        }),
    );
    assert_eq!(desk.strip(), ["a", "b2"]);
    assert_eq!(desk.workspace_of(code), "b2");
    assert_eq!(desk.visible_workspace(), "a");
    assert!(!desk.shown[CODE]);
    assert_eq!(desk.focused_workspace().as_deref(), Some("a"));
}

#[test]
fn reload_of_a_non_workspace_key_re_evaluates_nothing() {
    let mut desk = ab_desktop();
    // A rule-matching window sent elsewhere by hand would be moved back by a
    // re-evaluation: it must stay.
    let ghostty = desk.token(GHOSTTY, 1);
    desk.submit(Input::Intent(Intent::SendWindowToWorkspace {
        window: Some(ghostty),
        workspace: "b".into(),
    }));
    desk.settle();
    assert_eq!(desk.workspace_of(ghostty), "b");
    let before = desk.snap();
    let mark = desk.mark();
    let requests = desk.native.backend.visibility_requests().len();
    desk.reload(3, ab(&json!({ "animations": { "duration": 350 } })));
    let after = desk.snap();
    assert_eq!(after.workspaces, before.workspaces);
    assert_eq!(after.focus, before.focus);
    assert_eq!(desk.workspace_of(ghostty), "b");
    assert!(desk.writes_since(mark).is_empty(), "no frame write");
    assert_eq!(
        desk.native.backend.visibility_requests().len(),
        requests,
        "no hide or unhide"
    );
}

#[test]
fn the_users_reload_of_nine_workspaces_places_running_apps_without_a_restart() {
    let mut desk = Desktop::boot_with(Delivery::Immediate, GHOSTTY, true, false);
    let hub = desk.token(DEVICE_HUB, 6);
    let spotify = desk.token(SPOTIFY, 3);
    assert_eq!(desk.workspace_of(hub), "terminal");
    assert_eq!(desk.workspace_of(spotify), "music");
    assert_eq!(desk.strip(), [
        "terminal",
        "coding",
        "browser",
        "music",
        "communication",
        "misc",
        "guitar",
        "files",
        "tasks"
    ]);
    assert!(desk.shown[DEVICE_HUB]);
    desk.reload(
        2,
        json!({
            "enabled": true,
            "master": { "position": "left", "ratio": 75 },
            "ignore": [
                { "app_id": "com.apple.finder", "title": "Copy" },
                { "app_name": "Raycast" },
            ],
            "workspaces": [
                { "name": "files", "layout": "grid", "screen": "main",
                  "rules": [{ "app_id": "com.apple.finder" }] },
                { "name": "terminal", "layout": "master", "screen": "main",
                  "rules": [{ "app_id": "com.mitchellh.ghostty" }] },
                { "name": "coding", "layout": "monocle", "screen": "main",
                  "rules": [{ "app_id": "com.microsoft.VSCode" }, { "app_id": "dev.zed.Zed" }] },
                { "name": "browser", "layout": "monocle", "screen": "secondary",
                  "rules": [{ "app_id": "com.google.Chrome" }] },
                { "name": "design", "layout": "floating", "screen": "main",
                  "rules": [{ "app_id": "com.example.DeviceHub" }] },
                { "name": "communication", "layout": "floating", "screen": "main",
                  "rules": [{ "app_id": "net.whatsapp.WhatsApp" }, { "app_id": "com.hnc.Discord" }] },
                { "name": "media", "layout": "monocle", "screen": "main",
                  "rules": [{ "app_id": "com.spotify.client" }] },
                { "name": "misc", "layout": "floating", "screen": "main" },
                { "name": "tasks", "layout": "grid", "screen": "main" },
            ],
        }),
    );
    assert_eq!(desk.strip(), [
        "files",
        "terminal",
        "coding",
        "browser",
        "design",
        "communication",
        "media",
        "misc",
        "tasks"
    ]);
    assert_eq!(
        desk.workspace_of(hub),
        "design",
        "the moved rule places the running app"
    );
    assert_eq!(desk.published_workspace_of(hub).as_deref(), Some("design"));
    assert_eq!(
        desk.workspace_of(spotify),
        "media",
        "the renamed workspace's app"
    );
    assert_eq!(desk.workspace_of(desk.token(GHOSTTY, 1)), "terminal");
    assert_eq!(desk.workspace_of(desk.token(CODE, 2)), "coding");
    assert_eq!(desk.workspace_of(desk.token(WHATSAPP, 4)), "communication");
    assert_eq!(desk.workspace_of(desk.token(CHROME, 5)), "browser");
    assert_eq!(desk.visible_workspace(), "terminal");
    assert_eq!(desk.focused_workspace().as_deref(), Some("terminal"));
    assert!(desk.shown[GHOSTTY]);
    assert!(!desk.shown[DEVICE_HUB], "moved to a hidden workspace: hidden");
    desk.focus_workspace("design");
    assert!(desk.shown[DEVICE_HUB] && !desk.shown[GHOSTTY]);
    desk.focus_workspace("media");
    assert!(desk.shown[SPOTIFY] && !desk.shown[DEVICE_HUB]);
}

impl Desktop {
    /// Native focus requests queued so far.
    fn focus_mark(&self) -> usize { self.focuses.lock().expect("focuses").len() }

    fn focuses_since(&self, mark: usize) -> Vec<WindowInstance> {
        self.focuses.lock().expect("focuses")[mark..].to_vec()
    }

    fn published_focus_window(&self) -> Option<WindowInstance> {
        let snap = self.snap();
        let key = crate::published::project(&snap).focus.window?;
        crate::published::resolve_window_key(&snap, &key)
    }
}

#[test]
fn reload_moving_the_focused_window_to_a_hidden_workspace_refocuses_natively() {
    let mut desk = ab_desktop();
    let ghostty = desk.token(GHOSTTY, 1);
    // A second Ghostty window stays on the visible workspace: the application
    // is only partly moved and must stay shown.
    let scratch = desk.window_with(
        GHOSTTY,
        "scratch",
        "AXStandardWindow",
        (1200.0, 800.0),
        SnapshotOrigin::Created,
    );
    desk.settle();
    desk.app_focuses(GHOSTTY, ghostty);
    desk.cmd_tab(GHOSTTY);
    assert_eq!(desk.published_focus_window(), Some(ghostty));
    assert_eq!(desk.workspace_of(scratch), "a");
    let mark = desk.focus_mark();
    desk.reload(
        3,
        json!({
            "enabled": true,
            "workspaces": [
                { "name": "a" },
                { "name": "b", "rules": [{ "app_id": "com.microsoft.VSCode" }] },
                { "name": "c", "rules": [{ "title": "~" }] },
            ],
        }),
    );
    assert_eq!(desk.workspace_of(ghostty), "c");
    assert_eq!(desk.workspace_of(scratch), "a");
    assert_eq!(desk.focused_workspace().as_deref(), Some("a"));
    let focused = desk.published_focus_window().expect("a focused window");
    assert_ne!(focused, ghostty);
    assert_eq!(desk.workspace_of(focused), "a");
    assert_eq!(
        desk.focuses_since(mark),
        vec![focused],
        "one native focus request, for the published focus"
    );
    assert!(
        desk.shown[GHOSTTY],
        "Ghostty keeps a window on the visible workspace"
    );
}

#[test]
fn reload_moving_the_focused_window_to_a_visible_workspace_keeps_the_focus() {
    let mut desk = ab_desktop();
    let ghostty = desk.token(GHOSTTY, 1);
    desk.submit(Input::Displays(Arc::new(stache_macos::DisplayLayout::new(
        vec![
            crate::test_support::display(1, true, Some(60.0), 0.0),
            crate::test_support::display(2, false, Some(60.0), 1920.0),
        ],
    ))));
    desk.settle();
    assert_eq!(desk.published_focus_window(), Some(ghostty));
    let mark = desk.focus_mark();
    desk.reload(
        3,
        json!({
            "enabled": true,
            "workspaces": [
                { "name": "a", "screen": "main" },
                { "name": "b", "screen": "main",
                  "rules": [{ "app_id": "com.microsoft.VSCode" }] },
                { "name": "c", "screen": "secondary",
                  "rules": [{ "app_id": "com.mitchellh.ghostty" }] },
            ],
        }),
    );
    assert_eq!(desk.workspace_of(ghostty), "c");
    assert_eq!(
        desk.focused_workspace().as_deref(),
        Some("c"),
        "focus follows the window"
    );
    assert_eq!(desk.published_focus_window(), Some(ghostty));
    assert!(desk.focuses_since(mark).is_empty(), "it still has native focus");
    assert!(desk.shown[GHOSTTY]);
    let snap = desk.snap();
    let visible: Vec<_> = snap
        .workspaces
        .iter()
        .filter(|ws| ws.visible)
        .map(|ws| ws.name.as_str())
        .collect();
    assert_eq!(visible, ["a", "c"]);
}

#[test]
fn a_reload_without_re_evaluation_requests_no_native_focus() {
    let mut desk = ab_desktop();
    let mark = desk.focus_mark();
    desk.reload(3, ab(&json!({ "animations": { "duration": 350 } })));
    desk.reload(
        4,
        json!({
            "enabled": true,
            "workspaces": [
                { "name": "a", "layout": "grid", "rules": [{ "app_id": "com.mitchellh.ghostty" }] },
                { "name": "b", "rules": [{ "app_id": "com.microsoft.VSCode" }] },
            ],
        }),
    );
    assert!(desk.focuses_since(mark).is_empty());
}

// ------------------------------------------- focusing an empty workspace

impl Desktop {
    fn two_displays(&mut self) {
        self.submit(Input::Displays(Arc::new(stache_macos::DisplayLayout::new(
            vec![
                crate::test_support::display(1, true, Some(60.0), 0.0),
                crate::test_support::display(2, false, Some(60.0), 1920.0),
            ],
        ))));
        self.settle();
    }

    fn visible_set(&self) -> Vec<String> {
        let snap = self.snap();
        snap.workspaces
            .iter()
            .filter(|ws| ws.visible)
            .map(|ws| ws.name.clone())
            .collect()
    }

    /// Applications hidden or unhidden since `mark` (visibility requests).
    fn requests_since(&self, mark: usize) -> Vec<(&'static str, VisibilityOperation)> {
        self.native.backend.visibility_requests()[mark..]
            .iter()
            .map(|(key, operation)| {
                let index = self.keys.iter().position(|k| k == key).expect("known app");
                (APPS[index].1, *operation)
            })
            .collect()
    }
}

/// The managed applications the desktop shows (Finder, the agents, Zoom and
/// a Device Hub without a window have no managed window and are never hidden).
fn managed_shown(desk: &Desktop) -> Vec<&'static str> {
    let unmanaged = ["MenuBarAgent", "BetterDisplay", "zoom.us", "Finder"];
    desk.shown_set()
        .into_iter()
        .filter(|name| !unmanaged.contains(name) && (desk.hub || *name != "Device Hub"))
        .collect()
}

/// `terminal` (Ghostty and Device Hub, frontmost Ghostty) is visible; the CLI
/// focuses the empty `target`, then `terminal` again.
fn empty_round_trip(delivery: Delivery, target: &str) {
    let mut desk = Desktop::boot_with(delivery, GHOSTTY, true, false);
    desk.release_late();
    // Hiding the frontmost Ghostty makes the system activate Device Hub, which
    // is being hidden too.
    desk.successor = Some(DEVICE_HUB);
    assert_eq!(managed_shown(&desk), ["Ghostty", "Device Hub"]);
    let mark = desk.native.backend.visibility_requests().len();
    desk.focus_workspace(target);
    desk.release_late();
    assert_eq!(desk.visible_workspace(), target, "{delivery:?}");
    assert_eq!(desk.focused_workspace().as_deref(), Some(target));
    assert_eq!(desk.snap().focus.window, None, "{delivery:?}: nothing to focus");
    let requests = desk.requests_since(mark);
    for app in ["Ghostty", "Device Hub"] {
        assert!(
            requests.contains(&(app, VisibilityOperation::Hide)),
            "{delivery:?} {target}: {app} hidden: {requests:?}"
        );
    }
    assert!(
        managed_shown(&desk).is_empty(),
        "{delivery:?} {target}: the empty workspace shows nothing: {:?}",
        desk.shown_set()
    );
    desk.focus_workspace("terminal");
    desk.release_late();
    assert_eq!(desk.visible_workspace(), "terminal", "{delivery:?}");
    assert_eq!(
        managed_shown(&desk),
        ["Ghostty", "Device Hub"],
        "{delivery:?} {target}: back"
    );
    assert_eq!(desk.snap().focus.window, Some(desk.token(GHOSTTY, 1)));
}

#[test]
fn focusing_an_empty_floating_workspace_hides_the_previous_one() {
    for delivery in [
        Delivery::Immediate,
        Delivery::Twice,
        Delivery::Late,
        Delivery::Never,
    ] {
        empty_round_trip(delivery, "misc");
    }
}

#[test]
fn focusing_an_empty_tiled_workspace_hides_the_previous_one() {
    for delivery in [
        Delivery::Immediate,
        Delivery::Twice,
        Delivery::Late,
        Delivery::Never,
    ] {
        empty_round_trip(delivery, "tasks");
    }
}

#[test]
fn focusing_an_empty_workspace_from_one_assigned_to_a_missing_display_hides_it() {
    // One display: `browser` (screen = "secondary") falls back to main.
    for target in ["misc", "tasks"] {
        let mut desk = Desktop::boot(Delivery::Immediate, CHROME);
        assert_eq!(desk.visible_workspace(), "browser");
        assert_eq!(managed_shown(&desk), ["Google Chrome"]);
        desk.focus_workspace(target);
        assert_eq!(desk.visible_workspace(), target);
        assert!(
            managed_shown(&desk).is_empty(),
            "{target}: {:?}",
            desk.shown_set()
        );
        desk.focus_workspace("browser");
        assert_eq!(managed_shown(&desk), ["Google Chrome"], "{target}: back");
    }
}

#[test]
fn focusing_an_empty_workspace_with_two_displays_hides_only_its_display() {
    for target in ["misc", "tasks"] {
        let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
        desk.two_displays();
        desk.cmd_tab(GHOSTTY);
        assert_eq!(desk.visible_set(), ["terminal", "browser"]);
        assert_eq!(managed_shown(&desk), ["Ghostty", "Google Chrome"]);
        desk.focus_workspace(target);
        assert_eq!(desk.visible_set(), ["browser", target]);
        assert_eq!(desk.focused_workspace().as_deref(), Some(target));
        assert_eq!(
            managed_shown(&desk),
            ["Google Chrome"],
            "{target}: the other display keeps its workspace"
        );
        desk.focus_workspace("terminal");
        assert_eq!(desk.visible_set(), ["terminal", "browser"]);
        assert_eq!(
            managed_shown(&desk),
            ["Ghostty", "Google Chrome"],
            "{target}: back"
        );
    }
}

// ------------------------------------------------- reload and native focus

#[test]
fn reload_removing_the_focused_workspace_of_a_secondary_display_keeps_the_focus_there() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    desk.two_displays();
    desk.reload(
        2,
        json!({
            "enabled": true,
            "workspaces": [
                { "name": "a", "screen": "main",
                  "rules": [{ "app_id": "com.mitchellh.ghostty" }] },
                { "name": "s1", "screen": "secondary",
                  "rules": [{ "app_id": "com.google.Chrome" }] },
                { "name": "s2", "screen": "secondary" },
            ],
        }),
    );
    let chrome = desk.token(CHROME, 5);
    desk.cmd_tab(CHROME);
    assert_eq!(desk.focused_workspace().as_deref(), Some("s1"));
    assert_eq!(desk.published_focus_window(), Some(chrome));
    let mark = desk.focus_mark();
    desk.reload(
        3,
        json!({
            "enabled": true,
            "workspaces": [
                { "name": "a", "screen": "main",
                  "rules": [{ "app_id": "com.mitchellh.ghostty" }] },
                { "name": "s2", "screen": "secondary" },
            ],
        }),
    );
    assert_eq!(
        desk.workspace_of(chrome),
        "s2",
        "the display's visible workspace"
    );
    assert_eq!(
        desk.focused_workspace().as_deref(),
        Some("s2"),
        "focus stays on the display that had it"
    );
    assert_eq!(desk.published_focus_window(), Some(chrome));
    assert!(
        desk.focuses_since(mark).is_empty(),
        "Chrome still has native focus"
    );
    assert!(desk.shown[CHROME] && desk.shown[GHOSTTY]);
}

#[test]
fn reload_moving_the_only_focused_window_to_a_hidden_workspace_leaves_nothing_focused() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    desk.reload(
        2,
        json!({
            "enabled": true,
            "workspaces": [
                { "name": "a", "rules": [{ "app_id": "com.mitchellh.ghostty" }] },
                { "name": "z", "rules": [{ "app_id": "com.microsoft.VSCode" },
                                         { "app_id": "com.spotify.client" },
                                         { "app_id": "net.whatsapp.WhatsApp" },
                                         { "app_id": "com.google.Chrome" }] },
            ],
        }),
    );
    let ghostty = desk.token(GHOSTTY, 1);
    assert_eq!(desk.visible_workspace(), "a");
    assert_eq!(desk.published_focus_window(), Some(ghostty));
    let mark = desk.focus_mark();
    desk.reload(
        3,
        json!({
            "enabled": true,
            "workspaces": [
                { "name": "a" },
                { "name": "z", "rules": [{ "app_id": "com.microsoft.VSCode" },
                                         { "app_id": "com.spotify.client" },
                                         { "app_id": "net.whatsapp.WhatsApp" },
                                         { "app_id": "com.google.Chrome" }] },
                { "name": "c", "rules": [{ "app_id": "com.mitchellh.ghostty" }] },
            ],
        }),
    );
    assert_eq!(desk.workspace_of(ghostty), "c");
    assert_eq!(desk.visible_workspace(), "a");
    assert_eq!(desk.focused_workspace().as_deref(), Some("a"));
    assert_eq!(desk.published_focus_window(), None, "no window is left in `a`");
    assert!(desk.focuses_since(mark).is_empty(), "nothing to focus natively");
    assert!(managed_shown(&desk).is_empty(), "{:?}", desk.shown_set());
}

#[test]
fn reload_ignoring_the_focused_window_releases_it_and_keeps_the_workspace() {
    let mut desk = ab_desktop();
    let ghostty = desk.token(GHOSTTY, 1);
    assert_eq!(desk.published_focus_window(), Some(ghostty));
    let mark = desk.focus_mark();
    let requests = desk.native.backend.visibility_requests().len();
    desk.reload(
        3,
        ab(&json!({ "ignore": [{ "app_id": "com.mitchellh.ghostty" }] })),
    );
    let snap = desk.snap();
    assert!(snap.window(ghostty).is_none(), "released from tiling");
    assert_eq!(desk.focused_workspace().as_deref(), Some("a"));
    assert_ne!(desk.published_focus_window(), Some(ghostty));
    assert!(
        desk.focuses_since(mark).is_empty(),
        "the released window keeps the native focus"
    );
    assert!(desk.shown[GHOSTTY], "a released application is never hidden");
    assert!(
        !desk.requests_since(requests).iter().any(|(app, _)| *app == "Ghostty"),
        "{:?}",
        desk.requests_since(requests)
    );
}

/// A Finder window on the hidden `files` workspace.
fn finder_window(desk: &mut Desktop) -> WindowInstance {
    let finder = desk.window_with(
        FINDER,
        "Documents",
        "AXStandardWindow",
        (1200.0, 800.0),
        SnapshotOrigin::Created,
    );
    desk.settle();
    assert_eq!(desk.workspace_of(finder), "files");
    finder
}

/// terminal -> `target` (empty) -> terminal, with macOS keeping the last
/// visible regular application.
fn empty_and_back(desk: &mut Desktop, target: &str) {
    desk.focus_workspace(target);
    assert_eq!(desk.visible_workspace(), target);
    assert!(
        managed_shown(desk).is_empty(),
        "{target}: Ghostty is hidden: {:?}",
        desk.shown_set()
    );
    assert!(desk.shown[FINDER], "{target}: Finder hosts the desktop");
    assert_eq!(desk.frontmost, FINDER, "{target}: macOS made Finder active");
    desk.focus_workspace("terminal");
    assert_eq!(desk.visible_workspace(), "terminal");
    assert_eq!(managed_shown(desk), ["Ghostty"], "{target}: back");
    assert_eq!(desk.snap().focus.window, Some(desk.token(GHOSTTY, 1)));
    assert_eq!(
        desk.native.backend.exact_activation_requests().last(),
        Some(&desk.keys[GHOSTTY]),
        "{target}: Ghostty is focused natively again"
    );
    assert_eq!(desk.frontmost, GHOSTTY);
    assert!(desk.shown[FINDER], "{target}: a windowless Finder stays shown");
}

#[test]
fn an_empty_workspace_hides_the_last_app_once_a_windowless_finder_is_shown_again() {
    for target in ["tasks", "misc"] {
        let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
        desk.last_app_kept = true;
        let finder = finder_window(&mut desk);
        assert!(!desk.shown[FINDER], "Finder's window is on a hidden workspace");
        // The window closes: a windowless Finder is not kept hidden.
        desk.rig.push(WindowEventKind::Window {
            window: finder,
            change: WindowChange::Destroyed,
        });
        desk.settle();
        assert!(desk.shown[FINDER], "{target}: Finder is shown again");
        empty_and_back(&mut desk, target);
    }
}

#[test]
fn an_empty_workspace_hides_the_last_app_when_finder_was_never_hidden() {
    for target in ["tasks", "misc"] {
        let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
        desk.last_app_kept = true;
        assert!(desk.shown[FINDER]);
        empty_and_back(&mut desk, target);
    }
}

#[test]
fn a_hide_kept_by_a_hidden_finder_is_abandoned_then_retried_at_the_next_switch() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    desk.last_app_kept = true;
    finder_window(&mut desk);
    assert!(!desk.shown[FINDER]);
    desk.focus_workspace("tasks");
    for _ in 0..6 {
        desk.rig.clock.advance(crate::visibility::CONFIRMATION_TIMEOUT);
        desk.settle();
    }
    assert_eq!(managed_shown(&desk), ["Ghostty"], "kept on screen by macOS");
    let log = desk.vis.diagnostics();
    assert!(
        log.iter().any(|line| line.contains("hide abandoned")
            && line.contains("Finder is hidden because it has windows on a hidden workspace")),
        "{log:?}"
    );
    let sent = desk.native.backend.visibility_requests().len();
    desk.rig.clock.advance(Duration::from_secs(60));
    desk.settle();
    assert_eq!(
        desk.native.backend.visibility_requests().len(),
        sent,
        "abandoned: no endless retry"
    );
    // A switch to a workspace with a window: Spotify is shown, Ghostty can go.
    desk.focus_workspace("music");
    assert_eq!(desk.visible_workspace(), "music");
    assert_eq!(managed_shown(&desk), ["Spotify"], "the stuck app is hidden now");
    assert!(!desk.shown[FINDER], "Finder keeps its hidden window hidden");
}

#[test]
fn a_windowless_finder_hidden_by_an_earlier_run_is_shown_at_startup() {
    BOOT_HOOK.set(Some(|desk: &mut Desktop| {
        // An earlier Stache run hid Finder; this run does not own that.
        desk.shown[FINDER] = false;
        desk.last_app_kept = true;
    }));
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    assert!(
        desk.requests_since(0).contains(&("Finder", VisibilityOperation::Unhide)),
        "{:?}",
        desk.requests_since(0)
    );
    assert!(desk.shown[FINDER], "Finder is shown during startup");
    empty_and_back(&mut desk, "tasks");
}

#[test]
fn a_windowless_finder_hidden_mid_run_is_shown_only_when_a_switch_needs_it() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    desk.last_app_kept = true;
    // The user hides Finder (Ghostty is visible, so macOS allows it).
    desk.shown[FINDER] = false;
    desk.notify(FINDER, LifecycleKind::Hidden);
    desk.settle();
    let mark = desk.native.backend.visibility_requests().len();
    desk.rig.clock.advance(Duration::from_secs(5));
    desk.settle();
    assert!(!desk.shown[FINDER], "left alone while nothing needs it");
    assert!(desk.requests_since(mark).is_empty());
    desk.focus_workspace("tasks");
    let requests = desk.requests_since(mark);
    assert_eq!(
        requests[..2],
        [
            ("Finder", VisibilityOperation::Unhide),
            ("Ghostty", VisibilityOperation::Hide)
        ],
        "Finder is shown before the last app is hidden"
    );
    assert!(desk.shown[FINDER] && managed_shown(&desk).is_empty());
    assert_eq!(desk.frontmost, FINDER);
}

/// terminal -> tasks with Finder holding a window on hidden `files` and a
/// windowless Device Hub running.
fn hosted_by_device_hub(hub_hidden: bool) -> Desktop {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    desk.last_app_kept = true;
    desk.bare = vec![DEVICE_HUB];
    finder_window(&mut desk);
    assert!(!desk.shown[FINDER]);
    if hub_hidden {
        // Someone else hid it.
        desk.shown[DEVICE_HUB] = false;
        desk.notify(DEVICE_HUB, LifecycleKind::Hidden);
        desk.settle();
    }
    let mark = desk.native.backend.visibility_requests().len();
    let activations = desk.native.backend.exact_activation_requests().len();
    desk.focus_workspace("tasks");
    assert_eq!(desk.visible_workspace(), "tasks");
    let requests = desk.requests_since(mark);
    let expected: &[_] = if hub_hidden {
        &[
            ("Device Hub", VisibilityOperation::Unhide),
            ("Ghostty", VisibilityOperation::Hide),
        ]
    } else {
        &[("Ghostty", VisibilityOperation::Hide)]
    };
    assert_eq!(requests, expected, "the host is shown before the last app hides");
    assert_eq!(
        desk.native.backend.exact_activation_requests()[activations..],
        [desk.keys[DEVICE_HUB]],
        "the host is activated"
    );
    assert!(managed_shown(&desk).is_empty(), "{:?}", desk.shown_set());
    assert!(desk.shown[DEVICE_HUB], "the host stays shown");
    assert!(!desk.shown[FINDER], "Finder keeps its window hidden");
    assert_eq!(desk.frontmost, DEVICE_HUB);
    desk
}

#[test]
fn a_windowless_app_hosts_an_empty_workspace_while_finder_holds_a_hidden_window() {
    for hub_hidden in [false, true] {
        let mut desk = hosted_by_device_hub(hub_hidden);
        desk.focus_workspace("terminal");
        assert_eq!(managed_shown(&desk), ["Ghostty"]);
        assert_eq!(desk.snap().focus.window, Some(desk.token(GHOSTTY, 1)));
        assert_eq!(desk.frontmost, GHOSTTY);
        assert!(desk.shown[DEVICE_HUB], "the host is not hidden by Stache");
        assert!(!desk.shown[FINDER]);
        assert!(
            !desk.vis.diagnostics().iter().any(|line| line.contains("hide abandoned")),
            "{:?}",
            desk.vis.diagnostics()
        );
    }
}

#[test]
fn without_a_windowless_app_the_hide_is_abandoned_then_retried_at_the_next_switch() {
    // Device Hub has a window on `terminal`: no application is windowless.
    let mut desk = Desktop::boot_with(Delivery::Immediate, GHOSTTY, true, false);
    desk.last_app_kept = true;
    finder_window(&mut desk);
    let activations = desk.native.backend.exact_activation_requests().len();
    desk.empty_terminal_into("tasks");
    for _ in 0..6 {
        desk.rig.clock.advance(crate::visibility::CONFIRMATION_TIMEOUT);
        desk.settle();
    }
    assert_eq!(managed_shown(&desk).len(), 1, "kept on screen by macOS");
    assert_eq!(
        desk.native.backend.exact_activation_requests().len(),
        activations,
        "no host to activate"
    );
    let log = desk.vis.diagnostics();
    assert!(
        log.iter().any(|line| line.contains("hide abandoned")
            && line.contains("no windowless application was available as host either")),
        "{log:?}"
    );
    desk.focus_workspace("music");
    assert_eq!(managed_shown(&desk), ["Spotify"], "retried at the next switch");
}

#[test]
fn a_refused_focus_does_not_retry_an_abandoned_hide() {
    let mut desk = Desktop::boot_with(Delivery::Immediate, GHOSTTY, true, false);
    desk.last_app_kept = true;
    finder_window(&mut desk);
    desk.empty_terminal_into("tasks");
    for _ in 0..6 {
        desk.rig.clock.advance(crate::visibility::CONFIRMATION_TIMEOUT);
        desk.settle();
    }
    let abandoned = |desk: &Desktop| {
        desk.vis
            .diagnostics()
            .iter()
            .filter(|line| line.contains("hide abandoned"))
            .count()
    };
    assert_eq!(abandoned(&desk), 1, "{:?}", desk.vis.diagnostics());
    assert!(desk.inactive().contains(&"guitar".to_owned()));
    let requests = desk.native.backend.visibility_requests().len();
    let activations = desk.native.backend.exact_activation_requests().len();
    for serial in 1..=3 {
        desk.focus_workspace("guitar");
        for _ in 0..6 {
            desk.rig.clock.advance(crate::visibility::CONFIRMATION_TIMEOUT);
            desk.settle();
        }
        assert_eq!(desk.refused(), Some(("guitar".to_owned(), serial)));
    }
    assert!(
        desk.requests_since(requests).is_empty(),
        "{:?}",
        desk.requests_since(requests)
    );
    assert_eq!(
        desk.native.backend.exact_activation_requests().len(),
        activations
    );
    assert_eq!(abandoned(&desk), 1, "no new warning per press");
    // A real switch still retries the hide.
    desk.focus_workspace("music");
    assert_eq!(managed_shown(&desk), ["Spotify"], "retried at the next switch");
}

#[test]
fn a_host_that_opens_a_window_becomes_an_ordinary_app() {
    let mut desk = hosted_by_device_hub(false);
    let hub = desk.window_with(
        DEVICE_HUB,
        "Device Hub",
        "AXStandardWindow",
        (900.0, 600.0),
        SnapshotOrigin::Created,
    );
    desk.settle();
    // Its rule places it on `terminal`; the active app's new window takes the
    // focus there, as for any new window.
    assert_eq!(desk.workspace_of(hub), "terminal");
    assert_eq!(desk.visible_workspace(), "terminal");
    assert_eq!(desk.snap().focus.window, Some(hub));
    assert!(desk.shown[DEVICE_HUB] && desk.shown[GHOSTTY]);
    assert!(!desk.shown[FINDER]);
}

#[test]
fn a_windowless_finder_hosts_before_any_other_windowless_app() {
    let mut desk = Desktop::boot(Delivery::Immediate, GHOSTTY);
    desk.last_app_kept = true;
    desk.bare = vec![DEVICE_HUB];
    let activations = desk.native.backend.exact_activation_requests().len();
    empty_and_back(&mut desk, "tasks");
    assert!(
        !desk.native.backend.exact_activation_requests()[activations..]
            .contains(&desk.keys[DEVICE_HUB]),
        "Device Hub is never activated"
    );
}

#[test]
fn the_last_visible_app_is_not_sent_a_hide_macos_would_refuse() {
    // Every other application is hidden or being hidden, Finder holds a hidden
    // window and Device Hub has a window: nothing can host the desktop.
    let mut desk = Desktop::boot_with(Delivery::Immediate, GHOSTTY, true, false);
    desk.last_app_kept = true;
    for app in [ZOOM, BETTERDISPLAY] {
        desk.shown[app] = false;
        desk.notify(app, LifecycleKind::Hidden);
    }
    finder_window(&mut desk);
    let mark = desk.native.backend.visibility_requests().len();
    desk.empty_terminal_into("tasks");
    assert_eq!(desk.visible_workspace(), "terminal");
    assert_eq!(desk.requests_since(mark), [(
        "Device Hub",
        VisibilityOperation::Hide
    )]);
    assert_eq!(desk.snap().visibility.kept_last, Some(desk.ids[GHOSTTY]));
    assert_eq!(managed_shown(&desk), ["Ghostty"]);
    assert_eq!(desk.frontmost, GHOSTTY, "never deactivated by a refused hide");
    desk.rig.clock.advance(Duration::from_secs(10));
    desk.settle();
    assert_eq!(desk.requests_since(mark).len(), 1, "nothing re-sent");
    assert!(
        !desk.vis.diagnostics().iter().any(|line| line.contains("hide abandoned")),
        "{:?}",
        desk.vis.diagnostics()
    );
    // Device Hub's window joins Ghostty's on the hidden `tasks`, so the way
    // back shows the other hidden application again too.
    desk.send_window(desk.token(DEVICE_HUB, 6), "tasks");
    assert_eq!(managed_shown(&desk), ["Ghostty"], "still hidden meanwhile");
    let activations = desk.native.backend.exact_activation_requests().len();
    desk.focus_workspace("tasks");
    assert_eq!(managed_shown(&desk), ["Ghostty", "Device Hub"]);
    assert_eq!(desk.snap().focus.window, Some(desk.token(GHOSTTY, 1)));
    assert_eq!(
        desk.native.backend.exact_activation_requests()[activations..].last(),
        Some(&desk.keys[GHOSTTY]),
        "focused natively on the way back"
    );
}

#[test]
fn a_return_while_the_kept_hide_is_unsettled_focuses_the_window_natively() {
    // Zoom's visibility is unknown, so the hide is sent and macOS keeps Ghostty.
    let mut desk = Desktop::boot_with(Delivery::Immediate, GHOSTTY, true, false);
    desk.last_app_kept = true;
    finder_window(&mut desk);
    desk.empty_terminal_into("tasks");
    desk.rig.clock.advance(crate::visibility::CONFIRMATION_TIMEOUT);
    desk.settle();
    // Back before the hide is abandoned.
    let activations = desk.native.backend.exact_activation_requests().len();
    desk.focus_workspace("tasks");
    assert_eq!(desk.visible_workspace(), "tasks");
    assert!(managed_shown(&desk).contains(&"Ghostty"));
    assert_eq!(desk.snap().focus.window, Some(desk.token(GHOSTTY, 1)));
    assert_eq!(
        desk.native.backend.exact_activation_requests()[activations..].last(),
        Some(&desk.keys[GHOSTTY]),
        "focused natively on the way back"
    );
    assert_eq!(desk.frontmost, GHOSTTY);
}

// ------------------------------------------------------ inactive workspaces

/// The user's nine workspaces on one display (Device Hub stands for the
/// remaining applications with a window, on `design`). With `secondary`,
/// `browser` is on the other display.
fn nine_workspaces(browser_screen: &str) -> serde_json::Value {
    json!({
        "enabled": true,
        "workspaces": [
            { "name": "files", "layout": "grid", "screen": "main",
              "rules": [{ "app_id": "com.apple.finder" }] },
            { "name": "terminal", "layout": "master", "screen": "main",
              "rules": [{ "app_id": "com.mitchellh.ghostty" }] },
            { "name": "coding", "layout": "monocle", "screen": "main",
              "rules": [{ "app_id": "com.microsoft.VSCode" }] },
            { "name": "browser", "layout": "monocle", "screen": browser_screen,
              "rules": [{ "app_id": "com.google.Chrome" }] },
            { "name": "design", "layout": "floating", "screen": "main",
              "rules": [{ "app_id": "com.example.DeviceHub" }] },
            { "name": "communication", "layout": "floating", "screen": "main",
              "rules": [{ "app_id": "net.whatsapp.WhatsApp" }] },
            { "name": "media", "layout": "monocle", "screen": "main",
              "rules": [{ "app_id": "com.spotify.client" }] },
            { "name": "misc", "layout": "floating", "screen": "main" },
            { "name": "tasks", "layout": "grid", "screen": "main" },
        ],
    })
}

impl Desktop {
    /// The published `inactive` workspaces.
    fn inactive(&self) -> Vec<String> {
        crate::published::project(&self.snap())
            .workspaces
            .into_iter()
            .filter(|ws| ws.inactive)
            .map(|ws| ws.name)
            .collect()
    }

    /// The published refused focus: workspace and counter.
    fn refused(&self) -> Option<(String, u32)> {
        crate::published::project(&self.snap())
            .refused_focus
            .map(|refused| (refused.workspace, refused.serial))
    }

    /// No kept-last state and no abandoned hide so far.
    fn never_stuck(&self) {
        assert_eq!(self.snap().visibility.kept_last, None);
        assert!(
            !self.vis.diagnostics().iter().any(|line| line.contains("hide abandoned")),
            "{:?}",
            self.vis.diagnostics()
        );
    }
}

/// The user's setup: Ghostty on `terminal`, a Finder window on `files`, every
/// other application with a window, macOS keeping the last visible
/// application.
fn users_desktop() -> (Desktop, WindowInstance) {
    let mut desk = Desktop::boot_with(Delivery::Immediate, GHOSTTY, true, false);
    desk.last_app_kept = true;
    desk.reload(2, nine_workspaces("main"));
    let finder = finder_window(&mut desk);
    assert_eq!(desk.visible_workspace(), "terminal");
    assert_eq!(desk.workspace_of(desk.token(GHOSTTY, 1)), "terminal");
    assert!(!desk.shown[FINDER], "Finder's window is on a hidden workspace");
    (desk, finder)
}

#[test]
fn an_empty_workspace_without_a_desktop_host_is_inactive_until_finder_closes_its_window() {
    let (mut desk, finder) = users_desktop();
    assert_eq!(desk.inactive(), ["misc", "tasks"]);
    assert_eq!(desk.refused(), None);

    // The key for `tasks` is refused: nothing native happens.
    let requests = desk.native.backend.visibility_requests().len();
    let activations = desk.native.backend.exact_activation_requests().len();
    let focuses = desk.focus_mark();
    let writes = desk.mark();
    desk.focus_workspace("tasks");
    assert_eq!(
        desk.snap().last_intent.as_ref().map(|record| record.outcome.clone()),
        Some(crate::intent::IntentOutcome::Rejected(
            crate::intent::IntentRejection::InactiveWorkspace
        ))
    );
    assert_eq!(desk.visible_workspace(), "terminal");
    assert_eq!(desk.focused_workspace().as_deref(), Some("terminal"));
    assert!(desk.requests_since(requests).is_empty());
    assert_eq!(
        desk.native.backend.exact_activation_requests().len(),
        activations
    );
    assert!(desk.focuses_since(focuses).is_empty());
    assert!(desk.writes_since(writes).is_empty());
    assert!(managed_shown(&desk).contains(&"Ghostty"));
    assert_eq!(desk.refused(), Some(("tasks".to_owned(), 1)));
    desk.focus_workspace("tasks");
    assert_eq!(desk.refused(), Some(("tasks".to_owned(), 2)));
    desk.never_stuck();

    // The Finder window closes: Finder hosts the desktop again.
    desk.rig.push(WindowEventKind::Window {
        window: finder,
        change: WindowChange::Destroyed,
    });
    desk.settle();
    assert!(desk.shown[FINDER]);
    assert!(desk.inactive().is_empty());
    assert_eq!(desk.refused(), Some(("tasks".to_owned(), 2)), "kept, unchanged");

    // Now the key works and only the desktop is left.
    desk.focus_workspace("tasks");
    assert_eq!(desk.visible_workspace(), "tasks");
    assert!(managed_shown(&desk).is_empty(), "{:?}", desk.shown_set());
    assert!(desk.shown[FINDER]);
    assert_eq!(desk.frontmost, FINDER, "Finder is active");
    desk.never_stuck();

    // Coming back focuses Ghostty.
    desk.focus_workspace("terminal");
    assert_eq!(desk.visible_workspace(), "terminal");
    assert_eq!(managed_shown(&desk), ["Ghostty"]);
    assert_eq!(desk.snap().focus.window, Some(desk.token(GHOSTTY, 1)));
    assert_eq!(
        desk.native.backend.exact_activation_requests().last(),
        Some(&desk.keys[GHOSTTY])
    );
    assert_eq!(desk.frontmost, GHOSTTY);
    desk.never_stuck();
}

#[test]
fn a_window_on_another_display_makes_an_empty_workspace_reachable() {
    let mut desk = Desktop::boot_with(Delivery::Immediate, GHOSTTY, true, false);
    desk.last_app_kept = true;
    desk.two_displays();
    desk.reload(2, nine_workspaces("secondary"));
    finder_window(&mut desk);
    desk.cmd_tab(GHOSTTY);
    assert_eq!(desk.visible_set(), ["terminal", "browser"]);
    assert!(!desk.shown[FINDER]);
    assert!(desk.inactive().is_empty(), "Chrome stays on the other display");

    desk.focus_workspace("tasks");
    assert_eq!(desk.visible_set(), ["browser", "tasks"]);
    assert_eq!(managed_shown(&desk), ["Google Chrome"]);
    desk.never_stuck();
    desk.focus_workspace("terminal");
    assert_eq!(managed_shown(&desk), ["Ghostty", "Google Chrome"]);

    // Chrome's window moves to the hidden `coding`: the other display shows
    // nothing, and Chrome still has a window, so nothing hosts the desktop.
    desk.send_window(desk.token(CHROME, 5), "coding");
    assert_eq!(desk.inactive(), ["misc", "tasks"]);
    let requests = desk.native.backend.visibility_requests().len();
    desk.focus_workspace("tasks");
    assert_eq!(desk.refused(), Some(("tasks".to_owned(), 1)));
    assert!(desk.requests_since(requests).is_empty());
    assert_eq!(desk.visible_set(), ["terminal", "browser"]);
}
