//! The `Send + Sync` handle to the main-owned hotkey store.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_sdk::input::{InputService, ReservationOwner};
use stache_sdk::{CancellationToken, MainThreadExecutor};

use super::native::NativeFactory;
use super::store::{Job, Store, remove_store, with_store};
use super::types::{
    CommitReport, HotkeyError, HotkeyRequest, HotkeySink, StagedId, StagedReport, Status,
    StopReport,
};
use crate::applications::{ACKNOWLEDGEMENT_BUDGET, CANCELLATION_QUANTUM};

static NEXT_CONTROL: AtomicU64 = AtomicU64::new(1);

/// How long one acknowledgement may take and how often a wait rechecks
/// cancellation. The defaults are the shared 2 s / 10 ms bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// The acknowledgement budget per main-thread hop.
    pub budget: Duration,
    /// The longest a wait sleeps before rechecking cancellation.
    pub quantum: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            budget: ACKNOWLEDGEMENT_BUDGET,
            quantum: CANCELLATION_QUANTUM,
        }
    }
}

/// State shared by the control, the main-local store and the callback. Only
/// `Send + Sync` values: no native object is reachable from here.
pub(super) struct Shared {
    pub id: u64,
    pub executor: Arc<dyn MainThreadExecutor>,
    pub factory: NativeFactory,
    pub service: Arc<InputService>,
    pub owner: ReservationOwner,
    pub sink: Arc<dyn HotkeySink>,
    pub timing: Timing,
    /// The current generation; bumped by every stop.
    pub generation: AtomicU64,
    /// Registrations of generations up to this one never dispatch. Written
    /// off-main, so removal inhibits dispatch before any main-thread hop.
    pub inhibited: AtomicU64,
    pub closed: AtomicBool,
    pub refresh_pending: AtomicBool,
}

impl Shared {
    /// Queues one coalesced layout refresh. Safe from any thread; it only
    /// enqueues. Hints that arrive while one is queued are absorbed.
    pub(super) fn request_refresh(self: &Arc<Self>, generation: u64) {
        if self.closed.load(Ordering::Acquire) || self.refresh_pending.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let shared = Arc::clone(self);
        let job = Box::new(move || {
            // Cleared first so a hint that races this job queues a new one.
            shared.refresh_pending.store(false, Ordering::Release);
            if shared.closed.load(Ordering::Acquire)
                || shared.generation.load(Ordering::Acquire) != generation
            {
                return;
            }
            let job = Job::internal(&shared, generation);
            run_store(&shared, |store| store.refresh(&job));
        });
        if self.executor.execute(job).is_err() {
            self.refresh_pending.store(false, Ordering::Release);
        }
    }
}

/// Runs `f` on the store and then performs any refresh it asked for, outside
/// the store borrow.
pub(super) fn run_store<T>(shared: &Arc<Shared>, f: impl FnOnce(&mut Store) -> T) -> T {
    let (value, refresh) = with_store(shared, |store| {
        let value = f(store);
        (value, store.take_refresh_request())
    });
    if let Some(generation) = refresh {
        shared.request_refresh(generation);
    }
    value
}

enum AckState<T> {
    Pending,
    Ready(T),
    Closed,
}

enum AckWait<T> {
    Ready(T),
    TimedOut,
    Cancelled,
}

struct Ack<T> {
    state: Mutex<AckState<T>>,
    signal: Condvar,
}

impl<T> Ack<T> {
    const fn new() -> Self {
        Self {
            state: Mutex::new(AckState::Pending),
            signal: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, AckState<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn complete(&self, value: T) {
        let mut state = self.lock();
        if matches!(&*state, AckState::Pending) {
            *state = AckState::Ready(value);
            drop(state);
            self.signal.notify_all();
        }
    }

    /// Waits for the acknowledgement, rechecking cancellation every `quantum`.
    fn wait(&self, timing: Timing, cancel: &CancellationToken) -> AckWait<T> {
        let deadline = Instant::now() + timing.budget;
        let mut state = self.lock();
        loop {
            // A finished job wins over cancellation: its result is the truth
            // about native state (for example owed cleanup after a rollback).
            match std::mem::replace(&mut *state, AckState::Pending) {
                AckState::Ready(value) => {
                    *state = AckState::Closed;
                    return AckWait::Ready(value);
                }
                AckState::Closed => {
                    *state = AckState::Closed;
                    return AckWait::TimedOut;
                }
                AckState::Pending => {}
            }
            if cancel.is_cancelled() {
                return AckWait::Cancelled;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return AckWait::TimedOut;
            };
            let slice = remaining.min(timing.quantum);
            state =
                self.signal.wait_timeout(state, slice).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

/// The off-main handle to the main-owned Carbon/TIS store.
///
/// Every operation hops to the main thread, waits at most the acknowledgement
/// budget (rechecking cancellation each quantum) and returns plain values. A wait
/// that gives up marks the queued job abandoned, so late work rechecks that flag
/// and the caller's cancellation before it mutates anything native.
///
/// No Carbon or `TIS` object is ever reachable from this type.
pub struct HotkeyControl {
    shared: Arc<Shared>,
}

impl HotkeyControl {
    /// Creates a control over a native factory.
    ///
    /// Nothing native exists yet: the factory runs on the main thread the first
    /// time a job reaches the store, and the Carbon handler is installed lazily
    /// by the first commit that registers a chord.
    #[must_use]
    pub fn new(
        executor: Arc<dyn MainThreadExecutor>,
        native: NativeFactory,
        service: Arc<InputService>,
        owner: ReservationOwner,
        sink: Arc<dyn HotkeySink>,
    ) -> Self {
        Self::with_timing(executor, native, service, owner, sink, Timing::default())
    }

    /// Like [`Self::new`] with explicit acknowledgement bounds.
    #[must_use]
    pub fn with_timing(
        executor: Arc<dyn MainThreadExecutor>,
        native: NativeFactory,
        service: Arc<InputService>,
        owner: ReservationOwner,
        sink: Arc<dyn HotkeySink>,
        timing: Timing,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                id: NEXT_CONTROL.fetch_add(1, Ordering::Relaxed),
                executor,
                factory: native,
                service,
                owner,
                sink,
                timing,
                generation: AtomicU64::new(1),
                inhibited: AtomicU64::new(0),
                closed: AtomicBool::new(false),
                refresh_pending: AtomicBool::new(false),
            }),
        }
    }

    /// Creates a control over the real Carbon and `TIS` layer.
    ///
    /// Constructing it registers nothing; no production code calls this yet.
    #[must_use]
    pub fn system(
        executor: Arc<dyn MainThreadExecutor>,
        service: Arc<InputService>,
        owner: ReservationOwner,
        sink: Arc<dyn HotkeySink>,
    ) -> Self {
        let factory: NativeFactory = Arc::new(|| Box::new(super::ffi::SystemNative::new()));
        Self::new(executor, factory, service, owner, sink)
    }

    /// Stages `requests` as an inert candidate: keys are mapped, `§` is resolved
    /// against the current layout and collisions are settled. Nothing is
    /// registered natively. A later [`Self::prepare`] replaces an uncommitted one.
    ///
    /// # Errors
    /// [`HotkeyError`] for cancellation, a missed acknowledgement or a closed control.
    pub fn prepare(
        &self,
        cancel: &CancellationToken,
        requests: Vec<HotkeyRequest>,
    ) -> Result<StagedReport, HotkeyError> {
        self.call(cancel, true, move |store, job| store.prepare(job, requests))?
    }

    /// Commits a staged candidate: transfers unchanged registrations, registers
    /// new chords one by one, publishes reservations and swaps routes at one
    /// boundary, then unregisters removed chords.
    ///
    /// # Errors
    /// [`HotkeyError::Stale`] when generation, revision or layout moved on, plus
    /// the transport errors of [`Self::prepare`]. A failed commit leaves the
    /// last-good set untouched.
    pub fn commit(
        &self,
        cancel: &CancellationToken,
        staged: StagedId,
    ) -> Result<CommitReport, HotkeyError> {
        self.call(cancel, true, move |store, job| store.commit(job, staged))?
    }

    /// [`Self::prepare`] then [`Self::commit`] in one main-thread hop.
    ///
    /// # Errors
    /// As [`Self::commit`].
    pub fn apply(
        &self,
        cancel: &CancellationToken,
        requests: Vec<HotkeyRequest>,
    ) -> Result<CommitReport, HotkeyError> {
        self.call(cancel, true, move |store, job| store.apply(job, requests))?
    }

    /// Stops: every registration stops dispatching immediately (before any
    /// main-thread hop), then the main owner unregisters, removes reservations,
    /// the layout observer and the handler. Failed removals stay as inert guards
    /// and are reported as pending cleanup.
    ///
    /// # Errors
    /// Transport errors of [`Self::prepare`]; the logical stop has still happened.
    pub fn stop(&self, cancel: &CancellationToken) -> Result<StopReport, HotkeyError> {
        let old = self.shared.generation.fetch_add(1, Ordering::AcqRel);
        self.shared.inhibited.fetch_max(old, Ordering::AcqRel);
        self.call(cancel, false, Store::stop)
    }

    /// Retries owed cleanup and reports the store.
    ///
    /// # Errors
    /// Transport errors of [`Self::prepare`].
    pub fn status(&self, cancel: &CancellationToken) -> Result<Status, HotkeyError> {
        self.call(cancel, false, move |store, job| store.status(job))
    }

    /// Retries unregistering inert guards.
    ///
    /// # Errors
    /// Transport errors of [`Self::prepare`].
    pub fn retry_cleanup(&self, cancel: &CancellationToken) -> Result<usize, HotkeyError> {
        self.call(cancel, false, Store::retry_cleanup)
    }

    /// Queues a coalesced layout refresh. Exactly what the input-source observer
    /// calls: it only enqueues, with no query, registration or wait.
    pub fn request_layout_refresh(&self) {
        let generation = self.shared.generation.load(Ordering::Acquire);
        self.shared.request_refresh(generation);
    }

    fn call<T: Send + 'static>(
        &self,
        cancel: &CancellationToken,
        refuse_if_cancelled: bool,
        work: impl FnOnce(&mut Store, &Job) -> T + Send + 'static,
    ) -> Result<T, HotkeyError> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(HotkeyError::Closed);
        }
        if refuse_if_cancelled && cancel.is_cancelled() {
            return Err(HotkeyError::Cancelled);
        }
        let abandoned = Arc::new(AtomicBool::new(false));
        let job = Job::external(
            &self.shared,
            self.shared.generation.load(Ordering::Acquire),
            cancel.clone(),
            Arc::clone(&abandoned),
        );
        let ack = Arc::new(Ack::new());
        let (job_ack, shared) = (Arc::clone(&ack), Arc::clone(&self.shared));
        let main_job = Box::new(move || {
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                run_store(&shared, |store| work(store, &job))
            }));
            job_ack.complete(outcome.map_err(|_| HotkeyError::Panicked));
        });
        self.shared
            .executor
            .execute(main_job)
            .map_err(|error| HotkeyError::Dispatch(error.to_string()))?;
        match ack.wait(self.shared.timing, cancel) {
            AckWait::Ready(outcome) => outcome,
            AckWait::TimedOut => {
                abandoned.store(true, Ordering::Release);
                Err(HotkeyError::TimedOut)
            }
            AckWait::Cancelled => {
                abandoned.store(true, Ordering::Release);
                Err(HotkeyError::Cancelled)
            }
        }
    }
}

impl Drop for HotkeyControl {
    /// Dropping inhibits dispatch at once and queues the final main-thread
    /// teardown. It does not wait, so it is not proof of cleanup: use
    /// [`HotkeyControl::stop`] and check the report.
    fn drop(&mut self) {
        let generation = self.shared.generation.load(Ordering::Acquire);
        self.shared.inhibited.fetch_max(generation, Ordering::AcqRel);
        self.shared.closed.store(true, Ordering::Release);
        let shared = Arc::clone(&self.shared);
        let _ = self.shared.executor.execute(Box::new(move || remove_store(&shared)));
    }
}
