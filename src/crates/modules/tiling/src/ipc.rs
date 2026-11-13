//! The Tiling socket handler: `pause`, `resume`, `query`, `window`, `workspace`.
//!
//! [`TilingIpc`] is served through the app's `ModuleFactory::ipc_handlers` and
//! wrapped in the host's gate. It decodes each request once, encodes each
//! response once and keeps three rules:
//!
//! * **Queries** read exactly one published snapshot. A module that is
//!   disabled, paused, pending cleanup or not initialized answers a typed wire
//!   error, never an empty list.
//! * **Mutations** run as an ordered action list. Each action resolves its
//!   selector against a fresh snapshot, submits one typed [`Intent`] and waits
//!   for that intent's record before the next action resolves. The first
//!   reported failure stops the list; earlier admitted actions remain. A
//!   numeric focus target is resolved once to the current exact window token.
//! * **Deadlines**: one finite budget per request (inside the client's total
//!   `IO_TIMEOUT`); each wait is capped by one hop. Work that was never sent is
//!   abandoned and reported as a typed error; work that may have been sent is
//!   reported `uncertain` and never retried. Host begin-shutdown cancels every
//!   wait through the runtime's shutdown token.
//!
//! The success contract is admission: `enqueued` means the actor took the
//! request and changed (or deliberately left) desired state, not that a window
//! moved.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;
use stache_macos::windows::WindowInstance;
use stache_sdk::ipc::{HandlerError, HandlerResult, IpcHandler, decode_params, encode_response};
use stache_sdk::{CancellationToken, ModuleSnapshot, ModuleStatus};

use crate::config::LayoutType;
use crate::dto::{
    ActionEvidence, ActionStatus, AppDto, FrameDto, OperationEvidence, OperationKind,
    OperationResponse, QueryKind, QueryParams, ReasonEvidence, RequestEvidence, ResizeArg,
    ScreenDto, StatusAnimationFallbackDto, StatusAppDto, StatusDto, StatusExcludedDto,
    StatusHiddenDto, StatusNoFrameDto, StatusWindowDto, WindowDto, WindowParams, WorkspaceDto,
    WorkspaceParams, method,
};
use crate::events::IntentSubmitError;
use crate::intent::{
    FocusDirection, FocusTarget, Intent, IntentNoOp, IntentOutcome, IntentRecord, IntentRejection,
    ResizeDimension, ScreenTarget, SwapDirection,
};
use crate::layout::MAX_GRID_TILES;
use crate::module::TilingRuntime;
use crate::rules::{is_system_app_name, is_system_bundle};
use crate::state::{FailedApp, Health, Phase, ScreenState, TilingSnapshot};
use crate::status::{TilingRuntimeState, TilingStatusSource};
use crate::visibility::Ownership;

/// The finite budget of one request. It stays inside the client's total
/// `IO_TIMEOUT` (5 s) so a late answer is never discarded by the client.
pub const REQUEST_BUDGET: Duration = Duration::from_secs(4);
/// The longest one wait (one hop) may take; hops share the request budget.
pub const HOP_CAP: Duration = Duration::from_secs(2);
/// How often a waiter re-reads the published snapshot.
pub const POLL: Duration = Duration::from_millis(2);

/// Wait bounds. Production uses [`IpcTiming::default`]; tests shrink them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IpcTiming {
    /// The whole request.
    pub budget: Duration,
    /// One wait for an actor record or an acknowledgement.
    pub hop: Duration,
    /// The snapshot re-read period.
    pub poll: Duration,
}

impl Default for IpcTiming {
    fn default() -> Self {
        Self {
            budget: REQUEST_BUDGET,
            hop: HOP_CAP,
            poll: POLL,
        }
    }
}

// ------------------------------------------------------------------ seams

/// Why the actor refused to take an intent. Nothing was queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitFailure {
    /// There is no live runtime.
    NotRunning,
    /// The actor's mailbox is closed.
    Closed,
    /// The bounded semantic queue is full.
    Full,
}

/// The handler's view of the running actor: published snapshots in, typed
/// intents out.
pub trait ActorPort: Send + Sync {
    /// The latest published snapshot, `None` without a live runtime.
    fn snapshot(&self) -> Option<Arc<TilingSnapshot>>;

    /// Offers one intent to the actor without blocking.
    ///
    /// # Errors
    ///
    /// [`SubmitFailure`] when nothing was queued.
    fn submit(&self, intent: Intent) -> Result<(), SubmitFailure>;
}

impl ActorPort for TilingRuntime {
    fn snapshot(&self) -> Option<Arc<TilingSnapshot>> {
        self.tracking().map(|runtime| runtime.snapshot())
    }

    fn submit(&self, intent: Intent) -> Result<(), SubmitFailure> {
        let runtime = self.tracking().ok_or(SubmitFailure::NotRunning)?;
        runtime.ingress().submit_intent(intent).map_err(|error| match error {
            IntentSubmitError::Closed => SubmitFailure::Closed,
            IntentSubmitError::Full => SubmitFailure::Full,
        })
    }
}

/// Why a registry lifecycle call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleFailure {
    /// The registry is shutting down.
    ShuttingDown,
    /// The lifecycle call failed with this description.
    Failed(String),
}

/// The registry lifecycle the handler drives and observes. The host implements
/// it over the registry and the module's control.
pub trait LifecycleControl: Send + Sync {
    /// The module's published registry snapshot (no module or lifecycle lock).
    fn module(&self) -> Option<ModuleSnapshot>;

    /// Pauses the module and returns after the registry acknowledged the stop.
    /// Blocking: the handler runs it off the connection thread.
    ///
    /// # Errors
    ///
    /// [`LifecycleFailure`] when the registry refused or the stop failed.
    fn pause(&self) -> Result<(), LifecycleFailure>;

    /// Clears the pause with registry semantics (never overrides a disabled
    /// configuration or a quarantined cleanup).
    ///
    /// # Errors
    ///
    /// [`LifecycleFailure`] when the registry refused or the start failed.
    fn resume(&self) -> Result<(), LifecycleFailure>;
}

/// Whether Tiling can serve a request, from the registry snapshot and the
/// runtime observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// The module is not registered.
    NotInitialized,
    /// The desired configuration is disabled.
    Disabled,
    /// Paused without a configuration edit.
    Paused,
    /// A stop is in progress or its cleanup is quarantined.
    CleanupPending,
    /// Enabled, not paused, not running (stopped, waiting, unavailable or
    /// failed to start).
    NotRunning,
    /// Running with the action authority published.
    Running,
}

/// Maps the published registry snapshot and the runtime state to
/// [`Availability`]. Disabled outranks paused, which outranks pending cleanup.
#[must_use]
pub fn availability(module: Option<&ModuleSnapshot>, runtime: TilingRuntimeState) -> Availability {
    let Some(module) = module else {
        return Availability::NotInitialized;
    };
    if !module.enabled {
        return Availability::Disabled;
    }
    if module.paused && runtime == TilingRuntimeState::Inactive {
        return Availability::Paused;
    }
    if matches!(
        runtime,
        TilingRuntimeState::Quarantined | TilingRuntimeState::Stopping
    ) {
        return Availability::CleanupPending;
    }
    if module.paused {
        return Availability::Paused;
    }
    if module.status == ModuleStatus::Running && runtime == TilingRuntimeState::Running {
        return Availability::Running;
    }
    Availability::NotRunning
}

// -------------------------------------------------------------- bodies

/// The fixed wire bodies. One place so the CLI adapter and the tests agree.
pub mod body {
    /// The module is not registered or not started.
    pub const NOT_INITIALIZED: &str = "Tiling is not initialized";
    /// The runtime exists but the actor reports failed health.
    pub const HANDLE_UNAVAILABLE: &str = "Tiling handle is unavailable";
    /// Desired configuration is disabled.
    pub const DISABLED: &str = "Tiling is disabled";
    /// Paused without a configuration edit.
    pub const PAUSED: &str = "Tiling is paused";
    /// A stop is pending or its cleanup is quarantined.
    pub const CLEANUP_PENDING: &str = "Tiling cleanup is pending";
    /// Host shutdown began.
    pub const SHUTTING_DOWN: &str = "Tiling is shutting down";
    /// No workspace is focused.
    pub const NO_FOCUSED_WORKSPACE: &str = "No focused workspace";
    /// No window is focused.
    pub const NO_FOCUSED_WINDOW: &str = "No focused window";
    /// No workspace has that name.
    pub const WORKSPACE_NOT_FOUND: &str = "Workspace not found";
    /// The workspace has no window and cannot be shown as an empty desktop.
    pub const WORKSPACE_INACTIVE: &str = "Workspace is inactive";
    /// No display matches.
    pub const SCREEN_NOT_FOUND: &str = "Screen not found";
    /// The display has no visible workspace.
    pub const NO_VISIBLE_WORKSPACE: &str = "Target screen has no visible workspace";
    /// The frame could not be computed.
    pub const NO_FRAME: &str = "Tiling could not compute the target frame";
    /// A window request without an action.
    pub const NO_WINDOW_OPERATION: &str =
        "No window operation specified. Use --help for available options.";
    /// A workspace request without an action.
    pub const NO_WORKSPACE_OPERATION: &str =
        "No workspace operation specified. Use --help for available options.";
    /// The actor's channel is closed (nothing was sent).
    pub const SEND_FAILED: &str = "Failed to send message to actor: channel closed";
    /// The actor's queue is full (nothing was sent).
    pub const OVERFLOW: &str = "Actor overflow mailbox is full";

    /// A numeric window id with no current window.
    #[must_use]
    pub fn window_not_found(id: u32) -> String { format!("Window {id} not found") }

    /// A focus target that is neither a direction nor a window id.
    #[must_use]
    pub fn invalid_focus(target: &str) -> String { format!("Invalid focus direction: {target}") }

    /// A swap direction outside the six exact values.
    #[must_use]
    pub fn invalid_swap(direction: &str) -> String {
        format!("Invalid swap direction: {direction}")
    }

    /// A resize dimension other than width or height.
    #[must_use]
    pub fn invalid_dimension(dimension: &str) -> String {
        format!("Invalid resize dimension '{dimension}'. Must be 'width' or 'height'.")
    }

    /// A resize amount that is not an `i32`.
    #[must_use]
    pub fn invalid_amount(amount: &str) -> String {
        format!("Invalid resize amount '{amount}'. Must be an integer.")
    }

    /// A deadline spent before anything was sent. The duration is the
    /// configured budget, never the elapsed time.
    #[must_use]
    pub fn timed_out(budget: std::time::Duration) -> String {
        format!("Query timed out after {budget:?}")
    }
}

// ----------------------------------------------------------- the handler

struct Deadline {
    end: Instant,
}

impl Deadline {
    fn after(budget: Duration) -> Self {
        Self {
            end: Instant::now().checked_add(budget).unwrap_or_else(Instant::now),
        }
    }

    fn remaining(&self) -> Duration { self.end.saturating_duration_since(Instant::now()) }
}

/// Serializes the action lists of concurrent requests, so an intent record is
/// unambiguously the one a request is waiting for.
#[derive(Default)]
struct Lane {
    held: Mutex<bool>,
    freed: Condvar,
}

struct LaneGuard<'a>(&'a Lane);

impl Drop for LaneGuard<'_> {
    fn drop(&mut self) {
        *self.0.held.lock().unwrap_or_else(PoisonError::into_inner) = false;
        self.0.freed.notify_one();
    }
}

enum LaneStop {
    Timeout,
    Shutdown,
}

/// The serialization lane shared by every typed action path of one service.
///
/// The socket handler and the Bar's exact focus share it, so an intent record
/// is unambiguously the one its waiter submitted. Cheap to clone.
#[derive(Clone, Default)]
pub struct ActionLane(Arc<Lane>);

impl Lane {
    /// Takes the lane only if it is free right now: never queues.
    fn try_acquire(&self) -> Option<LaneGuard<'_>> {
        {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            if *held {
                return None;
            }
            *held = true;
        }
        Some(LaneGuard(self))
    }

    fn acquire(
        &self,
        deadline: &Deadline,
        cancel: &CancellationToken,
        poll: Duration,
    ) -> Result<LaneGuard<'_>, LaneStop> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if cancel.is_cancelled() {
                return Err(LaneStop::Shutdown);
            }
            if !*held {
                *held = true;
                return Ok(LaneGuard(self));
            }
            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return Err(LaneStop::Timeout);
            }
            let slice = remaining.min(poll.max(Duration::from_millis(1)) * 5);
            held = self.freed.wait_timeout(held, slice).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

/// The retained registry lifecycle worker of a [`TilingIpc`].
///
/// A pause or resume runs on its own thread so the connection can answer
/// within its deadline. The handle is never detached: the next lifecycle call
/// reaps a finished worker, and the host joins a still-running one, bounded,
/// in its factory `finish_shutdown`.
#[derive(Default)]
pub struct LifecycleThread {
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl LifecycleThread {
    fn store(&self, handle: JoinHandle<()>) {
        let previous = self.handle.lock().unwrap_or_else(PoisonError::into_inner).replace(handle);
        // Single flight: a previous worker cleared its busy flag before
        // returning, so this join only reaps a thread that already finished.
        if let Some(previous) = previous {
            let _ = previous.join();
        }
    }

    /// Joins the worker if it finishes within `timeout`. Returns `true` when no
    /// worker remains; a worker still running stays retained and owned.
    pub fn join_within(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let mut slot = self.handle.lock().unwrap_or_else(PoisonError::into_inner);
            match slot.as_ref() {
                None => return true,
                Some(handle) if handle.is_finished() => {
                    if let Some(handle) = slot.take() {
                        let _ = handle.join();
                    }
                    return true;
                }
                Some(_) => {}
            }
            drop(slot);
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// The Tiling socket handler.
pub struct TilingIpc {
    actor: Arc<dyn ActorPort>,
    lifecycle: Arc<dyn LifecycleControl>,
    status: TilingStatusSource,
    cancel: CancellationToken,
    timing: IpcTiming,
    lane: ActionLane,
    lifecycle_busy: Arc<AtomicBool>,
    lifecycle_thread: Arc<LifecycleThread>,
}

impl TilingIpc {
    /// A handler over an actor port, the registry lifecycle, the runtime status
    /// source and the host shutdown token.
    #[must_use]
    pub fn new(
        actor: Arc<dyn ActorPort>,
        lifecycle: Arc<dyn LifecycleControl>,
        status: TilingStatusSource,
        cancel: CancellationToken,
    ) -> Self {
        Self::with_timing(actor, lifecycle, status, cancel, IpcTiming::default())
    }

    /// [`Self::new`] with explicit wait bounds.
    #[must_use]
    pub fn with_timing(
        actor: Arc<dyn ActorPort>,
        lifecycle: Arc<dyn LifecycleControl>,
        status: TilingStatusSource,
        cancel: CancellationToken,
        timing: IpcTiming,
    ) -> Self {
        Self {
            actor,
            lifecycle,
            status,
            cancel,
            timing,
            lane: ActionLane::default(),
            lifecycle_busy: Arc::new(AtomicBool::new(false)),
            lifecycle_thread: Arc::new(LifecycleThread::default()),
        }
    }

    /// Shares `lane` with another typed action path of the same service (the
    /// Bar's [`TilingFocus`]), so their intents never interleave.
    #[must_use]
    pub fn with_lane(mut self, lane: ActionLane) -> Self {
        self.lane = lane;
        self
    }

    /// The retained lifecycle worker, for the host to join at shutdown.
    #[must_use]
    pub fn lifecycle_thread(&self) -> Arc<LifecycleThread> { Arc::clone(&self.lifecycle_thread) }

    fn availability(&self) -> Availability {
        availability(self.lifecycle.module().as_ref(), self.status.read().state)
    }

    fn require_running(&self) -> HandlerResult<()> {
        if self.cancel.is_cancelled() {
            return Err(HandlerError::unavailable(body::SHUTTING_DOWN));
        }
        match self.availability() {
            Availability::Running => Ok(()),
            other => Err(unavailable_error(other)),
        }
    }

    /// The live snapshot, refusing a failed actor.
    fn live_snapshot(&self) -> HandlerResult<Arc<TilingSnapshot>> {
        let snapshot = self
            .actor
            .snapshot()
            .ok_or_else(|| HandlerError::unavailable(body::NOT_INITIALIZED))?;
        if snapshot.health != Health::Healthy {
            return Err(HandlerError::unavailable(body::HANDLE_UNAVAILABLE));
        }
        Ok(snapshot)
    }

    // ------------------------------------------------------------ queries

    fn query(&self, params: Value) -> HandlerResult<Value> {
        let params: QueryParams = decode_params(params)?;
        validate_query(&params)?;
        self.require_running()?;
        let snapshot = self.live_snapshot()?;
        if snapshot.phase == Phase::Initializing && params.kind != QueryKind::Status {
            return Err(HandlerError::unavailable(body::NOT_INITIALIZED));
        }
        match params.kind {
            QueryKind::Screens => encode_response(&screens(&snapshot)),
            QueryKind::Workspaces => encode_response(&workspaces(&snapshot, &params)),
            QueryKind::Windows => encode_response(&windows(&snapshot, &params)),
            QueryKind::Apps => encode_response(&apps(&snapshot)),
            QueryKind::Status => encode_response(&status(&snapshot)),
        }
    }

    // ---------------------------------------------------------- lifecycle

    fn lifecycle_request(&self, params: Value, pause: bool) -> HandlerResult<Value> {
        zero_argument_params(params)?;
        let deadline = Deadline::after(self.timing.budget);
        let kind = if pause {
            OperationKind::Pause
        } else {
            OperationKind::Resume
        };
        let name = if pause { "pause" } else { "resume" };
        if self.cancel.is_cancelled() {
            return Err(HandlerError::unavailable(body::SHUTTING_DOWN));
        }
        match (self.availability(), pause) {
            (Availability::NotInitialized, _) => {
                return Err(HandlerError::unavailable(body::NOT_INITIALIZED));
            }
            (Availability::Disabled, _) => return Err(HandlerError::disabled(body::DISABLED)),
            // A quarantined cleanup is retried by a repeated pause; resume
            // never overrides it.
            (Availability::CleanupPending, false) => {
                return Err(HandlerError::unavailable(body::CLEANUP_PENDING));
            }
            _ => {}
        }
        let request = |status: ActionStatus| RequestEvidence {
            operation: kind,
            actions: vec![ActionEvidence {
                action: name.to_owned(),
                status,
            }],
        };
        match self.run_lifecycle(pause, &deadline) {
            LifecycleResult::Done(Ok(())) => {
                let after = self.availability();
                let settled = if pause {
                    matches!(after, Availability::Paused | Availability::Disabled)
                        && self.status.read().state == TilingRuntimeState::Inactive
                } else {
                    after == Availability::Running
                };
                if settled {
                    return encode_response(&OperationResponse::Applied(OperationEvidence::new(
                        request(ActionStatus::Acknowledged),
                    )));
                }
                if after == Availability::CleanupPending {
                    return encode_response(
                        &self.cleanup_uncertain(request(ActionStatus::SentUnconfirmed)),
                    );
                }
                Err(unavailable_error(after))
            }
            LifecycleResult::Done(Err(LifecycleFailure::ShuttingDown)) => {
                Err(HandlerError::unavailable(body::SHUTTING_DOWN))
            }
            LifecycleResult::Done(Err(LifecycleFailure::Failed(message))) => {
                if self.availability() == Availability::CleanupPending {
                    return encode_response(
                        &self.cleanup_uncertain(request(ActionStatus::SentUnconfirmed)),
                    );
                }
                Err(HandlerError::handler(message))
            }
            LifecycleResult::Busy => {
                // Another pause or resume is still in flight (possibly abandoned
                // after its deadline). This request was never sent, but the
                // module state it would act on is not settled: report a typed
                // uncertain outcome, not a cleanup message that may be false.
                let mut evidence = OperationEvidence::new(request(ActionStatus::NotAttempted));
                evidence.unknown.push(ReasonEvidence::new("stage:lifecycle", "inProgress"));
                encode_response(&OperationResponse::Uncertain(evidence))
            }
            LifecycleResult::Abandoned(reason) => {
                let mut evidence = OperationEvidence::new(request(ActionStatus::SentUnconfirmed));
                evidence.unknown.push(ReasonEvidence::new("stage:teardown", reason));
                encode_response(&OperationResponse::Uncertain(evidence))
            }
        }
    }

    /// An `uncertain` response naming each stage the quarantine still owes.
    fn cleanup_uncertain(&self, request: RequestEvidence) -> OperationResponse {
        let mut evidence = OperationEvidence::new(request);
        let owed = self.status.read().owed;
        if owed.is_empty() {
            evidence.unknown.push(ReasonEvidence::new("stage:teardown", "cleanupOwed"));
        }
        for stage in owed {
            evidence
                .unknown
                .push(ReasonEvidence::new(format!("stage:{stage}"), "cleanupOwed"));
        }
        OperationResponse::Uncertain(evidence)
    }

    /// Runs the blocking registry call on its own thread and waits for its
    /// acknowledgement, the request deadline or host shutdown, whichever comes
    /// first. An abandoned call keeps running but stays owned by
    /// [`LifecycleThread`]; the single-flight flag stops a second one from
    /// starting meanwhile.
    fn run_lifecycle(&self, pause: bool, deadline: &Deadline) -> LifecycleResult {
        if self
            .lifecycle_busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return LifecycleResult::Busy;
        }
        let (sender, receiver) = mpsc::channel();
        let cancel_sender = sender.clone();
        let _listener = self.cancel.on_cancel(Arc::new(move || {
            let _ = cancel_sender.send(LifecycleEvent::Shutdown);
        }));
        let control = Arc::clone(&self.lifecycle);
        let busy = Arc::clone(&self.lifecycle_busy);
        let spawned = std::thread::Builder::new()
            .name("stache-tiling-ipc-lifecycle".to_owned())
            .spawn(move || {
                let result = if pause {
                    control.pause()
                } else {
                    control.resume()
                };
                busy.store(false, Ordering::SeqCst);
                let _ = sender.send(LifecycleEvent::Done(result));
            });
        match spawned {
            Ok(handle) => self.lifecycle_thread.store(handle),
            Err(error) => {
                self.lifecycle_busy.store(false, Ordering::SeqCst);
                return LifecycleResult::Done(Err(LifecycleFailure::Failed(format!(
                    "the lifecycle worker could not start: {error}"
                ))));
            }
        }
        match receiver.recv_timeout(deadline.remaining()) {
            Ok(LifecycleEvent::Done(result)) => LifecycleResult::Done(result),
            Ok(LifecycleEvent::Shutdown) => LifecycleResult::Abandoned("shutdown"),
            Err(RecvTimeoutError::Timeout) => LifecycleResult::Abandoned("noAcknowledgement"),
            Err(RecvTimeoutError::Disconnected) => LifecycleResult::Abandoned("workerEnded"),
        }
    }

    // ---------------------------------------------------------- mutations

    fn window(&self, params: Value) -> HandlerResult<Value> {
        let params: WindowParams = decode_params(params)?;
        let steps = window_steps(params)?;
        self.run_actions(OperationKind::Window, &steps)
    }

    fn workspace(&self, params: Value) -> HandlerResult<Value> {
        let params: WorkspaceParams = decode_params(params)?;
        let steps = workspace_steps(params)?;
        self.run_actions(OperationKind::Workspace, &steps)
    }

    fn run_actions(&self, operation: OperationKind, steps: &[Step]) -> HandlerResult<Value> {
        let deadline = Deadline::after(self.timing.budget);
        self.require_running()?;
        let _lane =
            self.lane.0.acquire(&deadline, &self.cancel, self.timing.poll).map_err(|stop| {
                match stop {
                    LaneStop::Timeout => HandlerError::handler(body::timed_out(self.timing.budget)),
                    LaneStop::Shutdown => HandlerError::unavailable(body::SHUTTING_DOWN),
                }
            })?;
        let mut done: Vec<ActionEvidence> = Vec::with_capacity(steps.len());
        for (index, step) in steps.iter().enumerate() {
            if self.cancel.is_cancelled() {
                return Err(HandlerError::unavailable(body::SHUTTING_DOWN));
            }
            if deadline.remaining().is_zero() {
                return Err(HandlerError::handler(body::timed_out(self.timing.budget)));
            }
            let snapshot = self.live_snapshot()?;
            let planned = step.resolve(&snapshot)?;
            match self.admit(&planned.intent, &snapshot, &deadline)? {
                Admission::Recorded(record) => {
                    let status = planned.settle(&record.outcome)?;
                    done.push(ActionEvidence {
                        action: planned.name.to_owned(),
                        status,
                    });
                }
                Admission::Unconfirmed(reason) => {
                    done.push(ActionEvidence {
                        action: planned.name.to_owned(),
                        status: ActionStatus::SentUnconfirmed,
                    });
                    let mut evidence =
                        OperationEvidence::new(RequestEvidence { operation, actions: done });
                    evidence
                        .unknown
                        .push(ReasonEvidence::new(format!("action:{}", planned.name), reason));
                    for later in &steps[index + 1..] {
                        evidence.request.actions.push(ActionEvidence {
                            action: later.name().to_owned(),
                            status: ActionStatus::NotAttempted,
                        });
                        evidence.not_attempted.push(ReasonEvidence::new(
                            format!("action:{}", later.name()),
                            "notReached",
                        ));
                    }
                    return encode_response(&OperationResponse::Uncertain(evidence));
                }
            }
        }
        encode_response(&OperationResponse::Enqueued(OperationEvidence::new(
            RequestEvidence { operation, actions: done },
        )))
    }

    /// Offers one intent and waits for its record: the actor's state-transition
    /// order. A refusal before the offer is a typed error (nothing was sent);
    /// once offered, any failure to observe the record is `Unconfirmed`.
    fn admit(
        &self,
        intent: &Intent,
        before: &TilingSnapshot,
        deadline: &Deadline,
    ) -> HandlerResult<Admission> {
        admit_intent(
            self.actor.as_ref(),
            &self.cancel,
            self.timing,
            intent,
            before,
            deadline,
        )
        .map_err(|refusal| match refusal {
            Refusal::Budget => HandlerError::handler(body::timed_out(self.timing.budget)),
            Refusal::NotRunning => HandlerError::unavailable(body::NOT_INITIALIZED),
            Refusal::Closed => HandlerError::handler(body::SEND_FAILED),
            Refusal::Full => HandlerError::handler(body::OVERFLOW),
        })
    }
}

/// Why an intent was never offered.
enum Refusal {
    /// The deadline left no room for a wait.
    Budget,
    /// No live runtime.
    NotRunning,
    /// The mailbox is closed.
    Closed,
    /// The bounded queue is full.
    Full,
}

/// Offers one intent and waits (one hop, within `deadline`) for its record.
fn admit_intent(
    actor: &dyn ActorPort,
    cancel: &CancellationToken,
    timing: IpcTiming,
    intent: &Intent,
    before: &TilingSnapshot,
    deadline: &Deadline,
) -> Result<Admission, Refusal> {
    let baseline = recorded(before);
    let wait_end = deadline.remaining().min(timing.hop);
    if wait_end.is_zero() {
        return Err(Refusal::Budget);
    }
    match actor.submit(intent.clone()) {
        Ok(()) => {}
        Err(SubmitFailure::NotRunning) => return Err(Refusal::NotRunning),
        Err(SubmitFailure::Closed) => return Err(Refusal::Closed),
        Err(SubmitFailure::Full) => return Err(Refusal::Full),
    }
    let end = Instant::now().checked_add(wait_end).unwrap_or_else(Instant::now);
    loop {
        let Some(snapshot) = actor.snapshot() else {
            return Ok(Admission::Unconfirmed("actorClosed"));
        };
        if snapshot.health != Health::Healthy {
            return Ok(Admission::Unconfirmed("actorFailed"));
        }
        if recorded(&snapshot) > baseline
            && let Some(record) = &snapshot.last_intent
            && record.intent == *intent
        {
            return Ok(Admission::Recorded(record.clone()));
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(Admission::Unconfirmed("noAcknowledgement"));
        }
        if cancel.wait(left.min(timing.poll)) {
            return Ok(Admission::Unconfirmed("shutdown"));
        }
    }
}

impl IpcHandler for TilingIpc {
    fn handle(&self, method: &str, params: Value) -> HandlerResult<Value> {
        match method {
            method::PAUSE => self.lifecycle_request(params, true),
            method::RESUME => self.lifecycle_request(params, false),
            method::QUERY => self.query(params),
            method::WINDOW => self.window(params),
            method::WORKSPACE => self.workspace(params),
            _ => Err(HandlerError::unknown_method(method)),
        }
    }
}

enum LifecycleEvent {
    Done(Result<(), LifecycleFailure>),
    Shutdown,
}

enum LifecycleResult {
    Done(Result<(), LifecycleFailure>),
    Busy,
    Abandoned(&'static str),
}

enum Admission {
    Recorded(IntentRecord),
    Unconfirmed(&'static str),
}

/// The number of intents the actor has recorded.
const fn recorded(snapshot: &TilingSnapshot) -> u64 {
    snapshot.diagnostics.intents_applied
        + snapshot.diagnostics.intents_noop
        + snapshot.diagnostics.intents_rejected
}

fn unavailable_error(availability: Availability) -> HandlerError {
    match availability {
        Availability::Disabled => HandlerError::disabled(body::DISABLED),
        Availability::Paused => HandlerError::unavailable(body::PAUSED),
        Availability::CleanupPending => HandlerError::unavailable(body::CLEANUP_PENDING),
        Availability::NotInitialized | Availability::NotRunning | Availability::Running => {
            HandlerError::unavailable(body::NOT_INITIALIZED)
        }
    }
}

/// Accepts only the spellings of a zero-argument request: omitted (`null`) or
/// an empty object.
fn zero_argument_params(params: Value) -> HandlerResult<()> {
    let params = match params {
        Value::Object(map) if map.is_empty() => Value::Null,
        other => other,
    };
    decode_params::<()>(params)
}

// ---------------------------------------------------------- exact focus

/// One exact focus request from a host surface (the Bar strip).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExactFocus {
    /// Focus the workspace with this exact name.
    Workspace(String),
    /// Focus the window this opaque published key names (see
    /// [`crate::published::resolve_window_key`]).
    Window(String),
}

/// Why an exact focus was not attempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusUnavailable {
    /// Not registered, not started, not running or not initialized yet.
    NotRunning,
    /// The accepted configuration disables Tiling.
    Disabled,
    /// Paused for this session.
    Paused,
    /// A stop is in progress or its cleanup is quarantined.
    CleanupPending,
    /// Host shutdown began.
    ShuttingDown,
    /// Running, but the actor reports failed health.
    Unhealthy,
}

/// What became of one exact focus request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactFocusOutcome {
    /// The actor admitted it and changed desired state.
    Applied,
    /// Admitted; the target was already focused (or nothing was to do).
    NoOp,
    /// The workspace or window no longer exists (resolved at issuance or
    /// refused by the actor as unknown).
    Stale,
    /// Nothing was attempted.
    Unavailable(FocusUnavailable),
    /// Another action holds the lane, or the actor's queue is full. Nothing
    /// was sent.
    Busy,
    /// Sent, but its record was not observed (deadline, actor failure or
    /// shutdown). Never retried.
    Uncertain,
    /// The actor refused it for another reason (geometry, display).
    Refused,
    /// The actor's mailbox is closed or the budget was spent before sending.
    Failed,
}

/// The published registry facts of the Tiling module.
pub type ModuleSource = Arc<dyn Fn() -> Option<ModuleSnapshot> + Send + Sync>;

/// The typed exact-focus path of one Tiling service.
///
/// It shares the socket handler's availability rules, lane, bounded record wait,
/// cancellation and outcome settling, and submits only
/// [`Intent::FocusWorkspace`] or [`Intent::FocusWindow`] with an exact
/// [`FocusTarget::Window`] token. It never queues behind another action (a held
/// lane is `busy`), never pauses, resumes, enables or reconfigures anything.
pub struct TilingFocus {
    actor: Arc<dyn ActorPort>,
    module: ModuleSource,
    status: TilingStatusSource,
    cancel: CancellationToken,
    timing: IpcTiming,
    lane: ActionLane,
}

impl TilingFocus {
    /// A focus path over the runtime's actor port, the module's registry
    /// facts, its status source, the host shutdown token and a shared lane.
    #[must_use]
    pub fn new(
        actor: Arc<dyn ActorPort>,
        module: ModuleSource,
        status: TilingStatusSource,
        cancel: CancellationToken,
        timing: IpcTiming,
        lane: ActionLane,
    ) -> Self {
        Self {
            actor,
            module,
            status,
            cancel,
            timing,
            lane,
        }
    }

    /// Whether a live runtime reports healthy right now. Cheap: one snapshot
    /// cell load, no lock the actor takes.
    #[must_use]
    pub fn healthy(&self) -> bool {
        self.actor.snapshot().is_some_and(|snapshot| snapshot.health == Health::Healthy)
    }

    /// The exact current window `key` names in the live runtime snapshot.
    #[must_use]
    pub fn resolve(&self, key: &str) -> Option<WindowInstance> {
        self.actor
            .snapshot()
            .and_then(|snapshot| crate::published::resolve_window_key(&snapshot, key))
    }

    /// Why a focus would not be attempted right now, from the same registry
    /// and runtime facts the socket handler uses; `None` when Tiling serves.
    /// Reads published state only.
    #[must_use]
    pub fn unavailable(&self) -> Option<FocusUnavailable> {
        if self.cancel.is_cancelled() {
            return Some(FocusUnavailable::ShuttingDown);
        }
        match availability((self.module)().as_ref(), self.status.read().state) {
            Availability::Running => None,
            Availability::Disabled => Some(FocusUnavailable::Disabled),
            Availability::Paused => Some(FocusUnavailable::Paused),
            Availability::CleanupPending => Some(FocusUnavailable::CleanupPending),
            Availability::NotInitialized | Availability::NotRunning => {
                Some(FocusUnavailable::NotRunning)
            }
        }
    }

    /// Focuses `target` and waits (bounded) for the actor's record.
    #[must_use]
    pub fn focus(&self, target: &ExactFocus) -> ExactFocusOutcome {
        if let Some(reason) = self.unavailable() {
            return ExactFocusOutcome::Unavailable(reason);
        }
        let Some(_lane) = self.lane.0.try_acquire() else {
            return ExactFocusOutcome::Busy;
        };
        let deadline = Deadline::after(self.timing.budget);
        let Some(snapshot) = self.actor.snapshot() else {
            return ExactFocusOutcome::Unavailable(FocusUnavailable::NotRunning);
        };
        if snapshot.health != Health::Healthy {
            return ExactFocusOutcome::Unavailable(FocusUnavailable::Unhealthy);
        }
        if snapshot.phase == Phase::Initializing {
            return ExactFocusOutcome::Unavailable(FocusUnavailable::NotRunning);
        }
        let intent = match target {
            ExactFocus::Workspace(name) => {
                if !snapshot.workspaces.iter().any(|workspace| workspace.name == *name) {
                    return ExactFocusOutcome::Stale;
                }
                Intent::FocusWorkspace(name.clone())
            }
            ExactFocus::Window(key) => {
                let Some(token) = crate::published::resolve_window_key(&snapshot, key) else {
                    return ExactFocusOutcome::Stale;
                };
                Intent::FocusWindow(FocusTarget::Window(token))
            }
        };
        match admit_intent(
            self.actor.as_ref(),
            &self.cancel,
            self.timing,
            &intent,
            &snapshot,
            &deadline,
        ) {
            Err(Refusal::NotRunning) => {
                ExactFocusOutcome::Unavailable(FocusUnavailable::NotRunning)
            }
            Err(Refusal::Full) => ExactFocusOutcome::Busy,
            Err(Refusal::Closed | Refusal::Budget) => ExactFocusOutcome::Failed,
            Ok(Admission::Unconfirmed(_)) => ExactFocusOutcome::Uncertain,
            Ok(Admission::Recorded(record)) => match record.outcome {
                IntentOutcome::Applied => ExactFocusOutcome::Applied,
                IntentOutcome::NoOp(_) => ExactFocusOutcome::NoOp,
                IntentOutcome::Rejected(
                    IntentRejection::UnknownWorkspace | IntentRejection::UnknownWindow,
                ) => ExactFocusOutcome::Stale,
                IntentOutcome::Rejected(_) => ExactFocusOutcome::Refused,
            },
        }
    }
}

// ------------------------------------------------------------- planning

/// One requested action before it resolves against a snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Focus(String),
    Swap(String),
    ToggleFloating,
    Preset(String),
    Resize(ResizeArg),
    SendToScreen(String),
    SendToWorkspace(String),
    FocusWorkspace(String),
    Layout(LayoutType),
    Balance,
    SendWorkspaceToScreen(String),
}

/// What a silent admitted no-op means for an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Needs {
    /// Nothing: a no-op is a silent success.
    Nothing,
    /// A focused window; its absence is an error.
    Window,
    /// A focused workspace; its absence is an error.
    Workspace,
}

struct Planned {
    name: &'static str,
    intent: Intent,
    needs: Needs,
    numeric: Option<u32>,
}

fn window_steps(params: WindowParams) -> HandlerResult<Vec<Step>> {
    let mut steps = Vec::new();
    if let Some(target) = params.focus {
        steps.push(Step::Focus(target));
    }
    if let Some(direction) = params.swap {
        steps.push(Step::Swap(direction));
    }
    if params.toggle_floating {
        steps.push(Step::ToggleFloating);
    }
    if let Some(name) = params.preset {
        steps.push(Step::Preset(name));
    }
    steps.extend(params.resize.into_iter().map(Step::Resize));
    if let Some(screen) = params.send_to_screen {
        steps.push(Step::SendToScreen(screen));
    }
    if let Some(workspace) = params.send_to_workspace {
        steps.push(Step::SendToWorkspace(workspace));
    }
    if steps.is_empty() {
        return Err(HandlerError::handler(body::NO_WINDOW_OPERATION));
    }
    Ok(steps)
}

fn workspace_steps(params: WorkspaceParams) -> HandlerResult<Vec<Step>> {
    let mut steps = Vec::new();
    if let Some(name) = params.focus {
        steps.push(Step::FocusWorkspace(name));
    }
    if let Some(layout) = params.layout {
        steps.push(Step::Layout(layout));
    }
    if params.balance {
        steps.push(Step::Balance);
    }
    if let Some(screen) = params.send_to_screen {
        steps.push(Step::SendWorkspaceToScreen(screen));
    }
    if steps.is_empty() {
        return Err(HandlerError::handler(body::NO_WORKSPACE_OPERATION));
    }
    Ok(steps)
}

impl Step {
    const fn name(&self) -> &'static str {
        match self {
            Self::Focus(_) | Self::FocusWorkspace(_) => "focus",
            Self::Swap(_) => "swap",
            Self::ToggleFloating => "toggleFloating",
            Self::Preset(_) => "preset",
            Self::Resize(_) => "resize",
            Self::SendToScreen(_) | Self::SendWorkspaceToScreen(_) => "sendToScreen",
            Self::SendToWorkspace(_) => "sendToWorkspace",
            Self::Layout(_) => "layout",
            Self::Balance => "balance",
        }
    }

    /// Validates the step's free-form values and resolves selectors against
    /// the snapshot read for this step. This is the late semantic-validation
    /// boundary: earlier steps may already have been admitted.
    fn resolve(&self, snapshot: &TilingSnapshot) -> HandlerResult<Planned> {
        let plain = |intent: Intent, needs: Needs| Planned {
            name: self.name(),
            intent,
            needs,
            numeric: None,
        };
        match self {
            Self::Focus(target) => {
                if let Ok(id) = target.parse::<u32>() {
                    let token = resolve_window(snapshot, id)
                        .ok_or_else(|| HandlerError::handler(body::window_not_found(id)))?;
                    return Ok(Planned {
                        name: "focus",
                        intent: Intent::FocusWindow(FocusTarget::Window(token)),
                        needs: Needs::Nothing,
                        numeric: Some(id),
                    });
                }
                let direction = FocusDirection::parse(target)
                    .ok_or_else(|| HandlerError::handler(body::invalid_focus(target)))?;
                Ok(plain(
                    Intent::FocusWindow(FocusTarget::Direction(direction)),
                    Needs::Nothing,
                ))
            }
            Self::Swap(direction) => {
                let parsed = SwapDirection::parse(direction)
                    .ok_or_else(|| HandlerError::handler(body::invalid_swap(direction)))?;
                Ok(plain(Intent::SwapWindow(parsed), Needs::Nothing))
            }
            Self::ToggleFloating => Ok(plain(Intent::ToggleFloating, Needs::Window)),
            Self::Preset(name) => Ok(plain(Intent::Preset(name.clone()), Needs::Nothing)),
            Self::Resize(pair) => {
                let dimension = match pair.dimension.to_ascii_lowercase().as_str() {
                    "width" => ResizeDimension::Width,
                    "height" => ResizeDimension::Height,
                    _ => {
                        return Err(HandlerError::handler(body::invalid_dimension(
                            &pair.dimension,
                        )));
                    }
                };
                let amount = pair
                    .amount
                    .parse::<i32>()
                    .map_err(|_| HandlerError::handler(body::invalid_amount(&pair.amount)))?;
                Ok(plain(Intent::Resize { dimension, amount }, Needs::Nothing))
            }
            Self::SendToScreen(screen) => Ok(plain(
                Intent::SendWindowToScreen(ScreenTarget::parse(screen)),
                Needs::Nothing,
            )),
            Self::SendToWorkspace(workspace) => Ok(plain(
                Intent::SendWindowToWorkspace {
                    window: None,
                    workspace: workspace.clone(),
                },
                Needs::Window,
            )),
            Self::FocusWorkspace(name) => {
                Ok(plain(Intent::FocusWorkspace(name.clone()), Needs::Nothing))
            }
            Self::Layout(layout) => Ok(plain(Intent::SetLayout(*layout), Needs::Workspace)),
            Self::Balance => Ok(plain(Intent::Balance, Needs::Workspace)),
            Self::SendWorkspaceToScreen(screen) => Ok(plain(
                Intent::SendWorkspaceToScreen(ScreenTarget::parse(screen)),
                Needs::Nothing,
            )),
        }
    }
}

/// The current exact window token for a `CGWindowID`. Ambiguity (two windows
/// claiming one id) resolves to nothing rather than a guess.
fn resolve_window(snapshot: &TilingSnapshot, id: u32) -> Option<WindowInstance> {
    let mut matches = snapshot.windows.iter().filter(|window| window.cg_window_id == Some(id));
    let first = matches.next()?;
    matches.next().is_none().then_some(first.token)
}

impl Planned {
    /// Turns the actor's record for this intent into an action status or the
    /// typed error the appendix names.
    fn settle(&self, outcome: &IntentOutcome) -> HandlerResult<ActionStatus> {
        match outcome {
            IntentOutcome::Applied => Ok(ActionStatus::AdmittedChanged),
            IntentOutcome::NoOp(IntentNoOp::NoFocusedWindow | IntentNoOp::NoFocusedWorkspace)
                if self.needs == Needs::Window =>
            {
                Err(HandlerError::handler(body::NO_FOCUSED_WINDOW))
            }
            IntentOutcome::NoOp(IntentNoOp::NoFocusedWorkspace)
                if self.needs == Needs::Workspace =>
            {
                Err(HandlerError::handler(body::NO_FOCUSED_WORKSPACE))
            }
            IntentOutcome::NoOp(_) => Ok(ActionStatus::AdmittedUnchanged),
            IntentOutcome::Rejected(rejection) => Err(HandlerError::handler(match rejection {
                IntentRejection::UnknownWorkspace => body::WORKSPACE_NOT_FOUND.to_owned(),
                IntentRejection::InactiveWorkspace => body::WORKSPACE_INACTIVE.to_owned(),
                IntentRejection::UnknownWindow => self
                    .numeric
                    .map_or_else(|| body::NO_FOCUSED_WINDOW.to_owned(), body::window_not_found),
                IntentRejection::UnknownScreen => body::SCREEN_NOT_FOUND.to_owned(),
                IntentRejection::NoVisibleWorkspace => body::NO_VISIBLE_WORKSPACE.to_owned(),
                IntentRejection::Geometry(_) | IntentRejection::NoDisplayArea => {
                    body::NO_FRAME.to_owned()
                }
            })),
        }
    }
}

// -------------------------------------------------------------- queries

fn validate_query(params: &QueryParams) -> HandlerResult<()> {
    let conflict = |a: &str, b: &str| {
        Err(HandlerError::invalid_params(format!(
            "conflicting query filters: {a} and {b}"
        )))
    };
    let unsupported = |filter: &str| {
        Err(HandlerError::invalid_params(format!(
            "the {} query does not take {filter}",
            match params.kind {
                QueryKind::Screens => "screens",
                QueryKind::Workspaces => "workspaces",
                QueryKind::Windows => "windows",
                QueryKind::Apps => "apps",
                QueryKind::Status => "status",
            }
        )))
    };
    match params.kind {
        QueryKind::Screens | QueryKind::Apps | QueryKind::Status => {
            if params.screen.is_some() {
                return unsupported("screen");
            }
            if params.workspace.is_some() {
                return unsupported("workspace");
            }
            if params.focused_screen {
                return unsupported("focusedScreen");
            }
            if params.focused_workspace {
                return unsupported("focusedWorkspace");
            }
        }
        QueryKind::Workspaces => {
            if params.workspace.is_some() {
                return unsupported("workspace");
            }
            if params.focused_workspace {
                return unsupported("focusedWorkspace");
            }
            if params.focused_screen && params.screen.is_some() {
                return conflict("focusedScreen", "screen");
            }
        }
        QueryKind::Windows => {
            if params.focused_screen && params.screen.is_some() {
                return conflict("focusedScreen", "screen");
            }
            if params.focused_screen && params.workspace.is_some() {
                return conflict("focusedScreen", "workspace");
            }
            if params.focused_screen && params.focused_workspace {
                return conflict("focusedScreen", "focusedWorkspace");
            }
            if params.focused_workspace && params.screen.is_some() {
                return conflict("focusedWorkspace", "screen");
            }
            if params.focused_workspace && params.workspace.is_some() {
                return conflict("focusedWorkspace", "workspace");
            }
        }
    }
    Ok(())
}

fn screen_name(screen: &ScreenState) -> String {
    screen.name.clone().unwrap_or_else(|| format!("screen-{}", screen.id.raw()))
}

/// Exact `main`/`primary`/`secondary` (every non-main display), else an exact
/// display name.
fn screen_matches(filter: &str, screen: &ScreenState) -> bool {
    match filter {
        "main" | "primary" => screen.is_primary,
        "secondary" => !screen.is_primary,
        name => screen_name(screen) == name,
    }
}

fn focused_workspace_index(snapshot: &TilingSnapshot) -> Option<usize> {
    snapshot
        .focus
        .workspace
        .and_then(|id| snapshot.workspaces.iter().position(|ws| ws.id == id))
        .or_else(|| snapshot.workspaces.iter().position(|ws| ws.focused))
}

fn screens(snapshot: &TilingSnapshot) -> Vec<ScreenDto> {
    snapshot
        .screens
        .iter()
        .filter_map(|screen| {
            let info = snapshot.display_layout.get(screen.id)?;
            Some(ScreenDto {
                id: screen.id.raw(),
                name: screen_name(screen),
                is_main: screen.is_primary,
                is_builtin: info.is_builtin,
                scale_factor: if info.scale.is_finite() {
                    info.scale
                } else {
                    return None;
                },
                frame: FrameDto::finite(
                    info.frame.x,
                    info.frame.y,
                    info.frame.width,
                    info.frame.height,
                )?,
                visible_frame: FrameDto::finite(
                    info.visible_frame.x,
                    info.visible_frame.y,
                    info.visible_frame.width,
                    info.visible_frame.height,
                )?,
            })
        })
        .collect()
}

fn workspaces(snapshot: &TilingSnapshot, params: &QueryParams) -> Vec<WorkspaceDto> {
    let focused_screen = if params.focused_screen {
        match focused_workspace_index(snapshot) {
            Some(index) => Some(snapshot.workspaces[index].screen),
            None => return Vec::new(),
        }
    } else {
        None
    };
    snapshot
        .workspaces
        .iter()
        .filter_map(|workspace| {
            let screen = snapshot.screens.iter().find(|screen| screen.id == workspace.screen);
            if let Some(filter) = &params.screen
                && !screen.is_some_and(|screen| screen_matches(filter, screen))
            {
                return None;
            }
            if let Some(id) = focused_screen
                && workspace.screen != id
            {
                return None;
            }
            Some(WorkspaceDto {
                id: workspace.id.0.to_string(),
                name: workspace.name.clone(),
                screen_name: screen
                    .map_or_else(|| format!("screen-{}", workspace.screen.raw()), screen_name),
                layout: workspace.layout.as_str().to_owned(),
                is_visible: workspace.visible,
                is_focused: workspace.focused,
                window_count: workspace.order.len(),
            })
        })
        .collect()
}

/// Tiles beyond the Grid cap float as overflow: effective floating includes
/// them even though the user never floated them.
fn grid_overflow(snapshot: &TilingSnapshot) -> BTreeSet<WindowInstance> {
    let mut overflow = BTreeSet::new();
    for workspace in &snapshot.workspaces {
        if workspace.layout != LayoutType::Grid {
            continue;
        }
        let eligible = workspace.order.iter().filter(|token| {
            snapshot.window(**token).is_some_and(|window| {
                let flags = window.flags();
                let ineligible = flags.inactive_tab
                    || flags.fullscreen
                    || flags.minimized
                    || flags.hidden
                    || window.desired.floating;
                !ineligible
            })
        });
        overflow.extend(eligible.skip(MAX_GRID_TILES).copied());
    }
    overflow
}

fn windows(snapshot: &TilingSnapshot, params: &QueryParams) -> Vec<WindowDto> {
    let focused = focused_workspace_index(snapshot);
    let focused_workspace = focused.map(|index| snapshot.workspaces[index].id);
    let focused_screen = focused.map(|index| snapshot.workspaces[index].screen);
    if (params.focused_workspace && focused_workspace.is_none())
        || (params.focused_screen && focused_screen.is_none())
    {
        return Vec::new();
    }
    let overflow = grid_overflow(snapshot);
    snapshot
        .windows
        .iter()
        .filter_map(|window| {
            let workspace = snapshot
                .workspaces
                .iter()
                .find(|workspace| Some(workspace.id) == window.desired.workspace)?;
            if params.workspace.as_deref().is_some_and(|name| workspace.name != name) {
                return None;
            }
            if params.focused_workspace && Some(workspace.id) != focused_workspace {
                return None;
            }
            if let Some(filter) = &params.screen {
                let screen =
                    snapshot.screens.iter().find(|screen| screen.id == workspace.screen)?;
                if !screen_matches(filter, screen) {
                    return None;
                }
            }
            if params.focused_screen && Some(workspace.screen) != focused_screen {
                return None;
            }
            // A suspended window (an inactive tab) is never a tile.
            if window.suspended {
                return None;
            }
            // Only a confirmed window with a numeric id and a finite frame is
            // listed; nothing is fabricated for an unread one.
            let id = window.cg_window_id?;
            let frame = FrameDto::from_rect(window.observed.frame?)?;
            let app = snapshot.apps.iter().find(|app| app.info.application == window.application);
            Some(WindowDto {
                id,
                pid: app.map_or(0, |app| app.info.pid),
                app_id: app.map(|app| app.info.bundle_id.clone()).unwrap_or_default(),
                app_name: app.map(|app| app.info.name.clone()).unwrap_or_default(),
                title: window.observed.title.clone(),
                workspace: workspace.name.clone(),
                frame,
                is_minimized: window.observed.minimized,
                is_fullscreen: window.observed.fullscreen,
                is_floating: window.desired.floating || overflow.contains(&window.token),
                is_focused: snapshot.focus.window == Some(window.token),
            })
        })
        .collect()
}

fn apps(snapshot: &TilingSnapshot) -> Vec<AppDto> {
    snapshot
        .apps
        .iter()
        .filter(|app| !is_system_bundle(&app.info.bundle_id) && !is_system_app_name(&app.info.name))
        .map(|app| AppDto {
            pid: app.info.pid,
            name: app.info.name.clone(),
            bundle_id: app.info.bundle_id.clone(),
            is_hidden: app.hidden,
        })
        .collect()
}

/// Rows of each window list in the status answer.
const STATUS_ROWS: usize = 200;

#[allow(
    clippy::too_many_lines,
    reason = "one linear mapping of the snapshot into the status rows"
)]
pub(crate) fn status(snapshot: &TilingSnapshot) -> StatusDto {
    let catalog =
        |application| snapshot.apps.iter().find(|app| app.info.application == application);
    let row = |failed: &FailedApp| {
        let app = catalog(failed.application);
        StatusAppDto {
            pid: app.map_or(0, |app| app.info.pid),
            name: app.map(|app| app.info.name.clone()).unwrap_or_default(),
            bundle_id: app.map(|app| app.info.bundle_id.clone()).unwrap_or_default(),
            reason: failed.reason.clone(),
            retries: failed.retries,
        }
    };
    StatusDto {
        phase: snapshot.phase.as_str().to_owned(),
        healthy: snapshot.health == Health::Healthy,
        faults: snapshot.faults.clone(),
        layout_active: snapshot.actions_allowed,
        config_revision: snapshot.config_revision,
        plan_sequence: snapshot.plan_sequence,
        apps: snapshot.apps.len(),
        tracked_apps: snapshot.apps.iter().filter(|app| app.tracked).count(),
        windows: snapshot.windows.iter().filter(|window| !window.suspended).count(),
        suspended_windows: snapshot.windows.iter().filter(|window| window.suspended).count(),
        failed_apps: snapshot.failed_apps.iter().map(row).collect(),
        truncated_apps: snapshot.truncated_apps.iter().map(row).collect(),
        unconfirmed_apps: snapshot.unconfirmed_apps,
        inflight_apps: snapshot.inflight_apps,
        no_frame: snapshot
            .no_frame
            .iter()
            .map(|(display, reason)| StatusNoFrameDto {
                screen: snapshot
                    .screens
                    .iter()
                    .find(|screen| screen.id == *display)
                    .map_or_else(|| format!("screen-{}", display.raw()), screen_name),
                reason: format!("{reason:?}"),
            })
            .collect(),
        hidden_apps: snapshot
            .hidden_apps
            .iter()
            .map(|owned| {
                let app = catalog(owned.application);
                StatusHiddenDto {
                    name: app.map(|app| app.info.name.clone()).unwrap_or_default(),
                    bundle_id: app.map(|app| app.info.bundle_id.clone()).unwrap_or_default(),
                    ownership: match owned.ownership {
                        Ownership::PendingIssued => "pending",
                        Ownership::PendingPossible => "pending-possible",
                        Ownership::Confirmed => "confirmed",
                        Ownership::Ambiguous => "ambiguous",
                    }
                    .to_owned(),
                    reason: owned.reason.clone(),
                }
            })
            .collect(),
        animation_fallbacks: snapshot
            .animation_fallbacks
            .iter()
            .map(|(entry, retry_in)| {
                let app = catalog(entry.application);
                StatusAnimationFallbackDto {
                    name: app.map(|app| app.info.name.clone()).unwrap_or_default(),
                    bundle_id: app.map(|app| app.info.bundle_id.clone()).unwrap_or_default(),
                    kind: entry.kind.as_str().to_owned(),
                    reason: entry.reason.to_string(),
                    retry_in_ms: retry_in
                        .map(|left| u64::try_from(left.as_millis()).unwrap_or(u64::MAX)),
                }
            })
            .collect(),
        last_intent: snapshot
            .last_intent
            .as_ref()
            .map(|record| format!("{:?} -> {:?}", record.intent, record.outcome)),
        tracked_windows: snapshot
            .windows
            .iter()
            .filter(|window| !window.suspended)
            .take(STATUS_ROWS)
            .map(|window| {
                let app = catalog(window.application);
                StatusWindowDto {
                    id: window.cg_window_id,
                    app_name: app.map(|app| app.info.name.clone()).unwrap_or_default(),
                    bundle_id: app.map(|app| app.info.bundle_id.clone()).unwrap_or_default(),
                    title: window.observed.title.clone(),
                    subrole: window.observed.subrole.clone(),
                    frame: window.observed.frame.map(|frame| {
                        format!("{}x{}@{},{}", frame.width, frame.height, frame.x, frame.y)
                    }),
                    workspace: window.desired.workspace.and_then(|id| {
                        snapshot.workspaces.iter().find(|ws| ws.id == id).map(|ws| ws.name.clone())
                    }),
                    floating: window.desired.floating,
                    focused: snapshot.focus.window == Some(window.token),
                }
            })
            .collect(),
        excluded_windows: snapshot
            .excluded
            .iter()
            .take(STATUS_ROWS)
            .map(|(token, why)| {
                let app = catalog(token.application());
                StatusExcludedDto {
                    app_name: app.map(|app| app.info.name.clone()).unwrap_or_default(),
                    bundle_id: app.map(|app| app.info.bundle_id.clone()).unwrap_or_default(),
                    reason: why.label().to_owned(),
                }
            })
            .collect(),
        frame_attempts: snapshot.diagnostics.frame_attempts,
        enumeration_failures: snapshot.diagnostics.enumeration_failures,
    }
}
