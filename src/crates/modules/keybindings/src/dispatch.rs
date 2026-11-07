//! The bounded command dispatcher.
//!
//! A [`Dispatcher`] runs accepted shortcut command sequences through the part 01
//! [`CommandExecutor`]. Its limits are intentional safety bounds:
//!
//! * at most [`MAX_ACTIVE`] sequences run at once and at most [`MAX_WAITING`]
//!   more wait in FIFO admission order;
//! * independent presses, including repeated presses of one shortcut, run
//!   concurrently; only the steps inside one sequence are sequential and
//!   abort-on-failure;
//! * the newest submission is rejected at saturation and counted. Accepted work
//!   is never evicted and new work never bypasses a waiter;
//! * a sequence with no commands (an empty capture) bypasses command capacity.
//!
//! # Admission
//!
//! The dispatcher implements [`WorkReservations`], so the input router's
//! reservation and the dispatcher's acceptance are one decision. A reservation
//! counts against the same 80-sequence budget as queued and running work.
//! [`Dispatcher::submit_reserved`] converts it into a sequence atomically; a
//! reservation dropped unused releases its capacity. Carbon has no reservation
//! step and uses [`Dispatcher::submit`]; a rejection there is a lost invocation
//! the caller diagnoses, never a late execution.
//!
//! # Tasks
//!
//! Tasks are created lazily, one thread per active sequence, never idle. A
//! finishing task takes the next waiting sequence itself, so promotion is in
//! admission order and atomic with the slot being freed. Finished tasks are
//! reaped without blocking on the next submission.
//!
//! # Generations
//!
//! Every admitted sequence carries the generation it was admitted in.
//! [`Dispatcher::invalidate`] (pause, disable, stop, an accepted changed reload)
//! bumps the generation, discards waiting sequences and cancels every active
//! one, which kills and reaps only the directly spawned child through part 01.
//! The [`SpawnGate`] checks the generation under the same lock invalidation
//! takes, so no process starts after invalidation has won; a spawn already in
//! progress finishes and is then cancelled. A rejected or unchanged reload
//! simply does not call `invalidate`, and accepted work keeps running.
//!
//! # Retained ownership
//!
//! A child that cancellation could not confirm gone stays owned by the
//! executor's bounded reaper. When retained children plus active sequences
//! could use every reaper slot, new dispatch is refused with a diagnostic
//! instead of leaking ownership.
//!
//! The dispatcher never waits on the main thread, and its snapshot reads only
//! atomics.

// Critical sections hold the core lock on purpose: admission, promotion and
// invalidation must be atomic with the counters they publish.
#![allow(clippy::significant_drop_tightening)]

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use stache_sdk::commands::{
    CANCEL_CLEANUP, Cleanup, CommandExecutor, RETAINED_SLOTS, SequenceOutcome, SpawnGate,
};
use stache_sdk::input::{WorkReservation, WorkReservations};
use stache_sdk::threading::CancellationToken;

#[cfg(test)]
mod tests;

/// Sequences that may run at once.
pub const MAX_ACTIVE: usize = 16;
/// Sequences that may wait for a free slot.
pub const MAX_WAITING: usize = 64;
/// Every admitted or reserved sequence: running, waiting or reserved.
pub const CAPACITY: usize = MAX_ACTIVE + MAX_WAITING;
/// How long invalidation waits for a spawn already past the gate.
pub const SPAWN_DRAIN: Duration = Duration::from_millis(500);
/// How long shutdown waits for active tasks: the part 01 cleanup bound plus
/// a margin.
pub const SHUTDOWN_BOUND: Duration = CANCEL_CLEANUP.saturating_add(Duration::from_millis(500));

/// Receives dispatcher diagnostics (overflow, blocked dispatch, failures).
pub type DiagnosticSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Tunable bounds; the defaults are the shipped values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DispatcherConfig {
    /// Longest wait for an in-flight spawn during invalidation.
    pub spawn_drain: Duration,
    /// Longest wait for active tasks during shutdown.
    pub shutdown_bound: Duration,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            spawn_drain: SPAWN_DRAIN,
            shutdown_bound: SHUTDOWN_BOUND,
        }
    }
}

/// How a submitted sequence was accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// The sequence had no commands; no capacity was used.
    Bypassed,
    /// The sequence started running.
    Active,
    /// The sequence waits; `position` is its zero-based place in the queue.
    Queued {
        /// Zero-based queue position at admission.
        position: usize,
    },
}

/// Why a submission was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// All [`CAPACITY`] sequences are admitted or reserved.
    Full,
    /// Retained child ownership leaves no room for another sequence.
    Blocked {
        /// Children the reaper still owns.
        retained: usize,
    },
    /// The submission belongs to an invalidated generation.
    Stale,
    /// The dispatcher is shut down.
    Closed,
    /// The reservation belongs to another owner.
    ForeignReservation,
    /// The OS refused to create a task.
    Spawn(String),
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => write!(f, "the dispatcher is full ({CAPACITY} sequences)"),
            Self::Blocked { retained } => write!(
                f,
                "dispatch is blocked: {retained} unreaped children leave no ownership slot"
            ),
            Self::Stale => f.write_str("the submission belongs to an invalidated generation"),
            Self::Closed => f.write_str("the dispatcher is shut down"),
            Self::ForeignReservation => f.write_str("the reservation belongs to another owner"),
            Self::Spawn(error) => write!(f, "could not create a task: {error}"),
        }
    }
}

impl std::error::Error for Rejection {}

/// The result of [`Dispatcher::invalidate`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalidated {
    /// The new current generation.
    pub generation: u64,
    /// Waiting sequences discarded.
    pub discarded: usize,
    /// Whether every in-flight spawn finished inside the drain bound.
    pub drained: bool,
}

/// The result of [`Dispatcher::shutdown`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownReport {
    /// Tasks that did not finish inside the bound and were detached.
    pub detached: usize,
    /// Children the reaper still owns.
    pub retained: usize,
}

/// A lock-cheap, read-only view for status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DispatchSnapshot {
    /// The current generation.
    pub generation: u64,
    /// Sequences running now.
    pub active: usize,
    /// Sequences waiting for a slot.
    pub waiting: usize,
    /// Reservations taken and not yet converted or released.
    pub reserved: usize,
    /// Owned task threads, including finished ones not yet reaped.
    pub tasks: usize,
    /// Submissions refused because the dispatcher was full.
    pub overflow_drops: u64,
    /// Submissions refused because retained ownership blocked dispatch.
    pub blocked_drops: u64,
    /// Submissions refused as stale.
    pub stale_drops: u64,
    /// Waiting sequences discarded by invalidation.
    pub discarded: u64,
    /// Sequences that completed every step.
    pub completed: u64,
    /// Sequences that stopped at a failing step.
    pub failed: u64,
    /// Sequences stopped by cancellation.
    pub cancelled: u64,
    /// Children the reaper owns as of the last sweep.
    pub retained: usize,
    /// Cleanups that ever ended with a retained child.
    pub retained_total: u64,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct Job {
    commands: Arc<[String]>,
    generation: u64,
    /// The caller's currency tag, checked by the [`CurrencyGuard`] before
    /// every child is created.
    tag: Option<u64>,
    token: CancellationToken,
}

/// Answers whether work tagged with a value is still current.
///
/// The runtime tags each sequence with the accepted revision its binding came
/// from and answers from the accepted-configuration cell, so a queued or
/// active sequence starts no further child once a newer revision is
/// published, even before the runtime has observed it. It must be cheap and
/// non-blocking; it is called outside the dispatcher lock.
pub type CurrencyGuard = Arc<dyn Fn(u64) -> bool + Send + Sync>;

struct Core {
    generation: u64,
    token: CancellationToken,
    active: usize,
    waiting: VecDeque<Job>,
    reserved: usize,
    /// Spawns that passed the gate and have not finished creating a child.
    spawning: usize,
    closed: bool,
    next_task: u64,
    tasks: HashMap<u64, JoinHandle<()>>,
}

#[derive(Default)]
struct Counters {
    generation: AtomicU64,
    active: AtomicUsize,
    waiting: AtomicUsize,
    reserved: AtomicUsize,
    tasks: AtomicUsize,
    overflow: AtomicU64,
    blocked: AtomicU64,
    stale: AtomicU64,
    discarded: AtomicU64,
    completed: AtomicU64,
    failed: AtomicU64,
    cancelled: AtomicU64,
    retained: AtomicUsize,
    retained_total: AtomicU64,
}

struct Inner {
    executor: CommandExecutor,
    config: DispatcherConfig,
    diagnostics: DiagnosticSink,
    core: Mutex<Core>,
    sweeping: Mutex<()>,
    signal: Condvar,
    counters: Counters,
    currency: OnceLock<CurrencyGuard>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Inner {
    fn diagnose(&self, message: &str) { (self.diagnostics)(message); }

    fn publish(&self, core: &Core) {
        let counters = &self.counters;
        counters.generation.store(core.generation, Ordering::Release);
        counters.active.store(core.active, Ordering::Release);
        counters.waiting.store(core.waiting.len(), Ordering::Release);
        counters.reserved.store(core.reserved, Ordering::Release);
        counters.tasks.store(core.tasks.len(), Ordering::Release);
    }

    /// Sweeps the reaper and returns how many children it still owns.
    fn refresh_retained(&self) -> usize {
        // Serialized so a slower sweep never overwrites a newer count.
        let _serial = lock(&self.sweeping);
        let retained = self.executor.sweep().len();
        self.counters.retained.store(retained, Ordering::Release);
        retained
    }

    const fn blocked(retained: usize, core: &Core) -> bool {
        retained + core.active >= RETAINED_SLOTS
    }

    fn reap(&self) {
        let finished: Vec<JoinHandle<()>> = {
            let mut core = lock(&self.core);
            let mut ids = Vec::new();
            for (id, handle) in &core.tasks {
                if handle.is_finished() {
                    ids.push(*id);
                }
            }
            let finished = ids.into_iter().filter_map(|id| core.tasks.remove(&id)).collect();
            self.publish(&core);
            finished
        };
        for handle in finished {
            let _ = handle.join();
        }
    }

    fn note_cleanup(&self, cleanup: Option<&Cleanup>) {
        if let Some(Cleanup::Retained(message)) = cleanup {
            self.counters.retained_total.fetch_add(1, Ordering::Relaxed);
            self.diagnose(message);
        }
        self.refresh_retained();
    }

    fn run(self: &Arc<Self>, job: &Job) {
        let gate = GenerationGate {
            inner: self,
            generation: job.generation,
            tag: job.tag,
            token: job.token.clone(),
        };
        let outcome = self.executor.run_sequence(&job.commands, &job.token, &gate);
        let counters = &self.counters;
        match outcome {
            SequenceOutcome::Completed => {
                counters.completed.fetch_add(1, Ordering::Relaxed);
            }
            SequenceOutcome::Cancelled(cleanup) => {
                counters.cancelled.fetch_add(1, Ordering::Relaxed);
                self.note_cleanup(cleanup.as_ref());
            }
            SequenceOutcome::Failed { index, total, failure, cleanup } => {
                counters.failed.fetch_add(1, Ordering::Relaxed);
                self.diagnose(&format!(
                    "keybinding command {}/{total} failed: {failure}",
                    index + 1
                ));
                self.note_cleanup(cleanup.as_ref());
            }
        }
    }
}

impl WorkReservations for Inner {
    fn try_reserve(&self) -> bool {
        let retained = self.refresh_retained();
        let outcome = {
            let mut core = lock(&self.core);
            let outcome = if core.closed {
                Err(Rejection::Closed)
            } else if Self::blocked(retained, &core) {
                Err(Rejection::Blocked { retained })
            } else if core.active + core.waiting.len() + core.reserved >= CAPACITY {
                Err(Rejection::Full)
            } else {
                core.reserved += 1;
                Ok(())
            };
            self.publish(&core);
            outcome
        };
        match outcome {
            Ok(()) => true,
            Err(rejection) => {
                self.count_rejection(&rejection);
                false
            }
        }
    }

    fn release(&self) {
        let mut core = lock(&self.core);
        core.reserved = core.reserved.saturating_sub(1);
        self.publish(&core);
    }
}

impl Inner {
    fn count_rejection(&self, rejection: &Rejection) {
        let counter = match rejection {
            Rejection::Full => &self.counters.overflow,
            Rejection::Blocked { .. } => &self.counters.blocked,
            Rejection::Stale => &self.counters.stale,
            Rejection::Closed | Rejection::ForeignReservation | Rejection::Spawn(_) => {
                self.diagnose(&format!("keybinding dispatch refused: {rejection}"));
                return;
            }
        };
        counter.fetch_add(1, Ordering::Relaxed);
        self.diagnose(&format!("keybinding dispatch refused: {rejection}"));
    }
}

/// Checks a sequence's generation at the moment a child is created.
struct GenerationGate<'a> {
    inner: &'a Arc<Inner>,
    generation: u64,
    tag: Option<u64>,
    token: CancellationToken,
}

struct SpawnPermit<'a> {
    inner: &'a Inner,
}

impl Drop for SpawnPermit<'_> {
    fn drop(&mut self) {
        let mut core = lock(&self.inner.core);
        core.spawning = core.spawning.saturating_sub(1);
        drop(core);
        self.inner.signal.notify_all();
    }
}

impl SpawnGate for GenerationGate<'_> {
    fn admit(&self) -> Option<Box<dyn Send + '_>> {
        if let (Some(tag), Some(current)) = (self.tag, self.inner.currency.get())
            && !current(tag)
        {
            return None;
        }
        let mut core = lock(&self.inner.core);
        if core.closed || core.generation != self.generation || self.token.is_cancelled() {
            return None;
        }
        core.spawning += 1;
        drop(core);
        Some(Box::new(SpawnPermit { inner: self.inner }))
    }
}

fn worker(inner: &Arc<Inner>, first: Job) {
    let mut job = first;
    loop {
        if catch_unwind(AssertUnwindSafe(|| inner.run(&job))).is_err() {
            inner.counters.failed.fetch_add(1, Ordering::Relaxed);
            inner.diagnose("a keybinding task panicked; its sequence was abandoned");
        }
        let mut core = lock(&inner.core);
        if let Some(next) = core.waiting.pop_front() {
            inner.publish(&core);
            drop(core);
            job = next;
        } else {
            core.active = core.active.saturating_sub(1);
            inner.publish(&core);
            drop(core);
            inner.signal.notify_all();
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// The bounded, generation-aware command dispatcher.
///
/// Cloning shares one dispatcher. Call [`Dispatcher::shutdown`] before the last
/// handle is dropped; dropped handles never cancel running work.
#[derive(Clone)]
pub struct Dispatcher {
    inner: Arc<Inner>,
}

impl fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dispatcher").field("snapshot", &self.snapshot()).finish()
    }
}

impl Dispatcher {
    /// A dispatcher over `executor` with the shipped bounds. No task exists
    /// until work is admitted.
    #[must_use]
    pub fn new(executor: CommandExecutor, diagnostics: DiagnosticSink) -> Self {
        Self::with_config(executor, DispatcherConfig::default(), diagnostics)
    }

    /// A dispatcher with explicit bounds.
    #[must_use]
    pub fn with_config(
        executor: CommandExecutor,
        config: DispatcherConfig,
        diagnostics: DiagnosticSink,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                executor,
                config,
                diagnostics,
                core: Mutex::new(Core {
                    generation: 0,
                    token: CancellationToken::new(),
                    active: 0,
                    waiting: VecDeque::new(),
                    reserved: 0,
                    spawning: 0,
                    closed: false,
                    next_task: 0,
                    tasks: HashMap::new(),
                }),
                sweeping: Mutex::new(()),
                signal: Condvar::new(),
                counters: Counters::default(),
                currency: OnceLock::new(),
            }),
        }
    }

    /// The reservation owner to hand to the input router. Reservations taken
    /// from it share this dispatcher's one capacity budget.
    #[must_use]
    pub fn reservations(&self) -> Arc<dyn WorkReservations> {
        Arc::clone(&self.inner) as Arc<dyn WorkReservations>
    }

    /// Installs the [`CurrencyGuard`] for tagged work. Only the first install
    /// takes effect; returns whether this one did.
    pub fn set_currency_guard(&self, guard: CurrencyGuard) -> bool {
        self.inner.currency.set(guard).is_ok()
    }

    /// Submits a shared sequence only if `generation` is still current, tagged
    /// with `tag` for the [`CurrencyGuard`]. The commands are shared, not
    /// copied, so a callback does not clone them.
    ///
    /// # Errors
    /// Returns the [`Rejection`]; it is also counted and diagnosed.
    pub fn submit_tagged(
        &self,
        generation: u64,
        tag: u64,
        commands: &Arc<[String]>,
    ) -> Result<Admission, Rejection> {
        self.submit_inner(Some(generation), Some(tag), None, Arc::clone(commands))
    }

    /// The current generation. A registration captures it so a press from an
    /// invalidated generation is refused by [`Dispatcher::submit_at`].
    #[must_use]
    pub fn generation(&self) -> u64 { self.inner.counters.generation.load(Ordering::Acquire) }

    /// Submits a sequence in the current generation without a reservation.
    ///
    /// # Errors
    /// Returns the [`Rejection`]; it is also counted and diagnosed.
    pub fn submit(&self, commands: &[String]) -> Result<Admission, Rejection> {
        self.submit_inner(None, None, None, commands.into())
    }

    /// Submits a sequence only if `generation` is still current.
    ///
    /// # Errors
    /// Returns [`Rejection::Stale`] after an invalidation, or another
    /// [`Rejection`].
    pub fn submit_at(&self, generation: u64, commands: &[String]) -> Result<Admission, Rejection> {
        self.submit_inner(Some(generation), None, None, commands.into())
    }

    /// Converts a reservation into a sequence. The reservation's capacity
    /// becomes the sequence's capacity; on rejection it is released.
    ///
    /// # Errors
    /// Returns the [`Rejection`]; it is also counted and diagnosed.
    pub fn submit_reserved(
        &self,
        reservation: WorkReservation,
        generation: Option<u64>,
        commands: &[String],
    ) -> Result<Admission, Rejection> {
        self.submit_inner(generation, None, Some(reservation), commands.into())
    }

    fn submit_inner(
        &self,
        generation: Option<u64>,
        tag: Option<u64>,
        reservation: Option<WorkReservation>,
        commands: Arc<[String]>,
    ) -> Result<Admission, Rejection> {
        let inner = &self.inner;
        if let Some(reservation) = &reservation
            && !reservation.is_owned_by(&self.reservations())
        {
            inner.count_rejection(&Rejection::ForeignReservation);
            return Err(Rejection::ForeignReservation);
        }
        inner.reap();
        let retained = inner.refresh_retained();
        let converting = reservation.is_some();
        let result = {
            let mut core = lock(&inner.core);
            if converting {
                core.reserved = core.reserved.saturating_sub(1);
            }
            let result = self.admit(&mut core, generation, tag, converting, retained, commands);
            inner.publish(&core);
            result
        };
        if let Some(reservation) = reservation {
            // The slot was moved out of `reserved` under the lock above, so the
            // reservation is consumed without a second release. Ownership was
            // checked on entry; a mismatch here would be an internal bug.
            let redeemed = reservation.redeem(&self.reservations());
            debug_assert!(redeemed.is_ok(), "reservation ownership changed");
        }
        if let Err(rejection) = &result {
            inner.count_rejection(rejection);
        }
        result
    }

    fn admit(
        &self,
        core: &mut Core,
        generation: Option<u64>,
        tag: Option<u64>,
        converting: bool,
        retained: usize,
        commands: Arc<[String]>,
    ) -> Result<Admission, Rejection> {
        let inner = &self.inner;
        if commands.is_empty() {
            return Ok(Admission::Bypassed);
        }
        if core.closed {
            return Err(Rejection::Closed);
        }
        if generation.is_some_and(|generation| generation != core.generation) {
            return Err(Rejection::Stale);
        }
        if Inner::blocked(retained, core) {
            return Err(Rejection::Blocked { retained });
        }
        // A reservation already holds its share of the budget.
        if !converting && core.active + core.waiting.len() + core.reserved >= CAPACITY {
            return Err(Rejection::Full);
        }
        let job = Job {
            commands,
            generation: core.generation,
            tag,
            token: core.token.clone(),
        };
        if core.active < MAX_ACTIVE {
            let id = core.next_task;
            let task_inner = Arc::clone(inner);
            let handle = thread::Builder::new()
                .name(format!("stache-keybinding-{id}"))
                .spawn(move || worker(&task_inner, job))
                .map_err(|error| Rejection::Spawn(error.to_string()))?;
            core.next_task += 1;
            core.active += 1;
            core.tasks.insert(id, handle);
            Ok(Admission::Active)
        } else if core.waiting.len() < MAX_WAITING {
            core.waiting.push_back(job);
            Ok(Admission::Queued {
                position: core.waiting.len() - 1,
            })
        } else {
            Err(Rejection::Full)
        }
    }

    /// Invalidates the current generation: pause, disable, stop or an accepted
    /// changed reload.
    ///
    /// Waiting sequences are discarded and every active sequence is cancelled,
    /// which kills and reaps only its directly spawned child. After the
    /// generation bump no new process starts for the old generation. Waits at
    /// most [`DispatcherConfig::spawn_drain`] for a spawn already past the gate
    /// and never waits for children or the main thread.
    ///
    /// A rejected or unchanged reload must not call this.
    pub fn invalidate(&self) -> Invalidated {
        let inner = &self.inner;
        let (old, discarded, generation) = {
            let mut core = lock(&inner.core);
            core.generation += 1;
            let old = std::mem::replace(&mut core.token, CancellationToken::new());
            let discarded = core.waiting.len();
            core.waiting.clear();
            inner.publish(&core);
            (old, discarded, core.generation)
        };
        old.cancel();
        inner.counters.discarded.fetch_add(discarded as u64, Ordering::Relaxed);
        let core = lock(&inner.core);
        let (_core, timeout) = inner
            .signal
            .wait_timeout_while(core, inner.config.spawn_drain, |core| core.spawning > 0)
            .unwrap_or_else(PoisonError::into_inner);
        Invalidated {
            generation,
            discarded,
            drained: !timeout.timed_out(),
        }
    }

    /// Joins finished tasks without blocking.
    pub fn reap(&self) { self.inner.reap(); }

    /// A read-only view. Reads only atomics and never waits on a worker.
    #[must_use]
    pub fn snapshot(&self) -> DispatchSnapshot {
        let counters = &self.inner.counters;
        DispatchSnapshot {
            generation: counters.generation.load(Ordering::Acquire),
            active: counters.active.load(Ordering::Acquire),
            waiting: counters.waiting.load(Ordering::Acquire),
            reserved: counters.reserved.load(Ordering::Acquire),
            tasks: counters.tasks.load(Ordering::Acquire),
            overflow_drops: counters.overflow.load(Ordering::Relaxed),
            blocked_drops: counters.blocked.load(Ordering::Relaxed),
            stale_drops: counters.stale.load(Ordering::Relaxed),
            discarded: counters.discarded.load(Ordering::Relaxed),
            completed: counters.completed.load(Ordering::Relaxed),
            failed: counters.failed.load(Ordering::Relaxed),
            cancelled: counters.cancelled.load(Ordering::Relaxed),
            retained: counters.retained.load(Ordering::Acquire),
            retained_total: counters.retained_total.load(Ordering::Relaxed),
        }
    }

    /// Closes the dispatcher, invalidates the generation and waits at most
    /// [`DispatcherConfig::shutdown_bound`] for tasks to finish cleanup.
    ///
    /// Tasks that overrun the bound are detached, never joined, so a hung
    /// task cannot block host shutdown; children the reaper owns stay owned.
    /// Call it from a thread that is not the main thread.
    pub fn shutdown(&self) -> ShutdownReport {
        let inner = &self.inner;
        lock(&inner.core).closed = true;
        let _ = self.invalidate();
        let core = lock(&inner.core);
        let (mut core, _) = inner
            .signal
            .wait_timeout_while(core, inner.config.shutdown_bound, |core| core.active > 0)
            .unwrap_or_else(PoisonError::into_inner);
        let tasks = std::mem::take(&mut core.tasks);
        inner.publish(&core);
        drop(core);
        let mut detached = 0;
        for (_, handle) in tasks {
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                // Dropping the handle detaches the task; it is inert by its
                // cancellation contract.
                detached += 1;
            }
        }
        inner.counters.tasks.store(detached, Ordering::Release);
        ShutdownReport {
            detached,
            retained: inner.refresh_retained(),
        }
    }
}
