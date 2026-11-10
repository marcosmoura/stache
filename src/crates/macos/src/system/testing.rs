//! Scripted fakes for the system readers. A fake never reads the machine.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::{
    BatteryBackend, BatteryReadout, CpuUsageBackend, SmcBackend, SmcConnection, SmcKey, SmcOpener,
    SmcRaw, SystemReadError, WifiBackend, WifiInterfaceReading,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct State<T> {
    queue: VecDeque<T>,
    last: T,
    held: bool,
    inside: usize,
}

/// A scripted answer: queued values first, then the repeated last value. It
/// can be held, which parks every caller inside the read until released (a
/// hung native read).
pub struct Script<T> {
    state: Mutex<State<T>>,
    changed: Condvar,
    calls: AtomicUsize,
}

impl<T: Clone> Script<T> {
    /// A script answering `value` until told otherwise.
    #[must_use]
    pub fn new(value: T) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                last: value,
                held: false,
                inside: 0,
            }),
            changed: Condvar::new(),
            calls: AtomicUsize::new(0),
        })
    }

    /// Queues one answer.
    pub fn push(&self, value: T) { lock(&self.state).queue.push_back(value); }

    /// Replaces the repeated answer.
    pub fn set(&self, value: T) { lock(&self.state).last = value; }

    /// Parks every following call until [`release`](Self::release).
    pub fn hold(&self) { lock(&self.state).held = true; }

    /// Releases every parked call.
    pub fn release(&self) {
        lock(&self.state).held = false;
        self.changed.notify_all();
    }

    /// The number of calls started.
    #[must_use]
    pub fn calls(&self) -> usize { self.calls.load(Ordering::SeqCst) }

    /// Waits until `count` calls are parked inside the read.
    #[allow(clippy::significant_drop_tightening)] // the guard is the wait state
    #[must_use]
    pub fn wait_inside(&self, count: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        while state.inside < count {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    #[allow(clippy::significant_drop_tightening)] // the guard is the wait state
    fn answer(&self) -> T {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut state = lock(&self.state);
        if state.held {
            state.inside += 1;
            self.changed.notify_all();
            while state.held {
                state = self.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
            state.inside -= 1;
        }
        let next = state.queue.pop_front();
        next.unwrap_or_else(|| state.last.clone())
    }
}

/// A scripted CPU usage source.
pub struct FakeCpuUsage(pub Arc<Script<Result<f32, SystemReadError>>>);

impl FakeCpuUsage {
    /// A source answering `usage`.
    #[must_use]
    pub fn new(usage: f32) -> Self { Self(Script::new(Ok(usage))) }
}

impl CpuUsageBackend for FakeCpuUsage {
    fn usage(&self) -> Result<f32, SystemReadError> { self.0.answer() }
}

/// A scripted CPU temperature (the provider-level SMC fake).
pub struct FakeSmc(pub Arc<Script<Result<Option<f64>, SystemReadError>>>);

impl FakeSmc {
    /// An SMC answering `temperature`.
    #[must_use]
    pub fn new(temperature: Option<f64>) -> Self { Self(Script::new(Ok(temperature))) }

    /// An SMC that fails to open.
    #[must_use]
    pub fn unavailable() -> Self { Self(Script::new(Err(SystemReadError::new("smc unavailable")))) }
}

impl SmcBackend for FakeSmc {
    fn cpu_temperature(&self) -> Result<Option<f64>, SystemReadError> { self.0.answer() }
}

/// A scripted SMC key table, shared by every connection it opens.
#[derive(Clone, Default)]
pub struct FakeSmcKeys {
    state: Arc<Mutex<FakeSmcState>>,
}

#[derive(Default)]
struct FakeSmcState {
    keys: HashMap<SmcKey, VecDeque<SmcRaw>>,
    open_error: Option<SystemReadError>,
    read_error: Option<SystemReadError>,
    opens: usize,
    reads: usize,
}

impl FakeSmcKeys {
    /// Queues `raw` for `key`; the last queued value repeats.
    pub fn push(&self, key: &str, raw: SmcRaw) {
        if let Some(key) = SmcKey::parse(key) {
            lock(&self.state).keys.entry(key).or_default().push_back(raw);
        }
    }

    /// Sets `key` to the single value `raw`.
    pub fn set(&self, key: &str, raw: SmcRaw) {
        if let Some(key) = SmcKey::parse(key) {
            lock(&self.state).keys.insert(key, VecDeque::from([raw]));
        }
    }

    /// Removes every key.
    pub fn clear(&self) { lock(&self.state).keys.clear(); }

    /// Makes `open` fail (or succeed again with `None`).
    pub fn fail_open(&self, error: Option<SystemReadError>) {
        lock(&self.state).open_error = error;
    }

    /// Makes every read fail (or succeed again with `None`).
    pub fn fail_reads(&self, error: Option<SystemReadError>) {
        lock(&self.state).read_error = error;
    }

    /// The connections opened so far.
    #[must_use]
    pub fn opens(&self) -> usize { lock(&self.state).opens }

    /// The key reads so far.
    #[must_use]
    pub fn reads(&self) -> usize { lock(&self.state).reads }
}

impl SmcOpener for FakeSmcKeys {
    fn open(&self) -> Result<Box<dyn SmcConnection>, SystemReadError> {
        let mut state = lock(&self.state);
        state.opens += 1;
        if let Some(error) = state.open_error.clone() {
            return Err(error);
        }
        drop(state);
        Ok(Box::new(self.clone()))
    }
}

impl SmcConnection for FakeSmcKeys {
    fn read(&mut self, key: SmcKey) -> Result<Option<SmcRaw>, SystemReadError> {
        let mut state = lock(&self.state);
        state.reads += 1;
        if let Some(error) = state.read_error.clone() {
            return Err(error);
        }
        Ok(state.keys.get_mut(&key).and_then(|queue| {
            if queue.len() > 1 {
                queue.pop_front()
            } else {
                queue.front().cloned()
            }
        }))
    }
}

/// A scripted battery.
pub struct FakeBattery(pub Arc<Script<Result<Option<BatteryReadout>, SystemReadError>>>);

impl BatteryBackend for FakeBattery {
    fn read(&self) -> Result<Option<BatteryReadout>, SystemReadError> { self.0.answer() }
}

/// Scripted Wi-Fi interfaces.
pub struct FakeWifi(pub Arc<Script<Result<Vec<WifiInterfaceReading>, SystemReadError>>>);

impl WifiBackend for FakeWifi {
    fn interfaces(&self) -> Result<Vec<WifiInterfaceReading>, SystemReadError> { self.0.answer() }
}
