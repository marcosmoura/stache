//! Part 09 tests: the focus width grow and shrink. Pure ring planning
//! (TB-09-01) and the owner map on the fake host (TB-09-02, TB-09-03). No
//! window, layer, animation, timer or display link exists.

use std::time::Duration;

use super::tests::{BLUE, Rig, appearance, h};
use super::*;

const FRAME: OverlayRect = OverlayRect {
    x: 100.0,
    y: 100.0,
    width: 300.0,
    height: 200.0,
};

const EPS: f64 = 1e-9;

fn screens() -> Vec<OverlayScreen> {
    vec![OverlayScreen {
        frame: OverlayRect::new(0.0, 0.0, 1000.0, 800.0),
        scale: 2.0,
    }]
}

fn look(width: f64, paint: BorderPaint, placement: BorderPlacement) -> BorderAppearance {
    BorderAppearance {
        paint,
        placement,
        ..appearance(width)
    }
}

fn ring(look: &BorderAppearance, fraction: f64) -> (OverlayPlan, RingShape) {
    let full = plan_overlay(FRAME, look, None, 16.0, &screens());
    let shape = ring_at_fraction(&full, FRAME, look, (None, 16.0), &screens(), fraction);
    (full, shape)
}

fn close(a: f64, b: f64) -> bool { (a - b).abs() < EPS }

// ------------------------------------------------------------ TB-09-01 pure

/// TB-09-01: the full fraction is exactly the drawn ring, zero is an empty ring
/// on the target's edge, and the visible fraction and placement apply at every
/// width.
#[test]
fn the_ring_scales_from_the_target_edge_for_both_placements() {
    for placement in [BorderPlacement::Below, BorderPlacement::Above] {
        let look = look(6.0, BorderPaint::Solid(BLUE), placement);
        let (full, one) = ring(&look, 1.0);
        assert_eq!(one, full.layer_spec().ring_shape(), "{placement:?}");

        let (_, zero) = ring(&look, 0.0);
        assert_eq!(zero.outer, zero.inner, "nothing drawn at zero ({placement:?})");
        let visible = 6.0 * VISIBLE_FRACTION;
        // The zero ring is the target frame, in the full ring's coordinates.
        assert!(close(zero.outer.x, visible) && close(zero.outer.width, FRAME.width));
        assert!(close(zero.outer_radius, zero.inner_radius));
        assert!(
            zero.outer_radius > 0.0,
            "corners stay round, the path keeps its shape"
        );

        let (_, half) = ring(&look, 0.5);
        assert!(close(half.outer.x, visible / 2.0), "{placement:?} {half:?}");
        assert!(close(half.outer.width, FRAME.width + visible));
        match placement {
            // Half of the hidden sixth lies under the window.
            BorderPlacement::Below => assert!(close(half.inner.x, visible + 0.5)),
            BorderPlacement::Above => assert!(close(half.inner.x, visible)),
        }
    }
}

/// TB-09-01: a glow's ring geometry scales the same way, but a glow has no
/// crisp ring: only its band (the glow layer) animates, fading with the width.
#[test]
fn a_glow_ring_lines_up_with_its_wider_panel_and_the_halo_fades() {
    let look = look(4.0, BorderPaint::Glow(BLUE), BorderPlacement::Below);
    let (full, one) = ring(&look, 1.0);
    assert_eq!(one, full.layer_spec().ring_shape());
    let (_, zero) = ring(&look, 0.0);
    assert!(close(zero.outer.x, 4.0 * VISIBLE_FRACTION));
    let plan = plan_width(one, zero, Some((1.0, 0.0)), 0.2, OverlayEasing::EaseOut);
    let keys: Vec<_> = plan.animations.iter().map(|a| (a.layer, a.key, a.property)).collect();
    assert_eq!(keys, vec![(
        AnimLayer::Glow,
        KEY_WIDTH_GLOW,
        AnimProperty::Opacity
    )]);
    assert_eq!(plan.overrides, vec![LayerOverride {
        layer: AnimLayer::Glow,
        hidden: false,
        opacity: 0.0,
    }]);
    let band = &plan.animations[0];
    assert_eq!(band.kind, AnimKind::Basic {
        from: AnimValue::Number(1.0),
        to: AnimValue::Number(0.0),
    });
    assert_eq!(band.repeat, AnimRepeat::Once);
    assert_eq!(band.timing, OverlayEasing::EaseOut.control_points());
    assert!(close(band.duration, 0.2));
    // Solid and gradient animate only the two paths.
    let solid = plan_width(one, zero, None, 0.2, OverlayEasing::EaseOut);
    let keys: Vec<_> = solid.animations.iter().map(|a| (a.layer, a.key)).collect();
    assert_eq!(keys, vec![
        (AnimLayer::Ring, KEY_WIDTH_RING),
        (AnimLayer::GradientMask, KEY_WIDTH_MASK),
    ]);
    assert_eq!(solid.animations[0].kind, AnimKind::Basic {
        from: AnimValue::Ring(one),
        to: AnimValue::Ring(zero),
    });
    assert!(solid.overrides.is_empty());
}

// ------------------------------------------------------------ TB-09-02 fake host

fn fade(ms: u64) -> Transition {
    Transition {
        duration: Duration::from_millis(ms),
        easing: OverlayEasing::Linear,
    }
}

fn rig(transition: Option<Transition>) -> Rig {
    let rig = Rig::inline();
    rig.control.set_transition(transition);
    rig.backend.set_media_time(0.0);
    rig
}

fn put(rig: &Rig, look: BorderAppearance, change: AppearanceChange) {
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Show {
        target_cg_id: 10,
        frame: FRAME,
        appearance: look,
        change,
    }));
}

fn solid() -> BorderAppearance { look(6.0, BorderPaint::Solid(BLUE), BorderPlacement::Below) }

fn width_track(rig: &Rig) -> Option<(RingShape, RingShape, f64)> {
    let animation = rig.backend.animation(1, KEY_WIDTH_RING)?;
    let AnimKind::Basic {
        from: AnimValue::Ring(from),
        to: AnimValue::Ring(to),
    } = animation.kind
    else {
        return None;
    };
    Some((from, to, animation.duration))
}

fn shape(fraction: f64) -> RingShape { ring(&solid(), fraction).1 }

/// TB-09-02: a grow from hidden starts at zero width and ends at the full ring
/// (the model); a shrink ends at the zero ring with the panel still ordered in.
#[test]
fn a_grow_starts_at_zero_and_a_shrink_ends_at_zero_with_the_panel_ordered_in() {
    let rig = rig(Some(fade(200)));
    put(&rig, solid(), AppearanceChange::Grow);
    assert!(rig.backend.is_ordered_in(1));
    let (from, to, seconds) = width_track(&rig).expect("grow track");
    assert_eq!((from, to), (shape(0.0), shape(1.0)));
    assert!(close(seconds, 0.2));
    assert!(rig.backend.animation(1, KEY_WIDTH_MASK).is_some());
    assert_eq!(rig.backend.ring_override(1), None, "the model is the full ring");

    rig.backend.set_media_time(1.0);
    put(&rig, solid(), AppearanceChange::Shrink);
    assert!(
        rig.backend.is_ordered_in(1),
        "ordered in until the engine hides it"
    );
    let (from, to, _) = width_track(&rig).expect("shrink track");
    assert_eq!((from, to), (shape(1.0), shape(0.0)));
    assert_eq!(rig.backend.ring_override(1), Some(shape(0.0)), "model at zero");
    assert_eq!(rig.backend.implicit_animations(), 0);
    assert_eq!(rig.backend.untransacted_animation_calls(), 0);
    assert_eq!(rig.backend.scheduled_callbacks(), 0);
    // The shrink's end is observed by nobody: no call, no queued job.
    let events = rig.backend.event_count();
    rig.backend.advance_media_time(10.0);
    assert_eq!(rig.backend.event_count(), events);
    assert_eq!(rig.executor.queued(), 0);
}

/// TB-09-02: an interrupted change starts from the presented width, computed
/// from the running track's timing; its duration scales with the distance.
#[test]
fn an_interrupted_change_starts_from_the_presented_width() {
    let rig = rig(Some(fade(200)));
    put(&rig, solid(), AppearanceChange::Reload);
    put(&rig, solid(), AppearanceChange::Shrink);
    rig.backend.set_media_time(0.05);
    put(&rig, solid(), AppearanceChange::Grow);
    let (from, to, seconds) = width_track(&rig).expect("grow track");
    assert_eq!(from, shape(0.75), "a quarter of the linear shrink had run");
    assert_eq!(to, shape(1.0));
    assert!(close(seconds, 0.05));
    assert_eq!(rig.backend.ring_override(1), None);

    rig.backend.set_media_time(0.075);
    put(&rig, solid(), AppearanceChange::Shrink);
    let (from, to, _) = width_track(&rig).expect("shrink track");
    assert_eq!((from, to), (shape(0.875), shape(0.0)));
}

/// TB-09-02: a hide, `HideAll` or release during a shrink orders out at once
/// and removes the track.
#[test]
fn a_hide_hide_all_or_release_during_a_shrink_is_immediate() {
    let kinds: [fn() -> OverlayBatch; 3] = [
        || OverlayBatch::new().with(h(1), OverlayKind::Hide),
        || OverlayBatch::hide_all(HideReason::MissionControl),
        || OverlayBatch::new().with(h(1), OverlayKind::Release),
    ];
    for batch in kinds {
        let rig = rig(Some(fade(200)));
        put(&rig, solid(), AppearanceChange::Reload);
        put(&rig, solid(), AppearanceChange::Shrink);
        assert!(rig.backend.animation(1, KEY_WIDTH_RING).is_some());
        let _ = rig.control.apply(batch());
        assert!(!rig.backend.is_ordered_in(1));
        assert!(rig.backend.animation(1, KEY_WIDTH_RING).is_none());
        assert!(rig.backend.animation(1, KEY_WIDTH_MASK).is_none());
    }
}

/// TB-09-02: without a transition (absent or 0 ms) a grow is a plain show and a
/// shrink orders out at once.
#[test]
fn without_a_transition_grow_and_shrink_are_instant() {
    for transition in [None, Some(fade(0))] {
        let rig = rig(transition);
        put(&rig, solid(), AppearanceChange::Grow);
        assert!(rig.backend.is_ordered_in(1));
        assert!(rig.backend.animations(1).is_empty());
        put(&rig, solid(), AppearanceChange::Shrink);
        assert!(!rig.backend.is_ordered_in(1));
        assert!(rig.backend.animations(1).is_empty());
    }
}

/// TB-09-02: a move during a shrink keeps the zero model; a later show draws
/// the full ring again.
#[test]
fn a_resize_during_a_shrink_keeps_it_collapsed_and_a_show_restores_the_ring() {
    let rig = rig(Some(fade(200)));
    put(&rig, solid(), AppearanceChange::Reload);
    put(&rig, solid(), AppearanceChange::Shrink);
    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: OverlayRect::new(100.0, 100.0, 400.0, 200.0),
    }));
    let collapsed = rig.backend.ring_override(1).expect("still collapsed");
    assert_eq!(collapsed.outer, collapsed.inner);
    assert!(rig.backend.animation(1, KEY_WIDTH_RING).is_none(), "snapped");
    put(&rig, solid(), AppearanceChange::Reload);
    assert_eq!(rig.backend.ring_override(1), None);
}

/// TB-09-02: a glow's halo fades out with the shrink and in with the grow.
#[test]
fn a_glow_halo_fades_with_the_width() {
    let rig = rig(Some(fade(200)));
    let glow = look(4.0, BorderPaint::Glow(BLUE), BorderPlacement::Above);
    put(&rig, glow, AppearanceChange::Reload);
    put(&rig, glow, AppearanceChange::Shrink);
    let halo = rig.backend.animation(1, KEY_WIDTH_GLOW).expect("halo fade");
    assert!(
        rig.backend.animation(1, KEY_WIDTH_RING).is_none(),
        "the hidden ring does not animate"
    );
    assert_eq!(halo.kind, AnimKind::Basic {
        from: AnimValue::Number(1.0),
        to: AnimValue::Number(0.0),
    });
    assert_eq!(
        rig.backend.layer_override(1, AnimLayer::Glow).map(|o| o.opacity),
        Some(0.0)
    );
    rig.backend.set_media_time(1.0);
    put(&rig, glow, AppearanceChange::Grow);
    let halo = rig.backend.animation(1, KEY_WIDTH_GLOW).expect("halo fade in");
    assert_eq!(halo.kind, AnimKind::Basic {
        from: AnimValue::Number(0.0),
        to: AnimValue::Number(1.0),
    });
}

fn order(rig: &Rig) {
    let _ = rig
        .control
        .apply(OverlayBatch::new().with(h(1), OverlayKind::Order { target_cg_id: 10 }));
}

/// Review fix (part 09 interruption): focus back during a shrink grows the
/// ring, and the actor's follow-up `Order` (same geometry) keeps the grow
/// track and leaves no zero-ring override; a later resize does not collapse it.
#[test]
fn a_grow_during_a_shrink_survives_the_reorder_and_a_later_resize() {
    let rig = rig(Some(fade(200)));
    put(&rig, solid(), AppearanceChange::Reload);
    put(&rig, solid(), AppearanceChange::Shrink);
    rig.backend.set_media_time(0.05);
    put(&rig, solid(), AppearanceChange::Grow);
    order(&rig);
    let (from, to, _) = width_track(&rig).expect("the grow track survives the order");
    assert_eq!((from, to), (shape(0.75), shape(1.0)));
    assert_eq!(rig.backend.ring_override(1), None, "no zero ring after order");

    let _ = rig.control.apply(OverlayBatch::new().with(h(1), OverlayKind::Move {
        frame: OverlayRect::new(100.0, 100.0, 400.0, 200.0),
    }));
    assert_eq!(
        rig.backend.ring_override(1),
        None,
        "a resize does not collapse it"
    );
    assert!(rig.backend.is_ordered_in(1));
}

/// Review fix: a plain grow followed by an `Order` with the same plan keeps
/// the grow track.
#[test]
fn a_grow_keeps_its_track_across_an_order_with_the_same_plan() {
    let rig = rig(Some(fade(200)));
    put(&rig, solid(), AppearanceChange::Grow);
    order(&rig);
    let (from, to, _) = width_track(&rig).expect("grow track kept");
    assert_eq!((from, to), (shape(0.0), shape(1.0)));
}

/// Review fix: a shrink for a handle that is not live creates nothing.
#[test]
fn a_shrink_for_an_unknown_handle_creates_no_panel() {
    let rig = rig(Some(fade(200)));
    put(&rig, solid(), AppearanceChange::Shrink);
    assert_eq!(rig.backend.panels_created(), 0);
    assert_eq!(rig.backend.live_overlays(), 0);
}
