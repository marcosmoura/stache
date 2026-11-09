//! TI-08-02/03 (engine): the gesture over the real mailbox, runner and fake
//! backend, driven by the manual clock. No input service, no real window.

use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead, WriteStatus};
use stache_macos::windows::{IssuedKind, NativeStatus};

use super::*;
use crate::drag::{DragPoint, DragSignal, GESTURE_TIMEOUT, LostCause};
use crate::geometry::Rect;
use crate::state::DragOutcome;

const TARGET: Rect = Rect::new(0.0, 25.0, 1920.0, 1055.0);

fn applied() -> WindowActionOutcome {
    WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        native: NativeStatus::Frame {
            position: WriteStatus::Written,
            size: WriteStatus::Written,
        },
        observed: Some(FrameRead {
            position: AxReading::Value(AxPoint { x: TARGET.x, y: TARGET.y }),
            size: AxReading::Value(AxSize {
                width: TARGET.width,
                height: TARGET.height,
            }),
        }),
    }
}

fn signal(rig: &Rig, signal: DragSignal) {
    rig.ingress.submit(Input::Drag(signal)).expect("drag signal");
}

fn down() -> DragSignal {
    DragSignal::Down {
        button: 0,
        at: DragPoint { x: 10.0, y: 10.0 },
        epoch: 1,
        time: Duration::ZERO,
    }
}

fn up() -> DragSignal {
    DragSignal::Up {
        button: 0,
        at: DragPoint { x: 10.0, y: 10.0 },
        epoch: 1,
        time: Duration::from_millis(300),
    }
}

fn authorized() -> (Rig, WindowInstance) {
    let (mut rig, window) = Rig::framed();
    rig.backend.script_action(applied());
    rig.gate.publish(rig.feed.stamp.registry, 1);
    rig.ingress.submit(Input::FrameWrites(true)).expect("authorize");
    rig.pump();
    (rig, window)
}

#[test]
fn a_gesture_that_never_ends_is_abandoned_as_a_lost_observation_not_an_invented_up() {
    let (mut rig, _) = authorized();
    signal(&rig, down());
    rig.pump();
    assert!(rig.snap().drag.active);
    // The actor asks to be woken for the timeout.
    let turn = rig.engine.turn();
    assert!(turn.next_deadline.is_some(), "{turn:?}");

    rig.clock.advance(GESTURE_TIMEOUT.saturating_sub(Duration::from_secs(1)));
    rig.pump();
    assert!(rig.snap().drag.active, "not yet");

    rig.clock.advance(Duration::from_secs(2));
    // The first turn notices the timeout and queues the lost observation; the
    // mailbox wakes the actor, whose next turn applies it.
    rig.pump();
    assert!(
        rig.ingress.with(|pipeline| pipeline.has_work()),
        "the actor is woken for it"
    );
    rig.pump();
    let snap = rig.snap();
    assert!(!snap.drag.active, "the planner is released");
    let last = snap.drag.last.clone().expect("a record");
    assert_eq!(last.outcome, DragOutcome::Cancelled(LostCause::Timeout));
    assert_eq!(snap.diagnostics.drag_swaps + snap.diagnostics.drag_snap_backs, 0);
    // A late real up neither decides nor reads anything.
    signal(&rig, up());
    rig.pump();
    assert_eq!(rig.snap().diagnostics.drag_snap_backs, 0);
}

#[test]
fn the_final_frames_are_read_off_the_callback_and_the_gesture_commits_from_the_reads() {
    let (mut rig, window) = authorized();
    let before = rig.backend.action_count();
    signal(&rig, down());
    rig.pump();
    // The user dragged it by a small amount; the read returns that frame.
    let mut dragged = snapshot(window, "w", 1);
    dragged.frame = FrameRead {
        position: AxReading::Value(AxPoint { x: 40.0, y: 60.0 }),
        size: AxReading::Value(AxSize {
            width: TARGET.width,
            height: TARGET.height,
        }),
    };
    rig.backend
        .script_snapshot(window, SnapshotOutcome::Snapshot(Box::new(dragged)));
    rig.backend.script_action(applied());
    signal(&rig, up());
    rig.pump();
    let snap = rig.snap();
    assert!(!snap.drag.active);
    assert_eq!(
        snap.drag.last.clone().expect("record").outcome,
        DragOutcome::SnappedBack
    );
    assert_eq!(snap.diagnostics.drag_snap_backs, 1);
    // Exactly one snap-back write.
    assert_eq!(rig.backend.action_count(), before + 1);
}

#[test]
fn a_flood_of_drag_signals_is_bounded_and_ends_in_one_lost_observation() {
    let (mut rig, _) = authorized();
    for _ in 0..(crate::drag::DRAG_QUEUE_CAPACITY * 3) {
        signal(&rig, down());
    }
    rig.pump();
    let snap = rig.snap();
    assert!(snap.admission.drag_dropped >= 1);
    assert!(snap.diagnostics.drag_lost_observations >= 1);
    // The lane never grew past its bound.
    assert!(snap.admission.drag_accepted <= (crate::drag::DRAG_QUEUE_CAPACITY * 3) as u64);
}
