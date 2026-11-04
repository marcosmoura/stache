//! Deterministic Keep Awake doubles.
//!
//! These fakes plug into the real owner, generation and dispatch machinery. They
//! never call IOPM, CoreGraphics, the notification center or the screen lock.
//! [`FakeNativeBarrier`] freezes one backend call so a test can observe
//! `acquiring`, in-flight rollback or an in-progress release before the native
//! completion. Creation barriers also observe lifecycle cancellation.
//!
//! [`ManualExecutor`] is re-exported so a runtime test does not need a second
//! dispatcher double.

// The fakes hold their state mutex while assembling one result. There is no
// contention to reduce and splitting the guards would obscure the double.
#![allow(clippy::significant_drop_tightening)]

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_sdk::CancellationToken;

use super::assertion::{
    AssertionBackend, AssertionError, AssertionId, AssertionKind, OwnedAssertion,
};
use super::lock::{LockProbe, LockState};
use super::observation::{LockObservationBackend, LockObservationError, LockObserverHandle};
#[doc(inline)]
pub use crate::applications::testing::ManualExecutor;

const DEFAULT_BARRIER_TIMEOUT: Duration = Duration::from_secs(5);
const PARK_QUANTUM: Duration = Duration::from_millis(10);

/// The outcome of parking on a [`FakeNativeBarrier`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarrierWait {
    /// The test released the barrier.
    Released,
    /// The finite timeout expired before release.
    TimedOut,
    /// The observed cancellation token fired before release.
    Cancelled,
}

/// A finite one-shot barrier a test releases explicitly.
///
/// A backend call parks inside [`FakeNativeBarrier::park`] until the test calls
/// [`FakeNativeBarrier::release`], [`FakeNativeBarrier::cancel`] or the finite
/// timeout expires. Creation parks observe the lifecycle token; release and
/// removal parks ignore it because required native cleanup must still run, and
/// use the independent [`FakeNativeBarrier::cancel`] action instead. A cancelled
/// or timed-out fake call reports failure instead of silently succeeding.
#[derive(Debug, Default)]
pub struct FakeNativeBarrier {
    state: Mutex<BarrierState>,
    signal: Condvar,
}

#[derive(Debug, Default)]
struct BarrierState {
    entered: bool,
    released: bool,
    cancelled: bool,
}

impl FakeNativeBarrier {
    /// Waits until a backend call has entered the barrier.
    #[must_use]
    pub fn wait_entered(&self, timeout: Duration) -> bool {
        let state = lock(&self.state);
        let (state, _) = self
            .signal
            .wait_timeout_while(state, timeout, |state| !state.entered)
            .unwrap_or_else(PoisonError::into_inner);
        state.entered
    }

    /// Releases the parked call. Returns whether it had entered.
    #[must_use]
    pub fn release(&self) -> bool {
        let entered = {
            let mut state = lock(&self.state);
            state.released = true;
            state.entered
        };
        self.signal.notify_all();
        entered
    }

    /// Interrupts a parked cleanup call without a lifecycle token.
    ///
    /// This is the independent cancellation path for release and removal parks:
    /// the fake call reports an interrupted failure, so the exact ID stays
    /// unresolved instead of the cleanup being skipped or called successful.
    /// Returns whether the call had entered.
    #[must_use]
    pub fn cancel(&self) -> bool {
        let entered = {
            let mut state = lock(&self.state);
            state.cancelled = true;
            state.entered
        };
        self.signal.notify_all();
        entered
    }

    /// Parks the calling backend until release, cancellation or timeout.
    #[must_use]
    pub fn park(&self, cancellation: Option<&CancellationToken>, timeout: Duration) -> BarrierWait {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        state.entered = true;
        self.signal.notify_all();
        loop {
            // Cancellation wins over a simultaneous release so a cancelled
            // creation can never be observed after the cancel point.
            if cancellation.is_some_and(CancellationToken::is_cancelled) || state.cancelled {
                return BarrierWait::Cancelled;
            }
            if state.released {
                return BarrierWait::Released;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return BarrierWait::TimedOut;
            };
            let slice = remaining.min(PARK_QUANTUM);
            let (next, _) =
                self.signal.wait_timeout(state, slice).unwrap_or_else(PoisonError::into_inner);
            state = next;
        }
    }
}

/// A deterministic assertion backend.
#[derive(Clone)]
pub struct FakeAssertionBackend {
    state: Arc<FakeAssertionState>,
}

struct FakeAssertionState {
    next_id: AtomicU32,
    fixed_ids: Mutex<Option<[u32; 3]>>,
    barrier_timeout_ms: AtomicU64,
    created: Mutex<Vec<OwnedAssertion>>,
    create_attempts: Mutex<Vec<AssertionKind>>,
    release_attempts: Mutex<Vec<(AssertionKind, AssertionId)>>,
    fail_create: Mutex<Option<(AssertionKind, i32)>>,
    fail_release: Mutex<Option<(AssertionKind, i32)>>,
    fail_release_always: Mutex<Option<(AssertionKind, i32)>>,
    cancel_after_create: Mutex<Option<AssertionKind>>,
    block_create: Mutex<Option<Arc<FakeNativeBarrier>>>,
    block_create_kind: Mutex<std::collections::BTreeMap<AssertionKind, Arc<FakeNativeBarrier>>>,
    block_release: Mutex<Option<Arc<FakeNativeBarrier>>>,
}

impl Default for FakeAssertionBackend {
    fn default() -> Self {
        Self {
            state: Arc::new(FakeAssertionState {
                next_id: AtomicU32::new(100),
                fixed_ids: Mutex::new(None),
                barrier_timeout_ms: AtomicU64::new(
                    u64::try_from(DEFAULT_BARRIER_TIMEOUT.as_millis()).unwrap_or(5_000),
                ),
                created: Mutex::new(Vec::new()),
                create_attempts: Mutex::new(Vec::new()),
                release_attempts: Mutex::new(Vec::new()),
                fail_create: Mutex::new(None),
                fail_release: Mutex::new(None),
                fail_release_always: Mutex::new(None),
                cancel_after_create: Mutex::new(None),
                block_create: Mutex::new(None),
                block_create_kind: Mutex::new(std::collections::BTreeMap::new()),
                block_release: Mutex::new(None),
            }),
        }
    }
}

impl FakeAssertionBackend {
    /// Creates an empty fake.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Uses exact numeric IDs for display, idle and system creation.
    pub fn set_ids(&self, ids: [u32; 3]) { *lock(&self.state.fixed_ids) = Some(ids); }

    /// Shortens the finite barrier timeout for this fake.
    pub fn set_barrier_timeout(&self, timeout: Duration) {
        self.state.barrier_timeout_ms.store(
            u64::try_from(timeout.as_millis()).unwrap_or(5_000),
            Ordering::SeqCst,
        );
    }

    /// Fails the next creation of `kind` with `code`.
    pub fn fail_next_create(&self, kind: AssertionKind, code: i32) {
        *lock(&self.state.fail_create) = Some((kind, code));
    }

    /// Fails the next release of `kind` with `code`.
    pub fn fail_next_release(&self, kind: AssertionKind, code: i32) {
        *lock(&self.state.fail_release) = Some((kind, code));
    }

    /// Fails every release of `kind` with `code` until cleared.
    ///
    /// Use this to prove that repeated cleanup passes keep the exact ID pending
    /// instead of treating the first failure as a one-shot anomaly.
    pub fn fail_release_always(&self, kind: AssertionKind, code: i32) {
        *lock(&self.state.fail_release_always) = Some((kind, code));
    }

    /// Cancels the lifecycle token immediately after a successful `kind` create.
    ///
    /// This models cancellation arriving after the final native create returned,
    /// so the owner must roll back every ID it already recorded.
    pub fn cancel_after_create(&self, kind: AssertionKind) {
        *lock(&self.state.cancel_after_create) = Some(kind);
    }

    /// Clears any one-shot or persistent failure.
    pub fn clear_failures(&self) {
        *lock(&self.state.fail_create) = None;
        *lock(&self.state.fail_release) = None;
        *lock(&self.state.fail_release_always) = None;
        *lock(&self.state.cancel_after_create) = None;
    }

    /// Parks the next creation before it records success.
    #[must_use]
    pub fn block_next_create(&self) -> Arc<FakeNativeBarrier> {
        let barrier = Arc::new(FakeNativeBarrier::default());
        *lock(&self.state.block_create) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next creation of one exact kind before it records success.
    #[must_use]
    pub fn block_next_create_of(&self, kind: AssertionKind) -> Arc<FakeNativeBarrier> {
        let barrier = Arc::new(FakeNativeBarrier::default());
        lock(&self.state.block_create_kind).insert(kind, Arc::clone(&barrier));
        barrier
    }

    /// Parks the next release before it returns.
    #[must_use]
    pub fn block_next_release(&self) -> Arc<FakeNativeBarrier> {
        let barrier = Arc::new(FakeNativeBarrier::default());
        *lock(&self.state.block_release) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Every successful creation in call order.
    #[must_use]
    pub fn created(&self) -> Vec<OwnedAssertion> { lock(&self.state.created).clone() }

    /// Every creation attempt in call order.
    #[must_use]
    pub fn create_attempts(&self) -> Vec<AssertionKind> {
        lock(&self.state.create_attempts).clone()
    }

    /// Every release attempt in call order.
    #[must_use]
    pub fn release_attempts(&self) -> Vec<(AssertionKind, AssertionId)> {
        lock(&self.state.release_attempts).clone()
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.state.barrier_timeout_ms.load(Ordering::SeqCst))
    }

    fn take_barrier(
        slot: &Mutex<Option<Arc<FakeNativeBarrier>>>,
    ) -> Option<Arc<FakeNativeBarrier>> {
        lock(slot).take()
    }
}

impl AssertionBackend for FakeAssertionBackend {
    fn create(
        &self,
        kind: AssertionKind,
        cancellation: &CancellationToken,
    ) -> Result<AssertionId, AssertionError> {
        let kind_barrier = lock(&self.state.block_create_kind).remove(&kind);
        if let Some(barrier) = kind_barrier {
            match barrier.park(Some(cancellation), self.timeout()) {
                BarrierWait::Released => {}
                BarrierWait::Cancelled => return Err(AssertionError::Cancelled),
                BarrierWait::TimedOut => return Err(AssertionError::Interrupted),
            }
        }
        if let Some(barrier) = Self::take_barrier(&self.state.block_create) {
            match barrier.park(Some(cancellation), self.timeout()) {
                BarrierWait::Released => {}
                BarrierWait::Cancelled => return Err(AssertionError::Cancelled),
                BarrierWait::TimedOut => return Err(AssertionError::Interrupted),
            }
        }
        lock(&self.state.create_attempts).push(kind);
        if let Some((failed, code)) = { *lock(&self.state.fail_create) }
            && failed == kind
        {
            *lock(&self.state.fail_create) = None;
            return Err(AssertionError::Native { code });
        }
        let index = match kind {
            AssertionKind::Display => 0,
            AssertionKind::Idle => 1,
            AssertionKind::System => 2,
        };
        let raw = lock(&self.state.fixed_ids).map_or_else(
            || self.state.next_id.fetch_add(1, Ordering::SeqCst),
            |ids| ids[index],
        );
        let owned = OwnedAssertion {
            kind,
            id: AssertionId::new(raw),
        };
        lock(&self.state.created).push(owned);
        if *lock(&self.state.cancel_after_create) == Some(kind) {
            cancellation.cancel();
        }
        Ok(owned.id)
    }

    fn release(&self, kind: AssertionKind, id: AssertionId) -> Result<(), AssertionError> {
        lock(&self.state.release_attempts).push((kind, id));
        if let Some(barrier) = Self::take_barrier(&self.state.block_release)
            && barrier.park(None, self.timeout()) != BarrierWait::Released
        {
            // A timeout or independent barrier cancellation must never look like
            // a confirmed release: the owner keeps this exact ID unresolved.
            return Err(AssertionError::Interrupted);
        }
        if let Some((failed, code)) = { *lock(&self.state.fail_release_always) }
            && failed == kind
        {
            return Err(AssertionError::Native { code });
        }
        if let Some((failed, code)) = { *lock(&self.state.fail_release) }
            && failed == kind
        {
            *lock(&self.state.fail_release) = None;
            return Err(AssertionError::Native { code });
        }
        Ok(())
    }
}

/// A deterministic lock probe.
#[derive(Clone)]
pub struct FakeLockProbe {
    state: Arc<Mutex<LockState>>,
}

impl FakeLockProbe {
    /// Creates a probe returning `state`.
    #[must_use]
    pub fn new(state: LockState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// Replaces the state returned by later probes.
    pub fn set(&self, state: LockState) { *lock(&self.state) = state; }
}

impl LockProbe for FakeLockProbe {
    fn probe(&self) -> LockState { *lock(&self.state) }
}

/// A deterministic observation backend.
#[derive(Clone)]
pub struct FakeLockObservationBackend {
    state: Arc<FakeObservationState>,
}

struct FakeInstalled {
    handle: LockObserverHandle,
    generation: u64,
    wake: Arc<dyn Fn() + Send + Sync>,
}

struct FakeObservationState {
    barrier_timeout_ms: AtomicU64,
    installed: Mutex<Option<FakeInstalled>>,
    install_calls: Mutex<Vec<(LockObserverHandle, u64)>>,
    remove_calls: Mutex<Vec<(LockObserverHandle, u64)>>,
    fail_install: Mutex<Option<LockObservationError>>,
    fail_remove: Mutex<Option<LockObservationError>>,
    block_install: Mutex<Option<Arc<FakeNativeBarrier>>>,
    block_install_after: Mutex<Option<Arc<FakeNativeBarrier>>>,
    block_remove: Mutex<Option<Arc<FakeNativeBarrier>>>,
}

impl Default for FakeLockObservationBackend {
    fn default() -> Self {
        Self {
            state: Arc::new(FakeObservationState {
                barrier_timeout_ms: AtomicU64::new(
                    u64::try_from(DEFAULT_BARRIER_TIMEOUT.as_millis()).unwrap_or(5_000),
                ),
                installed: Mutex::new(None),
                install_calls: Mutex::new(Vec::new()),
                remove_calls: Mutex::new(Vec::new()),
                fail_install: Mutex::new(None),
                fail_remove: Mutex::new(None),
                block_install: Mutex::new(None),
                block_install_after: Mutex::new(None),
                block_remove: Mutex::new(None),
            }),
        }
    }
}

impl FakeLockObservationBackend {
    /// Creates an empty fake.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Shortens the finite barrier timeout for this fake.
    pub fn set_barrier_timeout(&self, timeout: Duration) {
        self.state.barrier_timeout_ms.store(
            u64::try_from(timeout.as_millis()).unwrap_or(5_000),
            Ordering::SeqCst,
        );
    }

    /// Fails the next install with the exact typed error.
    pub fn fail_next_install(&self, error: LockObservationError) {
        *lock(&self.state.fail_install) = Some(error);
    }

    /// Fails the next removal with the exact typed error.
    pub fn fail_next_remove(&self, error: LockObservationError) {
        *lock(&self.state.fail_remove) = Some(error);
    }

    /// Parks the next install before it registers.
    #[must_use]
    pub fn block_next_install(&self) -> Arc<FakeNativeBarrier> {
        let barrier = Arc::new(FakeNativeBarrier::default());
        *lock(&self.state.block_install) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next install after it registered, before it returns.
    #[must_use]
    pub fn block_next_install_after_registration(&self) -> Arc<FakeNativeBarrier> {
        let barrier = Arc::new(FakeNativeBarrier::default());
        *lock(&self.state.block_install_after) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next removal before it removes.
    #[must_use]
    pub fn block_next_remove(&self) -> Arc<FakeNativeBarrier> {
        let barrier = Arc::new(FakeNativeBarrier::default());
        *lock(&self.state.block_remove) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Delivers the installed refresh hint. Returns whether one is installed.
    #[must_use]
    pub fn deliver(&self) -> bool {
        let wake = lock(&self.state.installed).as_ref().map(|state| Arc::clone(&state.wake));
        let Some(wake) = wake else {
            return false;
        };
        wake();
        true
    }

    /// Captures the currently installed pure signal.
    ///
    /// Tests keep this across a successor install to prove a stale callback
    /// cannot raise or clear the successor's generation-local hint.
    #[must_use]
    pub fn installed_wake(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        lock(&self.state.installed).as_ref().map(|state| Arc::clone(&state.wake))
    }

    /// Delivers the currently installed hint from a captured generation.
    #[must_use]
    pub fn deliver_generation(&self, handle: LockObserverHandle, generation: u64) -> bool {
        let wake = lock(&self.state.installed).as_ref().and_then(|state| {
            (state.handle == handle && state.generation == generation)
                .then(|| Arc::clone(&state.wake))
        });
        let Some(wake) = wake else {
            return false;
        };
        wake();
        true
    }

    /// The installed generation for `handle`.
    #[must_use]
    pub fn installed_generation(&self, handle: LockObserverHandle) -> Option<u64> {
        lock(&self.state.installed)
            .as_ref()
            .filter(|state| state.handle == handle)
            .map(|state| state.generation)
    }

    /// Whether any generation is installed.
    #[must_use]
    pub fn is_installed(&self) -> bool { lock(&self.state.installed).is_some() }

    /// Every install attempt in call order.
    #[must_use]
    pub fn install_calls(&self) -> Vec<(LockObserverHandle, u64)> {
        lock(&self.state.install_calls).clone()
    }

    /// Every remove attempt in call order.
    #[must_use]
    pub fn remove_calls(&self) -> Vec<(LockObserverHandle, u64)> {
        lock(&self.state.remove_calls).clone()
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.state.barrier_timeout_ms.load(Ordering::SeqCst))
    }
}

impl LockObservationBackend for FakeLockObservationBackend {
    fn install(
        &self,
        handle: LockObserverHandle,
        generation: u64,
        wake: Arc<dyn Fn() + Send + Sync>,
        cancellation: &CancellationToken,
    ) -> Result<(), LockObservationError> {
        let barrier = lock(&self.state.block_install).take();
        if let Some(barrier) = barrier {
            match barrier.park(Some(cancellation), self.timeout()) {
                BarrierWait::Released => {}
                BarrierWait::Cancelled => return Err(LockObservationError::Cancelled),
                BarrierWait::TimedOut => return Err(LockObservationError::TimedOut),
            }
        }
        lock(&self.state.install_calls).push((handle, generation));
        let failure = lock(&self.state.fail_install).take();
        if let Some(error) = failure {
            return Err(error);
        }
        // Replacement models a successor on the same handle: a stale removal of
        // the predecessor's generation must leave this entry untouched.
        *lock(&self.state.installed) = Some(FakeInstalled { handle, generation, wake });
        let barrier = lock(&self.state.block_install_after).take();
        if let Some(barrier) = barrier
            && barrier.park(None, self.timeout()) != BarrierWait::Released
        {
            return Err(LockObservationError::TimedOut);
        }
        Ok(())
    }

    fn remove(
        &self,
        handle: LockObserverHandle,
        generation: u64,
    ) -> Result<(), LockObservationError> {
        lock(&self.state.remove_calls).push((handle, generation));
        let barrier = lock(&self.state.block_remove).take();
        if let Some(barrier) = barrier
            && barrier.park(None, self.timeout()) != BarrierWait::Released
        {
            // Timeout or independent cancellation keeps the exact registration
            // owned; it must never silently report a completed removal.
            return Err(LockObservationError::TimedOut);
        }
        let failure = lock(&self.state.fail_remove).take();
        if let Some(error) = failure {
            return Err(error);
        }
        let mut installed = lock(&self.state.installed);
        if installed
            .as_ref()
            .is_some_and(|state| state.handle == handle && state.generation == generation)
        {
            *installed = None;
        }
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
