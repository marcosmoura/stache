//! The animation lifecycle handle and the live sink.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::windows::{
    FrameAck, FrameReply, FrameReplyFn, NotIssuedReason, SnapshotOutcome, WindowActionOutcome,
};
use stache_macos::{ApplicationId, FrameClockFactory, StuckClock};
use stache_sdk::CancellationToken;

use super::{
    AnimationFrame, AnimationJob, AnimationSettings, AnimationShared, AnimationSink,
    AnimationWorker, FallbackReason, FrameCounters, MetricsSummary, ReadReplyFn,
};
use crate::actor::{Clock, FrameControl, FrameJob};
use crate::backend::WindowBackend;
use crate::config::AnimationConfig;
use crate::events::Ingress;
use crate::state::{CompletionResult, Input};

/// The live sink: gated, non-blocking frame batches and reads through the
/// backend, completions through the actor's mailbox.
pub struct LiveSink {
    backend: Arc<dyn WindowBackend>,
    ingress: Arc<Ingress>,
    frames: Arc<FrameControl>,
    cancel: CancellationToken,
    logger: Option<stache_sdk::ScopedLogger>,
    origin: crate::logclock::LogOrigin,
}

impl LiveSink {
    /// A sink over the runtime's backend, mailbox, issuance control and token.
    #[must_use]
    pub fn new(
        backend: Arc<dyn WindowBackend>,
        ingress: Arc<Ingress>,
        frames: Arc<FrameControl>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            backend,
            ingress,
            frames,
            cancel,
            logger: None,
            origin: crate::logclock::LogOrigin::new(Instant::now()),
        }
    }

    /// Writes one debug line per finished animation through `logger`.
    #[must_use]
    pub fn with_logger(
        mut self,
        logger: Option<stache_sdk::ScopedLogger>,
        origin: crate::logclock::LogOrigin,
    ) -> Self {
        self.logger = logger;
        self.origin = origin;
        self
    }
}

/// A reply that can be answered from a backend callback or, when the backend
/// refuses the submission (and so never runs the callback), from the caller.
struct Once<T>(Arc<Mutex<Option<T>>>);

impl<T> Clone for Once<T> {
    fn clone(&self) -> Self { Self(Arc::clone(&self.0)) }
}

impl<T> Once<T> {
    fn new(value: T) -> Self { Self(Arc::new(Mutex::new(Some(value)))) }

    fn take(&self) -> Option<T> { self.0.lock().unwrap_or_else(PoisonError::into_inner).take() }
}

impl LiveSink {
    fn submit_read(&self, job: &AnimationJob, reply: ReadReplyFn) {
        if self.stopping().is_some() {
            return reply(None);
        }
        let reply = Once::new(reply);
        let answer = reply.clone();
        let outcome = self.backend.submit_snapshot(
            job.window,
            Box::new(move |outcome| {
                if let Some(reply) = answer.take() {
                    reply(match outcome {
                        SnapshotOutcome::Snapshot(snapshot) => Some(snapshot.frame),
                        _ => None,
                    });
                }
            }),
        );
        if outcome.is_err()
            && let Some(reply) = reply.take()
        {
            reply(None);
        }
    }

    fn stopping(&self) -> Option<NotIssuedReason> {
        if self.frames.is_sealed() {
            Some(NotIssuedReason::Sealed)
        } else if self.cancel.is_cancelled() {
            Some(NotIssuedReason::Cancelled)
        } else {
            None
        }
    }
}

impl AnimationSink for LiveSink {
    fn submit_batch(
        &self,
        application: ApplicationId,
        frames: Vec<(AnimationFrame, FrameReplyFn)>,
    ) {
        let mut batch = Vec::with_capacity(frames.len());
        let mut held = Vec::with_capacity(frames.len());
        for (frame, reply) in frames {
            let job = FrameJob {
                generation: frame.job.stamp.registry,
                revision: frame.job.revision,
                plan_sequence: frame.job.plan_sequence,
                window: frame.job.window,
                target: frame.frame,
                components: frame.components,
            };
            match self.frames.request(&self.cancel, &job, frame.stage) {
                Err(outcome) => {
                    // Inert: answered here, never sent.
                    reply(FrameReply::Done(FrameAck { outcome, sent_at: None }));
                }
                Ok(request) => {
                    let reply = Once::new(reply);
                    let answer = reply.clone();
                    batch.push((
                        request,
                        Box::new(move |frame_reply| {
                            if let Some(reply) = answer.take() {
                                reply(frame_reply);
                            }
                        }) as FrameReplyFn,
                    ));
                    held.push(reply);
                }
            }
        }
        if let Err(error) = self.backend.submit_frames(application, batch) {
            // Nothing was queued and no callback will run.
            for reply in held {
                if let Some(reply) = reply.take() {
                    reply(FrameReply::Done(FrameAck {
                        outcome: WindowActionOutcome::NotIssued(NotIssuedReason::Dispatch(error)),
                        sent_at: None,
                    }));
                }
            }
        }
    }

    fn submit_settle(&self, job: &AnimationJob, reply: ReadReplyFn) {
        self.submit_read(job, reply);
    }

    fn submit_retarget_read(&self, job: &AnimationJob, reply: ReadReplyFn) {
        self.submit_read(job, reply);
    }

    fn complete(&self, job: &AnimationJob, outcome: WindowActionOutcome, counters: FrameCounters) {
        if let Some(logger) = &self.logger {
            logger.debug(format!(
                "{} tiling: animation window {} plan {} ended: {} (frames scheduled {}, \
                 written {}, dropped {}; display {:?}, period {:.1}ms)",
                self.origin.stamp(),
                job.window.serial(),
                job.plan_sequence,
                outcome_label(&outcome),
                counters.scheduled,
                counters.written,
                counters.dropped,
                job.display,
                job.period.as_secs_f64() * 1000.0,
            ));
        }
        // A closed mailbox means the actor is gone; nobody is waiting.
        let _ = self.ingress.submit(Input::Completion {
            stamp: job.stamp,
            result: CompletionResult::Frame {
                window: job.window,
                plan_sequence: job.plan_sequence,
                revision: job.revision,
                outcome,
            },
        });
        self.frames.finish(job.window, job.plan_sequence);
    }

    fn stopping(&self) -> Option<NotIssuedReason> { Self::stopping(self) }

    fn is_overflow(&self, application: ApplicationId) -> bool {
        self.backend.is_overflow(application)
    }
}

struct Slot {
    worker: Option<Arc<AnimationWorker>>,
    /// Workers closed by a disable, kept until joined (never detached).
    retired: Vec<Arc<AnimationWorker>>,
    settings: AnimationSettings,
    /// A stop: no worker is ever spawned again.
    stopped: bool,
    /// Why the latest enabled configuration has no worker (spawn failure).
    spawn_failure: Option<String>,
    /// Frame clocks of ended workers that did not stop: kept (never freed or
    /// detached) and retried by every [`AnimationControl::join_within`].
    quarantine: Vec<StuckClock>,
}

/// Owns at most one animation worker. Construction is inert: no thread, timer
/// or allocation beyond this handle until an enabled configuration arrives.
pub struct AnimationControl {
    sink: Arc<dyn AnimationSink>,
    clock: Arc<dyn Clock>,
    clocks: Arc<dyn FrameClockFactory>,
    cancel: CancellationToken,
    slot: Mutex<Slot>,
    /// Metrics and the instant-fallback set, shared with the actor and status.
    shared: AnimationShared,
}

/// What status reports about animations, read without any AX call.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnimationReport {
    /// Applications that move without animation, with the reason.
    pub fallbacks: Vec<(ApplicationId, FallbackReason)>,
    /// A metrics summary per application with data.
    pub metrics: Vec<(ApplicationId, MetricsSummary)>,
}

impl AnimationControl {
    /// A control over `sink`; `cancel` wakes and closes any worker.
    #[must_use]
    pub fn new(
        sink: Arc<dyn AnimationSink>,
        clock: Arc<dyn Clock>,
        clocks: Arc<dyn FrameClockFactory>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            sink,
            clock,
            clocks,
            cancel,
            slot: Mutex::new(Slot {
                worker: None,
                retired: Vec::new(),
                settings: AnimationSettings::default(),
                stopped: false,
                spawn_failure: None,
                quarantine: Vec::new(),
            }),
            shared: AnimationShared::default(),
        }
    }

    /// The live control of a runtime.
    #[must_use]
    pub fn live(
        backend: Arc<dyn WindowBackend>,
        ingress: Arc<Ingress>,
        frames: Arc<FrameControl>,
        clock: Arc<dyn Clock>,
        clocks: Arc<dyn FrameClockFactory>,
        cancel: CancellationToken,
    ) -> Self {
        Self::live_logged(
            backend,
            ingress,
            frames,
            clock,
            clocks,
            cancel,
            None,
            crate::logclock::LogOrigin::new(Instant::now()),
        )
    }

    /// [`Self::live`] with a debug line per finished animation.
    #[must_use]
    #[allow(clippy::too_many_arguments, reason = "the runtime's shared parts")]
    pub fn live_logged(
        backend: Arc<dyn WindowBackend>,
        ingress: Arc<Ingress>,
        frames: Arc<FrameControl>,
        clock: Arc<dyn Clock>,
        clocks: Arc<dyn FrameClockFactory>,
        cancel: CancellationToken,
        logger: Option<stache_sdk::ScopedLogger>,
        origin: crate::logclock::LogOrigin,
    ) -> Self {
        let sink =
            LiveSink::new(backend, ingress, frames, cancel.clone()).with_logger(logger, origin);
        Self::new(Arc::new(sink), clock, clocks, cancel)
    }

    fn slot(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Applies an accepted configuration: spawns the worker when animations are
    /// enabled, retires it when they are not, and updates duration and easing.
    ///
    /// # Errors
    ///
    /// The worker could not be spawned; writes then happen directly.
    pub fn configure(&self, config: &AnimationConfig) -> std::io::Result<()> {
        let mut slot = self.slot();
        slot.settings = AnimationSettings::from_config(config);
        if config.enabled && !slot.stopped {
            if slot.worker.is_none() {
                match AnimationWorker::spawn(
                    Arc::clone(&self.sink),
                    Arc::clone(&self.clock),
                    Arc::clone(&self.clocks),
                    &self.cancel,
                    self.shared.clone(),
                ) {
                    Ok(worker) => slot.worker = Some(Arc::new(worker)),
                    Err(error) => {
                        slot.spawn_failure = Some(format!(
                            "animations are unavailable: the animation worker could not start \
                             ({error}); windows move without animation"
                        ));
                        return Err(error);
                    }
                }
            }
            slot.spawn_failure = None;
        } else {
            slot.spawn_failure = None;
            // Turning animations off ends the fallback and the history: turning
            // them back on starts fresh.
            self.shared.fallback.clear();
            self.shared.book().clear();
            if let Some(worker) = slot.worker.take() {
                worker.close();
                slot.retired.push(worker);
            }
        }
        // Reap workers that already ended, so enable/disable cycles stay bounded.
        for worker in std::mem::take(&mut slot.retired) {
            if worker.join_within(Duration::ZERO) {
                slot.quarantine.extend(worker.take_stuck_clocks());
            } else {
                slot.retired.push(worker);
            }
        }
        drop(slot);
        Ok(())
    }

    /// Why the latest enabled configuration has no animation worker, if it
    /// could not be spawned. Writes are then direct; this is the observable
    /// reason for that feature loss.
    #[must_use]
    pub fn spawn_failure(&self) -> Option<String> { self.slot().spawn_failure.clone() }

    /// Why displays are paced by their deadline timer instead of a frame clock
    /// (creation failed, or the display is unknown), for status diagnostics.
    #[must_use]
    pub fn pacing_diagnostics(&self) -> Vec<String> {
        self.slot().worker.as_ref().map(|w| w.pacing_notes()).unwrap_or_default()
    }

    /// The applications that move without animation. The actor copies them
    /// into the pure state before each transition.
    #[must_use]
    pub fn fallback_apps(&self) -> std::collections::BTreeSet<ApplicationId> {
        self.shared.fallback.ids()
    }

    /// Puts `application` in instant fallback for `reason`, while animations
    /// are on. `true` when it was newly added.
    pub fn enter_fallback(&self, application: ApplicationId, reason: FallbackReason) -> bool {
        if self.slot().worker.is_none() {
            return false;
        }
        self.shared.fallback.insert(application, reason)
    }

    /// Forgets a terminated application: its fallback and its metrics.
    pub fn forget_application(&self, application: ApplicationId) {
        self.shared.fallback.forget(application);
        self.shared.book().forget(application);
    }

    /// The fallback set and metrics for status.
    #[must_use]
    pub fn report(&self) -> AnimationReport {
        AnimationReport {
            fallbacks: self.shared.fallback.entries(),
            metrics: self.shared.book().summaries(),
        }
    }

    /// The shared metrics and fallback state.
    #[must_use]
    pub const fn shared(&self) -> &AnimationShared { &self.shared }

    /// Offers a job to the worker. The job back when there is none (disabled,
    /// stopped, closed or full): the caller writes directly.
    ///
    /// # Errors
    ///
    /// The job, unchanged but for its settings.
    pub fn submit(&self, mut job: AnimationJob) -> Result<(), Box<AnimationJob>> {
        let (worker, settings) = {
            let slot = self.slot();
            (slot.worker.clone(), slot.settings)
        };
        let Some(worker) = worker else {
            return Err(Box::new(job));
        };
        job.settings = settings;
        worker.submit(job)
    }

    /// Closes every worker for good. Waits for nothing.
    pub fn stop(&self) {
        let mut slot = self.slot();
        slot.stopped = true;
        if let Some(worker) = slot.worker.take() {
            slot.retired.push(worker);
        }
        for worker in &slot.retired {
            worker.close();
        }
    }

    /// Joins every closed worker within `timeout` in total. A worker stuck in
    /// a native send keeps its handle. A worker that ended hands its frame
    /// clocks that did not stop to this control's quarantine, where
    /// [`Self::release_clocks`] retries them. `true` when none is left.
    #[must_use]
    pub fn join_within(&self, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        let workers: Vec<Arc<AnimationWorker>> = self.slot().retired.clone();
        let mut all = true;
        for worker in &workers {
            let remaining =
                deadline.map_or(timeout, |d| d.saturating_duration_since(Instant::now()));
            if worker.join_within(remaining) {
                let stuck = worker.take_stuck_clocks();
                let mut slot = self.slot();
                slot.quarantine.extend(stuck);
                slot.retired.retain(|w| !Arc::ptr_eq(w, worker));
            } else {
                all = false;
            }
        }
        all
    }

    /// Retries every frame clock that did not stop, within `timeout` in total.
    /// A clock that is still stuck stays owned here (never freed, never
    /// detached). `true` when none is left.
    #[must_use]
    pub fn release_clocks(&self, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        let held = std::mem::take(&mut self.slot().quarantine);
        let mut still = Vec::new();
        for stuck in held {
            let remaining =
                deadline.map_or(timeout, |d| d.saturating_duration_since(Instant::now()));
            if let Err(stuck) = stuck.retry(remaining) {
                still.push(stuck);
            }
        }
        let clean = still.is_empty();
        self.slot().quarantine.extend(still);
        clean
    }

    /// Frame clocks that did not stop, kept until a stop succeeds.
    #[must_use]
    pub fn stuck_clocks(&self) -> usize { self.slot().quarantine.len() }

    /// Whether an animation worker is running (false while disabled).
    #[must_use]
    pub fn is_running(&self) -> bool { self.slot().worker.is_some() }

    /// Workers closed but not yet joined.
    #[must_use]
    pub fn retired(&self) -> usize { self.slot().retired.len() }
}

/// A short label for a job's final outcome in a debug line.
fn outcome_label(outcome: &WindowActionOutcome) -> String {
    match outcome {
        WindowActionOutcome::Issued { kind, observed, .. } => {
            format!(
                "issued {kind:?}{}",
                if observed.is_some() {
                    " (read back)"
                } else {
                    ""
                }
            )
        }
        other => format!("{other:?}"),
    }
}
