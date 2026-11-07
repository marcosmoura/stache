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
use stache_keep_awake::{KeepAwakeConfig, KeepAwakeModule, KeepAwakeSnapshot};
use stache_keybindings::{KeybindingsConfig, KeybindingsRuntime};
use stache_macos::audio::{AudioBackend, CoreAudioBackend};
use stache_macos::{
    ACKNOWLEDGEMENT_BUDGET, ApplicationControl, AssertionOwner, LockObservationInstaller,
    LockProbe, SystemLockProbe,
};
use stache_menu_anywhere::{
    HEALTH_INTERVAL, MenuAnywhereConfig, MenuAnywhereParts, MenuAnywhereRuntime, SystemMenuHost,
};
use stache_notunes::{NoTunesConfig, NoTunesModule};
use stache_proxy_audio::{AudioIpc, ProxyAudioConfig, ProxyAudioModule};
use stache_sdk::input::{InputPriority, InputService};
use stache_sdk::ipc::{
    ConfigCore, CoreHandler, CoreReload, CoreStatus, HandlerResult, IpcHandler, IpcServer, Router,
    RouterError, ServerError, SocketShutdown,
};
use stache_sdk::shutdown::{
    CANCELLATION_QUANTUM, OWNER_WAIT_BUDGET, ShutdownWait, wait_for_finish,
};
use stache_sdk::{
    AcceptedWatch, CancellationToken, ChangeSet, ConfigCatalog, ConfigFileId, ConfigPaths,
    ConfigStore, ConfigStoreError, ConfigWatcher, EventBus, Initial, LogFilter, Logger,
    MainThreadExecutor, Module, ModuleConfig, ModuleHandle, ModuleId, ModuleSnapshot, ModuleStatus,
    PathEntry, Permission, PermissionProvider, PermissionRequirement, PermissionStatus, Registry,
    RegistryError, RegistryResult, ReloadOutcome, WatchError, WindowBackend, WindowError,
    WindowKey, WindowKind, WindowRequest, WindowSize, inspect_paths,
};

/// Host-assigned generic input priorities.
///
/// Hold-to-Quit > `MenuAnywhere` > Keybindings. The SDK knows no module
/// identity. Keybindings, Hold-to-Quit and `MenuAnywhere` subscribe.
pub mod input_priority {
    use super::InputPriority;

    /// Hold-to-Quit, the highest input priority. Its HID reservation yields an
    /// overlapping lower-priority Carbon hotkey, such as a Keybindings `Cmd+Q`.
    pub const HOLD_TO_QUIT: InputPriority = InputPriority { level: 300, tie_break: 0 };
    /// `MenuAnywhere`, between Hold-to-Quit and Keybindings.
    pub const MENU_ANYWHERE: InputPriority = InputPriority { level: 200, tie_break: 0 };
    /// Reserved for Keybindings, the lowest input priority.
    pub const KEYBINDINGS: InputPriority = InputPriority { level: 100, tie_break: 0 };
}

/// The host's meaning of the opaque `ExemptionFlags` frontmost bits.
///
/// The SDK assigns none; consumers such as Hold-to-Quit read them from the
/// event's context. Remaining bits are reserved.
pub mod input_context {
    use stache_sdk::input::ExemptionFlags;

    /// The frontmost application is Finder.
    pub const FINDER: ExemptionFlags = ExemptionFlags(1 << 0);
    /// The frontmost application is this process.
    pub const CURRENT_PROCESS: ExemptionFlags = ExemptionFlags(1 << 1);
}

use crate::bindings::{HostStatus, ModuleStatusView, config_file_view};
use crate::config_edit::{CommitOutcome, EnableEditError, plan_enabled};
use crate::keep_awake::KeepAwakeIpc;
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
    fn needed(&self) -> Vec<PermissionRequirement> { self.registry.permission_requirements() }

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
/// owns and exposes only published snapshots, held diagnostics and typed
/// accepted-configuration watches. It never exposes the mutable registry or
/// store, reads disk or applies configuration, so a handler built from it
/// observes state instead of changing it.
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

    /// A typed view over one registered module's accepted configuration.
    ///
    /// The watch reads the published `(revision, configuration)` pair without
    /// touching disk or waiting for module lifecycle work; a handler can use it
    /// to answer with the exact accepted configuration a sibling host applied.
    ///
    /// # Errors
    /// Returns a typed registry error for a stale handle or a handle whose
    /// configuration type differs from the registered module's.
    pub fn accepted_watch<C: ModuleConfig>(
        &self,
        handle: ModuleHandle<C>,
    ) -> RegistryResult<AcceptedWatch<C>> {
        self.registry.accepted_watch(handle)
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
    /// published registry snapshots, held configuration diagnostics and typed
    /// accepted-configuration watches.
    fn ipc_handlers(&self, _context: &FactoryContext) -> Vec<(&'static str, Arc<dyn IpcHandler>)> {
        Vec::new()
    }

    /// Adds this factory's separately published runtime details to the module's
    /// status view and returns whether the view is one of its own. The default
    /// adds nothing and returns `false`.
    ///
    /// Called with published data only: it must not lock module code, wait on
    /// native work or read the disk.
    fn project_status(&self, _view: &mut ModuleStatusView) -> bool { false }

    /// Stage 1: closes admission and wakes every host-service wait.
    ///
    /// Called once for every supplied factory, including one whose registration
    /// never completed, before the monitor cells close and the registry is
    /// invalidated. Must not join, perform filesystem or native work, or take a
    /// contended lifecycle or registry lock. The default is a no-op.
    fn begin_shutdown(&self) {}

    /// Stage 3: drains factory-owned work after the registry stopped modules.
    ///
    /// Called once in reverse registration order, on an abandonable carrier
    /// bounded by `wait`. `Ok(true)` means the factory drained within its
    /// bound, `Ok(false)` means it did not acknowledge, and `Err` is a failed
    /// drain. The default is an acknowledged no-op, so existing factories keep
    /// their behavior.
    ///
    /// # Errors
    /// Returns the factory's text diagnostic when its drain failed.
    fn finish_shutdown(&self, _wait: &dyn ShutdownWait) -> Result<bool, String> { Ok(true) }
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
    register_module_observed(registry, store, module, |_, _| Ok(()))
}

/// Registers one module and observes its typed registration before binding.
///
/// The observer runs after the module was accepted or rejected and before its
/// file is bound to the store, so a factory can install the typed accepted
/// watch while registration is still the only writer and before `start_all`.
/// It receives the typed handle; a failed observation stops registration with
/// its host error.
///
/// # Errors
/// Returns a typed store, registry or observer failure.
pub fn register_module_observed<M, F>(
    registry: &Registry,
    store: &ConfigStore,
    module: M,
    observe: F,
) -> Result<Arc<dyn ModuleControl>, HostError>
where
    M: Module,
    M::Config: Default + DeserializeOwned,
    F: FnOnce(&Registry, ModuleHandle<M::Config>) -> Result<(), HostError>,
{
    let id = module.descriptor().id;
    let handle = match store.initial::<M::Config>(id)? {
        Initial::Default(config) | Initial::Loaded(config) => registry.register(module, config)?,
        Initial::Invalid(diagnostic) => registry
            .register_rejected(module, stache_sdk::ConfigDiagnostic::new(diagnostic.message))?,
    };
    observe(registry, handle)?;
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

/// Registers the `MenuAnywhere` module over its host-lifetime runtime service.
///
/// Construction and registration are inert, including an enabled file: the
/// application control, the native menu host and the runtime create no
/// observer, thread, tap, read or menu until the enabled module starts, and a
/// read happens only on a trigger. Registration installs the typed
/// accepted-configuration watch before the registry starts modules. The factory
/// has no socket surface, CLI command or window: the one runtime detail, an
/// unhealthy running module, travels on the generic [`ModuleStatusView`].
///
/// The runtime owns its own application observation (it shares no control with
/// Hold-to-Quit or noTunes). A stop that could not prove its cleanup keeps the
/// exact native owners inside the runtime; [`ModuleFactory::finish_shutdown`]
/// retries them once the registry has stopped every module.
pub struct MenuAnywhereFactory {
    runtime: MenuAnywhereRuntime,
}

impl MenuAnywhereFactory {
    /// Wraps the one host-lifetime runtime service.
    #[must_use]
    pub const fn new(runtime: MenuAnywhereRuntime) -> Self { Self { runtime } }

    /// The production factory over the system Accessibility, `NSMenu`, geometry
    /// and application adapters. Nothing native is created.
    #[must_use]
    pub fn system(main_thread: Arc<dyn MainThreadExecutor>) -> Self {
        Self::new(MenuAnywhereRuntime::new(MenuAnywhereParts {
            applications: ApplicationControl::system(main_thread),
            menu: Arc::new(SystemMenuHost),
            priority: input_priority::MENU_ANYWHERE,
            health_interval: HEALTH_INTERVAL,
            acknowledgement: ACKNOWLEDGEMENT_BUDGET,
        }))
    }
}

impl ModuleFactory for MenuAnywhereFactory {
    fn id(&self) -> ModuleId { MenuAnywhereConfig::ID }

    fn register(
        &self,
        registry: &Registry,
        store: &ConfigStore,
    ) -> Result<Arc<dyn ModuleControl>, HostError> {
        let runtime = self.runtime.clone();
        register_module_observed(registry, store, runtime.module(), move |registry, handle| {
            runtime.install_accepted_watch(registry.accepted_watch(handle)?);
            Ok(())
        })
    }

    /// A running module whose input subscription or worker is no longer healthy
    /// is reported `unavailable` with the reason; a start that failed because
    /// the shared input service is missing is `unavailable` too, and an
    /// unacknowledged tap removal is shown as pending detail. The registry
    /// lifecycle itself is never rewritten by a read or action error.
    fn project_status(&self, view: &mut ModuleStatusView) -> bool {
        if view.id != MenuAnywhereConfig::ID.as_str() {
            return false;
        }
        if view.state == crate::bindings::ModuleState::Running
            && let Some(detail) = self.runtime.health_detail()
        {
            view.state = crate::bindings::ModuleState::Unavailable;
            view.detail = Some(detail);
        } else if view.state == crate::bindings::ModuleState::Failed
            && let Some(detail) = self.runtime.missing_service_detail()
        {
            // A missing or failed shared input service is a missing
            // prerequisite, not a module failure.
            view.state = crate::bindings::ModuleState::Unavailable;
            view.detail = Some(detail);
        } else if view.state != crate::bindings::ModuleState::Running
            && view.detail.is_none()
            && let Some(detail) = self.runtime.health_detail()
        {
            // An unacknowledged tap removal stays visible after the stop.
            view.detail = Some(detail);
        }
        true
    }

    fn begin_shutdown(&self) { self.runtime.begin_shutdown(); }

    fn finish_shutdown(&self, _wait: &dyn ShutdownWait) -> Result<bool, String> {
        Ok(self.runtime.finish_shutdown())
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

/// Registers the Keybindings module over the host-lifetime runtime service.
///
/// Construction is inert: the composition root builds the dispatcher, the
/// router, the Carbon control, the Caps owner and the shared input service, and
/// none of them subscribes, registers or spawns until the enabled module starts.
/// Registration installs the typed accepted-configuration watch before the
/// registry starts modules. The factory has no socket surface: the runtime
/// details travel on [`ModuleStatusView`].
pub struct KeybindingsFactory {
    runtime: KeybindingsRuntime,
}

impl KeybindingsFactory {
    /// Wraps the one host-lifetime runtime service.
    #[must_use]
    pub const fn new(runtime: KeybindingsRuntime) -> Self { Self { runtime } }
}

impl ModuleFactory for KeybindingsFactory {
    fn id(&self) -> ModuleId { KeybindingsConfig::ID }

    fn register(
        &self,
        registry: &Registry,
        store: &ConfigStore,
    ) -> Result<Arc<dyn ModuleControl>, HostError> {
        let runtime = self.runtime.clone();
        register_module_observed(registry, store, runtime.module(), move |registry, handle| {
            runtime.install_accepted_watch(registry.accepted_watch(handle)?);
            Ok(())
        })
    }

    fn project_status(&self, view: &mut ModuleStatusView) -> bool {
        if view.id != KeybindingsConfig::ID.as_str() {
            return false;
        }
        view.keybindings = Some(self.runtime.snapshot().read());
        true
    }

    fn begin_shutdown(&self) { self.runtime.begin_shutdown(); }

    fn finish_shutdown(&self, _wait: &dyn ShutdownWait) -> Result<bool, String> {
        Ok(self.runtime.finish_shutdown())
    }
}

/// Projects registry snapshots and overlays each factory's runtime details.
///
/// For a module a factory projected, a held configuration diagnostic (a parse,
/// type, I/O or deletion rejection that never reached the registry) takes
/// precedence over the snapshot diagnostic, as in the module status envelopes.
/// Other modules keep their snapshot diagnostic. It never replaces runtime details: the
/// diagnostic is the rejected-file channel only.
fn module_views(
    snapshots: &[ModuleSnapshot],
    factories: &[Arc<dyn ModuleFactory>],
    store: Option<&ConfigStore>,
) -> Vec<ModuleStatusView> {
    snapshots
        .iter()
        .map(|snapshot| {
            let mut view = ModuleStatusView::from_snapshot(snapshot);
            let mut projected = false;
            for factory in factories {
                projected |= factory.project_status(&mut view);
            }
            if projected
                && let Some(held) =
                    store.and_then(|store| store.diagnostic(ConfigFileId::module(snapshot.id)))
            {
                view.diagnostic = Some(held.to_string());
            }
            view
        })
        .collect()
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

    /// Creates the base production list: the proxyAudio pilot, the noTunes
    /// blocker, Keep Awake and `MenuAnywhere`.
    ///
    /// The app composition root adds the Wallpapers factory with
    /// [`crate::wallpapers::WallpapersFactory`] because that factory needs the
    /// identity paths, configuration directory and home. Registration only
    /// creates the modules and their read-only handlers; it
    /// starts no listener, installs no application or lock observer and acquires
    /// no power assertion. Starting depends on the accepted configuration. Every
    /// factory shares the host's main-thread executor; no module keeps a second
    /// clone of it. `owner` is the composition root's one Keep Awake assertion
    /// owner, so the same ledger can also be registered as the teardown
    /// `power-assertion` acknowledgement.
    #[must_use]
    pub fn production(main_thread: Arc<dyn MainThreadExecutor>, owner: AssertionOwner) -> Self {
        let backend: Arc<dyn AudioBackend> = Arc::new(CoreAudioBackend);
        Self::new()
            .with(ProxyAudioFactory::new(backend))
            .with(NoTunesFactory::new(ApplicationControl::system(Arc::clone(
                &main_thread,
            ))))
            .with(KeepAwakeFactory::new(
                owner,
                Arc::new(SystemLockProbe),
                LockObservationInstaller::system(Arc::clone(&main_thread)),
            ))
            .with(MenuAnywhereFactory::system(main_thread))
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

/// A bounded step the host runs once per desktop start, before modules start
/// and independent of module enabled state, configuration validity and the tray.
///
/// The host runs it on its own cancellable thread so a slow step never blocks
/// startup, never retries it and surfaces a returned condition once.
pub trait StartupRecovery: Send + Sync {
    /// A stable name for logs and the teardown record.
    fn name(&self) -> &'static str;

    /// Runs the step. Returns a condition to publish, or `None` when the step
    /// had nothing to report.
    fn run(&self, cancel: &CancellationToken) -> Option<String>;
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
    (handle.clone(), HostDriver {
        receiver,
        handle,
        input: None,
        recovery: None,
    })
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
    recovery: Mutex<Option<WatcherJob>>,
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
        if let Some(recovery) =
            self.recovery.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        {
            recovery.cancel();
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

/// The factory hooks the teardown stages retain.
///
/// The stage driver keeps its own strong factory handles: the runner drops
/// `HostSettings` when it finishes, but stage 1 must still reach every supplied
/// factory and stage 3 must drain factory-owned services after the registry
/// stopped modules. Invocation flags are atomics, so a repeated stage call does
/// not run a hook twice and no lock is held while a hook runs.
struct FactoryHooks {
    factories: Vec<Arc<dyn ModuleFactory>>,
    begun: AtomicBool,
    finished: AtomicBool,
    begin_failures: Mutex<Vec<String>>,
}

impl FactoryHooks {
    fn new(factories: Vec<Arc<dyn ModuleFactory>>) -> Self {
        Self {
            factories,
            begun: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            begin_failures: Mutex::new(Vec::new()),
        }
    }

    fn name(factory: &dyn ModuleFactory) -> String { format!("factory:{}", factory.id().as_str()) }

    /// Runs every begin hook once, containing a panic instead of losing the
    /// coordinator thread.
    fn begin(&self) {
        if self.begun.swap(true, Ordering::SeqCst) {
            return;
        }
        for factory in &self.factories {
            let name = Self::name(factory.as_ref());
            let begun =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| factory.begin_shutdown()));
            if begun.is_err() {
                self.begin_failures.lock().unwrap_or_else(PoisonError::into_inner).push(name);
            }
        }
    }

    /// Drains every factory once, in reverse registration order, on a bounded
    /// carrier that retains its factory when the wait is abandoned.
    fn finish(&self, wait: &Arc<dyn ShutdownWait>) -> StageReport {
        if self.finished.swap(true, Ordering::SeqCst) {
            return StageReport::default();
        }
        let mut report = StageReport {
            failed: std::mem::take(
                &mut *self.begin_failures.lock().unwrap_or_else(PoisonError::into_inner),
            ),
            unacknowledged: Vec::new(),
        };
        for factory in self.factories.iter().rev() {
            let name = Self::name(factory.as_ref());
            let worker = Arc::clone(factory);
            let worker_wait = Arc::clone(wait);
            let carrier = thread::Builder::new()
                .name(format!("stache-finish-{}", factory.id().as_str()))
                .spawn(move || worker.finish_shutdown(worker_wait.as_ref()));
            match carrier {
                Ok(handle) => {
                    if wait_for_finish(wait.as_ref(), &|| handle.is_finished()) {
                        match handle.join() {
                            Ok(Ok(true)) => {}
                            Ok(Ok(false)) => report.unacknowledged.push(name),
                            Ok(Err(_)) | Err(_) => report.failed.push(name),
                        }
                    } else {
                        // The detached carrier keeps its factory alive until the
                        // hook returns, so a stalled drain never loses its
                        // service state.
                        report.unacknowledged.push(name);
                    }
                }
                Err(_) => report.failed.push(name),
            }
        }
        report
    }
}

/// The production teardown stages over the shared host pieces.
pub struct HostStages {
    registry: Arc<Registry>,
    monitors: Arc<MonitorCells>,
    hooks: FactoryHooks,
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
        // Factory cancellation runs first: a host-service wait must wake before
        // the monitor cells close and before the registry refuses work.
        self.hooks.begin();
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
        if let Some(recovery) = Self::take(&self.monitors.recovery)
            && !recovery.stop_bounded(wait.as_ref())
        {
            report.unacknowledged.push("startup-recovery".to_owned());
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
        // Factory-owned services drain only after every module stop attempt,
        // so a module still inside `stop` never loses the service it may wait
        // on. Reverse registration order mirrors the registry's reverse stop.
        let hooks = self.hooks.finish(&wait);
        report.failed.extend(hooks.failed);
        report.unacknowledged.extend(hooks.unacknowledged);
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
        status.modules = module_views(&self.registry.snapshots(), &self.hooks.factories, None);
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status.clone();
        self.ui.status(&status);
    }
}

/// The unstarted host runner. The application creates the tray first, then
/// calls [`HostDriver::start`].
pub struct HostDriver {
    receiver: Receiver<HostCommand>,
    handle: HostHandle,
    input: Option<Arc<InputService>>,
    recovery: Option<Arc<dyn StartupRecovery>>,
}

impl HostDriver {
    /// Attaches the one shared input service the registry hands to modules.
    ///
    /// Without it (the default for host fixtures) every input subscription is
    /// `Unavailable` and no native owner is ever called.
    #[must_use]
    pub fn with_input_service(mut self, input: Arc<InputService>) -> Self {
        self.input = Some(input);
        self
    }
}

impl HostDriver {
    /// Attaches the one startup recovery step the host runs per desktop start.
    ///
    /// Without it (the default for host fixtures) nothing is recovered and no
    /// native tool is ever invoked.
    #[must_use]
    pub fn with_startup_recovery(mut self, recovery: Arc<dyn StartupRecovery>) -> Self {
        self.recovery = Some(recovery);
        self
    }
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
        let mut registry = Registry::new(
            Arc::clone(&services.logger),
            Arc::clone(&services.events),
            Arc::clone(&services.main_thread),
            Arc::clone(&services.permissions),
            Arc::clone(&services.windows),
        );
        if let Some(input) = self.input {
            registry = registry.with_input_service(input);
        }
        let registry = Arc::new(registry);
        let monitors = Arc::new(MonitorCells::default());
        let setup_failed = Arc::new(AtomicBool::new(false));
        let hooks = FactoryHooks::new(settings.factories.entries().to_vec());
        self.handle.teardown.install_stages(Arc::new(HostStages {
            registry: Arc::clone(&registry),
            monitors: Arc::clone(&monitors),
            hooks,
            setup_failed: Arc::clone(&setup_failed),
            finished: Arc::clone(&self.handle.finished),
            status: Arc::clone(&self.handle.status),
            ui: Arc::clone(&ui),
        }));
        let finished = self.handle.clone();
        let mut runner = Runner::new(
            settings,
            services,
            self.handle,
            ui,
            registry,
            monitors,
            setup_failed,
        );
        runner.recovery = self.recovery;
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
    recovery: Option<Arc<dyn StartupRecovery>>,
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
            recovery: None,
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

    /// Runs the startup recovery step once, on its own cancellable thread.
    ///
    /// A failure to start the thread is a condition, not a startup failure; the
    /// step is never retried.
    fn start_recovery(&mut self) {
        let Some(recovery) = self.recovery.take() else {
            return;
        };
        let cancel = CancellationToken::new();
        let worker_stop = cancel.clone();
        let sender = self.handle.sender.clone();
        let logger = Arc::clone(&self.services.logger);
        let name = recovery.name();
        let spawned = thread::Builder::new().name(format!("stache-{name}")).spawn(move || {
            let report = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                recovery.run(&worker_stop)
            }))
            .unwrap_or_else(|_| Some(format!("{name} panicked and was not retried")));
            if let Some(message) = report {
                logger.log(stache_sdk::LogRecord {
                    level: stache_sdk::LogLevel::Warn,
                    module: ModuleId::new("host"),
                    operation: stache_sdk::Operation::Start,
                    message: message.clone(),
                });
                let _ = sender.send(HostCommand::Condition(message));
            }
        });
        match spawned {
            Ok(thread) => {
                let job = WatcherJob { stop: cancel, thread };
                if let Some(job) = self.monitors.install(&self.monitors.recovery, job) {
                    // Stage 1 already closed the cells: reap it here instead.
                    job.stop_bounded(&LateReapWait::new());
                }
            }
            Err(error) => self.add_condition(format!("cannot start {name}: {error}")),
        }
    }

    fn start(&mut self) -> Result<(), HostError> {
        self.start_recovery();
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
        let snapshot = self.registry.snapshots().into_iter().find(|snapshot| snapshot.id == module);
        let Some(snapshot) = snapshot else {
            self.record_action_error(format!("module `{module}` has no published snapshot"));
            return;
        };
        if self.handle.shutdown_requested() {
            return;
        }
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
            modules: module_views(
                &self.registry.snapshots(),
                self.settings.factories.entries(),
                Some(&self.store),
            ),
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
    use std::sync::atomic::AtomicUsize;

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

    fn runner(root: &Path) -> Runner { runner_with_commands(root).0 }

    fn runner_with_commands(root: &Path) -> (Runner, Receiver<HostCommand>) {
        runner_over(
            root,
            ModuleFactories::new(),
            ConfigCatalog::new(),
            Arc::new(AllowAll),
            None,
        )
    }

    fn runner_over(
        root: &Path,
        factories: ModuleFactories,
        catalog: ConfigCatalog,
        permissions: Arc<dyn PermissionProvider>,
        input: Option<Arc<InputService>>,
    ) -> (Runner, Receiver<HostCommand>) {
        let app = AppPaths::resolve(&AppEnvironment {
            home: Some(root.to_path_buf()),
            temporary: Some(root.to_path_buf()),
        })
        .expect("app paths");
        let settings = HostSettings {
            app,
            config: ConfigPaths::new(root.join("config")),
            catalog,
            factories,
        };
        let services = HostServices {
            logger: Arc::new(DiscardLogger),
            log_filter: Arc::new(LogFilter::default()),
            events: Arc::new(EventBus::new()),
            main_thread: Arc::new(NoMainThread),
            permissions: Arc::clone(&permissions),
            windows: Arc::new(NoWindows),
            commands: Arc::new(NoCommands),
            poll: PermissionPoll {
                waiting: Duration::from_millis(1),
                backoff_start: Duration::from_millis(1),
                backoff_max: Duration::from_millis(1),
            },
        };
        let mut registry = Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(NoMainThread),
            permissions,
            Arc::new(NoWindows),
        );
        if let Some(input) = input {
            registry = registry.with_input_service(input);
        }
        let registry = Arc::new(registry);
        let (handle, driver) = new_host(quiet_teardown());
        let runner = Runner::new(
            settings,
            services,
            handle,
            Arc::new(QuietUi),
            registry,
            Arc::new(MonitorCells::default()),
            Arc::new(AtomicBool::new(false)),
        );
        (runner, driver.receiver)
    }

    /// A recovery step with a scripted result that records its runs and whether
    /// it was cancelled.
    struct ScriptedRecovery {
        result: Option<String>,
        runs: AtomicUsize,
        block_until_cancelled: bool,
        saw_cancel: AtomicBool,
        panics: bool,
    }

    impl ScriptedRecovery {
        fn new(result: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                result: result.map(str::to_owned),
                runs: AtomicUsize::new(0),
                block_until_cancelled: false,
                saw_cancel: AtomicBool::new(false),
                panics: false,
            })
        }
    }

    impl StartupRecovery for ScriptedRecovery {
        fn name(&self) -> &'static str { "scripted-recovery" }

        fn run(&self, cancel: &CancellationToken) -> Option<String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            assert!(!self.panics, "scripted failure");
            if self.block_until_cancelled {
                while !cancel.wait(Duration::from_millis(5)) {}
                self.saw_cancel.store(true, Ordering::SeqCst);
            }
            self.result.clone()
        }
    }

    fn next_condition(receiver: &Receiver<HostCommand>) -> Option<String> {
        match receiver.recv_timeout(Duration::from_millis(500)) {
            Ok(HostCommand::Condition(message)) => Some(message),
            _ => None,
        }
    }

    #[test]
    fn kb_04_07_the_recovery_step_runs_once_and_reports_its_condition_once() {
        let root = tempfile::tempdir().expect("root");
        let (mut runner, receiver) = runner_with_commands(root.path());
        let step = ScriptedRecovery::new(Some("Caps Lock recovery failed"));
        runner.recovery = Some(step.clone());
        runner.start_recovery();
        assert_eq!(
            next_condition(&receiver).as_deref(),
            Some("Caps Lock recovery failed")
        );
        assert!(next_condition(&receiver).is_none(), "reported once");
        // Taking the step makes a second call a no-op: never a retry loop.
        runner.start_recovery();
        assert!(next_condition(&receiver).is_none());
        assert_eq!(step.runs.load(Ordering::SeqCst), 1);
        let job = runner.monitors.recovery.lock().expect("cell").take().expect("job");
        assert!(job.stop_bounded(&LateReapWait::new()));
    }

    #[test]
    fn kb_04_07_a_quiet_step_and_a_panicking_step_do_not_disturb_startup() {
        let root = tempfile::tempdir().expect("root");
        let (mut runner, receiver) = runner_with_commands(root.path());
        runner.recovery = Some(ScriptedRecovery::new(None));
        runner.start_recovery();
        assert!(next_condition(&receiver).is_none(), "nothing to report");

        let (mut runner, receiver) = runner_with_commands(root.path());
        runner.recovery = Some(Arc::new(ScriptedRecovery {
            panics: true,
            ..Arc::into_inner(ScriptedRecovery::new(None)).expect("unique")
        }));
        runner.start_recovery();
        let message = next_condition(&receiver).expect("panic is reported");
        assert!(message.contains("panicked and was not retried"), "{message}");
    }

    #[test]
    fn kb_04_07_teardown_cancels_a_slow_recovery_and_reaps_its_thread() {
        let root = tempfile::tempdir().expect("root");
        let (mut runner, _receiver) = runner_with_commands(root.path());
        let step = Arc::new(ScriptedRecovery {
            block_until_cancelled: true,
            ..Arc::into_inner(ScriptedRecovery::new(None)).expect("unique")
        });
        runner.recovery = Some(step.clone());
        runner.start_recovery();
        assert!(runner.monitors.recovery.lock().expect("cell").is_some());
        runner.monitors.close();
        let job = runner.monitors.recovery.lock().expect("cell").take().expect("job");
        assert!(
            job.stop_bounded(&LateReapWait::new()),
            "the step woke on cancellation"
        );
        assert!(step.saw_cancel.load(Ordering::SeqCst));
    }

    #[test]
    fn kb_04_07_a_step_started_after_teardown_closed_the_cells_is_cancelled_and_reaped() {
        let root = tempfile::tempdir().expect("root");
        let (mut runner, _receiver) = runner_with_commands(root.path());
        let step = Arc::new(ScriptedRecovery {
            block_until_cancelled: true,
            ..Arc::into_inner(ScriptedRecovery::new(None)).expect("unique")
        });
        runner.recovery = Some(step.clone());
        runner.monitors.close();
        runner.start_recovery();
        assert!(runner.monitors.recovery.lock().expect("cell").is_none());
        assert!(
            step.saw_cancel.load(Ordering::SeqCst),
            "the late job was cancelled and joined"
        );
    }

    #[test]
    fn kb_04_07_the_production_recovery_restores_a_stale_journal_over_fakes() {
        use stache_keybindings::caps::CapsRecoveryStep;
        use stache_macos::caps_remap::testing::World;
        use stache_macos::caps_remap::{Mapping, OWNED};

        let root = tempfile::tempdir().expect("root");
        let key = Mapping { src: 100, dst: 200 };
        let world = World::new(vec![key]);
        world.remap().apply(&CancellationToken::new()).expect("apply");
        world.restart();
        assert_eq!(world.mappings(), vec![key, OWNED]);
        let (mut runner, receiver) = runner_with_commands(root.path());
        runner.recovery = Some(Arc::new(crate::caps_recovery::KeybindingsCapsRecovery::new(
            CapsRecoveryStep::new(Arc::new(world.remap())),
        )));
        runner.start_recovery();
        let deadline = Instant::now() + Duration::from_secs(5);
        while world.journal().is_some() {
            assert!(Instant::now() < deadline, "recovery never retired the journal");
            thread::sleep(Duration::from_millis(5));
        }
        let job = runner.monitors.recovery.lock().expect("cell").take().expect("job");
        assert!(job.stop_bounded(&LateReapWait::new()));
        assert_eq!(world.mappings(), vec![key]);
        assert_eq!(world.journal(), None);
        assert_eq!(world.sets(), 2);
        assert!(next_condition(&receiver).is_none(), "a clean restore is quiet");
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
        let factories =
            ModuleFactories::production(Arc::new(NoMainThread), AssertionOwner::system());
        let ids: Vec<ModuleId> = factories.entries().iter().map(|factory| factory.id()).collect();
        assert_eq!(
            ids,
            [
                stache_proxy_audio::ProxyAudioConfig::ID,
                stache_notunes::NoTunesConfig::ID,
                stache_keep_awake::KeepAwakeConfig::ID,
                stache_menu_anywhere::MenuAnywhereConfig::ID,
            ],
            "production registers the audio pilot, the blocker, Keep Awake and MenuAnywhere"
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
        events: Option<Arc<Mutex<Vec<String>>>>,
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
                events: None,
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
            if let Some(events) = &self.events {
                let events = Arc::clone(events);
                ctx.invalidation().on_invalidate(Arc::new(move || {
                    events.lock().unwrap_or_else(PoisonError::into_inner).push("invalidate".into());
                }));
            }
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
            if let Some(events) = &self.events {
                events
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(format!("stop:{}", self.id.as_str()));
            }
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

    #[derive(Clone, Debug, PartialEq)]
    struct InputConfig {
        caps: bool,
    }

    impl ModuleConfig for InputConfig {
        fn enabled(&self) -> bool { true }

        fn validate(&self) -> Result<(), stache_sdk::ConfigError> { Ok(()) }
    }

    /// A shared-input consumer that needs Accessibility only for `caps`.
    struct InputModule;

    impl Module for InputModule {
        type Config = InputConfig;

        fn descriptor(&self) -> stache_sdk::ModuleDescriptor {
            stache_sdk::ModuleDescriptor {
                id: ModuleId::new("input-consumer"),
                provides: &[],
                requires: &[],
                permissions: &[],
                executables: &[],
                surfaces: &[],
            }
        }

        fn required_permissions(&self, config: &InputConfig) -> Vec<PermissionRequirement> {
            if config.caps {
                vec![PermissionRequirement::shared_input(
                    Permission::Accessibility,
                )]
            } else {
                Vec::new()
            }
        }

        fn configure(
            &mut self,
            _: &InputConfig,
            _: &stache_sdk::ModuleContext,
        ) -> stache_sdk::ModuleResult<()> {
            Ok(())
        }

        fn start(&mut self, _: &stache_sdk::ModuleContext) -> stache_sdk::ModuleResult<()> {
            Ok(())
        }

        fn reconfigure(
            &mut self,
            _: &InputConfig,
            _: &stache_sdk::ModuleContext,
        ) -> stache_sdk::ModuleResult<()> {
            Ok(())
        }

        fn stop(&mut self, _: &stache_sdk::ModuleContext) -> stache_sdk::ModuleResult<()> { Ok(()) }
    }

    /// Counts `request` calls so a default test can prove it never prompts.
    struct CountingTrust {
        trusted: std::sync::atomic::AtomicBool,
        requests: AtomicUsize,
    }

    impl PermissionProvider for CountingTrust {
        fn status(&self, _: Permission) -> PermissionStatus {
            if self.trusted.load(Ordering::SeqCst) {
                PermissionStatus::Granted
            } else {
                PermissionStatus::Denied
            }
        }

        fn request(
            &self,
            permission: Permission,
        ) -> Result<PermissionStatus, stache_sdk::PermissionError> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            Ok(self.status(permission))
        }
    }

    #[test]
    fn kb_05_04_host_policy_never_prompts_for_shared_input_except_on_explicit_request() {
        let trust = Arc::new(CountingTrust {
            trusted: std::sync::atomic::AtomicBool::new(false),
            requests: AtomicUsize::new(0),
        });
        let provider: Arc<dyn PermissionProvider> = trust.clone();
        let registry = Arc::new(Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(NoMainThread),
            Arc::clone(&provider),
            Arc::new(NoWindows),
        ));
        let handle = registry.register(InputModule, InputConfig { caps: false }).expect("register");
        let host = RegistryPermissions {
            registry: Arc::clone(&registry),
            logger: Arc::new(DiscardLogger),
        };
        let mut manager = PermissionManager::new(provider);
        let requests = || trust.requests.load(Ordering::SeqCst);

        // Ordinary configuration: permission-free, running, no prompt.
        registry.start_all().expect("start");
        manager.refresh(&host, RefreshTrigger::Startup);
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        assert_eq!(requests(), 0);

        // Ordinary -> Caps without trust: accepted, Waiting, still no prompt.
        registry.reload(handle, InputConfig { caps: true }).expect("reload");
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::WaitingForPermission(Permission::Accessibility)
        );
        for trigger in [
            RefreshTrigger::Startup,
            RefreshTrigger::Tick,
            RefreshTrigger::Activation,
            RefreshTrigger::Tick,
        ] {
            let report = manager.refresh(&host, trigger);
            assert!(report.waiting);
            assert_eq!(report.prompted, None);
        }
        registry.reload(handle, InputConfig { caps: true }).expect("same reload");
        registry.set_enabled(handle, true).expect("enable");
        manager.refresh(&host, RefreshTrigger::Tick);
        assert_eq!(requests(), 0);

        // The explicit tray path may prompt.
        let report = manager.refresh(&host, RefreshTrigger::Explicit);
        assert_eq!(report.prompted, Some(Permission::Accessibility));
        assert_eq!(requests(), 1);

        // Trust arrives: the next tick starts the module; revocation stops it.
        trust.trusted.store(true, Ordering::SeqCst);
        manager.refresh(&host, RefreshTrigger::Tick);
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        trust.trusted.store(false, Ordering::SeqCst);
        manager.refresh(&host, RefreshTrigger::Tick);
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::WaitingForPermission(Permission::Accessibility)
        );

        // Caps -> ordinary leaves Waiting and runs without trust.
        registry.reload(handle, InputConfig { caps: false }).expect("reload");
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        manager.refresh(&host, RefreshTrigger::Tick);
        assert_eq!(requests(), 1);
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
            hooks: FactoryHooks::new(Vec::new()),
            setup_failed: Arc::new(AtomicBool::new(false)),
            finished,
            status: Arc::new(Mutex::new(HostStatus::default())),
            ui: Arc::new(QuietUi),
        }
    }

    fn stages_over_hooks(
        registry: Arc<Registry>,
        factories: Vec<Arc<dyn ModuleFactory>>,
    ) -> HostStages {
        let mut stages = stages_over(registry);
        stages.hooks = FactoryHooks::new(factories);
        stages
    }

    fn event_log(events: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        events.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    struct HookFactory {
        id: &'static str,
        events: Arc<Mutex<Vec<String>>>,
        finish_gate: Option<Arc<StopGate>>,
        finish_entered: Option<Arc<Mutex<Option<mpsc::Sender<()>>>>>,
    }

    impl HookFactory {
        fn new(id: &'static str, events: &Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                id,
                events: Arc::clone(events),
                finish_gate: None,
                finish_entered: None,
            }
        }

        fn push(&self, event: &str) {
            self.events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event.to_owned());
        }
    }

    impl ModuleFactory for HookFactory {
        fn id(&self) -> ModuleId { ModuleId::new(self.id) }

        fn register(
            &self,
            _: &Registry,
            _: &ConfigStore,
        ) -> Result<Arc<dyn ModuleControl>, HostError> {
            Err(HostError::Thread(
                "the hook factory registers no module".to_owned(),
            ))
        }

        fn begin_shutdown(&self) { self.push("begin"); }

        fn finish_shutdown(&self, _: &dyn ShutdownWait) -> Result<bool, String> {
            if let Some(entered) = &self.finish_entered {
                let sender = entered.lock().unwrap_or_else(PoisonError::into_inner).take();
                if let Some(sender) = sender {
                    let _ = sender.send(());
                }
            }
            if let Some(gate) = &self.finish_gate {
                gate.wait();
            }
            self.push("finish");
            Ok(true)
        }
    }

    struct IdleFactory;

    impl ModuleFactory for IdleFactory {
        fn id(&self) -> ModuleId { ModuleId::new("idle") }

        fn register(
            &self,
            _: &Registry,
            _: &ConfigStore,
        ) -> Result<Arc<dyn ModuleControl>, HostError> {
            Err(HostError::Thread(
                "the idle factory registers no module".to_owned(),
            ))
        }
    }

    struct PanickingFactory;

    impl ModuleFactory for PanickingFactory {
        fn id(&self) -> ModuleId { ModuleId::new("panic") }

        fn register(
            &self,
            _: &Registry,
            _: &ConfigStore,
        ) -> Result<Arc<dyn ModuleControl>, HostError> {
            Err(HostError::Thread(
                "the panicking factory registers no module".to_owned(),
            ))
        }

        fn begin_shutdown(&self) {
            panic!("injected begin-shutdown panic");
        }
    }

    #[test]
    fn factory_hooks_bracket_invalidation_and_module_stop() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry = stage_registry(vec![StageModule {
            events: Some(Arc::clone(&events)),
            ..StageModule::new("one", &stops)
        }]);
        let factory = HookFactory::new("one", &events);
        let stages = stages_over_hooks(registry, vec![Arc::new(factory)]);

        stages.invalidate();
        let report = stages.stop_registry(AdvancingWait::new());

        assert!(report.failed.is_empty(), "{report:?}");
        assert!(report.unacknowledged.is_empty(), "{report:?}");
        assert_eq!(event_log(&events), ["begin", "invalidate", "stop:one", "finish"]);
    }

    #[test]
    fn a_repeated_stage_call_does_not_repeat_a_factory_hook() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let factory = HookFactory::new("one", &events);
        let stages = stages_over_hooks(stage_registry(Vec::new()), vec![Arc::new(factory)]);

        stages.invalidate();
        stages.invalidate();
        stages.stop_registry(AdvancingWait::new());
        stages.stop_registry(AdvancingWait::new());

        let events = event_log(&events);
        assert_eq!(events.iter().filter(|event| *event == "begin").count(), 1);
        assert_eq!(events.iter().filter(|event| *event == "finish").count(), 1);
    }

    #[test]
    fn a_stalled_factory_drain_is_unacknowledged_and_keeps_its_owner() {
        let gate = StopGate::new();
        let entered = Arc::new(Mutex::new(None));
        let (sender, receiver) = mpsc::channel();
        *entered.lock().unwrap_or_else(PoisonError::into_inner) = Some(sender);
        let factory = Arc::new(HookFactory {
            finish_gate: Some(Arc::clone(&gate)),
            finish_entered: Some(Arc::clone(&entered)),
            ..HookFactory::new("hung", &Arc::new(Mutex::new(Vec::new())))
        });
        let retained = Arc::downgrade(&factory);
        let shared: Arc<dyn ModuleFactory> = factory.clone();
        let stages = stages_over_hooks(stage_registry(Vec::new()), vec![shared]);

        let report = stages.stop_registry(Arc::new(ExpiredWait { now: std::time::Instant::now() }));
        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the drain carrier entered");

        assert_eq!(report.unacknowledged, vec!["factory:hung".to_owned()]);
        assert!(report.failed.is_empty(), "{report:?}");

        drop(stages);
        drop(factory);
        assert!(
            retained.upgrade().is_some(),
            "a stalled drain must retain its factory and service"
        );
        gate.open();
        eventually("the stalled drain must release its factory", || {
            retained.upgrade().is_none()
        });
    }

    #[test]
    fn a_stalled_module_stop_does_not_destroy_the_factory_service() {
        let gate = StopGate::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry = stage_registry(vec![StageModule {
            gate: Some(Arc::clone(&gate)),
            events: Some(Arc::clone(&events)),
            ..StageModule::new("slow", &stops)
        }]);
        let factory = Arc::new(HookFactory::new("hook", &events));
        let retained = Arc::downgrade(&factory);
        let stages = stages_over_hooks(registry, vec![factory]);
        stages.invalidate();

        let stopping = thread::spawn(move || stages.stop_registry(AdvancingWait::new()));
        eventually("the module stop must be in flight", || {
            event_log(&events).iter().any(|event| event == "stop:slow")
        });
        assert!(
            retained.upgrade().is_some(),
            "the factory must outlive a stalled module stop"
        );
        assert!(
            !event_log(&events).iter().any(|event| event == "finish"),
            "the factory must not drain before the module stop returns"
        );

        gate.open();
        let report = stopping.join().expect("the stopping thread");
        assert!(report.failed.is_empty(), "{report:?}");
        assert!(report.unacknowledged.is_empty(), "{report:?}");
        let events = event_log(&events);
        let stop = events.iter().position(|event| event == "stop:slow").expect("stop");
        let finish = events.iter().position(|event| event == "finish").expect("finish");
        assert!(stop < finish, "{events:?}");
    }

    #[test]
    fn a_begin_hook_panic_is_named_without_losing_the_stage() {
        let stages =
            stages_over_hooks(stage_registry(Vec::new()), vec![Arc::new(PanickingFactory)]);
        stages.invalidate();

        let report = stages.stop_registry(AdvancingWait::new());

        assert_eq!(report.failed, vec!["factory:panic".to_owned()]);
        assert!(report.unacknowledged.is_empty(), "{report:?}");
    }

    #[test]
    fn an_idle_factory_keeps_the_default_acknowledged_no_op_hooks() {
        let wait = ExpiredWait { now: std::time::Instant::now() };
        IdleFactory.begin_shutdown();
        assert_eq!(IdleFactory.finish_shutdown(&wait), Ok(true));
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
    // -----------------------------------------------------------------------
    // Keybindings over the actual runner, registry and configuration store
    // -----------------------------------------------------------------------

    use stache_keybindings::test_support::{Rig as KbRig, wait_until as kb_wait};

    const KB_Q: u16 = 0x0c;
    const KB_CMD: u32 = 0x0100;

    struct KbHost {
        runner: Runner,
        rig: KbRig,
        _receiver: Receiver<HostCommand>,
        config: stache_sdk::ConfigPaths,
        _root: tempfile::TempDir,
    }

    impl KbHost {
        fn new(rig: KbRig, file: Option<&str>) -> Self {
            let root = tempfile::tempdir().expect("root");
            let config = ConfigPaths::new(root.path().join("config"));
            std::fs::create_dir_all(root.path().join("config")).expect("config directory");
            let factories =
                ModuleFactories::new().with(KeybindingsFactory::new(rig.runtime.clone()));
            let (runner, receiver) = runner_over(
                root.path(),
                factories,
                crate::catalog::catalog().expect("catalog"),
                Arc::new(AllowAll),
                Some(Arc::clone(&rig.input)),
            );
            let host = Self {
                runner,
                rig,
                _receiver: receiver,
                config,
                _root: root,
            };
            if let Some(file) = file {
                host.write(file);
            }
            host
        }

        fn path(&self) -> std::path::PathBuf { self.config.module_file(KeybindingsConfig::ID) }

        fn write(&self, text: &str) { std::fs::write(self.path(), text).expect("write"); }

        /// The registration and start prefix of `Runner::start`, without the
        /// socket, watcher, ticker or startup commands.
        fn register(&mut self) {
            self.runner.start_recovery();
            let factories: Vec<Arc<dyn ModuleFactory>> =
                self.runner.settings.factories.entries().to_vec();
            for factory in factories {
                let control =
                    factory.register(&self.runner.registry, &self.runner.store).expect("register");
                self.runner.controls.insert(factory.id().as_str().to_owned(), control);
            }
        }

        fn start(&mut self) {
            self.register();
            let _ = self.runner.registry.start_all();
            self.runner.ready = true;
            self.runner.publish();
        }

        fn reload(&mut self) {
            let mut changes = ChangeSet::default();
            changes.files.insert(ConfigFileId::module(KeybindingsConfig::ID));
            self.runner.apply_config(&changes);
            self.runner.publish();
        }

        fn view(&self) -> ModuleStatusView {
            self.runner
                .handle
                .status()
                .modules
                .into_iter()
                .find(|module| module.id == "keybindings")
                .expect("keybindings module")
        }
    }

    const KB_ON: &str = r#"{ "enabled": true, "bindings": { "Cmd+Q": "/bin/sh hold-q" } }"#;

    #[test]
    fn kb_05_01_the_keybindings_factory_is_inert_until_start_and_projects_runtime_details() {
        let mut host = KbHost::new(KbRig::new(), Some(KB_ON));
        host.register();
        // Registration and construction touch nothing.
        assert!(host.rig.carbon.calls().is_empty());
        assert_eq!(host.rig.backend.submit_calls(), 0);
        assert_eq!(host.rig.processes.count(), 0);
        host.runner.registry.start_all().expect("start");
        host.runner.publish();
        let view = host.view();
        assert_eq!(view.state, crate::bindings::ModuleState::Running);
        let details = view.keybindings.as_ref().expect("runtime details");
        assert_eq!(details.state, stache_keybindings::RuntimeState::Active);
        assert_eq!(details.revision, view.revision);
        assert_eq!(host.rig.carbon.registered_chords(), [(KB_Q, KB_CMD)]);
        // The wire shape: camelCase details on this module only.
        let json = serde_json::to_value(host.runner.handle.status()).expect("encode");
        assert_eq!(json["modules"][0]["keybindings"]["state"], "active");
        assert_eq!(
            json["modules"][0]["keybindings"]["revision"],
            serde_json::json!(view.revision)
        );
        let _ = host.runner.registry.shutdown();
    }

    #[test]
    fn kb_05_05_held_file_diagnostics_and_runtime_details_coexist_and_repair_independently() {
        let mut host = KbHost::new(KbRig::new(), Some(KB_ON));
        host.start();
        let revision = host.view().revision;
        // An invalid file is held; the running details are untouched.
        host.write("{ \"enabled\": ");
        host.reload();
        let view = host.view();
        assert!(view.diagnostic.is_some(), "the rejected file is held");
        assert_eq!(view.state, crate::bindings::ModuleState::Running);
        assert_eq!(view.revision, revision);
        assert_eq!(
            view.keybindings.as_ref().expect("details").state,
            stache_keybindings::RuntimeState::Active
        );
        assert!(
            !view
                .keybindings
                .as_ref()
                .expect("details")
                .diagnostics
                .iter()
                .any(|d| d.contains("expected")),
            "the file diagnostic never enters the runtime channel"
        );
        // A repaired file clears the diagnostic and advances the runtime.
        host.write(r#"{ "enabled": true, "bindings": { "Cmd+W": "/bin/sh quick-w" } }"#);
        host.reload();
        let view = host.view();
        assert!(view.diagnostic.is_none());
        assert_ne!(view.revision, revision);
        let adopted = view.revision;
        kb_wait("the repaired file is adopted", || {
            host.runner.publish();
            host.view().keybindings.is_some_and(|details| details.revision == adopted)
        });
        let _ = host.runner.registry.shutdown();
    }

    #[test]
    fn kb_05_07_a_rejected_or_unchanged_file_reload_cancels_no_accepted_work() {
        let mut host = KbHost::new(KbRig::new(), Some(KB_ON));
        host.start();
        host.rig.press_chord(KB_Q, KB_CMD);
        kb_wait("the held command", || host.rig.processes.count() == 1);
        let generation = host.rig.dispatcher.generation();
        // Same bytes: the registry sees an unchanged configuration.
        host.write(KB_ON);
        host.reload();
        // Rejected bytes: nothing is published.
        host.write("not json");
        host.reload();
        assert_eq!(host.rig.dispatcher.generation(), generation);
        assert!(!host.rig.processes.child("hold-q").killed());
        assert_eq!(host.rig.dispatcher.snapshot().active, 1);
        assert!(host.view().diagnostic.is_some());
        host.rig.processes.finish_all();
        let _ = host.runner.registry.shutdown();
    }

    #[test]
    fn kb_05_07_host_caps_recovery_runs_with_the_module_disabled_and_an_invalid_file() {
        use stache_keybindings::caps::CapsRecoveryStep;
        use stache_macos::caps_remap::{Mapping, OWNED};

        let key = Mapping { src: 100, dst: 200 };
        let rig = KbRig::with_mappings(vec![key]);
        // A crash left Next's own mapping and journal behind.
        rig.remap.apply(&CancellationToken::new()).expect("apply");
        rig.world.restart();
        assert_eq!(rig.world.mappings(), vec![key, OWNED]);
        let sets_before = rig.world.sets();
        // The module file is invalid and the module is therefore not enabled.
        let mut host = KbHost::new(rig, Some("{ not valid"));
        host.runner.recovery = Some(Arc::new(crate::caps_recovery::KeybindingsCapsRecovery::new(
            CapsRecoveryStep::new(Arc::clone(&host.rig.remap)),
        )));
        host.register();
        let deadline = Instant::now() + Duration::from_secs(5);
        while host.rig.world.journal().is_some() {
            assert!(Instant::now() < deadline, "recovery never retired the journal");
            thread::sleep(Duration::from_millis(5));
        }
        let job = host.runner.monitors.recovery.lock().expect("cell").take().expect("job");
        assert!(job.stop_bounded(&LateReapWait::new()));
        assert_eq!(host.rig.world.mappings(), vec![key]);
        assert_eq!(
            host.rig.world.sets() - sets_before,
            1,
            "one restore write, nothing else"
        );
        // The module itself stayed inert: no tap, no hotkey, no process.
        host.runner.registry.start_all().expect("start");
        host.runner.publish();
        assert!(host.view().diagnostic.is_some());
        assert!(host.rig.carbon.calls().is_empty());
        assert_eq!(host.rig.backend.submit_calls(), 0);
    }

    #[test]
    fn kb_05_06_host_shutdown_closes_admission_then_stops_the_module_then_drains() {
        let mut host = KbHost::new(KbRig::new(), Some(KB_ON));
        host.start();
        host.rig.press_chord(KB_Q, KB_CMD);
        kb_wait("the held command", || host.rig.processes.count() == 1);
        let factories: Vec<Arc<dyn ModuleFactory>> =
            host.runner.settings.factories.entries().to_vec();
        let stages = stages_over_hooks(Arc::clone(&host.runner.registry), factories);
        stages.invalidate();
        // Stage 1 closed admission before the module stopped.
        host.rig.press_chord(KB_Q, KB_CMD);
        assert_eq!(host.rig.processes.count(), 1, "admission is closed");
        let report = stages.stop_registry(AdvancingWait::new());
        assert!(report.failed.is_empty(), "{report:?}");
        assert!(report.unacknowledged.is_empty(), "{report:?}");
        assert!(
            host.rig.processes.child("hold-q").killed(),
            "the module stop cancelled it"
        );
        assert!(host.rig.carbon.registered_chords().is_empty());
        assert!(
            host.rig
                .dispatcher
                .submit(&[stache_keybindings::test_support::cmd("late")])
                .is_err(),
            "the factory drain closed the dispatcher"
        );
    }

    #[test]
    fn kb_05_06_two_independent_fake_hosts_share_nothing() {
        let mut first = KbHost::new(KbRig::new(), Some(KB_ON));
        let mut second = KbHost::new(KbRig::new(), Some(KB_ON));
        first.start();
        second.start();
        first.rig.press_chord(KB_Q, KB_CMD);
        kb_wait("first", || first.rig.processes.count() == 1);
        assert_eq!(second.rig.processes.count(), 0);
        let _ = second.runner.registry.shutdown();
        first.rig.press_chord(KB_Q, KB_CMD);
        kb_wait("first again", || first.rig.processes.count() == 2);
        first.rig.processes.finish_all();
        let _ = first.runner.registry.shutdown();
    }
}
