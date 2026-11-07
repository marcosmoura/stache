//! Read-only Keep Awake status over the app-owned handler.
//!
//! Every test uses a temporary configuration directory and fake SDK modules.
//! Nothing starts a native assertion, lock probe, observer, window or real
//! production factory.

use std::fs;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use stache_app::bindings::{ModuleState, ModuleStatusView};
use stache_app::host::{FactoryContext, ModuleFactory, ProxyAudioFactory, register_module};
use stache_app::keep_awake::{KeepAwakeIpc, KeepAwakeStatus};
use stache_keep_awake::{
    AssertionState, KeepAwakeCondition, KeepAwakeConditionKind, KeepAwakeConfig,
    KeepAwakeRuntimeStatus, KeepAwakeSnapshot, LockObservation, LockState,
};
use stache_macos::audio::{
    AudioBackend, AudioChangeSubscription, AudioDevice, AudioDirection, AudioError, AudioWakeup,
};
use stache_sdk::ipc::{
    CoreHandler, CoreReload, CoreStatus, HandlerErrorKind, HandlerResult, IpcHandler, Request,
    Response, Router,
};
use stache_sdk::{
    ConfigCatalog, ConfigError, ConfigPaths, ConfigStore, DiscardLogger, ErrorCategory, EventBus,
    LogFilter, MainThreadExecutor, Module, ModuleConfig, ModuleContext, ModuleDescriptor,
    ModuleError, ModuleHandle, ModuleResult, Permission, PermissionError, PermissionProvider,
    PermissionStatus, Registry, ReloadOutcome, ThreadError,
};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
struct TestConfig {
    enabled: bool,
}

impl ModuleConfig for TestConfig {
    fn enabled(&self) -> bool { self.enabled }

    fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
}

struct FakeModule {
    fail_start: bool,
}

impl Module for FakeModule {
    type Config = TestConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: KeepAwakeConfig::ID,
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[],
        }
    }

    fn configure(&mut self, _: &TestConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        if self.fail_start {
            return Err(ModuleError::new(
                ctx.id(),
                stache_sdk::Operation::Start,
                ErrorCategory::Lifecycle,
                "injected start failure",
            ));
        }
        Ok(())
    }

    fn reconfigure(&mut self, _: &TestConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

    fn stop(&mut self, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }
}

struct NoMain;

impl MainThreadExecutor for NoMain {
    fn is_main_thread(&self) -> bool { false }

    fn execute(&self, _: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> { Ok(()) }
}

struct AllowAll;

impl PermissionProvider for AllowAll {
    fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

    fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(PermissionStatus::Granted)
    }
}

struct NoCore;

impl CoreHandler for NoCore {
    fn reload(&self) -> HandlerResult<CoreReload> { Ok(CoreReload { files: Vec::new() }) }

    fn status(&self) -> HandlerResult<CoreStatus> {
        Ok(CoreStatus {
            directory: std::path::PathBuf::new(),
            files: Vec::new(),
        })
    }
}

struct Fixture {
    #[expect(
        dead_code,
        reason = "keeps the temporary directory alive for the fixture"
    )]
    root: tempfile::TempDir,
    registry: Arc<Registry>,
    store: Arc<ConfigStore>,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("root");
    let registry = Arc::new(Registry::new(
        Arc::new(DiscardLogger),
        Arc::new(EventBus::new()),
        Arc::new(NoMain),
        Arc::new(AllowAll),
        Arc::new(stache_sdk::UnavailableWindows),
    ));
    let mut catalog = ConfigCatalog::new();
    catalog.register::<TestConfig>(KeepAwakeConfig::ID).expect("test config");
    let store = Arc::new(ConfigStore::new(
        Arc::clone(&registry),
        catalog,
        ConfigPaths::new(root.path().join("config")),
        Arc::new(LogFilter::default()),
    ));
    Fixture { root, registry, store }
}

impl Fixture {
    fn context(&self) -> FactoryContext {
        FactoryContext::new(Arc::clone(&self.registry), Arc::clone(&self.store))
    }

    fn write(&self, text: &str) {
        fs::create_dir_all(self.store.paths().directory()).expect("config dir");
        fs::write(self.store.paths().module_file(KeepAwakeConfig::ID), text).expect("write config");
    }

    fn register(&self, enabled: bool, fail_start: bool) -> ModuleHandle<TestConfig> {
        let handle = self
            .registry
            .register(FakeModule { fail_start }, TestConfig { enabled })
            .expect("register");
        self.store.bind(handle).expect("bind");
        handle
    }
}

fn handler(context: &FactoryContext) -> KeepAwakeIpc {
    KeepAwakeIpc::new(context.clone(), KeepAwakeSnapshot::default())
}

fn status_of(handler: &KeepAwakeIpc) -> KeepAwakeStatus {
    let value = handler.handle("status", Value::Null).expect("status");
    serde_json::from_value(value).expect("typed envelope")
}

#[test]
fn status_accepts_omitted_null_and_empty_object_params() {
    let fixture = fixture();
    fixture.register(false, false);
    let handler = handler(&fixture.context());
    let null = handler.handle("status", Value::Null).expect("null");
    let empty = handler.handle("status", json!({})).expect("empty object");
    assert_eq!(null, empty, "omitted, null and {{}} are the same request");

    let mut router = Router::new(Arc::new(NoCore));
    router
        .register(
            KeepAwakeConfig::ID.as_str(),
            Arc::new(KeepAwakeIpc::new(
                fixture.context(),
                KeepAwakeSnapshot::default(),
            )),
        )
        .expect("register");
    let response = router.route(Request::without_params(KeepAwakeConfig::ID.as_str(), "status"));
    let Response::Ok { data } = response else {
        panic!("status must route successfully: {response:?}");
    };
    assert_eq!(data, null);
}

#[test]
fn status_rejects_nonempty_objects_and_every_bad_parameter_type() {
    let fixture = fixture();
    fixture.register(false, false);
    let handler = handler(&fixture.context());
    for params in [
        json!({ "unexpected": true }),
        json!(""),
        json!("status"),
        json!(0),
        json!(1.5),
        json!(true),
        json!([]),
        json!([null]),
    ] {
        let error = handler.handle("status", params.clone()).expect_err("rejected params");
        assert_eq!(
            error.kind(),
            HandlerErrorKind::InvalidParams,
            "params {params} must not be accepted"
        );
    }
}

#[test]
fn unknown_methods_are_typed() {
    let fixture = fixture();
    fixture.register(false, false);
    let handler = handler(&fixture.context());
    for method in ["state", "enable", "Status"] {
        let error = handler.handle(method, Value::Null).expect_err("unknown method");
        assert_eq!(error.kind(), HandlerErrorKind::UnknownMethod, "{method}");
    }
}

#[test]
fn the_wire_envelope_round_trips_every_assertion_state() {
    for (state, text) in [
        (AssertionState::Inactive, "inactive"),
        (AssertionState::Acquiring, "acquiring"),
        (AssertionState::Active, "active"),
        (AssertionState::ReleasePending, "releasePending"),
    ] {
        let status = KeepAwakeStatus {
            module: ModuleStatusView {
                id: "keepAwake".to_owned(),
                state: ModuleState::Running,
                enabled: true,
                paused: false,
                revision: "4294967296".to_owned(),
                detail: None,
                diagnostic: Some("held".to_owned()),
                keybindings: None,
            },
            runtime: KeepAwakeRuntimeStatus {
                assertion_state: state,
                lock_state: LockState::Locked,
                observation: LockObservation::NotificationsAndPolling,
                conditions: vec![KeepAwakeCondition {
                    kind: KeepAwakeConditionKind::AssertionReleaseFailed,
                    message: "cleanup is unresolved".to_owned(),
                }],
            },
        };
        let value = serde_json::to_value(&status).expect("encode");
        assert_eq!(value["module"]["revision"], "4294967296");
        assert_eq!(value["runtime"]["assertionState"], text);
        let decoded: KeepAwakeStatus = serde_json::from_value(value).expect("decode");
        assert_eq!(decoded, status);
    }
}

#[test]
fn status_answers_in_disabled_paused_and_failed_lifecycle_states() {
    let disabled = fixture();
    disabled.register(false, false);
    let status = status_of(&handler(&disabled.context()));
    assert_eq!(status.module.state, ModuleState::Disabled);
    assert!(!status.module.enabled);
    assert_eq!(status.module.revision, "1");
    assert_eq!(status.runtime, KeepAwakeRuntimeStatus::default());

    let paused = fixture();
    let handle = paused.register(true, false);
    paused.registry.start_all().expect("start");
    paused.registry.pause(handle).expect("pause");
    let status = status_of(&handler(&paused.context()));
    assert_eq!(status.module.state, ModuleState::Paused);
    assert!(status.module.enabled);
    assert!(status.module.paused);
    assert_eq!(status.runtime, KeepAwakeRuntimeStatus::default());

    let failed = fixture();
    failed.register(true, true);
    assert!(
        failed.registry.start_all().is_err(),
        "the injected failure must surface"
    );
    let status = status_of(&handler(&failed.context()));
    assert_eq!(status.module.state, ModuleState::Failed);
    assert!(status.module.detail.is_some());
    assert_eq!(status.runtime, KeepAwakeRuntimeStatus::default());
}

#[test]
fn held_file_diagnostic_overrides_the_registry_diagnostic() {
    let fixture = fixture();
    let handle = fixture
        .registry
        .register_rejected(
            FakeModule { fail_start: false },
            stache_sdk::ConfigDiagnostic::new("registry says the config is invalid"),
        )
        .expect("register rejected");
    fixture.store.bind(handle).expect("bind");
    let context = fixture.context();

    let registry_only = status_of(&handler(&context));
    assert_eq!(
        registry_only.module.diagnostic.as_deref(),
        Some("registry says the config is invalid"),
        "without a held file diagnostic the snapshot diagnostic is the fallback"
    );

    fixture.write("{ \"enabled\": ");
    let outcome = fixture.store.reload_one(KeepAwakeConfig::ID).expect("reload rejected file");
    assert!(matches!(outcome, ReloadOutcome::Rejected(_)));
    let held = context.diagnostic(KeepAwakeConfig::ID).expect("held diagnostic").to_string();
    assert!(held.contains("keepAwake.jsonc"), "{held}");

    let overridden = status_of(&handler(&context));
    assert_eq!(overridden.module.diagnostic.as_deref(), Some(held.as_str()));
}

#[test]
fn an_identical_good_repair_clears_the_held_diagnostic() {
    let fixture = fixture();
    let good = "{ \"enabled\": true }\n";
    fixture.write(good);
    register_module(&fixture.registry, &fixture.store, FakeModule {
        fail_start: false,
    })
    .expect("register");
    let context = fixture.context();
    assert!(context.diagnostic(KeepAwakeConfig::ID).is_none());

    fixture.write("{ \"enabled\": ");
    let rejected = fixture.store.reload_one(KeepAwakeConfig::ID).expect("rejected reload");
    assert!(matches!(rejected, ReloadOutcome::Rejected(_)));
    assert!(context.diagnostic(KeepAwakeConfig::ID).is_some());
    assert!(status_of(&handler(&context)).module.diagnostic.is_some());

    fixture.write(good);
    let repaired = fixture.store.reload_one(KeepAwakeConfig::ID).expect("repair reload");
    assert!(
        matches!(repaired, ReloadOutcome::Unchanged),
        "identical last-good bytes do not churn the accepted revision"
    );
    assert!(context.diagnostic(KeepAwakeConfig::ID).is_none());
    assert!(
        status_of(&handler(&context)).module.diagnostic.is_none(),
        "a fresh status read sees the cleared diagnostic"
    );
    let snapshot = fixture
        .registry
        .snapshots()
        .into_iter()
        .find(|snapshot| snapshot.id == KeepAwakeConfig::ID)
        .expect("snapshot");
    assert_eq!(snapshot.revision, 1);
}

#[test]
fn querying_status_never_reads_disk_or_accepts_changed_bytes() {
    let fixture = fixture();
    fixture.write("{ \"enabled\": false }\n");
    register_module(&fixture.registry, &fixture.store, FakeModule {
        fail_start: false,
    })
    .expect("register");
    let context = fixture.context();
    let before = status_of(&handler(&context));
    assert_eq!(before.module.revision, "1");
    assert!(before.module.diagnostic.is_none());

    fs::remove_file(fixture.store.paths().module_file(KeepAwakeConfig::ID)).expect("delete");
    let deleted = status_of(&handler(&context));
    assert_eq!(
        deleted, before,
        "deleting the file without a reload changes nothing"
    );
    assert!(context.diagnostic(KeepAwakeConfig::ID).is_none());

    fixture.write("{ \"enabled\": true, \"unknown\": 1 }\n");
    let changed = status_of(&handler(&context));
    assert_eq!(changed, before, "changed bytes are not accepted by a query");
    assert!(context.diagnostic(KeepAwakeConfig::ID).is_none());
    assert!(fixture.store.diagnostics().is_empty());
}

#[test]
fn a_missing_registry_snapshot_is_an_internal_handler_error() {
    let fixture = fixture();
    let error = handler(&fixture.context())
        .handle("status", Value::Null)
        .expect_err("no registered module");
    assert_eq!(error.kind(), HandlerErrorKind::Handler);
    assert!(error.message().contains("keepAwake"), "{error}");
}

struct EmptyAudio;

impl AudioBackend for EmptyAudio {
    fn devices(&self) -> Vec<AudioDevice> { Vec::new() }

    fn default_device(&self, _: AudioDirection) -> Option<AudioDevice> { None }

    fn set_default_device(&self, _: AudioDirection, _: u32) -> Result<(), AudioError> {
        Err(AudioError::new("set default audio device", -1))
    }

    fn is_hdmi(&self, _: u32) -> bool { false }

    fn subscribe(&self, _: AudioWakeup) -> Result<Box<dyn AudioChangeSubscription>, AudioError> {
        Err(AudioError::new("add audio property listener", -1))
    }
}

#[test]
fn the_generic_context_feeds_proxy_audio_without_mutating_published_state() {
    let fixture = fixture();
    fixture.register(false, false);
    let context = fixture.context();
    let before = fixture.registry.snapshots();

    let factory = ProxyAudioFactory::new(Arc::new(EmptyAudio));
    let handlers = factory.ipc_handlers(&context);
    assert_eq!(handlers.len(), 1);
    assert_eq!(handlers[0].0, "proxyAudio");
    assert_eq!(
        handlers[0].1.handle("list", Value::Null).expect("list"),
        json!([])
    );
    assert_eq!(
        handlers[0]
            .1
            .handle("list", json!({ "kind": "output" }))
            .expect("filtered list"),
        json!([])
    );

    assert_eq!(
        fixture.registry.snapshots(),
        before,
        "building handlers and reading the context publishes nothing"
    );
    assert!(context.diagnostic(KeepAwakeConfig::ID).is_none());
}
