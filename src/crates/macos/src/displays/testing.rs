//! A deterministic [`DisplayBackend`] double. It never touches `AppKit`.

#![allow(missing_docs, reason = "test fakes are self-describing")]
#![allow(clippy::must_use_candidate, reason = "test fakes")]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::{DisplayBackend, DisplayError, DisplayLayout};

#[derive(Default)]
struct State {
    layout: Mutex<DisplayLayout>,
    observers: Mutex<BTreeMap<u64, Arc<dyn Fn() + Send + Sync>>>,
    fail_observe: AtomicBool,
    fail_unobserve: AtomicUsize,
    layout_reads: AtomicUsize,
    removals: AtomicUsize,
}

/// The scriptable display backend.
#[derive(Clone, Default)]
pub struct FakeDisplayBackend {
    state: Arc<State>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakeDisplayBackend {
    pub fn new(layout: DisplayLayout) -> Self {
        let fake = Self::default();
        fake.set_layout(layout);
        fake
    }

    pub fn set_layout(&self, layout: DisplayLayout) { *lock(&self.state.layout) = layout; }

    pub fn fail_next_observe(&self) { self.state.fail_observe.store(true, Ordering::SeqCst); }

    pub fn fail_unobserves(&self, count: usize) {
        self.state.fail_unobserve.store(count, Ordering::SeqCst);
    }

    pub fn observers(&self) -> usize { lock(&self.state.observers).len() }

    pub fn removals(&self) -> usize { self.state.removals.load(Ordering::SeqCst) }

    pub fn layout_reads(&self) -> usize { self.state.layout_reads.load(Ordering::SeqCst) }

    /// Fires one native reconfiguration notification at every live observer and
    /// returns how many received it.
    pub fn reconfigure(&self) -> usize {
        let sinks: Vec<_> = lock(&self.state.observers).values().cloned().collect();
        let delivered = sinks.len();
        for sink in sinks {
            sink();
        }
        delivered
    }
}

impl DisplayBackend for FakeDisplayBackend {
    fn layout(&self) -> Result<DisplayLayout, DisplayError> {
        self.state.layout_reads.fetch_add(1, Ordering::SeqCst);
        Ok(lock(&self.state.layout).clone())
    }

    fn observe(&self, id: u64, notify: Arc<dyn Fn() + Send + Sync>) -> Result<(), DisplayError> {
        if self.state.fail_observe.swap(false, Ordering::SeqCst) {
            return Err(DisplayError::ObservationFailed("injected failure".to_owned()));
        }
        lock(&self.state.observers).insert(id, notify);
        Ok(())
    }

    fn unobserve(&self, id: u64) -> Result<(), DisplayError> {
        let fail = self
            .state
            .fail_unobserve
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok();
        if fail {
            return Err(DisplayError::RemovalFailed("injected failure".to_owned()));
        }
        if lock(&self.state.observers).remove(&id).is_some() {
            self.state.removals.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}
