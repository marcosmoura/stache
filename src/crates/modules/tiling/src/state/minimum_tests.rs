//! Grid and minimum-size scenarios replayed from native run 10 (Zed, one
//! 1920x1200 display, gaps 14): a window that reads back wider after a write
//! that moved and resized it must not teach a false minimum, a genuine refusal
//! re-plans once, and nothing is re-written in a loop. Pure state transitions.

use std::sync::Arc;

use stache_macos::applications::DisplayRect;
use stache_macos::ax::{AxPoint, AxReading, AxSize, FrameRead, WriteStatus};
use stache_macos::windows::{IssuedKind, NativeStatus, WindowActionOutcome, WindowInstance};
use stache_macos::{DisplayId, DisplayInfo, DisplayLayout};

use super::*;
use crate::geometry::Rect;
use crate::outcome::FrameComponents;
use crate::test_support::{Feed, app, info, run, snapshot, step};

const MASTER: Rect = Rect::new(14.0, 14.0, 939.0, 1172.0);
const TOP: Rect = Rect::new(967.0, 14.0, 939.0, 579.0);
const BOTTOM: Rect = Rect::new(967.0, 607.0, 939.0, 579.0);
const MONOCLE: Rect = Rect::new(14.0, 14.0, 1892.0, 1172.0);
/// Zed's real minimum width (it refuses anything narrower).
const ZED_MIN_WIDTH: f64 = 360.0;

fn display() -> DisplayLayout {
    DisplayLayout::new(vec![DisplayInfo {
        id: DisplayId::new(1),
        frame: DisplayRect::new(0.0, 0.0, 1920.0, 1200.0),
        visible_frame: DisplayRect::new(0.0, 0.0, 1920.0, 1200.0),
        scale: 1.0,
        refresh_hz: Some(120.0),
        name: Some("Built-in Display".to_owned()),
        is_primary: true,
        is_main: true,
        is_builtin: true,
    }])
}

fn config(layout: &str) -> Arc<crate::config::TilingConfig> { config_with(layout, false) }

fn config_with(layout: &str, animations: bool) -> Arc<crate::config::TilingConfig> {
    Arc::new(
        serde_json::from_value(serde_json::json!({
            "default_layout": layout,
            "gaps": { "inner": 14, "outer": 14 },
            "animations": { "enabled": animations },
        }))
        .expect("config"),
    )
}

const fn read(frame: Rect) -> FrameRead {
    FrameRead {
        position: AxReading::Value(AxPoint { x: frame.x, y: frame.y }),
        size: AxReading::Value(AxSize {
            width: frame.width,
            height: frame.height,
        }),
    }
}

/// One write the state asked for.
#[derive(Clone, Copy, Debug)]
struct Sent {
    window: WindowInstance,
    plan: u64,
    target: Rect,
    components: FrameComponents,
    animate: bool,
}

fn sent(effects: &[Effect]) -> Vec<Sent> {
    effects
        .iter()
        .filter_map(|effect| match *effect {
            Effect::WriteFrame {
                window,
                plan_sequence,
                target,
                components,
                animate,
                ..
            } => Some(Sent {
                window,
                plan: plan_sequence,
                target,
                components,
                animate,
            }),
            _ => None,
        })
        .collect()
}

/// A model of Zed answering frame writes.
struct Zed {
    /// The frame each window really has.
    frames: std::collections::BTreeMap<WindowInstance, Rect>,
    /// Windows whose next move-and-resize write reads back this wider width
    /// (the transient width seen in the run), once.
    artifact: std::collections::BTreeMap<WindowInstance, Rect>,
    /// Windows that genuinely refuse to be narrower than this.
    minimum: std::collections::BTreeMap<WindowInstance, f64>,
    /// An animated write is answered mid-motion: the readback is the frame the
    /// window had before the write (run 10, plans 3 and 4).
    animated_reads_before: bool,
}

impl Zed {
    fn answer(&mut self, write: &Sent) -> WindowActionOutcome {
        let before = self.frames[&write.window];
        let mut frame = Rect::new(
            if write.components.position {
                write.target.x
            } else {
                before.x
            },
            if write.components.position {
                write.target.y
            } else {
                before.y
            },
            if write.components.size {
                write.target.width
            } else {
                before.width
            },
            if write.components.size {
                write.target.height
            } else {
                before.height
            },
        );
        let floor = self.minimum.get(&write.window).copied().unwrap_or(ZED_MIN_WIDTH);
        frame.width = frame.width.max(floor);
        if write.components.position
            && write.components.size
            && let Some(transient) = self.artifact.remove(&write.window)
        {
            frame = transient;
        }
        self.frames.insert(write.window, frame);
        if write.animate && self.animated_reads_before {
            frame = before;
        }
        let near = |a: f64, b: f64| (a - b).abs() <= 1.0;
        let applied = near(frame.x, write.target.x)
            && near(frame.y, write.target.y)
            && near(frame.width, write.target.width)
            && near(frame.height, write.target.height);
        let status = |requested: bool| {
            if requested {
                WriteStatus::Written
            } else {
                WriteStatus::NotRequested
            }
        };
        WindowActionOutcome::Issued {
            kind: if applied {
                IssuedKind::Applied
            } else {
                IssuedKind::Partial
            },
            native: NativeStatus::Frame {
                position: status(write.components.position),
                size: status(write.components.size),
            },
            observed: Some(read(frame)),
        }
    }
}

struct World {
    state: TilingState,
    feed: Feed,
    windows: Vec<WindowInstance>,
    zed: Zed,
    /// Every write answered, in order.
    log: Vec<Sent>,
}

impl World {
    /// Zed with `frames.len()` windows at the given frames; `minimized` marks
    /// the minimized ones (they never get a cell).
    fn new(layout: &str, frames: &[Rect], minimized: &[bool]) -> Self {
        Self::with(config(layout), frames, minimized)
    }

    fn with(config: Arc<crate::config::TilingConfig>, frames: &[Rect], minimized: &[bool]) -> Self {
        let mut feed = Feed::new(1);
        let (mut state, _) = run(TilingState::new(""), vec![
            Input::Displays(Arc::new(display())),
            Input::Start(feed.stamp),
            Input::Applications {
                stamp: feed.stamp,
                apps: vec![info(6, "dev.zed.Zed", "Zed")],
            },
            Input::Completion {
                stamp: feed.stamp,
                result: CompletionResult::Track(
                    app(6),
                    stache_macos::windows::TrackOutcome::Tracking,
                ),
            },
        ]);
        let (next, _) = step(state, feed.complete(app(6)));
        state = next;
        let (next, _) = step(state, Input::Config { revision: 1, config });
        state = next;
        let mut windows = Vec::new();
        let mut zed_frames = std::collections::BTreeMap::new();
        for (index, frame) in frames.iter().enumerate() {
            let serial = u64::try_from(index).expect("index") + 1;
            let token = feed.token(6, serial);
            let mut snap = snapshot(token, "zed", u32::try_from(serial).expect("serial") + 30);
            snap.frame = read(*frame);
            snap.minimized = AxReading::Value(minimized.get(index).copied().unwrap_or(false));
            let input = feed.input(stache_macos::windows::WindowEventKind::Snapshot {
                snapshot: snap,
                origin: stache_macos::windows::SnapshotOrigin::Created,
            });
            let (next, _) = step(state, input);
            state = next;
            windows.push(token);
            zed_frames.insert(token, *frame);
        }
        assert_eq!(state.phase(), Phase::Complete);
        Self {
            state,
            feed,
            windows,
            zed: Zed {
                frames: zed_frames,
                artifact: std::collections::BTreeMap::new(),
                minimum: std::collections::BTreeMap::new(),
                animated_reads_before: false,
            },
            log: Vec::new(),
        }
    }

    fn apply(&mut self, input: Input) -> Vec<Effect> {
        let state = std::mem::replace(&mut self.state, TilingState::new(""));
        let (next, effects) = step(state, input);
        self.state = next;
        effects
    }

    /// Answers every write (and the writes those answers cause) until nothing
    /// is left, at most `limit` writes. Returns the number answered.
    fn settle(&mut self, effects: &[Effect], limit: usize) -> usize {
        let mut queue: std::collections::VecDeque<Sent> = sent(effects).into();
        let mut answered = 0;
        while let Some(write) = queue.pop_front() {
            // A write replaced by a newer plan for the same window is cancelled.
            if queue.iter().any(|later| later.window == write.window) {
                continue;
            }
            answered += 1;
            assert!(answered <= limit, "write loop: {:#?}", self.log);
            self.log.push(write);
            let outcome = self.zed.answer(&write);
            let stamp = self.feed.stamp;
            let revision = self.state.config_revision;
            let more = self.apply(Input::Completion {
                stamp,
                result: CompletionResult::Frame {
                    window: write.window,
                    plan_sequence: write.plan,
                    revision,
                    outcome,
                },
            });
            queue.extend(sent(&more));
        }
        answered
    }

    /// A read of `window` (as after a moved/resized notification).
    fn observe(&mut self, window: WindowInstance, frame: Rect) -> Vec<Effect> {
        let mut snap = snapshot(window, "zed", 31);
        snap.frame = read(frame);
        let stamp = self.feed.stamp;
        self.apply(Input::Completion {
            stamp,
            result: CompletionResult::Snapshot(
                window,
                stache_macos::windows::SnapshotOutcome::Snapshot(Box::new(snap)),
            ),
        })
    }

    fn frame(&self, index: usize) -> Rect { self.zed.frames[&self.windows[index]] }

    fn inferred(&self, index: usize) -> Option<crate::minimum_size::Size> {
        self.state
            .frames
            .records
            .get(&self.windows[index])
            .and_then(|record| record.minimum.inferred)
    }

    fn target(&self, index: usize) -> Option<Rect> {
        self.state.frames.desired.get(&self.windows[index]).copied()
    }

    /// The planned targets of the first three windows are the 50/25/25 grid.
    fn assert_grid_targets(&self) {
        assert_near(self.target(0).expect("master target"), MASTER);
        assert_near(self.target(1).expect("top target"), TOP);
        assert_near(self.target(2).expect("bottom target"), BOTTOM);
    }

    /// Writes after the first one to `index` are at most one direct size-only
    /// corrective write.
    fn assert_at_most_one_corrective(&self, index: usize) {
        let writes: Vec<_> = self.log.iter().filter(|w| w.window == self.windows[index]).collect();
        assert!(writes.len() <= 2, "{:#?}", self.log);
        assert!(
            writes
                .iter()
                .skip(1)
                .all(|w| !w.components.position && w.components.size && !w.animate),
            "{writes:#?}"
        );
    }

    /// Later reads of the current frames start no write.
    fn assert_quiet(&mut self, count: usize) {
        for _ in 0..2 {
            for index in 0..count {
                let frame = self.frame(index);
                let window = self.windows[index];
                let effects = self.observe(window, frame);
                assert!(sent(&effects).is_empty(), "{effects:?}");
            }
        }
    }

    fn writes_to(&self, index: usize) -> usize {
        self.log.iter().filter(|w| w.window == self.windows[index]).count()
    }
}

fn assert_near(actual: Rect, expected: Rect) {
    let close = |a: f64, b: f64| (a - b).abs() <= 1.0;
    assert!(
        close(actual.x, expected.x)
            && close(actual.y, expected.y)
            && close(actual.width, expected.width)
            && close(actual.height, expected.height),
        "{actual:?} != {expected:?}"
    );
}

#[test]
fn three_window_grid_is_half_width_master_and_two_quarter_cells() {
    let ids = [1_u8, 2, 3];
    let usable = Rect::new(14.0, 14.0, 1892.0, 1172.0);
    let gaps = crate::layout::Gaps {
        inner_h: 14.0,
        inner_v: 14.0,
        ..crate::layout::Gaps::default()
    };
    let frames = crate::layout::grid_layout(&ids, &usable, &gaps, &[]);
    assert_near(frames[0].1, MASTER);
    assert_near(frames[1].1, TOP);
    assert_near(frames[2].1, BOTTOM);
}

/// Run 10, part 2: a fresh grid start. "dotfiles" (the top-right cell) reads
/// back 1206 wide after the write that moved it right and shrank it; a
/// size-only write then lands. No minimum may be learned from that readback.
#[test]
fn a_transient_readback_after_a_move_and_resize_teaches_no_minimum() {
    let mut world = World::new(
        "grid",
        &[
            Rect::new(215.0, 607.0, 1690.0, 579.0),
            Rect::new(215.0, 14.0, 1690.0, 579.0),
            Rect::new(14.0, 14.0, 360.0, 1172.0),
        ],
        &[],
    );
    let dotfiles = world.windows[1];
    world.zed.artifact.insert(dotfiles, Rect::new(967.0, 14.0, 1206.0, 579.0));
    let effects = world.apply(Input::FrameWrites(true));
    world.settle(&effects, 12);

    assert_near(world.frame(0), MASTER);
    assert_near(world.frame(1), TOP);
    assert_near(world.frame(2), BOTTOM);
    assert_eq!(
        world.inferred(1),
        None,
        "no false minimum from the transient width"
    );
    // One write, plus at most one corrective size-only write for dotfiles.
    assert_eq!(world.writes_to(0), 1);
    assert!(world.writes_to(1) <= 2, "{:#?}", world.log);
    assert_eq!(world.writes_to(2), 1);
    let corrective: Vec<_> = world.log.iter().filter(|w| w.window == dotfiles).skip(1).collect();
    assert!(
        corrective.iter().all(|w| !w.components.position && w.components.size),
        "{corrective:?}"
    );
}

/// Run 10, part 1: `coding` switched live from monocle to grid by a reload,
/// five Zed windows of which two are minimized. Both stack windows read back
/// transient widths (1206 and 1471) after their move-and-resize write.
#[test]
fn a_live_monocle_to_grid_switch_settles_on_the_even_grid_without_a_fight() {
    let mut world = World::new("monocle", &[MONOCLE; 5], &[false, false, false, true, true]);
    let effects = world.apply(Input::FrameWrites(true));
    world.settle(&effects, 10);
    assert!(world.log.is_empty(), "monocle frames already hold");

    world
        .zed
        .artifact
        .insert(world.windows[1], Rect::new(967.0, 14.0, 1206.0, 1172.0));
    world
        .zed
        .artifact
        .insert(world.windows[2], Rect::new(967.0, 607.0, 1471.0, 617.0));
    let effects = world.apply(Input::Config {
        revision: 2,
        config: config("grid"),
    });
    world.settle(&effects, 12);

    assert_near(world.frame(0), MASTER);
    assert_near(world.frame(1), TOP);
    assert_near(world.frame(2), BOTTOM);
    assert_eq!(world.inferred(1), None);
    assert_eq!(world.inferred(2), None);
    assert_eq!(
        world.writes_to(0),
        1,
        "the master is written once: {:#?}",
        world.log
    );
    assert!(
        world.writes_to(1) <= 2 && world.writes_to(2) <= 2,
        "{:#?}",
        world.log
    );
    assert_eq!(
        world.writes_to(3) + world.writes_to(4),
        0,
        "minimized windows get no cell"
    );
    assert!(
        world.log.iter().all(|w| w.target.width >= ZED_MIN_WIDTH - 1.0),
        "nothing is squeezed below Zed's real minimum: {:#?}",
        world.log
    );

    // Later reads of the settled frames start nothing.
    for index in 0..3 {
        let frame = world.frame(index);
        let window = world.windows[index];
        let effects = world.observe(window, frame);
        assert!(sent(&effects).is_empty(), "{effects:?}");
    }
}

/// A genuine refusal: the size-only re-write still reads 1206. The minimum
/// is learned, but grid cells stay fixed (intentional deviation from legacy):
/// the window overflows its cell and is not written again.
#[test]
fn a_genuine_refusal_keeps_the_grid_and_goes_quiet() {
    let mut world = World::new(
        "grid",
        &[
            Rect::new(215.0, 607.0, 1690.0, 579.0),
            Rect::new(215.0, 14.0, 1690.0, 579.0),
            Rect::new(14.0, 14.0, 360.0, 1172.0),
        ],
        &[],
    );
    let dotfiles = world.windows[1];
    world.zed.minimum.insert(dotfiles, 1206.0);
    let effects = world.apply(Input::FrameWrites(true));
    world.settle(&effects, 12);

    let inferred = world.inferred(1).expect("a real refusal is learned");
    assert!((inferred.width - 1206.0).abs() < 1.0, "{inferred:?}");
    world.assert_grid_targets();
    assert_near(world.frame(0), MASTER);
    assert_near(world.frame(2), BOTTOM);
    assert_eq!(world.writes_to(0), 1, "{:#?}", world.log);
    assert_eq!(world.writes_to(2), 1, "{:#?}", world.log);
    assert_eq!(world.writes_to(1), 2, "one corrective write: {:#?}", world.log);
    world.assert_at_most_one_corrective(1);
    world.assert_quiet(3);
    assert_eq!(world.writes_to(1), 2);
}

/// The logged run 10 sequence (build 6d20b5d) with animations on: the stack
/// windows read back 1206 and 1204 after the move-and-resize write and after
/// the corrective write. The grid targets stay 50/25/25, the corrective write
/// is one direct size-only write, the master is never squeezed, and nothing
/// more is written on later reads.
#[test]
fn the_logged_refusal_sequence_keeps_the_fifty_twenty_five_grid() {
    let mut world = World::with(
        config_with("grid", true),
        &[
            Rect::new(215.0, 607.0, 1690.0, 579.0),
            Rect::new(702.0, 14.0, 1206.0, 579.0),
            Rect::new(14.0, 14.0, 360.0, 1172.0),
        ],
        &[],
    );
    world.zed.animated_reads_before = true;
    world.zed.minimum.insert(world.windows[1], 1206.0);
    world.zed.minimum.insert(world.windows[2], 1204.0);
    let effects = world.apply(Input::FrameWrites(true));
    world.settle(&effects, 12);

    world.assert_grid_targets();
    assert_near(world.frame(0), MASTER);
    assert!(
        world
            .log
            .iter()
            .all(|w| w.window != world.windows[0] || w.target.width >= 938.0),
        "the master is never squeezed: {:#?}",
        world.log
    );
    for index in 0..3 {
        world.assert_at_most_one_corrective(index);
    }
    world.assert_quiet(3);
    world.assert_grid_targets();
}

/// Five Zed windows, two minimized: the same 3-window grid, and a refusing
/// window does not change it.
#[test]
fn five_windows_with_two_minimized_hold_the_three_window_grid() {
    let mut world = World::new(
        "grid",
        &[
            Rect::new(215.0, 607.0, 1690.0, 579.0),
            Rect::new(215.0, 14.0, 1690.0, 579.0),
            Rect::new(14.0, 14.0, 360.0, 1172.0),
            MONOCLE,
            MONOCLE,
        ],
        &[false, false, false, true, true],
    );
    world.zed.minimum.insert(world.windows[2], 1204.0);
    let effects = world.apply(Input::FrameWrites(true));
    world.settle(&effects, 12);
    world.assert_grid_targets();
    assert_eq!(world.writes_to(3) + world.writes_to(4), 0);
    for index in 0..3 {
        world.assert_at_most_one_corrective(index);
    }
    world.assert_quiet(3);
}

/// Live monocle to grid with refusing stack windows: same grid, no fight.
#[test]
fn a_live_monocle_to_grid_switch_with_refusals_holds_the_grid() {
    let mut world = World::new("monocle", &[MONOCLE; 3], &[]);
    let effects = world.apply(Input::FrameWrites(true));
    world.settle(&effects, 10);
    world.zed.minimum.insert(world.windows[1], 1206.0);
    world.zed.minimum.insert(world.windows[2], 1204.0);
    let effects = world.apply(Input::Config {
        revision: 2,
        config: config("grid"),
    });
    world.settle(&effects, 12);
    world.assert_grid_targets();
    assert_near(world.frame(0), MASTER);
    for index in 0..3 {
        world.assert_at_most_one_corrective(index);
    }
    world.assert_quiet(3);
}

/// A window that accepted its size and later grows back past it on its own
/// (Zed re-asserting 360 after taking 187.8) teaches its minimum once; it is
/// not written back to the size it rejected, and the grid cell stays fixed.
#[test]
fn a_window_that_grows_back_after_an_applied_size_is_not_fought() {
    let mut world = World::new(
        "grid",
        &[
            Rect::new(215.0, 607.0, 1690.0, 579.0),
            Rect::new(215.0, 14.0, 1690.0, 579.0),
            Rect::new(14.0, 14.0, 360.0, 1172.0),
        ],
        &[],
    );
    let effects = world.apply(Input::FrameWrites(true));
    world.settle(&effects, 12);
    assert_near(world.frame(0), MASTER);
    let before = world.log.len();

    // The master grows back to 1100 wide by itself, more than once.
    let window = world.windows[0];
    let grown = Rect::new(14.0, 14.0, 1100.0, 1172.0);
    world.zed.frames.insert(window, grown);
    world.zed.minimum.insert(window, 1100.0);
    for _ in 0..4 {
        let effects = world.observe(window, world.zed.frames[&window]);
        world.settle(&effects, 8);
    }
    let master_writes = world.log[before..].iter().filter(|w| w.window == window);
    assert!(
        master_writes.clone().all(|w| w.target.width >= 1099.0),
        "never written back below what it grew to: {:#?}",
        world.log
    );
    assert_eq!(
        world.log.len(),
        before,
        "no write at all: {:#?}",
        &world.log[before..]
    );
    world.assert_grid_targets();
    assert!(world.inferred(0).is_some_and(|s| (s.width - 1100.0).abs() < 1.0));
}

/// A write that was never sent (it ended `NotIssued` because a reload bumped
/// the revision) is retried under the new revision even when the window has a
/// learned minimum above its cell: only a write that was really sent and
/// refused may be skipped.
#[test]
fn an_unsent_write_is_retried_despite_a_learned_minimum() {
    let mut world = World::new(
        "grid",
        &[
            Rect::new(215.0, 607.0, 1690.0, 579.0),
            Rect::new(215.0, 14.0, 1690.0, 579.0),
            Rect::new(14.0, 14.0, 360.0, 1172.0),
        ],
        &[],
    );
    let dotfiles = world.windows[1];
    world.zed.minimum.insert(dotfiles, 1206.0);
    let effects = world.apply(Input::FrameWrites(true));
    world.settle(&effects, 12);
    assert!(world.inferred(1).is_some());

    // The window was moved away; the latest write for its (same) cell ended
    // NotIssued under revision 1 and the configuration is now at revision 2.
    world.zed.frames.insert(dotfiles, Rect::new(300.0, 300.0, 1206.0, 579.0));
    world.zed.minimum.remove(&dotfiles);
    let record = world.state.frames.records.get_mut(&dotfiles).expect("record");
    let attempt = record.attempt.as_mut().expect("attempt");
    attempt.revision = 1;
    attempt.status = AttemptStatus::Settled(crate::outcome::OperationOutcome::NotAttempted {
        reason: crate::outcome::NotAttemptedReason::RevisionChanged,
        window: dotfiles,
        plan_sequence: attempt.plan_sequence,
    });
    world.state.config_revision = 2;
    let before = world.writes_to(1);
    let frame = world.frame(1);
    let effects = world.observe(dotfiles, frame);
    world.settle(&effects, 6);
    assert_eq!(world.writes_to(1) - before, 1, "{:#?}", world.log);
}

fn settled_grid() -> World {
    let mut world = World::new(
        "grid",
        &[
            Rect::new(215.0, 607.0, 1690.0, 579.0),
            Rect::new(215.0, 14.0, 1690.0, 579.0),
            Rect::new(14.0, 14.0, 360.0, 1172.0),
        ],
        &[],
    );
    let effects = world.apply(Input::FrameWrites(true));
    world.settle(&effects, 12);
    assert_near(world.frame(0), MASTER);
    world
}

/// Without the passive drag observer a user resize is invisible: a window
/// growing past its applied size teaches no minimum.
#[test]
fn growth_teaches_no_minimum_while_the_drag_observer_is_degraded() {
    let mut world = settled_grid();
    world.apply(Input::Drag(crate::drag::DragSignal::Unavailable));
    let window = world.windows[0];
    let grown = Rect::new(14.0, 14.0, 1100.0, 1172.0);
    world.zed.frames.insert(window, grown);
    world.observe(window, grown);
    assert_eq!(world.inferred(0), None);
}

/// A window zoomed or maximized to the usable area of its display is not
/// re-asserting a minimum: nothing is learned.
#[test]
fn growth_to_the_usable_area_teaches_no_minimum() {
    let mut world = settled_grid();
    let window = world.windows[0];
    world.zed.frames.insert(window, MONOCLE);
    world.observe(window, MONOCLE);
    assert_eq!(world.inferred(0), None);
}
