//! Grid shapes, the ten tile cap, overflow, promotion and window ordering.

use super::{assert_rect, frame};
use crate::{
    Exclusion, Gaps, GridOverflow, LayoutDiagnostic, LayoutPlan, LayoutRequest, LayoutType,
    MAX_GRID_TILES, MasterPosition, PlanStatus, Rect, UntouchedReason, WindowEntry, WindowFlags,
    insert_window, insertion_index, plan_layout, swap_windows,
};

const LANDSCAPE: Rect = Rect::new(0.0, 0.0, 1920.0, 1080.0);
const PORTRAIT: Rect = Rect::new(0.0, 0.0, 1080.0, 1920.0);

fn entries(count: u32) -> Vec<WindowEntry<u32>> { (1..=count).map(WindowEntry::new).collect() }

fn plan_for(
    layout: LayoutType,
    windows: &[WindowEntry<u32>],
    usable: Rect,
    gaps: Gaps,
    ratios: &[f64],
) -> LayoutPlan<u32> {
    plan_layout(&LayoutRequest {
        layout,
        windows,
        usable,
        gaps,
        master_ratio: 0.6,
        master_position: MasterPosition::Auto,
        ratios,
    })
}

fn grid(count: u32) -> LayoutPlan<u32> {
    plan_for(LayoutType::Grid, &entries(count), LANDSCAPE, Gaps::ZERO, &[])
}

fn flagged(id: u32, flags: WindowFlags) -> WindowEntry<u32> { WindowEntry::with_flags(id, flags) }

#[test]
fn landscape_shapes_one_to_ten() {
    // Legacy grid.rs shape table; every cell derived from the 1920x1080 area.
    let p = grid(1);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 1920.0, 1080.0);

    let p = grid(2);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 960.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 960.0, 0.0, 960.0, 1080.0);

    // 3: master column 960 wide, stack of two 540 tall cells.
    let p = grid(3);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 960.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 960.0, 0.0, 960.0, 540.0);
    assert_rect(frame(&p.frames, 3), 960.0, 540.0, 960.0, 540.0);

    // 4: 2x2.
    let p = grid(4);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 960.0, 540.0);
    assert_rect(frame(&p.frames, 2), 960.0, 0.0, 960.0, 540.0);
    assert_rect(frame(&p.frames, 3), 0.0, 540.0, 960.0, 540.0);
    assert_rect(frame(&p.frames, 4), 960.0, 540.0, 960.0, 540.0);

    // 5: master 1/3 wide, 2x2 stack of 640x540.
    let p = grid(5);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 640.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 640.0, 0.0, 640.0, 540.0);
    assert_rect(frame(&p.frames, 3), 1280.0, 0.0, 640.0, 540.0);
    assert_rect(frame(&p.frames, 4), 640.0, 540.0, 640.0, 540.0);
    assert_rect(frame(&p.frames, 5), 1280.0, 540.0, 640.0, 540.0);

    // 6: 2 rows x 3 columns.
    let p = grid(6);
    assert_rect(frame(&p.frames, 4), 0.0, 540.0, 640.0, 540.0);
    assert_rect(frame(&p.frames, 6), 1280.0, 540.0, 640.0, 540.0);

    // 7: master 640 wide, 3 rows x 2 columns of 640x360.
    let p = grid(7);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 640.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 640.0, 0.0, 640.0, 360.0);
    assert_rect(frame(&p.frames, 7), 1280.0, 720.0, 640.0, 360.0);

    // 8: 2 rows x 4 columns of 480x540.
    let p = grid(8);
    assert_rect(frame(&p.frames, 5), 0.0, 540.0, 480.0, 540.0);
    assert_rect(frame(&p.frames, 8), 1440.0, 540.0, 480.0, 540.0);

    // 9: 3x3 of 640x360.
    let p = grid(9);
    assert_rect(frame(&p.frames, 5), 640.0, 360.0, 640.0, 360.0);
    assert_rect(frame(&p.frames, 9), 1280.0, 720.0, 640.0, 360.0);

    // 10: first window spans the 480 wide left column; nine 480x360 cells.
    let p = grid(10);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 480.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 480.0, 0.0, 480.0, 360.0);
    assert_rect(frame(&p.frames, 5), 480.0, 360.0, 480.0, 360.0);
    assert_rect(frame(&p.frames, 10), 1440.0, 720.0, 480.0, 360.0);
}

#[test]
fn portrait_shapes() {
    let two = plan_for(LayoutType::Grid, &entries(2), PORTRAIT, Gaps::ZERO, &[]);
    assert_rect(frame(&two.frames, 1), 0.0, 0.0, 1080.0, 960.0);
    assert_rect(frame(&two.frames, 2), 0.0, 960.0, 1080.0, 960.0);

    // 5 windows: master row 1/3 of 1920 = 640 tall spanning both stack columns.
    let five = plan_for(LayoutType::Grid, &entries(5), PORTRAIT, Gaps::ZERO, &[]);
    assert_rect(frame(&five.frames, 1), 0.0, 0.0, 1080.0, 640.0);
    assert_rect(frame(&five.frames, 2), 0.0, 640.0, 540.0, 640.0);
    assert_rect(frame(&five.frames, 3), 540.0, 640.0, 540.0, 640.0);
    assert_rect(frame(&five.frames, 4), 0.0, 1280.0, 540.0, 640.0);
}

#[test]
fn grid_inner_gaps_and_custom_ratios() {
    // 2x2 with gap 10: cells (1920-10)/2 = 955 by (1080-10)/2 = 535.
    let p = plan_for(
        LayoutType::Grid,
        &entries(4),
        LANDSCAPE,
        Gaps::uniform(10.0, 0.0),
        &[],
    );
    assert_rect(frame(&p.frames, 2), 965.0, 0.0, 955.0, 535.0);
    assert_rect(frame(&p.frames, 4), 965.0, 545.0, 955.0, 535.0);
    // Master share override 0.3 on three windows.
    let p = plan_for(LayoutType::Grid, &entries(3), LANDSCAPE, Gaps::ZERO, &[0.3]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 576.0, 1080.0);
    assert_rect(frame(&p.frames, 2), 576.0, 0.0, 1344.0, 540.0);
    // Two windows: first ratio splits the pair.
    let p = plan_for(LayoutType::Grid, &entries(2), LANDSCAPE, Gaps::ZERO, &[0.25]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 480.0, 1080.0);
    // 2x2: column boundary 0.25 then row boundary 0.75.
    let p = plan_for(LayoutType::Grid, &entries(4), LANDSCAPE, Gaps::ZERO, &[
        0.25, 0.75,
    ]);
    assert_rect(frame(&p.frames, 1), 0.0, 0.0, 480.0, 810.0);
    assert_rect(frame(&p.frames, 4), 480.0, 810.0, 1440.0, 270.0);
}

#[test]
fn counts_zero_through_ten_tile_everyone_without_overflow() {
    for count in 0..=10_u32 {
        let p = grid(count);
        assert_eq!(p.frames.len(), count as usize);
        assert!(p.overflow.is_empty(), "n={count}");
        assert!(p.diagnostics.is_empty());
        assert_eq!(p.accounted(), count as usize);
        let ids: Vec<u32> = p.frames.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            (1..=count).collect::<Vec<_>>(),
            "layout order is workspace order"
        );
    }
}

#[test]
fn eleven_and_a_thousand_keep_every_identity() {
    let p = grid(11);
    assert_eq!(p.frames.len(), MAX_GRID_TILES);
    assert_eq!(p.overflow, vec![GridOverflow { id: 11, eligible_index: 10 }]);
    assert_eq!(p.diagnostics, vec![LayoutDiagnostic::GridCapacityExceeded {
        tiled: 10,
        overflow: 1
    }]);
    assert!(p.frame_of(&11).is_none(), "overflow never receives a frame");
    assert_eq!(p.accounted(), 11);
    // The first ten keep the exact ten-window geometry.
    let ten = grid(10);
    assert_eq!(p.frames, ten.frames);

    let p = grid(1000);
    assert_eq!(p.frames.len(), 10);
    assert_eq!(p.overflow.len(), 990);
    assert_eq!(p.overflow[0].id, 11);
    assert_eq!(p.overflow[989], GridOverflow { id: 1000, eligible_index: 999 });
    assert_eq!(p.accounted(), 1000);
}

#[test]
fn overflow_is_reported_even_when_geometry_is_infeasible() {
    let p = plan_for(
        LayoutType::Grid,
        &entries(12),
        Rect::new(0.0, 0.0, 0.0, 0.0),
        Gaps::ZERO,
        &[],
    );
    assert!(matches!(p.status, PlanStatus::Infeasible(_)));
    assert_eq!(p.overflow.len(), 2);
    assert_eq!(p.untouched.len(), 10);
    assert_eq!(p.accounted(), 12);
}

#[test]
fn eligibility_changes_promote_the_earliest_extra_and_back() {
    let all = entries(12);
    let base = plan_for(LayoutType::Grid, &all, LANDSCAPE, Gaps::ZERO, &[]);
    assert_eq!(base.overflow.iter().map(|o| o.id).collect::<Vec<_>>(), vec![
        11, 12
    ]);

    for (name, flags) in [
        ("minimized", WindowFlags {
            minimized: true,
            ..WindowFlags::default()
        }),
        ("hidden", WindowFlags {
            hidden: true,
            ..WindowFlags::default()
        }),
        ("inactive tab", WindowFlags {
            inactive_tab: true,
            ..WindowFlags::default()
        }),
        ("fullscreen", WindowFlags {
            fullscreen: true,
            ..WindowFlags::default()
        }),
    ] {
        let mut changed = all.clone();
        changed[2] = flagged(3, flags); // window 3 loses eligibility
        let p = plan_for(LayoutType::Grid, &changed, LANDSCAPE, Gaps::ZERO, &[]);
        let tiled: Vec<u32> = p.frames.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            tiled,
            vec![1, 2, 4, 5, 6, 7, 8, 9, 10, 11],
            "{name}: earliest extra (11) is promoted"
        );
        assert_eq!(
            p.overflow.iter().map(|o| o.id).collect::<Vec<_>>(),
            vec![12],
            "{name}"
        );
        assert_eq!(p.promoted_since(&base), vec![11], "{name}");
        assert_eq!(p.excluded.len(), 1);
        assert_eq!(p.accounted(), 12);
        // Regaining eligibility restores the original plan exactly: no stored state.
        let restored = plan_for(LayoutType::Grid, &all, LANDSCAPE, Gaps::ZERO, &[]);
        assert_eq!(restored, base, "{name}");
    }
}

#[test]
fn exclusion_reasons_are_reported() {
    let w = |f: WindowFlags| vec![flagged(1, f), WindowEntry::new(2)];
    let tab = plan_for(
        LayoutType::Dwindle,
        &w(WindowFlags {
            inactive_tab: true,
            ..WindowFlags::default()
        }),
        LANDSCAPE,
        Gaps::ZERO,
        &[],
    );
    assert_eq!(tab.excluded, vec![(1, Exclusion::InactiveTab)]);
    assert_rect(frame(&tab.frames, 2), 0.0, 0.0, 1920.0, 1080.0);
    for (flags, reason) in [
        (
            WindowFlags {
                fullscreen: true,
                ..WindowFlags::default()
            },
            Exclusion::Fullscreen,
        ),
        (
            WindowFlags {
                minimized: true,
                ..WindowFlags::default()
            },
            Exclusion::Minimized,
        ),
        (
            WindowFlags {
                hidden: true,
                ..WindowFlags::default()
            },
            Exclusion::Hidden,
        ),
    ] {
        let p = plan_for(LayoutType::Dwindle, &w(flags), LANDSCAPE, Gaps::ZERO, &[]);
        assert_eq!(p.excluded, vec![(1, reason)]);
    }
}

#[test]
fn explicit_floating_never_counts_toward_the_cap_and_is_never_overwritten() {
    let mut windows = entries(12);
    windows[11] = flagged(12, WindowFlags {
        explicit_floating: true,
        ..WindowFlags::default()
    });
    let p = plan_for(LayoutType::Grid, &windows, LANDSCAPE, Gaps::ZERO, &[]);
    // 11 eligible: ten tile, one overflows; the floating window is untouched.
    assert_eq!(p.overflow.iter().map(|o| o.id).collect::<Vec<_>>(), vec![11]);
    assert_eq!(p.untouched, vec![(12, UntouchedReason::ExplicitFloating)]);

    // Floating an early tile promotes the extra and the flag is preserved.
    let mut windows = entries(11);
    windows[1] = flagged(2, WindowFlags {
        explicit_floating: true,
        ..WindowFlags::default()
    });
    let p = plan_for(LayoutType::Grid, &windows, LANDSCAPE, Gaps::ZERO, &[]);
    assert!(p.overflow.is_empty());
    assert_eq!(p.frames.len(), 10);
    assert_eq!(p.untouched, vec![(2, UntouchedReason::ExplicitFloating)]);
    assert!(p.frame_of(&11).is_some());
}

#[test]
fn leaving_grid_removes_only_the_overflow_classification() {
    let mut windows = entries(14);
    windows[4] = flagged(5, WindowFlags {
        explicit_floating: true,
        ..WindowFlags::default()
    });
    let grid_plan = plan_for(LayoutType::Grid, &windows, LANDSCAPE, Gaps::ZERO, &[]);
    assert_eq!(grid_plan.overflow.len(), 3);
    let dwindle = plan_for(LayoutType::Dwindle, &windows, LANDSCAPE, Gaps::ZERO, &[]);
    assert!(dwindle.overflow.is_empty(), "no ten cap outside Grid");
    assert_eq!(dwindle.frames.len(), 13);
    assert_eq!(dwindle.untouched, vec![(5, UntouchedReason::ExplicitFloating)]);
    for layout in [
        LayoutType::Master,
        LayoutType::Monocle,
        LayoutType::SplitHorizontal,
    ] {
        let p = plan_for(layout, &windows, LANDSCAPE, Gaps::ZERO, &[]);
        assert_eq!(p.frames.len(), 13, "{layout:?}");
        assert!(p.overflow.is_empty());
    }
}

#[test]
fn floating_layout_round_trip_preserves_flags_and_order() {
    let mut windows = entries(12);
    windows[0] = flagged(1, WindowFlags {
        explicit_floating: true,
        ..WindowFlags::default()
    });
    let before = plan_for(LayoutType::Grid, &windows, LANDSCAPE, Gaps::ZERO, &[]);
    let floating = plan_for(LayoutType::Floating, &windows, LANDSCAPE, Gaps::ZERO, &[]);
    assert!(floating.frames.is_empty() && floating.overflow.is_empty());
    assert_eq!(floating.accounted(), 12);
    // The explicit flag stays an explicit flag; the rest merely sit in a floating layout.
    assert_eq!(floating.untouched[0], (1, UntouchedReason::ExplicitFloating));
    assert!(
        floating.untouched[1..]
            .iter()
            .all(|(_, r)| *r == UntouchedReason::FloatingLayout)
    );
    let after = plan_for(LayoutType::Grid, &windows, LANDSCAPE, Gaps::ZERO, &[]);
    assert_eq!(
        before, after,
        "returning restores eligibility with identical ordering"
    );
}

#[test]
fn removal_promotes_earliest_extra_and_swap_is_explicit_only() {
    let mut order: Vec<u32> = (1..=12).collect();
    let plan_of = |order: &[u32]| {
        let w: Vec<_> = order.iter().copied().map(WindowEntry::new).collect();
        plan_for(LayoutType::Grid, &w, LANDSCAPE, Gaps::ZERO, &[])
    };
    let before = plan_of(&order);
    assert_eq!(before.overflow.iter().map(|o| o.id).collect::<Vec<_>>(), vec![
        11, 12
    ]);

    // Removing a tiled window promotes 11, not 12.
    let mut removed = order.clone();
    removed.retain(|id| *id != 4);
    let after = plan_of(&removed);
    assert_eq!(after.promoted_since(&before), vec![11]);
    assert_eq!(after.overflow.iter().map(|o| o.id).collect::<Vec<_>>(), vec![12]);

    // Removing an overflow window changes no tile.
    let mut removed = order.clone();
    removed.retain(|id| *id != 11);
    assert_eq!(plan_of(&removed).frames, before.frames);

    // Explicit swap of an overflow window with a tile exchanges roles.
    assert!(swap_windows(&mut order, &1, &12));
    let swapped = plan_of(&order);
    assert!(swapped.frame_of(&12).is_some());
    assert!(swapped.is_overflow(&1));
    assert_eq!(frame(&swapped.frames, 12), frame(&before.frames, 1));
    assert!(!swap_windows(&mut order, &1, &1));
    assert!(!swap_windows(&mut order, &1, &99));
}

#[test]
fn focus_cannot_change_a_grid_plan() {
    // The planning API takes no focus input, so focus cannot displace a tile.
    // New-window placement is the only focus-aware step and Grid ignores it.
    let order: Vec<u32> = (1..=10).collect();
    for focused in [None, Some(&1), Some(&5), Some(&10), Some(&99)] {
        assert_eq!(
            insertion_index(&order, focused, LayoutType::Grid),
            10,
            "{focused:?}"
        );
    }
}

#[test]
fn non_grid_inserts_after_focus_else_appends_and_grid_always_appends() {
    // Legacy window.rs:147-164.
    let base: Vec<u32> = vec![1, 2, 3, 4];
    for layout in [
        LayoutType::Dwindle,
        LayoutType::Split,
        LayoutType::SplitVertical,
        LayoutType::SplitHorizontal,
        LayoutType::Monocle,
        LayoutType::Master,
        LayoutType::Floating,
    ] {
        let mut order = base.clone();
        assert_eq!(
            insert_window(&mut order, 9, Some(&2), layout),
            Some(2),
            "{layout:?}"
        );
        assert_eq!(order, vec![1, 2, 9, 3, 4]);
        let mut order = base.clone();
        assert_eq!(insert_window(&mut order, 9, None, layout), Some(4));
        assert_eq!(order, vec![1, 2, 3, 4, 9]);
        // A focused window from another workspace is not in `order`: append.
        let mut order = base.clone();
        insert_window(&mut order, 9, Some(&77), layout);
        assert_eq!(order, vec![1, 2, 3, 4, 9]);
        // Focus on the last window appends naturally.
        let mut order = base.clone();
        insert_window(&mut order, 9, Some(&4), layout);
        assert_eq!(order, vec![1, 2, 3, 4, 9]);
    }
    let mut order = base;
    assert_eq!(insert_window(&mut order, 9, Some(&2), LayoutType::Grid), Some(4));
    assert_eq!(order, vec![1, 2, 3, 4, 9]);
    // Already tracked: no change.
    assert_eq!(insert_window(&mut order, 3, Some(&1), LayoutType::Dwindle), None);
    assert_eq!(order, vec![1, 2, 3, 4, 9]);
}

#[test]
fn grid_eleventh_window_leaves_the_first_ten_unchanged_unlike_dwindle() {
    let mut grid_order: Vec<u32> = (1..=10).collect();
    insert_window(&mut grid_order, 11, Some(&3), LayoutType::Grid);
    let mut dwindle_order: Vec<u32> = (1..=10).collect();
    insert_window(&mut dwindle_order, 11, Some(&3), LayoutType::Dwindle);

    let ids = |order: &[u32]| order.iter().copied().map(WindowEntry::new).collect::<Vec<_>>();
    let before = grid(10);
    let after = plan_for(LayoutType::Grid, &ids(&grid_order), LANDSCAPE, Gaps::ZERO, &[]);
    assert_eq!(after.frames, before.frames, "first ten tiles untouched");
    assert!(after.is_overflow(&11));
    // Inserting after focus would have shifted window 4.. and displaced window 10.
    let displaced = plan_for(LayoutType::Grid, &ids(&dwindle_order), LANDSCAPE, Gaps::ZERO, &[
    ]);
    assert!(
        displaced.is_overflow(&10),
        "this is the displacement Grid avoids"
    );
}
