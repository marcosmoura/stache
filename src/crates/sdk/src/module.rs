//! Module identity, descriptors and the lifecycle trait.
//!
//! A module owns one typed configuration. Registration erases that type exactly
//! once; the rest of the SDK is configuration-agnostic. Descriptors are plain
//! `'static` data so they can be read without starting the module.

use std::fmt;

use crate::context::ModuleContext;
use crate::error::{ConfigError, ModuleResult};
use crate::permissions::{Permission, PermissionRequirement};

/// A stable module identifier.
///
/// The value is a `'static` string literal: identifiers are compile-time
/// constants, never user input. Registration rejects duplicates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(&'static str);

impl ModuleId {
    /// Creates an identifier from a stable literal.
    #[must_use]
    pub const fn new(id: &'static str) -> Self { Self(id) }

    /// The literal spelling used in descriptors, logs and errors.
    #[must_use]
    pub const fn as_str(self) -> &'static str { self.0 }
}

impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.0) }
}

/// A stable service identifier that one module provides and others require.
///
/// Services are declarations, not object references: modules never hold a
/// concrete implementation of another module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceId(&'static str);

impl ServiceId {
    /// Creates a service identifier from a stable literal.
    #[must_use]
    pub const fn new(id: &'static str) -> Self { Self(id) }

    /// The literal spelling used in descriptors and errors.
    #[must_use]
    pub const fn as_str(self) -> &'static str { self.0 }
}

impl fmt::Display for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.0) }
}

/// The configuration owned by one module.
///
/// `PartialEq` lets the registry recognise an unchanged reload and skip the
/// module entirely. `enabled` is the desired flag; `validate` rejects invalid
/// candidates before any module state is touched.
pub trait ModuleConfig: Clone + Send + Sync + PartialEq + 'static {
    /// Whether the configuration asks for the module to be enabled.
    fn enabled(&self) -> bool;

    /// Rejects invalid values with a diagnostic.
    ///
    /// # Errors
    /// Returns the reason the configuration cannot be applied.
    fn validate(&self) -> Result<(), ConfigError>;

    /// Rewrites a legacy section before `config migrate` validates and writes it.
    ///
    /// Migration is the only place a configuration type may know about spellings
    /// the running configuration no longer accepts. The default leaves the
    /// section untouched, so a type without legacy spellings needs no override.
    ///
    /// Returned notes are shown to the user as migration warnings, for a rewrite
    /// that changes behavior in a way the new file cannot express.
    fn migrate_legacy_section(section: &mut serde_json::Value) -> Vec<String> {
        let _ = section;
        Vec::new()
    }
}

/// The static facts a module publishes before it is started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleDescriptor {
    /// Unique module identifier.
    pub id: ModuleId,
    /// Services this module makes available while it is running.
    pub provides: &'static [ServiceId],
    /// Services that must be running before this module may start.
    pub requires: &'static [ServiceId],
    /// Operating-system permissions this module needs.
    pub permissions: &'static [Permission],
    /// Command names that must be present on `PATH` before the module starts.
    pub executables: &'static [&'static str],
    /// UI surfaces this module may open through its [`crate::WindowCapability`].
    ///
    /// An empty list means the module may not request a window. Surfaces name
    /// shipped routes; they are never URLs supplied at runtime.
    pub surfaces: &'static [crate::windows::SurfaceId],
}

/// One module's lifecycle implementation.
///
/// The registry calls `configure` immediately before the first `start` of a
/// runtime, `reconfigure` while running, and `stop` before releasing the
/// context. Implementations clean up their own resources when any method
/// fails; the registry only cancels and joins the workers it owns.
pub trait Module: Send + 'static {
    /// The configuration type associated with this module.
    type Config: ModuleConfig;

    /// Describes the module without starting it.
    fn descriptor(&self) -> ModuleDescriptor;

    /// The permissions this module needs while `config` is accepted.
    ///
    /// The default is the static [`ModuleDescriptor::permissions`] list with
    /// automatic prompting, so modules that do not override it are unchanged.
    /// The registry calls this with the registered, candidate or accepted
    /// configuration under the module lock; it must be pure, cheap and must not
    /// touch the platform. A shared-input consumer must return
    /// [`PermissionRequirement::shared_input`] requirements.
    fn required_permissions(&self, config: &Self::Config) -> Vec<PermissionRequirement> {
        let _ = config;
        self.descriptor()
            .permissions
            .iter()
            .map(|&permission| PermissionRequirement::automatic(permission))
            .collect()
    }

    /// Prepares the module with a configuration without starting work.
    ///
    /// # Errors
    /// Returns a typed lifecycle error when preparation fails.
    fn configure(&mut self, config: &Self::Config, ctx: &ModuleContext) -> ModuleResult<()>;

    /// Acquires the owned resources of the module.
    ///
    /// # Errors
    /// Returns a typed lifecycle error after cleaning up partial resources.
    fn start(&mut self, ctx: &ModuleContext) -> ModuleResult<()>;

    /// Applies a validated configuration to a running module.
    ///
    /// The implementation must leave the previous runtime intact when it
    /// returns an error.
    ///
    /// # Errors
    /// Returns a typed lifecycle error when the candidate cannot be applied.
    fn reconfigure(&mut self, config: &Self::Config, ctx: &ModuleContext) -> ModuleResult<()>;

    /// Releases resources. Must be safe to call more than once and after a
    /// partial start.
    ///
    /// # Errors
    /// Returns a typed lifecycle error when cleanup itself fails.
    fn stop(&mut self, ctx: &ModuleContext) -> ModuleResult<()>;
}
