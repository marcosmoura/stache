//! Observed module status and the snapshot exposed to the host.
//!
//! Desired state (enabled, paused) lives next to the observed status but is not
//! encoded in it. A configuration diagnostic is recorded separately so a
//! rejected reload never disguises a running module as stopped.

use crate::error::{ErrorCategory, ModuleError, Operation};
use crate::module::{ModuleId, ServiceId};
use crate::permissions::Permission;

/// What the registry currently observes for one module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleStatus {
    /// A valid configuration is staged but the module has not run yet.
    Stopped,
    /// The desired state is disabled, so no resources are held.
    Disabled,
    /// The module currently owns its runtime resources.
    Running,
    /// The module was explicitly paused; resources are released without a
    /// configuration edit.
    Paused,
    /// Startup is waiting for a permission grant.
    WaitingForPermission(Permission),
    /// A prerequisite is missing; recovery is explicit, not a retry loop.
    Unavailable(UnavailableReason),
    /// A lifecycle operation failed. The snapshot records a [`ModuleFailure`].
    Failed(ModuleFailure),
}

/// Why a module cannot start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnavailableReason {
    /// A required service has no running provider.
    MissingService(ServiceId),
    /// A declared external executable was not found.
    MissingExecutable(String),
}

/// The structured record attached to [`ModuleStatus::Failed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleFailure {
    /// The operation that failed.
    pub operation: Operation,
    /// The failure category.
    pub category: ErrorCategory,
    /// The human-readable description.
    pub message: String,
}

impl ModuleFailure {
    pub(crate) fn from_error(error: &ModuleError) -> Self {
        Self {
            operation: error.operation(),
            category: error.category(),
            message: error.message().to_owned(),
        }
    }
}

/// A candidate configuration rejected by validation.
///
/// The diagnostic describes the rejected candidate; it does not change the
/// observed status or the last-good revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDiagnostic {
    /// The validation reason shown to the user.
    pub message: String,
}

impl ConfigDiagnostic {
    /// Creates a diagnostic from a message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self { Self { message: message.into() } }

    pub(crate) fn from_error(error: &crate::error::ConfigError) -> Self {
        Self {
            message: error.message().to_owned(),
        }
    }
}

/// A point-in-time view of one module for the host and UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleSnapshot {
    /// The module this snapshot describes.
    pub id: ModuleId,
    /// The observed lifecycle status.
    pub status: ModuleStatus,
    /// The desired enabled flag.
    pub enabled: bool,
    /// The temporary pause flag, independent of the desired enabled flag.
    pub paused: bool,
    /// Revision of the last accepted configuration; `0` means none was accepted.
    pub revision: u64,
    /// The diagnostic for the most recently rejected configuration, if any.
    pub diagnostic: Option<ConfigDiagnostic>,
}
