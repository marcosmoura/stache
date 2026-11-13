//! A one-shot, result-returning host runner job with a bounded, cancellable
//! wait.
//!
//! The caller queues the job with a host command and waits; the runner claims
//! it with [`HostJob::begin`] before doing any work and answers with
//! [`HostJob::finish`]. A waiter that gives up (deadline or shutdown) while the
//! job is still queued cancels it, and the runner then skips it: a cancelled
//! job never runs, so it writes nothing.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How often a waiting caller rechecks that the host is still running.
const QUANTUM: Duration = Duration::from_millis(20);

enum State<T> {
    Queued,
    Running,
    Finished(T),
    Cancelled,
}

/// One queued job and its answer slot.
pub struct HostJob<T> {
    state: Mutex<State<T>>,
    changed: Condvar,
}

/// How a bounded wait ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobWait<T> {
    /// The runner answered.
    Done(T),
    /// The job was still queued and is now cancelled; it will never run.
    /// `stopped` is `true` when the host stopped, `false` on the deadline.
    Cancelled {
        /// Whether the host stopped (shutdown or runner gone).
        stopped: bool,
    },
    /// The runner started the job but did not answer in time.
    Uncertain,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<T: Clone> HostJob<T> {
    /// A queued job.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::Queued),
            changed: Condvar::new(),
        })
    }

    /// The runner claims the job. `false` when the waiter already cancelled it.
    pub fn begin(&self) -> bool {
        let mut state = lock(&self.state);
        if matches!(*state, State::Queued) {
            *state = State::Running;
            true
        } else {
            false
        }
    }

    /// The runner publishes the answer and wakes the waiter.
    pub fn finish(&self, value: T) {
        *lock(&self.state) = State::Finished(value);
        self.changed.notify_all();
    }

    /// Waits at most `bound`; `abandoned` reports that the host stopped and is
    /// polled at a coarse quantum only while waiting.
    pub fn wait(&self, bound: Duration, abandoned: impl Fn() -> bool) -> JobWait<T> {
        let deadline = Instant::now().checked_add(bound);
        let mut state = lock(&self.state);
        loop {
            if let State::Finished(value) = &*state {
                return JobWait::Done(value.clone());
            }
            if matches!(*state, State::Cancelled) {
                return JobWait::Cancelled { stopped: true };
            }
            let stopped = abandoned();
            let expired = deadline.is_some_and(|deadline| Instant::now() >= deadline);
            if stopped || expired {
                return if matches!(*state, State::Queued) {
                    *state = State::Cancelled;
                    JobWait::Cancelled { stopped }
                } else {
                    JobWait::Uncertain
                };
            }
            state = self
                .changed
                .wait_timeout(state, QUANTUM)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_cancels_a_queued_job_and_the_runner_skips_it() {
        let job = HostJob::<u8>::new();
        assert_eq!(
            job.wait(Duration::from_millis(30), || false),
            JobWait::Cancelled { stopped: false }
        );
        assert!(!job.begin());
    }

    #[test]
    fn a_stop_wakes_the_waiter_and_cancels_a_queued_job() {
        let job = HostJob::<u8>::new();
        assert_eq!(job.wait(Duration::from_secs(60), || true), JobWait::Cancelled {
            stopped: true
        });
        assert!(!job.begin());
    }

    #[test]
    fn a_started_job_is_uncertain_on_the_deadline_and_answers_otherwise() {
        let job = HostJob::<u8>::new();
        assert!(job.begin());
        assert_eq!(job.wait(Duration::from_millis(30), || false), JobWait::Uncertain);
        job.finish(7);
        assert_eq!(job.wait(Duration::from_millis(30), || false), JobWait::Done(7));
    }
}
