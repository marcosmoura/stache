//! The animation lifecycle handle and the live sink.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::ax::FrameRead;
use stache_macos::windows::{NotIssuedReason, WindowActionOutcome};
use stache_sdk::CancellationToken;

use super::{AnimationJob, AnimationSettings, AnimationSink, AnimationWorker};
use crate::actor::{BACKEND_BUDGET, Clock, FrameControl, FrameJob};
use crate::backend::WindowBackend;
use crate::config::AnimationConfig;
use crate::events::Ingress;
use crate::geometry::Rect;
use crate::state::{CompletionResult, Input};

/// The live sink: gated writes through [`FrameControl`], acknowledgements
/// through the actor's mailbox.
pub struct LiveSink {
    backend: Arc<dyn WindowBackend>,
    ingress: Arc<Ingress>,
    frames: Arc<FrameControl>,
    cancel: CancellationToken,
}

impl LiveSink {
    /// A sink over the runtime's backend, mailbox, issuance control and token.
    #[must_use]
    pub const fn new(
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
        }
    }
}

impl AnimationSink for LiveSink {
    fn write(&self, job: &AnimationJob, frame: Rect) -> WindowActionOutcome {
        self.frames.execute(&*self.backend, &self.cancel, &FrameJob {
            generation: job.stamp.registry,
            revision: job.revision,
            plan_sequence: job.plan_sequence,
            window: job.window,
            target: frame,
            components: job.components,
        })
    }

    fn settle(&self, job: &AnimationJob) -> Option<FrameRead> {
        if self.cancel.is_cancelled() || self.frames.is_sealed() {
            return None;
        }
        match self
            .backend
            .snapshot(job.window, &self.cancel, Some(Instant::now() + BACKEND_BUDGET))
        {
            stache_macos::windows::SnapshotOutcome::Snapshot(snapshot) => Some(snapshot.frame),
            _ => None,
        }
    }

    fn complete(&self, job: &AnimationJob, outcome: WindowActionOutcome) {
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

    fn stop_reason(&self) -> NotIssuedReason {
        if self.frames.is_sealed() {
            NotIssuedReason::Sealed
        } else {
            NotIssuedReason::Cancelled
        }
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
}

/// Owns at most one animation worker. Construction is inert: no thread, timer
/// or allocation beyond this handle until an enabled configuration arrives.
pub struct AnimationControl {
    sink: Arc<dyn AnimationSink>,
    clock: Arc<dyn Clock>,
    cancel: CancellationToken,
    slot: Mutex<Slot>,
}

impl AnimationControl {
    /// A control over `sink`; `cancel` wakes and closes any worker.
    #[must_use]
    pub fn new(
        sink: Arc<dyn AnimationSink>,
        clock: Arc<dyn Clock>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            sink,
            clock,
            cancel,
            slot: Mutex::new(Slot {
                worker: None,
                retired: Vec::new(),
                settings: AnimationSettings::default(),
                stopped: false,
                spawn_failure: None,
            }),
        }
    }

    /// The live control of a runtime.
    #[must_use]
    pub fn live(
        backend: Arc<dyn WindowBackend>,
        ingress: Arc<Ingress>,
        frames: Arc<FrameControl>,
        clock: Arc<dyn Clock>,
        cancel: CancellationToken,
    ) -> Self {
        let sink = LiveSink::new(backend, ingress, frames, cancel.clone());
        Self::new(Arc::new(sink), clock, cancel)
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
                    &self.cancel,
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
            if let Some(worker) = slot.worker.take() {
                worker.close();
                slot.retired.push(worker);
            }
        }
        // Reap workers that already ended, so enable/disable cycles stay bounded.
        slot.retired.retain(|worker| !worker.join_within(Duration::ZERO));
        drop(slot);
        Ok(())
    }

    /// Why the latest enabled configuration has no animation worker, if it
    /// could not be spawned. Writes are then direct; this is the observable
    /// reason for that feature loss.
    #[must_use]
    pub fn spawn_failure(&self) -> Option<String> { self.slot().spawn_failure.clone() }

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
    /// a native send keeps its handle. `true` when none is left.
    #[must_use]
    pub fn join_within(&self, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        let workers: Vec<Arc<AnimationWorker>> = self.slot().retired.clone();
        let mut all = true;
        for worker in &workers {
            let remaining =
                deadline.map_or(timeout, |d| d.saturating_duration_since(Instant::now()));
            all &= worker.join_within(remaining);
        }
        if all {
            self.slot()
                .retired
                .retain(|w| !workers.iter().any(|joined| Arc::ptr_eq(w, joined)));
        }
        all
    }

    /// Whether an animation worker is running (false while disabled).
    #[must_use]
    pub fn is_running(&self) -> bool { self.slot().worker.is_some() }

    /// Workers closed but not yet joined.
    #[must_use]
    pub fn retired(&self) -> usize { self.slot().retired.len() }
}
