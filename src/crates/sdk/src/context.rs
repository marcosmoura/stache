//! The capabilities handed to a module during one lifecycle operation.
//!
//! A context exposes only what a module needs: its identity, a scoped logger,
//! the event bus, main-thread dispatch, a spawn-only worker handle, permission
//! checks and the cancellation token for the current runtime. It deliberately
//! exposes neither the registry nor another module's implementation.

use std::sync::Arc;

use crate::error::{ErrorCategory, ModuleError, ModuleResult};
use crate::events::EventBus;
use crate::generation::GenerationScope;
use crate::input::InputCapability;
use crate::logging::ScopedLogger;
use crate::module::ModuleId;
use crate::permissions::PermissionProvider;
use crate::published_state::{PublishedState, StateError, StateHub, StateObserver, StatePublisher};
use crate::registry::ModuleInvalidation;
use crate::screen_reservations::{
    ReservationHub, ScreenReservationError, ScreenReservationObserver, ScreenReservations,
};
use crate::threading::{CancellationToken, MainThreadExecutor, WorkerHandle};
use crate::windows::WindowCapability;

/// Host services bound to one module and one lifecycle operation.
pub struct ModuleContext {
    id: ModuleId,
    logger: ScopedLogger,
    events: Arc<EventBus>,
    main_thread: Arc<dyn MainThreadExecutor>,
    workers: WorkerHandle,
    permissions: Arc<dyn PermissionProvider>,
    cancellation: CancellationToken,
    invalidation: ModuleInvalidation,
    windows: WindowCapability,
    input: InputCapability,
    generation: GenerationScope,
    states: Arc<StateHub>,
    observes: &'static [ModuleId],
    reservations: Arc<ReservationHub>,
    reserves_screen: bool,
}

/// Host services shared by every context of one registry.
pub(crate) struct ContextServices {
    pub events: Arc<EventBus>,
    pub main_thread: Arc<dyn MainThreadExecutor>,
    pub permissions: Arc<dyn PermissionProvider>,
    pub windows: WindowCapability,
    pub input: InputCapability,
    pub states: Arc<StateHub>,
    pub observes: &'static [ModuleId],
    pub reservations: Arc<ReservationHub>,
    pub reserves_screen: bool,
}

impl ModuleContext {
    pub(crate) fn new(
        id: ModuleId,
        logger: ScopedLogger,
        workers: WorkerHandle,
        cancellation: CancellationToken,
        invalidation: ModuleInvalidation,
        generation: GenerationScope,
        services: ContextServices,
    ) -> Self {
        Self {
            id,
            logger,
            events: services.events,
            main_thread: services.main_thread,
            workers,
            permissions: services.permissions,
            cancellation,
            invalidation,
            windows: services.windows,
            input: services.input,
            generation,
            states: services.states,
            observes: services.observes,
            reservations: services.reservations,
            reserves_screen: services.reserves_screen,
        }
    }

    /// The module this context belongs to.
    #[must_use]
    pub const fn id(&self) -> ModuleId { self.id }

    /// A logger already scoped to this module and operation.
    #[must_use]
    pub const fn logger(&self) -> &ScopedLogger { &self.logger }

    /// The host event bus. Publishing never blocks or calls subscribers inline.
    #[must_use]
    pub fn events(&self) -> &EventBus { &self.events }

    /// The main-thread dispatcher.
    #[must_use]
    pub fn main_thread(&self) -> &(dyn MainThreadExecutor + 'static) { self.main_thread.as_ref() }

    /// A handle that can spawn named workers into this runtime's group.
    ///
    /// It cannot stop or join the group; the registry does that before
    /// releasing the context.
    #[must_use]
    pub const fn workers(&self) -> &WorkerHandle { &self.workers }

    /// Spawns a named worker, attributing a failure to this module and the
    /// current operation.
    ///
    /// # Errors
    /// Returns a [`ModuleError`] when the runtime is already stopped or the OS
    /// refuses to create the thread.
    pub fn spawn_worker(
        &self,
        name: &str,
        job: impl FnOnce(CancellationToken) + Send + 'static,
    ) -> ModuleResult<()> {
        self.workers.spawn(name, job).map_err(|error| {
            ModuleError::new(
                self.id,
                self.logger.operation(),
                ErrorCategory::Platform,
                format!("failed to spawn worker `{name}`"),
            )
            .with_source(error)
        })
    }

    /// Fresh permission checks and explicit requests.
    #[must_use]
    pub fn permissions(&self) -> &(dyn PermissionProvider + 'static) { self.permissions.as_ref() }

    /// The token cancelled when this runtime stops.
    ///
    /// During `stop` it is already cancelled. Modules observe it; they do not
    /// cancel it themselves.
    #[must_use]
    pub fn cancellation(&self) -> CancellationToken { self.cancellation.clone() }

    /// The logical invalidation handle for this module.
    ///
    /// The registry fires it during stage-1 teardown without taking a
    /// registry, slot or lifecycle lock, including while a start or stop holds
    /// one of those locks. It is not a cleanup path: a module drops queued
    /// native work that has not been issued and lets already-issued work
    /// finish inert. Listeners run at most once; a listener registered after
    /// invalidation runs immediately.
    #[must_use]
    pub const fn invalidation(&self) -> &ModuleInvalidation { &self.invalidation }

    /// The module's declared window surfaces and the host's window backend.
    #[must_use]
    pub const fn windows(&self) -> &WindowCapability { &self.windows }

    /// The module-scoped input capability backed by the host's shared service.
    ///
    /// It is inert (every subscription returns an `Unavailable` error) unless the
    /// host attached a native owner.
    #[must_use]
    pub const fn input(&self) -> &InputCapability { &self.input }

    /// The generation scope of the runtime this context belongs to.
    ///
    /// The registry ends it before any cleanup wait when the runtime stops,
    /// pauses, is disabled or fails. Capabilities that must not outlive their
    /// runtime listen on it.
    #[must_use]
    pub(crate) const fn generation(&self) -> &GenerationScope { &self.generation }

    /// A typed publisher for this module's registered state schema, bound to
    /// this runtime generation.
    ///
    /// # Errors
    /// Returns [`StateError::NotRegistered`] when no schema is registered,
    /// [`StateError::SchemaMismatch`] when `T` is not the schema registered for
    /// this module and [`StateError::Unauthorized`] when `T` is registered for
    /// another provider.
    pub fn state_publisher<T: PublishedState>(&self) -> Result<StatePublisher<T>, StateError> {
        self.states.publisher::<T>(self.id, &self.generation)
    }

    /// A read-only observer of `provider`'s published state.
    ///
    /// The provider must be listed in this module's
    /// [`crate::ModuleDescriptor::observes`]. An unknown or stopped provider is
    /// valid and reads absent or unavailable; observing never changes this
    /// module's lifecycle.
    ///
    /// # Errors
    /// Returns [`StateError::NotObserved`] for an undeclared provider.
    pub fn observe_state(&self, provider: ModuleId) -> Result<StateObserver, StateError> {
        if !self.observes.contains(&provider) {
            return Err(StateError::NotObserved { module: self.id, provider });
        }
        Ok(StateObserver::new(self.states.cell(provider)))
    }

    /// The screen-reservation capability, bound to this runtime generation.
    ///
    /// The module must declare [`crate::ModuleDescriptor::reserves_screen`].
    /// When the generation ends (stop, pause, disable or failure, including a
    /// failed start) the host removes exactly this owner's reservations from
    /// this generation before any cleanup wait; a retained handle can never
    /// touch a successor's set.
    ///
    /// # Errors
    /// Returns [`ScreenReservationError::NotDeclared`] without the declaration.
    pub fn screen_reservations(&self) -> Result<ScreenReservations, ScreenReservationError> {
        if !self.reserves_screen {
            return Err(ScreenReservationError::NotDeclared { module: self.id });
        }
        Ok(ScreenReservations::new(
            Arc::clone(&self.reservations),
            self.id,
            self.generation().clone(),
        ))
    }

    /// A read-only observer of the host's aggregated screen reservations.
    ///
    /// Any module may read; only a declared owner may write. Subscribe before
    /// fetch: `read()` the snapshot, then `wait_for_change` with its revision.
    #[must_use]
    pub fn observe_screen_reservations(&self) -> ScreenReservationObserver {
        ScreenReservationObserver::new(Arc::clone(&self.reservations))
    }
}
