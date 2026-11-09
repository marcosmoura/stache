//! TI-08-02/03 (state): the drag gesture over the pure transition. The exact
//! identity, order, threshold, containment, ratio, revalidation, echo and
//! cancellation fixtures. No threads, input service or windows.

use std::sync::Arc;
use std::time::Duration;

use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead, WriteStatus};
use stache_macos::windows::{
    IssuedKind, NativeStatus, SnapshotOutcome, WindowActionOutcome, WindowChange, WindowInstance,
};

use super::frame_tests::{Fixture, one_screen};
use super::*;
use crate::config::TilingConfig;
use crate::drag::{DragPoint, DragSignal, LostCause};
use crate::geometry::Rect;
use crate::intent::Intent;
use crate::test_support::snapshot;

const EPOCH: u64 = 7;

fn config(layout: &str) -> Arc<TilingConfig> {
    Arc::new(
        serde_json::from_value(serde_json::json!({
            "default_layout": layout,
            "gaps": { "inner": 0, "outer": 0 },
        }))
        .expect("config"),
    )
}

fn point(x: f64, y: f64) -> DragPoint { DragPoint { x, y } }

fn frame_read(frame: Rect) -> FrameRead {
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
        observed: Some(frame_read(frame)),
    }
}

#[derive(Clone, Copy, Debug)]
struct Write {
    window: WindowInstance,
    plan: u64,
    target: Rect,
}

fn writes(effects: &[Effect]) -> Vec<Write> {
    effects
        .iter()
        .filter_map(|effect| match *effect {
            Effect::WriteFrame {
                window, plan_sequence, target, ..
            } => Some(Write {
                window,
                plan: plan_sequence,
                target,
            }),
            _ => None,
        })
        .collect()
}

fn reads(effects: &[Effect]) -> Vec<WindowInstance> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Snapshot { window, .. } => Some(*window),
            _ => None,
        })
        .collect()
}

struct Rig {
    f: Fixture,
}

impl Rig {
    /// `count` windows tiled by `layout`, every first write acknowledged at its
    /// target, so each frame is confirmed and no attempt is in flight.
    fn new(layout: &str, count: u32) -> Self {
        let mut f = Fixture::new(one_screen(), config(layout), count);
        let effects = f.enable();
        let mut rig = Self { f };
        for write in writes(&effects) {
            rig.ack(write);
        }
        rig
    }

    fn ack(&mut self, write: Write) -> Vec<Effect> {
        let (stamp, revision) = (self.f.feed.stamp, self.f.state.config_revision);
        self.f.apply(Input::Completion {
            stamp,
            result: CompletionResult::Frame {
                window: write.window,
                plan_sequence: write.plan,
                revision,
                outcome: applied_at(write.target),
            },
        })
    }

    fn token(&self, index: usize) -> WindowInstance { self.f.windows[index] }

    /// A user command replans; every resulting write is acknowledged at its
    /// target so the layout is settled again.
    fn reflow(&mut self) {
        let effects = self.f.apply(Input::Relayout);
        for write in writes(&effects) {
            self.ack(write);
        }
    }

    fn set_floating(&mut self, index: usize) {
        let token = self.token(index);
        self.f.state.windows.get_mut(&token).expect("window").desired.floating = true;
        self.reflow();
    }

    fn frame(&self, index: usize) -> Rect {
        self.f.state.windows[&self.token(index)].observed.frame.expect("frame")
    }

    fn observe(&mut self, window: WindowInstance, frame: Rect) -> Vec<Effect> {
        let mut snap = snapshot(window, "w", 1);
        snap.frame = frame_read(frame);
        let stamp = self.f.feed.stamp;
        self.f.apply(Input::Completion {
            stamp,
            result: CompletionResult::Snapshot(window, SnapshotOutcome::Snapshot(Box::new(snap))),
        })
    }

    /// Sets a window's confirmed frame without any plan reacting to it yet.
    fn place(&mut self, index: usize, frame: Rect) {
        let token = self.token(index);
        self.f.state.windows.get_mut(&token).expect("window").observed.frame = Some(frame);
    }

    fn down(&mut self, button: u32) -> Vec<Effect> { self.down_in(button, EPOCH) }

    fn down_in(&mut self, button: u32, epoch: u64) -> Vec<Effect> {
        self.f.apply(Input::Drag(DragSignal::Down {
            button,
            at: point(100.0, 100.0),
            epoch,
            time: Duration::from_millis(10),
        }))
    }

    fn up(&mut self, button: u32) -> Vec<Effect> { self.up_in(button, EPOCH) }

    fn up_in(&mut self, button: u32, epoch: u64) -> Vec<Effect> {
        self.f.apply(Input::Drag(DragSignal::Up {
            button,
            at: point(400.0, 100.0),
            epoch,
            time: Duration::from_millis(510),
        }))
    }

    fn lost(&mut self, cause: LostCause) -> Vec<Effect> {
        self.f.apply(Input::Drag(DragSignal::Lost(cause)))
    }

    /// Answers every outstanding drag read with the window's frame from `moved`
    /// (else its current frame). Returns the effects of the last answer, where the
    /// gesture commits and the plan runs.
    fn answer(&mut self, up_effects: &[Effect], moved: &[(usize, Rect)]) -> Vec<Effect> {
        let mut last = Vec::new();
        for window in reads(up_effects) {
            let frame = moved.iter().find(|(index, _)| self.token(*index) == window).map_or_else(
                || self.f.state.windows[&window].observed.frame.expect("frame"),
                |(_, frame)| *frame,
            );
            last = self.observe(window, frame);
        }
        last
    }

    /// A whole gesture: down, up, the reads answered with `moved`.
    fn drag(&mut self, moved: &[(usize, Rect)]) -> Vec<Effect> {
        self.down(0);
        let up = self.up(0);
        self.answer(&up, moved)
    }

    fn last(&self) -> DragRecord { self.f.state.drag.last.clone().expect("a finished gesture") }

    fn outcome(&self) -> DragOutcome { self.last().outcome }

    fn order(&self) -> Vec<WindowInstance> { self.f.state.workspaces[0].order.clone() }

    fn diagnostics(&self) -> &Diagnostics { &self.f.state.diagnostics }

    fn ratios(&self) -> Vec<f64> { self.f.state.workspaces[0].ratios.split_ratios.clone() }
}

fn moved_by(frame: Rect, dx: f64, dy: f64) -> Rect {
    Rect::new(frame.x + dx, frame.y + dy, frame.width, frame.height)
}

fn close(a: f64, b: f64) -> bool { (a - b).abs() < 1e-9 }

// --------------------------------------------------------------- capture

#[test]
fn a_down_captures_the_tiled_windows_in_order_with_their_exact_frames() {
    let mut rig = Rig::new("split", 3);
    rig.down(0);
    let gesture = rig.f.state.drag.gesture.clone().expect("gesture");
    let tokens: Vec<_> = gesture.windows.iter().map(|w| w.token).collect();
    assert_eq!(tokens, rig.f.windows);
    for (index, window) in gesture.windows.iter().enumerate() {
        assert_eq!(window.original, rig.frame(index));
    }
    assert_eq!(gesture.workspace, rig.f.state.workspaces[0].id);
    assert_eq!(gesture.epoch, EPOCH);
    assert_eq!(rig.diagnostics().drag_gestures, 1);
    assert!(rig.f.state.snapshot().drag.active);
}

#[test]
fn the_right_button_starts_a_gesture_like_the_left() {
    let mut rig = Rig::new("split", 2);
    rig.down(1);
    assert!(rig.f.state.drag.suppresses());
    let up = rig.up(1);
    assert_eq!(reads(&up).len(), 2);
}

#[test]
fn floating_windows_are_not_captured_and_their_movement_stays_the_users() {
    let mut rig = Rig::new("split", 3);
    let floating = rig.token(1);
    rig.set_floating(1);
    rig.down(0);
    let gesture = rig.f.state.drag.gesture.clone().expect("gesture");
    let tokens: Vec<_> = gesture.windows.iter().map(|w| w.token).collect();
    assert_eq!(tokens, vec![rig.token(0), rig.token(2)]);

    let up = rig.up(0);
    assert!(!reads(&up).contains(&floating));
    // The user dropped the floating window far away and inside a tile: no
    // snap-back for it, and it is no swap target.
    let first = rig.frame(0);
    let after = rig.answer(&up, &[]);
    assert!(writes(&after).iter().all(|w| w.window != floating), "{after:?}");
    let _ = first;
}

#[test]
fn a_floating_window_moved_during_a_gesture_is_never_pulled_back() {
    let mut rig = Rig::new("split", 3);
    let floating = rig.token(1);
    rig.set_floating(1);
    rig.down(0);
    let up = rig.up(0);
    // The floating window's own events during the gesture are accepted.
    rig.observe(floating, Rect::new(300.0, 300.0, 400.0, 300.0));
    let after = rig.answer(&up, &[]);
    assert!(writes(&after).is_empty(), "{after:?}");
    assert_eq!(
        rig.f.state.windows[&floating].observed.frame,
        Some(Rect::new(300.0, 300.0, 400.0, 300.0))
    );
}

#[test]
fn a_down_with_writes_off_no_tiles_or_a_floating_layout_starts_nothing() {
    let mut f = Fixture::new(one_screen(), config("split"), 2);
    // Writes are not published yet.
    f.apply(Input::Drag(DragSignal::Down {
        button: 0,
        at: point(1.0, 1.0),
        epoch: EPOCH,
        time: Duration::ZERO,
    }));
    assert!(!f.state.drag.suppresses());
    assert_eq!(f.state.diagnostics.drag_ignored, 1);

    let mut rig = Rig::new("floating", 2);
    rig.down(0);
    assert!(!rig.f.state.drag.suppresses());
    assert_eq!(rig.diagnostics().drag_ignored, 1);

    let mut empty = Rig::new("split", 0);
    empty.down(0);
    assert!(!empty.f.state.drag.suppresses());
}

// ------------------------------------------------------------ suppression

#[test]
fn nothing_is_written_while_a_gesture_is_active_and_no_attempt_is_consumed() {
    let mut rig = Rig::new("split", 2);
    let (first, second) = (rig.token(0), rig.token(1));
    rig.down(0);
    let plan = rig.f.state.frames.plan_sequence;
    // The user drags and resizes freely; each AX read is accepted as it is.
    for step in 1..=5 {
        let offset = f64::from(step) * 40.0;
        let effects = rig.observe(first, Rect::new(offset, offset, 500.0, 400.0));
        assert!(writes(&effects).is_empty(), "step {step}: {effects:?}");
    }
    rig.observe(second, Rect::new(900.0, 100.0, 300.0, 300.0));
    assert_eq!(rig.f.state.frames.plan_sequence, plan, "no plan moved");
    for token in [first, second] {
        let record = rig.f.state.frames.records.get(&token).expect("record");
        assert_eq!(
            record.externals, 0,
            "the user's change is not an external attempt"
        );
    }
    assert_eq!(
        rig.f.state.windows[&first].observed.frame,
        Some(Rect::new(200.0, 200.0, 500.0, 400.0))
    );
}

#[test]
fn a_plain_click_starts_no_plan_and_changes_nothing() {
    let mut rig = Rig::new("split", 2);
    let (order, plan) = (rig.order(), rig.f.state.frames.plan_sequence);
    let effects = rig.drag(&[]);
    assert!(writes(&effects).is_empty());
    assert_eq!(rig.order(), order);
    assert_eq!(rig.f.state.frames.plan_sequence, plan);
    assert_eq!(rig.outcome(), DragOutcome::SnappedBack);
    assert!(!rig.f.state.drag.suppresses());
    let record = rig.last();
    assert!(close(record.travel, 300.0));
    assert_eq!(record.held_for, Duration::from_millis(500));
}

// -------------------------------------------------------------- swap

/// Two overlapping windows and a third far away, so the threshold, the target
/// and the order are all controlled by the fixture.
fn overlapping() -> Rig {
    let mut rig = Rig::new("split", 3);
    rig.place(0, Rect::new(0.0, 0.0, 100.0, 100.0));
    rig.place(1, Rect::new(40.0, 0.0, 200.0, 100.0));
    rig.place(2, Rect::new(500.0, 0.0, 100.0, 100.0));
    rig
}

#[test]
fn the_swap_threshold_is_exactly_fifty_logical_points() {
    for (distance, swaps) in [(49.0, false), (50.0, false), (51.0, true)] {
        let mut rig = overlapping();
        let (a, b) = (rig.token(0), rig.token(1));
        let dropped = moved_by(rig.frame(0), distance, 0.0);
        rig.drag(&[(0, dropped)]);
        if swaps {
            assert_eq!(rig.outcome(), DragOutcome::Swapped { dragged: a, target: b });
            assert_eq!(rig.order()[..2], [b, a]);
        } else {
            assert_eq!(rig.outcome(), DragOutcome::SnappedBack, "{distance}px");
            assert_eq!(rig.order()[..2], [a, b], "{distance}px");
        }
    }
}

#[test]
fn a_swap_exchanges_exactly_two_tokens_and_keeps_the_rest_in_order() {
    let mut rig = Rig::new("split", 4);
    let tokens = rig.f.windows.clone();
    let first_tile = rig.frame(0);
    // Drag window 0 onto window 3's tile.
    let onto = moved_by(rig.frame(0), rig.frame(3).x - rig.frame(0).x, 0.0);
    let effects = rig.drag(&[(0, onto)]);
    assert_eq!(rig.outcome(), DragOutcome::Swapped {
        dragged: tokens[0],
        target: tokens[3],
    });
    assert_eq!(rig.order(), vec![tokens[3], tokens[1], tokens[2], tokens[0]]);
    assert_eq!(rig.diagnostics().drag_swaps, 1);
    // The new order is planned once. The dragged window already sits on the
    // target's tile, so only the displaced window is written, to the dragged
    // window's old tile; the others do not move.
    let moved = writes(&effects);
    assert_eq!(moved.len(), 1, "{moved:?}");
    assert_eq!(moved[0].window, tokens[3]);
    assert_eq!(moved[0].target, first_tile);
}

#[test]
fn when_two_windows_both_moved_beyond_fifty_the_greatest_travel_is_the_dragged_one() {
    let mut rig = overlapping();
    let tokens = rig.f.windows.clone();
    // Window 0 moves 80 (inside window 1); window 2 moves 380 onto window 1.
    let first = moved_by(rig.frame(0), 80.0, 0.0);
    let third = moved_by(rig.frame(2), -380.0, 0.0);
    rig.drag(&[(0, first), (2, third)]);
    // The centre of the third lands at (170, 50): window 0 [0..100] does not
    // contain x=170, window 1 [40..240] does.
    assert_eq!(rig.outcome(), DragOutcome::Swapped {
        dragged: tokens[2],
        target: tokens[1],
    });
}

#[test]
fn the_dropped_centre_exactly_on_a_targets_edge_counts() {
    let mut rig = Rig::new("split", 2);
    let tokens = rig.f.windows.clone();
    rig.place(0, Rect::new(0.0, 0.0, 100.0, 100.0));
    rig.place(1, Rect::new(300.0, 0.0, 200.0, 100.0));
    // Window 0's centre is (50, 50); move it so the centre is (300, 50): the
    // target's left edge.
    let edge = moved_by(rig.frame(0), 250.0, 0.0);
    rig.drag(&[(0, edge)]);
    assert_eq!(rig.outcome(), DragOutcome::Swapped {
        dragged: tokens[0],
        target: tokens[1],
    });

    let mut miss = Rig::new("split", 2);
    miss.place(0, Rect::new(0.0, 0.0, 100.0, 100.0));
    miss.place(1, Rect::new(300.0, 0.0, 200.0, 100.0));
    let short = moved_by(miss.frame(0), 249.999, 0.0);
    miss.drag(&[(0, short)]);
    assert_eq!(
        miss.outcome(),
        DragOutcome::SnappedBack,
        "just short of the edge"
    );
}

#[test]
fn the_first_target_in_stable_order_wins_when_originals_overlap() {
    let mut rig = Rig::new("split", 3);
    let tokens = rig.f.windows.clone();
    rig.place(0, Rect::new(0.0, 0.0, 100.0, 100.0));
    rig.place(1, Rect::new(500.0, 0.0, 300.0, 100.0));
    rig.place(2, Rect::new(600.0, 0.0, 300.0, 100.0));
    // The dropped centre (650, 50) is inside windows 1 and 2.
    let dropped = moved_by(rig.frame(0), 600.0, 0.0);
    rig.drag(&[(0, dropped)]);
    assert_eq!(rig.outcome(), DragOutcome::Swapped {
        dragged: tokens[0],
        target: tokens[1],
    });
}

#[test]
fn floating_windows_are_never_a_target_and_no_target_means_one_snap_back() {
    let mut rig = Rig::new("split", 3);
    let tokens = rig.f.windows.clone();
    rig.set_floating(2);
    rig.place(0, Rect::new(0.0, 0.0, 100.0, 100.0));
    rig.place(1, Rect::new(500.0, 0.0, 100.0, 100.0));
    rig.place(2, Rect::new(1000.0, 0.0, 300.0, 300.0));
    // Dropped inside the floating window only.
    let onto_floating = moved_by(rig.frame(0), 1100.0, 100.0);
    let plan = rig.f.state.frames.plan_sequence;
    let effects = rig.drag(&[(0, onto_floating)]);
    assert_eq!(rig.outcome(), DragOutcome::SnappedBack);
    assert_eq!(rig.order(), tokens, "no swap");
    // One snap-back: the dragged window is written back exactly once, and every
    // write belongs to one plan sequence.
    let back = writes(&effects);
    assert_eq!(
        back.iter().filter(|w| w.window == tokens[0]).count(),
        1,
        "{back:?}"
    );
    assert!(
        back.iter().all(|w| w.window != tokens[2]),
        "the floating window is user-owned"
    );
    assert!(
        back.iter().all(|w| w.plan == plan + 1),
        "a single plan sequence: {back:?}"
    );
    assert_eq!(rig.diagnostics().drag_snap_backs, 1);
}

#[test]
fn a_snap_back_is_possible_even_after_the_external_attempt_budget_was_spent() {
    let mut rig = Rig::new("split", 2);
    let first = rig.token(0);
    // Spend the one external attempt on the window before the gesture.
    let away = Rect::new(333.0, 222.0, 500.0, 400.0);
    let spent = rig.observe(first, away);
    for write in writes(&spent) {
        rig.ack(Write { target: away, ..write });
    }
    assert_eq!(rig.f.state.frames.records[&first].externals, 1);
    rig.observe(first, Rect::new(111.0, 77.0, 500.0, 400.0));
    // The drag moves it again; the commit resets the budget and snaps back.
    let effects = rig.drag(&[(0, Rect::new(400.0, 400.0, 500.0, 400.0))]);
    assert_eq!(
        writes(&effects).iter().filter(|w| w.window == first).count(),
        1,
        "{effects:?}"
    );
}

// ------------------------------------------------------------ revalidation

#[test]
fn a_changed_tile_list_at_commit_is_stale_no_swap() {
    let mut rig = overlapping();
    let tokens = rig.f.windows.clone();
    rig.down(0);
    // A window closes during the gesture.
    let destroyed = rig.f.feed.change(tokens[2], WindowChange::Destroyed);
    rig.f.apply(destroyed);
    let up = rig.up(0);
    let dropped = moved_by(rig.frame(0), 80.0, 0.0);
    rig.answer(&up, &[(0, dropped)]);
    assert_eq!(rig.outcome(), DragOutcome::Stale);
    assert_eq!(rig.order(), vec![tokens[0], tokens[1]], "not swapped");
    assert_eq!(rig.diagnostics().drag_stale, 1);
    assert!(!rig.f.state.drag.suppresses());
}

#[test]
fn a_changed_layout_or_revision_at_commit_is_stale() {
    let mut layout = overlapping();
    layout.down(0);
    layout
        .f
        .apply(Input::Intent(Intent::SetLayout(crate::config::LayoutType::Grid)));
    let up = layout.up(0);
    let dropped = moved_by(layout.frame(0), 80.0, 0.0);
    layout.answer(&up, &[(0, dropped)]);
    assert_eq!(layout.outcome(), DragOutcome::Stale);

    let mut revision = overlapping();
    revision.down(0);
    revision.f.apply(Input::Config {
        revision: 9,
        config: config("split"),
    });
    let up = revision.up(0);
    let dropped = moved_by(revision.frame(0), 80.0, 0.0);
    revision.answer(&up, &[(0, dropped)]);
    assert_eq!(revision.outcome(), DragOutcome::Stale);
}

#[test]
fn a_destroyed_window_during_the_final_reads_is_stale_not_a_swap() {
    let mut rig = overlapping();
    let tokens = rig.f.windows.clone();
    rig.down(0);
    let up = rig.up(0);
    // The target answers that it no longer exists.
    let stamp = rig.f.feed.stamp;
    for window in reads(&up) {
        let outcome = if window == tokens[1] {
            SnapshotOutcome::Stale(stache_macos::windows::StaleReason::Destroyed)
        } else {
            let mut snap = snapshot(window, "w", 1);
            let frame = if window == tokens[0] {
                moved_by(rig.frame(0), 80.0, 0.0)
            } else {
                rig.frame(2)
            };
            snap.frame = frame_read(frame);
            SnapshotOutcome::Snapshot(Box::new(snap))
        };
        rig.f.apply(Input::Completion {
            stamp,
            result: CompletionResult::Snapshot(window, outcome),
        });
    }
    assert_eq!(rig.outcome(), DragOutcome::Stale);
    assert!(!rig.order().contains(&tokens[1]));
}

#[test]
fn a_registry_restart_voids_the_gesture_and_a_late_up_is_ignored() {
    let mut rig = Rig::new("split", 2);
    rig.down(0);
    let mut next = crate::test_support::Feed::new(2);
    rig.f.apply(Input::Start(next.stamp));
    assert!(!rig.f.state.drag.suppresses());
    let effects = rig.up(0);
    assert!(reads(&effects).is_empty());
    let _ = &mut next;
}

// ------------------------------------------------------------- resize

fn resize_first(rig: &mut Rig, index: usize, dx: f64, dy: f64) {
    let frame = rig.frame(index);
    let grown = Rect::new(frame.x, frame.y, frame.width + dx, frame.height + dy);
    rig.drag(&[(index, grown)]);
}

#[test]
fn split_resize_moves_the_cumulative_boundary_by_the_share_of_the_area() {
    let mut rig = Rig::new("split", 3);
    let window = rig.token(0);
    resize_first(&mut rig, 0, 192.0, 0.0);
    assert_eq!(rig.outcome(), DragOutcome::Resized { window, changed: true });
    let ratios = rig.ratios();
    assert!(
        close(ratios[0], 1.0 / 3.0 + 0.1) && close(ratios[1], 2.0 / 3.0),
        "{ratios:?}"
    );
    assert_eq!(rig.diagnostics().drag_resizes, 1);
    // The ratio is replanned: the tiles are written once.
    assert!(rig.f.state.frames.plan_sequence > 1);
}

#[test]
fn split_resize_of_the_last_window_moves_the_previous_boundary_the_other_way() {
    let mut rig = Rig::new("split", 3);
    resize_first(&mut rig, 2, 192.0, 0.0);
    let ratios = rig.ratios();
    assert!(
        close(ratios[0], 1.0 / 3.0) && close(ratios[1], 2.0 / 3.0 - 0.1),
        "{ratios:?}"
    );
}

#[test]
fn dwindle_resize_moves_the_window_before_its_own_ratio() {
    let mut first = Rig::new("dwindle", 3);
    resize_first(&mut first, 0, 96.0, 0.0);
    assert!(close(first.ratios()[0], 0.55), "{:?}", first.ratios());
    assert!(close(first.ratios()[1], 0.5));

    let mut last = Rig::new("dwindle", 3);
    resize_first(&mut last, 2, 96.0, 0.0);
    assert!(close(last.ratios()[0], 0.5), "{:?}", last.ratios());
    assert!(close(last.ratios()[1], 0.55));
}

#[test]
fn grid_resize_follows_the_width_and_replans_for_a_height_change() {
    let mut wide = Rig::new("grid", 4);
    resize_first(&mut wide, 1, 96.0, 0.0);
    assert!(close(wide.ratios()[0], 0.55), "{:?}", wide.ratios());

    let mut tall = Rig::new("grid", 4);
    let window = tall.token(1);
    resize_first(&mut tall, 1, 0.0, 100.0);
    assert!(tall.ratios().is_empty());
    assert_eq!(tall.outcome(), DragOutcome::Resized { window, changed: false });
}

#[test]
fn master_resize_moves_the_master_share_and_flips_for_a_stack_window() {
    let configured = f64::from(TilingConfig::default().master.ratio) / 100.0;
    let mut master = Rig::new("master", 3);
    resize_first(&mut master, 0, 192.0, 0.0);
    let grown = master.f.state.workspaces[0].ratios.master_ratio.expect("override");
    assert!(close(grown, configured + 0.1), "{grown}");

    let mut stack = Rig::new("master", 3);
    resize_first(&mut stack, 1, 192.0, 0.0);
    let shrunk = stack.f.state.workspaces[0].ratios.master_ratio.expect("override");
    assert!(close(shrunk, configured - 0.1), "{shrunk}");
}

#[test]
fn monocle_has_no_ratio_and_only_replans() {
    let mut rig = Rig::new("monocle", 3);
    let window = rig.token(0);
    resize_first(&mut rig, 0, 192.0, 0.0);
    assert_eq!(rig.outcome(), DragOutcome::Resized { window, changed: false });
    assert!(rig.ratios().is_empty());
    assert!(rig.f.state.workspaces[0].ratios.master_ratio.is_none());
}

#[test]
fn a_resize_takes_precedence_over_a_swap_and_a_floating_resize_changes_nothing() {
    // The resized window's centre also moved beyond fifty: still a resize.
    let mut rig = Rig::new("split", 2);
    let frame = rig.frame(0);
    let resized = Rect::new(frame.x - 120.0, frame.y, frame.width + 300.0, frame.height);
    let tokens = rig.f.windows.clone();
    rig.drag(&[(0, resized)]);
    assert!(matches!(rig.outcome(), DragOutcome::Resized { window, .. } if window == tokens[0]));
    assert_eq!(rig.order(), tokens);

    let mut floating = Rig::new("split", 2);
    let window = floating.token(1);
    floating.set_floating(1);
    let before = floating.ratios();
    let frame = floating.f.state.windows[&window].observed.frame.expect("frame");
    floating.down(0);
    let up = floating.up(0);
    floating.observe(
        window,
        Rect::new(frame.x, frame.y, frame.width + 400.0, frame.height),
    );
    floating.answer(&up, &[]);
    assert_eq!(floating.ratios(), before);
}

// ------------------------------------------------------------ echoes

#[test]
fn an_own_write_that_completes_during_a_gesture_is_the_new_baseline_not_a_drag() {
    // Frame writes are published but not yet acknowledged when the button goes
    // down: the captured frame is the pre-write one.
    let mut f = Fixture::new(one_screen(), config("split"), 2);
    let effects = f.enable();
    let mut rig = Rig { f };
    let pending = writes(&effects);
    assert_eq!(pending.len(), 2);
    let tokens = rig.f.windows.clone();
    rig.down(0);
    let before = rig.f.state.drag.gesture.clone().expect("gesture");
    assert_ne!(before.windows[0].original, pending[0].target);

    // Their echoes arrive: the readbacks rebase the gesture.
    for write in &pending {
        rig.ack(*write);
    }
    let after = rig.f.state.drag.gesture.clone().expect("gesture");
    for (window, write) in after.windows.iter().zip(&pending) {
        assert_eq!(window.original, write.target, "rebased to the readback");
    }

    // Nobody touched anything: the reads match the baseline, so no window is
    // "dragged" even though the layout moved them by far more than fifty.
    let up = rig.up(0);
    rig.answer(&up, &[]);
    assert_eq!(rig.outcome(), DragOutcome::SnappedBack);
    assert_eq!(rig.order(), tokens);
    assert_eq!(rig.diagnostics().drag_swaps, 0);
}

#[test]
fn a_genuine_user_change_after_an_echo_is_still_a_drag() {
    let mut f = Fixture::new(one_screen(), config("split"), 2);
    let effects = f.enable();
    let mut rig = Rig { f };
    let pending = writes(&effects);
    let tokens = rig.f.windows.clone();
    rig.down(0);
    for write in &pending {
        rig.ack(*write);
    }
    // Then the user really drags window 0 onto window 1's tile.
    let onto = moved_by(pending[0].target, pending[1].target.x - pending[0].target.x, 0.0);
    let up = rig.up(0);
    rig.answer(&up, &[(0, onto)]);
    assert_eq!(rig.outcome(), DragOutcome::Swapped {
        dragged: tokens[0],
        target: tokens[1],
    });
}

#[test]
fn suppression_ends_with_the_gesture_so_a_later_external_change_is_not_masked() {
    let mut rig = Rig::new("split", 2);
    let first = rig.token(0);
    rig.drag(&[]);
    assert!(!rig.f.state.drag.suppresses());
    // A genuine external change after the gesture gets its one attempt again.
    let away = Rect::new(600.0, 300.0, 500.0, 400.0);
    let effects = rig.observe(first, away);
    assert_eq!(writes(&effects).iter().filter(|w| w.window == first).count(), 1);
}

// --------------------------------------------------------- cancellation

fn assert_cancelled(rig: &Rig, cause: LostCause) {
    assert!(!rig.f.state.drag.suppresses(), "the gesture is void");
    assert_eq!(rig.outcome(), DragOutcome::Cancelled(cause));
    assert_eq!(rig.diagnostics().drag_swaps, 0);
    assert_eq!(rig.diagnostics().drag_snap_backs, 0);
}

#[test]
fn every_lost_observation_cancels_the_gesture_and_reconciles_once() {
    for cause in [
        LostCause::Overflow,
        LostCause::Discontinuity,
        LostCause::Malformed,
        LostCause::PermissionLost,
        LostCause::Timeout,
        LostCause::Stopped,
    ] {
        let mut rig = Rig::new("split", 2);
        rig.down(0);
        let effects = rig.lost(cause);
        assert_cancelled(&rig, cause);
        // One refresh of the one application: the reconciliation.
        let refreshes = effects.iter().filter(|e| matches!(e, Effect::Refresh { .. })).count();
        assert_eq!(refreshes, 1, "{cause:?}: {effects:?}");
        assert_eq!(rig.diagnostics().drag_cancelled, 1);
        assert_eq!(rig.diagnostics().drag_lost_observations, 1);
        // A second loss with no gesture only counts: no second reconcile.
        let again = rig.lost(cause);
        assert!(
            again.iter().all(|e| !matches!(e, Effect::Refresh { .. })),
            "{again:?}"
        );
        assert_eq!(rig.diagnostics().drag_lost_observations, 2);
        assert_eq!(rig.diagnostics().drag_cancelled, 1);
    }
}

#[test]
fn a_lost_gesture_never_invents_an_up_or_replays_a_click() {
    let mut rig = Rig::new("split", 2);
    rig.down(0);
    rig.lost(LostCause::Discontinuity);
    // The real up arrives later: nothing to finish, nothing read.
    let up = rig.up(0);
    assert!(reads(&up).is_empty());
    assert_eq!(rig.outcome(), DragOutcome::Cancelled(LostCause::Discontinuity));
    // And nothing was moved by the cancellation itself.
    assert_eq!(
        rig.diagnostics().drag_swaps + rig.diagnostics().drag_snap_backs,
        0
    );
}

#[test]
fn an_up_in_another_input_epoch_proves_missed_events_and_cancels() {
    let mut rig = Rig::new("split", 2);
    rig.down(0);
    let effects = rig.up_in(0, EPOCH + 1);
    assert!(reads(&effects).is_empty());
    assert_cancelled(&rig, LostCause::Discontinuity);
}

#[test]
fn a_down_of_an_already_held_button_proves_a_missed_up() {
    let mut rig = Rig::new("split", 2);
    rig.down(0);
    let first = rig.f.state.drag.gesture_id().expect("gesture");
    rig.down(0);
    // The first gesture is void; the new down starts a fresh one.
    assert!(rig.f.state.drag.suppresses());
    assert_ne!(rig.f.state.drag.gesture_id(), Some(first));
    assert_eq!(
        rig.last().outcome,
        DragOutcome::Cancelled(LostCause::Discontinuity)
    );
}

#[test]
fn a_second_button_extends_the_gesture_until_the_last_one_is_up() {
    let mut rig = Rig::new("split", 2);
    rig.down(0);
    rig.down(1);
    let held_left = rig.up(0);
    assert!(reads(&held_left).is_empty(), "the right button is still down");
    assert!(rig.f.state.drag.suppresses());
    let released = rig.up(1);
    assert_eq!(reads(&released).len(), 2);
}

#[test]
fn losing_accessibility_cancels_the_gesture() {
    let mut rig = Rig::new("split", 2);
    rig.down(0);
    let event = rig.f.feed.input(stache_macos::windows::WindowEventKind::CapabilityChanged {
        capability: stache_macos::windows::Capability::Accessibility,
        available: false,
    });
    rig.f.apply(event);
    assert_cancelled(&rig, LostCause::PermissionLost);
}

#[test]
fn losing_an_observation_with_no_gesture_changes_nothing() {
    let mut rig = Rig::new("split", 2);
    let plan = rig.f.state.frames.plan_sequence;
    rig.lost(LostCause::Overflow);
    assert!(rig.f.state.drag.last.is_none());
    assert_eq!(rig.f.state.frames.plan_sequence, plan);
    assert_eq!(rig.diagnostics().drag_lost_observations, 1);
}
