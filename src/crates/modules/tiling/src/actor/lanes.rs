//! Read effects on per-application lanes.
//!
//! A read effect (`Track`, `Refresh`, `Snapshot`, `ReadGeometry`, `ReadFocus`,
//! `Untrack`) makes a blocking backend call with a deadline. One FIFO per
//! application keeps an application's reads in submission order and one at a
//! time; [`READ_WORKERS`] threads run the heads of different lanes at once, so
//! an application that does not answer delays only its own reads.
//!
//! One rule couples the lanes to the effects thread: a `ReadFocus` of an
//! application does not start before every `Focus` of that application that was
//! submitted before it has finished there (run or discarded). `Focus` activates
//! and then raises; the activation queues the read, which must see the raise,
//! as it did when both ran on one thread. No other read waits on the effects
//! thread, and the effects thread never waits on a lane.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use stache_macos::ApplicationId;

use super::{EffectEnv, RunnerError};
use crate::state::Effect;

/// The read worker threads.
pub const READ_WORKERS: usize = 4;
/// The read worker thread name prefix (`stache-tiling-reads-<n>`).
pub const READ_THREAD_PREFIX: &str = "stache-tiling-reads";

/// The application a read effect belongs to; `None` for every other effect.
pub(super) const fn read_application(effect: &Effect) -> Option<ApplicationId> {
    match effect {
        Effect::Track { application, .. }
        | Effect::Refresh { application, .. }
        | Effect::ReadFocus { application, .. }
        | Effect::Untrack { application, .. } => Some(*application),
        Effect::Snapshot { window, .. } | Effect::ReadGeometry { window, .. } => {
            Some(window.application())
        }
        _ => None,
    }
}

struct Queued {
    effect: Effect,
    /// A `ReadFocus` waits until this many `Focus` effects of its application
    /// finished on the effects thread.
    after_focus: Option<u64>,
}

#[derive(Default)]
struct Lane {
    queue: VecDeque<Queued>,
    /// A worker is running this lane's head.
    busy: bool,
}

/// `Focus` effects of one application: submitted to the effects thread and
/// finished there (run, discarded or refused).
#[derive(Clone, Copy, Default)]
struct FocusCount {
    submitted: u64,
    finished: u64,
}

struct PoolState {
    open: bool,
    capacity: usize,
    /// Reads queued and not started, over every lane.
    queued: usize,
    lanes: BTreeMap<ApplicationId, Lane>,
    /// Lanes with a queued head and no worker, oldest first.
    ready: VecDeque<ApplicationId>,
    focus: BTreeMap<ApplicationId, FocusCount>,
    cancelled: bool,
    #[cfg(test)]
    started: usize,
}

impl PoolState {
    fn runnable(&self, application: ApplicationId) -> bool {
        let Some(head) = self.lanes.get(&application).and_then(|lane| lane.queue.front()) else {
            return false;
        };
        self.cancelled
            || head.after_focus.is_none_or(|after| {
                self.focus.get(&application).is_none_or(|count| count.finished >= after)
            })
    }

    /// Takes the head of the oldest runnable lane.
    fn take(&mut self) -> Option<(ApplicationId, Effect)> {
        let index = (0..self.ready.len()).find(|index| self.runnable(self.ready[*index]))?;
        let application = self.ready.remove(index)?;
        let lane = self.lanes.get_mut(&application)?;
        let queued = lane.queue.pop_front()?;
        lane.busy = true;
        self.queued -= 1;
        #[cfg(test)]
        {
            self.started += 1;
        }
        Some((application, queued.effect))
    }

    /// The lane's head finished: the next one becomes ready, or the lane (and
    /// a settled focus count) is dropped.
    fn finish(&mut self, application: ApplicationId) {
        let Some(lane) = self.lanes.get_mut(&application) else {
            return;
        };
        lane.busy = false;
        if lane.queue.is_empty() {
            self.lanes.remove(&application);
            self.forget_settled(application);
        } else {
            self.ready.push_back(application);
        }
    }

    fn forget_settled(&mut self, application: ApplicationId) {
        let settled = self.focus.get(&application).is_some_and(|c| c.submitted == c.finished);
        if settled && !self.lanes.contains_key(&application) {
            self.focus.remove(&application);
        }
    }
}

/// The lanes and the condition variable their workers block on.
pub(super) struct ReadPool {
    state: Mutex<PoolState>,
    work: Condvar,
}

impl ReadPool {
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(PoolState {
                open: true,
                capacity: capacity.max(1),
                queued: 0,
                lanes: BTreeMap::new(),
                ready: VecDeque::new(),
                focus: BTreeMap::new(),
                cancelled: false,
                #[cfg(test)]
                started: 0,
            }),
            work: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues a read on its application's lane. A `Snapshot` or `ReadGeometry`
    /// for a window with one of the same kind already queued replaces that one
    /// in place, so one read answers for both.
    pub(super) fn submit(
        &self,
        application: ApplicationId,
        effect: Effect,
    ) -> Result<(), RunnerError> {
        let mut state = self.lock();
        if !state.open {
            return Err(RunnerError::Closed);
        }
        // The queued read has not started, so it takes the newest request in
        // place (same position, same focus wait): a geometry read is then
        // judged against the newest attempt's range.
        if let Some(lane) = state.lanes.get_mut(&application)
            && let Some(queued) =
                lane.queue.iter_mut().find(|queued| same_read(&queued.effect, &effect))
        {
            queued.effect = effect;
            return Ok(());
        }
        if state.queued >= state.capacity {
            return Err(RunnerError::Full);
        }
        let after_focus = match effect {
            Effect::ReadFocus { .. } => state
                .focus
                .get(&application)
                .filter(|count| count.submitted > count.finished)
                .map(|count| count.submitted),
            _ => None,
        };
        let lane = state.lanes.entry(application).or_default();
        let ready = !lane.busy && lane.queue.is_empty();
        lane.queue.push_back(Queued { effect, after_focus });
        state.queued += 1;
        if ready {
            state.ready.push_back(application);
        }
        drop(state);
        self.work.notify_one();
        Ok(())
    }

    /// A `Focus` of `application` is about to be queued on the effects thread.
    pub(super) fn focus_submitted(&self, application: ApplicationId) {
        self.lock().focus.entry(application).or_default().submitted += 1;
    }

    /// A `Focus` of `application` finished on the effects thread (run or
    /// discarded) or was refused by its queue.
    pub(super) fn focus_finished(&self, application: ApplicationId) {
        let mut state = self.lock();
        if let Some(count) = state.focus.get_mut(&application) {
            count.finished += 1;
        }
        state.forget_settled(application);
        drop(state);
        self.work.notify_all();
    }

    /// Closes admission; workers drain what is queued, then exit.
    pub(super) fn close(&self) {
        self.lock().open = false;
        self.work.notify_all();
    }

    /// Cancellation: no queued read waits for a focus any more (it is
    /// discarded), so the workers are woken to drain.
    pub(super) fn cancelled(&self) {
        self.lock().cancelled = true;
        self.work.notify_all();
    }

    /// One worker: runs runnable lane heads until the pool is closed and empty.
    /// Blocks on the condition variable while nothing is runnable.
    pub(super) fn work(&self, env: &EffectEnv) {
        let mut state = self.lock();
        loop {
            if let Some((application, effect)) = state.take() {
                drop(state);
                if env.cancel.is_cancelled() {
                    env.discard(&effect);
                } else {
                    env.run(&effect);
                }
                state = self.lock();
                state.finish(application);
                // The next head of this lane, or a slot for another one.
                self.work.notify_all();
                continue;
            }
            if !state.open && state.queued == 0 {
                return;
            }
            state = self.work.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Reads queued and not started (tests).
    #[cfg(test)]
    pub(super) fn queued(&self) -> usize { self.lock().queued }

    /// Reads started so far (tests).
    #[cfg(test)]
    pub(super) fn started(&self) -> usize { self.lock().started }

    /// Applications with a lane or a focus count (tests: the maps stay bounded).
    #[cfg(test)]
    pub(super) fn tracked_applications(&self) -> (usize, usize) {
        let state = self.lock();
        (state.lanes.len(), state.focus.len())
    }
}

/// Whether `queued` already answers `new`: the same window's snapshot, or the
/// same window's geometry read, for the same registry. A read queued for an
/// earlier registry completes with that stamp and is dropped as stale, so it
/// cannot answer for the current one.
fn same_read(queued: &Effect, new: &Effect) -> bool {
    match (queued, new) {
        (
            Effect::Snapshot { stamp: old, window: a },
            Effect::Snapshot { stamp: current, window: b },
        )
        | (
            Effect::ReadGeometry { stamp: old, window: a, .. },
            Effect::ReadGeometry { stamp: current, window: b, .. },
        ) => a == b && old == current,
        _ => false,
    }
}

/// The pool shared by the runner, its workers and the effects thread.
pub(super) type SharedPool = Arc<ReadPool>;
