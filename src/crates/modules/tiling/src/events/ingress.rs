//! The shared mailbox: a short lock around the [`Pipeline`] plus a wakeup.
//!
//! Cancellation and arrival both set state under the same lock the waiter
//! checks before sleeping, so a wakeup between the check and the sleep cannot
//! be lost.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use super::pipeline::{AdmissionStats, Pipeline};
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

/// The mailbox shared by producers and the actor.
pub struct Ingress {
    inner: Mutex<Inner>,
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
            wake: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
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
    pub fn wait(&self, deadline: Option<Instant>) -> Wake {
        let mut inner = self.lock();
        loop {
            if inner.shutdown {
                return Wake::Shutdown;
            }
            if inner.sink_dirty || inner.pipeline.has_work() {
                return Wake::Work;
            }
            inner = match deadline {
                None => self.wake.wait(inner).unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Wake::Deadline;
                    }
                    self.wake
                        .wait_timeout(inner, deadline - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
        }
    }
}
