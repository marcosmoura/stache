//! The caller-facing handle to one AX owner thread.
//!
//! ```text
//! consumers (any thread, never main-blocking)
//!    └─ bounded SyncSender<Command> + Wakeup ──> "<consumer thread name>"
//!                                                 ├─ WindowSystem (created in-thread)
//!                                                 ├─ per-app AxObserver + run loop
//!                                                 └─ retained window elements
//! ```
//!
//! The handle is `Send + Sync` and holds no native object. Blocking helpers
//! wait at most [`ACKNOWLEDGEMENT_BUDGET`] per hop, capped by the caller's
//! remaining deadline (one shared deadline, never multiplied), and observe
//! cancellation every [`CANCELLATION_QUANTUM`]. Never call a blocking helper
//! from the main thread; the asynchronous `submit_*` forms are main-safe. The
//! owner never waits on main, so no main -> owner -> main wait exists.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use stache_sdk::CancellationToken;

use super::commands::{
    ActionRequest, CleanupReport, CloseOutcome, Command, FocusOutcome, HiddenOutcome,
    HiddenRequest, InstallError, InstallRequest, Reply, SnapshotOutcome, StopOutcome, TrackOutcome,
    TrackRequest,
};
use super::core::Core;
use super::driver::{Driver, RunLoopDriver, Wakeup};
use super::identity::{AdapterId, RuntimeEpoch, WindowInstance};
use super::issuance::{DispatchError, IssuanceCancel, NotIssuedReason, WindowActionOutcome};
use super::system::{SystemWindowClock, SystemWindows, WindowClock, WindowSystem};
use crate::applications::{ACKNOWLEDGEMENT_BUDGET, ApplicationId, CANCELLATION_QUANTUM};
use crate::menu::OWNER_THREAD_NAME as MENU_ANYWHERE_THREAD_NAME;

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

struct Thread {
    done: Receiver<()>,
    join: JoinHandle<()>,
}

/// A handle to one window-ownership adapter and its dedicated AX owner thread.
pub struct WindowAdapter {
    id: AdapterId,
    sender: Mutex<Option<SyncSender<Command>>>,
    wakeup: Arc<Wakeup>,
    thread: Mutex<Option<Thread>>,
    cleanup_unconfirmed: AtomicBool,
}

impl WindowAdapter {
    /// Starts the owner thread over the real AX bindings with `thread_name`.
    ///
    /// # Errors
    ///
    /// [`SpawnError`] for a reserved, empty or duplicate name or a refused thread.
    pub fn system(thread_name: &str, options: &AdapterOptions) -> Result<Self, SpawnError> {
        Self::spawn(
            thread_name,
            options,
            Arc::new(SystemWindowClock),
            SystemWindows::new,
            RunLoopDriver::new,
        )
    }

    /// Starts the owner thread; `system` and `driver` are created **on** it,
    /// so they may hold `!Send` native objects.
    ///
    /// # Errors
    ///
    /// [`SpawnError`] for a reserved, empty or duplicate name or a refused thread.
    pub fn spawn<S, D>(
        thread_name: &str,
        options: &AdapterOptions,
        clock: Arc<dyn WindowClock>,
        system: impl FnOnce() -> S + Send + 'static,
        driver: impl FnOnce() -> D + Send + 'static,
    ) -> Result<Self, SpawnError>
    where
        S: WindowSystem,
        D: Driver,
    {
        let guard = NameGuard::claim(thread_name)?;
        let id = AdapterId::allocate();
        let (sender, receiver) = mpsc::sync_channel::<Command>(OWNER_QUEUE_BOUND);
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Arc<Wakeup>>(1);
        let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
        let read_tabs = options.read_tabs;
        let read_tab_bars = options.read_tab_bars;
        let join = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                let _guard = guard;
                let driver = driver();
                let wakeup = Arc::new(Wakeup::new(driver.waker()));
                let core = Core::new(system(), id, clock, read_tabs).with_tab_bars(read_tab_bars);
                let _ = ready_tx.send(Arc::clone(&wakeup));
                run_owner(core, &receiver, driver, &wakeup);
                let _ = done_tx.send(());
            })
            .map_err(|_| SpawnError::Thread)?;
        let wakeup =
            ready_rx.recv_timeout(ACKNOWLEDGEMENT_BUDGET).map_err(|_| SpawnError::Thread)?;
        Ok(Self {
            id,
            sender: Mutex::new(Some(sender)),
            wakeup,
            thread: Mutex::new(Some(Thread { done: done_rx, join })),
            cleanup_unconfirmed: AtomicBool::new(false),
        })
    }

    /// This adapter's identity; tokens it mints carry it.
    #[must_use]
    pub const fn id(&self) -> AdapterId { self.id }

    /// Whether an earlier stop was not acknowledged, so a new install is
    /// refused until [`Self::retry_cleanup`] confirms.
    #[must_use]
    pub fn cleanup_unconfirmed(&self) -> bool { self.cleanup_unconfirmed.load(Ordering::SeqCst) }

    fn submit(&self, command: Command) -> Result<(), DispatchError> {
        let guard = self.sender.lock().unwrap_or_else(PoisonError::into_inner);
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

    /// Queues `build(reply)` and waits one hop, observing `cancel` every
    /// [`CANCELLATION_QUANTUM`].
    fn request<T: Send + 'static>(
        &self,
        deadline: Option<Instant>,
        cancel: Option<&CancellationToken>,
        build: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T, RequestError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.submit(build(Box::new(move |value| {
            let _ = tx.send(value);
        })))
        .map_err(RequestError::Dispatch)?;
        match wait_reply(&rx, hop_wait(Instant::now(), deadline), cancel) {
            Waited::Got(value) => Ok(value),
            Waited::TimedOut | Waited::Cancelled => Err(RequestError::Unacknowledged),
            Waited::Gone => Err(RequestError::Unavailable),
        }
    }

    /// Starts a runtime epoch. Refused while an earlier cleanup is incomplete.
    ///
    /// # Errors
    ///
    /// [`InstallError`].
    pub fn install(
        &self,
        request: InstallRequest,
        deadline: Option<Instant>,
    ) -> Result<RuntimeEpoch, InstallError> {
        if self.cleanup_unconfirmed() {
            return Err(InstallError::Quarantined {
                applications: 0,
                registrations: 0,
            });
        }
        match self.request(deadline, None, |reply| Command::Install { request, reply }) {
            Ok(result) => result,
            Err(RequestError::Dispatch(error)) => Err(InstallError::Dispatch(error)),
            Err(RequestError::Unacknowledged) => Err(InstallError::Unacknowledged),
            Err(RequestError::Unavailable) => Err(InstallError::OwnerUnavailable),
        }
    }

    /// Installs observation for one exact application instance, then
    /// enumerates its windows asynchronously in chunks. Events leave through
    /// the epoch's sink.
    #[must_use]
    pub fn track_application(
        &self,
        epoch: RuntimeEpoch,
        request: TrackRequest,
        deadline: Option<Instant>,
    ) -> TrackOutcome {
        let cancel = request.cancel.clone();
        let outcome = self.request(deadline, Some(&cancel), |reply| Command::Track {
            epoch,
            request,
            reply,
        });
        track_outcome(outcome)
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
        let outcome = self.request(deadline, Some(&wait_cancel), |reply| Command::Refresh {
            epoch,
            application,
            cancel,
            deadline,
            reply,
        });
        track_outcome(outcome)
    }

    /// Reads the application's real `AXFocusedWindow` on the owner thread
    /// (bounded by the element timeout) and answers which tracked window it is.
    #[must_use]
    pub fn focused_window(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> FocusOutcome {
        match self.request(deadline, Some(cancel), |reply| Command::FocusedWindow {
            epoch,
            application,
            reply,
        }) {
            Ok(outcome) => outcome,
            Err(RequestError::Dispatch(error)) => FocusOutcome::Dispatch(error),
            Err(RequestError::Unacknowledged) => FocusOutcome::Unacknowledged,
            Err(RequestError::Unavailable) => FocusOutcome::OwnerUnavailable,
        }
    }

    /// Stops observing one application (it terminated or was replaced). The
    /// wait observes `cancel`; a cancelled wait is [`StopOutcome::Unacknowledged`]
    /// while the owner still runs the queued command.
    #[must_use]
    pub fn untrack_application(
        &self,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> StopOutcome {
        stop_outcome(&self.request(deadline, Some(cancel), |reply| Command::Untrack {
            epoch,
            application,
            reply,
        }))
    }

    /// Queues an action without waiting. `reply` runs on the owner thread. On
    /// `Err` nothing was queued, so nothing can be sent later.
    ///
    /// # Errors
    ///
    /// [`DispatchError`].
    pub fn submit_action(
        &self,
        request: ActionRequest,
        reply: impl FnOnce(WindowActionOutcome) + Send + 'static,
    ) -> Result<(), DispatchError> {
        self.submit(Command::Act {
            request,
            reply: Box::new(reply),
        })
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
        match self.request(deadline, Some(cancel), |reply| Command::Hidden {
            request,
            reply,
        }) {
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

    /// Reads a fresh snapshot of a live window. The wait observes `cancel`.
    #[must_use]
    pub fn snapshot(
        &self,
        window: WindowInstance,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> SnapshotOutcome {
        match self.request(deadline, Some(cancel), |reply| Command::Snapshot {
            window,
            reply,
        }) {
            Ok(outcome) => outcome,
            Err(RequestError::Dispatch(error)) => SnapshotOutcome::Dispatch(error),
            Err(RequestError::Unacknowledged) => SnapshotOutcome::Unacknowledged,
            Err(RequestError::Unavailable) => SnapshotOutcome::OwnerUnavailable,
        }
    }

    /// Resolves a `CGWindowID` to a **currently live** token. `CGWindowID` is
    /// metadata: a destroyed token is never revived, an ambiguous id resolves
    /// to nothing, and any failure is `None`.
    #[must_use]
    pub fn resolve_window_id(
        &self,
        cg_window_id: u32,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<WindowInstance> {
        self.request(deadline, Some(cancel), |reply| Command::Resolve {
            cg_window_id,
            reply,
        })
        .ok()
        .flatten()
    }

    /// Seals issuance immediately (lock-free, through `gate`), then stops the
    /// epoch on the owner: cancels enumeration and removes observer
    /// registrations with acknowledgement. Failed removals are quarantined for
    /// [`Self::retry_cleanup`].
    #[must_use]
    pub fn stop(
        &self,
        epoch: RuntimeEpoch,
        gate: &super::issuance::GenerationCell,
        deadline: Option<Instant>,
    ) -> StopOutcome {
        gate.seal();
        let outcome =
            stop_outcome(&self.request(deadline, None, |reply| Command::Stop { epoch, reply }));
        self.track_cleanup(outcome);
        outcome
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

    /// Retries every quarantined removal exactly once and reports what remains.
    /// `None` when the command was not acknowledged.
    #[must_use]
    pub fn retry_cleanup(
        &self,
        cancel: &CancellationToken,
        deadline: Option<Instant>,
    ) -> Option<CleanupReport> {
        let report = self.request(deadline, Some(cancel), |reply| Command::Retry { reply }).ok()?;
        if report.remaining_applications == 0 {
            self.cleanup_unconfirmed.store(false, Ordering::SeqCst);
        }
        Some(report)
    }

    /// Stops the owner and joins its thread, waiting at most
    /// [`ACKNOWLEDGEMENT_BUDGET`] per hop. Quarantined resources keep the
    /// thread alive; retry first. Never call from the main thread.
    #[must_use]
    pub fn close(&self) -> CloseOutcome {
        let outcome = match self.request(None, None, |reply| Command::Terminate { reply }) {
            Ok(outcome) => outcome,
            Err(RequestError::Dispatch(DispatchError::Closed)) => {
                return CloseOutcome::AlreadyClosed;
            }
            Err(RequestError::Dispatch(error)) => return CloseOutcome::Dispatch(error),
            Err(RequestError::Unacknowledged) => return CloseOutcome::TimedOut,
            Err(RequestError::Unavailable) => CloseOutcome::Closed,
        };
        if outcome != CloseOutcome::Closed {
            return outcome;
        }
        self.sender.lock().unwrap_or_else(PoisonError::into_inner).take();
        let Some(thread) = self.thread.lock().unwrap_or_else(PoisonError::into_inner).take() else {
            return CloseOutcome::AlreadyClosed;
        };
        match thread.done.recv_timeout(ACKNOWLEDGEMENT_BUDGET) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = thread.join.join();
                CloseOutcome::Closed
            }
            Err(RecvTimeoutError::Timeout) => CloseOutcome::TimedOut,
        }
    }
}

impl Drop for WindowAdapter {
    /// Dropping the handle ends the owner loop: the closed channel is observed
    /// on the next wakeup, which stops the epoch and releases every element on
    /// the owner thread. It never blocks; use [`WindowAdapter::close`] to join.
    fn drop(&mut self) {
        self.sender.lock().unwrap_or_else(PoisonError::into_inner).take();
        self.wakeup.signal();
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
        driver.park(if turn.active {
            Some(Duration::ZERO)
        } else {
            None
        });
    }
}
