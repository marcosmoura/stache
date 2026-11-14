//! The coordinator over the real sink, issuance control and the fake shard
//! backend (a per-application latest-frame-wins mailbox that can hold, refuse,
//! partially apply or delay writes). Fake clock; nothing touches AX.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use stache_macos::ApplicationId;
use stache_macos::windows::{
    DispatchError, FrameStage, GenerationCell, IssuedKind, NativeStatus, NotIssuedReason,
    RuntimeEpoch, WindowActionOutcome, WindowInstance,
};
use stache_sdk::CancellationToken;

use super::coordinator_tests::{Completed, Driver};
use super::tests::{BOTH, FROM, PERIOD, TO, app, job, written};
use super::*;
use crate::actor::{FrameControl, ManualClock};
use crate::backend::{FakeBackendCall, FakeWindowBackend, WindowBackend};
use crate::config::EasingType;
use crate::events::{GeometryCoalescer, Ingress, Pipeline};
use crate::geometry::Rect;
use crate::outcome::{OperationOutcome, classify_frame};
use crate::state::Stamp;

/// The live sink, with its completions recorded.
struct Tee {
    live: LiveSink,
    completed: Mutex<Vec<Completed>>,
}

impl AnimationSink for Tee {
    fn submit_batch(
        &self,
        application: ApplicationId,
        frames: Vec<(AnimationFrame, stache_macos::windows::FrameReplyFn)>,
    ) {
        self.live.submit_batch(application, frames);
    }

    fn submit_settle(&self, job: &AnimationJob, reply: ReadReplyFn) {
        self.live.submit_settle(job, reply);
    }

    fn submit_retarget_read(&self, job: &AnimationJob, reply: ReadReplyFn) {
        self.live.submit_retarget_read(job, reply);
    }

    fn complete(&self, job: &AnimationJob, outcome: WindowActionOutcome, counters: FrameCounters) {
        self.completed
            .lock()
            .expect("completed")
            .push((job.plan_sequence, outcome, counters));
        self.live.complete(job, outcome, counters);
    }

    fn stopping(&self) -> Option<NotIssuedReason> { self.live.stopping() }
}

struct Pipe {
    backend: Arc<FakeWindowBackend>,
    gate: Arc<GenerationCell>,
    frames: Arc<FrameControl>,
    sink: Arc<Tee>,
    epoch: RuntimeEpoch,
    driver: Driver,
}

impl std::ops::Deref for Pipe {
    type Target = Driver;

    fn deref(&self) -> &Driver { &self.driver }
}

impl std::ops::DerefMut for Pipe {
    fn deref_mut(&mut self) -> &mut Driver { &mut self.driver }
}

impl Pipe {
    fn new() -> Self {
        let backend = Arc::new(FakeWindowBackend::default());
        backend.script_echo_frames(true);
        let gate = Arc::new(GenerationCell::new());
        gate.publish(1, 1);
        let epoch = backend
            .install(
                stache_macos::windows::EventQueue::new(8),
                Arc::clone(&gate),
                None,
            )
            .expect("install");
        let frames = Arc::new(FrameControl::new(
            Arc::clone(&gate),
            Arc::new(ManualClock::default()) as Arc<dyn crate::actor::Clock>,
        ));
        let ingress = Arc::new(Ingress::new(Pipeline::with_bounds(
            64,
            crate::events::COMPLETION_CAPACITY,
            GeometryCoalescer::default(),
        )));
        let live = LiveSink::new(
            Arc::clone(&backend) as Arc<dyn WindowBackend>,
            ingress,
            Arc::clone(&frames),
            CancellationToken::new(),
        );
        let sink = Arc::new(Tee {
            live,
            completed: Mutex::new(Vec::new()),
        });
        let driver = Driver::new(Arc::clone(&sink) as Arc<dyn AnimationSink>);
        Self {
            backend,
            gate,
            frames,
            sink,
            epoch,
            driver,
        }
    }

    fn window(&self, application: u64, serial: u64) -> WindowInstance {
        WindowInstance::for_testing(self.backend.adapter(), self.epoch, app(application), serial)
    }

    /// The `CGWindowID` the jobs of `window` carry.
    fn cg_of(window: WindowInstance) -> u32 { 500 + u32::try_from(window.serial()).unwrap_or(0) }

    /// A job for `window`, registered with the issuance ledger as the actor does
    /// at dispatch.
    fn start(&mut self, window: WindowInstance, plan: u64, from: Rect, target: Rect) {
        let mut j = job(plan, from, target, EasingType::Linear);
        j.window = window;
        // The window server answers only for an id a test scripts.
        j.cg_window_id = Some(Self::cg_of(window));
        j.stamp = Stamp { epoch: self.epoch, registry: 1 };
        self.frames.note_dispatched(window, plan);
        self.driver.admit(j);
    }

    fn completed(&self) -> Vec<Completed> { self.sink.completed.lock().expect("completed").clone() }
}

fn partial() -> WindowActionOutcome {
    WindowActionOutcome::Issued {
        kind: IssuedKind::Partial,
        native: NativeStatus::Unobserved,
        observed: None,
    }
}

#[test]
fn a_held_application_does_not_delay_others_and_its_replaced_frames_are_dropped() {
    let mut pipe = Pipe::new();
    let (slow, fast) = (pipe.window(1, 1), pipe.window(2, 1));
    let hold = pipe.backend.hold_application(app(1));
    pipe.start(slow, 1, FROM, TO);
    pipe.start(fast, 2, FROM, TO);
    for _ in 0..6 {
        pipe.step();
    }
    let sent = pipe.backend.sent_frames();
    let fast_frames = sent.iter().filter(|s| s.application == app(2)).count();
    let slow_frames = sent.iter().filter(|s| s.application == app(1)).count();
    assert_eq!(fast_frames, 6, "the other application got a frame every tick");
    assert_eq!(
        slow_frames, 1,
        "the held application is stuck inside its first send"
    );
    assert_eq!(
        pipe.backend.pending_frames(app(1)),
        1,
        "a slow application keeps one pending frame, not a queue"
    );
    assert_eq!(
        pipe.backend.replaced_frames(),
        4,
        "the four frames in between were replaced"
    );
    assert!(pipe.completed().is_empty());

    // Releasing lets the slow application catch up; it skipped frames.
    hold.release();
    pipe.run(200);
    let done = pipe.completed();
    assert_eq!(done.len(), 2);
    let slow_done = done.iter().find(|(plan, ..)| *plan == 1).expect("slow plan");
    assert!(
        slow_done.2.dropped >= 4,
        "dropped frames are counted: {:?}",
        slow_done.2
    );
    let fast_done = done.iter().find(|(plan, ..)| *plan == 2).expect("fast plan");
    assert_eq!(fast_done.2.dropped, 0);
    for (plan, window) in [(1, slow), (2, fast)] {
        let outcome = &done.iter().find(|(p, ..)| *p == plan).expect("done").1;
        assert!(
            matches!(
                classify_frame(window, plan, TO, BOTH, outcome),
                OperationOutcome::Applied { .. }
            ),
            "plan {plan}: {outcome:?}"
        );
    }
    assert_eq!(pipe.frames.outstanding(), 0);
}

#[test]
fn intermediate_frames_do_no_readback_and_the_final_does_exactly_one() {
    let mut pipe = Pipe::new();
    let window = pipe.window(1, 1);
    pipe.backend.seed_window_frame(window, FROM);
    pipe.start(window, 1, FROM, TO);
    pipe.run(100);
    let sent = pipe.backend.sent_frames();
    assert_eq!(sent.first().expect("first").stage, FrameStage::First);
    assert_eq!(sent.last().expect("last").stage, FrameStage::Final);
    assert!(sent[1..sent.len() - 1].iter().all(|s| s.stage == FrameStage::Intermediate));
    assert!(sent.iter().all(|s| s.stage != FrameStage::Direct));
    assert_eq!(
        pipe.backend.frame_reads(window),
        1,
        "one settle readback (frame only), nothing else"
    );
    assert_eq!(pipe.backend.snapshot_reads(window), 0, "never a full snapshot");
    let done = pipe.completed();
    assert_eq!(done.len(), 1);
    assert_eq!(
        classify_frame(window, 1, TO, BOTH, &done[0].1).actual(),
        Some(TO),
        "the settle readback decides the outcome"
    );
}

#[test]
fn a_retarget_through_the_shard_does_exactly_one_extra_readback() {
    let mut pipe = Pipe::new();
    let window = pipe.window(1, 1);
    pipe.start(window, 1, FROM, TO);
    for _ in 0..5 {
        pipe.step();
    }
    let back = Rect::new(0.0, 300.0, 100.0, 100.0);
    pipe.start(window, 2, FROM, back);
    // No window-server bounds here: the retarget reads the frame only.
    assert_eq!(pipe.backend.frame_reads(window), 1, "the retarget read");
    pipe.run(200);
    assert_eq!(pipe.backend.frame_reads(window), 2, "plus the one settle read");
    assert_eq!(pipe.backend.snapshot_reads(window), 0, "never a full snapshot");
    let done = pipe.completed();
    assert_eq!(done.iter().map(|(plan, ..)| *plan).collect::<Vec<_>>(), vec![
        1, 2
    ]);
    assert!(matches!(
        done[0].1,
        WindowActionOutcome::NotIssued(NotIssuedReason::Superseded)
            | WindowActionOutcome::Issued { .. }
    ));
    assert_eq!(
        classify_frame(window, 2, back, BOTH, &done[1].1).actual(),
        Some(back)
    );
}

#[test]
fn a_retarget_starts_from_the_window_server_without_a_read_of_the_application() {
    let mut pipe = Pipe::new();
    let window = pipe.window(1, 1);
    pipe.backend.seed_window_frame(window, FROM);
    pipe.backend.script_bounds(Pipe::cg_of(window), window);
    pipe.start(window, 1, FROM, TO);
    for _ in 0..5 {
        pipe.step();
    }
    let back = Rect::new(0.0, 300.0, 100.0, 100.0);
    pipe.start(window, 2, FROM, back);
    assert_eq!(
        pipe.backend.frame_reads(window),
        0,
        "answered by the window server"
    );
    assert!(pipe.backend.calls().contains(&FakeBackendCall::Bounds(Pipe::cg_of(window))));
    pipe.run(200);
    assert_eq!(pipe.backend.frame_reads(window), 1, "only the settle read");
    assert_eq!(pipe.backend.snapshot_reads(window), 0);
    let done = pipe.completed();
    assert_eq!(
        classify_frame(window, 2, back, BOTH, &done.last().expect("done").1).actual(),
        Some(back),
        "the settle read, not the window server, decides the outcome"
    );
}

#[test]
fn a_partially_applied_frame_ends_the_job_without_a_final_snap() {
    let mut pipe = Pipe::new();
    let window = pipe.window(1, 1);
    pipe.backend.script_frame_for(app(1), written());
    pipe.backend.script_frame_for(app(1), partial());
    pipe.start(window, 1, FROM, TO);
    pipe.run(100);
    assert_eq!(pipe.backend.sent_frames().len(), 2, "no retry and no final snap");
    let done = pipe.completed();
    assert_eq!(done.len(), 1);
    assert!(matches!(done[0].1, WindowActionOutcome::Issued {
        kind: IssuedKind::Partial,
        ..
    }));
    assert_eq!(pipe.backend.snapshot_reads(window), 0);
    assert_eq!(pipe.backend.frame_reads(window), 0);
    assert_eq!(pipe.frames.outstanding(), 0);
}

#[test]
fn a_refused_batch_ends_the_job_with_the_dispatch_reason() {
    let mut pipe = Pipe::new();
    let window = pipe.window(1, 1);
    pipe.backend.script_dispatch_error_for(app(1), DispatchError::Full);
    pipe.start(window, 1, FROM, TO);
    pipe.run(100);
    let done = pipe.completed();
    assert_eq!(done.len(), 1);
    assert_eq!(
        done[0].1,
        WindowActionOutcome::NotIssued(NotIssuedReason::Dispatch(DispatchError::Full))
    );
    assert!(pipe.backend.sent_frames().is_empty());
}

#[test]
fn a_destroyed_window_ends_the_animation_as_stale() {
    let mut pipe = Pipe::new();
    let window = pipe.window(1, 1);
    pipe.backend.script_frame_for(app(1), written());
    pipe.backend.script_frame_for(app(1), written());
    pipe.backend.script_frame_for(
        app(1),
        WindowActionOutcome::Stale(stache_macos::windows::StaleReason::UnknownWindow),
    );
    pipe.start(window, 1, FROM, TO);
    pipe.run(100);
    let done = pipe.completed();
    assert_eq!(done.len(), 1);
    assert!(matches!(
        classify_frame(window, 1, TO, BOTH, &done[0].1),
        OperationOutcome::Stale { .. }
    ));
    let stages: Vec<_> = pipe.backend.sent_frames().iter().map(|s| s.stage).collect();
    assert_eq!(stages, [
        FrameStage::First,
        FrameStage::Intermediate,
        FrameStage::Intermediate
    ]);
}

#[test]
fn a_seal_during_a_held_write_defers_completion_and_reports_what_was_issued() {
    let mut pipe = Pipe::new();
    let window = pipe.window(1, 1);
    let hold = pipe.backend.hold_application(app(1));
    pipe.start(window, 1, FROM, TO);
    for _ in 0..3 {
        pipe.step();
    }
    assert_eq!(pipe.backend.pending_frames(app(1)), 1);
    pipe.gate.seal();
    pipe.advance(PERIOD);
    assert!(pipe.completed().is_empty(), "the held write has not answered");
    assert_eq!(
        pipe.frames.outstanding(),
        1,
        "still owed, so a drain waits for it"
    );
    assert_eq!(
        pipe.frames.drain(Duration::from_millis(20), &CancellationToken::new()),
        crate::actor::DrainOutcome::TimedOut { outstanding: 1 }
    );
    let sent_before = pipe.backend.sent_frames().len();
    hold.release();
    pipe.pump();
    assert_eq!(
        pipe.backend.sent_frames().len(),
        sent_before,
        "the queued frame was inert"
    );
    let done = pipe.completed();
    assert_eq!(done.len(), 1, "exactly one completion");
    assert!(
        matches!(done[0].1, WindowActionOutcome::Issued { .. }),
        "the held write was issued before the seal: {:?}",
        done[0].1
    );
    assert_eq!(pipe.frames.outstanding(), 0);
    assert_eq!(
        pipe.frames.drain(Duration::from_millis(20), &CancellationToken::new()),
        crate::actor::DrainOutcome::Drained
    );
}

#[test]
fn a_superseded_plan_acknowledges_the_old_plan_once_through_the_shard() {
    let mut pipe = Pipe::new();
    let window = pipe.window(1, 1);
    let hold = pipe.backend.hold_application(app(1));
    pipe.start(window, 1, FROM, TO);
    pipe.step();
    pipe.step();
    // A newer plan for the same window: the old one still has a frame in the
    // shard's send and one pending behind it.
    pipe.start(window, 2, FROM, Rect::new(0.0, 300.0, 100.0, 100.0));
    assert!(pipe.completed().is_empty());
    hold.release();
    pipe.run(300);
    let plans: Vec<u64> = pipe.completed().iter().map(|(plan, ..)| *plan).collect();
    assert_eq!(plans, vec![1, 2], "each plan acknowledged exactly once");
    assert_eq!(pipe.frames.outstanding(), 0);
}

// ------------------------------------------- size envelope (shard backend fake)

/// What each frame of an animation asked the shard to write: (stage, position
/// requested, size requested).
type Requested = (FrameStage, bool, Option<(f64, f64)>);

fn requested(pipe: &Pipe) -> Vec<Requested> {
    pipe.backend
        .sent_frames()
        .iter()
        .map(|s| {
            (
                s.stage,
                s.position.is_some(),
                s.size.map(|z| (z.width, z.height)),
            )
        })
        .collect()
}

fn animate(from: Rect, to: Rect, cross: bool) -> Pipe {
    let mut pipe = Pipe::new();
    let window = pipe.window(1, 1);
    pipe.backend.seed_window_frame(window, from);
    let mut j = job(1, from, to, EasingType::Linear);
    j.window = window;
    j.stamp = Stamp { epoch: pipe.epoch, registry: 1 };
    j.cross_display = cross;
    pipe.frames.note_dispatched(window, 1);
    pipe.driver.admit(j);
    pipe.run(300);
    pipe
}

fn only_size_writes(frames: &[Requested]) -> Vec<(FrameStage, (f64, f64))> {
    frames.iter().filter_map(|(stage, _, size)| size.map(|z| (*stage, z))).collect()
}

/// The sizes the shard was asked to write, in order, and the check that they
/// move monotonically from `from` to `to`, several times, ending exactly at
/// `to` on the final frame.
fn assert_size_ramp(frames: &[Requested], from: (f64, f64), to: (f64, f64)) {
    let sizes = only_size_writes(frames);
    assert!(sizes.len() >= 3, "several size writes: {sizes:?}");
    let (last_stage, last) = *sizes.last().expect("sizes");
    assert_eq!((last_stage, last), (FrameStage::Final, to), "{sizes:?}");
    assert_eq!(sizes[0].0, FrameStage::First, "{sizes:?}");
    for (axis, start, end) in [(0, from.0, to.0), (1, from.1, to.1)] {
        let values: Vec<f64> =
            sizes.iter().map(|(_, z)| if axis == 0 { z.0 } else { z.1 }).collect();
        let step = if end >= start { 1.0 } else { -1.0 };
        assert!(
            values.windows(2).all(|w| (w[1] - w[0]) * step >= 0.0),
            "monotonic: {values:?}"
        );
        let (low, high) = (start.min(end), start.max(end));
        assert!(
            values.iter().all(|v| *v >= low && *v <= high),
            "bounded: {values:?}"
        );
    }
    assert!(
        sizes[..sizes.len() - 1].iter().any(|(_, z)| *z != to),
        "intermediate sizes are not the target: {sizes:?}"
    );
}

#[test]
fn the_shard_sees_interpolated_growth_sizes_and_an_exact_final_write() {
    let to = Rect::new(1000.0, 0.0, 300.0, 200.0);
    let pipe = animate(FROM, to, false);
    let frames = requested(&pipe);
    assert!(frames.iter().all(|(_, position, _)| *position), "{frames:?}");
    assert_size_ramp(&frames, (FROM.width, FROM.height), (300.0, 200.0));
    let window = pipe.window(1, 1);
    assert_eq!(pipe.backend.written_frames().last(), Some(&(window, to)));
    assert_eq!(pipe.completed().len(), 1);
    assert_eq!(pipe.frames.outstanding(), 0);
}

#[test]
fn the_shard_sees_interpolated_shrink_sizes_and_an_exact_final_write() {
    let from = Rect::new(0.0, 0.0, 300.0, 200.0);
    let to = Rect::new(1000.0, 0.0, 100.0, 100.0);
    let pipe = animate(from, to, false);
    let frames = requested(&pipe);
    assert_size_ramp(&frames, (300.0, 200.0), (100.0, 100.0));
    assert_eq!(frames.last().map(|f| f.0), Some(FrameStage::Final));
    let window = pipe.window(1, 1);
    assert_eq!(pipe.backend.written_frames().last(), Some(&(window, to)));
}

#[test]
fn the_shard_sees_each_axis_ramp_toward_its_target_for_a_mixed_change() {
    let from = Rect::new(0.0, 0.0, 100.0, 300.0);
    let to = Rect::new(1000.0, 0.0, 300.0, 100.0);
    let pipe = animate(from, to, false);
    assert_size_ramp(&requested(&pipe), (100.0, 300.0), (300.0, 100.0));
}

#[test]
fn the_shard_sees_a_pure_move_write_no_size_until_the_final_frame() {
    let pipe = animate(FROM, TO, false);
    let frames = requested(&pipe);
    assert_eq!(only_size_writes(&frames), vec![(
        FrameStage::Final,
        (TO.width, TO.height)
    )]);
    let window = pipe.window(1, 1);
    assert_eq!(pipe.backend.written_frames().last(), Some(&(window, TO)));
}

#[test]
fn the_shard_sees_a_cross_display_move_ramp_its_size_like_any_other() {
    let from = Rect::new(0.0, 0.0, 300.0, 200.0);
    let to = Rect::new(1000.0, 0.0, 100.0, 100.0);
    let pipe = animate(from, to, true);
    assert_size_ramp(&requested(&pipe), (300.0, 200.0), (100.0, 100.0));
    let window = pipe.window(1, 1);
    assert_eq!(pipe.backend.written_frames().last(), Some(&(window, to)));
}
