//! Runtime, policy, race and lifecycle tests for the noTunes module.
//!
//! Every test runs over the injected application backend and main executor, so
//! no test observes, terminates or opens a real application. Replacement paths
//! and bundle identifiers are registered in the fake, never resolved from the
//! machine.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use stache_macos::{
    ApplicationBackend, ApplicationControl, ApplicationObservation, FakeAppKey,
    FakeApplicationBackend, ManualExecutor,
};
use stache_sdk::{
    CancellationToken, Logger, MainThreadExecutor, ModuleHandle, ModuleStatus, Operation,
    Permission, PermissionStatus, Registry, RegistryError, ReloadStatus, ScopedLogger,
};

use super::*;
use crate::policy::{Inbox, PolicySnapshot, PolicyWorker, WorkerSetup, WorkerState};
use crate::test_support::{CaptureLogger, registry};

const WAIT: Duration = Duration::from_secs(5);

/// The replacement a path target names.
const REPLACEMENT_PATH: &str = "/Applications/Player.app";

/// The bundle identifier the fake reports for [`REPLACEMENT_PATH`].
const REPLACEMENT_BUNDLE_ID: &str = "com.example.player";

/// A second replacement, opened through its bundle identifier.
const OTHER_PATH: &str = "/Applications/Other Player.app";

/// The bundle identifier of [`OTHER_PATH`].
const OTHER_BUNDLE_ID: &str = "com.example.other";

struct Harness {
    registry: Arc<Registry>,
    handle: ModuleHandle<NoTunesConfig>,
    backend: Arc<FakeApplicationBackend>,
    control: ApplicationControl,
    executor: Arc<ManualExecutor>,
    captured: Arc<CaptureLogger>,
}

impl Harness {
    fn new(config: NoTunesConfig) -> Self {
        Self::build(config, Duration::from_millis(50), None, None)
    }

    fn build(
        config: NoTunesConfig,
        budget: Duration,
        after_accept: Option<Arc<dyn Fn() + Send + Sync>>,
        snapshot_probe: Option<Sender<Arc<Mutex<PolicySnapshot>>>>,
    ) -> Self {
        Self::build_with(config, budget, after_accept, snapshot_probe, None)
    }

    fn build_with(
        config: NoTunesConfig,
        budget: Duration,
        after_accept: Option<Arc<dyn Fn() + Send + Sync>>,
        snapshot_probe: Option<Sender<Arc<Mutex<PolicySnapshot>>>>,
        after_spawn: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let executor = Arc::new(ManualExecutor::new(true));
        let backend = Arc::new(FakeApplicationBackend::new());
        let control = ApplicationControl::with_acknowledgement_budget(
            Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
            Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
            budget,
        );
        let captured = Arc::new(CaptureLogger::default());
        let registry = Arc::new(registry(Arc::clone(&captured) as Arc<dyn Logger>));
        let mut module = NoTunesModule::new(control.clone());
        if let Some(after_accept) = after_accept {
            module = module.with_after_accept_hook(after_accept);
        }
        if let Some(after_spawn) = after_spawn {
            module = module.with_after_spawn_hook(after_spawn);
        }
        if let Some(snapshot_probe) = snapshot_probe {
            module = module.with_snapshot_probe(snapshot_probe);
        }
        let handle = registry.register(module, config).expect("register");
        Self {
            registry,
            handle,
            backend,
            control,
            executor,
            captured,
        }
    }

    fn start(&self) -> Result<(), RegistryError> { self.registry.start_all() }

    fn status(&self) -> ModuleStatus {
        self.registry.snapshot(self.handle).expect("snapshot").status
    }

    fn messages(&self) -> Vec<String> { self.captured.messages() }

    fn warnings(&self) -> Vec<String> { self.captured.warnings() }

    fn wait_message(&self, fragment: &str) {
        wait_until(&format!("log message `{fragment}`"), || {
            self.messages().iter().any(|message| message.contains(fragment))
        });
    }

    fn wait_warning(&self, fragment: &str) {
        wait_until(&format!("warning `{fragment}`"), || {
            self.warnings().iter().any(|message| message.contains(fragment))
        });
    }

    /// Registers one replacement both ways in the fake workspace.
    fn register_replacement(&self, path: &str, bundle_id: &str) {
        self.backend.add_installed(path, bundle_id);
    }

    /// Registers both replacements the tests reconfigure between.
    fn register_replacements(&self) {
        self.register_replacement(REPLACEMENT_PATH, REPLACEMENT_BUNDLE_ID);
        self.register_replacement(OTHER_PATH, OTHER_BUNDLE_ID);
    }
}

/// One real worker over the real control, driven event by event.
struct WorkerHarness {
    backend: Arc<FakeApplicationBackend>,
    observation: Arc<ApplicationObservation>,
    snapshot: Arc<Mutex<PolicySnapshot>>,
    receiver: Receiver<Inbox>,
    cancellation: CancellationToken,
    worker: PolicyWorker,
}

impl WorkerHarness {
    fn new(config: NoTunesConfig) -> Self {
        let executor = Arc::new(ManualExecutor::new(true));
        let backend = Arc::new(FakeApplicationBackend::new());
        let control = ApplicationControl::with_acknowledgement_budget(
            Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
            Arc::clone(&backend) as Arc<dyn ApplicationBackend>,
            Duration::from_millis(50),
        );
        let cancellation = CancellationToken::new();
        let (inbox, receiver) = mpsc::channel();
        let notify = inbox.clone();
        let observation = Arc::new(
            control
                .install(&cancellation, move |application| {
                    let _ = notify.send(Inbox::Launch(application));
                })
                .expect("install"),
        );
        let token = observation.accept().expect("accept");
        let snapshot = Arc::new(Mutex::new(PolicySnapshot { config, token: Some(token) }));
        let active = Arc::new(AtomicBool::new(true));
        let captured = Arc::new(CaptureLogger::default());
        let logger = ScopedLogger::new(
            Arc::clone(&captured) as Arc<dyn Logger>,
            NoTunesConfig::ID,
            Operation::Start,
        );
        let worker = PolicyWorker::new(
            WorkerSetup {
                observation: Arc::clone(&observation),
                snapshot: Arc::clone(&snapshot),
                active,
                inbox,
                logger,
                cancellation: cancellation.clone(),
            },
            WorkerState::default(),
        );
        Self {
            backend,
            observation,
            snapshot,
            receiver,
            cancellation,
            worker,
        }
    }

    /// Delivers one fake launch and processes every event it produced.
    fn launch(&mut self, key: FakeAppKey) {
        let _ = self.backend.launch(key);
        self.run();
    }

    /// Processes every already-queued inbox event.
    fn run(&mut self) {
        while let Ok(event) = self.receiver.try_recv() {
            self.worker.handle(event);
        }
    }

    /// Removes and returns the next queued event without handling it.
    fn next_event(&self) -> Inbox { self.receiver.try_recv().expect("a queued inbox event") }
}

fn configured(enabled: bool, target: Option<&str>) -> NoTunesConfig {
    NoTunesConfig {
        enabled,
        target_app: target.map(str::to_owned),
    }
}

/// A bundle-identifier target installed at [`REPLACEMENT_PATH`].
fn bundle_target() -> NoTunesConfig { configured(true, Some(REPLACEMENT_BUNDLE_ID)) }

/// An absolute path target whose identifier the fake resolves.
fn path_target() -> NoTunesConfig { configured(true, Some(REPLACEMENT_PATH)) }

/// A second bundle-identifier target installed at [`OTHER_PATH`].
fn other_bundle_target() -> NoTunesConfig { configured(true, Some(OTHER_BUNDLE_ID)) }

/// Block only: no replacement is launched.
fn block_only() -> NoTunesConfig { configured(true, None) }

fn wait_until(label: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_disabled_module_holds_no_resources() {
    let harness = Harness::new(configured(false, Some(REPLACEMENT_BUNDLE_ID)));
    harness.start().expect("disabled start");

    assert_eq!(harness.status(), ModuleStatus::Disabled);
    assert_eq!(harness.backend.observations(), 0);
    assert!(harness.backend.termination_requests().is_empty());
    assert!(harness.backend.open_requests().is_empty());
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn every_start_sweeps_both_blocked_apps_without_a_replacement() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let itunes = harness.backend.add_running(ITUNES_BUNDLE_ID);
    harness.start().expect("start");

    assert_eq!(harness.status(), ModuleStatus::Running);
    // The sweep is synchronous: it finished before start returned.
    assert_eq!(harness.backend.termination_requests(), vec![music, itunes]);
    assert!(harness.backend.open_requests().is_empty());

    harness.registry.pause(harness.handle).expect("pause");
    assert_eq!(harness.status(), ModuleStatus::Paused);
    harness.registry.resume(harness.handle).expect("resume");
    assert_eq!(
        harness.backend.termination_requests(),
        vec![music, itunes, music, itunes],
        "resume is an actual start and sweeps again"
    );
    assert!(harness.backend.open_requests().is_empty());

    harness.registry.set_enabled(harness.handle, false).expect("disable");
    harness.registry.set_enabled(harness.handle, true).expect("enable");
    assert_eq!(
        harness.backend.termination_requests().len(),
        6,
        "disable stops and enable is an actual start that sweeps again"
    );
    assert!(harness.backend.open_requests().is_empty());
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_notification_for_a_swept_instance_opens_the_configured_target_once() {
    for config in [path_target(), bundle_target()] {
        let harness = Harness::new(config);
        harness.register_replacements();
        let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
        harness.start().expect("start");
        assert!(
            harness.backend.open_requests().is_empty(),
            "the sweep never opens"
        );
        assert_eq!(harness.backend.termination_requests(), vec![music]);

        let _ = harness.backend.launch(music);
        wait_until("the replacement open", || {
            harness.backend.open_requests().len() == 1
        });
        assert_eq!(
            harness.backend.open_requests(),
            vec![PathBuf::from(REPLACEMENT_PATH)],
            "a path target and its bundle identifier open the same application"
        );
        assert_eq!(
            harness.backend.termination_requests(),
            vec![music],
            "the duplicate termination decision is deduplicated"
        );
        harness.registry.shutdown().expect("shutdown");
    }
}

#[test]
fn the_absent_target_only_blocks() {
    let harness = Harness::new(block_only());
    harness.register_replacements();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.start().expect("start");
    let _ = harness.backend.launch(music);

    let second = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(second);
    wait_until("the second termination", || {
        harness.backend.termination_requests().len() == 2
    });
    assert!(harness.backend.open_requests().is_empty());
    harness.wait_message("no replacement application is configured");
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn unrelated_empty_and_case_variant_identifiers_are_ignored() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    let unrelated = harness.backend.add_running("com.tinyspeck.slackmacgap");
    let replacement = harness.backend.add_running(REPLACEMENT_BUNDLE_ID);
    let other = harness.backend.add_running(OTHER_BUNDLE_ID);
    let lower = harness.backend.add_running("com.apple.music");
    let upper = harness.backend.add_running("COM.APPLE.MUSIC");
    let empty = harness.backend.add_running("");
    let unknown = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.backend.forget_bundle_id(unknown);
    for key in [unrelated, replacement, other, lower, upper, empty, unknown] {
        let _ = harness.backend.launch(key);
    }

    // The marker is processed after every ignored notification; FIFO makes its
    // observable termination proof that the earlier events were handled.
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    wait_until("the marker termination", || {
        harness.backend.termination_requests().contains(&music)
    });
    assert_eq!(harness.backend.termination_requests(), vec![music]);
    // The configured replacement is itself one of the ignored instances, so the
    // marker proves the event finished without an open.
    harness.wait_message("replacement com.example.player is already running");
    assert!(harness.backend.open_requests().is_empty());
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_running_replacement_is_not_relaunched() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    let _running = harness.backend.add_running(REPLACEMENT_BUNDLE_ID);
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    harness.wait_message("replacement com.example.player is already running");

    harness.registry.reload(harness.handle, other_bundle_target()).expect("reload");
    let second = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(second);
    wait_until("the other open", || harness.backend.open_requests().len() == 1);
    assert_eq!(harness.backend.open_requests(), vec![PathBuf::from(OTHER_PATH)]);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn an_already_running_path_target_is_not_relaunched() {
    let harness = Harness::new(path_target());
    harness.register_replacements();
    let _replacement = harness.backend.add_running(REPLACEMENT_BUNDLE_ID);
    harness.start().expect("start");

    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    harness.wait_message("replacement com.example.player is already running");
    assert!(harness.backend.open_requests().is_empty());
    assert_eq!(harness.backend.termination_requests(), vec![music]);

    // The instance was decided: a duplicate notification must not re-decide it.
    // An unrelated launch is a FIFO marker whose own log proves the duplicate
    // above was handled first.
    let _ = harness.backend.launch(music);
    let marker = harness.backend.add_running("com.tinyspeck.slackmacgap");
    let _ = harness.backend.launch(marker);
    harness.wait_message("ignoring the launch of unrelated application com.tinyspeck.slackmacgap");

    assert_eq!(
        harness
            .messages()
            .iter()
            .filter(|message| message.contains("replacement com.example.player is already running"))
            .count(),
        1,
        "a decided instance must not be re-decided: {:?}",
        harness.messages()
    );
    assert!(harness.backend.open_requests().is_empty());
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn an_unresolvable_path_target_warns_and_keeps_running() {
    let harness = Harness::new(path_target());
    harness.backend.set_bundle_identifier_at_path(REPLACEMENT_PATH, None);
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);

    harness.wait_warning("no application bundle at /Applications/Player.app");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert!(harness.backend.open_requests().is_empty());
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn an_unresolvable_bundle_target_warns_and_keeps_running() {
    let harness = Harness::new(bundle_target());
    // The identifier is known, but no installed application claims it.
    harness
        .backend
        .set_bundle_identifier_at_path(REPLACEMENT_PATH, Some(REPLACEMENT_BUNDLE_ID));
    harness.backend.set_application_path(REPLACEMENT_BUNDLE_ID, None);
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);

    harness.wait_warning("com.example.player is not installed");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert!(harness.backend.open_requests().is_empty());
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_failed_path_resolution_query_warns_without_opening() {
    let harness = Harness::new(path_target());
    harness.register_replacements();
    harness.start().expect("start");
    // The worker parks inside the termination send, so the injected failure is
    // consumed by the next query: the path's bundle-identifier lookup.
    let barrier = harness.backend.block_next_terminate_issuance();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    assert!(barrier.wait_entered(WAIT));
    harness.backend.fail_next_query();
    assert!(barrier.release());

    harness.wait_warning("cannot read the bundle identifier at /Applications/Player.app");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert!(harness.backend.open_requests().is_empty());
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_failed_open_path_query_warns_without_opening() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    // The running-instance query is parked after its own preflight, so the
    // injected failure is consumed by the application-path lookup instead.
    let barrier = harness.backend.block_next_listing();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    assert!(barrier.wait_entered(WAIT));
    harness.backend.fail_next_query();
    assert!(barrier.release());

    harness.wait_warning("cannot resolve the application path for com.example.player");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert!(harness.backend.open_requests().is_empty());
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_query_failure_is_reported_without_terminating_or_opening() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.backend.fail_next_query();
    let _ = harness.backend.launch(music);

    harness.wait_warning("cannot identify a launched application");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert!(harness.backend.termination_requests().is_empty());
    assert!(harness.backend.open_requests().is_empty());
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_rejected_main_dispatch_warns_and_keeps_the_runtime_running() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.executor.reject(true);
    let _ = harness.backend.launch(music);

    harness.wait_warning("cannot identify a launched application");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert!(harness.backend.termination_requests().is_empty());
    harness.executor.reject(false);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_termination_refusal_still_attempts_the_replacement_and_logs_in_order() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.backend.refuse_terminate();
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);

    wait_until("the replacement open", || {
        harness.backend.open_requests().len() == 1
    });
    let messages = harness.messages();
    let refusal = messages
        .iter()
        .position(|message| message.contains("refused to terminate"))
        .expect("a refusal warning");
    let opening = messages
        .iter()
        .position(|message| message.contains("requested opening replacement"))
        .expect("an opening log");
    assert!(refusal < opening, "{messages:?}");
    assert_eq!(harness.backend.open_requests(), vec![PathBuf::from(
        REPLACEMENT_PATH
    )]);
    assert!(
        harness.backend.termination_requests().is_empty(),
        "a refused request was never accepted"
    );
    assert!(
        !messages.iter().any(|message| message.contains("terminated com.apple.Music")),
        "request acceptance is not confirmed exit: {messages:?}"
    );
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn an_open_failure_warns_and_leaves_the_runtime_running() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.backend.fail_next_open();
    let _ = harness.backend.launch(music);

    harness.wait_warning("failed to open replacement com.example.player");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert!(harness.backend.open_requests().is_empty());
    assert_eq!(harness.backend.pending_opens(), 0);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn an_open_completion_failure_is_reported_truthfully() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    wait_until("the open request", || harness.backend.pending_opens() == 1);

    assert!(harness.backend.complete_open(0, Err("injected open failure".to_owned())));
    harness.wait_warning("replacement open request 1 failed");
    assert_eq!(harness.status(), ModuleStatus::Running);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn duplicate_notifications_and_sweep_overlap_open_one_replacement() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.backend.complete_opens_synchronously();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.start().expect("start");

    let barrier = harness.backend.block_next_open();
    let _ = harness.backend.launch(music);
    assert!(barrier.wait_entered(WAIT));
    let _ = harness.backend.launch(music);
    assert!(barrier.release());

    // The first completion clears the reservation; a later instance opens once.
    harness.wait_message("replacement open request 1 succeeded");
    let second = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(second);
    wait_until("two opens", || harness.backend.open_requests().len() == 2);
    assert_eq!(
        harness.backend.termination_requests(),
        vec![music, second],
        "the sweep and the duplicate notification terminate each instance once"
    );
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_second_blocked_instance_waits_for_the_pending_replacement() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.backend.complete_opens_synchronously();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let itunes = harness.backend.add_running(ITUNES_BUNDLE_ID);
    harness.start().expect("start");

    let barrier = harness.backend.block_next_open();
    let _ = harness.backend.launch(music);
    assert!(barrier.wait_entered(WAIT));
    let _ = harness.backend.launch(itunes);
    assert!(barrier.release());

    harness.wait_message("replacement open request 1 succeeded");
    let third = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(third);
    wait_until("two opens", || harness.backend.open_requests().len() == 2);
    assert_eq!(
        harness.backend.termination_requests(),
        vec![music, itunes, third],
        "both blocked instances are terminated, but only one open is in flight"
    );
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_notification_during_installation_is_queued_and_opens_once() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.backend.launch_during_next_observe(music);
    harness.start().expect("start");

    wait_until("the replacement open", || {
        harness.backend.open_requests().len() == 1
    });
    assert_eq!(harness.backend.termination_requests(), vec![music]);
    assert_eq!(harness.backend.open_requests(), vec![PathBuf::from(
        REPLACEMENT_PATH
    )]);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_captured_notification_delivered_after_stop_is_inert() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let captured = harness.backend.capture_launch(music).expect("capture");

    harness.registry.pause(harness.handle).expect("pause");
    assert!(
        !captured.deliver(),
        "the removed generation rejects the delivery"
    );
    harness.registry.resume(harness.handle).expect("resume");

    let _ = harness.backend.launch(music);
    wait_until("the successor open", || {
        harness.backend.open_requests().len() == 1
    });
    assert_eq!(harness.backend.termination_requests(), vec![music]);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn an_already_issued_open_completion_after_stop_cannot_change_a_successor() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    wait_until("the open", || harness.backend.pending_opens() == 1);

    harness.registry.pause(harness.handle).expect("pause");
    assert_eq!(harness.backend.observations(), 0);
    assert!(harness.backend.complete_open(0, Ok(())));
    harness.registry.resume(harness.handle).expect("resume");
    assert_eq!(harness.backend.observations(), 1);

    let second = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(second);
    wait_until("the successor open", || {
        harness.backend.open_requests().len() == 2
    });
    assert_eq!(
        harness.backend.termination_requests(),
        vec![music, music, second],
        "the successor termination is its own generation; an issued open is\n         never undone by terminating the replacement"
    );
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_target_change_applies_live_without_a_new_observation_or_sweep() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    assert_eq!(harness.backend.observations(), 1);

    let report = harness.registry.reload(harness.handle, path_target()).expect("reload");
    assert!(matches!(report.status, ReloadStatus::Applied { .. }));
    assert_eq!(harness.backend.observations(), 1);
    assert!(
        harness.backend.termination_requests().is_empty(),
        "reconfigure never sweeps"
    );

    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    wait_until("the path open", || harness.backend.open_requests().len() == 1);
    assert_eq!(harness.backend.open_requests(), vec![PathBuf::from(
        REPLACEMENT_PATH
    )]);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_live_reconfigure_walks_path_bundle_identifier_and_absent_targets() {
    let harness = Harness::new(path_target());
    harness.register_replacements();
    harness.backend.complete_opens_synchronously();
    harness.start().expect("start");

    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    wait_until("the path open", || harness.backend.open_requests().len() == 1);
    assert_eq!(
        harness.backend.open_requests(),
        vec![PathBuf::from(REPLACEMENT_PATH)],
        "the path target opens exactly that path"
    );

    harness
        .registry
        .reload(harness.handle, other_bundle_target())
        .expect("reload to a bundle identifier");
    let second = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(second);
    wait_until("the bundle open", || harness.backend.open_requests().len() == 2);
    assert_eq!(
        harness.backend.open_requests(),
        vec![PathBuf::from(REPLACEMENT_PATH), PathBuf::from(OTHER_PATH)],
        "the bundle identifier target opens the path the workspace resolves"
    );

    harness
        .registry
        .reload(harness.handle, block_only())
        .expect("reload to block only");
    let third = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(third);
    harness.wait_message("no replacement application is configured");
    assert_eq!(
        harness.backend.open_requests().len(),
        2,
        "an absent target never opens a replacement"
    );

    harness
        .registry
        .reload(harness.handle, path_target())
        .expect("reload back to the path");
    let fourth = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(fourth);
    wait_until("the restored path open", || {
        harness.backend.open_requests().len() == 3
    });
    assert_eq!(
        harness.backend.open_requests().last(),
        Some(&PathBuf::from(REPLACEMENT_PATH))
    );
    assert_eq!(harness.backend.observations(), 1, "no resubscription");
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn an_unchanged_reload_is_a_no_op() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    let report = harness
        .registry
        .reload(harness.handle, bundle_target())
        .expect("unchanged reload");
    assert_eq!(report.status, ReloadStatus::Unchanged);
    assert_eq!(harness.backend.observations(), 1);
    assert!(harness.backend.termination_requests().is_empty());
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn an_unresolvable_path_under_a_superseded_revision_does_not_consume_the_instance() {
    let harness = Harness::new(path_target());
    harness.backend.set_bundle_identifier_at_path(REPLACEMENT_PATH, None);
    harness.register_replacement(OTHER_PATH, OTHER_BUNDLE_ID);
    harness.start().expect("start");

    let barrier = harness.backend.block_next_bundle_identifier_at_path();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    assert!(barrier.wait_entered(WAIT), "the path resolution is running");
    harness.registry.reload(harness.handle, other_bundle_target()).expect("reload");
    assert!(barrier.release());
    wait_until("the other open", || harness.backend.open_requests().len() == 1);
    assert_eq!(harness.backend.open_requests(), vec![PathBuf::from(OTHER_PATH)]);
    assert!(
        !harness
            .warnings()
            .iter()
            .any(|warning| warning.contains("no application bundle")),
        "a superseded path answer must not consume the instance: {:?}",
        harness.warnings()
    );
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_superseded_bundle_target_installed_path_does_not_consume_the_instance() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    // The accepted target's application is not installed.
    harness.backend.set_application_path(REPLACEMENT_BUNDLE_ID, None);
    harness.start().expect("start");

    let barrier = harness.backend.block_next_application_path();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    assert!(barrier.wait_entered(WAIT), "the installed-path query is running");
    harness.registry.reload(harness.handle, other_bundle_target()).expect("reload");
    assert!(barrier.release());

    wait_until("the other open", || harness.backend.open_requests().len() == 1);
    assert_eq!(harness.backend.open_requests(), vec![PathBuf::from(OTHER_PATH)]);
    assert!(
        !harness.warnings().iter().any(|warning| warning.contains("is not installed")),
        "a superseded installed-path answer must not consume the instance: {:?}",
        harness.warnings()
    );
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_superseded_bundle_target_installed_path_retries_into_an_absent_path_target() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.backend.set_application_path(REPLACEMENT_BUNDLE_ID, None);
    harness.backend.set_bundle_identifier_at_path(OTHER_PATH, None);
    harness.start().expect("start");

    let barrier = harness.backend.block_next_application_path();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    assert!(barrier.wait_entered(WAIT), "the installed-path query is running");
    harness
        .registry
        .reload(harness.handle, configured(true, Some(OTHER_PATH)))
        .expect("reload");
    assert!(barrier.release());

    harness.wait_warning("no application bundle at /Applications/Other Player.app");
    assert!(
        harness.backend.open_requests().is_empty(),
        "the superseded target must never open: {:?}",
        harness.backend.open_requests()
    );
    assert_eq!(
        harness
            .warnings()
            .iter()
            .filter(|warning| warning.contains("no application bundle"))
            .count(),
        1,
        "exactly one block-only decision for the new target: {:?}",
        harness.warnings()
    );
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_rejected_reconfigure_keeps_the_last_good_configuration_and_revision() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    let before = harness.registry.snapshot(harness.handle).expect("snapshot");

    let barrier = harness.backend.block_next_terminate_issuance();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    assert!(barrier.wait_entered(WAIT));
    let error = harness
        .registry
        .reload(harness.handle, path_target())
        .expect_err("the acceptance gate is stalled");
    assert!(matches!(error, RegistryError::Module(_)), "{error:?}");
    let after = harness.registry.snapshot(harness.handle).expect("snapshot");
    assert_eq!(after.status, ModuleStatus::Running);
    assert_eq!(after.revision, before.revision);
    assert!(barrier.release());

    // The old target is still current, so the issued policy opens its path.
    wait_until("the old target open", || {
        harness.backend.open_requests().len() == 1
    });
    assert_eq!(harness.backend.open_requests(), vec![PathBuf::from(
        REPLACEMENT_PATH
    )]);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn start_returns_without_waiting_for_a_stalled_native_send() {
    let (spawned_tx, spawned_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let spawned_tx = Mutex::new(spawned_tx);
    let release_rx = Mutex::new(release_rx);
    let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = spawned_tx.lock().expect("spawned sender").send(());
        let _ = release_rx.lock().expect("release receiver").recv();
    });
    let harness = Harness::build_with(
        bundle_target(),
        Duration::from_millis(50),
        None,
        None,
        Some(hook),
    );
    harness.register_replacements();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.backend.launch_during_next_observe(music);
    // The sweep cannot list the launched instance, so the worker issues the
    // termination that stalls inside the commit gate.
    harness.backend.fail_next_query();
    let barrier = harness.backend.block_next_terminate_issuance();

    let (done_tx, done_rx) = mpsc::channel();
    let registry = Arc::clone(&harness.registry);
    let start = std::thread::spawn(move || {
        let result = registry.start_all();
        let _ = done_tx.send(result);
    });
    spawned_rx.recv_timeout(WAIT).expect("the policy worker was spawned");
    assert!(
        barrier.wait_entered(WAIT),
        "the worker's native send holds the commit gate"
    );
    let _ = release_tx.send(());

    let outcome = done_rx.recv_timeout(WAIT);
    // Never leave the worker parked, even when the bounded read regressed.
    let _ = barrier.release();
    let outcome = outcome.expect("start must not wait for a stalled native send");
    start.join().expect("start thread");
    outcome.expect("start reports a truthful success while the gate was held");
    assert_eq!(harness.status(), ModuleStatus::Running);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_paused_reload_stages_the_next_start_target() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    harness.registry.pause(harness.handle).expect("pause");
    assert_eq!(harness.backend.observations(), 0);

    harness
        .registry
        .reload(harness.handle, other_bundle_target())
        .expect("paused reload");
    assert_eq!(harness.backend.observations(), 0);
    assert!(harness.backend.open_requests().is_empty());

    harness.registry.resume(harness.handle).expect("resume");
    assert_eq!(harness.backend.observations(), 1);
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    wait_until("the other open", || harness.backend.open_requests().len() == 1);
    assert_eq!(harness.backend.open_requests(), vec![PathBuf::from(OTHER_PATH)]);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn an_accepted_config_change_makes_a_queued_termination_reconsider_the_target() {
    let harness = Harness::new(path_target());
    harness.register_replacements();
    harness.start().expect("start");
    let barrier = harness.backend.block_next_terminate();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    assert!(barrier.wait_entered(WAIT));

    harness.registry.reload(harness.handle, block_only()).expect("reload");
    assert!(barrier.release());
    harness.wait_message("no replacement application is configured");

    assert_eq!(harness.backend.termination_requests(), vec![music]);
    assert!(
        harness.backend.open_requests().is_empty(),
        "the obsolete target must never open"
    );
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn an_accepted_config_change_makes_a_queued_open_use_the_new_target() {
    let harness = Harness::new(path_target());
    harness.register_replacements();
    harness.start().expect("start");
    let barrier = harness.backend.block_next_open();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    assert!(barrier.wait_entered(WAIT));

    harness.registry.reload(harness.handle, other_bundle_target()).expect("reload");
    assert!(barrier.release());
    wait_until("the other open", || harness.backend.open_requests().len() == 1);
    assert_eq!(
        harness.backend.open_requests(),
        vec![PathBuf::from(OTHER_PATH)],
        "the stale request is refused before issuance"
    );
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_stale_completion_does_not_clear_a_newer_pending_replacement() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    wait_until("the first open", || harness.backend.open_requests().len() == 1);

    harness.registry.reload(harness.handle, other_bundle_target()).expect("reload");
    let second = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(second);
    wait_until("the second open", || harness.backend.open_requests().len() == 2);

    // The old completion is stale; the third instance must still see the newer
    // pending reservation and must not open. FIFO: O1, L3, O2.
    assert!(harness.backend.complete_open(0, Ok(())));
    let third = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(third);
    assert!(harness.backend.complete_open(1, Ok(())));
    harness.wait_message("replacement open request 2 succeeded");

    assert_eq!(
        harness.backend.open_requests().len(),
        2,
        "a stale completion cannot erase the current pending replacement"
    );
    assert!(
        harness
            .messages()
            .iter()
            .any(|message| { message.contains("a replacement open is already pending") }),
        "{:?}",
        harness.messages()
    );
    assert_eq!(harness.status(), ModuleStatus::Running);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_synchronous_open_completion_clears_its_reservation() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.backend.complete_opens_synchronously();
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    wait_until("the first open", || harness.backend.open_requests().len() == 1);
    harness.wait_message("replacement open request 1 succeeded");

    let second = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(second);
    wait_until("the second open", || harness.backend.open_requests().len() == 2);
    assert_eq!(harness.backend.pending_opens(), 0);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_stop_during_a_queued_native_action_leaves_inert_work_and_can_be_retried() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);

    // Native work is now queued instead of running inline; the worker parks on
    // its query acknowledgement, so the stop acknowledgement is bounded.
    harness.executor.set_on_main(false);
    let _ = harness.backend.launch(music);
    let error = harness
        .registry
        .pause(harness.handle)
        .expect_err("the queued native action delays the cleanup acknowledgement");
    assert!(matches!(error, RegistryError::Module(_)), "{error:?}");
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    assert_eq!(
        harness.backend.observations(),
        1,
        "a failed removal keeps ownership for the retry"
    );

    harness.executor.set_on_main(true);
    harness.executor.drain();
    assert!(
        harness.backend.termination_requests().is_empty(),
        "the invalidated queued action never issued"
    );

    harness.registry.resume(harness.handle).expect("resume retries stop");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert_eq!(harness.backend.observations(), 1);
    assert_eq!(
        harness.backend.termination_requests(),
        vec![music],
        "the successor generation sweeps normally"
    );
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_failed_install_rolls_back_and_retry_starts_cleanly() {
    let harness = Harness::new(bundle_target());
    harness.backend.fail_next_observe();
    let error = harness.start().expect_err("observation failure");
    assert!(matches!(error, RegistryError::Module(_)), "{error:?}");
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    assert_eq!(harness.backend.observations(), 0);

    harness.start().expect("retry");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert_eq!(harness.backend.observations(), 1);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_failed_install_with_a_failed_cleanup_is_retried_by_the_sdk() {
    let harness = Harness::new(bundle_target());
    harness.backend.fail_next_observe_after_registration();
    harness.backend.fail_unobserves(3);
    let error = harness.start().expect_err("install and its cleanup fail");
    assert!(matches!(error, RegistryError::Module(_)), "{error:?}");
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    assert_eq!(harness.backend.observations(), 1);

    harness
        .registry
        .set_enabled(harness.handle, true)
        .expect("the retry cleans the owned generation and starts");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert_eq!(harness.backend.observations(), 1);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_failed_observer_removal_keeps_ownership_until_a_retry() {
    let harness = Harness::new(bundle_target());
    harness.start().expect("start");
    harness.backend.fail_next_unobserve();

    let error = harness.registry.pause(harness.handle).expect_err("observer removal failed");
    assert!(matches!(error, RegistryError::Module(_)), "{error:?}");
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    assert_eq!(harness.backend.observations(), 1);

    harness
        .registry
        .resume(harness.handle)
        .expect("resume retries stop before starting");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert_eq!(harness.backend.observations(), 1);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn the_descriptor_declares_nothing_and_a_permission_change_is_a_no_op() {
    let harness = Harness::new(bundle_target());
    let descriptors = harness.registry.descriptors();
    assert_eq!(descriptors.len(), 1);
    let descriptor = &descriptors[0];
    assert_eq!(descriptor.id, NoTunesConfig::ID);
    assert!(descriptor.provides.is_empty());
    assert!(descriptor.requires.is_empty());
    assert!(descriptor.permissions.is_empty());
    assert!(descriptor.executables.is_empty());
    assert!(descriptor.surfaces.is_empty());

    harness.start().expect("start");
    let observations = harness.backend.observations();
    harness
        .registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
        .expect("grant is a no-op");
    harness
        .registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
        .expect("denial is a no-op");
    assert_eq!(harness.backend.observations(), observations);
    assert_eq!(harness.status(), ModuleStatus::Running);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn sdk_shutdown_and_repeated_stop_leave_no_observer() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.start().expect("start");
    assert_eq!(harness.backend.observations(), 1);

    harness.registry.pause(harness.handle).expect("pause");
    harness.registry.pause(harness.handle).expect("repeated pause");
    assert_eq!(harness.backend.observations(), 0);
    assert_eq!(harness.backend.termination_requests(), vec![music]);

    harness.registry.shutdown().expect("shutdown");
    harness.registry.shutdown().expect("repeated shutdown");
    assert_eq!(harness.backend.observations(), 0);
}

#[test]
fn identity_is_not_pid_based_and_reuse_never_touches_an_unrelated_instance() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    let music = harness.backend.add_running_with_pid(MUSIC_BUNDLE_ID, 4242);
    harness.start().expect("start");
    harness.backend.terminate(music);

    let unrelated = harness.backend.add_running_with_pid("com.example.unrelated", 4242);
    let _ = harness.backend.launch(unrelated);
    let other = harness.backend.add_running_with_pid(MUSIC_BUNDLE_ID, 5151);
    let _ = harness.backend.launch(other);
    wait_until("the marker open", || harness.backend.open_requests().len() == 1);

    assert_eq!(
        harness.backend.termination_requests(),
        vec![music, other],
        "an instance with a reused PID is never addressed"
    );
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn an_exited_instance_is_retired_and_its_late_notification_is_ignored() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.backend.complete_opens_synchronously();
    harness.start().expect("start");
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    wait_until("the first open", || harness.backend.open_requests().len() == 1);
    harness.wait_message("replacement open request 1 succeeded");

    harness.backend.terminate(music);
    let marker = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(marker);
    wait_until("the marker open", || harness.backend.open_requests().len() == 2);

    let _ = harness.backend.launch(music);
    harness.wait_message("ignoring a launch notification for a retired application instance");
    assert_eq!(
        harness.backend.open_requests().len(),
        2,
        "a retired identity cannot open another replacement"
    );
    assert_eq!(harness.status(), ModuleStatus::Running);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn retirement_stays_bounded_across_many_exited_instances() {
    let harness = Harness::new(bundle_target());
    harness.register_replacements();
    harness.start().expect("start");
    let mut first = None;
    for _ in 0..300 {
        let key = harness.backend.add_running(MUSIC_BUNDLE_ID);
        if first.is_none() {
            first = Some(key);
        }
        let _ = harness.backend.launch(key);
        harness.backend.terminate(key);
    }
    let marker = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(marker);
    wait_until("the marker termination", || {
        harness.backend.termination_requests().contains(&marker)
    });
    assert_eq!(harness.status(), ModuleStatus::Running);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn the_absent_target_never_opens_even_from_a_queued_launch() {
    let harness = Harness::new(block_only());
    harness.start().expect("start");
    let music: FakeAppKey = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    harness.wait_message("no replacement application is configured");
    assert!(harness.backend.open_requests().is_empty());
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_sweep_query_failure_warns_without_failing_the_start() {
    let harness = Harness::new(bundle_target());
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.backend.fail_next_query();
    harness.start().expect("a sweep query failure does not fail the start");

    assert_eq!(harness.status(), ModuleStatus::Running);
    assert_eq!(harness.backend.observations(), 1);
    harness.wait_warning("failed to list running com.apple.Music instances");
    assert!(!harness.backend.termination_requests().contains(&music));

    // The healthy observation still blocks a later launch.
    let later = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(later);
    wait_until("the later termination", || {
        harness.backend.termination_requests().contains(&later)
    });
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_worker_cannot_read_the_old_target_between_acceptance_and_publication() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let entered_tx = Mutex::new(entered_tx);
    let release_rx = Mutex::new(release_rx);
    let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = entered_tx.lock().expect("entered sender").send(());
        let _ = release_rx.lock().expect("release receiver").recv();
    });
    let (probe_tx, probe_rx) = mpsc::channel();
    let harness = Harness::build(
        bundle_target(),
        Duration::from_millis(50),
        Some(hook),
        Some(probe_tx),
    );
    harness.register_replacements();
    harness.start().expect("start");
    let snapshot = probe_rx.recv_timeout(WAIT).expect("the running snapshot");

    let registry = Arc::clone(&harness.registry);
    let handle = harness.handle;
    let reload = std::thread::spawn(move || registry.reload(handle, other_bundle_target()));
    entered_rx.recv_timeout(WAIT).expect("the acceptance gate entered");
    assert!(
        snapshot.try_lock().is_err(),
        "acceptance and publication must be one step: a worker cannot read the old snapshot after the revision was accepted"
    );

    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    let _ = release_tx.send(());
    reload.join().expect("reload thread").expect("reload");

    wait_until("the other open", || harness.backend.open_requests().len() == 1);
    assert_eq!(harness.backend.open_requests(), vec![PathBuf::from(OTHER_PATH)]);
    assert_eq!(
        harness.backend.terminate_attempts(),
        1,
        "the worker must commit the accepted target on its first attempt instead of spending stale retries"
    );
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_queued_config_event_cannot_clear_a_pending_open_for_the_current_config() {
    let mut harness = WorkerHarness::new(bundle_target());
    harness.backend.add_installed(REPLACEMENT_PATH, REPLACEMENT_BUNDLE_ID);
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.launch(music);
    assert_eq!(harness.backend.open_requests().len(), 1);

    let itunes = harness.backend.add_running(ITUNES_BUNDLE_ID);
    harness.launch(itunes);
    assert_eq!(
        harness.backend.open_requests().len(),
        1,
        "a second blocked instance joins the pending replacement"
    );

    // An older config event was queued while the worker was busy. Its carried
    // revision is not authoritative: the worker must re-read the snapshot.
    harness.worker.handle(Inbox::ConfigChanged);
    let third = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.launch(third);
    assert_eq!(
        harness.backend.open_requests().len(),
        1,
        "an old config event cannot clear the current reservation"
    );

    assert!(harness.backend.complete_open(0, Ok(())));
    harness.run();
    harness.launch(itunes);
    assert_eq!(
        harness.backend.open_requests().len(),
        1,
        "a duplicate of the joined instance still cannot open a second player"
    );
}

#[test]
fn a_join_decision_suppresses_a_duplicate_after_a_success_or_failure() {
    for result in [Ok(()), Err("injected open failure".to_owned())] {
        let mut harness = WorkerHarness::new(bundle_target());
        harness.backend.add_installed(REPLACEMENT_PATH, REPLACEMENT_BUNDLE_ID);
        let first = harness.backend.add_running(MUSIC_BUNDLE_ID);
        harness.launch(first);
        let joined = harness.backend.add_running(ITUNES_BUNDLE_ID);
        harness.launch(joined);
        assert_eq!(harness.backend.open_requests().len(), 1);

        assert!(harness.backend.complete_open(0, result));
        harness.run();
        harness.launch(joined);
        assert_eq!(
            harness.backend.open_requests().len(),
            1,
            "the joined instance was already decided; its duplicate must not open"
        );
        assert_eq!(
            harness.backend.terminate_attempts(),
            2,
            "each blocked instance still gets at most one termination attempt"
        );
    }
}

#[test]
fn a_config_change_during_the_replacement_query_does_not_consume_the_instance() {
    let harness = Harness::new(path_target());
    harness.register_replacements();
    harness.start().expect("start");
    let barrier = harness.backend.block_next_listing();
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(music);
    assert!(barrier.wait_entered(WAIT), "the replacement query is running");

    harness.registry.reload(harness.handle, other_bundle_target()).expect("reload");
    assert!(barrier.release());

    wait_until("the other open", || harness.backend.open_requests().len() == 1);
    assert_eq!(
        harness.backend.open_requests(),
        vec![PathBuf::from(OTHER_PATH)],
        "the obsolete path decision must not consume the instance"
    );
    assert_eq!(harness.backend.terminate_attempts(), 1);
    harness.registry.shutdown().expect("shutdown");
}

#[test]
fn a_completion_queued_across_a_reconfigure_cannot_erase_a_newer_pending_open() {
    let mut harness = WorkerHarness::new(bundle_target());
    harness.backend.add_installed(REPLACEMENT_PATH, REPLACEMENT_BUNDLE_ID);
    harness.backend.add_installed(OTHER_PATH, OTHER_BUNDLE_ID);
    let music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.launch(music);
    assert!(harness.backend.complete_open(0, Ok(())));
    let queued = harness.next_event();

    // A newer accepted revision is published and opens its own replacement.
    let token = harness.observation.accept().expect("accept");
    {
        let mut snapshot = lock(&harness.snapshot);
        snapshot.config = other_bundle_target();
        snapshot.token = Some(token);
    }
    let second = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.launch(second);
    assert_eq!(harness.backend.open_requests().len(), 2);

    let baseline = harness.backend.is_running_calls();
    harness.worker.handle(queued);
    assert_eq!(
        harness.backend.is_running_calls(),
        baseline,
        "a superseded completion must not trigger retirement queries"
    );

    let third = harness.backend.add_running(MUSIC_BUNDLE_ID);
    harness.launch(third);
    assert_eq!(
        harness.backend.open_requests().len(),
        2,
        "the old completion must not erase the newer reservation"
    );
}

#[test]
fn retirement_stops_querying_after_cancellation() {
    let mut harness = WorkerHarness::new(block_only());
    for _ in 0..4 {
        let key = harness.backend.add_running(MUSIC_BUNDLE_ID);
        harness.launch(key);
    }
    let barrier = harness.backend.block_next_is_running();
    let baseline = harness.backend.is_running_calls();
    let cancellation = harness.cancellation.clone();
    std::thread::scope(|scope| {
        let worker = &mut harness.worker;
        let retirement = scope.spawn(move || worker.retire_exited());
        assert!(
            barrier.wait_entered(WAIT),
            "the first retirement query is running"
        );
        cancellation.cancel();
        assert!(barrier.release());
        retirement.join().expect("retirement thread");
    });
    assert_eq!(
        harness.backend.is_running_calls(),
        baseline + 1,
        "cancellation between queries must stop the rest of the retirement"
    );
}

#[test]
fn a_stop_during_the_initial_sweep_reports_a_failed_start_and_cleans_up() {
    let harness = Harness::new(bundle_target());
    let _music = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let barrier = harness.backend.block_next_listing();
    let registry = Arc::clone(&harness.registry);
    let start = std::thread::spawn(move || registry.start_all());
    assert!(barrier.wait_entered(WAIT), "the sweep is listing running apps");

    harness.control.stop().expect("stop invalidates and removes the observation");
    assert!(barrier.release());
    let error = start
        .join()
        .expect("start thread")
        .expect_err("start must not report Running on an invalidated observation");
    assert!(matches!(error, RegistryError::Module(_)), "{error:?}");
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    assert_eq!(harness.backend.observations(), 0);
    assert_eq!(
        harness.backend.list_requests(),
        vec![MUSIC_BUNDLE_ID.to_owned()],
        "the sweep must not continue into the second blocked app after stop"
    );
    assert!(
        harness.backend.termination_requests().is_empty(),
        "no invalidated termination may be issued"
    );
    harness.wait_warning("interrupted during the initial application sweep");

    harness.start().expect("a fresh start after the aborted one");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert_eq!(harness.backend.observations(), 1);
    let later = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(later);
    wait_until("the successor termination", || {
        harness.backend.termination_requests().contains(&later)
    });
}

#[test]
fn a_completed_install_with_a_lost_acknowledgement_is_retried_after_delayed_cleanup() {
    let harness = Harness::build(bundle_target(), Duration::from_millis(200), None, None);
    harness.executor.set_on_main(false);
    let observation = harness.backend.block_next_observe();
    harness.backend.fail_unobserves(1);
    let registry = Arc::clone(&harness.registry);
    let start = std::thread::spawn(move || registry.start_all());
    wait_until("the queued installation", || harness.executor.queued() == 1);

    let releaser = {
        let observation = Arc::clone(&observation);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            observation.release()
        })
    };
    assert!(
        harness.executor.drain_one(),
        "the registration completes natively while its acknowledgement cannot be consumed"
    );
    releaser.join().expect("releaser");
    let error = start.join().expect("start thread").expect_err("bounded start failure");
    assert!(matches!(error, RegistryError::Module(_)), "{error:?}");
    assert!(matches!(harness.status(), ModuleStatus::Failed(_)));
    assert_eq!(
        harness.backend.observations(),
        1,
        "the completed installation keeps an owned inert guard"
    );
    harness.wait_warning("failed to install the application observer");
    harness.wait_warning("observation cleanup also failed");

    let late = harness.backend.add_running(ITUNES_BUNDLE_ID);
    let stale = harness.backend.capture_launch(late).expect("the old observer delivery");

    harness.executor.set_on_main(true);
    assert!(harness.executor.drain() >= 1, "the delayed cleanup finally runs");
    assert_eq!(
        harness.backend.observations(),
        0,
        "the eventual cleanup removes only the old registration"
    );

    harness.registry.set_enabled(harness.handle, true).expect("retry");
    assert_eq!(harness.status(), ModuleStatus::Running);
    assert_eq!(harness.backend.observations(), 1);
    assert!(
        !stale.deliver(),
        "an old callback cannot reach the successor generation"
    );
    let later = harness.backend.add_running(MUSIC_BUNDLE_ID);
    let _ = harness.backend.launch(later);
    wait_until("the successor termination", || {
        harness.backend.termination_requests().contains(&later)
    });
    harness.registry.shutdown().expect("shutdown");
}
