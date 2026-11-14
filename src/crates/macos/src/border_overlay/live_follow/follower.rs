//! The follower: pure scheduling over injected seams.
//!
//! [`FollowCore`] owns no thread. The owner thread calls it with the slot events
//! it drained and a `now`; it decides which clocks run, reads bounds through an
//! injected closure and reports through the [`FollowSink`]. Nothing here reads a
//! clock, so the quiet period and the pacing are testable with plain instants.
//!
//! # Rules
//!
//! * A `Move` or `Resize` event marks the window *moving* and starts a
//!   [`FrameClock`] for its display (one per display, shared by its moving
//!   windows). A display whose clock cannot be created is paced by the owner's
//!   own timer (one frame period) and counted; live follow still works.
//! * On each tick of a display, every moving window of that display is read
//!   once (`FollowSink::authority` first: while Tiling owns the window nothing
//!   is read) and the samples go to the sink as one batch.
//! * A window is moving until [`FollowTiming::quiet`] passes without an event.
//!   At expiry it is read one last time (so the border lands on the final
//!   bounds), reported as no longer followed, and a display with no moving
//!   window stops its clock. Idle owns no clock.
//! * A clock that cannot stop is retained as a [`StuckClock`] and retried; it is
//!   never dropped.
//! * The first `Move`/`Resize` event of a window with no read in the current
//!   display period reads its bounds at once (on the owner thread) and reports
//!   the sample without waiting for the next display tick. Later events in the
//!   same period only mark the window moving; the display tick (or, when a tick
//!   fell inside the period of an immediate read, the end of that period) reads
//!   it next. A window is never read twice within one display period. The period
//!   is the interval between two observed ticks of its display, the timer period
//!   until one was measured.
//! * `Reorder` is forwarded to [`FollowSink::reorder`] (`act_on_reorder`, on by
//!   default): Tiling re-orders an `above` border above its window when the
//!   window server re-stacked it, coalesced and rate bounded on its side.
//! * Fast reorder: when the owner passes an order function
//!   ([`FollowCore::on_event_ordered`]), a `Reorder` of a target whose border
//!   is `above`, shown under an open Space gate and whose panel window number
//!   is known ([`FollowTarget::fast_panel`]) first re-stacks the panel above
//!   the target with the private call, on the owner thread, at most
//!   [`FAST_REORDER_CAP`] times per window per [`FAST_REORDER_WINDOW`]; the
//!   sink's `reorder` is still called after it (the main-thread `Order` stays
//!   authoritative). A missing symbol or refused call disables the fast path
//!   for the run (reported once); `NotShown` (the main thread's record) is not
//!   an error. Measured natively on 2026-11-14, the private call does not
//!   move the panel (see [`super::skylight`]); the main-thread `Order` is what
//!   re-stacks it.
//! * `Hide`, `Unhide` and `Close` are **not acted on** (native run 2: the
//!   event then taken for the hide fired after nearly every show).
//!   Each has its own flag in [`FollowTiming`], off by default, so a later
//!   verified run can enable it alone.
//! * Raw events ([`FollowCore::on_raw`]) only go to [`FollowSink::raw_events`]. Tiling's own destroy path releases the
//!   border of a closed window and its focus/raise re-order places a border.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::skylight::kind_of_event;
use super::slots::{RawEvent, SlotEvent};
use super::{
    FAST_REORDER_CAP, FAST_REORDER_WINDOW, FastOrderResult, FastReorder, FollowMove, FollowSink,
    FollowTarget, QUIET, RawWindowEvent, WindowEventKind,
};
use crate::DisplayId;
use crate::border_overlay::OverlayRect;
use crate::display_link::{FrameClock, FrameClockFactory, StuckClock, WakeFn};

/// Pacing parameters.
// One independent switch per unverified event kind, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FollowTiming {
    /// Silence after which a window stops being followed.
    pub quiet: Duration,
    /// The timer period for a display whose clock could not be created.
    pub timer_period: Duration,
    /// How long one clock stop may wait.
    pub clock_stop: Duration,
    /// Act on the candidate private hide event (unverified; off).
    pub act_on_hide: bool,
    /// Act on the candidate private unhide event (unverified; off).
    pub act_on_unhide: bool,
    /// Act on the candidate private reorder event (unverified; on: the sink
    /// re-orders only `above` borders).
    pub act_on_reorder: bool,
    /// Act on the candidate private close event (unverified; off).
    pub act_on_close: bool,
}

impl Default for FollowTiming {
    fn default() -> Self {
        Self {
            quiet: QUIET,
            timer_period: Duration::from_micros(16_667),
            clock_stop: Duration::from_millis(200),
            act_on_hide: false,
            act_on_unhide: false,
            act_on_reorder: true,
            act_on_close: false,
        }
    }
}

/// Follower counters, plain data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FollowCounters {
    /// Ticks that read at least one window.
    pub ticks: u64,
    /// Bounds reads.
    pub reads: u64,
    /// Reads that failed.
    pub failed_reads: u64,
    /// Samples skipped because Tiling owned the window.
    pub deferred: u64,
    /// Move or resize events for a window that has no display (fallback).
    pub no_display: u64,
    /// Events for a window the follower has no target for.
    pub unknown: u64,
    /// Clocks created.
    pub clocks_started: u64,
    /// Clocks stopped cleanly.
    pub clocks_stopped: u64,
    /// Clock creations that failed (the display runs on the timer).
    pub clock_failures: u64,
    /// `Unhide` events acted on (never shown by themselves; flag off by default).
    pub unhides: u64,
    /// Raw events handed to the sink for the debug log.
    pub raw: u64,
    /// Hide, unhide, reorder and close events dropped because their flag is off.
    pub ignored: u64,
    /// Reads made at once for an event, ahead of the display tick.
    pub immediate: u64,
    /// Fast reorders the window server accepted.
    pub fast_orders: u64,
    /// Fast reorders skipped because the main thread records the panel as not
    /// shown.
    pub fast_not_shown: u64,
    /// Fast reorders dropped by the per-window cap.
    pub fast_capped: u64,
    /// Fast reorder calls that failed (the first one disables the path).
    pub fast_failures: u64,
    /// The fast path is disabled for the rest of the run.
    pub fast_disabled: bool,
}

/// The private order call the owner hands the follower: `(panel, target)`.
pub type FastOrderFn<'a> = &'a mut dyn FnMut(u32, u32) -> FastOrderResult;

/// See the module documentation.
pub struct FollowCore {
    timing: FollowTiming,
    factory: Arc<dyn FrameClockFactory>,
    sink: Arc<dyn FollowSink>,
    wake: WakeFn,
    targets: BTreeMap<u32, FollowTarget>,
    /// Moving windows (by `cg_window_id`) and the time of their last event.
    moving: BTreeMap<u32, Instant>,
    clocks: BTreeMap<DisplayId, Box<dyn FrameClock>>,
    /// Displays paced by the timer, with their next due time.
    timers: BTreeMap<DisplayId, Instant>,
    stuck: Vec<StuckClock>,
    counters: FollowCounters,
    /// When each moving window was last read.
    last_read: BTreeMap<u32, Instant>,
    /// Moving windows whose read is held until their display period passed.
    waiting: BTreeSet<u32>,
    /// The last tick seen per display and the shortest interval between ticks.
    last_tick: BTreeMap<DisplayId, Instant>,
    periods: BTreeMap<DisplayId, Duration>,
    /// Fast reorders per window within the last [`FAST_REORDER_WINDOW`].
    fast_recent: BTreeMap<u32, VecDeque<Instant>>,
}

impl FollowCore {
    /// A follower over `factory` and `sink`. `wake` is what a clock tick calls.
    #[must_use]
    pub fn new(
        timing: FollowTiming,
        factory: Arc<dyn FrameClockFactory>,
        sink: Arc<dyn FollowSink>,
        wake: WakeFn,
    ) -> Self {
        Self {
            timing,
            factory,
            sink,
            wake,
            targets: BTreeMap::new(),
            moving: BTreeMap::new(),
            clocks: BTreeMap::new(),
            timers: BTreeMap::new(),
            stuck: Vec::new(),
            counters: FollowCounters::default(),
            last_read: BTreeMap::new(),
            waiting: BTreeSet::new(),
            last_tick: BTreeMap::new(),
            periods: BTreeMap::new(),
            fast_recent: BTreeMap::new(),
        }
    }

    /// The counters.
    #[must_use]
    pub const fn counters(&self) -> FollowCounters { self.counters }

    /// Clocks that could not stop and are retained.
    #[must_use]
    pub const fn stuck_clocks(&self) -> usize { self.stuck.len() }

    /// Live clocks (running, not stuck).
    #[must_use]
    pub fn running_clocks(&self) -> usize { self.clocks.len() }

    /// The bordered set changed. Windows that left stop being followed.
    pub fn set_targets(&mut self, targets: &[FollowTarget]) {
        self.targets = targets.iter().map(|t| (t.cg_window_id, *t)).collect();
        let gone: Vec<u32> = self
            .moving
            .keys()
            .filter(|id| !self.targets.contains_key(id))
            .copied()
            .collect();
        for id in gone {
            self.forget(id);
        }
        self.reconcile_clocks();
    }

    fn forget(&mut self, id: u32) {
        self.fast_recent.remove(&id);
        self.moving.remove(&id);
        self.last_read.remove(&id);
        self.waiting.remove(&id);
    }

    /// The display period of `display`: measured from its ticks, else the timer
    /// period.
    fn period(&self, display: DisplayId) -> Duration {
        self.periods.get(&display).copied().unwrap_or(self.timing.timer_period)
    }

    /// Whether `target` was read inside its display's current period.
    fn read_recently(&self, target: &FollowTarget, now: Instant) -> bool {
        let Some(display) = target.display else { return false };
        self.last_read
            .get(&target.cg_window_id)
            .is_some_and(|at| now.saturating_duration_since(*at) < self.period(display))
    }

    /// Reads `due` once each and reports one batch. Windows Tiling owns are not
    /// read.
    fn read_batch(
        &mut self,
        now: Instant,
        due: &[FollowTarget],
        bounds: &mut dyn FnMut(u32) -> Option<OverlayRect>,
    ) -> usize {
        let mut moves = Vec::with_capacity(due.len());
        for target in due {
            let Some(stamp) = self.sink.authority(target.window) else {
                self.counters.deferred += 1;
                continue;
            };
            self.counters.reads += 1;
            self.last_read.insert(target.cg_window_id, now);
            self.waiting.remove(&target.cg_window_id);
            match bounds(target.cg_window_id) {
                Some(bounds) => moves.push(FollowMove {
                    window: target.window,
                    bounds,
                    stamp,
                }),
                None => self.counters.failed_reads += 1,
            }
        }
        if !moves.is_empty() {
            self.counters.ticks += 1;
            self.sink.moves(&moves);
        }
        moves.len()
    }

    /// Hands the raw events drained at `now` to the sink, in order, stamped
    /// with `now` (the owner-thread clock).
    pub fn on_raw(&mut self, now: Instant, raw: &[RawEvent]) {
        if raw.is_empty() {
            return;
        }
        let stamped: Vec<RawWindowEvent> = raw
            .iter()
            .map(|event| RawWindowEvent {
                event: event.event,
                cg_window_id: event.cg_window_id,
                kind: kind_of_event(event.event),
                at: now,
            })
            .collect();
        self.counters.raw += stamped.len() as u64;
        self.sink.raw_events(&stamped);
    }

    /// Handles one window's drained events at `now`. A move or resize with no
    /// read in the current display period reads `bounds` at once. No fast
    /// reorder.
    pub fn on_event(
        &mut self,
        now: Instant,
        event: SlotEvent,
        bounds: &mut dyn FnMut(u32) -> Option<OverlayRect>,
    ) {
        self.on_event_ordered(now, event, bounds, None);
    }

    /// [`Self::on_event`] with the private order call for the fast reorder
    /// (see the module documentation).
    pub fn on_event_ordered(
        &mut self,
        now: Instant,
        event: SlotEvent,
        bounds: &mut dyn FnMut(u32) -> Option<OverlayRect>,
        order: Option<FastOrderFn<'_>>,
    ) {
        let Some(target) = self.targets.get(&event.cg_window_id).copied() else {
            self.counters.unknown += 1;
            return;
        };
        if event.has(WindowEventKind::Close) {
            if self.timing.act_on_close {
                self.forget(event.cg_window_id);
                self.targets.remove(&event.cg_window_id);
                self.sink.following(target.window, false);
                self.sink.close(target.window);
                self.reconcile_clocks();
                return;
            }
            self.counters.ignored += 1;
        }
        if event.has(WindowEventKind::Hide) {
            if self.timing.act_on_hide {
                self.sink.hide(target.window);
            } else {
                self.counters.ignored += 1;
            }
        }
        if event.has(WindowEventKind::Unhide) {
            if self.timing.act_on_unhide {
                self.counters.unhides += 1;
                self.sink.unhide(target.window);
            } else {
                self.counters.ignored += 1;
            }
        }
        if event.has(WindowEventKind::Reorder) {
            if self.timing.act_on_reorder {
                if let Some(order) = order {
                    self.fast_reorder(now, &target, order);
                }
                self.sink.reorder(target.window);
            } else {
                self.counters.ignored += 1;
            }
        }
        if event.has(WindowEventKind::Move) || event.has(WindowEventKind::Resize) {
            let Some(display) = target.display else {
                self.counters.no_display += 1;
                return;
            };
            if self.moving.insert(event.cg_window_id, now).is_none() {
                self.sink.following(target.window, true);
            }
            self.ensure_pacing(display, now);
            if self.read_recently(&target, now) {
                // Coalesced: the next tick (or the end of the period) reads it.
                self.waiting.insert(event.cg_window_id);
            } else if self.read_batch(now, &[target], bounds) > 0 {
                self.counters.immediate += 1;
            }
        }
    }

    /// The fast reorder of one `Reorder` event (at most once per drain pass:
    /// the slots hold one event per window).
    fn fast_reorder(&mut self, now: Instant, target: &FollowTarget, order: FastOrderFn<'_>) {
        if self.counters.fast_disabled {
            return;
        }
        let Some(panel) = target.fast_panel() else { return };
        if panel == target.cg_window_id {
            return;
        }
        let recent = self.fast_recent.entry(target.cg_window_id).or_default();
        while recent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= FAST_REORDER_WINDOW)
        {
            recent.pop_front();
        }
        if recent.len() >= FAST_REORDER_CAP {
            self.counters.fast_capped += 1;
            return;
        }
        let result = order(panel, target.cg_window_id);
        let mut disabled = false;
        match result {
            FastOrderResult::Ordered => {
                recent.push_back(now);
                self.counters.fast_orders += 1;
            }
            FastOrderResult::NotShown => self.counters.fast_not_shown += 1,
            FastOrderResult::Refused(_) | FastOrderResult::Missing(_) => {
                self.counters.fast_failures += 1;
                self.counters.fast_disabled = true;
                disabled = true;
            }
        }
        self.sink.fast_reorder(FastReorder {
            cg_window_id: target.cg_window_id,
            panel,
            result,
            disabled,
        });
    }

    fn ensure_pacing(&mut self, display: DisplayId, now: Instant) {
        if self.clocks.contains_key(&display) || self.timers.contains_key(&display) {
            return;
        }
        if let Ok(clock) = self.factory.create(display, Arc::clone(&self.wake)) {
            self.counters.clocks_started += 1;
            self.clocks.insert(display, clock);
        } else {
            self.counters.clock_failures += 1;
            self.timers.insert(display, now);
        }
    }

    /// One pass: retries stuck clocks, takes the display ticks that arrived,
    /// reads each due moving window once with `bounds`, reports one batch, ends
    /// the windows whose quiet period passed and stops idle clocks.
    pub fn tick(&mut self, now: Instant, bounds: &mut dyn FnMut(u32) -> Option<OverlayRect>) {
        self.retry_stuck();
        let mut ticked: Vec<DisplayId> = Vec::new();
        let mut seen: Vec<(DisplayId, Instant)> = Vec::new();
        for (display, clock) in &self.clocks {
            if let Some(at) = clock.take_tick() {
                ticked.push(*display);
                seen.push((*display, at));
            }
        }
        for (display, at) in seen {
            if let Some(previous) = self.last_tick.insert(display, at) {
                let interval = at.saturating_duration_since(previous);
                if (Duration::from_millis(2)..=Duration::from_millis(50)).contains(&interval) {
                    let shortest =
                        self.periods.get(&display).map_or(interval, |known| (*known).min(interval));
                    self.periods.insert(display, shortest);
                }
            }
        }
        for (display, due) in &mut self.timers {
            if *due <= now {
                *due = now + self.timing.timer_period;
                ticked.push(*display);
            }
        }
        let expired: Vec<u32> = self
            .moving
            .iter()
            .filter(|(_, last)| now.saturating_duration_since(**last) >= self.timing.quiet)
            .map(|(id, _)| *id)
            .collect();
        let waiting: Vec<u32> = self.waiting.iter().copied().collect();
        let candidates: Vec<FollowTarget> = self
            .moving
            .keys()
            .filter_map(|id| self.targets.get(id))
            .filter(|t| {
                expired.contains(&t.cg_window_id)
                    || waiting.contains(&t.cg_window_id)
                    || t.display.is_some_and(|d| ticked.contains(&d))
            })
            .copied()
            .collect();
        // At most one read per window per display period: a candidate read inside
        // its period waits for the end of that period.
        let mut due = Vec::with_capacity(candidates.len());
        for target in candidates {
            if self.read_recently(&target, now) {
                if !expired.contains(&target.cg_window_id) {
                    self.waiting.insert(target.cg_window_id);
                }
            } else {
                due.push(target);
            }
        }
        self.read_batch(now, &due, bounds);
        for id in expired {
            let target = self.targets.get(&id).copied();
            self.forget(id);
            if let Some(target) = target {
                self.sink.following(target.window, false);
            }
        }
        self.reconcile_clocks();
    }

    /// Stops the clocks and timers of displays with no moving window.
    fn reconcile_clocks(&mut self) {
        let busy: Vec<DisplayId> = self
            .moving
            .keys()
            .filter_map(|id| self.targets.get(id).and_then(|t| t.display))
            .collect();
        self.timers.retain(|display, _| busy.contains(display));
        let idle: Vec<DisplayId> =
            self.clocks.keys().filter(|d| !busy.contains(d)).copied().collect();
        for display in idle {
            if let Some(clock) = self.clocks.remove(&display) {
                self.stop_clock(clock);
            }
        }
    }

    fn stop_clock(&mut self, clock: Box<dyn FrameClock>) {
        match clock.stop(self.timing.clock_stop) {
            Ok(()) => self.counters.clocks_stopped += 1,
            Err(stuck) => self.stuck.push(stuck),
        }
    }

    fn retry_stuck(&mut self) {
        for stuck in std::mem::take(&mut self.stuck) {
            match stuck.retry(self.timing.clock_stop) {
                Ok(()) => self.counters.clocks_stopped += 1,
                Err(stuck) => self.stuck.push(stuck),
            }
        }
    }

    /// When the owner must run [`Self::tick`] next without a wake, if ever: the
    /// earliest quiet expiry, the next timer pace, or a stuck-clock retry.
    #[must_use]
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let quiet = self.moving.values().map(|last| *last + self.timing.quiet).min();
        let timer = self.timers.values().copied().min();
        let retry = (!self.stuck.is_empty()).then(|| now + Duration::from_millis(50));
        // A window held back inside its period is read when the period ends.
        let held = self
            .waiting
            .iter()
            .filter_map(|id| {
                let display = self.targets.get(id)?.display?;
                Some(*self.last_read.get(id)? + self.period(display))
            })
            .min();
        [quiet, timer, retry, held].into_iter().flatten().min()
    }

    /// Stops everything: every window stops being followed and every clock stops
    /// within the budget. `true` when no clock is stuck.
    pub fn shutdown(&mut self) -> bool {
        let moving: Vec<u32> = self.moving.keys().copied().collect();
        for id in moving {
            if let Some(target) = self.targets.get(&id) {
                self.sink.following(target.window, false);
            }
        }
        self.moving.clear();
        self.last_read.clear();
        self.waiting.clear();
        self.timers.clear();
        for clock in std::mem::take(&mut self.clocks).into_values() {
            self.stop_clock(clock);
        }
        self.retry_stuck();
        self.stuck.is_empty()
    }

    /// Takes the clocks that could not stop (the owner hands them to the source).
    pub fn take_stuck(&mut self) -> Vec<StuckClock> { std::mem::take(&mut self.stuck) }
}
