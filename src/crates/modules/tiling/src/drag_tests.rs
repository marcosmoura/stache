//! TI-08-02/03 (pure): the drag thresholds, selection, containment and
//! edge-aware resize fixtures, and the bounded drag lane. No threads, input service or windows.

use crate::config::{LayoutType, MasterPosition};
use crate::drag::{
    DRAG_QUEUE_CAPACITY, DragPoint, DragSignal, Edge, EdgeResize, LostCause, MIN_DRAG_DISTANCE,
    ResizeRequest, center, contains_inclusive, edge_resize, moved_edges, resized_window,
    select_dragged, select_target,
};
use crate::events::Pipeline;
use crate::geometry::Rect;
use crate::layout::{Gaps, LayoutRequest, WindowEntry, plan_layout};
use crate::minimum_size::Size;
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

// ------------------------------------------------------------ edge resize

const AREA: Rect = Rect::new(0.0, 0.0, 1000.0, 500.0);

struct Resized {
    outcome: EdgeResize,
    before: Vec<Rect>,
    after: Vec<Rect>,
}

fn plan_frames(
    layout: LayoutType,
    count: usize,
    area: Rect,
    gaps: Gaps,
    (ratios, master): (&[f64], f64),
) -> Vec<Rect> {
    let entries: Vec<WindowEntry<usize>> = (0..count).map(WindowEntry::new).collect();
    plan_layout(&LayoutRequest {
        layout,
        windows: &entries,
        usable: area,
        gaps,
        master_ratio: master,
        master_position: MasterPosition::Left,
        ratios,
    })
    .frames
    .into_iter()
    .map(|(_, frame)| frame)
    .collect()
}

/// Plans `count` tiles, resizes tile `position` with `edit`, solves and
/// replans with the outcome.
fn resize_in(
    layout: LayoutType,
    (count, position): (usize, usize),
    (area, gaps): (Rect, Gaps),
    minimums: &[Option<Size>],
    edit: impl Fn(Rect) -> Rect,
) -> Resized {
    let before = plan_frames(layout, count, area, gaps, (&[], 0.6));
    let old = before[position];
    let mut all = minimums.to_vec();
    all.resize(count, None);
    let outcome = edge_resize(
        &ResizeRequest {
            layout,
            count,
            position,
            usable: area,
            gaps,
            master_position: MasterPosition::Left,
            master_ratio: 0.6,
            ratios: &[],
            minimums: &all,
        },
        &old,
        &edit(old),
    );
    let after = match &outcome {
        EdgeResize::Moved { ratios, master_ratio } => {
            plan_frames(layout, count, area, gaps, (ratios, *master_ratio))
        }
        _ => before.clone(),
    };
    Resized { outcome, before, after }
}

fn resize(
    layout: LayoutType,
    count: usize,
    position: usize,
    edit: impl Fn(Rect) -> Rect,
) -> Resized {
    resize_in(layout, (count, position), (AREA, Gaps::ZERO), &[], edit)
}

fn near(a: f64, b: f64) -> bool { (a - b).abs() < 0.01 }

fn same_rect(a: &Rect, b: &Rect) -> bool {
    near(a.x, b.x) && near(a.y, b.y) && near(a.width, b.width) && near(a.height, b.height)
}

/// Which tiles changed frame.
fn changed(r: &Resized) -> Vec<usize> {
    (0..r.before.len())
        .filter(|i| !same_rect(&r.before[*i], &r.after[*i]))
        .collect()
}

fn left_to(x: f64) -> impl Fn(Rect) -> Rect {
    move |r| Rect::new(x, r.y, r.x + r.width - x, r.height)
}
fn right_to(x: f64) -> impl Fn(Rect) -> Rect { move |r| Rect::new(r.x, r.y, x - r.x, r.height) }
fn top_to(y: f64) -> impl Fn(Rect) -> Rect {
    move |r| Rect::new(r.x, y, r.width, r.y + r.height - y)
}
fn bottom_to(y: f64) -> impl Fn(Rect) -> Rect { move |r| Rect::new(r.x, r.y, r.width, y - r.y) }

#[test]
fn moved_edges_need_more_than_the_resize_threshold() {
    let old = Rect::new(100.0, 100.0, 400.0, 300.0);
    assert_eq!(moved_edges(&old, &Rect::new(95.0, 100.0, 412.0, 300.0)), vec![
        Edge::Right
    ]);
    assert_eq!(moved_edges(&old, &Rect::new(50.0, 40.0, 500.0, 400.0)), vec![
        Edge::Left,
        Edge::Right,
        Edge::Top,
        Edge::Bottom
    ]);
    assert!(moved_edges(&old, &old).is_empty());
}

#[test]
fn split_left_and_right_edges_move_only_the_two_tiles_beside_them() {
    let left = resize(LayoutType::SplitHorizontal, 3, 1, left_to(250.0));
    assert_eq!(changed(&left), vec![0, 1]);
    assert!(near(left.after[0].width, 250.0) && near(left.after[1].x, 250.0));
    assert!(near(left.after[1].x + left.after[1].width, 2000.0 / 3.0));

    let right = resize(LayoutType::SplitHorizontal, 3, 1, right_to(800.0));
    assert_eq!(changed(&right), vec![1, 2]);
    assert!(near(right.after[1].x + right.after[1].width, 800.0));

    let both = resize(LayoutType::SplitHorizontal, 3, 1, |r| {
        Rect::new(200.0, r.y, 600.0, r.height)
    });
    assert_eq!(changed(&both), vec![0, 1, 2]);
    assert!(near(both.after[1].x, 200.0) && near(both.after[1].width, 600.0));
}

#[test]
fn split_vertical_top_and_bottom_edges_move_their_boundary() {
    let bottom = resize(LayoutType::SplitVertical, 3, 0, bottom_to(250.0));
    assert_eq!(changed(&bottom), vec![0, 1]);
    assert!(near(bottom.after[0].height, 250.0));
    let top = resize(LayoutType::SplitVertical, 3, 2, top_to(300.0));
    assert_eq!(changed(&top), vec![1, 2]);
    assert!(near(top.after[2].y, 300.0));
    // Split follows the orientation: landscape divides width.
    let split = resize(LayoutType::Split, 2, 0, right_to(700.0));
    assert!(near(split.after[0].width, 700.0));
}

#[test]
fn outer_edges_and_edges_across_the_split_axis_have_no_boundary() {
    let outer = resize(LayoutType::SplitHorizontal, 3, 0, left_to(-100.0));
    assert_eq!(outer.outcome, EdgeResize::NoBoundary);
    let last = resize(LayoutType::SplitHorizontal, 3, 2, right_to(1100.0));
    assert_eq!(last.outcome, EdgeResize::NoBoundary);
    let across = resize(LayoutType::SplitHorizontal, 3, 1, bottom_to(300.0));
    assert_eq!(across.outcome, EdgeResize::NoBoundary);
    // An outer edge moved together with a boundary edge is ignored.
    let mixed = resize(LayoutType::SplitHorizontal, 3, 0, |r| {
        Rect::new(-50.0, r.y, 450.0, r.height)
    });
    assert!(near(mixed.after[0].x, 0.0) && near(mixed.after[0].width, 400.0));
}

#[test]
fn a_boundary_stops_at_a_neighbours_minimum_and_at_the_ratio_floor() {
    let minimum = resize_in(
        LayoutType::SplitHorizontal,
        (3, 1),
        (AREA, Gaps::ZERO),
        &[None, None, Some(Size::new(300.0, 0.0))],
        right_to(950.0),
    );
    assert!(
        (minimum.after[2].width - 300.0).abs() <= 1.0,
        "{:?}",
        minimum.after
    );
    let floor = resize(LayoutType::SplitHorizontal, 3, 1, right_to(1000.0));
    assert!(near(floor.after[2].width, 50.0), "{:?}", floor.after);
    // Already at the minimum: the boundary moves no further than the
    // violation tolerance.
    let blocked = resize_in(
        LayoutType::SplitHorizontal,
        (2, 0),
        (AREA, Gaps::ZERO),
        &[None, Some(Size::new(500.0, 0.0))],
        right_to(700.0),
    );
    assert!(blocked.after[1].width >= 499.0, "{:?}", blocked.after);
}

#[test]
fn dwindle_moves_the_split_whose_boundary_is_the_edge_even_an_ancestor() {
    // Landscape, four tiles: split 0 | (split 1 / (split 2 |)).
    let own = resize(LayoutType::Dwindle, 4, 0, right_to(600.0));
    let EdgeResize::Moved { ratios, .. } = &own.outcome else {
        panic!("{:?}", own.outcome)
    };
    assert!(near(ratios[0], 0.6) && near(ratios[1], 0.5) && near(ratios[2], 0.5));

    // Tile 3's top edge belongs to split 1, an ancestor.
    let ancestor = resize(LayoutType::Dwindle, 4, 3, top_to(300.0));
    let EdgeResize::Moved { ratios, .. } = &ancestor.outcome else {
        panic!()
    };
    assert!(
        near(ratios[0], 0.5) && near(ratios[1], 0.6) && near(ratios[2], 0.5),
        "{ratios:?}"
    );
    assert!(near(ancestor.after[3].y, 300.0) && near(ancestor.after[2].y, 300.0));
    assert!(same_rect(&ancestor.after[0], &ancestor.before[0]));

    // Tile 2's left edge is split 0's boundary.
    let left = resize(LayoutType::Dwindle, 4, 2, left_to(400.0));
    assert!(near(left.after[2].x, 400.0) && near(left.after[0].width, 400.0));
    // Its bottom edge is the area's border.
    let outer = resize(LayoutType::Dwindle, 4, 2, bottom_to(450.0));
    assert_eq!(outer.outcome, EdgeResize::NoBoundary);
}

#[test]
fn master_moves_the_share_on_its_boundary_and_stack_weights_between_stack_tiles() {
    // Master left, 0.6: master 600 wide, three stack tiles 166.67 tall.
    let share = resize(LayoutType::Master, 4, 0, right_to(500.0));
    let EdgeResize::Moved { master_ratio, .. } = share.outcome else {
        panic!()
    };
    assert!(near(master_ratio, 0.5));
    // A stack tile's left edge is the same boundary.
    let stack_left = resize(LayoutType::Master, 4, 2, left_to(700.0));
    let EdgeResize::Moved { master_ratio, .. } = stack_left.outcome else {
        panic!()
    };
    assert!(near(master_ratio, 0.7));

    let bottom = resize(LayoutType::Master, 4, 1, bottom_to(250.0));
    let EdgeResize::Moved { master_ratio, .. } = bottom.outcome.clone() else {
        panic!()
    };
    assert!(near(master_ratio, 0.6));
    assert_eq!(changed(&bottom), vec![1, 2]);
    assert!(near(bottom.after[1].height, 250.0) && near(bottom.after[2].y, 250.0));

    let top = resize(LayoutType::Master, 4, 3, top_to(400.0));
    assert_eq!(changed(&top), vec![2, 3]);
    let both = resize(LayoutType::Master, 4, 2, |r| {
        Rect::new(r.x, 100.0, r.width, 300.0)
    });
    assert_eq!(changed(&both), vec![1, 2, 3]);
    assert!(near(both.after[2].y, 100.0) && near(both.after[2].height, 300.0));

    assert_eq!(
        resize(LayoutType::Master, 4, 1, right_to(1100.0)).outcome,
        EdgeResize::NoBoundary
    );
    assert_eq!(
        resize(LayoutType::Master, 4, 0, bottom_to(400.0)).outcome,
        EdgeResize::NoBoundary
    );
}

#[test]
fn master_share_stops_at_a_stack_minimum() {
    let r = resize_in(
        LayoutType::Master,
        (3, 0),
        (AREA, Gaps::ZERO),
        &[None, Some(Size::new(300.0, 0.0))],
        right_to(900.0),
    );
    assert!((r.after[1].width - 300.0).abs() <= 1.0, "{:?}", r.after);
}

#[test]
fn grid_of_three_keeps_the_narrowed_left_column_and_fills_the_rest() {
    // The live report: 1920x1200, usable 1892x1130 at 14,56 with 14 gaps.
    let area = Rect::new(14.0, 56.0, 1892.0, 1130.0);
    let gaps = Gaps::uniform(14.0, 0.0);
    let r = resize_in(LayoutType::Grid, (3, 0), (area, gaps), &[], |r| {
        Rect::new(r.x, r.y, 631.0, r.height)
    });
    assert!(
        same_rect(&r.after[0], &Rect::new(14.0, 56.0, 631.0, 1130.0)),
        "{:?}",
        r.after
    );
    assert!(
        same_rect(&r.after[1], &Rect::new(659.0, 56.0, 1247.0, 558.0)),
        "{:?}",
        r.after
    );
    assert!(
        same_rect(&r.after[2], &Rect::new(659.0, 628.0, 1247.0, 558.0)),
        "{:?}",
        r.after
    );
}

#[test]
fn grid_edges_move_their_column_or_row_boundary() {
    // 2x2: column boundary for both columns, row boundary for both rows.
    let right = resize(LayoutType::Grid, 4, 0, right_to(300.0));
    assert_eq!(changed(&right), vec![0, 1, 2, 3]);
    assert!(near(right.after[2].width, 300.0) && near(right.after[3].x, 300.0));
    let bottom = resize(LayoutType::Grid, 4, 1, bottom_to(150.0));
    assert!(near(bottom.after[0].height, 150.0) && near(bottom.after[3].y, 150.0));
    let left = resize(LayoutType::Grid, 4, 3, left_to(600.0));
    assert!(near(left.after[0].width, 600.0));
    let top = resize(LayoutType::Grid, 4, 2, top_to(350.0));
    assert!(near(top.after[1].height, 350.0));
    let outer = resize(LayoutType::Grid, 4, 0, top_to(-40.0));
    assert_eq!(outer.outcome, EdgeResize::NoBoundary);

    // Five tiles: master column, 2x2 stack. A stack column boundary moves the
    // two stack columns only; a stack row boundary leaves the master alone.
    let column = resize(LayoutType::Grid, 5, 1, right_to(600.0));
    assert_eq!(changed(&column), vec![1, 2, 3, 4]);
    assert!(near(column.after[1].width, 600.0 - 1000.0 / 3.0));
    let row = resize(LayoutType::Grid, 5, 2, bottom_to(200.0));
    assert_eq!(changed(&row), vec![1, 2, 3, 4]);
    assert!(near(row.after[3].y, 200.0));
    // Both edges of one axis: two column boundaries.
    let both = resize(LayoutType::Grid, 6, 1, |r| {
        Rect::new(200.0, r.y, 600.0, r.height)
    });
    assert!(near(both.after[0].width, 200.0) && near(both.after[2].x, 800.0));
}

#[test]
fn grid_uneven_last_row_follows_the_shared_lattice() {
    // Portrait ten: the master spans three rows of the first column; the last
    // row's first cell sits under it and shares its column and row boundaries.
    let portrait = Rect::new(0.0, 0.0, 600.0, 1200.0);
    let right = resize_in(
        LayoutType::Grid,
        (10, 7),
        (portrait, Gaps::ZERO),
        &[],
        right_to(300.0),
    );
    assert!(near(right.after[7].width, 300.0) && near(right.after[0].width, 300.0));
    let top = resize_in(
        LayoutType::Grid,
        (10, 7),
        (portrait, Gaps::ZERO),
        &[],
        top_to(1000.0),
    );
    assert!(near(top.after[7].y, 1000.0));
    assert!(near(top.after[0].y + top.after[0].height, 1000.0));
    assert!(near(top.after[8].y, 1000.0) && near(top.after[6].y + top.after[6].height, 1000.0));
}

#[test]
fn grid_boundary_stops_at_a_reported_minimum() {
    let r = resize_in(
        LayoutType::Grid,
        (4, 0),
        (AREA, Gaps::ZERO),
        &[None, Some(Size::new(400.0, 0.0))],
        right_to(900.0),
    );
    assert!((r.after[1].width - 400.0).abs() <= 1.0, "{:?}", r.after);
}

#[test]
fn layouts_without_boundaries_or_with_one_tile_change_nothing() {
    assert_eq!(
        resize(LayoutType::Monocle, 3, 0, right_to(300.0)).outcome,
        EdgeResize::NoBoundary
    );
    let floating = edge_resize(
        &ResizeRequest {
            layout: LayoutType::Floating,
            count: 3,
            position: 0,
            usable: AREA,
            gaps: Gaps::ZERO,
            master_position: MasterPosition::Left,
            master_ratio: 0.6,
            ratios: &[],
            minimums: &[None, None, None],
        },
        &Rect::new(0.0, 0.0, 400.0, 500.0),
        &Rect::new(0.0, 0.0, 300.0, 500.0),
    );
    assert_eq!(floating, EdgeResize::NoBoundary);
    assert_eq!(
        resize(LayoutType::Dwindle, 1, 0, right_to(300.0)).outcome,
        EdgeResize::NoBoundary
    );
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
