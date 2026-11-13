//! The popover Overview commands against the host: each one queues exactly
//! the host command the former tray row queued, and Enable plus Pause/Resume
//! run through the real runner, the real `ConfigStore` and the real watcher
//! on a temp directory.

use std::time::Duration;

use super::config_documents_e2e::{Scenario, TILING};
use super::*;
use crate::popover_actions::{PopoverRequest, session};
use crate::session_action::{SessionAction, SessionActionOutcome};

fn queued(request: PopoverRequest) -> HostCommand {
    let (handle, driver) = new_host(super::tests::quiet_teardown());
    request.send(&handle);
    driver
        .receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("one queued command")
}

#[test]
fn each_popover_request_queues_the_tray_rows_host_command() {
    assert!(matches!(
        queued(PopoverRequest::SetEnabled { id: "keepAwake".to_owned(), enabled: true }),
        HostCommand::SetEnabled { id, enabled: true, job: None } if id == "keepAwake"
    ));
    assert!(matches!(
        queued(PopoverRequest::SetEnabled { id: "tiling".to_owned(), enabled: false }),
        HostCommand::SetEnabled { id, enabled: false, job: None } if id == "tiling"
    ));
    assert!(matches!(
        queued(PopoverRequest::Retry("proxyAudio".to_owned())),
        HostCommand::Retry(id) if id == "proxyAudio"
    ));
    assert!(matches!(
        queued(PopoverRequest::AccessGuide),
        HostCommand::OpenAccessGuide
    ));
}

#[test]
fn the_session_action_queues_one_session_job_for_the_named_module() {
    let (handle, driver) = new_host(super::tests::quiet_teardown());
    let waiter = std::thread::spawn(move || session(&handle, "tiling", SessionAction::Pause));
    let command = driver.receiver.recv_timeout(Duration::from_secs(5)).expect("queued");
    let HostCommand::Session(job) = command else {
        panic!("expected a session job")
    };
    assert_eq!(job.module, "tiling");
    assert_eq!(job.action, SessionAction::Pause);
    assert!(job.begin());
    job.finish(SessionActionOutcome::Stale);
    assert_eq!(waiter.join().expect("waiter"), SessionActionOutcome::Stale);
}

fn module(scenario: &Scenario, id: &str) -> crate::bindings::ModuleStatusView {
    scenario
        .runner
        .handle
        .status()
        .modules
        .into_iter()
        .find(|module| module.id == id)
        .expect("module")
}

/// Runs every queued command until the queue is idle.
fn drain(scenario: &mut Scenario) {
    while let Ok(command) = scenario.receiver.recv_timeout(Duration::from_millis(200)) {
        assert!(scenario.runner.run_command(command));
    }
}

/// Runs one session request: the caller blocks on its own thread while the
/// runner executes the queued job.
fn session_round(scenario: &mut Scenario, id: &str, action: SessionAction) -> SessionActionOutcome {
    let handle = scenario.runner.handle.clone();
    let id = id.to_owned();
    let waiter = std::thread::spawn(move || session(&handle, &id, action));
    loop {
        let command = scenario.receiver.recv_timeout(Duration::from_secs(5)).expect("command");
        let done = matches!(command, HostCommand::Session(_));
        assert!(scenario.runner.run_command(command));
        if done {
            break;
        }
    }
    waiter.join().expect("waiter")
}

#[test]
fn enable_pause_and_resume_from_the_popover_over_the_real_store() {
    let mut scenario = Scenario::new();
    assert_eq!(
        module(&scenario, "tiling").state,
        crate::bindings::ModuleState::Disabled
    );
    let before = scenario.revision("tiling");

    // Enable: the configuration-backed edit flips only `enabled`.
    PopoverRequest::SetEnabled {
        id: "tiling".to_owned(),
        enabled: true,
    }
    .send(&scenario.runner.handle);
    drain(&mut scenario);
    assert_eq!(
        scenario.read("tiling.toml"),
        TILING.replacen("enabled = false", "enabled = true", 1)
    );
    scenario.settle();
    assert_eq!(scenario.revision("tiling"), before + 1);
    let tiling = module(&scenario, "tiling");
    assert!(tiling.enabled);
    assert_eq!(tiling.state, crate::bindings::ModuleState::Running);
    assert_eq!(scenario.ui.take(), vec![vec!["tiling.toml".to_owned()]]);

    // Pause, a stale second pause, then resume: session only, no file write.
    assert_eq!(
        session_round(&mut scenario, "tiling", SessionAction::Pause),
        SessionActionOutcome::Applied
    );
    assert!(module(&scenario, "tiling").paused);
    assert_eq!(
        module(&scenario, "tiling").state,
        crate::bindings::ModuleState::Paused
    );
    assert_eq!(
        session_round(&mut scenario, "tiling", SessionAction::Pause),
        SessionActionOutcome::Stale
    );
    assert_eq!(
        session_round(&mut scenario, "tiling", SessionAction::Resume),
        SessionActionOutcome::Applied
    );
    assert_eq!(
        module(&scenario, "tiling").state,
        crate::bindings::ModuleState::Running
    );
    assert_eq!(
        scenario.read("tiling.toml"),
        TILING.replacen("enabled = false", "enabled = true", 1)
    );

    // Disable again through the same command.
    PopoverRequest::SetEnabled {
        id: "tiling".to_owned(),
        enabled: false,
    }
    .send(&scenario.runner.handle);
    drain(&mut scenario);
    assert_eq!(scenario.read("tiling.toml"), TILING);
    scenario.settle();
    let tiling = module(&scenario, "tiling");
    assert!(!tiling.enabled);
    assert_eq!(tiling.state, crate::bindings::ModuleState::Disabled);

    // An unknown module is the host's action error, not a write.
    PopoverRequest::SetEnabled {
        id: "nope".to_owned(),
        enabled: true,
    }
    .send(&scenario.runner.handle);
    drain(&mut scenario);
    assert!(
        scenario
            .runner
            .handle
            .status()
            .error
            .is_some_and(|error| error.contains("nope")),
        "the refusal is the host error: {:?}",
        scenario.runner.handle.status().error
    );
}

/// Runs one popover enable request: the caller blocks on its own thread while
/// the runner executes the queued command.
fn enable_round(
    scenario: &mut Scenario,
    id: &str,
    enabled: bool,
) -> crate::popover_actions::ModuleEnableOutcome {
    let handle = scenario.runner.handle.clone();
    let id = id.to_owned();
    let waiter = std::thread::spawn(move || handle.set_enabled_outcome(id, enabled));
    loop {
        let command = scenario.receiver.recv_timeout(Duration::from_secs(5)).expect("command");
        let done = matches!(command, HostCommand::SetEnabled { .. });
        assert!(scenario.runner.run_command(command));
        if done {
            break;
        }
    }
    waiter.join().expect("waiter")
}

#[test]
fn the_enable_outcome_reports_applied_unchanged_and_each_identical_refusal() {
    use crate::popover_actions::ModuleEnableOutcome;

    let mut scenario = Scenario::new();
    assert_eq!(
        enable_round(&mut scenario, "tiling", true),
        ModuleEnableOutcome::Applied
    );
    scenario.settle();
    assert_eq!(
        enable_round(&mut scenario, "tiling", true),
        ModuleEnableOutcome::Unchanged
    );

    // An invalid file: the same refusal twice, each one answered.
    std::fs::write(scenario.path("tiling.toml"), "enabled = [").expect("invalid");
    scenario.settle();
    let first = enable_round(&mut scenario, "tiling", false);
    let second = enable_round(&mut scenario, "tiling", false);
    assert!(
        matches!(&first, ModuleEnableOutcome::Refused { .. }),
        "{first:?}"
    );
    assert_eq!(first, second);
    assert_eq!(scenario.read("tiling.toml"), "enabled = [");

    // A symlinked file: refused twice, the target untouched.
    let target = scenario.path("elsewhere.toml");
    std::fs::write(&target, TILING).expect("target");
    std::fs::remove_file(scenario.path("tiling.toml")).expect("remove");
    std::os::unix::fs::symlink(&target, scenario.path("tiling.toml")).expect("symlink");
    scenario.settle();
    let first = enable_round(&mut scenario, "tiling", true);
    let second = enable_round(&mut scenario, "tiling", true);
    assert!(
        matches!(&first, ModuleEnableOutcome::Refused { .. }),
        "{first:?}"
    );
    assert_eq!(first, second);
    assert_eq!(std::fs::read_to_string(&target).expect("target"), TILING);
}

#[test]
fn an_enable_request_still_queued_at_shutdown_is_cancelled_and_never_runs() {
    use crate::popover_actions::ModuleEnableOutcome;

    let mut scenario = Scenario::new();
    let handle = scenario.runner.handle.clone();
    let waiter = std::thread::spawn(move || {
        handle.set_enabled_within("tiling", true, Duration::from_secs(60))
    });
    let command = scenario.receiver.recv_timeout(Duration::from_secs(5)).expect("queued");
    scenario.runner.handle.request_shutdown();
    assert!(matches!(
        waiter.join().expect("waiter"),
        ModuleEnableOutcome::Unavailable { .. }
    ));
    assert!(scenario.runner.run_command(command));
    assert_eq!(scenario.read("tiling.toml"), TILING);
}

#[test]
fn a_config_edit_that_timed_out_while_queued_writes_nothing() {
    use stache_sdk::config::{ConfigEditOp, PathSegment};

    use crate::config_documents::{ConfigEditOutcome, ConfigEditRefusal, ConfigEditRequest};

    let mut scenario = Scenario::new();
    let base_hash = Some(stache_sdk::config::content_hash(TILING.as_bytes()));
    let request = ConfigEditRequest {
        file: "tiling.toml".to_owned(),
        base_hash,
        op: ConfigEditOp::Set {
            path: vec![PathSegment::Key("enabled".to_owned())],
            value: serde_json::json!(true),
        },
    };
    let outcome = scenario.runner.handle.config_edit_within(request, Duration::from_millis(50));
    assert!(
        matches!(outcome, ConfigEditOutcome::Refused {
            reason: ConfigEditRefusal::Unavailable,
            ..
        }),
        "{outcome:?}"
    );
    // The runner reaches the edit only now: it is skipped.
    let command = scenario.receiver.recv_timeout(Duration::from_secs(5)).expect("queued");
    assert!(matches!(command, HostCommand::ConfigEdit { .. }));
    assert!(scenario.runner.run_command(command));
    assert_eq!(scenario.read("tiling.toml"), TILING);
    assert!(scenario.ui.take().is_empty());
}
