//! Exit convergence shared by the tray, Tauri's exit event, `AppKit`'s
//! terminate decision and POSIX signals.
//!
//! Every path feeds the same [`Teardown`] coordinator: tray Quit and signals
//! through [`HostHandle::request_shutdown`](crate::host::HostHandle), Tauri's
//! exit event through `request_shutdown_with_code`, `AppKit`'s terminate
//! decision through [`Teardown::begin_terminate`], and a coordinated restart
//! through [`HostHandle::request_restart`](crate::host::HostHandle). A request
//! only records a deadline and returns; a dedicated thread owns the state
//! machine, stores exactly one [`TeardownOutcome`] and dispatches one terminal
//! action. The host worker still performs the ordered teardown and reports its
//! completion, so a blocked command cannot keep the process alive past the
//! total.
//!
//! Teardown stages themselves are still unbounded behind this coordinator;
//! bounding them is the next spec part. A stalled main thread can therefore
//! stall the terminal action: this part deliberately adds no forced exit.
//! Signal handling uses `signal-hook` rather than project-authored unsafe code.

use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, Sender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::{Handle, Signals};
use stache_sdk::{InstanceLock, LogLevel, LogRecord, Logger, ModuleId, Operation};

/// Total time from the first stop request to the terminal action.
pub const TEARDOWN_TOTAL: Duration = Duration::from_secs(5);

/// Deadline applied by a later stop request.
pub const TEARDOWN_ESCALATION: Duration = Duration::from_millis(250);

/// One bounded teardown record attempt.
pub const TEARDOWN_RECORD_BUDGET: Duration = Duration::from_millis(50);

/// The only owner tracked in this part.
pub const HOST_WORKER: &str = "host-worker";

/// Owners in stable record order. Later parts append to this list.
pub const TEARDOWN_OWNERS: [&str; 1] = [HOST_WORKER];

/// The final-exit permission shared with Tauri's `ExitRequested` handler.
#[derive(Clone, Debug)]
pub struct ExitGate {
    allowed: Arc<AtomicBool>,
    exit_code: Arc<AtomicI32>,
}

impl Default for ExitGate {
    fn default() -> Self {
        Self {
            allowed: Arc::new(AtomicBool::new(false)),
            exit_code: Arc::new(AtomicI32::new(i32::MIN)),
        }
    }
}

impl ExitGate {
    /// Creates a gate that still prevents exit.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Allows the next Tauri exit event to proceed.
    pub fn allow(&self) { self.allowed.store(true, Ordering::SeqCst); }

    /// Records the host's intended process outcome and permits exit.
    ///
    /// `run_return` does not reliably report the code passed to `app.exit` on
    /// macOS, so the caller must combine this value with the runtime result.
    pub fn record_exit(&self, code: i32) {
        self.exit_code.store(code, Ordering::SeqCst);
        self.allow();
    }

    /// The host's requested exit code, if teardown reached its terminal action.
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        let code = self.exit_code.load(Ordering::SeqCst);
        (code != i32::MIN).then_some(code)
    }

    /// Whether the host finished its teardown.
    #[must_use]
    pub fn is_allowed(&self) -> bool { self.allowed.load(Ordering::SeqCst) }
}

/// What the run loop must do for one `RunEvent::ExitRequested`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitAction {
    /// Let the runtime exit; the host already finished its teardown.
    Allow,
    /// Keep the run loop alive: only a window closed, the host stays up.
    PreventOnly,
    /// Prevent this exit and start the coordinated host shutdown instead.
    PreventAndShutdown,
}

/// Decides how to answer one Tauri exit request.
///
/// `tauri-runtime-wry` emits `code: None` only when the last window was
/// destroyed, and `Some(code)` only for `AppHandle::exit`/`restart`. A window
/// close must not tear the host down, while an explicit quit request must.
#[must_use]
pub const fn exit_request_action(code: Option<i32>, allowed: bool) -> ExitAction {
    match (allowed, code) {
        (true, _) => ExitAction::Allow,
        (false, None) => ExitAction::PreventOnly,
        (false, Some(_)) => ExitAction::PreventAndShutdown,
    }
}

/// Why teardown started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeardownCause {
    /// Tray Quit or a termination signal.
    Request,
    /// Tauri's exit event with an explicit code.
    Exit,
    /// `AppKit`'s `applicationShouldTerminate:`.
    Terminate,
    /// An explicit coordinated restart.
    Restart,
}

impl TeardownCause {
    /// Stable record spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Exit => "exit",
            Self::Terminate => "terminate",
            Self::Restart => "restart",
        }
    }
}

/// The typed teardown result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeardownResult {
    /// Every owner acknowledged success.
    Clean,
    /// Every owner answered and at least one reported failure.
    Partial,
    /// At least one owner had not answered at the deadline.
    Uncertain,
}

impl TeardownResult {
    /// Stable record spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Partial => "partial",
            Self::Uncertain => "uncertain",
        }
    }
}

/// Exactly one teardown outcome per process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeardownOutcome {
    /// The typed result.
    pub result: TeardownResult,
    /// The first request's cause.
    pub cause: TeardownCause,
    /// Time from the first request to this outcome.
    pub elapsed: Duration,
    /// Whether a later request shortened the deadline.
    pub escalated: bool,
    /// The intended process code. `Partial` and `Uncertain` record 1.
    pub recorded_code: i32,
    /// Owners that answered with a failure, in [`TEARDOWN_OWNERS`] order.
    pub failed: Vec<&'static str>,
    /// Owners that had not answered at the deadline, in owner order.
    pub unacknowledged: Vec<&'static str>,
}

impl fmt::Display for TeardownOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "teardown: outcome={} cause={} elapsed_ms={} escalated={} \
             recorded_code={} failed={} unacknowledged={}",
            self.result.as_str(),
            self.cause.as_str(),
            self.elapsed.as_millis(),
            self.escalated,
            self.recorded_code,
            owner_list(&self.failed),
            owner_list(&self.unacknowledged),
        )
    }
}

fn owner_list(owners: &[&'static str]) -> String { format!("[{}]", owners.join(",")) }

/// Whether the one bounded record attempt reached the sink.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecordDelivery {
    /// The sink did not accept the record within the attempt's bound.
    #[default]
    Unavailable,
    /// The sink accepted the record before the attempt's bound.
    Delivered,
}

/// The sink behind the bounded teardown record channel.
pub trait TeardownRecordSink: Send + Sync {
    /// Writes one line. May block; never called by the coordinator thread.
    fn write(&self, line: &str);
}

/// One bounded, non-blocking teardown record attempt.
pub trait TeardownRecordChannel: Send + Sync {
    /// Attempts one write within `budget` and reports whether it completed.
    fn record(&self, line: &str, budget: Duration) -> RecordDelivery;
}

/// Writes teardown records to standard error.
#[derive(Debug, Clone, Copy, Default)]
pub struct StderrRecordSink;

impl TeardownRecordSink for StderrRecordSink {
    fn write(&self, line: &str) {
        let mut stderr = io::stderr().lock();
        let _ = writeln!(stderr, "{line}");
    }
}

/// A bounded channel whose writer thread owns the potentially blocking sink.
///
/// The coordinator only enqueues and waits for the completion signal, so a
/// blocked sink makes the attempt `unavailable` without delaying the deadline.
#[derive(Debug)]
pub struct BoundedRecordChannel {
    sender: Option<mpsc::SyncSender<RecordJob>>,
}

#[derive(Debug)]
struct RecordJob {
    line: String,
    done: Sender<()>,
    cancelled: Arc<AtomicBool>,
}

impl BoundedRecordChannel {
    /// Starts the record writer thread, if the OS allows it.
    #[must_use]
    pub fn new(sink: Arc<dyn TeardownRecordSink>) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<RecordJob>(4);
        let spawned = thread::Builder::new().name("stache-records".to_owned()).spawn(move || {
            while let Ok(job) = receiver.recv() {
                if !job.cancelled.load(Ordering::SeqCst) {
                    sink.write(&job.line);
                }
                let _ = job.done.send(());
            }
        });
        Self {
            sender: spawned.ok().map(|_| sender),
        }
    }
}

impl TeardownRecordChannel for BoundedRecordChannel {
    fn record(&self, line: &str, budget: Duration) -> RecordDelivery {
        if budget.is_zero() {
            return RecordDelivery::Unavailable;
        }
        let Some(sender) = &self.sender else {
            return RecordDelivery::Unavailable;
        };
        let (done, finished) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let job = RecordJob {
            line: line.to_owned(),
            done,
            cancelled: Arc::clone(&cancelled),
        };
        if let Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) = sender.try_send(job)
        {
            job.cancelled.store(true, Ordering::SeqCst);
            return RecordDelivery::Unavailable;
        }
        if finished.recv_timeout(budget).is_ok() {
            RecordDelivery::Delivered
        } else {
            cancelled.store(true, Ordering::SeqCst);
            RecordDelivery::Unavailable
        }
    }
}

/// A teardown record channel that accepts nothing. Test support.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscardRecordChannel;

impl TeardownRecordChannel for DiscardRecordChannel {
    fn record(&self, _line: &str, _budget: Duration) -> RecordDelivery {
        RecordDelivery::Unavailable
    }
}

/// Time source and wait behaviour for one coordinator.
pub trait TeardownClock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Instant;

    /// How long the driver may sleep when `remaining` time is left.
    ///
    /// The system clock returns `remaining`; a clock a test can advance returns
    /// a long value so only an explicit [`ManualClock::advance`] wakes it.
    fn pause(&self, remaining: Duration) -> Duration { remaining }

    /// Receives a wake handle once the driver starts.
    fn attach(&self, _wake: Wake) {}
}

/// Wall-clock time used by the desktop.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl TeardownClock for SystemClock {
    fn now(&self) -> Instant { Instant::now() }
}

/// A clock a test advances explicitly.
///
/// This is test support: the desktop always uses [`SystemClock`]. Advancing it
/// wakes the driver instead of sleeping through a real budget.
#[derive(Debug)]
pub struct ManualClock {
    now: Mutex<Instant>,
    wake: Mutex<Option<Wake>>,
}

impl ManualClock {
    /// Starts at the current instant with no offset.
    #[must_use]
    pub fn new() -> Self {
        Self {
            now: Mutex::new(Instant::now()),
            wake: Mutex::new(None),
        }
    }

    /// Moves time forward and wakes the coordinator driver.
    pub fn advance(&self, delta: Duration) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) += delta;
        let wake = self.wake.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}

impl Default for ManualClock {
    fn default() -> Self { Self::new() }
}

impl TeardownClock for ManualClock {
    fn now(&self) -> Instant { *self.now.lock().unwrap_or_else(PoisonError::into_inner) }

    fn pause(&self, _remaining: Duration) -> Duration { Duration::from_secs(3600) }

    fn attach(&self, wake: Wake) {
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(wake);
    }
}

/// Wakes the coordinator driver when a controllable clock advances.
#[derive(Clone, Debug)]
pub struct Wake {
    inner: Weak<Shared>,
}

impl Wake {
    /// Wakes every coordinator wait.
    pub fn wake(&self) {
        if let Some(inner) = self.inner.upgrade() {
            // Take and release the state lock so a wake cannot be lost between
            // the driver's last check and its `wait_timeout`.
            drop(inner.state.lock().unwrap_or_else(PoisonError::into_inner));
            inner.changed.notify_all();
        }
    }
}

/// Owns the single-instance claim so it can be released before relaunch.
pub trait InstanceOwner: Send + Sync {
    /// Releases the claim. Idempotent.
    fn release(&self);
}

/// The production instance claim, shared with the terminal action.
#[derive(Clone, Default)]
pub struct InstanceClaim {
    lock: Arc<Mutex<Option<InstanceLock>>>,
}

impl InstanceClaim {
    /// Takes ownership of one claimed lock.
    #[must_use]
    pub fn new(lock: InstanceLock) -> Self {
        Self {
            lock: Arc::new(Mutex::new(Some(lock))),
        }
    }
}

impl InstanceOwner for InstanceClaim {
    fn release(&self) {
        let taken = self.lock.lock().unwrap_or_else(PoisonError::into_inner).take();
        drop(taken);
    }
}

/// What the coordinator asks the process to do once, at the terminal point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalAction {
    /// Answer the pending `applicationShouldTerminate:` with `true` on main.
    Reply,
    /// Remove the tray and exit with this code.
    Exit(i32),
    /// Release the instance claim and relaunch.
    Restart,
}

/// Executes the one terminal action.
pub trait TerminalDriver: Send + Sync {
    /// Dispatches the action. Called exactly once per process.
    fn dispatch(&self, action: TerminalAction);
}

/// Answers one `applicationShouldTerminate:` without touching `AppKit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeardownAnswer {
    /// Keep waiting for the coordinated teardown, then reply `true`.
    Later,
    /// The outcome already exists; let `AppKit` terminate now.
    Now,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Running,
    Finished,
}

#[allow(clippy::struct_excessive_bools)] // Independent one-shot coordinator facts.
struct State {
    phase: Phase,
    cause: TeardownCause,
    first_request: Option<Instant>,
    deadline: Option<Instant>,
    escalated: bool,
    requested_code: i32,
    reply_pending: bool,
    host_worker: Option<(bool, Instant)>,
    outcome: Option<TeardownOutcome>,
    delivery: RecordDelivery,
    terminal: Option<TerminalAction>,
    dispatched: bool,
    bypass_recorded: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            cause: TeardownCause::Request,
            first_request: None,
            deadline: None,
            escalated: false,
            requested_code: 0,
            reply_pending: false,
            host_worker: None,
            outcome: None,
            delivery: RecordDelivery::Unavailable,
            terminal: None,
            dispatched: false,
            bypass_recorded: false,
        }
    }
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

/// One idempotent coordinator for every stop path in the process.
pub struct Teardown {
    inner: Arc<Shared>,
    clock: Arc<dyn TeardownClock>,
    records: Arc<dyn TeardownRecordChannel>,
    instance: Arc<dyn InstanceOwner>,
    diagnostics: Arc<dyn Logger>,
    terminal: OnceLock<Arc<dyn TerminalDriver>>,
    driver: Mutex<Option<JoinHandle<()>>>,
}

impl fmt::Debug for Teardown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Teardown")
            .field("outcome", &self.outcome())
            .finish_non_exhaustive()
    }
}

impl Teardown {
    /// Creates an idle coordinator. Nothing runs until [`Teardown::start`].
    #[must_use]
    pub fn new(
        clock: Arc<dyn TeardownClock>,
        records: Arc<dyn TeardownRecordChannel>,
        instance: Arc<dyn InstanceOwner>,
        diagnostics: Arc<dyn Logger>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Shared {
                state: Mutex::new(State::default()),
                changed: Condvar::new(),
            }),
            clock,
            records,
            instance,
            diagnostics,
            terminal: OnceLock::new(),
            driver: Mutex::new(None),
        })
    }

    /// Installs the one terminal driver. A second call is ignored.
    pub fn install_terminal(&self, terminal: Arc<dyn TerminalDriver>) {
        let _ = self.terminal.set(terminal);
    }

    /// Starts the dedicated coordinator thread.
    ///
    /// # Errors
    /// Returns the OS failure when the thread cannot be created.
    // The guard must span the spawn so two concurrent starts cannot create two
    // drivers; clippy's early-drop suggestion would allow that race.
    #[allow(clippy::significant_drop_tightening)]
    pub fn start(self: &Arc<Self>) -> io::Result<()> {
        let mut driver = self.driver.lock().unwrap_or_else(PoisonError::into_inner);
        if driver.is_some() {
            return Ok(());
        }
        let coordinator = Arc::clone(self);
        let handle = thread::Builder::new()
            .name("stache-teardown".to_owned())
            .spawn(move || coordinator.drive())?;
        *driver = Some(handle);
        Ok(())
    }

    /// Records one stop request without blocking.
    ///
    /// The first request sets the cause and the total deadline. A later one
    /// escalates: `min(current deadline, now + 250 ms)`. Requests after the
    /// terminal outcome are ignored.
    pub fn request(&self, cause: TeardownCause, code: Option<i32>) {
        self.record(cause, code, false);
    }

    /// Answers one `applicationShouldTerminate:`.
    ///
    /// Returns [`TeardownAnswer::Now`] after the outcome, otherwise records a
    /// pending reply without waiting.
    pub fn begin_terminate(&self) -> TeardownAnswer {
        if self.record(TeardownCause::Terminate, None, true) {
            TeardownAnswer::Later
        } else {
            TeardownAnswer::Now
        }
    }

    fn record(&self, cause: TeardownCause, code: Option<i32>, terminate: bool) -> bool {
        let now = self.clock.now();
        let mut state = self.lock();
        if state.phase == Phase::Finished {
            return false;
        }
        if terminate {
            state.reply_pending = true;
        }
        if let Some(code) = code
            && code != 0
            && state.requested_code == 0
        {
            state.requested_code = code;
        }
        match state.phase {
            Phase::Idle => {
                state.phase = Phase::Running;
                state.cause = cause;
                state.first_request = Some(now);
                state.deadline = Some(now + TEARDOWN_TOTAL);
            }
            Phase::Running => {
                state.escalated = true;
                let shortened = now + TEARDOWN_ESCALATION;
                if state.deadline.is_none_or(|deadline| shortened < deadline) {
                    state.deadline = Some(shortened);
                }
            }
            Phase::Finished => {}
        }
        drop(state);
        self.inner.changed.notify_all();
        true
    }

    /// Reports that the host worker finished its own teardown.
    ///
    /// `success` is `false` when setup failed or the registry reported an error
    /// while stopping. A late answer never changes a stored outcome.
    pub fn host_worker_finished(&self, success: bool) {
        // A setup failure or an early loop end can finish the worker before any
        // external request. It still gets exactly one coordinated outcome.
        if !self.requested() {
            self.request(TeardownCause::Request, None);
        }
        let at = self.clock.now();
        let mut state = self.lock();
        if state.phase == Phase::Running && state.host_worker.is_none() {
            state.host_worker = Some((success, at));
            drop(state);
            self.inner.changed.notify_all();
        }
    }

    /// Whether any stop request has arrived.
    #[must_use]
    pub fn requested(&self) -> bool { self.lock().phase != Phase::Idle }

    /// The stored outcome, if the coordinator reached its terminal point.
    #[must_use]
    pub fn outcome(&self) -> Option<TeardownOutcome> { self.lock().outcome.clone() }

    /// Whether the one record attempt reached the sink.
    #[must_use]
    pub fn delivery(&self) -> RecordDelivery { self.lock().delivery }

    /// The terminal action, once chosen.
    #[must_use]
    pub fn terminal_action(&self) -> Option<TerminalAction> { self.lock().terminal }

    /// The current deadline, if a request exists.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> { self.lock().deadline }

    /// Waits for the outcome, bounded by `timeout`.
    #[must_use]
    #[allow(clippy::significant_drop_tightening)] // The guard is the wait condition.
    pub fn wait_for_outcome(&self, timeout: Duration) -> Option<TeardownOutcome> {
        let state = self.lock();
        let (state, _) = self
            .inner
            .changed
            .wait_timeout_while(state, timeout, |state| state.phase != Phase::Finished)
            .unwrap_or_else(PoisonError::into_inner);
        state.outcome.clone()
    }

    /// Waits until the terminal action was dispatched, bounded by `timeout`.
    #[must_use]
    #[allow(clippy::significant_drop_tightening)] // The guard is the wait condition.
    pub fn wait_for_terminal(&self, timeout: Duration) -> bool {
        let state = self.lock();
        let (state, _) = self
            .inner
            .changed
            .wait_timeout_while(state, timeout, |state| !state.dispatched)
            .unwrap_or_else(PoisonError::into_inner);
        state.dispatched
    }

    /// Waits until a stop request exists, bounded by `timeout`.
    #[must_use]
    #[allow(clippy::significant_drop_tightening)] // The guard is the wait condition.
    pub fn wait_until_requested(&self, timeout: Duration) -> bool {
        let state = self.lock();
        let (state, _) = self
            .inner
            .changed
            .wait_timeout_while(state, timeout, |state| state.phase == Phase::Idle)
            .unwrap_or_else(PoisonError::into_inner);
        state.phase != Phase::Idle
    }

    /// Writes one bypass record when the process exits with no outcome.
    ///
    /// Returns `true` only for the first call on an unrecorded exit, so
    /// `RunEvent::Exit` cannot report the same bypass twice.
    pub fn note_unrecorded_exit(&self) -> bool {
        {
            let mut state = self.lock();
            if state.outcome.is_some() || state.bypass_recorded {
                return false;
            }
            state.bypass_recorded = true;
        }
        self.diagnostics.log(LogRecord {
            level: LogLevel::Error,
            module: ModuleId::new("host"),
            operation: Operation::Shutdown,
            message: "teardown: bypass path=Exit outcome=none".to_owned(),
        });
        true
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn drive(self: Arc<Self>) {
        self.clock.attach(Wake {
            inner: Arc::downgrade(&self.inner),
        });
        let mut state = self.lock();
        loop {
            match state.phase {
                Phase::Finished => break,
                Phase::Idle => {
                    state = self.inner.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
                }
                Phase::Running => {
                    let deadline = state.deadline.expect("a running teardown has a deadline");
                    // The record attempt is reserved inside the budget, so an
                    // expiring teardown still gets its one bounded write.
                    let cutoff = deadline.checked_sub(TEARDOWN_RECORD_BUDGET).unwrap_or(deadline);
                    let now = self.clock.now();
                    if let Some((success, at)) = state.host_worker {
                        if at <= deadline {
                            self.resolve(state, success, now);
                        } else {
                            self.resolve_deadline(state, now);
                        }
                        break;
                    }
                    if now >= cutoff {
                        self.resolve_deadline(state, now);
                        break;
                    }
                    let remaining = cutoff.saturating_duration_since(now);
                    let timeout = self.clock.pause(remaining);
                    if timeout.is_zero() {
                        continue;
                    }
                    state = self
                        .inner
                        .changed
                        .wait_timeout(state, timeout)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            }
        }
    }

    fn resolve(&self, state: MutexGuard<'_, State>, success: bool, now: Instant) {
        let result = if success {
            TeardownResult::Clean
        } else {
            TeardownResult::Partial
        };
        let failed = if success {
            Vec::new()
        } else {
            vec![HOST_WORKER]
        };
        self.finish(state, result, failed, Vec::new(), now);
    }

    fn resolve_deadline(&self, state: MutexGuard<'_, State>, now: Instant) {
        self.finish(
            state,
            TeardownResult::Uncertain,
            Vec::new(),
            vec![HOST_WORKER],
            now,
        );
    }

    fn finish(
        &self,
        mut state: MutexGuard<'_, State>,
        result: TeardownResult,
        failed: Vec<&'static str>,
        unacknowledged: Vec<&'static str>,
        now: Instant,
    ) {
        let first = state.first_request.unwrap_or(now);
        let recorded_code = if result == TeardownResult::Clean {
            state.requested_code
        } else {
            1
        };
        let outcome = TeardownOutcome {
            result,
            cause: state.cause,
            elapsed: now.saturating_duration_since(first),
            escalated: state.escalated,
            recorded_code,
            failed,
            unacknowledged,
        };
        let action = if state.reply_pending {
            TerminalAction::Reply
        } else if state.cause == TeardownCause::Restart && result == TeardownResult::Clean {
            TerminalAction::Restart
        } else {
            TerminalAction::Exit(recorded_code)
        };
        let remaining = state.deadline.map_or(Duration::ZERO, |deadline| {
            deadline.saturating_duration_since(now)
        });
        state.outcome = Some(outcome.clone());
        state.terminal = Some(action);
        state.phase = Phase::Finished;
        drop(state);
        self.inner.changed.notify_all();

        // The record is attempted after the slot is authoritative and before
        // the terminal action. Its bound comes out of the remaining budget; a
        // blocked or closed sink never delays termination.
        let budget = TEARDOWN_RECORD_BUDGET.min(remaining);
        let delivery = self.records.record(&outcome.to_string(), budget);
        self.lock().delivery = delivery;

        if action == TerminalAction::Restart {
            self.instance.release();
        }
        if let Some(terminal) = self.terminal.get() {
            terminal.dispatch(action);
        }
        self.lock().dispatched = true;
        self.inner.changed.notify_all();
    }
}

/// Logs one terminate-observation installation failure and keeps it visible.
pub fn report_terminate_install_failure(
    host: &crate::host::HostHandle,
    logger: &dyn Logger,
    error: &str,
) {
    logger.log(LogRecord {
        level: LogLevel::Error,
        module: ModuleId::new("host"),
        operation: Operation::Start,
        message: format!("terminate observation is unavailable: {error}"),
    });
    host.note_condition(format!("terminate observation is unavailable: {error}"));
}

/// Guards the single `AppKit` reply.
///
/// The terminal closure and, in the next part, the forced self-exit share this
/// claim: the first claimant wins, every later closure stays inert.
#[derive(Clone, Debug, Default)]
pub struct ReplyGuard {
    claimed: Arc<AtomicBool>,
}

impl ReplyGuard {
    /// Creates an unclaimed guard.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Claims the reply. Returns `true` exactly once per process.
    #[must_use]
    pub fn claim(&self) -> bool { !self.claimed.swap(true, Ordering::SeqCst) }
}

/// Installs SIGINT/SIGTERM/SIGHUP handling for one process.
pub struct SignalBridge {
    handle: Handle,
    thread: Option<JoinHandle<()>>,
}

impl SignalBridge {
    /// Starts a thread that calls `notify` for each termination signal.
    ///
    /// # Errors
    /// Returns the OS failure when the signal set cannot be registered or the
    /// thread cannot be created.
    pub fn install(notify: impl Fn() + Send + 'static) -> io::Result<Self> {
        let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
        let handle = signals.handle();
        let thread = thread::Builder::new().name("stache-signals".to_owned()).spawn(move || {
            for _signal in signals.forever() {
                notify();
            }
        })?;
        Ok(Self { handle, thread: Some(thread) })
    }

    /// Stops the signal thread, closes its handle and joins it.
    ///
    /// The coordinator calls this from its own thread during the terminal
    /// action, never on the main thread, so the join cannot block the event
    /// loop. A repeat signal is still observed until the outcome exists.
    pub fn stop(mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SignalBridge {
    fn drop(&mut self) {
        // Normal shutdown takes the bridge and calls `stop` off the main thread.
        // This fallback only releases the handle; it never blocks an unexpected
        // main-thread teardown on a join.
        self.handle.close();
        self.thread.take();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    use stache_sdk::DiscardLogger;

    use super::*;

    const LIMIT: Duration = Duration::from_secs(10);

    #[derive(Default)]
    struct RecordingRecords {
        lines: Mutex<Vec<String>>,
        budgets: Mutex<Vec<Duration>>,
    }

    impl TeardownRecordChannel for RecordingRecords {
        fn record(&self, line: &str, budget: Duration) -> RecordDelivery {
            self.lines.lock().unwrap_or_else(PoisonError::into_inner).push(line.to_owned());
            self.budgets.lock().unwrap_or_else(PoisonError::into_inner).push(budget);
            RecordDelivery::Delivered
        }
    }

    #[derive(Default)]
    struct RecordingInstance {
        released: AtomicBool,
    }

    impl InstanceOwner for RecordingInstance {
        fn release(&self) { self.released.store(true, Ordering::SeqCst); }
    }

    type Probe = Box<dyn Fn() -> bool + Send + Sync>;

    #[derive(Default)]
    struct RecordingTerminal {
        actions: Mutex<Vec<TerminalAction>>,
        probe: Mutex<Option<Probe>>,
        probe_result: Mutex<Option<bool>>,
    }

    impl RecordingTerminal {
        fn actions(&self) -> Vec<TerminalAction> {
            self.actions.lock().unwrap_or_else(PoisonError::into_inner).clone()
        }
    }

    impl TerminalDriver for RecordingTerminal {
        fn dispatch(&self, action: TerminalAction) {
            let result = self
                .probe
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .map(|probe| probe());
            *self.probe_result.lock().unwrap_or_else(PoisonError::into_inner) = result;
            self.actions.lock().unwrap_or_else(PoisonError::into_inner).push(action);
        }
    }

    #[derive(Default)]
    struct RecordingLogger {
        records: Mutex<Vec<LogRecord>>,
    }

    impl Logger for RecordingLogger {
        fn log(&self, record: LogRecord) {
            self.records.lock().unwrap_or_else(PoisonError::into_inner).push(record);
        }
    }

    struct Fixture {
        teardown: Arc<Teardown>,
        terminal: Arc<RecordingTerminal>,
        records: Arc<RecordingRecords>,
        clock: Arc<ManualClock>,
        instance: Arc<RecordingInstance>,
    }

    fn fixture() -> Fixture {
        let clock = Arc::new(ManualClock::new());
        let records = Arc::new(RecordingRecords::default());
        let instance = Arc::new(RecordingInstance::default());
        let terminal = Arc::new(RecordingTerminal::default());
        let teardown = Teardown::new(
            clock.clone(),
            records.clone(),
            instance.clone(),
            Arc::new(DiscardLogger),
        );
        teardown.install_terminal(terminal.clone());
        teardown.start().expect("start the coordinator");
        Fixture {
            teardown,
            terminal,
            records,
            clock,
            instance,
        }
    }

    fn outcome(fixture: &Fixture) -> TeardownOutcome {
        fixture.teardown.wait_for_outcome(LIMIT).expect("stored outcome")
    }

    fn terminal(fixture: &Fixture) {
        assert!(
            fixture.teardown.wait_for_terminal(LIMIT),
            "terminal action dispatched"
        );
    }

    fn lines(fixture: &Fixture) -> Vec<String> {
        fixture.records.lines.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    #[test]
    fn the_gate_blocks_until_allowed() {
        let gate = ExitGate::new();
        assert!(!gate.is_allowed());
        gate.allow();
        assert!(gate.is_allowed());
    }

    #[test]
    fn recording_an_exit_code_allows_exit_and_keeps_the_outcome() {
        let gate = ExitGate::new();
        assert_eq!(gate.exit_code(), None);
        gate.record_exit(1);
        assert!(gate.is_allowed());
        assert_eq!(gate.exit_code(), Some(1));
        gate.record_exit(0);
        assert_eq!(gate.exit_code(), Some(0));
    }

    #[test]
    fn an_allowed_gate_lets_every_exit_request_through() {
        for code in [None, Some(0), Some(1), Some(70)] {
            assert_eq!(exit_request_action(code, true), ExitAction::Allow);
        }
    }

    #[test]
    fn a_closed_last_window_only_prevents_the_exit() {
        assert_eq!(exit_request_action(None, false), ExitAction::PreventOnly);
        assert_eq!(exit_request_action(None, true), ExitAction::Allow);
    }

    #[test]
    fn an_explicit_quit_code_always_starts_the_shutdown() {
        for code in [Some(0), Some(1), Some(70)] {
            assert_eq!(exit_request_action(code, false), ExitAction::PreventAndShutdown);
        }
    }

    #[test]
    fn the_signal_bridge_installs_and_stops_without_a_signal() {
        let (sender, receiver) = mpsc::channel();
        let bridge = SignalBridge::install(move || {
            let _ = sender.send(());
        })
        .expect("signals");
        assert!(receiver.try_recv().is_err());
        bridge.stop();
    }

    #[test]
    fn the_reply_guard_can_be_claimed_only_once() {
        let guard = ReplyGuard::new();
        assert!(guard.claim());
        assert!(!guard.claim());
        assert!(ReplyGuard::new().claim(), "each process gets its own guard");
    }

    #[test]
    fn a_clean_host_answer_stores_one_outcome_before_dispatch() {
        let fixture = fixture();
        let teardown = Arc::clone(&fixture.teardown);
        *fixture.terminal.probe.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Box::new(move || teardown.outcome().is_some()));

        fixture.teardown.request(TeardownCause::Request, None);
        fixture.teardown.host_worker_finished(true);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Clean);
        assert_eq!(outcome.cause, TeardownCause::Request);
        assert!(outcome.elapsed.is_zero());
        assert!(!outcome.escalated);
        assert_eq!(outcome.recorded_code, 0);
        assert!(outcome.failed.is_empty());
        assert!(outcome.unacknowledged.is_empty());
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(0)]);
        assert_eq!(
            *fixture.terminal.probe_result.lock().unwrap_or_else(PoisonError::into_inner),
            Some(true),
            "the slot is authoritative before the terminal action"
        );
        assert_eq!(fixture.teardown.delivery(), RecordDelivery::Delivered);
        assert_eq!(lines(&fixture), vec![
            "teardown: outcome=clean cause=request elapsed_ms=0 escalated=false \
                 recorded_code=0 failed=[] unacknowledged=[]"
                .to_owned()
        ]);
    }

    #[test]
    fn a_failed_host_answer_is_partial_with_code_one() {
        let fixture = fixture();
        fixture.teardown.request(TeardownCause::Request, Some(7));
        fixture.teardown.host_worker_finished(false);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Partial);
        assert_eq!(outcome.recorded_code, 1);
        assert_eq!(outcome.failed, vec![HOST_WORKER]);
        assert!(outcome.unacknowledged.is_empty());
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(1)]);
        assert_eq!(lines(&fixture).len(), 1);
    }

    #[test]
    fn an_unanswered_host_worker_is_uncertain_at_the_deadline() {
        let fixture = fixture();
        fixture.teardown.request(TeardownCause::Request, None);
        fixture.clock.advance(TEARDOWN_TOTAL);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
        assert_eq!(outcome.recorded_code, 1);
        assert!(outcome.failed.is_empty());
        assert_eq!(outcome.unacknowledged, vec![HOST_WORKER]);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(1)]);
    }

    #[test]
    fn an_expiring_teardown_keeps_its_reserved_record_budget() {
        let fixture = fixture();
        fixture.teardown.request(TeardownCause::Request, None);
        let owner_wait = TEARDOWN_TOTAL.saturating_sub(TEARDOWN_RECORD_BUDGET);
        fixture.clock.advance(owner_wait.saturating_sub(Duration::from_millis(1)));
        assert!(
            fixture.teardown.outcome().is_none(),
            "the owner wait lasts until the reservation starts"
        );
        fixture.clock.advance(Duration::from_millis(1));

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
        assert_eq!(outcome.elapsed, owner_wait);
        terminal(&fixture);
        assert_eq!(
            *fixture.records.budgets.lock().unwrap_or_else(PoisonError::into_inner),
            vec![TEARDOWN_RECORD_BUDGET]
        );
        assert_eq!(fixture.teardown.delivery(), RecordDelivery::Delivered);
    }

    #[test]
    fn a_later_request_shortens_the_deadline_and_a_third_changes_nothing() {
        let fixture = fixture();
        fixture.teardown.request(TeardownCause::Request, None);
        let first = fixture.teardown.deadline().expect("first deadline");
        fixture.clock.advance(Duration::from_secs(1));
        fixture.teardown.request(TeardownCause::Request, Some(7));
        let second = fixture.teardown.deadline().expect("shortened deadline");
        assert_eq!(
            first.duration_since(second),
            Duration::from_millis(3750),
            "the later request must shorten the deadline to 250 ms from itself"
        );
        fixture.clock.advance(Duration::from_millis(100));
        fixture.teardown.request(TeardownCause::Request, Some(3));
        assert_eq!(
            fixture.teardown.deadline(),
            Some(second),
            "a third request must not extend or change the deadline"
        );
        fixture.teardown.host_worker_finished(true);
        let outcome = outcome(&fixture);
        assert!(outcome.escalated);
        assert_eq!(outcome.recorded_code, 7, "the first non-zero code wins");
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(7)]);
    }

    #[test]
    fn a_request_at_4900_ms_cannot_push_the_total_past_five_seconds() {
        let fixture = fixture();
        fixture.teardown.request(TeardownCause::Request, None);
        let first = fixture.teardown.deadline().expect("first deadline");
        fixture.clock.advance(Duration::from_millis(4900));
        fixture.teardown.request(TeardownCause::Request, None);
        assert_eq!(
            fixture.teardown.deadline(),
            Some(first),
            "a request at 4.9 s must stay capped by the total"
        );
        fixture.clock.advance(Duration::from_millis(200));
        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
    }

    #[test]
    fn a_terminate_request_keeps_the_reply_pending_and_answers_later() {
        let fixture = fixture();
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        fixture.teardown.host_worker_finished(true);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Clean);
        assert_eq!(outcome.cause, TeardownCause::Terminate);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Reply]);
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Now);
        assert_eq!(lines(&fixture).len(), 1);
    }

    #[test]
    fn a_repeat_terminate_escalates_without_a_second_reply() {
        let fixture = fixture();
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        let first = fixture.teardown.deadline().expect("first deadline");
        fixture.clock.advance(Duration::from_secs(1));
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        assert!(fixture.teardown.deadline().expect("shortened") < first);
        fixture.teardown.host_worker_finished(true);

        let outcome = outcome(&fixture);
        assert!(outcome.escalated);
        terminal(&fixture);
        assert_eq!(
            fixture.terminal.actions(),
            vec![TerminalAction::Reply],
            "a repeat terminate must not add a second reply"
        );
        assert_eq!(lines(&fixture).len(), 1);
    }

    #[test]
    fn a_deadline_expiry_with_a_pending_reply_still_replies_once() {
        let fixture = fixture();
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        fixture.clock.advance(TEARDOWN_TOTAL);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Reply]);
    }

    #[test]
    fn a_clean_restart_releases_the_lock_before_the_fake_relaunch() {
        let fixture = fixture();
        let instance = Arc::clone(&fixture.instance);
        *fixture.terminal.probe.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Box::new(move || instance.released.load(Ordering::SeqCst)));

        fixture.teardown.request(TeardownCause::Restart, None);
        fixture.teardown.host_worker_finished(true);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Clean);
        assert_eq!(outcome.cause, TeardownCause::Restart);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Restart]);
        assert!(fixture.instance.released.load(Ordering::SeqCst));
        assert_eq!(
            *fixture.terminal.probe_result.lock().unwrap_or_else(PoisonError::into_inner),
            Some(true),
            "the instance claim must be released before the relaunch"
        );
        assert!(lines(&fixture)[0].contains("cause=restart"));
    }

    #[test]
    fn a_partial_restart_finishes_with_status_one_and_never_relaunches() {
        let fixture = fixture();
        fixture.teardown.request(TeardownCause::Restart, None);
        fixture.teardown.host_worker_finished(false);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Partial);
        assert_eq!(outcome.recorded_code, 1);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(1)]);
        assert!(!fixture.instance.released.load(Ordering::SeqCst));
        assert_eq!(lines(&fixture).len(), 1);
        assert!(lines(&fixture)[0].contains("cause=restart"));
    }

    #[test]
    fn an_uncertain_restart_finishes_with_status_one_and_never_relaunches() {
        let fixture = fixture();
        fixture.teardown.request(TeardownCause::Restart, None);
        fixture.clock.advance(TEARDOWN_TOTAL);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(1)]);
        assert!(!fixture.instance.released.load(Ordering::SeqCst));
    }

    #[test]
    fn a_restart_with_a_pending_reply_never_relaunches() {
        let fixture = fixture();
        fixture.teardown.request(TeardownCause::Restart, None);
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        fixture.teardown.host_worker_finished(true);

        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Reply]);
        assert!(!fixture.instance.released.load(Ordering::SeqCst));
    }

    #[test]
    fn concurrent_requests_from_real_threads_produce_one_run_and_one_outcome() {
        let fixture = fixture();
        let causes = [
            TeardownCause::Request,
            TeardownCause::Exit,
            TeardownCause::Restart,
            TeardownCause::Request,
        ];
        let mut threads = Vec::new();
        for (index, cause) in causes.into_iter().enumerate() {
            let teardown = Arc::clone(&fixture.teardown);
            threads.push(thread::spawn(move || {
                teardown.request(cause, Some(i32::try_from(index).unwrap() + 1));
            }));
        }
        let teardown = Arc::clone(&fixture.teardown);
        threads.push(thread::spawn(move || {
            let _ = teardown.begin_terminate();
        }));
        for thread in threads {
            thread.join().expect("request thread");
        }

        fixture.teardown.host_worker_finished(true);
        let first = outcome(&fixture);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions().len(), 1);
        assert_eq!(lines(&fixture).len(), 1);
        fixture.teardown.request(TeardownCause::Restart, Some(9));
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Now);
        assert_eq!(fixture.terminal.actions().len(), 1);
        assert_eq!(lines(&fixture).len(), 1);
        assert_eq!(fixture.teardown.outcome(), Some(first));
    }

    #[test]
    fn a_blocked_record_sink_is_unavailable_and_never_delays_the_terminal_action() {
        struct HeldSink {
            started: Mutex<Option<mpsc::Sender<()>>>,
            release: Mutex<mpsc::Receiver<()>>,
        }

        impl TeardownRecordSink for HeldSink {
            fn write(&self, _line: &str) {
                let started = self.started.lock().unwrap_or_else(PoisonError::into_inner).take();
                if let Some(started) = started {
                    let _ = started.send(());
                }
                let _ = self.release.lock().unwrap_or_else(PoisonError::into_inner).recv();
            }
        }

        let (started, began) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let sink = Arc::new(HeldSink {
            started: Mutex::new(Some(started)),
            release: Mutex::new(released),
        });
        let terminal = Arc::new(RecordingTerminal::default());
        let teardown = Teardown::new(
            Arc::new(ManualClock::new()),
            Arc::new(BoundedRecordChannel::new(sink)),
            Arc::new(RecordingInstance::default()),
            Arc::new(DiscardLogger),
        );
        teardown.install_terminal(terminal.clone());
        teardown.start().expect("start");

        teardown.request(TeardownCause::Request, None);
        teardown.host_worker_finished(true);
        let dispatched = Instant::now();
        assert!(teardown.wait_for_terminal(LIMIT));
        assert!(
            dispatched.elapsed() < Duration::from_secs(1),
            "a blocked sink must not delay the terminal action"
        );
        assert_eq!(teardown.delivery(), RecordDelivery::Unavailable);
        assert_eq!(terminal.actions(), vec![TerminalAction::Exit(0)]);
        began.recv_timeout(LIMIT).expect("the sink received the attempt");
        release.send(()).expect("release the sink");
    }

    #[test]
    fn a_bypass_record_is_written_once_without_an_outcome() {
        let logger = Arc::new(RecordingLogger::default());
        let teardown = Teardown::new(
            Arc::new(ManualClock::new()),
            Arc::new(DiscardRecordChannel),
            Arc::new(RecordingInstance::default()),
            logger.clone(),
        );
        assert!(teardown.note_unrecorded_exit());
        assert!(!teardown.note_unrecorded_exit());
        let records = logger.records.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].level, LogLevel::Error);
        assert!(records[0].message.contains("bypass"));
    }

    #[test]
    fn a_recorded_outcome_suppresses_the_bypass_record() {
        let logger = Arc::new(RecordingLogger::default());
        let teardown = Teardown::new(
            Arc::new(ManualClock::new()),
            Arc::new(DiscardRecordChannel),
            Arc::new(RecordingInstance::default()),
            logger.clone(),
        );
        teardown.install_terminal(Arc::new(RecordingTerminal::default()));
        teardown.start().expect("start");
        teardown.request(TeardownCause::Request, None);
        teardown.host_worker_finished(true);
        assert!(teardown.wait_for_outcome(LIMIT).is_some());
        assert!(!teardown.note_unrecorded_exit());
        assert!(
            logger.records.lock().unwrap_or_else(PoisonError::into_inner).is_empty(),
            "a recorded outcome needs no bypass record"
        );
    }

    #[test]
    fn the_outcome_line_keeps_its_stable_field_and_owner_order() {
        let outcome = TeardownOutcome {
            result: TeardownResult::Uncertain,
            cause: TeardownCause::Terminate,
            elapsed: Duration::from_millis(5000),
            escalated: true,
            recorded_code: 1,
            failed: Vec::new(),
            unacknowledged: TEARDOWN_OWNERS.to_vec(),
        };
        assert_eq!(
            outcome.to_string(),
            "teardown: outcome=uncertain cause=terminate elapsed_ms=5000 escalated=true \
             recorded_code=1 failed=[] unacknowledged=[host-worker]"
        );
    }

    #[test]
    fn a_request_from_another_thread_is_observed() {
        let fixture = fixture();
        let teardown = Arc::clone(&fixture.teardown);
        let thread = thread::spawn(move || teardown.request(TeardownCause::Request, None));
        thread.join().expect("request");
        assert!(fixture.teardown.wait_until_requested(LIMIT));
        fixture.teardown.host_worker_finished(true);
        assert_eq!(outcome(&fixture).result, TeardownResult::Clean);
    }
}
