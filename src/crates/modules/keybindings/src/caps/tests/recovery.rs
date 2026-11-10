//! KB-04-07: the host's stale-remap recovery entry point.

use std::sync::Arc;

use stache_macos::caps_remap::testing::{CrashWhen, SetFault, World};
use stache_macos::caps_remap::{CAPS_LOCK_USAGE, Mapping, OWNED, RestoreOutcome};

use super::rig::{KEY_A, token};
use crate::caps::recover::{CapsRecoveryStep, recover_stale_caps_remap};

fn stale_world(initial: Vec<Mapping>) -> World {
    let world = World::new(initial);
    // A previous process applied the remap and then died.
    let step_log = world.remap();
    step_log.apply(&token()).expect("apply");
    world.restart();
    world
}

#[test]
fn a_stale_journal_is_restored_exactly_once_whatever_the_module_state() {
    // The step takes no configuration, enabled flag or module handle at all, so
    // a disabled module or an invalid config cannot influence it.
    let world = stale_world(vec![KEY_A]);
    let step = CapsRecoveryStep::new(Arc::new(world.remap()));
    let recovery = step.run(&token());
    assert!(recovery.restored());
    assert_eq!(recovery.diagnostic(), None);
    assert_eq!(world.mappings(), vec![KEY_A]);
    assert_eq!(world.journal(), None);
    assert_eq!(world.sets(), 2, "apply then exactly one restore write");
    // A second run in the same launch is a no-op.
    assert_eq!(step.run(&token()).outcome, RestoreOutcome::NoJournal);
    assert_eq!(world.sets(), 2);
}

#[test]
fn without_a_journal_recovery_does_nothing_even_if_caps_is_mapped() {
    for existing in [vec![KEY_A], vec![OWNED], vec![Mapping {
        src: CAPS_LOCK_USAGE,
        dst: 0x7_0000_00E0,
    }]] {
        let world = World::new(existing.clone());
        let recovery = recover_stale_caps_remap(&world.remap(), &token());
        assert_eq!(recovery.outcome, RestoreOutcome::NoJournal);
        assert_eq!(recovery.diagnostic(), None);
        assert_eq!((world.acquisitions(), world.gets(), world.sets()), (0, 0, 0));
        assert_eq!(
            world.mappings(),
            existing,
            "foreign and legacy mappings are never touched"
        );
    }
}

#[test]
fn a_foreign_mapping_under_our_journal_is_refused_with_a_diagnostic() {
    let world = stale_world(vec![KEY_A]);
    let foreign = Mapping {
        src: CAPS_LOCK_USAGE,
        dst: 0x7_0000_00E0,
    };
    world.external_mappings(vec![KEY_A, foreign]);
    let recovery = recover_stale_caps_remap(&world.remap(), &token());
    assert!(matches!(recovery.outcome, RestoreOutcome::OwnershipLost { .. }));
    let diagnostic = recovery.diagnostic().expect("diagnostic");
    assert!(diagnostic.contains("left untouched"), "{diagnostic}");
    assert_eq!(world.mappings(), vec![KEY_A, foreign]);
}

#[test]
fn corrupt_ambiguous_and_failed_recovery_report_once_and_never_retry() {
    // Corrupt journal.
    let world = World::new(vec![KEY_A, OWNED]);
    world.plant_journal(Some(b"not a journal".to_vec()));
    let recovery = recover_stale_caps_remap(&world.remap(), &token());
    assert!(matches!(recovery.outcome, RestoreOutcome::Blocked(_)));
    assert!(recovery.diagnostic().expect("diagnostic").contains("nothing was changed"));
    assert_eq!(world.sets(), 0);
    assert!(world.journal().is_some());

    // Restore write refused: reported, journal kept, exactly one attempt.
    let world = stale_world(vec![KEY_A]);
    world.script_set(SetFault::Exited);
    let before = world.sets();
    let step = CapsRecoveryStep::new(Arc::new(world.remap()));
    let recovery = step.run(&token());
    assert!(matches!(recovery.outcome, RestoreOutcome::CleanupPending(_)));
    assert!(recovery.diagnostic().expect("diagnostic").contains("not retried"));
    assert_eq!(world.sets(), before, "the refused write did not apply");
    assert!(world.journal().is_some());

    // Lease held elsewhere: reported, no mutation.
    let world = stale_world(vec![KEY_A]);
    world.hold_lease(true);
    let recovery = recover_stale_caps_remap(&world.remap(), &token());
    assert!(matches!(recovery.outcome, RestoreOutcome::CleanupPending(_)));
    assert_eq!(world.mappings(), vec![KEY_A, OWNED]);
}

#[test]
fn recovery_survives_a_crash_at_any_boundary_of_its_own() {
    let baseline = stale_world(vec![KEY_A]);
    baseline.reset_steps();
    let _ = recover_stale_caps_remap(&baseline.remap(), &token());
    for step in 1..=baseline.steps() {
        for when in [CrashWhen::Before, CrashWhen::After] {
            let world = stale_world(vec![KEY_A]);
            world.reset_steps();
            world.crash_at(step, when);
            let _ = recover_stale_caps_remap(&world.remap(), &token());
            world.restart();
            let again = recover_stale_caps_remap(&world.remap(), &token());
            assert!(again.outcome.is_resolved(), "{step} {when:?}");
            assert_eq!(world.mappings(), vec![KEY_A], "{step} {when:?}");
            assert_eq!(world.journal(), None, "{step} {when:?}");
        }
    }
}
