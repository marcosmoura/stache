//! KB-03-02..06: the hotkey owner over the fake Carbon/TIS layer.
//!
//! Nothing here registers a real hotkey, reads a real layout, installs a real
//! observer or touches a permission. The "main thread" is the test thread: a
//! [`ManualExecutor`] runs jobs inline, or queues them so a test can interleave
//! hint, stop and drain deterministically.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use keyboard_types::Code;
use stache_sdk::input::{
    Chord, Claim, InputEventKind, InputFilter, InputPriority, InputService, KeyCode, KindSet,
    ModifierFlags, ReservationBackend, ReservationError, ReservationOwner,
};
use stache_sdk::{CancellationToken, MainThreadExecutor};

use super::native::{Disposition, RawEvent, RawEventKind, SIGNATURE};
use super::testing::{Call, FakeLayout, FakeLog, RecordingSink};
use super::*;
use crate::applications::testing::ManualExecutor;
use crate::applications::{ACKNOWLEDGEMENT_BUDGET, CANCELLATION_QUANTUM};
use crate::keyboard_layout::{FallbackReason, LayoutId, SectionSource};

const CMD: ModifierFlags = ModifierFlags::COMMAND;
const KEY_Q: u16 = 0x0c;
const KEY_W: u16 = 0x0d;
const KEY_E: u16 = 0x0e;
const CARBON_CMD: u32 = 0x0100;
const CARBON_OPT: u32 = 0x0800;
const CARBON_SHIFT: u32 = 0x0200;

struct Rig {
    control: HotkeyControl,
    log: Arc<FakeLog>,
    executor: Arc<ManualExecutor>,
    sink: Arc<RecordingSink>,
    service: Arc<InputService>,
    owner: ReservationOwner,
    cancel: CancellationToken,
}

fn rig_with(on_main: bool, timing: Timing) -> Rig {
    let log = FakeLog::new();
    let executor = Arc::new(ManualExecutor::new(on_main));
    let sink = RecordingSink::new();
    let service = InputService::unavailable();
    let owner = service.reservations().new_owner();
    let control = HotkeyControl::with_timing(
        Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
        log.factory(),
        Arc::clone(&service),
        owner,
        Arc::clone(&sink) as Arc<dyn HotkeySink>,
        timing,
    );
    Rig {
        control,
        log,
        executor,
        sink,
        service,
        owner,
        cancel: CancellationToken::new(),
    }
}

fn rig() -> Rig { rig_with(true, Timing::default()) }

const fn prio(level: u16) -> InputPriority { InputPriority { level, tie_break: 0 } }

fn physical(key: u64, code: Code, modifiers: ModifierFlags) -> HotkeyRequest {
    HotkeyRequest {
        key,
        target: HotkeyTarget::Physical { code, modifiers },
        priority: prio(10),
    }
}

fn section(key: u64, modifiers: ModifierFlags) -> HotkeyRequest {
    HotkeyRequest {
        key,
        target: HotkeyTarget::Section { modifiers },
        priority: prio(10),
    }
}

fn q(key: u64) -> HotkeyRequest { physical(key, Code::KeyQ, CMD) }
fn w(key: u64) -> HotkeyRequest { physical(key, Code::KeyW, CMD) }
fn e(key: u64) -> HotkeyRequest { physical(key, Code::KeyE, CMD) }

impl Rig {
    fn apply(&self, requests: Vec<HotkeyRequest>) -> CommitReport {
        self.control.apply(&self.cancel, requests).expect("apply")
    }

    fn status(&self) -> Status { self.control.status(&self.cancel).expect("status") }

    fn id(&self, key_code: u16, modifiers: u32) -> u32 {
        self.log.id_of(key_code, modifiers).expect("registered chord")
    }

    fn active_ids(report: &CommitReport) -> Vec<(u64, u32)> {
        report
            .outcomes
            .iter()
            .filter_map(|(key, outcome)| match outcome {
                BindingOutcome::Active { token, .. } => Some((*key, token.native_id())),
                _ => None,
            })
            .collect()
    }

    fn hid_claim(
        &self,
        key_code: u16,
        modifiers: ModifierFlags,
        level: u16,
    ) -> stache_sdk::input::ReservationId {
        let hid = self.service.reservations().new_owner();
        self.service
            .reservations()
            .register(
                hid,
                ReservationBackend::Hid,
                Claim::Chord(Chord::new(KeyCode(key_code), modifiers)),
                prio(level),
            )
            .expect("hid claim")
    }
}

fn active_outcome(report: &CommitReport, key: u64) -> &BindingOutcome {
    &report.outcomes.iter().find(|(k, _)| *k == key).expect("outcome").1
}

// ---------------------------------------------------------------- KB-03-02

#[test]
#[allow(clippy::items_after_statements)]
fn kb_03_02_handles_are_send_and_native_objects_are_not() {
    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_send<T: Send>() {}
    assert_send_sync::<HotkeyControl>();
    assert_send_sync::<Scope>();
    assert_send::<BindingToken>();
    assert_send::<StagedId>();
    assert_send::<StagedReport>();
    assert_send::<CommitReport>();
    assert_send::<StopReport>();
    assert_send::<Status>();
    assert_send::<HotkeyError>();

    // Compile-time negative assertions: if any of these ever became Send (for
    // example through an unsafe shim) the impls become ambiguous and the test
    // stops compiling.
    trait AmbiguousIfSend<A> {
        fn marker() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
    trait AmbiguousIfSync<A> {
        fn marker() {}
    }
    impl<T: ?Sized> AmbiguousIfSync<()> for T {}
    impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}
    let _ = <super::ffi::SystemNative as AmbiguousIfSend<_>>::marker;
    let _ = <super::ffi::SystemNative as AmbiguousIfSync<_>>::marker;
    let _ = <super::store::Store as AmbiguousIfSend<_>>::marker;
    let _ = <super::store::Store as AmbiguousIfSync<_>>::marker;
    let _ = <super::native::Dispatch as AmbiguousIfSend<_>>::marker;
}

/// Compile-only: the real Carbon/TIS layer builds into a control. Nothing is
/// executed natively: the control only enqueues, and the executor never runs.
#[test]
fn kb_03_02_the_real_native_layer_builds_without_running() {
    let executor = Arc::new(ManualExecutor::new(false));
    let service = InputService::unavailable();
    let owner = service.reservations().new_owner();
    let control = HotkeyControl::system(
        Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
        service,
        owner,
        RecordingSink::new() as Arc<dyn HotkeySink>,
    );
    drop(control);
    // The drop queued its teardown but nothing ran, so no native layer exists.
    assert_eq!(executor.queued(), 1);
    // An unused real layer owns nothing and its drop makes no Carbon call.
    drop(super::ffi::SystemNative::new());
}

#[test]
fn kb_03_02_handler_install_failure_is_a_distinct_outcome() {
    let rig = rig();
    rig.log.fail_install(Some(-50));
    let report = rig.apply(vec![q(1), w(2)]);

    assert_eq!(report.handler, HandlerStatus::InstallFailed(-50));
    assert!(!report.all_active());
    for key in [1, 2] {
        assert_eq!(
            active_outcome(&report, key),
            &BindingOutcome::HandlerUnavailable { status: -50 }
        );
    }
    assert_eq!(
        rig.log.register_count(),
        0,
        "no chord is attempted without a handler"
    );
    assert!(
        rig.service.reservations().is_empty(),
        "a failed registration publishes nothing"
    );
    assert!(rig.status().active.is_empty());

    // The failure is not sticky: the next commit retries the lazy install.
    rig.log.fail_install(None);
    let report = rig.apply(vec![q(1), w(2)]);
    assert!(report.all_active());
    assert_eq!(report.handler, HandlerStatus::Installed);
}

#[test]
fn kb_03_02_the_handler_is_installed_lazily_and_only_once() {
    let rig = rig();
    assert!(
        rig.log.calls().is_empty(),
        "constructing a control touches nothing native"
    );
    let _ = rig.apply(vec![]);
    assert!(
        !rig.log.calls().contains(&Call::InstallHandler),
        "an empty set needs no handler"
    );
    let _ = rig.apply(vec![q(1)]);
    let _ = rig.apply(vec![q(1), w(2)]);
    let installs = rig.log.calls().iter().filter(|c| **c == Call::InstallHandler).count();
    assert_eq!(installs, 1);
}

#[test]
fn kb_03_02_one_occupied_chord_does_not_hide_the_successes() {
    let rig = rig();
    rig.log.occupy(KEY_W, CARBON_CMD);
    let report = rig.apply(vec![q(1), w(2), e(3)]);

    assert!(active_outcome(&report, 1).is_active());
    assert!(
        matches!(active_outcome(&report, 2), BindingOutcome::Occupied { status } if *status == native::STATUS_HOT_KEY_EXISTS)
    );
    assert!(active_outcome(&report, 3).is_active());
    assert!(
        !report.all_active(),
        "an occupied chord is never reported as active"
    );
    assert_eq!(rig.log.registered_chords(), vec![
        (KEY_Q, CARBON_CMD),
        (KEY_E, CARBON_CMD)
    ]);
    assert_eq!(
        rig.service.reservations().len(),
        2,
        "only successes publish reservations"
    );
}

#[test]
fn kb_03_02_unsupported_keys_fail_per_binding() {
    let rig = rig();
    let report = rig.apply(vec![
        physical(1, Code::MediaPlayPause, CMD),
        physical(2, Code::KeyQ, ModifierFlags::FUNCTION),
        q(3),
    ]);
    assert!(matches!(
        active_outcome(&report, 1),
        BindingOutcome::Unsupported(_)
    ));
    assert!(matches!(
        active_outcome(&report, 2),
        BindingOutcome::Unsupported(_)
    ));
    assert!(active_outcome(&report, 3).is_active());
    assert_eq!(rig.log.register_count(), 1);
}

#[test]
fn kb_03_02_native_ids_use_the_stache_signature_and_are_never_reused() {
    let rig = rig();
    let first = rig.apply(vec![q(1)]);
    let (_, first_id) = Rig::active_ids(&first)[0];
    // Remove it and register the very same chord again: a fresh ID.
    let _ = rig.apply(vec![]);
    let second = rig.apply(vec![q(1)]);
    let (_, second_id) = Rig::active_ids(&second)[0];
    assert!(second_id > first_id, "{second_id} must be newer than {first_id}");
    assert_ne!(
        SIGNATURE,
        u32::from_be_bytes(*b"stac"),
        "distinct from the legacy signature"
    );
}

#[test]
fn kb_03_02_a_press_dispatches_once_through_the_sink() {
    let rig = rig();
    let report = rig.apply(vec![q(7)]);
    let id = rig.id(KEY_Q, CARBON_CMD);
    assert_eq!(rig.log.press(id), Disposition::Handled);
    let pressed = rig.sink.pressed();
    assert_eq!(pressed.len(), 1);
    assert_eq!(pressed[0].key(), 7);
    assert_eq!(pressed[0].native_id(), id);
    assert_eq!(pressed[0].scope(), report.scope);
}

#[test]
fn kb_03_02_foreign_release_and_malformed_events_never_dispatch() {
    let rig = rig();
    let _ = rig.apply(vec![q(1)]);
    let id = rig.id(KEY_Q, CARBON_CMD);
    rig.log.clear_calls();

    let foreign = RawEvent {
        kind: RawEventKind::Pressed,
        hot_key: Ok((u32::from_be_bytes(*b"htrs"), id)),
    };
    assert_eq!(rig.log.deliver(foreign), Disposition::Forward);
    let foreign_release = RawEvent {
        kind: RawEventKind::Released,
        hot_key: Ok((u32::from_be_bytes(*b"htrs"), id)),
    };
    assert_eq!(rig.log.deliver(foreign_release), Disposition::Forward);
    let malformed = RawEvent {
        kind: RawEventKind::Pressed,
        hot_key: Err(-9870),
    };
    assert_eq!(rig.log.deliver(malformed), Disposition::Forward);
    let unknown = RawEvent {
        kind: RawEventKind::Pressed,
        hot_key: Ok((SIGNATURE, id + 10_000)),
    };
    assert_eq!(rig.log.deliver(unknown), Disposition::Forward);
    let other = RawEvent {
        kind: RawEventKind::Other,
        hot_key: Ok((SIGNATURE, id)),
    };
    assert_eq!(rig.log.deliver(other), Disposition::Forward);
    // A release of our own registration stops here and never dispatches.
    assert_eq!(rig.log.release(id), Disposition::Handled);

    assert!(rig.sink.pressed().is_empty());
    assert!(
        rig.log.calls().is_empty(),
        "the callback makes no native query, wait or spawn"
    );
}

#[test]
fn kb_03_02_the_callback_makes_no_native_call_even_when_it_dispatches() {
    let rig = rig();
    let _ = rig.apply(vec![q(1)]);
    let id = rig.id(KEY_Q, CARBON_CMD);
    rig.log.clear_calls();
    for _ in 0..5 {
        let _ = rig.log.press(id);
    }
    assert_eq!(rig.sink.pressed().len(), 5);
    assert!(rig.log.calls().is_empty());
}

#[test]
fn kb_03_02_a_panicking_sink_is_contained_and_still_owns_the_event() {
    struct Panics;
    impl HotkeySink for Panics {
        fn pressed(&self, _token: BindingToken) { panic!("sink bug") }
    }
    let log = FakeLog::new();
    let executor = Arc::new(ManualExecutor::new(true));
    let service = InputService::unavailable();
    let owner = service.reservations().new_owner();
    let control = HotkeyControl::new(
        executor as Arc<dyn MainThreadExecutor>,
        log.factory(),
        service,
        owner,
        Arc::new(Panics),
    );
    let cancel = CancellationToken::new();
    control.apply(&cancel, vec![q(1)]).expect("apply");
    let id = log.id_of(KEY_Q, CARBON_CMD).expect("registered");
    assert_eq!(log.press(id), Disposition::Handled);
}

// ---------------------------------------------------------------- KB-03-03

#[test]
fn kb_03_03_section_resolves_from_the_layout_and_merges_modifiers() {
    let rig = rig();
    rig.log.set_layout(FakeLayout::german());
    let report = rig.apply(vec![section(1, CMD)]);
    let BindingOutcome::Active { chord, .. } = active_outcome(&report, 1) else {
        panic!("active")
    };
    assert_eq!(chord.key_code, 0x14);
    assert_eq!(chord.modifiers, CMD | ModifierFlags::SHIFT);
    assert_eq!(
        chord.section,
        Some(SectionSource::Layout { required: ModifierFlags::SHIFT })
    );
    assert_eq!(rig.log.registered_chords(), vec![(
        0x14,
        CARBON_CMD | CARBON_SHIFT
    )]);
}

#[test]
fn kb_03_03_unreadable_layout_registers_the_iso_key_with_a_diagnostic() {
    let rig = rig();
    rig.log.set_layout_unreadable("no uchr");
    let report = rig.apply(vec![section(1, CMD)]);
    let BindingOutcome::Active { chord, .. } = active_outcome(&report, 1) else {
        panic!("active")
    };
    assert_eq!(chord.key_code, 0x0A);
    let Some(SectionSource::Fallback(FallbackReason::LayoutUnavailable(_))) = &chord.section else {
        panic!("fallback expected: {chord:?}")
    };
    assert!(
        chord
            .section
            .as_ref()
            .and_then(SectionSource::diagnostic)
            .expect("explained")
            .contains("does not guarantee")
    );
    assert_eq!(report.layout, None, "the active set is not tied to a layout");
}

#[test]
fn kb_03_03_ordinary_and_section_collisions_are_deterministic() {
    // On ANSI the section resolves to Option+6, exactly an ordinary chord.
    let ordinary = physical(5, Code::Digit6, ModifierFlags::OPTION);
    let expected_winner = 5;
    for requests in [vec![ordinary, section(2, ModifierFlags::NONE)], vec![
        section(2, ModifierFlags::NONE),
        ordinary,
    ]] {
        let rig = rig();
        let report = rig.apply(requests);
        assert!(
            active_outcome(&report, 5).is_active(),
            "the ordinary binding keeps the chord"
        );
        assert_eq!(active_outcome(&report, 2), &BindingOutcome::Collision {
            winner: expected_winner
        });
        assert_eq!(rig.log.register_count(), 1, "no silent double registration");
        let id = rig.id(0x16, CARBON_OPT);
        let _ = rig.log.press(id);
        assert_eq!(rig.sink.keys(), vec![5], "exactly one dispatch, never both");
    }
}

#[test]
fn kb_03_03_section_bindings_with_different_modifiers_coexist() {
    let rig = rig();
    let report = rig.apply(vec![section(1, ModifierFlags::NONE), section(2, CMD)]);
    assert!(report.all_active());
    assert_eq!(rig.log.registered_chords(), vec![
        (0x16, CARBON_OPT),
        (0x16, CARBON_OPT | CARBON_CMD)
    ]);
}

// ---------------------------------------------------------------- KB-03-04

#[test]
fn kb_03_04_unchanged_chords_are_reused_without_a_second_register() {
    let rig = rig();
    let first = rig.apply(vec![q(1), w(2)]);
    let before: Vec<_> = Rig::active_ids(&first);
    rig.log.clear_calls();

    let second = rig.apply(vec![q(1), w(2), e(3)]);
    let registers: Vec<_> = rig
        .log
        .calls()
        .into_iter()
        .filter(|c| matches!(c, Call::Register { .. }))
        .collect();
    assert_eq!(registers.len(), 1, "only the new chord registers: {registers:?}");
    assert!(matches!(active_outcome(&second, 1), BindingOutcome::Active {
        reused: true,
        ..
    }));
    assert!(matches!(active_outcome(&second, 2), BindingOutcome::Active {
        reused: true,
        ..
    }));
    assert!(matches!(active_outcome(&second, 3), BindingOutcome::Active {
        reused: false,
        ..
    }));
    let after = Rig::active_ids(&second);
    assert_eq!(before[..], after[..2], "the native IDs were transferred");
    assert!(second.scope.revision() > first.scope.revision());
    assert!(!rig.log.calls().iter().any(|c| matches!(c, Call::Unregister(_))));
}

#[test]
fn kb_03_04_removed_chords_are_unregistered_only_after_the_commit() {
    let rig = rig();
    let _ = rig.apply(vec![q(1), w(2)]);
    let w_id = rig.id(KEY_W, CARBON_CMD);
    rig.log.clear_calls();

    let report = rig.apply(vec![q(1)]);
    assert_eq!(report.removed, 1);
    assert_eq!(
        rig.log
            .calls()
            .iter()
            .filter(|c| matches!(c, Call::Unregister(_)))
            .collect::<Vec<_>>(),
        vec![&Call::Unregister(w_id)]
    );
    assert_eq!(rig.log.registered_chords(), vec![(KEY_Q, CARBON_CMD)]);
    assert_eq!(rig.service.reservations().len(), 1);
    // The retired ID is unknown now and is forwarded, never dispatched.
    assert_eq!(rig.log.press(w_id), Disposition::Forward);
    assert!(rig.sink.pressed().is_empty());
}

#[test]
fn kb_03_04_staging_is_inert_until_commit() {
    let rig = rig();
    let _ = rig.apply(vec![q(1)]);
    rig.log.clear_calls();
    let staged = rig.control.prepare(&rig.cancel, vec![q(1), w(2)]).expect("prepare");
    assert!(matches!(staged.bindings[0].1, StagedBinding::Reuse(_)));
    assert!(matches!(staged.bindings[1].1, StagedBinding::New(_)));
    assert_eq!(rig.log.register_count(), 0, "prepare registers nothing");
    assert_eq!(rig.service.reservations().len(), 1);

    let report = rig.control.commit(&rig.cancel, staged.id).expect("commit");
    assert!(report.all_active());
    assert_eq!(rig.log.register_count(), 1);
    // A staged id commits at most once.
    assert_eq!(
        rig.control.commit(&rig.cancel, staged.id).err(),
        Some(HotkeyError::Stale(StaleReason::Unknown))
    );
}

#[test]
fn kb_03_04_a_commit_cancelled_midway_rolls_back_and_keeps_the_last_good_set() {
    let rig = rig();
    let first = rig.apply(vec![q(1)]);
    let q_id = rig.id(KEY_Q, CARBON_CMD);
    let cancel = rig.cancel.clone();
    rig.log.set_hook(move |call| {
        if matches!(call, Call::Register { key_code, .. } if *key_code == KEY_W) {
            cancel.cancel();
        }
    });
    rig.log.clear_calls();
    let error = rig.control.apply(&rig.cancel, vec![q(1), w(2), e(3)]).err();
    assert_eq!(error, Some(HotkeyError::Cancelled));
    rig.log.clear_hook();

    let calls = rig.log.calls();
    let w_id = calls.iter().find_map(|c| match c {
        Call::Register { key_code, id, .. } if *key_code == KEY_W => Some(*id),
        _ => None,
    });
    assert_eq!(
        calls.iter().filter(|c| matches!(c, Call::Register { .. })).count(),
        1,
        "stops at the cancellation"
    );
    assert!(calls.contains(&Call::Unregister(w_id.expect("w was registered"))));
    assert_eq!(
        rig.log.registered_chords(),
        vec![(KEY_Q, CARBON_CMD)],
        "the last-good chord stays"
    );
    let fresh = CancellationToken::new();
    let status = rig.control.status(&fresh).expect("status");
    assert_eq!(
        status.scope.revision(),
        first.scope.revision(),
        "nothing was accepted"
    );
    assert_eq!(status.active.len(), 1);
    assert_eq!(rig.service.reservations().len(), 1);
    assert_eq!(rig.log.press(q_id), Disposition::Handled);
    assert_eq!(rig.sink.keys(), vec![1]);
}

#[test]
fn kb_03_04_rollback_that_cannot_unregister_reports_the_owed_cleanup() {
    let rig = rig();
    let _ = rig.apply(vec![q(1)]);
    let cancel = rig.cancel.clone();
    rig.log.set_hook(move |call| {
        if matches!(call, Call::Register { key_code, .. } if *key_code == KEY_W) {
            cancel.cancel();
        }
    });
    rig.log.fail_unregisters(1);
    let error = rig.control.apply(&rig.cancel, vec![q(1), w(2)]).err();
    assert!(
        matches!(error, Some(HotkeyError::CleanupPending { pending: 1, ref cause }) if **cause == HotkeyError::Cancelled),
        "{error:?}"
    );
    rig.log.clear_hook();
    let status = rig.control.status(&CancellationToken::new()).expect("status");
    assert_eq!(status.pending_cleanup.len(), 1);
    assert_eq!(status.active.len(), 1, "the last-good set is untouched");
}

#[test]
fn kb_03_04_a_layout_switch_between_prepare_and_commit_is_rejected_and_refreshed() {
    let rig = rig();
    let staged = rig
        .control
        .prepare(&rig.cancel, vec![section(1, ModifierFlags::NONE)])
        .expect("prepare");
    assert_eq!(staged.layout, Some(LayoutId(1)));
    rig.log.set_layout(FakeLayout::german());
    rig.log.clear_calls();

    let error = rig.control.commit(&rig.cancel, staged.id).err();
    assert_eq!(error, Some(HotkeyError::Stale(StaleReason::Layout)));
    assert_eq!(rig.log.register_count(), 0, "no native mutation on stale work");
    assert!(rig.service.reservations().is_empty());
    // The mismatch queued a refresh. Nothing is desired yet, so it is a no-op.
    assert!(rig.status().active.is_empty());
}

#[test]
fn kb_03_04_a_layout_switch_during_the_register_loop_rolls_back_before_publication() {
    let rig = rig();
    let log = Arc::clone(&rig.log);
    rig.log.set_hook(move |call| {
        if matches!(call, Call::Register { key_code, .. } if *key_code == KEY_Q) {
            log.set_layout(FakeLayout::german());
        }
    });
    let error = rig
        .control
        .apply(&rig.cancel, vec![q(1), section(2, ModifierFlags::NONE)])
        .err();
    assert_eq!(error, Some(HotkeyError::Stale(StaleReason::Layout)));
    rig.log.clear_hook();
    assert!(
        rig.log.registered_chords().is_empty(),
        "the inert registrations were rolled back"
    );
    assert!(rig.service.reservations().is_empty(), "nothing was published");
    assert!(rig.status().active.is_empty());
}

#[test]
fn kb_03_04_refresh_replaces_obsolete_section_routes_and_preserves_the_rest() {
    let rig = rig();
    let first = rig.apply(vec![q(1), section(2, ModifierFlags::NONE)]);
    let q_id = rig.id(KEY_Q, CARBON_CMD);
    let old_section = rig.id(0x16, CARBON_OPT);
    assert!(
        rig.status().observing,
        "the observer starts with the first active set"
    );
    rig.log.clear_calls();

    rig.log.set_layout(FakeLayout::german());
    assert!(rig.log.fire_hint());

    let status = rig.status();
    assert_eq!(status.refreshes, 1);
    assert_eq!(status.layout, Some(LayoutId(3)));
    assert_eq!(rig.log.registered_chords(), vec![
        (KEY_Q, CARBON_CMD),
        (0x14, CARBON_SHIFT)
    ]);
    // The unaffected chord kept its registration.
    assert_eq!(rig.id(KEY_Q, CARBON_CMD), q_id);
    assert_eq!(rig.log.register_count(), 1);
    assert!(rig.status().last_refresh.as_ref().expect("report").all_active());
    // The obsolete route is invalidated: its queued event forwards, never dispatches.
    assert_eq!(rig.log.press(old_section), Disposition::Forward);
    assert!(rig.sink.pressed().is_empty());
    let new_section = rig.id(0x14, CARBON_SHIFT);
    assert_ne!(new_section, old_section);
    assert_eq!(rig.log.press(new_section), Disposition::Handled);
    assert_eq!(rig.sink.keys(), vec![2]);
    assert!(status.scope.revision() > first.scope.revision());
}

#[test]
fn kb_03_04_a_queued_old_event_never_reaches_the_successor() {
    let rig = rig();
    let _ = rig.apply(vec![section(1, ModifierFlags::NONE)]);
    let old = rig.id(0x16, CARBON_OPT);
    rig.log.set_layout(FakeLayout::iso());
    assert!(rig.log.fire_hint());
    let new = rig.id(0x0A, 0);
    assert!(new > old, "IDs only grow");
    // The event for the old ID arrives after the replacement.
    assert_eq!(rig.log.press(old), Disposition::Forward);
    assert!(
        rig.sink.pressed().is_empty(),
        "the successor binding must not fire for a stale ID"
    );
    assert_eq!(rig.log.press(new), Disposition::Handled);
    assert_eq!(rig.sink.keys(), vec![1]);
}

#[test]
fn kb_03_04_hints_are_coalesced_into_one_refresh() {
    let rig = rig();
    let _ = rig.apply(vec![section(1, ModifierFlags::NONE)]);
    rig.log.clear_calls();

    rig.executor.set_on_main(false);
    rig.log.set_layout(FakeLayout::german());
    for _ in 0..5 {
        assert!(rig.log.fire_hint());
        rig.control.request_layout_refresh();
    }
    assert_eq!(rig.executor.queued(), 1, "five hints, one queued refresh");
    assert!(
        rig.log.calls().is_empty(),
        "a hint never queries, registers or waits"
    );

    assert_eq!(rig.executor.drain(), 1);
    rig.executor.set_on_main(true);
    assert_eq!(rig.status().refreshes, 1);
    assert_eq!(rig.log.register_count(), 1);

    // A hint for the layout that is already applied is a no-op.
    rig.log.clear_calls();
    rig.executor.set_on_main(false);
    assert!(rig.log.fire_hint());
    rig.executor.drain();
    rig.executor.set_on_main(true);
    assert_eq!(rig.log.register_count(), 0);
    assert!(!rig.log.calls().iter().any(|c| matches!(c, Call::Unregister(_))));
}

#[test]
fn kb_03_04_a_refresh_superseded_by_stop_does_nothing_native() {
    let rig = rig();
    let _ = rig.apply(vec![section(1, ModifierFlags::NONE)]);
    rig.executor.set_on_main(false);
    rig.log.set_layout(FakeLayout::german());
    assert!(rig.log.fire_hint());
    assert_eq!(rig.executor.queued(), 1);
    rig.executor.set_on_main(true);

    // Stop wins the race: the layout switch arrives during stop.
    let report = rig.control.stop(&rig.cancel).expect("stop");
    assert!(report.is_clean(), "{report:?}");
    rig.log.clear_calls();
    assert_eq!(rig.executor.drain(), 1);

    assert!(
        rig.log.calls().is_empty(),
        "the stale refresh recheck found a newer generation"
    );
    assert!(rig.log.registered_chords().is_empty());
    assert!(rig.service.reservations().is_empty());
}

#[test]
fn kb_03_04_a_layout_switch_during_stop_cannot_resurrect_registrations() {
    let rig = rig();
    let _ = rig.apply(vec![section(1, ModifierFlags::NONE), q(2)]);
    let log = Arc::clone(&rig.log);
    let hint_fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fired = Arc::clone(&hint_fired);
    rig.log.set_hook(move |call| {
        if matches!(call, Call::Unregister(_)) && !fired.swap(true, Ordering::SeqCst) {
            log.set_layout(FakeLayout::german());
            log.fire_hint();
        }
    });
    let report = rig.control.stop(&rig.cancel).expect("stop");
    rig.log.clear_hook();
    assert!(report.is_clean(), "{report:?}");
    assert!(rig.log.registered_chords().is_empty());
    assert!(rig.service.reservations().is_empty());
    assert!(!rig.log.observing());
    assert_eq!(rig.status().refreshes, 0);
}

#[test]
fn kb_03_04_restart_after_stop_adopts_the_new_generation() {
    let rig = rig();
    let first = rig.apply(vec![q(1)]);
    let _ = rig.control.stop(&rig.cancel).expect("stop");
    let second = rig.apply(vec![q(1)]);
    assert!(second.all_active());
    assert_ne!(first.scope.generation(), second.scope.generation());
    let id = rig.id(KEY_Q, CARBON_CMD);
    assert_eq!(rig.log.press(id), Disposition::Handled);
    assert_eq!(rig.sink.pressed().len(), 1);
}

#[test]
fn kb_03_04_a_late_stop_never_disturbs_a_newer_commit() {
    let slow = Timing {
        budget: Duration::from_millis(30),
        quantum: Duration::from_millis(5),
    };
    let rig = rig_with(true, slow);
    let _ = rig.apply(vec![q(1), w(2)]);

    // The logical stop happens off-main; its cleanup job times out and never runs.
    rig.executor.set_on_main(false);
    assert_eq!(rig.control.stop(&rig.cancel).err(), Some(HotkeyError::TimedOut));
    rig.executor.set_on_main(true);
    assert_eq!(
        rig.log.registered_chords().len(),
        2,
        "nothing native happened yet"
    );
    for id in rig.log.registered_ids() {
        assert_eq!(rig.log.press(id), Disposition::Handled, "owned but inert");
    }
    assert!(
        rig.sink.pressed().is_empty(),
        "logical removal inhibited dispatch before any hop"
    );

    // The next commit (new generation) voids the old entries and registers afresh.
    let report = rig.apply(vec![e(3)]);
    assert!(report.all_active());
    assert_eq!(report.pending_cleanup, 0);
    assert_eq!(rig.log.registered_chords(), vec![(KEY_E, CARBON_CMD)]);

    // The abandoned stop finally runs: it must not tear down the new set.
    assert_eq!(rig.executor.drain(), 1);
    assert_eq!(rig.log.registered_chords(), vec![(KEY_E, CARBON_CMD)]);
    let id = rig.id(KEY_E, CARBON_CMD);
    assert_eq!(rig.log.press(id), Disposition::Handled);
    assert_eq!(rig.sink.keys(), vec![3]);
}

#[test]
fn kb_03_04_reservations_match_the_actual_routes() {
    let rig = rig();
    let report = rig.apply(vec![q(1), section(2, CMD)]);
    let status = rig.status();
    assert_eq!(rig.service.reservations().len(), 2);
    assert_eq!(status.active.len(), 2);
    // Each active binding has a live Carbon reservation with the supplied priority.
    for binding in &status.active {
        assert!(rig.service.reservations().dispatchable(binding.reservation));
    }
    assert!(report.all_active());
    // A HID claim at the same chord is a recorded conflict with the Carbon one.
    let hid = rig.hid_claim(KEY_Q, CMD, 10);
    let conflicts = rig.service.reservations().conflicts();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].hid, hid);
    assert!(status.active.iter().any(|b| b.reservation == conflicts[0].carbon && b.key == 1));
}

#[test]
fn kb_03_04_a_central_conflict_with_a_hid_filter_is_recorded_not_dispatch_changing() {
    let rig = rig();
    let _ = rig.apply(vec![q(1)]);
    let hid = rig.service.reservations().new_owner();
    let filter = InputFilter::new(KindSet::of(InputEventKind::KeyDown)).with_keys([KeyCode(KEY_Q)]);
    // Equal priority: only a conflict, never a yield.
    let claim = rig
        .service
        .reservations()
        .register(hid, ReservationBackend::Hid, Claim::Filter(filter), prio(10))
        .expect("filter");
    let conflicts = rig.service.reservations().conflicts();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].hid, claim);
    let id = rig.id(KEY_Q, CARBON_CMD);
    assert_eq!(rig.log.press(id), Disposition::Handled);
    assert_eq!(
        rig.sink.keys(),
        vec![1],
        "an equal-priority overlap does not yield"
    );
}

#[test]
fn kb_03_04_a_failed_registration_has_no_reservation_and_removal_is_exact() {
    let rig = rig();
    rig.log.occupy(KEY_W, CARBON_CMD);
    let report = rig.apply(vec![q(1), w(2)]);
    assert!(!report.all_active());
    assert_eq!(rig.service.reservations().len(), 1);
    let status = rig.status();
    let q_reservation = status.active[0].reservation;

    // An unrelated owner's reservation is never touched by this owner's removal.
    let other = rig.service.reservations().new_owner();
    let other_id = rig
        .service
        .reservations()
        .register(
            other,
            ReservationBackend::Carbon,
            Claim::Chord(Chord::new(KeyCode(KEY_E), CMD)),
            prio(10),
        )
        .expect("other");

    let _ = rig.apply(vec![]);
    assert_eq!(
        rig.service.reservations().len(),
        1,
        "only the matching owned reservation went"
    );
    assert!(rig.service.reservations().dispatchable(other_id));
    // The removed reservation is stale: removing it again is refused.
    assert_eq!(
        rig.service.reservations().remove(q_reservation),
        Err(ReservationError::Stale)
    );
    let _ = rig.owner;
}

#[test]
fn kb_03_04_refresh_moves_the_reservation_with_the_route() {
    let rig = rig();
    let _ = rig.apply(vec![section(1, ModifierFlags::NONE)]);
    let before = rig.status().active[0].reservation;
    rig.log.set_layout(FakeLayout::german());
    assert!(rig.log.fire_hint());

    assert_eq!(rig.service.reservations().len(), 1);
    assert_eq!(
        rig.service.reservations().remove(before),
        Err(ReservationError::Stale),
        "the old reservation is gone"
    );
    // A HID claim on the old chord no longer conflicts; on the new chord it does.
    let _ = rig.hid_claim(0x16, ModifierFlags::OPTION, 10);
    assert!(rig.service.reservations().conflicts().is_empty());
    let _ = rig.hid_claim(0x14, ModifierFlags::SHIFT, 10);
    assert_eq!(rig.service.reservations().conflicts().len(), 1);
}

#[test]
fn kb_03_04_a_priority_change_updates_the_reservation_in_place() {
    let rig = rig();
    let _ = rig.apply(vec![q(1)]);
    let _ = rig.hid_claim(KEY_Q, CMD, 20);
    let reserved = rig.status().active[0].reservation;
    assert!(
        !rig.service.reservations().dispatchable(reserved),
        "the weaker reservation yields"
    );
    rig.log.clear_calls();
    let mut stronger = q(1);
    stronger.priority = prio(30);
    let report = rig.apply(vec![stronger]);
    assert!(matches!(active_outcome(&report, 1), BindingOutcome::Active {
        reused: true,
        ..
    }));
    assert_eq!(rig.log.register_count(), 0);
    let now = rig.status().active[0].reservation;
    assert!(
        rig.service.reservations().dispatchable(now),
        "now it outranks the claimant"
    );
}

// ---------------------------------------------------------------- KB-03-05

#[test]
fn kb_03_05_cleanup_failure_retains_an_inert_guard_and_reports_it() {
    let rig = rig();
    let _ = rig.apply(vec![q(1), w(2)]);
    let w_id = rig.id(KEY_W, CARBON_CMD);
    rig.log.stick(w_id);

    let report = rig.apply(vec![q(1)]);
    assert_eq!(report.pending_cleanup, 1);
    assert!(
        !report.all_active(),
        "owed cleanup is never reported as all active"
    );
    let status = rig.status();
    assert_eq!(status.pending_cleanup, vec![w_id]);
    // The guard is registered natively but inert: it swallows and never dispatches.
    assert_eq!(rig.log.press(w_id), Disposition::Handled);
    assert!(rig.sink.pressed().is_empty());
    // Its reservation is retained because the chord is still held natively.
    assert_eq!(rig.service.reservations().len(), 2);

    rig.log.unstick(w_id);
    assert_eq!(rig.control.retry_cleanup(&rig.cancel).expect("retry"), 0);
    assert_eq!(rig.service.reservations().len(), 1);
    assert_eq!(rig.log.press(w_id), Disposition::Forward);
}

#[test]
fn kb_03_05_stop_keeps_the_handler_while_a_guard_remains() {
    let rig = rig();
    let _ = rig.apply(vec![q(1), w(2)]);
    let w_id = rig.id(KEY_W, CARBON_CMD);
    rig.log.stick(w_id);

    let report = rig.control.stop(&rig.cancel).expect("stop");
    assert_eq!(report.unregistered, 1);
    assert_eq!(report.pending_cleanup, 1);
    assert_eq!(
        report.handler,
        HandlerStatus::Installed,
        "the handler must outlive a live registration"
    );
    assert!(!report.is_clean());
    assert!(rig.log.handler_installed());
    assert!(!rig.log.observing(), "the observer is removed independently");

    rig.log.unstick(w_id);
    let report = rig.control.stop(&rig.cancel).expect("second stop");
    assert!(report.is_clean(), "{report:?}");
    assert!(!rig.log.handler_installed());
    assert!(rig.service.reservations().is_empty());
}

#[test]
fn kb_03_05_handler_and_observer_removal_failures_are_reported() {
    let rig = rig();
    let _ = rig.apply(vec![q(1)]);
    rig.log.fail_remove_handler(Some(-7));
    rig.log.fail_unobserve(true);
    let report = rig.control.stop(&rig.cancel).expect("stop");
    assert_eq!(report.handler, HandlerStatus::RemovalPending(-7));
    assert!(report.observer_pending);
    assert!(!report.is_clean());
    assert_eq!(report.pending_cleanup, 0);

    rig.log.fail_remove_handler(None);
    rig.log.fail_unobserve(false);
    let report = rig.control.stop(&rig.cancel).expect("retry stop");
    assert!(report.is_clean(), "{report:?}");
}

#[test]
fn kb_03_05_observer_failure_degrades_refresh_but_not_registration() {
    let rig = rig();
    rig.log.fail_observe(true);
    let report = rig.apply(vec![q(1)]);
    assert!(report.all_active());
    let status = rig.status();
    assert!(!status.observing);
    assert!(
        status.diagnostics.iter().any(|d| d.contains("will not be observed")),
        "{:?}",
        status.diagnostics
    );
}

#[test]
fn kb_03_05_stop_inhibits_dispatch_before_any_main_thread_hop() {
    let slow = Timing {
        budget: Duration::from_millis(30),
        quantum: Duration::from_millis(5),
    };
    let rig = rig_with(true, slow);
    let _ = rig.apply(vec![q(1)]);
    let id = rig.id(KEY_Q, CARBON_CMD);
    rig.executor.set_on_main(false);
    let _ = rig.control.stop(&rig.cancel);
    assert_eq!(rig.executor.queued(), 1, "the cleanup is still queued");
    assert_eq!(rig.log.press(id), Disposition::Handled);
    assert!(
        rig.sink.pressed().is_empty(),
        "the press was swallowed before the cleanup ran"
    );
}

#[test]
fn kb_03_05_a_cancelled_cleanup_skips_native_mutation_and_keeps_ownership() {
    let rig = rig();
    let _ = rig.apply(vec![q(1)]);
    let id = rig.id(KEY_Q, CARBON_CMD);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    rig.log.clear_calls();

    let report = rig.control.stop(&cancelled).expect("the finished job's report wins");
    assert_eq!((report.unregistered, report.pending_cleanup), (0, 1));
    assert!(!report.is_clean());
    assert!(
        !rig.log
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Unregister(_) | Call::RemoveHandler))
    );
    let status = rig.status();
    assert_eq!(status.pending_cleanup, vec![id]);
}

#[test]
fn kb_03_05_dropping_the_control_inhibits_and_queues_teardown_without_waiting() {
    let rig = rig();
    let _ = rig.apply(vec![q(1)]);
    let Rig {
        control, log, executor, sink, ..
    } = rig;
    let id = log.id_of(KEY_Q, CARBON_CMD).expect("registered");
    executor.set_on_main(false);
    drop(control);
    assert_eq!(executor.queued(), 1);
    assert_eq!(log.press(id), Disposition::Handled);
    assert!(sink.pressed().is_empty());
    assert_eq!(executor.drain(), 1);
    assert!(
        log.registered_chords().is_empty(),
        "the store's drop made a best-effort teardown"
    );
}

// ---------------------------------------------------------------- KB-03-06

#[test]
fn kb_03_06_a_yielded_reservation_never_reaches_the_sink() {
    let rig = rig();
    let _ = rig.apply(vec![q(1), w(2)]);
    let q_id = rig.id(KEY_Q, CARBON_CMD);
    let w_id = rig.id(KEY_W, CARBON_CMD);

    // Both chords fire on "the same press"; only the unrelated one dispatches.
    let claimant = rig.hid_claim(KEY_Q, CMD, 20);
    assert_eq!(rig.log.press(q_id), Disposition::Handled);
    assert_eq!(rig.log.press(w_id), Disposition::Handled);
    assert_eq!(
        rig.sink.keys(),
        vec![2],
        "the yielded chord is suppressed, the other runs exactly once"
    );

    // Restored on claimant removal.
    rig.service.reservations().remove(claimant).expect("remove claimant");
    assert_eq!(rig.log.press(q_id), Disposition::Handled);
    assert_eq!(rig.sink.keys(), vec![2, 1]);
}

#[test]
fn kb_03_06_a_claimant_present_before_registration_yields_from_the_start() {
    let rig = rig();
    let _ = rig.hid_claim(KEY_Q, CMD, 20);
    let _ = rig.apply(vec![q(1)]);
    let id = rig.id(KEY_Q, CARBON_CMD);
    let _ = rig.log.press(id);
    assert!(rig.sink.pressed().is_empty());
}

#[test]
fn kb_03_06_yield_and_restore_across_pause_reload_and_stop() {
    let rig = rig();
    let _ = rig.apply(vec![q(1), w(2)]);
    let q_id = rig.id(KEY_Q, CARBON_CMD);
    let claimant = rig.hid_claim(KEY_Q, CMD, 20);
    let registry = rig.service.reservations();

    // Pause restores, resume yields again.
    registry.set_active(claimant, false).expect("pause");
    let _ = rig.log.press(q_id);
    assert_eq!(rig.sink.keys(), vec![1]);
    registry.set_active(claimant, true).expect("resume");
    let _ = rig.log.press(q_id);
    assert_eq!(rig.sink.keys(), vec![1], "yielded again");

    // The yield persists across a reload that keeps the chord (reclaim attempt).
    let report = rig.apply(vec![q(1), w(2), e(3)]);
    assert!(matches!(active_outcome(&report, 1), BindingOutcome::Active {
        reused: true,
        ..
    }));
    assert_eq!(
        rig.id(KEY_Q, CARBON_CMD),
        q_id,
        "the unchanged registration was transferred"
    );
    let _ = rig.log.press(q_id);
    assert_eq!(rig.sink.keys(), vec![1], "still yielded after reload");
    // An unrelated chord keeps dispatching meanwhile.
    let w_id = rig.id(KEY_W, CARBON_CMD);
    let _ = rig.log.press(w_id);
    assert_eq!(rig.sink.keys(), vec![1, 2]);

    // The claimant stops (removal): restored.
    registry.remove(claimant).expect("remove");
    let _ = rig.log.press(q_id);
    assert_eq!(rig.sink.keys(), vec![1, 2, 1]);

    // A claimant that outlives a stop cannot make a stopped route dispatch.
    let _ = rig.hid_claim(KEY_W, CMD, 20);
    let _ = rig.control.stop(&rig.cancel).expect("stop");
    let before = rig.sink.pressed().len();
    let _ = rig.log.press(q_id);
    let _ = rig.log.press(w_id);
    assert_eq!(rig.sink.pressed().len(), before);
}

#[test]
fn kb_03_06_double_yield_and_stale_yield_are_safe() {
    let rig = rig();
    let _ = rig.apply(vec![q(1)]);
    let reserved = rig.status().active[0].reservation;
    let claimant = rig.hid_claim(KEY_Q, CMD, 20);
    let registry = rig.service.reservations();
    registry.yield_to(reserved, claimant).expect("explicit yield");
    registry.yield_to(reserved, claimant).expect("idempotent");
    let id = rig.id(KEY_Q, CARBON_CMD);
    let _ = rig.log.press(id);
    assert!(rig.sink.pressed().is_empty());

    registry.remove(claimant).expect("remove");
    let _ = rig.log.press(id);
    assert_eq!(rig.sink.pressed().len(), 1, "one restore, one dispatch");

    // A reservation that no longer exists cannot be yielded.
    let _ = rig.apply(vec![]);
    let again = rig.hid_claim(KEY_Q, CMD, 20);
    assert_eq!(registry.yield_to(reserved, again), Err(ReservationError::Stale));
}

#[test]
fn kb_03_06_a_removed_binding_stays_inert_even_if_a_claimant_is_removed() {
    let rig = rig();
    let _ = rig.apply(vec![q(1)]);
    let id = rig.id(KEY_Q, CARBON_CMD);
    let claimant = rig.hid_claim(KEY_Q, CMD, 20);
    rig.log.stick(id);
    let _ = rig.apply(vec![]);
    rig.service.reservations().remove(claimant).expect("remove");
    // The registration is stuck natively, yet it never dispatches again.
    assert_eq!(rig.log.press(id), Disposition::Handled);
    assert!(rig.sink.pressed().is_empty());
}

// ------------------------------------------------------------------ bounds

#[test]
fn bounds_default_to_the_shared_two_second_ten_millisecond_budget() {
    let timing = Timing::default();
    assert_eq!(timing.budget, Duration::from_secs(2));
    assert_eq!(timing.quantum, Duration::from_millis(10));
    assert_eq!(timing.budget, ACKNOWLEDGEMENT_BUDGET);
    assert_eq!(timing.quantum, CANCELLATION_QUANTUM);
}

#[test]
fn bounds_an_unacknowledged_hop_times_out_and_late_work_does_not_mutate() {
    let timing = Timing {
        budget: Duration::from_millis(60),
        quantum: Duration::from_millis(5),
    };
    let rig = rig_with(false, timing);
    let started = Instant::now();
    let result = rig.control.apply(&rig.cancel, vec![q(1)]);
    let elapsed = started.elapsed();
    assert_eq!(result.err(), Some(HotkeyError::TimedOut));
    assert!(elapsed >= Duration::from_millis(60), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(1500), "{elapsed:?}");

    // The main queue finally drains: the abandoned job rechecks and does nothing.
    assert_eq!(rig.executor.drain(), 1);
    assert!(
        rig.log
            .calls()
            .iter()
            .all(|c| !matches!(c, Call::Register { .. } | Call::InstallHandler))
    );
    assert!(rig.service.reservations().is_empty());
}

#[test]
fn bounds_cancellation_wakes_a_waiting_hop_within_a_quantum() {
    let timing = Timing {
        budget: Duration::from_secs(5),
        quantum: Duration::from_millis(5),
    };
    let rig = rig_with(false, timing);
    let token = rig.cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        token.cancel();
    });
    let started = Instant::now();
    let result = rig.control.apply(&rig.cancel, vec![q(1)]);
    let elapsed = started.elapsed();
    canceller.join().expect("canceller");
    assert_eq!(result.err(), Some(HotkeyError::Cancelled));
    assert!(
        elapsed < Duration::from_secs(2),
        "returned long before the budget: {elapsed:?}"
    );
    assert_eq!(rig.executor.drain(), 1);
    assert_eq!(rig.log.register_count(), 0, "late cancelled work did not mutate");
}

#[test]
fn bounds_a_rejected_dispatch_is_reported() {
    let rig = rig_with(false, Timing::default());
    rig.executor.reject(true);
    assert!(matches!(
        rig.control.apply(&rig.cancel, vec![q(1)]),
        Err(HotkeyError::Dispatch(_))
    ));
}

#[test]
fn bounds_a_cancelled_token_is_refused_before_dispatch() {
    let rig = rig();
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        rig.control.apply(&token, vec![q(1)]).err(),
        Some(HotkeyError::Cancelled)
    );
    assert!(rig.log.calls().is_empty());
}

// ------------------------------------------------------------------ keymap

#[test]
fn kb_03_02_the_key_table_matches_the_archived_backend() {
    use super::keymap::virtual_key;
    for (code, expected) in [
        (Code::KeyA, 0x00),
        (Code::KeyQ, 0x0c),
        (Code::Digit1, 0x12),
        (Code::Digit6, 0x16),
        (Code::Backquote, 0x32),
        (Code::Space, 0x31),
        (Code::Enter, 0x24),
        (Code::Escape, 0x35),
        (Code::F1, 0x7a),
        (Code::F12, 0x6f),
        (Code::F20, 0x5a),
        (Code::ArrowLeft, 0x7b),
        (Code::ArrowUp, 0x7e),
        (Code::Numpad0, 0x52),
        (Code::NumpadEnter, 0x4c),
        (Code::Delete, 0x75),
    ] {
        assert_eq!(virtual_key(code), Some(expected), "{code:?}");
    }
    // The archive could not register these either: they fail per binding.
    for code in [
        Code::IntlBackslash,
        Code::MediaPlayPause,
        Code::ShiftLeft,
        Code::ContextMenu,
    ] {
        assert_eq!(virtual_key(code), None, "{code:?}");
    }
}

#[test]
fn kb_03_02_the_input_source_notification_name_is_the_tis_constant() {
    assert_eq!(
        crate::keyboard_layout::observer::INPUT_SOURCE_CHANGED,
        "com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged"
    );
}
