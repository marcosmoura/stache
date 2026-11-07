//! Graceful issuance tests (HQ-03-02, HQ-03-03).
//!
//! Distinct graceful/force recording, every outcome, and barriers before
//! preflight, before the permit/press commit and after the committed send. All
//! fakes: no application is ever terminated.

use std::sync::Arc;
use std::time::Duration;

use stache_sdk::{CancellationToken, MainThreadExecutor};

use super::frontmost::PreflightRejection;
use super::testing::{FakeApplicationBackend, ManualExecutor};
use super::*;
use crate::hold_hud::PressId;

const BUDGET: Duration = Duration::from_millis(500);

struct Fixture {
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
        backend,
        executor,
        observation: Arc::new(observation),
    }
}

struct Press {
    id: ApplicationId,
    accepted: AcceptedActionToken,
    authorization: PressAuthorization,
}

/// A frontmost, running application with an armed press authorization.
fn press(f: &Fixture, bundle: &str, pid: i32) -> (Press, super::testing::FakeAppKey) {
    let key = f.backend.add_running_with_pid(bundle, pid);
    f.backend.set_frontmost(Some(key));
    let id = f.observation.running_applications(bundle).expect("running")[0];
    let accepted = f.observation.accept().expect("accept");
    let authorization = PressAuthorization::new(PressId::new(1), id, accepted);
    (Press { id, accepted, authorization }, key)
}

fn quit(f: &Fixture, press: &Press) -> GracefulOutcome {
    f.observation
        .terminate_gracefully(press.accepted, press.id, &press.authorization)
}

fn spawn_quit(f: &Fixture, press: &Press) -> std::thread::JoinHandle<GracefulOutcome> {
    let observation = Arc::clone(&f.observation);
    let (accepted, id, authorization) = (press.accepted, press.id, press.authorization.clone());
    std::thread::spawn(move || observation.terminate_gracefully(accepted, id, &authorization))
}

#[test]
fn graceful_issuance_calls_only_terminate_and_never_the_force_path() {
    let f = fixture(BUDGET);
    let (press, key) = press(&f, "com.example.a", 100);
    assert_eq!(quit(&f, &press), GracefulOutcome::Requested);
    assert_eq!(f.backend.graceful_requests(), vec![key]);
    assert!(f.backend.termination_requests().is_empty(), "no forceTerminate");
    assert_eq!(f.backend.terminate_attempts(), 0);
    assert!(press.authorization.is_issued());
}

#[test]
fn the_force_api_keeps_its_signature_and_behaviour() {
    let f = fixture(BUDGET);
    let (press, key) = press(&f, "com.example.a", 100);
    // Same call shape noTunes uses.
    let outcome: Result<TerminateOutcome, ApplicationError> =
        f.observation.terminate(press.accepted, press.id);
    assert_eq!(outcome, Ok(TerminateOutcome::Requested));
    assert_eq!(f.backend.termination_requests(), vec![key]);
    assert_eq!(
        f.backend.graceful_attempts(),
        0,
        "force never takes the graceful path"
    );
}

#[test]
fn a_confirmed_refusal_is_refused_and_is_neither_retried_nor_escalated() {
    let f = fixture(BUDGET);
    let (press, _) = press(&f, "com.example.a", 100);
    f.backend.refuse_graceful_terminate();
    assert_eq!(quit(&f, &press), GracefulOutcome::Refused);
    assert_eq!(f.backend.graceful_attempts(), 1);
    assert!(f.backend.termination_requests().is_empty());
    assert!(!GracefulOutcome::Refused.proves_no_send());
}

#[test]
fn a_dead_instance_is_not_running_and_nothing_is_sent() {
    let f = fixture(BUDGET);
    let (press, key) = press(&f, "com.example.a", 100);
    f.backend.terminate(key);
    let outcome = quit(&f, &press);
    assert_eq!(outcome, GracefulOutcome::NotRunning);
    assert!(outcome.proves_no_send());
    assert!(f.backend.graceful_requests().is_empty());
}

#[test]
fn a_stale_accepted_token_sends_nothing() {
    let f = fixture(BUDGET);
    let (press, _) = press(&f, "com.example.a", 100);
    let _newer = f.observation.accept().expect("newer revision");
    let outcome = quit(&f, &press);
    assert_eq!(outcome, GracefulOutcome::Stale);
    assert!(outcome.proves_no_send());
    assert!(f.backend.graceful_requests().is_empty());
    assert!(!press.authorization.is_issued());
}

#[test]
fn an_authorization_for_another_instance_or_revision_is_stale() {
    let f = fixture(BUDGET);
    let (press, _) = press(&f, "com.example.a", 100);
    let other_key = f.backend.add_running_with_pid("com.example.b", 200);
    f.backend.set_frontmost(Some(other_key));
    let other = f.observation.running_applications("com.example.b").expect("running")[0];
    assert_eq!(
        f.observation.terminate_gracefully(press.accepted, other, &press.authorization),
        GracefulOutcome::Stale
    );
    assert_eq!(f.backend.graceful_attempts(), 0);
}

#[test]
fn a_foreign_control_instance_is_not_running() {
    let f = fixture(BUDGET);
    let (press, _) = press(&f, "com.example.a", 100);
    let other = fixture(BUDGET);
    assert_eq!(
        other
            .observation
            .terminate_gracefully(press.accepted, press.id, &press.authorization),
        GracefulOutcome::NotRunning
    );
    assert_eq!(other.backend.graceful_attempts(), 0);
}

#[test]
fn the_main_preflight_rejects_mismatched_exempt_missing_and_reused_instances() {
    let f = fixture(BUDGET);
    let (press, key) = press(&f, "com.example.a", 100);
    let b = f.backend.add_running_with_pid("com.example.b", 200);
    let finder = f.backend.add_running_with_pid(FINDER_BUNDLE_IDENTIFIER, 1);
    let stache = f.backend.add_running_with_pid("com.example.stache", 300);
    // Same bundle and same PID as the captured instance, different instance.
    let twin = f.backend.add_running_with_pid("com.example.a", 100);
    f.backend.set_current_process(Some(stache));

    for (front, expected) in [
        (Some(b), PreflightRejection::NotFrontmost),
        (Some(finder), PreflightRejection::Finder),
        (Some(stache), PreflightRejection::CurrentProcess),
        (Some(twin), PreflightRejection::NotFrontmost),
        (None, PreflightRejection::NoFrontmost),
    ] {
        f.backend.set_frontmost(front);
        // Fresh authorization per attempt: none of these may consume it.
        let authorization = PressAuthorization::new(PressId::new(2), press.id, press.accepted);
        let outcome = f.observation.terminate_gracefully(press.accepted, press.id, &authorization);
        assert_eq!(outcome, GracefulOutcome::Rejected(expected));
        assert!(outcome.proves_no_send());
        assert!(!authorization.is_issued());
    }
    assert!(f.backend.graceful_requests().is_empty(), "never retargeted");

    // The captured app is frontmost again and still eligible.
    f.backend.set_frontmost(Some(key));
    assert_eq!(quit(&f, &press), GracefulOutcome::Requested);
    assert_eq!(f.backend.graceful_requests(), vec![key]);
}

#[test]
fn a_cancelled_press_never_dispatches() {
    let f = fixture(BUDGET);
    let (press, _) = press(&f, "com.example.a", 100);
    assert_eq!(press.authorization.cancel(), PressCancel::Cancelled);
    assert_eq!(quit(&f, &press), GracefulOutcome::Cancelled);
    assert_eq!(f.backend.graceful_attempts(), 0);
}

#[test]
fn cancel_before_preflight_prevents_the_send() {
    let f = fixture(BUDGET);
    let (press, _) = press(&f, "com.example.a", 100);
    let barrier = f.backend.block_next_graceful_before_preflight();
    let worker = spawn_quit(&f, &press);
    assert!(barrier.wait_entered(Duration::from_secs(2)));
    assert_eq!(press.authorization.cancel(), PressCancel::Cancelled);
    let _ = barrier.release();
    assert_eq!(worker.join().expect("join"), GracefulOutcome::Cancelled);
    assert!(f.backend.graceful_requests().is_empty());
}

#[test]
fn cancel_before_the_permit_and_press_commit_prevents_the_send() {
    let f = fixture(BUDGET);
    let (press, _) = press(&f, "com.example.a", 100);
    let barrier = f.backend.block_next_graceful_before_commit();
    let worker = spawn_quit(&f, &press);
    assert!(barrier.wait_entered(Duration::from_secs(2)));
    assert_eq!(press.authorization.cancel(), PressCancel::Cancelled);
    let _ = barrier.release();
    let outcome = worker.join().expect("join");
    assert_eq!(outcome, GracefulOutcome::Cancelled);
    assert!(outcome.proves_no_send());
    assert!(f.backend.graceful_requests().is_empty());
    assert!(!press.authorization.is_issued());
}

#[test]
fn a_reconfigure_or_stop_before_the_commit_prevents_the_send() {
    for stop in [false, true] {
        let f = fixture(BUDGET);
        let (press, _) = press(&f, "com.example.a", 100);
        let barrier = f.backend.block_next_graceful_before_commit();
        let worker = spawn_quit(&f, &press);
        assert!(barrier.wait_entered(Duration::from_secs(2)));
        if stop {
            f.observation.stop().expect("stop");
        } else {
            let _ = f.observation.accept().expect("reconfigure");
        }
        let _ = barrier.release();
        assert_eq!(worker.join().expect("join"), GracefulOutcome::Stale);
        assert!(f.backend.graceful_requests().is_empty());
        assert!(
            !press.authorization.is_issued(),
            "the authorization was not consumed"
        );
    }
}

#[test]
fn a_send_already_committed_cannot_be_cancelled() {
    let f = fixture(BUDGET);
    let (press, key) = press(&f, "com.example.a", 100);
    // Inside the send, under the commit gate, after the press committed.
    let barrier = f.backend.block_next_graceful_issuance();
    let worker = spawn_quit(&f, &press);
    assert!(barrier.wait_entered(Duration::from_secs(2)));
    // Cancel is nonblocking and gate-free even though the gate is held.
    assert_eq!(press.authorization.cancel(), PressCancel::AlreadyIssued);
    let _ = barrier.release();
    assert_eq!(worker.join().expect("join"), GracefulOutcome::Requested);
    assert_eq!(f.backend.graceful_requests(), vec![key]);
}

#[test]
fn cancel_after_the_committed_send_reports_it_was_issued() {
    let f = fixture(BUDGET);
    let (press, key) = press(&f, "com.example.a", 100);
    let barrier = f.backend.block_next_graceful_after_issue();
    let worker = spawn_quit(&f, &press);
    assert!(barrier.wait_entered(Duration::from_secs(2)));
    assert_eq!(press.authorization.cancel(), PressCancel::AlreadyIssued);
    let _ = barrier.release();
    assert_eq!(worker.join().expect("join"), GracefulOutcome::Requested);
    assert_eq!(f.backend.graceful_requests(), vec![key]);
}

#[test]
fn the_press_authorization_commit_is_one_shot_in_both_orders() {
    let f = fixture(BUDGET);
    let (press, _) = press(&f, "com.example.a", 100);
    let auth = press.authorization.clone();
    assert!(auth.commit_send());
    assert!(!auth.commit_send(), "a second send is impossible");
    assert_eq!(auth.cancel(), PressCancel::AlreadyIssued);
    assert!(!auth.is_cancelled());

    let cancelled = PressAuthorization::new(PressId::new(3), press.id, press.accepted);
    assert_eq!(cancelled.cancel(), PressCancel::Cancelled);
    assert_eq!(cancelled.cancel(), PressCancel::Cancelled, "idempotent");
    assert!(!cancelled.commit_send());
    assert!(cancelled.is_cancelled() && !cancelled.is_issued());
    assert_eq!(cancelled.press(), PressId::new(3));
    assert_eq!(cancelled.application(), press.id);
    assert_eq!(cancelled.accepted(), press.accepted);
}

#[test]
fn a_timeout_after_issuance_is_uncertain_and_never_sends_twice() {
    let f = fixture(Duration::from_millis(100));
    f.executor.set_on_main(true);
    let (press, key) = press(&f, "com.example.a", 100);
    let barrier = f.backend.block_next_graceful_after_issue();
    f.executor.set_on_main(false);
    let caller = spawn_quit(&f, &press);
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
    assert_eq!(outcome, GracefulOutcome::Uncertain(ApplicationError::TimedOut));
    assert!(
        !outcome.proves_no_send(),
        "an issued request is never proven unsent"
    );
    assert_ne!(outcome, GracefulOutcome::Refused);
    // The late native return changes nothing and nothing is retried or forced.
    let _ = barrier.release();
    drainer.join().expect("drainer");
    assert_eq!(f.backend.graceful_requests(), vec![key]);
    assert!(f.backend.termination_requests().is_empty());
    assert_eq!(f.backend.graceful_attempts(), 1);
}

#[test]
fn an_abandoned_request_before_issuance_proves_no_send() {
    let f = fixture(Duration::from_millis(30));
    let (press, _) = press(&f, "com.example.a", 100);
    f.executor.set_on_main(false);
    let outcome = quit(&f, &press);
    assert_eq!(outcome, GracefulOutcome::NotIssued(ApplicationError::TimedOut));
    assert!(outcome.proves_no_send());
    // The queued job runs late and must stay inert.
    f.executor.set_on_main(true);
    f.executor.drain();
    assert!(f.backend.graceful_requests().is_empty());
    assert!(!press.authorization.is_issued());
}

#[test]
fn a_rejected_dispatch_proves_no_send() {
    let f = fixture(BUDGET);
    let (press, _) = press(&f, "com.example.a", 100);
    f.executor.reject(true);
    let outcome = quit(&f, &press);
    f.executor.reject(false);
    assert!(matches!(
        outcome,
        GracefulOutcome::NotIssued(ApplicationError::Dispatch(_))
    ));
    assert!(f.backend.graceful_requests().is_empty());
}

#[test]
fn a_preissuance_query_failure_is_not_issued() {
    let f = fixture(BUDGET);
    let (press, _) = press(&f, "com.example.a", 100);
    f.backend.fail_next_query();
    let outcome = quit(&f, &press);
    assert_eq!(
        outcome,
        GracefulOutcome::NotIssued(ApplicationError::Unavailable)
    );
    assert!(f.backend.graceful_requests().is_empty());
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
