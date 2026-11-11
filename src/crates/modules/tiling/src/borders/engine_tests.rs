//! Engine and route tests over fakes: `OverlayControl` over
//! `FakeOverlayBackend` and `ManualExecutor`, an injected clock, fake frame
//! events. No real window, overlay, display link or `SkyLight` is created.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use stache_macos::border_overlay::testing::{FakeEvent, FakeOverlayBackend, FakeSpaceEvents};
use stache_macos::border_overlay::{
    AppearanceChange, BorderAppearance, BorderPlacement, HideReason, KEY_PING_PONG, KEY_ROTATE,
    MAX_OVERLAYS, OverlayBackend, OverlayCapabilities, OverlayControl, OverlayKind, UnknownCause,
};
use stache_macos::windows::WindowInstance;
use stache_macos::{ApplicationId, ManualExecutor};

use super::appearance::{FocusInput, border_appearance};
use super::engine::{ActiveSpaceConfirm, BorderEngine, ConfirmFailed, DEFAULT_FRAME_PERIOD};
use super::model::{BorderEntry, BorderModel};
use super::route::{BORDER_HIDE_WAIT, BorderFrameKind, BorderFrames, HideWait, SharedBorderRoute};
use super::spaces::SuspendReason;
use crate::config::{BorderStateConfig, BordersConfig, LayoutType};
use crate::geometry::Rect;
use crate::test_support::{Feed, app};

const FRAME: Duration = DEFAULT_FRAME_PERIOD;

struct Rig {
    executor: Arc<ManualExecutor>,
    backend: Arc<FakeOverlayBackend>,
    route: Arc<SharedBorderRoute>,
    engine: BorderEngine,
    spaces: FakeSpaceEvents,
    feed: Feed,
    t0: Instant,
}

impl Rig {
    /// Jobs run inline (the executor is "on main").
    fn inline() -> Self {
        let rig = Self::queued();
        rig.executor.set_on_main(true);
        rig
    }

    /// Jobs wait in the queue until `drain`.
    fn queued() -> Self { Self::with_spaces(FakeSpaceEvents::new()) }

    /// The source never registers by itself (the closed startup gate).
    fn manual() -> Self {
        let rig = Self::with_spaces(FakeSpaceEvents::manual());
        rig.executor.set_on_main(true);
        rig
    }

    fn with_spaces(spaces: FakeSpaceEvents) -> Self {
        let executor = Arc::new(ManualExecutor::new(false));
        let backend = Arc::new(FakeOverlayBackend::single_display());
        let control = Arc::new(OverlayControl::new(
            Arc::clone(&executor) as Arc<dyn stache_sdk::MainThreadExecutor>,
            Arc::clone(&backend) as Arc<dyn OverlayBackend>,
            OverlayCapabilities::default(),
        ));
        let route = Arc::new(SharedBorderRoute::new(control));
        let mut engine = BorderEngine::new(Arc::clone(&route), &BordersConfig::default());
        // `FakeSpaceEvents::new()` reports "registered" on start, so the gate
        // opens; `Rig::manual` keeps it closed.
        let t0 = Instant::now();
        engine.set_space_source(Box::new(spaces.clone()));
        engine.start_spaces();
        engine.poll_spaces(t0);
        Self {
            executor,
            backend,
            route,
            engine,
            spaces,
            feed: Feed::new(1),
            t0,
        }
    }

    fn at(&self, millis: u64) -> Instant { self.t0 + Duration::from_millis(millis) }

    fn window(&self, application: u64, serial: u64) -> WindowInstance {
        self.feed.token(application, serial)
    }

    fn commands(&self) -> Vec<OverlayKind> {
        self.engine
            .sent
            .iter()
            .flat_map(|batch| batch.commands.iter().map(|command| command.kind))
            .collect()
    }

    fn count(&self, matches: impl Fn(&OverlayKind) -> bool) -> usize {
        self.commands().iter().filter(|kind| matches(kind)).count()
    }

    fn shows(&self) -> usize { self.count(|k| matches!(k, OverlayKind::Show { .. })) }

    /// The inner width of the latest configured plan (frames use distinct widths).
    fn drawn_width(&self) -> Option<f64> {
        self.backend.events().into_iter().rev().find_map(|event| match event {
            FakeEvent::Configure(_, plan) | FakeEvent::Translate(_, plan) => {
                Some(plan.target.width)
            }
            _ => None,
        })
    }

    /// Placements of a panel: a full configure or a translation-only origin set.
    fn configures(&self) -> usize {
        self.backend
            .events()
            .iter()
            .filter(|e| matches!(e, FakeEvent::Configure(..) | FakeEvent::Translate(..)))
            .count()
    }

    fn order_outs(&self) -> usize {
        self.backend
            .events()
            .iter()
            .filter(|e| matches!(e, FakeEvent::OrderOut(_)))
            .count()
    }
}

fn appearance() -> BorderAppearance {
    let config = BordersConfig::default();
    border_appearance(&config, &config.focused, 4).expect("appearance")
}

fn other_appearance() -> BorderAppearance {
    let config = BordersConfig::default();
    border_appearance(&config, &config.unfocused, 4).expect("appearance")
}

fn entry(window: WindowInstance, application: ApplicationId, cg: u32) -> BorderEntry {
    BorderEntry {
        window,
        application,
        cg_window_id: cg,
        frame: Rect::new(0.0, 0.0, 100.0, 100.0),
        focused: false,
        focus: FocusInput {
            layout: LayoutType::Dwindle,
            floating: false,
        },
        appearance: appearance(),
        plan_sequence: 1,
        held: false,
        write_pending: None,
        app_hidden: false,
        display: None,
        owner_pid: 0,
        collapsed: false,
    }
}

fn model(entries: Vec<BorderEntry>) -> BorderModel {
    BorderModel {
        entries: entries.into_iter().map(|e| (e.window, e)).collect(),
        ..BorderModel::default()
    }
}

/// Shows `entries` through the normal rule and returns the time after.
fn show_all(rig: &mut Rig, entries: Vec<BorderEntry>) {
    rig.engine.update(rig.at(0), model(entries));
    rig.engine.tick(rig.at(0) + FRAME);
}

// ------------------------------------------------------------ show after visible

#[test]
fn a_new_window_waits_one_display_frame() {
    let mut rig = Rig::inline();
    let w = rig.window(1, 1);
    rig.engine.update(rig.at(0), model(vec![entry(w, app(1), 11)]));
    assert_eq!(rig.shows(), 0, "never in the update that adds it");
    assert_eq!(rig.engine.next_deadline(), Some(rig.at(0) + FRAME));
    assert_eq!(
        rig.engine.tick(rig.at(0) + FRAME.saturating_sub(Duration::from_micros(1))),
        0
    );
    assert_eq!(rig.backend.ordered_in(), 0);
    assert_eq!(rig.engine.tick(rig.at(0) + FRAME), 1);
    assert_eq!(rig.backend.ordered_in(), 1);
    assert_eq!(rig.engine.next_deadline(), None);
    assert_eq!(rig.engine.pending_windows(), Vec::<WindowInstance>::new());
}

#[test]
fn a_reveal_shows_only_after_shown_observation_completed_write_and_one_frame() {
    let mut rig = Rig::inline();
    let (w, a) = (rig.window(1, 1), app(1));
    // The application was hidden by the runtime; the window is revealed with its
    // write still to come (plan 5).
    drop(rig.route.hide_application(a));
    let mut e = entry(w, a, 11);
    e.write_pending = Some(5);
    e.app_hidden = true;
    rig.engine.update(rig.at(0), model(vec![e.clone()]));
    assert_eq!(
        rig.engine.next_deadline(),
        None,
        "nothing holds: no shown, no write"
    );

    // 1. The shown observation alone is not enough (the model still says the
    //    application is being unhidden, and the write is pending).
    rig.engine.note_application_shown(rig.at(10), a);
    assert_eq!(rig.engine.next_deadline(), None);

    // The model catches up on the application but the write is still pending.
    e.app_hidden = false;
    rig.engine.update(rig.at(11), model(vec![e]));
    assert_eq!(rig.engine.next_deadline(), None, "write not completed");
    assert_eq!(rig.engine.tick(rig.at(500)), 0);
    assert_eq!(rig.shows(), 0);

    // 2. The reveal write completes.
    rig.engine.note_reveal_written(rig.at(20), w, 5);
    assert_eq!(rig.engine.next_deadline(), Some(rig.at(20) + FRAME));
    // 3. One display frame later it shows, not before.
    assert_eq!(rig.engine.tick(rig.at(20) + FRAME / 2), 0);
    assert_eq!(rig.engine.tick(rig.at(20) + FRAME), 1);
    assert_eq!(rig.shows(), 1);
}

#[test]
fn each_missing_condition_keeps_the_border_hidden() {
    // Missing application shown.
    let mut rig = Rig::inline();
    let (w, a) = (rig.window(1, 1), app(1));
    drop(rig.route.hide_application(a));
    rig.engine.update(rig.at(0), model(vec![entry(w, a, 11)]));
    assert_eq!(
        rig.engine.tick(rig.at(1000)),
        0,
        "application not observed shown"
    );
    rig.engine.note_application_shown(rig.at(1000), a);
    assert_eq!(rig.engine.tick(rig.at(1000) + FRAME), 1);

    // Missing write: confirmation of an older plan does not count.
    let mut rig = Rig::inline();
    let w = rig.window(1, 1);
    let mut e = entry(w, app(1), 11);
    e.write_pending = Some(7);
    rig.engine.update(rig.at(0), model(vec![e]));
    rig.engine.note_reveal_written(rig.at(1), w, 6);
    assert_eq!(
        rig.engine.tick(rig.at(1000)),
        0,
        "an older plan's write is not the reveal write"
    );
    rig.engine.note_reveal_written(rig.at(1000), w, 7);
    assert_eq!(rig.engine.tick(rig.at(1000)), 0, "the frame has not passed");
    assert_eq!(rig.engine.tick(rig.at(1000) + FRAME), 1);

    // A readback for the plan also counts as the completed write.
    let mut rig = Rig::inline();
    let w = rig.window(1, 1);
    let mut e = entry(w, app(1), 11);
    e.write_pending = Some(3);
    rig.engine.update(rig.at(0), model(vec![e]));
    rig.route.report(
        w,
        3,
        Rect::new(0.0, 0.0, 100.0, 100.0),
        BorderFrameKind::Readback,
    );
    rig.engine.note_application_shown(rig.at(5), app(1));
    assert_eq!(rig.engine.tick(rig.at(5) + FRAME), 1);

    // The model alone clears the write once the attempt settled.
    let mut rig = Rig::inline();
    let w = rig.window(1, 1);
    let mut e = entry(w, app(1), 11);
    e.write_pending = Some(2);
    rig.engine.update(rig.at(0), model(vec![e.clone()]));
    assert_eq!(rig.engine.next_deadline(), None);
    e.write_pending = None;
    rig.engine.update(rig.at(30), model(vec![e]));
    assert_eq!(rig.engine.next_deadline(), Some(rig.at(30) + FRAME));
}

#[test]
fn a_condition_turning_false_disarms_the_countdown() {
    let mut rig = Rig::inline();
    let (w, a) = (rig.window(1, 1), app(1));
    rig.engine.update(rig.at(0), model(vec![entry(w, a, 11)]));
    assert!(rig.engine.next_deadline().is_some());
    drop(rig.route.hide_application(a));
    rig.engine.update(rig.at(1), model(vec![entry(w, a, 11)]));
    assert_eq!(rig.engine.next_deadline(), None);
}

// ------------------------------------------------------------ hide before hide

#[test]
fn the_border_hide_is_acknowledged_before_the_application_hide_is_sent() {
    let mut rig = Rig::queued();
    let (w, a) = (rig.window(1, 1), app(1));
    rig.executor.set_on_main(true);
    show_all(&mut rig, vec![entry(w, a, 11)]);
    rig.executor.set_on_main(false);
    assert_eq!(rig.backend.ordered_in(), 1);

    // The effects thread hides the borders and waits; the main thread (here a
    // helper) acknowledges within the bound.
    let executor = Arc::clone(&rig.executor);
    let helper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(5));
        executor.drain()
    });
    let mut log = Vec::new();
    let outcome = rig.route.hide_application_bounded(a);
    log.push(("border hide done", rig.backend.ordered_in()));
    log.push(("HideApp sent", rig.backend.ordered_in()));
    helper.join().expect("helper");

    assert_eq!(outcome, HideWait::Acked);
    assert_eq!(
        log[0].1, 0,
        "the overlay was ordered out before the application hide"
    );
    assert_eq!(rig.route.counters().late_hides, 0);
    assert!(rig.route.application_hidden(a));
}

#[test]
fn a_hide_that_is_not_acknowledged_in_time_proceeds_and_counts_a_late_hide() {
    let mut rig = Rig::queued();
    let (w, a) = (rig.window(1, 1), app(1));
    rig.executor.set_on_main(true);
    show_all(&mut rig, vec![entry(w, a, 11)]);
    rig.executor.set_on_main(false);

    let started = Instant::now();
    let outcome = rig.route.hide_application_bounded(a);
    let waited = started.elapsed();
    assert_eq!(outcome, HideWait::TimedOut);
    assert!(waited >= BORDER_HIDE_WAIT && waited < Duration::from_millis(500));
    assert_eq!(rig.route.counters().late_hides, 1);
    assert_eq!(rig.backend.ordered_in(), 1, "the main thread has not run it yet");
    // The hide is never cancelled: it applies when main gets to it.
    rig.executor.drain();
    assert_eq!(rig.backend.ordered_in(), 0);
    assert_eq!(rig.engine.diagnostics().late_hides, 1);
    assert!(
        rig.engine
            .diagnostics()
            .messages()
            .iter()
            .any(|m| m.contains("not acknowledged")),
        "one diagnostic for late hides"
    );
}

#[test]
fn nothing_to_hide_waits_for_nothing() {
    let rig = Rig::queued();
    let started = Instant::now();
    assert_eq!(rig.route.hide_application_bounded(app(9)), HideWait::Nothing);
    assert!(started.elapsed() < BORDER_HIDE_WAIT);
}

#[test]
fn a_window_removed_by_the_update_is_hidden_in_that_update() {
    let mut rig = Rig::inline();
    let (a, b) = (rig.window(1, 1), rig.window(1, 2));
    show_all(&mut rig, vec![entry(a, app(1), 11), entry(b, app(1), 12)]);
    assert_eq!(rig.backend.ordered_in(), 2);
    rig.engine.update(rig.at(100), model(vec![entry(a, app(1), 11)]));
    assert_eq!(
        rig.backend.ordered_in(),
        1,
        "same-app window left the workspace: hidden now"
    );
    assert!(
        rig.engine
            .sent
            .last()
            .expect("batch")
            .commands
            .iter()
            .any(|c| matches!(c.kind, OverlayKind::Release))
    );
    assert_eq!(rig.engine.shown_windows(), vec![a]);
}

#[test]
fn the_effects_thread_hide_reaches_the_engine_and_the_border_returns_by_the_show_rule() {
    let mut rig = Rig::inline();
    let (w, a) = (rig.window(1, 1), app(1));
    show_all(&mut rig, vec![entry(w, a, 11)]);
    let handle = rig.engine.handle_of(w).expect("handle");

    assert_eq!(rig.route.hide_application_bounded(a), HideWait::Acked);
    rig.engine.update(rig.at(50), model(vec![entry(w, a, 11)]));
    assert!(
        rig.engine.shown_windows().is_empty(),
        "reconciled from the route"
    );
    assert_eq!(
        rig.engine.tick(rig.at(500)),
        0,
        "still hidden: no shown observation"
    );

    rig.engine.note_application_shown(rig.at(600), a);
    assert_eq!(rig.engine.tick(rig.at(600) + FRAME), 1);
    assert_eq!(rig.engine.handle_of(w), Some(handle), "the overlay is reused");
    assert_eq!(rig.backend.panels_created(), 1);
}

#[test]
fn a_hide_and_shown_observation_between_two_engine_calls_still_brings_the_border_back() {
    let mut rig = Rig::inline();
    let (w, a) = (rig.window(1, 1), app(1));
    show_all(&mut rig, vec![entry(w, a, 11)]);
    assert_eq!(rig.route.hide_application_bounded(a), HideWait::Acked);
    rig.engine.note_application_shown(rig.at(200), a);
    assert_eq!(
        rig.engine.tick(rig.at(200) + FRAME),
        1,
        "the stale shown flag was reconciled"
    );
    assert_eq!(rig.backend.ordered_in(), 1);
}

#[test]
fn an_external_hide_hides_in_the_turn_and_unhide_follows_the_show_rule() {
    let mut rig = Rig::inline();
    let (w, a) = (rig.window(1, 1), app(1));
    show_all(&mut rig, vec![entry(w, a, 11)]);
    let request = rig.engine.hide_application(a);
    assert_eq!(request.windows, vec![w]);
    assert_eq!(rig.backend.ordered_in(), 0);

    // The model drops the window while the application is hidden, then it returns.
    rig.engine.update(rig.at(50), model(vec![]));
    let mut e = entry(w, a, 11);
    e.write_pending = Some(4);
    rig.engine.update(rig.at(60), model(vec![e]));
    rig.engine.note_application_shown(rig.at(70), a);
    assert_eq!(rig.engine.tick(rig.at(900)), 0, "waits for the reveal write");
    rig.engine.note_reveal_written(rig.at(900), w, 4);
    assert_eq!(rig.engine.tick(rig.at(900) + FRAME), 1);
}

// ------------------------------------------------------------ frames

fn shown_rig() -> (Rig, WindowInstance) {
    let mut rig = Rig::inline();
    let w = rig.window(1, 1);
    let mut e = entry(w, app(1), 11);
    e.plan_sequence = 5;
    show_all(&mut rig, vec![e]);
    (rig, w)
}

const fn rect(width: f64) -> Rect { Rect::new(0.0, 0.0, width, 100.0) }

#[test]
fn borders_follow_targets_animation_frames_and_the_corrected_readback() {
    let (rig, w) = shown_rig();
    let before = rig.configures();
    rig.route.report(w, 5, rect(200.0), BorderFrameKind::Target);
    assert_eq!(rig.drawn_width(), Some(200.0));
    for width in [210.0, 220.0, 230.0] {
        rig.route.report(w, 5, rect(width), BorderFrameKind::AnimationFrame);
        assert_eq!(rig.drawn_width(), Some(width), "each frame as written");
    }
    rig.route.report(w, 5, rect(240.0), BorderFrameKind::Settled);
    assert_eq!(rig.drawn_width(), Some(240.0));
    // A refused or partial write: the readback is the observed frame.
    rig.route.report(w, 5, rect(237.0), BorderFrameKind::Readback);
    assert_eq!(rig.drawn_width(), Some(237.0));
    assert_eq!(rig.configures() - before, 6);
    assert_eq!(rig.route.counters().moves, 6);
}

#[test]
fn stale_plan_sequences_and_unknown_windows_are_ignored() {
    let (mut rig, w) = shown_rig();
    rig.route.report(w, 4, rect(300.0), BorderFrameKind::Target);
    assert_eq!(rig.drawn_width(), Some(100.0), "older plan ignored");
    assert_eq!(rig.route.counters().stale, 1);

    // A newer plan wins, and from then on the plan it replaced is stale.
    rig.route.report(w, 6, rect(310.0), BorderFrameKind::Target);
    assert_eq!(rig.drawn_width(), Some(310.0));
    rig.route.report(w, 5, rect(320.0), BorderFrameKind::Readback);
    assert_eq!(rig.drawn_width(), Some(310.0));
    assert_eq!(rig.route.counters().stale, 2);

    // The model's own plan sequence advances the same way.
    let mut e = entry(w, app(1), 11);
    e.plan_sequence = 9;
    e.frame = rect(310.0);
    rig.engine.update(rig.at(200), model(vec![e]));
    rig.route.report(w, 8, rect(330.0), BorderFrameKind::Target);
    assert_eq!(rig.drawn_width(), Some(310.0));

    let stranger = rig.window(2, 9);
    rig.route.report(stranger, 1, rect(50.0), BorderFrameKind::Target);
    assert_eq!(rig.route.counters().unknown, 1);
}

#[test]
fn a_burst_of_frames_costs_one_merged_job() {
    let mut rig = Rig::queued();
    let w = rig.window(1, 1);
    rig.executor.set_on_main(true);
    show_all(&mut rig, vec![entry(w, app(1), 11)]);
    rig.executor.set_on_main(false);
    let batches = rig.engine.route().control().counters().batches;
    for step in 0..100 {
        rig.route.report(
            w,
            1,
            rect(100.0 + f64::from(step)),
            BorderFrameKind::AnimationFrame,
        );
    }
    assert_eq!(rig.executor.queued(), 1);
    rig.executor.drain();
    assert_eq!(rig.route.control().counters().batches, batches + 1);
    assert_eq!(rig.drawn_width(), Some(199.0), "latest wins");
}

#[test]
fn frames_for_a_hidden_border_only_update_where_it_will_show() {
    let mut rig = Rig::inline();
    let w = rig.window(1, 1);
    let mut e = entry(w, app(1), 11);
    e.write_pending = Some(2);
    rig.engine.update(rig.at(0), model(vec![e]));
    // The reveal write's target and readback arrive before the border shows.
    rig.route.report(w, 2, rect(400.0), BorderFrameKind::Target);
    rig.route.report(w, 2, rect(410.0), BorderFrameKind::Readback);
    assert_eq!(rig.configures(), 0, "no overlay yet, no move");
    // The write is confirmed by the readback; the next engine call arms the frame.
    assert_eq!(rig.engine.tick(rig.at(5)), 0);
    assert_eq!(rig.engine.next_deadline(), Some(rig.at(5) + FRAME));
    assert_eq!(rig.engine.tick(rig.at(5) + FRAME), 1);
    assert_eq!(
        rig.drawn_width(),
        Some(410.0),
        "shown at the frame Tiling confirmed"
    );
}

#[test]
fn a_model_frame_change_moves_and_does_not_fight_animation_frames() {
    let (mut rig, w) = shown_rig();
    rig.route.report(w, 5, rect(250.0), BorderFrameKind::AnimationFrame);
    // The state publishes without a new observed frame: no move back.
    let mut e = entry(w, app(1), 11);
    e.plan_sequence = 5;
    rig.engine.update(rig.at(100), model(vec![e.clone()]));
    assert_eq!(rig.drawn_width(), Some(250.0));
    // The observed frame arrives.
    e.frame = rect(260.0);
    rig.engine.update(rig.at(200), model(vec![e]));
    assert_eq!(rig.drawn_width(), Some(260.0));
    assert_eq!(rig.count(|k| matches!(k, OverlayKind::Move { .. })), 1);
}

// ------------------------------------------------------------ focus and tabs

#[test]
fn a_focus_flip_recolors_in_place_with_a_state_change() {
    let (mut rig, w) = shown_rig();
    let handle = rig.engine.handle_of(w).expect("handle");
    let mut e = entry(w, app(1), 11);
    e.plan_sequence = 5;
    e.focused = true;
    e.appearance = other_appearance();
    rig.engine.update(rig.at(100), model(vec![e]));
    let last = rig.engine.sent.last().expect("batch").commands.clone();
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].handle, handle);
    assert!(matches!(last[0].kind, OverlayKind::Show {
        change: AppearanceChange::StateChange,
        ..
    }));
    assert_eq!(
        rig.backend.panels_created(),
        1,
        "recolored in place, not recreated"
    );
    assert_eq!(rig.engine.handle_of(w), Some(handle));
}

#[test]
fn reorder_sends_order_above_the_target_for_a_shown_border_only() {
    let (mut rig, w) = shown_rig();
    assert!(rig.engine.reorder(w));
    assert!(matches!(
        rig.engine.sent.last().expect("batch").commands[0].kind,
        OverlayKind::Order { target_cg_id: 11 }
    ));
    assert!(
        rig.backend
            .events()
            .iter()
            .filter(|e| matches!(e, FakeEvent::OrderRelative { .. }))
            .count()
            >= 2
    );
    assert!(!rig.engine.reorder(rig.window(7, 7)), "unknown window");
}

#[test]
fn tab_handover_releases_the_old_member_and_the_new_one_follows_the_show_rule() {
    let mut rig = Rig::inline();
    let (old, new, a) = (rig.window(1, 1), rig.window(1, 2), app(1));
    show_all(&mut rig, vec![entry(old, a, 11)]);
    let handle = rig.engine.handle_of(old).expect("handle");

    // The old member is suspended but still holds the slot: the overlay stays.
    let mut held = entry(old, a, 11);
    held.held = true;
    let commands = rig.commands().len();
    rig.engine.update(rig.at(100), model(vec![held]));
    assert_eq!(rig.commands().len(), commands, "slot_held changes nothing");
    assert_eq!(rig.engine.shown_windows(), vec![old]);
    assert_eq!(rig.backend.ordered_in(), 1);

    // A held window never gets a new overlay.
    let mut rig2 = Rig::inline();
    let w = rig2.window(1, 5);
    let mut h = entry(w, a, 15);
    h.held = true;
    rig2.engine.update(rig2.at(0), model(vec![h]));
    assert_eq!(rig2.engine.next_deadline(), None);
    assert!(rig2.engine.pending_windows().is_empty());

    // Handover: the old token is gone, the new one carries the slot (its reveal
    // marker was transferred, so its write is pending).
    let mut fresh = entry(new, a, 12);
    fresh.write_pending = Some(3);
    rig.engine.update(rig.at(200), model(vec![fresh]));
    let last = rig.engine.sent.last().expect("batch").commands.clone();
    assert!(
        last.iter()
            .any(|c| c.handle == handle && matches!(c.kind, OverlayKind::Release))
    );
    assert!(rig.engine.shown_windows().is_empty());
    assert_eq!(rig.engine.tick(rig.at(300)), 0);
    rig.engine.note_reveal_written(rig.at(300), new, 3);
    assert_eq!(rig.engine.tick(rig.at(300) + FRAME), 1);
    assert_eq!(rig.engine.shown_windows(), vec![new]);
}

// ------------------------------------------------------------ configuration

#[test]
fn configure_recomputes_appearances_in_place_with_reload() {
    let (mut rig, w) = shown_rig();
    let mut e = entry(w, app(1), 11);
    e.plan_sequence = 5;
    e.focused = true;
    rig.engine.update(rig.at(100), model(vec![e]));
    let handle = rig.engine.handle_of(w).expect("handle");

    let config: BordersConfig = serde_json::from_value(serde_json::json!({
        "focused": {"width": 9, "color": "#010203"},
    }))
    .expect("config");
    rig.engine.configure(&config);
    let last = rig.engine.sent.last().expect("batch").commands.clone();
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].handle, handle);
    let OverlayKind::Show { appearance, change, .. } = last[0].kind else {
        panic!("a reload is a Show");
    };
    assert_eq!(change, AppearanceChange::Reload);
    assert!((appearance.width - 9.0).abs() < f64::EPSILON);
    assert_eq!(rig.backend.panels_created(), 1);
    assert_eq!(rig.route.control().transition(), None);
}

#[test]
fn a_reload_that_disables_a_state_removes_those_borders() {
    let (mut rig, w) = shown_rig();
    let mut other = entry(rig.window(1, 2), app(1), 12);
    other.focused = true;
    let mut e = entry(w, app(1), 11);
    e.plan_sequence = 5;
    rig.engine.update(rig.at(100), model(vec![e, other]));
    rig.engine.tick(rig.at(100) + FRAME);
    let config = BordersConfig {
        unfocused: BorderStateConfig::Disabled(false),
        ..BordersConfig::default()
    };
    rig.engine.configure(&config);
    assert_eq!(rig.engine.shown_windows(), vec![rig.window(1, 2)]);
}

// ------------------------------------------------------------ limits and failure

#[test]
fn the_ledger_never_exceeds_the_overlay_limit() {
    let mut rig = Rig::inline();
    let entries: Vec<BorderEntry> = (0..=MAX_OVERLAYS as u64)
        .map(|n| entry(rig.window(1, n + 1), app(1), 100 + u32::try_from(n).expect("n")))
        .collect();
    show_all(&mut rig, entries);
    assert_eq!(rig.engine.shown_windows().len(), MAX_OVERLAYS);
    assert_eq!(rig.backend.live_overlays(), MAX_OVERLAYS);
    assert_eq!(rig.engine.diagnostics().engine.refused_shows, 1);
    assert!(rig.engine.diagnostics().messages().iter().any(|m| m.contains("refused")));
    // The refused border alone is affected; a freed slot lets it show.
    let first = rig.window(1, 1);
    let rest: Vec<BorderEntry> = (1..=MAX_OVERLAYS as u64)
        .map(|n| entry(rig.window(1, n + 1), app(1), 100 + u32::try_from(n).expect("n")))
        .collect();
    rig.engine.update(rig.at(500), model(rest));
    assert!(!rig.engine.shown_windows().contains(&first));
    rig.engine.tick(rig.at(500) + FRAME);
    assert_eq!(rig.engine.shown_windows().len(), MAX_OVERLAYS);
}

#[test]
fn missing_cg_ids_are_a_diagnostic_and_the_engine_sends_nothing_for_them() {
    let mut rig = Rig::inline();
    let mut m = model(vec![]);
    m.missing_cg_id = 2;
    rig.engine.update(rig.at(0), m);
    assert!(rig.engine.sent.is_empty());
    assert!(rig.engine.diagnostics().messages().iter().any(|m| m.contains("no window id")));
}

#[test]
fn a_creation_failure_is_counted_for_that_border_only() {
    let mut rig = Rig::inline();
    rig.backend.fail_creation(true);
    let (a, b) = (rig.window(1, 1), rig.window(1, 2));
    show_all(&mut rig, vec![entry(a, app(1), 11), entry(b, app(1), 12)]);
    rig.backend.fail_creation(false);
    let w = rig.window(1, 3);
    rig.engine.update(
        rig.at(100),
        model(vec![
            entry(a, app(1), 11),
            entry(b, app(1), 12),
            entry(w, app(1), 13),
        ]),
    );
    rig.engine.tick(rig.at(100) + FRAME);
    rig.engine.update(rig.at(200), model(vec![entry(w, app(1), 13)]));
    assert!(rig.engine.diagnostics().engine.failed_commands >= 2);
    assert_eq!(rig.backend.ordered_in(), 1, "the healthy border is drawn");
}

// ------------------------------------------------------------ part 05 hooks

struct Refuse(BTreeSet<WindowInstance>, Arc<std::sync::Mutex<Vec<usize>>>);

impl ActiveSpaceConfirm for Refuse {
    fn confirm_on_active_space(
        &mut self,
        windows: &[(WindowInstance, u32)],
    ) -> Result<BTreeSet<WindowInstance>, ConfirmFailed> {
        self.1.lock().expect("calls").push(windows.len());
        Ok(windows.iter().map(|(w, _)| *w).filter(|w| !self.0.contains(w)).collect())
    }
}

#[test]
fn suspend_hides_all_bumps_the_epoch_and_blocks_shows_and_orders() {
    let mut rig = Rig::inline();
    let (a, b) = (rig.window(1, 1), rig.window(1, 2));
    show_all(&mut rig, vec![entry(a, app(1), 11)]);
    assert_eq!(rig.engine.gate_epoch(), 0);

    rig.engine.suspend(SuspendReason::MissionControl);
    assert_eq!(rig.engine.gate_epoch(), 1);
    assert_eq!(
        rig.engine.sent.last().expect("batch").hide_all,
        Some(HideReason::MissionControl)
    );
    assert_eq!(rig.backend.ordered_in(), 0);
    assert_eq!(rig.engine.next_deadline(), None);

    // The same reason again is a duplicate; a second reason is a new HideAll.
    rig.engine.suspend(SuspendReason::MissionControl);
    assert_eq!(rig.engine.gate_epoch(), 1);
    rig.engine.suspend(SuspendReason::Unknown {
        cause: UnknownCause::QueryFailed,
    });
    assert_eq!(rig.engine.gate_epoch(), 2);

    // Model updates continue; no Show, no Order.
    let shows = rig.shows();
    rig.engine.update(
        rig.at(200),
        model(vec![entry(a, app(1), 11), entry(b, app(1), 12)]),
    );
    assert_eq!(rig.engine.tick(rig.at(10_000)), 0);
    assert_eq!(rig.shows(), shows);
    assert!(!rig.engine.reorder(a));
    assert_eq!(rig.engine.diagnostics().engine.suppressed_orders, 1);
    // Frames for hidden borders move nothing.
    let configures = rig.configures();
    rig.route.report(a, 1, rect(500.0), BorderFrameKind::Target);
    assert_eq!(rig.configures(), configures);
}

#[test]
fn resume_reconfirms_then_applies_the_show_rule_with_one_frame() {
    let mut rig = Rig::inline();
    let (a, b) = (rig.window(1, 1), rig.window(1, 2));
    show_all(&mut rig, vec![entry(a, app(1), 11), entry(b, app(1), 12)]);
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    rig.engine
        .set_confirmer(Box::new(Refuse(BTreeSet::from([b]), Arc::clone(&calls))));

    rig.engine.suspend(SuspendReason::MissionControl);
    rig.engine.suspend(SuspendReason::Unknown {
        cause: UnknownCause::DockUnregistered,
    });
    rig.engine.resume(rig.at(1000), SuspendReason::MissionControl);
    assert!(
        calls.lock().expect("calls").is_empty(),
        "not the last reason yet"
    );
    assert_eq!(rig.engine.next_deadline(), None);

    rig.engine.resume(rig.at(1000), SuspendReason::Unknown {
        cause: UnknownCause::DockUnregistered,
    });
    assert_eq!(
        calls.lock().expect("calls").as_slice(),
        &[2],
        "re-confirmed once, both windows"
    );
    assert_eq!(
        rig.engine.next_deadline(),
        Some(rig.at(1000) + FRAME),
        "then one frame"
    );
    assert_eq!(rig.engine.tick(rig.at(1000)), 0);
    assert_eq!(rig.engine.tick(rig.at(1000) + FRAME), 1);
    assert_eq!(
        rig.engine.shown_windows(),
        vec![a],
        "the unconfirmed window stays hidden"
    );
    assert_eq!(rig.engine.diagnostics().engine.unconfirmed, 1);

    // The Space check is repeated for it only when asked.
    rig.engine.set_confirmer(Box::new(super::engine::ConfirmAll));
    rig.engine.reconfirm(rig.at(2000));
    assert_eq!(rig.engine.tick(rig.at(2000) + FRAME), 1);
    assert_eq!(rig.engine.shown_windows().len(), 2);
}

#[test]
fn resume_shows_the_current_model_not_the_one_at_suspension() {
    let mut rig = Rig::inline();
    let (a, b) = (rig.window(1, 1), rig.window(1, 2));
    show_all(&mut rig, vec![entry(a, app(1), 11)]);
    rig.engine.suspend(SuspendReason::MissionControl);
    let mut moved = entry(b, app(1), 12);
    moved.frame = rect(321.0);
    rig.engine.update(rig.at(100), model(vec![moved]));
    rig.engine.resume(rig.at(200), SuspendReason::MissionControl);
    assert_eq!(rig.engine.tick(rig.at(200) + FRAME), 1);
    assert_eq!(rig.engine.shown_windows(), vec![b]);
    assert_eq!(rig.drawn_width(), Some(321.0));
}

#[test]
fn a_show_computed_before_a_suspension_is_not_sent_after_it() {
    let mut rig = Rig::inline();
    let a = rig.window(1, 1);
    rig.engine.update(rig.at(0), model(vec![entry(a, app(1), 11)]));
    rig.engine.suspend(SuspendReason::SpaceTransition {
        display: stache_macos::DisplayId::new(1),
    });
    assert_eq!(rig.engine.tick(rig.at(0) + FRAME), 0);
    assert_eq!(rig.shows(), 0);
    assert_eq!(rig.order_outs(), 0, "nothing was ever ordered in");
}

// ------------------------------------------------- part 05: gate and sources

/// A confirmer a test switches: `away` (nothing is on the active Space) or
/// `fail` (the query fails).
#[derive(Clone, Default)]
struct Switch {
    away: Arc<std::sync::atomic::AtomicBool>,
    fail: Arc<std::sync::atomic::AtomicBool>,
    calls: Arc<std::sync::Mutex<Vec<usize>>>,
}

impl ActiveSpaceConfirm for Switch {
    fn confirm_on_active_space(
        &mut self,
        windows: &[(WindowInstance, u32)],
    ) -> Result<BTreeSet<WindowInstance>, ConfirmFailed> {
        use std::sync::atomic::Ordering::SeqCst;
        self.calls.lock().expect("calls").push(windows.len());
        if self.fail.load(SeqCst) {
            return Err(ConfirmFailed);
        }
        if self.away.load(SeqCst) {
            return Ok(BTreeSet::new());
        }
        Ok(windows.iter().map(|(w, _)| *w).collect())
    }
}

fn space_messages(rig: &Rig) -> Vec<String> {
    rig.engine
        .diagnostics()
        .messages()
        .into_iter()
        .filter(|m| m.contains("Spaces or Mission Control"))
        .collect()
}

fn two_shown(rig: &mut Rig) -> (WindowInstance, WindowInstance) {
    let (a, b) = (rig.window(1, 1), rig.window(1, 2));
    show_all(rig, vec![entry(a, app(1), 11), entry(b, app(1), 12)]);
    assert_eq!(rig.backend.ordered_in(), 2);
    (a, b)
}

/// TB-05-03: the production startup state. No source has reported, so nothing
/// is shown and one diagnostic says why.
#[test]
fn the_engine_starts_closed_and_opens_after_registration_and_a_confirmation() {
    let mut rig = Rig::manual();
    let a = rig.window(1, 1);
    rig.engine.update(rig.at(0), model(vec![entry(a, app(1), 11)]));
    assert_eq!(rig.engine.tick(rig.at(10_000)), 0);
    assert_eq!(rig.backend.ordered_in(), 0);
    assert_eq!(rig.engine.next_deadline(), None);
    let messages = space_messages(&rig);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("sources not started"), "{messages:?}");

    let switch = Switch::default();
    rig.engine.set_confirmer(Box::new(switch.clone()));
    rig.spaces.register();
    rig.engine.poll_spaces(rig.at(100));
    assert_eq!(*switch.calls.lock().expect("calls"), [1], "a full confirmation");
    assert!(space_messages(&rig).is_empty());
    assert_eq!(rig.engine.tick(rig.at(100)), 0, "one display frame first");
    assert_eq!(rig.engine.tick(rig.at(100) + FRAME), 1);
}

#[test]
fn the_default_no_source_keeps_every_border_hidden() {
    let executor = Arc::new(ManualExecutor::new(true));
    let backend = Arc::new(FakeOverlayBackend::single_display());
    let control = Arc::new(OverlayControl::new(
        Arc::clone(&executor) as Arc<dyn stache_sdk::MainThreadExecutor>,
        Arc::clone(&backend) as Arc<dyn OverlayBackend>,
        OverlayCapabilities::default(),
    ));
    let route = Arc::new(SharedBorderRoute::new(control));
    let mut engine = BorderEngine::new(route, &BordersConfig::default());
    let feed = Feed::new(1);
    let t0 = Instant::now();
    engine.start_spaces();
    engine.poll_spaces(t0);
    engine.update(t0, model(vec![entry(feed.token(1, 1), app(1), 11)]));
    assert_eq!(engine.tick(t0 + Duration::from_secs(5)), 0);
    assert_eq!(backend.ordered_in(), 0);
    assert!(engine.diagnostics().spaces.is_some());
}

/// TB-05-01 (engine half): enter, duplicate, exit, out of order.
#[test]
fn space_facts_hide_once_ignore_duplicates_and_resume_after_the_last_exit() {
    let mut rig = Rig::inline();
    two_shown(&mut rig);
    let epoch = rig.engine.gate_epoch();

    // An exit without an enter changes nothing.
    rig.spaces.exit_mission_control();
    rig.engine.poll_spaces(rig.at(100));
    assert_eq!(rig.engine.gate_epoch(), epoch);
    assert_eq!(rig.backend.ordered_in(), 2);

    rig.spaces.enter_mission_control();
    rig.spaces.enter_mission_control();
    rig.engine.poll_spaces(rig.at(200));
    assert_eq!(
        rig.engine.gate_epoch(),
        epoch + 1,
        "one bump for a duplicate pair"
    );
    assert_eq!(rig.engine.diagnostics().engine.hide_alls, 1);
    assert_eq!(rig.backend.ordered_in(), 0);
    assert!(rig.engine.suspended());

    rig.spaces.exit_mission_control();
    rig.spaces.exit_mission_control();
    rig.engine.poll_spaces(rig.at(300));
    assert!(!rig.engine.suspended());
    assert_eq!(rig.backend.ordered_in(), 0, "the show rule needs one frame");
    assert_eq!(rig.engine.tick(rig.at(300) + FRAME), 2);
    assert_eq!(rig.backend.ordered_in(), 2);
}

/// TB-05-01: resume order is re-confirm, then the show rule, then one frame;
/// every command is stamped with the gate epoch it was computed under.
#[test]
fn resume_confirms_first_then_shows_after_one_frame_under_the_new_epoch() {
    let mut rig = Rig::inline();
    two_shown(&mut rig);
    let switch = Switch::default();
    rig.engine.set_confirmer(Box::new(switch.clone()));
    rig.spaces.begin_space_change(stache_macos::DisplayId::new(1));
    rig.spaces.enter_mission_control();
    rig.engine.poll_spaces(rig.at(100));
    let hide_epoch = rig.engine.gate_epoch();
    assert_eq!(hide_epoch, 2);

    // The model keeps changing while suspended.
    let (a, b) = (rig.window(1, 1), rig.window(1, 2));
    let mut moved = entry(a, app(1), 11);
    moved.frame = rect(222.0);
    rig.engine.update(rig.at(150), model(vec![moved, entry(b, app(1), 12)]));
    assert_eq!(rig.backend.ordered_in(), 0);

    rig.spaces.exit_mission_control();
    rig.engine.poll_spaces(rig.at(200));
    assert!(
        switch.calls.lock().expect("calls").is_empty(),
        "a Space change is still open"
    );
    rig.spaces.complete_space_change(stache_macos::DisplayId::new(1));
    rig.engine.poll_spaces(rig.at(300));
    assert_eq!(
        *switch.calls.lock().expect("calls"),
        [2],
        "re-confirmed once, both"
    );
    assert_eq!(rig.backend.ordered_in(), 0, "confirmation alone shows nothing");
    assert_eq!(rig.engine.next_deadline(), Some(rig.at(300) + FRAME));
    assert_eq!(rig.engine.tick(rig.at(300) + FRAME), 2);
    assert!(
        rig.backend.events().iter().any(
            |e| matches!(e, FakeEvent::Configure(_, plan) if (plan.target.width - 222.0).abs() < 1e-9)
        ),
        "resume draws the current model"
    );

    let shows: Vec<u64> = rig
        .engine
        .sent
        .iter()
        .flat_map(|batch| batch.commands.iter())
        .filter(|c| matches!(c.kind, OverlayKind::Show { .. }))
        .map(|c| c.epoch)
        .collect();
    assert_eq!(&shows[shows.len() - 2..], &[hide_epoch, hide_epoch]);
}

/// TB-05-02 (engine and control together): a Show computed and queued before a
/// suspension is dropped on main even though it was sent first.
#[test]
fn a_queued_show_is_dropped_when_mission_control_starts_before_main_runs_it() {
    let mut rig = Rig::queued();
    rig.executor.set_on_main(false);
    let a = rig.window(1, 1);
    rig.engine.update(rig.at(0), model(vec![entry(a, app(1), 11)]));
    assert_eq!(
        rig.engine.tick(rig.at(0) + FRAME),
        1,
        "the Show is sent, main has not run it"
    );
    assert_eq!(rig.backend.ordered_in(), 0);

    rig.spaces.enter_mission_control();
    rig.engine.poll_spaces(rig.at(10));
    rig.executor.drain();
    assert_eq!(rig.backend.ordered_in(), 0, "never ordered in");
    assert_eq!(rig.route.control().counters().stale_dropped, 1);
    assert!(rig.engine.shown_windows().is_empty());

    // After the exit the same handle is shown again under the new epoch.
    rig.spaces.exit_mission_control();
    rig.engine.poll_spaces(rig.at(20));
    assert_eq!(rig.engine.tick(rig.at(20) + FRAME), 1);
    rig.executor.drain();
    assert_eq!(rig.backend.ordered_in(), 1);
}

/// TB-05-03: every fail-closed cause keeps every overlay hidden with exactly one
/// diagnostic and clears after re-registration and a re-confirmation.
#[test]
fn every_fail_closed_cause_hides_everything_with_one_diagnostic_until_restored() {
    let causes = [
        UnknownCause::DockUnregistered,
        UnknownCause::MissingSymbol("SLSRegisterNotifyProc"),
        UnknownCause::RegistrationFailed,
        UnknownCause::DisplayChange,
        UnknownCause::OwnerStopped,
    ];
    for cause in causes {
        let mut rig = Rig::inline();
        let (a, b) = two_shown(&mut rig);
        let switch = Switch::default();
        rig.engine.set_confirmer(Box::new(switch.clone()));

        rig.spaces.lose(cause);
        rig.engine.poll_spaces(rig.at(100));
        assert_eq!(rig.backend.ordered_in(), 0, "{cause}");
        let messages = space_messages(&rig);
        assert_eq!(messages.len(), 1, "{cause}: {messages:?}");
        assert!(messages[0].contains(&cause.to_string()), "{messages:?}");

        // Nothing shows however long we wait or whatever the model does.
        rig.engine.update(
            rig.at(200),
            model(vec![entry(a, app(1), 11), entry(b, app(1), 12)]),
        );
        assert_eq!(rig.engine.tick(rig.at(60_000)), 0, "{cause}");
        assert_eq!(rig.backend.ordered_in(), 0, "{cause}");
        assert_eq!(space_messages(&rig).len(), 1, "still exactly one");

        rig.spaces.restore(cause);
        rig.engine.poll_spaces(rig.at(61_000));
        assert_eq!(
            switch.calls.lock().expect("calls").len(),
            1,
            "{cause}: re-confirmed"
        );
        assert!(space_messages(&rig).is_empty(), "{cause}");
        assert_eq!(rig.engine.tick(rig.at(61_000) + FRAME), 2, "{cause}");
        assert_eq!(rig.backend.ordered_in(), 2, "{cause}");
    }
}

/// TB-05-03: the Dock quit and relaunched; the observer must register again.
#[test]
fn a_dock_relaunch_keeps_borders_hidden_until_it_is_registered_again() {
    let mut rig = Rig::inline();
    two_shown(&mut rig);
    rig.spaces.dock_quit();
    rig.engine.poll_spaces(rig.at(100));
    assert_eq!(rig.backend.ordered_in(), 0);
    // Mission Control opens and closes while the Dock is away: still hidden.
    rig.spaces.enter_mission_control();
    rig.spaces.exit_mission_control();
    rig.engine.poll_spaces(rig.at(200));
    assert_eq!(rig.engine.tick(rig.at(10_000)), 0);
    assert_eq!(space_messages(&rig).len(), 1);
    rig.spaces.dock_relaunched();
    rig.engine.poll_spaces(rig.at(20_000));
    assert_eq!(rig.engine.tick(rig.at(20_000) + FRAME), 2);
}

/// TB-05-03: overflow. The facts are lost, so everything hides, the source is
/// asked to re-sync and only its report re-opens the gate.
#[test]
fn an_observer_overflow_hides_everything_until_the_source_resyncs() {
    let mut rig = Rig::inline();
    two_shown(&mut rig);
    assert!(rig.spaces.overflow() > 0);
    rig.engine.poll_spaces(rig.at(100));
    assert_eq!(rig.backend.ordered_in(), 0);
    assert_eq!(rig.spaces.resyncs(), 1);
    assert_eq!(rig.engine.diagnostics().engine.space_overflows, 1);
    let messages = space_messages(&rig);
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("overflowed"), "{messages:?}");

    // The fake resyncs at once: its report arrives with the next drain.
    rig.engine.poll_spaces(rig.at(200));
    assert!(space_messages(&rig).is_empty());
    assert_eq!(rig.engine.tick(rig.at(200) + FRAME), 2);
}

#[test]
fn an_overflow_forgets_a_lost_enter_and_waits_for_the_manual_resync_report() {
    let spaces = FakeSpaceEvents::manual();
    let mut rig = Rig::with_spaces(spaces.clone());
    rig.executor.set_on_main(true);
    spaces.register();
    rig.engine.poll_spaces(rig.at(0));
    two_shown(&mut rig);
    spaces.enter_mission_control();
    assert!(spaces.overflow() > 0);
    rig.engine.poll_spaces(rig.at(100));
    assert!(rig.engine.suspended());
    assert_eq!(rig.engine.tick(rig.at(100_000)), 0, "no report, still hidden");
    // The source reports that Mission Control is gone, then the resync.
    spaces.restore(UnknownCause::MailboxOverflow);
    rig.engine.poll_spaces(rig.at(100_000));
    assert!(!rig.engine.suspended());
    assert_eq!(rig.engine.tick(rig.at(100_000) + FRAME), 2);
}

/// TB-05-03: a Space query failure keeps borders hidden, is retried only when
/// the world changes, and the diagnostic clears only after a good confirmation.
#[test]
fn a_query_failure_hides_until_a_later_event_re_confirms_successfully() {
    let mut rig = Rig::inline();
    two_shown(&mut rig);
    let switch = Switch::default();
    rig.engine.set_confirmer(Box::new(switch.clone()));
    switch.fail.store(true, std::sync::atomic::Ordering::SeqCst);

    rig.spaces.enter_mission_control();
    rig.spaces.exit_mission_control();
    rig.engine.poll_spaces(rig.at(100));
    assert!(rig.engine.suspended(), "the confirmation failed: fail closed");
    let messages = space_messages(&rig);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("Space query failed"), "{messages:?}");
    let calls = switch.calls.lock().expect("calls").len();
    assert_eq!(rig.engine.tick(rig.at(60_000)), 0);
    assert_eq!(
        switch.calls.lock().expect("calls").len(),
        calls,
        "never periodic"
    );

    // A settled Space change retries; still failing, so still one diagnostic.
    rig.spaces.begin_space_change(stache_macos::DisplayId::new(1));
    rig.spaces.complete_space_change(stache_macos::DisplayId::new(1));
    rig.engine.poll_spaces(rig.at(61_000));
    assert!(rig.engine.suspended());
    assert_eq!(space_messages(&rig).len(), 1);

    switch.fail.store(false, std::sync::atomic::Ordering::SeqCst);
    rig.spaces.begin_space_change(stache_macos::DisplayId::new(1));
    rig.spaces.complete_space_change(stache_macos::DisplayId::new(1));
    rig.engine.poll_spaces(rig.at(62_000));
    assert!(!rig.engine.suspended());
    assert!(space_messages(&rig).is_empty());
    assert_eq!(rig.engine.tick(rig.at(62_000) + FRAME), 2);
}

#[test]
fn a_failing_confirmation_on_show_hides_everything() {
    let mut rig = Rig::inline();
    let switch = Switch::default();
    rig.engine.set_confirmer(Box::new(switch.clone()));
    let a = rig.window(1, 1);
    rig.engine.update(rig.at(0), model(vec![entry(a, app(1), 11)]));
    switch.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(rig.engine.tick(rig.at(0) + FRAME), 0);
    assert!(rig.engine.suspended());
    assert_eq!(rig.backend.ordered_in(), 0);
    assert_eq!(space_messages(&rig).len(), 1);
}

#[test]
fn windows_on_an_inactive_space_stay_hidden_after_a_resume() {
    let mut rig = Rig::inline();
    let (a, _b) = two_shown(&mut rig);
    let switch = Switch::default();
    rig.engine.set_confirmer(Box::new(switch.clone()));
    rig.spaces.begin_space_change(stache_macos::DisplayId::new(1));
    rig.engine.poll_spaces(rig.at(100));
    switch.away.store(true, std::sync::atomic::Ordering::SeqCst);
    rig.spaces.complete_space_change(stache_macos::DisplayId::new(1));
    rig.engine.poll_spaces(rig.at(200));
    assert_eq!(rig.engine.tick(rig.at(10_000)), 0, "both are on another Space");
    assert_eq!(rig.backend.ordered_in(), 0);
    assert!(rig.engine.pending_windows().contains(&a));
    assert!(space_messages(&rig).is_empty(), "known, not unknown");

    // Back: the next Space change confirms them again.
    switch.away.store(false, std::sync::atomic::Ordering::SeqCst);
    rig.spaces.begin_space_change(stache_macos::DisplayId::new(1));
    rig.spaces.complete_space_change(stache_macos::DisplayId::new(1));
    rig.engine.poll_spaces(rig.at(11_000));
    assert_eq!(rig.engine.tick(rig.at(11_000) + FRAME), 2);
}

/// TB-05-03: a fullscreen transition hides that window's border only.
#[test]
fn fullscreen_enter_and_exit_hide_only_that_windows_border() {
    let mut rig = Rig::inline();
    let (a, b) = two_shown(&mut rig);
    let epoch = rig.engine.gate_epoch();
    rig.spaces.fullscreen(a, true);
    rig.engine.poll_spaces(rig.at(100));
    assert_eq!(rig.engine.shown_windows(), vec![b], "the other border stays");
    assert_eq!(rig.backend.ordered_in(), 1);
    assert_eq!(rig.engine.gate_epoch(), epoch, "no global suspension");
    assert!(!rig.engine.suspended());
    assert_eq!(rig.engine.diagnostics().engine.hide_alls, 0);
    assert_eq!(rig.engine.diagnostics().engine.window_hides, 1);
    assert!(space_messages(&rig).is_empty());

    // The window's border cannot come back, however the model changes.
    rig.engine.update(
        rig.at(200),
        model(vec![entry(a, app(1), 11), entry(b, app(1), 12)]),
    );
    assert_eq!(rig.engine.tick(rig.at(60_000)), 0);
    assert_eq!(rig.engine.shown_windows(), vec![b]);
    assert!(!rig.engine.reorder(a));

    // Other windows still re-order normally.
    assert!(rig.engine.reorder(b));

    rig.spaces.fullscreen(a, false);
    rig.engine.poll_spaces(rig.at(61_000));
    assert_eq!(rig.engine.shown_windows(), vec![b], "one frame first");
    assert_eq!(rig.engine.tick(rig.at(61_000) + FRAME), 1);
    assert_eq!(rig.engine.shown_windows().len(), 2);
    assert_eq!(rig.backend.ordered_in(), 2);
}

#[test]
fn stopping_the_source_closes_the_mailbox_and_a_restart_begins_closed() {
    let mut rig = Rig::inline();
    two_shown(&mut rig);
    rig.engine.stop_spaces();
    assert_eq!(rig.spaces.stops(), 1);
    assert_eq!(
        rig.spaces
            .inject(stache_macos::border_overlay::SpaceFact::MissionControlEntered),
        stache_macos::border_overlay::PushOutcome::Closed,
        "no callback after stop"
    );
    rig.engine.stop_spaces();
    assert_eq!(rig.spaces.stops(), 1, "stopping twice is a no-op");
    rig.engine.start_spaces();
    assert_eq!(rig.spaces.starts(), 2);
    assert!(rig.engine.suspended(), "a new run starts closed");
    rig.engine.poll_spaces(rig.at(10));
    assert!(!rig.engine.suspended(), "until the source registers again");
}

fn model_with_fullscreen(entries: Vec<BorderEntry>, fullscreen: &[WindowInstance]) -> BorderModel {
    BorderModel {
        fullscreen: fullscreen.iter().copied().collect(),
        ..model(entries)
    }
}

/// The engine reports the fullscreen flag it sees in the model to the gate, so
/// no extra native observer exists: a flipped flag hides that border only and
/// leaving fullscreen re-confirms it like a reveal.
#[test]
fn the_model_fullscreen_flag_hides_that_border_and_leaving_re_confirms_it() {
    let mut rig = Rig::inline();
    let (a, b) = two_shown(&mut rig);
    let epoch = rig.engine.gate_epoch();
    // The adapter read `AXFullScreen` for `a`: it leaves the entries and enters
    // the fullscreen set.
    rig.engine.update(
        rig.at(100),
        model_with_fullscreen(vec![entry(b, app(1), 12)], &[a]),
    );
    assert_eq!(rig.engine.shown_windows(), vec![b]);
    assert!(rig.engine.gate().window_blocked(a));
    assert_eq!(
        rig.engine.gate_epoch(),
        epoch,
        "window-scoped: no global suspension"
    );
    assert!(!rig.engine.suspended());

    // The flag clears and the window is back in the entries in one update.
    rig.engine.update(
        rig.at(5_000),
        model_with_fullscreen(vec![entry(a, app(1), 11), entry(b, app(1), 12)], &[]),
    );
    assert!(!rig.engine.gate().window_blocked(a));
    assert_eq!(rig.engine.tick(rig.at(5_000)), 0, "one display frame first");
    assert_eq!(rig.engine.tick(rig.at(5_000) + FRAME), 1);
    assert_eq!(rig.engine.shown_windows().len(), 2);
}

#[test]
fn a_fullscreen_window_stays_blocked_across_an_overflow_and_a_restart() {
    let mut rig = Rig::inline();
    let (a, b) = two_shown(&mut rig);
    rig.engine.update(
        rig.at(100),
        model_with_fullscreen(vec![entry(b, app(1), 12)], &[a]),
    );
    assert!(rig.engine.gate().window_blocked(a));

    // The overflow forgets every learned reason; the engine re-enters its own.
    for _ in 0..=stache_macos::border_overlay::MAX_SPACE_FACTS {
        let _ = rig
            .spaces
            .inject(stache_macos::border_overlay::SpaceFact::DisplayLayoutChanging);
    }
    rig.engine.poll_spaces(rig.at(200));
    assert!(
        rig.engine.gate().window_blocked(a),
        "re-entered after the overflow"
    );

    rig.engine.stop_spaces();
    rig.engine.start_spaces();
    assert!(rig.engine.gate().window_blocked(a), "re-entered on a restart");
}

#[path = "live_tests.rs"]
mod live;

// ------------------------------------------------------------ part 07: budget

/// TB-07-03: a main thread that leaves a batch unacknowledged for
/// `BORDER_ACK_WAIT` makes status report it busy; nothing waits for it, later
/// batches merge into the one pending batch (one queued job), and an idle
/// engine schedules no deadline at all (zero wakeups).
#[test]
fn a_main_thread_held_past_the_ack_wait_is_reported_busy_and_batches_merge() {
    use super::{BORDER_ACK_WAIT, MAIN_BUSY_DIAGNOSTIC};
    let mut rig = Rig::queued();
    let (w, a) = (rig.window(1, 1), app(1));
    // Nothing pending, nothing moving: no deadline, nothing busy.
    assert_eq!(rig.engine.next_deadline(), None);
    assert!(!rig.engine.diagnostics().main_busy);

    rig.engine.update(rig.at(0), model(vec![entry(w, a, 11)]));
    rig.engine.tick(rig.at(0) + FRAME);
    assert_eq!(rig.executor.queued(), 1, "one job for the show");
    // The unacknowledged batch is the only deadline: it becomes late at +250 ms.
    let sent = rig.at(0) + FRAME;
    assert_eq!(rig.engine.next_deadline(), Some(sent + BORDER_ACK_WAIT));
    rig.engine.tick(sent + Duration::from_millis(249));
    assert!(!rig.engine.diagnostics().main_busy, "not late yet");

    // Later updates merge into the single pending batch: no second job.
    let mut moved = entry(w, a, 11);
    moved.frame = Rect::new(5.0, 5.0, 300.0, 200.0);
    rig.engine.update(sent + Duration::from_millis(100), model(vec![moved]));
    rig.engine.tick(sent + Duration::from_millis(120));
    assert_eq!(rig.executor.queued(), 1, "latest wins inside one pending batch");

    rig.engine.tick(sent + BORDER_ACK_WAIT);
    let diagnostics = rig.engine.diagnostics();
    assert!(diagnostics.main_busy);
    assert_eq!(MAIN_BUSY_DIAGNOSTIC, "borders: main thread busy");
    assert!(diagnostics.messages().contains(&"borders: main thread busy".to_owned()));
    assert_eq!(
        rig.engine.next_deadline(),
        Some(sent + BORDER_ACK_WAIT + BORDER_ACK_WAIT),
        "flagged: it looks again, only while the batch stays unacknowledged"
    );

    // Main catches up: the batch is acknowledged and the flag clears with the
    // next look; the engine is idle again.
    assert_eq!(rig.executor.drain(), 1);
    rig.engine.tick(sent + BORDER_ACK_WAIT + Duration::from_millis(10));
    assert!(!rig.engine.diagnostics().main_busy);
    assert_eq!(rig.engine.next_deadline(), None);
}

/// The ledger the stop compares: handles held plus releases it could not send.
#[test]
fn retiring_the_ledger_counts_every_handle_without_sending_anything() {
    let mut rig = Rig::inline();
    let (w1, w2) = (rig.window(1, 1), rig.window(1, 2));
    show_all(&mut rig, vec![entry(w1, app(1), 11), entry(w2, app(1), 12)]);
    assert_eq!(rig.engine.ledger_handles(), 2);
    let sent = rig.engine.sent.len();
    assert_eq!(rig.engine.retire_ledger(), 2);
    assert_eq!(rig.engine.sent.len(), sent, "nothing was sent");
    assert_eq!(rig.engine.ledger_handles(), 0);

    // A release dropped by a closed route still counts until the control is asked.
    let mut rig = Rig::inline();
    let w = rig.window(1, 1);
    show_all(&mut rig, vec![entry(w, app(1), 11)]);
    rig.route.close();
    rig.engine.update(rig.at(100), model(Vec::new()));
    assert_eq!(rig.engine.retire_ledger(), 1);
    let ack = rig.route.control().release_all().wait(Duration::ZERO).expect("ack");
    assert_eq!(
        ack.handles, 1,
        "the control still held what the closed route never released"
    );
}

// ------------------------------------------------------------ debug log

#[derive(Default)]
struct CaptureLog {
    records: std::sync::Mutex<Vec<stache_sdk::LogRecord>>,
}

impl stache_sdk::Logger for CaptureLog {
    fn log(&self, record: stache_sdk::LogRecord) { self.records.lock().unwrap().push(record); }
}

impl CaptureLog {
    fn lines(&self) -> Vec<String> {
        self.records.lock().unwrap().iter().map(|r| r.message.clone()).collect()
    }
}

#[test]
fn the_debug_log_stamps_a_show_and_stays_silent_on_an_idle_tick() {
    let mut rig = Rig::inline();
    let capture = Arc::new(CaptureLog::default());
    rig.route.log().set(
        Some(stache_sdk::ScopedLogger::new(
            Arc::clone(&capture) as Arc<dyn stache_sdk::Logger>,
            stache_sdk::ModuleId::new("tiling"),
            stache_sdk::Operation::Start,
        )),
        crate::logclock::LogOrigin::default(),
    );
    let w = rig.window(1, 1);
    show_all(&mut rig, vec![entry(w, app(1), 11)]);
    // Drains the acknowledgement of the show batch.
    rig.engine.tick(rig.at(500));
    let lines = capture.lines();
    assert!(
        lines.iter().all(|l| l.starts_with("[t+") && l.contains("tiling borders: ")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("show window=") && l.contains("released_by=show-rule"))
    );
    assert!(
        lines.iter().any(|l| l.contains("batch sent") && l.contains("show=1")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("main applied shown=1")),
        "{lines:?}"
    );

    capture.records.lock().unwrap().clear();
    assert_eq!(rig.engine.tick(rig.at(1000)), 0);
    rig.engine.tick(rig.at(2000));
    assert_eq!(
        capture.lines(),
        Vec::<String>::new(),
        "an idle tick logs nothing"
    );
}

// ------------------------------------------------------------ configured motion, fade and order

fn borders(value: serde_json::Value) -> BordersConfig {
    serde_json::from_value(value).expect("borders config")
}

/// Two shown borders on one application: `focused_window` focused, the other not.
fn focus_pair() -> (Rig, WindowInstance, WindowInstance) {
    let mut rig = Rig::inline();
    let (a, b) = (rig.window(1, 1), rig.window(1, 2));
    let mut first = entry(a, app(1), 11);
    first.focused = true;
    show_all(&mut rig, vec![first, entry(b, app(1), 12)]);
    (rig, a, b)
}

fn animation_keys(rig: &Rig, panel: u64) -> Vec<&'static str> {
    rig.backend
        .events()
        .into_iter()
        .filter_map(|event| match event {
            FakeEvent::AddAnimation { panel: p, animation } if p == panel => Some(animation.key),
            _ => None,
        })
        .collect()
}

fn panel_of(rig: &Rig, cg: u32) -> u64 {
    rig.backend
        .events()
        .into_iter()
        .find_map(|event| match event {
            FakeEvent::OrderRelative { panel, target, .. } if target == cg => Some(panel),
            _ => None,
        })
        .expect("panel ordered against its target")
}

fn animated(style: &str) -> serde_json::Value {
    serde_json::json!({
        "width": 4,
        "gradient": {"from": "#ff0000", "to": "#0000ff", "angle": 45},
        "animation": {"style": style, "duration": 4000, "easing": "linear"},
    })
}

/// TB-08-04: a configured `loop` installs a rotation on every bordered window
/// whose active state carries it.
#[test]
fn a_configured_loop_rotates_every_border_that_carries_it() {
    let (mut rig, _, _) = focus_pair();
    let panels = [panel_of(&rig, 11), panel_of(&rig, 12)];
    rig.engine.configure(&borders(serde_json::json!({
        "focused": animated("loop"),
        "unfocused": animated("loop"),
    })));
    for panel in panels {
        assert!(
            animation_keys(&rig, panel).contains(&KEY_ROTATE),
            "panel {panel} rotates"
        );
    }
}

/// TB-08-04: a configured `ping_pong` (or a legacy animation) runs only on the
/// focused border.
#[test]
fn a_configured_ping_pong_runs_only_on_the_focused_border() {
    let (mut rig, _, _) = focus_pair();
    let (focused, other) = (panel_of(&rig, 11), panel_of(&rig, 12));
    let legacy = serde_json::json!({
        "width": 4,
        "gradient": {"from": "#ff0000", "to": "#0000ff", "angle": 45},
        "animation": {"duration": 800, "easing": "ease_in_out"},
    });
    rig.engine.configure(&borders(serde_json::json!({
        "focused": legacy,
        "unfocused": animated("ping_pong"),
    })));
    assert!(animation_keys(&rig, focused).contains(&KEY_PING_PONG));
    assert!(animation_keys(&rig, other).is_empty());
}

fn fades(rig: &Rig, panel: u64) -> Vec<f64> {
    rig.backend
        .events()
        .into_iter()
        .filter_map(|event| match event {
            FakeEvent::AddAnimation { panel: p, animation }
                if p == panel && animation.key.starts_with("stache.fade") =>
            {
                Some(animation.duration)
            }
            _ => None,
        })
        .collect()
}

/// TB-08-04: a configured `transition` cross-fades a focus change; a reload
/// with another transition applies on the next focus change.
#[test]
fn a_configured_transition_cross_fades_focus_changes_and_reloads_apply_next() {
    let (mut rig, w) = shown_rig();
    let panel = panel_of(&rig, 11);
    rig.engine
        .configure(&borders(serde_json::json!({"transition": {"duration": 200}})));
    assert!(fades(&rig, panel).is_empty(), "a reload alone never fades");

    // The reload recomputed the unfocused look; focus brings the focused one.
    let mut e = entry(w, app(1), 11);
    e.plan_sequence = 5;
    e.focused = true;
    e.appearance = appearance();
    rig.engine.update(rig.at(100), model(vec![e.clone()]));
    let first = fades(&rig, panel);
    assert!(!first.is_empty(), "the focus change cross-fades");
    assert!(first.iter().all(|d| (d - 0.2).abs() < 1e-9), "{first:?}");

    rig.engine.configure(&borders(serde_json::json!({
        "transition": {"duration": 500, "easing": "linear"}
    })));
    assert_eq!(fades(&rig, panel).len(), first.len(), "no fade on reload");
    e.focused = false;
    e.appearance = other_appearance();
    rig.engine.update(rig.at(200), model(vec![e]));
    let all = fades(&rig, panel);
    assert!(all.len() > first.len());
    assert!(
        all[first.len()..].iter().all(|d| (d - 0.5).abs() < 1e-9),
        "{all:?}"
    );
}

/// Without `transition` a focus change is instant.
#[test]
fn without_a_transition_a_focus_change_is_instant() {
    let (mut rig, w) = shown_rig();
    let panel = panel_of(&rig, 11);
    let mut e = entry(w, app(1), 11);
    e.plan_sequence = 5;
    e.focused = true;
    e.appearance = other_appearance();
    rig.engine.update(rig.at(100), model(vec![e]));
    assert!(fades(&rig, panel).is_empty());
}

fn placements(rig: &Rig, cg: u32) -> Vec<BorderPlacement> {
    rig.backend
        .events()
        .into_iter()
        .filter_map(|event| match event {
            FakeEvent::OrderRelative { target, placement, .. } if target == cg => Some(placement),
            _ => None,
        })
        .collect()
}

/// TB-08-04: `order = "above"` orders overlays above their windows.
#[test]
fn order_above_orders_the_overlays_above() {
    let mut rig = Rig::inline();
    let config = borders(serde_json::json!({"order": "above"}));
    rig.engine.configure(&config);
    let w = rig.window(1, 1);
    let mut e = entry(w, app(1), 11);
    e.appearance = border_appearance(&config, &config.unfocused, 4).expect("appearance");
    show_all(&mut rig, vec![e]);
    assert_eq!(placements(&rig, 11), vec![BorderPlacement::Above]);
}

/// TB-08-04: a reload from `below` to `above` re-orders the live overlays in place.
#[test]
fn a_reload_from_below_to_above_reorders_live_overlays() {
    let (mut rig, _, _) = focus_pair();
    assert_eq!(placements(&rig, 11), vec![BorderPlacement::Below]);
    assert_eq!(placements(&rig, 12), vec![BorderPlacement::Below]);
    rig.engine.configure(&borders(serde_json::json!({"order": "above"})));
    for cg in [11, 12] {
        assert_eq!(
            placements(&rig, cg).last(),
            Some(&BorderPlacement::Above),
            "{cg}"
        );
    }
    assert_eq!(rig.backend.panels_created(), 2, "re-ordered, not recreated");
}

/// A target missing from the on-screen list one frame after its reveal stays
/// hidden without polling, and is confirmed again (once) by its next
/// confirming frame, then shown after one display frame.
#[test]
fn an_unconfirmed_window_shows_after_its_next_confirming_frame() {
    use std::sync::atomic::Ordering::SeqCst;
    let mut rig = Rig::inline();
    let a = rig.window(1, 1);
    let switch = Switch::default();
    switch.away.store(true, SeqCst);
    rig.engine.set_confirmer(Box::new(switch.clone()));
    rig.engine.update(rig.at(0), model(vec![entry(a, app(1), 11)]));
    assert_eq!(rig.engine.tick(rig.at(0) + FRAME), 0);
    assert_eq!(switch.calls.lock().expect("calls").len(), 1);
    assert!(rig.engine.shown_windows().is_empty());

    // No confirming event: nothing asks again, however long it waits.
    switch.away.store(false, SeqCst);
    assert_eq!(rig.engine.next_deadline(), None);
    assert_eq!(rig.engine.tick(rig.at(5000)), 0);
    assert_eq!(switch.calls.lock().expect("calls").len(), 1, "no polling");

    // Its next confirming frame: one more attempt, then one display frame.
    rig.route.report(a, 1, rect(10.0), BorderFrameKind::Settled);
    assert_eq!(rig.engine.tick(rig.at(6000)), 0);
    assert_eq!(rig.engine.tick(rig.at(6000) + FRAME), 1);
    assert_eq!(rig.engine.shown_windows(), vec![a]);
    assert_eq!(switch.calls.lock().expect("calls").len(), 2, "asked once more");
}

/// The application of an unconfirmed window is shown again: one more attempt.
#[test]
fn an_unconfirmed_window_is_confirmed_again_when_its_application_is_shown() {
    use std::sync::atomic::Ordering::SeqCst;
    let mut rig = Rig::inline();
    let a = rig.window(1, 1);
    let switch = Switch::default();
    switch.away.store(true, SeqCst);
    rig.engine.set_confirmer(Box::new(switch.clone()));
    rig.engine.update(rig.at(0), model(vec![entry(a, app(1), 11)]));
    assert_eq!(rig.engine.tick(rig.at(0) + FRAME), 0);
    switch.away.store(false, SeqCst);
    rig.engine.note_application_shown(rig.at(100), app(1));
    assert_eq!(rig.engine.tick(rig.at(100) + FRAME), 1);
    assert_eq!(switch.calls.lock().expect("calls").len(), 2);
}

/// A window refused twice by the Space check, then confirmed: the unconfirmed
/// count is the number of windows currently unconfirmed, back to 0.
#[test]
fn the_unconfirmed_count_returns_to_zero_after_two_refusals_and_a_confirmation() {
    use std::sync::atomic::Ordering::SeqCst;
    let mut rig = Rig::inline();
    let a = rig.window(1, 1);
    let switch = Switch::default();
    switch.away.store(true, SeqCst);
    rig.engine.set_confirmer(Box::new(switch.clone()));
    rig.engine.update(rig.at(0), model(vec![entry(a, app(1), 11)]));
    assert_eq!(rig.engine.tick(rig.at(0) + FRAME), 0);
    assert_eq!(rig.engine.diagnostics().engine.unconfirmed, 1);
    rig.engine.note_application_shown(rig.at(100), app(1));
    assert_eq!(rig.engine.diagnostics().engine.unconfirmed, 0);
    assert_eq!(rig.engine.tick(rig.at(100) + FRAME), 0);
    assert_eq!(
        rig.engine.diagnostics().engine.unconfirmed,
        1,
        "refused twice: one window"
    );
    switch.away.store(false, SeqCst);
    rig.engine.note_application_shown(rig.at(200), app(1));
    assert_eq!(rig.engine.tick(rig.at(200) + FRAME), 1);
    assert_eq!(rig.engine.diagnostics().engine.unconfirmed, 0);
}

// ------------------------------------------------------------ part 09 focus width

fn width_config() -> BordersConfig {
    borders(serde_json::json!({
        "unfocused": false,
        "transition": {"duration": 200, "easing": "linear"},
    }))
}

/// `focused` decides which of two windows is bordered; the other is a
/// collapsed entry (part 09).
fn width_pair(rig: &Rig, focused_first: bool) -> Vec<BorderEntry> {
    let (a, b) = (rig.window(1, 1), rig.window(1, 2));
    let mut first = entry(a, app(1), 11);
    let mut second = entry(b, app(1), 12);
    first.focused = focused_first;
    first.collapsed = !focused_first;
    second.focused = !focused_first;
    second.collapsed = focused_first;
    vec![first, second]
}

fn changes(rig: &Rig) -> Vec<AppearanceChange> {
    rig.commands()
        .into_iter()
        .filter_map(|kind| match kind {
            OverlayKind::Show { change, .. } => Some(change),
            _ => None,
        })
        .collect()
}

fn width_rig() -> Rig {
    let mut rig = Rig::inline();
    rig.engine.configure(&width_config());
    let pair = width_pair(&rig, true);
    show_all(&mut rig, pair);
    assert_eq!(rig.backend.ordered_in(), 1, "a collapsed entry never shows");
    rig
}

/// TB-09-02: a focus change shrinks the old border (kept ordered in, released
/// when the transition is over, through `next_deadline`) and grows the new
/// one from zero after its display frame.
#[test]
fn a_focus_change_shrinks_the_old_border_and_grows_the_new_one() {
    let mut rig = width_rig();
    let pair = width_pair(&rig, false);
    rig.engine.update(rig.at(100), model(pair));
    assert_eq!(changes(&rig), vec![
        AppearanceChange::Reload,
        AppearanceChange::Shrink
    ]);
    assert!(
        rig.backend.is_ordered_in(1),
        "the shrinking border stays ordered in"
    );
    assert!(rig.backend.ring_override(1).is_some());
    assert_eq!(rig.engine.next_deadline(), Some(rig.at(100) + FRAME));
    rig.engine.tick(rig.at(100) + FRAME);
    assert_eq!(changes(&rig).last(), Some(&AppearanceChange::Grow));
    assert_eq!(rig.backend.ordered_in(), 2);
    assert_eq!(rig.engine.next_deadline(), Some(rig.at(300)), "the shrink's end");
    assert_eq!(rig.count(|k| matches!(k, OverlayKind::Release)), 0);
    rig.engine.tick(rig.at(300));
    assert_eq!(rig.count(|k| matches!(k, OverlayKind::Release)), 1);
    assert_eq!(rig.backend.ordered_in(), 1);
    assert_eq!(rig.engine.next_deadline(), None, "idle afterwards");
    assert_eq!(
        rig.engine.ledger_handles(),
        1,
        "an unfocused window holds no overlay"
    );
}

/// TB-09-02: focus flipping back mid-shrink grows the same overlay again; no
/// hide or release follows.
#[test]
fn focus_back_mid_shrink_grows_the_same_overlay() {
    let mut rig = width_rig();
    let away = width_pair(&rig, false);
    rig.engine.update(rig.at(100), model(away));
    let back = width_pair(&rig, true);
    rig.engine.update(rig.at(150), model(back));
    assert_eq!(changes(&rig), vec![
        AppearanceChange::Reload,
        AppearanceChange::Shrink,
        AppearanceChange::Grow,
    ]);
    assert_eq!(rig.engine.next_deadline(), None);
    rig.engine.tick(rig.at(1000));
    assert!(rig.backend.is_ordered_in(1));
    assert_eq!(
        rig.count(|k| matches!(k, OverlayKind::Release | OverlayKind::Hide)),
        0
    );
}

/// TB-09-02: an application hide, a `HideAll` or the window leaving the model
/// during a shrink hides the overlay at once, never after the shrink.
#[test]
fn a_hide_during_a_shrink_is_immediate() {
    // Application hide (the effects thread path).
    let mut rig = width_rig();
    let away = width_pair(&rig, false);
    rig.engine.update(rig.at(100), model(away));
    drop(rig.route.hide_application(app(1)));
    assert!(
        !rig.backend.is_ordered_in(1),
        "hidden before the application hides"
    );

    // Mission Control.
    let mut rig = width_rig();
    let away = width_pair(&rig, false);
    rig.engine.update(rig.at(100), model(away));
    rig.engine.suspend(SuspendReason::MissionControl);
    assert_eq!(rig.backend.ordered_in(), 0);
    assert_eq!(rig.engine.next_deadline(), None);
    // The next update drops the hidden collapsed overlay's reservation (the
    // newly focused window has not shown yet, so nothing is reserved).
    assert_eq!(rig.engine.ledger_handles(), 1);
    let away = width_pair(&rig, false);
    rig.engine.update(rig.at(120), model(away));
    assert_eq!(rig.engine.ledger_handles(), 0);

    // The window leaves the model (workspace switch, close).
    let mut rig = width_rig();
    let away = width_pair(&rig, false);
    rig.engine.update(rig.at(100), model(away.clone()));
    rig.engine.update(rig.at(110), model(away[1..].to_vec()));
    assert!(!rig.backend.is_ordered_in(1));
    assert_eq!(rig.count(|k| matches!(k, OverlayKind::Release)), 1);
}

/// TB-09-01: the model keeps unfocused windows as collapsed entries only when
/// `unfocused = false` and the transition is not instant; a reload hides a
/// shrinking border at once.
#[test]
fn collapsed_entries_need_unfocused_off_and_a_transition() {
    use super::model::collapses_on_blur;
    assert!(collapses_on_blur(&width_config()));
    assert!(!collapses_on_blur(&borders(
        serde_json::json!({"unfocused": false})
    )));
    assert!(!collapses_on_blur(&borders(serde_json::json!({
        "unfocused": false, "transition": {"duration": 0}
    }))));
    assert!(!collapses_on_blur(&borders(serde_json::json!({
        "transition": {"duration": 200}
    }))));

    let mut rig = width_rig();
    let away = width_pair(&rig, false);
    rig.engine.update(rig.at(100), model(away));
    rig.engine.configure(&width_config());
    assert!(!rig.backend.is_ordered_in(1), "a reload is instant");
}

/// Review fix: a reload that draws unfocused windows again (`unfocused`
/// back on) mid-shrink ends the shrink: the border stays at full width, with
/// no release or flicker at the old deadline.
#[test]
fn a_reload_that_draws_a_shrinking_border_again_cancels_its_release() {
    let mut rig = width_rig();
    let away = width_pair(&rig, false);
    rig.engine.update(rig.at(100), model(away));
    rig.engine.configure(&borders(serde_json::json!({
        "transition": {"duration": 200, "easing": "linear"},
    })));
    assert!(rig.backend.is_ordered_in(1));
    assert_eq!(rig.backend.ring_override(1), None, "the full ring again");
    rig.engine.tick(rig.at(300));
    rig.engine.tick(rig.at(1000));
    assert!(rig.backend.is_ordered_in(1), "no release at the old deadline");
    assert_eq!(
        rig.count(|k| matches!(k, OverlayKind::Release | OverlayKind::Hide)),
        0
    );
}

/// Review fix: a collapsed entry is never pending.
#[test]
fn collapsed_entries_are_not_pending() {
    let mut rig = width_rig();
    assert!(rig.engine.pending_windows().is_empty());
    // After a shrink and its release, the unfocused window is collapsed and not
    // shown: it must not read as waiting for the show rule.
    let pair = width_pair(&rig, false);
    rig.engine.update(rig.at(100), model(pair));
    rig.engine.tick(rig.at(100) + FRAME);
    rig.engine.tick(rig.at(300));
    assert_eq!(rig.count(|k| matches!(k, OverlayKind::Release)), 1);
    assert!(
        rig.engine.pending_windows().is_empty(),
        "{:?}",
        rig.engine.pending_windows()
    );
}

// ------------------------------------------------------------ focus ordering

fn placed_entry(rig: &Rig, serial: u64, cg: u32, placement: BorderPlacement) -> BorderEntry {
    let w = rig.window(serial, serial);
    let mut e = entry(w, app(serial), cg);
    e.plan_sequence = 5;
    e.appearance.placement = placement;
    e
}

fn orders(rig: &Rig) -> usize { rig.count(|k| matches!(k, OverlayKind::Order { .. })) }

fn hides(rig: &Rig) -> usize { rig.count(|k| matches!(k, OverlayKind::Hide)) }

#[test]
fn an_above_focus_change_orders_once_now_and_once_after_the_settle() {
    let mut rig = Rig::inline();
    let e = placed_entry(&rig, 1, 11, BorderPlacement::Above);
    let w = e.window;
    show_all(&mut rig, vec![e.clone()]);
    let before = orders(&rig);
    let now = rig.at(100);
    rig.engine.note_focus(now, Some(w));
    // `Effect::Focus` completed and live follow saw the raise: absorbed.
    rig.route.note_raised(w);
    rig.route.note_raised(w);
    rig.engine.reorder_raised(now);
    // The focus recolor is a state change, never an `Order`.
    let mut recolored = e;
    recolored.focused = true;
    recolored.appearance.paint = other_appearance().paint;
    rig.engine.update(now, model(vec![recolored]));
    assert_eq!(orders(&rig) - before, 1, "one immediate order");
    let due = rig.engine.next_deadline().expect("a settle deadline");
    assert!(due > now);
    rig.engine.tick(due.checked_sub(Duration::from_millis(1)).expect("after t0"));
    assert_eq!(orders(&rig) - before, 1);
    rig.engine.tick(due);
    assert_eq!(orders(&rig) - before, 2, "one settle order");
    rig.engine.tick(due + Duration::from_secs(1));
    rig.engine.reorder_raised(due + Duration::from_secs(1));
    assert_eq!(orders(&rig) - before, 2, "never more than one follow-up");
    assert_eq!(rig.engine.next_deadline(), None, "no repeat loop");
    assert_eq!(hides(&rig), 0);
    assert_eq!(rig.order_outs(), 0);
}

#[test]
fn rapid_focus_changes_coalesce_into_one_settle_for_the_last_focus() {
    let mut rig = Rig::inline();
    let a = placed_entry(&rig, 1, 11, BorderPlacement::Above);
    let b = placed_entry(&rig, 2, 12, BorderPlacement::Above);
    let (wa, wb) = (a.window, b.window);
    show_all(&mut rig, vec![a, b]);
    let before = orders(&rig);
    for (i, w) in [wa, wb, wa, wb, wa].into_iter().enumerate() {
        let now = rig.at(100 + i as u64 * 5);
        rig.engine.note_focus(now, Some(w));
        rig.route.note_raised(w);
        rig.engine.reorder_raised(now);
        rig.engine.tick(now);
    }
    assert_eq!(orders(&rig) - before, 5, "one order per focus change");
    let due = rig.engine.next_deadline().expect("settle");
    rig.engine.tick(due);
    rig.engine.tick(due + Duration::from_secs(1));
    assert_eq!(orders(&rig) - before, 6, "a single settle order");
    assert_eq!(hides(&rig), 0);
}

#[test]
fn a_settle_is_skipped_when_focus_moved_away() {
    let mut rig = Rig::inline();
    let a = placed_entry(&rig, 1, 11, BorderPlacement::Above);
    let w = a.window;
    show_all(&mut rig, vec![a]);
    let before = orders(&rig);
    rig.engine.note_focus(rig.at(100), Some(w));
    rig.engine.note_focus(rig.at(101), None);
    rig.engine.tick(rig.at(2000));
    assert_eq!(orders(&rig) - before, 1);
}

#[test]
fn a_below_focus_change_orders_once_and_schedules_no_settle() {
    let mut rig = Rig::inline();
    let e = placed_entry(&rig, 1, 11, BorderPlacement::Below);
    let w = e.window;
    show_all(&mut rig, vec![e]);
    let before = orders(&rig);
    rig.engine.note_focus(rig.at(100), Some(w));
    assert_eq!(orders(&rig) - before, 1);
    assert_eq!(rig.engine.next_deadline(), None);
    rig.engine.tick(rig.at(2000));
    assert_eq!(orders(&rig) - before, 1);
}

#[test]
fn a_raise_intent_on_the_focused_above_window_orders_now_and_once_after_the_settle() {
    let mut rig = Rig::inline();
    let e = placed_entry(&rig, 1, 11, BorderPlacement::Above);
    let w = e.window;
    show_all(&mut rig, vec![e]);
    rig.engine.note_focus(rig.at(100), Some(w));
    rig.engine.tick(rig.at(2000));
    assert_eq!(rig.engine.next_deadline(), None);
    let before = orders(&rig);
    // A click inside the already-focused window (mouse up): no focus change.
    let now = rig.at(3000);
    rig.engine.note_raise_intent(now, w);
    assert_eq!(orders(&rig) - before, 1, "one immediate order");
    let due = rig.engine.next_deadline().expect("a settle deadline");
    rig.engine.tick(due);
    rig.engine.tick(due + Duration::from_secs(1));
    assert_eq!(orders(&rig) - before, 2, "one settle order");
    assert_eq!(rig.engine.next_deadline(), None, "no repeat loop");
    // A raise intent for a window that is not focused does nothing.
    let other = rig.window(9, 9);
    rig.engine.note_raise_intent(rig.at(5000), other);
    assert_eq!(orders(&rig) - before, 2);
    assert_eq!(hides(&rig), 0);
    assert_eq!(rig.order_outs(), 0);
}

#[test]
fn a_raise_intent_on_a_below_window_orders_nothing() {
    let mut rig = Rig::inline();
    let e = placed_entry(&rig, 1, 11, BorderPlacement::Below);
    let w = e.window;
    show_all(&mut rig, vec![e]);
    rig.engine.note_focus(rig.at(100), Some(w));
    let before = orders(&rig);
    rig.engine.note_raise_intent(rig.at(3000), w);
    rig.engine.note_activation(rig.at(3001), w.application());
    rig.engine.tick(rig.at(9000));
    assert_eq!(orders(&rig) - before, 0);
    assert_eq!(rig.engine.next_deadline(), None);
}

#[test]
fn activation_of_the_focused_windows_app_reasserts_once_without_a_focus_change() {
    let mut rig = Rig::inline();
    let a = placed_entry(&rig, 1, 11, BorderPlacement::Above);
    let w = a.window;
    show_all(&mut rig, vec![a]);
    rig.engine.note_focus(rig.at(100), Some(w));
    rig.engine.tick(rig.at(2000));
    let before = orders(&rig);
    // Another application's activation does nothing.
    rig.engine.note_activation(rig.at(3000), app(7));
    assert_eq!(orders(&rig) - before, 0);
    rig.engine.note_activation(rig.at(3000), w.application());
    assert_eq!(orders(&rig) - before, 1);
    let due = rig.engine.next_deadline().expect("settle");
    rig.engine.tick(due);
    rig.engine.tick(due + Duration::from_secs(1));
    assert_eq!(orders(&rig) - before, 2);
    assert_eq!(rig.engine.next_deadline(), None);
}

#[test]
fn a_burst_of_focus_mouse_up_and_activation_coalesces_into_one_settle() {
    let mut rig = Rig::inline();
    let a = placed_entry(&rig, 1, 11, BorderPlacement::Above);
    let w = a.window;
    show_all(&mut rig, vec![a]);
    let before = orders(&rig);
    rig.engine.note_focus(rig.at(100), Some(w));
    rig.engine.note_raise_intent(rig.at(110), w);
    rig.engine.note_activation(rig.at(120), w.application());
    rig.engine.tick(rig.at(121));
    assert_eq!(orders(&rig) - before, 3, "one immediate order per event");
    let due = rig.engine.next_deadline().expect("settle");
    assert!(due > rig.at(120), "measured from the last event");
    rig.engine.tick(due);
    rig.engine.tick(due + Duration::from_secs(1));
    assert_eq!(orders(&rig) - before, 4, "a single settle order");
    assert_eq!(rig.engine.next_deadline(), None);
    assert_eq!(hides(&rig), 0);
}

#[test]
fn a_raise_intent_while_suspended_orders_nothing() {
    let mut rig = Rig::inline();
    let a = placed_entry(&rig, 1, 11, BorderPlacement::Above);
    let w = a.window;
    show_all(&mut rig, vec![a]);
    rig.engine.note_focus(rig.at(100), Some(w));
    rig.engine.tick(rig.at(2000));
    rig.engine.suspend(SuspendReason::MissionControl);
    let before = orders(&rig);
    rig.engine.note_raise_intent(rig.at(3000), w);
    rig.engine.tick(rig.at(9000));
    assert_eq!(orders(&rig) - before, 0);
}
