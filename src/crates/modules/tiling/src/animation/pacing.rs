//! The frame clocks of the animation worker: one per display with running
//! jobs, created on the first and stopped when the display has none left.
//!
//! A display whose clock cannot be created (or is unknown) is paced by the
//! deadline timer and reported through [`Notes`]; the note clears when a clock
//! for that display later works. Owned by the worker thread alone.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::{DisplayId, FrameClock, FrameClockFactory, StuckClock, WakeFn};

/// How long the worker waits for a callback in flight when it stops one clock.
pub const CLOCK_STOP_BUDGET: Duration = Duration::from_millis(100);

/// The pacing diagnostics: why a display uses the timer.
#[derive(Default)]
pub struct Notes(Mutex<BTreeMap<Option<DisplayId>, String>>);

impl Notes {
    fn set(&self, display: Option<DisplayId>, reason: &str) {
        let label = display.map_or_else(|| "unknown".to_owned(), |d| d.raw().to_string());
        let note = format!("animation pacing: display {label} uses a timer: {reason}");
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(display, note);
    }

    fn clear(&self, display: Option<DisplayId>) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(&display);
    }

    /// The current notes, in display order.
    #[must_use]
    pub fn list(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }
}

/// The live clocks of a worker.
pub struct ClockSet {
    factory: Arc<dyn FrameClockFactory>,
    wake: WakeFn,
    notes: Arc<Notes>,
    clocks: BTreeMap<DisplayId, Box<dyn FrameClock>>,
    /// Displays that failed to get a clock for their current run of jobs.
    timer: BTreeSet<DisplayId>,
    /// Clocks that did not stop; retried at every sync and at the end.
    stuck: Vec<StuckClock>,
}

impl ClockSet {
    /// An empty set: no clock exists until a display needs one.
    #[must_use]
    pub fn new(factory: Arc<dyn FrameClockFactory>, wake: WakeFn, notes: Arc<Notes>) -> Self {
        Self {
            factory,
            wake,
            notes,
            clocks: BTreeMap::new(),
            timer: BTreeSet::new(),
            stuck: Vec::new(),
        }
    }

    /// Brings the clocks to `needed`: creates the missing ones, stops the ones
    /// no job needs, and retries the stuck. Returns the displays with a live
    /// clock. `unplaced` is whether a job has no known display.
    pub fn sync(&mut self, needed: &BTreeSet<DisplayId>, unplaced: bool) -> BTreeSet<DisplayId> {
        self.retry_stuck(Duration::ZERO);
        let idle: Vec<DisplayId> =
            self.clocks.keys().filter(|d| !needed.contains(d)).copied().collect();
        for display in idle {
            if let Some(clock) = self.clocks.remove(&display) {
                self.stop_one(clock);
            }
        }
        self.timer.retain(|d| needed.contains(d));
        for &display in needed {
            if self.clocks.contains_key(&display) || self.timer.contains(&display) {
                continue;
            }
            match self.factory.create(display, Arc::clone(&self.wake)) {
                Ok(clock) => {
                    self.notes.clear(Some(display));
                    self.clocks.insert(display, clock);
                }
                Err(error) => {
                    self.notes.set(Some(display), &error.reason);
                    self.timer.insert(display);
                }
            }
        }
        if unplaced {
            self.notes.set(None, "the target display is unknown");
        } else {
            self.notes.clear(None);
        }
        self.clocks.keys().copied().collect()
    }

    /// The latest tick of every display that has one.
    #[must_use]
    pub fn drain(&self) -> BTreeMap<DisplayId, Instant> {
        self.clocks.iter().filter_map(|(d, c)| c.take_tick().map(|t| (*d, t))).collect()
    }

    /// Stops every clock; the ones that do not stop stay in the stuck list.
    pub fn stop_all(&mut self) {
        for clock in std::mem::take(&mut self.clocks).into_values() {
            self.stop_one(clock);
        }
        self.timer.clear();
        self.retry_stuck(CLOCK_STOP_BUDGET);
    }

    /// Hands over the clocks that never stopped.
    #[must_use]
    pub fn take_stuck(&mut self) -> Vec<StuckClock> { std::mem::take(&mut self.stuck) }

    fn stop_one(&mut self, clock: Box<dyn FrameClock>) {
        if let Err(stuck) = clock.stop(CLOCK_STOP_BUDGET) {
            self.stuck.push(stuck);
        }
    }

    fn retry_stuck(&mut self, budget: Duration) {
        for stuck in std::mem::take(&mut self.stuck) {
            if let Err(still) = stuck.retry(budget) {
                self.stuck.push(still);
            }
        }
    }
}
