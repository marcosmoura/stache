//! The dedicated AX owner thread.
//!
//! ```text
//! callers (any thread) --bounded SyncSender<Command>--> "stache-ax-owner"
//!                                                         ├─ A: AxApi          (created in-thread)
//!                                                         └─ HashMap<PresentationId,
//!                                                              Vec<(ActionToken, A::Element)>>
//! ```
//!
//! The thread creates the AX API inside itself from a `Send` factory. AX
//! elements ([`AxElement`](super::ax::AxElement)) are `!Send`, so they cannot
//! cross the channel: commands and replies carry only pure data, IDs and typed
//! outcomes. The owner never dispatches to the main thread and never waits on
//! another thread, so no main -> owner -> main wait exists.
//!
//! Retained elements for actionable leaves live in the per-presentation map
//! until a release command (or shutdown) drops them on the owner thread.
//!
//! # Press and leases
//!
//! [`AxOwner::lease`] records a pure-data [`Lease`] on one action token in a
//! shared table (no native object). [`AxOwner::submit_press`] queues a press
//! that carries the lease; the owner looks the element up (in the live map, or
//! in the deferred list when its presentation was already released), checks the
//! [`PressGate`] and deadline, sets the 1 s messaging timeout on exactly that
//! element, commits the gate immediately before the send and calls `AXPress`.
//! A release command defers every leased element instead of dropping it and
//! reports the leased part honestly; the element is dropped exactly once, on
//! the owner thread, after its lease ended (the owner sweeps while elements are
//! deferred). The press itself runs on the owner thread, so a stalled send
//! delays queued release commands (which then time out as pending) but never
//! delays logical invalidation: [`PressGate::cancel`] is a lock-free
//! compare-and-swap. Panics inside owner work are contained with `catch_unwind` (effective in
//! unwind-enabled builds only; the release profile aborts).

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use stache_sdk::CancellationToken;

use super::ax::{AxApi, AxError, SystemAx};
use super::invocation::{PRESS_TIMEOUT, PressGate, PressOutcome};
use super::reader::{
    MenuClock, MenuReadOutcome, MenuTarget, ReadContext, TargetValidator, read_menu,
};
use super::snapshot::{ActionToken, PresentationId};
use crate::applications::ACKNOWLEDGEMENT_BUDGET;

/// The stable owner thread name.
pub const OWNER_THREAD_NAME: &str = "stache-ax-owner";
/// Maximum queued commands.
pub const QUEUE_BOUND: usize = 16;

/// Dependencies the owner thread uses; all `Send + Sync`.
#[derive(Clone)]
pub struct OwnerConfig {
    /// The monotonic clock.
    pub clock: Arc<dyn MenuClock>,
    /// Identity validation hook.
    pub validator: Arc<dyn TargetValidator>,
    /// This process id.
    pub own_pid: i32,
}

/// One read request.
pub struct ReadRequest {
    /// The captured target, or `None` when nothing was frontmost.
    pub target: Option<MenuTarget>,
    /// The unique presentation this read creates.
    pub presentation: PresentationId,
    /// Cancellation; observed before every AX call.
    pub cancel: CancellationToken,
    /// An optional deadline tighter than the 1.5 s read budget.
    pub deadline: Option<Instant>,
}

/// The result of an acknowledged release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuReleaseOutcome {
    /// The presentation's retained elements were released.
    Released {
        /// How many retained elements were dropped.
        elements: usize,
    },
    /// No such presentation (already released, failed read, or never read).
    Unknown,
    /// No acknowledgement in time. The command stays queued and will still run;
    /// cleanup is pending, not successful.
    Pending,
    /// The command could not be queued (full or owner stopped).
    DispatchFailure,
    /// Part of the presentation was released; the rest is leased by a selected
    /// action and will be released exactly once after its press resolves.
    Deferred {
        /// Elements dropped now.
        released: usize,
        /// Elements kept alive by a lease.
        leased: usize,
    },
}

/// Why a command could not be queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitError {
    /// The bounded queue is full.
    Full,
    /// The owner has stopped or never started.
    Closed,
}

/// A pure-data claim that keeps one action's element alive until its press
/// resolves. Ending is idempotent and happens on drop.
#[derive(Debug)]
pub struct Lease {
    token: ActionToken,
    table: Arc<LeaseTable>,
}

impl Lease {
    /// The leased action.
    #[must_use]
    pub const fn token(&self) -> ActionToken { self.token }
}

impl Drop for Lease {
    fn drop(&mut self) { self.table.end(self.token); }
}

/// The shared lease table: tokens only, never a native object.
#[derive(Debug, Default)]
struct LeaseTable {
    held: Mutex<HashMap<ActionToken, u32>>,
}

impl LeaseTable {
    fn table(&self) -> std::sync::MutexGuard<'_, HashMap<ActionToken, u32>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn begin(self: &Arc<Self>, token: ActionToken) -> Lease {
        *self.table().entry(token).or_insert(0) += 1;
        Lease { token, table: Arc::clone(self) }
    }

    fn end(&self, token: ActionToken) {
        let mut table = self.table();
        if let Some(count) = table.get_mut(&token) {
            *count -= 1;
            if *count == 0 {
                table.remove(&token);
            }
        }
    }

    fn is_held(&self, token: ActionToken) -> bool { self.table().contains_key(&token) }
}

/// One press request.
pub struct PressRequest {
    /// The selected action.
    pub token: ActionToken,
    /// Refuse to send after this instant.
    pub deadline: Instant,
    /// The presentation-specific one-shot issuance gate.
    pub gate: PressGate,
    /// The lease, dropped (ended) on the owner after the press resolves.
    pub lease: Lease,
}

type PressReply = Box<dyn FnOnce(PressOutcome) + Send>;
type Deferred<E> = Vec<(ActionToken, E)>;
/// How often the owner sweeps deferred elements while any exist.
const SWEEP_INTERVAL: Duration = Duration::from_millis(20);

type ReadReply = Box<dyn FnOnce(MenuReadOutcome) + Send>;
type ReleaseReply = Box<dyn FnOnce(MenuReleaseOutcome) + Send>;
type Retained<E> = HashMap<PresentationId, Vec<(ActionToken, E)>>;

enum Command {
    Read {
        request: ReadRequest,
        reply: ReadReply,
    },
    Release {
        presentation: PresentationId,
        reply: ReleaseReply,
    },
    Press {
        request: PressRequest,
        reply: PressReply,
    },
    Shutdown {
        reply: ReleaseReply,
    },
}

/// A handle to the AX owner thread. `Send + Sync`; holds no native object.
pub struct AxOwner {
    sender: Mutex<Option<SyncSender<Command>>>,
    leases: Arc<LeaseTable>,
}

impl AxOwner {
    /// Starts the owner over the real AX bindings. The binding is created on the
    /// owner thread and nothing is messaged until a read.
    #[must_use]
    pub fn system(config: OwnerConfig) -> Self { Self::new(SystemAx::new, config) }

    /// Starts the owner thread; `factory` runs *on* that thread.
    #[must_use]
    pub fn new<A, F>(factory: F, config: OwnerConfig) -> Self
    where
        A: AxApi + 'static,
        F: FnOnce() -> A + Send + 'static, {
        let (sender, receiver) = mpsc::sync_channel::<Command>(QUEUE_BOUND);
        let leases = Arc::new(LeaseTable::default());
        let owner_leases = Arc::clone(&leases);
        let spawned = thread::Builder::new()
            .name(OWNER_THREAD_NAME.to_owned())
            .spawn(move || run(&factory(), &config, &receiver, &owner_leases));
        Self {
            sender: Mutex::new(spawned.ok().map(|_| sender)),
            leases,
        }
    }

    fn submit(&self, command: Command) -> Result<(), SubmitError> {
        self.sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .ok_or(SubmitError::Closed)?
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => SubmitError::Full,
                TrySendError::Disconnected(_) => SubmitError::Closed,
            })
    }

    /// Queues a read; `reply` runs on the owner thread with the outcome. On
    /// `Err` the request was not queued and `reply` is dropped uncalled.
    ///
    /// # Errors
    ///
    /// [`SubmitError`] when the command cannot be queued.
    pub fn submit_read(
        &self,
        request: ReadRequest,
        reply: impl FnOnce(MenuReadOutcome) + Send + 'static,
    ) -> Result<(), SubmitError> {
        self.submit(Command::Read {
            request,
            reply: Box::new(reply),
        })
    }

    /// Queues a release of one presentation; `reply` runs on the owner thread.
    ///
    /// # Errors
    ///
    /// [`SubmitError`] when the command cannot be queued.
    pub fn submit_release(
        &self,
        presentation: PresentationId,
        reply: impl FnOnce(MenuReleaseOutcome) + Send + 'static,
    ) -> Result<(), SubmitError> {
        self.submit(Command::Release {
            presentation,
            reply: Box::new(reply),
        })
    }

    /// Takes a lease on `token`: a release will defer that element instead of
    /// dropping it until the lease ends. Touches no native object and never
    /// waits, so it is safe from a `Cocoa` callback.
    #[must_use]
    pub fn lease(&self, token: ActionToken) -> Lease { self.leases.begin(token) }

    /// Queues a press; `reply` runs on the owner thread with the outcome. On
    /// `Err` the request was dropped uncalled (its lease ends), so no press can
    /// happen later.
    ///
    /// # Errors
    ///
    /// [`SubmitError`] when the command cannot be queued.
    pub fn submit_press(
        &self,
        request: PressRequest,
        reply: impl FnOnce(PressOutcome) + Send + 'static,
    ) -> Result<(), SubmitError> {
        self.submit(Command::Press {
            request,
            reply: Box::new(reply),
        })
    }

    /// Releases a presentation, waiting at most [`ACKNOWLEDGEMENT_BUDGET`]. A
    /// timeout is [`MenuReleaseOutcome::Pending`], never success. Never call
    /// from the main thread.
    #[must_use]
    pub fn release(&self, presentation: PresentationId) -> MenuReleaseOutcome {
        self.release_within(presentation, ACKNOWLEDGEMENT_BUDGET)
    }

    /// [`Self::release`] with an explicit wait.
    #[must_use]
    pub fn release_within(
        &self,
        presentation: PresentationId,
        wait: Duration,
    ) -> MenuReleaseOutcome {
        let (tx, rx) = mpsc::sync_channel(1);
        let queued = self.submit_release(presentation, move |outcome| {
            let _ = tx.send(outcome);
        });
        wait_for(queued, &rx, wait)
    }

    /// Releases everything and stops the thread, waiting at most
    /// [`ACKNOWLEDGEMENT_BUDGET`]. A timeout is [`MenuReleaseOutcome::Pending`].
    #[must_use]
    pub fn shutdown(&self) -> MenuReleaseOutcome { self.shutdown_within(ACKNOWLEDGEMENT_BUDGET) }

    /// [`Self::shutdown`] with an explicit wait.
    #[must_use]
    pub fn shutdown_within(&self, wait: Duration) -> MenuReleaseOutcome {
        let (tx, rx) = mpsc::sync_channel(1);
        let queued = self.submit(Command::Shutdown {
            reply: Box::new(move |outcome| {
                let _ = tx.send(outcome);
            }),
        });
        wait_for(queued, &rx, wait)
    }
}

fn wait_for(
    queued: Result<(), SubmitError>,
    rx: &mpsc::Receiver<MenuReleaseOutcome>,
    wait: Duration,
) -> MenuReleaseOutcome {
    if queued.is_err() {
        return MenuReleaseOutcome::DispatchFailure;
    }
    match rx.recv_timeout(wait) {
        Ok(outcome) => outcome,
        Err(RecvTimeoutError::Timeout) => MenuReleaseOutcome::Pending,
        Err(RecvTimeoutError::Disconnected) => MenuReleaseOutcome::DispatchFailure,
    }
}

/// The owner loop. `api` and everything retained stay on this thread.
fn run<A: AxApi>(
    api: &A,
    config: &OwnerConfig,
    receiver: &mpsc::Receiver<Command>,
    leases: &LeaseTable,
) {
    let mut retained: Retained<A::Element> = HashMap::new();
    let mut deferred: Deferred<A::Element> = Vec::new();
    // When every handle is dropped the loop ends and `retained` drops here, on
    // the owner thread, releasing every element.
    loop {
        let command = if deferred.is_empty() {
            receiver.recv().ok()
        } else {
            match receiver.recv_timeout(SWEEP_INTERVAL) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => {
                    sweep(&mut deferred, leases);
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => None,
            }
        };
        let Some(command) = command else { return };
        match command {
            Command::Read { request, reply } => {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    handle_read(api, config, &mut retained, &request)
                }))
                .unwrap_or(MenuReadOutcome::OwnerPanic);
                let _ = catch_unwind(AssertUnwindSafe(|| reply(outcome)));
            }
            Command::Release { presentation, reply } => {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    handle_release(&mut retained, &mut deferred, leases, presentation)
                }))
                .unwrap_or(MenuReleaseOutcome::DispatchFailure);
                let _ = catch_unwind(AssertUnwindSafe(|| reply(outcome)));
            }
            Command::Press { request, reply } => {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    handle_press(api, config, &retained, &deferred, &request)
                }))
                .unwrap_or(PressOutcome::OwnerPanic);
                let _ = catch_unwind(AssertUnwindSafe(|| reply(outcome)));
                // The lease ends once the press resolved and was reported.
                drop(request);
            }
            Command::Shutdown { reply } => {
                let elements = retained.values().map(Vec::len).sum::<usize>() + deferred.len();
                retained.clear();
                deferred.clear();
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    reply(MenuReleaseOutcome::Released { elements });
                }));
                return;
            }
        }
        sweep(&mut deferred, leases);
    }
}

/// Drops deferred elements whose lease ended. Each is dropped exactly once.
fn sweep<E>(deferred: &mut Deferred<E>, leases: &LeaseTable) {
    deferred.retain(|(token, _)| leases.is_held(*token));
}

fn handle_release<E>(
    retained: &mut Retained<E>,
    deferred: &mut Deferred<E>,
    leases: &LeaseTable,
    presentation: PresentationId,
) -> MenuReleaseOutcome {
    let Some(items) = retained.remove(&presentation) else {
        return MenuReleaseOutcome::Unknown;
    };
    let total = items.len();
    let mut kept = 0;
    for (token, element) in items {
        if leases.is_held(token) {
            deferred.push((token, element));
            kept += 1;
        }
    }
    if kept == 0 {
        MenuReleaseOutcome::Released { elements: total }
    } else {
        MenuReleaseOutcome::Deferred {
            released: total - kept,
            leased: kept,
        }
    }
}

/// The send boundary. Order matters: staleness, cancellation and deadline are
/// checked, the 1 s timeout is set on this exact element object, and only then
/// the one-shot gate commits immediately before `AXPress`.
fn handle_press<A: AxApi>(
    api: &A,
    config: &OwnerConfig,
    retained: &Retained<A::Element>,
    deferred: &Deferred<A::Element>,
    request: &PressRequest,
) -> PressOutcome {
    let token = request.token;
    let element = retained
        .get(&token.presentation())
        .and_then(|items| items.iter().find(|(held, _)| *held == token))
        .or_else(|| deferred.iter().find(|(held, _)| *held == token))
        .map(|(_, element)| element);
    let Some(element) = element else {
        return PressOutcome::Stale;
    };
    if request.gate.is_cancelled() {
        return PressOutcome::Cancelled;
    }
    if config.clock.now() >= request.deadline {
        return PressOutcome::ExpiredBeforeSend;
    }
    if let Err(error) = api.set_messaging_timeout(element, PRESS_TIMEOUT) {
        return PressOutcome::TimeoutSetFailed(error);
    }
    if config.clock.now() >= request.deadline {
        return PressOutcome::ExpiredBeforeSend;
    }
    // At-most-once: the compare-and-swap loses to a cancellation that won first.
    if !request.gate.commit_send() {
        return PressOutcome::Cancelled;
    }
    match api.press(element) {
        Ok(()) => PressOutcome::Pressed,
        Err(AxError::CannotComplete) => PressOutcome::Uncertain(AxError::CannotComplete),
        Err(AxError::ApiDisabled) => PressOutcome::PermissionDenied,
        Err(
            error @ (AxError::InvalidElement
            | AxError::ActionUnsupported
            | AxError::AttributeUnsupported
            | AxError::NotImplemented),
        ) => PressOutcome::Unsupported(error),
        Err(error) => PressOutcome::Failed(error),
    }
}

fn handle_read<A: AxApi>(
    api: &A,
    config: &OwnerConfig,
    retained: &mut Retained<A::Element>,
    request: &ReadRequest,
) -> MenuReadOutcome {
    let context = ReadContext {
        clock: config.clock.as_ref(),
        cancel: &request.cancel,
        validator: config.validator.as_ref(),
        own_pid: config.own_pid,
        request_deadline: request.deadline,
    };
    match read_menu(api, &context, request.target.as_ref(), request.presentation) {
        Ok(success) => {
            if retained.contains_key(&request.presentation) {
                // A reused presentation id is a caller bug; `success` drops
                // here, releasing the new elements instead of replacing live ones.
                return MenuReadOutcome::DispatchFailure;
            }
            retained.insert(request.presentation, success.retained);
            MenuReadOutcome::Snapshot(success.snapshot)
        }
        Err(outcome) => outcome,
    }
}

const _: () = {
    const fn assert_send<T: Send + 'static>() {}
    const fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send::<Command>();
    assert_send::<ReadRequest>();
    assert_send::<PressRequest>();
    assert_send::<Lease>();
    assert_send::<OwnerConfig>();
    assert_send_sync::<AxOwner>();
    assert_send::<MenuReleaseOutcome>();
};
