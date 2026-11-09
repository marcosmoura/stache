//! Minimum-size enforcement, refusal handling and ratio state.

use super::{assert_rect, frame};
use crate::{
    Change, FrameObservation, Gaps, InfeasibleReason, LayoutType, MinimumConstraint,
    MinimumRequest, MinimumStatus, RatioState, Rect, Size, default_ratios, enforce,
    find_violations, guarded_ratio_change, infer_minimum, observe_write, redistribute,
    resize_cumulative,
};

const AREA: Rect = Rect::new(0.0, 0.0, 1200.0, 600.0);

fn run(
    layout: LayoutType,
    ids: &[u32],
    usable: Rect,
    gaps: Gaps,
    ratios: &[f64],
    minimums: &[(u32, Size)],
) -> crate::MinimumResult<u32> {
    let lookup = |id: &u32| minimums.iter().find(|(k, _)| k == id).map(|(_, s)| *s);
    enforce(&MinimumRequest {
        layout,
        ids,
        usable,
        gaps,
        ratios,
        minimum_of: &lookup,
    })
}

fn width(w: f64) -> Size { Size::new(w, 0.0) }

#[test]
fn constraint_prefers_reported_over_inferred_and_ignores_invalid() {
    let mut c = MinimumConstraint {
        reported: Some(Size::new(300.0, 200.0)),
        inferred: Some(Size::new(500.0, 400.0)),
    };
    assert_eq!(c.effective(), Some(Size::new(300.0, 200.0)));
    c.reported = None;
    assert_eq!(c.effective(), Some(Size::new(500.0, 400.0)));
    c.clear_inferred();
    assert_eq!(c.effective(), None);
    let bad = MinimumConstraint {
        reported: Some(Size::new(f64::NAN, 1.0)),
        inferred: Some(width(50.0)),
    };
    assert_eq!(bad.effective(), Some(width(50.0)));
}

#[test]
fn violations_use_a_one_point_tolerance() {
    let frames = [
        (1_u32, Rect::new(0.0, 0.0, 599.5, 100.0)),
        (2, Rect::new(0.0, 0.0, 598.0, 100.0)),
    ];
    let lookup = |_: &u32| Some(Size::new(600.0, 100.0));
    let v = find_violations(&frames, &lookup);
    assert_eq!(v.len(), 1);
    assert_eq!(
        (v[0].id, v[0].index, v[0].width, v[0].height),
        (2, 1, true, false)
    );
}

#[test]
fn split_redistributes_by_hand_computed_shares() {
    // Equal split of 1200 is 400 each. Window 1 needs 600 (ratio 0.5): its share
    // grows by 1/6, taken from window 2 (next in index order with room), giving
    // shares 1/2, 1/6, 1/3 -> widths 600, 200, 400.
    let r = run(
        LayoutType::SplitHorizontal,
        &[1, 2, 3],
        AREA,
        Gaps::ZERO,
        &[],
        &[(1, width(600.0))],
    );
    assert_eq!(r.status, MinimumStatus::Adjusted);
    assert_rect(frame(&r.frames, 1), 0.0, 0.0, 600.0, 600.0);
    assert_rect(frame(&r.frames, 2), 600.0, 0.0, 200.0, 600.0);
    assert_rect(frame(&r.frames, 3), 800.0, 0.0, 400.0, 600.0);
    let ratios = r.ratios.unwrap();
    assert!((ratios[0] - 0.5).abs() < 1e-9 && (ratios[1] - 2.0 / 3.0).abs() < 1e-9);
}

#[test]
fn split_preserves_user_ratios_where_possible() {
    // User boundaries 0.25 / 0.5 -> shares 0.25, 0.25, 0.5. Window 2 needs 480
    // (0.4): it takes 0.15 from window 1 (0.25 -> 0.1), leaving 0.1, 0.4, 0.5.
    let r = run(
        LayoutType::SplitHorizontal,
        &[1, 2, 3],
        AREA,
        Gaps::ZERO,
        &[0.25, 0.5],
        &[(2, width(480.0))],
    );
    assert_eq!(r.status, MinimumStatus::Adjusted);
    assert_rect(frame(&r.frames, 1), 0.0, 0.0, 120.0, 600.0);
    assert_rect(frame(&r.frames, 2), 120.0, 0.0, 480.0, 600.0);
    assert_rect(frame(&r.frames, 3), 600.0, 0.0, 600.0, 600.0);
}

#[test]
fn split_vertical_uses_heights_and_gaps() {
    // 600 high, gap 20, 2 windows -> available 580. Window 2 needs 400 tall.
    let gaps = Gaps::new(0.0, 20.0, 0.0, 0.0, 0.0, 0.0);
    let lookup = |id: &u32| (*id == 2).then(|| Size::new(0.0, 400.0));
    let r = enforce(&MinimumRequest {
        layout: LayoutType::SplitVertical,
        ids: &[1, 2],
        usable: AREA,
        gaps,
        ratios: &[],
        minimum_of: &lookup,
    });
    assert_eq!(r.status, MinimumStatus::Adjusted);
    assert_rect(frame(&r.frames, 1), 0.0, 0.0, 1200.0, 180.0);
    assert_rect(frame(&r.frames, 2), 0.0, 200.0, 1200.0, 400.0);
}

#[test]
fn split_satisfied_and_within_tolerance_changes_nothing() {
    let r = run(LayoutType::SplitHorizontal, &[1, 2], AREA, Gaps::ZERO, &[], &[(
        1,
        width(300.0),
    )]);
    assert_eq!(r.status, MinimumStatus::Satisfied);
    assert!(r.ratios.is_none());
    assert_rect(frame(&r.frames, 1), 0.0, 0.0, 600.0, 600.0);
    let r = run(LayoutType::SplitHorizontal, &[1, 2], AREA, Gaps::ZERO, &[], &[(
        1,
        width(600.5),
    )]);
    assert_eq!(r.status, MinimumStatus::Satisfied);
}

#[test]
fn impossible_split_is_reported_and_frames_make_no_compliance_claim() {
    let r = run(LayoutType::SplitHorizontal, &[1, 2], AREA, Gaps::ZERO, &[], &[
        (1, width(700.0)),
        (2, width(700.0)),
    ]);
    assert_eq!(
        r.status,
        MinimumStatus::Infeasible(InfeasibleReason::RequiredExceedsAvailable {
            required: 1400.0,
            available: 1200.0
        })
    );
    // The unmodified equal split, not a proportionally scaled "compliant-looking" one.
    assert_rect(frame(&r.frames, 1), 0.0, 0.0, 600.0, 600.0);
    assert_rect(frame(&r.frames, 2), 600.0, 0.0, 600.0, 600.0);
    assert!(r.ratios.is_none());
}

#[test]
fn unusable_inputs_are_infeasible_not_panics() {
    let r = run(
        LayoutType::Dwindle,
        &[1, 2],
        Rect::new(0.0, 0.0, 0.0, 0.0),
        Gaps::ZERO,
        &[],
        &[(1, width(10.0))],
    );
    assert!(matches!(
        r.status,
        MinimumStatus::Infeasible(InfeasibleReason::NoUsableSpace)
    ));
    assert!(r.frames.is_empty());
    let r = run(LayoutType::Dwindle, &[1, 2], AREA, Gaps::ZERO, &[f64::NAN], &[(
        1,
        width(10.0),
    )]);
    assert!(matches!(r.status, MinimumStatus::Infeasible(_)));
    // Gaps that swallow the axis.
    let gaps = Gaps::new(2000.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let r = run(LayoutType::SplitHorizontal, &[1, 2], AREA, gaps, &[], &[(
        1,
        width(10.0),
    )]);
    assert!(matches!(r.status, MinimumStatus::Infeasible(_)));
}

#[test]
fn layouts_without_ratio_solving_are_returned_untouched() {
    for layout in [
        LayoutType::Monocle,
        LayoutType::Master,
        LayoutType::Floating,
    ] {
        let r = run(layout, &[1, 2], AREA, Gaps::ZERO, &[], &[(1, width(5000.0))]);
        assert_eq!(r.status, MinimumStatus::Satisfied, "{layout:?}");
        assert!(r.frames.is_empty());
    }
    let single = run(LayoutType::Dwindle, &[1], AREA, Gaps::ZERO, &[], &[(
        1,
        width(5000.0),
    )]);
    assert_eq!(single.status, MinimumStatus::Satisfied);
}

#[test]
fn dwindle_grows_the_first_window_by_hand_computed_ratio() {
    // 1200x600 landscape, window 1 needs 800: deficit 200 / 1200 = 1/6, so
    // split ratio 0.5 + 1/6 = 2/3 and widths 800 / 400.
    let r = run(LayoutType::Dwindle, &[1, 2], AREA, Gaps::ZERO, &[], &[(
        1,
        width(800.0),
    )]);
    assert_eq!(r.status, MinimumStatus::Adjusted);
    assert_rect(frame(&r.frames, 1), 0.0, 0.0, 800.0, 600.0);
    assert_rect(frame(&r.frames, 2), 800.0, 0.0, 400.0, 600.0);
    assert!((r.ratios.unwrap()[0] - 2.0 / 3.0).abs() < 1e-9);
    // Window 2 needs 800 instead: ratio 0.5 - 1/6.
    let r = run(LayoutType::Dwindle, &[1, 2], AREA, Gaps::ZERO, &[], &[(
        2,
        width(800.0),
    )]);
    assert_eq!(r.status, MinimumStatus::Adjusted);
    assert_rect(frame(&r.frames, 1), 0.0, 0.0, 400.0, 600.0);
    assert_rect(frame(&r.frames, 2), 400.0, 0.0, 800.0, 600.0);
}

#[test]
fn impossible_dwindle_stops_after_one_pass_without_looping() {
    // Both windows want 800 of 1200: the adjustments cancel, so the solver stops
    // with the original frames and says so.
    let r = run(LayoutType::Dwindle, &[1, 2], AREA, Gaps::ZERO, &[], &[
        (1, width(800.0)),
        (2, width(800.0)),
    ]);
    assert_eq!(r.status, MinimumStatus::Unresolved { remaining: 2 });
    assert_rect(frame(&r.frames, 1), 0.0, 0.0, 600.0, 600.0);
}

#[test]
fn many_constrained_windows_terminate_within_the_pass_limit() {
    let ids: Vec<u32> = (1..=200).collect();
    let minimums: Vec<(u32, Size)> = ids.iter().map(|id| (*id, Size::new(900.0, 900.0))).collect();
    for layout in [
        LayoutType::Dwindle,
        LayoutType::Grid,
        LayoutType::Split,
        LayoutType::SplitVertical,
    ] {
        let r = run(layout, &ids, AREA, Gaps::uniform(2.0, 0.0), &[], &minimums);
        assert!(
            matches!(
                r.status,
                MinimumStatus::Unresolved { .. } | MinimumStatus::Infeasible(_)
            ),
            "{layout:?}: {:?}",
            r.status
        );
    }
}

#[test]
fn regular_grid_solves_each_axis_directly() {
    // 1000x800, 2x2: cells 500x400. Window 2 (column 1, row 0) needs 700 wide:
    // columns get 0 + 150 and 700 + 150; rows stay 400.
    let area = Rect::new(0.0, 0.0, 1000.0, 800.0);
    let r = run(LayoutType::Grid, &[1, 2, 3, 4], area, Gaps::ZERO, &[], &[(
        2,
        width(700.0),
    )]);
    assert_eq!(r.status, MinimumStatus::Adjusted);
    assert_rect(frame(&r.frames, 1), 0.0, 0.0, 150.0, 400.0);
    assert_rect(frame(&r.frames, 2), 150.0, 0.0, 850.0, 400.0);
    assert_rect(frame(&r.frames, 3), 0.0, 400.0, 150.0, 400.0);
    assert_rect(frame(&r.frames, 4), 150.0, 400.0, 850.0, 400.0);
    let ratios = r.ratios.unwrap();
    assert!((ratios[0] - 0.15).abs() < 1e-9 && (ratios[1] - 0.5).abs() < 1e-9);
}

#[test]
fn regular_grid_reports_impossible_columns() {
    let area = Rect::new(0.0, 0.0, 1000.0, 800.0);
    let r = run(LayoutType::Grid, &[1, 2, 3, 4], area, Gaps::ZERO, &[], &[
        (1, width(600.0)),
        (2, width(600.0)),
    ]);
    assert_eq!(
        r.status,
        MinimumStatus::Infeasible(InfeasibleReason::RequiredExceedsAvailable {
            required: 1200.0,
            available: 1000.0
        })
    );
    assert_rect(frame(&r.frames, 1), 0.0, 0.0, 500.0, 400.0);
}

#[test]
fn master_stack_grid_adjusts_the_master_share() {
    // 3 windows, 1200 wide: default master 600. Window 1 needs 800 -> +1/6.
    let r = run(LayoutType::Grid, &[1, 2, 3], AREA, Gaps::ZERO, &[], &[(
        1,
        width(800.0),
    )]);
    assert_eq!(r.status, MinimumStatus::Adjusted);
    assert_rect(frame(&r.frames, 1), 0.0, 0.0, 800.0, 600.0);
    assert_rect(frame(&r.frames, 2), 800.0, 0.0, 400.0, 300.0);
}

#[test]
fn ten_window_grid_cannot_be_ratio_solved_and_says_so() {
    let ids: Vec<u32> = (1..=10).collect();
    let r = run(LayoutType::Grid, &ids, AREA, Gaps::ZERO, &[], &[(
        2,
        width(5000.0),
    )]);
    assert_eq!(r.status, MinimumStatus::Unresolved { remaining: 1 });
    assert_eq!(r.frames.len(), 10);
    assert!(r.ratios.is_none());
    // Extra identities beyond the cap are never laid out here.
    let eleven: Vec<u32> = (1..=11).collect();
    assert_eq!(
        run(LayoutType::Grid, &eleven, AREA, Gaps::ZERO, &[], &[]).frames.len(),
        10
    );
}

#[test]
fn write_observations_keep_the_actual_frame() {
    let before = Rect::new(0.0, 0.0, 500.0, 500.0);
    let want = Rect::new(0.0, 0.0, 300.0, 500.0);
    assert_eq!(
        observe_write(&before, &want, &Rect::new(2.0, 1.0, 303.0, 500.0), None),
        FrameObservation::Honored
    );

    // Unchanged frame: the app refused the resize.
    let refused = observe_write(&before, &want, &before, Some(true));
    assert_eq!(refused, FrameObservation::Refused { actual: before });
    assert_eq!(refused.actual(), Some(before));

    // Moved part of the way.
    let halfway = Rect::new(0.0, 0.0, 400.0, 500.0);
    assert_eq!(
        observe_write(&before, &want, &halfway, Some(true)),
        FrameObservation::Partial { actual: halfway }
    );

    // A window that reports itself fixed size.
    let fixed = Rect::new(100.0, 100.0, 500.0, 500.0);
    assert_eq!(
        observe_write(&before, &want, &fixed, Some(false)),
        FrameObservation::FixedSize { actual: fixed }
    );
    assert_eq!(FrameObservation::Honored.actual(), None);

    // Garbage observations preserve the previous frame instead of poisoning state.
    let nan = Rect::new(f64::NAN, 0.0, 1.0, 1.0);
    assert_eq!(
        observe_write(&before, &want, &nan, None),
        FrameObservation::Refused { actual: before }
    );
}

#[test]
fn minimum_inference_matches_legacy_tolerance_and_precedence() {
    let expected = Rect::new(0.0, 0.0, 400.0, 300.0);
    // Within 5 points: no inference.
    assert_eq!(
        infer_minimum(&expected, &Rect::new(0.0, 0.0, 405.0, 305.0), None, None),
        None
    );
    // Wider only: width inferred, height keeps the earlier inference (or 0).
    let wide = Rect::new(0.0, 0.0, 500.0, 300.0);
    assert_eq!(
        infer_minimum(&expected, &wide, None, None),
        Some(Size::new(500.0, 0.0))
    );
    assert_eq!(
        infer_minimum(&expected, &wide, None, Some(Size::new(10.0, 250.0))),
        Some(Size::new(500.0, 250.0))
    );
    // A reported minimum makes inference unnecessary.
    assert_eq!(
        infer_minimum(&expected, &wide, Some(Size::new(1.0, 1.0)), None),
        None
    );
    assert_eq!(
        infer_minimum(&expected, &Rect::new(0.0, 0.0, f64::NAN, 1.0), None, None),
        None
    );
}

#[test]
fn fixed_size_window_feeds_back_into_a_feasible_layout() {
    // Wrote 600 wide, the window stayed 700: infer, then lay out around it.
    let before = Rect::new(0.0, 0.0, 700.0, 600.0);
    let want = Rect::new(0.0, 0.0, 600.0, 600.0);
    let observation = observe_write(&before, &want, &before, Some(false));
    let actual = observation.actual().unwrap();
    let inferred = infer_minimum(&want, &actual, None, None).unwrap();
    assert_eq!(inferred, Size::new(700.0, 0.0));
    let r = run(LayoutType::SplitHorizontal, &[1, 2], AREA, Gaps::ZERO, &[], &[(
        1, inferred,
    )]);
    assert_eq!(r.status, MinimumStatus::Adjusted);
    assert_rect(frame(&r.frames, 1), 0.0, 0.0, 700.0, 600.0);
    assert_rect(frame(&r.frames, 2), 700.0, 0.0, 500.0, 600.0);
}

#[test]
fn ratio_state_layout_change_and_balance_semantics() {
    let mut state = RatioState {
        split_ratios: vec![0.4],
        master_ratio: Some(0.7),
    };
    assert_eq!(
        state.set_layout(LayoutType::Dwindle, LayoutType::Dwindle),
        Change::Unchanged
    );
    assert_eq!(
        state.split_ratios,
        vec![0.4],
        "re-selecting the layout keeps ratios"
    );
    assert_eq!(
        state.set_layout(LayoutType::Dwindle, LayoutType::Master),
        Change::Changed
    );
    assert!(state.split_ratios.is_empty() && state.master_ratio.is_none());

    let mut empty = RatioState::default();
    assert_eq!(empty.balance(false), Change::Unchanged, "nothing to balance");
    assert_eq!(
        empty.balance(true),
        Change::Changed,
        "inferred minimums alone are balanceable"
    );
    let mut state = RatioState {
        split_ratios: vec![0.4],
        master_ratio: None,
    };
    assert_eq!(state.balance(false), Change::Changed);
    assert!(state.split_ratios.is_empty());
}

#[test]
fn stale_split_boundaries_are_ignored_after_membership_changes() {
    let state = RatioState {
        split_ratios: vec![0.33, 0.66],
        master_ratio: None,
    };
    assert_eq!(state.effective_split_ratios(LayoutType::SplitHorizontal, 3), &[
        0.33, 0.66
    ]);
    assert!(state.effective_split_ratios(LayoutType::SplitHorizontal, 4).is_empty());
    assert!(state.effective_split_ratios(LayoutType::Split, 2).is_empty());
    assert!(state.effective_split_ratios(LayoutType::SplitVertical, 1).is_empty());
    // Other layouts interpret ratios themselves and are passed through.
    assert_eq!(state.effective_split_ratios(LayoutType::Dwindle, 7), &[
        0.33, 0.66
    ]);
    assert_eq!(state.split_ratios.len(), 2, "stored value is not mutated");
}

#[test]
fn master_override_clamps_and_reports_no_ops() {
    let mut state = RatioState::default();
    assert_eq!(state.resize_master(0.6, 0.1), Change::Changed);
    assert!((state.master_ratio.unwrap() - 0.7).abs() < 1e-12);
    assert_eq!(state.resize_master(0.6, 0.5), Change::Changed);
    assert!((state.master_ratio.unwrap() - 0.9).abs() < 1e-12);
    assert_eq!(
        state.resize_master(0.6, 0.2),
        Change::Unchanged,
        "already at the bound"
    );
    assert_eq!(state.resize_master(0.6, 0.0), Change::Unchanged);
    assert_eq!(state.resize_master(0.6, f64::NAN), Change::Unchanged);
    let mut low = RatioState::default();
    assert_eq!(low.resize_master(0.6, -2.0), Change::Changed);
    assert!((low.master_ratio.unwrap() - 0.1).abs() < 1e-12);
}

#[test]
fn default_ratio_tables() {
    assert_eq!(default_ratios(LayoutType::Split, 4), vec![0.25, 0.5, 0.75]);
    assert_eq!(default_ratios(LayoutType::Dwindle, 3), vec![0.5, 0.5]);
    assert_eq!(default_ratios(LayoutType::Grid, 5), vec![1.0 / 3.0]);
    assert_eq!(default_ratios(LayoutType::Grid, 10), vec![0.25]);
    assert_eq!(default_ratios(LayoutType::Grid, 4), vec![0.5]);
    assert!(default_ratios(LayoutType::Master, 4).is_empty());
    assert!(default_ratios(LayoutType::Dwindle, 0).is_empty());
}

#[test]
fn cumulative_resize_respects_minimum_shares() {
    // Shares 0.33 / 0.33 / 0.34 with a 0.05 floor.
    let floor = [0.05, 0.05, 0.05];
    let mut grow_first = [0.33, 0.66];
    resize_cumulative(&mut grow_first, 0, 0.1, &floor);
    assert!((grow_first[0] - 0.43).abs() < 1e-9 && (grow_first[1] - 0.66).abs() < 1e-9);

    let mut grow_second = [0.33, 0.66];
    resize_cumulative(&mut grow_second, 0, -0.1, &floor);
    assert!((grow_second[0] - 0.23).abs() < 1e-9);

    // Window 1 is already at its 0.33 minimum: shrinking it is blocked.
    let mut blocked = [0.33, 0.66];
    resize_cumulative(&mut blocked, 0, 0.1, &[0.05, 0.33, 0.05]);
    assert!((blocked[0] - 0.33).abs() < 1e-9);

    // Out-of-range index and NaN do nothing.
    let mut same = [0.33, 0.66];
    resize_cumulative(&mut same, 5, 0.1, &floor);
    resize_cumulative(&mut same, 0, f64::NAN, &floor);
    assert_eq!(same, [0.33, 0.66]);
}

#[test]
fn dwindle_resize_is_blocked_when_it_would_violate_a_minimum() {
    let lookup = |id: &u32| (*id == 2).then(|| width(500.0));
    let request = |ratios: &'static [f64]| MinimumRequest {
        layout: LayoutType::Dwindle,
        ids: &[1, 2],
        usable: AREA,
        gaps: Gaps::ZERO,
        ratios,
        minimum_of: &lookup,
    };
    // 0.5 + 0.1 = 0.6 leaves window 2 with 480 < 499: blocked.
    assert_eq!(guarded_ratio_change(&request(&[0.5]), 0, 0.1), None);
    // 0.5 + 0.05 = 0.55 leaves 540: accepted.
    let ok = guarded_ratio_change(&request(&[0.5]), 0, 0.05).unwrap();
    assert!((ok[0] - 0.55).abs() < 1e-9);
    // No change (clamp at 0.9 already) and bad indices are None.
    assert_eq!(guarded_ratio_change(&request(&[0.9]), 0, 0.2), None);
    assert_eq!(guarded_ratio_change(&request(&[0.5]), 3, 0.1), None);
}

#[test]
fn redistribute_is_bounded_and_normalized() {
    let shares = redistribute(&[], &[0.6, 0.6, 0.6], 3);
    assert_eq!(shares.len(), 3);
    assert!((shares.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    assert!(redistribute(&[], &[], 0).is_empty());
    // Non-finite minimums are ignored.
    let shares = redistribute(&[], &[f64::NAN, f64::INFINITY], 2);
    assert!((shares[0] - 0.5).abs() < 1e-9);
}
