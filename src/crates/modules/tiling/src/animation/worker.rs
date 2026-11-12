//! The animation coordinator: a deterministic engine and the one thread around it.
//!
//! [`Animator`] is handed the time and a stream of acknowledgements. On each
//! tick it computes the next frame of every running window and sends **one
//! batch per application** through an [`AnimationSink`]; it never waits for a
//! write. The shard of each application keeps only the latest pending frame
//! per window, so a slow application skips frames instead of queueing them.
//! Acknowledgements (frame replies and snapshot replies, posted from shard
//! threads) come back as [`Ack`] messages through an [`AckPort`], so tests drive
//! the engine with a fake clock and a fake sink.
//!
//! [`AnimationWorker`] runs it on one owned thread behind a mailbox that takes
//! new jobs (at most one pending per window, the newest replaces the older)
//! and acknowledgements, woken by either, by a display frame clock tick (see
//! [`super::pacing`]), by the frame timer of a display without a clock or by
//! cancellation, never sleeping under a lock.
//!
//! Every job ends with exactly one [`AnimationSink::complete`], acknowledging
//! the exact plan sequence it was created for. A job that is superseded,
//! cancelled or sealed acknowledges a typed no-send for *its own* plan, so it
//! can never mark a successor applied. When such a job still has a frame in
//! flight (submitted, not yet answered) its completion waits for that answer
//! and reports it if the frame was issued, so the report is truthful.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::ops::AddAssign;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use stache_macos::ax::FrameRead;
use stache_macos::windows::{
    FrameReply, FrameReplyFn, FrameStage, IssuedKind, NativeStatus, NotIssuedReason,
    WindowActionOutcome, WindowInstance,
};
use stache_macos::{ApplicationId, DisplayId, FrameClockFactory, StuckClock, WakeFn};
use stache_sdk::{CancelListener, CancellationToken};

use super::fallback::{FallbackReason, FallbackSet, MotionKind};
use super::metrics::{AnimationRecord, LatencyRing, MetricsBook, Trigger};
use super::pacing::{ClockSet, Notes};
use super::size::SizePlan;
use super::{
    AnimationSettings, MAX_ANIMATED, MAX_DURATION_MS, MIN_ANIMATION_DISTANCE,
    RETARGET_READ_PERIODS, SETTLE, SETTLE_READ_BUDGET, SpringState, Transition, apply_easing,
};
use crate::actor::{Clock, join_within};
use crate::config::EasingType;
use crate::geometry::Rect;
use crate::outcome::FrameComponents;
use crate::state::Stamp;

/// One animated frame plan for one window.
#[derive(Clone, Copy, Debug)]
pub struct AnimationJob {
    /// The registry generation and backend epoch.
    pub stamp: Stamp,
    /// The accepted configuration revision the plan was made under.
    pub revision: u64,
    /// The layout plan sequence this job acknowledges.
    pub plan_sequence: u64,
    /// The exact window.
    pub window: WindowInstance,
    /// The frame the planner believed the window had.
    pub from: Rect,
    /// The desired final frame.
    pub target: Rect,
    /// The components to write.
    pub components: FrameComponents,
    /// The frame period (clamped display refresh): the timer pacing period and
    /// the basis of the retarget and stall deadlines.
    pub period: Duration,
    /// The display containing the target rect, whose frame clock paces the job.
    /// `None` when it is unknown: the job is paced by the timer.
    pub display: Option<DisplayId>,
    /// The start and the target are on different displays.
    pub cross_display: bool,
    /// Duration and easing.
    pub settings: AnimationSettings,
}

/// What one animation did with its frames: the counters part 06 builds
/// metrics on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameCounters {
    /// Frames the coordinator computed and sent.
    pub scheduled: usize,
    /// Frames the shard acknowledged as written.
    pub written: usize,
    /// Frames a newer frame replaced in the shard mailbox before they were
    /// written.
    pub dropped: usize,
}

impl AddAssign for FrameCounters {
    fn add_assign(&mut self, other: Self) {
        self.scheduled += other.scheduled;
        self.written += other.written;
        self.dropped += other.dropped;
    }
}

/// One frame the coordinator asks the sink to write.
#[derive(Clone, Copy, Debug)]
pub struct AnimationFrame {
    /// The job the frame belongs to.
    pub job: AnimationJob,
    /// The frame to write.
    pub frame: Rect,
    /// First, intermediate or final.
    pub stage: FrameStage,
    /// The components this frame writes (see [`super::size`]); not the job's.
    pub components: FrameComponents,
}

/// The callback a snapshot read is answered through, exactly once. `None`
/// when the frame could not be read.
pub type ReadReplyFn = Box<dyn FnOnce(Option<FrameRead>) + Send>;

/// Why a read was requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadKind {
    /// The one read at retarget time.
    Retarget,
    /// The one read after the final write settled.
    Settle,
}

/// An acknowledgement coming back to the coordinator.
#[derive(Clone, Debug)]
pub struct Ack {
    window: WindowInstance,
    plan_sequence: u64,
    body: AckBody,
}

#[derive(Clone, Debug)]
enum AckBody {
    Frame {
        seq: u64,
        reply: FrameReply,
    },
    Read {
        kind: ReadKind,
        frame: Option<FrameRead>,
    },
}

/// Where acknowledgements are posted. It must not block: it runs on shard
/// threads, or inline on the coordinator while a batch is submitted.
pub type AckPort = Arc<dyn Fn(Ack) + Send + Sync>;

/// Where an animation sends its writes and reads, and its completions.
///
/// Every reply handed to a `submit_*` method runs exactly once, on another
/// thread or inline; the engine never waits for one.
pub trait AnimationSink: Send + Sync {
    /// One batch of frames for one application's mailbox. The implementation
    /// re-checks the latest plan, seal, generation and cancellation, and the
    /// shard re-checks generation, revision, token, plan sequence and deadline
    /// at the native send. A frame that is inert or refused is answered with a
    /// typed no-send.
    fn submit_batch(&self, application: ApplicationId, frames: Vec<(AnimationFrame, FrameReplyFn)>);
    /// The one bounded readback after the final write settled.
    fn submit_settle(&self, job: &AnimationJob, reply: ReadReplyFn);
    /// The one bounded readback at retarget time.
    fn submit_retarget_read(&self, job: &AnimationJob, reply: ReadReplyFn);
    /// Acknowledges the job's exact plan and releases its outstanding slot.
    fn complete(&self, job: &AnimationJob, outcome: WindowActionOutcome, counters: FrameCounters);
    /// Why the runtime is stopping (sealed, else cancelled), if it is.
    fn stopping(&self) -> Option<NotIssuedReason>;
    /// Whether `application` shares the overflow AX thread (the fallback
    /// policy names it as the reason). Defaults to no.
    fn is_overflow(&self, _application: ApplicationId) -> bool { false }
}

/// What the coordinator, the actor and status share: the metrics book (written
/// at job end, read by status) and the instant-fallback set. Neither lock is
/// ever held across a call-out.
#[derive(Clone, Default)]
pub struct AnimationShared {
    /// Per-application animation metrics.
    pub metrics: Arc<Mutex<MetricsBook>>,
    /// Applications that move without animation.
    pub fallback: Arc<FallbackSet>,
}

impl AnimationShared {
    /// The metrics book, locked.
    pub fn book(&self) -> MutexGuard<'_, MetricsBook> {
        self.metrics.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What one running job measured, kept in fixed buffers.
#[derive(Clone, Copy, Debug, Default)]
struct Timing {
    /// When the first frame was sent.
    first_sent: Option<Instant>,
    /// When the final frame was acknowledged as written.
    final_acked: Option<Instant>,
    /// When the final frame was due: the display tick that produced it for a
    /// clocked job, the timer deadline it was computed at otherwise.
    scheduled_end: Option<Instant>,
    /// The fallback cut the animation short: no metrics.
    forced: bool,
    /// The job's frames were paced by a live display frame clock.
    clocked: bool,
    latency: LatencyRing,
}

struct InFlight {
    seq: u64,
    sent: Instant,
    frame: Rect,
    stage: FrameStage,
    components: FrameComponents,
}

/// A frame the coordinator decided to send.
struct Outgoing {
    index: usize,
    frame: Rect,
    stage: FrameStage,
    components: FrameComponents,
}

/// A clocked job that sees no tick for this many frame periods is paced by its
/// timer until the clock ticks again, so a stalled clock cannot strand it.
pub const CLOCK_STALL_PERIODS: u32 = 4;

struct Run {
    transition: Transition,
    duration: Duration,
    instant: bool,
    started: Instant,
    last: Instant,
    spring: Option<SpringState>,
    next_frame: Instant,
    /// A clocked job whose clock stopped ticking: paced by its timer at the
    /// normal period until a tick arrives again.
    stalled: bool,
    sent_first: bool,
    sizes: SizePlan,
}

impl Run {
    /// When a frame is due without a tick: the next timer frame, or for a
    /// clocked job the stall deadline (the timer once it has stalled).
    fn deadline(&self, period: Duration, clocked: bool) -> Instant {
        if clocked && !self.stalled {
            self.next_frame + period * (CLOCK_STALL_PERIODS - 1)
        } else {
            self.next_frame
        }
    }

    /// The frame to write at `now` and whether it is the final one.
    fn frame_at(&mut self, job: &AnimationJob, now: Instant) -> (Rect, bool) {
        if self.instant {
            return (job.target, true);
        }
        let elapsed = now.saturating_duration_since(self.started);
        if let Some(spring) = self.spring.as_mut() {
            let dt = now.saturating_duration_since(self.last).as_secs_f64();
            self.last = now;
            let (_, settled) = spring.update(dt);
            let capped = elapsed > Duration::from_millis(u64::from(MAX_DURATION_MS));
            if settled || capped {
                return (job.target, true);
            }
            return (
                self.transition.interpolate(spring.calculate_position(spring.elapsed)),
                false,
            );
        }
        let progress = if self.duration.is_zero() {
            1.0
        } else {
            (elapsed.as_secs_f64() / self.duration.as_secs_f64()).min(1.0)
        };
        if progress >= 1.0 {
            return (job.target, true);
        }
        (
            self.transition.interpolate(apply_easing(progress, job.settings.easing)),
            false,
        )
    }
}

enum Phase {
    /// Waiting for the retarget read; `fallback` is where the window starts if
    /// the read fails or is late.
    Reading { deadline: Instant, fallback: Rect },
    /// Computing and sending frames.
    Running(Run),
    /// The final frame is sent; waiting for its acknowledgement.
    AwaitFinal,
    /// The final frame was written; the window settles before the read.
    Settling {
        until: Instant,
        outcome: WindowActionOutcome,
    },
    /// The settle read is out.
    SettleRead {
        deadline: Instant,
        outcome: WindowActionOutcome,
    },
}

struct Active {
    job: AnimationJob,
    phase: Phase,
    in_flight: Vec<InFlight>,
    /// The last frame the shard acknowledged as written.
    last_acked: Option<Rect>,
    counters: FrameCounters,
    timing: Timing,
}

impl Active {
    const fn new(job: AnimationJob, phase: Phase) -> Self {
        Self {
            job,
            phase,
            in_flight: Vec::new(),
            last_acked: None,
            counters: FrameCounters {
                scheduled: 0,
                written: 0,
                dropped: 0,
            },
            timing: Timing {
                first_sent: None,
                final_acked: None,
                scheduled_end: None,
                forced: false,
                clocked: false,
                latency: LatencyRing::new(),
            },
        }
    }

    /// Ends the animation early: the next tick writes the exact target once.
    fn force_final(&mut self, now: Instant) {
        if let Phase::Reading { fallback, .. } = self.phase {
            self.start(fallback, now);
        }
        if let Phase::Running(run) = &mut self.phase {
            run.instant = true;
            self.timing.forced = true;
        }
    }

    /// Begins computing frames from `start`.
    fn start(&mut self, start: Rect, now: Instant) {
        let transition = Transition::new(start, self.job.target);
        let distance = transition.max_distance();
        let duration = self.job.settings.calculate_duration(distance);
        let spring =
            (self.job.settings.easing == EasingType::Spring).then(|| SpringState::new(duration));
        let instant = distance < MIN_ANIMATION_DISTANCE;
        self.phase = Phase::Running(Run {
            transition,
            duration,
            instant,
            started: now,
            last: now,
            spring,
            next_frame: if instant { now } else { now + self.job.period },
            stalled: false,
            sent_first: false,
            sizes: SizePlan {
                wanted: self.job.components,
                start: (start.width, start.height),
                target: (self.job.target.width, self.job.target.height),
                instant,
            },
        });
    }

    /// When this job next needs the coordinator, if it does.
    fn due(&self, clocked: &BTreeSet<DisplayId>) -> Option<Instant> {
        match &self.phase {
            Phase::Reading { deadline, .. } | Phase::SettleRead { deadline, .. } => Some(*deadline),
            Phase::Running(run) => Some(run.deadline(
                self.job.period,
                self.job.display.is_some_and(|d| clocked.contains(&d)),
            )),
            Phase::Settling { until, .. } => Some(*until),
            Phase::AwaitFinal => None,
        }
    }

    /// The outcome to report if the job ends without a verdict of its own: the
    /// final write's, once it was acknowledged, else the typed no-send.
    const fn verdict(&self, reason: NotIssuedReason) -> WindowActionOutcome {
        match &self.phase {
            Phase::Settling { outcome, .. } | Phase::SettleRead { outcome, .. } => *outcome,
            _ => WindowActionOutcome::NotIssued(reason),
        }
    }
}

/// A job that ended with a frame still in flight: its completion waits for the
/// answer.
struct Ending {
    job: AnimationJob,
    outcome: WindowActionOutcome,
    in_flight: Vec<InFlight>,
    counters: FrameCounters,
    timing: Timing,
}

/// Whether `outcome` is a write the shard accepted for every requested part.
fn is_written(outcome: &WindowActionOutcome, components: FrameComponents) -> bool {
    let WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        native: NativeStatus::Frame { position, size },
        ..
    } = outcome
    else {
        return false;
    };
    let written = |wanted: bool, status: &stache_macos::ax::WriteStatus| {
        !wanted || matches!(status, stache_macos::ax::WriteStatus::Written)
    };
    written(components.position, position) && written(components.size, size)
}

/// The deterministic animation coordinator.
pub struct Animator {
    sink: Arc<dyn AnimationSink>,
    port: AckPort,
    active: Vec<Active>,
    ending: Vec<Ending>,
    seq: u64,
    totals: FrameCounters,
    /// Displays whose frame clock is live: their jobs are paced by its ticks.
    clocked: BTreeSet<DisplayId>,
    shared: AnimationShared,
}

impl Animator {
    /// A coordinator writing through `sink`; acknowledgements are posted to
    /// `port` and handed back through [`Animator::on_ack`].
    #[must_use]
    pub fn new(sink: Arc<dyn AnimationSink>, port: AckPort) -> Self {
        Self::with_shared(sink, port, AnimationShared::default())
    }

    /// Like [`Animator::new`], recording metrics into and consulting the
    /// fallback set of `shared`.
    #[must_use]
    pub fn with_shared(
        sink: Arc<dyn AnimationSink>,
        port: AckPort,
        shared: AnimationShared,
    ) -> Self {
        Self {
            sink,
            port,
            active: Vec::new(),
            ending: Vec::new(),
            seq: 0,
            totals: FrameCounters::default(),
            clocked: BTreeSet::new(),
            shared,
        }
    }

    /// The shared metrics book and fallback set.
    #[must_use]
    pub const fn shared(&self) -> &AnimationShared { &self.shared }

    /// Declares which displays have a live frame clock. A job on any other
    /// display (or on none) is paced by its deadline timer.
    pub fn set_clocked(&mut self, displays: BTreeSet<DisplayId>) { self.clocked = displays; }

    /// The displays that need a frame clock: those of jobs about to run or
    /// running. Settling and ended jobs need none.
    #[must_use]
    pub fn displays_needing_clock(&self) -> BTreeSet<DisplayId> {
        self.active
            .iter()
            .filter(|a| matches!(a.phase, Phase::Reading { .. } | Phase::Running(_)))
            .filter_map(|a| a.job.display)
            .collect()
    }

    /// Whether a job that is about to run or running has no known display.
    #[must_use]
    pub fn has_unplaced(&self) -> bool {
        self.active.iter().any(|a| {
            a.job.display.is_none() && matches!(a.phase, Phase::Reading { .. } | Phase::Running(_))
        })
    }

    /// Jobs running or settling.
    #[must_use]
    pub const fn len(&self) -> usize { self.active.len() }

    /// Whether nothing is running and no ended job awaits a frame answer.
    #[must_use]
    pub const fn is_empty(&self) -> bool { self.active.is_empty() && self.ending.is_empty() }

    /// Jobs that ended but wait for an in-flight frame to answer.
    #[must_use]
    pub const fn deferred(&self) -> usize { self.ending.len() }

    /// The counters of every job that completed.
    #[must_use]
    pub const fn totals(&self) -> FrameCounters { self.totals }

    fn finish(
        &mut self,
        job: &AnimationJob,
        outcome: WindowActionOutcome,
        counters: FrameCounters,
        timing: &Timing,
    ) {
        self.totals += counters;
        self.record(job, counters, timing);
        self.sink.complete(job, outcome, counters);
    }

    /// Records a finished animation and, when its application now fails the
    /// rate or lag criterion in 2 of its last 3, puts it in fallback. The book
    /// is locked for the record only; the reason is worked out after.
    fn record(&self, job: &AnimationJob, counters: FrameCounters, timing: &Timing) {
        let (Some(first), Some(acked), Some(end)) =
            (timing.first_sent, timing.final_acked, timing.scheduled_end)
        else {
            return;
        };
        if timing.forced || counters.scheduled < 2 {
            return;
        }
        let elapsed = acked.saturating_duration_since(first);
        let (write_p50, write_p95) = timing.latency.percentiles();
        let kind = MotionKind::of(job.components.size);
        let record = AnimationRecord {
            plan_sequence: job.plan_sequence,
            kind,
            frames: counters,
            rate_hz: if elapsed.is_zero() {
                f64::INFINITY
            } else {
                f64::from(u32::try_from(counters.written).unwrap_or(u32::MAX))
                    / elapsed.as_secs_f64()
            },
            write_p50,
            write_p95,
            end_lag: acked.saturating_duration_since(end),
            period: job.period,
        };
        let application = job.window.application();
        let trigger = self.shared.book().record(application, record);
        if !record.rate_bad() && !record.lag_bad() {
            self.shared.fallback.healthy(application, kind);
        }
        let Some(trigger) = trigger else { return };
        let reason = if self.sink.is_overflow(application) {
            FallbackReason::Overflow
        } else if !timing.clocked {
            FallbackReason::NoFrameClock
        } else if trigger == Trigger::Rate {
            FallbackReason::BelowMinimumRate
        } else {
            FallbackReason::LateEnd
        };
        self.shared.fallback.insert(application, kind, reason, acked);
    }

    /// Ends `active` with `outcome`: now, or once its in-flight frames answer.
    fn conclude(&mut self, active: Active, outcome: WindowActionOutcome) {
        if active.in_flight.is_empty() {
            self.finish(&active.job, outcome, active.counters, &active.timing);
        } else {
            self.ending.push(Ending {
                job: active.job,
                outcome,
                in_flight: active.in_flight,
                counters: active.counters,
                timing: active.timing,
            });
        }
    }

    fn read_reply(&self, job: &AnimationJob, kind: ReadKind) -> ReadReplyFn {
        let port = Arc::clone(&self.port);
        let (window, plan_sequence) = (job.window, job.plan_sequence);
        Box::new(move |frame| {
            port(Ack {
                window,
                plan_sequence,
                body: AckBody::Read { kind, frame },
            });
        })
    }

    /// Starts `job`. A job already running for the same window is retargeted:
    /// it ends as superseded (acknowledging its own plan, once any in-flight
    /// frame answered) and the new job takes one bounded readback to learn where
    /// the window really is. If that read fails or is late the new job starts
    /// from the old job's last acknowledged written frame, else from
    /// `job.from`; it never starts from the destination the old job was heading
    /// to.
    pub fn admit(&mut self, job: AnimationJob, now: Instant) {
        let mut fallback = job.from;
        let mut retarget = false;
        if let Some(index) = self.active.iter().position(|a| a.job.window == job.window) {
            let old = self.active.remove(index);
            if let Some(acked) = old.last_acked {
                fallback = acked;
            }
            retarget = true;
            let outcome = old.verdict(NotIssuedReason::Superseded);
            self.conclude(old, outcome);
        }
        if self.active.len() >= MAX_ANIMATED {
            // Defense in depth: the worker never admits beyond the cap (its
            // `submit` refuses so the caller writes directly), but should a job
            // arrive anyway the window still reaches its target with one final
            // frame, never a silent drop.
            self.active.push(Active::new(job, Phase::AwaitFinal));
            self.send(
                vec![Outgoing {
                    index: self.active.len() - 1,
                    frame: job.target,
                    stage: FrameStage::Final,
                    components: job.components,
                }],
                now,
            );
            return;
        }
        let mut active = Active::new(job, Phase::AwaitFinal);
        if retarget {
            active.phase = Phase::Reading {
                deadline: now + job.period * RETARGET_READ_PERIODS,
                fallback,
            };
            self.active.push(active);
            let reply = self.read_reply(&job, ReadKind::Retarget);
            self.sink.submit_retarget_read(&job, reply);
        } else {
            active.start(job.from, now);
            self.active.push(active);
        }
    }

    /// Sends the given frames, one batch per application, and records them in
    /// flight.
    fn send(&mut self, frames: Vec<Outgoing>, now: Instant) {
        let mut batches: Vec<(ApplicationId, Vec<(AnimationFrame, FrameReplyFn)>)> = Vec::new();
        for Outgoing {
            index,
            frame,
            stage,
            components,
        } in frames
        {
            let active = &mut self.active[index];
            self.seq += 1;
            let seq = self.seq;
            active.in_flight.push(InFlight {
                seq,
                sent: now,
                frame,
                stage,
                components,
            });
            active.counters.scheduled += 1;
            active.timing.first_sent.get_or_insert(now);
            let port = Arc::clone(&self.port);
            let (window, plan_sequence) = (active.job.window, active.job.plan_sequence);
            let reply: FrameReplyFn = Box::new(move |reply| {
                port(Ack {
                    window,
                    plan_sequence,
                    body: AckBody::Frame { seq, reply },
                });
            });
            let entry = (
                AnimationFrame {
                    job: active.job,
                    frame,
                    stage,
                    components,
                },
                reply,
            );
            let application = window.application();
            match batches.iter_mut().find(|(app, _)| *app == application) {
                Some((_, batch)) => batch.push(entry),
                None => batches.push((application, vec![entry])),
            }
        }
        for (application, batch) in batches {
            self.sink.submit_batch(application, batch);
        }
    }

    /// Folds one acknowledgement in. An acknowledgement for a job that is gone
    /// is ignored.
    pub fn on_ack(&mut self, ack: Ack, now: Instant) {
        let Ack { window, plan_sequence, body } = ack;
        match body {
            AckBody::Frame { seq, reply } => {
                if let Some(index) = self
                    .active
                    .iter()
                    .position(|a| a.job.window == window && a.job.plan_sequence == plan_sequence)
                {
                    self.frame_answered(index, seq, reply, now);
                } else if let Some(index) = self
                    .ending
                    .iter()
                    .position(|e| e.job.window == window && e.job.plan_sequence == plan_sequence)
                {
                    self.ending_answered(index, seq, reply);
                }
            }
            AckBody::Read { kind, frame } => {
                let Some(index) = self
                    .active
                    .iter()
                    .position(|a| a.job.window == window && a.job.plan_sequence == plan_sequence)
                else {
                    return;
                };
                match (kind, &self.active[index].phase) {
                    (ReadKind::Retarget, Phase::Reading { fallback, .. }) => {
                        let fallback = *fallback;
                        let start = frame.and_then(|read| read.complete()).map_or(fallback, |f| {
                            Rect::new(f.origin.x, f.origin.y, f.size.width, f.size.height)
                        });
                        self.active[index].start(start, now);
                    }
                    (ReadKind::Settle, Phase::SettleRead { outcome, .. }) => {
                        let outcome = *outcome;
                        let done = self.active.remove(index);
                        self.conclude(done, with_readback(outcome, frame));
                    }
                    _ => {}
                }
            }
        }
    }

    fn frame_answered(&mut self, index: usize, seq: u64, reply: FrameReply, now: Instant) {
        let active = &mut self.active[index];
        let Some(at) = active.in_flight.iter().position(|f| f.seq == seq) else {
            return;
        };
        let sent = active.in_flight.remove(at);
        let ack = match reply {
            FrameReply::Replaced => {
                active.counters.dropped += 1;
                return;
            }
            FrameReply::Done(ack) => ack,
        };
        if is_written(&ack.outcome, sent.components) {
            active.counters.written += 1;
            active.timing.latency.push(now.saturating_duration_since(sent.sent));
            if sent.stage == FrameStage::Final {
                active.timing.final_acked = Some(now);
            }
            active.last_acked = Some(sent.frame);
            if sent.stage == FrameStage::Final && matches!(active.phase, Phase::AwaitFinal) {
                active.phase = Phase::Settling {
                    until: now + SETTLE,
                    outcome: ack.outcome,
                };
            }
            return;
        }
        // Refused, partial, unresizable, uncertain, stale, expired or not
        // issued: the job ends with that outcome, no final snap and no retry.
        // A straggler from before the final frame decides nothing.
        if matches!(active.phase, Phase::Settling { .. } | Phase::SettleRead { .. }) {
            return;
        }
        let done = self.active.remove(index);
        self.conclude(done, ack.outcome);
    }

    fn ending_answered(&mut self, index: usize, seq: u64, reply: FrameReply) {
        let ending = &mut self.ending[index];
        let Some(at) = ending.in_flight.iter().position(|f| f.seq == seq) else {
            return;
        };
        let sent = ending.in_flight.remove(at);
        match reply {
            FrameReply::Replaced => ending.counters.dropped += 1,
            FrameReply::Done(ack) => {
                if is_written(&ack.outcome, sent.components) {
                    ending.counters.written += 1;
                }
                // An issued frame is the truth about the window; anything else
                // leaves the reason the job ended for.
                if matches!(ack.outcome, WindowActionOutcome::Issued { .. }) {
                    ending.outcome = ack.outcome;
                }
            }
        }
        if ending.in_flight.is_empty() {
            let done = self.ending.remove(index);
            self.finish(&done.job, done.outcome, done.counters, &done.timing);
        }
    }

    /// Runs everything due at `now`. Returns when it next needs to run, or
    /// `None` when nothing is timed (idle, or only waiting for answers).
    pub fn tick(&mut self, now: Instant) -> Option<Instant> {
        self.tick_paced(now, &BTreeMap::new())
    }

    /// Like [`Animator::tick`], with the latest frame-clock tick of each
    /// display. A job on a clocked display computes its frame for that tick's
    /// timestamp (all of a display's jobs for the same one, sent together); a
    /// job without a tick waits for the next, or for its stall deadline.
    pub fn tick_paced(
        &mut self,
        now: Instant,
        ticks: &BTreeMap<DisplayId, Instant>,
    ) -> Option<Instant> {
        if let Some(reason) = self.sink.stopping() {
            self.abort(reason);
            return None;
        }
        let clocked = &self.clocked;
        let fallback = &self.shared.fallback;
        let mut sends: Vec<Outgoing> = Vec::new();
        let mut reads: Vec<usize> = Vec::new();
        let mut expired: Vec<usize> = Vec::new();
        for (index, active) in self.active.iter_mut().enumerate() {
            // An application that fell back mid-animation: one exact final
            // write at this tick and no further intermediate frames.
            if matches!(active.phase, Phase::Reading { .. } | Phase::Running(_))
                && !active.timing.forced
                && fallback.contains(
                    active.job.window.application(),
                    MotionKind::of(active.job.components.size),
                )
            {
                active.force_final(now);
            }
            loop {
                match &mut active.phase {
                    Phase::Reading { deadline, fallback } if now >= *deadline => {
                        let start = *fallback;
                        active.start(start, now);
                        continue;
                    }
                    Phase::Running(run) if run_ready(run, &active.job, clocked, ticks, now) => {
                        let tick = active.job.display.and_then(|d| ticks.get(&d)).copied();
                        let live = active.job.display.is_some_and(|d| clocked.contains(&d));
                        let ticked = tick.filter(|_| live);
                        let due = run.deadline(active.job.period, live);
                        // A clocked job without a tick is stalled: paced by its
                        // timer, and attributed as timer-paced, until a tick.
                        run.stalled = live && ticked.is_none();
                        active.timing.clocked = ticked.is_some();
                        let at = ticked.map_or(now, |t| t.min(now));
                        let (frame, last) = run.frame_at(&active.job, at);
                        let stage = if last {
                            FrameStage::Final
                        } else if run.sent_first {
                            FrameStage::Intermediate
                        } else {
                            FrameStage::First
                        };
                        run.sent_first = true;
                        let write = run.sizes.at(stage, (frame.width, frame.height));
                        let frame = Rect::new(frame.x, frame.y, write.size.0, write.size.1);
                        if last {
                            active.timing.scheduled_end =
                                Some(if ticked.is_some() { at } else { due.min(at) });
                            active.phase = Phase::AwaitFinal;
                        } else {
                            run.next_frame = at + active.job.period;
                        }
                        sends.push(Outgoing {
                            index,
                            frame,
                            stage,
                            components: write.components,
                        });
                    }
                    Phase::Settling { until, outcome } if now >= *until => {
                        let outcome = *outcome;
                        active.phase = Phase::SettleRead {
                            deadline: now + SETTLE_READ_BUDGET,
                            outcome,
                        };
                        reads.push(index);
                    }
                    Phase::SettleRead { deadline, .. } if now >= *deadline => expired.push(index),
                    _ => {}
                }
                break;
            }
        }
        // Frames: one batch per application.
        if !sends.is_empty() {
            self.send(sends, now);
        }
        for index in reads {
            let job = self.active[index].job;
            let reply = self.read_reply(&job, ReadKind::Settle);
            self.sink.submit_settle(&job, reply);
        }
        // A settle read that never answered reports the final outcome as it is.
        for index in expired.into_iter().rev() {
            let done = self.active.remove(index);
            let outcome = done.verdict(NotIssuedReason::Cancelled);
            self.conclude(done, outcome);
        }
        self.active.iter().filter_map(|a| a.due(&self.clocked)).min()
    }

    /// Ends every job for a stop: none sends again. A job with a frame in
    /// flight completes when that frame answers.
    fn abort(&mut self, reason: NotIssuedReason) {
        for active in std::mem::take(&mut self.active) {
            let outcome = active.verdict(reason);
            self.conclude(active, outcome);
        }
    }

    /// Completes every job still waiting for an in-flight answer with what it
    /// has, once (after a contained panic nothing will answer them).
    pub fn complete_ending(&mut self) {
        for done in std::mem::take(&mut self.ending) {
            self.finish(&done.job, done.outcome, done.counters, &done.timing);
        }
    }

    /// Ends every job (a stop): none writes again.
    pub fn abort_all(&mut self) {
        let reason = self.sink.stopping().unwrap_or(NotIssuedReason::Cancelled);
        self.abort(reason);
    }

    /// The windows running or settling.
    #[must_use]
    pub fn windows(&self) -> BTreeSet<WindowInstance> {
        self.active.iter().map(|a| a.job.window).collect()
    }

    /// The last frame the shard acknowledged as written for the window's
    /// running job: where the window was last known to be.
    #[must_use]
    pub fn last_acknowledged(&self, window: WindowInstance) -> Option<Rect> {
        self.active.iter().find(|a| a.job.window == window).and_then(|a| a.last_acked)
    }
}

/// Whether a running job's next frame is due: instant jobs at once; a clocked
/// job on its display's tick (or at its stall deadline); any other at its timer.
fn run_ready(
    run: &Run,
    job: &AnimationJob,
    clocked: &BTreeSet<DisplayId>,
    ticks: &BTreeMap<DisplayId, Instant>,
    now: Instant,
) -> bool {
    if run.instant {
        return true;
    }
    let live = job.display.filter(|d| clocked.contains(d));
    live.is_some_and(|d| ticks.contains_key(&d)) || now >= run.deadline(job.period, live.is_some())
}

/// The final outcome with the settle readback attached, when it is complete.
const fn with_readback(
    outcome: WindowActionOutcome,
    read: Option<FrameRead>,
) -> WindowActionOutcome {
    match (outcome, read) {
        (WindowActionOutcome::Issued { kind, native, .. }, Some(read))
            if read.complete().is_some() =>
        {
            WindowActionOutcome::Issued {
                kind,
                native,
                observed: Some(read),
            }
        }
        (outcome, _) => outcome,
    }
}

// ------------------------------------------------------------------- worker

#[derive(Default)]
struct Mailbox {
    pending: BTreeMap<WindowInstance, AnimationJob>,
    acks: VecDeque<Ack>,
    /// The windows the worker holds (running or settling), a superset while a
    /// step is in progress, so `pending + running` never exceeds the cap.
    running: BTreeSet<WindowInstance>,
    /// A frame clock ticked since the worker last looked.
    ticked: bool,
    closed: bool,
}

/// The mailbox and its condvar: all a frame clock's callback may hold. It owns
/// no clock, so a callback dropping the last reference never tears one down on
/// a `CoreVideo` thread.
struct Core {
    mailbox: Mutex<Mailbox>,
    wake: Condvar,
}

struct Shared {
    core: Arc<Core>,
    sink: Arc<dyn AnimationSink>,
    /// Why displays are paced by the timer.
    notes: Arc<Notes>,
    /// Metrics and the fallback set.
    animation: AnimationShared,
    /// Clocks that did not stop before the thread ended, kept (never freed or
    /// detached) until a later stop retries them.
    quarantine: Mutex<Vec<StuckClock>>,
}

impl std::ops::Deref for Shared {
    type Target = Core;

    fn deref(&self) -> &Core { &self.core }
}

impl Core {
    fn mailbox(&self) -> MutexGuard<'_, Mailbox> {
        self.mailbox.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn close(&self) {
        self.mailbox().closed = true;
        self.wake.notify_all();
    }

    /// Called by a frame clock's thread: a tick is waiting.
    fn tick_wake(&self) {
        self.mailbox().ticked = true;
        self.wake.notify_all();
    }

    fn post(&self, ack: Ack) {
        self.mailbox().acks.push_back(ack);
        self.wake.notify_all();
    }
}

/// The one animation worker: a thread, a bounded latest-plan mailbox and the
/// coordinator. Spawned only while animations are enabled.
pub struct AnimationWorker {
    shared: Arc<Shared>,
    handle: Mutex<Option<JoinHandle<()>>>,
    _cancel: CancelListener,
}

impl AnimationWorker {
    /// Starts the thread. `cancel` closing wakes it at once.
    ///
    /// # Errors
    ///
    /// The thread could not be spawned.
    pub fn spawn(
        sink: Arc<dyn AnimationSink>,
        clock: Arc<dyn Clock>,
        factory: Arc<dyn FrameClockFactory>,
        cancel: &CancellationToken,
        animation: AnimationShared,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            core: Arc::new(Core {
                mailbox: Mutex::new(Mailbox::default()),
                wake: Condvar::new(),
            }),
            sink: Arc::clone(&sink),
            notes: Arc::new(Notes::default()),
            animation,
            quarantine: Mutex::new(Vec::new()),
        });
        // The listener takes the mailbox lock before notifying, so a cancel
        // between the loop's check and its wait is not lost.
        let woken = Arc::clone(&shared);
        let listener = cancel.on_cancel(Arc::new(move || woken.close()));
        let running = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("stache-tiling-animation".to_owned())
            .spawn(move || run(&running, clock.as_ref(), &sink, factory))?;
        Ok(Self {
            shared,
            handle: Mutex::new(Some(handle)),
            _cancel: listener,
        })
    }

    /// Hands `job` to the worker. The newest job for a window replaces an older
    /// pending one (which is acknowledged as superseded).
    ///
    /// # Errors
    ///
    /// The job back, when the worker is closed or its mailbox is full: the
    /// caller then writes directly.
    pub fn submit(&self, job: AnimationJob) -> Result<(), Box<AnimationJob>> {
        let replaced = {
            let mut mailbox = self.shared.mailbox();
            if mailbox.closed {
                return Err(Box::new(job));
            }
            // A window already held (pending or running) is retargeted without
            // growing the set; a new one needs room across pending and running,
            // else the caller writes its target directly.
            let held =
                mailbox.pending.contains_key(&job.window) || mailbox.running.contains(&job.window);
            let total = mailbox.running.len()
                + mailbox.pending.keys().filter(|w| !mailbox.running.contains(w)).count();
            if !held && total >= MAX_ANIMATED {
                return Err(Box::new(job));
            }
            mailbox.pending.insert(job.window, job)
        };
        self.shared.wake.notify_all();
        if let Some(old) = replaced {
            self.shared.sink.complete(
                &old,
                WindowActionOutcome::NotIssued(NotIssuedReason::Superseded),
                FrameCounters::default(),
            );
        }
        Ok(())
    }

    /// Stops admitting and wakes the thread; it ends every running job and
    /// exits once every in-flight frame answered. Waits for nothing.
    pub fn close(&self) { self.shared.close(); }

    /// Why displays are paced by the timer, as status diagnostics.
    #[must_use]
    pub fn pacing_notes(&self) -> Vec<String> { self.shared.notes.list() }

    /// Takes the frame clocks that did not stop before the thread ended.
    #[must_use]
    pub fn take_stuck_clocks(&self) -> Vec<StuckClock> {
        std::mem::take(&mut *self.shared.quarantine.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Jobs waiting in the mailbox.
    #[must_use]
    pub fn pending(&self) -> usize { self.shared.mailbox().pending.len() }

    /// Joins the thread if it ends within `timeout`; a thread that does not end
    /// keeps its handle for a retry (never detached). `true` when joined.
    #[must_use]
    pub fn join_within(&self, timeout: Duration) -> bool { join_within(&self.handle, timeout) }
}

impl Drop for AnimationWorker {
    fn drop(&mut self) { self.shared.close(); }
}

/// What the loop took from the mailbox.
struct Work {
    jobs: BTreeMap<WindowInstance, AnimationJob>,
    acks: Vec<Ack>,
    closed: bool,
}

fn next_work(
    shared: &Shared,
    clock: &dyn Clock,
    animator: &Animator,
    next: Option<Instant>,
    shut: bool,
) -> Work {
    let mut mailbox = shared.mailbox();
    loop {
        if mailbox.closed {
            // Closed: end everything once, then only wait for the answers of
            // frames still in flight.
            if !shut || animator.is_empty() || !mailbox.acks.is_empty() {
                break;
            }
            mailbox = shared.wake.wait(mailbox).unwrap_or_else(PoisonError::into_inner);
            continue;
        }
        if !mailbox.pending.is_empty() || !mailbox.acks.is_empty() || mailbox.ticked {
            break;
        }
        match next {
            None => {
                mailbox = shared.wake.wait(mailbox).unwrap_or_else(PoisonError::into_inner);
            }
            Some(at) => {
                let now = clock.now();
                if now >= at {
                    break;
                }
                mailbox = shared
                    .wake
                    .wait_timeout(mailbox, at - now)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
    }
    mailbox.ticked = false;
    let jobs = std::mem::take(&mut mailbox.pending);
    let acks = mailbox.acks.drain(..).collect();
    mailbox.running.extend(jobs.keys().copied());
    Work {
        jobs,
        acks,
        closed: mailbox.closed,
    }
}

fn run(
    shared: &Arc<Shared>,
    clock: &dyn Clock,
    sink: &Arc<dyn AnimationSink>,
    factory: Arc<dyn FrameClockFactory>,
) {
    // The clocks hold only the wake-only core, never the state that owns
    // clocks, so a callback can never drop one.
    let wake = wake_handle(&shared.core);
    let mut clocks = ClockSet::new(factory, wake, Arc::clone(&shared.notes));
    drive(shared, clock, sink, &mut clocks);
    clocks.stop_all();
    let stuck = clocks.take_stuck();
    if !stuck.is_empty() {
        shared.quarantine.lock().unwrap_or_else(PoisonError::into_inner).extend(stuck);
    }
}

/// The callback a frame clock holds: the wake-only core, never a clock owner.
fn wake_handle(core: &Arc<Core>) -> WakeFn {
    let core = Arc::clone(core);
    Arc::new(move || core.tick_wake())
}

fn drive(
    shared: &Arc<Shared>,
    clock: &dyn Clock,
    sink: &Arc<dyn AnimationSink>,
    clocks: &mut ClockSet,
) {
    let poster = Arc::clone(shared);
    let port: AckPort = Arc::new(move |ack| poster.post(ack));
    let mut animator = Animator::with_shared(Arc::clone(sink), port, shared.animation.clone());
    let mut next: Option<Instant> = None;
    let mut shut = false;
    loop {
        let work = next_work(shared, clock, &animator, next, shut);
        let step = catch_unwind(AssertUnwindSafe(|| {
            let now = clock.now();
            for ack in work.acks {
                animator.on_ack(ack, now);
            }
            if work.closed {
                let reason = sink.stopping().unwrap_or(NotIssuedReason::Cancelled);
                for job in work.jobs.into_values() {
                    sink.complete(
                        &job,
                        WindowActionOutcome::NotIssued(reason),
                        FrameCounters::default(),
                    );
                }
                if !shut {
                    animator.abort_all();
                }
                // Nothing animates again: no clock is needed.
                clocks.stop_all();
                return None;
            }
            for job in work.jobs.into_values() {
                animator.admit(job, now);
            }
            animator.set_clocked(
                clocks.sync(&animator.displays_needing_clock(), animator.has_unplaced()),
            );
            let ticks = clocks.drain();
            let next = animator.tick_paced(clock.now(), &ticks);
            // A display whose last job just sent its final frame needs no clock.
            animator.set_clocked(
                clocks.sync(&animator.displays_needing_clock(), animator.has_unplaced()),
            );
            Some(next)
        }));
        match step {
            Ok(Some(at)) => {
                next = at;
                let held = animator.windows();
                shared.mailbox().running = held;
            }
            Ok(None) => {
                shut = true;
                next = None;
                if animator.is_empty() {
                    return;
                }
            }
            Err(_) => {
                // A contained panic: nothing runs again; release what is held.
                shared.close();
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    animator.abort_all();
                    animator.complete_ending();
                    let pending = std::mem::take(&mut shared.mailbox().pending);
                    for job in pending.into_values() {
                        sink.complete(
                            &job,
                            WindowActionOutcome::NotIssued(
                                sink.stopping().unwrap_or(NotIssuedReason::Cancelled),
                            ),
                            FrameCounters::default(),
                        );
                    }
                }));
                return;
            }
        }
    }
}

#[cfg(test)]
mod wake_tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::ThreadId;

    use super::*;
    use crate::animation::coordinator_tests::FakeSink;

    struct Tracked(Arc<Mutex<Option<ThreadId>>>);

    impl stache_macos::FrameClock for Tracked {
        fn take_tick(&self) -> Option<Instant> { None }

        fn stop(self: Box<Self>, _: Duration) -> Result<(), StuckClock> { Ok(()) }
    }

    impl Drop for Tracked {
        fn drop(&mut self) { *self.0.lock().expect("drop") = Some(std::thread::current().id()); }
    }

    fn shared_with_quarantined_clock(dropped: &Arc<Mutex<Option<ThreadId>>>) -> Arc<Shared> {
        let sink = FakeSink::new();
        Arc::new(Shared {
            core: Arc::new(Core {
                mailbox: Mutex::new(Mailbox::default()),
                wake: Condvar::new(),
            }),
            sink: sink as Arc<dyn AnimationSink>,
            notes: Arc::new(Notes::default()),
            animation: AnimationShared::default(),
            quarantine: Mutex::new(vec![StuckClock::new(Box::new(Tracked(Arc::clone(dropped))))]),
        })
    }

    #[test]
    fn a_clock_callback_holds_no_clock_so_it_can_never_tear_one_down() {
        let dropped = Arc::new(Mutex::new(None));
        let shared = shared_with_quarantined_clock(&dropped);
        let wake = wake_handle(&shared.core);
        wake();
        assert!(shared.mailbox().ticked, "the callback only marks a tick");
        // The worker's state goes away while a callback is still held: the
        // clock drops here, with its owner, not with the callback.
        let owner = std::thread::current().id();
        drop(shared);
        assert_eq!(*dropped.lock().expect("drop"), Some(owner));
        // The last callback reference drops on another thread: no clock exists
        // for it to free.
        let freed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&freed);
        std::thread::spawn(move || {
            wake();
            flag.store(true, Ordering::SeqCst);
        })
        .join()
        .expect("thread");
        assert!(freed.load(Ordering::SeqCst));
        assert_eq!(
            *dropped.lock().expect("drop"),
            Some(owner),
            "not dropped again elsewhere"
        );
    }
}
