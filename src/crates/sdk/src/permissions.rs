//! Operating-system permissions requested by a module descriptor.
//!
//! The SDK never caches a permission forever. The registry asks the injected
//! provider for a fresh status before starting a module and reacts to explicit
//! change notifications. Native prompt policy belongs to the host.
//!
//! Requirements come from two places: each module's `required_permissions` and
//! an optional host-level baseline set on the registry, which applies to every
//! module and is merged with the module's requirements (user-only wins).

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

/// Whether the host may show the platform prompt without a user action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PromptPolicy {
    /// The host may request once automatically (startup, enable, reload, tick or
    /// activation) in addition to explicit user requests. This is the default
    /// for ordinary modules.
    Automatic,
    /// The host may check and poll the status but must request only from an
    /// explicit user action such as the tray. Mandatory for every shared-input
    /// consumer.
    UserOnly,
}

/// One permission a module currently needs and how it may be requested.
///
/// A module computes its requirements from its accepted or candidate typed
/// configuration through `Module::required_permissions`. The host may also set
/// a baseline requirement that the registry merges into every module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PermissionRequirement {
    permission: Permission,
    prompt: PromptPolicy,
}

impl PermissionRequirement {
    /// A requirement the host may prompt for once automatically.
    #[must_use]
    pub const fn automatic(permission: Permission) -> Self {
        Self {
            permission,
            prompt: PromptPolicy::Automatic,
        }
    }

    /// A requirement the host may request only from an explicit user action.
    #[must_use]
    pub const fn user_only(permission: Permission) -> Self {
        Self {
            permission,
            prompt: PromptPolicy::UserOnly,
        }
    }

    /// A requirement of a shared-input consumer.
    ///
    /// Every module that depends on the shared input service (Keybindings,
    /// Hold-to-Quit, `MenuAnywhere`) declares its input permission through
    /// this constructor so the host never prompts automatically. The host's
    /// Accessibility baseline is also user-only, which makes this redundant for
    /// Accessibility; it remains the declaration for non-baseline permissions.
    #[must_use]
    pub const fn shared_input(permission: Permission) -> Self { Self::user_only(permission) }

    /// The required permission.
    #[must_use]
    pub const fn permission(self) -> Permission { self.permission }

    /// The prompt policy.
    #[must_use]
    pub const fn prompt(self) -> PromptPolicy { self.prompt }

    /// Whether the host may request this permission without a user action.
    #[must_use]
    pub const fn may_auto_prompt(self) -> bool { matches!(self.prompt, PromptPolicy::Automatic) }
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
