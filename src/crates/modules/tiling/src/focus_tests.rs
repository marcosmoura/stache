//! The Bar's exact focus path ([`TilingFocus`]) over the socket tests' fake
//! actor: availability, issuance revalidation, the shared lane, the bounded
//! record wait and the typed outcomes. No socket is involved.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use stache_macos::windows::WindowInstance;
use stache_sdk::ModuleStatus;
use stache_sdk::ipc::IpcHandler as _;

use crate::intent::{FocusTarget, Intent};
use crate::ipc::{
    ActionLane, ActorPort as _, ExactFocus, ExactFocusOutcome, FocusUnavailable, IpcTiming,
    TilingFocus, TilingIpc,
};
use crate::ipc_tests::{Built, Mode, Rig, config, fast, inactive_b, module};
use crate::published::window_key;
use crate::state::{Health, HealthFault, TilingState};

fn focus_of(rig: &Rig, timing: IpcTiming, lane: ActionLane) -> TilingFocus {
    let life = Arc::clone(&rig.life);
    TilingFocus::new(
        Arc::new(Arc::clone(&rig.actor)),
        Arc::new(move || crate::ipc::LifecycleControl::module(&life)),
        rig.status.clone(),
        rig.cancel.clone(),
        timing,
        lane,
    )
}

/// Like the socket tests' standard world, but with the unique per-epoch
/// serials the real adapter allocates (window keys are serial based).
fn standard() -> (Rig, WindowInstance, WindowInstance) {
    let mut built = Built::new(config());
    let editor = built.add(1, 1, "Editor — main.rs", 0.0, 101);
    let browser = built.add(2, 2, "Browser 日本語 🙂", 1920.0, 201);
    built.focus(editor);
    (Rig::new(built.state), editor, browser)
}

fn focus(rig: &Rig) -> TilingFocus { focus_of(rig, fast(), ActionLane::default()) }

fn window(key: String) -> ExactFocus { ExactFocus::Window(key) }

#[test]
fn an_exact_window_key_reaches_the_actor_as_the_exact_token() {
    let (rig, editor, browser) = standard();
    let focus = focus(&rig);
    assert!(focus.healthy());
    assert_eq!(focus.resolve(&window_key(browser)), Some(browser));
    assert_eq!(
        focus.focus(&window(window_key(browser))),
        ExactFocusOutcome::Applied
    );
    assert_eq!(rig.actor.submitted(), vec![Intent::FocusWindow(
        FocusTarget::Window(browser)
    )]);
    // The already focused window is an admitted no-op.
    assert_eq!(
        focus.focus(&window(window_key(browser))),
        ExactFocusOutcome::NoOp
    );
    assert_eq!(
        focus.focus(&window(window_key(editor))),
        ExactFocusOutcome::Applied
    );
}

#[test]
fn a_workspace_name_reaches_the_actor_and_the_focused_one_is_a_no_op() {
    let (rig, ..) = standard();
    let focus = focus(&rig);
    assert_eq!(
        focus.focus(&ExactFocus::Workspace("b".to_owned())),
        ExactFocusOutcome::Applied
    );
    assert_eq!(
        focus.focus(&ExactFocus::Workspace("b".to_owned())),
        ExactFocusOutcome::NoOp
    );
    assert_eq!(rig.actor.submitted(), vec![
        Intent::FocusWorkspace("b".to_owned()),
        Intent::FocusWorkspace("b".to_owned()),
    ]);
}

#[test]
fn unknown_malformed_or_foreign_targets_are_stale_and_send_nothing() {
    let (rig, _, browser) = standard();
    let focus = focus(&rig);
    let raw = window_key(browser);
    for key in [
        String::new(),
        "abc".to_owned(),
        format!("+{raw}"),
        format!("0{raw}"),
        format!("{raw} "),
        "1".to_owned(),
        // A key of the same serial in another backend epoch (another
        // generation) never resolves.
        (raw.parse::<u128>().expect("key") + (1u128 << 64)).to_string(),
    ] {
        assert_eq!(
            focus.focus(&window(key.clone())),
            ExactFocusOutcome::Stale,
            "{key:?}"
        );
    }
    for name in ["", "B", "a ", "zz"] {
        assert_eq!(
            focus.focus(&ExactFocus::Workspace(name.to_owned())),
            ExactFocusOutcome::Stale,
            "{name:?}"
        );
    }
    assert!(rig.actor.submitted().is_empty(), "nothing reached the actor");
}

#[test]
fn availability_is_typed_and_nothing_is_sent_or_resumed() {
    let (rig, _, browser) = standard();
    let focus = focus(&rig);
    let target = window(window_key(browser));
    for (snapshot, expected) in [
        (None, FocusUnavailable::NotRunning),
        (
            Some(module(ModuleStatus::Disabled, false, false)),
            FocusUnavailable::Disabled,
        ),
        (
            Some(module(ModuleStatus::Running, true, true)),
            FocusUnavailable::Paused,
        ),
        (
            Some(module(ModuleStatus::Stopped, true, false)),
            FocusUnavailable::NotRunning,
        ),
    ] {
        rig.life.set_module(snapshot);
        assert_eq!(focus.focus(&target), ExactFocusOutcome::Unavailable(expected));
    }
    rig.life.set_module(Some(module(ModuleStatus::Running, true, false)));
    rig.status.stopping();
    assert_eq!(
        focus.focus(&target),
        ExactFocusOutcome::Unavailable(FocusUnavailable::CleanupPending)
    );
    assert!(rig.actor.submitted().is_empty());
    assert_eq!(
        rig.life.resumes.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a click never resumes"
    );
}

#[test]
fn an_unhealthy_dead_or_initializing_runtime_is_unavailable() {
    let (rig, _, browser) = standard();
    let focus = focus(&rig);
    rig.actor.publish(|state| state.health = Health::Failed(HealthFault::Panicked));
    assert!(!focus.healthy());
    assert_eq!(
        focus.focus(&window(window_key(browser))),
        ExactFocusOutcome::Unavailable(FocusUnavailable::Unhealthy)
    );
    rig.actor.kill();
    assert_eq!(
        focus.focus(&window(window_key(browser))),
        ExactFocusOutcome::Unavailable(FocusUnavailable::NotRunning)
    );
    let rig = Rig::new(TilingState::new("com.stache.app"));
    assert_eq!(
        focus_of(&rig, fast(), ActionLane::default()).focus(&ExactFocus::Workspace("a".to_owned())),
        ExactFocusOutcome::Unavailable(FocusUnavailable::NotRunning),
        "before the first enumeration nothing is focused"
    );
}

#[test]
fn a_full_queue_is_busy_and_a_closed_mailbox_failed() {
    let (rig, _, browser) = standard();
    let focus = focus(&rig);
    rig.actor.set_mode(Mode::Full);
    assert_eq!(
        focus.focus(&window(window_key(browser))),
        ExactFocusOutcome::Busy
    );
    rig.actor.set_mode(Mode::Closed);
    assert_eq!(
        focus.focus(&window(window_key(browser))),
        ExactFocusOutcome::Failed
    );
}

#[test]
fn an_unrecorded_intent_is_uncertain_and_shutdown_ends_the_wait_promptly() {
    let (rig, _, browser) = standard();
    let timing = IpcTiming {
        budget: Duration::from_secs(4),
        hop: Duration::from_millis(80),
        poll: Duration::from_millis(2),
    };
    let focus = focus_of(&rig, timing, ActionLane::default());
    rig.actor.set_mode(Mode::Swallow);
    assert_eq!(
        focus.focus(&window(window_key(browser))),
        ExactFocusOutcome::Uncertain
    );

    let timing = IpcTiming {
        hop: Duration::from_secs(2),
        ..timing
    };
    let focus = focus_of(&rig, timing, ActionLane::default());
    let cancel = rig.cancel;
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        cancel.cancel();
    });
    let started = Instant::now();
    assert_eq!(
        focus.focus(&window(window_key(browser))),
        ExactFocusOutcome::Uncertain
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    canceller.join().expect("join");
    // After shutdown nothing more is attempted.
    assert_eq!(
        focus.focus(&window(window_key(browser))),
        ExactFocusOutcome::Unavailable(FocusUnavailable::ShuttingDown)
    );
}

#[test]
fn a_held_lane_is_busy_and_never_queues() {
    let (rig, editor, browser) = standard();
    let lane = ActionLane::default();
    let ipc = Arc::new(
        TilingIpc::with_timing(
            Arc::new(Arc::clone(&rig.actor)),
            Arc::new(Arc::clone(&rig.life)),
            rig.status.clone(),
            rig.cancel.clone(),
            fast(),
        )
        .with_lane(lane.clone()),
    );
    let focus = focus_of(&rig, fast(), lane);
    rig.actor.set_mode(Mode::Delay(Duration::from_millis(300)));
    let socket = std::thread::spawn(move || ipc.handle("window", json!({"swap": "right"})));
    // Wait until the socket request is inside the lane (its intent was sent).
    let deadline = Instant::now() + Duration::from_secs(5);
    while rig.actor.submitted().is_empty() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let started = Instant::now();
    assert_eq!(
        focus.focus(&window(window_key(browser))),
        ExactFocusOutcome::Busy
    );
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "busy never waits"
    );
    let _ = socket.join().expect("join");
    rig.actor.set_mode(Mode::Process);
    assert_eq!(
        focus.focus(&window(window_key(editor))),
        ExactFocusOutcome::NoOp,
        "the lane is free again"
    );
}

#[test]
fn an_inactive_workspace_is_refused_and_recorded() {
    let rig = Rig::new(inactive_b().state);
    let focus = focus(&rig);
    for serial in 1..=2 {
        assert_eq!(
            focus.focus(&ExactFocus::Workspace("b".to_owned())),
            ExactFocusOutcome::Refused
        );
        let snapshot = rig.actor.snapshot().expect("snapshot");
        assert_eq!(
            snapshot.refused_focus.map(|refused| refused.serial),
            Some(serial)
        );
    }
}
