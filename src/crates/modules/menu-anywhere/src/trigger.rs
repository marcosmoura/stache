//! The `MenuAnywhere` trigger: the pure filter, the callback decision and the
//! nonblocking admission state the callback shares with the worker.
//!
//! Everything here runs on the input tap's callback thread or is read by it, so
//! it obeys the shared service's contract: one `try_read` of an immutable
//! [`Published`] tuple, atomics, one bounded `try_send` and the injected
//! monotonic clock. No logging, allocation, native or application query, main
//! dispatch, lifecycle call or blocking wait.
//!
//! # Matching
//!
//! The service filter is only a prefilter (`MouseDown`, the one button number,
//! `AllOf` the required modifiers). The callback always repeats the whole
//! decision against its published tuple, so a filter swap that briefly runs
//! ahead of or behind the tuple can only pass an event, never mix a new
//! modifier set with an old button or revision.
//!
//! * `right_click` is button number 1 and `middle_click` is button number 2 only.
//!   The macOS adapter classifies `rightMouseDown`/`otherMouseDown` into the
//!   same `MouseDown` kind with the native button number and drops any event
//!   whose type contradicts its button (left is 0, right is 1, other is 2 or
//!   more), so the button number alone is enough here: a left click, a
//!   mismatched event and buttons 3 and above never reach the handler as a
//!   match.
//! * Modifiers compare the four supported bits exactly. Unrelated flags (Caps
//!   Lock, Fn) are dropped; an extra supported modifier rejects; `[]` matches a
//!   click with no supported modifier held.
//!
//! # Admission table
//!
//! | Condition                                                          | Decision   | Outcome                                  |
//! | ------------------------------------------------------------------ | ---------- | ---------------------------------------- |
//! | not a `MouseDown`, wrong button or flags, ineligible, dead run     | `Pass`     | none                                     |
//! | eligible match, free reservation and queue slot                    | `Suppress` | one request                              |
//! | eligible match, reservation held (queued/reading/showing/invoking) | `Suppress` | [`Outcome::Coalesced`], newest dropped   |
//! | eligible match, reservation won, queue slot full                   | `Pass`     | [`Outcome::QueueFull`], reservation freed |
//! | eligible match, reservation won, worker queue disconnected         | `Pass`     | [`Outcome::WorkerUnavailable`], health off |
//!
//! A trigger admitted by the callback stays consumed even if the worker later
//! fails to capture or read. A concurrent invalidation after the callback's
//! decision can consume that one already admitted click while its work is
//! cancelled: the linearization point is the callback's `Suppress`, never a
//! retroactive pass-through.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, PoisonError, RwLock, TryLockError};
use std::time::Instant;

use stache_macos::menu::{MenuClock, QuartzPoint};
use stache_sdk::input::{
    CallbackScope, Disposition, InputEvent, InputEventKind, InputFilter, InputHandler, KindSet,
    ModifierFlags, ModifierMatch, MouseButton, PressStream,
};

use crate::config::{MenuAnywhereConfig, MenuAnywhereMouseButton, ModifierMask};

/// Button number of the secondary (right) button.
pub const RIGHT_BUTTON: u32 = 1;
/// Button number of the middle button. Back, forward and above never match.
pub const MIDDLE_BUTTON: u32 = 2;

/// The button number a configured button means.
#[must_use]
pub const fn button_number(button: MenuAnywhereMouseButton) -> u32 {
    match button {
        MenuAnywhereMouseButton::RightClick => RIGHT_BUTTON,
        MenuAnywhereMouseButton::MiddleClick => MIDDLE_BUTTON,
    }
}

/// The supported modifiers held in a service flag mask; every other flag is
/// dropped.
#[must_use]
pub fn held_modifiers(flags: ModifierFlags) -> ModifierMask {
    let table = [
        (ModifierFlags::CONTROL, ModifierMask::CONTROL),
        (ModifierFlags::OPTION, ModifierMask::OPTION),
        (ModifierFlags::COMMAND, ModifierMask::COMMAND),
        (ModifierFlags::SHIFT, ModifierMask::SHIFT),
    ];
    let bits = table
        .into_iter()
        .filter(|(flag, _)| flags.contains(*flag))
        .fold(0, |bits, (_, mask)| bits | mask.bits());
    ModifierMask::from_bits_truncate(bits)
}

/// The service flags a required mask names.
#[must_use]
pub fn required_flags(mask: ModifierMask) -> ModifierFlags {
    let table = [
        (ModifierMask::CONTROL, ModifierFlags::CONTROL),
        (ModifierMask::OPTION, ModifierFlags::OPTION),
        (ModifierMask::COMMAND, ModifierFlags::COMMAND),
        (ModifierMask::SHIFT, ModifierFlags::SHIFT),
    ];
    table
        .into_iter()
        .filter(|(bit, _)| bit.bits() & mask.bits() != 0)
        .fold(ModifierFlags::NONE, |flags, (_, flag)| flags | flag)
}

/// The accepted trigger: one button number and the exact modifier set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Matcher {
    /// The required button number.
    pub button: u32,
    /// The required modifiers, compared exactly.
    pub modifiers: ModifierMask,
}

impl Matcher {
    /// The matcher an accepted configuration means.
    #[must_use]
    pub fn from_config(config: &MenuAnywhereConfig) -> Self {
        Self {
            button: button_number(config.mouse_button),
            modifiers: config.required_modifiers(),
        }
    }

    /// The service prefilter: `MouseDown` of the right or middle button with
    /// any modifiers.
    ///
    /// It is deliberately independent of the configuration, so a reconfigure
    /// never replaces the shared-service filter. The published
    /// matcher-and-revision tuple read by the callback is the exact authority
    /// for button, modifiers and repeat.
    #[must_use]
    pub fn prefilter() -> InputFilter {
        InputFilter::new(KindSet::of(InputEventKind::MouseDown))
            .with_buttons([MouseButton(RIGHT_BUTTON), MouseButton(MIDDLE_BUTTON)])
            .with_modifiers(ModifierMatch::Any)
    }

    /// Whether a mouse event is this trigger.
    #[must_use]
    pub fn matches(
        self,
        kind: InputEventKind,
        stream: PressStream,
        flags: ModifierFlags,
        repeat: bool,
    ) -> bool {
        kind == InputEventKind::MouseDown
            && !repeat
            && stream == PressStream::Button(MouseButton(self.button))
            && self.modifiers.matches(held_modifiers(flags))
    }
}

/// The immutable tuple a callback reads: one coherent accepted filter, its
/// revision, the runtime generation and permission/service health.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Published {
    /// The runtime generation this tuple belongs to.
    pub generation: u64,
    /// The accepted-configuration revision the matcher was accepted at.
    pub revision: u64,
    /// The accepted trigger.
    pub matcher: Matcher,
    /// Accessibility is granted and not revoked.
    pub permission: bool,
    /// The shared input service and the worker are healthy.
    pub service: bool,
}

impl Published {
    /// Whether a match may be suppressed.
    #[must_use]
    pub const fn eligible(&self) -> bool { self.permission && self.service }
}

/// The published cell. Writers replace the whole tuple under a short write
/// lock; the callback only `try_read`s, and a contended read fails open.
pub struct PublishedCell {
    value: RwLock<Published>,
}

impl PublishedCell {
    /// A cell holding `value`.
    #[must_use]
    pub const fn new(value: Published) -> Self { Self { value: RwLock::new(value) } }

    /// The tuple, or `None` while a writer holds the cell.
    #[must_use]
    pub fn try_load(&self) -> Option<Published> {
        match self.value.try_read() {
            Ok(value) => Some(*value),
            Err(TryLockError::Poisoned(poisoned)) => Some(*poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// The tuple, waiting for a writer. Never called from the callback.
    #[must_use]
    pub fn load(&self) -> Published { *self.value.read().unwrap_or_else(PoisonError::into_inner) }

    /// Replaces the tuple atomically.
    pub fn store(&self, value: Published) {
        *self.value.write().unwrap_or_else(PoisonError::into_inner) = value;
    }

    /// Changes the tuple atomically.
    pub fn update(&self, change: impl FnOnce(&mut Published)) {
        change(&mut self.value.write().unwrap_or_else(PoisonError::into_inner));
    }
}

/// A typed, counted outcome. Counters are nonblocking atomics read and logged
/// off the callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum Outcome {
    /// A match was admitted and queued.
    Admitted,
    /// A match arrived while the reservation was held and was swallowed.
    Coalesced,
    /// The bounded queue was unexpectedly full; the click passed.
    QueueFull,
    /// The worker queue was disconnected; the click passed.
    WorkerUnavailable,
    /// The published tuple was being replaced; the click passed.
    SnapshotBusy,
    /// A queued request outlived the capture window and was dropped.
    RequestStale,
    /// Authority was revoked before a stage.
    Revoked,
    /// The frontmost capture failed or lacked an identity.
    CaptureFailed,
    /// The capture hop exceeded the acknowledgement window.
    CaptureSlow,
    /// There was no confirmed frontmost application.
    NoFrontmost,
    /// The frontmost application is this process.
    SelfTarget,
    /// The trigger event carried no Quartz location.
    PointerUnavailable,
    /// A complete menu was read.
    Read,
    /// The target has no menu bar or an empty one.
    NoMenu,
    /// Accessibility denied the read.
    ReadPermissionDenied,
    /// The read exceeded its deadline or an element timeout.
    ReadTimeout,
    /// The read was cancelled.
    ReadCancelled,
    /// The read failed structurally (unsupported, malformed, limits, transport).
    ReadFailed,
    /// The captured instance changed during or after the read.
    StaleTarget,
    /// The menu was shown.
    Shown,
    /// The authority or target was stale at show.
    ShowStale,
    /// Geometry or the presenter rejected the show.
    ShowRejected,
    /// The show was not acknowledged in time or could not be dispatched.
    ShowTimeout,
    /// A menu action was selected.
    Selected,
    /// The press succeeded.
    ActionPressed,
    /// The selection was stale, cancelled or expired before it was issued.
    ActionStale,
    /// The application refused activation or the press was unsupported.
    ActionRefused,
    /// An issued send has an unknown result; never retried.
    ActionUncertain,
    /// The action failed in another way.
    ActionFailed,
    /// Cleanup finished.
    CleanupComplete,
    /// Cleanup is pending and retained.
    CleanupPending,
}

impl Outcome {
    /// Every outcome, in declaration order.
    pub const ALL: [Self; 31] = [
        Self::Admitted,
        Self::Coalesced,
        Self::QueueFull,
        Self::WorkerUnavailable,
        Self::SnapshotBusy,
        Self::RequestStale,
        Self::Revoked,
        Self::CaptureFailed,
        Self::CaptureSlow,
        Self::NoFrontmost,
        Self::SelfTarget,
        Self::PointerUnavailable,
        Self::Read,
        Self::NoMenu,
        Self::ReadPermissionDenied,
        Self::ReadTimeout,
        Self::ReadCancelled,
        Self::ReadFailed,
        Self::StaleTarget,
        Self::Shown,
        Self::ShowStale,
        Self::ShowRejected,
        Self::ShowTimeout,
        Self::Selected,
        Self::ActionPressed,
        Self::ActionStale,
        Self::ActionRefused,
        Self::ActionUncertain,
        Self::ActionFailed,
        Self::CleanupComplete,
        Self::CleanupPending,
    ];

    /// A short stable label for logs. It never carries menu titles.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Coalesced => "coalesced",
            Self::QueueFull => "queue full",
            Self::WorkerUnavailable => "worker unavailable",
            Self::SnapshotBusy => "snapshot busy",
            Self::RequestStale => "request stale",
            Self::Revoked => "revoked",
            Self::CaptureFailed => "capture failed",
            Self::CaptureSlow => "capture slow",
            Self::NoFrontmost => "no frontmost application",
            Self::SelfTarget => "self target",
            Self::PointerUnavailable => "event point unavailable",
            Self::Read => "read",
            Self::NoMenu => "no menu",
            Self::ReadPermissionDenied => "read permission denied",
            Self::ReadTimeout => "read timeout",
            Self::ReadCancelled => "read cancelled",
            Self::ReadFailed => "read failed",
            Self::StaleTarget => "stale target",
            Self::Shown => "shown",
            Self::ShowStale => "show stale",
            Self::ShowRejected => "show rejected",
            Self::ShowTimeout => "show timeout",
            Self::Selected => "selected",
            Self::ActionPressed => "action pressed",
            Self::ActionStale => "action stale",
            Self::ActionRefused => "action refused",
            Self::ActionUncertain => "action uncertain",
            Self::ActionFailed => "action failed",
            Self::CleanupComplete => "cleanup complete",
            Self::CleanupPending => "cleanup pending",
        }
    }
}

/// Nonblocking outcome counters shared by the callback and the worker.
pub struct Counters {
    cells: [AtomicU64; Outcome::ALL.len()],
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            cells: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl Counters {
    /// Counts one outcome.
    pub fn bump(&self, outcome: Outcome) {
        self.cells[outcome as usize].fetch_add(1, Ordering::Relaxed);
    }

    /// How many times `outcome` happened.
    #[must_use]
    pub fn get(&self, outcome: Outcome) -> u64 {
        self.cells[outcome as usize].load(Ordering::Relaxed)
    }
}

/// What the callback hands the worker, and what the worker hands back to stop.
#[derive(Debug)]
pub enum Message {
    /// One admitted trigger.
    Request(Request),
    /// Wakes the worker so it observes cancellation.
    Stop,
}

/// One admitted trigger: the coherent tuple the callback captured.
#[derive(Clone, Copy, Debug)]
pub struct Request {
    /// The busy reservation this request owns, released exactly once by its
    /// own worker pass.
    pub reservation: u64,
    /// The runtime generation.
    pub generation: u64,
    /// The accepted revision the trigger matched at.
    pub revision: u64,
    /// The trigger time on the injected monotonic clock.
    pub triggered_at: Instant,
    /// The tapped event's own Quartz point, copied in the callback. `None` only
    /// when the event carried no location.
    pub anchor: Option<QuartzPoint>,
}

/// The single busy reservation and the one-slot request queue.
pub struct Admission {
    busy: AtomicU64,
    serial: AtomicU64,
    queue: SyncSender<Message>,
    worker_available: AtomicBool,
    counters: Arc<Counters>,
}

impl Admission {
    /// Admission over the sending half of a one-slot queue.
    #[must_use]
    pub const fn new(queue: SyncSender<Message>, counters: Arc<Counters>) -> Self {
        Self {
            busy: AtomicU64::new(0),
            serial: AtomicU64::new(0),
            queue,
            worker_available: AtomicBool::new(true),
            counters,
        }
    }

    /// Whether the worker queue is still connected.
    #[must_use]
    pub fn worker_available(&self) -> bool { self.worker_available.load(Ordering::SeqCst) }

    /// Whether a reservation is held right now.
    #[must_use]
    pub fn is_busy(&self) -> bool { self.busy.load(Ordering::SeqCst) != 0 }

    /// The held reservation, or `0`.
    #[must_use]
    pub fn reservation(&self) -> u64 { self.busy.load(Ordering::SeqCst) }

    /// Releases `reservation` if, and only if, it is still the held one.
    ///
    /// A stale holder can never free a successor's reservation. Returns whether
    /// this call released it.
    pub fn release(&self, reservation: u64) -> bool {
        reservation != 0
            && self
                .busy
                .compare_exchange(reservation, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
    }

    /// Wakes the worker without a request. A full queue already wakes it.
    pub fn wake(&self) { let _ = self.queue.try_send(Message::Stop); }

    /// The admission decision for an eligible match.
    pub(crate) fn admit(
        &self,
        published: &Published,
        now: Instant,
        anchor: Option<QuartzPoint>,
    ) -> Disposition {
        let reservation = self.serial.fetch_add(1, Ordering::SeqCst) + 1;
        if self
            .busy
            .compare_exchange(0, reservation, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            self.counters.bump(Outcome::Coalesced);
            return Disposition::Suppress;
        }
        let request = Request {
            reservation,
            generation: published.generation,
            revision: published.revision,
            triggered_at: now,
            anchor,
        };
        match self.queue.try_send(Message::Request(request)) {
            Ok(()) => {
                self.counters.bump(Outcome::Admitted);
                Disposition::Suppress
            }
            Err(TrySendError::Full(_)) => {
                self.release(reservation);
                self.counters.bump(Outcome::QueueFull);
                Disposition::Pass
            }
            Err(TrySendError::Disconnected(_)) => {
                self.release(reservation);
                self.worker_available.store(false, Ordering::SeqCst);
                self.counters.bump(Outcome::WorkerUnavailable);
                Disposition::Pass
            }
        }
    }
}

/// The suppressing handler. One per run.
pub struct TriggerHandler {
    cell: Arc<PublishedCell>,
    admission: Arc<Admission>,
    generation: u64,
    clock: Arc<dyn MenuClock>,
    counters: Arc<Counters>,
}

impl TriggerHandler {
    /// A handler for one run.
    #[must_use]
    pub fn new(
        cell: Arc<PublishedCell>,
        admission: Arc<Admission>,
        generation: u64,
        clock: Arc<dyn MenuClock>,
        counters: Arc<Counters>,
    ) -> Self {
        Self {
            cell,
            admission,
            generation,
            clock,
            counters,
        }
    }

    /// The pure decision for one event against one published tuple.
    ///
    /// Exposed so the filter tables can run without a tap.
    #[must_use]
    pub fn decide(&self, event: &InputEvent, published: &Published) -> Disposition {
        if published.generation != self.generation
            || !published.eligible()
            || !self.admission.worker_available()
            || !published.matcher.matches(
                event.kind(),
                event.stream(),
                event.flags(),
                event.repeat(),
            )
        {
            return Disposition::Pass;
        }
        let anchor = event.location().map(|point| QuartzPoint { x: point.x, y: point.y });
        self.admission.admit(published, self.clock.now(), anchor)
    }
}

impl InputHandler for TriggerHandler {
    fn on_event(&self, event: &InputEvent, _scope: &mut CallbackScope) -> Disposition {
        if event.kind() != InputEventKind::MouseDown {
            return Disposition::Pass;
        }
        let Some(published) = self.cell.try_load() else {
            self.counters.bump(Outcome::SnapshotBusy);
            return Disposition::Pass;
        };
        self.decide(event, &published)
    }
}
