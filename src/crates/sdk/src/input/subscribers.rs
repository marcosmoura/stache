//! Subscription contracts: priorities, structurally separate handler and
//! observer traits, the callback scope and the published subscriber set.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

pub use super::event::Disposition;
use super::event::{InputEvent, PressId};
use super::filter::InputFilter;
use super::work::WorkReservation;

/// Opaque subscription identity scoped by service, tap generation and revision.
///
/// IDs from another service or from a retired generation are rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId {
    pub(crate) service: u64,
    pub(crate) generation: u64,
    pub(crate) revision: u64,
}

/// A generic ordered priority. Higher `level` runs first; `tie_break` orders
/// equal levels. The SDK knows no module identities: the host assigns values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct InputPriority {
    /// Primary order; higher runs first.
    pub level: u16,
    /// Secondary order for equal levels; higher runs first.
    pub tie_break: u16,
}

/// Whether the first down must reserve a work slot before the handler runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkRequirement {
    /// No work; for example deliberate empty-command capture.
    None,
    /// Reserve one slot. When none is free, the whole press fails open.
    Reserve,
}

/// Declaration of a suppressing subscription.
#[derive(Debug, Clone)]
pub struct HandlerSpec {
    /// What to receive.
    pub filter: InputFilter,
    /// Ordering among handlers.
    pub priority: InputPriority,
    /// Work reservation policy for a first down.
    pub work: WorkRequirement,
}

/// Declaration of a passive subscription.
#[derive(Debug, Clone)]
pub struct ObserverSpec {
    /// What to receive.
    pub filter: InputFilter,
}

/// The only mutable surface a handler callback has.
///
/// It can take the work slot reserved for this press and change one
/// preallocated per-press word. It cannot allocate, block, log or reach the
/// service.
#[derive(Debug)]
pub struct CallbackScope {
    work: Option<WorkReservation>,
    press_state: u64,
    first_down: bool,
}

impl CallbackScope {
    pub(crate) const fn new(first_down: bool, press_state: u64) -> Self {
        Self {
            work: None,
            press_state,
            first_down,
        }
    }

    pub(crate) fn set_work(&mut self, work: WorkReservation) { self.work = Some(work); }

    pub(crate) const fn press_state_value(&self) -> u64 { self.press_state }

    /// Whether this is the first down of the press.
    #[must_use]
    pub const fn is_first_down(&self) -> bool { self.first_down }

    /// Takes the work slot reserved for this press, once.
    pub const fn take_work(&mut self) -> Option<WorkReservation> { self.work.take() }

    /// The per-press word, `0` for a new press.
    #[must_use]
    pub const fn press_state(&self) -> u64 { self.press_state }

    /// Replaces the per-press word.
    pub const fn set_press_state(&mut self, value: u64) { self.press_state = value; }
}

/// A suppressing subscriber.
///
/// Callbacks must be bounded, non-blocking and non-panicking. They take `&self`
/// so shared state is atomic or preallocated; the router never takes a blocking
/// lock around them.
pub trait InputHandler: Send + Sync {
    /// Decides the first down of a press, or observes a repeat/up of a press it
    /// owns. For a repeat or up the returned value is ignored: the first-down
    /// disposition persists until the up.
    fn on_event(&self, event: &InputEvent, scope: &mut CallbackScope) -> Disposition;

    /// A tap was disabled: cancel any armed action for this press. No release is
    /// synthesized.
    fn on_cancel(&self, _press: PressId) {}
}

/// A passive subscriber on the listen-only tap. It returns nothing, so it cannot
/// suppress, reorder or influence the suppressing tap.
pub trait InputObserver: Send + Sync {
    /// Receives one matching event.
    fn on_event(&self, event: &InputEvent);

    /// The listen-only tap was disabled; events may be missing.
    fn on_discontinuity(&self) {}
}

/// Why a subscriber stopped receiving callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InertReason {
    /// Logically unsubscribed.
    Unsubscribed,
    /// Replaced by a priority or filter change.
    Replaced,
    /// The callback panicked.
    Panicked,
    /// A returned callback exceeded the soft time target.
    Slow,
    /// The owning module was invalidated.
    Invalidated,
}

impl InertReason {
    const fn code(self) -> u8 {
        match self {
            Self::Unsubscribed => 1,
            Self::Replaced => 2,
            Self::Panicked => 3,
            Self::Slow => 4,
            Self::Invalidated => 5,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Unsubscribed,
            2 => Self::Replaced,
            3 => Self::Panicked,
            4 => Self::Slow,
            5 => Self::Invalidated,
            _ => return None,
        })
    }
}

/// The atomic inert flag checked before every callback.
#[derive(Debug, Default)]
pub(crate) struct Health(AtomicU8);

impl Health {
    /// Marks inert; the first reason wins. Returns whether this call did it.
    pub(crate) fn mark(&self, reason: InertReason) -> bool {
        self.0
            .compare_exchange(0, reason.code(), Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub(crate) fn is_inert(&self) -> bool { self.0.load(Ordering::SeqCst) != 0 }

    pub(crate) fn reason(&self) -> Option<InertReason> {
        InertReason::from_code(self.0.load(Ordering::SeqCst))
    }
}

pub(crate) struct HandlerEntry {
    pub(crate) id: SubscriptionId,
    pub(crate) sequence: u64,
    pub(crate) priority: InputPriority,
    pub(crate) filter: InputFilter,
    pub(crate) work: WorkRequirement,
    pub(crate) handler: Arc<dyn InputHandler>,
    pub(crate) health: Health,
}

pub(crate) struct ObserverEntry {
    pub(crate) id: SubscriptionId,
    pub(crate) filter: InputFilter,
    pub(crate) observer: Arc<dyn InputObserver>,
    pub(crate) health: Health,
}

/// An immutable, ordered subscriber snapshot rebuilt off the tap path.
#[derive(Default)]
pub(crate) struct SubscriberSet {
    /// Descending priority, then ascending registration sequence.
    pub(crate) handlers: Vec<Arc<HandlerEntry>>,
    pub(crate) observers: Vec<Arc<ObserverEntry>>,
}

impl SubscriberSet {
    pub(crate) fn build(handlers: &[Arc<HandlerEntry>], observers: &[Arc<ObserverEntry>]) -> Self {
        let mut handlers = handlers.to_vec();
        handlers.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.sequence.cmp(&b.sequence)));
        Self {
            handlers,
            observers: observers.to_vec(),
        }
    }
}
