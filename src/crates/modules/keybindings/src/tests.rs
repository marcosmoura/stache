//! KB-05-01..07: the Keybindings runtime over the actual registry and fakes.
//!
//! Nothing here registers a real hotkey, installs a real tap, runs `hidutil` or
//! spawns a real command. The "main thread" is a pump thread, the tap is the fake
//! input backend, `hidutil` and the journal are the fake Caps world and every
//! child is scripted.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use stache_macos::caps_remap::testing::SetFault;
use stache_macos::caps_remap::{OWNED, RestoreOutcome};
use stache_sdk::input::testing::FakeResponse;
use stache_sdk::input::{
    ControlAction, Disposition, InputEventKind, KeyCode, ModifierFlags, PressStream, RawInput,
    TapKind,
};
use stache_sdk::{
    DiscardLogger, EventBus, Module, ModuleHandle, ModuleStatus, Permission, PermissionError,
    PermissionProvider, PermissionStatus, PromptPolicy, Registry, ReloadStatus, UnavailableWindows,
};

use crate::caps::F18_KEY;
use crate::dispatch::{CAPACITY, Rejection};
use crate::status::{BindingState, KeybindingsRuntimeStatus, RemapPhase, RuntimeState, TapState};
use crate::test_support::{Rig, cmd, config, wait_until};
use crate::{KeybindingsConfig, RuntimeTiming};

const CMD: u32 = 0x0100;
const KEY_Q: u16 = 0x0c;
const KEY_W: u16 = 0x0d;
const KEY_E: u16 = 0x0e;
const S: u16 = 1;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A permission provider whose trust a test flips and whose prompts it counts.
struct Trust {
    trusted: AtomicBool,
    requests: AtomicUsize,
}

impl PermissionProvider for Trust {
    fn status(&self, _: Permission) -> PermissionStatus {
        if self.trusted.load(Ordering::SeqCst) {
            PermissionStatus::Granted
        } else {
            PermissionStatus::Denied
        }
    }

    fn request(&self, permission: Permission) -> Result<PermissionStatus, PermissionError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        Ok(self.status(permission))
    }
}

struct Host {
    rig: Rig,
    registry: Arc<Registry>,
    handle: ModuleHandle<KeybindingsConfig>,
    trust: Arc<Trust>,
}

impl Host {
    fn new(config: KeybindingsConfig) -> Self { Self::with(Rig::new(), config, true) }

    fn with(rig: Rig, config: KeybindingsConfig, trusted: bool) -> Self {
        let trust = Arc::new(Trust {
            trusted: AtomicBool::new(trusted),
            requests: AtomicUsize::new(0),
        });
        let registry = Arc::new(
            Registry::new(
                Arc::new(DiscardLogger),
                Arc::new(EventBus::new()),
                Arc::clone(&rig.executor) as Arc<dyn stache_sdk::MainThreadExecutor>,
                Arc::clone(&trust) as Arc<dyn PermissionProvider>,
                Arc::new(UnavailableWindows),
            )
            .with_input_service(Arc::clone(&rig.input)),
        );
        let handle = registry.register(rig.runtime.module(), config).expect("register");
        rig.runtime
            .install_accepted_watch(registry.accepted_watch(handle).expect("accepted watch"));
        Self { rig, registry, handle, trust }
    }

    fn started(config: KeybindingsConfig) -> Self {
        let host = Self::new(config);
        host.registry.start_all().expect("start");
        host
    }

    fn status(&self) -> ModuleStatus {
        self.registry.snapshot(self.handle).expect("snapshot").status
    }

    fn revision(&self) -> u64 { self.registry.snapshot(self.handle).expect("snapshot").revision }

    fn runtime(&self) -> KeybindingsRuntimeStatus { self.rig.runtime.snapshot().read() }

    /// Waits for the runtime to adopt the registry's accepted revision.
    fn adopted(&self) {
        let revision = self.revision().to_string();
        wait_until("the revision is adopted", || self.runtime().revision == revision);
    }

    fn reload(
        &self,
        config: KeybindingsConfig,
    ) -> stache_sdk::RegistryResult<stache_sdk::ReloadReport> {
        self.registry.reload(self.handle, config)
    }

    fn press(&self, key: u16) { self.rig.press_chord(key, CMD); }

    fn tags(&self) -> Vec<String> { self.rig.processes.tags() }
}

fn raw(kind: InputEventKind, key: u16) -> RawInput {
    RawInput {
        kind,
        stream: PressStream::Key(KeyCode(key)),
        flags: ModifierFlags::NONE,
        repeat: false,
        timestamp: Duration::ZERO,
        location: None,
    }
}

fn caps_chord(rig: &Rig, key: u16) -> Disposition {
    let service = &rig.input;
    service.dispatch(TapKind::Suppressing, raw(InputEventKind::KeyDown, F18_KEY.0));
    let disposition = service.dispatch(TapKind::Suppressing, raw(InputEventKind::KeyDown, key));
    service.dispatch(TapKind::Suppressing, raw(InputEventKind::KeyUp, key));
    service.dispatch(TapKind::Suppressing, raw(InputEventKind::KeyUp, F18_KEY.0));
    disposition
}

/// Holds the coordinator between a publication and its adoption.
struct Window {
    entered: Receiver<u64>,
    release: Sender<()>,
}

fn windowed_rig() -> (Rig, Window) {
    let (entered_tx, entered) = channel();
    let (release, release_rx) = channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let armed = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&armed);
    let observed: crate::runtime::ObservedHook = Arc::new(move |revision| {
        if gate.load(Ordering::SeqCst) {
            let _ = entered_tx.send(revision);
            let _ = release_rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
        }
    });
    let rig = Rig::with_timing(RuntimeTiming {
        poll: Duration::from_millis(5),
        observed: Some(observed),
        adopting: None,
    });
    // The gate arms after the first start so only reloads are held.
    ARMED.with(|cell| *cell.borrow_mut() = Some(armed));
    (rig, Window { entered, release })
}

/// Like [`windowed_rig`], but holds `flip` while it owns the operations lock.
fn adopting_rig() -> (Rig, Window, Arc<AtomicBool>) {
    let (entered_tx, entered) = channel();
    let (release, release_rx) = channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let armed = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&armed);
    let adopting: crate::runtime::ObservedHook = Arc::new(move |revision| {
        if gate.load(Ordering::SeqCst) {
            let _ = entered_tx.send(revision);
            let _ = release_rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
        }
    });
    let rig = Rig::with_timing(RuntimeTiming {
        poll: Duration::from_millis(5),
        observed: None,
        adopting: Some(adopting),
    });
    (rig, Window { entered, release }, armed)
}

thread_local! {
    static ARMED: std::cell::RefCell<Option<Arc<AtomicBool>>> = const { std::cell::RefCell::new(None) };
}

fn arm() {
    ARMED
        .with(|cell| cell.borrow().as_ref().expect("a windowed rig").store(true, Ordering::SeqCst));
}

// ---------------------------------------------------------------------------
// KB-05-01: inert construction, lifecycle and honest registration
// ---------------------------------------------------------------------------

#[test]
fn kb_05_01_construction_registration_and_disabled_are_inert() {
    let host = Host::new(config(false, &[
        ("Cmd+Q", &["quick"]),
        ("CapsLock+S", &["quick2"]),
    ]));
    assert_inert(&host);
    host.registry.start_all().expect("start_all");
    assert_eq!(host.status(), ModuleStatus::Disabled);
    assert_inert(&host);
    assert_eq!(host.runtime().state, RuntimeState::Inactive);
    assert_eq!(host.runtime().revision, "0");
    assert_eq!(host.rig.dispatcher.snapshot().tasks, 0, "no eager workers");
}

fn assert_inert(host: &Host) {
    assert!(host.rig.carbon.calls().is_empty(), "no Carbon call");
    assert_eq!(host.rig.backend.submit_calls(), 0, "no tap subscription");
    assert_eq!(
        (host.rig.world.sets(), host.rig.world.gets()),
        (0, 0),
        "no hidutil"
    );
    assert_eq!(host.rig.processes.count(), 0, "no process");
}

#[test]
fn kb_05_01_an_enabled_ordinary_configuration_registers_without_a_tap_or_remap() {
    let host = Host::started(config(true, &[("Cmd+Q", &["quick-q"])]));
    assert_eq!(host.status(), ModuleStatus::Running);
    let status = host.runtime();
    assert_eq!(status.state, RuntimeState::Active);
    assert_eq!((status.active, status.rejected), (1, 0));
    assert_eq!(status.tap, TapState::NotRequired);
    assert_eq!(status.remap.phase, RemapPhase::Inactive);
    assert_eq!(status.revision, host.revision().to_string());
    assert_eq!(host.rig.carbon.registered_chords(), [(KEY_Q, CMD)]);
    assert_eq!(
        host.rig.backend.submit_calls(),
        0,
        "ordinary shortcuts need no tap"
    );
    assert_eq!(host.rig.world.sets(), 0);
    host.press(KEY_Q);
    wait_until("the command", || host.tags() == ["quick-q"]);
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_01_partial_registration_reports_the_exact_active_and_rejected_subset() {
    let rig = Rig::new();
    rig.carbon.occupy(KEY_W, CMD);
    let host = Host::with(
        rig,
        config(true, &[
            ("Cmd+Q", &["quick"]),
            ("Cmd+W", &["quick"]),
            ("Bogus+K", &["quick"]),
            ("Cmd+MediaPlayPause", &["quick"]),
        ]),
        true,
    );
    host.registry.start_all().expect("partial success starts");
    assert_eq!(host.status(), ModuleStatus::Running);
    let status = host.runtime();
    assert_eq!(status.state, RuntimeState::Degraded);
    assert_eq!((status.active, status.rejected), (1, 3));
    let by_name = |name: &str| {
        status
            .bindings
            .iter()
            .find(|b| b.shortcut == name)
            .unwrap_or_else(|| panic!("{name}"))
    };
    assert_eq!(by_name("Cmd+Q").state, BindingState::Active);
    assert_eq!(by_name("Cmd+W").state, BindingState::Rejected);
    assert!(
        by_name("Cmd+W")
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("already registered"))
    );
    assert_eq!(by_name("Bogus+K").state, BindingState::Rejected);
    assert_eq!(by_name("Cmd+MediaPlayPause").state, BindingState::Rejected);
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_01_every_usable_binding_failing_is_a_start_failure_that_cleans_up() {
    let rig = Rig::new();
    rig.carbon.occupy(KEY_Q, CMD);
    rig.carbon.occupy(KEY_W, CMD);
    let host = Host::with(
        rig,
        config(true, &[("Cmd+Q", &["quick"]), ("Cmd+W", &["quick"])]),
        true,
    );
    assert!(host.registry.start_all().is_err());
    assert!(
        matches!(host.status(), ModuleStatus::Failed(_)),
        "{:?}",
        host.status()
    );
    assert!(host.rig.carbon.registered_chords().is_empty());
    assert!(!host.rig.carbon.handler_installed(), "the handler is released");
    assert_eq!(host.runtime().state, RuntimeState::Inactive);
    assert_eq!(host.rig.processes.count(), 0);
}

#[test]
fn kb_05_01_an_intentionally_empty_or_all_invalid_map_is_valid_and_inactive() {
    let empty = Host::started(config(true, &[]));
    assert_eq!(empty.status(), ModuleStatus::Running);
    assert_eq!(empty.runtime().state, RuntimeState::Inactive);
    assert!(empty.rig.carbon.calls().is_empty());
    assert_eq!(empty.rig.backend.submit_calls(), 0);

    let invalid = Host::started(config(true, &[("Bogus+K", &["quick"])]));
    assert_eq!(invalid.status(), ModuleStatus::Running);
    let status = invalid.runtime();
    assert_eq!((status.active, status.rejected), (0, 1));
    assert!(
        invalid.rig.carbon.calls().is_empty(),
        "nothing usable, nothing native"
    );
}

#[test]
fn kb_05_01_pause_resume_and_retry_release_and_reacquire_the_resources() {
    let host = Host::started(config(true, &[("Cmd+Q", &["quick"])]));
    host.registry.pause(host.handle).expect("pause");
    assert_eq!(host.status(), ModuleStatus::Paused);
    assert!(host.rig.carbon.registered_chords().is_empty());
    assert!(!host.rig.carbon.handler_installed());
    assert_eq!(host.runtime().state, RuntimeState::Inactive);
    host.registry.resume(host.handle).expect("resume");
    assert_eq!(host.status(), ModuleStatus::Running);
    assert_eq!(host.rig.carbon.registered_chords(), [(KEY_Q, CMD)]);
    host.registry.shutdown().expect("shutdown");

    // Retry: a failed install is repaired by enabling again.
    let rig = Rig::new();
    rig.carbon.fail_install(Some(-1));
    let host = Host::with(rig, config(true, &[("Cmd+Q", &["quick"])]), true);
    assert!(host.registry.start_all().is_err());
    assert!(matches!(host.status(), ModuleStatus::Failed(_)));
    host.rig.carbon.fail_install(None);
    host.registry.set_enabled(host.handle, true).expect("retry");
    assert_eq!(host.status(), ModuleStatus::Running);
    assert_eq!(host.runtime().state, RuntimeState::Active);
    host.registry.shutdown().expect("shutdown");
}

// ---------------------------------------------------------------------------
// KB-05-04: permissions
// ---------------------------------------------------------------------------

#[test]
fn kb_05_04_only_caps_bindings_require_user_only_input_trust() {
    let module = Rig::new().runtime.module();
    assert!(module.required_permissions(&config(true, &[("Cmd+Q", &["x"])])).is_empty());
    assert!(module.required_permissions(&config(true, &[("Cmd+§", &["x"])])).is_empty());
    assert!(module.required_permissions(&config(true, &[])).is_empty());
    let caps = module.required_permissions(&config(true, &[("CapsLock+S", &["x"])]));
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0].permission(), Permission::Accessibility);
    assert_eq!(caps[0].prompt(), PromptPolicy::UserOnly);
    assert!(!caps[0].may_auto_prompt());
    // A misspelled Caps binding is not a Caps binding.
    assert!(module.required_permissions(&config(true, &[("CapsLock", &["x"])])).is_empty());
}

#[test]
fn kb_05_04_caps_without_trust_waits_and_never_prompts_or_touches_caps() {
    let host = Host::with(Rig::new(), config(true, &[("CapsLock+S", &["quick"])]), false);
    host.registry.start_all().expect("waiting is not a failure");
    assert_eq!(
        host.status(),
        ModuleStatus::WaitingForPermission(Permission::Accessibility)
    );
    assert_inert(&host);
    assert_eq!(
        host.trust.requests.load(Ordering::SeqCst),
        0,
        "no automatic prompt"
    );
    // Granting starts it.
    host.trust.trusted.store(true, Ordering::SeqCst);
    host.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
        .expect("grant");
    assert_eq!(host.status(), ModuleStatus::Running);
    assert_eq!(host.runtime().remap.phase, RemapPhase::Owned);
    // Revocation stops all Caps activity before any reacquisition.
    host.trust.trusted.store(false, Ordering::SeqCst);
    host.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
        .expect("revoke");
    assert_eq!(
        host.status(),
        ModuleStatus::WaitingForPermission(Permission::Accessibility)
    );
    assert_eq!(host.rig.world.mappings(), Vec::new(), "the remap was restored");
    assert_eq!(host.runtime().remap.phase, RemapPhase::Inactive);
    assert_eq!(host.trust.requests.load(Ordering::SeqCst), 0);
}

#[test]
fn kb_05_04_a_trusted_but_failed_tap_is_a_start_failure_not_waiting() {
    let rig = Rig::new();
    rig.backend.script(
        TapKind::Suppressing,
        ControlAction::Install,
        FakeResponse::Fail("no tap".to_owned()),
    );
    let host = Host::with(rig, config(true, &[("CapsLock+S", &["quick"])]), true);
    assert!(host.registry.start_all().is_err());
    assert!(
        matches!(host.status(), ModuleStatus::Failed(_)),
        "{:?}",
        host.status()
    );
    assert_eq!(host.rig.world.sets(), 0, "no remap without a confirmed tap");
}

// ---------------------------------------------------------------------------
// KB-05-03: accepted reload
// ---------------------------------------------------------------------------

#[test]
fn kb_05_03_nothing_executes_between_publication_and_observation() {
    let (rig, window) = windowed_rig();
    let host = Host::with(
        rig,
        config(true, &[("Cmd+Q", &["quick-old"]), ("Cmd+E", &["quick-e"])]),
        true,
    );
    host.registry.start_all().expect("start");
    arm();
    let removed_id = host.rig.carbon.id_of(KEY_E, CMD).expect("registered");
    let report = host
        .reload(config(true, &[
            ("Cmd+Q", &["quick-new-q"]),
            ("Cmd+W", &["quick-new-w"]),
        ]))
        .expect("accepted");
    let ReloadStatus::Applied { revision } = report.status else {
        panic!("applied")
    };
    assert_eq!(
        window.entered.recv_timeout(Duration::from_secs(10)).expect("observed"),
        revision
    );
    // The candidate is staged inertly: the new chord is not registered and the
    // candidate has no route.
    assert_eq!(host.rig.carbon.registered_chords().len(), 2);
    assert!(host.rig.carbon.id_of(KEY_W, CMD).is_none());
    // Published but not adopted: neither binding set executes.
    host.press(KEY_Q);
    host.press(KEY_E);
    let before = host.runtime().dispatch;
    assert_eq!(before.window_drops, 2);
    assert_eq!(before.submitted, 0);
    assert_eq!(host.rig.processes.count(), 0);
    window.release.send(()).expect("release");
    host.adopted();
    assert_eq!(
        host.rig.carbon.id_of(KEY_E, CMD),
        None,
        "the removed chord is unregistered"
    );
    host.press(KEY_Q);
    host.press(KEY_W);
    wait_until("both new commands", || host.tags().len() == 2);
    let mut tags = host.tags();
    tags.sort();
    assert_eq!(tags, ["quick-new-q", "quick-new-w"]);
    // A stale native ID of a retired registration never dispatches.
    let carbon = Arc::clone(&host.rig.carbon);
    host.rig.executor.run(move || carbon.press(removed_id));
    thread::sleep(Duration::from_millis(30));
    assert_eq!(host.rig.processes.count(), 2);
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_03_a_newer_staged_candidate_survives_observation_of_an_older_revision() {
    let (rig, window) = windowed_rig();
    let host = Host::with(rig, config(true, &[("Cmd+Q", &["quick-old"])]), true);
    host.registry.start_all().expect("start");
    arm();
    let first = host.reload(config(true, &[("Cmd+Q", &["quick-a"])])).expect("accepted");
    let ReloadStatus::Applied { revision } = first.status else {
        panic!("applied")
    };
    // The coordinator observed the first revision and is held before `flip`.
    assert_eq!(
        window.entered.recv_timeout(Duration::from_secs(10)).expect("observed"),
        revision
    );
    // A second accepted reload stages its candidate while the first is held.
    host.reload(config(true, &[("Cmd+W", &["quick-b"])])).expect("accepted");
    window.release.send(()).expect("release the older revision");
    window.release.send(()).expect("release the newer revision");
    host.adopted();
    assert!(
        host.rig.carbon.id_of(KEY_W, CMD).is_some(),
        "the newer chord is registered"
    );
    host.press(KEY_W);
    wait_until("the newer binding runs", || host.tags() == ["quick-b"]);
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_04_07_module_start_is_ordered_after_the_host_startup_recovery() {
    use stache_sdk::CancellationToken;

    use crate::caps::CapsRecoveryStep;

    let host = Host::new(config(true, &[("CapsLock+S", &["quick-caps"])]));
    // The composition arms the latch before the host can start any module.
    host.rig.remap.begin_startup_recovery();
    let starter = {
        let registry = Arc::clone(&host.registry);
        thread::spawn(move || registry.start_all())
    };
    // The module reached its confirmed tap and is waiting to apply.
    wait_until("the tap is active", || host.rig.backend.submit_calls() > 0);
    assert!(!starter.is_finished(), "the module start waits for the recovery");
    assert_eq!(
        host.rig.world.sets(),
        0,
        "nothing is mapped before the recovery ran"
    );
    let recovery =
        CapsRecoveryStep::new(Arc::clone(&host.rig.remap)).run(&CancellationToken::new());
    assert!(!recovery.restored(), "no stale journal: nothing to restore");
    starter.join().expect("start thread").expect("start");
    // The recovery ran first, so it did not restore the module's fresh journal.
    assert_eq!(host.runtime().remap.phase, RemapPhase::Owned);
    assert_eq!(host.rig.world.mappings(), vec![OWNED]);
    assert!(host.rig.remap.journal_present());
    assert_eq!(host.rig.world.sets(), 1, "exactly the module's own write");
    host.registry.shutdown().expect("shutdown");
    assert_eq!(host.rig.world.mappings(), Vec::new());
}

#[test]
fn kb_05_03_an_observed_revision_stops_old_binding_work() {
    let host = Host::started(config(true, &[("Cmd+Q", &["hold-a", "quick-after"])]));
    host.press(KEY_Q);
    wait_until("the first step", || host.tags() == ["hold-a"]);
    let generation = host.rig.dispatcher.generation();
    host.reload(config(true, &[("Cmd+Q", &["quick-b"])])).expect("accepted");
    host.adopted();
    assert!(
        host.rig.dispatcher.generation() > generation,
        "the old generation is invalidated"
    );
    wait_until("the child is killed", || {
        host.rig.processes.child("hold-a").killed()
    });
    wait_until("the cancellation is counted", || {
        host.rig.dispatcher.snapshot().cancelled == 1
    });
    assert!(
        !host.tags().contains(&"quick-after".to_owned()),
        "later steps never start"
    );
    host.press(KEY_Q);
    wait_until("the new command", || host.tags().contains(&"quick-b".to_owned()));
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_03_an_unchanged_reload_cancels_nothing() {
    let setup = config(true, &[("Cmd+Q", &["hold-a"])]);
    let host = Host::started(setup.clone());
    host.press(KEY_Q);
    wait_until("running", || host.tags() == ["hold-a"]);
    let generation = host.rig.dispatcher.generation();
    let revision = host.revision();
    let report = host.reload(setup).expect("unchanged");
    assert_eq!(report.status, ReloadStatus::Unchanged);
    assert_eq!(host.revision(), revision);
    assert_eq!(host.rig.dispatcher.generation(), generation);
    thread::sleep(Duration::from_millis(30));
    assert!(!host.rig.processes.child("hold-a").killed());
    assert_eq!(host.rig.dispatcher.snapshot().active, 1);
    host.rig.processes.finish_all();
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_03_a_rejected_candidate_keeps_the_last_good_state_and_queued_work() {
    let host = Host::started(config(true, &[("Cmd+Q", &["hold-a"])]));
    host.press(KEY_Q);
    wait_until("running", || host.tags() == ["hold-a"]);
    let generation = host.rig.dispatcher.generation();
    let revision = host.revision();
    let calls = host.rig.carbon.calls().len();
    // Staging fails: the main thread refuses the prepare hop.
    host.rig.executor.reject(true);
    let rejected = host.reload(config(true, &[("Cmd+W", &["quick"])]));
    host.rig.executor.reject(false);
    assert!(rejected.is_err(), "the candidate could not be staged");
    assert_eq!(host.revision(), revision, "nothing was published");
    assert_eq!(host.status(), ModuleStatus::Running);
    assert_eq!(host.rig.dispatcher.generation(), generation, "no invalidation");
    assert!(!host.rig.processes.child("hold-a").killed());
    assert_eq!(host.rig.carbon.calls().len(), calls, "no native change");
    host.press(KEY_Q);
    wait_until("the last-good route still works", || {
        host.rig.processes.count() == 2
    });
    host.rig.processes.finish_all();
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_03_a_candidate_with_no_usable_binding_is_rejected_before_publication() {
    let host = Host::started(config(true, &[("Cmd+Q", &["quick"])]));
    let revision = host.revision();
    let rejected = host.reload(config(true, &[("Cmd+MediaPlayPause", &["quick"])]));
    assert!(rejected.is_err());
    assert_eq!(host.revision(), revision);
    assert_eq!(
        host.runtime().state,
        RuntimeState::Active,
        "last-good stays active"
    );
    assert_eq!(host.rig.carbon.registered_chords(), [(KEY_Q, CMD)]);
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_03_caps_is_applied_only_after_the_revision_is_observed() {
    let (rig, window) = windowed_rig();
    let host = Host::with(rig, config(true, &[("Cmd+Q", &["quick"])]), true);
    host.registry.start_all().expect("start");
    arm();
    host.reload(config(true, &[
        ("Cmd+Q", &["quick"]),
        ("CapsLock+S", &["quick-caps"]),
    ]))
    .expect("accepted");
    window.entered.recv_timeout(Duration::from_secs(10)).expect("observed");
    assert_eq!(host.rig.world.sets(), 0, "staging never mutates Caps");
    assert_eq!(host.rig.backend.submit_calls(), 0, "no tap before the commit");
    window.release.send(()).expect("release");
    host.adopted();
    assert_eq!(host.runtime().remap.phase, RemapPhase::Owned);
    assert_eq!(host.runtime().tap, TapState::Ready);
    assert_eq!(host.rig.world.mappings(), vec![OWNED]);
    assert_eq!(caps_chord(&host.rig, S), Disposition::Suppress);
    wait_until("the Caps command", || host.tags() == ["quick-caps"]);
    host.registry.shutdown().expect("shutdown");
    assert_eq!(host.rig.world.mappings(), Vec::new(), "stop restores");
}

#[test]
fn kb_05_03_caps_removal_is_deferred_until_commit_and_never_by_a_rejected_candidate() {
    let (rig, window) = windowed_rig();
    let host = Host::with(
        rig,
        config(true, &[("Cmd+Q", &["quick"]), ("CapsLock+S", &["quick-caps"])]),
        true,
    );
    host.registry.start_all().expect("start");
    assert_eq!(host.rig.world.mappings(), vec![OWNED]);
    // A rejected removal candidate leaves the remap alone.
    host.rig.executor.reject(true);
    assert!(host.reload(config(true, &[("Cmd+Q", &["quick"])])).is_err());
    host.rig.executor.reject(false);
    assert_eq!(host.rig.world.mappings(), vec![OWNED]);
    assert_eq!(host.runtime().remap.phase, RemapPhase::Owned);

    arm();
    host.reload(config(true, &[("Cmd+Q", &["quick"])])).expect("accepted");
    window.entered.recv_timeout(Duration::from_secs(10)).expect("observed");
    assert_eq!(
        host.rig.world.mappings(),
        vec![OWNED],
        "destructive retirement waits for commit"
    );
    window.release.send(()).expect("release");
    host.adopted();
    assert_eq!(host.rig.world.mappings(), Vec::new());
    assert_eq!(host.runtime().remap.phase, RemapPhase::Inactive);
    assert_eq!(host.runtime().tap, TapState::NotRequired);
    assert!(
        !host.rig.backend.is_live(TapKind::Suppressing),
        "the tap is released"
    );
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_03_a_post_commit_caps_failure_is_an_accepted_degraded_configuration() {
    let host = Host::started(config(true, &[("Cmd+Q", &["quick"])]));
    host.rig.backend.script(
        TapKind::Suppressing,
        ControlAction::Install,
        FakeResponse::Fail("no tap".to_owned()),
    );
    let report = host
        .reload(config(true, &[
            ("Cmd+Q", &["quick"]),
            ("CapsLock+S", &["quick-caps"]),
        ]))
        .expect("the configuration is accepted, never rejected");
    assert!(matches!(report.status, ReloadStatus::Applied { .. }));
    assert!(report.start_error.is_none());
    host.adopted();
    assert_eq!(host.status(), ModuleStatus::Running);
    let status = host.runtime();
    assert_eq!(status.state, RuntimeState::Degraded);
    assert_eq!((status.active, status.rejected), (1, 1));
    let caps = status.bindings.iter().find(|b| b.shortcut == "CapsLock+S").expect("caps");
    assert_eq!(caps.state, BindingState::Rejected);
    assert_eq!(host.rig.world.sets(), 0);
    host.press(KEY_Q);
    wait_until("ordinary still works", || host.tags() == ["quick"]);
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_03_a_restore_failure_after_commit_is_visible_cleanup_not_a_rejection() {
    let host = Host::started(config(true, &[
        ("CapsLock+S", &["quick-caps"]),
        ("Cmd+Q", &["quick"]),
    ]));
    assert_eq!(host.runtime().remap.phase, RemapPhase::Owned);
    host.rig.world.script_set(SetFault::Exited);
    let report = host
        .reload(config(true, &[("Cmd+Q", &["quick"])]))
        .expect("the removal is accepted even though the restore failed");
    assert!(matches!(report.status, ReloadStatus::Applied { .. }));
    host.adopted();
    let status = host.runtime();
    assert_eq!(status.remap.phase, RemapPhase::CleanupPending);
    assert!(status.retained.caps_cleanup);
    assert_eq!(
        host.rig.world.mappings(),
        vec![OWNED],
        "our mapping is still pending"
    );
    assert_eq!(host.status(), ModuleStatus::Running);
    // The retained cleanup is retried by the next stop and then resolves.
    host.registry.shutdown().expect("the retry restores");
    assert_eq!(host.rig.world.mappings(), Vec::new());
    assert_eq!(host.runtime().remap.phase, RemapPhase::Inactive);
}

#[test]
fn kb_05_03_ordinary_to_caps_to_ordinary_follows_trust() {
    let host = Host::with(Rig::new(), config(true, &[("Cmd+Q", &["quick"])]), true);
    host.registry.start_all().expect("start");
    host.trust.trusted.store(false, Ordering::SeqCst);
    // Ordinary to Caps without trust: accepted, waiting, nothing active.
    let report = host
        .reload(config(true, &[
            ("Cmd+Q", &["quick"]),
            ("CapsLock+S", &["quick-caps"]),
        ]))
        .expect("accepted");
    assert!(matches!(report.status, ReloadStatus::Applied { .. }));
    assert_eq!(
        host.status(),
        ModuleStatus::WaitingForPermission(Permission::Accessibility)
    );
    assert!(
        host.rig.carbon.registered_chords().is_empty(),
        "the whole runtime waits"
    );
    assert_eq!(host.runtime().state, RuntimeState::Inactive);
    assert_eq!(host.rig.world.sets(), 0);
    // Grant: both start.
    host.trust.trusted.store(true, Ordering::SeqCst);
    host.registry
        .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
        .expect("grant");
    assert_eq!(host.status(), ModuleStatus::Running);
    assert_eq!(host.runtime().state, RuntimeState::Active);
    assert_eq!(host.runtime().remap.phase, RemapPhase::Owned);
    // Back to ordinary: Caps is released, the ordinary chord stays.
    host.reload(config(true, &[("Cmd+Q", &["quick"])])).expect("accepted");
    host.adopted();
    assert_eq!(host.runtime().remap.phase, RemapPhase::Inactive);
    assert_eq!(host.rig.world.mappings(), Vec::new());
    assert_eq!(host.rig.carbon.registered_chords(), [(KEY_Q, CMD)]);
    assert_eq!(
        host.trust.requests.load(Ordering::SeqCst),
        0,
        "never an automatic prompt"
    );
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_03_edits_while_paused_apply_on_resume_with_no_staging() {
    let host = Host::started(config(true, &[("Cmd+Q", &["quick"])]));
    host.registry.pause(host.handle).expect("pause");
    let calls = host.rig.carbon.calls().len();
    host.reload(config(true, &[("Cmd+W", &["quick-w"])]))
        .expect("accepted while paused");
    assert_eq!(host.status(), ModuleStatus::Paused);
    assert_eq!(
        host.rig.carbon.calls().len(),
        calls,
        "a paused edit touches nothing"
    );
    host.registry.resume(host.handle).expect("resume");
    assert_eq!(host.rig.carbon.registered_chords(), [(KEY_W, CMD)]);
    assert_eq!(host.runtime().revision, host.revision().to_string());
    host.press(KEY_W);
    wait_until("the edited command", || host.tags() == ["quick-w"]);
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_03_a_revision_without_staged_resources_leaves_every_route_inert() {
    // Disabling publishes a revision the coordinator may observe before the
    // registry stops the module; it must not adopt anything for it.
    let host = Host::started(config(true, &[("Cmd+Q", &["quick"])]));
    host.registry.set_enabled(host.handle, false).expect("disable");
    assert_eq!(host.status(), ModuleStatus::Disabled);
    assert!(host.rig.carbon.registered_chords().is_empty());
    host.press_missing();
}

impl Host {
    fn press_missing(&self) {
        assert!(self.rig.carbon.id_of(KEY_Q, CMD).is_none());
        assert_eq!(self.rig.processes.count(), 0);
    }
}

// ---------------------------------------------------------------------------
// KB-05-02: dispatcher wiring
// ---------------------------------------------------------------------------

#[test]
fn kb_05_02_a_full_dispatcher_makes_a_carbon_press_a_lost_invocation_never_run_late() {
    let host = Host::started(config(true, &[("Cmd+Q", &["target"])]));
    for index in 0..CAPACITY {
        host.rig.dispatcher.submit(&[cmd(&format!("h{index}"))]).expect("admitted");
    }
    host.press(KEY_Q);
    let status = host.runtime();
    assert_eq!(status.dispatch.lost, 1);
    assert_eq!(status.dispatch.overflow_drops, 1);
    assert_eq!(host.rig.dispatcher.submit(&[cmd("again")]), Err(Rejection::Full));
    host.rig.processes.finish_all();
    wait_until("drain", || {
        host.rig.processes.finish_all();
        let snapshot = host.rig.dispatcher.snapshot();
        snapshot.active == 0 && snapshot.waiting == 0
    });
    assert!(
        !host.tags().contains(&"target".to_owned()),
        "never delivered late"
    );
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_02_caps_chords_and_carbon_chords_share_one_bounded_dispatcher() {
    let host = Host::started(config(true, &[
        ("CapsLock+S", &["quick-caps"]),
        ("Cmd+Q", &["quick-carbon"]),
        ("Cmd+W", &[]),
    ]));
    assert_eq!(caps_chord(&host.rig, S), Disposition::Suppress);
    host.press(KEY_Q);
    host.press(KEY_W);
    wait_until("both commands", || host.tags().len() == 2);
    let status = host.runtime();
    assert_eq!(
        status.dispatch.submitted, 3,
        "the empty capture is accepted without a process"
    );
    assert_eq!(status.dispatch.lost, 0);
    assert_eq!(host.rig.processes.count(), 2);
    host.registry.shutdown().expect("shutdown");
}

// ---------------------------------------------------------------------------
// KB-05-05: status
// ---------------------------------------------------------------------------

#[test]
fn kb_05_05_reading_status_never_waits_for_native_work() {
    let host = Host::started(config(true, &[("Cmd+Q", &["quick"])]));
    let (entered_tx, entered) = channel();
    let (release_tx, release) = channel::<()>();
    let pump = Arc::clone(&host.rig.executor);
    let blocker = thread::spawn(move || {
        pump.run(move || {
            entered_tx.send(()).expect("entered");
            let _ = release.recv();
        });
    });
    entered.recv_timeout(Duration::from_secs(10)).expect("the main thread is busy");
    // The main thread is blocked in "native work"; a read still returns.
    let snapshot = host.rig.runtime.snapshot();
    let (done_tx, done) = channel();
    let reader = thread::spawn(move || done_tx.send(snapshot.read().state).expect("send"));
    assert_eq!(
        done.recv_timeout(Duration::from_secs(10)).expect("read"),
        RuntimeState::Active
    );
    reader.join().expect("reader");
    release_tx.send(()).expect("release");
    blocker.join().expect("blocker");
    host.registry.shutdown().expect("shutdown");
}

// ---------------------------------------------------------------------------
// KB-05-06: stop, retained cleanup, shutdown and independence
// ---------------------------------------------------------------------------

#[test]
fn kb_05_06_stop_invalidates_first_is_repeatable_and_successors_get_a_new_generation() {
    let host = Host::started(config(true, &[("Cmd+Q", &["hold-a"])]));
    host.press(KEY_Q);
    wait_until("running", || host.tags() == ["hold-a"]);
    let generation = host.rig.dispatcher.generation();
    host.registry.pause(host.handle).expect("pause");
    assert!(
        host.rig.processes.child("hold-a").killed(),
        "invalidated before cleanup returned"
    );
    assert!(host.rig.dispatcher.generation() > generation);
    host.registry.pause(host.handle).expect("a repeated stop is safe");
    host.registry.resume(host.handle).expect("resume");
    host.press(KEY_Q);
    wait_until("the successor runs", || host.rig.processes.count() == 2);
    host.rig.processes.finish_all();
    host.registry.shutdown().expect("shutdown");
    host.registry.shutdown().expect("repeated shutdown");
}

#[test]
fn kb_05_06_a_stop_during_adoption_invalidates_the_adopted_generation() {
    let (rig, window, armed) = adopting_rig();
    let host = Host::with(rig, config(true, &[("Cmd+Q", &["hold-a"])]), true);
    host.registry.start_all().expect("start");
    armed.store(true, Ordering::SeqCst);
    host.reload(config(true, &[("Cmd+W", &["hold-b"])])).expect("accepted");
    // `flip` holds the operations lock, before it invalidates.
    window.entered.recv_timeout(Duration::from_secs(10)).expect("flip is held");
    let held = host.rig.dispatcher.generation();
    let stopper = {
        let registry = Arc::clone(&host.registry);
        let handle = host.handle;
        thread::spawn(move || registry.pause(handle))
    };
    // The stop's early invalidation lands, then it blocks on the lock.
    wait_until("the stop invalidated", || {
        host.rig.dispatcher.generation() > held
    });
    window.release.send(()).expect("release");
    stopper.join().expect("stop thread").expect("pause");
    let installed = held + 2;
    assert!(
        host.rig.dispatcher.generation() > installed,
        "the generation `flip` installed was invalidated again"
    );
    assert_eq!(host.rig.router.route_count(), 0, "no route survives the stop");
    assert!(
        host.rig.dispatcher.submit_at(installed, &[cmd("late")]).is_err(),
        "nothing is admitted in the adopted generation"
    );
    assert_eq!(host.rig.processes.count(), 0);
    armed.store(false, Ordering::SeqCst);
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_06_retained_carbon_cleanup_is_bounded_visible_and_retried() {
    let host = Host::started(config(true, &[("Cmd+Q", &["quick"]), ("Cmd+W", &["quick"])]));
    host.rig.carbon.fail_unregisters(8);
    let stopped = host.registry.pause(host.handle);
    assert!(stopped.is_err(), "unresolved cleanup is reported");
    let status = host.runtime();
    assert!(!status.retained.hotkey_ids.is_empty(), "{status:?}");
    assert_eq!(status.state, RuntimeState::Inactive);
    // Presses of an inert, retained registration never dispatch.
    for id in host.rig.carbon.registered_ids() {
        let carbon = Arc::clone(&host.rig.carbon);
        host.rig.executor.run(move || carbon.press(id));
    }
    thread::sleep(Duration::from_millis(20));
    assert_eq!(host.rig.processes.count(), 0);
    // Another stop retries, and unrelated shutdown is never blocked.
    host.rig.carbon.fail_unregisters(0);
    host.registry.shutdown().expect("the retry resolves");
    assert!(host.rig.carbon.registered_chords().is_empty());
    assert!(host.runtime().retained.hotkey_ids.is_empty());
}

#[test]
fn kb_05_06_an_uncertain_caps_start_keeps_ownership_and_stop_retries_it() {
    let rig = Rig::new();
    let host = {
        rig.world.script_set(SetFault::TimedOutApplied { unconfirmed: false });
        Host::with(
            rig,
            config(true, &[("CapsLock+S", &["quick"]), ("Cmd+Q", &["quick"])]),
            true,
        )
    };
    host.registry
        .start_all()
        .expect("the ordinary chord keeps the module partially running");
    let status = host.runtime();
    assert_eq!(status.state, RuntimeState::Degraded);
    assert!(
        matches!(
            status.remap.phase,
            RemapPhase::Uncertain | RemapPhase::CleanupPending
        ),
        "{:?}",
        status.remap
    );
    // Stop resolves it under the lease (the fake world still holds our mapping).
    host.registry.shutdown().expect("stop restores the mapping");
    assert_eq!(host.rig.world.mappings(), Vec::new());
    assert!(matches!(
        host.rig.remap.restore(&stache_sdk::CancellationToken::new()),
        RestoreOutcome::NoJournal
    ));
}

#[test]
fn kb_05_06_shutdown_during_setup_cancels_the_wait_and_leaves_nothing_behind() {
    let rig = Rig::new();
    rig.backend
        .script(TapKind::Suppressing, ControlAction::Install, FakeResponse::Hold);
    let host = Arc::new(Host::with(
        rig,
        config(true, &[("CapsLock+S", &["quick"])]),
        true,
    ));
    let starter = {
        let host = Arc::clone(&host);
        thread::spawn(move || host.registry.start_all())
    };
    wait_until("the tap install is pending", || host.rig.backend.queued() > 0);
    host.registry.invalidate();
    let result = starter.join().expect("start thread");
    assert!(result.is_err(), "the start did not complete");
    host.registry.shutdown().expect("shutdown");
    assert_eq!(host.rig.world.sets(), 0, "Caps was never remapped");
    assert!(host.rig.carbon.registered_chords().is_empty());
}

#[test]
fn kb_05_06_host_shutdown_closes_admission_then_the_dispatcher() {
    let host = Host::started(config(true, &[("Cmd+Q", &["quick"])]));
    host.rig.runtime.begin_shutdown();
    host.press(KEY_Q);
    assert_eq!(host.rig.processes.count(), 0, "admission is closed");
    host.registry.shutdown().expect("shutdown");
    assert!(host.rig.runtime.finish_shutdown());
    assert_eq!(
        host.rig.dispatcher.submit(&[cmd("late")]),
        Err(Rejection::Closed)
    );
}

#[test]
fn kb_05_06_two_hosts_are_independent() {
    let first = Host::started(config(true, &[("Cmd+Q", &["quick-one"])]));
    let second = Host::started(config(true, &[("Cmd+Q", &["quick-two"])]));
    first.press(KEY_Q);
    wait_until("first", || first.tags() == ["quick-one"]);
    assert!(second.tags().is_empty());
    second.registry.pause(second.handle).expect("pause");
    first.press(KEY_Q);
    wait_until("first again", || first.tags().len() == 2);
    assert!(second.tags().is_empty());
    first.registry.shutdown().expect("shutdown");
    second.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_03_a_carbon_removal_failure_after_commit_is_retained_inert_and_visible() {
    let host = Host::started(config(true, &[("Cmd+Q", &["quick-q"]), ("Cmd+W", &["quick-w"])]));
    let retired = host.rig.carbon.id_of(KEY_W, CMD).expect("registered");
    host.rig.carbon.fail_unregisters(1);
    let report = host.reload(config(true, &[("Cmd+Q", &["quick-q2"])])).expect("accepted");
    assert!(matches!(report.status, ReloadStatus::Applied { .. }));
    host.adopted();
    let status = host.runtime();
    assert_eq!(status.state, RuntimeState::Degraded, "cleanup is owed");
    assert_eq!(status.retained.hotkey_ids, [retired]);
    assert_eq!((status.active, status.rejected), (1, 0));
    // The retired registration is inert; the new set runs.
    let carbon = Arc::clone(&host.rig.carbon);
    host.rig.executor.run(move || carbon.press(retired));
    host.press(KEY_Q);
    wait_until("the new command", || host.tags() == ["quick-q2"]);
    // The owed cleanup is retried and resolved by the next stop.
    host.registry.shutdown().expect("the retry resolves");
    assert!(host.rig.carbon.registered_chords().is_empty());
}

#[test]
fn kb_05_03_a_layout_switch_between_publication_and_observation_still_adopts_the_revision() {
    use stache_macos::hotkeys::testing::FakeLayout;

    let (rig, window) = windowed_rig();
    let host = Host::with(rig, config(true, &[("Cmd+Q", &["quick"])]), true);
    host.registry.start_all().expect("start");
    arm();
    host.reload(config(true, &[
        ("Cmd+Q", &["quick"]),
        ("Cmd+§", &["quick-section"]),
    ]))
    .expect("accepted");
    window.entered.recv_timeout(Duration::from_secs(10)).expect("observed");
    // The layout changes after the candidate was staged and before it is flipped.
    host.rig.carbon.set_layout(FakeLayout::german());
    let carbon = Arc::clone(&host.rig.carbon);
    host.rig.executor.run(move || carbon.fire_hint());
    window.release.send(()).expect("release");
    host.adopted();
    let status = host.runtime();
    assert_eq!((status.active, status.rejected), (2, 0), "{status:?}");
    assert_eq!(status.state, RuntimeState::Active);
    host.registry.shutdown().expect("shutdown");
}

#[test]
fn kb_05_06_a_blocked_main_thread_bounds_stop_instead_of_deadlocking() {
    let host = Host::started(config(true, &[("Cmd+Q", &["hold-a"])]));
    host.press(KEY_Q);
    wait_until("running", || host.tags() == ["hold-a"]);
    let (entered_tx, entered) = channel();
    let (release_tx, release) = channel::<()>();
    let pump = Arc::clone(&host.rig.executor);
    let blocker = thread::spawn(move || {
        pump.run(move || {
            entered_tx.send(()).expect("entered");
            let _ = release.recv();
        });
    });
    entered.recv_timeout(Duration::from_secs(10)).expect("the main thread is busy");
    // The stop never waits for a thread that needs main: it gives up at the
    // shared acknowledgement bound, reports the owed cleanup and has already
    // invalidated dispatch.
    let started = std::time::Instant::now();
    let stopped = host.registry.pause(host.handle);
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "bounded: {:?}",
        started.elapsed()
    );
    assert!(stopped.is_err(), "unconfirmed Carbon cleanup is reported");
    assert!(
        host.rig.processes.child("hold-a").killed(),
        "dispatch was invalidated first"
    );
    release_tx.send(()).expect("release");
    blocker.join().expect("blocker");
    // The late job ran inertly and a repeated stop resolves the cleanup.
    host.registry.pause(host.handle).expect("the retry resolves");
    assert!(host.rig.carbon.registered_chords().is_empty());
    host.registry.shutdown().expect("shutdown");
}
