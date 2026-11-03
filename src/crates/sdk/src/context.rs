//! The capabilities handed to a module during one lifecycle operation.
//!
//! A context exposes only what a module needs: its identity, a scoped logger,
//! the event bus, main-thread dispatch, a spawn-only worker handle, permission
//! checks and the cancellation token for the current runtime. It deliberately
//! exposes neither the registry nor another module's implementation.

use std::sync::Arc;

use crate::error::{ErrorCategory, ModuleError, ModuleResult};
use crate::events::EventBus;
use crate::logging::ScopedLogger;
use crate::module::ModuleId;
use crate::permissions::PermissionProvider;
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
    windows: WindowCapability,
}

/// Host services shared by every context of one registry.
pub(crate) struct ContextServices {
    pub events: Arc<EventBus>,
    pub main_thread: Arc<dyn MainThreadExecutor>,
    pub permissions: Arc<dyn PermissionProvider>,
    pub windows: WindowCapability,
}

impl ModuleContext {
    pub(crate) fn new(
        id: ModuleId,
        logger: ScopedLogger,
        workers: WorkerHandle,
        cancellation: CancellationToken,
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
            windows: services.windows,
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

    /// The module's declared window surfaces and the host's window backend.
    #[must_use]
    pub const fn windows(&self) -> &WindowCapability { &self.windows }
}
