//! Exact-instance activation tests (MA-02-08).
//!
//! Distinct activation recording, every outcome, retained-instance and
//! generation isolation, and barriers before preflight, before the permit and
//! authorization commit, inside the send and after the issued activation. All
//! fakes: no application is ever activated.

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
    authorization: ActivationAuthorization,
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
        authorization: ActivationAuthorization::new(id, accepted),
    }
}

fn activate(f: &Fixture, target: &Target) -> ActivationOutcome {
    f.observation.activate_exact(target.accepted, target.id, &target.authorization)
}

fn spawn_activate(f: &Fixture, target: &Target) -> std::thread::JoinHandle<ActivationOutcome> {
    let observation = Arc::clone(&f.observation);
    let (accepted, id, authorization) = (target.accepted, target.id, target.authorization.clone());
    std::thread::spawn(move || observation.activate_exact(accepted, id, &authorization))
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

fn assert_nothing_else_sent(f: &Fixture) {
    assert!(f.backend.termination_requests().is_empty());
    assert!(f.backend.graceful_requests().is_empty());
    assert!(f.backend.open_requests().is_empty());
}

#[test]
fn requested_records_exactly_the_exact_instance_and_nothing_else() {
    let f = fixture(BUDGET);
    let (t, key) = target(&f, "com.example.a", 100);
    let other = f.backend.add_running_with_pid("com.example.b", 200);
    f.backend.set_frontmost(Some(other));
    assert_eq!(activate(&f, &t), ActivationOutcome::Requested);
    // The exact captured instance, not whatever is frontmost.
    assert_eq!(f.backend.exact_activation_requests(), vec![key]);
    assert!(t.authorization.is_issued());
    assert_nothing_else_sent(&f);
    assert_eq!(f.backend.terminate_attempts(), 0);
    assert_eq!(f.backend.graceful_attempts(), 0);
}

#[test]
fn refused_is_a_confirmed_false_and_is_never_retried() {
    let f = fixture(BUDGET);
    let (t, key) = target(&f, "com.example.a", 100);
    f.backend.refuse_exact_activation();
    let outcome = activate(&f, &t);
    assert_eq!(outcome, ActivationOutcome::Refused);
    assert!(!outcome.proves_no_send());
    assert_eq!(f.backend.exact_activation_attempts(), 1);
    assert_eq!(f.backend.exact_activation_requests(), vec![key]);
}

#[test]
fn a_dead_instance_is_stale_and_nothing_is_sent() {
    let f = fixture(BUDGET);
    let (t, key) = target(&f, "com.example.a", 100);
    f.backend.terminate(key);
    let outcome = activate(&f, &t);
    assert_eq!(outcome, ActivationOutcome::Stale);
    assert!(outcome.proves_no_send());
    assert!(f.backend.exact_activation_requests().is_empty());
}

#[test]
fn a_same_pid_same_bundle_replacement_is_rejected_and_never_retargeted() {
    let f = fixture(BUDGET);
    let (old, old_key) = target(&f, "com.example.a", 100);
    f.backend.terminate(old_key);
    let replacement = f.backend.add_running_with_pid("com.example.a", 100);
    // The old retained id never resolves to the replacement.
    assert_eq!(activate(&f, &old), ActivationOutcome::Stale);
    assert!(f.backend.exact_activation_requests().is_empty());
    // The replacement has its own id and activates only itself.
    let fresh = retarget(&f, "com.example.a");
    assert_ne!(fresh.id, old.id);
    assert_eq!(activate(&f, &fresh), ActivationOutcome::Requested);
    assert_eq!(f.backend.exact_activation_requests(), vec![replacement]);
}

#[test]
fn a_stale_token_or_mismatched_authorization_sends_nothing() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    let _newer = f.observation.accept().expect("newer revision");
    assert_eq!(activate(&f, &t), ActivationOutcome::Stale);
    // The permit refused at the send boundary; the authorization is untouched.
    let attempts = f.backend.exact_activation_attempts();

    let (fresh, _) = target(&f, "com.example.b", 200);
    // An authorization naming another instance is stale, not consumed.
    assert_eq!(
        f.observation.activate_exact(fresh.accepted, fresh.id, &t.authorization),
        ActivationOutcome::Stale
    );
    assert_eq!(
        f.backend.exact_activation_attempts(),
        attempts,
        "rejected before dispatch"
    );
    assert!(f.backend.exact_activation_requests().is_empty());
    assert!(!t.authorization.is_issued() && !fresh.authorization.is_issued());
}

#[test]
fn a_foreign_control_instance_and_an_old_generation_are_stale() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    let other = fixture(BUDGET);
    assert_eq!(
        other.observation.activate_exact(t.accepted, t.id, &t.authorization),
        ActivationOutcome::Stale
    );
    assert_eq!(other.backend.exact_activation_attempts(), 0);

    // Successor generation on the same control: the old id is isolated.
    f.observation.stop().expect("stop");
    let successor = f.control.install(&CancellationToken::new(), |_| {}).expect("reinstall");
    assert_eq!(
        successor.activate_exact(t.accepted, t.id, &t.authorization),
        ActivationOutcome::Stale
    );
    assert_eq!(
        activate(&f, &t),
        ActivationOutcome::Stale,
        "stopped generation is inert"
    );
    assert!(f.backend.exact_activation_requests().is_empty());

    let id = successor.running_applications("com.example.a").expect("running")[0];
    let accepted = successor.accept().expect("accept");
    let authorization = ActivationAuthorization::new(id, accepted);
    assert_eq!(
        successor.activate_exact(accepted, id, &authorization),
        ActivationOutcome::Requested
    );
    assert_eq!(f.backend.exact_activation_requests().len(), 1);
}

#[test]
fn cancelled_or_expired_authorizations_never_dispatch() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    assert_eq!(t.authorization.cancel(), ActivationCancel::Cancelled);
    assert_eq!(activate(&f, &t), ActivationOutcome::Cancelled);

    let past = Instant::now().checked_sub(Duration::from_millis(1)).expect("past");
    let expired = ActivationAuthorization::with_deadline(t.id, t.accepted, past);
    let outcome = f.observation.activate_exact(t.accepted, t.id, &expired);
    assert_eq!(outcome, ActivationOutcome::ExpiredBeforeSend);
    assert!(outcome.proves_no_send());
    assert_eq!(f.backend.exact_activation_attempts(), 0);
}

#[test]
fn cancel_before_native_preflight_prevents_the_send() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    let barrier = f.backend.block_next_activation();
    let worker = spawn_activate(&f, &t);
    assert!(barrier.wait_entered(Duration::from_secs(2)));
    assert_eq!(t.authorization.cancel(), ActivationCancel::Cancelled);
    let _ = barrier.release();
    assert_eq!(worker.join().expect("join"), ActivationOutcome::Cancelled);
    assert!(f.backend.exact_activation_requests().is_empty());
}

#[test]
fn cancel_immediately_before_the_permit_commit_prevents_the_send() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    let barrier = f.backend.block_next_activation_before_commit();
    let worker = spawn_activate(&f, &t);
    assert!(barrier.wait_entered(Duration::from_secs(2)));
    assert_eq!(t.authorization.cancel(), ActivationCancel::Cancelled);
    let _ = barrier.release();
    let outcome = worker.join().expect("join");
    assert_eq!(outcome, ActivationOutcome::Cancelled);
    assert!(outcome.proves_no_send());
    assert!(f.backend.exact_activation_requests().is_empty());
    assert!(!t.authorization.is_issued());
}

#[test]
fn a_deadline_passing_before_the_commit_prevents_the_send() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    let authorization = ActivationAuthorization::with_deadline(
        t.id,
        t.accepted,
        Instant::now() + Duration::from_millis(60),
    );
    let barrier = f.backend.block_next_activation_before_commit();
    let observation = Arc::clone(&f.observation);
    let (accepted, id, job_authorization) = (t.accepted, t.id, authorization.clone());
    let worker =
        std::thread::spawn(move || observation.activate_exact(accepted, id, &job_authorization));
    assert!(barrier.wait_entered(Duration::from_secs(2)));
    std::thread::sleep(Duration::from_millis(80));
    let _ = barrier.release();
    assert_eq!(
        worker.join().expect("join"),
        ActivationOutcome::ExpiredBeforeSend
    );
    assert!(f.backend.exact_activation_requests().is_empty());
    assert!(!authorization.is_issued());
}

#[test]
fn a_reconfigure_or_stop_before_the_commit_prevents_the_send() {
    for stop in [false, true] {
        let f = fixture(BUDGET);
        let (t, _) = target(&f, "com.example.a", 100);
        let barrier = f.backend.block_next_activation_before_commit();
        let worker = spawn_activate(&f, &t);
        assert!(barrier.wait_entered(Duration::from_secs(2)));
        if stop {
            f.observation.stop().expect("stop");
        } else {
            let _ = f.observation.accept().expect("reconfigure");
        }
        let _ = barrier.release();
        assert_eq!(worker.join().expect("join"), ActivationOutcome::Stale);
        assert!(f.backend.exact_activation_requests().is_empty());
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
    let barrier = f.backend.block_next_activation();
    let worker = spawn_activate(&f, &t);
    assert!(barrier.wait_entered(Duration::from_secs(2)));
    f.backend.terminate(key);
    let _replacement = f.backend.add_running_with_pid("com.example.a", 100);
    let _ = barrier.release();
    assert_eq!(worker.join().expect("join"), ActivationOutcome::Stale);
    assert!(f.backend.exact_activation_requests().is_empty());
}

#[test]
fn issuance_winning_records_one_activation_even_when_cancelled_afterwards() {
    for after_issue in [false, true] {
        let f = fixture(BUDGET);
        let (t, key) = target(&f, "com.example.a", 100);
        let barrier = if after_issue {
            f.backend.block_next_activation_after_issue()
        } else {
            // Inside the send, under the commit gate, after the commit.
            f.backend.block_next_activation_issuance()
        };
        let worker = spawn_activate(&f, &t);
        assert!(barrier.wait_entered(Duration::from_secs(2)));
        // Cancel is nonblocking and gate-free and cannot reverse the send.
        assert_eq!(t.authorization.cancel(), ActivationCancel::AlreadyIssued);
        let _ = barrier.release();
        assert_eq!(worker.join().expect("join"), ActivationOutcome::Requested);
        assert_eq!(f.backend.exact_activation_requests(), vec![key]);
        assert!(t.authorization.is_issued() && !t.authorization.is_cancelled());
        assert_eq!(f.backend.exact_activation_attempts(), 1, "never retried");
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
    assert!(!auth.is_cancelled());

    let cancelled = ActivationAuthorization::new(t.id, t.accepted);
    assert_eq!(cancelled.cancel(), ActivationCancel::Cancelled);
    assert_eq!(cancelled.cancel(), ActivationCancel::Cancelled, "idempotent");
    assert!(!cancelled.commit_send());
    assert!(cancelled.is_cancelled() && !cancelled.is_issued());
    assert_eq!(cancelled.application(), t.id);
    assert_eq!(cancelled.accepted(), t.accepted);
}

#[test]
fn a_timeout_after_issuance_is_uncertain_and_a_late_return_is_harmless() {
    let f = fixture(Duration::from_millis(100));
    f.executor.set_on_main(true);
    let (t, key) = target(&f, "com.example.a", 100);
    let barrier = f.backend.block_next_activation_after_issue();
    f.executor.set_on_main(false);
    let caller = spawn_activate(&f, &t);
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
    assert_eq!(outcome, ActivationOutcome::Uncertain(ApplicationError::TimedOut));
    assert!(!outcome.proves_no_send());
    assert_ne!(outcome, ActivationOutcome::Refused);
    let _ = barrier.release();
    drainer.join().expect("drainer");
    assert_eq!(f.backend.exact_activation_requests(), vec![key]);
    assert_eq!(f.backend.exact_activation_attempts(), 1);
    assert_nothing_else_sent(&f);
}

#[test]
fn an_abandoned_request_before_issuance_proves_no_send() {
    let f = fixture(Duration::from_millis(30));
    let (t, _) = target(&f, "com.example.a", 100);
    f.executor.set_on_main(false);
    let outcome = activate(&f, &t);
    assert_eq!(outcome, ActivationOutcome::NotIssued(ApplicationError::TimedOut));
    assert!(outcome.proves_no_send());
    // The queued job runs late and must stay inert.
    f.executor.set_on_main(true);
    f.executor.drain();
    assert!(f.backend.exact_activation_requests().is_empty());
    assert!(!t.authorization.is_issued());
}

#[test]
fn a_rejected_dispatch_proves_no_send() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    f.executor.reject(true);
    let outcome = activate(&f, &t);
    f.executor.reject(false);
    assert!(matches!(
        outcome,
        ActivationOutcome::NotIssued(ApplicationError::Dispatch(_))
    ));
    assert!(outcome.proves_no_send());
    assert!(f.backend.exact_activation_requests().is_empty());
    assert!(!t.authorization.is_issued());
}

#[test]
fn a_preissuance_query_failure_is_not_issued() {
    let f = fixture(BUDGET);
    let (t, _) = target(&f, "com.example.a", 100);
    f.backend.fail_next_query();
    assert_eq!(
        activate(&f, &t),
        ActivationOutcome::NotIssued(ApplicationError::Unavailable)
    );
    assert!(f.backend.exact_activation_requests().is_empty());
}

#[test]
fn exact_activation_does_not_disturb_the_frontmost_fixture_or_other_operations() {
    let f = fixture(BUDGET);
    let (t, key) = target(&f, "com.example.a", 100);
    // The fake frontmost fixture is separate from the recorded request.
    assert_eq!(f.backend.activate(key), 0);
    assert!(f.backend.exact_activation_requests().is_empty());
    assert_eq!(activate(&f, &t), ActivationOutcome::Requested);
    let result: Result<TerminateOutcome, ApplicationError> =
        f.observation.terminate(t.accepted, t.id);
    assert_eq!(result, Ok(TerminateOutcome::Requested));
    assert_eq!(f.backend.termination_requests(), vec![key]);
    assert_eq!(f.backend.exact_activation_requests(), vec![key]);
}
