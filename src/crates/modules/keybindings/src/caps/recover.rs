//! Host startup recovery of a crash-left Caps remap.
//!
//! A crash while Caps Lock is remapped leaves the global mapping behind and
//! physical Caps Lock sends F18 until something restores it. Recovery needs a
//! trigger that does not depend on the module being enabled, its configuration
//! being valid or the tray existing, so the host runs
//! [`recover_stale_caps_remap`] once per desktop start. It acts only when this
//! application's own journal exists, restores exactly the mapping that journal
//! records, never touches a foreign or legacy mapping, never retries and reports
//! failure instead of blocking startup. A crash still needs a relaunch to
//! recover.

use std::sync::Arc;

use stache_macos::caps_remap::{CapsRemap, RestoreOutcome};
use stache_sdk::{AppPaths, CancellationToken};

/// What recovery found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsRecovery {
    /// The restore outcome.
    pub outcome: RestoreOutcome,
}

impl CapsRecovery {
    /// A diagnostic when something needs the user or operator, `None` for the
    /// quiet outcomes (no journal, restored).
    #[must_use]
    pub fn diagnostic(&self) -> Option<String> {
        match &self.outcome {
            RestoreOutcome::NoJournal | RestoreOutcome::Restored => None,
            RestoreOutcome::OwnershipLost { observed_destination } => Some(format!(
                "Caps Lock recovery refused: Caps Lock is mapped to {observed_destination:#x}, \
                 which Stache did not put there, so it was left untouched"
            )),
            RestoreOutcome::CleanupPending(reason) => Some(format!(
                "Caps Lock recovery did not complete and was not retried; physical Caps Lock may \
                 still send F18 until the next launch: {reason}"
            )),
            RestoreOutcome::Uncertain(reason) => Some(format!(
                "Caps Lock recovery is uncertain and was not retried; the journal was kept: {reason}"
            )),
            RestoreOutcome::Blocked(reason) => Some(format!(
                "Caps Lock recovery is blocked and nothing was changed: {reason}"
            )),
        }
    }

    /// Whether recovery restored a stale remap.
    #[must_use]
    pub const fn restored(&self) -> bool { matches!(self.outcome, RestoreOutcome::Restored) }
}

/// Recovers a stale Next-owned Caps remap, once, bounded by the lease (5 s),
/// the `hidutil` limits (5 s each) and `cancel`.
///
/// With no journal this reads one directory entry and does nothing else: no
/// lease, no `hidutil`, no created path. It releases the remap's startup latch
/// (see `CapsRemap::begin_startup_recovery`) on every outcome, so the module's
/// first apply is ordered after it.
#[must_use]
pub fn recover_stale_caps_remap(remap: &CapsRemap, cancel: &CancellationToken) -> CapsRecovery {
    CapsRecovery {
        outcome: remap.restore_at_startup(cancel),
    }
}

/// The host's recovery step: a [`CapsRemap`] ready to recover.
#[derive(Clone)]
pub struct CapsRecoveryStep {
    remap: Arc<CapsRemap>,
}

impl CapsRecoveryStep {
    /// A step over an injected remap. Tests pass fakes.
    #[must_use]
    pub const fn new(remap: Arc<CapsRemap>) -> Self { Self { remap } }

    /// The production step: real `hidutil`, the durable journal and the lease of
    /// this identity. Constructing it runs nothing.
    #[must_use]
    pub fn production(paths: &AppPaths) -> Self {
        Self::new(Arc::new(CapsRemap::production(paths)))
    }

    /// The remap this step recovers; the runtime shares it.
    #[must_use]
    pub fn remap(&self) -> Arc<CapsRemap> { Arc::clone(&self.remap) }

    /// Runs recovery.
    #[must_use]
    pub fn run(&self, cancel: &CancellationToken) -> CapsRecovery {
        recover_stale_caps_remap(&self.remap, cancel)
    }
}
