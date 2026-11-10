//! Display-clock pacing, the timer fallback, clock lifecycle and stop safety,
//! and the per-stage components of the size envelope through the coordinator.
//! Fake frame clocks only: no test here creates a display link.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use stache_macos::windows::{FrameStage, IssuedKind, NativeStatus, WindowActionOutcome};
use stache_macos::{DisplayId, FakeFrameClockFactory, FrameClockFactory};
use stache_sdk::CancellationToken;

use super::coordinator_tests::{FakeSink, Rig, wait_for};
use super::pacing::{ClockSet, Notes};
use super::tests::{FROM, TO, job, window_in, written};
use super::*;
use crate::actor::{Clock, SystemClock};
use crate::config::{AnimationConfig, EasingType};
use crate::geometry::Rect;
use crate::outcome::FrameComponents;

const D1: DisplayId = DisplayId::new(1);
const D2: DisplayId = DisplayId::new(2);
const MS: Duration = Duration::from_millis(1);

fn on(display: Option<DisplayId>, mut job: AnimationJob) -> AnimationJob {
    job.display = display;
    job
}

fn clocked(rig: &mut Rig, displays: &[DisplayId]) {
    rig.animator.set_clocked(displays.iter().copied().collect());
}

fn near(a: f64, b: f64) -> bool { (a - b).abs() < 1e-6 }

// ------------------------------------------------------- coordinator pacing

#[test]
fn a_clocked_job_waits_for_its_tick_and_computes_the_tick_timestamp() {
    let mut rig = Rig::new();
    clocked(&mut rig, &[D1]);
    let start = rig.now;
    rig.admit(on(Some(D1), job(1, FROM, TO, EasingType::Linear)));
    assert!(rig.sink.sent().is_empty(), "no tick, no frame");
    // The coordinator is late: it runs 40 ms in, for a tick stamped 30 ms in.
    rig.now = start + 40 * MS;
    rig.ticks.insert(D1, start + 30 * MS);
    rig.pump();
    let sent = rig.sink.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].stage, FrameStage::First);
    assert!(
        near(sent[0].frame.x, 150.0),
        "30/200 of the way: {:?}",
        sent[0].frame
    );
    // The same tick is not used twice.
    rig.pump();
    assert_eq!(rig.sink.sent().len(), 1);
}

#[test]
fn two_displays_tick_independently_and_send_one_batch_per_application() {
    let mut rig = Rig::new();
    clocked(&mut rig, &[D1, D2]);
    let t0 = rig.now;
    let (w1, w2, w3) = (window_in(1, 1), window_in(1, 2), window_in(2, 1));
    for (plan, window, display) in [(1, w1, D1), (2, w2, D2), (3, w3, D2)] {
        let mut j = on(Some(display), job(plan, FROM, TO, EasingType::Linear));
        j.window = window;
        rig.admit(j);
    }
    rig.now = t0 + 30 * MS;
    rig.ticks.insert(D1, t0 + 20 * MS);
    rig.ticks.insert(D2, t0 + 25 * MS);
    rig.pump();
    let batches = rig.sink.batches();
    assert_eq!(batches.len(), 2, "one batch per application: {batches:?}");
    let app1 = batches.iter().find(|(a, _)| *a == w1.application()).expect("app 1");
    assert_eq!(
        app1.1.len(),
        2,
        "both displays' windows of one application together"
    );
    let sent = rig.sink.sent();
    let x_of = |w| sent.iter().find(|s| s.window == w).expect("sent").frame.x;
    assert!(near(x_of(w1), 100.0), "own tick: 20/200");
    assert!(near(x_of(w2), 125.0), "own tick: 25/200");
    assert!(near(x_of(w3), 125.0), "same tick as the other window of D2");

    // Only the faster display ticks next: the other display's window waits.
    rig.now = t0 + 50 * MS;
    rig.ticks.insert(D2, t0 + 45 * MS);
    rig.pump();
    let sent = rig.sink.sent();
    assert_eq!(sent.iter().filter(|s| s.window == w1).count(), 1);
    assert_eq!(sent.iter().filter(|s| s.window == w2).count(), 2);
    assert_eq!(sent.iter().filter(|s| s.window == w3).count(), 2);
}

#[test]
fn several_ticks_before_the_coordinator_runs_give_one_frame_not_a_burst() {
    let factory = FakeFrameClockFactory::new();
    let mut held_clocks = ClockSet::new(
        Arc::new(factory.clone()) as Arc<dyn FrameClockFactory>,
        Arc::new(|| {}),
        Arc::new(Notes::default()),
    );
    assert_eq!(
        held_clocks.sync(&BTreeSet::from([D1]), false),
        BTreeSet::from([D1])
    );
    let mut rig = Rig::new();
    clocked(&mut rig, &[D1]);
    let t0 = rig.now;
    rig.admit(on(Some(D1), job(1, FROM, TO, EasingType::Linear)));
    for n in 1..=5 {
        assert!(factory.fire(D1, t0 + 10 * n * MS));
    }
    rig.now = t0 + 80 * MS;
    rig.ticks = held_clocks.drain();
    assert_eq!(rig.ticks.len(), 1, "one slot per display");
    rig.pump();
    let sent = rig.sink.sent();
    assert_eq!(sent.len(), 1, "latest tick wins, no catch-up burst: {sent:?}");
    assert!(
        near(sent[0].frame.x, 250.0),
        "the latest tick (50 ms): {:?}",
        sent[0].frame
    );
    assert!(held_clocks.drain().is_empty());
}

#[test]
fn a_clock_that_stops_ticking_cannot_strand_a_job() {
    let mut rig = Rig::new();
    clocked(&mut rig, &[D1]);
    rig.admit(on(Some(D1), job(1, FROM, TO, EasingType::Linear)));
    // No tick ever arrives: the job is paced by its timer at the normal period
    // once the stall deadline passes (not one frame every few periods).
    let mut times = Vec::new();
    let mut seen = 0;
    for _ in 0..200 {
        if !rig.step() {
            break;
        }
        let count = rig.sink.sent().len();
        if count > seen {
            times.push(rig.now);
            seen = count;
        }
    }
    let sent = rig.sink.sent();
    assert_eq!(sent.last().expect("last").stage, FrameStage::Final, "{sent:?}");
    assert_eq!(rig.sink.completed().len(), 1);
    assert!(
        times.len() >= 8,
        "timer-rate frames after the stall: {}",
        times.len()
    );
    let gaps: Vec<_> = times.windows(2).skip(1).map(|w| w[1] - w[0]).collect();
    assert!(
        gaps.iter().all(|g| *g <= super::tests::PERIOD * 2),
        "frames keep the display period once stalled: {gaps:?}"
    );
}

#[test]
fn a_display_without_a_clock_is_paced_by_the_timer() {
    let mut rig = Rig::new();
    clocked(&mut rig, &[]);
    rig.admit(on(Some(D1), job(1, FROM, TO, EasingType::Linear)));
    assert!(rig.step());
    assert_eq!(
        rig.sink.sent().len(),
        1,
        "the timer sent the first frame one period in"
    );
    rig.run(200);
    assert!(rig.sink.sent().len() >= 8, "full-rate timer frames");
    assert_eq!(rig.sink.completed().len(), 1);
}

#[test]
fn only_running_displays_need_a_clock() {
    let mut rig = Rig::new();
    assert!(rig.animator.displays_needing_clock().is_empty());
    rig.admit(on(Some(D2), job(1, FROM, TO, EasingType::Linear)));
    assert_eq!(rig.animator.displays_needing_clock(), BTreeSet::from([D2]));
    assert!(!rig.animator.has_unplaced());
    rig.admit(on(None, {
        let mut j = job(2, FROM, TO, EasingType::Linear);
        j.window = window_in(2, 1);
        j
    }));
    assert!(rig.animator.has_unplaced());
    rig.run(200);
    assert!(
        rig.animator.displays_needing_clock().is_empty(),
        "settled jobs need none"
    );
}

// ----------------------------------------------------------- clock lifecycle

#[test]
fn clock_creation_failure_uses_the_timer_with_a_diagnostic_until_it_works() {
    let factory = FakeFrameClockFactory::new();
    factory.fail_creation(D2, "no display link");
    let notes = Arc::new(Notes::default());
    let mut held_clocks = ClockSet::new(
        Arc::new(factory.clone()) as Arc<dyn FrameClockFactory>,
        Arc::new(|| {}),
        Arc::clone(&notes),
    );
    let live = held_clocks.sync(&BTreeSet::from([D1, D2]), false);
    assert_eq!(live, BTreeSet::from([D1]), "D2 has no clock");
    assert_eq!(notes.list(), vec![
        "animation pacing: display 2 uses a timer: no display link".to_owned()
    ]);
    // The failure is not retried while the display keeps running jobs.
    held_clocks.sync(&BTreeSet::from([D1, D2]), false);
    assert_eq!(factory.attempts().iter().filter(|d| **d == D2).count(), 1);
    // Idle: the display has no job; the next first job retries and succeeds.
    held_clocks.sync(&BTreeSet::from([D1]), false);
    factory.allow_creation(D2);
    let live = held_clocks.sync(&BTreeSet::from([D1, D2]), false);
    assert_eq!(live, BTreeSet::from([D1, D2]));
    assert!(notes.list().is_empty(), "cleared once the clock works");
    // Idle owns zero clocks.
    held_clocks.sync(&BTreeSet::new(), false);
    assert_eq!(factory.live(), 0);
    assert_eq!(factory.stopped().len(), 2);
    // An unknown display is reported too.
    held_clocks.sync(&BTreeSet::new(), true);
    assert_eq!(notes.list(), vec![
        "animation pacing: display unknown uses a timer: the target display is unknown".to_owned()
    ]);
    held_clocks.sync(&BTreeSet::new(), false);
    assert!(notes.list().is_empty());
}

fn control(sink: &Arc<FakeSink>, factory: &FakeFrameClockFactory) -> AnimationControl {
    AnimationControl::new(
        Arc::clone(sink) as Arc<dyn AnimationSink>,
        Arc::new(SystemClock) as Arc<dyn Clock>,
        Arc::new(factory.clone()) as Arc<dyn FrameClockFactory>,
        CancellationToken::new(),
    )
}

fn enabled() -> AnimationConfig {
    AnimationConfig {
        enabled: true,
        duration: 200,
        easing: EasingType::Linear,
    }
}

/// Fires `displays` every few milliseconds until `done`.
fn drive(factory: &FakeFrameClockFactory, displays: &[DisplayId], mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out driving the clocks");
        for display in displays {
            factory.fire(*display, Instant::now());
        }
        std::thread::sleep(4 * MS);
    }
}

fn quick(plan: u64, window: usize, display: Option<DisplayId>) -> AnimationJob {
    let mut j = on(display, job(plan, FROM, TO, EasingType::Linear));
    j.window = window_in(1, window as u64);
    j
}

#[test]
fn a_worker_owns_a_clock_only_while_its_display_has_running_jobs() {
    let sink = FakeSink::new();
    let factory = FakeFrameClockFactory::new();
    let control = control(&sink, &factory);
    control.configure(&AnimationConfig::default()).expect("disabled");
    assert!(factory.attempts().is_empty(), "disabled owns no clock");
    control.configure(&enabled()).expect("enabled");
    std::thread::sleep(20 * MS);
    assert!(factory.attempts().is_empty(), "idle owns no clock");

    control.submit(quick(1, 1, Some(D1))).expect("admitted");
    wait_for("the clock", || factory.is_live(D1));
    drive(&factory, &[D1], || sink.completed().len() == 1);
    wait_for("the clock to be released", || factory.live() == 0);
    assert_eq!(factory.created(), vec![D1]);
    assert_eq!(factory.stopped(), vec![D1]);
    let sent = sink.sent();
    assert_eq!(sent.first().expect("first").stage, FrameStage::First);
    assert_eq!(sent.last().expect("last").stage, FrameStage::Final);
    assert!(sink.batches().len() >= 3);
    control.stop();
    assert!(control.join_within(Duration::from_secs(5)));
    assert!(control.release_clocks(Duration::from_secs(1)));
}

#[test]
fn a_creation_failure_on_one_display_paces_it_by_the_timer_and_reports_it() {
    let sink = FakeSink::new();
    let factory = FakeFrameClockFactory::new();
    factory.fail_creation(D2, "no link");
    let control = control(&sink, &factory);
    control.configure(&enabled()).expect("enabled");
    control.submit(quick(1, 1, Some(D1))).expect("admitted");
    control.submit(quick(2, 2, Some(D2))).expect("admitted");
    control.submit(quick(3, 3, None)).expect("admitted");
    // Only D1 is driven; the D2 and unknown-display jobs run on their timers.
    drive(&factory, &[D1], || sink.completed().len() == 3);
    let notes = control.pacing_diagnostics();
    assert!(
        notes.contains(&"animation pacing: display 2 uses a timer: no link".to_owned()),
        "{notes:?}"
    );
    wait_for("the unknown-display note to clear", || {
        control.pacing_diagnostics().len() == 1
    });
    control.stop();
    assert!(control.join_within(Duration::from_secs(5)));
}

#[test]
fn stopping_with_a_clock_running_stops_and_frees_it() {
    let sink = FakeSink::new();
    let factory = FakeFrameClockFactory::new();
    let control = control(&sink, &factory);
    control.configure(&enabled()).expect("enabled");
    control.submit(quick(1, 1, Some(D1))).expect("admitted");
    wait_for("the clock", || factory.live() == 1);
    control.stop();
    assert!(control.join_within(Duration::from_secs(5)));
    assert_eq!(factory.live(), 0);
    assert_eq!(factory.stopped(), vec![D1]);
    assert_eq!(control.stuck_clocks(), 0);
    assert!(
        !factory.fire(D1, Instant::now()),
        "no clock, no callback after the stop"
    );
}

#[test]
fn a_clock_that_cannot_stop_is_quarantined_and_retried() {
    let sink = FakeSink::new();
    let factory = FakeFrameClockFactory::new();
    factory.set_stuck(D1, true);
    let control = control(&sink, &factory);
    control.configure(&enabled()).expect("enabled");
    control.submit(quick(1, 1, Some(D1))).expect("admitted");
    wait_for("the clock", || factory.live() == 1);
    control.stop();
    assert!(
        control.join_within(Duration::from_secs(5)),
        "the worker itself ends"
    );
    assert_eq!(control.stuck_clocks(), 1, "kept, not leaked or detached");
    assert_eq!(factory.live(), 1);
    assert!(!control.release_clocks(30 * MS), "still stuck");
    assert_eq!(control.stuck_clocks(), 1);
    factory.set_stuck(D1, false);
    assert!(control.release_clocks(Duration::from_secs(1)));
    assert_eq!(control.stuck_clocks(), 0);
    assert_eq!(factory.live(), 0);
}

#[test]
fn a_held_callback_delays_the_stop_and_nothing_runs_after_the_release() {
    let sink = FakeSink::new();
    let factory = FakeFrameClockFactory::new();
    let control = control(&sink, &factory);
    control.configure(&enabled()).expect("enabled");
    control.submit(quick(1, 1, Some(D1))).expect("admitted");
    wait_for("the clock", || factory.live() == 1);
    let held = factory.fire_held(D1, Instant::now()).expect("a clock");
    held.wait_entered();
    control.stop();
    assert!(control.join_within(Duration::from_secs(5)));
    assert_eq!(factory.live(), 1, "a callback in flight keeps the clock owned");
    assert_eq!(control.stuck_clocks(), 1);
    held.release();
    assert!(control.release_clocks(Duration::from_secs(1)));
    assert_eq!(factory.live(), 0);
    assert!(!factory.fire(D1, Instant::now()));
}

// --------------------------------------------------- stage components (size)

fn run_with(from: Rect, to: Rect, cross: bool) -> Rig {
    let mut rig = Rig::new();
    let mut j = job(1, from, to, EasingType::Linear);
    j.cross_display = cross;
    rig.admit(j);
    rig.run(300);
    rig
}

fn comps(position: bool, size: bool) -> FrameComponents { FrameComponents { position, size } }

/// The frames of a finished animation: every first and intermediate frame
/// writes the position and, when `resizes`, the interpolated size (moving
/// monotonically from `from` to `to`, strictly between them); the final frame
/// writes the exact target, size included.
fn assert_sizes(rig: &Rig, from: Rect, to: Rect, resizes: bool) {
    let sent = rig.sink.sent();
    assert!(sent.len() >= 3, "{sent:?}");
    assert_eq!(sent[0].stage, FrameStage::First);
    let (last, body) = sent.split_last().expect("frames");
    for frame in body {
        assert_eq!(frame.components, comps(true, resizes), "{frame:?}");
    }
    assert_eq!(last.stage, FrameStage::Final);
    assert_eq!(last.components, comps(true, true), "final: {last:?}");
    assert_eq!(last.frame, to, "the final frame is the exact target");
    let widths: Vec<f64> = body.iter().map(|f| f.frame.width).collect();
    let heights: Vec<f64> = body.iter().map(|f| f.frame.height).collect();
    for (values, start, end) in [
        (widths, from.width, to.width),
        (heights, from.height, to.height),
    ] {
        let (low, high) = (start.min(end), start.max(end));
        let step = if end >= start { 1.0 } else { -1.0 };
        assert!(
            values.windows(2).all(|w| (w[1] - w[0]) * step >= 0.0),
            "monotonic: {values:?}"
        );
        if resizes && (end - start).abs() > 0.5 {
            assert!(values.len() >= 2, "several size writes: {values:?}");
            assert!(
                values.iter().all(|v| *v >= low && *v <= high),
                "between start and target: {values:?}"
            );
            assert!(
                values.iter().any(|v| (*v - end).abs() > 0.5),
                "not snapped to the target at once"
            );
        }
    }
    if !resizes {
        assert!(body.iter().all(|f| (f.frame.width, f.frame.height) == (to.width, to.height)));
    }
}

#[test]
fn growth_animates_the_size_on_every_frame_and_the_final_frame_is_exact() {
    let to = Rect::new(1000.0, 0.0, 300.0, 200.0);
    let rig = run_with(FROM, to, false);
    assert_sizes(&rig, FROM, to, true);
    let sent = rig.sink.sent();
    assert!(
        sent[0].frame.width < 300.0,
        "the first frame is not the target size"
    );
    assert_eq!(rig.sink.completed().len(), 1);
}

#[test]
fn shrink_animates_the_size_on_every_frame_and_the_final_frame_is_exact() {
    let from = Rect::new(0.0, 0.0, 300.0, 200.0);
    let to = Rect::new(1000.0, 0.0, 100.0, 100.0);
    let rig = run_with(from, to, false);
    assert_sizes(&rig, from, to, true);
}

#[test]
fn mixed_axes_interpolate_each_axis_toward_its_target() {
    let from = Rect::new(0.0, 0.0, 100.0, 300.0);
    let to = Rect::new(1000.0, 0.0, 300.0, 100.0);
    let rig = run_with(from, to, false);
    assert_sizes(&rig, from, to, true);
    let sent = rig.sink.sent();
    assert!(
        sent[1].frame.width > 100.0 && sent[1].frame.height < 300.0,
        "{sent:?}"
    );
}

#[test]
fn equal_sizes_are_written_only_by_the_final_frame() {
    let rig = run_with(FROM, TO, false);
    assert_sizes(&rig, FROM, TO, false);
}

#[test]
fn a_cross_display_move_animates_the_size_like_any_other() {
    let grow = Rect::new(1000.0, 0.0, 300.0, 200.0);
    let rig = run_with(FROM, grow, true);
    assert_sizes(&rig, FROM, grow, true);
    let shrink_from = Rect::new(0.0, 0.0, 300.0, 200.0);
    let shrink = Rect::new(1000.0, 0.0, 100.0, 100.0);
    let rig = run_with(shrink_from, shrink, true);
    assert_sizes(&rig, shrink_from, shrink, true);
}

#[test]
fn an_instant_job_writes_exactly_the_wanted_components() {
    let to = Rect::new(2.0, 0.0, 102.0, 100.0);
    let rig = run_with(FROM, to, false);
    let sent = rig.sink.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].stage, FrameStage::Final);
    assert_eq!(sent[0].components, crate::animation::tests::BOTH);
    assert_eq!(sent[0].frame, to);
}

#[test]
fn a_refused_size_at_the_final_frame_ends_the_job_with_that_outcome() {
    let from = Rect::new(0.0, 0.0, 300.0, 200.0);
    let to = Rect::new(1000.0, 0.0, 100.0, 100.0);
    let unresizable = WindowActionOutcome::Issued {
        kind: IssuedKind::Rejected,
        native: NativeStatus::Frame {
            position: stache_macos::ax::WriteStatus::Written,
            size: stache_macos::ax::WriteStatus::Failed(stache_macos::ax::AxError::CannotComplete),
        },
        observed: None,
    };
    let mut rig = Rig::new();
    // Every frame before the final one is accepted; the final size is refused.
    rig.sink.script(std::iter::repeat_n(written(), 5).chain([unresizable]));
    rig.admit(job(1, from, to, EasingType::Linear));
    rig.sink.script([]);
    rig.run(300);
    let done = rig.sink.completed();
    assert_eq!(done.len(), 1);
    let sent = rig.sink.sent();
    assert_eq!(sent.len(), 6, "{sent:?}");
    assert_eq!(done[0].1, unresizable, "no retry and no snap");
}
