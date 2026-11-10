//! A scripted location source. A fake never touches `CoreLocation`.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{
    ConsentNotifier, LocationOutcome, LocationRequest, LocationSource, LocationUnavailable,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Answers queued outcomes, then the repeated last one, and records every
/// request so tests can assert the requested accuracy.
pub struct FakeLocationSource {
    queue: Mutex<VecDeque<LocationOutcome>>,
    last: Mutex<LocationOutcome>,
    requests: Mutex<Vec<LocationRequest>>,
    consent: Mutex<FakeConsent>,
}

/// The fake's single consent watcher, mirroring the native one.
#[derive(Default)]
struct FakeConsent {
    /// A read timed out awaiting consent and its watcher is alive.
    pending: bool,
    notifier: Option<ConsentNotifier>,
    /// Watchers created (a manager each) and still alive at most one.
    created: usize,
    cancelled: usize,
    notified: usize,
}

impl FakeLocationSource {
    /// A source answering `outcome` until told otherwise.
    #[must_use]
    pub const fn new(outcome: LocationOutcome) -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            last: Mutex::new(outcome),
            requests: Mutex::new(Vec::new()),
            consent: Mutex::new(FakeConsent {
                pending: false,
                notifier: None,
                created: 0,
                cancelled: 0,
                notified: 0,
            }),
        }
    }

    /// A source that reports Location as unavailable for `reason`.
    #[must_use]
    pub const fn unavailable(reason: LocationUnavailable) -> Self {
        Self::new(LocationOutcome::Unavailable(reason))
    }

    /// Queues one answer.
    pub fn push(&self, outcome: LocationOutcome) { lock(&self.queue).push_back(outcome); }

    /// Replaces the repeated answer.
    pub fn set(&self, outcome: LocationOutcome) { *lock(&self.last) = outcome; }

    /// Whether a consent watcher (the one kept manager) is alive.
    #[must_use]
    pub fn watcher_pending(&self) -> bool { lock(&self.consent).pending }

    /// Consent watchers created so far (one per read that timed out awaiting
    /// consent while none was alive).
    #[must_use]
    pub fn watchers_created(&self) -> usize { lock(&self.consent).created }

    /// Watchers ended by `cancel_pending`.
    #[must_use]
    pub fn watchers_cancelled(&self) -> usize { lock(&self.consent).cancelled }

    /// Notifications delivered.
    #[must_use]
    pub fn notifications(&self) -> usize { lock(&self.consent).notified }

    /// The user decides: a live watcher is released and its notifier called
    /// once. `false` (nothing happens) without a live watcher.
    pub fn decide_consent(&self) -> bool {
        let notifier = {
            let mut consent = lock(&self.consent);
            if !consent.pending {
                return false;
            }
            consent.pending = false;
            consent.notified += 1;
            consent.notifier.take()
        };
        if let Some(notifier) = notifier {
            notifier();
        }
        true
    }

    /// Every request received, oldest first.
    #[must_use]
    pub fn requests(&self) -> Vec<LocationRequest> { lock(&self.requests).clone() }
}

impl FakeLocationSource {
    fn answer(
        &self,
        request: LocationRequest,
        notifier: Option<ConsentNotifier>,
    ) -> LocationOutcome {
        lock(&self.requests).push(request);
        let next = lock(&self.queue).pop_front();
        let outcome = next.unwrap_or_else(|| *lock(&self.last));
        let mut consent = lock(&self.consent);
        // A newer read replaces the watcher: never two managers.
        let replaced = consent.pending;
        consent.pending = false;
        consent.notifier = None;
        if outcome == LocationOutcome::Unavailable(LocationUnavailable::NotDeterminedTimeout) {
            consent.pending = true;
            consent.notifier = notifier;
            if !replaced {
                consent.created += 1;
            }
        }
        drop(consent);
        outcome
    }
}

impl LocationSource for FakeLocationSource {
    fn read(&self, request: LocationRequest) -> LocationOutcome { self.answer(request, None) }

    fn read_watched(
        &self,
        request: LocationRequest,
        on_decided: ConsentNotifier,
    ) -> LocationOutcome {
        self.answer(request, Some(on_decided))
    }

    fn cancel_pending(&self) {
        let mut consent = lock(&self.consent);
        if consent.pending {
            consent.pending = false;
            consent.notifier = None;
            consent.cancelled += 1;
        }
    }
}
