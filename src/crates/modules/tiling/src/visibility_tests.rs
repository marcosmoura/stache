//! Visibility ledger, executor and restore tests. Fakes only: the application
//! backend, the window backend and the clock are all scripted; nothing is ever
//! hidden, unhidden, activated or raised for real.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use stache_macos::ax::AxReading;
use stache_macos::windows::{
    EventQueue, GenerationCell, HiddenOperation, HiddenOutcome, IssuedKind, RuntimeEpoch,
};
use stache_macos::{
    ApplicationBackend, ApplicationControl, ApplicationId, DisplayId, FakeAppKey,
    FakeApplicationBackend, ManualExecutor, VisibilityOperation,
};
use stache_sdk::{CancellationToken, MainThreadExecutor};

use crate::actor::DrainOutcome;
use crate::backend::{FakeBackendCall, FakeWindowBackend, WindowBackend};
use crate::state::WorkspaceId;
use crate::test_support::app;
use crate::visibility::{
    ApplicationActions, ApplicationActionsSource, Exec, HideEvidence, Ledger, Observation,
    Ownership, Planned, Report, UnhideEvidence, VisibilityControl,
};
use crate::workspaces::{AppVisibility, VisibilityRequirement};

// ---------------------------------------------------------------- pure ledger

fn entry(application: ApplicationId, needed: bool, externally_hidden: bool) -> AppVisibility {
    AppVisibility {
        application,
        needed_by: if needed {
            vec![WorkspaceId(1)]
        } else {
            Vec::new()
        },
        displays: if needed {
            vec![DisplayId::new(1)]
        } else {
            Vec::new()
        },
        inactive_in: if needed {
            Vec::new()
        } else {
            vec![WorkspaceId(2)]
        },
        hide_wanted: !needed,
        externally_hidden,
        host: false,
    }
}

fn requirement(entries: Vec<AppVisibility>) -> VisibilityRequirement {
    VisibilityRequirement {
        finder_held: false,
        settled: false,
        desktop_host: None,
        host_active: false,
        kept_last: None,
        apps: entries,
        conflicts: Vec::new(),
    }
}

fn live(_: ApplicationId) -> bool { true }

/// The start of the test clock.
fn base() -> std::time::Instant {
    static BASE: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *BASE.get_or_init(std::time::Instant::now)
}

fn plan(ledger: &mut Ledger, entries: Vec<AppVisibility>) -> Vec<Planned> {
    plan_at(ledger, entries, base())
}

fn plan_at(
    ledger: &mut Ledger,
    entries: Vec<AppVisibility>,
    now: std::time::Instant,
) -> Vec<Planned> {
    ledger.plan(&requirement(entries), &live, true, now)
}

#[test]
fn a_hidden_wanted_application_is_hidden_once_then_owned_only_when_confirmed() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    assert_eq!(plan(&mut ledger, vec![entry(a, false, false)]), vec![
        Planned::Hide(a)
    ]);
    assert_eq!(ledger.ownership(a), Some(Ownership::PendingIssued));
    // In flight: no second request, no oscillation.
    assert!(plan(&mut ledger, vec![entry(a, false, false)]).is_empty());
    assert!(plan(&mut ledger, vec![entry(a, true, false)]).is_empty());
    ledger.apply(Report::Hide(a, HideEvidence::Sent));
    assert_eq!(ledger.ownership(a), Some(Ownership::PendingIssued));
    ledger.observe(a, Observation::Hidden);
    assert_eq!(ledger.ownership(a), Some(Ownership::Confirmed));
    assert!(
        plan(&mut ledger, vec![entry(a, false, true)]).is_empty(),
        "already owned"
    );
}

#[test]
fn a_prehidden_application_is_never_hidden_or_owned() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    assert!(plan(&mut ledger, vec![entry(a, false, true)]).is_empty());
    assert_eq!(ledger.ownership(a), None);
    // Becoming needed later never makes the runtime unhide what it never hid.
    assert!(plan(&mut ledger, vec![entry(a, true, true)]).is_empty());
    assert_eq!(ledger.ownership(a), None);
}

#[test]
fn an_external_hide_with_no_issuance_never_becomes_owned() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    ledger.observe(a, Observation::Hidden);
    assert_eq!(ledger.ownership(a), None);
    assert!(plan(&mut ledger, vec![entry(a, true, true)]).is_empty());
}

#[test]
fn a_hide_that_proved_no_send_is_never_owned() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    // The external hide lands first, then the proof that ours sent nothing.
    ledger.observe(a, Observation::Hidden);
    ledger.apply(Report::Hide(a, HideEvidence::NotSent("stale".to_owned())));
    assert_eq!(ledger.ownership(a), None);
    assert!(ledger.owned().is_empty());
}

#[test]
fn an_external_show_relinquishes_and_blocks_a_rehide_until_needed_again() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Sent));
    ledger.observe(a, Observation::Hidden);
    ledger.observe(a, Observation::Shown);
    assert_eq!(ledger.ownership(a), None, "relinquished");
    assert!(ledger.is_suppressed(a));
    assert!(
        plan(&mut ledger, vec![entry(a, false, false)]).is_empty(),
        "the user showed it; it is not hidden again"
    );
    // Needed again, then unneeded: hidden again.
    assert!(plan(&mut ledger, vec![entry(a, true, false)]).is_empty());
    assert!(!ledger.is_suppressed(a));
    assert_eq!(plan(&mut ledger, vec![entry(a, false, false)]), vec![
        Planned::Hide(a)
    ]);
}

#[test]
fn a_needed_application_is_never_hidden_even_shared_across_displays_and_inactive_workspaces() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let shared = AppVisibility {
        application: a,
        needed_by: vec![WorkspaceId(1), WorkspaceId(3)],
        displays: vec![DisplayId::new(1), DisplayId::new(2)],
        inactive_in: vec![WorkspaceId(2)],
        hide_wanted: false,
        externally_hidden: false,
        host: false,
    };
    assert!(plan(&mut ledger, vec![shared]).is_empty());
    assert!(ledger.owned().is_empty());
}

#[test]
fn an_owned_application_is_unhidden_once_when_needed_and_not_again_while_in_flight() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Confirmed));
    assert_eq!(ledger.ownership(a), Some(Ownership::Confirmed));
    assert_eq!(plan(&mut ledger, vec![entry(a, true, true)]), vec![
        Planned::Unhide(a)
    ]);
    assert!(plan(&mut ledger, vec![entry(a, true, true)]).is_empty());
    ledger.apply(Report::Unhide(a, UnhideEvidence::Sent));
    assert!(
        plan(&mut ledger, vec![entry(a, true, true)]).is_empty(),
        "awaiting the show"
    );
    ledger.observe(a, Observation::Shown);
    assert_eq!(ledger.ownership(a), None);
    assert!(!ledger.is_suppressed(a), "our own unhide is not a user choice");
}

#[test]
fn an_uncertain_hide_stays_pending_possible_and_is_never_cleared_by_time() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Possible("lost".to_owned())));
    assert_eq!(ledger.ownership(a), Some(Ownership::PendingPossible));
    for _ in 0..3 {
        assert!(plan(&mut ledger, vec![entry(a, false, false)]).is_empty());
    }
    assert_eq!(ledger.ownership(a), Some(Ownership::PendingPossible));
    assert_eq!(ledger.owned()[0].reason.as_deref(), Some("lost"));
    // Reconciled by an observation.
    ledger.observe(a, Observation::Hidden);
    assert_eq!(ledger.ownership(a), Some(Ownership::Confirmed));
}

#[test]
fn a_show_while_a_hide_may_have_landed_is_ambiguous_and_never_a_restore_candidate() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Possible("lost".to_owned())));
    ledger.observe(a, Observation::Shown);
    assert_eq!(ledger.ownership(a), Some(Ownership::Ambiguous));
    assert!(ledger.restore_candidates().is_empty());
    assert!(
        plan(&mut ledger, vec![entry(a, true, true)]).is_empty(),
        "never auto-unhidden"
    );
    assert!(ledger.owned()[0].reason.is_some());
    // A show that raced the still-unreported hide is the same ambiguity.
    let b = app(2);
    let _ = plan(&mut ledger, vec![entry(b, false, false)]);
    ledger.observe(b, Observation::Shown);
    ledger.apply(Report::Hide(b, HideEvidence::Sent));
    assert_eq!(ledger.ownership(b), Some(Ownership::Ambiguous));
}

#[test]
fn an_ambiguous_record_of_a_visible_application_is_dropped_and_not_hidden_until_needed() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Sent));
    ledger.observe(a, Observation::Shown);
    assert_eq!(ledger.ownership(a), Some(Ownership::Ambiguous));
    // The application is observed visible: nothing is held, so nothing is owed.
    // The show may have been the user's, so it is not hidden against them.
    assert!(plan(&mut ledger, vec![entry(a, false, false)]).is_empty());
    assert!(ledger.owned().is_empty());
    assert!(ledger.is_suppressed(a));
    assert!(plan(&mut ledger, vec![entry(a, false, false)]).is_empty());
    // Needed again, then not needed: it can be hidden again.
    assert!(plan(&mut ledger, vec![entry(a, true, false)]).is_empty());
    assert!(!ledger.is_suppressed(a));
    assert_eq!(plan(&mut ledger, vec![entry(a, false, false)]), vec![
        Planned::Hide(a)
    ]);
}

#[test]
fn an_unconfirmed_unhide_never_blocks_a_later_hide() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Sent));
    ledger.observe(a, Observation::Hidden);
    assert_eq!(plan(&mut ledger, vec![entry(a, true, true)]), vec![
        Planned::Unhide(a)
    ]);
    ledger.apply(Report::Unhide(a, UnhideEvidence::Sent));
    // No shown observation ever comes; the application is observed visible.
    assert_eq!(plan(&mut ledger, vec![entry(a, false, false)]), vec![
        Planned::Hide(a)
    ]);
}

#[test]
fn an_unhide_whose_shown_never_arrives_is_retried_then_bounded_and_a_hide_still_follows() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Sent));
    ledger.observe(a, Observation::Hidden);
    assert_eq!(plan(&mut ledger, vec![entry(a, true, true)]), vec![
        Planned::Unhide(a)
    ]);
    ledger.apply(Report::Unhide(a, UnhideEvidence::Sent));
    // It still reads hidden and no notification arrives: wait, then try again.
    // Nothing waits in turns: the wait is the confirmation timeout.
    assert!(plan(&mut ledger, vec![entry(a, true, true)]).is_empty());
    let due = base() + crate::visibility::CONFIRMATION_TIMEOUT;
    assert_eq!(ledger.next_deadline(base()), Some(due), "the actor wakes for it");
    let again = plan_at(&mut ledger, vec![entry(a, true, true)], due);
    assert_eq!(again, vec![Planned::Unhide(a)]);
    ledger.apply(Report::Unhide(a, UnhideEvidence::Failed("x".to_owned())));
    // Not needed any more: the failure streak does not stand in the way.
    assert!(
        plan(&mut ledger, vec![entry(a, false, true)]).is_empty(),
        "still hidden: nothing to do"
    );
}

#[test]
fn a_hide_that_never_landed_is_re_sent_a_bounded_number_of_times() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Sent));
    let mut sent = 0;
    for step in 0..80_u32 {
        let now = base() + std::time::Duration::from_millis(100) * step;
        let planned = plan_at(&mut ledger, vec![entry(a, false, false)], now);
        if !planned.is_empty() {
            sent += 1;
            ledger.apply(Report::Hide(a, HideEvidence::Sent));
        }
    }
    assert_eq!(sent, usize::from(crate::visibility::MAX_HIDE_ATTEMPTS));
    let owned = ledger.owned();
    assert_eq!(
        owned[0].reason.as_deref(),
        Some("hide abandoned: still visible after every retry"),
        "giving up is reported"
    );
}

#[test]
fn a_terminal_gone_clears_the_record_and_the_suppression() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Confirmed));
    ledger.observe(a, Observation::Gone);
    assert!(ledger.owned().is_empty());
    // The list is authoritative too: an application missing from it is gone.
    let b = app(2);
    let _ = plan(&mut ledger, vec![entry(b, false, false)]);
    ledger.apply(Report::Hide(b, HideEvidence::Confirmed));
    let _ = ledger.plan(&requirement(Vec::new()), &|_| false, true, base());
    assert!(ledger.owned().is_empty());
}

#[test]
fn normal_unhide_retries_are_capped_but_restoration_still_tries() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Confirmed));
    for _ in 0..3 {
        assert_eq!(plan(&mut ledger, vec![entry(a, true, true)]), vec![
            Planned::Unhide(a)
        ]);
        ledger.apply(Report::Unhide(a, UnhideEvidence::Failed("refused".to_owned())));
    }
    assert!(
        plan(&mut ledger, vec![entry(a, true, true)]).is_empty(),
        "capped"
    );
    assert_eq!(ledger.restore_candidates(), vec![a]);
}

// ------------------------------------------------------------ control fixture

/// Opens a fresh observation of the fake application backend per run and
/// remembers the exact ids it saw, so a test can seed the ledger with them.
pub struct FixtureSource {
    native: Arc<Native>,
    bundles: Vec<&'static str>,
    keys: Vec<FakeAppKey>,
    ids: Mutex<Vec<ApplicationId>>,
}

impl FixtureSource {
    /// Adds one running instance per bundle.
    pub fn new(native: &Arc<Native>, bundles: &[&'static str]) -> Arc<Self> {
        let keys = bundles
            .iter()
            .enumerate()
            .map(|(n, bundle)| {
                native
                    .backend
                    .add_running_with_pid(bundle, 100 + i32::try_from(n).unwrap_or_default())
            })
            .collect();
        Arc::new(Self {
            native: Arc::clone(native),
            bundles: bundles.to_vec(),
            keys,
            ids: Mutex::new(Vec::new()),
        })
    }

    /// The ids of the latest opened observation.
    pub fn ids(&self) -> Vec<ApplicationId> {
        self.ids.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The fake backend keys, in bundle order.
    pub fn keys(&self) -> Vec<FakeAppKey> { self.keys.clone() }
}

impl ApplicationActionsSource for FixtureSource {
    fn open(&self, cancel: &CancellationToken) -> Result<ApplicationActions, String> {
        let observation = self.native.control.install(cancel, |_| {}).map_err(|e| e.to_string())?;
        let ids = self
            .bundles
            .iter()
            .map(|bundle| {
                let id = *observation
                    .running_applications(bundle)
                    .map_err(|e| e.to_string())?
                    .last()
                    .ok_or_else(|| "not running".to_owned())?;
                Ok(id)
            })
            .collect::<Result<Vec<_>, String>>()?;
        *self.ids.lock().unwrap_or_else(PoisonError::into_inner) = ids;
        Ok(ApplicationActions::new(observation))
    }
}

pub struct Native {
    pub backend: Arc<FakeApplicationBackend>,
    pub executor: Arc<ManualExecutor>,
    pub control: ApplicationControl,
}

impl Native {
    pub fn new(budget: Duration) -> Self {
        let executor = Arc::new(ManualExecutor::new(true));
        let backend = Arc::new(FakeApplicationBackend::new());
        let control = ApplicationControl::with_acknowledgement_budget(
            Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
            Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
            budget,
        );
        Self { backend, executor, control }
    }

    /// Installs the observation and lists `bundles`, one running instance each.
    pub fn install(
        &self,
        bundles: &[&str],
    ) -> (ApplicationActions, Vec<(ApplicationId, FakeAppKey)>) {
        let keys: Vec<FakeAppKey> = bundles
            .iter()
            .enumerate()
            .map(|(n, bundle)| {
                self.backend
                    .add_running_with_pid(bundle, 100 + i32::try_from(n).unwrap_or_default())
            })
            .collect();
        let observation = self.control.install(&CancellationToken::new(), |_| {}).expect("install");
        let ids: Vec<(ApplicationId, FakeAppKey)> = bundles
            .iter()
            .zip(keys)
            .map(|(bundle, key)| {
                (
                    *observation.running_applications(bundle).expect("running").last().expect("id"),
                    key,
                )
            })
            .collect();
        (ApplicationActions::new(observation), ids)
    }
}

const GENERATION: u64 = 7;

struct Fixture {
    gate: Arc<GenerationCell>,
    native: Native,
    actions: ApplicationActions,
    windows: Arc<FakeWindowBackend>,
    epoch: RuntimeEpoch,
    cancel: CancellationToken,
    vis: VisibilityControl,
    ids: Vec<(ApplicationId, FakeAppKey)>,
}

impl Fixture {
    fn new(bundles: &[&str]) -> Self { Self::with_budget(bundles, Duration::from_millis(500)) }

    fn with_budget(bundles: &[&str], budget: Duration) -> Self {
        let native = Native::new(budget);
        let (actions, ids) = native.install(bundles);
        let windows = Arc::new(FakeWindowBackend::default());
        let gate = Arc::new(GenerationCell::new());
        gate.publish(GENERATION, 1);
        let epoch = windows.install(EventQueue::new(8), Arc::clone(&gate), None).expect("install");
        Self {
            gate,
            native,
            actions,
            windows,
            epoch,
            cancel: CancellationToken::new(),
            vis: VisibilityControl::new(),
            ids,
        }
    }

    fn id(&self, n: usize) -> ApplicationId { self.ids[n].0 }

    fn key(&self, n: usize) -> FakeAppKey { self.ids[n].1 }

    fn exec(&self) -> Exec<'_> {
        Exec {
            actions: Some(&self.actions),
            backend: &*self.windows,
            epoch: self.epoch,
            cancel: &self.cancel,
        }
    }

    fn plan(&self, entries: Vec<AppVisibility>) -> Vec<Planned> {
        self.vis.plan(&requirement(entries), &live, true, base())
    }

    /// Plans and runs the hide of application `n`.
    fn hide(&self, n: usize) {
        let a = self.id(n);
        assert_eq!(self.plan(vec![entry(a, false, false)]), vec![Planned::Hide(a)]);
        self.vis.execute_hide(&self.exec(), a);
    }

    fn requests(&self) -> Vec<(FakeAppKey, VisibilityOperation)> {
        self.native.backend.visibility_requests()
    }

    fn hidden_calls(&self) -> Vec<FakeBackendCall> {
        self.windows
            .calls()
            .into_iter()
            .filter(|call| matches!(call, FakeBackendCall::Hidden(..)))
            .collect()
    }
}

fn written(
    status: Result<(), stache_macos::ax::AxError>,
    hidden: bool,
    kind: IssuedKind,
) -> HiddenOutcome {
    HiddenOutcome::Written {
        kind,
        status,
        observed: AxReading::Value(hidden),
    }
}

// ------------------------------------------------------------------ executors

#[test]
fn the_public_hide_is_tried_first_and_confirmed_only_by_observation() {
    let f = Fixture::new(&["com.a"]);
    f.hide(0);
    assert_eq!(f.requests(), vec![(f.key(0), VisibilityOperation::Hide)]);
    assert!(f.native.backend.is_hidden(f.key(0)));
    assert!(
        f.hidden_calls().is_empty(),
        "no AXHidden when the public hide worked"
    );
    // Requested is a request, not a confirmation.
    assert_eq!(f.vis.ownership(f.id(0)), Some(Ownership::PendingIssued));
    f.vis.observe(f.id(0), Observation::Hidden);
    assert_eq!(f.vis.ownership(f.id(0)), Some(Ownership::Confirmed));
}

#[test]
fn a_refused_public_hide_falls_back_to_axhidden_with_a_typed_confirmed_result() {
    let f = Fixture::new(&["com.a"]);
    f.native.backend.refuse_visibility();
    f.windows.script_hidden(written(Ok(()), true, IssuedKind::Applied));
    f.hide(0);
    assert_eq!(f.hidden_calls(), vec![FakeBackendCall::Hidden(
        f.id(0),
        HiddenOperation::Set(true)
    )]);
    assert_eq!(
        f.vis.ownership(f.id(0)),
        Some(Ownership::Confirmed),
        "the AXHidden readback after our own write confirms it"
    );
}

#[test]
fn a_refused_hide_with_a_failed_fallback_is_never_owned() {
    let f = Fixture::new(&["com.a"]);
    f.native.backend.refuse_visibility();
    f.windows.script_hidden(written(
        Err(stache_macos::ax::AxError::AttributeUnsupported),
        false,
        IssuedKind::Rejected,
    ));
    f.hide(0);
    assert_eq!(f.vis.ownership(f.id(0)), None);
    assert!(f.vis.diagnostics().iter().any(|line| line.contains("never owned")));
}

#[test]
fn an_uncertain_axhidden_fallback_stays_pending_possible() {
    let f = Fixture::new(&["com.a"]);
    f.native.backend.refuse_visibility();
    f.windows.script_hidden(HiddenOutcome::Unobserved);
    f.hide(0);
    assert_eq!(f.vis.ownership(f.id(0)), Some(Ownership::PendingPossible));
}

#[test]
fn a_replaced_or_dead_instance_is_never_owned() {
    let f = Fixture::new(&["com.a"]);
    f.native.backend.terminate(f.key(0));
    f.hide(0);
    assert_eq!(f.vis.ownership(f.id(0)), None);
    assert!(f.requests().is_empty(), "nothing was sent");
}

#[test]
fn a_hide_with_no_application_control_is_never_owned() {
    let f = Fixture::new(&["com.a"]);
    let a = f.id(0);
    assert_eq!(f.plan(vec![entry(a, false, false)]), vec![Planned::Hide(a)]);
    f.vis.execute_hide(
        &Exec {
            actions: None,
            backend: &*f.windows,
            epoch: f.epoch,
            cancel: &f.cancel,
        },
        a,
    );
    assert_eq!(f.vis.ownership(a), None);
}

#[test]
fn nothing_is_planned_after_the_seal() {
    let f = Fixture::new(&["com.a"]);
    f.vis.seal();
    assert!(f.plan(vec![entry(f.id(0), false, false)]).is_empty());
}

#[test]
fn a_late_old_hide_cannot_issue_once_sealed() {
    let f = Fixture::new(&["com.a"]);
    let a = f.id(0);
    assert_eq!(f.plan(vec![entry(a, false, false)]), vec![Planned::Hide(a)]);
    // The job was planned and queued; the seal lands before it starts.
    f.vis.seal();
    f.vis.execute_hide(&f.exec(), a);
    assert!(f.requests().is_empty(), "no native hide after the seal");
    assert_eq!(f.vis.ownership(a), None);
    assert_eq!(f.vis.outstanding(), 0);
}

#[test]
fn a_hide_parked_before_its_send_is_cancelled_by_the_seal_and_never_issues() {
    let f = Arc::new(Fixture::new(&["com.a"]));
    let a = f.id(0);
    assert_eq!(f.plan(vec![entry(a, false, false)]), vec![Planned::Hide(a)]);
    let barrier = f.native.backend.block_next_visibility_before_commit();
    let runner = Arc::clone(&f);
    let worker = std::thread::spawn(move || runner.vis.execute_hide(&runner.exec(), a));
    assert!(
        barrier.wait_entered(Duration::from_secs(5)),
        "the hide is parked before its send"
    );
    f.vis.seal();
    assert!(barrier.release());
    worker.join().expect("worker");
    assert!(
        f.requests().is_empty(),
        "the old hide cannot issue after the seal"
    );
    assert!(!f.native.backend.is_hidden(f.key(0)));
    assert_eq!(f.vis.ownership(a), None);
}

#[test]
fn a_hide_already_sent_when_sealed_is_accounted_and_restored() {
    let f = Arc::new(Fixture::new(&["com.a"]));
    let a = f.id(0);
    assert_eq!(f.plan(vec![entry(a, false, false)]), vec![Planned::Hide(a)]);
    let barrier = f.native.backend.block_next_visibility_issuance();
    let runner = Arc::clone(&f);
    let worker = std::thread::spawn(move || runner.vis.execute_hide(&runner.exec(), a));
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    f.vis.seal();
    // Not drained: the committed send is still running.
    assert!(matches!(
        f.vis.drain(Duration::from_millis(30), &CancellationToken::new()),
        DrainOutcome::TimedOut { outstanding: 1 }
    ));
    let blocked = f.vis.restore(&f.exec(), Duration::from_millis(30));
    assert!(blocked.blocked.is_some() && !blocked.is_complete());
    assert!(barrier.release());
    worker.join().expect("worker");
    assert!(f.native.backend.is_hidden(f.key(0)));
    let restored = f.vis.restore(&f.exec(), Duration::from_secs(2));
    assert!(restored.is_complete(), "{restored:?}");
    assert!(!f.native.backend.is_hidden(f.key(0)));
}

// -------------------------------------------------------------------- restore

fn hidden_and_confirmed(f: &Fixture, n: usize) {
    f.hide(n);
    f.vis.observe(f.id(n), Observation::Hidden);
    assert_eq!(f.vis.ownership(f.id(n)), Some(Ownership::Confirmed));
}

#[test]
fn restore_unhides_only_what_the_runtime_hid() {
    let f = Fixture::new(&["com.a", "com.b"]);
    hidden_and_confirmed(&f, 0);
    // B was hidden by someone else and is never ours.
    f.vis.observe(f.id(1), Observation::Hidden);
    let outcome = f.vis.restore(&f.exec(), Duration::from_secs(2));
    assert!(outcome.is_complete() && outcome.cleared == 1, "{outcome:?}");
    assert_eq!(f.requests(), vec![
        (f.key(0), VisibilityOperation::Hide),
        (f.key(0), VisibilityOperation::Unhide),
    ]);
    assert!(f.vis.owned().is_empty());
}

#[test]
fn restore_never_touches_a_replacement_and_clears_a_dead_instance() {
    let f = Fixture::new(&["com.a"]);
    hidden_and_confirmed(&f, 0);
    f.native.backend.terminate(f.key(0));
    // The same bundle relaunches as a new instance (even a new pid).
    let replacement = f.native.backend.add_running_with_pid("com.a", 999);
    let outcome = f.vis.restore(&f.exec(), Duration::from_secs(2));
    assert!(outcome.is_complete(), "{outcome:?}");
    assert_eq!(
        f.requests(),
        vec![(f.key(0), VisibilityOperation::Hide)],
        "no unhide at all"
    );
    assert!(!f.native.backend.is_hidden(replacement));
    assert!(f.vis.owned().is_empty());
}

#[test]
fn an_already_shown_application_is_cleared_before_the_restore() {
    let f = Fixture::new(&["com.a"]);
    hidden_and_confirmed(&f, 0);
    f.vis.observe(f.id(0), Observation::Shown);
    let outcome = f.vis.restore(&f.exec(), Duration::from_secs(2));
    assert!(outcome.is_complete() && outcome.cleared == 0);
    assert_eq!(f.requests().len(), 1, "only the original hide");
}

#[test]
fn a_refused_restore_stays_owed_with_a_reason_and_a_retry_restores_only_the_rest() {
    let f = Fixture::new(&["com.a", "com.b"]);
    hidden_and_confirmed(&f, 0);
    hidden_and_confirmed(&f, 1);
    f.native.backend.refuse_visibility();
    f.windows.script_hidden(written(
        Err(stache_macos::ax::AxError::CannotComplete),
        true,
        IssuedKind::Uncertain,
    ));
    f.windows.script_hidden(written(Ok(()), false, IssuedKind::Applied));
    let first = f.vis.restore(&f.exec(), Duration::from_secs(2));
    // Application 0's fallback failed; application 1's fallback restored it.
    assert!(!first.is_complete());
    assert_eq!(first.remaining.len(), 1, "{first:?}");
    assert_eq!(first.remaining[0].application, f.id(0));
    assert!(first.remaining[0].reason.is_some());
    assert_eq!(first.cleared, 1);
    let before = f.requests().len();
    f.native.backend.allow_visibility();
    let second = f.vis.restore(&f.exec(), Duration::from_secs(2));
    assert!(second.is_complete(), "{second:?}");
    let retried: Vec<_> = f.requests().split_off(before);
    assert_eq!(
        retried,
        vec![(f.key(0), VisibilityOperation::Unhide)],
        "only the remaining one"
    );
}

#[test]
fn a_restore_fallback_write_without_a_confirming_readback_stays_owed() {
    let f = Fixture::new(&["com.a"]);
    hidden_and_confirmed(&f, 0);
    f.native.backend.refuse_visibility();
    // Ok status, but the readback still says hidden: not a confirmation.
    f.windows.script_hidden(HiddenOutcome::Written {
        kind: IssuedKind::Applied,
        status: Ok(()),
        observed: AxReading::Unsupported,
    });
    let first = f.vis.restore(&f.exec(), Duration::from_secs(2));
    assert!(!first.is_complete(), "{first:?}");
    assert_eq!(first.remaining.len(), 1);
    assert!(
        first.remaining[0]
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("not confirmed")),
        "{first:?}"
    );
    // A confirmed readback on the retry clears it.
    f.windows.script_hidden(written(Ok(()), false, IssuedKind::Applied));
    let second = f.vis.restore(&f.exec(), Duration::from_secs(2));
    assert!(second.is_complete(), "{second:?}");
}

fn lagging_write(f: &Fixture) {
    f.windows.script_hidden(HiddenOutcome::Written {
        kind: IssuedKind::Applied,
        status: Ok(()),
        observed: AxReading::Value(true),
    });
}

fn read(hidden: bool) -> HiddenOutcome { HiddenOutcome::Read(AxReading::Value(hidden)) }

#[test]
fn a_restore_readback_that_lags_clears_within_the_settle() {
    let f = Fixture::new(&["com.a"]);
    hidden_and_confirmed(&f, 0);
    f.native.backend.refuse_visibility();
    lagging_write(&f);
    f.windows.script_hidden(read(true));
    f.windows.script_hidden(read(false));
    let outcome = f.vis.restore(&f.exec(), Duration::from_secs(2));
    assert!(outcome.is_complete() && outcome.cleared == 1, "{outcome:?}");
    assert!(f.vis.owned().is_empty());
    assert_eq!(f.hidden_calls().len(), 3, "one write and two reads");
}

#[test]
fn a_restore_readback_that_stays_hidden_stays_owed_after_a_bounded_settle() {
    let f = Fixture::new(&["com.a"]);
    hidden_and_confirmed(&f, 0);
    f.native.backend.refuse_visibility();
    lagging_write(&f);
    for _ in 0..40 {
        f.windows.script_hidden(read(true));
    }
    let started = Instant::now();
    let outcome = f.vis.restore(&f.exec(), Duration::from_secs(2));
    let took = started.elapsed();
    assert!(!outcome.is_complete(), "{outcome:?}");
    assert!(
        outcome.remaining[0]
            .reason
            .as_deref()
            .is_some_and(|r| r.contains("not confirmed")),
        "{outcome:?}"
    );
    assert!(
        took >= Duration::from_millis(200) && took < Duration::from_secs(1),
        "{took:?}"
    );
}

#[test]
fn the_settle_is_shared_across_many_applications() {
    let bundles = [
        "com.a", "com.b", "com.c", "com.d", "com.e", "com.f", "com.g", "com.h",
    ];
    let f = Fixture::new(&bundles);
    for n in 0..bundles.len() {
        hidden_and_confirmed(&f, n);
    }
    f.native.backend.refuse_visibility();
    for _ in 0..bundles.len() {
        lagging_write(&f);
    }
    for _ in 0..400 {
        f.windows.script_hidden(read(true));
    }
    let started = Instant::now();
    let outcome = f.vis.restore(&f.exec(), Duration::from_secs(2));
    let took = started.elapsed();
    assert_eq!(outcome.remaining.len(), bundles.len(), "{outcome:?}");
    assert!(
        took < Duration::from_millis(900),
        "one shared settle, not 8: {took:?}"
    );
}

#[test]
fn the_settle_never_outlasts_the_restore_hop() {
    let f = Fixture::new(&["com.a"]);
    hidden_and_confirmed(&f, 0);
    f.native.backend.refuse_visibility();
    lagging_write(&f);
    for _ in 0..40 {
        f.windows.script_hidden(read(true));
    }
    let started = Instant::now();
    let outcome = f.vis.restore(&f.exec(), Duration::from_millis(60));
    assert!(!outcome.is_complete(), "{outcome:?}");
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn the_runtime_unhide_does_not_settle_or_reread() {
    let f = Fixture::new(&["com.a"]);
    hidden_and_confirmed(&f, 0);
    f.native.backend.refuse_visibility();
    lagging_write(&f);
    let before = f.hidden_calls().len();
    f.vis.execute_unhide(&f.exec(), f.id(0));
    assert_eq!(f.hidden_calls().len() - before, 1);
    assert!(
        !f.hidden_calls()
            .iter()
            .skip(before)
            .any(|c| matches!(c, FakeBackendCall::Hidden(_, HiddenOperation::Read)))
    );
}

#[test]
fn an_uncertain_hide_then_stop_is_reported_not_cleared() {
    let f = Arc::new(Fixture::with_budget(&["com.a"], Duration::from_millis(60)));
    let a = f.id(0);
    // Main work is queued, not inline, so the acknowledgement can time out.
    f.native.executor.set_on_main(false);
    assert_eq!(f.plan(vec![entry(a, false, false)]), vec![Planned::Hide(a)]);
    let barrier = f.native.backend.block_next_visibility_after_issue();
    let runner = Arc::clone(&f);
    let worker = std::thread::spawn(move || runner.vis.execute_hide(&runner.exec(), a));
    for _ in 0..500 {
        if f.native.executor.queued() > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let main = Arc::clone(&f);
    let drainer = std::thread::spawn(move || main.native.executor.drain());
    assert!(
        barrier.wait_entered(Duration::from_secs(5)),
        "sent, result withheld"
    );
    worker.join().expect("worker");
    assert!(barrier.release());
    drainer.join().expect("main");
    f.native.executor.set_on_main(true);
    assert_eq!(f.vis.ownership(a), Some(Ownership::PendingPossible));
    // The unhide cannot be confirmed either: the restore reports, never clears.
    f.native.backend.refuse_visibility();
    f.windows.script_hidden(HiddenOutcome::Unobserved);
    let outcome = f.vis.restore(&f.exec(), Duration::from_secs(2));
    assert!(!outcome.is_complete());
    assert_eq!(outcome.remaining.len(), 1);
    assert_eq!(f.vis.ownership(a), Some(Ownership::PendingPossible));
}

#[test]
fn an_ambiguous_record_is_reported_and_retained_but_never_unhidden() {
    let f = Fixture::new(&["com.a"]);
    let a = f.id(0);
    assert_eq!(f.plan(vec![entry(a, false, false)]), vec![Planned::Hide(a)]);
    f.vis.execute_hide(&f.exec(), a);
    f.vis.observe(a, Observation::Shown);
    assert_eq!(f.vis.ownership(a), Some(Ownership::Ambiguous));
    let outcome = f.vis.restore(&f.exec(), Duration::from_secs(2));
    assert!(outcome.is_complete(), "ambiguity never holds the stop");
    assert_eq!(outcome.ambiguous.len(), 1);
    assert!(outcome.ambiguous[0].reason.is_some());
    assert_eq!(
        f.requests(),
        vec![(f.key(0), VisibilityOperation::Hide)],
        "never unhidden"
    );
    assert!(f.vis.diagnostics().iter().any(|line| line.contains("ambiguous")));
}

#[test]
fn restore_does_not_run_while_requests_are_still_in_flight() {
    let f = Fixture::new(&["com.a"]);
    let a = f.id(0);
    assert_eq!(f.plan(vec![entry(a, false, false)]), vec![Planned::Hide(a)]);
    let outcome = f.vis.restore(&f.exec(), Duration::from_millis(40));
    assert!(outcome.blocked.is_some());
    assert!(
        f.requests().is_empty(),
        "no restoration unhide while a hide may still issue"
    );
}

// ---------------------------------------------------------------------- focus

fn window_of(f: &Fixture, n: usize) -> stache_macos::windows::WindowInstance {
    stache_macos::windows::WindowInstance::for_testing(f.windows.adapter(), f.epoch, f.id(n), 1)
}

fn focus_frames(f: &Fixture) -> (Arc<crate::actor::FrameControl>, u64) {
    let clock: Arc<dyn crate::actor::Clock> = Arc::new(crate::actor::SystemClock);
    (
        Arc::new(crate::actor::FrameControl::new(Arc::clone(&f.gate), clock)),
        GENERATION,
    )
}

#[test]
fn focus_unhides_an_owned_hidden_application_before_activation_then_raises() {
    use stache_macos::applications::ActivationOutcome;

    use crate::visibility::FocusStep;
    let f = Fixture::new(&["com.a"]);
    hidden_and_confirmed(&f, 0);
    f.windows.script_action(stache_macos::windows::WindowActionOutcome::Issued {
        kind: IssuedKind::Applied,
        native: stache_macos::windows::NativeStatus::Action(Ok(())),
        observed: None,
    });
    let (frames, generation) = focus_frames(&f);
    f.vis.note_focus_dispatched();
    f.vis.execute_focus(&f.exec(), &frames, generation, window_of(&f, 0));
    f.vis.finish_focus();
    let report = f.vis.focus_reports().pop().expect("report");
    assert!(
        matches!(report.steps[0], FocusStep::Unhide(UnhideEvidence::Sent)),
        "{report:?}"
    );
    assert_eq!(
        report.steps[1],
        FocusStep::Activate(ActivationOutcome::Requested)
    );
    assert!(matches!(report.steps[2], FocusStep::Raise(_)));
    assert_eq!(report.steps.len(), 3);
    assert!(!f.native.backend.is_hidden(f.key(0)), "unhidden first");
    assert_eq!(f.native.backend.exact_activation_requests(), vec![f.key(0)]);
    assert_eq!(f.windows.action_count(), 1, "one raise through the backend gate");
}

#[test]
fn focus_of_a_visible_application_activates_and_raises_without_unhiding() {
    use crate::visibility::FocusStep;
    let f = Fixture::new(&["com.a"]);
    let (frames, generation) = focus_frames(&f);
    f.vis.execute_focus(&f.exec(), &frames, generation, window_of(&f, 0));
    let report = f.vis.focus_reports().pop().expect("report");
    assert!(matches!(report.steps[0], FocusStep::Activate(_)));
    assert!(f.requests().is_empty());
}

#[test]
fn a_refused_or_stale_activation_is_typed_and_never_raises() {
    use stache_macos::applications::ActivationOutcome;

    use crate::visibility::FocusStep;
    let f = Fixture::new(&["com.a", "com.b"]);
    f.native.backend.refuse_exact_activation();
    let (frames, generation) = focus_frames(&f);
    f.vis.execute_focus(&f.exec(), &frames, generation, window_of(&f, 0));
    let refused = f.vis.focus_reports().pop().expect("report");
    assert_eq!(refused.steps[0], FocusStep::Activate(ActivationOutcome::Refused));
    assert!(matches!(refused.steps[1], FocusStep::Skipped(_)));
    f.native.backend.terminate(f.key(1));
    f.vis.execute_focus(&f.exec(), &frames, generation, window_of(&f, 1));
    let stale = f.vis.focus_reports().pop().expect("report");
    assert_eq!(stale.steps[0], FocusStep::Activate(ActivationOutcome::Stale));
    assert_eq!(f.windows.action_count(), 0, "a failed activation never raises");
}

#[test]
fn focus_after_the_seal_activates_and_raises_nothing() {
    use crate::visibility::FocusStep;
    let f = Fixture::new(&["com.a"]);
    let (frames, generation) = focus_frames(&f);
    f.vis.seal();
    f.vis.execute_focus(&f.exec(), &frames, generation, window_of(&f, 0));
    let report = f.vis.focus_reports().pop().expect("report");
    assert_eq!(report.steps, vec![FocusStep::Skipped("sealed")]);
    assert!(f.native.backend.exact_activation_requests().is_empty());
}

#[test]
fn a_focus_without_application_control_is_a_typed_skip() {
    use crate::visibility::FocusStep;
    let f = Fixture::new(&["com.a"]);
    let (frames, generation) = focus_frames(&f);
    f.vis.execute_focus(
        &Exec {
            actions: None,
            backend: &*f.windows,
            epoch: f.epoch,
            cancel: &f.cancel,
        },
        &frames,
        generation,
        window_of(&f, 0),
    );
    let report = f.vis.focus_reports().pop().expect("report");
    assert!(matches!(report.steps[0], FocusStep::Skipped(_)));
}

// ------------------------------------------------------------- TI-06-04

#[test]
fn the_actions_directory_answers_with_the_runs_own_exact_instances() {
    let native = Native::new(Duration::from_millis(500));
    let (actions, ids) = native.install(&["com.a", "com.b"]);
    native.backend.set_name(ids[0].1, Some("Alpha"));
    let directory = crate::visibility::ActionsDirectory::new(Arc::new(actions));
    let mut running = crate::actor::AppDirectory::running(&directory);
    running.sort_by_key(|info| info.pid);
    assert_eq!(running.len(), 2);
    assert_eq!(
        running[0].application, ids[0].0,
        "the same generation the actions accept"
    );
    assert_eq!(running[0].bundle_id, "com.a");
    assert_eq!(running[0].name, "Alpha");
    assert_eq!(running[0].pid, 100);
    assert_eq!(
        crate::actor::AppDirectory::describe(&directory, ids[1].0).map(|info| info.bundle_id),
        Some("com.b".to_owned())
    );
    native.backend.fail_next_query();
    assert!(
        crate::actor::AppDirectory::running(&directory).is_empty(),
        "a failed query yields nothing rather than a guess"
    );
}

#[test]
fn the_lazy_system_backend_starts_nothing_until_used_and_cleans_up_inertly() {
    use crate::backend::{LazySystemBackend, WindowBackend};
    let backend = LazySystemBackend::new(stache_macos::windows::AdapterOptions::default());
    assert!(!backend.started(), "construction spawns no owner thread");
    assert_eq!(
        backend
            .retry_cleanup(&CancellationToken::new(), None)
            .map(|r| r.remaining_registrations),
        Some(0)
    );
    assert!(!backend.started(), "cleanup never starts a thread");
    assert!(matches!(
        backend.close(),
        stache_macos::windows::CloseOutcome::Closed
    ));
    assert!(!backend.started());
    assert!(matches!(
        backend.close(),
        stache_macos::windows::CloseOutcome::AlreadyClosed
    ));
}

/// Re-sends `a`'s hide (each re-send reported as `evidence`) for eight
/// seconds: every attempt is spent and the hide is abandoned.
fn spend_hide_attempts(ledger: &mut Ledger, a: ApplicationId, evidence: &HideEvidence) {
    for step in 0..80_u32 {
        let now = base() + Duration::from_millis(100) * step;
        if !plan_at(ledger, vec![entry(a, false, false)], now).is_empty() {
            ledger.apply(Report::Hide(a, evidence.clone()));
        }
    }
}

#[test]
fn a_switch_during_the_last_wait_does_not_restart_the_hide_round_or_drop_its_reason() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    let possible = HideEvidence::Possible("not acknowledged".to_owned());
    ledger.apply(Report::Hide(a, possible.clone()));
    let mut last = base();
    let mut sent = 0;
    for step in 0..80_u32 {
        let now = base() + std::time::Duration::from_millis(100) * step;
        if !plan_at(&mut ledger, vec![entry(a, false, false)], now).is_empty() {
            ledger.apply(Report::Hide(a, possible.clone()));
            last = now;
            sent += 1;
            if sent == usize::from(crate::visibility::MAX_HIDE_ATTEMPTS) {
                break;
            }
        }
    }
    let reason = ledger.owned()[0].reason.clone();
    assert!(reason.is_some(), "the evidence reason");
    // A switch within the last confirmation window.
    ledger.rearm_abandoned();
    let soon = last + std::time::Duration::from_millis(100);
    assert!(
        plan_at(&mut ledger, vec![entry(a, false, false)], soon).is_empty(),
        "the round is not restarted early"
    );
    assert_eq!(ledger.owned()[0].reason, reason, "the evidence reason stays");
}

#[test]
fn an_ambiguous_record_keeps_its_reason_across_a_switch() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Possible("lost".to_owned())));
    ledger.observe(a, Observation::Shown);
    assert_eq!(ledger.ownership(a), Some(Ownership::Ambiguous));
    let reason = ledger.owned()[0].reason.clone();
    ledger.rearm_abandoned();
    assert_eq!(ledger.owned()[0].reason, reason);
}

#[test]
fn an_abandoned_hide_that_later_reads_hidden_drops_the_abandon_reason() {
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Sent));
    // It reads visible: re-sent until abandoned.
    spend_hide_attempts(&mut ledger, a, &HideEvidence::Sent);
    assert_eq!(
        ledger.owned()[0].reason.as_deref(),
        Some(crate::visibility::ABANDONED_REASON)
    );
    ledger.observe(a, Observation::Hidden);
    assert_eq!(ledger.owned()[0].reason, None, "no stale abandon reason");
}

#[test]
fn the_startup_probe_is_spent_even_when_finder_had_a_window_then() {
    let (mut ledger, finder) = (Ledger::default(), app(1));
    // Startup is done; Finder has a tracked window, so it is no host yet.
    let mut startup = requirement(vec![entry(finder, true, false)]);
    startup.settled = true;
    assert!(ledger.plan(&startup, &|_| true, true, base()).is_empty());
    // Later its window closed and someone else hid it; nothing hides now.
    let mut host = entry(finder, false, true);
    host.host = true;
    host.hide_wanted = false;
    let mut later = requirement(vec![host]);
    later.settled = true;
    assert!(
        ledger.plan(&later, &|_| true, true, base()).is_empty(),
        "no un-hide outside the documented cases"
    );
}

/// Ghostty (`a`) wanted hidden, a windowless host (`h`) chosen because
/// Finder is held hidden.
fn hosted(a: ApplicationId, h: ApplicationId, active: bool, hidden: bool) -> VisibilityRequirement {
    let mut host = entry(h, false, hidden);
    host.hide_wanted = false;
    host.inactive_in = Vec::new();
    host.host = true;
    VisibilityRequirement {
        finder_held: true,
        settled: true,
        desktop_host: Some(h),
        host_active: active,
        ..requirement(vec![entry(a, false, false), host])
    }
}

#[test]
fn a_windowless_host_is_unhidden_and_activated_before_any_hide() {
    let (mut ledger, a, h) = (Ledger::default(), app(1), app(2));
    let planned = ledger.plan(&hosted(a, h, false, true), &live, true, base());
    assert_eq!(planned, vec![Planned::Unhide(h), Planned::Activate(h)]);
    ledger.apply(Report::Unhide(h, UnhideEvidence::Sent));
    // Waiting for the activation: no hide, no second activation.
    let later = base() + Duration::from_millis(100);
    assert!(ledger.plan(&hosted(a, h, false, false), &live, true, later).is_empty());
    assert_eq!(
        ledger.next_deadline(later),
        Some(base() + crate::visibility::CONFIRMATION_TIMEOUT)
    );
    // Active: the hides go out.
    assert_eq!(
        ledger.plan(&hosted(a, h, true, false), &live, true, later),
        vec![Planned::Hide(a)]
    );
}

#[test]
fn a_host_activation_that_is_refused_or_never_seen_releases_the_hides() {
    let (a, h) = (app(1), app(2));
    // The once-per-run probe of a visible host reads it shown.
    let mut ledger = Ledger::default();
    assert_eq!(
        ledger.plan(&hosted(a, h, false, false), &live, true, base()),
        vec![Planned::Unhide(h), Planned::Activate(h)]
    );
    ledger.apply(Report::Unhide(h, UnhideEvidence::Shown));
    ledger.apply(Report::Activate(h, false));
    assert_eq!(
        ledger.plan(&hosted(a, h, false, false), &live, true, base()),
        vec![Planned::Hide(a)]
    );

    let mut ledger = Ledger::default();
    let _ = ledger.plan(&hosted(a, h, false, false), &live, true, base());
    let late = base() + crate::visibility::CONFIRMATION_TIMEOUT;
    assert_eq!(
        ledger.plan(&hosted(a, h, false, false), &live, true, late),
        vec![Planned::Hide(a)]
    );
}

#[test]
fn a_hide_read_back_hidden_is_never_resent_or_abandoned() {
    // Measured: an application that already was hidden refuses the public hide,
    // the AXHidden readback says hidden and no hidden notification ever comes.
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Confirmed));
    for step in 0..80_u32 {
        let now = base() + Duration::from_millis(100) * step;
        assert!(plan_at(&mut ledger, vec![entry(a, false, false)], now).is_empty());
    }
    assert_eq!(ledger.ownership(a), Some(Ownership::Confirmed));
    assert_eq!(ledger.owned()[0].reason, None, "never abandoned");
    // A real show still releases it.
    ledger.observe(a, Observation::Shown);
    assert_eq!(ledger.ownership(a), None);
}

#[test]
fn a_host_seen_active_is_not_activated_again_after_a_rearm() {
    let (a, h) = (app(1), app(2));
    let mut ledger = Ledger::default();
    let _ = ledger.plan(&hosted(a, h, false, false), &live, true, base());
    ledger.apply(Report::Unhide(h, UnhideEvidence::Shown));
    let _ = ledger.plan(&hosted(a, h, true, false), &live, true, base());
    // The user opens an accessory app: the host is no longer active.
    let late = base() + crate::visibility::CONFIRMATION_TIMEOUT * 2;
    for _ in 0..3 {
        ledger.rearm_abandoned();
        let planned = ledger.plan(&hosted(a, h, false, false), &live, true, late);
        assert!(!planned.contains(&Planned::Activate(h)), "{planned:?}");
    }
}

#[test]
fn a_switch_never_resends_a_hide_that_read_back_hidden() {
    // An application that read back hidden (it was hidden before this run)
    // stays quiet across switches and activations: re-sending its hide on
    // every one queued slow, useless requests ahead of the hide that matters.
    let (mut ledger, a) = (Ledger::default(), app(1));
    let _ = plan(&mut ledger, vec![entry(a, false, false)]);
    ledger.apply(Report::Hide(a, HideEvidence::Confirmed));
    for round in 0..5_u32 {
        ledger.rearm_abandoned();
        let start = base() + Duration::from_secs(20) * round;
        for step in 0..80_u32 {
            let now = start + Duration::from_millis(100) * step;
            let planned = plan_at(&mut ledger, vec![entry(a, false, false)], now);
            assert!(planned.is_empty(), "round {round} step {step}: {planned:?}");
        }
        assert_eq!(ledger.owned()[0].reason, None, "never abandoned");
    }
}
