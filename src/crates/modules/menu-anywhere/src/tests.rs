//! `MenuAnywhere` runtime tests over the fakes.
//!
//! Test names carry the acceptance criterion they prove (`ma_03_0N_...`). Every
//! blocking wait has a timeout, and no test creates a real tap, AX element,
//! menu, activation or prompt.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use MenuAnywhereModifier::{Command, Control, Option as Alt, Shift};
use MenuAnywhereMouseButton::{MiddleClick, RightClick};
use stache_macos::menu::testing::Op;
use stache_sdk::input::testing::FakeResponse;
use stache_sdk::input::{
    CallbackScope, ControlAction, Disposition, HandlerSpec, InputEvent, InputEventKind,
    InputHandler, InputPoint, InputPriority, KindSet, ModifierFlags, ModifierMatch, MouseButton,
    PressStream, TapKind, TapStatus, WorkRequirement,
};
use stache_sdk::{
    CancellationToken, ErrorCategory, ModuleStatus, Operation, Permission, PermissionStatus,
};

use crate::config::{MenuAnywhereConfig, MenuAnywhereModifier, MenuAnywhereMouseButton};
use crate::runtime::{CAPTURE_WINDOW, Stage};
use crate::test_support::{CHORD, Rig, config, default_config, hold_ax, wait_until};
use crate::trigger::{
    Admission, Counters, Matcher, Message, Outcome, Published, held_modifiers, required_flags,
};

const NONE: ModifierFlags = ModifierFlags::NONE;

fn matcher(button: MenuAnywhereMouseButton, modifiers: &[MenuAnywhereModifier]) -> Matcher {
    Matcher::from_config(&config(button, modifiers))
}

fn down(matcher: Matcher, button: u32, flags: ModifierFlags) -> bool {
    matcher.matches(
        InputEventKind::MouseDown,
        PressStream::Button(MouseButton(button)),
        flags,
        false,
    )
}

fn revision_of(rig: &Rig) -> u64 { rig.registry.snapshot(rig.handle).expect("snapshot").revision }

/// Waits until the config worker published the registry's accepted revision.
fn wait_adopted(rig: &Rig) {
    let accepted = revision_of(rig);
    wait_until("the accepted revision to be published", || {
        rig.runtime.published().is_some_and(|published| published.revision == accepted)
    });
}

/// Finishes the open menu by dismissing it and waits for the reservation.
fn dismiss(rig: &Rig) {
    let id = rig.wait_tracking();
    rig.menu.presenter.dismiss(id);
    rig.wait_idle();
}

// ---------------------------------------------------------------------------
// MA-03-01: consumption of the host services, inert construction, one tap
// ---------------------------------------------------------------------------

#[test]
fn ma_03_01_building_the_runtime_and_registering_are_inert() {
    let rig = Rig::new(default_config());
    assert_eq!(rig.menu.opens(), 0, "no menu stack");
    assert_eq!(rig.apps.observations(), 0, "no application observer");
    assert_eq!(rig.apps.activation_observers(), 0);
    assert!(rig.tap.submitted().is_empty(), "no tap request");
    assert_eq!(rig.menu.ax.call_count(), 0, "no AX call");
    assert_eq!(rig.trust.requests(), 0);
    assert_ne!(rig.status(), ModuleStatus::Running, "registered, not started");
}

#[test]
fn ma_03_01_start_acquires_one_subscription_on_one_suppressing_tap_and_reads_nothing() {
    let (rig, _app) = Rig::started(default_config());
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(rig.tap.count(TapKind::Suppressing, ControlAction::Install), 1);
    assert!(rig.tap.is_live(TapKind::Suppressing));
    assert!(
        !rig.tap.is_live(TapKind::ListenOnly),
        "MenuAnywhere never uses the passive tap"
    );
    assert_eq!(rig.menu.opens(), 1);
    assert_eq!(rig.apps.activation_observers(), 1);
    assert_eq!(
        rig.menu.ax.call_count(),
        0,
        "native reads start only on a trigger"
    );
    assert!(rig.menu.presenter.prepared().is_empty());
    let published = rig.runtime.published().expect("published");
    assert!(published.eligible());
    assert_eq!(published.revision, revision_of(&rig));
}

#[test]
fn ma_03_01_the_module_requires_only_the_shared_input_accessibility_requirement() {
    use stache_sdk::{Module, PermissionRequirement, PromptPolicy};
    let rig = Rig::new(default_config());
    let module = rig.runtime.module();
    let enabled = module.required_permissions(&default_config());
    assert_eq!(enabled, vec![PermissionRequirement::shared_input(
        Permission::Accessibility
    )]);
    assert!(module.required_permissions(&MenuAnywhereConfig::default()).is_empty());
    assert_eq!(enabled[0].prompt(), PromptPolicy::UserOnly);
    assert!(!enabled[0].may_auto_prompt(), "user-prompt-only metadata");
}

// ---------------------------------------------------------------------------
// MA-03-02: filter tables, priority, overlap, re-enable
// ---------------------------------------------------------------------------

#[test]
fn ma_03_02_right_click_matches_button_one_and_exact_modifiers_only() {
    let m = matcher(RightClick, &[Control, Command]);
    assert!(down(m, 1, CHORD));
    assert!(!down(m, 0, CHORD), "left");
    assert!(!down(m, 2, CHORD), "middle");
    assert!(!down(m, 3, CHORD), "button 3");
    assert!(!down(m, 4, CHORD), "button 4");
    assert!(!down(m, 1, ModifierFlags::CONTROL), "subset");
    assert!(
        !down(m, 1, CHORD | ModifierFlags::SHIFT),
        "extra supported modifier"
    );
    assert!(
        !down(m, 1, CHORD | ModifierFlags::OPTION),
        "extra supported modifier"
    );
    assert!(!down(m, 1, NONE), "bare click");
    assert!(
        down(m, 1, CHORD | ModifierFlags::CAPS_LOCK),
        "caps lock is not a held modifier"
    );
    assert!(down(m, 1, CHORD | ModifierFlags::FUNCTION), "fn is unrelated");
}

#[test]
fn ma_03_02_middle_click_matches_button_two_only() {
    let m = matcher(MiddleClick, &[Shift]);
    assert!(down(m, 2, ModifierFlags::SHIFT));
    assert!(!down(m, 1, ModifierFlags::SHIFT));
    assert!(!down(m, 0, ModifierFlags::SHIFT));
    for other in [3, 4, 5, 31] {
        assert!(!down(m, other, ModifierFlags::SHIFT), "button {other}");
    }
    assert!(!down(m, 2, NONE));
}

#[test]
fn ma_03_02_an_empty_modifier_list_captures_bare_clicks_and_nothing_else() {
    let m = matcher(RightClick, &[]);
    assert!(down(m, 1, NONE));
    assert!(down(m, 1, ModifierFlags::CAPS_LOCK | ModifierFlags::FUNCTION));
    assert!(!down(m, 1, ModifierFlags::SHIFT));
    assert!(!down(m, 1, ModifierFlags::COMMAND));
    assert!(!down(m, 0, NONE));
    assert!(!down(m, 2, NONE));
}

#[test]
fn ma_03_02_only_a_non_repeat_mouse_down_matches() {
    let m = matcher(RightClick, &[Control, Command]);
    let stream = PressStream::Button(MouseButton(1));
    assert!(!m.matches(InputEventKind::MouseUp, stream, CHORD, false));
    assert!(!m.matches(InputEventKind::KeyDown, stream, CHORD, false));
    assert!(!m.matches(InputEventKind::FlagsChanged, stream, CHORD, false));
    assert!(!m.matches(InputEventKind::MouseDown, stream, CHORD, true));
    assert!(!m.matches(
        InputEventKind::MouseDown,
        PressStream::Key(stache_sdk::input::KeyCode(1)),
        CHORD,
        false
    ));
}

#[test]
fn ma_03_02_duplicate_and_reordered_modifiers_collapse_to_one_mask() {
    let a = matcher(RightClick, &[Control, Command, Control]);
    let b = matcher(RightClick, &[Command, Control]);
    assert_eq!(a, b);
    assert_eq!(held_modifiers(CHORD).bits(), b.modifiers.bits());
    assert_eq!(required_flags(b.modifiers), CHORD);
    assert_eq!(
        held_modifiers(ModifierFlags::CAPS_LOCK | ModifierFlags::FUNCTION).bits(),
        0
    );
    let all = ModifierFlags::CONTROL
        | ModifierFlags::OPTION
        | ModifierFlags::COMMAND
        | ModifierFlags::SHIFT;
    assert_eq!(held_modifiers(all).bits(), 0b1111);
}

#[test]
fn ma_03_02_the_service_prefilter_is_config_independent_and_the_callback_is_exact() {
    let filter = Matcher::prefilter();
    assert!(filter.validate().is_ok());
    assert!(filter.kinds().contains(InputEventKind::MouseDown));
    assert!(!filter.kinds().contains(InputEventKind::MouseUp));
    assert!(!filter.kinds().contains(InputEventKind::KeyDown));
    let at = |button, flags| {
        filter.matches(
            InputEventKind::MouseDown,
            PressStream::Button(MouseButton(button)),
            flags,
        )
    };
    assert!(at(1, CHORD));
    assert!(
        at(1, ModifierFlags::NONE),
        "modifiers are decided by the callback"
    );
    assert!(at(2, ModifierFlags::SHIFT));
    assert!(!at(0, CHORD));
    assert!(!at(3, CHORD));
    assert_eq!(filter.modifiers(), ModifierMatch::Any);
}

#[test]
fn ma_03_02_through_the_shared_service_only_the_exact_chord_is_consumed() {
    let (rig, _app) = Rig::started(default_config());
    let passes = [
        rig.down(0, CHORD),
        rig.down(2, CHORD),
        rig.down(3, CHORD),
        rig.down(1, NONE),
        rig.down(1, ModifierFlags::CONTROL),
        rig.down(1, CHORD | ModifierFlags::SHIFT),
        rig.mouse(InputEventKind::MouseUp, 1, CHORD),
    ];
    assert!(passes.iter().all(|d| *d == Disposition::Pass), "{passes:?}");
    assert_eq!(rig.count(Outcome::Admitted), 0);
    assert!(!rig.runtime.is_busy());
    assert_eq!(
        rig.down(1, CHORD | ModifierFlags::CAPS_LOCK),
        Disposition::Suppress
    );
    assert_eq!(
        rig.up(1, CHORD | ModifierFlags::CAPS_LOCK),
        Disposition::Suppress
    );
    dismiss(&rig);
}

#[test]
fn ma_03_02_a_middle_click_configuration_consumes_button_two_and_passes_the_right_button() {
    let (rig, _app) = Rig::started(config(MiddleClick, &[Alt]));
    assert_eq!(rig.down(1, ModifierFlags::OPTION), Disposition::Pass);
    assert_eq!(rig.down(3, ModifierFlags::OPTION), Disposition::Pass);
    assert_eq!(rig.down(2, NONE), Disposition::Pass);
    assert_eq!(rig.down(2, ModifierFlags::OPTION), Disposition::Suppress);
    assert_eq!(rig.up(2, ModifierFlags::OPTION), Disposition::Suppress);
    dismiss(&rig);
}

/// A scripted competing handler at an explicit priority.
struct Competitor {
    decision: Disposition,
    calls: AtomicUsize,
}

impl InputHandler for Competitor {
    fn on_event(&self, _: &InputEvent, _: &mut CallbackScope) -> Disposition {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.decision
    }
}

fn competitor(rig: &Rig, level: u16, decision: Disposition) -> (Arc<Competitor>, impl Sized) {
    let handler = Arc::new(Competitor {
        decision,
        calls: AtomicUsize::new(0),
    });
    let filter = stache_sdk::input::InputFilter::new(KindSet::of(InputEventKind::MouseDown))
        .with_buttons([MouseButton(1)])
        .with_modifiers(ModifierMatch::AllOf(CHORD));
    let subscription = rig
        .input
        .subscribe_handler(
            HandlerSpec {
                filter,
                priority: InputPriority { level, tie_break: 0 },
                work: WorkRequirement::None,
            },
            Arc::clone(&handler) as Arc<dyn InputHandler>,
            &CancellationToken::new(),
        )
        .expect("subscribe the competitor");
    (handler, subscription)
}

#[test]
fn ma_03_02_a_higher_priority_handler_that_consumes_wins_and_menu_anywhere_stays_idle() {
    let (rig, _app) = Rig::started(default_config());
    let (high, _sub) = competitor(&rig, 300, Disposition::Suppress);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    assert_eq!(high.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        rig.count(Outcome::Admitted),
        0,
        "Hold-to-Quit level outranks MenuAnywhere"
    );
    assert!(!rig.runtime.is_busy());
    assert_eq!(rig.menu.ax.call_count(), 0);
}

#[test]
fn ma_03_02_a_higher_priority_pass_reaches_menu_anywhere_and_a_lower_one_is_never_asked() {
    let (rig, _app) = Rig::started(default_config());
    let (high, _high_sub) = competitor(&rig, 300, Disposition::Pass);
    let (low, _low_sub) = competitor(&rig, 100, Disposition::Suppress);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    assert_eq!(high.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        low.calls.load(Ordering::SeqCst),
        0,
        "MenuAnywhere outranks Keybindings level"
    );
    rig.wait_count(Outcome::Admitted, 1);
    dismiss(&rig);
}

#[test]
fn ma_03_02_a_disabled_tap_that_is_re_enabled_keeps_the_subscription_working() {
    let (rig, _app) = Rig::started(default_config());
    rig.input.tap_disabled(TapKind::Suppressing);
    let cancel = CancellationToken::new();
    let _ = rig.input.retry_reenable(TapKind::Suppressing, &cancel);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    assert_eq!(rig.chord_up(), Disposition::Suppress);
    dismiss(&rig);
    assert_eq!(rig.status(), ModuleStatus::Running);
}

// ---------------------------------------------------------------------------
// MA-03-03: admission, one reservation, bounded queue, callback purity
// ---------------------------------------------------------------------------

fn published(eligible: bool) -> Published {
    Published {
        generation: 1,
        revision: 3,
        matcher: matcher(RightClick, &[Control, Command]),
        permission: eligible,
        service: eligible,
    }
}

fn admission() -> (Admission, std::sync::mpsc::Receiver<Message>, Arc<Counters>) {
    let (sender, receiver) = sync_channel(1);
    let counters = Arc::new(Counters::default());
    (Admission::new(sender, Arc::clone(&counters)), receiver, counters)
}

#[test]
fn ma_03_03_a_free_reservation_and_queue_slot_suppress_with_one_request() {
    let (admission, receiver, counters) = admission();
    let now = Instant::now();
    assert_eq!(
        admission.admit(&published(true), now, None),
        Disposition::Suppress
    );
    assert!(admission.is_busy());
    let Ok(Message::Request(request)) = receiver.try_recv() else {
        panic!("one request was queued");
    };
    assert_eq!(request.reservation, admission.reservation());
    assert_eq!((request.generation, request.revision), (1, 3));
    assert_eq!(request.triggered_at, now);
    assert!(receiver.try_recv().is_err(), "exactly one request");
    assert_eq!(counters.get(Outcome::Admitted), 1);
}

#[test]
fn ma_03_03_a_busy_reservation_swallows_and_drops_the_newest_trigger() {
    let (admission, receiver, counters) = admission();
    assert_eq!(
        admission.admit(&published(true), Instant::now(), None),
        Disposition::Suppress
    );
    let first = admission.reservation();
    for _ in 0..5 {
        assert_eq!(
            admission.admit(&published(true), Instant::now(), None),
            Disposition::Suppress
        );
    }
    assert_eq!(counters.get(Outcome::Coalesced), 5);
    assert_eq!(counters.get(Outcome::Admitted), 1);
    assert_eq!(
        admission.reservation(),
        first,
        "the first reservation is never overwritten"
    );
    assert!(matches!(receiver.try_recv(), Ok(Message::Request(_))));
    assert!(receiver.try_recv().is_err(), "no queue growth and no replay");
}

#[test]
fn ma_03_03_a_full_queue_without_a_busy_reservation_passes_and_frees_its_own_reservation() {
    let (sender, receiver) = sync_channel(1);
    let counters = Arc::new(Counters::default());
    let admission = Admission::new(sender.clone(), Arc::clone(&counters));
    sender.try_send(Message::Stop).expect("fill the slot");
    assert_eq!(
        admission.admit(&published(true), Instant::now(), None),
        Disposition::Pass
    );
    assert!(!admission.is_busy(), "queue pressure never leaks a reservation");
    assert_eq!(counters.get(Outcome::QueueFull), 1);
    assert!(admission.worker_available());
    assert!(matches!(receiver.try_recv(), Ok(Message::Stop)));
    assert_eq!(
        admission.admit(&published(true), Instant::now(), None),
        Disposition::Suppress
    );
}

#[test]
fn ma_03_03_a_disconnected_queue_passes_frees_the_reservation_and_marks_health_false() {
    let (admission, receiver, counters) = admission();
    drop(receiver);
    assert_eq!(
        admission.admit(&published(true), Instant::now(), None),
        Disposition::Pass
    );
    assert!(!admission.is_busy());
    assert!(!admission.worker_available());
    assert_eq!(counters.get(Outcome::WorkerUnavailable), 1);
}

#[test]
fn ma_03_03_a_stale_holder_can_only_release_its_own_reservation() {
    let (admission, receiver, _counters) = admission();
    assert_eq!(
        admission.admit(&published(true), Instant::now(), None),
        Disposition::Suppress
    );
    let first = admission.reservation();
    let _ = receiver.try_recv();
    assert!(admission.release(first));
    assert!(!admission.release(first), "released exactly once");
    assert_eq!(
        admission.admit(&published(true), Instant::now(), None),
        Disposition::Suppress
    );
    let second = admission.reservation();
    assert_ne!(first, second);
    assert!(
        !admission.release(first),
        "a stale release cannot clear the successor"
    );
    assert!(admission.is_busy());
    assert!(!admission.release(0));
    assert!(admission.release(second));
}

#[test]
fn ma_03_03_an_ineligible_or_dead_tuple_passes_without_work() {
    let (rig, _app) = Rig::started(default_config());
    rig.runtime.begin_shutdown();
    assert_eq!(rig.chord_down(), Disposition::Pass);
    assert_eq!(rig.count(Outcome::Admitted), 0);
    assert!(!rig.runtime.is_busy());
}

#[test]
fn ma_03_03_matches_during_a_read_are_coalesced_and_replay_nothing() {
    let (rig, _app) = Rig::started(default_config());
    let hold = hold_ax(&rig.menu.ax, Op::MenuBar);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    assert_eq!(rig.chord_up(), Disposition::Suppress);
    hold.wait_entered();
    assert!(rig.runtime.is_busy());
    for _ in 0..3 {
        assert_eq!(rig.chord_down(), Disposition::Suppress, "swallowed while busy");
        assert_eq!(rig.chord_up(), Disposition::Suppress);
    }
    assert_eq!(rig.count(Outcome::Coalesced), 3);
    assert_eq!(rig.count(Outcome::Admitted), 1);
    hold.release();
    let id = rig.wait_tracking();
    for _ in 0..2 {
        assert_eq!(
            rig.chord_down(),
            Disposition::Suppress,
            "swallowed while showing"
        );
        assert_eq!(rig.chord_up(), Disposition::Suppress);
    }
    assert_eq!(rig.count(Outcome::Coalesced), 5);
    rig.menu.presenter.dismiss(id);
    rig.wait_idle();
    assert_eq!(
        rig.menu.presenter.prepared().len(),
        1,
        "one popup, never a replay"
    );
    assert_eq!(rig.count(Outcome::Admitted), 1);
    // The reservation is free again: the next trigger starts a fresh popup.
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    rig.wait_count(Outcome::Admitted, 2);
    dismiss(&rig);
}

#[test]
fn ma_03_03_the_callback_fails_open_while_the_tuple_is_being_replaced() {
    let (rig, _app) = Rig::started(default_config());
    let seen = rig.runtime.while_publishing(|| rig.chord_down()).expect("a run");
    assert_eq!(
        seen,
        Disposition::Pass,
        "a contended read never blocks or suppresses"
    );
    assert_eq!(rig.count(Outcome::SnapshotBusy), 1);
    assert!(!rig.runtime.is_busy());
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    dismiss(&rig);
}

#[test]
fn ma_03_03_a_callback_never_touches_applications_ax_or_the_presenter() {
    let (rig, _app) = Rig::started(default_config());
    let hold = rig.hold(Stage::Admitted);
    let queries = rig.apps.frontmost_queries();
    let is_running = rig.apps.is_running_calls();
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    hold.wait_entered();
    assert_eq!(
        rig.apps.frontmost_queries(),
        queries,
        "the callback asked nothing"
    );
    assert_eq!(rig.apps.is_running_calls(), is_running);
    assert_eq!(rig.menu.ax.call_count(), 0);
    assert!(rig.menu.presenter.prepared().is_empty());
    hold.release();
    dismiss(&rig);
}

// ---------------------------------------------------------------------------
// MA-03-04: acceptance coherence
// ---------------------------------------------------------------------------

#[test]
fn ma_03_04_an_enabled_reload_replaces_the_filter_and_publishes_the_new_revision() {
    let (rig, _app) = Rig::started(default_config());
    let before = rig.runtime.published().expect("published");
    rig.registry.reload(rig.handle, config(RightClick, &[Shift])).expect("reload");
    wait_adopted(&rig);
    let after = rig.runtime.published().expect("published");
    assert!(after.revision > before.revision);
    assert_eq!(after.matcher, matcher(RightClick, &[Shift]));
    assert_eq!(
        rig.chord_down(),
        Disposition::Pass,
        "the old chord no longer matches"
    );
    assert_eq!(rig.down(1, ModifierFlags::SHIFT), Disposition::Suppress);
    assert_eq!(rig.up(1, ModifierFlags::SHIFT), Disposition::Suppress);
    dismiss(&rig);
}

#[test]
fn ma_03_04_a_button_change_swaps_the_service_prefilter_too() {
    let (rig, _app) = Rig::started(default_config());
    rig.registry
        .reload(rig.handle, config(MiddleClick, &[Control, Command]))
        .expect("reload");
    wait_adopted(&rig);
    assert_eq!(rig.down(1, CHORD), Disposition::Pass);
    assert_eq!(rig.down(2, CHORD), Disposition::Suppress);
    assert_eq!(rig.up(2, CHORD), Disposition::Suppress);
    dismiss(&rig);
}

#[test]
fn ma_03_04_a_callback_during_staging_uses_the_previous_accepted_tuple() {
    let (rig, _app) = Rig::started(default_config());
    let held = rig.hold(Stage::AdoptBeforePublish);
    let before = rig.runtime.published().expect("published");
    let submitted = rig.tap.submitted().len();
    rig.registry.reload(rig.handle, config(MiddleClick, &[Shift])).expect("reload");
    held.wait_entered();
    // The registry published the candidate, but the tuple has not flipped, and
    // the service prefilter never changes, so no mixed combination is consumed.
    assert_eq!(rig.runtime.published().expect("published"), before);
    assert_eq!(rig.down(2, ModifierFlags::SHIFT), Disposition::Pass);
    assert_eq!(rig.down(2, CHORD), Disposition::Pass);
    assert_eq!(rig.down(1, ModifierFlags::SHIFT), Disposition::Pass);
    assert_eq!(rig.count(Outcome::Admitted), 0);
    assert_eq!(
        rig.chord_down(),
        Disposition::Suppress,
        "the previous tuple still decides"
    );
    assert_eq!(rig.chord_up(), Disposition::Suppress);
    dismiss(&rig);
    held.release();
    wait_adopted(&rig);
    assert_eq!(rig.chord_down(), Disposition::Pass);
    assert_eq!(rig.down(2, ModifierFlags::SHIFT), Disposition::Suppress);
    assert_eq!(rig.up(2, ModifierFlags::SHIFT), Disposition::Suppress);
    dismiss(&rig);
    assert_eq!(
        rig.tap.submitted().len(),
        submitted,
        "a reconfigure never touches the shared-service filter"
    );
}

#[test]
fn ma_03_04_an_unchanged_reload_keeps_the_filter_and_revision() {
    let (rig, _app) = Rig::started(default_config());
    let before = rig.runtime.published().expect("published");
    let submitted = rig.tap.submitted().len();
    rig.registry.reload(rig.handle, default_config()).expect("unchanged");
    assert_eq!(rig.runtime.published().expect("published"), before);
    assert_eq!(revision_of(&rig), before.revision);
    assert_eq!(rig.tap.submitted().len(), submitted, "no native change");
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    dismiss(&rig);
}

#[test]
fn ma_03_04_a_paused_reload_stages_for_the_next_start_and_changes_nothing_live() {
    let (rig, _app) = Rig::started(default_config());
    rig.registry.pause(rig.handle).expect("pause");
    assert_eq!(rig.status(), ModuleStatus::Paused);
    assert!(rig.runtime.published().is_none(), "no run while paused");
    rig.registry
        .reload(rig.handle, config(RightClick, &[Shift]))
        .expect("reload while paused");
    assert_eq!(
        rig.status(),
        ModuleStatus::Paused,
        "a reload never overrides the pause"
    );
    assert_eq!(rig.chord_down(), Disposition::Pass);
    assert_eq!(rig.down(1, ModifierFlags::SHIFT), Disposition::Pass);
    rig.registry.resume(rig.handle).expect("resume");
    assert_eq!(rig.status(), ModuleStatus::Running);
    let published = rig.runtime.published().expect("published");
    assert_eq!(published.matcher, matcher(RightClick, &[Shift]));
    assert_eq!(published.revision, revision_of(&rig));
    assert_eq!(rig.down(1, ModifierFlags::SHIFT), Disposition::Suppress);
    rig.wait_count(Outcome::Admitted, 1);
    dismiss(&rig);
}

#[test]
fn ma_03_04_a_disabling_reload_ends_eligibility_and_an_enabling_reload_restarts() {
    let (rig, _app) = Rig::started(default_config());
    let mut disabled = default_config();
    disabled.enabled = false;
    rig.registry.reload(rig.handle, disabled).expect("disable");
    assert_eq!(rig.status(), ModuleStatus::Disabled);
    assert_eq!(rig.chord_down(), Disposition::Pass);
    assert_eq!(rig.apps.activation_observers(), 0, "everything released");
    assert!(!rig.tap.is_live(TapKind::Suppressing));
    rig.registry.reload(rig.handle, config(RightClick, &[Alt])).expect("enable");
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(
        rig.runtime.published().expect("published").revision,
        revision_of(&rig)
    );
    assert_eq!(rig.down(1, ModifierFlags::OPTION), Disposition::Suppress);
    rig.wait_count(Outcome::Admitted, 1);
    dismiss(&rig);
}

#[test]
fn ma_03_04_an_enabled_reconfigure_keeps_the_in_flight_presentations_authority() {
    let (rig, _app) = Rig::started(default_config());
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let id = rig.wait_tracking();
    rig.registry
        .reload(rig.handle, config(RightClick, &[Shift]))
        .expect("reconfigure");
    wait_adopted(&rig);
    // The popup shown under the captured revision still finishes.
    assert!(rig.menu.presenter.is_tracking(id));
    rig.choose(id, "Open");
    rig.wait_idle();
    assert_eq!(
        rig.menu.ax.ops(Op::Press).len(),
        1,
        "the captured revision finished its action"
    );
    assert_eq!(rig.count(Outcome::ActionPressed), 1);
    // Subsequent triggers use the new filter.
    assert_eq!(rig.chord_down(), Disposition::Pass);
    assert_eq!(rig.down(1, ModifierFlags::SHIFT), Disposition::Suppress);
    dismiss(&rig);
}

#[test]
fn ma_03_04_a_reconfigure_racing_the_immediate_send_does_not_cancel_the_action() {
    let (rig, _app) = Rig::started(default_config());
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let id = rig.wait_tracking();
    let before_action = rig.hold(Stage::BeforeAction);
    rig.choose(id, "Close");
    before_action.wait_entered();
    rig.registry.reload(rig.handle, config(MiddleClick, &[])).expect("reconfigure");
    wait_adopted(&rig);
    before_action.release();
    rig.wait_idle();
    assert_eq!(rig.menu.ax.ops(Op::Press).len(), 1);
    assert_eq!(rig.apps.exact_activation_requests().len(), 1);
}

// ---------------------------------------------------------------------------
// MA-03-05: capture timing and identity
// ---------------------------------------------------------------------------

#[test]
fn ma_03_05_trigger_and_capture_times_are_recorded_separately_and_honestly() {
    let (rig, _app) = Rig::started(default_config());
    let held = rig.hold(Stage::Admitted);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    held.wait_entered();
    rig.menu.clock.advance(Duration::from_millis(700));
    held.release();
    dismiss(&rig);
    let report = rig.runtime.reports().pop().expect("a report");
    let started = report.capture_started_at.expect("capture started");
    let captured = report.captured_at.expect("captured");
    assert_eq!(
        started.duration_since(report.triggered_at),
        Duration::from_millis(700)
    );
    assert!(captured >= started);
    assert!(report.target.is_some());
    assert_eq!(report.outcome, Outcome::Shown);
}

#[test]
fn ma_03_05_a_request_older_than_the_capture_window_is_dropped_without_native_work() {
    let (rig, _app) = Rig::started(default_config());
    let held = rig.hold(Stage::Admitted);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    assert_eq!(rig.chord_up(), Disposition::Suppress);
    held.wait_entered();
    rig.menu.clock.advance(CAPTURE_WINDOW + Duration::from_millis(1));
    let queries = rig.apps.frontmost_queries();
    held.release();
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::RequestStale), 1);
    assert_eq!(
        rig.apps.frontmost_queries(),
        queries,
        "no capture for a stale request"
    );
    assert_eq!(rig.menu.ax.call_count(), 0);
    assert!(rig.menu.presenter.prepared().is_empty());
    // The trigger stays consumed and nothing replays; the next click works.
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    dismiss(&rig);
}

#[test]
fn ma_03_05_a_capture_hop_over_the_window_is_rejected() {
    let (rig, _app) = Rig::started(default_config());
    let barrier = rig.apps.block_next_frontmost();
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    assert!(barrier.wait_entered(Duration::from_secs(10)));
    rig.menu.clock.advance(CAPTURE_WINDOW + Duration::from_millis(1));
    let _ = barrier.release();
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::CaptureSlow), 1);
    assert_eq!(rig.menu.ax.call_count(), 0, "a slow capture never reads");
}

#[test]
fn ma_03_05_the_application_frontmost_at_processing_is_the_observed_capture_target() {
    let (rig, first) = Rig::started(default_config());
    // Capture with the first application frontmost.
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    dismiss(&rig);
    let first_target = rig.runtime.reports().pop().expect("report").target.expect("target");
    // A different application becomes frontmost between the click and processing.
    let second = rig.add_app("com.example.other", 5151);
    let held = rig.hold(Stage::Admitted);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    held.wait_entered();
    rig.focus(second);
    held.release();
    dismiss(&rig);
    let second_target = rig.runtime.reports().pop().expect("report").target.expect("target");
    assert_ne!(first_target, second_target, "no reconstructed click-time target");
    let _ = first;
}

#[test]
fn ma_03_05_no_frontmost_application_is_a_typed_outcome_and_frees_the_reservation() {
    let (rig, _app) = Rig::started(default_config());
    rig.apps.set_frontmost(None);
    let _ = rig.apps.deliver_activation();
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::NoFrontmost), 1);
    assert_eq!(rig.menu.ax.call_count(), 0);
    assert_eq!(
        rig.status(),
        ModuleStatus::Running,
        "a read error never fails the lifecycle"
    );
}

#[test]
fn ma_03_05_this_process_is_never_read_or_shown() {
    let (rig, _app) = Rig::started(default_config());
    let own = rig.add_app("com.example.stache", 7);
    rig.apps.set_current_process(Some(own.key));
    rig.focus(own);
    assert_eq!(
        rig.chord_down(),
        Disposition::Suppress,
        "the click is still consumed"
    );
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::SelfTarget), 1);
    assert_eq!(rig.menu.ax.call_count(), 0);
    assert!(rig.menu.presenter.prepared().is_empty());
}

#[test]
fn ma_03_05_a_focus_change_during_the_read_is_a_stale_target_and_shows_nothing() {
    let (rig, _app) = Rig::started(default_config());
    let other = rig.add_app("com.example.other", 5151);
    let hold = hold_ax(&rig.menu.ax, Op::MenuBar);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    hold.wait_entered();
    rig.focus(other);
    hold.release();
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::StaleTarget), 1);
    assert!(
        rig.menu.presenter.prepared().is_empty(),
        "never a menu for the new frontmost app"
    );
    assert_eq!(rig.menu.ax.ops(Op::Press).len(), 0);
}

#[test]
fn ma_03_05_a_focus_change_between_the_read_and_the_show_is_stale() {
    let (rig, _app) = Rig::started(default_config());
    let other = rig.add_app("com.example.other", 5151);
    let held = rig.hold(Stage::BeforeShow);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    held.wait_entered();
    rig.focus(other);
    held.release();
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::StaleTarget), 1);
    assert!(rig.menu.presenter.prepared().is_empty());
}

#[test]
fn ma_03_05_the_captured_instance_terminating_before_the_show_is_stale() {
    let (rig, app) = Rig::started(default_config());
    let held = rig.hold(Stage::BeforeShow);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    held.wait_entered();
    rig.apps.terminate(app.key);
    held.release();
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::StaleTarget), 1);
    assert!(rig.menu.presenter.prepared().is_empty());
}

#[test]
fn ma_03_05_the_action_targets_the_captured_instance_even_if_focus_moved_while_tracking() {
    let (rig, app) = Rig::started(default_config());
    let other = rig.add_app("com.example.other", 5151);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let id = rig.wait_tracking();
    rig.focus(other);
    rig.choose(id, "Open");
    rig.wait_idle();
    assert_eq!(
        rig.apps.exact_activation_requests(),
        vec![app.key],
        "no fallback to the new app"
    );
    assert_eq!(rig.menu.ax.ops(Op::Press).len(), 1);
    assert_eq!(rig.count(Outcome::ActionPressed), 1);
}

#[test]
fn ma_03_05_a_relaunched_instance_with_the_same_pid_and_bundle_is_stale_for_the_action() {
    let (rig, app) = Rig::started(default_config());
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let id = rig.wait_tracking();
    rig.apps.terminate(app.key);
    let relaunched = rig.add_app("com.example.editor", app.pid);
    rig.focus(relaunched);
    rig.choose(id, "Open");
    rig.wait_idle();
    assert_eq!(
        rig.menu.ax.ops(Op::Press).len(),
        0,
        "no press for a replaced instance"
    );
    assert_eq!(rig.count(Outcome::ActionPressed), 0);
    assert_eq!(rig.count(Outcome::ActionStale), 1, "{:?}", rig.runtime.reports());
    assert!(!rig.apps.exact_activation_requests().contains(&relaunched.key));
}

#[test]
fn ma_03_05_an_activation_refusal_issues_no_press_and_is_never_retried() {
    let (rig, _app) = Rig::started(default_config());
    rig.apps.refuse_exact_activation();
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let id = rig.wait_tracking();
    rig.choose(id, "Open");
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::ActionRefused), 1);
    assert_eq!(rig.apps.exact_activation_attempts(), 1, "no retry");
    assert_eq!(rig.menu.ax.ops(Op::Press).len(), 0);
}

#[test]
fn ma_03_05_an_event_without_a_point_is_a_typed_outcome() {
    let (rig, _app) = Rig::started(default_config());
    assert_eq!(
        rig.mouse_at(InputEventKind::MouseDown, 1, CHORD, None),
        Disposition::Suppress
    );
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::PointerUnavailable), 1);
    assert_eq!(rig.menu.ax.call_count(), 0);
}

#[test]
fn ma_03_05_the_menu_is_shown_at_the_event_quartz_point() {
    let (rig, _app) = Rig::started(default_config());
    let gate = rig.hold(Stage::BeforeCapture);
    // The event point is captured by the callback; nothing sampled later can
    // move it, however long the worker takes to start.
    assert_eq!(
        rig.mouse_at(
            InputEventKind::MouseDown,
            1,
            CHORD,
            Some(InputPoint { x: 640.0, y: 300.0 })
        ),
        Disposition::Suppress
    );
    gate.wait_entered();
    gate.release();
    let _ = rig.wait_tracking();
    let located = rig.menu.presenter.track_locations();
    assert_eq!(located.len(), 1);
    // single_display is 1440x900 with matching AppKit space: y is flipped.
    assert_eq!((located[0].x, located[0].y), (640.0, 600.0));
    dismiss(&rig);
}

#[test]
fn ma_03_05_read_failures_are_typed_counted_and_never_fail_the_lifecycle() {
    let (rig, _app) = Rig::started(default_config());
    rig.menu.ax.inject(Op::MenuBar, None, stache_macos::menu::AxError::ApiDisabled);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::ReadPermissionDenied), 1);
    assert_eq!(
        rig.status(),
        ModuleStatus::Running,
        "an error is counted, not a lifecycle failure"
    );
    assert!(
        rig.logger
            .warnings()
            .iter()
            .all(|warning| !["Apple", "File", "Open", "Close", "About"]
                .iter()
                .any(|title| warning.contains(title))),
        "no menu titles in logs"
    );
}

#[test]
fn ma_03_05_an_application_without_a_menu_bar_is_a_typed_no_menu() {
    let rig = Rig::with_menu(default_config(), Vec::new());
    let app = rig.add_app("com.example.editor", 4242);
    rig.registry.start_all().expect("start");
    rig.focus(app);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::NoMenu), 1);
    assert!(rig.menu.presenter.prepared().is_empty());
}

#[test]
fn ma_03_05_a_rejected_dispatch_is_a_typed_show_failure_with_a_clean_release() {
    let (rig, _app) = Rig::started(default_config());
    rig.menu.main.reject(true);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::ShowTimeout), 1);
    rig.menu.main.reject(false);
    wait_until("the retained elements to be released", || {
        rig.menu.ax.counts().live() == 0
    });
}

// ---------------------------------------------------------------------------
// MA-03-06: lifecycle, permissions, rollback, recovery
// ---------------------------------------------------------------------------

#[test]
fn ma_03_06_missing_accessibility_waits_without_acquiring_or_prompting() {
    let rig = Rig::new(default_config());
    rig.trust.set(false);
    rig.registry.start_all().expect("start");
    assert_eq!(
        rig.status(),
        ModuleStatus::WaitingForPermission(Permission::Accessibility)
    );
    assert_eq!(rig.menu.opens(), 0);
    assert_eq!(rig.apps.observations(), 0);
    assert!(!rig.tap.is_live(TapKind::Suppressing));
    assert_eq!(rig.trust.requests(), 0, "startup only checks");
}

#[test]
fn ma_03_06_grant_then_revoke_then_grant_never_requests_and_follows_the_permission() {
    let rig = Rig::new(default_config());
    rig.trust.set(false);
    rig.registry.start_all().expect("start");
    rig.trust.set(true);
    rig.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
        .expect("grant");
    assert_eq!(rig.status(), ModuleStatus::Running);
    rig.trust.set(false);
    rig.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
        .expect("revoke");
    assert_eq!(
        rig.status(),
        ModuleStatus::WaitingForPermission(Permission::Accessibility)
    );
    assert!(rig.runtime.published().is_none(), "torn down");
    assert!(!rig.tap.is_live(TapKind::Suppressing));
    assert_eq!(rig.apps.activation_observers(), 0);
    rig.trust.set(true);
    rig.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
        .expect("grant");
    assert_eq!(rig.status(), ModuleStatus::Running);
    rig.registry.reload(rig.handle, config(RightClick, &[Shift])).expect("reload");
    rig.registry.set_enabled(rig.handle, false).expect("disable");
    rig.registry.set_enabled(rig.handle, true).expect("enable");
    assert_eq!(rig.trust.requests(), 0, "enable, reload and recovery only check");
}

#[test]
fn ma_03_06_recovery_never_overrides_a_pause_or_a_disable() {
    let rig = Rig::new(default_config());
    rig.registry.start_all().expect("start");
    rig.registry.pause(rig.handle).expect("pause");
    rig.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
        .expect("grant");
    assert_eq!(rig.status(), ModuleStatus::Paused);
    rig.registry.resume(rig.handle).expect("resume");
    rig.registry.set_enabled(rig.handle, false).expect("disable");
    rig.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
        .expect("grant");
    assert_eq!(rig.status(), ModuleStatus::Disabled);
    assert!(rig.runtime.published().is_none());
}

#[test]
fn ma_03_06_a_missing_input_service_is_a_typed_dependency_failure_that_acquires_nothing() {
    let rig = Rig::without_input_service(default_config());
    let _ = rig.registry.start_all();
    let ModuleStatus::Failed(failure) = rig.status() else {
        panic!("expected a failed start, got {:?}", rig.status());
    };
    assert_eq!(failure.operation, Operation::Start);
    assert_eq!(failure.category, ErrorCategory::Dependency);
    assert_eq!(rig.menu.opens(), 0);
    assert_eq!(rig.apps.observations(), 0);
}

fn assert_fully_rolled_back(rig: &Rig) {
    assert!(rig.runtime.published().is_none(), "no run is retained");
    assert_eq!(rig.apps.activation_observers(), 0, "observer removed");
    assert!(!rig.tap.is_live(TapKind::Suppressing), "no tap");
    assert!(!rig.runtime.is_busy());
}

#[test]
fn ma_03_06_a_failed_menu_stack_rolls_the_start_back_and_a_retry_recovers() {
    let rig = Rig::new(default_config());
    rig.menu.fail_next_open();
    let _ = rig.registry.start_all();
    let ModuleStatus::Failed(failure) = rig.status() else {
        panic!("{:?}", rig.status())
    };
    assert_eq!(
        (failure.operation, failure.category),
        (Operation::Start, ErrorCategory::Platform)
    );
    assert_fully_rolled_back(&rig);
    rig.registry.set_enabled(rig.handle, true).expect("retry");
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(rig.menu.opens(), 1);
}

#[test]
fn ma_03_06_a_rejected_subscription_rolls_back_the_observer_and_the_menu_stack() {
    let rig = Rig::new(default_config());
    rig.tap.script(
        TapKind::Suppressing,
        ControlAction::Install,
        FakeResponse::Fail("denied".into()),
    );
    let _ = rig.registry.start_all();
    let ModuleStatus::Failed(failure) = rig.status() else {
        panic!("{:?}", rig.status())
    };
    assert_eq!(failure.operation, Operation::Start);
    assert_eq!(rig.menu.opens(), 1, "the stack was opened and then closed");
    assert_fully_rolled_back(&rig);
    rig.registry.set_enabled(rig.handle, true).expect("retry");
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert!(rig.tap.is_live(TapKind::Suppressing));
}

#[test]
fn ma_03_06_a_failed_observer_install_rolls_back_cleanly() {
    let rig = Rig::new(default_config());
    rig.apps.fail_next_observe();
    let _ = rig.registry.start_all();
    assert!(matches!(rig.status(), ModuleStatus::Failed(_)));
    assert_eq!(rig.menu.opens(), 0);
    assert_fully_rolled_back(&rig);
    rig.registry.set_enabled(rig.handle, true).expect("retry");
    assert_eq!(rig.status(), ModuleStatus::Running);
}

#[test]
fn ma_03_06_a_tap_that_cannot_be_re_enabled_does_not_rewrite_the_lifecycle() {
    let (rig, _app) = Rig::started(default_config());
    rig.tap.script(
        TapKind::Suppressing,
        ControlAction::Enable,
        FakeResponse::Fail("no".into()),
    );
    rig.input.tap_disabled(TapKind::Suppressing);
    wait_until("the failed re-enable", || {
        matches!(
            rig.input.tap_status(TapKind::Suppressing),
            TapStatus::ReenableFailed { .. }
        )
    });
    assert_eq!(
        rig.status(),
        ModuleStatus::Running,
        "the shared service owns that failure"
    );
    assert!(rig.runtime.health_detail().is_none());
    // A later successful re-enable recovers the same subscription.
    rig.input
        .retry_reenable(TapKind::Suppressing, &CancellationToken::new())
        .expect("the retry succeeds");
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    dismiss(&rig);
}

#[test]
fn ma_03_06_a_dead_worker_fails_open_withdraws_eligibility_and_reports_degraded_not_failed() {
    let (rig, _app) = Rig::started(default_config());
    rig.runtime.set_stage_hook(Some(Arc::new(|stage| {
        assert_ne!(stage, Stage::Admitted, "injected worker death");
    })));
    assert_eq!(
        rig.chord_down(),
        Disposition::Suppress,
        "the admitted click stays consumed"
    );
    wait_until("the reservation to free after the worker died", || {
        !rig.runtime.is_busy()
    });
    assert_eq!(
        rig.chord_down(),
        Disposition::Pass,
        "a disconnected queue passes"
    );
    assert_eq!(rig.count(Outcome::WorkerUnavailable), 1);
    wait_until("the health check", || rig.runtime.health_detail().is_some());
    assert!(
        !rig.runtime.published().expect("published").service,
        "ineligible"
    );
    assert_eq!(
        rig.status(),
        ModuleStatus::Running,
        "the registry lifecycle is not rewritten"
    );
    assert_eq!(rig.chord_down(), Disposition::Pass);
}

// ---------------------------------------------------------------------------
// MA-03-07: stop, disable, pause, permission loss, pending cleanup, successors
// ---------------------------------------------------------------------------

fn assert_menu_gone(rig: &Rig, id: stache_macos::menu::PresentationId) {
    // Nothing dismissed it, so removal proves tracking was cancelled first.
    assert!(!rig.menu.presenter.is_tracking(id), "tracking ended");
    assert!(
        rig.menu.presenter.removed().contains(&id),
        "the native menu was removed"
    );
    assert!(!rig.menu.presenter.has_menu(id));
    wait_until("retained elements to be released", || {
        rig.menu.ax.counts().live() == 0
    });
}

#[test]
fn ma_03_07_pause_during_tracking_cancels_the_menu_and_issues_nothing() {
    let (rig, _app) = Rig::started(default_config());
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    assert_eq!(rig.chord_up(), Disposition::Suppress);
    let id = rig.wait_tracking();
    rig.registry.pause(rig.handle).expect("pause");
    assert_eq!(rig.status(), ModuleStatus::Paused);
    assert_menu_gone(&rig, id);
    assert_eq!(rig.chord_down(), Disposition::Pass, "ineligible at once");
    assert_eq!(rig.menu.ax.ops(Op::Press).len(), 0);
    assert_eq!(rig.apps.exact_activation_attempts(), 0);
    assert!(!rig.runtime.is_busy());
    wait_until("the tap to be removed", || {
        !rig.tap.is_live(TapKind::Suppressing)
    });
}

#[test]
fn ma_03_07_disable_during_tracking_cancels_the_menu() {
    let (rig, _app) = Rig::started(default_config());
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let id = rig.wait_tracking();
    rig.registry.set_enabled(rig.handle, false).expect("disable");
    assert_eq!(rig.status(), ModuleStatus::Disabled);
    assert_menu_gone(&rig, id);
}

#[test]
fn ma_03_07_permission_loss_during_tracking_cancels_the_menu_and_waits() {
    let (rig, _app) = Rig::started(default_config());
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let id = rig.wait_tracking();
    rig.trust.set(false);
    rig.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
        .expect("revoke");
    assert_eq!(
        rig.status(),
        ModuleStatus::WaitingForPermission(Permission::Accessibility)
    );
    assert_menu_gone(&rig, id);
    assert_eq!(rig.chord_down(), Disposition::Pass);
}

#[test]
fn ma_03_07_pause_between_selection_and_action_issues_no_activation_or_press() {
    let (rig, _app) = Rig::started(default_config());
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let id = rig.wait_tracking();
    let held = rig.hold(Stage::BeforeAction);
    rig.choose(id, "Open");
    held.wait_entered();
    // The registry joins the worker, so release it once the stop has revoked
    // authority (eligibility ends before any worker is joined).
    let releaser = {
        let held = Arc::clone(&held);
        let runtime = rig.runtime.clone();
        std::thread::spawn(move || {
            wait_until("eligibility to be revoked", || {
                runtime.published().is_none_or(|published| !published.eligible())
            });
            held.release();
        })
    };
    rig.registry.pause(rig.handle).expect("pause");
    releaser.join().expect("releaser");
    wait_until("the worker to finish", || rig.count(Outcome::ActionStale) >= 1);
    assert_eq!(
        rig.menu.ax.ops(Op::Press).len(),
        0,
        "an unissued press never happens"
    );
    assert_eq!(rig.apps.exact_activation_attempts(), 0);
    wait_until("the lease to end and the elements to be released", || {
        rig.menu.ax.counts().live() == 0
    });
}

#[test]
fn ma_03_07_stop_with_a_stalled_press_reports_pending_then_releases_exactly_once() {
    let (rig, _app) = Rig::started(default_config());
    let press = hold_ax(&rig.menu.ax, Op::Press);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let id = rig.wait_tracking();
    rig.choose(id, "Open");
    press.wait_entered();
    // The owner thread is inside the AX press: invalidation is immediate, but
    // the owner cannot acknowledge its shutdown.
    let started = Instant::now();
    let stopped = rig.registry.pause(rig.handle);
    assert!(started.elapsed() < Duration::from_secs(5), "stop is bounded");
    assert!(
        stopped.is_err(),
        "a stalled owner is reported, not claimed released"
    );
    assert!(
        matches!(rig.status(), ModuleStatus::Failed(failure) if failure.operation == Operation::Stop)
    );
    assert_eq!(
        rig.chord_down(),
        Disposition::Pass,
        "eligibility was revoked immediately"
    );
    assert!(rig.count(Outcome::CleanupPending) >= 1);
    press.release();
    wait_until("the press to resolve", || rig.menu.ax.counts().live() == 0);
    assert_eq!(
        rig.menu.ax.ops(Op::Press).len(),
        1,
        "the press already issued is never repeated"
    );
    assert!(
        rig.runtime.finish_shutdown(),
        "the retained cleanup completes on retry"
    );
    assert_eq!(rig.menu.ax.counts().live(), 0, "released exactly once");
    assert_eq!(rig.menu.ax.counts().created, rig.menu.ax.counts().released);
}

#[test]
fn ma_03_07_a_late_read_from_a_stopped_run_cannot_touch_the_successor() {
    let (rig, _app) = Rig::started(default_config());
    let read = hold_ax(&rig.menu.ax, Op::MenuBar);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    read.wait_entered();
    let _ = rig.registry.pause(rig.handle);
    assert!(!rig.runtime.is_busy() || rig.runtime.published().is_some());
    read.release();
    // The retained predecessor resolves at the next start, which then succeeds.
    wait_until("the stalled owner to drain", || rig.runtime.finish_shutdown());
    rig.registry.set_enabled(rig.handle, true).expect("enable");
    rig.registry.resume(rig.handle).expect("resume");
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert!(!rig.runtime.is_busy(), "the successor starts free");
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let id = rig.wait_tracking();
    assert_eq!(
        rig.menu.presenter.tracked().len(),
        1,
        "only the successor's popup exists"
    );
    rig.menu.presenter.dismiss(id);
    rig.wait_idle();
    assert_eq!(rig.count(Outcome::Shown), 1);
}

#[test]
fn ma_03_07_a_removal_that_is_not_acknowledged_stays_pending_and_never_hits_a_successor() {
    let (rig, _app) = Rig::started(default_config());
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let old = rig.wait_tracking();
    rig.menu.main.pause();
    let stopped = rig.registry.pause(rig.handle);
    assert!(stopped.is_err(), "the unacknowledged removal is reported");
    assert!(rig.count(Outcome::CleanupPending) >= 1);
    assert!(
        !rig.runtime.finish_shutdown(),
        "still pending while main is stalled"
    );
    rig.menu.main.resume();
    wait_until("the eventual cleanup", || rig.runtime.finish_shutdown());
    assert!(rig.menu.presenter.removed().contains(&old));
    rig.registry.set_enabled(rig.handle, true).expect("enable");
    rig.registry.resume(rig.handle).expect("resume");
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let fresh = rig.wait_tracking();
    assert_ne!(fresh, old);
    assert!(
        rig.menu.presenter.has_menu(fresh),
        "the late cleanup did not remove the successor"
    );
    rig.menu.presenter.dismiss(fresh);
    rig.wait_idle();
}

// ---------------------------------------------------------------------------
// MA-03-08: supported shutdown
// ---------------------------------------------------------------------------

#[test]
fn ma_03_08_registry_shutdown_with_an_open_modal_menu_cancels_it_and_joins_the_owners() {
    let (rig, _app) = Rig::started(default_config());
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let id = rig.wait_tracking();
    rig.runtime.begin_shutdown();
    assert_eq!(
        rig.chord_down(),
        Disposition::Pass,
        "stage one revokes immediately"
    );
    let started = Instant::now();
    rig.registry.shutdown().expect("shutdown");
    assert!(started.elapsed() < Duration::from_secs(5), "responsive");
    assert_menu_gone(&rig, id);
    assert!(rig.runtime.finish_shutdown(), "nothing owed after a clean stop");
    assert!(!rig.runtime.is_busy());
    assert_eq!(rig.apps.activation_observers(), 0);
}

#[test]
fn ma_03_08_a_stalled_press_does_not_deadlock_shutdown() {
    let (rig, _app) = Rig::started(default_config());
    let press = hold_ax(&rig.menu.ax, Op::Press);
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    let id = rig.wait_tracking();
    rig.choose(id, "Close");
    press.wait_entered();
    let started = Instant::now();
    let _ = rig.registry.shutdown();
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "shutdown returned while the press stalled"
    );
    press.release();
    wait_until("the retained cleanup", || rig.runtime.finish_shutdown());
}

#[test]
fn ma_03_07_repeated_pause_resume_cycles_do_not_keep_past_runs_alive() {
    let (rig, _app) = Rig::started(default_config());
    let mut probes = Vec::new();
    for _ in 0..3 {
        probes.push(rig.runtime.run_probe().expect("a live run"));
        rig.registry.pause(rig.handle).expect("pause");
        rig.registry.resume(rig.handle).expect("resume");
        assert_eq!(rig.status(), ModuleStatus::Running);
    }
    let live = probes.iter().filter(|alive| alive()).count();
    assert_eq!(
        live, 0,
        "the invalidation and cancellation listeners hold only weak run handles"
    );
    assert!(rig.runtime.run_probe().is_some_and(|alive| alive()));
    assert_eq!(rig.chord_down(), Disposition::Suppress);
    dismiss(&rig);
}

#[test]
fn ma_03_07_an_unacknowledged_tap_removal_is_reported_as_cleanup_pending() {
    let (rig, _app) = Rig::started(default_config());
    assert!(rig.runtime.health_detail().is_none());
    rig.tap.script(
        TapKind::Suppressing,
        ControlAction::Remove,
        FakeResponse::Fail("removal refused".into()),
    );
    let before = rig.runtime.count(Outcome::CleanupPending);
    rig.registry.pause(rig.handle).expect("pause");
    let detail = rig.runtime.health_detail().expect("pending detail after the stop");
    assert!(detail.contains("cleanupPending"), "{detail}");
    assert!(detail.contains("not acknowledged"), "{detail}");
    assert_eq!(rig.runtime.count(Outcome::CleanupPending), before + 1);
}
