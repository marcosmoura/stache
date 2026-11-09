//! Exact-instance application hide and unhide.
//!
//! [`ApplicationObservation::hide_exact`] and [`ApplicationObservation::unhide_exact`]
//! are additive operations next to [`ApplicationObservation::activate_exact`],
//! [`ApplicationObservation::terminate`] and [`ApplicationObservation::open`];
//! those keep their signatures and behaviour. They hide or unhide exactly the
//! retained instance named by an [`ApplicationId`] of the current generation:
//! never by PID, bundle, name or path, never every application, and never a
//! second activation path. Focus and raise stay
//! [`ApplicationObservation::activate_exact`]; "unhide then focus" is two
//! separately authorized steps, so cancelling the second cannot undo the first.
//!
//! # Issuance ordering
//!
//! Identical to activation. The backend validates liveness and generation on
//! main, then calls [`super::IssuancePermit::issue`] at the exact native send.
//! Inside that closure (under the commit gate, immediate native call only) it
//! checks the caller's deadline and commits the [`VisibilityAuthorization`] with
//! one compare-and-swap. [`VisibilityAuthorization::cancel`] committed first:
//! nothing is sent. The send committed first: cancel reports
//! [`ActivationCancel::AlreadyIssued`]; a hide cannot be reversed by cancelling.
//!
//! A timeout before issuance abandons the queued job so it can never send
//! later ([`VisibilityOutcome::NotIssued`]); a timeout after issuance is
//! [`VisibilityOutcome::Uncertain`], never "definitely not applied". Execution
//! is on main through the host executor; the call never waits main -> worker ->
//! main.
//!
//! # Native binding
//!
//! `-[NSRunningApplication hide]` and `-[NSRunningApplication unhide]` (public
//! `NSRunningApplication.h`; each returns `BOOL`). `Requested` is the native
//! `true`, which means the request was sent, not that the application finished
//! hiding. The effect is observed through the lifecycle notifications.

use std::sync::Arc;
use std::time::Instant;

use super::activation::{ActivationAuthorization, ActivationCancel};
use super::{
    AcceptedActionToken, Ack, AckWait, ApplicationError, ApplicationId, ApplicationObservation,
    RequestCommit,
};

/// Which visibility change one request asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VisibilityOperation {
    /// `-[NSRunningApplication hide]`.
    Hide,
    /// `-[NSRunningApplication unhide]`.
    Unhide,
}

/// A narrow, typed, one-shot authorization for one exact-instance hide or
/// unhide. Same semantics and thread-safety as [`ActivationAuthorization`].
#[derive(Clone, Debug)]
pub struct VisibilityAuthorization {
    inner: ActivationAuthorization,
}

impl VisibilityAuthorization {
    /// Creates an armed authorization without a caller deadline.
    #[must_use]
    pub fn new(application: ApplicationId, accepted: AcceptedActionToken) -> Self {
        Self {
            inner: ActivationAuthorization::new(application, accepted),
        }
    }

    /// Creates an armed authorization that refuses to send after `deadline`.
    #[must_use]
    pub fn with_deadline(
        application: ApplicationId,
        accepted: AcceptedActionToken,
        deadline: Instant,
    ) -> Self {
        Self {
            inner: ActivationAuthorization::with_deadline(application, accepted, deadline),
        }
    }

    /// The exact application instance it authorizes.
    #[must_use]
    pub fn application(&self) -> ApplicationId { self.inner.application() }

    /// The accepted publication it was created under.
    #[must_use]
    pub fn accepted(&self) -> AcceptedActionToken { self.inner.accepted() }

    /// Cancels immediately. Idempotent, nonblocking and gate-free.
    #[must_use = "AlreadyIssued means the request cannot be undone"]
    pub fn cancel(&self) -> ActivationCancel { self.inner.cancel() }

    /// Whether it was cancelled before any send committed.
    #[must_use]
    pub fn is_cancelled(&self) -> bool { self.inner.is_cancelled() }

    /// Whether the send committed.
    #[must_use]
    pub fn is_issued(&self) -> bool { self.inner.is_issued() }

    /// Whether the caller's deadline (if any) has passed.
    #[must_use]
    pub fn is_expired(&self) -> bool { self.inner.is_expired() }

    /// Whether it names this exact request.
    #[must_use]
    pub fn matches(&self, application: ApplicationId, accepted: AcceptedActionToken) -> bool {
        self.inner.matches(application, accepted)
    }

    /// The one-shot commit, called by a backend inside the permit's send
    /// closure. Returns `true` exactly once and only while still armed.
    #[must_use]
    pub fn commit_send(&self) -> bool { self.inner.commit_send() }
}

/// The truth about one exact-instance hide or unhide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VisibilityOutcome {
    /// The native call returned `true`: the request was sent, not that the
    /// application finished changing visibility.
    Requested,
    /// The native call returned `false`. Not proof of no side effects.
    Refused,
    /// Nothing was sent: the instance terminated, was replaced (even by the same
    /// PID or bundle), belongs to another control or an older generation, or the
    /// accepted token or authorization does not name this request.
    Stale,
    /// The authorization was cancelled before the send committed.
    Cancelled,
    /// The caller's deadline passed before the send committed.
    ExpiredBeforeSend,
    /// The request provably never reached the send.
    NotIssued(ApplicationError),
    /// The send may have been issued but its result was not observed. Never
    /// `Refused` and never proof of no effect.
    Uncertain(ApplicationError),
}

impl VisibilityOutcome {
    /// Whether this outcome proves no hide or unhide was sent.
    #[must_use]
    pub const fn proves_no_send(&self) -> bool {
        matches!(
            self,
            Self::Stale | Self::Cancelled | Self::ExpiredBeforeSend | Self::NotIssued(_)
        )
    }
}

impl ApplicationObservation {
    /// Hides the exact retained instance named by `authorization`.
    ///
    /// See the module documentation for the issuance ordering. A fresh
    /// acknowledgement window applies to this one hop. The request is never
    /// retried and never retargets.
    pub fn hide_exact(
        &self,
        accepted: AcceptedActionToken,
        application: ApplicationId,
        authorization: &VisibilityAuthorization,
    ) -> VisibilityOutcome {
        self.visibility_exact(VisibilityOperation::Hide, accepted, application, authorization)
    }

    /// Unhides the exact retained instance named by `authorization`.
    ///
    /// Same contract as [`ApplicationObservation::hide_exact`]. Unhiding does
    /// not focus: follow it with a separately authorized
    /// [`ApplicationObservation::activate_exact`].
    pub fn unhide_exact(
        &self,
        accepted: AcceptedActionToken,
        application: ApplicationId,
        authorization: &VisibilityAuthorization,
    ) -> VisibilityOutcome {
        self.visibility_exact(VisibilityOperation::Unhide, accepted, application, authorization)
    }

    fn visibility_exact(
        &self,
        operation: VisibilityOperation,
        accepted: AcceptedActionToken,
        application: ApplicationId,
        authorization: &VisibilityAuthorization,
    ) -> VisibilityOutcome {
        if !self.owns(application) || !authorization.matches(application, accepted) {
            return VisibilityOutcome::Stale;
        }
        // Cheap early classification only; the commit decisions are the permit,
        // the deadline and the one-shot authorization inside the send.
        if authorization.is_cancelled() {
            return VisibilityOutcome::Cancelled;
        }
        if authorization.is_expired() {
            return VisibilityOutcome::ExpiredBeforeSend;
        }
        if !self.state.is_active_now(self.generation) {
            return VisibilityOutcome::Stale;
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
            job_ack.complete(backend.request_visibility(
                state.handle,
                application,
                operation,
                &job_authorization,
                permit,
            ));
        });
        if let Err(error) = self.state.executor.execute(job) {
            return VisibilityOutcome::NotIssued(ApplicationError::Dispatch(error.to_string()));
        }
        let uncertain_or_unissued = |error: ApplicationError| {
            if authorization.is_cancelled() || request.is_unissued() {
                VisibilityOutcome::NotIssued(error)
            } else {
                VisibilityOutcome::Uncertain(error)
            }
        };
        match ack.wait(self.state.budget) {
            AckWait::Ready(Ok(outcome)) => outcome,
            AckWait::Ready(Err(error)) => uncertain_or_unissued(error),
            AckWait::TimedOut | AckWait::Cancelled => {
                if request.abandon_confirmed() {
                    VisibilityOutcome::NotIssued(ApplicationError::TimedOut)
                } else {
                    uncertain_or_unissued(ApplicationError::TimedOut)
                }
            }
        }
    }
}
