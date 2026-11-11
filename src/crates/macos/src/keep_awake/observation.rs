//! Main-thread-owned distributed notification observation.
//!
//! The observer is a `MainThreadOnly` selector target retained in a
//! main-thread-local map keyed by an opaque handle and generation. The
//! distributed notification center does not retain selector observers, so the
//! map owns the target until an exact-generation removal completes. Callers only
//! see `Send + Sync` handles and a coalesced refresh flag.
//!
//! Delivery is a hint at most: the installed Foundation header
//! `NSDistributedNotificationCenter.h` states that distributed notifications use
//! the main run loop and can be delayed or dropped. The callback ignores payload,
//! contains panics (unwind-enabled builds only; release aborts) and only signals. It never probes,
//! acquires an assertion or
//! runs lifecycle code.
//!
//! # Dispatch
//!
//! Off-main installation and removal use the host `MainThreadExecutor` and wait
//! at most [`OBSERVATION_BUDGET`] per hop, rechecking cancellation at least every
//! [`OBSERVATION_CANCELLATION_QUANTUM`]. A call made on the main thread runs
//! inline. The generation gate is held only for field updates and is never held
//! across a native callback, a backend call or an acknowledgement wait.
//!
//! Stop takes a separate *wait* cancellation token. Logical invalidation and the
//! exact native cleanup dispatch always happen first, even when that token is
//! already cancelled; only the acknowledgement wait may return
//! [`LockObservationError::Cancelled`] promptly. Cancellation therefore never
//! skips required removal. Dropping a [`LockObservation`] schedules the same
//! exact removal without waiting.
//!
//! A cancelled or timed-out install invalidates its generation immediately and
//! keeps cleanup owned. The queued install rechecks the exact generation and
//! cancellation before registration and again afterwards; if native registration
//! completed in the meantime, it removes exactly that generation. A later
//! main-queue drain therefore cannot revive a timed-out install or disturb a
//! successor. If the main queue never drains, the registered observer can only be
//! removed when it eventually runs; that eventual cleanup is documented here and
//! deliberately not replaced by a detached thread.
//!
//! Refresh coalescing is generation-local: every install owns one
//! `RefreshSignal`. A predecessor callback, `take_refresh` or contained panic
//! can only touch its own slot, never a successor's hint.
//!
//! # Manual observation
//!
//! A real notification delivery requires a live application main run loop. The
//! default test suite uses fake backends and never proves real delivery.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    NSDistributedNotificationCenter, NSNotification, NSNotificationSuspensionBehavior, NSObject,
    NSString,
};
use stache_sdk::{CancellationToken, MainThreadExecutor};

use super::lock::{SCREEN_LOCK_NOTIFICATION, SCREEN_UNLOCK_NOTIFICATION};

/// How long one dispatched observation hop may wait for its acknowledgement.
///
/// The host's main-thread smoke harness waits up to five seconds for one hop, so
/// this per-hop bound stays well inside it. The main dispatch queue normally
/// acknowledges in microseconds; this is not a shutdown SLA.
pub const OBSERVATION_BUDGET: Duration = Duration::from_secs(2);

/// The longest a waiting install sleeps before rechecking cancellation.
///
/// The native dispatch path has no cancellation wakeup hook, so the wait polls
/// at this quantum. It is small enough for lifecycle responsiveness and large
/// enough to avoid a busy loop.
pub const OBSERVATION_CANCELLATION_QUANTUM: Duration = Duration::from_millis(10);

const GATE_RETRY: Duration = Duration::from_millis(1);

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// An opaque key for one installer's main-thread observation state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LockObserverHandle(u64);

impl LockObserverHandle {
    fn allocate() -> Self { Self(NEXT_HANDLE.fetch_add(1, Ordering::Relaxed)) }
}

/// A failed observation install, removal or dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockObservationError {
    /// A backend operation was invoked off the main thread.
    NotMainThread,
    /// The host dispatcher rejected the job.
    Dispatch(String),
    /// The notification service is unavailable; only a probe fallback remains.
    Unavailable(String),
    /// No acknowledgement arrived inside [`OBSERVATION_BUDGET`].
    TimedOut,
    /// Lifecycle cancellation arrived before the install completed.
    Cancelled,
    /// An observation is already installed on this installer.
    AlreadyInstalled,
    /// Native selector registration failed.
    InstallFailed(String),
    /// Native selector removal failed.
    RemovalFailed(String),
}

impl fmt::Display for LockObservationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMainThread => f.write_str("screen lock observation requires the main thread"),
            Self::Dispatch(message) => write!(f, "cannot dispatch to the main thread: {message}"),
            Self::Unavailable(message) => {
                write!(f, "screen lock observation is unavailable: {message}")
            }
            Self::TimedOut => f.write_str("timed out waiting for a native acknowledgement"),
            Self::Cancelled => f.write_str("the screen lock installation was cancelled"),
            Self::AlreadyInstalled => f.write_str("a screen lock observation is already installed"),
            Self::InstallFailed(message) => {
                write!(f, "cannot install screen lock observation: {message}")
            }
            Self::RemovalFailed(message) => {
                write!(f, "cannot remove screen lock observation: {message}")
            }
        }
    }
}

impl std::error::Error for LockObservationError {}

/// The injectable native observation operations.
///
/// Implementations run on the main thread and must keep every `!Send` native
/// object there. The system backend stores the retained selector target in a
/// main-thread-local map; fake backends store only pure test state.
pub trait LockObservationBackend: Send + Sync {
    /// Registers selector observation for one exact generation.
    ///
    /// `wake` is a pure refresh signal. The backend must call it only for a
    /// delivered notification; it must not probe, acquire or run lifecycle code.
    ///
    /// # Errors
    /// Returns [`LockObservationError::NotMainThread`] off-main, an unavailable
    /// service or a registration failure. The caller owns rollback.
    fn install(
        &self,
        handle: LockObserverHandle,
        generation: u64,
        wake: Arc<dyn Fn() + Send + Sync>,
        cancellation: &CancellationToken,
    ) -> Result<(), LockObservationError>;

    /// Removes observation for one exact generation.
    ///
    /// This is idempotent. A different live generation on the same handle is a
    /// successor and must be left untouched.
    ///
    /// # Errors
    /// Returns a removal failure; the caller keeps the generation invalid and
    /// retains cleanup ownership.
    fn remove(
        &self,
        handle: LockObserverHandle,
        generation: u64,
    ) -> Result<(), LockObservationError>;
}

/// The module-facing observation installer.
///
/// It is `Send + Sync`, starts no work and owns no native object. Clone it
/// freely; all clones share one handle, generation gate and backend entry.
#[derive(Clone)]
pub struct LockObservationInstaller {
    state: Arc<ObservationState>,
}

struct ObservationState {
    handle: LockObserverHandle,
    executor: Arc<dyn MainThreadExecutor>,
    backend: Arc<dyn LockObservationBackend>,
    budget: Duration,
    next_generation: AtomicU64,
    /// The generation this installer most recently published, or zero.
    active: AtomicU64,
    /// The highest generation logically invalidated.
    invalidated: AtomicU64,
    /// The generation whose native registration this installer still owns.
    ///
    /// This lock-free mirror is authoritative for reads; `gate` serializes claims
    /// and ownership transitions.
    owned: AtomicU64,
    gate: Mutex<Option<u64>>,
}

/// A generation-local coalescing signal.
///
/// Every install owns its own slot, so a predecessor callback, take or panic can
/// never raise or clear a successor's hint even if its generation check races.
#[derive(Debug, Default)]
struct RefreshSignal {
    pending: AtomicBool,
}

impl RefreshSignal {
    /// Raises the hint. Returns whether it was newly raised.
    fn raise(&self) -> bool { !self.pending.swap(true, Ordering::SeqCst) }

    /// Takes and clears the hint.
    fn take(&self) -> bool { self.pending.swap(false, Ordering::SeqCst) }

    /// Clears the hint after a consumer panic.
    fn clear(&self) { self.pending.store(false, Ordering::SeqCst); }
}

impl LockObservationInstaller {
    /// Creates an installer over the real distributed notification observer.
    #[must_use]
    pub fn system(executor: Arc<dyn MainThreadExecutor>) -> Self {
        Self::new(executor, Arc::new(SystemLockObservationBackend))
    }

    /// Creates an installer over an injected backend.
    #[must_use]
    pub fn new(
        executor: Arc<dyn MainThreadExecutor>,
        backend: Arc<dyn LockObservationBackend>,
    ) -> Self {
        Self::with_budget(executor, backend, OBSERVATION_BUDGET)
    }

    /// Creates an installer with an explicit per-hop acknowledgement budget.
    ///
    /// Only tests and hosts with their own main-hop budget should need this.
    #[must_use]
    pub fn with_budget(
        executor: Arc<dyn MainThreadExecutor>,
        backend: Arc<dyn LockObservationBackend>,
        budget: Duration,
    ) -> Self {
        Self {
            state: Arc::new(ObservationState {
                handle: LockObserverHandle::allocate(),
                executor,
                backend,
                budget,
                next_generation: AtomicU64::new(0),
                active: AtomicU64::new(0),
                invalidated: AtomicU64::new(0),
                owned: AtomicU64::new(0),
                gate: Mutex::new(None),
            }),
        }
    }

    /// Installs selector observation and returns its live generation.
    ///
    /// The call returns only after native registration is acknowledged and the
    /// cancellation token is rechecked. A failed, cancelled or timed-out install
    /// invalidates exactly its generation and keeps cleanup owned; a later
    /// install retries that specific cleanup instead of reporting
    /// [`LockObservationError::AlreadyInstalled`] forever.
    ///
    /// # Errors
    /// Returns cancellation, a dispatch rejection, a typed unavailable service, a
    /// native failure or [`LockObservationError::TimedOut`].
    pub fn install(
        &self,
        cancellation: &CancellationToken,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<LockObservation, LockObservationError> {
        if cancellation.is_cancelled() {
            return Err(LockObservationError::Cancelled);
        }
        let generation = self.claim_generation(cancellation)?;
        if cancellation.is_cancelled() {
            return Err(self.abandon_install(generation, LockObservationError::Cancelled));
        }

        let refresh = Arc::new(RefreshSignal::default());
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let weak = Arc::downgrade(&self.state);
        let signal_refresh = Arc::clone(&refresh);
        let signal: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            if !state.is_published(generation) {
                return;
            }
            if !signal_refresh.raise() {
                return;
            }
            let notify = Arc::clone(&notify);
            if catch_unwind(AssertUnwindSafe(move || notify())).is_err() {
                // Only this generation's slot is reset; a successor's hint is
                // untouchable from a stale callback.
                signal_refresh.clear();
            }
        });

        let ack = Arc::new(Ack::new());
        let job_ack = Arc::clone(&ack);
        let job_state = Arc::clone(&self.state);
        let token = cancellation.clone();
        let deadline = Instant::now() + self.state.budget;
        let job = Box::new(move || {
            if Instant::now() >= deadline
                || token.is_cancelled()
                || !job_state.is_active(generation)
            {
                job_ack.complete(InstallAck::Aborted);
                return;
            }
            let backend = Arc::clone(&job_state.backend);
            match backend.install(job_state.handle, generation, signal, &token) {
                Ok(()) => {
                    if Instant::now() >= deadline
                        || token.is_cancelled()
                        || !job_state.is_active(generation)
                    {
                        if backend.remove(job_state.handle, generation).is_ok() {
                            job_state.release_ownership(generation);
                        }
                        job_ack.complete(InstallAck::Aborted);
                    } else {
                        job_ack.complete(InstallAck::Done(Ok(())));
                    }
                }
                Err(error) => {
                    if Instant::now() >= deadline
                        || token.is_cancelled()
                        || !job_state.is_active(generation)
                    {
                        if backend.remove(job_state.handle, generation).is_ok() {
                            job_state.release_ownership(generation);
                        }
                        job_ack.complete(InstallAck::Aborted);
                    } else {
                        job_ack.complete(InstallAck::Done(Err(error)));
                    }
                }
            }
        });
        if let Err(error) = self.state.executor.execute(job) {
            return Err(
                self.abandon_install(generation, LockObservationError::Dispatch(error.to_string()))
            );
        }
        let outcome =
            ack.wait_cancellable(self.state.budget, cancellation, OBSERVATION_CANCELLATION_QUANTUM);
        self.finish_install(generation, refresh, outcome, cancellation)
    }

    /// Invalidates and removes the generation this installer still owns.
    ///
    /// This is the cleanup path for a lifecycle that failed before an
    /// observation handle existed. It is idempotent and safe with nothing owned.
    /// Logical invalidation happens before native removal, so callbacks and
    /// queued work become inert immediately even if cleanup later times out. A
    /// failed or timed-out removal keeps ownership for a retry.
    ///
    /// `wait_cancellation` bounds only the acknowledgement wait. It is never
    /// consulted before enqueueing native cleanup, so an already-cancelled
    /// lifecycle token still dispatches the exact removal; the caller may instead
    /// observe [`LockObservationError::Cancelled`] promptly.
    ///
    /// # Errors
    /// Returns [`LockObservationError::Cancelled`] or
    /// [`LockObservationError::TimedOut`] when the acknowledgement did not
    /// arrive, a dispatch rejection or a native removal failure.
    pub fn stop(&self, wait_cancellation: &CancellationToken) -> Result<(), LockObservationError> {
        // Read ownership lock-free so an already-cancelled lifecycle token can
        // never skip required cleanup. The acknowledgement wait below is the
        // cancellable part.
        let active = self.state.active.load(Ordering::SeqCst);
        let owned = self.state.owned.load(Ordering::SeqCst);
        self.state.invalidate(active);
        self.state.deactivate(active);
        if owned == 0 {
            return Ok(());
        }
        let generation = owned;
        self.state.invalidate(generation);
        self.state.deactivate(generation);
        self.state.cleanup_and_wait(generation, wait_cancellation)
    }

    fn finish_install(
        &self,
        generation: u64,
        refresh: Arc<RefreshSignal>,
        outcome: AckWait<InstallAck>,
        cancellation: &CancellationToken,
    ) -> Result<LockObservation, LockObservationError> {
        let failure = match outcome {
            AckWait::Ready(InstallAck::Done(Ok(()))) => {
                if cancellation.is_cancelled() || !self.state.is_active(generation) {
                    return Err(self.abandon_install(generation, LockObservationError::Cancelled));
                }
                return Ok(LockObservation {
                    state: Arc::clone(&self.state),
                    generation,
                    refresh,
                    removed: AtomicBool::new(false),
                });
            }
            AckWait::Ready(InstallAck::Done(Err(error))) => error,
            AckWait::Ready(InstallAck::Aborted) | AckWait::Cancelled => {
                LockObservationError::Cancelled
            }
            AckWait::TimedOut => LockObservationError::TimedOut,
        };
        Err(self.abandon_install(generation, failure))
    }

    fn claim_generation(
        &self,
        wait_cancellation: &CancellationToken,
    ) -> Result<u64, LockObservationError> {
        loop {
            let pending = {
                let mut gate = self.state.gate_within(wait_cancellation)?;
                if self.state.active.load(Ordering::SeqCst) != 0 {
                    return Err(LockObservationError::AlreadyInstalled);
                }
                if let Some(owned) = *gate {
                    Some(owned)
                } else {
                    let generation = self.state.next_generation.fetch_add(1, Ordering::SeqCst) + 1;
                    *gate = Some(generation);
                    self.state.owned.store(generation, Ordering::SeqCst);
                    self.state.active.store(generation, Ordering::SeqCst);
                    drop(gate);
                    return Ok(generation);
                }
            };
            if let Some(owned) = pending {
                self.state.cleanup_and_wait(owned, wait_cancellation)?;
            }
        }
    }

    fn abandon_install(
        &self,
        generation: u64,
        error: LockObservationError,
    ) -> LockObservationError {
        self.state.invalidate(generation);
        // Clear publication now so a later install retries the owned cleanup
        // instead of reporting AlreadyInstalled while the queue is stalled.
        self.state.deactivate(generation);
        self.state.schedule_cleanup(generation);
        error
    }
}

impl ObservationState {
    fn is_invalidated(&self, generation: u64) -> bool {
        generation != 0 && generation <= self.invalidated.load(Ordering::SeqCst)
    }

    fn invalidate(&self, generation: u64) {
        if generation != 0 {
            self.invalidated.fetch_max(generation, Ordering::SeqCst);
        }
    }

    fn is_active(&self, generation: u64) -> bool {
        generation != 0
            && !self.is_invalidated(generation)
            && self.active.load(Ordering::SeqCst) == generation
    }

    fn is_published(&self, generation: u64) -> bool { self.is_active(generation) }

    fn gate_within(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<MutexGuard<'_, Option<u64>>, LockObservationError> {
        let deadline = Instant::now() + self.budget;
        loop {
            if cancellation.is_cancelled() {
                return Err(LockObservationError::Cancelled);
            }
            match self.gate.try_lock() {
                Ok(gate) => return Ok(gate),
                Err(TryLockError::Poisoned(poisoned)) => return Ok(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => {}
            }
            if Instant::now() >= deadline {
                return Err(LockObservationError::TimedOut);
            }
            std::thread::sleep(GATE_RETRY);
        }
    }

    /// Clears only the exact generation.
    ///
    /// A compare-exchange cannot overwrite a successor that published between the
    /// load and the store, which a load/store pair outside the gate could.
    fn deactivate(&self, generation: u64) {
        let _ = self.active.compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst);
    }

    fn release_ownership(&self, generation: u64) {
        let mut gate = lock(&self.gate);
        if *gate == Some(generation) {
            *gate = None;
            self.owned.store(0, Ordering::SeqCst);
            drop(gate);
        }
    }

    /// Dispatches one exact-generation removal and waits for a cancellable ack.
    ///
    /// The caller must invalidate before calling; the job deactivates even when
    /// native removal fails so a logically dead generation cannot block a later
    /// install. Ownership is released only on a confirmed removal.
    fn cleanup_and_wait(
        self: &Arc<Self>,
        generation: u64,
        wait_cancellation: &CancellationToken,
    ) -> Result<(), LockObservationError> {
        let ack = Arc::new(Ack::new());
        let job_ack = Arc::clone(&ack);
        let state = Arc::clone(self);
        let job = Box::new(move || {
            let result = state.backend.remove(state.handle, generation);
            if result.is_ok() {
                state.release_ownership(generation);
            }
            state.deactivate(generation);
            job_ack.complete(result);
        });
        if let Err(error) = self.executor.execute(job) {
            return Err(LockObservationError::Dispatch(error.to_string()));
        }
        match ack.wait_cancellable(self.budget, wait_cancellation, OBSERVATION_CANCELLATION_QUANTUM)
        {
            AckWait::Ready(result) => result,
            AckWait::TimedOut => Err(LockObservationError::TimedOut),
            AckWait::Cancelled => Err(LockObservationError::Cancelled),
        }
    }

    /// Schedules nonblocking exact cleanup for a logically dead generation.
    fn schedule_cleanup(self: &Arc<Self>, generation: u64) {
        let state = Arc::clone(self);
        let job = Box::new(move || {
            state.invalidate(generation);
            state.deactivate(generation);
            if state.backend.remove(state.handle, generation).is_ok() {
                state.release_ownership(generation);
            }
        });
        let _ = self.executor.execute(job);
    }
}

/// One live screen-lock observation generation.
///
/// It is `Send + Sync` and owns no native object. Keep it while refresh hints
/// are wanted. [`LockObservation::take_refresh`] returns and clears one
/// coalesced hint; the consumer probes on that hint rather than trusting the
/// notification payload.
pub struct LockObservation {
    state: Arc<ObservationState>,
    generation: u64,
    refresh: Arc<RefreshSignal>,
    removed: AtomicBool,
}

impl LockObservation {
    /// The exact generation this observation owns.
    #[must_use]
    pub const fn generation(&self) -> u64 { self.generation }

    /// Whether this generation is published and not invalidated.
    ///
    /// This reads atomics only and answers while a native send holds the gate.
    /// It is advisory: a concurrent stop can invalidate immediately after.
    #[must_use]
    pub fn is_active(&self) -> bool { self.state.is_published(self.generation) }

    /// Takes and clears this generation's coalesced refresh hint.
    ///
    /// Returns `false` after stop or invalidation. A stale take can only clear
    /// its own generation-local slot, never a successor's hint. Callers should
    /// take the hint before probing and re-take it after probing, because a
    /// notification can arrive while the probe runs.
    #[must_use]
    pub fn take_refresh(&self) -> bool {
        if !self.state.is_published(self.generation) {
            return false;
        }
        self.refresh.take()
    }

    /// Removes observation and waits for a bounded, cancellable acknowledgement.
    ///
    /// Logical invalidation and exact native cleanup dispatch happen first;
    /// callbacks and queued work are inert even if the main queue stalls.
    /// `wait_cancellation` only interrupts the acknowledgement wait: it never
    /// skips or cancels the enqueued native removal. A failed or timed-out removal
    /// keeps ownership for a retry. Repeated calls are safe.
    ///
    /// # Errors
    /// Returns [`LockObservationError::Cancelled`] or
    /// [`LockObservationError::TimedOut`] when the acknowledgement did not
    /// arrive, a dispatch rejection or a native removal failure.
    pub fn stop(&self, wait_cancellation: &CancellationToken) -> Result<(), LockObservationError> {
        if self.removed.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.state.invalidate(self.generation);
        self.state.deactivate(self.generation);
        self.state.cleanup_and_wait(self.generation, wait_cancellation).map(|()| {
            self.removed.store(true, Ordering::SeqCst);
        })
    }
}

impl Drop for LockObservation {
    /// Schedules exact cleanup without waiting for the main queue.
    ///
    /// Drop must not block for [`OBSERVATION_BUDGET`]; the queued job still names
    /// this exact generation and cannot remove a successor.
    fn drop(&mut self) {
        if self.removed.load(Ordering::SeqCst) {
            return;
        }
        self.state.invalidate(self.generation);
        self.state.deactivate(self.generation);
        self.state.schedule_cleanup(self.generation);
    }
}

impl fmt::Debug for LockObservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LockObservation")
            .field("generation", &self.generation)
            .field("active", &self.is_active())
            .field("removed", &self.removed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Drop for ObservationState {
    /// Best-effort final removal once every external owner is gone.
    ///
    /// The job is dispatched without blocking and names this exact generation. A
    /// rejected dispatch or an undrained main queue means the observer stays
    /// registered until that queue runs; no success is claimed.
    fn drop(&mut self) {
        let generation = self.owned.load(Ordering::SeqCst);
        if generation == 0 {
            return;
        }
        let executor = Arc::clone(&self.executor);
        let backend = Arc::clone(&self.backend);
        let handle = self.handle;
        let job = Box::new(move || {
            let _ = backend.remove(handle, generation);
        });
        let _ = executor.execute(job);
    }
}

enum AckState<T> {
    Pending,
    Ready(T),
    Closed,
}

struct Ack<T> {
    state: Mutex<AckState<T>>,
    signal: Condvar,
}

enum AckWait<T> {
    Ready(T),
    TimedOut,
    Cancelled,
}

impl<T> Ack<T> {
    const fn new() -> Self {
        Self {
            state: Mutex::new(AckState::Pending),
            signal: Condvar::new(),
        }
    }

    fn complete(&self, value: T) {
        let mut state = lock(&self.state);
        if matches!(&*state, AckState::Pending) {
            *state = AckState::Ready(value);
            drop(state);
            self.signal.notify_all();
        }
    }

    fn wait_cancellable(
        &self,
        budget: Duration,
        cancellation: &CancellationToken,
        quantum: Duration,
    ) -> AckWait<T> {
        let deadline = Instant::now() + budget;
        let mut state = lock(&self.state);
        loop {
            match &*state {
                AckState::Ready(_) => {
                    let AckState::Ready(value) = std::mem::replace(&mut *state, AckState::Closed)
                    else {
                        unreachable!("matched Ready");
                    };
                    return AckWait::Ready(value);
                }
                AckState::Closed => return AckWait::TimedOut,
                AckState::Pending => {}
            }
            if cancellation.is_cancelled() {
                return AckWait::Cancelled;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return AckWait::TimedOut;
            };
            let slice = remaining.min(quantum);
            let (next, _) =
                self.signal.wait_timeout(state, slice).unwrap_or_else(PoisonError::into_inner);
            state = next;
        }
    }
}

enum InstallAck {
    Done(Result<(), LockObservationError>),
    Aborted,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
struct ObserverIvars {
    wake: Arc<dyn Fn() + Send + Sync>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. The class owns only an
    // `Arc` and a generation; the notification center calls the selector with a
    // valid `NSNotification`. The class is `MainThreadOnly`, so it cannot move
    // off-main and no `Send`/`Sync` shim exists.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StacheKeepAwakeScreenLockObserver"]
    #[ivars = ObserverIvars]
    struct ScreenLockObserverObject;

    impl ScreenLockObserverObject {
        /// Ignores payload, contains panics (unwind-enabled builds only; release aborts) and only
        /// signals.
        #[unsafe(method(stacheKeepAwakeScreenLockChanged:))]
        fn screen_lock_changed(&self, _notification: &NSNotification) {
            let wake = Arc::clone(&self.ivars().wake);
            let _ = catch_unwind(AssertUnwindSafe(move || wake()));
        }
    }

    unsafe impl NSObjectProtocol for ScreenLockObserverObject {}
);

struct ObserverState {
    generation: u64,
    center: Retained<NSDistributedNotificationCenter>,
    observer: Retained<ScreenLockObserverObject>,
}

thread_local! {
    static OBSERVERS: RefCell<BTreeMap<LockObserverHandle, ObserverState>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// The real distributed-notification backend.
///
/// The value is stateless; native state lives in the main-thread-local observer
/// map. Only a main-thread call may register or remove.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemLockObservationBackend;

impl LockObservationBackend for SystemLockObservationBackend {
    fn install(
        &self,
        handle: LockObserverHandle,
        generation: u64,
        wake: Arc<dyn Fn() + Send + Sync>,
        cancellation: &CancellationToken,
    ) -> Result<(), LockObservationError> {
        let mtm = MainThreadMarker::new().ok_or(LockObservationError::NotMainThread)?;
        if cancellation.is_cancelled() {
            return Err(LockObservationError::Cancelled);
        }
        OBSERVERS.with(|cell| {
            let mut observers = cell.borrow_mut();
            if observers.contains_key(&handle) {
                return Err(LockObservationError::AlreadyInstalled);
            }
            let allocated = ScreenLockObserverObject::alloc(mtm).set_ivars(ObserverIvars { wake });
            // SAFETY: `alloc` returns an uninitialized instance of this exact
            // subclass, and `init` is NSObject's designated initializer. The
            // class has no `Drop`; the map retains the result.
            let observer: Retained<ScreenLockObserverObject> =
                unsafe { msg_send![super(allocated), init] };
            let center = NSDistributedNotificationCenter::defaultCenter();
            for name in [SCREEN_LOCK_NOTIFICATION, SCREEN_UNLOCK_NOTIFICATION] {
                let name = NSString::from_str(name);
                // SAFETY: the observer is a live selector target retained below;
                // the selector is implemented above with an `NSNotification`
                // argument; both names are immutable static strings; a nil object
                // means any sender. The center does not retain the observer, so
                // the map keeps it alive until exact-generation removal.
                unsafe {
                    center.addObserver_selector_name_object_suspensionBehavior(
                        AsRef::<AnyObject>::as_ref(&*observer),
                        sel!(stacheKeepAwakeScreenLockChanged:),
                        Some(&*name),
                        None,
                        NSNotificationSuspensionBehavior::Coalesce,
                    );
                }
            }
            observers.insert(handle, ObserverState { generation, center, observer });
            Ok(())
        })
    }

    fn remove(
        &self,
        handle: LockObserverHandle,
        generation: u64,
    ) -> Result<(), LockObservationError> {
        let _mtm = MainThreadMarker::new().ok_or(LockObservationError::NotMainThread)?;
        let removed = OBSERVERS.with(|cell| {
            let mut observers = cell.borrow_mut();
            match observers.get(&handle) {
                Some(state) if state.generation == generation => observers.remove(&handle),
                // A successor generation is never removed by predecessor cleanup.
                _ => None,
            }
        });
        let Some(state) = removed else {
            return Ok(());
        };
        for name in [SCREEN_LOCK_NOTIFICATION, SCREEN_UNLOCK_NOTIFICATION] {
            let name = NSString::from_str(name);
            // SAFETY: the retained observer came from this center generation and
            // is still live here; removal is idempotent for an installed
            // observer. The names match registration.
            unsafe {
                state.center.removeObserver_name_object(
                    AsRef::<AnyObject>::as_ref(&*state.observer),
                    Some(&*name),
                    None,
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::keep_awake::testing::{FakeLockObservationBackend, FakeNativeBarrier};

    fn installer(
        executor: Arc<ManualExecutorLike>,
        fake: &FakeLockObservationBackend,
        budget: Duration,
    ) -> LockObservationInstaller {
        LockObservationInstaller::with_budget(executor, Arc::new(fake.clone()), budget)
    }

    type ManualExecutorLike = crate::applications::testing::ManualExecutor;

    #[test]
    fn no_main_drain_times_out_without_registration_and_late_drain_is_inert() {
        let executor = Arc::new(ManualExecutorLike::new(false));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_millis(30));
        let token = CancellationToken::new();

        let error = installer.install(&token, || {}).expect_err("undrained install must time out");
        assert_eq!(error, LockObservationError::TimedOut);
        assert!(!fake.is_installed());
        assert!(fake.install_calls().is_empty());

        // The queued install observes the invalidated generation and must not
        // register; its ordered cleanup is then an inert removal.
        executor.drain();
        assert!(!fake.is_installed());
        assert!(fake.install_calls().is_empty());
        assert_eq!(fake.remove_calls().len(), 1);
    }

    #[test]
    fn cancellation_before_dispatch_never_touches_the_backend() {
        let executor = Arc::new(ManualExecutorLike::new(false));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_millis(30));
        let token = CancellationToken::new();
        token.cancel();

        assert_eq!(
            installer.install(&token, || {}).expect_err("cancelled"),
            LockObservationError::Cancelled
        );
        assert_eq!(executor.queued(), 0);
        assert!(fake.install_calls().is_empty());
        assert!(fake.remove_calls().is_empty());
    }

    #[test]
    fn cancellation_during_registration_removes_exactly_the_attempted_generation() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_secs(1));
        let barrier = fake.block_next_install_after_registration();
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let worker = thread::spawn(move || installer.install(&worker_token, || {}));

        assert!(barrier.wait_entered(Duration::from_secs(2)));
        token.cancel();
        assert!(barrier.release());
        assert_eq!(
            worker.join().expect("worker must not panic").expect_err("cancelled"),
            LockObservationError::Cancelled
        );
        assert!(!fake.is_installed());
        assert_eq!(fake.install_calls().len(), 1);
        assert!(fake.remove_calls().iter().all(|(_, generation)| *generation == 1));
        assert!(!fake.remove_calls().is_empty());
    }

    #[test]
    fn cancellation_before_registration_does_not_leave_an_observer() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_secs(1));
        let barrier = fake.block_next_install();
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let worker = thread::spawn(move || installer.install(&worker_token, || {}));

        assert!(barrier.wait_entered(Duration::from_secs(2)));
        token.cancel();
        assert!(barrier.release());
        assert_eq!(
            worker.join().expect("worker must not panic").expect_err("cancelled"),
            LockObservationError::Cancelled
        );
        assert!(!fake.is_installed());
        assert!(fake.install_calls().is_empty());
    }

    #[test]
    fn a_post_registration_timeout_never_leaves_a_live_observer_after_drain() {
        let executor = Arc::new(ManualExecutorLike::new(false));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_millis(40));
        let barrier = fake.block_next_install_after_registration();
        let token = CancellationToken::new();

        let (result_tx, result_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = installer.install(&token, || {});
            let _ = result_tx.send(result);
        });

        let stop = Arc::new(AtomicBool::new(false));
        let drain_executor = Arc::clone(&executor);
        let drain_stop = Arc::clone(&stop);
        let drain = thread::spawn(move || {
            while !drain_stop.load(Ordering::SeqCst) {
                let _ = drain_executor.drain();
                thread::sleep(Duration::from_millis(1));
            }
            let _ = drain_executor.drain();
        });

        assert!(barrier.wait_entered(Duration::from_secs(2)));
        let result = result_rx.recv_timeout(Duration::from_secs(2)).expect("install result");
        assert_eq!(
            result.expect_err("install must time out"),
            LockObservationError::TimedOut
        );
        assert!(barrier.release());

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && (fake.is_installed() || fake.remove_calls().is_empty()) {
            thread::sleep(Duration::from_millis(1));
        }
        stop.store(true, Ordering::SeqCst);
        drain.join().expect("drain must not panic");
        worker.join().expect("worker must not panic");

        assert!(!fake.is_installed());
        assert_eq!(fake.install_calls().len(), 1);
        assert_eq!(fake.remove_calls()[0].1, 1);
    }

    #[test]
    fn removing_twice_removes_native_observation_once() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_secs(1));
        let token = CancellationToken::new();
        let observation = installer.install(&token, || {}).expect("installed");

        let wait = CancellationToken::new();
        observation.stop(&wait).expect("first removal");
        observation.stop(&wait).expect("second removal is inert");
        installer.stop(&wait).expect("installer stop is inert");
        assert_eq!(fake.remove_calls().len(), 1);
        drop(observation);
        assert_eq!(fake.remove_calls().len(), 1);
        assert!(!fake.is_installed());
    }

    #[test]
    fn a_callback_after_invalidation_never_signals_even_if_native_cleanup_stalls() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_millis(30));
        let token = CancellationToken::new();
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&notified);
        let observation = installer
            .install(&token, move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("installed");

        executor.set_on_main(false);
        let wait = CancellationToken::new();
        assert_eq!(
            observation.stop(&wait).expect_err("cleanup cannot drain"),
            LockObservationError::TimedOut
        );
        assert!(fake.is_installed());
        assert!(fake.deliver());
        assert_eq!(notified.load(Ordering::SeqCst), 0);
        assert!(!observation.take_refresh());

        executor.drain();
        assert!(!fake.is_installed());
    }

    #[test]
    fn a_cancelled_removal_wait_returns_promptly_and_cleanup_stays_owned() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_secs(5));
        let token = CancellationToken::new();
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&notified);
        let observation = installer
            .install(&token, move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("installed");

        executor.set_on_main(false);
        let wait = CancellationToken::new();
        let worker_wait = wait.clone();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = result_tx.send(observation.stop(&worker_wait));
        });

        // Let the removal job queue and the waiter block, then cancel only the
        // acknowledgement wait. The removal itself must still be enqueued.
        thread::sleep(Duration::from_millis(20));
        wait.cancel();
        assert_eq!(
            result_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("cancelled wait must return promptly")
                .expect_err("wait was cancelled"),
            LockObservationError::Cancelled
        );
        assert!(fake.is_installed());
        assert!(fake.deliver());
        assert_eq!(notified.load(Ordering::SeqCst), 0);

        worker.join().expect("stop worker must not panic");
        executor.drain();
        assert!(!fake.is_installed());
        assert!(!fake.remove_calls().is_empty());
        assert!(fake.remove_calls().iter().all(|(_, generation)| *generation == 1));
    }

    #[test]
    fn a_pre_cancelled_wait_still_enqueues_the_exact_cleanup() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_secs(1));
        let token = CancellationToken::new();
        let observation = installer.install(&token, || {}).expect("installed");

        executor.set_on_main(false);
        let wait = CancellationToken::new();
        wait.cancel();
        assert_eq!(
            observation.stop(&wait).expect_err("cancelled wait"),
            LockObservationError::Cancelled
        );
        assert!(fake.is_installed());
        assert_eq!(executor.queued(), 1);

        executor.drain();
        assert!(!fake.is_installed());
        assert_eq!(fake.remove_calls()[0].1, 1);
    }

    #[test]
    fn a_timed_out_removal_barrier_never_reports_success() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        fake.set_barrier_timeout(Duration::from_millis(20));
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_secs(1));
        let token = CancellationToken::new();
        let observation = installer.install(&token, || {}).expect("installed");
        let _barrier = fake.block_next_remove();
        let wait = CancellationToken::new();

        assert_eq!(
            observation.stop(&wait).expect_err("removal barrier timed out"),
            LockObservationError::TimedOut
        );
        assert!(fake.is_installed());
        assert!(!observation.is_active());
        // The exact registration is still owned and a later pass removes it.
        observation.stop(&wait).expect("retry removes exactly generation one");
        assert!(!fake.is_installed());
    }

    #[test]
    fn dropping_an_observation_schedules_cleanup_without_waiting() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_secs(5));
        let token = CancellationToken::new();
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&notified);
        let observation = installer
            .install(&token, move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("installed");

        executor.set_on_main(false);
        let started = Instant::now();
        drop(observation);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(executor.queued(), 1);
        assert!(fake.is_installed());
        assert!(fake.deliver());
        assert_eq!(notified.load(Ordering::SeqCst), 0);

        executor.drain();
        assert!(!fake.is_installed());
        assert_eq!(fake.remove_calls()[0].1, 1);
    }

    #[test]
    fn install_pending_predecessor_cleanup_wait_observes_cancellation() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_millis(30));
        let first = CancellationToken::new();
        let observation = installer.install(&first, || {}).expect("first install");

        // Force a timed-out stop: the generation stays owned for the next install.
        executor.set_on_main(false);
        let stop_wait = CancellationToken::new();
        assert_eq!(
            observation.stop(&stop_wait).expect_err("cleanup cannot drain"),
            LockObservationError::TimedOut
        );
        assert!(fake.is_installed());

        let second = CancellationToken::new();
        let worker_wait = second.clone();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = result_tx.send(installer.install(&worker_wait, || {}));
        });
        thread::sleep(Duration::from_millis(10));
        second.cancel();
        assert_eq!(
            result_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("cancelled predecessor cleanup wait must return promptly")
                .expect_err("predecessor wait was cancelled"),
            LockObservationError::Cancelled
        );

        // Both queued cleanups name generation 1; the second is inert. The
        // registered observer is eventually gone and no successor was installed.
        executor.drain();
        assert!(!fake.is_installed());
        assert!(fake.remove_calls().iter().all(|(_, generation)| *generation == 1));
        assert!(fake.install_calls().iter().all(|(_, generation)| *generation == 1));
        worker.join().expect("install worker must not panic");
    }

    #[test]
    fn installer_restart_cleans_a_timed_out_predecessor_then_isolates_the_successor() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_millis(30));
        let first = CancellationToken::new();
        let first_calls = Arc::new(AtomicUsize::new(0));
        let first_counter = Arc::clone(&first_calls);
        let observation = installer
            .install(&first, move || {
                first_counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("first install");
        let stale_wake = fake.installed_wake().expect("captured first sink");

        executor.set_on_main(false);
        let stop_wait = CancellationToken::new();
        assert_eq!(
            observation.stop(&stop_wait).expect_err("cleanup cannot drain"),
            LockObservationError::TimedOut
        );
        assert!(fake.is_installed());

        // The next install retries the owned predecessor cleanup before it
        // publishes its own generation.
        executor.set_on_main(true);
        let second = CancellationToken::new();
        let second_calls = Arc::new(AtomicUsize::new(0));
        let second_counter = Arc::clone(&second_calls);
        let successor = installer
            .install(&second, move || {
                second_counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("successor install");
        assert_eq!(fake.installed_generation(installer.state.handle), Some(2));

        // The predecessor's own queued cleanup drains later and is inert.
        executor.drain();
        assert_eq!(fake.installed_generation(installer.state.handle), Some(2));

        assert!(fake.deliver());
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
        stale_wake();
        assert_eq!(first_calls.load(Ordering::SeqCst), 0);
        assert!(!observation.take_refresh());
        assert!(successor.take_refresh());
    }

    #[test]
    fn stale_generation_callback_and_take_cannot_clear_a_successor_hint() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_secs(1));
        let first = CancellationToken::new();
        let first_calls = Arc::new(AtomicUsize::new(0));
        let first_counter = Arc::clone(&first_calls);
        let observation = installer
            .install(&first, move || {
                first_counter.fetch_add(1, Ordering::SeqCst);
                panic!("stale consumer panic must be contained");
            })
            .expect("generation one");
        let stale_wake = fake.installed_wake().expect("captured first sink");

        // The old callback panics while its own generation is live; it only
        // clears its own slot and the next real delivery still arrives.
        assert!(fake.deliver());
        assert_eq!(first_calls.load(Ordering::SeqCst), 1);
        assert!(fake.deliver());
        assert_eq!(first_calls.load(Ordering::SeqCst), 2);

        let wait = CancellationToken::new();
        observation.stop(&wait).expect("first removal");
        let second = CancellationToken::new();
        let second_calls = Arc::new(AtomicUsize::new(0));
        let second_counter = Arc::clone(&second_calls);
        let successor = installer
            .install(&second, move || {
                second_counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("successor install");

        assert!(fake.deliver());
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
        // A captured predecessor callback must not raise or clear the successor.
        stale_wake();
        assert_eq!(first_calls.load(Ordering::SeqCst), 2);
        assert!(!observation.take_refresh());
        assert!(successor.take_refresh());
        assert!(!successor.take_refresh());

        // Same for a second delivery and a stale take after it.
        assert!(fake.deliver());
        stale_wake();
        assert_eq!(second_calls.load(Ordering::SeqCst), 2);
        assert!(!observation.take_refresh());
        assert!(successor.take_refresh());
    }

    #[test]
    fn in_flight_predecessor_panic_cannot_clear_a_successor_hint() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(executor, &fake, Duration::from_secs(1));
        let barrier = Arc::new(FakeNativeBarrier::default());
        let callback_barrier = Arc::clone(&barrier);
        let token = CancellationToken::new();
        let predecessor = installer
            .install(&token, move || {
                assert_eq!(
                    callback_barrier.park(None, Duration::from_secs(2)),
                    crate::keep_awake::testing::BarrierWait::Released
                );
                panic!("contained predecessor panic after successor publication");
            })
            .expect("predecessor");
        let stale_wake = fake.installed_wake().expect("captured predecessor callback");

        thread::scope(|scope| {
            // Pause after the predecessor passed its generation check and raised
            // its own hint, not merely before invoking an already-stale sink.
            let callback = scope.spawn(move || stale_wake());
            assert!(barrier.wait_entered(Duration::from_secs(2)));
            let stale_refresh = Arc::clone(&predecessor.refresh);
            predecessor.stop(&CancellationToken::new()).expect("remove predecessor");
            let successor = installer.install(&token, || {}).expect("successor");
            assert!(fake.deliver());

            // Simulate a reader paused after the same predecessor generation
            // check: its exact slot cannot consume the successor's hint.
            assert!(stale_refresh.take());
            assert!(barrier.release());
            callback.join().expect("callback panic stays contained");
            assert!(successor.take_refresh());
            assert!(!successor.take_refresh());
        });
    }

    #[test]
    fn unavailable_is_typed_and_creates_no_observation() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        fake.fail_next_install(LockObservationError::Unavailable(
            "no distributed service".into(),
        ));
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_secs(1));
        let token = CancellationToken::new();

        assert_eq!(
            installer.install(&token, || {}).expect_err("unavailable"),
            LockObservationError::Unavailable("no distributed service".into())
        );
        assert!(!fake.is_installed());
        // The failed generation is invalidated; its cleanup passed through the
        // backend even though registration never happened.
        assert_eq!(fake.remove_calls().len(), 1);
    }

    #[test]
    fn a_predecessor_removal_never_touches_a_successor_generation() {
        let backend = FakeLockObservationBackend::new();
        let handle = LockObserverHandle::allocate();
        let token = CancellationToken::new();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});

        backend.install(handle, 1, Arc::clone(&wake), &token).expect("first");
        backend.install(handle, 2, wake, &token).expect("successor");
        backend.remove(handle, 1).expect("stale removal is inert");
        assert_eq!(backend.installed_generation(handle), Some(2));
        backend.remove(handle, 2).expect("exact removal");
        assert_eq!(backend.installed_generation(handle), None);
    }

    #[test]
    fn refresh_hints_coalesce_until_taken() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_secs(1));
        let token = CancellationToken::new();
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&notified);
        let observation = installer
            .install(&token, move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect("installed");

        assert!(fake.deliver());
        assert!(fake.deliver());
        assert_eq!(notified.load(Ordering::SeqCst), 1);
        assert!(observation.take_refresh());
        assert!(!observation.take_refresh());
        assert!(fake.deliver());
        assert_eq!(notified.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_panicking_consumer_is_contained_and_later_hints_still_arrive() {
        let executor = Arc::new(ManualExecutorLike::new(true));
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_secs(1));
        let token = CancellationToken::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let observation = installer
            .install(&token, move || {
                counter.fetch_add(1, Ordering::SeqCst);
                panic!("consumer panic must not cross the native boundary");
            })
            .expect("installed");

        assert!(fake.deliver());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // The panic resets the coalescing flag so a later hint can wake again.
        assert!(fake.deliver());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(!observation.take_refresh());
    }

    #[test]
    fn dispatch_rejection_is_reported_without_native_registration() {
        let executor = Arc::new(ManualExecutorLike::new(false));
        executor.reject(true);
        let fake = FakeLockObservationBackend::new();
        let installer = installer(Arc::clone(&executor), &fake, Duration::from_millis(30));
        let token = CancellationToken::new();

        let error = installer.install(&token, || {}).expect_err("dispatch rejected");
        assert!(matches!(error, LockObservationError::Dispatch(_)));
        assert!(fake.install_calls().is_empty());
    }

    #[test]
    #[allow(clippy::items_after_statements)]
    fn opaque_handles_are_send_and_sync_but_the_selector_object_is_neither() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LockObservation>();
        assert_send_sync::<LockObservationInstaller>();
        assert_send_sync::<SystemLockObservationBackend>();
        assert_send_sync::<LockObserverHandle>();
        assert_send_sync::<FakeNativeBarrier>();

        // Compile-time negative assertion: if a MainThreadOnly selector target
        // ever became Send (for example through an unsafe shim), this becomes an
        // ambiguous impl and fails to compile.
        trait AmbiguousIfSend<A> {
            fn marker() {}
        }
        impl<T: ?Sized> AmbiguousIfSend<()> for T {}
        impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
        let _ = <Retained<ScreenLockObserverObject> as AmbiguousIfSend<_>>::marker;

        trait AmbiguousIfSync<A> {
            fn marker() {}
        }
        impl<T: ?Sized> AmbiguousIfSync<()> for T {}
        impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}
        let _ = <Retained<ScreenLockObserverObject> as AmbiguousIfSync<_>>::marker;

        // Installed objc2-foundation 0.3.2 does NOT mark the center
        // `MainThreadOnly`: `Retained<NSDistributedNotificationCenter>` is
        // `Send + Sync`. The installed map entry is still unshippable because it
        // owns the `!Send` selector target, proven above and here.
        assert_send_sync::<Retained<NSDistributedNotificationCenter>>();
        let _ = <ObserverState as AmbiguousIfSend<_>>::marker;
        let _ = <ObserverState as AmbiguousIfSync<_>>::marker;
    }
}
