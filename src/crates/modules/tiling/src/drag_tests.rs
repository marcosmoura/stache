//! TI-08-02/03 (pure): the drag thresholds, selection, containment and ratio
//! fixtures, and the bounded drag lane. No threads, input service or windows.

use crate::config::LayoutType;
use crate::drag::{
    DRAG_QUEUE_CAPACITY, DragPoint, DragSignal, LostCause, MIN_DRAG_DISTANCE, RatioMove,
    ResizeContext, center, contains_inclusive, ratio_move, resized_window, select_dragged,
    select_target,
};
use crate::events::Pipeline;
use crate::geometry::Rect;
use crate::intent::ResizeDimension;
use crate::state::Input;

type Frames = Vec<(u32, Rect)>;

fn moved(by: (f64, f64), rect: Rect) -> Rect {
    Rect::new(rect.x + by.0, rect.y + by.1, rect.width, rect.height)
}

fn pair() -> Frames {
    vec![
        (1, Rect::new(0.0, 0.0, 100.0, 100.0)),
        (2, Rect::new(200.0, 0.0, 100.0, 100.0)),
    ]
}

fn at(x: f64, y: f64) -> DragPoint { DragPoint { x, y } }

// ------------------------------------------------------------ dragged window

#[test]
fn the_threshold_is_strictly_above_fifty_logical_points() {
    assert!((MIN_DRAG_DISTANCE - 50.0).abs() < f64::EPSILON);
    for (distance, expected) in [(49.0, None), (50.0, None), (51.0, Some(1))] {
        let originals = pair();
        let currents: Frames = vec![
            (1, moved((distance, 0.0), originals[0].1)),
            (2, originals[1].1),
        ];
        let picked = select_dragged(&originals, &currents).map(|(key, _)| key);
        assert_eq!(picked, expected, "{distance}px");
    }
}

#[test]
fn the_distance_is_the_euclidean_centre_travel_not_an_axis() {
    // 30 and 40 make exactly 50: no drag. 31 and 40 make more than 50.
    let originals = pair();
    let at_fifty: Frames = vec![
        (1, moved((30.0, 40.0), originals[0].1)),
        (2, originals[1].1),
    ];
    assert!(select_dragged(&originals, &at_fifty).is_none());
    let beyond: Frames = vec![
        (1, moved((31.0, 40.0), originals[0].1)),
        (2, originals[1].1),
    ];
    assert_eq!(select_dragged(&originals, &beyond).map(|(key, _)| key), Some(1));
}

#[test]
fn a_resize_alone_moves_the_centre_by_half_the_change() {
    // The right edge moves by 90: the centre moves by 45, below the threshold.
    let originals = pair();
    let grown: Frames = vec![(1, Rect::new(0.0, 0.0, 190.0, 100.0)), (2, originals[1].1)];
    assert!(select_dragged(&originals, &grown).is_none());
}

#[test]
fn when_two_windows_both_moved_beyond_fifty_the_greatest_travel_wins() {
    let originals: Frames = vec![
        (1, Rect::new(0.0, 0.0, 100.0, 100.0)),
        (2, Rect::new(200.0, 0.0, 100.0, 100.0)),
        (3, Rect::new(400.0, 0.0, 100.0, 100.0)),
    ];
    let currents: Frames = vec![
        (1, moved((80.0, 0.0), originals[0].1)),
        (2, moved((0.0, 300.0), originals[1].1)),
        (3, moved((60.0, 0.0), originals[2].1)),
    ];
    assert_eq!(
        select_dragged(&originals, &currents).map(|(key, _)| key),
        Some(2)
    );
    // Reversed order of travel picks the other one: order does not decide.
    let currents: Frames = vec![
        (1, moved((300.0, 0.0), originals[0].1)),
        (2, moved((0.0, 80.0), originals[1].1)),
        (3, originals[2].1),
    ];
    assert_eq!(
        select_dragged(&originals, &currents).map(|(key, _)| key),
        Some(1)
    );
}

#[test]
fn an_equal_travel_keeps_the_earlier_window() {
    let originals = pair();
    let currents: Frames = vec![
        (1, moved((0.0, 70.0), originals[0].1)),
        (2, moved((0.0, 70.0), originals[1].1)),
    ];
    assert_eq!(
        select_dragged(&originals, &currents).map(|(key, _)| key),
        Some(1)
    );
}

#[test]
fn a_window_without_a_current_frame_is_never_selected() {
    let originals = pair();
    let currents: Frames = vec![(2, moved((0.0, 500.0), originals[1].1))];
    assert_eq!(
        select_dragged(&originals, &currents).map(|(key, _)| key),
        Some(2)
    );
    assert!(select_dragged(&originals, &[]).is_none());
}

// ------------------------------------------------------------------- target

#[test]
fn containment_is_inclusive_on_every_edge() {
    let target = Rect::new(100.0, 100.0, 200.0, 100.0);
    for (x, y, inside) in [
        (100.0, 100.0, true),
        (300.0, 200.0, true),
        (100.0, 200.0, true),
        (300.0, 100.0, true),
        (99.999, 150.0, false),
        (300.001, 150.0, false),
        (200.0, 99.999, false),
        (200.0, 200.001, false),
    ] {
        assert_eq!(contains_inclusive(&target, at(x, y)), inside, "({x}, {y})");
    }
    let originals: Frames = vec![(1, Rect::new(0.0, 0.0, 50.0, 50.0)), (2, target)];
    // The dropped centre exactly on the target's edge counts.
    assert_eq!(select_target(&originals, 1, at(300.0, 150.0)), Some(2));
    assert_eq!(select_target(&originals, 1, at(300.001, 150.0)), None);
}

#[test]
fn the_first_original_in_stable_order_wins_when_originals_overlap() {
    let originals: Frames = vec![
        (1, Rect::new(0.0, 0.0, 100.0, 100.0)),
        (2, Rect::new(50.0, 0.0, 200.0, 100.0)),
        (3, Rect::new(100.0, 0.0, 200.0, 100.0)),
    ];
    // (120, 50) is inside 2 and 3; 2 comes first.
    assert_eq!(select_target(&originals, 1, at(120.0, 50.0)), Some(2));
    // Dragging 2 ignores 2 itself: 1 contains x=60 and comes first.
    assert_eq!(select_target(&originals, 2, at(60.0, 50.0)), Some(1));
    assert_eq!(select_target(&originals, 2, at(120.0, 50.0)), Some(3));
}

#[test]
fn the_dragged_window_is_never_its_own_target_and_no_target_is_none() {
    let originals = pair();
    assert_eq!(select_target(&originals, 1, center(&originals[0].1)), None);
    assert_eq!(select_target(&originals, 1, at(1000.0, 1000.0)), None);
}

// ------------------------------------------------------------------- resize

#[test]
fn a_resize_needs_more_than_five_points_of_combined_change() {
    let originals = pair();
    let near_five: Frames = vec![(1, Rect::new(0.0, 0.0, 103.0, 102.0)), (2, originals[1].1)];
    assert!(resized_window(&originals, &near_five).is_none(), "exactly five");
    let beyond: Frames = vec![(1, Rect::new(0.0, 0.0, 103.0, 102.5)), (2, originals[1].1)];
    let (key, old, new) = resized_window(&originals, &beyond).expect("resized");
    assert_eq!(key, 1);
    assert_eq!(old, originals[0].1);
    assert_eq!(new, beyond[0].1);
}

#[test]
fn the_window_with_the_greatest_size_change_is_the_resized_one() {
    let originals = pair();
    let currents: Frames = vec![
        (1, Rect::new(0.0, 0.0, 120.0, 100.0)),
        (2, Rect::new(200.0, 0.0, 100.0, 160.0)),
    ];
    assert_eq!(
        resized_window(&originals, &currents).map(|(key, ..)| key),
        Some(2)
    );
}

fn context(layout: LayoutType, position: usize, count: usize) -> ResizeContext {
    ResizeContext {
        layout,
        position,
        count,
        width: 1000.0,
        height: 500.0,
        master_uses_width: true,
    }
}

const OLD: Rect = Rect::new(0.0, 0.0, 400.0, 300.0);

#[test]
fn split_layouts_move_the_boundary_after_the_window_and_flip_for_the_last() {
    let wider = Rect::new(0.0, 0.0, 500.0, 300.0);
    for layout in [
        LayoutType::Split,
        LayoutType::SplitHorizontal,
        LayoutType::SplitVertical,
    ] {
        assert_eq!(
            ratio_move(&context(layout, 0, 3), &OLD, &wider),
            RatioMove::Split { index: 0, delta: 0.1 },
            "{layout:?}"
        );
        assert_eq!(
            ratio_move(&context(layout, 1, 3), &OLD, &wider),
            RatioMove::Split { index: 1, delta: 0.1 }
        );
        // The last window owns no boundary: the previous one moves, reversed.
        assert_eq!(
            ratio_move(&context(layout, 2, 3), &OLD, &wider),
            RatioMove::Split { index: 1, delta: -0.1 }
        );
    }
}

#[test]
fn the_dominant_dimension_picks_the_extent_that_scales_the_delta() {
    let taller = Rect::new(0.0, 0.0, 410.0, 400.0);
    // Height changed more: 100 over the 500 high extent, not 10 over the width.
    assert_eq!(
        ratio_move(&context(LayoutType::Split, 0, 2), &OLD, &taller),
        RatioMove::Split { index: 0, delta: 0.2 }
    );
}

#[test]
fn dwindle_moves_the_window_before_its_own_ratio_and_clamps_the_last() {
    let wider = Rect::new(0.0, 0.0, 450.0, 300.0);
    assert_eq!(
        ratio_move(&context(LayoutType::Dwindle, 0, 4), &OLD, &wider),
        RatioMove::Split { index: 0, delta: 0.05 }
    );
    assert_eq!(
        ratio_move(&context(LayoutType::Dwindle, 1, 4), &OLD, &wider),
        RatioMove::Split { index: 0, delta: 0.05 }
    );
    assert_eq!(
        ratio_move(&context(LayoutType::Dwindle, 3, 4), &OLD, &wider),
        RatioMove::Split { index: 2, delta: 0.05 }
    );
}

#[test]
fn grid_follows_width_only_and_replans_for_height() {
    let wider = Rect::new(0.0, 0.0, 500.0, 300.0);
    assert_eq!(
        ratio_move(&context(LayoutType::Grid, 2, 4), &OLD, &wider),
        RatioMove::Split { index: 0, delta: 0.1 }
    );
    let taller = Rect::new(0.0, 0.0, 400.0, 400.0);
    assert_eq!(
        ratio_move(&context(LayoutType::Grid, 2, 4), &OLD, &taller),
        RatioMove::Relayout
    );
}

#[test]
fn master_uses_the_master_orientation_and_flips_in_the_caller_for_stack() {
    let wider = Rect::new(0.0, 0.0, 500.0, 300.0);
    assert_eq!(
        ratio_move(&context(LayoutType::Master, 0, 3), &OLD, &wider),
        RatioMove::Master {
            dimension: ResizeDimension::Width,
            delta: 0.1
        }
    );
    // Top or bottom master: the height drives the boundary even if the width
    // changed more.
    let mut vertical = context(LayoutType::Master, 1, 3);
    vertical.master_uses_width = false;
    let both = Rect::new(0.0, 0.0, 700.0, 350.0);
    assert_eq!(ratio_move(&vertical, &OLD, &both), RatioMove::Master {
        dimension: ResizeDimension::Height,
        delta: 0.1
    });
}

#[test]
fn layouts_without_ratios_or_with_one_tile_change_nothing() {
    let wider = Rect::new(0.0, 0.0, 500.0, 300.0);
    assert_eq!(
        ratio_move(&context(LayoutType::Monocle, 0, 3), &OLD, &wider),
        RatioMove::Relayout
    );
    assert_eq!(
        ratio_move(&context(LayoutType::Floating, 0, 3), &OLD, &wider),
        RatioMove::Relayout
    );
    assert_eq!(
        ratio_move(&context(LayoutType::Dwindle, 0, 1), &OLD, &wider),
        RatioMove::Nothing
    );
    let mut empty = context(LayoutType::Split, 0, 3);
    empty.width = 0.0;
    assert_eq!(ratio_move(&empty, &OLD, &wider), RatioMove::Relayout);
}

// --------------------------------------------------------------------- lane

fn down(button: u32) -> Input {
    Input::Drag(DragSignal::Down {
        button,
        at: at(1.0, 2.0),
        epoch: 1,
        time: std::time::Duration::ZERO,
    })
}

#[test]
fn the_drag_lane_is_bounded_and_reports_one_lost_observation_when_full() {
    let mut pipeline = Pipeline::new();
    for _ in 0..DRAG_QUEUE_CAPACITY {
        pipeline.admit_input(down(0));
    }
    assert_eq!(pipeline.stats().drag_accepted, DRAG_QUEUE_CAPACITY as u64);
    assert_eq!(pipeline.stats().drag_dropped, 0);
    // One more cannot be admitted: everything queued is void.
    pipeline.admit_input(down(0));
    pipeline.admit_input(down(0));
    let stats = pipeline.stats();
    assert!(stats.drag_dropped >= 1);
    let taken = pipeline.take(1000);
    let drags: Vec<&Input> = taken.iter().filter(|input| matches!(input, Input::Drag(_))).collect();
    // The queued signals were discarded; the actor sees the loss first.
    assert_eq!(drags[0], &Input::Drag(DragSignal::Lost(LostCause::Overflow)));
    assert!(drags.len() <= 2, "queue was cleared: {}", drags.len());
    assert!(!pipeline.has_work());
}

#[test]
fn drag_signals_keep_their_order_and_wake_the_actor() {
    let mut pipeline = Pipeline::new();
    assert!(!pipeline.has_work());
    pipeline.admit_input(down(0));
    pipeline.admit_input(down(1));
    assert!(pipeline.has_work());
    let taken = pipeline.take(1000);
    assert_eq!(taken, vec![down(0), down(1)]);
}
