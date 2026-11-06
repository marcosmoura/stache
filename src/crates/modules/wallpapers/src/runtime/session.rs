//! One rotation session's owned timer state.
//!
//! A session is created by `WallpapersModule::start` and owns the accepted
//! snapshot it was started from. Accepted publications are adopted only after a
//! matching `(revision, config)` pair is visible; until then, jobs use the last
//! coherent session state. The session owns a cancellation token and a wake
//! signal so `stop`, disable and interval changes wake the one timer without
//! waiting for its old deadline.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_sdk::{AcceptedConfig, CancellationToken};

use super::clock::RuntimeSignal;
use crate::WallpapersConfig;
use crate::sources::SourcePool;

struct SessionData {
    revision: u64,
    config: WallpapersConfig,
    pool: SourcePool,
    deadline: Option<Instant>,
    pending_positive_interval: bool,
    invalidated: bool,
}

/// One coherent session snapshot: revision, config and pool from one lock
/// acquisition.
pub(crate) struct SessionSnapshot {
    pub(crate) revision: u64,
    pub(crate) config: WallpapersConfig,
    pub(crate) pool: SourcePool,
}

/// One running rotation session.
pub(crate) struct SessionControl {
    id: u64,
    live: AtomicBool,
    signal: Arc<RuntimeSignal>,
    cancel: CancellationToken,
    data: Mutex<SessionData>,
    timer_owner: AtomicU64,
    next_timer: AtomicU64,
    full_success_resets: AtomicU64,
}

impl SessionControl {
    /// Creates a session from one coherent accepted snapshot.
    pub(crate) fn new(
        id: u64,
        pair: &AcceptedConfig<WallpapersConfig>,
        pool: SourcePool,
        signal: Arc<RuntimeSignal>,
        now: Instant,
    ) -> Self {
        let interval = pair.config.interval;
        Self {
            id,
            live: AtomicBool::new(true),
            signal,
            cancel: CancellationToken::new(),
            data: Mutex::new(SessionData {
                revision: pair.revision,
                config: (*pair.config).clone(),
                pool,
                deadline: interval_deadline(now, interval),
                pending_positive_interval: interval > 0,
                invalidated: false,
            }),
            timer_owner: AtomicU64::new(0),
            next_timer: AtomicU64::new(0),
            full_success_resets: AtomicU64::new(0),
        }
    }

    pub(crate) const fn id(&self) -> u64 { self.id }

    pub(crate) fn is_live(&self) -> bool { self.live.load(Ordering::SeqCst) }

    /// The token cancelled when this session is invalidated.
    pub(crate) fn cancellation(&self) -> CancellationToken { self.cancel.clone() }

    /// The session wake source. Timers wait on it; `stop`/disable wakes it.
    pub(crate) const fn signal(&self) -> &Arc<RuntimeSignal> { &self.signal }

    /// Invalidates the session and wakes its timer. Idempotent.
    pub(crate) fn invalidate(&self) {
        self.live.store(false, Ordering::SeqCst);
        self.cancel.cancel();
        lock(&self.data).invalidated = true;
        self.signal.stop();
    }

    pub(crate) fn revision(&self) -> u64 { lock(&self.data).revision }

    /// Reads the adopted revision, configuration and pool together.
    pub(crate) fn snapshot(&self) -> SessionSnapshot {
        let data = lock(&self.data);
        SessionSnapshot {
            revision: data.revision,
            config: data.config.clone(),
            pool: data.pool.clone(),
        }
    }

    pub(crate) fn interval(&self) -> u64 { lock(&self.data).config.interval }

    pub(crate) fn pending_positive_interval(&self) -> bool {
        lock(&self.data).pending_positive_interval
    }

    pub(crate) fn set_pending_positive_interval(&self, pending: bool) {
        lock(&self.data).pending_positive_interval = pending;
        self.signal.notify();
    }

    pub(crate) fn deadline(&self) -> Option<Instant> { lock(&self.data).deadline }

    /// Adopts a newly visible accepted pair and pool.
    ///
    /// The deadline is preserved when the interval is unchanged. An interval
    /// change resets it from `now` without applying an image; zero removes it.
    pub(crate) fn adopt(
        &self,
        pair: &AcceptedConfig<WallpapersConfig>,
        pool: SourcePool,
        now: Instant,
    ) {
        let mut data = lock(&self.data);
        let interval_changed = data.config.interval != pair.config.interval;
        data.revision = pair.revision;
        data.config = (*pair.config).clone();
        data.pool = pool;
        data.pending_positive_interval = pair.config.interval > 0;
        if interval_changed || (pair.config.interval > 0 && data.deadline.is_none()) {
            data.deadline = interval_deadline(now, pair.config.interval);
        }
        drop(data);
        self.signal.notify();
    }

    /// Records a complete known success and schedules the next interval from
    /// completion, only while `revision` is still the adopted revision and the
    /// caller's accepted/session predicate is still current.
    ///
    /// The revision, accepted predicate check and deadline write happen under
    /// the same session lock. A publication that is visible to the predicate
    /// therefore makes the reset inert, so an old completion cannot reset a
    /// successor revision's deadline.
    pub(crate) fn note_full_success_if(
        &self,
        now: Instant,
        revision: u64,
        current: &(dyn Fn() -> bool + Send + Sync),
    ) {
        if !self.is_live() {
            return;
        }
        let mut data = lock(&self.data);
        if data.invalidated || data.revision != revision || !current() {
            return;
        }
        if data.config.interval > 0 {
            data.deadline = interval_deadline(now, data.config.interval);
            self.full_success_resets.fetch_add(1, Ordering::SeqCst);
        }
        drop(data);
        self.signal.notify();
    }

    /// Schedules the next normal interval after an unsuccessful attempt, only
    /// while `revision` and the caller's accepted/session predicate are current.
    pub(crate) fn note_attempt_finished_if(
        &self,
        now: Instant,
        revision: u64,
        current: &(dyn Fn() -> bool + Send + Sync),
    ) {
        if !self.is_live() {
            return;
        }
        let mut data = lock(&self.data);
        if data.invalidated || data.revision != revision || !current() {
            return;
        }
        if data.config.interval > 0 {
            data.deadline = interval_deadline(now, data.config.interval);
        }
        drop(data);
        self.signal.notify();
    }

    pub(crate) fn timer_spawned(&self) -> bool { self.timer_owner.load(Ordering::SeqCst) != 0 }

    /// Claims the timer slot, returning a new owner id, or `None` when another
    /// worker already owns it.
    pub(crate) fn try_claim_timer(&self) -> Option<u64> {
        let id = self.next_timer.fetch_add(1, Ordering::SeqCst) + 1;
        self.timer_owner
            .compare_exchange(0, id, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| id)
    }

    /// Releases the timer slot only if `id` still owns it, so an exiting worker
    /// never clears a successor timer's marker.
    pub(crate) fn release_timer(&self, id: u64) {
        let _ = self.timer_owner.compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub(crate) fn full_success_resets(&self) -> u64 {
        self.full_success_resets.load(Ordering::SeqCst)
    }
}

fn interval_deadline(now: Instant, interval: u64) -> Option<Instant> {
    if interval == 0 {
        return None;
    }
    let duration = Duration::from_secs(interval);
    Some(
        now.checked_add(duration)
            .unwrap_or_else(|| now.checked_add(Duration::from_hours(87_600)).unwrap_or(now)),
    )
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
