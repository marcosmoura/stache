use std::sync::Arc;

use parking_lot::Mutex;

use crate::modules::services::lifecycle::{LifecycleModule, ModuleStatus};

/// Fixed collection of all toggleable modules, registered once at startup and
/// never mutated afterwards.
///
/// `toggle` clones the `Arc` out of the guard and drops the lock before
/// calling `pause`/`resume`, so slow OS calls never block concurrent lookups
/// or registrations.
pub struct LifecycleRegistry {
    modules: Mutex<Vec<Arc<dyn LifecycleModule>>>,
}

impl LifecycleRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            modules: Mutex::new(Vec::new()),
        }
    }

    /// Registers a module. Call only during startup, before any reader.
    pub fn register(&self, module: Arc<dyn LifecycleModule>) { self.modules.lock().push(module); }

    /// Snapshot of all registered modules.
    #[must_use]
    pub fn modules(&self) -> Vec<Arc<dyn LifecycleModule>> { self.modules.lock().clone() }

    /// Finds a module by id without calling into it.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Arc<dyn LifecycleModule>> {
        self.modules.lock().iter().find(|m| m.id() == id).cloned()
    }

    /// Toggles a module: pause if running, resume if paused.
    ///
    /// Rejects `ConfiguredOff` and `Unavailable` modules. The lock is dropped
    /// before any OS call.
    ///
    /// # Errors
    ///
    /// Returns an error if the module is missing, not toggleable, or the
    /// pause/resume call fails.
    pub fn toggle(&self, id: &str) -> Result<ModuleStatus, String> {
        let module = self.get(id).ok_or_else(|| format!("unknown module {id:?}"))?;
        match module.status() {
            ModuleStatus::ConfiguredOff => Err(format!("{id} is disabled in config")),
            ModuleStatus::Unavailable(reason) => Err(format!("{id} is unavailable: {reason}")),
            ModuleStatus::Running => {
                module.pause()?;
                Ok(module.status())
            }
            ModuleStatus::Paused => {
                module.resume()?;
                Ok(module.status())
            }
        }
    }
}

impl Default for LifecycleRegistry {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    struct MockModule {
        id: &'static str,
        name: &'static str,
        state: parking_lot::Mutex<ModuleStatus>,
    }

    impl MockModule {
        fn new(id: &'static str, name: &'static str, state: ModuleStatus) -> Arc<Self> {
            Arc::new(Self {
                id,
                name,
                state: parking_lot::Mutex::new(state),
            })
        }
    }

    impl LifecycleModule for MockModule {
        fn name(&self) -> &'static str { self.name }

        fn id(&self) -> &'static str { self.id }

        fn start(&self) -> Result<(), String> {
            *self.state.lock() = ModuleStatus::Running;
            Ok(())
        }

        fn pause(&self) -> Result<(), String> {
            *self.state.lock() = ModuleStatus::Paused;
            Ok(())
        }

        fn resume(&self) -> Result<(), String> {
            *self.state.lock() = ModuleStatus::Running;
            Ok(())
        }

        fn status(&self) -> ModuleStatus { self.state.lock().clone() }
    }

    /// Pause blocks until released, simulating a slow OS call (e.g. tiling teardown).
    struct BlockingPauseModule {
        id: &'static str,
        name: &'static str,
        state: parking_lot::Mutex<ModuleStatus>,
        started: mpsc::Sender<()>,
        release: std::sync::Mutex<mpsc::Receiver<()>>,
    }

    impl LifecycleModule for BlockingPauseModule {
        fn name(&self) -> &'static str { self.name }

        fn id(&self) -> &'static str { self.id }

        fn start(&self) -> Result<(), String> {
            *self.state.lock() = ModuleStatus::Running;
            Ok(())
        }

        fn pause(&self) -> Result<(), String> {
            let _ = self.started.send(());
            let _ = self.release.lock().unwrap().recv();
            *self.state.lock() = ModuleStatus::Paused;
            Ok(())
        }

        fn resume(&self) -> Result<(), String> {
            *self.state.lock() = ModuleStatus::Running;
            Ok(())
        }

        fn status(&self) -> ModuleStatus { self.state.lock().clone() }
    }

    #[test]
    fn register_and_get_by_id() {
        let registry = LifecycleRegistry::new();
        registry.register(MockModule::new("a", "A", ModuleStatus::Running));
        assert!(registry.get("a").is_some());
        assert!(registry.get("missing").is_none());
    }

    #[test]
    fn modules_snapshot_returns_all() {
        let registry = LifecycleRegistry::new();
        registry.register(MockModule::new("a", "A", ModuleStatus::Running));
        registry.register(MockModule::new("b", "B", ModuleStatus::Paused));
        assert_eq!(registry.modules().len(), 2);
    }

    #[test]
    fn toggle_running_pauses_and_returns_paused() {
        let registry = LifecycleRegistry::new();
        registry.register(MockModule::new("a", "A", ModuleStatus::Running));
        assert_eq!(registry.toggle("a").unwrap(), ModuleStatus::Paused);
    }

    #[test]
    fn toggle_paused_resumes_and_returns_running() {
        let registry = LifecycleRegistry::new();
        registry.register(MockModule::new("a", "A", ModuleStatus::Paused));
        assert_eq!(registry.toggle("a").unwrap(), ModuleStatus::Running);
    }

    #[test]
    fn toggle_unknown_id_errors() {
        let registry = LifecycleRegistry::new();
        assert!(registry.toggle("missing").unwrap_err().contains("unknown module"));
    }

    #[test]
    fn toggle_configured_off_errors() {
        let registry = LifecycleRegistry::new();
        registry.register(MockModule::new("a", "A", ModuleStatus::ConfiguredOff));
        assert!(registry.toggle("a").unwrap_err().contains("disabled"));
    }

    #[test]
    fn toggle_unavailable_errors() {
        let registry = LifecycleRegistry::new();
        registry.register(MockModule::new(
            "a",
            "A",
            ModuleStatus::Unavailable("perm".into()),
        ));
        assert!(registry.toggle("a").unwrap_err().contains("unavailable"));
    }

    #[test]
    fn toggle_never_holds_registry_lock_during_os_call() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let registry = Arc::new(LifecycleRegistry::new());
        registry.register(Arc::new(BlockingPauseModule {
            id: "a",
            name: "A",
            state: parking_lot::Mutex::new(ModuleStatus::Running),
            started: started_tx,
            release: std::sync::Mutex::new(release_rx),
        }));

        let registry2 = Arc::clone(&registry);
        let handle = std::thread::spawn(move || {
            registry2.toggle("a").unwrap();
        });

        // pause() is now in flight, blocked on its release channel.
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        // The registry lock must be free while toggle() runs the OS call.
        assert!(registry.get("a").is_some());
        registry.register(MockModule::new("b", "B", ModuleStatus::Paused));
        assert_eq!(registry.modules().len(), 2);

        release_tx.send(()).unwrap();
        handle.join().unwrap();
    }
}
