//! Live follow and the public fallback at the engine level (part 06), over the
//! fakes of `engine_tests` plus `FakeLiveFollow`. No `SkyLight` symbol is
//! resolved, no display link created, no callback registered.
//!
//! Spec acceptance mapping (`docs/specs/2026-11-09-tiling-borders/06-live-follow.md`):
//!
//! * TB-06-01 (capability, one diagnostic, subscription cap):
//!   `an_unavailable_live_follow_has_exactly_one_diagnostic_and_nothing_else_is_disabled`,
//!   `the_bordered_set_is_pushed_only_when_it_changes_and_never_exceeds_the_cap`,
//!   plus the owner-thread tests in `stache-macos` (`border_overlay::live_follow`).
//! * TB-06-02 (authority, order): `a_live_sample_moves_the_border_*`,
//!   `a_tiling_frame_supersedes_a_live_sample_read_before_it`,
//!   `live_samples_wait_while_a_tiling_write_is_in_flight`, `a_live_move_after_hide_all_never_shows`,
//!   `private_*`, `unhide_never_shows_by_itself`.
//! * TB-06-03 (fallback): `an_external_move_hides_the_border_and_it_returns_after_the_window_settles`,
//!   `a_drag_start_hides_the_border_until_the_gesture_ended_and_the_frame_settled`,
//!   `with_live_follow_active_an_external_sample_is_not_applied_and_holds_nothing`.

use stache_macos::border_overlay::{FakeLiveFollow, OverlayRect, unavailable_message};

use super::*;
use crate::borders::engine::{FOLLOW_GRACE, HOLD_QUIET, LIVE_REORDER_CAP};

const MISSING: &str = "missing symbol SLSGetWindowBounds";

/// An engine with a live follow source, one shown border (`w`, cg 11, plan 5).
fn live_rig(live: &FakeLiveFollow) -> (Rig, WindowInstance) {
    let mut rig = Rig::inline();
    rig.engine.set_live_follow(Box::new(live.clone()));
    rig.engine.start_live();
    let w = rig.window(1, 1);
    let mut e = entry(w, app(1), 11);
    e.plan_sequence = 5;
    show_all(&mut rig, vec![e]);
    assert_eq!(rig.engine.shown_windows(), vec![w]);
    (rig, w)
}

fn moved(w: WindowInstance, x: f64) -> BorderEntry {
    let mut e = entry(w, app(1), 11);
    e.plan_sequence = 5;
    e.frame = Rect::new(x, 0.0, 100.0, 100.0);
    e
}

fn hides(rig: &Rig) -> usize { rig.count(|k| matches!(k, OverlayKind::Hide)) }

fn moves(rig: &Rig) -> usize { rig.count(|k| matches!(k, OverlayKind::Move { .. })) }

// ------------------------------------------------------------- capability

#[test]
fn an_unavailable_live_follow_has_exactly_one_diagnostic_and_nothing_else_is_disabled() {
    let live = FakeLiveFollow::unavailable(MISSING);
    let (rig, w) = live_rig(&live);
    let messages = rig.engine.diagnostics().messages();
    let expected = unavailable_message(MISSING);
    assert_eq!(messages.iter().filter(|m| **m == expected).count(), 1);
    // Nothing else is reported or disabled: the rest is what a rig without a
    // live source reports.
    let (baseline, _) = shown_rig();
    let mut others = messages;
    others.retain(|m| *m != expected);
    assert_eq!(others, baseline.engine.diagnostics().messages());
    assert_eq!(
        expected,
        "borders: live follow unavailable (missing symbol SLSGetWindowBounds); borders hide while a window moves"
    );
    // The border still shows, recolors and follows Tiling frames.
    assert_eq!(rig.engine.shown_windows(), vec![w]);
    rig.route.report(w, 5, rect(120.0), BorderFrameKind::Target);
    assert_eq!(rig.drawn_width(), Some(120.0));
    // An active source reports nothing.
    let active = FakeLiveFollow::active();
    let (rig, _) = live_rig(&active);
    assert_eq!(
        rig.engine.diagnostics().messages(),
        baseline.engine.diagnostics().messages()
    );
    assert!(rig.engine.diagnostics().live.is_none());
}

#[test]
fn the_source_starts_with_the_engine_and_stops_with_it() {
    let live = FakeLiveFollow::active();
    let mut rig = Rig::inline();
    rig.engine.set_live_follow(Box::new(live.clone()));
    assert_eq!(live.started(), 0, "nothing starts at construction");
    rig.engine.start_live();
    rig.engine.start_live();
    assert_eq!(live.started(), 1, "once per enabled period");
    assert!(live.sink().is_some());
    rig.engine.stop_live();
    assert_eq!(live.stopped(), 1);
    assert!(live.sink().is_none());
}

#[test]
fn the_bordered_set_is_pushed_only_when_it_changes_and_never_exceeds_the_cap() {
    let live = FakeLiveFollow::active();
    let (mut rig, w) = live_rig(&live);
    let sets = live.target_sets();
    assert_eq!(sets.last().unwrap().len(), 1);
    assert_eq!(sets.last().unwrap()[0].cg_window_id, 11);
    let pushed = sets.len();
    // The same model again, a frame change and a tick push nothing.
    rig.engine.update(
        rig.at(50),
        model(vec![{
            let mut e = entry(w, app(1), 11);
            e.plan_sequence = 5;
            e
        }]),
    );
    rig.engine.tick(rig.at(60));
    assert_eq!(
        live.target_sets().len(),
        pushed,
        "coalesced: unchanged set, no push"
    );
    // More windows than overlays: the pushed set is bounded.
    let many: Vec<BorderEntry> = (1..=100)
        .map(|n| entry(rig.window(1, n), app(1), u32::try_from(10 + n).unwrap()))
        .collect();
    rig.engine.update(rig.at(100), model(many));
    assert!(live.target_sets().len() > pushed);
    assert!(live.target_sets().iter().all(|set| set.len() <= MAX_OVERLAYS));
    assert_eq!(live.target_sets().last().unwrap().len(), MAX_OVERLAYS);
}

// ------------------------------------------------------------------ authority

#[test]
fn a_live_sample_moves_the_border_with_one_batch_and_no_tiling_input() {
    let live = FakeLiveFollow::active();
    let (rig, w) = live_rig(&live);
    let sink = live.sink().expect("sink");
    let configures = rig.configures();
    let stamp = sink.authority(w).expect("Tiling owns nothing: a live sample may be read");
    let sent = rig.engine.sent.len();
    sink.moves(&[stache_macos::border_overlay::FollowMove {
        window: w,
        bounds: OverlayRect::new(40.0, 5.0, 100.0, 100.0),
        stamp,
    }]);
    assert_eq!(rig.configures(), configures + 1, "the overlay moved");
    assert_eq!(rig.route.counters().live_moves, 1);
    assert_eq!(
        rig.route.latest_frame(w),
        Some(Rect::new(40.0, 5.0, 100.0, 100.0)),
        "the route knows the live frame"
    );
    assert_eq!(
        rig.engine.sent.len(),
        sent,
        "the engine sent nothing: live follow is the route's"
    );
}

#[test]
fn a_tiling_frame_supersedes_a_live_sample_read_before_it() {
    let live = FakeLiveFollow::active();
    let (rig, w) = live_rig(&live);
    let sink = live.sink().expect("sink");
    let stamp = sink.authority(w).expect("stamp");
    // A Tiling animation frame for the current plan arrives after the read.
    rig.route.report(w, 5, rect(130.0), BorderFrameKind::AnimationFrame);
    let configures = rig.configures();
    sink.moves(&[stache_macos::border_overlay::FollowMove {
        window: w,
        bounds: OverlayRect::new(900.0, 0.0, 100.0, 100.0),
        stamp,
    }]);
    assert_eq!(rig.configures(), configures, "the stale live sample was dropped");
    assert_eq!(rig.route.counters().live_dropped, 1);
    assert_eq!(
        rig.route.latest_frame(w),
        Some(rect(130.0)),
        "the Tiling frame stands"
    );
}

#[test]
fn live_samples_wait_while_a_tiling_write_is_in_flight() {
    let live = FakeLiveFollow::active();
    let (mut rig, w) = live_rig(&live);
    let sink = live.sink().expect("sink");
    assert!(sink.authority(w).is_some());
    let mut e = entry(w, app(1), 11);
    e.plan_sequence = 6;
    e.write_pending = Some(6);
    rig.engine.update(rig.at(70), model(vec![e.clone()]));
    assert_eq!(
        sink.authority(w),
        None,
        "Tiling owns the window: nothing is read"
    );
    e.write_pending = None;
    rig.engine.update(rig.at(80), model(vec![e]));
    assert!(sink.authority(w).is_some(), "the write completed");
}

#[test]
fn a_live_move_after_hide_all_never_shows() {
    let live = FakeLiveFollow::active();
    let (mut rig, w) = live_rig(&live);
    let sink = live.sink().expect("sink");
    let stamp = sink.authority(w).expect("stamp");
    let handle = rig.engine.handle_of(w).expect("handle");
    rig.engine.suspend(SuspendReason::MissionControl);
    let ordered_in = rig.backend.ordered_in();
    assert_eq!(ordered_in, 0, "HideAll ordered the overlay out");
    // The sample was read before the HideAll: the route drops it ...
    sink.moves(&[stache_macos::border_overlay::FollowMove {
        window: w,
        bounds: OverlayRect::new(10.0, 10.0, 100.0, 100.0),
        stamp,
    }]);
    assert_eq!(rig.route.counters().live_moves, 0);
    // ... and even a Move that reaches the control never orders a window in.
    drop(
        rig.route
            .control()
            .apply(stache_macos::border_overlay::OverlayBatch::new().with(
                handle,
                OverlayKind::Move {
                    frame: OverlayRect::new(20.0, 20.0, 100.0, 100.0),
                },
            )),
    );
    assert_eq!(
        rig.backend.ordered_in(),
        0,
        "a Move never shows a hidden overlay"
    );
    assert_eq!(sink.authority(w), None, "no sample is even read while hidden");
}

// ----------------------------------------------------------- hide and order

#[test]
fn private_hide_hides_earlier_and_only_an_unhide_plus_the_show_rule_bring_it_back() {
    let live = FakeLiveFollow::active();
    let (mut rig, w) = live_rig(&live);
    let sink = live.sink().expect("sink");
    sink.hide(w);
    assert_eq!(
        rig.backend.ordered_in(),
        0,
        "ordered out at once, before the engine looks"
    );
    // The engine learns of it and holds the border.
    assert_eq!(rig.engine.tick(rig.at(500)), 0);
    assert!(rig.engine.is_held_back(w));
    assert_eq!(
        rig.engine.tick(rig.at(5000)),
        0,
        "no time brings it back by itself"
    );
    assert_eq!(rig.backend.ordered_in(), 0);
    // The unhide lifts the hold only; the show rule then waits one frame.
    let before = rig.shows();
    sink.unhide(w);
    assert_eq!(rig.shows(), before, "the unhide itself shows nothing");
    assert_eq!(rig.backend.ordered_in(), 0);
    assert_eq!(rig.engine.tick(rig.at(6000)), 0, "the frame has not passed");
    assert!(!rig.engine.is_held_back(w));
    assert_eq!(rig.engine.tick(rig.at(6000) + FRAME), 1);
    assert_eq!(rig.backend.ordered_in(), 1);
}

#[test]
fn unhide_never_shows_by_itself() {
    // Application hidden: the show rule says no.
    let live = FakeLiveFollow::active();
    let (mut rig, w) = live_rig(&live);
    let sink = live.sink().expect("sink");
    sink.hide(w);
    rig.engine.tick(rig.at(10));
    drop(rig.route.hide_application(app(1)));
    sink.unhide(w);
    assert_eq!(rig.engine.tick(rig.at(5000)), 0);
    assert_eq!(rig.backend.ordered_in(), 0, "the application is still hidden");

    // Space gate suspended: the gate says no.
    let live = FakeLiveFollow::active();
    let (mut rig, w) = live_rig(&live);
    let sink = live.sink().expect("sink");
    rig.engine.suspend(SuspendReason::MissionControl);
    sink.unhide(w);
    assert_eq!(rig.engine.tick(rig.at(5000)), 0);
    assert_eq!(rig.backend.ordered_in(), 0, "Mission Control is still active");

    // A window that never was shown: an unhide creates nothing.
    let live = FakeLiveFollow::active();
    let mut rig = Rig::inline();
    rig.engine.set_live_follow(Box::new(live.clone()));
    rig.engine.start_live();
    let w = rig.window(1, 1);
    let mut e = entry(w, app(1), 11);
    e.write_pending = Some(9);
    rig.engine.update(rig.at(0), model(vec![e]));
    live.sink().expect("sink").unhide(w);
    assert_eq!(rig.engine.tick(rig.at(5000)), 0, "the write is still pending");
    assert_eq!(rig.backend.ordered_in(), 0);
}

#[test]
fn private_reorder_re_orders_an_above_border_and_not_while_suspended() {
    let (mut rig, live, w) = reorder_rig(BorderPlacement::Above);
    let sink = live.sink().expect("sink");
    sink.reorder(w);
    rig.engine.reorder_raised(rig.at(1000));
    assert!(matches!(
        rig.engine.sent.last().expect("batch").commands[0].kind,
        OverlayKind::Order { target_cg_id: 11 }
    ));
    let orders = rig.count(|k| matches!(k, OverlayKind::Order { .. }));
    rig.engine.suspend(SuspendReason::MissionControl);
    sink.reorder(w);
    rig.engine.reorder_raised(rig.at(1000));
    assert_eq!(
        rig.count(|k| matches!(k, OverlayKind::Order { .. })),
        orders,
        "no Order while the gate holds a reason"
    );
}

#[test]
fn private_close_releases_the_overlay_and_it_is_not_recreated_for_a_lingering_model() {
    let live = FakeLiveFollow::active();
    let (mut rig, w) = live_rig(&live);
    let sink = live.sink().expect("sink");
    sink.close(w);
    assert_eq!(rig.backend.ordered_in(), 0);
    rig.engine.tick(rig.at(100));
    assert_eq!(rig.count(|k| matches!(k, OverlayKind::Release)), 1);
    assert_eq!(rig.engine.handle_of(w), None);
    // Tiling has not heard of the close yet: the model still lists the window.
    rig.engine.update(rig.at(200), model(vec![entry(w, app(1), 11)]));
    assert_eq!(rig.engine.tick(rig.at(5000)), 0);
    assert_eq!(
        rig.backend.ordered_in(),
        0,
        "a destroyed window gets no border back"
    );
    // Once the model drops it, a window reusing the entry may be bordered again.
    rig.engine.update(rig.at(300), model(vec![]));
    rig.engine.update(rig.at(400), model(vec![entry(w, app(1), 11)]));
    assert_eq!(rig.engine.tick(rig.at(400) + FRAME), 1);
}

// ---------------------------------------------------------- public fallback

#[test]
fn an_external_move_hides_the_border_and_it_returns_after_the_window_settles() {
    let live = FakeLiveFollow::unavailable(MISSING);
    let (mut rig, w) = live_rig(&live);
    assert_eq!(hides(&rig), 0);
    // A coalesced sample Tiling did not cause: no write in flight, no Tiling frame.
    rig.engine.update(rig.at(100), model(vec![moved(w, 30.0)]));
    assert_eq!(hides(&rig), 1, "hidden at the first external sample");
    assert_eq!(moves(&rig), 0, "never lagging behind the window");
    assert_eq!(rig.backend.ordered_in(), 0);
    assert!(rig.engine.is_held_back(w));
    // More samples keep it hidden and refresh the quiet timer.
    rig.engine.update(rig.at(150), model(vec![moved(w, 60.0)]));
    rig.engine.update(rig.at(200), model(vec![moved(w, 90.0)]));
    assert_eq!(rig.engine.tick(rig.at(199) + HOLD_QUIET), 0);
    assert_eq!(rig.engine.next_deadline(), Some(rig.at(200) + HOLD_QUIET));
    // Quiet: the show rule applies (one display frame) at the settled frame.
    assert_eq!(
        rig.engine.tick(rig.at(200) + HOLD_QUIET),
        0,
        "released; the frame has not passed"
    );
    assert_eq!(rig.engine.tick(rig.at(200) + HOLD_QUIET + FRAME), 1);
    assert_eq!(rig.backend.ordered_in(), 1);
    assert_eq!(
        rig.route.latest_frame(w),
        Some(Rect::new(90.0, 0.0, 100.0, 100.0))
    );
    assert!(!rig.engine.is_held_back(w));
}

#[test]
fn a_tiling_caused_frame_change_is_not_external() {
    let live = FakeLiveFollow::unavailable(MISSING);
    let (mut rig, w) = live_rig(&live);
    // A write in flight explains the change.
    let mut e = moved(w, 30.0);
    e.write_pending = Some(6);
    rig.engine.update(rig.at(100), model(vec![e]));
    assert_eq!(hides(&rig), 0);
    assert_eq!(moves(&rig), 1, "a Tiling frame moves the border as before");
    // A Tiling frame event explains the change even with the write settled.
    rig.route.report(
        w,
        6,
        Rect::new(70.0, 0.0, 100.0, 100.0),
        BorderFrameKind::Settled,
    );
    let mut e = moved(w, 70.0);
    e.plan_sequence = 6;
    rig.engine.update(rig.at(200), model(vec![e]));
    assert_eq!(hides(&rig), 0);
    assert!(rig.engine.shown_windows().contains(&w));
}

#[test]
fn a_drag_hides_the_border_from_its_first_motion_until_the_gesture_ended_and_the_frame_settled() {
    let live = FakeLiveFollow::unavailable(MISSING);
    let (mut rig, w) = live_rig(&live);
    // Mouse down on the bordered window (the model's `pressed` set): a plain
    // click must not flicker the border, so nothing hides yet.
    let pressed = |entries: Vec<BorderEntry>| BorderModel {
        pressed: BTreeSet::from([w]),
        ..model(entries)
    };
    rig.engine.update(rig.at(100), pressed(vec![entry(w, app(1), 11)]));
    assert_eq!(hides(&rig), 0, "a mouse down alone hides nothing");
    // The first external motion hides it; however long the gesture lasts, the
    // border stays hidden.
    rig.engine.update(rig.at(120), pressed(vec![moved(w, 40.0)]));
    assert_eq!(hides(&rig), 1, "hidden at the first motion of the drag");
    assert_eq!(rig.engine.tick(rig.at(30_000)), 0);
    assert_eq!(rig.backend.ordered_in(), 0);
    // The mouse went up; the settled snapshot decided the gesture and Tiling
    // writes the final frame (snap-back or swap): the border waits for it.
    let mut settling = moved(w, 0.0);
    settling.write_pending = Some(6);
    settling.plan_sequence = 6;
    rig.engine.update(rig.at(40_000), model(vec![settling.clone()]));
    assert_eq!(
        rig.engine.tick(rig.at(40_000) + HOLD_QUIET + FRAME),
        0,
        "write still in flight"
    );
    // The write is confirmed at the final frame: the show rule shows it.
    rig.route
        .report(w, 6, Rect::new(0.0, 0.0, 100.0, 100.0), BorderFrameKind::Settled);
    settling.write_pending = None;
    rig.engine.update(rig.at(40_200), model(vec![settling]));
    assert_eq!(rig.engine.tick(rig.at(40_200) + FRAME), 1);
    assert_eq!(rig.backend.ordered_in(), 1);
}

#[test]
fn a_plain_click_without_live_follow_never_hides_the_border() {
    let live = FakeLiveFollow::unavailable(MISSING);
    let (mut rig, w) = live_rig(&live);
    rig.engine.update(rig.at(100), BorderModel {
        pressed: BTreeSet::from([w]),
        ..model(vec![entry(w, app(1), 11)])
    });
    // Mouse up with no motion: the border never left.
    rig.engine.update(rig.at(180), model(vec![entry(w, app(1), 11)]));
    assert_eq!(rig.engine.tick(rig.at(400)), 0);
    assert_eq!(hides(&rig), 0);
    assert!(rig.engine.shown_windows().contains(&w));
}

#[test]
fn with_live_follow_active_a_mouse_down_hides_nothing() {
    let live = FakeLiveFollow::active();
    let (mut rig, w) = live_rig(&live);
    rig.engine.update(rig.at(100), BorderModel {
        pressed: BTreeSet::from([w]),
        ..model(vec![entry(w, app(1), 11)])
    });
    assert_eq!(hides(&rig), 0, "the border follows the drag instead");
    assert!(rig.engine.shown_windows().contains(&w));
}

#[test]
fn with_live_follow_active_an_external_sample_is_not_applied_and_holds_nothing() {
    let live = FakeLiveFollow::active();
    let (mut rig, w) = live_rig(&live);
    let sink = live.sink().expect("sink");
    // The follower follows the window.
    sink.following(w, true);
    rig.engine.update(rig.at(100), model(vec![moved(w, 30.0)]));
    assert_eq!(hides(&rig), 0);
    assert_eq!(
        moves(&rig),
        0,
        "the live bounds are newer than the coalesced sample"
    );
    assert_eq!(rig.engine.diagnostics().engine.live_suppressed, 1);
    // The quiet period passed; the final coalesced sample still belongs to the drag.
    sink.following(w, false);
    rig.engine.update(rig.at(200), model(vec![moved(w, 60.0)]));
    assert_eq!(hides(&rig), 0);
    assert_eq!(moves(&rig), 0);
    assert_eq!(rig.engine.diagnostics().engine.live_suppressed, 2);
}

#[test]
fn with_live_follow_active_but_silent_for_the_window_the_fallback_still_hides() {
    // Registered, yet no event ever arrives for this window (a wrong payload or
    // event id): the first external sample falls back to hiding.
    let live = FakeLiveFollow::active();
    let (mut rig, w) = live_rig(&live);
    rig.engine.set_follow_grace(FOLLOW_GRACE);
    rig.engine.update(rig.at(100), model(vec![moved(w, 30.0)]));
    assert_eq!(hides(&rig), 1);
    assert_eq!(moves(&rig), 0);
}

#[test]
fn without_a_live_source_the_engine_behaves_as_before_part_06() {
    let (mut rig, w) = shown_rig();
    rig.engine.update(
        rig.at(100),
        model(vec![{
            let mut e = entry(w, app(1), 11);
            e.plan_sequence = 5;
            e.frame = Rect::new(30.0, 0.0, 100.0, 100.0);
            e
        }]),
    );
    assert_eq!(hides(&rig), 0);
    assert_eq!(moves(&rig), 1);
    assert!(rig.engine.live_status().is_none());
    assert!(rig.engine.diagnostics().live.is_none());
}

// ------------------------------------------- window-server reorder (`above`)

/// An engine with an active live follow and one shown border placed `placement`.
fn reorder_rig(placement: BorderPlacement) -> (Rig, FakeLiveFollow, WindowInstance) {
    let live = FakeLiveFollow::active();
    let mut rig = Rig::inline();
    rig.engine.set_live_follow(Box::new(live.clone()));
    rig.engine.start_live();
    let e = placed_entry(&rig, 1, 11, placement);
    let w = e.window;
    show_all(&mut rig, vec![e]);
    rig.engine.note_focus(rig.at(50), Some(w));
    (rig, live, w)
}

fn order_count(rig: &Rig) -> usize { rig.count(|k| matches!(k, OverlayKind::Order { .. })) }

#[test]
fn a_reorder_event_for_an_above_window_orders_its_overlay_once() {
    let (mut rig, live, w) = reorder_rig(BorderPlacement::Above);
    let before = order_count(&rig);
    live.sink().expect("sink").reorder(w);
    rig.engine.tick(rig.at(1000));
    assert_eq!(order_count(&rig) - before, 1);
    assert_eq!(rig.engine.diagnostics().engine.live_reorders, 1);
    rig.engine.tick(rig.at(2000));
    assert_eq!(order_count(&rig) - before, 1, "nothing repeats by itself");
    assert_eq!(rig.engine.next_deadline(), None);
    assert_eq!(rig.order_outs(), 0);
}

#[test]
fn several_reorder_events_in_one_frame_give_one_order() {
    let (mut rig, live, w) = reorder_rig(BorderPlacement::Above);
    let sink = live.sink().expect("sink");
    let before = order_count(&rig);
    sink.reorder(w);
    sink.reorder(w);
    rig.engine.tick(rig.at(1000));
    // More notifications inside the same display frame fold into one `Order`
    // at the end of that frame.
    sink.reorder(w);
    rig.engine.tick(rig.at(1001));
    sink.reorder(w);
    rig.engine.tick(rig.at(1002));
    assert_eq!(order_count(&rig) - before, 1, "one order inside the frame");
    let due = rig.engine.next_deadline().expect("the frame's follow-up");
    assert_eq!(due, rig.at(1000) + FRAME);
    rig.engine.tick(due);
    rig.engine.tick(due + Duration::from_millis(500));
    assert_eq!(
        order_count(&rig) - before,
        2,
        "one more at the next frame, never a storm"
    );
}

#[test]
fn a_reorder_feedback_loop_is_capped_with_a_diagnostic() {
    let (mut rig, live, w) = reorder_rig(BorderPlacement::Above);
    let sink = live.sink().expect("sink");
    let before = order_count(&rig);
    // Our own `Order` re-stacks the window and notifies again, every frame.
    for i in 0..20_u64 {
        sink.reorder(w);
        rig.engine.tick(rig.at(1000 + i * 20));
    }
    assert_eq!(order_count(&rig) - before, LIVE_REORDER_CAP);
    assert!(rig.engine.diagnostics().engine.live_reorders_capped > 0);
    // After the window the cap lifts again.
    sink.reorder(w);
    rig.engine.tick(rig.at(5000));
    assert_eq!(order_count(&rig) - before, LIVE_REORDER_CAP + 1);
}

#[test]
fn a_reorder_event_for_a_below_window_orders_nothing() {
    let (mut rig, live, w) = reorder_rig(BorderPlacement::Below);
    let before = order_count(&rig);
    live.sink().expect("sink").reorder(w);
    rig.engine.tick(rig.at(1000));
    assert_eq!(order_count(&rig) - before, 0);
    assert_eq!(rig.engine.next_deadline(), None);
}

#[test]
fn a_reorder_event_while_suspended_orders_nothing() {
    let (mut rig, live, w) = reorder_rig(BorderPlacement::Above);
    rig.engine.suspend(SuspendReason::MissionControl);
    let before = order_count(&rig);
    live.sink().expect("sink").reorder(w);
    rig.engine.tick(rig.at(1000));
    rig.engine.tick(rig.at(3000));
    assert_eq!(order_count(&rig) - before, 0);
}

#[test]
fn a_reorder_event_for_an_unbordered_window_orders_nothing() {
    let (mut rig, live, _) = reorder_rig(BorderPlacement::Above);
    let before = order_count(&rig);
    live.sink().expect("sink").reorder(rig.window(9, 9));
    rig.engine.tick(rig.at(1000));
    assert_eq!(order_count(&rig) - before, 0);
}

#[test]
fn with_live_follow_active_a_focus_change_orders_once_and_schedules_nothing() {
    let live = FakeLiveFollow::active();
    let mut rig = Rig::inline();
    rig.engine.set_live_follow(Box::new(live));
    rig.engine.start_live();
    let e = placed_entry(&rig, 1, 11, BorderPlacement::Above);
    let w = e.window;
    show_all(&mut rig, vec![e]);
    let before = order_count(&rig);
    rig.engine.note_focus(rig.at(100), Some(w));
    assert_eq!(order_count(&rig) - before, 1);
    assert_eq!(rig.engine.next_deadline(), None, "no timer");
    rig.engine.tick(rig.at(2000));
    assert_eq!(order_count(&rig) - before, 1);
}

// ------------------------------------------ fast reorder (owner-thread) facts

#[test]
fn the_targets_update_carries_the_panel_number_placement_and_fast_reorder_permission() {
    let (mut rig, live, w) = reorder_rig(BorderPlacement::Above);
    rig.engine.tick(rig.at(100));
    let last = live.target_sets().last().unwrap().clone();
    let target = last.iter().find(|t| t.window == w).expect("bordered");
    assert!(
        target.overlay_window.is_some(),
        "the main thread's window number"
    );
    assert!(target.above);
    assert!(target.fast_reorder, "shown under an open gate");
    assert_eq!(target.fast_panel(), target.overlay_window);
    // A suspension withdraws the permission with the hiding batch itself.
    rig.engine.suspend(SuspendReason::MissionControl);
    let last = live.target_sets().last().unwrap().clone();
    let target = last.iter().find(|t| t.window == w).expect("still bordered");
    assert!(!target.fast_reorder, "suspended: no fast reorder");
    assert_eq!(target.fast_panel(), None);

    let (mut rig, live, w) = reorder_rig(BorderPlacement::Below);
    rig.engine.tick(rig.at(100));
    let last = live.target_sets().last().unwrap().clone();
    let target = last.iter().find(|t| t.window == w).expect("bordered");
    assert!(!target.above);
    assert_eq!(target.fast_panel(), None, "below never fast-reorders");
}

// ------------------------------------------------- the end of a Tiling write

/// The model during and after a Tiling write of plan 6 at `width`.
fn writing(w: WindowInstance, width: f64, pending: bool) -> BorderEntry {
    let mut e = entry(w, app(1), 11);
    e.plan_sequence = 6;
    e.write_pending = pending.then_some(6);
    e.frame = Rect::new(0.0, 0.0, width, 100.0);
    e
}

/// The final frame was settled (a confirming report), but the model is left at
/// a frame read during the motion: the border stays on the target, with and
/// without live follow, and nothing hides.
#[test]
fn a_write_that_settled_keeps_the_border_on_the_target_whatever_the_model_says() {
    for live in [None, Some(FakeLiveFollow::active())] {
        let (mut rig, w) = live.as_ref().map_or_else(shown_rig, live_rig);
        rig.engine.set_follow_grace(FOLLOW_GRACE);
        rig.route.report(
            w,
            6,
            Rect::new(0.0, 0.0, 150.0, 100.0),
            BorderFrameKind::AnimationFrame,
        );
        rig.engine.update(rig.at(100), model(vec![writing(w, 130.0, true)]));
        rig.route
            .report(w, 6, Rect::new(0.0, 0.0, 180.0, 100.0), BorderFrameKind::Settled);
        assert_eq!(rig.drawn_width(), Some(180.0));
        let before = moves(&rig);
        rig.engine.update(rig.at(200), model(vec![writing(w, 130.0, false)]));
        assert_eq!(
            rig.drawn_width(),
            Some(180.0),
            "live {}: on the target",
            live.is_some()
        );
        assert_eq!(moves(&rig), before, "no move back");
        assert_eq!(hides(&rig), 0, "no hide");
    }
}

/// A write that ends with no report (partial without a read, cancelled,
/// stale, sealed) while live follow is active: the held model frame applies
/// with one move, and nothing blinks.
#[test]
fn a_write_that_ends_unreported_with_live_follow_moves_once_without_a_blink() {
    let live = FakeLiveFollow::active();
    let (mut rig, w) = live_rig(&live);
    rig.engine.set_follow_grace(FOLLOW_GRACE);
    rig.route.report(
        w,
        6,
        Rect::new(0.0, 0.0, 150.0, 100.0),
        BorderFrameKind::AnimationFrame,
    );
    rig.engine.update(rig.at(100), model(vec![writing(w, 170.0, true)]));
    assert_eq!(rig.drawn_width(), Some(150.0));
    let (before_moves, before_shows) = (moves(&rig), rig.shows());
    rig.engine.update(rig.at(200), model(vec![writing(w, 170.0, false)]));
    assert_eq!(rig.drawn_width(), Some(170.0));
    assert_eq!(moves(&rig), before_moves + 1, "one move");
    assert_eq!((hides(&rig), rig.shows()), (0, before_shows), "no hide, no show");
}
