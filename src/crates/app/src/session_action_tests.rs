use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use super::*;

fn json<T: Serialize>(value: &T) -> serde_json::Value { serde_json::to_value(value).expect("json") }

#[test]
fn outcomes_have_exact_concrete_wire_forms() {
    use SessionActionOutcome::{
        Applied, Busy, Disabled, NotApplied, Stale, Unauthorized, Unavailable, Uncertain,
    };
    for (outcome, wire) in [
        (Applied, serde_json::json!({ "kind": "applied" })),
        (
            NotApplied {
                reason: SessionNotApplied::Refused,
            },
            serde_json::json!({ "kind": "notApplied", "reason": "refused" }),
        ),
        (
            NotApplied {
                reason: SessionNotApplied::NotReflected,
            },
            serde_json::json!({ "kind": "notApplied", "reason": "notReflected" }),
        ),
        (Stale, serde_json::json!({ "kind": "stale" })),
        (Disabled, serde_json::json!({ "kind": "disabled" })),
        (
            Unavailable {
                reason: SessionUnavailable::ReleasePending,
            },
            serde_json::json!({ "kind": "unavailable", "reason": "releasePending" }),
        ),
        (
            Unavailable {
                reason: SessionUnavailable::WaitingForPermission,
            },
            serde_json::json!({ "kind": "unavailable", "reason": "waitingForPermission" }),
        ),
        (Busy, serde_json::json!({ "kind": "busy" })),
        (Uncertain, serde_json::json!({ "kind": "uncertain" })),
        (Unauthorized, serde_json::json!({ "kind": "unauthorized" })),
    ] {
        assert_eq!(json(&outcome), wire);
        assert_eq!(
            serde_json::from_value::<SessionActionOutcome>(wire).expect("round"),
            outcome
        );
    }
}

#[test]
fn the_action_enum_is_closed_to_pause_and_resume() {
    assert_eq!(json(&SessionAction::Pause), "pause");
    assert_eq!(json(&SessionAction::Resume), "resume");
    for other in [
        "enable", "disable", "retry", "toggle", "Pause", "config", "",
    ] {
        assert!(
            serde_json::from_value::<SessionAction>(serde_json::json!(other)).is_err(),
            "{other:?} must not deserialize"
        );
    }
}

#[test]
fn the_bar_capability_holds_only_pause_and_resume_of_keep_awake() {
    assert!(bar_may("keepAwake", SessionAction::Pause));
    assert!(bar_may("keepAwake", SessionAction::Resume));
    for module in [
        "tiling",
        "bar",
        "keybindings",
        "holdToQuit",
        "wallpapers",
        "keepawake",
        "KeepAwake",
        "keepAwake ",
        "",
        "global",
    ] {
        for action in [SessionAction::Pause, SessionAction::Resume] {
            assert!(!bar_may(module, action), "{module:?}");
        }
    }
}

#[test]
fn a_finished_job_returns_its_outcome() {
    let job = SessionJob::new("keepAwake", SessionAction::Pause);
    assert!(job.begin());
    let runner = Arc::clone(&job);
    let handle = thread::spawn(move || runner.finish(SessionActionOutcome::Applied));
    assert_eq!(
        job.wait(Duration::from_secs(5), || false),
        SessionActionOutcome::Applied
    );
    handle.join().expect("runner");
}

#[test]
fn a_queued_job_is_cancelled_by_shutdown_and_the_runner_skips_it() {
    let job = SessionJob::new("keepAwake", SessionAction::Pause);
    let stopped = AtomicBool::new(false);
    stopped.store(true, Ordering::SeqCst);
    let started = Instant::now();
    assert_eq!(
        job.wait(Duration::from_secs(30), || stopped.load(Ordering::SeqCst)),
        SessionActionOutcome::Unavailable {
            reason: SessionUnavailable::ShuttingDown
        }
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "shutdown wakes the waiter"
    );
    assert!(!job.begin(), "a cancelled job never starts");
}

#[test]
fn a_running_job_at_shutdown_is_uncertain_and_never_hangs() {
    let job = SessionJob::new("keepAwake", SessionAction::Resume);
    assert!(job.begin());
    let started = Instant::now();
    assert_eq!(
        job.wait(Duration::from_secs(30), || true),
        SessionActionOutcome::Uncertain
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a_job_the_runner_never_reaches_times_out_as_busy_and_a_started_one_as_uncertain() {
    let queued = SessionJob::new("keepAwake", SessionAction::Pause);
    assert_eq!(
        queued.wait(Duration::from_millis(60), || false),
        SessionActionOutcome::Busy
    );
    assert!(!queued.begin());
    let running = SessionJob::new("keepAwake", SessionAction::Pause);
    assert!(running.begin());
    assert_eq!(
        running.wait(Duration::from_millis(60), || false),
        SessionActionOutcome::Uncertain
    );
}
