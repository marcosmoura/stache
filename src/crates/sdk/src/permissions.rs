//! Operating-system permissions requested by a module descriptor.
//!
//! The SDK never caches a permission forever. The registry asks the injected
//! provider for a fresh status before starting a module and reacts to explicit
//! change notifications. Native prompt policy belongs to the host.

use std::error::Error as StdError;
use std::fmt;

/// A permission a module can declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// macOS Accessibility trust, required by input and window control.
    Accessibility,
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("accessibility") }
}

/// The state reported by a [`PermissionProvider`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionStatus {
    /// The user has granted the permission.
    Granted,
    /// The user has denied the permission.
    Denied,
    /// The platform could not determine the status yet.
    Unknown,
}

/// Injects fresh permission checks and explicit requests.
///
/// Implementations must be safe to call from any thread. `status` never
/// prompts; only `request` may show a platform dialog.
pub trait PermissionProvider: Send + Sync {
    /// Returns the current status without prompting.
    fn status(&self, permission: Permission) -> PermissionStatus;

    /// Asks the platform to grant the permission, potentially prompting.
    ///
    /// # Errors
    /// Returns an error when the request cannot be presented.
    fn request(&self, permission: Permission) -> Result<PermissionStatus, PermissionError>;
}

/// A permission request that could not be presented by the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionError {
    permission: Permission,
    message: String,
}

impl PermissionError {
    /// Creates a request failure for one permission.
    #[must_use]
    pub fn new(permission: Permission, message: impl Into<String>) -> Self {
        Self {
            permission,
            message: message.into(),
        }
    }

    /// The permission whose request failed.
    #[must_use]
    pub const fn permission(&self) -> Permission { self.permission }

    /// The human-readable reason.
    #[must_use]
    pub fn message(&self) -> &str { &self.message }
}

impl fmt::Display for PermissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} permission request failed: {}",
            self.permission, self.message
        )
    }
}

impl StdError for PermissionError {}
