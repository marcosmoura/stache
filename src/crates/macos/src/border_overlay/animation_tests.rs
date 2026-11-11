//! Part 03 tests: motion installation, removal, phase, cross-fades and idleness.
//! Everything runs on the fake host; no layer, animation or display link exists.

use std::time::Duration;

use super::testing::FakeEvent;
use super::tests::{BLUE, RED, Rig, appearance, h, rect};
use super::*;

const GREEN: BorderColor = BorderColor { r: 0.0, g: 1.0, b: 0.0, a: 1.0 };
const WHITE: BorderColor = BorderColor { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };

fn gradient(from: BorderColor, to: BorderColor, angle: f64) -> BorderPaint {
    BorderPaint::Gradient { from, to, angle }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "call sites pass the optional motion field"
)]
fn spin(turn_ms: u64, repeat: MotionRepeat) -> Option<BorderMotion> {
    Some(BorderMotion::Rotate {
        turn: Duration::from_millis(turn_ms),
        easing: OverlayEasing::Linear,
        repeat,
    })
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "call sites pass the optional motion field"
)]
fn swap(leg_ms: u64, easing: OverlayEasing) -> Option<BorderMotion> {
    Some(BorderMotion::PingPong {
        leg: Duration::from_millis(leg_ms),
        easing,
    })
}

fn look(paint: BorderPaint, motion: Option<BorderMotion>) -> BorderAppearance {
    BorderAppearance {
        paint,
        motion,
        ..appearance(4.0)
    }
}

const FRAME: OverlayRect = OverlayRect {
    x: 100.0,
    y: 100.0,
    width: 300.0,
    height: 200.0,
};

fn show_look(look: BorderAppearance, change: AppearanceChange) -> OverlayKind {
    OverlayKind::Show {
        target_cg_id: 10,
        frame: FRAME,
        appearance: look,
        change,
    }
}

fn put(rig: &Rig, handle: u64, kind: OverlayKind) {
    let _ = rig.control.apply(OverlayBatch::new().with(h(handle), kind));
}

fn put_look(rig: &Rig, look: BorderAppearance) {
    put(rig, 1, show_look(look, AppearanceChange::Reload));
}

fn change_to(rig: &Rig, look: BorderAppearance) {
    put(rig, 1, show_look(look, AppearanceChange::StateChange));
}

fn fade(ms: u64) -> Transition {
    Transition {
        duration: Duration::from_millis(ms),
        easing: OverlayEasing::EaseOut,
    }
}

fn keys(rig: &Rig, panel: u64) -> Vec<&'static str> {
    rig.backend.animations(panel).into_iter().map(|(_, a)| a.key).collect()
}

// ------------------------------------------------------------------ easing

#[test]
fn every_easing_has_control_points_and_a_bezier_that_tracks_its_curve() {
    let all = [
        OverlayEasing::Linear,
        OverlayEasing::EaseIn,
        OverlayEasing::EaseOut,
        OverlayEasing::EaseInOut,
        OverlayEasing::EaseOutExpo,
    ];
    assert_eq!(OverlayEasing::Linear.control_points(), [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(OverlayEasing::EaseOutExpo.control_points(), [
        0.16, 1.0, 0.3, 1.0
    ]);
    for easing in all {
        let [x1, _, x2, _] = easing.control_points();
        assert!(
            (0.0..=1.0).contains(&x1) && (0.0..=1.0).contains(&x2),
            "valid x range"
        );
        assert!(easing.progress(0.0).abs() < 1e-12);
        assert!((easing.progress(1.0) - 1.0).abs() < 1e-12);
        let mut last = 0.0;
        for step in 1..=100 {
            let t = f64::from(step) / 100.0;
            let p = easing.progress(t);
            assert!(p >= last - 1e-9, "{easing:?} is monotone");
            last = p;
            let back = easing.time_for_progress(p);
            assert!((back - t).abs() < 1e-6, "{easing:?} inverts at {t}");
        }
    }
    assert!((OverlayEasing::Linear.progress(0.3) - 0.3).abs() < 1e-9);
    assert!(OverlayEasing::EaseOut.progress(0.5) > 0.5);
    assert!(OverlayEasing::EaseIn.progress(0.5) < 0.5);
}

#[test]
fn durations_clamp_to_sixteen_milliseconds_and_zero_transition_is_instant() {
    assert_eq!(clamp_motion(Duration::from_millis(3)), MIN_MOTION);
    assert_eq!(
        clamp_motion(Duration::from_millis(500)),
        Duration::from_millis(500)
    );
    let fast = spin(1, MotionRepeat::Loop).expect("motion");
    assert_eq!(fast.period(), Duration::from_millis(16));
    assert_eq!(
        swap(0, OverlayEasing::Linear).expect("motion").period(),
        MIN_MOTION
    );
    assert_eq!(fade(0).effective(), None);
    assert_eq!(fade(5).effective(), Some(MIN_MOTION));
    assert_eq!(fade(200).effective(), Some(Duration::from_millis(200)));
}

// ------------------------------------------------------------------ TB-03-02

#[test]
fn a_loop_installs_one_forever_keyframe_track_in_one_degree_steps() {
    let rig = Rig::inline();
    put_look(
        &rig,
        look(gradient(RED, BLUE, 30.0), spin(4000, MotionRepeat::Loop)),
    );
    let all = rig.backend.animations(1);
    assert_eq!(all.len(), 1, "one render-server animation per border");
    let (layer, animation) = &all[0];
    assert_eq!(*layer, AnimLayer::Gradient);
    assert_eq!(animation.key, KEY_ROTATE);
    assert_eq!(animation.property, AnimProperty::Direction);
    assert_eq!(animation.repeat, AnimRepeat::Forever);
    assert!((animation.duration - 4.0).abs() < 1e-12);
    assert!(animation.time_offset.abs() < f64::EPSILON, "phase 0");
    assert_eq!(animation.timing, OverlayEasing::Linear.control_points());
    assert_eq!(animation.keyframe_count(), Some(361));
    let AnimKind::Keyframes { values, key_times } = &animation.kind else {
        panic!("keyframes")
    };
    let tree = rig.backend.layer_tree(1).expect("tree");
    let size = (tree.ring_frame.width, tree.ring_frame.height);
    let FillSpec::Gradient { start, end, .. } = tree.fill else {
        panic!("gradient")
    };
    // Starts at the configured direction and ends exactly there after a full turn.
    assert_eq!(values[0], AnimValue::Direction(start, end));
    assert_eq!(values[360], AnimValue::Direction(start, end));
    assert!((key_times[0]).abs() < f64::EPSILON && (key_times[360] - 1.0).abs() < f64::EPSILON);
    // Every keyframe is exactly gradient_points(a + k) (counterclockwise).
    for (k, value) in values.iter().enumerate() {
        let (s, e) =
            gradient_points(size, 30.0 + f64::from(u16::try_from(k).expect("361 keyframes")));
        assert_eq!(*value, AnimValue::Direction(s, e));
    }
    assert!(key_times.windows(2).all(|pair| pair[1] >= pair[0]));
}

#[test]
fn a_turn_easing_is_baked_into_key_times_so_steps_stay_within_a_degree() {
    for easing in [
        OverlayEasing::EaseIn,
        OverlayEasing::EaseOut,
        OverlayEasing::EaseInOut,
        OverlayEasing::EaseOutExpo,
    ] {
        let animation = rotation_animation(
            (300.0, 200.0),
            10.0,
            BorderMotion::Rotate {
                turn: Duration::from_secs(2),
                easing,
                repeat: MotionRepeat::Once,
            },
            Duration::ZERO,
        )
        .expect("track");
        let AnimKind::Keyframes { key_times, values } = &animation.kind else {
            panic!("keyframes")
        };
        assert_eq!(values.len(), 361);
        for (k, time) in key_times.iter().enumerate() {
            let want = easing.progress(*time) * 360.0;
            assert!(
                (want - f64::from(u16::try_from(k).expect("361 keyframes"))).abs() < 1e-3,
                "{easing:?} keyframe {k} sits at angle {want}"
            );
        }
        assert_eq!(animation.timing, OverlayEasing::Linear.control_points());
    }
}

#[test]
fn a_once_rotation_plays_one_turn_and_a_short_turn_is_clamped() {
    let rig = Rig::inline();
    put_look(&rig, look(gradient(RED, BLUE, 0.0), spin(2, MotionRepeat::Once)));
    let animation = rig.backend.animation(1, KEY_ROTATE).expect("installed");
    assert_eq!(animation.repeat, AnimRepeat::Once);
    assert!((animation.duration - 0.016).abs() < 1e-12, "clamped to 16 ms");
}

#[test]
fn ping_pong_installs_a_color_swap_track_with_the_leg_easing() {
    let rig = Rig::inline();
    put_look(
        &rig,
        look(gradient(RED, BLUE, 45.0), swap(500, OverlayEasing::EaseInOut)),
    );
    let animation = rig.backend.animation(1, KEY_PING_PONG).expect("installed");
    assert_eq!(animation.property, AnimProperty::Colors);
    assert_eq!(animation.repeat, AnimRepeat::Forever);
    assert!((animation.duration - 1.0).abs() < 1e-12, "out and back");
    assert_eq!(animation.timing, OverlayEasing::EaseInOut.control_points());
    let AnimKind::Keyframes { values, key_times } = animation.kind else {
        panic!("keyframes")
    };
    assert_eq!(values, vec![
        AnimValue::Colors(RED, BLUE),
        AnimValue::Colors(BLUE, RED),
        AnimValue::Colors(RED, BLUE),
    ]);
    assert_eq!(key_times, vec![0.0, 0.5, 1.0]);
}

#[test]
fn motion_on_solid_or_glow_paint_installs_nothing() {
    let rig = Rig::inline();
    put_look(
        &rig,
        look(BorderPaint::Solid(RED), spin(1000, MotionRepeat::Loop)),
    );
    put_look(
        &rig,
        look(BorderPaint::Glow(RED), swap(500, OverlayEasing::Linear)),
    );
    assert!(rig.backend.animations(1).is_empty());
    assert_eq!(rig.control.diagnostics().animation, None, "not a failure");
}

#[test]
fn only_the_borders_given_a_motion_animate_and_each_keeps_its_own() {
    let rig = Rig::inline();
    // Handle 1 is "focused" (ping-pong), 2 and 3 carry rotations, 4 carries none.
    put(
        &rig,
        1,
        show_look(
            look(gradient(RED, BLUE, 0.0), swap(300, OverlayEasing::Linear)),
            AppearanceChange::Reload,
        ),
    );
    put(
        &rig,
        2,
        show_look(
            look(gradient(RED, BLUE, 0.0), spin(2000, MotionRepeat::Loop)),
            AppearanceChange::Reload,
        ),
    );
    put(
        &rig,
        3,
        show_look(
            look(gradient(GREEN, BLUE, 90.0), spin(3000, MotionRepeat::Once)),
            AppearanceChange::Reload,
        ),
    );
    put(
        &rig,
        4,
        show_look(look(gradient(RED, BLUE, 0.0), None), AppearanceChange::Reload),
    );
    assert_eq!(keys(&rig, 1), vec![KEY_PING_PONG]);
    assert_eq!(keys(&rig, 2), vec![KEY_ROTATE]);
    assert_eq!(keys(&rig, 3), vec![KEY_ROTATE]);
    assert!(keys(&rig, 4).is_empty());
    // Focus loss: the border is re-shown without the ping-pong (state change).
    change_to(&rig, look(gradient(RED, BLUE, 0.0), None));
    assert!(keys(&rig, 1).is_empty(), "removed from the old border");
    assert_eq!(rig.backend.removals(KEY_PING_PONG), 1);
    assert_eq!(keys(&rig, 2), vec![KEY_ROTATE], "others untouched");
}

#[test]
fn hide_release_width_zero_and_hide_all_remove_the_animation() {
    let rig = Rig::inline();
    let spinning = look(gradient(RED, BLUE, 0.0), spin(2000, MotionRepeat::Loop));
    for handle in 1..=4_u64 {
        put(&rig, handle, show_look(spinning, AppearanceChange::Reload));
    }
    assert_eq!(rig.backend.forever_animations(), 4);
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Hide));
    assert!(keys(&rig, 1).is_empty(), "hide");
    let _ = rig.control.apply(OverlayBatch::new().with(h(2), OverlayKind::Release));
    assert!(keys(&rig, 2).is_empty(), "release");
    put(
        &rig,
        3,
        show_look(
            BorderAppearance { width: 0.0, ..spinning },
            AppearanceChange::Reload,
        ),
    );
    assert!(keys(&rig, 3).is_empty(), "width 0 draws nothing");
    let _ = rig.control.apply(OverlayBatch::hide_all(HideReason::SpaceChange));
    assert!(keys(&rig, 4).is_empty(), "hide all");
    assert_eq!(rig.backend.forever_animations(), 0);
    assert_eq!(rig.backend.removals(KEY_ROTATE), 4);
}

#[test]
fn a_hidden_border_restarts_from_phase_zero_when_shown_again() {
    let rig = Rig::inline();
    let spinning = look(gradient(RED, BLUE, 0.0), spin(4000, MotionRepeat::Loop));
    rig.backend.set_media_time(100.0);
    put_look(&rig, spinning);
    rig.backend.advance_media_time(1.5);
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Hide));
    rig.backend.advance_media_time(1.0);
    put_look(&rig, spinning);
    let animation = rig.backend.animation(1, KEY_ROTATE).expect("installed again");
    assert!(animation.time_offset.abs() < f64::EPSILON, "phase 0, not 2.5 s");
}

#[test]
fn a_state_change_restarts_a_once_rotation_but_a_reload_or_recolor_does_not() {
    let rig = Rig::inline();
    rig.backend.set_media_time(10.0);
    put_look(
        &rig,
        look(gradient(RED, BLUE, 0.0), spin(4000, MotionRepeat::Once)),
    );
    let added = rig.backend.animations_added();
    rig.backend.advance_media_time(1.0);
    // Reload and recolor with the same motion and size: untouched.
    put_look(
        &rig,
        look(gradient(RED, BLUE, 0.0), spin(4000, MotionRepeat::Once)),
    );
    put_look(
        &rig,
        look(gradient(GREEN, WHITE, 0.0), spin(4000, MotionRepeat::Once)),
    );
    assert_eq!(rig.backend.animations_added(), added, "no restart");
    assert_eq!(rig.backend.removals(KEY_ROTATE), 0);
    // A Move at the same size is not a restart either.
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(0.0, 0.0, 300.0, 200.0),
    }));
    assert_eq!(rig.backend.animations_added(), added);
    // The window's state changed (focus): one more turn from phase 0.
    change_to(
        &rig,
        look(gradient(RED, BLUE, 0.0), spin(4000, MotionRepeat::Once)),
    );
    assert_eq!(rig.backend.animations_added(), added + 1);
    assert_eq!(rig.backend.removals(KEY_ROTATE), 1);
    assert!(
        rig.backend.animation(1, KEY_ROTATE).expect("restarted").time_offset.abs() < f64::EPSILON
    );
    // A different motion is a new motion even for a reload.
    put_look(
        &rig,
        look(gradient(RED, BLUE, 0.0), spin(1000, MotionRepeat::Loop)),
    );
    assert_eq!(rig.backend.animations_added(), added + 2);
}

#[test]
fn a_resize_rebuilds_the_track_for_the_new_size_and_keeps_the_phase() {
    let rig = Rig::inline();
    rig.backend.set_media_time(50.0);
    put_look(
        &rig,
        look(gradient(RED, BLUE, 45.0), spin(4000, MotionRepeat::Loop)),
    );
    let before = rig.backend.animation(1, KEY_ROTATE).expect("track");
    rig.backend.advance_media_time(1.0);
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(100.0, 100.0, 600.0, 100.0),
    }));
    let after = rig.backend.animation(1, KEY_ROTATE).expect("rebuilt");
    assert_ne!(before.kind, after.kind, "built for the new size");
    assert!(
        (after.time_offset - 1.0).abs() < 1e-9,
        "phase kept: {}",
        after.time_offset
    );
    assert_eq!(rig.backend.removals(KEY_ROTATE), 1);
    // The phase keeps counting from the original start, wrapping per turn.
    rig.backend.advance_media_time(4.5);
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(100.0, 100.0, 650.0, 100.0),
    }));
    let wrapped = rig.backend.animation(1, KEY_ROTATE).expect("rebuilt again");
    assert!(
        (wrapped.time_offset - 1.5).abs() < 1e-9,
        "5.5 s into a 4 s loop: {}",
        wrapped.time_offset
    );
    // Same size again: nothing to rebuild.
    let added = rig.backend.animations_added();
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(10.0, 10.0, 650.0, 100.0),
    }));
    assert_eq!(rig.backend.animations_added(), added);
}

#[test]
fn a_finished_once_rotation_is_not_rebuilt_by_a_resize() {
    let rig = Rig::inline();
    rig.backend.set_media_time(0.0);
    put_look(
        &rig,
        look(gradient(RED, BLUE, 0.0), spin(1000, MotionRepeat::Once)),
    );
    rig.backend.advance_media_time(0.4);
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(0.0, 0.0, 500.0, 500.0),
    }));
    let mid = rig.backend.animation(1, KEY_ROTATE).expect("mid turn");
    assert!((mid.time_offset - 0.4).abs() < 1e-9);
    rig.backend.advance_media_time(2.0);
    let added = rig.backend.animations_added();
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(0.0, 0.0, 700.0, 500.0),
    }));
    assert_eq!(
        rig.backend.animations_added(),
        added,
        "the turn is over; the model shows a"
    );
    assert!(rig.backend.animation(1, KEY_ROTATE).is_none());
}

#[test]
fn an_undescribable_motion_draws_statically_with_one_diagnostic() {
    assert_eq!(
        rotation_animation(
            (0.0, 40.0),
            0.0,
            spin(1000, MotionRepeat::Loop).expect("motion"),
            Duration::ZERO
        ),
        Err(DescribeError::EmptySize)
    );
    let rig = Rig::inline();
    assert_eq!(rig.control.diagnostics().animation, None);
    put(&rig, 1, OverlayKind::Show {
        target_cg_id: 10,
        frame: rect(0.0, 0.0, 0.0, 0.0),
        appearance: look(gradient(RED, BLUE, 0.0), spin(1000, MotionRepeat::Loop)),
        change: AppearanceChange::Reload,
    });
    assert!(keys(&rig, 1).is_empty(), "static");
    assert_eq!(rig.backend.ordered_in(), 1, "the border still draws");
    assert_eq!(rig.control.diagnostics().animation, Some(ANIMATION_DIAGNOSTIC));
    assert!(rig.control.diagnostics().messages().contains(&ANIMATION_DIAGNOSTIC));
    // A later describable motion clears it.
    put_look(
        &rig,
        look(gradient(RED, BLUE, 0.0), spin(1000, MotionRepeat::Loop)),
    );
    assert_eq!(rig.control.diagnostics().animation, None);
}

// ------------------------------------------------------------------ TB-03-03

fn fading_rig() -> Rig {
    let rig = Rig::inline();
    rig.control.set_transition(Some(fade(200)));
    rig
}

#[test]
fn a_solid_to_solid_state_change_fades_from_the_presented_color() {
    let rig = fading_rig();
    put_look(&rig, look(BorderPaint::Solid(RED), None));
    rig.backend.script_presentation(1, Presented::Solid(WHITE));
    change_to(&rig, look(BorderPaint::Solid(BLUE), None));
    let all = rig.backend.animations(1);
    assert_eq!(all.len(), 1);
    let animation = &all[0].1;
    assert_eq!(animation.property, AnimProperty::FillColor);
    assert_eq!(animation.kind, AnimKind::Basic {
        from: AnimValue::Color(WHITE),
        to: AnimValue::Color(BLUE)
    });
    assert!((animation.duration - 0.2).abs() < 1e-12);
    assert_eq!(animation.timing, OverlayEasing::EaseOut.control_points());
    assert_eq!(
        rig.backend.layer_tree(1).expect("tree").fill,
        FillSpec::Solid(BLUE),
        "model is final"
    );
    assert_eq!(rig.backend.implicit_animations(), 0);
    assert_eq!(
        rig.backend.untransacted_animation_calls(),
        0,
        "added inside the transaction"
    );
}

#[test]
fn solid_gradient_and_glow_fade_color_by_color_or_by_opacity() {
    let rig = fading_rig();
    // solid -> gradient: the gradient layer fades from [c, c].
    put_look(&rig, look(BorderPaint::Solid(RED), None));
    change_to(&rig, look(gradient(GREEN, BLUE, 0.0), None));
    let colors = rig.backend.animation(1, "stache.fade.gradient.colors").expect("colors");
    assert_eq!(colors.kind, AnimKind::Basic {
        from: AnimValue::Colors(RED, RED),
        to: AnimValue::Colors(GREEN, BLUE),
    });
    // gradient -> gradient: from the presented pair (scripted mid-fade).
    rig.backend.script_presentation(1, Presented::Gradient(WHITE, RED));
    change_to(&rig, look(gradient(BLUE, GREEN, 0.0), None));
    let colors = rig.backend.animation(1, "stache.fade.gradient.colors").expect("colors");
    assert_eq!(colors.kind, AnimKind::Basic {
        from: AnimValue::Colors(WHITE, RED),
        to: AnimValue::Colors(BLUE, GREEN),
    });
    // gradient -> solid: gradient fades to [c, c] while the ring waits, then takes over.
    change_to(&rig, look(BorderPaint::Solid(RED), None));
    let colors = rig.backend.animation(1, "stache.fade.gradient.colors").expect("colors");
    assert_eq!(colors.kind, AnimKind::Basic {
        from: AnimValue::Colors(BLUE, GREEN),
        to: AnimValue::Colors(RED, RED),
    });
    assert!(rig.backend.animation(1, "stache.fade.gradient.opacity").is_some());
    assert!(rig.backend.animation(1, "stache.fade.ring.opacity").is_some());
    assert_eq!(
        rig.backend.layer_override(1, AnimLayer::Gradient),
        Some(LayerOverride {
            layer: AnimLayer::Gradient,
            hidden: false,
            opacity: 0.0
        }),
        "the leaving gradient stays drawable until the fade ends"
    );
}

#[test]
fn glow_fades_by_the_opacity_of_its_band_and_never_a_ring() {
    let rig = fading_rig();
    put_look(&rig, look(BorderPaint::Solid(RED), None));
    // solid -> glow: the band fades in, the hidden ring fades out (no fill fade).
    change_to(&rig, look(BorderPaint::Glow(BLUE), None));
    assert_eq!(rig.backend.shown_layers(1), vec![AnimLayer::Glow]);
    let halo = rig.backend.animation(1, "stache.fade.glow.opacity").expect("halo");
    assert_eq!(halo.layer, AnimLayer::Glow);
    assert_eq!(halo.kind, AnimKind::Basic {
        from: AnimValue::Number(0.0),
        to: AnimValue::Number(1.0)
    });
    let ring_out = rig.backend.animation(1, "stache.fade.ring.opacity").expect("ring");
    assert_eq!(ring_out.kind, AnimKind::Basic {
        from: AnimValue::Number(1.0),
        to: AnimValue::Number(0.0)
    });
    assert!(rig.backend.animation(1, "stache.fade.ring.fill_color").is_none());
    assert_eq!(
        rig.backend.layer_override(1, AnimLayer::Ring).map(|o| o.opacity),
        Some(0.0),
        "the leaving ring stays drawable until the fade ends"
    );
    // glow -> solid: the band fades out and stays drawable; the ring fades in.
    change_to(&rig, look(BorderPaint::Solid(RED), None));
    let halo = rig.backend.animation(1, "stache.fade.glow.opacity").expect("halo");
    assert_eq!(halo.kind, AnimKind::Basic {
        from: AnimValue::Number(1.0),
        to: AnimValue::Number(0.0)
    });
    let ring_in = rig.backend.animation(1, "stache.fade.ring.opacity").expect("ring");
    assert_eq!(ring_in.kind, AnimKind::Basic {
        from: AnimValue::Number(0.0),
        to: AnimValue::Number(1.0)
    });
    assert_eq!(
        rig.backend.layer_override(1, AnimLayer::Glow),
        Some(LayerOverride {
            layer: AnimLayer::Glow,
            hidden: false,
            opacity: 0.0
        })
    );
    // glow -> gradient: opacity cross-fade of the glow layer and the gradient.
    change_to(&rig, look(BorderPaint::Glow(RED), None));
    change_to(&rig, look(gradient(RED, BLUE, 0.0), None));
    assert!(
        rig.backend.animation(1, "stache.fade.ring.opacity").is_none(),
        "a glow has no ring to fade"
    );
    let gradient_in = rig.backend.animation(1, "stache.fade.gradient.opacity").expect("gradient");
    assert_eq!(gradient_in.kind, AnimKind::Basic {
        from: AnimValue::Number(0.0),
        to: AnimValue::Number(1.0)
    });
    let halo_out = rig.backend.animation(1, "stache.fade.glow.opacity").expect("halo");
    assert_eq!(halo_out.kind, AnimKind::Basic {
        from: AnimValue::Number(1.0),
        to: AnimValue::Number(0.0)
    });
    assert!(rig.backend.layer_override(1, AnimLayer::Glow).is_some());
    // gradient -> glow: the reverse cross-fade.
    change_to(&rig, look(BorderPaint::Glow(BLUE), None));
    assert!(rig.backend.animation(1, "stache.fade.ring.opacity").is_none());
    let gradient_out = rig.backend.animation(1, "stache.fade.gradient.opacity").expect("gradient");
    assert_eq!(gradient_out.kind, AnimKind::Basic {
        from: AnimValue::Number(1.0),
        to: AnimValue::Number(0.0)
    });
    assert!(rig.backend.layer_override(1, AnimLayer::Gradient).is_some());
    let halo_in = rig.backend.animation(1, "stache.fade.glow.opacity").expect("halo");
    assert_eq!(halo_in.kind, AnimKind::Basic {
        from: AnimValue::Number(0.0),
        to: AnimValue::Number(1.0)
    });
    // glow -> glow: the band bitmap is re-rendered and the new band fades in.
    let renders = rig.backend.glow_renders(1).len();
    change_to(&rig, look(BorderPaint::Glow(GREEN), None));
    assert!(rig.backend.animation(1, "stache.fade.ring.fill_color").is_none());
    let halo_in = rig.backend.animation(1, "stache.fade.glow.opacity").expect("halo");
    assert_eq!(halo_in.kind, AnimKind::Basic {
        from: AnimValue::Number(0.0),
        to: AnimValue::Number(1.0)
    });
    assert_eq!(rig.backend.glow_renders(1).len(), renders + 1);
    assert_eq!(rig.backend.glow_renders(1).last().expect("render").color, GREEN);
}

#[test]
fn an_interrupted_fade_restarts_from_the_current_presentation() {
    let rig = fading_rig();
    put_look(&rig, look(BorderPaint::Solid(RED), None));
    change_to(&rig, look(BorderPaint::Solid(BLUE), None));
    let first = rig.backend.animation(1, "stache.fade.ring.fill_color").expect("first");
    // Midway the user sees purple; focus changes again.
    let purple = BorderColor { r: 0.5, g: 0.0, b: 0.5, a: 1.0 };
    rig.backend.script_presentation(1, Presented::Solid(purple));
    change_to(&rig, look(BorderPaint::Solid(GREEN), None));
    let second = rig.backend.animation(1, "stache.fade.ring.fill_color").expect("second");
    assert_ne!(first, second);
    assert_eq!(second.kind, AnimKind::Basic {
        from: AnimValue::Color(purple),
        to: AnimValue::Color(GREEN)
    });
    assert_eq!(
        keys(&rig, 1).len(),
        1,
        "no leftover animation from the first fade"
    );
    assert_eq!(rig.backend.removals("stache.fade.ring.fill_color"), 1);
}

#[test]
fn no_fade_without_a_transition_with_zero_ms_on_reload_or_on_first_show() {
    let scripted = Presented::Solid(WHITE);
    // Absent transition.
    let rig = Rig::inline();
    put_look(&rig, look(BorderPaint::Solid(RED), None));
    rig.backend.script_presentation(1, scripted);
    change_to(&rig, look(BorderPaint::Solid(BLUE), None));
    assert!(rig.backend.animations(1).is_empty());
    // 0 ms.
    rig.control.set_transition(Some(fade(0)));
    change_to(&rig, look(BorderPaint::Solid(RED), None));
    assert!(rig.backend.animations(1).is_empty());
    // A reload is instant even with a transition, and ends a fade in flight.
    rig.control.set_transition(Some(fade(200)));
    change_to(&rig, look(BorderPaint::Solid(GREEN), None));
    assert_eq!(rig.backend.animations(1).len(), 1);
    put_look(&rig, look(BorderPaint::Solid(BLUE), None));
    assert!(
        rig.backend.animations(1).is_empty(),
        "reload is instant and cancels the fade"
    );
    // First show and show after hide are instant.
    let fresh = fading_rig();
    fresh.backend.script_presentation(1, scripted);
    change_to(&fresh, look(BorderPaint::Solid(BLUE), None));
    assert!(fresh.backend.animations(1).is_empty(), "first show");
    let _ = fresh.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Hide));
    change_to(&fresh, look(BorderPaint::Solid(RED), None));
    assert!(fresh.backend.animations(1).is_empty(), "shown from hidden");
}

#[test]
fn width_changes_are_instant_and_an_equal_color_adds_nothing() {
    let rig = fading_rig();
    put_look(&rig, look(BorderPaint::Solid(RED), None));
    let wide = BorderAppearance {
        width: 9.0,
        ..look(BorderPaint::Solid(RED), None)
    };
    change_to(&rig, wide);
    assert!(rig.backend.animations(1).is_empty());
    assert!((rig.backend.layer_tree(1).expect("tree").inner.x - 9.0).abs() < f64::EPSILON);
}

#[test]
fn a_hide_is_never_delayed_by_a_fade() {
    let rig = fading_rig();
    put_look(&rig, look(BorderPaint::Solid(RED), None));
    change_to(&rig, look(BorderPaint::Solid(BLUE), None));
    assert_eq!(rig.backend.animations(1).len(), 1);
    let before = rig.backend.event_count();
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Hide));
    assert_eq!(rig.backend.ordered_in(), 0, "ordered out in the same batch");
    assert!(rig.backend.animations(1).is_empty());
    let events = rig.backend.events();
    assert!(events[before..].iter().any(|event| matches!(event, FakeEvent::OrderOut(1))));
    // Fade and hide in one batch: the hide wins, nothing fades.
    let both = Rig::queued();
    both.control.set_transition(Some(fade(200)));
    both.executor.set_on_main(true);
    put_look(&both, look(BorderPaint::Solid(RED), None));
    both.executor.set_on_main(false);
    change_to(&both, look(BorderPaint::Solid(BLUE), None));
    let _ = both.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Hide));
    both.drain();
    assert_eq!(both.backend.ordered_in(), 0);
    assert!(both.backend.animations(1).is_empty());
}

#[test]
fn a_ping_pong_keeps_the_gradient_colors_and_a_move_keeps_the_fade_overrides() {
    let rig = fading_rig();
    put_look(&rig, look(gradient(RED, BLUE, 0.0), None));
    change_to(
        &rig,
        look(gradient(GREEN, WHITE, 0.0), swap(400, OverlayEasing::Linear)),
    );
    assert_eq!(
        keys(&rig, 1),
        vec![KEY_PING_PONG],
        "colors belong to the ping-pong"
    );

    // A Move during a fade re-applies the override the layer write reset.
    let rig = fading_rig();
    rig.backend.set_media_time(0.0);
    put_look(&rig, look(gradient(RED, BLUE, 0.0), None));
    change_to(&rig, look(BorderPaint::Solid(GREEN), None));
    assert!(rig.backend.layer_override(1, AnimLayer::Gradient).is_some());
    rig.backend.advance_media_time(0.1);
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(0.0, 0.0, 400.0, 300.0),
    }));
    assert!(
        rig.backend.layer_override(1, AnimLayer::Gradient).is_some(),
        "still fading"
    );
    rig.backend.advance_media_time(0.2);
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(0.0, 0.0, 410.0, 300.0),
    }));
    assert!(
        rig.backend.layer_override(1, AnimLayer::Gradient).is_none(),
        "the fade is over"
    );
}

#[test]
fn a_rotation_continues_through_a_cross_fade_of_the_new_state() {
    let rig = fading_rig();
    put_look(&rig, look(BorderPaint::Solid(RED), None));
    change_to(
        &rig,
        look(gradient(GREEN, BLUE, 20.0), spin(3000, MotionRepeat::Loop)),
    );
    assert_eq!(keys(&rig, 1), vec!["stache.fade.gradient.colors", KEY_ROTATE]);
}

// ------------------------------------------------------------------ merge (control)

fn pending_rig() -> Rig {
    let rig = Rig::queued();
    rig.executor.set_on_main(true);
    rig.control.set_transition(Some(fade(200)));
    put_look(&rig, look(BorderPaint::Solid(RED), None));
    rig.executor.set_on_main(false);
    rig
}

#[test]
fn the_latest_show_decides_whether_a_pending_change_fades() {
    let rig = pending_rig();
    change_to(&rig, look(BorderPaint::Solid(BLUE), None));
    put_look(&rig, look(BorderPaint::Solid(GREEN), None));
    rig.drain();
    assert!(
        rig.backend.animations(1).is_empty(),
        "latest Reload wins: instant"
    );

    let rig = pending_rig();
    put_look(&rig, look(BorderPaint::Solid(BLUE), None));
    change_to(&rig, look(BorderPaint::Solid(GREEN), None));
    rig.drain();
    assert_eq!(
        rig.backend.animations(1).len(),
        1,
        "latest StateChange wins: fade"
    );

    // Move and Order after the Show are absorbed; the change flag stays.
    let rig = pending_rig();
    change_to(&rig, look(BorderPaint::Solid(BLUE), None));
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: rect(0.0, 0.0, 50.0, 50.0),
    }));
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), OverlayKind::Order { target_cg_id: 77 }));
    rig.drain();
    assert_eq!(rig.backend.animations(1).len(), 1);
    assert_eq!(rig.control.counters().batches, 2);

    // A Hide then a StateChange Show in one batch: the Show replaces the Hide.
    let rig = pending_rig();
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Hide));
    change_to(&rig, look(BorderPaint::Solid(BLUE), None));
    rig.drain();
    assert_eq!(rig.backend.ordered_in(), 1);
    assert_eq!(rig.backend.animations(1).len(), 1);
}

#[test]
fn the_transition_in_force_when_the_job_runs_is_used() {
    let rig = pending_rig();
    rig.control.set_transition(None);
    change_to(&rig, look(BorderPaint::Solid(BLUE), None));
    rig.control.set_transition(Some(fade(80)));
    rig.drain();
    let animation = rig.backend.animation(1, "stache.fade.ring.fill_color").expect("fade");
    assert!((animation.duration - 0.08).abs() < 1e-12);
    assert_eq!(rig.control.transition(), Some(fade(80)));
}

// ------------------------------------------------------------------ TB-03-04

#[test]
fn no_timer_display_link_or_callback_is_scheduled_and_an_idle_engine_does_nothing() {
    let rig = fading_rig();
    rig.backend.set_media_time(0.0);
    put_look(
        &rig,
        look(gradient(RED, BLUE, 0.0), spin(500, MotionRepeat::Loop)),
    );
    put(
        &rig,
        2,
        show_look(
            look(gradient(RED, BLUE, 0.0), spin(500, MotionRepeat::Once)),
            AppearanceChange::Reload,
        ),
    );
    put(
        &rig,
        3,
        show_look(
            look(gradient(RED, BLUE, 0.0), swap(100, OverlayEasing::Linear)),
            AppearanceChange::Reload,
        ),
    );
    change_to(&rig, look(BorderPaint::Solid(BLUE), None));
    // Every animation was added inside a transaction, none by an implicit action.
    assert_eq!(rig.backend.untransacted_animation_calls(), 0);
    assert_eq!(rig.backend.implicit_animations(), 0);
    assert_eq!(rig.backend.scheduled_callbacks(), 0);
    assert_eq!(rig.executor.queued(), 0, "nothing queued after the batch");
    let events = rig.backend.event_count();
    let counters = rig.control.counters();
    // Time passes: the fades and the finite turn end, the loop keeps running.
    for _ in 0..50 {
        rig.backend.advance_media_time(0.1);
    }
    assert_eq!(
        rig.backend.event_count(),
        events,
        "no backend call without a new batch"
    );
    assert_eq!(rig.executor.queued(), 0);
    assert_eq!(rig.control.counters(), counters);
    assert_eq!(rig.backend.panels_created(), 3);
}

#[test]
fn an_engine_with_only_finished_finite_animations_does_no_work() {
    let rig = fading_rig();
    rig.backend.set_media_time(0.0);
    put_look(&rig, look(BorderPaint::Solid(RED), None));
    change_to(&rig, look(BorderPaint::Solid(BLUE), None));
    put(
        &rig,
        2,
        show_look(
            look(gradient(RED, BLUE, 0.0), spin(300, MotionRepeat::Once)),
            AppearanceChange::Reload,
        ),
    );
    let events = rig.backend.event_count();
    rig.backend.advance_media_time(60.0);
    assert_eq!(rig.backend.event_count(), events);
    assert_eq!(rig.executor.queued(), 0);
    assert_eq!(rig.backend.scheduled_callbacks(), 0);
    // Nothing observed the end: the model values were already final.
    assert_eq!(
        rig.backend.layer_tree(1).expect("tree").fill,
        FillSpec::Solid(BLUE)
    );
}
