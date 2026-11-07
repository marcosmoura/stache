//! The Hold-to-Quit callback decision and the state it shares with the worker.
//!
//! Everything here runs on the input tap's callback thread or is read by it, so
//! it obeys the shared service's contract: atomics, one nonblocking
//! [`FrontmostWatch::snapshot`], `event.context()`, a `try_lock` on a bounded
//! preallocated inbox and a thread unpark. No logging, allocation, native or
//! application query, main dispatch or blocking wait.
//!
//! # Callback decision table
//!
//! | Event                                     | Condition                                                                                      | Result                                              |
//! | ----------------------------------------- | ---------------------------------------------------------------------------------------------- | --------------------------------------------------- |
//! | first nonrepeat `Q` down (Command-only)   | accepting, HUD ready, armed revision == observed revision                                      | gates below, else `Pass`                            |
//! |                                           | module snapshot `eligible_application()` is `Some` (confirmed, not Finder, not this process)    | else `Pass` (unconfirmed, exempt, unknown)          |
//! |                                           | SDK context is `Known`, no exemption bits, same bundle identifier                              | else `Pass` (stale, unknown, exempt, mismatch)      |
//! |                                           | worker running and the inbox accepted the down                                                  | else `Pass` and the press is cancelled              |
//! | all gates hold                            | fresh `PressId` stored with `set_press_state`                                                  | `Suppress`                                          |
//! | `Q` repeat                                | flags no longer exactly Command                                                                | cancel the press, `Suppress` (ledger)               |
//! | `Q` up                                    | always                                                                                         | cancel the press, `Suppress` (ledger)               |
//! | `FlagsChanged` of a named modifier key    | flags no longer exactly Command                                                                | cancel the claimed press, `Pass`                    |
//! | `on_cancel` (tap disabled)                | always                                                                                         | cancel the claimed press                            |
//! | frontmost activation                      | epoch moved past the claimed press's epoch                                                     | cancel the claimed press                            |
//!
//! Pairing is the shared ledger's job: a passed down never acquires a
//! suppressor mid-press, and repeats and up of a claimed press stay suppressed
//! through the up, including after the handler goes inert.

#![allow(clippy::redundant_pub_crate)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError};
use std::thread::Thread;
use std::time::{Duration, Instant};

use stache_macos::hold_hud::PressId as HudPressId;
use stache_macos::{
    AcceptedActionToken, ApplicationId, FrontmostListener, FrontmostSnapshot, FrontmostWatch,
    PressAuthorization,
};
use stache_sdk::input::{
    CallbackScope, Disposition, FrontmostState, InputEvent, InputEventKind, InputFilter,
    InputHandler, KeyCode, KindSet, ModifierFlags, ModifierMatch, PressId, PressStream,
};

use crate::config::CommandQuitConfig;

/// Physical key code of `Q`.
pub const KEY_Q: u16 = 12;

/// Every key the filter names: `Q` plus left/right Command, Shift, Option and
/// Control. Caps Lock (57) is deliberately absent: Keybindings' Caps handler
/// owns it.
pub const HOLD_KEYS: [u16; 9] = [KEY_Q, 54, 55, 56, 58, 59, 60, 61, 62];

/// How many claimed downs the inbox holds. Only one physical `Q` press can be
/// live, so a handful is generous; a full inbox fails open.
pub(crate) const INBOX_CAPACITY: usize = 8;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The module's injected time source and wait point.
///
/// The worker reads time and parks through it, so a test advances fake time and
/// observes every wait without sleeping.
pub trait HoldClock: Send + Sync {
    /// The current monotonic instant.
    fn now(&self) -> Instant;

    /// Parks the calling worker until it is unparked or `timeout` elapses on
    /// this clock. `None` is an indefinite wait. Spurious returns are allowed:
    /// the worker rechecks all state.
    fn park(&self, timeout: Option<Duration>);
}

/// The real clock: [`Instant::now`] and [`std::thread::park`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemHoldClock;

impl HoldClock for SystemHoldClock {
    fn now(&self) -> Instant { Instant::now() }

    fn park(&self, timeout: Option<Duration>) {
        match timeout {
            Some(timeout) => std::thread::park_timeout(timeout),
            None => std::thread::park(),
        }
    }
}

/// Whether `flags` are exactly Command, ignoring Caps Lock.
///
/// Fn, Shift, Option and Control all make this `false`, which is also what the
/// `Exact` filter reservation matches.
#[must_use]
pub const fn is_command_only(flags: ModifierFlags) -> bool {
    flags.normalized().bits() == ModifierFlags::COMMAND.bits()
}

/// The input filter: kinds `KeyDown | KeyUp | FlagsChanged`, the [`HOLD_KEYS`]
/// and `ModifierMatch::Exact(COMMAND)`.
///
/// `Exact` ignores Caps Lock and left/right. `FlagsChanged` for a named key
/// always reaches the handler (shipped filter rule), so a Command release or an
/// added Shift/Option/Control mid-hold is seen. `Exact` is required because the
/// shipped reservation registry derives Carbon yields from this filter, and
/// `AllOf` would also silence ordinary Cmd+Shift+Q-style bindings.
#[must_use]
pub fn filter() -> InputFilter {
    InputFilter::new(
        KindSet::of(InputEventKind::KeyDown)
            .with(InputEventKind::KeyUp)
            .with(InputEventKind::FlagsChanged),
    )
    .with_keys(HOLD_KEYS.map(KeyCode))
    .with_modifiers(ModifierMatch::Exact(ModifierFlags::COMMAND))
}

/// The checked deadline of a hold: `down + hold_ms`, or `None` when it is not
/// representable (a cancellable indefinite wait).
#[must_use]
pub fn deadline(down: Instant, hold_ms: u64) -> Option<Instant> {
    down.checked_add(Duration::from_millis(hold_ms))
}

/// Why a first `Q` down is passed through. Plain data: deciding allocates
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassReason {
    /// The runtime is not accepting (starting, reconfiguring, stopping).
    NotAccepting,
    /// The HUD is not ready (degraded or stopping).
    HudNotReady,
    /// The armed revision is not the observed published revision.
    RevisionNotArmed,
    /// The module's own snapshot has no eligible application: unconfirmed,
    /// unknown, stale, Finder or Stache itself.
    ApplicationNotEligible,
    /// The SDK context is `Stale` or `Unknown`.
    ContextNotKnown,
    /// The SDK context carries an exemption bit.
    ContextExempt,
    /// The SDK context names a different bundle than the module snapshot.
    BundleMismatch,
    /// The worker is not running.
    NoWorker,
    /// The bounded inbox refused the down.
    InboxRefused,
}

/// The facts one first down is decided on.
#[derive(Clone, Copy)]
pub(crate) struct ClaimInputs<'a> {
    pub accepting: bool,
    pub hud_ready: bool,
    pub armed: u64,
    pub observed: u64,
    pub snapshot: &'a FrontmostSnapshot,
    pub context: &'a FrontmostState,
}

/// The exact instance and activation epoch a claimed press captured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub application: ApplicationId,
    pub epoch: u64,
}

/// Both frontmost gates must agree; any disagreement fails open.
pub(crate) fn decide(inputs: &ClaimInputs<'_>) -> Result<Target, PassReason> {
    if !inputs.accepting {
        return Err(PassReason::NotAccepting);
    }
    if !inputs.hud_ready {
        return Err(PassReason::HudNotReady);
    }
    if inputs.armed == 0 || inputs.armed != inputs.observed {
        return Err(PassReason::RevisionNotArmed);
    }
    let Some(application) = inputs.snapshot.eligible_application() else {
        return Err(PassReason::ApplicationNotEligible);
    };
    let FrontmostState::Known(app) = inputs.context else {
        return Err(PassReason::ContextNotKnown);
    };
    if app.exemptions.0 != 0 {
        return Err(PassReason::ContextExempt);
    }
    if inputs.snapshot.bundle_identifier.as_deref() != Some(&*app.bundle_id) {
        return Err(PassReason::BundleMismatch);
    }
    Ok(Target {
        application,
        epoch: inputs.snapshot.epoch,
    })
}

/// One claimed down, queued for the worker.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Down {
    pub press: u64,
    pub application: ApplicationId,
    pub epoch: u64,
    pub at: Instant,
    pub revision: u64,
}

/// A candidate staged by `reconfigure`, waiting for its accepted publication.
#[derive(Clone, Debug)]
pub(crate) struct Staged {
    pub config: CommandQuitConfig,
    pub token: AcceptedActionToken,
}

/// The state shared by the callback, the worker and the lifecycle paths of one
/// run.
///
/// Press identifiers are positive and strictly increasing, and `0` means none.
/// "Cancelled" is the monotone `cancelled_through` mark: a press is cancelled
/// iff its identifier is at or below it, so a cancel can never be lost to queue
/// capacity and a later press is never affected by an earlier cancel.
pub(crate) struct Shared {
    /// Whether new presses may be claimed.
    pub accepting: AtomicBool,
    /// Whether the HUD is installed and healthy.
    pub hud_ready: AtomicBool,
    /// The revision the worker armed; `0` is none.
    pub armed_revision: AtomicU64,
    /// The newest accepted revision the change listener saw.
    pub observed_revision: AtomicU64,
    next_press: AtomicU64,
    current: AtomicU64,
    current_epoch: AtomicU64,
    cancelled_through: AtomicU64,
    inbox: Mutex<VecDeque<Down>>,
    authorization: Mutex<Option<(u64, PressAuthorization)>>,
    worker: OnceLock<Thread>,
    /// The candidate staged by `reconfigure`.
    pub staged: Mutex<Option<Staged>>,
}

impl Shared {
    pub(crate) fn new() -> Self {
        Self {
            accepting: AtomicBool::new(false),
            hud_ready: AtomicBool::new(false),
            armed_revision: AtomicU64::new(0),
            observed_revision: AtomicU64::new(0),
            next_press: AtomicU64::new(0),
            current: AtomicU64::new(0),
            current_epoch: AtomicU64::new(0),
            cancelled_through: AtomicU64::new(0),
            inbox: Mutex::new(VecDeque::with_capacity(INBOX_CAPACITY)),
            authorization: Mutex::new(None),
            worker: OnceLock::new(),
            staged: Mutex::new(None),
        }
    }

    /// Registers the worker thread; wakes before this are not needed because
    /// the worker rechecks everything first.
    pub(crate) fn set_worker(&self, thread: Thread) { let _ = self.worker.set(thread); }

    /// Whether the worker is registered.
    pub(crate) fn has_worker(&self) -> bool { self.worker.get().is_some() }

    /// Wakes the worker. An unpark before the park makes the park return, so no
    /// wake is lost.
    pub(crate) fn wake(&self) {
        if let Some(worker) = self.worker.get() {
            worker.unpark();
        }
    }

    /// Whether `press` was cancelled.
    pub(crate) fn is_cancelled(&self, press: u64) -> bool {
        self.cancelled_through.load(Ordering::SeqCst) >= press
    }

    /// Callback-safe cancel of `press` and everything older: sets the mark, then
    /// cancels a published authorization when the slot is free, then wakes the
    /// worker. The worker rechecks the mark after it publishes an authorization,
    /// so a contended slot cannot lose the cancel.
    pub(crate) fn cancel(&self, press: u64) {
        self.cancelled_through.fetch_max(press, Ordering::SeqCst);
        match self.authorization.try_lock() {
            Ok(slot) => cancel_published(slot.as_ref(), press),
            Err(TryLockError::Poisoned(poisoned)) => {
                cancel_published(poisoned.into_inner().as_ref(), press);
            }
            // The worker is publishing; it rechecks the mark afterwards.
            Err(TryLockError::WouldBlock) => {}
        }
        self.wake();
    }

    /// Cancels the live claimed press, if any (flags change, tap disable,
    /// activation).
    pub(crate) fn cancel_current(&self) {
        let press = self.current.load(Ordering::SeqCst);
        if press != 0 {
            self.cancel(press);
        }
    }

    /// Lifecycle cancel of every press claimed so far, with a blocking lock on
    /// the authorization slot. Not for the callback.
    pub(crate) fn cancel_all(&self) {
        let through = self.next_press.load(Ordering::SeqCst);
        self.cancelled_through.fetch_max(through, Ordering::SeqCst);
        if let Some((_, authorization)) = lock(&self.authorization).as_ref() {
            let _ = authorization.cancel();
        }
        self.wake();
    }

    /// Publishes the one-shot authorization for `press`, then rechecks the
    /// cancel mark so a cancel that raced the publication is applied.
    pub(crate) fn publish_authorization(&self, press: u64, authorization: &PressAuthorization) {
        *lock(&self.authorization) = Some((press, authorization.clone()));
        if self.is_cancelled(press) {
            let _ = authorization.cancel();
        }
    }

    /// Drops the authorization of `press`.
    pub(crate) fn clear_authorization(&self, press: u64) {
        let mut slot = lock(&self.authorization);
        if slot.as_ref().is_some_and(|(published, _)| *published == press) {
            *slot = None;
        }
    }

    /// Pops the oldest claimed down.
    pub(crate) fn pop(&self) -> Option<Down> { lock(&self.inbox).pop_front() }

    /// Nonblocking push; `false` means contended or full.
    fn try_push(&self, down: Down) -> bool {
        let mut inbox = match self.inbox.try_lock() {
            Ok(inbox) => inbox,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return false,
        };
        if inbox.len() >= INBOX_CAPACITY {
            return false;
        }
        inbox.push_back(down);
        true
    }

    /// The frontmost activation path: a press whose epoch the activation moved
    /// past is cancelled.
    fn activation(&self, epoch: u64) {
        let press = self.current.load(Ordering::SeqCst);
        if press != 0 && epoch > self.current_epoch.load(Ordering::SeqCst) {
            self.cancel(press);
        }
    }
}

/// Cancels the published authorization when it belongs to `press` or older.
fn cancel_published(published: Option<&(u64, PressAuthorization)>, press: u64) {
    if let Some((owner, authorization)) = published
        && *owner <= press
    {
        let _ = authorization.cancel();
    }
}

/// Cancels the claimed press when a workspace activation moves the epoch.
pub(crate) struct ActivationCancel(pub Arc<Shared>);

impl FrontmostListener for ActivationCancel {
    fn invalidated(&self, epoch: u64) { self.0.activation(epoch); }

    fn published(&self, _snapshot: &FrontmostSnapshot) {}
}

/// The suppressing handler.
pub(crate) struct HoldHandler {
    shared: Arc<Shared>,
    watch: FrontmostWatch,
    clock: Arc<dyn HoldClock>,
}

impl HoldHandler {
    pub(crate) fn new(
        shared: Arc<Shared>,
        watch: FrontmostWatch,
        clock: Arc<dyn HoldClock>,
    ) -> Self {
        Self { shared, watch, clock }
    }

    fn claim(&self, event: &InputEvent, scope: &mut CallbackScope) -> Disposition {
        let shared = &self.shared;
        let snapshot = self.watch.snapshot();
        let inputs = ClaimInputs {
            accepting: shared.accepting.load(Ordering::SeqCst),
            hud_ready: shared.hud_ready.load(Ordering::SeqCst),
            armed: shared.armed_revision.load(Ordering::SeqCst),
            observed: shared.observed_revision.load(Ordering::SeqCst),
            snapshot: &snapshot,
            context: event.context().frontmost(),
        };
        let Ok(target) = decide(&inputs) else {
            return Disposition::Pass;
        };
        if !shared.has_worker() {
            return Disposition::Pass;
        }
        // A fresh identifier for every physical press. An older press that never
        // saw its up is superseded and cancelled.
        let press = shared.next_press.fetch_add(1, Ordering::SeqCst) + 1;
        shared.cancel(press - 1);
        shared.current_epoch.store(target.epoch, Ordering::SeqCst);
        shared.current.store(press, Ordering::SeqCst);
        // Recheck after claiming: a stop or reconfigure that cleared `accepting`
        // between the gate and the claim leaves this press cancelled and passed.
        if !shared.accepting.load(Ordering::SeqCst) {
            shared.cancel(press);
            return Disposition::Pass;
        }
        let down = Down {
            press,
            application: target.application,
            epoch: target.epoch,
            at: self.clock.now(),
            revision: inputs.armed,
        };
        if !shared.try_push(down) {
            shared.cancel(press);
            return Disposition::Pass;
        }
        scope.set_press_state(press);
        shared.wake();
        Disposition::Suppress
    }
}

impl InputHandler for HoldHandler {
    fn on_event(&self, event: &InputEvent, scope: &mut CallbackScope) -> Disposition {
        match event.kind() {
            InputEventKind::FlagsChanged => {
                if !is_command_only(event.flags()) {
                    self.shared.cancel_current();
                }
                Disposition::Pass
            }
            InputEventKind::KeyDown | InputEventKind::KeyUp
                if event.stream() == PressStream::Key(KeyCode(KEY_Q)) =>
            {
                let press = scope.press_state();
                if event.kind() == InputEventKind::KeyDown {
                    if scope.is_first_down() && !event.repeat() {
                        return self.claim(event, scope);
                    }
                    if press != 0 && !is_command_only(event.flags()) {
                        self.shared.cancel(press);
                    }
                } else if press != 0 {
                    self.shared.cancel(press);
                }
                // The ledger owns pairing; a claimed press stays suppressed.
                Disposition::Suppress
            }
            _ => Disposition::Pass,
        }
    }

    fn on_cancel(&self, _press: PressId) { self.shared.cancel_current(); }
}

/// The HUD authorization predicate: the press is not cancelled, the runtime is
/// still accepting and the captured activation epoch is current. Pure atomics,
/// evaluated on main at commit and before ordering front.
pub(crate) fn hud_predicate(
    shared: &Arc<Shared>,
    watch: &FrontmostWatch,
    press: u64,
    epoch: u64,
) -> impl Fn() -> bool + Send + Sync + 'static {
    let shared = Arc::clone(shared);
    let watch = watch.clone();
    move || {
        !shared.is_cancelled(press)
            && shared.accepting.load(Ordering::SeqCst)
            && watch.is_epoch_current(epoch)
    }
}

/// The HUD identifier of a claimed press.
pub(crate) const fn hud_press(press: u64) -> HudPressId { HudPressId::new(press) }
