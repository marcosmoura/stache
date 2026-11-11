//! Issuance control for frame writes.
//!
//! [`FrameControl`] is the one place that turns a pure
//! [`crate::Effect::WriteFrame`] into a gated native request and tracks what is
//! outstanding. It holds:
//!
//! * the **latest plan per window** (and its deadline), written by the actor at
//!   dispatch. A queued write whose plan is no longer the latest, or that was
//!   cancelled, becomes inert before it reaches the backend;
//! * an **outstanding counter** that [`FrameControl::drain`] waits on with a
//!   finite, cancellation-wakeable wait.
//!
//! Logical invalidation never goes through this type's lock: it is
//! [`GenerationCell::seal`] (atomic), which the owner re-checks at the actual
//! native send. The lock here protects two small maps and is never held across
//! a backend call, a completion submit or a wait on another party.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::ax::{AxPoint, AxSize};
use stache_macos::windows::{
    ActionRequest, FrameReply, FrameReplyFn, FrameRequest, FrameStage, GenerationCell,
    NotIssuedReason, WindowAction, WindowActionOutcome, WindowInstance, WindowIssuance,
};
use stache_sdk::CancellationToken;

use super::Clock;
use crate::backend::WindowBackend;
use crate::borders::{BorderFrameKind, BorderFrames, outcome_frame};
use crate::geometry::Rect;
use crate::outcome::FrameComponents;

/// How long after the actor dispatched a write the owner may still send it.
pub const FRAME_PLAN_DEADLINE: Duration = Duration::from_secs(2);
/// The re-check period of an unbounded drain.
pub const DRAIN_QUANTUM: Duration = Duration::from_millis(50);

/// What [`FrameControl::drain`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrainOutcome {
    /// Nothing is outstanding.
    Drained,
    /// The timeout elapsed with writes still outstanding.
    TimedOut {
        /// Writes still queued or running.
        outstanding: usize,
    },
    /// The caller's token was cancelled with writes still outstanding.
    Cancelled {
        /// Writes still queued or running.
        outstanding: usize,
    },
}

/// One frame write handed to the runner.
#[derive(Clone, Copy, Debug)]
pub struct FrameJob {
    /// The registry generation (the issuance generation).
    pub generation: u64,
    /// The accepted configuration revision.
    pub revision: u64,
    /// The layout plan sequence.
    pub plan_sequence: u64,
    /// The exact window.
    pub window: WindowInstance,
    /// The desired frame.
    pub target: Rect,
    /// The components to write.
    pub components: FrameComponents,
}

#[derive(Default)]
struct Ledger {
    latest: BTreeMap<WindowInstance, (u64, Instant)>,
    outstanding: usize,
}

/// Issuance bookkeeping shared by the actor, the runner and the lifecycle.
pub struct FrameControl {
    gate: Arc<GenerationCell>,
    clock: Arc<dyn Clock>,
    shared: Arc<Shared>,
    /// Where the native borders learn each written frame. Set once at
    /// composition; reporting never waits.
    borders: OnceLock<Arc<dyn BorderFrames>>,
}

struct Shared {
    ledger: Mutex<Ledger>,
    idle: Condvar,
}

impl FrameControl {
    /// Control over `gate`.
    #[must_use]
    pub fn new(gate: Arc<GenerationCell>, clock: Arc<dyn Clock>) -> Self {
        Self {
            gate,
            clock,
            shared: Arc::new(Shared {
                ledger: Mutex::new(Ledger::default()),
                idle: Condvar::new(),
            }),
            borders: OnceLock::new(),
        }
    }

    /// Reports every frame this control dispatches, and (through the animation
    /// sink) every animated frame, to the native borders. Set once; a second
    /// call is ignored.
    pub fn set_border_frames(&self, borders: Arc<dyn BorderFrames>) {
        let _ = self.borders.set(borders);
    }

    /// Reports one frame to the borders, if any are composed. Never blocks.
    pub fn report_border(
        &self,
        window: WindowInstance,
        plan_sequence: u64,
        frame: Rect,
        kind: BorderFrameKind,
    ) {
        if let Some(borders) = self.borders.get() {
            borders.report(window, plan_sequence, frame, kind);
        }
    }

    /// Reports what a finished write left the window at (see
    /// [`outcome_frame`]).
    pub fn report_border_outcome(
        &self,
        window: WindowInstance,
        plan_sequence: u64,
        target: Rect,
        outcome: &WindowActionOutcome,
        confirmed: BorderFrameKind,
    ) {
        if let Some((frame, kind)) = outcome_frame(target, outcome, confirmed) {
            self.report_border(window, plan_sequence, frame, kind);
        }
    }

    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.shared.ledger.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The actor is about to queue a write: it becomes the latest plan of its
    /// window and counts as outstanding until it finishes or is discarded.
    pub fn note_dispatched(&self, window: WindowInstance, plan_sequence: u64) {
        let deadline = self.clock.now() + FRAME_PLAN_DEADLINE;
        let mut ledger = self.ledger();
        ledger.latest.insert(window, (plan_sequence, deadline));
        ledger.outstanding += 1;
    }

    /// Queueing failed: the write never became outstanding.
    pub fn note_not_queued(&self, window: WindowInstance, plan_sequence: u64) {
        let mut ledger = self.ledger();
        ledger.outstanding = ledger.outstanding.saturating_sub(1);
        if ledger.latest.get(&window).is_some_and(|(plan, _)| *plan == plan_sequence) {
            ledger.latest.remove(&window);
        }
        drop(ledger);
        self.shared.idle.notify_all();
    }

    /// The queued write for `window` is no longer wanted.
    pub fn cancel(&self, window: WindowInstance) { self.ledger().latest.remove(&window); }

    /// Whether `plan_sequence` is the latest plan of `window`.
    #[must_use]
    pub fn is_current(&self, window: WindowInstance, plan_sequence: u64) -> bool {
        self.ledger()
            .latest
            .get(&window)
            .is_some_and(|(plan, _)| *plan == plan_sequence)
    }

    /// Whether the runtime withdrew the action authority for good.
    #[must_use]
    pub fn is_sealed(&self) -> bool { self.gate.is_sealed() }

    /// Writes queued or running.
    #[must_use]
    pub fn outstanding(&self) -> usize { self.ledger().outstanding }

    /// A write finished (or was discarded unrun).
    pub fn finish(&self, window: WindowInstance, plan_sequence: u64) {
        let mut ledger = self.ledger();
        ledger.outstanding = ledger.outstanding.saturating_sub(1);
        if ledger.latest.get(&window).is_some_and(|(plan, _)| *plan == plan_sequence) {
            ledger.latest.remove(&window);
        }
        drop(ledger);
        self.shared.idle.notify_all();
    }

    /// The gated frame request for `job`, or the typed no-send that makes it
    /// inert.
    ///
    /// Inert when the plan is no longer the latest, the runtime is sealed, the
    /// generation is not live or the caller cancelled. Otherwise the shard
    /// re-checks generation, accepted revision, exact token, plan sequence and
    /// deadline at the native send.
    ///
    /// # Errors
    ///
    /// The outcome to report for a frame that must not be sent.
    pub fn request(
        &self,
        cancel: &CancellationToken,
        job: &FrameJob,
        stage: FrameStage,
    ) -> Result<FrameRequest, WindowActionOutcome> {
        let deadline = {
            let ledger = self.ledger();
            match ledger.latest.get(&job.window) {
                Some((plan, deadline)) if *plan == job.plan_sequence => *deadline,
                _ => return Err(WindowActionOutcome::NotIssued(NotIssuedReason::Superseded)),
            }
        };
        if self.gate.is_sealed() {
            return Err(WindowActionOutcome::NotIssued(NotIssuedReason::Sealed));
        }
        let live = self.gate.live_generation();
        if live == 0 || live != job.generation {
            return Err(WindowActionOutcome::NotIssued(
                NotIssuedReason::GenerationInvalid,
            ));
        }
        if cancel.is_cancelled() {
            return Err(WindowActionOutcome::NotIssued(NotIssuedReason::Cancelled));
        }
        let position = job.components.position.then_some(AxPoint {
            x: job.target.x,
            y: job.target.y,
        });
        let size = job.components.size.then_some(AxSize {
            width: job.target.width,
            height: job.target.height,
        });
        Ok(FrameRequest {
            issuance: WindowIssuance::new(
                job.window,
                job.generation,
                job.revision,
                job.plan_sequence,
                deadline,
            ),
            position,
            size,
            stage,
        })
    }

    /// Submits one non-animated frame write without waiting and answers
    /// `reply` exactly once with its outcome: inline for a write that is inert
    /// or refused, otherwise on the application's shard thread (or inline for
    /// an unrouted application). The shard reads the frame back once
    /// ([`FrameStage::Direct`]).
    ///
    /// `reply` must not block and no lock may be held across this call.
    pub fn submit_direct(
        &self,
        backend: &dyn WindowBackend,
        cancel: &CancellationToken,
        job: &FrameJob,
        reply: impl FnOnce(WindowActionOutcome) + Send + 'static,
    ) {
        let request = match self.request(cancel, job, FrameStage::Direct) {
            Ok(request) => request,
            Err(outcome) => return reply(outcome),
        };
        // The border follows the target at once and is corrected by the
        // readback in the reply; a write that is inert reported nothing above.
        self.report_border(
            job.window,
            job.plan_sequence,
            job.target,
            BorderFrameKind::Target,
        );
        let reply = {
            let borders = self.borders.get().cloned();
            let (window, plan_sequence, target) = (job.window, job.plan_sequence, job.target);
            move |outcome: WindowActionOutcome| {
                if let Some(borders) = borders
                    && let Some((frame, kind)) =
                        outcome_frame(target, &outcome, BorderFrameKind::Readback)
                {
                    borders.report(window, plan_sequence, frame, kind);
                }
                reply(outcome);
            }
        };
        // Shared so a refused batch (whose reply never runs) can still answer.
        let slot = Arc::new(Mutex::new(Some(reply)));
        let answer = Arc::clone(&slot);
        let callback: FrameReplyFn = Box::new(move |frame| {
            let outcome = match frame {
                FrameReply::Done(ack) => ack.outcome,
                // A newer frame of the same plan replaced it unwritten.
                FrameReply::Replaced => WindowActionOutcome::NotIssued(NotIssuedReason::Superseded),
            };
            let reply = answer.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(reply) = reply {
                reply(outcome);
            }
        });
        if let Err(error) =
            backend.submit_frames(job.window.application(), vec![(request, callback)])
        {
            let reply = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(reply) = reply {
                reply(WindowActionOutcome::NotIssued(NotIssuedReason::Dispatch(error)));
            }
        }
    }

    /// Raises one exact window through the issuance gate (focus).
    ///
    /// Not a plan: it carries the newest plan sequence so the plan floor never
    /// supersedes it, and the accepted revision the gate holds now. Inert (a
    /// typed no-send) when the runtime is sealed, the generation is not live or
    /// the caller cancelled; the owner re-checks all of it at the native send.
    #[must_use]
    pub fn raise(
        &self,
        backend: &dyn WindowBackend,
        cancel: &CancellationToken,
        window: WindowInstance,
        generation: u64,
    ) -> WindowActionOutcome {
        if self.gate.is_sealed() {
            return WindowActionOutcome::NotIssued(NotIssuedReason::Sealed);
        }
        let live = self.gate.live_generation();
        if live == 0 || live != generation {
            return WindowActionOutcome::NotIssued(NotIssuedReason::GenerationInvalid);
        }
        if cancel.is_cancelled() {
            return WindowActionOutcome::NotIssued(NotIssuedReason::Cancelled);
        }
        let issuance = WindowIssuance::new(
            window,
            generation,
            self.gate.revision(),
            u64::MAX,
            self.clock.now() + FRAME_PLAN_DEADLINE,
        );
        backend.act(
            ActionRequest {
                issuance,
                action: WindowAction::Raise,
            },
            cancel,
        )
    }

    /// Waits until nothing is outstanding, `timeout` elapses or `cancel` fires.
    ///
    /// Cancellation wakes the wait through a listener that takes the ledger
    /// lock before notifying, so a cancel between the check and the sleep is not
    /// lost. The lock is never held across a sleep.
    #[must_use]
    pub fn drain(&self, timeout: Duration, cancel: &CancellationToken) -> DrainOutcome {
        let deadline = Instant::now().checked_add(timeout);
        let shared = Arc::clone(&self.shared);
        let _wake = cancel.on_cancel(Arc::new(move || {
            let _held = shared.ledger.lock().unwrap_or_else(PoisonError::into_inner);
            shared.idle.notify_all();
        }));
        let mut ledger = self.ledger();
        loop {
            if ledger.outstanding == 0 {
                return DrainOutcome::Drained;
            }
            if cancel.is_cancelled() {
                return DrainOutcome::Cancelled {
                    outstanding: ledger.outstanding,
                };
            }
            let remaining =
                deadline.map_or(DRAIN_QUANTUM, |d| d.saturating_duration_since(Instant::now()));
            if remaining.is_zero() {
                return DrainOutcome::TimedOut {
                    outstanding: ledger.outstanding,
                };
            }
            ledger = self
                .shared
                .idle
                .wait_timeout(ledger, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}
