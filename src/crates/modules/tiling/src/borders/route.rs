//! The thread-safe half of the border engine: the
//! [`BorderFrames`] seam the frame paths report into, and the routing table the
//! effects thread uses to hide an application's borders before the application
//! itself hides.
//!
//! The engine (`engine.rs`) is owned by the actor thread. Frames arrive on the
//! actor, the animation worker and the effects threads, and `HideApp` runs on
//! the effects thread, so what those threads need lives here behind one short
//! `Mutex`. The lock is only ever held to copy or update small plain values: it
//! is never held across [`OverlayControl::apply`], an acknowledgement wait or
//! any call out.
//!
//! # Frames
//!
//! [`BorderFrames::report`] never waits. A frame for the window's *current*
//! plan sequence (or a newer one) becomes a `Move` at once when the border is
//! visible, otherwise it is remembered so the engine shows the border at it.
//! An older plan sequence is ignored and an unknown window dropped (both
//! counted). The overlay control merges moves latest-wins into one pending
//! batch, so a burst of animation frames costs at most one native job.
//! `Readback` and `Settled` frames also confirm the plan: they are what the
//! show-after-visible rule waits for.
//!
//! # Hiding before the application hides
//!
//! [`SharedBorderRoute::hide_application_bounded`] marks the application hidden
//! (borders of it stay hidden until `note_application_shown`), sends `Hide` for
//! its visible overlays and waits at most [`BORDER_HIDE_WAIT`] for the
//! main-thread acknowledgement of that batch (and so of every batch before it).
//! On timeout the caller proceeds and a late-hide counter increments: the
//! application hide is never delayed further for borders.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::ApplicationId;
use stache_macos::ax::FrameRead;
use stache_macos::border_overlay::{
    AckError, ApplyAck, FastReorder, FollowMove, FollowSink, OverlayBatch, OverlayControl,
    OverlayHandle, OverlayKind, OverlayRect, OverlayTicket, RawWindowEvent,
};
use stache_macos::windows::{IssuedKind, WindowActionOutcome, WindowInstance};

use crate::geometry::Rect;

/// The longest the effects thread waits for a border hide acknowledgement
/// before an application hide (two frames at 60 Hz).
pub const BORDER_HIDE_WAIT: Duration = Duration::from_millis(32);
/// Raised windows remembered between two actor turns.
const MAX_RAISED: usize = 64;

/// Where a reported frame comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderFrameKind {
    /// A direct write was dispatched with this target.
    Target,
    /// One animated frame was written.
    AnimationFrame,
    /// An animation settled on this frame.
    Settled,
    /// The frame read back (or observed) after a write. A refused or partial
    /// write corrects the border to this frame.
    Readback,
}

impl BorderFrameKind {
    /// Whether the frame confirms the plan (the write completed).
    #[must_use]
    pub const fn confirms(self) -> bool { matches!(self, Self::Settled | Self::Readback) }
}

/// Wakes the actor after live follow queued a note for the engine. Must not block.
pub type NoteWaker = Arc<dyn Fn() + Send + Sync>;

/// What the owner thread of live follow noted for the engine since its last look.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveNotes {
    /// Windows the window server hid (their border was hidden at once).
    pub hidden: Vec<WindowInstance>,
    /// Windows the window server unhid. Never a reason to show a border.
    pub unhidden: Vec<WindowInstance>,
    /// Windows the window server destroyed.
    pub closed: Vec<WindowInstance>,
    /// Windows the window server re-stacked (private reorder event): an
    /// `above` border is re-ordered above its window, coalesced per frame.
    pub reordered: Vec<WindowInstance>,
}

/// The bounded, non-blocking seam the frame paths report through.
pub trait BorderFrames: Send + Sync {
    /// Reports the frame of `window` for layout plan `plan_sequence`. Never
    /// waits and never fails the caller.
    fn report(
        &self,
        window: WindowInstance,
        plan_sequence: u64,
        frame: Rect,
        kind: BorderFrameKind,
    );
}

/// What a bounded hide wait ended with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HideWait {
    /// The application had no overlay to hide and nothing was in flight.
    Nothing,
    /// The main thread acknowledged the hide.
    Acked,
    /// The wait elapsed; the caller proceeds (a late hide is counted).
    TimedOut,
    /// The acknowledgement cannot be awaited (main thread, dispatch failure,
    /// superseded); the caller proceeds.
    Skipped(AckError),
}

/// Waits at most `wait` for `ticket`. Pure of any counter; callers count.
#[must_use]
pub fn await_hide(ticket: &OverlayTicket<ApplyAck>, wait: Duration) -> HideWait {
    match ticket.wait(wait) {
        Ok(_) => HideWait::Acked,
        Err(AckError::TimedOut) => HideWait::TimedOut,
        Err(other) => HideWait::Skipped(other),
    }
}

/// An application hide request: the windows whose overlay it hid and the
/// acknowledgement to wait for, if any batch is outstanding.
pub struct HideRequest {
    /// Windows whose overlay this request ordered out.
    pub windows: Vec<WindowInstance>,
    /// Acknowledgement of the batch that carries the hide (or of the latest
    /// batch when nothing needed hiding).
    pub ticket: Option<OverlayTicket<ApplyAck>>,
}

/// What `set_visible` found after a show was sent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Shown {
    /// All good.
    Clean,
    /// The application was marked hidden meanwhile: hide the overlay again.
    AppHidden,
    /// A newer frame arrived meanwhile: move the overlay to it.
    Frame(Rect),
}

struct RouteEntry {
    application: ApplicationId,
    handle: Option<OverlayHandle>,
    /// The window's current plan sequence.
    plan: u64,
    /// The newest plan sequence a confirming frame was reported for.
    confirmed: u64,
    /// The newest plan sequence any frame was reported for by the frame paths.
    reported: u64,
    /// Counts confirming events (a `Settled`/`Readback` report or a
    /// completed reveal write): the engine re-attempts the Space confirmation
    /// of a window it could not confirm once per such event.
    confirmations: u64,
    visible: bool,
    /// The newest frame known (reported or sent by the engine).
    frame: Option<Rect>,
    /// The frame last sent to the overlay.
    drawn: Option<Rect>,
    /// Bumped by every Tiling frame (report or model move). A live sample read
    /// under an older stamp is dropped: Tiling frames supersede live samples.
    stamp: u64,
    /// A Tiling write for the window is in flight: live samples wait.
    busy: bool,
    /// The live follower is inside the window's quiet period.
    following: bool,
    /// When the follower last stopped following the window.
    last_followed: Option<Instant>,
}

#[derive(Default)]
struct Table {
    entries: BTreeMap<WindowInstance, RouteEntry>,
    hidden_apps: BTreeSet<ApplicationId>,
    /// Windows hidden through [`SharedBorderRoute::hide_application`] since the
    /// engine last looked.
    hidden_windows: Vec<WindowInstance>,
    last_ticket: Option<OverlayTicket<ApplyAck>>,
    /// Windows the effects thread raised (`Effect::Focus` completed) since the
    /// engine last looked; their overlays are re-ordered.
    raised: Vec<WindowInstance>,
    /// Windows the window server hid (live follow) since the engine last looked.
    live_hidden: Vec<WindowInstance>,
    /// Windows the window server unhid since the engine last looked.
    live_unhidden: Vec<WindowInstance>,
    /// Windows the window server destroyed since the engine last looked.
    live_closed: Vec<WindowInstance>,
    /// Windows the window server re-stacked since the engine last looked.
    live_reordered: Vec<WindowInstance>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Converts a Tiling rectangle to the overlay's.
#[must_use]
pub const fn overlay_rect(rect: Rect) -> OverlayRect {
    OverlayRect::new(rect.x, rect.y, rect.width, rect.height)
}

/// The frame and kind to report for a finished frame write, from its outcome.
///
/// An applied write reports `confirmed` (the observed frame when it was read
/// back, else `target`); a refused, partial or uncertain write reports the
/// observed frame as a readback, when one was read; a write that was never sent
/// reports nothing.
#[must_use]
pub fn outcome_frame(
    target: Rect,
    outcome: &WindowActionOutcome,
    confirmed: BorderFrameKind,
) -> Option<(Rect, BorderFrameKind)> {
    let WindowActionOutcome::Issued { kind, observed, .. } = outcome else {
        return None;
    };
    let read = observed.as_ref().and_then(FrameRead::complete).map(|frame| {
        Rect::new(
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height,
        )
    });
    match (kind, read) {
        (IssuedKind::Applied, read) => Some((read.unwrap_or(target), confirmed)),
        (_, Some(read)) => Some((read, BorderFrameKind::Readback)),
        (_, None) => None,
    }
}

/// Frame counters of the route, plain data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RouteCounters {
    /// Frames turned into a `Move`.
    pub moves: u64,
    /// Frames ignored because their plan sequence was older than the window's.
    pub stale: u64,
    /// Frames for a window the route does not know.
    pub unknown: u64,
    /// Live samples turned into a `Move`.
    pub live_moves: u64,
    /// Live samples dropped because a Tiling frame superseded them or the border
    /// was not visible.
    pub live_dropped: u64,
    /// Hides that waited the full bound without an acknowledgement.
    pub late_hides: u64,
    /// Hides whose acknowledgement could not be awaited.
    pub skipped_waits: u64,
}

/// The shared routing table; clone-free, share it behind an `Arc`.
pub struct SharedBorderRoute {
    control: std::sync::Arc<OverlayControl>,
    table: Mutex<Table>,
    moves: AtomicU64,
    stale: AtomicU64,
    unknown: AtomicU64,
    late_hides: AtomicU64,
    skipped_waits: AtomicU64,
    live_moves: AtomicU64,
    live_dropped: AtomicU64,
    /// What wakes the actor when live follow queued a note for the engine.
    waker: Mutex<Option<NoteWaker>>,
    /// Set by the stop: nothing is sent to the overlay control afterwards.
    closed: AtomicBool,
    /// Debug evidence for the native run (silent until the runtime sets it).
    log: super::blog::LogSlot,
}

impl SharedBorderRoute {
    /// A route over `control` with no window.
    #[must_use]
    pub fn new(control: std::sync::Arc<OverlayControl>) -> Self {
        Self {
            control,
            table: Mutex::new(Table::default()),
            moves: AtomicU64::new(0),
            stale: AtomicU64::new(0),
            unknown: AtomicU64::new(0),
            late_hides: AtomicU64::new(0),
            skipped_waits: AtomicU64::new(0),
            live_moves: AtomicU64::new(0),
            live_dropped: AtomicU64::new(0),
            waker: Mutex::new(None),
            closed: AtomicBool::new(false),
            log: super::blog::LogSlot::new(),
        }
    }

    /// The debug log of the native borders (the engine and the effects thread
    /// share it).
    #[must_use]
    pub const fn log(&self) -> &super::blog::LogSlot { &self.log }

    /// Stops all traffic to the overlay control for good (the stop calls it
    /// before `release_all`, so a late show cannot leave an overlay behind).
    pub fn close(&self) { self.closed.store(true, Ordering::SeqCst); }

    /// Whether [`Self::close`] ran.
    #[must_use]
    pub fn is_closed(&self) -> bool { self.closed.load(Ordering::SeqCst) }

    /// Records that `window` was raised by Tiling (effects thread, after the
    /// focus effect ran). The actor re-orders its overlay.
    pub fn note_raised(&self, window: WindowInstance) {
        let mut table = lock(&self.table);
        if table.raised.len() < MAX_RAISED {
            table.raised.push(window);
        }
    }

    /// Takes the windows raised since the last call.
    pub(super) fn drain_raised(&self) -> Vec<WindowInstance> {
        std::mem::take(&mut lock(&self.table).raised)
    }

    /// The counters.
    #[must_use]
    pub fn counters(&self) -> RouteCounters {
        RouteCounters {
            moves: self.moves.load(Ordering::Relaxed),
            stale: self.stale.load(Ordering::Relaxed),
            unknown: self.unknown.load(Ordering::Relaxed),
            live_moves: self.live_moves.load(Ordering::Relaxed),
            live_dropped: self.live_dropped.load(Ordering::Relaxed),
            late_hides: self.late_hides.load(Ordering::Relaxed),
            skipped_waits: self.skipped_waits.load(Ordering::Relaxed),
        }
    }

    /// Hides the visible overlays of `application` and marks it hidden: its
    /// borders stay hidden until [`Self::note_application_shown`]. Returns the
    /// windows hidden and the acknowledgement to wait for. Never waits.
    ///
    /// Callable from any thread (the actor for an external hide, the effects
    /// thread before `HideApp`).
    #[must_use]
    pub fn hide_application(&self, application: ApplicationId) -> HideRequest {
        let (targets, fallback) = {
            let mut table = lock(&self.table);
            table.hidden_apps.insert(application);
            let mut targets = Vec::new();
            for (window, entry) in &mut table.entries {
                if entry.application == application
                    && entry.visible
                    && let Some(handle) = entry.handle
                {
                    entry.visible = false;
                    targets.push((*window, handle));
                }
            }
            table.hidden_windows.extend(targets.iter().map(|(window, _)| *window));
            (targets, table.last_ticket.clone())
        };
        if targets.is_empty() {
            return HideRequest {
                windows: Vec::new(),
                ticket: fallback,
            };
        }
        let mut batch = OverlayBatch::new();
        for (_, handle) in &targets {
            batch = batch.with(*handle, OverlayKind::Hide);
        }
        let ticket = self.control.apply(batch).ticket;
        lock(&self.table).last_ticket = Some(ticket.clone());
        HideRequest {
            windows: targets.into_iter().map(|(window, _)| window).collect(),
            ticket: Some(ticket),
        }
    }

    /// [`Self::hide_application`] and wait at most [`BORDER_HIDE_WAIT`] for the
    /// acknowledgement. For the effects thread, right before `HideApp`.
    pub fn hide_application_bounded(&self, application: ApplicationId) -> HideWait {
        let started = Instant::now();
        self.log.debug(|| format!("hide_application_bounded start app={application:?}"));
        let request = self.hide_application(application);
        let windows = request.windows.len();
        let Some(ticket) = request.ticket else {
            self.log.debug(|| {
                format!(
                    "hide_application_bounded outcome=Nothing app={application:?} windows={windows} \
                     elapsed_us={}",
                    started.elapsed().as_micros()
                )
            });
            return HideWait::Nothing;
        };
        let outcome = await_hide(&ticket, BORDER_HIDE_WAIT);
        match outcome {
            HideWait::TimedOut => {
                self.late_hides.fetch_add(1, Ordering::Relaxed);
            }
            HideWait::Skipped(_) => {
                self.skipped_waits.fetch_add(1, Ordering::Relaxed);
            }
            HideWait::Nothing | HideWait::Acked => {}
        }
        let line = || {
            let elapsed = started.elapsed();
            format!(
                "hide_application_bounded outcome={outcome:?} app={application:?} \
                 windows={windows} elapsed_us={} ({:.2}ms, bound {}ms)",
                elapsed.as_micros(),
                elapsed.as_secs_f64() * 1000.0,
                BORDER_HIDE_WAIT.as_millis()
            )
        };
        if outcome == HideWait::TimedOut {
            self.log.info(line);
        } else {
            self.log.debug(line);
        }
        outcome
    }

    /// The application is shown again (the shown observation, or a failed
    /// hide): its borders may follow the show rule.
    pub fn note_application_shown(&self, application: ApplicationId) {
        lock(&self.table).hidden_apps.remove(&application);
    }

    /// Whether the application is marked hidden.
    #[must_use]
    pub fn application_hidden(&self, application: ApplicationId) -> bool {
        lock(&self.table).hidden_apps.contains(&application)
    }

    // ---- engine side (actor thread) ---------------------------------------

    /// The overlay control this route drives.
    #[must_use]
    pub const fn control(&self) -> &std::sync::Arc<OverlayControl> { &self.control }

    /// Registers or refreshes a window. A larger `plan` replaces the current
    /// one; the handle and application follow the engine's ledger.
    pub(super) fn upsert(
        &self,
        window: WindowInstance,
        application: ApplicationId,
        handle: Option<OverlayHandle>,
        plan: u64,
    ) {
        let mut table = lock(&self.table);
        let entry = table.entries.entry(window).or_insert(RouteEntry {
            application,
            handle,
            plan,
            confirmed: 0,
            reported: 0,
            confirmations: 0,
            visible: false,
            frame: None,
            drawn: None,
            stamp: 0,
            busy: false,
            following: false,
            last_followed: None,
        });
        entry.application = application;
        entry.handle = handle;
        entry.plan = entry.plan.max(plan);
        if handle.is_none() {
            entry.visible = false;
        }
        drop(table);
    }

    /// Forgets every window (the engine retired its ledger).
    pub(super) fn clear_windows(&self) { lock(&self.table).entries.clear(); }

    /// Forgets a window.
    pub(super) fn remove(&self, window: WindowInstance) {
        lock(&self.table).entries.remove(&window);
    }

    /// Takes the windows hidden through [`Self::hide_application`] since the
    /// last call.
    pub(super) fn drain_hidden(&self) -> Vec<WindowInstance> {
        std::mem::take(&mut lock(&self.table).hidden_windows)
    }

    /// Marks a window not visible (an engine-side hide).
    pub(super) fn set_hidden(&self, window: WindowInstance) {
        if let Some(entry) = lock(&self.table).entries.get_mut(&window) {
            entry.visible = false;
        }
    }

    /// Records the frame the engine sends (model frame changes).
    pub(super) fn set_frame(&self, window: WindowInstance, frame: Rect) {
        if let Some(entry) = lock(&self.table).entries.get_mut(&window) {
            entry.frame = Some(frame);
            entry.drawn = Some(frame);
            entry.stamp += 1;
        }
    }

    /// The newest frame known for `window` (reported or sent), if any.
    pub(super) fn latest_frame(&self, window: WindowInstance) -> Option<Rect> {
        lock(&self.table).entries.get(&window).and_then(|entry| entry.frame)
    }

    /// The newest plan sequence the frame paths reported a frame for (0: none).
    pub(super) fn reported(&self, window: WindowInstance) -> u64 {
        lock(&self.table).entries.get(&window).map_or(0, |entry| entry.reported)
    }

    /// Whether a confirming frame for `plan` or later was reported.
    pub(super) fn confirmed(&self, window: WindowInstance, plan: u64) -> bool {
        lock(&self.table)
            .entries
            .get(&window)
            .is_some_and(|entry| entry.confirmed >= plan)
    }

    /// Records a completed write (the wiring's `note_reveal_written`).
    pub(super) fn confirm(&self, window: WindowInstance, plan: u64) {
        if let Some(entry) = lock(&self.table).entries.get_mut(&window) {
            entry.confirmed = entry.confirmed.max(plan);
            entry.confirmations += 1;
        }
    }

    /// How many confirming events were seen for `window`.
    pub(super) fn confirmations(&self, window: WindowInstance) -> u64 {
        lock(&self.table).entries.get(&window).map_or(0, |entry| entry.confirmations)
    }

    /// Marks a window visible after a `Show` at `drawn` was sent, and reports
    /// what changed while it was being sent.
    pub(super) fn set_visible(&self, window: WindowInstance, drawn: Rect) -> Shown {
        let mut table = lock(&self.table);
        let hidden = table
            .entries
            .get(&window)
            .is_some_and(|entry| table.hidden_apps.contains(&entry.application));
        let Some(entry) = table.entries.get_mut(&window) else {
            return Shown::Clean;
        };
        if hidden {
            entry.visible = false;
            return Shown::AppHidden;
        }
        entry.visible = true;
        entry.drawn = Some(drawn);
        let shown = match entry.frame {
            Some(frame) if frame != drawn => {
                entry.drawn = Some(frame);
                Shown::Frame(frame)
            }
            _ => {
                entry.frame = Some(drawn);
                Shown::Clean
            }
        };
        drop(table);
        shown
    }

    // ---- live follow (part 06) ----------------------------------------------

    /// Sets what wakes the actor when live follow queues a note.
    pub fn set_waker(&self, waker: Option<NoteWaker>) { *lock(&self.waker) = waker; }

    fn notify(&self) {
        let waker = lock(&self.waker).clone();
        if let Some(waker) = waker {
            waker();
        }
    }

    /// Tells the route whether a Tiling write for `window` is in flight (the
    /// engine, from the model, at each update). Live samples wait while it is.
    pub(super) fn set_busy(&self, window: WindowInstance, busy: bool) {
        if let Some(entry) = lock(&self.table).entries.get_mut(&window) {
            entry.busy = busy;
        }
    }

    /// Whether the live follower is inside the window's quiet period.
    #[must_use]
    pub fn is_following(&self, window: WindowInstance) -> bool {
        lock(&self.table).entries.get(&window).is_some_and(|entry| entry.following)
    }

    /// Whether the follower followed `window` within `grace`, now or before.
    #[must_use]
    pub fn followed_within(&self, window: WindowInstance, grace: Duration) -> bool {
        lock(&self.table).entries.get(&window).is_some_and(|entry| {
            entry.following || entry.last_followed.is_some_and(|at| at.elapsed() <= grace)
        })
    }

    /// Takes the notes live follow made since the last call.
    pub(super) fn drain_live(&self) -> LiveNotes {
        let mut table = lock(&self.table);
        LiveNotes {
            hidden: std::mem::take(&mut table.live_hidden),
            unhidden: std::mem::take(&mut table.live_unhidden),
            closed: std::mem::take(&mut table.live_closed),
            reordered: std::mem::take(&mut table.live_reordered),
        }
    }

    /// Orders `window`'s overlay out at once (a live hide or close): the border
    /// hides earlier than the engine would. Returns whether a `Hide` was sent.
    fn hide_visible(&self, window: WindowInstance) -> bool {
        let handle = {
            let mut table = lock(&self.table);
            let Some(entry) = table.entries.get_mut(&window) else {
                return false;
            };
            let Some(handle) = entry.handle.filter(|_| entry.visible) else {
                return false;
            };
            entry.visible = false;
            table.hidden_windows.push(window);
            handle
        };
        if self.is_closed() {
            return false;
        }
        drop(self.control.apply(OverlayBatch::new().with(handle, OverlayKind::Hide)));
        true
    }

    /// Remembers the acknowledgement of the newest batch the engine sent.
    pub(super) fn note_ticket(&self, ticket: OverlayTicket<ApplyAck>) {
        lock(&self.table).last_ticket = Some(ticket);
    }
}

impl BorderFrames for SharedBorderRoute {
    fn report(
        &self,
        window: WindowInstance,
        plan_sequence: u64,
        frame: Rect,
        kind: BorderFrameKind,
    ) {
        let handle = {
            let mut table = lock(&self.table);
            let Some(entry) = table.entries.get_mut(&window) else {
                self.unknown.fetch_add(1, Ordering::Relaxed);
                return;
            };
            if plan_sequence < entry.plan {
                self.stale.fetch_add(1, Ordering::Relaxed);
                return;
            }
            entry.plan = plan_sequence;
            entry.reported = plan_sequence;
            entry.stamp += 1;
            if kind.confirms() {
                entry.confirmed = entry.confirmed.max(plan_sequence);
                entry.confirmations += 1;
            }
            entry.frame = Some(frame);
            if !entry.visible || entry.drawn == Some(frame) {
                return;
            }
            let Some(handle) = entry.handle else {
                return;
            };
            entry.drawn = Some(frame);
            drop(table);
            handle
        };
        if self.is_closed() {
            return;
        }
        self.moves.fetch_add(1, Ordering::Relaxed);
        // Dropping the receipt is harmless; the control merges it latest-wins.
        drop(self.control.apply(
            OverlayBatch::new().with(handle, OverlayKind::Move { frame: overlay_rect(frame) }),
        ));
    }
}

/// Live follow reports into the route. Every method is non-blocking, takes the
/// table lock only to copy small values and never holds it across the overlay
/// control.
impl FollowSink for SharedBorderRoute {
    fn authority(&self, window: WindowInstance) -> Option<u64> {
        let table = lock(&self.table);
        let stamp = table
            .entries
            .get(&window)
            .filter(|entry| entry.handle.is_some() && entry.visible && !entry.busy)
            .map(|entry| entry.stamp);
        drop(table);
        stamp
    }

    fn moves(&self, moves: &[FollowMove]) {
        let mut batch = OverlayBatch::new();
        let mut sent = 0_u64;
        let mut dropped = 0_u64;
        {
            let mut table = lock(&self.table);
            for sample in moves {
                let Some(entry) = table.entries.get_mut(&sample.window) else {
                    dropped += 1;
                    continue;
                };
                // A Tiling frame (or a write in flight) since the read wins.
                let handle = entry
                    .handle
                    .filter(|_| entry.visible && !entry.busy && entry.stamp == sample.stamp);
                let Some(handle) = handle else {
                    dropped += 1;
                    continue;
                };
                let frame = Rect::new(
                    sample.bounds.x,
                    sample.bounds.y,
                    sample.bounds.width,
                    sample.bounds.height,
                );
                entry.frame = Some(frame);
                entry.drawn = Some(frame);
                batch = batch.with(handle, OverlayKind::Move { frame: sample.bounds });
                sent += 1;
            }
            drop(table);
        }
        self.live_dropped.fetch_add(dropped, Ordering::Relaxed);
        if sent == 0 || self.is_closed() {
            return;
        }
        self.live_moves.fetch_add(sent, Ordering::Relaxed);
        // One coalesced batch per tick. A `Move` never orders a window in, so a
        // `HideAll` that overtook this batch leaves the overlay hidden.
        drop(self.control.apply(batch));
    }

    fn following(&self, window: WindowInstance, following: bool) {
        if let Some(entry) = lock(&self.table).entries.get_mut(&window) {
            if entry.following && !following {
                entry.last_followed = Some(Instant::now());
            }
            entry.following = following;
        }
    }

    fn hide(&self, window: WindowInstance) {
        let _ = self.hide_visible(window);
        lock(&self.table).live_hidden.push(window);
        self.notify();
    }

    fn unhide(&self, window: WindowInstance) {
        // Never shows anything: the engine lifts its live hold and the show rule
        // and the Space gate decide.
        lock(&self.table).live_unhidden.push(window);
        self.notify();
    }

    fn reorder(&self, window: WindowInstance) {
        // Not a Tiling raise: the engine decides (`above` only, coalesced per
        // display frame, rate bounded).
        {
            let mut table = lock(&self.table);
            if table.live_reordered.len() < MAX_RAISED && !table.live_reordered.contains(&window) {
                table.live_reordered.push(window);
            }
        }
        self.notify();
    }

    fn raw_events(&self, events: &[RawWindowEvent]) {
        // Owner thread, after the drain: never inside the private callback.
        for raw in events {
            self.log.debug(|| {
                let kind = raw.kind.map_or_else(|| "-".to_owned(), |kind| format!("{kind:?}"));
                format!(
                    "live raw event id={} kind={kind} cg={}",
                    raw.event, raw.cg_window_id
                )
            });
        }
    }

    fn fast_reorder(&self, report: FastReorder) {
        // Owner thread, right after the private call.
        self.log.debug(|| {
            format!(
                "live fast reorder cg={} panel={} result={:?}",
                report.cg_window_id, report.panel, report.result
            )
        });
        if report.disabled {
            self.log.info(|| {
                format!(
                    "live fast reorder disabled ({:?}); the main-thread Order path is kept",
                    report.result
                )
            });
        }
    }

    fn close(&self, window: WindowInstance) {
        let _ = self.hide_visible(window);
        lock(&self.table).live_closed.push(window);
        self.notify();
    }
}
