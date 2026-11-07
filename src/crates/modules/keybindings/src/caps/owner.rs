//! Tap-before-remap acquisition and invalidate-first release.
//!
//! [`CapsOwner`] is the only place that joins the shared input service to the
//! global remap, in a fixed order:
//!
//! * **start**: subscribe the suppressing handler and wait for the service's
//!   confirmed tap acknowledgement; only then apply the remap. A missing
//!   permission, cancelled readiness, timeout or disabled tap returns before the
//!   remap and cannot mutate Caps.
//! * **stop**: invalidate dispatch and clear armed state first, then restore
//!   under the lease. Resolved outcomes release the subscription. A restore
//!   that is still pending keeps an inert, safety-only subscription (F18 and
//!   the matched key-ups are swallowed, no command can be enqueued) and blocks
//!   a successor from acquiring until it resolves.
//!
//! The owner never starts a thread. The caller (the Keybindings runtime, off the
//! tap and off the main thread) calls [`CapsOwner::stop`] and polls
//! [`CapsOwner::check_tap`] to notice a lost tap, because the input service
//! reports tap loss to a handler only for presses that were held.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use stache_macos::caps_remap::{ApplyError, CapsRemap, RestoreOutcome};
use stache_sdk::CancellationToken;
use stache_sdk::input::{
    Epoch, HandlerSpec, InputError, InputEventKind, InputFilter, InputPriority, InputService,
    KeyCode, KindSet, NativeCleanup, Subscription, TapKind, TapStatus, WorkRequirement,
};

use super::handler::{CapsHandler, CapsSink};
use super::parser::CapsKey;
use super::state::F18_KEY;

/// Why starting failed.
///
/// In every case Caps Lock was not (or is not known to have been) left
/// remapped, except [`CapsStartError::Remap`] with an uncertain error, which
/// keeps cleanup pending and is reflected in [`CapsOwner::status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapsStartError {
    /// There are no Caps bindings to own.
    NoBindings,
    /// A previous start is active or its cleanup is still pending.
    Busy,
    /// An earlier remap is unresolved and blocks acquisition.
    Blocked(String),
    /// The input subscription or tap acknowledgement failed; nothing was remapped.
    Input(InputError),
    /// The tap was not ready after subscribing; nothing was remapped.
    TapNotReady(TapStatus),
    /// The remap was refused or did not complete.
    Remap(ApplyError),
}

impl std::fmt::Display for CapsStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoBindings => f.write_str("there are no CapsLock bindings"),
            Self::Busy => f.write_str("a CapsLock remap is already active or still cleaning up"),
            Self::Blocked(reason) => write!(f, "CapsLock ownership is blocked: {reason}"),
            Self::Input(error) => write!(f, "the input tap is unavailable: {error}"),
            Self::TapNotReady(status) => write!(f, "the input tap is not ready: {status:?}"),
            Self::Remap(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CapsStartError {}

/// The truthful state of Caps ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapsStatus {
    /// Nothing is owned and nothing is pending.
    Inactive,
    /// The remap is applied and verified and dispatch is live.
    Active,
    /// Restoration did not complete; our mapping is still pending.
    CleanupPending {
        /// Why.
        reason: String,
        /// Physical Caps may emit F18 to applications until recovery.
        f18_exposed: bool,
    },
    /// A native write may have happened and is unverified.
    Uncertain {
        /// Why.
        reason: String,
        /// Physical Caps may emit F18 to applications until recovery.
        f18_exposed: bool,
    },
    /// Caps Lock is mapped by someone else; nothing of theirs was changed.
    OwnershipLost {
        /// The foreign destination usage.
        observed_destination: u64,
    },
    /// A corrupt journal or unsafe path blocks everything.
    Blocked(String),
}

/// What a stop did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopReport {
    /// Always `true`: dispatch is invalidated before anything else happens.
    pub dispatch_invalidated: bool,
    /// The restore outcome.
    pub restore: RestoreOutcome,
    /// The resulting status.
    pub status: CapsStatus,
    /// Native tap cleanup when the subscription was released.
    pub native: Option<NativeCleanup>,
}

struct Active {
    handler: Arc<CapsHandler>,
    subscription: Subscription,
    epoch: Epoch,
}

/// The Caps ownership owner. One per application.
pub struct CapsOwner {
    input: Arc<InputService>,
    remap: Arc<CapsRemap>,
    priority: InputPriority,
    /// Serializes start and stop; never held on the tap.
    operation: Mutex<()>,
    active: Mutex<Option<Active>>,
    status: Mutex<CapsStatus>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn filter(keys: impl IntoIterator<Item = KeyCode>) -> InputFilter {
    InputFilter::new(KindSet::of(InputEventKind::KeyDown).with(InputEventKind::KeyUp))
        .with_keys(keys)
}

impl CapsOwner {
    /// An owner over the shared input service and the remap.
    #[must_use]
    pub const fn new(
        input: Arc<InputService>,
        remap: Arc<CapsRemap>,
        priority: InputPriority,
    ) -> Self {
        Self {
            input,
            remap,
            priority,
            operation: Mutex::new(()),
            active: Mutex::new(None),
            status: Mutex::new(CapsStatus::Inactive),
        }
    }

    /// The current status.
    #[must_use]
    pub fn status(&self) -> CapsStatus { lock(&self.status).clone() }

    fn set_status(&self, status: CapsStatus) { *lock(&self.status) = status; }

    /// Whether a handler (live or safety-only) is subscribed.
    #[must_use]
    pub fn has_subscription(&self) -> bool { lock(&self.active).is_some() }

    /// Subscribes, waits for the confirmed tap, then remaps Caps Lock.
    ///
    /// # Errors
    /// Returns a [`CapsStartError`]. Before the remap step nothing was mutated.
    pub fn start(
        &self,
        bindings: &[CapsKey],
        sink: Arc<dyn CapsSink>,
        cancel: &CancellationToken,
    ) -> Result<(), CapsStartError> {
        let _operation = lock(&self.operation);
        if bindings.is_empty() {
            return Err(CapsStartError::NoBindings);
        }
        if lock(&self.active).is_some() {
            return Err(CapsStartError::Busy);
        }
        if let Some(reason) = self.remap.pending() {
            return Err(CapsStartError::Blocked(reason));
        }
        let handler = Arc::new(CapsHandler::new(bindings.iter().copied(), sink));
        let keys = std::iter::once(F18_KEY).chain(handler.bound_keys().iter().map(|k| KeyCode(*k)));
        let spec = HandlerSpec {
            filter: filter(keys),
            priority: self.priority,
            // Capacity is the dispatcher's concern; reserving here
            // would let a full queue leak the F18 surrogate to applications.
            work: WorkRequirement::None,
        };
        // Subscribing returns only after the tap acknowledged it is installed
        // and enabled, honoring cancellation and the 2 s native bound.
        let subscription = self
            .input
            .subscribe_handler(spec, Arc::clone(&handler) as _, cancel)
            .map_err(CapsStartError::Input)?;
        let status = self.input.tap_status(TapKind::Suppressing);
        if status != TapStatus::Ready {
            let _ = subscription.unsubscribe();
            return Err(CapsStartError::TapNotReady(status));
        }
        let epoch = self.input.epoch();
        match self.remap.apply(cancel) {
            Ok(()) => {
                *lock(&self.active) = Some(Active { handler, subscription, epoch });
                self.set_status(CapsStatus::Active);
                Ok(())
            }
            Err(error) if error.needs_restore() => {
                // The mapping may exist: keep the inert subscription so F18 is
                // swallowed until a restore resolves it.
                handler.invalidate();
                let _ = subscription.set_filter(filter([F18_KEY]));
                *lock(&self.active) = Some(Active { handler, subscription, epoch });
                self.set_status(CapsStatus::Uncertain {
                    reason: error.to_string(),
                    f18_exposed: false,
                });
                Err(CapsStartError::Remap(error))
            }
            Err(error) => {
                let _ = subscription.unsubscribe();
                if let ApplyError::Blocked(reason) = &error {
                    self.set_status(CapsStatus::Blocked(reason.clone()));
                }
                Err(CapsStartError::Remap(error))
            }
        }
    }

    /// Invalidates dispatch, then restores under the lease.
    ///
    /// Safe to call repeatedly: a pending restore is retried, and with nothing
    /// owned it reports [`RestoreOutcome::NoJournal`]. Call off the tap and off
    /// the main thread.
    pub fn stop(&self, cancel: &CancellationToken) -> StopReport {
        let _operation = lock(&self.operation);
        self.stop_locked(cancel)
    }

    fn stop_locked(&self, cancel: &CancellationToken) -> StopReport {
        // 1. Invalidate dispatch and clear armed state before any slow work.
        {
            let active = lock(&self.active);
            if let Some(active) = active.as_ref() {
                active.handler.invalidate();
                // 2. Narrow to the safety-only filter; a failure only leaves a
                //    wider, still inert, subscription.
                let _ = active.subscription.set_filter(filter([F18_KEY]));
            }
        }
        // 3. Restore under the lease.
        let restore = self.remap.restore(cancel);
        let exposed = !self.tap_protects_f18();
        let status = match &restore {
            RestoreOutcome::Restored | RestoreOutcome::NoJournal => CapsStatus::Inactive,
            RestoreOutcome::OwnershipLost { observed_destination } => CapsStatus::OwnershipLost {
                observed_destination: *observed_destination,
            },
            RestoreOutcome::CleanupPending(reason) => CapsStatus::CleanupPending {
                reason: reason.clone(),
                f18_exposed: exposed,
            },
            RestoreOutcome::Uncertain(reason) => CapsStatus::Uncertain {
                reason: reason.clone(),
                f18_exposed: exposed,
            },
            RestoreOutcome::Blocked(reason) => CapsStatus::Blocked(reason.clone()),
        };
        let native = if restore.is_resolved() {
            lock(&self.active).take().map(|active| active.subscription.unsubscribe().native)
        } else {
            None
        };
        self.set_status(status.clone());
        StopReport {
            dispatch_invalidated: true,
            restore,
            status,
            native,
        }
    }

    /// Whether the retained subscription still swallows F18.
    fn tap_protects_f18(&self) -> bool {
        let active = lock(&self.active);
        active.as_ref().is_some_and(|active| {
            active.subscription.inert_reason().is_none()
                && self.input.tap_status(TapKind::Suppressing) == TapStatus::Ready
        })
    }

    /// Whether the retained subscription's tap is gone; `None` without one.
    fn tap_is_lost(&self) -> Option<bool> {
        let active = lock(&self.active);
        active.as_ref().map(|active| {
            active.handler.tap_lost()
                || active.subscription.inert_reason().is_some()
                || self.input.epoch() != active.epoch
                || self.input.tap_status(TapKind::Suppressing) != TapStatus::Ready
        })
    }

    /// Detects a lost tap and starts cleanup when it happened.
    ///
    /// Returns `Some` when the tap was disabled, re-enabling failed, the
    /// subscription went inert or the epoch advanced since [`CapsOwner::start`].
    /// Dispatch is already cancelled by then and no later re-enable resumes an
    /// armed old chord. Call from the runtime's bounded watch, off the tap.
    pub fn check_tap(&self, cancel: &CancellationToken) -> Option<StopReport> {
        let _operation = lock(&self.operation);
        let lost = self.tap_is_lost()?;
        lost.then(|| self.stop_locked(cancel))
    }
}
