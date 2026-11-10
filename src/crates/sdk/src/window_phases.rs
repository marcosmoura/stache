//! Observable native window lifecycle phases.
//!
//! A [`WindowBackend`](crate::WindowBackend) only queues native work, so a
//! successful `open` means the request was accepted, never that a window exists.
//! The host reports what actually happened through a shared [`WindowPhases`]
//! tracker; the owning module reads the current phase and waits for a change
//! without holding any SDK gate and without involving the main thread.
//!
//! Phases only move forward: `Requested -> Created -> Loaded`, with `Failed`
//! reachable before the window is loaded and `Closing -> Closed` reachable from
//! any live phase. A late callback that would move a window backwards, out of
//! `Failed`, or revive a closed window is ignored, so a stale create or load
//! callback never resurrects state.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use crate::module::ModuleId;
use crate::threading::CancellationToken;
use crate::windows::WindowId;

/// Where one native window is in its life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowPhase {
    /// The host queue accepted the request; no native window exists yet.
    Requested,
    /// The native window exists; its page has not finished loading.
    Created,
    /// The page finished loading.
    Loaded,
    /// Native creation or the page load failed.
    Failed(String),
    /// A close was dispatched and is not acknowledged yet.
    Closing,
    /// The window is gone, or the identity is unknown to the tracker.
    Closed,
}

impl WindowPhase {
    /// Whether no further phase change can happen.
    #[must_use]
    pub const fn is_terminal(&self) -> bool { matches!(self, Self::Failed(_) | Self::Closed) }

    const fn rank(&self) -> u8 {
        match self {
            Self::Requested => 0,
            Self::Created => 1,
            Self::Loaded => 2,
            Self::Failed(_) => 3,
            Self::Closing => 4,
            Self::Closed => 5,
        }
    }
}

/// One observation: the phase and the revision that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhaseSnapshot {
    /// The phase at the time of the read.
    pub phase: WindowPhase,
    /// Changes whenever the phase changes. Pass it back to
    /// [`WindowPhases::wait_change`] to wait for the next change.
    pub revision: u64,
}

/// How a bounded wait ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhaseWait {
    /// The phase changed since the supplied revision, or it is terminal.
    Changed(PhaseSnapshot),
    /// The deadline elapsed with no change.
    TimedOut(PhaseSnapshot),
    /// A supplied cancellation token fired first.
    Cancelled,
}

struct Entry {
    owner: ModuleId,
    phase: WindowPhase,
    revision: u64,
}

#[derive(Default)]
struct State {
    next_revision: u64,
    entries: HashMap<WindowId, Entry>,
}

/// Shared window phase tracker, written by the host and read by owners.
#[derive(Default)]
pub struct WindowPhases {
    state: Mutex<State>,
    changed: Condvar,
}

impl WindowPhases {
    /// Creates an empty tracker.
    #[must_use]
    pub fn new() -> Arc<Self> { Arc::new(Self::default()) }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Starts tracking a freshly accepted request in [`WindowPhase::Requested`].
    pub fn begin(&self, owner: ModuleId, id: WindowId) {
        let mut state = self.lock();
        state.next_revision += 1;
        let revision = state.next_revision;
        state.entries.insert(id, Entry {
            owner,
            phase: WindowPhase::Requested,
            revision,
        });
        drop(state);
        self.changed.notify_all();
    }

    /// Moves a tracked window forward. Returns whether the phase changed.
    ///
    /// Unknown identities, backward moves, moves out of a terminal phase and
    /// `Closed` for a window that was never tracked are ignored, so a late
    /// native callback cannot resurrect anything. Reaching `Closed` drops the
    /// entry; an unknown identity reads as `Closed`.
    pub fn advance(&self, id: WindowId, next: WindowPhase) -> bool {
        let mut state = self.lock();
        let Some(entry) = state.entries.get(&id) else {
            return false;
        };
        let allowed = match (&entry.phase, &next) {
            (WindowPhase::Failed(_) | WindowPhase::Closed, _) => false,
            (_, WindowPhase::Failed(_)) => {
                matches!(entry.phase, WindowPhase::Requested | WindowPhase::Created)
            }
            (current, next) => next.rank() > current.rank(),
        };
        if !allowed {
            return false;
        }
        if next == WindowPhase::Closed {
            state.entries.remove(&id);
        } else {
            state.next_revision += 1;
            let revision = state.next_revision;
            if let Some(entry) = state.entries.get_mut(&id) {
                entry.phase = next;
                entry.revision = revision;
            }
        }
        drop(state);
        self.changed.notify_all();
        true
    }

    /// Drops the entry for one window regardless of phase, waking waiters.
    ///
    /// The host calls this when the owner releases a window that already
    /// reached a terminal `Failed` phase, which no native event will clear.
    pub fn discard(&self, id: WindowId) {
        let mut state = self.lock();
        state.entries.remove(&id);
        drop(state);
        self.changed.notify_all();
    }

    /// Drops every `Failed` entry of one owner, waking waiters.
    pub fn discard_failed(&self, owner: ModuleId) {
        let mut state = self.lock();
        state.entries.retain(|_, entry| {
            entry.owner != owner || !matches!(entry.phase, WindowPhase::Failed(_))
        });
        drop(state);
        self.changed.notify_all();
    }

    /// Whether the identity is currently tracked and live (not terminal).
    #[must_use]
    pub fn is_live(&self, id: WindowId) -> bool {
        self.lock().entries.get(&id).is_some_and(|entry| !entry.phase.is_terminal())
    }

    /// Reads the current phase.
    #[must_use]
    pub fn snapshot(&self, id: WindowId) -> PhaseSnapshot { Self::read(&self.lock(), id) }

    fn read(state: &State, id: WindowId) -> PhaseSnapshot {
        state.entries.get(&id).map_or(
            PhaseSnapshot {
                phase: WindowPhase::Closed,
                revision: u64::MAX,
            },
            |entry| PhaseSnapshot {
                phase: entry.phase.clone(),
                revision: entry.revision,
            },
        )
    }

    /// Waits until the phase differs from `after`, the deadline passes or a
    /// token is cancelled.
    ///
    /// The read and the wait share one lock, and every token's listener takes
    /// that lock before notifying, so a change or cancellation between the read
    /// and the wait cannot be lost. A terminal phase returns immediately because
    /// nothing can change it again. This never touches the main thread.
    #[must_use]
    pub fn wait_change(
        self: &Arc<Self>,
        id: WindowId,
        after: u64,
        deadline: Instant,
        cancel: &[&CancellationToken],
    ) -> PhaseWait {
        let wake: Arc<dyn Fn() + Send + Sync> = {
            let tracker = Arc::clone(self);
            Arc::new(move || {
                // Taking the lock orders the notification after a waiter that
                // already checked the token and is about to sleep.
                drop(tracker.lock());
                tracker.changed.notify_all();
            })
        };
        // Listeners are registered before the lock is taken: an already
        // cancelled token runs its listener inline and must not need the lock
        // this thread holds.
        let _guards: Vec<_> =
            cancel.iter().map(|token| token.on_cancel(Arc::clone(&wake))).collect();
        let mut state = self.lock();
        loop {
            if cancel.iter().any(|token| token.is_cancelled()) {
                return PhaseWait::Cancelled;
            }
            let snapshot = Self::read(&state, id);
            if snapshot.revision != after || snapshot.phase.is_terminal() {
                return PhaseWait::Changed(snapshot);
            }
            let now = Instant::now();
            if now >= deadline {
                return PhaseWait::TimedOut(snapshot);
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}
