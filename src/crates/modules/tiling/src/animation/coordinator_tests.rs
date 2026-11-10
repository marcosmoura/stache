//! The coordinator over a fake clock and a fake sink: batches per application,
//! stages, acknowledgements, retarget reads, deferred completion and the
//! worker thread around it. Nothing here moves a window.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use stache_macos::ApplicationId;
use stache_macos::ax::FrameRead;
use stache_macos::windows::{
    DispatchError, FrameAck, FrameReply, FrameReplyFn, FrameStage, IssuedKind, NativeStatus,
    NotIssuedReason, StaleReason, WindowActionOutcome, WindowInstance,
};
use stache_sdk::CancellationToken;

use super::tests::{BOTH, FROM, PERIOD, TO, job, read, window, window_in, written};
use super::*;
use crate::actor::{Clock, SystemClock};
use crate::config::{AnimationConfig, EasingType};
use crate::geometry::Rect;
use crate::outcome::{OperationOutcome, classify_frame};

/// One frame a sink was asked to write.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Sent {
    pub application: ApplicationId,
    pub window: WindowInstance,
    pub plan: u64,
    pub frame: Rect,
    pub stage: FrameStage,
    pub components: crate::outcome::FrameComponents,
}

/// What a fake read answers.
#[derive(Clone, Copy, Debug, Default)]
pub(super) enum ReadScript {
    /// The last written frame of the window.
    #[default]
    Follow,
    /// The read fails.
    Fail,
}

pub(super) type Completed = (u64, WindowActionOutcome, FrameCounters);

/// A fake sink: answers every frame inline as written unless scripted, holds
/// replies for a test to answer later, and reads back the last written frame.
pub(super) struct FakeSink {
    sent: Mutex<Vec<Sent>>,
    batches: Mutex<Vec<(ApplicationId, Vec<WindowInstance>)>>,
    held: Mutex<Vec<(Sent, FrameReplyFn)>>,
    pub(super) manual: AtomicBool,
    overflow: Mutex<std::collections::BTreeSet<ApplicationId>>,
    script: Mutex<VecDeque<WindowActionOutcome>>,
    positions: Mutex<HashMap<WindowInstance, Rect>>,
    settle_reads: AtomicUsize,
    retarget_reads: AtomicUsize,
    settle_override: Mutex<ReadScript>,
    retarget_override: Mutex<ReadScript>,
    held_reads: Mutex<Vec<ReadReplyFn>>,
    pub(super) manual_reads: AtomicBool,
    completed: Mutex<Vec<Completed>>,
    sealed: AtomicBool,
    entered: AtomicUsize,
    hold: Mutex<()>,
    /// The next batch panics (a contained-panic test).
    pub(super) panic_batch: AtomicBool,
}

impl FakeSink {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            sent: Mutex::new(Vec::new()),
            batches: Mutex::new(Vec::new()),
            held: Mutex::new(Vec::new()),
            manual: AtomicBool::new(false),
            overflow: Mutex::new(std::collections::BTreeSet::new()),
            script: Mutex::new(VecDeque::new()),
            positions: Mutex::new(HashMap::new()),
            settle_reads: AtomicUsize::new(0),
            retarget_reads: AtomicUsize::new(0),
            settle_override: Mutex::new(ReadScript::Follow),
            retarget_override: Mutex::new(ReadScript::Follow),
            held_reads: Mutex::new(Vec::new()),
            manual_reads: AtomicBool::new(false),
            completed: Mutex::new(Vec::new()),
            sealed: AtomicBool::new(false),
            entered: AtomicUsize::new(0),
            hold: Mutex::new(()),
            panic_batch: AtomicBool::new(false),
        })
    }

    pub(super) fn sent(&self) -> Vec<Sent> { self.sent.lock().expect("sent").clone() }

    pub(super) fn batches(&self) -> Vec<(ApplicationId, Vec<WindowInstance>)> {
        self.batches.lock().expect("batches").clone()
    }

    pub(super) fn completed(&self) -> Vec<Completed> {
        self.completed.lock().expect("completed").clone()
    }

    pub(super) fn script(&self, outcomes: impl IntoIterator<Item = WindowActionOutcome>) {
        self.script.lock().expect("script").extend(outcomes);
    }

    fn position(&self, window: WindowInstance) -> Option<Rect> {
        self.positions.lock().expect("positions").get(&window).copied()
    }

    pub(super) fn settle_reads(&self) -> usize { self.settle_reads.load(Ordering::SeqCst) }

    fn retarget_reads(&self) -> usize { self.retarget_reads.load(Ordering::SeqCst) }

    /// Answers the `index`th held frame.
    pub(super) fn answer(&self, index: usize, reply: FrameReply) {
        let (sent, callback) = self.held.lock().expect("held").remove(index);
        if let FrameReply::Done(FrameAck { outcome, .. }) = reply
            && matches!(outcome, WindowActionOutcome::Issued { .. })
        {
            self.positions.lock().expect("positions").insert(sent.window, sent.frame);
        }
        callback(reply);
    }

    /// Marks `application` as hosted on the overflow shard.
    pub(super) fn set_overflow(&self, application: ApplicationId) {
        self.overflow.lock().expect("overflow").insert(application);
    }

    /// The stage of the `index`th held frame.
    pub(super) fn held_stage(&self, index: usize) -> FrameStage {
        self.held.lock().expect("held")[index].0.stage
    }

    /// The window of the `index`th held frame.
    pub(super) fn held_window(&self, index: usize) -> WindowInstance {
        self.held.lock().expect("held")[index].0.window
    }

    pub(super) fn held(&self) -> usize { self.held.lock().expect("held").len() }

    fn read_for(&self, window: WindowInstance, over: &Mutex<ReadScript>) -> Option<FrameRead> {
        let wanted = *over.lock().expect("override");
        match wanted {
            ReadScript::Fail => None,
            ReadScript::Follow => self.position(window).map(read),
        }
    }
}

impl AnimationSink for FakeSink {
    fn submit_batch(
        &self,
        application: ApplicationId,
        frames: Vec<(AnimationFrame, FrameReplyFn)>,
    ) {
        self.entered.fetch_add(1, Ordering::SeqCst);
        assert!(!self.panic_batch.load(Ordering::SeqCst), "injected sink panic");
        drop(self.hold.lock().expect("hold"));
        self.batches
            .lock()
            .expect("batches")
            .push((application, frames.iter().map(|(f, _)| f.job.window).collect()));
        for (frame, reply) in frames {
            let sent = Sent {
                application,
                window: frame.job.window,
                plan: frame.job.plan_sequence,
                frame: frame.frame,
                stage: frame.stage,
                components: frame.components,
            };
            if self.sealed.load(Ordering::SeqCst) {
                reply(FrameReply::Done(FrameAck {
                    outcome: WindowActionOutcome::NotIssued(NotIssuedReason::Sealed),
                    sent_at: None,
                }));
                continue;
            }
            self.sent.lock().expect("sent").push(sent);
            if self.manual.load(Ordering::SeqCst) {
                self.held.lock().expect("held").push((sent, reply));
                continue;
            }
            let outcome = self.script.lock().expect("script").pop_front().unwrap_or_else(written);
            if matches!(outcome, WindowActionOutcome::Issued { .. }) {
                self.positions.lock().expect("positions").insert(sent.window, sent.frame);
            }
            reply(FrameReply::Done(FrameAck { outcome, sent_at: None }));
        }
    }

    fn submit_settle(&self, job: &AnimationJob, reply: ReadReplyFn) {
        self.settle_reads.fetch_add(1, Ordering::SeqCst);
        if self.manual_reads.load(Ordering::SeqCst) {
            self.held_reads.lock().expect("reads").push(reply);
            return;
        }
        reply(self.read_for(job.window, &self.settle_override));
    }

    fn submit_retarget_read(&self, job: &AnimationJob, reply: ReadReplyFn) {
        self.retarget_reads.fetch_add(1, Ordering::SeqCst);
        if self.manual_reads.load(Ordering::SeqCst) {
            self.held_reads.lock().expect("reads").push(reply);
            return;
        }
        reply(self.read_for(job.window, &self.retarget_override));
    }

    fn complete(&self, job: &AnimationJob, outcome: WindowActionOutcome, counters: FrameCounters) {
        self.completed
            .lock()
            .expect("completed")
            .push((job.plan_sequence, outcome, counters));
    }

    fn stopping(&self) -> Option<NotIssuedReason> {
        self.sealed.load(Ordering::SeqCst).then_some(NotIssuedReason::Sealed)
    }

    fn is_overflow(&self, application: ApplicationId) -> bool {
        self.overflow.lock().expect("overflow").contains(&application)
    }
}

type Inbox = Arc<Mutex<VecDeque<Ack>>>;

/// An animator over a fake clock, its acknowledgements delivered by `pump`.
pub(super) struct Driver {
    inbox: Inbox,
    pub(super) animator: Animator,
    pub(super) now: Instant,
    pub(super) next: Option<Instant>,
    /// The frame-clock ticks the next pump hands to the coordinator.
    pub(super) ticks: std::collections::BTreeMap<stache_macos::DisplayId, Instant>,
}

impl Driver {
    pub(super) fn new(sink: Arc<dyn AnimationSink>) -> Self {
        let inbox: Inbox = Arc::new(Mutex::new(VecDeque::new()));
        let port: AckPort = {
            let inbox = Arc::clone(&inbox);
            Arc::new(move |ack| inbox.lock().expect("inbox").push_back(ack))
        };
        Self {
            inbox,
            animator: Animator::new(sink, port),
            now: Instant::now(),
            next: None,
            ticks: std::collections::BTreeMap::new(),
        }
    }

    /// Delivers every acknowledgement and runs what is due, until quiet.
    pub(super) fn pump(&mut self) {
        loop {
            let acks: Vec<Ack> = self.inbox.lock().expect("inbox").drain(..).collect();
            for ack in acks {
                self.animator.on_ack(ack, self.now);
            }
            let ticks = std::mem::take(&mut self.ticks);
            self.next = self.animator.tick_paced(self.now, &ticks);
            if self.inbox.lock().expect("inbox").is_empty() {
                return;
            }
        }
    }

    pub(super) fn admit(&mut self, job: AnimationJob) {
        self.animator.admit(job, self.now);
        self.pump();
    }

    /// Moves the clock to the next due instant. `false` when nothing is timed.
    pub(super) fn step(&mut self) -> bool {
        let Some(next) = self.next else { return false };
        self.now = next.max(self.now);
        self.pump();
        true
    }

    pub(super) fn run(&mut self, limit: usize) {
        for _ in 0..limit {
            if !self.step() {
                return;
            }
        }
    }

    pub(super) fn advance(&mut self, by: Duration) {
        self.now += by;
        self.pump();
    }
}

/// A [`Driver`] over a [`FakeSink`].
pub(super) struct Rig {
    pub(super) sink: Arc<FakeSink>,
    driver: Driver,
}

impl Rig {
    pub(super) fn new() -> Self {
        let sink = FakeSink::new();
        let driver = Driver::new(Arc::clone(&sink) as Arc<dyn AnimationSink>);
        Self { sink, driver }
    }
}

impl std::ops::Deref for Rig {
    type Target = Driver;

    fn deref(&self) -> &Driver { &self.driver }
}

impl std::ops::DerefMut for Rig {
    fn deref_mut(&mut self) -> &mut Driver { &mut self.driver }
}

fn windows_of(sent: &[Sent], plan: u64) -> Vec<Sent> {
    sent.iter().filter(|s| s.plan == plan).copied().collect()
}

// ----------------------------------------------------------------- stepper

#[test]
fn an_animation_sends_first_intermediate_and_final_frames_and_reads_back_once() {
    let mut rig = Rig::new();
    rig.admit(job(7, FROM, TO, EasingType::Linear));
    rig.run(100);
    let sent = rig.sink.sent();
    assert!(sent.len() >= 8, "intermediate frames were sent: {}", sent.len());
    assert!(sent.iter().all(|s| s.plan == 7));
    assert_eq!(sent[0].stage, FrameStage::First);
    assert!(
        sent[1..sent.len() - 1].iter().all(|s| s.stage == FrameStage::Intermediate),
        "{sent:?}"
    );
    assert_eq!(sent.last().expect("last").stage, FrameStage::Final);
    assert_eq!(
        sent.last().expect("last").frame,
        TO,
        "the final frame is the exact target"
    );
    let xs: Vec<f64> = sent.iter().map(|s| s.frame.x).collect();
    assert!(xs.windows(2).all(|w| w[0] <= w[1]), "monotonic travel {xs:?}");
    assert_eq!(rig.sink.settle_reads(), 1, "exactly one settle readback");
    assert_eq!(rig.sink.retarget_reads(), 0, "no retarget read for a fresh job");
    let done = rig.sink.completed();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].0, 7);
    let outcome = classify_frame(window(1), 7, TO, BOTH, &done[0].1);
    assert!(
        matches!(outcome, OperationOutcome::Applied { .. }),
        "{outcome:?}"
    );
    assert_eq!(outcome.actual(), Some(TO), "the readback is reported");
    assert_eq!(done[0].2, FrameCounters {
        scheduled: sent.len(),
        written: sent.len(),
        dropped: 0
    });
    assert_eq!(rig.animator.totals(), done[0].2);
    assert!(rig.animator.is_empty());
}

#[test]
fn only_the_first_and_final_frames_ask_for_membership() {
    let mut rig = Rig::new();
    rig.admit(job(1, FROM, TO, EasingType::Linear));
    rig.run(100);
    let stages: Vec<FrameStage> = rig.sink.sent().iter().map(|s| s.stage).collect();
    assert_eq!(stages.iter().filter(|s| **s == FrameStage::First).count(), 1);
    assert_eq!(stages.iter().filter(|s| **s == FrameStage::Final).count(), 1);
    assert!(stages.iter().all(|s| *s != FrameStage::Direct));
    assert!(stages.len() > 3);
}

#[test]
fn a_small_move_is_one_final_frame() {
    let mut rig = Rig::new();
    let target = Rect::new(2.0, 0.0, 100.0, 100.0);
    rig.admit(job(1, FROM, target, EasingType::EaseOut));
    rig.run(20);
    let sent = rig.sink.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!((sent[0].frame, sent[0].stage), (target, FrameStage::Final));
    assert_eq!(rig.sink.completed().len(), 1);
    assert_eq!(rig.sink.settle_reads(), 1);
}

#[test]
fn a_tick_sends_one_batch_per_application() {
    let mut rig = Rig::new();
    for application in 1..=3 {
        for serial in 1..=2 {
            let mut j = job(10 * application + serial, FROM, TO, EasingType::Linear);
            j.window = window_in(application, serial);
            rig.admit(j);
        }
    }
    assert!(
        rig.sink.batches().is_empty(),
        "the first frame is due one period later"
    );
    for tick in 1..=3 {
        rig.step();
        let batches = rig.sink.batches();
        assert_eq!(batches.len(), 3 * tick, "three applications, one batch each");
        for (application, windows) in &batches[3 * (tick - 1)..] {
            assert_eq!(windows.len(), 2, "both windows of an application share a batch");
            assert!(windows.iter().all(|w| w.application() == *application));
        }
        let mut applications: Vec<_> = batches[3 * (tick - 1)..].iter().map(|(a, _)| *a).collect();
        applications.sort_by_key(|a| format!("{a:?}"));
        applications.dedup();
        assert_eq!(applications.len(), 3);
    }
}

#[test]
fn a_retarget_takes_one_read_and_starts_from_where_the_window_is() {
    let mut rig = Rig::new();
    rig.admit(job(1, FROM, TO, EasingType::Linear));
    for _ in 0..5 {
        rig.step();
    }
    let reported = rig.sink.position(window(1)).expect("a frame was written");
    assert!(
        reported.x > 0.0 && reported.x < 1000.0,
        "mid-flight {reported:?}"
    );
    let settle_before = rig.sink.settle_reads();

    // The planner still believes the window is at FROM and wants it elsewhere.
    let target = Rect::new(0.0, 300.0, 100.0, 100.0);
    rig.admit(job(2, FROM, target, EasingType::Linear));
    assert_eq!(rig.sink.retarget_reads(), 1, "one read at retarget time");
    let done = rig.sink.completed();
    assert_eq!(done.len(), 1, "the interrupted job is acknowledged once");
    assert_eq!(done[0].0, 1, "for its own plan");
    assert!(matches!(
        done[0].1,
        WindowActionOutcome::NotIssued(NotIssuedReason::Superseded)
    ));
    let before = rig.sink.sent().len();
    rig.step();
    let first = windows_of(&rig.sink.sent()[before..], 2)[0];
    assert_eq!(first.stage, FrameStage::First);
    assert!(
        first.frame.x < reported.x && first.frame.x > reported.x * 0.5,
        "the new animation starts at the read x ({}), not at the planner's 0 or the old \
         destination 1000: {first:?}",
        reported.x
    );
    rig.run(100);
    assert_eq!(rig.sink.sent().last().expect("last").frame, target);
    assert_eq!(rig.sink.retarget_reads(), 1, "still the one retarget read");
    assert_eq!(
        rig.sink.settle_reads(),
        settle_before + 1,
        "only the new job settles"
    );
}

#[test]
fn a_failed_retarget_read_starts_from_the_last_acknowledged_frame() {
    let mut rig = Rig::new();
    *rig.sink.retarget_override.lock().expect("override") = ReadScript::Fail;
    rig.admit(job(1, FROM, TO, EasingType::Linear));
    for _ in 0..5 {
        rig.step();
    }
    let acked = rig.animator.last_acknowledged(window(1)).expect("acknowledged");
    assert!(acked.x > 0.0 && acked.x < 1000.0);
    let target = Rect::new(0.0, 300.0, 100.0, 100.0);
    rig.admit(job(2, FROM, target, EasingType::Linear));
    assert_eq!(rig.sink.retarget_reads(), 1);
    let before = rig.sink.sent().len();
    rig.step();
    let first = windows_of(&rig.sink.sent()[before..], 2)[0];
    assert!(
        first.frame.x < acked.x && first.frame.x > acked.x * 0.5,
        "from the last acknowledged x {} (not the old destination): {first:?}",
        acked.x
    );
    rig.run(100);
    assert_eq!(rig.sink.sent().last().expect("last").frame, target);
    assert_eq!(rig.sink.retarget_reads(), 1, "never a second read");
}

#[test]
fn a_late_retarget_read_times_out_and_is_ignored() {
    let mut rig = Rig::new();
    rig.admit(job(1, FROM, TO, EasingType::Linear));
    for _ in 0..5 {
        rig.step();
    }
    let acked = rig.animator.last_acknowledged(window(1)).expect("acknowledged");
    rig.sink.manual_reads.store(true, Ordering::SeqCst);
    let target = Rect::new(0.0, 300.0, 100.0, 100.0);
    rig.admit(job(2, FROM, target, EasingType::Linear));
    let before = rig.sink.sent().len();
    rig.advance(PERIOD);
    assert_eq!(
        rig.sink.sent().len(),
        before,
        "waiting for the read, nothing sent"
    );
    rig.advance(PERIOD * 2);
    rig.step();
    let sent = rig.sink.sent();
    let first = windows_of(&sent[before..], 2)[0];
    assert!(first.frame.x < acked.x, "started from the fallback: {first:?}");
    // The read answers far too late: it changes nothing.
    let late = rig.sink.held_reads.lock().expect("reads").remove(0);
    late(Some(read(Rect::new(900.0, 0.0, 100.0, 100.0))));
    rig.pump();
    rig.run(100);
    let plan2: Vec<f64> = windows_of(&rig.sink.sent(), 2).iter().map(|s| s.frame.x).collect();
    assert!(plan2.iter().all(|x| *x < acked.x), "{plan2:?}");
}

#[test]
fn a_retarget_without_an_acknowledged_frame_and_a_failed_read_starts_from_the_plan() {
    let mut rig = Rig::new();
    rig.sink.manual.store(true, Ordering::SeqCst);
    rig.admit(job(1, FROM, TO, EasingType::Linear));
    rig.step();
    assert_eq!(rig.sink.held(), 1, "the first frame is unanswered");
    *rig.sink.retarget_override.lock().expect("override") = ReadScript::Fail;
    let from = Rect::new(50.0, 0.0, 100.0, 100.0);
    let target = Rect::new(50.0, 400.0, 100.0, 100.0);
    rig.sink.manual.store(false, Ordering::SeqCst);
    rig.admit(job(2, from, target, EasingType::Linear));
    let before = rig.sink.sent().len();
    rig.step();
    let first = windows_of(&rig.sink.sent()[before..], 2)[0];
    assert!(
        first.frame.x <= 50.0 + 1e-9 && first.frame.y < 100.0,
        "near the plan's from: {first:?}"
    );
}

#[test]
fn a_b_a_never_lets_an_old_job_mark_a_successor_applied() {
    let mut rig = Rig::new();
    let b = Rect::new(600.0, 0.0, 100.0, 100.0);
    rig.admit(job(1, FROM, b, EasingType::Linear));
    rig.step();
    rig.admit(job(2, FROM, FROM, EasingType::Linear));
    rig.step();
    rig.admit(job(3, FROM, b, EasingType::Linear));
    rig.run(200);
    let done = rig.sink.completed();
    let plans: Vec<u64> = done.iter().map(|(plan, ..)| *plan).collect();
    assert_eq!(
        plans,
        vec![1, 2, 3],
        "each job acknowledges exactly its own plan, once"
    );
    for (plan, outcome, _) in &done[..2] {
        assert!(
            matches!(
                outcome,
                WindowActionOutcome::NotIssued(NotIssuedReason::Superseded)
            ),
            "plan {plan} is a typed no-send, never applied: {outcome:?}"
        );
    }
    assert!(matches!(
        classify_frame(window(1), 3, b, BOTH, &done[2].1),
        OperationOutcome::Applied { .. }
    ));
}

#[test]
fn a_b_a_brings_the_window_back() {
    let mut rig = Rig::new();
    rig.admit(job(1, FROM, TO, EasingType::Linear));
    for _ in 0..6 {
        rig.step();
    }
    let away = rig.sink.position(window(1)).expect("moved");
    assert!(away.x > 0.0);
    // The planner now wants the window back at A.
    rig.admit(job(2, TO, FROM, EasingType::Linear));
    rig.run(200);
    assert_eq!(rig.sink.position(window(1)), Some(FROM), "back at A");
    let done = rig.sink.completed();
    assert_eq!(done.iter().map(|(plan, ..)| *plan).collect::<Vec<_>>(), vec![
        1, 2
    ]);
    assert!(matches!(
        classify_frame(window(1), 2, FROM, BOTH, &done[1].1),
        OperationOutcome::Applied { .. }
    ));
}

#[test]
fn a_superseded_job_with_a_frame_in_flight_completes_when_the_frame_answers() {
    let mut rig = Rig::new();
    rig.sink.manual.store(true, Ordering::SeqCst);
    rig.admit(job(1, FROM, TO, EasingType::Linear));
    rig.step();
    assert_eq!(rig.sink.held(), 1);
    rig.sink.manual.store(false, Ordering::SeqCst);
    rig.admit(job(
        2,
        FROM,
        Rect::new(0.0, 300.0, 100.0, 100.0),
        EasingType::Linear,
    ));
    assert!(
        rig.sink.completed().is_empty(),
        "the old plan waits for its in-flight frame"
    );
    assert_eq!(rig.animator.deferred(), 1);
    // The held frame was in fact written: that is what the old plan reports.
    rig.sink.answer(
        0,
        FrameReply::Done(FrameAck {
            outcome: written(),
            sent_at: None,
        }),
    );
    rig.pump();
    let done = rig.sink.completed();
    assert_eq!(done.len(), 1, "exactly one acknowledgement of the old plan");
    assert_eq!(done[0].0, 1);
    assert!(matches!(done[0].1, WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        ..
    }));
    assert_eq!(rig.animator.deferred(), 0);

    // A frame that was never sent reports the supersede reason.
    let mut other = Rig::new();
    other.sink.manual.store(true, Ordering::SeqCst);
    other.admit(job(1, FROM, TO, EasingType::Linear));
    other.step();
    other.admit(job(2, FROM, TO, EasingType::Linear));
    other.sink.answer(
        0,
        FrameReply::Done(FrameAck {
            outcome: WindowActionOutcome::NotIssued(NotIssuedReason::Superseded),
            sent_at: None,
        }),
    );
    other.pump();
    let done = other.sink.completed();
    assert_eq!(done.len(), 1);
    assert!(matches!(
        done[0].1,
        WindowActionOutcome::NotIssued(NotIssuedReason::Superseded)
    ));
}

#[test]
fn a_refused_frame_ends_the_job_with_that_outcome_and_no_final_snap() {
    let rejected = WindowActionOutcome::Issued {
        kind: IssuedKind::Rejected,
        native: NativeStatus::Frame {
            position: stache_macos::ax::WriteStatus::Failed(
                stache_macos::ax::AxError::CannotComplete,
            ),
            size: stache_macos::ax::WriteStatus::NotRequested,
        },
        observed: None,
    };
    let outcomes = [
        rejected,
        WindowActionOutcome::Issued {
            kind: IssuedKind::Partial,
            native: NativeStatus::Unobserved,
            observed: None,
        },
        WindowActionOutcome::Issued {
            kind: IssuedKind::Uncertain,
            native: NativeStatus::Unobserved,
            observed: None,
        },
        WindowActionOutcome::Stale(StaleReason::UnknownWindow),
        WindowActionOutcome::ExpiredBeforeSend,
        WindowActionOutcome::NotIssued(NotIssuedReason::Dispatch(DispatchError::Full)),
        WindowActionOutcome::NotIssued(NotIssuedReason::Superseded),
    ];
    for outcome in outcomes {
        let mut rig = Rig::new();
        rig.sink.script([written(), outcome]);
        rig.admit(job(1, FROM, TO, EasingType::Linear));
        rig.run(100);
        assert_eq!(
            rig.sink.sent().len(),
            2,
            "no retry and no final snap: {outcome:?}"
        );
        let done = rig.sink.completed();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].1, outcome);
        assert_eq!(rig.sink.settle_reads(), 0);
        assert!(rig.animator.is_empty());
        let later = rig.now + Duration::from_secs(5);
        assert_eq!(rig.animator.tick(later), None);
        assert_eq!(rig.sink.sent().len(), 2);
    }
}

#[test]
fn a_window_destroyed_mid_animation_ends_as_stale() {
    let mut rig = Rig::new();
    rig.sink.script([
        written(),
        written(),
        WindowActionOutcome::Stale(StaleReason::UnknownWindow),
    ]);
    rig.admit(job(1, FROM, TO, EasingType::Linear));
    rig.run(100);
    let sent = rig.sink.sent();
    assert_eq!(
        sent.iter().map(|s| s.stage).collect::<Vec<_>>(),
        vec![
            FrameStage::First,
            FrameStage::Intermediate,
            FrameStage::Intermediate
        ],
        "membership was asked for once, at the first frame"
    );
    let done = rig.sink.completed();
    assert_eq!(done.len(), 1);
    assert!(matches!(
        classify_frame(window(1), 1, TO, BOTH, &done[0].1),
        OperationOutcome::Stale { .. }
    ));
}

#[test]
fn replaced_frames_are_counted_as_dropped_and_do_not_end_the_job() {
    let mut rig = Rig::new();
    rig.sink.manual.store(true, Ordering::SeqCst);
    rig.admit(job(1, FROM, TO, EasingType::Linear));
    rig.step();
    rig.step();
    rig.step();
    assert_eq!(rig.sink.held(), 3, "frames keep going out while unanswered");
    rig.sink.answer(0, FrameReply::Replaced);
    rig.sink.answer(0, FrameReply::Replaced);
    rig.sink.answer(
        0,
        FrameReply::Done(FrameAck {
            outcome: written(),
            sent_at: None,
        }),
    );
    rig.sink.manual.store(false, Ordering::SeqCst);
    rig.pump();
    rig.run(100);
    let done = rig.sink.completed();
    assert_eq!(done.len(), 1);
    let counters = done[0].2;
    assert_eq!(counters.dropped, 2);
    assert_eq!(counters.scheduled, counters.written + counters.dropped);
    assert_eq!(rig.sink.sent().last().expect("last").frame, TO);
}

#[test]
fn a_seal_during_a_held_frame_defers_the_completion_until_it_answers() {
    let mut rig = Rig::new();
    rig.sink.manual.store(true, Ordering::SeqCst);
    rig.admit(job(1, FROM, TO, EasingType::Linear));
    rig.step();
    rig.sink.sealed.store(true, Ordering::SeqCst);
    rig.advance(PERIOD);
    assert_eq!(rig.sink.sent().len(), 1, "nothing more is sent after the seal");
    assert!(
        rig.sink.completed().is_empty(),
        "the held frame has not answered"
    );
    assert_eq!(rig.animator.deferred(), 1);
    assert!(!rig.animator.is_empty());
    // The frame was in fact issued before the seal: that is reported.
    rig.sink.answer(
        0,
        FrameReply::Done(FrameAck {
            outcome: written(),
            sent_at: None,
        }),
    );
    rig.pump();
    let done = rig.sink.completed();
    assert_eq!(done.len(), 1);
    assert!(
        matches!(done[0].1, WindowActionOutcome::Issued { .. }),
        "{:?}",
        done[0].1
    );
    assert!(rig.animator.is_empty());

    // A sealed (inert) frame reports the seal.
    let mut other = Rig::new();
    other.sink.manual.store(true, Ordering::SeqCst);
    other.admit(job(1, FROM, TO, EasingType::Linear));
    other.step();
    other.sink.sealed.store(true, Ordering::SeqCst);
    other.advance(PERIOD);
    other.sink.answer(
        0,
        FrameReply::Done(FrameAck {
            outcome: WindowActionOutcome::NotIssued(NotIssuedReason::Sealed),
            sent_at: None,
        }),
    );
    other.pump();
    assert!(matches!(
        other.sink.completed()[0].1,
        WindowActionOutcome::NotIssued(NotIssuedReason::Sealed)
    ));
}

#[test]
fn a_seal_with_nothing_in_flight_completes_at_once() {
    let mut rig = Rig::new();
    rig.admit(job(1, FROM, TO, EasingType::EaseOut));
    rig.step();
    let written_before = rig.sink.sent().len();
    rig.sink.sealed.store(true, Ordering::SeqCst);
    rig.advance(PERIOD);
    assert_eq!(
        rig.sink.sent().len(),
        written_before,
        "nothing is sent after the seal"
    );
    let done = rig.sink.completed();
    assert_eq!(done.len(), 1);
    assert!(matches!(
        done[0].1,
        WindowActionOutcome::NotIssued(NotIssuedReason::Sealed)
    ));
    assert!(rig.animator.is_empty());
}

#[test]
fn an_abort_acknowledges_every_job_with_the_stop_reason_and_sends_nothing() {
    let mut rig = Rig::new();
    rig.admit(job(1, FROM, TO, EasingType::Spring));
    let mut other = job(2, FROM, TO, EasingType::Spring);
    other.window = window(2);
    rig.admit(other);
    rig.sink.sealed.store(true, Ordering::SeqCst);
    rig.animator.abort_all();
    assert!(rig.sink.sent().is_empty());
    let done = rig.sink.completed();
    assert_eq!(done.len(), 2);
    assert!(
        done.iter()
            .all(|(_, o, _)| matches!(o, WindowActionOutcome::NotIssued(NotIssuedReason::Sealed)))
    );
    assert!(rig.animator.is_empty());
}

#[test]
fn a_failed_settle_readback_reports_the_final_outcome_without_observation() {
    let mut rig = Rig::new();
    *rig.sink.settle_override.lock().expect("override") = ReadScript::Fail;
    rig.admit(job(1, FROM, TO, EasingType::Linear));
    rig.run(100);
    let done = rig.sink.completed();
    assert_eq!(done.len(), 1);
    assert!(matches!(done[0].1, WindowActionOutcome::Issued {
        observed: None,
        ..
    }));
    assert_eq!(rig.sink.settle_reads(), 1);
}

#[test]
fn an_unanswered_settle_readback_ends_the_job_after_its_budget() {
    let mut rig = Rig::new();
    rig.sink.manual_reads.store(true, Ordering::SeqCst);
    rig.admit(job(1, FROM, TO, EasingType::Linear));
    while rig.sink.settle_reads() == 0 {
        assert!(rig.step(), "the animation reaches its settle read");
    }
    assert!(rig.sink.completed().is_empty(), "waiting for the read");
    rig.advance(SETTLE_READ_BUDGET + Duration::from_millis(1));
    assert_eq!(rig.sink.completed().len(), 1);
    assert!(rig.animator.is_empty());
}

#[test]
fn a_spring_animation_ends_on_the_exact_target() {
    let mut rig = Rig::new();
    rig.admit(job(1, FROM, TO, EasingType::Spring));
    rig.run(200);
    let sent = rig.sink.sent();
    assert_eq!(sent.last().expect("last").frame, TO);
    assert!(
        sent.iter().all(|s| s.frame.x <= TO.x),
        "no overshoot past the target"
    );
    assert_eq!(rig.sink.completed().len(), 1);
}

#[test]
fn an_animator_at_capacity_sends_the_exact_target_at_once() {
    let mut rig = Rig::new();
    for n in 0..=(MAX_ANIMATED as u64) {
        let mut j = job(10 + n, FROM, TO, EasingType::Linear);
        j.window = window(100 + n);
        let now = rig.now;
        rig.animator.admit(j, now);
    }
    rig.pump();
    let overflow = MAX_ANIMATED as u64 + 10;
    let sent = rig.sink.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(
        (sent[0].plan, sent[0].frame, sent[0].stage),
        (overflow, TO, FrameStage::Final)
    );
    rig.run(500);
    let done = rig.sink.completed();
    assert_eq!(done.len(), MAX_ANIMATED + 1);
    assert!(
        done.iter().all(|(_, o, _)| matches!(o, WindowActionOutcome::Issued { .. })),
        "not NotIssued(Full)"
    );
}

// ------------------------------------------------------------------ worker

pub(super) fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn spawn(sink: &Arc<FakeSink>, cancel: &CancellationToken) -> AnimationWorker {
    AnimationWorker::spawn(
        Arc::clone(sink) as Arc<dyn AnimationSink>,
        Arc::new(SystemClock) as Arc<dyn Clock>,
        Arc::new(stache_macos::FakeFrameClockFactory::failing("timer"))
            as Arc<dyn stache_macos::FrameClockFactory>,
        cancel,
        AnimationShared::default(),
    )
    .expect("worker")
}

#[test]
fn cancellation_wakes_a_sleeping_worker_promptly() {
    let sink = FakeSink::new();
    let cancel = CancellationToken::new();
    let worker = spawn(&sink, &cancel);
    let mut slow = job(1, FROM, TO, EasingType::EaseOut);
    slow.period = Duration::from_secs(30);
    assert!(worker.submit(slow).is_ok());
    wait_for("the job was admitted", || worker.pending() == 0);
    let begun = Instant::now();
    cancel.cancel();
    assert!(worker.join_within(Duration::from_secs(5)), "the worker ended");
    assert!(begun.elapsed() < Duration::from_secs(2), "woken, not slept out");
    let done = sink.completed();
    assert_eq!(done.len(), 1);
    assert!(matches!(
        done[0].1,
        WindowActionOutcome::NotIssued(NotIssuedReason::Cancelled)
    ));
    assert!(sink.sent().is_empty());
    assert!(
        worker.submit(job(2, FROM, TO, EasingType::Linear)).is_err(),
        "closed"
    );
}

#[test]
fn the_worker_stays_alive_after_close_until_every_in_flight_frame_answers() {
    let sink = FakeSink::new();
    sink.manual.store(true, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let worker = spawn(&sink, &cancel);
    let mut fast = job(1, FROM, TO, EasingType::Linear);
    fast.period = Duration::from_millis(2);
    assert!(worker.submit(fast).is_ok());
    wait_for("a frame in flight", || sink.held() >= 1);
    worker.close();
    assert!(
        !worker.join_within(Duration::from_millis(100)),
        "a held frame keeps the coordinator alive"
    );
    assert!(
        sink.completed().is_empty(),
        "not completed while a frame is in flight"
    );
    // Every held frame answers; the last one is an issued write.
    while sink.held() > 0 {
        sink.answer(
            0,
            FrameReply::Done(FrameAck {
                outcome: written(),
                sent_at: None,
            }),
        );
    }
    assert!(
        worker.join_within(Duration::from_secs(5)),
        "the coordinator ended"
    );
    let done = sink.completed();
    assert_eq!(done.len(), 1, "exactly one completion");
    assert!(
        matches!(done[0].1, WindowActionOutcome::Issued { .. }),
        "{:?}",
        done[0].1
    );
}

#[test]
fn the_mailbox_keeps_only_the_newest_plan_per_window() {
    let sink = FakeSink::new();
    let cancel = CancellationToken::new();
    let worker = spawn(&sink, &cancel);
    // Hold the thread inside a batch so later submissions stay pending.
    let hold = sink.hold.lock().expect("hold");
    let mut first = job(1, FROM, TO, EasingType::Linear);
    first.period = Duration::from_millis(1);
    assert!(worker.submit(first).is_ok());
    wait_for("the worker blocked in a batch", || {
        worker.pending() == 0 && sink.entered.load(Ordering::SeqCst) > 0
    });
    assert!(worker.submit(job(2, FROM, TO, EasingType::Linear)).is_ok());
    assert!(worker.submit(job(3, FROM, TO, EasingType::Linear)).is_ok());
    assert_eq!(worker.pending(), 1, "the newest plan replaced the older one");
    assert_eq!(
        sink.completed().iter().map(|(plan, ..)| *plan).collect::<Vec<_>>(),
        vec![2],
        "the replaced plan was acknowledged as superseded"
    );
    drop(hold);
    cancel.cancel();
    assert!(worker.join_within(Duration::from_secs(5)));
    let plans: Vec<u64> = sink.completed().iter().map(|(plan, ..)| *plan).collect();
    assert!(
        plans.contains(&1) && plans.contains(&2) && plans.contains(&3),
        "{plans:?}"
    );
    assert_eq!(plans.len(), 3, "each plan acknowledged exactly once");
}

#[test]
fn the_mailbox_is_bounded() {
    let sink = FakeSink::new();
    let cancel = CancellationToken::new();
    let worker = spawn(&sink, &cancel);
    let hold = sink.hold.lock().expect("hold");
    let mut first = job(1, FROM, TO, EasingType::Linear);
    first.period = Duration::from_millis(1);
    assert!(worker.submit(first).is_ok());
    wait_for("the worker blocked in a batch", || {
        worker.pending() == 0 && sink.entered.load(Ordering::SeqCst) > 0
    });
    // One window runs; the rest of the cap is pending.
    for n in 0..(MAX_ANIMATED as u64 - 1) {
        let mut j = job(10 + n, FROM, TO, EasingType::Linear);
        j.window = window(100 + n);
        assert!(worker.submit(j).is_ok());
    }
    let mut extra = job(99, FROM, TO, EasingType::Linear);
    extra.window = window(999);
    let refused = worker.submit(extra).expect_err("full: the caller writes directly");
    assert_eq!(
        refused.plan_sequence, 99,
        "the overflow job comes back untouched"
    );
    assert!(
        sink.completed().iter().all(|(plan, ..)| *plan != 99),
        "a refused job is not acknowledged by the worker: the caller owns its write"
    );
    // A newer plan for a window the worker already holds is still accepted.
    let mut again = job(100, FROM, TO, EasingType::Linear);
    again.window = window(100);
    assert!(worker.submit(again).is_ok());
    drop(hold);
    cancel.cancel();
    assert!(worker.join_within(Duration::from_secs(5)));
}

#[test]
fn seventeen_windows_all_reach_their_exact_target() {
    let sink = FakeSink::new();
    let cancel = CancellationToken::new();
    let worker = spawn(&sink, &cancel);
    let mut direct = Vec::new();
    for n in 0..=(MAX_ANIMATED as u64) {
        let mut j = job(10 + n, FROM, TO, EasingType::Linear);
        j.window = window(100 + n);
        j.period = Duration::from_millis(2);
        if let Err(back) = worker.submit(j) {
            // The caller (`EffectEnv::run`) writes the exact target itself.
            direct.push(back.plan_sequence);
        }
    }
    assert_eq!(direct.len(), 1, "only the 17th window is refused");
    // While 16 are running or settling a second plan for one of them is accepted.
    let mut second = job(500, FROM, Rect::new(5.0, 5.0, 100.0, 100.0), EasingType::Linear);
    second.window = window(100);
    second.period = Duration::from_millis(2);
    assert!(
        worker.submit(second).is_ok(),
        "a held window is retargeted, never dropped"
    );
    wait_for("every accepted plan acknowledged", || {
        sink.completed().len() > MAX_ANIMATED
    });
    let completed = sink.completed();
    let sent = sink.sent();
    for n in 1..(MAX_ANIMATED as u64) {
        let plan = 10 + n;
        assert!(
            completed.iter().any(|(p, ..)| *p == plan),
            "plan {plan} acknowledged"
        );
        assert_eq!(
            sent.iter().rev().find(|s| s.plan == plan).map(|s| s.frame),
            Some(TO),
            "plan {plan} ended on its exact target"
        );
    }
    cancel.cancel();
    assert!(worker.join_within(Duration::from_secs(5)));
}

#[test]
fn a_disabled_control_owns_no_worker_and_follows_the_configuration() {
    let sink = FakeSink::new();
    let cancel = CancellationToken::new();
    let control = AnimationControl::new(
        Arc::clone(&sink) as Arc<dyn AnimationSink>,
        Arc::new(SystemClock),
        Arc::new(stache_macos::FakeFrameClockFactory::failing("timer"))
            as Arc<dyn stache_macos::FrameClockFactory>,
        cancel,
    );
    assert!(!control.is_running(), "construction is inert");
    control.configure(&AnimationConfig::default()).expect("disabled");
    assert!(!control.is_running(), "default off spawns nothing");
    assert!(control.submit(job(1, FROM, TO, EasingType::Linear)).is_err());

    let on = AnimationConfig {
        enabled: true,
        duration: 300,
        easing: EasingType::Linear,
    };
    control.configure(&on).expect("enable");
    assert!(control.is_running());
    control.configure(&AnimationConfig::default()).expect("disable");
    assert!(
        !control.is_running(),
        "a reload that disables retires the worker"
    );
    assert!(control.submit(job(2, FROM, TO, EasingType::Linear)).is_err());
    assert!(control.join_within(Duration::from_secs(5)));
    assert_eq!(control.retired(), 0);

    control.configure(&on).expect("enable again");
    assert!(control.is_running());
    control.stop();
    assert!(control.join_within(Duration::from_secs(5)));
    control.configure(&on).expect("ignored after a stop");
    assert!(!control.is_running(), "a stopped control never spawns again");
}

#[test]
fn a_contained_panic_completes_an_ending_job_exactly_once() {
    let sink = FakeSink::new();
    sink.manual.store(true, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let worker = spawn(&sink, &cancel);
    let mut first = job(1, FROM, TO, EasingType::Linear);
    first.period = Duration::from_millis(2);
    assert!(worker.submit(first).is_ok());
    wait_for("two frames in flight", || sink.held() >= 2);
    // The first frame is refused while another is still in flight: the job
    // ends, its completion waiting for that answer.
    sink.answer(
        0,
        FrameReply::Done(FrameAck {
            outcome: WindowActionOutcome::NotIssued(NotIssuedReason::Cancelled),
            sent_at: None,
        }),
    );
    wait_for("the job is ending", || {
        sink.held() >= 1 && sink.completed().is_empty()
    });
    sink.panic_batch.store(true, Ordering::SeqCst);
    let mut second = job(2, FROM, TO, EasingType::Linear);
    second.window = window_in(2, 1);
    second.period = Duration::from_millis(2);
    assert!(worker.submit(second).is_ok());
    assert!(
        worker.join_within(Duration::from_secs(5)),
        "the worker ended after the contained panic"
    );
    let mut plans: Vec<u64> = sink.completed().iter().map(|c| c.0).collect();
    plans.sort_unstable();
    assert_eq!(plans, vec![1, 2], "each job completed exactly once");
}
