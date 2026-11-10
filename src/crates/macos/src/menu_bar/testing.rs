//! A deterministic [`MenuBarBackend`] double. It never touches `AppKit`.

#![allow(missing_docs, reason = "test fakes are self-describing")]
#![allow(clippy::must_use_candidate, reason = "test fakes")]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::{MenuBarBackend, MenuBarError, MenuBarSample};

struct State {
    sample: Mutex<MenuBarSample>,
    fail_sample: AtomicBool,
    observers: Mutex<BTreeMap<u64, Arc<dyn Fn() + Send + Sync>>>,
    fail_observe: AtomicUsize,
    fail_unobserve: AtomicUsize,
    samples: AtomicUsize,
    removals: AtomicUsize,
    installs: AtomicUsize,
    log: Mutex<Vec<&'static str>>,
}

/// The scriptable menu-bar backend.
#[derive(Clone)]
pub struct FakeMenuBarBackend {
    state: Arc<State>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A sample of a permanently visible menu bar (auto-hide off).
pub const fn visible_sample() -> MenuBarSample {
    MenuBarSample {
        menu_bar_visible: Some(true),
        pointer_in_row: Some(false),
        window_list: None,
        auto_hide: Some(false),
    }
}

/// A sample of an auto-hidden menu bar (auto-hide on, pointer elsewhere).
pub const fn hidden_sample() -> MenuBarSample {
    MenuBarSample {
        menu_bar_visible: Some(false),
        pointer_in_row: Some(false),
        window_list: None,
        auto_hide: Some(true),
    }
}

impl FakeMenuBarBackend {
    pub fn new(sample: MenuBarSample) -> Self {
        Self {
            state: Arc::new(State {
                sample: Mutex::new(sample),
                fail_sample: AtomicBool::new(false),
                observers: Mutex::default(),
                fail_observe: AtomicUsize::new(0),
                fail_unobserve: AtomicUsize::new(0),
                samples: AtomicUsize::new(0),
                removals: AtomicUsize::new(0),
                installs: AtomicUsize::new(0),
                log: Mutex::default(),
            }),
        }
    }

    pub fn set_sample(&self, sample: MenuBarSample) { *lock(&self.state.sample) = sample; }

    pub fn fail_samples(&self, fail: bool) { self.state.fail_sample.store(fail, Ordering::SeqCst); }

    pub fn fail_next_observe(&self) { self.fail_observes(1); }

    pub fn fail_observes(&self, count: usize) {
        self.state.fail_observe.store(count, Ordering::SeqCst);
    }

    pub fn fail_unobserves(&self, count: usize) {
        self.state.fail_unobserve.store(count, Ordering::SeqCst);
    }

    pub fn observers(&self) -> usize { lock(&self.state.observers).len() }

    pub fn installs(&self) -> usize { self.state.installs.load(Ordering::SeqCst) }

    pub fn removals(&self) -> usize { self.state.removals.load(Ordering::SeqCst) }

    pub fn samples(&self) -> usize { self.state.samples.load(Ordering::SeqCst) }

    /// The native calls in order: `"observe"`, `"sample"`, `"unobserve"`.
    pub fn log(&self) -> Vec<&'static str> { lock(&self.state.log).clone() }

    /// Fires one native notification at every live observer and returns how
    /// many received it.
    pub fn signal(&self) -> usize {
        let sinks: Vec<_> = lock(&self.state.observers).values().cloned().collect();
        let delivered = sinks.len();
        for sink in sinks {
            sink();
        }
        delivered
    }
}

impl MenuBarBackend for FakeMenuBarBackend {
    fn sample(&self) -> Result<MenuBarSample, MenuBarError> {
        self.state.samples.fetch_add(1, Ordering::SeqCst);
        lock(&self.state.log).push("sample");
        if self.state.fail_sample.load(Ordering::SeqCst) {
            return Err(MenuBarError::ObservationFailed(
                "injected sample failure".to_owned(),
            ));
        }
        Ok(lock(&self.state.sample).clone())
    }

    fn observe(&self, id: u64, notify: Arc<dyn Fn() + Send + Sync>) -> Result<(), MenuBarError> {
        lock(&self.state.log).push("observe");
        let fail = self
            .state
            .fail_observe
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok();
        if fail {
            return Err(MenuBarError::ObservationFailed("injected failure".to_owned()));
        }
        self.state.installs.fetch_add(1, Ordering::SeqCst);
        lock(&self.state.observers).insert(id, notify);
        Ok(())
    }

    fn unobserve(&self, id: u64) -> Result<(), MenuBarError> {
        lock(&self.state.log).push("unobserve");
        let fail = self
            .state
            .fail_unobserve
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok();
        if fail {
            return Err(MenuBarError::RemovalFailed("injected failure".to_owned()));
        }
        if lock(&self.state.observers).remove(&id).is_some() {
            self.state.removals.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}
