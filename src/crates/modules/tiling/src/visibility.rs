//! Runtime-owned application visibility: the ledger, the hide/unhide/focus
//! executors and the restore at stop.
//!
//! This is Tiling's own bookkeeping, distinct from the macOS adapter in
//! `stache_macos::applications::visibility` that sends the native hide.
//!
//! # What is owned
//!
//! The [`Ledger`] records only **runtime-caused visible-to-hidden transitions of
//! one exact application instance** (an [`ApplicationId`]). An application that
//! was already hidden, or that someone else hides, never becomes owned. A
//! shown application is never re-hidden against the user's wish: an external
//! show relinquishes ownership and suppresses a new hide until the application
//! is needed again.
//!
//! # States
//!
//! ```text
//! (none) --plan hide--> PendingIssued --report Sent------+--hidden seen--> Confirmed
//!                          |   |        report Possible-> PendingPossible --hidden seen--> Confirmed
//!                          |   +--report Confirmed (AXHidden readback)-----> Confirmed
//!                          +--report NotSent--> (none)         (never owned)
//! Pending*  --shown seen while a hide may have landed--> Ambiguous (kept, never unhidden)
//! Confirmed --external show / our unhide observed------> (none)    (released)
//! any       --terminal gone / replaced instance---------> (none)    (cleared)
//! ```
//!
//! `PendingIssued` is a hide whose native request was sent and not yet seen;
//! `PendingPossible` is a hide whose result was lost, so it may have hidden the
//! application. A hide that proved it sent nothing is never recorded as owned.
//! `Ambiguous` means a show was observed while a hide of ours may have landed:
//! the ledger cannot tell our hide from an external one, so it keeps the record
//! and a diagnostic and never unhides it on its own (explicit recovery needs
//! approval).
//!
//! # Writers
//!
//! The actor is the only caller of [`VisibilityControl::plan`] and
//! [`VisibilityControl::observe`]. The effect worker reports an outcome into an
//! inbox ([`VisibilityControl`] folds it in order before the actor next reads
//! the ledger) and wakes the actor with a completion; no report is applied under
//! a registry or module lock. After the seal and the drain, the module's stop
//! path is the only other writer ([`VisibilityControl::restore`]).
//!
//! # Seal
//!
//! [`VisibilityControl::seal`] marks the control sealed and cancels every armed
//! one-shot authorization (public hide or unhide, the `AXHidden` fallback and
//! activation). Cancelling is the atomic check at the native send: a request
//! that has not committed its send can never issue afterwards, and one that did
//! is accounted as possibly applied. Restoration uses its own authorizations,
//! created after the drain, so a late old hide cannot race it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::applications::{ActivationAuthorization, ActivationOutcome};
use stache_macos::windows::{
    ApplicationIssuance, HiddenOperation, HiddenOutcome, HiddenRequest, NotIssuedReason,
    RuntimeEpoch, WindowActionOutcome, WindowInstance,
};
use stache_macos::{
    ApplicationControl, ApplicationId, ApplicationObservation, VisibilityAuthorization,
    VisibilityOutcome,
};
use stache_sdk::CancellationToken;

use crate::actor::{AppDirectory, DRAIN_QUANTUM, DrainOutcome, FrameControl};
use crate::backend::WindowBackend;
use crate::state::AppInfo;
use crate::workspaces::VisibilityRequirement;

/// How long after arming a request the native side may still send it.
pub const VISIBILITY_DEADLINE: Duration = Duration::from_secs(2);
/// Failed normal unhides after which the actor stops retrying one application.
/// Restoration at stop still tries.
pub const MAX_UNHIDE_FAILURES: u8 = 3;
/// Diagnostics and focus reports kept.
pub const LOG_CAPACITY: usize = 32;

// ---------------------------------------------------------------------------
// Native application actions
// ---------------------------------------------------------------------------

/// The exact-instance application actions Tiling uses: hide, unhide and
/// activation. It wraps one [`ApplicationObservation`]; there is no second
/// activation path.
pub struct ApplicationActions {
    observation: ApplicationObservation,
}

impl ApplicationActions {
    /// Wraps an installed observation.
    #[must_use]
    pub const fn new(observation: ApplicationObservation) -> Self { Self { observation } }

    /// An armed one-shot hide/unhide authorization for `application`.
    #[must_use]
    pub fn visibility_authorization(
        &self,
        application: ApplicationId,
        deadline: Instant,
    ) -> VisibilityAuthorization {
        VisibilityAuthorization::with_deadline(application, self.observation.accepted(), deadline)
    }

    /// An armed one-shot activation authorization for `application`.
    #[must_use]
    pub fn activation_authorization(
        &self,
        application: ApplicationId,
        deadline: Instant,
    ) -> ActivationAuthorization {
        ActivationAuthorization::with_deadline(application, self.observation.accepted(), deadline)
    }

    /// Hides exactly the instance `authorization` names.
    #[must_use]
    pub fn hide(&self, authorization: &VisibilityAuthorization) -> VisibilityOutcome {
        self.observation.hide_exact(
            authorization.accepted(),
            authorization.application(),
            authorization,
        )
    }

    /// Unhides exactly the instance `authorization` names.
    #[must_use]
    pub fn unhide(&self, authorization: &VisibilityAuthorization) -> VisibilityOutcome {
        self.observation.unhide_exact(
            authorization.accepted(),
            authorization.application(),
            authorization,
        )
    }

    /// Activates exactly the instance `authorization` names. The only
    /// activation path.
    #[must_use]
    pub fn activate(&self, authorization: &ActivationAuthorization) -> ActivationOutcome {
        self.observation.activate_exact(
            authorization.accepted(),
            authorization.application(),
            authorization,
        )
    }
}

impl ApplicationActions {
    /// Every live running application with metadata, as exact instances of the
    /// observation this run owns. Empty when the query fails.
    #[must_use]
    pub fn running_details(&self) -> Vec<stache_macos::FrontmostApplication> {
        self.observation.running_details().unwrap_or_default()
    }
}

/// The production [`AppDirectory`] over the run's own application observation.
///
/// Every [`ApplicationId`] it returns is an instance of the same generation the
/// hide, unhide and activation authorizations accept.
pub struct ActionsDirectory {
    actions: std::sync::Arc<ApplicationActions>,
}

impl ActionsDirectory {
    /// A directory over the run's actions.
    #[must_use]
    pub const fn new(actions: std::sync::Arc<ApplicationActions>) -> Self { Self { actions } }

    fn info(application: stache_macos::FrontmostApplication) -> Option<AppInfo> {
        Some(AppInfo {
            application: application.id,
            pid: application.pid?,
            bundle_id: application.bundle_identifier.unwrap_or_default(),
            name: application.name.unwrap_or_default(),
        })
    }
}

impl AppDirectory for ActionsDirectory {
    fn running(&self) -> Vec<AppInfo> {
        self.actions.running_details().into_iter().filter_map(Self::info).collect()
    }

    fn describe(&self, application: ApplicationId) -> Option<AppInfo> {
        self.actions
            .running_details()
            .into_iter()
            .find(|candidate| candidate.id == application)
            .and_then(Self::info)
    }
}

/// Installs one [`ApplicationActions`] per module run.
pub trait ApplicationActionsSource: Send + Sync {
    /// Installs an application observation for one run.
    ///
    /// # Errors
    ///
    /// Why the observation could not be installed.
    fn open(&self, cancel: &CancellationToken) -> Result<ApplicationActions, String>;
}

/// The production source: one [`ApplicationControl`] per host.
pub struct ControlActionsSource {
    control: ApplicationControl,
}

impl ControlActionsSource {
    /// Wraps the host's application control.
    #[must_use]
    pub const fn new(control: ApplicationControl) -> Self { Self { control } }
}

impl ApplicationActionsSource for ControlActionsSource {
    fn open(&self, cancel: &CancellationToken) -> Result<ApplicationActions, String> {
        self.control
            .install(cancel, |_| {})
            .map(ApplicationActions::new)
            .map_err(|error| error.to_string())
    }
}

// ---------------------------------------------------------------------------
// The pure ledger
// ---------------------------------------------------------------------------

/// How the runtime holds a hidden application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ownership {
    /// A hide was issued; hidden is not yet observed.
    PendingIssued,
    /// A hide may have been issued but its result was lost.
    PendingPossible,
    /// Hidden was observed (or read back) after our issuance.
    Confirmed,
    /// A show was observed while a hide of ours may have landed.
    Ambiguous,
}

/// One owned or possibly owned application, as published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedApp {
    /// The exact application instance.
    pub application: ApplicationId,
    /// The ownership state.
    pub ownership: Ownership,
    /// Why it is uncertain or why its last unhide failed.
    pub reason: Option<String>,
    /// Failed unhide attempts.
    pub unhide_failures: u8,
}

#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent in-flight and observation marks, not a state machine"
)]
struct Entry {
    ownership: Ownership,
    hide_inflight: bool,
    unhide_inflight: bool,
    unhide_sent: bool,
    hidden_seen: bool,
    shown_seen: bool,
    unhide_failures: u8,
    reason: Option<String>,
}

impl Entry {
    const fn new() -> Self {
        Self {
            ownership: Ownership::PendingIssued,
            hide_inflight: true,
            unhide_inflight: false,
            unhide_sent: false,
            hidden_seen: false,
            shown_seen: false,
            unhide_failures: 0,
            reason: None,
        }
    }

    const fn ours_unhiding(&self) -> bool { self.unhide_inflight || self.unhide_sent }
}

/// What a hide attempt proved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HideEvidence {
    /// Provably nothing was hidden by us.
    NotSent(String),
    /// A read-back after our own write shows the application hidden.
    Confirmed,
    /// The request was sent; hidden is not yet observed.
    Sent,
    /// The request may have been sent and its result is unknown.
    Possible(String),
}

/// What an unhide attempt proved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnhideEvidence {
    /// A read-back shows the application visible.
    Shown,
    /// The request was sent; shown is not yet observed.
    Sent,
    /// The exact instance is gone or was replaced.
    Gone,
    /// The request failed or its result is unknown.
    Failed(String),
}

/// A worker result waiting to be folded into the ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// A hide finished.
    Hide(ApplicationId, HideEvidence),
    /// An unhide finished.
    Unhide(ApplicationId, UnhideEvidence),
}

/// What the actor observed about an application's visibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observation {
    /// The application was hidden (by anyone).
    Hidden,
    /// The application was shown (by anyone).
    Shown,
    /// The exact instance terminated.
    Gone,
}

/// A request the actor decided to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Planned {
    /// Hide this exact application.
    Hide(ApplicationId),
    /// Unhide this exact application.
    Unhide(ApplicationId),
}

/// The ledger: pure, lock-free state. [`VisibilityControl`] guards it.
#[derive(Debug, Default)]
pub struct Ledger {
    entries: BTreeMap<ApplicationId, Entry>,
    suppressed: BTreeSet<ApplicationId>,
    released: u64,
    log: VecDeque<String>,
}

impl Ledger {
    fn note(&mut self, message: String) {
        if self.log.len() == LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(message);
    }

    /// Plans hides and unhides from a requirement. With `enabled` false (no
    /// authority, or sealed) it only clears applications that are gone.
    pub fn plan(
        &mut self,
        requirement: &VisibilityRequirement,
        live: &dyn Fn(ApplicationId) -> bool,
        enabled: bool,
    ) -> Vec<Planned> {
        let gone: Vec<ApplicationId> =
            self.entries.keys().copied().filter(|application| !live(*application)).collect();
        for application in gone {
            self.observe(application, Observation::Gone);
        }
        self.suppressed.retain(|application| live(*application));
        if !enabled {
            return Vec::new();
        }
        let mut planned = Vec::new();
        for app in &requirement.apps {
            let needed = !app.needed_by.is_empty();
            if needed {
                self.suppressed.remove(&app.application);
            }
            match self.entries.get_mut(&app.application) {
                None => {
                    if app.hide_wanted
                        && !app.externally_hidden
                        && !self.suppressed.contains(&app.application)
                    {
                        self.entries.insert(app.application, Entry::new());
                        planned.push(Planned::Hide(app.application));
                    }
                }
                Some(entry) => {
                    if entry.hide_inflight || entry.unhide_inflight || entry.unhide_sent {
                        continue;
                    }
                    let owned = matches!(
                        entry.ownership,
                        Ownership::Confirmed
                            | Ownership::PendingIssued
                            | Ownership::PendingPossible
                    );
                    if needed && owned && entry.unhide_failures < MAX_UNHIDE_FAILURES {
                        entry.unhide_inflight = true;
                        planned.push(Planned::Unhide(app.application));
                    }
                }
            }
        }
        planned
    }

    /// An effect that never ran: forget its in-flight mark.
    pub fn cancel_planned(&mut self, planned: Planned) {
        match planned {
            Planned::Hide(application) => {
                if self.entries.get(&application).is_some_and(|entry| entry.hide_inflight) {
                    self.entries.remove(&application);
                }
            }
            Planned::Unhide(application) => {
                if let Some(entry) = self.entries.get_mut(&application) {
                    entry.unhide_inflight = false;
                }
            }
        }
    }

    /// Marks an unhide in flight when `application` is an owned hidden one that
    /// no operation is already working on. `true` when the caller may unhide.
    pub fn begin_unhide(&mut self, application: ApplicationId) -> bool {
        let Some(entry) = self.entries.get_mut(&application) else {
            return false;
        };
        let owned = matches!(
            entry.ownership,
            Ownership::Confirmed | Ownership::PendingIssued | Ownership::PendingPossible
        );
        if !owned || entry.hide_inflight || entry.unhide_inflight {
            return false;
        }
        entry.unhide_inflight = true;
        true
    }

    /// Folds one worker result.
    pub fn apply(&mut self, report: Report) {
        match report {
            Report::Hide(application, evidence) => self.apply_hide(application, evidence),
            Report::Unhide(application, evidence) => self.apply_unhide(application, evidence),
        }
    }

    fn apply_hide(&mut self, application: ApplicationId, evidence: HideEvidence) {
        let Some(entry) = self.entries.get_mut(&application) else {
            return;
        };
        entry.hide_inflight = false;
        let (hidden_seen, shown_seen) = (entry.hidden_seen, entry.shown_seen);
        entry.hidden_seen = false;
        entry.shown_seen = false;
        match evidence {
            HideEvidence::NotSent(reason) => {
                self.entries.remove(&application);
                self.note(format!("{application:?}: hide not sent ({reason}); never owned"));
            }
            HideEvidence::Confirmed => entry.ownership = Ownership::Confirmed,
            HideEvidence::Sent | HideEvidence::Possible(_) => {
                let reason = match &evidence {
                    HideEvidence::Possible(reason) => Some(reason.clone()),
                    _ => None,
                };
                if shown_seen {
                    entry.ownership = Ownership::Ambiguous;
                    entry.reason = Some("shown while a hide of ours may have landed".to_owned());
                    let message = format!(
                        "{application:?}: ambiguous, cannot tell our hide from an external show"
                    );
                    self.note(message);
                } else if hidden_seen {
                    entry.ownership = Ownership::Confirmed;
                } else if let Some(reason) = reason {
                    entry.ownership = Ownership::PendingPossible;
                    entry.reason = Some(reason);
                } else {
                    entry.ownership = Ownership::PendingIssued;
                }
            }
        }
    }

    fn apply_unhide(&mut self, application: ApplicationId, evidence: UnhideEvidence) {
        let Some(entry) = self.entries.get_mut(&application) else {
            return;
        };
        entry.unhide_inflight = false;
        match evidence {
            UnhideEvidence::Shown | UnhideEvidence::Gone => {
                self.entries.remove(&application);
                self.released += 1;
            }
            UnhideEvidence::Sent => entry.unhide_sent = true,
            UnhideEvidence::Failed(reason) => {
                entry.unhide_failures = entry.unhide_failures.saturating_add(1);
                entry.reason = Some(reason);
            }
        }
    }

    /// Folds one observation of an application's visibility.
    pub fn observe(&mut self, application: ApplicationId, observation: Observation) {
        match observation {
            Observation::Gone => {
                if self.entries.remove(&application).is_some() {
                    self.released += 1;
                }
                self.suppressed.remove(&application);
            }
            Observation::Hidden => {
                if let Some(entry) = self.entries.get_mut(&application) {
                    if entry.hide_inflight {
                        entry.hidden_seen = true;
                    } else if matches!(
                        entry.ownership,
                        Ownership::PendingIssued | Ownership::PendingPossible
                    ) {
                        entry.ownership = Ownership::Confirmed;
                        entry.reason = None;
                    }
                }
            }
            Observation::Shown => self.observe_shown(application),
        }
    }

    fn observe_shown(&mut self, application: ApplicationId) {
        let Some(entry) = self.entries.get_mut(&application) else {
            return;
        };
        if entry.hide_inflight {
            entry.shown_seen = true;
            return;
        }
        let ours = entry.ours_unhiding();
        match entry.ownership {
            Ownership::Confirmed => {
                self.entries.remove(&application);
                self.released += 1;
                if !ours {
                    // The user showed it: do not hide it again until needed.
                    self.suppressed.insert(application);
                }
            }
            Ownership::PendingIssued | Ownership::PendingPossible => {
                if ours {
                    self.entries.remove(&application);
                    self.released += 1;
                } else {
                    entry.ownership = Ownership::Ambiguous;
                    entry.reason = Some("shown while a hide of ours may have landed".to_owned());
                    let message = format!(
                        "{application:?}: ambiguous, cannot tell our hide from an external show"
                    );
                    self.note(message);
                }
            }
            Ownership::Ambiguous => {}
        }
    }

    /// The applications a restore must try to unhide.
    #[must_use]
    pub fn restore_candidates(&self) -> Vec<ApplicationId> {
        self.entries
            .iter()
            .filter(|(_, entry)| {
                matches!(
                    entry.ownership,
                    Ownership::Confirmed | Ownership::PendingIssued | Ownership::PendingPossible
                ) && !entry.unhide_inflight
            })
            .map(|(application, _)| *application)
            .collect()
    }

    /// Folds the result of a restoration unhide.
    pub fn restored(&mut self, application: ApplicationId, evidence: UnhideEvidence) {
        match evidence {
            UnhideEvidence::Shown | UnhideEvidence::Sent | UnhideEvidence::Gone => {
                if self.entries.remove(&application).is_some() {
                    self.released += 1;
                }
            }
            UnhideEvidence::Failed(reason) => {
                if let Some(entry) = self.entries.get_mut(&application) {
                    entry.unhide_failures = entry.unhide_failures.saturating_add(1);
                    entry.reason = Some(reason);
                }
            }
        }
    }

    /// Every record, ordered by application.
    #[must_use]
    pub fn owned(&self) -> Vec<OwnedApp> {
        self.entries
            .iter()
            .map(|(application, entry)| OwnedApp {
                application: *application,
                ownership: entry.ownership,
                reason: entry.reason.clone(),
                unhide_failures: entry.unhide_failures,
            })
            .collect()
    }

    /// The ownership of one application, if recorded.
    #[must_use]
    pub fn ownership(&self, application: ApplicationId) -> Option<Ownership> {
        self.entries.get(&application).map(|entry| entry.ownership)
    }

    /// Test seed: records `application` as already settled in `ownership`.
    #[cfg(test)]
    pub fn seed(&mut self, application: ApplicationId, ownership: Ownership) {
        let mut entry = Entry::new();
        entry.hide_inflight = false;
        entry.ownership = ownership;
        if ownership == Ownership::Ambiguous {
            entry.reason = Some("seeded".to_owned());
        }
        self.entries.insert(application, entry);
    }

    /// Whether `application` is suppressed after an external show.
    #[must_use]
    pub fn is_suppressed(&self, application: ApplicationId) -> bool {
        self.suppressed.contains(&application)
    }

    /// Records released so far.
    #[must_use]
    pub const fn released(&self) -> u64 { self.released }
}

// ---------------------------------------------------------------------------
// The shared control
// ---------------------------------------------------------------------------

enum Armed {
    Visibility(VisibilityAuthorization),
    Activation(ActivationAuthorization),
    Hidden(ApplicationIssuance),
}

impl Armed {
    fn cancel(&self) {
        match self {
            Self::Visibility(authorization) => {
                let _ = authorization.cancel();
            }
            Self::Activation(authorization) => {
                let _ = authorization.cancel();
            }
            Self::Hidden(issuance) => {
                let _ = issuance.cancel();
            }
        }
    }
}

#[derive(Default)]
struct Book {
    ledger: Ledger,
    inbox: VecDeque<Report>,
    sealed: bool,
    outstanding: usize,
    armed: BTreeMap<u64, Armed>,
    next_armed: u64,
    focus: VecDeque<FocusReport>,
}

impl Book {
    fn fold(&mut self) {
        while let Some(report) = self.inbox.pop_front() {
            self.ledger.apply(report);
        }
    }
}

struct Shared {
    book: Mutex<Book>,
    idle: Condvar,
}

/// The native capabilities one execution needs.
pub struct Exec<'a> {
    /// Hide, unhide and activation. `None` when the observation could not be
    /// installed: nothing is hidden and focus cannot activate.
    pub actions: Option<&'a ApplicationActions>,
    /// The window backend (`AXHidden` fallback and raise).
    pub backend: &'a dyn WindowBackend,
    /// The backend epoch.
    pub epoch: RuntimeEpoch,
    /// Cancels waits.
    pub cancel: &'a CancellationToken,
}

/// One step of a focus request, in the order it ran.
#[derive(Clone, Debug, PartialEq)]
pub enum FocusStep {
    /// The owned-hidden application was unhidden first.
    Unhide(UnhideEvidence),
    /// The only activation path.
    Activate(ActivationOutcome),
    /// `AXRaise` through the backend issuance gate.
    Raise(WindowActionOutcome),
    /// A step did not run.
    Skipped(&'static str),
}

/// The typed outcome of one focus request. Enqueueing is never success.
#[derive(Clone, Debug, PartialEq)]
pub struct FocusReport {
    /// The window asked for.
    pub window: WindowInstance,
    /// What ran, in order.
    pub steps: Vec<FocusStep>,
}

/// What a restore found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreOutcome {
    /// Applications restored or cleared this attempt.
    pub cleared: usize,
    /// Records still owed (failed or uncertain unhide).
    pub remaining: Vec<OwnedApp>,
    /// Ambiguous records: reported and retained, never unhidden.
    pub ambiguous: Vec<OwnedApp>,
    /// Why the restore could not run to the end.
    pub blocked: Option<String>,
}

impl RestoreOutcome {
    /// Nothing is owed any more.
    #[must_use]
    pub const fn is_complete(&self) -> bool { self.blocked.is_none() && self.remaining.is_empty() }
}

/// The ledger plus the seal, the in-flight accounting and the executors.
pub struct VisibilityControl {
    shared: Arc<Shared>,
}

impl Default for VisibilityControl {
    fn default() -> Self { Self::new() }
}

impl VisibilityControl {
    /// An empty, unsealed control.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                book: Mutex::new(Book::default()),
                idle: Condvar::new(),
            }),
        }
    }

    fn book(&self) -> MutexGuard<'_, Book> {
        self.shared.book.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Seals: no new normal hide or unhide, and every armed one-shot
    /// authorization is cancelled at once (one atomic per request, never
    /// waiting on a native send). Idempotent.
    pub fn seal(&self) {
        let mut book = self.book();
        book.sealed = true;
        for armed in book.armed.values() {
            armed.cancel();
        }
        drop(book);
        self.shared.idle.notify_all();
    }

    /// Whether the control is sealed.
    #[must_use]
    pub fn is_sealed(&self) -> bool { self.book().sealed }

    // ---- actor side --------------------------------------------------

    /// Plans the hides and unhides the requirement asks for and marks each in
    /// flight. `enabled` is the actor's frame/action authority; a sealed
    /// control plans nothing.
    pub fn plan(
        &self,
        requirement: &VisibilityRequirement,
        live: &dyn Fn(ApplicationId) -> bool,
        enabled: bool,
    ) -> Vec<Planned> {
        let mut book = self.book();
        book.fold();
        let enabled = enabled && !book.sealed;
        let planned = book.ledger.plan(requirement, live, enabled);
        book.outstanding += planned.len();
        planned
    }

    /// A planned effect never reached the worker.
    pub fn cancel_planned(&self, planned: Planned) {
        let mut book = self.book();
        book.ledger.cancel_planned(planned);
        book.outstanding = book.outstanding.saturating_sub(1);
        drop(book);
        self.shared.idle.notify_all();
    }

    /// Folds one observation of an application's visibility.
    pub fn observe(&self, application: ApplicationId, observation: Observation) {
        let mut book = self.book();
        book.fold();
        book.ledger.observe(application, observation);
    }

    /// A focus effect was queued: the drain waits for it.
    pub fn note_focus_dispatched(&self) { self.book().outstanding += 1; }

    /// A focus effect finished or was discarded.
    pub fn finish_focus(&self) {
        let mut book = self.book();
        book.outstanding = book.outstanding.saturating_sub(1);
        drop(book);
        self.shared.idle.notify_all();
    }

    /// Every record, after folding pending reports.
    #[must_use]
    pub fn owned(&self) -> Vec<OwnedApp> {
        let mut book = self.book();
        book.fold();
        book.ledger.owned()
    }

    /// Test seed: see [`Ledger::seed`].
    #[cfg(test)]
    pub fn seed(&self, application: ApplicationId, ownership: Ownership) {
        self.book().ledger.seed(application, ownership);
    }

    /// The records that are `Ambiguous`.
    #[must_use]
    pub fn ambiguous(&self) -> Vec<OwnedApp> {
        self.owned()
            .into_iter()
            .filter(|app| app.ownership == Ownership::Ambiguous)
            .collect()
    }

    /// The ownership of one application.
    #[must_use]
    pub fn ownership(&self, application: ApplicationId) -> Option<Ownership> {
        let mut book = self.book();
        book.fold();
        book.ledger.ownership(application)
    }

    /// Ledger diagnostics, oldest first.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<String> { self.book().ledger.log.iter().cloned().collect() }

    /// The latest focus reports, oldest first.
    #[must_use]
    pub fn focus_reports(&self) -> Vec<FocusReport> { self.book().focus.iter().cloned().collect() }

    /// Requests queued or running.
    #[must_use]
    pub fn outstanding(&self) -> usize { self.book().outstanding }

    /// Waits until nothing is outstanding, `timeout` elapses or `cancel` fires.
    #[must_use]
    pub fn drain(&self, timeout: Duration, cancel: &CancellationToken) -> DrainOutcome {
        let deadline = Instant::now().checked_add(timeout);
        let shared = Arc::clone(&self.shared);
        let _wake = cancel.on_cancel(Arc::new(move || {
            let _held = shared.book.lock().unwrap_or_else(PoisonError::into_inner);
            shared.idle.notify_all();
        }));
        let mut book = self.book();
        loop {
            if book.outstanding == 0 {
                return DrainOutcome::Drained;
            }
            if cancel.is_cancelled() {
                return DrainOutcome::Cancelled { outstanding: book.outstanding };
            }
            let remaining =
                deadline.map_or(DRAIN_QUANTUM, |d| d.saturating_duration_since(Instant::now()));
            if remaining.is_zero() {
                return DrainOutcome::TimedOut { outstanding: book.outstanding };
            }
            book = self
                .shared
                .idle
                .wait_timeout(book, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    // ---- worker side -------------------------------------------------

    fn arm(&self, armed: Armed) -> Option<u64> {
        let mut book = self.book();
        if book.sealed {
            return None;
        }
        book.next_armed += 1;
        let id = book.next_armed;
        book.armed.insert(id, armed);
        drop(book);
        Some(id)
    }

    fn disarm(&self, id: u64) { self.book().armed.remove(&id); }

    fn report(&self, report: Report) {
        let mut book = self.book();
        book.inbox.push_back(report);
        book.outstanding = book.outstanding.saturating_sub(1);
        drop(book);
        self.shared.idle.notify_all();
    }

    fn ax_write(
        &self,
        exec: &Exec<'_>,
        application: ApplicationId,
        hidden: bool,
        restoring: bool,
    ) -> HiddenOutcome {
        let deadline = Instant::now() + VISIBILITY_DEADLINE;
        let issuance = ApplicationIssuance::new(application, deadline);
        let id = if restoring {
            None
        } else {
            match self.arm(Armed::Hidden(issuance.clone())) {
                Some(id) => Some(id),
                None => return HiddenOutcome::NotIssued(NotIssuedReason::Sealed),
            }
        };
        let outcome = exec.backend.application_hidden(
            HiddenRequest {
                epoch: exec.epoch,
                application,
                operation: HiddenOperation::Set(hidden),
                issuance: Some(issuance),
            },
            exec.cancel,
            Some(deadline),
        );
        if let Some(id) = id {
            self.disarm(id);
        }
        outcome
    }

    /// Runs one planned hide: public hide first, `AXHidden` only on a refusal.
    pub fn execute_hide(&self, exec: &Exec<'_>, application: ApplicationId) {
        let evidence = self.hide_evidence(exec, application);
        self.report(Report::Hide(application, evidence));
    }

    fn hide_evidence(&self, exec: &Exec<'_>, application: ApplicationId) -> HideEvidence {
        let Some(actions) = exec.actions else {
            return HideEvidence::NotSent("application control unavailable".to_owned());
        };
        let authorization =
            actions.visibility_authorization(application, Instant::now() + VISIBILITY_DEADLINE);
        let Some(id) = self.arm(Armed::Visibility(authorization.clone())) else {
            return HideEvidence::NotSent("sealed".to_owned());
        };
        let public = actions.hide(&authorization);
        self.disarm(id);
        match public {
            VisibilityOutcome::Requested => HideEvidence::Sent,
            VisibilityOutcome::Refused => match self.ax_write(exec, application, true, false) {
                HiddenOutcome::Written { status: Ok(()), observed, .. } => {
                    if observed == stache_macos::ax::AxReading::Value(true) {
                        HideEvidence::Confirmed
                    } else {
                        HideEvidence::Sent
                    }
                }
                HiddenOutcome::Written { kind, status, .. } => {
                    use stache_macos::windows::IssuedKind;
                    if kind == IssuedKind::Uncertain {
                        HideEvidence::Possible(format!("AXHidden fallback uncertain: {status:?}"))
                    } else {
                        HideEvidence::NotSent(format!("hide refused; AXHidden {status:?}"))
                    }
                }
                HiddenOutcome::Unobserved => {
                    HideEvidence::Possible("AXHidden fallback not acknowledged".to_owned())
                }
                other => HideEvidence::NotSent(format!("hide refused; fallback {other:?}")),
            },
            VisibilityOutcome::Uncertain(error) => {
                HideEvidence::Possible(format!("hide not acknowledged: {error}"))
            }
            other => HideEvidence::NotSent(format!("{other:?}")),
        }
    }

    /// Runs one planned unhide.
    pub fn execute_unhide(&self, exec: &Exec<'_>, application: ApplicationId) {
        let evidence = self.unhide_evidence(exec, application, false);
        self.report(Report::Unhide(application, evidence));
    }

    fn unhide_evidence(
        &self,
        exec: &Exec<'_>,
        application: ApplicationId,
        restoring: bool,
    ) -> UnhideEvidence {
        let Some(actions) = exec.actions else {
            return UnhideEvidence::Failed("application control unavailable".to_owned());
        };
        let authorization =
            actions.visibility_authorization(application, Instant::now() + VISIBILITY_DEADLINE);
        let id = if restoring {
            None
        } else {
            match self.arm(Armed::Visibility(authorization.clone())) {
                Some(id) => Some(id),
                None => return UnhideEvidence::Failed("sealed".to_owned()),
            }
        };
        let public = actions.unhide(&authorization);
        if let Some(id) = id {
            self.disarm(id);
        }
        match public {
            // `Requested` means the native unhide call returned true. The public
            // path has no confirming channel, so it counts as cleared (also in
            // a restore): requiring a readback here would quarantine every stop.
            // Only the AXHidden fallback has a readback, and a restore holds it
            // to that readback.
            VisibilityOutcome::Requested => UnhideEvidence::Sent,
            VisibilityOutcome::Stale => UnhideEvidence::Gone,
            VisibilityOutcome::Refused => {
                match self.ax_write(exec, application, false, restoring) {
                    HiddenOutcome::Written { status: Ok(()), observed, .. } => {
                        if observed == stache_macos::ax::AxReading::Value(false) {
                            UnhideEvidence::Shown
                        } else if restoring {
                            // An accepted write without a confirming readback
                            // proves nothing: the record stays owed (spec 06:
                            // an uncertain unhide remains pending).
                            UnhideEvidence::Failed(format!(
                                "AXHidden write accepted but not confirmed by readback ({observed:?})"
                            ))
                        } else {
                            UnhideEvidence::Sent
                        }
                    }
                    other => UnhideEvidence::Failed(format!("unhide refused; fallback {other:?}")),
                }
            }
            VisibilityOutcome::Uncertain(error) => {
                UnhideEvidence::Failed(format!("unhide not acknowledged: {error}"))
            }
            other => UnhideEvidence::Failed(format!("{other:?}")),
        }
    }

    /// Runs one focus request: unhide an owned-hidden application first, then
    /// the only activation path, then `AXRaise` through the issuance gate.
    pub fn execute_focus(
        &self,
        exec: &Exec<'_>,
        frames: &FrameControl,
        generation: u64,
        window: WindowInstance,
    ) {
        let mut steps = Vec::new();
        let application = window.application();
        'run: {
            let Some(actions) = exec.actions else {
                steps.push(FocusStep::Skipped("application control unavailable"));
                break 'run;
            };
            if self.is_sealed() {
                steps.push(FocusStep::Skipped("sealed"));
                break 'run;
            }
            let claimed = {
                let mut book = self.book();
                book.fold();
                let claimed = book.ledger.begin_unhide(application);
                if claimed {
                    book.outstanding += 1;
                }
                claimed
            };
            if claimed {
                let evidence = self.unhide_evidence(exec, application, false);
                steps.push(FocusStep::Unhide(evidence.clone()));
                self.report(Report::Unhide(application, evidence));
            }
            let authorization =
                actions.activation_authorization(application, Instant::now() + VISIBILITY_DEADLINE);
            let Some(id) = self.arm(Armed::Activation(authorization.clone())) else {
                steps.push(FocusStep::Skipped("sealed"));
                break 'run;
            };
            let activation = actions.activate(&authorization);
            self.disarm(id);
            let sent = matches!(
                activation,
                ActivationOutcome::Requested | ActivationOutcome::Uncertain(_)
            );
            steps.push(FocusStep::Activate(activation));
            if sent {
                steps.push(FocusStep::Raise(frames.raise(
                    exec.backend,
                    exec.cancel,
                    window,
                    generation,
                )));
            } else {
                steps.push(FocusStep::Skipped("activation was not sent"));
            }
        }
        let mut book = self.book();
        if book.focus.len() == LOG_CAPACITY {
            book.focus.pop_front();
        }
        book.focus.push_back(FocusReport { window, steps });
    }

    // ---- stop side ---------------------------------------------------

    /// Restores what the runtime hid: seals, waits for the in-flight requests,
    /// folds their results, then unhides every still-owned or possibly owned
    /// exact instance with restoration-only authorizations.
    ///
    /// A replaced or terminated instance clears without touching its
    /// replacement. A failed or uncertain unhide stays in the ledger and in
    /// [`RestoreOutcome::remaining`]; a later call retries exactly those.
    /// Ambiguous records are reported and retained, never unhidden.
    #[must_use]
    pub fn restore(&self, exec: &Exec<'_>, hop: Duration) -> RestoreOutcome {
        self.seal();
        match self.drain(hop, &CancellationToken::new()) {
            DrainOutcome::Drained => {}
            other => {
                let mut outcome = self.restore_outcome(0);
                outcome.blocked = Some(format!("visibility requests did not drain: {other:?}"));
                return outcome;
            }
        }
        let candidates = {
            let mut book = self.book();
            book.fold();
            book.ledger.restore_candidates()
        };
        let mut cleared = 0;
        for application in candidates {
            let evidence = self.unhide_evidence(exec, application, true);
            if !matches!(evidence, UnhideEvidence::Failed(_)) {
                cleared += 1;
            }
            self.book().ledger.restored(application, evidence);
        }
        self.restore_outcome(cleared)
    }

    fn restore_outcome(&self, cleared: usize) -> RestoreOutcome {
        let owned = self.owned();
        let (ambiguous, remaining) =
            owned.into_iter().partition(|app| app.ownership == Ownership::Ambiguous);
        RestoreOutcome {
            cleared,
            remaining,
            ambiguous,
            blocked: None,
        }
    }
}
