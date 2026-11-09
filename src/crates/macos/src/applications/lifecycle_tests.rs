//! Lifecycle notification tests (terminate, hide, unhide, launch).
//!
//! The fake delivers the same pure events through the same deferral contract
//! as the system backend. No workspace notification center is touched.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use stache_sdk::{CancellationToken, MainThreadExecutor};

use super::lifecycle::diagnostic_pid;
use super::testing::{FakeApplicationBackend, ManualExecutor};
use super::*;

const BUDGET: Duration = Duration::from_millis(500);

struct Fixture {
    backend: Arc<FakeApplicationBackend>,
    observation: ApplicationObservation,
    control: ApplicationControl,
    events: Arc<Mutex<Vec<LifecycleEvent>>>,
}

fn fixture() -> Fixture {
    let executor = Arc::new(ManualExecutor::new(true));
    let backend = Arc::new(FakeApplicationBackend::new());
    let control = ApplicationControl::with_acknowledgement_budget(
        Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
        Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
        BUDGET,
    );
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    Fixture {
        backend,
        observation,
        control,
        events: Arc::new(Mutex::new(Vec::new())),
    }
}

fn watch(f: &Fixture) {
    let events = Arc::clone(&f.events);
    f.observation
        .watch_lifecycle(&CancellationToken::new(), move |event| {
            events.lock().expect("events").push(event);
        })
        .expect("watch");
}

fn events(f: &Fixture) -> Vec<LifecycleEvent> { f.events.lock().expect("events").clone() }

#[test]
fn terminate_hide_unhide_and_launch_deliver_the_exact_id() {
    let f = fixture();
    let key = f.backend.add_running_with_pid("com.example.a", 321);
    let id = f.observation.running_applications("com.example.a").expect("running")[0];
    watch(&f);
    for kind in LifecycleKind::ALL {
        assert_eq!(f.backend.deliver_lifecycle(kind, key), 1);
    }
    let seen = events(&f);
    assert_eq!(
        seen.iter().map(|event| event.kind).collect::<Vec<_>>(),
        LifecycleKind::ALL
    );
    assert!(seen.iter().all(|event| event.application == id && event.pid == Some(321)));
}

#[test]
fn a_launch_hide_or_unhide_interns_a_new_instance_and_terminate_does_not() {
    let f = fixture();
    watch(&f);
    let unseen = f.backend.add_running_with_pid("com.example.never-seen", 5);
    // Terminate for an instance the control never interned has no id: the
    // consumer is told explicitly instead of the event vanishing.
    assert_eq!(f.backend.deliver_lifecycle(LifecycleKind::Terminated, unseen), 1);
    let first = events(&f);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].kind, LifecycleKind::TerminatedUnresolved);
    assert_eq!(first[0].pid, Some(5));
    // Hide interns it; the same id then names its termination.
    assert_eq!(f.backend.deliver_lifecycle(LifecycleKind::Hidden, unseen), 1);
    f.backend.terminate(unseen);
    assert_eq!(f.backend.deliver_lifecycle(LifecycleKind::Terminated, unseen), 1);
    let seen = events(&f);
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[1].kind, LifecycleKind::Hidden);
    assert_eq!(seen[2].kind, LifecycleKind::Terminated);
    assert_eq!(seen[1].application, seen[2].application);
    assert!(!f.observation.is_running(seen[2].application).expect("query"));
}

#[test]
fn a_terminate_after_another_intern_pruned_it_still_names_the_original_id() {
    let f = fixture();
    let a = f.backend.add_running_with_pid("com.example.a", 10);
    let b = f.backend.add_running_with_pid("com.example.b", 11);
    let a_id = f.observation.running_applications("com.example.a").expect("running")[0];
    watch(&f);
    f.backend.terminate(a);
    // Another application's launch interns first and prunes the terminated one.
    assert_eq!(f.backend.deliver_lifecycle(LifecycleKind::Launched, b), 1);
    assert_eq!(f.backend.deliver_lifecycle(LifecycleKind::Terminated, a), 1);
    let seen = events(&f);
    assert_eq!(seen[1].kind, LifecycleKind::Terminated);
    assert_eq!(seen[1].application, a_id);
    // The tombstone is cleared on delivery: a repeat is unresolved.
    assert_eq!(f.backend.deliver_lifecycle(LifecycleKind::Terminated, a), 1);
    assert_eq!(events(&f)[2].kind, LifecycleKind::TerminatedUnresolved);
}

#[test]
fn an_evicted_tombstone_is_an_explicit_unresolved_terminate() {
    let f = fixture();
    let keys: Vec<_> = (0..70)
        .map(|index| f.backend.add_running_with_pid(&format!("com.example.n{index}"), 100 + index))
        .collect();
    watch(&f);
    for key in &keys {
        let _ = f.backend.deliver_lifecycle(LifecycleKind::Hidden, *key);
    }
    for key in &keys {
        f.backend.terminate(*key);
    }
    let survivor = f.backend.add_running_with_pid("com.example.survivor", 999);
    // One intern prunes all 70 into a 64-entry tombstone list: the oldest six
    // are evicted.
    let _ = f.backend.deliver_lifecycle(LifecycleKind::Launched, survivor);
    let _ = f.backend.deliver_lifecycle(LifecycleKind::Terminated, keys[0]);
    let _ = f.backend.deliver_lifecycle(LifecycleKind::Terminated, keys[69]);
    let seen = events(&f);
    let tail = &seen[seen.len() - 2..];
    assert_eq!(tail[0].kind, LifecycleKind::TerminatedUnresolved);
    assert_eq!(tail[1].kind, LifecycleKind::Terminated);
}

#[test]
fn pid_reuse_by_a_replacement_is_a_different_instance() {
    let f = fixture();
    let old = f.backend.add_running_with_pid("com.example.a", 100);
    let old_id = f.observation.running_applications("com.example.a").expect("running")[0];
    watch(&f);
    f.backend.terminate(old);
    let _ = f.backend.deliver_lifecycle(LifecycleKind::Terminated, old);
    let replacement = f.backend.add_running_with_pid("com.example.a", 100);
    let _ = f.backend.deliver_lifecycle(LifecycleKind::Launched, replacement);
    let seen = events(&f);
    assert_eq!(seen[0].application, old_id);
    assert_eq!(seen[1].pid, Some(100), "the pid is the same");
    assert_ne!(seen[1].application, old_id, "the identity is not");
}

#[test]
fn an_unknown_pid_is_diagnostic_none_never_identity() {
    assert_eq!(diagnostic_pid(0), None);
    assert_eq!(diagnostic_pid(-1), None);
    assert_eq!(diagnostic_pid(i32::MIN), None);
    assert_eq!(diagnostic_pid(42), Some(42));
    let f = fixture();
    let key = f.backend.add_running_with_pid("com.example.a", 0);
    watch(&f);
    let _ = f.backend.deliver_lifecycle(LifecycleKind::Hidden, key);
    let seen = events(&f);
    assert_eq!(seen[0].pid, None);
    assert_eq!(seen.len(), 1, "an unknown pid still delivers the exact id");
}

#[test]
fn delivery_is_deferred_never_inline_and_a_rejected_enqueue_drops_it() {
    let f = fixture();
    let queue = Arc::new(ManualExecutor::new(false));
    f.backend.defer_launches(Arc::clone(&queue));
    let key = f.backend.add_running_with_pid("com.example.a", 7);
    watch(&f);
    assert_eq!(f.backend.deliver_lifecycle(LifecycleKind::Hidden, key), 1);
    assert!(events(&f).is_empty(), "nothing ran inside the native callback");
    assert_eq!(queue.queued(), 1);
    assert_eq!(queue.drain(), 1);
    assert_eq!(events(&f).len(), 1);

    queue.reject(true);
    assert_eq!(f.backend.deliver_lifecycle(LifecycleKind::Unhidden, key), 1);
    queue.reject(false);
    assert_eq!(queue.drain(), 0);
    assert_eq!(
        events(&f).len(),
        1,
        "a rejected enqueue is dropped, not run inline"
    );
}

#[test]
fn a_stopped_or_superseded_generation_receives_nothing() {
    let f = fixture();
    let queue = Arc::new(ManualExecutor::new(false));
    f.backend.defer_launches(Arc::clone(&queue));
    let key = f.backend.add_running_with_pid("com.example.a", 7);
    watch(&f);
    assert_eq!(f.backend.deliver_lifecycle(LifecycleKind::Hidden, key), 1);
    // Invalidate before the deferred delivery runs.
    f.observation.invalidate();
    assert_eq!(queue.drain(), 1);
    assert!(
        events(&f).is_empty(),
        "the queued delivery rechecks the generation"
    );

    f.observation.stop().expect("stop");
    assert_eq!(f.backend.lifecycle_observers(), 0);
    assert_eq!(f.backend.deliver_lifecycle(LifecycleKind::Hidden, key), 0);
    let successor = f.control.install(&CancellationToken::new(), |_| {}).expect("reinstall");
    assert_eq!(
        f.backend.lifecycle_observers(),
        0,
        "the successor installs its own"
    );
    drop(successor);
}

#[test]
fn watching_twice_fails_without_removing_the_live_watch() {
    let f = fixture();
    watch(&f);
    let second = f.observation.watch_lifecycle(&CancellationToken::new(), |_| {});
    assert_eq!(second, Err(ApplicationError::AlreadyInstalled));
    assert_eq!(f.backend.lifecycle_observers(), 1);
    assert_eq!(f.backend.lifecycle_removals(), 0);
    let key = f.backend.add_running_with_pid("com.example.a", 9);
    assert_eq!(f.backend.deliver_lifecycle(LifecycleKind::Hidden, key), 1);
    assert_eq!(events(&f).len(), 1);
}

#[test]
fn failed_install_cancelled_install_and_failed_removal_stay_owned_and_retry() {
    let f = fixture();
    f.backend.fail_next_lifecycle_observe();
    let failed = f.observation.watch_lifecycle(&CancellationToken::new(), |_| {});
    assert!(matches!(failed, Err(ApplicationError::ObservationFailed(_))));
    assert_eq!(f.backend.lifecycle_observers(), 0);

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        f.observation.watch_lifecycle(&cancelled, |_| {}),
        Err(ApplicationError::Cancelled)
    );

    watch(&f);
    // The generation removal fails once, stays owned and retries exactly once.
    f.backend.fail_lifecycle_unobserves(1);
    assert!(matches!(
        f.observation.stop(),
        Err(ApplicationError::RemovalFailed(_))
    ));
    assert_eq!(
        f.backend.lifecycle_observers(),
        1,
        "failed removal stays registered and owned"
    );
    assert_eq!(f.backend.lifecycle_removals(), 0);
    f.observation.stop().expect("retry");
    assert_eq!(f.backend.lifecycle_observers(), 0);
    assert_eq!(f.backend.lifecycle_removals(), 1, "removed exactly once");
    f.observation.stop().expect("idempotent");
    assert_eq!(f.backend.lifecycle_removals(), 1);
}

#[test]
fn a_backend_without_lifecycle_support_is_unavailable_not_silent() {
    struct NoLifecycle(FakeApplicationBackend);
    impl ApplicationBackend for NoLifecycle {
        fn observe(
            &self,
            handle: BackendHandle,
            generation: u64,
            notify: Arc<dyn Fn(ApplicationId) + Send + Sync>,
        ) -> Result<(), ApplicationError> {
            self.0.observe(handle, generation, notify)
        }

        fn unobserve(
            &self,
            handle: BackendHandle,
            generation: u64,
        ) -> Result<(), ApplicationError> {
            self.0.unobserve(handle, generation)
        }

        fn running_applications(
            &self,
            handle: BackendHandle,
            bundle_id: &str,
        ) -> Result<Vec<ApplicationId>, ApplicationError> {
            self.0.running_applications(handle, bundle_id)
        }

        fn is_running(
            &self,
            handle: BackendHandle,
            application: ApplicationId,
        ) -> Result<bool, ApplicationError> {
            self.0.is_running(handle, application)
        }

        fn bundle_identifier(
            &self,
            handle: BackendHandle,
            application: ApplicationId,
        ) -> Result<Option<String>, ApplicationError> {
            self.0.bundle_identifier(handle, application)
        }

        fn bundle_identifier_at_path(
            &self,
            handle: BackendHandle,
            path: &std::path::Path,
        ) -> Result<Option<String>, ApplicationError> {
            self.0.bundle_identifier_at_path(handle, path)
        }

        fn application_path(
            &self,
            handle: BackendHandle,
            bundle_id: &str,
        ) -> Result<Option<std::path::PathBuf>, ApplicationError> {
            self.0.application_path(handle, bundle_id)
        }

        fn request_terminate(
            &self,
            handle: BackendHandle,
            application: ApplicationId,
            permit: IssuancePermit<'_>,
        ) -> Result<TerminateOutcome, ApplicationError> {
            self.0.request_terminate(handle, application, permit)
        }

        fn open(
            &self,
            handle: BackendHandle,
            path: &std::path::Path,
            completion: NativeOpenCompletion,
            permit: IssuancePermit<'_>,
        ) -> Result<OpenRequest, ApplicationError> {
            self.0.open(handle, path, completion, permit)
        }
    }

    let executor = Arc::new(ManualExecutor::new(true));
    let inner = FakeApplicationBackend::new();
    let _key = inner.add_running_with_pid("com.example.a", 5);
    let control = ApplicationControl::with_acknowledgement_budget(
        executor as Arc<dyn MainThreadExecutor>,
        Arc::new(NoLifecycle(inner)),
        BUDGET,
    );
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    assert_eq!(
        observation.watch_lifecycle(&CancellationToken::new(), |_| {}),
        Err(ApplicationError::Unavailable)
    );
    // Hide is equally explicit: the default backend method reports Unavailable
    // before the issuance permit, so it proves no send.
    let id = observation.running_applications("com.example.a").expect("running")[0];
    let accepted = observation.accept().expect("accept");
    let authorization = VisibilityAuthorization::new(id, accepted);
    let outcome = observation.hide_exact(accepted, id, &authorization);
    assert_eq!(
        outcome,
        VisibilityOutcome::NotIssued(ApplicationError::Unavailable)
    );
    assert!(outcome.proves_no_send());
    assert!(!authorization.is_issued());
}
