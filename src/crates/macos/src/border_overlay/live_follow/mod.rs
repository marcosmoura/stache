//! Live follow of user drags and resizes (part 06).
//!
//! While the user moves or resizes a bordered window, the window server sends
//! private notifications. [`SystemLiveFollow`] listens for them, and while at
//! least one bordered window reports motion it ticks a per-display
//! [`FrameClock`] (the shared adapter of `display_link`, no second adapter). The
//! first move or resize event of a window with no read in the current display
//! period reads its bounds at once on the owner thread and hands the move to a
//! [`FollowSink`] (the Tiling route, which sends it to the overlay control); the
//! display clock then only paces the later reads, at most one per window per
//! display period, coalesced into one batch per tick. The clock stops after
//! [`QUIET`] without events; idle owns no clock.
//!
//! Move and resize events act, and so does the reorder event (forwarded to
//! the sink, which re-orders only an `above` border; see Tiling's engine). The
//! hide, unhide and close events are **not acted on** (native run 2: the event
//! then taken for the hide fired after nearly every show and hid the border,
//! which fits the hide and unhide ids having been swapped then; see
//! [`skylight`]); each has its own flag in [`FollowTiming`], off by default,
//! for a later verified run.
//!
//! Every raw private event of a watched window (its id, including the
//! logging-only candidates 800..=830, and the window id) is drained on the owner
//! thread, stamped with the owner's clock and handed to
//! [`FollowSink::raw_events`] for the debug log (never logged in a callback). Without the private symbols the engine falls back to hiding the
//! border while its window moves (public fallback, Tiling side).
//!
//! Approach only, never code (decision 5): the reference border tool's `src/events.c`
//! (`events_register`: which notifications exist and that its own windows are
//! ignored by owner), `src/windows.c` (`windows_update_notifications`: a
//! per-window subscription of the bordered ids), `src/border.c` (`border_move`:
//! read the target bounds, move the border) and `src/animation.c` (display
//! pacing) were studied for what to listen for. The declarations and every
//! structure here are written from the spec; no header of either project is
//! copied.
//!
//! # Threads
//!
//! ```text
//! actor thread            owner thread `stache-tiling-borders-events`        any thread
//!   set_targets ───────▶  session: register, subscribe, pump (run loop)  ◀─── callbacks
//!   (latest wins)         FollowCore: events, clocks, bounds reads,             copy (kind, id)
//!                         quiet period, sink calls                              into EventSlots
//! ```
//!
//! * The **owner thread** owns the registration and its run loop
//!   ([`WindowEventSession`] is created, used and dropped there; it is not
//!   `Send`) and also runs the follower ([`FollowCore`]): the private bounds
//!   read and the batch send never run on the actor or the main thread and
//!   never wait for either. Deliberately one thread (not the spaces owner): the
//!   follower must not be delayed by Dock Accessibility drains.
//! * **Callbacks** (unverified thread, possibly main) only copy
//!   `(event, cg_window_id)` into [`EventSlots`] (a fixed array, latest per
//!   window and kind, no allocation, no lock held while calling out, no AX) and
//!   wake the owner. Events for windows that are not watched (not currently
//!   bordered, or owned by Stache) are dropped and counted.
//! * The **actor** only calls [`LiveFollow::set_targets`] (a latest-wins
//!   mailbox, never blocking) when the bordered set changes, and reads
//!   [`LiveFollow::status`].
//! * The **frame clock** callbacks only store a tick and wake the owner.
//!
//! # Stop
//!
//! [`SystemLiveFollow::stop_bounded`] closes the slots first (waits for the
//! callbacks in progress), posts a stop, waits for the owner within a bound and
//! joins it. The owner stops every clock with the shared stop guarantee (no
//! callback after release). A clock that cannot stop is a [`StuckClock`]: it is
//! retained by the source (never dropped, never detached) and reported by
//! [`SystemLiveFollow::stuck_clocks`]; the outcome is then not `Stopped`.
//!
//! # Unverified until the part 06 native check
//!
//! The event ids and payload layout of the window notifications, the signatures
//! of the subscription and bounds functions, the thread the callbacks run on,
//! whether the registration needs the owner's run loop, the cost of one bounds
//! read and the display link cadence on `ProMotion` displays. See
//! [`skylight`].

#[cfg(any(test, feature = "testing"))]
mod fakes;
mod follower;
pub mod native;
mod owner;
pub mod skylight;
mod slots;
#[cfg(test)]
mod tests;

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(any(test, feature = "testing"))]
pub use fakes::{FakeLiveFollow, FakeSessionControl, FakeSink, SinkCall};
pub use follower::{FastOrderFn, FollowCore, FollowCounters, FollowTiming};
pub use owner::{SessionMaker, SystemLiveFollow, forgotten_follow_owners};
pub use slots::{EventSlots, RAW_CAPACITY, RawEvent, SlotEvent, SlotsCounters};

use super::OverlayRect;
pub use super::space_sources::{LoopWaker, StopObserver, StopOutcome};
use crate::DisplayId;
use crate::windows::WindowInstance;

/// How long a window may stay silent before the follower stops ticking for it.
pub const QUIET: Duration = Duration::from_millis(100);
/// The owner thread name.
pub const OWNER_THREAD_NAME: &str = "stache-tiling-borders-events";

/// The window notifications live follow listens for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WindowEventKind {
    /// The window moved.
    Move,
    /// The window was resized.
    Resize,
    /// The window was re-ordered in the window server's stack (forwarded to
    /// the sink, which re-orders `above` borders).
    Reorder,
    /// The window was ordered out or hidden (not acted on).
    Hide,
    /// The window was ordered in or unhidden; also posted when its application
    /// orders the visible window front (not acted on).
    Unhide,
    /// The window was destroyed (candidate id, unverified, not acted on).
    Close,
}

impl WindowEventKind {
    /// Every kind.
    pub const ALL: [Self; 6] = [
        Self::Move,
        Self::Resize,
        Self::Reorder,
        Self::Hide,
        Self::Unhide,
        Self::Close,
    ];

    /// The bit this kind occupies in a slot.
    #[must_use]
    pub const fn bit(self) -> u32 {
        match self {
            Self::Move => 1,
            Self::Resize => 2,
            Self::Reorder => 4,
            Self::Hide => 8,
            Self::Unhide => 16,
            Self::Close => 32,
        }
    }
}

/// One bordered window the follower may follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FollowTarget {
    /// The exact Tiling window (opaque to the follower; handed back to the sink).
    pub window: WindowInstance,
    /// The window server id events and bounds reads use.
    pub cg_window_id: u32,
    /// The display the window is on (selects the frame clock). `None` is never
    /// followed: the Tiling fallback handles it.
    pub display: Option<DisplayId>,
    /// The owner process, to drop Stache's own windows.
    pub owner_pid: Option<i32>,
    /// The window number of the border's overlay panel (recorded on the main
    /// thread after the panel was created; `None` until known). Only the fast
    /// reorder path uses it.
    pub overlay_window: Option<u32>,
    /// The border is placed `above` its window.
    pub above: bool,
    /// The engine's verdict that the fast reorder may re-stack the panel now:
    /// the border is shown and the Space gate is known and open (not
    /// suspended, not window-blocked). `false` when unknown.
    pub fast_reorder: bool,
}

impl FollowTarget {
    /// The overlay panel the fast reorder may re-stack, if every condition
    /// holds (`above`, shown under an open gate, window number known).
    #[must_use]
    pub const fn fast_panel(&self) -> Option<u32> {
        if self.above && self.fast_reorder {
            self.overlay_window
        } else {
            None
        }
    }
}

/// At most this many fast reorders per window per [`FAST_REORDER_WINDOW`] (the
/// same bound as Tiling's reorder loop guard).
pub const FAST_REORDER_CAP: usize = 4;
/// The window of [`FAST_REORDER_CAP`].
pub const FAST_REORDER_WINDOW: Duration = Duration::from_secs(1);

/// The outcome of one private fast order call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FastOrderResult {
    /// The window server accepted the order.
    Ordered,
    /// The main thread's record says the panel is not shown (ordered out,
    /// hidden, released or never registered): nothing was called.
    NotShown,
    /// The call returned this non-zero `CGError`.
    Refused(i32),
    /// The private symbol (named) or the connection is unavailable.
    Missing(&'static str),
}

/// One fast reorder attempt, reported to the sink on the owner thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FastReorder {
    /// The target window server id.
    pub cg_window_id: u32,
    /// The overlay panel's window number.
    pub panel: u32,
    /// What the call did.
    pub result: FastOrderResult,
    /// `true` when this result disabled the fast path for the rest of the run
    /// (reported once).
    pub disabled: bool,
}

/// One bounds sample of a moving window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FollowMove {
    /// The window.
    pub window: WindowInstance,
    /// Its bounds, global top-left points.
    pub bounds: OverlayRect,
    /// The authority stamp [`FollowSink::authority`] returned before the read.
    pub stamp: u64,
}

/// One raw private event of a watched window, stamped on the owner thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawWindowEvent {
    /// The private event id as received.
    pub event: u32,
    /// The window server id.
    pub cg_window_id: u32,
    /// The kind the id maps to; `None` for a logging-only id.
    pub kind: Option<WindowEventKind>,
    /// When the owner drained it (owner-thread clock).
    pub at: Instant,
}

/// What the follower reports into. Implemented by the Tiling route. Every method
/// must be non-blocking; none may take the actor or main thread.
pub trait FollowSink: Send + Sync {
    /// The authority stamp for a live sample of `window`, or `None` while Tiling
    /// owns the window (a write in flight, unknown window): no bounds are read.
    fn authority(&self, window: WindowInstance) -> Option<u64>;
    /// One coalesced batch of live samples for one tick. A sample whose stamp is
    /// no longer current (a Tiling frame arrived meanwhile) is dropped.
    fn moves(&self, moves: &[FollowMove]);
    /// Whether `window` is currently followed (inside its quiet period).
    fn following(&self, window: WindowInstance, following: bool);
    /// The window server hid the window: hide its border earlier.
    fn hide(&self, window: WindowInstance);
    /// The window server unhid the window. Never shows a border by itself.
    fn unhide(&self, window: WindowInstance);
    /// The window was re-ordered: re-order its border relative to it.
    fn reorder(&self, window: WindowInstance);
    /// The window was destroyed: release its border.
    fn close(&self, window: WindowInstance);
    /// The raw events drained in one owner pass, in arrival order (debug
    /// evidence only; called on the owner thread, never in a callback).
    fn raw_events(&self, events: &[RawWindowEvent]) { let _ = events; }
    /// One fast reorder attempt (debug evidence; called on the owner thread
    /// right after the call, before [`Self::reorder`]).
    fn fast_reorder(&self, report: FastReorder) { let _ = report; }
}

/// Whether live follow works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveStatus {
    /// The owner has not finished registering yet.
    Starting,
    /// Registered and subscribed.
    Active,
    /// Unavailable, with the reason.
    Unavailable(String),
}

impl LiveStatus {
    /// The one diagnostic for an unavailable live follow.
    #[must_use]
    pub fn diagnostic(&self) -> Option<String> {
        match self {
            Self::Unavailable(reason) => Some(unavailable_message(reason)),
            Self::Starting | Self::Active => None,
        }
    }
}

/// The diagnostic for an unavailable live follow.
#[must_use]
pub fn unavailable_message(reason: &str) -> String {
    format!("borders: live follow unavailable ({reason}); borders hide while a window moves")
}

/// Why registration or a subscription failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveUnavailable {
    /// The reason, for the diagnostic.
    pub reason: String,
}

impl LiveUnavailable {
    /// A failure with `reason`.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self { Self { reason: reason.into() } }

    /// A missing private symbol.
    #[must_use]
    pub fn missing_symbol(name: &str) -> Self { Self::new(format!("missing symbol {name}")) }
}

impl fmt::Display for LiveUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.reason) }
}

/// The owner-thread side of the native notifications. Created, used and dropped
/// on the owner thread; deliberately not `Send`.
pub trait WindowEventSession {
    /// A waker for this session's run loop, callable from any thread. It must
    /// make a pending or future [`Self::pump`] return.
    fn waker(&self) -> LoopWaker;
    /// Resolves every needed symbol and registers the callbacks, which record
    /// into `slots`. Total: any missing symbol or refused registration is an
    /// error.
    ///
    /// # Errors
    /// Why live follow is unavailable.
    fn register(&mut self, slots: Arc<EventSlots>) -> Result<(), LiveUnavailable>;
    /// Replaces the per-window subscription with `ids` (at most `MAX_OVERLAYS`).
    ///
    /// # Errors
    /// Why live follow became unavailable.
    fn subscribe(&mut self, ids: &[u32]) -> Result<(), LiveUnavailable>;
    /// One private bounds read, global top-left points.
    fn bounds(&mut self, cg_window_id: u32) -> Option<OverlayRect>;
    /// Orders the overlay panel `panel` (a window of this process) directly
    /// above `target` with a private window-server call, only when the main
    /// thread records `panel` as shown. Never orders a panel in. The default
    /// has no private call.
    fn order_above(&mut self, panel: u32, target: u32) -> FastOrderResult {
        let _ = (panel, target);
        FastOrderResult::Missing("order_above")
    }
    /// Runs the run loop until it is woken, a source was handled or `timeout`.
    fn pump(&mut self, timeout: Duration);
    /// Removes every registration (idempotent).
    fn unregister(&mut self);
}

/// The engine's view of live follow.
pub trait LiveFollow: Send {
    /// Starts the owner; non-blocking (registration happens on the owner).
    fn start(&mut self, sink: Arc<dyn FollowSink>);
    /// Replaces the bordered set (latest wins, bounded to `MAX_OVERLAYS`).
    fn set_targets(&mut self, targets: &[FollowTarget]);
    /// The current status.
    fn status(&self) -> LiveStatus;
    /// Stops the owner and every clock within a bound. A timed out stop keeps
    /// the owner and the stuck clocks (never detached), and a later `stop`
    /// retries them.
    fn stop(&mut self) -> StopOutcome;
}

/// Creates a fresh live follow for each run of the module.
pub type LiveFollowFactory = Arc<dyn Fn() -> Box<dyn LiveFollow> + Send + Sync>;

/// A live follow that is permanently unavailable (the symbols cannot be
/// resolved on this build): the Tiling fallback applies and the diagnostic
/// carries `reason`.
#[derive(Clone, Debug)]
pub struct UnavailableLiveFollow {
    reason: String,
}

impl UnavailableLiveFollow {
    /// Unavailable for `reason`.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self { Self { reason: reason.into() } }
}

impl LiveFollow for UnavailableLiveFollow {
    fn start(&mut self, _sink: Arc<dyn FollowSink>) {}

    fn set_targets(&mut self, _targets: &[FollowTarget]) {}

    fn status(&self) -> LiveStatus { LiveStatus::Unavailable(self.reason.clone()) }

    fn stop(&mut self) -> StopOutcome { StopOutcome::NotRunning }
}
