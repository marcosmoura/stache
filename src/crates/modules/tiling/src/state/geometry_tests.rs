//! Geometry samples of windows Stache is moving: which read a sample asks for,
//! and what a window-server frame changes. Pure state transitions.

use stache_macos::windows::{IssuedKind, NativeStatus, WindowChange, WindowInstance};

use super::frame_tests::{Fixture, gap_config, one_screen};
use super::*;
use crate::geometry::Rect;

fn reads(effects: &[Effect]) -> Vec<Effect> {
    effects
        .iter()
        .filter(|e| matches!(e, Effect::Snapshot { .. } | Effect::ReadGeometry { .. }))
        .cloned()
        .collect()
}

fn written(effects: &[Effect]) -> Vec<(WindowInstance, u64)> {
    effects
        .iter()
        .filter_map(|e| match *e {
            Effect::WriteFrame { window, plan_sequence, .. } => Some((window, plan_sequence)),
            _ => None,
        })
        .collect()
}

fn travel(fixture: &Fixture, window: WindowInstance) -> (Rect, Rect) {
    let attempt = fixture.state.frames.records[&window].attempt.as_ref().expect("attempt");
    (attempt.from, attempt.target)
}

fn cg_of(fixture: &Fixture, window: WindowInstance) -> u32 {
    fixture.state.windows[&window].cg_window_id.expect("an id")
}

/// Three windows with an in-flight write each; the second settles.
fn three_in_flight() -> (Fixture, Vec<(WindowInstance, u64)>) {
    let mut fixture = Fixture::new(one_screen(), gap_config(0), 3);
    let writes = written(&fixture.enable());
    assert_eq!(writes.len(), 3, "every window is written: {writes:?}");
    (fixture, writes)
}

#[test]
fn a_geometry_sample_reads_the_window_server_only_for_a_window_in_flight_with_an_id() {
    let (mut fixture, writes) = three_in_flight();
    let [a, b, c] = [fixture.windows[0], fixture.windows[1], fixture.windows[2]];
    // `b` settles: no attempt in flight any more.
    let (_, plan) = writes.iter().find(|(w, _)| *w == b).copied().expect("b");
    fixture.ack(b, plan, WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        native: NativeStatus::Frame {
            position: stache_macos::ax::WriteStatus::Written,
            size: stache_macos::ax::WriteStatus::Written,
        },
        observed: None,
    });
    // `c` is in flight but its window id is unknown.
    fixture.state.windows.get_mut(&c).expect("c").cg_window_id = None;
    let stamp = fixture.stamp();
    let untracked = fixture.feed.token(1, 99);

    let effects = fixture.apply(Input::GeometryBatch(vec![a, b, c, untracked]));
    assert_eq!(reads(&effects), vec![
        Effect::ReadGeometry {
            stamp,
            window: a,
            cg_window_id: cg_of(&fixture, a),
            start: travel(&fixture, a).0,
            target: travel(&fixture, a).1,
        },
        Effect::Snapshot { stamp, window: b },
        Effect::Snapshot { stamp, window: c },
    ]);

    // The direct moved or resized arm asks for the same reads.
    for change in [WindowChange::Moved, WindowChange::Resized] {
        let input = fixture.feed.change(a, change);
        assert_eq!(reads(&fixture.apply(input)), vec![Effect::ReadGeometry {
            stamp,
            window: a,
            cg_window_id: cg_of(&fixture, a),
            start: travel(&fixture, a).0,
            target: travel(&fixture, a).1,
        }]);
        let input = fixture.feed.change(b, change);
        assert_eq!(reads(&fixture.apply(input)), vec![Effect::Snapshot {
            stamp,
            window: b
        }]);
    }
    // A title change still reads the whole snapshot, in flight or not.
    let input = fixture.feed.change(a, WindowChange::TitleChanged);
    assert_eq!(reads(&fixture.apply(input)), vec![Effect::Snapshot {
        stamp,
        window: a
    }]);
}

#[test]
fn a_geometry_completion_changes_the_observed_frame_and_nothing_else() {
    let (mut fixture, _) = three_in_flight();
    let window = fixture.windows[0];
    let stamp = fixture.stamp();
    // A full-screen-sized frame: still no fullscreen flag, exclusion or slot.
    let frame = Rect::new(0.0, 0.0, 1920.0, 1080.0);
    let mut expected = fixture.state.clone();
    expected.windows.get_mut(&window).expect("window").observed.frame = Some(frame);
    let effects = fixture.apply(Input::Completion {
        stamp,
        result: CompletionResult::Geometry(window, frame),
    });
    assert!(written(&effects).is_empty(), "{effects:?}");
    // Every input advances the input sequence; nothing else differs.
    expected.sequence = fixture.state.sequence;
    assert_eq!(format!("{:?}", fixture.state), format!("{expected:?}"));

    // An untracked window: nothing at all.
    let mut before = fixture.state.clone();
    let untracked = fixture.feed.token(1, 99);
    fixture.apply(Input::Completion {
        stamp,
        result: CompletionResult::Geometry(untracked, frame),
    });
    before.sequence = fixture.state.sequence;
    assert_eq!(format!("{:?}", fixture.state), format!("{before:?}"));
}

#[test]
fn a_geometry_completion_passes_the_echo_guard_of_a_settled_animation() {
    let (mut fixture, writes) = three_in_flight();
    let window = fixture.windows[0];
    let (_, plan) = writes.iter().find(|(w, _)| *w == window).copied().expect("write");
    let target = fixture.state.frames.records[&window].attempt.as_ref().expect("attempt").target;
    fixture.ack(window, plan, WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        native: NativeStatus::Frame {
            position: stache_macos::ax::WriteStatus::Written,
            size: stache_macos::ax::WriteStatus::Written,
        },
        observed: None,
    });
    // As after an animated attempt settled: one own echo still to ignore.
    fixture.state.frames.records.get_mut(&window).expect("record").settle_echoes = 1;
    let confirmed = fixture.state.windows[&window].observed.frame;
    let stamp = fixture.stamp();
    let echo = Rect::new(target.x + 40.0, target.y, target.width, target.height);
    fixture.apply(Input::Completion {
        stamp,
        result: CompletionResult::Geometry(window, echo),
    });
    assert_eq!(fixture.state.windows[&window].observed.frame, confirmed);
    assert_eq!(fixture.state.frames.records[&window].settle_echoes, 0);
    assert_eq!(fixture.state.diagnostics.animation_echoes_ignored, 1);
}

#[test]
fn a_frame_is_explained_only_inside_the_travel_within_the_tolerance() {
    let start = Rect::new(0.0, 0.0, 100.0, 100.0);
    let target = Rect::new(200.0, 50.0, 300.0, 80.0);
    let inside = Rect::new(100.0, 25.0, 200.0, 90.0);
    assert!(explained_by_travel(&inside, &start, &target));
    assert!(explained_by_travel(&inside, &target, &start), "either direction");
    // On the edges, within the frame tolerance.
    assert!(explained_by_travel(
        &Rect::new(-4.9, 54.9, 304.9, 75.1),
        &start,
        &target
    ));
    // Each component outside.
    for frame in [
        Rect::new(-5.1, 25.0, 200.0, 90.0),
        Rect::new(205.1, 25.0, 200.0, 90.0),
        Rect::new(100.0, 55.1, 200.0, 90.0),
        Rect::new(100.0, 25.0, 305.1, 90.0),
        Rect::new(100.0, 25.0, 200.0, 74.9),
        Rect::new(0.0, 0.0, 1920.0, 1080.0),
    ] {
        assert!(!explained_by_travel(&frame, &start, &target), "{frame:?}");
    }
}
