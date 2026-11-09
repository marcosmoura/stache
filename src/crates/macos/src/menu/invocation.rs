//! Exact-instance invocation of a selected menu action.
//!
//! ```text
//! selection (main callback) ── lease on the AX owner, ticket created
//! invoke (worker thread, never main):
//!   recheck ─ activate_exact(captured ApplicationId) ─ recheck ─ press on owner
//!   fresh 2 s window per hop; cancellation between hops suppresses what is
//!   not yet issued and reports what already was
//! ```
//!
//! # Guarantees
//!
//! - **Exact target.** Activation targets the captured [`ApplicationId`] through
//!   the shared applications adapter, never whatever is frontmost; PID alone is
//!   never identity, so a relaunched process with the same PID is stale.
//! - **At most once.** Activation uses its [`ActivationAuthorization`]
//!   (one-shot commit at the permit boundary). Press uses the equivalent
//!   [`PressGate`], committed on the AX owner immediately before `AXPress`.
//!   Neither is retried, ever.
//! - **Stop is immediate.** [`InvocationTicket::cancel`] is two compare-and-swaps
//!   and never waits, so it returns while a press holds the owner thread.
//!   Already issued sends cannot be undone; the report says which were issued.
//! - **Honest outcomes.** `Pressed` is AX success, not proof of a visible effect.
//!   `kAXErrorCannotComplete` may follow an action that ran, so it is
//!   [`MenuInvocationOutcome::Uncertain`].

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::ax::AxError;
use super::owner::{AxOwner, Lease, PressRequest};
use super::reader::MenuClock;
use super::snapshot::ActionToken;
use crate::applications::{
    AcceptedActionToken, ActivationAuthorization, ActivationCancel, ActivationOutcome,
    ApplicationId, ApplicationObservation,
};
pub use crate::ax::PRESS_TIMEOUT;

/// What the invoker asks about a presentation at every recheck.
pub trait InvocationAuthority: Send + Sync {
    /// Whether the presentation is still live and its generation and captured
    /// revision still authorized (an opaque, caller-supplied decision).
    fn is_current(&self) -> bool;
    /// Whether `token` belongs to this presentation.
    fn contains(&self, token: ActionToken) -> bool;
}

const ARMED: u8 = 0;
const CANCELLED: u8 = 1;
const COMMITTED: u8 = 2;

/// The result of [`PressGate::cancel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressCancel {
    /// No press can be sent for this gate.
    Cancelled,
    /// The send committed first and cannot be undone.
    AlreadyIssued,
}

/// A one-shot issuance gate for `AXPress`, analogous to
/// [`ActivationAuthorization::commit_send`]. Cheap to clone; `Send + Sync`.
#[derive(Debug, Clone, Default)]
pub struct PressGate {
    state: Arc<AtomicU8>,
}

impl PressGate {
    /// An armed gate.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(ARMED)),
        }
    }

    /// Cancels without waiting on anything.
    #[must_use = "AlreadyIssued means the press cannot be undone"]
    pub fn cancel(&self) -> PressCancel {
        match self
            .state
            .compare_exchange(ARMED, CANCELLED, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) | Err(CANCELLED) => PressCancel::Cancelled,
            Err(_) => PressCancel::AlreadyIssued,
        }
    }

    /// The one-shot commit, called by the owner immediately before the send.
    #[must_use]
    pub fn commit_send(&self) -> bool {
        self.state
            .compare_exchange(ARMED, COMMITTED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Whether the gate was cancelled before any send committed.
    #[must_use]
    pub fn is_cancelled(&self) -> bool { self.state.load(Ordering::SeqCst) == CANCELLED }

    /// Whether the press send committed.
    #[must_use]
    pub fn is_issued(&self) -> bool { self.state.load(Ordering::SeqCst) == COMMITTED }
}

/// The AX owner's answer to a press request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressOutcome {
    /// `AXPress` returned success (not proof of a visible effect).
    Pressed,
    /// The token's element is unknown or already released. Nothing was sent.
    Stale,
    /// The gate was cancelled first. Nothing was sent.
    Cancelled,
    /// The deadline passed before the send. Nothing was sent.
    ExpiredBeforeSend,
    /// The element lacks the action or is invalid. Nothing took effect.
    Unsupported(AxError),
    /// Accessibility is not granted.
    PermissionDenied,
    /// The press timeout could not be set. Nothing was sent.
    TimeoutSetFailed(AxError),
    /// `kAXErrorCannotComplete`: the action may have run. Never retried.
    Uncertain(AxError),
    /// A definitive failure code after the send.
    Failed(AxError),
    /// The owner panicked while handling the request.
    OwnerPanic,
}

/// Which hop an uncertain outcome belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UncertainStage {
    /// The activation send was issued but its result was lost or late.
    Activation,
    /// The press send was issued but its result was lost, late or
    /// `CannotComplete`.
    Press,
}

/// The typed result of one invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuInvocationOutcome {
    /// `AXPress` returned success.
    Pressed,
    /// The token, presentation, generation, revision or target instance is no
    /// longer valid.
    Stale,
    /// Cancelled before the press was issued.
    Cancelled,
    /// A deadline passed before the next send.
    ExpiredBeforeSend,
    /// The element lacks the action or is invalid.
    Unsupported(AxError),
    /// Accessibility is not granted.
    PermissionDenied,
    /// The press timeout could not be set; no press was sent.
    TimeoutSetFailed(AxError),
    /// A definitive AX failure after the press was sent.
    Failed(AxError),
    /// Activation returned `false`; no press was attempted.
    ActivationRefused,
    /// A hop could not be queued or answered before it was issued.
    DispatchFailure,
    /// An issued send has an unknown result. Never retried.
    Uncertain(UncertainStage),
}

/// An invocation outcome plus what was irreversibly issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuInvocationReport {
    /// What happened.
    pub outcome: MenuInvocationOutcome,
    /// Whether the activation send committed (it cannot be undone).
    pub activation_issued: bool,
    /// Whether the press send committed.
    pub press_issued: bool,
}

/// What [`InvocationTicket::cancel`] could not prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CancelReport {
    /// The activation send had already committed.
    pub activation_issued: bool,
    /// The press send had already committed.
    pub press_issued: bool,
}

struct TicketInner {
    token: ActionToken,
    application: ApplicationId,
    accepted: AcceptedActionToken,
    gate: PressGate,
    cancelled: AtomicBool,
    activation: Mutex<Option<ActivationAuthorization>>,
    lease: Mutex<Option<Lease>>,
}

/// One selected action: its lease, authorizations and cancellation. Cloning
/// shares the same ticket.
#[derive(Clone)]
pub struct InvocationTicket {
    inner: Arc<TicketInner>,
}

impl InvocationTicket {
    /// Creates a ticket holding `lease` until the press resolves or the last
    /// clone drops.
    #[must_use]
    pub fn new(application: ApplicationId, accepted: AcceptedActionToken, lease: Lease) -> Self {
        Self {
            inner: Arc::new(TicketInner {
                token: lease.token(),
                application,
                accepted,
                gate: PressGate::new(),
                cancelled: AtomicBool::new(false),
                activation: Mutex::new(None),
                lease: Mutex::new(Some(lease)),
            }),
        }
    }

    /// The selected action.
    #[must_use]
    pub fn token(&self) -> ActionToken { self.inner.token }

    /// Whether the ticket was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool { self.inner.cancelled.load(Ordering::SeqCst) }

    /// Cancels immediately, without waiting on any gate or thread. Whatever was
    /// already issued is reported and stays issued.
    #[must_use = "the report says which sends could not be prevented"]
    pub fn cancel(&self) -> CancelReport {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        let press_issued = self.inner.gate.cancel() == PressCancel::AlreadyIssued;
        let activation_issued = self
            .inner
            .activation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|authorization| authorization.cancel() == ActivationCancel::AlreadyIssued);
        CancelReport {
            activation_issued,
            press_issued,
        }
    }

    fn take_lease(&self) -> Option<Lease> {
        self.inner.lease.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    fn activation_issued(&self) -> bool {
        self.inner
            .activation
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(ActivationAuthorization::is_issued)
    }
}

/// Everything [`invoke`] depends on.
pub struct InvocationContext<'a> {
    /// The shared applications adapter (exact activation and liveness).
    pub observation: &'a ApplicationObservation,
    /// The AX owner (press).
    pub owner: &'a AxOwner,
    /// The monotonic clock.
    pub clock: &'a dyn MenuClock,
    /// The presentation's authority.
    pub authority: &'a dyn InvocationAuthority,
    /// The acknowledgement window of each hop.
    pub budget: Duration,
}

/// Activates the captured instance, then presses the selected element.
///
/// Blocks the calling thread (bounded by `budget` per hop). Never call it from
/// the main thread or from a `Cocoa` callback.
#[must_use]
pub fn invoke(
    context: &InvocationContext<'_>,
    ticket: &InvocationTicket,
    deadline: Instant,
) -> MenuInvocationReport {
    let outcome = run(context, ticket, deadline);
    // Whatever happened, the lease must not outlive the invocation.
    drop(ticket.take_lease());
    MenuInvocationReport {
        outcome,
        activation_issued: ticket.activation_issued(),
        press_issued: ticket.inner.gate.is_issued(),
    }
}

fn recheck(
    context: &InvocationContext<'_>,
    ticket: &InvocationTicket,
    deadline: Instant,
) -> Option<MenuInvocationOutcome> {
    if ticket.is_cancelled() {
        return Some(MenuInvocationOutcome::Cancelled);
    }
    if !context.authority.contains(ticket.inner.token) || !context.authority.is_current() {
        return Some(MenuInvocationOutcome::Stale);
    }
    if context.clock.now() >= deadline {
        return Some(MenuInvocationOutcome::ExpiredBeforeSend);
    }
    match context.observation.is_running(ticket.inner.application) {
        Ok(true) => None,
        Ok(false) => Some(MenuInvocationOutcome::Stale),
        Err(_) => Some(MenuInvocationOutcome::DispatchFailure),
    }
}

fn run(
    context: &InvocationContext<'_>,
    ticket: &InvocationTicket,
    deadline: Instant,
) -> MenuInvocationOutcome {
    if let Some(stop) = recheck(context, ticket, deadline) {
        return stop;
    }
    // Hop 1: exact-instance activation under its one-shot authorization.
    let authorization = ActivationAuthorization::with_deadline(
        ticket.inner.application,
        ticket.inner.accepted,
        deadline,
    );
    *ticket.inner.activation.lock().unwrap_or_else(PoisonError::into_inner) =
        Some(authorization.clone());
    if ticket.is_cancelled() {
        // A cancel that raced the publication above could not see it.
        let _ = authorization.cancel();
    }
    match context.observation.activate_exact(
        ticket.inner.accepted,
        ticket.inner.application,
        &authorization,
    ) {
        ActivationOutcome::Requested => {}
        ActivationOutcome::Refused => return MenuInvocationOutcome::ActivationRefused,
        ActivationOutcome::Stale => return MenuInvocationOutcome::Stale,
        ActivationOutcome::Cancelled => return MenuInvocationOutcome::Cancelled,
        ActivationOutcome::ExpiredBeforeSend => return MenuInvocationOutcome::ExpiredBeforeSend,
        ActivationOutcome::NotIssued(_) => return MenuInvocationOutcome::DispatchFailure,
        ActivationOutcome::Uncertain(_) => {
            return MenuInvocationOutcome::Uncertain(UncertainStage::Activation);
        }
    }
    // Cancellation between activation and press suppresses the press only.
    if let Some(stop) = recheck(context, ticket, deadline) {
        return stop;
    }
    press(context, ticket, deadline)
}

/// Hop 2: the press, with a fresh acknowledgement window.
fn press(
    context: &InvocationContext<'_>,
    ticket: &InvocationTicket,
    deadline: Instant,
) -> MenuInvocationOutcome {
    let Some(lease) = ticket.take_lease() else {
        return MenuInvocationOutcome::Stale;
    };
    let (tx, rx) = mpsc::sync_channel(1);
    let request = PressRequest {
        token: ticket.inner.token,
        deadline,
        gate: ticket.inner.gate.clone(),
        lease,
    };
    let queued = context.owner.submit_press(request, move |outcome| {
        let _ = tx.send(outcome);
    });
    if queued.is_err() {
        // The request, with its gate, was dropped uncalled: nothing can press.
        return MenuInvocationOutcome::DispatchFailure;
    }
    match rx.recv_timeout(context.budget) {
        Ok(outcome) => map_press(outcome),
        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
            // Close the gate so a queued press can never send later; if the
            // send already committed, the result is simply unknown.
            match ticket.inner.gate.cancel() {
                PressCancel::Cancelled => MenuInvocationOutcome::DispatchFailure,
                PressCancel::AlreadyIssued => {
                    MenuInvocationOutcome::Uncertain(UncertainStage::Press)
                }
            }
        }
    }
}

const fn map_press(outcome: PressOutcome) -> MenuInvocationOutcome {
    match outcome {
        PressOutcome::Pressed => MenuInvocationOutcome::Pressed,
        PressOutcome::Stale => MenuInvocationOutcome::Stale,
        PressOutcome::Cancelled => MenuInvocationOutcome::Cancelled,
        PressOutcome::ExpiredBeforeSend => MenuInvocationOutcome::ExpiredBeforeSend,
        PressOutcome::Unsupported(error) => MenuInvocationOutcome::Unsupported(error),
        PressOutcome::PermissionDenied => MenuInvocationOutcome::PermissionDenied,
        PressOutcome::TimeoutSetFailed(error) => MenuInvocationOutcome::TimeoutSetFailed(error),
        PressOutcome::Uncertain(_) => MenuInvocationOutcome::Uncertain(UncertainStage::Press),
        PressOutcome::Failed(error) => MenuInvocationOutcome::Failed(error),
        PressOutcome::OwnerPanic => MenuInvocationOutcome::DispatchFailure,
    }
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<InvocationTicket>();
    assert_send_sync::<PressGate>();
    assert_send_sync::<MenuInvocationReport>();
};
