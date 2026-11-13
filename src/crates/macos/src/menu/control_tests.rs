//! Presenter, invocation and cleanup tests over the fakes (MA-02-05, 06, 07).
//!
//! No real `NSMenu`, AX element or activation is ever created. A thread named
//! `fake-main` stands in for the main thread; barriers park fake AX calls and
//! fake activation at the exact boundary under test.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use stache_sdk::{CancellationToken, MainThreadExecutor};

use super::geometry::{DisplayGeometry, GeometryError};
use super::testing::{FakeAx, FakeClock, FakeValidator, Node, Op};
use super::testing_native::{FakeGeometry, FakeMainThread, FakePresenter};
use super::*;
use crate::applications::{
    AcceptedActionToken, ApplicationBackend, ApplicationControl, ApplicationObservation,
    DisplayRect,
};
use crate::desktop::DisplayId;
use crate::{FakeAppKey, FakeApplicationBackend, ManualExecutor};

const BUDGET: Duration = Duration::from_millis(400);
const TARGET_PID: i32 = 4242;
const BUNDLE: &str = "com.example.menu-target";

struct Authority(AtomicBool);

impl Authority {
    fn new() -> Arc<Self> { Arc::new(Self(AtomicBool::new(true))) }

    fn revoke(&self) { self.0.store(false, Ordering::SeqCst); }
}

impl PresentationAuthority for Authority {
    fn is_current(&self) -> bool { self.0.load(Ordering::SeqCst) }
}

/// A parked fake AX call.
struct Hold {
    entered: Receiver<()>,
    release: mpsc::Sender<()>,
}

impl Hold {
    fn wait_entered(&self) { self.entered.recv_timeout(Duration::from_secs(5)).expect("entered"); }

    fn release(&self) { let _ = self.release.send(()); }
}

/// Parks the first AX call of `op` made after this is installed.
fn hold_on(ax: &FakeAx, op: Op) -> Hold {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let entered_tx = Mutex::new(Some(entered_tx));
    let release_rx = Mutex::new(release_rx);
    ax.set_hook(move |call| {
        if call.op == op {
            let taken = entered_tx.lock().expect("lock").take();
            if let Some(tx) = taken {
                let _ = tx.send(());
                let _ = release_rx.lock().expect("lock").recv_timeout(Duration::from_secs(10));
            }
        }
    });
    Hold { entered, release }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..3000 {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(1));
    }
    panic!("condition never became true");
}

fn single_display() -> GeometrySnapshot {
    let q = DisplayRect::new(0.0, 0.0, 1440.0, 900.0);
    GeometrySnapshot {
        primary: DisplayId::new(1),
        displays: vec![DisplayGeometry {
            id: DisplayId::new(1),
            quartz: q,
            appkit: DisplayRect::new(0.0, 0.0, 1440.0, 900.0),
            backing_scale: 2.0,
        }],
    }
}

fn find_token(items: &[MenuItemSnapshot], title: &str) -> Option<ActionToken> {
    for item in items {
        if item.title == title && item.action.is_some() {
            return item.action;
        }
        if let MenuItemKind::Submenu(sub) = &item.kind
            && let Some(token) = find_token(&sub.items, title)
        {
            return Some(token);
        }
    }
    None
}

struct Rig {
    control: Arc<MenuControl>,
    main: Arc<FakeMainThread>,
    presenter: Arc<FakePresenter>,
    geometry: Arc<FakeGeometry>,
    ax: FakeAx,
    owner: Arc<AxOwner>,
    clock: Arc<FakeClock>,
    backend: Arc<FakeApplicationBackend>,
    observation: Arc<ApplicationObservation>,
    _application_control: ApplicationControl,
    key: FakeAppKey,
    target: MenuTarget,
    accepted: AcceptedActionToken,
    authority: Arc<Authority>,
    events: Receiver<MenuEvent>,
}

impl Rig {
    fn new() -> Self {
        let apple = Node::submenu("Apple", vec![Node::item("About")]);
        let file = Node::submenu("File", vec![Node::item("Open"), Node::item("Close")]);
        let ax = FakeAx::new(vec![apple, file, Node::item("Solo")]);
        let clock = Arc::new(FakeClock::default());
        let owner = Arc::new(AxOwner::new(
            {
                let ax = ax.clone();
                move || ax
            },
            OwnerConfig {
                clock: Arc::clone(&clock) as Arc<dyn MenuClock>,
                validator: Arc::new(FakeValidator::always(TargetCheck::Current)),
                own_pid: 1,
            },
        ));
        let backend = Arc::new(FakeApplicationBackend::new());
        let application_control = ApplicationControl::with_acknowledgement_budget(
            Arc::new(ManualExecutor::new(true)) as Arc<dyn MainThreadExecutor>,
            Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
            BUDGET,
        );
        let observation = Arc::new(
            application_control.install(&CancellationToken::new(), |_| {}).expect("install"),
        );
        let key = backend.add_running_with_pid(BUNDLE, TARGET_PID);
        let application = observation.running_applications(BUNDLE).expect("running")[0];
        let accepted = observation.accept().expect("accept");
        let main = FakeMainThread::start();
        let presenter = FakePresenter::new(Arc::clone(&main));
        let geometry = FakeGeometry::new(single_display());
        let control = Arc::new(MenuControl::new(
            Arc::clone(&owner),
            Arc::clone(&presenter) as Arc<dyn PresenterBackend>,
            Arc::clone(&geometry) as Arc<dyn GeometrySource>,
            Arc::clone(&main) as Arc<dyn MainDispatcher>,
            Arc::clone(&clock) as Arc<dyn MenuClock>,
            ControlConfig { budget: BUDGET },
        ));
        let events = control.take_events().expect("events");
        Self {
            control,
            main,
            presenter,
            geometry,
            ax,
            owner,
            clock,
            backend,
            observation,
            _application_control: application_control,
            key,
            target: MenuTarget { pid: TARGET_PID, application },
            accepted,
            authority: Authority::new(),
            events,
        }
    }

    fn deadline(&self) -> Instant { self.clock.now() + Duration::from_secs(10) }

    fn read(&self) -> MenuSnapshot {
        let presentation = PresentationId::allocate();
        let (tx, rx) = mpsc::channel();
        self.owner
            .submit_read(
                ReadRequest {
                    target: Some(self.target),
                    presentation,
                    cancel: CancellationToken::new(),
                    deadline: None,
                },
                move |outcome| {
                    let _ = tx.send(outcome);
                },
            )
            .expect("queued");
        match rx.recv_timeout(Duration::from_secs(5)).expect("reply") {
            MenuReadOutcome::Snapshot(snapshot) => snapshot,
            other => panic!("read failed: {other:?}"),
        }
    }

    fn request(&self, snapshot: MenuSnapshot, anchor: QuartzPoint) -> ShowRequest {
        ShowRequest {
            snapshot,
            anchor,
            expected_geometry: None,
            target: self.target,
            accepted: self.accepted,
            authority: Arc::clone(&self.authority) as Arc<dyn PresentationAuthority>,
            deadline: self.deadline(),
        }
    }

    fn show_at(&self, snapshot: MenuSnapshot, anchor: QuartzPoint) -> MenuPresentationOutcome {
        self.control.show(self.request(snapshot, anchor))
    }

    /// Reads and shows a menu; returns the snapshot.
    fn shown(&self) -> MenuSnapshot {
        let snapshot = self.read();
        let outcome = self.show_at(snapshot.clone(), QuartzPoint { x: 100.0, y: 100.0 });
        assert!(
            matches!(outcome, MenuPresentationOutcome::Shown { .. }),
            "{outcome:?}"
        );
        wait_until(|| self.presenter.is_tracking(snapshot.presentation));
        snapshot
    }

    fn next_event(&self) -> MenuEvent {
        self.events.recv_timeout(Duration::from_secs(5)).expect("event")
    }

    fn select(&self, snapshot: &MenuSnapshot, title: &str) -> Selection {
        let token = find_token(&snapshot.items, title).expect("token");
        self.presenter.choose(snapshot.presentation, token);
        loop {
            match self.next_event() {
                MenuEvent::Selected(selection) => return selection,
                MenuEvent::Closed(_) => {}
            }
        }
    }

    fn invoke(&self, selection: &Selection) -> MenuInvocationReport {
        self.control.invoke(selection, &self.observation, self.deadline())
    }

    fn spawn_invoke(&self, selection: &Selection) -> JoinHandle<MenuInvocationReport> {
        let control = Arc::clone(&self.control);
        let observation = Arc::clone(&self.observation);
        let selection = selection.clone();
        let deadline = self.deadline();
        thread::spawn(move || control.invoke(&selection, &observation, deadline))
    }

    fn presses(&self) -> usize { self.ax.ops(Op::Press).len() }

    fn activations(&self) -> usize { self.backend.exact_activation_requests().len() }
}

impl Drop for Rig {
    fn drop(&mut self) { self.main.shutdown(); }
}

struct Foreign;

impl InvocationAuthority for Foreign {
    fn is_current(&self) -> bool { true }

    fn contains(&self, _token: ActionToken) -> bool { false }
}

// ---- Show, geometry, dispatch ------------------------------------------------------

#[test]
fn show_checks_geometry_on_main_and_never_builds_native_state_off_main() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    assert_eq!(rig.geometry.reads(), 1);
    assert_eq!(rig.presenter.off_main_calls(), 0, "presenter state is main-only");
    assert_eq!(rig.presenter.prepared(), vec![snapshot.presentation]);
    let plan = rig.presenter.plan(snapshot.presentation).expect("plan");
    assert_eq!(plan.tokens.len(), 3, "Open, Close and Solo carry actions");
}

#[test]
fn an_anchor_outside_every_display_is_rejected_without_a_menu() {
    let rig = Rig::new();
    let snapshot = rig.read();
    let outcome = rig.show_at(snapshot, QuartzPoint { x: 99_999.0, y: 5.0 });
    assert_eq!(
        outcome,
        MenuPresentationOutcome::GeometryRejected(GeometryError::OutsideDesktop)
    );
    assert!(rig.presenter.prepared().is_empty());
}

#[test]
fn geometry_replaced_between_capture_and_show_is_rejected_on_main() {
    let rig = Rig::new();
    let snapshot = rig.read();
    let mut request = rig.request(snapshot, QuartzPoint { x: 10.0, y: 10.0 });
    let mut replaced = single_display();
    replaced.displays[0].appkit.width = 1920.0;
    replaced.displays[0].quartz.width = 1920.0;
    request.expected_geometry = Some(replaced);
    assert_eq!(
        rig.control.show(request),
        MenuPresentationOutcome::GeometryRejected(GeometryError::Changed)
    );
    rig.geometry.set(Err(GeometryError::Unavailable));
    let again = rig.read();
    assert_eq!(
        rig.show_at(again, QuartzPoint { x: 10.0, y: 10.0 }),
        MenuPresentationOutcome::GeometryRejected(GeometryError::Unavailable)
    );
}

#[test]
fn stale_authority_expired_deadline_and_busy_are_distinct_outcomes() {
    let rig = Rig::new();
    let first = rig.read();
    let at = QuartzPoint { x: 10.0, y: 10.0 };
    rig.authority.revoke();
    assert_eq!(rig.show_at(first, at), MenuPresentationOutcome::Stale);
    let rig = Rig::new();
    let snapshot = rig.read();
    let mut request = rig.request(snapshot, at);
    request.deadline = rig.clock.now();
    assert_eq!(rig.control.show(request), MenuPresentationOutcome::Expired);
    let open = rig.shown();
    let second = rig.read();
    assert_eq!(rig.show_at(second, at), MenuPresentationOutcome::Busy);
    assert_eq!(
        rig.control.cancel(open.presentation),
        CancelOutcome::Acknowledged
    );
}

#[test]
fn dispatch_rejection_is_typed_for_show_cancel_and_release() {
    let rig = Rig::new();
    let at = QuartzPoint { x: 10.0, y: 10.0 };
    rig.main.reject(true);
    let snapshot = rig.read();
    assert_eq!(
        rig.show_at(snapshot, at),
        MenuPresentationOutcome::DispatchFailure
    );
    rig.main.reject(false);
    let open = rig.shown();
    rig.main.reject(true);
    assert_eq!(
        rig.control.cancel(open.presentation),
        CancelOutcome::DispatchFailure
    );
    assert_eq!(rig.control.release(open.presentation), CleanupOutcome::Pending {
        presentation: open.presentation,
        error: CleanupError::NativeDispatch
    });
    rig.main.reject(false);
    assert_eq!(
        rig.control.cancel(open.presentation),
        CancelOutcome::Acknowledged
    );
}

#[test]
fn a_stalled_main_abandons_the_queued_show_so_it_can_never_appear_late() {
    let rig = Rig::new();
    let snapshot = rig.read();
    rig.main.pause();
    let outcome = rig.show_at(snapshot, QuartzPoint { x: 10.0, y: 10.0 });
    assert_eq!(outcome, MenuPresentationOutcome::AckTimeout);
    rig.main.resume();
    thread::sleep(Duration::from_millis(60));
    assert!(rig.presenter.prepared().is_empty());
    assert!(rig.presenter.tracked().is_empty());
}

#[test]
fn invalidation_before_the_queued_show_runs_cancels_it() {
    let rig = Rig::new();
    let snapshot = rig.read();
    let id = snapshot.presentation;
    rig.main.pause();
    let control = Arc::clone(&rig.control);
    let request = rig.request(snapshot, QuartzPoint { x: 10.0, y: 10.0 });
    let shower = thread::spawn(move || control.show(request));
    wait_until(|| rig.main.queued() >= 1);
    rig.control.invalidate(id);
    rig.main.resume();
    assert_eq!(shower.join().expect("join"), MenuPresentationOutcome::Cancelled);
    assert!(rig.presenter.prepared().is_empty());
}

#[test]
fn a_stop_that_races_the_show_after_prepare_removes_the_native_menu() {
    let rig = Rig::new();
    let snapshot = rig.read();
    let id = snapshot.presentation;
    let control = Arc::clone(&rig.control);
    rig.presenter.after_prepare(Some(Arc::new(move || control.invalidate(id))));
    let request = rig.request(snapshot, QuartzPoint { x: 10.0, y: 10.0 });
    assert_eq!(rig.control.show(request), MenuPresentationOutcome::Cancelled);
    assert_eq!(rig.presenter.prepared(), vec![id]);
    assert_eq!(rig.presenter.removed(), vec![id], "prepare and remove balance");
    assert!(!rig.presenter.has_menu(id), "no native entry is left behind");
    assert!(rig.presenter.tracked().is_empty());
    assert_eq!(rig.presenter.off_main_calls(), 0);
    rig.presenter.after_prepare(None); // break the control <-> hook cycle
}

// ---- Tracking cancellation and cleanup (MA-02-06) --------------------------------

#[test]
fn cancellation_runs_during_tracking_without_firing_an_action() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let id = snapshot.presentation;
    assert_eq!(rig.control.cancel(id), CancelOutcome::Acknowledged);
    assert!(rig.presenter.cancelled_while_tracking(id));
    assert!(matches!(rig.next_event(), MenuEvent::Closed(closed) if closed == id));
    assert!(rig.events.try_recv().is_err(), "no action fired");
    assert!(matches!(rig.control.release(id), CleanupOutcome::Complete {
        elements: 3
    }));
    assert_eq!(rig.ax.counts().live(), 0);
}

#[test]
fn a_dispatcher_that_does_not_drain_during_tracking_cannot_cancel_it() {
    let rig = Rig::new();
    rig.presenter.drain_during_tracking(false);
    let snapshot = rig.shown();
    let id = snapshot.presentation;
    // The job is queued but does not run while tracking: pending, not success.
    assert_eq!(rig.control.cancel(id), CancelOutcome::Pending);
    assert!(!rig.presenter.cancelled_while_tracking(id));
    assert!(rig.presenter.is_tracking(id));
    // Only after tracking exits does the stale job run, and it is a no-op.
    rig.presenter.dismiss(id);
    assert!(matches!(rig.next_event(), MenuEvent::Closed(_)));
    wait_until(|| rig.main.queued() == 0);
    assert!(!rig.presenter.cancelled_while_tracking(id));
    assert!(matches!(
        rig.control.release(id),
        CleanupOutcome::Complete { .. }
    ));
}

#[test]
fn cleanup_timeout_is_pending_then_a_retry_succeeds() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let id = snapshot.presentation;
    rig.presenter.dismiss(id);
    assert!(matches!(rig.next_event(), MenuEvent::Closed(_)));
    rig.main.pause();
    assert_eq!(rig.control.release(id), CleanupOutcome::Pending {
        presentation: id,
        error: CleanupError::NativeTimeout
    });
    assert!(rig.presenter.has_menu(id), "pending is not success");
    rig.main.resume();
    assert!(matches!(rig.control.release(id), CleanupOutcome::Complete {
        elements: 3
    }));
    assert!(!rig.presenter.has_menu(id));
    assert_eq!(rig.control.release(id), CleanupOutcome::Unknown);
    assert_eq!(rig.ax.counts().live(), 0);
}

#[test]
fn late_cleanup_names_its_presentation_and_cannot_remove_a_successor() {
    let rig = Rig::new();
    let first = rig.shown();
    rig.presenter.dismiss(first.presentation);
    assert!(matches!(rig.next_event(), MenuEvent::Closed(_)));
    rig.main.pause();
    assert!(matches!(
        rig.control.release(first.presentation),
        CleanupOutcome::Pending { .. }
    ));
    rig.main.resume();
    // A successor is shown before the old cleanup is retried.
    let second = rig.shown();
    assert_ne!(first.presentation, second.presentation);
    assert!(matches!(
        rig.control.release(first.presentation),
        CleanupOutcome::Complete { .. }
    ));
    assert_eq!(rig.presenter.removed(), vec![first.presentation]);
    assert!(rig.presenter.has_menu(second.presentation), "successor survives");
    assert!(rig.presenter.is_tracking(second.presentation));
    assert!(
        rig.ax.counts().live() > 0,
        "the successor's retained elements were not released by the old cleanup"
    );
    assert_eq!(
        rig.control.cancel(second.presentation),
        CancelOutcome::Acknowledged
    );
    let _ = rig.control.release(second.presentation);
    assert_eq!(rig.ax.counts().live(), 0);
}

#[test]
fn dropping_the_control_requests_cleanup_best_effort() {
    let rig = Rig::new();
    let control = MenuControl::new(
        Arc::clone(&rig.owner),
        Arc::clone(&rig.presenter) as Arc<dyn PresenterBackend>,
        Arc::clone(&rig.geometry) as Arc<dyn GeometrySource>,
        Arc::clone(&rig.main) as Arc<dyn MainDispatcher>,
        Arc::clone(&rig.clock) as Arc<dyn MenuClock>,
        ControlConfig { budget: BUDGET },
    );
    let snapshot = rig.read();
    let id = snapshot.presentation;
    let shown = control.show(rig.request(snapshot, QuartzPoint { x: 10.0, y: 10.0 }));
    assert!(matches!(shown, MenuPresentationOutcome::Shown { .. }));
    wait_until(|| rig.presenter.is_tracking(id));
    drop(control);
    wait_until(|| !rig.presenter.has_menu(id));
    wait_until(|| rig.ax.counts().live() == 0);
}

// ---- Selection (callbacks) ------------------------------------------------------------

#[test]
fn the_menu_action_only_enqueues_a_token_and_performs_no_ax_work() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let before = rig.ax.call_count();
    let selection = rig.select(&snapshot, "Open");
    assert_eq!(
        selection.token(),
        find_token(&snapshot.items, "Open").expect("token")
    );
    assert_eq!(rig.ax.call_count(), before, "no AX call from the callback");
    assert_eq!(rig.presses(), 0);
    assert_eq!(rig.activations(), 0);
}

#[test]
fn a_second_selection_is_dropped() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let open = find_token(&snapshot.items, "Open").expect("token");
    let close = find_token(&snapshot.items, "Close").expect("token");
    rig.presenter.choose(snapshot.presentation, open);
    rig.presenter.choose(snapshot.presentation, close);
    let mut selected = 0;
    let mut closed = false;
    while !closed {
        match rig.next_event() {
            MenuEvent::Selected(_) => selected += 1,
            MenuEvent::Closed(_) => closed = true,
        }
    }
    thread::sleep(Duration::from_millis(60));
    while let Ok(event) = rig.events.try_recv() {
        if matches!(event, MenuEvent::Selected(_)) {
            selected += 1;
        }
    }
    assert_eq!(selected, 1);
}

#[test]
fn foreign_tokens_are_dropped_at_selection() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let foreign = ActionToken::new(PresentationId::allocate(), 0);
    rig.presenter.choose(snapshot.presentation, foreign);
    assert!(matches!(rig.next_event(), MenuEvent::Closed(_)));
    thread::sleep(Duration::from_millis(40));
    assert!(rig.events.try_recv().is_err());
}

#[test]
fn a_panic_in_the_menu_action_is_contained() {
    let rig = Rig::new();
    rig.presenter.panic_in_sink(true);
    let snapshot = rig.shown();
    let selection = rig.select(&snapshot, "Open");
    assert_eq!(
        selection.token(),
        find_token(&snapshot.items, "Open").expect("token")
    );
    assert!(matches!(rig.next_event(), MenuEvent::Closed(_)));
}

// ---- Invocation (MA-02-05) -----------------------------------------------------------------

#[test]
fn a_selected_action_activates_the_captured_instance_then_presses_once() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let selection = rig.select(&snapshot, "Open");
    let report = rig.invoke(&selection);
    assert_eq!(report.outcome, MenuInvocationOutcome::Pressed);
    assert!(report.activation_issued && report.press_issued);
    assert_eq!(rig.activations(), 1);
    assert_eq!(rig.presses(), 1);
    assert_eq!(
        rig.ax.ops(Op::SetTimeout).last().map(|call| call.node),
        rig.ax.ops(Op::Press).last().map(|call| call.node),
        "the press timeout is set on the pressed element"
    );
}

#[test]
fn an_expired_deadline_sends_nothing() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let selection = rig.select(&snapshot, "Open");
    let deadline = rig.deadline();
    rig.clock.advance(Duration::from_secs(11));
    let report = rig.control.invoke(&selection, &rig.observation, deadline);
    assert_eq!(report.outcome, MenuInvocationOutcome::ExpiredBeforeSend);
    assert!(!report.activation_issued && !report.press_issued);
    assert_eq!(rig.backend.exact_activation_attempts(), 0);
    assert_eq!(rig.presses(), 0);
}

#[test]
fn a_revoked_authority_makes_the_action_stale() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let selection = rig.select(&snapshot, "Open");
    rig.authority.revoke();
    let report = rig.invoke(&selection);
    assert_eq!(report.outcome, MenuInvocationOutcome::Stale);
    assert_eq!(rig.backend.exact_activation_attempts(), 0);
    assert_eq!(rig.presses(), 0);
}

#[test]
fn a_token_the_presentation_does_not_contain_is_stale_before_any_send() {
    let rig = Rig::new();
    let snapshot = rig.read();
    let token = find_token(&snapshot.items, "Open").expect("token");
    let ticket =
        InvocationTicket::new(rig.target.application, rig.accepted, rig.owner.lease(token));
    let context = InvocationContext {
        observation: &rig.observation,
        owner: &rig.owner,
        clock: rig.clock.as_ref(),
        authority: &Foreign,
        budget: BUDGET,
    };
    let report = invoke(&context, &ticket, rig.deadline());
    assert_eq!(report.outcome, MenuInvocationOutcome::Stale);
    assert_eq!(rig.backend.exact_activation_attempts(), 0);
    assert_eq!(rig.presses(), 0);
}

#[test]
fn a_relaunched_target_with_the_same_pid_is_stale_at_action_time() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let selection = rig.select(&snapshot, "Open");
    rig.backend.terminate(rig.key);
    let _replacement = rig.backend.add_running_with_pid(BUNDLE, TARGET_PID);
    let report = rig.invoke(&selection);
    assert_eq!(report.outcome, MenuInvocationOutcome::Stale);
    assert_eq!(rig.backend.exact_activation_attempts(), 0);
    assert_eq!(rig.presses(), 0);
}

#[test]
fn a_refused_activation_does_not_press_and_is_not_retried() {
    let rig = Rig::new();
    rig.backend.refuse_exact_activation();
    let snapshot = rig.shown();
    let selection = rig.select(&snapshot, "Open");
    let report = rig.invoke(&selection);
    assert_eq!(report.outcome, MenuInvocationOutcome::ActivationRefused);
    assert!(report.activation_issued, "a refused send was still issued");
    assert_eq!(rig.backend.exact_activation_attempts(), 1);
    assert_eq!(rig.presses(), 0);
}

#[test]
fn cannot_complete_is_uncertain_and_the_press_is_never_retried() {
    let rig = Rig::new();
    rig.ax.inject(Op::Press, None, AxError::CannotComplete);
    let snapshot = rig.shown();
    let selection = rig.select(&snapshot, "Open");
    let report = rig.invoke(&selection);
    assert_eq!(
        report.outcome,
        MenuInvocationOutcome::Uncertain(UncertainStage::Press)
    );
    assert!(report.activation_issued && report.press_issued);
    assert_eq!(rig.presses(), 1);
    assert_eq!(rig.activations(), 1);
}

#[test]
fn other_press_failures_are_typed() {
    let rig = Rig::new();
    rig.ax.inject(Op::Press, None, AxError::ActionUnsupported);
    let snapshot = rig.shown();
    let selection = rig.select(&snapshot, "Open");
    assert_eq!(
        rig.invoke(&selection).outcome,
        MenuInvocationOutcome::Unsupported(AxError::ActionUnsupported)
    );
}

#[test]
fn cancellation_before_the_activation_preflight_sends_nothing() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let selection = rig.select(&snapshot, "Open");
    let barrier = rig.backend.block_next_activation();
    let join = rig.spawn_invoke(&selection);
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    let report = selection.ticket().cancel();
    assert!(!report.activation_issued && !report.press_issued);
    assert!(barrier.release());
    let result = join.join().expect("join");
    assert_eq!(result.outcome, MenuInvocationOutcome::Cancelled);
    assert!(!result.activation_issued);
    assert_eq!(rig.activations(), 0);
    assert_eq!(rig.presses(), 0);
}

#[test]
fn cancellation_between_activation_and_press_suppresses_press_but_reports_activation() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let selection = rig.select(&snapshot, "Open");
    let barrier = rig.backend.block_next_activation_after_issue();
    let join = rig.spawn_invoke(&selection);
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    let report = selection.ticket().cancel();
    assert!(report.activation_issued, "activation cannot be undone");
    assert!(!report.press_issued);
    assert!(barrier.release());
    let result = join.join().expect("join");
    assert_eq!(result.outcome, MenuInvocationOutcome::Cancelled);
    assert!(result.activation_issued && !result.press_issued);
    assert_eq!(rig.activations(), 1);
    assert_eq!(rig.presses(), 0);
}

#[test]
fn cancellation_before_the_press_send_on_the_owner_suppresses_it() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let selection = rig.select(&snapshot, "Open");
    let hold = hold_on(&rig.ax, Op::SetTimeout);
    let join = rig.spawn_invoke(&selection);
    hold.wait_entered();
    let report = selection.ticket().cancel();
    assert!(report.activation_issued && !report.press_issued);
    hold.release();
    let result = join.join().expect("join");
    assert_eq!(result.outcome, MenuInvocationOutcome::Cancelled);
    assert!(result.activation_issued && !result.press_issued);
    assert_eq!(rig.presses(), 0);
}

// ---- Leases (MA-02-07) -----------------------------------------------------------------------

#[test]
fn a_selection_racing_close_keeps_its_element_until_the_press_resolves() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let id = snapshot.presentation;
    let selection = rig.select(&snapshot, "Open");
    // Tracking closed; cleanup runs before the invocation.
    let cleanup = rig.control.release(id);
    assert_eq!(cleanup, CleanupOutcome::LeasedPending { released: 2, leased: 1 });
    assert_eq!(rig.ax.counts().live(), 1, "only the leased element survives");
    let report = rig.invoke(&selection);
    assert_eq!(report.outcome, MenuInvocationOutcome::Pressed);
    assert_eq!(rig.presses(), 1);
    wait_until(|| rig.ax.counts().live() == 0);
    let counts = rig.ax.counts();
    assert_eq!(counts.created, counts.released, "released exactly once");
}

#[test]
fn stop_before_the_press_releases_the_leased_element_exactly_once_without_pressing() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let id = snapshot.presentation;
    let selection = rig.select(&snapshot, "Open");
    rig.dismiss_ok(id);
    assert_eq!(rig.control.cancel(id), CancelOutcome::NotTracking);
    assert!(selection.ticket().is_cancelled());
    assert!(matches!(
        rig.control.release(id),
        CleanupOutcome::LeasedPending { leased: 1, .. }
    ));
    let report = rig.invoke(&selection);
    assert_eq!(report.outcome, MenuInvocationOutcome::Cancelled);
    wait_until(|| rig.ax.counts().live() == 0);
    assert_eq!(rig.presses(), 0);
    assert_eq!(rig.activations(), 0);
}

#[test]
fn stop_while_the_press_send_is_held_invalidates_immediately_and_releases_later() {
    let rig = Rig::new();
    let snapshot = rig.shown();
    let id = snapshot.presentation;
    let selection = rig.select(&snapshot, "Open");
    let hold = hold_on(&rig.ax, Op::Press);
    let join = rig.spawn_invoke(&selection);
    hold.wait_entered();

    // Immediate invalidation: never waits on the held send.
    let started = Instant::now();
    let report = selection.ticket().cancel();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(report.press_issued && report.activation_issued);
    rig.control.invalidate(id);

    // The owner is stalled inside the send: AX cleanup is pending, not success.
    assert_eq!(rig.control.release(id), CleanupOutcome::Pending {
        presentation: id,
        error: CleanupError::AxTimeout
    });

    // The invocation's own hop times out after an issued send: uncertain.
    let result = join.join().expect("join");
    assert_eq!(
        result.outcome,
        MenuInvocationOutcome::Uncertain(UncertainStage::Press)
    );
    assert!(result.press_issued);

    // The late press resolves; the retained element is released exactly once.
    hold.release();
    wait_until(|| rig.ax.counts().live() == 0);
    assert_eq!(rig.presses(), 1, "never retried");
    let retry = rig.control.release(id);
    assert!(matches!(retry, CleanupOutcome::Complete { .. }), "{retry:?}");
    let counts = rig.ax.counts();
    assert_eq!(counts.created, counts.released);
}

impl Rig {
    /// Dismisses the open menu and waits for the close event.
    fn dismiss_ok(&self, id: PresentationId) {
        self.presenter.dismiss(id);
        wait_until(|| !self.presenter.is_tracking(id));
    }
}
