//! The main-thread-local store: route tables, native registrations and the
//! staging, commit, refresh and removal transactions over them.
//!
//! One [`Store`] per control lives in a thread-local map on the main thread. It
//! owns the native layer (`Box<dyn Native>`, `!Send`) and an `Rc` route table
//! shared with the Carbon callback, so none of it can be moved off-main by type.
//!
//! # Transaction order
//!
//! `commit` does every fallible native step first and publishes last:
//!
//! 1. recheck cancellation, generation, revision and layout identity;
//! 2. retry owed cleanup, install the handler lazily, register each *new* chord
//!    individually (inert routes: they swallow but never dispatch);
//! 3. recheck cancellation and layout identity again; on failure unregister what
//!    this commit registered and leave the last-good set untouched;
//! 4. one non-native swap: transfer unchanged registrations, publish reservations
//!    for new ones, flip routes live, bump the revision, and inhibit routes that
//!    are no longer wanted;
//! 5. unregister removed chords; failures stay as inert guards.
//!
//! A chord is never registered twice to stage a reload, and the last-good set is
//! never unregistered before step 4.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use stache_sdk::CancellationToken;
use stache_sdk::input::{Chord, Claim, KeyCode, ReservationBackend, ReservationId};

use super::control::Shared;
use super::dispatch::{Route, Routes, dispatcher};
use super::native::{Native, NativeHotKeyId, RegisterFailure};
use super::staging::{Candidate, Resolved, resolve};
use super::types::{
    ActiveBinding, BindingOutcome, BindingToken, CommitReport, HandlerStatus, HotkeyError,
    HotkeyRequest, Scope, StagedBinding, StagedId, StagedReport, StaleReason, Status, StopReport,
};
use crate::keyboard_layout::{LayoutId, LayoutRead};

static NEXT_NATIVE_ID: AtomicU32 = AtomicU32::new(1);

/// The next process-monotonic native ID. IDs are never reused; `None` once the
/// 32-bit space is exhausted (registration then fails instead of wrapping).
fn next_native_id() -> Option<NativeHotKeyId> {
    NEXT_NATIVE_ID
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |next| next.checked_add(1))
        .ok()
        .map(NativeHotKeyId)
}

const DIAGNOSTIC_LIMIT: usize = 64;

thread_local! {
    static STORES: RefCell<HashMap<u64, Store>> = RefCell::new(HashMap::new());
}

/// Runs `f` on the control's store, creating it (and its native layer) lazily.
pub(super) fn with_store<T>(shared: &Arc<Shared>, f: impl FnOnce(&mut Store) -> T) -> T {
    STORES.with(|cell| {
        let mut stores = cell.borrow_mut();
        let store = stores.entry(shared.id).or_insert_with(|| Store::new(Arc::clone(shared)));
        f(store)
    })
}

/// Drops the store, which makes a best-effort native teardown.
pub(super) fn remove_store(shared: &Shared) {
    let store = STORES.with(|cell| cell.borrow_mut().remove(&shared.id));
    drop(store);
}

/// The context of one queued main-thread job.
pub(super) struct Job {
    shared: Arc<Shared>,
    pub generation: u64,
    cancel: CancellationToken,
    abandoned: Arc<AtomicBool>,
}

impl Job {
    pub(super) fn external(
        shared: &Arc<Shared>,
        generation: u64,
        cancel: CancellationToken,
        abandoned: Arc<AtomicBool>,
    ) -> Self {
        Self {
            shared: Arc::clone(shared),
            generation,
            cancel,
            abandoned,
        }
    }

    /// A job owned by the store itself (layout refresh).
    pub(super) fn internal(shared: &Arc<Shared>, generation: u64) -> Self {
        Self::external(
            shared,
            generation,
            CancellationToken::new(),
            Arc::new(AtomicBool::new(false)),
        )
    }

    /// Cancelled by the caller, abandoned by a gave-up wait, or closed.
    fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
            || self.abandoned.load(Ordering::Acquire)
            || self.shared.closed.load(Ordering::Acquire)
    }

    fn is_current(&self) -> bool {
        self.shared.generation.load(Ordering::Acquire) == self.generation
    }
}

struct Active {
    native: NativeHotKeyId,
    candidate: Candidate,
    token: BindingToken,
    reservation: ReservationId,
}

/// A native registration that is no longer wanted but is still registered.
struct Guard {
    native: NativeHotKeyId,
    reservation: Option<ReservationId>,
}

struct Staged {
    serial: u64,
    generation: u64,
    revision: u64,
    layout: Option<LayoutId>,
    requests: Vec<HotkeyRequest>,
    resolved: Vec<(u64, Resolved)>,
}

struct Fresh {
    native: NativeHotKeyId,
    candidate: Candidate,
}

pub(super) struct Store {
    shared: Arc<Shared>,
    native: Box<dyn Native>,
    routes: Rc<RefCell<Routes>>,
    handler: HandlerStatus,
    active: Vec<Active>,
    guards: Vec<Guard>,
    desired: Vec<HotkeyRequest>,
    staged: Option<Staged>,
    revision: u64,
    generation: u64,
    layout: Option<LayoutId>,
    observing: bool,
    observer_pending: bool,
    serial: u64,
    refreshes: u64,
    last_refresh: Option<CommitReport>,
    diagnostics: VecDeque<String>,
    refresh_wanted: bool,
}

impl Store {
    fn new(shared: Arc<Shared>) -> Self {
        let native = (shared.factory)();
        let generation = shared.generation.load(Ordering::Acquire);
        Self {
            shared,
            native,
            routes: Rc::new(RefCell::new(HashMap::new())),
            handler: HandlerStatus::NotInstalled,
            active: Vec::new(),
            guards: Vec::new(),
            desired: Vec::new(),
            staged: None,
            revision: 0,
            generation,
            layout: None,
            observing: false,
            observer_pending: false,
            serial: 0,
            refreshes: 0,
            last_refresh: None,
            diagnostics: VecDeque::new(),
            refresh_wanted: false,
        }
    }

    pub(super) fn take_refresh_request(&mut self) -> Option<u64> {
        std::mem::take(&mut self.refresh_wanted).then_some(self.generation)
    }

    fn note(&mut self, message: String) {
        if self.diagnostics.len() >= DIAGNOSTIC_LIMIT {
            self.diagnostics.pop_front();
        }
        self.diagnostics.push_back(message);
    }

    fn scope(&self, revision: u64) -> Scope {
        Scope {
            control: self.shared.id,
            generation: self.generation,
            revision,
        }
    }

    // ------------------------------------------------------------ staging

    pub(super) fn prepare(
        &mut self,
        job: &Job,
        requests: Vec<HotkeyRequest>,
    ) -> Result<StagedReport, HotkeyError> {
        if job.is_cancelled() {
            return Err(HotkeyError::Cancelled);
        }
        self.adopt_generation(job);
        let staged = self.stage(requests);
        let report = StagedReport {
            id: StagedId {
                scope: self.scope(self.revision),
                serial: staged.serial,
            },
            layout: staged.layout,
            bindings: staged
                .resolved
                .iter()
                .map(|(key, resolved)| (*key, self.preview(resolved)))
                .collect(),
        };
        self.staged = Some(staged);
        Ok(report)
    }

    fn preview(&self, resolved: &Resolved) -> StagedBinding {
        match resolved {
            Resolved::Rejected(outcome) => StagedBinding::Rejected(outcome.clone()),
            Resolved::Ready(candidate) => {
                if self.active.iter().any(|a| a.candidate.physical() == candidate.physical()) {
                    StagedBinding::Reuse(candidate.resolved())
                } else {
                    StagedBinding::New(candidate.resolved())
                }
            }
        }
    }

    fn stage(&mut self, requests: Vec<HotkeyRequest>) -> Staged {
        let layout = self.native.read_layout();
        self.serial += 1;
        Staged {
            serial: self.serial,
            generation: self.generation,
            revision: self.revision,
            layout: layout.identity(),
            resolved: resolve(&requests, &layout),
            requests,
        }
    }

    /// A newer generation (a stop happened) invalidates everything the store
    /// still holds from the old one, even when that stop's own cleanup never ran.
    fn adopt_generation(&mut self, job: &Job) {
        if self.generation == job.generation {
            return;
        }
        self.invalidate_all();
        self.cleanup_guards(job, true);
        self.generation = job.generation;
        self.desired.clear();
        self.staged = None;
        self.layout = None;
        self.revision += 1;
    }

    // ------------------------------------------------------------- commit

    pub(super) fn apply(
        &mut self,
        job: &Job,
        requests: Vec<HotkeyRequest>,
    ) -> Result<CommitReport, HotkeyError> {
        if job.is_cancelled() {
            return Err(HotkeyError::Cancelled);
        }
        self.adopt_generation(job);
        let staged = self.stage(requests);
        self.commit_staged(job, staged)
    }

    pub(super) fn commit(&mut self, job: &Job, id: StagedId) -> Result<CommitReport, HotkeyError> {
        if job.is_cancelled() {
            return Err(HotkeyError::Cancelled);
        }
        self.adopt_generation(job);
        if id.scope.control != self.shared.id {
            return Err(HotkeyError::Stale(StaleReason::Unknown));
        }
        let staged = match self.staged.take() {
            Some(staged) if staged.serial == id.serial => staged,
            other => {
                // A different or missing candidate: keep whatever is staged.
                self.staged = other;
                return Err(HotkeyError::Stale(StaleReason::Unknown));
            }
        };
        self.commit_staged(job, staged)
    }

    fn check_current(&mut self, job: &Job, staged: &Staged) -> Result<(), HotkeyError> {
        if job.is_cancelled() {
            return Err(HotkeyError::Cancelled);
        }
        if !job.is_current() || staged.generation != self.generation {
            return Err(HotkeyError::Stale(StaleReason::Generation));
        }
        if staged.revision != self.revision {
            return Err(HotkeyError::Stale(StaleReason::Revision));
        }
        if self.native.read_layout().identity() != staged.layout {
            self.refresh_wanted = true;
            return Err(HotkeyError::Stale(StaleReason::Layout));
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines, reason = "one linear transaction")]
    fn commit_staged(&mut self, job: &Job, staged: Staged) -> Result<CommitReport, HotkeyError> {
        // Step 1: nothing has been mutated yet.
        self.check_current(job, &staged)?;
        self.cleanup_guards(job, false);

        // Which Ready candidates need a new native registration.
        let to_register: Vec<&Candidate> = staged
            .resolved
            .iter()
            .filter_map(|(_, resolved)| match resolved {
                Resolved::Ready(c)
                    if !self.active.iter().any(|a| a.candidate.physical() == c.physical()) =>
                {
                    Some(c)
                }
                _ => None,
            })
            .collect();

        if !to_register.is_empty()
            && let Err(status) = self.ensure_handler()
        {
            // Distinct from a per-chord conflict: nothing was attempted and the
            // last-good set is untouched.
            let outcomes = staged
                .resolved
                .iter()
                .map(|(key, resolved)| {
                    let outcome = match resolved {
                        Resolved::Rejected(outcome) => outcome.clone(),
                        Resolved::Ready(c) if self.token_for_active(c).is_some() => {
                            self.reused_outcome(c)
                        }
                        Resolved::Ready(_) => BindingOutcome::HandlerUnavailable { status },
                    };
                    (*key, outcome)
                })
                .collect();
            return Ok(self.report(self.revision, outcomes, 0));
        }

        // Step 2: register new chords individually, inert.
        let next_revision = self.revision + 1;
        let mut fresh: Vec<Fresh> = Vec::new();
        let mut failures: HashMap<u64, BindingOutcome> = HashMap::new();
        let candidates: Vec<Candidate> = to_register.into_iter().cloned().collect();
        for candidate in candidates {
            if job.is_cancelled() {
                return Err(self.roll_back(fresh, HotkeyError::Cancelled));
            }
            let Some(native_id) = next_native_id() else {
                failures.insert(candidate.key, BindingOutcome::Failed {
                    status: 0,
                    detail: "native hotkey IDs are exhausted".to_owned(),
                });
                continue;
            };
            match self.native.register(candidate.key_code, candidate.carbon, native_id) {
                Ok(()) => {
                    self.routes.borrow_mut().insert(native_id.0, Route {
                        token: self.token(next_revision, native_id, candidate.key),
                        reservation: None,
                        live: false,
                        generation: self.generation,
                    });
                    fresh.push(Fresh { native: native_id, candidate });
                }
                Err(RegisterFailure::Occupied(status)) => {
                    failures.insert(candidate.key, BindingOutcome::Occupied { status });
                }
                Err(RegisterFailure::Failed(status)) => {
                    failures.insert(candidate.key, BindingOutcome::Failed {
                        status,
                        detail: format!("RegisterEventHotKey returned {status}"),
                    });
                }
            }
        }

        // Step 3: last check before publication.
        if let Err(error) = self.check_current(job, &staged) {
            return Err(self.roll_back(fresh, error));
        }

        // Step 4: the swap. No native call, no failure.
        let Swapped { removed, reused } = self.swap(&staged, fresh, next_revision, &mut failures);
        let removed_count = removed.len();
        let outcomes = staged
            .resolved
            .iter()
            .map(|(key, resolved)| {
                let outcome = match resolved {
                    Resolved::Rejected(outcome) => outcome.clone(),
                    Resolved::Ready(c) => failures.remove(key).unwrap_or_else(|| {
                        self.active.iter().find(|a| a.candidate.key == *key).map_or_else(
                            || BindingOutcome::Failed {
                                status: 0,
                                detail: "the registration is not active".to_owned(),
                            },
                            |entry| BindingOutcome::Active {
                                token: entry.token,
                                chord: c.resolved(),
                                reused: reused.contains(key),
                            },
                        )
                    }),
                };
                (*key, outcome)
            })
            .collect();
        self.revision = next_revision;
        self.desired = staged.requests;
        self.layout = staged.layout;

        // Step 5: cleanup after the swap.
        self.cleanup_removed(job, removed);
        self.ensure_observer(job);
        Ok(self.report(self.revision, outcomes, removed_count))
    }

    fn token(&self, revision: u64, native: NativeHotKeyId, key: u64) -> BindingToken {
        BindingToken {
            scope: self.scope(revision),
            native: native.0,
            key,
        }
    }

    fn token_for_active(&self, candidate: &Candidate) -> Option<BindingToken> {
        self.active
            .iter()
            .find(|a| a.candidate.physical() == candidate.physical())
            .map(|a| a.token)
    }

    fn reused_outcome(&self, candidate: &Candidate) -> BindingOutcome {
        BindingOutcome::Active {
            token: self.token_for_active(candidate).expect("the chord is active"),
            chord: candidate.resolved(),
            reused: true,
        }
    }

    // --------------------------------------------------------------- swap

    /// Transfers unchanged registrations, publishes reservations for new ones,
    /// flips routes and inhibits the rest. Performs no native call.
    fn swap(
        &mut self,
        staged: &Staged,
        fresh: Vec<Fresh>,
        next_revision: u64,
        failures: &mut HashMap<u64, BindingOutcome>,
    ) -> Swapped {
        let registry = self.shared.service.reservations();
        let mut old: HashMap<(u16, u32), Active> = std::mem::take(&mut self.active)
            .into_iter()
            .map(|a| (a.candidate.physical(), a))
            .collect();
        let mut fresh: HashMap<(u16, u32), Fresh> =
            fresh.into_iter().map(|f| (f.candidate.physical(), f)).collect();
        let mut reused = std::collections::HashSet::new();
        let mut next_active = Vec::new();
        let mut removed = Vec::new();

        for (_, resolved) in &staged.resolved {
            let Resolved::Ready(candidate) = resolved else { continue };
            let chord = Chord::new(KeyCode(candidate.key_code), candidate.modifiers);
            if let Some(mut entry) = old.remove(&candidate.physical()) {
                // Unchanged chord: the native registration is transferred.
                if entry.candidate.priority != candidate.priority {
                    entry.reservation = registry
                        .update(entry.reservation, Claim::Chord(chord), candidate.priority)
                        .or_else(|_| {
                            registry.register(
                                self.shared.owner,
                                ReservationBackend::Carbon,
                                Claim::Chord(chord),
                                candidate.priority,
                            )
                        })
                        .unwrap_or(entry.reservation);
                }
                entry.candidate = candidate.clone();
                entry.token = BindingToken {
                    scope: Scope {
                        control: self.shared.id,
                        generation: self.generation,
                        revision: next_revision,
                    },
                    native: entry.native.0,
                    key: candidate.key,
                };
                self.set_route(&entry, true);
                reused.insert(candidate.key);
                next_active.push(entry);
            } else if let Some(new) = fresh.remove(&candidate.physical()) {
                match registry.register(
                    self.shared.owner,
                    ReservationBackend::Carbon,
                    Claim::Chord(chord),
                    candidate.priority,
                ) {
                    Ok(reservation) => {
                        let entry = Active {
                            native: new.native,
                            candidate: candidate.clone(),
                            token: BindingToken {
                                scope: Scope {
                                    control: self.shared.id,
                                    generation: self.generation,
                                    revision: next_revision,
                                },
                                native: new.native.0,
                                key: candidate.key,
                            },
                            reservation,
                        };
                        self.set_route(&entry, true);
                        next_active.push(entry);
                    }
                    Err(error) => {
                        // Cannot publish a reservation: the route stays inert and
                        // the registration is owed cleanup.
                        failures.insert(candidate.key, BindingOutcome::Failed {
                            status: 0,
                            detail: error.to_string(),
                        });
                        self.guards.push(Guard {
                            native: new.native,
                            reservation: None,
                        });
                    }
                }
            }
        }
        // Whatever was not transferred is obsolete: stop its dispatch now, in the
        // same step that makes the replacements live.
        for (_, entry) in old {
            if let Some(route) = self.routes.borrow_mut().get_mut(&entry.native.0) {
                route.live = false;
            }
            removed.push(entry);
        }
        self.active = next_active;
        Swapped { removed, reused }
    }

    fn set_route(&self, entry: &Active, live: bool) {
        self.routes.borrow_mut().insert(entry.native.0, Route {
            token: entry.token,
            reservation: Some(entry.reservation),
            live,
            generation: self.generation,
        });
    }

    // ----------------------------------------------------------- cleanup

    /// Unregisters what one commit registered but did not publish.
    fn roll_back(&mut self, fresh: Vec<Fresh>, cause: HotkeyError) -> HotkeyError {
        let before = self.guards.len();
        for entry in fresh {
            self.unregister_or_guard(entry.native, None);
        }
        let pending = self.guards.len().saturating_sub(before);
        if pending == 0 {
            cause
        } else {
            HotkeyError::CleanupPending {
                pending,
                cause: Box::new(cause),
            }
        }
    }

    /// Unregisters natively; on failure keeps an inert guard and the route.
    /// Returns whether the registration is gone.
    fn unregister_or_guard(
        &mut self,
        native: NativeHotKeyId,
        reservation: Option<ReservationId>,
    ) -> bool {
        match self.native.unregister(native) {
            Ok(()) => {
                self.routes.borrow_mut().remove(&native.0);
                if let Some(reservation) = reservation {
                    // Only this exact owned reservation; a stale one is ignored.
                    let _ = self.shared.service.reservations().remove(reservation);
                }
                true
            }
            Err(status) => {
                if let Some(route) = self.routes.borrow_mut().get_mut(&native.0) {
                    route.live = false;
                }
                self.guards.push(Guard { native, reservation });
                self.note(format!(
                    "unregistering hotkey {} failed with status {status}",
                    native.0
                ));
                false
            }
        }
    }

    /// Cleanup after the swap. A cancelled job skips native mutation and keeps
    /// the entries as guards, so ownership stays exact.
    fn cleanup_removed(&mut self, job: &Job, removed: Vec<Active>) {
        for entry in removed {
            if job.is_cancelled() {
                self.guards.push(Guard {
                    native: entry.native,
                    reservation: Some(entry.reservation),
                });
            } else {
                self.unregister_or_guard(entry.native, Some(entry.reservation));
            }
        }
    }

    /// Retries owed unregistrations. With `force`, cancellation is ignored (used
    /// only when adopting a newer generation, where the old entries are void).
    fn cleanup_guards(&mut self, job: &Job, force: bool) -> usize {
        if !force && job.is_cancelled() {
            return 0;
        }
        let guards = std::mem::take(&mut self.guards);
        self.retry(guards)
    }

    /// Unregisters `guards`, keeping the ones that still fail.
    fn retry(&mut self, guards: Vec<Guard>) -> usize {
        let mut cleaned = 0;
        for guard in guards {
            match self.native.unregister(guard.native) {
                Ok(()) => {
                    self.routes.borrow_mut().remove(&guard.native.0);
                    if let Some(reservation) = guard.reservation {
                        let _ = self.shared.service.reservations().remove(reservation);
                    }
                    cleaned += 1;
                }
                Err(_) => self.guards.push(guard),
            }
        }
        cleaned
    }

    pub(super) fn retry_cleanup(&mut self, job: &Job) -> usize {
        self.adopt_generation(job);
        self.cleanup_guards(job, false);
        self.guards.len()
    }

    /// Makes every active registration inert and moves it to the guards.
    fn invalidate_all(&mut self) {
        for entry in std::mem::take(&mut self.active) {
            if let Some(route) = self.routes.borrow_mut().get_mut(&entry.native.0) {
                route.live = false;
            }
            self.guards.push(Guard {
                native: entry.native,
                reservation: Some(entry.reservation),
            });
        }
    }

    // ----------------------------------------------------- handler, observer

    fn ensure_handler(&mut self) -> Result<(), i32> {
        if matches!(
            self.handler,
            HandlerStatus::Installed | HandlerStatus::RemovalPending(_)
        ) {
            return Ok(());
        }
        let dispatch = dispatcher(Rc::clone(&self.routes), Arc::clone(&self.shared));
        match self.native.install_handler(dispatch) {
            Ok(()) => {
                self.handler = HandlerStatus::Installed;
                Ok(())
            }
            Err(status) => {
                self.handler = HandlerStatus::InstallFailed(status);
                self.note(format!(
                    "installing the Carbon handler failed with status {status}"
                ));
                Err(status)
            }
        }
    }

    fn ensure_observer(&mut self, job: &Job) {
        if self.observing || self.active.is_empty() || job.is_cancelled() {
            return;
        }
        let weak = Arc::downgrade(&self.shared);
        let generation = self.generation;
        let hint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(shared) = weak.upgrade()
                && shared.generation.load(Ordering::Acquire) == generation
            {
                shared.request_refresh(generation);
            }
        });
        match self.native.observe_layout(hint) {
            Ok(()) => {
                self.observing = true;
                self.observer_pending = false;
            }
            Err(error) => {
                self.note(format!("layout changes will not be observed: {error}"));
            }
        }
    }

    // ------------------------------------------------------------ refresh

    /// Re-resolves layout-dependent chords through the staging path. Superseded
    /// work (stopped, restarted, unchanged layout) does nothing native.
    pub(super) fn refresh(&mut self, job: &Job) {
        if job.is_cancelled() || !job.is_current() || self.generation != job.generation {
            return;
        }
        self.refreshes += 1;
        if self.desired.is_empty() {
            return;
        }
        let layout = self.native.read_layout();
        if layout.identity() == self.layout {
            // A coalesced or spurious hint: the active set already matches.
            return;
        }
        let requests = self.desired.clone();
        let staged = self.stage_with(requests, &layout);
        match self.commit_staged(job, staged) {
            Ok(report) => {
                if !report.all_active() {
                    self.note("layout refresh left a degraded set".to_owned());
                }
                self.last_refresh = Some(report);
            }
            Err(error) => self.note(format!("layout refresh was not applied: {error}")),
        }
    }

    fn stage_with(&mut self, requests: Vec<HotkeyRequest>, layout: &LayoutRead) -> Staged {
        self.serial += 1;
        Staged {
            serial: self.serial,
            generation: self.generation,
            revision: self.revision,
            layout: layout.identity(),
            resolved: resolve(&requests, layout),
            requests,
        }
    }

    // --------------------------------------------------------------- stop

    pub(super) fn stop(&mut self, job: &Job) -> StopReport {
        if job.abandoned.load(Ordering::Acquire) {
            // The caller gave up waiting. Dispatch is already inhibited by the
            // generation bump; the next job adopts the new generation and voids
            // whatever is left, so a late stop never disturbs a newer commit.
            return StopReport {
                unregistered: 0,
                pending_cleanup: self.active.len() + self.guards.len(),
                handler: self.handler,
                observer_pending: self.observing || self.observer_pending,
            };
        }
        self.staged = None;
        self.desired.clear();
        self.layout = None;
        self.revision += 1;
        self.generation = job.generation;
        self.refresh_wanted = false;

        let owed = std::mem::take(&mut self.guards);
        let mut unregistered = 0;
        for entry in std::mem::take(&mut self.active) {
            if let Some(route) = self.routes.borrow_mut().get_mut(&entry.native.0) {
                route.live = false;
            }
            if job.is_cancelled() {
                self.guards.push(Guard {
                    native: entry.native,
                    reservation: Some(entry.reservation),
                });
            } else if self.unregister_or_guard(entry.native, Some(entry.reservation)) {
                unregistered += 1;
            }
        }
        if job.is_cancelled() {
            self.guards.extend(owed);
        } else {
            unregistered += self.retry(owed);
        }

        if (self.observing || self.observer_pending) && !job.is_cancelled() {
            match self.native.unobserve_layout() {
                Ok(()) => {
                    self.observing = false;
                    self.observer_pending = false;
                }
                Err(error) => {
                    self.observer_pending = true;
                    self.note(format!("removing the layout observer failed: {error}"));
                }
            }
        }
        // The handler must outlive any registration that still exists natively.
        if self.guards.is_empty()
            && !job.is_cancelled()
            && matches!(
                self.handler,
                HandlerStatus::Installed | HandlerStatus::RemovalPending(_)
            )
        {
            match self.native.remove_handler() {
                Ok(()) => self.handler = HandlerStatus::NotInstalled,
                Err(status) => {
                    self.handler = HandlerStatus::RemovalPending(status);
                    self.note(format!(
                        "removing the Carbon handler failed with status {status}"
                    ));
                }
            }
        }
        StopReport {
            unregistered,
            pending_cleanup: self.guards.len(),
            handler: self.handler,
            observer_pending: self.observing || self.observer_pending,
        }
    }

    // ------------------------------------------------------------- status

    /// A read-only snapshot. Entries of an older generation are void: they are
    /// listed as owed cleanup, not as active.
    pub(super) fn status(&self, job: &Job) -> Status {
        let stale = self.generation != job.generation;
        Status {
            scope: self.scope(self.revision),
            handler: self.handler,
            active: if stale {
                Vec::new()
            } else {
                self.active
                    .iter()
                    .map(|a| ActiveBinding {
                        key: a.candidate.key,
                        token: a.token,
                        chord: a.candidate.resolved(),
                        reservation: a.reservation,
                    })
                    .collect()
            },
            pending_cleanup: self
                .guards
                .iter()
                .map(|g| g.native.0)
                .chain(self.active.iter().filter(|_| stale).map(|a| a.native.0))
                .collect(),
            layout: self.layout,
            observing: self.observing,
            refreshes: self.refreshes,
            last_refresh: self.last_refresh.clone(),
            diagnostics: self.diagnostics.iter().cloned().collect(),
        }
    }

    fn report(
        &self,
        revision: u64,
        outcomes: Vec<(u64, BindingOutcome)>,
        removed: usize,
    ) -> CommitReport {
        CommitReport {
            scope: self.scope(revision),
            layout: self.layout,
            outcomes,
            removed,
            pending_cleanup: self.guards.len(),
            handler: self.handler,
        }
    }
}

struct Swapped {
    removed: Vec<Active>,
    reused: std::collections::HashSet<u64>,
}

impl Drop for Store {
    /// Best-effort teardown when the control is dropped or the thread ends.
    /// Not proof of cleanup: `HotkeyControl::stop` reports the truth.
    fn drop(&mut self) {
        for entry in std::mem::take(&mut self.active) {
            let _ = self.native.unregister(entry.native);
            let _ = self.shared.service.reservations().remove(entry.reservation);
        }
        for guard in std::mem::take(&mut self.guards) {
            let _ = self.native.unregister(guard.native);
            if let Some(reservation) = guard.reservation {
                let _ = self.shared.service.reservations().remove(reservation);
            }
        }
        if self.observing || self.observer_pending {
            let _ = self.native.unobserve_layout();
        }
        if matches!(
            self.handler,
            HandlerStatus::Installed | HandlerStatus::RemovalPending(_)
        ) {
            let _ = self.native.remove_handler();
        }
    }
}
