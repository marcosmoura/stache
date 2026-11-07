//! KB-04-02, KB-04-05, KB-04-06: tap-before-remap, invalidate-first release and
//! truthful retained cleanup.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use stache_macos::caps_remap::testing::SetFault;
use stache_macos::caps_remap::{ApplyError, F18_USAGE, Mapping, OWNED, RestoreOutcome};
use stache_sdk::input::testing::FakeResponse;
use stache_sdk::input::{
    ControlAction, Disposition, InputError, InputEventKind, NativeCleanup, TapKind,
};

use super::rig::{F18, KEY_A, Rig, S, token};
use crate::caps::owner::{CapsStartError, CapsStatus};
use crate::caps::parser::CapsKey;

const BINDINGS: [CapsKey; 1] = [CapsKey(S)];

#[test]
fn kb_04_02_the_remap_is_applied_only_after_the_tap_is_confirmed_ready() {
    let rig = Rig::new(vec![KEY_A]);
    let ready_at_set = Arc::new(AtomicBool::new(false));
    let seen_ready = Arc::clone(&ready_at_set);
    let service = Arc::clone(&rig.service);
    let backend = Arc::clone(&rig.backend);
    let owner = rig.owner_watching(move |call| {
        if call == "set" {
            let ready = service.tap_status(TapKind::Suppressing)
                == stache_sdk::input::TapStatus::Ready
                && backend.is_live(TapKind::Suppressing);
            seen_ready.store(ready, Ordering::SeqCst);
        }
    });
    owner.start(&BINDINGS, rig.sink(), &token()).expect("start");
    assert!(
        ready_at_set.load(Ordering::SeqCst),
        "hidutil ran with an acknowledged tap"
    );
    assert_eq!(owner.status(), CapsStatus::Active);
    assert_eq!(rig.world.mappings(), vec![KEY_A, OWNED]);
    assert!(owner.has_subscription());
}

#[test]
fn kb_04_02_a_missing_permission_failed_install_or_timeout_never_mutates_caps() {
    // Install refused (for example no Input Monitoring grant).
    let rig = Rig::new(vec![KEY_A]);
    rig.backend.script(
        TapKind::Suppressing,
        ControlAction::Install,
        FakeResponse::Fail("not permitted".to_owned()),
    );
    let owner = rig.owner();
    let error = owner.start(&BINDINGS, rig.sink(), &token()).expect_err("no tap");
    assert!(
        matches!(error, CapsStartError::Input(InputError::Native { .. })),
        "{error:?}"
    );
    assert_eq!(
        (rig.world.acquisitions(), rig.world.gets(), rig.world.sets()),
        (0, 0, 0)
    );
    assert_eq!(rig.world.journal(), None);
    assert_eq!(owner.status(), CapsStatus::Inactive);

    // Enable never acknowledged: a bounded 2 s timeout, then nothing remapped.
    let rig = Rig::new(vec![KEY_A]);
    rig.backend
        .script(TapKind::Suppressing, ControlAction::Enable, FakeResponse::Drop);
    let owner = rig.owner();
    let error = owner.start(&BINDINGS, rig.sink(), &token()).expect_err("timeout");
    assert!(matches!(error, CapsStartError::Input(_)), "{error:?}");
    assert!(rig.clock.waits().iter().sum::<Duration>() >= Duration::from_secs(2));
    assert_eq!((rig.world.acquisitions(), rig.world.sets()), (0, 0));

    // Cancelled readiness.
    let rig = Rig::new(vec![KEY_A]);
    let cancelled = token();
    cancelled.cancel();
    let error = rig.owner().start(&BINDINGS, rig.sink(), &cancelled).expect_err("cancelled");
    assert!(
        matches!(error, CapsStartError::Input(InputError::Cancelled)),
        "{error:?}"
    );
    assert_eq!((rig.world.acquisitions(), rig.world.sets()), (0, 0));

    // No native owner at all.
    let rig = Rig::new(vec![KEY_A]);
    let owner = crate::caps::owner::CapsOwner::new(
        stache_sdk::input::InputService::unavailable(),
        Arc::new(rig.world.remap()),
        stache_sdk::input::InputPriority::default(),
    );
    let error = owner.start(&BINDINGS, rig.sink(), &token()).expect_err("unavailable");
    assert!(
        matches!(error, CapsStartError::Input(InputError::Unavailable)),
        "{error:?}"
    );
    assert_eq!((rig.world.acquisitions(), rig.world.sets()), (0, 0));
}

#[test]
fn kb_04_02_a_foreign_mapping_or_an_empty_binding_set_releases_the_tap_without_a_remap() {
    let rig = Rig::new(vec![Mapping {
        src: stache_macos::caps_remap::CAPS_LOCK_USAGE,
        dst: F18_USAGE,
    }]);
    let owner = rig.owner();
    let error = owner.start(&BINDINGS, rig.sink(), &token()).expect_err("unowned F18");
    assert!(
        matches!(error, CapsStartError::Remap(ApplyError::Unowned { .. })),
        "{error:?}"
    );
    assert_eq!(rig.world.sets(), 0);
    assert!(!owner.has_subscription(), "the subscription is released again");
    assert!(!rig.backend.is_live(TapKind::Suppressing), "and so is the tap");

    let rig = Rig::new(vec![]);
    assert_eq!(
        rig.owner().start(&[], rig.sink(), &token()),
        Err(CapsStartError::NoBindings)
    );
    assert_eq!(rig.backend.submit_calls(), 0);
}

#[test]
fn kb_04_06_stop_invalidates_dispatch_before_any_restore_work() {
    let rig = Rig::new(vec![KEY_A]);
    let owner_cell = Arc::new(Mutex::new(Vec::<(&'static str, usize, usize)>::new()));
    let observed = Arc::clone(&owner_cell);
    let service = Arc::clone(&rig.service);
    let sink = Arc::clone(&rig.sink);
    let armed = Arc::new(AtomicBool::new(false));
    let hook_armed = Arc::clone(&armed);
    let owner = rig.owner_watching(move |call| {
        if hook_armed.load(Ordering::SeqCst) {
            // A chord pressed while the restore is running must not enqueue.
            service.dispatch(TapKind::Suppressing, raw(InputEventKind::KeyDown, F18));
            let disposition =
                service.dispatch(TapKind::Suppressing, raw(InputEventKind::KeyDown, S));
            observed.lock().unwrap_or_else(PoisonError::into_inner).push((
                call,
                sink.taken().len(),
                usize::from(disposition == Disposition::Suppress),
            ));
        }
    });
    owner.start(&BINDINGS, rig.sink(), &token()).expect("start");
    // Dispatch works before the stop.
    rig.down(F18);
    rig.down(S);
    assert_eq!(rig.sink.taken(), [S]);
    rig.up(S);
    rig.up(F18);
    armed.store(true, Ordering::SeqCst);
    let report = owner.stop(&token());
    assert!(report.dispatch_invalidated);
    assert_eq!(report.restore, RestoreOutcome::Restored);
    assert_eq!(report.status, CapsStatus::Inactive);
    // A chord key pressed during the restore is swallowed; its tombstone keeps
    // the tap installed until its release, so removal may be deferred.
    assert!(matches!(
        report.native,
        Some(NativeCleanup::Closed | NativeCleanup::NotNeeded | NativeCleanup::Deferred)
    ));
    let seen = owner_cell.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(!seen.is_empty(), "the hook observed the restore");
    assert!(
        seen.iter().all(|(_, enqueued, _)| *enqueued == 1),
        "no command was enqueued during restore: {seen:?}"
    );
    assert_eq!(rig.world.mappings(), vec![KEY_A]);
    assert_eq!(rig.world.journal(), None);
    assert!(!owner.has_subscription());
}

fn raw(kind: InputEventKind, key: u16) -> stache_sdk::input::RawInput {
    stache_sdk::input::RawInput {
        kind,
        stream: stache_sdk::input::PressStream::Key(stache_sdk::input::KeyCode(key)),
        flags: stache_sdk::input::ModifierFlags::NONE,
        repeat: false,
        timestamp: Duration::ZERO,
    }
}

#[test]
fn kb_04_06_a_failed_restore_is_cleanup_pending_with_an_inert_safety_subscription() {
    let rig = Rig::new(vec![KEY_A]);
    let owner = rig.owner();
    owner.start(&BINDINGS, rig.sink(), &token()).expect("start");
    // A press already owned when the stop arrives.
    rig.down(F18);
    rig.down(S);
    assert_eq!(rig.sink.taken(), [S]);

    rig.world.script_set(SetFault::Exited);
    let report = owner.stop(&token());
    assert!(
        matches!(report.restore, RestoreOutcome::CleanupPending(_)),
        "{report:?}"
    );
    assert_eq!(
        report.status,
        CapsStatus::CleanupPending {
            reason: match &report.restore {
                RestoreOutcome::CleanupPending(reason) => reason.clone(),
                _ => unreachable!(),
            },
            f18_exposed: false,
        },
        "the tap still swallows F18, so it is not exposed"
    );
    assert_eq!(report.native, None, "the subscription is retained");
    assert!(owner.has_subscription());
    assert_eq!(rig.world.mappings(), vec![KEY_A, OWNED]);
    // Not Paused-and-restored: a successor cannot acquire.
    assert!(matches!(
        owner.start(&BINDINGS, rig.sink(), &token()),
        Err(CapsStartError::Busy)
    ));

    // The retained subscription swallows F18 and the owned key-ups but cannot
    // enqueue a command.
    assert_eq!(
        rig.up(S),
        Disposition::Suppress,
        "matched key-up of the old press"
    );
    assert_eq!(rig.up(F18), Disposition::Suppress);
    assert_eq!(rig.down(F18), Disposition::Suppress, "F18 stays swallowed");
    assert_eq!(rig.down(S), Disposition::Pass, "a new chord cannot run");
    rig.up(S);
    rig.up(F18);
    assert_eq!(rig.sink.taken(), [S], "no command after the stop");

    // The retry resolves and finally releases the subscription.
    let retry = owner.stop(&token());
    assert_eq!(retry.restore, RestoreOutcome::Restored);
    assert_eq!(retry.status, CapsStatus::Inactive);
    assert!(!owner.has_subscription());
    assert_eq!(rig.world.mappings(), vec![KEY_A]);
    owner
        .start(&BINDINGS, rig.sink(), &token())
        .expect("a fresh start after resolution");
}

#[test]
fn kb_04_06_an_unavailable_tap_with_a_pending_restore_discloses_f18_exposure() {
    let rig = Rig::new(vec![KEY_A]);
    let owner = rig.owner();
    owner.start(&BINDINGS, rig.sink(), &token()).expect("start");
    // The OS disables the tap and re-enabling is never acknowledged.
    rig.backend.script(
        TapKind::Suppressing,
        ControlAction::Enable,
        FakeResponse::Fail("gone".to_owned()),
    );
    rig.service.tap_disabled(TapKind::Suppressing);
    rig.world.script_set(SetFault::Exited);
    let report = owner.check_tap(&token()).expect("tap loss detected");
    assert!(report.dispatch_invalidated);
    match report.status {
        CapsStatus::CleanupPending { f18_exposed, .. } => assert!(f18_exposed, "F18 may leak"),
        other => panic!("expected pending cleanup, got {other:?}"),
    }
    assert!(owner.has_subscription());
    assert_eq!(rig.world.mappings(), vec![KEY_A, OWNED]);
    // Dispatch is never revived to hide the degradation.
    assert!(owner.check_tap(&token()).is_some());
    assert_eq!(rig.sink.taken(), Vec::<u16>::new());
}

#[test]
fn kb_04_06_tap_loss_without_a_held_press_is_detected_and_cleans_up() {
    let rig = Rig::new(vec![KEY_A]);
    let owner = rig.owner();
    owner.start(&BINDINGS, rig.sink(), &token()).expect("start");
    assert!(
        owner.check_tap(&token()).is_none(),
        "a healthy tap needs no cleanup"
    );
    rig.service.tap_disabled(TapKind::Suppressing);
    let report = owner.check_tap(&token()).expect("epoch advanced");
    assert_eq!(report.restore, RestoreOutcome::Restored);
    assert_eq!(report.status, CapsStatus::Inactive);
    assert_eq!(rig.world.mappings(), vec![KEY_A]);
    assert!(!owner.has_subscription());
}

#[test]
fn kb_04_05_an_uncertain_apply_keeps_an_inert_subscription_until_restored() {
    let rig = Rig::new(vec![KEY_A]);
    let owner = rig.owner();
    rig.world.script_set(SetFault::TimedOutApplied { unconfirmed: true });
    let error = owner.start(&BINDINGS, rig.sink(), &token()).expect_err("uncertain");
    assert!(
        matches!(error, CapsStartError::Remap(ref apply) if apply.needs_restore()),
        "{error:?}"
    );
    assert!(matches!(owner.status(), CapsStatus::Uncertain { .. }));
    assert!(owner.has_subscription());
    assert_eq!(
        rig.down(F18),
        Disposition::Suppress,
        "F18 is swallowed while uncertain"
    );
    assert_eq!(rig.down(S), Disposition::Pass);
    assert!(rig.sink.taken().is_empty());
    let report = owner.stop(&token());
    assert_eq!(report.restore, RestoreOutcome::Restored);
    assert_eq!(rig.world.mappings(), vec![KEY_A]);
}

#[test]
fn kb_04_05_a_restore_with_an_unconfirmed_child_is_uncertain_not_restored() {
    let rig = Rig::new(vec![KEY_A]);
    let owner = rig.owner();
    owner.start(&BINDINGS, rig.sink(), &token()).expect("start");
    rig.world.script_set(SetFault::TimedOutApplied { unconfirmed: true });
    rig.world.set_unreaped(1);
    let report = owner.stop(&token());
    assert!(
        matches!(report.status, CapsStatus::Uncertain { .. }),
        "{report:?}"
    );
    assert!(owner.has_subscription());
    assert!(rig.world.journal().is_some());
    rig.world.set_unreaped(0);
    assert_eq!(owner.stop(&token()).restore, RestoreOutcome::Restored);
}

#[test]
fn kb_04_04_losing_ownership_on_stop_is_reported_and_the_foreign_mapping_kept() {
    let rig = Rig::new(vec![KEY_A]);
    let owner = rig.owner();
    owner.start(&BINDINGS, rig.sink(), &token()).expect("start");
    let foreign = Mapping {
        src: stache_macos::caps_remap::CAPS_LOCK_USAGE,
        dst: 0x7_0000_00E0,
    };
    rig.world.external_mappings(vec![KEY_A, foreign]);
    let report = owner.stop(&token());
    assert_eq!(report.status, CapsStatus::OwnershipLost {
        observed_destination: foreign.dst
    });
    assert_eq!(rig.world.mappings(), vec![KEY_A, foreign]);
    assert!(!owner.has_subscription());
}

#[test]
fn kb_04_06_stopping_with_nothing_owned_is_a_quiet_no_op() {
    let rig = Rig::new(vec![KEY_A]);
    let owner = rig.owner();
    let report = owner.stop(&token());
    assert_eq!(report.restore, RestoreOutcome::NoJournal);
    assert_eq!(report.status, CapsStatus::Inactive);
    assert_eq!(
        (rig.world.acquisitions(), rig.world.gets(), rig.world.sets()),
        (0, 0, 0)
    );
}

#[test]
fn kb_04_06_late_stop_and_successor_races_serialize_on_the_lease() {
    let rig = Rig::new(vec![KEY_A]);
    let first = rig.owner();
    first.start(&BINDINGS, rig.sink(), &token()).expect("start");
    // Another cooperating process holds the lease when the stop runs.
    rig.world.hold_lease(true);
    let report = first.stop(&token());
    assert!(
        matches!(report.restore, RestoreOutcome::CleanupPending(_)),
        "{report:?}"
    );
    assert_eq!(
        rig.world.mappings(),
        vec![KEY_A, OWNED],
        "no mutation without the lease"
    );
    assert!(first.has_subscription());
    rig.world.hold_lease(false);
    // A successor is refused while the cleanup is pending, in this owner...
    assert!(matches!(
        first.start(&BINDINGS, rig.sink(), &token()),
        Err(CapsStartError::Busy)
    ));
    // ...and in any other owner sharing the same remap (for example after a
    // reload swapped the module instance).
    let remap = Arc::new(rig.world.remap());
    let sharing = crate::caps::owner::CapsOwner::new(
        Arc::clone(&rig.service),
        Arc::clone(&remap),
        stache_sdk::input::InputPriority::default(),
    );
    let _ = sharing.stop(&token());
    assert_eq!(
        rig.world.mappings(),
        vec![KEY_A],
        "the shared remap's stop restores it"
    );
    // The late stop on the first owner then finds nothing left to do.
    let late = first.stop(&token());
    assert!(late.restore.is_resolved(), "{late:?}");
    assert!(!first.has_subscription());
}
