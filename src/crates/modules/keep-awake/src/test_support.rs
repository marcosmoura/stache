//! Shared fakes and a registry-backed harness for the Keep Awake suites.
//!
//! Every test runs over the injected assertion backend, lock probe, observation
//! backend and main-thread executor. Nothing here touches IOPM, CoreGraphics or
//! the real notification center, and the harness always cancels, stops and joins
//! its generation before dropping.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::{
    AssertionOwner, FakeAssertionBackend, FakeLockObservationBackend, FakeLockProbe,
    FakeNativeBarrier, LockObservationBackend, LockObservationInstaller, LockProbe,
    LockState as NativeLockState, ManualExecutor,
};
use stache_sdk::{
    EventBus, LogRecord, Logger, MainThreadExecutor, ModuleHandle, ModuleStatus, Permission,
    PermissionError, PermissionProvider, PermissionStatus, Registry, RegistryError, ReloadReport,
};

use crate::KeepAwakeModule;
use crate::config::KeepAwakeConfig;
use crate::runtime::Signals;
use crate::status::{KeepAwakeConditionKind, KeepAwakeRuntimeStatus, KeepAwakeSnapshot};

/// A generous real-time bound for any test wait.
pub const WAIT: Duration = Duration::from_secs(5);

/// Polls `condition` until it holds or [`WAIT`] elapses.
pub fn wait_until(label: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A logger that keeps every record in order.
#[derive(Default)]
pub struct CaptureLogger {
    records: Mutex<Vec<LogRecord>>,
}

impl Logger for CaptureLogger {
    fn log(&self, record: LogRecord) { lock(&self.records).push(record); }
}

impl CaptureLogger {
    pub fn records(&self) -> Vec<LogRecord> { lock(&self.records).clone() }

    pub fn messages(&self) -> Vec<String> {
        self.records().into_iter().map(|record| record.message).collect()
    }

    pub fn count_containing(&self, fragment: &str) -> usize {
        self.messages().iter().filter(|message| message.contains(fragment)).count()
    }
}

pub struct AllowAll;

impl PermissionProvider for AllowAll {
    fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

    fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(PermissionStatus::Granted)
    }
}

/// A registry with permissive host services and no windows.
#[must_use]
pub fn registry(logger: Arc<dyn Logger>) -> Registry {
    Registry::new(
        logger,
        Arc::new(EventBus::new()),
        Arc::new(ManualExecutor::new(true)),
        Arc::new(AllowAll),
        Arc::new(stache_sdk::UnavailableWindows),
    )
}

/// A deterministic, counting and optionally parked lock probe.
///
/// The scripted queue answers the next calls before the inner state is read, so
/// a test can make the pre-create probe and the post-create validation probe
/// disagree.
#[derive(Clone)]
pub struct Probe {
    inner: FakeLockProbe,
    calls: Arc<AtomicUsize>,
    block: Arc<Mutex<Option<Arc<FakeNativeBarrier>>>>,
    scripted: Arc<Mutex<VecDeque<NativeLockState>>>,
    observed_creates: Arc<Mutex<Vec<usize>>>,
    panic_next: Arc<AtomicBool>,
    backend: FakeAssertionBackend,
}

impl Probe {
    fn new(state: NativeLockState, backend: FakeAssertionBackend) -> Self {
        Self {
            inner: FakeLockProbe::new(state),
            calls: Arc::new(AtomicUsize::new(0)),
            block: Arc::new(Mutex::new(None)),
            scripted: Arc::new(Mutex::new(VecDeque::new())),
            observed_creates: Arc::new(Mutex::new(Vec::new())),
            panic_next: Arc::new(AtomicBool::new(false)),
            backend,
        }
    }

    pub fn set(&self, state: NativeLockState) { self.inner.set(state); }

    pub fn script(&self, states: impl IntoIterator<Item = NativeLockState>) {
        lock(&self.scripted).extend(states);
    }

    pub fn calls(&self) -> usize { self.calls.load(Ordering::SeqCst) }

    pub fn observed_creates(&self) -> Vec<usize> { lock(&self.observed_creates).clone() }

    /// Makes the next probe call panic, as a native failure on the worker would.
    pub fn panic_next(&self) { self.panic_next.store(true, Ordering::SeqCst); }

    /// Parks the next probe call until the returned barrier is released.
    pub fn block_next(&self) -> Arc<FakeNativeBarrier> {
        let barrier = Arc::new(FakeNativeBarrier::default());
        *lock(&self.block) = Some(Arc::clone(&barrier));
        barrier
    }
}

impl LockProbe for Probe {
    fn probe(&self) -> NativeLockState {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(
            !self.panic_next.swap(false, Ordering::SeqCst),
            "test-only probe panic"
        );
        lock(&self.observed_creates).push(self.backend.created().len());
        let barrier = lock(&self.block).take();
        if let Some(barrier) = barrier {
            let _ = barrier.park(None, WAIT);
        }
        let scripted = lock(&self.scripted).pop_front();
        if let Some(state) = scripted {
            return state;
        }
        self.inner.probe()
    }
}

/// One registered module over deterministic fakes.
pub struct Harness {
    pub registry: Arc<Registry>,
    pub handle: ModuleHandle<KeepAwakeConfig>,
    pub backend: FakeAssertionBackend,
    pub observation: FakeLockObservationBackend,
    pub probe: Probe,
    pub owner: AssertionOwner,
    pub snapshot: KeepAwakeSnapshot,
    pub captured: Arc<CaptureLogger>,
    signals: Arc<Mutex<Vec<Arc<Signals>>>>,
}

impl Harness {
    pub fn new(enabled: bool) -> Self {
        Self::build(enabled, NativeLockState::Unlocked, |module| module)
    }

    pub fn locked() -> Self { Self::build(true, NativeLockState::Locked, |module| module) }

    pub fn build(
        enabled: bool,
        state: NativeLockState,
        configure: impl FnOnce(KeepAwakeModule) -> KeepAwakeModule,
    ) -> Self {
        let executor = Arc::new(ManualExecutor::new(true));
        let backend = FakeAssertionBackend::new();
        let probe = Probe::new(state, backend.clone());
        let observation = FakeLockObservationBackend::new();
        let installer = LockObservationInstaller::new(
            Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
            Arc::new(observation.clone()) as Arc<dyn LockObservationBackend>,
        );
        let owner = AssertionOwner::new(Arc::new(backend.clone()));
        let snapshot = KeepAwakeSnapshot::default();
        let captured = Arc::new(CaptureLogger::default());
        let registry = Arc::new(registry(Arc::clone(&captured) as Arc<dyn Logger>));
        let signals = Arc::new(Mutex::new(Vec::new()));
        let module = KeepAwakeModule::with_snapshot(
            owner.clone(),
            Arc::new(probe.clone()) as Arc<dyn LockProbe>,
            installer,
            snapshot.clone(),
        );
        let module = configure(module).with_signals_probe(Arc::clone(&signals));
        let handle = registry
            .register(module, KeepAwakeConfig { enabled })
            .expect("register keepAwake");
        Self {
            registry,
            handle,
            backend,
            observation,
            probe,
            owner,
            snapshot,
            captured,
            signals,
        }
    }

    pub fn start(&self) -> Result<(), RegistryError> { self.registry.start_all() }

    pub fn status(&self) -> ModuleStatus {
        self.registry.snapshot(self.handle).expect("snapshot").status
    }

    pub fn runtime(&self) -> KeepAwakeRuntimeStatus { self.snapshot.read() }

    pub fn conditions(&self) -> Vec<KeepAwakeConditionKind> {
        self.runtime().conditions.into_iter().map(|condition| condition.kind).collect()
    }

    pub fn deliver(&self) -> bool { self.observation.deliver() }

    /// The newest generation's signals.
    pub fn signals(&self) -> Arc<Signals> {
        lock(&self.signals).last().cloned().expect("a started generation")
    }

    pub fn generation_count(&self) -> usize { lock(&self.signals).len() }

    pub fn set_enabled(&self, enabled: bool) -> Result<(), RegistryError> {
        self.registry.set_enabled(self.handle, enabled)
    }

    pub fn pause(&self) -> Result<(), RegistryError> { self.registry.pause(self.handle) }

    pub fn resume(&self) -> Result<(), RegistryError> { self.registry.resume(self.handle) }

    pub fn reload(&self, config: KeepAwakeConfig) -> Result<ReloadReport, RegistryError> {
        self.registry.reload(self.handle, config)
    }

    /// Waits until the worker is parked, then advances the fake clock by `by`.
    pub fn advance(&self, by: Duration) {
        let signals = self.signals();
        signals.wait_until_waiting(WAIT);
        signals.advance(by);
    }

    /// Advances one full reconciliation interval.
    pub fn tick(&self) { self.advance(Duration::from_secs(2)); }
}

impl Drop for Harness {
    fn drop(&mut self) {
        for signals in lock(&self.signals).iter() {
            signals.stop();
        }
        let _ = self.registry.shutdown();
    }
}
