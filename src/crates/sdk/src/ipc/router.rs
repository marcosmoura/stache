//! Request routing and the reserved core namespace.
//!
//! A [`Router`] owns exactly one handler per module name. The SDK never
//! switches on a module identifier: modules contribute handlers from the
//! composition root, and `core` is the only reserved name.
//!
//! The router has no notion of the host's Accessibility gate. The host wraps
//! each contributed handler before registration so it answers `unavailable`
//! while the gate is closed, and its core service fills the additive `access`
//! field of [`CoreReload`] and [`CoreStatus`]; [`ConfigCore`] leaves it `None`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::{ConfigStore, PathState, ReloadOutcome, inspect_paths};
use crate::ipc::protocol::{
    CoreApplyOutcome, CoreFileOutcome, CoreFileState, CoreFileStatus, CoreReload, CoreStatus,
    Request, Response, WireErrorKind,
};

/// The reserved module name for reload and status.
pub const CORE_MODULE: &str = "core";

/// A method failure reported to the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandlerError {
    kind: HandlerErrorKind,
    message: String,
}

impl HandlerError {
    /// Parameters do not match the adapter's type.
    #[must_use]
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            kind: HandlerErrorKind::InvalidParams,
            message: message.into(),
        }
    }

    /// The module declares no such method.
    #[must_use]
    pub fn unknown_method(method: &str) -> Self {
        Self {
            kind: HandlerErrorKind::UnknownMethod,
            message: format!("unknown method `{method}`"),
        }
    }

    /// The module is disabled in configuration.
    #[must_use]
    pub fn disabled(reason: impl Into<String>) -> Self {
        Self {
            kind: HandlerErrorKind::Disabled,
            message: reason.into(),
        }
    }

    /// The module or one of its dependencies is unavailable.
    #[must_use]
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            kind: HandlerErrorKind::Unavailable,
            message: reason.into(),
        }
    }

    /// The handler failed internally.
    #[must_use]
    pub fn handler(message: impl Into<String>) -> Self {
        Self {
            kind: HandlerErrorKind::Handler,
            message: message.into(),
        }
    }

    /// The failure category.
    #[must_use]
    pub const fn kind(&self) -> HandlerErrorKind { self.kind }

    /// The human-readable reason.
    #[must_use]
    pub fn message(&self) -> &str { &self.message }
}

impl fmt::Display for HandlerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for HandlerError {}

/// Why a handler refused a method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerErrorKind {
    /// The parameters do not match the adapter's type.
    InvalidParams,
    /// The module declares no such method.
    UnknownMethod,
    /// The module is disabled in configuration.
    Disabled,
    /// The module or a dependency is unavailable.
    Unavailable,
    /// The handler failed internally.
    Handler,
}

impl fmt::Display for HandlerErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidParams => "invalid params",
            Self::UnknownMethod => "unknown method",
            Self::Disabled => "disabled",
            Self::Unavailable => "unavailable",
            Self::Handler => "handler error",
        })
    }
}

impl From<HandlerErrorKind> for WireErrorKind {
    fn from(kind: HandlerErrorKind) -> Self {
        match kind {
            HandlerErrorKind::InvalidParams => Self::InvalidParams,
            HandlerErrorKind::Disabled => Self::Disabled,
            HandlerErrorKind::Unavailable => Self::Unavailable,
            HandlerErrorKind::Handler => Self::Handler,
            HandlerErrorKind::UnknownMethod => Self::UnknownMethod,
        }
    }
}

/// Result alias for one handler call.
pub type HandlerResult<T> = Result<T, HandlerError>;

/// One module's method dispatcher.
pub trait IpcHandler: Send + Sync {
    /// Executes one method with already-parsed parameters.
    ///
    /// # Errors
    /// Returns a typed [`HandlerError`] when the method is unknown, disabled,
    /// unavailable or fails.
    fn handle(&self, method: &str, params: Value) -> HandlerResult<Value>;
}

/// The reserved `core` methods served by the host.
pub trait CoreHandler: Send + Sync {
    /// Rereads every configuration file independently.
    ///
    /// # Errors
    /// Returns a typed failure when the store itself cannot run.
    fn reload(&self) -> HandlerResult<CoreReload>;

    /// Reports the configuration files and their diagnostics.
    ///
    /// # Errors
    /// Returns a typed failure when the report cannot be produced.
    fn status(&self) -> HandlerResult<CoreStatus>;
}

/// Deserializes method parameters into one adapter type.
///
/// # Errors
/// Returns [`HandlerError::invalid_params`] with the serde message.
pub fn decode_params<T: DeserializeOwned>(params: Value) -> HandlerResult<T> {
    serde_json::from_value(params).map_err(|error| HandlerError::invalid_params(error.to_string()))
}

/// Serializes a typed handler response exactly once.
///
/// # Errors
/// Returns [`HandlerError::handler`] when the response cannot be represented.
pub fn encode_response<T: Serialize>(value: &T) -> HandlerResult<Value> {
    serde_json::to_value(value)
        .map_err(|error| HandlerError::handler(format!("cannot serialize response: {error}")))
}

/// A module name is empty or reserved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouterError {
    /// Handlers need a non-empty module name.
    EmptyModuleName,
    /// `core` is served by the router itself.
    ReservedModuleName,
    /// A handler is already registered for this name.
    DuplicateModule {
        /// The duplicated name.
        module: String,
    },
}

impl fmt::Display for RouterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyModuleName => f.write_str("a module name cannot be empty"),
            Self::ReservedModuleName => f.write_str("`core` is reserved for router methods"),
            Self::DuplicateModule { module } => {
                write!(f, "a handler is already registered for `{module}`")
            }
        }
    }
}

impl std::error::Error for RouterError {}

/// Maps request names to handlers and serves the core namespace.
pub struct Router {
    core: Arc<dyn CoreHandler>,
    modules: BTreeMap<String, Arc<dyn IpcHandler>>,
    stopped: AtomicBool,
}

impl Router {
    /// Creates a router with the host's core service.
    #[must_use]
    pub const fn new(core: Arc<dyn CoreHandler>) -> Self {
        Self {
            core,
            modules: BTreeMap::new(),
            stopped: AtomicBool::new(false),
        }
    }

    /// Refuses further dispatch. Repeated calls are no-ops.
    ///
    /// Takes no lock and joins nothing, so teardown can stop dispatch before
    /// any wait. A request that was already read when this runs is answered
    /// with the `unavailable` wire kind; a handler already running finishes
    /// normally.
    pub fn stop(&self) { self.stopped.store(true, Ordering::SeqCst); }

    /// Whether dispatch has been stopped.
    #[must_use]
    pub fn is_stopped(&self) -> bool { self.stopped.load(Ordering::SeqCst) }

    /// Registers one module handler.
    ///
    /// # Errors
    /// Returns [`RouterError`] for an empty name, the reserved `core` name or
    /// a duplicate registration.
    pub fn register(
        &mut self,
        module: impl Into<String>,
        handler: Arc<dyn IpcHandler>,
    ) -> Result<(), RouterError> {
        let module = module.into();
        if module.is_empty() {
            return Err(RouterError::EmptyModuleName);
        }
        if module == CORE_MODULE {
            return Err(RouterError::ReservedModuleName);
        }
        if self.modules.contains_key(&module) {
            return Err(RouterError::DuplicateModule { module });
        }
        self.modules.insert(module, handler);
        Ok(())
    }

    /// Routes one request. Unknown names and methods never panic.
    #[must_use]
    pub fn route(&self, request: Request) -> Response {
        if self.stopped.load(Ordering::SeqCst) {
            let refusal = HandlerError::unavailable("shutting down");
            return Response::failure(refusal.kind.into(), refusal.message);
        }
        if request.module == CORE_MODULE {
            return self.route_core(&request.method);
        }
        let Some(handler) = self.modules.get(&request.module) else {
            return Response::failure(
                WireErrorKind::UnknownModule,
                format!("no module `{}` is registered", request.module),
            );
        };
        match handler.handle(&request.method, request.params) {
            Ok(data) => Response::ok(data),
            Err(error) => Response::failure(error.kind.into(), error.message),
        }
    }

    fn route_core(&self, method: &str) -> Response {
        let result = match method {
            "reload" => self.core.reload().and_then(encode_core),
            "status" => self.core.status().and_then(encode_core),
            _ => {
                return Response::failure(
                    WireErrorKind::UnknownMethod,
                    format!("unknown method `{method}` for module `core`"),
                );
            }
        };
        match result {
            Ok(data) => Response::ok(data),
            Err(error) => Response::failure(error.kind.into(), error.message),
        }
    }
}

fn encode_core<T: Serialize>(value: T) -> HandlerResult<Value> { encode_response(&value) }

/// The default core service backed by the configuration store.
pub struct ConfigCore {
    store: Arc<ConfigStore>,
}

impl ConfigCore {
    /// Serves reload and status from one store.
    #[must_use]
    pub const fn new(store: Arc<ConfigStore>) -> Self { Self { store } }
}

impl CoreHandler for ConfigCore {
    fn reload(&self) -> HandlerResult<CoreReload> {
        let files = self
            .store
            .reload_all()
            .into_iter()
            .map(|file| {
                let diagnostic = self.store.diagnostic(file.file).map(|error| error.to_string());
                let outcome = match file.outcome {
                    ReloadOutcome::Applied { revision } => CoreApplyOutcome::Applied { revision },
                    ReloadOutcome::Unchanged => CoreApplyOutcome::Unchanged,
                    ReloadOutcome::Rejected(_) => CoreApplyOutcome::Rejected,
                };
                CoreFileOutcome {
                    file: file.file.file_name(),
                    outcome,
                    diagnostic,
                }
            })
            .collect();
        Ok(CoreReload { files, access: None })
    }

    fn status(&self) -> HandlerResult<CoreStatus> {
        let report = inspect_paths(self.store.catalog(), self.store.paths());
        let files = report
            .entries
            .into_iter()
            .map(|entry| {
                let (state, diagnostic) = match &entry.state {
                    PathState::Missing => (CoreFileState::Missing, None),
                    PathState::Valid => (CoreFileState::Valid, None),
                    PathState::Invalid(diagnostic) => {
                        (CoreFileState::Invalid, Some(diagnostic.to_string()))
                    }
                };
                let accepted = self.store.current_source_accepted(entry.file);
                let diagnostic = self
                    .store
                    .diagnostic(entry.file)
                    .map(|diagnostic| diagnostic.to_string())
                    .or(diagnostic);
                CoreFileStatus {
                    file: entry.file.file_name(),
                    state,
                    active: state == CoreFileState::Valid && accepted,
                    diagnostic,
                    warnings: entry.warnings,
                }
            })
            .collect();
        Ok(CoreStatus {
            directory: report.directory,
            files,
            access: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;
    use crate::config::{ConfigPaths, GlobalConfig};
    use crate::context::ModuleContext;
    use crate::events::EventBus;
    use crate::logging::{DiscardLogger, LogFilter};
    use crate::module::{Module, ModuleConfig, ModuleDescriptor, ModuleId};
    use crate::permissions::{Permission, PermissionError, PermissionProvider, PermissionStatus};
    use crate::registry::Registry;
    use crate::threading::{MainThreadExecutor, ThreadError};
    use crate::{ConfigCatalog, ConfigError, ModuleResult};

    struct NoCore;

    impl CoreHandler for NoCore {
        fn reload(&self) -> HandlerResult<CoreReload> {
            Ok(CoreReload {
                files: Vec::new(),
                access: None,
            })
        }

        fn status(&self) -> HandlerResult<CoreStatus> {
            Ok(CoreStatus {
                directory: "/config".into(),
                files: Vec::new(),
                access: None,
            })
        }
    }

    #[derive(Deserialize)]
    struct EchoParams {
        text: String,
    }

    struct Echo;

    impl IpcHandler for Echo {
        fn handle(&self, method: &str, params: Value) -> HandlerResult<Value> {
            match method {
                "echo" => encode_response(&decode_params::<EchoParams>(params)?.text),
                "disabled" => Err(HandlerError::disabled("audio is disabled")),
                "missing" => Err(HandlerError::unavailable("no audio device")),
                _ => Err(HandlerError::unknown_method(method)),
            }
        }
    }

    fn router() -> Router {
        let mut router = Router::new(Arc::new(NoCore));
        router.register("demo", Arc::new(Echo)).expect("register");
        router
    }

    #[test]
    fn typed_methods_round_trip_over_the_router() {
        let response = router()
            .route(Request::new("demo", "echo", json!({ "text": "hello" })).expect("request"));
        assert_eq!(response, Response::ok(json!("hello")));
    }

    #[test]
    fn unknown_modules_methods_and_params_fail_with_typed_kinds() {
        let router = router();
        let unknown_module = router.route(Request::without_params("missing", "echo"));
        assert!(matches!(
            unknown_module,
            Response::Error { error } if error.kind == WireErrorKind::UnknownModule
        ));

        let unknown_method = router.route(Request::without_params("demo", "nope"));
        assert!(matches!(
            unknown_method,
            Response::Error { error } if error.kind == WireErrorKind::UnknownMethod
        ));

        let bad_params = router.route(Request::without_params("demo", "echo"));
        assert!(matches!(
            bad_params,
            Response::Error { error } if error.kind == WireErrorKind::InvalidParams
        ));
    }

    #[test]
    fn disabled_and_unavailable_keep_their_categories() {
        let router = router();
        let disabled = router.route(Request::without_params("demo", "disabled"));
        assert!(matches!(
            disabled,
            Response::Error { error } if error.kind == WireErrorKind::Disabled
        ));
        let unavailable = router.route(Request::without_params("demo", "missing"));
        assert!(matches!(
            unavailable,
            Response::Error { error } if error.kind == WireErrorKind::Unavailable
        ));
    }

    #[test]
    fn core_and_duplicate_names_are_rejected() {
        let mut router = Router::new(Arc::new(NoCore));
        router.register("demo", Arc::new(Echo)).expect("register");
        assert_eq!(
            router.register("demo", Arc::new(Echo)),
            Err(RouterError::DuplicateModule { module: "demo".to_owned() })
        );
        assert_eq!(
            router.register("core", Arc::new(Echo)),
            Err(RouterError::ReservedModuleName)
        );
        assert_eq!(
            router.register("", Arc::new(Echo)),
            Err(RouterError::EmptyModuleName)
        );
    }

    #[test]
    fn core_unknown_methods_are_typed() {
        let response = router().route(Request::without_params("core", "explode"));
        assert!(matches!(
            response,
            Response::Error { error } if error.kind == WireErrorKind::UnknownMethod
        ));
    }

    #[test]
    fn a_request_already_read_is_refused_with_the_unavailable_kind_after_stop() {
        let router = router();
        // Read first, as the server does, then dispatch stops before routing.
        let request: Request = serde_json::from_slice(
            br#"{"module":"demo","method":"echo","params":{"text":"late"}}"#,
        )
        .expect("request read");
        router.stop();
        assert!(router.is_stopped());
        let response = router.route(request);
        assert!(
            matches!(response, Response::Error { ref error } if error.kind == WireErrorKind::Unavailable),
            "{response:?}"
        );
        let wire = serde_json::to_value(&response).expect("wire");
        assert_eq!(wire["error"]["kind"], "unavailable", "{wire}");
        assert_eq!(wire["error"]["message"], "shutting down", "{wire}");
    }

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default)]
    struct TestConfig {
        enabled: bool,
        value: u8,
    }

    impl ModuleConfig for TestConfig {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
    }

    struct TestModule;

    impl Module for TestModule {
        type Config = TestConfig;

        fn descriptor(&self) -> ModuleDescriptor {
            ModuleDescriptor {
                id: ModuleId::new("alpha"),
                provides: &[],
                requires: &[],
                permissions: &[],
                executables: &[],
                surfaces: &[],
                observes: &[],
                reserves_screen: false,
            }
        }

        fn configure(&mut self, _: &TestConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

        fn start(&mut self, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

        fn reconfigure(&mut self, _: &TestConfig, _: &ModuleContext) -> ModuleResult<()> {
            Err(crate::ModuleError::new(
                ModuleId::new("alpha"),
                crate::Operation::Reconfigure,
                crate::ErrorCategory::Config,
                "reconfigure rejected",
            ))
        }

        fn stop(&mut self, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }
    }

    struct InlineMain;

    impl MainThreadExecutor for InlineMain {
        fn is_main_thread(&self) -> bool { true }

        fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            job();
            Ok(())
        }
    }

    struct AllowAll;

    impl PermissionProvider for AllowAll {
        fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

        fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
            Ok(PermissionStatus::Granted)
        }
    }

    fn core() -> (
        tempfile::TempDir,
        Arc<ConfigStore>,
        ConfigCore,
        Arc<Registry>,
        crate::registry::ModuleHandle<TestConfig>,
    ) {
        let root = tempfile::tempdir().expect("temp dir");
        let mut catalog = ConfigCatalog::new();
        catalog.register::<TestConfig>(ModuleId::new("alpha")).expect("alpha");
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        let registry = Arc::new(Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(InlineMain),
            Arc::new(AllowAll),
            Arc::new(crate::windows::UnavailableWindows),
        ));
        let paths = ConfigPaths::new(root.path().join("config"));
        fs::create_dir_all(paths.directory()).expect("config dir");
        let store = Arc::new(ConfigStore::new(
            registry.clone(),
            catalog,
            paths,
            Arc::new(LogFilter::default()),
        ));
        let handle = registry.register(TestModule, TestConfig::default()).expect("register");
        store.bind(handle).expect("bind");
        registry.start_all().expect("start");
        let core = ConfigCore::new(store.clone());
        (root, store, core, registry, handle)
    }

    #[test]
    fn config_core_reports_partial_reloads_and_keeps_last_good_state() {
        let (_root, store, core, registry, handle) = core();
        fs::write(
            store.paths().module_file(ModuleId::new("alpha")),
            "enabled = true\n",
        )
        .expect("good file");
        fs::write(store.paths().global_file(), "logging = { level = \"debug\" }\n")
            .expect("global file");
        let report = core.reload().expect("reload");
        assert_eq!(report.access, None, "a plain configuration core reports no gate");
        assert_eq!(core.status().expect("status").access, None);
        assert_eq!(report.files.len(), 2);
        assert!(!report.has_failures(), "{report:?}");
        assert!(matches!(
            report.files[0].outcome,
            CoreApplyOutcome::Applied { .. }
        ));

        fs::write(store.paths().module_file(ModuleId::new("alpha")), "not toml =")
            .expect("bad file");
        let rejected = core.reload().expect("reload");
        assert!(rejected.has_failures());
        assert_eq!(rejected.files[1].outcome, CoreApplyOutcome::Rejected);
        assert!(
            rejected.files[1]
                .diagnostic
                .as_deref()
                .is_some_and(|text| text.contains("alpha.toml"))
        );
        let snapshot = registry.snapshot(handle).expect("snapshot");
        assert_eq!(snapshot.revision, 2, "the last accepted revision must survive");
        assert_eq!(snapshot.status, crate::ModuleStatus::Running);
    }

    #[test]
    fn config_core_status_marks_accepted_valid_files_active() {
        let (_root, store, core, _registry, _handle) = core();
        fs::write(
            store.paths().module_file(ModuleId::new("alpha")),
            "enabled = true\n",
        )
        .expect("good file");
        let status = core.status().expect("status");
        assert_eq!(status.files[0].file, "global.toml");
        assert_eq!(status.files[0].state, CoreFileState::Missing);
        assert!(!status.files[0].active);
        assert_eq!(status.files[1].state, CoreFileState::Valid);
        assert!(!status.files[1].active, "not loaded yet");
        core.reload().expect("accept");
        core.reload().expect("unchanged source");
        let status = core.status().expect("status");
        assert!(status.files[1].active);

        fs::write(
            store.paths().module_file(ModuleId::new("alpha")),
            "enabled = true\nvalue = 1\n",
        )
        .expect("new valid bytes");
        assert!(!core.status().expect("status").files[1].active);
        core.reload().expect("rejected reconfigure");
        let status = core.status().expect("status");
        assert_eq!(status.files[1].state, CoreFileState::Valid);
        assert!(!status.files[1].active);
        assert!(
            status.files[1]
                .diagnostic
                .as_deref()
                .is_some_and(|text| text.contains("reconfigure rejected"))
        );

        fs::remove_file(store.paths().module_file(ModuleId::new("alpha"))).expect("delete");
        core.reload().expect("deleted");
        let status = core.status().expect("status");
        assert_eq!(status.files[1].state, CoreFileState::Missing);
        assert!(
            status.files[1]
                .diagnostic
                .as_deref()
                .is_some_and(|text| text.contains("deleted"))
        );

        fs::write(store.paths().module_file(ModuleId::new("alpha")), "not toml =")
            .expect("bad file");
        let status = core.status().expect("status");
        assert_eq!(status.files[1].state, CoreFileState::Invalid);
        assert!(status.files[1].diagnostic.is_some());
    }

    #[test]
    fn unloaded_global_and_unbound_module_are_not_active() {
        let (_root, store, core, registry, _) = core();
        fs::write(store.paths().global_file(), "# defaults\n").expect("global");
        assert!(!core.status().expect("status").files[0].active);
        store.load_global();
        assert!(core.status().expect("status").files[0].active);
        let unbound = ConfigStore::new(
            registry,
            {
                let mut catalog = ConfigCatalog::new();
                catalog.register::<TestConfig>(ModuleId::new("alpha")).expect("catalog");
                catalog
            },
            store.paths().clone(),
            Arc::new(LogFilter::default()),
        );
        fs::write(store.paths().module_file(ModuleId::new("alpha")), "# defaults\n")
            .expect("module");
        unbound.initial::<TestConfig>(ModuleId::new("alpha")).expect("initial");
        assert!(
            !unbound.current_source_accepted(crate::ConfigFileId::module(ModuleId::new("alpha")))
        );
    }
}
