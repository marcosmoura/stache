//! Runtime, policy, race and lifecycle tests for Keep Awake.
//!
//! Everything runs over the injected native fakes and a registry-backed
//! context: no test acquires a real assertion, changes the screen lock, installs
//! a real observer or waits on the real reconciliation interval. Only the slow
//! stop test spends real time, to outlast the two-second bound it replaced.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use stache_macos::{
    AssertionId, AssertionKind, AssertionOwner, FakeAssertionBackend, FakeLockObservationBackend,
    FakeLockProbe, LockObservationBackend, LockObservationError, LockObservationInstaller,
    LockProbe, LockState as NativeLockState, LockUnknown, ManualExecutor,
};
use stache_sdk::{
    ErrorCategory, MainThreadExecutor, ModuleStatus, Operation, RegistryError, ReloadStatus,
};

use super::*;
use crate::test_support::{Harness, WAIT, wait_until};

const KINDS: [AssertionKind; 3] = [
    AssertionKind::Display,
    AssertionKind::Idle,
    AssertionKind::System,
];

const FAILURE_CODE: i32 = -536_870_212;
const RELEASE_FAILURE_CODE: i32 = -536_870_184;

fn failure(error: &RegistryError) -> (Operation, ErrorCategory) {
    match error {
        RegistryError::Module(error) => (error.operation(), error.category()),
        other => panic!("expected a module error, got {other:?}"),
    }
}

fn is_release_failed(runtime: &KeepAwakeRuntimeStatus) -> bool {
    runtime
        .conditions
        .iter()
        .any(|condition| condition.kind == KeepAwakeConditionKind::AssertionReleaseFailed)
}

fn wait_assertion(harness: &Harness, expected: AssertionState) {
    wait_until(&format!("assertion {expected:?}"), || {
        harness.runtime().assertion_state == expected
    });
}

#[test]
fn a_disabled_constructor_and_config_touch_no_native_resource() {
    let harness = Harness::new(false);
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    harness.start().expect("start_all");
    assert_eq!(harness.status(), ModuleStatus::Disabled);
    assert!(harness.backend.create_attempts().is_empty());
    assert!(harness.backend.release_attempts().is_empty());
    assert!(harness.observation.install_calls().is_empty());
    assert_eq!(harness.probe.calls(), 0);
    assert_eq!(harness.generation_count(), 0);
}

#[test]
fn the_descriptor_declares_only_the_configuration_identity() {
    fn assert_send<T: Send>() {}
    assert_send::<KeepAwakeModule>();
    let harness = Harness::new(false);
    let descriptors = harness.registry.descriptors();
    let descriptor = descriptors
        .iter()
        .find(|descriptor| descriptor.id == KeepAwakeConfig::ID)
        .expect("keepAwake descriptor");
    assert!(descriptor.provides.is_empty());
    assert!(descriptor.requires.is_empty());
    assert!(descriptor.permissions.is_empty());
    assert!(descriptor.executables.is_empty());
    assert!(descriptor.surfaces.is_empty());
}

#[test]
fn a_locked_start_runs_without_creating_an_assertion() {
    let harness = Harness::locked();
    harness.start().expect("start_all");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus {
        assertion_state: AssertionState::Inactive,
        lock_state: LockState::Locked,
        observation: LockObservation::NotificationsAndPolling,
        conditions: Vec::new(),
    });
    assert!(harness.backend.create_attempts().is_empty());
    assert_eq!(harness.observation.install_calls().len(), 1);
    assert!(harness.conditions().is_empty());
}

#[test]
fn an_unlocked_start_acquires_the_complete_three_assertion_lease() {
    let harness = Harness::new(true);
    harness.backend.set_ids([11, 12, 13]);
    harness.start().expect("start_all");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert_eq!(harness.runtime().assertion_state, AssertionState::Active);
    assert_eq!(harness.runtime().lock_state, LockState::Unlocked);
    assert_eq!(
        harness.runtime().observation,
        LockObservation::NotificationsAndPolling
    );
    let created: Vec<(AssertionKind, AssertionId)> = harness
        .backend
        .created()
        .into_iter()
        .map(|owned| (owned.kind, owned.id))
        .collect();
    assert_eq!(created, vec![
        (AssertionKind::Display, AssertionId::new(11)),
        (AssertionKind::Idle, AssertionId::new(12)),
        (AssertionKind::System, AssertionId::new(13)),
    ]);
    assert!(harness.owner.pending().is_empty());
    assert!(harness.conditions().is_empty());
}

#[test]
fn the_absent_lock_key_is_the_native_unlocked_compatibility_case() {
    // The native probe maps "valid dictionary with no lock key" to `Unlocked`;
    // the runtime therefore treats it exactly like an explicit unlocked read.
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    assert_eq!(harness.runtime().lock_state, LockState::Unlocked);
    assert_eq!(harness.runtime().assertion_state, AssertionState::Active);
    assert_eq!(harness.backend.create_attempts(), KINDS.to_vec());
}

#[test]
fn a_lock_releases_and_an_unlock_restores_prevention() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    assert_eq!(harness.runtime().assertion_state, AssertionState::Active);

    harness.probe.set(NativeLockState::Locked);
    assert!(harness.deliver());
    wait_assertion(&harness, AssertionState::Inactive);
    assert_eq!(harness.runtime().lock_state, LockState::Locked);
    assert_eq!(harness.backend.release_attempts().len(), 3);
    assert!(harness.conditions().is_empty());

    harness.probe.set(NativeLockState::Unlocked);
    assert!(harness.deliver());
    wait_assertion(&harness, AssertionState::Active);
    assert_eq!(harness.backend.created().len(), 6);
    assert!(harness.owner.pending().is_empty());
}

#[test]
fn repeated_lock_and_unlock_cycles_reuse_no_lease() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    for cycle in 0..3 {
        harness.probe.set(NativeLockState::Locked);
        assert!(harness.deliver());
        wait_assertion(&harness, AssertionState::Inactive);
        harness.probe.set(NativeLockState::Unlocked);
        assert!(harness.deliver());
        wait_assertion(&harness, AssertionState::Active);
        assert_eq!(harness.backend.created().len(), 3 + 3 * (cycle + 1));
        assert!(harness.owner.pending().is_empty());
    }
    assert_eq!(harness.backend.release_attempts().len(), 9);
}

#[test]
fn a_late_unlock_while_paused_is_inert_and_resume_probes_again() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    assert_eq!(harness.runtime().assertion_state, AssertionState::Active);

    harness.pause().expect("pause");
    assert_eq!(harness.status(), ModuleStatus::Paused);
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert!(!harness.observation.is_installed());

    harness.probe.set(NativeLockState::Unlocked);
    assert!(!harness.deliver(), "no observation is installed while paused");

    // A stale unlock must not be replayed: resume probes the lock it finds now.
    harness.probe.set(NativeLockState::Locked);
    harness.resume().expect("resume");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert_eq!(harness.runtime().assertion_state, AssertionState::Inactive);
    assert_eq!(harness.runtime().lock_state, LockState::Locked);
    assert_eq!(harness.backend.created().len(), 3);

    harness.probe.set(NativeLockState::Unlocked);
    assert!(harness.deliver());
    wait_assertion(&harness, AssertionState::Active);
    assert_eq!(harness.backend.created().len(), 6);
    assert_eq!(harness.observation.install_calls().len(), 2);
}

#[test]
fn a_late_unlock_while_disabled_is_inert() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    harness.set_enabled(false).expect("disable");
    assert_eq!(harness.status(), ModuleStatus::Disabled);
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert!(!harness.observation.is_installed());

    harness.probe.set(NativeLockState::Unlocked);
    assert!(!harness.deliver());
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(harness.backend.created().len(), 3);
    assert_eq!(harness.runtime().assertion_state, AssertionState::Inactive);

    // Repeated disable and shutdown are safe.
    harness.set_enabled(false).expect("disable twice");
    harness.registry.shutdown().expect("shutdown is idempotent");
}

#[test]
fn an_unknown_probe_with_a_failed_observer_still_fails_cleanly() {
    let harness = Harness::build(
        true,
        NativeLockState::Unknown(LockUnknown::SessionDictionaryUnavailable),
        |module| module,
    );
    harness
        .observation
        .fail_next_install(LockObservationError::Unavailable("test-only failure".to_owned()));
    let error = harness.start().expect_err("unknown must fail the start");
    assert_eq!(failure(&error), (Operation::Start, ErrorCategory::Platform));
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert!(harness.backend.create_attempts().is_empty());
    assert!(harness.owner.pending().is_empty());
    assert_eq!(harness.observation.install_calls().len(), 1);
    assert!(!harness.observation.is_installed());
}

#[test]
fn an_unknown_initial_probe_fails_the_start_and_retries_after_recovery() {
    let harness = Harness::build(
        true,
        NativeLockState::Unknown(LockUnknown::SessionDictionaryUnavailable),
        |module| module,
    );
    let error = harness.start().expect_err("unknown must fail the start");
    assert_eq!(failure(&error), (Operation::Start, ErrorCategory::Platform));
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert_eq!(harness.probe.calls(), 1);
    assert!(harness.backend.create_attempts().is_empty());
    assert_eq!(harness.observation.install_calls().len(), 1);
    assert_eq!(harness.observation.remove_calls().len(), 1);
    assert!(!harness.observation.is_installed());

    harness.probe.set(NativeLockState::Unlocked);
    harness.set_enabled(false).expect("clear the failure");
    harness.set_enabled(true).expect("retry the start");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert_eq!(harness.runtime().assertion_state, AssertionState::Active);
    assert_eq!(harness.backend.created().len(), 3);
}

#[test]
fn each_initial_creation_failure_rolls_back_and_retries() {
    for kind in KINDS {
        let harness = Harness::new(true);
        harness.backend.set_ids([11, 12, 13]);
        harness.backend.fail_next_create(kind, FAILURE_CODE);
        let error = harness.start().expect_err("creation failure must fail the start");
        assert_eq!(failure(&error), (Operation::Start, ErrorCategory::Platform));
        let position = KINDS.iter().position(|candidate| *candidate == kind).expect("known kind");
        assert_eq!(harness.backend.created().len(), position);
        assert_eq!(harness.backend.release_attempts().len(), position);
        assert!(harness.owner.pending().is_empty(), "{kind:?}");
        assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default(), "{kind:?}");

        harness.backend.clear_failures();
        harness.set_enabled(false).expect("disable after a failed start");
        harness.set_enabled(true).expect("retry the start");
        wait_assertion(&harness, AssertionState::Active);
        assert_eq!(harness.backend.created().len(), position + 3);
        assert!(harness.runtime().conditions.is_empty());
    }
}

#[test]
fn an_initial_rollback_failure_retains_pending_until_recovery() {
    let harness = Harness::new(true);
    harness.backend.fail_next_create(AssertionKind::Idle, FAILURE_CODE);
    harness
        .backend
        .fail_release_always(AssertionKind::Display, RELEASE_FAILURE_CODE);

    let error = harness.start().expect_err("rollback failure must fail the start");
    assert_eq!(failure(&error), (Operation::Start, ErrorCategory::Platform));
    let runtime = harness.runtime();
    assert_eq!(runtime.assertion_state, AssertionState::ReleasePending);
    assert_eq!(runtime.lock_state, LockState::Unknown);
    assert_eq!(runtime.observation, LockObservation::Inactive);
    assert!(is_release_failed(&runtime), "{runtime:?}");

    let pending = harness.owner.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, AssertionKind::Display);
    let retained = pending[0].id;

    harness.backend.clear_failures();
    harness.set_enabled(false).expect("the retained ID now cleans up");
    assert_eq!(harness.status(), ModuleStatus::Disabled);
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert!(harness.owner.pending().is_empty());

    // A confirmed cleanup pass is inert and performs no duplicate native call.
    let attempts = harness.backend.release_attempts().len();
    assert!(harness.owner.cleanup_pending().is_complete());
    assert_eq!(harness.backend.release_attempts().len(), attempts);
    assert!(
        harness.backend.release_attempts().iter().any(|(_, id)| *id == retained),
        "the retained ID was attempted"
    );

    harness.set_enabled(true).expect("retry the start");
    wait_assertion(&harness, AssertionState::Active);
    assert_eq!(harness.backend.created().len(), 4);
    assert!(harness.owner.pending().is_empty());
}

#[test]
fn a_runtime_release_failure_stays_pending_and_recovers_on_the_timer() {
    let harness = Harness::new(true);
    harness.backend.set_ids([11, 12, 13]);
    harness.start().expect("start_all");
    harness.backend.fail_release_always(AssertionKind::System, RELEASE_FAILURE_CODE);

    harness.probe.set(NativeLockState::Locked);
    assert!(harness.deliver());
    wait_assertion(&harness, AssertionState::ReleasePending);
    let runtime = harness.runtime();
    assert_eq!(runtime.lock_state, LockState::Locked);
    assert!(is_release_failed(&runtime), "{runtime:?}");
    let pending = harness.owner.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, AssertionKind::System);
    assert_eq!(pending[0].id, AssertionId::new(13));
    let attempts = harness.backend.release_attempts().len();

    harness.backend.clear_failures();
    harness.tick();
    wait_assertion(&harness, AssertionState::Inactive);
    assert!(harness.runtime().conditions.is_empty());
    assert!(harness.owner.pending().is_empty());
    assert_eq!(harness.backend.release_attempts().len(), attempts + 1);

    // The lock knowledge stays current and another pass is inert.
    harness.tick();
    assert_eq!(harness.backend.release_attempts().len(), attempts + 1);
    assert_eq!(harness.runtime().assertion_state, AssertionState::Inactive);
}

#[test]
fn a_stop_release_failure_surfaces_the_lifecycle_error_and_recovers() {
    let harness = Harness::new(true);
    harness.backend.set_ids([11, 12, 13]);
    harness.start().expect("start_all");
    harness.backend.fail_release_always(AssertionKind::Idle, RELEASE_FAILURE_CODE);

    let error = harness.set_enabled(false).expect_err("a failed release must fail the stop");
    assert_eq!(failure(&error), (Operation::Stop, ErrorCategory::Platform));
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    let runtime = harness.runtime();
    assert_eq!(runtime.assertion_state, AssertionState::ReleasePending);
    assert_eq!(runtime.lock_state, LockState::Unknown);
    assert_eq!(runtime.observation, LockObservation::Inactive);
    assert!(is_release_failed(&runtime), "{runtime:?}");
    let pending = harness.owner.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, AssertionKind::Idle);
    assert_eq!(pending[0].id, AssertionId::new(12));

    // Retry resolves the retained cleanup before any fresh acquisition.
    harness.backend.clear_failures();
    harness.set_enabled(true).expect("retry after the failed stop");
    wait_assertion(&harness, AssertionState::Active);
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert!(harness.owner.pending().is_empty());
    assert!(harness.runtime().conditions.is_empty());
    assert_eq!(harness.backend.created().len(), 6);
}

#[test]
fn a_pause_release_failure_retains_pending_status() {
    let harness = Harness::new(true);
    harness.backend.set_ids([11, 12, 13]);
    harness.start().expect("start_all");
    harness.backend.fail_release_always(AssertionKind::System, RELEASE_FAILURE_CODE);

    let error = harness.pause().expect_err("a failed release must fail the pause");
    assert_eq!(failure(&error), (Operation::Stop, ErrorCategory::Platform));
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    let runtime = harness.runtime();
    assert_eq!(runtime.assertion_state, AssertionState::ReleasePending);
    assert_eq!(runtime.lock_state, LockState::Unknown);
    assert_eq!(runtime.observation, LockObservation::Inactive);
    assert!(is_release_failed(&runtime), "{runtime:?}");
    assert_eq!(harness.owner.pending().len(), 1);

    harness.backend.clear_failures();
    harness.resume().expect("resume after the failed pause");
    wait_assertion(&harness, AssertionState::Active);
    assert!(harness.owner.pending().is_empty());
    assert!(harness.runtime().conditions.is_empty());
}

#[test]
fn a_clean_stop_confirms_release_before_reporting_inactive() {
    let harness = Harness::new(true);
    harness.backend.set_ids([11, 12, 13]);
    harness.start().expect("start_all");
    harness.set_enabled(false).expect("disable");
    assert_eq!(harness.status(), ModuleStatus::Disabled);
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert_eq!(harness.backend.release_attempts().len(), 3);
    assert!(harness.owner.pending().is_empty());
    assert!(!harness.observation.is_installed());
}

#[test]
fn stop_publishes_release_pending_before_the_first_native_release() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    let barrier = harness.backend.block_next_release();
    let registry = Arc::clone(&harness.registry);
    let handle = harness.handle;
    let stop = std::thread::spawn(move || registry.set_enabled(handle, false));
    assert!(barrier.wait_entered(WAIT), "the release must have started");

    // The read is prompt while the native barrier is held.
    let runtime = harness.runtime();
    assert_eq!(runtime.assertion_state, AssertionState::ReleasePending);
    assert!(!is_release_failed(&runtime), "a normal release is not a failure");
    assert_eq!(harness.backend.release_attempts().len(), 1);

    assert!(barrier.release());
    stop.join().expect("stop thread").expect("stop succeeds");
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
}

#[test]
fn pause_and_disable_leave_no_observation_and_no_timer() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    let signals = harness.signals();
    harness.pause().expect("pause");
    assert_eq!(harness.status(), ModuleStatus::Paused);
    assert!(!harness.observation.is_installed());

    let calls = harness.probe.calls();
    let created = harness.backend.created().len();
    signals.advance(Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(harness.probe.calls(), calls, "a paused generation has no timer");
    assert_eq!(harness.backend.created().len(), created);

    harness.resume().expect("resume");
    assert_eq!(harness.status(), ModuleStatus::Running);
    wait_assertion(&harness, AssertionState::Active);
    assert_eq!(harness.observation.install_calls().len(), 2);
    assert_eq!(harness.generation_count(), 2);
}

#[test]
fn the_initial_creation_publishes_acquiring_before_native_work() {
    let harness = Harness::new(true);
    let barrier = harness.backend.block_next_create();
    let registry = Arc::clone(&harness.registry);
    let start = std::thread::spawn(move || registry.start_all());
    assert!(barrier.wait_entered(WAIT), "the creation must have started");

    let runtime = harness.runtime();
    assert_eq!(runtime.assertion_state, AssertionState::Acquiring);
    assert_eq!(runtime.lock_state, LockState::Unlocked);
    assert!(harness.backend.created().is_empty());

    assert!(barrier.release());
    start.join().expect("start thread").expect("start succeeds");
    wait_assertion(&harness, AssertionState::Active);
    assert_eq!(harness.backend.created().len(), 3);
}

#[test]
fn a_runtime_creation_keeps_the_published_read_prompt() {
    let harness = Harness::locked();
    harness.start().expect("start_all");
    harness.probe.set(NativeLockState::Unlocked);
    let barrier = harness.backend.block_next_create();
    assert!(harness.deliver());
    assert!(barrier.wait_entered(WAIT), "the creation must have started");

    let runtime = harness.runtime();
    assert_eq!(runtime.assertion_state, AssertionState::Acquiring);
    assert_eq!(runtime.lock_state, LockState::Unlocked);

    assert!(barrier.release());
    wait_assertion(&harness, AssertionState::Active);
    assert_eq!(harness.backend.created().len(), 3);
}

#[test]
fn a_candidate_blocked_in_validation_is_never_adopted_after_stop() {
    let harness = Harness::locked();
    harness.start().expect("start_all");
    harness.probe.set(NativeLockState::Unlocked);
    let create = harness.backend.block_next_create();
    assert!(harness.deliver());
    assert!(create.wait_entered(WAIT));
    let validation = harness.probe.block_next();
    assert!(create.release());
    assert!(validation.wait_entered(WAIT), "validation probe must be running");
    assert_eq!(harness.runtime().assertion_state, AssertionState::Acquiring);

    let registry = Arc::clone(&harness.registry);
    let handle = harness.handle;
    let stop = std::thread::spawn(move || registry.set_enabled(handle, false));
    wait_until("the stop transition", || {
        harness.status() != ModuleStatus::Running
    });
    // The invalidated worker still owns an unadopted candidate: never `active`.
    assert_eq!(harness.runtime().assertion_state, AssertionState::Acquiring);
    assert!(!is_release_failed(&harness.runtime()));

    assert!(validation.release());
    stop.join().expect("stop thread").expect("stop succeeds");
    // Stop waited for the candidate, so its success is already confirmed.
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert_eq!(harness.backend.release_attempts().len(), 3);
    assert_eq!(harness.backend.created().len(), 3);
}

/// Parks a worker in candidate validation with all three IDs created.
fn block_a_candidate_in_validation(harness: &Harness) -> Arc<stache_macos::FakeNativeBarrier> {
    harness.start().expect("start_all");
    harness.probe.set(NativeLockState::Unlocked);
    let create = harness.backend.block_next_create();
    assert!(harness.deliver());
    assert!(create.wait_entered(WAIT));
    let validation = harness.probe.block_next();
    assert!(create.release());
    assert!(validation.wait_entered(WAIT), "validation probe must be running");
    validation
}

#[test]
fn a_failed_rollback_of_an_in_flight_candidate_fails_the_stop() {
    let harness = Harness::locked();
    harness.backend.set_ids([11, 12, 13]);
    let validation = block_a_candidate_in_validation(&harness);
    harness.backend.fail_release_always(AssertionKind::Idle, RELEASE_FAILURE_CODE);

    let registry = Arc::clone(&harness.registry);
    let handle = harness.handle;
    let stop = std::thread::spawn(move || registry.set_enabled(handle, false));
    wait_until("the stop transition", || {
        harness.status() != ModuleStatus::Running
    });
    assert!(validation.release());

    let error = stop
        .join()
        .expect("stop thread")
        .expect_err("the rollback failure must surface");
    assert_eq!(failure(&error), (Operation::Stop, ErrorCategory::Platform));
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    let runtime = harness.runtime();
    assert_eq!(runtime.assertion_state, AssertionState::ReleasePending);
    assert!(is_release_failed(&runtime), "{runtime:?}");
    let pending = harness.owner.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, AssertionId::new(12));

    harness.backend.clear_failures();
    harness.set_enabled(true).expect("the retry resolves the retained ID first");
    wait_assertion(&harness, AssertionState::Active);
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert!(harness.owner.pending().is_empty());
    assert!(harness.runtime().conditions.is_empty());
    assert_eq!(harness.backend.created().len(), 6);
}

#[test]
fn a_stop_during_a_worker_release_is_not_a_false_failure() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    let release = harness.backend.block_next_release();
    harness.probe.set(NativeLockState::Locked);
    assert!(harness.deliver());
    assert!(release.wait_entered(WAIT), "the worker release must be running");

    let registry = Arc::clone(&harness.registry);
    let handle = harness.handle;
    let stop = std::thread::spawn(move || registry.set_enabled(handle, false));
    wait_until("the stop transition", || {
        harness.status() != ModuleStatus::Running
    });
    let runtime = harness.runtime();
    assert_eq!(runtime.assertion_state, AssertionState::ReleasePending);
    assert!(!is_release_failed(&runtime), "{runtime:?}");

    assert!(release.release());
    stop.join().expect("stop thread").expect("stop succeeds");
    assert_eq!(harness.status(), ModuleStatus::Disabled);
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert_eq!(harness.backend.release_attempts().len(), 3);
}

#[test]
fn slow_work_delays_the_stop_without_failing_it() {
    let harness = Harness::locked();
    let validation = block_a_candidate_in_validation(&harness);

    let registry = Arc::clone(&harness.registry);
    let handle = harness.handle;
    let stop = std::thread::spawn(move || registry.set_enabled(handle, false));
    wait_until("the stop transition", || {
        harness.status() != ModuleStatus::Running
    });

    // Longer than the old two-second bound, which failed a clean stop.
    std::thread::sleep(Duration::from_millis(2_200));
    assert!(!stop.is_finished(), "the stop waits for the work it must report");
    assert_eq!(harness.runtime().assertion_state, AssertionState::Acquiring);
    assert!(validation.release());

    stop.join().expect("stop thread").expect("slow but clean work is a clean stop");
    assert_eq!(harness.status(), ModuleStatus::Disabled);
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert_eq!(harness.backend.release_attempts().len(), 3);
}

#[test]
fn a_worker_panic_during_acquisition_does_not_hang_the_stop() {
    let harness = Harness::locked();
    harness.start().expect("start_all");
    harness.probe.set(NativeLockState::Unlocked);
    let create = harness.backend.block_next_create();
    assert!(harness.deliver());
    assert!(create.wait_entered(WAIT));
    harness.probe.panic_next();
    let probes = harness.probe.calls();
    assert!(create.release());
    // Stopping before the validation probe would cancel the candidate instead.
    wait_until("the panicking probe", || harness.probe.calls() > probes);

    let registry = Arc::clone(&harness.registry);
    let handle = harness.handle;
    let (sender, receiver) = mpsc::channel();
    let stop = std::thread::spawn(move || {
        let _ = sender.send(registry.set_enabled(handle, false).is_ok());
    });
    assert_eq!(receiver.recv_timeout(WAIT), Ok(true));
    stop.join().expect("stop thread");
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert_eq!(harness.backend.release_attempts().len(), 3);
    assert!(harness.owner.pending().is_empty());
}

#[test]
fn repeating_a_failed_disable_retries_the_cleanup_while_disabled() {
    let harness = Harness::new(true);
    harness.backend.set_ids([11, 12, 13]);
    harness.start().expect("start_all");
    harness.backend.fail_release_always(AssertionKind::Idle, RELEASE_FAILURE_CODE);
    harness.set_enabled(false).expect_err("a failed release must fail the disable");
    assert_eq!(harness.owner.pending().len(), 1);

    harness.set_enabled(false).expect_err("a still-failing retry stays visible");
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    assert!(is_release_failed(&harness.runtime()));
    assert_eq!(harness.owner.pending().len(), 1);

    harness.backend.clear_failures();
    harness.set_enabled(false).expect("the retry confirms the cleanup");
    assert_eq!(harness.status(), ModuleStatus::Disabled);
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert!(harness.owner.pending().is_empty());
    assert_eq!(harness.backend.created().len(), 3, "disabled stays inert");
}

#[test]
fn repeating_a_failed_pause_retries_the_cleanup_while_paused() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    harness.backend.fail_release_always(AssertionKind::System, RELEASE_FAILURE_CODE);
    harness.pause().expect_err("a failed release must fail the pause");

    harness.backend.clear_failures();
    harness.pause().expect("the retry confirms the cleanup");
    assert_eq!(harness.status(), ModuleStatus::Paused);
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert!(harness.owner.pending().is_empty());
    assert_eq!(harness.backend.created().len(), 3, "paused stays inert");
}

#[test]
fn a_failed_observation_removal_fails_the_stop_and_retries() {
    let harness = Harness::locked();
    harness.start().expect("start_all");
    harness
        .observation
        .fail_next_remove(LockObservationError::Unavailable("test-only failure".to_owned()));

    let error = harness.set_enabled(false).expect_err("a failed removal must fail the stop");
    assert_eq!(failure(&error), (Operation::Stop, ErrorCategory::Platform));
    assert_eq!(harness.conditions(), vec![
        KeepAwakeConditionKind::ObservationDispatchFailed
    ]);
    assert_eq!(harness.runtime().assertion_state, AssertionState::Inactive);

    harness.set_enabled(true).expect("the retry removes the old observation first");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert!(harness.conditions().is_empty());
    assert_eq!(harness.observation.remove_calls().len(), 2);
    assert_eq!(harness.observation.install_calls().len(), 2);
    assert!(harness.observation.is_installed());
}

#[test]
fn dropping_the_module_is_not_release_evidence() {
    let harness = Harness::new(true);
    harness.backend.set_ids([11, 12, 13]);
    harness.start().expect("start_all");
    harness.backend.fail_release_always(AssertionKind::System, RELEASE_FAILURE_CODE);
    harness.set_enabled(false).expect_err("a failed release must fail the stop");

    let owner = harness.owner.clone();
    let snapshot = harness.snapshot.clone();
    let backend = harness.backend.clone();
    let attempts = backend.release_attempts().len();
    drop(harness);

    // Shutdown and the final best-effort Drop both tried and neither succeeded.
    assert!(backend.release_attempts().len() > attempts);
    assert_eq!(owner.pending().len(), 1);
    let runtime = snapshot.read();
    assert_eq!(runtime.assertion_state, AssertionState::ReleasePending);
    assert!(is_release_failed(&runtime), "{runtime:?}");
}

#[test]
fn a_candidate_that_completes_after_cancellation_is_disposed() {
    let harness = Harness::locked();
    harness.start().expect("start_all");
    harness.probe.set(NativeLockState::Unlocked);
    harness.backend.cancel_after_create(AssertionKind::System);
    let barrier = harness.backend.block_next_release();
    assert!(harness.deliver());
    assert!(
        barrier.wait_entered(WAIT),
        "the rollback release must have started"
    );

    let runtime = harness.runtime();
    assert_eq!(runtime.assertion_state, AssertionState::ReleasePending);
    assert!(
        !is_release_failed(&runtime),
        "an in-flight rollback is not a failure"
    );

    assert!(barrier.release());
    wait_assertion(&harness, AssertionState::Inactive);
    assert_eq!(harness.backend.created().len(), 3);
    assert_eq!(harness.backend.release_attempts().len(), 3);
    assert!(harness.owner.pending().is_empty());
    assert!(harness.runtime().conditions.is_empty());
}

#[test]
fn a_locked_validation_probe_discards_the_candidate() {
    let harness = Harness::new(true);
    harness.probe.script([NativeLockState::Unlocked, NativeLockState::Locked]);
    harness.start().expect("a raced candidate is not a start failure");
    assert_eq!(harness.runtime().assertion_state, AssertionState::Inactive);
    assert_eq!(harness.runtime().lock_state, LockState::Locked);
    assert_eq!(harness.backend.created().len(), 3);
    assert_eq!(harness.backend.release_attempts().len(), 3);
    assert!(harness.runtime().conditions.is_empty());
    assert!(harness.owner.pending().is_empty());

    // The next pass finds the current lock unlocked and acquires a fresh lease.
    harness.probe.set(NativeLockState::Unlocked);
    harness.tick();
    wait_assertion(&harness, AssertionState::Active);
    assert_eq!(harness.backend.created().len(), 6);
}

#[test]
fn an_unknown_validation_probe_discards_the_candidate() {
    let harness = Harness::new(true);
    harness.probe.script([
        NativeLockState::Unlocked,
        NativeLockState::Unknown(LockUnknown::SessionDictionaryUnavailable),
    ]);
    harness.start().expect("an unknown validation probe is not a start failure");
    assert_eq!(harness.runtime().assertion_state, AssertionState::Inactive);
    assert_eq!(harness.backend.release_attempts().len(), 3);
    // The failed read is visible immediately, not after the next pass.
    assert_eq!(harness.runtime().lock_state, LockState::Unknown);
    assert_eq!(harness.conditions(), vec![
        KeepAwakeConditionKind::LockProbeFailed
    ]);
    assert!(harness.owner.pending().is_empty());
}

#[test]
fn a_stale_callback_cannot_touch_a_successor_generation() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    assert_eq!(harness.runtime().assertion_state, AssertionState::Active);
    let old_wake = harness.observation.installed_wake().expect("installed wake");
    let (old_handle, old_generation) = harness.observation.install_calls()[0];

    harness.set_enabled(false).expect("disable");
    harness.set_enabled(true).expect("enable again");
    wait_assertion(&harness, AssertionState::Active);
    assert_eq!(harness.generation_count(), 2);

    let calls = harness.probe.calls();
    let created = harness.backend.created().len();
    old_wake();
    assert!(!harness.observation.deliver_generation(old_handle, old_generation));
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(harness.probe.calls(), calls, "a stale callback is inert");
    assert_eq!(harness.backend.created().len(), created);
    assert_eq!(harness.runtime().assertion_state, AssertionState::Active);
}

#[test]
fn an_unchanged_reload_does_not_restart_resources() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    let calls = harness.probe.calls();
    let report = harness.reload(KeepAwakeConfig { enabled: true }).expect("reload");
    assert_eq!(report.status, ReloadStatus::Unchanged);
    assert_eq!(harness.observation.install_calls().len(), 1);
    assert_eq!(harness.generation_count(), 1);
    assert_eq!(harness.backend.created().len(), 3);
    assert_eq!(harness.probe.calls(), calls);

    // A repeated start pass is a no-op too.
    harness.start().expect("second start_all");
    assert_eq!(harness.observation.install_calls().len(), 1);
    assert_eq!(harness.backend.created().len(), 3);
}

#[test]
fn a_notification_storm_coalesces_into_bounded_passes() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    wait_assertion(&harness, AssertionState::Active);
    let barrier = harness.probe.block_next();
    assert!(harness.deliver());
    assert!(barrier.wait_entered(WAIT), "the probe must be running");
    for _ in 0..50 {
        assert!(harness.deliver());
    }
    assert!(barrier.release());
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(harness.backend.create_attempts(), KINDS.to_vec());
    assert_eq!(harness.runtime().assertion_state, AssertionState::Active);
    assert!(
        harness.probe.calls() <= 4,
        "a storm must coalesce, saw {} probes",
        harness.probe.calls()
    );
}

#[test]
fn a_dropped_hint_is_recovered_by_the_two_second_timer() {
    let harness = Harness::locked();
    harness.start().expect("start_all");
    let calls = harness.probe.calls();
    harness.probe.set(NativeLockState::Unlocked);
    harness.advance(Duration::from_millis(1_999));
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(
        harness.probe.calls(),
        calls,
        "sub-interval time must not reconcile"
    );

    harness.advance(Duration::from_millis(1));
    wait_assertion(&harness, AssertionState::Active);
    assert_eq!(harness.backend.created().len(), 3);
}

#[test]
fn sub_interval_fake_time_does_not_reconcile() {
    let harness = Harness::locked();
    harness.start().expect("start_all");
    let calls = harness.probe.calls();
    harness.signals().wait_until_waiting(WAIT);
    harness.advance(Duration::from_secs(1));
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(harness.probe.calls(), calls);
    assert_eq!(harness.runtime().assertion_state, AssertionState::Inactive);
}

#[test]
fn stop_wakes_the_waiting_worker_without_advancing_the_interval() {
    let harness = Harness::locked();
    harness.start().expect("start_all");
    harness.signals().wait_until_waiting(WAIT);
    let registry = Arc::clone(&harness.registry);
    let handle = harness.handle;
    let (sender, receiver) = mpsc::channel();
    let stop = std::thread::spawn(move || {
        let _ = sender.send(registry.set_enabled(handle, false).is_ok());
    });
    assert_eq!(receiver.recv_timeout(WAIT), Ok(true));
    stop.join().expect("stop thread");
    assert_eq!(harness.status(), ModuleStatus::Disabled);
}

#[test]
fn an_unavailable_observer_runs_a_visible_polling_fallback_and_recovers() {
    let harness = Harness::new(true);
    harness
        .observation
        .fail_next_install(LockObservationError::Unavailable("test-only failure".to_owned()));
    harness.start().expect("observer failure alone succeeds degraded");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert_eq!(harness.runtime().observation, LockObservation::PollingFallback);
    assert_eq!(harness.conditions(), vec![
        KeepAwakeConditionKind::ObserverUnavailable
    ]);
    assert_eq!(
        harness.captured.count_containing("condition set: ObserverUnavailable"),
        1
    );

    // The owned timer still reconciles without notifications.
    harness.probe.set(NativeLockState::Locked);
    harness.tick();
    wait_assertion(&harness, AssertionState::Inactive);
    assert_eq!(harness.runtime().lock_state, LockState::Locked);

    // An observer warning coexists with a probe failure, and unchanged
    // conditions are not logged again.
    let calls = harness.probe.calls();
    harness.probe.set(NativeLockState::Unknown(
        LockUnknown::SessionDictionaryUnavailable,
    ));
    harness.tick();
    wait_until("the probe failure condition", || {
        harness.conditions().contains(&KeepAwakeConditionKind::LockProbeFailed)
    });
    harness.tick();
    std::thread::sleep(Duration::from_millis(30));
    assert!(harness.probe.calls() > calls);
    assert_eq!(harness.conditions(), vec![
        KeepAwakeConditionKind::ObserverUnavailable,
        KeepAwakeConditionKind::LockProbeFailed,
    ]);
    assert_eq!(
        harness.captured.count_containing("condition set: LockProbeFailed"),
        1
    );
    assert_eq!(
        harness.captured.count_containing("condition set: ObserverUnavailable"),
        1
    );

    harness.set_enabled(false).expect("stop");
    assert!(harness.conditions().is_empty());
    harness.probe.set(NativeLockState::Unlocked);
    harness.set_enabled(true).expect("start again");
    assert_eq!(
        harness.runtime().observation,
        LockObservation::NotificationsAndPolling
    );
    assert!(harness.conditions().is_empty());
    assert_eq!(harness.observation.install_calls().len(), 2);
}

#[test]
fn observer_probe_and_release_conditions_coexist_in_stable_order() {
    let harness = Harness::new(true);
    harness
        .observation
        .fail_next_install(LockObservationError::Unavailable("test-only failure".to_owned()));
    harness.start().expect("start_all");
    harness.backend.fail_release_always(AssertionKind::System, RELEASE_FAILURE_CODE);
    harness.probe.set(NativeLockState::Locked);
    harness.tick();
    wait_assertion(&harness, AssertionState::ReleasePending);

    harness.probe.set(NativeLockState::Unknown(
        LockUnknown::SessionDictionaryUnavailable,
    ));
    harness.tick();
    wait_until("the probe condition", || {
        harness.conditions().contains(&KeepAwakeConditionKind::LockProbeFailed)
    });

    assert_eq!(harness.conditions(), vec![
        KeepAwakeConditionKind::ObserverUnavailable,
        KeepAwakeConditionKind::LockProbeFailed,
        KeepAwakeConditionKind::AssertionReleaseFailed,
    ]);
    assert_eq!(harness.status(), ModuleStatus::Running);
}

#[test]
fn a_runtime_unknown_probe_suppresses_acquisition_and_releases() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    wait_assertion(&harness, AssertionState::Active);

    harness.probe.set(NativeLockState::Unknown(
        LockUnknown::SessionDictionaryUnavailable,
    ));
    assert!(harness.deliver());
    wait_assertion(&harness, AssertionState::Inactive);
    assert_eq!(harness.runtime().lock_state, LockState::Unknown);
    assert_eq!(harness.conditions(), vec![
        KeepAwakeConditionKind::LockProbeFailed
    ]);
    assert_eq!(harness.backend.created().len(), 3);

    harness.probe.set(NativeLockState::Unlocked);
    assert!(harness.deliver());
    wait_assertion(&harness, AssertionState::Active);
    assert_eq!(harness.backend.created().len(), 6);
    assert!(harness.runtime().conditions.is_empty());
}

#[test]
fn pending_ids_block_a_successor_lease_and_release_exactly_once() {
    let harness = Harness::new(true);
    harness.backend.set_ids([11, 12, 13]);
    harness.start().expect("start_all");
    harness.backend.fail_release_always(AssertionKind::Idle, RELEASE_FAILURE_CODE);
    harness.probe.set(NativeLockState::Locked);
    assert!(harness.deliver());
    wait_assertion(&harness, AssertionState::ReleasePending);
    assert_eq!(harness.owner.pending().len(), 1);

    // Unlock while the cleanup gate is still unresolved: no new lease.
    harness.probe.set(NativeLockState::Unlocked);
    harness.tick();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(harness.runtime().assertion_state, AssertionState::ReleasePending);
    assert_eq!(harness.backend.create_attempts().len(), 3);
    assert_eq!(harness.owner.pending().len(), 1);

    harness.backend.clear_failures();
    harness.tick();
    wait_assertion(&harness, AssertionState::Active);
    assert!(harness.owner.pending().is_empty());
    assert_eq!(harness.backend.create_attempts().len(), 6);
    assert_eq!(harness.runtime().conditions.len(), 0);

    // The retained ID was attempted again exactly once and is now inert.
    let attempts = harness.backend.release_attempts();
    let id = AssertionId::new(12);
    assert!(attempts.iter().any(|(_, attempted)| *attempted == id));
    let before = attempts.len();
    let report = harness.owner.cleanup_pending();
    assert!(report.pending().is_empty());
    assert_eq!(harness.backend.release_attempts().len(), before);
}

#[test]
fn queries_read_only_the_published_cell() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    let created = harness.backend.created().len();
    let attempts = harness.backend.release_attempts().len();
    let installs = harness.observation.install_calls().len();
    let probes = harness.probe.calls();
    for _ in 0..100 {
        let runtime = harness.snapshot.read();
        assert_eq!(runtime, harness.runtime());
        assert_eq!(runtime.assertion_state, AssertionState::Active);
    }
    assert_eq!(harness.backend.created().len(), created);
    assert_eq!(harness.backend.release_attempts().len(), attempts);
    assert_eq!(harness.observation.install_calls().len(), installs);
    assert_eq!(harness.probe.calls(), probes);

    // A module that was never started has an independent, inert reader.
    let backend = FakeAssertionBackend::new();
    let owner = AssertionOwner::new(Arc::new(backend.clone()));
    let executor = Arc::new(ManualExecutor::new(true));
    let installer = LockObservationInstaller::new(
        Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
        Arc::new(FakeLockObservationBackend::new()) as Arc<dyn LockObservationBackend>,
    );
    let module = KeepAwakeModule::new(
        owner,
        Arc::new(FakeLockProbe::new(NativeLockState::Unlocked)) as Arc<dyn LockProbe>,
        installer,
    );
    let reader = module.snapshot_reader();
    assert_eq!(reader.read(), KeepAwakeRuntimeStatus::default());
    assert!(backend.create_attempts().is_empty());
    assert!(backend.release_attempts().is_empty());
}

#[test]
fn a_failed_worker_spawn_cleans_up_every_resource() {
    let harness = Harness::build(
        true,
        NativeLockState::Unlocked,
        KeepAwakeModule::with_spawn_failure,
    );
    let error = harness.start().expect_err("the injected spawn failure must fail the start");
    assert_eq!(failure(&error), (Operation::Start, ErrorCategory::Platform));
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert_eq!(harness.observation.install_calls().len(), 1);
    assert_eq!(harness.observation.remove_calls().len(), 1);
    assert!(!harness.observation.is_installed());
    assert_eq!(harness.backend.created().len(), 3);
    assert_eq!(harness.backend.release_attempts().len(), 3);
    assert!(harness.owner.pending().is_empty());
}

#[test]
fn a_duplicate_registration_is_rejected_without_native_effects() {
    let registry = Arc::new(crate::test_support::registry(Arc::new(
        crate::test_support::CaptureLogger::default(),
    )));
    let first_backend = FakeAssertionBackend::new();
    let second_backend = FakeAssertionBackend::new();
    let build = |backend: &FakeAssertionBackend| {
        let owner = AssertionOwner::new(Arc::new(backend.clone()));
        let executor = Arc::new(ManualExecutor::new(true));
        let installer = LockObservationInstaller::new(
            Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
            Arc::new(FakeLockObservationBackend::new()) as Arc<dyn LockObservationBackend>,
        );
        KeepAwakeModule::new(
            owner,
            Arc::new(FakeLockProbe::new(NativeLockState::Unlocked)) as Arc<dyn LockProbe>,
            installer,
        )
    };
    registry
        .register(build(&first_backend), KeepAwakeConfig { enabled: true })
        .expect("first registration");
    let error = registry
        .register(build(&second_backend), KeepAwakeConfig { enabled: true })
        .expect_err("the duplicate must be rejected");
    assert!(matches!(error, RegistryError::DuplicateModule { .. }));
    assert!(first_backend.create_attempts().is_empty());
    assert!(second_backend.create_attempts().is_empty());
}

#[test]
fn the_lock_probe_runs_before_the_first_creation() {
    let harness = Harness::new(true);
    harness.backend.set_ids([11, 12, 13]);
    harness.start().expect("start_all");
    let observed = harness.probe.observed_creates();
    assert_eq!(observed[0], 0, "the pre-acquisition probe saw no assertion");
    assert_eq!(observed[1], 3, "the validation probe follows the complete lease");
    assert_eq!(harness.probe.calls(), 2);
}

#[test]
fn a_runtime_acquisition_failure_rolls_back_and_retries_on_the_timer() {
    let harness = Harness::locked();
    harness.start().expect("start_all");
    harness.probe.set(NativeLockState::Unlocked);
    harness.backend.fail_next_create(AssertionKind::Idle, FAILURE_CODE);
    assert!(harness.deliver());
    wait_until("the failed acquisition", || {
        harness.conditions().contains(&KeepAwakeConditionKind::AssertionAcquireFailed)
    });
    assert_eq!(harness.runtime().assertion_state, AssertionState::Inactive);
    assert!(!is_release_failed(&harness.runtime()));
    assert_eq!(harness.backend.created().len(), 1);
    assert_eq!(harness.backend.release_attempts().len(), 1);
    assert!(harness.owner.pending().is_empty());
    assert_eq!(harness.status(), ModuleStatus::Running);

    harness.tick();
    wait_assertion(&harness, AssertionState::Active);
    assert!(harness.runtime().conditions.is_empty());
    assert_eq!(harness.backend.created().len(), 4);
}

#[test]
fn stage_one_invalidation_during_the_initial_probe_prevents_acquisition() {
    let harness = Harness::new(true);
    let probe = harness.probe.block_next();
    let registry = Arc::clone(&harness.registry);
    let start = std::thread::spawn(move || registry.start_all());
    assert!(probe.wait_entered(WAIT), "the initial probe must be parked");

    // Stage-1 invalidation is logical only: it returns while native work is
    // parked and never probes, creates or releases anything itself.
    harness.registry.invalidate();
    assert!(harness.backend.create_attempts().is_empty());
    assert!(harness.backend.release_attempts().is_empty());
    assert!(harness.observation.remove_calls().is_empty());

    assert!(probe.release());
    let error = start.join().expect("start thread").expect_err("teardown raced the start");
    assert_eq!(failure(&error), (Operation::Start, ErrorCategory::Lifecycle));
    assert!(harness.backend.create_attempts().is_empty());
    assert!(harness.owner.pending().is_empty());
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert!(!harness.observation.is_installed());
}

#[test]
fn invalidation_during_a_blocked_creation_stops_before_the_next_id() {
    let harness = Harness::locked();
    harness.start().expect("start_all");
    harness.probe.set(NativeLockState::Unlocked);
    let create = harness.backend.block_next_create_of(AssertionKind::Idle);
    let release = harness.backend.block_next_release();
    assert!(harness.deliver());
    assert!(create.wait_entered(WAIT), "the idle creation must be parked");
    assert_eq!(harness.backend.created().len(), 1, "only display was created");
    assert_eq!(harness.runtime().assertion_state, AssertionState::Acquiring);

    let registry = Arc::clone(&harness.registry);
    let (sender, receiver) = mpsc::channel();
    let invalidator = std::thread::spawn(move || {
        registry.invalidate();
        let _ = sender.send(());
    });
    assert_eq!(
        receiver.recv_timeout(WAIT),
        Ok(()),
        "invalidation must return without waiting for native work"
    );
    invalidator.join().expect("invalidation thread");
    assert_ne!(
        harness.runtime().assertion_state,
        AssertionState::Active,
        "an invalidated candidate is never adopted"
    );

    // The parked creation observes the cancelled token, so the owner stops at
    // the display prefix and publishes `releasePending` before rolling it back.
    assert!(release.wait_entered(WAIT), "the rollback release must start");
    let runtime = harness.runtime();
    assert_eq!(runtime.assertion_state, AssertionState::ReleasePending);
    assert!(!is_release_failed(&runtime), "{runtime:?}");
    assert_eq!(harness.backend.release_attempts().len(), 1);

    assert!(release.release());
    wait_assertion(&harness, AssertionState::Inactive);
    assert_eq!(harness.backend.create_attempts(), vec![AssertionKind::Display]);
    assert_eq!(harness.backend.created().len(), 1);
    assert_eq!(harness.backend.release_attempts().len(), 1);
    assert!(harness.owner.pending().is_empty());
    assert!(!is_release_failed(&harness.runtime()));
}

#[test]
fn invalidation_while_a_candidate_waits_in_validation_never_adopts_it() {
    let harness = Harness::locked();
    let validation = block_a_candidate_in_validation(&harness);
    assert_eq!(harness.runtime().assertion_state, AssertionState::Acquiring);
    let release = harness.backend.block_next_release();

    harness.registry.invalidate();
    assert_ne!(harness.runtime().assertion_state, AssertionState::Active);
    assert!(
        harness.backend.release_attempts().is_empty(),
        "invalidation itself releases nothing"
    );

    assert!(validation.release());
    assert!(release.wait_entered(WAIT), "the rollback release must start");
    let runtime = harness.runtime();
    assert_eq!(runtime.assertion_state, AssertionState::ReleasePending);
    assert!(!is_release_failed(&runtime), "{runtime:?}");

    assert!(release.release());
    wait_assertion(&harness, AssertionState::Inactive);
    assert_eq!(harness.backend.release_attempts().len(), 3);
    assert!(harness.owner.pending().is_empty());
    assert_eq!(harness.backend.created().len(), 3);
}

#[test]
fn invalidation_after_active_releases_nothing_until_stop() {
    let harness = Harness::new(true);
    harness.start().expect("start_all");
    assert_eq!(harness.runtime().assertion_state, AssertionState::Active);
    let probes = harness.probe.calls();
    let created = harness.backend.created().len();

    harness.registry.invalidate();

    assert_eq!(harness.runtime().assertion_state, AssertionState::Active);
    assert!(
        harness.backend.release_attempts().is_empty(),
        "invalidation is logical; physical release stays in stop"
    );
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(
        harness.probe.calls(),
        probes,
        "the worker exits without probing again"
    );
    assert_eq!(harness.backend.created().len(), created);

    harness.registry.shutdown().expect("stop releases the retained lease");
    assert_eq!(harness.backend.release_attempts().len(), 3);
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert!(harness.owner.pending().is_empty());
}

#[test]
fn a_start_racing_an_already_invalidated_handle_never_acquires() {
    let harness = Harness::new(true);
    let install = harness.observation.block_next_install();
    let registry = Arc::clone(&harness.registry);
    let start = std::thread::spawn(move || registry.start_all());
    assert!(
        install.wait_entered(WAIT),
        "the observation install must be parked"
    );

    harness.registry.invalidate();
    assert!(install.release());

    let error = start.join().expect("start thread").expect_err("teardown raced the start");
    assert_eq!(failure(&error), (Operation::Start, ErrorCategory::Lifecycle));
    assert_eq!(
        harness.probe.calls(),
        0,
        "an already-invalidated handle never probes"
    );
    assert!(harness.backend.create_attempts().is_empty());
    assert!(harness.backend.release_attempts().is_empty());
    assert!(harness.owner.pending().is_empty());
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert!(!harness.observation.is_installed());
}

#[test]
fn invalidation_cancels_only_the_native_token_and_still_stops_native_work() {
    let harness = Harness::locked();
    harness.start().expect("start_all");
    harness.probe.set(NativeLockState::Unlocked);
    let create = harness.backend.block_next_create_of(AssertionKind::Idle);
    let release = harness.backend.block_next_release();
    assert!(harness.deliver());
    assert!(create.wait_entered(WAIT), "the idle creation must be parked");

    let (sdk_token, native_token) = harness.tokens();
    assert!(!sdk_token.is_cancelled(), "start leaves the SDK token alone");
    assert!(!native_token.is_cancelled());

    harness.registry.invalidate();

    assert!(
        native_token.is_cancelled(),
        "invalidation cancels this generation's own native work"
    );
    assert!(
        !sdk_token.is_cancelled(),
        "the module must not cancel the SDK-owned lifecycle token"
    );

    // The parked creation still stops before the next ID because the owner
    // observes the module-owned token, not the SDK one.
    assert!(release.wait_entered(WAIT), "the rollback release must start");
    assert!(release.release());
    wait_assertion(&harness, AssertionState::Inactive);
    assert_eq!(harness.backend.create_attempts(), vec![AssertionKind::Display]);
    assert_eq!(harness.backend.created().len(), 1);
    assert!(harness.owner.pending().is_empty());
    assert!(!sdk_token.is_cancelled());

    // The registry owns the lifecycle token and cancels it before `stop`.
    harness.registry.shutdown().expect("stop");
    assert!(sdk_token.is_cancelled(), "the registry cancels its own token");
    assert_eq!(harness.runtime(), KeepAwakeRuntimeStatus::default());
    assert!(harness.owner.pending().is_empty());
}

mod published_state {
    use stache_sdk::{
        ModuleId, PublishedState, RuntimeEnd, SchemaIdentity, StateEnvelope, StateReading,
        StateSchema, StateUnavailableReason,
    };

    use super::*;

    fn envelope(harness: &Harness) -> Option<StateEnvelope> {
        match harness.states.read().state {
            StateReading::Available(envelope) => Some(envelope),
            _ => None,
        }
    }

    fn published(harness: &Harness) -> Option<KeepAwakeRuntimeStatus> {
        envelope(harness)
            .map(|envelope| envelope.decode(KeepAwakeConfig::ID).expect("the codec decodes"))
    }

    fn wait_published(harness: &Harness, expected: &KeepAwakeRuntimeStatus) {
        wait_until("the published state", || {
            published(harness).as_ref() == Some(expected)
        });
    }

    fn tombstone(harness: &Harness) -> Option<StateUnavailableReason> {
        match harness.states.read().state {
            StateReading::Unavailable(tombstone) => Some(tombstone.reason),
            _ => None,
        }
    }

    #[test]
    fn the_schema_identity_is_owned_by_keep_awake() {
        assert_eq!(KeepAwakeRuntimeStatus::SCHEMA, StateSchema {
            name: "keepAwakeStatus",
            version: 1,
        });
        let harness = Harness::new(true);
        harness.start().expect("start_all");
        let envelope = envelope(&harness).expect("published after start");
        assert_eq!(envelope.schema, SchemaIdentity {
            provider: ModuleId::new("keepAwake"),
            name: "keepAwakeStatus",
            version: 1,
        });
        // The envelope carries the accepted revision the watch showed.
        let accepted = harness.registry.accepted_watch(harness.handle).expect("watch").read();
        assert_eq!(envelope.revision, accepted.expect("accepted").revision);
    }

    #[test]
    fn start_releases_the_final_held_state_and_never_acquires_for_real() {
        let harness = Harness::new(true);
        harness.start().expect("start_all");
        let status = published(&harness).expect("published");
        assert_eq!(status.assertion_state, AssertionState::Active);
        assert_eq!(status.lock_state, LockState::Unlocked);
        assert_eq!(status, harness.runtime());
        // Only the injected fake backend ever created assertions.
        assert_eq!(harness.backend.created().len(), 3);
    }

    #[test]
    fn every_observable_transition_is_published() {
        let harness = Harness::new(true);
        harness.start().expect("start_all");
        wait_published(&harness, &harness.runtime());

        harness.probe.set(NativeLockState::Locked);
        assert!(harness.deliver());
        wait_assertion(&harness, AssertionState::Inactive);
        let locked = published(&harness);
        wait_until("locked published", || {
            published(&harness).is_some_and(|status| status.lock_state == LockState::Locked)
        });
        assert_eq!(
            published(&harness).expect("locked").assertion_state,
            AssertionState::Inactive
        );
        drop(locked);

        harness.probe.set(NativeLockState::Unlocked);
        let barrier = harness.backend.block_next_create();
        assert!(harness.deliver());
        assert!(barrier.wait_entered(WAIT));
        wait_until("acquiring published", || {
            published(&harness)
                .is_some_and(|status| status.assertion_state == AssertionState::Acquiring)
        });
        assert!(barrier.release());
        wait_published(&harness, &KeepAwakeRuntimeStatus {
            assertion_state: AssertionState::Active,
            lock_state: LockState::Unlocked,
            observation: LockObservation::NotificationsAndPolling,
            conditions: Vec::new(),
        });
    }

    #[test]
    fn an_unknown_lock_probe_is_published_as_unknown_and_never_active() {
        let harness = Harness::new(true);
        harness.start().expect("start_all");
        harness.probe.set(NativeLockState::Unknown(
            LockUnknown::SessionDictionaryUnavailable,
        ));
        assert!(harness.deliver());
        wait_until("unknown lock published", || {
            published(&harness).is_some_and(|status| status.lock_state == LockState::Unknown)
        });
        assert_eq!(published(&harness), Some(harness.runtime()));
        assert!(
            published(&harness)
                .expect("published")
                .conditions
                .iter()
                .any(|condition| condition.kind == KeepAwakeConditionKind::LockProbeFailed),
            "the failed probe is a published condition"
        );
    }

    #[test]
    fn a_start_failure_publishes_nothing_available() {
        let harness = Harness::build(
            true,
            NativeLockState::Unknown(LockUnknown::SessionDictionaryUnavailable),
            |module| module,
        );
        assert!(harness.start().is_err());
        assert!(envelope(&harness).is_none());
    }

    #[test]
    fn disable_pause_and_failed_release_tombstone_the_channel() {
        let harness = Harness::new(true);
        harness.start().expect("start_all");
        assert!(published(&harness).is_some());

        harness.pause().expect("pause");
        assert_eq!(
            tombstone(&harness),
            Some(StateUnavailableReason::Ended(RuntimeEnd::Paused))
        );

        harness.resume().expect("resume");
        wait_until("republished after resume", || published(&harness).is_some());
        let sequence = envelope(&harness).expect("published").sequence;
        assert!(sequence >= 1);

        harness.set_enabled(false).expect("disable");
        assert_eq!(
            tombstone(&harness),
            Some(StateUnavailableReason::Ended(RuntimeEnd::Disabled))
        );
    }

    #[test]
    fn a_new_generation_restarts_the_sequence() {
        let harness = Harness::new(true);
        harness.start().expect("start_all");
        let first = envelope(&harness).expect("first");
        harness.pause().expect("pause");
        harness.resume().expect("resume");
        wait_until("second generation", || envelope(&harness).is_some());
        let second = envelope(&harness).expect("second");
        assert_ne!(first.generation, second.generation);
        assert!(second.sequence >= 1);
    }

    #[test]
    fn a_failed_release_never_leaves_stale_active_data() {
        let harness = Harness::new(true);
        harness.start().expect("start_all");
        harness
            .backend
            .fail_release_always(AssertionKind::Display, RELEASE_FAILURE_CODE);
        assert!(harness.set_enabled(false).is_err());
        assert!(
            envelope(&harness).is_none(),
            "the ended generation reads unavailable"
        );
        assert!(matches!(
            tombstone(&harness),
            Some(StateUnavailableReason::Ended(
                RuntimeEnd::Disabled | RuntimeEnd::Failed
            ))
        ));
        // The truthful pending state stays in the module's own snapshot.
        assert_eq!(harness.runtime().assertion_state, AssertionState::ReleasePending);
    }

    #[test]
    fn a_start_failure_never_exposes_available_state() {
        let harness = Harness::build(
            true,
            NativeLockState::Unlocked,
            KeepAwakeModule::with_spawn_failure,
        );
        assert!(harness.start().is_err());
        assert!(envelope(&harness).is_none());
    }
}
