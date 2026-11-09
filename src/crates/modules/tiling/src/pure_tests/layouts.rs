//! Golden fixtures for dwindle, split, master, monocle and floating plans.
//!
//! Each expectation is derived by hand from the cited legacy algorithm.

use super::{assert_rect, frame};
use crate::{
    Gaps, GeometryError, LayoutPlan, LayoutRequest, LayoutType, MasterConfig, MasterPosition,
    PlanStatus, Rect, UntouchedReason, WindowEntry, master_ratio, plan_layout,
};

const LANDSCAPE: Rect = Rect::new(0.0, 0.0, 1920.0, 1080.0);
const PORTRAIT: Rect = Rect::new(0.0, 0.0, 1080.0, 1920.0);

fn windows(count: u32) -> Vec<WindowEntry<u32>> { (1..=count).map(WindowEntry::new).collect() }

fn plan(
    layout: LayoutType,
    count: u32,
    usable: Rect,
    gaps: Gaps,
    ratios: &[f64],
) -> LayoutPlan<u32> {
    plan_with(
        layout,
        &windows(count),
        usable,
        gaps,
        0.6,
        MasterPosition::Auto,
        ratios,
    )
}

fn plan_with(
    layout: LayoutType,
    entries: &[WindowEntry<u32>],
    usable: Rect,
    gaps: Gaps,
    master_ratio: f64,
    master_position: MasterPosition,
    ratios: &[f64],
) -> LayoutPlan<u32> {
    plan_layout(&LayoutRequest {
        layout,
        windows: entries,
        usable,
        gaps,
        master_ratio,
        master_position,
        ratios,
    })
}

#[test]
fn dwindle_landscape_spiral_without_gaps() {
    // Legacy dwindle.rs: split i divides the previous window's frame; landscape
    // starts horizontal and alternates.
    let p = plan(LayoutType::Dwindle, 4, LANDSCAPE, Gaps::ZERO, &[]);
    assert_eq!(p.status, PlanStatus::Ready);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 960.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 960.0, 0.0, 960.0, 540.0);
    assert_rect(frame(&p.frames, 3), 960.0, 540.0, 480.0, 540.0);
    assert_rect(frame(&p.frames, 4), 1440.0, 540.0, 480.0, 540.0);
}

#[test]
fn dwindle_landscape_with_inner_gaps() {
    // split0: (1920-10)/2 = 955, right starts 955+10 = 965.
    // split1 of the right half: (1080-10)/2 = 535, bottom starts 545.
    let p = plan(LayoutType::Dwindle, 3, LANDSCAPE, Gaps::uniform(10.0, 0.0), &[]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 955.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 965.0, 0.0, 955.0, 535.0);
    assert_rect(frame(&p.frames, 3), 965.0, 545.0, 955.0, 535.0);
}

#[test]
fn dwindle_portrait_starts_vertical() {
    let p = plan(LayoutType::Dwindle, 3, PORTRAIT, Gaps::ZERO, &[]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 1080.0, 960.0);
    assert_rect(frame(&p.frames, 2), 0.0, 960.0, 540.0, 960.0);
    assert_rect(frame(&p.frames, 3), 540.0, 960.0, 540.0, 960.0);
}

#[test]
fn dwindle_node_ratios_are_clamped_to_five_to_ninety_five_percent() {
    let p = plan(LayoutType::Dwindle, 2, LANDSCAPE, Gaps::ZERO, &[0.7]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 1344.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 1344.0, 0.0, 576.0, 1080.0);
    let p = plan(LayoutType::Dwindle, 2, LANDSCAPE, Gaps::ZERO, &[0.99]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 1824.0, 1080.0);
    let p = plan(LayoutType::Dwindle, 2, LANDSCAPE, Gaps::ZERO, &[0.0]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 96.0, 1080.0);
}

#[test]
fn dwindle_single_window_takes_the_whole_area() {
    let p = plan(LayoutType::Dwindle, 1, LANDSCAPE, Gaps::uniform(10.0, 0.0), &[]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 1920.0, 1080.0);
}

#[test]
fn split_horizontal_equal_and_gaps() {
    // (1920 - 2*15)/3 = 630 each; x = 0, 645, 1290.
    let gaps = Gaps::new(15.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let p = plan(LayoutType::SplitHorizontal, 3, LANDSCAPE, gaps, &[]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 630.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 645.0, 0.0, 630.0, 1080.0);
    assert_rect(frame(&p.frames, 3), 1290.0, 0.0, 630.0, 1080.0);
}

#[test]
fn split_cumulative_boundaries() {
    // Boundaries 0.25 and 0.5 of 1920 -> widths 480, 480, 960.
    let p = plan(LayoutType::SplitHorizontal, 3, LANDSCAPE, Gaps::ZERO, &[
        0.25, 0.5,
    ]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 480.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 480.0, 0.0, 480.0, 1080.0);
    assert_rect(frame(&p.frames, 3), 960.0, 0.0, 960.0, 1080.0);
    // With a 20 point gap: available 1880; boundary 0.25 -> 470, widths 470/470/940.
    let gaps = Gaps::new(20.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let p = plan(LayoutType::SplitHorizontal, 3, LANDSCAPE, gaps, &[0.25, 0.5]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 470.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 490.0, 0.0, 470.0, 1080.0);
    assert_rect(frame(&p.frames, 3), 980.0, 0.0, 940.0, 1080.0);
}

#[test]
fn invalid_cumulative_boundaries_behave_as_equal_split() {
    for ratios in [
        &[0.6, 0.4][..],
        &[0.5][..],
        &[0.0, 0.5][..],
        &[0.5, 1.0][..],
        &[f64::NAN, 0.5][..],
    ] {
        let p = plan(LayoutType::SplitHorizontal, 3, LANDSCAPE, Gaps::ZERO, ratios);
        assert_eq!(p.status, PlanStatus::Ready, "{ratios:?}");
        assert_rect(frame(&p.frames, 2), 640.0, 0.0, 640.0, 1080.0);
    }
}

#[test]
fn split_auto_follows_orientation_but_explicit_variants_do_not() {
    let landscape = plan(LayoutType::Split, 2, LANDSCAPE, Gaps::ZERO, &[]);
    assert_rect(frame(&landscape.frames, 2), 960.0, 0.0, 960.0, 1080.0);
    let portrait = plan(LayoutType::Split, 2, PORTRAIT, Gaps::ZERO, &[]);
    assert_rect(frame(&portrait.frames, 2), 0.0, 960.0, 1080.0, 960.0);
    let vertical = plan(LayoutType::SplitVertical, 3, LANDSCAPE, Gaps::ZERO, &[]);
    assert_rect(frame(&vertical.frames, 3), 0.0, 720.0, 1920.0, 360.0);
    let horizontal = plan(LayoutType::SplitHorizontal, 2, PORTRAIT, Gaps::ZERO, &[]);
    assert_rect(frame(&horizontal.frames, 2), 540.0, 0.0, 540.0, 1920.0);
}

#[test]
fn master_left_on_landscape_with_gaps() {
    // Ratio 0.6, no gap: master 1152 wide; stack 768 wide, two 540 tall rows.
    let p = plan(LayoutType::Master, 3, LANDSCAPE, Gaps::ZERO, &[]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 1152.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 1152.0, 0.0, 768.0, 540.0);
    assert_rect(frame(&p.frames, 3), 1152.0, 540.0, 768.0, 540.0);
    // Gap 10: available 1910 -> master 1146, stack 764 at x 1156; rows (1080-10)/2 = 535.
    let p = plan(LayoutType::Master, 3, LANDSCAPE, Gaps::uniform(10.0, 0.0), &[]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 1146.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 1156.0, 0.0, 764.0, 535.0);
    assert_rect(frame(&p.frames, 3), 1156.0, 545.0, 764.0, 535.0);
}

#[test]
fn master_auto_is_top_on_portrait() {
    let p = plan(LayoutType::Master, 3, PORTRAIT, Gaps::ZERO, &[]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 1080.0, 1152.0);
    assert_rect(frame(&p.frames, 2), 0.0, 1152.0, 540.0, 768.0);
    assert_rect(frame(&p.frames, 3), 540.0, 1152.0, 540.0, 768.0);
}

#[test]
fn master_right_top_bottom_explicit() {
    let right = plan_with(
        LayoutType::Master,
        &windows(3),
        LANDSCAPE,
        Gaps::ZERO,
        0.6,
        MasterPosition::Right,
        &[],
    );
    assert_rect(frame(&right.frames, 1), 768.0, 0.0, 1152.0, 1080.0);
    assert_rect(frame(&right.frames, 2), 0.0, 0.0, 768.0, 540.0);
    let bottom = plan_with(
        LayoutType::Master,
        &windows(3),
        LANDSCAPE,
        Gaps::ZERO,
        0.6,
        MasterPosition::Bottom,
        &[],
    );
    assert_rect(frame(&bottom.frames, 1), 0.0, 432.0, 1920.0, 648.0);
    assert_rect(frame(&bottom.frames, 2), 0.0, 0.0, 960.0, 432.0);
    assert_rect(frame(&bottom.frames, 3), 960.0, 0.0, 960.0, 432.0);
    // Explicit left on a portrait area is not rotated to top.
    let left = plan_with(
        LayoutType::Master,
        &windows(2),
        PORTRAIT,
        Gaps::ZERO,
        0.5,
        MasterPosition::Left,
        &[],
    );
    assert_rect(frame(&left.frames, 1), 0.0, 0.0, 540.0, 1920.0);
}

#[test]
fn master_ratio_is_clamped_and_overridable() {
    let low = plan_with(
        LayoutType::Master,
        &windows(2),
        LANDSCAPE,
        Gaps::ZERO,
        0.0,
        MasterPosition::Left,
        &[],
    );
    assert_rect(frame(&low.frames, 1), 0.0, 0.0, 192.0, 1080.0);
    let high = plan_with(
        LayoutType::Master,
        &windows(2),
        LANDSCAPE,
        Gaps::ZERO,
        1.0,
        MasterPosition::Left,
        &[],
    );
    assert_rect(frame(&high.frames, 1), 0.0, 0.0, 1728.0, 1080.0);
    let config = MasterConfig {
        ratio: 60,
        position: MasterPosition::Auto,
    };
    assert_eq!(master_ratio(&config, None), 0.6);
    assert_eq!(master_ratio(&config, Some(0.75)), 0.75);
}

#[test]
fn monocle_gives_every_window_the_full_usable_frame() {
    let p = plan(LayoutType::Monocle, 3, LANDSCAPE, Gaps::uniform(10.0, 0.0), &[]);
    for id in 1..=3 {
        assert_rect(frame(&p.frames, id), 0.0, 0.0, 1920.0, 1080.0);
    }
}

#[test]
fn floating_writes_nothing_and_accounts_for_every_window() {
    let p = plan(LayoutType::Floating, 3, LANDSCAPE, Gaps::ZERO, &[]);
    assert!(p.frames.is_empty());
    assert_eq!(p.untouched.len(), 3);
    assert!(p.untouched.iter().all(|(_, reason)| *reason == UntouchedReason::FloatingLayout));
    assert_eq!(p.accounted(), 3);
}

#[test]
fn empty_workspace_is_an_empty_ready_plan() {
    for layout in [
        LayoutType::Dwindle,
        LayoutType::Grid,
        LayoutType::Master,
        LayoutType::Split,
    ] {
        let p = plan(layout, 0, LANDSCAPE, Gaps::ZERO, &[]);
        assert_eq!(p.status, PlanStatus::Ready);
        assert_eq!(p.accounted(), 0);
    }
}

#[test]
fn negative_origin_usable_area_offsets_frames() {
    let usable = Rect::new(-1920.0, -176.0, 1920.0, 1056.0);
    let p = plan(LayoutType::SplitHorizontal, 2, usable, Gaps::ZERO, &[]);
    assert_rect(frame(&p.frames, 1), -1920.0, -176.0, 960.0, 1056.0);
    assert_rect(frame(&p.frames, 2), -960.0, -176.0, 960.0, 1056.0);
}

#[test]
fn infeasible_geometry_yields_no_frames_but_accounts_for_everyone() {
    let layouts = [
        LayoutType::Dwindle,
        LayoutType::Split,
        LayoutType::SplitVertical,
        LayoutType::SplitHorizontal,
        LayoutType::Monocle,
        LayoutType::Master,
        LayoutType::Grid,
    ];
    for layout in layouts {
        // Zero usable area.
        let p = plan(layout, 4, Rect::new(0.0, 0.0, 0.0, 1080.0), Gaps::ZERO, &[]);
        assert_eq!(
            p.status,
            PlanStatus::Infeasible(GeometryError::NoUsableArea),
            "{layout:?}"
        );
        assert!(p.frames.is_empty());
        assert_eq!(p.accounted(), 4);
        assert!(p.untouched.iter().all(|(_, r)| *r == UntouchedReason::Infeasible));
        // Non-finite usable area.
        let p = plan(layout, 2, Rect::new(f64::NAN, 0.0, 100.0, 100.0), Gaps::ZERO, &[
        ]);
        assert!(matches!(
            p.status,
            PlanStatus::Infeasible(GeometryError::NonFinite { .. })
        ));
        // Negative size.
        let p = plan(layout, 2, Rect::new(0.0, 0.0, -5.0, 100.0), Gaps::ZERO, &[]);
        assert!(matches!(p.status, PlanStatus::Infeasible(_)));
    }
}

#[test]
fn excessive_inner_gaps_degenerate_instead_of_producing_negative_frames() {
    // 4 columns with 700 point gaps on a 1920 wide area: negative widths.
    let gaps = Gaps::new(700.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let p = plan(LayoutType::SplitHorizontal, 4, LANDSCAPE, gaps, &[]);
    assert_eq!(p.status, PlanStatus::Infeasible(GeometryError::DegenerateFrame));
    assert!(p.frames.is_empty());
    assert_eq!(p.accounted(), 4);
}

#[test]
fn non_finite_ratios_and_gaps_are_infeasible_not_nan() {
    let p = plan(LayoutType::Dwindle, 3, LANDSCAPE, Gaps::ZERO, &[f64::NAN]);
    assert!(matches!(
        p.status,
        PlanStatus::Infeasible(GeometryError::NonFinite { .. })
    ));
    let p = plan_with(
        LayoutType::Master,
        &windows(3),
        LANDSCAPE,
        Gaps::ZERO,
        f64::INFINITY,
        MasterPosition::Auto,
        &[],
    );
    assert!(matches!(
        p.status,
        PlanStatus::Infeasible(GeometryError::NonFinite { .. })
    ));
    let p = plan(LayoutType::Grid, 3, LANDSCAPE, Gaps::ZERO, &[f64::NEG_INFINITY]);
    assert!(matches!(
        p.status,
        PlanStatus::Infeasible(GeometryError::NonFinite { .. })
    ));
    let p = plan(
        LayoutType::Dwindle,
        3,
        LANDSCAPE,
        Gaps::uniform(f64::NAN, 0.0),
        &[],
    );
    assert!(matches!(p.status, PlanStatus::Infeasible(_)));
    let p = plan(LayoutType::Dwindle, 3, LANDSCAPE, Gaps::uniform(-4.0, 0.0), &[]);
    assert!(matches!(
        p.status,
        PlanStatus::Infeasible(GeometryError::Negative { .. })
    ));
}

#[test]
fn every_finite_plan_has_only_finite_positive_frames() {
    // Deterministic sweep over layouts, counts, sizes and gaps.
    let sizes = [
        (1920.0, 1080.0),
        (1080.0, 1920.0),
        (800.0, 800.0),
        (3024.0, 1964.0),
    ];
    let gap_set = [
        Gaps::ZERO,
        Gaps::uniform(8.0, 0.0),
        Gaps::new(3.0, 11.0, 0.0, 0.0, 0.0, 0.0),
    ];
    let layouts = [
        LayoutType::Dwindle,
        LayoutType::Split,
        LayoutType::SplitVertical,
        LayoutType::SplitHorizontal,
        LayoutType::Monocle,
        LayoutType::Master,
        LayoutType::Grid,
    ];
    for layout in layouts {
        for (w, h) in sizes {
            for gaps in gap_set {
                for count in 0..=10 {
                    let p = plan(layout, count, Rect::new(-w / 3.0, 17.0, w, h), gaps, &[]);
                    assert_eq!(p.status, PlanStatus::Ready, "{layout:?} {w}x{h} n={count}");
                    assert_eq!(p.accounted(), count as usize);
                    for (_, f) in &p.frames {
                        assert!(f.has_area(), "{layout:?} {w}x{h} n={count}: {f:?}");
                    }
                }
            }
        }
    }
}
