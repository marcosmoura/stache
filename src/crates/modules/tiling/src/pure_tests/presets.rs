//! Preset math, lookup, precedence and the one-shot floating policy.

use super::assert_rect;
use crate::{
    DimensionValue, FloatingContext, FloatingPosition, FloatingPreset, FloatingTarget,
    FloatingTrigger, GeometryError, LayoutType, PositionDecision, PositionNoOp, PresetNoOp,
    PresetOutcome, PresetRequest, Rect, center_frame, choose_floating_target, default_position,
    find_preset, preset_frame, resolve_preset,
};

fn pct(value: &str) -> DimensionValue { DimensionValue::Percentage(value.to_owned()) }

fn preset(name: &str, w: DimensionValue, h: DimensionValue) -> FloatingPreset {
    FloatingPreset {
        name: name.to_owned(),
        width: w,
        height: h,
        x: None,
        y: None,
        center: false,
    }
}

const USABLE: Rect = Rect::new(0.0, 0.0, 1920.0, 1080.0);

#[test]
fn half_screen_presets_leave_exactly_one_inner_gap() {
    // Legacy floating.rs:63-125: 50% width = (1920 - 10) / 2 = 955; the right
    // half starts at 0 + 955 + 10 = 965.
    let mut left = preset("left", pct("50%"), pct("100%"));
    left.x = Some(DimensionValue::Pixels(0));
    let mut right = preset("right", pct("50%"), pct("100%"));
    right.x = Some(pct("50%"));
    assert_rect(
        preset_frame(&left, &USABLE, 10.0, 10.0).unwrap(),
        0.0,
        0.0,
        955.0,
        1080.0,
    );
    assert_rect(
        preset_frame(&right, &USABLE, 10.0, 10.0).unwrap(),
        965.0,
        0.0,
        955.0,
        1080.0,
    );
}

#[test]
fn quarter_presets_use_the_vertical_gap_and_an_offset_area() {
    // Usable (100, 46, 1800, 1000), inner (10, 20): w = 895, h = 490,
    // x = 100 + 895 + 10 = 1005, y = 46 + 490 + 20 = 556.
    let usable = Rect::new(100.0, 46.0, 1800.0, 1000.0);
    let mut p = preset("br", pct("50%"), pct("50%"));
    p.x = Some(pct("50%"));
    p.y = Some(pct("50%"));
    assert_rect(
        preset_frame(&p, &usable, 10.0, 20.0).unwrap(),
        1005.0,
        556.0,
        895.0,
        490.0,
    );
    // Origin quarter.
    let tl = preset("tl", pct("50%"), pct("50%"));
    assert_rect(
        preset_frame(&tl, &usable, 10.0, 20.0).unwrap(),
        100.0,
        46.0,
        895.0,
        490.0,
    );
}

#[test]
fn half_width_with_non_half_x_resolves_the_position_normally() {
    let usable = Rect::new(100.0, 0.0, 1800.0, 1000.0);
    let mut p = preset("p", pct("50%"), pct("100%"));
    p.x = Some(pct("25%"));
    // x = 100 + 0.25 * 1800 = 550; width (1800-10)/2 = 895.
    assert_rect(
        preset_frame(&p, &usable, 10.0, 0.0).unwrap(),
        550.0,
        0.0,
        895.0,
        1000.0,
    );
}

#[test]
fn center_ignores_x_and_y() {
    let mut p = preset("c", pct("80%"), DimensionValue::Pixels(600));
    p.center = true;
    p.x = Some(DimensionValue::Pixels(5));
    p.y = Some(pct("90%"));
    // width 0.8 * 1920 = 1536 -> x = 192; height 600 -> y = 240.
    assert_rect(
        preset_frame(&p, &USABLE, 0.0, 0.0).unwrap(),
        192.0,
        240.0,
        1536.0,
        600.0,
    );
}

#[test]
fn sizes_and_positions_are_clamped_into_the_usable_area() {
    let mut p = preset("big", DimensionValue::Pixels(5000), DimensionValue::Pixels(5000));
    p.x = Some(DimensionValue::Pixels(300));
    assert_rect(
        preset_frame(&p, &USABLE, 0.0, 0.0).unwrap(),
        0.0,
        0.0,
        1920.0,
        1080.0,
    );
    let mut p = preset("edge", DimensionValue::Pixels(1000), DimensionValue::Pixels(500));
    p.x = Some(DimensionValue::Pixels(5000));
    p.y = Some(DimensionValue::Pixels(5000));
    assert_rect(
        preset_frame(&p, &USABLE, 0.0, 0.0).unwrap(),
        920.0,
        580.0,
        1000.0,
        500.0,
    );
    // A zero percentage is raised to one point.
    let tiny = preset("tiny", pct("0%"), pct("0%"));
    assert_rect(
        preset_frame(&tiny, &USABLE, 0.0, 0.0).unwrap(),
        0.0,
        0.0,
        1.0,
        1.0,
    );
    // Negative origin display.
    let usable = Rect::new(-1920.0, -176.0, 1920.0, 1056.0);
    let mut p = preset("n", DimensionValue::Pixels(400), DimensionValue::Pixels(300));
    p.x = Some(DimensionValue::Pixels(100));
    p.y = Some(DimensionValue::Pixels(50));
    assert_rect(
        preset_frame(&p, &usable, 0.0, 0.0).unwrap(),
        -1820.0,
        -126.0,
        400.0,
        300.0,
    );
}

#[test]
fn preset_geometry_errors_are_explicit() {
    let p = preset("p", pct("50%"), pct("50%"));
    assert_eq!(
        preset_frame(&p, &Rect::new(0.0, 0.0, 0.5, 100.0), 0.0, 0.0),
        Err(GeometryError::NoUsableArea)
    );
    assert!(matches!(
        preset_frame(&p, &Rect::new(0.0, f64::NAN, 100.0, 100.0), 0.0, 0.0),
        Err(GeometryError::NonFinite { .. })
    ));
    assert!(matches!(
        preset_frame(&p, &USABLE, f64::INFINITY, 0.0),
        Err(GeometryError::NonFinite { .. })
    ));
    assert!(matches!(
        preset_frame(&p, &USABLE, -1.0, 0.0),
        Err(GeometryError::Negative { .. })
    ));
}

#[test]
fn lookup_is_case_insensitive() {
    let presets = vec![
        preset("Center", pct("50%"), pct("50%")),
        preset("ÜBER", pct("10%"), pct("10%")),
    ];
    assert_eq!(find_preset(&presets, "CENTER").unwrap().name, "Center");
    assert_eq!(find_preset(&presets, "über").unwrap().name, "ÜBER");
    assert!(find_preset(&presets, "missing").is_none());
    assert!(find_preset(&[], "center").is_none());
}

fn request<'a>(
    presets: &'a [FloatingPreset],
    name: &'a str,
    layout: LayoutType,
) -> PresetRequest<'a> {
    PresetRequest {
        presets,
        name,
        layout,
        has_focused_window: true,
        window_eligible: true,
        usable: USABLE,
        inner_h: 0.0,
        inner_v: 0.0,
    }
}

#[test]
fn explicit_preset_applies_only_in_a_floating_workspace() {
    let presets = vec![preset("half", pct("50%"), pct("100%"))];
    let in_floating = resolve_preset(&request(&presets, "HALF", LayoutType::Floating));
    assert!(matches!(in_floating, PresetOutcome::Target(_)));
    // Every other layout, including a window that is individually floating in a
    // tiled workspace, is a silent no-op with an internal reason only.
    for layout in [
        LayoutType::Dwindle,
        LayoutType::Split,
        LayoutType::SplitVertical,
        LayoutType::SplitHorizontal,
        LayoutType::Monocle,
        LayoutType::Master,
        LayoutType::Grid,
    ] {
        assert_eq!(
            resolve_preset(&request(&presets, "half", layout)),
            PresetOutcome::NoOp(PresetNoOp::NotFloatingLayout),
            "{layout:?}"
        );
    }
}

#[test]
fn preset_no_op_reasons() {
    let presets = vec![preset("half", pct("50%"), pct("100%"))];
    assert_eq!(
        resolve_preset(&request(&presets, "nope", LayoutType::Floating)),
        PresetOutcome::NoOp(PresetNoOp::UnknownPreset)
    );
    let mut r = request(&presets, "half", LayoutType::Floating);
    r.has_focused_window = false;
    assert_eq!(
        resolve_preset(&r),
        PresetOutcome::NoOp(PresetNoOp::NoFocusedWindow)
    );
    let mut r = request(&presets, "half", LayoutType::Floating);
    r.window_eligible = false; // e.g. a Grid overflow window: explicit action only if eligible
    assert_eq!(resolve_preset(&r), PresetOutcome::NoOp(PresetNoOp::NotEligible));
    let mut r = request(&presets, "half", LayoutType::Floating);
    r.usable = Rect::new(0.0, 0.0, 0.0, 0.0);
    assert!(matches!(resolve_preset(&r), PresetOutcome::Infeasible(_)));
}

fn ctx(trigger: FloatingTrigger, layout: LayoutType) -> FloatingContext {
    FloatingContext {
        trigger,
        layout,
        explicit_floating: false,
        grid_overflow: false,
        already_positioned: false,
    }
}

#[test]
fn new_floating_windows_get_default_position_once() {
    use FloatingPosition::{Center, Default as Keep};
    let new_in_floating = ctx(FloatingTrigger::NewWindow, LayoutType::Floating);
    assert_eq!(
        default_position(Center, &new_in_floating),
        PositionDecision::Center
    );
    assert_eq!(
        default_position(Keep, &new_in_floating),
        PositionDecision::KeepFrame
    );

    let mut toggled = ctx(FloatingTrigger::ExplicitFloatToggle, LayoutType::Dwindle);
    toggled.explicit_floating = true;
    assert_eq!(default_position(Center, &toggled), PositionDecision::Center);

    let mut new_explicit = ctx(FloatingTrigger::NewWindow, LayoutType::Dwindle);
    new_explicit.explicit_floating = true;
    assert_eq!(default_position(Center, &new_explicit), PositionDecision::Center);

    // Once only.
    let mut again = new_in_floating;
    again.already_positioned = true;
    assert_eq!(
        default_position(Center, &again),
        PositionDecision::NoOp(PositionNoOp::AlreadyPositioned)
    );
}

#[test]
fn other_triggers_never_position() {
    for trigger in [
        FloatingTrigger::InitialAdoption,
        FloatingTrigger::LayoutChange,
        FloatingTrigger::ConfigReload,
        FloatingTrigger::FocusChange,
        FloatingTrigger::Reconcile,
    ] {
        let mut c = ctx(trigger, LayoutType::Floating);
        c.explicit_floating = true;
        assert_eq!(
            default_position(FloatingPosition::Center, &c),
            PositionDecision::NoOp(PositionNoOp::TriggerDoesNotPosition),
            "{trigger:?}"
        );
    }
}

#[test]
fn tiled_windows_and_grid_overflow_are_not_positioned() {
    let tiled = ctx(FloatingTrigger::NewWindow, LayoutType::Dwindle);
    assert_eq!(
        default_position(FloatingPosition::Center, &tiled),
        PositionDecision::NoOp(PositionNoOp::NotFloating)
    );
    // Grid overflow bypasses automatic centering even when the toggle looks floating.
    let mut overflow = ctx(FloatingTrigger::NewWindow, LayoutType::Grid);
    overflow.grid_overflow = true;
    assert_eq!(
        default_position(FloatingPosition::Center, &overflow),
        PositionDecision::NoOp(PositionNoOp::GridOverflow)
    );
    overflow.explicit_floating = true;
    assert_eq!(
        default_position(FloatingPosition::Center, &overflow),
        PositionDecision::NoOp(PositionNoOp::GridOverflow)
    );
    // A later eligible *explicit* toggle is a user action and positions normally.
    let mut explicit = ctx(FloatingTrigger::ExplicitFloatToggle, LayoutType::Grid);
    explicit.explicit_floating = true;
    assert_eq!(
        default_position(FloatingPosition::Center, &explicit),
        PositionDecision::Center
    );
}

#[test]
fn explicit_preset_outranks_default_position() {
    let frame = Rect::new(1.0, 2.0, 3.0, 4.0);
    let target = Some(PresetOutcome::Target(frame));
    assert_eq!(
        choose_floating_target(target, PositionDecision::Center),
        FloatingTarget::Preset(frame)
    );
    // A preset no-op (outside a Floating workspace) does not suppress the default.
    let no_op = Some(PresetOutcome::NoOp(PresetNoOp::NotFloatingLayout));
    assert_eq!(
        choose_floating_target(no_op, PositionDecision::Center),
        FloatingTarget::Center
    );
    assert_eq!(
        choose_floating_target(None, PositionDecision::Center),
        FloatingTarget::Center
    );
    assert_eq!(
        choose_floating_target(None, PositionDecision::KeepFrame),
        FloatingTarget::Keep
    );
    assert_eq!(
        choose_floating_target(None, PositionDecision::NoOp(PositionNoOp::NotFloating)),
        FloatingTarget::Keep
    );
}

#[test]
fn centering_preserves_size_and_pins_oversized_windows() {
    let usable = Rect::new(0.0, 46.0, 1920.0, 1034.0);
    let observed = Rect::new(10.0, 10.0, 400.0, 300.0);
    assert_rect(
        center_frame(&observed, &usable).unwrap(),
        760.0,
        413.0,
        400.0,
        300.0,
    );
    let huge = Rect::new(0.0, 0.0, 3000.0, 2000.0);
    assert_rect(center_frame(&huge, &usable).unwrap(), 0.0, 46.0, 3000.0, 2000.0);
    let offset = Rect::new(-1920.0, -176.0, 1920.0, 1056.0);
    assert_rect(
        center_frame(&observed, &offset).unwrap(),
        -1160.0,
        202.0,
        400.0,
        300.0,
    );
    assert!(matches!(
        center_frame(&Rect::new(0.0, 0.0, f64::NAN, 1.0), &usable),
        Err(GeometryError::NonFinite { .. })
    ));
    assert_eq!(
        center_frame(&Rect::new(0.0, 0.0, 0.0, 1.0), &usable),
        Err(GeometryError::DegenerateFrame)
    );
}

#[test]
fn preset_on_open_is_inert() {
    // Nothing in the pure modules may read the declared-only config field.
    for (name, source) in [
        ("presets", include_str!("../presets.rs")),
        ("rules", include_str!("../rules.rs")),
        ("minimum_size", include_str!("../minimum_size.rs")),
        ("geometry", include_str!("../geometry.rs")),
        ("layout", include_str!("../layout.rs")),
        ("layout/dispatch", include_str!("../layout/dispatch.rs")),
        ("layout/ordering", include_str!("../layout/ordering.rs")),
    ] {
        assert!(
            !source.contains("preset_on_open"),
            "{name} must not consume preset-on-open"
        );
    }
}
