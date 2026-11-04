//! Generation, acknowledgement, cancellation and commit tests for application
//! control.
//!
//! Every test injects [`ManualExecutor`] and [`FakeApplicationBackend`], so no
//! test observes, opens or terminates a real application.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use stache_sdk::{CancellationToken, MainThreadExecutor, ThreadError};

use super::testing::{FakeApplicationBackend, ManualExecutor};
use super::*;

fn harness(
    on_main: bool,
    budget: Duration,
) -> (
    ApplicationControl,
    Arc<FakeApplicationBackend>,
    Arc<ManualExecutor>,
) {
    let executor = Arc::new(ManualExecutor::new(on_main));
    let backend = Arc::new(FakeApplicationBackend::new());
    let control = ApplicationControl::with_acknowledgement_budget(
        Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
        Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
        budget,
    );
    (control, backend, executor)
}

fn wait_for_queue(executor: &ManualExecutor, at_least: usize) {
    for _ in 0..500 {
        if executor.queued() >= at_least {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the executor queue never reached {at_least}");
}

fn wait_for_observations(backend: &FakeApplicationBackend, at_least: usize) {
    for _ in 0..500 {
        if backend.observations() >= at_least {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the backend never reached {at_least} observations");
}

/// The observable part of an [`OpenCompletion`], safe to compare across tests.
#[derive(Debug, PartialEq)]
enum Seen {
    Current(u64, u64, Result<(), ApplicationError>),
    Stale(u64, u64, Result<(), ApplicationError>),
}

fn record(events: &Arc<Mutex<Vec<Seen>>>) -> impl FnOnce(OpenCompletion) + Send + 'static {
    let events = Arc::clone(events);
    move |completion| {
        events.lock().expect("events").push(match completion {
            OpenCompletion::Current(context, result) => {
                Seen::Current(context.generation, context.request, result)
            }
            OpenCompletion::Stale(context, result) => {
                Seen::Stale(context.generation, context.request, result)
            }
        });
    }
}

#[test]
fn install_and_stop_are_acknowledged_and_idempotent() {
    let (control, backend, executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    assert_eq!(backend.observations(), 1);
    assert_eq!(executor.queued(), 0);
    observation.stop().expect("stop");
    assert_eq!(backend.observations(), 0);
    observation.stop().expect("repeated stop");
    assert_eq!(backend.observations(), 0);
}

#[test]
fn off_main_install_queues_without_blocking_and_completes_on_main() {
    let (control, backend, executor) = harness(false, Duration::from_secs(1));
    let handle = std::thread::spawn(move || control.install(&CancellationToken::new(), |_| {}));
    wait_for_queue(&executor, 1);
    assert_eq!(backend.observations(), 0, "nothing may install off-main");
    executor.set_on_main(true);
    executor.drain();
    let observation = handle.join().expect("join").expect("install");
    assert_eq!(backend.observations(), 1);
    drop(observation);
    assert_eq!(backend.observations(), 0);
}

#[test]
fn a_missing_acknowledgement_is_bounded_and_a_late_install_is_inert() {
    let (control, backend, executor) = harness(false, Duration::from_millis(20));
    let error = control
        .install(&CancellationToken::new(), |_| {})
        .expect_err("no acknowledgement");
    assert_eq!(error, ApplicationError::TimedOut);
    assert_eq!(backend.observations(), 0);
    executor.set_on_main(true);
    executor.drain();
    assert_eq!(backend.observations(), 0, "a late install must stay inert");
    let observation = control
        .install(&CancellationToken::new(), |_| {})
        .expect("a failed generation can be replaced");
    assert_eq!(backend.observations(), 1);
    drop(observation);
}

#[test]
fn cancellation_before_acknowledgement_never_resurrects_observation() {
    let (control, backend, executor) = harness(false, Duration::from_secs(1));
    let token = CancellationToken::new();
    let cancelled = token.clone();
    let handle = std::thread::spawn(move || control.install(&cancelled, |_| {}));
    wait_for_queue(&executor, 1);
    token.cancel();
    executor.set_on_main(true);
    executor.drain();
    assert_eq!(
        handle.join().expect("join").expect_err("cancelled"),
        ApplicationError::Cancelled
    );
    assert_eq!(backend.observations(), 0);
}

#[test]
fn a_queued_install_observes_cancellation_without_draining_main() {
    let (control, backend, executor) = harness(false, Duration::from_secs(5));
    let token = CancellationToken::new();
    let cancelled = token.clone();
    let started = Instant::now();
    let handle = std::thread::spawn(move || control.install(&cancelled, |_| {}));
    wait_for_queue(&executor, 1);
    token.cancel();
    let error = handle.join().expect("join").expect_err("cancelled");
    let elapsed = started.elapsed();
    assert_eq!(error, ApplicationError::Cancelled);
    assert!(
        elapsed < Duration::from_secs(1),
        "cancellation must not wait the acknowledgement budget: {elapsed:?}"
    );
    assert_eq!(backend.observations(), 0);
    executor.set_on_main(true);
    executor.drain();
    assert_eq!(backend.observations(), 0, "the late job must stay inert");
}

#[test]
fn cancellation_during_registration_rolls_back_the_exact_generation() {
    let (control, backend, executor) = harness(false, Duration::from_secs(5));
    let barrier = backend.block_next_observe();
    let token = CancellationToken::new();
    let cancelled = token.clone();
    let install = std::thread::spawn(move || control.install(&cancelled, |_| {}));
    wait_for_queue(&executor, 1);
    let drain_executor = Arc::clone(&executor);
    let drainer = std::thread::spawn(move || {
        drain_executor.set_on_main(true);
        drain_executor.drain();
    });
    wait_for_observations(&backend, 1);
    token.cancel();
    let _ = barrier.release();
    drainer.join().expect("drainer");
    assert_eq!(
        install.join().expect("install").expect_err("cancelled"),
        ApplicationError::Cancelled
    );
    assert_eq!(
        backend.observations(),
        0,
        "a cancellation after registration must roll the exact generation back"
    );
}

#[test]
fn stop_still_removes_observation_after_lifecycle_cancellation() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let token = CancellationToken::new();
    let observation = control.install(&token, |_| {}).expect("install");
    token.cancel();
    observation.stop().expect("stop ignores the cancelled lifecycle token");
    assert_eq!(backend.observations(), 0);
}

#[test]
fn rejected_dispatch_and_an_unavailable_run_loop_are_reported() {
    let (control, backend, executor) = harness(false, Duration::from_millis(20));
    executor.reject(true);
    let error = control.install(&CancellationToken::new(), |_| {}).expect_err("rejected");
    assert!(matches!(error, ApplicationError::Dispatch(_)), "{error}");
    assert_eq!(backend.observations(), 0);
    assert_eq!(executor.queued(), 0);
}

#[test]
fn a_partial_install_is_rolled_back_in_dispatch_order() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    backend.fail_next_observe();
    let error = control
        .install(&CancellationToken::new(), |_| {})
        .expect_err("registration failed");
    assert!(
        matches!(error, ApplicationError::ObservationFailed(_)),
        "{error}"
    );
    assert_eq!(backend.observations(), 0);
    backend.fail_next_observe_after_registration();
    let error = control
        .install(&CancellationToken::new(), |_| {})
        .expect_err("registration failed after installing");
    assert!(
        matches!(error, ApplicationError::ObservationFailed(_)),
        "{error}"
    );
    assert_eq!(backend.observations(), 0);
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("retry");
    assert_eq!(backend.observations(), 1);
    drop(observation);
}

#[test]
fn a_failed_install_keeps_cleanup_owned_and_a_retry_recovers() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    backend.fail_next_observe_after_registration();
    backend.fail_next_unobserve();
    let error = control
        .install(&CancellationToken::new(), |_| {})
        .expect_err("registration failed and rollback failed");
    assert!(
        matches!(error, ApplicationError::ObservationFailed(_)),
        "{error}"
    );
    assert_eq!(backend.observations(), 1, "the failed generation is still owned");

    let observation = control
        .install(&CancellationToken::new(), |_| {})
        .expect("a fresh install cleans the owned failed generation");
    assert_eq!(backend.observations(), 1);
    drop(observation);
    assert_eq!(backend.observations(), 0);
}

#[test]
fn a_rejected_cleanup_dispatch_is_actionable_instead_of_already_installed() {
    let (control, backend, executor) = harness(true, Duration::from_millis(200));
    backend.fail_next_observe_after_registration();
    backend.fail_next_unobserve();
    control
        .install(&CancellationToken::new(), |_| {})
        .expect_err("registration and rollback fail");
    assert_eq!(backend.observations(), 1);

    executor.reject(true);
    let error = control
        .install(&CancellationToken::new(), |_| {})
        .expect_err("cleanup cannot be dispatched");
    assert!(matches!(error, ApplicationError::Dispatch(_)), "{error}");
    assert_ne!(error, ApplicationError::AlreadyInstalled);

    executor.reject(false);
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("retry");
    assert_eq!(backend.observations(), 1);
    drop(observation);
}

#[test]
fn a_native_removal_error_is_surfaced_and_cleanup_can_be_retried() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    backend.fail_next_unobserve();
    let error = observation.stop().expect_err("removal failed");
    assert!(matches!(error, ApplicationError::RemovalFailed(_)), "{error}");
    assert_eq!(backend.observations(), 1, "the fake kept observation");
    observation.stop().expect("retry removes observation");
    assert_eq!(backend.observations(), 0);
}

#[test]
fn the_system_backend_refuses_off_main_calls() {
    let backend = SystemApplicationBackend;
    let handle = BackendHandle::allocate();
    assert_eq!(
        backend.running_applications(handle, "com.apple.Music").expect_err("off-main"),
        ApplicationError::NotMainThread
    );
    assert_eq!(
        backend.observe(handle, 1, Arc::new(|_| {})).expect_err("off-main"),
        ApplicationError::NotMainThread
    );
}

#[test]
fn native_ingestion_requires_the_exact_installed_generation() {
    assert!(ingestion_is_current(Some(3), 3));
    assert!(!ingestion_is_current(Some(4), 3));
    assert!(!ingestion_is_current(None, 3));
    assert!(!ingestion_is_current(Some(0), 0));
}

#[test]
fn a_deferred_notification_never_runs_inline_and_a_rejected_enqueue_drops_it() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let notify: Arc<dyn Fn(ApplicationId) + Send + Sync> = Arc::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let application = ApplicationId {
        handle: BackendHandle::allocate(),
        generation: 1,
        token: 0,
    };

    let mut queued: Option<Box<dyn FnOnce() + Send>> = None;
    defer_notification(
        |job| {
            queued = Some(job);
            Ok(())
        },
        Arc::clone(&notify),
        application,
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "enqueueing a delivery must not run the sink"
    );
    queued.expect("the delivery was queued")();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    defer_notification(|_| Err(ThreadError::GroupStopped), notify, application);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a rejected enqueue drops the notification instead of running it inline"
    );
}

#[test]
fn a_timed_out_stop_stays_invalid_and_can_be_retried() {
    let (control, backend, executor) = harness(true, Duration::from_millis(20));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    executor.set_on_main(false);
    assert_eq!(
        observation.stop().expect_err("no acknowledgement"),
        ApplicationError::TimedOut
    );
    assert_eq!(backend.observations(), 1, "removal is still queued");
    executor.set_on_main(true);
    executor.drain();
    assert_eq!(backend.observations(), 0);
    observation.stop().expect("retry acknowledges cleanup");
}

#[test]
fn failed_cleanup_leaves_callbacks_inert() {
    let (control, backend, executor) = harness(true, Duration::from_millis(20));
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let observation = control
        .install(&CancellationToken::new(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("install");
    let key = backend.add_running("com.apple.Music");
    executor.reject(true);
    assert!(observation.stop().is_err(), "dispatch is rejected");
    let _ = backend.launch(key);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "an invalid generation must ignore late notifications"
    );
    executor.reject(false);
    observation.stop().expect("cleanup retry");
    assert_eq!(backend.observations(), 0);
}

#[test]
fn queued_termination_is_cancelled_by_stop() {
    let (control, backend, executor) = harness(true, Duration::from_millis(20));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let _ = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    executor.set_on_main(false);
    assert_eq!(
        observation.terminate(accepted, application).expect_err("queued"),
        ApplicationError::TimedOut
    );
    assert_eq!(
        observation.stop().expect_err("queued"),
        ApplicationError::TimedOut
    );
    executor.set_on_main(true);
    executor.drain();
    assert!(
        backend.termination_requests().is_empty(),
        "a stale queued termination must not run"
    );
    assert_eq!(backend.observations(), 0);
    observation.stop().expect("cleanup acknowledged");
}

#[test]
fn queued_open_is_cancelled_by_stop() {
    let (control, backend, executor) = harness(true, Duration::from_millis(20));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let accepted = observation.accept().expect("accept");
    let completed = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&completed);
    executor.set_on_main(false);
    assert_eq!(
        observation
            .open(accepted, Path::new("/Applications/Spotify.app"), move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect_err("queued"),
        ApplicationError::TimedOut
    );
    assert_eq!(
        observation.stop().expect_err("queued"),
        ApplicationError::TimedOut
    );
    executor.set_on_main(true);
    executor.drain();
    assert!(backend.open_requests().is_empty());
    assert_eq!(completed.load(Ordering::SeqCst), 0);
    observation.stop().expect("cleanup acknowledged");
}

#[test]
fn a_timed_out_queued_termination_never_runs_without_stop_or_reconfigure() {
    let (control, backend, executor) = harness(false, Duration::from_millis(20));
    executor.set_on_main(true);
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let _ = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    executor.set_on_main(false);
    assert_eq!(
        observation.terminate(accepted, application).expect_err("queued"),
        ApplicationError::TimedOut
    );
    executor.set_on_main(true);
    executor.drain();
    assert!(
        backend.termination_requests().is_empty(),
        "an abandoned request must never issue a native action"
    );
    observation.stop().expect("cleanup");
}

#[test]
fn a_timed_out_queued_open_never_opens_without_reconfigure() {
    let (control, backend, executor) = harness(false, Duration::from_millis(20));
    executor.set_on_main(true);
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let accepted = observation.accept().expect("accept");
    let completed = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&completed);
    executor.set_on_main(false);
    assert_eq!(
        observation
            .open(accepted, Path::new("/Applications/Spotify.app"), move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect_err("queued"),
        ApplicationError::TimedOut
    );
    executor.set_on_main(true);
    executor.drain();
    assert!(backend.open_requests().is_empty());
    assert_eq!(completed.load(Ordering::SeqCst), 0);
    observation.stop().expect("cleanup");
}

#[test]
fn an_open_prepared_under_an_accepted_revision_is_not_issued_after_a_newer_one() {
    let (control, backend, _executor) = harness(true, Duration::from_secs(5));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let accepted = observation.accept().expect("accept");
    let completed = Arc::new(AtomicUsize::new(0));
    // This barrier models native path and configuration preparation that has
    // not issued anything yet.
    let barrier = backend.block_next_open();
    std::thread::scope(|scope| {
        let observation = &observation;
        let completed = Arc::clone(&completed);
        let open = scope.spawn(move || {
            observation.open(accepted, Path::new("/Applications/Spotify.app"), move |_| {
                completed.fetch_add(1, Ordering::SeqCst);
            })
        });
        assert!(
            barrier.wait_entered(Duration::from_secs(2)),
            "open must reach the native preparation"
        );
        // The accepted target changes while the native preparation is parked.
        let _newer = observation.accept().expect("accept while preparation is parked");
        let _ = barrier.release();
        assert_eq!(
            open.join().expect("open").expect("result"),
            OpenRequest::Stale,
            "an obsolete target must not be opened after the accepted target changed"
        );
    });
    assert!(
        backend.open_requests().is_empty(),
        "nothing may reach the native send"
    );
    assert_eq!(completed.load(Ordering::SeqCst), 0, "no completion may run");
}

#[test]
fn a_stop_before_native_issuance_keeps_a_prepared_open_inert() {
    let (control, backend, _executor) = harness(true, Duration::from_secs(5));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let accepted = observation.accept().expect("accept");
    let completed = Arc::new(AtomicUsize::new(0));
    let barrier = backend.block_next_open();
    std::thread::scope(|scope| {
        let observation = &observation;
        let completed = Arc::clone(&completed);
        let open = scope.spawn(move || {
            observation.open(accepted, Path::new("/Applications/Spotify.app"), move |_| {
                completed.fetch_add(1, Ordering::SeqCst);
            })
        });
        assert!(
            barrier.wait_entered(Duration::from_secs(2)),
            "open must reach the native preparation"
        );
        observation.stop().expect("stop while native preparation is still parked");
        let _ = barrier.release();
        assert_eq!(
            open.join().expect("open").expect("result"),
            OpenRequest::Stale,
            "a request invalidated before the native send must not be issued"
        );
    });
    assert!(backend.open_requests().is_empty());
    assert_eq!(completed.load(Ordering::SeqCst), 0);
    assert_eq!(backend.observations(), 0);
}

#[test]
fn a_timed_out_termination_that_already_issued_may_finish_after_the_timeout() {
    let (control, backend, executor) = harness(false, Duration::from_millis(100));
    executor.set_on_main(true);
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let key = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    let barrier = backend.block_next_terminate_after_issue();
    executor.set_on_main(false);
    let (tx, rx) = std::sync::mpsc::channel();
    let terminate = std::thread::spawn(move || {
        let result = observation.terminate(accepted, application);
        let _ = tx.send(result.clone());
        result
    });
    wait_for_queue(&executor, 1);
    let drainer = {
        let executor = Arc::clone(&executor);
        std::thread::spawn(move || {
            executor.set_on_main(true);
            executor.drain();
        })
    };
    assert!(
        barrier.wait_entered(Duration::from_secs(2)),
        "the request must be issued before the fake native call blocks"
    );
    let timed_out = rx.recv_timeout(Duration::from_secs(2)).expect("terminate returns");
    assert_eq!(
        timed_out.expect_err("timed out after issue"),
        ApplicationError::TimedOut
    );
    let _ = barrier.release();
    drainer.join().expect("drainer");
    let _ = terminate.join().expect("terminate");
    assert_eq!(
        backend.termination_requests(),
        vec![key],
        "already issued work may finish after the acknowledgement timeout"
    );
}

#[test]
fn a_stop_before_native_issuance_keeps_a_queued_termination_inert() {
    let (control, backend, _executor) = harness(true, Duration::from_secs(5));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let _ = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    // This barrier models native preflight that has not issued anything yet.
    let barrier = backend.block_next_terminate();
    std::thread::scope(|scope| {
        let terminate = scope.spawn(|| observation.terminate(accepted, application));
        assert!(
            barrier.wait_entered(Duration::from_secs(2)),
            "termination must reach the native preflight"
        );
        // Both acceptance and stop linearize while the backend prepares.
        let _newer = observation.accept().expect("accept");
        observation.stop().expect("stop while native preparation is still parked");
        let _ = barrier.release();
        assert_eq!(
            terminate.join().expect("terminate").expect("result"),
            TerminateOutcome::Stale,
            "a request invalidated before the native send must not be issued"
        );
    });
    assert!(
        backend.termination_requests().is_empty(),
        "no native termination may be issued after a completed stop"
    );
    assert_eq!(backend.observations(), 0);
}

#[test]
fn an_already_issued_termination_finishes_after_a_concurrent_stop() {
    let (control, backend, _executor) = harness(true, Duration::from_secs(5));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let key = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    // This barrier is after the irreversible send; the request is accepted by
    // the fake system here.
    let barrier = backend.block_next_terminate_after_issue();
    std::thread::scope(|scope| {
        let terminate = scope.spawn(|| observation.terminate(accepted, application));
        assert!(
            barrier.wait_entered(Duration::from_secs(2)),
            "termination must reach the native send"
        );
        let _newer = observation.accept().expect("accept");
        observation.stop().expect("stop after the request was issued");
        let _ = barrier.release();
        assert_eq!(
            terminate.join().expect("terminate").expect("request"),
            TerminateOutcome::Requested,
            "an already issued request cannot be undone by a later stop"
        );
    });
    assert_eq!(backend.termination_requests(), vec![key]);
    assert_eq!(backend.observations(), 0);
}

#[test]
fn invalidation_is_prompt_while_a_native_send_holds_the_gate() {
    let (control, backend, _executor) = harness(true, Duration::from_secs(5));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let _ = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    // This barrier is inside the irreversible native send, where the control
    // holds the commit gate.
    let barrier = backend.block_next_terminate_issuance();
    std::thread::scope(|scope| {
        let terminate = scope.spawn(|| observation.terminate(accepted, application));
        assert!(
            barrier.wait_entered(Duration::from_secs(2)),
            "the termination must hold the commit gate inside the native send"
        );
        let started = Instant::now();
        observation.invalidate();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "invalidation must not wait for the stalled send: {:?}",
            started.elapsed()
        );
        assert!(!observation.is_active(), "the generation is inert immediately");
        // A queued action rechecks the generation at issuance. The permit is
        // that exact decision and must refuse without taking the gate.
        let request = RequestCommit::new(Instant::now() + Duration::from_secs(5));
        let permit = observation.state.issuance(observation.generation, accepted, &request, None);
        assert!(
            permit.issue(|| ()).is_none(),
            "nothing may be issued after invalidation"
        );
        let _ = barrier.release();
        assert_eq!(
            terminate.join().expect("terminate").expect("result"),
            TerminateOutcome::Requested,
            "work already issued finishes inert"
        );
    });
    let stale = observation.accept().expect("accept");
    assert_eq!(stale.revision, 0, "a later acceptance yields a stale token");
    assert_eq!(backend.termination_requests().len(), 1);
    observation.stop().expect("cleanup");
}

#[test]
fn a_queued_open_after_invalidation_never_reaches_the_native_send() {
    let (control, backend, executor) = harness(true, Duration::from_millis(20));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let accepted = observation.accept().expect("accept");
    let completed = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&completed);
    executor.set_on_main(false);
    assert_eq!(
        observation
            .open(accepted, Path::new("/Applications/Spotify.app"), move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .expect_err("queued"),
        ApplicationError::TimedOut
    );
    observation.invalidate();
    executor.set_on_main(true);
    executor.drain();
    assert!(
        backend.open_requests().is_empty(),
        "an invalidated generation must refuse the queued issuance"
    );
    assert_eq!(completed.load(Ordering::SeqCst), 0, "no completion may run");
    observation.stop().expect("cleanup");
}

#[test]
fn a_newer_accepted_revision_keeps_a_queued_action_inert() {
    let (control, backend, executor) = harness(true, Duration::from_secs(5));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let _ = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    executor.set_on_main(false);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| observation.terminate(accepted, application));
        wait_for_queue(&executor, 1);
        let _newer = observation.accept().expect("accept");
        executor.set_on_main(true);
        executor.drain();
        assert_eq!(
            worker.join().expect("worker").expect("result"),
            TerminateOutcome::Stale
        );
    });
    assert!(backend.termination_requests().is_empty());
    observation.stop().expect("cleanup");
}

#[test]
fn query_failures_are_distinguished_from_absence() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let _ = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];

    backend.fail_next_query();
    assert_eq!(
        observation.running_applications("com.apple.Music").expect_err("query failure"),
        ApplicationError::Unavailable
    );
    backend.fail_next_query();
    assert_eq!(
        observation.is_running(application).expect_err("query failure"),
        ApplicationError::Unavailable
    );
    backend.fail_next_query();
    assert_eq!(
        observation.bundle_identifier(application).expect_err("query failure"),
        ApplicationError::Unavailable
    );

    assert_eq!(
        observation.running_applications("com.apple.Music").expect("snapshot"),
        vec![application],
        "an injected failure must not be reported as an empty result"
    );
    assert!(
        observation.is_running(application).expect("query"),
        "nor as a not-running answer"
    );
    assert_eq!(
        observation.bundle_identifier(application).expect("query"),
        Some("com.apple.Music".to_owned()),
        "nor as an unknown instance"
    );
}

#[test]
fn a_termination_preflight_query_failure_prevents_the_native_request() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let _ = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    backend.fail_next_query();
    assert_eq!(
        observation.terminate(accepted, application).expect_err("preflight failure"),
        ApplicationError::Unavailable,
        "a failed identity preflight is a proven error, not a not-running answer"
    );
    assert!(
        backend.termination_requests().is_empty(),
        "no native request may follow a failed preflight"
    );
    assert_eq!(
        observation.terminate(accepted, application).expect("request"),
        TerminateOutcome::Requested,
        "the same accepted token still issues once the query succeeds"
    );
    assert_eq!(backend.termination_requests().len(), 1);
}

#[test]
fn termination_acceptance_is_not_confirmed_exit() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let key = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    assert_eq!(
        observation.terminate(accepted, application).expect("request"),
        TerminateOutcome::Requested
    );
    assert!(
        observation.is_running(application).expect("query"),
        "an accepted request does not prove exit"
    );
    assert_eq!(backend.termination_requests(), vec![key]);
    backend.terminate(key);
    assert!(!observation.is_running(application).expect("query"));
    assert_eq!(
        observation.terminate(accepted, application).expect("query"),
        TerminateOutcome::NotRunning
    );
    assert_eq!(backend.termination_requests(), vec![key]);
}

#[test]
fn a_reused_pid_never_addresses_the_replacement_instance() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let first_key = backend.add_running_with_pid("com.apple.Music", 42);
    let first = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    backend.terminate(first_key);
    let second_key = backend.add_running_with_pid("com.apple.Music", 42);
    assert_eq!(backend.pid(second_key), Some(42));
    let second = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    assert_ne!(first, second, "a replacement instance gets a new token");
    let accepted = observation.accept().expect("accept");
    assert_eq!(
        observation.terminate(accepted, first).expect("query"),
        TerminateOutcome::NotRunning
    );
    assert!(backend.termination_requests().is_empty());
    assert_eq!(
        observation.terminate(accepted, second).expect("request"),
        TerminateOutcome::Requested
    );
    assert_eq!(backend.termination_requests(), vec![second_key]);
}

#[test]
fn termination_refusal_is_reported() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let _ = backend.add_running("com.apple.Music");
    backend.refuse_terminate();
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    assert_eq!(
        observation
            .terminate(observation.accept().expect("accept"), application)
            .expect("request"),
        TerminateOutcome::Refused
    );
}

#[test]
fn accepted_tokens_are_revisioned_and_stale_tokens_do_not_act() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let key = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let old = observation.accept().expect("accept");
    let current = observation.accept().expect("accept");
    assert!(!observation.is_current(old));
    assert!(observation.is_current(current));
    assert_eq!(
        observation.terminate(old, application).expect("query"),
        TerminateOutcome::Stale
    );
    assert!(backend.termination_requests().is_empty());
    assert_eq!(
        observation.terminate(current, application).expect("request"),
        TerminateOutcome::Requested
    );
    assert_eq!(backend.termination_requests(), vec![key]);
}

#[test]
fn a_stale_accept_cannot_mutate_a_successor_generation() {
    let (control, _backend, _executor) = harness(true, Duration::from_millis(200));
    let first = control.install(&CancellationToken::new(), |_| {}).expect("first");
    let first_token = first.accept().expect("accept");
    let first_generation = first.generation();
    first.stop().expect("first stop");

    let second = control.install(&CancellationToken::new(), |_| {}).expect("second");
    let second_token = second.accept().expect("accept");
    assert!(second.is_current(second_token));

    let stale = first.accept().expect("accept");
    assert_eq!(stale.generation, first_generation);
    assert!(!first.is_current(stale));
    assert!(!first.is_current(first_token));
    assert!(
        second.is_current(second_token),
        "accepting on a stopped observation must not bump the successor revision"
    );
}

#[test]
fn cross_control_tokens_and_instances_never_address_another_control() {
    let executor = Arc::new(ManualExecutor::new(true));
    let backend = Arc::new(FakeApplicationBackend::new());
    let control = |backend: &Arc<FakeApplicationBackend>| {
        ApplicationControl::with_acknowledgement_budget(
            Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
            Arc::clone(backend) as Arc<dyn ApplicationBackend>,
            Duration::from_millis(200),
        )
    };
    let first = control(&backend);
    let second = control(&backend);
    let first_observation = first.install(&CancellationToken::new(), |_| {}).expect("first");
    let second_observation = second.install(&CancellationToken::new(), |_| {}).expect("second");
    assert_eq!(
        first_observation.generation(),
        second_observation.generation(),
        "independent controls share numeric generations"
    );

    let key = backend.add_running("com.apple.Music");
    let application =
        second_observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let first_token = first_observation.accept().expect("accept");
    let second_token = second_observation.accept().expect("accept");

    assert_eq!(
        first_observation.terminate(first_token, application).expect("foreign instance"),
        TerminateOutcome::NotRunning
    );
    assert_eq!(
        second_observation.terminate(first_token, application).expect("foreign token"),
        TerminateOutcome::Stale
    );
    assert!(backend.termination_requests().is_empty());

    assert_eq!(
        second_observation.terminate(second_token, application).expect("own token"),
        TerminateOutcome::Requested
    );
    assert_eq!(backend.termination_requests(), vec![key]);
}

#[test]
fn an_open_completion_after_stop_is_stale_and_diagnostic_only() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let generation = observation.generation();
    let events = Arc::new(Mutex::new(Vec::new()));
    let request = observation
        .open(
            observation.accept().expect("accept"),
            Path::new("/Applications/Spotify.app"),
            record(&events),
        )
        .expect("open");
    assert_eq!(request, OpenRequest::Issued);
    observation.stop().expect("stop");
    assert!(backend.complete_open(0, Ok(())));
    assert_eq!(
        *events.lock().expect("events"),
        vec![Seen::Stale(generation, 1, Ok(()))],
        "a completion after stop must be reported as stale, not current"
    );

    let successor = control.install(&CancellationToken::new(), |_| {}).expect("successor");
    assert_eq!(
        *events.lock().expect("events"),
        vec![Seen::Stale(generation, 1, Ok(()))],
        "delivering a stale completion must not queue successor work"
    );
    drop(successor);
}

#[test]
fn open_completion_failure_is_surfaced_while_current() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let events = Arc::new(Mutex::new(Vec::new()));
    observation
        .open(
            observation.accept().expect("accept"),
            Path::new("/Applications/Spotify.app"),
            record(&events),
        )
        .expect("open");
    let generation = observation.generation();
    assert!(backend.complete_open(0, Err("Spotify is unavailable".to_owned())));
    assert_eq!(*events.lock().expect("events"), vec![Seen::Current(
        generation,
        1,
        Err(ApplicationError::OpenFailed("Spotify is unavailable".to_owned()))
    )]);
}

#[test]
fn a_completion_is_stale_after_a_newer_accepted_revision() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let generation = observation.generation();
    let events = Arc::new(Mutex::new(Vec::new()));
    observation
        .open(
            observation.accept().expect("accept"),
            Path::new("/Applications/Spotify.app"),
            record(&events),
        )
        .expect("open");
    let _newer = observation.accept().expect("accept");
    assert!(backend.complete_open(0, Ok(())));
    assert_eq!(*events.lock().expect("events"), vec![Seen::Stale(
        generation,
        1,
        Ok(())
    )]);
}

#[test]
fn an_older_completion_is_stale_while_a_newer_open_is_pending() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let generation = observation.generation();
    let events = Arc::new(Mutex::new(Vec::new()));
    let accepted = observation.accept().expect("accept");
    observation
        .open(accepted, Path::new("/Applications/Spotify.app"), record(&events))
        .expect("first open");
    observation
        .open(accepted, Path::new("/Applications/TIDAL.app"), record(&events))
        .expect("second open");
    assert_eq!(backend.pending_opens(), 2);

    assert!(backend.complete_open(0, Ok(())));
    assert_eq!(
        *events.lock().expect("events"),
        vec![Seen::Stale(generation, 1, Ok(()))],
        "the older completion cannot supersede the pending newer request"
    );
    assert_eq!(backend.pending_opens(), 1, "the newer request stays pending");

    assert!(backend.complete_open(1, Ok(())));
    assert_eq!(*events.lock().expect("events"), vec![
        Seen::Stale(generation, 1, Ok(())),
        Seen::Current(generation, 2, Ok(())),
    ]);
}

#[test]
fn a_synchronous_open_completion_is_current_and_does_not_deadlock() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    backend.complete_opens_synchronously();
    let events = Arc::new(Mutex::new(Vec::new()));
    let request = observation
        .open(
            observation.accept().expect("accept"),
            Path::new("/Applications/Spotify.app"),
            record(&events),
        )
        .expect("open");
    assert_eq!(request, OpenRequest::Issued);
    assert_eq!(*events.lock().expect("events"), vec![Seen::Current(
        observation.generation(),
        1,
        Ok(())
    )]);
}

#[test]
fn a_rejected_completion_delivery_drops_the_result_without_panicking() {
    let (control, backend, executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let events = Arc::new(Mutex::new(Vec::new()));
    observation
        .open(
            observation.accept().expect("accept"),
            Path::new("/Applications/Spotify.app"),
            record(&events),
        )
        .expect("open");
    executor.reject(true);
    assert!(backend.complete_open(0, Ok(())));
    assert_eq!(
        events.lock().expect("events").len(),
        0,
        "a rejected delivery is lost; see NativeOpenCompletion"
    );
    assert_eq!(backend.pending_opens(), 0);
    executor.reject(false);
}

#[test]
fn an_immediate_open_failure_is_reported_without_a_completion() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let completed = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&completed);
    backend.fail_next_open();
    let error = observation
        .open(
            observation.accept().expect("accept"),
            Path::new("/Applications/Spotify.app"),
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        )
        .expect_err("open failed");
    assert!(matches!(error, ApplicationError::OpenFailed(_)), "{error}");
    assert_eq!(completed.load(Ordering::SeqCst), 0);
}

#[test]
fn a_panicking_notification_callback_is_contained() {
    let (control, backend, executor) = harness(true, Duration::from_millis(200));
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let observation = control
        .install(&CancellationToken::new(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            panic!("expected test panic");
        })
        .expect("install");
    let key = backend.add_running("com.apple.Music");
    assert_eq!(backend.launch(key), 1);
    assert_eq!(backend.launch(key), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // The sink owns the containment, so a deferred delivery is contained too.
    backend.defer_launches(Arc::clone(&executor));
    assert_eq!(backend.launch(key), 1);
    assert_eq!(executor.drain(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(backend.observations(), 1);
    drop(observation);
}

#[test]
fn a_notification_during_a_native_send_is_deferred_until_after_issuance() {
    let (control, backend, executor) = harness(true, Duration::from_millis(200));
    let seen: Arc<Mutex<Option<ApplicationId>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&seen);
    let observation = control
        .install(&CancellationToken::new(), move |application| {
            *slot.lock().expect("slot") = Some(application);
        })
        .expect("install");
    let key = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];

    backend.defer_launches(Arc::clone(&executor));
    // `forceTerminate` synchronously delivers a will-launch notification while
    // the issuance permit still holds the commit gate.
    backend.launch_during_next_terminate_send(key);
    let accepted = observation.accept().expect("accept");
    assert_eq!(
        observation.terminate(accepted, application).expect("terminate"),
        TerminateOutcome::Requested
    );
    assert_eq!(backend.termination_requests(), vec![key]);
    assert!(
        seen.lock().expect("slot").is_none(),
        "the sink must not run inside the native send"
    );
    assert_eq!(executor.queued(), 1, "the notification is queued instead");

    // Only after the send has committed does the deferred delivery run.
    assert_eq!(executor.drain(), 1);
    let application = seen.lock().expect("slot").expect("deferred notification");
    assert_eq!(
        observation.bundle_identifier(application).expect("query"),
        Some("com.apple.Music".to_owned())
    );
    assert!(observation.is_current(observation.accepted()));
    observation.stop().expect("stop after the send");
    assert_eq!(backend.observations(), 0);
}

#[test]
fn a_deferred_notification_is_suppressed_by_stop_and_leaves_a_successor_untouched() {
    let (control, backend, executor) = harness(true, Duration::from_millis(200));
    let first_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&first_calls);
    let first = control
        .install(&CancellationToken::new(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("first install");
    let key = backend.add_running("com.apple.Music");

    backend.defer_launches(Arc::clone(&executor));
    assert_eq!(backend.launch(key), 1);
    assert_eq!(first_calls.load(Ordering::SeqCst), 0, "the delivery is queued");
    first.stop().expect("stop before the delivery runs");
    assert_eq!(backend.observations(), 0);

    let second_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&second_calls);
    let second = control
        .install(&CancellationToken::new(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("second install");
    assert_eq!(backend.launch(key), 1);
    assert_eq!(executor.drain(), 2, "stale and successor deliveries are queued");
    assert_eq!(
        first_calls.load(Ordering::SeqCst),
        0,
        "a removed generation never reaches its caller"
    );
    assert_eq!(second_calls.load(Ordering::SeqCst), 1);
    assert_eq!(backend.observations(), 1);
    drop(second);
}

#[test]
fn a_panicking_open_completion_is_contained() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    observation
        .open(
            observation.accept().expect("accept"),
            Path::new("/Applications/Spotify.app"),
            |_| panic!("expected test panic"),
        )
        .expect("open");
    assert!(backend.complete_open(0, Ok(())));
    assert_eq!(backend.pending_opens(), 0, "the completion was delivered once");

    // The observation survives a panicking user callback and still reports its
    // own request ordering.
    let generation = observation.generation();
    let events = Arc::new(Mutex::new(Vec::new()));
    observation
        .open(
            observation.accept().expect("accept"),
            Path::new("/Applications/Spotify.app"),
            record(&events),
        )
        .expect("successor open");
    assert!(backend.complete_open(1, Ok(())));
    assert_eq!(*events.lock().expect("events"), vec![Seen::Current(
        generation,
        2,
        Ok(())
    )]);
    assert_eq!(backend.observations(), 1);
}

#[test]
fn a_notification_instance_reports_its_exact_bundle_identifier() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let seen: Arc<Mutex<Option<ApplicationId>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&seen);
    let observation = control
        .install(&CancellationToken::new(), move |application| {
            *slot.lock().expect("slot") = Some(application);
        })
        .expect("install");
    let key = backend.add_running("com.apple.Music");
    let _ = backend.launch(key);
    let application = seen.lock().expect("slot").expect("notification");
    assert_eq!(
        observation.bundle_identifier(application).expect("query"),
        Some("com.apple.Music".to_owned())
    );
    observation.stop().expect("stop");
    assert_eq!(
        observation.bundle_identifier(application).expect_err("stale generation"),
        ApplicationError::NotInstalled
    );
}

#[test]
fn a_captured_notification_from_a_removed_generation_cannot_pollute_a_successor() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let first_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&first_calls);
    let first = control
        .install(&CancellationToken::new(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("first install");
    let key = backend.add_running("com.apple.Music");
    let captured = backend.capture_launch(key).expect("captured native block");
    first.stop().expect("first stop");

    let second_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&second_calls);
    let second = control
        .install(&CancellationToken::new(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("second install");
    assert_eq!(backend.observations(), 1);

    assert!(
        !captured.deliver(),
        "a captured block from the removed generation must be ignored"
    );
    assert_eq!(first_calls.load(Ordering::SeqCst), 0);
    assert_eq!(second_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        backend.observations(),
        1,
        "no store or observer may be recreated"
    );

    assert_eq!(backend.launch(key), 1);
    assert_eq!(second_calls.load(Ordering::SeqCst), 1);
    drop(second);
}

#[test]
fn a_successor_generation_is_isolated_from_stale_actions_and_removal() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let first_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&first_calls);
    let first = control
        .install(&CancellationToken::new(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("first install");
    let key = backend.add_running("com.apple.Music");
    let application = first.running_applications("com.apple.Music").expect("snapshot")[0];
    let stale = first.accept().expect("accept");
    first.stop().expect("first stop");
    let second_calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&second_calls);
    let second = control
        .install(&CancellationToken::new(), move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .expect("second install");
    assert_eq!(backend.observations(), 1);
    assert_eq!(
        first.terminate(stale, application).expect("stale"),
        TerminateOutcome::Stale
    );
    assert!(backend.termination_requests().is_empty());
    assert_eq!(
        first.running_applications("com.apple.Music").expect_err("stale generation"),
        ApplicationError::NotInstalled
    );
    backend
        .unobserve(application.handle, first.generation())
        .expect("stale removal is a no-op");
    assert_eq!(backend.observations(), 1);
    assert_eq!(backend.launch(key), 1);
    assert_eq!(first_calls.load(Ordering::SeqCst), 0);
    assert_eq!(second_calls.load(Ordering::SeqCst), 1);
    drop(second);
}

#[test]
fn a_timeout_before_issuance_prevents_a_later_native_request() {
    let (control, backend, executor) = harness(false, Duration::from_millis(30));
    executor.set_on_main(true);
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let _ = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    // Native preflight enters, then outlives the caller's acknowledgement
    // budget without ever reaching the permit.
    let barrier = backend.block_next_terminate();
    executor.set_on_main(false);
    let (tx, rx) = std::sync::mpsc::channel();
    let terminate = std::thread::spawn(move || {
        let result = observation.terminate(accepted, application);
        let _ = tx.send(result.clone());
        result
    });
    wait_for_queue(&executor, 1);
    let drainer = {
        let executor = Arc::clone(&executor);
        std::thread::spawn(move || {
            executor.set_on_main(true);
            executor.drain();
        })
    };
    assert!(
        barrier.wait_entered(Duration::from_secs(2)),
        "native preflight must have entered"
    );
    let timed_out = rx.recv_timeout(Duration::from_secs(2)).expect("terminate returns");
    assert_eq!(
        timed_out.expect_err("timed out in preflight"),
        ApplicationError::TimedOut
    );
    let _ = barrier.release();
    drainer.join().expect("drainer");
    let _ = terminate.join().expect("terminate");
    assert!(
        backend.termination_requests().is_empty(),
        "an abandoned request must not issue after its deadline, even if preflight entered earlier"
    );
    assert_eq!(
        backend.observations(),
        0,
        "dropping the observation cleaned it up"
    );
}

#[test]
fn a_stalled_issuance_bounds_acceptance_and_keeps_the_old_snapshot() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(50));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let key = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    let barrier = backend.block_next_terminate_issuance();
    std::thread::scope(|scope| {
        let terminate = scope.spawn(|| observation.terminate(accepted, application));
        assert!(
            barrier.wait_entered(Duration::from_secs(2)),
            "the native send must be in flight under the gate"
        );
        assert_eq!(
            observation.accept().expect_err("the gate is held by the stalled send"),
            ApplicationError::TimedOut,
            "acceptance must not wait indefinitely on a stalled native call"
        );
        let _ = barrier.release();
        assert_eq!(
            terminate.join().expect("terminate").expect("request"),
            TerminateOutcome::Requested
        );
    });
    assert_eq!(backend.termination_requests(), vec![key]);
    assert!(
        observation.is_current(accepted),
        "a rejected acceptance alone must leave the previous snapshot current"
    );
    observation.stop().expect("cleanup after the gate is free");
}

#[test]
fn an_advisory_active_read_answers_while_a_stalled_send_holds_the_gate() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(50));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let key = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    assert!(observation.is_active(), "a live generation is active");

    let barrier = backend.block_next_terminate_issuance();
    std::thread::scope(|scope| {
        let terminate = scope.spawn(|| observation.terminate(accepted, application));
        assert!(
            barrier.wait_entered(Duration::from_secs(2)),
            "the native send must be in flight under the gate"
        );
        let (answered_tx, answered_rx) = mpsc::channel();
        let observation_ref = &observation;
        let reader = scope.spawn(move || {
            let _ = answered_tx.send(observation_ref.is_active());
        });
        let answered = answered_rx.recv_timeout(Duration::from_secs(2));
        // Release before asserting so a regression that blocks on the gate
        // cannot deadlock the scope join.
        let _ = barrier.release();
        assert_eq!(
            terminate.join().expect("terminate").expect("request"),
            TerminateOutcome::Requested
        );
        reader.join().expect("reader");
        assert_eq!(
            answered,
            Ok(true),
            "the advisory read must answer while the stalled send holds the gate"
        );
    });
    assert_eq!(backend.termination_requests(), vec![key]);
    observation.stop().expect("cleanup after the gate is free");
    assert!(!observation.is_active(), "a stopped generation is never active");
}

#[test]
fn a_failed_stop_still_invalidates_the_generation_immediately() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(50));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    let key = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");
    let barrier = backend.block_next_terminate_issuance();
    std::thread::scope(|scope| {
        let terminate = scope.spawn(|| observation.terminate(accepted, application));
        assert!(
            barrier.wait_entered(Duration::from_secs(2)),
            "the native send must be in flight under the gate"
        );
        assert_eq!(
            observation.stop().expect_err("stop is bounded too"),
            ApplicationError::TimedOut,
            "stop must not wait indefinitely on a stalled native call"
        );
        assert!(
            !observation.is_current(accepted),
            "a stopped generation is inert even though its native guard is retained"
        );
        assert!(
            !observation.is_current(observation.accepted()),
            "the published token is stale as soon as stop invalidates the generation"
        );
        let _ = barrier.release();
        assert_eq!(
            terminate.join().expect("terminate").expect("request"),
            TerminateOutcome::Requested
        );
    });
    assert_eq!(backend.termination_requests(), vec![key]);
    observation.stop().expect("cleanup after the gate is free");
    assert_eq!(backend.observations(), 0);
}

#[test]
fn a_queued_request_stays_inert_when_a_stalled_send_makes_stop_time_out() {
    let (control, backend, executor) = harness(true, Duration::from_millis(50));
    let observation =
        Arc::new(control.install(&CancellationToken::new(), |_| {}).expect("install"));
    let key = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("snapshot")[0];
    let accepted = observation.accept().expect("accept");

    // The first request reaches its irreversible send and holds the commit gate.
    let issuance = backend.block_next_terminate_issuance();
    executor.set_on_main(false);
    let first = {
        let observation = Arc::clone(&observation);
        std::thread::spawn(move || observation.terminate(accepted, application))
    };
    wait_for_queue(&executor, 1);
    let drainer = {
        let executor = Arc::clone(&executor);
        std::thread::spawn(move || {
            executor.set_on_main(true);
            executor.drain();
        })
    };
    assert!(
        issuance.wait_entered(Duration::from_secs(5)),
        "the first send must hold the gate"
    );

    // A second request is queued and parked in native preparation.
    let preflight = backend.block_next_terminate();
    let second = {
        let observation = Arc::clone(&observation);
        std::thread::spawn(move || observation.terminate(accepted, application))
    };
    assert!(
        preflight.wait_entered(Duration::from_secs(5)),
        "the second request must reach native preparation"
    );

    assert_eq!(
        control.stop().expect_err("the stalled send holds the gate"),
        ApplicationError::TimedOut,
        "invalidation itself must not wait for the stalled gate"
    );
    let _ = preflight.release();
    assert_eq!(
        second.join().expect("second terminate").expect("result"),
        TerminateOutcome::Stale,
        "a stop that timed out logically invalidates already-queued work"
    );
    let _ = issuance.release();
    let _ = first.join().expect("first terminate");
    drainer.join().expect("drainer");
    assert_eq!(
        backend.termination_requests(),
        vec![key],
        "only the request already inside the native send reached the backend"
    );

    observation.stop().expect("cleanup after the gate is free");
    assert_eq!(backend.observations(), 0);
}

#[test]
fn a_completed_install_with_a_lost_acknowledgement_keeps_an_inert_owned_guard() {
    let (control, backend, executor) = harness(false, Duration::from_millis(200));
    let observe = backend.block_next_observe();
    backend.fail_unobserves(1);
    let callbacks = Arc::new(AtomicUsize::new(0));
    let install_callbacks = Arc::clone(&callbacks);
    let install_control = control.clone();
    let install = std::thread::spawn(move || {
        install_control.install(&CancellationToken::new(), move |_| {
            install_callbacks.fetch_add(1, Ordering::SeqCst);
        })
    });
    wait_for_queue(&executor, 1);
    let releaser = {
        let observe = Arc::clone(&observe);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            observe.release()
        })
    };
    assert!(
        executor.drain_one(),
        "the queued registration must run while its acknowledgement is lost"
    );
    releaser.join().expect("releaser");
    let error = install
        .join()
        .expect("install thread")
        .expect_err("the acknowledgement was lost");
    assert_eq!(error, ApplicationError::TimedOut);
    assert_eq!(
        backend.observations(),
        1,
        "installation completed natively; the guard is owned and inert"
    );

    let key = backend.add_running("com.apple.Music");
    let stale = backend.capture_launch(key).expect("the old observer delivery");
    assert!(stale.deliver(), "the captured block is still delivered");
    assert_eq!(
        callbacks.load(Ordering::SeqCst),
        0,
        "an invalidated generation must not run its callback"
    );

    assert_eq!(
        control.stop().expect_err("the main queue is stalled"),
        ApplicationError::TimedOut,
        "bounded cleanup failure is reported truthfully"
    );
    assert_eq!(backend.observations(), 1, "the inert guard is retained");
    assert_eq!(
        control
            .install(&CancellationToken::new(), |_| {})
            .expect_err("the old guard is cleaned first"),
        ApplicationError::TimedOut,
        "a successor cannot start over an unacknowledged old guard"
    );

    executor.set_on_main(true);
    assert!(executor.drain() >= 1, "the delayed cleanup can finally run");
    assert_eq!(
        backend.observations(),
        0,
        "cleanup removed only the old generation"
    );
    control.stop().expect("nothing is owned after the delayed cleanup");

    let successor = control.install(&CancellationToken::new(), |_| {}).expect("successor");
    assert_eq!(backend.observations(), 1);
    assert!(
        !stale.deliver(),
        "a late old callback cannot reach the successor generation"
    );
    let accepted = successor.accept().expect("successor accept");
    let application = successor.running_applications("com.apple.Music").expect("query")[0];
    assert_eq!(
        successor.terminate(accepted, application).expect("terminate"),
        TerminateOutcome::Requested
    );
    successor.stop().expect("successor cleanup");
}

#[test]
fn a_late_completion_after_successor_installation_is_stale_and_leaves_successor_pending() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let first = control.install(&CancellationToken::new(), |_| {}).expect("first");
    let first_generation = first.generation();
    let events = Arc::new(Mutex::new(Vec::new()));
    first
        .open(
            first.accept().expect("accept"),
            Path::new("/Applications/Spotify.app"),
            record(&events),
        )
        .expect("first open");
    first.stop().expect("stop");

    let successor = control.install(&CancellationToken::new(), |_| {}).expect("successor");
    let successor_generation = successor.generation();
    successor
        .open(
            successor.accept().expect("accept"),
            Path::new("/Applications/TIDAL.app"),
            record(&events),
        )
        .expect("successor open");
    assert_eq!(backend.pending_opens(), 2);

    // The first request completes only after the successor is installed and has
    // its own pending open.
    assert!(backend.complete_open(0, Ok(())));
    assert_eq!(
        *events.lock().expect("events"),
        vec![Seen::Stale(first_generation, 1, Ok(()))],
        "a late completion must not become current for the successor"
    );
    assert_eq!(
        backend.pending_opens(),
        1,
        "the successor's pending open is untouched"
    );

    assert!(backend.complete_open(1, Ok(())));
    assert_eq!(*events.lock().expect("events"), vec![
        Seen::Stale(first_generation, 1, Ok(())),
        Seen::Current(successor_generation, 1, Ok(())),
    ]);
    drop(successor);
}

#[test]
fn a_synchronous_completion_may_reenter_accept_and_stop() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation =
        Arc::new(control.install(&CancellationToken::new(), |_| {}).expect("install"));
    backend.complete_opens_synchronously();
    let generation = observation.generation();
    let reentrant = Arc::clone(&observation);
    let events = Arc::new(Mutex::new(Vec::new()));
    let request = observation
        .open(
            observation.accept().expect("accept"),
            Path::new("/Applications/Spotify.app"),
            {
                let events = Arc::clone(&events);
                move |completion| {
                    events.lock().expect("events").push(match completion {
                        OpenCompletion::Current(context, result) => {
                            Seen::Current(context.generation, context.request, result)
                        }
                        OpenCompletion::Stale(context, result) => {
                            Seen::Stale(context.generation, context.request, result)
                        }
                    });
                    // Re-enter the control read/write paths while the native send
                    // is still on the stack.
                    reentrant.accept().expect("reentrant accept");
                    reentrant.stop().expect("reentrant stop");
                }
            },
        )
        .expect("open");
    assert_eq!(request, OpenRequest::Issued);
    assert_eq!(*events.lock().expect("events"), vec![Seen::Current(
        generation,
        1,
        Ok(())
    )]);
    assert_eq!(
        backend.observations(),
        0,
        "the reentrant stop removed observation"
    );
    drop(observation);
}

#[test]
fn a_weak_observer_sink_does_not_keep_the_control_or_backend_alive() {
    let executor = Arc::new(ManualExecutor::new(true));
    let backend = Arc::new(FakeApplicationBackend::new());
    let weak_backend = Arc::downgrade(&backend);
    let control = ApplicationControl::with_acknowledgement_budget(
        Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
        Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
        Duration::from_millis(200),
    );
    drop(backend);
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    drop(control);
    assert!(
        weak_backend.upgrade().is_some(),
        "the live observation still owns the control state"
    );
    drop(observation);
    assert!(
        weak_backend.upgrade().is_none(),
        "a weak observer sink must not keep the control and backend alive"
    );
}

#[test]
fn the_final_control_drop_cleans_a_partial_install_when_the_dispatcher_resumes() {
    let executor = Arc::new(ManualExecutor::new(true));
    let backend = Arc::new(FakeApplicationBackend::new());
    {
        let control = ApplicationControl::with_acknowledgement_budget(
            Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
            Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
            Duration::from_millis(200),
        );
        backend.fail_next_observe_after_registration();
        backend.fail_next_unobserve();
        control
            .install(&CancellationToken::new(), |_| {})
            .expect_err("registration and rollback fail");
        assert_eq!(backend.observations(), 1, "the failed generation stays owned");
        // The dispatcher is offline when the last external owner goes away.
        executor.set_on_main(false);
    }
    assert_eq!(
        executor.queued(),
        1,
        "the final drop queues cleanup without blocking"
    );
    assert_eq!(
        backend.observations(),
        1,
        "the failed generation is still installed"
    );
    executor.set_on_main(true);
    executor.drain();
    assert_eq!(
        backend.observations(),
        0,
        "the final drop cleanup removed the partial observer when the dispatcher resumed"
    );
}

#[test]
fn the_module_facing_handles_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ApplicationControl>();
    assert_send_sync::<ApplicationObservation>();
    assert_send_sync::<ApplicationId>();
}

#[test]
fn control_stop_cleans_a_failed_install_generation() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    control.stop().expect("nothing is owned");
    backend.fail_next_observe_after_registration();
    backend.fail_unobserves(1);
    let error = control
        .install(&CancellationToken::new(), |_| {})
        .expect_err("registration failed");
    assert!(
        matches!(error, ApplicationError::ObservationFailed(_)),
        "{error}"
    );
    assert_eq!(backend.observations(), 1, "the failed generation is still owned");

    control.stop().expect("a direct control stop acknowledges the owned cleanup");
    assert_eq!(backend.observations(), 0);
    control.stop().expect("repeated control stop");
}

#[test]
fn control_stop_reports_a_cleanup_failure_and_can_be_retried() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    backend.fail_next_observe_after_registration();
    backend.fail_unobserves(2);
    let error = control
        .install(&CancellationToken::new(), |_| {})
        .expect_err("registration and its scheduled rollback failed");
    assert!(
        matches!(error, ApplicationError::ObservationFailed(_)),
        "{error}"
    );
    assert_eq!(backend.observations(), 1);

    let error = control.stop().expect_err("the ack cleanup also fails");
    assert!(matches!(error, ApplicationError::RemovalFailed(_)), "{error}");
    assert_eq!(backend.observations(), 1, "ownership is kept for a retry");
    control.stop().expect("the retry removes the exact generation");
    assert_eq!(backend.observations(), 0);
}

#[test]
fn control_stop_before_an_install_acknowledgement_makes_the_install_inert() {
    let (control, backend, executor) = harness(false, Duration::from_secs(1));
    let install_control = control.clone();
    let install =
        std::thread::spawn(move || install_control.install(&CancellationToken::new(), |_| {}));
    wait_for_queue(&executor, 1);

    let stop_control = control.clone();
    let stop = std::thread::spawn(move || stop_control.stop());
    wait_for_queue(&executor, 2);
    executor.set_on_main(true);
    executor.drain();

    assert_eq!(
        install.join().expect("install").expect_err("cancelled"),
        ApplicationError::Cancelled
    );
    stop.join().expect("stop").expect("the queued cleanup is acknowledged");
    assert_eq!(
        backend.observations(),
        0,
        "a late install must observe the invalidated generation"
    );
    control.stop().expect("nothing is owned after the late install");
}

#[test]
fn control_stop_is_bounded_by_a_stalled_native_send() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(20));
    let observation =
        Arc::new(control.install(&CancellationToken::new(), |_| {}).expect("install"));
    let accepted = observation.accept().expect("accept");
    let _music = backend.add_running("com.apple.Music");
    let application = observation.running_applications("com.apple.Music").expect("query")[0];
    let barrier = backend.block_next_terminate_issuance();
    let terminating = Arc::clone(&observation);
    let terminate = std::thread::spawn(move || terminating.terminate(accepted, application));
    assert!(barrier.wait_entered(Duration::from_secs(5)));

    assert_eq!(
        control.stop().expect_err("the stalled send holds the gate"),
        ApplicationError::TimedOut
    );
    assert!(
        !observation.is_current(accepted),
        "a failed control stop still invalidates the published generation immediately"
    );
    assert!(barrier.release());
    assert_eq!(
        terminate.join().expect("terminate").expect("result"),
        TerminateOutcome::Requested
    );

    observation.stop().expect("the observation is still owned");
    control.stop().expect("nothing is owned after the observation stopped");
}

/// A mutable user-info dictionary typed exactly like `NSNotification.userInfo`.
fn user_info(
    entries: &[(&NSString, &AnyObject)],
) -> Retained<objc2_foundation::NSMutableDictionary<AnyObject, AnyObject>> {
    let dictionary = objc2_foundation::NSMutableDictionary::<AnyObject, AnyObject>::dictionary();
    for (key, value) in entries {
        // SAFETY: every key is a Foundation string object, which implements
        // `NSCopying`, and every value is a live Objective-C object.
        unsafe { dictionary.setObject_forKey(value, ProtocolObject::from_ref(*key)) };
    }
    dictionary
}

fn will_launch_notification(
    user_info: Option<&objc2_foundation::NSDictionary>,
) -> Retained<NSNotification> {
    // SAFETY: the notification name is an immutable AppKit constant, no object
    // is supplied, and a user-info dictionary lives for the call.
    unsafe {
        NSNotification::notificationWithName_object_userInfo(
            NSWorkspaceWillLaunchApplicationNotification,
            None,
            user_info,
        )
    }
}

#[test]
fn launch_application_rejects_missing_and_wrong_typed_user_info() {
    assert!(
        launch_application(&will_launch_notification(None)).is_none(),
        "a nil userInfo carries no application"
    );

    let empty = user_info(&[]);
    assert!(
        launch_application(&will_launch_notification(Some(&empty))).is_none(),
        "a userInfo without the application key carries no application"
    );

    let key = unsafe { NSWorkspaceApplicationKey };
    let unrelated_key = NSString::from_str("unrelated");
    let text = NSString::from_str("not an application");
    let text_object = AsRef::<AnyObject>::as_ref(&*text);
    let unrelated = user_info(&[(&unrelated_key, text_object)]);
    assert!(
        launch_application(&will_launch_notification(Some(&unrelated))).is_none(),
        "an unrelated key must not classify as an application"
    );

    let wrong_class = user_info(&[(key, text_object)]);
    assert!(
        launch_application(&will_launch_notification(Some(&wrong_class))).is_none(),
        "a value of the wrong class must be dropped instead of downcast unsafely"
    );
}

#[test]
fn the_resolution_queries_report_missing_answers_instead_of_failing() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    backend.add_installed("/Applications/Player.app", "com.example.player");

    assert_eq!(
        observation
            .bundle_identifier_at_path(Path::new("/Applications/Player.app"))
            .expect("query"),
        Some("com.example.player".to_owned()),
        "a registered path answers its identifier"
    );
    assert_eq!(
        observation
            .bundle_identifier_at_path(Path::new("/Applications/Notes.txt"))
            .expect("query"),
        None,
        "a path without an application bundle is an answer, not a failure"
    );
    assert_eq!(
        observation.application_path("com.example.player").expect("query"),
        Some(PathBuf::from("/Applications/Player.app"))
    );
    assert_eq!(
        observation.application_path("com.example.unknown").expect("query"),
        None,
        "an unknown identifier is an answer, not a failure"
    );
    observation.stop().expect("stop");
}

#[test]
fn a_resolution_query_failure_is_surfaced_as_an_error() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    let observation = control.install(&CancellationToken::new(), |_| {}).expect("install");
    backend.add_installed("/Applications/Player.app", "com.example.player");

    backend.fail_next_query();
    assert_eq!(
        observation
            .bundle_identifier_at_path(Path::new("/Applications/Player.app"))
            .expect_err("the failure is not an answer"),
        ApplicationError::Unavailable
    );
    backend.fail_next_query();
    assert_eq!(
        observation
            .application_path("com.example.player")
            .expect_err("the failure is not an answer"),
        ApplicationError::Unavailable
    );

    // The injected failure is one-shot: the healthy answer follows.
    assert_eq!(
        observation.application_path("com.example.player").expect("query"),
        Some(PathBuf::from("/Applications/Player.app"))
    );
    observation.stop().expect("stop");
}

#[test]
fn resolution_queries_are_generation_scoped() {
    let (control, backend, _executor) = harness(true, Duration::from_millis(200));
    backend.add_installed("/Applications/Player.app", "com.example.player");
    let first = control.install(&CancellationToken::new(), |_| {}).expect("first install");
    first.stop().expect("stop");

    assert_eq!(
        first
            .bundle_identifier_at_path(Path::new("/Applications/Player.app"))
            .expect_err("an invalidated generation is inert"),
        ApplicationError::NotInstalled
    );
    assert_eq!(
        first
            .application_path("com.example.player")
            .expect_err("an invalidated generation is inert"),
        ApplicationError::NotInstalled
    );

    let successor = control.install(&CancellationToken::new(), |_| {}).expect("successor");
    assert_eq!(
        successor.application_path("com.example.player").expect("query"),
        Some(PathBuf::from("/Applications/Player.app")),
        "the successor generation reads the live answer"
    );
    successor.stop().expect("stop");
}
