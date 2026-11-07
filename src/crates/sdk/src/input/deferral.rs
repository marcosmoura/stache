//! The wake-up for deferred suppressing-tap removal.
//!
//! When the last suppressing handler leaves while the press ledger still holds a
//! suppressed tombstone, the tap must outlive the handler so the rest of that
//! press stays swallowed. The tap callback only flips [`RemovalSignal`] to "due";
//! a short-lived helper thread owned by the service performs the acknowledged
//! removal through the normal control path.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

#[derive(Default)]
struct SignalState {
    /// The deferral the helper thread serves; `0` means none.
    current: u64,
    due: bool,
}

/// How a deferral wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    /// The last tombstone is gone; removal is due.
    Due,
    /// The deferral was cancelled or replaced.
    Cancelled,
}

#[derive(Default)]
pub(crate) struct RemovalSignal {
    /// Read by the tap callback; set only while a deferral waits for tombstones.
    armed: AtomicBool,
    /// A deferral exists that has not completed or been cancelled.
    pending: AtomicBool,
    state: Mutex<SignalState>,
    cv: Condvar,
}

impl RemovalSignal {
    fn lock(&self) -> MutexGuard<'_, SignalState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts deferral `id`. Called with the ledger lock held.
    pub(crate) fn arm(&self, id: u64) {
        {
            let mut state = self.lock();
            state.current = id;
            state.due = false;
        }
        self.pending.store(true, Ordering::SeqCst);
        self.armed.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_armed(&self) -> bool { self.armed.load(Ordering::SeqCst) }

    /// Whether a deferral is outstanding (waiting or about to remove).
    pub(crate) fn is_pending(&self) -> bool { self.pending.load(Ordering::SeqCst) }

    /// Flags removal as due. Safe on the tap callback: a flag flip and a notify.
    pub(crate) fn mark_due(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.lock().due = true;
            self.cv.notify_all();
        }
    }

    /// Abandons the current deferral; its helper wakes as cancelled.
    pub(crate) fn cancel(&self) {
        self.armed.store(false, Ordering::SeqCst);
        self.pending.store(false, Ordering::SeqCst);
        {
            let mut state = self.lock();
            state.current = 0;
            state.due = false;
        }
        self.cv.notify_all();
    }

    /// The removal ran (or was handed to the owner); nothing is pending.
    pub(crate) fn finish(&self) {
        self.pending.store(false, Ordering::SeqCst);
        self.armed.store(false, Ordering::SeqCst);
        let mut state = self.lock();
        state.current = 0;
        state.due = false;
    }

    /// Blocks the helper until deferral `id` is due or cancelled. No timeout.
    pub(crate) fn wait(&self, id: u64) -> Wake {
        let guard = self.lock();
        let guard = self
            .cv
            .wait_while(guard, |state| state.current == id && !state.due)
            .unwrap_or_else(PoisonError::into_inner);
        if guard.current == id && guard.due {
            Wake::Due
        } else {
            Wake::Cancelled
        }
    }
}
