//! Cancellation, named workers and main-thread dispatch.
//!
//! Workers are owned by a [`WorkerGroup`]; a group must be cancelled and joined
//! before its callback data is released. The registry gives modules only a
//! [`WorkerHandle`], so modules can spawn named work but cannot detach it or
//! join it themselves. Main-thread dispatch runs inline on the main thread and
//! never blocks the caller while waiting for the main thread.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::{fmt, io};

use crate::lock;
use crate::shutdown::{ShutdownWait, wait_for_finish};

/// A one-shot cancellation flag that can be cloned cheaply and waited on.
///
/// Cancellation is irreversible. `wait` returns `true` for cancelled and
/// `false` for a timeout.
#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<CancelInner>,
}

#[derive(Default)]
struct CancelInner {
    cancelled: Mutex<bool>,
    signal: Condvar,
}

impl CancellationToken {
    /// Creates a token that is not cancelled.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Cancels the token and wakes every waiter. Repeated calls are no-ops.
    pub fn cancel(&self) {
        let mut cancelled = lock(&self.inner.cancelled);
        if !*cancelled {
            *cancelled = true;
            drop(cancelled);
            self.inner.signal.notify_all();
        }
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool { *lock(&self.inner.cancelled) }

    /// Waits until cancelled or the timeout elapses.
    ///
    /// Returns `true` when cancelled and `false` on timeout.
    #[must_use]
    pub fn wait(&self, timeout: Duration) -> bool {
        let cancelled = lock(&self.inner.cancelled);
        let (cancelled, _) = self
            .inner
            .signal
            .wait_timeout_while(cancelled, timeout, |cancelled| !*cancelled)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *cancelled
    }
}

/// A failure to schedule background or main-thread work.
#[derive(Debug)]
pub enum ThreadError {
    /// The group was already stopped, so it cannot accept new workers.
    GroupStopped,
    /// The operating system refused to create a thread.
    SpawnFailed(io::Error),
}

impl fmt::Display for ThreadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GroupStopped => f.write_str("the worker group is stopped"),
            Self::SpawnFailed(error) => write!(f, "failed to spawn worker: {error}"),
        }
    }
}

impl std::error::Error for ThreadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SpawnFailed(error) => Some(error),
            Self::GroupStopped => None,
        }
    }
}

/// The owning side of a set of named worker threads.
///
/// The owner is responsible for calling [`WorkerGroup::cancel_and_join`] before
/// dropping the group. Modules receive only a [`WorkerHandle`].
#[must_use]
pub struct WorkerGroup {
    inner: Arc<WorkerInner>,
}

/// A clonable, spawn-only handle into a [`WorkerGroup`].
///
/// A handle cannot stop or join the group, and spawning through it fails once
/// the owning group has stopped.
#[derive(Clone)]
pub struct WorkerHandle {
    inner: Arc<WorkerInner>,
}

struct WorkerInner {
    label: String,
    cancellation: CancellationToken,
    threads: Mutex<WorkerThreads>,
}

struct WorkerThreads {
    stopped: bool,
    handles: Vec<JoinHandle<()>>,
}

impl WorkerGroup {
    /// Creates an empty group whose workers share one cancellation token.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(WorkerInner {
                label: label.into(),
                cancellation: CancellationToken::new(),
                threads: Mutex::new(WorkerThreads {
                    stopped: false,
                    handles: Vec::new(),
                }),
            }),
        }
    }

    /// Returns a clonable handle that only allows spawning.
    #[must_use]
    pub fn handle(&self) -> WorkerHandle { WorkerHandle { inner: Arc::clone(&self.inner) } }

    /// The token shared by the group and every worker it spawns.
    #[must_use]
    pub fn cancellation(&self) -> CancellationToken { self.inner.cancellation.clone() }

    /// Spawns a named worker that receives the group token.
    ///
    /// The group lock is held while the thread is created and its handle is
    /// recorded, so a concurrent [`WorkerGroup::cancel_and_join`] can never
    /// miss a worker.
    ///
    /// # Errors
    /// Returns [`ThreadError::GroupStopped`] after the group stopped, or
    /// [`ThreadError::SpawnFailed`] when the OS rejects the thread.
    pub fn spawn(
        &self,
        name: &str,
        job: impl FnOnce(CancellationToken) + Send + 'static,
    ) -> Result<(), ThreadError> {
        spawn_worker(&self.inner, name, job)
    }

    /// Cancels the token, then joins every worker.
    ///
    /// This consumes the group: no worker can be added afterwards. Call it
    /// from a thread that is not one of this group's workers; joining the
    /// calling worker would wait forever. The registry invokes it off the main
    /// thread as part of stop and shutdown.
    pub fn cancel_and_join(self) {
        let handles = {
            let mut threads = lock(&self.inner.threads);
            threads.stopped = true;
            std::mem::take(&mut threads.handles)
        };
        self.inner.cancellation.cancel();
        for handle in handles {
            let _ = handle.join();
        }
    }

    /// Cancels the token, then joins every worker within the teardown bound.
    ///
    /// Each worker is awaited with [`wait_for_finish`], bounded by
    /// `min(now + OWNER_WAIT_BUDGET, deadline())`. Returns `true` when every
    /// worker finished inside the bound. On `false` the unfinished handles are
    /// dropped, so those workers are detached instead of joined: a detached
    /// worker keeps what it owns and must be inert by its own cancellation
    /// contract.
    pub fn cancel_and_join_bounded(self, wait: &dyn ShutdownWait) -> bool {
        let handles = {
            let mut threads = lock(&self.inner.threads);
            threads.stopped = true;
            std::mem::take(&mut threads.handles)
        };
        self.inner.cancellation.cancel();
        let mut all_finished = true;
        for handle in handles {
            if wait_for_finish(wait, &|| handle.is_finished()) {
                let _ = handle.join();
            } else {
                // Dropping the handle detaches the worker; never join it.
                all_finished = false;
            }
        }
        all_finished
    }
}

impl WorkerHandle {
    /// Spawns a named worker through the handle.
    ///
    /// # Errors
    /// Returns [`ThreadError`] under the same conditions as
    /// [`WorkerGroup::spawn`].
    pub fn spawn(
        &self,
        name: &str,
        job: impl FnOnce(CancellationToken) + Send + 'static,
    ) -> Result<(), ThreadError> {
        spawn_worker(&self.inner, name, job)
    }

    /// The token shared by the group and every worker it spawns.
    #[must_use]
    pub fn cancellation(&self) -> CancellationToken { self.inner.cancellation.clone() }
}

fn spawn_worker(
    inner: &Arc<WorkerInner>,
    name: &str,
    job: impl FnOnce(CancellationToken) + Send + 'static,
) -> Result<(), ThreadError> {
    let mut threads = lock(&inner.threads);
    if threads.stopped {
        return Err(ThreadError::GroupStopped);
    }
    let token = inner.cancellation.clone();
    let spawned = thread::Builder::new()
        .name(format!("{}-{name}", inner.label))
        .spawn(move || job(token))
        .map_err(ThreadError::SpawnFailed)?;
    threads.handles.push(spawned);
    drop(threads);
    Ok(())
}

/// Dispatch to the platform main thread without ever waiting for it.
///
/// Implementations run the job inline when `is_main_thread` is true. Off the
/// main thread they hand the job to the platform queue and return immediately.
pub trait MainThreadExecutor: Send + Sync {
    /// Whether the calling thread is the platform main thread.
    fn is_main_thread(&self) -> bool;

    /// Runs the job inline on the main thread, or enqueues it off-main.
    ///
    /// # Errors
    /// Returns [`ThreadError`] when the host cannot enqueue the job.
    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError>;
}

/// A test-only executor that runs jobs inline or queues them, with no platform
/// dependency. It is deliberately not exported outside tests.
#[cfg(test)]
struct FakeExecutor {
    on_main: bool,
    queued: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

#[cfg(test)]
impl FakeExecutor {
    fn new(on_main: bool) -> Self {
        Self {
            on_main,
            queued: Mutex::new(Vec::new()),
        }
    }

    fn drain(&self) {
        let jobs = std::mem::take(&mut *lock(&self.queued));
        for job in jobs {
            job();
        }
    }
}

#[cfg(test)]
impl MainThreadExecutor for FakeExecutor {
    fn is_main_thread(&self) -> bool { self.on_main }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        if self.on_main {
            job();
        } else {
            lock(&self.queued).push(job);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn cancel_flips_the_flag_and_repeated_cancel_is_harmless() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn wait_returns_true_when_cancelled_from_another_thread() {
        let token = CancellationToken::new();
        let waiter = token.clone();
        let (finished_tx, finished_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let cancelled = waiter.wait(Duration::from_secs(60));
            let _ = finished_tx.send(cancelled);
        });
        token.cancel();
        assert_eq!(finished_rx.recv_timeout(Duration::from_secs(5)), Ok(true));
        handle.join().expect("waiter");
    }

    #[test]
    fn wait_reports_timeout_without_cancellation() {
        let token = CancellationToken::new();
        assert!(!token.wait(Duration::from_millis(100)));
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancelled_token_wakes_immediately() {
        let token = CancellationToken::new();
        token.cancel();
        assert!(token.wait(Duration::from_secs(1)));
    }

    #[test]
    fn group_stop_cancels_and_joins_a_blocked_worker() {
        let group = WorkerGroup::new("test");
        let (ready_tx, ready_rx) = mpsc::channel();
        group
            .spawn("blocked", move |token| {
                let _ = ready_tx.send(());
                let _ = token.wait(Duration::from_secs(60));
            })
            .expect("spawn");
        ready_rx.recv_timeout(Duration::from_secs(5)).expect("worker started");
        group.cancel_and_join();
    }

    #[test]
    fn stopped_group_rejects_new_workers() {
        let group = WorkerGroup::new("test");
        let handle = group.handle();
        group.cancel_and_join();
        let error = handle.spawn("late", |_| {}).expect_err("stopped group must reject spawn");
        assert!(matches!(error, ThreadError::GroupStopped));
    }

    #[test]
    fn workers_share_the_group_token() {
        let group = WorkerGroup::new("test");
        let (tx, rx) = mpsc::channel();
        group
            .spawn("observe", move |token| {
                let cancelled = token.wait(Duration::from_secs(60));
                let _ = tx.send(cancelled);
            })
            .expect("spawn");
        group.cancellation().cancel();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(true));
        group.cancel_and_join();
    }

    #[test]
    fn panicking_worker_does_not_block_join() {
        let group = WorkerGroup::new("test");
        group.spawn("panic", |_| panic!("expected test panic")).expect("spawn");
        group.cancel_and_join();
    }

    #[test]
    fn main_thread_executor_runs_inline() {
        let executor = FakeExecutor::new(true);
        let counter = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&counter);
        executor
            .execute(Box::new(move || {
                observed.fetch_add(1, Ordering::SeqCst);
            }))
            .expect("execute");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert!(executor.queued.lock().expect("queue").is_empty());
    }

    #[test]
    fn off_main_executor_queues_without_waiting() {
        let executor = FakeExecutor::new(false);
        let counter = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&counter);
        executor
            .execute(Box::new(move || {
                observed.fetch_add(1, Ordering::SeqCst);
            }))
            .expect("execute");
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        assert!(!executor.is_main_thread());
        executor.drain();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn bounded_cancel_joins_a_worker_that_exits() {
        let group = WorkerGroup::new("test");
        let (ready_tx, ready_rx) = mpsc::channel();
        group
            .spawn("quick", move |token| {
                let _ = ready_tx.send(());
                let _ = token.wait(Duration::from_secs(60));
            })
            .expect("spawn");
        ready_rx.recv_timeout(Duration::from_secs(5)).expect("worker started");
        let wait = crate::shutdown::testing::FakeShutdownWait::with_budget(Duration::from_secs(2));
        assert!(group.cancel_and_join_bounded(&wait));
        assert!(!wait.overslept());
    }

    #[test]
    fn bounded_cancel_detaches_a_worker_that_ignores_cancellation() {
        let group = WorkerGroup::new("test");
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let exited = Arc::new(AtomicBool::new(false));
        let exited_in_worker = Arc::clone(&exited);
        group
            .spawn("stubborn", move |_| {
                let _ = ready_tx.send(());
                let _ = release_rx.recv();
                exited_in_worker.store(true, Ordering::SeqCst);
            })
            .expect("spawn");
        ready_rx.recv_timeout(Duration::from_secs(5)).expect("worker started");
        let wait = crate::shutdown::testing::FakeShutdownWait::with_budget(Duration::from_secs(2));
        wait.expire_now();
        assert!(!group.cancel_and_join_bounded(&wait));
        assert!(
            !exited.load(Ordering::SeqCst),
            "a detached worker must not be joined"
        );
        release.send(()).expect("release worker");
    }

    #[test]
    fn stopping_is_visible_to_already_running_workers() {
        let group = WorkerGroup::new("test");
        let handle = group.handle();
        let started = Arc::new(AtomicBool::new(false));
        let started_in_worker = Arc::clone(&started);
        group
            .spawn("flag", move |_| started_in_worker.store(true, Ordering::SeqCst))
            .expect("spawn");
        group.cancel_and_join();
        assert!(started.load(Ordering::SeqCst));
        assert!(matches!(
            handle.spawn("late", |_| {}),
            Err(ThreadError::GroupStopped)
        ));
    }
}
