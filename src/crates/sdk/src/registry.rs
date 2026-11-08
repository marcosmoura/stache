//! The host-owned module registry.
//!
//! One registry owns every transition. There are no global registries and no
//! mutable statics. Each module has its own lock, so independent modules can be
//! operated on concurrently; the registry-wide map lock is never held while a
//! module method runs. Multi-module operations (start, permission and service
//! recovery) take a separate lifecycle lock so they cannot interleave.
//! Teardown does not: [`Registry::invalidate`] fires logical module
//! invalidation, [`Registry::shutdown_targets`] reads stop metadata, and
//! [`Registry::stop_module`] stops one module without ever taking `lifecycle`.
//! The only nesting is the consumer's module lock while `service_ready` reads the
//! provider's published metadata; it never takes the provider's module lock,
//! calls provider code or joins a worker. Readers never take a module lock, so
//! `snapshot`, `snapshots` and service readiness cannot wait on module code or a
//! worker join.
//!
//! Each slot publishes its metadata into a separately locked cell at every
//! transition. The cell may show the last completed state while a transition is
//! in flight; a stopping module publishes its non-running target before cleanup
//! begins, so dependents never observe a service as ready during shutdown.
//!
//! Lifecycle calls that join workers (`stop`, `shutdown`, disable, pause) run
//! on the caller's thread. The host must call them off the main thread and
//! dispatch UI updates instead.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::config::accepted::{AcceptedCell, AcceptedWatch};
use crate::context::ModuleContext;
use crate::error::{
    ErrorCategory, ModuleError, ModuleResult, Operation, RegistryError, RegistryResult,
};
use crate::events::EventBus;
use crate::generation::{GenerationScope, RuntimeEnd};
use crate::input::{InputCapability, InputService};
use crate::logging::{Logger, ScopedLogger};
use crate::module::{Module, ModuleConfig, ModuleDescriptor, ModuleId, ServiceId};
use crate::permissions::{Permission, PermissionProvider, PermissionRequirement, PermissionStatus};
use crate::published_state::{GenerationBinding, StateCell, StateHub, StateObserver, StateSchemas};
use crate::screen_reservations::{
    DisplayId, ReservationHub, ScreenReservationError, ScreenReservationObserver,
    TopologyGeneration,
};
use crate::shutdown::ShutdownWait;
use crate::status::{
    ConfigDiagnostic, ModuleFailure, ModuleSnapshot, ModuleStatus, UnavailableReason,
};
use crate::threading::{CancellationToken, MainThreadExecutor, WorkerGroup};
use crate::windows::{SurfaceId, WindowBackend, WindowCapability};
use crate::{dependencies, lock};

/// A typed reference to a registered module.
///
/// The configuration type comes from `Module::Config`, so `reload` cannot be
/// given a configuration of the wrong type. Handles are plain identifiers; a
/// stale handle is rejected as [`RegistryError::UnknownModule`].
pub struct ModuleHandle<C> {
    id: ModuleId,
    marker: PhantomData<fn() -> C>,
}

impl<C> ModuleHandle<C> {
    /// The module identifier behind this handle.
    #[must_use]
    pub const fn id(&self) -> ModuleId { self.id }

    /// Builds an unregistered handle for the wrong-type test.
    #[cfg(test)]
    pub(crate) fn for_test(id: ModuleId) -> Self { Self { id, marker: PhantomData } }
}

impl<C> Copy for ModuleHandle<C> {}

impl<C> Clone for ModuleHandle<C> {
    fn clone(&self) -> Self { *self }
}

impl<C> PartialEq for ModuleHandle<C> {
    fn eq(&self, other: &Self) -> bool { self.id == other.id }
}

impl<C> Eq for ModuleHandle<C> {}

impl<C> Hash for ModuleHandle<C> {
    fn hash<H: Hasher>(&self, state: &mut H) { self.id.hash(state); }
}

impl<C> fmt::Debug for ModuleHandle<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModuleHandle").field("id", &self.id).finish_non_exhaustive()
    }
}

/// The result of applying a configuration through [`Registry::reload`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReloadStatus {
    /// The candidate replaced the accepted configuration at this revision.
    Applied {
        /// The committed revision.
        revision: u64,
    },
    /// The candidate matches the accepted configuration; nothing changed.
    Unchanged,
}

/// The full result of [`Registry::reload`].
#[derive(Debug)]
pub struct ReloadReport {
    /// What happened to the configuration.
    pub status: ReloadStatus,
    /// A start failure after the configuration was accepted.
    ///
    /// The configuration remains committed at its revised state; the module
    /// snapshot records `Failed` with the same error.
    pub start_error: Option<ModuleError>,
}

/// One module the coordinator must stop, in reverse start order.
///
/// Targets come from [`Registry::shutdown_targets`] and are handed back to
/// [`Registry::stop_module`]. The sequence is the start order the module was
/// admitted with; a higher sequence stops first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StopTarget {
    /// The module to stop.
    pub id: ModuleId,
    /// The reverse-start sequence assigned when the start was admitted.
    pub sequence: u64,
}

/// Configuration erased once at registration.
trait ErasedConfig: Send + Sync {
    fn enabled(&self) -> bool;
    fn as_any(&self) -> &dyn Any;
    fn equals(&self, other: &dyn ErasedConfig) -> bool;
}

struct ConfigBox<C: ModuleConfig>(C);

impl<C: ModuleConfig> ErasedConfig for ConfigBox<C> {
    fn enabled(&self) -> bool { self.0.enabled() }

    fn as_any(&self) -> &dyn Any { &self.0 }

    fn equals(&self, other: &dyn ErasedConfig) -> bool {
        other.as_any().downcast_ref::<C>().is_some_and(|candidate| candidate == &self.0)
    }
}

/// Module erased once at registration; the only downcast boundary in the SDK.
trait ErasedModule: Send {
    fn configure(&mut self, config: &dyn ErasedConfig, ctx: &ModuleContext) -> ModuleResult<()>;
    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()>;
    fn reconfigure(&mut self, config: &dyn ErasedConfig, ctx: &ModuleContext) -> ModuleResult<()>;
    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()>;
    fn requirements(&self, config: &dyn ErasedConfig) -> Vec<PermissionRequirement>;
}

/// The descriptor's static permissions with automatic prompting.
fn static_requirements(descriptor: &ModuleDescriptor) -> Vec<PermissionRequirement> {
    descriptor
        .permissions
        .iter()
        .map(|&p| PermissionRequirement::automatic(p))
        .collect()
}

/// Merges one requirement per permission: a user-only requirement wins over an
/// automatic one.
fn merge_requirement(merged: &mut Vec<PermissionRequirement>, requirement: PermissionRequirement) {
    match merged.iter_mut().find(|m| m.permission() == requirement.permission()) {
        Some(existing) if existing.may_auto_prompt() && !requirement.may_auto_prompt() => {
            *existing = requirement;
        }
        Some(_) => {}
        None => merged.push(requirement),
    }
}

struct ModuleAdapter<M: Module> {
    module: M,
    id: ModuleId,
}

fn typed_config<C: ModuleConfig>(
    config: &dyn ErasedConfig,
    module: ModuleId,
    operation: Operation,
) -> ModuleResult<&C> {
    config.as_any().downcast_ref::<C>().ok_or_else(|| {
        ModuleError::new(
            module,
            operation,
            ErrorCategory::Lifecycle,
            "registered configuration type does not match the module",
        )
    })
}

impl<M: Module> ErasedModule for ModuleAdapter<M> {
    fn configure(&mut self, config: &dyn ErasedConfig, ctx: &ModuleContext) -> ModuleResult<()> {
        let config = typed_config::<M::Config>(config, self.id, Operation::Configure)?;
        self.module.configure(config, ctx)
    }

    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> { self.module.start(ctx) }

    fn reconfigure(&mut self, config: &dyn ErasedConfig, ctx: &ModuleContext) -> ModuleResult<()> {
        let config = typed_config::<M::Config>(config, self.id, Operation::Reconfigure)?;
        self.module.reconfigure(config, ctx)
    }

    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()> { self.module.stop(ctx) }

    fn requirements(&self, config: &dyn ErasedConfig) -> Vec<PermissionRequirement> {
        config.as_any().downcast_ref::<M::Config>().map_or_else(
            || static_requirements(&self.module.descriptor()),
            |config| self.module.required_permissions(config),
        )
    }
}

/// Workers, windows and cancellation owned by one runtime of one module.
struct Runtime {
    workers: WorkerGroup,
    cancellation: CancellationToken,
    windows: WindowCapability,
    input: InputCapability,
    /// This runtime's generation. Ended before any cleanup wait; every
    /// generation-scoped capability listens on it.
    scope: GenerationScope,
    /// The published-state binding, when the module has a registered schema.
    state: Option<GenerationBinding>,
}

impl Runtime {
    fn new(
        id: ModuleId,
        generation: u64,
        surfaces: &'static [SurfaceId],
        backend: Arc<dyn WindowBackend>,
        input: Arc<InputService>,
    ) -> Self {
        let workers = WorkerGroup::new(id.as_str());
        let cancellation = workers.cancellation();
        let windows = WindowCapability::new(id, surfaces, backend, cancellation.clone());
        let input = InputCapability::new(id, input, cancellation.clone());
        Self {
            workers,
            cancellation,
            windows,
            input,
            scope: GenerationScope::new(generation),
            state: None,
        }
    }

    /// Ends the generation so scoped capabilities reject late work and publish
    /// their tombstones. The first end wins.
    fn end_generation(&self, end: RuntimeEnd) { self.scope.invalidate(end); }

    fn invalidate_windows(&self) {
        self.windows.invalidate();
        self.input.invalidate();
    }

    fn release_windows(&self) {
        self.windows.release_owner();
        self.input.release_owner();
    }
}

/// Logical, lock-free invalidation shared with one module.
///
/// Each module slot owns one handle for its whole lifetime and the registry
/// retains it outside the slot state lock and outside `lifecycle`, so
/// [`Registry::invalidate`] can fire it while a start or stop holds either
/// lock. Firing is logical only: queued native work drops, already-issued work
/// finishes inert.
///
/// A listener runs exactly once, after the listener list lock has been
/// released and never under a registry, slot or lifecycle lock. A panicking
/// listener is contained with [`std::panic::catch_unwind`] so it cannot stop
/// the other listeners or poison the handle; the release profile aborts on
/// panic, so that containment applies only where unwinding is enabled.
#[derive(Clone, Default)]
pub struct ModuleInvalidation {
    state: Arc<InvalidationState>,
}

#[derive(Default)]
struct InvalidationState {
    invalidated: AtomicBool,
    listeners: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

impl ModuleInvalidation {
    /// Creates a handle that has not been invalidated.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Invalidates once. Never takes a registry, slot or lifecycle lock.
    ///
    /// Repeated calls are no-ops. Every listener registered before this call
    /// runs exactly once before it returns.
    pub fn invalidate(&self) {
        if self.state.invalidated.swap(true, Ordering::SeqCst) {
            return;
        }
        let listeners = {
            let mut listeners = lock(&self.state.listeners);
            std::mem::take(&mut *listeners)
        };
        run_listeners(&listeners);
    }

    /// Whether invalidation has been requested.
    #[must_use]
    pub fn is_invalidated(&self) -> bool { self.state.invalidated.load(Ordering::SeqCst) }

    /// Registers a listener, running it immediately when already invalidated.
    ///
    /// The check and the insertion both hold the listener lock, so a concurrent
    /// [`ModuleInvalidation::invalidate`] cannot miss this registration. A
    /// listener registered after invalidation is invoked exactly once, before
    /// this call returns.
    pub fn on_invalidate(&self, listener: Arc<dyn Fn() + Send + Sync>) {
        {
            let mut listeners = lock(&self.state.listeners);
            if !self.state.invalidated.load(Ordering::SeqCst) {
                listeners.push(listener);
                return;
            }
        }
        run_listener(&listener);
    }
}

fn run_listeners(listeners: &[Arc<dyn Fn() + Send + Sync>]) {
    for listener in listeners {
        run_listener(listener);
    }
}

fn run_listener(listener: &Arc<dyn Fn() + Send + Sync>) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener()));
}

impl fmt::Debug for ModuleInvalidation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModuleInvalidation")
            .field("invalidated", &self.is_invalidated())
            .finish_non_exhaustive()
    }
}

struct ModuleSlot {
    descriptor: ModuleDescriptor,
    state: Mutex<ModuleState>,
    /// Metadata published for readers.
    ///
    /// This lock never guards module code. Readers and service readiness hold
    /// it only long enough to clone the current snapshot.
    published: Mutex<ModuleSnapshot>,
    /// Stop metadata readable without the slot state lock.
    ///
    /// Teardown snapshots targets from this cell, so a blocked start that holds
    /// the state lock cannot delay it. The cell is only ever mutated for a few
    /// instructions and never held across module code.
    meta: Mutex<SlotMeta>,
    /// Logical invalidation shared with every runtime of this module.
    invalidation: ModuleInvalidation,
    /// The last accepted configuration pair, readable without any other lock.
    accepted: Arc<AcceptedCell>,
    /// Permission requirements of the accepted configuration, readable by the
    /// host without the state lock.
    requirements: Mutex<Vec<PermissionRequirement>>,
    /// The provider's published-state cell, present when a schema is registered.
    state_cell: Option<Arc<StateCell>>,
}

/// Stop metadata for one slot, kept outside the slot state lock.
#[derive(Clone, Copy, Debug, Default)]
struct SlotMeta {
    /// The start sequence assigned when the start was admitted.
    sequence: Option<u64>,
    /// Set from admission of a start until a successful stop commits.
    needs_stop: bool,
}

impl ModuleSlot {
    /// Returns the last published snapshot without touching the module lock.
    fn published_snapshot(&self) -> ModuleSnapshot { lock(&self.published).clone() }

    /// Publishes the state's metadata. The caller holds the module lock; no
    /// module or native code runs while the short published lock is held.
    fn publish(&self, state: &ModuleState) {
        *lock(&self.published) = state.snapshot(self.descriptor.id);
        lock(&self.requirements).clone_from(&state.requirements);
        if let Some(cell) = &self.state_cell {
            cell.note_status(status_end(&state.status));
        }
    }

    /// Locks the module state and republishes it when the guard drops.
    fn lock_state(&self) -> SlotState<'_> {
        SlotState {
            slot: self,
            state: lock(&self.state),
        }
    }
}

/// Module state guard that publishes the slot's snapshot on drop.
///
/// Operations lock the module state for their whole transition; publishing on
/// drop keeps the reader-visible cell correct on every early return without a
/// manual update at each exit. Callers publish explicitly before a blocking
/// stop or cleanup so dependents stop seeing a running service first.
struct SlotState<'a> {
    slot: &'a ModuleSlot,
    state: MutexGuard<'a, ModuleState>,
}

impl SlotState<'_> {
    /// Publishes the current state before a blocking stop or cleanup begins.
    fn publish(&self) { self.slot.publish(&self.state); }
}

impl Deref for SlotState<'_> {
    type Target = ModuleState;

    fn deref(&self) -> &Self::Target { &self.state }
}

impl DerefMut for SlotState<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.state }
}

impl Drop for SlotState<'_> {
    fn drop(&mut self) { self.slot.publish(&self.state); }
}

/// The initial state computed by a registration entry point.
struct Registration {
    config: Option<Box<dyn ErasedConfig>>,
    published: Option<Arc<dyn Any + Send + Sync>>,
    enabled: bool,
    revision: u64,
    status: ModuleStatus,
    diagnostic: Option<ConfigDiagnostic>,
}

impl Registration {
    fn configured(
        config: Box<dyn ErasedConfig>,
        published: Arc<dyn Any + Send + Sync>,
        enabled: bool,
    ) -> Self {
        Self {
            config: Some(config),
            published: Some(published),
            enabled,
            revision: 1,
            status: if enabled {
                ModuleStatus::Stopped
            } else {
                ModuleStatus::Disabled
            },
            diagnostic: None,
        }
    }

    fn rejected(enabled: bool, diagnostic: ConfigDiagnostic) -> Self {
        Self {
            config: None,
            published: None,
            enabled,
            revision: 0,
            status: ModuleStatus::Stopped,
            diagnostic: Some(diagnostic),
        }
    }
}

struct ModuleState {
    module: Box<dyn ErasedModule>,
    config: Option<Box<dyn ErasedConfig>>,
    enabled: bool,
    paused: bool,
    revision: u64,
    status: ModuleStatus,
    diagnostic: Option<ConfigDiagnostic>,
    runtime: Option<Runtime>,
    needs_stop: bool,
    /// Permissions required by the accepted configuration (the static
    /// descriptor list while none was accepted).
    requirements: Vec<PermissionRequirement>,
}

impl ModuleState {
    fn snapshot(&self, id: ModuleId) -> ModuleSnapshot {
        ModuleSnapshot {
            id,
            status: self.status.clone(),
            enabled: self.enabled,
            paused: self.paused,
            revision: self.revision,
            diagnostic: self.diagnostic.clone(),
        }
    }
}

/// The host-owned registry of modules.
///
/// Registration happens during composition. Runtime operations are safe to call
/// from several threads; each module serializes its own transitions.
pub struct Registry {
    logger: Arc<dyn Logger>,
    events: Arc<EventBus>,
    main_thread: Arc<dyn MainThreadExecutor>,
    permissions: Arc<dyn PermissionProvider>,
    windows: Arc<dyn WindowBackend>,
    input: Arc<InputService>,
    /// Host-level requirements merged into every module; immutable once the
    /// first module is registered.
    baseline: Vec<PermissionRequirement>,
    slots: Mutex<Vec<Arc<ModuleSlot>>>,
    failed_services: Mutex<HashSet<ServiceId>>,
    lifecycle: Mutex<()>,
    shutting_down: AtomicBool,
    started: AtomicBool,
    next_sequence: AtomicU64,
    next_generation: AtomicU64,
    states: Arc<StateHub>,
    reservations: Arc<ReservationHub>,
}

impl Registry {
    /// Creates an empty registry with injected host services.
    #[must_use]
    pub fn new(
        logger: Arc<dyn Logger>,
        events: Arc<EventBus>,
        main_thread: Arc<dyn MainThreadExecutor>,
        permissions: Arc<dyn PermissionProvider>,
        windows: Arc<dyn WindowBackend>,
    ) -> Self {
        Self {
            logger,
            events,
            main_thread,
            permissions,
            windows,
            input: InputService::unavailable(),
            baseline: Vec::new(),
            slots: Mutex::new(Vec::new()),
            failed_services: Mutex::new(HashSet::new()),
            lifecycle: Mutex::new(()),
            shutting_down: AtomicBool::new(false),
            started: AtomicBool::new(false),
            next_sequence: AtomicU64::new(0),
            next_generation: AtomicU64::new(0),
            states: Arc::new(StateHub::default()),
            reservations: Arc::new(ReservationHub::default()),
        }
    }

    /// Attaches the host's shared input service. Without it, input
    /// subscriptions return an `Unavailable` error and no native owner is called.
    #[must_use]
    pub fn with_input_service(mut self, input: Arc<InputService>) -> Self {
        self.input = input;
        self
    }

    /// Registers the published-state contracts of provider modules.
    ///
    /// Like the baseline, schemas are immutable once the first module is
    /// registered, so this consuming builder must run before the first
    /// `register`. The application builds the set from the owning crates' types;
    /// the SDK never imports them.
    ///
    /// # Errors
    /// Returns [`RegistryError::StateSchemaAfterRegistration`] when a module is
    /// already registered and [`RegistryError::DuplicateStateSchema`] when a
    /// provider would get a second schema.
    pub fn with_state_schemas(self, schemas: &StateSchemas) -> RegistryResult<Self> {
        if !lock(&self.slots).is_empty() {
            return Err(RegistryError::StateSchemaAfterRegistration);
        }
        for schema in schemas.entries() {
            if self.states.register(*schema).is_err() {
                return Err(RegistryError::DuplicateStateSchema { provider: schema.provider() });
            }
        }
        Ok(self)
    }

    /// Registers the published-state contract `T` for `provider`.
    ///
    /// # Errors
    /// The same errors as [`Registry::with_state_schemas`].
    pub fn with_state_schema<T: crate::published_state::PublishedState>(
        self,
        provider: ModuleId,
    ) -> RegistryResult<Self> {
        self.with_state_schemas(&StateSchemas::new().with::<T>(provider))
    }

    /// A read-only observer of one provider's published state.
    ///
    /// An unknown provider, or one without a registered schema, reads
    /// [`crate::StateReading::Absent`]; this never fails and takes no module,
    /// lifecycle or reload lock. Hosts use it for app-level reads; modules use
    /// [`ModuleContext::observe_state`] and declare `observes`.
    #[must_use]
    pub fn observe_state(&self, provider: ModuleId) -> StateObserver {
        StateObserver::new(self.states.cell(provider))
    }

    /// A read-only observer of the aggregated screen reservations.
    ///
    /// Subscribe before fetch: `read()` the snapshot, then `wait_for_change`
    /// with its revision. Takes no module or lifecycle lock.
    #[must_use]
    pub fn observe_screen_reservations(&self) -> ScreenReservationObserver {
        ScreenReservationObserver::new(Arc::clone(&self.reservations))
    }

    /// Applies a new display topology reported by the host.
    ///
    /// Reservations of displays the topology omits are dropped and never
    /// resurrected if the numeric ID returns. Returns the new snapshot revision.
    ///
    /// # Errors
    /// Returns [`ScreenReservationError::NonMonotonicTopology`] unless
    /// `generation` is newer than the current one and
    /// [`ScreenReservationError::NullDisplay`] for display `0`.
    pub fn apply_screen_topology(
        &self,
        generation: TopologyGeneration,
        displays: &[DisplayId],
    ) -> Result<u64, ScreenReservationError> {
        self.reservations.apply_topology(generation, displays)
    }

    /// Sets host-level permission requirements merged into every module.
    ///
    /// The baseline applies to every registered module whatever its own
    /// configuration, and `permission_requirements` reports it even when no
    /// module is enabled. Merging follows the per-permission rule: user-only
    /// wins over automatic. The default baseline is empty. It is immutable after
    /// construction, so this consuming builder must run before the first
    /// `register`.
    ///
    /// # Errors
    /// Returns [`RegistryError::BaselineAfterRegistration`] when a module is
    /// already registered.
    pub fn with_baseline_requirements(
        mut self,
        baseline: Vec<PermissionRequirement>,
    ) -> RegistryResult<Self> {
        if !lock(&self.slots).is_empty() {
            return Err(RegistryError::BaselineAfterRegistration);
        }
        let mut merged = Vec::new();
        for requirement in baseline {
            merge_requirement(&mut merged, requirement);
        }
        self.baseline = merged;
        Ok(self)
    }

    /// A module's requirements merged with the baseline.
    fn effective_requirements(
        &self,
        module: Vec<PermissionRequirement>,
    ) -> Vec<PermissionRequirement> {
        let mut merged = Vec::new();
        for requirement in module.into_iter().chain(self.baseline.iter().copied()) {
            merge_requirement(&mut merged, requirement);
        }
        merged
    }

    /// Registers one module and its initial configuration.
    ///
    /// The configuration is validated immediately. An invalid configuration
    /// registers the module as `Stopped` with a diagnostic; it never becomes
    /// `Disabled` because of a validation failure. Duplicate module IDs,
    /// duplicate service providers, self-dependencies and cycles are rejected.
    ///
    /// # Errors
    /// Returns a typed registry error for a duplicate or conflicting declaration.
    pub fn register<M: Module>(
        &self,
        module: M,
        config: M::Config,
    ) -> RegistryResult<ModuleHandle<M::Config>> {
        let enabled = config.enabled();
        let initial = match config.validate() {
            Ok(()) => {
                let published: Arc<dyn Any + Send + Sync> = Arc::new(config.clone());
                Registration::configured(Box::new(ConfigBox(config)), published, enabled)
            }
            Err(error) => Registration::rejected(enabled, ConfigDiagnostic::from_error(&error)),
        };
        self.register_module(module, initial)
    }

    /// Registers a module with no accepted configuration.
    ///
    /// The snapshot is `Stopped` with revision `0` and the supplied diagnostic.
    /// A later valid [`Registry::reload`] commits revision `1`, after which the
    /// host can start the module normally. Use this when the first
    /// configuration file exists but cannot be decoded.
    ///
    /// # Errors
    /// Returns a typed registry error for a duplicate or conflicting declaration.
    pub fn register_rejected<M: Module>(
        &self,
        module: M,
        diagnostic: ConfigDiagnostic,
    ) -> RegistryResult<ModuleHandle<M::Config>> {
        self.register_module(module, Registration::rejected(false, diagnostic))
    }

    fn register_module<M: Module>(
        &self,
        module: M,
        initial: Registration,
    ) -> RegistryResult<ModuleHandle<M::Config>> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let descriptor = module.descriptor();
        let id = descriptor.id;
        let mut slots = lock(&self.slots);
        if slots.iter().any(|slot| slot.descriptor.id == id) {
            return Err(RegistryError::DuplicateModule { id });
        }
        let mut descriptors: Vec<ModuleDescriptor> =
            slots.iter().map(|slot| slot.descriptor).collect();
        descriptors.push(descriptor);
        dependencies::check_registration(&descriptors)?;
        let Registration {
            config,
            published: accepted_config,
            enabled,
            revision,
            status,
            diagnostic,
        } = initial;
        let adapter = ModuleAdapter { module, id };
        let requirements = self.effective_requirements(config.as_ref().map_or_else(
            || static_requirements(&descriptor),
            |config| adapter.requirements(config.as_ref()),
        ));
        let state = ModuleState {
            module: Box::new(adapter),
            config,
            enabled,
            paused: false,
            revision,
            status,
            diagnostic,
            runtime: None,
            needs_stop: false,
            requirements,
        };
        let published = Mutex::new(state.snapshot(id));
        let requirements = Mutex::new(state.requirements.clone());
        let state_cell = self.states.schema_cell(id);
        if let Some(cell) = &state_cell {
            cell.mark_registered();
            cell.note_status(status_end(&state.status));
        }
        let accepted = Arc::new(AcceptedCell::new(
            std::any::TypeId::of::<M::Config>(),
            std::any::type_name::<M::Config>(),
        ));
        if let Some(accepted_config) = accepted_config {
            accepted.publish(revision, accepted_config);
        }
        let slot = Arc::new(ModuleSlot {
            descriptor,
            state: Mutex::new(state),
            published,
            meta: Mutex::new(SlotMeta::default()),
            invalidation: ModuleInvalidation::new(),
            accepted: Arc::clone(&accepted),
            requirements,
            state_cell,
        });
        {
            let cell = Arc::clone(&accepted);
            slot.invalidation.on_invalidate(Arc::new(move || cell.close()));
        }
        slots.push(Arc::clone(&slot));
        // A shutdown that started while this registration held the slots lock
        // must not leave a module behind: remove it and invalidate its handle
        // before returning, so no native work can be issued for it.
        if self.shutting_down.load(Ordering::SeqCst) {
            slots.retain(|existing| !Arc::ptr_eq(existing, &slot));
            drop(slots);
            slot.invalidation.invalidate();
            return Err(RegistryError::ShuttingDown);
        }
        Ok(ModuleHandle { id, marker: PhantomData })
    }

    /// Starts every enabled, non-paused module in dependency order.
    ///
    /// Modules blocked by a permission or a missing service are recorded in
    /// their own status; independent modules still start. A module failure does
    /// not stop the pass, but the first failure is returned after all modules
    /// were attempted.
    ///
    /// # Errors
    /// Returns a declaration error before starting anything, or the first module
    /// failure after attempting every module.
    pub fn start_all(&self) -> RegistryResult<()> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let _lifecycle = lock(&self.lifecycle);
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let slots = self.slots_snapshot();
        let descriptors: Vec<ModuleDescriptor> = slots.iter().map(|slot| slot.descriptor).collect();
        let order = dependencies::order(&descriptors)?;
        self.started.store(true, Ordering::SeqCst);
        let mut first_error = None;
        for id in order {
            let Some(slot) = slot_in(&slots, id) else {
                continue;
            };
            match self.start_slot(slot) {
                Ok(()) => {}
                Err(RegistryError::ShuttingDown) => return Err(RegistryError::ShuttingDown),
                Err(error) => keep_first(&mut first_error, error),
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Sets the desired enabled flag.
    ///
    /// Disabling stops resources and clears no configuration. Enabling clears a
    /// temporary pause and attempts to start the module when it is not paused.
    /// Setting the flag it already has is a no-op, except that enabling an
    /// already-enabled stopped module retries the start.
    ///
    /// # Errors
    /// Returns an unknown-handle, shutdown or module error.
    pub fn set_enabled<C>(&self, handle: ModuleHandle<C>, enabled: bool) -> RegistryResult<()> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let slot = self.slot(handle)?;
        let result = self.set_enabled_inner(&slot, enabled);
        self.reconcile_after(&slot, result)
    }

    fn set_enabled_inner(&self, slot: &ModuleSlot, enabled: bool) -> RegistryResult<()> {
        {
            let mut guard = slot.lock_state();
            // Commitment recheck: a shutdown that raced the entry check must
            // not admit a transition, including an already-satisfied change.
            if self.shutting_down.load(Ordering::SeqCst) {
                return Err(RegistryError::ShuttingDown);
            }
            if guard.enabled == enabled {
                if !enabled && guard.needs_stop {
                    // A previous stop failed, so this repeat retries its cleanup.
                    guard.status = ModuleStatus::Disabled;
                    guard.publish();
                    self.quiesce(slot, &mut guard)?;
                    return Ok(());
                }
                if !enabled || guard.paused {
                    return Ok(());
                }
                drop(guard);
                return self.start_slot(slot);
            }
            guard.enabled = enabled;
            if !enabled {
                guard.status = ModuleStatus::Disabled;
                guard.publish();
                self.quiesce(slot, &mut guard)?;
                return Ok(());
            }
            guard.paused = false;
        }
        self.start_slot(slot)
    }

    /// Pauses a module without editing its configuration.
    ///
    /// A running module is stopped and marked `Paused`. A disabled module keeps
    /// the pause flag for the next enable. Pausing twice is a no-op unless the
    /// first stop failed, in which case the repeat retries that cleanup.
    ///
    /// # Errors
    /// Returns an unknown-handle or stop error.
    pub fn pause<C>(&self, handle: ModuleHandle<C>) -> RegistryResult<()> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let slot = self.slot(handle)?;
        let result = self.pause_inner(&slot);
        self.reconcile_after(&slot, result)
    }

    fn pause_inner(&self, slot: &ModuleSlot) -> RegistryResult<()> {
        let mut guard = slot.lock_state();
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        if guard.paused && !(guard.enabled && guard.needs_stop) {
            return Ok(());
        }
        guard.paused = true;
        if !guard.enabled {
            return Ok(());
        }
        guard.status = ModuleStatus::Paused;
        guard.publish();
        self.quiesce(slot, &mut guard)?;
        Ok(())
    }

    /// Clears a pause and retries the start.
    ///
    /// Resuming a disabled module only clears the flag; it stays disabled.
    ///
    /// # Errors
    /// Returns an unknown-handle or module error.
    pub fn resume<C>(&self, handle: ModuleHandle<C>) -> RegistryResult<()> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let slot = self.slot(handle)?;
        let result = self.resume_inner(&slot);
        self.reconcile_after(&slot, result)
    }

    fn resume_inner(&self, slot: &ModuleSlot) -> RegistryResult<()> {
        {
            let mut guard = slot.lock_state();
            if self.shutting_down.load(Ordering::SeqCst) {
                return Err(RegistryError::ShuttingDown);
            }
            if !guard.paused {
                return Ok(());
            }
            guard.paused = false;
            if !guard.enabled {
                guard.status = ModuleStatus::Disabled;
                return Ok(());
            }
        }
        self.start_slot(slot)
    }

    /// Validates and applies a new configuration.
    ///
    /// Validation failures are recorded as a diagnostic and leave the status,
    /// configuration and revision untouched. A running module receives
    /// `reconfigure`; a stopped, paused or disabled module only updates its
    /// next-start snapshot. An unchanged configuration is a no-op and reports
    /// [`ReloadStatus::Unchanged`]. If the new configuration disables the
    /// module, resources are stopped afterwards.
    ///
    /// Once [`Registry::start_all`] has run, an accepted reload that enables a
    /// stopped, disabled or failed module starts it. Permission and service
    /// blocks, pause, and already-running resources are left alone. A start
    /// failure is returned in [`ReloadReport::start_error`] while the accepted
    /// configuration stays committed.
    ///
    /// # Errors
    /// Returns an unknown-handle, invalid-config or module error.
    pub fn reload<C: ModuleConfig>(
        &self,
        handle: ModuleHandle<C>,
        config: C,
    ) -> RegistryResult<ReloadReport> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let slot = self.slot(handle)?;
        let status = match self.reload_inner(&slot, config) {
            Ok(status) => status,
            Err(error) => return self.reconcile_after(&slot, Err(error)),
        };
        let start_error = match self.start_after_reload(&slot) {
            Ok(()) => None,
            Err(RegistryError::Module(error)) => Some(error),
            Err(error) => return Err(error),
        };
        let reconciled = self.reconcile_after(&slot, Ok(()));
        if start_error.is_some() {
            // The caller already receives the start error; a reconciliation
            // failure must not disappear, so report it through the injected
            // logger with the reloading module and operation.
            if let Err(error) = reconciled {
                ScopedLogger::new(Arc::clone(&self.logger), slot.descriptor.id, Operation::Reload)
                    .warn(format!("reconciliation after reload failed: {error}"));
            }
            Ok(ReloadReport { status, start_error })
        } else {
            reconciled.map(|()| ReloadReport { status, start_error: None })
        }
    }

    /// Starts a module after an accepted reload once the registry has started.
    ///
    /// Paused modules, permission/service blocks and running resources are left
    /// untouched. Failed modules are retried so a changed configuration can
    /// recover them.
    fn start_after_reload(&self, slot: &ModuleSlot) -> RegistryResult<()> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        if !self.started.load(Ordering::SeqCst) {
            return Ok(());
        }
        let eligible = {
            let state = lock(&slot.state);
            state.enabled
                && !state.paused
                && state.config.is_some()
                && matches!(
                    state.status,
                    ModuleStatus::Stopped | ModuleStatus::Disabled | ModuleStatus::Failed(_)
                )
        };
        if eligible {
            self.start_slot(slot)
        } else {
            Ok(())
        }
    }

    fn reload_inner<C: ModuleConfig>(
        &self,
        slot: &ModuleSlot,
        config: C,
    ) -> RegistryResult<ReloadStatus> {
        if let Err(error) = config.validate() {
            {
                let mut guard = slot.lock_state();
                guard.diagnostic = Some(ConfigDiagnostic::from_error(&error));
            }
            return Err(RegistryError::InvalidConfig {
                module: slot.descriptor.id,
                message: error.message().to_owned(),
            });
        }
        let published: Arc<dyn Any + Send + Sync> = Arc::new(config.clone());
        let candidate: Box<dyn ErasedConfig> = Box::new(ConfigBox(config));
        let enabled = candidate.enabled();
        let mut guard = slot.lock_state();
        // Commitment recheck: shutdown may have started while the candidate was
        // validated, so refuse before mutating the diagnostic, config or state.
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let state = &mut *guard;
        state.diagnostic = None;
        let unchanged =
            state.config.as_ref().is_some_and(|current| current.equals(candidate.as_ref()));
        if unchanged && state.enabled == enabled {
            return Ok(ReloadStatus::Unchanged);
        }
        let changed = !unchanged;
        let previous_status = state.status.clone();
        let was_running = previous_status == ModuleStatus::Running;
        let candidate_requirements =
            self.effective_requirements(state.module.requirements(candidate.as_ref()));
        // A running module whose candidate newly needs a permission that is not
        // granted must not adopt it: the whole runtime stops and waits, and the
        // candidate is still accepted. Fresh, nonprompting status only.
        let blocked = if !unchanged && enabled && was_running && !state.paused {
            self.missing_permission(&candidate_requirements)
        } else {
            None
        };
        if !unchanged {
            // A disabling reload only stops; it never asks the module to adopt a
            // configuration it is about to release.
            if enabled {
                if was_running {
                    self.reconfigure_running(slot, state, candidate.as_ref(), blocked.is_some())?;
                } else if state.needs_stop {
                    self.quiesce(slot, state)?;
                }
            }
            state.config = Some(candidate);
            state.requirements = candidate_requirements;
            state.revision += 1;
            // Publish the coherent pair before any later stop work, so a
            // configuration committed despite a start or stop error is visible
            // and readers never wait on module or native cleanup.
            // The published-state cell learns the revision first, so no
            // envelope can carry a revision that has not been accepted. A
            // provider that labels payloads only with revisions read from the
            // watch never runs ahead of it; the cell may briefly accept a
            // revision the watch does not show yet.
            if let Some(cell) = &slot.state_cell {
                cell.advance_accepted(state.revision);
            }
            slot.accepted.publish(state.revision, published);
        }
        if !state.enabled && enabled {
            state.paused = false;
        }
        state.enabled = enabled;
        if !enabled {
            state.status = ModuleStatus::Disabled;
            slot.publish(state);
            self.quiesce(slot, state)?;
        } else if state.paused {
            state.status = ModuleStatus::Paused;
        } else if let Some(permission) = blocked {
            state.status = ModuleStatus::WaitingForPermission(permission);
            slot.publish(state);
            self.quiesce(slot, state)?;
        } else if was_running {
            state.status = ModuleStatus::Running;
        } else if matches!(
            previous_status,
            ModuleStatus::WaitingForPermission(_) | ModuleStatus::Unavailable(_)
        ) {
            // A reload stages the configuration but does not clear a prerequisite
            // block; the matching recovery event still starts the module.
            // A permission block whose permission the accepted configuration no
            // longer requires is released so the start below re-evaluates.
            state.status = match previous_status {
                ModuleStatus::WaitingForPermission(permission)
                    if !state.requirements.iter().any(|r| r.permission() == permission) =>
                {
                    ModuleStatus::Stopped
                }
                other => other,
            };
        } else {
            state.status = ModuleStatus::Stopped;
        }
        Ok(if changed {
            ReloadStatus::Applied { revision: state.revision }
        } else {
            ReloadStatus::Unchanged
        })
    }

    /// Applies a candidate to the running module. A failure changes nothing:
    /// published state carries provider-labelled revisions and has no hold.
    fn reconfigure_running(
        &self,
        slot: &ModuleSlot,
        state: &mut ModuleState,
        candidate: &dyn ErasedConfig,
        blocked: bool,
    ) -> RegistryResult<()> {
        let Some(runtime) = state.runtime.as_ref() else {
            return Err(RegistryError::Module(ModuleError::new(
                slot.descriptor.id,
                Operation::Reconfigure,
                ErrorCategory::Lifecycle,
                "running module has no runtime",
            )));
        };
        let ctx = self.context(slot, runtime, Operation::Reconfigure);
        if blocked {
            return Ok(());
        }
        state.module.reconfigure(candidate, &ctx)?;
        Ok(())
    }

    /// Returns a typed view over this module's accepted configuration.
    ///
    /// The watch reads the last published `(revision, configuration)` pair
    /// without taking a module, lifecycle or reload lock, calling module code or
    /// reading disk. `read` returns `None` while no configuration was ever
    /// accepted, and `wait_for_change` supplies a bounded, cancellable wait.
    ///
    /// The handle's configuration type is checked against the type the module
    /// registered. Handles only carry an identifier, so a handle from another
    /// registry with the same identifier is rejected instead of exposing a
    /// different type's configuration.
    ///
    /// # Errors
    /// Returns [`RegistryError::UnknownModule`] for a stale handle, or
    /// [`RegistryError::ConfigTypeMismatch`] when the handle's type differs from
    /// the registered module's configuration type.
    pub fn accepted_watch<C: ModuleConfig>(
        &self,
        handle: ModuleHandle<C>,
    ) -> RegistryResult<AcceptedWatch<C>> {
        let slot = self.slot(handle)?;
        AcceptedCell::watch(&slot.accepted).map_err(|mismatch| RegistryError::ConfigTypeMismatch {
            id: slot.descriptor.id,
            expected: mismatch.expected,
            registered: mismatch.registered,
        })
    }

    /// Returns the current snapshot of one module.
    ///
    /// The snapshot is the last published transition metadata; it never waits
    /// for the module's lock, a lifecycle call or a worker join.
    ///
    /// # Errors
    /// Returns [`RegistryError::UnknownModule`] for a stale handle.
    pub fn snapshot<C>(&self, handle: ModuleHandle<C>) -> RegistryResult<ModuleSnapshot> {
        let slot = self.slot(handle)?;
        Ok(slot.published_snapshot())
    }

    /// Returns snapshots of every registered module in registration order.
    ///
    /// Like [`Registry::snapshot`], this reads only published metadata.
    #[must_use]
    pub fn snapshots(&self) -> Vec<ModuleSnapshot> {
        self.slots_snapshot().iter().map(|slot| slot.published_snapshot()).collect()
    }

    /// The declared metadata of every registered module, in registration order.
    ///
    /// This is the same static data returned by `Module::descriptor`; reading it
    /// starts nothing and takes no module lock.
    #[must_use]
    pub fn descriptors(&self) -> Vec<ModuleDescriptor> {
        self.slots_snapshot().iter().map(|slot| slot.descriptor).collect()
    }

    /// Permissions needed right now: the baseline, always, merged with the
    /// requirements of enabled, non-paused modules.
    ///
    /// Module requirements come from each module's accepted configuration and
    /// are merged per permission: one user-only requirement makes the merged
    /// requirement user-only. The baseline is reported even with no registered,
    /// enabled or unpaused module. Reads published data only and never prompts.
    #[must_use]
    pub fn permission_requirements(&self) -> Vec<PermissionRequirement> {
        let mut merged: Vec<PermissionRequirement> = Vec::new();
        for &requirement in &self.baseline {
            merge_requirement(&mut merged, requirement);
        }
        for slot in self.slots_snapshot() {
            let snapshot = slot.published_snapshot();
            if !snapshot.enabled || snapshot.paused {
                continue;
            }
            for &requirement in lock(&slot.requirements).iter() {
                merge_requirement(&mut merged, requirement);
            }
        }
        merged
    }

    /// Reports that a service stopped working.
    ///
    /// Recording the failure is enough: reconciliation stops running dependents
    /// and marks them `Unavailable`. The provider itself is not changed.
    ///
    /// # Errors
    /// Returns a declaration or stop error.
    pub fn service_failed(&self, service: ServiceId) -> RegistryResult<()> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let _lifecycle = lock(&self.lifecycle);
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        lock(&self.failed_services).insert(service);
        self.reconcile_locked()
    }

    /// Reports that a service is available again.
    ///
    /// Enabled, non-paused modules waiting on a missing service are retried in
    /// dependency order.
    ///
    /// # Errors
    /// Returns a declaration or module error.
    pub fn service_recovered(&self, service: ServiceId) -> RegistryResult<()> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let _lifecycle = lock(&self.lifecycle);
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        lock(&self.failed_services).remove(&service);
        self.reconcile_locked()
    }

    /// Reconciles service dependents after a provider changes state.
    fn reconcile(&self) -> RegistryResult<()> {
        let _lifecycle = lock(&self.lifecycle);
        self.reconcile_locked()
    }

    /// The reconciliation pass. The caller holds the lifecycle lock and no
    /// module lock.
    fn reconcile_locked(&self) -> RegistryResult<()> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let slots = self.slots_snapshot();
        let descriptors: Vec<ModuleDescriptor> = slots.iter().map(|slot| slot.descriptor).collect();
        let order = dependencies::order(&descriptors)?;
        let mut unavailable: HashSet<ServiceId> = lock(&self.failed_services).clone();
        for slot in &slots {
            if lock(&slot.state).status != ModuleStatus::Running {
                unavailable.extend(slot.descriptor.provides.iter().copied());
            }
        }
        let mut stopped: HashMap<ModuleId, ServiceId> = HashMap::new();
        for id in &order {
            let Some(slot) = slot_in(&slots, *id) else {
                continue;
            };
            let missing = slot
                .descriptor
                .requires
                .iter()
                .copied()
                .find(|service| unavailable.contains(service));
            if let Some(service) = missing {
                stopped.insert(*id, service);
                unavailable.extend(slot.descriptor.provides.iter().copied());
            }
        }
        let mut first_error = None;
        for id in order.iter().rev() {
            let Some(&service) = stopped.get(id) else {
                continue;
            };
            let Some(slot) = slot_in(&slots, *id) else {
                continue;
            };
            let mut guard = slot.lock_state();
            if guard.status != ModuleStatus::Running {
                continue;
            }
            guard.status = ModuleStatus::Unavailable(UnavailableReason::MissingService(service));
            guard.publish();
            if let Err(error) = self.quiesce(slot, &mut guard) {
                keep_first(&mut first_error, error);
            }
        }
        for id in &order {
            let Some(slot) = slot_in(&slots, *id) else {
                continue;
            };
            let retry = {
                let state = lock(&slot.state);
                state.enabled
                    && !state.paused
                    && matches!(
                        state.status,
                        ModuleStatus::Unavailable(UnavailableReason::MissingService(_))
                    )
            };
            if !retry {
                continue;
            }
            match self.start_slot(slot) {
                Ok(()) => {}
                Err(RegistryError::ShuttingDown) => return Err(RegistryError::ShuttingDown),
                Err(error) => keep_first(&mut first_error, error),
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Runs reconciliation only when the module can affect dependents.
    fn reconcile_after<T>(
        &self,
        slot: &ModuleSlot,
        result: RegistryResult<T>,
    ) -> RegistryResult<T> {
        if slot.descriptor.provides.is_empty() {
            return result;
        }
        let reconciled = self.reconcile();
        match result {
            Ok(value) => reconciled.map(|()| value),
            Err(error) => Err(error),
        }
    }

    /// Applies a new permission status.
    ///
    /// A grant retries enabled, non-paused modules waiting for that permission.
    /// A denial or unknown status stops running modules that declare it, in
    /// reverse dependency order, and marks them waiting. Service dependents of
    /// affected providers are reconciled afterwards.
    ///
    /// # Errors
    /// Returns a declaration or module error.
    pub fn on_permission_change(
        &self,
        permission: Permission,
        status: PermissionStatus,
    ) -> RegistryResult<()> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let _lifecycle = lock(&self.lifecycle);
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let slots = self.slots_snapshot();
        let descriptors: Vec<ModuleDescriptor> = slots.iter().map(|slot| slot.descriptor).collect();
        let order = dependencies::order(&descriptors)?;
        let mut first_error = None;
        match status {
            PermissionStatus::Granted => {
                for id in order {
                    let Some(slot) = slot_in(&slots, id) else {
                        continue;
                    };
                    let waiting = {
                        let state = lock(&slot.state);
                        state.enabled
                            && !state.paused
                            && state.status == ModuleStatus::WaitingForPermission(permission)
                    };
                    if !waiting {
                        continue;
                    }
                    match self.start_slot(slot) {
                        Ok(()) => {}
                        Err(RegistryError::ShuttingDown) => {
                            return Err(RegistryError::ShuttingDown);
                        }
                        Err(error) => keep_first(&mut first_error, error),
                    }
                }
            }
            PermissionStatus::Denied | PermissionStatus::Unknown => {
                for id in order.iter().rev() {
                    let Some(slot) = slot_in(&slots, *id) else {
                        continue;
                    };
                    let mut guard = slot.lock_state();
                    if !guard.enabled
                        || guard.paused
                        || guard.status != ModuleStatus::Running
                        || !guard.requirements.iter().any(|r| r.permission() == permission)
                    {
                        continue;
                    }
                    guard.status = ModuleStatus::WaitingForPermission(permission);
                    guard.publish();
                    if let Err(error) = self.quiesce(slot, &mut guard) {
                        keep_first(&mut first_error, error);
                    }
                }
            }
        }
        let reconciled = self.reconcile_locked();
        first_error.map_or(reconciled, Err)
    }

    /// Sets the shutdown latch and invalidates every module handle.
    ///
    /// Takes no lifecycle lock, no slot state lock and joins nothing, so it
    /// returns even while another thread holds either lock in module code.
    pub fn invalidate(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        for slot in self.slots_snapshot() {
            slot.invalidation.invalidate();
        }
        self.states.close_all();
        self.reservations.close();
    }

    /// Snapshots stop targets in reverse start order from slot metadata only.
    ///
    /// Returns immediately even while a start holds a slot state lock. A slot
    /// that already stopped is omitted.
    #[must_use]
    pub fn shutdown_targets(&self) -> Vec<StopTarget> {
        let mut targets: Vec<StopTarget> = self
            .slots_snapshot()
            .iter()
            .filter_map(|slot| {
                let meta = *lock(&slot.meta);
                if !meta.needs_stop {
                    return None;
                }
                let sequence = meta.sequence?;
                Some(StopTarget {
                    id: slot.descriptor.id,
                    sequence,
                })
            })
            .collect();
        targets.sort_by_key(|target| std::cmp::Reverse(target.sequence));
        targets
    }

    /// Stops exactly one target. Idempotent and never refused.
    ///
    /// Takes only this module's state lock, publishes a non-running status,
    /// then quiesces so the ordinary stop path performs the stop and its retry
    /// bookkeeping. It never takes `lifecycle`. A target that already stopped
    /// is a no-op.
    ///
    /// # Errors
    /// Returns the module's stop failure. The slot stays consistent: it keeps
    /// `needs_stop` set and reports `Failed`, so the next target still stops.
    pub fn stop_module(&self, target: &StopTarget) -> RegistryResult<()> {
        self.stop_target(target, None).map(|_| ())
    }

    /// Stops one target like [`Registry::stop_module`], bounding its worker join.
    ///
    /// Returns `Ok(false)` when `Module::stop` returned but a worker outlived the
    /// bound. That worker is detached and the slot lock is released, so a hung
    /// worker retains nothing another caller needs.
    ///
    /// # Errors
    /// Returns the module's stop failure, as [`Registry::stop_module`] does.
    pub fn stop_module_bounded(
        &self,
        target: &StopTarget,
        wait: &dyn ShutdownWait,
    ) -> RegistryResult<bool> {
        self.stop_target(target, Some(wait))
    }

    fn stop_target(
        &self,
        target: &StopTarget,
        wait: Option<&dyn ShutdownWait>,
    ) -> RegistryResult<bool> {
        let Some(slot) =
            self.slots_snapshot().into_iter().find(|slot| slot.descriptor.id == target.id)
        else {
            return Ok(true);
        };
        let mut guard = slot.lock_state();
        if guard.runtime.is_none() && !guard.needs_stop {
            return Ok(true);
        }
        guard.status = if guard.enabled {
            ModuleStatus::Stopped
        } else {
            ModuleStatus::Disabled
        };
        guard.publish();
        self.quiesce_joining(&slot, &mut guard, wait)
    }

    /// Invalidates every module, then stops them in reverse start order.
    ///
    /// Repeated calls are no-ops. Once called, the registry refuses new starts,
    /// registrations and lifecycle changes with [`RegistryError::ShuttingDown`].
    /// No lifecycle lock is taken and no lock spans a stop.
    ///
    /// # Errors
    /// Returns the first stop failure after attempting every module.
    pub fn shutdown(&self) -> RegistryResult<()> {
        self.invalidate();
        let mut first_error = None;
        for target in self.shutdown_targets() {
            if let Err(error) = self.stop_module(&target) {
                keep_first(&mut first_error, error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn start_slot(&self, slot: &ModuleSlot) -> RegistryResult<()> {
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        let mut guard = slot.lock_state();
        let state = &mut *guard;
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err(RegistryError::ShuttingDown);
        }
        if state.status == ModuleStatus::Running || !state.enabled || state.paused {
            return Ok(());
        }
        if state.config.is_none() {
            state.status = ModuleStatus::Stopped;
            return Ok(());
        }
        if state.runtime.is_some() || state.needs_stop {
            self.quiesce(slot, state)?;
        }
        for &executable in slot.descriptor.executables {
            if !executable_available(executable) {
                state.status = ModuleStatus::Unavailable(UnavailableReason::MissingExecutable(
                    executable.to_owned(),
                ));
                return Ok(());
            }
        }
        if let Some(permission) = self.missing_permission(&state.requirements) {
            state.status = ModuleStatus::WaitingForPermission(permission);
            return Ok(());
        }
        for &service in slot.descriptor.requires {
            if !self.service_ready(service) {
                state.status =
                    ModuleStatus::Unavailable(UnavailableReason::MissingService(service));
                return Ok(());
            }
        }
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        // Metadata is truthful from admission, so teardown can find this
        // module while the start below is still blocked in module code.
        {
            let mut meta = lock(&slot.meta);
            meta.sequence = Some(sequence);
            meta.needs_stop = true;
        }
        let mut runtime = self.new_runtime(slot);
        if let Some(cell) = &slot.state_cell {
            // Bound to the revision this start actually runs with. Held until
            // the start succeeds, so a failed start never exposes state.
            runtime.state = Some(GenerationBinding::begin(cell, &runtime.scope, state.revision));
        }
        state.runtime = Some(runtime);
        state.needs_stop = true;
        match self.configure_and_start(slot, state) {
            Ok(()) => {
                if self.shutting_down.load(Ordering::SeqCst) {
                    // The latch was set while this start was in flight. Release
                    // what it created before returning: a late completion must
                    // not publish running state, restart work or keep a native
                    // resource alive past the refusal.
                    state.status = if state.enabled {
                        ModuleStatus::Stopped
                    } else {
                        ModuleStatus::Disabled
                    };
                    let _cleanup = self.quiesce(slot, state);
                    return Err(RegistryError::ShuttingDown);
                }
                if let Some(binding) = state.runtime.as_ref().and_then(|r| r.state.as_ref()) {
                    binding.activate();
                }
                state.status = ModuleStatus::Running;
                Ok(())
            }
            Err(error) => {
                if let Some(runtime) = state.runtime.as_ref() {
                    runtime.end_generation(RuntimeEnd::Failed);
                }
                // Publish the failure before cleanup so every observer reads
                // Failed, never the status from before this start. Then release
                // partial resources. A cleanup failure leaves `needs_stop` set,
                // but never masks the configure/start error, so the start
                // failure is restored after cleanup.
                let failure = ModuleStatus::Failed(ModuleFailure::from_error(&error));
                state.status = failure.clone();
                let _cleanup = self.quiesce(slot, state);
                state.status = failure;
                Err(RegistryError::Module(error))
            }
        }
    }

    /// The first required permission whose fresh, nonprompting status is not
    /// granted.
    fn missing_permission(&self, requirements: &[PermissionRequirement]) -> Option<Permission> {
        requirements
            .iter()
            .map(|requirement| requirement.permission())
            .find(|&permission| self.permissions.status(permission) != PermissionStatus::Granted)
    }

    fn configure_and_start(&self, slot: &ModuleSlot, state: &mut ModuleState) -> ModuleResult<()> {
        let ModuleState { module, config, runtime, .. } = state;
        let Some(config) = config.as_ref() else {
            return Err(ModuleError::new(
                slot.descriptor.id,
                Operation::Configure,
                ErrorCategory::Lifecycle,
                "no accepted configuration",
            ));
        };
        let Some(runtime) = runtime.as_ref() else {
            return Err(ModuleError::new(
                slot.descriptor.id,
                Operation::Start,
                ErrorCategory::Lifecycle,
                "no runtime was prepared",
            ));
        };
        let configure = self.context(slot, runtime, Operation::Configure);
        module.configure(config.as_ref(), &configure)?;
        let start = self.context(slot, runtime, Operation::Start);
        module.start(&start)
    }

    /// Stops the module and joins its workers. The caller holds the module lock
    /// and has already set the non-running target status.
    fn quiesce(&self, slot: &ModuleSlot, state: &mut ModuleState) -> RegistryResult<()> {
        self.quiesce_joining(slot, state, None).map(|_| ())
    }

    /// Returns whether every worker joined. Without a `wait` the join is
    /// unbounded; with one, a worker outliving the bound is detached.
    fn quiesce_joining(
        &self,
        slot: &ModuleSlot,
        state: &mut ModuleState,
        wait: Option<&dyn ShutdownWait>,
    ) -> RegistryResult<bool> {
        if state.runtime.is_none() && !state.needs_stop {
            return Ok(true);
        }
        // Publish before cancelling or calling `stop` so a dependent checking
        // this provider's readiness sees the non-running target immediately.
        debug_assert_ne!(
            state.status,
            ModuleStatus::Running,
            "a stopping module must publish a non-running status first"
        );
        slot.publish(state);
        let runtime = state.runtime.take().unwrap_or_else(|| self.new_runtime(slot));
        // End the generation first: scoped capabilities reject late writes and
        // publish their tombstones before any cancellation, module stop or
        // worker join can wait.
        runtime.end_generation(end_for_status(&state.status));
        runtime.cancellation.cancel();
        runtime.invalidate_windows();
        let needs_stop = state.needs_stop;
        let result = if needs_stop {
            let ctx = self.context(slot, &runtime, Operation::Stop);
            state.module.stop(&ctx)
        } else {
            Ok(())
        };
        // Windows belong to this exact runtime. Invalidate it before owner
        // cleanup so an obsolete capability cannot open or release a successor
        // runtime's windows after a restart. Releasing before the worker join
        // keeps the runtime available for the stop context without widening the
        // gate.
        runtime.release_windows();
        let joined = if let Some(wait) = wait {
            runtime.workers.cancel_and_join_bounded(wait)
        } else {
            runtime.workers.cancel_and_join();
            true
        };
        match result {
            Ok(()) => {
                state.needs_stop = false;
                let mut meta = lock(&slot.meta);
                meta.needs_stop = false;
                meta.sequence = None;
                Ok(joined)
            }
            Err(error) => {
                state.needs_stop = true;
                state.status = ModuleStatus::Failed(ModuleFailure::from_error(&error));
                Err(RegistryError::Module(error))
            }
        }
    }

    /// Whether a service is ready.
    ///
    /// The caller may already hold the consumer's module lock; this reads only
    /// the provider's published snapshot, so it never waits for provider module
    /// code or a worker join.
    fn service_ready(&self, service: ServiceId) -> bool {
        if lock(&self.failed_services).contains(&service) {
            return false;
        }
        let provider = self
            .slots_snapshot()
            .into_iter()
            .find(|slot| slot.descriptor.provides.contains(&service));
        provider.is_some_and(|slot| slot.published_snapshot().status == ModuleStatus::Running)
    }

    fn new_runtime(&self, slot: &ModuleSlot) -> Runtime {
        Runtime::new(
            slot.descriptor.id,
            self.next_generation.fetch_add(1, Ordering::Relaxed) + 1,
            slot.descriptor.surfaces,
            Arc::clone(&self.windows),
            Arc::clone(&self.input),
        )
    }

    fn context(&self, slot: &ModuleSlot, runtime: &Runtime, operation: Operation) -> ModuleContext {
        ModuleContext::new(
            slot.descriptor.id,
            ScopedLogger::new(Arc::clone(&self.logger), slot.descriptor.id, operation),
            runtime.workers.handle(),
            runtime.cancellation.clone(),
            slot.invalidation.clone(),
            runtime.scope.clone(),
            crate::context::ContextServices {
                states: Arc::clone(&self.states),
                observes: slot.descriptor.observes,
                reservations: Arc::clone(&self.reservations),
                reserves_screen: slot.descriptor.reserves_screen,
                events: Arc::clone(&self.events),
                main_thread: Arc::clone(&self.main_thread),
                permissions: Arc::clone(&self.permissions),
                windows: runtime.windows.clone(),
                input: runtime.input.clone(),
            },
        )
    }

    fn slot<C>(&self, handle: ModuleHandle<C>) -> RegistryResult<Arc<ModuleSlot>> {
        self.slots_snapshot()
            .into_iter()
            .find(|slot| slot.descriptor.id == handle.id())
            .ok_or(RegistryError::UnknownModule { id: handle.id() })
    }

    fn slots_snapshot(&self) -> Vec<Arc<ModuleSlot>> { lock(&self.slots).clone() }

    #[cfg(test)]
    pub(crate) const fn reservation_hub(&self) -> &Arc<ReservationHub> { &self.reservations }

    #[cfg(test)]
    pub(crate) const fn state_hub(&self) -> &Arc<StateHub> { &self.states }
}

/// The reason a never-started provider reads unavailable for `status`.
const fn status_end(status: &ModuleStatus) -> Option<RuntimeEnd> {
    match status {
        ModuleStatus::Stopped | ModuleStatus::Running => None,
        other => Some(end_for_status(other)),
    }
}

/// The generation end implied by the non-running status a stop publishes first.
const fn end_for_status(status: &ModuleStatus) -> RuntimeEnd {
    match status {
        ModuleStatus::Disabled => RuntimeEnd::Disabled,
        ModuleStatus::Paused => RuntimeEnd::Paused,
        ModuleStatus::Failed(_) => RuntimeEnd::Failed,
        ModuleStatus::WaitingForPermission(_) => RuntimeEnd::PermissionBlocked,
        ModuleStatus::Unavailable(_) => RuntimeEnd::DependencyUnavailable,
        ModuleStatus::Stopped | ModuleStatus::Running => RuntimeEnd::Stopped,
    }
}

/// Whether a command name resolves to an executable file on the current `PATH`.
fn executable_available(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| {
        directory
            .join(name)
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
}

fn slot_in(slots: &[Arc<ModuleSlot>], id: ModuleId) -> Option<&Arc<ModuleSlot>> {
    slots.iter().find(|slot| slot.descriptor.id == id)
}

fn keep_first(first: &mut Option<RegistryError>, error: RegistryError) {
    if first.is_none() {
        *first = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Barrier, mpsc};
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::error::ConfigError;
    use crate::logging::LogRecord;
    use crate::permissions::PermissionError;

    const SERVICE: ServiceId = ServiceId::new("test.service");
    const SERVICE_B: ServiceId = ServiceId::new("test.service.b");
    static PROVIDES_SERVICE: [ServiceId; 1] = [SERVICE];
    static PROVIDES_SERVICE_B: [ServiceId; 1] = [SERVICE_B];
    static REQUIRES_SERVICE: [ServiceId; 1] = [SERVICE];
    static REQUIRES_SERVICE_B: [ServiceId; 1] = [SERVICE_B];
    static ACCESSIBILITY: [Permission; 1] = [Permission::Accessibility];

    /// Ordered event log shared by the fake modules and the test.
    #[derive(Default)]
    struct Trace {
        events: Mutex<Vec<String>>,
    }

    impl Trace {
        fn record(&self, event: impl Into<String>) { lock(&self.events).push(event.into()); }

        fn events(&self) -> Vec<String> { lock(&self.events).clone() }
    }

    #[derive(Default)]
    struct CapturingLogger {
        records: Mutex<Vec<LogRecord>>,
    }

    impl Logger for CapturingLogger {
        fn log(&self, record: LogRecord) { lock(&self.records).push(record); }
    }

    impl CapturingLogger {
        fn records(&self) -> Vec<LogRecord> { lock(&self.records).clone() }
    }

    struct FakePermissions {
        statuses: Mutex<HashMap<Permission, PermissionStatus>>,
        requests: AtomicUsize,
    }

    impl FakePermissions {
        fn new() -> Self {
            Self {
                statuses: Mutex::new(HashMap::new()),
                requests: AtomicUsize::new(0),
            }
        }

        fn set(&self, permission: Permission, status: PermissionStatus) {
            lock(&self.statuses).insert(permission, status);
        }
    }

    impl PermissionProvider for FakePermissions {
        fn status(&self, permission: Permission) -> PermissionStatus {
            lock(&self.statuses)
                .get(&permission)
                .copied()
                .unwrap_or(PermissionStatus::Granted)
        }

        fn request(&self, permission: Permission) -> Result<PermissionStatus, PermissionError> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            Ok(self.status(permission))
        }
    }

    struct InlineExecutor;

    impl MainThreadExecutor for InlineExecutor {
        fn is_main_thread(&self) -> bool { true }

        fn execute(
            &self,
            job: Box<dyn FnOnce() + Send + 'static>,
        ) -> Result<(), crate::threading::ThreadError> {
            job();
            Ok(())
        }
    }

    struct Harness {
        registry: Registry,
        trace: Arc<Trace>,
        logger: Arc<CapturingLogger>,
        permissions: Arc<FakePermissions>,
    }

    impl Harness {
        fn new() -> Self { Self::with_windows(Arc::new(crate::windows::UnavailableWindows)) }

        fn with_windows(windows: Arc<dyn crate::windows::WindowBackend>) -> Self {
            let logger = Arc::new(CapturingLogger::default());
            let permissions = Arc::new(FakePermissions::new());
            let logger_sink: Arc<dyn Logger> = logger.clone();
            let permission_sink: Arc<dyn PermissionProvider> = permissions.clone();
            let registry = Registry::new(
                logger_sink,
                Arc::new(EventBus::new()),
                Arc::new(InlineExecutor),
                permission_sink,
                windows,
            );
            Self {
                registry,
                trace: Arc::new(Trace::default()),
                logger,
                permissions,
            }
        }

        fn fake(&self, id: &'static str) -> (FakeModule, Arc<Mutex<FakeState>>) {
            fake(id, &self.trace)
        }

        fn gated(
            &self,
            id: &'static str,
            blocked: Blocked,
        ) -> (
            FakeModule,
            Arc<Mutex<FakeState>>,
            mpsc::Receiver<()>,
            mpsc::Sender<()>,
        ) {
            let (module, state) = self.fake(id);
            let (gate, entered, release) = Gate::new(blocked);
            (
                FakeModule { gate: Some(gate), ..module },
                state,
                entered,
                release,
            )
        }
    }

    #[derive(Default)]
    struct RecordingWindows {
        released: Mutex<Vec<ModuleId>>,
    }

    impl crate::windows::WindowBackend for RecordingWindows {
        fn open(
            &self,
            _: ModuleId,
            _: &crate::windows::WindowRequest,
        ) -> Result<crate::windows::WindowId, crate::windows::WindowError> {
            Err(crate::windows::WindowError::Backend("not used".to_owned()))
        }

        fn focus(&self, _: crate::windows::WindowId) -> Result<(), crate::windows::WindowError> {
            Ok(())
        }

        fn close(&self, _: crate::windows::WindowId) {}

        fn release_owner(&self, owner: ModuleId) { lock(&self.released).push(owner); }
    }

    #[derive(Default)]
    struct LeaseBackend {
        next: Mutex<u64>,
        entries: Mutex<HashMap<(ModuleId, String), crate::windows::WindowId>>,
        opened: Mutex<Vec<crate::windows::WindowId>>,
        closed: Mutex<Vec<crate::windows::WindowId>>,
        released: Mutex<Vec<ModuleId>>,
    }

    impl crate::windows::WindowBackend for LeaseBackend {
        fn open(
            &self,
            owner: ModuleId,
            request: &crate::windows::WindowRequest,
        ) -> Result<crate::windows::WindowId, crate::windows::WindowError> {
            let key = (owner, request.key.as_str().to_owned());
            let existing = lock(&self.entries).get(&key).copied();
            if let Some(id) = existing {
                lock(&self.opened).push(id);
                return Ok(id);
            }
            let mut next = lock(&self.next);
            *next += 1;
            let id = crate::windows::WindowId::new(*next);
            drop(next);
            lock(&self.entries).insert(key, id);
            lock(&self.opened).push(id);
            Ok(id)
        }

        fn focus(&self, _: crate::windows::WindowId) -> Result<(), crate::windows::WindowError> {
            Ok(())
        }

        fn close(&self, window: crate::windows::WindowId) {
            lock(&self.closed).push(window);
            lock(&self.entries).retain(|_, id| *id != window);
        }

        fn release_owner(&self, owner: ModuleId) {
            lock(&self.released).push(owner);
            lock(&self.entries).retain(|(entry_owner, _), _| *entry_owner != owner);
        }
    }

    static TEST_SURFACES: [crate::windows::SurfaceId; 1] = [crate::windows::SurfaceId::new("test")];

    fn lease_request() -> crate::windows::WindowRequest {
        crate::windows::WindowRequest::new(
            crate::windows::WindowKey::new("main").expect("key"),
            crate::windows::SurfaceId::new("test"),
            crate::windows::WindowSize::new(800, 600).expect("size"),
            crate::windows::WindowKind::Standard,
        )
    }

    struct TwoLeaseModule {
        id: ModuleId,
        configure: Option<crate::windows::OwnedWindow>,
        start: Option<crate::windows::OwnedWindow>,
        drops: u32,
    }

    impl TwoLeaseModule {
        fn new(id: &'static str) -> Self {
            Self {
                id: ModuleId::new(id),
                configure: None,
                start: None,
                drops: 0,
            }
        }
    }

    impl Module for TwoLeaseModule {
        type Config = FakeConfig;

        fn descriptor(&self) -> ModuleDescriptor {
            ModuleDescriptor {
                id: self.id,
                provides: &[],
                requires: &[],
                permissions: &[],
                executables: &[],
                surfaces: &TEST_SURFACES,
                observes: &[],
                reserves_screen: false,
            }
        }

        fn configure(&mut self, _: &FakeConfig, ctx: &ModuleContext) -> ModuleResult<()> {
            self.configure =
                Some(ctx.windows().request(&lease_request()).expect("configure window"));
            Ok(())
        }

        fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
            self.start = Some(ctx.windows().request(&lease_request()).expect("start window"));
            Ok(())
        }

        fn reconfigure(&mut self, _: &FakeConfig, _: &ModuleContext) -> ModuleResult<()> {
            self.drops += 1;
            match self.drops {
                1 => drop(self.configure.take()),
                2 => drop(self.start.take()),
                _ => {}
            }
            Ok(())
        }

        fn stop(&mut self, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }
    }

    #[test]
    fn contexts_for_one_runtime_share_one_window_lease() {
        let windows = Arc::new(LeaseBackend::default());
        let harness = Harness::with_windows(windows.clone());
        let handle = harness
            .registry
            .register(TwoLeaseModule::new("leases"), FakeConfig::on(1))
            .expect("register");
        harness.registry.start_all().expect("start");
        assert_eq!(lock(&windows.entries).len(), 1);

        harness.registry.reload(handle, FakeConfig::on(2)).expect("first reconfigure");
        assert!(
            lock(&windows.closed).is_empty(),
            "dropping the configure lease must not close the start lease"
        );

        harness.registry.reload(handle, FakeConfig::on(3)).expect("second reconfigure");
        assert_eq!(lock(&windows.closed).len(), 1);
        harness.registry.shutdown().expect("shutdown");
    }

    #[derive(Default)]
    struct LeaseRecord {
        entries: Vec<(crate::windows::WindowCapability, crate::windows::OwnedWindow)>,
    }

    struct RetainedLeaseModule {
        id: ModuleId,
        record: Arc<Mutex<LeaseRecord>>,
    }

    impl Module for RetainedLeaseModule {
        type Config = FakeConfig;

        fn descriptor(&self) -> ModuleDescriptor {
            ModuleDescriptor {
                id: self.id,
                provides: &[],
                requires: &[],
                permissions: &[],
                executables: &[],
                surfaces: &TEST_SURFACES,
                observes: &[],
                reserves_screen: false,
            }
        }

        fn configure(&mut self, _: &FakeConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

        fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
            let window = ctx.windows().request(&lease_request()).expect("window");
            lock(&self.record).entries.push((ctx.windows().clone(), window));
            Ok(())
        }

        fn reconfigure(&mut self, _: &FakeConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

        fn stop(&mut self, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }
    }

    #[test]
    fn an_obsolete_capability_cannot_close_or_release_a_restarted_runtime() {
        let windows = Arc::new(LeaseBackend::default());
        let harness = Harness::with_windows(windows.clone());
        let record = Arc::new(Mutex::new(LeaseRecord::default()));
        let handle = harness
            .registry
            .register(
                RetainedLeaseModule {
                    id: ModuleId::new("retained"),
                    record: Arc::clone(&record),
                },
                FakeConfig::on(1),
            )
            .expect("register");
        harness.registry.start_all().expect("start");
        let first_id = {
            let record = lock(&record);
            assert_eq!(record.entries.len(), 1);
            record.entries[0].1.id()
        };

        harness.registry.pause(handle).expect("pause");
        harness.registry.resume(handle).expect("resume");
        let second_id = {
            let record = lock(&record);
            assert_eq!(record.entries.len(), 2);
            record.entries[1].1.id()
        };
        assert_ne!(first_id, second_id);

        let (old_capability, old_window) = {
            let mut record = lock(&record);
            record.entries.remove(0)
        };
        drop(old_window);
        assert!(
            lock(&windows.closed).is_empty(),
            "dropping an obsolete handle closed a successor window"
        );
        assert!(lock(&windows.entries).values().any(|id| *id == second_id));
        assert!(matches!(
            old_capability.request(&lease_request()),
            Err(crate::windows::WindowError::OwnerStopped { .. })
        ));
        let (_, new_window) = {
            let mut record = lock(&record);
            record.entries.remove(0)
        };
        drop(new_window);
        assert_eq!(*lock(&windows.closed), vec![second_id]);
        harness.registry.shutdown().expect("shutdown");
    }

    #[test]
    fn a_failed_start_releases_owned_windows() {
        let windows = Arc::new(RecordingWindows::default());
        let harness = Harness::with_windows(windows.clone());
        let (mut broken, _) = harness.fake("broken");
        broken.fail.start = true;
        harness.registry.register(broken, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect_err("start fails");
        assert_eq!(*lock(&windows.released), vec![ModuleId::new("broken")]);
    }

    #[test]
    fn stopping_a_module_releases_owned_windows() {
        let windows = Arc::new(RecordingWindows::default());
        let harness = Harness::with_windows(windows.clone());
        let (module, _) = harness.fake("module");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        lock(&windows.released).clear();
        harness.registry.pause(handle).expect("pause");
        assert_eq!(*lock(&windows.released), vec![ModuleId::new("module")]);
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeConfig {
        enabled: bool,
        value: u32,
        valid: bool,
    }

    impl FakeConfig {
        fn on(value: u32) -> Self {
            Self {
                enabled: true,
                value,
                valid: true,
            }
        }

        fn off(value: u32) -> Self {
            Self {
                enabled: false,
                value,
                valid: true,
            }
        }

        fn invalid() -> Self {
            Self {
                enabled: true,
                value: 0,
                valid: false,
            }
        }
    }

    impl ModuleConfig for FakeConfig {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), ConfigError> {
            if self.valid {
                Ok(())
            } else {
                Err(ConfigError::new("fake config is invalid"))
            }
        }
    }

    /// Each lifecycle method has an independent failpoint.
    #[allow(clippy::struct_excessive_bools)]
    #[derive(Clone, Copy, Default)]
    struct FailPlan {
        configure: bool,
        start: bool,
        reconfigure: bool,
        stop: bool,
    }

    /// A lifecycle call the fake module blocks inside until the test releases it.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Blocked {
        Configure,
        Start,
        Reconfigure,
        Stop,
    }

    /// One-shot gate: the first matching operation signals entry and waits.
    struct Gate {
        blocked: Blocked,
        entered: mpsc::Sender<()>,
        release: Mutex<Option<mpsc::Receiver<()>>>,
    }

    impl Gate {
        fn new(blocked: Blocked) -> (Arc<Self>, mpsc::Receiver<()>, mpsc::Sender<()>) {
            let (entered, entered_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            (
                Arc::new(Self {
                    blocked,
                    entered,
                    release: Mutex::new(Some(release_rx)),
                }),
                entered_rx,
                release,
            )
        }

        fn pass(&self, operation: Blocked) {
            if operation != self.blocked {
                return;
            }
            let receiver = {
                let mut release = lock(&self.release);
                if release.is_none() {
                    return;
                }
                self.entered.send(()).expect("the test is listening for the gate");
                release.take().expect("checked above")
            };
            receiver.recv().expect("the test releases the gate");
        }
    }

    /// Runs a read on another thread and fails if it waits on blocked module code.
    fn within<T: Send + 'static>(
        label: &'static str,
        job: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(job());
        });
        receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{label} waited for blocked module code"))
    }

    fn wait_until_blocked(entered: &mpsc::Receiver<()>) {
        entered
            .recv_timeout(Duration::from_secs(5))
            .expect("the module operation did not start");
    }

    #[derive(Default)]
    struct FakeState {
        starts: u32,
        stops: u32,
        held: bool,
        stopped_while_held: bool,
        double_start: bool,
    }

    struct FakeModule {
        id: ModuleId,
        provides: &'static [ServiceId],
        requires: &'static [ServiceId],
        permissions: &'static [Permission],
        executables: &'static [&'static str],
        surfaces: &'static [crate::windows::SurfaceId],
        fail: FailPlan,
        gate: Option<Arc<Gate>>,
        trace: Arc<Trace>,
        state: Arc<Mutex<FakeState>>,
        invalidation: Arc<Mutex<Option<ModuleInvalidation>>>,
        invalidation_hits: Arc<AtomicUsize>,
        native: Arc<Mutex<Vec<&'static str>>>,
        descriptor_entered: Option<mpsc::Sender<()>>,
        /// Requirement demanded by configs with `value >= 100`.
        high_value_requirement: Option<PermissionRequirement>,
    }

    impl FakeModule {
        /// Simulates queued native work, which rechecks the handle at issuance.
        fn issue(&self, action: &'static str) {
            issue_native(&self.invalidation, &self.native, action);
        }
    }

    /// Queued native work: dropped when its captured handle is invalidated.
    fn issue_native(
        handle: &Mutex<Option<ModuleInvalidation>>,
        native: &Mutex<Vec<&'static str>>,
        action: &'static str,
    ) {
        let invalidated = lock(handle).as_ref().is_some_and(ModuleInvalidation::is_invalidated);
        if !invalidated {
            lock(native).push(action);
        }
    }

    fn fake(id: &'static str, trace: &Arc<Trace>) -> (FakeModule, Arc<Mutex<FakeState>>) {
        let state = Arc::new(Mutex::new(FakeState::default()));
        (
            FakeModule {
                id: ModuleId::new(id),
                provides: &[],
                requires: &[],
                permissions: &[],
                executables: &[],
                surfaces: &[],
                fail: FailPlan::default(),
                gate: None,
                trace: Arc::clone(trace),
                state: Arc::clone(&state),
                invalidation: Arc::new(Mutex::new(None)),
                invalidation_hits: Arc::new(AtomicUsize::new(0)),
                native: Arc::new(Mutex::new(Vec::new())),
                descriptor_entered: None,
                high_value_requirement: None,
            },
            state,
        )
    }

    impl Module for FakeModule {
        type Config = FakeConfig;

        fn descriptor(&self) -> ModuleDescriptor {
            if let Some(entered) = &self.descriptor_entered {
                let _ = entered.send(());
            }
            ModuleDescriptor {
                id: self.id,
                provides: self.provides,
                requires: self.requires,
                permissions: self.permissions,
                executables: self.executables,
                surfaces: self.surfaces,
                observes: &[],
                reserves_screen: false,
            }
        }

        fn required_permissions(&self, config: &FakeConfig) -> Vec<PermissionRequirement> {
            match self.high_value_requirement {
                Some(requirement) if config.value >= 100 => vec![requirement],
                Some(_) => Vec::new(),
                None => static_requirements(&self.descriptor()),
            }
        }

        fn configure(&mut self, config: &FakeConfig, ctx: &ModuleContext) -> ModuleResult<()> {
            if let Some(gate) = &self.gate {
                gate.pass(Blocked::Configure);
            }
            self.trace.record(format!("{}.configure.{}", self.id, config.value));
            ctx.logger().info(format!("configure {}", config.value));
            if self.fail.configure {
                return Err(ModuleError::new(
                    self.id,
                    Operation::Configure,
                    ErrorCategory::Config,
                    "configure failpoint",
                ));
            }
            Ok(())
        }

        fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
            let invalidation = ctx.invalidation().clone();
            {
                let hits = Arc::clone(&self.invalidation_hits);
                invalidation.on_invalidate(Arc::new(move || {
                    hits.fetch_add(1, Ordering::SeqCst);
                }));
            }
            *lock(&self.invalidation) = Some(invalidation);
            self.issue("start");
            if let Some(gate) = &self.gate {
                gate.pass(Blocked::Start);
            }
            {
                let mut state = lock(&self.state);
                if state.held {
                    state.double_start = true;
                }
                state.starts += 1;
                state.held = true;
            }
            self.trace.record(format!("{}.start", self.id));
            ctx.logger().info("start");
            if self.fail.start {
                return Err(ModuleError::new(
                    self.id,
                    Operation::Start,
                    ErrorCategory::Platform,
                    "start failpoint",
                ));
            }
            Ok(())
        }

        fn reconfigure(&mut self, config: &FakeConfig, ctx: &ModuleContext) -> ModuleResult<()> {
            if let Some(gate) = &self.gate {
                gate.pass(Blocked::Reconfigure);
            }
            self.trace.record(format!("{}.reconfigure.{}", self.id, config.value));
            ctx.logger().info(format!("reconfigure {}", config.value));
            if self.fail.reconfigure {
                return Err(ModuleError::new(
                    self.id,
                    Operation::Reconfigure,
                    ErrorCategory::Lifecycle,
                    "reconfigure failpoint",
                ));
            }
            Ok(())
        }

        fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
            if let Some(gate) = &self.gate {
                gate.pass(Blocked::Stop);
            }
            {
                let mut state = lock(&self.state);
                state.stops += 1;
                state.stopped_while_held = state.held;
                state.held = false;
            }
            self.trace.record(format!("{}.stop", self.id));
            ctx.logger().info("stop");
            if self.fail.stop {
                return Err(ModuleError::new(
                    self.id,
                    Operation::Stop,
                    ErrorCategory::Platform,
                    "stop failpoint",
                ));
            }
            Ok(())
        }
    }

    /// A module whose worker ignores cancellation, so stopping blocks in the join.
    struct JoinModule {
        id: ModuleId,
        started: mpsc::Sender<()>,
        stopped: mpsc::Sender<()>,
        release: Option<mpsc::Receiver<()>>,
    }

    impl Module for JoinModule {
        type Config = FakeConfig;

        fn descriptor(&self) -> ModuleDescriptor {
            ModuleDescriptor {
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

        fn configure(&mut self, _: &FakeConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

        fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
            let release = self.release.take().expect("the module starts once");
            let started = self.started.clone();
            ctx.spawn_worker("ignores-cancel", move |_| {
                let _ = started.send(());
                let _ = release.recv();
            })
        }

        fn reconfigure(&mut self, _: &FakeConfig, _: &ModuleContext) -> ModuleResult<()> { Ok(()) }

        fn stop(&mut self, _: &ModuleContext) -> ModuleResult<()> {
            let _ = self.stopped.send(());
            Ok(())
        }
    }

    #[test]
    fn duplicate_module_ids_are_rejected() {
        let harness = Harness::new();
        let (first, _) = harness.fake("duplicate");
        let (second, _) = harness.fake("duplicate");
        harness.registry.register(first, FakeConfig::on(1)).expect("first registration");
        let error = harness.registry.register(second, FakeConfig::on(1)).expect_err("duplicate id");
        assert!(matches!(
            error,
            RegistryError::DuplicateModule { id } if id == ModuleId::new("duplicate")
        ));
    }

    #[test]
    fn duplicate_service_providers_are_rejected_at_registration() {
        let harness = Harness::new();
        let (first, _) = harness.fake("first");
        let (mut second, _) = harness.fake("second");
        let mut first = first;
        first.provides = &PROVIDES_SERVICE;
        second.provides = &PROVIDES_SERVICE;
        harness.registry.register(first, FakeConfig::on(1)).expect("first registration");
        let error = harness
            .registry
            .register(second, FakeConfig::on(1))
            .expect_err("duplicate provider");
        assert!(matches!(error, RegistryError::DuplicateProvider { .. }));
    }

    #[test]
    fn cyclic_declarations_are_rejected_before_startup() {
        let harness = Harness::new();
        let (mut alpha, _) = harness.fake("alpha");
        alpha.provides = &PROVIDES_SERVICE;
        alpha.requires = &REQUIRES_SERVICE_B;
        let (mut beta, _) = harness.fake("beta");
        beta.provides = &PROVIDES_SERVICE_B;
        beta.requires = &REQUIRES_SERVICE;
        harness.registry.register(alpha, FakeConfig::on(1)).expect("alpha");
        let error = harness.registry.register(beta, FakeConfig::on(1)).expect_err("cycle");
        assert!(matches!(error, RegistryError::CyclicDependency { .. }));
    }

    #[test]
    fn missing_services_fail_before_any_module_starts() {
        let harness = Harness::new();
        let (mut consumer, consumer_state) = harness.fake("consumer");
        consumer.requires = &REQUIRES_SERVICE;
        let (solo, solo_state) = harness.fake("solo");
        harness.registry.register(consumer, FakeConfig::on(1)).expect("consumer");
        harness.registry.register(solo, FakeConfig::on(1)).expect("solo");
        let error = harness.registry.start_all().expect_err("missing provider");
        assert!(matches!(error, RegistryError::MissingProvider { .. }));
        assert_eq!(lock(&consumer_state).starts, 0);
        assert_eq!(lock(&solo_state).starts, 0);
        assert_eq!(harness.trace.events(), Vec::<String>::new());
    }

    #[test]
    fn starts_in_dependency_order_and_shuts_down_in_reverse() {
        let harness = Harness::new();
        let (mut consumer, consumer_state) = harness.fake("consumer");
        consumer.requires = &REQUIRES_SERVICE;
        let (mut provider, provider_state) = harness.fake("provider");
        provider.provides = &PROVIDES_SERVICE;
        // Register the consumer first to prove ordering is not registration order.
        harness.registry.register(consumer, FakeConfig::on(1)).expect("consumer");
        harness.registry.register(provider, FakeConfig::on(1)).expect("provider");
        harness.registry.start_all().expect("start");
        assert_eq!(harness.trace.events(), vec![
            "provider.configure.1",
            "provider.start",
            "consumer.configure.1",
            "consumer.start",
        ]);
        harness.registry.shutdown().expect("shutdown");
        assert_eq!(harness.trace.events().split_off(4), vec![
            "consumer.stop",
            "provider.stop"
        ]);
        assert_eq!(lock(&provider_state).starts, 1);
        assert_eq!(lock(&consumer_state).starts, 1);
    }

    #[test]
    fn a_second_start_all_does_not_duplicate_resources() {
        let harness = Harness::new();
        let (module, state) = harness.fake("solo");
        harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("first start");
        harness.registry.start_all().expect("second start");
        assert_eq!(lock(&state).starts, 1);
    }

    #[test]
    fn disabling_stops_resources_and_prevents_starting() {
        let harness = Harness::new();
        let (module, state) = harness.fake("solo");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        harness.registry.set_enabled(handle, false).expect("disable");
        let snapshot = harness.registry.snapshot(handle).expect("snapshot");
        assert_eq!(snapshot.status, ModuleStatus::Disabled);
        assert!(!snapshot.enabled);
        assert_eq!(lock(&state).stops, 1);
        harness.registry.start_all().expect("start disabled");
        assert_eq!(lock(&state).starts, 1);
    }

    #[test]
    fn pause_stops_and_resume_restarts() {
        let harness = Harness::new();
        let (module, state) = harness.fake("solo");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        harness.registry.pause(handle).expect("pause");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Paused
        );
        assert_eq!(lock(&state).stops, 1);
        harness.registry.resume(handle).expect("resume");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&state).starts, 2);
    }

    #[test]
    fn a_grant_after_disable_does_not_start_the_module() {
        let harness = Harness::new();
        let (mut module, state) = harness.fake("gated");
        module.permissions = &ACCESSIBILITY;
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Denied);
        harness.registry.start_all().expect("start");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::WaitingForPermission(Permission::Accessibility)
        );
        harness.registry.set_enabled(handle, false).expect("disable");
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Granted);
        harness
            .registry
            .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
            .expect("grant");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Disabled
        );
        assert_eq!(lock(&state).starts, 0);
        harness.registry.set_enabled(handle, true).expect("enable");
        assert_eq!(lock(&state).starts, 1);
    }

    #[test]
    fn permission_revocation_stops_only_the_dependent() {
        let harness = Harness::new();
        let (mut gated, gated_state) = harness.fake("gated");
        gated.permissions = &ACCESSIBILITY;
        let (free, free_state) = harness.fake("free");
        let gated_handle = harness.registry.register(gated, FakeConfig::on(1)).expect("gated");
        let free_handle = harness.registry.register(free, FakeConfig::on(1)).expect("free");
        harness.registry.start_all().expect("start");
        assert_eq!(lock(&gated_state).starts, 1);
        assert_eq!(lock(&free_state).starts, 1);
        harness
            .registry
            .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
            .expect("revoke");
        assert_eq!(
            harness.registry.snapshot(gated_handle).expect("gated snapshot").status,
            ModuleStatus::WaitingForPermission(Permission::Accessibility)
        );
        assert_eq!(lock(&gated_state).stops, 1);
        assert_eq!(
            harness.registry.snapshot(free_handle).expect("free snapshot").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&free_state).stops, 0);
    }

    #[test]
    fn a_paused_reload_only_updates_the_next_start_snapshot() {
        let harness = Harness::new();
        let (module, state) = harness.fake("solo");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        harness.registry.pause(handle).expect("pause");
        harness.registry.reload(handle, FakeConfig::on(2)).expect("reload");
        let snapshot = harness.registry.snapshot(handle).expect("snapshot");
        assert_eq!(snapshot.status, ModuleStatus::Paused);
        assert_eq!(snapshot.revision, 2);
        assert!(!harness.trace.events().iter().any(|event| event.contains("reconfigure")));
        harness.registry.resume(handle).expect("resume");
        assert_eq!(lock(&state).starts, 2);
        assert_eq!(harness.trace.events().split_off(3), vec![
            "solo.configure.2",
            "solo.start"
        ]);
    }

    #[test]
    fn a_rejected_reconfigure_keeps_running_and_the_last_revision() {
        let harness = Harness::new();
        let (module, state) = harness.fake("solo");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        let error = harness.registry.reload(handle, FakeConfig::invalid()).expect_err("invalid");
        assert!(matches!(error, RegistryError::InvalidConfig { .. }));
        let snapshot = harness.registry.snapshot(handle).expect("snapshot");
        assert_eq!(snapshot.status, ModuleStatus::Running);
        assert_eq!(snapshot.revision, 1);
        assert_eq!(
            snapshot.diagnostic.expect("diagnostic").message,
            "fake config is invalid"
        );
        assert_eq!(lock(&state).stops, 0);
    }

    #[test]
    fn a_failed_reconfigure_keeps_the_previous_configuration() {
        let harness = Harness::new();
        let (mut module, _) = harness.fake("solo");
        module.fail.reconfigure = true;
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        let error = harness.registry.reload(handle, FakeConfig::on(2)).expect_err("reconfigure");
        assert!(matches!(error, RegistryError::Module(_)));
        let snapshot = harness.registry.snapshot(handle).expect("snapshot");
        assert_eq!(snapshot.status, ModuleStatus::Running);
        assert_eq!(snapshot.revision, 1);
        // Reloading the still-active configuration is recognised as unchanged.
        harness.registry.reload(handle, FakeConfig::on(1)).expect("unchanged reload");
        assert_eq!(harness.registry.snapshot(handle).expect("snapshot").revision, 1);
    }

    #[test]
    fn an_invalid_initial_config_is_stopped_with_a_diagnostic() {
        let harness = Harness::new();
        let (module, _) = harness.fake("solo");
        let handle = harness
            .registry
            .register(module, FakeConfig {
                enabled: false,
                ..FakeConfig::invalid()
            })
            .expect("register");
        let snapshot = harness.registry.snapshot(handle).expect("snapshot");
        assert_eq!(snapshot.status, ModuleStatus::Stopped);
        assert!(!snapshot.enabled);
        assert_eq!(snapshot.revision, 0);
        assert!(snapshot.diagnostic.is_some());
    }

    #[test]
    fn stop_after_a_partial_start_is_safe_and_idempotent() {
        let harness = Harness::new();
        let (mut module, state) = harness.fake("partial");
        module.fail.start = true;
        harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect_err("start fails");
        assert!(
            !lock(&state).held,
            "a failed start must release partial resources"
        );
        assert_eq!(lock(&state).stops, 1);
        assert!(lock(&state).stopped_while_held);
        harness.registry.shutdown().expect("shutdown");
        assert_eq!(lock(&state).stops, 1, "shutdown must not stop again");
        harness.registry.shutdown().expect("second shutdown");
        assert_eq!(lock(&state).stops, 1);
    }

    #[test]
    fn a_failed_start_does_not_block_an_independent_module() {
        let harness = Harness::new();
        let (mut broken, broken_state) = harness.fake("broken");
        broken.fail.start = true;
        let (healthy, healthy_state) = harness.fake("healthy");
        harness.registry.register(broken, FakeConfig::on(1)).expect("broken");
        harness.registry.register(healthy, FakeConfig::on(1)).expect("healthy");
        harness.registry.start_all().expect_err("broken fails");
        assert_eq!(lock(&broken_state).starts, 1);
        assert_eq!(lock(&healthy_state).starts, 1);
        assert_eq!(
            harness.registry.snapshots()[1].status,
            ModuleStatus::Running,
            "the healthy module started"
        );
    }

    #[test]
    fn a_configure_failure_is_reported_with_its_operation() {
        let harness = Harness::new();
        let (mut module, state) = harness.fake("solo");
        module.fail.configure = true;
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect_err("configure fails");
        match harness.registry.snapshot(handle).expect("snapshot").status {
            ModuleStatus::Failed(failure) => {
                assert_eq!(failure.operation, Operation::Configure);
                assert_eq!(failure.category, ErrorCategory::Config);
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(lock(&state).starts, 0);
        assert_eq!(
            lock(&state).stops,
            1,
            "configure failure still releases resources"
        );
    }

    #[test]
    fn an_unchanged_reload_does_not_call_the_module() {
        let harness = Harness::new();
        let (module, _) = harness.fake("solo");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        assert_eq!(
            harness.registry.reload(handle, FakeConfig::on(1)).expect("unchanged").status,
            ReloadStatus::Unchanged
        );
        assert!(!harness.trace.events().iter().any(|event| event.contains("reconfigure")));
        assert_eq!(harness.registry.snapshot(handle).expect("snapshot").revision, 1);
    }

    #[test]
    fn reloading_before_start_stages_and_after_start_applies_the_enable() {
        let harness = Harness::new();
        let (module, state) = harness.fake("solo");
        let handle = harness.registry.register(module, FakeConfig::off(1)).expect("register");
        harness.registry.reload(handle, FakeConfig::on(2)).expect("stage");
        let staged = harness.registry.snapshot(handle).expect("snapshot");
        assert_eq!(staged.status, ModuleStatus::Stopped);
        assert!(staged.enabled);
        assert_eq!(staged.revision, 2);
        assert_eq!(lock(&state).starts, 0);

        harness.registry.start_all().expect("start");
        assert_eq!(lock(&state).starts, 1);

        harness.registry.reload(handle, FakeConfig::off(3)).expect("disable");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Disabled
        );
        harness.registry.reload(handle, FakeConfig::on(4)).expect("enable");
        let enabled = harness.registry.snapshot(handle).expect("snapshot");
        assert_eq!(enabled.status, ModuleStatus::Running);
        assert_eq!(enabled.revision, 4);
        assert_eq!(lock(&state).starts, 2);
    }

    #[test]
    fn reloading_with_a_disabled_config_stops_a_running_module() {
        let harness = Harness::new();
        let (module, state) = harness.fake("solo");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        harness.registry.reload(handle, FakeConfig::off(2)).expect("reload");
        let snapshot = harness.registry.snapshot(handle).expect("snapshot");
        assert_eq!(snapshot.status, ModuleStatus::Disabled);
        assert_eq!(snapshot.revision, 2);
        assert_eq!(lock(&state).stops, 1);
    }

    #[test]
    fn a_service_failure_stops_dependents_and_recovery_restarts_them() {
        let harness = Harness::new();
        let (mut provider, provider_state) = harness.fake("provider");
        provider.provides = &PROVIDES_SERVICE;
        let (mut consumer, consumer_state) = harness.fake("consumer");
        consumer.requires = &REQUIRES_SERVICE;
        let provider_handle =
            harness.registry.register(provider, FakeConfig::on(1)).expect("provider");
        let consumer_handle =
            harness.registry.register(consumer, FakeConfig::on(1)).expect("consumer");
        harness.registry.start_all().expect("start");
        harness.registry.service_failed(SERVICE).expect("failure");
        assert_eq!(
            harness.registry.snapshot(consumer_handle).expect("consumer").status,
            ModuleStatus::Unavailable(UnavailableReason::MissingService(SERVICE))
        );
        assert_eq!(lock(&consumer_state).stops, 1);
        assert_eq!(
            harness.registry.snapshot(provider_handle).expect("provider").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&provider_state).stops, 0);
        harness.registry.service_recovered(SERVICE).expect("recovery");
        assert_eq!(
            harness.registry.snapshot(consumer_handle).expect("consumer").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&consumer_state).starts, 2);
    }

    #[test]
    fn snapshots_list_modules_in_registration_order() {
        let harness = Harness::new();
        let (first, _) = harness.fake("first");
        let (second, _) = harness.fake("second");
        harness.registry.register(first, FakeConfig::on(1)).expect("first");
        harness.registry.register(second, FakeConfig::on(1)).expect("second");
        let snapshots = harness.registry.snapshots();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].id, ModuleId::new("first"));
        assert_eq!(snapshots[1].id, ModuleId::new("second"));
        assert_eq!(snapshots[0].revision, 1);
        assert_eq!(snapshots[1].revision, 1);
    }

    #[test]
    fn unknown_handles_are_rejected() {
        let harness = Harness::new();
        let handle: ModuleHandle<FakeConfig> = ModuleHandle {
            id: ModuleId::new("missing"),
            marker: PhantomData,
        };
        assert!(matches!(
            harness.registry.snapshot(handle),
            Err(RegistryError::UnknownModule { .. })
        ));
        assert!(matches!(
            harness.registry.set_enabled(handle, true),
            Err(RegistryError::UnknownModule { .. })
        ));
    }

    #[test]
    fn shutdown_is_idempotent() {
        let harness = Harness::new();
        let (module, state) = harness.fake("solo");
        harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        harness.registry.shutdown().expect("shutdown");
        assert_eq!(lock(&state).stops, 1);
        harness.registry.shutdown().expect("second shutdown");
        assert_eq!(lock(&state).stops, 1);
    }

    #[test]
    fn logs_carry_module_and_operation_and_two_registries_stay_separate() {
        let first = Harness::new();
        let second = Harness::new();
        let (alpha, _) = first.fake("alpha");
        let (beta, _) = second.fake("beta");
        first.registry.register(alpha, FakeConfig::on(1)).expect("alpha");
        second.registry.register(beta, FakeConfig::on(1)).expect("beta");
        first.registry.start_all().expect("first start");
        second.registry.start_all().expect("second start");
        let first_records = first.logger.records();
        let second_records = second.logger.records();
        assert!(!first_records.is_empty());
        assert!(first_records.iter().all(|record| record.module == ModuleId::new("alpha")));
        assert!(first_records.iter().any(|record| record.operation == Operation::Start));
        assert!(second_records.iter().all(|record| record.module == ModuleId::new("beta")));
        assert!(second_records.iter().any(|record| record.operation == Operation::Start));
    }

    #[test]
    fn a_missing_executable_reports_an_actionable_unavailable_reason() {
        let harness = Harness::new();
        let (mut module, state) = harness.fake("needs-tool");
        module.executables = &["stache-missing-binary-fixture"];
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start pass");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Unavailable(UnavailableReason::MissingExecutable(
                "stache-missing-binary-fixture".to_owned()
            ))
        );
        assert_eq!(lock(&state).starts, 0);
    }

    #[test]
    fn a_reload_does_not_clear_a_permission_block() {
        let harness = Harness::new();
        let (mut module, state) = harness.fake("gated");
        module.permissions = &ACCESSIBILITY;
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Denied);
        harness.registry.start_all().expect("start");
        harness.registry.reload(handle, FakeConfig::on(2)).expect("reload");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::WaitingForPermission(Permission::Accessibility)
        );
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Granted);
        harness
            .registry
            .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
            .expect("grant");
        assert_eq!(lock(&state).starts, 1);
    }

    fn caps_module(
        harness: &Harness,
        requirement: PermissionRequirement,
    ) -> (FakeModule, Arc<Mutex<FakeState>>) {
        let (mut module, state) = harness.fake("shared-input");
        module.high_value_requirement = Some(requirement);
        (module, state)
    }

    fn input_requirement() -> PermissionRequirement {
        PermissionRequirement::shared_input(Permission::Accessibility)
    }

    #[test]
    fn kb_05_04_static_descriptor_requirements_are_the_default_and_automatic() {
        let harness = Harness::new();
        let (mut module, _) = harness.fake("static");
        module.permissions = &ACCESSIBILITY;
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        assert_eq!(harness.registry.permission_requirements(), vec![
            PermissionRequirement::automatic(Permission::Accessibility)
        ]);
        harness.registry.pause(handle).expect("pause");
        assert!(harness.registry.permission_requirements().is_empty());
        let (plain, _) = harness.fake("plain");
        harness.registry.register(plain, FakeConfig::on(1)).expect("register");
        assert!(harness.registry.permission_requirements().is_empty());
    }

    #[test]
    fn kb_05_04_ordinary_config_needs_no_permission_and_starts_untrusted() {
        let harness = Harness::new();
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Denied);
        let (module, state) = caps_module(&harness, input_requirement());
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        assert!(harness.registry.permission_requirements().is_empty());
        harness.registry.start_all().expect("start");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&state).starts, 1);
        assert_eq!(harness.permissions.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn kb_05_04_a_required_config_waits_without_trust_and_starts_when_granted() {
        let harness = Harness::new();
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Denied);
        let (module, state) = caps_module(&harness, input_requirement());
        let handle = harness.registry.register(module, FakeConfig::on(100)).expect("register");
        assert_eq!(harness.registry.permission_requirements(), vec![
            input_requirement()
        ]);
        harness.registry.start_all().expect("start");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::WaitingForPermission(Permission::Accessibility)
        );
        assert_eq!(lock(&state).starts, 0);
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Granted);
        harness
            .registry
            .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
            .expect("grant");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        assert_eq!(harness.permissions.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn kb_05_04_ordinary_to_required_without_trust_is_an_accepted_waiting_state() {
        let harness = Harness::new();
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Denied);
        let (module, state) = caps_module(&harness, input_requirement());
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        let report = harness.registry.reload(handle, FakeConfig::on(100)).expect("reload");
        assert_eq!(report.status, ReloadStatus::Applied { revision: 2 });
        assert!(report.start_error.is_none());
        let snapshot = harness.registry.snapshot(handle).expect("snapshot");
        assert_eq!(
            snapshot.status,
            ModuleStatus::WaitingForPermission(Permission::Accessibility)
        );
        assert_eq!(snapshot.revision, 2);
        assert!(snapshot.diagnostic.is_none());
        // The runtime was stopped and never asked to adopt the candidate.
        assert_eq!(lock(&state).stops, 1);
        assert!(!harness.trace.events().iter().any(|entry| entry.contains("reconfigure")));
        assert_eq!(harness.registry.permission_requirements(), vec![
            input_requirement()
        ]);
        // Grant starts the module with the accepted candidate.
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Granted);
        harness
            .registry
            .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
            .expect("grant");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        assert!(harness.trace.events().iter().any(|entry| entry.ends_with("configure.100")));
        assert_eq!(harness.permissions.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn kb_05_04_ordinary_to_required_with_trust_reconfigures_in_place() {
        let harness = Harness::new();
        let (module, state) = caps_module(&harness, input_requirement());
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        harness.registry.reload(handle, FakeConfig::on(100)).expect("reload");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&state).stops, 0);
        assert!(harness.trace.events().iter().any(|entry| entry.ends_with("reconfigure.100")));
    }

    #[test]
    fn kb_05_04_required_to_none_leaves_waiting_and_starts() {
        let harness = Harness::new();
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Denied);
        let (module, state) = caps_module(&harness, input_requirement());
        let handle = harness.registry.register(module, FakeConfig::on(100)).expect("register");
        harness.registry.start_all().expect("start");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::WaitingForPermission(Permission::Accessibility)
        );
        // Still requiring it keeps the wait.
        harness.registry.reload(handle, FakeConfig::on(101)).expect("reload");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::WaitingForPermission(Permission::Accessibility)
        );
        harness.registry.reload(handle, FakeConfig::on(2)).expect("reload");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&state).starts, 1);
        assert!(harness.registry.permission_requirements().is_empty());
        assert_eq!(harness.permissions.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn kb_05_04_revocation_stops_a_required_module_and_ignores_an_ordinary_one() {
        let harness = Harness::new();
        let (module, state) = caps_module(&harness, input_requirement());
        let handle = harness.registry.register(module, FakeConfig::on(100)).expect("register");
        harness.registry.start_all().expect("start");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Denied);
        harness
            .registry
            .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
            .expect("revoke");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::WaitingForPermission(Permission::Accessibility)
        );
        assert_eq!(lock(&state).stops, 1);
        // An ordinary config is unaffected by the same revocation.
        harness.registry.reload(handle, FakeConfig::on(3)).expect("reload");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        harness
            .registry
            .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
            .expect("revoke");
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
    }

    #[test]
    fn kb_05_04_a_trusted_but_failed_start_is_failed_not_waiting() {
        let harness = Harness::new();
        let (mut module, _) = caps_module(&harness, input_requirement());
        module.fail.start = true;
        let handle = harness.registry.register(module, FakeConfig::on(100)).expect("register");
        assert!(harness.registry.start_all().is_err());
        assert!(matches!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Failed(_)
        ));
    }

    #[test]
    fn kb_05_04_requirement_merge_prefers_user_only() {
        let harness = Harness::new();
        let (mut automatic, _) = harness.fake("automatic");
        automatic.permissions = &ACCESSIBILITY;
        harness.registry.register(automatic, FakeConfig::on(1)).expect("register");
        let (input, _) = caps_module(&harness, input_requirement());
        harness.registry.register(input, FakeConfig::on(100)).expect("register");
        assert_eq!(harness.registry.permission_requirements(), vec![
            input_requirement()
        ]);
    }

    #[test]
    fn concurrent_toggles_stay_serialized_and_never_double_start() {
        let harness = Harness::new();
        let (module, state) = harness.fake("racy");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        let registry = Arc::new(harness.registry);
        let mut threads = Vec::new();
        for worker in 0..4_u32 {
            let registry = Arc::clone(&registry);
            threads.push(std::thread::spawn(move || {
                for round in 0..50_u32 {
                    let enabled = (worker + round) % 2 == 0;
                    registry.set_enabled(handle, enabled).expect("toggle");
                    if round % 10 == 0 {
                        registry.reload(handle, FakeConfig::on(round + 1)).expect("reload");
                    }
                }
            }));
        }
        for thread in threads {
            thread.join().expect("worker");
        }
        let snapshot = registry.snapshot(handle).expect("snapshot");
        let state = lock(&state);
        assert!(
            !state.double_start,
            "a second start ran without an intervening stop"
        );
        match snapshot.status {
            ModuleStatus::Running => assert_eq!(state.starts, state.stops + 1),
            ModuleStatus::Disabled | ModuleStatus::Stopped => {
                assert_eq!(state.starts, state.stops);
            }
            other => panic!("unexpected final status {other:?}"),
        }
    }

    /// A module is either running with one outstanding start or fully stopped.
    fn assert_balanced(label: &str, status: &ModuleStatus, starts: u32, stops: u32) {
        match status {
            ModuleStatus::Running => assert_eq!(
                starts,
                stops + 1,
                "{label} is running with {starts} starts and {stops} stops"
            ),
            ModuleStatus::Disabled
            | ModuleStatus::Stopped
            | ModuleStatus::Paused
            | ModuleStatus::WaitingForPermission(_)
            | ModuleStatus::Unavailable(_) => assert_eq!(
                starts, stops,
                "{label} is {status:?} with {starts} starts and {stops} stops"
            ),
            ModuleStatus::Failed(_) => panic!("{label} failed: {status:?}"),
        }
    }

    #[test]
    fn a_provider_toggle_racing_a_consumer_toggle_never_double_starts() {
        let harness = Harness::new();
        let (mut provider, provider_state) = harness.fake("provider");
        provider.provides = &PROVIDES_SERVICE;
        let (mut consumer, consumer_state) = harness.fake("consumer");
        consumer.requires = &REQUIRES_SERVICE;
        let provider_handle =
            harness.registry.register(provider, FakeConfig::on(1)).expect("provider");
        let consumer_handle =
            harness.registry.register(consumer, FakeConfig::on(1)).expect("consumer");
        harness.registry.start_all().expect("start");
        let registry = Arc::new(harness.registry);
        let barrier = Arc::new(Barrier::new(2));

        let provider_thread = {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for round in 0..100_u32 {
                    registry.set_enabled(provider_handle, round % 2 == 0).expect("provider toggle");
                }
            })
        };
        let consumer_thread = {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for round in 0..100_u32 {
                    registry.set_enabled(consumer_handle, round % 2 == 0).expect("consumer toggle");
                }
            })
        };
        for thread in [provider_thread, consumer_thread] {
            thread.join().expect("worker");
        }

        let provider_snapshot = registry.snapshot(provider_handle).expect("provider snapshot");
        let consumer_snapshot = registry.snapshot(consumer_handle).expect("consumer snapshot");
        let provider_state = lock(&provider_state);
        let consumer_state = lock(&consumer_state);
        assert!(!provider_state.double_start, "the provider started twice");
        assert!(!consumer_state.double_start, "the consumer started twice");
        assert_balanced(
            "provider",
            &provider_snapshot.status,
            provider_state.starts,
            provider_state.stops,
        );
        assert_balanced(
            "consumer",
            &consumer_snapshot.status,
            consumer_state.starts,
            consumer_state.stops,
        );
    }

    #[test]
    fn shutdown_racing_set_enabled_leaves_no_running_module() {
        let harness = Harness::new();
        let (module, state) = harness.fake("racy-shutdown");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        let registry = Arc::new(harness.registry);
        let barrier = Arc::new(Barrier::new(2));

        let shutdown_thread = {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                registry.shutdown().expect("shutdown");
            })
        };
        let toggle_thread = {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for round in 0..100_u32 {
                    match registry.set_enabled(handle, round % 2 == 0) {
                        Ok(()) | Err(RegistryError::ShuttingDown) => {}
                        Err(error) => panic!("unexpected toggle error: {error}"),
                    }
                }
            })
        };
        for thread in [shutdown_thread, toggle_thread] {
            thread.join().expect("worker");
        }

        let snapshot = registry.snapshot(handle).expect("snapshot");
        assert_ne!(
            snapshot.status,
            ModuleStatus::Running,
            "a module survived shutdown"
        );
        let state = lock(&state);
        assert!(!state.double_start, "the module started twice");
        assert_eq!(
            state.starts, state.stops,
            "shutdown must stop every started module"
        );
    }

    #[test]
    fn a_valid_reload_recovers_from_an_invalid_initial_config() {
        let harness = Harness::new();
        let (module, state) = harness.fake("solo");
        let handle = harness.registry.register(module, FakeConfig::invalid()).expect("register");
        let broken = harness.registry.snapshot(handle).expect("snapshot");
        assert_eq!(broken.revision, 0);
        assert!(broken.diagnostic.is_some());
        assert_eq!(
            harness.registry.reload(handle, FakeConfig::on(3)).expect("reload").status,
            ReloadStatus::Applied { revision: 1 }
        );
        let repaired = harness.registry.snapshot(handle).expect("snapshot");
        assert_eq!(repaired.revision, 1);
        assert!(repaired.diagnostic.is_none());
        assert_eq!(repaired.status, ModuleStatus::Stopped);
        harness.registry.set_enabled(handle, true).expect("enable");
        assert_eq!(lock(&state).starts, 1);
    }

    #[test]
    fn disabling_a_provider_stops_and_restarts_its_consumer() {
        let harness = Harness::new();
        let (mut provider, provider_state) = harness.fake("provider");
        provider.provides = &PROVIDES_SERVICE;
        let (mut consumer, consumer_state) = harness.fake("consumer");
        consumer.requires = &REQUIRES_SERVICE;
        let (independent, independent_state) = harness.fake("independent");
        let provider_handle =
            harness.registry.register(provider, FakeConfig::on(1)).expect("provider");
        let consumer_handle =
            harness.registry.register(consumer, FakeConfig::on(1)).expect("consumer");
        let independent_handle =
            harness.registry.register(independent, FakeConfig::on(1)).expect("independent");
        harness.registry.start_all().expect("start");
        harness.registry.set_enabled(provider_handle, false).expect("disable provider");
        assert_eq!(
            harness.registry.snapshot(consumer_handle).expect("consumer").status,
            ModuleStatus::Unavailable(UnavailableReason::MissingService(SERVICE))
        );
        assert_eq!(lock(&consumer_state).stops, 1);
        assert_eq!(lock(&provider_state).stops, 1);
        assert_eq!(
            harness.registry.snapshot(independent_handle).expect("independent").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&independent_state).stops, 0);
        harness.registry.set_enabled(provider_handle, true).expect("enable provider");
        assert_eq!(
            harness.registry.snapshot(consumer_handle).expect("consumer").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&consumer_state).starts, 2);
    }

    #[test]
    fn pausing_a_provider_stops_and_resumes_its_consumer() {
        let harness = Harness::new();
        let (mut provider, _) = harness.fake("provider");
        provider.provides = &PROVIDES_SERVICE;
        let (mut consumer, consumer_state) = harness.fake("consumer");
        consumer.requires = &REQUIRES_SERVICE;
        let provider_handle =
            harness.registry.register(provider, FakeConfig::on(1)).expect("provider");
        let consumer_handle =
            harness.registry.register(consumer, FakeConfig::on(1)).expect("consumer");
        harness.registry.start_all().expect("start");
        harness.registry.pause(provider_handle).expect("pause provider");
        assert_eq!(
            harness.registry.snapshot(consumer_handle).expect("consumer").status,
            ModuleStatus::Unavailable(UnavailableReason::MissingService(SERVICE))
        );
        assert_eq!(lock(&consumer_state).stops, 1);
        harness.registry.resume(provider_handle).expect("resume provider");
        assert_eq!(
            harness.registry.snapshot(consumer_handle).expect("consumer").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&consumer_state).starts, 2);
    }

    #[test]
    fn a_transitive_dependent_chain_cascades_both_ways() {
        let harness = Harness::new();
        let (mut alpha, alpha_state) = harness.fake("alpha");
        alpha.provides = &PROVIDES_SERVICE;
        let (mut beta, beta_state) = harness.fake("beta");
        beta.provides = &PROVIDES_SERVICE_B;
        beta.requires = &REQUIRES_SERVICE;
        let (mut gamma, gamma_state) = harness.fake("gamma");
        gamma.requires = &REQUIRES_SERVICE_B;
        let alpha_handle = harness.registry.register(alpha, FakeConfig::on(1)).expect("alpha");
        let beta_handle = harness.registry.register(beta, FakeConfig::on(1)).expect("beta");
        let gamma_handle = harness.registry.register(gamma, FakeConfig::on(1)).expect("gamma");
        harness.registry.start_all().expect("start");
        harness.registry.set_enabled(alpha_handle, false).expect("disable alpha");
        assert_eq!(
            harness.registry.snapshot(beta_handle).expect("beta").status,
            ModuleStatus::Unavailable(UnavailableReason::MissingService(SERVICE))
        );
        assert_eq!(
            harness.registry.snapshot(gamma_handle).expect("gamma").status,
            ModuleStatus::Unavailable(UnavailableReason::MissingService(SERVICE_B))
        );
        assert_eq!(lock(&beta_state).stops, 1);
        assert_eq!(lock(&gamma_state).stops, 1);
        harness.registry.set_enabled(alpha_handle, true).expect("enable alpha");
        assert_eq!(
            harness.registry.snapshot(beta_handle).expect("beta").status,
            ModuleStatus::Running
        );
        assert_eq!(
            harness.registry.snapshot(gamma_handle).expect("gamma").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&beta_state).starts, 2);
        assert_eq!(lock(&gamma_state).starts, 2);
        assert_eq!(lock(&alpha_state).starts, 2);
    }

    #[test]
    fn recovery_does_not_start_disabled_or_paused_consumers() {
        let harness = Harness::new();
        let (mut provider, _) = harness.fake("provider");
        provider.provides = &PROVIDES_SERVICE;
        let (mut running, running_state) = harness.fake("running");
        running.requires = &REQUIRES_SERVICE;
        let (mut disabled, disabled_state) = harness.fake("disabled");
        disabled.requires = &REQUIRES_SERVICE;
        let (mut paused, paused_state) = harness.fake("paused");
        paused.requires = &REQUIRES_SERVICE;
        let provider_handle =
            harness.registry.register(provider, FakeConfig::on(1)).expect("provider");
        let running_handle =
            harness.registry.register(running, FakeConfig::on(1)).expect("running");
        let disabled_handle =
            harness.registry.register(disabled, FakeConfig::on(1)).expect("disabled");
        let paused_handle = harness.registry.register(paused, FakeConfig::on(1)).expect("paused");
        harness.registry.start_all().expect("start");
        harness.registry.set_enabled(disabled_handle, false).expect("disable consumer");
        harness.registry.pause(paused_handle).expect("pause consumer");
        harness.registry.set_enabled(provider_handle, false).expect("disable provider");
        harness.registry.set_enabled(provider_handle, true).expect("enable provider");
        assert_eq!(
            harness.registry.snapshot(running_handle).expect("running").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&running_state).starts, 2);
        assert_eq!(
            harness.registry.snapshot(disabled_handle).expect("disabled").status,
            ModuleStatus::Disabled
        );
        assert_eq!(lock(&disabled_state).starts, 1);
        assert_eq!(
            harness.registry.snapshot(paused_handle).expect("paused").status,
            ModuleStatus::Paused
        );
        assert_eq!(lock(&paused_state).starts, 1);
    }

    #[test]
    fn permission_revocation_of_a_provider_stops_its_consumer() {
        let harness = Harness::new();
        let (mut provider, _) = harness.fake("provider");
        provider.provides = &PROVIDES_SERVICE;
        provider.permissions = &ACCESSIBILITY;
        let (mut consumer, consumer_state) = harness.fake("consumer");
        consumer.requires = &REQUIRES_SERVICE;
        let provider_handle =
            harness.registry.register(provider, FakeConfig::on(1)).expect("provider");
        let consumer_handle =
            harness.registry.register(consumer, FakeConfig::on(1)).expect("consumer");
        harness.registry.start_all().expect("start");
        harness
            .registry
            .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
            .expect("revoke");
        assert_eq!(
            harness.registry.snapshot(provider_handle).expect("provider").status,
            ModuleStatus::WaitingForPermission(Permission::Accessibility)
        );
        assert_eq!(
            harness.registry.snapshot(consumer_handle).expect("consumer").status,
            ModuleStatus::Unavailable(UnavailableReason::MissingService(SERVICE))
        );
        assert_eq!(lock(&consumer_state).stops, 1);
    }

    #[test]
    fn a_cleanup_stop_failure_does_not_mask_the_start_error() {
        let harness = Harness::new();
        let (mut module, state) = harness.fake("stuck");
        module.fail.start = true;
        module.fail.stop = true;
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        let error = harness.registry.start_all().expect_err("start fails");
        match error {
            RegistryError::Module(module_error) => {
                assert_eq!(module_error.operation(), Operation::Start);
            }
            other => panic!("expected a module error, got {other}"),
        }
        match harness.registry.snapshot(handle).expect("snapshot").status {
            ModuleStatus::Failed(failure) => assert_eq!(failure.operation, Operation::Start),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(lock(&state).stops, 1);
    }

    #[test]
    fn repeating_a_failed_disable_or_pause_retries_the_stop() {
        let harness = Harness::new();
        let (mut disabled, disabled_state) = harness.fake("failed-disable");
        disabled.fail.stop = true;
        let disabled = harness.registry.register(disabled, FakeConfig::on(1)).expect("register");
        let (mut paused, paused_state) = harness.fake("failed-pause");
        paused.fail.stop = true;
        let paused = harness.registry.register(paused, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start_all");

        harness.registry.set_enabled(disabled, false).expect_err("stop fails");
        harness.registry.set_enabled(disabled, false).expect_err("the repeat retries");
        assert_eq!(lock(&disabled_state).stops, 2);
        assert_eq!(lock(&disabled_state).starts, 1, "a retry starts nothing");

        harness.registry.pause(paused).expect_err("stop fails");
        harness.registry.pause(paused).expect_err("the repeat retries");
        assert_eq!(lock(&paused_state).stops, 2);
        assert_eq!(lock(&paused_state).starts, 1, "a retry starts nothing");
    }

    #[test]
    fn repeating_a_clean_disable_or_pause_does_not_stop_again() {
        let harness = Harness::new();
        let (disabled, disabled_state) = harness.fake("clean-disable");
        let disabled = harness.registry.register(disabled, FakeConfig::on(1)).expect("register");
        let (paused, paused_state) = harness.fake("clean-pause");
        let paused = harness.registry.register(paused, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start_all");

        harness.registry.set_enabled(disabled, false).expect("disable");
        harness.registry.set_enabled(disabled, false).expect("repeat");
        assert_eq!(lock(&disabled_state).stops, 1);

        harness.registry.pause(paused).expect("pause");
        harness.registry.pause(paused).expect("repeat");
        assert_eq!(lock(&paused_state).stops, 1);
    }

    #[test]
    fn a_reload_that_disables_a_provider_stops_it_without_reconfiguring() {
        let harness = Harness::new();
        let (mut provider, provider_state) = harness.fake("provider");
        provider.provides = &PROVIDES_SERVICE;
        let (mut consumer, consumer_state) = harness.fake("consumer");
        consumer.requires = &REQUIRES_SERVICE;
        let provider_handle =
            harness.registry.register(provider, FakeConfig::on(1)).expect("provider");
        let consumer_handle =
            harness.registry.register(consumer, FakeConfig::on(1)).expect("consumer");
        harness.registry.start_all().expect("start");
        harness
            .registry
            .reload(provider_handle, FakeConfig::off(2))
            .expect("disable reload");
        assert_eq!(
            harness.registry.snapshot(provider_handle).expect("provider").status,
            ModuleStatus::Disabled
        );
        assert_eq!(
            harness.registry.snapshot(consumer_handle).expect("consumer").status,
            ModuleStatus::Unavailable(UnavailableReason::MissingService(SERVICE))
        );
        assert_eq!(lock(&provider_state).stops, 1);
        assert_eq!(lock(&consumer_state).stops, 1);
        assert!(
            !harness.trace.events().iter().any(|event| event == "provider.reconfigure.2"),
            "a disabling reload must stop, not reconfigure"
        );
    }

    #[test]
    fn status_reads_do_not_wait_for_a_blocked_configure() {
        let harness = Harness::new();
        let (module, _state, entered, release) = harness.gated("solo", Blocked::Configure);
        let handle = harness.registry.register(module, FakeConfig::off(0)).expect("register");
        let registry = Arc::new(harness.registry);

        let starter = {
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || {
                registry.set_enabled(handle, true).expect("enable");
            })
        };
        wait_until_blocked(&entered);

        let snapshot = {
            let registry = Arc::clone(&registry);
            within("snapshot", move || registry.snapshot(handle).expect("snapshot"))
        };
        assert_eq!(
            snapshot.status,
            ModuleStatus::Disabled,
            "the published status is the last completed one"
        );
        assert!(
            snapshot.enabled,
            "the desired flag is published before configure runs"
        );
        assert_eq!(snapshot.revision, 1);

        let listed = {
            let registry = Arc::clone(&registry);
            within("snapshots", move || registry.snapshots())
        };
        assert_eq!(listed[0].status, ModuleStatus::Disabled);
        assert!(listed[0].enabled);
        assert_eq!(listed[0].revision, 1);

        release.send(()).expect("release configure");
        starter.join().expect("start thread");
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
    }

    #[test]
    fn status_reads_do_not_wait_for_a_blocked_start() {
        let harness = Harness::new();
        let (module, _state, entered, release) = harness.gated("solo", Blocked::Start);
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        let registry = Arc::new(harness.registry);

        let starter = {
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || {
                registry.start_all().expect("start");
            })
        };
        wait_until_blocked(&entered);

        let snapshot = {
            let registry = Arc::clone(&registry);
            within("snapshot", move || registry.snapshot(handle).expect("snapshot"))
        };
        assert_eq!(snapshot.status, ModuleStatus::Stopped);
        assert!(snapshot.enabled);

        let listed = {
            let registry = Arc::clone(&registry);
            within("snapshots", move || registry.snapshots())
        };
        assert_eq!(listed[0].status, ModuleStatus::Stopped);
        assert!(listed[0].enabled);

        release.send(()).expect("release start");
        starter.join().expect("start thread");
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
    }

    #[test]
    fn status_reads_do_not_wait_for_a_blocked_stop_and_readiness_drops_first() {
        let harness = Harness::new();
        let (mut provider, provider_state, entered, release) =
            harness.gated("provider", Blocked::Stop);
        provider.provides = &PROVIDES_SERVICE;
        let (mut consumer, consumer_state) = harness.fake("consumer");
        consumer.requires = &REQUIRES_SERVICE;
        let provider_handle =
            harness.registry.register(provider, FakeConfig::on(1)).expect("provider");
        let consumer_handle =
            harness.registry.register(consumer, FakeConfig::off(1)).expect("consumer");
        harness.registry.start_all().expect("start");
        let registry = Arc::new(harness.registry);

        let stopper = {
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || {
                registry.set_enabled(provider_handle, false).expect("disable");
            })
        };
        wait_until_blocked(&entered);

        let snapshot = {
            let registry = Arc::clone(&registry);
            within("snapshot", move || {
                registry.snapshot(provider_handle).expect("snapshot")
            })
        };
        assert_eq!(
            snapshot.status,
            ModuleStatus::Disabled,
            "the non-running target is published before `stop` runs"
        );
        assert!(!snapshot.enabled);
        assert_eq!(lock(&provider_state).stops, 0);

        let listed = {
            let registry = Arc::clone(&registry);
            within("snapshots", move || registry.snapshots())
        };
        assert_eq!(listed[0].status, ModuleStatus::Disabled);

        within("dependent start", {
            let registry = Arc::clone(&registry);
            move || registry.set_enabled(consumer_handle, true).expect("consumer start")
        });
        let consumer_snapshot = {
            let registry = Arc::clone(&registry);
            within("consumer snapshot", move || {
                registry.snapshot(consumer_handle).expect("consumer snapshot")
            })
        };
        assert_eq!(
            consumer_snapshot.status,
            ModuleStatus::Unavailable(UnavailableReason::MissingService(SERVICE))
        );
        assert_eq!(
            lock(&consumer_state).starts,
            0,
            "the consumer never starts against a stopping provider"
        );

        release.send(()).expect("release stop");
        stopper.join().expect("stop thread");
        assert_eq!(lock(&provider_state).stops, 1);

        registry.set_enabled(provider_handle, true).expect("provider recovery");
        assert_eq!(
            registry.snapshot(consumer_handle).expect("consumer").status,
            ModuleStatus::Running
        );
        assert_eq!(lock(&consumer_state).starts, 1);
    }

    #[test]
    fn status_reads_do_not_wait_for_a_blocked_reconfigure_and_revision_publishes_after() {
        let harness = Harness::new();
        let (module, _state, entered, release) = harness.gated("solo", Blocked::Reconfigure);
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        let registry = Arc::new(harness.registry);

        let reloader = {
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || {
                registry.reload(handle, FakeConfig::on(2)).expect("reload");
            })
        };
        wait_until_blocked(&entered);

        let snapshot = {
            let registry = Arc::clone(&registry);
            within("snapshot", move || registry.snapshot(handle).expect("snapshot"))
        };
        assert_eq!(snapshot.status, ModuleStatus::Running);
        assert_eq!(
            snapshot.revision, 1,
            "the old revision stays visible during reconfigure"
        );

        let listed = {
            let registry = Arc::clone(&registry);
            within("snapshots", move || registry.snapshots())
        };
        assert_eq!(listed[0].status, ModuleStatus::Running);
        assert_eq!(listed[0].revision, 1);

        release.send(()).expect("release reconfigure");
        reloader.join().expect("reload thread");
        let updated = registry.snapshot(handle).expect("snapshot");
        assert_eq!(updated.status, ModuleStatus::Running);
        assert_eq!(updated.revision, 2);
        assert!(updated.diagnostic.is_none());
    }

    #[test]
    fn snapshots_publish_config_diagnostics_and_recover_after_a_valid_reload() {
        let harness = Harness::new();
        let (module, _) = harness.fake("solo");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");

        harness.registry.reload(handle, FakeConfig::invalid()).expect_err("invalid");
        let rejected = harness.registry.snapshots().remove(0);
        assert_eq!(rejected.status, ModuleStatus::Running);
        assert_eq!(rejected.revision, 1);
        assert_eq!(
            rejected.diagnostic.expect("diagnostic").message,
            "fake config is invalid"
        );

        harness.registry.reload(handle, FakeConfig::on(2)).expect("valid");
        let accepted = harness.registry.snapshots().remove(0);
        assert_eq!(accepted.status, ModuleStatus::Running);
        assert_eq!(accepted.revision, 2);
        assert!(accepted.diagnostic.is_none());
    }

    #[test]
    fn shutdown_publishes_non_running_state_before_stopping_the_module() {
        let harness = Harness::new();
        let (module, _state, entered, release) = harness.gated("solo", Blocked::Stop);
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        let registry = Arc::new(harness.registry);

        let stopper = {
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || {
                registry.shutdown().expect("shutdown");
            })
        };
        wait_until_blocked(&entered);

        let snapshot = {
            let registry = Arc::clone(&registry);
            within("snapshot", move || registry.snapshot(handle).expect("snapshot"))
        };
        assert_eq!(snapshot.status, ModuleStatus::Stopped);
        assert!(snapshot.enabled);

        release.send(()).expect("release stop");
        stopper.join().expect("shutdown thread");
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Stopped
        );
    }

    #[test]
    fn status_reads_do_not_wait_for_a_worker_join() {
        let harness = Harness::new();
        let (started, started_rx) = mpsc::channel();
        let (stop_returned, stop_returned_rx) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let module = JoinModule {
            id: ModuleId::new("join"),
            started,
            stopped: stop_returned,
            release: Some(release_rx),
        };
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        started_rx.recv_timeout(Duration::from_secs(5)).expect("worker started");
        let registry = Arc::new(harness.registry);

        let disable_thread = {
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || {
                registry.set_enabled(handle, false).expect("disable");
            })
        };
        stop_returned_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("stop returned before the join");

        let snapshot = {
            let registry = Arc::clone(&registry);
            within("snapshot", move || registry.snapshot(handle).expect("snapshot"))
        };
        assert_eq!(snapshot.status, ModuleStatus::Disabled);
        let listed = {
            let registry = Arc::clone(&registry);
            within("snapshots", move || registry.snapshots())
        };
        assert_eq!(listed[0].status, ModuleStatus::Disabled);

        release.send(()).expect("release worker");
        disable_thread.join().expect("stop thread");
    }

    #[test]
    fn the_invalidation_handle_and_stop_target_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ModuleInvalidation>();
        assert_send_sync::<StopTarget>();
    }

    #[test]
    fn invalidation_runs_each_listener_exactly_once_and_never_under_the_listener_lock() {
        let invalidation = ModuleInvalidation::new();
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let first_seen = Arc::clone(&first);
        invalidation.on_invalidate(Arc::new(move || {
            first_seen.fetch_add(1, Ordering::SeqCst);
        }));
        let second_seen = Arc::clone(&second);
        let reenter = invalidation.clone();
        invalidation.on_invalidate(Arc::new(move || {
            // Re-entering the handle would deadlock if the list lock were held
            // while listeners ran.
            reenter.on_invalidate(Arc::new(|| {}));
            second_seen.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(!invalidation.is_invalidated());
        invalidation.invalidate();
        invalidation.invalidate();
        assert!(invalidation.is_invalidated());
        assert_eq!(first.load(Ordering::SeqCst), 1);
        assert_eq!(second.load(Ordering::SeqCst), 1);

        let late = Arc::new(AtomicUsize::new(0));
        let late_seen = Arc::clone(&late);
        invalidation.on_invalidate(Arc::new(move || {
            late_seen.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(
            late.load(Ordering::SeqCst),
            1,
            "a late registration runs immediately"
        );
    }

    #[test]
    fn a_panicking_listener_does_not_stop_the_others() {
        let invalidation = ModuleInvalidation::new();
        invalidation.on_invalidate(Arc::new(|| panic!("expected test panic")));
        let ran = Arc::new(AtomicBool::new(false));
        let ran_in_listener = Arc::clone(&ran);
        invalidation.on_invalidate(Arc::new(move || {
            ran_in_listener.store(true, Ordering::SeqCst);
        }));
        invalidation.invalidate();
        assert!(ran.load(Ordering::SeqCst));
        assert!(invalidation.is_invalidated());
    }

    #[test]
    fn invalidation_fires_while_a_start_holds_lifecycle_and_the_slot_lock() {
        let harness = Harness::new();
        let (module, state, entered, release) = harness.gated("blocked", Blocked::Start);
        let invalidation = Arc::clone(&module.invalidation);
        let hits = Arc::clone(&module.invalidation_hits);
        let native = Arc::clone(&module.native);
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        let registry = Arc::new(harness.registry);

        let starter = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.start_all())
        };
        wait_until_blocked(&entered);

        // `start_all` holds `lifecycle` and this start holds the slot lock.
        registry.invalidate();
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "the handle fires despite both locks"
        );
        let captured = lock(&invalidation).clone().expect("the start captured the handle");
        assert!(captured.is_invalidated());
        assert_eq!(*lock(&native), vec!["start"]);

        // Queued native work rechecks the handle at issuance.
        issue_native(&invalidation, &native, "queued");
        assert_eq!(
            *lock(&native),
            vec!["start"],
            "queued work must not issue after invalidation"
        );

        release.send(()).expect("release start");
        let result = starter.join().expect("start thread");
        assert!(matches!(result, Err(RegistryError::ShuttingDown)));
        assert_eq!(
            registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Stopped
        );
        assert_eq!(lock(&state).stops, 1, "a late start releases its runtime inline");
    }

    #[test]
    fn a_committed_start_blocked_at_the_latch_is_refused_and_releases_its_runtime() {
        let harness = Harness::new();
        let (module, state, entered, release) = harness.gated("late-start", Blocked::Start);
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        let registry = Arc::new(harness.registry);

        let starter = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.set_enabled(handle, true))
        };
        wait_until_blocked(&entered);
        registry.invalidate();
        release.send(()).expect("release start");

        let result = starter.join().expect("start thread");
        assert!(matches!(result, Err(RegistryError::ShuttingDown)));
        let snapshot = registry.snapshot(handle).expect("snapshot");
        assert_eq!(snapshot.status, ModuleStatus::Stopped);
        assert_eq!(lock(&state).starts, 1);
        assert_eq!(lock(&state).stops, 1, "the refused start quiesced inline");
        assert!(
            registry.shutdown_targets().is_empty(),
            "no target survived the refusal"
        );
    }

    #[test]
    fn registration_racing_shutdown_is_refused_and_existing_handles_are_invalidated() {
        let harness = Harness::new();
        let (module, _state) = harness.fake("existing");
        let invalidation = Arc::clone(&module.invalidation);
        let hits = Arc::clone(&module.invalidation_hits);
        harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        let existing = lock(&invalidation).clone().expect("the start captured the handle");

        let (descriptor_entered, descriptor_rx) = mpsc::channel();
        let (mut racing, _) = harness.fake("racing");
        racing.descriptor_entered = Some(descriptor_entered);
        let registry = Arc::new(harness.registry);

        // Holding the slots lock parks the registration after its entry check
        // and lets the latch be set before it can push.
        let slots = lock(&registry.slots);
        let racer = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.register(racing, FakeConfig::on(1)))
        };
        descriptor_rx.recv_timeout(Duration::from_secs(5)).expect("descriptor read");
        let invalidator = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.invalidate())
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !registry.shutting_down.load(Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline, "the latch was never set");
            thread::yield_now();
        }
        drop(slots);
        invalidator.join().expect("invalidator");

        assert!(matches!(
            racer.join().expect("registration"),
            Err(RegistryError::ShuttingDown)
        ));
        assert_eq!(registry.snapshots().len(), 1, "the racing slot was removed");
        assert!(existing.is_invalidated());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn lifecycle_and_registration_are_refused_after_invalidation() {
        let harness = Harness::new();
        let (module, state) = harness.fake("solo");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        let (paused, _) = harness.fake("paused");
        let paused_handle = harness.registry.register(paused, FakeConfig::on(1)).expect("paused");
        let (staged, _) = harness.fake("staged");
        let staged_handle = harness.registry.register(staged, FakeConfig::off(1)).expect("staged");
        harness.registry.start_all().expect("start");
        harness.registry.pause(paused_handle).expect("pause");

        harness.registry.invalidate();

        assert!(matches!(
            harness.registry.set_enabled(handle, true),
            Err(RegistryError::ShuttingDown)
        ));
        assert!(matches!(
            harness.registry.set_enabled(handle, false),
            Err(RegistryError::ShuttingDown)
        ));
        assert!(matches!(
            harness.registry.pause(paused_handle),
            Err(RegistryError::ShuttingDown)
        ));
        assert!(matches!(
            harness.registry.resume(paused_handle),
            Err(RegistryError::ShuttingDown)
        ));
        assert!(matches!(
            harness.registry.reload(handle, FakeConfig::on(2)),
            Err(RegistryError::ShuttingDown)
        ));
        assert!(matches!(
            harness.registry.reload(staged_handle, FakeConfig::on(2)),
            Err(RegistryError::ShuttingDown)
        ));
        assert!(matches!(
            harness.registry.start_all(),
            Err(RegistryError::ShuttingDown)
        ));
        assert!(matches!(
            harness.registry.service_failed(SERVICE),
            Err(RegistryError::ShuttingDown)
        ));
        assert!(matches!(
            harness.registry.service_recovered(SERVICE),
            Err(RegistryError::ShuttingDown)
        ));
        assert!(matches!(
            harness
                .registry
                .on_permission_change(Permission::Accessibility, PermissionStatus::Granted),
            Err(RegistryError::ShuttingDown)
        ));
        let (late, _) = harness.fake("late");
        assert!(matches!(
            harness.registry.register(late, FakeConfig::on(1)),
            Err(RegistryError::ShuttingDown)
        ));

        // Nothing was mutated by the refused calls.
        assert_eq!(
            harness.registry.snapshot(handle).expect("snapshot").status,
            ModuleStatus::Running
        );
        assert_eq!(
            harness.registry.snapshot(paused_handle).expect("paused").status,
            ModuleStatus::Paused
        );
        assert_eq!(
            harness.registry.snapshot(staged_handle).expect("staged").status,
            ModuleStatus::Disabled
        );
        assert_eq!(lock(&state).starts, 1);
        assert_eq!(lock(&state).stops, 0);
    }

    #[test]
    fn targets_are_reverse_start_order_and_a_failing_stop_does_not_stop_the_next() {
        let harness = Harness::new();
        let (first, first_state) = harness.fake("first");
        let (mut broken, _) = harness.fake("broken");
        broken.fail.stop = true;
        let (last, last_state) = harness.fake("last");
        harness.registry.register(first, FakeConfig::on(1)).expect("first");
        harness.registry.register(broken, FakeConfig::on(1)).expect("broken");
        harness.registry.register(last, FakeConfig::on(1)).expect("last");
        harness.registry.start_all().expect("start");

        let targets = harness.registry.shutdown_targets();
        let ids: Vec<ModuleId> = targets.iter().map(|target| target.id).collect();
        assert_eq!(ids, vec![
            ModuleId::new("last"),
            ModuleId::new("broken"),
            ModuleId::new("first")
        ]);
        assert!(targets.windows(2).all(|pair| pair[0].sequence > pair[1].sequence));

        let error = harness.registry.shutdown().expect_err("the failing stop is reported");
        assert!(matches!(error, RegistryError::Module(_)));
        let stops: Vec<String> = harness
            .trace
            .events()
            .into_iter()
            .filter(|event| event.ends_with("stop"))
            .collect();
        assert_eq!(stops, vec!["last.stop", "broken.stop", "first.stop"]);
        assert_eq!(lock(&last_state).stops, 1);
        assert_eq!(lock(&first_state).stops, 1);
        // The failing slot stays a target so a retry can name it.
        let remaining = harness.registry.shutdown_targets();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, ModuleId::new("broken"));
    }

    #[test]
    fn a_stopped_slot_is_omitted_and_stop_module_is_idempotent() {
        let harness = Harness::new();
        let (first, first_state) = harness.fake("first");
        let (second, second_state) = harness.fake("second");
        let first_handle = harness.registry.register(first, FakeConfig::on(1)).expect("first");
        harness.registry.register(second, FakeConfig::on(1)).expect("second");
        harness.registry.start_all().expect("start");
        harness.registry.pause(first_handle).expect("pause");

        let targets = harness.registry.shutdown_targets();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].id, ModuleId::new("second"));

        let stopped = StopTarget {
            id: ModuleId::new("first"),
            sequence: 0,
        };
        harness.registry.stop_module(&stopped).expect("idempotent stop");
        assert_eq!(lock(&first_state).stops, 1);
        assert_eq!(lock(&second_state).stops, 0);
    }

    #[test]
    fn targets_return_during_a_blocked_start_and_the_stop_waits_for_the_slot_lock() {
        let harness = Harness::new();
        let (module, state, entered, release) = harness.gated("blocked", Blocked::Start);
        harness.registry.register(module, FakeConfig::on(1)).expect("register");
        let registry = Arc::new(harness.registry);

        let starter = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.start_all())
        };
        wait_until_blocked(&entered);

        let targets = {
            let registry = Arc::clone(&registry);
            within("shutdown_targets", move || registry.shutdown_targets())
        };
        assert_eq!(targets.len(), 1, "metadata is available during a blocked start");
        let target = targets[0];

        let (result, result_rx) = mpsc::channel();
        let stopper = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || {
                let outcome = registry.stop_module(&target);
                let _ = result.send(outcome);
            })
        };
        assert_eq!(
            lock(&state).stops,
            0,
            "module code must not run during the start"
        );
        assert!(
            result_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "the stop must wait for the slot lock instead of refusing"
        );

        release.send(()).expect("release start");
        starter.join().expect("start thread").expect("start completes");
        result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("stop result")
            .expect("the stop succeeds");
        stopper.join().expect("stop thread");
        assert_eq!(lock(&state).stops, 1, "the waiting stop performed the stop");
        assert!(registry.shutdown_targets().is_empty());
    }

    // ---- Accessibility launch gate part 01: baseline requirements ----

    fn baseline() -> Vec<PermissionRequirement> {
        vec![PermissionRequirement::user_only(Permission::Accessibility)]
    }

    fn baseline_harness(requirements: Vec<PermissionRequirement>) -> Harness {
        let mut harness = Harness::new();
        harness.registry =
            harness.registry.with_baseline_requirements(requirements).expect("baseline");
        harness
    }

    fn waiting() -> ModuleStatus { ModuleStatus::WaitingForPermission(Permission::Accessibility) }

    fn status_of(harness: &Harness, handle: ModuleHandle<FakeConfig>) -> ModuleStatus {
        harness.registry.snapshot(handle).expect("snapshot").status
    }

    fn grant(harness: &Harness) {
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Granted);
        harness
            .registry
            .on_permission_change(Permission::Accessibility, PermissionStatus::Granted)
            .expect("grant");
    }

    fn position(events: &[String], needle: &str) -> usize {
        events
            .iter()
            .position(|e| e == needle)
            .unwrap_or_else(|| panic!("{needle} in {events:?}"))
    }

    #[test]
    fn ag_01_01_the_default_baseline_is_empty() {
        let harness = Harness::new();
        assert!(harness.registry.permission_requirements().is_empty());
        let harness = baseline_harness(Vec::new());
        let (module, _) = harness.fake("plain");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Denied);
        harness.registry.start_all().expect("start");
        assert_eq!(status_of(&harness, handle), ModuleStatus::Running);
        assert!(harness.registry.permission_requirements().is_empty());
    }

    #[test]
    fn ag_01_02_a_missing_baseline_blocks_every_module_before_any_module_code() {
        let harness = baseline_harness(baseline());
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Denied);
        let (mut provider, provider_state) = harness.fake("provider");
        provider.provides = &PROVIDES_SERVICE;
        let (mut consumer, consumer_state) = harness.fake("consumer");
        consumer.requires = &REQUIRES_SERVICE;
        let (plain, plain_state) = harness.fake("plain");
        let a = harness.registry.register(provider, FakeConfig::on(1)).expect("register");
        let b = harness.registry.register(consumer, FakeConfig::on(1)).expect("register");
        let c = harness.registry.register(plain, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        for handle in [a, b, c] {
            assert_eq!(status_of(&harness, handle), waiting());
        }
        for state in [&provider_state, &consumer_state, &plain_state] {
            assert_eq!(lock(state).starts, 0);
        }
        assert!(harness.trace.events().is_empty(), "no configure or start ran");
        assert_eq!(harness.permissions.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ag_01_03_the_baseline_is_reported_with_disabled_paused_or_no_modules() {
        let expected = baseline();
        let harness = baseline_harness(baseline());
        assert_eq!(
            harness.registry.permission_requirements(),
            expected,
            "zero modules"
        );
        let (one, _) = harness.fake("one");
        let (two, _) = harness.fake("two");
        let off = harness.registry.register(one, FakeConfig::off(1)).expect("register");
        let paused = harness.registry.register(two, FakeConfig::on(1)).expect("register");
        harness.registry.pause(paused).expect("pause");
        assert_eq!(status_of(&harness, off), ModuleStatus::Disabled);
        assert_eq!(
            harness.registry.permission_requirements(),
            expected,
            "disabled and paused"
        );
        assert_eq!(harness.permissions.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ag_01_04_a_grant_starts_waiting_modules_in_dependency_order_and_a_failure_is_isolated() {
        let harness = baseline_harness(baseline());
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Denied);
        // Registered consumer-first to prove the order comes from dependencies.
        let (mut consumer, _) = harness.fake("consumer");
        consumer.requires = &REQUIRES_SERVICE;
        consumer.fail.start = true;
        let (mut provider, _) = harness.fake("provider");
        provider.provides = &PROVIDES_SERVICE;
        let (plain, _) = harness.fake("plain");
        let b = harness.registry.register(consumer, FakeConfig::on(1)).expect("register");
        let a = harness.registry.register(provider, FakeConfig::on(1)).expect("register");
        let c = harness.registry.register(plain, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        assert!(harness.trace.events().is_empty());
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Granted);
        let result = harness
            .registry
            .on_permission_change(Permission::Accessibility, PermissionStatus::Granted);
        assert!(result.is_err(), "the failed start is reported");
        let events = harness.trace.events();
        assert!(position(&events, "provider.start") < position(&events, "consumer.start"));
        assert_eq!(status_of(&harness, a), ModuleStatus::Running);
        assert_eq!(status_of(&harness, c), ModuleStatus::Running);
        assert!(matches!(status_of(&harness, b), ModuleStatus::Failed(_)));
        assert_eq!(harness.permissions.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ag_01_05_revocation_stops_running_modules_in_reverse_order_and_skips_disabled_and_paused() {
        let harness = baseline_harness(baseline());
        let (mut provider, _) = harness.fake("provider");
        provider.provides = &PROVIDES_SERVICE;
        let (mut consumer, _) = harness.fake("consumer");
        consumer.requires = &REQUIRES_SERVICE;
        let (off, off_state) = harness.fake("off");
        let (idle, idle_state) = harness.fake("idle");
        let a = harness.registry.register(provider, FakeConfig::on(1)).expect("register");
        let b = harness.registry.register(consumer, FakeConfig::on(1)).expect("register");
        let off_handle = harness.registry.register(off, FakeConfig::off(1)).expect("register");
        let idle_handle = harness.registry.register(idle, FakeConfig::on(1)).expect("register");
        harness.registry.pause(idle_handle).expect("pause");
        harness.registry.start_all().expect("start");
        assert_eq!(status_of(&harness, a), ModuleStatus::Running);
        assert_eq!(status_of(&harness, b), ModuleStatus::Running);
        let before = harness.trace.events();
        assert!(position(&before, "provider.start") < position(&before, "consumer.start"));

        harness.permissions.set(Permission::Accessibility, PermissionStatus::Denied);
        harness
            .registry
            .on_permission_change(Permission::Accessibility, PermissionStatus::Denied)
            .expect("revoke");
        let events = harness.trace.events();
        assert!(position(&events, "consumer.stop") < position(&events, "provider.stop"));
        assert_eq!(status_of(&harness, a), waiting());
        assert_eq!(status_of(&harness, b), waiting());
        assert_eq!(status_of(&harness, off_handle), ModuleStatus::Disabled);
        assert_eq!(status_of(&harness, idle_handle), ModuleStatus::Paused);
        for state in [&off_state, &idle_state] {
            let state = lock(state);
            assert_eq!((state.starts, state.stops), (0, 0));
        }
        assert_eq!(harness.permissions.requests.load(Ordering::SeqCst), 0);

        // A later grant restarts them again in dependency order.
        grant(&harness);
        let after = harness.trace.events();
        let restarts: Vec<&String> = after[events.len()..].iter().collect();
        let first = restarts.iter().position(|e| e.as_str() == "provider.start").expect("p");
        let second = restarts.iter().position(|e| e.as_str() == "consumer.start").expect("c");
        assert!(first < second);
    }

    #[test]
    fn ag_01_06_baseline_and_module_requirements_merge_to_one_user_only_entry() {
        let harness = baseline_harness(baseline());
        let (module, _) = caps_module(&harness, input_requirement());
        harness.registry.register(module, FakeConfig::on(100)).expect("register");
        assert_eq!(harness.registry.permission_requirements(), baseline());

        let harness = baseline_harness(baseline());
        let (module, _) = caps_module(
            &harness,
            PermissionRequirement::automatic(Permission::Accessibility),
        );
        harness.registry.register(module, FakeConfig::on(100)).expect("register");
        assert_eq!(harness.registry.permission_requirements(), baseline());

        // An automatic baseline yields to a user-only module requirement.
        let harness =
            baseline_harness(vec![PermissionRequirement::automatic(Permission::Accessibility)]);
        let (module, _) = caps_module(&harness, input_requirement());
        harness.registry.register(module, FakeConfig::on(100)).expect("register");
        assert_eq!(harness.registry.permission_requirements(), vec![
            input_requirement()
        ]);
    }

    #[test]
    fn ag_01_07_a_reload_while_the_baseline_is_missing_waits_and_a_grant_uses_the_newest() {
        let harness = baseline_harness(baseline());
        harness.permissions.set(Permission::Accessibility, PermissionStatus::Denied);
        let (module, state) = harness.fake("plain");
        let handle = harness.registry.register(module, FakeConfig::on(1)).expect("register");
        harness.registry.start_all().expect("start");
        assert_eq!(status_of(&harness, handle), waiting());
        let report = harness.registry.reload(handle, FakeConfig::on(2)).expect("reload");
        assert_eq!(report.status, ReloadStatus::Applied { revision: 2 });
        assert!(report.start_error.is_none());
        assert_eq!(status_of(&harness, handle), waiting());
        assert_eq!(lock(&state).starts, 0);
        let newer = harness.registry.reload(handle, FakeConfig::on(3)).expect("reload");
        assert_eq!(newer.status, ReloadStatus::Applied { revision: 3 });
        assert!(harness.trace.events().is_empty());
        grant(&harness);
        assert_eq!(status_of(&harness, handle), ModuleStatus::Running);
        assert_eq!(harness.trace.events(), vec!["plain.configure.3", "plain.start"]);
        assert_eq!(harness.permissions.requests.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn ag_01_08_the_baseline_cannot_be_set_after_a_registration() {
        let harness = Harness::new();
        let (module, _) = harness.fake("plain");
        harness.registry.register(module, FakeConfig::on(1)).expect("register");
        let Err(error) = harness.registry.with_baseline_requirements(baseline()) else {
            panic!("the builder must refuse after registration");
        };
        assert!(matches!(error, RegistryError::BaselineAfterRegistration));
    }
}
