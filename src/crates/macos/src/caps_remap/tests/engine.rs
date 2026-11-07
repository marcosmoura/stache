//! KB-04-02..05: the ownership transaction over fakes with fault injection.

use stache_sdk::CancellationToken;

use super::super::engine::{ApplyError, RestoreOutcome};
use super::super::journal::{Journal, Phase};
use super::super::mapping::{CAPS_LOCK_USAGE, F18_USAGE, Mapping, OWNED};
use super::super::storage::LeaseFailure;
use super::super::testing::{CrashWhen, SetFault, World};

const KEY_A: Mapping = Mapping {
    src: 0x7_0000_00E1,
    dst: 0x7_0000_00E5,
};
const KEY_B: Mapping = Mapping { src: 100, dst: 200 };
const FOREIGN: Mapping = Mapping {
    src: CAPS_LOCK_USAGE,
    dst: 0x7_0000_00E0,
};

fn token() -> CancellationToken { CancellationToken::new() }

fn journal_phase(world: &World) -> Option<Phase> {
    world
        .journal()
        .map(|bytes| Journal::decode(&bytes).expect("trusted journal").phase)
}

fn position(log: &[&str], point: &str) -> usize {
    log.iter()
        .position(|p| *p == point)
        .unwrap_or_else(|| panic!("{point} in {log:?}"))
}

#[test]
fn kb_04_03_the_journal_is_durable_before_the_native_write_and_verified_before_applied() {
    let world = World::new(vec![KEY_A, KEY_B]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    let log = world.log();
    assert_eq!(log[0], "lease.acquire");
    assert!(position(&log, "journal.fsync_dir") < position(&log, "hidutil.set"));
    let set = position(&log, "hidutil.set");
    assert!(
        log[set + 1..].contains(&"hidutil.get"),
        "reread after the write: {log:?}"
    );
    assert_eq!(world.mappings(), vec![KEY_A, KEY_B, OWNED]);
    assert_eq!(journal_phase(&world), Some(Phase::Applied));
    assert_eq!(remap.pending(), None);
}

#[test]
fn kb_04_03_restore_journals_intent_then_verifies_absence_before_retiring() {
    let world = World::new(vec![KEY_A, KEY_B]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    world.reset_steps();
    let before = world.log().len();
    assert_eq!(remap.restore(&token()), RestoreOutcome::Restored);
    let log = world.log()[before..].to_vec();
    let set = position(&log, "hidutil.set");
    let intent = position(&log, "journal.rename");
    let unlink = position(&log, "journal.unlink");
    assert!(
        intent < set,
        "restore intent is journaled before the write: {log:?}"
    );
    assert!(
        set < unlink,
        "the journal is retired only after the write: {log:?}"
    );
    assert!(
        log[set + 1..unlink].contains(&"hidutil.get"),
        "absence is verified first: {log:?}"
    );
    assert_eq!(world.mappings(), vec![KEY_A, KEY_B]);
    assert_eq!(world.journal(), None);
}

#[test]
fn kb_04_02_a_foreign_or_unproven_caps_mapping_is_refused_without_mutation() {
    for existing in [FOREIGN, Mapping {
        src: CAPS_LOCK_USAGE,
        dst: F18_USAGE,
    }] {
        let world = World::new(vec![KEY_A, existing]);
        let error = world.remap().apply(&token()).expect_err("refused");
        assert!(matches!(error, ApplyError::Unowned { .. }), "{error:?}");
        assert_eq!(world.sets(), 0);
        assert_eq!(world.journal(), None);
        assert_eq!(world.mappings(), vec![KEY_A, existing]);
        assert!(!world.log().iter().any(|p| p.starts_with("journal.create")));
    }
}

#[test]
fn kb_04_02_malformed_duplicate_null_and_quoted_output_refuses_before_any_write() {
    let cases: [&[u8]; 6] = [
        b"(",
        b"Error",
        b"( { HIDKeyboardModifierMappingSrc = \"1\"; HIDKeyboardModifierMappingDst = 2; } )",
        b"( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 1; }, { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 2; } )",
        b"( null )",
        b"\xff\xfe",
    ];
    for raw in cases {
        let world = World::new(vec![]);
        world.raw_get(Some(raw.to_vec()));
        let error = world.remap().apply(&token()).expect_err("refused");
        assert!(matches!(error, ApplyError::Read(_)), "{error:?}");
        assert_eq!(world.sets(), 0);
        assert_eq!(world.journal(), None);
    }
}

#[test]
fn kb_04_02_a_failed_read_or_cancelled_lease_changes_nothing() {
    let world = World::new(vec![KEY_A]);
    world.fail_next("hidutil.get");
    assert!(matches!(world.remap().apply(&token()), Err(ApplyError::Read(_))));
    assert_eq!((world.sets(), world.journal()), (0, None));

    let world = World::new(vec![KEY_A]);
    let cancelled = token();
    cancelled.cancel();
    assert_eq!(
        world.remap().apply(&cancelled),
        Err(ApplyError::Lease(LeaseFailure::Cancelled))
    );
    assert_eq!((world.gets(), world.sets(), world.journal()), (0, 0, None));
}

#[test]
fn kb_04_04_a_busy_lease_mutates_nothing_and_a_second_owner_is_excluded() {
    let world = World::new(vec![KEY_A]);
    world.hold_lease(true);
    assert_eq!(
        world.remap().apply(&token()),
        Err(ApplyError::Lease(LeaseFailure::Busy))
    );
    assert_eq!((world.gets(), world.sets(), world.journal()), (0, 0, None));
    assert_eq!(world.mappings(), vec![KEY_A]);
    world.hold_lease(false);
    world.remap().apply(&token()).expect("free again");
}

#[test]
fn kb_04_04_a_replaced_lock_identity_fails_closed_before_every_mutation() {
    let world = World::new(vec![KEY_A]);
    world.replace_lease(true);
    let error = world.remap().apply(&token()).expect_err("replaced");
    assert!(matches!(error, ApplyError::Journal(_)), "{error:?}");
    assert_eq!((world.sets(), world.journal()), (0, None));

    // Restore with a replaced lock never writes either.
    let world = World::new(vec![KEY_A]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    world.replace_lease(true);
    let outcome = remap.restore(&token());
    assert!(
        matches!(outcome, RestoreOutcome::CleanupPending(_)),
        "{outcome:?}"
    );
    assert_eq!(world.mappings(), vec![KEY_A, OWNED]);
    assert!(world.journal().is_some());
}

#[test]
fn kb_04_03_journal_creation_failures_never_mutate_the_mapping() {
    for point in [
        "journal.create",
        "journal.fsync",
        "journal.rename",
        "journal.fsync_dir",
    ] {
        let world = World::new(vec![KEY_A]);
        world.fail_next(point);
        let error = world.remap().apply(&token()).expect_err(point);
        assert!(matches!(error, ApplyError::Journal(_)), "{point}: {error:?}");
        assert_eq!(world.sets(), 0, "{point}");
        assert_eq!(world.mappings(), vec![KEY_A], "{point}");
        // Whatever intent became durable is reconciled idempotently later.
        world.remap().restore(&token());
        assert_eq!(world.mappings(), vec![KEY_A], "{point}");
        assert_eq!(world.journal(), None, "{point}");
    }
}

#[test]
fn kb_04_03_unverified_restores_never_delete_the_journal() {
    // Native write refused: nothing changed, the journal stays, truthfully pending.
    let world = World::new(vec![KEY_A]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    world.script_set(SetFault::Exited);
    let outcome = remap.restore(&token());
    assert!(
        matches!(outcome, RestoreOutcome::CleanupPending(_)),
        "{outcome:?}"
    );
    assert_eq!(world.mappings(), vec![KEY_A, OWNED]);
    assert_eq!(journal_phase(&world), Some(Phase::RestorePending));
    assert!(remap.pending().is_some());
    assert!(
        matches!(remap.apply(&token()), Err(ApplyError::Blocked(_))),
        "successor is blocked"
    );
    // The retry resolves it.
    assert_eq!(remap.restore(&token()), RestoreOutcome::Restored);
    assert_eq!(remap.pending(), None);
    assert_eq!(world.journal(), None);

    // Reread failure after the write: uncertain, journal retained.
    let world = World::new(vec![]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    world.fail_nth("hidutil.get", 2);
    let outcome = remap.restore(&token());
    assert!(matches!(outcome, RestoreOutcome::Uncertain(_)), "{outcome:?}");
    assert!(
        world.journal().is_some(),
        "an unverified restore never deletes the journal"
    );
    assert!(remap.pending().is_some());
    assert_eq!(remap.restore(&token()), RestoreOutcome::Restored);
}

#[test]
fn kb_04_05_a_post_spawn_timeout_is_uncertain_until_reread_and_child_are_confirmed() {
    // Timed out after applying, child reaped, reread proves absence: restored.
    let world = World::new(vec![KEY_A]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    world.script_set(SetFault::TimedOutApplied { unconfirmed: false });
    assert_eq!(remap.restore(&token()), RestoreOutcome::Restored);
    assert_eq!(world.mappings(), vec![KEY_A]);

    // The same, but the child could not be confirmed gone: never "restored".
    let world = World::new(vec![KEY_A]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    world.script_set(SetFault::TimedOutApplied { unconfirmed: true });
    world.set_unreaped(1);
    let outcome = remap.restore(&token());
    assert!(matches!(outcome, RestoreOutcome::Uncertain(_)), "{outcome:?}");
    assert!(world.journal().is_some(), "journal is retained");
    assert!(remap.pending().is_some());
    // Once the child is confirmed gone a retry retires the journal.
    world.set_unreaped(0);
    assert_eq!(remap.restore(&token()), RestoreOutcome::Restored);

    // Timed out and still mapped with an unconfirmed child: uncertain.
    let world = World::new(vec![]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    world.script_set(SetFault::TimedOutUnchanged { unconfirmed: true });
    let outcome = remap.restore(&token());
    assert!(matches!(outcome, RestoreOutcome::Uncertain(_)), "{outcome:?}");
    assert_eq!(world.mappings(), vec![OWNED]);

    // Timed out, child reaped, mapping verifiably unchanged: pending, retryable.
    let world = World::new(vec![]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    world.script_set(SetFault::TimedOutUnchanged { unconfirmed: false });
    let outcome = remap.restore(&token());
    assert!(
        matches!(outcome, RestoreOutcome::CleanupPending(_)),
        "{outcome:?}"
    );
}

#[test]
fn kb_04_05_an_apply_that_times_out_keeps_the_journal_and_requires_restore() {
    let world = World::new(vec![KEY_A]);
    let remap = world.remap();
    world.script_set(SetFault::TimedOutApplied { unconfirmed: true });
    let error = remap.apply(&token()).expect_err("uncertain");
    assert!(error.needs_restore(), "{error:?}");
    assert_eq!(journal_phase(&world), Some(Phase::ApplyPending));
    assert!(matches!(remap.apply(&token()), Err(ApplyError::Blocked(_))));
    world.set_unreaped(0);
    assert_eq!(remap.restore(&token()), RestoreOutcome::Restored);
    assert_eq!(world.mappings(), vec![KEY_A]);

    // Timed out before applying with the child reaped: verifiably not applied.
    let world = World::new(vec![KEY_A]);
    world.script_set(SetFault::TimedOutUnchanged { unconfirmed: false });
    let error = world.remap().apply(&token()).expect_err("not applied");
    assert!(matches!(error, ApplyError::NotApplied(_)), "{error:?}");
    assert_eq!(world.journal(), None);
    assert_eq!(world.mappings(), vec![KEY_A]);

    // hidutil refused to run: nothing changed and the stale intent is retired.
    let world = World::new(vec![KEY_A]);
    world.script_set(SetFault::NotRun);
    assert!(matches!(
        world.remap().apply(&token()),
        Err(ApplyError::NotApplied(_))
    ));
    assert_eq!(world.journal(), None);
}

#[test]
fn kb_04_04_external_changes_are_preserved_and_ownership_loss_is_never_restored() {
    // Unrelated external edits between apply and restore survive the restore.
    let world = World::new(vec![KEY_A]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    let external = Mapping { src: 7, dst: 8 };
    world.external_mappings(vec![external, KEY_A, OWNED]);
    assert_eq!(remap.restore(&token()), RestoreOutcome::Restored);
    assert_eq!(world.mappings(), vec![external, KEY_A]);

    // Caps changed by someone else: not overwritten, not "restored".
    let world = World::new(vec![KEY_A]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    world.external_mappings(vec![KEY_A, FOREIGN]);
    assert_eq!(remap.restore(&token()), RestoreOutcome::OwnershipLost {
        observed_destination: FOREIGN.dst
    });
    assert_eq!(world.mappings(), vec![KEY_A, FOREIGN]);
    assert_eq!(world.sets(), 1, "only the original apply wrote");
    assert_eq!(journal_phase(&world), Some(Phase::OwnershipLost));
    // Acquisition stays refused while the foreign mapping exists.
    assert!(matches!(remap.apply(&token()), Err(ApplyError::Unowned { .. })));
    // When the foreign mapping disappears the record is retired and apply works.
    world.external_mappings(vec![KEY_A]);
    remap.apply(&token()).expect("reconciled then applied");
    assert_eq!(journal_phase(&world), Some(Phase::Applied));
}

#[test]
fn kb_04_04_a_mapping_changed_during_restore_is_not_overwritten() {
    // Another writer replaces Caps between our write and the verification read.
    let world = World::new(vec![]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    world.script_set(SetFault::Raced(vec![FOREIGN]));
    let outcome = remap.restore(&token());
    assert_eq!(outcome, RestoreOutcome::OwnershipLost {
        observed_destination: FOREIGN.dst
    });
    assert_eq!(world.mappings(), vec![FOREIGN]);
    assert!(world.journal().is_some(), "ownership loss keeps the record");
}

#[test]
fn kb_04_03_a_corrupt_or_foreign_journal_blocks_without_touching_anything() {
    let planted: [&[u8]; 4] = [
        b"not json",
        b"{\"previous_caps_usage\":300}",
        b"",
        b"{\"version\":9,\"identity\":\"com.marcosmoura.stache.next\",\"revision\":1,\"phase\":\"applied\",\"transition\":{\"src\":1,\"dst\":2},\"prior_caps\":\"absent\"}",
    ];
    for bytes in planted {
        let world = World::new(vec![KEY_A, OWNED]);
        world.plant_journal(Some(bytes.to_vec()));
        let remap = world.remap();
        let outcome = remap.restore(&token());
        assert!(matches!(outcome, RestoreOutcome::Blocked(_)), "{outcome:?}");
        assert!(matches!(remap.apply(&token()), Err(ApplyError::Blocked(_))));
        assert_eq!(world.sets(), 0);
        assert_eq!(world.mappings(), vec![KEY_A, OWNED]);
        assert_eq!(world.journal(), Some(bytes.to_vec()), "never deleted");
    }
}

#[test]
fn kb_04_03_a_mapping_without_a_journal_is_never_adopted() {
    let world = World::new(vec![OWNED]);
    let remap = world.remap();
    assert_eq!(remap.restore(&token()), RestoreOutcome::NoJournal);
    assert_eq!(world.sets(), 0);
    assert_eq!(world.mappings(), vec![OWNED]);
    // No journal means no lease and no hidutil at all.
    assert_eq!((world.acquisitions(), world.gets()), (0, 0));
}

#[test]
fn kb_04_03_a_stale_journal_is_reconciled_before_a_fresh_apply() {
    // Crash left an applied mapping and journal; the next apply restores first.
    let world = World::new(vec![KEY_A]);
    world.remap().apply(&token()).expect("apply");
    world.restart();
    let fresh = world.remap();
    fresh.apply(&token()).expect("reconcile then apply");
    assert_eq!(world.mappings(), vec![KEY_A, OWNED]);
    assert_eq!(world.sets(), 3, "apply, restore, apply");
}

#[test]
fn kb_04_05_an_unreaped_earlier_child_blocks_retirement() {
    let world = World::new(vec![KEY_A]);
    let remap = world.remap();
    remap.apply(&token()).expect("apply");
    world.set_unreaped(1);
    world.external_mappings(vec![KEY_A]);
    let outcome = remap.restore(&token());
    assert!(matches!(outcome, RestoreOutcome::Uncertain(_)), "{outcome:?}");
    assert!(world.journal().is_some());
}

/// Every apply crash boundary recovers to the exact starting mapping, and the
/// write-ahead rule holds at every instant: the owned mapping never exists
/// without a trusted journal.
#[test]
fn kb_04_03_every_apply_crash_boundary_recovers() {
    let initial = vec![KEY_A, KEY_B];
    let baseline = World::new(initial.clone());
    baseline.remap().apply(&token()).expect("apply");
    let steps = baseline.steps();
    assert!(steps >= 12, "the apply has many transitions: {steps}");
    for step in 1..=steps {
        for when in [CrashWhen::Before, CrashWhen::After] {
            let world = World::new(initial.clone());
            world.crash_at(step, when);
            let _ = world.remap().apply(&token());
            let state = (step, when, world.log());
            if world.mappings().contains(&OWNED) {
                let journal = world.journal();
                assert!(
                    journal.is_some_and(|bytes| Journal::decode(&bytes).is_ok()),
                    "the owned mapping needs a journal at {state:?}"
                );
            }
            world.restart();
            let outcome = world.remap().restore(&token());
            assert!(outcome.is_resolved(), "{outcome:?} at {state:?}");
            assert_eq!(world.mappings(), initial, "{state:?}");
            assert_eq!(world.journal(), None, "{state:?}");
        }
    }
}

#[test]
fn kb_04_03_every_restore_crash_boundary_recovers() {
    let initial = vec![KEY_A, KEY_B];
    let baseline = World::new(initial.clone());
    let remap = baseline.remap();
    remap.apply(&token()).expect("apply");
    baseline.reset_steps();
    assert_eq!(remap.restore(&token()), RestoreOutcome::Restored);
    let steps = baseline.steps();
    assert!(steps >= 8, "{steps}");
    for step in 1..=steps {
        for when in [CrashWhen::Before, CrashWhen::After] {
            let world = World::new(initial.clone());
            let remap = world.remap();
            remap.apply(&token()).expect("apply");
            world.reset_steps();
            world.crash_at(step, when);
            let _ = remap.restore(&token());
            let state = (step, when, world.log());
            if world.mappings().contains(&OWNED) {
                assert!(
                    world.journal().is_some(),
                    "journal outlives the mapping at {state:?}"
                );
            }
            world.restart();
            assert!(world.remap().restore(&token()).is_resolved(), "{state:?}");
            assert_eq!(world.mappings(), initial, "{state:?}");
            assert_eq!(world.journal(), None, "{state:?}");
        }
    }
}

#[test]
fn kb_04_03_recovery_itself_survives_a_crash_at_every_boundary() {
    let initial = vec![KEY_A];
    let seed = |world: &World| {
        world.remap().apply(&token()).expect("apply");
        world.restart();
        world.reset_steps();
    };
    let baseline = World::new(initial.clone());
    seed(&baseline);
    assert_eq!(baseline.remap().restore(&token()), RestoreOutcome::Restored);
    let steps = baseline.steps();
    for step in 1..=steps {
        for when in [CrashWhen::Before, CrashWhen::After] {
            let world = World::new(initial.clone());
            seed(&world);
            world.crash_at(step, when);
            let _ = world.remap().restore(&token());
            world.restart();
            assert!(world.remap().restore(&token()).is_resolved(), "{step} {when:?}");
            assert_eq!(world.mappings(), initial, "{step} {when:?}");
            assert_eq!(world.journal(), None, "{step} {when:?}");
        }
    }
}

#[test]
fn kb_04_03_a_crash_after_the_write_but_before_the_phase_update_is_recovered() {
    let world = World::new(vec![KEY_A]);
    // Crash right after hidutil.set takes effect (the acknowledgement is lost).
    let baseline = World::new(vec![KEY_A]);
    baseline.remap().apply(&token()).expect("apply");
    let set_step = baseline.log().iter().position(|p| *p == "hidutil.set").expect("set") + 1;
    world.crash_at(set_step, CrashWhen::After);
    assert!(world.remap().apply(&token()).is_err());
    assert_eq!(world.mappings(), vec![KEY_A, OWNED]);
    assert_eq!(journal_phase(&world), Some(Phase::ApplyPending));
    world.restart();
    assert_eq!(world.remap().restore(&token()), RestoreOutcome::Restored);
    assert_eq!(world.mappings(), vec![KEY_A]);
}

// ---------------------------------------------------------------------------
// KB-04-07: apply is ordered after an armed startup recovery
// ---------------------------------------------------------------------------

use std::sync::Arc;
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

use super::super::engine::CapsRemap;
use super::super::hidutil::{Hidutil, HidutilError};

fn stale(world: &World) {
    let first = world.remap();
    first.apply(&token()).expect("apply");
    world.restart();
}

#[test]
fn kb_04_07_apply_blocks_until_the_startup_recovery_completes_then_proceeds() {
    let world = World::new(vec![KEY_A]);
    stale(&world);
    let remap = Arc::new(world.remap());
    remap.begin_startup_recovery();
    let (started, started_rx) = channel();
    let applier = {
        let remap = Arc::clone(&remap);
        thread::spawn(move || {
            started.send(()).expect("signal");
            remap.apply(&token())
        })
    };
    started_rx.recv().expect("applier running");
    // While the recovery has not run, apply has touched nothing.
    let sets = world.sets();
    assert!(!applier.is_finished(), "apply waits for the recovery");
    assert_eq!(world.sets(), sets, "and has mutated nothing");
    assert_eq!(remap.restore_at_startup(&token()), RestoreOutcome::Restored);
    applier.join().expect("applier").expect("apply after recovery");
    let log = world.log();
    let restore = log.iter().position(|p| *p == "hidutil.set").expect("restore write");
    let second = log.iter().rposition(|p| *p == "hidutil.set").expect("apply write");
    assert!(restore < second, "recovery wrote first: {log:?}");
    assert_eq!(world.mappings(), vec![KEY_A, OWNED]);
    assert_eq!(
        journal_phase(&world),
        Some(Phase::Applied),
        "the fresh journal survives"
    );
}

struct PanicHidutil;

impl Hidutil for PanicHidutil {
    fn get(&self, _: &CancellationToken) -> Result<Vec<u8>, HidutilError> { panic!("hidutil") }

    fn set(&self, _: &str, _: &CancellationToken) -> Result<(), HidutilError> { panic!("hidutil") }

    fn unreaped(&self) -> usize { 0 }
}

#[test]
fn kb_04_07_a_panicking_recovery_still_releases_apply() {
    let world = World::new(vec![KEY_A]);
    stale(&world);
    let panicking = Arc::new(CapsRemap::new(
        Arc::new(PanicHidutil),
        world.storage(),
        world.leases(),
    ));
    panicking.begin_startup_recovery();
    let crashed = {
        let remap = Arc::clone(&panicking);
        thread::spawn(move || remap.restore_at_startup(&token())).join()
    };
    assert!(crashed.is_err(), "the recovery panicked");
    // The latch is released: apply no longer waits (it proceeds into the lease
    // and the panicking hidutil, which is the point), so it surfaces a panic
    // rather than a recovery wait.
    let applied = {
        let remap = Arc::clone(&panicking);
        thread::spawn(move || remap.apply(&token())).join()
    };
    assert!(applied.is_err(), "apply ran past the latch");
}

#[test]
fn kb_04_07_an_unfinished_recovery_expires_into_a_typed_error_without_mutation() {
    let world = World::new(vec![KEY_A]);
    let remap = world.remap().with_startup_bound(Duration::from_millis(40));
    remap.begin_startup_recovery();
    let error = remap.apply(&token()).expect_err("the bound expires");
    assert!(matches!(error, ApplyError::Recovery(_)), "{error:?}");
    assert_eq!(
        (world.gets(), world.sets(), world.acquisitions(), world.journal()),
        (0, 0, 0, None)
    );
    // Cancellation also stops the wait without mutation.
    let slow = world.remap();
    slow.begin_startup_recovery();
    let cancelled = token();
    cancelled.cancel();
    assert_eq!(
        slow.apply(&cancelled),
        Err(ApplyError::Lease(LeaseFailure::Cancelled))
    );
    assert_eq!(
        (world.sets(), world.acquisitions(), world.journal()),
        (0, 0, None)
    );
}

#[test]
fn kb_04_07_without_an_armed_latch_apply_does_not_wait() {
    let world = World::new(vec![KEY_A]);
    let remap = world.remap().with_startup_bound(Duration::from_millis(1));
    remap.apply(&token()).expect("no recovery registered");
    // A recovery run that was never armed is harmless and releases nothing.
    assert_eq!(remap.restore_at_startup(&token()), RestoreOutcome::Restored);
}

#[test]
fn kb_04_07_an_abandoned_recovery_releases_apply_immediately() {
    let world = World::new(vec![KEY_A]);
    let remap = world.remap().with_startup_bound(Duration::from_secs(60));
    remap.begin_startup_recovery();
    // The recovery step was dropped without running: apply must not wait out
    // the bound.
    remap.abandon_startup_recovery();
    let started = std::time::Instant::now();
    remap.apply(&token()).expect("released");
    assert!(started.elapsed() < Duration::from_secs(5));
}
