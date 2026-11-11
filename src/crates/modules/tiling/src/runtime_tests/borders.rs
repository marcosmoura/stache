//! Native borders end to end: the real actor, border engine, route, effect
//! runner and frame paths over the fake window backend, the fake application
//! backend and the fake overlay backend with a manual main executor.
//!
//! Every scenario reads one ordered event log that interleaves the overlay
//! commands as the (fake) main thread applies them with the native calls the
//! window and application backends saw (frame writes, hide, unhide). The log is
//! sampled whenever the overlay backend runs and at every assertion, so a native
//! call made before an overlay command is logged before it.
//!
//! Spec acceptance mapping (`docs/specs/2026-11-09-tiling-borders/`):
//!
//! * TB-04-02 (hide before the application hides; show only after visible):
//!   `a_workspace_switch_hides_and_acknowledges_borders_before_the_app_hide`,
//!   `a_slow_main_thread_times_out_the_border_hide_but_never_delays_the_app_hide`,
//!   `incoming_borders_wait_for_the_shown_observation_the_reveal_write_and_one_frame`,
//!   `an_external_hide_orders_the_overlays_out_in_the_same_turn`,
//!   `a_window_of_an_application_on_two_workspaces_is_hidden_before_its_frame_write`,
//!   `startup_borders_only_the_visible_workspace_after_one_frame`.
//! * TB-04-03 (frames, focus, tabs):
//!   `direct_writes_move_the_border_and_a_stale_plan_is_ignored`,
//!   `a_partial_write_corrects_the_border_to_the_observed_frame`,
//!   `animated_layout_changes_are_followed_frame_by_frame_and_settle_exact`,
//!   `a_focus_change_recolors_in_place_and_reorders`,
//!   `a_tab_handover_releases_the_old_overlay_and_slot_held_keeps_it`, plus the
//!   configuration (`config_reload_*`, `disabling_*`) and stop scenarios.
//! * TB-09-02 (focus width grow and shrink end to end):
//!   `a_focus_change_shrinks_and_grows_and_an_app_hide_mid_shrink_hides_at_once`.

use std::sync::{Mutex, PoisonError};

use serde_json::json;
use stache_macos::border_overlay::testing::{FakeOverlayBackend, FakeSpaceEvents};
use stache_macos::border_overlay::{
    AppearanceChange, ApplyReport, BorderAppearance, BorderPaint, OverlayBackend,
    OverlayCapabilities, OverlayControl, OverlayHandle, OverlayKind, OverlayRect, OverlayWork,
    UnknownCause,
};
use stache_macos::windows::{IssuedKind, NativeStatus};
use stache_macos::{FakeAppKey, FakeApplicationBackend, ManualExecutor, VisibilityOperation};

use super::*;
use crate::actor::BorderNotes;
use crate::borders::{
    ActiveSpaceConfirm, BorderEngine, BorderFrameKind, BorderFrames, ConfirmFailed,
    SharedBorderRoute,
};
use crate::config::{BordersConfig, TilingConfig};
use crate::geometry::Rect;
use crate::intent::{FocusTarget, Intent};
use crate::state::AppInfo;
use crate::visibility::{ApplicationActionsSource, VisibilityControl};
use crate::visibility_tests::{FixtureSource, Native};

const FRAME: Duration = Duration::from_millis(20);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// --------------------------------------------------------------- event log

/// One thing that happened, in order.
#[derive(Clone, Debug, PartialEq)]
enum Ev {
    Show {
        handle: OverlayHandle,
        cg: u32,
        change: AppearanceChange,
        appearance: BorderAppearance,
        frame: OverlayRect,
    },
    Move(OverlayHandle, OverlayRect),
    Hide(OverlayHandle),
    Order(OverlayHandle, u32),
    Release(OverlayHandle),
    HideAll,
    ReleaseAll,
    /// A frame write handed to the window backend.
    Write(WindowInstance),
    AppHide(FakeAppKey),
    AppUnhide(FakeAppKey),
    Mark(&'static str),
}

struct Log {
    events: Mutex<Vec<Ev>>,
    seen: Mutex<(usize, usize)>,
    windows: Option<Arc<FakeWindowBackend>>,
    apps: Option<Arc<FakeApplicationBackend>>,
}

impl Log {
    /// Logs the native calls the backends saw since the last sample.
    fn sample(&self) {
        let mut fresh = Vec::new();
        {
            let mut seen = lock(&self.seen);
            if let Some(windows) = &self.windows {
                let batches = windows.frame_batches();
                for batch in batches.iter().skip(seen.0) {
                    fresh.extend(batch.frames.iter().map(|(window, _, _)| Ev::Write(*window)));
                }
                seen.0 = batches.len();
            }
            if let Some(apps) = &self.apps {
                let requests = apps.visibility_requests();
                for (key, operation) in requests.iter().skip(seen.1) {
                    fresh.push(match operation {
                        VisibilityOperation::Hide => Ev::AppHide(*key),
                        VisibilityOperation::Unhide => Ev::AppUnhide(*key),
                    });
                }
                seen.1 = requests.len();
            }
        }
        lock(&self.events).extend(fresh);
    }

    fn record(&self, event: Ev) { lock(&self.events).push(event); }

    fn mark(&self, name: &'static str) {
        self.sample();
        self.record(Ev::Mark(name));
    }

    fn all(&self) -> Vec<Ev> {
        self.sample();
        lock(&self.events).clone()
    }
}

/// The fake overlay backend, logging each command as the "main thread" runs it.
struct LoggedOverlay {
    inner: Arc<FakeOverlayBackend>,
    log: Arc<Log>,
}

impl OverlayBackend for LoggedOverlay {
    fn apply(&self, work: &OverlayWork) -> ApplyReport {
        self.log.sample();
        if work.hide_all.is_some() {
            self.log.record(Ev::HideAll);
        }
        for command in &work.commands {
            self.log.record(match command.kind {
                OverlayKind::Show {
                    target_cg_id,
                    frame,
                    appearance,
                    change,
                } => Ev::Show {
                    handle: command.handle,
                    cg: target_cg_id,
                    change,
                    appearance,
                    frame,
                },
                OverlayKind::Move { frame } => Ev::Move(command.handle, frame),
                OverlayKind::Hide => Ev::Hide(command.handle),
                OverlayKind::Order { target_cg_id } => Ev::Order(command.handle, target_cg_id),
                OverlayKind::Release => Ev::Release(command.handle),
            });
        }
        self.inner.apply(work)
    }

    fn release_all(&self) -> usize {
        self.log.sample();
        self.log.record(Ev::ReleaseAll);
        self.inner.release_all()
    }
}

/// The overlay side: control, route and the fake native layer.
struct Overlay {
    exec: Arc<ManualExecutor>,
    fake: Arc<FakeOverlayBackend>,
    log: Arc<Log>,
    control: Arc<OverlayControl>,
    route: Arc<SharedBorderRoute>,
    /// The Space event source the engine starts when borders are enabled; it
    /// reports "registered" at once. Scenarios inject facts through it.
    spaces: FakeSpaceEvents,
    /// While set, the Space confirmation finds every window on another Space.
    away: Arc<std::sync::atomic::AtomicBool>,
}

/// The Space confirmation of the scenarios: nothing is on the active Space
/// while `away` is set.
struct SpaceSwitch {
    away: Arc<std::sync::atomic::AtomicBool>,
}

impl ActiveSpaceConfirm for SpaceSwitch {
    fn confirm_on_active_space(
        &mut self,
        windows: &[(WindowInstance, u32)],
    ) -> Result<std::collections::BTreeSet<WindowInstance>, ConfirmFailed> {
        if self.away.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(std::collections::BTreeSet::new());
        }
        Ok(windows.iter().map(|(window, _)| *window).collect())
    }
}

impl Overlay {
    /// Jobs run inline until a test calls `exec.set_on_main(false)`.
    fn new(
        frames: &Arc<crate::FrameControl>,
        windows: Arc<FakeWindowBackend>,
        apps: Option<Arc<FakeApplicationBackend>>,
    ) -> Self {
        let exec = Arc::new(ManualExecutor::new(false));
        exec.set_on_main(true);
        let fake = Arc::new(FakeOverlayBackend::single_display());
        let (log, control) = Self::control(&exec, &fake, Some(windows), apps);
        let route = Arc::new(SharedBorderRoute::new(Arc::clone(&control)));
        frames.set_border_frames(Arc::clone(&route) as Arc<dyn BorderFrames>);
        Self {
            exec,
            fake,
            log,
            control,
            route,
            spaces: FakeSpaceEvents::new(),
            away: Arc::default(),
        }
    }

    fn control(
        exec: &Arc<ManualExecutor>,
        fake: &Arc<FakeOverlayBackend>,
        windows: Option<Arc<FakeWindowBackend>>,
        apps: Option<Arc<FakeApplicationBackend>>,
    ) -> (Arc<Log>, Arc<OverlayControl>) {
        let log = Arc::new(Log {
            events: Mutex::new(Vec::new()),
            seen: Mutex::new((0, 0)),
            windows,
            apps,
        });
        let control = Arc::new(OverlayControl::new(
            Arc::clone(exec) as Arc<dyn stache_sdk::MainThreadExecutor>,
            Arc::new(LoggedOverlay {
                inner: Arc::clone(fake),
                log: Arc::clone(&log),
            }) as Arc<dyn OverlayBackend>,
            OverlayCapabilities::default(),
        ));
        (log, control)
    }

    fn engine(&self) -> BorderEngine {
        let mut engine = BorderEngine::new(Arc::clone(&self.route), &BordersConfig::default());
        engine.set_space_source(Box::new(self.spaces.clone()));
        engine.set_confirmer(Box::new(SpaceSwitch { away: Arc::clone(&self.away) }));
        engine
    }
}

// ------------------------------------------------------------------ helpers

fn overlay_rect(rect: Rect) -> OverlayRect {
    OverlayRect::new(rect.x, rect.y, rect.width, rect.height)
}

/// Whether the border paints in the focused color (`#b4befe`, red 0.706) and
/// not the unfocused one (`#6c7086`, red 0.424).
fn is_focused_color(appearance: &BorderAppearance) -> bool {
    matches!(appearance.paint, BorderPaint::Solid(color) if color.r > 0.6)
}

fn red(appearance: &BorderAppearance) -> bool {
    matches!(appearance.paint, BorderPaint::Solid(color) if color.r > 0.99 && color.g < 0.01)
}

fn borders_config(borders: &serde_json::Value, outer: u32) -> Arc<TilingConfig> {
    Arc::new(
        serde_json::from_value(json!({
            "workspaces": [{ "name": "a" }, { "name": "b" }],
            "gaps": { "inner": 0, "outer": outer },
            "animations": { "enabled": false },
            "borders": borders,
        }))
        .expect("config"),
    )
}

fn enabled() -> serde_json::Value { json!({ "enabled": true }) }

fn raised() -> WindowActionOutcome {
    WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        native: NativeStatus::Action(Ok(())),
        observed: None,
    }
}

/// The desktop: two applications, workspaces `a` (visible) and `b`, the real
/// actor and effect runner, the border engine and the logged overlay.
struct Desk {
    rig: Rig,
    ids: Vec<ApplicationId>,
    keys: Vec<FakeAppKey>,
    overlay: Overlay,
    /// The border notes the status surface reads.
    notes: BorderNotes,
    /// The stop-time ledger the border side reports into.
    #[allow(dead_code, reason = "held so the border side reports somewhere")]
    lifecycle: Arc<crate::borders::BorderLifecycle>,
    revision: u64,
}

impl Desk {
    fn new() -> Self { Self::build(None) }

    /// A desk whose border engine has a live follow source.
    fn build(live: Option<Box<dyn stache_macos::border_overlay::LiveFollow>>) -> Self {
        let native = Arc::new(Native::new(Duration::from_millis(500)));
        let source = FixtureSource::new(&native, &["com.a", "com.b"]);
        let actions = source.open(&CancellationToken::new()).expect("open");
        let (ids, keys) = (source.ids(), source.keys());
        let mut rig = Rig::new("", Vec::new(), 64, 64);
        *rig.directory.apps.lock().expect("apps") = ids
            .iter()
            .enumerate()
            .map(|(n, id)| AppInfo {
                application: *id,
                pid: 100 + i32::try_from(n).unwrap_or_default(),
                bundle_id: format!("com.fixture.{n}"),
                name: format!("Fixture {n}"),
            })
            .collect();
        rig.backend.script_echo_frames(true);
        let vis = Arc::new(VisibilityControl::new());
        let overlay = Overlay::new(
            &rig.frames,
            Arc::clone(&rig.backend),
            Some(Arc::clone(&native.backend)),
        );
        let env = EffectEnv {
            backend: Arc::clone(&rig.backend) as Arc<dyn WindowBackend>,
            directory: Arc::clone(&rig.directory) as Arc<dyn crate::actor::AppDirectory>,
            ingress: Arc::clone(&rig.ingress),
            cancel: CancellationToken::new(),
            frames: Arc::clone(&rig.frames),
            visibility: Arc::clone(&vis),
            actions: Some(Arc::new(actions)),
            animation: Arc::clone(&rig.animation),
            borders: Some(Arc::clone(&overlay.route)),
        };
        let mut engine = Engine::new(
            TilingState::new(""),
            Arc::clone(&rig.ingress),
            Arc::clone(&rig.cell),
            Arc::new(InlineRunner::new(env)),
            Arc::clone(&rig.sink),
            Arc::clone(&rig.clock) as Arc<dyn crate::actor::Clock>,
        );
        engine.set_frame_control(Arc::clone(&rig.frames));
        engine.set_visibility_control(Arc::clone(&vis));
        let notes = BorderNotes::default();
        let mut border_engine = overlay.engine();
        if let Some(live) = live {
            border_engine.set_live_follow(live);
        }
        let lifecycle = Arc::new(crate::borders::BorderLifecycle::new());
        engine.set_border_engine(border_engine, Arc::clone(&notes), Arc::clone(&lifecycle));
        rig.engine = engine;
        rig.start();
        for id in &ids {
            rig.complete(*id);
        }
        let mut desk = Self {
            rig,
            ids,
            keys,
            overlay,
            notes,
            lifecycle,
            revision: 0,
        };
        desk.reload(&enabled(), 0);
        desk
    }

    fn reload(&mut self, borders: &serde_json::Value, outer: u32) {
        self.revision += 1;
        self.rig
            .ingress
            .submit(Input::Config {
                revision: self.revision,
                config: borders_config(borders, outer),
            })
            .expect("config");
        self.rig.pump();
    }

    fn window(&mut self, app: usize, serial: u64) -> WindowInstance {
        let token = WindowInstance::for_testing(
            self.rig.feed.adapter,
            self.rig.feed.epoch,
            self.ids[app],
            serial,
        );
        self.rig.created(token, "w", Self::cg(app, serial));
        self.rig.pump();
        token
    }

    const fn cg(app: usize, serial: u64) -> u32 { 100 * (app as u32 + 1) + serial as u32 }

    fn authorize(&mut self) {
        self.rig.gate.publish(self.rig.feed.stamp.registry, 1);
        self.rig.ingress.submit(Input::FrameWrites(true)).expect("authorize");
        self.rig.pump();
    }

    fn intent(&mut self, intent: Intent) {
        self.rig.ingress.submit(Input::Intent(intent)).expect("intent");
        self.rig.pump();
    }

    fn workspace(&mut self, name: &str) { self.intent(Intent::FocusWorkspace(name.into())); }

    fn lifecycle(&mut self, app: usize, kind: LifecycleKind) {
        let info = AppInfo {
            application: self.ids[app],
            pid: 100 + i32::try_from(app).unwrap_or_default(),
            bundle_id: format!("com.fixture.{app}"),
            name: format!("Fixture {app}"),
        };
        self.rig.ingress.submit(Input::Lifecycle { kind, info }).expect("lifecycle");
        self.rig.pump();
    }

    fn focus(&mut self, window: WindowInstance) {
        self.rig.push(WindowEventKind::Window {
            window,
            change: WindowChange::Focused,
        });
        self.rig.pump();
    }

    /// One display frame passes.
    fn frame(&mut self) {
        self.rig.clock.advance(FRAME);
        self.rig.pump();
    }

    fn mark(&self, name: &'static str) { self.overlay.log.mark(name); }

    fn log(&self) -> Vec<Ev> { self.overlay.log.all() }

    /// Events after the last mark called `name`.
    fn since(&self, name: &'static str) -> Vec<Ev> {
        let log = self.log();
        let at = log.iter().rposition(|e| *e == Ev::Mark(name)).expect("mark");
        log[at + 1..].to_vec()
    }

    /// The first overlay handle shown for the window with this `cg`.
    fn handle(&self, cg: u32) -> OverlayHandle {
        self.log()
            .iter()
            .find_map(|e| match e {
                Ev::Show { handle, cg: shown, .. } if *shown == cg => Some(*handle),
                _ => None,
            })
            .expect("a shown overlay")
    }

    /// The `Show` commands for `cg` in `events`.
    fn shows_in(events: &[Ev], cg: u32) -> Vec<&Ev> {
        events
            .iter()
            .filter(|e| matches!(e, Ev::Show { cg: c, .. } if *c == cg))
            .collect()
    }

    fn pos(events: &[Ev], wanted: impl Fn(&Ev) -> bool, what: &str) -> usize {
        events
            .iter()
            .position(wanted)
            .unwrap_or_else(|| panic!("{what} missing in {events:#?}"))
    }

    fn live(&self) -> usize { self.overlay.fake.live_overlays() }

    /// The standard start: `a` holds W1 (app 0) and W2 (app 1), `b` holds W3
    /// (app 1). W1 is focused. Writes are authorized and the first frame has
    /// passed.
    fn started() -> (Self, [WindowInstance; 3]) { Self::started_with(None) }

    fn started_with(
        live: Option<Box<dyn stache_macos::border_overlay::LiveFollow>>,
    ) -> (Self, [WindowInstance; 3]) {
        let mut desk = Self::build(live);
        let w1 = desk.window(0, 1);
        let w2 = desk.window(1, 1);
        desk.workspace("b");
        let w3 = desk.window(1, 2);
        desk.workspace("a");
        desk.focus(w1);
        desk.authorize();
        desk.frame();
        desk.frame();
        (desk, [w1, w2, w3])
    }
}

// ---------------------------------------------------------------- scenarios

/// TB-04-02: only the visible workspace's windows get a border, after one
/// display frame, and the focused one wears the focused color.
#[test]
fn startup_borders_only_the_visible_workspace_after_one_frame() {
    let mut desk = Desk::new();
    let w1 = desk.window(0, 1);
    let _w2 = desk.window(1, 1);
    desk.workspace("b");
    let _w3 = desk.window(1, 2);
    desk.workspace("a");
    desk.focus(w1);
    assert!(
        Desk::shows_in(&desk.log(), 101).is_empty(),
        "no border before a display frame has passed: {:#?}",
        desk.log()
    );
    desk.frame();
    let log = desk.log();
    let first = Desk::shows_in(&log, 101);
    let second = Desk::shows_in(&log, 201);
    assert_eq!((first.len(), second.len()), (1, 1), "{log:#?}");
    assert!(
        Desk::shows_in(&log, 202).is_empty(),
        "the window on the hidden workspace has no border"
    );
    let Ev::Show { appearance, .. } = first[0] else {
        unreachable!()
    };
    assert!(
        is_focused_color(appearance),
        "the focused window wears the focused color"
    );
    let Ev::Show { appearance, .. } = second[0] else {
        unreachable!()
    };
    assert!(!is_focused_color(appearance));
    assert_eq!(desk.live(), 2);
    assert_eq!(desk.overlay.fake.panels_created(), 2);
}

/// TB-04-02: the outgoing application's overlays are hidden and acknowledged
/// before the application hide.
#[test]
fn a_workspace_switch_hides_and_acknowledges_borders_before_the_app_hide() {
    let (mut desk, [w1, _w2, _w3]) = Desk::started();
    let h1 = desk.handle(Desk::cg(0, 1));
    assert_eq!(desk.live(), 2);
    desk.mark("switch");
    desk.workspace("b");
    let after = desk.since("switch");
    let release = Desk::pos(&after, |e| matches!(e, Ev::Release(h) if *h == h1), "W1 release");
    let hide = Desk::pos(
        &after,
        |e| matches!(e, Ev::AppHide(k) if *k == desk.keys[0]),
        "app hide",
    );
    assert!(
        release < hide,
        "the overlay is out (and acknowledged: the fake main thread ran it) before HideApp: \
         {after:#?}"
    );
    assert_eq!(desk.overlay.route.counters().late_hides, 0);
    assert!(desk.rig.snap().window(w1).is_some());
}

/// TB-04-02: a main thread that does not answer within the bound delays the
/// application hide by at most the bound and the lateness is counted.
#[test]
fn a_slow_main_thread_times_out_the_border_hide_but_never_delays_the_app_hide() {
    let (mut desk, _windows) = Desk::started();
    desk.overlay.exec.set_on_main(false);
    desk.mark("switch");
    let began = Instant::now();
    desk.workspace("b");
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "the hide waited more than the bound: {:?}",
        began.elapsed()
    );
    let after = desk.since("switch");
    assert!(
        after.iter().any(|e| matches!(e, Ev::AppHide(k) if *k == desk.keys[0])),
        "the application hide still went out: {after:#?}"
    );
    assert!(
        !after.iter().any(|e| matches!(e, Ev::Release(_))),
        "the border commands are still queued on the main thread: {after:#?}"
    );
    assert_eq!(desk.overlay.route.counters().late_hides, 1);
    // The main thread catches up: the overlays go out.
    desk.overlay.exec.set_on_main(true);
    desk.overlay.exec.drain();
    assert!(desk.since("switch").iter().any(|e| matches!(e, Ev::Release(_))));
    assert_eq!(desk.live(), 0);
}

/// TB-04-02: incoming windows show only after the shown observation, the
/// completed reveal write and one display frame, in that order.
#[test]
fn incoming_borders_wait_for_the_shown_observation_the_reveal_write_and_one_frame() {
    let (mut desk, [_w1, _w2, _w3]) = Desk::started();
    desk.workspace("b");
    desk.lifecycle(0, LifecycleKind::Hidden);
    // A layout change while away makes the return a real reveal write.
    desk.reload(&enabled(), 40);
    desk.mark("back");
    desk.workspace("a");
    desk.frame();
    desk.frame();
    let waiting = desk.since("back");
    assert!(
        waiting.iter().any(|e| matches!(e, Ev::AppUnhide(k) if *k == desk.keys[0])),
        "{waiting:#?}"
    );
    assert!(
        Desk::shows_in(&waiting, 101).is_empty(),
        "the hidden application's window stays unbordered until it is observed shown: \
         {waiting:#?}"
    );
    // The application that was never hidden follows write -> frame -> show.
    let w2_write = Desk::pos(
        &waiting,
        |e| matches!(e, Ev::Write(w) if w.application() == desk.ids[1]),
        "W2 write",
    );
    let w2_show = Desk::pos(&waiting, |e| matches!(e, Ev::Show { cg: 201, .. }), "W2 show");
    assert!(w2_write < w2_show, "{waiting:#?}");

    desk.mark("observed shown");
    desk.lifecycle(0, LifecycleKind::Unhidden);
    assert!(
        Desk::shows_in(&desk.since("observed shown"), 101).is_empty(),
        "one display frame has not passed since the shown observation"
    );
    desk.frame();
    let log = desk.log();
    let shown_at = log
        .iter()
        .rposition(|e| matches!(e, Ev::Show { cg: 101, .. }))
        .expect("W1 show");
    let observed_at = Desk::pos(&log, |e| *e == Ev::Mark("observed shown"), "mark");
    let unhide_at = Desk::pos(
        &log,
        |e| matches!(e, Ev::AppUnhide(k) if *k == desk.keys[0]),
        "unhide",
    );
    let write_at = log
        .iter()
        .rposition(|e| matches!(e, Ev::Write(w) if w.application() == desk.ids[0]))
        .expect("W1 reveal write");
    assert!(
        unhide_at < observed_at && observed_at < write_at && write_at < shown_at,
        "unhide, shown observation, the reveal write, one frame, then the border: {log:#?}"
    );
}

/// TB-04-02: a user hide (command-H) hides the overlays in the turn the
/// `Hidden` observation arrives; Cmd-Tab unhide follows the show rule.
#[test]
fn an_external_hide_orders_the_overlays_out_in_the_same_turn() {
    let (mut desk, _windows) = Desk::started();
    let h1 = desk.handle(Desk::cg(0, 1));
    desk.mark("command-h");
    desk.lifecycle(0, LifecycleKind::Hidden);
    let hidden = desk.since("command-h");
    assert!(hidden.contains(&Ev::Hide(h1)), "{hidden:#?}");
    assert!(
        !hidden.iter().any(|e| matches!(e, Ev::Hide(h) if *h != h1)),
        "only app 0 hid"
    );
    assert!(
        desk.overlay.fake.ordered_in() == 1,
        "the other application keeps its border"
    );

    desk.mark("cmd-tab");
    desk.lifecycle(0, LifecycleKind::Unhidden);
    assert!(
        Desk::shows_in(&desk.since("cmd-tab"), 101).is_empty(),
        "not before a frame"
    );
    desk.frame();
    let shown = desk.since("cmd-tab");
    assert_eq!(Desk::shows_in(&shown, 101).len(), 1, "{shown:#?}");
    assert_eq!(desk.overlay.fake.ordered_in(), 2);
    assert_eq!(desk.overlay.fake.panels_created(), 2, "the overlay was reused");
}

/// TB-04-02: windows of an application that stays visible (no application
/// hide) leave the visible workspace: their overlays are hidden before the
/// frame writes of the incoming workspace.
#[test]
fn a_window_of_an_application_on_two_workspaces_is_hidden_before_its_frame_write() {
    let (mut desk, [_w1, _w2, _w3]) = Desk::started();
    let h2 = desk.handle(Desk::cg(1, 1));
    desk.reload(&enabled(), 30);
    desk.mark("switch");
    desk.workspace("b");
    let after = desk.since("switch");
    assert!(
        !after.iter().any(|e| matches!(e, Ev::AppHide(k) if *k == desk.keys[1])),
        "the shared application is not hidden: {after:#?}"
    );
    let release = Desk::pos(&after, |e| matches!(e, Ev::Release(h) if *h == h2), "W2 release");
    let write = Desk::pos(
        &after,
        |e| matches!(e, Ev::Write(w) if w.application() == desk.ids[1]),
        "the incoming window's frame write",
    );
    assert!(release < write, "{after:#?}");
    desk.frame();
    assert_eq!(
        Desk::shows_in(&desk.since("switch"), 202).len(),
        1,
        "W3 shows after its write"
    );
}

/// TB-04-03: a direct write moves the border to its target and the readback
/// confirms it; a frame of an older plan is ignored.
#[test]
fn direct_writes_move_the_border_and_a_stale_plan_is_ignored() {
    let (mut desk, [w1, _w2, _w3]) = Desk::started();
    let h1 = desk.handle(Desk::cg(0, 1));
    desk.mark("relayout");
    desk.reload(&enabled(), 60);
    desk.frame();
    let after = desk.since("relayout");
    let target = desk
        .rig
        .backend
        .written_frames()
        .iter()
        .rev()
        .find(|(window, _)| *window == w1)
        .map(|(_, frame)| *frame)
        .expect("a write for W1");
    let last_move = after
        .iter()
        .rev()
        .find_map(|e| match e {
            Ev::Move(h, frame) if *h == h1 => Some(*frame),
            _ => None,
        })
        .expect("the border moved");
    assert_eq!(last_move, overlay_rect(target), "{after:#?}");
    // The write is also ordered after the border knew about the target: the
    // Move is applied no later than the next display frame.
    let stale = desk.overlay.route.counters().stale;
    let moves = desk.overlay.route.counters().moves;
    desk.overlay.route.report(
        w1,
        0,
        Rect::new(1.0, 2.0, 3.0, 4.0),
        BorderFrameKind::AnimationFrame,
    );
    let counters = desk.overlay.route.counters();
    assert_eq!(counters.stale, stale + 1, "a frame of an older plan is dropped");
    assert_eq!(counters.moves, moves, "and moves nothing");
    assert!(!desk.log().contains(&Ev::Move(h1, OverlayRect::new(1.0, 2.0, 3.0, 4.0))));
}

/// TB-04-03: a partial write corrects the border to the observed frame.
#[test]
fn a_partial_write_corrects_the_border_to_the_observed_frame() {
    let (mut desk, [w1, _w2, _w3]) = Desk::started();
    let h1 = desk.handle(Desk::cg(0, 1));
    let observed = Rect::new(7.0, 31.0, 640.0, 480.0);
    desk.rig.backend.script_frame_for(desk.ids[0], WindowActionOutcome::Issued {
        kind: IssuedKind::Partial,
        native: NativeStatus::Frame {
            position: stache_macos::ax::WriteStatus::Written,
            size: stache_macos::ax::WriteStatus::Failed(
                stache_macos::ax::AxError::ActionUnsupported,
            ),
        },
        observed: Some(stache_macos::ax::FrameRead {
            position: stache_macos::ax::AxReading::Value(stache_macos::ax::AxPoint {
                x: observed.x,
                y: observed.y,
            }),
            size: stache_macos::ax::AxReading::Value(stache_macos::ax::AxSize {
                width: observed.width,
                height: observed.height,
            }),
        }),
    });
    desk.mark("relayout");
    desk.reload(&enabled(), 80);
    desk.frame();
    let after = desk.since("relayout");
    let last_move = after
        .iter()
        .rev()
        .find_map(|e| match e {
            Ev::Move(h, frame) if *h == h1 => Some(*frame),
            _ => None,
        })
        .expect("the border moved");
    assert_eq!(
        last_move,
        overlay_rect(observed),
        "the border follows what the window really did, not what was asked: {after:#?}"
    );
    let _ = w1;
}

/// TB-04-03: a focus change recolors in place and re-orders; no overlay is
/// created or released. A Tiling focus re-orders after the raise.
#[test]
fn a_focus_change_recolors_in_place_and_reorders() {
    let (mut desk, [w1, w2, _w3]) = Desk::started();
    let (cg1, cg2) = (Desk::cg(0, 1), Desk::cg(1, 1));
    let (h1, h2) = (desk.handle(cg1), desk.handle(cg2));
    desk.mark("focus");
    desk.focus(w2);
    let after = desk.since("focus");
    let recolor = |handle: OverlayHandle| {
        after.iter().find_map(|e| match e {
            Ev::Show {
                handle: h, change, appearance, ..
            } if *h == handle => Some((*change, is_focused_color(appearance))),
            _ => None,
        })
    };
    assert_eq!(
        recolor(h2),
        Some((AppearanceChange::StateChange, true)),
        "{after:#?}"
    );
    assert_eq!(
        recolor(h1),
        Some((AppearanceChange::StateChange, false)),
        "{after:#?}"
    );
    assert!(
        after.contains(&Ev::Order(h2, cg2)),
        "the new focus is re-ordered: {after:#?}"
    );
    assert!(!after.iter().any(|e| matches!(e, Ev::Release(_) | Ev::Hide(_))));
    assert_eq!(desk.overlay.fake.panels_created(), 2, "no new panel");

    // Tiling focuses a window: the border is re-ordered after the raise.
    desk.rig.backend.script_action(raised());
    desk.mark("tiling focus");
    desk.intent(Intent::FocusWindow(FocusTarget::Window(w1)));
    let after = desk.since("tiling focus");
    assert!(after.contains(&Ev::Order(h1, cg1)), "{after:#?}");
    assert_eq!(desk.overlay.fake.panels_created(), 2);
}

/// TB-09-02 end to end: with `unfocused = false` and a transition, a focus
/// change shrinks the old border (its panel stays ordered in) and grows the
/// new one after its display frame; an application hide mid-shrink hides the
/// shrinking border in the same turn, and the shrink's end releases nothing
/// twice.
#[test]
fn a_focus_change_shrinks_and_grows_and_an_app_hide_mid_shrink_hides_at_once() {
    let (mut desk, [_w1, w2, _w3]) = Desk::started();
    let (cg1, cg2) = (Desk::cg(0, 1), Desk::cg(1, 1));
    let h1 = desk.handle(cg1);
    desk.mark("reload");
    desk.reload(
        &json!({
            "enabled": true,
            "unfocused": false,
            "transition": { "duration": 200, "easing": "linear" },
        }),
        0,
    );
    desk.frame();
    assert_eq!(desk.overlay.fake.ordered_in(), 1, "only the focused border");

    desk.mark("focus");
    desk.focus(w2);
    let after = desk.since("focus");
    assert!(
        Desk::shows_in(&after, cg1).iter().any(|e| matches!(e, Ev::Show {
            change: AppearanceChange::Shrink,
            ..
        })),
        "{after:#?}"
    );
    assert!(Desk::shows_in(&after, cg2).is_empty(), "not before a frame");
    assert!(!after.iter().any(|e| matches!(e, Ev::Hide(h) | Ev::Release(h) if *h == h1)));
    assert_eq!(
        desk.overlay.fake.ordered_in(),
        1,
        "the shrinking border stays in"
    );
    desk.frame();
    let grown = desk.since("focus");
    assert!(
        Desk::shows_in(&grown, cg2).iter().any(|e| matches!(e, Ev::Show {
            change: AppearanceChange::Grow,
            ..
        })),
        "{grown:#?}"
    );
    assert_eq!(desk.overlay.fake.ordered_in(), 2, "both visible mid-shrink");

    desk.mark("command-h");
    desk.lifecycle(0, LifecycleKind::Hidden);
    let hidden = desk.since("command-h");
    assert!(
        hidden.iter().any(|e| matches!(e, Ev::Hide(h) | Ev::Release(h) if *h == h1)),
        "hidden in the same turn, not after the shrink: {hidden:#?}"
    );
    assert_eq!(desk.overlay.fake.ordered_in(), 1);
    for _ in 0..15 {
        desk.frame();
    }
    assert_eq!(
        desk.overlay.fake.ordered_in(),
        1,
        "the new focus keeps its border"
    );
}

/// Review fix (part 09 interruption), end to end: focus leaves and comes back
/// within the transition. The engine grows the shrinking border and the actor
/// then reorders it (`Order`, same geometry): the grow track survives, no
/// zero-ring model is left, and the border stays visible at full width.
#[test]
fn focus_back_mid_shrink_with_the_reorder_leaves_the_border_visible() {
    use stache_macos::border_overlay::KEY_WIDTH_RING;
    use stache_macos::border_overlay::testing::FakeEvent;

    let (mut desk, [w1, w2, _w3]) = Desk::started();
    let cg1 = Desk::cg(0, 1);
    let h1 = desk.handle(cg1);
    desk.reload(
        &json!({
            "enabled": true,
            "unfocused": false,
            "transition": { "duration": 200, "easing": "linear" },
        }),
        0,
    );
    desk.frame();
    desk.overlay.fake.set_media_time(0.0);
    desk.focus(w2);
    desk.frame();
    desk.overlay.fake.set_media_time(0.05);
    desk.mark("back");
    desk.focus(w1);
    let after = desk.since("back");
    let grow = Desk::pos(
        &after,
        |e| matches!(e, Ev::Show { handle, change: AppearanceChange::Grow, .. } if *handle == h1),
        "grow",
    );
    let order = Desk::pos(&after, |e| *e == Ev::Order(h1, cg1), "reorder");
    assert!(grow < order, "{after:#?}");
    let panel = desk
        .overlay
        .fake
        .events()
        .iter()
        .rev()
        .find_map(|e| match e {
            FakeEvent::OrderRelative { panel, target, .. } if *target == cg1 => Some(*panel),
            _ => None,
        })
        .expect("panel of the focused border");
    assert!(desk.overlay.fake.is_ordered_in(panel));
    assert_eq!(desk.overlay.fake.ring_override(panel), None, "full width");
    assert!(
        desk.overlay.fake.animation(panel, KEY_WIDTH_RING).is_some(),
        "the grow track survives the reorder"
    );
    for _ in 0..15 {
        desk.frame();
    }
    assert!(desk.overlay.fake.is_ordered_in(panel));
    assert_eq!(desk.overlay.fake.ring_override(panel), None);
    assert!(
        !desk.log().iter().any(|e| matches!(e, Ev::Hide(h) | Ev::Release(h) if *h == h1)),
        "never hidden"
    );
}

/// Configuration: a color change is an instant reload; disabling the focused
/// state removes that border; `enabled: false` releases every overlay and
/// `true` shows them again.
#[test]
fn config_reload_recolors_instantly_and_enabled_false_releases_everything() {
    let (mut desk, [_w1, _w2, _w3]) = Desk::started();
    let h1 = desk.handle(Desk::cg(0, 1));
    desk.mark("recolor");
    desk.reload(
        &json!({ "enabled": true, "focused": { "width": 4, "color": "#ff0000" } }),
        0,
    );
    let after = desk.since("recolor");
    let recolored = after.iter().find_map(|e| match e {
        Ev::Show { handle, change, appearance, .. } if *handle == h1 => {
            Some((*change, red(appearance)))
        }
        _ => None,
    });
    assert_eq!(
        recolored,
        Some((AppearanceChange::Reload, true)),
        "no fade: {after:#?}"
    );
    assert_eq!(desk.overlay.fake.panels_created(), 2);

    // Disabling the focused state: every border uses the active state's width
    // (resolved OQ-1), and a disabled state has none, so no border remains.
    desk.mark("no focused border");
    desk.reload(&json!({ "enabled": true, "focused": false }), 0);
    let after = desk.since("no focused border");
    assert!(after.contains(&Ev::Release(h1)), "{after:#?}");
    assert_eq!(desk.live(), 0);
    // Turning the state back on shows them again, on fresh overlays.
    desk.reload(&enabled(), 0);
    desk.frame();
    desk.frame();
    assert_eq!(desk.live(), 2);

    // enabled -> false releases every overlay on the main thread.
    desk.mark("off");
    desk.reload(&json!({ "enabled": false }), 0);
    let after = desk.since("off");
    assert!(after.contains(&Ev::ReleaseAll), "{after:#?}");
    assert_eq!(desk.live(), 0);
    assert_eq!(desk.overlay.fake.ordered_in(), 0);
    desk.frame();
    desk.frame();
    assert!(
        !desk.since("off").iter().any(|e| matches!(e, Ev::Show { .. })),
        "nothing shows while borders are disabled"
    );

    // false -> true shows them again.
    desk.mark("on");
    desk.reload(&enabled(), 0);
    desk.frame();
    desk.frame();
    let after = desk.since("on");
    assert_eq!(Desk::shows_in(&after, 101).len(), 1, "{after:#?}");
    assert_eq!(Desk::shows_in(&after, 201).len(), 1, "{after:#?}");
    assert_eq!(desk.live(), 2);
}

/// Stop: closing the route and releasing leaves no overlay, and nothing is
/// sent afterwards.
#[test]
fn stop_releases_every_overlay_and_nothing_shows_afterwards() {
    let (mut desk, [w1, _w2, _w3]) = Desk::started();
    assert_eq!(desk.live(), 2);
    desk.overlay.route.close();
    let ticket = desk.overlay.control.release_all();
    assert!(ticket.wait(Duration::from_millis(500)).is_ok());
    assert_eq!(desk.live(), 0);
    assert_eq!(desk.overlay.fake.ordered_in(), 0);
    desk.mark("stopped");
    desk.reload(&enabled(), 90);
    desk.focus(w1);
    desk.frame();
    desk.frame();
    let overlay_events: Vec<Ev> = desk
        .since("stopped")
        .into_iter()
        .filter(|e| !matches!(e, Ev::Write(_)))
        .collect();
    assert_eq!(overlay_events, Vec::<Ev>::new(), "a stopped route sends nothing");
    assert_eq!(desk.live(), 0);
}

// ---------------------------------------------------------------------- tabs

/// TB-04-03: a slot's border follows its active member. While the old member
/// still holds the slot (`slot_held`) its overlay stays in place; on handover
/// the old overlay is released and the new member gets one after a frame.
#[test]
fn a_tab_handover_releases_the_old_overlay_and_slot_held_keeps_it() {
    use super::tabs::Tabs;
    let (mut tabs, single, other) = Tabs::single();
    let overlay = Overlay::new(&tabs.rig.frames, Arc::clone(&tabs.rig.backend), None);
    tabs.rig
        .engine
        .set_border_engine(overlay.engine(), BorderNotes::default(), Arc::default());
    tabs.rig
        .ingress
        .submit(Input::Config {
            revision: 2,
            config: Arc::new(
                serde_json::from_value(json!({
                    "enabled": true,
                    "workspaces": [{ "name": "main", "layout": "master", "screen": "main" }],
                    "borders": { "enabled": true },
                }))
                .expect("config"),
            ),
        })
        .expect("config");
    tabs.rig.pump();
    tabs.focus(single);
    tabs.wait(FRAME);
    tabs.wait(FRAME);
    let log = &overlay.log;
    let shown = |cg: u32| {
        log.all()
            .iter()
            .filter_map(|e| match e {
                Ev::Show { handle, cg: c, .. } if *c == cg => Some(*handle),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let single_cg = 100 + u32::try_from(single.serial()).unwrap();
    let other_cg = 100 + u32::try_from(other.serial()).unwrap();
    let held = shown(single_cg).first().copied().expect("the single window has a border");
    assert!(!shown(other_cg).is_empty());

    // Command-T: the old window is unlisted but holds the slot.
    log.mark("suspend");
    tabs.suspend(single);
    let after_suspend = lock(&log.events).clone();
    let after_suspend =
        after_suspend.split(|e| *e == Ev::Mark("suspend")).next_back().unwrap().to_vec();
    assert!(
        !after_suspend
            .iter()
            .any(|e| matches!(e, Ev::Release(h) | Ev::Hide(h) if *h == held)),
        "slot_held keeps the existing overlay in place: {after_suspend:#?}"
    );
    assert_eq!(overlay.fake.live_overlays(), 2);

    // The new tab arrives and takes the slot.
    let slot = tabs.slot(single);
    let fresh = tabs.token(1);
    let fresh_cg = 100 + u32::try_from(fresh.serial()).unwrap();
    log.mark("handover");
    tabs.created(fresh, super::tabs::bar(7, 2), slot);
    let released: Vec<_> = lock(&log.events)
        .split(|e| *e == Ev::Mark("handover"))
        .next_back()
        .unwrap()
        .to_vec();
    assert!(released.contains(&Ev::Release(held)), "{released:#?}");
    assert!(
        released.iter().all(|e| !matches!(e, Ev::Show { cg, .. } if *cg == fresh_cg)),
        "the new member waits for the show rule: {released:#?}"
    );
    tabs.wait(FRAME);
    tabs.wait(FRAME);
    assert_eq!(
        shown(fresh_cg).len(),
        1,
        "the new member's border follows the slot"
    );
    assert_eq!(overlay.fake.live_overlays(), 2);
}

// ---------------------------------------------------- animation and stop (real
// ---------------------------------------------------------------- runtime)

use stache_macos::ax::testing::{Call, FakeNode, FakeWindowAx, NodeId};
use stache_macos::ax::{AxError, AxPoint, AxSize, BoolAttribute, WindowString};
use stache_macos::windows::testing::FakeWindowSystem;
use stache_macos::windows::{AdapterOptions, RunLoopDriver, SystemWindowClock, WindowAdapter};
use stache_macos::{DisplayId, FakeFrameClockFactory};

use crate::backend::SystemWindowBackend;
use crate::test_support::FakeAppDirectory;

const D1: DisplayId = DisplayId::new(1);
static NAMES_B: AtomicUsize = AtomicUsize::new(0);

fn window_node(ax: &FakeWindowAx, cg: u32) -> NodeId {
    let mut node = FakeNode {
        position: Ok(AxPoint { x: 10.0, y: 20.0 }),
        size: Ok(AxSize { width: 300.0, height: 200.0 }),
        position_settable: Ok(true),
        size_settable: Ok(true),
        window_id: Ok(cg),
        ..FakeNode::default()
    };
    node.strings.insert(WindowString::Title, Ok(format!("window {cg}")));
    node.strings.insert(WindowString::Role, Ok("AXWindow".to_owned()));
    node.strings.insert(WindowString::Subrole, Ok("AXStandardWindow".to_owned()));
    node.bools.insert(BoolAttribute::Minimized, Ok(false));
    node.bools.insert(BoolAttribute::FullScreen, Ok(false));
    ax.add_node(node)
}

fn anim_config(outer: u32) -> Arc<TilingConfig> {
    Arc::new(
        serde_json::from_value(json!({
            "enabled": true,
            "gaps": { "inner": 0, "outer": outer },
            "animations": { "enabled": true, "duration": 200 },
            "borders": { "enabled": true },
        }))
        .expect("config"),
    )
}

/// The real tracking runtime (actor and effect threads, animation worker over a
/// fake frame clock, the sharded window adapter on a fake AX) with the native
/// borders composed over the logged fake overlay.
struct Anim {
    ax: FakeWindowAx,
    runtime: TrackingRuntime,
    clocks: FakeFrameClockFactory,
    nodes: Vec<NodeId>,
    overlay_log: Arc<Log>,
    fake: Arc<FakeOverlayBackend>,
    spaces: FakeSpaceEvents,
    revision: u64,
    /// Live follow over the fake notification session and its own fake clocks.
    session: stache_macos::border_overlay::FakeSessionControl,
    live_clocks: FakeFrameClockFactory,
}

impl Anim {
    fn boot() -> Self { Self::boot_with(false) }

    /// `live`: compose the native live follow over fakes (quiet period of a
    /// minute, so a scenario stops it explicitly).
    fn boot_with(live: bool) -> Self {
        let ax = FakeWindowAx::new();
        let nodes: Vec<NodeId> = (101..=102).map(|cg| window_node(&ax, cg)).collect();
        let element = ax.add_node(FakeNode {
            windows: Ok(nodes.clone()),
            enhanced_ui: Err(AxError::AttributeUnsupported),
            ..FakeNode::default()
        });
        let application = info(1, "com.test.app1", "App 1");
        ax.set_application(application.pid, element);
        let (system, _observers) = FakeWindowSystem::factory(ax.clone());
        let adapter = WindowAdapter::spawn(
            &format!(
                "stache-test-borders-anim-{}",
                NAMES_B.fetch_add(1, Ordering::SeqCst)
            ),
            &AdapterOptions::default(),
            Arc::new(SystemWindowClock),
            system,
            RunLoopDriver::new,
        )
        .expect("spawn");
        let backend = Arc::new(SystemWindowBackend::from_adapter(adapter, false));
        let directory = Arc::new(FakeAppDirectory::default());
        *directory.apps.lock().expect("apps") = vec![application];

        let exec = Arc::new(ManualExecutor::new(false));
        exec.set_on_main(true);
        let fake = Arc::new(FakeOverlayBackend::single_display());
        let (overlay_log, control) = Overlay::control(&exec, &fake, None, None);
        let clocks = FakeFrameClockFactory::new();
        let mut options = TrackingOptions::new("");
        options.frame_clocks = Some(Arc::new(clocks.clone()));
        options.overlay = Some(control);
        let spaces = FakeSpaceEvents::new();
        let factory_spaces = spaces.clone();
        options.space_events = Some(Arc::new(move || Box::new(factory_spaces.clone())));
        let session = stache_macos::border_overlay::FakeSessionControl::new();
        let live_clocks = FakeFrameClockFactory::new();
        if live {
            let (maker, clocks) = (session.maker(), live_clocks.clone());
            options.live_follow = Some(Arc::new(move || {
                Box::new(
                    stache_macos::border_overlay::SystemLiveFollow::new(
                        Arc::clone(&maker),
                        Arc::new(clocks.clone()) as Arc<dyn stache_macos::FrameClockFactory>,
                        None,
                    )
                    .with_timing(stache_macos::border_overlay::FollowTiming {
                        quiet: Duration::from_secs(60),
                        ..stache_macos::border_overlay::FollowTiming::default()
                    }),
                ) as Box<dyn stache_macos::border_overlay::LiveFollow>
            }));
        }
        let runtime = TrackingRuntime::start(
            backend as Arc<dyn WindowBackend>,
            directory as Arc<dyn crate::actor::AppDirectory>,
            &options,
        )
        .expect("start");
        runtime.submit_displays(None, one_display()).expect("displays");
        runtime.submit_config(1, anim_config(200)).expect("config");
        wait_until("enumeration", || {
            let snap = runtime.snapshot();
            snap.phase == Phase::Complete && snap.windows.len() == 2
        });
        assert!(runtime.authorize(1));
        let anim = Self {
            ax,
            runtime,
            clocks,
            nodes,
            overlay_log,
            fake,
            spaces,
            revision: 1,
            session,
            live_clocks,
        };
        anim.wait_settled();
        anim
    }

    fn settled(&self) -> bool {
        let snap = self.runtime.snapshot();
        let live: Vec<_> = snap.frames.iter().filter(|f| f.desired.is_some()).collect();
        live.len() == 2
            && live
                .iter()
                .all(|f| !f.in_flight && f.outcome.is_some() && f.actual == f.desired)
            && self.runtime.outstanding_frames() == 0
    }

    fn wait_settled(&self) { wait_until("settled layout", || self.settled()); }

    fn reload(&mut self, outer: u32) {
        self.revision += 1;
        self.runtime.submit_config(self.revision, anim_config(outer)).expect("config");
    }

    fn frame_of(&self, node: NodeId) -> Rect {
        let mut frame = None;
        self.ax.edit(node, |n| {
            let (Ok(p), Ok(s)) = (n.position, n.size) else { return };
            frame = Some(Rect::new(p.x, p.y, s.width, s.height));
        });
        frame.expect("a readable node")
    }

    fn positions_written(&self, node: NodeId) -> Vec<(f64, f64)> {
        self.ax
            .calls()
            .iter()
            .filter_map(|c| match c {
                Call::SetPosition(n, p) if *n == node => Some((p.x, p.y)),
                _ => None,
            })
            .collect()
    }

    fn window_of(&self, node: NodeId) -> WindowInstance {
        let mut cg = 0;
        self.ax.edit(node, |n| cg = n.window_id.unwrap_or(0));
        self.runtime
            .snapshot()
            .windows
            .iter()
            .find(|w| w.cg_window_id == Some(cg))
            .map(|w| w.token)
            .expect("tracked")
    }

    fn events(&self) -> Vec<Ev> { lock(&self.overlay_log.events).clone() }

    fn moves_of(&self, handle: OverlayHandle) -> Vec<OverlayRect> {
        self.events()
            .iter()
            .filter_map(|e| match e {
                Ev::Move(h, frame) if *h == handle => Some(*frame),
                _ => None,
            })
            .collect()
    }
}

impl Drop for Anim {
    fn drop(&mut self) { self.runtime.shutdown(); }
}

/// TB-04-03: during an animated layout change the border follows every frame
/// written to the window, and the settled frame; a stale plan's frames are
/// ignored.
#[test]
fn animated_layout_changes_are_followed_frame_by_frame_and_settle_exact() {
    let mut anim = Anim::boot();
    wait_until("both borders shown", || {
        anim.events().iter().filter(|e| matches!(e, Ev::Show { .. })).count() == 2
    });
    let node = anim.nodes[0];
    let window = anim.window_of(node);
    let handle = anim
        .events()
        .iter()
        .find_map(|e| match e {
            Ev::Show { handle, cg: 101, .. } => Some(*handle),
            _ => None,
        })
        .expect("W1 border");
    assert_eq!(anim.fake.live_overlays(), 2);
    assert!(
        anim.moves_of(handle).is_empty()
            || anim.moves_of(handle).last() == Some(&overlay_rect(anim.frame_of(node))),
        "the border starts at the window"
    );

    let writes_before = anim.positions_written(node).len();
    let moves_before = anim.moves_of(handle).len();
    anim.reload(0);
    wait_until("the animation clock", || anim.clocks.is_live(D1));
    wait_until("the animation to finish", || {
        anim.clocks.fire(D1, Instant::now());
        std::thread::sleep(Duration::from_millis(2));
        anim.settled() && anim.clocks.live() == 0
    });

    let written: Vec<(f64, f64)> = anim.positions_written(node)[writes_before..].to_vec();
    let moves: Vec<OverlayRect> = anim.moves_of(handle)[moves_before..].to_vec();
    assert!(written.len() >= 3, "the window was animated: {written:?}");
    for (x, y) in &written {
        assert!(
            moves.iter().any(|m| (m.x - x).abs() < 1e-6 && (m.y - y).abs() < 1e-6),
            "the border followed the frame written at ({x}, {y}): {moves:?}"
        );
    }
    let last = *moves.last().expect("moves");
    let final_frame = anim.frame_of(node);
    assert_eq!(
        last,
        overlay_rect(final_frame),
        "the border settles on the window"
    );

    // A stale plan's frame (older than the window's current plan) is ignored.
    let route = anim.runtime.border_route().expect("route");
    let before = route.counters();
    route.report(
        window,
        0,
        Rect::new(1.0, 2.0, 3.0, 4.0),
        BorderFrameKind::AnimationFrame,
    );
    let after = route.counters();
    assert_eq!(after.stale, before.stale + 1);
    assert_eq!(after.moves, before.moves);
    assert_eq!(anim.moves_of(handle).last().copied(), Some(last));
}

/// Stop: the borders stage releases every overlay within its bound and no
/// layout change shows one afterwards.
#[test]
fn stopping_the_runtime_releases_every_overlay() {
    let anim = Anim::boot();
    wait_until("both borders shown", || {
        anim.events().iter().filter(|e| matches!(e, Ev::Show { .. })).count() == 2
    });
    assert_eq!(anim.fake.live_overlays(), 2);
    assert!(anim.runtime.owes_borders_cleanup());
    // The module seals and wakes first; the actor then hands its owners over.
    anim.runtime.quiesce();
    assert_eq!(
        anim.runtime.stop_borders(Duration::from_secs(2), Duration::from_secs(2)),
        crate::tracking::BordersStop::Done
    );
    assert_eq!(anim.fake.live_overlays(), 0);
    assert_eq!(anim.fake.ordered_in(), 0);
    assert!(anim.runtime.border_route().expect("route").is_closed());
    let shows = anim.events().iter().filter(|e| matches!(e, Ev::Show { .. })).count();
    // The mailbox is closed with the actor: nothing can reach the engine, and
    // the route refuses a late frame report.
    assert!(anim.runtime.ingress().submit(Input::Relist).is_err());
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(
        anim.events().iter().filter(|e| matches!(e, Ev::Show { .. })).count(),
        shows,
        "nothing shows after the stop"
    );
    assert_eq!(anim.fake.live_overlays(), 0);
    // A second stop (a retry) is harmless.
    assert_eq!(
        anim.runtime.stop_borders(Duration::from_secs(2), Duration::from_secs(2)),
        crate::tracking::BordersStop::Done
    );
}

// ------------------------------------------------- part 05: Spaces scenarios

impl Desk {
    /// Commands after the `HideAll` that follows `name`, as the log shows them.
    fn after_hide_all(&self, name: &'static str) -> Vec<Ev> {
        let events = self.since(name);
        let at = Self::pos(&events, |e| *e == Ev::HideAll, "HideAll");
        events[at + 1..].to_vec()
    }

    fn shows(events: &[Ev]) -> Vec<u32> {
        events
            .iter()
            .filter_map(|e| match e {
                Ev::Show { cg, .. } => Some(*cg),
                _ => None,
            })
            .collect()
    }

    fn spaces(&self) -> &FakeSpaceEvents { &self.overlay.spaces }

    /// The Spaces diagnostics published in the border notes.
    fn messages(&self) -> Vec<String> {
        lock(&self.notes)
            .iter()
            .filter(|note| note.contains("Spaces or Mission Control"))
            .cloned()
            .collect()
    }
}

/// TB-05-03 and the hard rule: a workspace switch that reveals windows while
/// Mission Control is open shows nothing until it ends and one frame passes.
#[test]
fn a_reveal_during_mission_control_waits_for_the_exit() {
    let (mut desk, [_w1, _w2, _w3]) = Desk::started();
    assert_eq!(desk.overlay.fake.ordered_in(), 2);
    desk.mark("mc");
    desk.spaces().enter_mission_control();
    desk.rig.pump();
    assert_eq!(
        desk.overlay.fake.ordered_in(),
        0,
        "hidden in the turn the fact arrived"
    );
    assert_eq!(
        desk.since("mc").iter().filter(|e| **e == Ev::HideAll).count(),
        1,
        "one HideAll"
    );

    desk.workspace("b");
    desk.frame();
    desk.frame();
    desk.frame();
    let during = desk.after_hide_all("mc");
    assert!(
        Desk::shows(&during).is_empty(),
        "no Show while suspended: {during:#?}"
    );
    assert!(
        !during.iter().any(|e| matches!(e, Ev::Order(..))),
        "no Order while suspended: {during:#?}"
    );
    assert_eq!(desk.overlay.fake.ordered_in(), 0);

    desk.mark("exit");
    desk.spaces().exit_mission_control();
    desk.rig.pump();
    assert!(Desk::shows(&desk.since("exit")).is_empty(), "one frame first");
    desk.frame();
    desk.frame();
    let after = desk.since("exit");
    assert_eq!(
        Desk::shows(&after),
        vec![202],
        "the current state: only the revealed window"
    );
    assert_eq!(desk.overlay.fake.ordered_in(), 1);
}

/// A Space change that starts in the middle of a workspace switch.
#[test]
fn a_space_change_during_a_workspace_switch_shows_nothing_until_it_completes() {
    let (mut desk, [_w1, _w2, _w3]) = Desk::started();
    desk.mark("space");
    desk.spaces().begin_space_change(D1);
    desk.rig.pump();
    assert_eq!(desk.overlay.fake.ordered_in(), 0);
    desk.workspace("b");
    desk.frame();
    desk.frame();
    assert_eq!(desk.overlay.fake.ordered_in(), 0);
    assert!(Desk::shows(&desk.after_hide_all("space")).is_empty());

    desk.mark("done");
    desk.spaces().complete_space_change(D1);
    desk.rig.pump();
    desk.frame();
    desk.frame();
    assert_eq!(Desk::shows(&desk.since("done")), vec![202]);
    assert_eq!(desk.overlay.fake.ordered_in(), 1);
}

/// Leave for another Space and come back: nothing is drawn while away (the
/// windows are not on the active Space), and every border returns afterwards.
#[test]
fn a_space_switch_away_and_back_returns_every_border() {
    use std::sync::atomic::Ordering::SeqCst;
    let (mut desk, [_w1, _w2, _w3]) = Desk::started();
    desk.mark("away");
    desk.spaces().begin_space_change(D1);
    desk.rig.pump();
    desk.overlay.away.store(true, SeqCst);
    desk.spaces().complete_space_change(D1);
    desk.rig.pump();
    desk.frame();
    desk.frame();
    assert_eq!(
        desk.overlay.fake.ordered_in(),
        0,
        "on another Space nothing is drawn"
    );
    assert!(Desk::shows(&desk.after_hide_all("away")).is_empty());
    assert!(
        desk.messages().is_empty(),
        "a known inactive Space is not a diagnostic"
    );

    desk.mark("back");
    desk.overlay.away.store(false, SeqCst);
    desk.spaces().begin_space_change(D1);
    desk.spaces().complete_space_change(D1);
    desk.rig.pump();
    desk.frame();
    desk.frame();
    let mut shown = Desk::shows(&desk.since("back"));
    shown.sort_unstable();
    assert_eq!(shown, vec![101, 201]);
    assert_eq!(desk.overlay.fake.ordered_in(), 2);
}

/// A lost owner thread keeps every border hidden with one diagnostic.
#[test]
fn a_lost_owner_keeps_every_border_hidden_with_one_diagnostic() {
    let mut desk = Desk::new();
    desk.spaces().lose(UnknownCause::OwnerStopped);
    desk.rig.pump();
    let w1 = desk.window(0, 1);
    desk.focus(w1);
    desk.authorize();
    desk.frame();
    desk.frame();
    assert_eq!(desk.overlay.fake.ordered_in(), 0);
    assert!(Desk::shows(&desk.log()).is_empty());
    let messages = desk.messages();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("owner thread stopped"), "{messages:?}");
}

/// A running animation and Mission Control: no frame of the stream may bring a
/// hidden border back; the border returns at the settled frame afterwards.
#[test]
fn mission_control_during_an_animation_frame_stream_never_re_shows_a_border() {
    let mut anim = Anim::boot();
    wait_until("both borders shown", || {
        anim.events().iter().filter(|e| matches!(e, Ev::Show { .. })).count() == 2
    });
    let node = anim.nodes[0];
    anim.reload(0);
    wait_until("the animation clock", || anim.clocks.is_live(D1));
    for _ in 0..3 {
        anim.clocks.fire(D1, Instant::now());
        std::thread::sleep(Duration::from_millis(2));
    }

    anim.spaces.enter_mission_control();
    wait_until("every border hidden", || anim.fake.ordered_in() == 0);
    let mark = anim.events().len();
    wait_until("the animation to finish", || {
        anim.clocks.fire(D1, Instant::now());
        std::thread::sleep(Duration::from_millis(2));
        anim.settled() && anim.clocks.live() == 0
    });
    let during: Vec<Ev> = anim.events()[mark..].to_vec();
    assert!(
        !during.iter().any(|e| matches!(e, Ev::Show { .. } | Ev::Order(..))),
        "no frame brought a border back: {during:#?}"
    );
    assert_eq!(anim.fake.ordered_in(), 0);

    let shows_before = anim.events().iter().filter(|e| matches!(e, Ev::Show { .. })).count();
    anim.spaces.exit_mission_control();
    wait_until("borders return", || anim.fake.ordered_in() == 2);
    let events = anim.events();
    assert_eq!(
        events.iter().filter(|e| matches!(e, Ev::Show { .. })).count(),
        shows_before + 2
    );
    let last_show = events
        .iter()
        .rev()
        .find_map(|e| match e {
            Ev::Show { cg: 101, frame, .. } => Some(*frame),
            _ => None,
        })
        .expect("W1 show");
    assert_eq!(
        last_show,
        overlay_rect(anim.frame_of(node)),
        "at the settled frame"
    );
}

// ------------------------------------------------- live follow (part 06)

mod live {
    //! TB-06-02 and TB-06-03 end to end: a user drag with live follow (fake
    //! notifications and fake frame clocks over the real owner thread), the same
    //! drag with live follow unavailable (the public fallback), and a Tiling
    //! animation running while a live sample arrives.

    use stache_macos::border_overlay::{
        FakeLiveFollow, FakeSessionControl, FollowTiming, SystemLiveFollow, WindowEventKind as Ev2,
    };
    use stache_macos::windows::SnapshotOutcome;

    use super::*;
    use crate::drag::{DragPoint, DragSignal};
    use crate::state::DragOutcome;
    use crate::test_support::snapshot;

    const MISSING: &str = "missing symbol SLSGetWindowBounds";

    fn frame_read(rect: Rect) -> stache_macos::ax::FrameRead {
        stache_macos::ax::FrameRead {
            position: stache_macos::ax::AxReading::Value(AxPoint { x: rect.x, y: rect.y }),
            size: stache_macos::ax::AxReading::Value(AxSize {
                width: rect.width,
                height: rect.height,
            }),
        }
    }

    fn live_source(
        session: &FakeSessionControl,
        clocks: &FakeFrameClockFactory,
    ) -> Box<dyn stache_macos::border_overlay::LiveFollow> {
        Box::new(
            SystemLiveFollow::new(
                session.maker(),
                Arc::new(clocks.clone()) as Arc<dyn stache_macos::FrameClockFactory>,
                None,
            )
            .with_timing(FollowTiming {
                quiet: Duration::from_secs(60),
                ..FollowTiming::default()
            }),
        )
    }

    /// The frame Tiling last wrote to `window`.
    fn tile_of(desk: &Desk, window: WindowInstance) -> Rect {
        desk.rig
            .backend
            .written_frames()
            .iter()
            .rev()
            .find(|(w, _)| *w == window)
            .map(|(_, frame)| *frame)
            .expect("a write for the window")
    }

    fn signal(desk: &Desk, signal: DragSignal) {
        desk.rig.ingress.submit(Input::Drag(signal)).expect("drag signal");
    }

    fn down_at(frame: Rect) -> DragSignal {
        DragSignal::Down {
            button: 0,
            at: DragPoint {
                x: frame.x + frame.width / 2.0,
                y: frame.y + frame.height / 2.0,
            },
            epoch: 1,
            time: Duration::ZERO,
        }
    }

    fn up_at(frame: Rect) -> DragSignal {
        DragSignal::Up {
            button: 0,
            at: DragPoint {
                x: frame.x + frame.width / 2.0 + 30.0,
                y: frame.y + frame.height / 2.0,
            },
            epoch: 1,
            time: Duration::from_millis(300),
        }
    }

    /// The settled snapshot of a window nobody touched: its current frame.
    fn untouched(desk: &Desk, window: WindowInstance, cg: u32) {
        let frame = tile_of(desk, window);
        let mut still = snapshot(window, "w", cg);
        still.frame = frame_read(frame);
        desk.rig
            .backend
            .script_snapshot(window, SnapshotOutcome::Snapshot(Box::new(still)));
    }

    /// The user moved the window: the coalesced sample reads this frame.
    fn user_moved(desk: &mut Desk, window: WindowInstance, cg: u32, frame: Rect) {
        let mut moved = snapshot(window, "w", cg);
        moved.frame = frame_read(frame);
        desk.rig
            .backend
            .script_snapshot(window, SnapshotOutcome::Snapshot(Box::new(moved)));
        desk.rig.push(WindowEventKind::Window {
            window,
            change: WindowChange::Moved,
        });
        // The first frame admits the event, the second flushes its coalesced
        // geometry sample and reads the window.
        desk.frame();
        desk.frame();
    }

    fn moves_in(events: &[Ev], handle: OverlayHandle) -> Vec<OverlayRect> {
        events
            .iter()
            .filter_map(|e| match e {
                Ev::Move(h, frame) if *h == handle => Some(*frame),
                _ => None,
            })
            .collect()
    }

    /// TB-06-02: with live follow a user drag moves the border once per display
    /// tick from the latest bounds, the border never hides, the coalesced samples
    /// do not pull it back, and nothing reaches Tiling: no window write during
    /// the drag, the drag outcome is `drag.rs`'s alone.
    #[test]
    fn a_user_drag_is_followed_live_once_per_tick_without_touching_tiling() {
        let session = FakeSessionControl::new();
        let clocks = FakeFrameClockFactory::new();
        let (mut desk, [w1, w2, _w3]) = Desk::started_with(Some(live_source(&session, &clocks)));
        let cg1 = Desk::cg(0, 1);
        let h1 = desk.handle(cg1);
        untouched(&desk, w2, Desk::cg(1, 1));
        wait_until("live follow registered and subscribed", || {
            session.registered() && session.subscriptions().iter().any(|set| set.contains(&cg1))
        });
        assert!(
            session.pump_threads().iter().all(|n| n == "stache-tiling-borders-events"),
            "the run loop is pumped on the owner thread only"
        );
        assert!(
            lock(&desk.notes).iter().all(|m| !m.contains("live follow")),
            "an active live follow reports nothing"
        );
        let tile = tile_of(&desk, w1);
        let writes_before_drag = desk.rig.backend.written_frames().len();

        // Mouse down on the bordered window: the border stays and follows.
        desk.mark("down");
        signal(&desk, down_at(tile));
        desk.rig.pump();
        assert!(
            !desk.since("down").contains(&Ev::Hide(h1)),
            "no hide on the mouse down"
        );

        // The window server reports motion; the first event starts the clock and
        // reads the bounds at once, without waiting for a display tick.
        assert!(clocks.created().is_empty(), "idle owns no clock");
        let reads = |session: &FakeSessionControl| {
            session.bounds_reads().iter().filter(|id| **id == cg1).count()
        };
        let a = Rect::new(tile.x + 10.0, tile.y + 4.0, tile.width, tile.height);
        let b = Rect::new(tile.x + 22.0, tile.y + 9.0, tile.width, tile.height);
        session.set_bounds(cg1, overlay_rect(a));
        assert!(session.inject(Ev2::Move, cg1));
        wait_until("the display clock", || clocks.is_live(D1));
        wait_until("the first live move", || {
            moves_in(&desk.log(), h1).contains(&overlay_rect(a))
        });
        assert_eq!(reads(&session), 1, "read at once, no tick fired yet");
        // Later events and ticks inside the display period read at most once more
        // (at the end of the period), never twice.
        session.set_bounds(cg1, overlay_rect(b));
        assert!(session.inject(Ev2::Resize, cg1));
        assert!(clocks.fire(D1, Instant::now()));
        wait_until("the second live move", || {
            moves_in(&desk.log(), h1).last() == Some(&overlay_rect(b))
        });
        // Real time: an event that lands after the first period reads at once and
        // the tick then waits for the next period, so two or three reads.
        assert!(
            (2..=3).contains(&reads(&session)),
            "one bounds read per period, never AX: {}",
            reads(&session)
        );

        // The coalesced AX sample (older than the live bounds) neither hides the
        // border nor pulls it back.
        user_moved(
            &mut desk,
            w1,
            cg1,
            Rect::new(tile.x + 15.0, tile.y + 6.0, tile.width, tile.height),
        );
        let during = desk.since("down");
        assert!(!during.contains(&Ev::Hide(h1)));
        assert_eq!(moves_in(&during, h1).last(), Some(&overlay_rect(b)));

        // The user lets go; the settled snapshot decides (snap back) and Tiling
        // writes the tile again. The drag outcome is Tiling's, untouched by live
        // follow, and Tiling wrote nothing during the gesture.
        assert_eq!(
            desk.rig.backend.written_frames().len(),
            writes_before_drag,
            "live follow wrote no window and the planner was suppressed"
        );
        signal(&desk, up_at(tile));
        desk.rig.pump();
        desk.frame();
        let snap = desk.rig.snap();
        assert_eq!(
            snap.drag.last.clone().expect("record").outcome,
            DragOutcome::SnappedBack
        );
        assert_eq!(snap.diagnostics.drag_snap_backs, 1);
        wait_until("the border back on the tile", || {
            moves_in(&desk.log(), h1).last() == Some(&overlay_rect(tile))
        });
        assert!(!desk.log().contains(&Ev::Hide(h1)), "the border never hid");

        // Disabling borders stops the owner and the clock before the release.
        desk.reload(&json!({ "enabled": false }), 0);
        assert_eq!(clocks.live(), 0, "no clock outlives the engine");
        assert!(!session.registered(), "the registrations were removed");
        assert!(!session.inject(Ev2::Move, cg1), "no callback after stop");
    }

    /// TB-06-03: the same drag without live follow. The border hides at the
    /// mouse down, stays hidden through the motion and returns only at the
    /// settled frame after the gesture ended.
    #[test]
    fn the_same_drag_without_live_follow_hides_and_shows_at_the_settled_frame() {
        let live = FakeLiveFollow::unavailable(MISSING);
        let (mut desk, [w1, w2, _w3]) = Desk::started_with(Some(Box::new(live)));
        let (cg1, cg2) = (Desk::cg(0, 1), Desk::cg(1, 1));
        let (h1, h2) = (desk.handle(cg1), desk.handle(cg2));
        untouched(&desk, w2, Desk::cg(1, 1));
        let tile = tile_of(&desk, w1);
        let diagnostic = stache_macos::border_overlay::unavailable_message(MISSING);
        assert_eq!(
            lock(&desk.notes).iter().filter(|m| **m == diagnostic).count(),
            1,
            "exactly one diagnostic: {:?}",
            lock(&desk.notes)
        );

        desk.mark("down");
        signal(&desk, down_at(tile));
        desk.rig.pump();
        let at_down = desk.since("down");
        assert!(
            !at_down.contains(&Ev::Hide(h1)),
            "a mouse down alone never hides (no flicker on a click): {at_down:#?}"
        );

        // The window moves: hidden from the first motion, and no border chases it.
        for step in 1..=4 {
            user_moved(
                &mut desk,
                w1,
                cg1,
                Rect::new(tile.x + 10.0 * f64::from(step), tile.y, tile.width, tile.height),
            );
        }
        let moving = desk.since("down");
        assert!(
            moving.contains(&Ev::Hide(h1)),
            "hidden once it moved: {moving:#?}"
        );
        assert!(!moving.contains(&Ev::Hide(h2)), "only the dragged window");
        assert!(
            moves_in(&moving, h1).is_empty(),
            "no lagging border: {moving:#?}"
        );
        assert!(DeskShows::none(&moving, cg1));

        // Mouse up; the settled snapshot snaps the window back; the border waits.
        desk.mark("up");
        signal(&desk, up_at(tile));
        desk.rig.pump();
        assert!(DeskShows::none(&desk.since("up"), cg1), "not at the mouse up");
        for _ in 0..12 {
            desk.frame();
        }
        let snap = desk.rig.snap();
        assert_eq!(
            snap.drag.last.clone().expect("record").outcome,
            DragOutcome::SnappedBack
        );
        let after = desk.since("down");
        let hide = Desk::pos(&after, |e| *e == Ev::Hide(h1), "the hide");
        let write = Desk::pos(
            &after,
            |e| matches!(e, Ev::Write(w) if *w == w1),
            "the snap-back",
        );
        let show = Desk::pos(
            &after,
            |e| matches!(e, Ev::Show { cg, .. } if *cg == cg1),
            "the show",
        );
        assert!(
            hide < write && write < show,
            "hide, then Tiling's write, then show: {after:#?}"
        );
        let shown = Desk::shows_in(&after, cg1);
        assert_eq!(shown.len(), 1);
        assert!(
            matches!(shown[0], Ev::Show { frame, .. } if *frame == overlay_rect(tile)),
            "at the settled frame: {shown:?}"
        );
    }

    /// TB-06-03: an external move of a window Tiling does not manage by a gesture
    /// (a floating window) hides the border and shows it again once the window
    /// stopped.
    #[test]
    fn an_external_move_without_a_gesture_hides_until_the_window_settles() {
        let live = FakeLiveFollow::unavailable(MISSING);
        let (mut desk, [w1, _w2, _w3]) = Desk::started_with(Some(Box::new(live)));
        let cg1 = Desk::cg(0, 1);
        let h1 = desk.handle(cg1);
        // A floating window: Tiling never writes it again, so nothing explains a move.
        desk.intent(Intent::ToggleFloating);
        for _ in 0..3 {
            desk.frame();
        }
        let tile = tile_of(&desk, w1);
        desk.mark("moved");
        user_moved(
            &mut desk,
            w1,
            cg1,
            Rect::new(tile.x + 40.0, tile.y + 8.0, tile.width, tile.height),
        );
        let first = desk.since("moved");
        assert!(first.contains(&Ev::Hide(h1)), "{first:#?}");
        assert!(moves_in(&first, h1).is_empty());
        for _ in 0..8 {
            desk.frame();
        }
        let later = desk.since("moved");
        let shown = Desk::shows_in(&later, cg1);
        assert_eq!(shown.len(), 1, "back after the window stopped: {later:#?}");
        assert!(matches!(
            shown[0],
            Ev::Show { frame, .. }
                if *frame == overlay_rect(Rect::new(tile.x + 40.0, tile.y + 8.0, tile.width, tile.height))
        ));
    }

    struct DeskShows;

    impl DeskShows {
        fn none(events: &[Ev], cg: u32) -> bool { Desk::shows_in(events, cg).is_empty() }
    }

    /// TB-06-02: a Tiling animation in flight owns the window. A live sample
    /// that arrives meanwhile is never read, so the border follows exactly the
    /// TB-07-02 end to end: borders enabled, windows bordered and a drag in
    /// progress (live follow owns a running display clock). The stop (seal, then
    /// the borders stage, as the module runs it) stops the owners first, then
    /// releases every overlay on main and the ledger matches the acknowledgement;
    /// nothing shows afterwards and the stage is clean.
    #[test]
    fn stopping_during_a_drag_stops_the_owners_before_the_release_and_nothing_shows_afterwards() {
        let anim = Anim::boot_with(true);
        wait_until("both borders shown", || {
            anim.events().iter().filter(|e| matches!(e, Ev::Show { .. })).count() == 2
        });
        let cg = 101;
        wait_until("live follow subscribed", || {
            anim.session.registered()
                && anim.session.subscriptions().iter().any(|set| set.contains(&cg))
        });
        anim.session.set_bounds(cg, OverlayRect::new(10.0, 10.0, 300.0, 200.0));
        assert!(anim.session.inject(Ev2::Move, cg));
        wait_until("the drag's display clock", || anim.live_clocks.is_live(D1));
        assert_eq!(anim.fake.live_overlays(), 2);

        anim.runtime.quiesce();
        let stop = anim.runtime.stop_borders(Duration::from_secs(2), Duration::from_secs(2));
        assert_eq!(stop, crate::tracking::BordersStop::Done);

        assert!(!anim.live_clocks.is_live(D1), "the owner stopped its clock");
        assert!(!anim.session.registered(), "the owner unregistered");
        assert_eq!((anim.fake.live_overlays(), anim.fake.pool_len()), (0, 0));
        assert_eq!(anim.fake.ordered_in(), 0);
        let events = anim.events();
        let released = events.iter().filter(|e| matches!(e, Ev::ReleaseAll)).count();
        assert_eq!(released, 1, "one release of everything");
        let at = events.iter().position(|e| matches!(e, Ev::ReleaseAll)).expect("release");
        assert!(
            events[at + 1..]
                .iter()
                .all(|e| !matches!(e, Ev::Show { .. } | Ev::Move(..) | Ev::Order(..))),
            "nothing shows or moves after the release: {:?}",
            &events[at + 1..]
        );
        assert!(
            !anim.live_clocks.fire(D1, Instant::now()),
            "no clock left to fire"
        );
        // A retry is harmless and still clean.
        assert_eq!(
            anim.runtime.stop_borders(Duration::from_secs(2), Duration::from_secs(2)),
            crate::tracking::BordersStop::Done
        );
    }

    /// frames Tiling writes; live follow resumes once the animation settled.
    #[test]
    fn a_tiling_animation_supersedes_live_samples_until_it_settles() {
        let mut anim = Anim::boot_with(true);
        wait_until("both borders shown", || {
            anim.events().iter().filter(|e| matches!(e, Ev::Show { .. })).count() == 2
        });
        let (node, cg) = (anim.nodes[0], 101);
        let handle = anim
            .events()
            .iter()
            .find_map(|e| match e {
                Ev::Show { handle, cg: 101, .. } => Some(*handle),
                _ => None,
            })
            .expect("W1 border");
        wait_until("live follow subscribed", || {
            anim.session.registered()
                && anim.session.subscriptions().iter().any(|set| set.contains(&cg))
        });
        // A bogus "user" position the window server would report for W1.
        let user = OverlayRect::new(777.0, 333.0, 400.0, 300.0);
        anim.session.set_bounds(cg, user);
        let reads =
            |anim: &Anim| anim.session.bounds_reads().iter().filter(|id| **id == cg).count();

        anim.reload(0);
        wait_until("the animation clock", || anim.clocks.is_live(D1));
        assert!(anim.session.inject(Ev2::Move, cg));
        wait_until("the live clock", || anim.live_clocks.is_live(D1));
        for _ in 0..5 {
            anim.live_clocks.fire(D1, Instant::now());
            anim.clocks.fire(D1, Instant::now());
            std::thread::sleep(Duration::from_millis(4));
        }
        assert_eq!(
            reads(&anim),
            0,
            "no live read while Tiling's animation is in flight"
        );
        assert!(
            !anim.moves_of(handle).contains(&user),
            "no live sample moved the border"
        );

        wait_until("the animation to finish", || {
            anim.clocks.fire(D1, Instant::now());
            std::thread::sleep(Duration::from_millis(2));
            anim.settled() && anim.clocks.live() == 0
        });
        let final_frame = overlay_rect(anim.frame_of(node));
        wait_until("the border settled", || {
            anim.moves_of(handle).last() == Some(&final_frame)
        });
        assert!(!anim.moves_of(handle).contains(&user));

        // The animation settled: a live sample is read and applied again.
        anim.session.set_bounds(cg, user);
        assert!(anim.session.inject(Ev2::Move, cg));
        wait_until("live follow resumed", || {
            anim.live_clocks.fire(D1, Instant::now());
            std::thread::sleep(Duration::from_millis(2));
            anim.moves_of(handle).last() == Some(&user)
        });
        assert!(reads(&anim) >= 1);
    }

    fn click_at(frame: Rect) -> [DragSignal; 2] {
        let at = DragPoint {
            x: frame.x + frame.width / 2.0,
            y: frame.y + frame.height / 2.0,
        };
        [
            DragSignal::Down {
                button: 0,
                at,
                epoch: 1,
                time: Duration::ZERO,
            },
            DragSignal::Up {
                button: 0,
                at,
                epoch: 1,
                time: Duration::from_millis(80),
            },
        ]
    }

    fn orders_of(events: &[Ev], handle: OverlayHandle) -> usize {
        events.iter().filter(|e| matches!(e, Ev::Order(h, _) if *h == handle)).count()
    }

    /// Native run 15: a click inside the already-focused window (no Tiling
    /// focus change) re-orders its `above` overlay once at the mouse up and
    /// once after the settle; nothing hides or orders out. The application's
    /// activation without a focus change re-asserts the same way. `below`
    /// re-asserts nothing.
    #[test]
    fn a_click_in_the_focused_window_reorders_its_above_overlay() {
        let (mut desk, [w1, w2, _w3]) = Desk::started();
        let cg1 = Desk::cg(0, 1);
        let h1 = desk.handle(cg1);
        let tile = tile_of(&desk, w1);

        desk.reload(&json!({ "enabled": true, "order": "above" }), 0);
        for _ in 0..10 {
            desk.frame();
        }
        // The snapshots the up queues read the windows where they are.
        untouched(&desk, w1, cg1);
        untouched(&desk, w2, Desk::cg(1, 1));
        desk.mark("click");
        let [down, up] = click_at(tile);
        signal(&desk, down);
        desk.rig.pump();
        assert_eq!(orders_of(&desk.since("click"), h1), 0, "nothing on the down");
        signal(&desk, up);
        desk.rig.pump();
        let after = desk.since("click");
        assert!(
            after.contains(&Ev::Order(h1, cg1)),
            "re-ordered at the up: {after:#?}"
        );
        assert_eq!(orders_of(&after, h1), 1);
        for _ in 0..10 {
            desk.frame();
        }
        let after = desk.since("click");
        assert_eq!(orders_of(&after, h1), 2, "one settle order: {after:#?}");
        assert!(
            !after.iter().any(|e| matches!(e, Ev::Hide(_) | Ev::Release(_))),
            "{after:#?}"
        );

        desk.mark("activation");
        desk.rig.ingress.submit(Input::Activated(w1.application())).expect("activated");
        desk.rig.pump();
        for _ in 0..10 {
            desk.frame();
        }
        let after = desk.since("activation");
        assert_eq!(orders_of(&after, h1), 2, "activation re-asserts: {after:#?}");
        assert!(!after.iter().any(|e| matches!(e, Ev::Hide(_) | Ev::Release(_))));
    }

    /// With live follow active the timers are gone: a focus change orders the
    /// new focus once (no settle), a click or an activation inside the focused
    /// window orders nothing, and the window server's reorder notification for
    /// the window (its own raise) puts the `above` border back on top, once.
    #[test]
    fn with_live_follow_active_above_is_kept_by_the_reorder_event_not_timers() {
        let live = FakeLiveFollow::active();
        let (mut desk, [w1, w2, _w3]) = Desk::started_with(Some(Box::new(live.clone())));
        let (cg1, cg2) = (Desk::cg(0, 1), Desk::cg(1, 1));
        let (h1, h2) = (desk.handle(cg1), desk.handle(cg2));
        let tile = tile_of(&desk, w1);
        desk.reload(&json!({ "enabled": true, "order": "above" }), 0);
        for _ in 0..10 {
            desk.frame();
        }

        desk.mark("focus");
        desk.focus(w2);
        for _ in 0..20 {
            desk.frame();
        }
        let after = desk.since("focus");
        assert_eq!(orders_of(&after, h2), 1, "one order, no settle: {after:#?}");
        desk.focus(w1);
        for _ in 0..10 {
            desk.frame();
        }

        untouched(&desk, w1, cg1);
        untouched(&desk, w2, cg2);
        desk.mark("click");
        let [down, up] = click_at(tile);
        signal(&desk, down);
        desk.rig.pump();
        signal(&desk, up);
        desk.rig.pump();
        desk.rig.ingress.submit(Input::Activated(w1.application())).expect("activated");
        desk.rig.pump();
        for _ in 0..20 {
            desk.frame();
        }
        let after = desk.since("click");
        assert_eq!(orders_of(&after, h1), 0, "no timer re-order: {after:#?}");

        desk.mark("reorder");
        live.sink().expect("sink").reorder(w1);
        desk.rig.pump();
        for _ in 0..10 {
            desk.frame();
        }
        let after = desk.since("reorder");
        assert_eq!(orders_of(&after, h1), 1, "restored above once: {after:#?}");
        assert!(after.contains(&Ev::Order(h1, cg1)));
        assert!(!after.iter().any(|e| matches!(e, Ev::Hide(_) | Ev::Release(_))));
    }
}

// ------------------------------------------------- native fullscreen enter/exit

/// A read of W1: in native fullscreen (the whole display) or back in a window.
fn w1_read(window: WindowInstance, fullscreen: bool) -> stache_macos::windows::WindowSnapshot {
    let mut read = crate::test_support::snapshot(window, "w", Desk::cg(0, 1));
    read.fullscreen = stache_macos::ax::AxReading::Value(fullscreen);
    if fullscreen {
        read.frame.position =
            stache_macos::ax::AxReading::Value(stache_macos::ax::AxPoint { x: 0.0, y: 0.0 });
        read.frame.size = stache_macos::ax::AxReading::Value(stache_macos::ax::AxSize {
            width: 1920.0,
            height: 1080.0,
        });
    }
    read
}

/// A fullscreen window loses its border; when it leaves fullscreen and no
/// notification reports the flag flipping, the re-read on the Space change of
/// the exit clears it, the window is written back into the layout and its border returns after
/// one display frame through the show rule.
#[test]
fn a_window_leaving_fullscreen_is_laid_out_again_and_its_border_returns() {
    let (mut desk, [w1, _w2, _w3]) = Desk::started();
    let h1 = desk.handle(Desk::cg(0, 1));
    desk.rig
        .backend
        .script_snapshot(w1, SnapshotOutcome::Snapshot(Box::new(w1_read(w1, true))));
    desk.mark("enter");
    desk.rig.push(WindowEventKind::Window {
        window: w1,
        change: WindowChange::Resized,
    });
    desk.rig.pump();
    desk.frame();
    let entered = desk.since("enter");
    assert!(desk.rig.snap().window(w1).expect("w1").observed.fullscreen);
    assert!(entered.contains(&Ev::Hide(h1)), "the border hides: {entered:#?}");
    assert!(
        Desk::shows_in(&entered, Desk::cg(0, 1)).is_empty(),
        "{entered:#?}"
    );

    // The exit's resize still reads fullscreen; the flag flips afterwards with
    // no further notification.
    desk.rig.push(WindowEventKind::Window {
        window: w1,
        change: WindowChange::Resized,
    });
    desk.rig.pump();
    desk.frame();
    assert!(desk.rig.snap().window(w1).expect("w1").observed.fullscreen);
    desk.rig
        .backend
        .script_snapshot(w1, SnapshotOutcome::Snapshot(Box::new(w1_read(w1, false))));
    desk.mark("exit");
    space_changed(&mut desk.rig);
    assert!(!desk.rig.snap().window(w1).expect("w1").observed.fullscreen);
    let exited = desk.since("exit");
    let write = Desk::pos(&exited, |e| *e == Ev::Write(w1), "the layout write of W1");
    assert!(
        Desk::shows_in(&exited, Desk::cg(0, 1)).is_empty(),
        "no border before a display frame: {exited:#?}"
    );
    desk.frame();
    desk.frame();
    let exited = desk.since("exit");
    let show = Desk::pos(
        &exited,
        |e| matches!(e, Ev::Show { cg, .. } if *cg == Desk::cg(0, 1)),
        "the border of W1",
    );
    assert!(write < show, "written before the border returns: {exited:#?}");
}

/// The active Space changed (the workspace notification names no application).
fn space_changed(rig: &mut Rig) {
    rig.ingress
        .submit(crate::state::Input::Lifecycle {
            kind: stache_macos::applications::LifecycleKind::SpaceChanged,
            info: crate::state::AppInfo {
                application: crate::test_support::app(999),
                pid: 0,
                bundle_id: String::new(),
                name: String::new(),
            },
        })
        .expect("space change");
    rig.pump();
}

/// An application that unlists its window in native fullscreen (Ghostty): the
/// slot is released and the border goes; on the exit the Space change lists the
/// application again, the window returns to its slot, is written, and its
/// border returns after one display frame.
#[test]
fn a_window_unlisted_by_fullscreen_returns_with_its_border_after_the_space_change() {
    let (mut desk, [w1, _w2, _w3]) = Desk::started();
    let h1 = desk.handle(Desk::cg(0, 1));
    let slot = desk
        .rig
        .snap()
        .frames
        .iter()
        .find(|status| status.window == w1)
        .and_then(|status| status.actual.or(status.desired))
        .expect("frame");
    desk.mark("enter");
    desk.rig.push(WindowEventKind::Window {
        window: w1,
        change: WindowChange::Suspended,
    });
    desk.rig.pump();
    desk.rig.clock.advance(crate::actor::TAB_SETTLE_DELAY);
    desk.rig.engine.turn();
    desk.rig.pump();
    desk.rig.complete(w1.application());
    desk.rig.pump();
    desk.frame();
    let entered = desk.since("enter");
    assert!(desk.rig.snap().window(w1).is_some_and(|w| w.suspended));
    assert_eq!(desk.rig.snap().diagnostics.tab_slots_released, 1);
    assert!(
        entered.contains(&Ev::Hide(h1)) || entered.contains(&Ev::Release(h1)),
        "the border goes: {entered:#?}"
    );

    desk.mark("exit");
    space_changed(&mut desk.rig);
    assert!(
        desk.rig
            .calls()
            .iter()
            .any(|call| matches!(call, FakeBackendCall::Refresh(a) if *a == w1.application())),
        "the application is listed again"
    );
    let mut read = w1_read(w1, false);
    read.frame.position =
        stache_macos::ax::AxReading::Value(stache_macos::ax::AxPoint { x: slot.x, y: slot.y });
    desk.rig.push(WindowEventKind::Snapshot {
        snapshot: read,
        origin: stache_macos::windows::SnapshotOrigin::Enumeration,
    });
    desk.rig.pump();
    desk.rig.complete(w1.application());
    desk.rig.pump();
    assert!(!desk.rig.snap().window(w1).expect("w1").suspended);
    let exited = desk.since("exit");
    let write = Desk::pos(&exited, |e| *e == Ev::Write(w1), "the layout write of W1");
    desk.frame();
    desk.frame();
    let exited = desk.since("exit");
    let show = Desk::pos(
        &exited,
        |e| matches!(e, Ev::Show { cg, .. } if *cg == Desk::cg(0, 1)),
        "the border of W1",
    );
    assert!(write < show, "written before the border returns: {exited:#?}");
}

/// Native runs 4-7 (Ghostty): the window is unlisted on fullscreen entry while
/// it reads windowed and the entry series runs out; on the exit Space change it
/// rejoins `AXWindows` only on the third retry. It returns to its slot, is
/// written and its border shows.
#[test]
fn a_spent_unlisted_window_returns_with_its_border_on_the_exit_series() {
    let (mut desk, [w1, _w2, _w3]) = Desk::started();
    let slot = desk
        .rig
        .snap()
        .frames
        .iter()
        .find(|status| status.window == w1)
        .and_then(|status| status.actual.or(status.desired))
        .expect("frame");
    desk.rig.push(WindowEventKind::Window {
        window: w1,
        change: WindowChange::Suspended,
    });
    desk.rig.pump();
    let settle = |desk: &mut Desk| {
        desk.rig.clock.advance(crate::actor::TAB_SETTLE_DELAY);
        desk.rig.engine.turn();
        desk.rig.pump();
        desk.rig.complete(w1.application());
        desk.rig.pump();
    };
    settle(&mut desk);
    assert!(desk.rig.snap().window(w1).is_some_and(|w| w.suspended));
    let refreshes = |desk: &Desk| {
        desk.rig
            .calls()
            .iter()
            .filter(|call| matches!(call, FakeBackendCall::Refresh(a) if *a == w1.application()))
            .count()
    };
    // Entry Space change: the whole series runs out.
    space_changed(&mut desk.rig);
    desk.rig.complete(w1.application());
    desk.rig.pump();
    for _ in 0..usize::from(crate::state::MAX_FULLSCREEN_RECHECKS) + 4 {
        settle(&mut desk);
    }
    let spent = refreshes(&desk);
    settle(&mut desk);
    assert_eq!(refreshes(&desk), spent, "the entry series ran out");

    desk.mark("exit");
    space_changed(&mut desk.rig);
    desk.rig.complete(w1.application());
    desk.rig.pump();
    for _ in 0..2 {
        settle(&mut desk);
    }
    desk.rig.clock.advance(crate::actor::TAB_SETTLE_DELAY);
    desk.rig.engine.turn();
    desk.rig.pump();
    assert_eq!(refreshes(&desk), spent + 4, "the third retry");
    let mut read = w1_read(w1, false);
    read.frame.position =
        stache_macos::ax::AxReading::Value(stache_macos::ax::AxPoint { x: slot.x, y: slot.y });
    desk.rig.push(WindowEventKind::Snapshot {
        snapshot: read,
        origin: stache_macos::windows::SnapshotOrigin::Enumeration,
    });
    desk.rig.pump();
    desk.rig.complete(w1.application());
    desk.rig.pump();
    assert!(!desk.rig.snap().window(w1).expect("w1").suspended);
    desk.frame();
    desk.frame();
    let exited = desk.since("exit");
    let write = Desk::pos(&exited, |e| *e == Ev::Write(w1), "the layout write of W1");
    let show = Desk::pos(
        &exited,
        |e| matches!(e, Ev::Show { cg, .. } if *cg == Desk::cg(0, 1)),
        "the border of W1",
    );
    assert!(write < show, "written before the border returns: {exited:#?}");
}

/// Native run 6: the listing on the exit's Space change finds the window again
/// while it still reads fullscreen; no notification follows. The settle listing
/// reads it again, windowed: it is written to its tile and its border returns.
#[test]
fn a_window_listed_again_while_still_fullscreen_returns_with_its_border_after_a_reread() {
    let (mut desk, [w1, _w2, _w3]) = Desk::started();
    desk.rig.push(WindowEventKind::Window {
        window: w1,
        change: WindowChange::Suspended,
    });
    desk.rig.pump();
    desk.rig.clock.advance(crate::actor::TAB_SETTLE_DELAY);
    desk.rig.engine.turn();
    desk.rig.pump();
    desk.rig.complete(w1.application());
    desk.rig.pump();
    desk.frame();
    assert_eq!(desk.rig.snap().diagnostics.tab_slots_released, 1);

    desk.mark("exit");
    space_changed(&mut desk.rig);
    desk.rig.push(WindowEventKind::Snapshot {
        snapshot: w1_read(w1, true),
        origin: stache_macos::windows::SnapshotOrigin::Enumeration,
    });
    desk.rig.pump();
    desk.rig.complete(w1.application());
    desk.rig.pump();
    desk.frame();
    desk.frame();
    assert!(!desk.rig.snap().window(w1).expect("w1").suspended);
    let exited = desk.since("exit");
    assert!(!exited.contains(&Ev::Write(w1)), "not written while fullscreen");
    assert!(
        Desk::shows_in(&exited, Desk::cg(0, 1)).is_empty(),
        "no border while fullscreen: {exited:#?}"
    );

    // The settle listing reads it windowed.
    let refreshes = |desk: &Desk| {
        desk.rig
            .calls()
            .iter()
            .filter(|c| matches!(c, FakeBackendCall::Refresh(a) if *a == w1.application()))
            .count()
    };
    let before = refreshes(&desk);
    desk.rig.clock.advance(crate::actor::TAB_SETTLE_DELAY);
    desk.rig.engine.turn();
    desk.rig.pump();
    assert_eq!(refreshes(&desk), before + 1, "listed again after the settle");
    desk.rig.push(WindowEventKind::Snapshot {
        snapshot: w1_read(w1, false),
        origin: stache_macos::windows::SnapshotOrigin::Enumeration,
    });
    desk.rig.pump();
    desk.rig.complete(w1.application());
    desk.rig.pump();
    desk.frame();
    desk.frame();
    let exited = desk.since("exit");
    let write = Desk::pos(&exited, |e| *e == Ev::Write(w1), "the layout write of W1");
    let show = Desk::pos(
        &exited,
        |e| matches!(e, Ev::Show { cg, .. } if *cg == Desk::cg(0, 1)),
        "the border of W1",
    );
    assert!(write < show, "written before the border returns: {exited:#?}");
}

/// Native run 7: the exit's Space change lists the application before the
/// window rejoined `AXWindows`; a settle listing finds it later, it is written
/// to its tile and its border returns after one display frame.
#[test]
fn a_window_missing_from_the_space_change_listing_returns_with_its_border_later() {
    let (mut desk, [w1, _w2, _w3]) = Desk::started();
    desk.rig.push(WindowEventKind::Window {
        window: w1,
        change: WindowChange::Suspended,
    });
    desk.rig.pump();
    desk.rig.clock.advance(crate::actor::TAB_SETTLE_DELAY);
    desk.rig.engine.turn();
    desk.rig.pump();
    desk.rig.complete(w1.application());
    desk.rig.pump();
    desk.frame();
    assert_eq!(desk.rig.snap().diagnostics.tab_slots_released, 1);

    desk.mark("exit");
    space_changed(&mut desk.rig);
    // The immediate listing lacks the window.
    desk.rig.complete(w1.application());
    desk.rig.pump();
    assert!(desk.rig.snap().window(w1).expect("w1").suspended);
    let refreshes = |desk: &Desk| {
        desk.rig
            .calls()
            .iter()
            .filter(|c| matches!(c, FakeBackendCall::Refresh(a) if *a == w1.application()))
            .count()
    };
    let before = refreshes(&desk);
    desk.rig.clock.advance(crate::actor::TAB_SETTLE_DELAY);
    desk.rig.engine.turn();
    desk.rig.pump();
    assert_eq!(refreshes(&desk), before + 1, "listed again after the settle");
    desk.rig.push(WindowEventKind::Snapshot {
        snapshot: w1_read(w1, false),
        origin: stache_macos::windows::SnapshotOrigin::Enumeration,
    });
    desk.rig.pump();
    desk.rig.complete(w1.application());
    desk.rig.pump();
    assert!(!desk.rig.snap().window(w1).expect("w1").suspended);
    let exited = desk.since("exit");
    let write = Desk::pos(&exited, |e| *e == Ev::Write(w1), "the layout write of W1");
    assert!(
        Desk::shows_in(&exited, Desk::cg(0, 1)).is_empty(),
        "no border before a display frame: {exited:#?}"
    );
    desk.frame();
    desk.frame();
    let exited = desk.since("exit");
    let show = Desk::pos(
        &exited,
        |e| matches!(e, Ev::Show { cg, .. } if *cg == Desk::cg(0, 1)),
        "the border of W1",
    );
    assert!(write < show, "written before the border returns: {exited:#?}");
}

/// Native run 8 (`unfocused = false`): the frontmost application's window
/// flickers in `AXWindows` while fullscreen and its slot is released; listed
/// windowed by the exit's Space change, it is laid out and, focused again
/// without any activation, its focused border returns.
#[test]
fn native_run8_a_returned_window_of_the_frontmost_application_gets_its_focused_border_back() {
    let (mut desk, [w1, _w2, _w3]) = Desk::started();
    desk.reload(&json!({ "enabled": true, "unfocused": false }), 0);
    desk.rig.backend.script_focused(w1.application(), w1);
    desk.rig.ingress.submit(Input::Activated(w1.application())).expect("activated");
    desk.rig.pump();
    desk.frame();
    let release = |desk: &mut Desk| {
        desk.rig.push(WindowEventKind::Window {
            window: w1,
            change: WindowChange::Suspended,
        });
        desk.rig.pump();
        desk.rig.clock.advance(crate::actor::TAB_SETTLE_DELAY);
        desk.rig.engine.turn();
        desk.rig.pump();
        desk.rig.complete(w1.application());
        desk.rig.pump();
        desk.frame();
    };
    let listed = |desk: &mut Desk, fullscreen: bool| {
        desk.rig.push(WindowEventKind::Snapshot {
            snapshot: w1_read(w1, fullscreen),
            origin: stache_macos::windows::SnapshotOrigin::Enumeration,
        });
        desk.rig.pump();
        desk.rig.complete(w1.application());
        desk.rig.pump();
        desk.frame();
    };
    release(&mut desk);
    // Entry: listed while fullscreen, then unlisted again.
    space_changed(&mut desk.rig);
    listed(&mut desk, true);
    release(&mut desk);
    assert_eq!(desk.rig.snap().diagnostics.tab_slots_released, 2);

    desk.mark("exit");
    space_changed(&mut desk.rig);
    listed(&mut desk, false);
    desk.frame();
    desk.frame();
    assert!(!desk.rig.snap().window(w1).expect("w1").suspended);
    assert_eq!(desk.rig.snap().focus.window, Some(w1), "focused again");
    let exited = desk.since("exit");
    let show = Desk::shows_in(&exited, Desk::cg(0, 1));
    assert!(!show.is_empty(), "the border of W1 returns: {exited:#?}");
    let Ev::Show { appearance, .. } = show[show.len() - 1] else {
        unreachable!()
    };
    assert!(is_focused_color(appearance), "in the focused color");
}
