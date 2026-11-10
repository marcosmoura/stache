//! Host frontmost-context publisher tests (HQ-04-08). Fakes only: a
//! `FakeApplicationBackend`, a `ManualExecutor` and an unavailable input
//! service whose publisher cell is read back directly.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use stache_macos::{ApplicationBackend, FakeAppKey, FakeApplicationBackend, ManualExecutor};
use stache_sdk::input::{ExemptionFlags, FrontmostState, InputService};

use super::*;
use crate::shutdown::OwnerState;

const BUDGET: Duration = Duration::from_millis(60);

struct Rig {
    context: FrontmostContext,
    backend: Arc<FakeApplicationBackend>,
    executor: Arc<ManualExecutor>,
    publisher: ContextPublisher,
    _service: Arc<InputService>,
}

fn rig() -> Rig {
    let executor = Arc::new(ManualExecutor::new(true));
    let backend = Arc::new(FakeApplicationBackend::new());
    backend.defer_launches(Arc::clone(&executor));
    let control = ApplicationControl::with_acknowledgement_budget(
        Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
        Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
        BUDGET,
    );
    let service = InputService::unavailable();
    let publisher = service.context_publisher();
    Rig {
        context: FrontmostContext::new(control, publisher.clone()),
        backend,
        executor,
        publisher,
        _service: service,
    }
}

impl Rig {
    fn lease(&self) -> FrontmostLease {
        self.context.lease(&CancellationToken::new()).expect("lease")
    }

    fn state(&self) -> FrontmostState { self.publisher.snapshot().frontmost().clone() }

    fn app(&self, bundle: &str, pid: i32) -> FakeAppKey {
        self.backend.add_running_with_pid(bundle, pid)
    }

    /// Activates `key`, returning after the activation but before its refresh.
    fn activate(&self, key: FakeAppKey) {
        assert_eq!(self.backend.activate(key), 1);
    }

    fn observers(&self) -> usize { self.backend.activation_observers() }
}

fn known(bundle: &str, bits: u32) -> FrontmostState {
    FrontmostState::Known(FrontmostApp {
        bundle_id: Arc::from(bundle),
        exemptions: ExemptionFlags(bits),
    })
}

#[test]
fn nothing_runs_without_a_lessee() {
    let rig = rig();
    assert_eq!(rig.observers(), 0);
    assert_eq!(rig.backend.observations(), 0);
    assert_eq!(rig.context.leases(), 0);
    assert_eq!(rig.publisher.snapshot().version(), 0);
}

#[test]
fn lease_refcount_installs_once_and_stops_with_the_last_release() {
    let rig = rig();
    let first = rig.lease();
    assert_eq!(rig.observers(), 1);
    let second = rig.lease();
    assert_eq!(rig.observers(), 1);
    assert_eq!(rig.context.leases(), 2);
    assert_eq!(first.release(), ReleaseOutcome::Held);
    assert_eq!(rig.observers(), 1);
    assert_eq!(second.release(), ReleaseOutcome::Stopped);
    assert_eq!(rig.observers(), 0);
    assert_eq!(rig.backend.observations(), 0);
    let again = rig.lease();
    assert_eq!(rig.observers(), 1);
    drop(again);
    assert_eq!(rig.observers(), 0);
    assert_eq!(rig.context.leases(), 0);
}

#[test]
fn activation_marks_stale_before_the_refresh_publishes() {
    let rig = rig();
    let a = rig.app("com.example.a", 10);
    let b = rig.app("com.example.b", 11);
    rig.backend.set_frontmost(Some(a));
    let _lease = rig.lease();
    assert_eq!(rig.state(), known("com.example.a", 0));
    rig.activate(b);
    assert_eq!(rig.state(), FrontmostState::Stale);
    rig.executor.drain();
    assert_eq!(rig.state(), known("com.example.b", 0));
}

#[test]
fn exempt_applications_stay_known_with_their_bits() {
    let rig = rig();
    let finder = rig.app("com.apple.finder", 20);
    let own = rig.app("com.example.stache", 21);
    let plain = rig.app("com.example.plain", 22);
    rig.backend.set_current_process(Some(own));
    let _lease = rig.lease();
    rig.activate(finder);
    rig.executor.drain();
    assert_eq!(rig.state(), known("com.apple.finder", input_context::FINDER.0));
    rig.activate(own);
    rig.executor.drain();
    assert_eq!(
        rig.state(),
        known("com.example.stache", input_context::CURRENT_PROCESS.0)
    );
    rig.activate(plain);
    rig.executor.drain();
    assert_eq!(rig.state(), known("com.example.plain", 0));
}

#[test]
fn unconfirmed_missing_bundle_and_failed_refresh_are_unknown() {
    let rig = rig();
    let a = rig.app("com.example.a", 30);
    let b = rig.app("com.example.b", 31);
    rig.backend.set_frontmost(Some(a));
    let _lease = rig.lease();
    assert_eq!(rig.state(), known("com.example.a", 0));
    // No bundle identifier: unconfirmed metadata.
    rig.backend.forget_bundle_id(b);
    rig.activate(b);
    rig.executor.drain();
    assert_eq!(rig.state(), FrontmostState::Unknown);
    // A failed refresh query.
    rig.activate(a);
    rig.backend.fail_next_frontmost();
    rig.executor.drain();
    assert_eq!(rig.state(), FrontmostState::Unknown);
    // No frontmost application at all.
    rig.backend.set_frontmost(None);
    let _ = rig.backend.deliver_activation();
    rig.executor.drain();
    assert_eq!(rig.state(), FrontmostState::Unknown);
    // Recovery publishes again.
    rig.activate(a);
    rig.executor.drain();
    assert_eq!(rig.state(), known("com.example.a", 0));
}

#[test]
fn a_superseded_refresh_never_overwrites_a_newer_stale() {
    let rig = rig();
    let a = rig.app("com.example.a", 40);
    rig.backend.set_frontmost(Some(a));
    let lease = rig.lease();
    let generation = lock(&rig.context.inner.gate).generation;
    let listener = Listener {
        generation,
        gate: Arc::clone(&rig.context.inner.gate),
        publisher: rig.publisher.clone(),
    };
    listener.invalidated(7);
    assert_eq!(rig.state(), FrontmostState::Stale);
    // A confirmed answer for an older epoch arrives late.
    let mut late = confirmed_snapshot(generation, 6);
    listener.published(&late);
    assert_eq!(rig.state(), FrontmostState::Stale);
    // The current epoch publishes; a rejected refresh at it is unknown.
    late.epoch = 7;
    listener.published(&late);
    assert_eq!(rig.state(), known("com.example.late", 0));
    let mut rejected = confirmed_snapshot(generation, 7);
    rejected.state = stache_macos::FrontmostState::Unconfirmed(Unconfirmed::Invalidated);
    listener.published(&rejected);
    assert_eq!(rig.state(), FrontmostState::Unknown);
    drop(lease);
}

fn confirmed_snapshot(generation: u64, epoch: u64) -> FrontmostSnapshot {
    FrontmostSnapshot {
        generation,
        epoch,
        state: stache_macos::FrontmostState::Confirmed,
        application: None,
        bundle_identifier: Some(Arc::from("com.example.late")),
        name: None,
        is_current_process: false,
        is_finder: false,
        pid: None,
    }
}

#[test]
fn stop_marks_unknown() {
    let rig = rig();
    let a = rig.app("com.example.a", 50);
    rig.backend.set_frontmost(Some(a));
    let lease = rig.lease();
    assert_eq!(rig.state(), known("com.example.a", 0));
    assert_eq!(lease.release(), ReleaseOutcome::Stopped);
    assert_eq!(rig.state(), FrontmostState::Unknown);
}

#[test]
fn a_previous_generation_cannot_overwrite_a_newer_lease() {
    let rig = rig();
    let a = rig.app("com.example.a", 60);
    rig.backend.set_frontmost(Some(a));
    let first = rig.lease();
    let old_generation = lock(&rig.context.inner.gate).generation;
    assert_eq!(first.release(), ReleaseOutcome::Stopped);
    let _second = rig.lease();
    assert_ne!(lock(&rig.context.inner.gate).generation, old_generation);
    assert_eq!(rig.state(), known("com.example.a", 0));
    let old = Listener {
        generation: old_generation,
        gate: Arc::clone(&rig.context.inner.gate),
        publisher: rig.publisher.clone(),
    };
    old.invalidated(99);
    old.published(&confirmed_snapshot(old_generation, 99));
    assert_eq!(rig.state(), known("com.example.a", 0));
    let mut inactive = confirmed_snapshot(old_generation, 99);
    inactive.state = stache_macos::FrontmostState::Unconfirmed(Unconfirmed::Inactive);
    old.published(&inactive);
    assert_eq!(rig.state(), known("com.example.a", 0));
}

#[test]
fn publisher_reads_never_deadlock_with_listener_calls() {
    let rig = rig();
    let a = rig.app("com.example.a", 70);
    let b = rig.app("com.example.b", 71);
    rig.backend.set_frontmost(Some(a));
    let _lease = rig.lease();
    let publisher = rig.publisher.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&reads);
    let reader = thread::spawn(move || {
        for _ in 0..2000 {
            let _ = publisher.snapshot();
            counter.fetch_add(1, Ordering::SeqCst);
        }
    });
    for _ in 0..200 {
        rig.activate(b);
        rig.executor.drain();
        rig.activate(a);
        rig.executor.drain();
    }
    reader.join().expect("reader");
    assert_eq!(reads.load(Ordering::SeqCst), 2000);
}

#[test]
fn failed_removal_stays_owned_blocks_a_duplicate_and_is_retried_first() {
    let rig = rig();
    let a = rig.app("com.example.a", 80);
    rig.backend.set_frontmost(Some(a));
    let lease = rig.lease();
    rig.backend.fail_unobserves(2);
    let ReleaseOutcome::Retained(_) = lease.release() else {
        panic!("removal failure must be retained");
    };
    assert!(rig.context.has_retained_cleanup());
    assert_eq!(rig.state(), FrontmostState::Unknown);
    assert_eq!(rig.observers(), 1, "the observer is still owned");
    // The retry fails too: no duplicate install, still owned.
    let error = rig.context.lease(&CancellationToken::new()).expect_err("blocked");
    assert!(matches!(error, LeaseError::Cleanup(_)));
    assert_eq!(rig.context.leases(), 0);
    assert_eq!(rig.observers(), 1);
    // The next lease retries the cleanup first, then installs.
    let lease = rig.lease();
    assert!(!rig.context.has_retained_cleanup());
    assert_eq!(rig.observers(), 1);
    assert_eq!(rig.state(), known("com.example.a", 0));
    assert_eq!(lease.release(), ReleaseOutcome::Stopped);
    assert_eq!(rig.observers(), 0);
}

#[test]
fn a_removal_timeout_keeps_ownership_and_cleans_exactly_once() {
    let rig = rig();
    let lease = rig.lease();
    rig.executor.set_on_main(false);
    assert_eq!(
        lease.release(),
        ReleaseOutcome::Retained(ApplicationError::TimedOut)
    );
    assert!(rig.context.has_retained_cleanup());
    assert!(matches!(
        rig.context.lease(&CancellationToken::new()),
        Err(LeaseError::Cleanup(ApplicationError::TimedOut))
    ));
    assert_eq!(rig.observers(), 1);
    rig.executor.set_on_main(true);
    rig.executor.drain();
    assert_eq!(rig.observers(), 0);
    let lease = rig.lease();
    assert!(!rig.context.has_retained_cleanup());
    assert_eq!(rig.observers(), 1);
    drop(lease);
    assert_eq!(rig.observers(), 0);
}

#[test]
fn a_failed_install_is_a_typed_error_and_holds_no_lease() {
    let rig = rig();
    rig.backend.fail_next_observe();
    let error = rig.context.lease(&CancellationToken::new()).expect_err("install fails");
    assert!(matches!(error, LeaseError::Install(_)));
    assert!(error.to_string().contains("observation"));
    assert_eq!(rig.context.leases(), 0);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(rig.context.lease(&cancelled).is_err());
    assert_eq!(rig.context.leases(), 0);
    // A later lease succeeds.
    let lease = rig.lease();
    assert_eq!(rig.observers(), 1);
    drop(lease);
}

#[test]
fn concurrent_leases_and_releases_keep_exactly_one_observer_while_held() {
    let rig = Arc::new(rig());
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let rig = Arc::clone(&rig);
            thread::spawn(move || {
                for _ in 0..40 {
                    let lease = rig.lease();
                    assert_eq!(rig.observers(), 1);
                    assert!(rig.context.leases() >= 1);
                    drop(lease);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("worker");
    }
    assert_eq!(rig.context.leases(), 0);
    assert_eq!(rig.observers(), 0);
    assert_eq!(rig.backend.observations(), 0);
}

fn wait_answered(answer: &OwnerAnswer) -> OwnerState {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !answer.is_answered() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    answer.state()
}

#[test]
fn the_teardown_owner_acknowledges_idle_and_retained_cleanup() {
    let rig = rig();
    let owner = FrontmostContextOwner::new(rig.context.clone());
    assert_eq!(owner.name(), "frontmost-context");
    assert!(!owner.on_main());
    // Nothing leased and nothing retained: acknowledged immediately.
    assert_eq!(wait_answered(&owner.release()), OwnerState::Succeeded);
    // Retained cleanup is retried and acknowledged.
    let lease = rig.lease();
    rig.backend.fail_next_unobserve();
    let ReleaseOutcome::Retained(_) = lease.release() else {
        panic!("retained");
    };
    assert_eq!(wait_answered(&owner.release()), OwnerState::Succeeded);
    assert!(!rig.context.has_retained_cleanup());
    assert_eq!(rig.observers(), 0);
    // A failing retry answers failure and stays owned.
    let lease = rig.lease();
    rig.backend.fail_unobserves(2);
    let ReleaseOutcome::Retained(_) = lease.release() else {
        panic!("retained");
    };
    assert_eq!(wait_answered(&owner.release()), OwnerState::Failed);
    assert!(rig.context.has_retained_cleanup());
    // A held lease is not an answer.
    drop(rig.lease());
    let held = rig.lease();
    let pending = owner.release();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(pending.state(), OwnerState::Pending);
    drop(held);
}
