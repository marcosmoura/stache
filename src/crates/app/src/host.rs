//! The desktop composition root.
//!
//! One [`HostDriver`] owns the registry, configuration store and watcher, socket
//! server, permission policy and module control handles. It runs on a dedicated
//! worker thread: tray and Tauri callbacks enqueue commands and return, so the
//! `AppKit` main thread never waits for module code, a worker join or a socket
//! accept loop.
//!
//! Setup order is deliberate: the application creates the tray first, then
//! starts this driver. The driver loads configuration, registers factories,
//! starts modules, opens the control socket and only then executes
//! `execOnStartup` once. A shutdown requested before or during setup is
//! processed without starting new modules.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::de::DeserializeOwned;
use stache_keep_awake::{KeepAwakeConfig, KeepAwakeModule, KeepAwakeSnapshot};
use stache_macos::audio::{AudioBackend, CoreAudioBackend};
use stache_macos::{
    ApplicationControl, AssertionOwner, LockObservationInstaller, LockProbe, SystemLockProbe,
};
use stache_notunes::{NoTunesConfig, NoTunesModule};
use stache_proxy_audio::{AudioIpc, ProxyAudioConfig, ProxyAudioModule};
use stache_sdk::ipc::{
    ConfigCore, CoreHandler, CoreReload, CoreStatus, HandlerResult, IpcHandler, IpcServer, Router,
    RouterError, ServerError,
};
use stache_sdk::{
    CancellationToken, ChangeSet, ConfigCatalog, ConfigFileId, ConfigPaths, ConfigStore,
    ConfigStoreError, ConfigWatcher, EventBus, Initial, LogFilter, Logger, MainThreadExecutor,
    Module, ModuleConfig, ModuleHandle, ModuleId, ModuleSnapshot, ModuleStatus, PathEntry,
    Permission, PermissionProvider, PermissionStatus, Registry, RegistryError, ReloadOutcome,
    WatchError, WindowBackend, WindowError, WindowKey, WindowKind, WindowRequest, WindowSize,
    inspect_paths,
};

use crate::bindings::{HostStatus, ModuleStatusView, config_file_view};
use crate::config_edit::{CommitOutcome, EnableEditError, plan_enabled};
use crate::keep_awake::KeepAwakeIpc;
use crate::permissions::{
    PermissionHost, PermissionManager, PermissionPoll, PermissionTicker, PollSchedule,
    RefreshReport, RefreshTrigger, TickGate,
};
use crate::startup::{CommandRunner, StartupJob};
use crate::windows::HOST_OWNER;

/// Registry-facing adapter for [`PermissionHost`].
struct RegistryPermissions {
    registry: Arc<Registry>,
    logger: Arc<dyn Logger>,
}

impl PermissionHost for RegistryPermissions {
    fn needed(&self) -> Vec<Permission> {
        let snapshots: BTreeMap<ModuleId, ModuleSnapshot> = self
            .registry
            .snapshots()
            .into_iter()
            .map(|snapshot| (snapshot.id, snapshot))
            .collect();
        let mut needed = Vec::new();
        for descriptor in self.registry.descriptors() {
            let Some(snapshot) = snapshots.get(&descriptor.id) else {
                continue;
            };
            if snapshot.enabled && !snapshot.paused {
                for permission in descriptor.permissions {
                    if !needed.contains(permission) {
                        needed.push(*permission);
                    }
                }
            }
        }
        needed
    }

    fn waiting(&self) -> bool {
        self.registry
            .snapshots()
            .iter()
            .any(|snapshot| matches!(snapshot.status, ModuleStatus::WaitingForPermission(_)))
    }

    fn apply(&self, permission: Permission, status: PermissionStatus) {
        if let Err(error) = self.registry.on_permission_change(permission, status) {
            self.logger.log(stache_sdk::LogRecord {
                level: stache_sdk::LogLevel::Warn,
                module: ModuleId::new("host"),
                operation: stache_sdk::Operation::Reload,
                message: format!("permission change was not applied: {error}"),
            });
        }
    }
}

/// The socket core service. A CLI reload changes module state on the IPC
/// thread, so it asks the host worker to publish the new status once.
struct HostCore {
    core: ConfigCore,
    sender: Sender<HostCommand>,
}

impl CoreHandler for HostCore {
    fn reload(&self) -> HandlerResult<CoreReload> {
        let result = self.core.reload();
        let _ = self.sender.send(HostCommand::Refresh);
        result
    }

    fn status(&self) -> HandlerResult<CoreStatus> { self.core.status() }
}

/// A type-erased control surface for one registered module.
pub trait ModuleControl: Send + Sync {
    /// The module identifier.
    fn id(&self) -> ModuleId;
    /// Applies a desired enable flag.
    ///
    /// # Errors
    /// Returns a registry lifecycle failure.
    fn set_enabled(&self, registry: &Registry, enabled: bool) -> Result<(), RegistryError>;
    /// Pauses session state.
    ///
    /// # Errors
    /// Returns a registry lifecycle failure.
    fn pause(&self, registry: &Registry) -> Result<(), RegistryError>;
    /// Resumes session state.
    ///
    /// # Errors
    /// Returns a registry lifecycle failure.
    fn resume(&self, registry: &Registry) -> Result<(), RegistryError>;
}

impl<C: ModuleConfig> ModuleControl for ModuleHandle<C> {
    #[allow(clippy::use_self)] // `Self::id` would recurse into this trait method.
    fn id(&self) -> ModuleId { ModuleHandle::id(self) }

    fn set_enabled(&self, registry: &Registry, enabled: bool) -> Result<(), RegistryError> {
        registry.set_enabled(*self, enabled)
    }

    fn pause(&self, registry: &Registry) -> Result<(), RegistryError> { registry.pause(*self) }

    fn resume(&self, registry: &Registry) -> Result<(), RegistryError> { registry.resume(*self) }
}

/// Read-only views a module factory may use while building socket handlers.
///
/// The context holds the same registry and configuration store the host already
/// owns and exposes only published snapshots and held diagnostics. It never
/// exposes the mutable registry or store, reads disk or applies configuration,
/// so a handler built from it observes state instead of changing it.
#[derive(Clone)]
pub struct FactoryContext {
    registry: Arc<Registry>,
    store: Arc<ConfigStore>,
}

impl FactoryContext {
    /// Wraps the host's registry and configuration store.
    #[must_use]
    pub const fn new(registry: Arc<Registry>, store: Arc<ConfigStore>) -> Self {
        Self { registry, store }
    }

    /// The published snapshot of one module, if it is registered.
    ///
    /// This reads only published metadata, never the module's lifecycle lock or
    /// the module itself.
    #[must_use]
    pub fn module_snapshot(&self, id: ModuleId) -> Option<ModuleSnapshot> {
        self.registry.snapshots().into_iter().find(|snapshot| snapshot.id == id)
    }

    /// The configuration diagnostic currently held for one module file.
    #[must_use]
    pub fn diagnostic(&self, id: ModuleId) -> Option<stache_sdk::config::ConfigDiagnostic> {
        self.store.diagnostic(ConfigFileId::module(id))
    }
}

/// One module's registration in the composition root.
///
/// Part 6 implements this in the app crate; the SDK never sees the factory.
pub trait ModuleFactory: Send + Sync {
    /// The catalog identifier this factory registers.
    fn id(&self) -> ModuleId;

    /// Reads the initial configuration, registers the module and binds it.
    ///
    /// # Errors
    /// Returns a typed store or registry failure.
    fn register(
        &self,
        registry: &Registry,
        store: &ConfigStore,
    ) -> Result<Arc<dyn ModuleControl>, HostError>;

    /// Socket handlers this factory contributes to the host router.
    ///
    /// The default is empty; a module that has a socket surface returns its
    /// module name and handler here. The context carries only read-only
    /// published registry snapshots and held configuration diagnostics.
    fn ipc_handlers(&self, _context: &FactoryContext) -> Vec<(&'static str, Arc<dyn IpcHandler>)> {
        Vec::new()
    }
}

/// Registers one module from its catalog initial state and binds its config.
///
/// # Errors
/// Returns a typed store or registry failure.
pub fn register_module<M>(
    registry: &Registry,
    store: &ConfigStore,
    module: M,
) -> Result<Arc<dyn ModuleControl>, HostError>
where
    M: Module,
    M::Config: Default + DeserializeOwned,
{
    let id = module.descriptor().id;
    let handle = match store.initial::<M::Config>(id)? {
        Initial::Default(config) | Initial::Loaded(config) => registry.register(module, config)?,
        Initial::Invalid(diagnostic) => registry
            .register_rejected(module, stache_sdk::ConfigDiagnostic::new(diagnostic.message))?,
    };
    store.bind(handle)?;
    Ok(Arc::new(handle))
}

/// Registers the proxyAudio pilot and its read-only list handler over one shared
/// backend.
pub struct ProxyAudioFactory {
    backend: Arc<dyn AudioBackend>,
}

impl ProxyAudioFactory {
    /// Creates a factory over one audio backend.
    #[must_use]
    pub fn new(backend: Arc<dyn AudioBackend>) -> Self { Self { backend } }
}

impl ModuleFactory for ProxyAudioFactory {
    fn id(&self) -> ModuleId { ProxyAudioConfig::ID }

    fn register(
        &self,
        registry: &Registry,
        store: &ConfigStore,
    ) -> Result<Arc<dyn ModuleControl>, HostError> {
        register_module(registry, store, ProxyAudioModule::new(Arc::clone(&self.backend)))
    }

    fn ipc_handlers(&self, _context: &FactoryContext) -> Vec<(&'static str, Arc<dyn IpcHandler>)> {
        vec![(
            ProxyAudioConfig::ID.as_str(),
            Arc::new(AudioIpc::new(Arc::clone(&self.backend))),
        )]
    }
}

/// Registers the noTunes blocker over one safe application control.
pub struct NoTunesFactory {
    control: ApplicationControl,
}

impl NoTunesFactory {
    /// Creates a factory over one application control.
    #[must_use]
    pub const fn new(control: ApplicationControl) -> Self { Self { control } }
}

impl ModuleFactory for NoTunesFactory {
    fn id(&self) -> ModuleId { NoTunesConfig::ID }

    fn register(
        &self,
        registry: &Registry,
        store: &ConfigStore,
    ) -> Result<Arc<dyn ModuleControl>, HostError> {
        register_module(registry, store, NoTunesModule::new(self.control.clone()))
    }
}

/// Registers the Keep Awake runtime and its read-only status handler over one
/// shared runtime cell.
///
/// Construction is inert: the owner, probe and observation installer perform no
/// native work until an enabled module is started. The factory creates exactly
/// one [`KeepAwakeSnapshot`] cell and shares it with both the module (through
/// `with_snapshot`) and the socket handler, so a status query reads the module's
/// publication instead of a mirror. Tests inject fake adapters through
/// [`KeepAwakeFactory::new`]; the production list uses the inert native ones.
pub struct KeepAwakeFactory {
    owner: AssertionOwner,
    probe: Arc<dyn LockProbe>,
    observation: LockObservationInstaller,
    snapshot: KeepAwakeSnapshot,
}

impl KeepAwakeFactory {
    /// Creates a factory over one injected owner, probe and observation
    /// installer.
    #[must_use]
    pub fn new(
        owner: AssertionOwner,
        probe: Arc<dyn LockProbe>,
        observation: LockObservationInstaller,
    ) -> Self {
        Self {
            owner,
            probe,
            observation,
            snapshot: KeepAwakeSnapshot::default(),
        }
    }
}

impl ModuleFactory for KeepAwakeFactory {
    fn id(&self) -> ModuleId { KeepAwakeConfig::ID }

    fn register(
        &self,
        registry: &Registry,
        store: &ConfigStore,
    ) -> Result<Arc<dyn ModuleControl>, HostError> {
        register_module(
            registry,
            store,
            KeepAwakeModule::with_snapshot(
                self.owner.clone(),
                Arc::clone(&self.probe),
                self.observation.clone(),
                self.snapshot.clone(),
            ),
        )
    }

    fn ipc_handlers(&self, context: &FactoryContext) -> Vec<(&'static str, Arc<dyn IpcHandler>)> {
        vec![(
            KeepAwakeConfig::ID.as_str(),
            Arc::new(KeepAwakeIpc::new(context.clone(), self.snapshot.clone())),
        )]
    }
}

/// The factories the composition root registers.
#[derive(Default)]
pub struct ModuleFactories {
    entries: Vec<Arc<dyn ModuleFactory>>,
}

impl ModuleFactories {
    /// Creates an empty factory list.
    #[must_use]
    pub const fn new() -> Self { Self { entries: Vec::new() } }

    /// Creates the production list: the proxyAudio pilot, the noTunes blocker and
    /// Keep Awake.
    ///
    /// Registration only creates the modules and their read-only handlers; it
    /// starts no listener, installs no application or lock observer and acquires
    /// no power assertion. Starting depends on the accepted configuration. Every
    /// factory shares the host's main-thread executor; no module keeps a second
    /// clone of it.
    #[must_use]
    pub fn production(main_thread: Arc<dyn MainThreadExecutor>) -> Self {
        let backend: Arc<dyn AudioBackend> = Arc::new(CoreAudioBackend);
        Self::new()
            .with(ProxyAudioFactory::new(backend))
            .with(NoTunesFactory::new(ApplicationControl::system(Arc::clone(
                &main_thread,
            ))))
            .with(KeepAwakeFactory::new(
                AssertionOwner::system(),
                Arc::new(SystemLockProbe),
                LockObservationInstaller::system(main_thread),
            ))
    }

    /// Adds one factory.
    #[must_use]
    pub fn with(mut self, factory: impl ModuleFactory + 'static) -> Self {
        self.entries.push(Arc::new(factory));
        self
    }

    /// The registered factories.
    #[must_use]
    pub fn entries(&self) -> &[Arc<dyn ModuleFactory>] { &self.entries }
}

/// Paths and registrations the runner owns.
pub struct HostSettings {
    /// Identity cache/runtime paths, including the control socket.
    pub app: stache_sdk::AppPaths,
    /// Configuration directory and file paths.
    pub config: ConfigPaths,
    /// Authoritative configuration types.
    pub catalog: ConfigCatalog,
    /// Module factories.
    pub factories: ModuleFactories,
}

/// Injected host services.
pub struct HostServices {
    /// Registry logger.
    pub logger: Arc<dyn Logger>,
    /// Live level filter used by the logger and `global.jsonc`.
    pub log_filter: Arc<LogFilter>,
    /// Registry event bus.
    pub events: Arc<EventBus>,
    /// Main-thread dispatcher.
    pub main_thread: Arc<dyn MainThreadExecutor>,
    /// Fresh permission checks.
    pub permissions: Arc<dyn PermissionProvider>,
    /// Window backend.
    pub windows: Arc<dyn WindowBackend>,
    /// One-shot startup command runner.
    pub commands: Arc<dyn CommandRunner>,
    /// Permission polling delays.
    pub poll: PermissionPoll,
}

/// What the host tells the application shell.
pub trait HostUi: Send + Sync {
    /// Publishes a new status snapshot.
    fn status(&self, status: &HostStatus);
    /// Requests final process exit. Called on the host worker thread.
    fn exit(&self, code: i32);
}

/// A typed host setup or action failure.
#[derive(Debug)]
pub enum HostError {
    /// Configuration store failure.
    Store(ConfigStoreError),
    /// Registry failure.
    Registry(RegistryError),
    /// Socket server failure.
    Ipc(ServerError),
    /// Socket router registration failure.
    Router(RouterError),
    /// Watcher failure.
    Watch(WatchError),
    /// Window failure.
    Window(WindowError),
    /// A configuration diagnostic.
    Config(stache_sdk::config::ConfigDiagnostic),
    /// A configuration edit failure.
    Edit(EnableEditError),
    /// Thread creation failed.
    Thread(String),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(f),
            Self::Registry(error) => error.fmt(f),
            Self::Ipc(error) => error.fmt(f),
            Self::Router(error) => error.fmt(f),
            Self::Watch(error) => error.fmt(f),
            Self::Window(error) => error.fmt(f),
            Self::Config(error) => error.fmt(f),
            Self::Edit(error) => error.fmt(f),
            Self::Thread(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Registry(error) => Some(error),
            Self::Ipc(error) => Some(error),
            Self::Router(error) => Some(error),
            Self::Watch(error) => Some(error),
            Self::Window(error) => Some(error),
            Self::Config(error) => Some(error),
            Self::Edit(error) => Some(error),
            Self::Thread(_) => None,
        }
    }
}

impl From<ConfigStoreError> for HostError {
    fn from(error: ConfigStoreError) -> Self { Self::Store(error) }
}

impl From<RegistryError> for HostError {
    fn from(error: RegistryError) -> Self { Self::Registry(error) }
}

impl From<ServerError> for HostError {
    fn from(error: ServerError) -> Self { Self::Ipc(error) }
}

impl From<RouterError> for HostError {
    fn from(error: RouterError) -> Self { Self::Router(error) }
}

impl From<WatchError> for HostError {
    fn from(error: WatchError) -> Self { Self::Watch(error) }
}

impl From<WindowError> for HostError {
    fn from(error: WindowError) -> Self { Self::Window(error) }
}

impl From<stache_sdk::config::ConfigDiagnostic> for HostError {
    fn from(error: stache_sdk::config::ConfigDiagnostic) -> Self { Self::Config(error) }
}

impl From<EnableEditError> for HostError {
    fn from(error: EnableEditError) -> Self { Self::Edit(error) }
}

/// Tauri-managed access to the published status.
#[derive(Clone)]
pub struct HostState(pub HostHandle);

/// The application's command handle. Cloning is cheap and thread-safe.
#[derive(Clone)]
pub struct HostHandle {
    sender: Sender<HostCommand>,
    status: Arc<Mutex<HostStatus>>,
    shutdown_requested: Arc<AtomicBool>,
    finished: Arc<Finished>,
}

struct Finished {
    done: Mutex<bool>,
    signal: Condvar,
}

impl HostHandle {
    /// The last published status. Never touches module locks.
    #[must_use]
    pub fn status(&self) -> HostStatus {
        self.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Requests a configuration-backed enable change.
    pub fn set_enabled(&self, id: impl Into<String>, enabled: bool) {
        self.send(HostCommand::SetEnabled { id: id.into(), enabled });
    }

    /// Requests a session-only pause.
    pub fn pause(&self, id: impl Into<String>) { self.send(HostCommand::Pause(id.into())); }

    /// Requests a session-only resume.
    pub fn resume(&self, id: impl Into<String>) { self.send(HostCommand::Resume(id.into())); }

    /// Rechecks permissions after the application becomes active.
    pub fn refresh_permissions(&self) {
        self.send(HostCommand::RefreshPermissions(RefreshTrigger::Activation));
    }

    /// Prompts for a needed permission from an explicit user action.
    pub fn request_permission(&self) {
        self.send(HostCommand::RefreshPermissions(RefreshTrigger::Explicit));
    }

    /// Opens the explicit developer test window. Launch never calls this.
    pub fn open_test_window(&self) { self.send(HostCommand::OpenTestWindow); }

    /// Retries an enabled, non-paused module that is failed or unavailable.
    pub fn retry(&self, id: impl Into<String>) { self.send(HostCommand::Retry(id.into())); }

    /// Requests the idempotent shutdown coordinator.
    pub fn request_shutdown(&self) {
        if !self.shutdown_requested.swap(true, Ordering::SeqCst) {
            self.send(HostCommand::Shutdown);
        }
    }

    /// Whether shutdown was requested.
    #[must_use]
    pub fn shutdown_requested(&self) -> bool { self.shutdown_requested.load(Ordering::SeqCst) }

    /// Waits until the runner finished all teardown.
    ///
    /// Call only off the main thread.
    #[must_use]
    pub fn wait_for_exit(&self, timeout: Duration) -> bool {
        let done = self.finished.done.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *done {
            return true;
        }
        let (done, _) = self
            .finished
            .signal
            .wait_timeout_while(done, timeout, |done| !*done)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *done
    }

    /// Whether teardown finished.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        *self.finished.done.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn send(&self, command: HostCommand) { let _ = self.sender.send(command); }

    fn publish(&self, status: HostStatus) {
        *self.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = status;
    }

    fn finish(&self) {
        let mut done = self.finished.done.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *done = true;
        drop(done);
        self.finished.signal.notify_all();
    }
}

/// Creates the command handle and the not-yet-started driver.
#[must_use]
pub fn new_host() -> (HostHandle, HostDriver) {
    let (sender, receiver) = mpsc::channel();
    let handle = HostHandle {
        sender,
        status: Arc::new(Mutex::new(HostStatus::default())),
        shutdown_requested: Arc::new(AtomicBool::new(false)),
        finished: Arc::new(Finished {
            done: Mutex::new(false),
            signal: Condvar::new(),
        }),
    };
    (handle.clone(), HostDriver { receiver, handle })
}

/// The unstarted host runner. The application creates the tray first, then
/// calls [`HostDriver::start`].
pub struct HostDriver {
    receiver: Receiver<HostCommand>,
    handle: HostHandle,
}

impl HostDriver {
    /// Spawns the host worker thread.
    ///
    /// # Errors
    /// Returns a typed error when the worker thread cannot be created.
    pub fn start(
        self,
        settings: HostSettings,
        services: HostServices,
        ui: Arc<dyn HostUi>,
    ) -> Result<(), HostError> {
        thread::Builder::new()
            .name("stache-host".to_owned())
            .spawn(move || Runner::new(settings, services, self.handle, ui).run(&self.receiver))
            .map(|_| ())
            .map_err(|error| HostError::Thread(format!("cannot start host worker: {error}")))
    }
}

enum HostCommand {
    SetEnabled { id: String, enabled: bool },
    Pause(String),
    Resume(String),
    Retry(String),
    RefreshPermissions(RefreshTrigger),
    PermissionTick,
    Refresh,
    StartupFinished(Option<String>),
    ConfigChanges(ChangeSet),
    WatcherFailed(String),
    OpenTestWindow,
    Shutdown,
}

struct Runner {
    settings: HostSettings,
    services: HostServices,
    handle: HostHandle,
    ui: Arc<dyn HostUi>,
    registry: Arc<Registry>,
    store: Arc<ConfigStore>,
    controls: BTreeMap<String, Arc<dyn ModuleControl>>,
    permissions: PermissionManager,
    schedule: PollSchedule,
    tick_gate: TickGate,
    ticker: Option<PermissionTicker>,
    startup: Option<StartupJob>,
    ipc: Option<IpcServer>,
    watcher: Option<WatcherJob>,
    /// Conditions that stay until the host restarts, such as a dead config
    /// watcher or a failed startup command. A later successful action never
    /// hides them.
    persistent_errors: Vec<String>,
    /// The last failed tray or host action, cleared by a later successful one.
    last_action_error: Option<String>,
    ready: bool,
}

struct WatcherJob {
    stop: CancellationToken,
    thread: JoinHandle<()>,
}

impl WatcherJob {
    fn stop(self) {
        self.stop.cancel();
        let _ = self.thread.join();
    }
}

impl Runner {
    fn new(
        settings: HostSettings,
        services: HostServices,
        handle: HostHandle,
        ui: Arc<dyn HostUi>,
    ) -> Self {
        let registry = Arc::new(Registry::new(
            Arc::clone(&services.logger),
            Arc::clone(&services.events),
            Arc::clone(&services.main_thread),
            Arc::clone(&services.permissions),
            Arc::clone(&services.windows),
        ));
        let store = Arc::new(ConfigStore::new(
            Arc::clone(&registry),
            settings.catalog.clone(),
            settings.config.clone(),
            Arc::clone(&services.log_filter),
        ));
        let schedule = PollSchedule::new(services.poll);
        let permissions = PermissionManager::new(Arc::clone(&services.permissions));
        Self {
            settings,
            services,
            handle,
            ui,
            registry,
            store,
            controls: BTreeMap::new(),
            permissions,
            schedule,
            tick_gate: TickGate::new(),
            ticker: None,
            startup: None,
            ipc: None,
            watcher: None,
            persistent_errors: Vec::new(),
            last_action_error: None,
            ready: false,
        }
    }

    fn run(mut self, receiver: &Receiver<HostCommand>) {
        if self.handle.shutdown_requested() {
            self.finish(0);
            return;
        }
        if let Err(error) = self.start() {
            self.persistent_errors.push(error.to_string());
            self.log_warn(format!("host setup failed: {error}"));
            self.publish();
            self.teardown();
            self.finish(1);
            return;
        }
        if self.handle.shutdown_requested() {
            self.teardown();
            self.finish(0);
            return;
        }
        self.ready = true;
        self.publish();
        loop {
            if self.handle.shutdown_requested() {
                break;
            }
            let command = match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => command,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            if self.handle.shutdown_requested() {
                break;
            }
            match command {
                HostCommand::Shutdown => break,
                HostCommand::SetEnabled { id, enabled } => self.set_enabled(&id, enabled),
                HostCommand::Pause(id) => self.pause(&id),
                HostCommand::Resume(id) => self.resume(&id),
                HostCommand::Retry(id) => self.retry(&id),
                HostCommand::RefreshPermissions(trigger) => {
                    self.refresh_permissions(trigger);
                }
                HostCommand::PermissionTick => {
                    self.tick_gate.finish();
                    self.permission_tick();
                }
                HostCommand::Refresh => {
                    self.refresh_permissions(RefreshTrigger::Tick);
                }
                HostCommand::StartupFinished(error) => {
                    if let Some(error) = error {
                        self.add_condition(error);
                    }
                }
                HostCommand::ConfigChanges(changes) => self.apply_config(&changes),
                HostCommand::WatcherFailed(message) => self.add_condition(message),
                HostCommand::OpenTestWindow => self.open_test_window(),
            }
        }
        self.teardown();
        self.finish(0);
    }

    fn start(&mut self) -> Result<(), HostError> {
        for factory in self.settings.factories.entries() {
            let id = factory.id();
            let control = factory.register(&self.registry, &self.store)?;
            if control.id() != id {
                return Err(HostError::Thread(format!(
                    "factory `{id}` registered module `{}`",
                    control.id()
                )));
            }
            self.controls.insert(id.as_str().to_owned(), control);
        }
        if self.handle.shutdown_requested() {
            return Ok(());
        }
        if let Initial::Invalid(diagnostic) = self.store.load_global() {
            self.log_warn(format!("global configuration is invalid: {diagnostic}"));
        }
        if let Err(error) = self.registry.start_all() {
            self.log_warn(format!("one or more modules failed to start: {error}"));
        }
        if self.handle.shutdown_requested() {
            return Ok(());
        }
        let context = FactoryContext::new(Arc::clone(&self.registry), Arc::clone(&self.store));
        let mut router = Router::new(Arc::new(HostCore {
            core: ConfigCore::new(Arc::clone(&self.store)),
            sender: self.handle.sender.clone(),
        }));
        for factory in self.settings.factories.entries() {
            for (name, handler) in factory.ipc_handlers(&context) {
                router.register(name, handler)?;
            }
        }
        self.ipc = Some(IpcServer::start(&self.settings.app, Arc::new(router))?);
        self.start_watcher()?;
        self.start_ticker();
        self.refresh_permissions(RefreshTrigger::Startup);
        if !self.handle.shutdown_requested() {
            self.start_startup_commands();
        }
        Ok(())
    }

    fn start_watcher(&mut self) -> Result<(), HostError> {
        let mut watcher = ConfigWatcher::new(&self.settings.config, self.store.catalog())?;
        let stop = CancellationToken::new();
        let worker_stop = stop.clone();
        let sender = self.handle.sender.clone();
        let thread = thread::Builder::new()
            .name("stache-config".to_owned())
            .spawn(move || {
                loop {
                    if worker_stop.is_cancelled() {
                        break;
                    }
                    match watcher.next(Duration::from_millis(250)) {
                        Ok(Some(changes)) => {
                            if sender.send(HostCommand::ConfigChanges(changes)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let _ = sender.send(HostCommand::WatcherFailed(error.to_string()));
                            break;
                        }
                    }
                }
            })
            .map_err(|error| HostError::Thread(format!("cannot start config watcher: {error}")))?;
        self.watcher = Some(WatcherJob { stop, thread });
        Ok(())
    }

    fn start_ticker(&mut self) {
        let sender = self.handle.sender.clone();
        let gate = self.tick_gate.clone();
        match PermissionTicker::start(self.schedule.next(false), move || {
            if gate.try_begin() {
                let _ = sender.send(HostCommand::PermissionTick);
            }
        }) {
            Ok(ticker) => self.ticker = Some(ticker),
            Err(error) => self.log_warn(format!("permission polling is unavailable: {error}")),
        }
    }

    fn start_startup_commands(&mut self) {
        let Some(global) = self.store.global_config() else {
            return;
        };
        let commands: Vec<String> =
            global.exec_on_startup.commands().into_iter().map(str::to_owned).collect();
        if commands.is_empty() {
            return;
        }
        let sender = self.handle.sender.clone();
        let logger = Arc::clone(&self.services.logger);
        let runner = Arc::clone(&self.services.commands);
        let finished = move |result: Result<(), String>| match result {
            Ok(()) => {
                let _ = sender.send(HostCommand::StartupFinished(None));
            }
            Err(error) => {
                logger.log(stache_sdk::LogRecord {
                    level: stache_sdk::LogLevel::Error,
                    module: ModuleId::new("host"),
                    operation: stache_sdk::Operation::Start,
                    message: format!("execOnStartup failed: {error}"),
                });
                let _ = sender.send(HostCommand::StartupFinished(Some(format!(
                    "execOnStartup failed: {error}"
                ))));
            }
        };
        match StartupJob::spawn(commands, runner, finished) {
            Ok(job) => self.startup = Some(job),
            Err(error) => self.add_condition(format!("cannot start execOnStartup: {error}")),
        }
    }

    fn set_enabled(&mut self, id: &str, enabled: bool) {
        let Some(control) = self.controls.get(id).cloned() else {
            self.record_action_error(format!("no module is registered as `{id}`"));
            return;
        };
        let module = control.id();
        let edit = match plan_enabled(self.store.catalog(), self.store.paths(), module, enabled) {
            Ok(edit) => edit,
            Err(error) => {
                self.record_action_error(error.to_string());
                return;
            }
        };
        match edit.commit() {
            Ok(CommitOutcome::Written) => {}
            Ok(CommitOutcome::Unchanged) => {
                // The file already has the requested value. Apply it once if the
                // runtime has not accepted the current bytes, then retry an
                // eligible failed start instead of only clearing the error.
                if let Err(error) = self.sync_module_file(module) {
                    self.record_action_error(error);
                    return;
                }
                if let Err(error) = control.set_enabled(&self.registry, enabled) {
                    self.record_action_error(error.to_string());
                    return;
                }
                self.clear_action_error();
                self.refresh_permissions(RefreshTrigger::Tick);
                return;
            }
            Err(error) => {
                self.record_action_error(error.to_string());
                return;
            }
        }
        if let Err(error) = self.sync_module_file(module) {
            self.record_action_error(error);
            return;
        }
        // A successful write clears an older action failure even when the
        // permission refresh itself reports nothing new.
        self.clear_action_error();
        self.refresh_permissions(RefreshTrigger::Tick);
        // A failed reload during the permission refresh must not hide the
        // successful edit; `refresh_permissions` publishes.
    }

    fn sync_module_file(&self, module: ModuleId) -> Result<(), String> {
        match self.store.reload_one(module) {
            Ok(ReloadOutcome::Applied { .. } | ReloadOutcome::Unchanged) => Ok(()),
            Ok(ReloadOutcome::Rejected(diagnostic)) => {
                Err(format!("configuration was written but rejected: {diagnostic}"))
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn pause(&mut self, id: &str) {
        let Some(control) = self.controls.get(id).cloned() else {
            self.record_action_error(format!("no module is registered as `{id}`"));
            return;
        };
        match control.pause(&self.registry) {
            Ok(()) => self.clear_action_error(),
            Err(error) => self.record_action_error(error.to_string()),
        }
        self.refresh_permissions(RefreshTrigger::Tick);
    }

    fn resume(&mut self, id: &str) {
        let Some(control) = self.controls.get(id).cloned() else {
            self.record_action_error(format!("no module is registered as `{id}`"));
            return;
        };
        match control.resume(&self.registry) {
            Ok(()) => self.clear_action_error(),
            Err(error) => self.record_action_error(error.to_string()),
        }
        self.refresh_permissions(RefreshTrigger::Tick);
    }

    fn retry(&mut self, id: &str) {
        let Some(control) = self.controls.get(id).cloned() else {
            self.record_action_error(format!("no module is registered as `{id}`"));
            return;
        };
        let module = control.id();
        let snapshot = self.registry.snapshots().into_iter().find(|snapshot| snapshot.id == module);
        let Some(snapshot) = snapshot else {
            self.record_action_error(format!("module `{module}` has no published snapshot"));
            return;
        };
        // A failed state while disabled or paused can only be a failed stop, so
        // Retry repeats that request: it retries the cleanup and starts nothing.
        let failed = matches!(snapshot.status, ModuleStatus::Failed(_));
        let result = if !snapshot.enabled && failed {
            control.set_enabled(&self.registry, false)
        } else if snapshot.enabled && snapshot.paused && failed {
            control.pause(&self.registry)
        } else if snapshot.enabled && !snapshot.paused {
            control.set_enabled(&self.registry, true)
        } else {
            self.record_action_error(format!(
                "module `{module}` cannot retry while disabled or paused"
            ));
            return;
        };
        match result {
            Ok(()) => self.clear_action_error(),
            Err(error) => self.record_action_error(error.to_string()),
        }
        self.refresh_permissions(RefreshTrigger::Tick);
    }

    fn refresh_permissions(&mut self, trigger: RefreshTrigger) -> RefreshReport {
        let host = RegistryPermissions {
            registry: Arc::clone(&self.registry),
            logger: Arc::clone(&self.services.logger),
        };
        let report = self.permissions.refresh(&host, trigger);
        if let Some(error) = &report.prompt_error {
            self.last_action_error = Some(format!("permission request failed: {error}"));
        } else if report.changed || report.prompted.is_some() {
            self.last_action_error = None;
        }
        if let Some(ticker) = &self.ticker {
            ticker.set_delay(self.schedule.next(report.waiting));
        }
        self.publish();
        report
    }

    fn permission_tick(&mut self) {
        let host = RegistryPermissions {
            registry: Arc::clone(&self.registry),
            logger: Arc::clone(&self.services.logger),
        };
        let report = self.permissions.refresh(&host, RefreshTrigger::Tick);
        if let Some(ticker) = &self.ticker {
            ticker.set_delay(self.schedule.next(report.waiting));
        }
        if let Some(error) = &report.prompt_error {
            self.last_action_error = Some(format!("permission request failed: {error}"));
            self.publish();
        } else if report.changed || report.prompted.is_some() {
            self.publish();
        }
    }

    fn apply_config(&mut self, changes: &ChangeSet) {
        let outcomes = self.store.apply(changes);
        let rejected: Vec<String> = outcomes
            .iter()
            .filter_map(|file| file.outcome.diagnostic())
            .map(ToString::to_string)
            .collect();
        if rejected.is_empty() {
            self.clear_action_error();
        } else {
            self.last_action_error = Some(rejected.join("; "));
        }
        self.refresh_permissions(RefreshTrigger::Tick);
    }

    fn open_test_window(&mut self) {
        let Ok(key) = WindowKey::new("test") else {
            self.record_action_error("invalid built-in test window key".to_owned());
            return;
        };
        let Ok(size) = WindowSize::new(900, 640) else {
            self.record_action_error("invalid built-in test window size".to_owned());
            return;
        };
        let request =
            WindowRequest::new(key, crate::windows::TEST_SURFACE, size, WindowKind::Standard);
        match self.services.windows.open(HOST_OWNER, &request) {
            Ok(_) => self.clear_action_error(),
            Err(error) => self.record_action_error(error.to_string()),
        }
        self.publish();
    }

    fn teardown(&mut self) {
        // Stop ingress before dropping services that handlers may still use.
        if let Some(mut ipc) = self.ipc.take() {
            ipc.shutdown();
        }
        // Cancel monitors before their owners stop.
        if let Some(watcher) = self.watcher.take() {
            watcher.stop();
        }
        if let Some(ticker) = self.ticker.take() {
            ticker.stop();
        }
        if let Some(startup) = self.startup.take() {
            startup.stop();
        }
        // Registry teardown is reverse start order and may join module workers.
        // This thread is never the AppKit main thread.
        if let Err(error) = self.registry.shutdown() {
            self.log_warn(format!("module shutdown reported: {error}"));
        }
        self.services.windows.close_all();
        self.ready = false;
        self.publish();
    }

    fn finish(self, code: i32) {
        self.ui.exit(code);
        self.handle.finish();
    }

    fn publish(&self) {
        let status = HostStatus {
            ready: self.ready,
            error: self.error_text(),
            modules: self
                .registry
                .snapshots()
                .iter()
                .map(ModuleStatusView::from_snapshot)
                .collect(),
            config: self.config_views(),
        };
        self.handle.publish(status.clone());
        self.ui.status(&status);
    }

    fn config_views(&self) -> Vec<crate::bindings::ConfigFileView> {
        let entries: Vec<PathEntry> =
            inspect_paths(self.store.catalog(), self.store.paths()).entries;
        entries.iter().map(|entry| config_file_view(entry, &self.store)).collect()
    }

    fn record_action_error(&mut self, message: String) {
        self.last_action_error = Some(message);
        self.publish();
    }

    fn clear_action_error(&mut self) { self.last_action_error = None; }

    fn add_condition(&mut self, message: String) {
        self.persistent_errors.push(message);
        self.publish();
    }

    /// The published error text: persistent conditions first, then the last
    /// action failure. The DTO keeps one string, so the UI and generated
    /// bindings keep their shape.
    fn error_text(&self) -> Option<String> {
        if self.persistent_errors.is_empty() && self.last_action_error.is_none() {
            return None;
        }
        let mut parts = self.persistent_errors.clone();
        parts.extend(self.last_action_error.clone());
        Some(parts.join("; "))
    }

    fn log_warn(&self, message: String) { self.log(stache_sdk::LogLevel::Warn, message); }

    fn log(&self, level: stache_sdk::LogLevel, message: String) {
        self.services.logger.log(stache_sdk::LogRecord {
            level,
            module: ModuleId::new("host"),
            operation: stache_sdk::Operation::Start,
            message,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use stache_sdk::{
        AppEnvironment, AppPaths, DiscardLogger, EventBus, LogFilter, PermissionError,
        PermissionStatus, ThreadError, WindowId,
    };

    use super::*;

    struct AllowAll;

    impl PermissionProvider for AllowAll {
        fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

        fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
            Ok(PermissionStatus::Granted)
        }
    }

    struct NoMainThread;

    impl MainThreadExecutor for NoMainThread {
        fn is_main_thread(&self) -> bool { false }

        fn execute(&self, _: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            Ok(())
        }
    }

    struct NoWindows;

    impl WindowBackend for NoWindows {
        fn open(&self, _: ModuleId, _: &WindowRequest) -> Result<WindowId, WindowError> {
            Ok(WindowId::new(1))
        }

        fn focus(&self, _: WindowId) -> Result<(), WindowError> { Ok(()) }

        fn close(&self, _: WindowId) {}

        fn release_owner(&self, _: ModuleId) {}

        fn close_all(&self) {}
    }

    struct NoCommands;

    impl CommandRunner for NoCommands {
        fn run(&self, _: &[String], _: &CancellationToken) -> Result<(), String> { Ok(()) }
    }

    struct QuietUi;

    impl HostUi for QuietUi {
        fn status(&self, _: &HostStatus) {}

        fn exit(&self, _: i32) {}
    }

    fn runner(root: &Path) -> Runner {
        let app = AppPaths::resolve(&AppEnvironment {
            home: Some(root.to_path_buf()),
            temporary: Some(root.to_path_buf()),
        })
        .expect("app paths");
        let settings = HostSettings {
            app,
            config: ConfigPaths::new(root.join("config")),
            catalog: ConfigCatalog::new(),
            factories: ModuleFactories::new(),
        };
        let services = HostServices {
            logger: Arc::new(DiscardLogger),
            log_filter: Arc::new(LogFilter::default()),
            events: Arc::new(EventBus::new()),
            main_thread: Arc::new(NoMainThread),
            permissions: Arc::new(AllowAll),
            windows: Arc::new(NoWindows),
            commands: Arc::new(NoCommands),
            poll: PermissionPoll {
                waiting: Duration::from_millis(1),
                backoff_start: Duration::from_millis(1),
                backoff_max: Duration::from_millis(1),
            },
        };
        let (handle, _driver) = new_host();
        Runner::new(settings, services, handle, Arc::new(QuietUi))
    }

    #[test]
    fn production_factories_register_the_audio_pilot_the_blocker_and_keep_awake() {
        let root = tempfile::tempdir().expect("root");
        let registry = Arc::new(Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(NoMainThread),
            Arc::new(AllowAll),
            Arc::new(NoWindows),
        ));
        let store = Arc::new(ConfigStore::new(
            Arc::clone(&registry),
            crate::catalog::catalog().expect("catalog"),
            ConfigPaths::new(root.path().join("config")),
            Arc::new(LogFilter::default()),
        ));
        let factories = ModuleFactories::production(Arc::new(NoMainThread));
        let ids: Vec<ModuleId> = factories.entries().iter().map(|factory| factory.id()).collect();
        assert_eq!(
            ids,
            [
                stache_proxy_audio::ProxyAudioConfig::ID,
                stache_notunes::NoTunesConfig::ID,
                stache_keep_awake::KeepAwakeConfig::ID,
            ],
            "production registers the audio pilot, the blocker and then Keep Awake"
        );

        for factory in factories.entries() {
            let id = factory.id();
            let control = factory.register(&registry, &store).expect("register");
            assert_eq!(control.id(), id);
            let snapshot = registry
                .snapshots()
                .into_iter()
                .find(|snapshot| snapshot.id == id)
                .expect("snapshot");
            assert_eq!(
                snapshot.status,
                ModuleStatus::Disabled,
                "registration must not start `{id}`"
            );
        }

        // Binding the default-disabled file starts no assertion, probe, observer,
        // worker or timer and never creates the configuration file.
        let keep_awake = registry
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.id == stache_keep_awake::KeepAwakeConfig::ID)
            .expect("keepAwake snapshot");
        assert!(!keep_awake.enabled, "the default configuration is disabled");
        assert!(
            !store.paths().module_file(stache_keep_awake::KeepAwakeConfig::ID).exists(),
            "registration never writes the configuration file"
        );

        let context = FactoryContext::new(Arc::clone(&registry), Arc::clone(&store));
        let audio = &factories.entries()[0];
        let handlers = audio.ipc_handlers(&context);
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].0, "proxyAudio");
        assert!(
            factories.entries()[1].ipc_handlers(&context).is_empty(),
            "the blocker contributes no command or socket handler"
        );
        let handlers = factories.entries()[2].ipc_handlers(&context);
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].0, "keepAwake");
    }

    #[test]
    fn a_watcher_failure_survives_a_later_successful_action() {
        let root = tempfile::tempdir().expect("root");
        let mut runner = runner(root.path());
        runner.record_action_error("tray action failed".to_owned());
        runner.add_condition("configuration watcher failed: gone".to_owned());
        assert_eq!(
            runner.error_text().as_deref(),
            Some("configuration watcher failed: gone; tray action failed")
        );

        runner.apply_config(&ChangeSet::default());
        assert_eq!(
            runner.error_text().as_deref(),
            Some("configuration watcher failed: gone"),
            "an accepted change clears only the last action error"
        );
    }
}
