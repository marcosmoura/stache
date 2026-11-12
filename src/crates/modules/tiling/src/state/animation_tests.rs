//! TI-08-04 (state): the pure planning of animated frame writes. Whether a plan
//! animates, the A, B, A history, bounded echo suppression and the drag gesture.
//! No threads, backend or timers.

use std::sync::Arc;
use std::time::Duration;

use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead, WriteStatus};
use stache_macos::windows::{IssuedKind, NativeStatus, WindowActionOutcome, WindowInstance};

use super::frame_tests::{Fixture, one_screen};
use super::*;
use crate::animation::MotionKind;
use crate::config::TilingConfig;
use crate::drag::{DragPoint, DragSignal};
use crate::geometry::Rect;
use crate::state::frames::MAX_SETTLE_ECHOES;

/// `(0, 25, 1920, 1055)`: the one window of a zero-gap workspace.
const T0: Rect = Rect::new(0.0, 25.0, 1920.0, 1055.0);

fn config(animations: bool, outer: u32) -> Arc<TilingConfig> {
    Arc::new(
        serde_json::from_value(serde_json::json!({
            "gaps": { "inner": 0, "outer": outer },
            "animations": { "enabled": animations },
        }))
        .expect("config"),
    )
}

fn fixture(animations: bool) -> Fixture { Fixture::new(one_screen(), config(animations, 0), 1) }

fn read(frame: Rect) -> FrameRead {
    FrameRead {
        position: AxReading::Value(AxPoint { x: frame.x, y: frame.y }),
        size: AxReading::Value(AxSize {
            width: frame.width,
            height: frame.height,
        }),
    }
}

fn applied_at(frame: Rect) -> WindowActionOutcome {
    WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        native: NativeStatus::Frame {
            position: WriteStatus::Written,
            size: WriteStatus::Written,
        },
        observed: Some(read(frame)),
    }
}

struct Write {
    plan: u64,
    target: Rect,
    from: Rect,
    animate: bool,
    refresh_hz: Option<f64>,
    components: crate::outcome::FrameComponents,
}

fn writes(effects: &[Effect]) -> Vec<Write> {
    effects
        .iter()
        .filter_map(|effect| match *effect {
            Effect::WriteFrame {
                plan_sequence,
                target,
                from,
                animate,
                refresh_hz,
                components,
                ..
            } => Some(Write {
                plan: plan_sequence,
                target,
                from,
                animate,
                refresh_hz,
                components,
            }),
            _ => None,
        })
        .collect()
}

fn reconfigure(f: &mut Fixture, revision: u64, outer: u32) -> Vec<Effect> {
    f.apply(Input::Config {
        revision,
        config: config(true, outer),
    })
}

fn window(f: &Fixture) -> WindowInstance { f.windows[0] }

/// Animations on, the startup plan written directly (a revealed workspace never
/// animates) and settled at [`T0`]. Later layout changes animate.
fn settled() -> Fixture {
    let mut f = fixture(true);
    let w = window(&f);
    let startup = writes(&f.enable()).remove(0);
    assert!(!startup.animate, "the first plan reveals the workspace: direct");
    assert_eq!(startup.target, T0);
    f.ack(w, startup.plan, applied_at(T0));
    f
}

#[test]
fn animation_is_off_by_default_so_a_plan_is_one_direct_write() {
    let mut f = fixture(false);
    let planned = writes(&f.enable());
    assert_eq!(planned.len(), 1);
    assert!(!planned[0].animate, "default off never animates");
    assert_eq!(planned[0].target, T0);
}

#[test]
fn an_enabled_large_move_animates_from_the_actual_frame_at_the_display_rate() {
    let mut f = settled();
    let planned = writes(&reconfigure(&mut f, 2, 40));
    assert_eq!(planned.len(), 1);
    assert!(planned[0].animate);
    assert_eq!(planned[0].from, T0);
    assert_eq!(planned[0].refresh_hz, Some(60.0));
}

#[test]
fn a_just_revealed_workspace_is_written_directly_even_with_animations_on() {
    let mut f = fixture(true);
    let planned = writes(&f.enable());
    assert_eq!(planned.len(), 1);
    assert!(
        !planned[0].animate,
        "startup and workspace switches never animate"
    );
    assert_eq!(planned[0].from, Rect::new(10.0, 10.0, 800.0, 600.0));
}

#[test]
fn a_move_within_the_write_tolerance_plans_nothing() {
    let mut f = fixture(true);
    let w = window(&f);
    f.state.windows.get_mut(&w).expect("window").observed.frame =
        Some(Rect::new(2.0, 27.0, 1918.0, 1053.0));
    assert!(
        writes(&f.enable()).is_empty(),
        "nothing to animate and nothing to write"
    );
}

#[test]
fn a_b_a_rewrites_the_window_back_and_an_old_ack_never_settles_the_successor() {
    let mut f = settled();
    let w = window(&f);
    let a = writes(&reconfigure(&mut f, 2, 40)).remove(0);
    assert!(a.animate);

    // B: while A is mid-flight (the window sits at an intermediate position).
    let moving = Rect::new(300.0, 100.0, 1200.0, 800.0);
    f.observe(w, moving);
    let b_effects = reconfigure(&mut f, 3, 0);
    let b = writes(&b_effects).remove(0);
    assert!(b.plan > a.plan);
    assert!(
        b_effects
            .iter()
            .any(|e| matches!(e, Effect::CancelFrame { window } if *window == w)),
        "the older queued plan becomes inert"
    );

    // A again: the observed frame happens to equal the destination of the first
    // plan, yet the window is in motion toward B, so it is written again.
    f.observe(w, a.target);
    let a2 = writes(&reconfigure(&mut f, 4, 40)).remove(0);
    assert!(a2.plan > b.plan, "a third distinct plan sequence");
    assert_eq!(a2.target, a.target);
    assert!(
        !a2.components.is_empty(),
        "an animated window in motion is never assumed arrived"
    );

    // Late acknowledgements of the first two plans.
    let revision = f.revision();
    let stale_before = f.state.diagnostics.stale_frame_acks;
    for plan in [a.plan, b.plan] {
        let stamp = f.stamp();
        f.apply(Input::Completion {
            stamp,
            result: CompletionResult::Frame {
                window: w,
                plan_sequence: plan,
                revision,
                outcome: applied_at(a.target),
            },
        });
    }
    assert_eq!(f.state.diagnostics.stale_frame_acks, stale_before + 2);
    let attempt = f.state.frames.records[&w].attempt.as_ref().expect("attempt");
    assert_eq!(attempt.plan_sequence, a2.plan);
    assert!(
        attempt.in_flight(),
        "the successor is still waiting for its own ack"
    );
}

#[test]
fn an_application_in_instant_fallback_gets_one_direct_write_per_plan_a_b_a() {
    let mut f = settled();
    let w = window(&f);
    // An outer gap change resizes the window: its kind is `Resize`.
    f.state.set_animation_fallback([(w.application(), MotionKind::Resize)].into());
    let a = writes(&reconfigure(&mut f, 2, 40));
    assert_eq!(a.len(), 1, "one write per plan");
    assert!(!a[0].animate, "direct, not animated");
    assert_eq!(a[0].from, T0);
    f.ack(w, a[0].plan, applied_at(a[0].target));
    let b = writes(&reconfigure(&mut f, 3, 0));
    assert_eq!(b.len(), 1);
    assert!(!b[0].animate);
    f.ack(w, b[0].plan, applied_at(b[0].target));
    let a2 = writes(&reconfigure(&mut f, 4, 40));
    assert_eq!(a2.len(), 1);
    assert!(!a2[0].animate, "A, B, A stays direct");
    assert_eq!(a2[0].target, a[0].target);
}

#[test]
fn the_fallback_of_another_application_changes_nothing_and_a_relaunch_animates_again() {
    let mut f = settled();
    let w = window(&f);
    // Another instance (a relaunch of the same application has a new id).
    f.state.set_animation_fallback(
        [(stache_macos::ApplicationId::for_testing(900), MotionKind::Resize)].into(),
    );
    let planned = writes(&reconfigure(&mut f, 2, 40));
    assert!(planned[0].animate, "unaffected");
    f.ack(w, planned[0].plan, applied_at(planned[0].target));
    // The set is emptied (the application terminated): animated again.
    f.state.set_animation_fallback(std::collections::BTreeSet::new());
    let again = writes(&reconfigure(&mut f, 3, 0));
    assert!(again[0].animate);
}

#[test]
fn a_fallback_for_one_kind_leaves_the_other_kind_animating() {
    let mut f = settled();
    let w = window(&f);
    // In fallback for moves only: a gap change resizes, so it still animates.
    f.state.set_animation_fallback([(w.application(), MotionKind::Move)].into());
    let resize = writes(&reconfigure(&mut f, 2, 40)).remove(0);
    assert!(resize.components.size, "the gap change resizes");
    assert!(resize.animate, "a resize animates while only moves fall back");
    f.ack(w, resize.plan, applied_at(resize.target));
    // In fallback for resizes: the same kind of change is direct.
    f.state.set_animation_fallback([(w.application(), MotionKind::Resize)].into());
    let back = writes(&reconfigure(&mut f, 3, 0)).remove(0);
    assert!(back.components.size);
    assert!(!back.animate, "a resize in a resize fallback is direct");
}

#[test]
fn own_settling_echoes_are_ignored_a_bounded_number_of_times() {
    let mut f = settled();
    let w = window(&f);
    let planned = writes(&reconfigure(&mut f, 2, 40)).remove(0);
    assert!(planned.animate);
    f.ack(w, planned.plan, applied_at(planned.target));
    assert_eq!(f.state.frames.records[&w].settle_echoes, MAX_SETTLE_ECHOES);
    let moving = Rect::new(900.0, 400.0, 1000.0, 600.0);
    for echo in 1..=MAX_SETTLE_ECHOES {
        let effects = f.observe(w, moving);
        assert!(writes(&effects).is_empty(), "no write fights our own echo");
        assert_eq!(
            f.state.windows[&w].observed.frame,
            Some(planned.target),
            "the confirmed frame is kept"
        );
        assert_eq!(f.state.diagnostics.animation_echoes_ignored, u64::from(echo));
    }
    // The budget is finite: a real change after it is seen like any other.
    f.observe(w, moving);
    assert_eq!(f.state.windows[&w].observed.frame, Some(moving));
    assert_eq!(
        f.state.diagnostics.animation_echoes_ignored,
        u64::from(MAX_SETTLE_ECHOES)
    );
}

#[test]
fn a_direct_write_has_no_echo_suppression() {
    let mut f = fixture(false);
    let w = window(&f);
    let planned = writes(&f.enable()).remove(0);
    f.ack(w, planned.plan, applied_at(T0));
    assert_eq!(f.state.frames.records[&w].settle_echoes, 0);
    f.observe(w, Rect::new(900.0, 400.0, 1000.0, 600.0));
    assert_eq!(f.state.diagnostics.animation_echoes_ignored, 0);
}

fn down() -> Input {
    Input::Drag(DragSignal::Down {
        button: 0,
        at: DragPoint { x: 100.0, y: 100.0 },
        epoch: 7,
        time: Duration::from_millis(10),
    })
}

#[test]
fn a_drag_gesture_cancels_a_running_animation_and_blocks_new_ones() {
    let mut f = settled();
    let w = window(&f);
    let planned = writes(&reconfigure(&mut f, 2, 40)).remove(0);
    assert!(planned.animate);
    let effects = f.apply(down());
    assert!(f.state.drag.suppresses(), "a gesture is in progress");
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CancelFrame { window } if *window == w)),
        "the running animation is cancelled at the gesture start"
    );
    // While the gesture lasts, a replan writes nothing, animated or not.
    let replanned = reconfigure(&mut f, 3, 80);
    assert!(writes(&replanned).is_empty(), "no animation during a gesture");
}
