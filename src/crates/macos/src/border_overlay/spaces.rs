//! Space and Mission Control facts: the seam between native detection sources
//! and the Tiling gate (part 05).
//!
//! This module holds only plain data, a bounded mailbox and the source trait.
//! The native sources (Dock Accessibility observer, private `SkyLight` Space
//! notifications, `NSWorkspace` notifications, the owner thread
//! `stache-tiling-borders-spaces`) implement [`SpaceEvents`] and push
//! [`SpaceFact`]s; the actor drains them each turn and feeds the pure gate.
//!
//! Approach only, never code (decision 5): the reference border tool's files
//! `src/events.c` (`space_handler`), `src/border.c` and `src/misc/window.h`
//! (per-Space placement) and yabai `src/mission_control.c` (Dock Accessibility
//! notifications) were studied for which signals exist. Nothing is copied,
//! translated or closely paraphrased.
//!
//! # Contract for a source
//!
//! * [`SpaceEvents::start`] runs on the actor thread and must not block: it
//!   spawns or signals the owner thread and returns. Registration happens on the
//!   owner thread; when every required registration succeeded it pushes
//!   `SourceRestored(UnknownCause::SourcesNotStarted)` ("registered"). Until
//!   then, and whenever it pushes `SourceLost`, every border stays hidden.
//! * Callbacks only record a bounded fact through a [`SpaceSink`] (no AX read,
//!   no allocation, no lock held while calling out). The sink never blocks.
//! * A full mailbox drops the fact and remembers the overflow. The actor then
//!   holds `Unknown { MailboxOverflow }`, forgets every reason it learned from
//!   facts and calls [`SpaceEvents::resync`]. The source re-reads its own state
//!   and pushes the facts that describe it in full (`SourceLost` for every
//!   capability still lost, `MissionControlEntered` if active,
//!   `SpaceChangeBegan` for each display in transition, `FullscreenChanged`
//!   for each window in transition) and finally
//!   `SourceRestored(UnknownCause::MailboxOverflow)`.
//! * [`SpaceEvents::stop`] runs on the actor thread before the overlays are
//!   released. After it returns the sink is closed: no fact is accepted and no
//!   callback runs.
//! * Facts may be duplicated or out of order (an exit without an enter, a
//!   completion without a start); the gate treats them as idempotent.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::space_sources::StopOutcome;
use crate::DisplayId;
use crate::windows::WindowInstance;

/// Most facts the mailbox holds between two drains.
pub const MAX_SPACE_FACTS: usize = 64;

/// Why the Space or Mission Control state cannot be determined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnknownCause {
    /// No source has reported that it is registered yet (the startup state).
    SourcesNotStarted,
    /// The Dock process is absent or its observer is not registered (launch,
    /// termination, relaunch).
    DockUnregistered,
    /// The mailbox overflowed; facts were lost.
    MailboxOverflow,
    /// A required private symbol is missing.
    MissingSymbol(&'static str),
    /// A notification registration failed.
    RegistrationFailed,
    /// A Space query failed or was ambiguous.
    QueryFailed,
    /// A display or Space layout change is in progress.
    DisplayChange,
    /// The owner thread is not running.
    OwnerStopped,
}

impl fmt::Display for UnknownCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourcesNotStarted => f.write_str("sources not started"),
            Self::DockUnregistered => f.write_str("Dock not registered"),
            Self::MailboxOverflow => f.write_str("event mailbox overflowed"),
            Self::MissingSymbol(name) => write!(f, "missing symbol {name}"),
            Self::RegistrationFailed => f.write_str("registration failed"),
            Self::QueryFailed => f.write_str("Space query failed"),
            Self::DisplayChange => f.write_str("display change in progress"),
            Self::OwnerStopped => f.write_str("owner thread stopped"),
        }
    }
}

/// One bounded fact a source reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceFact {
    /// Mission Control, App Exposé or Show Desktop began.
    MissionControlEntered,
    /// Mission Control, App Exposé or Show Desktop ended.
    MissionControlExited,
    /// A Space change began on a display.
    SpaceChangeBegan {
        /// The display.
        display: DisplayId,
    },
    /// The Space change on a display completed.
    SpaceChangeCompleted {
        /// The display.
        display: DisplayId,
    },
    /// A window entered or left fullscreen (the transition begins on enter and
    /// ends on leave).
    FullscreenChanged {
        /// The window.
        window: WindowInstance,
        /// `true` when the window is entering fullscreen.
        entering: bool,
    },
    /// A source (or one of its registrations) was lost.
    SourceLost(UnknownCause),
    /// A lost source works again (or, for `SourcesNotStarted`, is registered).
    SourceRestored(UnknownCause),
    /// A display or Space layout change began.
    DisplayLayoutChanging,
    /// A display or Space layout change ended.
    DisplayLayoutChanged,
    /// What a source saw, for the debug log only. The gate ignores it.
    Diagnostic(SpaceDiagnostic),
}

/// What a native source saw, for the debug log only; never changes the gate.
///
/// It lets a native check see what the system actually delivered (part 05
/// native run 3: no Dock notification arrived and the private event came at
/// the end of the transition).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceDiagnostic {
    /// One Dock Accessibility notification registration and its result.
    DockRegistration {
        /// The notification name.
        name: &'static str,
        /// `None` on success, else the AX failure.
        error: Option<crate::ax::AxError>,
    },
    /// One Dock notification was delivered and drained.
    DockNotification {
        /// The notification name.
        name: &'static str,
    },
    /// A Dock callback could not be recorded (an unknown name or element, or
    /// a full observer mailbox); the observer is rebuilt.
    DockDropped,
    /// One private Space-change candidate registration and its result.
    PrivateRegistration {
        /// The candidate event id.
        event: u32,
        /// The returned `CGError` (0 is success); `None` when the symbol is
        /// missing.
        code: Option<i32>,
    },
    /// A private Space-change candidate event arrived (it is reported as
    /// `SpaceChangeBegan` right after).
    PrivateEvent {
        /// The candidate event id that fired.
        event: u32,
    },
}

/// What [`SpaceSink::push`] did with a fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    /// Queued.
    Queued,
    /// The mailbox was full: the fact was dropped and the overflow remembered.
    Overflowed,
    /// The mailbox is closed (the source was stopped); the fact was dropped.
    Closed,
}

/// What one [`SpaceMailbox::drain`] returns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpaceDrain {
    /// Facts in arrival order.
    pub facts: Vec<SpaceFact>,
    /// Whether any fact was dropped since the last drain.
    pub overflowed: bool,
}

/// Wakes the actor after a fact was queued. Must not block.
pub type SpaceWaker = Arc<dyn Fn() + Send + Sync>;

struct State {
    queue: VecDeque<SpaceFact>,
    overflowed: bool,
    closed: bool,
}

struct Shared {
    state: Mutex<State>,
    waker: Mutex<Option<SpaceWaker>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The bounded mailbox between a source and the actor. Cheap to clone the
/// producer half ([`SpaceSink`]); the actor owns the mailbox and drains it.
pub struct SpaceMailbox {
    shared: Arc<Shared>,
}

impl Default for SpaceMailbox {
    fn default() -> Self { Self::new() }
}

impl SpaceMailbox {
    /// An open, empty mailbox with a fixed buffer of [`MAX_SPACE_FACTS`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    queue: VecDeque::with_capacity(MAX_SPACE_FACTS),
                    overflowed: false,
                    closed: false,
                }),
                waker: Mutex::new(None),
            }),
        }
    }

    /// Sets what wakes the actor when a fact is queued.
    pub fn set_waker(&self, waker: Option<SpaceWaker>) { *lock(&self.shared.waker) = waker; }

    /// A producer handle.
    #[must_use]
    pub fn sink(&self) -> SpaceSink {
        SpaceSink {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Takes everything queued and the overflow flag.
    #[must_use]
    pub fn drain(&self) -> SpaceDrain {
        let mut state = lock(&self.shared.state);
        SpaceDrain {
            facts: state.queue.drain(..).collect(),
            overflowed: std::mem::take(&mut state.overflowed),
        }
    }

    /// Whether anything waits to be drained.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        let state = lock(&self.shared.state);
        state.overflowed || !state.queue.is_empty()
    }

    /// Refuses further facts and discards the queue (the source stopped).
    pub fn close(&self) {
        let mut state = lock(&self.shared.state);
        state.closed = true;
        state.queue.clear();
        state.overflowed = false;
    }

    /// Accepts facts again (a new start).
    pub fn reopen(&self) {
        let mut state = lock(&self.shared.state);
        state.closed = false;
        state.queue.clear();
        state.overflowed = false;
    }
}

/// The producer half: callable from any thread, never blocks beyond a short
/// lock, never allocates.
#[derive(Clone)]
pub struct SpaceSink {
    shared: Arc<Shared>,
}

impl SpaceSink {
    /// Records one fact and wakes the actor.
    #[must_use]
    pub fn push(&self, fact: SpaceFact) -> PushOutcome {
        let outcome = {
            let mut state = lock(&self.shared.state);
            if state.closed {
                return PushOutcome::Closed;
            }
            if state.queue.len() >= MAX_SPACE_FACTS {
                state.overflowed = true;
                PushOutcome::Overflowed
            } else {
                state.queue.push_back(fact);
                PushOutcome::Queued
            }
        };
        // Called with no lock held.
        let waker = lock(&self.shared.waker).clone();
        if let Some(waker) = waker {
            waker();
        }
        outcome
    }
}

/// A native detection source. See the module documentation for the contract.
pub trait SpaceEvents: Send {
    /// Starts the owner and its registrations; non-blocking.
    fn start(&mut self, sink: SpaceSink);
    /// Asks the owner to re-read its state and report it (after an overflow);
    /// non-blocking.
    fn resync(&mut self);
    /// Stops the owner within a bound; after a [`StopOutcome::Stopped`] or
    /// [`StopOutcome::NotRunning`] no callback runs. A timed out stop keeps the
    /// thread handle (never detached), and a later `stop` retries it.
    fn stop(&mut self) -> StopOutcome;
}

/// Creates a fresh source for each run of the module (the host composes it; the
/// source owns no thread or registration until `start`).
pub type SpaceEventsFactory = Arc<dyn Fn() -> Box<dyn SpaceEvents> + Send + Sync>;

/// The source before any native one exists: it registers nothing and never
/// reports, so the gate stays in `Unknown { SourcesNotStarted }` and borders
/// stay hidden (fail closed).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSpaceEvents;

impl SpaceEvents for NoSpaceEvents {
    fn start(&mut self, _sink: SpaceSink) {}

    fn resync(&mut self) {}

    fn stop(&mut self) -> StopOutcome { StopOutcome::NotRunning }
}
