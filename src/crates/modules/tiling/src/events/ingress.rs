//! The shared mailbox: a short lock around the [`Pipeline`] plus a wakeup.
//!
//! Cancellation and arrival both set state under the same lock the waiter
//! checks before sleeping, so a wakeup between the check and the sleep cannot
//! be lost.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::Instant;

use super::pipeline::{AdmissionStats, Pipeline};
use crate::drag::DRAG_QUEUE_CAPACITY;
use crate::state::Input;

/// Why a submission was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// The actor stopped (cancelled, halted or failed).
    Closed,
}

/// Why an intent was not admitted. Nothing was queued in either case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentSubmitError {
    /// The actor stopped (cancelled, halted or failed).
    Closed,
    /// The bounded semantic queue is full.
    Full,
}

/// Why the actor woke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// Work is queued or the sink has events.
    Work,
    /// The geometry deadline came due.
    Deadline,
    /// The actor must stop.
    Shutdown,
}

struct Inner {
    pipeline: Pipeline,
    shutdown: bool,
    closed: bool,
    sink_dirty: bool,
}

/// Drag signals that arrived while the mailbox lock was busy. Only the input
/// callback pushes (never waiting) and only a holder of the mailbox lock
/// drains, so the callback never waits for the actor.
#[derive(Default)]
struct Staged {
    signals: VecDeque<Input>,
    overflow: bool,
    /// The actor checked the stage and is going to sleep (or sleeps) on the
    /// wakeup: a signal staged now must reach it through the lock.
    sleeping: bool,
}

/// The mailbox shared by producers and the actor.
pub struct Ingress {
    inner: Mutex<Inner>,
    staged: Mutex<Staged>,
    wake: Condvar,
}

impl Ingress {
    /// A mailbox over `pipeline`.
    #[must_use]
    pub const fn new(pipeline: Pipeline) -> Self {
        Self {
            inner: Mutex::new(Inner {
                pipeline,
                shutdown: false,
                closed: false,
                sink_dirty: false,
            }),
            staged: Mutex::new(Staged {
                signals: VecDeque::new(),
                overflow: false,
                sleeping: false,
            }),
            wake: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if self.flush_staged(&mut inner) {
            // The actor may sleep with no one else to wake it.
            self.wake.notify_all();
        }
        inner
    }

    /// Moves staged drag signals into the pipeline, in order, ahead of
    /// anything admitted after them. Called with the mailbox lock held;
    /// `true` when anything was staged.
    fn flush_staged(&self, inner: &mut Inner) -> bool {
        let mut staged = self.staged.lock().unwrap_or_else(PoisonError::into_inner);
        let overflow = std::mem::take(&mut staged.overflow);
        if overflow {
            inner.pipeline.note_drag_overflow();
        }
        let any = overflow || !staged.signals.is_empty();
        for input in staged.signals.drain(..) {
            inner.pipeline.admit_input(input);
        }
        drop(staged);
        any
    }

    /// With the mailbox lock held, right before sleeping: flushes the stage
    /// and reports whether anything was there; otherwise marks the actor as
    /// sleeping, so a later stage hands its signal over through the lock.
    fn flush_or_sleep(&self, inner: &mut Inner) -> bool {
        let mut staged = self.staged.lock().unwrap_or_else(PoisonError::into_inner);
        if staged.overflow || !staged.signals.is_empty() {
            drop(staged);
            return self.flush_staged(inner);
        }
        staged.sleeping = true;
        false
    }

    /// Admits a drag signal from the input callback without ever waiting for
    /// the actor: when the mailbox lock is busy the signal is staged (bounded
    /// like the drag lane; a full stage is one lost observation) and admitted,
    /// in order, by the next holder of the lock. When the actor sleeps (or is
    /// about to), the next holder may be no one, so the callback takes the
    /// lock as soon as the short holder releases it (the actor itself never
    /// holds it while asleep) and admits the stage.
    pub fn submit_drag(&self, input: Input) {
        match self.inner.try_lock() {
            Ok(mut inner) => self.admit_locked(&mut inner, input),
            Err(TryLockError::Poisoned(poisoned)) => {
                self.admit_locked(&mut poisoned.into_inner(), input);
            }
            Err(TryLockError::WouldBlock) => {
                let mut staged = self.staged.lock().unwrap_or_else(PoisonError::into_inner);
                if staged.signals.len() >= DRAG_QUEUE_CAPACITY {
                    staged.signals.clear();
                    staged.overflow = true;
                } else {
                    staged.signals.push_back(input);
                }
                let mut sleeping = staged.sleeping;
                drop(staged);
                while sleeping {
                    match self.inner.try_lock() {
                        Ok(mut inner) => {
                            self.flush_staged(&mut inner);
                            break;
                        }
                        Err(TryLockError::Poisoned(poisoned)) => {
                            self.flush_staged(&mut poisoned.into_inner());
                            break;
                        }
                        Err(TryLockError::WouldBlock) => {
                            std::thread::yield_now();
                            sleeping =
                                self.staged.lock().unwrap_or_else(PoisonError::into_inner).sleeping;
                        }
                    }
                }
            }
        }
        self.wake.notify_all();
    }

    fn admit_locked(&self, inner: &mut Inner, input: Input) {
        self.flush_staged(inner);
        if inner.closed || inner.shutdown {
            inner.pipeline.note_closed();
            return;
        }
        inner.pipeline.admit_input(input);
    }

    /// Admits an input (nonblocking beyond the short lock).
    ///
    /// # Errors
    ///
    /// [`SubmitError::Closed`] once the actor stopped.
    pub fn submit(&self, input: Input) -> Result<(), SubmitError> {
        let mut inner = self.lock();
        if inner.closed || inner.shutdown {
            inner.pipeline.note_closed();
            return Err(SubmitError::Closed);
        }
        inner.pipeline.admit_input(input);
        drop(inner);
        self.wake.notify_all();
        Ok(())
    }

    /// Admits a user intent and reports whether the bounded queue took it.
    ///
    /// Unlike [`Self::submit`], a full semantic queue is an error here: a
    /// silently dropped intent would leave the caller waiting for a record that
    /// never comes.
    ///
    /// # Errors
    ///
    /// [`IntentSubmitError::Closed`] once the actor stopped, or
    /// [`IntentSubmitError::Full`] when the queue refused the intent.
    pub fn submit_intent(&self, intent: crate::intent::Intent) -> Result<(), IntentSubmitError> {
        let mut inner = self.lock();
        if inner.closed || inner.shutdown {
            inner.pipeline.note_closed();
            return Err(IntentSubmitError::Closed);
        }
        let before = inner.pipeline.stats().semantic_rejected;
        inner.pipeline.admit_input(Input::Intent(intent));
        let refused = inner.pipeline.stats().semantic_rejected > before;
        drop(inner);
        if refused {
            return Err(IntentSubmitError::Full);
        }
        self.wake.notify_all();
        Ok(())
    }

    /// The backend sink has events (called from its nonblocking notify).
    pub fn notify_sink(&self) {
        self.lock().sink_dirty = true;
        self.wake.notify_all();
    }

    /// Clears and returns the sink flag; call before draining the sink.
    pub fn take_sink_dirty(&self) -> bool { std::mem::take(&mut self.lock().sink_dirty) }

    /// Asks the actor to stop; wakes it.
    pub fn shutdown(&self) {
        self.lock().shutdown = true;
        self.wake.notify_all();
    }

    /// Refuses further submissions (the actor halted or failed).
    pub fn close(&self) {
        self.lock().closed = true;
        self.wake.notify_all();
    }

    /// Whether a stop was requested.
    #[must_use]
    pub fn is_shutdown(&self) -> bool { self.lock().shutdown }

    /// Runs `f` on the pipeline under the lock. Keep `f` short.
    pub fn with<R>(&self, f: impl FnOnce(&mut Pipeline) -> R) -> R { f(&mut self.lock().pipeline) }

    /// Admission accounting.
    #[must_use]
    pub fn stats(&self) -> AdmissionStats { self.lock().pipeline.stats() }

    /// Sleeps until work, `deadline` or shutdown. Shutdown wins; the check and
    /// the sleep share one lock.
    #[must_use]
    #[allow(clippy::significant_drop_tightening)] // the guard is the wait state
    pub fn wait(&self, deadline: Option<Instant>) -> Wake {
        let mut inner = self.lock();
        loop {
            if inner.shutdown {
                return Wake::Shutdown;
            }
            if inner.sink_dirty || inner.pipeline.has_work() {
                return Wake::Work;
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Wake::Deadline;
            }
            if self.flush_or_sleep(&mut inner) {
                continue;
            }
            inner = match deadline {
                None => self.wake.wait(inner).unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    self.wake
                        .wait_timeout(inner, deadline.saturating_duration_since(Instant::now()))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
            // Awake: a signal staged while another holder had the lock is
            // admitted now, not at some unrelated later wake.
            self.staged.lock().unwrap_or_else(PoisonError::into_inner).sleeping = false;
            self.flush_staged(&mut inner);
        }
    }
}
