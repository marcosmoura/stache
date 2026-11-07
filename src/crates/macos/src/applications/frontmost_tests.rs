//! Frontmost observation, publication and preflight tests (HQ-03-01, HQ-03-03).
//!
//! Every test uses [`FakeApplicationBackend`] and [`ManualExecutor`]; nothing
//! observes the real workspace or touches a real application.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use stache_sdk::{CancellationToken, MainThreadExecutor};

use super::frontmost::{
    FrontmostListener, FrontmostPreflight, FrontmostState, PreflightRejection, Unconfirmed,
    classify_preflight,
};
use super::testing::{FakeAppKey, FakeApplicationBackend, ManualExecutor};
use super::*;

const BUDGET: Duration = Duration::from_millis(500);

struct Fixture {
    control: ApplicationControl,
    backend: Arc<FakeApplicationBackend>,
    executor: Arc<ManualExecutor>,
    observation: ApplicationObservation,
}

fn control(
    budget: Duration,
) -> (
    ApplicationControl,
    Arc<FakeApplicationBackend>,
    Arc<ManualExecutor>,
) {
    let executor = Arc::new(ManualExecutor::new(true));
    let backend = Arc::new(FakeApplicationBackend::new());
    // Activation refreshes are always queued, never run inline, like the
    // system backend's main-queue deferral.
    backend.defer_launches(Arc::clone(&executor));
    let control = ApplicationControl::with_acknowledgement_budget(
        Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
        Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
        budget,
    );
    (control, backend, executor)
}

fn fixture() -> Fixture {
    let (control, backend, executor) = control(BUDGET);
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    Fixture {
        control,
        backend,
        executor,
        observation,
    }
}

fn id_of(fixture: &Fixture, bundle: &str) -> ApplicationId {
    fixture.observation.running_applications(bundle).expect("running")[0]
}

fn watch(fixture: &Fixture) -> FrontmostWatch {
    fixture.observation.watch_frontmost(&CancellationToken::new()).expect("watch")
}

fn app(fixture: &Fixture, bundle: &str, pid: i32) -> FakeAppKey {
    fixture.backend.add_running_with_pid(bundle, pid)
}

#[test]
fn the_initial_query_publishes_the_exact_confirmed_frontmost_instance() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    f.backend.set_name(a, Some("Alpha"));
    f.backend.set_frontmost(Some(a));
    let watch = watch(&f);
    let snapshot = watch.snapshot();
    assert_eq!(snapshot.state, FrontmostState::Confirmed);
    assert_eq!(snapshot.application, Some(id_of(&f, "com.example.a")));
    assert_eq!(snapshot.bundle_identifier.as_deref(), Some("com.example.a"));
    assert_eq!(snapshot.name.as_deref(), Some("Alpha"));
    assert_eq!(snapshot.generation, f.observation.generation());
    assert!(!snapshot.is_exempt());
    assert_eq!(snapshot.eligible_application(), snapshot.application);
    assert!(watch.is_epoch_current(snapshot.epoch));
}

#[test]
fn no_frontmost_application_is_unconfirmed_and_never_eligible() {
    let f = fixture();
    let watch = watch(&f);
    let snapshot = watch.snapshot();
    assert_eq!(
        snapshot.state,
        FrontmostState::Unconfirmed(Unconfirmed::NoFrontmost)
    );
    assert_eq!(snapshot.eligible_application(), None);
    assert!(!snapshot.is_exempt());
}

#[test]
fn an_activation_invalidates_immediately_before_the_deferred_refresh() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    f.backend.set_frontmost(Some(a));
    let watch = watch(&f);
    let before = watch.snapshot();
    assert!(before.is_confirmed());

    assert_eq!(f.backend.activate(b), 1);
    // No refresh ran yet: the old confirmation is already invalid.
    assert_eq!(f.executor.queued(), 1);
    let pending = watch.snapshot();
    assert_eq!(
        pending.state,
        FrontmostState::Unconfirmed(Unconfirmed::Invalidated)
    );
    assert_eq!(pending.eligible_application(), None);
    assert!(!watch.is_epoch_current(before.epoch));

    f.executor.drain();
    let after = watch.snapshot();
    assert!(after.is_confirmed());
    assert_eq!(after.application, Some(id_of(&f, "com.example.b")));
    assert!(after.epoch > before.epoch);
}

#[test]
fn a_b_a_invalidates_an_a_press_even_when_the_final_identity_is_a() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    f.backend.set_frontmost(Some(a));
    let watch = watch(&f);
    let first = watch.snapshot();
    let a_id = first.application.expect("a");

    let _ = f.backend.activate(b);
    let _ = f.backend.activate(a);
    assert_eq!(f.executor.queued(), 1, "a burst coalesces to one refresh");
    f.executor.drain();
    let last = watch.snapshot();
    assert!(last.is_confirmed());
    assert_eq!(last.application, Some(a_id), "identity returned to A");
    assert!(last.epoch > first.epoch, "the epoch never repeats");
    assert!(
        !watch.is_epoch_current(first.epoch),
        "the first A press is invalid"
    );
    assert!(watch.is_epoch_current(last.epoch));
}

#[test]
fn a_rejected_refresh_enqueue_stays_unconfirmed_and_recovers_on_the_next_activation() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    f.backend.set_frontmost(Some(a));
    let watch = watch(&f);

    f.executor.reject(true);
    let _ = f.backend.activate(b);
    f.executor.reject(false);
    assert_eq!(f.executor.queued(), 0, "the enqueue was rejected");
    assert_eq!(
        watch.snapshot().state,
        FrontmostState::Unconfirmed(Unconfirmed::Invalidated),
        "a failed delivery is never an indefinitely trusted identity"
    );

    // The coalescing flag was released, so the next activation enqueues again.
    let _ = f.backend.activate(b);
    assert_eq!(f.executor.queued(), 1);
    f.executor.drain();
    assert!(watch.snapshot().is_confirmed());
}

#[test]
fn an_explicit_refresh_recovers_an_unconfirmed_state() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    f.backend.set_frontmost(Some(a));
    let watch = watch(&f);
    f.executor.reject(true);
    let _ = f.backend.activate(b);
    f.executor.reject(false);
    assert!(!watch.snapshot().is_confirmed());
    let recovered = watch.refresh().expect("refresh");
    assert!(recovered.is_confirmed());
    assert_eq!(recovered.application, Some(id_of(&f, "com.example.b")));
}

#[test]
fn a_failed_refresh_query_is_unconfirmed() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    f.backend.set_frontmost(Some(a));
    let watch = watch(&f);
    f.backend.fail_next_frontmost();
    let snapshot = watch.refresh().expect("refresh runs");
    assert_eq!(
        snapshot.state,
        FrontmostState::Unconfirmed(Unconfirmed::QueryFailed)
    );
}

#[test]
fn a_dropped_notification_is_the_documented_unobservable_interval() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    f.backend.set_frontmost(Some(a));
    let watch = watch(&f);
    let a_id = id_of(&f, "com.example.a");

    // The OS switched focus and the notification was lost (or not yet
    // ingested): the published cell still says A. That cannot be known here.
    f.backend.set_frontmost(Some(b));
    f.backend.drop_next_activation_delivery();
    assert_eq!(f.backend.deliver_activation(), 0);
    assert_eq!(watch.snapshot().application, Some(a_id));

    // The native preflight never trusts that cache.
    let preflight = f.observation.preflight_frontmost(a_id).expect("preflight");
    assert_eq!(
        preflight,
        FrontmostPreflight::Rejected(PreflightRejection::NotFrontmost)
    );
}

#[test]
fn a_query_overlapping_install_never_overwrites_a_newer_activation() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    f.backend.set_frontmost(Some(a));
    let barrier = f.backend.block_next_frontmost();
    let observation = Arc::new(f.observation);
    let installer = {
        let observation = Arc::clone(&observation);
        std::thread::spawn(move || observation.watch_frontmost(&CancellationToken::new()))
    };
    assert!(
        barrier.wait_entered(Duration::from_secs(2)),
        "the initial query is in flight"
    );

    // Observation is already live; B activates while the old answer (A) is held.
    assert_eq!(f.backend.activate(b), 1);
    let _ = barrier.release();
    let watch = installer.join().expect("join").expect("watch");

    let stale = watch.snapshot();
    assert_eq!(
        stale.state,
        FrontmostState::Unconfirmed(Unconfirmed::Invalidated),
        "the stale A answer must not be published"
    );
    f.executor.drain();
    let snapshot = watch.snapshot();
    assert!(snapshot.is_confirmed());
    assert_eq!(snapshot.bundle_identifier.as_deref(), Some("com.example.b"));
}

#[test]
fn a_deferred_refresh_after_stop_is_inert() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    f.backend.set_frontmost(Some(a));
    let watch = watch(&f);
    let _ = f.backend.activate(b);
    let queries = f.backend.frontmost_queries();
    f.observation.stop().expect("stop");
    assert_eq!(f.backend.activation_observers(), 0);
    f.executor.drain();
    assert_eq!(
        f.backend.frontmost_queries(),
        queries,
        "no native query after stop"
    );
    assert_eq!(
        watch.snapshot().state,
        FrontmostState::Unconfirmed(Unconfirmed::Inactive)
    );
    assert!(!watch.is_epoch_current(watch.epoch()));
    assert!(matches!(watch.refresh(), Err(ApplicationError::NotInstalled)));
}

#[test]
fn a_stale_refresh_cannot_overwrite_a_successor_generation() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    f.backend.set_frontmost(Some(a));
    let old_watch = watch(&f);
    let _ = f.backend.activate(b);
    f.observation.stop().expect("stop");

    let successor = f.control.install(&CancellationToken::new(), |_| {}).expect("successor");
    let new_watch = successor.watch_frontmost(&CancellationToken::new()).expect("watch");
    let before = new_watch.snapshot();
    assert!(before.is_confirmed());
    assert_ne!(before.generation, old_watch.snapshot().generation);

    f.executor.drain();
    assert_eq!(new_watch.snapshot(), before, "the old refresh is inert");
}

#[test]
fn unknown_metadata_needed_for_an_exemption_is_unconfirmed() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    f.backend.forget_bundle_id(a);
    f.backend.set_frontmost(Some(a));
    let watch = watch(&f);
    let snapshot = watch.snapshot();
    assert_eq!(
        snapshot.state,
        FrontmostState::Unconfirmed(Unconfirmed::UnknownMetadata)
    );
    assert_eq!(snapshot.eligible_application(), None);
}

#[test]
fn finder_and_the_exact_current_instance_are_exempt() {
    let f = fixture();
    let finder = app(&f, FINDER_BUNDLE_IDENTIFIER, 1);
    f.backend.set_frontmost(Some(finder));
    let watch = watch(&f);
    let snapshot = watch.snapshot();
    assert!(snapshot.is_confirmed() && snapshot.is_finder && snapshot.is_exempt());
    assert_eq!(snapshot.eligible_application(), None);

    // Self is the exact retained instance, not a bundle identifier match: a
    // second instance with the same bundle is not this process.
    let stache = app(&f, "com.example.stache", 300);
    let impostor = app(&f, "com.example.stache", 301);
    f.backend.set_current_process(Some(stache));
    let _ = f.backend.activate(stache);
    f.executor.drain();
    let own = watch.snapshot();
    assert!(own.is_current_process && own.is_exempt());
    assert_eq!(own.eligible_application(), None);

    let _ = f.backend.activate(impostor);
    f.executor.drain();
    let other = watch.snapshot();
    assert!(!other.is_current_process && !other.is_exempt());
    assert!(
        other.eligible_application().is_some(),
        "same bundle, different instance"
    );
}

#[test]
fn the_input_path_read_is_nonblocking_and_makes_no_native_query() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    f.backend.set_frontmost(Some(a));
    let watch = watch(&f);
    let queries = f.backend.frontmost_queries();
    for _ in 0..50 {
        assert!(watch.snapshot().is_confirmed());
    }
    assert_eq!(f.backend.frontmost_queries(), queries);

    // A held publication cell reports Busy instead of waiting (this thread would
    // deadlock if the read blocked).
    let held = watch.hold_publication();
    assert_eq!(
        watch.snapshot().state,
        FrontmostState::Unconfirmed(Unconfirmed::Busy)
    );
    drop(held);
    assert!(watch.snapshot().is_confirmed());
}

#[test]
fn will_launch_observation_is_unchanged_by_a_watch() {
    let (control, backend, executor) = control(BUDGET);
    let launches = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&launches);
    let observation = control
        .install(&CancellationToken::new(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("install");
    let a = backend.add_running_with_pid("com.example.a", 100);
    backend.set_frontmost(Some(a));
    let watch = observation.watch_frontmost(&CancellationToken::new()).expect("watch");
    // Activation never reaches the will-launch sink.
    let _ = backend.activate(a);
    executor.drain();
    assert_eq!(launches.load(Ordering::SeqCst), 0);
    assert!(watch.snapshot().is_confirmed());
    // A launch still does, through its own deferral contract.
    let newcomer = backend.add_running_with_pid("com.example.c", 400);
    assert_eq!(backend.launch(newcomer), 1);
    executor.drain();
    assert_eq!(launches.load(Ordering::SeqCst), 1);
}

#[test]
fn a_second_live_watch_is_refused() {
    let f = fixture();
    let first = watch(&f);
    let second = f.observation.watch_frontmost(&CancellationToken::new());
    assert_eq!(second.expect_err("already"), ApplicationError::AlreadyInstalled);
    assert_eq!(f.backend.activation_observers(), 1);
    drop(first);
}

#[test]
fn a_failed_install_is_inert_and_a_retry_registers_exactly_one_observer() {
    let f = fixture();
    f.backend.fail_next_activation_observe();
    let error = f.observation.watch_frontmost(&CancellationToken::new()).expect_err("fails");
    assert!(matches!(error, ApplicationError::ObservationFailed(_)));
    assert_eq!(f.backend.activation_observers(), 0);
    let watch = watch(&f);
    assert_eq!(f.backend.activation_observers(), 1);
    assert!(matches!(
        watch.snapshot().state,
        FrontmostState::Confirmed | FrontmostState::Unconfirmed(Unconfirmed::NoFrontmost)
    ));
}

#[test]
fn a_cancelled_install_registers_nothing_and_stays_inert() {
    let f = fixture();
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        f.observation.watch_frontmost(&token).expect_err("cancelled"),
        ApplicationError::Cancelled
    );
    assert_eq!(f.backend.activation_observers(), 0);
}

#[test]
fn a_rejected_install_dispatch_is_reported_and_leaves_nothing_registered() {
    let f = fixture();
    f.executor.reject(true);
    let error = f.observation.watch_frontmost(&CancellationToken::new()).expect_err("rejected");
    f.executor.reject(false);
    assert!(matches!(error, ApplicationError::Dispatch(_)));
    assert_eq!(f.backend.activation_observers(), 0);
    let _ = watch(&f);
}

#[test]
fn removal_blocked_on_main_times_out_with_inert_ownership_then_cleans_up_once() {
    let (control, backend, executor) = control(Duration::from_millis(40));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let a = backend.add_running_with_pid("com.example.a", 100);
    backend.set_frontmost(Some(a));
    let watch = observation.watch_frontmost(&CancellationToken::new()).expect("watch");
    assert!(watch.snapshot().is_confirmed());

    // Main is blocked: the removal job queues and the stop times out.
    executor.set_on_main(false);
    assert_eq!(
        observation.stop().expect_err("blocked"),
        ApplicationError::TimedOut
    );
    assert_eq!(backend.activation_observers(), 1, "ownership is retained");
    assert_eq!(
        watch.snapshot().state,
        FrontmostState::Unconfirmed(Unconfirmed::Inactive),
        "logically inert before native cleanup"
    );
    let _ = backend.activate(a);
    assert_eq!(executor.queued(), 1, "only the removal job is queued");

    executor.set_on_main(true);
    executor.drain();
    assert_eq!(backend.activation_observers(), 0);
    assert_eq!(backend.activation_removals(), 1);
    observation.stop().expect("retry");
    drop(observation);
    assert_eq!(backend.activation_removals(), 1, "cleanup happened exactly once");
}

#[test]
fn a_failed_activation_removal_keeps_ownership_until_a_retry() {
    let f = fixture();
    let _ = watch(&f);
    f.backend.fail_next_unobserve();
    assert!(matches!(
        f.observation.stop(),
        Err(ApplicationError::RemovalFailed(_))
    ));
    assert_eq!(f.backend.activation_observers(), 1);
    f.observation.stop().expect("retry");
    assert_eq!(f.backend.activation_observers(), 0);
    assert_eq!(f.backend.activation_removals(), 1);
}

// ----- preflight (HQ-03-03) -------------------------------------------------

#[test]
fn preflight_accepts_only_the_running_frontmost_nonexempt_exact_instance() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    f.backend.set_name(a, Some("Alpha"));
    f.backend.set_frontmost(Some(a));
    let a_id = id_of(&f, "com.example.a");
    match f.observation.preflight_frontmost(a_id).expect("preflight") {
        FrontmostPreflight::Eligible(front) => {
            assert_eq!(front.id, a_id);
            assert_eq!(front.name.as_deref(), Some("Alpha"));
        }
        other => panic!("expected eligible, got {other:?}"),
    }
}

#[test]
fn preflight_sees_finder_self_and_other_apps_before_any_notification_refresh() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    let finder = app(&f, FINDER_BUNDLE_IDENTIFIER, 1);
    let stache = app(&f, "com.example.stache", 300);
    f.backend.set_current_process(Some(stache));
    f.backend.set_frontmost(Some(a));
    let watch = watch(&f);
    let a_id = id_of(&f, "com.example.a");
    let stale = watch.snapshot();
    assert_eq!(stale.application, Some(a_id));

    for (front, expected) in [
        (b, PreflightRejection::NotFrontmost),
        (finder, PreflightRejection::Finder),
        (stache, PreflightRejection::CurrentProcess),
    ] {
        // OS focus moved; no notification has been ingested.
        f.backend.set_frontmost(Some(front));
        assert_eq!(watch.snapshot().application, Some(a_id), "cache still says A");
        assert_eq!(
            f.observation.preflight_frontmost(a_id).expect("preflight"),
            FrontmostPreflight::Rejected(expected)
        );
    }
    f.backend.set_frontmost(None);
    assert_eq!(
        f.observation.preflight_frontmost(a_id).expect("preflight"),
        FrontmostPreflight::Rejected(PreflightRejection::NoFrontmost)
    );
}

#[test]
fn preflight_rejects_a_same_bundle_replacement_and_a_reused_pid() {
    let f = fixture();
    let old = app(&f, "com.example.a", 100);
    let a_id = {
        f.backend.set_frontmost(Some(old));
        id_of(&f, "com.example.a")
    };
    f.backend.terminate(old);
    // The replacement has the same bundle identifier and the reused PID.
    let replacement = app(&f, "com.example.a", 100);
    f.backend.set_frontmost(Some(replacement));
    let replacement_id = id_of(&f, "com.example.a");
    assert_ne!(a_id, replacement_id);
    assert_eq!(f.backend.pid(old), f.backend.pid(replacement));
    assert_eq!(
        f.observation.preflight_frontmost(a_id).expect("preflight"),
        FrontmostPreflight::NotRunning,
        "the dead instance is not retargeted to its replacement"
    );
    // A live captured instance is not confused with a same-bundle, same-PID
    // sibling that is frontmost.
    let sibling = app(&f, "com.example.a", 100);
    f.backend.set_frontmost(Some(sibling));
    assert_eq!(
        f.observation.preflight_frontmost(replacement_id).expect("preflight"),
        FrontmostPreflight::Rejected(PreflightRejection::NotFrontmost)
    );
}

#[test]
fn preflight_with_unknown_metadata_for_the_captured_frontmost_is_unconfirmed() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let a_id = id_of(&f, "com.example.a");
    f.backend.forget_bundle_id(a);
    f.backend.set_frontmost(Some(a));
    assert_eq!(
        f.observation.preflight_frontmost(a_id).expect("preflight"),
        FrontmostPreflight::Rejected(PreflightRejection::Unconfirmed)
    );
}

#[test]
fn preflight_for_a_foreign_control_instance_is_not_running() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    f.backend.set_frontmost(Some(a));
    let (other_control, _other_backend, _other_executor) = control(BUDGET);
    let other = other_control.install(&CancellationToken::new(), |_| {}).expect("other");
    assert_eq!(
        other.preflight_frontmost(id_of(&f, "com.example.a")).expect("preflight"),
        FrontmostPreflight::NotRunning
    );
}

#[test]
fn a_preflight_query_failure_is_an_error_not_an_approval() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    f.backend.set_frontmost(Some(a));
    let a_id = id_of(&f, "com.example.a");
    f.backend.fail_next_frontmost();
    assert_eq!(
        f.observation.preflight_frontmost(a_id).expect_err("fails"),
        ApplicationError::Unavailable
    );
}

#[test]
fn classify_preflight_is_a_pure_table() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let a_id = id_of(&f, "com.example.a");
    let make = |id: ApplicationId, bundle: Option<&str>, current: bool| FrontmostApplication {
        id,
        bundle_identifier: bundle.map(str::to_owned),
        name: None,
        is_current_process: current,
        pid: None,
    };
    let _ = a;
    assert_eq!(
        classify_preflight(a_id, false, None),
        FrontmostPreflight::NotRunning
    );
    assert_eq!(
        classify_preflight(a_id, true, None),
        FrontmostPreflight::Rejected(PreflightRejection::NoFrontmost)
    );
    assert!(matches!(
        classify_preflight(a_id, true, Some(&make(a_id, Some("x"), false))),
        FrontmostPreflight::Eligible(_)
    ));
    assert_eq!(
        classify_preflight(
            a_id,
            true,
            Some(&make(a_id, Some(FINDER_BUNDLE_IDENTIFIER), false))
        ),
        FrontmostPreflight::Rejected(PreflightRejection::Finder)
    );
    assert_eq!(
        classify_preflight(a_id, true, Some(&make(a_id, Some("x"), true))),
        FrontmostPreflight::Rejected(PreflightRejection::CurrentProcess)
    );
    assert_eq!(
        classify_preflight(a_id, true, Some(&make(a_id, None, false))),
        FrontmostPreflight::Rejected(PreflightRejection::Unconfirmed)
    );
}

#[test]
fn the_new_system_backend_operations_refuse_off_main_calls() {
    let backend = SystemApplicationBackend;
    let handle = BackendHandle::allocate();
    assert_eq!(
        backend.frontmost(handle).expect_err("off-main"),
        ApplicationError::NotMainThread
    );
    assert_eq!(
        backend.observe_activation(handle, 1, Arc::new(|| None)).expect_err("off-main"),
        ApplicationError::NotMainThread
    );
    assert_eq!(
        backend.unobserve_activation(handle, 1).expect_err("off-main"),
        ApplicationError::NotMainThread
    );
}

#[test]
fn the_native_activation_and_geometry_apis_are_compile_proven() {
    // Taking the items proves the bindings exist and link without calling them.
    // SAFETY: reading an immutable AppKit constant has no side effect.
    let _name: &objc2_foundation::NSString =
        unsafe { objc2_app_kit::NSWorkspaceDidActivateApplicationNotification };
    let read: fn() -> Option<Vec<WindowRecord>> = super::display::read_window_records;
    std::hint::black_box(read);
    let current: fn() -> objc2::rc::Retained<NSRunningApplication> =
        NSRunningApplication::currentApplication;
    std::hint::black_box(current);
}

// ------------------------------------------------------- observed watch

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Invalidated(u64),
    Published(FrontmostState, Option<ApplicationId>, u64),
}

#[derive(Default)]
struct Recording {
    events: std::sync::Mutex<Vec<Event>>,
    watch: std::sync::Mutex<Option<FrontmostWatch>>,
    busy_reads: AtomicUsize,
    panic: std::sync::atomic::AtomicBool,
}

impl Recording {
    fn events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    fn push(&self, event: Event) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event);
    }
}

impl FrontmostListener for Recording {
    fn invalidated(&self, epoch: u64) {
        self.push(Event::Invalidated(epoch));
        assert!(!self.panic.load(Ordering::SeqCst), "listener panic");
    }

    fn published(&self, snapshot: &FrontmostSnapshot) {
        // The lock must be free: a read here must never report `Busy`.
        let watch = self.watch.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        if let Some(watch) = watch
            && watch.snapshot().state == FrontmostState::Unconfirmed(Unconfirmed::Busy)
        {
            self.busy_reads.fetch_add(1, Ordering::SeqCst);
        }
        self.push(Event::Published(
            snapshot.state,
            snapshot.application,
            snapshot.epoch,
        ));
        assert!(!self.panic.load(Ordering::SeqCst), "listener panic");
    }
}

fn observed(f: &Fixture) -> (FrontmostWatch, Arc<Recording>) {
    let recording = Arc::new(Recording::default());
    let watch = f
        .observation
        .watch_frontmost_observed(
            &CancellationToken::new(),
            Arc::clone(&recording) as Arc<dyn FrontmostListener>,
        )
        .expect("observed watch");
    *recording.watch.lock().unwrap() = Some(watch.clone());
    (watch, recording)
}

#[test]
fn the_listener_is_invalidated_before_the_refresh_and_published_after_it() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    f.backend.set_frontmost(Some(a));
    let (watch, listener) = observed(&f);
    // The initial confirmed publish.
    assert_eq!(listener.events(), vec![Event::Published(
        FrontmostState::Confirmed,
        Some(id_of(&f, "com.example.a")),
        watch.epoch()
    )]);

    f.backend.set_frontmost(Some(b));
    let _ = f.backend.activate(b);
    // Invalidated at once, with the new epoch; nothing published yet.
    assert_eq!(listener.events(), vec![Event::Invalidated(watch.epoch())]);
    f.executor.drain();
    assert_eq!(listener.events(), vec![Event::Published(
        FrontmostState::Confirmed,
        Some(id_of(&f, "com.example.b")),
        watch.epoch()
    )]);
    assert_eq!(listener.busy_reads.load(Ordering::SeqCst), 0);
}

#[test]
fn a_failed_refresh_publishes_the_unconfirmed_snapshot() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    f.backend.set_frontmost(Some(a));
    let (watch, listener) = observed(&f);
    let _ = listener.events();
    f.backend.fail_next_frontmost();
    let _ = watch.refresh().expect("refresh runs");
    assert_eq!(listener.events(), vec![Event::Published(
        FrontmostState::Unconfirmed(Unconfirmed::QueryFailed),
        None,
        watch.epoch()
    )]);
}

#[test]
fn a_rejected_refresh_publishes_unconfirmed_at_the_current_epoch() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    f.backend.set_frontmost(Some(a));
    let barrier = f.backend.block_next_frontmost();
    let recording = Arc::new(Recording::default());
    let observation = Arc::new(f.observation);
    let installer = {
        let observation = Arc::clone(&observation);
        let listener = Arc::clone(&recording) as Arc<dyn FrontmostListener>;
        std::thread::spawn(move || {
            observation.watch_frontmost_observed(&CancellationToken::new(), listener)
        })
    };
    assert!(barrier.wait_entered(Duration::from_secs(2)));
    assert_eq!(f.backend.activate(b), 1);
    let _ = barrier.release();
    let watch = installer.join().expect("join").expect("watch");
    // Invalidated by B, then the stale A answer was rejected.
    assert_eq!(recording.events(), vec![
        Event::Invalidated(watch.epoch()),
        Event::Published(
            FrontmostState::Unconfirmed(Unconfirmed::Invalidated),
            None,
            watch.epoch()
        ),
    ]);
}

#[test]
fn stopping_publishes_inactive_exactly_once() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    f.backend.set_frontmost(Some(a));
    let (watch, listener) = observed(&f);
    let _ = listener.events();
    f.observation.stop().expect("stop");
    assert_eq!(listener.events(), vec![Event::Published(
        FrontmostState::Unconfirmed(Unconfirmed::Inactive),
        None,
        watch.epoch()
    )]);
    f.observation.stop().expect("stop again");
    f.executor.drain();
    assert_eq!(listener.events(), vec![]);
}

#[test]
fn a_failed_install_publishes_inactive_once() {
    let f = fixture();
    let recording = Arc::new(Recording::default());
    f.backend.fail_next_activation_observe();
    let result = f.observation.watch_frontmost_observed(
        &CancellationToken::new(),
        Arc::clone(&recording) as Arc<dyn FrontmostListener>,
    );
    assert!(result.is_err());
    let events = recording.events();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        Event::Published(FrontmostState::Unconfirmed(Unconfirmed::Inactive), None, _)
    ));
}

#[test]
fn a_b_a_callbacks_arrive_in_order_and_end_confirmed() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    f.backend.set_frontmost(Some(a));
    let (watch, listener) = observed(&f);
    let _ = listener.events();

    f.backend.set_frontmost(Some(b));
    let _ = f.backend.activate(b);
    let first = watch.epoch();
    f.backend.set_frontmost(Some(a));
    let _ = f.backend.activate(a);
    let second = watch.epoch();
    assert_eq!(listener.events(), vec![
        Event::Invalidated(first),
        Event::Invalidated(second)
    ]);
    f.executor.drain();
    // One coalesced refresh at the latest epoch, identity A again.
    assert_eq!(listener.events(), vec![Event::Published(
        FrontmostState::Confirmed,
        Some(id_of(&f, "com.example.a")),
        second
    )]);
    assert!(second > first);
}

#[test]
fn a_panicking_listener_is_contained() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    let b = app(&f, "com.example.b", 200);
    f.backend.set_frontmost(Some(a));
    let (watch, listener) = observed(&f);
    let _ = listener.events();
    listener.panic.store(true, Ordering::SeqCst);
    f.backend.set_frontmost(Some(b));
    let _ = f.backend.activate(b);
    // The sink still queued its refresh despite the panic.
    assert_eq!(f.executor.queued(), 1);
    f.executor.drain();
    assert!(watch.snapshot().is_confirmed());
    assert_eq!(
        watch.snapshot().bundle_identifier.as_deref(),
        Some("com.example.b")
    );
    let events = listener.events();
    assert!(matches!(events[0], Event::Invalidated(_)));
    assert!(matches!(
        events[1],
        Event::Published(FrontmostState::Confirmed, _, _)
    ));
}

#[test]
fn the_unobserved_watch_is_unchanged_without_a_listener() {
    let f = fixture();
    let a = app(&f, "com.example.a", 100);
    f.backend.set_frontmost(Some(a));
    let watch = watch(&f);
    f.observation.stop().expect("stop");
    assert_eq!(
        watch.snapshot().state,
        FrontmostState::Unconfirmed(Unconfirmed::Inactive)
    );
}
