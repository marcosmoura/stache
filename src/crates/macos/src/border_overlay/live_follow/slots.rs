//! The bounded latest-per-window slot the private callbacks write into.
//!
//! A fixed array of [`MAX_OVERLAYS`] entries, each an atomic window id and an
//! atomic bit set of [`WindowEventKind`]s. A callback ([`EventSlots::record`])
//! scans the array for the window id, sets one bit and wakes the owner. It
//! allocates nothing, takes no lock while calling out (the waker is cloned under
//! a short lock and called after it), reads no AX state and never blocks. An id
//! that is not in the array is dropped and counted: only windows that are
//! currently bordered, and not owned by Stache, are watched.
//!
//! The owner replaces the watched ids with [`EventSlots::watch`] when the
//! bordered set changes. A callback racing that replacement can set a bit on a
//! slot that was just reassigned: the new window then gets one spurious event,
//! which costs one bounds read and nothing else.
//!
//! [`EventSlots::close`] is the "close first" step of a stop: it refuses new
//! records and waits (bounded) for the callbacks in progress, so after it
//! returns `true` no callback touches the slots.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::{LoopWaker, WindowEventKind};
use crate::border_overlay::MAX_OVERLAYS;

/// One window's pending events, taken by [`EventSlots::drain`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotEvent {
    /// The window server id.
    pub cg_window_id: u32,
    /// The kinds seen since the last drain.
    pub kinds: u32,
}

impl SlotEvent {
    /// Whether `kind` is among the events.
    #[must_use]
    pub const fn has(&self, kind: WindowEventKind) -> bool { self.kinds & kind.bit() != 0 }
}

/// Slot counters, plain data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotsCounters {
    /// Events recorded for a watched window.
    pub recorded: u64,
    /// Events for a window that is not watched (foreign, unknown or closed).
    pub unwatched: u64,
    /// Events refused because the slots are closed.
    pub refused: u64,
    /// Windows left out of the watched set because Stache owns them.
    pub own_dropped: u64,
    /// Windows left out of the watched set because the cap was reached.
    pub capped: u64,
}

struct Entry {
    id: AtomicU32,
    kinds: AtomicU32,
}

/// See the module documentation.
pub struct EventSlots {
    entries: [Entry; MAX_OVERLAYS],
    accepting: AtomicBool,
    in_flight: AtomicUsize,
    waker: Mutex<Option<LoopWaker>>,
    own_pid: i32,
    recorded: AtomicU64,
    unwatched: AtomicU64,
    refused: AtomicU64,
    own_dropped: AtomicU64,
    capped: AtomicU64,
}

impl EventSlots {
    /// Empty, open slots. Windows owned by `own_pid` are never watched.
    #[must_use]
    pub fn new(own_pid: i32) -> Self {
        Self {
            entries: std::array::from_fn(|_| Entry {
                id: AtomicU32::new(0),
                kinds: AtomicU32::new(0),
            }),
            accepting: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
            waker: Mutex::new(None),
            own_pid,
            recorded: AtomicU64::new(0),
            unwatched: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            own_dropped: AtomicU64::new(0),
            capped: AtomicU64::new(0),
        }
    }

    /// Slots for the current process.
    #[must_use]
    pub fn for_current_process() -> Self {
        Self::new(i32::try_from(std::process::id()).unwrap_or(i32::MAX))
    }

    /// Sets (or withdraws) what wakes the owner after a record.
    pub fn set_waker(&self, waker: Option<LoopWaker>) {
        *self.waker.lock().unwrap_or_else(PoisonError::into_inner) = waker;
    }

    /// Wakes the owner (used by the frame clocks and the actor too).
    pub fn wake(&self) {
        let waker = self.waker.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(waker) = waker {
            waker();
        }
    }

    /// The callback side: records one event for `cg_window_id`.
    pub fn record(&self, kind: WindowEventKind, cg_window_id: u32) {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        if self.accepting.load(Ordering::SeqCst) {
            self.record_open(kind, cg_window_id);
        } else {
            self.refused.fetch_add(1, Ordering::Relaxed);
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    fn record_open(&self, kind: WindowEventKind, cg_window_id: u32) {
        let entry = (cg_window_id != 0)
            .then(|| self.entries.iter().find(|e| e.id.load(Ordering::SeqCst) == cg_window_id))
            .flatten();
        let Some(entry) = entry else {
            self.unwatched.fetch_add(1, Ordering::Relaxed);
            return;
        };
        // Hide and unhide cancel each other: the latest of the two wins.
        match kind {
            WindowEventKind::Hide => {
                entry.kinds.fetch_and(!WindowEventKind::Unhide.bit(), Ordering::SeqCst);
            }
            WindowEventKind::Unhide => {
                entry.kinds.fetch_and(!WindowEventKind::Hide.bit(), Ordering::SeqCst);
            }
            _ => {}
        }
        entry.kinds.fetch_or(kind.bit(), Ordering::SeqCst);
        self.recorded.fetch_add(1, Ordering::Relaxed);
        // `wake` holds no lock while it calls out.
        self.wake();
    }

    /// The owner side: replaces the watched windows. `(id, owner pid)`; ids owned
    /// by this process and ids beyond [`MAX_OVERLAYS`] are left out and counted.
    /// Returns the ids now watched, in order.
    pub fn watch(&self, windows: &[(u32, Option<i32>)]) -> Vec<u32> {
        let mut wanted: Vec<u32> = Vec::with_capacity(windows.len().min(MAX_OVERLAYS));
        for (id, pid) in windows {
            if *id == 0 || wanted.contains(id) {
                continue;
            }
            if *pid == Some(self.own_pid) {
                self.own_dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if wanted.len() >= MAX_OVERLAYS {
                self.capped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            wanted.push(*id);
        }
        // Free the slots of windows no longer wanted first.
        for entry in &self.entries {
            let id = entry.id.load(Ordering::SeqCst);
            if id != 0 && !wanted.contains(&id) {
                entry.id.store(0, Ordering::SeqCst);
                entry.kinds.store(0, Ordering::SeqCst);
            }
        }
        for id in &wanted {
            if self.entries.iter().any(|e| e.id.load(Ordering::SeqCst) == *id) {
                continue;
            }
            if let Some(free) = self.entries.iter().find(|e| e.id.load(Ordering::SeqCst) == 0) {
                free.kinds.store(0, Ordering::SeqCst);
                free.id.store(*id, Ordering::SeqCst);
            }
        }
        wanted
    }

    /// Takes the pending events (owner side), one per window.
    #[must_use]
    pub fn drain(&self) -> Vec<SlotEvent> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let id = entry.id.load(Ordering::SeqCst);
                if id == 0 {
                    return None;
                }
                let kinds = entry.kinds.swap(0, Ordering::SeqCst);
                (kinds != 0).then_some(SlotEvent { cg_window_id: id, kinds })
            })
            .collect()
    }

    /// Refuses further records and waits at most `budget` for the callbacks in
    /// progress. `true` when none runs.
    #[must_use]
    pub fn close(&self, budget: Duration) -> bool {
        self.accepting.store(false, Ordering::SeqCst);
        let deadline = Instant::now().checked_add(budget);
        loop {
            if self.in_flight.load(Ordering::SeqCst) == 0 {
                return true;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// The counters.
    #[must_use]
    pub fn counters(&self) -> SlotsCounters {
        SlotsCounters {
            recorded: self.recorded.load(Ordering::Relaxed),
            unwatched: self.unwatched.load(Ordering::Relaxed),
            refused: self.refused.load(Ordering::Relaxed),
            own_dropped: self.own_dropped.load(Ordering::Relaxed),
            capped: self.capped.load(Ordering::Relaxed),
        }
    }

    /// Callbacks currently inside [`Self::record`] (tests).
    #[must_use]
    pub fn in_flight(&self) -> usize { self.in_flight.load(Ordering::SeqCst) }
}
