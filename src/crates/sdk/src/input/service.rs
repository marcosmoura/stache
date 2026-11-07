//! The host-created input service: subscriptions, lazy tap lifecycle and the
//! tap entry points.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError, Weak};

use super::context_snapshot::ContextPublisher;
use super::control::{
    ACKNOWLEDGEMENT_BUDGET, CANCELLATION_QUANTUM, ControlAction, ControlRejected, ControlRequest,
    FailureCause, InputBackend, InputClock, MAX_RETRY_ATTEMPTS, SystemClock, TapKind, Ticket,
};
use super::deferral::Wake;
use super::diagnostics::{Diagnostics, InputCounters, InputDiagnostic};
use super::event::{Disposition, Epoch, RawInput};
use super::filter::{FilterError, InputFilter};
use super::ledger::PRESS_LEDGER_CAPACITY;
use super::reservations::{
    Claim, ReservationBackend, ReservationError, ReservationId, ReservationOwner,
    ReservationRegistry,
};
use super::router::{Router, bump};
use super::subscribers::{
    HandlerEntry, HandlerSpec, Health, InertReason, InputHandler, InputObserver, InputPriority,
    ObserverEntry, ObserverSpec, SubscriberSet, SubscriptionId,
};
use super::work::WorkReservations;
use crate::threading::CancellationToken;

static NEXT_SERVICE: AtomicU64 = AtomicU64::new(1);

/// Why a tap is still holding native resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retained {
    /// The action that failed.
    pub action: ControlAction,
    /// Why it failed.
    pub cause: FailureCause,
}

/// The observable lifecycle state of one tap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TapStatus {
    /// No native resources exist.
    Absent,
    /// Installed, enabled and acknowledged.
    Ready,
    /// The OS disabled the tap; a re-enable is awaiting acknowledgement.
    Disabled,
    /// Re-enabling failed. Visible, never reported as running.
    ReenableFailed {
        /// Attempts made since the disable.
        attempts: u32,
        /// Why the last attempt failed.
        cause: FailureCause,
    },
    /// An action failed; inert resources are retained for cleanup.
    Retained(Retained),
}

/// Why an input operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    /// The service is the inert placeholder; no native owner exists.
    Unavailable,
    /// The module capability was invalidated or released.
    Invalidated,
    /// The filter is invalid.
    InvalidFilter(FilterError),
    /// The reservation registry refused the claim.
    Reservation(ReservationError),
    /// The ID was issued by another service.
    ForeignService,
    /// The ID is from a retired generation or was already removed.
    StaleSubscription,
    /// The operation does not apply to this subscription kind.
    WrongKind,
    /// The caller was cancelled before the tap became ready.
    Cancelled,
    /// The tap is disabled or re-enabling; subscribing now would be dishonest.
    TapNotReady {
        /// The tap.
        tap: TapKind,
        /// Its status.
        status: TapStatus,
    },
    /// A previous failure left resources that could not be closed yet.
    TapNotClosed {
        /// The tap.
        tap: TapKind,
        /// Why.
        retained: Retained,
    },
    /// A native action failed or was not acknowledged.
    Native {
        /// The tap.
        tap: TapKind,
        /// The action.
        action: ControlAction,
        /// Why.
        cause: FailureCause,
    },
    /// The bounded retry budget is spent.
    RetryExhausted {
        /// The tap.
        tap: TapKind,
    },
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("input service is unavailable"),
            Self::Invalidated => f.write_str("input capability was invalidated"),
            Self::InvalidFilter(error) => write!(f, "invalid input filter: {error}"),
            Self::Reservation(error) => write!(f, "reservation refused: {error}"),
            Self::ForeignService => f.write_str("subscription belongs to another service"),
            Self::StaleSubscription => f.write_str("subscription is stale"),
            Self::WrongKind => f.write_str("operation does not apply to this subscription"),
            Self::Cancelled => f.write_str("input startup was cancelled"),
            Self::TapNotReady { tap, status } => write!(f, "{tap:?} tap is not ready: {status:?}"),
            Self::TapNotClosed { tap, retained } => {
                write!(f, "{tap:?} tap is not closed: {retained:?}")
            }
            Self::Native { tap, action, cause } => {
                write!(f, "{tap:?} tap {action:?} failed: {cause:?}")
            }
            Self::RetryExhausted { tap } => write!(f, "{tap:?} tap retry budget is spent"),
        }
    }
}

impl std::error::Error for InputError {}

impl From<FilterError> for InputError {
    fn from(error: FilterError) -> Self { Self::InvalidFilter(error) }
}

/// What happened to native resources when a subscription ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeCleanup {
    /// Other consumers still need the tap, or it never existed.
    NotNeeded,
    /// The tap was removed and acknowledged.
    Closed,
    /// The last suppressing handler left while the press ledger still holds a
    /// suppressed tombstone. Logical unsubscribe and reservation removal are
    /// done, but the suppressing tap stays installed so the rest of that press
    /// (repeats and up) is still swallowed. It is removed with acknowledgement
    /// off the tap thread once the last tombstone is consumed by its up, a
    /// tap-disable drains the ledger or the owner shuts down; a subscription
    /// made first reuses it. Failures surface in [`InputService::tap_status`]
    /// and [`InputService::diagnostics`].
    Deferred,
    /// The tap is not physically closed; inert resources are retained.
    Retained(Retained),
}

/// The result of ending a subscription. Logical invalidation always succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsubscribeReport {
    /// State of native cleanup.
    pub native: NativeCleanup,
}

struct TapState {
    status: TapStatus,
    generation: u64,
    reenable_attempts: u32,
}

struct TapCell {
    state: Mutex<TapState>,
}

impl TapCell {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(TapState {
                status: TapStatus::Absent,
                generation: 0,
                reenable_attempts: 0,
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, TapState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Default)]
struct Control {
    next_revision: u64,
    next_generation: u64,
    next_sequence: u64,
    handlers: Vec<Arc<HandlerEntry>>,
    observers: Vec<Arc<ObserverEntry>>,
    reservations: HashMap<u64, (ReservationOwner, ReservationId)>,
    next_deferral: u64,
    /// The deferred suppressing-tap removal in flight, if any.
    deferral: Option<u64>,
}

#[derive(Clone)]
pub(crate) enum EntryRef {
    Handler(Arc<HandlerEntry>),
    Observer(Arc<ObserverEntry>),
}

impl EntryRef {
    pub(crate) fn id(&self) -> SubscriptionId {
        match self {
            Self::Handler(entry) => entry.id,
            Self::Observer(entry) => entry.id,
        }
    }

    fn health(&self) -> &Health {
        match self {
            Self::Handler(entry) => &entry.health,
            Self::Observer(entry) => &entry.health,
        }
    }
}

/// The host-created, shared input service.
pub struct InputService {
    identity: u64,
    backend: Option<Arc<dyn InputBackend>>,
    clock: Arc<dyn InputClock>,
    router: Router,
    diagnostics: Arc<Diagnostics>,
    reservations: ReservationRegistry,
    publisher: ContextPublisher,
    control: Mutex<Control>,
    suppressing: Arc<TapCell>,
    listen_only: Arc<TapCell>,
    this: Weak<Self>,
    /// Test seam: holds the deferral helper before it takes the control lock.
    #[cfg(any(test, feature = "testing"))]
    deferral_gate: Arc<super::testing::DeferralGate>,
}

impl InputService {
    /// The inert placeholder: every subscription returns
    /// [`InputError::Unavailable`] and no backend is ever called.
    #[must_use]
    pub fn unavailable() -> Arc<Self> {
        Self::build(None, Arc::new(super::work::NoWork), Arc::new(SystemClock))
    }

    /// A service over a native owner, with the real clock.
    #[must_use]
    pub fn new(backend: Arc<dyn InputBackend>, work: Arc<dyn WorkReservations>) -> Arc<Self> {
        Self::with_clock(backend, work, Arc::new(SystemClock))
    }

    /// A service with an injected clock.
    #[must_use]
    pub fn with_clock(
        backend: Arc<dyn InputBackend>,
        work: Arc<dyn WorkReservations>,
        clock: Arc<dyn InputClock>,
    ) -> Arc<Self> {
        Self::build(Some(backend), work, clock)
    }

    fn build(
        backend: Option<Arc<dyn InputBackend>>,
        work: Arc<dyn WorkReservations>,
        clock: Arc<dyn InputClock>,
    ) -> Arc<Self> {
        let identity = NEXT_SERVICE.fetch_add(1, Ordering::Relaxed);
        let diagnostics = Arc::new(Diagnostics::default());
        let router = Router::new(
            work,
            Arc::clone(&clock),
            Arc::clone(&diagnostics),
            PRESS_LEDGER_CAPACITY,
        );
        let publisher = ContextPublisher::new(Arc::clone(&router.context));
        Arc::new_cyclic(|this| Self {
            identity,
            backend,
            clock,
            router,
            reservations: ReservationRegistry::new(identity, Arc::clone(&diagnostics)),
            diagnostics,
            publisher,
            control: Mutex::new(Control::default()),
            suppressing: TapCell::new(),
            listen_only: TapCell::new(),
            this: Weak::clone(this),
            #[cfg(any(test, feature = "testing"))]
            deferral_gate: Arc::default(),
        })
    }

    /// Holds the deferred-removal helper before it takes the control lock until
    /// the returned guard is released or dropped. Tests only.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn hold_deferral_helper(&self) -> super::testing::DeferralHold { self.deferral_gate.hold() }

    /// Whether a native owner is attached.
    #[must_use]
    pub const fn is_available(&self) -> bool { self.backend.is_some() }

    /// The handle the host uses to publish frontmost context off the tap.
    #[must_use]
    pub fn context_publisher(&self) -> ContextPublisher { self.publisher.clone() }

    /// The central cross-backend reservation registry.
    #[must_use]
    pub const fn reservations(&self) -> &ReservationRegistry { &self.reservations }

    /// The current owner epoch.
    #[must_use]
    pub fn epoch(&self) -> Epoch { self.router.epoch() }

    /// Presses currently tracked by the ledger.
    #[must_use]
    pub fn ledger_len(&self) -> usize { self.router.ledger_len() }

    /// A snapshot of the counters.
    #[must_use]
    pub fn counters(&self) -> InputCounters {
        self.router.counters.snapshot(self.diagnostics.dropped())
    }

    /// The bounded diagnostic log, oldest first.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<InputDiagnostic> { self.diagnostics.snapshot() }

    /// The lifecycle state of one tap.
    #[must_use]
    pub fn tap_status(&self, tap: TapKind) -> TapStatus { self.cell(tap).lock().status.clone() }

    const fn cell(&self, tap: TapKind) -> &Arc<TapCell> {
        match tap {
            TapKind::Suppressing => &self.suppressing,
            TapKind::ListenOnly => &self.listen_only,
        }
    }

    fn control(&self) -> MutexGuard<'_, Control> {
        self.control.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn require_backend(&self) -> Result<&Arc<dyn InputBackend>, InputError> {
        self.backend.as_ref().ok_or(InputError::Unavailable)
    }

    // ---- tap entry points (owner thread) ----

    /// Routes one native event. Called synchronously from the tap callback.
    ///
    /// For [`TapKind::ListenOnly`] the result is always [`Disposition::Pass`]:
    /// passive delivery cannot decide an event.
    pub fn dispatch(&self, tap: TapKind, raw: RawInput) -> Disposition {
        match tap {
            TapKind::Suppressing => self.router.dispatch_suppressing(raw),
            TapKind::ListenOnly => {
                self.router.dispatch_passive(raw);
                Disposition::Pass
            }
        }
    }

    /// The OS disabled a tap. Cancels armed per-press state, advances the epoch,
    /// never synthesizes releases, reports the discontinuity and submits one
    /// non-blocking re-enable request whose result becomes visible in
    /// [`Self::tap_status`].
    pub fn tap_disabled(&self, tap: TapKind) {
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let epoch = match tap {
            TapKind::Suppressing => self.router.cancel_presses_and_advance(),
            TapKind::ListenOnly => self.router.notify_observers_discontinuity(),
        };
        self.diagnostics.record(InputDiagnostic::Discontinuity { tap, epoch });
        if tap == TapKind::Suppressing && self.router.removal.is_pending() {
            // The drained ledger made the deferred removal due; re-enabling a tap
            // that is about to be removed would only race it. The status must
            // still tell the truth: a subscription that reuses the tap before
            // the removal runs re-enables it (`revive_disabled_for_reuse`).
            //
            // A `ReenableFailed` status is not skipped: while a subscription is
            // reviving the tap it is the in-flight placeholder, and a disable
            // that lands then needs its own re-enable (normal path below).
            let mut state = self.cell(tap).lock();
            if matches!(state.status, TapStatus::Ready | TapStatus::Disabled) {
                if state.status == TapStatus::Ready {
                    state.status = TapStatus::Disabled;
                    state.reenable_attempts = 0;
                }
                return;
            }
        }
        let cell = self.cell(tap);
        let generation = {
            let mut state = cell.lock();
            if !matches!(
                state.status,
                TapStatus::Ready | TapStatus::Disabled | TapStatus::ReenableFailed { .. }
            ) {
                return;
            }
            state.status = TapStatus::Disabled;
            state.reenable_attempts = 1;
            state.generation
        };
        let ticket = self.reenable_ticket(tap, generation);
        let request = ControlRequest::new(tap, ControlAction::Enable, generation, ticket);
        if let Err(rejected) = backend.submit(request) {
            if rejected == ControlRejected::QueueFull {
                bump(&self.router.counters.control_rejected);
            }
            self.set_reenable_failed(tap, generation, FailureCause::Rejected(rejected));
        }
    }

    fn reenable_ticket(&self, tap: TapKind, generation: u64) -> Arc<Ticket> {
        let cell = Arc::clone(self.cell(tap));
        let diagnostics = Arc::clone(&self.diagnostics);
        Ticket::new(Some(Box::new(move |result| {
            let mut state = cell.lock();
            if state.generation != generation || state.status != TapStatus::Disabled {
                return;
            }
            match result {
                Ok(()) => {
                    state.status = TapStatus::Ready;
                    state.reenable_attempts = 0;
                }
                Err(error) => {
                    let attempts = state.reenable_attempts;
                    let cause = FailureCause::Failed(error.0);
                    state.status = TapStatus::ReenableFailed { attempts, cause: cause.clone() };
                    drop(state);
                    diagnostics.record(InputDiagnostic::ReenableFailed { tap, attempts, cause });
                }
            }
        })))
    }

    fn set_reenable_failed(&self, tap: TapKind, generation: u64, cause: FailureCause) {
        let mut state = self.cell(tap).lock();
        if state.generation != generation {
            return;
        }
        let attempts = state.reenable_attempts;
        state.status = TapStatus::ReenableFailed { attempts, cause: cause.clone() };
        drop(state);
        self.diagnostics
            .record(InputDiagnostic::ReenableFailed { tap, attempts, cause });
    }

    /// Explicitly retries re-enabling a disabled tap, waiting for the
    /// acknowledgement. At most [`MAX_RETRY_ATTEMPTS`] attempts per disable.
    ///
    /// # Errors
    /// Returns [`InputError::RetryExhausted`] when the budget is spent, or
    /// [`InputError::Native`] when this attempt fails.
    pub fn retry_reenable(
        &self,
        tap: TapKind,
        cancel: &CancellationToken,
    ) -> Result<TapStatus, InputError> {
        self.require_backend()?;
        let _control = self.control();
        let cell = self.cell(tap);
        let (generation, attempts) = {
            let mut state = cell.lock();
            if !matches!(
                state.status,
                TapStatus::Disabled | TapStatus::ReenableFailed { .. }
            ) {
                return Ok(state.status.clone());
            }
            if state.reenable_attempts >= MAX_RETRY_ATTEMPTS {
                return Err(InputError::RetryExhausted { tap });
            }
            state.reenable_attempts += 1;
            // The outstanding request, if any, is abandoned by the generation guard
            // below: its continuation only acts while the status is `Disabled`.
            state.status = TapStatus::ReenableFailed {
                attempts: state.reenable_attempts,
                cause: FailureCause::Cancelled,
            };
            (state.generation, state.reenable_attempts)
        };
        match self.run(tap, ControlAction::Enable, generation, cancel) {
            Ok(()) => {
                let mut state = cell.lock();
                state.status = TapStatus::Ready;
                state.reenable_attempts = 0;
                Ok(TapStatus::Ready)
            }
            Err(cause) => {
                let status = TapStatus::ReenableFailed { attempts, cause: cause.clone() };
                cell.lock().status = status;
                self.diagnostics.record(InputDiagnostic::ReenableFailed {
                    tap,
                    attempts,
                    cause: cause.clone(),
                });
                Err(InputError::Native {
                    tap,
                    action: ControlAction::Enable,
                    cause,
                })
            }
        }
    }

    /// Explicitly retries closing a tap that still holds retained resources.
    ///
    /// # Errors
    /// Returns [`InputError::TapNotClosed`] with the reason it is still open.
    pub fn retry_cleanup(&self, tap: TapKind) -> Result<(), InputError> {
        self.require_backend()?;
        let _control = self.control();
        if self.cell(tap).lock().status == TapStatus::Absent {
            return Ok(());
        }
        self.remove_tap(tap)
            .map_err(|retained| InputError::TapNotClosed { tap, retained })
    }

    // ---- subscriptions ----

    /// Subscribes a suppressing handler. The suppressing tap is installed lazily
    /// and the subscription becomes live only after the tap is acknowledged.
    ///
    /// # Errors
    /// Returns an [`InputError`]; no handler is retained on failure.
    pub fn subscribe_handler(
        self: &Arc<Self>,
        spec: HandlerSpec,
        handler: Arc<dyn InputHandler>,
        cancel: &CancellationToken,
    ) -> Result<Subscription, InputError> {
        self.require_backend()?;
        spec.filter.validate()?;
        let mut control = self.control();
        self.revive_disabled_for_reuse(&control, cancel)?;
        let owner = self.reservations.new_owner();
        let reservation = self
            .reservations
            .register(
                owner,
                ReservationBackend::Hid,
                Claim::Filter(spec.filter.clone()),
                spec.priority,
            )
            .map_err(InputError::Reservation)?;
        let generation = match self.ensure_tap(&mut control, TapKind::Suppressing, cancel) {
            Ok(generation) => generation,
            Err(error) => {
                let _ = self.reservations.remove(reservation);
                return Err(error);
            }
        };
        // A tap still held for a tombstone is reused, not installed twice.
        self.cancel_deferral(&mut control);
        control.next_revision += 1;
        control.next_sequence += 1;
        let id = SubscriptionId {
            service: self.identity,
            generation,
            revision: control.next_revision,
        };
        let entry = Arc::new(HandlerEntry {
            id,
            sequence: control.next_sequence,
            priority: spec.priority,
            filter: spec.filter,
            work: spec.work,
            handler,
            health: Health::default(),
        });
        self.diagnose_overlaps(&control, &entry);
        control.reservations.insert(id.revision, (owner, reservation));
        control.handlers.push(Arc::clone(&entry));
        self.publish(&control);
        drop(control);
        Ok(Subscription::new(Arc::clone(self), EntryRef::Handler(entry)))
    }

    /// Subscribes a passive observer. Only the listen-only tap is created;
    /// the suppressing tap is never touched.
    ///
    /// # Errors
    /// Returns an [`InputError`]; no observer is retained on failure.
    pub fn subscribe_observer(
        self: &Arc<Self>,
        spec: ObserverSpec,
        observer: Arc<dyn InputObserver>,
        cancel: &CancellationToken,
    ) -> Result<Subscription, InputError> {
        self.require_backend()?;
        spec.filter.validate()?;
        let mut control = self.control();
        let generation = self.ensure_tap(&mut control, TapKind::ListenOnly, cancel)?;
        control.next_revision += 1;
        let id = SubscriptionId {
            service: self.identity,
            generation,
            revision: control.next_revision,
        };
        let entry = Arc::new(ObserverEntry {
            id,
            filter: spec.filter,
            observer,
            health: Health::default(),
        });
        control.observers.push(Arc::clone(&entry));
        self.publish(&control);
        drop(control);
        Ok(Subscription::new(Arc::clone(self), EntryRef::Observer(entry)))
    }

    /// Ends a subscription by ID. Logical invalidation is immediate and
    /// independent of native cleanup.
    ///
    /// # Errors
    /// Returns [`InputError::ForeignService`] or [`InputError::StaleSubscription`].
    pub fn unsubscribe(&self, id: SubscriptionId) -> Result<UnsubscribeReport, InputError> {
        let mut control = self.control();
        let entry = self.lookup(&control, id)?;
        entry.health().mark(InertReason::Unsubscribed);
        let tap = match &entry {
            EntryRef::Handler(_) => {
                control.handlers.retain(|e| e.id != id);
                if let Some((_, reservation)) = control.reservations.remove(&id.revision) {
                    let _ = self.reservations.remove(reservation);
                }
                TapKind::Suppressing
            }
            EntryRef::Observer(_) => {
                control.observers.retain(|e| e.id != id);
                TapKind::ListenOnly
            }
        };
        self.publish(&control);
        let still_used = match tap {
            TapKind::Suppressing => !control.handlers.is_empty(),
            TapKind::ListenOnly => !control.observers.is_empty(),
        };
        let native = if still_used || self.cell(tap).lock().status == TapStatus::Absent {
            NativeCleanup::NotNeeded
        } else if tap == TapKind::Suppressing && self.defer_removal(&mut control) {
            NativeCleanup::Deferred
        } else {
            match self.remove_tap(tap) {
                Ok(()) => NativeCleanup::Closed,
                Err(retained) => NativeCleanup::Retained(retained),
            }
        };
        drop(control);
        Ok(UnsubscribeReport { native })
    }

    /// Defers removing the suppressing tap while a suppressed tombstone is live.
    /// `false` means there is none, or no helper thread could be started, and the
    /// caller removes the tap now.
    fn defer_removal(&self, control: &mut Control) -> bool {
        control.next_deferral += 1;
        let id = control.next_deferral;
        if !self.router.arm_removal_if_tombstones(id) {
            return false;
        }
        let signal = Arc::clone(&self.router.removal);
        let service = Weak::clone(&self.this);
        let spawned = std::thread::Builder::new()
            .name("stache-input-deferred-removal".into())
            .spawn(move || {
                // Waits holding only the signal, so a dropped service wakes it.
                if signal.wait(id) == Wake::Due
                    && let Some(service) = service.upgrade()
                {
                    service.finish_deferred_removal(id);
                }
            });
        if spawned.is_err() {
            self.router.removal.cancel();
            return false;
        }
        control.deferral = Some(id);
        true
    }

    fn finish_deferred_removal(&self, id: u64) {
        #[cfg(any(test, feature = "testing"))]
        self.deferral_gate.wait_open();
        let mut control = self.control();
        if control.deferral != Some(id) {
            return;
        }
        control.deferral = None;
        self.router.removal.finish();
        if control.handlers.is_empty()
            && self.cell(TapKind::Suppressing).lock().status != TapStatus::Absent
        {
            // Failure is retained and recorded; `retry_cleanup` is explicit.
            let _ = self.remove_tap(TapKind::Suppressing);
        }
    }

    /// A tap disabled while its removal was deferred is still held for reuse by
    /// a new subscription: re-enable it (acknowledged) and cancel the removal.
    /// On failure the deferral stays, so the helper still removes the tap.
    fn revive_disabled_for_reuse(
        &self,
        control: &Control,
        cancel: &CancellationToken,
    ) -> Result<(), InputError> {
        let tap = TapKind::Suppressing;
        if control.deferral.is_none() {
            return Ok(());
        }
        let cell = self.cell(tap);
        let generation = {
            let mut state = cell.lock();
            if state.status != TapStatus::Disabled {
                return Ok(());
            }
            state.reenable_attempts = 1;
            // In-flight placeholder (as in `retry_reenable`): a disable that lands
            // during the acknowledged `Enable` takes `tap_disabled`'s normal path.
            state.status = TapStatus::ReenableFailed {
                attempts: 1,
                cause: FailureCause::Cancelled,
            };
            state.generation
        };
        match self.run(tap, ControlAction::Enable, generation, cancel) {
            Ok(()) => {
                let mut state = cell.lock();
                // Only the placeholder at this generation becomes `Ready`; an
                // intervening disable keeps its own state and re-enable.
                if state.generation == generation
                    && state.status
                        == (TapStatus::ReenableFailed {
                            attempts: 1,
                            cause: FailureCause::Cancelled,
                        })
                {
                    state.status = TapStatus::Ready;
                    state.reenable_attempts = 0;
                }
                Ok(())
            }
            Err(cause) => {
                let status = TapStatus::ReenableFailed {
                    attempts: 1,
                    cause: cause.clone(),
                };
                cell.lock().status = status;
                self.diagnostics.record(InputDiagnostic::ReenableFailed {
                    tap,
                    attempts: 1,
                    cause: cause.clone(),
                });
                Err(match cause {
                    FailureCause::Cancelled => InputError::Cancelled,
                    cause => InputError::Native {
                        tap,
                        action: ControlAction::Enable,
                        cause,
                    },
                })
            }
        }
    }

    fn cancel_deferral(&self, control: &mut Control) {
        if control.deferral.take().is_some() {
            self.router.removal.cancel();
        }
    }

    /// Coordinated owner teardown: hands any deferred removal to the owner now.
    ///
    /// The `Remove` is only queued, not awaited, so a shutdown that follows runs
    /// it with the other queued removals. Never blocks: when a control operation
    /// is in flight the deferred removal is already being completed by it.
    pub fn begin_owner_shutdown(&self) {
        let control = match self.control.try_lock() {
            Ok(control) => control,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        let mut control = control;
        if control.deferral.take().is_none() {
            return;
        }
        self.router.removal.cancel();
        let cell = Arc::clone(self.cell(TapKind::Suppressing));
        let generation = cell.lock().generation;
        let ticket = Ticket::new(Some(Box::new(move |result| {
            let mut state = cell.lock();
            if state.generation == generation && result.is_ok() {
                state.status = TapStatus::Absent;
                state.reenable_attempts = 0;
            }
        })));
        let request =
            ControlRequest::new(TapKind::Suppressing, ControlAction::Remove, generation, ticket);
        if let Some(backend) = self.backend.as_ref() {
            let _ = backend.submit(request);
        }
        drop(control);
    }

    /// Changes a handler's priority and/or filter. The old ID becomes stale and
    /// its callbacks stop immediately; a press it already suppressed is still
    /// consumed by an inert ledger tombstone. Returns the replacement ID.
    ///
    /// # Errors
    /// Returns an [`InputError`]; on failure the old subscription is unchanged.
    pub(crate) fn update_handler(
        &self,
        id: SubscriptionId,
        priority: Option<InputPriority>,
        filter: Option<InputFilter>,
    ) -> Result<(SubscriptionId, EntryRef), InputError> {
        if let Some(filter) = &filter {
            filter.validate()?;
        }
        let mut control = self.control();
        let EntryRef::Handler(old) = self.lookup(&control, id)? else {
            return Err(InputError::WrongKind);
        };
        let priority = priority.unwrap_or(old.priority);
        let filter = filter.unwrap_or_else(|| old.filter.clone());
        let (owner, reservation) = control
            .reservations
            .get(&id.revision)
            .copied()
            .ok_or(InputError::StaleSubscription)?;
        let new_reservation = self
            .reservations
            .update(reservation, Claim::Filter(filter.clone()), priority)
            .map_err(InputError::Reservation)?;
        control.next_revision += 1;
        let new_id = SubscriptionId {
            revision: control.next_revision,
            ..id
        };
        let entry = Arc::new(HandlerEntry {
            id: new_id,
            sequence: old.sequence,
            priority,
            filter,
            work: old.work,
            handler: Arc::clone(&old.handler),
            health: Health::default(),
        });
        old.health.mark(InertReason::Replaced);
        control.reservations.remove(&id.revision);
        control.reservations.insert(new_id.revision, (owner, new_reservation));
        control.handlers.retain(|e| e.id != id);
        self.diagnose_overlaps(&control, &entry);
        control.handlers.push(Arc::clone(&entry));
        self.publish(&control);
        drop(control);
        Ok((new_id, EntryRef::Handler(entry)))
    }

    fn lookup(&self, control: &Control, id: SubscriptionId) -> Result<EntryRef, InputError> {
        if id.service != self.identity {
            return Err(InputError::ForeignService);
        }
        control
            .handlers
            .iter()
            .find(|e| e.id == id)
            .map(|e| EntryRef::Handler(Arc::clone(e)))
            .or_else(|| {
                control
                    .observers
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| EntryRef::Observer(Arc::clone(e)))
            })
            .ok_or(InputError::StaleSubscription)
    }

    fn publish(&self, control: &Control) {
        self.router.subscribers.store(Arc::new(SubscriberSet::build(
            &control.handlers,
            &control.observers,
        )));
    }

    fn diagnose_overlaps(&self, control: &Control, entry: &HandlerEntry) {
        for other in &control.handlers {
            if other.id != entry.id
                && !other.health.is_inert()
                && other.filter.overlaps(&entry.filter)
            {
                self.diagnostics.record(InputDiagnostic::Overlap {
                    first: other.id,
                    second: entry.id,
                    equal_priority: other.priority == entry.priority,
                });
            }
        }
    }

    // ---- native lifecycle ----

    fn ensure_tap(
        &self,
        control: &mut Control,
        tap: TapKind,
        cancel: &CancellationToken,
    ) -> Result<u64, InputError> {
        if cancel.is_cancelled() {
            return Err(InputError::Cancelled);
        }
        let (status, generation) = {
            let state = self.cell(tap).lock();
            (state.status.clone(), state.generation)
        };
        match status {
            TapStatus::Ready => return Ok(generation),
            TapStatus::Disabled | TapStatus::ReenableFailed { .. } => {
                return Err(InputError::TapNotReady { tap, status });
            }
            TapStatus::Retained(_) => {
                self.remove_tap(tap)
                    .map_err(|retained| InputError::TapNotClosed { tap, retained })?;
            }
            TapStatus::Absent => {}
        }
        control.next_generation += 1;
        let generation = control.next_generation;
        self.cell(tap).lock().generation = generation;
        for action in [ControlAction::Install, ControlAction::Enable] {
            if let Err(cause) = self.run(tap, action, generation, cancel) {
                // A rejected install issued nothing, so nothing is retained.
                let nothing_issued =
                    action == ControlAction::Install && matches!(cause, FailureCause::Rejected(_));
                if !nothing_issued {
                    self.cell(tap).lock().status =
                        TapStatus::Retained(Retained { action, cause: cause.clone() });
                    // Best effort; failure keeps the retained state for retry.
                    let _ = self.remove_tap(tap);
                }
                return Err(match cause {
                    FailureCause::Cancelled => InputError::Cancelled,
                    cause => InputError::Native { tap, action, cause },
                });
            }
        }
        self.cell(tap).lock().status = TapStatus::Ready;
        Ok(generation)
    }

    /// Removes a tap and waits for the acknowledgement. On failure resources are
    /// retained as inert and the reason is recorded; there is no false success.
    fn remove_tap(&self, tap: TapKind) -> Result<(), Retained> {
        let generation = self.cell(tap).lock().generation;
        match self.run(tap, ControlAction::Remove, generation, &CancellationToken::new()) {
            Ok(()) => {
                let mut state = self.cell(tap).lock();
                state.status = TapStatus::Absent;
                state.reenable_attempts = 0;
                Ok(())
            }
            Err(cause) => {
                let retained = Retained {
                    action: ControlAction::Remove,
                    cause,
                };
                self.cell(tap).lock().status = TapStatus::Retained(retained.clone());
                self.diagnostics.record(InputDiagnostic::TapNotClosed {
                    tap,
                    retained: retained.clone(),
                });
                Err(retained)
            }
        }
    }

    fn run(
        &self,
        tap: TapKind,
        action: ControlAction,
        generation: u64,
        cancel: &CancellationToken,
    ) -> Result<(), FailureCause> {
        let backend =
            self.backend.as_ref().ok_or(FailureCause::Rejected(ControlRejected::Closed))?;
        let ticket = Ticket::new(None);
        let request = ControlRequest::new(tap, action, generation, Arc::clone(&ticket));
        if let Err(rejected) = backend.submit(request) {
            if rejected == ControlRejected::QueueFull {
                bump(&self.router.counters.control_rejected);
            }
            return Err(FailureCause::Rejected(rejected));
        }
        self.await_ack(&ticket, cancel, action != ControlAction::Remove)
    }

    /// Waits up to [`ACKNOWLEDGEMENT_BUDGET`] in [`CANCELLATION_QUANTUM`] slices on
    /// the injected clock. A timed-out or cancelled ticket is marked so a queued
    /// owner never acts on it late.
    fn await_ack(
        &self,
        ticket: &Arc<Ticket>,
        cancel: &CancellationToken,
        revoke: bool,
    ) -> Result<(), FailureCause> {
        let started = self.clock.now();
        let deadline = started.checked_add(ACKNOWLEDGEMENT_BUDGET).unwrap_or(started);
        loop {
            if let Some(result) = ticket.take_outcome() {
                return result.map_err(|error| FailureCause::Failed(error.0));
            }
            if cancel.is_cancelled() {
                return ticket
                    .finish_waiting(revoke)
                    .map_or(Err(FailureCause::Cancelled), |result| {
                        result.map_err(|error| FailureCause::Failed(error.0))
                    });
            }
            let now = self.clock.now();
            if now >= deadline {
                return ticket
                    .finish_waiting(revoke)
                    .map_or(Err(FailureCause::TimedOut), |result| {
                        result.map_err(|error| FailureCause::Failed(error.0))
                    });
            }
            let slice = CANCELLATION_QUANTUM.min(deadline.saturating_duration_since(now));
            self.clock.wait_for(&ticket.signal, slice);
        }
    }
}

/// A live subscription handle. Dropping it unsubscribes.
pub struct Subscription {
    inner: Arc<SubscriptionInner>,
}

pub(crate) struct SubscriptionInner {
    service: Arc<InputService>,
    entry: Mutex<EntryRef>,
    released: AtomicBool,
}

impl SubscriptionInner {
    fn entry(&self) -> EntryRef {
        self.entry.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Immediate logical invalidation: callbacks stop before any lock-bound work.
    pub(crate) fn invalidate(&self) { self.entry().health().mark(InertReason::Invalidated); }

    /// Logical invalidation plus native cleanup. Idempotent.
    pub(crate) fn release(&self) -> UnsubscribeReport {
        let entry = self.entry();
        entry.health().mark(InertReason::Unsubscribed);
        if self.released.swap(true, Ordering::SeqCst) {
            return UnsubscribeReport {
                native: NativeCleanup::NotNeeded,
            };
        }
        self.service.unsubscribe(entry.id()).unwrap_or(UnsubscribeReport {
            native: NativeCleanup::NotNeeded,
        })
    }

    pub(crate) fn is_released(&self) -> bool { self.released.load(Ordering::SeqCst) }
}

impl Subscription {
    fn new(service: Arc<InputService>, entry: EntryRef) -> Self {
        Self {
            inner: Arc::new(SubscriptionInner {
                service,
                entry: Mutex::new(entry),
                released: AtomicBool::new(false),
            }),
        }
    }

    pub(crate) fn inner(&self) -> Arc<SubscriptionInner> { Arc::clone(&self.inner) }

    /// The current opaque ID. It changes when priority or filter change.
    #[must_use]
    pub fn id(&self) -> SubscriptionId { self.inner.entry().id() }

    /// Why callbacks stopped, if they did.
    #[must_use]
    pub fn inert_reason(&self) -> Option<InertReason> { self.inner.entry().health().reason() }

    /// Changes the handler priority; the old ID becomes stale and a replacement entry is published.
    ///
    /// # Errors
    /// Returns an [`InputError`]; the subscription is unchanged on failure.
    pub fn set_priority(&self, priority: InputPriority) -> Result<(), InputError> {
        self.update(Some(priority), None)
    }

    /// Changes the handler filter; the old ID becomes stale and a replacement entry is published.
    ///
    /// # Errors
    /// Returns an [`InputError`]; the subscription is unchanged on failure.
    pub fn set_filter(&self, filter: InputFilter) -> Result<(), InputError> {
        self.update(None, Some(filter))
    }

    fn update(
        &self,
        priority: Option<InputPriority>,
        filter: Option<InputFilter>,
    ) -> Result<(), InputError> {
        if self.inner.is_released() {
            return Err(InputError::StaleSubscription);
        }
        let id = self.id();
        let (_, entry) = self.inner.service.update_handler(id, priority, filter)?;
        *self.inner.entry.lock().unwrap_or_else(PoisonError::into_inner) = entry;
        Ok(())
    }

    /// Ends the subscription and reports native cleanup.
    #[must_use]
    pub fn unsubscribe(self) -> UnsubscribeReport { self.inner.release() }
}

impl Drop for Subscription {
    fn drop(&mut self) { let _ = self.inner.release(); }
}

impl Drop for InputService {
    fn drop(&mut self) { self.router.removal.cancel(); }
}

impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subscription").field("id", &self.id()).finish()
    }
}
