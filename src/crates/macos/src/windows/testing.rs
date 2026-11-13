//! Fakes for the window adapter: a fake system (fake AX arena plus scripted
//! observers), a stepping clock and a recording driver. No real AX, run loop
//! or desktop effect is involved.

#![allow(missing_docs, reason = "test fakes are self-describing")]
#![allow(clippy::must_use_candidate, reason = "test fakes")]
#![allow(clippy::missing_panics_doc, reason = "test fakes")]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the fake holds one short-lived lock per scripted step"
)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::driver::{Driver, Waker};
use super::lease::VoiceOverProbe;
use super::system::{WindowClock, WindowObserver, WindowSystem};
use crate::ax::testing::{Call, FakeWindowAx, FakeWindowElement, NodeId};
use crate::ax::{AxDrain, AxError, AxEvent, AxNotification, ElementKey};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct PidScript {
    pending: Vec<(AxNotification, NodeId)>,
    overflow: bool,
    add_error: Option<AxError>,
    remove_error: Option<AxError>,
    registrations: Vec<(NodeId, AxNotification)>,
    /// Callback-delivered, unregistered elements still held (like the real
    /// mailbox's interned entries) until the owner releases them.
    interned: HashSet<NodeId>,
    attached: bool,
    observers_created: usize,
    remove_attempts: usize,
    exited: bool,
}

#[derive(Default)]
struct ObserverState {
    pids: HashMap<i32, PidScript>,
    create_error: Option<AxError>,
}

/// The scripted observer side. Cloneable handle for tests.
#[derive(Clone, Default)]
pub struct FakeObservers {
    state: Arc<Mutex<ObserverState>>,
}

impl FakeObservers {
    /// Queues a callback delivery for `pid`'s observer.
    pub fn inject(&self, pid: i32, notification: AxNotification, node: NodeId) {
        let mut state = lock(&self.state);
        let script = state.pids.entry(pid).or_default();
        if !script.registrations.iter().any(|(registered, _)| *registered == node) {
            script.interned.insert(node);
        }
        script.pending.push((notification, node));
    }

    /// Whether the fake still holds a callback-delivered element for `node`.
    pub fn is_interned(&self, pid: i32, node: NodeId) -> bool {
        lock(&self.state)
            .pids
            .get(&pid)
            .is_some_and(|script| script.interned.contains(&node))
    }

    pub fn set_overflow(&self, pid: i32) {
        lock(&self.state).pids.entry(pid).or_default().overflow = true;
    }

    pub fn set_add_error(&self, pid: i32, error: Option<AxError>) {
        lock(&self.state).pids.entry(pid).or_default().add_error = error;
    }

    pub fn set_remove_error(&self, pid: i32, error: Option<AxError>) {
        lock(&self.state).pids.entry(pid).or_default().remove_error = error;
    }

    /// Marks `pid`'s process as exited (or running again).
    pub fn set_exited(&self, pid: i32, exited: bool) {
        lock(&self.state).pids.entry(pid).or_default().exited = exited;
    }

    pub fn set_create_error(&self, error: Option<AxError>) {
        lock(&self.state).create_error = error;
    }

    pub fn registrations(&self, pid: i32) -> Vec<(NodeId, AxNotification)> {
        lock(&self.state)
            .pids
            .get(&pid)
            .map_or_default(|script| script.registrations.clone())
    }

    pub fn is_attached(&self, pid: i32) -> bool {
        lock(&self.state).pids.get(&pid).is_some_and(|script| script.attached)
    }

    pub fn remove_attempts(&self, pid: i32) -> usize {
        lock(&self.state).pids.get(&pid).map_or(0, |script| script.remove_attempts)
    }

    pub fn observers_created(&self, pid: i32) -> usize {
        lock(&self.state).pids.get(&pid).map_or(0, |script| script.observers_created)
    }
}

/// A fake per-application observer.
pub struct FakeObserver {
    pid: i32,
    ax: FakeWindowAx,
    script: FakeObservers,
}

const fn key_of(node: NodeId) -> ElementKey { ElementKey::from_raw(node as u64 + 1) }

fn node_of(key: ElementKey) -> NodeId {
    usize::try_from(key.raw_for_fakes()).map_or(0, |raw| raw.saturating_sub(1))
}

impl WindowObserver<FakeWindowElement> for FakeObserver {
    fn attach(&mut self) {
        lock(&self.script.state).pids.entry(self.pid).or_default().attached = true;
    }

    fn detach(&mut self) {
        lock(&self.script.state).pids.entry(self.pid).or_default().attached = false;
    }

    fn add_notification(
        &mut self,
        element: &FakeWindowElement,
        notification: AxNotification,
    ) -> Result<ElementKey, AxError> {
        let node = element.node();
        let mut state = lock(&self.script.state);
        let script = state.pids.entry(self.pid).or_default();
        if let Some(error) = script.add_error {
            return Err(error);
        }
        if !script.registrations.contains(&(node, notification)) {
            script.registrations.push((node, notification));
        }
        drop(state);
        self.ax.record(Call::Observe(node, notification));
        Ok(key_of(node))
    }

    fn remove_notification(
        &mut self,
        key: ElementKey,
        notification: AxNotification,
    ) -> Result<(), AxError> {
        let node = node_of(key);
        let mut state = lock(&self.script.state);
        let script = state.pids.entry(self.pid).or_default();
        script.remove_attempts += 1;
        if let Some(error) = script.remove_error {
            return Err(error);
        }
        script.registrations.retain(|entry| *entry != (node, notification));
        drop(state);
        self.ax.record(Call::Unobserve(node, notification));
        Ok(())
    }

    fn drain(&mut self) -> AxDrain {
        let mut state = lock(&self.script.state);
        let script = state.pids.entry(self.pid).or_default();
        let pending = std::mem::take(&mut script.pending);
        let overflowed = std::mem::take(&mut script.overflow);
        drop(state);
        AxDrain {
            events: pending
                .into_iter()
                .map(|(notification, node)| AxEvent {
                    notification,
                    element: key_of(node),
                })
                .collect(),
            overflowed,
        }
    }

    fn element(&self, key: ElementKey) -> Option<FakeWindowElement> {
        let node = node_of(key);
        let state = lock(&self.script.state);
        let script = state.pids.get(&self.pid)?;
        let held = script.interned.contains(&node)
            || script.registrations.iter().any(|(registered, _)| *registered == node);
        drop(state);
        held.then(|| self.ax.element(node))
    }

    fn has_pending_event(&self, key: ElementKey) -> bool {
        let node = node_of(key);
        lock(&self.script.state)
            .pids
            .get(&self.pid)
            .is_some_and(|script| script.pending.iter().any(|(_, pending)| *pending == node))
    }

    fn release(&mut self, key: ElementKey) {
        let node = node_of(key);
        let mut state = lock(&self.script.state);
        let script = state.pids.entry(self.pid).or_default();
        if !script.registrations.iter().any(|(registered, _)| *registered == node) {
            script.interned.remove(&node);
        }
    }

    fn forget_destroyed(&mut self, key: ElementKey) {
        let node = node_of(key);
        let mut state = lock(&self.script.state);
        let script = state.pids.entry(self.pid).or_default();
        script.registrations.retain(|(registered, _)| *registered != node);
        script.interned.remove(&node);
        drop(state);
    }

    fn target_exited(&self) -> bool {
        lock(&self.script.state).pids.get(&self.pid).is_some_and(|script| script.exited)
    }
}

/// The fake window system.
pub struct FakeWindowSystem {
    ax: FakeWindowAx,
    observers: FakeObservers,
}

impl FakeWindowSystem {
    pub fn new(ax: FakeWindowAx) -> (Self, FakeObservers) {
        let observers = FakeObservers::default();
        (
            Self {
                ax,
                observers: observers.clone(),
            },
            observers,
        )
    }
}

impl FakeWindowSystem {
    /// A factory for adapters that build one system per shard thread: every
    /// system shares `ax` and one observer script, so assertions see all
    /// shards at once.
    pub fn factory(ax: FakeWindowAx) -> (impl Fn() -> Self + Send + Sync + 'static, FakeObservers) {
        let observers = FakeObservers::default();
        let shared = observers.clone();
        (
            move || Self {
                ax: ax.clone(),
                observers: shared.clone(),
            },
            observers,
        )
    }
}

impl WindowSystem for FakeWindowSystem {
    type Ax = FakeWindowAx;
    type Observer = FakeObserver;

    fn ax(&self) -> &FakeWindowAx { &self.ax }

    fn observer(&self, pid: i32) -> Result<FakeObserver, AxError> {
        let mut state = lock(&self.observers.state);
        if let Some(error) = state.create_error {
            return Err(error);
        }
        state.pids.entry(pid).or_default().observers_created += 1;
        drop(state);
        Ok(FakeObserver {
            pid,
            ax: self.ax.clone(),
            script: self.observers.clone(),
        })
    }
}

/// A clock whose every reading advances by `step`.
pub struct SteppingClock {
    now: Mutex<Instant>,
    step: Mutex<Duration>,
}

impl SteppingClock {
    pub fn new(step: Duration) -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(Instant::now()),
            step: Mutex::new(step),
        })
    }

    pub fn advance(&self, by: Duration) { *lock(&self.now) += by; }

    pub fn set_step(&self, step: Duration) { *lock(&self.step) = step; }

    pub fn peek(&self) -> Instant { *lock(&self.now) }
}

impl WindowClock for SteppingClock {
    fn now(&self) -> Instant {
        let step = *lock(&self.step);
        let mut now = lock(&self.now);
        let reading = *now;
        *now += step;
        reading
    }
}

/// A waker that counts wakes.
#[derive(Default)]
pub struct CountingWaker {
    wakes: Mutex<usize>,
}

impl CountingWaker {
    pub fn wakes(&self) -> usize { *lock(&self.wakes) }
}

impl Waker for CountingWaker {
    fn wake(&self) { *lock(&self.wakes) += 1; }
}

/// A driver that records every park timeout and runs one scripted step per park.
pub struct ScriptedDriver {
    pub parks: Arc<Mutex<Vec<Option<Duration>>>>,
    steps: Vec<Box<dyn FnOnce()>>,
    waker: Arc<CountingWaker>,
}

impl ScriptedDriver {
    pub fn new(steps: Vec<Box<dyn FnOnce()>>) -> (Self, Arc<Mutex<Vec<Option<Duration>>>>) {
        let parks = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                parks: Arc::clone(&parks),
                steps,
                waker: Arc::new(CountingWaker::default()),
            },
            parks,
        )
    }
}

impl Driver for ScriptedDriver {
    fn waker(&self) -> Arc<dyn Waker> { self.waker.clone() }

    fn park(&mut self, timeout: Option<Duration>) {
        lock(&self.parks).push(timeout);
        if !self.steps.is_empty() {
            self.steps.remove(0)();
        }
    }
}

/// A `VoiceOver` probe whose answer a test sets.
#[derive(Debug, Default)]
pub struct FakeVoiceOver {
    running: AtomicBool,
}

impl FakeVoiceOver {
    pub fn set(&self, running: bool) { self.running.store(running, Ordering::SeqCst); }
}

impl VoiceOverProbe for FakeVoiceOver {
    fn running(&self) -> bool { self.running.load(Ordering::SeqCst) }
}
