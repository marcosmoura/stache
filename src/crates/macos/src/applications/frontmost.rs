//! Frontmost-application observation, publication and preflight.
//!
//! This extends the applications adapter additively. The will-launch
//! observation is untouched: a separate workspace activation observer is
//! registered on the same generation, interns instances in the same
//! main-thread owner maps and is removed together with the generation.
//!
//! # Publication model
//!
//! The input path reads [`FrontmostWatch::snapshot`]. That read is
//! nonblocking and never calls a native API: it loads one atomic activation
//! epoch, `try_lock`s a tiny mutex around plain data and compares the epoch the
//! data was confirmed at with the current one. Anything unexpected (contention,
//! an invalidated generation, a pending refresh, missing metadata) reports an
//! explicit [`Unconfirmed`] reason, so the caller fails open.
//!
//! A native activation callback only invalidates and enqueues:
//!
//! 1. the activation epoch is bumped through a plain atomic, so the previously
//!    published confirmation is stale immediately;
//! 2. a refresh is coalesced into at most one pending main-queue job (a
//!    rejected enqueue drops the ticket and leaves the state unconfirmed).
//!
//! The deferred refresh runs on main, reads the epoch, queries the real
//! frontmost application, and publishes only when the epoch is unchanged, so an
//! older answer can never overwrite a newer activation. A -> B -> A therefore
//! invalidates an A press even though the final identity equals A: the epoch
//! differs. Install registers observation before the initial query and
//! epoch-checks that query the same way.
//!
//! # Limitations
//!
//! Publication cannot atomically track an OS focus switch that precedes its
//! notification. In that unobservable interval the snapshot may still show the
//! previous application, so a key-down can be suppressed (or passed) based on
//! it. [`ApplicationObservation::preflight_frontmost`] and the graceful issuance
//! re-query the real frontmost application on main immediately before showing
//! or sending, so a now-exempt or different application is never shown or
//! quit. The adapter never knowingly trusts a stale cache and does not claim
//! polling would remove the interval.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError, Weak};

use stache_sdk::CancellationToken;

use super::{
    AckWait, ApplicationBackend, ApplicationError, ApplicationId, ApplicationObservation,
    BackendHandle, CANCELLATION_QUANTUM, ControlState, InstallAck, lock,
};

/// The Finder bundle identifier; ⌘Q passes through it.
pub const FINDER_BUNDLE_IDENTIFIER: &str = "com.apple.finder";

/// One frontmost application as the native query reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontmostApplication {
    /// The exact retained instance.
    pub id: ApplicationId,
    /// Its bundle identifier, when the system reports one.
    pub bundle_identifier: Option<String>,
    /// Its localized name, when the system reports one.
    pub name: Option<String>,
    /// Whether it is this process, compared by retained-instance equality
    /// against `NSRunningApplication.currentApplication`.
    pub is_current_process: bool,
    /// Its process identifier, when known. A PID is never identity: it only
    /// lets a consumer create a native handle (an Accessibility root) for the
    /// exact `id`, which is validated again before and after use.
    pub pid: Option<i32>,
    /// Whether it is a regular application (`NSApplicationActivationPolicyRegular`).
    /// Accessory and prohibited (agent, background) applications are not.
    pub regular: bool,
}

/// How a frontmost application is treated by an exemption policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrontmostClass {
    /// A nonexempt application.
    Eligible,
    /// Finder (`com.apple.finder`).
    Finder,
    /// This process.
    CurrentProcess,
    /// The bundle identifier needed to decide an exemption is unknown.
    UnknownMetadata,
}

impl FrontmostApplication {
    /// Classifies the application for an exemption decision.
    #[must_use]
    pub fn class(&self) -> FrontmostClass {
        if self.is_current_process {
            FrontmostClass::CurrentProcess
        } else {
            match self.bundle_identifier.as_deref() {
                Some(FINDER_BUNDLE_IDENTIFIER) => FrontmostClass::Finder,
                Some(_) => FrontmostClass::Eligible,
                None => FrontmostClass::UnknownMetadata,
            }
        }
    }
}

/// Why a snapshot is not a confirmed answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unconfirmed {
    /// Nothing has been published yet.
    NotObserved,
    /// An activation arrived after the last confirmation and its refresh has
    /// not published (or was dropped).
    Invalidated,
    /// The system reported no frontmost application.
    NoFrontmost,
    /// The bundle identifier needed for exemption classification is unknown.
    UnknownMetadata,
    /// The refresh query failed.
    QueryFailed,
    /// The generation is stopped, invalidated or the watch failed to install.
    Inactive,
    /// The publication cell was busy; the read did not wait.
    Busy,
}

/// Whether a snapshot may be acted on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrontmostState {
    /// The snapshot matches the current activation epoch.
    Confirmed,
    /// The snapshot must not be acted on.
    Unconfirmed(Unconfirmed),
}

/// Pure frontmost data published for the shared input path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontmostSnapshot {
    /// The observation generation that published it.
    pub generation: u64,
    /// The activation epoch. It changes on every activation notification and
    /// never repeats, so a press that captured an epoch is invalid once it
    /// differs, even after A -> B -> A.
    pub epoch: u64,
    /// Whether the data is confirmed for `epoch`.
    pub state: FrontmostState,
    /// The exact frontmost instance, when known.
    pub application: Option<ApplicationId>,
    /// Its bundle identifier, when known.
    pub bundle_identifier: Option<Arc<str>>,
    /// Its localized name, when known.
    pub name: Option<Arc<str>>,
    /// Whether it is this process (exact instance comparison).
    pub is_current_process: bool,
    /// Whether it is Finder.
    pub is_finder: bool,
    /// Its process identifier, when known (see [`FrontmostApplication::pid`]).
    pub pid: Option<i32>,
}

impl FrontmostSnapshot {
    const fn unconfirmed(generation: u64, epoch: u64, reason: Unconfirmed) -> Self {
        Self {
            generation,
            epoch,
            state: FrontmostState::Unconfirmed(reason),
            application: None,
            bundle_identifier: None,
            name: None,
            is_current_process: false,
            is_finder: false,
            pid: None,
        }
    }

    /// Whether the snapshot is confirmed for its epoch.
    #[must_use]
    pub const fn is_confirmed(&self) -> bool { matches!(self.state, FrontmostState::Confirmed) }

    /// Whether the confirmed frontmost application is Finder or this process.
    #[must_use]
    pub const fn is_exempt(&self) -> bool {
        self.is_confirmed() && (self.is_finder || self.is_current_process)
    }

    /// The confirmed, nonexempt frontmost instance, if any.
    ///
    /// Missing, unconfirmed or exempt state is `None`: callers fail open.
    #[must_use]
    pub const fn eligible_application(&self) -> Option<ApplicationId> {
        if self.is_confirmed() && !self.is_finder && !self.is_current_process {
            self.application
        } else {
            None
        }
    }
}

/// Why a main preflight refused to proceed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreflightRejection {
    /// The system reports no frontmost application.
    NoFrontmost,
    /// A different application is frontmost.
    NotFrontmost,
    /// Finder is frontmost.
    Finder,
    /// This process is frontmost.
    CurrentProcess,
    /// The captured instance is frontmost but its bundle identifier is unknown,
    /// so the Finder exemption cannot be decided.
    Unconfirmed,
}

/// The result of a main-thread preflight against the exact captured instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrontmostPreflight {
    /// The captured instance is running, frontmost, and neither Finder nor this
    /// process.
    Eligible(FrontmostApplication),
    /// The captured instance is no longer running (or belongs to another
    /// control).
    NotRunning,
    /// The press must be cancelled; never retarget to the current frontmost.
    Rejected(PreflightRejection),
}

/// Decides a preflight from already-gathered native facts. Pure.
#[must_use]
pub fn classify_preflight(
    captured: ApplicationId,
    running: bool,
    frontmost: Option<&FrontmostApplication>,
) -> FrontmostPreflight {
    if !running {
        return FrontmostPreflight::NotRunning;
    }
    let Some(front) = frontmost else {
        return FrontmostPreflight::Rejected(PreflightRejection::NoFrontmost);
    };
    let class = front.class();
    if front.id != captured {
        return FrontmostPreflight::Rejected(match class {
            FrontmostClass::Finder => PreflightRejection::Finder,
            FrontmostClass::CurrentProcess => PreflightRejection::CurrentProcess,
            FrontmostClass::Eligible | FrontmostClass::UnknownMetadata => {
                PreflightRejection::NotFrontmost
            }
        });
    }
    match class {
        FrontmostClass::Eligible => FrontmostPreflight::Eligible(front.clone()),
        FrontmostClass::Finder => FrontmostPreflight::Rejected(PreflightRejection::Finder),
        FrontmostClass::CurrentProcess => {
            FrontmostPreflight::Rejected(PreflightRejection::CurrentProcess)
        }
        FrontmostClass::UnknownMetadata => {
            FrontmostPreflight::Rejected(PreflightRejection::Unconfirmed)
        }
    }
}

/// Runs the exact-identity/current-frontmost preflight on the calling (main)
/// thread through the backend's own queries.
pub(super) fn preflight_on_main(
    backend: &dyn ApplicationBackend,
    handle: BackendHandle,
    captured: ApplicationId,
) -> Result<FrontmostPreflight, ApplicationError> {
    let running = backend.is_running(handle, captured)?;
    if !running {
        return Ok(FrontmostPreflight::NotRunning);
    }
    let front = backend.frontmost(handle)?;
    Ok(classify_preflight(captured, running, front.as_ref()))
}

/// Change notifications for a host that republishes frontmost state elsewhere.
///
/// Registered through [`ApplicationObservation::watch_frontmost_observed`].
/// Both callbacks are contained (unwind-enabled builds only; release aborts): a
/// panic is caught and never reaches the activation sink or the main refresh.
/// Neither runs while the publication cell is locked, so an implementation may
/// call [`FrontmostWatch::snapshot`].
pub trait FrontmostListener: Send + Sync {
    /// An activation was ingested and the epoch advanced to `epoch`; the
    /// previous confirmation is already stale.
    ///
    /// Runs from the native activation sink right after the epoch bump. It must
    /// be bounded: it may take its own short locks but must not dispatch to
    /// main, query the system or wait for anything.
    fn invalidated(&self, epoch: u64);

    /// The state after a refresh on the main thread, or after the watch died.
    ///
    /// Called after a confirmed publish, after a failed query (the resulting
    /// unconfirmed snapshot), and when a refresh was rejected because a newer
    /// activation arrived (an [`Unconfirmed::Invalidated`] snapshot at the
    /// current epoch). Exactly once more, with [`Unconfirmed::Inactive`], when
    /// the watch stops, is invalidated or fails to install; that call is not
    /// tied to the main thread.
    fn published(&self, snapshot: &FrontmostSnapshot);
}

/// The shared publication cell.
pub(super) struct FrontmostShared {
    generation: u64,
    epoch: AtomicU64,
    dead: AtomicBool,
    refresh_pending: Arc<AtomicBool>,
    published: Mutex<FrontmostSnapshot>,
    listener: Option<Arc<dyn FrontmostListener>>,
    inactive_sent: AtomicBool,
}

impl FrontmostShared {
    fn new(generation: u64, listener: Option<Arc<dyn FrontmostListener>>) -> Self {
        Self {
            listener,
            inactive_sent: AtomicBool::new(false),
            generation,
            // Epoch 1 is "now"; the initial published cell carries epoch 0 and
            // is therefore unconfirmed until the first refresh publishes.
            epoch: AtomicU64::new(1),
            dead: AtomicBool::new(false),
            refresh_pending: Arc::new(AtomicBool::new(false)),
            published: Mutex::new(FrontmostSnapshot::unconfirmed(
                generation,
                0,
                Unconfirmed::NotObserved,
            )),
        }
    }

    pub(super) fn kill(&self) {
        self.dead.store(true, Ordering::SeqCst);
        self.notify_inactive();
    }

    /// Tells the listener, once, that the watch is no longer live.
    pub(super) fn notify_inactive(&self) {
        let Some(listener) = &self.listener else {
            return;
        };
        if self.inactive_sent.swap(true, Ordering::SeqCst) {
            return;
        }
        let epoch = self.epoch.load(Ordering::SeqCst);
        let snapshot =
            FrontmostSnapshot::unconfirmed(self.generation, epoch, Unconfirmed::Inactive);
        contain(|| listener.published(&snapshot));
    }

    pub(super) const fn generation(&self) -> u64 { self.generation }

    fn notify_published(&self, snapshot: &FrontmostSnapshot) {
        if let Some(listener) = &self.listener {
            contain(|| listener.published(snapshot));
        }
    }

    pub(super) fn is_dead(&self) -> bool { self.dead.load(Ordering::SeqCst) }

    /// Publishes a query result when no newer activation arrived meanwhile.
    fn publish(
        &self,
        query_epoch: u64,
        result: Result<Option<FrontmostApplication>, ApplicationError>,
    ) {
        let mut cell = lock(&self.published);
        if self.is_dead() {
            return;
        }
        let now = self.epoch.load(Ordering::SeqCst);
        if now != query_epoch {
            drop(cell);
            // A newer activation superseded this answer: nothing is published,
            // but the listener learns the state is unconfirmed.
            self.notify_published(&FrontmostSnapshot::unconfirmed(
                self.generation,
                now,
                Unconfirmed::Invalidated,
            ));
            return;
        }
        let snapshot = build_snapshot(self.generation, query_epoch, result);
        cell.clone_from(&snapshot);
        // The listener never runs inside the publication lock.
        drop(cell);
        self.notify_published(&snapshot);
    }
}

/// Runs a listener callback, containing a panic (unwind-enabled builds only; release aborts).
fn contain(call: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call));
}

fn build_snapshot(
    generation: u64,
    epoch: u64,
    result: Result<Option<FrontmostApplication>, ApplicationError>,
) -> FrontmostSnapshot {
    let front = match result {
        Ok(Some(front)) => front,
        Ok(None) => {
            return FrontmostSnapshot::unconfirmed(generation, epoch, Unconfirmed::NoFrontmost);
        }
        Err(_) => {
            return FrontmostSnapshot::unconfirmed(generation, epoch, Unconfirmed::QueryFailed);
        }
    };
    let class = front.class();
    let state = if class == FrontmostClass::UnknownMetadata {
        FrontmostState::Unconfirmed(Unconfirmed::UnknownMetadata)
    } else {
        FrontmostState::Confirmed
    };
    FrontmostSnapshot {
        generation,
        epoch,
        state,
        application: Some(front.id),
        bundle_identifier: front.bundle_identifier.map(Arc::from),
        name: front.name.map(Arc::from),
        is_current_process: class == FrontmostClass::CurrentProcess,
        is_finder: class == FrontmostClass::Finder,
        pid: front.pid,
    }
}

/// Runs one refresh on the calling (main) thread: query, then epoch-checked
/// publish.
pub(super) fn refresh_now(shared: &FrontmostShared, state: &ControlState) {
    if shared.is_dead() || state.is_invalidated(shared.generation) {
        return;
    }
    let epoch = shared.epoch.load(Ordering::SeqCst);
    let result = state.backend.frontmost(state.handle);
    shared.publish(epoch, result);
}

/// One coalesced deferred refresh.
///
/// A backend enqueues [`RefreshTicket::run`] on a later main turn. Dropping the
/// ticket without running it (a rejected enqueue) releases the coalescing flag
/// and leaves the published state unconfirmed.
#[must_use = "a refresh ticket only refreshes when it is run"]
pub struct RefreshTicket {
    shared: Arc<FrontmostShared>,
    state: Weak<ControlState>,
    armed: bool,
}

impl RefreshTicket {
    /// Runs the refresh on the main thread. Panics are contained (unwind-enabled builds only;
    /// release aborts).
    pub fn run(mut self) {
        self.armed = false;
        self.shared.refresh_pending.store(false, Ordering::SeqCst);
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            refresh_now(&self.shared, &state);
        }));
    }
}

impl Drop for RefreshTicket {
    fn drop(&mut self) {
        if self.armed {
            self.shared.refresh_pending.store(false, Ordering::SeqCst);
        }
    }
}

impl std::fmt::Debug for RefreshTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshTicket").finish_non_exhaustive()
    }
}

/// The sink a backend calls from its native activation callback.
///
/// It performs only plain-state invalidation and returns the coalesced refresh
/// the backend must enqueue on a later main turn (never inline: the callback may
/// run inside a native send that holds the commit gate).
pub type ActivationSink = Arc<dyn Fn() -> Option<RefreshTicket> + Send + Sync>;

/// Enqueues the refresh returned by `sink`; a rejected enqueue leaves the state
/// invalidated (unconfirmed) and never runs the refresh inline.
pub(super) fn defer_activation(
    enqueue: impl FnOnce(Box<dyn FnOnce() + Send>) -> Result<(), stache_sdk::ThreadError>,
    sink: &ActivationSink,
) {
    if let Some(ticket) = sink() {
        let _ = enqueue(Box::new(move || ticket.run()));
    }
}

/// The module-facing, `Send + Sync` read handle for frontmost state.
#[derive(Clone)]
pub struct FrontmostWatch {
    shared: Arc<FrontmostShared>,
    state: Arc<ControlState>,
}

impl std::fmt::Debug for FrontmostWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrontmostWatch")
            .field("generation", &self.shared.generation)
            .finish_non_exhaustive()
    }
}

impl FrontmostWatch {
    /// Holds the publication cell so a test can prove reads never wait on it.
    #[cfg(test)]
    pub(super) fn hold_publication(&self) -> std::sync::MutexGuard<'_, FrontmostSnapshot> {
        lock(&self.shared.published)
    }

    /// The current published snapshot. Nonblocking; never calls a native API.
    ///
    /// Safe to call from the shared input path. Any contention, pending
    /// refresh, invalidated generation or unknown metadata is reported as
    /// unconfirmed.
    #[must_use]
    pub fn snapshot(&self) -> FrontmostSnapshot {
        let generation = self.shared.generation;
        let epoch = self.shared.epoch.load(Ordering::SeqCst);
        if self.shared.is_dead() || self.state.is_invalidated(generation) {
            return FrontmostSnapshot::unconfirmed(generation, epoch, Unconfirmed::Inactive);
        }
        let published = match self.shared.published.try_lock() {
            Ok(cell) => cell.clone(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().clone(),
            Err(TryLockError::WouldBlock) => {
                return FrontmostSnapshot::unconfirmed(generation, epoch, Unconfirmed::Busy);
            }
        };
        // Re-read after the copy: an activation between the two reads makes the
        // copy stale, never trusted.
        let now = self.shared.epoch.load(Ordering::SeqCst);
        if published.epoch != epoch || now != epoch {
            return FrontmostSnapshot::unconfirmed(generation, now, Unconfirmed::Invalidated);
        }
        published
    }

    /// The current activation epoch. A plain atomic read.
    #[must_use]
    pub fn epoch(&self) -> u64 { self.shared.epoch.load(Ordering::SeqCst) }

    /// Whether `epoch` (captured from a snapshot) is still the current
    /// activation epoch of a live generation. A plain atomic read; this
    /// invalidates an A -> B -> A press even when the final identity is A, but
    /// it is not a substitute for the native preflight.
    #[must_use]
    pub fn is_epoch_current(&self, epoch: u64) -> bool {
        !self.shared.is_dead()
            && !self.state.is_invalidated(self.shared.generation)
            && self.shared.epoch.load(Ordering::SeqCst) == epoch
    }

    /// Requests a main refresh and returns the resulting snapshot.
    ///
    /// For worker threads recovering from an unconfirmed state. Waits at most
    /// the acknowledgement budget and never from the main thread's own queue
    /// (an inline main call completes directly).
    ///
    /// # Errors
    /// Returns a dispatch rejection, [`ApplicationError::NotInstalled`] for a
    /// stopped generation or [`ApplicationError::TimedOut`].
    pub fn refresh(&self) -> Result<FrontmostSnapshot, ApplicationError> {
        let ack = Arc::new(super::Ack::new());
        let job_ack = Arc::clone(&ack);
        let state = Arc::clone(&self.state);
        let shared = Arc::clone(&self.shared);
        let job = Box::new(move || {
            if shared.is_dead() || !state.is_active(shared.generation) {
                job_ack.complete(false);
                return;
            }
            refresh_now(&shared, &state);
            job_ack.complete(true);
        });
        self.state
            .executor
            .execute(job)
            .map_err(|error| ApplicationError::Dispatch(error.to_string()))?;
        match ack.wait(self.state.budget) {
            AckWait::Ready(true) => Ok(self.snapshot()),
            AckWait::Ready(false) => Err(ApplicationError::NotInstalled),
            AckWait::TimedOut => Err(ApplicationError::TimedOut),
            AckWait::Cancelled => Err(ApplicationError::Cancelled),
        }
    }
}

impl ApplicationObservation {
    /// Installs workspace activation observation and publishes the first
    /// frontmost snapshot.
    ///
    /// Observation is registered first and the initial query is epoch-checked,
    /// so an activation that lands during the query is never overwritten by the
    /// older answer. The wait observes cancellation every
    /// [`CANCELLATION_QUANTUM`] and the acknowledgement budget. A failed,
    /// cancelled or timed-out install makes the watch inert immediately; a
    /// registered activation observer stays owned by this observation and is
    /// removed with it (or by the next successful attempt), exactly once.
    ///
    /// # Errors
    /// Returns [`ApplicationError::AlreadyInstalled`] while a live watch
    /// exists, cancellation, a dispatch rejection, a registration failure or a
    /// timeout.
    pub fn watch_frontmost(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<FrontmostWatch, ApplicationError> {
        self.install_frontmost(cancellation, None)
    }

    /// Like [`Self::watch_frontmost`], with a [`FrontmostListener`] told about
    /// every invalidation and publication.
    ///
    /// `invalidated` runs from the activation sink, bounded and never waiting
    /// on main. `published` runs on main after each refresh, outside the
    /// publication lock, and once more with [`Unconfirmed::Inactive`] when the
    /// watch dies. The watch itself behaves exactly like the unobserved one.
    ///
    /// # Errors
    /// The same as [`Self::watch_frontmost`].
    pub fn watch_frontmost_observed(
        &self,
        cancellation: &CancellationToken,
        listener: Arc<dyn FrontmostListener>,
    ) -> Result<FrontmostWatch, ApplicationError> {
        self.install_frontmost(cancellation, Some(listener))
    }

    fn install_frontmost(
        &self,
        cancellation: &CancellationToken,
        listener: Option<Arc<dyn FrontmostListener>>,
    ) -> Result<FrontmostWatch, ApplicationError> {
        if cancellation.is_cancelled() {
            return Err(ApplicationError::Cancelled);
        }
        let generation = self.generation;
        let shared = {
            let mut slot = lock(&self.frontmost);
            if slot.as_ref().is_some_and(|existing| !existing.is_dead()) {
                return Err(ApplicationError::AlreadyInstalled);
            }
            let shared = Arc::new(FrontmostShared::new(generation, listener));
            *slot = Some(Arc::clone(&shared));
            drop(slot);
            self.state.watch_frontmost_inactive_on_invalidation(&shared);
            shared
        };

        let weak_state = Arc::downgrade(&self.state);
        let sink_shared = Arc::clone(&shared);
        let sink: ActivationSink = Arc::new(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // Invalidate first, through plain atomics only.
                let epoch = sink_shared.epoch.fetch_add(1, Ordering::SeqCst) + 1;
                let state = weak_state.upgrade()?;
                if sink_shared.is_dead() || state.is_invalidated(generation) {
                    return None;
                }
                if let Some(listener) = &sink_shared.listener {
                    contain(|| listener.invalidated(epoch));
                }
                if sink_shared.refresh_pending.swap(true, Ordering::SeqCst) {
                    return None;
                }
                Some(RefreshTicket {
                    shared: Arc::clone(&sink_shared),
                    state: Arc::downgrade(&state),
                    armed: true,
                })
            }))
            .unwrap_or(None)
        });

        let ack = Arc::new(super::Ack::new());
        let job_ack = Arc::clone(&ack);
        let job_state = Arc::clone(&self.state);
        let job_shared = Arc::clone(&shared);
        let token = cancellation.clone();
        let job = Box::new(move || {
            if token.is_cancelled() || !job_state.is_active(generation) || job_shared.is_dead() {
                job_ack.complete(InstallAck::Aborted);
                return;
            }
            // A failed predecessor may have left its observer registered.
            if let Err(error) = job_state.backend.unobserve_activation(job_state.handle, generation)
            {
                job_ack.complete(InstallAck::Done(Err(error)));
                return;
            }
            match job_state.backend.observe_activation(job_state.handle, generation, sink) {
                Ok(()) => {
                    if token.is_cancelled() || job_shared.is_dead() {
                        job_ack.complete(InstallAck::Aborted);
                        return;
                    }
                    // Observation is live: the initial query is epoch-checked.
                    refresh_now(&job_shared, &job_state);
                    job_ack.complete(InstallAck::Done(Ok(())));
                }
                Err(error) => job_ack.complete(InstallAck::Done(Err(error))),
            }
        });
        if let Err(error) = self.state.executor.execute(job) {
            shared.kill();
            return Err(ApplicationError::Dispatch(error.to_string()));
        }
        let outcome = ack.wait_cancellable(self.state.budget, cancellation, CANCELLATION_QUANTUM);
        let failure = match outcome {
            AckWait::Ready(InstallAck::Done(Ok(()))) if !cancellation.is_cancelled() => {
                return Ok(FrontmostWatch {
                    shared,
                    state: Arc::clone(&self.state),
                });
            }
            AckWait::Ready(InstallAck::Done(Ok(())) | InstallAck::Aborted) | AckWait::Cancelled => {
                ApplicationError::Cancelled
            }
            AckWait::Ready(InstallAck::Done(Err(error))) => error,
            AckWait::TimedOut => ApplicationError::TimedOut,
        };
        // Inert immediately; the observer (if registered) stays owned by this
        // observation and `stop`/drop removes it exactly once.
        shared.kill();
        let state = Arc::clone(&self.state);
        let _ = self.state.executor.execute(Box::new(move || {
            let _ = state.backend.unobserve_activation(state.handle, generation);
        }));
        Err(failure)
    }

    /// Re-queries the real frontmost application and compares it with the exact
    /// captured instance.
    ///
    /// Call this on the show path and again before graceful issuance. It never
    /// trusts the published snapshot or an activation epoch.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NotInstalled`] for a stopped generation, a
    /// dispatch rejection, a query failure or a timeout.
    pub fn preflight_frontmost(
        &self,
        application: ApplicationId,
    ) -> Result<FrontmostPreflight, ApplicationError> {
        if !self.owns(application) {
            return Ok(FrontmostPreflight::NotRunning);
        }
        self.query(move |backend, handle| preflight_on_main(backend, handle, application))
    }
}
