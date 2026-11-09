//! The owned borders runner and its lifecycle handle.
//!
//! # Authority
//!
//! Accepted `tiling.borders.enabled: true` is the only consent. It lets Stache
//! overwrite the shared `JankyBorders` width, style, hidpi, active and inactive
//! colors and blacklist; their previous values are neither read nor restored.
//! Stache never starts, restarts or kills a borders service: every attempt
//! looks `git.felix.borders` up first ([`ServiceAttempt::begin`]), a missing
//! service publishes a diagnostic and sends nothing (no CLI either), and the
//! CLI fallback runs only after that lookup succeeded in the same attempt and
//! never after an [`SendOutcome::Uncertain`] send.
//!
//! Stop, pause, disable and reload-to-disabled cancel the runner's animation and
//! release the owned connection. They send **no** mutation (no `width=0`): the
//! service keeps the last settings Stache sent.
//!
//! # Runner
//!
//! [`BordersControl`] is inert until an accepted enabled configuration: no
//! thread, port, child or timer exists while borders are disabled. When enabled
//! it owns one [`BordersWorker`] thread that creates, uses and drops the
//! `!Send` transport, owns the [`CliRunner`] and its own cancellation token.
//! Updates arrive as plain values in a latest-update slot; each deposit bumps a
//! monotonic epoch. The single runner thread is the only sender, so an
//! animation frame can never be reordered behind a newer base command; the
//! epoch lets it also skip a frame whose base command was already superseded
//! (checked under the slot guard, which is never held across a send or a sleep).
//! Frames are paced at no more than 8 per second with cancellation-wakeable
//! waits. A failed send is not cached as success; only a **new** update (or an
//! explicit [`BordersControl::retry`]) tries again, and there is no retry loop.

use std::fmt::Write as _;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use stache_macos::borders::{
    BordersTransport, CLI_CHILD_DEADLINE, CliOutcome, CliRunner, MACH_SEND_DEADLINE_MAX,
    MACH_SEND_TIMED_OUT, MissingService, Rejection, SERVICE_NAME, SendOutcome, ServiceAttempt,
};
use stache_sdk::commands::CommandExecutor;
use stache_sdk::threading::CancellationToken;

use super::policy::{
    AnimationPlan, FRAME_PERIOD, FocusInput, PingPong, base_args, blacklist, focus_command,
};
use crate::actor::{Clock, join_within};
use crate::config::{BordersConfig, TilingConfig};

/// The runner thread name.
pub const BORDERS_THREAD_NAME: &str = "stache-tiling-borders";
/// Per-frame Mach send deadline (frames are Mach only, never CLI).
pub const FRAME_SEND_DEADLINE: Duration = Duration::from_millis(100);
/// How long a finished runner waits to reap a helper child it could not reap
/// (the SDK's cancel/reap bound).
pub const REAP_BOUND: Duration = CLI_CHILD_DEADLINE;
/// The longest a stop waits for the runner: one Mach send, one CLI child and
/// the cancel/reap window.
pub const JOIN_BOUND: Duration = Duration::from_millis(500 + 2_000 + 2_000);

/// Creates the transport **on the runner thread** (it is `!Send`).
pub type TransportFactory = Arc<dyn Fn() -> Box<dyn BordersTransport> + Send + Sync>;
/// Creates the executor owning the helper CLI's retained-child slots.
pub type ExecutorFactory = Arc<dyn Fn() -> CommandExecutor + Send + Sync>;
/// Receives the current delivery fault (`None` clears it).
pub type FaultSink = Arc<dyn Fn(Option<&Fault>) + Send + Sync>;

/// What the host composes for borders. `None` in the module parts means no
/// authority to touch the service: an enabled configuration then only reports
/// a diagnostic.
#[derive(Clone)]
pub struct BordersParts {
    /// Builds the transport on the runner thread.
    pub transport: TransportFactory,
    /// Builds the executor for the helper CLI.
    pub executor: ExecutorFactory,
    /// The shortest period between animation frames (never below 125 ms).
    pub frame_period: Duration,
    /// Debug log of every command sent to the service (`None` is silent).
    pub logger: Option<stache_sdk::ScopedLogger>,
    /// The runtime-start reference of the timestamps in the debug log.
    pub origin: crate::logclock::LogOrigin,
}

impl BordersParts {
    /// Parts with the production 8 fps pacing.
    #[must_use]
    pub fn new(transport: TransportFactory, executor: ExecutorFactory) -> Self {
        Self {
            transport,
            executor,
            frame_period: FRAME_PERIOD,
            logger: None,
            origin: crate::logclock::LogOrigin::default(),
        }
    }
}

/// The class of a delivery problem, as surfaced in status diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    /// The service `git.felix.borders` is not registered.
    MissingService,
    /// Stache has no authority to touch the service (not composed).
    Authority,
    /// A send or helper child outlived its deadline; delivery is uncertain.
    Timeout,
    /// The kernel or the helper rejected the command, or the argv is unsafe.
    Protocol,
    /// A runner or helper child could not be fully released.
    Cleanup,
    /// The runner thread could not be started.
    Spawn,
}

/// A typed delivery problem. Borders degrade; nothing else is affected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    /// The class.
    pub kind: FaultKind,
    /// The human message.
    pub message: String,
}

impl Fault {
    fn new(kind: FaultKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }

    /// The status diagnostic text.
    #[must_use]
    pub fn describe(&self) -> String { format!("borders: {}", self.message) }
}

/// The accepted, immutable inputs of the runner.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The borders configuration.
    pub config: BordersConfig,
    /// The precomputed blacklist.
    pub blacklist: String,
}

impl Settings {
    /// The settings of `config`, or `None` when borders are not enabled.
    #[must_use]
    pub fn from_config(config: &TilingConfig) -> Option<Self> {
        config.borders.enabled.then(|| Self {
            blacklist: blacklist(&config.ignore, &config.borders.ignore),
            config: config.borders.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Core: synchronous, clock-driven, fake-testable
// ---------------------------------------------------------------------------

/// One batch of deposited updates.
#[derive(Clone, Debug, Default)]
pub struct Update {
    pub settings: Option<Arc<Settings>>,
    pub focus: Option<FocusInput>,
    pub retry: bool,
}

struct Animation {
    cursor: PingPong,
    started: Instant,
    /// The epoch of the base command this animation belongs to.
    epoch: u64,
    /// Not before this instant.
    due: Instant,
}

/// The send engine owned by the runner thread.
pub struct Core {
    transport: Box<dyn BordersTransport>,
    cli: CliRunner,
    cancel: CancellationToken,
    clock: Arc<dyn Clock>,
    period: Duration,
    sink: FaultSink,
    delivered: Arc<AtomicBool>,
    settings: Option<Arc<Settings>>,
    focus: Option<FocusInput>,
    base_ok: bool,
    last_focus_args: Option<Vec<String>>,
    animation: Option<Animation>,
    last_frame: Option<Instant>,
    fault: Option<Fault>,
    reap_bound: Duration,
    logger: Option<stache_sdk::ScopedLogger>,
    origin: crate::logclock::LogOrigin,
}

impl Core {
    pub fn new(
        transport: Box<dyn BordersTransport>,
        cli: CliRunner,
        cancel: CancellationToken,
        clock: Arc<dyn Clock>,
        period: Duration,
        sink: FaultSink,
        delivered: Arc<AtomicBool>,
    ) -> Self {
        Self {
            transport,
            cli,
            cancel,
            clock,
            period: period.max(FRAME_PERIOD),
            sink,
            delivered,
            settings: None,
            focus: None,
            base_ok: false,
            last_focus_args: None,
            animation: None,
            last_frame: None,
            fault: None,
            reap_bound: REAP_BOUND,
            logger: None,
            origin: crate::logclock::LogOrigin::default(),
        }
    }

    /// A shorter reap window (tests).
    #[cfg(test)]
    pub const fn with_reap_bound(mut self, bound: Duration) -> Self {
        self.reap_bound = bound;
        self
    }

    fn set_fault(&mut self, fault: Option<Fault>) {
        if self.fault != fault {
            (self.sink)(fault.as_ref());
            self.fault = fault;
        }
    }

    /// Applies one batch of updates observed at `epoch`.
    pub fn apply(&mut self, update: Update, epoch: u64) {
        if let Some(settings) = update.settings {
            self.settings = Some(settings);
            self.base_ok = false;
            self.last_focus_args = None;
        }
        if update.retry {
            self.base_ok = false;
            self.last_focus_args = None;
        }
        if let Some(focus) = update.focus {
            self.focus = Some(focus);
        }
        let Some(settings) = self.settings.clone() else { return };

        if !self.base_ok {
            self.animation = None;
            let args = base_args(&settings.config, &settings.blacklist);
            if !self.deliver(&args) {
                return;
            }
            self.base_ok = true;
        }
        let Some(focus) = self.focus else { return };
        let command = focus_command(&settings.config, focus);
        if self.last_focus_args.as_ref() == Some(&command.args) {
            // Nothing new reaches the service: a running animation continues
            // under the newest epoch.
            if let Some(animation) = &mut self.animation {
                animation.epoch = epoch;
            }
            return;
        }
        self.animation = None;
        // Send only the settings that changed since the last delivered focus
        // command: reapplying an unchanged `width` makes JankyBorders rebuild
        // every border, which reads as a blink.
        let changed = changed_args(self.last_focus_args.as_deref(), &command.args);
        if self.deliver(&changed) {
            self.last_focus_args = Some(command.args);
            self.animation = command.animation.map(|plan| self.start_animation(plan, epoch));
        } else {
            // The service state is unknown: the same command must be sent again
            // by the next new update.
            self.last_focus_args = None;
        }
    }

    fn start_animation(&self, plan: AnimationPlan, epoch: u64) -> Animation {
        let now = self.clock.now();
        let due = self.last_frame.map_or(now, |last| (last + self.period).max(now));
        Animation {
            cursor: PingPong::new(plan),
            started: now,
            epoch,
            due,
        }
    }

    /// How long until the next frame is due; `None` without an animation.
    pub fn next_wait(&self) -> Option<Duration> {
        let now = self.clock.now();
        self.animation
            .as_ref()
            .map(|animation| animation.due.saturating_duration_since(now))
    }

    /// Sends one frame when one is due and its base command is still current.
    pub fn frame(&mut self, current_epoch: u64) {
        let now = self.clock.now();
        let Some(animation) = &mut self.animation else { return };
        if now < animation.due || animation.epoch != current_epoch {
            return;
        }
        let argument = animation.cursor.frame(now.saturating_duration_since(animation.started));
        animation.due = now + self.period;
        self.last_frame = Some(now);
        if !self.attempt_frame(&[argument]) {
            // No autonomous retry: the next new update starts afresh.
            self.animation = None;
        }
    }

    /// Whether an animation is running.
    #[cfg(test)]
    pub const fn animating(&self) -> bool { self.animation.is_some() }

    /// A frame: lookup, then Mach only. Never the CLI.
    fn attempt_frame(&mut self, args: &[String]) -> bool {
        let outcome = match ServiceAttempt::begin(&mut *self.transport) {
            Ok(mut attempt) => attempt.send(args, FRAME_SEND_DEADLINE),
            Err(missing) => {
                self.set_fault(Some(missing_service(missing)));
                return false;
            }
        };
        match outcome {
            SendOutcome::Sent => {
                self.delivered.store(true, Ordering::SeqCst);
                self.set_fault(None);
                true
            }
            other => {
                self.set_fault(Some(mach_fault(other)));
                false
            }
        }
    }

    /// A base or focus command: lookup, Mach, and the CLI fallback only after a
    /// proven rejection in the same attempt.
    /// Logs every command sent to the service at debug level.
    #[must_use]
    pub fn with_logger(
        mut self,
        logger: Option<stache_sdk::ScopedLogger>,
        origin: crate::logclock::LogOrigin,
    ) -> Self {
        self.logger = logger;
        self.origin = origin;
        self
    }

    fn deliver(&mut self, args: &[String]) -> bool {
        if let Some(logger) = &self.logger {
            logger.debug(format!("{} tiling borders: send {args:?}", self.origin.stamp()));
        }
        let mut attempt = match ServiceAttempt::begin(&mut *self.transport) {
            Ok(attempt) => attempt,
            Err(missing) => {
                self.set_fault(Some(missing_service(missing)));
                return false;
            }
        };
        let outcome = attempt.send(args, MACH_SEND_DEADLINE_MAX);
        let result = match outcome {
            SendOutcome::Sent => Ok(()),
            SendOutcome::Rejected(Rejection::Kernel(code)) => {
                // Proven not enqueued: one CLI fallback in this same attempt,
                // after a fresh lookup so a vanished service never gets the CLI
                // (which could start it).
                match attempt.confirm_present() {
                    Ok(()) => {
                        let cli = attempt.cli_fallback(&self.cli, args, &self.cancel, &self.cancel);
                        cli_result(&cli, code)
                    }
                    Err(missing) => Err(Some(missing_service(missing))),
                }
            }
            other => Err(Some(mach_fault(other))),
        };
        match result {
            Ok(()) => {
                self.delivered.store(true, Ordering::SeqCst);
                self.set_fault(None);
                true
            }
            Err(fault) => {
                if fault.is_some() {
                    self.set_fault(fault);
                }
                false
            }
        }
    }

    /// Releases the connection (no mutation) and reaps helper children for up to
    /// [`REAP_BOUND`]. Returns the runner and the children it could not reap.
    pub fn finish(self) -> Option<Parked> {
        let Self { transport, cli, reap_bound, .. } = self;
        drop(transport);
        let deadline = Instant::now() + reap_bound;
        loop {
            let left = cli.sweep();
            if left.is_empty() {
                return None;
            }
            if Instant::now() >= deadline {
                return Some(Parked { cli, left });
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn missing_service(missing: MissingService) -> Fault {
    Fault::new(
        FaultKind::MissingService,
        format!(
            "the JankyBorders service {SERVICE_NAME} is not running ({missing}); Stache never \
             starts it, so nothing was sent and no CLI was run"
        ),
    )
}

fn mach_fault(outcome: SendOutcome) -> Fault {
    match outcome {
        SendOutcome::Uncertain(code) if code == MACH_SEND_TIMED_OUT => Fault::new(
            FaultKind::Timeout,
            "the Mach send timed out; delivery is uncertain and was not retried or sent over \
             the CLI",
        ),
        SendOutcome::Uncertain(code) => Fault::new(
            FaultKind::Protocol,
            format!(
                "the Mach send ended with code {code}; delivery is uncertain and was not retried \
                 or sent over the CLI"
            ),
        ),
        SendOutcome::Rejected(Rejection::InvalidArguments(error)) => Fault::new(
            FaultKind::Protocol,
            format!("the settings were not sent: {error}"),
        ),
        SendOutcome::Rejected(Rejection::Kernel(code)) => Fault::new(
            FaultKind::Protocol,
            format!("the kernel rejected the message ({code})"),
        ),
        SendOutcome::MissingService => missing_service(MissingService { code: -1 }),
        SendOutcome::Sent => Fault::new(FaultKind::Protocol, "unexpected send outcome"),
    }
}

/// `Err(None)` is a cancellation (shutdown), not a fault.
fn cli_result(outcome: &CliOutcome, code: i32) -> Result<(), Option<Fault>> {
    let prefix = format!("the Mach send was rejected ({code}) and the CLI fallback");
    match outcome {
        CliOutcome::Succeeded => Ok(()),
        CliOutcome::Cancelled => Err(None),
        CliOutcome::TimedOut { retained } => {
            let mut message = format!("{prefix} timed out; the command may already have acted");
            if let Some(child) = retained {
                let _ = write!(message, "; helper child retained: {child}");
            }
            let kind = if retained.is_some() {
                FaultKind::Cleanup
            } else {
                FaultKind::Timeout
            };
            Err(Some(Fault::new(kind, message)))
        }
        CliOutcome::ExitedWithFailure(status) => Err(Some(Fault::new(
            FaultKind::Protocol,
            format!("{prefix} exited unsuccessfully ({status:?})"),
        ))),
        CliOutcome::Unavailable(reason) => Err(Some(Fault::new(
            FaultKind::Protocol,
            format!("{prefix} is unavailable: {reason}"),
        ))),
        CliOutcome::InvalidArguments(error) => Err(Some(Fault::new(
            FaultKind::Protocol,
            format!("the settings were not sent: {error}"),
        ))),
        CliOutcome::OutputOverflow => Err(Some(Fault::new(
            FaultKind::Protocol,
            format!("{prefix} exceeded its output cap; the command may already have acted"),
        ))),
        CliOutcome::Failed(reason) => Err(Some(Fault::new(
            FaultKind::Protocol,
            format!("{prefix} failed: {reason}"),
        ))),
    }
}

/// A runner whose helper child could not be reaped, kept for a later retry.
pub struct Parked {
    pub(super) cli: CliRunner,
    pub(super) left: Vec<String>,
}

// ---------------------------------------------------------------------------
// Worker thread
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Slot {
    settings: Option<Arc<Settings>>,
    focus: Option<FocusInput>,
    retry: bool,
    dirty: bool,
    closed: bool,
}

struct Shared {
    slot: Mutex<Slot>,
    wake: Condvar,
    /// Bumped by every deposit.
    epoch: AtomicU64,
    cancel: CancellationToken,
    parked: Mutex<Option<Parked>>,
    delivered: Arc<AtomicBool>,
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The one owned runner thread. Dropping it closes it; the handle is joined by
/// its bounded `join_within`, never detached.
pub struct BordersWorker {
    shared: Arc<Shared>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl BordersWorker {
    fn spawn(
        parts: &BordersParts,
        clock: Arc<dyn Clock>,
        sink: FaultSink,
        settings: Arc<Settings>,
        focus: Option<FocusInput>,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot {
                settings: Some(settings),
                focus,
                retry: false,
                dirty: true,
                closed: false,
            }),
            wake: Condvar::new(),
            epoch: AtomicU64::new(1),
            cancel: CancellationToken::new(),
            parked: Mutex::new(None),
            delivered: Arc::new(AtomicBool::new(false)),
        });
        let thread_shared = Arc::clone(&shared);
        let transport = Arc::clone(&parts.transport);
        let executor = Arc::clone(&parts.executor);
        let period = parts.frame_period;
        let logger = parts.logger.clone();
        let origin = parts.origin;
        let handle =
            std::thread::Builder::new()
                .name(BORDERS_THREAD_NAME.to_owned())
                .spawn(move || {
                    let core = Core::new(
                        transport(),
                        CliRunner::new(executor()),
                        thread_shared.cancel.clone(),
                        clock,
                        period,
                        sink,
                        Arc::clone(&thread_shared.delivered),
                    )
                    .with_logger(logger, origin);
                    run(core, &thread_shared);
                })?;
        Ok(Self {
            shared,
            handle: Mutex::new(Some(handle)),
        })
    }

    fn deposit(&self, change: impl FnOnce(&mut Slot)) {
        let mut slot = guard(&self.shared.slot);
        change(&mut slot);
        slot.dirty = true;
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        drop(slot);
        self.shared.wake.notify_one();
    }

    /// Closes the runner and wakes every wait. Waits for nothing.
    fn close(&self) {
        guard(&self.shared.slot).closed = true;
        self.shared.cancel.cancel();
        self.shared.wake.notify_all();
    }

    fn join_within(&self, timeout: Duration) -> bool { join_within(&self.handle, timeout) }

    /// Sweeps a parked helper child once. `true` when none is left.
    fn reap(&self) -> bool {
        let mut parked = guard(&self.shared.parked);
        let Some(held) = parked.as_mut() else { return true };
        held.left = held.cli.sweep();
        if held.left.is_empty() {
            *parked = None;
            true
        } else {
            false
        }
    }

    fn retained(&self) -> Vec<String> {
        guard(&self.shared.parked)
            .as_ref()
            .map(|held| held.left.clone())
            .unwrap_or_default()
    }

    fn delivered(&self) -> bool { self.shared.delivered.load(Ordering::SeqCst) }
}

impl Drop for BordersWorker {
    fn drop(&mut self) { self.close(); }
}

fn run(mut core: Core, shared: &Shared) {
    loop {
        let batch = {
            let mut slot = guard(&shared.slot);
            loop {
                if slot.closed {
                    drop(slot);
                    if let Some(parked) = core.finish() {
                        *guard(&shared.parked) = Some(parked);
                    }
                    return;
                }
                if slot.dirty {
                    break;
                }
                match core.next_wait() {
                    None => {
                        slot = shared.wake.wait(slot).unwrap_or_else(PoisonError::into_inner);
                    }
                    // A frame is due and nothing new was deposited.
                    Some(wait) if wait.is_zero() => break,
                    Some(wait) => {
                        slot = shared
                            .wake
                            .wait_timeout(slot, wait)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0;
                    }
                }
            }
            slot.dirty.then(|| {
                let update = Update {
                    settings: slot.settings.take(),
                    focus: slot.focus.take(),
                    retry: std::mem::take(&mut slot.retry),
                };
                slot.dirty = false;
                (update, shared.epoch.load(Ordering::SeqCst))
            })
        };
        // Only a new deposit sends a command; waking for a frame never retries.
        if let Some((update, epoch)) = batch {
            core.apply(update, epoch);
        }
        // The narrow ordering guard: the epoch is read under the slot lock, which
        // is released before the send.
        let current = {
            let _slot = guard(&shared.slot);
            shared.epoch.load(Ordering::SeqCst)
        };
        core.frame(current);
    }
}

// ---------------------------------------------------------------------------
// Lifecycle handle
// ---------------------------------------------------------------------------

struct ControlSlot {
    worker: Option<Arc<BordersWorker>>,
    /// Closed runners kept until joined and reaped (never detached).
    retired: Vec<Arc<BordersWorker>>,
    settings: Option<Arc<Settings>>,
    focus: Option<FocusInput>,
    stopped: bool,
    /// A runner that sent something has left settings on the service.
    left_settings: bool,
}

/// Owns at most one [`BordersWorker`]. Construction is inert: no thread, port,
/// child or timer until an accepted enabled configuration.
pub struct BordersControl {
    parts: Option<BordersParts>,
    clock: Arc<dyn Clock>,
    sink: FaultSink,
    slot: Mutex<ControlSlot>,
}

impl BordersControl {
    /// An inert control. `parts` is `None` when the host composed no borders
    /// authority.
    #[must_use]
    pub fn new(parts: Option<BordersParts>, clock: Arc<dyn Clock>, sink: FaultSink) -> Self {
        Self {
            parts,
            clock,
            sink,
            slot: Mutex::new(ControlSlot {
                worker: None,
                retired: Vec::new(),
                settings: None,
                focus: None,
                stopped: false,
                left_settings: false,
            }),
        }
    }

    fn slot(&self) -> MutexGuard<'_, ControlSlot> { guard(&self.slot) }

    /// Applies an accepted configuration: starts the runner when borders are
    /// enabled, closes it (sending nothing) when they are not, and hands a
    /// changed configuration to a running one.
    pub fn configure(&self, config: &TilingConfig) {
        let settings = Settings::from_config(config).map(Arc::new);
        let mut slot = self.slot();
        if slot.stopped {
            return;
        }
        let Some(settings) = settings else {
            slot.settings = None;
            Self::retire(&mut slot);
            drop(slot);
            (self.sink)(None);
            return;
        };
        // Reap runners that already ended so enable/disable cycles stay bounded.
        slot.retired
            .retain(|worker| !(worker.join_within(Duration::ZERO) && worker.reap()));
        if let Some(worker) = slot.worker.clone() {
            if slot.settings.as_deref() != Some(&*settings) {
                slot.settings = Some(Arc::clone(&settings));
                worker.deposit(|inner| inner.settings = Some(settings));
            }
            return;
        }
        let Some(parts) = &self.parts else {
            slot.settings = Some(settings);
            drop(slot);
            (self.sink)(Some(&Fault::new(
                FaultKind::Authority,
                "borders are requested but this host composed no JankyBorders authority; nothing \
                 is sent",
            )));
            return;
        };
        if !slot.retired.is_empty() {
            slot.settings = Some(settings);
            drop(slot);
            (self.sink)(Some(&Fault::new(
                FaultKind::Cleanup,
                "the previous borders runner has not ended yet; borders stay off until the next \
                 configuration reload",
            )));
            return;
        }
        slot.settings = Some(Arc::clone(&settings));
        match BordersWorker::spawn(
            parts,
            Arc::clone(&self.clock),
            Arc::clone(&self.sink),
            settings,
            slot.focus,
        ) {
            Ok(worker) => slot.worker = Some(Arc::new(worker)),
            Err(error) => {
                drop(slot);
                (self.sink)(Some(&Fault::new(
                    FaultKind::Spawn,
                    format!("the borders runner could not start ({error}); borders are off"),
                )));
            }
        }
    }

    fn retire(slot: &mut ControlSlot) {
        if let Some(worker) = slot.worker.take() {
            slot.left_settings |= worker.delivered();
            worker.close();
            slot.retired.push(worker);
        }
    }

    /// Feeds the focus state as a plain value. Cheap and allocation-free; with
    /// no runner it only remembers the value for a later enable.
    pub fn observe(&self, focus: FocusInput) {
        let mut slot = self.slot();
        if slot.focus == Some(focus) {
            return;
        }
        slot.focus = Some(focus);
        if let Some(worker) = slot.worker.clone() {
            drop(slot);
            worker.deposit(|inner| inner.focus = Some(focus));
        }
    }

    /// Asks the runner to resend its settings (an explicit retry after a fault).
    pub fn retry(&self) {
        let worker = self.slot().worker.clone();
        if let Some(worker) = worker {
            worker.deposit(|inner| inner.retry = true);
        }
    }

    /// Closes every runner for good. Waits for nothing and sends nothing.
    pub fn stop(&self) {
        let mut slot = self.slot();
        slot.stopped = true;
        Self::retire(&mut slot);
        for worker in &slot.retired {
            worker.close();
        }
    }

    /// Joins every closed runner within `timeout` in total and reaps its helper
    /// children. `true` when nothing is left.
    #[must_use]
    pub fn join_within(&self, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        let workers: Vec<Arc<BordersWorker>> = self.slot().retired.clone();
        let mut all = true;
        for worker in &workers {
            let remaining =
                deadline.map_or(timeout, |end| end.saturating_duration_since(Instant::now()));
            all &= worker.join_within(remaining) && worker.reap();
        }
        let mut guard = self.slot();
        let slot = &mut *guard;
        slot.retired.retain(|worker| {
            let done = worker.join_within(Duration::ZERO) && worker.reap();
            if done {
                slot.left_settings |= worker.delivered();
            }
            !done
        });
        drop(guard);
        all
    }

    /// Whether a runner thread is running (false while disabled).
    #[must_use]
    pub fn is_running(&self) -> bool { self.slot().worker.is_some() }

    /// Whether any runner or closed-but-unjoined runner still exists.
    #[must_use]
    pub fn owes_cleanup(&self) -> bool {
        let slot = self.slot();
        slot.worker.is_some() || !slot.retired.is_empty()
    }

    /// Whether Stache sent a command that the service still holds: after a stop
    /// the service keeps these last settings.
    #[must_use]
    pub fn left_settings(&self) -> bool {
        let slot = self.slot();
        slot.left_settings
            || slot.worker.iter().chain(&slot.retired).any(|worker| worker.delivered())
    }

    /// Helper children a stopped runner could not reap.
    #[must_use]
    pub fn retained(&self) -> Vec<String> {
        self.slot().retired.iter().flat_map(|worker| worker.retained()).collect()
    }
}

/// The `key=value` arguments of `next` whose value differs from `previous`
/// (all of `next` without a previous command). Argument boundaries are kept.
pub(super) fn changed_args(previous: Option<&[String]>, next: &[String]) -> Vec<String> {
    let Some(previous) = previous else { return next.to_vec() };
    next.iter()
        .filter(|arg| {
            let key = arg.split_once('=').map_or(arg.as_str(), |(key, _)| key);
            !previous.iter().any(|old| {
                old.split_once('=').map_or(old.as_str(), |(old_key, _)| old_key) == key
                    && *old == **arg
            })
        })
        .cloned()
        .collect()
}
