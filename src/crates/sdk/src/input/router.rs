//! The synchronous dispatch core called from the tap callback.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};

use super::context_snapshot::{ContextSnapshot, SnapshotCell};
use super::control::{CALLBACK_SOFT_TARGET, InputClock};
use super::deferral::RemovalSignal;
use super::diagnostics::{Diagnostics, InputCounters, InputDiagnostic};
use super::event::{Disposition, Epoch, InputEvent, InputEventKind, PressId, RawInput};
use super::ledger::{Owned, PressEntry, PressLedger};
use super::subscribers::{
    CallbackScope, HandlerEntry, InertReason, ObserverEntry, SubscriberSet, WorkRequirement,
};
use super::work::{WorkReservation, WorkReservations};

#[derive(Default)]
pub(crate) struct Counters {
    pub(crate) ledger_exhausted: AtomicU64,
    pub(crate) work_rejected: AtomicU64,
    pub(crate) panics: AtomicU64,
    pub(crate) slow_callbacks: AtomicU64,
    pub(crate) contention: AtomicU64,
    pub(crate) orphan_events: AtomicU64,
    pub(crate) discontinuities: AtomicU64,
    pub(crate) malformed: AtomicU64,
    pub(crate) duplicate_downs: AtomicU64,
    pub(crate) control_rejected: AtomicU64,
}

pub(crate) fn bump(counter: &AtomicU64) { counter.fetch_add(1, Ordering::Relaxed); }

impl Counters {
    pub(crate) fn snapshot(&self, diagnostics_dropped: u64) -> InputCounters {
        let get = |c: &AtomicU64| c.load(Ordering::Relaxed);
        InputCounters {
            ledger_exhausted: get(&self.ledger_exhausted),
            work_rejected: get(&self.work_rejected),
            panics: get(&self.panics),
            slow_callbacks: get(&self.slow_callbacks),
            contention: get(&self.contention),
            orphan_events: get(&self.orphan_events),
            discontinuities: get(&self.discontinuities),
            malformed: get(&self.malformed),
            duplicate_downs: get(&self.duplicate_downs),
            control_rejected: get(&self.control_rejected),
            diagnostics_dropped,
        }
    }
}

pub(crate) struct Router {
    pub(crate) subscribers: SnapshotCell<SubscriberSet>,
    pub(crate) context: Arc<SnapshotCell<ContextSnapshot>>,
    unknown: Arc<ContextSnapshot>,
    ledger: Mutex<PressLedger>,
    work: Arc<dyn WorkReservations>,
    clock: Arc<dyn InputClock>,
    pub(crate) counters: Counters,
    pub(crate) diagnostics: Arc<Diagnostics>,
    epoch: AtomicU64,
    pub(crate) removal: Arc<RemovalSignal>,
}

impl Router {
    pub(crate) fn new(
        work: Arc<dyn WorkReservations>,
        clock: Arc<dyn InputClock>,
        diagnostics: Arc<Diagnostics>,
        ledger_capacity: usize,
    ) -> Self {
        let unknown = Arc::new(ContextSnapshot::unknown());
        Self {
            subscribers: SnapshotCell::new(Arc::new(SubscriberSet::default())),
            context: Arc::new(SnapshotCell::new(Arc::clone(&unknown))),
            unknown,
            ledger: Mutex::new(PressLedger::with_capacity(ledger_capacity)),
            work,
            clock,
            counters: Counters::default(),
            diagnostics,
            epoch: AtomicU64::new(0),
            removal: Arc::new(RemovalSignal::default()),
        }
    }

    pub(crate) fn epoch(&self) -> Epoch { Epoch(self.epoch.load(Ordering::SeqCst)) }

    pub(crate) fn ledger_len(&self) -> usize {
        self.ledger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }

    fn lock_ledger(&self) -> Option<MutexGuard<'_, PressLedger>> {
        match self.ledger.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => {
                bump(&self.counters.contention);
                None
            }
        }
    }

    fn event(&self, raw: RawInput) -> InputEvent {
        let context = self.context.load_nonblocking().unwrap_or_else(|| {
            bump(&self.counters.contention);
            Arc::clone(&self.unknown)
        });
        InputEvent::new(raw, self.epoch(), context)
    }

    /// Dispatch for the suppressing tap.
    pub(crate) fn dispatch_suppressing(&self, raw: RawInput) -> Disposition {
        if !raw.is_consistent() {
            bump(&self.counters.malformed);
            return Disposition::Pass;
        }
        let Some(set) = self.subscribers.load_nonblocking() else {
            bump(&self.counters.contention);
            return Disposition::Pass;
        };
        if raw.kind == InputEventKind::FlagsChanged {
            return self.dispatch_flags(&set, raw);
        }
        let Some(mut ledger) = self.lock_ledger() else {
            return Disposition::Pass;
        };
        let disposition = if raw.kind.is_down() && !raw.repeat {
            self.first_down(&set, &mut ledger, raw)
        } else {
            self.continue_press(&mut ledger, raw)
        };
        self.note_ledger(&ledger);
        disposition
    }

    /// Flags deferred tap removal as due once no suppressed tombstone is left.
    /// Only an atomic check, a flag flip and a notify: no native work, no wait.
    fn note_ledger(&self, ledger: &PressLedger) {
        if self.removal.is_armed() && ledger.suppressed() == 0 {
            self.removal.mark_due();
        }
    }

    /// Arms deferred tap removal when the ledger holds a suppressed tombstone.
    /// Atomic with the callback's ledger check, so a tombstone consumed
    /// concurrently can never leave an armed deferral that nothing completes.
    pub(crate) fn arm_removal_if_tombstones(&self, id: u64) -> bool {
        let ledger = self.ledger.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if ledger.suppressed() == 0 {
            return false;
        }
        self.removal.arm(id);
        drop(ledger);
        true
    }

    /// Dispatch for the listen-only tap. It returns nothing: passive delivery can
    /// never decide the fate of an event.
    pub(crate) fn dispatch_passive(&self, raw: RawInput) {
        if !raw.is_consistent() {
            bump(&self.counters.malformed);
            return;
        }
        let Some(set) = self.subscribers.load_nonblocking() else {
            bump(&self.counters.contention);
            return;
        };
        let mut event = None;
        for entry in &set.observers {
            if entry.health.is_inert() || !entry.filter.matches(raw.kind, raw.stream, raw.flags) {
                continue;
            }
            let event = event.get_or_insert_with(|| self.event(raw));
            self.invoke_observer(entry, event);
        }
    }

    fn dispatch_flags(&self, set: &SubscriberSet, raw: RawInput) -> Disposition {
        let mut event = None;
        for entry in &set.handlers {
            if entry.health.is_inert() || !entry.filter.matches(raw.kind, raw.stream, raw.flags) {
                continue;
            }
            let mut scope = CallbackScope::new(false, 0);
            if !self.prepare_work(entry, &mut scope) {
                continue;
            }
            let event = event.get_or_insert_with(|| self.event(raw));
            if self.invoke_handler(entry, event, &mut scope) == Disposition::Suppress {
                return Disposition::Suppress;
            }
        }
        Disposition::Pass
    }

    /// Reserves the work slot a handler declared. `false` means rejected.
    fn prepare_work(&self, entry: &HandlerEntry, scope: &mut CallbackScope) -> bool {
        if entry.work == WorkRequirement::None {
            return true;
        }
        WorkReservation::try_new(&self.work).map_or_else(
            |_| {
                bump(&self.counters.work_rejected);
                false
            },
            |reservation| {
                scope.set_work(reservation);
                true
            },
        )
    }

    fn first_down(
        &self,
        set: &SubscriberSet,
        ledger: &mut PressLedger,
        raw: RawInput,
    ) -> Disposition {
        let matching = |entry: &&Arc<HandlerEntry>| {
            !entry.health.is_inert() && entry.filter.matches(raw.kind, raw.stream, raw.flags)
        };
        if !set.handlers.iter().any(|entry| matching(&entry)) {
            // Nothing to own. A stale entry for this stream is a missed up.
            if ledger.remove(raw.stream).is_some() {
                bump(&self.counters.duplicate_downs);
            }
            return Disposition::Pass;
        }
        if ledger.remove(raw.stream).is_some() {
            bump(&self.counters.duplicate_downs);
        }
        if ledger.is_full() {
            bump(&self.counters.ledger_exhausted);
            return Disposition::Pass;
        }
        let event = self.event(raw);
        let press = event.press();
        let mut owner = Owned::Passed;
        let mut state = 0;
        for entry in set.handlers.iter().filter(matching) {
            let mut scope = CallbackScope::new(true, 0);
            if !self.prepare_work(entry, &mut scope) {
                // Required work is unavailable: the whole press fails open.
                owner = Owned::Passed;
                break;
            }
            if self.invoke_handler(entry, &event, &mut scope) == Disposition::Suppress {
                state = scope.press_state_value();
                owner = Owned::Suppressed(Arc::clone(entry));
                break;
            }
        }
        let disposition = match owner {
            Owned::Passed => Disposition::Pass,
            Owned::Suppressed(_) => Disposition::Suppress,
        };
        ledger.insert(PressEntry { press, owner, state });
        disposition
    }

    /// Repeat and up are routed by the ledger, never re-filtered.
    fn continue_press(&self, ledger: &mut PressLedger, raw: RawInput) -> Disposition {
        let is_up = raw.kind.is_up();
        let Some(entry) = ledger.get_mut(raw.stream) else {
            bump(&self.counters.orphan_events);
            return Disposition::Pass;
        };
        let owner = match &entry.owner {
            Owned::Passed => None,
            Owned::Suppressed(handler) => Some(Arc::clone(handler)),
        };
        let disposition = if let Some(handler) = owner {
            if !handler.health.is_inert() {
                let event = self.event(raw);
                let mut scope = CallbackScope::new(false, entry.state);
                let _ = self.invoke_handler(&handler, &event, &mut scope);
                entry.state = scope.press_state_value();
            }
            Disposition::Suppress
        } else {
            Disposition::Pass
        };
        if is_up {
            ledger.remove(raw.stream);
        }
        disposition
    }

    fn invoke_handler(
        &self,
        entry: &HandlerEntry,
        event: &InputEvent,
        scope: &mut CallbackScope,
    ) -> Disposition {
        let start = self.clock.now();
        let result = catch_unwind(AssertUnwindSafe(|| entry.handler.on_event(event, scope)));
        let elapsed = self.clock.now().saturating_duration_since(start);
        let Ok(disposition) = result else {
            bump(&self.counters.panics);
            if entry.health.mark(InertReason::Panicked) {
                self.diagnostics
                    .try_record(InputDiagnostic::CallbackPanicked { subscription: entry.id });
            }
            return Disposition::Pass;
        };
        if elapsed > CALLBACK_SOFT_TARGET {
            bump(&self.counters.slow_callbacks);
            if entry.health.mark(InertReason::Slow) {
                self.diagnostics.try_record(InputDiagnostic::CallbackSlow {
                    subscription: entry.id,
                    elapsed,
                });
            }
        }
        disposition
    }

    fn invoke_observer(&self, entry: &ObserverEntry, event: &InputEvent) {
        let start = self.clock.now();
        let result = catch_unwind(AssertUnwindSafe(|| entry.observer.on_event(event)));
        let elapsed = self.clock.now().saturating_duration_since(start);
        if result.is_err() {
            bump(&self.counters.panics);
            if entry.health.mark(InertReason::Panicked) {
                self.diagnostics
                    .try_record(InputDiagnostic::CallbackPanicked { subscription: entry.id });
            }
        } else if elapsed > CALLBACK_SOFT_TARGET {
            bump(&self.counters.slow_callbacks);
            if entry.health.mark(InertReason::Slow) {
                self.diagnostics.try_record(InputDiagnostic::CallbackSlow {
                    subscription: entry.id,
                    elapsed,
                });
            }
        }
    }

    /// Tap-disabled handling for the suppressing tap: cancel armed per-press
    /// state, drop the ledger (orphans pass afterwards) and advance the epoch.
    /// No release is synthesized.
    pub(crate) fn cancel_presses_and_advance(&self) -> Epoch {
        let drained = {
            let mut ledger = self.ledger.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let drained = ledger.drain();
            self.epoch.fetch_add(1, Ordering::SeqCst);
            self.note_ledger(&ledger);
            drained
        };
        for entry in drained {
            if let Owned::Suppressed(handler) = entry.owner
                && !handler.health.is_inert()
            {
                let press: PressId = entry.press;
                let _ = catch_unwind(AssertUnwindSafe(|| handler.handler.on_cancel(press)));
            }
        }
        bump(&self.counters.discontinuities);
        self.epoch()
    }

    /// Tap-disabled handling for the listen-only tap.
    pub(crate) fn notify_observers_discontinuity(&self) -> Epoch {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        bump(&self.counters.discontinuities);
        if let Some(set) = self.subscribers.load_nonblocking() {
            for entry in &set.observers {
                if !entry.health.is_inert() {
                    let _ = catch_unwind(AssertUnwindSafe(|| entry.observer.on_discontinuity()));
                }
            }
        }
        self.epoch()
    }
}
