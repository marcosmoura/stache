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
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use stache_macos::ApplicationControl;
use stache_macos::audio::{AudioBackend, CoreAudioBackend};
use stache_notunes::{NoTunesConfig, NoTunesModule};
use stache_proxy_audio::{AudioIpc, ProxyAudioConfig, ProxyAudioModule};
use stache_sdk::ipc::{
    ConfigCore, CoreHandler, CoreReload, CoreStatus, HandlerResult, IpcHandler, IpcServer, Router,
    RouterError, ServerError, SocketShutdown,
};
use stache_sdk::shutdown::{
    CANCELLATION_QUANTUM, OWNER_WAIT_BUDGET, ShutdownWait, wait_for_finish,
};
use stache_sdk::{
    CancellationToken, ChangeSet, ConfigCatalog, ConfigPaths, ConfigStore, ConfigStoreError,
    ConfigWatcher, EventBus, Initial, LogFilter, Logger, MainThreadExecutor, Module, ModuleConfig,
    ModuleHandle, ModuleId, ModuleSnapshot, ModuleStatus, PathEntry, Permission,
    PermissionProvider, PermissionStatus, Registry, RegistryError, ReloadOutcome, WatchError,
    WindowBackend, WindowError, WindowKey, WindowKind, WindowRequest, WindowSize, inspect_paths,
};

use crate::bindings::{HostStatus, ModuleStatusView, config_file_view};
use crate::config_edit::{CommitOutcome, EnableEditError, plan_enabled};
use crate::permissions::{
    PermissionHost, PermissionManager, PermissionPoll, PermissionTicker, PollSchedule,
    RefreshReport, RefreshTrigger, TickGate,
};
use crate::shutdown::{
    HOST_WORKER, NativeSlot, SlotOutcome, StageReport, Teardown, TeardownCause, TeardownOutcome,
    TeardownStages,
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
    /// module name and handler here.
    fn ipc_handlers(&self) -> Vec<(&'static str, Arc<dyn IpcHandler>)> { Vec::new() }
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

    fn ipc_handlers(&self) -> Vec<(&'static str, Arc<dyn IpcHandler>)> {
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

/// The factories the composition root registers.
#[derive(Default)]
pub struct ModuleFactories {
    entries: Vec<Arc<dyn ModuleFactory>>,
}

impl ModuleFactories {
    /// Creates an empty factory list.
    #[must_use]
    pub const fn new() -> Self { Self { entries: Vec::new() } }

    /// Creates the production list: the proxyAudio pilot and the noTunes blocker.
    ///
    /// Registration only creates the modules and the pilot's read-only handler;
    /// it starts no listener and installs no application observer. Starting
    /// depends on the accepted configuration. Both factories share the host's
    /// main-thread executor; neither module keeps a second clone of it.
    #[must_use]
    pub fn production(main_thread: Arc<dyn MainThreadExecutor>) -> Self {
        let backend: Arc<dyn AudioBackend> = Arc::new(CoreAudioBackend);
        Self::new()
            .with(ProxyAudioFactory::new(backend))
            .with(NoTunesFactory::new(ApplicationControl::system(main_thread)))
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
    teardown: Arc<Teardown>,
    finished: Arc<Finished>,
}

struct Finished {
    done: Mutex<bool>,
    signal: Condvar,
}

impl Finished {
    fn is_finished(&self) -> bool {
        *self.done.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
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

    /// Requests the idempotent teardown coordinator.
    pub fn request_shutdown(&self) {
        self.teardown.request(TeardownCause::Request, None);
        self.send(HostCommand::Shutdown);
    }

    /// Requests teardown from Tauri's exit event, keeping its first non-zero code.
    pub fn request_shutdown_with_code(&self, code: Option<i32>) {
        self.teardown.request(TeardownCause::Exit, code);
        self.send(HostCommand::Shutdown);
    }

    /// Requests one coordinated restart through the same coordinator.
    pub fn request_restart(&self) {
        self.teardown.request(TeardownCause::Restart, None);
        self.send(HostCommand::Shutdown);
    }

    /// Whether a teardown request has arrived.
    #[must_use]
    pub fn shutdown_requested(&self) -> bool { self.teardown.requested() }

    /// The stored teardown outcome, if the coordinator reached its deadline.
    #[must_use]
    pub fn teardown_outcome(&self) -> Option<TeardownOutcome> { self.teardown.outcome() }

    /// Waits, off the main thread, until a teardown request arrives.
    #[must_use]
    pub fn wait_until_teardown_requested(&self, timeout: Duration) -> bool {
        self.teardown.wait_until_requested(timeout)
    }

    /// Keeps one condition visible until the host restarts.
    pub fn note_condition(&self, message: String) { self.send(HostCommand::Condition(message)); }

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

/// Creates the command handle over one coordinator and the not-yet-started driver.
#[must_use]
pub fn new_host(teardown: Arc<Teardown>) -> (HostHandle, HostDriver) {
    let (sender, receiver) = mpsc::channel();
    let handle = HostHandle {
        sender,
        status: Arc::new(Mutex::new(HostStatus::default())),
        teardown,
        finished: Arc::new(Finished {
            done: Mutex::new(false),
            signal: Condvar::new(),
        }),
    };
    (handle.clone(), HostDriver { receiver, handle })
}

/// The jobs and socket the coordinator removes during teardown.
///
/// The runner starts them and the stage driver takes them out of these cells,
/// so neither side needs the other's thread to be reachable when a stop hangs.
/// Stage 1 closes the cells first: a job that finishes starting afterwards is
/// handed back to the runner, which cancels and reaps it itself.
#[derive(Default)]
struct MonitorCells {
    ipc: Mutex<Option<IpcServer>>,
    watcher: Mutex<Option<WatcherJob>>,
    ticker: Mutex<Option<PermissionTicker>>,
    startup: Mutex<Option<StartupJob>>,
    closed: AtomicBool,
}

impl MonitorCells {
    /// Closes every cell before the stage driver empties them.
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(ipc) = self.ipc.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            ipc.stop_accepting();
        }
        if let Some(watcher) = self.watcher.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        {
            watcher.cancel();
        }
        if let Some(ticker) = self.ticker.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            ticker.cancel();
        }
        if let Some(startup) = self.startup.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        {
            startup.cancel();
        }
    }

    /// Installs a job unless stage 1 already closed the cells.
    ///
    /// Installing and taking share the cell lock, so a job is either seen by the
    /// stage driver or handed back to the runner to stop itself.
    fn install<T>(&self, cell: &Mutex<Option<T>>, job: T) -> Option<T> {
        let mut slot = cell.lock().unwrap_or_else(PoisonError::into_inner);
        if self.closed.load(Ordering::SeqCst) {
            return Some(job);
        }
        *slot = Some(job);
        None
    }
}

/// The production teardown stages over the shared host pieces.
pub struct HostStages {
    registry: Arc<Registry>,
    monitors: Arc<MonitorCells>,
    setup_failed: Arc<AtomicBool>,
    finished: Arc<Finished>,
    status: Arc<Mutex<HostStatus>>,
    ui: Arc<dyn HostUi>,
}

impl HostStages {
    fn take<T>(cell: &Mutex<Option<T>>) -> Option<T> {
        cell.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    /// Waits for the runner until `target`, in one-quantum slices.
    ///
    /// The whole wait shares the stage's start, so it is observed alongside the
    /// monitor reaping and never adds a second serial owner budget.
    fn wait_for_runner_until(&self, wait: &dyn ShutdownWait, target: Instant) -> bool {
        loop {
            if self.finished.is_finished() {
                return true;
            }
            let now = wait.now();
            if now >= target {
                return false;
            }
            let deadline = wait.deadline();
            if now >= deadline {
                return false;
            }
            wait.wait(
                CANCELLATION_QUANTUM
                    .min(target.saturating_duration_since(now))
                    .min(deadline.saturating_duration_since(now)),
            );
        }
    }
}

impl TeardownStages for HostStages {
    fn invalidate(&self) {
        // Close the monitor cells first so a job that finishes starting after
        // this point is handed back to the runner instead of installed into a
        // cell the stage driver already emptied.
        self.monitors.close();
        self.registry.invalidate();
    }

    fn reap_monitors(&self, wait: Arc<dyn ShutdownWait>) -> StageReport {
        // The runner budget shares this stage's start, so waiting for it adds no
        // serial owner budget after the monitor reaping below.
        let started = wait.now();
        let target = started.checked_add(OWNER_WAIT_BUDGET).unwrap_or(started);
        let mut report = StageReport::default();
        if let Some(watcher) = Self::take(&self.monitors.watcher)
            && !watcher.stop_bounded(wait.as_ref())
        {
            report.unacknowledged.push("watcher".to_owned());
        }
        if let Some(ticker) = Self::take(&self.monitors.ticker)
            && !ticker.stop_bounded(wait.as_ref())
        {
            report.unacknowledged.push("permission-ticker".to_owned());
        }
        if let Some(startup) = Self::take(&self.monitors.startup)
            && !startup.stop_bounded(wait.as_ref())
        {
            report.unacknowledged.push("startup-job".to_owned());
        }
        if self.setup_failed.load(Ordering::SeqCst) {
            report.failed.push(HOST_WORKER.to_owned());
        } else if !self.wait_for_runner_until(wait.as_ref(), target) {
            report.unacknowledged.push(HOST_WORKER.to_owned());
        }
        report
    }

    fn stop_registry(&self, wait: Arc<dyn ShutdownWait>) -> StageReport {
        let mut report = StageReport::default();
        for target in self.registry.shutdown_targets() {
            let name = format!("module:{}", target.id.as_str());
            let registry = Arc::clone(&self.registry);
            let worker_wait = Arc::clone(&wait);
            let carrier = thread::Builder::new()
                .name(format!("stache-stop-{}", target.id.as_str()))
                .spawn(move || registry.stop_module_bounded(&target, worker_wait.as_ref()));
            match carrier {
                Ok(handle) => {
                    if wait_for_finish(wait.as_ref(), &|| handle.is_finished()) {
                        match handle.join() {
                            Ok(Ok(true)) => {}
                            Ok(Ok(false)) => report.unacknowledged.push(name),
                            Ok(Err(_)) | Err(_) => report.failed.push(name),
                        }
                    } else {
                        // The carrier is detached and keeps what it owns; the
                        // next module still stops.
                        report.unacknowledged.push(name);
                    }
                }
                Err(_) => report.failed.push(name),
            }
        }
        report
    }

    fn cleanup(&self, slot: NativeSlot, wait: Arc<dyn ShutdownWait>) -> SlotOutcome {
        match slot {
            NativeSlot::Socket => {
                let Some(ipc) = Self::take(&self.monitors.ipc) else {
                    return SlotOutcome::Empty;
                };
                // Never hold the cell lock here: the bounded shutdown waits for
                // the accept thread and every handler permit, then always
                // removes the owned endpoint.
                match ipc.shutdown_bounded(wait.as_ref()) {
                    SocketShutdown::Acknowledged => SlotOutcome::Acknowledged,
                    SocketShutdown::Unacknowledged => SlotOutcome::Unacknowledged,
                    SocketShutdown::RemovalFailed(_) => SlotOutcome::Failed,
                }
            }
            // Registered owners acknowledge the webview, tray, windows and the
            // four module-owned slots themselves.
            NativeSlot::Tray
            | NativeSlot::Webview
            | NativeSlot::Windows
            | NativeSlot::PowerAssertion
            | NativeSlot::InputTap
            | NativeSlot::ApplicationObservers
            | NativeSlot::Hud => SlotOutcome::Empty,
        }
    }

    fn publish_terminal(&self) {
        let mut status = self.status.lock().unwrap_or_else(PoisonError::into_inner).clone();
        status.ready = false;
        status.modules =
            self.registry.snapshots().iter().map(ModuleStatusView::from_snapshot).collect();
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status.clone();
        self.ui.status(&status);
    }
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
    /// The registry, the monitor cells and the setup-failure flag exist before
    /// the runner starts, so the coordinator's stages can reach them even when
    /// the runner is stuck in a command.
    ///
    /// # Errors
    /// Returns a typed error when the worker thread cannot be created.
    pub fn start(
        self,
        settings: HostSettings,
        services: HostServices,
        ui: Arc<dyn HostUi>,
    ) -> Result<(), HostError> {
        let registry = Arc::new(Registry::new(
            Arc::clone(&services.logger),
            Arc::clone(&services.events),
            Arc::clone(&services.main_thread),
            Arc::clone(&services.permissions),
            Arc::clone(&services.windows),
        ));
        let monitors = Arc::new(MonitorCells::default());
        let setup_failed = Arc::new(AtomicBool::new(false));
        self.handle.teardown.install_stages(Arc::new(HostStages {
            registry: Arc::clone(&registry),
            monitors: Arc::clone(&monitors),
            setup_failed: Arc::clone(&setup_failed),
            finished: Arc::clone(&self.handle.finished),
            status: Arc::clone(&self.handle.status),
            ui: Arc::clone(&ui),
        }));
        let finished = self.handle.clone();
        let runner = Runner::new(
            settings,
            services,
            self.handle,
            ui,
            registry,
            monitors,
            setup_failed,
        );
        let spawned = thread::Builder::new()
            .name("stache-host".to_owned())
            .spawn(move || runner.run(&self.receiver));
        if spawned.is_err() {
            // A worker that never started must not read as a hung one.
            finished.finish();
        }
        spawned
            .map(|_| ())
            .map_err(|error| HostError::Thread(format!("cannot start host worker: {error}")))
    }
}

enum HostCommand {
    SetEnabled { id: String, enabled: bool },
    Condition(String),
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
    monitors: Arc<MonitorCells>,
    setup_failed: Arc<AtomicBool>,
    store: Arc<ConfigStore>,
    controls: BTreeMap<String, Arc<dyn ModuleControl>>,
    permissions: PermissionManager,
    schedule: PollSchedule,
    tick_gate: TickGate,
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

/// A real-time bound for a job the runner reaps because stage 1 already closed
/// the monitor cells. Teardown never waits on it.
struct LateReapWait {
    deadline: Instant,
}

impl LateReapWait {
    fn new() -> Self {
        Self {
            deadline: Instant::now() + OWNER_WAIT_BUDGET,
        }
    }
}

impl ShutdownWait for LateReapWait {
    fn now(&self) -> Instant { Instant::now() }

    fn deadline(&self) -> Instant { self.deadline }

    fn wait(&self, timeout: Duration) { thread::sleep(timeout.min(CANCELLATION_QUANTUM)); }
}

impl WatcherJob {
    fn cancel(&self) { self.stop.cancel(); }

    fn stop_bounded(self, wait: &dyn ShutdownWait) -> bool {
        self.cancel();
        let thread = self.thread;
        if wait_for_finish(wait, &|| thread.is_finished()) {
            let _ = thread.join();
            true
        } else {
            // Dropping the handle detaches the thread instead of joining it.
            false
        }
    }
}

impl Runner {
    #[allow(clippy::too_many_arguments)] // The driver builds these pieces once.
    fn new(
        settings: HostSettings,
        services: HostServices,
        handle: HostHandle,
        ui: Arc<dyn HostUi>,
        registry: Arc<Registry>,
        monitors: Arc<MonitorCells>,
        setup_failed: Arc<AtomicBool>,
    ) -> Self {
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
            monitors,
            setup_failed,
            store,
            controls: BTreeMap::new(),
            permissions,
            schedule,
            tick_gate: TickGate::new(),
            persistent_errors: Vec::new(),
            last_action_error: None,
            ready: false,
        }
    }

    fn run(mut self, receiver: &Receiver<HostCommand>) {
        if self.handle.shutdown_requested() {
            // Nothing started; the coordinator's stages have nothing to stop.
            self.finish(true);
            return;
        }
        if let Err(error) = self.start() {
            self.persistent_errors.push(error.to_string());
            self.log_warn(format!("host setup failed: {error}"));
            self.publish();
            self.finish(false);
            return;
        }
        if self.handle.shutdown_requested() {
            self.finish(true);
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
            // Once shutdown is requested, queued non-teardown work is dropped
            // instead of started.
            if self.handle.shutdown_requested() {
                break;
            }
            match command {
                HostCommand::Shutdown => break,
                HostCommand::Condition(message) | HostCommand::WatcherFailed(message) => {
                    self.add_condition(message);
                }
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
                HostCommand::OpenTestWindow => self.open_test_window(),
            }
        }
        self.finish(true);
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
        let mut router = Router::new(Arc::new(HostCore {
            core: ConfigCore::new(Arc::clone(&self.store)),
            sender: self.handle.sender.clone(),
        }));
        for factory in self.settings.factories.entries() {
            for (name, handler) in factory.ipc_handlers() {
                router.register(name, handler)?;
            }
        }
        if self.handle.shutdown_requested() {
            return Ok(());
        }
        let server = IpcServer::start(&self.settings.app, Arc::new(router))?;
        if let Some(mut server) = self.monitors.install(&self.monitors.ipc, server) {
            // Stage 1 closed the cells while the listener was starting.
            server.shutdown();
            return Ok(());
        }
        self.start_watcher()?;
        self.start_ticker();
        self.refresh_permissions(RefreshTrigger::Startup);
        if !self.handle.shutdown_requested() {
            self.start_startup_commands();
        }
        Ok(())
    }

    fn start_watcher(&self) -> Result<(), HostError> {
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
        let job = WatcherJob { stop, thread };
        if let Some(job) = self.monitors.install(&self.monitors.watcher, job) {
            // Stage 1 already closed the cells: reap it here instead.
            job.stop_bounded(&LateReapWait::new());
        }
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
            Ok(ticker) => {
                if let Some(ticker) = self.monitors.install(&self.monitors.ticker, ticker) {
                    ticker.stop_bounded(&LateReapWait::new());
                }
            }
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
            Ok(job) => {
                if let Some(job) = self.monitors.install(&self.monitors.startup, job) {
                    // Stage 1 already closed the cells: the job kills its own
                    // child and stops before the next command.
                    job.stop_bounded(&LateReapWait::new());
                }
            }
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
                if self.handle.shutdown_requested() {
                    return;
                }
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
        if self.handle.shutdown_requested() {
            // The committed write is not rolled back, but no registry or
            // permission work may follow a teardown request.
            return;
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
        let eligible = self
            .registry
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.id == module)
            .is_some_and(|snapshot| snapshot.enabled && !snapshot.paused);
        if !eligible {
            self.record_action_error(format!(
                "module `{module}` cannot retry while disabled or paused"
            ));
            return;
        }
        if self.handle.shutdown_requested() {
            return;
        }
        match control.set_enabled(&self.registry, true) {
            Ok(()) => self.clear_action_error(),
            Err(error) => self.record_action_error(error.to_string()),
        }
        self.refresh_permissions(RefreshTrigger::Tick);
    }

    fn refresh_permissions(&mut self, trigger: RefreshTrigger) -> RefreshReport {
        if self.handle.shutdown_requested() {
            // A late refresh could prompt or apply a status after invalidation.
            return RefreshReport::default();
        }
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
        if let Some(ticker) =
            self.monitors.ticker.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        {
            ticker.set_delay(self.schedule.next(report.waiting));
        }
        self.publish();
        report
    }

    fn permission_tick(&mut self) {
        if self.handle.shutdown_requested() {
            return;
        }
        let host = RegistryPermissions {
            registry: Arc::clone(&self.registry),
            logger: Arc::clone(&self.services.logger),
        };
        let report = self.permissions.refresh(&host, RefreshTrigger::Tick);
        if let Some(ticker) =
            self.monitors.ticker.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        {
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
        if self.handle.shutdown_requested() {
            // The accepted write stays; no permission refresh or publication
            // may follow a teardown request.
            return;
        }
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
        if self.handle.shutdown_requested() {
            // A request that arrived while this command waited must not open a
            // window after the hosts' windows were released.
            return;
        }
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

    /// Publishes the final ready=false status and releases `wait_for_exit`.
    ///
    /// A setup failure records the flag the stage driver reports as
    /// `host-worker` failed. The final status is published before the request
    /// is created, so the one terminal publication carries the setup error; a
    /// request that already exists leaves publication to that terminal status.
    fn finish(mut self, success: bool) {
        self.ready = false;
        if !success {
            self.setup_failed.store(true, Ordering::SeqCst);
        }
        self.publish();
        if !success && !self.handle.shutdown_requested() {
            self.handle.teardown.request(TeardownCause::Request, None);
        }
        self.handle.finish();
    }

    fn publish(&self) {
        if self.handle.shutdown_requested() {
            // `HostStages::publish_terminal` is the one terminal status. A late
            // command completion cannot publish over it.
            return;
        }
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
    }

    struct NoCommands;

    impl CommandRunner for NoCommands {
        fn run(&self, _: &[String], _: &CancellationToken) -> Result<(), String> { Ok(()) }
    }

    struct QuietUi;

    impl HostUi for QuietUi {
        fn status(&self, _: &HostStatus) {}
    }

    fn quiet_teardown() -> Arc<Teardown> {
        Teardown::new(
            Arc::new(crate::shutdown::SystemClock),
            Arc::new(crate::shutdown::DiscardRecordChannel),
            Arc::new(crate::shutdown::InstanceClaim::default()),
            Arc::new(DiscardLogger),
        )
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
        let registry = Arc::new(Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(NoMainThread),
            Arc::new(AllowAll),
            Arc::new(NoWindows),
        ));
        let (handle, _driver) = new_host(quiet_teardown());
        Runner::new(
            settings,
            services,
            handle,
            Arc::new(QuietUi),
            registry,
            Arc::new(MonitorCells::default()),
            Arc::new(AtomicBool::new(false)),
        )
    }

    #[test]
    fn production_factories_register_the_audio_pilot_and_the_notunes_blocker() {
        let root = tempfile::tempdir().expect("root");
        let registry = Arc::new(Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(NoMainThread),
            Arc::new(AllowAll),
            Arc::new(NoWindows),
        ));
        let store = ConfigStore::new(
            Arc::clone(&registry),
            crate::catalog::catalog().expect("catalog"),
            ConfigPaths::new(root.path().join("config")),
            Arc::new(LogFilter::default()),
        );
        let factories = ModuleFactories::production(Arc::new(NoMainThread));
        let ids: Vec<ModuleId> = factories.entries().iter().map(|factory| factory.id()).collect();
        assert_eq!(
            ids,
            [
                stache_proxy_audio::ProxyAudioConfig::ID,
                stache_notunes::NoTunesConfig::ID,
            ],
            "production registers the audio pilot first and the blocker second"
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

        let audio = &factories.entries()[0];
        let handlers = audio.ipc_handlers();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].0, "proxyAudio");
        assert!(
            factories.entries()[1].ipc_handlers().is_empty(),
            "the blocker contributes no command or socket handler"
        );
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

    /// A wait whose live deadline already expired: no slice may sleep.
    struct ExpiredWait {
        now: std::time::Instant,
    }

    impl ShutdownWait for ExpiredWait {
        fn now(&self) -> std::time::Instant { self.now }

        fn deadline(&self) -> std::time::Instant { self.now }

        fn wait(&self, _timeout: Duration) {}
    }

    /// A real wait with an owner budget left, used for a cooperative owner.
    struct ShortWait {
        deadline: std::time::Instant,
    }

    impl ShortWait {
        fn new() -> Self {
            Self {
                deadline: std::time::Instant::now() + Duration::from_secs(2),
            }
        }
    }

    impl ShutdownWait for ShortWait {
        fn now(&self) -> std::time::Instant { std::time::Instant::now() }

        fn deadline(&self) -> std::time::Instant { self.deadline }

        fn wait(&self, timeout: Duration) {
            std::thread::sleep(timeout.min(Duration::from_millis(20)));
        }
    }

    /// Injected time: each slice advances a virtual clock instead of waiting it
    /// out, so a two-second owner bound costs only a few hundred real slices.
    struct AdvancingWait {
        start: std::time::Instant,
        elapsed: Mutex<Duration>,
    }

    impl AdvancingWait {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                start: std::time::Instant::now(),
                elapsed: Mutex::new(Duration::ZERO),
            })
        }
    }

    impl ShutdownWait for AdvancingWait {
        fn now(&self) -> std::time::Instant {
            self.start + *self.elapsed.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn deadline(&self) -> std::time::Instant { self.start + Duration::from_secs(5) }

        fn wait(&self, timeout: Duration) {
            *self.elapsed.lock().unwrap_or_else(PoisonError::into_inner) += timeout;
            // A real pause gives a cooperative owner a chance to finish.
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Polls a real-time condition from a test, failing after a real bound.
    fn eventually(what: &str, condition: impl Fn() -> bool) {
        let limit = std::time::Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(std::time::Instant::now() < limit, "{what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    struct StageConfig {
        enabled: bool,
    }

    impl ModuleConfig for StageConfig {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), stache_sdk::ConfigError> { Ok(()) }
    }

    struct StopGate {
        open: Mutex<bool>,
        signal: Condvar,
    }

    impl StopGate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                open: Mutex::new(false),
                signal: Condvar::new(),
            })
        }

        #[allow(clippy::significant_drop_tightening)] // The guard is the loop condition.
        fn wait(&self) {
            let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
            while !*open {
                open = self.signal.wait(open).unwrap_or_else(PoisonError::into_inner);
            }
        }

        fn open(&self) {
            *self.open.lock().unwrap_or_else(PoisonError::into_inner) = true;
            self.signal.notify_all();
        }
    }

    struct StageModule {
        id: ModuleId,
        stops: Arc<std::sync::atomic::AtomicUsize>,
        fail: bool,
        gate: Option<Arc<StopGate>>,
        start_gate: Option<Arc<StopGate>>,
        worker_gate: Option<Arc<StopGate>>,
    }

    impl StageModule {
        fn new(id: &'static str, stops: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
            Self {
                id: ModuleId::new(id),
                stops: Arc::clone(stops),
                fail: false,
                gate: None,
                start_gate: None,
                worker_gate: None,
            }
        }
    }

    impl Module for StageModule {
        type Config = StageConfig;

        fn descriptor(&self) -> stache_sdk::ModuleDescriptor {
            stache_sdk::ModuleDescriptor {
                id: self.id,
                provides: &[],
                requires: &[],
                permissions: &[],
                executables: &[],
                surfaces: &[],
            }
        }

        fn configure(
            &mut self,
            _: &StageConfig,
            _: &stache_sdk::ModuleContext,
        ) -> stache_sdk::ModuleResult<()> {
            Ok(())
        }

        fn start(&mut self, ctx: &stache_sdk::ModuleContext) -> stache_sdk::ModuleResult<()> {
            if let Some(gate) = &self.start_gate {
                gate.wait();
            }
            if let Some(gate) = &self.worker_gate {
                // This worker ignores cancellation, so only the bound ends its wait.
                let gate = Arc::clone(gate);
                ctx.spawn_worker("hung", move |_| gate.wait())?;
            }
            Ok(())
        }

        fn reconfigure(
            &mut self,
            _: &StageConfig,
            _: &stache_sdk::ModuleContext,
        ) -> stache_sdk::ModuleResult<()> {
            Ok(())
        }

        fn stop(&mut self, ctx: &stache_sdk::ModuleContext) -> stache_sdk::ModuleResult<()> {
            self.stops.fetch_add(1, Ordering::SeqCst);
            if let Some(gate) = &self.gate {
                gate.wait();
            }
            if self.fail {
                return Err(stache_sdk::ModuleError::new(
                    ctx.id(),
                    stache_sdk::Operation::Stop,
                    stache_sdk::ErrorCategory::Lifecycle,
                    "injected stop failure",
                ));
            }
            Ok(())
        }
    }

    fn stage_registry(modules: Vec<StageModule>) -> Arc<Registry> {
        let registry = Arc::new(Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(NoMainThread),
            Arc::new(AllowAll),
            Arc::new(NoWindows),
        ));
        for module in modules {
            registry
                .register(module, StageConfig { enabled: true })
                .expect("register the stage module");
        }
        registry.start_all().expect("start the stage modules");
        registry
    }

    fn finished() -> Arc<Finished> {
        Arc::new(Finished {
            done: Mutex::new(true),
            signal: Condvar::new(),
        })
    }

    fn unfinished() -> Arc<Finished> {
        Arc::new(Finished {
            done: Mutex::new(false),
            signal: Condvar::new(),
        })
    }

    fn stages_over(registry: Arc<Registry>) -> HostStages {
        stages_over_with(registry, Arc::new(MonitorCells::default()), finished())
    }

    fn stages_over_with(
        registry: Arc<Registry>,
        monitors: Arc<MonitorCells>,
        finished: Arc<Finished>,
    ) -> HostStages {
        HostStages {
            registry,
            monitors,
            setup_failed: Arc::new(AtomicBool::new(false)),
            finished,
            status: Arc::new(Mutex::new(HostStatus::default())),
            ui: Arc::new(QuietUi),
        }
    }

    #[test]
    fn registry_stage_reports_a_failure_and_still_stops_the_next_module() {
        let first_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let failing_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry =
            stage_registry(vec![StageModule::new("first-ok", &first_stops), StageModule {
                fail: true,
                ..StageModule::new("second-fails", &failing_stops)
            }]);
        let report = stages_over(registry).stop_registry(AdvancingWait::new());
        assert_eq!(report.failed, vec!["module:second-fails".to_owned()]);
        assert!(report.unacknowledged.is_empty());
        assert_eq!(first_stops.load(Ordering::SeqCst), 1);
        assert_eq!(failing_stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_hung_module_stop_is_abandoned_and_the_next_module_still_stops() {
        let gate = StopGate::new();
        let next_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry =
            stage_registry(vec![StageModule::new("next-ok", &next_stops), StageModule {
                gate: Some(Arc::clone(&gate)),
                ..StageModule::new("hung", &Arc::default())
            }]);
        let report = stages_over(Arc::clone(&registry)).stop_registry(AdvancingWait::new());
        assert_eq!(report.unacknowledged, vec!["module:hung".to_owned()]);
        assert!(report.failed.is_empty());
        assert_eq!(
            next_stops.load(Ordering::SeqCst),
            1,
            "the next module still stops"
        );
        // Abandonment keeps no shared lock: these all read through while the
        // carrier is still inside module code.
        assert_eq!(registry.snapshots().len(), 2);
        registry.invalidate();
        assert_eq!(registry.shutdown_targets().len(), 1, "only the hung one remains");
        gate.open();
    }

    #[test]
    fn a_blocked_start_is_abandoned_and_its_late_completion_stays_inert() {
        let gate = StopGate::new();
        let other_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let blocked_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry = Arc::new(Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(NoMainThread),
            Arc::new(AllowAll),
            Arc::new(NoWindows),
        ));
        registry
            .register(StageModule::new("other", &other_stops), StageConfig {
                enabled: true,
            })
            .expect("register the other module");
        registry
            .register(
                StageModule {
                    start_gate: Some(Arc::clone(&gate)),
                    ..StageModule::new("blocked", &blocked_stops)
                },
                StageConfig { enabled: true },
            )
            .expect("register the blocked module");
        let starter = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.start_all())
        };
        // Metadata is written before module code runs, so two targets mean the
        // start is at or inside the gate while holding its slot lock.
        eventually("the start must reach the blocked module", || {
            registry.shutdown_targets().len() == 2
        });

        let stages = stages_over(Arc::clone(&registry));
        stages.invalidate();
        let report = stages.stop_registry(AdvancingWait::new());
        assert_eq!(report.unacknowledged, vec!["module:blocked".to_owned()]);
        assert!(report.failed.is_empty());
        assert_eq!(other_stops.load(Ordering::SeqCst), 1, "the other module stops");

        gate.open();
        let late_start = starter.join().expect("start thread");
        assert!(late_start.is_err(), "the late start is refused");
        eventually("the late start must release what it created", || {
            blocked_stops.load(Ordering::SeqCst) == 1 && registry.shutdown_targets().is_empty()
        });
        assert!(
            registry
                .snapshots()
                .iter()
                .all(|snapshot| snapshot.status != ModuleStatus::Running),
            "a late completion must not publish running state"
        );
    }

    #[test]
    fn a_hung_module_worker_is_named_without_retaining_the_module_lock() {
        let gate = StopGate::new();
        let hung_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let next_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry =
            stage_registry(vec![StageModule::new("next-ok", &next_stops), StageModule {
                worker_gate: Some(Arc::clone(&gate)),
                ..StageModule::new("hung-worker", &hung_stops)
            }]);
        let report = stages_over(Arc::clone(&registry)).stop_registry(AdvancingWait::new());
        assert_eq!(report.unacknowledged, vec!["module:hung-worker".to_owned()]);
        assert!(report.failed.is_empty());
        assert_eq!(hung_stops.load(Ordering::SeqCst), 1, "Module::stop still ran");
        assert_eq!(
            next_stops.load(Ordering::SeqCst),
            1,
            "the next module still stops"
        );
        // The bounded join detaches the worker and the carrier returns, so the
        // slot records the stop even though the worker never finished.
        eventually("the carrier must release the slot", || {
            registry.shutdown_targets().is_empty()
        });
        gate.open();
    }

    #[test]
    fn modules_unreached_at_the_total_are_named_in_reverse_start_order() {
        let gate = StopGate::new();
        let first_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let second_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry = stage_registry(vec![
            StageModule {
                gate: Some(Arc::clone(&gate)),
                ..StageModule::new("first", &first_stops)
            },
            StageModule {
                gate: Some(Arc::clone(&gate)),
                ..StageModule::new("second", &second_stops)
            },
        ]);
        let wait = ExpiredWait { now: std::time::Instant::now() };
        let report = stages_over(registry).stop_registry(Arc::new(wait));
        assert_eq!(report.unacknowledged, vec![
            "module:second".to_owned(),
            "module:first".to_owned()
        ]);
        assert!(report.failed.is_empty());
        gate.open();
    }

    #[test]
    fn a_hung_ticker_is_abandoned_at_its_bound() {
        let (entered_tx, entered) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        let entered_tx = Mutex::new(entered_tx);
        let ticker = PermissionTicker::start(Duration::from_millis(1), move || {
            let _ = entered_tx.lock().expect("entered").send(());
            let _ = released.lock().expect("released").recv();
        })
        .expect("ticker");
        entered.recv_timeout(Duration::from_secs(5)).expect("the tick is running");
        let wait = ExpiredWait { now: std::time::Instant::now() };
        assert!(!ticker.stop_bounded(&wait), "a hung ticker is detached");
        drop(release);
    }

    #[test]
    fn a_hung_watcher_is_abandoned_at_its_bound() {
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        let (release, released) = mpsc::channel();
        let thread = thread::spawn(move || {
            let _ = released.recv();
        });
        let job = WatcherJob { stop, thread };
        let wait = ExpiredWait { now: std::time::Instant::now() };
        assert!(!job.stop_bounded(&wait), "a hung watcher is detached");
        assert!(cancelled.is_cancelled(), "cancellation is still requested");
        release.send(()).expect("release the watcher");
    }

    #[test]
    fn a_hung_startup_job_is_abandoned_at_its_bound() {
        struct HungRunner {
            release: Mutex<mpsc::Receiver<()>>,
        }

        impl CommandRunner for HungRunner {
            fn run(&self, _: &[String], _: &CancellationToken) -> Result<(), String> {
                let _ = self.release.lock().expect("release").recv();
                Ok(())
            }
        }

        let (release, released) = mpsc::channel();
        let job = StartupJob::spawn(
            vec!["ignored".to_owned()],
            Arc::new(HungRunner { release: Mutex::new(released) }),
            |_| {},
        )
        .expect("startup job");
        let wait = ExpiredWait { now: std::time::Instant::now() };
        assert!(!job.stop_bounded(&wait), "a hung startup job is detached");
        release.send(()).expect("release the startup job");
    }

    #[test]
    fn a_ticker_stops_inside_its_owner_bound() {
        let ticker = PermissionTicker::start(Duration::from_secs(60), || {}).expect("ticker");
        assert!(ticker.stop_bounded(&ShortWait::new()));
    }

    fn watcher_job() -> WatcherJob {
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        let thread = thread::spawn(move || {
            while !cancelled.is_cancelled() {
                let _ = cancelled.wait(Duration::from_millis(5));
            }
        });
        WatcherJob { stop, thread }
    }

    fn startup_job() -> StartupJob {
        StartupJob::spawn(Vec::new(), Arc::new(NoCommands), |_| {}).expect("startup job")
    }

    fn monitor_cells() -> Arc<MonitorCells> {
        let monitors = Arc::new(MonitorCells::default());
        *monitors.watcher.lock().unwrap_or_else(PoisonError::into_inner) = Some(watcher_job());
        *monitors.ticker.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(PermissionTicker::start(Duration::from_secs(60), || {}).expect("ticker"));
        *monitors.startup.lock().unwrap_or_else(PoisonError::into_inner) = Some(startup_job());
        monitors
    }

    #[test]
    fn monitor_owners_report_success_when_finished() {
        let registry = stage_registry(Vec::new());
        let report = stages_over_with(registry, monitor_cells(), finished())
            .reap_monitors(AdvancingWait::new());
        assert_eq!(report, StageReport::default());
    }

    #[test]
    fn an_idle_runner_acknowledges() {
        let registry = stage_registry(Vec::new());
        let report = stages_over_with(registry, Arc::new(MonitorCells::default()), finished())
            .reap_monitors(AdvancingWait::new());
        assert!(report.failed.is_empty());
        assert!(report.unacknowledged.is_empty(), "{report:?}");
    }

    #[test]
    fn a_runner_that_never_finishes_outside_any_module_is_named_host_worker() {
        let registry = stage_registry(Vec::new());
        let report = stages_over_with(registry, Arc::new(MonitorCells::default()), unfinished())
            .reap_monitors(AdvancingWait::new());
        assert_eq!(report.unacknowledged, vec![HOST_WORKER.to_owned()]);
        assert!(report.failed.is_empty());
    }

    #[test]
    fn hung_monitor_owners_are_named_in_stage_order() {
        struct BlockedCommands {
            release: Mutex<mpsc::Receiver<()>>,
        }

        impl CommandRunner for BlockedCommands {
            fn run(&self, _: &[String], _: &CancellationToken) -> Result<(), String> {
                let _ = self.release.lock().unwrap_or_else(PoisonError::into_inner).recv();
                Ok(())
            }
        }

        let (release_watcher, watcher_block) = mpsc::channel::<()>();
        let watcher_stop = CancellationToken::new();
        let blocked_watcher = thread::spawn(move || {
            let _ = watcher_block.recv();
        });
        let (entered_tx, entered) = mpsc::channel();
        let (release_ticker, ticker_block) = mpsc::channel::<()>();
        let ticker_block = Mutex::new(ticker_block);
        let entered_tx = Mutex::new(entered_tx);
        let ticker = PermissionTicker::start(Duration::from_millis(1), move || {
            let _ = entered_tx.lock().unwrap_or_else(PoisonError::into_inner).send(());
            let _ = ticker_block.lock().unwrap_or_else(PoisonError::into_inner).recv();
        })
        .expect("ticker");
        entered.recv_timeout(Duration::from_secs(5)).expect("the tick is running");
        let (release_startup, startup_block) = mpsc::channel::<()>();
        let startup = StartupJob::spawn(
            Vec::new(),
            Arc::new(BlockedCommands {
                release: Mutex::new(startup_block),
            }),
            |_| {},
        )
        .expect("startup job");

        let monitors = Arc::new(MonitorCells::default());
        *monitors.watcher.lock().unwrap_or_else(PoisonError::into_inner) = Some(WatcherJob {
            stop: watcher_stop,
            thread: blocked_watcher,
        });
        *monitors.ticker.lock().unwrap_or_else(PoisonError::into_inner) = Some(ticker);
        *monitors.startup.lock().unwrap_or_else(PoisonError::into_inner) = Some(startup);

        let report = stages_over_with(stage_registry(Vec::new()), monitors, finished())
            .reap_monitors(AdvancingWait::new());
        assert_eq!(report.unacknowledged, vec![
            "watcher".to_owned(),
            "permission-ticker".to_owned(),
            "startup-job".to_owned()
        ]);
        assert!(report.failed.is_empty());

        let _ = release_watcher.send(());
        let _ = release_ticker.send(());
        let _ = release_startup.send(());
    }

    #[test]
    fn a_failed_socket_removal_is_a_failed_socket_slot() {
        use std::os::unix::fs::PermissionsExt;

        struct NoCore;

        impl CoreHandler for NoCore {
            fn reload(&self) -> HandlerResult<CoreReload> { Ok(CoreReload { files: Vec::new() }) }

            fn status(&self) -> HandlerResult<CoreStatus> {
                Ok(CoreStatus {
                    directory: "/config".into(),
                    files: Vec::new(),
                })
            }
        }

        let root = tempfile::tempdir().expect("root");
        let app = AppPaths::resolve(&AppEnvironment {
            home: Some(root.path().to_path_buf()),
            temporary: Some(root.path().to_path_buf()),
        })
        .expect("app paths");
        let server =
            IpcServer::start(&app, Arc::new(Router::new(Arc::new(NoCore)))).expect("server");
        let directory = app.runtime_directory().to_path_buf();
        let monitors = Arc::new(MonitorCells::default());
        *monitors.ipc.lock().unwrap_or_else(PoisonError::into_inner) = Some(server);
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500))
            .expect("restrict the runtime directory");
        let stages = stages_over_with(stage_registry(Vec::new()), monitors, finished());
        let outcome = stages.cleanup(NativeSlot::Socket, AdvancingWait::new());
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .expect("restore the runtime directory");
        assert_eq!(outcome, SlotOutcome::Failed);
    }

    #[test]
    fn a_hung_and_a_failing_module_are_all_named_in_reverse_start_order() {
        let hung_a = StopGate::new();
        let hung_c = StopGate::new();
        let registry = stage_registry(vec![
            StageModule {
                gate: Some(Arc::clone(&hung_a)),
                ..StageModule::new("a", &Arc::default())
            },
            StageModule {
                fail: true,
                ..StageModule::new("b", &Arc::default())
            },
            StageModule {
                gate: Some(Arc::clone(&hung_c)),
                ..StageModule::new("c", &Arc::default())
            },
            StageModule {
                fail: true,
                ..StageModule::new("d", &Arc::default())
            },
        ]);
        let report = stages_over(registry).stop_registry(AdvancingWait::new());
        assert_eq!(report.failed, vec!["module:d".to_owned(), "module:b".to_owned()]);
        assert_eq!(
            report.unacknowledged,
            vec!["module:c".to_owned(), "module:a".to_owned()],
            "one hung stop must not mask another"
        );
        hung_a.open();
        hung_c.open();
    }
}
