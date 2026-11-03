//! Reading configuration files and applying them to a registry.

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::{fmt, fs};

use serde::de::DeserializeOwned;

use crate::config::catalog::ConfigCatalog;
use crate::config::diagnostics::{ConfigDiagnostic, ConfigFileId, DiagnosticKind};
use crate::config::global::GlobalConfig;
use crate::config::loader;
use crate::config::paths::ConfigPaths;
use crate::config::watcher::ChangeSet;
use crate::lock;
use crate::logging::LogFilter;
use crate::module::{ModuleConfig, ModuleId};
use crate::registry::{ModuleHandle, Registry, ReloadReport, ReloadStatus};

/// The local state of one expected file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathState {
    /// The file is absent.
    Missing,
    /// The file parses and validates.
    Valid,
    /// The file exists but is rejected.
    Invalid(ConfigDiagnostic),
}

/// One expected file in a [`PathReport`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathEntry {
    /// Which catalog file this entry describes.
    pub file: ConfigFileId,
    /// The path on disk.
    pub path: PathBuf,
    /// The local state, independent of any running host.
    pub state: PathState,
    /// Unknown keys in an otherwise valid file.
    pub warnings: Vec<String>,
}

/// The expected files and their local state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathReport {
    /// The configuration directory.
    pub directory: PathBuf,
    /// One entry per registered file, in file order.
    pub entries: Vec<PathEntry>,
}

/// Inspects every registered file without starting or contacting a host.
#[must_use]
pub fn inspect_paths(catalog: &ConfigCatalog, paths: &ConfigPaths) -> PathReport {
    let entries = catalog
        .ids()
        .map(|id| {
            let file = ConfigFileId::module(id);
            let path = paths.file(file);
            let (state, warnings) = match fs::read_to_string(&path) {
                Ok(text) => match catalog.inspect(id, &path, &text) {
                    Ok(warnings) => (PathState::Valid, warnings),
                    Err(diagnostic) => (PathState::Invalid(diagnostic), Vec::new()),
                },
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    (PathState::Missing, Vec::new())
                }
                Err(error) => (
                    PathState::Invalid(ConfigDiagnostic::io(file, &path, &error)),
                    Vec::new(),
                ),
            };
            PathEntry { file, path, state, warnings }
        })
        .collect();
    PathReport {
        directory: paths.directory().to_path_buf(),
        entries,
    }
}

/// The result of first reading one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Initial<T> {
    /// The file is absent and the typed default is used.
    Default(T),
    /// The file was accepted; the registry still validates it on registration.
    Loaded(T),
    /// The file exists but cannot be decoded, so the registry must register it
    /// without an accepted configuration.
    Invalid(ConfigDiagnostic),
}

/// The result of applying one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReloadOutcome {
    /// The candidate was accepted and committed at this revision.
    Applied {
        /// The committed revision.
        revision: u64,
    },
    /// The candidate decodes to the already accepted value.
    Unchanged,
    /// Nothing was applied; the last-good state and revision are preserved.
    Rejected(ConfigDiagnostic),
}

impl ReloadOutcome {
    /// Whether the outcome carries a diagnostic.
    #[must_use]
    pub const fn diagnostic(&self) -> Option<&ConfigDiagnostic> {
        match self {
            Self::Rejected(diagnostic) => Some(diagnostic),
            Self::Applied { .. } | Self::Unchanged => None,
        }
    }
}

/// One file's outcome inside a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileOutcome {
    /// The file that was evaluated.
    pub file: ConfigFileId,
    /// What happened to it.
    pub outcome: ReloadOutcome,
}

/// A configuration store failure for operations that do not describe a file.
#[derive(Debug)]
pub enum ConfigStoreError {
    /// The identifier has no catalog entry.
    UnknownConfig {
        /// The unknown identifier.
        id: ModuleId,
    },
    /// The identifier was never bound to a registered module handle.
    Unbound {
        /// The unbound identifier.
        id: ModuleId,
    },
    /// The handle's type does not match the catalog registration.
    TypeMismatch {
        /// The identifier with the mismatch.
        id: ModuleId,
    },
}

impl fmt::Display for ConfigStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownConfig { id } => {
                write!(f, "no configuration type is registered for `{id}`")
            }
            Self::Unbound { id } => {
                write!(f, "module `{id}` is not bound to a running registry")
            }
            Self::TypeMismatch { id } => {
                write!(
                    f,
                    "module `{id}` was registered with a different configuration type"
                )
            }
        }
    }
}

impl std::error::Error for ConfigStoreError {}

/// A failure while applying a decoded candidate.
struct ReloadFailure {
    kind: DiagnosticKind,
    message: String,
}

/// A typed handle bound to the catalog entry it reloads.
trait Binding: Send + Sync {
    fn reload(
        &self,
        registry: &Registry,
        candidate: Box<dyn Any + Send + Sync>,
    ) -> Result<ReloadReport, ReloadFailure>;
}

struct TypedBinding<C> {
    handle: ModuleHandle<C>,
}

impl<C: ModuleConfig> Binding for TypedBinding<C> {
    fn reload(
        &self,
        registry: &Registry,
        candidate: Box<dyn Any + Send + Sync>,
    ) -> Result<ReloadReport, ReloadFailure> {
        let config = *candidate.downcast::<C>().map_err(|_| ReloadFailure {
            kind: DiagnosticKind::Application,
            message: "registered configuration type mismatch".to_owned(),
        })?;
        match registry.reload(self.handle, config) {
            Ok(report) => Ok(report),
            Err(crate::RegistryError::InvalidConfig { message, .. }) => Err(ReloadFailure {
                kind: DiagnosticKind::Validation,
                message,
            }),
            Err(error) => Err(ReloadFailure {
                kind: DiagnosticKind::Application,
                message: error.to_string(),
            }),
        }
    }
}

#[derive(Default)]
struct StoreState {
    /// Raw text of the last accepted (applied or decoded-unchanged) file.
    sources: HashMap<ConfigFileId, String>,
    /// Files that have been read at least once. Deletion is meaningful only
    /// for these; an absent file that never existed is simply unchanged.
    seen: HashSet<ConfigFileId>,
    diagnostics: BTreeMap<ConfigFileId, ConfigDiagnostic>,
    warnings: BTreeMap<ConfigFileId, Vec<String>>,
    global: Option<GlobalConfig>,
    global_revision: u64,
}

/// Reads configuration files and applies them to a registry.
///
/// The store owns no lifecycle state: a rejected candidate leaves the
/// registry's last-good configuration, revision and runtime untouched.
pub struct ConfigStore {
    registry: Arc<Registry>,
    catalog: ConfigCatalog,
    paths: ConfigPaths,
    log_filter: Arc<LogFilter>,
    bindings: Mutex<BTreeMap<ModuleId, Arc<dyn Binding>>>,
    state: Mutex<StoreState>,
    reload_lock: Mutex<()>,
}

impl ConfigStore {
    /// Creates a store over an existing registry and injected paths.
    #[must_use]
    pub fn new(
        registry: Arc<Registry>,
        catalog: ConfigCatalog,
        paths: ConfigPaths,
        log_filter: Arc<LogFilter>,
    ) -> Self {
        Self {
            registry,
            catalog,
            paths,
            log_filter,
            bindings: Mutex::new(BTreeMap::new()),
            state: Mutex::new(StoreState::default()),
            reload_lock: Mutex::new(()),
        }
    }

    /// The catalog backing this store.
    #[must_use]
    pub const fn catalog(&self) -> &ConfigCatalog { &self.catalog }

    /// The resolved configuration paths.
    #[must_use]
    pub const fn paths(&self) -> &ConfigPaths { &self.paths }

    /// The shared filter updated when the global level changes.
    #[must_use]
    pub const fn log_filter(&self) -> &Arc<LogFilter> { &self.log_filter }

    /// Binds a registered module to its catalog entry.
    ///
    /// # Errors
    /// Returns [`ConfigStoreError`] when the identifier is unregistered or the
    /// handle's configuration type differs from the catalog entry.
    pub fn bind<C: ModuleConfig>(&self, handle: ModuleHandle<C>) -> Result<(), ConfigStoreError> {
        let id = handle.id();
        let type_id = self.catalog.type_id(id).ok_or(ConfigStoreError::UnknownConfig { id })?;
        if type_id != std::any::TypeId::of::<C>() {
            return Err(ConfigStoreError::TypeMismatch { id });
        }
        lock(&self.bindings).insert(id, Arc::new(TypedBinding { handle }));
        Ok(())
    }

    /// Reads one module file for the first time.
    ///
    /// A missing file yields the typed default. A supplied file that cannot be
    /// decoded is never replaced by the default; the caller registers the
    /// module with [`Registry::register_rejected`] and binds it like any other,
    /// so a later valid save can activate it.
    ///
    /// # Errors
    /// Returns [`ConfigStoreError::UnknownConfig`] for an unregistered ID.
    pub fn initial<C>(&self, id: ModuleId) -> Result<Initial<C>, ConfigStoreError>
    where C: ModuleConfig + Default + DeserializeOwned {
        if !self.catalog.contains(id) {
            return Err(ConfigStoreError::UnknownConfig { id });
        }
        let file = ConfigFileId::module(id);
        let path = self.paths.file(file);
        match fs::read_to_string(&path) {
            Ok(text) => {
                lock(&self.state).seen.insert(file);
                match self.catalog.decode_typed::<C>(id, &path, &text) {
                    Ok((config, warnings)) => match loader::validate(file, &path, &config) {
                        Ok(()) => {
                            self.set_warnings(file, warnings);
                            lock(&self.state).sources.insert(file, text);
                            Ok(Initial::Loaded(config))
                        }
                        Err(diagnostic) => {
                            self.set_warnings(file, Vec::new());
                            self.record(diagnostic);
                            Ok(Initial::Loaded(config))
                        }
                    },
                    Err(diagnostic) => {
                        self.set_warnings(file, Vec::new());
                        self.record(diagnostic.clone());
                        Ok(Initial::Invalid(diagnostic))
                    }
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Initial::Default(C::default())),
            Err(error) => {
                lock(&self.state).seen.insert(file);
                self.set_warnings(file, Vec::new());
                let diagnostic = ConfigDiagnostic::io(file, &path, &error);
                self.record(diagnostic.clone());
                Ok(Initial::Invalid(diagnostic))
            }
        }
    }

    /// Reads `global.jsonc` for the first time and applies its logging level.
    ///
    /// A missing or rejected file is reported through the returned value while
    /// the injected [`LogFilter`] keeps its current level.
    pub fn load_global(&self) -> Initial<GlobalConfig> {
        let file = ConfigFileId::Global;
        let path = self.paths.global_file();
        let (config, from_default) = match fs::read_to_string(&path) {
            Ok(text) => {
                lock(&self.state).seen.insert(file);
                match self.catalog.decode_typed::<GlobalConfig>(GlobalConfig::ID, &path, &text) {
                    Ok((config, warnings)) => {
                        self.set_warnings(file, warnings);
                        lock(&self.state).sources.insert(file, text);
                        (config, false)
                    }
                    Err(diagnostic) => {
                        self.set_warnings(file, Vec::new());
                        self.record(diagnostic.clone());
                        return Initial::Invalid(diagnostic);
                    }
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => (GlobalConfig::default(), true),
            Err(error) => {
                lock(&self.state).seen.insert(file);
                self.set_warnings(file, Vec::new());
                let diagnostic = ConfigDiagnostic::io(file, &path, &error);
                self.record(diagnostic.clone());
                return Initial::Invalid(diagnostic);
            }
        };
        self.accept_global(&config);
        lock(&self.state).global_revision = 1;
        if from_default {
            Initial::Default(config)
        } else {
            Initial::Loaded(config)
        }
    }

    /// Rereads `global.jsonc` and updates the logging level in place.
    ///
    /// Startup commands are never executed here or anywhere else in the config
    /// code; the host decides when the desktop launches.
    #[must_use]
    pub fn reload_global(&self) -> ReloadOutcome {
        let _reload = lock(&self.reload_lock);
        self.reload_global_locked()
    }

    /// Rereads one bound module file and applies it through the registry.
    ///
    /// # Errors
    /// Returns [`ConfigStoreError`] when the module is unregistered or unbound.
    pub fn reload_one(&self, id: ModuleId) -> Result<ReloadOutcome, ConfigStoreError> {
        let _reload = lock(&self.reload_lock);
        self.reload_bound(id)
    }

    /// Rereads the global file and every bound module file.
    ///
    /// Files are evaluated independently; one rejection does not prevent the
    /// others from being applied.
    #[must_use]
    pub fn reload_all(&self) -> Vec<FileOutcome> {
        let _reload = lock(&self.reload_lock);
        let mut outcomes = vec![FileOutcome {
            file: ConfigFileId::Global,
            outcome: self.reload_global_locked(),
        }];
        let ids: Vec<ModuleId> = lock(&self.bindings).keys().copied().collect();
        for id in ids {
            if let Ok(outcome) = self.reload_bound(id) {
                outcomes.push(FileOutcome {
                    file: ConfigFileId::Module(id),
                    outcome,
                });
            }
        }
        outcomes
    }

    /// Applies one watcher batch. A rescan rereads every file; otherwise only
    /// the named files are touched.
    #[must_use]
    pub fn apply(&self, changes: &ChangeSet) -> Vec<FileOutcome> {
        if changes.rescan {
            return self.reload_all();
        }
        let _reload = lock(&self.reload_lock);
        let mut outcomes = Vec::with_capacity(changes.files.len());
        for file in &changes.files {
            let outcome = match file {
                ConfigFileId::Global => self.reload_global_locked(),
                ConfigFileId::Module(id) => match self.reload_bound(*id) {
                    Ok(outcome) => outcome,
                    Err(_) => continue,
                },
            };
            outcomes.push(FileOutcome { file: *file, outcome });
        }
        outcomes
    }

    /// The diagnostic currently held for one file.
    #[must_use]
    pub fn diagnostic(&self, file: ConfigFileId) -> Option<ConfigDiagnostic> {
        lock(&self.state).diagnostics.get(&file).cloned()
    }

    /// Every held diagnostic in file order.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<ConfigDiagnostic> {
        lock(&self.state).diagnostics.values().cloned().collect()
    }

    /// Whether the host accepted the current on-disk bytes for a loaded global
    /// file or a bound module. A held diagnostic does not erase last-good state.
    #[must_use]
    pub fn current_source_accepted(&self, file: ConfigFileId) -> bool {
        if let ConfigFileId::Module(id) = file
            && !lock(&self.bindings).contains_key(&id)
        {
            return false;
        }
        let Ok(text) = fs::read_to_string(self.paths.file(file)) else {
            return false;
        };
        let state = lock(&self.state);
        if file == ConfigFileId::Global && state.global.is_none() {
            return false;
        }
        state.sources.get(&file) == Some(&text)
    }

    /// Unknown keys currently reported for one file, in decode order.
    ///
    /// A warning never rejects the file or blocks a later reload.
    #[must_use]
    pub fn warnings(&self, file: ConfigFileId) -> Vec<String> {
        lock(&self.state).warnings.get(&file).cloned().unwrap_or_default()
    }

    /// The last accepted global configuration.
    #[must_use]
    pub fn global_config(&self) -> Option<GlobalConfig> { lock(&self.state).global.clone() }

    fn reload_global_locked(&self) -> ReloadOutcome {
        let file = ConfigFileId::Global;
        let path = self.paths.global_file();
        let text = match fs::read_to_string(&path) {
            Ok(text) => {
                lock(&self.state).seen.insert(file);
                text
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                self.set_warnings(file, Vec::new());
                return if lock(&self.state).seen.contains(&file) {
                    let diagnostic = ConfigDiagnostic::deleted(file, &path);
                    self.record(diagnostic.clone());
                    ReloadOutcome::Rejected(diagnostic)
                } else {
                    ReloadOutcome::Unchanged
                };
            }
            Err(error) => {
                lock(&self.state).seen.insert(file);
                self.set_warnings(file, Vec::new());
                let diagnostic = ConfigDiagnostic::io(file, &path, &error);
                self.record(diagnostic.clone());
                return ReloadOutcome::Rejected(diagnostic);
            }
        };
        let cached_and_clean = {
            let state = lock(&self.state);
            state.sources.get(&file) == Some(&text) && !state.diagnostics.contains_key(&file)
        };
        if cached_and_clean {
            return ReloadOutcome::Unchanged;
        }
        let (config, warnings) =
            match self.catalog.decode_typed::<GlobalConfig>(GlobalConfig::ID, &path, &text) {
                Ok(decoded) => decoded,
                Err(diagnostic) => {
                    self.set_warnings(file, Vec::new());
                    self.record(diagnostic.clone());
                    return ReloadOutcome::Rejected(diagnostic);
                }
            };
        self.set_warnings(file, warnings);
        let mut state = lock(&self.state);
        if state.global.as_ref() == Some(&config) {
            state.sources.insert(file, text);
            state.diagnostics.remove(&file);
            return ReloadOutcome::Unchanged;
        }
        state.sources.insert(file, text);
        state.global = Some(config.clone());
        state.global_revision += 1;
        let revision = state.global_revision;
        state.diagnostics.remove(&file);
        drop(state);
        self.log_filter.set_level(config.log_level());
        ReloadOutcome::Applied { revision }
    }

    fn reload_bound(&self, id: ModuleId) -> Result<ReloadOutcome, ConfigStoreError> {
        if !self.catalog.contains(id) {
            return Err(ConfigStoreError::UnknownConfig { id });
        }
        let binding =
            lock(&self.bindings).get(&id).cloned().ok_or(ConfigStoreError::Unbound { id })?;
        let file = ConfigFileId::Module(id);
        let path = self.paths.file(file);
        let text = match fs::read_to_string(&path) {
            Ok(text) => {
                lock(&self.state).seen.insert(file);
                text
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                self.set_warnings(file, Vec::new());
                return if lock(&self.state).seen.contains(&file) {
                    let diagnostic = ConfigDiagnostic::deleted(file, &path);
                    self.record(diagnostic.clone());
                    Ok(ReloadOutcome::Rejected(diagnostic))
                } else {
                    Ok(ReloadOutcome::Unchanged)
                };
            }
            Err(error) => {
                lock(&self.state).seen.insert(file);
                self.set_warnings(file, Vec::new());
                let diagnostic = ConfigDiagnostic::io(file, &path, &error);
                self.record(diagnostic.clone());
                return Ok(ReloadOutcome::Rejected(diagnostic));
            }
        };
        let cached_and_clean = {
            let state = lock(&self.state);
            state.sources.get(&file) == Some(&text) && !state.diagnostics.contains_key(&file)
        };
        if cached_and_clean {
            return Ok(ReloadOutcome::Unchanged);
        }
        let (candidate, warnings) = match self.catalog.decode(id, &path, &text) {
            Ok(decoded) => decoded.into_parts(),
            Err(diagnostic) => {
                self.set_warnings(file, Vec::new());
                self.record(diagnostic.clone());
                return Ok(ReloadOutcome::Rejected(diagnostic));
            }
        };
        match binding.reload(&self.registry, candidate) {
            Ok(report) => {
                self.set_warnings(file, warnings);
                lock(&self.state).sources.insert(file, text);
                lock(&self.state).diagnostics.remove(&file);
                if let Some(error) = report.start_error {
                    self.record(ConfigDiagnostic {
                        file,
                        path,
                        kind: DiagnosticKind::Application,
                        location: None,
                        field: None,
                        message: format!(
                            "configuration applied but the module failed to start: {error}"
                        ),
                    });
                }
                Ok(match report.status {
                    ReloadStatus::Applied { revision } => ReloadOutcome::Applied { revision },
                    ReloadStatus::Unchanged => ReloadOutcome::Unchanged,
                })
            }
            Err(failure) => {
                self.set_warnings(file, Vec::new());
                let diagnostic = ConfigDiagnostic {
                    file,
                    path,
                    kind: failure.kind,
                    location: None,
                    field: None,
                    message: failure.message,
                };
                self.record(diagnostic.clone());
                Ok(ReloadOutcome::Rejected(diagnostic))
            }
        }
    }

    fn accept_global(&self, config: &GlobalConfig) {
        let mut state = lock(&self.state);
        state.diagnostics.remove(&ConfigFileId::Global);
        state.global = Some(config.clone());
        drop(state);
        self.log_filter.set_level(config.log_level());
    }

    fn record(&self, diagnostic: ConfigDiagnostic) {
        lock(&self.state).diagnostics.insert(diagnostic.file, diagnostic);
    }

    fn set_warnings(&self, file: ConfigFileId, warnings: Vec<String>) {
        let mut state = lock(&self.state);
        if warnings.is_empty() {
            state.warnings.remove(&file);
        } else {
            state.warnings.insert(file, warnings);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::events::EventBus;
    use crate::module::{Module, ModuleDescriptor};
    use crate::permissions::{Permission, PermissionError, PermissionProvider, PermissionStatus};
    use crate::status::ConfigDiagnostic as RegistryDiagnostic;
    use crate::threading::{MainThreadExecutor, ThreadError};
    use crate::{DiscardLogger, LogLevel, ModuleResult, Operation};

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default)]
    struct TestConfig {
        enabled: bool,
        value: u32,
    }

    impl ModuleConfig for TestConfig {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), crate::ConfigError> {
            if self.value > 10 {
                return Err(crate::ConfigError::new("value must be at most 10"));
            }
            Ok(())
        }
    }

    struct Recorder {
        id: &'static str,
        events: Arc<Mutex<Vec<String>>>,
        last_value: u32,
        failed_starts: u32,
    }

    impl Module for Recorder {
        type Config = TestConfig;

        fn descriptor(&self) -> ModuleDescriptor {
            ModuleDescriptor {
                id: ModuleId::new(self.id),
                provides: &[],
                requires: &[],
                permissions: &[],
                executables: &[],
                surfaces: &[],
            }
        }

        fn configure(&mut self, config: &TestConfig, _: &crate::ModuleContext) -> ModuleResult<()> {
            self.last_value = config.value;
            lock(&self.events).push(format!("{}:configure:{}", self.id, config.value));
            Ok(())
        }

        fn start(&mut self, _: &crate::ModuleContext) -> ModuleResult<()> {
            lock(&self.events).push(format!("{}:start", self.id));
            if self.failed_starts > 0 {
                self.failed_starts -= 1;
                return Err(crate::ModuleError::new(
                    ModuleId::new(self.id),
                    Operation::Start,
                    crate::ErrorCategory::Lifecycle,
                    "start refused by test",
                ));
            }
            Ok(())
        }

        fn reconfigure(
            &mut self,
            config: &TestConfig,
            _: &crate::ModuleContext,
        ) -> ModuleResult<()> {
            self.last_value = config.value;
            lock(&self.events).push(format!("{}:reconfigure:{}", self.id, config.value));
            if config.value == 7 {
                return Err(crate::ModuleError::new(
                    ModuleId::new(self.id),
                    Operation::Reconfigure,
                    crate::ErrorCategory::Lifecycle,
                    "refused by test",
                ));
            }
            Ok(())
        }

        fn stop(&mut self, _: &crate::ModuleContext) -> ModuleResult<()> {
            lock(&self.events).push(format!("{}:stop", self.id));
            Ok(())
        }
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

    fn registry() -> Arc<Registry> {
        Arc::new(Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(InlineMain),
            Arc::new(AllowAll),
            Arc::new(crate::windows::UnavailableWindows),
        ))
    }

    fn catalog() -> ConfigCatalog {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<TestConfig>(ModuleId::new("alpha")).expect("alpha");
        catalog.register::<TestConfig>(ModuleId::new("beta")).expect("beta");
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        catalog
    }

    fn empty_setup() -> (tempfile::TempDir, Arc<Registry>, Arc<ConfigStore>, EventLog) {
        let root = tempfile::tempdir().expect("temp dir");
        let registry = registry();
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(ConfigStore::new(
            registry.clone(),
            catalog(),
            ConfigPaths::new(root.path().join("config")),
            Arc::new(LogFilter::default()),
        ));
        fs::create_dir_all(store.paths().directory()).expect("config dir");
        (root, registry, store, events)
    }

    fn setup_full() -> FullSetup {
        let (root, registry, store, events) = empty_setup();
        let handle = registry
            .register(
                Recorder {
                    id: "alpha",
                    events: events.clone(),
                    last_value: 0,
                    failed_starts: 0,
                },
                TestConfig { enabled: true, value: 1 },
            )
            .expect("register");
        store.bind(handle).expect("bind");
        (root, registry, store, handle, events)
    }

    fn setup() -> PlainSetup {
        let (root, registry, store, handle, _) = setup_full();
        (root, registry, store, handle)
    }

    fn write(store: &ConfigStore, id: ModuleId, text: &str) {
        fs::write(store.paths().module_file(id), text).expect("write config");
    }

    type EventLog = Arc<Mutex<Vec<String>>>;
    type FullSetup = (
        tempfile::TempDir,
        Arc<Registry>,
        Arc<ConfigStore>,
        ModuleHandle<TestConfig>,
        EventLog,
    );
    type PlainSetup = (
        tempfile::TempDir,
        Arc<Registry>,
        Arc<ConfigStore>,
        ModuleHandle<TestConfig>,
    );

    #[test]
    fn a_missing_initial_file_uses_the_typed_default() {
        let (_root, _registry, store, _handle) = setup();
        let initial: Initial<TestConfig> = store.initial(ModuleId::new("alpha")).expect("initial");
        assert_eq!(initial, Initial::Default(TestConfig::default()));
    }

    #[test]
    fn an_empty_file_is_rejected_with_an_actionable_message() {
        let (_root, _registry, store, _handle) = setup();
        let file = ConfigFileId::module(ModuleId::new("alpha"));
        write(&store, ModuleId::new("alpha"), " \n\t");
        let initial: Initial<TestConfig> = store.initial(ModuleId::new("alpha")).expect("initial");
        let Initial::Invalid(diagnostic) = initial else {
            panic!("an empty file must not use defaults silently");
        };
        assert_eq!(diagnostic.kind, DiagnosticKind::Type);
        assert_eq!(diagnostic.message, "file is empty; write `{}` to use defaults");
        assert!(store.warnings(file).is_empty());
    }

    #[test]
    fn a_malformed_initial_file_is_registered_rejected_and_recovers() {
        let (_root, registry, store, events) = empty_setup();
        write(&store, ModuleId::new("alpha"), "{ \"value\": ");
        let initial: Initial<TestConfig> = store.initial(ModuleId::new("alpha")).expect("initial");
        let Initial::Invalid(diagnostic) = initial else {
            panic!("expected invalid");
        };
        assert_eq!(diagnostic.kind, DiagnosticKind::Parse);
        let handle = registry
            .register_rejected(
                Recorder {
                    id: "alpha",
                    events: events.clone(),
                    last_value: 0,
                    failed_starts: 0,
                },
                RegistryDiagnostic::new(diagnostic.message),
            )
            .expect("register rejected");
        store.bind(handle).expect("bind");
        let broken = registry.snapshot(handle).expect("snapshot");
        assert_eq!(broken.status, crate::ModuleStatus::Stopped);
        assert_eq!(broken.revision, 0);
        assert!(broken.diagnostic.is_some());
        registry.start_all().expect("start");

        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 2 }",
        );
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("reload"),
            ReloadOutcome::Applied { revision: 1 }
        );
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            crate::ModuleStatus::Running,
            "the reload must start the recovered module"
        );
        assert!(lock(&events).iter().any(|event| event == "alpha:start"));
    }

    #[test]
    fn a_start_failure_after_reload_is_applied_with_a_diagnostic_and_retried() {
        let (_root, registry, store, events) = empty_setup();
        let handle = registry
            .register(
                Recorder {
                    id: "alpha",
                    events,
                    last_value: 0,
                    failed_starts: 1,
                },
                TestConfig { enabled: false, value: 1 },
            )
            .expect("register");
        store.bind(handle).expect("bind");
        registry.start_all().expect("start");

        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 8 }",
        );
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("reload"),
            ReloadOutcome::Applied { revision: 2 },
            "the configuration is committed even when the start fails"
        );
        let diagnostic = store
            .diagnostic(ConfigFileId::module(ModuleId::new("alpha")))
            .expect("start diagnostic");
        assert_eq!(diagnostic.kind, DiagnosticKind::Application);
        assert!(diagnostic.message.contains("failed to start"), "{diagnostic}");
        let failed = registry.snapshot(handle).expect("snapshot");
        assert!(matches!(failed.status, crate::ModuleStatus::Failed(_)));
        assert_eq!(failed.revision, 2);

        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 8 }",
        );
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("retry"),
            ReloadOutcome::Unchanged,
            "an identical save must retry the failed start through the diagnostic path"
        );
        let retried = registry.snapshot(handle).expect("snapshot");
        assert_eq!(retried.status, crate::ModuleStatus::Running);
        assert_eq!(retried.revision, 2);
        assert!(store.diagnostic(ConfigFileId::module(ModuleId::new("alpha"))).is_none());
    }

    #[test]
    fn a_malformed_initial_file_is_never_replaced_by_defaults() {
        let (_root, _registry, store, _handle) = setup();
        write(&store, ModuleId::new("alpha"), "{ \"value\": ");
        let initial: Initial<TestConfig> = store.initial(ModuleId::new("alpha")).expect("initial");
        match initial {
            Initial::Invalid(diagnostic) => assert_eq!(diagnostic.kind, DiagnosticKind::Parse),
            other => panic!("expected invalid, got {other:?}"),
        }
    }

    #[test]
    fn an_invalid_initial_value_is_loaded_for_registration_and_diagnosed() {
        let (_root, _registry, store, _handle) = setup();
        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 99 }",
        );
        let initial: Initial<TestConfig> = store.initial(ModuleId::new("alpha")).expect("initial");
        assert!(matches!(initial, Initial::Loaded(_)));
        let diagnostic = store
            .diagnostic(ConfigFileId::module(ModuleId::new("alpha")))
            .expect("diagnostic");
        assert_eq!(diagnostic.kind, DiagnosticKind::Validation);
    }

    #[test]
    fn reloading_one_file_applies_only_that_module() {
        let (_root, registry, store, handle) = setup();
        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 2 }",
        );
        let outcome = store.reload_one(ModuleId::new("alpha")).expect("reload");
        assert_eq!(outcome, ReloadOutcome::Applied { revision: 2 });
        assert_eq!(registry.snapshot(handle).expect("snapshot").revision, 2);
    }

    #[test]
    fn unchanged_content_does_not_touch_the_registry() {
        let (_root, registry, store, handle) = setup();
        let text = "{ \"enabled\": true, \"value\": 2 }";
        write(&store, ModuleId::new("alpha"), text);
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("first"),
            ReloadOutcome::Applied { revision: 2 }
        );
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("second"),
            ReloadOutcome::Unchanged
        );
        assert_eq!(registry.snapshot(handle).expect("snapshot").revision, 2);
    }

    #[test]
    fn unknown_keys_are_warnings_and_never_block_a_reload() {
        let (_root, registry, store, handle) = setup();
        let file = ConfigFileId::module(ModuleId::new("alpha"));
        registry.start_all().expect("start");
        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 2, \"enable\": true }",
        );
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("reload"),
            ReloadOutcome::Applied { revision: 2 }
        );
        assert_eq!(store.warnings(file), ["enable"]);
        assert!(
            store.diagnostic(file).is_none(),
            "a typo must not invalidate the file"
        );
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            crate::ModuleStatus::Running
        );

        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("identical save"),
            ReloadOutcome::Unchanged
        );
        assert_eq!(store.warnings(file), ["enable"]);

        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 2 }",
        );
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("typo removed"),
            ReloadOutcome::Unchanged
        );
        assert!(store.warnings(file).is_empty());
    }

    #[test]
    fn a_rejected_candidate_keeps_last_good_revision_and_runtime() {
        let (_root, registry, store, handle) = setup();
        registry.start_all().expect("start");
        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 99 }",
        );
        let outcome = store.reload_one(ModuleId::new("alpha")).expect("reload");
        let ReloadOutcome::Rejected(diagnostic) = outcome else {
            panic!("expected rejection");
        };
        assert_eq!(diagnostic.kind, DiagnosticKind::Validation);
        assert_eq!(
            store.diagnostic(ConfigFileId::module(ModuleId::new("alpha"))),
            Some(diagnostic)
        );
        let snapshot = registry.snapshot(handle).expect("snapshot");
        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.status, crate::ModuleStatus::Running);
        assert!(snapshot.diagnostic.is_some());
    }

    #[test]
    fn reverting_to_the_last_good_bytes_clears_diagnostics() {
        let (_root, registry, store, handle) = setup();
        registry.start_all().expect("start");
        let good = "{ \"enabled\": true, \"value\": 2 }";
        write(&store, ModuleId::new("alpha"), good);
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("good"),
            ReloadOutcome::Applied { revision: 2 }
        );

        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 99 }",
        );
        assert!(matches!(
            store.reload_one(ModuleId::new("alpha")).expect("bad"),
            ReloadOutcome::Rejected(_)
        ));
        assert!(store.diagnostic(ConfigFileId::module(ModuleId::new("alpha"))).is_some());
        assert!(registry.snapshot(handle).expect("snapshot").diagnostic.is_some());

        write(&store, ModuleId::new("alpha"), good);
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("undo"),
            ReloadOutcome::Unchanged
        );
        assert!(store.diagnostic(ConfigFileId::module(ModuleId::new("alpha"))).is_none());
        let snapshot = registry.snapshot(handle).expect("snapshot");
        assert!(
            snapshot.diagnostic.is_none(),
            "the last-good snapshot keeps a stale diagnostic"
        );
        assert_eq!(snapshot.revision, 2);
        assert_eq!(snapshot.status, crate::ModuleStatus::Running);
    }

    #[test]
    fn recreating_the_last_good_bytes_clears_the_deletion_diagnostic() {
        let (_root, registry, store, handle) = setup();
        registry.start_all().expect("start");
        let good = "{ \"enabled\": true, \"value\": 2 }";
        write(&store, ModuleId::new("alpha"), good);
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("good"),
            ReloadOutcome::Applied { revision: 2 }
        );
        let path = store.paths().module_file(ModuleId::new("alpha"));
        fs::remove_file(&path).expect("delete");
        let ReloadOutcome::Rejected(diagnostic) =
            store.reload_one(ModuleId::new("alpha")).expect("deleted")
        else {
            panic!("expected deletion rejection");
        };
        assert_eq!(diagnostic.kind, DiagnosticKind::Deleted);

        write(&store, ModuleId::new("alpha"), good);
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("recreate"),
            ReloadOutcome::Unchanged
        );
        assert!(store.diagnostic(ConfigFileId::module(ModuleId::new("alpha"))).is_none());
        let snapshot = registry.snapshot(handle).expect("snapshot");
        assert!(snapshot.diagnostic.is_none());
        assert_eq!(snapshot.revision, 2);
        assert_eq!(snapshot.status, crate::ModuleStatus::Running);
    }

    #[test]
    fn a_failed_reconfigure_is_retried_on_identical_content() {
        let (_root, registry, store, handle, events) = setup_full();
        registry.start_all().expect("start");
        let text = "{ \"enabled\": true, \"value\": 7 }";
        write(&store, ModuleId::new("alpha"), text);
        assert!(matches!(
            store.reload_one(ModuleId::new("alpha")).expect("first"),
            ReloadOutcome::Rejected(_)
        ));
        assert_eq!(
            lock(&events)
                .iter()
                .filter(|event| event.as_str() == "alpha:reconfigure:7")
                .count(),
            1
        );
        assert!(matches!(
            store.reload_one(ModuleId::new("alpha")).expect("second"),
            ReloadOutcome::Rejected(_)
        ));
        assert_eq!(
            lock(&events)
                .iter()
                .filter(|event| event.as_str() == "alpha:reconfigure:7")
                .count(),
            2,
            "a rejected candidate must not be cached as accepted"
        );
        assert_eq!(registry.snapshot(handle).expect("snapshot").revision, 1);
    }

    #[test]
    fn an_identical_bad_save_reports_the_diagnostic_again() {
        let (_root, _registry, store, _handle) = setup();
        let text = "{ \"enabled\": ";
        write(&store, ModuleId::new("alpha"), text);
        assert!(matches!(
            store.reload_one(ModuleId::new("alpha")).expect("first"),
            ReloadOutcome::Rejected(_)
        ));
        assert!(matches!(
            store.reload_one(ModuleId::new("alpha")).expect("second"),
            ReloadOutcome::Rejected(_)
        ));
    }

    #[test]
    fn deleting_and_recreating_a_file_preserves_then_replaces_last_good() {
        let (_root, registry, store, handle) = setup();
        registry.start_all().expect("start");
        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 2 }",
        );
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("reload"),
            ReloadOutcome::Applied { revision: 2 }
        );
        let path = store.paths().module_file(ModuleId::new("alpha"));
        fs::remove_file(&path).expect("delete");
        let outcome = store.reload_one(ModuleId::new("alpha")).expect("reload");
        let ReloadOutcome::Rejected(diagnostic) = outcome else {
            panic!("expected deletion rejection");
        };
        assert_eq!(diagnostic.kind, DiagnosticKind::Deleted);
        assert_eq!(registry.snapshot(handle).expect("snapshot").revision, 2);

        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 3 }",
        );
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("reload"),
            ReloadOutcome::Applied { revision: 3 }
        );
    }

    #[test]
    fn an_absent_module_file_that_never_existed_is_unchanged() {
        let (_root, _registry, store, _handle) = setup();
        let initial: Initial<TestConfig> = store.initial(ModuleId::new("alpha")).expect("initial");
        assert_eq!(initial, Initial::Default(TestConfig::default()));
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("reload"),
            ReloadOutcome::Unchanged
        );
        assert!(store.diagnostic(ConfigFileId::module(ModuleId::new("alpha"))).is_none());
    }

    #[test]
    fn an_absent_global_file_that_never_existed_is_unchanged() {
        let (_root, _registry, store, _handle) = setup();
        assert!(matches!(store.load_global(), Initial::Default(_)));
        assert_eq!(store.reload_all()[0].outcome, ReloadOutcome::Unchanged);
        assert!(store.diagnostic(ConfigFileId::Global).is_none());
    }

    #[test]
    fn a_paused_module_accepts_config_without_resuming() {
        let (_root, registry, store, handle) = setup();
        registry.start_all().expect("start");
        registry.pause(handle).expect("pause");
        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 4 }",
        );
        assert_eq!(
            store.reload_one(ModuleId::new("alpha")).expect("reload"),
            ReloadOutcome::Applied { revision: 2 }
        );
        let snapshot = registry.snapshot(handle).expect("snapshot");
        assert_eq!(snapshot.status, crate::ModuleStatus::Paused);
        assert_eq!(snapshot.revision, 2);
    }

    #[test]
    fn global_level_changes_live_and_rejections_keep_the_previous_level() {
        let (_root, _registry, store, _handle) = setup();
        write(
            &store,
            ModuleId::new("global"),
            "{ \"logging\": { \"level\": \"debug\" } }",
        );
        assert!(matches!(store.load_global(), Initial::Loaded(_)));
        assert_eq!(store.log_filter().level(), LogLevel::Debug);

        fs::write(
            store.paths().global_file(),
            "{ \"logging\": { \"level\": \"error\" } }",
        )
        .expect("write global");
        assert!(matches!(store.reload_global(), ReloadOutcome::Applied {
            revision: 2
        }));
        assert_eq!(store.log_filter().level(), LogLevel::Error);

        fs::write(store.paths().global_file(), "{ \"logging\": ").expect("write broken global");
        assert!(matches!(store.reload_global(), ReloadOutcome::Rejected(_)));
        assert_eq!(store.log_filter().level(), LogLevel::Error);
        assert!(
            matches!(store.reload_global(), ReloadOutcome::Rejected(_)),
            "an identical bad global save must be retried"
        );
    }

    #[test]
    fn a_malformed_initial_global_file_keeps_the_default_level() {
        let (_root, _registry, store, _handle) = setup();
        fs::write(store.paths().global_file(), "{ \"logging\": ").expect("write broken global");
        assert!(matches!(store.load_global(), Initial::Invalid(_)));
        assert_eq!(store.log_filter().level(), LogLevel::Info);
        assert!(store.global_config().is_none());
    }

    #[test]
    fn config_code_never_executes_startup_commands() {
        let (root, _registry, store, _handle) = setup();
        let sentinel = root.path().join("executed");
        let command = format!("touch {}", sentinel.display());
        fs::write(
            store.paths().global_file(),
            format!("{{ \"execOnStartup\": \"{command}\" }}"),
        )
        .expect("write global");
        store.load_global();
        fs::write(
            store.paths().global_file(),
            format!("{{ \"execOnStartup\": [\"{command}\"] }}"),
        )
        .expect("rewrite global");
        let _ = store.reload_global();
        assert!(!sentinel.exists(), "config loading must never run commands");
    }

    #[test]
    fn apply_only_reloads_the_named_files() {
        let (_root, registry, store, handle) = setup();
        write(
            &store,
            ModuleId::new("alpha"),
            "{ \"enabled\": true, \"value\": 2 }",
        );
        write(
            &store,
            ModuleId::new("beta"),
            "{ \"enabled\": true, \"value\": 2 }",
        );
        let mut changes = ChangeSet::default();
        changes.files.insert(ConfigFileId::module(ModuleId::new("alpha")));
        let outcomes = store.apply(&changes);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].file, ConfigFileId::module(ModuleId::new("alpha")));
        assert_eq!(registry.snapshot(handle).expect("snapshot").revision, 2);
    }

    #[test]
    fn path_inspection_marks_missing_valid_and_invalid_files() {
        let (root, _registry, _store, _handle) = setup();
        let paths = ConfigPaths::new(root.path().join("config"));
        fs::write(paths.module_file(ModuleId::new("alpha")), "{ \"value\": 2 }").expect("write");
        fs::write(paths.module_file(ModuleId::new("beta")), "{ \"value\": 99 }").expect("write");
        let report = inspect_paths(&catalog(), &paths);
        assert_eq!(report.entries.len(), 3);
        assert_eq!(report.entries[0].file, ConfigFileId::Global);
        assert_eq!(report.entries[0].state, PathState::Missing);
        assert_eq!(report.entries[1].state, PathState::Valid);
        let PathState::Invalid(diagnostic) = &report.entries[2].state else {
            panic!("expected invalid");
        };
        assert_eq!(diagnostic.kind, DiagnosticKind::Validation);
    }
}
