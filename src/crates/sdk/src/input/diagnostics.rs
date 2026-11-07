//! Bounded diagnostics and counters.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError, TryLockError};
use std::time::Duration;

use super::control::{FailureCause, TapKind};
use super::event::Epoch;
use super::reservations::ReservationId;
use super::service::Retained;
use super::subscribers::SubscriptionId;

/// Most diagnostics kept; older ones are dropped and counted.
pub const DIAGNOSTIC_CAPACITY: usize = 256;

/// One observable fact the input service reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputDiagnostic {
    /// Two suppressing subscriptions can match the same event.
    Overlap {
        /// The earlier subscription.
        first: SubscriptionId,
        /// The later subscription.
        second: SubscriptionId,
        /// Both have exactly the same priority, so order falls back to registration.
        equal_priority: bool,
    },
    /// An explicit yield was refused because the overlapping claimant does not
    /// outrank the Carbon reservation, so the overlap cannot be arbitrated.
    UnsupportedOverlap {
        /// The reservation already held.
        existing: ReservationId,
    },
    /// A Carbon reservation began yielding to a higher-priority HID claimant and
    /// must not dispatch.
    Yielded {
        /// The Carbon reservation.
        reservation: ReservationId,
        /// The HID claimant it yields to.
        claimant: ReservationId,
    },
    /// A Carbon reservation stopped yielding and may dispatch again.
    YieldRestored {
        /// The Carbon reservation.
        reservation: ReservationId,
    },
    /// A cross-backend overlap was recorded as a conflict.
    ReservationConflict {
        /// One side.
        first: ReservationId,
        /// The other side.
        second: ReservationId,
    },
    /// A callback panicked and the subscriber is inert.
    CallbackPanicked {
        /// The subscriber.
        subscription: SubscriptionId,
    },
    /// A returned callback exceeded the soft target and the subscriber is inert.
    CallbackSlow {
        /// The subscriber.
        subscription: SubscriptionId,
        /// Measured on the injected clock.
        elapsed: Duration,
    },
    /// A tap was disabled; events may be missing and the epoch advanced.
    Discontinuity {
        /// The tap.
        tap: TapKind,
        /// The new epoch.
        epoch: Epoch,
    },
    /// Re-enabling a disabled tap failed.
    ReenableFailed {
        /// The tap.
        tap: TapKind,
        /// Attempts made so far.
        attempts: u32,
        /// Why the last attempt failed.
        cause: FailureCause,
    },
    /// The tap could not be physically closed and resources are retained.
    TapNotClosed {
        /// The tap.
        tap: TapKind,
        /// Why.
        retained: Retained,
    },
}

#[derive(Default)]
pub(crate) struct Diagnostics {
    log: Mutex<VecDeque<InputDiagnostic>>,
    dropped: AtomicU64,
}

impl Diagnostics {
    fn push(&self, log: &mut VecDeque<InputDiagnostic>, item: InputDiagnostic) {
        if log.len() >= DIAGNOSTIC_CAPACITY {
            log.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        log.push_back(item);
    }

    pub(crate) fn record(&self, item: InputDiagnostic) {
        let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        self.push(&mut log, item);
    }

    /// Non-blocking variant for the tap path; drops on contention.
    pub(crate) fn try_record(&self, item: InputDiagnostic) {
        match self.log.try_lock() {
            Ok(mut log) => self.push(&mut log, item),
            Err(TryLockError::Poisoned(poisoned)) => self.push(&mut poisoned.into_inner(), item),
            Err(TryLockError::WouldBlock) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<InputDiagnostic> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    pub(crate) fn dropped(&self) -> u64 { self.dropped.load(Ordering::Relaxed) }
}

/// A snapshot of the service counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputCounters {
    /// New presses passed because the 1,024-entry ledger was full.
    pub ledger_exhausted: u64,
    /// Work reservations refused (the press failed open).
    pub work_rejected: u64,
    /// Callback panics contained.
    pub panics: u64,
    /// Returned callbacks over the soft target.
    pub slow_callbacks: u64,
    /// Tap-path reads that found a lock or cell busy and failed open.
    pub contention: u64,
    /// Repeat/up events with no ledger entry, passed.
    pub orphan_events: u64,
    /// Tap-disabled discontinuities.
    pub discontinuities: u64,
    /// Events whose kind and stream disagreed.
    pub malformed: u64,
    /// Non-repeat downs for a stream that still had an entry.
    pub duplicate_downs: u64,
    /// Native control requests refused by the owner queue.
    pub control_rejected: u64,
    /// Diagnostics dropped because the log was full or busy.
    pub diagnostics_dropped: u64,
}
