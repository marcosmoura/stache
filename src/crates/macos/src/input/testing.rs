//! A fake native layer for the owner tests. It never creates a tap, registers a
//! hotkey, injects input or touches a permission.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the fake holds one short-lived lock per scripted step"
)]

use std::collections::HashSet;
use std::marker::PhantomData;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, ThreadId};
use std::time::Duration;

use stache_sdk::input::{InputService, NoWork, TapKind};

use super::MacInputBackend;
use super::owner::{NativeInput, WakeHandle};
use super::system::ServiceSlot;

/// One native operation the fake performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Install(TapKind),
    Enable(TapKind),
    Remove(TapKind),
}

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent scripted switches"
)]
struct State {
    calls: Vec<Call>,
    live: HashSet<TapKind>,
    partial: HashSet<TapKind>,
    threads: HashSet<ThreadId>,
    thread_names: HashSet<String>,
    dropped: bool,
    created: bool,
    install_fails_partially: bool,
    install_fails_clean: bool,
    remove_failures: usize,
    factory_open: bool,
}

/// Shared observations and scripted failures.
pub struct FakeLog {
    state: Mutex<State>,
    changed: Condvar,
}

impl FakeLog {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                factory_open: true,
                ..State::default()
            }),
            changed: Condvar::new(),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn calls(&self) -> Vec<Call> { self.state().calls.clone() }

    pub fn is_live(&self, kind: TapKind) -> bool { self.state().live.contains(&kind) }

    pub fn has_partial(&self, kind: TapKind) -> bool { self.state().partial.contains(&kind) }

    pub fn dropped(&self) -> bool { self.state().dropped }

    pub fn created(&self) -> bool { self.state().created }

    pub fn thread_ids(&self) -> HashSet<ThreadId> { self.state().threads.clone() }

    pub fn thread_names(&self) -> HashSet<String> { self.state().thread_names.clone() }

    pub fn fail_install_partially(&self) { self.state().install_fails_partially = true; }

    pub fn fail_install_cleanly(&self) { self.state().install_fails_clean = true; }

    pub fn fail_next_removals(&self, count: usize) { self.state().remove_failures = count; }

    /// Holds the owner inside its native factory until [`Self::open_factory`].
    pub fn close_factory(&self) { self.state().factory_open = false; }

    pub fn open_factory(&self) {
        self.state().factory_open = true;
        self.changed.notify_all();
    }

    pub fn wait_until(&self, what: impl Fn(&[Call]) -> bool) -> bool {
        let state = self.state();
        let (state, timeout) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |state| !what(&state.calls))
            .unwrap_or_else(PoisonError::into_inner);
        drop(state);
        !timeout.timed_out()
    }

    fn record(&self, call: Call) {
        let mut state = self.state();
        state.calls.push(call);
        state.threads.insert(thread::current().id());
        if let Some(name) = thread::current().name() {
            state.thread_names.insert(name.to_owned());
        }
        drop(state);
        self.changed.notify_all();
    }
}

#[derive(Default)]
struct FakeWake {
    flag: Mutex<bool>,
    wake: Condvar,
}

impl WakeHandle for FakeWake {
    fn wake(&self) {
        *self.flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.wake.notify_all();
    }
}

/// The fake layer; `!Send` like the real one.
pub struct FakeNative {
    log: Arc<FakeLog>,
    wake: Arc<FakeWake>,
    _confined: PhantomData<*const ()>,
}

impl FakeNative {
    fn new(log: Arc<FakeLog>) -> Self {
        // Blocks the owner thread inside the factory when the test closed it.
        {
            let state = log.state();
            drop(
                log.changed
                    .wait_while(state, |state| !state.factory_open)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
        log.state().created = true;
        Self {
            log,
            wake: Arc::new(FakeWake::default()),
            _confined: PhantomData,
        }
    }
}

impl Drop for FakeNative {
    fn drop(&mut self) {
        self.log.state().dropped = true;
        self.log.changed.notify_all();
    }
}

impl NativeInput for FakeNative {
    fn wake_handle(&self) -> Arc<dyn WakeHandle> { Arc::clone(&self.wake) as Arc<dyn WakeHandle> }

    fn install(&mut self, kind: TapKind) -> Result<(), String> {
        self.log.record(Call::Install(kind));
        let mut state = self.log.state();
        if state.install_fails_clean {
            return Err("injected create failure".to_owned());
        }
        state.partial.insert(kind);
        if state.install_fails_partially {
            return Err("injected partial install failure".to_owned());
        }
        state.partial.remove(&kind);
        state.live.insert(kind);
        Ok(())
    }

    fn enable(&mut self, kind: TapKind) -> Result<(), String> {
        self.log.record(Call::Enable(kind));
        if self.log.state().live.contains(&kind) {
            Ok(())
        } else {
            Err("the event tap is not installed".to_owned())
        }
    }

    fn remove(&mut self, kind: TapKind) -> Result<(), String> {
        self.log.record(Call::Remove(kind));
        let mut state = self.log.state();
        if state.remove_failures > 0 {
            state.remove_failures -= 1;
            return Err("injected removal failure".to_owned());
        }
        state.live.remove(&kind);
        state.partial.remove(&kind);
        Ok(())
    }

    fn park(&mut self, limit: Duration) {
        let flag = self.wake.flag.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut flag, _) = self
            .wake
            .wake
            .wait_timeout_while(flag, limit, |woken| !*woken)
            .unwrap_or_else(PoisonError::into_inner);
        *flag = false;
    }
}

/// A service over a backend whose native layer is the fake.
pub fn fake_service(log: &Arc<FakeLog>) -> (Arc<InputService>, Arc<MacInputBackend>) {
    let slot: Arc<ServiceSlot> = Arc::new(OnceLock::new());
    let factory_log = Arc::clone(log);
    let backend = Arc::new(MacInputBackend::from_factory(
        Box::new(move || Ok(Box::new(FakeNative::new(factory_log)) as Box<dyn NativeInput>)),
        slot,
    ));
    let service = InputService::new(
        Arc::clone(&backend) as Arc<dyn stache_sdk::input::InputBackend>,
        Arc::new(NoWork),
    );
    (service, backend)
}
