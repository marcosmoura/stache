//! The animation engine and the one worker thread around it.
//!
//! [`Animator`] is a deterministic stepper: it is handed the time, writes
//! through an [`AnimationSink`] and returns when it next needs to run, so tests
//! drive it with a fake clock and a fake writer. [`AnimationWorker`] runs it on
//! one owned thread behind a latest-plan mailbox: at most one pending job per
//! window (the newest replaces the older), woken by a submission or by
//! cancellation, never sleeping under a lock.
//!
//! Every job ends with exactly one [`AnimationSink::complete`], acknowledging
//! the exact plan sequence it was created for. A job that is superseded, cancelled
//! or sealed acknowledges a typed no-send for *its own* plan, so it can never
//! mark a successor applied.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use stache_macos::ax::FrameRead;
use stache_macos::windows::{IssuedKind, NotIssuedReason, WindowActionOutcome, WindowInstance};
use stache_sdk::{CancelListener, CancellationToken};

use super::{
    AnimationSettings, MAX_ANIMATED, MAX_DURATION_MS, MIN_ANIMATION_DISTANCE, SETTLE, SpringState,
    Transition, apply_easing,
};
use crate::actor::{Clock, join_within};
use crate::config::EasingType;
use crate::geometry::Rect;
use crate::outcome::{FrameComponents, OperationOutcome, classify_frame};
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
    /// The frame period (clamped display refresh).
    pub period: Duration,
    /// Duration and easing.
    pub settings: AnimationSettings,
}

/// Where an animation sends its writes and acknowledgements.
pub trait AnimationSink: Send + Sync {
    /// One gated frame write (an intermediate or the final frame). The
    /// implementation re-checks generation, revision, token, plan sequence,
    /// deadline and seal at the native send.
    fn write(&self, job: &AnimationJob, frame: Rect) -> WindowActionOutcome;
    /// The one bounded readback after settling. `None` when it cannot be read
    /// or the runtime is stopping.
    fn settle(&self, job: &AnimationJob) -> Option<FrameRead>;
    /// Acknowledges the job's exact plan and releases its outstanding slot.
    fn complete(&self, job: &AnimationJob, outcome: WindowActionOutcome);
    /// Why a stop reaches every job: sealed, else cancelled.
    fn stop_reason(&self) -> NotIssuedReason;
}

struct Settling {
    until: Instant,
    outcome: WindowActionOutcome,
}

struct Active {
    job: AnimationJob,
    transition: Transition,
    duration: Duration,
    instant: bool,
    started: Instant,
    last: Instant,
    spring: Option<SpringState>,
    /// The frame of the last readback: where the window really is.
    confirmed: Option<Rect>,
    next_frame: Instant,
    settling: Option<Settling>,
}

impl Active {
    /// The frame to write at `now` and whether it is the final one.
    fn frame_at(&mut self, now: Instant) -> (Rect, bool) {
        if self.instant {
            return (self.job.target, true);
        }
        let elapsed = now.saturating_duration_since(self.started);
        if let Some(spring) = self.spring.as_mut() {
            let dt = now.saturating_duration_since(self.last).as_secs_f64();
            self.last = now;
            let (_, settled) = spring.update(dt);
            let capped = elapsed > Duration::from_millis(u64::from(MAX_DURATION_MS));
            if settled || capped {
                return (self.job.target, true);
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
            return (self.job.target, true);
        }
        (
            self.transition.interpolate(apply_easing(progress, self.job.settings.easing)),
            false,
        )
    }
}

/// The deterministic animation stepper.
pub struct Animator {
    sink: Arc<dyn AnimationSink>,
    active: Vec<Active>,
}

impl Animator {
    /// A stepper writing through `sink`.
    #[must_use]
    pub fn new(sink: Arc<dyn AnimationSink>) -> Self { Self { sink, active: Vec::new() } }

    /// Jobs running or settling.
    #[must_use]
    pub const fn len(&self) -> usize { self.active.len() }

    /// Whether nothing is running.
    #[must_use]
    pub const fn is_empty(&self) -> bool { self.active.is_empty() }

    /// Starts `job`. A job already running for the same window is retargeted:
    /// it ends as superseded (acknowledging its own plan) and the new job starts
    /// from the **confirmed** position of the last readback, not from the
    /// destination the old job was heading to or the planner's stale belief.
    pub fn admit(&mut self, job: AnimationJob, now: Instant) {
        let mut start = job.from;
        if let Some(index) = self.active.iter().position(|a| a.job.window == job.window) {
            let old = self.active.remove(index);
            if let Some(confirmed) = old.confirmed {
                start = confirmed;
            }
            self.end(old, NotIssuedReason::Superseded);
        }
        if self.active.len() >= MAX_ANIMATED {
            // Defense in depth: the worker never admits beyond the cap (its
            // `submit` refuses so the caller writes directly), but should a job
            // arrive anyway the window still reaches its target with one direct
            // write, never a silent drop.
            let outcome = self.sink.write(&job, job.target);
            self.sink.complete(&job, outcome);
            return;
        }
        let transition = Transition::new(start, job.target);
        let distance = transition.max_distance();
        let duration = job.settings.calculate_duration(distance);
        let spring =
            (job.settings.easing == EasingType::Spring).then(|| SpringState::new(duration));
        self.active.push(Active {
            job,
            transition,
            duration,
            instant: distance < MIN_ANIMATION_DISTANCE,
            started: now,
            last: now,
            spring,
            confirmed: None,
            next_frame: now + job.period,
            settling: None,
        });
    }

    /// Ends an active job without a further write.
    fn end(&self, active: Active, reason: NotIssuedReason) {
        let outcome = active.settling.map_or(WindowActionOutcome::NotIssued(reason), |settling| {
            settling.outcome
        });
        self.sink.complete(&active.job, outcome);
    }

    /// Runs everything due at `now`. Returns when it next needs to run, or
    /// `None` when idle.
    pub fn tick(&mut self, now: Instant) -> Option<Instant> {
        let mut next: Option<Instant> = None;
        let mut index = 0;
        while index < self.active.len() {
            let due = match &self.active[index].settling {
                Some(settling) => settling.until,
                None => self.active[index].next_frame,
            };
            if now < due {
                next = Some(soonest(next, due));
                index += 1;
                continue;
            }
            if self.active[index].settling.is_some() {
                let done = self.active.remove(index);
                self.finish_settled(&done);
                continue;
            }
            let (frame, last) = self.active[index].frame_at(now);
            let outcome = self.sink.write(&self.active[index].job, frame);
            let kept = self.accept(index, now, frame, last, outcome);
            if kept {
                let a = &self.active[index];
                next = Some(soonest(
                    next,
                    a.settling.as_ref().map_or(a.next_frame, |s| s.until),
                ));
                index += 1;
            } else {
                self.active.remove(index);
            }
        }
        next
    }

    /// Folds a write outcome into the job. `false` when the job ended (and was
    /// acknowledged): a target that is refused, partial, unresizable, stale or
    /// no longer authorized stops animating, with no final snap and no retry.
    fn accept(
        &mut self,
        index: usize,
        now: Instant,
        frame: Rect,
        last: bool,
        outcome: WindowActionOutcome,
    ) -> bool {
        let a = &mut self.active[index];
        let confirmed = matches!(outcome, WindowActionOutcome::Issued {
            kind: IssuedKind::Applied,
            ..
        }) && matches!(
            classify_frame(
                a.job.window,
                a.job.plan_sequence,
                frame,
                a.job.components,
                &outcome
            ),
            OperationOutcome::Applied { .. }
        );
        if !confirmed {
            self.sink.complete(&a.job, outcome);
            return false;
        }
        if let WindowActionOutcome::Issued { observed: Some(read), .. } = &outcome
            && let Some(read) = read.complete()
        {
            a.confirmed = Some(Rect::new(
                read.origin.x,
                read.origin.y,
                read.size.width,
                read.size.height,
            ));
        }
        if last {
            a.settling = Some(Settling { until: now + SETTLE, outcome });
        } else {
            a.next_frame = now + a.job.period;
        }
        true
    }

    /// The final write was acknowledged and the window had its settling time:
    /// one readback, reported with the final outcome.
    fn finish_settled(&self, active: &Active) {
        let Some(Settling { outcome, .. }) = active.settling else {
            return;
        };
        let outcome = match (outcome, self.sink.settle(&active.job)) {
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
        };
        self.sink.complete(&active.job, outcome);
    }

    /// Ends every job (a stop): none writes again.
    pub fn abort_all(&mut self) {
        let reason = self.sink.stop_reason();
        for active in std::mem::take(&mut self.active) {
            self.end(active, reason);
        }
    }

    /// The windows running or settling.
    #[must_use]
    pub fn windows(&self) -> BTreeSet<WindowInstance> {
        self.active.iter().map(|a| a.job.window).collect()
    }

    /// Where the window really is, per the last readback of its running job.
    #[must_use]
    pub fn confirmed(&self, window: WindowInstance) -> Option<Rect> {
        self.active.iter().find(|a| a.job.window == window).and_then(|a| a.confirmed)
    }
}

fn soonest(current: Option<Instant>, at: Instant) -> Instant { current.map_or(at, |c| c.min(at)) }

// ------------------------------------------------------------------- worker

#[derive(Default)]
struct Mailbox {
    pending: BTreeMap<WindowInstance, AnimationJob>,
    /// The windows the worker holds (running or settling), a superset while a
    /// step is in progress, so `pending + running` never exceeds the cap.
    running: BTreeSet<WindowInstance>,
    closed: bool,
}

struct Shared {
    mailbox: Mutex<Mailbox>,
    wake: Condvar,
    sink: Arc<dyn AnimationSink>,
}

impl Shared {
    fn mailbox(&self) -> MutexGuard<'_, Mailbox> {
        self.mailbox.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn close(&self) {
        self.mailbox().closed = true;
        self.wake.notify_all();
    }
}

/// The one animation worker: a thread, a bounded latest-plan mailbox and the
/// stepper. Spawned only while animations are enabled.
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
        cancel: &CancellationToken,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox::default()),
            wake: Condvar::new(),
            sink: Arc::clone(&sink),
        });
        // The listener takes the mailbox lock before notifying, so a cancel
        // between the loop's check and its wait is not lost.
        let woken = Arc::clone(&shared);
        let listener = cancel.on_cancel(Arc::new(move || woken.close()));
        let running = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("stache-tiling-animation".to_owned())
            .spawn(move || run(&running, clock.as_ref(), sink))?;
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
            self.shared
                .sink
                .complete(&old, WindowActionOutcome::NotIssued(NotIssuedReason::Superseded));
        }
        Ok(())
    }

    /// Stops admitting and wakes the thread; it ends every running job and
    /// exits. Waits for nothing.
    pub fn close(&self) { self.shared.close(); }

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

fn run(shared: &Shared, clock: &dyn Clock, sink: Arc<dyn AnimationSink>) {
    let mut animator = Animator::new(sink);
    let mut next: Option<Instant> = None;
    loop {
        let (jobs, closed) = {
            let mut mailbox = shared.mailbox();
            loop {
                if mailbox.closed || !mailbox.pending.is_empty() {
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
            let jobs = std::mem::take(&mut mailbox.pending);
            let taken: Vec<WindowInstance> = jobs.keys().copied().collect();
            mailbox.running.extend(taken);
            (jobs, mailbox.closed)
        };
        let step = catch_unwind(AssertUnwindSafe(|| {
            let now = clock.now();
            if closed {
                for job in jobs.into_values() {
                    shared
                        .sink
                        .complete(&job, WindowActionOutcome::NotIssued(shared.sink.stop_reason()));
                }
                animator.abort_all();
                return None;
            }
            for job in jobs.into_values() {
                animator.admit(job, now);
            }
            Some(animator.tick(clock.now()))
        }));
        match step {
            Ok(Some(at)) => {
                next = at;
                let held = animator.windows();
                shared.mailbox().running = held;
            }
            Ok(None) => return,
            Err(_) => {
                // A contained panic: nothing runs again; release what is held.
                shared.close();
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    animator.abort_all();
                    let pending = std::mem::take(&mut shared.mailbox().pending);
                    for job in pending.into_values() {
                        shared.sink.complete(
                            &job,
                            WindowActionOutcome::NotIssued(shared.sink.stop_reason()),
                        );
                    }
                }));
                return;
            }
        }
    }
}
