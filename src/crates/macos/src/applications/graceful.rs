//! Graceful (`terminate`) requests with per-press authorization.
//!
//! [`ApplicationObservation::terminate_gracefully`] is a separately named
//! operation next to the force-terminating
//! [`ApplicationObservation::terminate`], which keeps its signature and
//! behaviour. The graceful path calls only `-[NSRunningApplication terminate]`:
//! there is no retry, no escalation and no `forceTerminate` fallback.
//!
//! # Issuance ordering
//!
//! Issuance reuses [`super::IssuancePermit::issue`] at the actual send. Inside that
//! closure (under the commit gate, immediate native call only) the backend also
//! commits the caller's [`PressAuthorization`] with one compare-and-swap:
//!
//! - [`PressAuthorization::cancel`] committed first: the CAS in the send fails,
//!   nothing is sent and the outcome is [`GracefulOutcome::Cancelled`].
//! - The send committed first: [`PressAuthorization::cancel`] reports
//!   [`PressCancel::AlreadyIssued`] and cannot undo it.
//!
//! The authorization is revalidated at that boundary, not only when the request
//! was enqueued. Cancelling never touches the commit gate, so it is safe from
//! the input path.
//!
//! The main exact-identity / current-frontmost preflight runs immediately
//! before the permit. See the limitation in [`super::frontmost`]: it cannot
//! close the interval between an OS focus change and the send itself.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

use super::frontmost::PreflightRejection;
use super::{
    AcceptedActionToken, Ack, AckWait, ApplicationError, ApplicationId, ApplicationObservation,
    RequestCommit,
};
use crate::hold_hud::PressId;

const ARMED: u8 = 0;
const CANCELLED: u8 = 1;
const COMMITTED: u8 = 2;

#[derive(Debug)]
struct PressInner {
    press: PressId,
    application: ApplicationId,
    accepted: AcceptedActionToken,
    state: AtomicU8,
}

/// A narrow, typed, one-shot authorization for the graceful quit of one press.
///
/// Part of a press lifecycle: the runtime creates it only after the matching HUD
/// show acknowledgement for `press`, binds it to the exact captured
/// `application` and the `accepted` publication, and calls
/// [`PressAuthorization::cancel`] the instant the press is released, cancelled,
/// reconfigured or stopped. It is cheap to clone and `Send + Sync`; every clone
/// shares one commit state.
#[derive(Clone, Debug)]
pub struct PressAuthorization {
    inner: Arc<PressInner>,
}

/// The result of [`PressAuthorization::cancel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressCancel {
    /// The authorization is cancelled; no graceful request can be sent for it.
    Cancelled,
    /// The send committed first. It cannot be undone.
    AlreadyIssued,
}

impl PressAuthorization {
    /// Creates an armed authorization.
    #[must_use]
    pub fn new(press: PressId, application: ApplicationId, accepted: AcceptedActionToken) -> Self {
        Self {
            inner: Arc::new(PressInner {
                press,
                application,
                accepted,
                state: AtomicU8::new(ARMED),
            }),
        }
    }

    /// The press this authorization belongs to.
    #[must_use]
    pub fn press(&self) -> PressId { self.inner.press }

    /// The exact application instance it authorizes.
    #[must_use]
    pub fn application(&self) -> ApplicationId { self.inner.application }

    /// The accepted publication it was created under.
    #[must_use]
    pub fn accepted(&self) -> AcceptedActionToken { self.inner.accepted }

    /// Cancels immediately. Idempotent, nonblocking and gate-free.
    #[must_use = "AlreadyIssued means the send cannot be undone"]
    pub fn cancel(&self) -> PressCancel {
        match self.inner.state.compare_exchange(
            ARMED,
            CANCELLED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) | Err(CANCELLED) => PressCancel::Cancelled,
            Err(_) => PressCancel::AlreadyIssued,
        }
    }

    /// Whether the authorization was cancelled before any send committed.
    #[must_use]
    pub fn is_cancelled(&self) -> bool { self.inner.state.load(Ordering::SeqCst) == CANCELLED }

    /// Whether the send committed. The request may or may not have been accepted
    /// by the system; see [`GracefulOutcome`].
    #[must_use]
    pub fn is_issued(&self) -> bool { self.inner.state.load(Ordering::SeqCst) == COMMITTED }

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

/// The truth about one graceful request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GracefulOutcome {
    /// `terminate` returned `true`: the request was accepted, not that the
    /// process exited.
    Requested,
    /// `terminate` returned `false`: macOS confirmed it refused the request.
    Refused,
    /// The exact instance is not running (or belongs to another control).
    /// Nothing was sent.
    NotRunning,
    /// The accepted token or generation was stale, or the authorization does not
    /// name this request. Nothing was sent.
    Stale,
    /// The main preflight refused; nothing was sent and nothing was retargeted.
    Rejected(PreflightRejection),
    /// The press authorization was cancelled before the send committed. Nothing
    /// was sent.
    Cancelled,
    /// The request provably never reached the send: dispatch rejection, a
    /// pre-send failure or a deadline abandoned before issuance.
    NotIssued(ApplicationError),
    /// The send may have been issued but its result was not observed (the
    /// acknowledgement timed out or the result was lost after issuance). This
    /// is never `Refused` and never proof of no effect.
    Uncertain(ApplicationError),
}

impl GracefulOutcome {
    /// Whether this outcome proves no `terminate` was sent for this request.
    #[must_use]
    pub const fn proves_no_send(&self) -> bool {
        matches!(
            self,
            Self::NotRunning
                | Self::Stale
                | Self::Rejected(_)
                | Self::Cancelled
                | Self::NotIssued(_)
        )
    }
}

impl ApplicationObservation {
    /// Requests graceful termination (`-[NSRunningApplication terminate]`) of
    /// the exact instance named by `authorization`.
    ///
    /// Never calls `forceTerminate`, never retries and never falls back. The
    /// backend runs the main preflight (exact identity running, current
    /// frontmost, not Finder, not this process) immediately before issuance and
    /// then issues through [`super::IssuancePermit::issue`] with the press
    /// authorization committed at the send; see the module documentation for
    /// the cancel/send ordering. A late result of a timed-out request is never
    /// delivered and never authorizes a successor action.
    ///
    /// The result is truthful: [`GracefulOutcome::Requested`] is acceptance, not
    /// exit; [`GracefulOutcome::Refused`] is a confirmed `false` return; a
    /// timeout after issuance is [`GracefulOutcome::Uncertain`], while a
    /// timeout the request lost before issuance is
    /// [`GracefulOutcome::NotIssued`] and the queued job can never send.
    pub fn terminate_gracefully(
        &self,
        accepted: AcceptedActionToken,
        application: ApplicationId,
        authorization: &PressAuthorization,
    ) -> GracefulOutcome {
        if !self.owns(application) {
            return GracefulOutcome::NotRunning;
        }
        if !authorization.matches(application, accepted) {
            return GracefulOutcome::Stale;
        }
        // Cheap early classification only; the commit decisions are the permit
        // and the one-shot authorization inside the send.
        if authorization.is_cancelled() {
            return GracefulOutcome::Cancelled;
        }
        if !self.state.is_active_now(self.generation) {
            return GracefulOutcome::Stale;
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
            job_ack.complete(backend.request_graceful_terminate(
                state.handle,
                application,
                &job_authorization,
                permit,
            ));
        });
        if let Err(error) = self.state.executor.execute(job) {
            return GracefulOutcome::NotIssued(ApplicationError::Dispatch(error.to_string()));
        }
        let uncertain_or_unissued = |error: ApplicationError| {
            // Cancelled authorizations can never send; anything else past the
            // request commit may have.
            if authorization.is_cancelled() || request.is_unissued() {
                GracefulOutcome::NotIssued(error)
            } else {
                GracefulOutcome::Uncertain(error)
            }
        };
        match ack.wait(self.state.budget) {
            AckWait::Ready(Ok(outcome)) => outcome,
            AckWait::Ready(Err(error)) => uncertain_or_unissued(error),
            AckWait::TimedOut | AckWait::Cancelled => {
                let abandoned = request.abandon_confirmed();
                if abandoned {
                    GracefulOutcome::NotIssued(ApplicationError::TimedOut)
                } else {
                    uncertain_or_unissued(ApplicationError::TimedOut)
                }
            }
        }
    }
}
