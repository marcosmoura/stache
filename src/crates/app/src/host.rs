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
//! starts modules and opens the control socket. `exec_on_startup` runs once per
//! launch, after the Accessibility gate first opens (see [`crate::access`]).
//! A shutdown requested before or during setup is processed without starting
//! new modules.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak};
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
    RegistryError, RegistryResult, ReloadOutcome, StateObserver, StateSchemas, WatchError,
    WindowBackend, WindowError, WindowId, inspect_paths,
};
use stache_tiling::ipc::{
    ActionLane, IpcTiming, LifecycleControl, LifecycleFailure, LifecycleThread, TilingFocus,
    TilingIpc,
};
use stache_tiling::{
    AppDirectory, DisplayProjection, TilingConfig, TilingParts, TilingRuntime, TilingRuntimeState,
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

use crate::access::{
    AccessCell, AccessGate, AccessGateState, AccessTransition, GatedHandler, SettingsOpener,
    enclosing_app,
};
use crate::bindings::{AccessState, AccessView, HostStatus, ModuleStatusView, config_file_view};
use crate::config_documents::{ConfigEditOutcome, ConfigEditRequest, run_edit};
use crate::config_edit::{CommitOutcome, EnableEditError, plan_enabled};
use crate::host_job::{HostJob, JobWait};
use crate::keep_awake::KeepAwakeIpc;
use crate::permissions::{
    PermissionHost, PermissionManager, PermissionPoll, PermissionTicker, PollSchedule,
    RefreshReport, RefreshTrigger, TickGate,
};
use crate::popover_actions::ModuleEnableOutcome;
use crate::session_action::{
    self, SESSION_WAIT, SessionAction, SessionActionOutcome, SessionControl, SessionJob,
    SessionUnavailable,
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
    /// The socket gate cell of a gated host. A lost Accessibility status closes
    /// it before the registry stops modules, so a slow stop never leaves the
    /// socket open to a module that is losing trust.
    access: Option<AccessCell>,
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
        if permission == Permission::Accessibility
            && status != PermissionStatus::Granted
            && let Some(cell) = &self.access
        {
            cell.close();
        }
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
    access: AccessCell,
}

impl CoreHandler for HostCore {
    fn reload(&self) -> HandlerResult<CoreReload> {
        let result = self.core.reload().map(|mut report| {
            report.access = Some(self.access.report());
            report
        });
        let _ = self.sender.send(HostCommand::Refresh);
        result
    }

    fn status(&self) -> HandlerResult<CoreStatus> {
        self.core.status().map(|mut status| {
            status.access = Some(self.access.report());
            status
        })
    }
}

/// Builds the socket router: the core service plus every factory-contributed
/// handler, each wrapped so it answers `unavailable` while the gate is closed.
fn build_router(
    core: Arc<dyn CoreHandler>,
    access: &AccessCell,
    factories: &ModuleFactories,
    context: &FactoryContext,
) -> Result<Router, RouterError> {
    let mut router = Router::new(core);
    for factory in factories.entries() {
        for (name, handler) in factory.ipc_handlers(context) {
            router.register(name, Arc::new(GatedHandler::new(access.clone(), handler)))?;
        }
    }
    Ok(router)
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

    /// A read-only observer of one provider's published state.
    ///
    /// An unknown provider, or one with no registered schema, reads absent. The
    /// observer takes no registry, module or lifecycle lock and cannot publish.
    /// Decode a payload with the provider's own type through
    /// [`stache_sdk::StateEnvelope::decode`], or project it for the frontend
    /// with [`crate::published_state::PublishedStateView`].
    #[must_use]
    pub fn observe_state(&self, provider: ModuleId) -> StateObserver {
        self.registry.observe_state(provider)
    }

    /// A read-only observer of the host's aggregated screen reservations.
    ///
    /// Subscribe before fetch: `read()` the snapshot, then `wait_for_change`
    /// with its revision. The snapshot carries the display topology generation,
    /// each display's maximum-aggregated edge insets and the exact owner and
    /// generation of every contribution. It cannot stage or remove anything and
    /// never reads a producer's configuration.
    #[must_use]
    pub fn observe_screen_reservations(&self) -> stache_sdk::ScreenReservationObserver {
        self.registry.observe_screen_reservations()
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

    /// Hands the host registry to a factory whose component must apply a host
    /// effect (the Bar-scoped screen-topology feed) before any module starts.
    ///
    /// Called once per factory, before `register`. A factory keeps only a
    /// `Weak`, so the registry that owns its module never owns the registry
    /// back. The default ignores it.
    fn bind_registry(&self, _registry: &Arc<Registry>) {}

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
        let snapshot = self.snapshot.clone();
        // The accepted watch labels each published-state envelope with the
        // revision the registry accepted; it is installed before any start.
        register_module_observed(
            registry,
            store,
            KeepAwakeModule::with_snapshot(
                self.owner.clone(),
                Arc::clone(&self.probe),
                self.observation.clone(),
                self.snapshot.clone(),
            ),
            move |registry, handle| {
                snapshot.install_accepted_watch(registry.accepted_watch(handle)?);
                Ok(())
            },
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

/// Registers the Tiling module over the host-lifetime runtime service.
///
/// Construction is inert: nothing native is installed, no thread is spawned and
/// no reservation is read until an enabled module starts. Registration installs
/// the typed accepted-configuration watch before the registry starts modules.
/// The factory serves the `tiling` socket methods (`pause`, `resume`, `query`,
/// `window`, `workspace`) through [`TilingIpc`]; the runtime details travel on
/// [`ModuleStatusView`]. `begin_shutdown` cancels every handler wait before the
/// connection joins.
///
/// The production composition (`lib.rs`) appends [`TilingFactory::production`],
/// default disabled; the module's own `enabled` setting starts it.
pub struct TilingFactory {
    runtime: TilingRuntime,
    /// The registry control, set by `register` and read by `ipc_handlers`.
    control: Arc<OnceLock<Arc<dyn ModuleControl>>>,
    /// The socket handlers' retained lifecycle workers, joined (bounded) by
    /// `finish_shutdown`; never detached.
    lifecycle_threads: Mutex<Vec<Arc<LifecycleThread>>>,
    /// The action lane the socket handler and the Bar focus path share.
    lane: ActionLane,
    /// The host registry, bound before `register`.
    registry: Arc<OnceLock<Weak<Registry>>>,
    /// The typed exact-focus path the Bar strip click uses.
    focus: Arc<TilingFocus>,
}

impl TilingFactory {
    /// Wraps the one host-lifetime runtime service.
    #[must_use]
    pub fn new(runtime: TilingRuntime) -> Self {
        let lane = ActionLane::default();
        let registry: Arc<OnceLock<Weak<Registry>>> = Arc::new(OnceLock::new());
        let source = Arc::clone(&registry);
        let focus = Arc::new(TilingFocus::new(
            Arc::new(runtime.clone()),
            Arc::new(move || {
                let registry = source.get()?.upgrade()?;
                registry
                    .snapshots()
                    .into_iter()
                    .find(|snapshot| snapshot.id == TilingConfig::ID)
            }),
            runtime.status(),
            runtime.shutdown_token(),
            IpcTiming::default(),
            lane.clone(),
        ));
        Self {
            runtime,
            control: Arc::new(OnceLock::new()),
            lifecycle_threads: Mutex::new(Vec::new()),
            lane,
            registry,
            focus,
        }
    }

    /// Tiling's typed exact-focus path for the Bar strip click. It shares the
    /// socket handler's lane, availability rules, bounded wait and shutdown
    /// token; it never pauses, resumes or reconfigures anything.
    #[must_use]
    pub fn focus(&self) -> Arc<TilingFocus> { Arc::clone(&self.focus) }

    /// Builds the service over a backend and directory, injecting this crate's
    /// native-to-SDK display projection and observing the host reservations
    /// through the module context.
    ///
    /// `applications` installs the exact-instance application actions (hide,
    /// unhide and the one `activate_exact` path) for each module run; build it
    /// with [`stache_tiling::ControlActionsSource`] over the host's
    /// `ApplicationControl`. `None` runs without them: nothing is hidden and
    /// focus cannot activate an application.
    #[must_use]
    pub fn composed(
        backend: Arc<dyn stache_tiling::WindowBackend>,
        directory: Arc<dyn AppDirectory>,
        applications: Option<Arc<dyn stache_tiling::ApplicationActionsSource>>,
        self_bundle_id: &str,
    ) -> Self {
        let mut parts = TilingParts::new(
            backend,
            directory,
            DisplayProjection::new(crate::display::sdk_display_id),
            self_bundle_id,
        );
        parts.applications = applications;
        Self::new(TilingRuntime::new(parts))
    }
}

impl TilingFactory {
    /// The production composition. Everything is inert until an enabled module
    /// starts:
    ///
    /// - the window backend is [`stache_tiling::LazySystemBackend`]: the
    ///   adapter is created on the first epoch install and its
    ///   `stache-tiling-ax-<n>` shard threads start on the first tracked
    ///   applications;
    /// - applications come from the run's own application observation over a
    ///   Tiling-private [`ApplicationControl`] (like every other consumer, one
    ///   control per module), used for exact hide, unhide, activation and the
    ///   application directory;
    /// - displays come from a system [`stache_macos::DisplayControl`] over the
    ///   same main-thread executor;
    /// - the `CGWindowID`-keyed tab member reads stay off
    ///   (`AdapterOptions::read_tabs` is false): their window id mapping is
    ///   proven wrong, so status reports tabs as not read. The owner instead
    ///   reads each window's tab bar identity (`read_tab_bars`), the shared
    ///   `AXTabGroup` element `AppKit` moves between the windows of a group.
    #[must_use]
    pub fn production(main_thread: Arc<dyn MainThreadExecutor>, self_bundle_id: &str) -> Self {
        let overlay_executor = Arc::clone(&main_thread);
        let applications = stache_tiling::ControlActionsSource::new(ApplicationControl::system(
            Arc::clone(&main_thread),
        ));
        let mut parts = TilingParts::new(
            Arc::new(stache_tiling::LazySystemBackend::new(
                stache_macos::windows::AdapterOptions {
                    read_tabs: false,
                    read_tab_bars: true,
                    // `None`: the system adapter probes VoiceOver itself.
                    ..stache_macos::windows::AdapterOptions::default()
                },
            )),
            Arc::new(NoApplications),
            DisplayProjection::new(crate::display::sdk_display_id),
            self_bundle_id,
        );
        parts.applications = Some(Arc::new(applications));
        parts.displays = Some(stache_macos::DisplayControl::system(main_thread));
        // Borders are drawn by Stache's own overlay control over the main-thread
        // executor. Nothing native exists yet: the control creates panels on
        // first show.
        {
            use stache_macos::border_overlay::{
                LazyCornerRadiusResolver, OverlayBackend, OverlayCapabilities, OverlayControl,
                RadiusCapability, SystemOverlayBackend, SystemSpaceEvents,
            };
            // Looked up lazily (first overlay radius request), so nothing native
            // happens here while borders are disabled. The capability starts as
            // FallbackOnly (true until known) and becomes Available when every
            // private radius symbol resolves; a missing one keeps the fallback
            // radius and its one diagnostic.
            let capabilities = OverlayCapabilities {
                radius: RadiusCapability::FallbackOnly,
                ..OverlayCapabilities::default()
            };
            let control_slot: Arc<std::sync::OnceLock<std::sync::Weak<OverlayControl>>> =
                Arc::new(std::sync::OnceLock::new());
            let observer_slot = Arc::clone(&control_slot);
            let resolver = LazyCornerRadiusResolver::system().observed(move |capability| {
                if let Some(control) = observer_slot.get().and_then(std::sync::Weak::upgrade) {
                    control.set_radius_capability(capability);
                }
            });
            let control = Arc::new(OverlayControl::new(
                overlay_executor,
                Arc::new(SystemOverlayBackend::new(Arc::new(resolver))) as Arc<dyn OverlayBackend>,
                capabilities,
            ));
            let _ = control_slot.set(Arc::downgrade(&control));
            parts.overlay = Some(control);
            // The Space and Mission Control sources and the active-Space
            // confirmation are factories: nothing native exists until the border
            // engine starts (borders enabled); the owner thread then registers
            // and the gate opens only after every source reports in. A stop
            // timeout is not observed yet (part 07 marks it owed).
            parts.space_events = Some(SystemSpaceEvents::factory(None));
            parts.space_confirm = Some(stache_tiling::system_confirmer_factory());
            // Live follow (part 06): the owner thread resolves the private
            // symbols and registers when the engine starts; the shared system
            // frame clock factory paces it. Without the symbols the Tiling
            // fallback hides a border while its window moves.
            parts.live_follow = Some(stache_macos::border_overlay::SystemLiveFollow::factory(
                Arc::new(stache_macos::SystemFrameClockFactory),
                None,
            ));
        }
        Self::new(TilingRuntime::new(parts))
    }
}

/// The directory of a run without application actions. A production run always
/// has actions (a run fails to start without them), so it never answers.
struct NoApplications;

impl AppDirectory for NoApplications {
    fn running(&self) -> Vec<stache_tiling::AppInfo> { Vec::new() }

    fn describe(&self, _: stache_macos::ApplicationId) -> Option<stache_tiling::AppInfo> { None }
}

impl ModuleFactory for TilingFactory {
    fn id(&self) -> ModuleId { TilingConfig::ID }

    fn bind_registry(&self, registry: &Arc<Registry>) {
        let _ = self.registry.set(Arc::downgrade(registry));
    }

    fn register(
        &self,
        registry: &Registry,
        store: &ConfigStore,
    ) -> Result<Arc<dyn ModuleControl>, HostError> {
        let runtime = self.runtime.clone();
        let control = register_module_observed(
            registry,
            store,
            runtime.module(),
            move |registry, handle| {
                runtime.install_accepted_watch(registry.accepted_watch(handle)?);
                Ok(())
            },
        )?;
        let _ = self.control.set(Arc::clone(&control));
        Ok(control)
    }

    /// The `tiling` socket methods: `pause`, `resume`, `query`, `window` and
    /// `workspace`. The handler reads published snapshots and drives the
    /// registry lifecycle; it starts nothing itself.
    fn ipc_handlers(&self, context: &FactoryContext) -> Vec<(&'static str, Arc<dyn IpcHandler>)> {
        let lifecycle = TilingLifecycle {
            context: context.clone(),
            control: Arc::clone(&self.control),
        };
        let handler = TilingIpc::new(
            Arc::new(self.runtime.clone()),
            Arc::new(lifecycle),
            self.runtime.status(),
            self.runtime.shutdown_token(),
        )
        .with_lane(self.lane.clone());
        self.lifecycle_threads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(handler.lifecycle_thread());
        vec![(TilingConfig::ID.as_str(), Arc::new(handler))]
    }

    /// A running module whose actor reports an unhealthy runtime is shown
    /// `unavailable` with the reason; a quarantined cleanup keeps the registry's
    /// failure and adds the owed stages through the runtime details. The
    /// registry lifecycle itself is never rewritten by a read or action error.
    fn project_status(&self, view: &mut ModuleStatusView) -> bool {
        if view.id != TilingConfig::ID.as_str() {
            return false;
        }
        let status = self.runtime.status().read();
        if view.state == crate::bindings::ModuleState::Running
            && status.state == TilingRuntimeState::Running
            && !status.healthy
        {
            view.state = crate::bindings::ModuleState::Unavailable;
            view.detail.clone_from(&status.detail);
        }
        view.tiling = Some(status);
        true
    }

    fn begin_shutdown(&self) { self.runtime.begin_shutdown(); }

    /// Joins any socket lifecycle worker (bounded, one fresh hop each) before
    /// draining the runtime. A worker still running is reported as not
    /// acknowledged and stays owned, never detached.
    fn finish_shutdown(&self, _wait: &dyn ShutdownWait) -> Result<bool, String> {
        let threads = self.lifecycle_threads.lock().unwrap_or_else(PoisonError::into_inner).clone();
        // Join every worker, never short-circuiting past one still running.
        let joined = threads
            .iter()
            .filter(|thread| !thread.join_within(TILING_LIFECYCLE_JOIN))
            .count()
            == 0;
        Ok(self.runtime.finish_shutdown() && joined)
    }
}

/// The bound for joining one Tiling socket lifecycle worker at shutdown.
const TILING_LIFECYCLE_JOIN: Duration = Duration::from_secs(2);

/// The registry lifecycle the Tiling socket handler drives: the published module
/// snapshot, and pause/resume through the registry (never a second path).
struct TilingLifecycle {
    context: FactoryContext,
    control: Arc<OnceLock<Arc<dyn ModuleControl>>>,
}

impl TilingLifecycle {
    fn failure(error: &RegistryError) -> LifecycleFailure {
        match error {
            RegistryError::ShuttingDown => LifecycleFailure::ShuttingDown,
            other => LifecycleFailure::Failed(other.to_string()),
        }
    }

    fn control(&self) -> Result<&Arc<dyn ModuleControl>, LifecycleFailure> {
        self.control
            .get()
            .ok_or_else(|| LifecycleFailure::Failed("Tiling is not initialized".to_owned()))
    }
}

impl LifecycleControl for TilingLifecycle {
    fn module(&self) -> Option<ModuleSnapshot> { self.context.module_snapshot(TilingConfig::ID) }

    fn pause(&self) -> Result<(), LifecycleFailure> {
        self.control()?
            .pause(&self.context.registry)
            .map_err(|error| Self::failure(&error))
    }

    fn resume(&self) -> Result<(), LifecycleFailure> {
        self.control()?
            .resume(&self.context.registry)
            .map_err(|error| Self::failure(&error))
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
    /// Live level filter used by the logger and `global.toml`.
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
    /// Host-level baseline permission requirements merged into every module.
    /// A baseline with Accessibility turns the launch gate on; an empty one
    /// (host test fixtures) leaves the host ungated.
    pub baseline: Vec<PermissionRequirement>,
    /// Opens System Settings for the guidance actions.
    pub settings: Arc<dyn SettingsOpener>,
}

/// What the host tells the application shell.
pub trait HostUi: Send + Sync {
    /// Publishes a new status snapshot.
    fn status(&self, status: &HostStatus);

    /// Announces that catalog files changed on disk (`config-changed`).
    fn config_changed(&self, files: &[String]);
}

/// The longest a `config_edit` caller waits for the host runner.
const CONFIG_EDIT_WAIT: Duration = Duration::from_secs(10);

/// The longest a popover enable request waits for the host runner.
const ENABLE_WAIT: Duration = Duration::from_secs(15);

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
    /// The single in-flight session action slot (see [`crate::session_action`]).
    session_slot: Arc<AtomicBool>,
    status: Arc<Mutex<HostStatus>>,
    teardown: Arc<Teardown>,
    finished: Arc<Finished>,
    /// Completed worker-loop passes. Test-only introspection.
    #[cfg(test)]
    passes: Arc<std::sync::atomic::AtomicUsize>,
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
        self.send(HostCommand::SetEnabled {
            id: id.into(),
            enabled,
            job: None,
        });
    }

    /// Requests a configuration-backed enable change and returns what the
    /// host did once the runner processed it (applied, unchanged or refused
    /// with the message it also publishes). The wait is bounded and a
    /// shutdown wakes it; a request still queued then is cancelled and never
    /// runs. Call only off the main thread.
    #[must_use]
    pub fn set_enabled_outcome(&self, id: impl Into<String>, enabled: bool) -> ModuleEnableOutcome {
        self.set_enabled_within(id, enabled, ENABLE_WAIT)
    }

    pub(crate) fn set_enabled_within(
        &self,
        id: impl Into<String>,
        enabled: bool,
        bound: Duration,
    ) -> ModuleEnableOutcome {
        if self.shutdown_requested() {
            return ModuleEnableOutcome::unavailable("the host is shutting down");
        }
        let job = HostJob::new();
        let command = HostCommand::SetEnabled {
            id: id.into(),
            enabled,
            job: Some(Arc::clone(&job)),
        };
        if self.sender.send(command).is_err() {
            return ModuleEnableOutcome::unavailable("the host is not running");
        }
        match job.wait(bound, || self.shutdown_requested() || self.is_finished()) {
            JobWait::Done(outcome) => outcome,
            JobWait::Cancelled { stopped: true } => {
                ModuleEnableOutcome::unavailable("the host is shutting down")
            }
            JobWait::Cancelled { stopped: false } => {
                ModuleEnableOutcome::unavailable("the host did not answer in time")
            }
            JobWait::Uncertain => {
                ModuleEnableOutcome::unavailable("the host did not confirm the change in time")
            }
        }
    }

    /// Runs one settings-editor edit on the host runner and returns its
    /// outcome: plan, commit, one reload and `config-changed`, in command order.
    ///
    /// The wait is bounded; a shutdown or a missing runner is `unavailable`.
    /// Call only off the main thread.
    #[must_use]
    pub fn config_edit(&self, request: ConfigEditRequest) -> ConfigEditOutcome {
        self.config_edit_within(request, CONFIG_EDIT_WAIT)
    }

    /// [`Self::config_edit`] with an explicit bound. An edit still queued when
    /// the wait ends is cancelled: the runner skips it and writes nothing.
    pub(crate) fn config_edit_within(
        &self,
        request: ConfigEditRequest,
        bound: Duration,
    ) -> ConfigEditOutcome {
        if self.shutdown_requested() {
            return ConfigEditOutcome::unavailable("the host is shutting down");
        }
        let job = HostJob::new();
        if self
            .sender
            .send(HostCommand::ConfigEdit { request, job: Arc::clone(&job) })
            .is_err()
        {
            return ConfigEditOutcome::unavailable("the host is not running");
        }
        match job.wait(bound, || self.shutdown_requested() || self.is_finished()) {
            JobWait::Done(outcome) => outcome,
            JobWait::Cancelled { stopped: true } => {
                ConfigEditOutcome::unavailable("the host is shutting down")
            }
            JobWait::Cancelled { stopped: false } => ConfigEditOutcome::unavailable(
                "the host did not answer in time; nothing was written",
            ),
            JobWait::Uncertain => {
                ConfigEditOutcome::unavailable("the host did not confirm the edit in time")
            }
        }
    }

    /// Requests a session-only pause.
    pub fn pause(&self, id: impl Into<String>) { self.send(HostCommand::Pause(id.into())); }

    /// Requests a session-only resume.
    pub fn resume(&self, id: impl Into<String>) { self.send(HostCommand::Resume(id.into())); }

    /// Pauses or resumes one module for this session and returns what the host
    /// actually did, after the host runner executed it in command order.
    ///
    /// Reuses the module controls the tray uses; it never enables, disables,
    /// retries or writes configuration. At most one request is in flight (a
    /// second is `busy`), the wait is bounded and a shutdown wakes it. Call only
    /// off the main thread.
    #[must_use]
    pub fn session_action(&self, module: &str, action: SessionAction) -> SessionActionOutcome {
        if self
            .session_slot
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return SessionActionOutcome::Busy;
        }
        let outcome = if self.shutdown_requested() {
            SessionActionOutcome::Unavailable {
                reason: SessionUnavailable::ShuttingDown,
            }
        } else {
            let job = SessionJob::new(module, action);
            if self.sender.send(HostCommand::Session(Arc::clone(&job))).is_err() {
                SessionActionOutcome::Unavailable {
                    reason: SessionUnavailable::NotRunning,
                }
            } else {
                job.wait(SESSION_WAIT, || self.shutdown_requested() || self.is_finished())
            }
        };
        self.session_slot.store(false, Ordering::SeqCst);
        outcome
    }

    /// Rechecks permissions after the application becomes active.
    pub fn refresh_permissions(&self) {
        self.send(HostCommand::RefreshPermissions(RefreshTrigger::Activation));
    }

    /// Prompts for a needed permission from an explicit user action.
    ///
    /// While the Accessibility gate is open this requests nothing.
    pub fn request_permission(&self) {
        self.send(HostCommand::RefreshPermissions(RefreshTrigger::Explicit));
    }

    /// Opens the Accessibility pane of System Settings while the gate is not open.
    pub fn open_accessibility_settings(&self) { self.send(HostCommand::OpenAccessibilitySettings); }

    /// Opens or focuses the Accessibility guidance window while the gate is not
    /// open.
    pub fn open_access_guide(&self) { self.send(HostCommand::OpenAccessGuide); }

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

impl SessionControl for HostHandle {
    fn session_action(&self, module: &str, action: SessionAction) -> SessionActionOutcome {
        Self::session_action(self, module, action)
    }
}

/// Creates the command handle over one coordinator and the not-yet-started driver.
#[must_use]
pub fn new_host(teardown: Arc<Teardown>) -> (HostHandle, HostDriver) {
    let (sender, receiver) = mpsc::channel();
    // The worker blocks on its channel, so every recorded stop request (any
    // `request`, the AppKit `begin_terminate`) must deliver a wake.
    let wake = sender.clone();
    teardown.on_request(Arc::new(move || {
        let _ = wake.send(HostCommand::Shutdown);
    }));
    let handle = HostHandle {
        sender,
        session_slot: Arc::new(AtomicBool::new(false)),
        status: Arc::new(Mutex::new(HostStatus::default())),
        teardown,
        finished: Arc::new(Finished {
            done: Mutex::new(false),
            signal: Condvar::new(),
        }),
        #[cfg(test)]
        passes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };
    (handle.clone(), HostDriver {
        receiver,
        handle,
        input: None,
        state_schemas: None,
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
    /// coordinator thread (unwind-enabled builds only; release aborts).
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
    state_schemas: Option<StateSchemas>,
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
    /// Registers the provider published-state schemas the app composed from the
    /// owning module crates.
    ///
    /// They are applied to the registry before any module registers, so a
    /// provider can only publish the type registered here. Without them (the
    /// default) no module publishes state and every observer reads absent.
    #[must_use]
    pub fn with_state_schemas(mut self, schemas: StateSchemas) -> Self {
        self.state_schemas = Some(schemas);
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
        if !services.baseline.is_empty() {
            registry = registry.with_baseline_requirements(services.baseline.clone())?;
        }
        if let Some(schemas) = &self.state_schemas {
            registry = registry.with_state_schemas(schemas)?;
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
    SetEnabled {
        id: String,
        enabled: bool,
        /// The popover's answer slot; the tray sends none.
        job: Option<Arc<HostJob<ModuleEnableOutcome>>>,
    },
    Condition(String),
    Pause(String),
    Resume(String),
    Session(Arc<SessionJob>),
    Retry(String),
    RefreshPermissions(RefreshTrigger),
    PermissionTick,
    Refresh,
    StartupFinished(Option<String>),
    ConfigChanges(ChangeSet),
    ConfigEdit {
        request: ConfigEditRequest,
        /// Claimed before `run_edit`; a cancelled job writes nothing.
        job: Arc<HostJob<ConfigEditOutcome>>,
    },
    WatcherFailed(String),
    OpenAccessibilitySettings,
    OpenAccessGuide,
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
    /// The hash of the bytes the host itself last committed per file, so the
    /// watcher's echo of that write is not announced a second time.
    echoes: BTreeMap<ConfigFileId, String>,
    ready: bool,
    recovery: Option<Arc<dyn StartupRecovery>>,
    /// The Accessibility launch gate, fed from every permission refresh.
    gate: AccessGate,
    /// Whether the baseline requires Accessibility. Without it the host is
    /// ungated: `access` reads open and nothing is deferred.
    gated: bool,
    /// The lock-free copy of the gate state the socket handlers read.
    access_cell: AccessCell,
    /// Where `exec_on_startup` stands in this launch.
    commands: StartupCommandsPhase,
    app_path: Option<String>,
    /// The guidance window of the current gated episode, if one was opened. A
    /// user close leaves the id stale; closing a stale id is a no-op.
    access_window: Option<WindowId>,
}

/// When `exec_on_startup` may run: once per launch, after setup and the gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupCommandsPhase {
    /// Setup is not complete yet.
    Setup,
    /// Setup is complete; waiting for the gate to open.
    Armed,
    /// Already started in this launch.
    Started,
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
        // Every accepted `global.toml` (first load and applied reloads)
        // applies `[ui] appearance` to the whole application (live and future
        // windows and panels inherit it); a rejected or unchanged reload never
        // reaches the backend.
        let windows = Arc::clone(&services.windows);
        store.on_global_accepted(Arc::new(move |config: &stache_sdk::GlobalConfig| {
            windows.set_appearance(config.ui.appearance);
        }));
        let schedule = PollSchedule::new(services.poll);
        let permissions = PermissionManager::new(Arc::clone(&services.permissions));
        let gated = services
            .baseline
            .iter()
            .any(|requirement| requirement.permission() == Permission::Accessibility);
        let app_path = std::env::current_exe().ok().and_then(|path| enclosing_app(&path));
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
            echoes: BTreeMap::new(),
            ready: false,
            recovery: None,
            gate: AccessGate::new(),
            gated,
            access_cell: if gated {
                AccessCell::checking()
            } else {
                AccessCell::open()
            },
            commands: StartupCommandsPhase::Setup,
            app_path,
            access_window: None,
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
            // Blocks without a deadline: every teardown request sends a wake
            // through the coordinator's request listener.
            let Ok(command) = receiver.recv() else {
                break;
            };
            #[cfg(test)]
            self.handle.passes.fetch_add(1, Ordering::Relaxed);
            // Once shutdown is requested, queued non-teardown work is dropped
            // instead of started.
            if self.handle.shutdown_requested() {
                break;
            }
            if !self.run_command(command) {
                break;
            }
        }
        self.finish(true);
    }

    /// Executes one queued command in order. `false` stops the loop.
    fn run_command(&mut self, command: HostCommand) -> bool {
        match command {
            HostCommand::Shutdown => return false,
            HostCommand::Condition(message) | HostCommand::WatcherFailed(message) => {
                self.add_condition(message);
            }
            HostCommand::SetEnabled { id, enabled, job } => match job {
                None => {
                    let _ = self.set_enabled(&id, enabled);
                }
                Some(job) => {
                    if job.begin() {
                        job.finish(self.set_enabled(&id, enabled));
                    }
                }
            },
            HostCommand::Pause(id) => self.pause(&id),
            HostCommand::Resume(id) => self.resume(&id),
            HostCommand::Session(job) => self.session_action(&job),
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
            HostCommand::ConfigEdit { request, job } => {
                if job.begin() {
                    job.finish(self.config_edit(&request));
                }
            }
            HostCommand::OpenAccessibilitySettings => self.open_accessibility_settings(),
            HostCommand::OpenAccessGuide => self.open_access_guide(),
        }
        true
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
            factory.bind_registry(&self.registry);
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
        self.log_stale_config_hint();
        self.log_config_trust_warnings();
        // The first nonprompting Accessibility check runs before any module
        // starts. It publishes nothing; the registry's own per-slot check still
        // guards every start, so a racing grant or revocation cannot start a
        // module without trust.
        self.check_permissions(RefreshTrigger::Startup);
        if self.handle.shutdown_requested() {
            return Ok(());
        }
        if let Err(error) = self.registry.start_all() {
            self.log_warn(format!("one or more modules failed to start: {error}"));
        }
        if self.handle.shutdown_requested() {
            return Ok(());
        }
        let context = FactoryContext::new(Arc::clone(&self.registry), Arc::clone(&self.store));
        let router = build_router(
            Arc::new(HostCore {
                core: ConfigCore::new(Arc::clone(&self.store)),
                sender: self.handle.sender.clone(),
                access: self.access_cell.clone(),
            }),
            &self.access_cell,
            &self.settings.factories,
            &context,
        )?;
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
        self.commands = StartupCommandsPhase::Armed;
        self.start_startup_commands_once();
        Ok(())
    }

    fn start_watcher(&self) -> Result<(), HostError> {
        let mut watcher = ConfigWatcher::new(&self.settings.config, self.store.catalog())?;
        let stop = CancellationToken::new();
        let worker_stop = stop.clone();
        let sender = self.handle.sender.clone();
        // Cancelling the job wakes the blocked wait through the watcher's own
        // channel; no native call runs on the cancelling thread.
        let wake = watcher.stop_handle();
        let thread = thread::Builder::new()
            .name("stache-config".to_owned())
            .spawn(move || {
                let _listener = worker_stop.on_cancel(Arc::new(move || wake.stop()));
                loop {
                    if worker_stop.is_cancelled() {
                        break;
                    }
                    match watcher.wait() {
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

    /// Starts `exec_on_startup` at most once per launch, and only once setup is
    /// complete and the gate is open (or the host is ungated).
    fn start_startup_commands_once(&mut self) {
        if self.commands != StartupCommandsPhase::Armed
            || !self.access_open()
            || self.handle.shutdown_requested()
        {
            return;
        }
        self.commands = StartupCommandsPhase::Started;
        self.start_startup_commands();
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
                    message: format!("exec_on_startup failed: {error}"),
                });
                let _ = sender.send(HostCommand::StartupFinished(Some(format!(
                    "exec_on_startup failed: {error}"
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
            Err(error) => self.add_condition(format!("cannot start exec_on_startup: {error}")),
        }
    }

    /// Applies a configuration-backed enable change and reports what happened;
    /// a refusal is also published as the action error.
    fn set_enabled(&mut self, id: &str, enabled: bool) -> ModuleEnableOutcome {
        let Some(control) = self.controls.get(id).cloned() else {
            return self.refuse_enable(format!("no module is registered as `{id}`"));
        };
        let module = control.id();
        let edit = match plan_enabled(self.store.catalog(), self.store.paths(), module, enabled) {
            Ok(edit) => edit,
            Err(error) => return self.refuse_enable(error.to_string()),
        };
        let hash = edit.hash();
        match edit.commit() {
            Ok(CommitOutcome::Written) => {
                if let Some(hash) = hash {
                    self.echoes.insert(ConfigFileId::module(module), hash);
                }
            }
            Ok(CommitOutcome::Unchanged) => {
                if self.handle.shutdown_requested() {
                    return ModuleEnableOutcome::unavailable("the host is shutting down");
                }
                // The file already has the requested value. Apply it once if the
                // runtime has not accepted the current bytes, then retry an
                // eligible failed start instead of only clearing the error.
                if let Err(error) = self.sync_module_file(module) {
                    return self.refuse_enable(error);
                }
                if let Err(error) = control.set_enabled(&self.registry, enabled) {
                    return self.refuse_enable(error.to_string());
                }
                self.clear_action_error();
                self.refresh_permissions(RefreshTrigger::Tick);
                return ModuleEnableOutcome::Unchanged;
            }
            Err(error) => return self.refuse_enable(error.to_string()),
        }
        if self.handle.shutdown_requested() {
            // The committed write is not rolled back, but no registry or
            // permission work may follow a teardown request.
            return ModuleEnableOutcome::Applied;
        }
        if let Err(error) = self.sync_module_file(module) {
            let outcome = self.refuse_enable(error);
            self.ui.config_changed(&[ConfigFileId::module(module).file_name()]);
            return outcome;
        }
        // A successful write clears an older action failure even when the
        // permission refresh itself reports nothing new.
        self.clear_action_error();
        self.refresh_permissions(RefreshTrigger::Tick);
        self.ui.config_changed(&[ConfigFileId::module(module).file_name()]);
        // A failed reload during the permission refresh must not hide the
        // successful edit; `refresh_permissions` publishes.
        ModuleEnableOutcome::Applied
    }

    /// Publishes `message` as the action error and returns it as a refusal.
    fn refuse_enable(&mut self, message: String) -> ModuleEnableOutcome {
        self.record_action_error(message.clone());
        ModuleEnableOutcome::Refused { message }
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

    /// Executes one result-returning session request on the host runner, in
    /// command order, through the same module controls as `pause`/`resume`.
    fn session_action(&mut self, job: &SessionJob) {
        if !job.begin() {
            return;
        }
        let outcome =
            session_action::execute(&self.registry, &self.controls, &job.module, job.action);
        match outcome {
            SessionActionOutcome::Applied => self.clear_action_error(),
            SessionActionOutcome::NotApplied { .. } => self
                .record_action_error(format!("module `{}` did not {:?}", job.module, job.action)),
            _ => {}
        }
        // Publishes the new session state (the Bar and the tray read it).
        self.refresh_permissions(RefreshTrigger::Tick);
        job.finish(outcome);
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

    /// Whether trust is present: the gate is open, or the host is ungated.
    const fn access_open(&self) -> bool { !self.gated || self.gate.is_open() }

    /// Reads fresh statuses through the permission manager and feeds the gate.
    ///
    /// An explicit request while the gate is open requests nothing. Returns the
    /// report with `waiting` true while the gate is not open, so the ticker
    /// keeps the 1 s delay even when every module is disabled.
    fn check_permissions(&mut self, trigger: RefreshTrigger) -> (RefreshReport, AccessTransition) {
        if self.handle.shutdown_requested() {
            // A late refresh could prompt or apply a status after invalidation.
            return (RefreshReport::default(), AccessTransition::None);
        }
        let trigger = if trigger == RefreshTrigger::Explicit && self.gated && self.gate.is_open() {
            RefreshTrigger::Tick
        } else {
            trigger
        };
        let host = RegistryPermissions {
            registry: Arc::clone(&self.registry),
            logger: Arc::clone(&self.services.logger),
            access: self.gated.then(|| self.access_cell.clone()),
        };
        let mut report = self.permissions.refresh(&host, trigger);
        let transition = self.observe_access(&report);
        report.waiting |= !self.access_open();
        (report, transition)
    }

    fn observe_access(&mut self, report: &RefreshReport) -> AccessTransition {
        if !self.gated {
            return AccessTransition::None;
        }
        let Some(status) = report
            .statuses
            .iter()
            .find(|(permission, _)| *permission == Permission::Accessibility)
            .map(|(_, status)| *status)
        else {
            return AccessTransition::None;
        };
        let transition = self.gate.observe(status);
        self.access_cell.store(self.gate.state());
        match transition {
            AccessTransition::None => {}
            AccessTransition::Blocked => {
                let reason = if status == PermissionStatus::Denied {
                    "denied"
                } else {
                    "unknown"
                };
                self.log(
                    stache_sdk::LogLevel::Info,
                    format!("access: blocked (accessibility {reason})"),
                );
                self.show_access_window();
            }
            AccessTransition::Opened { .. } => {
                self.log(stache_sdk::LogLevel::Info, "access: open".to_owned());
                self.close_access_window();
                // The phase guard keeps this at most once per launch.
                self.start_startup_commands_once();
            }
            AccessTransition::Revoked => {
                self.log(stache_sdk::LogLevel::Info, "access: revoked".to_owned());
                self.show_access_window();
            }
        }
        transition
    }

    fn set_ticker_delay(&mut self, waiting: bool) {
        if let Some(ticker) =
            self.monitors.ticker.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        {
            ticker.set_delay(self.schedule.next(waiting));
        }
    }

    fn refresh_permissions(&mut self, trigger: RefreshTrigger) -> RefreshReport {
        if self.handle.shutdown_requested() {
            return RefreshReport::default();
        }
        let (report, _) = self.check_permissions(trigger);
        if let Some(error) = &report.prompt_error {
            self.last_action_error = Some(format!("permission request failed: {error}"));
        } else if report.changed || report.prompted.is_some() {
            self.last_action_error = None;
        }
        self.set_ticker_delay(report.waiting);
        self.publish();
        report
    }

    fn permission_tick(&mut self) {
        if self.handle.shutdown_requested() {
            return;
        }
        let (report, transition) = self.check_permissions(RefreshTrigger::Tick);
        self.set_ticker_delay(report.waiting);
        if let Some(error) = &report.prompt_error {
            self.last_action_error = Some(format!("permission request failed: {error}"));
            self.publish();
        } else if report.changed
            || report.prompted.is_some()
            || transition != AccessTransition::None
        {
            self.publish();
        }
    }

    fn open_accessibility_settings(&mut self) {
        if self.handle.shutdown_requested() || self.access_open() {
            return;
        }
        match self.services.settings.open_accessibility_settings() {
            Ok(()) => self.clear_action_error(),
            Err(error) => {
                self.last_action_error =
                    Some(format!("cannot open Accessibility settings: {error}"));
            }
        }
        self.publish();
    }

    /// Opens (or focuses) the guidance window once for a gated episode.
    fn show_access_window(&mut self) {
        if self.handle.shutdown_requested() {
            return;
        }
        let request = match crate::windows::access_window_request() {
            Ok(request) => request,
            Err(error) => {
                self.log_warn(format!("cannot build the guidance window request: {error}"));
                return;
            }
        };
        match self.services.windows.open(HOST_OWNER, &request) {
            Ok(id) => {
                self.access_window = Some(id);
                self.log(
                    stache_sdk::LogLevel::Info,
                    "access: guidance window requested".to_owned(),
                );
            }
            Err(error) => {
                self.log_warn(format!("cannot open the guidance window: {error}"));
            }
        }
    }

    /// Closes the guidance window through the existing release path.
    fn close_access_window(&mut self) {
        if let Some(id) = self.access_window.take() {
            self.services.windows.close(id);
        }
    }

    /// The tray row: opens or focuses the guidance window while the gate is not
    /// open. A user close does not reopen it; only this row or the next
    /// episode does.
    fn open_access_guide(&mut self) {
        if self.handle.shutdown_requested() || self.access_open() {
            return;
        }
        let request = match crate::windows::access_window_request() {
            Ok(request) => request,
            Err(error) => {
                self.record_action_error(error.to_string());
                return;
            }
        };
        match self.services.windows.open(HOST_OWNER, &request) {
            Ok(id) => {
                self.access_window = Some(id);
                self.clear_action_error();
            }
            Err(error) => self.last_action_error = Some(error.to_string()),
        }
        self.publish();
    }

    /// Plans, commits and reloads one settings-editor edit, then publishes and
    /// announces the file. A refused edit writes, publishes and announces
    /// nothing.
    fn config_edit(&mut self, request: &ConfigEditRequest) -> ConfigEditOutcome {
        let run = run_edit(&self.store, request);
        if let Some((file, hash)) = run.written {
            self.echoes.insert(file, hash);
            if self.handle.shutdown_requested() {
                return run.outcome;
            }
            self.refresh_permissions(RefreshTrigger::Tick);
            self.ui.config_changed(&[file.file_name()]);
        }
        run.outcome
    }

    /// The catalog files a watcher batch announces, in catalog order, without
    /// the host's own echoes: a file whose bytes still hash to what the host
    /// committed and whose reload was not rejected.
    fn announced(
        &mut self,
        changes: &ChangeSet,
        outcomes: &[stache_sdk::FileOutcome],
    ) -> Vec<String> {
        self.store
            .catalog()
            .ids()
            .map(ConfigFileId::module)
            .filter(|file| changes.rescan || changes.files.contains(file))
            .filter(|file| {
                let Some(expected) = self.echoes.get(file) else {
                    return true;
                };
                let rejected = outcomes
                    .iter()
                    .any(|outcome| outcome.file == *file && outcome.outcome.diagnostic().is_some());
                let current = std::fs::read(self.store.paths().file(*file))
                    .ok()
                    .map(|bytes| stache_sdk::config::content_hash(&bytes));
                // One save can arrive as several batches; the echo entry stays
                // until the bytes differ from the host's own write.
                let echo = !rejected && current.as_deref() == Some(expected.as_str());
                if !echo {
                    self.echoes.remove(file);
                }
                !echo
            })
            .map(ConfigFileId::file_name)
            .collect()
    }

    fn apply_config(&mut self, changes: &ChangeSet) {
        let outcomes = self.store.apply(changes);
        if self.handle.shutdown_requested() {
            // The accepted write stays; no permission refresh or publication
            // may follow a teardown request.
            return;
        }
        let announced = self.announced(changes, &outcomes);
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
        if !announced.is_empty() {
            self.ui.config_changed(&announced);
        }
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
            access: self.access_view(),
        };
        self.handle.publish(status.clone());
        self.ui.status(&status);
    }

    fn access_view(&self) -> AccessView {
        let (state, revoked) = if self.gated {
            match self.gate.state() {
                AccessGateState::Checking => (AccessState::Checking, false),
                AccessGateState::Blocked { revoked } => (AccessState::Blocked, revoked),
                AccessGateState::Open => (AccessState::Open, false),
            }
        } else {
            (AccessState::Open, false)
        };
        AccessView {
            state,
            revoked,
            app_path: self.app_path.clone(),
        }
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

    /// Logs one record listing every `<id>.jsonc` that has no `<id>.toml`.
    ///
    /// Called once from `start`; reloads and watcher batches never repeat it.
    fn log_stale_config_hint(&self) {
        let stale = self.settings.config.stale_jsonc_files(self.store.catalog().ids());
        if stale.is_empty() {
            return;
        }
        let hints: Vec<String> =
            stale.iter().map(|name| stache_sdk::config::stale_jsonc_hint(name)).collect();
        self.log_warn(hints.join("; "));
    }

    /// Logs one record listing every configuration path someone else can
    /// write. Called once from `start`; never refuses a file.
    fn log_config_trust_warnings(&self) {
        let warnings =
            stache_sdk::config::inspect_catalog_trust(self.store.catalog(), &self.settings.config);
        if warnings.is_empty() {
            return;
        }
        let lines: Vec<String> = warnings.iter().map(ToString::to_string).collect();
        self.log_warn(lines.join("; "));
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
        PermissionStatus, ThreadError, WindowId, WindowRequest,
    };

    use super::*;

    pub(super) struct AllowAll;

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

        fn config_changed(&self, _: &[String]) {}
    }

    pub(super) fn quiet_teardown() -> Arc<Teardown> {
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

    /// Services over fakes with one permission polling period.
    fn quiet_services(permissions: Arc<dyn PermissionProvider>, poll: Duration) -> HostServices {
        HostServices {
            logger: Arc::new(DiscardLogger),
            log_filter: Arc::new(LogFilter::default()),
            events: Arc::new(EventBus::new()),
            main_thread: Arc::new(NoMainThread),
            permissions,
            windows: Arc::new(NoWindows),
            commands: Arc::new(NoCommands),
            poll: PermissionPoll {
                waiting: poll,
                backoff_start: poll,
                backoff_max: poll,
            },
            baseline: Vec::new(),
            settings: Arc::new(crate::access::UnavailableSettings),
        }
    }

    pub(super) fn runner_over(
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
        let services = quiet_services(Arc::clone(&permissions), Duration::from_millis(1));
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

    /// Starts the real host worker over fakes, with permission polling slow
    /// enough that no tick lands during a test, and waits until it is ready.
    fn started_worker(root: &Path) -> (HostHandle, Arc<Teardown>) {
        let teardown = quiet_teardown();
        let (handle, driver) = new_host(Arc::clone(&teardown));
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
        driver
            .start(
                settings,
                quiet_services(Arc::new(AllowAll), Duration::from_secs(3600)),
                Arc::new(QuietUi),
            )
            .expect("start host");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !handle.status().ready {
            assert!(Instant::now() < deadline, "the host never became ready");
            thread::sleep(Duration::from_millis(5));
        }
        (handle, teardown)
    }

    /// Asserts the idle worker completes no pass, then that `request` ends the
    /// loop within 100 ms, and finally lets the coordinator clean up.
    fn assert_wakes_on(request: impl FnOnce(&HostHandle, &Teardown)) {
        let root = tempfile::tempdir().expect("root");
        let (handle, teardown) = started_worker(root.path());
        // Startup may still deliver its own completions; let them settle.
        thread::sleep(Duration::from_millis(100));
        let settled = handle.passes.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(500));
        assert_eq!(
            handle.passes.load(Ordering::SeqCst),
            settled,
            "the idle worker loop woke up"
        );

        let requested = Instant::now();
        request(&handle, &teardown);
        assert!(
            handle.wait_for_exit(Duration::from_secs(2)),
            "the loop never ended"
        );
        let elapsed = requested.elapsed();
        assert!(
            elapsed < Duration::from_millis(100),
            "the loop ended after {elapsed:?}"
        );

        teardown.start().expect("start the coordinator");
        let _ = teardown.wait_for_outcome(Duration::from_secs(10));
    }

    #[test]
    fn the_worker_loop_wakes_on_request_shutdown() {
        // Tray quit, the `quit_app` binding, the signal bridge and smoke runs.
        assert_wakes_on(|handle, _| handle.request_shutdown());
    }

    #[test]
    fn the_worker_loop_wakes_on_the_tauri_exit_request() {
        assert_wakes_on(|handle, _| handle.request_shutdown_with_code(Some(3)));
    }

    #[test]
    fn the_worker_loop_wakes_on_request_restart() {
        assert_wakes_on(|handle, _| handle.request_restart());
    }

    #[test]
    fn the_worker_loop_wakes_on_the_appkit_terminate_path() {
        assert_wakes_on(|_, teardown| {
            assert_eq!(
                teardown.begin_terminate(),
                crate::shutdown::TeardownAnswer::Later
            );
        });
    }

    #[test]
    fn the_worker_loop_wakes_on_a_direct_coordinator_request() {
        assert_wakes_on(|_, teardown| teardown.request(TeardownCause::Request, None));
    }

    /// Counts requests over a status that tests change between refreshes.
    struct Counted {
        status: Mutex<PermissionStatus>,
        requests: AtomicUsize,
    }

    impl Counted {
        fn new(status: PermissionStatus) -> Arc<Self> {
            Arc::new(Self {
                status: Mutex::new(status),
                requests: AtomicUsize::new(0),
            })
        }

        fn set(&self, status: PermissionStatus) { *self.status.lock().expect("status") = status; }

        fn requests(&self) -> usize { self.requests.load(Ordering::SeqCst) }
    }

    impl PermissionProvider for Counted {
        fn status(&self, _: Permission) -> PermissionStatus { *self.status.lock().expect("status") }

        fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            Ok(*self.status.lock().expect("status"))
        }
    }

    #[derive(Default)]
    struct Opens {
        calls: AtomicUsize,
        fail: AtomicBool,
    }

    impl SettingsOpener for Opens {
        fn open_accessibility_settings(&self) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                Err("denied by test".to_owned())
            } else {
                Ok(())
            }
        }
    }

    /// A runner over the production baseline: Accessibility gates the host.
    fn gated_runner(
        root: &Path,
        permissions: Arc<dyn PermissionProvider>,
        opener: Arc<Opens>,
    ) -> Runner {
        let mut catalog = ConfigCatalog::new();
        catalog
            .register::<stache_sdk::GlobalConfig>(stache_sdk::GlobalConfig::ID)
            .expect("global config");
        let (mut runner, _) = runner_over(
            root,
            ModuleFactories::new(),
            catalog,
            Arc::clone(&permissions),
            None,
        );
        let baseline = vec![PermissionRequirement::user_only(Permission::Accessibility)];
        let registry = Registry::new(
            Arc::new(DiscardLogger),
            Arc::new(EventBus::new()),
            Arc::new(NoMainThread),
            permissions,
            Arc::new(NoWindows),
        )
        .with_baseline_requirements(baseline.clone())
        .expect("baseline");
        runner.registry = Arc::new(registry);
        runner.gated = true;
        runner.services.baseline = baseline;
        runner.services.settings = opener;
        runner
    }

    #[test]
    fn a_closed_gate_keeps_the_waiting_poll_with_every_module_disabled() {
        let root = tempfile::tempdir().expect("root");
        let permissions = Counted::new(PermissionStatus::Denied);
        let mut runner = gated_runner(root.path(), permissions.clone(), Arc::default());
        let (report, transition) = runner.check_permissions(RefreshTrigger::Startup);
        assert_eq!(transition, AccessTransition::Blocked);
        assert!(report.waiting, "no module exists, yet the closed gate waits");
        assert_eq!(runner.access_view().state, AccessState::Blocked);
        // `set_ticker_delay` hands the ticker `schedule.next(waiting)`: the
        // waiting delay while gated, even with no module enabled.
        let waiting_delay = Duration::from_millis(7);
        runner.schedule = PollSchedule::new(PermissionPoll {
            waiting: waiting_delay,
            backoff_start: Duration::from_millis(100),
            backoff_max: Duration::from_millis(200),
        });
        assert_eq!(runner.schedule.next(report.waiting), waiting_delay);

        permissions.set(PermissionStatus::Granted);
        let (report, transition) = runner.check_permissions(RefreshTrigger::Tick);
        assert_eq!(transition, AccessTransition::Opened { first: true });
        assert!(!report.waiting, "an open gate with nothing waiting backs off");
        assert_eq!(
            runner.schedule.next(report.waiting),
            Duration::from_millis(100),
            "an open gate returns to the backoff delay"
        );
        assert_eq!(permissions.requests(), 0);
    }

    /// Counts the startup commands the host runs.
    #[derive(Default)]
    struct CountingCommands {
        runs: AtomicUsize,
    }

    impl CommandRunner for CountingCommands {
        fn run(&self, _: &[String], _: &CancellationToken) -> Result<(), String> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn wait_for_runs(commands: &CountingCommands, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while commands.runs.load(Ordering::SeqCst) < expected {
            assert!(Instant::now() < deadline, "startup commands did not run");
            thread::sleep(Duration::from_millis(5));
        }
        // Give a wrongful extra run time to show up.
        thread::sleep(Duration::from_millis(50));
        assert_eq!(commands.runs.load(Ordering::SeqCst), expected);
    }

    #[derive(Default)]
    struct CaptureLogger(Mutex<Vec<stache_sdk::LogRecord>>);

    impl stache_sdk::Logger for CaptureLogger {
        fn log(&self, record: stache_sdk::LogRecord) {
            self.0.lock().expect("records").push(record);
        }
    }

    #[test]
    fn the_stale_jsonc_hint_is_logged_once_at_startup_and_not_on_reload() {
        let root = tempfile::tempdir().expect("root");
        let config = root.path().join("config");
        std::fs::create_dir_all(&config).expect("config dir");
        std::fs::write(config.join("tiling.jsonc"), "{}").expect("tiling.jsonc");
        std::fs::write(config.join("bar.jsonc"), "{}").expect("bar.jsonc");
        std::fs::write(config.join("bar.toml"), "").expect("bar.toml");
        let mut catalog = ConfigCatalog::new();
        catalog
            .register::<stache_sdk::GlobalConfig>(stache_sdk::GlobalConfig::ID)
            .expect("global");
        catalog
            .register::<stache_tiling::TilingConfig>(stache_tiling::TilingConfig::ID)
            .expect("tiling");
        let (mut runner, _receiver) = runner_over(
            root.path(),
            ModuleFactories::new(),
            catalog,
            Arc::new(AllowAll),
            None,
        );
        let capture = Arc::new(CaptureLogger::default());
        runner.services.logger = capture.clone();
        runner.start().expect("start");
        let hints = |capture: &CaptureLogger| -> Vec<String> {
            capture
                .0
                .lock()
                .expect("records")
                .iter()
                .filter(|record| record.message.contains("stache config migrate"))
                .map(|record| record.message.clone())
                .collect()
        };
        assert_eq!(hints(&capture), vec![
            "note: tiling.jsonc is ignored; run `stache config migrate`".to_owned()
        ]);
        let level = capture.0.lock().expect("records")[0].level;
        assert_eq!(level, stache_sdk::LogLevel::Warn);

        runner.apply_config(&ChangeSet {
            rescan: true,
            files: std::collections::BTreeSet::default(),
        });
        assert_eq!(hints(&capture).len(), 1, "reloads never repeat the hint");
        runner.finish(true);
    }

    #[test]
    fn config_trust_warnings_are_logged_once_at_startup_and_not_on_reload() {
        use std::os::unix::fs::PermissionsExt;
        let start_with = |mode: u32| {
            let root = tempfile::tempdir().expect("root");
            let config = root.path().join("config");
            std::fs::create_dir_all(&config).expect("config dir");
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(mode))
                .expect("chmod");
            let mut catalog = ConfigCatalog::new();
            catalog
                .register::<stache_sdk::GlobalConfig>(stache_sdk::GlobalConfig::ID)
                .expect("global");
            let (mut runner, receiver) = runner_over(
                root.path(),
                ModuleFactories::new(),
                catalog,
                Arc::new(AllowAll),
                None,
            );
            let capture = Arc::new(CaptureLogger::default());
            runner.services.logger = capture.clone();
            runner.start().expect("start");
            (root, config, runner, receiver, capture)
        };
        let trust = |capture: &CaptureLogger| -> Vec<(stache_sdk::LogLevel, String)> {
            capture
                .0
                .lock()
                .expect("records")
                .iter()
                .filter(|record| record.message.contains("#trust-model"))
                .map(|record| (record.level, record.message.clone()))
                .collect()
        };

        let (_root, _config, runner, _receiver, capture) = start_with(0o755);
        assert!(trust(&capture).is_empty(), "clean configuration logs nothing");
        runner.finish(true);

        let (_root, config, mut runner, _receiver, capture) = start_with(0o775);
        let owner = stache_sdk::config::current_owner();
        assert_eq!(trust(&capture), vec![(
            stache_sdk::LogLevel::Warn,
            format!(
                "warning: {} is group-writable (mode 0775, owner uid {owner}); Stache runs commands from its configuration, see docs/architecture/config.md#trust-model",
                std::fs::canonicalize(&config).expect("canonical").display()
            )
        )]);
        runner.apply_config(&ChangeSet {
            rescan: true,
            files: std::collections::BTreeSet::default(),
        });
        assert_eq!(trust(&capture).len(), 1, "reloads never repeat the warning");
        runner.finish(true);
    }

    #[test]
    fn startup_commands_run_once_even_when_the_first_opening_precedes_arming() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join("config")).expect("config dir");
        std::fs::write(
            root.path().join("config/global.toml"),
            "exec_on_startup = [\"true\"]\n",
        )
        .expect("global config");
        let permissions = Counted::new(PermissionStatus::Granted);
        let mut runner = gated_runner(root.path(), permissions.clone(), Arc::default());
        let commands = Arc::new(CountingCommands::default());
        runner.services.commands = commands.clone();
        runner.store.load_global();

        // The first opening lands while setup is still incomplete: no-op.
        let (_, transition) = runner.check_permissions(RefreshTrigger::Startup);
        assert_eq!(transition, AccessTransition::Opened { first: true });
        assert_eq!(runner.commands, StartupCommandsPhase::Setup);
        // Trust is lost before arming.
        permissions.set(PermissionStatus::Denied);
        let (_, transition) = runner.check_permissions(RefreshTrigger::Tick);
        assert_eq!(transition, AccessTransition::Revoked);
        runner.commands = StartupCommandsPhase::Armed;
        runner.start_startup_commands_once();
        assert_eq!(runner.commands, StartupCommandsPhase::Armed, "still gated");

        // The next grant is not `first`, yet it must run the commands once.
        permissions.set(PermissionStatus::Granted);
        let (_, transition) = runner.check_permissions(RefreshTrigger::Tick);
        assert_eq!(transition, AccessTransition::Opened { first: false });
        assert_eq!(runner.commands, StartupCommandsPhase::Started);
        wait_for_runs(&commands, 1);

        // A later revoke and re-grant does not rerun them.
        permissions.set(PermissionStatus::Denied);
        runner.check_permissions(RefreshTrigger::Tick);
        permissions.set(PermissionStatus::Granted);
        runner.check_permissions(RefreshTrigger::Tick);
        wait_for_runs(&commands, 1);
        let job = runner.monitors.startup.lock().expect("cell").take();
        if let Some(job) = job {
            job.stop_bounded(&LateReapWait::new());
        }
    }

    #[test]
    fn an_explicit_request_while_open_is_downgraded_to_a_check() {
        let root = tempfile::tempdir().expect("root");
        let permissions = Counted::new(PermissionStatus::Denied);
        let mut runner = gated_runner(root.path(), permissions.clone(), Arc::default());
        runner.check_permissions(RefreshTrigger::Startup);
        runner.check_permissions(RefreshTrigger::Tick);
        assert_eq!(permissions.requests(), 0);
        runner.check_permissions(RefreshTrigger::Explicit);
        assert_eq!(permissions.requests(), 1);

        permissions.set(PermissionStatus::Granted);
        runner.check_permissions(RefreshTrigger::Tick);
        assert!(runner.gate.is_open());
        // The status turns Denied right before the explicit click: the gate is
        // still open, so the click only detects the revocation.
        permissions.set(PermissionStatus::Denied);
        let (_, transition) = runner.check_permissions(RefreshTrigger::Explicit);
        assert_eq!(transition, AccessTransition::Revoked);
        assert_eq!(permissions.requests(), 1);
        assert!(runner.access_view().revoked);
    }

    #[test]
    fn a_settings_failure_is_the_action_error_until_the_next_success() {
        let root = tempfile::tempdir().expect("root");
        let opener = Arc::new(Opens::default());
        let mut runner = gated_runner(
            root.path(),
            Counted::new(PermissionStatus::Denied),
            opener.clone(),
        );
        runner.check_permissions(RefreshTrigger::Startup);
        opener.fail.store(true, Ordering::SeqCst);
        runner.open_accessibility_settings();
        assert!(runner.error_text().expect("error").contains("denied by test"));
        opener.fail.store(false, Ordering::SeqCst);
        runner.open_accessibility_settings();
        assert_eq!(runner.error_text(), None);
        assert_eq!(opener.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn after_a_shutdown_request_nothing_is_requested_opened_or_checked() {
        let root = tempfile::tempdir().expect("root");
        let permissions = Counted::new(PermissionStatus::Denied);
        let opener = Arc::new(Opens::default());
        let mut runner = gated_runner(root.path(), permissions.clone(), opener.clone());
        runner.check_permissions(RefreshTrigger::Startup);
        runner.handle.request_shutdown();
        let (report, transition) = runner.check_permissions(RefreshTrigger::Explicit);
        runner.open_accessibility_settings();
        assert_eq!(report, RefreshReport::default());
        assert_eq!(transition, AccessTransition::None);
        assert_eq!(permissions.requests(), 0);
        assert_eq!(opener.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_ungated_host_reads_open_and_ignores_the_gate() {
        let root = tempfile::tempdir().expect("root");
        let mut runner = runner(root.path());
        let (report, transition) = runner.check_permissions(RefreshTrigger::Startup);
        assert_eq!(transition, AccessTransition::None);
        assert!(!report.waiting);
        assert_eq!(runner.access_view().state, AccessState::Open);
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
    fn the_recovery_step_runs_once_and_reports_its_condition_once() {
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
    fn a_quiet_step_and_a_panicking_step_do_not_disturb_startup() {
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
    fn teardown_cancels_a_slow_recovery_and_reaps_its_thread() {
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
    fn a_step_started_after_teardown_closed_the_cells_is_cancelled_and_reaped() {
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
    fn the_production_recovery_restores_a_stale_journal_over_fakes() {
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
        assert!(
            !ids.contains(&stache_bar::BarConfig::ID),
            "Bar is appended by the composition root (`lib.rs`), not by this base list"
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

        // The same context exposes the host's read-only reservation snapshot.
        let reservations = context.observe_screen_reservations().read();
        assert_eq!(reservations.revision, 0);
        assert!(reservations.displays.is_empty());
        assert_eq!(
            reservations,
            registry.observe_screen_reservations().read(),
            "the factory context and the registry read one hub"
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
                observes: &[],
                reserves_screen: false,
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
                observes: &[],
                reserves_screen: false,
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
    fn host_policy_never_prompts_for_shared_input_except_on_explicit_request() {
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
            access: None,
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

    const KB_ON: &str = "enabled = true\nbindings = { \"Cmd+Q\" = \"/bin/sh hold-q\" }\n";

    #[test]
    fn the_keybindings_factory_is_inert_until_start_and_projects_runtime_details() {
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
    fn held_file_diagnostics_and_runtime_details_coexist_and_repair_independently() {
        let mut host = KbHost::new(KbRig::new(), Some(KB_ON));
        host.start();
        let revision = host.view().revision;
        // An invalid file is held; the running details are untouched.
        host.write("enabled = ");
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
        host.write("enabled = true\nbindings = { \"Cmd+W\" = \"/bin/sh quick-w\" }\n");
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
    fn a_rejected_or_unchanged_file_reload_cancels_no_accepted_work() {
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
    fn host_caps_recovery_runs_with_the_module_disabled_and_an_invalid_file() {
        use stache_keybindings::caps::CapsRecoveryStep;
        use stache_macos::caps_remap::{Mapping, OWNED};

        let key = Mapping { src: 100, dst: 200 };
        let rig = KbRig::with_mappings(vec![key]);
        // A crash left this build identity's own mapping and journal behind.
        rig.remap.apply(&CancellationToken::new()).expect("apply");
        rig.world.restart();
        assert_eq!(rig.world.mappings(), vec![key, OWNED]);
        let sets_before = rig.world.sets();
        // The module file is invalid and the module is therefore not enabled.
        let mut host = KbHost::new(rig, Some("not valid ="));
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
    fn host_shutdown_closes_admission_then_stops_the_module_then_drains() {
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
    fn two_independent_fake_hosts_share_nothing() {
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

    // -----------------------------------------------------------------------
    // Tiling over the actual runner, registry and configuration store (fakes)
    // -----------------------------------------------------------------------

    use stache_macos::windows::StopOutcome;
    use stache_tiling::{FakeBackendCall, FakeWindowBackend, TilingRuntimeState};

    struct NoApps;

    /// Runs main-thread jobs inline on the calling thread.
    struct InlineMain;

    impl MainThreadExecutor for InlineMain {
        fn is_main_thread(&self) -> bool { false }

        fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
            job();
            Ok(())
        }
    }

    fn ti_layout() -> stache_macos::DisplayLayout {
        stache_macos::DisplayLayout::new(vec![stache_macos::DisplayInfo {
            id: stache_macos::DisplayId::new(1),
            frame: stache_macos::DisplayRect::new(0.0, 0.0, 1920.0, 1080.0),
            visible_frame: stache_macos::DisplayRect::new(0.0, 0.0, 1920.0, 1055.0),
            scale: 2.0,
            refresh_hz: Some(60.0),
            name: Some("Display 1".to_owned()),
            is_primary: true,
            is_main: true,
            is_builtin: false,
        }])
    }

    impl AppDirectory for NoApps {
        fn running(&self) -> Vec<stache_tiling::AppInfo> { Vec::new() }

        fn describe(&self, _: stache_macos::ApplicationId) -> Option<stache_tiling::AppInfo> {
            None
        }
    }

    struct TiHost {
        runner: Runner,
        backend: Arc<FakeWindowBackend>,
        factory: Arc<TilingFactory>,
        display: stache_macos::FakeDisplayBackend,
        _receiver: Receiver<HostCommand>,
        config: stache_sdk::ConfigPaths,
        _root: tempfile::TempDir,
    }

    /// Counts requests: Tiling and the registry only ever check.
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

    impl TiHost {
        fn new(file: Option<&str>) -> Self { Self::composed(file, Vec::new(), true).0 }

        /// Tiling over fakes plus `extra` independent factories.
        fn composed(
            file: Option<&str>,
            extra: Vec<Arc<dyn ModuleFactory>>,
            trusted: bool,
        ) -> (Self, Arc<Trust>) {
            let root = tempfile::tempdir().expect("root");
            let config = ConfigPaths::new(root.path().join("config"));
            std::fs::create_dir_all(root.path().join("config")).expect("config directory");
            let backend = Arc::new(FakeWindowBackend::default());
            let display = stache_macos::FakeDisplayBackend::new(ti_layout());
            let mut parts = TilingParts::new(
                Arc::clone(&backend) as Arc<dyn stache_tiling::WindowBackend>,
                Arc::new(NoApps),
                DisplayProjection::new(crate::display::sdk_display_id),
                "",
            );
            parts.displays = Some(stache_macos::DisplayControl::new(
                Arc::new(InlineMain),
                Arc::new(display.clone()),
            ));
            parts.timing.hop = Duration::from_millis(60);
            parts.timing.poll = Duration::from_millis(5);
            let factory = Arc::new(TilingFactory::new(TilingRuntime::new(parts)));
            let mut factories = ModuleFactories::new();
            factories.entries.push(Arc::clone(&factory) as Arc<dyn ModuleFactory>);
            factories.entries.extend(extra);
            let trust = Arc::new(Trust {
                trusted: AtomicBool::new(trusted),
                requests: AtomicUsize::new(0),
            });
            let (runner, receiver) = runner_over(
                root.path(),
                factories,
                crate::catalog::catalog().expect("catalog"),
                Arc::clone(&trust) as Arc<dyn PermissionProvider>,
                None,
            );
            let host = Self {
                runner,
                backend,
                factory,
                display,
                _receiver: receiver,
                config,
                _root: root,
            };
            if let Some(file) = file {
                host.write(file);
            }
            (host, trust)
        }

        fn path(&self) -> std::path::PathBuf { self.config.module_file(TilingConfig::ID) }

        fn write(&self, text: &str) { std::fs::write(self.path(), text).expect("write"); }

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
            changes.files.insert(ConfigFileId::module(TilingConfig::ID));
            self.runner.apply_config(&changes);
            self.runner.publish();
        }

        fn view(&self) -> ModuleStatusView {
            self.runner
                .handle
                .status()
                .modules
                .into_iter()
                .find(|module| module.id == "tiling")
                .expect("tiling module")
        }
    }

    const TI_ON: &str = "enabled = true\n";

    /// End to end over the real worker, watcher and coordinator: an edit to
    /// `tiling.toml` reaches the blocked config thread and is applied, and a
    /// teardown request still ends everything `Clean`.
    #[test]
    fn a_tiling_edit_reloads_through_the_blocking_waits_and_teardown_is_clean() {
        let root = tempfile::tempdir().expect("root");
        let config = ConfigPaths::new(root.path().join("config"));
        std::fs::create_dir_all(config.directory()).expect("config directory");
        std::fs::write(config.module_file(TilingConfig::ID), TI_ON).expect("write");
        let backend = Arc::new(FakeWindowBackend::default());
        let display = stache_macos::FakeDisplayBackend::new(ti_layout());
        let mut parts = TilingParts::new(
            Arc::clone(&backend) as Arc<dyn stache_tiling::WindowBackend>,
            Arc::new(NoApps),
            DisplayProjection::new(crate::display::sdk_display_id),
            "",
        );
        parts.displays = Some(stache_macos::DisplayControl::new(
            Arc::new(InlineMain),
            Arc::new(display),
        ));
        parts.timing.hop = Duration::from_millis(60);
        parts.timing.poll = Duration::from_millis(5);
        let mut factories = ModuleFactories::new();
        factories.entries.push(Arc::new(TilingFactory::new(TilingRuntime::new(parts))));
        let app = AppPaths::resolve(&AppEnvironment {
            home: Some(root.path().to_path_buf()),
            temporary: Some(root.path().to_path_buf()),
        })
        .expect("app paths");
        let settings = HostSettings {
            app,
            config: config.clone(),
            catalog: crate::catalog::catalog().expect("catalog"),
            factories,
        };
        let teardown = quiet_teardown();
        let (handle, driver) = new_host(Arc::clone(&teardown));
        driver
            .start(
                settings,
                quiet_services(Arc::new(AllowAll), Duration::from_secs(3600)),
                Arc::new(QuietUi),
            )
            .expect("start host");
        teardown.start().expect("start the coordinator");
        let tiling = |handle: &HostHandle| {
            handle.status().modules.into_iter().find(|module| module.id == "tiling")
        };
        ti_wait("ready", || handle.status().ready);
        let revision = tiling(&handle).expect("tiling module").revision;

        std::fs::write(
            config.module_file(TilingConfig::ID),
            "enabled = true\ndefault_layout = \"floating\"\n",
        )
        .expect("edit");
        ti_wait("the watcher applied the edit", || {
            tiling(&handle).is_some_and(|view| view.revision != revision)
        });
        assert!(tiling(&handle).expect("tiling module").diagnostic.is_none());

        handle.request_shutdown();
        assert!(
            handle.wait_for_exit(Duration::from_secs(5)),
            "the worker never ended"
        );
        let outcome = teardown.wait_for_outcome(Duration::from_secs(10)).expect("outcome");
        assert_eq!(
            outcome.result,
            crate::shutdown::TeardownResult::Clean,
            "{outcome}"
        );
    }

    fn ti_wait(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn the_tiling_factory_is_inert_until_start_and_projects_runtime_details() {
        let mut host = TiHost::new(Some(TI_ON));
        host.register();
        assert!(
            host.backend.calls().is_empty(),
            "registration touches nothing native"
        );
        host.runner.registry.start_all().expect("start");
        host.runner.publish();
        let view = host.view();
        assert_eq!(view.state, crate::bindings::ModuleState::Running);
        let details = view.tiling.as_ref().expect("runtime details");
        assert_eq!(details.state, TilingRuntimeState::Running);
        assert!(details.healthy);
        ti_wait("the actor adopted the revision", || {
            host.runner.publish();
            host.view().tiling.is_some_and(|t| t.config_revision == host.view().revision)
        });
        let json = serde_json::to_value(host.runner.handle.status()).expect("encode");
        let module = json["modules"]
            .as_array()
            .expect("modules")
            .iter()
            .find(|m| m["id"] == "tiling")
            .expect("tiling");
        assert_eq!(module["tiling"]["state"], "running");
        for other in json["modules"].as_array().expect("modules") {
            if other["id"] != "tiling" {
                assert!(other.get("tiling").is_none(), "details are tiling-only");
            }
        }
        let _ = host.runner.registry.shutdown();
        assert!(host.backend.calls().iter().any(|c| matches!(c, FakeBackendCall::Stop(_))));
    }

    /// The production list plus the production Tiling factory, as `lib.rs`
    /// composes them, over a main thread that runs nothing.
    fn production_with_tiling() -> ModuleFactories {
        ModuleFactories::production(Arc::new(NoMainThread), AssertionOwner::system()).with(
            TilingFactory::production(Arc::new(NoMainThread), "com.example.stache"),
        )
    }

    /// The production list plus the production Bar factory, as `lib.rs`
    /// composes them, over a main thread that runs nothing.
    fn production_with_bar() -> ModuleFactories {
        ModuleFactories::production(Arc::new(NoMainThread), AssertionOwner::system())
            .with(crate::bar::BarFactory::production(Arc::new(NoMainThread)))
    }

    #[test]
    fn the_production_composition_registers_bar_default_disabled_and_inert() {
        let factories = production_with_bar();
        assert_eq!(
            factories.entries().last().map(|factory| factory.id()),
            Some(stache_bar::BarConfig::ID),
            "Bar is chained after the base list"
        );
        let root = tempfile::tempdir().expect("root");
        let (mut runner, _receiver) = runner_over(
            root.path(),
            factories,
            crate::catalog::catalog().expect("catalog"),
            Arc::new(AllowAll),
            None,
        );
        runner.start_recovery();
        let entries: Vec<Arc<dyn ModuleFactory>> = runner.settings.factories.entries().to_vec();
        for factory in entries {
            let control = factory.register(&runner.registry, &runner.store).expect("register");
            runner.controls.insert(factory.id().as_str().to_owned(), control);
        }
        let _ = runner.registry.start_all();
        runner.publish();
        let view = runner
            .handle
            .status()
            .modules
            .into_iter()
            .find(|module| module.id == "bar")
            .expect("bar is registered");
        assert_eq!(view.state, crate::bindings::ModuleState::Disabled);
        assert!(!view.enabled);
        let bar = view.bar.expect("details");
        assert_eq!(
            bar.state,
            stache_bar::BarRuntimeState::Inactive,
            "nothing was started"
        );
        assert_eq!(
            bar.panel,
            stache_bar::BarPanelState::None,
            "no panel was requested"
        );
        assert!(bar.frame.is_none() && bar.reservation.is_none());
        let _ = runner.registry.shutdown();
    }

    #[test]
    fn the_production_composition_registers_tiling_default_disabled() {
        let factories = production_with_tiling();
        assert!(factories.entries().iter().any(|factory| factory.id() == TilingConfig::ID));
        // Registering over an empty configuration directory: nothing starts.
        let root = tempfile::tempdir().expect("root");
        let (mut runner, _receiver) = runner_over(
            root.path(),
            factories,
            crate::catalog::catalog().expect("catalog"),
            Arc::new(AllowAll),
            None,
        );
        runner.start_recovery();
        let entries: Vec<Arc<dyn ModuleFactory>> = runner.settings.factories.entries().to_vec();
        for factory in entries {
            let control = factory.register(&runner.registry, &runner.store).expect("register");
            runner.controls.insert(factory.id().as_str().to_owned(), control);
        }
        let _ = runner.registry.start_all();
        runner.publish();
        let view = runner
            .handle
            .status()
            .modules
            .into_iter()
            .find(|module| module.id == "tiling")
            .expect("tiling is registered");
        assert_eq!(view.state, crate::bindings::ModuleState::Disabled);
        assert_eq!(
            view.tiling.expect("details").state,
            TilingRuntimeState::Inactive,
            "default disabled: nothing was started"
        );
        let _ = runner.registry.shutdown();
    }

    #[test]
    fn enabling_tiling_over_fakes_reaches_running_and_delivers_displays_first() {
        let mut host = TiHost::new(Some(TI_ON));
        host.start();
        let view = host.view();
        assert_eq!(view.state, crate::bindings::ModuleState::Running);
        assert_eq!(host.display.observers(), 1, "the display feed observes natively");
        assert!(host.display.layout_reads() >= 1);
        let tracking = host.factory.runtime.tracking().expect("running");
        ti_wait("the native display reached the actor", || {
            tracking.snapshot().screens.len() == 1
        });
        let _ = host.runner.registry.shutdown();
        assert_eq!(host.display.observers(), 0, "stop removes the observer");
    }

    #[test]
    fn missing_accessibility_waits_without_acquiring_or_prompting() {
        let (mut host, trust) = TiHost::composed(Some(TI_ON), Vec::new(), false);
        host.start();
        let view = host.view();
        assert_eq!(view.state, crate::bindings::ModuleState::WaitingForPermission);
        assert_eq!(view.tiling.expect("details").state, TilingRuntimeState::Inactive);
        assert!(host.backend.calls().is_empty(), "nothing native was acquired");
        assert_eq!(host.display.observers(), 0);
        assert_eq!(host.display.layout_reads(), 0);
        assert_eq!(trust.requests.load(Ordering::SeqCst), 0, "Tiling never prompts");
        let _ = host.runner.registry.shutdown();
    }

    #[test]
    fn requested_borders_without_a_composed_authority_degrade_borders_only() {
        let mut host = TiHost::new(Some(
            "enabled = true\nanimations = { enabled = true }\nborders = { enabled = true }\n",
        ));
        host.start();
        let view = host.view();
        assert_eq!(
            view.state,
            crate::bindings::ModuleState::Running,
            "not a failed module"
        );
        let details = view.tiling.expect("details");
        assert!(details.healthy);
        // Borders (this host composed no border overlays, so nothing is
        // drawn), the explicit "tabs are not read" capability and, with no shared
        // input service attached to this fake host, the degraded passive drag
        // (only drag, never the module). Animation is implemented (part 08), so a
        // requested animation publishes no unsupported diagnostic.
        assert_eq!(details.diagnostics.len(), 3, "{details:?}");
        assert!(details.diagnostics.iter().any(|d| d.contains("composed no border overlays")));
        assert!(!details.diagnostics.iter().any(|d| d.contains("animation")));
        assert!(details.diagnostics.iter().any(|d| d.contains("native tabs are not read")));
        assert!(details.diagnostics.iter().any(|d| d.contains("drag is unavailable")));
        let json = serde_json::to_value(host.runner.handle.status()).expect("encode");
        let module = json["modules"]
            .as_array()
            .expect("modules")
            .iter()
            .find(|m| m["id"] == "tiling")
            .expect("tiling");
        assert_eq!(module["tiling"]["diagnostics"].as_array().map(Vec::len), Some(3));
        let _ = host.runner.registry.shutdown();
    }

    #[test]
    fn a_stopped_or_quarantined_tiling_leaves_an_independent_module_running() {
        let backend = Arc::new(stache_macos::FakeApplicationBackend::new());
        let executor = Arc::new(stache_macos::ManualExecutor::new(true));
        let control = ApplicationControl::with_acknowledgement_budget(
            Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
            Arc::clone(&backend) as Arc<dyn stache_macos::ApplicationBackend>,
            Duration::from_millis(500),
        );
        let (mut host, _) = TiHost::composed(
            Some(TI_ON),
            vec![Arc::new(NoTunesFactory::new(control)) as Arc<dyn ModuleFactory>],
            true,
        );
        std::fs::write(
            host.config.module_file(stache_notunes::NoTunesConfig::ID),
            "enabled = true\n",
        )
        .expect("write notunes");
        host.start();
        let state_of = |host: &TiHost, id: &str| {
            host.runner
                .handle
                .status()
                .modules
                .into_iter()
                .find(|module| module.id == id)
                .expect("module")
                .state
        };
        assert_eq!(state_of(&host, "tiling"), crate::bindings::ModuleState::Running);
        assert_eq!(state_of(&host, "notunes"), crate::bindings::ModuleState::Running);
        host.backend.script_stop(StopOutcome::Unacknowledged);
        host.write("enabled = false\n");
        host.reload();
        let view = host.view();
        assert_eq!(
            view.tiling.expect("details").state,
            TilingRuntimeState::Quarantined
        );
        assert_eq!(
            state_of(&host, "notunes"),
            crate::bindings::ModuleState::Running,
            "a quarantined Tiling never stops another module"
        );
        let _ = host.runner.registry.shutdown();
    }

    #[test]
    fn held_file_diagnostics_repair_independently_of_cleanup_errors() {
        let mut host = TiHost::new(Some(TI_ON));
        host.start();
        let revision = host.view().revision;
        host.write("enabled = ");
        host.reload();
        let view = host.view();
        assert!(view.diagnostic.is_some(), "the rejected file is held");
        assert_eq!(view.state, crate::bindings::ModuleState::Running);
        assert_eq!(view.revision, revision);
        assert_eq!(
            view.tiling.as_ref().expect("details").state,
            TilingRuntimeState::Running
        );
        host.write("enabled = true\ndefault_layout = \"floating\"\n");
        host.reload();
        let view = host.view();
        assert!(view.diagnostic.is_none());
        assert_ne!(view.revision, revision);
        let adopted = view.revision;
        ti_wait("the repaired file is adopted", || {
            host.runner.publish();
            host.view().tiling.is_some_and(|t| t.config_revision == adopted)
        });

        // A cleanup error is retained by the runtime and independent of the
        // file diagnostic.
        host.backend.script_stop(StopOutcome::Unacknowledged);
        host.write("enabled = false\n");
        host.reload();
        let view = host.view();
        let details = view.tiling.as_ref().expect("details");
        assert_eq!(details.state, TilingRuntimeState::Quarantined);
        assert_eq!(details.owed, ["backend epoch"]);
        host.write("not json");
        host.reload();
        let view = host.view();
        assert!(view.diagnostic.is_some());
        assert_eq!(
            view.tiling.as_ref().expect("details").state,
            TilingRuntimeState::Quarantined,
            "the file diagnostic never hides the cleanup state"
        );
        host.write("enabled = false\n");
        host.reload();
        let view = host.view();
        assert!(
            view.diagnostic.is_none(),
            "a repaired file clears only its own diagnostic"
        );
        let _ = host.runner.registry.shutdown();
    }

    /// The `tiling` socket handler the factory contributes, over the real
    /// registry and store.
    fn ti_handler(host: &TiHost) -> Arc<dyn IpcHandler> {
        let context =
            FactoryContext::new(Arc::clone(&host.runner.registry), Arc::clone(&host.runner.store));
        let mut handlers = host.factory.ipc_handlers(&context);
        assert_eq!(handlers.len(), 1);
        let (name, handler) = handlers.remove(0);
        assert_eq!(name, "tiling");
        handler
    }

    #[test]
    fn the_factory_serves_tiling_methods_over_the_real_registry_lifecycle() {
        use stache_sdk::ipc::HandlerErrorKind;
        let mut host = TiHost::new(Some(TI_ON));
        host.start();
        let handler = ti_handler(&host);
        let error = |method: &str| handler.handle(method, serde_json::json!({"kind": "screens"}));

        // Running: the screens arrive from the native display feed.
        ti_wait("screens are published", || {
            handler
                .handle("query", serde_json::json!({"kind": "screens"}))
                .is_ok_and(|value| value.as_array().is_some_and(|rows| !rows.is_empty()))
        });
        let screens = handler.handle("query", serde_json::json!({"kind": "screens"})).expect("ok");
        assert_eq!(screens[0]["name"], "Display 1");

        // Pause waits for the registry teardown and reports Applied.
        let paused = handler.handle("pause", serde_json::Value::Null).expect("pause");
        assert_eq!(paused["outcome"], "applied");
        assert_eq!(
            host.runner
                .registry
                .snapshots()
                .iter()
                .find(|s| s.id == TilingConfig::ID)
                .map(|s| s.paused),
            Some(true)
        );
        let refused = error("query").expect_err("paused");
        assert_eq!(
            (refused.kind(), refused.message()),
            (HandlerErrorKind::Unavailable, "Tiling is paused")
        );
        host.runner.publish();
        assert_eq!(
            host.view().tiling.expect("details").state,
            TilingRuntimeState::Inactive
        );

        // Resume restarts through the registry.
        let resumed = handler.handle("resume", serde_json::Value::Null).expect("resume");
        assert_eq!(resumed["outcome"], "applied");
        ti_wait("the resumed module serves queries", || error("query").is_ok());

        // A disabled configuration is disabled, and resume cannot override it.
        host.write("enabled = false\n");
        host.reload();
        let disabled = error("query").expect_err("disabled");
        assert_eq!(
            (disabled.kind(), disabled.message()),
            (HandlerErrorKind::Disabled, "Tiling is disabled")
        );
        let resume = handler.handle("resume", serde_json::Value::Null).expect_err("disabled");
        assert_eq!(resume.kind(), HandlerErrorKind::Disabled);

        // Host begin-shutdown cancels every handler wait.
        host.factory.begin_shutdown();
        let closing = handler
            .handle("window", serde_json::json!({"toggleFloating": true}))
            .expect_err("shutting down");
        assert_eq!(closing.message(), "Tiling is shutting down");
        let _ = host.runner.registry.shutdown();
    }

    #[test]
    fn the_factory_shutdown_closes_the_backend() {
        let mut host = TiHost::new(Some(TI_ON));
        host.start();
        let _ = host.runner.registry.shutdown();
        host.factory.begin_shutdown();
        let wait = LateReapWait {
            deadline: Instant::now() + Duration::from_secs(5),
        };
        assert_eq!(host.factory.finish_shutdown(&wait), Ok(true));
        assert!(host.backend.calls().iter().any(|c| matches!(c, FakeBackendCall::Close)));
    }
}

#[cfg(test)]
#[path = "config_documents_e2e.rs"]
mod config_documents_e2e;

#[cfg(test)]
#[path = "popover_actions_tests.rs"]
mod popover_actions_tests;
