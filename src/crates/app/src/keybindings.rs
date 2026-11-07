//! The Keybindings composition root.
//!
//! [`compose`] builds, once per desktop launch, the host-lifetime pieces the
//! Keybindings module needs and nothing else: the bounded dispatcher, the shared
//! input service whose work reservations **are** that dispatcher (so the tap's
//! reservation and the dispatcher's admission are one decision), the router that
//! serves as both the Carbon and the Caps sink, the Carbon control, the Caps
//! owner, and the host's crash-recovery step over the **same** `CapsRemap` the
//! runtime uses. Every piece is inert: no tap, hotkey, remap, worker or process
//! exists until the enabled module starts, and recovery acts only on Next's own
//! journal.

use std::sync::Arc;

use stache_keybindings::caps::{CapsOwner, CapsRecoveryStep};
use stache_keybindings::{
    DiagnosticLog, Dispatcher, KeybindingsParts, KeybindingsRuntime, Router, RuntimeTiming,
};
use stache_macos::MacInputBackend;
use stache_macos::hotkeys::{HotkeyControl, HotkeySink};
use stache_sdk::commands::{CommandEnvironment, CommandExecutor};
use stache_sdk::input::InputService;
use stache_sdk::{AppPaths, MainThreadExecutor};

use crate::caps_recovery::KeybindingsCapsRecovery;
use crate::host::input_priority;

/// What the composition root keeps from [`compose`].
pub struct KeybindingsComposition {
    /// The service the Keybindings factory registers.
    pub runtime: KeybindingsRuntime,
    /// The one shared input service handed to the registry.
    pub input: Arc<InputService>,
    /// The macOS owner behind it, for coordinated teardown.
    pub backend: Arc<MacInputBackend>,
    /// The host startup recovery over the shared remap.
    pub recovery: KeybindingsCapsRecovery,
}

/// Builds the Keybindings pieces over the real native layers. Nothing runs.
#[must_use]
pub fn compose(
    paths: &AppPaths,
    main_thread: Arc<dyn MainThreadExecutor>,
) -> KeybindingsComposition {
    let log = DiagnosticLog::new();
    let executor = CommandExecutor::new(CommandEnvironment::from_env()).with_diagnostics({
        let sink = log.sink();
        move |message| sink(message)
    });
    let dispatcher = Dispatcher::new(executor, log.sink());
    let (input, backend) = MacInputBackend::service(dispatcher.reservations());
    let recovery_step = CapsRecoveryStep::production(paths);
    let remap = recovery_step.remap();
    // Ordered before any module can start: the module's first apply waits for
    // the host's startup recovery to finish.
    remap.begin_startup_recovery();
    let router = Router::new(dispatcher.clone());
    let hotkeys = HotkeyControl::system(
        main_thread,
        Arc::clone(&input),
        input.reservations().new_owner(),
        Arc::clone(&router) as Arc<dyn HotkeySink>,
    );
    let caps = CapsOwner::new(
        Arc::clone(&input),
        Arc::clone(&remap),
        input_priority::KEYBINDINGS,
    );
    let runtime = KeybindingsRuntime::new(KeybindingsParts {
        dispatcher,
        router,
        hotkeys: Arc::new(hotkeys),
        caps: Arc::new(caps),
        remap,
        input: Arc::clone(&input),
        log,
        priority: input_priority::KEYBINDINGS,
        timing: RuntimeTiming::default(),
    });
    KeybindingsComposition {
        runtime,
        input,
        backend,
        recovery: KeybindingsCapsRecovery::new(recovery_step),
    }
}

#[cfg(test)]
mod tests {
    use stache_macos::ManualExecutor;
    use stache_sdk::{AppEnvironment, AppPaths};

    use super::*;

    #[test]
    fn kb_05_01_composition_is_inert_and_shares_one_remap_and_one_dispatcher() {
        let home = tempfile::tempdir().expect("home");
        let temporary = tempfile::tempdir().expect("temporary");
        let environment = AppEnvironment {
            home: Some(home.path().to_path_buf()),
            temporary: Some(temporary.path().to_path_buf()),
        };
        let paths = AppPaths::resolve(&environment).expect("paths");
        let main: Arc<dyn MainThreadExecutor> = Arc::new(ManualExecutor::new(false));
        let composition = compose(&paths, main);
        let status = composition.runtime.snapshot().read();
        assert_eq!(status.revision, "0");
        assert_eq!(status.dispatch.active, 0);
        assert_eq!(status.dispatch.reserved, 0);
        assert!(composition.input.is_available());
        // Nothing was created on disk and no native owner thread started.
        assert!(std::fs::read_dir(home.path()).expect("home").next().is_none());
        assert!(std::fs::read_dir(temporary.path()).expect("temporary").next().is_none());
    }
}
