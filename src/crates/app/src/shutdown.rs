//! Exit convergence shared by the tray, Tauri's exit event, `AppKit`'s
//! terminate decision and POSIX signals.
//!
//! Every path feeds the same [`Teardown`] coordinator: tray Quit and signals
//! through [`HostHandle::request_shutdown`](crate::host::HostHandle), Tauri's
//! exit event through `request_shutdown_with_code`, `AppKit`'s terminate
//! decision through [`Teardown::begin_terminate`], and a coordinated restart
//! through [`HostHandle::request_restart`](crate::host::HostHandle). A request
//! only records a deadline and returns; a dedicated thread owns the state
//! machine, runs the ordered stages, stores exactly one [`TeardownOutcome`] and
//! dispatches one terminal action. Every stage and owner wait is bounded by the
//! coordinator's live deadline, so a blocked owner cannot keep the process
//! alive past the total.
//!
//! A stalled main thread cannot stall the terminal action forever: the
//! coordinator shares one [`TerminalProgress`] claim with the main-thread
//! closure and forces this process to exit when the action was never delivered.
//! Signal handling uses `signal-hook` rather than project-authored unsafe code.

use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::sync::mpsc::{self, Sender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::{Handle, Signals};
use stache_sdk::shutdown::{
    CANCELLATION_QUANTUM, OWNER_WAIT_BUDGET, ShutdownWait, wait_for_finish,
};
use stache_sdk::{
    InstanceLock, LogLevel, LogRecord, Logger, MainThreadExecutor, ModuleId, Operation,
};

/// Total time from the first stop request to the terminal action.
pub const TEARDOWN_TOTAL: Duration = Duration::from_secs(5);

/// Deadline applied by a later stop request.
pub const TEARDOWN_ESCALATION: Duration = Duration::from_millis(250);

/// One bounded teardown record attempt.
pub const TEARDOWN_RECORD_BUDGET: Duration = Duration::from_millis(50);

/// The host worker owner name, reported when the runner recorded a setup
/// failure or did not quiesce inside its bound.
pub const HOST_WORKER: &str = "host-worker";

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
    /// Owners that answered with a failure, in stage order.
    pub failed: Vec<String>,
    /// Owners that had not answered at the deadline, in stage order.
    pub unacknowledged: Vec<String>,
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

fn owner_list(owners: &[String]) -> String { format!("[{}]", owners.join(",")) }

/// One native cleanup slot, in the approved order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeSlot {
    /// Keep Awake's power assertion.
    PowerAssertion,
    /// Keybindings' event tap.
    InputTap,
    /// Application observers, including noTunes' `ApplicationControl` observer.
    ApplicationObservers,
    /// Hold-to-Quit's HUD.
    Hud,
    /// Host-owned windows.
    Windows,
    /// The control socket.
    Socket,
    /// The tray item.
    Tray,
    /// The webview and its window.
    Webview,
}

impl NativeSlot {
    /// The approved cleanup order.
    pub const ORDER: [Self; 8] = [
        Self::PowerAssertion,
        Self::InputTap,
        Self::ApplicationObservers,
        Self::Hud,
        Self::Windows,
        Self::Socket,
        Self::Tray,
        Self::Webview,
    ];

    /// Stable record spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PowerAssertion => "power-assertion",
            Self::InputTap => "input-tap",
            Self::ApplicationObservers => "application-observers",
            Self::Hud => "hud",
            Self::Windows => "windows",
            Self::Socket => "socket",
            Self::Tray => "tray",
            Self::Webview => "webview",
        }
    }
}

/// What one stage or one native slot reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SlotOutcome {
    /// The owner released the resource and reported success.
    #[default]
    Acknowledged,
    /// Nothing owns this slot today, so there was nothing to acknowledge.
    Empty,
    /// The owner reported a failure.
    Failed,
    /// The owner did not answer inside its bound and was abandoned.
    Unacknowledged,
}

/// Owner names one stage reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StageReport {
    /// Owners that reported a failure.
    pub failed: Vec<String>,
    /// Owners that did not answer inside their bound.
    pub unacknowledged: Vec<String>,
}

/// One completion answer. The first answer wins and clones share it.
#[derive(Clone, Debug, Default)]
pub struct OwnerAnswer {
    state: Arc<AtomicU8>,
}

/// The state of one owner acknowledgement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OwnerState {
    /// The owner has not answered yet.
    #[default]
    Pending,
    /// The owner released its resource.
    Succeeded,
    /// The owner reported that it could not release its resource.
    Failed,
}

impl OwnerAnswer {
    /// Creates an unanswered cell.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Records success unless an answer already exists.
    pub fn succeed(&self) { self.answer(OwnerState::Succeeded); }

    /// Records failure unless an answer already exists.
    pub fn fail(&self) { self.answer(OwnerState::Failed); }

    /// The stored answer.
    #[must_use]
    pub fn state(&self) -> OwnerState {
        match self.state.load(Ordering::SeqCst) {
            1 => OwnerState::Succeeded,
            2 => OwnerState::Failed,
            _ => OwnerState::Pending,
        }
    }

    /// Whether any answer exists.
    #[must_use]
    pub fn is_answered(&self) -> bool { self.state() != OwnerState::Pending }

    fn answer(&self, state: OwnerState) {
        let value = match state {
            OwnerState::Pending => return,
            OwnerState::Succeeded => 1,
            OwnerState::Failed => 2,
        };
        let _ = self.state.compare_exchange(0, value, Ordering::SeqCst, Ordering::SeqCst);
    }
}

/// One native resource that acknowledges its own release.
///
/// A later owner implements this instead of adding a second shutdown path.
pub trait NativeOwner: Send + Sync {
    /// Stable diagnostic name used in the teardown record.
    fn name(&self) -> &'static str;

    /// Whether release requires the main thread, sharing the one hop budget.
    fn on_main(&self) -> bool;

    /// Starts the release without blocking the caller and returns its answer.
    fn release(&self) -> OwnerAnswer;
}

/// A [`NativeOwner`] whose release runs as one main-thread hop.
///
/// `release()` only enqueues; the returned answer is completed by that
/// closure's own return value, never by the dispatch itself.
pub struct MainHopOwner {
    name: &'static str,
    executor: Arc<dyn MainThreadExecutor>,
    release: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}

impl MainHopOwner {
    /// Wraps one main-thread release closure under a stable name.
    #[must_use]
    pub fn new(
        name: &'static str,
        executor: Arc<dyn MainThreadExecutor>,
        release: impl Fn() -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            executor,
            release: Arc::new(release),
        }
    }
}

impl NativeOwner for MainHopOwner {
    fn name(&self) -> &'static str { self.name }

    fn on_main(&self) -> bool { true }

    fn release(&self) -> OwnerAnswer {
        let answer = OwnerAnswer::new();
        let closure_answer = answer.clone();
        let release = Arc::clone(&self.release);
        let dispatched = self.executor.execute(Box::new(move || match release() {
            Ok(()) => closure_answer.succeed(),
            Err(_) => closure_answer.fail(),
        }));
        if dispatched.is_err() {
            answer.fail();
        }
        answer
    }
}

/// Runs the ordered teardown stages for one process.
pub trait TeardownStages: Send + Sync {
    /// Stage 1. Must not join and must not take a contended registry or
    /// lifecycle lock.
    fn invalidate(&self);

    /// Stage 2: watcher, ticker, startup job and the IPC accept worker, bounded.
    fn reap_monitors(&self, wait: Arc<dyn ShutdownWait>) -> StageReport;

    /// Stage 3: registry targets in reverse start order, one module at a time.
    fn stop_registry(&self, wait: Arc<dyn ShutdownWait>) -> StageReport;

    /// Stage 4: exactly one native slot.
    fn cleanup(&self, slot: NativeSlot, wait: Arc<dyn ShutdownWait>) -> SlotOutcome;

    /// Stage 5: publish the one terminal host status (non-blocking).
    fn publish_terminal(&self);
}

/// The one forced exit on the teardown path. Faked in tests.
pub trait ExitProcess: Send + Sync {
    /// Ends this process with `code`.
    fn exit(&self, code: i32);
}

/// The production [`ExitProcess`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemExit;

impl ExitProcess for SystemExit {
    fn exit(&self, code: i32) { std::process::exit(code); }
}

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
        if budget.is_zero() {
            // Handed off without waiting: the writer may still flush the line
            // before the process exits, but no deadline is extended for it.
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
    /// The system clock returns `remaining`; [`ManualClock`] returns a short
    /// slice, since only its explicit [`ManualClock::advance`] moves time.
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

    // A short real pause: time still moves only through `advance`, and an
    // `advance` landing between a driver's clock read and its wait cannot strand
    // that wait, because the driver rereads the clock after this slice.
    fn pause(&self, remaining: Duration) -> Duration { remaining.min(Duration::from_millis(1)) }

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

impl TerminalAction {
    /// Whether this action is the `AppKit` terminate reply.
    #[must_use]
    pub const fn is_reply(self) -> bool { matches!(self, Self::Reply) }
}

/// The one claim shared by the main-thread closure and the forced exit.
///
/// Acceptance means the closure started: it claims before replying or exiting.
/// Delivery is tracked separately, so acceptance alone cannot suppress a forced
/// exit after the deadline. A closure that loses the claim is inert.
#[derive(Clone, Debug, Default)]
pub struct TerminalProgress {
    inner: Arc<Mutex<ProgressState>>,
}

#[derive(Debug, Default)]
struct ProgressState {
    claimed: bool,
    claimed_at: Option<Instant>,
    delivered: bool,
}

impl TerminalProgress {
    /// Creates an unclaimed, undelivered cell.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Claims the one reply/exit slot. True exactly once; `now` is recorded.
    #[must_use]
    pub fn claim(&self, now: Instant) -> bool {
        let mut state = self.lock();
        if state.claimed {
            return false;
        }
        state.claimed = true;
        state.claimed_at = Some(now);
        true
    }

    /// Records that the terminal action completed on its own thread.
    pub fn mark_delivered(&self) { self.lock().delivered = true; }

    /// Whether the closure or the forced exit claimed the action.
    #[must_use]
    pub fn claimed(&self) -> bool { self.lock().claimed }

    /// When the claim happened.
    #[must_use]
    pub fn claimed_at(&self) -> Option<Instant> { self.lock().claimed_at }

    /// Whether the action completed.
    #[must_use]
    pub fn delivered(&self) -> bool { self.lock().delivered }

    fn lock(&self) -> MutexGuard<'_, ProgressState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Executes the one terminal action.
pub trait TerminalDriver: Send + Sync {
    /// Dispatches the action and returns the shared claim/delivery cell.
    ///
    /// Called exactly once per process.
    fn dispatch(&self, action: TerminalAction) -> TerminalProgress;

    /// Stage 6: opens the exit gate with the recorded code.
    fn open_gate(&self, code: i32) { let _ = code; }

    /// Stage 5 tail: closes the signal bridge.
    fn close_signals(&self) {}
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
    outcome: Option<TeardownOutcome>,
    delivery: RecordDelivery,
    terminal: Option<TerminalAction>,
    terminal_evidence: Option<String>,
    terminal_delivery: RecordDelivery,
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
            outcome: None,
            delivery: RecordDelivery::Unavailable,
            terminal: None,
            terminal_evidence: None,
            terminal_delivery: RecordDelivery::Unavailable,
            dispatched: false,
            bypass_recorded: false,
        }
    }
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

/// A [`ShutdownWait`] owned by the coordinator.
///
/// `wait` blocks on the state condition variable the drive loop uses, so a
/// later request and [`ManualClock::advance`] both wake an owner wait. On the
/// system clock the pause is exactly the requested timeout.
pub struct TeardownWait {
    teardown: Arc<Teardown>,
}

impl ShutdownWait for TeardownWait {
    fn now(&self) -> Instant { self.teardown.clock.now() }

    // Owner waits end at the cutoff, not the deadline: the reserved budget pays
    // for the record and the terminal dispatch, so a hung owner cannot leave the
    // reply unclaimed at the deadline.
    fn deadline(&self) -> Instant { self.teardown.cutoff() }

    #[allow(clippy::significant_drop_tightening)] // The guard is the wait condition.
    fn wait(&self, timeout: Duration) {
        if timeout.is_zero() {
            return;
        }
        let pause = self.teardown.clock.pause(timeout);
        let state = self.teardown.lock();
        drop(
            self.teardown
                .inner
                .changed
                .wait_timeout(state, pause)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

/// One native owner registered into a cleanup slot.
#[derive(Clone)]
struct OwnerRegistration {
    slot: NativeSlot,
    owner: Arc<dyn NativeOwner>,
}

/// One idempotent coordinator for every stop path in the process.
pub struct Teardown {
    inner: Arc<Shared>,
    clock: Arc<dyn TeardownClock>,
    records: Arc<dyn TeardownRecordChannel>,
    instance: Arc<dyn InstanceOwner>,
    diagnostics: Arc<dyn Logger>,
    stages: OnceLock<Arc<dyn TeardownStages>>,
    exit: OnceLock<Arc<dyn ExitProcess>>,
    terminal: OnceLock<Arc<dyn TerminalDriver>>,
    owners: Mutex<Vec<OwnerRegistration>>,
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
            stages: OnceLock::new(),
            exit: OnceLock::new(),
            terminal: OnceLock::new(),
            owners: Mutex::new(Vec::new()),
            driver: Mutex::new(None),
        })
    }

    /// Registers one native owner into its cleanup slot.
    ///
    /// Registrations are snapshotted when the run starts, in registration
    /// order within a slot. Call before [`Teardown::start`] or before the first
    /// request so a request cannot race the registration.
    pub fn register_owner(&self, slot: NativeSlot, owner: Arc<dyn NativeOwner>) {
        self.owners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(OwnerRegistration { slot, owner });
    }

    /// Installs the one stage driver. A second call is ignored.
    ///
    /// Required before [`Teardown::start`]; without it the coordinator stores an
    /// internal `Uncertain` outcome naming `stages`.
    pub fn install_stages(&self, stages: Arc<dyn TeardownStages>) {
        let _ = self.stages.set(stages);
    }

    /// Installs the one forced exit. A second call is ignored.
    ///
    /// Tests omit it so a forced exit records evidence without ending the test
    /// process.
    pub fn install_exit(&self, exit: Arc<dyn ExitProcess>) { let _ = self.exit.set(exit); }

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

    /// Whether any stop request has arrived.
    #[must_use]
    pub fn requested(&self) -> bool { self.lock().phase != Phase::Idle }

    /// The stored outcome, if the coordinator reached its terminal point.
    #[must_use]
    pub fn outcome(&self) -> Option<TeardownOutcome> { self.lock().outcome.clone() }

    /// Whether the one outcome record attempt reached the sink.
    #[must_use]
    pub fn delivery(&self) -> RecordDelivery { self.lock().delivery }

    /// The stored terminal evidence, if the terminal path recorded any.
    #[must_use]
    pub fn terminal_evidence(&self) -> Option<String> { self.lock().terminal_evidence.clone() }

    /// Whether the terminal evidence record attempt reached the sink.
    #[must_use]
    pub fn terminal_delivery(&self) -> RecordDelivery { self.lock().terminal_delivery }

    /// The terminal action, once chosen.
    #[must_use]
    pub fn terminal_action(&self) -> Option<TerminalAction> { self.lock().terminal }

    /// The current deadline, if a request exists.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> { self.lock().deadline }

    /// Waits for the stored terminal evidence, bounded by `timeout`.
    #[must_use]
    #[allow(clippy::significant_drop_tightening)] // The guard is the wait condition.
    pub fn wait_for_terminal_evidence(&self, timeout: Duration) -> Option<String> {
        let state = self.lock();
        let (state, _) = self
            .inner
            .changed
            .wait_timeout_while(state, timeout, |state| state.terminal_evidence.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        state.terminal_evidence.clone()
    }

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

    #[allow(clippy::significant_drop_tightening)] // The guard is the wait condition.
    fn drive(self: Arc<Self>) {
        self.clock.attach(Wake {
            inner: Arc::downgrade(&self.inner),
        });
        {
            let mut state = self.lock();
            while state.phase == Phase::Idle {
                state = self.inner.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
        }
        self.run();
    }

    /// Runs one ordered teardown and its terminal action.
    #[allow(clippy::too_many_lines)] // One ordered bounded sequence, by design.
    fn run(self: &Arc<Self>) {
        let Some(stages) = self.stages.get().map(Arc::clone) else {
            // No driver was installed, so no stage can be attempted. The
            // outcome, the record, the exit gate and the terminal action still
            // run: returning here would leave a prevented exit request waiting
            // for a coordinator that already finished.
            self.finish(
                StageReport {
                    failed: Vec::new(),
                    unacknowledged: vec![STAGES.to_owned()],
                },
                self.clock.now(),
            );
            return;
        };
        let wait: Arc<dyn ShutdownWait> = Arc::new(TeardownWait { teardown: Arc::clone(self) });
        if self.deadline().is_none() {
            self.finish(
                StageReport {
                    failed: Vec::new(),
                    unacknowledged: vec![STAGES.to_owned()],
                },
                self.clock.now(),
            );
            return;
        }
        let mut report = StageReport::default();
        // Registrations are snapshotted when the run starts, so a later
        // registration cannot join a stage that already ran.
        let owners = self.owners.lock().unwrap_or_else(PoisonError::into_inner).clone();

        // Stage 1: invalidation is lock-free and must happen before any join.
        stages.invalidate();

        // Stage 2: watcher, ticker, startup job and socket acceptance.
        if self.clock.now() < self.cutoff() {
            merge_stage(&mut report, stages.reap_monitors(Arc::clone(&wait)));
        } else {
            note_unacknowledged(&mut report, MONITORS_STAGE);
        }

        // Stage 3: registry targets in reverse start order.
        if self.clock.now() < self.cutoff() {
            merge_stage(&mut report, stages.stop_registry(Arc::clone(&wait)));
        } else {
            note_unacknowledged(&mut report, REGISTRY_STAGE);
        }

        // Stage 4: native cleanup slots, in the approved order. A registered
        // owner answers through its own release; a dispatch is never an answer.
        let mut late = false;
        let mut main_stalled = false;
        for slot in NativeSlot::ORDER {
            let past_cutoff = late || self.clock.now() >= self.cutoff();
            late = past_cutoff;
            // Past the cutoff the driver's cleanup still runs, without a wait,
            // and reports what it could not complete. An empty slot owns nothing,
            // so it is never named.
            match stages.cleanup(slot, Arc::clone(&wait)) {
                SlotOutcome::Empty | SlotOutcome::Acknowledged => {}
                SlotOutcome::Failed => note_failed(&mut report, slot.as_str()),
                SlotOutcome::Unacknowledged => note_unacknowledged(&mut report, slot.as_str()),
            }
            for registered in owners.iter().filter(|owner| owner.slot == slot) {
                let owner = registered.owner.as_ref();
                let answer = owner.release();
                // Past the cutoff, or after the shared main queue stalled, the
                // closure stays queued but no further hop is spent.
                if !(late || main_stalled && owner.on_main()) {
                    let _within_bound = wait_for_finish(wait.as_ref(), &|| answer.is_answered());
                }
                // The answer state, not the wait result, decides the name: an
                // owner with nothing to release has already answered, and an
                // answer arriving just after the bound still counts.
                match answer.state() {
                    OwnerState::Succeeded => {}
                    OwnerState::Failed => note_failed(&mut report, owner.name()),
                    OwnerState::Pending => {
                        note_unacknowledged(&mut report, owner.name());
                        if owner.on_main() {
                            main_stalled = true;
                        }
                    }
                }
            }
        }

        self.finish(report, self.clock.now());
    }

    /// The stage cutoff, always recomputed from the live deadline.
    ///
    /// A later request shortens the deadline, so a single stale cutoff would
    /// let a stage start after the reservation already began.
    fn cutoff(&self) -> Instant {
        self.deadline().map_or_else(
            || self.clock.now(),
            |deadline| deadline.checked_sub(TEARDOWN_RECORD_BUDGET).unwrap_or(deadline),
        )
    }

    /// Stores the one outcome, records it, then runs the terminal action.
    #[allow(clippy::significant_drop_tightening)] // The guard builds the stored outcome.
    fn finish(&self, report: StageReport, now: Instant) {
        let result = if !report.failed.is_empty() {
            TeardownResult::Partial
        } else if !report.unacknowledged.is_empty() {
            TeardownResult::Uncertain
        } else {
            TeardownResult::Clean
        };
        let (outcome, action, deadline, first) = {
            let mut state = self.lock();
            let first = state.first_request.unwrap_or(now);
            let deadline = state.deadline.unwrap_or(now);
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
                failed: report.failed,
                unacknowledged: report.unacknowledged,
            };
            let action = if state.reply_pending {
                TerminalAction::Reply
            } else if state.cause == TeardownCause::Restart && result == TeardownResult::Clean {
                TerminalAction::Restart
            } else {
                TerminalAction::Exit(recorded_code)
            };
            state.outcome = Some(outcome.clone());
            state.terminal = Some(action);
            state.phase = Phase::Finished;
            (outcome, action, deadline, first)
        };
        self.inner.changed.notify_all();

        // The record is attempted after the slot is authoritative and before
        // the terminal action. Its bound comes out of the remaining budget; a
        // blocked or closed sink never delays termination.
        let budget = TEARDOWN_RECORD_BUDGET.min(deadline.saturating_duration_since(now));
        let delivery = self.records.record(&outcome.to_string(), budget);
        self.lock().delivery = delivery;

        if let Some(stages) = self.stages.get() {
            stages.publish_terminal();
        }
        let Some(terminal) = self.terminal.get().map(Arc::clone) else {
            self.mark_dispatched();
            return;
        };
        // Stage 5 tail: close the signal bridge so a repeat signal can no
        // longer escalate after the outcome.
        terminal.close_signals();
        if action == TerminalAction::Restart {
            self.instance.release();
        }
        // Stage 6: the exit gate opens only after the outcome exists.
        terminal.open_gate(outcome.recorded_code);
        let progress = terminal.dispatch(action);
        self.mark_dispatched();
        self.await_terminal(action, &outcome, &progress, deadline, first);
    }

    fn mark_dispatched(&self) {
        self.lock().dispatched = true;
        self.inner.changed.notify_all();
    }

    /// Waits for the terminal action, then forces this process to exit when the
    /// action was not delivered.
    fn await_terminal(
        &self,
        action: TerminalAction,
        outcome: &TeardownOutcome,
        progress: &TerminalProgress,
        deadline: Instant,
        first: Instant,
    ) {
        // Wait for delivery until the live deadline, polling a quantum at a time
        // through the same wakeable wait the stages use.
        while !progress.delivered() {
            let now = self.clock.now();
            if now >= deadline {
                break;
            }
            self.pause(CANCELLATION_QUANTUM.min(deadline.saturating_duration_since(now)));
        }
        let now = self.clock.now();
        if progress.delivered() {
            if action.is_reply() {
                self.store_terminal_evidence(outcome, false, false, false, deadline);
            }
            return;
        }

        // The first claimant at the deadline wins the forced exit. A closure
        // that already claimed owns the action, but acceptance alone does not
        // suppress a forced exit for a runtime action.
        let forced_claim = progress.claim(now);
        if !action.is_reply() {
            // An unclaimed or claimed-but-incomplete runtime action exits at
            // the deadline. There is no reply exception on this path.
            self.store_terminal_evidence(outcome, true, false, false, deadline);
            self.force_exit(outcome.recorded_code);
            return;
        }
        if forced_claim {
            // The closure never started: there is no claimed reply to extend.
            self.store_terminal_evidence(outcome, true, false, false, deadline);
            self.force_exit(outcome.recorded_code);
            return;
        }

        // The `AppKit` closure claimed before the deadline but delivery has not
        // completed. It gets exactly one owner hop from the deadline. The hard
        // ceiling keeps a shortened deadline from extending the total bound.
        let ceiling = first
            .checked_add(TEARDOWN_TOTAL)
            .and_then(|request| request.checked_add(OWNER_WAIT_BUDGET))
            .unwrap_or(deadline);
        let hop_end = deadline.checked_add(OWNER_WAIT_BUDGET).unwrap_or(deadline).min(ceiling);
        while !progress.delivered() {
            let now = self.clock.now();
            if now >= hop_end {
                break;
            }
            self.pause(CANCELLATION_QUANTUM.min(hop_end.saturating_duration_since(now)));
        }
        if progress.delivered() {
            self.store_terminal_evidence(outcome, false, true, true, hop_end);
            return;
        }
        self.store_terminal_evidence(outcome, true, true, true, hop_end);
        self.force_exit(outcome.recorded_code);
    }

    fn store_terminal_evidence(
        &self,
        outcome: &TeardownOutcome,
        forced_exit: bool,
        reply_late: bool,
        bound_extended: bool,
        applicable_deadline: Instant,
    ) -> RecordDelivery {
        let line = format!(
            "teardown: terminal=true forced_exit={forced_exit} reply_late={reply_late} \
             bound_extended={bound_extended} outcome={} cause={} elapsed_ms={} recorded_code={}",
            outcome.result.as_str(),
            outcome.cause.as_str(),
            outcome.elapsed.as_millis(),
            outcome.recorded_code,
        );
        {
            let mut state = self.lock();
            state.terminal_evidence = Some(line.clone());
        }
        self.inner.changed.notify_all();
        let now = self.clock.now();
        let budget = TEARDOWN_RECORD_BUDGET.min(applicable_deadline.saturating_duration_since(now));
        let delivery = if budget.is_zero() {
            // Still hand the line to the channel, but never count a zero-budget
            // attempt as delivered.
            let _ = self.records.record(&line, Duration::ZERO);
            RecordDelivery::Unavailable
        } else {
            self.records.record(&line, budget)
        };
        self.lock().terminal_delivery = delivery;
        delivery
    }

    fn force_exit(&self, code: i32) {
        if let Some(exit) = self.exit.get() {
            exit.exit(code);
        }
    }

    /// Sleeps at most `timeout` on the coordinator's clock and wake source.
    #[allow(clippy::significant_drop_tightening)] // The guard is the wait condition.
    fn pause(&self, timeout: Duration) {
        if timeout.is_zero() {
            return;
        }
        let pause = self.clock.pause(timeout);
        let state = self.lock();
        drop(
            self.inner
                .changed
                .wait_timeout(state, pause)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

/// Name reported when the runner recorded a setup or teardown failure.
const STAGES: &str = "stages";
/// Name reported when stage 2 was never reached before the deadline.
const MONITORS_STAGE: &str = "monitors";
/// Name reported when stage 3 was never reached before the deadline.
const REGISTRY_STAGE: &str = "registry";

fn merge_stage(report: &mut StageReport, stage: StageReport) {
    for owner in stage.failed {
        note_failed(report, &owner);
    }
    for owner in stage.unacknowledged {
        note_unacknowledged(report, &owner);
    }
}

fn note_failed(report: &mut StageReport, owner: &str) {
    if !report.failed.iter().any(|known| known == owner) {
        report.failed.push(owner.to_owned());
    }
}

fn note_unacknowledged(report: &mut StageReport, owner: &str) {
    if !report.unacknowledged.iter().any(|known| known == owner) {
        report.unacknowledged.push(owner.to_owned());
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
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    use stache_sdk::{DiscardLogger, MainThreadExecutor, ThreadError};

    use super::*;

    const LIMIT: Duration = Duration::from_secs(10);

    type BoolProbe = Box<dyn Fn() -> bool + Send + Sync>;
    type Hook = Box<dyn Fn() + Send + Sync>;

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[derive(Default)]
    struct RecordingRecords {
        lines: Mutex<Vec<String>>,
        budgets: Mutex<Vec<Duration>>,
    }

    impl RecordingRecords {
        fn lines(&self) -> Vec<String> { lock(&self.lines).clone() }

        fn budgets(&self) -> Vec<Duration> { lock(&self.budgets).clone() }
    }

    impl TeardownRecordChannel for RecordingRecords {
        fn record(&self, line: &str, budget: Duration) -> RecordDelivery {
            lock(&self.lines).push(line.to_owned());
            lock(&self.budgets).push(budget);
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

    /// A gate a test opens to keep the coordinator inside a stage.
    #[derive(Default)]
    struct Hold {
        released: Mutex<bool>,
        signal: Condvar,
    }

    impl Hold {
        #[allow(clippy::significant_drop_tightening)] // The guard is the loop condition.
        fn wait(&self) {
            let mut released = lock(&self.released);
            while !*released {
                released = self.signal.wait(released).unwrap_or_else(PoisonError::into_inner);
            }
        }

        fn release(&self) {
            *lock(&self.released) = true;
            self.signal.notify_all();
        }
    }

    /// A recording fake for every stage.
    #[derive(Default)]
    struct RecordingStages {
        calls: Mutex<Arc<Mutex<Vec<String>>>>,
        reap: Mutex<StageReport>,
        registry: Mutex<StageReport>,
        outcomes: Mutex<Vec<(NativeSlot, SlotOutcome)>>,
        hold: Mutex<Option<Arc<Hold>>>,
        entered: Mutex<Option<mpsc::Sender<()>>>,
        before: Mutex<Option<Hook>>,
    }

    impl RecordingStages {
        fn with_reports(reap: StageReport, registry: StageReport) -> Self {
            Self {
                reap: Mutex::new(reap),
                registry: Mutex::new(registry),
                ..Self::default()
            }
        }

        fn calls(&self) -> Vec<String> {
            let calls = Arc::clone(&lock(&self.calls));
            lock(&calls).clone()
        }

        fn record(&self, call: &str) {
            let calls = Arc::clone(&lock(&self.calls));
            lock(&calls).push(call.to_owned());
        }

        fn use_calls(&self, calls: Arc<Mutex<Vec<String>>>) { *lock(&self.calls) = calls; }

        fn hold(&self) -> Arc<Hold> {
            let hold = Arc::new(Hold::default());
            *lock(&self.hold) = Some(Arc::clone(&hold));
            hold
        }

        fn signal_entered(&self) -> mpsc::Receiver<()> {
            let (sender, receiver) = mpsc::channel();
            *lock(&self.entered) = Some(sender);
            receiver
        }

        fn set_before(&self, hook: Hook) { *lock(&self.before) = Some(hook); }

        fn set_outcome(&self, slot: NativeSlot, outcome: SlotOutcome) {
            lock(&self.outcomes).push((slot, outcome));
        }

        fn outcome_for(&self, slot: NativeSlot) -> SlotOutcome {
            lock(&self.outcomes)
                .iter()
                .rev()
                .find(|(candidate, _)| *candidate == slot)
                .map_or(SlotOutcome::Acknowledged, |(_, outcome)| *outcome)
        }
    }

    impl TeardownStages for RecordingStages {
        fn invalidate(&self) {
            self.record("invalidate");
            if let Some(before) = lock(&self.before).as_ref() {
                before();
            }
        }

        fn reap_monitors(&self, _wait: Arc<dyn ShutdownWait>) -> StageReport {
            self.record("reap_monitors");
            let entered = lock(&self.entered).take();
            if let Some(entered) = entered {
                let _ = entered.send(());
            }
            let hold = lock(&self.hold).clone();
            if let Some(hold) = hold {
                hold.wait();
            }
            lock(&self.reap).clone()
        }

        fn stop_registry(&self, _wait: Arc<dyn ShutdownWait>) -> StageReport {
            self.record("stop_registry");
            lock(&self.registry).clone()
        }

        fn cleanup(&self, slot: NativeSlot, _wait: Arc<dyn ShutdownWait>) -> SlotOutcome {
            self.record(&format!("cleanup:{}", slot.as_str()));
            self.outcome_for(slot)
        }

        fn publish_terminal(&self) { self.record("publish_terminal"); }
    }

    /// Records main-thread dispatches instead of running them.
    #[derive(Default)]
    struct RecordingExecutor {
        jobs: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
        fail: AtomicBool,
    }

    impl RecordingExecutor {
        fn run(&self) {
            let jobs = std::mem::take(&mut *lock(&self.jobs));
            for job in jobs {
                job();
            }
        }

        fn queued(&self) -> usize { lock(&self.jobs).len() }
    }

    impl MainThreadExecutor for RecordingExecutor {
        fn is_main_thread(&self) -> bool { false }

        fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(ThreadError::GroupStopped);
            }
            lock(&self.jobs).push(job);
            Ok(())
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum OwnerBehaviour {
        Acknowledge,
        Fail,
        Hang,
    }

    /// A native owner fake with the three completion behaviours.
    struct FakeOwner {
        name: &'static str,
        on_main: bool,
        behaviour: OwnerBehaviour,
        releases: AtomicUsize,
        pending: Mutex<Option<OwnerAnswer>>,
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    impl FakeOwner {
        fn new(
            name: &'static str,
            on_main: bool,
            behaviour: OwnerBehaviour,
            order: &Arc<Mutex<Vec<&'static str>>>,
        ) -> Arc<Self> {
            Arc::new(Self {
                name,
                on_main,
                behaviour,
                releases: AtomicUsize::new(0),
                pending: Mutex::new(None),
                order: Arc::clone(order),
            })
        }

        fn releases(&self) -> usize { self.releases.load(Ordering::SeqCst) }

        /// Answers a hung owner after the fact.
        fn answer_late(&self) {
            if let Some(answer) = lock(&self.pending).as_ref() {
                answer.succeed();
            }
        }
    }

    impl NativeOwner for FakeOwner {
        fn name(&self) -> &'static str { self.name }

        fn on_main(&self) -> bool { self.on_main }

        fn release(&self) -> OwnerAnswer {
            self.releases.fetch_add(1, Ordering::SeqCst);
            lock(&self.order).push(self.name);
            let answer = OwnerAnswer::new();
            match self.behaviour {
                OwnerBehaviour::Acknowledge => answer.succeed(),
                OwnerBehaviour::Fail => answer.fail(),
                OwnerBehaviour::Hang => *lock(&self.pending) = Some(answer.clone()),
            }
            answer
        }
    }

    /// Advances the frozen coordinator clock until an outcome exists.
    fn advance_until_outcome(fixture: &Fixture) {
        let deadline = Instant::now() + LIMIT;
        while fixture.teardown.outcome().is_none() {
            assert!(Instant::now() < deadline, "teardown did not reach an outcome");
            fixture.clock.advance(Duration::from_millis(250));
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[derive(Default)]
    struct FakeExit {
        clock: Option<Arc<ManualClock>>,
        codes: Mutex<Vec<i32>>,
        at: Mutex<Vec<Instant>>,
        probe: Mutex<Option<BoolProbe>>,
        probe_result: Mutex<Option<bool>>,
        signal: Condvar,
    }

    impl FakeExit {
        fn new(clock: Arc<ManualClock>) -> Arc<Self> {
            Arc::new(Self {
                clock: Some(clock),
                ..Self::default()
            })
        }

        fn codes(&self) -> Vec<i32> { lock(&self.codes).clone() }

        fn at(&self) -> Vec<Instant> { lock(&self.at).clone() }

        fn set_probe(&self, probe: BoolProbe) { *lock(&self.probe) = Some(probe); }

        fn probe_result(&self) -> Option<bool> { *lock(&self.probe_result) }

        #[allow(clippy::significant_drop_tightening)] // The guard is the wait condition.
        fn wait(&self, timeout: Duration) -> Vec<i32> {
            let codes = lock(&self.codes);
            let (codes, _) = self
                .signal
                .wait_timeout_while(codes, timeout, |codes| codes.is_empty())
                .unwrap_or_else(PoisonError::into_inner);
            codes.clone()
        }
    }

    impl ExitProcess for FakeExit {
        fn exit(&self, code: i32) {
            let result = lock(&self.probe).as_ref().map(|probe| probe());
            *lock(&self.probe_result) = result;
            lock(&self.codes).push(code);
            let at = self.clock.as_ref().map_or_else(Instant::now, |clock| clock.now());
            lock(&self.at).push(at);
            self.signal.notify_all();
        }
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    enum DeliveryMode {
        /// The closure claimed and completed before the coordinator looked.
        #[default]
        DeliverNow,
        /// The closure claimed but delivery has not completed.
        ClaimOnly,
        /// The closure never started.
        NeverClaim,
    }

    struct RecordingTerminal {
        gate: ExitGate,
        calls: Arc<Mutex<Vec<String>>>,
        actions: Mutex<Vec<TerminalAction>>,
        mode: Mutex<DeliveryMode>,
        progress: Mutex<Option<TerminalProgress>>,
        probe: Mutex<Option<BoolProbe>>,
        probe_result: Mutex<Option<bool>>,
        relaunches: std::sync::atomic::AtomicUsize,
    }

    impl RecordingTerminal {
        fn new(gate: ExitGate, calls: Arc<Mutex<Vec<String>>>) -> Arc<Self> {
            Arc::new(Self {
                gate,
                calls,
                actions: Mutex::new(Vec::new()),
                mode: Mutex::new(DeliveryMode::DeliverNow),
                progress: Mutex::new(None),
                probe: Mutex::new(None),
                probe_result: Mutex::new(None),
                relaunches: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn actions(&self) -> Vec<TerminalAction> { lock(&self.actions).clone() }

        fn set_mode(&self, mode: DeliveryMode) { *lock(&self.mode) = mode; }

        fn set_probe(&self, probe: BoolProbe) { *lock(&self.probe) = Some(probe); }

        fn probe_result(&self) -> Option<bool> { *lock(&self.probe_result) }

        fn progress(&self) -> TerminalProgress {
            lock(&self.progress).clone().expect("the terminal was dispatched")
        }

        fn relaunches(&self) -> usize { self.relaunches.load(Ordering::SeqCst) }
    }

    impl TerminalDriver for RecordingTerminal {
        fn dispatch(&self, action: TerminalAction) -> TerminalProgress {
            let result = lock(&self.probe).as_ref().map(|probe| probe());
            *lock(&self.probe_result) = result;
            lock(&self.calls).push("dispatch".to_owned());
            lock(&self.actions).push(action);
            let progress = TerminalProgress::new();
            let mode = *lock(&self.mode);
            match mode {
                DeliveryMode::DeliverNow => {
                    let _ = progress.claim(Instant::now());
                    if action == TerminalAction::Restart {
                        self.relaunches.fetch_add(1, Ordering::SeqCst);
                    }
                    progress.mark_delivered();
                }
                DeliveryMode::ClaimOnly => {
                    let _ = progress.claim(Instant::now());
                }
                DeliveryMode::NeverClaim => {}
            }
            *lock(&self.progress) = Some(progress.clone());
            progress
        }

        fn open_gate(&self, code: i32) {
            lock(&self.calls).push("open_gate".to_owned());
            self.gate.record_exit(code);
        }

        fn close_signals(&self) { lock(&self.calls).push("close_signals".to_owned()); }
    }

    struct Fixture {
        teardown: Arc<Teardown>,
        terminal: Arc<RecordingTerminal>,
        stages: Arc<RecordingStages>,
        records: Arc<RecordingRecords>,
        clock: Arc<ManualClock>,
        instance: Arc<RecordingInstance>,
        exit: Arc<FakeExit>,
        gate: ExitGate,
        calls: Arc<Mutex<Vec<String>>>,
    }

    fn fixture() -> Fixture {
        fixture_with(Arc::new(RecordingStages::default()), DeliveryMode::DeliverNow)
    }

    fn fixture_with(stages: Arc<RecordingStages>, mode: DeliveryMode) -> Fixture {
        let clock = Arc::new(ManualClock::new());
        let records = Arc::new(RecordingRecords::default());
        let instance = Arc::new(RecordingInstance::default());
        let gate = ExitGate::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        stages.use_calls(Arc::clone(&calls));
        let terminal = RecordingTerminal::new(gate.clone(), Arc::clone(&calls));
        terminal.set_mode(mode);
        let exit = FakeExit::new(Arc::clone(&clock));
        let teardown = Teardown::new(
            Arc::clone(&clock) as Arc<dyn TeardownClock>,
            Arc::clone(&records) as Arc<dyn TeardownRecordChannel>,
            Arc::clone(&instance) as Arc<dyn InstanceOwner>,
            Arc::new(DiscardLogger),
        );
        teardown.install_stages(Arc::clone(&stages) as Arc<dyn TeardownStages>);
        teardown.install_terminal(Arc::clone(&terminal) as Arc<dyn TerminalDriver>);
        teardown.install_exit(Arc::clone(&exit) as Arc<dyn ExitProcess>);
        teardown.start().expect("start the coordinator");
        Fixture {
            teardown,
            terminal,
            stages,
            records,
            clock,
            instance,
            exit,
            gate,
            calls,
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

    fn calls(fixture: &Fixture) -> Vec<String> { lock(&fixture.calls).clone() }

    const SLOT_CALLS: [&str; 8] = [
        "cleanup:power-assertion",
        "cleanup:input-tap",
        "cleanup:application-observers",
        "cleanup:hud",
        "cleanup:windows",
        "cleanup:socket",
        "cleanup:tray",
        "cleanup:webview",
    ];

    fn expected_order() -> Vec<String> {
        let mut order = vec!["invalidate".to_owned(), "reap_monitors".to_owned()];
        order.push("stop_registry".to_owned());
        order.extend(SLOT_CALLS.iter().map(|call| (*call).to_owned()));
        order.extend([
            "publish_terminal".to_owned(),
            "close_signals".to_owned(),
            "open_gate".to_owned(),
            "dispatch".to_owned(),
        ]);
        order
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
    fn the_terminal_progress_can_be_claimed_once_and_tracks_delivery() {
        let progress = TerminalProgress::new();
        let now = Instant::now();
        assert!(!progress.claimed());
        assert_eq!(progress.claimed_at(), None);
        assert!(progress.claim(now));
        assert_eq!(progress.claimed_at(), Some(now));
        assert!(!progress.claim(now + Duration::from_secs(1)));
        assert!(!progress.delivered());
        progress.mark_delivered();
        assert!(progress.delivered());
        assert!(
            TerminalProgress::new().claim(now),
            "each process gets its own claim"
        );
    }

    #[test]
    fn stage_order_is_exact_and_invalidation_precedes_every_stop() {
        let stages = Arc::new(RecordingStages::default());
        let seen_after_invalidate = Arc::new(Mutex::new(Vec::new()));
        let stages_for_hook = Arc::clone(&stages);
        let snapshot = Arc::clone(&seen_after_invalidate);
        stages.set_before(Box::new(move || {
            *lock(&snapshot) = stages_for_hook.calls();
        }));
        let fixture = fixture_with(stages, DeliveryMode::DeliverNow);

        fixture.teardown.request(TeardownCause::Request, None);
        let outcome = outcome(&fixture);
        terminal(&fixture);

        assert_eq!(outcome.result, TeardownResult::Clean);
        assert_eq!(calls(&fixture), expected_order());
        assert_eq!(
            lock(&seen_after_invalidate).clone(),
            vec!["invalidate".to_owned()],
            "no stop may run before invalidation"
        );
    }

    #[test]
    fn a_clean_teardown_stores_one_outcome_before_dispatch() {
        let fixture = fixture();
        let teardown = Arc::clone(&fixture.teardown);
        let gate = fixture.gate.clone();
        fixture.terminal.set_probe(Box::new(move || {
            teardown.outcome().is_some() && gate.is_allowed()
        }));

        fixture.teardown.request(TeardownCause::Request, None);
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
            fixture.terminal.probe_result(),
            Some(true),
            "the outcome and exit gate exist before the terminal action"
        );
        assert_eq!(fixture.teardown.delivery(), RecordDelivery::Delivered);
        assert_eq!(fixture.records.lines(), vec![
            "teardown: outcome=clean cause=request elapsed_ms=0 escalated=false \
                 recorded_code=0 failed=[] unacknowledged=[]"
                .to_owned()
        ]);
    }

    #[test]
    fn a_failed_stage_reports_partial_and_code_one() {
        let fixture = fixture_with(
            Arc::new(RecordingStages::with_reports(
                StageReport {
                    failed: vec!["config-watcher".to_owned()],
                    unacknowledged: Vec::new(),
                },
                StageReport {
                    failed: vec!["module-a".to_owned()],
                    unacknowledged: Vec::new(),
                },
            )),
            DeliveryMode::DeliverNow,
        );
        fixture.teardown.request(TeardownCause::Request, Some(7));

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Partial);
        assert_eq!(outcome.recorded_code, 1);
        assert_eq!(outcome.failed, vec![
            "config-watcher".to_owned(),
            "module-a".to_owned()
        ]);
        assert!(outcome.unacknowledged.is_empty());
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(1)]);
        assert_eq!(fixture.records.lines().len(), 1);
    }

    #[test]
    fn an_unacknowledged_stage_is_uncertain() {
        let fixture = fixture_with(
            Arc::new(RecordingStages::with_reports(
                StageReport::default(),
                StageReport {
                    failed: Vec::new(),
                    unacknowledged: vec!["module-a".to_owned()],
                },
            )),
            DeliveryMode::DeliverNow,
        );
        fixture.teardown.request(TeardownCause::Request, None);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
        assert_eq!(outcome.recorded_code, 1);
        assert_eq!(outcome.unacknowledged, vec!["module-a".to_owned()]);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(1)]);
    }

    #[test]
    fn an_expiring_teardown_keeps_its_reserved_record_budget() {
        let fixture = fixture();
        let hold = fixture.stages.hold();
        let entered = fixture.stages.signal_entered();
        fixture.teardown.request(TeardownCause::Request, None);
        entered.recv_timeout(LIMIT).expect("the first stage started");
        let owner_wait = TEARDOWN_TOTAL.saturating_sub(TEARDOWN_RECORD_BUDGET);
        fixture.clock.advance(owner_wait.saturating_sub(Duration::from_millis(1)));
        assert!(
            fixture.teardown.outcome().is_none(),
            "the owner wait lasts until the reservation starts"
        );
        fixture.clock.advance(Duration::from_millis(1));
        hold.release();

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
        assert_eq!(outcome.elapsed, owner_wait);
        terminal(&fixture);
        assert_eq!(fixture.records.budgets(), vec![TEARDOWN_RECORD_BUDGET]);
        assert_eq!(fixture.teardown.delivery(), RecordDelivery::Delivered);
    }

    #[test]
    fn a_later_request_shortens_the_deadline_and_a_third_changes_nothing() {
        let fixture = fixture();
        let hold = fixture.stages.hold();
        let entered = fixture.stages.signal_entered();
        fixture.teardown.request(TeardownCause::Request, None);
        entered.recv_timeout(LIMIT).expect("the first stage started");
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
        hold.release();
        let outcome = outcome(&fixture);
        assert!(outcome.escalated);
        assert_eq!(outcome.recorded_code, 7, "the first non-zero code wins");
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(7)]);
    }

    #[test]
    fn a_request_at_4900_ms_cannot_push_the_total_past_five_seconds() {
        let fixture = fixture();
        let hold = fixture.stages.hold();
        let entered = fixture.stages.signal_entered();
        fixture.teardown.request(TeardownCause::Request, None);
        entered.recv_timeout(LIMIT).expect("the first stage started");
        let first = fixture.teardown.deadline().expect("first deadline");
        fixture.clock.advance(Duration::from_millis(4900));
        fixture.teardown.request(TeardownCause::Request, None);
        assert_eq!(
            fixture.teardown.deadline(),
            Some(first),
            "a request at 4.9 s must stay capped by the total"
        );
        fixture.clock.advance(Duration::from_millis(200));
        hold.release();
        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
    }

    #[test]
    fn a_terminate_request_keeps_the_reply_pending_and_answers_later() {
        let fixture = fixture();
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Clean);
        assert_eq!(outcome.cause, TeardownCause::Terminate);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Reply]);
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Now);
        let evidence = fixture
            .teardown
            .wait_for_terminal_evidence(LIMIT)
            .expect("a delivered reply records evidence");
        assert!(evidence.contains("forced_exit=false reply_late=false bound_extended=false"));
        assert_eq!(fixture.records.lines().len(), 2, "outcome and reply evidence");
    }

    #[test]
    fn a_repeat_terminate_escalates_without_a_second_reply() {
        let fixture = fixture();
        let hold = fixture.stages.hold();
        let entered = fixture.stages.signal_entered();
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        entered.recv_timeout(LIMIT).expect("the first stage started");
        let first = fixture.teardown.deadline().expect("first deadline");
        fixture.clock.advance(Duration::from_secs(1));
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        assert!(fixture.teardown.deadline().expect("shortened") < first);
        hold.release();

        let outcome = outcome(&fixture);
        assert!(outcome.escalated);
        terminal(&fixture);
        assert_eq!(
            fixture.terminal.actions(),
            vec![TerminalAction::Reply],
            "a repeat terminate must not add a second reply"
        );
        assert!(fixture.teardown.wait_for_terminal_evidence(LIMIT).is_some());
        assert_eq!(fixture.records.lines().len(), 2, "outcome and reply evidence");
    }

    #[test]
    fn a_deadline_expiry_with_a_pending_reply_still_replies_once() {
        let fixture = fixture();
        let hold = fixture.stages.hold();
        let entered = fixture.stages.signal_entered();
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        entered.recv_timeout(LIMIT).expect("the first stage started");
        fixture.clock.advance(TEARDOWN_TOTAL);
        hold.release();

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Reply]);
    }

    #[test]
    fn a_clean_restart_releases_the_lock_before_the_fake_relaunch() {
        let fixture = fixture();
        let instance = Arc::clone(&fixture.instance);
        fixture
            .terminal
            .set_probe(Box::new(move || instance.released.load(Ordering::SeqCst)));

        fixture.teardown.request(TeardownCause::Restart, None);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Clean);
        assert_eq!(outcome.cause, TeardownCause::Restart);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Restart]);
        assert!(fixture.instance.released.load(Ordering::SeqCst));
        assert_eq!(
            fixture.terminal.probe_result(),
            Some(true),
            "the instance claim must be released before the relaunch"
        );
        assert!(fixture.records.lines()[0].contains("cause=restart"));
    }

    #[test]
    fn a_partial_restart_finishes_with_status_one_and_never_relaunches() {
        let fixture = fixture_with(
            Arc::new(RecordingStages::with_reports(
                StageReport {
                    failed: vec!["module-a".to_owned()],
                    unacknowledged: Vec::new(),
                },
                StageReport::default(),
            )),
            DeliveryMode::DeliverNow,
        );
        fixture.teardown.request(TeardownCause::Restart, None);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Partial);
        assert_eq!(outcome.recorded_code, 1);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(1)]);
        assert!(!fixture.instance.released.load(Ordering::SeqCst));
        assert_eq!(fixture.terminal.relaunches(), 0);
    }

    #[test]
    fn an_uncertain_restart_finishes_with_status_one_and_never_relaunches() {
        let fixture = fixture_with(
            Arc::new(RecordingStages::with_reports(
                StageReport::default(),
                StageReport {
                    failed: Vec::new(),
                    unacknowledged: vec!["module-a".to_owned()],
                },
            )),
            DeliveryMode::DeliverNow,
        );
        fixture.teardown.request(TeardownCause::Restart, None);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(1)]);
        assert!(!fixture.instance.released.load(Ordering::SeqCst));
        assert_eq!(fixture.terminal.relaunches(), 0);
    }

    #[test]
    fn a_restart_with_a_pending_reply_never_relaunches() {
        let fixture = fixture();
        let hold = fixture.stages.hold();
        let entered = fixture.stages.signal_entered();
        fixture.teardown.request(TeardownCause::Restart, None);
        entered.recv_timeout(LIMIT).expect("the first stage started");
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        hold.release();

        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Reply]);
        assert!(!fixture.instance.released.load(Ordering::SeqCst));
        assert_eq!(fixture.terminal.relaunches(), 0);
    }

    #[test]
    fn concurrent_requests_from_real_threads_produce_one_run_and_one_outcome() {
        let fixture = fixture();
        let hold = fixture.stages.hold();
        let entered = fixture.stages.signal_entered();
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
        entered.recv_timeout(LIMIT).expect("the first stage started");
        hold.release();

        let first = outcome(&fixture);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions().len(), 1);
        assert!(fixture.teardown.wait_for_terminal_evidence(LIMIT).is_some());
        assert_eq!(fixture.records.lines().len(), 2, "outcome and reply evidence");
        fixture.teardown.request(TeardownCause::Restart, Some(9));
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Now);
        assert_eq!(fixture.terminal.actions().len(), 1);
        assert_eq!(fixture.records.lines().len(), 2);
        assert_eq!(fixture.teardown.outcome(), Some(first));
    }

    #[test]
    fn invalidation_never_waits_for_a_blocked_stage() {
        let fixture = fixture();
        let hold = fixture.stages.hold();
        let entered = fixture.stages.signal_entered();
        fixture.teardown.request(TeardownCause::Request, None);
        entered.recv_timeout(LIMIT).expect("the first stage started");

        assert_eq!(
            fixture.stages.calls(),
            vec!["invalidate".to_owned(), "reap_monitors".to_owned()],
            "invalidation returned before the blocked stage, so it took no join"
        );
        hold.release();
        outcome(&fixture);
    }

    #[test]
    fn escalation_wakes_a_stage_wait_within_one_quantum() {
        let fixture = fixture();
        let hold = fixture.stages.hold();
        let entered = fixture.stages.signal_entered();
        fixture.teardown.request(TeardownCause::Request, None);
        entered.recv_timeout(LIMIT).expect("the first stage started");

        let wait = TeardownWait {
            teardown: Arc::clone(&fixture.teardown),
        };
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            wait.wait(Duration::from_secs(30));
            let _ = sender.send(wait.now());
        });
        // Let the helper block on the coordinator condvar before waking it.
        thread::sleep(Duration::from_millis(25));
        fixture.teardown.request(TeardownCause::Request, None);
        let woke = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("a later request must wake every stage wait");
        handle.join().expect("wait thread");
        assert!(
            fixture.teardown.deadline().expect("deadline") <= woke + TEARDOWN_ESCALATION,
            "the shortened deadline is visible to the wait"
        );
        hold.release();
        outcome(&fixture);
    }

    #[test]
    fn queued_work_is_dropped_and_issued_work_is_not_retried() {
        let fixture = fixture_with(
            Arc::new(RecordingStages::with_reports(
                StageReport {
                    failed: vec!["module-a".to_owned()],
                    unacknowledged: Vec::new(),
                },
                StageReport::default(),
            )),
            DeliveryMode::DeliverNow,
        );
        fixture.teardown.request(TeardownCause::Request, None);
        let first = outcome(&fixture);
        terminal(&fixture);
        let calls_after = fixture.stages.calls().len();
        let records_after = fixture.records.lines().len();

        fixture.teardown.request(TeardownCause::Request, None);
        fixture.teardown.request(TeardownCause::Restart, None);
        assert_eq!(
            fixture.stages.calls().len(),
            calls_after,
            "no stage runs again after the outcome"
        );
        assert_eq!(fixture.records.lines().len(), records_after);
        assert_eq!(fixture.terminal.actions().len(), 1);
        assert_eq!(fixture.exit.codes(), Vec::<i32>::new());
        assert_eq!(fixture.teardown.outcome(), Some(first));
    }

    #[test]
    fn the_gate_opens_only_after_the_outcome_exists() {
        let fixture = fixture();
        let hold = fixture.stages.hold();
        let entered = fixture.stages.signal_entered();
        assert!(!fixture.gate.is_allowed());
        assert_eq!(
            exit_request_action(None, fixture.gate.is_allowed()),
            ExitAction::PreventOnly
        );
        fixture.teardown.request(TeardownCause::Request, None);
        entered.recv_timeout(LIMIT).expect("the first stage started");
        assert!(
            !fixture.gate.is_allowed(),
            "the gate stays closed while the outcome is missing"
        );
        hold.release();

        outcome(&fixture);
        terminal(&fixture);
        assert!(fixture.gate.is_allowed());
        assert_eq!(fixture.gate.exit_code(), Some(0));
        assert_eq!(
            exit_request_action(None, fixture.gate.is_allowed()),
            ExitAction::Allow
        );
    }

    #[test]
    fn a_poisoned_lock_and_failing_owners_still_produce_one_outcome() {
        let stages = Arc::new(RecordingStages::with_reports(
            StageReport {
                failed: vec!["config-watcher".to_owned()],
                unacknowledged: Vec::new(),
            },
            StageReport {
                failed: vec!["module-a".to_owned()],
                unacknowledged: Vec::new(),
            },
        ));
        stages.set_outcome(NativeSlot::Windows, SlotOutcome::Failed);
        stages.set_outcome(NativeSlot::Socket, SlotOutcome::Unacknowledged);
        let fixture = fixture_with(stages, DeliveryMode::DeliverNow);
        catch_unwind(AssertUnwindSafe(|| {
            let _guard = fixture.teardown.lock();
            panic!("poison the coordinator state lock");
        }))
        .expect_err("the injected panic must unwind");

        fixture.teardown.request(TeardownCause::Request, None);
        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Partial);
        assert_eq!(outcome.failed, vec![
            "config-watcher".to_owned(),
            "module-a".to_owned(),
            "windows".to_owned()
        ]);
        assert_eq!(outcome.unacknowledged, vec!["socket".to_owned()]);
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(1)]);
        assert_eq!(fixture.records.lines().len(), 1);
    }

    #[test]
    fn a_queue_only_main_exits_once_at_the_deadline_after_dispatch() {
        for terminate in [false, true] {
            let fixture =
                fixture_with(Arc::new(RecordingStages::default()), DeliveryMode::NeverClaim);
            let teardown = Arc::clone(&fixture.teardown);
            fixture.exit.set_probe(Box::new(move || {
                teardown.outcome().is_some() && teardown.terminal_evidence().is_some()
            }));
            if terminate {
                assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
            } else {
                fixture.teardown.request(TeardownCause::Request, None);
            }
            let deadline = fixture.teardown.deadline().expect("deadline");
            outcome(&fixture);
            terminal(&fixture);
            assert!(
                fixture.exit.codes().is_empty(),
                "an undrained main queue must not end the process before the deadline"
            );
            fixture.clock.advance(TEARDOWN_TOTAL);
            assert_eq!(fixture.exit.wait(LIMIT), vec![0]);
            assert_eq!(fixture.exit.at(), vec![deadline]);
            assert_eq!(
                fixture.exit.probe_result(),
                Some(true),
                "the outcome and evidence exist before the exit"
            );
            assert_eq!(fixture.terminal.actions().len(), 1);
            let evidence = fixture.teardown.terminal_evidence().expect("evidence");
            assert!(evidence.contains("forced_exit=true reply_late=false bound_extended=false"));
        }
    }

    #[test]
    fn a_shortened_deadline_still_exits_exactly_at_the_deadline() {
        let fixture = fixture_with(Arc::new(RecordingStages::default()), DeliveryMode::NeverClaim);
        let hold = fixture.stages.hold();
        let entered = fixture.stages.signal_entered();
        fixture.teardown.request(TeardownCause::Request, None);
        entered.recv_timeout(LIMIT).expect("the first stage started");
        fixture.clock.advance(Duration::from_millis(100));
        fixture.teardown.request(TeardownCause::Request, None);
        let shortened = fixture.teardown.deadline().expect("shortened deadline");
        assert_eq!(shortened, fixture.clock.now() + TEARDOWN_ESCALATION);
        hold.release();

        outcome(&fixture);
        terminal(&fixture);
        assert!(fixture.exit.codes().is_empty());
        fixture.clock.advance(TEARDOWN_ESCALATION);
        assert_eq!(fixture.exit.wait(LIMIT), vec![0]);
        assert_eq!(fixture.exit.at(), vec![shortened]);
    }

    #[test]
    fn a_late_closure_loses_the_claim_and_is_inert() {
        let fixture = fixture_with(Arc::new(RecordingStages::default()), DeliveryMode::NeverClaim);
        fixture.teardown.request(TeardownCause::Request, None);
        outcome(&fixture);
        terminal(&fixture);
        let progress = fixture.terminal.progress();
        fixture.clock.advance(TEARDOWN_TOTAL);
        assert_eq!(fixture.exit.wait(LIMIT), vec![0]);

        let deadline = fixture.exit.at()[0];
        assert!(
            !progress.claim(deadline),
            "a closure first started after the deadline must lose the claim"
        );
        assert_eq!(fixture.terminal.actions().len(), 1);
        assert_eq!(fixture.exit.codes(), vec![0]);
        assert_eq!(fixture.records.lines().len(), 2);
    }

    #[test]
    fn a_claimed_reply_resumed_after_the_deadline_delivers_late_without_exit() {
        let fixture = fixture_with(Arc::new(RecordingStages::default()), DeliveryMode::ClaimOnly);
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        outcome(&fixture);
        terminal(&fixture);
        assert!(fixture.exit.codes().is_empty());

        fixture.clock.advance(TEARDOWN_TOTAL);
        thread::sleep(Duration::from_millis(25));
        assert!(
            fixture.exit.codes().is_empty(),
            "a claimed reply must not force an exit at the deadline"
        );
        fixture
            .clock
            .advance(OWNER_WAIT_BUDGET.saturating_sub(Duration::from_millis(1)));
        fixture.terminal.progress().mark_delivered();
        fixture.clock.advance(Duration::from_millis(1));

        let evidence =
            fixture.teardown.wait_for_terminal_evidence(LIMIT).expect("late reply evidence");
        assert!(evidence.contains("forced_exit=false reply_late=true bound_extended=true"));
        assert!(fixture.exit.codes().is_empty());
        assert_eq!(fixture.exit.at(), Vec::<Instant>::new());
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Reply]);
    }

    #[test]
    fn delivery_never_completing_exits_at_deadline_plus_two_seconds() {
        let fixture = fixture_with(Arc::new(RecordingStages::default()), DeliveryMode::ClaimOnly);
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        let deadline = fixture.teardown.deadline().expect("deadline");
        outcome(&fixture);
        terminal(&fixture);
        fixture.clock.advance(TEARDOWN_TOTAL);
        thread::sleep(Duration::from_millis(25));
        assert!(fixture.exit.codes().is_empty());

        fixture.clock.advance(OWNER_WAIT_BUDGET);
        assert_eq!(fixture.exit.wait(LIMIT), vec![0]);
        assert_eq!(fixture.exit.at(), vec![deadline + OWNER_WAIT_BUDGET]);
        let evidence = fixture.teardown.terminal_evidence().expect("evidence");
        assert!(evidence.contains("forced_exit=true reply_late=true bound_extended=true"));
    }

    #[test]
    fn a_claimed_incomplete_runtime_action_exits_at_the_deadline() {
        for restart in [false, true] {
            let fixture =
                fixture_with(Arc::new(RecordingStages::default()), DeliveryMode::ClaimOnly);
            let cause = if restart {
                TeardownCause::Restart
            } else {
                TeardownCause::Request
            };
            fixture.teardown.request(cause, None);
            let deadline = fixture.teardown.deadline().expect("deadline");
            outcome(&fixture);
            terminal(&fixture);
            fixture.clock.advance(TEARDOWN_TOTAL);
            assert_eq!(fixture.exit.wait(LIMIT), vec![0]);
            assert_eq!(
                fixture.exit.at(),
                vec![deadline],
                "a runtime action gets no reply hop"
            );
            let evidence = fixture.teardown.terminal_evidence().expect("evidence");
            assert!(evidence.contains("forced_exit=true reply_late=false bound_extended=false"));
            assert_eq!(fixture.terminal.relaunches(), 0);
        }
    }

    #[test]
    fn a_forced_exit_records_one_outcome_then_evidence_then_exits() {
        let fixture = fixture_with(Arc::new(RecordingStages::default()), DeliveryMode::NeverClaim);
        fixture.teardown.request(TeardownCause::Restart, None);
        outcome(&fixture);
        terminal(&fixture);
        fixture.clock.advance(TEARDOWN_TOTAL);
        assert_eq!(fixture.exit.wait(LIMIT), vec![0]);

        let lines = fixture.records.lines();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("teardown: outcome=clean cause=restart"));
        assert!(lines[1].starts_with("teardown: terminal=true forced_exit=true"));
        assert!(lines[1].contains("outcome=clean cause=restart"));
        assert_eq!(
            fixture.teardown.outcome().expect("outcome").result,
            TeardownResult::Clean
        );
        assert_eq!(fixture.teardown.terminal_delivery(), RecordDelivery::Unavailable);
        assert_eq!(
            fixture.terminal.relaunches(),
            0,
            "a forced exit never relaunches"
        );
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Restart]);
    }

    #[test]
    fn a_timely_delivered_reply_records_no_forced_exit_or_extension() {
        let fixture = fixture();
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        outcome(&fixture);
        terminal(&fixture);
        let evidence = fixture.teardown.wait_for_terminal_evidence(LIMIT).expect("reply evidence");
        assert!(evidence.contains("forced_exit=false reply_late=false bound_extended=false"));
        assert!(fixture.exit.codes().is_empty());
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Reply]);
    }

    #[test]
    fn a_blocked_record_sink_cannot_extend_the_terminal_deadline() {
        struct HeldSink {
            started: Mutex<Option<mpsc::Sender<()>>>,
            release: Mutex<mpsc::Receiver<()>>,
        }

        impl TeardownRecordSink for HeldSink {
            fn write(&self, _line: &str) {
                let started = lock(&self.started).take();
                if let Some(started) = started {
                    let _ = started.send(());
                }
                let _ = lock(&self.release).recv();
            }
        }

        let (started, began) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let sink = Arc::new(HeldSink {
            started: Mutex::new(Some(started)),
            release: Mutex::new(released),
        });
        let clock = Arc::new(ManualClock::new());
        let gate = ExitGate::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let terminal = RecordingTerminal::new(gate, Arc::clone(&calls));
        terminal.set_mode(DeliveryMode::NeverClaim);
        let exit = FakeExit::new(Arc::clone(&clock));
        let stages = Arc::new(RecordingStages::default());
        let teardown = Teardown::new(
            Arc::clone(&clock) as Arc<dyn TeardownClock>,
            Arc::new(BoundedRecordChannel::new(sink)),
            Arc::new(RecordingInstance::default()),
            Arc::new(DiscardLogger),
        );
        teardown.install_stages(stages);
        teardown.install_terminal(Arc::clone(&terminal) as Arc<dyn TerminalDriver>);
        teardown.install_exit(Arc::clone(&exit) as Arc<dyn ExitProcess>);
        teardown.start().expect("start");

        let requested = Instant::now();
        teardown.request(TeardownCause::Request, None);
        let deadline = teardown.deadline().expect("deadline");
        assert!(teardown.wait_for_terminal(LIMIT));
        // The sink stays blocked until the end of the test, so any bound well
        // under `LIMIT` proves the point; 2 s leaves room for a loaded machine.
        assert!(
            requested.elapsed() < Duration::from_secs(2),
            "a blocked sink must not delay the terminal action"
        );
        assert_eq!(teardown.delivery(), RecordDelivery::Unavailable);
        began.recv_timeout(LIMIT).expect("the sink received the attempt");

        clock.advance(TEARDOWN_TOTAL);
        assert_eq!(exit.wait(LIMIT), vec![0]);
        assert_eq!(exit.at(), vec![deadline]);
        assert!(teardown.outcome().is_some());
        let evidence = teardown.terminal_evidence().expect("forced-exit evidence");
        assert!(evidence.contains("forced_exit=true"));
        assert_eq!(teardown.terminal_delivery(), RecordDelivery::Unavailable);
        release.send(()).expect("release the sink");
    }

    #[test]
    fn a_zero_budget_record_is_handed_off_without_being_awaited() {
        #[derive(Default)]
        struct CountingSink {
            lines: Mutex<Vec<String>>,
        }

        impl TeardownRecordSink for CountingSink {
            fn write(&self, line: &str) { lock(&self.lines).push(line.to_owned()); }
        }

        let sink = Arc::new(CountingSink::default());
        let channel = BoundedRecordChannel::new(sink.clone() as Arc<dyn TeardownRecordSink>);
        assert_eq!(
            channel.record("teardown: terminal=true forced_exit=true", Duration::ZERO),
            RecordDelivery::Unavailable,
            "a zero budget is never observed as delivered"
        );
        let deadline = Instant::now() + LIMIT;
        while lock(&sink.lines).is_empty() {
            assert!(
                Instant::now() < deadline,
                "the zero-budget record was dropped instead of handed off"
            );
            thread::sleep(Duration::from_millis(5));
        }
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
        let records = lock(&logger.records).clone();
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
        let stages = Arc::new(RecordingStages::default());
        let terminal = RecordingTerminal::new(ExitGate::new(), Arc::new(Mutex::new(Vec::new())));
        teardown.install_stages(stages);
        teardown.install_terminal(terminal);
        teardown.start().expect("start");
        teardown.request(TeardownCause::Request, None);
        assert!(teardown.wait_for_outcome(LIMIT).is_some());
        assert!(!teardown.note_unrecorded_exit());
        assert!(
            lock(&logger.records).is_empty(),
            "a recorded outcome needs no bypass record"
        );
    }

    #[derive(Default)]
    struct RecordingLogger {
        records: Mutex<Vec<LogRecord>>,
    }

    impl Logger for RecordingLogger {
        fn log(&self, record: LogRecord) { lock(&self.records).push(record); }
    }

    #[test]
    fn a_main_hop_owner_answers_only_when_its_closure_runs() {
        let executor = Arc::new(RecordingExecutor::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let hit = Arc::clone(&calls);
        let owner = MainHopOwner::new(
            "activation",
            Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
            move || {
                hit.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        let answer = owner.release();
        assert_eq!(executor.queued(), 1, "release only enqueues the hop");
        assert!(!answer.is_answered(), "a dispatch is never an acknowledgement");
        executor.run();
        assert_eq!(answer.state(), OwnerState::Succeeded);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_rejected_main_hop_and_a_failing_closure_are_failures() {
        let rejected = Arc::new(RecordingExecutor::default());
        rejected.fail.store(true, Ordering::SeqCst);
        let owner = MainHopOwner::new(
            "tray",
            Arc::clone(&rejected) as Arc<dyn MainThreadExecutor>,
            || Ok(()),
        );
        assert_eq!(owner.release().state(), OwnerState::Failed);

        let executor = Arc::new(RecordingExecutor::default());
        let owner = MainHopOwner::new(
            "tray",
            Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
            || Err("observer is gone".to_owned()),
        );
        let answer = owner.release();
        executor.run();
        assert_eq!(answer.state(), OwnerState::Failed);
    }

    #[test]
    fn a_never_run_tray_hop_is_uncertain_naming_tray_and_still_exits() {
        let executor = Arc::new(RecordingExecutor::default());
        let fixture = fixture();
        fixture.teardown.register_owner(
            NativeSlot::Tray,
            Arc::new(MainHopOwner::new(
                "tray",
                Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
                || Ok(()),
            )),
        );
        fixture.teardown.request(TeardownCause::Request, None);
        advance_until_outcome(&fixture);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
        assert_eq!(outcome.unacknowledged, vec!["tray".to_owned()]);
        assert_eq!(executor.queued(), 1, "the tray closure stays queued");
        terminal(&fixture);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(1)]);
    }

    #[test]
    fn a_stalled_main_queue_costs_one_hop_and_names_every_main_owner() {
        let fixture = fixture();
        let order = Arc::new(Mutex::new(Vec::new()));
        let power = FakeOwner::new("power", true, OwnerBehaviour::Hang, &order);
        let input = FakeOwner::new("input", true, OwnerBehaviour::Hang, &order);
        fixture.teardown.register_owner(NativeSlot::PowerAssertion, power.clone());
        fixture.teardown.register_owner(NativeSlot::InputTap, input.clone());
        fixture.teardown.request(TeardownCause::Request, None);
        advance_until_outcome(&fixture);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
        assert_eq!(outcome.unacknowledged, vec![
            "power".to_owned(),
            "input".to_owned()
        ]);
        assert_eq!(power.releases(), 1);
        assert_eq!(input.releases(), 1, "the second main owner is still released");
        assert!(
            outcome.elapsed >= OWNER_WAIT_BUDGET,
            "the first hop costs its budget: {:?}",
            outcome.elapsed
        );
        assert!(
            outcome.elapsed < OWNER_WAIT_BUDGET * 2,
            "the stalled queue must not charge a second hop: {:?}",
            outcome.elapsed
        );
    }

    #[test]
    fn a_late_owner_closure_changes_nothing_and_writes_no_second_record() {
        let executor = Arc::new(RecordingExecutor::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let hit = Arc::clone(&calls);
        let fixture = fixture();
        fixture.teardown.register_owner(
            NativeSlot::PowerAssertion,
            Arc::new(MainHopOwner::new(
                "late-power",
                Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
                move || {
                    hit.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )),
        );
        fixture.teardown.request(TeardownCause::Request, None);
        advance_until_outcome(&fixture);
        let outcome = outcome(&fixture);
        terminal(&fixture);
        let records = fixture.records.lines().len();
        let stage_calls = fixture.stages.calls().len();

        executor.run();

        assert_eq!(calls.load(Ordering::SeqCst), 1, "the closure still releases");
        assert_eq!(fixture.teardown.outcome(), Some(outcome));
        assert_eq!(fixture.records.lines().len(), records);
        assert_eq!(fixture.stages.calls().len(), stage_calls);
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Exit(1)]);
    }

    #[test]
    fn an_owner_without_an_answer_never_produces_clean() {
        let fixture = fixture();
        let order = Arc::new(Mutex::new(Vec::new()));
        let hung = FakeOwner::new("hud-owner", false, OwnerBehaviour::Hang, &order);
        fixture.teardown.register_owner(NativeSlot::Hud, hung.clone());
        fixture.teardown.request(TeardownCause::Request, None);
        advance_until_outcome(&fixture);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Uncertain);
        assert_eq!(outcome.unacknowledged, vec!["hud-owner".to_owned()]);
        hung.answer_late();
        assert_eq!(
            fixture.teardown.outcome(),
            Some(outcome),
            "a late answer cannot upgrade the stored outcome"
        );
    }

    /// An owner with nothing to release, which answers from `release` itself.
    struct AlreadyReleased;

    impl NativeOwner for AlreadyReleased {
        fn name(&self) -> &'static str { "nothing-open" }

        fn on_main(&self) -> bool { true }

        fn release(&self) -> OwnerAnswer {
            let answer = OwnerAnswer::new();
            answer.succeed();
            answer
        }
    }

    /// A worker-thread owner that never answers.
    struct NeverAnswers;

    impl NativeOwner for NeverAnswers {
        fn name(&self) -> &'static str { "never-answers" }

        fn on_main(&self) -> bool { false }

        fn release(&self) -> OwnerAnswer { OwnerAnswer::new() }
    }

    #[test]
    fn a_hung_owner_stops_at_the_cutoff_and_leaves_the_budget_for_the_reply() {
        let fixture = fixture();
        fixture.teardown.register_owner(NativeSlot::Hud, Arc::new(NeverAnswers));
        assert_eq!(fixture.teardown.begin_terminate(), TeardownAnswer::Later);
        let deadline = Instant::now() + LIMIT;
        while !calls(&fixture).contains(&"cleanup:hud".to_owned()) {
            assert!(Instant::now() < deadline, "the hung owner was not reached");
            thread::sleep(Duration::from_millis(1));
        }
        fixture.clock.advance(Duration::from_millis(100));
        fixture.teardown.request(TeardownCause::Request, None);
        advance_until_cutoff(&fixture);

        let outcome = outcome(&fixture);
        assert!(outcome.escalated);
        assert_eq!(outcome.unacknowledged, vec!["never-answers".to_owned()]);
        terminal(&fixture);
        assert_eq!(
            fixture.records.budgets()[0],
            TEARDOWN_RECORD_BUDGET,
            "the owner wait must not spend the reserved budget"
        );
        assert_eq!(fixture.terminal.actions(), vec![TerminalAction::Reply]);
        assert!(
            fixture.exit.codes().is_empty(),
            "a delivered reply is not forced"
        );
    }

    /// Steps exactly to the cutoff of the shortened deadline, never past it.
    fn advance_until_cutoff(fixture: &Fixture) {
        let deadline = fixture.teardown.deadline().expect("deadline");
        let cutoff = deadline.checked_sub(TEARDOWN_RECORD_BUDGET).expect("cutoff");
        let remaining = cutoff.saturating_duration_since(fixture.clock.now());
        fixture.clock.advance(remaining);
    }

    #[test]
    fn an_answered_owner_is_not_named_after_the_cutoff_and_an_empty_slot_never_is() {
        let fixture = fixture();
        fixture.teardown.register_owner(NativeSlot::Windows, Arc::new(AlreadyReleased));
        fixture.stages.set_outcome(NativeSlot::PowerAssertion, SlotOutcome::Empty);
        let hold = fixture.stages.hold();
        let entered = fixture.stages.signal_entered();
        fixture.teardown.request(TeardownCause::Request, None);
        entered.recv_timeout(LIMIT).expect("the first stage started");
        fixture.clock.advance(TEARDOWN_TOTAL.saturating_sub(TEARDOWN_RECORD_BUDGET));
        hold.release();

        let outcome = outcome(&fixture);
        assert!(!outcome.unacknowledged.iter().any(|owner| owner == "nothing-open"));
        assert!(!outcome.unacknowledged.iter().any(|owner| owner == "power-assertion"));
    }

    #[test]
    fn registered_owners_run_in_slot_order() {
        let fixture = fixture();
        let order = Arc::new(Mutex::new(Vec::new()));
        let hud = FakeOwner::new("hud", false, OwnerBehaviour::Acknowledge, &order);
        let webview = FakeOwner::new("webview", false, OwnerBehaviour::Acknowledge, &order);
        let power = FakeOwner::new("power", true, OwnerBehaviour::Acknowledge, &order);
        fixture.teardown.register_owner(NativeSlot::Hud, hud.clone());
        fixture.teardown.register_owner(NativeSlot::Webview, webview.clone());
        fixture.teardown.register_owner(NativeSlot::PowerAssertion, power.clone());
        fixture.teardown.request(TeardownCause::Request, None);

        assert_eq!(outcome(&fixture).result, TeardownResult::Clean);
        assert_eq!(*lock(&order), vec!["power", "hud", "webview"]);
        assert_eq!(power.releases(), 1);
        assert_eq!(hud.releases(), 1);
        assert_eq!(webview.releases(), 1);
    }

    #[test]
    fn a_failed_owner_is_named_once_and_does_not_mask_an_unacknowledged_one() {
        let fixture = fixture();
        let order = Arc::new(Mutex::new(Vec::new()));
        let failed = FakeOwner::new("power", false, OwnerBehaviour::Fail, &order);
        let hung = FakeOwner::new("hud-owner", false, OwnerBehaviour::Hang, &order);
        fixture.teardown.register_owner(NativeSlot::PowerAssertion, failed);
        fixture.teardown.register_owner(NativeSlot::Hud, hung);
        fixture.teardown.request(TeardownCause::Request, None);
        advance_until_outcome(&fixture);

        let outcome = outcome(&fixture);
        assert_eq!(outcome.result, TeardownResult::Partial);
        assert_eq!(outcome.failed, vec!["power".to_owned()]);
        assert_eq!(outcome.unacknowledged, vec!["hud-owner".to_owned()]);
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
            unacknowledged: vec!["config-watcher".to_owned(), "module-a".to_owned()],
        };
        assert_eq!(
            outcome.to_string(),
            "teardown: outcome=uncertain cause=terminate elapsed_ms=5000 escalated=true \
             recorded_code=1 failed=[] unacknowledged=[config-watcher,module-a]"
        );
    }

    #[test]
    fn a_request_from_another_thread_is_observed() {
        let fixture = fixture();
        let teardown = Arc::clone(&fixture.teardown);
        let thread = thread::spawn(move || teardown.request(TeardownCause::Request, None));
        thread.join().expect("request");
        assert!(fixture.teardown.wait_until_requested(LIMIT));
        assert_eq!(outcome(&fixture).result, TeardownResult::Clean);
    }

    #[test]
    fn a_missing_stage_driver_stores_one_internal_uncertain_outcome_and_still_terminates() {
        let teardown = Teardown::new(
            Arc::new(ManualClock::new()),
            Arc::new(RecordingRecords::default()),
            Arc::new(RecordingInstance::default()),
            Arc::new(DiscardLogger),
        );
        let calls = Arc::new(Mutex::new(Vec::new()));
        let terminal = RecordingTerminal::new(ExitGate::new(), Arc::clone(&calls));
        teardown.install_terminal(Arc::clone(&terminal) as Arc<dyn TerminalDriver>);
        teardown.start().expect("start");
        teardown.request(TeardownCause::Request, None);
        let outcome = teardown.wait_for_outcome(LIMIT).expect("outcome");
        assert_eq!(outcome.result, TeardownResult::Uncertain);
        assert_eq!(outcome.unacknowledged, vec!["stages".to_owned()]);
        assert!(
            teardown.wait_for_terminal(LIMIT),
            "a coordinator with no stage driver must still terminate"
        );
        assert_eq!(terminal.actions(), vec![TerminalAction::Exit(1)]);
        assert!(
            lock(&calls).contains(&"open_gate".to_owned()),
            "the exit gate opens only after the outcome exists"
        );
    }
}
