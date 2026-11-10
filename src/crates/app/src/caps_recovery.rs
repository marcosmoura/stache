//! The host's startup recovery of a crash-left Caps Lock remap.
//!
//! The host runs [`KeybindingsCapsRecovery`] once per desktop start through
//! [`crate::host::StartupRecovery`]. It composes the Keybindings-owned
//! [`CapsRecoveryStep`] the same way the production factories compose their
//! modules; the SDK knows no module identity. It runs regardless of the
//! module's enabled state and configuration validity, acts only when Next's own
//! journal exists, and reports a failure instead of retrying or blocking
//! startup. Default and test builds construct it only over fakes.

use stache_keybindings::caps::CapsRecoveryStep;
use stache_sdk::{AppPaths, CancellationToken};

use crate::host::StartupRecovery;

/// The Keybindings Caps recovery as a host startup step.
pub struct KeybindingsCapsRecovery {
    step: CapsRecoveryStep,
}

impl KeybindingsCapsRecovery {
    /// The stable step name.
    pub const NAME: &'static str = "caps-recovery";

    /// A step over an injected recovery. Tests pass a fake-backed one.
    #[must_use]
    pub const fn new(step: CapsRecoveryStep) -> Self { Self { step } }

    /// The production step for this identity: real `hidutil`, the durable
    /// journal and the lease. Creating it runs nothing and creates no path.
    #[must_use]
    pub fn production(paths: &AppPaths) -> Self { Self::new(CapsRecoveryStep::production(paths)) }
}

impl Drop for KeybindingsCapsRecovery {
    /// A step dropped without running (the host could not start its thread,
    /// or never ran it) releases the startup latch so the module's first
    /// Caps apply does not wait for a recovery that will never happen.
    fn drop(&mut self) { self.step.remap().abandon_startup_recovery(); }
}

impl StartupRecovery for KeybindingsCapsRecovery {
    fn name(&self) -> &'static str { Self::NAME }

    fn run(&self, cancel: &CancellationToken) -> Option<String> {
        self.step.run(cancel).diagnostic()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use stache_keybindings::caps::CapsRecoveryStep;
    use stache_macos::caps_remap::testing::World;
    use stache_sdk::CancellationToken;

    use super::*;

    #[test]
    fn a_recovery_dropped_without_running_releases_the_startup_latch() {
        let world = World::new(Vec::new());
        let remap = Arc::new(world.remap().with_startup_bound(Duration::from_secs(60)));
        remap.begin_startup_recovery();
        drop(KeybindingsCapsRecovery::new(CapsRecoveryStep::new(Arc::clone(
            &remap,
        ))));
        let started = Instant::now();
        remap.apply(&CancellationToken::new()).expect("released");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
