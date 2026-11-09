//! Exact-instance hide/unhide tests.
//!
//! Distinct hide/unhide recording, every outcome, retained-instance and
//! generation isolation, PID/app replacement, and barriers before preflight,
//! before the permit and authorization commit, inside the send and after the
//! issued request. All fakes: no application is ever hidden or unhidden.

use std::sync::Arc;
use std::time::{Duration, Instant};

use stache_sdk::{CancellationToken, MainThreadExecutor};

use super::testing::{FakeAppKey, FakeApplicationBackend, ManualExecutor};
use super::*;

const BUDGET: Duration = Duration::from_millis(500);

struct Fixture {
    control: ApplicationControl,
    backend: Arc<FakeApplicationBackend>,
    executor: Arc<ManualExecutor>,
    observation: Arc<ApplicationObservation>,
}

fn fixture(budget: Duration) -> Fixture {
    let executor = Arc::new(ManualExecutor::new(true));
    let backend = Arc::new(FakeApplicationBackend::new());
    let control = ApplicationControl::with_acknowledgement_budget(
        Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
        Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
        budget,
    );
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    Fixture {
        control,
        backend,
        executor,
        observation: Arc::new(observation),
    }
}

struct Target {
    id: ApplicationId,
    accepted: AcceptedActionToken,
    authorization: VisibilityAuthorization,
}

fn target(f: &Fixture, bundle: &str, pid: i32) -> (Target, FakeAppKey) {
    let key = f.backend.add_running_with_pid(bundle, pid);
    (retarget(f, bundle), key)
}

fn retarget(f: &Fixture, bundle: &str) -> Target {
    let id = *f
        .observation
        .running_applications(bundle)
        .expect("running")
        .last()
        .expect("one");
    let accepted = f.observation.accept().expect("accept");
    Target {
        id,
        accepted,
        authorization: VisibilityAuthorization::new(id, accepted),
    }
}

fn run(f: &Fixture, operation: VisibilityOperation, target: &Target) -> VisibilityOutcome {
    match operation {
        VisibilityOperation::Hide => {
            f.observation.hide_exact(target.accepted, target.id, &target.authorization)
        }
        VisibilityOperation::Unhide => {
            f.observation.unhide_exact(target.accepted, target.id, &target.authorization)
        }
    }
}

fn spawn(
    f: &Fixture,
    operation: VisibilityOperation,
    target: &Target,
) -> std::thread::JoinHandle<VisibilityOutcome> {
    let observation = Arc::clone(&f.observation);
    let (accepted, id, authorization) = (target.accepted, target.id, target.authorization.clone());
    std::thread::spawn(move || match operation {
        VisibilityOperation::Hide => observation.hide_exact(accepted, id, &authorization),
        VisibilityOperation::Unhide => observation.unhide_exact(accepted, id, &authorization),
    })
}

fn wait_queued(executor: &ManualExecutor, at_least: usize) {
    for _ in 0..500 {
        if executor.queued() >= at_least {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the executor queue never reached {at_least}");
}

const BOTH: [VisibilityOperation; 2] = [VisibilityOperation::Hide, VisibilityOperation::Unhide];

#[test]
fn hide_and_unhide_record_exactly_the_exact_instance_and_nothing_else() {
    let f = fixture(BUDGET);
    let (t, key) = target(&f, "com.example.a", 100);
    let other = f.backend.add_running_with_pid("com.example.b", 200);
    f.backend.set_frontmost(Some(other));
    assert_eq!(
        run(&f, VisibilityOperation::Hide, &t),
        VisibilityOutcome::Requested
    );
    assert!(f.backend.is_hidden(key) && !f.backend.is_hidden(other));
    let again = retarget(&f, "com.example.a");
    assert_eq!(
        run(&f, VisibilityOperation::Unhide, &again),
        VisibilityOutcome::Requested
    );
    assert!(!f.backend.is_hidden(key));
    assert_eq!(f.backend.visibility_requests(), vec![
        (key, VisibilityOperation::Hide),
        (key, VisibilityOperation::Unhide),
    ]);
    // No second activation path, termination or open.
    assert!(f.backend.exact_activation_requests().is_empty());
    assert!(f.backend.termination_requests().is_empty());
    assert!(f.backend.graceful_requests().is_empty());
    assert!(f.backend.open_requests().is_empty());
}

#[test]
fn refused_is_a_confirmed_false_and_is_never_retried() {
    for operation in BOTH {
        let f = fixture(BUDGET);
        let (t, key) = target(&f, "com.example.a", 100);
        f.backend.refuse_visibility();
        let outcome = run(&f, operation, &t);
        assert_eq!(outcome, VisibilityOutcome::Refused);
        assert!(!outcome.proves_no_send());
        assert_eq!(f.backend.visibility_attempts(), 1);
        assert_eq!(f.backend.visibility_requests(), vec![(key, operation)]);
        assert!(!f.backend.is_hidden(key), "a refused hide changed nothing");
    }
}

#[test]
fn a_dead_instance_is_stale_and_nothing_is_sent() {
    for operation in BOTH {
        let f = fixture(BUDGET);
        let (t, key) = target(&f, "com.example.a", 100);
        f.backend.terminate(key);
        let outcome = run(&f, operation, &t);
        assert_eq!(outcome, VisibilityOutcome::Stale);
        assert!(outcome.proves_no_send());
        assert!(f.backend.visibility_requests().is_empty());
    }
}

#[test]
fn a_same_pid_same_bundle_replacement_is_rejected_and_never_retargeted() {
    let f = fixture(BUDGET);
    let (old, old_key) = target(&f, "com.example.a", 100);
    f.backend.terminate(old_key);
    let replacement = f.backend.add_running_with_pid("com.example.a", 100);
    assert_eq!(
        run(&f, VisibilityOperation::Hide, &old),
        VisibilityOutcome::Stale
    );
    assert!(f.backend.visibility_requests().is_empty());
    assert!(!f.backend.is_hidden(replacement));
    let fresh = retarget(&f, "com.example.a");
    assert_ne!(fresh.id, old.id);
    assert_eq!(
        run(&f, VisibilityOperation::Hide, &fresh),
        VisibilityOutcome::Requested
    );
    assert_eq!(f.backend.visibility_requests(), vec![(
        replacement,
        VisibilityOperation::Hide
    )]);
}

#[test]
fn a_stale_token_or_mismatched_authorization_sends_nothing() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    let _newer = f.observation.accept().expect("newer revision");
    assert_eq!(run(&f, VisibilityOperation::Hide, &t), VisibilityOutcome::Stale);
    let attempts = f.backend.visibility_attempts();

    let (fresh, _) = target(&f, "com.example.b", 200);
    assert_eq!(
        f.observation.hide_exact(fresh.accepted, fresh.id, &t.authorization),
        VisibilityOutcome::Stale
    );
    assert_eq!(
        f.backend.visibility_attempts(),
        attempts,
        "rejected before dispatch"
    );
    assert!(f.backend.visibility_requests().is_empty());
    assert!(!t.authorization.is_issued() && !fresh.authorization.is_issued());
}

#[test]
fn a_foreign_control_instance_and_an_old_generation_are_stale() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    let other = fixture(BUDGET);
    assert_eq!(
        other.observation.unhide_exact(t.accepted, t.id, &t.authorization),
        VisibilityOutcome::Stale
    );
    assert_eq!(other.backend.visibility_attempts(), 0);

    f.observation.stop().expect("stop");
    let successor = f.control.install(&CancellationToken::new(), |_| {}).expect("reinstall");
    assert_eq!(
        successor.hide_exact(t.accepted, t.id, &t.authorization),
        VisibilityOutcome::Stale
    );
    assert_eq!(run(&f, VisibilityOperation::Hide, &t), VisibilityOutcome::Stale);
    assert!(f.backend.visibility_requests().is_empty());

    let id = successor.running_applications("com.example.a").expect("running")[0];
    let accepted = successor.accept().expect("accept");
    let authorization = VisibilityAuthorization::new(id, accepted);
    assert_eq!(
        successor.hide_exact(accepted, id, &authorization),
        VisibilityOutcome::Requested
    );
    assert_eq!(f.backend.visibility_requests().len(), 1);
}

#[test]
fn cancelled_or_expired_authorizations_never_dispatch() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    assert_eq!(t.authorization.cancel(), ActivationCancel::Cancelled);
    assert_eq!(
        run(&f, VisibilityOperation::Hide, &t),
        VisibilityOutcome::Cancelled
    );

    let past = Instant::now().checked_sub(Duration::from_millis(1)).expect("past");
    let expired = VisibilityAuthorization::with_deadline(t.id, t.accepted, past);
    let outcome = f.observation.unhide_exact(t.accepted, t.id, &expired);
    assert_eq!(outcome, VisibilityOutcome::ExpiredBeforeSend);
    assert!(outcome.proves_no_send());
    assert_eq!(f.backend.visibility_attempts(), 0);
}

#[test]
fn cancel_before_preflight_or_before_commit_prevents_the_send() {
    for before_commit in [false, true] {
        let f = fixture(BUDGET);
        let (t, _) = target(&f, "com.example.a", 100);
        let barrier = if before_commit {
            f.backend.block_next_visibility_before_commit()
        } else {
            f.backend.block_next_visibility()
        };
        let worker = spawn(&f, VisibilityOperation::Hide, &t);
        assert!(barrier.wait_entered(Duration::from_secs(2)));
        assert_eq!(t.authorization.cancel(), ActivationCancel::Cancelled);
        let _ = barrier.release();
        let outcome = worker.join().expect("join");
        assert_eq!(outcome, VisibilityOutcome::Cancelled);
        assert!(outcome.proves_no_send());
        assert!(f.backend.visibility_requests().is_empty());
        assert!(!t.authorization.is_issued());
    }
}

#[test]
fn a_deadline_passing_before_the_commit_prevents_the_send() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    let authorization = VisibilityAuthorization::with_deadline(
        t.id,
        t.accepted,
        Instant::now() + Duration::from_millis(60),
    );
    let barrier = f.backend.block_next_visibility_before_commit();
    let observation = Arc::clone(&f.observation);
    let (accepted, id, job_authorization) = (t.accepted, t.id, authorization.clone());
    let worker =
        std::thread::spawn(move || observation.hide_exact(accepted, id, &job_authorization));
    assert!(barrier.wait_entered(Duration::from_secs(2)));
    std::thread::sleep(Duration::from_millis(80));
    let _ = barrier.release();
    assert_eq!(
        worker.join().expect("join"),
        VisibilityOutcome::ExpiredBeforeSend
    );
    assert!(f.backend.visibility_requests().is_empty());
    assert!(!authorization.is_issued());
}

#[test]
fn a_reconfigure_or_stop_before_the_commit_prevents_the_send() {
    for stop in [false, true] {
        let f = fixture(BUDGET);
        let (t, _) = target(&f, "com.example.a", 100);
        let barrier = f.backend.block_next_visibility_before_commit();
        let worker = spawn(&f, VisibilityOperation::Unhide, &t);
        assert!(barrier.wait_entered(Duration::from_secs(2)));
        if stop {
            f.observation.stop().expect("stop");
        } else {
            let _ = f.observation.accept().expect("reconfigure");
        }
        let _ = barrier.release();
        assert_eq!(worker.join().expect("join"), VisibilityOutcome::Stale);
        assert!(f.backend.visibility_requests().is_empty());
        assert!(
            !t.authorization.is_issued(),
            "the authorization was not consumed"
        );
    }
}

#[test]
fn the_instance_dying_or_being_replaced_at_the_barrier_prevents_the_send() {
    let f = fixture(BUDGET);
    let (t, key) = target(&f, "com.example.a", 100);
    let barrier = f.backend.block_next_visibility();
    let worker = spawn(&f, VisibilityOperation::Hide, &t);
    assert!(barrier.wait_entered(Duration::from_secs(2)));
    f.backend.terminate(key);
    let replacement = f.backend.add_running_with_pid("com.example.a", 100);
    let _ = barrier.release();
    assert_eq!(worker.join().expect("join"), VisibilityOutcome::Stale);
    assert!(f.backend.visibility_requests().is_empty());
    assert!(!f.backend.is_hidden(replacement));
}

#[test]
fn issuance_winning_records_one_request_even_when_cancelled_afterwards() {
    for after_issue in [false, true] {
        let f = fixture(BUDGET);
        let (t, key) = target(&f, "com.example.a", 100);
        let barrier = if after_issue {
            f.backend.block_next_visibility_after_issue()
        } else {
            f.backend.block_next_visibility_issuance()
        };
        let worker = spawn(&f, VisibilityOperation::Hide, &t);
        assert!(barrier.wait_entered(Duration::from_secs(2)));
        assert_eq!(t.authorization.cancel(), ActivationCancel::AlreadyIssued);
        let _ = barrier.release();
        assert_eq!(worker.join().expect("join"), VisibilityOutcome::Requested);
        assert_eq!(f.backend.visibility_requests(), vec![(
            key,
            VisibilityOperation::Hide
        )]);
        assert!(t.authorization.is_issued() && !t.authorization.is_cancelled());
        assert_eq!(f.backend.visibility_attempts(), 1, "never retried");
    }
}

#[test]
fn the_authorization_commit_is_one_shot_in_both_orders() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    let auth = t.authorization.clone();
    assert!(auth.commit_send());
    assert!(!auth.commit_send(), "a second send is impossible");
    assert_eq!(auth.cancel(), ActivationCancel::AlreadyIssued);

    let cancelled = VisibilityAuthorization::new(t.id, t.accepted);
    assert_eq!(cancelled.cancel(), ActivationCancel::Cancelled);
    assert!(!cancelled.commit_send());
    assert_eq!(cancelled.application(), t.id);
    assert_eq!(cancelled.accepted(), t.accepted);
}

#[test]
fn a_timeout_after_issuance_is_uncertain_and_a_late_return_is_harmless() {
    let f = fixture(Duration::from_millis(100));
    f.executor.set_on_main(true);
    let (t, key) = target(&f, "com.example.a", 100);
    let barrier = f.backend.block_next_visibility_after_issue();
    f.executor.set_on_main(false);
    let caller = spawn(&f, VisibilityOperation::Hide, &t);
    wait_queued(&f.executor, 1);
    let drainer = {
        let executor = Arc::clone(&f.executor);
        std::thread::spawn(move || {
            executor.set_on_main(true);
            executor.drain();
        })
    };
    assert!(
        barrier.wait_entered(Duration::from_secs(2)),
        "issued before parking"
    );
    let outcome = caller.join().expect("join");
    assert_eq!(outcome, VisibilityOutcome::Uncertain(ApplicationError::TimedOut));
    assert!(!outcome.proves_no_send());
    assert_ne!(outcome, VisibilityOutcome::Refused);
    let _ = barrier.release();
    drainer.join().expect("drainer");
    assert_eq!(f.backend.visibility_requests(), vec![(
        key,
        VisibilityOperation::Hide
    )]);
    assert_eq!(f.backend.visibility_attempts(), 1);
}

#[test]
fn an_abandoned_request_before_issuance_proves_no_send() {
    let f = fixture(Duration::from_millis(30));
    let (t, _) = target(&f, "com.example.a", 100);
    f.executor.set_on_main(false);
    let outcome = run(&f, VisibilityOperation::Unhide, &t);
    assert_eq!(outcome, VisibilityOutcome::NotIssued(ApplicationError::TimedOut));
    assert!(outcome.proves_no_send());
    // The queued job runs late and must stay inert.
    f.executor.set_on_main(true);
    f.executor.drain();
    assert!(f.backend.visibility_requests().is_empty());
    assert!(!t.authorization.is_issued());
}

#[test]
fn a_rejected_dispatch_or_preissuance_failure_proves_no_send() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    f.executor.reject(true);
    let outcome = run(&f, VisibilityOperation::Hide, &t);
    f.executor.reject(false);
    assert!(matches!(
        outcome,
        VisibilityOutcome::NotIssued(ApplicationError::Dispatch(_))
    ));
    assert!(outcome.proves_no_send());

    f.backend.fail_next_query();
    assert_eq!(
        run(&f, VisibilityOperation::Hide, &t),
        VisibilityOutcome::NotIssued(ApplicationError::Unavailable)
    );
    assert!(f.backend.visibility_requests().is_empty());
    assert!(!t.authorization.is_issued());
}

#[test]
fn visibility_does_not_disturb_activation_or_termination() {
    let f = fixture(BUDGET);
    let (t, key) = target(&f, "com.example.a", 100);
    assert_eq!(
        run(&f, VisibilityOperation::Hide, &t),
        VisibilityOutcome::Requested
    );
    let activation = ActivationAuthorization::new(t.id, t.accepted);
    assert_eq!(
        f.observation.activate_exact(t.accepted, t.id, &activation),
        ActivationOutcome::Requested
    );
    assert_eq!(f.backend.exact_activation_requests(), vec![key]);
    assert_eq!(
        f.observation.terminate(t.accepted, t.id),
        Ok(TerminateOutcome::Requested)
    );
    assert_eq!(f.backend.visibility_requests(), vec![(
        key,
        VisibilityOperation::Hide
    )]);
}
