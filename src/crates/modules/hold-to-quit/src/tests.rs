//! Runtime, policy, race and lifecycle tests for Hold-to-Quit.
//!
//! Every test runs over fakes: the shared input service over its fake tap, the
//! fake application backend, the fake HUD backend, manual executors and a fake
//! module clock. No test installs a tap, shows a panel, terminates an
//! application or prompts for a permission. Names carry the acceptance
//! criterion they prove (`hq_04_0N_`, `hq_05_0N_`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use stache_macos::hold_hud::testing::FakeHudCall;
use stache_macos::hold_hud::{HudAnchor, HudInstallError, HudMotion, HudShowFailure, PressId};
use stache_macos::{CANCELLATION_QUANTUM, GracefulOutcome};
use stache_sdk::input::{
    Chord, Claim, Disposition, ExemptionFlags, FrontmostApp, FrontmostState, InputEventKind,
    InputService, KeyCode, ModifierFlags, PressStream, ReservationBackend, TapKind,
};
use stache_sdk::{ErrorCategory, ModuleStatus, Operation, Permission, PermissionStatus};

use crate::config::{CommandQuitConfig, HudAnimation, HudPosition};
use crate::policy::{self, ClaimInputs, PassReason, decide, is_command_only};
use crate::runtime::IssueReport;
use crate::test_support::{App, Rig, config, config_with_dismiss, wait_until};

const HOLD: u64 = 1500;

fn hold_for(ms: u64) -> Duration { Duration::from_millis(ms) }

/// A started rig with an editor frontmost.
fn focused(hold: u64) -> (Rig, App) {
    let rig = Rig::started(config(hold));
    let editor = rig.add_app("com.example.editor");
    rig.focus(&editor);
    (rig, editor)
}

/// Presses `Q` and waits until the worker tracks the hold (parked for the
/// deadline).
fn begin_hold(rig: &Rig, hold: u64) {
    assert_eq!(rig.q_down(), Disposition::Suppress);
    rig.wait_parked_for("the worker to track the hold", |timeout| {
        timeout == Some(hold_for(hold))
    });
}

fn inputs<'a>(
    snapshot: &'a stache_macos::FrontmostSnapshot,
    context: &'a FrontmostState,
) -> ClaimInputs<'a> {
    ClaimInputs {
        accepting: true,
        hud_ready: true,
        armed: 3,
        observed: 3,
        snapshot,
        context,
    }
}

fn reports(rig: &Rig) -> Vec<IssueReport> { rig.runtime.reports() }

fn wait_report(rig: &Rig) -> IssueReport {
    wait_until("an issue report", || !reports(rig).is_empty());
    reports(rig).remove(0)
}

fn orders(rig: &Rig) -> usize { rig.hud_backend.order_count() }

fn hides(rig: &Rig) -> usize {
    rig.hud_backend
        .calls()
        .iter()
        .filter(|call| matches!(call, FakeHudCall::Hide { .. }))
        .count()
}

// ---------------------------------------------------------------------------
// HQ-04-02: the callback decision and physical pairing
// ---------------------------------------------------------------------------

#[test]
fn hq_04_02_the_filter_is_exact_command_keycode_twelve_plus_the_named_modifier_keys() {
    let filter = policy::filter();
    let key = |code: u16| PressStream::Key(KeyCode(code));
    let down = InputEventKind::KeyDown;
    assert!(filter.matches(down, key(12), ModifierFlags::COMMAND));
    // `Exact` ignores Caps Lock.
    assert!(filter.matches(down, key(12), ModifierFlags::COMMAND | ModifierFlags::CAPS_LOCK));
    for extra in [
        ModifierFlags::SHIFT,
        ModifierFlags::CONTROL,
        ModifierFlags::OPTION,
        ModifierFlags::FUNCTION,
    ] {
        assert!(!filter.matches(down, key(12), ModifierFlags::COMMAND | extra));
    }
    assert!(!filter.matches(down, key(12), ModifierFlags::NONE));
    assert!(
        !filter.matches(down, key(13), ModifierFlags::COMMAND),
        "only physical Q"
    );
    // A named modifier key always reaches the handler, so a release is seen.
    for code in policy::HOLD_KEYS {
        assert!(filter.matches(InputEventKind::FlagsChanged, key(code), ModifierFlags::NONE));
    }
    assert!(
        !filter.matches(InputEventKind::FlagsChanged, key(57), ModifierFlags::NONE),
        "Caps Lock"
    );
    assert_eq!(policy::HOLD_KEYS, [12, 54, 55, 56, 58, 59, 60, 61, 62]);
    assert!(is_command_only(
        ModifierFlags::COMMAND | ModifierFlags::CAPS_LOCK
    ));
    assert!(!is_command_only(
        ModifierFlags::COMMAND | ModifierFlags::FUNCTION
    ));
}

#[test]
fn hq_04_02_the_decision_table_fails_open_on_every_disagreement() {
    use stache_macos::{
        ApplicationBackend, ApplicationControl, FakeApplicationBackend, FrontmostSnapshot,
        FrontmostState as Native, ManualExecutor, Unconfirmed,
    };
    // A separate control only to mint an exact instance identifier.
    let backend = Arc::new(FakeApplicationBackend::new());
    let _ = backend.add_running("com.example.editor");
    let control = ApplicationControl::new(
        Arc::new(ManualExecutor::new(true)),
        Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
    );
    let observation =
        control.install(&stache_sdk::CancellationToken::new(), |_| {}).expect("install");
    let id = observation.running_applications("com.example.editor").expect("list")[0];
    let confirmed = FrontmostSnapshot {
        generation: 1,
        epoch: 7,
        state: Native::Confirmed,
        application: Some(id),
        bundle_identifier: Some(Arc::from("com.example.editor")),
        name: None,
        is_current_process: false,
        is_finder: false,
        pid: None,
    };
    let known = |bundle: &str, bits: u32| {
        FrontmostState::Known(FrontmostApp {
            bundle_id: Arc::from(bundle),
            exemptions: ExemptionFlags(bits),
        })
    };
    let agree = known("com.example.editor", 0);
    let target = decide(&inputs(&confirmed, &agree)).expect("all gates agree");
    assert_eq!((target.application, target.epoch), (id, 7));

    let pass = |snapshot: &FrontmostSnapshot, context: &FrontmostState| {
        decide(&inputs(snapshot, context)).err()
    };
    assert_eq!(
        pass(&confirmed, &known("com.example.other", 0)),
        Some(PassReason::BundleMismatch)
    );
    assert_eq!(
        pass(&confirmed, &known("com.example.editor", 1)),
        Some(PassReason::ContextExempt)
    );
    assert_eq!(
        pass(&confirmed, &FrontmostState::Stale),
        Some(PassReason::ContextNotKnown)
    );
    assert_eq!(
        pass(&confirmed, &FrontmostState::Unknown),
        Some(PassReason::ContextNotKnown)
    );
    let mut snapshot = confirmed.clone();
    snapshot.bundle_identifier = None;
    assert_eq!(pass(&snapshot, &agree), Some(PassReason::BundleMismatch));
    let mut snapshot = confirmed.clone();
    snapshot.is_finder = true;
    assert_eq!(
        pass(&snapshot, &agree),
        Some(PassReason::ApplicationNotEligible),
        "Finder"
    );
    let mut snapshot = confirmed.clone();
    snapshot.is_current_process = true;
    assert_eq!(
        pass(&snapshot, &agree),
        Some(PassReason::ApplicationNotEligible),
        "this process"
    );
    let mut snapshot = confirmed.clone();
    snapshot.state = Native::Unconfirmed(Unconfirmed::Invalidated);
    assert_eq!(
        pass(&snapshot, &agree),
        Some(PassReason::ApplicationNotEligible),
        "unconfirmed"
    );

    let mut gates = inputs(&confirmed, &agree);
    gates.accepting = false;
    assert_eq!(decide(&gates).err(), Some(PassReason::NotAccepting));
    gates.accepting = true;
    gates.hud_ready = false;
    assert_eq!(decide(&gates).err(), Some(PassReason::HudNotReady));
    gates.hud_ready = true;
    gates.observed = 4;
    assert_eq!(decide(&gates).err(), Some(PassReason::RevisionNotArmed));
    gates.armed = 0;
    gates.observed = 0;
    assert_eq!(decide(&gates).err(), Some(PassReason::RevisionNotArmed));
}

#[test]
fn hq_04_02_a_command_only_q_down_is_suppressed_and_the_up_is_paired() {
    let (rig, _editor) = focused(HOLD);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    assert_eq!(rig.input.ledger_len(), 1);
    assert_eq!(rig.q_repeat(), Disposition::Suppress);
    assert_eq!(rig.q_up(), Disposition::Suppress);
    assert_eq!(rig.input.ledger_len(), 0, "the up consumed the press");
}

#[test]
fn hq_04_02_the_modifier_matrix_passes_everything_but_command_only() {
    let (rig, _editor) = focused(HOLD);
    for extra in [
        ModifierFlags::SHIFT,
        ModifierFlags::CONTROL,
        ModifierFlags::OPTION,
        ModifierFlags::FUNCTION,
    ] {
        assert_eq!(
            rig.q_down_with(ModifierFlags::COMMAND | extra),
            Disposition::Pass,
            "{extra:?}"
        );
        assert_eq!(
            rig.raw(InputEventKind::KeyUp, 12, ModifierFlags::COMMAND | extra, false),
            Disposition::Pass
        );
    }
    assert_eq!(rig.q_down_with(ModifierFlags::NONE), Disposition::Pass);
    assert_eq!(
        rig.raw(InputEventKind::KeyUp, 12, ModifierFlags::NONE, false),
        Disposition::Pass
    );
    assert_eq!(rig.shows(), 0, "nothing was shown for a passed press");
    // Caps Lock is ignored by `Exact`.
    assert_eq!(
        rig.q_down_with(ModifierFlags::COMMAND | ModifierFlags::CAPS_LOCK),
        Disposition::Suppress
    );
}

#[test]
fn hq_04_02_a_repeat_never_starts_a_press() {
    let (rig, _editor) = focused(HOLD);
    assert_eq!(rig.q_repeat(), Disposition::Pass, "an orphan repeat passes");
    assert_eq!(rig.input.ledger_len(), 0);
    assert_eq!(rig.shows(), 0);
}

#[test]
fn hq_04_02_finder_self_and_unknown_frontmost_pass_without_a_hud() {
    let rig = Rig::started(config(HOLD));
    let finder = {
        let mut app = rig.add_app("com.apple.finder");
        app.exemptions = ExemptionFlags(1);
        app
    };
    rig.focus(&finder);
    assert_eq!(rig.q_down(), Disposition::Pass, "Finder");
    let _ = rig.q_up();

    let own = rig.add_app("com.example.stache");
    rig.apps.set_current_process(Some(own.key));
    let mut own_app = own;
    own_app.exemptions = ExemptionFlags(2);
    rig.focus(&own_app);
    assert_eq!(rig.q_down(), Disposition::Pass, "this process");
    let _ = rig.q_up();

    // Unknown and stale SDK metadata.
    let editor = rig.add_app("com.example.editor");
    rig.focus(&editor);
    rig.input.context_publisher().mark_unknown();
    assert_eq!(rig.q_down(), Disposition::Pass, "unknown context");
    let _ = rig.q_up();
    rig.input.context_publisher().mark_stale();
    assert_eq!(rig.q_down(), Disposition::Pass, "stale context");
    let _ = rig.q_up();
    assert_eq!(rig.shows(), 0);
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_04_02_both_frontmost_gates_must_agree_on_the_bundle() {
    let (rig, _editor) = focused(HOLD);
    // The SDK context names another bundle than the module's snapshot.
    rig.input.context_publisher().publish_frontmost(FrontmostApp {
        bundle_id: Arc::from("com.example.other"),
        exemptions: ExemptionFlags(0),
    });
    assert_eq!(rig.q_down(), Disposition::Pass);
    let _ = rig.q_up();
    // An exemption bit on the SDK context also fails open.
    rig.input.context_publisher().publish_frontmost(FrontmostApp {
        bundle_id: Arc::from("com.example.editor"),
        exemptions: ExemptionFlags(1 << 4),
    });
    assert_eq!(rig.q_down(), Disposition::Pass);
    let _ = rig.q_up();
    assert_eq!(rig.shows(), 0);
}

#[test]
fn hq_04_02_the_exact_instance_decides_self_not_the_bundle() {
    let rig = Rig::started(config(HOLD));
    let own = rig.add_app("com.example.stache");
    let other = rig.add_app("com.example.stache");
    rig.apps.set_current_process(Some(own.key));
    // Another instance with the same bundle identifier is not this process.
    rig.focus(&other);
    assert_eq!(
        rig.q_down(),
        Disposition::Suppress,
        "another `stache` is eligible"
    );
    let _ = rig.q_up();
    wait_until("the other instance's hold to end", || {
        rig.input.ledger_len() == 0
    });
    let mut own = own;
    own.exemptions = ExemptionFlags(2);
    rig.focus(&own);
    assert_eq!(
        rig.q_down(),
        Disposition::Pass,
        "the exact current process is exempt"
    );
}

#[test]
fn hq_04_02_a_passed_press_that_becomes_eligible_stays_passed() {
    let rig = Rig::started(config(HOLD));
    let finder = {
        let mut app = rig.add_app("com.apple.finder");
        app.exemptions = ExemptionFlags(1);
        app
    };
    let editor = rig.add_app("com.example.editor");
    rig.focus(&finder);
    assert_eq!(rig.q_down(), Disposition::Pass);
    rig.focus(&editor);
    assert_eq!(
        rig.q_repeat(),
        Disposition::Pass,
        "no suppressor is acquired mid-press"
    );
    assert_eq!(rig.q_up(), Disposition::Pass);
    assert_eq!(rig.shows(), 0);
}

#[test]
fn hq_04_02_two_presses_in_one_epoch_get_distinct_hud_press_ids() {
    let (rig, _editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    assert_eq!(rig.q_up(), Disposition::Suppress);
    wait_until("the first hold to end", || hides(&rig) == 1);
    rig.wait_parked();
    begin_hold(&rig, HOLD);
    assert_eq!(rig.q_up(), Disposition::Suppress);
    wait_until("the second hold to end", || hides(&rig) == 2);
    let shows: Vec<_> = rig
        .hud_backend
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            FakeHudCall::Show { press, .. } => Some(press),
            _ => None,
        })
        .collect();
    assert_eq!(shows.len(), 2);
    assert_ne!(shows[0], shows[1], "a fresh PressId per physical press");
    assert!(shows[1].raw() > shows[0].raw(), "monotonic");
    assert_eq!(rig.input.epoch().0, 0, "both presses ran in one input epoch");
}

#[test]
fn hq_04_02_command_release_before_the_q_up_cancels_and_the_rest_stays_swallowed() {
    let (rig, _editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    assert_eq!(rig.command_flags(ModifierFlags::NONE), Disposition::Pass);
    wait_until("the HUD to hide", || hides(&rig) == 1);
    assert_eq!(
        rig.q_repeat(),
        Disposition::Suppress,
        "the rest of the press is swallowed"
    );
    assert_eq!(rig.q_up(), Disposition::Suppress);
    rig.clock.advance(hold_for(HOLD * 2));
    assert_eq!(rig.apps.graceful_attempts(), 0);
    assert!(reports(&rig).is_empty());
}

#[test]
fn hq_04_02_an_added_modifier_mid_hold_cancels_it() {
    let (rig, _editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    assert_eq!(
        rig.raw(
            InputEventKind::FlagsChanged,
            56,
            ModifierFlags::COMMAND | ModifierFlags::SHIFT,
            false
        ),
        Disposition::Pass
    );
    wait_until("the HUD to hide", || hides(&rig) == 1);
    // A repeat whose flags no longer match is still the same physical press.
    assert_eq!(
        rig.raw(
            InputEventKind::KeyDown,
            12,
            ModifierFlags::COMMAND | ModifierFlags::SHIFT,
            true
        ),
        Disposition::Suppress
    );
    assert_eq!(rig.q_up(), Disposition::Suppress);
    rig.clock.advance(hold_for(HOLD * 2));
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_04_02_a_fast_down_up_before_the_worker_runs_shows_nothing() {
    let (rig, editor) = focused(HOLD);
    // Hold the worker inside its first press so the second queues behind it.
    let barrier = rig.apps.block_next_is_running();
    assert_eq!(rig.q_down(), Disposition::Suppress);
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    assert_eq!(rig.q_up(), Disposition::Suppress);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    assert_eq!(rig.q_up(), Disposition::Suppress);
    let _ = barrier.release();
    rig.wait_parked();
    wait_until("the worker to settle", || rig.clock.is_parked());
    assert_eq!(
        rig.shows(),
        0,
        "both presses were released before they could show"
    );
    assert_eq!(orders(&rig), 0);
    let _ = editor;
}

#[test]
fn hq_04_02_a_full_inbox_fails_open_for_a_new_down_and_never_loses_a_cancel() {
    let (rig, _editor) = focused(HOLD);
    // Phase one: the worker is busy, so the inbox fills (one press in flight and
    // eight queued) and the tenth down passes. The release of a passed press
    // passes too.
    let barrier = rig.apps.block_next_is_running();
    assert_eq!(rig.q_down(), Disposition::Suppress);
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    for _ in 0..8 {
        assert_eq!(rig.q_down(), Disposition::Suppress);
    }
    assert_eq!(rig.q_down(), Disposition::Pass, "a full inbox passes a new down");
    assert_eq!(rig.q_up(), Disposition::Pass);
    let _ = barrier.release();
    wait_until("the worker to drain", || rig.clock.is_parked());
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
    rig.clock.advance(hold_for(HOLD * 4));
    assert_eq!(rig.shows(), 0, "every queued press was superseded and cancelled");
    assert_eq!(rig.apps.graceful_attempts(), 0);

    // Phase two: a release while the inbox is full still cancels, because the
    // cancel is an atomic mark and not a queued message.
    let barrier = rig.apps.block_next_is_running();
    assert_eq!(rig.q_down(), Disposition::Suppress);
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    for _ in 0..8 {
        assert_eq!(rig.q_down(), Disposition::Suppress);
    }
    assert_eq!(
        rig.q_up(),
        Disposition::Suppress,
        "the claimed press stays paired"
    );
    let _ = barrier.release();
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
    rig.clock.advance(hold_for(HOLD * 4));
    assert_eq!(rig.shows(), 0, "the release reached the press");
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_04_02_a_slow_callback_goes_inert_but_leaves_the_press_tombstoned() {
    let (rig, _editor) = focused(HOLD);
    let service_clock = Arc::clone(&rig.input_clock);
    rig.clock.set_on_now(Some(Arc::new(move || {
        service_clock.advance(Duration::from_millis(50));
    })));
    assert_eq!(rig.q_down(), Disposition::Suppress, "the claim itself stands");
    rig.clock.set_on_now(None);
    assert_eq!(rig.q_repeat(), Disposition::Suppress);
    assert_eq!(
        rig.q_up(),
        Disposition::Suppress,
        "the tombstone swallows the up"
    );
    assert_eq!(rig.input.ledger_len(), 0);
    // The handler is inert now: a new down passes.
    assert_eq!(rig.q_down(), Disposition::Pass);
    let _ = rig.q_up();
}

#[test]
fn hq_04_02_a_panicking_callback_fails_open_and_goes_inert() {
    let (rig, _editor) = focused(HOLD);
    rig.clock.set_on_now(Some(Arc::new(|| panic!("boom"))));
    assert_eq!(
        rig.q_down(),
        Disposition::Pass,
        "the router contains the panic and fails open"
    );
    rig.clock.set_on_now(None);
    let _ = rig.q_up();
    assert_eq!(rig.q_down(), Disposition::Pass, "the handler stays inert");
    assert_eq!(rig.input.counters().panics, 1);
    assert_eq!(rig.shows(), 0);
}

#[test]
fn hq_04_02_a_tap_disable_cancels_the_press_and_orphans_pass() {
    let (rig, _editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    rig.input.tap_disabled(TapKind::Suppressing);
    wait_until("the HUD to hide", || hides(&rig) == 1);
    // The shipped ledger drained: the remaining repeats and up pass through.
    assert_eq!(rig.q_repeat(), Disposition::Pass);
    assert_eq!(rig.q_up(), Disposition::Pass);
    rig.clock.advance(hold_for(HOLD * 2));
    assert_eq!(rig.apps.graceful_attempts(), 0, "an old hold is never resumed");
}

// ---------------------------------------------------------------------------
// HQ-04-03: the hold, the deadline and issuance
// ---------------------------------------------------------------------------

#[test]
fn hq_04_03_a_full_hold_asks_the_captured_instance_to_quit_gracefully_once() {
    let (rig, editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    assert_eq!(orders(&rig), 1, "only the matching Shown arms the hold");
    assert_eq!(rig.apps.graceful_attempts(), 0, "nothing before the deadline");
    rig.clock.advance(hold_for(HOLD));
    let report = wait_report(&rig);
    assert_eq!(report.outcome, GracefulOutcome::Requested);
    assert_eq!(rig.apps.graceful_requests(), vec![editor.key]);
    assert_eq!(rig.apps.terminate_attempts(), 0, "never `forceTerminate`");
    assert_eq!(hides(&rig), 1, "the HUD hides before the send");
    // The rest of the press stays paired and issues nothing more.
    assert_eq!(rig.q_repeat(), Disposition::Suppress);
    assert_eq!(rig.q_up(), Disposition::Suppress);
    rig.clock.advance(hold_for(HOLD));
    assert_eq!(rig.apps.graceful_attempts(), 1, "at most once");
    assert_eq!(reports(&rig).len(), 1);
}

#[test]
fn hq_04_03_a_release_before_the_deadline_sends_nothing() {
    let (rig, _editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    rig.clock.advance(hold_for(HOLD - 1));
    assert_eq!(rig.q_up(), Disposition::Suppress);
    wait_until("the HUD to hide", || hides(&rig) == 1);
    rig.clock.advance(hold_for(10));
    assert_eq!(rig.apps.graceful_attempts(), 0);
    assert!(reports(&rig).is_empty());
}

#[test]
fn hq_04_03_a_release_delivered_at_the_deadline_wins_over_a_worker_that_has_not_run() {
    let (rig, _editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    // The deadline passes, but the worker is not scheduled yet; the release is
    // delivered at that instant and reaches the press first.
    rig.clock.advance_quietly(hold_for(HOLD));
    assert_eq!(rig.q_up(), Disposition::Suppress);
    rig.clock.wake();
    wait_until("the HUD to hide", || hides(&rig) == 1);
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_04_03_a_release_after_the_deadline_but_before_the_commit_prevents_the_send() {
    let (rig, _editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    let barrier = rig.apps.block_next_graceful_before_commit();
    rig.clock.advance(hold_for(HOLD));
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    assert_eq!(
        rig.q_up(),
        Disposition::Suppress,
        "the release reaches the one-shot authorization"
    );
    let _ = barrier.release();
    let report = wait_report(&rig);
    assert_eq!(report.outcome, GracefulOutcome::Cancelled);
    assert!(rig.apps.graceful_requests().is_empty());
}

#[test]
fn hq_04_03_a_release_after_the_commit_cannot_undo_the_request() {
    let (rig, editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    let barrier = rig.apps.block_next_graceful_after_issue();
    rig.clock.advance(hold_for(HOLD));
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    assert_eq!(rig.q_up(), Disposition::Suppress);
    let _ = barrier.release();
    let report = wait_report(&rig);
    assert_eq!(report.outcome, GracefulOutcome::Requested);
    assert_eq!(rig.apps.graceful_requests(), vec![editor.key]);
}

#[test]
fn hq_04_03_a_refusal_is_reported_as_refused_and_never_escalated() {
    let (rig, _editor) = focused(HOLD);
    rig.apps.refuse_graceful_terminate();
    begin_hold(&rig, HOLD);
    rig.clock.advance(hold_for(HOLD));
    assert_eq!(wait_report(&rig).outcome, GracefulOutcome::Refused);
    assert_eq!(rig.apps.terminate_attempts(), 0, "no forceTerminate fallback");
    assert_eq!(rig.apps.graceful_attempts(), 1, "no retry");
}

#[test]
fn hq_04_03_an_unacknowledged_send_is_uncertain_never_refused() {
    let rig = Rig::pumped(config(HOLD), Duration::from_millis(50));
    rig.start().expect("start");
    rig.wait_parked();
    let editor = rig.add_app("com.example.editor");
    rig.focus(&editor);
    begin_hold(&rig, HOLD);
    let barrier = rig.apps.block_next_graceful_after_issue();
    rig.clock.advance(hold_for(HOLD));
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    // The control's acknowledgement budget (50 ms) elapses while the send is
    // parked after it committed.
    let report = wait_report(&rig);
    assert!(
        matches!(report.outcome, GracefulOutcome::Uncertain(_)),
        "{:?}",
        report.outcome
    );
    let _ = barrier.release();
}

#[test]
fn hq_04_03_zero_duration_proceeds_only_after_a_matching_shown() {
    let (rig, editor) = focused(0);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    assert_eq!(wait_report(&rig).outcome, GracefulOutcome::Requested);
    assert_eq!(rig.apps.graceful_requests(), vec![editor.key]);
    assert_eq!(orders(&rig), 1, "the HUD was shown before the send");
}

#[test]
fn hq_04_03_a_zero_duration_with_a_pending_show_waits_for_the_acknowledgement() {
    let (rig, _editor) = focused(0);
    // New show jobs queue instead of running.
    rig.hud_executor.set_on_main(false);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    rig.wait_parked_for("the pending show", |timeout| {
        timeout == Some(CANCELLATION_QUANTUM)
    });
    assert_eq!(
        rig.apps.graceful_attempts(),
        0,
        "no terminate without a matching Shown"
    );
    rig.hud_executor.set_on_main(true);
    assert_eq!(rig.hud_executor.drain(), 1);
    rig.clock.advance(CANCELLATION_QUANTUM);
    assert_eq!(wait_report(&rig).outcome, GracefulOutcome::Requested);
}

#[test]
fn hq_04_03_a_show_pending_at_the_deadline_never_issues_and_a_late_shown_is_ignored_when_cancelled()
{
    let (rig, _editor) = focused(100);
    rig.hud_executor.set_on_main(false);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    rig.wait_parked_for("the pending show", |timeout| {
        timeout == Some(CANCELLATION_QUANTUM)
    });
    rig.clock.advance(hold_for(500));
    rig.wait_parked_for("the pending show again", |timeout| {
        timeout == Some(CANCELLATION_QUANTUM)
    });
    assert_eq!(
        rig.apps.graceful_attempts(),
        0,
        "the deadline alone never issues"
    );
    // Cancel the press, then let the queued show run: it must not order front.
    assert_eq!(rig.q_up(), Disposition::Suppress);
    rig.clock.advance(CANCELLATION_QUANTUM);
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
    rig.hud_executor.set_on_main(true);
    rig.hud_executor.drain();
    assert_eq!(orders(&rig), 0, "a late show for a dead press is never ordered");
    assert_eq!(hides(&rig), 1, "the dead press was hidden");
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_04_03_a_pending_show_expires_only_by_draining_hud_events_then_degrades() {
    let (rig, _editor) = focused(HOLD);
    rig.hud_executor.set_on_main(false);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    rig.wait_parked_for("the pending show", |timeout| {
        timeout == Some(CANCELLATION_QUANTUM)
    });
    // The HUD acknowledgement budget (50 ms) expires in real time; only the
    // worker's drain observes it.
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        rig.runtime.degraded_detail().is_none(),
        "nothing observed it yet"
    );
    rig.clock.advance(CANCELLATION_QUANTUM);
    wait_until("the degraded detail", || rig.runtime.degraded_detail().is_some());
    assert!(rig.runtime.degraded_detail().is_some_and(|detail| detail.contains("TimedOut")));
    assert_eq!(rig.apps.graceful_attempts(), 0);
    assert_eq!(
        rig.status(),
        ModuleStatus::Running,
        "degraded mode stays Running"
    );
}

#[test]
fn hq_04_03_a_failed_show_cancels_the_press_and_enters_degraded_pass_through() {
    let (rig, _editor) = focused(HOLD);
    rig.hud_backend.fail_next_show(HudShowFailure::Native("boom".to_owned()));
    assert_eq!(rig.q_down(), Disposition::Suppress);
    wait_until("the degraded detail", || rig.runtime.degraded_detail().is_some());
    assert_eq!(rig.status(), ModuleStatus::Running);
    // The subscription is gone: the swallowed press stays paired, new presses pass.
    assert_eq!(rig.q_up(), Disposition::Suppress);
    assert_eq!(rig.q_down(), Disposition::Pass);
    let _ = rig.q_up();
    assert_eq!(rig.apps.graceful_attempts(), 0);
    let warnings = rig.logger.warnings();
    assert_eq!(
        warnings.iter().filter(|message| message.contains("passes through")).count(),
        1,
        "one warning: {warnings:?}"
    );
}

#[test]
fn hq_04_03_a_u64_max_duration_is_a_cancellable_indefinite_wait() {
    let (rig, _editor) = focused(u64::MAX);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    wait_until("the HUD to be shown", || orders(&rig) == 1);
    rig.wait_parked_for("the hold wait", |timeout| {
        timeout.is_none_or(|wait| wait > hold_for(10_000_000))
    });
    assert_eq!(rig.apps.graceful_attempts(), 0);
    assert_eq!(rig.q_up(), Disposition::Suppress);
    wait_until("the HUD to hide", || hides(&rig) == 1);
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_04_03_deadline_arithmetic_is_checked() {
    let now = std::time::Instant::now();
    assert_eq!(policy::deadline(now, 0), Some(now));
    assert_eq!(policy::deadline(now, 1500), now.checked_add(hold_for(1500)));
    assert_eq!(
        policy::deadline(now, u64::MAX),
        now.checked_add(Duration::from_millis(u64::MAX)),
        "no overflow panic"
    );
}

// ---------------------------------------------------------------------------
// HQ-04-04: races with frontmost, reconfigure and lifecycle
// ---------------------------------------------------------------------------

#[test]
fn hq_04_04_a_focus_change_cancels_and_never_quits_either_application() {
    let (rig, _editor) = focused(HOLD);
    let other = rig.add_app("com.example.other");
    begin_hold(&rig, HOLD);
    rig.focus(&other);
    wait_until("the HUD to hide", || hides(&rig) == 1);
    rig.clock.advance(hold_for(HOLD * 2));
    assert_eq!(
        rig.apps.graceful_attempts(),
        0,
        "neither the captured nor the current app"
    );
    assert_eq!(rig.q_up(), Disposition::Suppress, "the press stays paired");
}

#[test]
fn hq_04_04_a_burst_a_to_b_to_a_still_cancels() {
    let (rig, editor) = focused(HOLD);
    let other = rig.add_app("com.example.other");
    begin_hold(&rig, HOLD);
    rig.focus(&other);
    rig.focus(&editor);
    wait_until("the HUD to hide", || hides(&rig) == 1);
    rig.clock.advance(hold_for(HOLD * 2));
    assert_eq!(
        rig.apps.graceful_attempts(),
        0,
        "the epoch moved even though A is frontmost again"
    );
}

#[test]
fn hq_04_04_a_focus_change_before_the_notification_is_caught_by_the_main_preflight() {
    let (rig, _editor) = focused(HOLD);
    let other = rig.add_app("com.example.other");
    begin_hold(&rig, HOLD);
    // The OS changes focus but the notification has not been ingested yet.
    rig.apps.set_frontmost(Some(other.key));
    rig.clock.advance(hold_for(HOLD));
    let report = wait_report(&rig);
    assert!(
        matches!(report.outcome, GracefulOutcome::Rejected(_)),
        "{:?}",
        report.outcome
    );
    assert!(
        rig.apps.graceful_requests().is_empty(),
        "the replacement is never quit"
    );
}

#[test]
fn hq_04_04_an_exempt_application_at_show_time_is_never_shown() {
    let (rig, _editor) = focused(HOLD);
    let finder = rig.add_app("com.apple.finder");
    // Focus moved to Finder at the OS level before the show preflight.
    rig.apps.set_frontmost(Some(finder.key));
    assert_eq!(
        rig.q_down(),
        Disposition::Suppress,
        "the callback still saw the old snapshot"
    );
    wait_until("the worker to settle", || rig.clock.is_parked());
    rig.wait_parked();
    assert_eq!(rig.shows(), 0);
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_04_04_an_exempt_application_at_send_time_is_rejected() {
    let (rig, _editor) = focused(HOLD);
    let finder = rig.add_app("com.apple.finder");
    begin_hold(&rig, HOLD);
    rig.apps.set_frontmost(Some(finder.key));
    rig.clock.advance(hold_for(HOLD));
    assert!(matches!(wait_report(&rig).outcome, GracefulOutcome::Rejected(_)));
    assert!(rig.apps.graceful_requests().is_empty());
}

#[test]
fn hq_04_04_a_captured_application_that_exits_is_not_replaced() {
    let (rig, editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    rig.apps.terminate(editor.key);
    let replacement = rig.add_app("com.example.editor");
    rig.apps.set_frontmost(Some(replacement.key));
    rig.clock.advance(hold_for(HOLD));
    let report = wait_report(&rig);
    assert!(report.outcome.proves_no_send(), "{:?}", report.outcome);
    assert!(
        rig.apps.graceful_requests().is_empty(),
        "the replacement is never quit"
    );
}

#[test]
fn hq_04_04_reconfigure_cancels_the_press_and_rearms_at_the_new_value() {
    let (rig, editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    let report = rig.registry.reload(rig.handle, config(300)).expect("reload");
    assert!(report.start_error.is_none());
    wait_until("the HUD to hide", || hides(&rig) == 1);
    rig.clock.advance(hold_for(HOLD));
    assert_eq!(rig.apps.graceful_attempts(), 0, "the old press never revives");
    assert_eq!(
        rig.q_up(),
        Disposition::Suppress,
        "the old press stays swallowed"
    );
    // The next press uses the new, accepted value.
    wait_until("the runtime to re-arm", || {
        let disposition = rig.q_down();
        let armed = disposition == Disposition::Suppress;
        let _ = rig.q_up();
        armed
    });
    wait_until("a fresh press to track", || {
        rig.q_down() == Disposition::Suppress && {
            rig.wait_parked();
            rig.clock.parked_timeout() == Some(Some(hold_for(300)))
        }
    });
    rig.clock.advance(hold_for(300));
    assert_eq!(wait_report(&rig).outcome, GracefulOutcome::Requested);
    assert_eq!(rig.apps.graceful_requests(), vec![editor.key]);
}

#[test]
fn hq_04_04_new_presses_pass_between_reconfigure_and_the_matching_publication() {
    let (rig, _editor) = focused(HOLD);
    let during = Arc::new(std::sync::Mutex::new(None));
    let probe = Arc::clone(&rig.input);
    let slot = Arc::clone(&during);
    // `reconfigure` returns before the registry publishes: in that window the
    // runtime is not accepting, so a press passes.
    rig.runtime.set_reconfigure_hook(Some(Arc::new(move || {
        let down = probe.dispatch(TapKind::Suppressing, stache_sdk::input::RawInput {
            kind: InputEventKind::KeyDown,
            stream: PressStream::Key(KeyCode(12)),
            flags: ModifierFlags::COMMAND,
            repeat: false,
            timestamp: Duration::ZERO,
            location: None,
        });
        *slot.lock().expect("slot") = Some(down);
    })));
    rig.registry.reload(rig.handle, config(300)).expect("reload");
    assert_eq!(*during.lock().expect("slot"), Some(Disposition::Pass));
    rig.runtime.set_reconfigure_hook(None);
    let _ = rig.q_up();
    // Once the publication is adopted, new presses arm again.
    wait_until("the runtime to re-arm", || {
        let down = rig.q_down();
        let _ = rig.q_up();
        down == Disposition::Suppress
    });
}

#[test]
fn hq_04_04_an_older_adoption_cannot_reopen_acceptance_while_a_newer_candidate_is_staged() {
    use std::sync::atomic::{AtomicBool, Ordering as O};
    let (rig, _editor) = focused(HOLD);
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let applied = Arc::new(AtomicBool::new(false));
    let (e, r, a) = (Arc::clone(&entered), Arc::clone(&release), Arc::clone(&applied));
    rig.runtime.set_adopt_hook(Some(Arc::new(move |after| {
        if after {
            a.store(true, O::SeqCst);
        } else {
            e.store(true, O::SeqCst);
            while !r.load(O::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    })));
    // S1 is published; the worker reads it and stops just before applying it.
    rig.registry.reload(rig.handle, config(300)).expect("reload S1");
    wait_until("the worker to reach adopt", || entered.load(O::SeqCst));
    let during = Arc::new(std::sync::Mutex::new(None));
    let probe = Arc::clone(&rig.input);
    let slot = Arc::clone(&during);
    let (r, a) = (Arc::clone(&release), Arc::clone(&applied));
    // S2 is staged and unpublished while the stale adoption completes.
    rig.runtime.set_reconfigure_hook(Some(Arc::new(move || {
        r.store(true, O::SeqCst);
        while !a.load(O::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let down = probe.dispatch(TapKind::Suppressing, stache_sdk::input::RawInput {
            kind: InputEventKind::KeyDown,
            stream: PressStream::Key(KeyCode(12)),
            flags: ModifierFlags::COMMAND,
            repeat: false,
            timestamp: Duration::ZERO,
            location: None,
        });
        *slot.lock().expect("slot") = Some(down);
    })));
    rig.registry.reload(rig.handle, config(400)).expect("reload S2");
    rig.runtime.set_reconfigure_hook(None);
    rig.runtime.set_adopt_hook(None);
    assert_eq!(
        *during.lock().expect("slot"),
        Some(Disposition::Pass),
        "no press is claimed under S1 while S2 is staged"
    );
    let _ = rig.q_up();
    // S2 then arms normally.
    wait_until("the runtime to re-arm at S2", || {
        if rig.q_down() == Disposition::Suppress {
            rig.wait_parked();
            let armed = rig.clock.parked_timeout() == Some(Some(hold_for(400)));
            let _ = rig.q_up();
            wait_until("the press to end", || rig.input.ledger_len() == 0);
            rig.wait_parked();
            armed
        } else {
            false
        }
    });
}

#[test]
fn hq_04_04_an_unchanged_reload_is_a_no_op_and_keeps_the_press() {
    let (rig, editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    let report = rig.registry.reload(rig.handle, config(HOLD)).expect("unchanged reload");
    assert!(report.start_error.is_none());
    assert_eq!(hides(&rig), 0, "the in-flight press was not cancelled");
    rig.clock.advance(hold_for(HOLD));
    assert_eq!(wait_report(&rig).outcome, GracefulOutcome::Requested);
    assert_eq!(rig.apps.graceful_requests(), vec![editor.key]);
}

#[test]
fn hq_04_04_an_accept_timeout_rejects_the_candidate_and_preserves_the_old_state() {
    let (rig, editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    // A stalled native send holds the commit gate, so `accept` cannot be
    // observed inside its budget.
    let barrier = rig.apps.block_next_graceful_issuance();
    rig.clock.advance(hold_for(HOLD));
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    let result = rig.registry.reload(rig.handle, config(10));
    assert!(result.is_err(), "the candidate is rejected: {result:?}");
    let _ = barrier.release();
    assert_eq!(wait_report(&rig).outcome, GracefulOutcome::Requested);
    assert_eq!(rig.apps.graceful_requests(), vec![editor.key]);
    // The old value stays accepted and armed.
    let accepted = rig.registry.accepted_watch(rig.handle).expect("watch").read().expect("pair");
    assert_eq!(accepted.config.hold_duration, HOLD);
    assert_eq!(rig.status(), ModuleStatus::Running);
}

#[test]
fn hq_04_04_rapid_accepted_changes_arm_only_the_last_value() {
    let (rig, _editor) = focused(HOLD);
    for hold in [200_u64, 300, 400, 500] {
        rig.registry.reload(rig.handle, config(hold)).expect("reload");
    }
    wait_until("the last value to arm", || {
        if rig.q_down() == Disposition::Suppress {
            rig.wait_parked();
            let armed = rig.clock.parked_timeout() == Some(Some(hold_for(500)));
            let _ = rig.q_up();
            wait_until("the press to end", || rig.input.ledger_len() == 0);
            rig.wait_parked();
            armed
        } else {
            false
        }
    });
}

#[test]
fn hq_04_04_a_stop_with_a_staged_unpublished_candidate_is_clean() {
    let (rig, _editor) = focused(HOLD);
    let barrier = rig.apps.block_next_is_running();
    assert_eq!(rig.q_down(), Disposition::Suppress);
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    // The worker is busy, so the staged candidate is not adopted.
    rig.registry.reload(rig.handle, config(300)).expect("reload");
    let registry = Arc::clone(&rig.registry);
    let handle = rig.handle;
    let pause = std::thread::spawn(move || registry.pause(handle));
    wait_until("the HUD to release", || rig.hud_backend.release_count() == 1);
    let _ = barrier.release();
    pause.join().expect("pause thread").expect("pause");
    assert_eq!(rig.status(), ModuleStatus::Paused);
    assert_eq!(rig.apps.graceful_attempts(), 0);
    // Resume uses the new value.
    rig.registry.resume(rig.handle).expect("resume");
    rig.wait_parked();
    assert_eq!(
        rig.q_up(),
        Disposition::Suppress,
        "the old tombstone is consumed"
    );
    wait_until("the new run to arm", || rig.q_down() == Disposition::Suppress);
    rig.wait_parked_for("the new value", |timeout| timeout == Some(hold_for(300)));
}

#[test]
fn hq_04_04_late_results_of_a_stopped_run_cannot_touch_its_successor() {
    let (rig, _editor) = focused(HOLD);
    let barrier = rig.apps.block_next_is_running();
    assert_eq!(rig.q_down(), Disposition::Suppress);
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    let registry = Arc::clone(&rig.registry);
    let handle = rig.handle;
    let pause = std::thread::spawn(move || registry.pause(handle));
    wait_until("the HUD to release", || rig.hud_backend.release_count() == 1);
    let _ = barrier.release();
    pause.join().expect("pause").expect("pause");
    rig.registry.resume(rig.handle).expect("resume");
    rig.wait_parked();
    assert_eq!(rig.shows(), 0, "the stopped run never showed anything");
    assert!(rig.runtime.degraded_detail().is_none());
    assert!(reports(&rig).is_empty());
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

// ---------------------------------------------------------------------------
// HQ-04-05: stop, cleanup and idleness
// ---------------------------------------------------------------------------

#[test]
fn hq_04_05_an_idle_worker_never_polls() {
    let rig = Rig::started(config(HOLD));
    for _ in 0..5 {
        rig.clock.advance(Duration::from_secs(3600));
    }
    assert_eq!(
        rig.clock.waits(),
        vec![None],
        "one indefinite wait and nothing else"
    );
}

#[test]
fn hq_04_05_stop_cancels_before_the_join_and_cleans_up_while_the_worker_is_blocked() {
    let (rig, _editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    let barrier = rig.apps.block_next_graceful_before_commit();
    rig.clock.advance(hold_for(HOLD));
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    let registry = Arc::clone(&rig.registry);
    let handle = rig.handle;
    let pause = std::thread::spawn(move || registry.pause(handle));
    // Cleanup finished (HUD released, observation stopped, lease released) while
    // the registry is still joining the blocked worker.
    wait_until("the HUD to release", || rig.hud_backend.release_count() == 1);
    wait_until("the lease to release", || rig.leaser.held() == 0);
    assert!(!pause.is_finished(), "the join waits for the blocked worker");
    let _ = barrier.release();
    pause.join().expect("pause").expect("pause");
    assert_eq!(
        rig.apps.graceful_attempts(),
        1,
        "the blocked attempt ran to its commit"
    );
    assert!(
        rig.apps.graceful_requests().is_empty(),
        "stop cancelled the send before the join"
    );
}

#[test]
fn hq_04_05_stop_after_the_commit_reports_the_committed_request() {
    let (rig, editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    let barrier = rig.apps.block_next_graceful_after_issue();
    rig.clock.advance(hold_for(HOLD));
    assert!(barrier.wait_entered(Duration::from_secs(5)));
    let registry = Arc::clone(&rig.registry);
    let handle = rig.handle;
    let pause = std::thread::spawn(move || registry.pause(handle));
    wait_until("the lease to release", || rig.leaser.held() == 0);
    let _ = barrier.release();
    pause.join().expect("pause").expect("pause");
    assert_eq!(
        rig.apps.graceful_requests(),
        vec![editor.key],
        "a committed send may finish"
    );
}

#[test]
fn hq_04_05_a_main_queue_that_never_drains_fails_start_boundedly_and_rolls_back() {
    let rig = Rig::with(config(HOLD), Duration::from_millis(30), false);
    let started = std::time::Instant::now();
    let error = rig.start().expect_err("start cannot complete");
    assert!(started.elapsed() < Duration::from_secs(5), "bounded");
    let _ = error;
    assert!(
        matches!(rig.status(), ModuleStatus::Failed(_)),
        "{:?}",
        rig.status()
    );
    assert_eq!(rig.leaser.held(), 0, "the lease was rolled back");
    assert_eq!(rig.runtime.degraded_detail(), None);
    assert_eq!(rig.input.ledger_len(), 0);
    assert_eq!(rig.tap.submitted().len(), 0, "no subscription means no tap");
}

#[test]
fn hq_04_05_a_hud_release_timeout_is_retained_then_retry_after_the_drain_succeeds() {
    let (rig, _editor) = focused(HOLD);
    rig.hud_executor.set_on_main(false);
    let error = rig.registry.pause(rig.handle).expect_err("the HUD release is not acknowledged");
    assert!(error.to_string().contains("retained"), "{error}");
    assert!(rig.hud.has_retained());
    assert!(matches!(rig.status(), ModuleStatus::Failed(_)));
    // A retained HUD blocks a second panel, and a repeated stop never reports
    // success on a timeout.
    let created = rig.hud_backend.panels_created();
    assert!(rig.hud.create(&stache_sdk::CancellationToken::new()).is_err());
    assert!(
        rig.registry.pause(rig.handle).is_err(),
        "Retry stays truthful while undrained"
    );
    assert_eq!(rig.hud_backend.panels_created(), created, "no second panel");
    // The main queue finally drains; the retried release is acknowledged.
    rig.hud_executor.set_on_main(true);
    rig.hud_executor.drain();
    rig.registry.pause(rig.handle).expect("the retry after the drain succeeds");
    assert!(!rig.hud.has_retained());
    assert_eq!(rig.hud_backend.panels_live(), 0);
    rig.registry.resume(rig.handle).expect("resume");
    rig.wait_parked();
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(
        rig.hud_backend.panels_created(),
        created + 1,
        "exactly one successor panel"
    );
}

#[test]
fn hq_04_05_an_observer_removal_failure_is_retained_and_retried_without_duplicates() {
    let (rig, _editor) = focused(HOLD);
    rig.apps.fail_next_unobserve();
    let error = rig.registry.pause(rig.handle).expect_err("the removal is not acknowledged");
    assert!(error.to_string().contains("observation"), "{error}");
    assert_eq!(rig.leaser.held(), 0, "the independent lease was still released");
    assert_eq!(rig.hud_backend.release_count(), 1, "the HUD was released once");
    assert!(
        rig.apps.activation_observers() <= 1,
        "never a second observer while one is owned"
    );
    // Resume first retries the owed stop (idempotently), then starts exactly one
    // successor.
    rig.registry.resume(rig.handle).expect("retry and resume");
    assert_eq!(
        rig.hud_backend.release_count(),
        1,
        "the retry touched only the owed owner"
    );
    assert_eq!(rig.apps.activation_observers(), 1);
    rig.wait_parked();
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(rig.leaser.held(), 1);
}

// ---------------------------------------------------------------------------
// HQ-04-01: lifecycle, permissions and failure
// ---------------------------------------------------------------------------

#[test]
fn hq_04_01_construction_registration_and_a_disabled_start_are_inert() {
    let rig = Rig::new(CommandQuitConfig::default());
    rig.start().expect("start");
    assert_eq!(rig.status(), ModuleStatus::Disabled);
    assert_eq!(rig.leaser.leases(), 0);
    assert_eq!(rig.apps.observations(), 0);
    assert_eq!(rig.apps.activation_observers(), 0);
    assert_eq!(rig.hud_backend.panels_created(), 0);
    assert!(rig.tap.submitted().is_empty(), "no tap exists while disabled");
    assert!(!rig.clock.is_parked(), "no worker exists while disabled");
}

#[test]
fn hq_04_01_permissions_are_user_only_and_required_only_while_enabled() {
    let rig = Rig::new(config(HOLD));
    let requirements = rig.registry.permission_requirements();
    assert_eq!(requirements.len(), 1);
    assert_eq!(requirements[0].permission(), Permission::Accessibility);
    assert!(
        !requirements[0].may_auto_prompt(),
        "only the tray request prompts"
    );
    let disabled = Rig::new(CommandQuitConfig::default());
    assert!(disabled.registry.permission_requirements().is_empty());
}

#[test]
fn hq_04_01_a_denied_permission_waits_without_starting_anything_or_prompting() {
    let rig = Rig::new(config(HOLD));
    rig.trust.set(false);
    rig.start().expect("start");
    assert_eq!(
        rig.status(),
        ModuleStatus::WaitingForPermission(Permission::Accessibility)
    );
    assert_eq!(rig.trust.requests(), 0, "automatic triggers never prompt");
    assert_eq!(rig.leaser.leases(), 0);
    assert_eq!(rig.hud_backend.panels_created(), 0);
    // Granting starts it through the generic recovery path.
    rig.trust.set(true);
    let _ = rig
        .registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Granted);
    wait_until("the module to run", || rig.status() == ModuleStatus::Running);
    // Revocation stops it and leaves nothing behind.
    rig.trust.set(false);
    let _ = rig
        .registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Denied);
    assert_eq!(
        rig.status(),
        ModuleStatus::WaitingForPermission(Permission::Accessibility)
    );
    assert_eq!(rig.leaser.held(), 0);
    assert_eq!(rig.hud_backend.release_count(), 1);
}

#[test]
fn hq_04_01_pause_and_resume_release_and_reacquire_everything() {
    let (rig, _editor) = focused(HOLD);
    assert_eq!(rig.leaser.held(), 1);
    rig.registry.pause(rig.handle).expect("pause");
    assert_eq!(rig.status(), ModuleStatus::Paused);
    assert_eq!(rig.leaser.held(), 0);
    assert_eq!(rig.apps.activation_observers(), 0);
    assert_eq!(rig.hud_backend.panels_live(), 0);
    assert_eq!(rig.q_down(), Disposition::Pass, "paused means no subscription");
    let _ = rig.q_up();
    rig.registry.resume(rig.handle).expect("resume");
    rig.wait_parked();
    assert_eq!(rig.leaser.held(), 1);
    assert_eq!(rig.hud_backend.panels_live(), 1);
    assert_eq!(rig.status(), ModuleStatus::Running);
}

fn assert_start_failure(rig: &Rig) {
    let status = rig.status();
    let ModuleStatus::Failed(failure) = status else {
        panic!("expected Failed(Start), got {status:?}");
    };
    assert_eq!(failure.operation, Operation::Start);
    assert_eq!(rig.leaser.held(), 0, "the lease was rolled back");
    assert_eq!(rig.apps.activation_observers(), 0, "the watch was rolled back");
    assert_eq!(rig.hud_backend.panels_live(), 0, "no panel survives");
    assert!(rig.tap.submitted().is_empty(), "no subscription, no tap");
    assert_eq!(rig.q_down(), Disposition::Pass, "callbacks are inert");
}

#[test]
fn hq_04_01_a_lease_failure_is_a_typed_start_failure_then_retry_recovers() {
    let rig = Rig::new(config(HOLD));
    rig.leaser.fail_next_lease();
    let _ = rig.start();
    assert_start_failure(&rig);
    assert_eq!(rig.apps.observations(), 0, "the observation was never installed");
    rig.registry.resume(rig.handle).ok();
    rig.registry.reload(rig.handle, config(HOLD)).ok();
    // Retry restarts a failed module.
    let retry = rig.registry.set_enabled(rig.handle, true);
    assert!(
        retry.is_ok() || rig.status() == ModuleStatus::Running,
        "{retry:?}"
    );
    wait_until("the retry to run", || rig.status() == ModuleStatus::Running);
}

#[test]
fn hq_04_01_an_observation_failure_rolls_back_the_lease() {
    let rig = Rig::new(config(HOLD));
    rig.apps.fail_next_observe();
    let _ = rig.start();
    assert_start_failure(&rig);
    assert_eq!(rig.leaser.leases(), 1);
    assert_eq!(
        rig.hud_backend.panels_created(),
        0,
        "the HUD is installed after the observation"
    );
}

#[test]
fn hq_04_01_a_hud_install_failure_rolls_back_in_reverse_and_blocks_nothing() {
    let rig = Rig::new(config(HOLD));
    rig.hud_backend.fail_next_install(HudInstallError::NoScreen);
    let _ = rig.start();
    assert_start_failure(&rig);
    assert_eq!(rig.leaser.leases(), 1);
    assert!(!rig.hud.has_retained(), "a failed install holds no native panel");
    // Retry succeeds from a clean slate.
    rig.registry.pause(rig.handle).ok();
    let _ = rig.registry.resume(rig.handle);
    wait_until("the retry to run", || rig.status() == ModuleStatus::Running);
}

#[test]
fn hq_04_01_an_unavailable_input_service_is_a_typed_start_failure() {
    let mut rig = Rig::new(config(HOLD));
    // Re-register over the inert placeholder service.
    let parts = rig.registry.descriptors();
    assert_eq!(parts.len(), 1);
    drop(rig.registry);
    let registry = Arc::new(
        stache_sdk::Registry::new(
            Arc::clone(&rig.logger) as Arc<dyn stache_sdk::Logger>,
            Arc::new(stache_sdk::EventBus::new()),
            Arc::clone(&rig.apps_executor) as Arc<dyn stache_sdk::MainThreadExecutor>,
            Arc::clone(&rig.trust) as Arc<dyn stache_sdk::PermissionProvider>,
            Arc::new(stache_sdk::UnavailableWindows),
        )
        .with_input_service(InputService::unavailable()),
    );
    let handle = registry.register(rig.runtime.module(), config(HOLD)).expect("register");
    rig.runtime
        .install_accepted_watch(registry.accepted_watch(handle).expect("watch"));
    let _ = registry.start_all();
    let status = registry.snapshot(handle).expect("snapshot").status;
    let ModuleStatus::Failed(failure) = status else {
        panic!("expected Failed(Start), got {status:?}");
    };
    assert_eq!(failure.operation, Operation::Start);
    assert_eq!(failure.category, ErrorCategory::Platform);
    assert_eq!(rig.leaser.held(), 0, "full rollback");
    assert_eq!(rig.hud_backend.panels_live(), 0);
    assert_eq!(rig.apps.activation_observers(), 0);
    rig.registry = registry;
}

#[test]
fn hq_04_01_a_runtime_hud_failure_is_degraded_pass_through_and_pause_resume_recovers() {
    let (rig, _editor) = focused(HOLD);
    rig.hud_backend.fail_next_show(HudShowFailure::NotInstalled);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    wait_until("the degraded detail", || rig.runtime.degraded_detail().is_some());
    assert_eq!(rig.status(), ModuleStatus::Running);
    assert_eq!(rig.leaser.held(), 1, "still running with its lease");
    let _ = rig.q_up();
    assert_eq!(
        rig.q_down(),
        Disposition::Pass,
        "no subscription: every press passes"
    );
    let _ = rig.q_up();
    rig.registry.pause(rig.handle).expect("pause");
    assert!(rig.runtime.degraded_detail().is_none(), "stop clears the detail");
    rig.registry.resume(rig.handle).expect("resume");
    rig.wait_parked();
    assert_eq!(rig.q_down(), Disposition::Suppress, "recovered");
    let _ = rig.q_up();
}

#[test]
fn hq_04_01_a_stop_that_cannot_release_the_lease_reports_it_and_retries_idempotently() {
    let (rig, _editor) = focused(HOLD);
    rig.leaser.fail_next_release();
    let error = rig
        .registry
        .pause(rig.handle)
        .expect_err("an unacknowledged lease is a stop error");
    assert!(error.to_string().contains("lease"), "{error}");
    // The retry is idempotent and does not touch the already released owners.
    rig.registry.pause(rig.handle).ok();
    assert_eq!(rig.hud_backend.release_count(), 1);
}

// ---------------------------------------------------------------------------
// HQ-04-06: the Carbon yield
// ---------------------------------------------------------------------------

fn carbon_cmd_q(rig: &Rig, owner_priority: u16, chord: Chord) -> stache_sdk::input::ReservationId {
    let registry = rig.input.reservations();
    let owner = registry.new_owner();
    registry
        .register(
            owner,
            ReservationBackend::Carbon,
            Claim::Chord(chord),
            stache_sdk::input::InputPriority {
                level: owner_priority,
                tie_break: 0,
            },
        )
        .expect("carbon reservation")
}

#[test]
fn hq_04_06_the_hid_reservation_yields_a_carbon_cmd_q_in_both_orders_and_restores_it() {
    let cmd_q = Chord::new(KeyCode(12), ModifierFlags::COMMAND);
    let cmd_shift_q = Chord::new(KeyCode(12), ModifierFlags::COMMAND | ModifierFlags::SHIFT);

    // Carbon registered after the subscription.
    let rig = Rig::started(config(HOLD));
    let before = carbon_cmd_q(&rig, 100, cmd_q);
    let shifted = carbon_cmd_q(&rig, 100, cmd_shift_q);
    let registry = rig.input.reservations();
    assert!(
        !registry.try_dispatchable(before).expect("free"),
        "Cmd+Q yields to Hold-to-Quit"
    );
    assert!(
        registry.try_dispatchable(shifted).expect("free"),
        "Cmd+Shift+Q is untouched"
    );

    // Pause restores it, resume yields it again, stop restores it.
    rig.registry.pause(rig.handle).expect("pause");
    assert!(registry.try_dispatchable(before).expect("free"));
    rig.registry.resume(rig.handle).expect("resume");
    rig.wait_parked();
    assert!(!registry.try_dispatchable(before).expect("free"));
    rig.registry.set_enabled(rig.handle, false).expect("disable");
    assert!(registry.try_dispatchable(before).expect("free"));
    assert!(registry.try_dispatchable(shifted).expect("free"));

    // Carbon registered before the subscription.
    let rig = Rig::new(config(HOLD));
    let early = carbon_cmd_q(&rig, 100, cmd_q);
    let registry = rig.input.reservations();
    assert!(
        registry.try_dispatchable(early).expect("free"),
        "alone it dispatches"
    );
    rig.start().expect("start");
    rig.wait_parked();
    assert!(
        !registry.try_dispatchable(early).expect("free"),
        "yielded once subscribed"
    );
}

#[test]
fn hq_04_06_degrade_restores_the_carbon_binding() {
    let (rig, _editor) = focused(HOLD);
    let binding = carbon_cmd_q(&rig, 100, Chord::new(KeyCode(12), ModifierFlags::COMMAND));
    let registry = rig.input.reservations();
    assert!(!registry.try_dispatchable(binding).expect("free"));
    rig.hud_backend.fail_next_show(HudShowFailure::NotInstalled);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    wait_until("the degraded detail", || rig.runtime.degraded_detail().is_some());
    assert!(
        registry.try_dispatchable(binding).expect("free"),
        "no subscription, binding works"
    );
}

#[test]
fn hq_04_06_a_lower_priority_menu_anywhere_and_the_caps_handler_coexist_in_order() {
    use std::sync::Mutex;

    use stache_sdk::input::{
        CallbackScope, HandlerSpec, InputEvent, InputFilter, InputHandler, InputPriority, KindSet,
        WorkRequirement,
    };

    struct Recorder {
        name: &'static str,
        order: Arc<Mutex<Vec<&'static str>>>,
        disposition: Disposition,
    }
    impl InputHandler for Recorder {
        fn on_event(&self, _event: &InputEvent, _scope: &mut CallbackScope) -> Disposition {
            self.order.lock().expect("order").push(self.name);
            self.disposition
        }
    }

    let (rig, _editor) = focused(HOLD);
    let order = Arc::new(Mutex::new(Vec::new()));
    let cancel = stache_sdk::CancellationToken::new();
    let caps = rig
        .input
        .subscribe_handler(
            HandlerSpec {
                filter: InputFilter::new(KindSet::of(InputEventKind::FlagsChanged))
                    .with_keys([KeyCode(57)]),
                priority: InputPriority { level: 100, tie_break: 0 },
                work: WorkRequirement::None,
            },
            Arc::new(Recorder {
                name: "caps",
                order: Arc::clone(&order),
                disposition: Disposition::Pass,
            }),
            &cancel,
        )
        .expect("caps handler");
    let menu = rig
        .input
        .subscribe_handler(
            HandlerSpec {
                filter: InputFilter::new(KindSet::of(InputEventKind::KeyDown))
                    .with_keys([KeyCode(12)]),
                priority: InputPriority { level: 200, tie_break: 0 },
                work: WorkRequirement::None,
            },
            Arc::new(Recorder {
                name: "menu",
                order: Arc::clone(&order),
                disposition: Disposition::Pass,
            }),
            &cancel,
        )
        .expect("menu handler");
    // Hold-to-Quit (300) is consulted first and claims Command-only Q before the
    // lower-priority handler is reached.
    assert_eq!(rig.q_down(), Disposition::Suppress);
    assert!(
        order.lock().expect("order").is_empty(),
        "the claim ends the chain"
    );
    let _ = rig.q_up();
    // A press Hold-to-Quit passes reaches MenuAnywhere next.
    assert_eq!(
        rig.q_down_with(ModifierFlags::COMMAND | ModifierFlags::CAPS_LOCK),
        Disposition::Suppress
    );
    let _ = rig.q_up();
    rig.input.context_publisher().mark_unknown();
    assert_eq!(rig.q_down(), Disposition::Pass);
    assert_eq!(*order.lock().expect("order"), vec!["menu"]);
    let _ = rig.q_up();
    // Caps Lock (57) is not a Hold-to-Quit key.
    assert_eq!(
        rig.raw(InputEventKind::FlagsChanged, 57, ModifierFlags::CAPS_LOCK, false),
        Disposition::Pass
    );
    assert_eq!(*order.lock().expect("order"), vec!["menu", "caps"]);
    drop((caps, menu));
}

// ---------------------------------------------------------------------------
// HQ-05: the HUD lingers, frozen, after an early release
// ---------------------------------------------------------------------------

const DISMISS: u64 = 1500;

fn freezes(rig: &Rig) -> usize {
    rig.hud_backend
        .calls()
        .iter()
        .filter(|call| matches!(call, FakeHudCall::Freeze { .. }))
        .count()
}

fn hidden_presses(rig: &Rig) -> Vec<PressId> {
    rig.hud_backend
        .calls()
        .iter()
        .filter_map(|call| match call {
            FakeHudCall::Hide { press, .. } => Some(*press),
            _ => None,
        })
        .collect()
}

fn shown_presses(rig: &Rig) -> Vec<PressId> {
    rig.hud_backend
        .calls()
        .iter()
        .filter_map(|call| match call {
            FakeHudCall::Show { press, .. } => Some(*press),
            _ => None,
        })
        .collect()
}

/// A started rig with an editor frontmost and `dismiss` milliseconds of linger.
fn lingering(hold: u64, dismiss: u64) -> (Rig, App) {
    let rig = Rig::started(config_with_dismiss(hold, dismiss));
    let editor = rig.add_app("com.example.editor");
    rig.focus(&editor);
    (rig, editor)
}

/// Holds for `held` milliseconds, releases, and waits until the worker froze
/// the bar and parks for exactly the remaining linger.
fn release_early(rig: &Rig, total: u64, held: u64, dismiss: u64) {
    begin_hold(rig, total);
    rig.clock.advance(hold_for(held));
    assert_eq!(rig.q_up(), Disposition::Suppress);
    wait_until("the bar to freeze", || freezes(rig) == 1);
    rig.wait_parked_for("the linger wait", |timeout| timeout == Some(hold_for(dismiss)));
}

#[test]
fn hq_05_01_an_early_release_freezes_then_hides_after_the_dismiss_delay() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    release_early(&rig, HOLD, 400, DISMISS);
    let press = shown_presses(&rig)[0];
    assert_eq!(hides(&rig), 0, "nothing hides during the linger");
    assert_eq!(
        rig.hud_backend.displayed(rig.hud_handle()),
        Some(press),
        "still on screen"
    );
    rig.clock.advance(hold_for(DISMISS - 1));
    assert_eq!(hides(&rig), 0, "not before 400 + 1500 ms");
    rig.clock.advance(hold_for(1));
    wait_until("the single hide", || hides(&rig) == 1);
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
    assert_eq!(freezes(&rig), 1, "frozen once");
    assert_eq!(rig.hud_backend.displayed(rig.hud_handle()), None);
    assert_eq!(rig.apps.graceful_attempts(), 0, "a lingering press never quits");
    assert!(reports(&rig).is_empty());
    rig.clock.advance(hold_for(HOLD));
    assert_eq!(hides(&rig), 1, "exactly one hide");
}

#[test]
fn hq_05_01_a_shorter_delay_hides_at_release_plus_the_delay() {
    let (rig, _editor) = lingering(HOLD, 300);
    release_early(&rig, HOLD, 400, 300);
    rig.clock.advance(hold_for(299));
    assert_eq!(hides(&rig), 0);
    rig.clock.advance(hold_for(1));
    wait_until("the hide", || hides(&rig) == 1);
    assert_eq!(freezes(&rig), 1);
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_05_01_a_zero_delay_hides_at_once_without_a_freeze() {
    let (rig, _editor) = lingering(HOLD, 0);
    begin_hold(&rig, HOLD);
    rig.clock.advance(hold_for(400));
    assert_eq!(rig.q_up(), Disposition::Suppress);
    wait_until("the immediate hide", || hides(&rig) == 1);
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
    assert_eq!(freezes(&rig), 0);
}

#[test]
fn hq_05_01_a_cancel_at_the_deadline_and_a_command_release_linger_too() {
    // Command released before Q: the same cancelled mark, so it lingers.
    let (rig, _editor) = lingering(HOLD, DISMISS);
    begin_hold(&rig, HOLD);
    let _ = rig.command_flags(ModifierFlags::NONE);
    wait_until("the bar to freeze", || freezes(&rig) == 1);
    assert_eq!(hides(&rig), 0);
    rig.clock.advance(hold_for(DISMISS));
    wait_until("the hide", || hides(&rig) == 1);

    // A release delivered at the deadline wins and lingers.
    let (rig, _editor) = lingering(HOLD, DISMISS);
    begin_hold(&rig, HOLD);
    rig.clock.advance_quietly(hold_for(HOLD));
    assert_eq!(rig.q_up(), Disposition::Suppress);
    rig.clock.wake();
    wait_until("the bar to freeze", || freezes(&rig) == 1);
    assert_eq!(hides(&rig), 0);
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_05_01_a_focus_change_lingers_without_quitting() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    let other = rig.add_app("com.example.other");
    begin_hold(&rig, HOLD);
    rig.focus(&other);
    wait_until("the bar to freeze", || freezes(&rig) == 1);
    assert_eq!(hides(&rig), 0);
    rig.clock.advance(hold_for(DISMISS));
    wait_until("the hide", || hides(&rig) == 1);
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_05_02_a_new_press_during_the_linger_shows_and_is_never_hidden_by_the_old_one() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    release_early(&rig, HOLD, 400, DISMISS);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    wait_until("the second show", || orders(&rig) == 2);
    // The linger is gone: the worker waits for the new hold, not the old linger.
    rig.wait_parked_for("the fresh hold", |timeout| timeout == Some(hold_for(HOLD)));
    let presses = shown_presses(&rig);
    assert_eq!(presses.len(), 2);
    assert_ne!(presses[0], presses[1]);
    let handle = rig.hud_handle();
    assert_eq!(
        rig.hud_backend
            .calls()
            .into_iter()
            .skip_while(|call| { !matches!(call, FakeHudCall::Freeze { .. }) })
            .collect::<Vec<_>>(),
        vec![
            FakeHudCall::Freeze { handle, press: presses[0] },
            FakeHudCall::Show { handle, press: presses[1] },
            FakeHudCall::Order { handle, press: presses[1] },
            FakeHudCall::Hide { handle, press: presses[0] },
        ],
        "the old press is hidden right after the new show, a no-op once the new press is displayed"
    );
    assert_eq!(rig.hud_backend.displayed(handle), Some(presses[1]));
    assert_eq!(
        hidden_presses(&rig),
        vec![presses[0]],
        "the successor is never hidden"
    );
    // The new hold is a real one and quits at its own deadline.
    rig.clock.advance(hold_for(HOLD));
    assert_eq!(wait_report(&rig).outcome, GracefulOutcome::Requested);
    assert_eq!(
        hidden_presses(&rig),
        vec![presses[0], presses[1]],
        "only the successful fire hid the new press"
    );
}

#[test]
fn hq_05_02_a_successor_refused_before_it_touches_the_panel_leaves_nothing_on_screen() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    release_early(&rig, HOLD, 400, DISMISS);
    let first = shown_presses(&rig)[0];
    // The successor's show job waits in the queue while its press is released,
    // so the gate refuses it and the old panel would stay frozen forever.
    rig.hud_executor.set_on_main(false);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    rig.wait_parked_for("the pending show", |timeout| {
        timeout == Some(CANCELLATION_QUANTUM)
    });
    assert_eq!(rig.q_up(), Disposition::Suppress);
    rig.wait_parked_for("the successor's linger wait", |timeout| {
        timeout == Some(hold_for(DISMISS))
    });
    rig.hud_executor.set_on_main(true);
    rig.hud_executor.drain();
    rig.clock.wake();
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
    let handle = rig.hud_handle();
    assert_eq!(
        rig.hud_backend.displayed(handle),
        None,
        "the old panel was ordered out"
    );
    assert_eq!(orders(&rig), 1, "the refused successor never reached the screen");
    assert_eq!(
        hidden_presses(&rig),
        vec![first],
        "only the old press is hidden, never the successor"
    );
    rig.clock.advance(hold_for(DISMISS));
    assert_eq!(hidden_presses(&rig), vec![first]);
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_05_02_a_successor_whose_show_fails_orders_the_old_panel_out() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    release_early(&rig, HOLD, 400, DISMISS);
    let first = shown_presses(&rig)[0];
    rig.hud_backend.fail_next_show(HudShowFailure::NoScreen);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    wait_until("the old panel to be ordered out", || {
        rig.hud_backend.displayed(rig.hud_handle()).is_none() && hides(&rig) >= 1
    });
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
    assert_eq!(orders(&rig), 1, "the failed successor never reached the screen");
    assert!(hidden_presses(&rig).contains(&first), "the old press was hidden");
    assert_eq!(rig.hud_backend.displayed(rig.hud_handle()), None);
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_05_03_a_successful_fire_hides_at_once_with_no_freeze() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    begin_hold(&rig, HOLD);
    rig.clock.advance(hold_for(HOLD));
    assert_eq!(wait_report(&rig).outcome, GracefulOutcome::Requested);
    assert_eq!(hides(&rig), 1);
    assert_eq!(freezes(&rig), 0);
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
}

#[test]
fn hq_05_03_a_revision_change_hides_a_live_press_at_once() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    begin_hold(&rig, HOLD);
    rig.registry
        .reload(rig.handle, config_with_dismiss(300, DISMISS))
        .expect("reload");
    wait_until("the immediate hide", || hides(&rig) == 1);
    assert_eq!(freezes(&rig), 0, "a revision change never lingers");
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
}

#[test]
fn hq_05_03_a_revision_change_hides_a_pending_linger_at_once() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    release_early(&rig, HOLD, 400, DISMISS);
    assert_eq!(hides(&rig), 0);
    rig.registry
        .reload(rig.handle, config_with_dismiss(200, DISMISS))
        .expect("reload");
    wait_until("the linger to hide at once", || hides(&rig) == 1);
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
    assert_eq!(freezes(&rig), 1);
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_05_03_a_failed_show_degrades_with_an_immediate_hide_and_no_freeze() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    rig.hud_backend.fail_next_show(HudShowFailure::Native("boom".to_owned()));
    assert_eq!(rig.q_down(), Disposition::Suppress);
    wait_until("the degraded detail", || rig.runtime.degraded_detail().is_some());
    assert_eq!(freezes(&rig), 0);
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
}

/// Asserts that the press ended at once: no freeze, one hide, nothing on screen.
fn assert_immediate_dismissal(rig: &Rig) {
    wait_until("the immediate hide", || hides(rig) >= 1);
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
    assert_eq!(freezes(rig), 0, "an ending that is not the user's never lingers");
    assert_eq!(rig.hud_backend.displayed(rig.hud_handle()), None);
    assert_eq!(rig.apps.graceful_attempts(), 0);
    rig.clock.advance(hold_for(DISMISS * 2));
    assert_eq!(freezes(rig), 0);
}

#[test]
fn hq_05_03_supersession_of_a_live_press_hides_it_at_once() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    begin_hold(&rig, HOLD);
    let first = shown_presses(&rig)[0];
    // A second claimed down while the first is still live supersedes it.
    assert_eq!(rig.q_down(), Disposition::Suppress);
    wait_until("the second show", || orders(&rig) == 2);
    assert_eq!(freezes(&rig), 0, "a superseded press never lingers");
    assert!(
        hidden_presses(&rig).contains(&first),
        "the superseded press is hidden"
    );
    let second = shown_presses(&rig)[1];
    assert!(!hidden_presses(&rig).contains(&second));
    assert_eq!(rig.hud_backend.displayed(rig.hud_handle()), Some(second));
}

#[test]
fn hq_05_03_stop_during_a_live_press_hides_it_at_once() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    begin_hold(&rig, HOLD);
    rig.registry.pause(rig.handle).expect("pause");
    assert_eq!(freezes(&rig), 0, "stop never lingers");
    // Either the worker's hide or the owner's release ordered it out.
    assert_eq!(rig.hud_backend.displayed(rig.hud_handle()), None);
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_05_03_a_reconfigure_that_clears_accepting_hides_a_live_press_at_once() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    begin_hold(&rig, HOLD);
    // A different dismissDelay is an accepted change too.
    rig.registry
        .reload(rig.handle, config_with_dismiss(HOLD, DISMISS + 1))
        .expect("reload");
    assert_immediate_dismissal(&rig);
}

#[test]
fn hq_05_03_an_epoch_the_worker_finds_stale_hides_at_once() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    begin_hold(&rig, HOLD);
    // Invalidating the observation moves the watch past the press's epoch
    // without the activation listener cancelling the press.
    let _ = rig.control.stop();
    rig.clock.advance(hold_for(HOLD));
    assert_immediate_dismissal(&rig);
}

#[test]
fn hq_05_03_a_tap_disable_cancel_lingers_and_never_quits() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    begin_hold(&rig, HOLD);
    // The callback cancel changes neither `accepting` nor the revision, so it
    // lingers; no authorization survives it.
    rig.input.tap_disabled(TapKind::Suppressing);
    wait_until("the bar to freeze", || freezes(&rig) == 1);
    assert_eq!(hides(&rig), 0);
    rig.clock.advance(hold_for(DISMISS));
    wait_until("the hide", || hides(&rig) == 1);
    rig.clock.advance(hold_for(HOLD * 2));
    assert_eq!(rig.apps.graceful_attempts(), 0, "a cancelled press never quits");
    assert!(reports(&rig).is_empty());
}

#[test]
fn hq_05_03_stop_during_a_linger_ends_it_without_waiting() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    release_early(&rig, HOLD, 400, DISMISS);
    rig.registry.pause(rig.handle).expect("pause does not wait for the linger");
    assert_eq!(
        rig.hud_backend.displayed(rig.hud_handle()),
        None,
        "nothing stays on screen"
    );
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_05_04_a_stale_for_the_lingering_press_clears_it_without_a_second_hide() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    rig.hud_executor.set_on_main(false);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    rig.wait_parked_for("the pending show", |timeout| {
        timeout == Some(CANCELLATION_QUANTUM)
    });
    assert_eq!(rig.q_up(), Disposition::Suppress);
    rig.wait_parked_for("the linger wait", |timeout| timeout == Some(hold_for(DISMISS)));
    // The release beat the show: the gate refuses it and reports Stale.
    rig.hud_executor.set_on_main(true);
    rig.hud_executor.drain();
    rig.clock.wake();
    rig.wait_parked_for("the idle wait", |timeout| timeout.is_none());
    assert_eq!(
        orders(&rig),
        0,
        "a release that beats the show leaves nothing on screen"
    );
    assert_eq!(hides(&rig), 0, "no second hide");
    rig.clock.advance(hold_for(DISMISS));
    assert_eq!(hides(&rig), 0);
    assert_eq!(rig.apps.graceful_attempts(), 0);
}

#[test]
fn hq_05_04_a_shown_after_the_release_is_frozen_never_tracked_and_hidden_once() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    rig.hud_executor.set_on_main(false);
    assert_eq!(rig.q_down(), Disposition::Suppress);
    rig.wait_parked_for("the pending show", |timeout| {
        timeout == Some(CANCELLATION_QUANTUM)
    });
    // The release lands after the gate passed and before the Shown is delivered.
    let ordered = Arc::new(AtomicBool::new(false));
    let go = Arc::new(AtomicBool::new(false));
    let hook_flags = (Arc::clone(&ordered), Arc::clone(&go));
    rig.hud_backend.set_post_order(Some(Arc::new(move || {
        hook_flags.0.store(true, Ordering::SeqCst);
        wait_until("the release", || hook_flags.1.load(Ordering::SeqCst));
    })));
    let executor = Arc::clone(&rig.hud_executor);
    let drain = std::thread::spawn(move || executor.drain());
    wait_until("the panel to be ordered", || ordered.load(Ordering::SeqCst));
    assert_eq!(rig.q_up(), Disposition::Suppress);
    rig.wait_parked_for("the linger wait", |timeout| timeout == Some(hold_for(DISMISS)));
    go.store(true, Ordering::SeqCst);
    drain.join().expect("drain");
    rig.hud_executor.set_on_main(true);
    rig.hud_executor.drain();
    rig.clock.wake();
    wait_until("the late Shown to freeze again", || freezes(&rig) == 2);
    rig.wait_parked_for("the linger wait", |timeout| timeout == Some(hold_for(DISMISS)));
    assert_eq!(
        hides(&rig),
        0,
        "a late Shown for the lingering press is not hidden"
    );
    assert_eq!(rig.apps.graceful_attempts(), 0, "and never tracked");
    rig.clock.advance(hold_for(DISMISS));
    wait_until("the single hide", || hides(&rig) == 1);
    assert_eq!(rig.hud_backend.displayed(rig.hud_handle()), None);
}

#[test]
fn hq_05_06_the_idle_worker_parks_only_for_the_remaining_linger() {
    let (rig, _editor) = lingering(HOLD, DISMISS);
    release_early(&rig, HOLD, 400, DISMISS);
    rig.clock.advance(hold_for(600));
    rig.clock.wake();
    rig.wait_parked_for("the remaining linger", |timeout| timeout == Some(hold_for(900)));
    rig.clock.advance(hold_for(900));
    wait_until("the hide", || hides(&rig) == 1);
    rig.wait_parked_for("no timeout again", |timeout| timeout.is_none());
    let waits = rig.clock.waits();
    assert_eq!(waits.last(), Some(&None));
    assert!(
        waits
            .iter()
            .all(|wait| wait.is_none_or(|wait| wait <= hold_for(DISMISS.max(HOLD)))),
        "every wait is bounded by the hold or the linger: {waits:?}"
    );
}

#[test]
fn hq_05_01_an_unrepresentable_linger_waits_without_a_timeout() {
    let (rig, _editor) = lingering(HOLD, u64::MAX);
    begin_hold(&rig, HOLD);
    assert_eq!(rig.q_up(), Disposition::Suppress);
    wait_until("the bar to freeze", || freezes(&rig) == 1);
    let huge = hold_for(10_000_000);
    rig.wait_parked_for("a very long or unbounded linger", |timeout| {
        timeout.is_none_or(|wait| wait > huge)
    });
    assert_eq!(hides(&rig), 0);
    // A new press supersedes it.
    assert_eq!(rig.q_down(), Disposition::Suppress);
    wait_until("the second show", || orders(&rig) == 2);
    let presses = shown_presses(&rig);
    assert_eq!(
        hidden_presses(&rig),
        vec![presses[0]],
        "only the old press is hidden"
    );
    assert_eq!(rig.hud_backend.displayed(rig.hud_handle()), Some(presses[1]));
}

// ---------------------------------------------------------------------------
// HQ-06: HUD position and animation
// ---------------------------------------------------------------------------

fn styled(position: HudPosition, animation: HudAnimation) -> CommandQuitConfig {
    CommandQuitConfig {
        position,
        animation,
        ..config(HOLD)
    }
}

#[test]
fn hq_06_04_the_default_show_carries_bottom_and_fade() {
    let (rig, _editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    wait_until("the show", || orders(&rig) == 1);
    let shows = rig.hud_backend.shows();
    assert_eq!(shows.len(), 1);
    assert_eq!(
        (shows[0].anchor, shows[0].motion),
        (HudAnchor::Bottom, HudMotion::Fade)
    );
}

#[test]
fn hq_06_04_every_position_and_animation_maps_to_the_adapter() {
    let positions = [
        (HudPosition::Bottom, HudAnchor::Bottom),
        (HudPosition::Top, HudAnchor::Top),
        (HudPosition::Left, HudAnchor::Left),
        (HudPosition::Right, HudAnchor::Right),
        (HudPosition::Center, HudAnchor::Center),
    ];
    for (position, anchor) in positions {
        for (animation, motion) in [
            (HudAnimation::Fade, HudMotion::Fade),
            (HudAnimation::Slide, HudMotion::Slide),
        ] {
            let rig = Rig::started(styled(position, animation));
            let editor = rig.add_app("com.example.editor");
            rig.focus(&editor);
            begin_hold(&rig, HOLD);
            wait_until("the show", || orders(&rig) == 1);
            let shows = rig.hud_backend.shows();
            assert_eq!((shows[0].anchor, shows[0].motion), (anchor, motion));
        }
    }
}

#[test]
fn hq_06_04_a_reload_applies_the_new_values_to_the_next_press() {
    let (rig, _editor) = focused(HOLD);
    begin_hold(&rig, HOLD);
    wait_until("the first show", || orders(&rig) == 1);
    assert_eq!(rig.q_up(), Disposition::Suppress);
    wait_until("the first hide", || hides(&rig) == 1);
    rig.registry
        .reload(rig.handle, styled(HudPosition::Top, HudAnimation::Slide))
        .expect("reload");
    // New presses pass until the reloaded value is published.
    wait_until("the runtime to re-arm", || {
        let armed = rig.q_down() == Disposition::Suppress;
        if !armed {
            let _ = rig.q_up();
        }
        armed
    });
    wait_until("the second show", || orders(&rig) == 2);
    let shows = rig.hud_backend.shows();
    assert_eq!(
        (shows[0].anchor, shows[0].motion),
        (HudAnchor::Bottom, HudMotion::Fade)
    );
    assert_eq!(
        (shows[1].anchor, shows[1].motion),
        (HudAnchor::Top, HudMotion::Slide)
    );
}
