//! Native-owner control contract: constants, requests, acknowledgements, the
//! bounded request queue and the injectable clock.
//!
//! Slice B (the macOS owner thread) implements [`InputBackend`] with one
//! dedicated run-loop thread that drains a [`ControlQueue`], rechecks
//! [`ControlRequest::is_cancelled`] before every native mutation, performs the
//! action and calls [`ControlRequest::acknowledge`]. It calls the service's
//! `dispatch` synchronously from the tap callback.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Time allowed for one install/enable/remove acknowledgement. Equal to
/// `stache-macos::ACKNOWLEDGEMENT_BUDGET`; slice B adds the equality assertion.
pub const ACKNOWLEDGEMENT_BUDGET: Duration = Duration::from_secs(2);

/// Granularity of cancellation-aware acknowledgement waits. Equal to
/// `stache-macos::CANCELLATION_QUANTUM`; slice B adds the equality assertion.
pub const CANCELLATION_QUANTUM: Duration = Duration::from_millis(10);

/// Hard bound of the native owner control-request queue.
pub const CONTROL_QUEUE_CAPACITY: usize = 256;

/// Soft per-callback time target. It is measured after a callback returns and
/// cannot preempt a callback.
pub const CALLBACK_SOFT_TARGET: Duration = Duration::from_millis(1);

/// Bound on explicit cleanup or re-enable retries per failure episode.
pub const MAX_RETRY_ATTEMPTS: u32 = 3;

/// Which native tap a request or event concerns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TapKind {
    /// The single suppressing head-insert tap.
    Suppressing,
    /// The single listen-only observation tap.
    ListenOnly,
}

/// A native owner action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    /// Create the tap in a disabled state.
    Install,
    /// Enable (or re-enable) the tap.
    Enable,
    /// Remove the tap and every partial resource.
    Remove,
}

/// A native failure reported by the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeError(pub String);

/// Why a request was not accepted into the owner queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRejected {
    /// The queue holds [`CONTROL_QUEUE_CAPACITY`] requests.
    QueueFull,
    /// The owner is gone.
    Closed,
}

impl fmt::Display for ControlRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::QueueFull => "native control queue is full",
            Self::Closed => "native owner is closed",
        })
    }
}

impl std::error::Error for ControlRejected {}

/// Why an action did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureCause {
    /// No acknowledgement inside [`ACKNOWLEDGEMENT_BUDGET`].
    TimedOut,
    /// The owner queue refused the request.
    Rejected(ControlRejected),
    /// The owner reported a native failure.
    Failed(String),
    /// The waiting caller was cancelled.
    Cancelled,
}

/// A one-shot signal a waiter blocks on.
#[derive(Default)]
pub struct AckSignal {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl AckSignal {
    pub(crate) fn set(&self) {
        *self.flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cv.notify_all();
    }

    /// Whether the signal fired.
    pub fn is_set(&self) -> bool { *self.flag.lock().unwrap_or_else(PoisonError::into_inner) }

    /// Blocks until the signal fires or `timeout` passes (real time).
    pub fn wait(&self, timeout: Duration) {
        let guard = self.flag.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = self.cv.wait_timeout_while(guard, timeout, |set| !*set);
    }
}

/// The injectable time source for every input timing decision.
pub trait InputClock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Instant;

    /// Waits until `signal` fires or `timeout` elapses on this clock.
    fn wait_for(&self, signal: &AckSignal, timeout: Duration);
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl InputClock for SystemClock {
    fn now(&self) -> Instant { Instant::now() }

    fn wait_for(&self, signal: &AckSignal, timeout: Duration) { signal.wait(timeout); }
}

type Continuation = Box<dyn FnOnce(Result<(), NativeError>) + Send>;

#[derive(Default)]
struct TicketState {
    outcome: Option<Result<(), NativeError>>,
    cancelled: bool,
    continuation: Option<Continuation>,
}

/// Shared state between a waiting service call and the owner.
pub(crate) struct Ticket {
    state: Mutex<TicketState>,
    pub(crate) signal: AckSignal,
}

impl Ticket {
    pub(crate) fn new(continuation: Option<Continuation>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(TicketState {
                continuation,
                ..TicketState::default()
            }),
            signal: AckSignal::default(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TicketState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn take_outcome(&self) -> Option<Result<(), NativeError>> {
        self.lock().outcome.take()
    }

    /// Stops waiting: returns an outcome that raced in. When `revoke` is set the
    /// ticket is also cancelled so a queued owner never acts on it; removals are
    /// never revoked because eventual owner cleanup must still run.
    pub(crate) fn finish_waiting(&self, revoke: bool) -> Option<Result<(), NativeError>> {
        let mut state = self.lock();
        let outcome = state.outcome.take();
        if outcome.is_none() {
            state.continuation = None;
            // A removal stays open so eventual owner cleanup still runs; without
            // a continuation its late acknowledgement is a no-op.
            if revoke {
                state.cancelled = true;
            }
        }
        outcome
    }

    fn is_cancelled(&self) -> bool { self.lock().cancelled }

    fn complete(&self, result: Result<(), NativeError>) {
        let mut state = self.lock();
        if state.cancelled {
            return;
        }
        if let Some(continuation) = state.continuation.take() {
            drop(state);
            continuation(result);
        } else {
            state.outcome = Some(result);
            drop(state);
            self.signal.set();
        }
    }
}

/// One native owner action to perform.
pub struct ControlRequest {
    kind: TapKind,
    action: ControlAction,
    generation: u64,
    ticket: Arc<Ticket>,
}

impl ControlRequest {
    pub(crate) const fn new(
        kind: TapKind,
        action: ControlAction,
        generation: u64,
        ticket: Arc<Ticket>,
    ) -> Self {
        Self {
            kind,
            action,
            generation,
            ticket,
        }
    }

    /// The tap this request targets.
    #[must_use]
    pub const fn kind(&self) -> TapKind { self.kind }

    /// The action to perform.
    #[must_use]
    pub const fn action(&self) -> ControlAction { self.action }

    /// The tap generation the action belongs to.
    #[must_use]
    pub const fn generation(&self) -> u64 { self.generation }

    /// Whether the caller cancelled or gave up. The owner must check this
    /// immediately before every native mutation: a cancelled queued install
    /// must never install late. A [`ControlAction::Remove`] is never cancelled:
    /// a timed-out removal stays queued for eventual owner cleanup.
    #[must_use]
    pub fn is_cancelled(&self) -> bool { self.ticket.is_cancelled() }

    /// Reports the outcome. A late acknowledgement for a cancelled request is
    /// ignored by the service.
    pub fn acknowledge(self, result: Result<(), NativeError>) { self.ticket.complete(result); }
}

impl fmt::Debug for ControlRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlRequest")
            .field("kind", &self.kind)
            .field("action", &self.action)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// The safe contract a native owner implements.
///
/// `submit` must never block: it either queues the request for the single owner
/// thread or rejects it. No native pointer crosses this trait.
pub trait InputBackend: Send + Sync {
    /// Queues one request for the owner.
    ///
    /// # Errors
    /// Returns [`ControlRejected`] when the queue is full or the owner is gone.
    fn submit(&self, request: ControlRequest) -> Result<(), ControlRejected>;
}

/// The bounded owner control queue shared by backend implementations.
pub struct ControlQueue {
    items: Mutex<VecDeque<ControlRequest>>,
    ready: Condvar,
    capacity: usize,
}

impl ControlQueue {
    /// A queue with the hard [`CONTROL_QUEUE_CAPACITY`].
    #[must_use]
    pub fn new() -> Self { Self::with_capacity(CONTROL_QUEUE_CAPACITY) }

    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            items: Mutex::new(VecDeque::with_capacity(capacity)),
            ready: Condvar::new(),
            capacity,
        }
    }

    /// Pushes without blocking.
    ///
    /// # Errors
    /// Returns [`ControlRejected::QueueFull`] when full; nothing is evicted.
    pub fn try_push(&self, request: ControlRequest) -> Result<(), ControlRejected> {
        let mut items = self.items.lock().unwrap_or_else(PoisonError::into_inner);
        if items.len() >= self.capacity {
            return Err(ControlRejected::QueueFull);
        }
        items.push_back(request);
        drop(items);
        self.ready.notify_one();
        Ok(())
    }

    /// Pops the oldest request without waiting.
    #[must_use]
    pub fn try_pop(&self) -> Option<ControlRequest> {
        self.items.lock().unwrap_or_else(PoisonError::into_inner).pop_front()
    }

    /// Pops the oldest request, waiting at most `timeout` (owner thread only).
    #[must_use]
    pub fn pop_timeout(&self, timeout: Duration) -> Option<ControlRequest> {
        let guard = self.items.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut guard, _) = self
            .ready
            .wait_timeout_while(guard, timeout, |items| items.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
        guard.pop_front()
    }

    /// Queued requests.
    #[must_use]
    pub fn len(&self) -> usize { self.items.lock().unwrap_or_else(PoisonError::into_inner).len() }

    /// Whether the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool { self.len() == 0 }
}

impl Default for ControlQueue {
    fn default() -> Self { Self::new() }
}
