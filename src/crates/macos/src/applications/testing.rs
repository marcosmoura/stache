//! Deterministic doubles for application observation.
//!
//! [`ManualExecutor`] lets a test choose whether a job runs inline or waits in a
//! queue, and [`FakeApplicationBackend`] records every native decision without
//! touching `AppKit`. Both plug into the real [`ApplicationControl`] generation,
//! acknowledgement and commit machinery.

// The fake holds its in-memory state mutexes while building a result. There is
// no contention to reduce, and splitting the guards would obscure the double.
#![allow(clippy::significant_drop_tightening)]

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use stache_sdk::{MainThreadExecutor, ThreadError};

use super::activation::{ActivationAuthorization, ActivationOutcome};
use super::display::{DisplayHint, ScreenRecord, WindowRecord, select_display};
use super::frontmost::{
    ActivationSink, FrontmostApplication, FrontmostPreflight, defer_activation, preflight_on_main,
};
use super::graceful::{GracefulOutcome, PressAuthorization};
use super::lifecycle::{LifecycleEvent, LifecycleKind, LifecycleSink, defer_lifecycle};
use super::visibility::{VisibilityAuthorization, VisibilityOperation, VisibilityOutcome};
use super::{
    ApplicationBackend, ApplicationError, ApplicationId, BackendHandle, IssuancePermit,
    NativeOpenCompletion, OpenRequest, TerminateOutcome, defer_notification,
};

/// A main-thread executor a test drives explicitly.
pub struct ManualExecutor {
    state: Mutex<ExecutorState>,
}

struct ExecutorState {
    on_main: bool,
    reject: bool,
    queued: Vec<Box<dyn FnOnce() + Send>>,
}

impl ManualExecutor {
    /// Creates an executor that starts on or off the main thread.
    #[must_use]
    pub fn new(on_main: bool) -> Self {
        Self {
            state: Mutex::new(ExecutorState {
                on_main,
                reject: false,
                queued: Vec::new(),
            }),
        }
    }

    /// Changes whether new jobs run inline.
    pub fn set_on_main(&self, on_main: bool) { lock(&self.state).on_main = on_main; }

    /// Makes new jobs fail with [`ThreadError::GroupStopped`].
    pub fn reject(&self, reject: bool) { lock(&self.state).reject = reject; }

    /// The number of jobs waiting for the main thread.
    #[must_use]
    pub fn queued(&self) -> usize { lock(&self.state).queued.len() }

    /// Queues a job without running it, even when this executor runs jobs inline.
    ///
    /// This mirrors the system backend's notification deferral, which always
    /// enqueues on the main dispatch queue.
    pub fn enqueue(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        lock(&self.state).queued.push(job);
    }

    /// Queues a job like [`ManualExecutor::enqueue`], but fails when the
    /// executor rejects work (a rejected main-queue enqueue).
    ///
    /// # Errors
    /// Returns [`ThreadError::GroupStopped`] while rejecting.
    pub fn try_enqueue(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        let mut state = lock(&self.state);
        if state.reject {
            return Err(ThreadError::GroupStopped);
        }
        state.queued.push(job);
        Ok(())
    }

    /// Runs every queued job in order and returns how many ran.
    pub fn drain(&self) -> usize {
        let jobs = std::mem::take(&mut lock(&self.state).queued);
        let count = jobs.len();
        for job in jobs {
            job();
        }
        count
    }

    /// Runs one queued job and returns whether one was present.
    pub fn drain_one(&self) -> bool {
        let job = {
            let mut state = lock(&self.state);
            if state.queued.is_empty() {
                return false;
            }
            state.queued.remove(0)
        };
        job();
        true
    }
}

impl MainThreadExecutor for ManualExecutor {
    fn is_main_thread(&self) -> bool { lock(&self.state).on_main }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        let mut state = lock(&self.state);
        if state.reject {
            return Err(ThreadError::GroupStopped);
        }
        if state.on_main {
            drop(state);
            job();
        } else {
            state.queued.push(job);
        }
        Ok(())
    }
}

/// A one-shot rendezvous a test uses to hold a fake native call open.
///
/// The [`FakeApplicationBackend`] `block_next_*` helpers return one. The native
/// call parks inside [`FakeBarrier::enter`] until the test calls
/// [`FakeBarrier::release`].
#[derive(Debug, Default)]
pub struct FakeBarrier {
    state: Mutex<BarrierState>,
    signal: Condvar,
}

#[derive(Debug, Default)]
struct BarrierState {
    entered: bool,
    released: bool,
}

impl FakeBarrier {
    /// Waits until the native call has entered the barrier.
    #[must_use]
    pub fn wait_entered(&self, timeout: Duration) -> bool {
        let state = lock(&self.state);
        let (state, _) = self
            .signal
            .wait_timeout_while(state, timeout, |state| !state.entered)
            .unwrap_or_else(PoisonError::into_inner);
        state.entered
    }

    /// Releases the parked native call. Returns whether it had entered.
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

    fn enter(&self) {
        let mut state = lock(&self.state);
        state.entered = true;
        self.signal.notify_all();
        while !state.released {
            state = self.signal.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

type FakeDelivery = (Arc<dyn Fn(ApplicationId) + Send + Sync>, ApplicationId);

/// A stable key for one fake running application.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FakeAppKey(u64);

/// One notification delivery captured from an installed observation.
///
/// It mirrors a native block that has already been copied by the notification
/// center: delivering it after removal must be ignored by the same generation
/// gate the native block uses, and must not intern or pollute a successor.
pub struct FakeLaunch {
    backend: Arc<FakeState>,
    handle: BackendHandle,
    generation: u64,
    key: FakeAppKey,
    notify: Arc<dyn Fn(ApplicationId) + Send + Sync>,
}

impl FakeLaunch {
    /// Delivers the captured notification. Returns whether the generation gate
    /// accepted it and the sink ran.
    #[must_use]
    pub fn deliver(&self) -> bool {
        let applications = lock(&self.backend.applications);
        let mut observations = lock(&self.backend.observations);
        let Some(observation) = observations.get_mut(&self.handle) else {
            return false;
        };
        if observation.generation != self.generation {
            return false;
        }
        let id = intern(self.handle, observation, &applications, self.key);
        let notify = Arc::clone(&self.notify);
        drop(observations);
        notify(id);
        true
    }
}

/// A deterministic [`ApplicationBackend`].
#[derive(Clone, Default)]
pub struct FakeApplicationBackend {
    state: Arc<FakeState>,
}

#[derive(Default)]
struct FakeState {
    observations: Mutex<BTreeMap<BackendHandle, FakeObservation>>,
    applications: Mutex<BTreeMap<u64, FakeApplication>>,
    next_application: AtomicU64,
    fail_observe: AtomicBool,
    fail_after_observe: AtomicBool,
    fail_unobserve: AtomicUsize,
    fail_query: AtomicBool,
    fail_open: AtomicBool,
    refuse_terminate: AtomicBool,
    synchronous_open_completion: AtomicBool,
    terminate_requests: Mutex<Vec<FakeAppKey>>,
    open_requests: Mutex<Vec<PathBuf>>,
    pending_opens: Mutex<Vec<Option<NativeOpenCompletion>>>,
    bundle_ids_by_path: Mutex<BTreeMap<PathBuf, Option<String>>>,
    paths_by_bundle_id: Mutex<BTreeMap<String, Option<PathBuf>>>,
    observe_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    open_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    terminate_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    terminate_issuance_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    terminate_after_issue_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    list_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    is_running_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    bundle_identifier_at_path_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    application_path_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    list_requests: Mutex<Vec<String>>,
    terminate_attempts: AtomicUsize,
    is_running_calls: AtomicUsize,
    notification_queue: Mutex<Option<Arc<ManualExecutor>>>,
    launch_during_terminate_send: Mutex<Option<FakeAppKey>>,
    launch_during_observe: Mutex<Option<FakeAppKey>>,
    frontmost: Mutex<Option<FakeAppKey>>,
    current_process: Mutex<Option<FakeAppKey>>,
    activations: Mutex<BTreeMap<BackendHandle, FakeActivation>>,
    fail_activation_observe: AtomicBool,
    fail_activation_unobserve: AtomicUsize,
    activation_removals: AtomicUsize,
    drop_next_activation: AtomicBool,
    fail_frontmost: AtomicBool,
    frontmost_queries: AtomicUsize,
    frontmost_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    graceful_requests: Mutex<Vec<FakeAppKey>>,
    graceful_attempts: AtomicUsize,
    refuse_graceful: AtomicBool,
    graceful_preflight_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    graceful_commit_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    graceful_after_issue_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    graceful_issuance_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    exact_activation_requests: Mutex<Vec<FakeAppKey>>,
    exact_activation_attempts: AtomicUsize,
    refuse_exact_activation: AtomicBool,
    exact_activation_preflight_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    exact_activation_commit_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    exact_activation_issuance_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    exact_activation_after_issue_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    display: Mutex<FakeDisplay>,
    display_queries: AtomicUsize,
    lifecycles: Mutex<BTreeMap<BackendHandle, FakeLifecycle>>,
    fail_lifecycle_observe: AtomicBool,
    fail_lifecycle_unobserve: AtomicUsize,
    lifecycle_removals: AtomicUsize,
    visibility_requests: Mutex<Vec<(FakeAppKey, VisibilityOperation)>>,
    visibility_attempts: AtomicUsize,
    refuse_visibility: AtomicBool,
    visibility_preflight_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    visibility_commit_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    visibility_issuance_barrier: Mutex<Option<Arc<FakeBarrier>>>,
    visibility_after_issue_barrier: Mutex<Option<Arc<FakeBarrier>>>,
}

struct FakeLifecycle {
    generation: u64,
    sink: LifecycleSink,
}

struct FakeActivation {
    generation: u64,
    sink: ActivationSink,
}

struct FakeDisplay {
    windows: Option<Vec<WindowRecord>>,
    screens: Vec<ScreenRecord>,
}

impl Default for FakeDisplay {
    fn default() -> Self {
        Self {
            windows: Some(Vec::new()),
            screens: Vec::new(),
        }
    }
}

struct FakeObservation {
    generation: u64,
    next_token: u64,
    notify: Arc<dyn Fn(ApplicationId) + Send + Sync>,
    applications: BTreeMap<u64, FakeAppKey>,
    /// Pruned terminated instances, oldest first (bounded like the system store).
    tombstones: VecDeque<(u64, FakeAppKey)>,
}

struct FakeApplication {
    bundle_id: Option<String>,
    name: Option<String>,
    pid: i32,
    alive: bool,
    hidden: bool,
    regular: bool,
}

impl FakeApplicationBackend {
    /// Creates a backend with no applications and no failures.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Adds one running application and returns its stable fake key.
    #[must_use]
    pub fn add_running(&self, bundle_id: &str) -> FakeAppKey {
        self.add_running_with_pid(bundle_id, 0)
    }

    /// Adds one running application with an explicit PID.
    #[must_use]
    pub fn add_running_with_pid(&self, bundle_id: &str, pid: i32) -> FakeAppKey {
        let key = FakeAppKey(self.state.next_application.fetch_add(1, Ordering::SeqCst) + 1);
        lock(&self.state.applications).insert(key.0, FakeApplication {
            bundle_id: Some(bundle_id.to_owned()),
            name: None,
            pid,
            alive: true,
            hidden: false,
            regular: true,
        });
        key
    }

    /// Marks an application as an accessory or background (agent) one, as an
    /// `NSApplicationActivationPolicy` other than regular.
    pub fn set_regular(&self, key: FakeAppKey, regular: bool) {
        if let Some(application) = lock(&self.state.applications).get_mut(&key.0) {
            application.regular = regular;
        }
    }

    /// Marks an application as exited.
    pub fn terminate(&self, key: FakeAppKey) {
        if let Some(application) = lock(&self.state.applications).get_mut(&key.0) {
            application.alive = false;
        }
    }

    /// Removes an application's bundle identifier, modelling native data that
    /// cannot identify the instance.
    pub fn forget_bundle_id(&self, key: FakeAppKey) {
        if let Some(application) = lock(&self.state.applications).get_mut(&key.0) {
            application.bundle_id = None;
        }
    }

    /// Makes the next registration fail before installing anything.
    pub fn fail_next_observe(&self) { self.state.fail_observe.store(true, Ordering::SeqCst); }

    /// Makes the next registration install and then report an error.
    pub fn fail_next_observe_after_registration(&self) {
        self.state.fail_after_observe.store(true, Ordering::SeqCst);
    }

    /// Makes the next removal fail and leave observation installed.
    pub fn fail_next_unobserve(&self) { self.state.fail_unobserve.store(1, Ordering::SeqCst); }

    /// Makes the next `count` removals fail and leave observation installed.
    ///
    /// This covers a failed install whose own rollback and the caller's
    /// acknowledged cleanup both fail.
    pub fn fail_unobserves(&self, count: usize) {
        self.state.fail_unobserve.store(count, Ordering::SeqCst);
    }

    /// Makes the next running-instance query fail before it reads the store.
    ///
    /// This covers [`ApplicationBackend::running_applications`],
    /// [`ApplicationBackend::is_running`], [`ApplicationBackend::bundle_identifier`],
    /// [`ApplicationBackend::bundle_identifier_at_path`] and
    /// [`ApplicationBackend::application_path`], including the identity
    /// preflight inside [`ApplicationBackend::request_terminate`]. The result is
    /// [`ApplicationError::Unavailable`], so a failure is distinguishable from
    /// an empty, not-running or unknown answer.
    pub fn fail_next_query(&self) { self.state.fail_query.store(true, Ordering::SeqCst); }

    /// Registers one installed application so both resolution queries agree.
    pub fn add_installed(&self, path: impl Into<PathBuf>, bundle_id: &str) {
        let path = path.into();
        lock(&self.state.bundle_ids_by_path).insert(path.clone(), Some(bundle_id.to_owned()));
        lock(&self.state.paths_by_bundle_id).insert(bundle_id.to_owned(), Some(path));
    }

    /// Sets the answer of [`ApplicationBackend::bundle_identifier_at_path`].
    ///
    /// `None` models a path that is not an application bundle.
    pub fn set_bundle_identifier_at_path(&self, path: impl Into<PathBuf>, bundle_id: Option<&str>) {
        lock(&self.state.bundle_ids_by_path).insert(path.into(), bundle_id.map(str::to_owned));
    }

    /// Sets the answer of [`ApplicationBackend::application_path`].
    ///
    /// `None` models an identifier no installed application claims.
    pub fn set_application_path(&self, bundle_id: &str, path: Option<PathBuf>) {
        lock(&self.state.paths_by_bundle_id).insert(bundle_id.to_owned(), path);
    }

    /// Makes the next open fail before reaching `AppKit`.
    pub fn fail_next_open(&self) { self.state.fail_open.store(true, Ordering::SeqCst); }

    /// Makes every termination request be refused by the fake system.
    pub fn refuse_terminate(&self) { self.state.refuse_terminate.store(true, Ordering::SeqCst); }

    /// Makes every open complete synchronously inside [`ApplicationBackend::open`].
    pub fn complete_opens_synchronously(&self) {
        self.state.synchronous_open_completion.store(true, Ordering::SeqCst);
    }

    /// Parks the next registration after it installs and before it returns.
    #[must_use]
    pub fn block_next_observe(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.observe_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next open after preparation and before the issuance permit is
    /// invoked.
    ///
    /// This models native preparation that is still cancellable: an acceptance
    /// or stop that completes while the fake is parked must make the permit
    /// refuse the request, so nothing is opened and no completion runs.
    #[must_use]
    pub fn block_next_open(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.open_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next termination after instance preflight and before the
    /// issuance permit is invoked.
    ///
    /// This models slow native preparation that is still cancellable: a stop or
    /// reconfigure that finishes while the fake is parked must make the permit
    /// refuse the request.
    #[must_use]
    pub fn block_next_terminate(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.terminate_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next termination inside the irreversible native send.
    ///
    /// The control holds the commit gate for that call, so a control-side stop
    /// or acceptance cannot complete while the fake is parked. This models a
    /// stalled native call and proves the gate acquisition is bounded.
    #[must_use]
    pub fn block_next_terminate_issuance(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.terminate_issuance_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next termination after the request was issued and before the
    /// backend returns.
    ///
    /// The request is already irreversible here, so a later stop must not cancel
    /// it; it may only prevent successor work.
    #[must_use]
    pub fn block_next_terminate_after_issue(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.terminate_after_issue_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next running-instance listing after preparation.
    ///
    /// This models a slow native query that is already running when a
    /// configuration acceptance or stop commits.
    #[must_use]
    pub fn block_next_listing(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.list_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next `is_running` query.
    ///
    /// This gives a test a deterministic point inside a multi-instance
    /// retirement to cancel the runtime before the next query.
    #[must_use]
    pub fn block_next_is_running(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.is_running_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next bundle-identifier-at-path query.
    ///
    /// This models a slow path resolution that is already running when a
    /// configuration acceptance commits.
    #[must_use]
    pub fn block_next_bundle_identifier_at_path(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.bundle_identifier_at_path_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next application-path query.
    ///
    /// This models a slow installed-path lookup that is already running when a
    /// configuration acceptance commits.
    #[must_use]
    pub fn block_next_application_path(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.application_path_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// The bundle identifiers the backend was asked to list, in order.
    #[must_use]
    pub fn list_requests(&self) -> Vec<String> { lock(&self.state.list_requests).clone() }

    /// The number of `is_running` queries answered by the backend.
    #[must_use]
    pub fn is_running_calls(&self) -> usize { self.state.is_running_calls.load(Ordering::SeqCst) }

    /// The number of termination attempts, including refused and stale ones.
    #[must_use]
    pub fn terminate_attempts(&self) -> usize {
        self.state.terminate_attempts.load(Ordering::SeqCst)
    }

    /// The number of live observations across every handle.
    #[must_use]
    pub fn observations(&self) -> usize { lock(&self.state.observations).len() }

    /// Delivers a will-launch notification to every live observation.
    ///
    /// Without [`FakeApplicationBackend::defer_launches`] the sink is called
    /// synchronously, so an ordinary explicit launch stays deterministic.
    ///
    /// Returns how many observers received it.
    #[must_use]
    pub fn launch(&self, key: FakeAppKey) -> usize {
        let deliveries: Vec<FakeDelivery> = {
            let applications = lock(&self.state.applications);
            let mut observations = lock(&self.state.observations);
            observations
                .iter_mut()
                .map(|(handle, observation)| {
                    let id = intern(*handle, observation, &applications, key);
                    (Arc::clone(&observation.notify), id)
                })
                .collect()
        };
        let delivered = deliveries.len();
        for (notify, id) in deliveries {
            self.dispatch_notification(notify, id);
        }
        delivered
    }

    /// Defers every later simulated launch delivery through `executor`'s queue.
    ///
    /// This mirrors the system backend, which always enqueues the pure
    /// notification delivery on the main dispatch queue instead of calling the
    /// sink from inside the native callback.
    pub fn defer_launches(&self, executor: Arc<ManualExecutor>) {
        *lock(&self.state.notification_queue) = Some(executor);
    }

    /// Delivers `key` from inside the next termination send, while the issuance
    /// permit holds the commit gate.
    ///
    /// This models a native send that synchronously delivers a will-launch
    /// notification. Pair it with [`FakeApplicationBackend::defer_launches`],
    /// which queues the sink the way the system backend does; calling the sink
    /// under the gate instead would deadlock.
    pub fn launch_during_next_terminate_send(&self, key: FakeAppKey) {
        *lock(&self.state.launch_during_terminate_send) = Some(key);
    }

    /// Delivers `key` from inside the next registration, after the fake
    /// observer exists but before `observe` returns.
    ///
    /// This models a will-launch notification that arrives while the lifecycle
    /// is still installing: the module's inbox must already exist and the event
    /// must be processed after the initial sweep, not dropped.
    pub fn launch_during_next_observe(&self, key: FakeAppKey) {
        *lock(&self.state.launch_during_observe) = Some(key);
    }

    /// Captures one installed observation's delivery like a native block that
    /// outlives removal. Delivering it must pass the generation gate.
    #[must_use]
    pub fn capture_launch(&self, key: FakeAppKey) -> Option<FakeLaunch> {
        let observations = lock(&self.state.observations);
        let (handle, observation) = observations.iter().next()?;
        Some(FakeLaunch {
            backend: Arc::clone(&self.state),
            handle: *handle,
            generation: observation.generation,
            key,
            notify: Arc::clone(&observation.notify),
        })
    }

    /// The termination requests the backend accepted.
    #[must_use]
    pub fn termination_requests(&self) -> Vec<FakeAppKey> {
        lock(&self.state.terminate_requests).clone()
    }

    /// The paths the backend was asked to open.
    #[must_use]
    pub fn open_requests(&self) -> Vec<PathBuf> { lock(&self.state.open_requests).clone() }

    /// The number of open requests whose completion is still pending.
    #[must_use]
    pub fn pending_opens(&self) -> usize {
        lock(&self.state.pending_opens)
            .iter()
            .filter(|completion| completion.is_some())
            .count()
    }

    /// Completes one pending open by issue order.
    ///
    /// Returns whether a completion was waiting at `index`.
    #[must_use]
    pub fn complete_open(&self, index: usize, result: Result<(), String>) -> bool {
        let completion = lock(&self.state.pending_opens).get_mut(index).and_then(Option::take);
        completion.is_some_and(|completion| {
            completion(result.map_err(ApplicationError::OpenFailed));
            true
        })
    }

    /// The PID recorded for one fake application.
    #[must_use]
    pub fn pid(&self, key: FakeAppKey) -> Option<i32> {
        lock(&self.state.applications).get(&key.0).map(|application| application.pid)
    }

    // ----- frontmost, graceful and display fixtures ------------------------

    /// Sets the localized name of one fake application.
    pub fn set_name(&self, key: FakeAppKey, name: Option<&str>) {
        if let Some(application) = lock(&self.state.applications).get_mut(&key.0) {
            application.name = name.map(str::to_owned);
        }
    }

    /// Marks one fake application as this process (`currentApplication`).
    pub fn set_current_process(&self, key: Option<FakeAppKey>) {
        *lock(&self.state.current_process) = key;
    }

    /// Changes the OS frontmost application without delivering any activation
    /// notification: the unobservable interval before notification ingestion.
    pub fn set_frontmost(&self, key: Option<FakeAppKey>) { *lock(&self.state.frontmost) = key; }

    /// Activates `key` and delivers the activation notification to every live
    /// activation observer. Returns how many observers received it.
    ///
    /// The refresh is deferred through [`FakeApplicationBackend::defer_launches`]'s
    /// queue when configured, mirroring the system backend's main-queue deferral.
    #[must_use]
    pub fn activate(&self, key: FakeAppKey) -> usize {
        self.set_frontmost(Some(key));
        self.deliver_activation()
    }

    /// Delivers one activation notification without changing the frontmost
    /// application (for example a duplicate or lagging notification).
    #[must_use]
    pub fn deliver_activation(&self) -> usize {
        if self.state.drop_next_activation.swap(false, Ordering::SeqCst) {
            return 0;
        }
        let sinks: Vec<ActivationSink> = lock(&self.state.activations)
            .values()
            .map(|registration| Arc::clone(&registration.sink))
            .collect();
        let delivered = sinks.len();
        let executor = lock(&self.state.notification_queue).clone();
        for sink in sinks {
            if let Some(executor) = &executor {
                let executor = Arc::clone(executor);
                defer_activation(move |job| executor.try_enqueue(job), &sink);
            } else {
                defer_activation(
                    |job| {
                        job();
                        Ok(())
                    },
                    &sink,
                );
            }
        }
        delivered
    }

    /// Loses the next activation notification entirely (a dropped delivery).
    pub fn drop_next_activation_delivery(&self) {
        self.state.drop_next_activation.store(true, Ordering::SeqCst);
    }

    /// Makes the next activation registration fail.
    pub fn fail_next_activation_observe(&self) {
        self.state.fail_activation_observe.store(true, Ordering::SeqCst);
    }

    /// Makes the next `count` activation removals fail and leave it registered.
    pub fn fail_activation_unobserves(&self, count: usize) {
        self.state.fail_activation_unobserve.store(count, Ordering::SeqCst);
    }

    /// Makes the next frontmost query fail.
    pub fn fail_next_frontmost(&self) { self.state.fail_frontmost.store(true, Ordering::SeqCst); }

    /// Parks the next frontmost query after it read the OS answer and before it
    /// returns (a query that overlaps a later activation).
    #[must_use]
    pub fn block_next_frontmost(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.frontmost_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// The number of live activation registrations.
    #[must_use]
    pub fn activation_observers(&self) -> usize { lock(&self.state.activations).len() }

    /// How many activation registrations have actually been removed.
    #[must_use]
    pub fn activation_removals(&self) -> usize {
        self.state.activation_removals.load(Ordering::SeqCst)
    }

    /// The number of frontmost queries answered or attempted.
    #[must_use]
    pub fn frontmost_queries(&self) -> usize { self.state.frontmost_queries.load(Ordering::SeqCst) }

    /// The graceful requests the backend sent (`terminate`, never force).
    #[must_use]
    pub fn graceful_requests(&self) -> Vec<FakeAppKey> {
        lock(&self.state.graceful_requests).clone()
    }

    /// The number of graceful attempts, including rejected and cancelled ones.
    #[must_use]
    pub fn graceful_attempts(&self) -> usize { self.state.graceful_attempts.load(Ordering::SeqCst) }

    /// Makes every graceful request return `false` from the fake `terminate`.
    pub fn refuse_graceful_terminate(&self) {
        self.state.refuse_graceful.store(true, Ordering::SeqCst);
    }

    /// Parks the next graceful request before its main preflight.
    #[must_use]
    pub fn block_next_graceful_before_preflight(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.graceful_preflight_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next graceful request after preflight and before the permit and
    /// press commit (still cancellable).
    #[must_use]
    pub fn block_next_graceful_before_commit(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.graceful_commit_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next graceful request inside the irreversible send, after the
    /// press authorization committed.
    #[must_use]
    pub fn block_next_graceful_issuance(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.graceful_issuance_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next graceful request after the send committed and before the
    /// backend returns.
    #[must_use]
    pub fn block_next_graceful_after_issue(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.graceful_after_issue_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// The exact-instance activation requests the backend sent. Recorded
    /// independently of open, force and graceful termination, and of the
    /// frontmost-changing [`FakeApplicationBackend::activate`] fixture.
    #[must_use]
    pub fn exact_activation_requests(&self) -> Vec<FakeAppKey> {
        lock(&self.state.exact_activation_requests).clone()
    }

    /// The number of exact activation attempts reaching the backend, including
    /// stale, cancelled and expired ones.
    #[must_use]
    pub fn exact_activation_attempts(&self) -> usize {
        self.state.exact_activation_attempts.load(Ordering::SeqCst)
    }

    /// Makes every exact activation return `false` from the fake native call.
    pub fn refuse_exact_activation(&self) {
        self.state.refuse_exact_activation.store(true, Ordering::SeqCst);
    }

    /// Parks the next exact activation before its native preflight.
    #[must_use]
    pub fn block_next_activation(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.exact_activation_preflight_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next exact activation after preflight and immediately before
    /// the permit and authorization commit (still cancellable).
    #[must_use]
    pub fn block_next_activation_before_commit(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.exact_activation_commit_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next exact activation inside the irreversible send, after the
    /// authorization committed and before it is recorded.
    #[must_use]
    pub fn block_next_activation_issuance(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.exact_activation_issuance_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next exact activation after the send committed and before the
    /// backend returns.
    #[must_use]
    pub fn block_next_activation_after_issue(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.exact_activation_after_issue_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    // ----- hide/unhide and lifecycle fixtures -------------------------------

    /// The exact-instance hide/unhide requests the backend sent, in order.
    #[must_use]
    pub fn visibility_requests(&self) -> Vec<(FakeAppKey, VisibilityOperation)> {
        lock(&self.state.visibility_requests).clone()
    }

    /// The number of hide/unhide attempts reaching the backend, including stale,
    /// cancelled and expired ones.
    #[must_use]
    pub fn visibility_attempts(&self) -> usize {
        self.state.visibility_attempts.load(Ordering::SeqCst)
    }

    /// Makes every hide/unhide return `false` from the fake native call.
    pub fn refuse_visibility(&self) { self.state.refuse_visibility.store(true, Ordering::SeqCst); }

    /// Stops refusing hide/unhide: later requests succeed again.
    pub fn allow_visibility(&self) { self.state.refuse_visibility.store(false, Ordering::SeqCst); }

    /// Whether the fake application is currently hidden.
    #[must_use]
    pub fn is_hidden(&self, key: FakeAppKey) -> bool {
        lock(&self.state.applications)
            .get(&key.0)
            .is_some_and(|application| application.hidden)
    }

    /// Parks the next hide/unhide before its native preflight.
    #[must_use]
    pub fn block_next_visibility(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.visibility_preflight_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next hide/unhide after preflight and before the permit and
    /// authorization commit (still cancellable).
    #[must_use]
    pub fn block_next_visibility_before_commit(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.visibility_commit_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next hide/unhide inside the irreversible send, after the
    /// authorization committed and before it is recorded.
    #[must_use]
    pub fn block_next_visibility_issuance(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.visibility_issuance_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Parks the next hide/unhide after the send committed and before the
    /// backend returns.
    #[must_use]
    pub fn block_next_visibility_after_issue(&self) -> Arc<FakeBarrier> {
        let barrier = Arc::new(FakeBarrier::default());
        *lock(&self.state.visibility_after_issue_barrier) = Some(Arc::clone(&barrier));
        barrier
    }

    /// Makes the next lifecycle registration fail.
    pub fn fail_next_lifecycle_observe(&self) {
        self.state.fail_lifecycle_observe.store(true, Ordering::SeqCst);
    }

    /// Makes the next `count` lifecycle removals fail and leave it registered.
    pub fn fail_lifecycle_unobserves(&self, count: usize) {
        self.state.fail_lifecycle_unobserve.store(count, Ordering::SeqCst);
    }

    /// The number of live lifecycle registrations.
    #[must_use]
    pub fn lifecycle_observers(&self) -> usize { lock(&self.state.lifecycles).len() }

    /// How many lifecycle registrations have actually been removed.
    #[must_use]
    pub fn lifecycle_removals(&self) -> usize {
        self.state.lifecycle_removals.load(Ordering::SeqCst)
    }

    /// Delivers one lifecycle notification for `key` to every live registration
    /// and returns how many registrations received it.
    ///
    /// Mirrors the system backend: a termination resolves through the live
    /// instances, then the bounded tombstones of pruned ones; a termination
    /// that resolves nowhere is delivered as
    /// [`LifecycleKind::TerminatedUnresolved`]. Other kinds intern the instance
    /// (pruning terminated ones first, like the system store). Delivery goes through the
    /// [`FakeApplicationBackend::defer_launches`] queue when configured and is
    /// otherwise immediate. This changes no application state; use
    /// [`FakeApplicationBackend::terminate`] or a hide request for that.
    #[must_use]
    pub fn deliver_lifecycle(&self, kind: LifecycleKind, key: FakeAppKey) -> usize {
        let pid = lock(&self.state.applications)
            .get(&key.0)
            .and_then(|application| super::lifecycle::diagnostic_pid(application.pid));
        let deliveries: Vec<(LifecycleSink, LifecycleKind, ApplicationId)> = {
            let applications = lock(&self.state.applications);
            let mut observations = lock(&self.state.observations);
            lock(&self.state.lifecycles)
                .iter()
                .filter_map(|(handle, registration)| {
                    let observation = observations.get_mut(handle)?;
                    if observation.generation != registration.generation {
                        return None;
                    }
                    let (kind, id) = if kind == LifecycleKind::Terminated {
                        // No pruning here, like the system lookup; pruned
                        // instances resolve through the tombstones and are
                        // cleared on delivery.
                        let live = observation
                            .applications
                            .iter()
                            .find_map(|(token, stored)| (*stored == key).then_some(*token));
                        let token = live.or_else(|| {
                            let index = observation
                                .tombstones
                                .iter()
                                .position(|(_, stored)| *stored == key)?;
                            observation.tombstones.remove(index).map(|(token, _)| token)
                        });
                        match token {
                            Some(token) => (kind, ApplicationId {
                                handle: *handle,
                                generation: observation.generation,
                                token,
                            }),
                            None => (
                                LifecycleKind::TerminatedUnresolved,
                                ApplicationId::unresolved(*handle, observation.generation),
                            ),
                        }
                    } else {
                        (kind, intern(*handle, observation, &applications, key))
                    };
                    Some((Arc::clone(&registration.sink), kind, id))
                })
                .collect()
        };
        let delivered = deliveries.len();
        let executor = lock(&self.state.notification_queue).clone();
        for (sink, kind, application) in deliveries {
            let event = LifecycleEvent { kind, application, pid };
            if let Some(executor) = &executor {
                let executor = Arc::clone(executor);
                defer_lifecycle(move |job| executor.try_enqueue(job), sink, event);
            } else {
                defer_lifecycle(
                    |job| {
                        job();
                        Ok(())
                    },
                    sink,
                    event,
                );
            }
        }
        delivered
    }

    fn remove_lifecycle(&self, handle: BackendHandle, generation: u64) {
        let mut lifecycles = lock(&self.state.lifecycles);
        if lifecycles
            .get(&handle)
            .is_some_and(|registration| registration.generation == generation)
        {
            lifecycles.remove(&handle);
            self.state.lifecycle_removals.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Sets the fake window list (front to back); `None` models an unavailable
    /// `CGWindowListCopyWindowInfo` result.
    pub fn set_windows(&self, windows: Option<Vec<WindowRecord>>) {
        lock(&self.state.display).windows = windows;
    }

    /// Sets the attached screens; index 0 must be the primary screen.
    pub fn set_screens(&self, screens: Vec<ScreenRecord>) {
        lock(&self.state.display).screens = screens;
    }

    /// The number of display geometry reads.
    #[must_use]
    pub fn display_queries(&self) -> usize { self.state.display_queries.load(Ordering::SeqCst) }

    /// Consumes the one-shot query failure shared by every running-instance
    /// query.
    fn query_preflight(&self) -> Result<(), ApplicationError> {
        if self.state.fail_query.swap(false, Ordering::SeqCst) {
            Err(ApplicationError::Unavailable)
        } else {
            Ok(())
        }
    }

    /// Delivers one captured notification through the configured deferral, or
    /// directly when the test did not configure one.
    fn dispatch_notification(
        &self,
        notify: Arc<dyn Fn(ApplicationId) + Send + Sync>,
        application: ApplicationId,
    ) {
        let executor = lock(&self.state.notification_queue).clone();
        if let Some(executor) = executor {
            defer_notification(
                move |delivery| {
                    executor.enqueue(delivery);
                    Ok(())
                },
                notify,
                application,
            );
        } else {
            notify(application);
        }
    }

    /// Mirrors a native send that synchronously delivers a will-launch
    /// notification while the permit still holds the gate.
    fn remove_activation(&self, handle: BackendHandle, generation: u64) {
        let mut activations = lock(&self.state.activations);
        if activations
            .get(&handle)
            .is_some_and(|registration| registration.generation == generation)
        {
            activations.remove(&handle);
            self.state.activation_removals.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn deliver_notification_in_terminate_send(&self) {
        let Some(key) = lock(&self.state.launch_during_terminate_send).take() else {
            return;
        };
        let _ = self.launch(key);
    }
}

/// Mirrors the system `InstanceStore::intern`: every intern first prunes
/// terminated instances, moving them to the bounded tombstone list.
fn intern(
    handle: BackendHandle,
    observation: &mut FakeObservation,
    applications: &BTreeMap<u64, FakeApplication>,
    key: FakeAppKey,
) -> ApplicationId {
    let dead: Vec<u64> = observation
        .applications
        .iter()
        .filter(|(_, stored)| applications.get(&stored.0).is_some_and(|app| !app.alive))
        .map(|(token, _)| *token)
        .collect();
    for token in dead {
        if let Some(stored) = observation.applications.remove(&token) {
            observation.tombstones.push_back((token, stored));
            while observation.tombstones.len() > super::TERMINATED_TOMBSTONES {
                observation.tombstones.pop_front();
            }
        }
    }
    let existing = observation
        .applications
        .iter()
        .find_map(|(token, stored)| (*stored == key).then_some(*token))
        .or_else(|| {
            observation
                .tombstones
                .iter()
                .find_map(|(token, stored)| (*stored == key).then_some(*token))
        });
    if let Some(token) = existing {
        return ApplicationId {
            handle,
            generation: observation.generation,
            token,
        };
    }
    let token = observation.next_token;
    observation.next_token += 1;
    observation.applications.insert(token, key);
    ApplicationId {
        handle,
        generation: observation.generation,
        token,
    }
}

impl ApplicationBackend for FakeApplicationBackend {
    fn observe(
        &self,
        handle: BackendHandle,
        generation: u64,
        notify: Arc<dyn Fn(ApplicationId) + Send + Sync>,
    ) -> Result<(), ApplicationError> {
        if self.state.fail_observe.swap(false, Ordering::SeqCst) {
            return Err(ApplicationError::ObservationFailed(
                "injected failure".to_owned(),
            ));
        }
        let barrier = lock(&self.state.observe_barrier).take();
        {
            let mut observations = lock(&self.state.observations);
            if observations.contains_key(&handle) {
                return Err(ApplicationError::AlreadyInstalled);
            }
            observations.insert(handle, FakeObservation {
                generation,
                next_token: 0,
                notify,
                applications: BTreeMap::new(),
                tombstones: VecDeque::new(),
            });
        }
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        let launch_during_observe = lock(&self.state.launch_during_observe).take();
        if let Some(key) = launch_during_observe {
            // The observer is registered at this point, so this is a real
            // notification delivered before `observe` returns.
            let _ = self.launch(key);
        }
        if self.state.fail_after_observe.swap(false, Ordering::SeqCst) {
            return Err(ApplicationError::ObservationFailed(
                "injected failure after registration".to_owned(),
            ));
        }
        Ok(())
    }

    fn unobserve(&self, handle: BackendHandle, generation: u64) -> Result<(), ApplicationError> {
        let fail = self
            .state
            .fail_unobserve
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok();
        if fail {
            return Err(ApplicationError::RemovalFailed("injected failure".to_owned()));
        }
        // A failed lifecycle removal fails the generation removal too and
        // leaves everything registered, so cleanup stays owned for a retry.
        let lifecycle_registered = lock(&self.state.lifecycles).contains_key(&handle);
        if lifecycle_registered
            && self
                .state
                .fail_lifecycle_unobserve
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
        {
            return Err(ApplicationError::RemovalFailed(
                "injected lifecycle failure".to_owned(),
            ));
        }
        let mut observations = lock(&self.state.observations);
        if observations
            .get(&handle)
            .is_some_and(|observation| observation.generation == generation)
        {
            observations.remove(&handle);
            self.remove_activation(handle, generation);
            self.remove_lifecycle(handle, generation);
        }
        Ok(())
    }

    fn observe_lifecycle(
        &self,
        handle: BackendHandle,
        generation: u64,
        notify: LifecycleSink,
    ) -> Result<(), ApplicationError> {
        if self.state.fail_lifecycle_observe.swap(false, Ordering::SeqCst) {
            return Err(ApplicationError::ObservationFailed(
                "injected lifecycle failure".to_owned(),
            ));
        }
        let live = lock(&self.state.observations)
            .get(&handle)
            .is_some_and(|observation| observation.generation == generation);
        if !live {
            return Err(ApplicationError::NotInstalled);
        }
        let mut lifecycles = lock(&self.state.lifecycles);
        if lifecycles.contains_key(&handle) {
            return Err(ApplicationError::AlreadyInstalled);
        }
        lifecycles.insert(handle, FakeLifecycle { generation, sink: notify });
        Ok(())
    }

    fn unobserve_lifecycle(
        &self,
        handle: BackendHandle,
        generation: u64,
    ) -> Result<(), ApplicationError> {
        let fail = self
            .state
            .fail_lifecycle_unobserve
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok();
        if fail {
            return Err(ApplicationError::RemovalFailed(
                "injected lifecycle failure".to_owned(),
            ));
        }
        self.remove_lifecycle(handle, generation);
        Ok(())
    }

    fn request_visibility(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
        operation: VisibilityOperation,
        authorization: &VisibilityAuthorization,
        permit: IssuancePermit<'_>,
    ) -> Result<VisibilityOutcome, ApplicationError> {
        self.state.visibility_attempts.fetch_add(1, Ordering::SeqCst);
        let barrier = lock(&self.state.visibility_preflight_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        if !self.is_running(handle, application)? {
            return Ok(VisibilityOutcome::Stale);
        }
        let key = {
            let observations = lock(&self.state.observations);
            observations
                .get(&handle)
                .and_then(|observation| observation.applications.get(&application.token).copied())
        };
        let Some(key) = key else {
            return Ok(VisibilityOutcome::Stale);
        };
        // Preflight is complete; this barrier is still cancellable.
        let barrier = lock(&self.state.visibility_commit_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        let issuance_barrier = lock(&self.state.visibility_issuance_barrier).take();
        let Some(outcome) = permit.issue(|| {
            if authorization.is_expired() {
                return VisibilityOutcome::ExpiredBeforeSend;
            }
            if !authorization.commit_send() {
                return VisibilityOutcome::Cancelled;
            }
            if let Some(barrier) = issuance_barrier {
                barrier.enter();
            }
            lock(&self.state.visibility_requests).push((key, operation));
            if self.state.refuse_visibility.load(Ordering::SeqCst) {
                return VisibilityOutcome::Refused;
            }
            if let Some(fake) = lock(&self.state.applications).get_mut(&key.0) {
                fake.hidden = operation == VisibilityOperation::Hide;
            }
            VisibilityOutcome::Requested
        }) else {
            return Ok(VisibilityOutcome::Stale);
        };
        let after = lock(&self.state.visibility_after_issue_barrier).take();
        if let Some(barrier) = after {
            barrier.enter();
        }
        Ok(outcome)
    }

    fn observe_activation(
        &self,
        handle: BackendHandle,
        generation: u64,
        notify: ActivationSink,
    ) -> Result<(), ApplicationError> {
        if self.state.fail_activation_observe.swap(false, Ordering::SeqCst) {
            return Err(ApplicationError::ObservationFailed(
                "injected activation failure".to_owned(),
            ));
        }
        let live = lock(&self.state.observations)
            .get(&handle)
            .is_some_and(|observation| observation.generation == generation);
        if !live {
            return Err(ApplicationError::NotInstalled);
        }
        let mut activations = lock(&self.state.activations);
        if activations.contains_key(&handle) {
            return Err(ApplicationError::AlreadyInstalled);
        }
        activations.insert(handle, FakeActivation { generation, sink: notify });
        Ok(())
    }

    fn unobserve_activation(
        &self,
        handle: BackendHandle,
        generation: u64,
    ) -> Result<(), ApplicationError> {
        let fail = self
            .state
            .fail_activation_unobserve
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok();
        if fail {
            return Err(ApplicationError::RemovalFailed(
                "injected activation failure".to_owned(),
            ));
        }
        self.remove_activation(handle, generation);
        Ok(())
    }

    fn running_details(
        &self,
        handle: BackendHandle,
    ) -> Result<Vec<FrontmostApplication>, ApplicationError> {
        self.query_preflight()?;
        let current = *lock(&self.state.current_process);
        let applications = lock(&self.state.applications);
        let mut observations = lock(&self.state.observations);
        let Some(observation) = observations.get_mut(&handle) else {
            return Err(ApplicationError::NotInstalled);
        };
        let mut details = Vec::new();
        for (key, application) in applications.iter() {
            if !application.alive {
                continue;
            }
            let key = FakeAppKey(*key);
            details.push(FrontmostApplication {
                id: intern(handle, observation, &applications, key),
                bundle_identifier: application.bundle_id.clone(),
                name: application.name.clone(),
                is_current_process: current == Some(key),
                pid: Some(application.pid),
                regular: application.regular,
            });
        }
        Ok(details)
    }

    fn frontmost(
        &self,
        handle: BackendHandle,
    ) -> Result<Option<FrontmostApplication>, ApplicationError> {
        self.state.frontmost_queries.fetch_add(1, Ordering::SeqCst);
        if self.state.fail_frontmost.swap(false, Ordering::SeqCst) {
            return Err(ApplicationError::Unavailable);
        }
        let key = *lock(&self.state.frontmost);
        let current = *lock(&self.state.current_process);
        let result = {
            // Same lock order as `running_applications`: applications first.
            let applications = lock(&self.state.applications);
            let mut observations = lock(&self.state.observations);
            let Some(observation) = observations.get_mut(&handle) else {
                return Err(ApplicationError::NotInstalled);
            };
            key.and_then(|key| {
                let application = applications.get(&key.0)?;
                if !application.alive {
                    return None;
                }
                let id = intern(handle, observation, &applications, key);
                Some(FrontmostApplication {
                    id,
                    bundle_identifier: application.bundle_id.clone(),
                    name: application.name.clone(),
                    is_current_process: current == Some(key),
                    pid: Some(application.pid),
                    regular: application.regular,
                })
            })
        };
        // The answer has been read; a later activation can still land before
        // this query returns.
        let barrier = lock(&self.state.frontmost_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        Ok(result)
    }

    fn request_graceful_terminate(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
        authorization: &PressAuthorization,
        permit: IssuancePermit<'_>,
    ) -> Result<GracefulOutcome, ApplicationError> {
        self.state.graceful_attempts.fetch_add(1, Ordering::SeqCst);
        let barrier = lock(&self.state.graceful_preflight_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        match preflight_on_main(self, handle, application)? {
            FrontmostPreflight::Eligible(_) => {}
            FrontmostPreflight::NotRunning => return Ok(GracefulOutcome::NotRunning),
            FrontmostPreflight::Rejected(reason) => {
                return Ok(GracefulOutcome::Rejected(reason));
            }
        }
        let key = {
            let observations = lock(&self.state.observations);
            observations
                .get(&handle)
                .and_then(|observation| observation.applications.get(&application.token).copied())
        };
        let Some(key) = key else {
            return Ok(GracefulOutcome::NotRunning);
        };
        // Preflight is complete; this barrier is still cancellable.
        let barrier = lock(&self.state.graceful_commit_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        let issuance_barrier = lock(&self.state.graceful_issuance_barrier).take();
        let Some(outcome) = permit.issue(|| {
            // The one-shot press commit and the send are the irreversible step.
            if !authorization.commit_send() {
                return GracefulOutcome::Cancelled;
            }
            if let Some(barrier) = issuance_barrier {
                barrier.enter();
            }
            lock(&self.state.graceful_requests).push(key);
            if self.state.refuse_graceful.load(Ordering::SeqCst) {
                GracefulOutcome::Refused
            } else {
                GracefulOutcome::Requested
            }
        }) else {
            return Ok(GracefulOutcome::Stale);
        };
        let after = lock(&self.state.graceful_after_issue_barrier).take();
        if let Some(barrier) = after {
            barrier.enter();
        }
        Ok(outcome)
    }

    fn request_activate(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
        authorization: &ActivationAuthorization,
        permit: IssuancePermit<'_>,
    ) -> Result<ActivationOutcome, ApplicationError> {
        self.state.exact_activation_attempts.fetch_add(1, Ordering::SeqCst);
        let barrier = lock(&self.state.exact_activation_preflight_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        if !self.is_running(handle, application)? {
            return Ok(ActivationOutcome::Stale);
        }
        let key = {
            let observations = lock(&self.state.observations);
            observations
                .get(&handle)
                .and_then(|observation| observation.applications.get(&application.token).copied())
        };
        let Some(key) = key else {
            return Ok(ActivationOutcome::Stale);
        };
        // Preflight is complete; this barrier is still cancellable.
        let barrier = lock(&self.state.exact_activation_commit_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        let issuance_barrier = lock(&self.state.exact_activation_issuance_barrier).take();
        let Some(outcome) = permit.issue(|| {
            if authorization.is_expired() {
                return ActivationOutcome::ExpiredBeforeSend;
            }
            if !authorization.commit_send() {
                return ActivationOutcome::Cancelled;
            }
            if let Some(barrier) = issuance_barrier {
                barrier.enter();
            }
            lock(&self.state.exact_activation_requests).push(key);
            if self.state.refuse_exact_activation.load(Ordering::SeqCst) {
                ActivationOutcome::Refused
            } else {
                ActivationOutcome::Requested
            }
        }) else {
            return Ok(ActivationOutcome::Stale);
        };
        let after = lock(&self.state.exact_activation_after_issue_barrier).take();
        if let Some(barrier) = after {
            barrier.enter();
        }
        Ok(outcome)
    }

    fn display_hint(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
    ) -> Result<DisplayHint, ApplicationError> {
        self.state.display_queries.fetch_add(1, Ordering::SeqCst);
        let key = {
            let observations = lock(&self.state.observations);
            observations
                .get(&handle)
                .and_then(|observation| observation.applications.get(&application.token).copied())
        };
        let Some(key) = key else {
            return Ok(DisplayHint::none(super::display::DisplaySource::NotValidated));
        };
        let pid = self.pid(key).unwrap_or(0);
        let display = lock(&self.state.display);
        Ok(select_display(display.windows.as_deref(), pid, &display.screens))
    }

    fn running_applications(
        &self,
        handle: BackendHandle,
        bundle_id: &str,
    ) -> Result<Vec<ApplicationId>, ApplicationError> {
        self.query_preflight()?;
        lock(&self.state.list_requests).push(bundle_id.to_owned());
        let barrier = lock(&self.state.list_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        let matches = {
            let applications = lock(&self.state.applications);
            let mut observations = lock(&self.state.observations);
            let Some(observation) = observations.get_mut(&handle) else {
                return Ok(Vec::new());
            };
            let mut matches = Vec::new();
            for (key, application) in applications.iter() {
                if application.alive && application.bundle_id.as_deref() == Some(bundle_id) {
                    matches.push(intern(handle, observation, &applications, FakeAppKey(*key)));
                }
            }
            matches
        };
        Ok(matches)
    }

    fn is_running(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
    ) -> Result<bool, ApplicationError> {
        self.state.is_running_calls.fetch_add(1, Ordering::SeqCst);
        self.query_preflight()?;
        let barrier = lock(&self.state.is_running_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        if application.handle != handle {
            return Ok(false);
        }
        let key = {
            let observations = lock(&self.state.observations);
            let Some(observation) = observations.get(&handle) else {
                return Ok(false);
            };
            if observation.generation != application.generation {
                return Ok(false);
            }
            observation.applications.get(&application.token).copied()
        };
        let Some(key) = key else {
            return Ok(false);
        };
        Ok(lock(&self.state.applications)
            .get(&key.0)
            .is_some_and(|application| application.alive))
    }

    fn bundle_identifier(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
    ) -> Result<Option<String>, ApplicationError> {
        self.query_preflight()?;
        if application.handle != handle {
            return Ok(None);
        }
        let key = {
            let observations = lock(&self.state.observations);
            let Some(observation) = observations.get(&handle) else {
                return Ok(None);
            };
            if observation.generation != application.generation {
                return Ok(None);
            }
            observation.applications.get(&application.token).copied()
        };
        Ok(key.and_then(|key| {
            lock(&self.state.applications)
                .get(&key.0)
                .and_then(|application| application.bundle_id.clone())
        }))
    }

    fn bundle_identifier_at_path(
        &self,
        _handle: BackendHandle,
        path: &Path,
    ) -> Result<Option<String>, ApplicationError> {
        self.query_preflight()?;
        let barrier = lock(&self.state.bundle_identifier_at_path_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        Ok(lock(&self.state.bundle_ids_by_path).get(path).cloned().flatten())
    }

    fn application_path(
        &self,
        _handle: BackendHandle,
        bundle_id: &str,
    ) -> Result<Option<PathBuf>, ApplicationError> {
        self.query_preflight()?;
        let barrier = lock(&self.state.application_path_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        Ok(lock(&self.state.paths_by_bundle_id).get(bundle_id).cloned().flatten())
    }

    fn request_terminate(
        &self,
        handle: BackendHandle,
        application: ApplicationId,
        permit: IssuancePermit<'_>,
    ) -> Result<TerminateOutcome, ApplicationError> {
        self.state.terminate_attempts.fetch_add(1, Ordering::SeqCst);
        if !self.is_running(handle, application)? {
            return Ok(TerminateOutcome::NotRunning);
        }
        if self.state.refuse_terminate.load(Ordering::SeqCst) {
            return Ok(TerminateOutcome::Refused);
        }
        let key = {
            let observations = lock(&self.state.observations);
            let Some(observation) = observations.get(&handle) else {
                return Ok(TerminateOutcome::NotRunning);
            };
            observation.applications.get(&application.token).copied()
        };
        let Some(key) = key else {
            return Ok(TerminateOutcome::NotRunning);
        };
        // Preflight is complete; this barrier is still cancellable.
        let barrier = lock(&self.state.terminate_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        let issuance_barrier = lock(&self.state.terminate_issuance_barrier).take();
        let Some(outcome) = permit.issue(|| {
            // This closure is the irreversible send: only it runs under the
            // control's commit gate.
            if let Some(barrier) = issuance_barrier {
                barrier.enter();
            }
            lock(&self.state.terminate_requests).push(key);
            // The send may synchronously deliver a will-launch notification.
            // The fake goes through the same deferral contract as the system
            // backend, so no sink runs while the gate is held here.
            self.deliver_notification_in_terminate_send();
            TerminateOutcome::Requested
        }) else {
            return Ok(TerminateOutcome::Stale);
        };
        let after = lock(&self.state.terminate_after_issue_barrier).take();
        if let Some(barrier) = after {
            barrier.enter();
        }
        Ok(outcome)
    }

    fn open(
        &self,
        _handle: BackendHandle,
        path: &Path,
        completion: NativeOpenCompletion,
        permit: IssuancePermit<'_>,
    ) -> Result<OpenRequest, ApplicationError> {
        if self.state.fail_open.swap(false, Ordering::SeqCst) {
            return Err(ApplicationError::OpenFailed("injected failure".to_owned()));
        }
        let path = path.to_path_buf();
        // Preparation is complete; this barrier is still cancellable because the
        // permit below has not been asked to issue anything yet.
        let barrier = lock(&self.state.open_barrier).take();
        if let Some(barrier) = barrier {
            barrier.enter();
        }
        let Some(()) = permit.issue(|| {
            // The request record and the completion call are the fake's
            // irreversible send. A synchronous completion therefore exercises
            // the real reentrancy path.
            lock(&self.state.open_requests).push(path);
            if self.state.synchronous_open_completion.load(Ordering::SeqCst) {
                completion(Ok(()));
            } else {
                lock(&self.state.pending_opens).push(Some(completion));
            }
        }) else {
            return Ok(OpenRequest::Stale);
        };
        Ok(OpenRequest::Issued)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
