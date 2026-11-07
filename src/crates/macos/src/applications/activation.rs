//! Exact retained-instance activation (`MenuAnywhere` MA-02-08).
//!
//! [`ApplicationObservation::activate_exact`] is a separately named, additive
//! operation next to [`ApplicationObservation::terminate`],
//! [`ApplicationObservation::terminate_gracefully`] and
//! [`ApplicationObservation::open`]; those keep their signatures and behaviour.
//! It activates exactly the retained instance named by an [`ApplicationId`] of
//! the current generation. It never resolves or opens by path, never activates
//! "whatever is frontmost", never keeps a second application map and never
//! retries.
//!
//! # Issuance ordering
//!
//! Issuance reuses [`super::IssuancePermit::issue`] at the actual native send.
//! Inside that closure (under the commit gate, immediate native call only) the
//! backend also checks the caller's deadline and commits the caller's
//! [`ActivationAuthorization`] with one compare-and-swap:
//!
//! - [`ActivationAuthorization::cancel`] committed first: nothing is sent and
//!   the outcome is [`ActivationOutcome::Cancelled`].
//! - The send committed first: [`ActivationAuthorization::cancel`] reports
//!   [`ActivationCancel::AlreadyIssued`]. Activation cannot be reversed; a
//!   caller can only suppress its own later, unissued steps.
//!
//! Cancelling never touches the commit gate, so it is safe from any thread.
//!
//! # Native binding
//!
//! The system backend calls `-[NSRunningApplication activateWithOptions:]` with
//! empty options. In the macOS SDK header (`NSRunningApplication.h`) this is the
//! non-deprecated public activation of the receiver; `activateFromApplication:
//! options:` is the cooperative form that needs another application to have
//! yielded activation, which does not apply here. `Requested` is the native
//! "request sent" `true`, never proof that focus changed.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

use super::{
    AcceptedActionToken, Ack, AckWait, ApplicationError, ApplicationId, ApplicationObservation,
    RequestCommit,
};

const ARMED: u8 = 0;
const CANCELLED: u8 = 1;
const COMMITTED: u8 = 2;

#[derive(Debug)]
struct ActivationInner {
    application: ApplicationId,
    accepted: AcceptedActionToken,
    deadline: Option<Instant>,
    state: AtomicU8,
}

/// A narrow, typed, one-shot authorization for one exact-instance activation.
///
/// The caller binds it to the exact captured `application` and the `accepted`
/// publication, optionally with its own `deadline`, and calls
/// [`ActivationAuthorization::cancel`] the instant its owning lifecycle (for
/// example a menu presentation) is abandoned. It is cheap to clone and
/// `Send + Sync`; every clone shares one commit state.
#[derive(Clone, Debug)]
pub struct ActivationAuthorization {
    inner: Arc<ActivationInner>,
}

/// The result of [`ActivationAuthorization::cancel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationCancel {
    /// The authorization is cancelled; no activation can be sent for it.
    Cancelled,
    /// The send committed first. Activation cannot be undone.
    AlreadyIssued,
}

impl ActivationAuthorization {
    /// Creates an armed authorization without a caller deadline.
    #[must_use]
    pub fn new(application: ApplicationId, accepted: AcceptedActionToken) -> Self {
        Self::build(application, accepted, None)
    }

    /// Creates an armed authorization that refuses to send after `deadline`.
    #[must_use]
    pub fn with_deadline(
        application: ApplicationId,
        accepted: AcceptedActionToken,
        deadline: Instant,
    ) -> Self {
        Self::build(application, accepted, Some(deadline))
    }

    fn build(
        application: ApplicationId,
        accepted: AcceptedActionToken,
        deadline: Option<Instant>,
    ) -> Self {
        Self {
            inner: Arc::new(ActivationInner {
                application,
                accepted,
                deadline,
                state: AtomicU8::new(ARMED),
            }),
        }
    }

    /// The exact application instance it authorizes.
    #[must_use]
    pub fn application(&self) -> ApplicationId { self.inner.application }

    /// The accepted publication it was created under.
    #[must_use]
    pub fn accepted(&self) -> AcceptedActionToken { self.inner.accepted }

    /// Cancels immediately. Idempotent, nonblocking and gate-free.
    #[must_use = "AlreadyIssued means the activation cannot be undone"]
    pub fn cancel(&self) -> ActivationCancel {
        match self.inner.state.compare_exchange(
            ARMED,
            CANCELLED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) | Err(CANCELLED) => ActivationCancel::Cancelled,
            Err(_) => ActivationCancel::AlreadyIssued,
        }
    }

    /// Whether the authorization was cancelled before any send committed.
    #[must_use]
    pub fn is_cancelled(&self) -> bool { self.inner.state.load(Ordering::SeqCst) == CANCELLED }

    /// Whether the send committed. The request may or may not have been accepted
    /// by the system; see [`ActivationOutcome`].
    #[must_use]
    pub fn is_issued(&self) -> bool { self.inner.state.load(Ordering::SeqCst) == COMMITTED }

    /// Whether the caller's deadline (if any) has passed.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.inner.deadline.is_some_and(|deadline| Instant::now() >= deadline)
    }

    /// Whether the authorization names this exact request.
    #[must_use]
    pub fn matches(&self, application: ApplicationId, accepted: AcceptedActionToken) -> bool {
        self.inner.application == application && self.inner.accepted == accepted
    }

    /// The one-shot commit, called by a backend inside the permit's send
    /// closure. Returns `true` exactly once and only while still armed.
    #[must_use]
    pub fn commit_send(&self) -> bool {
        self.inner
            .state
            .compare_exchange(ARMED, COMMITTED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

/// The truth about one exact-instance activation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivationOutcome {
    /// Activation returned `true`: the request was sent, not that focus moved.
    Requested,
    /// Activation returned `false`: macOS confirmed it refused the request. This
    /// is not proof of no side effects.
    Refused,
    /// Nothing was sent: the instance terminated, was replaced (even by the same
    /// PID or bundle), belongs to another control or an older generation, or the
    /// accepted token or authorization does not name this request.
    Stale,
    /// The authorization was cancelled before the send committed. Nothing was
    /// sent.
    Cancelled,
    /// The caller's deadline passed before the send committed. Nothing was sent.
    ExpiredBeforeSend,
    /// The request provably never reached the send: dispatch rejection, a
    /// pre-send failure or an acknowledgement window abandoned before issuance.
    NotIssued(ApplicationError),
    /// The send may have been issued but its result was not observed (the
    /// acknowledgement timed out or was lost after issuance). Never `Refused`
    /// and never proof of no effect.
    Uncertain(ApplicationError),
}

impl ActivationOutcome {
    /// Whether this outcome proves no activation was sent for this request.
    #[must_use]
    pub const fn proves_no_send(&self) -> bool {
        matches!(
            self,
            Self::Stale | Self::Cancelled | Self::ExpiredBeforeSend | Self::NotIssued(_)
        )
    }
}

impl ApplicationObservation {
    /// Activates the exact retained instance named by `authorization`.
    ///
    /// The backend resolves the retained [`ApplicationId`] on main and validates
    /// liveness and owner generation immediately before the permit, then issues
    /// through [`super::IssuancePermit::issue`] with the caller's deadline and
    /// authorization committed at the send; see the module documentation. A
    /// fresh acknowledgement window applies to this one hop. Activation is never
    /// retried and never retargets, and a late result of a timed-out request is
    /// never delivered.
    ///
    /// [`ActivationOutcome::Requested`] is native acceptance, not proven focus;
    /// a timeout after issuance is [`ActivationOutcome::Uncertain`], while a
    /// timeout the request lost before issuance is
    /// [`ActivationOutcome::NotIssued`] and the queued job can never send.
    pub fn activate_exact(
        &self,
        accepted: AcceptedActionToken,
        application: ApplicationId,
        authorization: &ActivationAuthorization,
    ) -> ActivationOutcome {
        if !self.owns(application) || !authorization.matches(application, accepted) {
            return ActivationOutcome::Stale;
        }
        // Cheap early classification only; the commit decisions are the permit,
        // the deadline and the one-shot authorization inside the send.
        if authorization.is_cancelled() {
            return ActivationOutcome::Cancelled;
        }
        if authorization.is_expired() {
            return ActivationOutcome::ExpiredBeforeSend;
        }
        if !self.state.is_active_now(self.generation) {
            return ActivationOutcome::Stale;
        }
        let request = Arc::new(RequestCommit::new(Instant::now() + self.state.budget));
        let ack = Arc::new(Ack::new());
        let job_ack = Arc::clone(&ack);
        let job_request = Arc::clone(&request);
        let state = Arc::clone(&self.state);
        let generation = self.generation;
        let job_authorization = authorization.clone();
        let job = Box::new(move || {
            let permit = state.issuance(generation, accepted, &job_request, None);
            let backend = Arc::clone(&state.backend);
            job_ack.complete(backend.request_activate(
                state.handle,
                application,
                &job_authorization,
                permit,
            ));
        });
        if let Err(error) = self.state.executor.execute(job) {
            return ActivationOutcome::NotIssued(ApplicationError::Dispatch(error.to_string()));
        }
        let uncertain_or_unissued = |error: ApplicationError| {
            if authorization.is_cancelled() || request.is_unissued() {
                ActivationOutcome::NotIssued(error)
            } else {
                ActivationOutcome::Uncertain(error)
            }
        };
        match ack.wait(self.state.budget) {
            AckWait::Ready(Ok(outcome)) => outcome,
            AckWait::Ready(Err(error)) => uncertain_or_unissued(error),
            AckWait::TimedOut | AckWait::Cancelled => {
                if request.abandon_confirmed() {
                    ActivationOutcome::NotIssued(ApplicationError::TimedOut)
                } else {
                    uncertain_or_unissued(ApplicationError::TimedOut)
                }
            }
        }
    }
}
