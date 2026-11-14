//! The caller-facing handle to the per-application AX owner shards.
//!
//! ```text
//! consumers (any thread, never main-blocking)
//!    └─ WindowAdapter (router: no thread, no AX call)
//!         ├─ "<prefix>-0" ... "<prefix>-31"   one dedicated shard per application
//!         └─ "<prefix>-overflow"              every application beyond the cap
//!              each shard: bounded SyncSender<Command> + Wakeup ──> thread
//!                 ├─ WindowSystem (created in-thread)
//!                 ├─ AxObserver + run loop of its application(s)
//!                 └─ retained window elements
//!              each shard also: latest-frame-wins mailbox (shared with the router)
//! ```
//!
//! The handle is `Send + Sync` and holds no native object. A shard is today's
//! owner [`Core`] on its own thread. The router mints the runtime epoch,
//! installs it on each shard together with shared serial and event-sequence
//! counters, and routes every command by application, so one slow application
//! never delays another. Its state lives behind a mutex that is never held
//! across a send to a shard, a wait or a reply callback. Blocking helpers wait
//! at most [`ACKNOWLEDGEMENT_BUDGET`] per hop, capped by the caller's remaining
//! deadline (one shared deadline, never multiplied), and observe cancellation
//! every [`CANCELLATION_QUANTUM`]. Never call a blocking helper from the main
//! thread; the asynchronous `submit_*` forms are main-safe. No shard waits on
//! main, so no main -> owner -> main wait exists.
//!
//! Frame writes take a second path: [`WindowAdapter::submit_frames`] inserts a
//! whole per-application batch into the shard's bounded mailbox under one lock
//! (at most one pending frame per window; a newer frame of the same plan
//! replaces the pending one, a newer plan supersedes it, an older plan is
//! refused) and signals one wakeup. The mailbox lock is held only to insert or
//! take, never across an AX call or a reply. Every frame reply runs exactly
//! once. [`WindowAdapter::submit_snapshot`] is the non-blocking `snapshot`.
//!
//! No shard exists until the first `track_application`: an adapter that is
//! created, or an epoch that is installed, owns zero shard threads.

#![allow(
    clippy::significant_drop_tightening,
    reason = "each router guard covers a short block of map reads; the whole point is that no send or wait happens inside"
)]

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use stache_sdk::CancellationToken;

use super::commands::{
    ActionRequest, CleanupReport, CloseOutcome, Command, FocusOutcome, FrameReadOutcome,
    FrameReplyFn, FrameRequest, HiddenOutcome, HiddenRequest, InstallError, InstallRequest,
    LeaseRestoreOutcome, Reply, Resolution, SnapshotOutcome, StopOutcome, TrackOutcome,
    TrackRequest, WindowAction,
};
use super::core::{Core, EpochCounters};
use super::driver::{Driver, RunLoopDriver, Wakeup};
use super::frames::{self, FrameCallback, FrameMailbox, done};
use super::identity::{AdapterId, RuntimeEpoch, WindowInstance};
use super::issuance::{
    DispatchError, GenerationCell, IssuanceCancel, NotIssuedReason, StaleReason,
    WindowActionOutcome, WindowIssuance,
};
use super::lease::{SystemVoiceOverProbe, VoiceOverProbe};
use super::system::{SystemWindowClock, SystemWindows, WindowClock, WindowSystem};
use crate::applications::{ACKNOWLEDGEMENT_BUDGET, ApplicationId, CANCELLATION_QUANTUM};
use crate::menu::OWNER_THREAD_NAME as MENU_ANYWHERE_THREAD_NAME;

/// The most dedicated (one application each) shards an adapter runs. Further
/// applications share one overflow shard.
pub const MAX_APPLICATION_SHARDS: usize = 32;

/// The most commands queued for the owner.
pub const OWNER_QUEUE_BOUND: usize = 64;

/// Why the owner thread could not start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpawnError {
    /// The name is empty.
    EmptyThreadName,
    /// The name is `MenuAnywhere`'s `stache-ax-owner`: AX owners are never shared.
    ReservedThreadName,
    /// Another live window adapter already uses this name.
    ThreadNameInUse,
    /// The operating system refused the thread.
    Thread,
}

fn used_names() -> &'static Mutex<HashSet<String>> {
    static NAMES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    NAMES.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Releases a thread name when the owner thread ends.
struct NameGuard(String);

impl NameGuard {
    fn claim(name: &str) -> Result<Self, SpawnError> {
        if name.is_empty() {
            return Err(SpawnError::EmptyThreadName);
        }
        if name == MENU_ANYWHERE_THREAD_NAME {
            return Err(SpawnError::ReservedThreadName);
        }
        let mut names = used_names().lock().unwrap_or_else(PoisonError::into_inner);
        if !names.insert(name.to_owned()) {
            return Err(SpawnError::ThreadNameInUse);
        }
        drop(names);
        Ok(Self(name.to_owned()))
    }
}

impl Drop for NameGuard {
    fn drop(&mut self) {
        used_names().lock().unwrap_or_else(PoisonError::into_inner).remove(&self.0);
    }
}

/// Options for [`WindowAdapter::spawn`].
#[derive(Clone, Debug, Default)]
pub struct AdapterOptions {
    /// Read tab groups into snapshots (extra `AX` reads). Off by default:
    /// snapshots then report [`TabInfo::Unknown`](super::TabInfo::Unknown). A
    /// failed read reports [`TabInfo::Failed`](super::TabInfo::Failed).
    pub read_tabs: bool,
    /// Read each window's native tab bar identity (a bounded search for its
    /// `AXTabGroup` on the owner thread). Tiling uses it to keep a tab group's
    /// slot when `AppKit` swaps the window that represents the group.
    pub read_tab_bars: bool,
    /// Decides whether `VoiceOver` is running, which keeps the
    /// `AXEnhancedUserInterface` lease off. [`WindowAdapter::system`] fills in
    /// the system probe when this is `None`; [`WindowAdapter::spawn`] treats
    /// `None` as "never running", so fake-driven adapters never read the
    /// system preferences.
    pub voice_over: Option<Arc<dyn VoiceOverProbe>>,
}

/// The wait one hop may take: the acknowledgement budget capped by the
/// command's remaining deadline.
#[must_use]
pub fn hop_wait(now: Instant, deadline: Option<Instant>) -> Duration {
    deadline.map_or(ACKNOWLEDGEMENT_BUDGET, |deadline| {
        ACKNOWLEDGEMENT_BUDGET.min(deadline.saturating_duration_since(now))
    })
}

enum Waited<T> {
    Got(T),
    TimedOut,
    Cancelled,
    Gone,
}

fn wait_reply<T>(
    receiver: &Receiver<T>,
    wait: Duration,
    cancel: Option<&CancellationToken>,
) -> Waited<T> {
    let end = Instant::now() + wait;
    loop {
        let remaining = end.saturating_duration_since(Instant::now());
        let slice = if cancel.is_some() {
            remaining.min(CANCELLATION_QUANTUM)
        } else {
            remaining
        };
        match receiver.recv_timeout(slice) {
            Ok(value) => return Waited::Got(value),
            Err(RecvTimeoutError::Disconnected) => return Waited::Gone,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Waited::Cancelled;
        }
        if Instant::now() >= end {
            return Waited::TimedOut;
        }
    }
}

/// Which kind of shard a thread is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShardKind {
    /// Hosts exactly one application; the number is its name suffix.
    Dedicated(usize),
    /// Hosts every application beyond the dedicated cap.
    Overflow,
}

impl ShardKind {
    fn name(self, prefix: &str) -> String {
        match self {
            Self::Dedicated(slot) => format!("{prefix}-{slot}"),
            Self::Overflow => format!("{prefix}-overflow"),
        }
    }

    const fn order(self) -> usize {
        match self {
            Self::Dedicated(slot) => slot,
            Self::Overflow => usize::MAX,
        }
    }
}

pub(super) struct Thread {
    done: Receiver<()>,
    join: JoinHandle<()>,
}

impl Thread {
    #[cfg(test)]
    pub(super) const fn new(done: Receiver<()>, join: JoinHandle<()>) -> Self {
        Self { done, join }
    }
}

enum ThreadState {
    Running(Thread),
    /// One caller is waiting for the thread; no other caller may report it
    /// joined.
    Joining,
    Joined,
}

/// The owner thread's join state. Only the caller that actually joined the
/// thread reports success; a concurrent caller sees it still joining.
pub(super) struct JoinSlot(Mutex<ThreadState>);

impl JoinSlot {
    pub(super) const fn new(thread: Thread) -> Self {
        Self(Mutex::new(ThreadState::Running(thread)))
    }

    /// Waits until `end`. `true` only when the thread is gone.
    pub(super) fn join(&self, end: Instant) -> bool {
        let thread = {
            let mut state = locked(&self.0);
            match std::mem::replace(&mut *state, ThreadState::Joining) {
                ThreadState::Running(thread) => thread,
                ThreadState::Joining => return false,
                ThreadState::Joined => {
                    *state = ThreadState::Joined;
                    return true;
                }
            }
        };
        match thread.done.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = thread.join.join();
                *locked(&self.0) = ThreadState::Joined;
                true
            }
            Err(RecvTimeoutError::Timeout) => {
                *locked(&self.0) = ThreadState::Running(thread);
                false
            }
        }
    }
}

/// One AX owner thread: today's [`Core`] behind a bounded command queue.
struct Shard {
    kind: ShardKind,
    sender: Mutex<Option<SyncSender<Command>>>,
    wakeup: Arc<Wakeup>,
    frames: Arc<Mutex<FrameMailbox>>,
    thread: JoinSlot,
}

type Spawner = Box<dyn Fn(ShardKind, NameGuard) -> Result<Shard, SpawnError> + Send + Sync>;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shard {
    fn submit(&self, command: Command) -> Result<(), DispatchError> {
        let guard = locked(&self.sender);
        let sender = guard.as_ref().ok_or(DispatchError::Closed)?;
        let sent = sender.try_send(command);
        drop(guard);
        match sent {
            Ok(()) => {
                self.wakeup.signal();
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(DispatchError::Full),
            Err(TrySendError::Disconnected(_)) => Err(DispatchError::Closed),
        }
    }

    /// Inserts a frame batch under one lock, then signals one wakeup.
    fn submit_frames(
        &self,
        batch: Vec<(FrameRequest, FrameCallback)>,
    ) -> Result<(), DispatchError> {
        frames::submit(&self.frames, batch)?;
        self.wakeup.signal();
        Ok(())
    }

    /// Queues `build(reply)` and returns the reply receiver.
    fn ask<T: Send + 'static>(
        &self,
        build: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<Receiver<T>, DispatchError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.submit(build(Box::new(move |value| {
            let _ = tx.send(value);
        })))?;
        Ok(rx)
    }

    /// Queues `build(reply)` and waits one hop, observing `cancel` every
    /// [`CANCELLATION_QUANTUM`].
    fn request<T: Send + 'static>(
        &self,
        deadline: Option<Instant>,
        cancel: Option<&CancellationToken>,
        build: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, RequestError> {
        let rx = self.ask(build).map_err(RequestError::Dispatch)?;
        await_reply(&rx, Instant::now() + hop_wait(Instant::now(), deadline), cancel)
    }

    /// Joins the finished thread, waiting until `end`. `true` when it is gone.
    fn join(&self, end: Instant) -> bool {
        locked(&self.sender).take();
        self.thread.join(end)
    }

    /// Whether the command channel is gone (the shard was told to end).
    fn is_detached(&self) -> bool { locked(&self.sender).is_none() }
}

impl Drop for Shard {
    /// Dropping the handle ends the owner loop: the closed channel is observed
    /// on the next wakeup, which stops the epoch and releases every element on
    /// the owner thread. It never blocks.
    fn drop(&mut self) {
        locked(&self.sender).take();
        self.wakeup.signal();
    }
}

fn await_reply<T>(
    receiver: &Receiver<T>,
    end: Instant,
    cancel: Option<&CancellationToken>,
) -> Result<T, RequestError> {
    match wait_reply(receiver, end.saturating_duration_since(Instant::now()), cancel) {
        Waited::Got(value) => Ok(value),
        Waited::TimedOut | Waited::Cancelled => Err(RequestError::Unacknowledged),
        Waited::Gone => Err(RequestError::Unavailable),
    }
}

/// Sends one command to every shard, then waits for all replies under one
/// shared `end`.
fn fan_out<T: Send + 'static>(
    shards: &[Arc<Shard>],
    end: Instant,
    cancel: Option<&CancellationToken>,
    build: impl Fn(Reply<T>) -> Command,
) -> Vec<Result<T, RequestError>> {
    let pending: Vec<_> = shards.iter().map(|shard| shard.ask(&build)).collect();
    pending
        .into_iter()
        .map(|queued| match queued {
            Ok(rx) => await_reply(&rx, end, cancel),
            Err(error) => Err(RequestError::Dispatch(error)),
        })
        .collect()
}

/// A shard that stays alive holding quarantined or unconfirmed resources.
struct Retired {
    shard: Arc<Shard>,
    applications: usize,
    registrations: usize,
}

/// What an install attached to the running epoch.
struct Installed {
    epoch: RuntimeEpoch,
    request: InstallRequest,
    counters: EpochCounters,
}

#[derive(Default)]
struct State {
    next_epoch: u64,
    installed: Option<Installed>,
    /// The shard hosting each tracked application.
    by_app: HashMap<ApplicationId, Arc<Shard>>,
    /// Dedicated name slots in use (live and retired shards).
    slots: BTreeSet<usize>,
    overflow: Option<Arc<Shard>>,
    retired: Vec<Retired>,
    closed: bool,
}

impl State {
    /// Every live shard once, dedicated by slot then overflow.
    fn live_shards(&self) -> Vec<Arc<Shard>> {
        let mut shards: Vec<_> = self.by_app.values().cloned().collect();
        shards.extend(self.overflow.iter().cloned());
        shards.sort_by_key(|shard| shard.kind.order());
        shards.dedup_by(|a, b| Arc::ptr_eq(a, b));
        shards
    }

    fn quarantine_totals(&self) -> (usize, usize) {
        self.retired.iter().fold((0, 0), |(apps, regs), held| {
            (apps + held.applications, regs + held.registrations)
        })
    }
}

/// Diagnostics about the shard layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShardStats {
    /// Live dedicated shards (one application each).
    pub dedicated: usize,
    /// Applications sharing the overflow shard.
    pub overflow_applications: usize,
    /// Shards kept alive holding quarantined or unconfirmed resources.
    pub quarantined: usize,
}

/// A handle to one window-ownership adapter: a router over application shards.
pub struct WindowAdapter {
    id: AdapterId,
    clock: Arc<dyn WindowClock>,
    spawner: Spawner,
    max_dedicated: usize,
    state: Mutex<State>,
    /// Serializes shard creation and the epoch-wide teardown so a stop never
    /// misses a shard that is being created. Held across a fresh shard's
    /// spawn and install only, never across a reply wait on a tracked shard.
    lifecycle: Mutex<()>,
    prefix_guard: Mutex<Option<NameGuard>>,
    prefix: String,
    cleanup_unconfirmed: AtomicBool,
}

impl WindowAdapter {
    /// Creates an adapter over the real AX bindings. Shard threads are named
    /// `<thread_prefix>-<n>` and `<thread_prefix>-overflow`.
    ///
    /// # Errors
    ///
    /// [`SpawnError`] for a reserved, empty or duplicate name.
    pub fn system(thread_prefix: &str, options: &AdapterOptions) -> Result<Self, SpawnError> {
        let mut options = options.clone();
        if options.voice_over.is_none() {
            options.voice_over = Some(Arc::new(SystemVoiceOverProbe::new()));
        }
        Self::spawn(
            thread_prefix,
            &options,
            Arc::new(SystemWindowClock),
            SystemWindows::new,
            RunLoopDriver::new,
        )
    }

    /// Creates an adapter. It spawns no thread: a shard starts on the first
    /// tracked application, and `system` and `driver` are called **on** each
    /// shard thread, so they may build `!Send` native objects.
    ///
    /// # Errors
    ///
    /// [`SpawnError`] for a reserved, empty or duplicate prefix.
    pub fn spawn<S, D>(
        thread_prefix: &str,
        options: &AdapterOptions,
        clock: Arc<dyn WindowClock>,
        system: impl Fn() -> S + Send + Sync + 'static,
        driver: impl Fn() -> D + Send + Sync + 'static,
    ) -> Result<Self, SpawnError>
    where
        S: WindowSystem + 'static,
        D: Driver + 'static,
    {
        let guard = NameGuard::claim(thread_prefix)?;
        let id = AdapterId::allocate();
        let system = Arc::new(system);
        let driver = Arc::new(driver);
        let shard_clock = Arc::clone(&clock);
        let prefix = thread_prefix.to_owned();
        let name_prefix = prefix.clone();
        let read_tabs = options.read_tabs;
        let read_tab_bars = options.read_tab_bars;
        let voice_over: Option<Arc<dyn VoiceOverProbe>> = options.voice_over.clone();
        let spawner: Spawner = Box::new(move |kind, name_guard| {
            let (sender, receiver) = mpsc::sync_channel::<Command>(OWNER_QUEUE_BOUND);
            let (ready_tx, ready_rx) = mpsc::sync_channel::<Arc<Wakeup>>(1);
            let frames = Arc::new(Mutex::new(FrameMailbox::default()));
            let owner_frames = Arc::clone(&frames);
            let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
            let system = Arc::clone(&system);
            let driver = Arc::clone(&driver);
            let clock = Arc::clone(&shard_clock);
            let voice_over = voice_over.clone();
            let join = thread::Builder::new()
                .name(kind.name(&name_prefix))
                .spawn(move || {
                    let _guard = name_guard;
                    let driver = driver();
                    let wakeup = Arc::new(Wakeup::new(driver.waker()));
                    let mut core = Core::new(system(), id, clock, read_tabs)
                        .with_tab_bars(read_tab_bars)
                        .with_frames(owner_frames);
                    if let Some(probe) = &voice_over {
                        core = core.with_voice_over(Arc::clone(probe));
                    }
                    let _ = ready_tx.send(Arc::clone(&wakeup));
                    run_owner(core, &receiver, driver, &wakeup);
                    let _ = done_tx.send(());
                })
                .map_err(|_| SpawnError::Thread)?;
            let wakeup =
                ready_rx.recv_timeout(ACKNOWLEDGEMENT_BUDGET).map_err(|_| SpawnError::Thread)?;
            Ok(Shard {
                kind,
                sender: Mutex::new(Some(sender)),
                wakeup,
                frames,
                thread: JoinSlot::new(Thread { done: done_rx, join }),
            })
        });
        Ok(Self {
            id,
            clock,
            spawner,
            max_dedicated: MAX_APPLICATION_SHARDS,
            state: Mutex::new(State::default()),
            lifecycle: Mutex::new(()),
            prefix_guard: Mutex::new(Some(guard)),
            prefix,
            cleanup_unconfirmed: AtomicBool::new(false),
        })
    }

    /// Lowers the dedicated shard cap so tests reach the overflow shard
    /// without dozens of threads.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub const fn with_dedicated_limit(mut self, limit: usize) -> Self {
        self.max_dedicated = limit;
        self
    }

    /// This adapter's identity; tokens it mints carry it.
    #[must_use]
    pub const fn id(&self) -> AdapterId { self.id }

    /// Whether an earlier stop was not acknowledged, so a new install is
    /// refused until [`Self::retry_cleanup`] confirms.
    #[must_use]
    pub fn cleanup_unconfirmed(&self) -> bool { self.cleanup_unconfirmed.load(Ordering::SeqCst) }

    /// How many shards run and how many applications share the overflow
    /// shard, for status diagnostics.
    #[must_use]
    pub fn shard_stats(&self) -> ShardStats {
        let state = locked(&self.state);
        let overflow_applications =
            state.by_app.values().filter(|shard| shard.kind == ShardKind::Overflow).count();
        let dedicated = state
            .live_shards()
            .iter()
            .filter(|shard| shard.kind != ShardKind::Overflow)
            .count();
        ShardStats {
            dedicated,
            overflow_applications,
            quarantined: state.retired.len(),
        }
    }

    /// Whether `application` is hosted on the shared overflow shard (past the
    /// dedicated cap) rather than on its own shard. `false` when it is not
    /// tracked.
    #[must_use]
    pub fn is_overflow(&self, application: ApplicationId) -> bool {
        locked(&self.state)
            .by_app
            .get(&application)
            .is_some_and(|shard| shard.kind == ShardKind::Overflow)
    }

    // ---- install -----------------------------------------------------------

    /// Starts a runtime epoch. Refused while an earlier cleanup is incomplete.
    /// It starts no thread: shards start as applications are tracked.
    ///
    /// # Errors
    ///
    /// [`InstallError`].
    pub fn install(
        &self,
        request: InstallRequest,
        _deadline: Option<Instant>,
    ) -> Result<RuntimeEpoch, InstallError> {
        if self.cleanup_unconfirmed() {
            return Err(InstallError::Quarantined {
                applications: 0,
                registrations: 0,
            });
        }
        let mut state = locked(&self.state);
        if state.closed {
            return Err(InstallError::Dispatch(DispatchError::Closed));
        }
        if let Some(installed) = &state.installed {
            return Err(InstallError::AlreadyInstalled(installed.epoch));
        }
        if !state.retired.is_empty() {
            let (applications, registrations) = state.quarantine_totals();
            return Err(InstallError::Quarantined { applications, registrations });
        }
        state.next_epoch += 1;
        let epoch = RuntimeEpoch::new(state.next_epoch);
        state.installed = Some(Installed {
            epoch,
            request,
            counters: EpochCounters::new(),
        });
        Ok(epoch)
    }

    // ---- tracking ------------------------------------------------------------

    /// Installs observation for one exact application instance on its own
    /// shard (or the overflow shard past the cap), then enumerates its windows
    /// asynchronously in chunks. Events leave through the epoch's sink. A
    /// shard that cannot start is reported as [`TrackOutcome::OwnerUnavailable`]
    /// with nothing left running.
    #[must_use]
    pub fn track_application(
        &self,
        epoch: RuntimeEpoch,
        request: TrackRequest,
        deadline: Option<Instant>,
    ) -> TrackOutcome {
        let cancel = request.cancel.clone();
        let application = request.application;
        let shard = match self.shard_for_track(epoch, application) {
            Ok(shard) => shard,
            Err(outcome) => return outcome,
        };
        let outcome = track_outcome(shard.request(deadline, Some(&cancel), |reply| {
            Command::Track { epoch, request, reply }
        }));
        if !matches!(
            outcome,
            TrackOutcome::Tracking
                | TrackOutcome::AlreadyTracked
                | TrackOutcome::Unacknowledged
                | TrackOutcome::Dispatch(_)
        ) {
            // Nothing is observed: release the mapping and an emptied shard.
            self.release_application(application, &shard, None);
        }
        outcome
    }

    fn shard_for_track(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
    ) -> Result<Arc<Shard>, TrackOutcome> {
        let _life = locked(&self.lifecycle);
        let (kind, request, counters) = {
            let mut state = locked(&self.state);
            if state.closed {
                return Err(TrackOutcome::Dispatch(DispatchError::Closed));
            }
            let Some(installed) = state.installed.as_ref().filter(|i| i.epoch == epoch) else {
                return Err(TrackOutcome::WrongEpoch);
            };
            if installed.request.gate.is_sealed() {
                return Err(TrackOutcome::WrongEpoch);
            }
            let (request, counters) = (installed.request.clone(), installed.counters.clone());
            if let Some(shard) = state.by_app.get(&application) {
                return Ok(Arc::clone(shard));
            }
            let slot = (0..self.max_dedicated).find(|slot| !state.slots.contains(slot));
            if let Some(slot) = slot {
                state.slots.insert(slot);
                (ShardKind::Dedicated(slot), request, counters)
            } else if let Some(shard) = state.overflow.clone() {
                state.by_app.insert(application, Arc::clone(&shard));
                return Ok(shard);
            } else {
                (ShardKind::Overflow, request, counters)
            }
        };
        let shard = self.start_shard(kind, epoch, request, counters).inspect_err(|_| {
            if let ShardKind::Dedicated(slot) = kind {
                locked(&self.state).slots.remove(&slot);
            }
        })?;
        let shard = Arc::new(shard);
        let mut state = locked(&self.state);
        if kind == ShardKind::Overflow {
            state.overflow = Some(Arc::clone(&shard));
        }
        state.by_app.insert(application, Arc::clone(&shard));
        drop(state);
        Ok(shard)
    }

    /// Spawns a shard thread and installs the router's epoch on it.
    fn start_shard(
        &self,
        kind: ShardKind,
        epoch: RuntimeEpoch,
        request: InstallRequest,
        counters: EpochCounters,
    ) -> Result<Shard, TrackOutcome> {
        let guard = NameGuard::claim(&kind.name(&self.prefix))
            .map_err(|_| TrackOutcome::OwnerUnavailable)?;
        let shard = (self.spawner)(kind, guard).map_err(|_| TrackOutcome::OwnerUnavailable)?;
        let installed = shard.request(None, None, |reply| Command::InstallShard {
            request,
            epoch,
            counters,
            reply,
        });
        if matches!(installed, Ok(Ok(_))) {
            return Ok(shard);
        }
        // Never leave a half-started thread: stop it, and keep it (retired)
        // if it does not finish.
        let leftovers = self.terminate(vec![Arc::new(shard)]);
        let mut state = locked(&self.state);
        for left in leftovers {
            state.retired.push(Retired {
                shard: left.shard,
                applications: 0,
                registrations: 0,
            });
        }
        drop(state);
        Err(TrackOutcome::OwnerUnavailable)
    }

    /// Re-enumerates a tracked application (reconciles after sink or observer
    /// overflow).
    #[must_use]
    pub fn refresh_application(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: CancellationToken,
        deadline: Option<Instant>,
    ) -> TrackOutcome {
        let wait_cancel = cancel.clone();
        let outcome = match self.route(application) {
            Route::Shard(shard) => {
                shard.request(deadline, Some(&wait_cancel), |reply| Command::Refresh {
                    epoch,
                    application,
                    cancel,
                    deadline,
                    reply,
                })
            }
            Route::Closed => Err(RequestError::Dispatch(DispatchError::Closed)),
            Route::Unrouted(_) => Ok(TrackOutcome::WrongEpoch),
        };
        track_outcome(outcome)
    }

    /// Reads the application's real `AXFocusedWindow` on its shard (bounded by
    /// the element timeout) and answers which tracked window it is.
    #[must_use]
    pub fn focused_window(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> FocusOutcome {
        let outcome = match self.route(application) {
            Route::Shard(shard) => shard.request(deadline, Some(cancel), |reply| {
                Command::FocusedWindow { epoch, application, reply }
            }),
            Route::Closed => Err(RequestError::Dispatch(DispatchError::Closed)),
            Route::Unrouted(_) => Ok(FocusOutcome::WrongEpoch),
        };
        match outcome {
            Ok(outcome) => outcome,
            Err(RequestError::Dispatch(error)) => FocusOutcome::Dispatch(error),
            Err(RequestError::Unacknowledged) => FocusOutcome::Unacknowledged,
            Err(RequestError::Unavailable) => FocusOutcome::OwnerUnavailable,
        }
    }

    /// Stops observing one application (it terminated or was replaced) and
    /// ends its dedicated shard. The wait observes `cancel`; a cancelled wait
    /// is [`StopOutcome::Unacknowledged`] while the shard still runs the
    /// queued command.
    #[must_use]
    pub fn untrack_application(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> StopOutcome {
        let shard = {
            let state = locked(&self.state);
            if state.closed {
                return StopOutcome::Dispatch(DispatchError::Closed);
            }
            if state.installed.as_ref().is_none_or(|installed| installed.epoch != epoch) {
                return StopOutcome::NotCurrent;
            }
            let Some(shard) = state.by_app.get(&application).cloned() else {
                return Self::with_retired(&state, StopOutcome::Clean { applications: 0 });
            };
            shard
        };
        let outcome = stop_outcome(&shard.request(deadline, Some(cancel), |reply| {
            Command::Untrack { epoch, application, reply }
        }));
        if matches!(outcome, StopOutcome::Dispatch(_)) {
            return outcome;
        }
        self.release_application(application, &shard, Some(outcome));
        Self::with_retired(&locked(&self.state), outcome)
    }

    /// A clean answer is only clean while no shard is retired.
    fn with_retired(state: &State, outcome: StopOutcome) -> StopOutcome {
        if matches!(outcome, StopOutcome::Clean { .. }) && !state.retired.is_empty() {
            let (applications, registrations) = state.quarantine_totals();
            StopOutcome::Quarantined { applications, registrations }
        } else {
            outcome
        }
    }

    /// Forgets `application` and ends its shard when that leaves it empty. A
    /// shard that does not finish is retired, never detached.
    fn release_application(
        &self,
        application: ApplicationId,
        shard: &Arc<Shard>,
        outcome: Option<StopOutcome>,
    ) {
        let emptied = {
            let mut state = locked(&self.state);
            if state.by_app.get(&application).is_some_and(|held| Arc::ptr_eq(held, shard)) {
                state.by_app.remove(&application);
            }
            let hosts_more = state.by_app.values().any(|held| Arc::ptr_eq(held, shard));
            if !hosts_more && state.overflow.as_ref().is_some_and(|held| Arc::ptr_eq(held, shard)) {
                state.overflow = None;
            }
            !hosts_more
        };
        if !emptied {
            return;
        }
        let applications = match outcome {
            Some(StopOutcome::Quarantined { applications, .. }) => applications,
            _ => 0,
        };
        for left in self.terminate(vec![Arc::clone(shard)]) {
            locked(&self.state).retired.push(Retired {
                shard: left.shard,
                applications: applications.max(1),
                registrations: left.registrations,
            });
        }
    }

    // ---- actions -----------------------------------------------------------

    /// Queues an action on the shard of its window's application without
    /// waiting. `reply` runs on that shard thread, or on the calling thread
    /// when no shard hosts the application (the typed stale outcome). On `Err`
    /// nothing was queued, so nothing can be sent later.
    ///
    /// # Errors
    ///
    /// [`DispatchError`].
    pub fn submit_action(
        &self,
        request: ActionRequest,
        reply: impl FnOnce(WindowActionOutcome) + Send + 'static,
    ) -> Result<(), DispatchError> {
        match self.route(request.issuance.window().application()) {
            Route::Shard(shard) => shard.submit(Command::Act {
                request,
                reply: Box::new(reply),
            }),
            Route::Closed => Err(DispatchError::Closed),
            Route::Unrouted(unrouted) => {
                reply(self.unrouted_action(&unrouted, &request));
                Ok(())
            }
        }
    }

    /// Queues a batch of frame writes for one application's shard without
    /// waiting. The whole batch is routed once and inserted under one mailbox
    /// lock, then one wakeup is signalled. Each reply runs exactly once: on
    /// the shard thread, on the thread that replaced it, or inline here when no
    /// shard hosts the application (the same typed outcome an action gets).
    /// On `Err` nothing was queued and no reply ran.
    ///
    /// # Errors
    ///
    /// [`DispatchError::Closed`] when the adapter or the shard is closed;
    /// [`DispatchError::Full`] when the batch would exceed
    /// [`FRAME_MAILBOX_BOUND`](super::FRAME_MAILBOX_BOUND).
    pub fn submit_frames(
        &self,
        application: ApplicationId,
        batch: Vec<(FrameRequest, FrameReplyFn)>,
    ) -> Result<(), DispatchError> {
        match self.route(application) {
            Route::Shard(shard) => shard.submit_frames(batch),
            Route::Closed => Err(DispatchError::Closed),
            Route::Unrouted(unrouted) => {
                for (request, reply) in batch {
                    let empty = request.position.is_none() && request.size.is_none();
                    let outcome = self.unrouted_outcome(&unrouted, &request.issuance, empty);
                    reply(done(outcome));
                }
                Ok(())
            }
        }
    }

    /// The outcome the single owner gave for an action whose application has
    /// no tracked state.
    fn unrouted_action(&self, unrouted: &Unrouted, request: &ActionRequest) -> WindowActionOutcome {
        let empty = request.action == (WindowAction::SetFrame { position: None, size: None });
        self.unrouted_outcome(unrouted, &request.issuance, empty)
    }

    fn unrouted_outcome(
        &self,
        unrouted: &Unrouted,
        issuance: &WindowIssuance,
        empty: bool,
    ) -> WindowActionOutcome {
        let window = issuance.window();
        let Some(gate) = &unrouted.gate else {
            return WindowActionOutcome::Stale(StaleReason::WrongEpoch);
        };
        if let Err(reason) = gate.admit(issuance) {
            return WindowActionOutcome::NotIssued(reason);
        }
        if empty {
            return WindowActionOutcome::NotIssued(NotIssuedReason::EmptyRequest);
        }
        if self.clock.now() >= issuance.deadline() {
            return WindowActionOutcome::ExpiredBeforeSend;
        }
        WindowActionOutcome::Stale(unrouted.stale_reason(window))
    }

    /// Queues an action and waits at most one hop, capped by the issuance
    /// deadline. If the wait ends first the issuance is cancelled: when that
    /// wins, nothing was sent; when the send had already committed the outcome
    /// is `Uncertain`, never "not applied".
    #[must_use]
    pub fn act(&self, request: ActionRequest, cancel: &CancellationToken) -> WindowActionOutcome {
        let issuance = request.issuance.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        if let Err(error) = self.submit_action(request, move |outcome| {
            let _ = tx.send(outcome);
        }) {
            return WindowActionOutcome::NotIssued(NotIssuedReason::Dispatch(error));
        }
        let wait = hop_wait(Instant::now(), Some(issuance.deadline()));
        match wait_reply(&rx, wait, Some(cancel)) {
            Waited::Got(outcome) => outcome,
            ended => match issuance.cancel() {
                IssuanceCancel::AlreadyIssued => WindowActionOutcome::uncertain(),
                IssuanceCancel::Cancelled => WindowActionOutcome::NotIssued(match ended {
                    Waited::Gone => NotIssuedReason::OwnerUnavailable,
                    _ if Instant::now() >= issuance.deadline() => {
                        return WindowActionOutcome::ExpiredBeforeSend;
                    }
                    _ => NotIssuedReason::AbandonedBeforeSend,
                }),
            },
        }
    }

    /// Reads or writes the `AXHidden` of one tracked application and waits at
    /// most one hop. If the wait ends first the write's issuance is cancelled:
    /// when that wins nothing was sent; when the send had already committed the
    /// outcome is [`HiddenOutcome::Unobserved`], never "not applied".
    #[must_use]
    pub fn application_hidden(
        &self,
        request: HiddenRequest,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> HiddenOutcome {
        let issuance = request.issuance.clone();
        let outcome = match self.route(request.application) {
            Route::Shard(shard) => shard.request(deadline, Some(cancel), |reply| Command::Hidden {
                request,
                reply,
            }),
            Route::Closed => Err(RequestError::Dispatch(DispatchError::Closed)),
            Route::Unrouted(unrouted) => {
                Ok(HiddenOutcome::Stale(if unrouted.epoch == Some(request.epoch) {
                    StaleReason::ApplicationGone
                } else {
                    StaleReason::WrongEpoch
                }))
            }
        };
        match outcome {
            Ok(outcome) => outcome,
            Err(RequestError::Dispatch(error)) => {
                HiddenOutcome::NotIssued(NotIssuedReason::Dispatch(error))
            }
            Err(error) => {
                let unavailable = matches!(error, RequestError::Unavailable);
                match issuance.map(|issuance| issuance.cancel()) {
                    Some(IssuanceCancel::AlreadyIssued) => HiddenOutcome::Unobserved,
                    _ if unavailable => HiddenOutcome::NotIssued(NotIssuedReason::OwnerUnavailable),
                    _ => HiddenOutcome::NotIssued(NotIssuedReason::AbandonedBeforeSend),
                }
            }
        }
    }

    /// The non-blocking form of [`Self::snapshot`]: queues the same command and
    /// returns. `reply` runs on the shard thread, or inline on the calling
    /// thread when no shard hosts the application (the typed stale outcome).
    /// On `Err` nothing was queued.
    ///
    /// # Errors
    ///
    /// [`DispatchError`].
    pub fn submit_snapshot(
        &self,
        window: WindowInstance,
        reply: impl FnOnce(SnapshotOutcome) + Send + 'static,
    ) -> Result<(), DispatchError> {
        match self.route(window.application()) {
            Route::Shard(shard) => {
                shard.submit(Command::Snapshot { window, reply: Box::new(reply) })
            }
            Route::Closed => Err(DispatchError::Closed),
            Route::Unrouted(unrouted) => {
                reply(SnapshotOutcome::Stale(unrouted.stale_reason(window)));
                Ok(())
            }
        }
    }

    /// Queues a frame-only read (`AXPosition` and `AXSize` after the light
    /// validation, no `AXWindows` listing) without waiting, with the routing
    /// of [`Self::submit_snapshot`]: `reply` runs on the shard thread, or
    /// inline with the typed stale outcome when no shard hosts the
    /// application. On `Err` nothing was queued.
    ///
    /// # Errors
    ///
    /// [`DispatchError`].
    pub fn submit_frame_read(
        &self,
        window: WindowInstance,
        reply: impl FnOnce(FrameReadOutcome) + Send + 'static,
    ) -> Result<(), DispatchError> {
        match self.route(window.application()) {
            Route::Shard(shard) => {
                shard.submit(Command::ReadFrame { window, reply: Box::new(reply) })
            }
            Route::Closed => Err(DispatchError::Closed),
            Route::Unrouted(unrouted) => {
                reply(FrameReadOutcome::Stale(unrouted.stale_reason(window)));
                Ok(())
            }
        }
    }

    /// Reads a fresh snapshot of a live window on its application's shard.
    /// The wait observes `cancel`.
    #[must_use]
    pub fn snapshot(
        &self,
        window: WindowInstance,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> SnapshotOutcome {
        let outcome = match self.route(window.application()) {
            Route::Shard(shard) => shard.request(deadline, Some(cancel), |reply| {
                Command::Snapshot { window, reply }
            }),
            Route::Closed => Err(RequestError::Dispatch(DispatchError::Closed)),
            Route::Unrouted(unrouted) => Ok(SnapshotOutcome::Stale(unrouted.stale_reason(window))),
        };
        match outcome {
            Ok(outcome) => outcome,
            Err(RequestError::Dispatch(error)) => SnapshotOutcome::Dispatch(error),
            Err(RequestError::Unacknowledged) => SnapshotOutcome::Unacknowledged,
            Err(RequestError::Unavailable) => SnapshotOutcome::OwnerUnavailable,
        }
    }

    /// Resolves a `CGWindowID` to a **currently live** token by asking every
    /// shard under one deadline. `CGWindowID` is metadata: a destroyed token
    /// is never revived, an id claimed twice (within or across applications)
    /// resolves to nothing, and any failure is `None`.
    #[must_use]
    pub fn resolve_window_id(
        &self,
        cg_window_id: u32,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<WindowInstance> {
        let shards = {
            let state = locked(&self.state);
            if state.closed {
                return None;
            }
            state.live_shards()
        };
        let end = Instant::now() + hop_wait(Instant::now(), deadline);
        let answers = fan_out(&shards, end, Some(cancel), |reply| Command::ResolveRaw {
            cg_window_id,
            reply,
        });
        let mut found = None;
        let mut matches = 0;
        for answer in answers {
            match answer.ok()? {
                Resolution::NoMatch => {}
                Resolution::Ambiguous => return None,
                Resolution::Single(token) => {
                    matches += 1;
                    found = token;
                }
            }
        }
        if matches == 1 { found } else { None }
    }

    // ---- stop, retry, close ------------------------------------------------------

    /// Seals issuance immediately (lock-free, through `gate`), then stops the
    /// epoch on every shard under one shared deadline: each cancels
    /// enumeration and removes observer registrations with acknowledgement.
    /// Clean shards are joined; the rest are kept (never detached) and failed
    /// removals are quarantined for [`Self::retry_cleanup`].
    #[must_use]
    pub fn stop(
        &self,
        epoch: RuntimeEpoch,
        gate: &GenerationCell,
        deadline: Option<Instant>,
    ) -> StopOutcome {
        gate.seal();
        let outcome = self.stop_epoch(epoch, deadline);
        self.track_cleanup(outcome);
        outcome
    }

    fn stop_epoch(&self, epoch: RuntimeEpoch, deadline: Option<Instant>) -> StopOutcome {
        let shards = {
            let _life = locked(&self.lifecycle);
            let mut state = locked(&self.state);
            if state.closed {
                return StopOutcome::Dispatch(DispatchError::Closed);
            }
            if state.installed.as_ref().is_none_or(|installed| installed.epoch != epoch) {
                return StopOutcome::NotCurrent;
            }
            let shards = state.live_shards();
            state.installed = None;
            state.by_app.clear();
            state.overflow = None;
            shards
        };
        let end = Instant::now() + hop_wait(Instant::now(), deadline);
        let answers = fan_out(&shards, end, None, |reply| Command::Stop { epoch, reply });
        let mut worst: Option<StopOutcome> = None;
        let mut cleaned = 0;
        let mut finished = Vec::new();
        let mut kept = Vec::new();
        for (shard, answer) in shards.into_iter().zip(answers) {
            let outcome = stop_outcome(&answer);
            match outcome {
                StopOutcome::Clean { applications } => {
                    cleaned += applications;
                    finished.push(shard);
                }
                StopOutcome::Quarantined { applications, registrations } => {
                    kept.push(Retired {
                        shard,
                        applications,
                        registrations,
                    });
                }
                StopOutcome::NotCurrent => finished.push(shard),
                other => {
                    worst = Some(worst_stop(worst, other));
                    kept.push(Retired {
                        shard,
                        applications: 0,
                        registrations: 0,
                    });
                }
            }
        }
        for left in self.terminate(finished) {
            kept.push(Retired {
                shard: left.shard,
                applications: 0,
                registrations: left.registrations,
            });
        }
        let mut state = locked(&self.state);
        state.retired.extend(kept);
        let (applications, registrations) = state.quarantine_totals();
        let clean = state.retired.is_empty();
        drop(state);
        worst.unwrap_or(if clean {
            StopOutcome::Clean { applications: cleaned }
        } else {
            StopOutcome::Quarantined { applications, registrations }
        })
    }

    fn track_cleanup(&self, outcome: StopOutcome) {
        let unconfirmed = matches!(
            outcome,
            StopOutcome::Unacknowledged
                | StopOutcome::Dispatch(_)
                | StopOutcome::Quarantined { .. }
        );
        if unconfirmed {
            self.cleanup_unconfirmed.store(true, Ordering::SeqCst);
        } else if matches!(outcome, StopOutcome::Clean { .. }) {
            self.cleanup_unconfirmed.store(false, Ordering::SeqCst);
        }
    }

    /// Restores every open `AXEnhancedUserInterface` lease now, ignoring the
    /// idle grace: each shard writes the attribute back on the applications it
    /// turned off, under one shared deadline. This is owned cleanup and is
    /// allowed after the seal; call it after the frame drain, before the
    /// runtime-hidden restore. A lease that cannot be restored stays owed to
    /// the shard: the epoch stop and [`Self::retry_cleanup`] retry it.
    #[must_use]
    pub fn restore_enhanced_ui(&self, deadline: Option<Instant>) -> LeaseRestoreOutcome {
        let shards = {
            let state = locked(&self.state);
            if state.closed {
                return LeaseRestoreOutcome::Clean { restored: 0 };
            }
            // Retired (quarantined) shards may still owe a lease.
            let mut shards = state.live_shards();
            shards.extend(state.retired.iter().map(|held| Arc::clone(&held.shard)));
            shards
        };
        let end = Instant::now() + hop_wait(Instant::now(), deadline);
        let answers = fan_out(&shards, end, None, |reply| Command::RestoreLeases { reply });
        let mut restored = 0;
        let mut owed = Vec::new();
        let mut unacknowledged = false;
        for answer in answers {
            match answer {
                Ok(report) => {
                    restored += report.restored;
                    owed.extend(report.owed);
                }
                // The shard ended: it restored its leases when it did.
                Err(RequestError::Unavailable | RequestError::Dispatch(DispatchError::Closed)) => {}
                Err(RequestError::Unacknowledged | RequestError::Dispatch(_)) => {
                    unacknowledged = true;
                }
            }
        }
        if unacknowledged {
            LeaseRestoreOutcome::Unacknowledged
        } else if owed.is_empty() {
            LeaseRestoreOutcome::Clean { restored }
        } else {
            owed.sort();
            LeaseRestoreOutcome::Owed { applications: owed }
        }
    }

    /// Retries every quarantined removal exactly once on every shard under one
    /// deadline and reports what remains. Shards that end up empty are joined.
    /// `None` when a command was not acknowledged.
    #[must_use]
    pub fn retry_cleanup(
        &self,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<CleanupReport> {
        let shards = {
            let state = locked(&self.state);
            if state.closed {
                return None;
            }
            let mut shards = state.live_shards();
            shards.extend(state.retired.iter().map(|held| Arc::clone(&held.shard)));
            shards
        };
        // A retired shard whose channel is gone can only be joined.
        let (detached, shards): (Vec<_>, Vec<_>) =
            shards.into_iter().partition(|shard| shard.is_detached());
        let end = Instant::now() + hop_wait(Instant::now(), deadline);
        let answers = fan_out(&shards, end, Some(cancel), |reply| Command::Retry { reply });
        let mut total = CleanupReport {
            cleaned_applications: 0,
            remaining_applications: 0,
            remaining_registrations: 0,
        };
        let mut empty = Vec::new();
        let mut unanswered = false;
        let mut updates = Vec::new();
        for (shard, answer) in shards.into_iter().zip(answers) {
            let Ok(report) = answer else {
                unanswered = true;
                continue;
            };
            total.cleaned_applications += report.cleaned_applications;
            total.remaining_applications += report.remaining_applications;
            total.remaining_registrations += report.remaining_registrations;
            if report.remaining_applications == 0 && report.remaining_registrations == 0 {
                empty.push(shard);
            } else {
                updates.push((shard, report));
            }
        }
        let join_end = Instant::now() + hop_wait(Instant::now(), deadline);
        let joins: Vec<_> = detached
            .into_iter()
            .map(|shard| {
                let joined = shard.join(join_end);
                (shard, joined)
            })
            .collect();
        {
            let mut state = locked(&self.state);
            for (shard, joined) in joins {
                if joined {
                    if let ShardKind::Dedicated(slot) = shard.kind {
                        state.slots.remove(&slot);
                    }
                    state.retired.retain(|held| !Arc::ptr_eq(&held.shard, &shard));
                } else if let Some(held) =
                    state.retired.iter().find(|held| Arc::ptr_eq(&held.shard, &shard))
                {
                    total.remaining_applications += held.applications;
                    total.remaining_registrations += held.registrations;
                }
            }
            for (shard, report) in updates {
                if let Some(held) =
                    state.retired.iter_mut().find(|held| Arc::ptr_eq(&held.shard, &shard))
                {
                    held.applications = report.remaining_applications;
                    held.registrations = report.remaining_registrations;
                }
            }
        }
        // Retired shards with nothing left are ended and joined.
        let retired_empty: Vec<_> = {
            let mut state = locked(&self.state);
            let (gone, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut state.retired)
                .into_iter()
                .partition(|held| empty.iter().any(|done| Arc::ptr_eq(done, &held.shard)));
            state.retired = keep;
            gone.into_iter().map(|held| held.shard).collect()
        };
        let leftovers = self.terminate(retired_empty);
        if !leftovers.is_empty() {
            let mut state = locked(&self.state);
            for left in leftovers {
                total.remaining_registrations += left.registrations;
                state.retired.push(Retired {
                    shard: left.shard,
                    applications: 0,
                    registrations: left.registrations,
                });
            }
        }
        if unanswered {
            return None;
        }
        let remaining = locked(&self.state).retired.len();
        if total.remaining_applications == 0 && remaining == 0 {
            self.cleanup_unconfirmed.store(false, Ordering::SeqCst);
        }
        Some(total)
    }

    /// Stops every shard and joins its thread, waiting at most
    /// [`ACKNOWLEDGEMENT_BUDGET`] per phase. Quarantined resources keep their
    /// shard alive; retry first. Never call from the main thread.
    #[must_use]
    pub fn close(&self) -> CloseOutcome {
        let _life = locked(&self.lifecycle);
        let shards = {
            let mut state = locked(&self.state);
            if state.closed {
                return CloseOutcome::AlreadyClosed;
            }
            if let Some(installed) = state.installed.take() {
                installed.request.gate.seal();
            }
            let mut shards = state.live_shards();
            shards.extend(state.retired.drain(..).map(|held| held.shard));
            state.by_app.clear();
            state.overflow = None;
            shards
        };
        let mut registrations = 0;
        let mut timed_out = false;
        let mut dispatch = None;
        let leftovers = self.terminate(shards);
        {
            let mut state = locked(&self.state);
            for left in leftovers {
                match left.kind {
                    LeftKind::Quarantined => registrations += left.registrations,
                    LeftKind::TimedOut => timed_out = true,
                    LeftKind::Dispatch(error) => dispatch = dispatch.or(Some(error)),
                }
                state.retired.push(Retired {
                    shard: left.shard,
                    applications: 0,
                    registrations: left.registrations,
                });
            }
            if state.retired.is_empty() {
                state.closed = true;
                drop(state);
                locked(&self.prefix_guard).take();
                return CloseOutcome::Closed;
            }
        }
        if let Some(error) = dispatch {
            return CloseOutcome::Dispatch(error);
        }
        if timed_out {
            return CloseOutcome::TimedOut;
        }
        CloseOutcome::Quarantined { registrations }
    }

    // ---- routing and shard teardown ---------------------------------------------------

    fn route(&self, application: ApplicationId) -> Route {
        let state = locked(&self.state);
        if state.closed {
            return Route::Closed;
        }
        state.by_app.get(&application).map_or_else(
            || {
                Route::Unrouted(Unrouted {
                    adapter: self.id,
                    epoch: state.installed.as_ref().map(|installed| installed.epoch),
                    gate: state
                        .installed
                        .as_ref()
                        .map(|installed| Arc::clone(&installed.request.gate)),
                })
            },
            |shard| Route::Shard(Arc::clone(shard)),
        )
    }

    /// Asks every shard to terminate (all sent first, one shared deadline),
    /// then joins the ones that did. Returns the shards that stay alive.
    fn terminate(&self, shards: Vec<Arc<Shard>>) -> Vec<Left> {
        if shards.is_empty() {
            return Vec::new();
        }
        let end = Instant::now() + ACKNOWLEDGEMENT_BUDGET;
        let answers = fan_out(&shards, end, None, |reply| Command::Terminate { reply });
        let join_end = Instant::now() + ACKNOWLEDGEMENT_BUDGET;
        let mut left = Vec::new();
        let mut freed = Vec::new();
        for (shard, answer) in shards.into_iter().zip(answers) {
            let kind = match answer {
                Ok(CloseOutcome::Closed)
                | Err(RequestError::Unavailable | RequestError::Dispatch(DispatchError::Closed)) => {
                    if shard.join(join_end) {
                        if let ShardKind::Dedicated(slot) = shard.kind {
                            freed.push(slot);
                        }
                        continue;
                    }
                    LeftKind::TimedOut
                }
                Ok(CloseOutcome::Quarantined { .. }) => LeftKind::Quarantined,
                Ok(_) | Err(RequestError::Unacknowledged) => LeftKind::TimedOut,
                Err(RequestError::Dispatch(error)) => LeftKind::Dispatch(error),
            };
            let registrations = match answer {
                Ok(CloseOutcome::Quarantined { registrations }) => registrations,
                _ => 0,
            };
            left.push(Left { shard, kind, registrations });
        }
        if !freed.is_empty() {
            let mut state = locked(&self.state);
            for slot in freed {
                state.slots.remove(&slot);
            }
        }
        left
    }
}

enum LeftKind {
    Quarantined,
    TimedOut,
    Dispatch(DispatchError),
}

/// A shard that did not finish terminating.
struct Left {
    shard: Arc<Shard>,
    kind: LeftKind,
    registrations: usize,
}

enum Route {
    Shard(Arc<Shard>),
    Unrouted(Unrouted),
    Closed,
}

/// What the router knows when no shard hosts the application.
struct Unrouted {
    adapter: AdapterId,
    epoch: Option<RuntimeEpoch>,
    gate: Option<Arc<GenerationCell>>,
}

impl Unrouted {
    /// The reason the single owner gave for a window it did not track.
    fn stale_reason(&self, window: WindowInstance) -> StaleReason {
        if self.epoch.is_none() {
            return StaleReason::WrongEpoch;
        }
        if window.adapter() != self.adapter {
            StaleReason::WrongAdapter
        } else if self.epoch != Some(window.epoch()) {
            StaleReason::WrongEpoch
        } else {
            StaleReason::ApplicationGone
        }
    }
}

fn worst_stop(current: Option<StopOutcome>, next: StopOutcome) -> StopOutcome {
    let rank = |outcome: StopOutcome| match outcome {
        StopOutcome::Unacknowledged => 3,
        StopOutcome::Dispatch(_) => 2,
        _ => 1,
    };
    match current {
        Some(held) if rank(held) >= rank(next) => held,
        _ => next,
    }
}

#[derive(Clone, Copy)]
enum RequestError {
    Dispatch(DispatchError),
    Unacknowledged,
    Unavailable,
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "clippy asks for by-value and by-ref in turn"
)]
const fn track_outcome(outcome: Result<TrackOutcome, RequestError>) -> TrackOutcome {
    match outcome {
        Ok(outcome) => outcome,
        Err(RequestError::Dispatch(error)) => TrackOutcome::Dispatch(error),
        Err(RequestError::Unacknowledged) => TrackOutcome::Unacknowledged,
        Err(RequestError::Unavailable) => TrackOutcome::OwnerUnavailable,
    }
}

const fn stop_outcome(outcome: &Result<StopOutcome, RequestError>) -> StopOutcome {
    match outcome {
        Ok(outcome) => *outcome,
        Err(RequestError::Dispatch(error)) => StopOutcome::Dispatch(*error),
        Err(RequestError::Unacknowledged) => StopOutcome::Unacknowledged,
        Err(RequestError::Unavailable) => StopOutcome::OwnerUnavailable,
    }
}

/// The owner loop. `core`, the driver and everything native stay on this thread.
pub(super) fn run_owner<S: WindowSystem, D: Driver>(
    mut core: Core<S>,
    commands: &Receiver<Command>,
    mut driver: D,
    wakeup: &Wakeup,
) {
    loop {
        // Cleared before draining: a command queued after the drain re-arms it.
        wakeup.clear();
        let turn = core.turn(commands);
        if turn.finished {
            return;
        }
        // An open lease needs the shard again when its idle grace ends.
        driver.park(if turn.active {
            Some(Duration::ZERO)
        } else {
            turn.deadline.map(|at| at.saturating_duration_since(core.now()))
        });
    }
}
