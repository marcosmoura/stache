//! [`MenuControl`]: the handle combining the AX owner, the presenter, geometry
//! and a tracking-compatible main dispatcher.
//!
//! ```text
//! caller thread ── show ──dispatch──> main closure ─ checks, geometry, prepare ─ ack ─ track (nested)
//!               ── cancel ─dispatch─> main closure ─ cancelTracking(exact menu) ─ ack
//!               ── release ─dispatch─> main closure ─ remove native state ─ ack
//!                          └─ AxOwner::release_within ─ ack
//! selection (main callback) ─ lease + ticket ─> MenuEvent::Selected ─> worker ─ invoke
//! ```
//!
//! Every wait happens on the calling (non-main) thread with a fresh
//! acknowledgement window; nothing waits on main. A timeout is a typed
//! *pending* result, never success; the queued main job keeps running and is
//! idempotent, so retrying is safe. Every main closure names its
//! [`PresentationId`], so a late completion can only touch its own
//! presentation and never a successor. `Drop` is best effort (no waiting) and is
//! never the proof of cleanup.
//!
//! Invalidation (`cancel`, `invalidate`) is atomic and immediate: it flips flags
//! and cancels the selected ticket's gates without waiting on a held gate.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::dispatch::{DispatchError, MainDispatcher};
use super::geometry::{GeometryError, GeometrySnapshot, GeometrySource, QuartzPoint};
use super::invocation::{
    InvocationAuthority, InvocationContext, InvocationTicket, MenuInvocationOutcome,
    MenuInvocationReport, invoke,
};
use super::owner::{AxOwner, MenuReleaseOutcome};
use super::presenter::{
    CancelResult, PresenterBackend, PresenterError, RemoveResult, SelectionSink, TrackResult,
};
use super::reader::{MenuClock, MenuTarget};
use super::snapshot::{ActionToken, MenuItemKind, MenuItemSnapshot, MenuSnapshot, PresentationId};
use crate::applications::{
    ACKNOWLEDGEMENT_BUDGET, AcceptedActionToken, ApplicationObservation, CANCELLATION_QUANTUM,
};
use crate::desktop::DisplayId;

/// The caller-supplied, opaque revision authority of one presentation.
///
/// It says whether the runtime generation and captured revision are still
/// authorized. It does not revoke an accepted popup merely because newer
/// configuration exists.
pub trait PresentationAuthority: Send + Sync {
    /// Whether the presentation may still be shown and invoked.
    fn is_current(&self) -> bool;
}

/// One show request.
pub struct ShowRequest {
    /// The complete snapshot from the AX owner.
    pub snapshot: MenuSnapshot,
    /// The click in Quartz global coordinates.
    pub anchor: QuartzPoint,
    /// The geometry the anchor was captured against, when known.
    pub expected_geometry: Option<GeometrySnapshot>,
    /// The exact captured application instance.
    pub target: MenuTarget,
    /// The accepted-action token of that instance's observation.
    pub accepted: AcceptedActionToken,
    /// The generation/revision authority.
    pub authority: Arc<dyn PresentationAuthority>,
    /// Refuse to show after this instant.
    pub deadline: Instant,
}

/// The result of [`MenuControl::show`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuPresentationOutcome {
    /// Checks passed, the native menu is built and tracking begins on main.
    Shown {
        /// The presentation.
        presentation: PresentationId,
        /// The display containing the anchor.
        display: DisplayId,
    },
    /// The authority no longer covers this presentation.
    Stale,
    /// Cancelled or invalidated before tracking began.
    Cancelled,
    /// The request deadline passed before showing.
    Expired,
    /// Geometry was missing, ambiguous, changed or the anchor was outside.
    GeometryRejected(GeometryError),
    /// The native menu could not be built.
    PresenterFailure(PresenterError),
    /// The main job could not be queued.
    DispatchFailure,
    /// The main job was not acknowledged in time. It will not show if it had
    /// not started; if it had, cancellation was requested.
    AckTimeout,
    /// Another presentation is still open.
    Busy,
}

/// Events from the presenter, consumed once via [`MenuControl::take_events`].
pub enum MenuEvent {
    /// A menu action was selected. At most one per presentation.
    Selected(Selection),
    /// Tracking ended (selection, dismissal or cancellation).
    Closed(PresentationId),
}

/// A selected action with its lease and authority.
#[derive(Clone)]
pub struct Selection {
    ticket: InvocationTicket,
    authority: Arc<dyn InvocationAuthority>,
}

impl Selection {
    /// The selected token.
    #[must_use]
    pub fn token(&self) -> ActionToken { self.ticket.token() }

    /// The ticket, to cancel the invocation immediately.
    #[must_use]
    pub const fn ticket(&self) -> &InvocationTicket { &self.ticket }
}

/// The result of [`MenuControl::cancel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The cancel job ran on main and called `cancelTracking` on the exact menu.
    Acknowledged,
    /// The job ran but the menu was not tracking (never began, or already over).
    NotTracking,
    /// No acknowledgement in time; the job stays queued. Not success.
    Pending,
    /// The job could not be queued.
    DispatchFailure,
    /// No such presentation.
    Unknown,
}

/// Why cleanup is not finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupError {
    /// The native removal was not acknowledged in time.
    NativeTimeout,
    /// The native removal could not be queued.
    NativeDispatch,
    /// Tracking had not ended within the window.
    StillTracking,
    /// The AX release was not acknowledged in time.
    AxTimeout,
    /// The AX release could not be queued.
    AxDispatch,
}

/// The result of [`MenuControl::release`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupOutcome {
    /// Native state removed and every retained element released.
    Complete {
        /// How many retained elements were dropped.
        elements: usize,
    },
    /// Cleanup finished except for leased elements, which are released exactly
    /// once after their press resolves.
    LeasedPending {
        /// Dropped now.
        released: usize,
        /// Held by a lease.
        leased: usize,
    },
    /// Not finished: `cleanupPending`. Retrying is safe.
    Pending {
        /// The presentation this names.
        presentation: PresentationId,
        /// What is still outstanding.
        error: CleanupError,
    },
    /// No such presentation (already cleaned).
    Unknown,
}

/// Tunables; the default is the 2 s per-hop acknowledgement window.
#[derive(Debug, Clone, Copy)]
pub struct ControlConfig {
    /// The fresh acknowledgement window of every hop.
    pub budget: Duration,
}

impl Default for ControlConfig {
    fn default() -> Self { Self { budget: ACKNOWLEDGEMENT_BUDGET } }
}

// Presentation phases.
const QUEUED: u8 = 0;
const PREPARING: u8 = 1;
const TRACKING: u8 = 2;
const ENDED: u8 = 3;
const ABANDONED: u8 = 4;

struct PresentationState {
    id: PresentationId,
    tokens: HashSet<ActionToken>,
    target: MenuTarget,
    accepted: AcceptedActionToken,
    authority: Arc<dyn PresentationAuthority>,
    phase: AtomicU8,
    /// Do not show / stop showing.
    stop_show: AtomicBool,
    /// Invocation authority revoked (stop).
    invalidated: AtomicBool,
    selected: AtomicBool,
    ticket: Mutex<Option<InvocationTicket>>,
    native_removed: AtomicBool,
    ax_done: AtomicBool,
}

impl PresentationState {
    /// Immediate, nonblocking invalidation.
    fn invalidate(&self) {
        self.stop_show.store(true, Ordering::SeqCst);
        self.invalidated.store(true, Ordering::SeqCst);
        let ticket = self.ticket.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(ticket) = ticket {
            let _ = ticket.cancel();
        }
    }
}

impl InvocationAuthority for PresentationState {
    fn is_current(&self) -> bool {
        !self.invalidated.load(Ordering::SeqCst) && self.authority.is_current()
    }

    fn contains(&self, token: ActionToken) -> bool { self.tokens.contains(&token) }
}

fn collect_tokens(items: &[MenuItemSnapshot], out: &mut HashSet<ActionToken>) {
    for item in items {
        out.extend(item.action);
        if let MenuItemKind::Submenu(sub) = &item.kind {
            collect_tokens(&sub.items, out);
        }
    }
}

struct Inner {
    owner: Arc<AxOwner>,
    backend: Arc<dyn PresenterBackend>,
    geometry: Arc<dyn GeometrySource>,
    dispatcher: Arc<dyn MainDispatcher>,
    clock: Arc<dyn MenuClock>,
    config: ControlConfig,
    events: Sender<MenuEvent>,
    receiver: Mutex<Option<Receiver<MenuEvent>>>,
    presentations: Mutex<Vec<Arc<PresentationState>>>,
    active: Mutex<Option<PresentationId>>,
}

impl Inner {
    fn states(&self) -> std::sync::MutexGuard<'_, Vec<Arc<PresentationState>>> {
        self.presentations.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn find(&self, id: PresentationId) -> Option<Arc<PresentationState>> {
        self.states().iter().find(|state| state.id == id).cloned()
    }

    fn forget(&self, id: PresentationId) { self.states().retain(|state| state.id != id); }

    fn clear_active(&self, id: PresentationId) {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if *active == Some(id) {
            *active = None;
        }
    }
}

/// The presenter control handle. `Send + Sync`; holds no native object.
pub struct MenuControl {
    inner: Arc<Inner>,
}

type ShowAck = Result<DisplayId, MenuPresentationOutcome>;

impl MenuControl {
    /// Creates a control over the given adapters.
    #[must_use]
    pub fn new(
        owner: Arc<AxOwner>,
        backend: Arc<dyn PresenterBackend>,
        geometry: Arc<dyn GeometrySource>,
        dispatcher: Arc<dyn MainDispatcher>,
        clock: Arc<dyn MenuClock>,
        config: ControlConfig,
    ) -> Self {
        let (events, receiver) = mpsc::channel();
        Self {
            inner: Arc::new(Inner {
                owner,
                backend,
                geometry,
                dispatcher,
                clock,
                config,
                events,
                receiver: Mutex::new(Some(receiver)),
                presentations: Mutex::new(Vec::new()),
                active: Mutex::new(None),
            }),
        }
    }

    /// The AX owner (for reads).
    #[must_use]
    pub fn owner(&self) -> &Arc<AxOwner> { &self.inner.owner }

    /// Takes the event stream; `None` after the first call.
    #[must_use]
    pub fn take_events(&self) -> Option<Receiver<MenuEvent>> {
        self.inner.receiver.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    /// Shows the menu. Blocks at most one acknowledgement window; never call
    /// from main. `Shown` means the main job passed every check and tracking
    /// begins immediately after the acknowledgement.
    #[must_use]
    pub fn show(&self, request: ShowRequest) -> MenuPresentationOutcome {
        let inner = &self.inner;
        let id = request.snapshot.presentation;
        if !request.authority.is_current() {
            return MenuPresentationOutcome::Stale;
        }
        if inner.clock.now() >= request.deadline {
            return MenuPresentationOutcome::Expired;
        }
        {
            let mut active = inner.active.lock().unwrap_or_else(PoisonError::into_inner);
            if active.is_some() || inner.find(id).is_some() {
                return MenuPresentationOutcome::Busy;
            }
            *active = Some(id);
        }
        let mut tokens = HashSet::new();
        collect_tokens(&request.snapshot.items, &mut tokens);
        let state = Arc::new(PresentationState {
            id,
            tokens,
            target: request.target,
            accepted: request.accepted,
            authority: Arc::clone(&request.authority),
            phase: AtomicU8::new(QUEUED),
            stop_show: AtomicBool::new(false),
            invalidated: AtomicBool::new(false),
            selected: AtomicBool::new(false),
            ticket: Mutex::new(None),
            native_removed: AtomicBool::new(false),
            ax_done: AtomicBool::new(false),
        });
        inner.states().push(Arc::clone(&state));
        let (tx, rx) = mpsc::sync_channel::<ShowAck>(1);
        let job = {
            let inner = Arc::clone(inner);
            let state = Arc::clone(&state);
            let ShowRequest {
                snapshot,
                anchor,
                expected_geometry,
                deadline,
                ..
            } = request;
            move || {
                show_on_main(
                    &inner,
                    &state,
                    &snapshot,
                    anchor,
                    expected_geometry.as_ref(),
                    deadline,
                    &tx,
                );
            }
        };
        if inner.dispatcher.dispatch(Box::new(job)).is_err() {
            inner.forget(id);
            inner.clear_active(id);
            return MenuPresentationOutcome::DispatchFailure;
        }
        match rx.recv_timeout(inner.config.budget) {
            Ok(Ok(display)) => MenuPresentationOutcome::Shown { presentation: id, display },
            Ok(Err(outcome)) => {
                inner.forget(id);
                inner.clear_active(id);
                outcome
            }
            Err(_) => {
                // Race the main job for the phase. Winning abandons it before
                // it can show; losing means it started, so stop it showing.
                state.stop_show.store(true, Ordering::SeqCst);
                let abandoned = state
                    .phase
                    .compare_exchange(QUEUED, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok();
                if abandoned {
                    inner.forget(id);
                    inner.clear_active(id);
                } else {
                    // Possibly tracking already: ask for exact cancellation.
                    let _ = self.dispatch_cancel(&state);
                }
                MenuPresentationOutcome::AckTimeout
            }
        }
    }

    fn dispatch_cancel(
        &self,
        state: &Arc<PresentationState>,
    ) -> Result<Receiver<CancelResult>, DispatchError> {
        let (tx, rx) = mpsc::sync_channel(1);
        let backend = Arc::clone(&self.inner.backend);
        let id = state.id;
        self.inner
            .dispatcher
            .dispatch(Box::new(move || {
                let _ = tx.send(backend.cancel_tracking(id));
            }))
            .map(|()| rx)
    }

    /// Stops the presentation: atomic, immediate invalidation (including the
    /// selected ticket), then exact `cancelTracking` on main with a fresh
    /// acknowledgement window.
    #[must_use]
    pub fn cancel(&self, presentation: PresentationId) -> CancelOutcome {
        let Some(state) = self.inner.find(presentation) else {
            return CancelOutcome::Unknown;
        };
        state.invalidate();
        self.cancel_native(&state)
    }

    /// Invalidates without any native request or wait.
    pub fn invalidate(&self, presentation: PresentationId) {
        if let Some(state) = self.inner.find(presentation) {
            state.invalidate();
        }
    }

    fn cancel_native(&self, state: &Arc<PresentationState>) -> CancelOutcome {
        let Ok(rx) = self.dispatch_cancel(state) else {
            return CancelOutcome::DispatchFailure;
        };
        match rx.recv_timeout(self.inner.config.budget) {
            Ok(CancelResult::Requested) => CancelOutcome::Acknowledged,
            Ok(CancelResult::NotTracking | CancelResult::Unknown) => CancelOutcome::NotTracking,
            Err(_) => CancelOutcome::Pending,
        }
    }

    /// Cleans up one presentation: exact tracking cancellation, confirmed
    /// native removal, then AX release, each with a fresh acknowledgement window
    /// independent of any cancelled lifecycle token. Idempotent and safe to
    /// retry; completed parts are not repeated. A selected action's lease is
    /// *not* revoked here (use [`Self::cancel`] to stop it).
    #[must_use]
    pub fn release(&self, presentation: PresentationId) -> CleanupOutcome {
        let inner = &self.inner;
        let Some(state) = inner.find(presentation) else {
            return CleanupOutcome::Unknown;
        };
        state.stop_show.store(true, Ordering::SeqCst);
        let pending = |error| CleanupOutcome::Pending { presentation, error };
        if !state.native_removed.load(Ordering::SeqCst) {
            if let Err(error) = self.remove_native(&state) {
                return pending(error);
            }
            state.native_removed.store(true, Ordering::SeqCst);
        }
        let outcome = if state.ax_done.load(Ordering::SeqCst) {
            MenuReleaseOutcome::Unknown
        } else {
            inner.owner.release_within(presentation, inner.config.budget)
        };
        let result = match outcome {
            MenuReleaseOutcome::Released { elements } => CleanupOutcome::Complete { elements },
            MenuReleaseOutcome::Unknown => CleanupOutcome::Complete { elements: 0 },
            MenuReleaseOutcome::Deferred { released, leased } => {
                CleanupOutcome::LeasedPending { released, leased }
            }
            MenuReleaseOutcome::Pending => return pending(CleanupError::AxTimeout),
            MenuReleaseOutcome::DispatchFailure => return pending(CleanupError::AxDispatch),
        };
        state.ax_done.store(true, Ordering::SeqCst);
        inner.forget(presentation);
        result
    }

    /// Cancels tracking and removes the native state, retrying `StillTracking`
    /// within one window (the nested loop needs a moment to unwind).
    fn remove_native(&self, state: &Arc<PresentationState>) -> Result<(), CleanupError> {
        let inner = &self.inner;
        let started = Instant::now();
        match self.cancel_native(state) {
            CancelOutcome::DispatchFailure => return Err(CleanupError::NativeDispatch),
            CancelOutcome::Pending => return Err(CleanupError::NativeTimeout),
            CancelOutcome::Acknowledged | CancelOutcome::NotTracking | CancelOutcome::Unknown => {}
        }
        loop {
            let (tx, rx) = mpsc::sync_channel(1);
            let backend = Arc::clone(&inner.backend);
            let id = state.id;
            inner
                .dispatcher
                .dispatch(Box::new(move || {
                    let _ = tx.send(backend.remove(id));
                }))
                .map_err(|_| CleanupError::NativeDispatch)?;
            let remaining = inner.config.budget.saturating_sub(started.elapsed());
            match rx.recv_timeout(remaining.max(CANCELLATION_QUANTUM)) {
                Ok(RemoveResult::Removed | RemoveResult::Unknown) => return Ok(()),
                Ok(RemoveResult::StillTracking) => {
                    if started.elapsed() >= inner.config.budget {
                        return Err(CleanupError::StillTracking);
                    }
                    thread::sleep(CANCELLATION_QUANTUM);
                }
                Err(_) => return Err(CleanupError::NativeTimeout),
            }
        }
    }

    /// Runs the activation-then-press flow for a selection. Blocks (bounded per
    /// hop); never call from main.
    #[must_use]
    pub fn invoke(
        &self,
        selection: &Selection,
        observation: &ApplicationObservation,
        deadline: Instant,
    ) -> MenuInvocationReport {
        let context = InvocationContext {
            observation,
            owner: &self.inner.owner,
            clock: self.inner.clock.as_ref(),
            authority: selection.authority.as_ref(),
            budget: self.inner.config.budget,
        };
        invoke(&context, &selection.ticket, deadline)
    }
}

impl Drop for MenuControl {
    /// Best effort only: invalidates, queues native removal and AX release and
    /// returns without waiting. Not proof of cleanup.
    fn drop(&mut self) {
        let states: Vec<_> = self.inner.states().clone();
        for state in states {
            state.invalidate();
            let backend = Arc::clone(&self.inner.backend);
            let id = state.id;
            let _ = self.inner.dispatcher.dispatch(Box::new(move || {
                let _ = backend.cancel_tracking(id);
            }));
            remove_eventually(&self.inner.dispatcher, &self.inner.backend, id, 100);
            let _ = self.inner.owner.submit_release(id, |_| {});
        }
    }
}

/// Queues a native removal that re-queues itself while tracking has not yet
/// unwound (best effort, never waits).
fn remove_eventually(
    dispatcher: &Arc<dyn MainDispatcher>,
    backend: &Arc<dyn PresenterBackend>,
    id: PresentationId,
    attempts: u32,
) {
    let (next_dispatcher, next_backend) = (Arc::clone(dispatcher), Arc::clone(backend));
    let _ = dispatcher.dispatch(Box::new(move || {
        if next_backend.remove(id) == RemoveResult::StillTracking && attempts > 0 {
            remove_eventually(&next_dispatcher, &next_backend, id, attempts - 1);
        }
    }));
}

/// The main-thread side of [`MenuControl::show`].
fn show_on_main(
    inner: &Arc<Inner>,
    state: &Arc<PresentationState>,
    snapshot: &MenuSnapshot,
    anchor: QuartzPoint,
    expected: Option<&GeometrySnapshot>,
    deadline: Instant,
    ack: &SyncSender<ShowAck>,
) {
    let id = state.id;
    if state
        .phase
        .compare_exchange(QUEUED, PREPARING, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return; // abandoned by a timed-out caller
    }
    let fail = |outcome: MenuPresentationOutcome| {
        state.phase.store(ENDED, Ordering::SeqCst);
        inner.clear_active(id);
        let _ = ack.send(Err(outcome));
    };
    if state.stop_show.load(Ordering::SeqCst) || state.invalidated.load(Ordering::SeqCst) {
        return fail(MenuPresentationOutcome::Cancelled);
    }
    if !InvocationAuthority::is_current(state.as_ref()) {
        return fail(MenuPresentationOutcome::Stale);
    }
    if inner.clock.now() >= deadline {
        return fail(MenuPresentationOutcome::Expired);
    }
    let placement = match inner
        .geometry
        .snapshot()
        .and_then(|geometry| geometry.place_checked(expected, anchor))
    {
        Ok(placement) => placement,
        Err(error) => return fail(MenuPresentationOutcome::GeometryRejected(error)),
    };
    let sink = selection_sink(inner, state);
    if let Err(error) = inner.backend.prepare(id, snapshot, sink) {
        return fail(MenuPresentationOutcome::PresenterFailure(error));
    }
    if state.stop_show.load(Ordering::SeqCst) {
        // Prepared natively but never shown: the caller forgets this
        // presentation, so nothing else would ever remove the native entry.
        let _ = inner.backend.remove(id);
        return fail(MenuPresentationOutcome::Cancelled);
    }
    state.phase.store(TRACKING, Ordering::SeqCst);
    let _ = ack.send(Ok(placement.display));
    // A cancel that raced the acknowledgement must not open the menu.
    if !state.stop_show.load(Ordering::SeqCst) {
        let _: TrackResult = inner.backend.track(id, placement.location);
    }
    state.phase.store(ENDED, Ordering::SeqCst);
    inner.clear_active(id);
    let _ = inner.events.send(MenuEvent::Closed(id));
}

/// The Cocoa action callback target: lease, ticket and enqueue only.
fn selection_sink(inner: &Arc<Inner>, state: &Arc<PresentationState>) -> SelectionSink {
    let inner = Arc::clone(inner);
    let state = Arc::clone(state);
    Arc::new(move |token| {
        if !state.tokens.contains(&token)
            || state.invalidated.load(Ordering::SeqCst)
            || !state.authority.is_current()
        {
            return; // foreign, stale or revoked: dropped
        }
        if state.selected.swap(true, Ordering::SeqCst) {
            return; // double selection: the second is dropped
        }
        let lease = inner.owner.lease(token);
        let ticket = InvocationTicket::new(state.target.application, state.accepted, lease);
        *state.ticket.lock().unwrap_or_else(PoisonError::into_inner) = Some(ticket.clone());
        let authority: Arc<dyn InvocationAuthority> = Arc::clone(&state) as _;
        let _ = inner.events.send(MenuEvent::Selected(Selection { ticket, authority }));
    })
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<MenuControl>();
    assert_send_sync::<Selection>();
    assert_send_sync::<MenuInvocationOutcome>();
};
