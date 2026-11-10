//! The Bar module: the SDK [`Module`] over the host-lifetime [`BarRuntime`].

use std::sync::Arc;
use std::sync::atomic::Ordering;

use stache_sdk::{
    AcceptedWatch, ErrorCategory, Module, ModuleContext, ModuleDescriptor, ModuleError, ModuleId,
    ModuleResult, Operation,
};

use crate::config::BarConfig;
use crate::runtime::{BAR_SURFACE, BarParts, Core, Inner, Run};
use crate::status::BarStatusSource;
use crate::window::{Desired, plan};

/// The optional Keep Awake published-state provider Bar observes.
///
/// Only the identifier string is shared with the provider; this crate does not
/// depend on the Keep Awake crate (checked by `bar_never_depends_on_keep_awake`).
pub const KEEP_AWAKE_PROVIDER: ModuleId = ModuleId::new("keepAwake");

fn error(operation: Operation, category: ErrorCategory, message: impl Into<String>) -> ModuleError {
    ModuleError::new(BarConfig::ID, operation, category, message)
}

/// The host-lifetime Bar service. Cheap to clone; construction is inert.
#[derive(Clone)]
pub struct BarRuntime {
    inner: Arc<Inner>,
}

impl BarRuntime {
    /// Builds the service. Nothing native is touched, no thread is spawned and
    /// no topology is applied until an enabled module starts.
    #[must_use]
    pub fn new(parts: BarParts) -> Self {
        Self {
            inner: Arc::new(Inner::new(parts, BarStatusSource::default())),
        }
    }

    /// The module to register with the registry.
    #[must_use]
    pub fn module(&self) -> BarModule {
        BarModule {
            inner: Arc::clone(&self.inner),
            config: None,
        }
    }

    /// Installs the typed accepted-configuration watch the status reads its
    /// committed revision from.
    pub fn install_accepted_watch(&self, watch: AcceptedWatch<BarConfig>) {
        self.inner.status.install_watch(watch);
    }

    /// The runtime observation.
    #[must_use]
    pub fn status(&self) -> BarStatusSource { self.inner.status.clone() }

    /// Stage 1: refuses new starts and wakes every wait. Never joins.
    pub fn begin_shutdown(&self) {
        self.inner.closing.store(true, Ordering::SeqCst);
        self.inner.shutdown.cancel();
    }

    /// Stage 3: finishes any cleanup a stop left owed. `false` means something
    /// is still owed and stays owned.
    #[must_use]
    pub fn finish_shutdown(&self) -> bool { self.inner.teardown() }
}

/// The Bar [`Module`].
pub struct BarModule {
    inner: Arc<Inner>,
    config: Option<BarConfig>,
}

impl BarModule {
    fn teardown_failed_start(&self, reason: String) -> ModuleError {
        let finished = self.inner.teardown();
        self.inner.status.start_failed(reason.clone(), !finished);
        error(Operation::Start, ErrorCategory::Platform, reason)
    }
}

impl Module for BarModule {
    type Config = BarConfig;

    fn descriptor(&self) -> ModuleDescriptor {
        ModuleDescriptor {
            id: BarConfig::ID,
            provides: &[],
            requires: &[],
            permissions: &[],
            executables: &[],
            surfaces: &[BAR_SURFACE],
            // Keep Awake is an optional published-state provider: its absence,
            // disablement or failure never stops Bar. The identifier is only a
            // name; this crate never imports the Keep Awake crate. The app
            // projects the state into the Bar view.
            observes: &[KEEP_AWAKE_PROVIDER],
            reserves_screen: true,
        }
    }

    fn configure(&mut self, config: &BarConfig, _ctx: &ModuleContext) -> ModuleResult<()> {
        stache_sdk::ModuleConfig::validate(config).map_err(|rejection| {
            error(Operation::Configure, ErrorCategory::Config, rejection.message())
        })?;
        self.config = Some(config.clone());
        Ok(())
    }

    #[allow(clippy::significant_drop_tightening)] // the core lock covers the first converge
    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()> {
        let inner = Arc::clone(&self.inner);
        if inner.closing.load(Ordering::SeqCst) {
            return Err(error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "the host is shutting down",
            ));
        }
        let Some(config) = self.config.clone() else {
            return Err(error(
                Operation::Start,
                ErrorCategory::Lifecycle,
                "not configured",
            ));
        };
        {
            let core = lock(&inner.core);
            if core.run.is_some()
                || core.lingering.is_some()
                || inner.menus_owed()
                || inner.providers_owed()
            {
                return Err(error(
                    Operation::Start,
                    ErrorCategory::Lifecycle,
                    "the previous run still owns resources; stop must finish first",
                ));
            }
        }
        inner.status.starting();
        let reservations = ctx.screen_reservations().map_err(|reason| {
            inner.status.start_failed(reason.to_string(), false);
            error(Operation::Start, ErrorCategory::Lifecycle, reason.to_string())
        })?;
        let lease = inner.parts.topology.begin().map_err(|reason| {
            inner.status.start_failed(reason.clone(), false);
            error(Operation::Start, ErrorCategory::Platform, reason)
        })?;
        let run_id = inner.next_run();
        inner.status.generation(run_id);
        inner.status.accepted(config.height, config.padding);
        let reading = lease.read();
        let seen = reading.revision;
        let converged = {
            let mut core = lock(&inner.core);
            core.config = Some(config.clone());
            let run = core.run.insert(Run::new(run_id, ctx.windows().clone(), reservations, lease));
            inner.converge_fresh(run, &config, &reading)
        };
        if let Err(reason) = converged {
            return Err(self.teardown_failed_start(reason));
        }
        // The page load is observed by Bar's own workers, never here: a page
        // that does not load must not hold the registry's lifecycle gate.
        let panel = Arc::clone(&inner);
        if let Err(spawn) = ctx.spawn_worker("stache-bar-panel", move |token| {
            panel.watch_panel(run_id, &token);
        }) {
            return Err(self.teardown_failed_start(spawn.to_string()));
        }
        let worker = Arc::clone(&inner);
        if let Err(spawn) = ctx.spawn_worker("stache-bar-topology", move |token| {
            worker.follow(run_id, seen, &token);
        }) {
            return Err(self.teardown_failed_start(spawn.to_string()));
        }
        inner.status.running();
        Ok(())
    }

    #[allow(clippy::significant_drop_tightening)] // the core lock covers the converge
    fn reconfigure(&mut self, config: &BarConfig, _ctx: &ModuleContext) -> ModuleResult<()> {
        let inner = Arc::clone(&self.inner);
        let (lease, run_id) = {
            let core = lock(&inner.core);
            let Some(run) = core.run.as_ref() else {
                return Err(error(
                    Operation::Reconfigure,
                    ErrorCategory::Lifecycle,
                    "not started",
                ));
            };
            (Arc::clone(&run.lease), run.id)
        };
        // Preflight with no lock held: a frame that cannot be expressed is
        // rejected before anything changes.
        let reading = lease.read();
        if let Desired::Invalid(reason) = plan(config, reading.layout.as_deref(), reading.primary) {
            return Err(error(Operation::Reconfigure, ErrorCategory::Config, reason));
        }
        {
            let mut core = lock(&inner.core);
            let previous = core.config.clone();
            let Core { config: held, run, .. } = &mut *core;
            let Some(run) = run.as_mut().filter(|run| run.id == run_id) else {
                return Err(error(
                    Operation::Reconfigure,
                    ErrorCategory::Lifecycle,
                    "stopped",
                ));
            };
            match inner.converge_fresh(run, config, &reading) {
                Ok(()) => *held = Some(config.clone()),
                Err(reason) => {
                    // Restore the previous shape; the old frame stays.
                    if let Some(previous) = &previous {
                        let _ = inner.converge_fresh(run, previous, &reading);
                    }
                    return Err(error(Operation::Reconfigure, ErrorCategory::Platform, reason));
                }
            }
        }
        self.inner.status.accepted(config.height, config.padding);
        self.config = Some(config.clone());
        Ok(())
    }

    fn stop(&mut self, _ctx: &ModuleContext) -> ModuleResult<()> {
        self.inner.status.stopping();
        let finished = self.inner.teardown();
        self.inner.status.stopped(!finished);
        if finished {
            Ok(())
        } else {
            Err(error(
                Operation::Stop,
                ErrorCategory::Platform,
                "the display feed or a provider worker has not finished ending; stop retries the exact owner",
            ))
        }
    }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
