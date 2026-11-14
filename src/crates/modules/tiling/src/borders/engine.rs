//! The border engine: diffs successive
//! [`BorderModel`]s into overlay batches and orders border changes around
//! Tiling's own effects.
//!
//! Approach studied, never code (decision 5): the reference border tool's `src/events.c`
//! (`window_modify_handler`, `space_handler`, `front_app_handler`) and
//! `src/windows.c` (`windows_window_hide`, `windows_window_unhide`,
//! `windows_determine_and_focus_active_window`) react to the window server
//! after the fact, which is what makes a border blink when an application is
//! unhidden. Stache's engine knows what Tiling is about to do, so it hides a
//! border *before* the application hides and shows it only *after* the window
//! is visible at its frame.
//!
//! # Ownership and threads
//!
//! [`BorderEngine`] is owned by one thread, the actor; it spawns nothing and
//! waits for nothing. What other threads need is the shared [`SharedBorderRoute`]
//! (`engine.route()`): the frame paths report into it as [`BorderFrames`], and
//! the effects thread calls `hide_application_bounded` right before `HideApp`.
//! The engine never produces a Tiling input.
//!
//! # One batch per update
//!
//! [`BorderEngine::update`] diffs the new model against the ledger and sends at
//! most one [`OverlayBatch`]: `Release` for windows that left the model (they
//! are hidden in the update that removes them, before the turn's frame writes,
//! which the wiring guarantees by updating before it dispatches effects),
//! `Move` for a changed model frame, `Show` with [`AppearanceChange::StateChange`]
//! for a changed appearance (focus or state flip; the overlay cross-fades), and
//! `Show` for windows whose conditions hold and whose frame has passed.
//! [`BorderEngine::configure`] recomputes appearances in place and sends
//! `Show` with [`AppearanceChange::Reload`]. A refused `Show` affects that
//! border only and is counted.
//!
//! # Show only after visible (the blink fix)
//!
//! A window that is not yet drawn is *pending*. Every window that enters the
//! model starts pending, whether it came from a reveal (workspace switch,
//! display return, unhide, external show) or is simply new: the model says
//! which by itself. A reveal is the model's `write_pending` (the window is
//! still in the `revealed` set, or its frame write is in flight); a plain new
//! window has none and only needs the application and one frame. A pending
//! border is shown once all hold:
//!
//! 1. its application is not hidden: not `app_hidden` in the model (hidden by
//!    anyone, or held hidden or being unhidden by the runtime), and not marked
//!    hidden in the route (`hide_application`), until `note_application_shown`;
//! 2. its reveal write completed: the model no longer has `write_pending`, or a
//!    confirming frame (`Settled`/`Readback`) or `note_reveal_written` for that
//!    plan arrived;
//! 3. one display frame has passed since the last of the above became true
//!    (`frame_period`, default 1/60 s, on the injected clock): `tick(now)` shows
//!    it once `next_deadline()` has passed.
//!
//! Any condition turning false again disarms the window. The frame is the
//! newest the route knows (a direct write's target or an animation frame), else
//! the model's.
//!
//! # Hide before the application hides
//!
//! `route().hide_application_bounded(app)` (effects thread) or
//! [`BorderEngine::hide_application`] (actor thread, for an external ⌘H
//! observed after the fact: the lag is the observation delay, documented) hides
//! the application's visible overlays at once and marks it hidden; the engine
//! learns of it from the route at its next call. After `Hidden`, borders return
//! only through the show rule.
//!
//! # Spaces and Mission Control (part 05)
//!
//! The pure [`SpaceGate`] (`spaces.rs`) is the only suspension state. The
//! engine owns the bounded [`SpaceMailbox`] a [`SpaceEvents`] source fills
//! (`start_spaces`/`stop_spaces`); the actor calls [`BorderEngine::poll_spaces`]
//! each turn before the border update, which drains it and feeds the gate.
//!
//! * Entering a global reason sends one `HideAll` under the bumped gate epoch
//!   and blocks every `Show` and `Order` while any reason is held; model updates
//!   continue. A window-scoped fullscreen reason sends a `Hide` for that window
//!   only (see `spaces.rs`).
//! * Every `Show`, `Order` and appearance change is stamped with the gate epoch
//!   it was computed under; the main thread drops one older than the latest
//!   applied `HideAll` (part 01 `OverlayOwners`). Route `Move`s and `Hide`s
//!   carry no epoch: they can never order a window in.
//! * Leaving the last global reason re-confirms every bordered window on the
//!   active Space of its display through [`ActiveSpaceConfirm`] (off main, on
//!   resume and show only, never periodically), then the show rule applies with
//!   one display frame. A confirmation failure holds `Unknown { QueryFailed }`.
//! * The gate starts in `Unknown { SourcesNotStarted }`: with no source that
//!   reports "registered", every border stays hidden (fail closed).
//!
//! # Live follow and the public fallback (part 06)
//!
//! With a [`LiveFollow`] source set (`set_live_follow`), `start_live` starts its
//! owner thread and the engine keeps it told of the bordered set (bounded,
//! coalesced: only when the set changed). The owner reports into the route
//! ([`FollowSink`] on [`SharedBorderRoute`]): live samples move a border, a
//! private hide hides it earlier, a reorder re-orders it, a close releases it,
//! and an unhide only lifts the engine's live hold (it never shows a border; the
//! show rule and the Space gate decide). A live sample is read only while no
//! Tiling write for the window is in flight (`set_busy` from the model) and is
//! dropped when a Tiling frame arrived since the read (the route's stamp).
//!
//! **The fallback needs no private symbol.** A border is *held* (hidden, kept
//! out of the show rule) when
//!
//! the window's observed frame changed with no Tiling write in flight and no
//! Tiling frame explaining it (an external move or resize, from the coalesced
//! geometry samples) and live follow is not following it. A mouse down alone
//! does not hold (a plain click must not flicker the border); while the button
//! stays down on a held window (the model's `pressed` set, read from the drag
//! gesture) the hold lasts until the gesture ended.
//!
//! A hold lasts until its trigger is gone and [`HOLD_QUIET`] passed without a
//! further frame change; the border then follows the normal show rule (the
//! confirmed frame after the gesture, one display frame, the Space gate). With
//! live follow active, an external frame change of a window followed within
//! [`FOLLOW_GRACE`] is not applied (the live bounds are newer than the
//! coalesced sample) and holds nothing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use stache_macos::border_overlay::{
    AppearanceChange, ApplyAck, BorderAppearance, BorderPlacement, FollowSink, FollowTarget,
    LiveFollow, LiveStatus, MAX_OVERLAYS, NoSpaceEvents, OverlayBatch, OverlayDiagnostics,
    OverlayHandle, OverlayKind, OverlayTicket, Refusal, RefusalReason, SpaceEvents, SpaceFact,
    SpaceMailbox, SpaceWaker, StopOutcome, Transition, UnknownCause,
};
use stache_macos::windows::WindowInstance;
use stache_macos::{ApplicationId, DisplayId};

use super::model::{BorderEntry, BorderModel};
use super::motion::transition_for;
use super::route::{HideRequest, SharedBorderRoute, Shown, overlay_rect};
use super::spaces::{GateEffect, Resume, SpaceGate, SuspendReason};
use crate::config::BordersConfig;
use crate::geometry::Rect;

/// One display refresh at 60 Hz: how long a border waits after its window
/// became visible.
pub const DEFAULT_FRAME_PERIOD: Duration = Duration::from_micros(16_667);
/// Acknowledgements kept to notice failed commands.
const MAX_TICKETS: usize = 16;

/// How long the main thread may leave an overlay batch unacknowledged before
/// status reports it busy. Nothing waits for it: later batches merge into the
/// one pending batch (latest wins), the actor and the effect runner never block.
pub const BORDER_ACK_WAIT: Duration = Duration::from_millis(250);

/// The one diagnostic for a main thread that does not acknowledge in time.
pub const MAIN_BUSY_DIAGNOSTIC: &str = "borders: main thread busy";
/// How long a held border waits without a frame change before the show rule may
/// apply again (the public fallback, part 06).
pub const HOLD_QUIET: Duration = Duration::from_millis(100);
/// How long after live follow stopped following a window an external frame
/// change is still attributed to that drag (the coalesced sample arrives after
/// the last live event).
pub const FOLLOW_GRACE: Duration = Duration::from_secs(1);
/// Frame differences below this many points are the same frame.
const FRAME_TOLERANCE: f64 = 1.0;

/// The Space query failed or was ambiguous: nothing is confirmed and the gate
/// holds `Unknown { QueryFailed }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmFailed;

/// Confirms that targets are on the active Space of their display.
///
/// **Contract.** It runs on the engine's (actor) thread, never on the main
/// thread, only when a border is about to be shown or a suspension resumes,
/// never periodically. It may use the gated private window-Space and
/// current-Space queries, else the public on-screen window listing. A window it
/// does not return stays hidden. A failed or ambiguous query is an `Err`.
pub trait ActiveSpaceConfirm: Send {
    /// The subset of `windows` (`(window, cg_window_id)`) confirmed on the
    /// active Space.
    ///
    /// # Errors
    /// [`ConfirmFailed`] when the Space state could not be determined.
    fn confirm_on_active_space(
        &mut self,
        windows: &[(WindowInstance, u32)],
    ) -> Result<BTreeSet<WindowInstance>, ConfirmFailed>;
}

/// The default: every window is confirmed.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConfirmAll;

impl ActiveSpaceConfirm for ConfirmAll {
    fn confirm_on_active_space(
        &mut self,
        windows: &[(WindowInstance, u32)],
    ) -> Result<BTreeSet<WindowInstance>, ConfirmFailed> {
        Ok(windows.iter().map(|(window, _)| *window).collect())
    }
}

/// Engine counters, plain data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineCounters {
    /// `Show` commands the engine or the control refused (cap).
    pub refused_shows: u64,
    /// Overlay commands that failed natively (from acknowledgements).
    pub failed_commands: u64,
    /// `Order` requests dropped while suspended.
    pub suppressed_orders: u64,
    /// `HideAll` requests sent.
    pub hide_alls: u64,
    /// Single-window hides sent for a fullscreen transition.
    pub window_hides: u64,
    /// Space facts the gate processed.
    pub space_facts: u64,
    /// Mailbox overflows seen.
    pub space_overflows: u64,
    /// Windows with every condition but a `cg_window_id`, as of the last model.
    pub missing_cg_id: u64,
    /// Windows the Space confirmation refused.
    pub unconfirmed: u64,
    /// Borders held by the public fallback (an external move or a mouse down).
    pub fallback_holds: u64,
    /// External frame changes not applied because live follow owns the window.
    pub live_suppressed: u64,
    /// `Order`s sent for a window-server reorder of an `above` window.
    pub live_reorders: u64,
    /// Reorder notifications folded into an `Order` already sent or due in
    /// the same display frame.
    pub live_reorders_coalesced: u64,
    /// Reorder notifications dropped by the per-window rate cap
    /// ([`LIVE_REORDER_CAP`] per [`LIVE_REORDER_WINDOW`]).
    pub live_reorders_capped: u64,
}

/// Everything the status surface reports for borders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BorderDiagnostics {
    /// Capability gaps of the overlay layer.
    pub overlay: OverlayDiagnostics,
    /// The one Spaces and Mission Control diagnostic, while any `Unknown` holds.
    pub spaces: Option<String>,
    /// Engine counters.
    pub engine: EngineCounters,
    /// Border hides the effects thread did not see acknowledged in time.
    pub late_hides: u64,
    /// Frames ignored for an older plan sequence.
    pub stale_frames: u64,
    /// The one diagnostic while live follow is unavailable.
    pub live: Option<String>,
    /// Live samples applied as a `Move`.
    pub live_moves: u64,
    /// The main thread did not acknowledge an overlay batch within
    /// [`BORDER_ACK_WAIT`]; later batches merge latest-wins meanwhile.
    pub main_busy: bool,
}

impl BorderDiagnostics {
    /// The published messages, one per kind of problem.
    #[must_use]
    pub fn messages(&self) -> Vec<String> {
        let mut messages: Vec<String> =
            self.overlay.messages().into_iter().map(str::to_owned).collect();
        messages.extend(self.spaces.clone());
        messages.extend(self.live.clone());
        if self.engine.refused_shows > 0 {
            messages.push(format!(
                "borders: {} overlays were refused (limit {MAX_OVERLAYS}); those windows have no border",
                self.engine.refused_shows
            ));
        }
        if self.engine.missing_cg_id > 0 {
            messages.push(format!(
                "borders: {} windows have no window id; they have no border",
                self.engine.missing_cg_id
            ));
        }
        if self.engine.failed_commands > 0 {
            messages.push(format!(
                "borders: {} overlay commands failed",
                self.engine.failed_commands
            ));
        }
        if self.late_hides > 0 {
            messages.push(format!(
                "borders: {} border hides were not acknowledged before an application hid",
                self.late_hides
            ));
        }
        if self.main_busy {
            messages.push(MAIN_BUSY_DIAGNOSTIC.to_owned());
        }
        messages
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent per-window flags read by different show-rule steps"
)]
struct Tracked {
    application: ApplicationId,
    cg: u32,
    /// Reserved in the control (shown, or hidden and kept).
    handle: Option<OverlayHandle>,
    shown: bool,
    /// The appearance last sent (meaningful while shown).
    appearance: BorderAppearance,
    /// The last model frame seen.
    frame: Rect,
    /// When a pending window may be shown.
    ready_at: Option<Instant>,
    /// The Space confirmation refused it; it waits for a resume, a reconfirm
    /// or its next confirming event (see [`BorderEngine::retry_unconfirmed`]).
    unconfirmed: bool,
    /// The route's confirmation count when it became unconfirmed.
    confirmations_seen: u64,
    /// The control or the cap refused its show; retried once room exists.
    refused: bool,
    /// The display and owner process, for live follow.
    display: Option<DisplayId>,
    owner_pid: i32,
    /// Part 09: the model entry is collapsed (unfocused with `unfocused =
    /// false` and a transition). While shown, its overlay is shrinking or has
    /// shrunk and waits for `collapse_until`.
    collapsed: bool,
    /// Part 09: when the shrink of a shown, collapsed overlay is over and the
    /// engine sends its `Hide` (a deadline of [`BorderEngine::next_deadline`]).
    collapse_until: Option<Instant>,
    /// Part 09: the entry just stopped being collapsed while hidden: its next
    /// show grows from zero width.
    grow_next: bool,
    /// The plan of an in-flight Tiling write that holds the model frame back
    /// (the route owns the border's frame until it ends).
    owned_plan: Option<u64>,
}

/// What an external frame change does to a border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExternalPolicy {
    /// Move the border to the new frame (no live follow composed).
    Apply,
    /// Live follow owns the window: the coalesced sample is older than its bounds.
    Suppress,
    /// Hide the border until the window stops moving (the public fallback).
    Hold,
}

/// A border kept hidden by the public fallback.
#[derive(Clone, Copy, Debug)]
struct Hold {
    /// The last time the trigger was seen (a frame change, or the mouse down
    /// still in progress).
    last: Instant,
}

/// The live follow source and what the engine knows of it.
struct LiveSide {
    source: Box<dyn LiveFollow>,
    started: bool,
    /// The last stop timed out: the next one retries.
    stuck: bool,
    /// The set last pushed to the source.
    targets: Vec<FollowTarget>,
}

/// The border engine; see the module documentation.
pub struct BorderEngine {
    route: Arc<SharedBorderRoute>,
    config: BordersConfig,
    model: BorderModel,
    ledger: BTreeMap<WindowInstance, Tracked>,
    frame_period: Duration,
    gate: SpaceGate,
    mailbox: SpaceMailbox,
    source: Box<dyn SpaceEvents>,
    spaces_started: bool,
    /// The last Spaces stop timed out: the next one retries.
    spaces_stuck: bool,
    /// Windows whose fullscreen flag the engine has reported to the gate.
    fullscreen: BTreeSet<WindowInstance>,
    confirmer: Box<dyn ActiveSpaceConfirm>,
    counters: EngineCounters,
    /// Batches sent and not yet seen acknowledged, with the engine time they were sent.
    tickets: Vec<(Instant, OverlayTicket<ApplyAck>)>,
    /// The newest time the actor handed the engine (it never reads a clock).
    now: Instant,
    /// Whether some batch has waited longer than [`BORDER_ACK_WAIT`].
    main_busy: bool,
    /// `Release` commands dropped because the route was already closed: the
    /// handles are still held by the control and still count in the ledger
    /// comparison of the stop.
    unsent_releases: usize,
    live: Option<LiveSide>,
    /// Borders held by the public fallback.
    holds: BTreeMap<WindowInstance, Hold>,
    /// Windows the window server hid (live follow): no show until it unhides or
    /// the model drops them.
    live_hold: BTreeSet<WindowInstance>,
    /// Windows the window server destroyed, ignored until the model drops them.
    destroyed: BTreeSet<WindowInstance>,
    /// Destroyed windows whose overlay still has to be released.
    to_release: Vec<WindowInstance>,
    follow_grace: Duration,
    /// The live follow status last logged (change detection for the debug log).
    live_seen: Option<LiveStatus>,
    /// The focused window the actor last reported.
    focus: Option<WindowInstance>,
    /// Window-server reorders of `above` windows (live follow), per window.
    live_reorders: BTreeMap<WindowInstance, LiveReorder>,
    /// Every batch sent, for tests.
    #[cfg(test)]
    pub(super) sent: Vec<OverlayBatch>,
}

/// The per-window state of the reaction to window-server reorders.
#[derive(Debug, Default)]
struct LiveReorder {
    /// When the `Order` for a reorder is due (coalesced per display frame).
    pending: Option<Instant>,
    /// When the last such `Order` was sent.
    last: Option<Instant>,
    /// The `Order`s sent within the last [`LIVE_REORDER_WINDOW`].
    recent: VecDeque<Instant>,
    /// The cap diagnostic was logged for the current burst.
    capped: bool,
}

/// At most this many reorder-driven `Order`s per window per
/// [`LIVE_REORDER_WINDOW`]: a feedback loop (our own `Order` re-stacking the
/// window and notifying again) stops there with a diagnostic.
pub const LIVE_REORDER_CAP: usize = 4;
/// The window of [`LIVE_REORDER_CAP`].
pub const LIVE_REORDER_WINDOW: Duration = Duration::from_secs(1);

impl BorderEngine {
    /// An engine over `route` (which owns the control) for `config`.
    #[must_use]
    pub fn new(route: Arc<SharedBorderRoute>, config: &BordersConfig) -> Self {
        route.control().set_transition(transition_for(config));
        Self {
            route,
            config: config.clone(),
            model: BorderModel::default(),
            ledger: BTreeMap::new(),
            frame_period: DEFAULT_FRAME_PERIOD,
            gate: SpaceGate::new(),
            mailbox: SpaceMailbox::new(),
            source: Box::new(NoSpaceEvents),
            spaces_started: false,
            spaces_stuck: false,
            fullscreen: BTreeSet::new(),
            confirmer: Box::new(ConfirmAll),
            counters: EngineCounters::default(),
            tickets: Vec::new(),
            now: Instant::now(),
            main_busy: false,
            unsent_releases: 0,
            live: None,
            holds: BTreeMap::new(),
            live_hold: BTreeSet::new(),
            destroyed: BTreeSet::new(),
            to_release: Vec::new(),
            follow_grace: FOLLOW_GRACE,
            live_seen: None,
            focus: None,
            live_reorders: BTreeMap::new(),
            #[cfg(test)]
            sent: Vec::new(),
        }
    }

    /// Sets the display frame period (per display refresh).
    pub const fn set_frame_period(&mut self, period: Duration) { self.frame_period = period; }

    /// Replaces the active-Space confirmation.
    pub fn set_confirmer(&mut self, confirmer: Box<dyn ActiveSpaceConfirm>) {
        self.confirmer = confirmer;
    }

    /// Replaces the Space event source (before [`Self::start_spaces`]).
    pub fn set_space_source(&mut self, source: Box<dyn SpaceEvents>) { self.source = source; }

    /// Sets what wakes the actor when a Space fact is queued.
    pub fn set_space_waker(&self, waker: Option<SpaceWaker>) { self.mailbox.set_waker(waker); }

    /// Sets the live follow source (before [`Self::start_live`]). Without one the
    /// engine moves borders only from the model, as before part 06.
    pub fn set_live_follow(&mut self, source: Box<dyn LiveFollow>) {
        self.live = Some(LiveSide {
            source,
            started: false,
            stuck: false,
            targets: Vec::new(),
        });
    }

    /// Sets what wakes the actor when live follow queued a note for the engine.
    pub fn set_live_waker(&self, waker: Option<super::route::NoteWaker>) {
        self.route.set_waker(waker);
    }

    /// Overrides how long a window counts as recently followed (tests).
    pub const fn set_follow_grace(&mut self, grace: Duration) { self.follow_grace = grace; }

    /// Starts the live follow owner (once per enabled period). Never blocks; the
    /// owner registers on its own thread and reports its status.
    pub fn start_live(&mut self) {
        let sink = Arc::clone(&self.route) as Arc<dyn FollowSink>;
        if let Some(live) = self.live.as_mut()
            && !live.started
        {
            live.started = true;
            live.targets.clear();
            live.source.start(sink);
            self.live_seen = None;
            self.route.log().debug(|| "live follow start requested".to_owned());
        }
        self.sync_live_targets();
    }

    /// Stops the live follow owner and every clock (before the overlays are
    /// released). After it returns no live sample arrives.
    ///
    /// Returns the typed outcome: anything but `Stopped`/`NotRunning` is a stuck
    /// owner that keeps its thread and clocks and is retried by a later stop.
    pub fn stop_live(&mut self) -> StopOutcome {
        self.holds.clear();
        self.live_hold.clear();
        self.live_reorders.clear();
        let Some(live) = self.live.as_mut() else {
            return StopOutcome::NotRunning;
        };
        if !live.started && !live.stuck {
            return StopOutcome::NotRunning;
        }
        live.started = false;
        live.targets.clear();
        // A timed out stop is asked again: the retry must reach the source.
        let outcome = live.source.stop();
        live.stuck = matches!(outcome, StopOutcome::TimedOut { .. });
        self.live_seen = None;
        self.route.log().debug(|| format!("live follow stop outcome={outcome:?}"));
        outcome
    }

    /// The live follow status, when a source is composed.
    #[must_use]
    pub fn live_status(&self) -> Option<LiveStatus> {
        self.live.as_ref().map(|live| live.source.status())
    }

    fn live_active(&self) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.source.status() == LiveStatus::Active)
    }

    /// Whether `window` is held back by the fallback or a live hide.
    #[must_use]
    pub fn is_held_back(&self, window: WindowInstance) -> bool {
        self.holds.contains_key(&window) || self.live_hold.contains(&window)
    }

    /// The shared half: hand it to the frame paths as `Arc<dyn BorderFrames>`
    /// and to the effects thread for the bounded hide.
    #[must_use]
    pub fn route(&self) -> Arc<SharedBorderRoute> { Arc::clone(&self.route) }

    /// The configuration in effect.
    #[must_use]
    pub const fn config(&self) -> &BordersConfig { &self.config }

    /// The gate epoch: bumped each time a global suspension began. Every `Show`,
    /// `Order` and appearance change is stamped with it.
    #[must_use]
    pub const fn gate_epoch(&self) -> u64 { self.gate.epoch() }

    /// Whether a global suspension reason is held (nothing may be shown).
    #[must_use]
    pub fn suspended(&self) -> bool { self.gate.suspended() }

    /// The gate (read only).
    #[must_use]
    pub const fn gate(&self) -> &SpaceGate { &self.gate }

    /// Windows that currently have a visible overlay.
    #[must_use]
    pub fn shown_windows(&self) -> Vec<WindowInstance> {
        self.ledger.iter().filter(|(_, t)| t.shown).map(|(w, _)| *w).collect()
    }

    /// Windows waiting to be shown.
    #[must_use]
    pub fn pending_windows(&self) -> Vec<WindowInstance> {
        self.ledger
            .iter()
            .filter(|(w, t)| {
                !t.shown && self.model.entries.get(w).is_some_and(|e| !e.held && !e.collapsed)
            })
            .map(|(w, _)| *w)
            .collect()
    }

    /// The overlay handle of a window that holds one.
    #[must_use]
    pub fn handle_of(&self, window: WindowInstance) -> Option<OverlayHandle> {
        self.ledger.get(&window).and_then(|t| t.handle)
    }

    /// Counters and capability gaps.
    #[must_use]
    pub fn diagnostics(&self) -> BorderDiagnostics {
        let route = self.route.counters();
        BorderDiagnostics {
            overlay: self.route.control().diagnostics(),
            spaces: self.gate.diagnostic(),
            engine: self.counters,
            late_hides: route.late_hides,
            stale_frames: route.stale,
            live: self.live.as_ref().and_then(|live| live.source.status().diagnostic()),
            live_moves: route.live_moves,
            main_busy: self.main_busy,
        }
    }

    /// When the actor should call [`Self::tick`] next, if a border waits only
    /// for its display frame.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.suspended() {
            return None;
        }
        let ready = self.ledger.values().filter(|t| !t.shown).filter_map(|t| t.ready_at).min();
        let holds = self
            .holds
            .iter()
            .filter(|(window, _)| !self.model.pressed.contains(window))
            .map(|(_, hold)| hold.last + HOLD_QUIET)
            .min();
        // The only deadline besides a pending show: the oldest batch the main
        // thread has not acknowledged yet, until it is flagged. Acknowledged
        // batches and an idle engine schedule nothing.
        // While one is late the engine looks again every `BORDER_ACK_WAIT` so the
        // flag clears when main catches up; acknowledged batches schedule nothing.
        let ack = self
            .tickets
            .iter()
            .filter(|(_, ticket)| ticket.try_get().is_none())
            .map(|(sent, _)| {
                (*sent + BORDER_ACK_WAIT)
                    .max(self.now + BORDER_ACK_WAIT * u32::from(self.main_busy))
            })
            .min();
        let collapse =
            self.ledger.values().filter(|t| t.shown).filter_map(|t| t.collapse_until).min();
        let reorder = self.live_reorders.values().filter_map(|r| r.pending).min();
        [ready, holds, ack, collapse, reorder].into_iter().flatten().min()
    }

    // ---- inputs ------------------------------------------------------------

    /// Diffs `model` against the ledger and sends the differences as one batch.
    pub fn update(&mut self, now: Instant, model: BorderModel) {
        self.now = now;
        self.reconcile_route();
        self.counters.missing_cg_id = model.missing_cg_id as u64;
        self.sync_fullscreen(now, &model.fullscreen);
        self.model = model;
        let mut batch = self.new_batch();
        let model = std::mem::take(&mut self.model);

        // Holds and notes about windows the model no longer lists are over.
        self.destroyed.retain(|w| model.entries.contains_key(w));
        self.live_hold.retain(|w| model.entries.contains_key(w));
        self.holds.retain(|w, _| model.entries.contains_key(w));
        for window in std::mem::take(&mut self.to_release) {
            self.drop_window(window, &mut batch);
        }
        let gone: Vec<WindowInstance> =
            self.ledger.keys().filter(|w| !model.entries.contains_key(w)).copied().collect();
        for window in gone {
            self.drop_window(window, &mut batch);
        }
        for entry in model.entries.values() {
            self.route.set_busy(entry.window, entry.write_pending.is_some());
        }
        for entry in model.entries.values() {
            self.diff_entry(now, entry, &mut batch);
        }
        self.model = model;
        self.finish_collapses(now, &mut batch);
        self.release_holds(now);
        self.arm(now);
        let mut shows = self.fire_ready(now, &mut batch);
        let refused = self.flush(batch);
        self.after_flush(&refused, &mut shows);
        self.sync_live_targets();
        self.live_reorders.retain(|w, _| self.model.entries.contains_key(w));
        self.fire_live_reorders(now);
    }

    /// Recomputes appearances for a new configuration in place (instant, no
    /// cross-fade) and sets the engine-wide transition.
    pub fn configure(&mut self, config: &BordersConfig) {
        self.config = config.clone();
        self.route.control().set_transition(transition_for(config));
        self.model.reappearance(config);
        let mut batch = self.new_batch();
        let windows: Vec<WindowInstance> = self.ledger.keys().copied().collect();
        for window in windows {
            let Some(tracked) = self.ledger.get(&window) else {
                continue;
            };
            match self.model.entries.get(&window).cloned() {
                None => self.drop_window(window, &mut batch),
                // Part 09: a reload is instant: a shrinking border hides now.
                Some(entry) if entry.collapsed => {
                    let shown = tracked.shown;
                    let handle = tracked.handle;
                    if let Some(tracked) = self.ledger.get_mut(&window) {
                        tracked.shown = false;
                        tracked.collapsed = true;
                        tracked.collapse_until = None;
                        tracked.ready_at = None;
                    }
                    if let (true, Some(handle)) = (shown, handle) {
                        batch = batch.with(handle, OverlayKind::Hide);
                        self.route.set_hidden(window);
                    }
                }
                Some(entry) => {
                    // A reload that makes a shrinking entry drawn again ends
                    // the shrink: no release at the old deadline, and the
                    // overlay draws the full ring at once.
                    let was_collapsed = tracked.collapsed;
                    let shown = tracked.shown;
                    let changed = tracked.appearance != entry.appearance;
                    if let Some(tracked) = self.ledger.get_mut(&window) {
                        tracked.collapsed = false;
                        tracked.collapse_until = None;
                    }
                    if shown && (changed || was_collapsed) {
                        self.send_show(&entry, AppearanceChange::Reload, &mut batch);
                    }
                }
            }
        }
        let refused = self.flush(batch);
        self.after_flush(&refused, &mut Vec::new());
    }

    /// Shows every pending border whose conditions hold and whose frame has
    /// passed. Returns how many were shown.
    pub fn tick(&mut self, now: Instant) -> usize {
        self.now = now;
        self.poll_tickets();
        self.reconcile_route();
        self.release_holds(now);
        // A confirming frame reported since the last call may have opened a gate.
        self.arm(now);
        let mut batch = self.new_batch();
        for window in std::mem::take(&mut self.to_release) {
            self.drop_window(window, &mut batch);
        }
        self.finish_collapses(now, &mut batch);
        let mut shows = self.fire_ready(now, &mut batch);
        let count = shows.len();
        let refused = self.flush(batch);
        self.after_flush(&refused, &mut shows);
        self.sync_live_targets();
        self.fire_live_reorders(now);
        count
    }

    /// Queues the `Order` for a window-server reorder of `window` (live
    /// follow). Only an `above` border reacts; it is due at once, or at the end
    /// of the display frame of the last such `Order` (several notifications in
    /// one frame give one `Order`).
    fn note_live_reorder(&mut self, window: WindowInstance) {
        if !self.placement_above(window) || !self.ledger.contains_key(&window) {
            return;
        }
        let frame = self.frame_period;
        let now = self.now;
        let state = self.live_reorders.entry(window).or_default();
        if state.pending.is_some() {
            self.counters.live_reorders_coalesced += 1;
            return;
        }
        let due = state.last.map_or(now, |last| (last + frame).max(now));
        if due > now {
            self.counters.live_reorders_coalesced += 1;
        }
        state.pending = Some(due);
    }

    /// Sends the reorder `Order`s that are due, at most [`LIVE_REORDER_CAP`]
    /// per window per [`LIVE_REORDER_WINDOW`] (then one diagnostic per burst).
    /// Suspended (Space gate), `reorder` sends nothing.
    fn fire_live_reorders(&mut self, now: Instant) {
        let due: Vec<WindowInstance> = self
            .live_reorders
            .iter()
            .filter(|(_, r)| r.pending.is_some_and(|at| at <= now))
            .map(|(w, _)| *w)
            .collect();
        for window in due {
            let Some(state) = self.live_reorders.get_mut(&window) else {
                continue;
            };
            state.pending = None;
            while state
                .recent
                .front()
                .is_some_and(|at| now.saturating_duration_since(*at) >= LIVE_REORDER_WINDOW)
            {
                state.recent.pop_front();
            }
            if state.recent.len() >= LIVE_REORDER_CAP {
                self.counters.live_reorders_capped += 1;
                if !state.capped {
                    state.capped = true;
                    self.route.log().debug(|| {
                        format!(
                            "live reorder cap reached window={window:?} cap={LIVE_REORDER_CAP}/{}ms; further reorders dropped",
                            LIVE_REORDER_WINDOW.as_millis()
                        )
                    });
                }
                continue;
            }
            state.capped = false;
            if !self.placement_above(window) {
                continue;
            }
            if self.reorder(window) {
                self.counters.live_reorders += 1;
                if let Some(state) = self.live_reorders.get_mut(&window) {
                    state.last = Some(now);
                    state.recent.push_back(now);
                }
            }
        }
    }

    /// Whether `window`'s border is ordered directly above it, and so has to be
    /// re-ordered when the window is raised. A `Top` border (the focused
    /// window's, one level higher) cannot be covered and is left alone.
    fn placement_above(&self, window: WindowInstance) -> bool {
        self.model
            .entries
            .get(&window)
            .is_some_and(|entry| entry.appearance.placement == BorderPlacement::Above)
    }

    /// The focused window changed: its border is ordered once now, and that is
    /// all. The focused window's `above` border is placed one window level up
    /// by the model ([`BorderPlacement::Top`]), where a later raise of its
    /// window (a click in it, its application's activation) cannot cover it,
    /// so nothing is re-ordered afterwards.
    pub fn note_focus(&mut self, now: Instant, window: Option<WindowInstance>) {
        self.now = now;
        self.focus = window;
        if let Some(window) = window {
            self.reorder(window);
        }
    }

    /// The application's windows are shown again (`Observation::Shown`).
    pub fn note_application_shown(&mut self, now: Instant, application: ApplicationId) {
        self.reconcile_route();
        self.route.note_application_shown(application);
        // A shown application is a confirming event for its windows the Space
        // check refused: one more attempt each (no polling).
        for tracked in self.ledger.values_mut().filter(|t| t.application == application) {
            tracked.unconfirmed = false;
        }
        self.recount_unconfirmed();
        self.arm(now);
    }

    /// The reveal frame write of `window` for `plan_sequence` completed (or a
    /// fresh snapshot confirmed its frame after an external show).
    pub fn note_reveal_written(
        &mut self,
        now: Instant,
        window: WindowInstance,
        plan_sequence: u64,
    ) {
        self.reconcile_route();
        self.route.confirm(window, plan_sequence);
        self.arm(now);
    }

    /// Hides the application's borders at once (an external hide observed
    /// after the fact; also callable before `HideApp` on the actor thread).
    /// Returns the acknowledgement to wait for, if the caller wants to.
    pub fn hide_application(&mut self, application: ApplicationId) -> HideRequest {
        let request = self.route.hide_application(application);
        self.reconcile_route();
        request
    }

    /// Re-orders the window's overlay above its target (after `Effect::Focus`,
    /// or when another bordered window is observed focused). Dropped while
    /// suspended. Returns whether an `Order` was sent.
    pub fn reorder(&mut self, window: WindowInstance) -> bool {
        if self.suspended() {
            self.counters.suppressed_orders += 1;
            return false;
        }
        let Some(tracked) = self.ledger.get(&window).filter(|t| t.shown) else {
            return false;
        };
        let (Some(handle), cg) = (tracked.handle, tracked.cg) else {
            return false;
        };
        let batch = self.new_batch().with(handle, OverlayKind::Order { target_cg_id: cg });
        let refused = self.flush(batch);
        self.after_flush(&refused, &mut Vec::new());
        true
    }

    /// Re-orders the overlays of the windows Tiling raised since the last call
    /// (the effects thread reports them through the route).
    /// Also sends the due window-server reorder `Order`s (live follow).
    pub fn reorder_raised(&mut self, now: Instant) {
        self.now = now;
        self.reconcile_route();
        self.fire_live_reorders(now);
        for window in self.route.drain_raised() {
            self.reorder(window);
        }
    }

    // ---- Spaces and Mission Control (part 05) -------------------------------

    /// Starts the Space event source (once per enabled period). The gate starts
    /// in `Unknown { SourcesNotStarted }` and opens only after the source
    /// reports that it is registered and a confirmation succeeded. Never blocks.
    pub fn start_spaces(&mut self) {
        if self.spaces_started {
            return;
        }
        self.spaces_started = true;
        self.gate = SpaceGate::new();
        self.reenter_fullscreen();
        self.mailbox.reopen();
        self.source.start(self.mailbox.sink());
        self.route
            .log()
            .debug(|| "spaces source start requested; gate Unknown(SourcesNotStarted)".to_owned());
    }

    /// Stops the source and discards what it queued. Call before the overlays
    /// are released; afterwards no fact arrives. The gate is closed again.
    ///
    /// Returns the typed outcome: anything but `Stopped`/`NotRunning` is a stuck
    /// owner that keeps its thread and is retried by a later stop.
    pub fn stop_spaces(&mut self) -> StopOutcome {
        if !self.spaces_started && !self.spaces_stuck {
            return StopOutcome::NotRunning;
        }
        self.spaces_started = false;
        let outcome = self.source.stop();
        self.spaces_stuck = matches!(outcome, StopOutcome::TimedOut { .. });
        self.mailbox.close();
        self.gate = SpaceGate::new();
        self.route.log().debug(|| format!("spaces source stop outcome={outcome:?}"));
        outcome
    }

    /// Ends the engine's claim on its overlays *without sending anything* and
    /// returns how many handles it accounted for: the ledger's handles plus the
    /// releases it could not send because the route was already closed. The
    /// caller then releases the control and compares this number with
    /// [`stache_macos::border_overlay::ReleaseAck::handles`]. The ledger is empty
    /// afterwards.
    pub fn retire_ledger(&mut self) -> usize {
        let expected = self.handles_in_use() + std::mem::take(&mut self.unsent_releases);
        self.ledger.clear();
        self.recount_unconfirmed();
        self.to_release.clear();
        self.holds.clear();
        self.live_hold.clear();
        self.destroyed.clear();
        self.model = BorderModel::default();
        self.route.clear_windows();
        expected
    }

    /// Hands out the owners whose stop timed out so the lifecycle can retry them
    /// after the engine is gone (never dropped while stuck, never detached).
    pub fn take_stuck(
        &mut self,
        live: StopOutcome,
        spaces: StopOutcome,
    ) -> Vec<super::lifecycle::RetainedOwner> {
        use super::lifecycle::RetainedOwner;
        let mut out = Vec::new();
        if matches!(live, StopOutcome::TimedOut { .. })
            && let Some(side) = self.live.take()
        {
            out.push(RetainedOwner::Live(side.source));
        }
        if matches!(spaces, StopOutcome::TimedOut { .. }) {
            out.push(RetainedOwner::Spaces(std::mem::replace(
                &mut self.source,
                Box::new(NoSpaceEvents),
            )));
        }
        out
    }

    /// Drains the Space mailbox into the gate. The actor calls it at the start
    /// of every turn and before a tick, ahead of the border update.
    pub fn poll_spaces(&mut self, now: Instant) {
        if !self.spaces_started || !self.mailbox.has_pending() {
            return;
        }
        let drained = self.mailbox.drain();
        self.route.log().debug(|| {
            format!(
                "space facts drained count={} overflowed={}",
                drained.facts.len(),
                drained.overflowed
            )
        });
        if drained.overflowed {
            self.counters.space_overflows += 1;
            let effect = self.gate.overflow();
            self.run_effect(effect);
            // The overflow forgot every reason, including the fullscreen ones the
            // engine itself reported (the source knows nothing of them).
            self.reenter_fullscreen();
            self.source.resync();
        }
        for fact in drained.facts {
            self.counters.space_facts += 1;
            self.handle_fact(now, fact);
        }
    }

    /// Reports the fullscreen transitions of the model to the gate. The engine,
    /// not the native source, owns this signal: the flag comes from the Tiling
    /// state (the window adapter's `AXFullScreen` read), so no extra observer
    /// exists. The latency is the adapter's: a window is hidden when its flag
    /// flips in the state, which is after the first `AXWindowResized` or
    /// `AXWindowMoved` of the transition was read (unverified whether that is
    /// before the first animation frame; the native check measures it).
    fn sync_fullscreen(&mut self, now: Instant, current: &BTreeSet<WindowInstance>) {
        if !self.spaces_started {
            self.fullscreen.clone_from(current);
            return;
        }
        let entering: Vec<WindowInstance> = current.difference(&self.fullscreen).copied().collect();
        let leaving: Vec<WindowInstance> = self.fullscreen.difference(current).copied().collect();
        self.fullscreen.clone_from(current);
        for window in entering {
            self.route.log().debug(|| format!("fullscreen enter window={window:?}"));
            self.handle_fact(now, SpaceFact::FullscreenChanged { window, entering: true });
        }
        for window in leaving {
            self.route.log().debug(|| format!("fullscreen exit window={window:?}"));
            self.handle_fact(now, SpaceFact::FullscreenChanged { window, entering: false });
        }
    }

    /// Re-enters the window-scoped reason of every window known to be fullscreen
    /// (after the gate was recreated or an overflow forgot its reasons).
    fn reenter_fullscreen(&mut self) {
        for window in self.fullscreen.clone() {
            let effect = self.gate.enter(SuspendReason::FullscreenTransition { window });
            self.run_effect(effect);
        }
    }

    /// Feeds one fact to the gate and carries out what it asks.
    pub fn handle_fact(&mut self, now: Instant, fact: SpaceFact) {
        self.route.log().debug(|| {
            format!(
                "space fact {fact:?} epoch={} suspended={}",
                self.gate.epoch(),
                self.suspended()
            )
        });
        let step = self.gate.apply(fact);
        self.run_effect(step.effect);
        if let Some(resume) = step.resume {
            self.resume_from(now, resume);
        }
    }

    /// Enters a suspension reason (tests and non-source callers). A global
    /// reason hides every overlay at once (`HideAll`) and bumps the gate epoch;
    /// no `Show` or `Order` is sent until the last global reason is resumed.
    pub fn suspend(&mut self, reason: SuspendReason) {
        let effect = self.gate.enter(reason);
        self.run_effect(effect);
    }

    /// Leaves a suspension reason. Leaving the last global one re-confirms
    /// every window on the active Space, then the show rule applies with one
    /// frame.
    pub fn resume(&mut self, now: Instant, reason: SuspendReason) {
        if let Some(resume) = self.gate.leave(reason) {
            self.resume_from(now, resume);
        }
    }

    /// Re-confirms windows the Space check refused (callers use it when the
    /// Space state changes). A failure holds `Unknown { QueryFailed }`.
    pub fn reconfirm(&mut self, now: Instant) {
        let windows: Vec<(WindowInstance, u32)> = self
            .ledger
            .iter()
            .filter(|(_, t)| t.unconfirmed)
            .map(|(w, t)| (*w, t.cg))
            .collect();
        let Ok(confirmed) = self.confirmer.confirm_on_active_space(&windows) else {
            self.query_failed();
            return;
        };
        for (window, tracked) in &mut self.ledger {
            if confirmed.contains(window) {
                tracked.unconfirmed = false;
            }
        }
        self.recount_unconfirmed();
        self.arm(now);
    }

    /// Hides what a newly entered reason asks to hide.
    fn run_effect(&mut self, effect: Option<GateEffect>) {
        match effect {
            None => {}
            Some(GateEffect::HideAll { reason, epoch }) => {
                self.route.log().debug(|| {
                    format!(
                        "gate suspend scope=global reason={reason:?} new_epoch={epoch} shown={}",
                        self.ledger.values().filter(|t| t.shown).count()
                    )
                });
                self.counters.hide_alls += 1;
                let refused = self.flush(OverlayBatch::hide_all(reason).at_epoch(epoch));
                self.after_flush(&refused, &mut Vec::new());
                for (window, tracked) in &mut self.ledger {
                    tracked.shown = false;
                    tracked.ready_at = None;
                    self.route.set_hidden(*window);
                }
            }
            Some(GateEffect::HideWindow { window }) => {
                let Some(tracked) = self.ledger.get_mut(&window) else {
                    return;
                };
                tracked.ready_at = None;
                let was_shown = std::mem::replace(&mut tracked.shown, false);
                let handle = tracked.handle;
                self.route.log().debug(|| {
                    format!(
                        "gate suspend scope=window window={window:?} was_shown={was_shown} \
                         epoch={}",
                        self.gate.epoch()
                    )
                });
                self.route.set_hidden(window);
                if let (true, Some(handle)) = (was_shown, handle) {
                    self.counters.window_hides += 1;
                    let batch = self.new_batch().with(handle, OverlayKind::Hide);
                    let refused = self.flush(batch);
                    self.after_flush(&refused, &mut Vec::new());
                }
            }
        }
    }

    /// Re-confirms after a suspension ended: every bordered window, or the one
    /// whose window-scoped reason cleared. Then the show rule applies.
    fn resume_from(&mut self, now: Instant, resume: Resume) {
        let windows: Vec<(WindowInstance, u32)> = match resume {
            Resume::All => self.ledger.iter().map(|(w, t)| (*w, t.cg)).collect(),
            Resume::Window(window) => {
                self.ledger.get(&window).map(|t| (window, t.cg)).into_iter().collect()
            }
        };
        self.route.log().debug(|| {
            format!(
                "gate resume {resume:?} candidates={} epoch={}",
                windows.len(),
                self.gate.epoch()
            )
        });
        let Ok(confirmed) = self.confirmer.confirm_on_active_space(&windows) else {
            self.query_failed();
            return;
        };
        for (window, _) in &windows {
            let seen = self.route.confirmations(*window);
            if let Some(tracked) = self.ledger.get_mut(window) {
                tracked.unconfirmed = !confirmed.contains(window);
                tracked.confirmations_seen = seen;
            }
        }
        self.recount_unconfirmed();
        if resume == Resume::All {
            self.gate.confirmed();
        }
        self.route.log().debug(|| {
            format!(
                "gate resumed {resume:?} confirmed={}/{} epoch={} suspended={}",
                confirmed.len(),
                windows.len(),
                self.gate.epoch(),
                self.suspended()
            )
        });
        self.arm(now);
    }

    /// The Space query failed: every border stays (or becomes) hidden.
    fn query_failed(&mut self) {
        self.route
            .log()
            .debug(|| "gate Space query failed; Unknown(QueryFailed)".to_owned());
        let effect = self.gate.enter(SuspendReason::Unknown {
            cause: UnknownCause::QueryFailed,
        });
        self.run_effect(effect);
    }

    const fn new_batch(&self) -> OverlayBatch { OverlayBatch::new().at_epoch(self.gate.epoch()) }

    /// Releases every window the engine tracks (shutdown of the engine, not of
    /// the control): the ledger is empty afterwards.
    pub fn clear(&mut self) {
        let mut batch = self.new_batch();
        let windows: Vec<WindowInstance> = self.ledger.keys().copied().collect();
        for window in windows {
            self.drop_window(window, &mut batch);
        }
        let refused = self.flush(batch);
        self.after_flush(&refused, &mut Vec::new());
    }

    // ---- internals ---------------------------------------------------------

    /// Applies hides the effects thread made through the route.
    fn reconcile_route(&mut self) {
        self.note_live_status();
        for window in self.route.drain_hidden() {
            if let Some(tracked) = self.ledger.get_mut(&window) {
                tracked.shown = false;
                tracked.ready_at = None;
            }
        }
        let notes = self.route.drain_live();
        if !(notes.hidden.is_empty()
            && notes.unhidden.is_empty()
            && notes.closed.is_empty()
            && notes.reordered.is_empty())
        {
            self.route.log().debug(|| {
                format!(
                    "live notes drained hidden={:?} unhidden={:?} closed={:?} reordered={:?}",
                    notes.hidden, notes.unhidden, notes.closed, notes.reordered
                )
            });
        }
        // A private hide holds the border hidden until the window server unhides
        // the window (or the model drops it); the unhide only lifts the hold.
        for window in notes.hidden {
            if self.ledger.contains_key(&window) {
                self.live_hold.insert(window);
            }
        }
        for window in notes.unhidden {
            self.live_hold.remove(&window);
        }
        for window in notes.reordered {
            self.note_live_reorder(window);
        }
        for window in notes.closed {
            if self.ledger.contains_key(&window) {
                self.destroyed.insert(window);
                self.live_hold.remove(&window);
                self.holds.remove(&window);
                self.to_release.push(window);
            }
        }
    }

    /// Logs a change of the live follow status (actor side, on the drain).
    fn note_live_status(&mut self) {
        if !self.route.log().enabled() {
            return;
        }
        let Some(status) = self.live.as_ref().map(|live| live.source.status()) else {
            return;
        };
        if self.live_seen.as_ref() == Some(&status) {
            return;
        }
        self.route.log().debug(|| match &status {
            LiveStatus::Unavailable(reason) => {
                format!("live follow status Unavailable reason={reason:?}")
            }
            other => format!("live follow status {other:?}"),
        });
        self.live_seen = Some(status);
    }

    /// One debug line for a batch about to be sent: command counts and the epoch.
    fn log_batch(&self, label: &str, batch: &OverlayBatch) {
        self.route.log().debug(|| {
            let count = |wanted: fn(&OverlayKind) -> bool| {
                batch.commands.iter().filter(|command| wanted(&command.kind)).count()
            };
            format!(
                "{label} epoch={} show={} hide={} release={} move={} order={} hide_all={:?}",
                batch.epoch,
                count(|kind| matches!(kind, OverlayKind::Show { .. })),
                count(|kind| matches!(kind, OverlayKind::Hide)),
                count(|kind| matches!(kind, OverlayKind::Release)),
                count(|kind| matches!(kind, OverlayKind::Move { .. })),
                count(|kind| matches!(kind, OverlayKind::Order { .. })),
                batch.hide_all
            )
        });
    }

    // ---- public fallback (part 06) ------------------------------------------

    /// Creates or refreshes the hold of `window` at `now`; a shown border hides.
    fn hold_window(&mut self, window: WindowInstance, now: Instant, batch: &mut OverlayBatch) {
        let epoch = self.gate.epoch();
        if self.holds.insert(window, Hold { last: now }).is_none() {
            self.counters.fallback_holds += 1;
            self.route.log().debug(|| {
                format!(
                    "hold start window={window:?} trigger=external-frame-change live_active={}",
                    self.live_active()
                )
            });
        }
        let Some(tracked) = self.ledger.get_mut(&window) else {
            return;
        };
        tracked.ready_at = None;
        if std::mem::replace(&mut tracked.shown, false)
            && let Some(handle) = tracked.handle
        {
            batch.push_at(handle, OverlayKind::Hide, epoch);
            self.route.set_hidden(window);
        }
    }

    /// Ends the holds whose trigger is gone and that saw no frame change for
    /// [`HOLD_QUIET`]; the show rule then applies as for any pending window.
    fn release_holds(&mut self, now: Instant) {
        let pressed = &self.model.pressed;
        let log = self.route.log();
        self.holds.retain(|window, hold| {
            let keep =
                pressed.contains(window) || now.saturating_duration_since(hold.last) < HOLD_QUIET;
            if !keep {
                log.debug(|| {
                    format!(
                        "hold end window={window:?} quiet_ms={:.1}",
                        now.saturating_duration_since(hold.last).as_secs_f64() * 1000.0
                    )
                });
            }
            keep
        });
    }

    /// Tells the live follow source which windows are bordered, only when the
    /// set changed. Bounded by the source to `MAX_OVERLAYS`.
    fn sync_live_targets(&mut self) {
        if !self.live.as_ref().is_some_and(|live| live.started) {
            return;
        }
        let targets: Vec<FollowTarget> = self
            .ledger
            .iter()
            .filter(|(window, tracked)| {
                !self.destroyed.contains(window) && (tracked.shown || !tracked.collapsed)
            })
            .take(MAX_OVERLAYS)
            .map(|(window, tracked)| FollowTarget {
                window: *window,
                cg_window_id: tracked.cg,
                display: tracked.display,
                owner_pid: Some(tracked.owner_pid),
                overlay_window: tracked.handle.and_then(|h| self.route.control().window_number(h)),
                above: self.placement_above(*window),
                // Shown under a known, open gate; anything else (unknown,
                // suspended, window-blocked, hidden) never fast-reorders.
                fast_reorder: tracked.shown
                    && !self.gate.suspended()
                    && !self.gate.window_blocked(*window),
            })
            .collect();
        let Some(live) = self.live.as_mut() else { return };
        if targets != live.targets {
            live.source.set_targets(&targets);
            live.targets = targets;
        }
    }

    /// The handles the engine accounts for right now (its ledger, plus releases
    /// it could not send after the route closed).
    #[must_use]
    pub fn ledger_handles(&self) -> usize { self.handles_in_use() + self.unsent_releases }

    fn handles_in_use(&self) -> usize {
        self.ledger.values().filter(|t| t.handle.is_some()).count()
    }

    fn drop_window(&mut self, window: WindowInstance, batch: &mut OverlayBatch) {
        let epoch = self.gate.epoch();
        if let Some(tracked) = self.ledger.remove(&window) {
            if tracked.unconfirmed {
                self.recount_unconfirmed();
            }
            if let Some(handle) = tracked.handle {
                batch.push_at(handle, OverlayKind::Release, epoch);
            }
        }
        self.route.remove(window);
    }

    /// The frame to draw: the newest the route knows, else the model's.
    fn frame_for(&self, entry: &BorderEntry) -> Rect {
        self.route.latest_frame(entry.window).unwrap_or(entry.frame)
    }

    fn send_show(
        &mut self,
        entry: &BorderEntry,
        change: AppearanceChange,
        batch: &mut OverlayBatch,
    ) {
        let frame = self.frame_for(entry);
        let epoch = self.gate.epoch();
        let Some(tracked) = self.ledger.get_mut(&entry.window) else {
            return;
        };
        let Some(handle) = tracked.handle else { return };
        tracked.appearance = entry.appearance;
        tracked.cg = entry.cg_window_id;
        batch.push_at(
            handle,
            OverlayKind::Show {
                target_cg_id: entry.cg_window_id,
                frame: overlay_rect(frame),
                appearance: entry.appearance,
                change,
            },
            epoch,
        );
    }

    /// Whether the model's frame change of `entry` is not Tiling's doing: no
    /// Tiling write in flight and the newest frame the route knows (reported by
    /// the frame paths or sent by the engine) is a different frame.
    fn external_change(&self, entry: &BorderEntry) -> bool {
        entry.write_pending.is_none()
            && self.route.latest_frame(entry.window).is_some_and(|known| {
                (known.x - entry.frame.x).abs() > FRAME_TOLERANCE
                    || (known.y - entry.frame.y).abs() > FRAME_TOLERANCE
                    || (known.width - entry.frame.width).abs() > FRAME_TOLERANCE
                    || (known.height - entry.frame.height).abs() > FRAME_TOLERANCE
            })
    }

    /// What an external frame change of `window` does to its border.
    fn external_policy(&self, window: WindowInstance) -> ExternalPolicy {
        if self.live.is_none() {
            ExternalPolicy::Apply
        } else if self.live_active() && self.route.followed_within(window, self.follow_grace) {
            ExternalPolicy::Suppress
        } else {
            ExternalPolicy::Hold
        }
    }

    /// Whether the route owns the border's frame for `entry`: a Tiling write is
    /// in flight and reported a frame.
    ///
    /// While a Tiling write is in flight the route owns the border's frame:
    /// it moves with each acknowledged frame of that write. The model's
    /// observed frame lags behind it (a coalesced read from the motion), so
    /// applying it would pull the border back mid-animation. It is not
    /// consumed either: a write that ends without reporting a frame (a
    /// partial, cancelled or sealed one) leaves it to be applied then.
    fn tiling_owns(&self, entry: &BorderEntry) -> bool {
        entry
            .write_pending
            .is_some_and(|plan| self.route.reported(entry.window) >= plan)
    }

    /// Whether a write that held the model frame back just ended, and if so
    /// whether a confirming frame was reported for its plan. Its end is
    /// Tiling's own, never an external change: a confirming frame stays (the
    /// model may still show a frame from the motion); without one, the held
    /// model frame applies.
    fn ended_write(&self, entry: &BorderEntry) -> Option<bool> {
        self.ledger
            .get(&entry.window)
            .and_then(|tracked| tracked.owned_plan)
            .filter(|_| entry.write_pending.is_none())
            .map(|plan| self.route.confirmed(entry.window, plan))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one linear diff of a window's border"
    )]
    fn diff_entry(&mut self, now: Instant, entry: &BorderEntry, batch: &mut OverlayBatch) {
        let epoch = self.gate.epoch();
        if self.destroyed.contains(&entry.window) {
            return;
        }
        let Some(known) = self.ledger.get(&entry.window).map(|tracked| tracked.frame) else {
            if entry.held {
                return;
            }
            self.route.upsert(entry.window, entry.application, None, entry.plan_sequence);
            self.route.set_frame(entry.window, entry.frame);
            self.ledger.insert(entry.window, Tracked {
                application: entry.application,
                cg: entry.cg_window_id,
                handle: None,
                shown: false,
                appearance: entry.appearance,
                frame: entry.frame,
                ready_at: None,
                unconfirmed: false,
                confirmations_seen: 0,
                refused: false,
                display: entry.display,
                owner_pid: entry.owner_pid,
                collapsed: entry.collapsed,
                collapse_until: None,
                grow_next: false,
                owned_plan: None,
            });
            return;
        };
        let ended = self.ended_write(entry);
        // The public fallback and live follow decide what a frame change that
        // Tiling did not cause does (see the module documentation).
        let mut skip_move = false;
        if known != entry.frame && ended.is_none() {
            if let Some(hold) = self.holds.get_mut(&entry.window) {
                hold.last = now;
            } else if self.external_change(entry) {
                match self.external_policy(entry.window) {
                    ExternalPolicy::Apply => {}
                    ExternalPolicy::Suppress => {
                        skip_move = true;
                        self.counters.live_suppressed += 1;
                    }
                    ExternalPolicy::Hold => self.hold_window(entry.window, now, batch),
                }
            }
        }
        let tiling_owns = self.tiling_owns(entry);
        let Some(tracked) = self.ledger.get_mut(&entry.window) else {
            return;
        };
        tracked.application = entry.application;
        tracked.display = entry.display;
        tracked.owner_pid = entry.owner_pid;
        self.route.upsert(
            entry.window,
            entry.application,
            tracked.handle,
            entry.plan_sequence,
        );
        let frame_changed = ended != Some(true) && tracked.frame != entry.frame;
        if tiling_owns && tracked.shown {
            tracked.owned_plan = entry.write_pending;
        } else {
            tracked.frame = entry.frame;
            tracked.owned_plan = None;
        }
        let was_collapsed = std::mem::replace(&mut tracked.collapsed, entry.collapsed);
        if !tracked.shown {
            tracked.grow_next = (tracked.grow_next || was_collapsed) && !entry.collapsed;
            tracked.collapse_until = None;
            tracked.appearance = entry.appearance;
            tracked.cg = entry.cg_window_id;
            if frame_changed && !skip_move {
                self.route.set_frame(entry.window, entry.frame);
            }
            return;
        }
        let handle = tracked.handle;
        if entry.app_hidden || self.route.application_hidden(entry.application) {
            // The application is hidden under a shown border: hide it now.
            tracked.shown = false;
            tracked.ready_at = None;
            if let Some(handle) = handle {
                batch.push_at(handle, OverlayKind::Hide, epoch);
            }
            self.route.set_hidden(entry.window);
            return;
        }
        self.diff_appearance(now, entry, was_collapsed, batch);
        if frame_changed
            && !skip_move
            && !tiling_owns
            && let Some(handle) = handle
        {
            batch.push_at(
                handle,
                OverlayKind::Move {
                    frame: overlay_rect(entry.frame),
                },
                epoch,
            );
            self.route.set_frame(entry.window, entry.frame);
        }
    }

    /// Part 09: releases every shrunk overlay whose shrink is over. The panel
    /// was ordered in, drawing nothing, since the shrink ended; this `Release`
    /// orders it out and returns it to the pool, so an unfocused window holds
    /// no overlay (nor does a collapsed one some other hide reached first). Any earlier hide (application hide, `HideAll`, a window
    /// leaving the model) already ordered it out at once.
    fn finish_collapses(&mut self, now: Instant, batch: &mut OverlayBatch) {
        let epoch = self.gate.epoch();
        for (window, tracked) in &mut self.ledger {
            let due = tracked.shown && tracked.collapse_until.is_some_and(|at| at <= now);
            // A collapsed window hidden some other way (an application hide, a
            // `HideAll`) keeps no reservation either.
            let stale = !tracked.shown && tracked.collapsed && tracked.handle.is_some();
            if due || stale {
                tracked.shown = false;
                tracked.collapse_until = None;
                tracked.ready_at = None;
                if let Some(handle) = tracked.handle.take() {
                    batch.push_at(handle, OverlayKind::Release, epoch);
                }
                self.route.upsert(*window, tracked.application, None, 0);
            }
        }
    }

    /// Sends what a shown border's appearance change asks for: a cross-fade or
    /// reload, or (part 09) a shrink when focus left and a grow when it came
    /// back mid-shrink.
    fn diff_appearance(
        &mut self,
        now: Instant,
        entry: &BorderEntry,
        was_collapsed: bool,
        batch: &mut OverlayBatch,
    ) {
        let epoch = self.gate.epoch();
        let latest = self.frame_for(entry);
        let Some(tracked) = self.ledger.get_mut(&entry.window) else {
            return;
        };
        let handle = tracked.handle;
        let target_changed = tracked.cg != entry.cg_window_id;
        let appearance_changed = tracked.appearance != entry.appearance;
        match (was_collapsed, entry.collapsed) {
            // Part 09: focus left: shrink the look last drawn, hide when due.
            // It is placed as the entry says: a window that lost the focus is
            // no longer on top, and must not shrink over the one that took it.
            (false, true) => {
                tracked.collapse_until = transition_for(&self.config)
                    .and_then(Transition::effective)
                    .map(|duration| now + duration);
                if let Some(handle) = handle {
                    batch.push_at(
                        handle,
                        OverlayKind::Show {
                            target_cg_id: entry.cg_window_id,
                            frame: overlay_rect(latest),
                            appearance: BorderAppearance {
                                placement: entry.appearance.placement,
                                ..tracked.appearance
                            },
                            change: AppearanceChange::Shrink,
                        },
                        epoch,
                    );
                }
                tracked.cg = entry.cg_window_id;
            }
            // Shrinking: only the frame follows.
            (true, true) => {}
            // Focus came back mid-shrink: grow from the presented width.
            (true, false) => {
                tracked.collapse_until = None;
                self.send_show(entry, AppearanceChange::Grow, batch);
            }
            (false, false) => {
                if target_changed {
                    self.send_show(entry, AppearanceChange::Reload, batch);
                } else if appearance_changed {
                    self.send_show(entry, AppearanceChange::StateChange, batch);
                }
            }
        }
    }

    fn gates_open(&self, window: WindowInstance, tracked: &Tracked) -> bool {
        let Some(entry) = self.model.entries.get(&window) else {
            return false;
        };
        !entry.held
            && !entry.collapsed
            && !entry.app_hidden
            && !self.holds.contains_key(&window)
            && !self.live_hold.contains(&window)
            && !self.destroyed.contains(&window)
            && !tracked.unconfirmed
            && !self.gate.window_blocked(window)
            && !self.route.application_hidden(tracked.application)
            && entry.write_pending.is_none_or(|plan| self.route.confirmed(window, plan))
    }

    /// A window the Space confirmation refused gets one more attempt after each
    /// confirming event of its own (a `Settled`/`Readback` frame or a completed
    /// reveal write): it is marked confirmable again, the show rule arms it and
    /// `fire_ready` asks the confirmer once. Never polled.
    fn retry_unconfirmed(&mut self) {
        let mut changed = false;
        for (window, tracked) in &mut self.ledger {
            if tracked.unconfirmed && self.route.confirmations(*window) > tracked.confirmations_seen
            {
                tracked.unconfirmed = false;
                changed = true;
            }
        }
        if changed {
            self.recount_unconfirmed();
        }
    }

    /// `counters.unconfirmed` is the number of tracked windows currently
    /// unconfirmed, recounted from the ledger whenever the flag changes.
    fn recount_unconfirmed(&mut self) {
        self.counters.unconfirmed = self.ledger.values().filter(|t| t.unconfirmed).count() as u64;
    }

    /// Starts or cancels the display-frame countdown of every pending window.
    fn arm(&mut self, now: Instant) {
        if self.suspended() {
            return;
        }
        self.retry_unconfirmed();
        let in_use = self.handles_in_use();
        let open: Vec<(WindowInstance, bool)> = self
            .ledger
            .iter()
            .filter(|(_, t)| !t.shown)
            .map(|(w, t)| (*w, self.gates_open(*w, t)))
            .collect();
        let period = self.frame_period;
        for (window, is_open) in open {
            let Some(tracked) = self.ledger.get_mut(&window) else {
                continue;
            };
            if tracked.refused && in_use < MAX_OVERLAYS {
                tracked.refused = false;
            }
            if is_open && !tracked.refused {
                tracked.ready_at.get_or_insert(now + period);
            } else {
                tracked.ready_at = None;
            }
        }
    }

    /// Adds a `Show` for every window that is ready; returns what was added so
    /// the caller can settle the route after the batch was sent.
    fn fire_ready(
        &mut self,
        now: Instant,
        batch: &mut OverlayBatch,
    ) -> Vec<(WindowInstance, Rect)> {
        if self.suspended() {
            return Vec::new();
        }
        let due: Vec<WindowInstance> = self
            .ledger
            .iter()
            .filter(|(w, t)| {
                !t.shown && t.ready_at.is_some_and(|at| at <= now) && self.gates_open(**w, t)
            })
            .map(|(w, _)| *w)
            .collect();
        if due.is_empty() {
            return Vec::new();
        }
        let candidates: Vec<(WindowInstance, u32)> =
            due.iter().filter_map(|w| self.ledger.get(w).map(|t| (*w, t.cg))).collect();
        let Ok(confirmed) = self.confirmer.confirm_on_active_space(&candidates) else {
            self.query_failed();
            return Vec::new();
        };
        let mut shows = Vec::new();
        for window in due {
            let Some(entry) = self.model.entries.get(&window).cloned() else {
                continue;
            };
            if !confirmed.contains(&window) {
                let seen = self.route.confirmations(window);
                if let Some(tracked) = self.ledger.get_mut(&window) {
                    tracked.unconfirmed = true;
                    tracked.confirmations_seen = seen;
                    tracked.ready_at = None;
                }
                self.recount_unconfirmed();
                continue;
            }
            let in_use = self.handles_in_use();
            let Some(tracked) = self.ledger.get_mut(&window) else {
                continue;
            };
            if tracked.handle.is_none() && in_use >= MAX_OVERLAYS {
                tracked.refused = true;
                tracked.ready_at = None;
                self.counters.refused_shows += 1;
                continue;
            }
            let handle = *tracked.handle.get_or_insert_with(OverlayHandle::next);
            let ready_at = tracked.ready_at;
            tracked.shown = true;
            tracked.ready_at = None;
            tracked.collapse_until = None;
            let change = if std::mem::take(&mut tracked.grow_next) {
                AppearanceChange::Grow
            } else {
                AppearanceChange::Reload
            };
            self.route.upsert(window, entry.application, Some(handle), entry.plan_sequence);
            let frame = self.frame_for(&entry);
            self.send_show(&entry, change, batch);
            self.route.log().debug(|| {
                format!(
                    "show window={window:?} cg={} handle={handle:?} released_by=show-rule \
                     (app visible, reveal_plan={:?} confirmed, frame passed, late_us={}) epoch={}",
                    entry.cg_window_id,
                    entry.write_pending,
                    ready_at.map_or(0, |at| now.saturating_duration_since(at).as_micros()),
                    self.gate.epoch()
                )
            });
            shows.push((window, frame));
        }
        shows
    }

    fn flush(&mut self, batch: OverlayBatch) -> Vec<Refusal> {
        self.poll_tickets();
        if self.route.is_closed() {
            self.unsent_releases += batch
                .commands
                .iter()
                .filter(|command| matches!(command.kind, OverlayKind::Release))
                .count();
            return Vec::new();
        }
        if batch.hide_all.is_none() && batch.commands.is_empty() {
            return Vec::new();
        }
        // A batch that hides anything first withdraws the fast reorder of the
        // live-follow owner (best effort; the main thread's panel record is
        // the hard guard, see `panel_registry`).
        if batch.hide_all.is_some()
            || batch
                .commands
                .iter()
                .any(|command| matches!(command.kind, OverlayKind::Hide | OverlayKind::Release))
        {
            self.sync_live_targets();
        }
        #[cfg(test)]
        self.sent.push(batch.clone());
        self.log_batch("batch sent", &batch);
        let receipt = self.route.control().apply(batch);
        self.route.note_ticket(receipt.ticket.clone());
        if self.tickets.len() >= MAX_TICKETS {
            self.tickets.remove(0);
        }
        self.tickets.push((self.now, receipt.ticket));
        receipt.refused
    }

    /// Counts commands that failed natively, from acknowledgements that came in.
    fn poll_tickets(&mut self) {
        let mut failed = 0_u64;
        let now = self.now;
        let log = self.route.log();
        self.tickets.retain(|(sent, ticket)| {
            let result = ticket.try_get();
            match result {
                Some(Ok(ack)) => {
                    failed += u64::from(ack.summary.failed);
                    log.debug(|| {
                        let s = ack.summary;
                        format!(
                            "main applied shown={} moved={} ordered={} hidden={} released={} \
                             created={} reused={} refused={} failed={} hid_all={} \
                             stale_dropped={} acked_after_ms={:.1}",
                            s.shown,
                            s.moved,
                            s.ordered,
                            s.hidden,
                            s.released,
                            s.created,
                            s.reused,
                            s.refused_shows,
                            s.failed,
                            s.hid_all,
                            s.stale_dropped,
                            now.saturating_duration_since(*sent).as_secs_f64() * 1000.0
                        )
                    });
                }
                Some(Err(error)) => {
                    log.debug(|| format!("main apply not acknowledged error={error:?}"));
                }
                None => {}
            }
            result.is_none()
        });
        self.counters.failed_commands += failed;
        // What is left is unacknowledged: busy once one has waited long enough,
        // and not busy again as soon as none is left.
        let busy = self
            .tickets
            .iter()
            .any(|(sent, _)| now.saturating_duration_since(*sent) >= BORDER_ACK_WAIT);
        if busy != self.main_busy {
            self.route.log().debug(|| {
                let oldest = self.tickets.iter().map(|(sent, _)| *sent).min();
                format!(
                    "main thread busy {} unacked={} oldest_wait_ms={:.1}",
                    if busy { "set" } else { "cleared" },
                    self.tickets.len(),
                    oldest.map_or(0.0, |sent| {
                        now.saturating_duration_since(sent).as_secs_f64() * 1000.0
                    })
                )
            });
        }
        self.main_busy = busy;
    }

    /// Settles refused shows and the route after a batch was sent.
    fn after_flush(&mut self, refused: &[Refusal], shows: &mut Vec<(WindowInstance, Rect)>) {
        for refusal in refused {
            if refusal.reason != RefusalReason::CapReached {
                continue;
            }
            self.counters.refused_shows += 1;
            let window = self
                .ledger
                .iter()
                .find(|(_, t)| t.handle == Some(refusal.handle))
                .map(|(w, _)| *w);
            if let Some(window) = window
                && let Some(tracked) = self.ledger.get_mut(&window)
            {
                tracked.handle = None;
                tracked.shown = false;
                tracked.refused = true;
                shows.retain(|(w, _)| *w != window);
                self.route.upsert(window, tracked.application, None, 0);
            }
        }
        let mut follow_up = self.new_batch();
        for (window, drawn) in shows.drain(..) {
            let Some(handle) = self.ledger.get(&window).and_then(|t| t.handle) else {
                continue;
            };
            match self.route.set_visible(window, drawn) {
                Shown::Clean => {}
                Shown::AppHidden => {
                    if let Some(tracked) = self.ledger.get_mut(&window) {
                        tracked.shown = false;
                    }
                    follow_up = follow_up.with(handle, OverlayKind::Hide);
                }
                Shown::Frame(frame) => {
                    follow_up =
                        follow_up.with(handle, OverlayKind::Move { frame: overlay_rect(frame) });
                }
            }
        }
        if !follow_up.commands.is_empty() && !self.route.is_closed() {
            #[cfg(test)]
            self.sent.push(follow_up.clone());
            self.log_batch("batch sent (follow-up)", &follow_up);
            let receipt = self.route.control().apply(follow_up);
            self.route.note_ticket(receipt.ticket);
        }
    }
}
