//! Fake-only tests: no observer, no symbol and no window list is touched.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::skylight::{SkyLightSpaceNotify, SkyLightSymbols, SpaceChangeRegistrar};
use super::*;
use crate::border_overlay::spaces::{SpaceFact as F, SpaceMailbox, UnknownCause as U};
use crate::windows::{AdapterId, RuntimeEpoch, WindowInstance};
use crate::{ApplicationId, DisplayId};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn display(n: u32) -> DisplayId { DisplayId::new(n) }

// ---- fakes -------------------------------------------------------------------

#[derive(Clone, Default)]
struct FakeDisplays(
    Arc<Mutex<Option<Vec<u32>>>>,
    Arc<Mutex<BTreeMap<u32, DisplayGeometry>>>,
);

impl FakeDisplays {
    fn with(ids: &[u32]) -> Self { Self(Arc::new(Mutex::new(Some(ids.to_vec()))), Arc::default()) }

    fn set(&self, ids: Option<&[u32]>) { *lock(&self.0) = ids.map(<[u32]>::to_vec); }

    /// Gives display `id` a geometry `width` points wide.
    fn resize(&self, id: u32, width: f64) {
        lock(&self.1).insert(id, DisplayGeometry {
            x: 0.0,
            y: 0.0,
            width,
            height: 1200.0,
            pixels_wide: 1920,
            pixels_high: 1200,
        });
    }
}

impl DisplayLister for FakeDisplays {
    fn active(&self, out: &mut [u32; MAX_DISPLAYS]) -> Option<usize> {
        let ids = lock(&self.0).clone()?;
        out[..ids.len()].copy_from_slice(&ids);
        Some(ids.len())
    }

    fn geometry(&self, id: u32) -> Option<DisplayGeometry> { lock(&self.1).get(&id).copied() }
}

#[derive(Default)]
struct Control {
    woken: Mutex<bool>,
    changed: Condvar,
    /// Registration results to hand out, per source (default `Ok`).
    scripted: Mutex<BTreeMap<usize, VecDeque<Result<(), U>>>>,
    log: Mutex<Vec<String>>,
    hub: Mutex<Option<Arc<SourceHub>>>,
    stuck: AtomicBool,
}

impl Control {
    fn index(source: Source) -> usize { Source::ALL.iter().position(|s| *s == source).unwrap_or(0) }

    fn script(&self, source: Source, results: &[Result<(), U>]) {
        lock(&self.scripted).insert(Self::index(source), results.iter().copied().collect());
    }

    fn log(&self) -> Vec<String> { lock(&self.log).clone() }

    fn hub(&self) -> Arc<SourceHub> { lock(&self.hub).clone().expect("session created") }

    fn wake(&self) {
        *lock(&self.woken) = true;
        self.changed.notify_all();
    }
}

struct FakeSession {
    control: Arc<Control>,
}

impl SpaceSession for FakeSession {
    fn waker(&self) -> LoopWaker {
        let control = Arc::clone(&self.control);
        Arc::new(move || control.wake())
    }

    fn register(&mut self, source: Source) -> Result<(), U> {
        lock(&self.control.log).push(format!("register {source:?}"));
        lock(&self.control.scripted)
            .get_mut(&Control::index(source))
            .and_then(VecDeque::pop_front)
            .unwrap_or(Ok(()))
    }

    fn unregister(&mut self, source: Source) {
        lock(&self.control.log).push(format!("unregister {source:?}"));
    }

    fn pump(&mut self) {
        while self.control.stuck.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        let guard = lock(&self.control.woken);
        let mut guard = self
            .control
            .changed
            .wait_while(guard, |woken| !*woken)
            .unwrap_or_else(PoisonError::into_inner);
        *guard = false;
    }
}

struct Rig {
    source: SystemSpaceEvents,
    control: Arc<Control>,
    mailbox: SpaceMailbox,
    outcomes: Arc<Mutex<Vec<StopOutcome>>>,
}

fn rig(budget: Duration) -> Rig {
    let control = Arc::new(Control::default());
    let displays = FakeDisplays::with(&[1, 2]);
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let sessions = {
        let control = Arc::clone(&control);
        Arc::new(move || {
            let control = Arc::clone(&control);
            let factory: SessionFactory = Box::new(move |hub| {
                *lock(&control.hub) = Some(hub);
                Box::new(FakeSession { control }) as Box<dyn SpaceSession>
            });
            (factory, Box::new(displays.clone()) as Box<dyn DisplayLister>)
        })
    };
    let sink_outcomes = Arc::clone(&outcomes);
    let observer: StopObserver = Arc::new(move |outcome| lock(&sink_outcomes).push(outcome));
    Rig {
        source: SystemSpaceEvents::with_sessions(sessions, budget, Some(observer)),
        control,
        mailbox: SpaceMailbox::new(),
        outcomes,
    }
}

impl Rig {
    fn start(&mut self) { self.source.start(self.mailbox.sink()); }

    /// Waits until at least `count` facts arrived, then returns everything.
    fn facts(&self, count: usize) -> Vec<SpaceFact> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut facts = Vec::new();
        while facts.len() < count && Instant::now() < deadline {
            facts.extend(self.mailbox.drain().facts);
            std::thread::sleep(Duration::from_millis(2));
        }
        facts
    }
}

use super::super::spaces::SpaceFact;

fn hub_rig(ids: &[u32]) -> (Arc<SourceHub>, SpaceMailbox, FakeDisplays) {
    let mailbox = SpaceMailbox::new();
    let displays = FakeDisplays::with(ids);
    let hub = Arc::new(SourceHub::new(
        mailbox.sink(),
        Box::new(displays.clone()),
        Arc::new(Commands::default()),
    ));
    (hub, mailbox, displays)
}

// ---- notification names -> facts ------------------------------------------------

#[test]
fn dock_show_and_exit_map_to_mission_control_facts() {
    let (hub, mailbox, _) = hub_rig(&[1]);
    hub.mission_control(true);
    hub.mission_control(false);
    assert_eq!(mailbox.drain().facts, [
        F::MissionControlEntered,
        F::MissionControlExited
    ]);
}

#[test]
fn the_dock_notification_names_are_the_documented_candidates() {
    use crate::ax::AxNotification as N;
    let names: Vec<_> = N::DOCK.iter().map(|n| n.name()).collect();
    assert_eq!(names, [
        "AXExposeShowAllWindows",
        "AXExposeShowFrontWindows",
        "AXExposeShowDesktop",
        "AXExposeExit"
    ]);
}

#[test]
fn a_private_space_change_begins_a_transition_on_every_display() {
    let (hub, mailbox, _) = hub_rig(&[1, 2]);
    hub.space_change_began();
    assert_eq!(mailbox.drain().facts, [
        F::SpaceChangeBegan { display: display(1) },
        F::SpaceChangeBegan { display: display(2) },
    ]);
}

#[test]
fn the_public_completion_covers_active_and_pending_displays_once() {
    let (hub, mailbox, displays) = hub_rig(&[1, 2]);
    hub.space_change_began();
    let _ = mailbox.drain();
    displays.set(Some(&[2, 3]));
    hub.space_change_completed();
    let mut facts = mailbox.drain().facts;
    facts.sort_by_key(|fact| format!("{fact:?}"));
    assert_eq!(facts, [
        F::SpaceChangeCompleted { display: display(1) },
        F::SpaceChangeCompleted { display: display(2) },
        F::SpaceChangeCompleted { display: display(3) },
    ]);
    hub.space_change_completed();
    assert_eq!(
        mailbox.drain().facts.len(),
        2,
        "pending was cleared, only active remain"
    );
}

#[test]
fn an_unreadable_display_list_fails_closed() {
    let (hub, mailbox, displays) = hub_rig(&[1]);
    displays.set(None);
    hub.space_change_began();
    assert_eq!(mailbox.drain().facts, [F::SourceLost(U::QueryFailed)]);
    displays.set(Some(&[]));
    hub.space_change_began();
    assert_eq!(mailbox.drain().facts, [F::SourceLost(U::QueryFailed)]);
}

#[test]
fn a_layout_change_reports_both_ends_and_completes_vanished_transitions() {
    let (hub, mailbox, displays) = hub_rig(&[1, 2]);
    hub.space_change_began();
    let _ = mailbox.drain();
    displays.set(Some(&[1]));
    hub.display_layout_changed();
    assert_eq!(mailbox.drain().facts, [
        F::DisplayLayoutChanging,
        F::SpaceChangeCompleted { display: display(2) },
        F::DisplayLayoutChanged,
    ]);
}

#[test]
fn screen_parameter_notifications_with_an_unchanged_layout_are_ignored() {
    // macOS also posts the notification when only the display's HDR headroom
    // changed (many times a second while brightness adapts): no layout fact.
    let (hub, mailbox, _) = hub_rig(&[1, 2]);
    hub.display_layout_changed();
    let _ = mailbox.drain();
    for _ in 0..30 {
        hub.display_layout_changed();
    }
    assert!(mailbox.drain().facts.is_empty(), "the layout did not change");
}

#[test]
fn a_primed_hub_ignores_the_first_unchanged_notification_too() {
    let (hub, mailbox, displays) = hub_rig(&[1]);
    displays.resize(1, 1920.0);
    hub.prime_display_layout();
    hub.display_layout_changed();
    assert!(mailbox.drain().facts.is_empty(), "same displays, same geometry");
}

#[test]
fn a_geometry_change_of_the_same_displays_is_a_layout_change_once() {
    let (hub, mailbox, displays) = hub_rig(&[1]);
    displays.resize(1, 1920.0);
    hub.prime_display_layout();
    displays.resize(1, 2560.0);
    hub.display_layout_changed();
    assert_eq!(mailbox.drain().facts, [
        F::DisplayLayoutChanging,
        F::DisplayLayoutChanged
    ]);
    hub.display_layout_changed();
    assert!(
        mailbox.drain().facts.is_empty(),
        "the new layout is the known one"
    );
}

#[test]
fn an_unreadable_display_list_fails_closed_and_the_next_read_reports_once() {
    let (hub, mailbox, displays) = hub_rig(&[1]);
    hub.prime_display_layout();
    displays.set(None);
    for _ in 0..2 {
        hub.display_layout_changed();
        assert_eq!(mailbox.drain().facts, [
            F::DisplayLayoutChanging,
            F::SourceLost(U::QueryFailed),
            F::DisplayLayoutChanged,
        ]);
    }
    displays.set(Some(&[1]));
    hub.display_layout_changed();
    assert_eq!(mailbox.drain().facts, [
        F::DisplayLayoutChanging,
        F::DisplayLayoutChanged
    ]);
    hub.display_layout_changed();
    assert!(mailbox.drain().facts.is_empty());
}

#[test]
fn resync_repushes_mission_control_and_pending_transitions() {
    let (hub, mailbox, _) = hub_rig(&[1, 2]);
    hub.resync();
    assert!(
        mailbox.drain().facts.is_empty(),
        "nothing active, nothing to re-push"
    );
    hub.mission_control(true);
    hub.space_change_began();
    let _ = mailbox.drain();
    hub.resync();
    assert_eq!(mailbox.drain().facts, [
        F::MissionControlEntered,
        F::SpaceChangeBegan { display: display(1) },
        F::SpaceChangeBegan { display: display(2) },
    ]);
}

#[test]
fn a_closed_hub_pushes_nothing_and_close_waits_for_a_push_in_progress() {
    let (hub, mailbox, _) = hub_rig(&[1]);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let entered = Arc::new(AtomicBool::new(false));
    {
        let (release, entered) = (Arc::clone(&release), Arc::clone(&entered));
        mailbox.set_waker(Some(Arc::new(move || {
            entered.store(true, Ordering::SeqCst);
            let (flag, changed) = &*release;
            let guard = lock(flag);
            let _guard = changed.wait_while(guard, |released| !*released).unwrap();
        })));
    }
    let worker = {
        let hub = Arc::clone(&hub);
        std::thread::spawn(move || hub.mission_control(true))
    };
    while !entered.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        !hub.close(Duration::from_millis(20)),
        "a push is still in progress"
    );
    {
        let (flag, changed) = &*release;
        *lock(flag) = true;
        changed.notify_all();
    }
    worker.join().unwrap();
    assert!(hub.close(Duration::from_secs(1)));
    let _ = mailbox.drain();
    hub.mission_control(true);
    assert!(mailbox.drain().facts.is_empty(), "nothing is pushed after close");
}

// ---- registration state machine -------------------------------------------------

fn collect(f: impl FnOnce(&mut dyn FnMut(SpaceFact))) -> Vec<SpaceFact> {
    let mut facts = Vec::new();
    f(&mut |fact| facts.push(fact));
    facts
}

#[test]
fn all_sources_registered_reports_started_once() {
    let mut state = OwnerState::default();
    let facts = collect(|emit| {
        for source in Source::ALL {
            state.registered(source, Ok(()), emit);
        }
        state.finish_initial(emit);
        state.finish_initial(emit);
    });
    assert_eq!(facts, [F::SourceRestored(U::SourcesNotStarted)]);
    assert!(state.started());
}

#[test]
fn a_failed_source_is_lost_and_the_start_report_waits_for_it() {
    let mut state = OwnerState::default();
    let facts = collect(|emit| {
        state.registered(Source::Dock, Err(U::DockUnregistered), emit);
        state.registered(Source::Workspace, Ok(()), emit);
        state.registered(
            Source::SkyLight,
            Err(U::MissingSymbol("SLSMainConnectionID")),
            emit,
        );
        state.finish_initial(emit);
    });
    assert_eq!(facts, [
        F::SourceLost(U::DockUnregistered),
        F::SourceLost(U::MissingSymbol("SLSMainConnectionID")),
    ]);
    assert!(!state.started());
    // The Dock comes back, but the symbol stays missing: still not started.
    let facts = collect(|emit| state.registered(Source::Dock, Ok(()), emit));
    assert_eq!(facts, [F::SourceRestored(U::DockUnregistered)]);
    assert!(!state.started());
}

#[test]
fn a_dock_that_was_absent_at_start_completes_the_start_when_it_registers() {
    let mut state = OwnerState::default();
    let _ = collect(|emit| {
        state.registered(Source::Dock, Err(U::DockUnregistered), emit);
        state.registered(Source::Workspace, Ok(()), emit);
        state.registered(Source::SkyLight, Ok(()), emit);
        state.finish_initial(emit);
    });
    let facts = collect(|emit| state.registered(Source::Dock, Ok(()), emit));
    assert_eq!(facts, [
        F::SourceRestored(U::DockUnregistered),
        F::SourceRestored(U::SourcesNotStarted)
    ]);
}

#[test]
fn a_shared_cause_is_restored_only_when_no_source_still_has_it() {
    let mut state = OwnerState::default();
    let _ = collect(|emit| {
        state.registered(Source::Dock, Err(U::RegistrationFailed), emit);
        state.registered(Source::Workspace, Err(U::RegistrationFailed), emit);
    });
    let facts = collect(|emit| state.registered(Source::Dock, Ok(()), emit));
    assert!(facts.is_empty(), "the workspace still has that cause");
    let facts = collect(|emit| state.registered(Source::Workspace, Ok(()), emit));
    assert_eq!(facts, [F::SourceRestored(U::RegistrationFailed)]);
}

#[test]
fn a_new_cause_supersedes_the_old_one_of_the_same_source() {
    let mut state = OwnerState::default();
    let _ = collect(|emit| state.registered(Source::Dock, Err(U::RegistrationFailed), emit));
    let facts = collect(|emit| state.registered(Source::Dock, Err(U::DockUnregistered), emit));
    assert_eq!(facts, [
        F::SourceLost(U::DockUnregistered),
        F::SourceRestored(U::RegistrationFailed)
    ]);
    let facts = collect(|emit| state.resync_lost(emit));
    assert_eq!(facts, [F::SourceLost(U::DockUnregistered)]);
}

// ---- the owner thread with a fake session ------------------------------------------

#[test]
fn the_owner_registers_every_source_and_reports_started() {
    let mut rig = rig(Duration::from_secs(2));
    rig.start();
    assert_eq!(rig.facts(1), [F::SourceRestored(U::SourcesNotStarted)]);
    assert_eq!(rig.control.log(), [
        "register Dock",
        "register Workspace",
        "register SkyLight"
    ]);
    assert_eq!(
        rig.source.stop_bounded(Duration::from_secs(2)),
        StopOutcome::Stopped
    );
}

#[test]
fn registration_failures_push_source_lost_and_never_the_start_report() {
    let mut rig = rig(Duration::from_secs(2));
    rig.control.script(Source::Dock, &[Err(U::DockUnregistered)]);
    rig.control.script(Source::SkyLight, &[Err(U::MissingSymbol(
        "SLSRegisterConnectionNotifyProc",
    ))]);
    rig.start();
    assert_eq!(rig.facts(2), [
        F::SourceLost(U::DockUnregistered),
        F::SourceLost(U::MissingSymbol("SLSRegisterConnectionNotifyProc")),
    ]);
    std::thread::sleep(Duration::from_millis(30));
    assert!(rig.mailbox.drain().facts.is_empty());
    let _ = rig.source.stop_bounded(Duration::from_secs(2));
}

#[test]
fn a_dock_relaunch_is_lost_then_restored() {
    let mut rig = rig(Duration::from_secs(2));
    rig.start();
    assert_eq!(rig.facts(1).len(), 1);
    rig.control.hub().dock_application_changed();
    assert_eq!(rig.facts(2), [
        F::SourceLost(U::DockUnregistered),
        F::SourceRestored(U::DockUnregistered)
    ]);
    // The old observer is removed before the new one is created.
    let log = rig.control.log();
    assert_eq!(&log[3..], ["unregister Dock", "register Dock"]);
    let _ = rig.source.stop_bounded(Duration::from_secs(2));
}

#[test]
fn a_dock_that_stays_absent_stays_lost() {
    let mut rig = rig(Duration::from_secs(2));
    rig.start();
    let _ = rig.facts(1);
    rig.control.script(Source::Dock, &[Err(U::DockUnregistered)]);
    rig.control.hub().dock_application_changed();
    assert_eq!(rig.facts(1), [F::SourceLost(U::DockUnregistered)]);
    std::thread::sleep(Duration::from_millis(30));
    assert!(rig.mailbox.drain().facts.is_empty());
    rig.control.script(Source::Dock, &[Ok(())]);
    rig.control.hub().dock_application_changed();
    assert_eq!(rig.facts(1), [F::SourceRestored(U::DockUnregistered)]);
    let _ = rig.source.stop_bounded(Duration::from_secs(2));
}

#[test]
fn a_resync_re_pushes_the_state_in_full_and_ends_with_the_overflow_restore() {
    let mut rig = rig(Duration::from_secs(2));
    rig.control.script(Source::SkyLight, &[Err(U::RegistrationFailed)]);
    rig.start();
    let _ = rig.facts(1);
    rig.control.hub().mission_control(true);
    rig.control.hub().space_change_began();
    let _ = rig.facts(3);
    rig.source.resync();
    assert_eq!(rig.facts(5), [
        F::SourceLost(U::RegistrationFailed),
        F::MissionControlEntered,
        F::SpaceChangeBegan { display: display(1) },
        F::SpaceChangeBegan { display: display(2) },
        F::SourceRestored(U::MailboxOverflow),
    ]);
    let _ = rig.source.stop_bounded(Duration::from_secs(2));
}

#[test]
fn stop_closes_the_sink_before_the_join_so_no_fact_arrives_after_it() {
    let mut rig = rig(Duration::from_secs(2));
    rig.start();
    let _ = rig.facts(1);
    let hub = rig.control.hub();
    assert_eq!(
        rig.source.stop_bounded(Duration::from_secs(2)),
        StopOutcome::Stopped
    );
    // A late callback (any thread) finds the hub closed.
    hub.mission_control(true);
    hub.space_change_began();
    hub.dock_application_changed();
    assert!(rig.mailbox.drain().facts.is_empty());
    assert!(!hub.accepting());
    let log = rig.control.log();
    assert!(log.ends_with(&[
        "unregister Dock".to_owned(),
        "unregister Workspace".to_owned(),
        "unregister SkyLight".to_owned()
    ]));
    assert_eq!(*lock(&rig.outcomes), [StopOutcome::Stopped]);
    assert_eq!(
        rig.source.stop_bounded(Duration::from_secs(1)),
        StopOutcome::NotRunning
    );
}

#[test]
fn a_stop_that_passes_the_join_bound_is_reported_and_retried() {
    let mut rig = rig(Duration::from_millis(50));
    rig.start();
    let _ = rig.facts(1);
    rig.control.stuck.store(true, Ordering::SeqCst);
    // Wake the owner so it is inside the stuck pump when the stop arrives.
    rig.control.wake();
    std::thread::sleep(Duration::from_millis(20));
    let outcome = rig.source.stop_bounded(Duration::from_millis(50));
    assert_eq!(outcome, StopOutcome::TimedOut {
        owner_joined: false,
        callbacks_drained: true
    });
    assert_eq!(rig.source.last_stop(), Some(outcome));
    // Released, a retry joins and reports a clean stop.
    rig.control.stuck.store(false, Ordering::SeqCst);
    rig.control.wake();
    assert_eq!(
        rig.source.stop_bounded(Duration::from_secs(2)),
        StopOutcome::Stopped
    );
}

#[test]
fn start_creates_nothing_before_it_is_called_and_a_second_start_is_refused_while_stuck() {
    let rig_unused = rig(Duration::from_millis(30));
    assert!(rig_unused.control.log().is_empty());
    assert_eq!(rig_unused.source.last_stop(), None);
    let mut rig = rig(Duration::from_millis(30));
    rig.start();
    let _ = rig.facts(1);
    rig.control.stuck.store(true, Ordering::SeqCst);
    rig.control.wake();
    std::thread::sleep(Duration::from_millis(20));
    rig.start();
    assert_eq!(rig.facts(1), [F::SourceLost(U::OwnerStopped)]);
    rig.control.stuck.store(false, Ordering::SeqCst);
    rig.control.wake();
    assert_eq!(
        rig.source.stop_bounded(Duration::from_secs(2)),
        StopOutcome::Stopped
    );
}

// ---- the private notification ---------------------------------------------------------

#[test]
fn a_missing_private_symbol_fails_registration_by_name() {
    let (hub, _mailbox, _) = hub_rig(&[1]);
    let mut notify = SkyLightSpaceNotify::with_symbols(SkyLightSymbols::missing());
    assert_eq!(
        notify.register(hub),
        Err(U::MissingSymbol("SLSMainConnectionID")),
        "fail closed, nothing registered"
    );
    notify.unregister();
}

#[test]
fn an_injected_lookup_that_finds_nothing_is_missing_and_resolves_no_symbol() {
    // SAFETY: the lookup returns null for every name, so nothing is transmuted.
    let symbols = unsafe { SkyLightSymbols::from_lookup(|_: &CStr| std::ptr::null_mut()) };
    assert_eq!(symbols.missing_symbol(), Some("SLSMainConnectionID"));
}

// ---- the on-screen confirmation -----------------------------------------------------------

struct FakeList(Result<Vec<u32>, ()>);

impl OnScreenWindows for FakeList {
    fn on_screen(&mut self, ids: &mut Vec<u32>) -> Result<(), WindowListError> {
        match &self.0 {
            Ok(list) => {
                *ids = list.clone();
                Ok(())
            }
            Err(()) => Err(WindowListError),
        }
    }
}

fn instance(n: u64) -> WindowInstance {
    static ADAPTER: std::sync::OnceLock<AdapterId> = std::sync::OnceLock::new();
    let adapter = *ADAPTER.get_or_init(AdapterId::for_testing);
    WindowInstance::for_testing(
        adapter,
        RuntimeEpoch::for_testing(1),
        ApplicationId::for_testing(1),
        n,
    )
}

#[test]
fn only_listed_windows_are_confirmed_and_a_failed_list_confirms_nothing() {
    let windows = [(instance(1), 10), (instance(2), 20), (instance(3), 30)];
    let confirmed = confirm_on_screen(&mut FakeList(Ok(vec![30, 10, 99])), &windows).unwrap();
    assert_eq!(confirmed.into_iter().collect::<Vec<_>>(), [
        instance(1),
        instance(3)
    ]);
    assert!(confirm_on_screen(&mut FakeList(Ok(vec![])), &windows).unwrap().is_empty());
    assert_eq!(
        confirm_on_screen(&mut FakeList(Err(())), &windows),
        Err(WindowListError)
    );
    // Nothing to confirm needs no query.
    assert!(confirm_on_screen(&mut FakeList(Err(())), &[]).unwrap().is_empty());
}

// ---- private Space-change candidates (diagnostics) -----------------------------------

#[test]
fn a_private_candidate_event_is_logged_by_id_then_begins_a_transition() {
    let (hub, mailbox, _) = hub_rig(&[1]);
    hub.private_space_event(1400);
    assert_eq!(mailbox.drain().facts, [
        F::Diagnostic(crate::border_overlay::SpaceDiagnostic::PrivateEvent { event: 1400 }),
        F::SpaceChangeBegan { display: display(1) },
    ]);
}

/// The candidate callbacks the fake registration accepted, by event id.
static FAKE_REGISTERED: Mutex<Vec<(u32, super::super::private_skylight::NotifyCallback)>> =
    Mutex::new(Vec::new());

const extern "C" fn fake_connection() -> std::ffi::c_int { 7 }

/// Accepts only `1401`, refuses every other id with a `CGError`.
unsafe extern "C" fn fake_register(
    _: std::ffi::c_int,
    callback: super::super::private_skylight::NotifyCallback,
    event: u32,
    _: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    if event == 1401 {
        lock(&FAKE_REGISTERED).push((event, callback));
        0
    } else {
        1000
    }
}

unsafe extern "C" fn fake_remove(
    _: std::ffi::c_int,
    _: super::super::private_skylight::NotifyCallback,
    event: u32,
    _: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    lock(&FAKE_REGISTERED).retain(|(held, _)| *held != event);
    0
}

#[test]
fn every_private_candidate_registration_is_reported_and_the_callback_names_its_id() {
    use super::skylight::SPACE_CHANGE_EVENTS;
    use crate::border_overlay::SpaceDiagnostic as D;
    let (hub, mailbox, _) = hub_rig(&[1]);
    // SAFETY: each address is a function with the documented shape of its name.
    let symbols = unsafe {
        SkyLightSymbols::from_lookup(|name: &CStr| match name.to_bytes() {
            b"SLSMainConnectionID" => fake_connection as *mut std::ffi::c_void,
            b"SLSRegisterConnectionNotifyProc" => fake_register as *mut std::ffi::c_void,
            b"SLSRemoveConnectionNotifyProc" => fake_remove as *mut std::ffi::c_void,
            _ => std::ptr::null_mut(),
        })
    };
    let mut notify = SkyLightSpaceNotify::with_symbols(symbols);
    assert_eq!(
        notify.register(Arc::clone(&hub)),
        Ok(()),
        "one accepted candidate is enough"
    );
    assert_eq!(SPACE_CHANGE_EVENTS, [1401, 1400]);
    assert_eq!(mailbox.drain().facts, [
        F::Diagnostic(D::PrivateRegistration { event: 1401, code: Some(0) }),
        F::Diagnostic(D::PrivateRegistration { event: 1400, code: Some(1000) }),
    ]);
    let (event, callback) = lock(&FAKE_REGISTERED)[0];
    // SAFETY: the callback reads no argument.
    unsafe { callback(event, std::ptr::null_mut(), 0, std::ptr::null_mut(), 0) };
    assert_eq!(mailbox.drain().facts, [
        F::Diagnostic(D::PrivateEvent { event: 1401 }),
        F::SpaceChangeBegan { display: display(1) },
    ]);
    notify.unregister();
    assert!(
        lock(&FAKE_REGISTERED).is_empty(),
        "the accepted candidate is removed"
    );
}

/// Dropping the source while its owner is stuck: the stop times out, the
/// observer is told, and the owner is forgotten explicitly (counted), never
/// silently detached with its state freed. Released, the owner exits.
#[test]
fn a_drop_whose_stop_times_out_forgets_the_owner_explicitly_and_reports_it() {
    let mut rig = rig(Duration::from_millis(50));
    rig.start();
    let _ = rig.facts(1);
    rig.control.stuck.store(true, Ordering::SeqCst);
    rig.control.wake();
    std::thread::sleep(Duration::from_millis(20));
    let before = super::forgotten_space_owners();
    let Rig { source, control, outcomes, .. } = rig;
    drop(source);
    assert_eq!(
        lock(&outcomes).last(),
        Some(&StopOutcome::TimedOut {
            owner_joined: false,
            callbacks_drained: true
        })
    );
    assert_eq!(super::forgotten_space_owners(), before + 1);
    control.stuck.store(false, Ordering::SeqCst);
    control.wake();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !control.log().iter().any(|l| l.starts_with("unregister")) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        control.log().iter().any(|l| l.starts_with("unregister")),
        "the owner left"
    );
}
