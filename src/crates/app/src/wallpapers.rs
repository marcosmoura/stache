//! The app-owned Wallpapers factory.
//!
//! [`WallpapersFactory`] owns one host-lifetime [`WallpapersService`]. Building
//! the factory is inert: it canonicalizes the configuration directory, creates
//! the guarded native adapter and the service cells, and starts no coordinator
//! thread, session, timer or native call.
//!
//! Registration binds the configured module and installs the module's typed
//! accepted-configuration watch before the registry starts modules, so
//! publication wakes the coordinator and the live rotation timer. The factory
//! also contributes the `wallpapers` route; its `set` method stays available
//! while the module is disabled or paused. Shutdown hooks delegate to the
//! service: begin closes admission and wakes every host wait, finish drains the
//! coordinator and reports whether it and any issued native call returned.

use std::path::PathBuf;
use std::sync::Arc;

use stache_macos::{GuardedDesktopBackend, NativeDesktop};
use stache_sdk::ipc::IpcHandler;
use stache_sdk::shutdown::ShutdownWait;
use stache_sdk::{AppPaths, ConfigStore, MainThreadExecutor, ModuleId, Registry};
use stache_wallpapers::{ServiceError, SourceContext, WallpapersConfig, WallpapersService};

use crate::host::{
    FactoryContext, HostError, ModuleControl, ModuleFactory, register_module_observed,
};

/// Registers the Wallpapers runtime over one host-lifetime service.
pub struct WallpapersFactory {
    service: Arc<WallpapersService>,
}

impl WallpapersFactory {
    /// Builds the production service over the host's identity paths and
    /// main-thread executor.
    ///
    /// Construction reads the configuration directory identity and the injected
    /// home only; it starts no thread and performs no native work.
    ///
    /// # Errors
    /// Returns [`ServiceError`] when the configuration directory cannot be
    /// canonicalized.
    pub fn new(
        paths: AppPaths,
        config_directory: impl Into<PathBuf>,
        home: Option<PathBuf>,
        main_thread: Arc<dyn MainThreadExecutor>,
    ) -> Result<Self, ServiceError> {
        let backend: Arc<dyn GuardedDesktopBackend> = Arc::new(NativeDesktop::new(main_thread));
        let service = WallpapersService::production(
            paths,
            SourceContext::new(config_directory, home),
            backend,
        )?;
        Ok(Self { service })
    }

    /// Wraps an already constructed service.
    ///
    /// This is the injection point for tests and for a composition root that
    /// needs a non-production clock, random source, processor or desktop
    /// backend. It performs no work.
    #[must_use]
    pub const fn with_service(service: Arc<WallpapersService>) -> Self { Self { service } }
}

impl ModuleFactory for WallpapersFactory {
    fn id(&self) -> ModuleId { WallpapersConfig::ID }

    fn register(
        &self,
        registry: &Registry,
        store: &ConfigStore,
    ) -> Result<Arc<dyn ModuleControl>, HostError> {
        let service = Arc::clone(&self.service);
        let module = service.module();
        register_module_observed(registry, store, module, move |registry, handle| {
            service.install_accepted_watch(registry.accepted_watch(handle)?);
            Ok(())
        })
    }

    fn ipc_handlers(&self, _context: &FactoryContext) -> Vec<(&'static str, Arc<dyn IpcHandler>)> {
        vec![(WallpapersConfig::ID.as_str(), Arc::new(self.service.handler()))]
    }

    fn begin_shutdown(&self) { self.service.begin_shutdown(); }

    fn finish_shutdown(&self, wait: &dyn ShutdownWait) -> Result<bool, String> {
        Ok(self.service.finish_shutdown(wait))
    }
}
