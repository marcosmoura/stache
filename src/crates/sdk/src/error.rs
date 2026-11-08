//! Typed lifecycle errors.
//!
//! Every module operation reports a [`ModuleError`] carrying the module, the
//! operation, a category and an optional source. [`RegistryError`] is the
//! separate type for host-level failures such as unknown handles or invalid
//! service declarations. Neither type is used for control flow decisions that
//! depend on human-readable text.

use std::error::Error as StdError;
use std::fmt;

use crate::module::{ModuleId, ServiceId};

/// The lifecycle action that produced an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    /// Preparing a module with its configuration, without starting work.
    Configure,
    /// Acquiring a module's owned resources.
    Start,
    /// Applying a new configuration to a running module.
    Reconfigure,
    /// Reloading a module through the registry.
    Reload,
    /// Releasing a module's resources.
    Stop,
    /// Registering a module with the registry.
    Register,
    /// Shutting the registry down.
    Shutdown,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Configure => "configure",
            Self::Start => "start",
            Self::Reconfigure => "reconfigure",
            Self::Reload => "reload",
            Self::Stop => "stop",
            Self::Register => "register",
            Self::Shutdown => "shutdown",
        })
    }
}

/// The subsystem a failure belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCategory {
    /// A configuration rejected before or during a reload.
    Config,
    /// An OS permission is missing or could not be requested.
    Permission,
    /// A declared service dependency is missing.
    Dependency,
    /// A native platform API failed.
    Platform,
    /// The control socket or another IPC surface failed.
    Ipc,
    /// The registry state machine rejected the transition.
    Lifecycle,
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Config => "config",
            Self::Permission => "permission",
            Self::Dependency => "dependency",
            Self::Platform => "platform",
            Self::Ipc => "ipc",
            Self::Lifecycle => "lifecycle",
        })
    }
}

/// A lifecycle failure attributed to one module and operation.
///
/// `message` is for humans and logs only; callers branch on [`ModuleError::category`]
/// and [`ModuleError::operation`], never on the text.
#[derive(Debug)]
pub struct ModuleError {
    module: ModuleId,
    operation: Operation,
    category: ErrorCategory,
    message: String,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

impl ModuleError {
    /// Creates an error without a lower-level source.
    #[must_use]
    pub fn new(
        module: ModuleId,
        operation: Operation,
        category: ErrorCategory,
        message: impl Into<String>,
    ) -> Self {
        Self {
            module,
            operation,
            category,
            message: message.into(),
            source: None,
        }
    }

    /// Attaches the originating error for `source()` chaining.
    #[must_use]
    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// The module whose operation failed.
    #[must_use]
    pub const fn module(&self) -> ModuleId { self.module }

    /// The operation that failed.
    #[must_use]
    pub const fn operation(&self) -> Operation { self.operation }

    /// The subsystem that failed.
    #[must_use]
    pub const fn category(&self) -> ErrorCategory { self.category }

    /// The human-readable failure description.
    #[must_use]
    pub fn message(&self) -> &str { &self.message }
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "module `{}` failed to {} ({}): {}",
            self.module, self.operation, self.category, self.message
        )
    }
}

impl StdError for ModuleError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_deref().map(|source| source as &(dyn StdError + 'static))
    }
}

/// A configuration rejected by [`crate::ModuleConfig::validate`].
///
/// The message is a diagnostic for the user; it does not drive control flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    message: String,
}

impl ConfigError {
    /// Creates a validation diagnostic.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self { Self { message: message.into() } }

    /// The human-readable reason the configuration was rejected.
    #[must_use]
    pub fn message(&self) -> &str { &self.message }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.message) }
}

impl StdError for ConfigError {}

/// A failure owned by the registry rather than by one module operation.
#[derive(Debug)]
pub enum RegistryError {
    /// A module with this identifier is already registered.
    DuplicateModule {
        /// The duplicate identifier.
        id: ModuleId,
    },
    /// The handle no longer identifies a registered module.
    UnknownModule {
        /// The unknown identifier.
        id: ModuleId,
    },
    /// Two descriptors declare the same provided service.
    DuplicateProvider {
        /// The contested service.
        service: ServiceId,
        /// The module registered first.
        first: ModuleId,
        /// The module registered last.
        second: ModuleId,
    },
    /// A module requires a service that it provides itself.
    SelfDependency {
        /// The offending module.
        module: ModuleId,
        /// The self-provided service.
        service: ServiceId,
    },
    /// A required service has no registered provider.
    MissingProvider {
        /// The module with the unmet requirement.
        module: ModuleId,
        /// The missing service.
        service: ServiceId,
    },
    /// The service declarations contain a cycle.
    CyclicDependency {
        /// The cycle in declaration order, starting and ending with the same module.
        cycle: Vec<ModuleId>,
    },
    /// The handle's configuration type differs from the registered module's.
    ConfigTypeMismatch {
        /// The module whose registered configuration type was requested.
        id: ModuleId,
        /// The type name requested through the handle.
        expected: &'static str,
        /// The type name the module registered.
        registered: &'static str,
    },
    /// A configuration failed validation and was not applied.
    InvalidConfig {
        /// The module that rejected the candidate.
        module: ModuleId,
        /// The validation diagnostic.
        message: String,
    },
    /// The baseline permission requirements were set after a module registered.
    BaselineAfterRegistration,
    /// A published-state schema was registered after a module registered.
    StateSchemaAfterRegistration,
    /// A provider already has a published-state schema.
    DuplicateStateSchema {
        /// The provider with two schemas.
        provider: ModuleId,
    },
    /// The registry is shutting down or has already shut down.
    ShuttingDown,
    /// A module operation failed; the module snapshot records the same failure.
    Module(ModuleError),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateModule { id } => write!(f, "module `{id}` is already registered"),
            Self::UnknownModule { id } => write!(f, "no module is registered as `{id}`"),
            Self::DuplicateProvider { service, first, second } => {
                write!(
                    f,
                    "service `{service}` is provided by both `{first}` and `{second}`"
                )
            }
            Self::SelfDependency { module, service } => {
                write!(
                    f,
                    "module `{module}` requires service `{service}` that it provides itself"
                )
            }
            Self::MissingProvider { module, service } => {
                write!(
                    f,
                    "module `{module}` requires service `{service}` with no registered provider"
                )
            }
            Self::CyclicDependency { cycle } => {
                let path: Vec<&str> = cycle.iter().map(|id| id.as_str()).collect();
                write!(f, "service dependency cycle: {}", path.join(" -> "))
            }
            Self::ConfigTypeMismatch { id, expected, registered } => write!(
                f,
                "module `{id}` was registered with `{registered}`, not the requested `{expected}`"
            ),
            Self::InvalidConfig { module, message } => {
                write!(f, "module `{module}` rejected its configuration: {message}")
            }
            Self::BaselineAfterRegistration => {
                f.write_str("baseline permission requirements must be set before any registration")
            }
            Self::StateSchemaAfterRegistration => f.write_str(
                "published-state schemas must be registered before any module registration",
            ),
            Self::DuplicateStateSchema { provider } => {
                write!(
                    f,
                    "provider `{provider}` already registered a published-state schema"
                )
            }
            Self::ShuttingDown => f.write_str("the registry is shut down"),
            Self::Module(error) => error.fmt(f),
        }
    }
}

impl StdError for RegistryError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Module(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ModuleError> for RegistryError {
    fn from(error: ModuleError) -> Self { Self::Module(error) }
}

/// Result alias for one module operation.
pub type ModuleResult<T> = Result<T, ModuleError>;

/// Result alias for registry operations.
pub type RegistryResult<T> = Result<T, RegistryError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_module_operation_and_category() {
        let error = ModuleError::new(
            ModuleId::new("proxy-audio"),
            Operation::Start,
            ErrorCategory::Platform,
            "audio device unavailable",
        );
        let rendered = error.to_string();
        assert!(rendered.contains("proxy-audio"), "{rendered}");
        assert!(rendered.contains("start"), "{rendered}");
        assert!(rendered.contains("platform"), "{rendered}");
        assert!(rendered.contains("audio device unavailable"), "{rendered}");
    }

    #[test]
    fn source_chain_preserves_the_cause() {
        let cause = std::io::Error::other("device gone");
        let error = ModuleError::new(
            ModuleId::new("m"),
            Operation::Stop,
            ErrorCategory::Platform,
            "stop failed",
        )
        .with_source(cause);
        let source = StdError::source(&error).expect("source");
        assert_eq!(source.to_string(), "device gone");
    }

    #[test]
    fn registry_error_display_names_the_conflict() {
        let error = RegistryError::DuplicateProvider {
            service: ServiceId::new("audio"),
            first: ModuleId::new("one"),
            second: ModuleId::new("two"),
        };
        let rendered = error.to_string();
        assert!(rendered.contains("audio"), "{rendered}");
        assert!(rendered.contains("one"), "{rendered}");
        assert!(rendered.contains("two"), "{rendered}");
    }

    #[test]
    fn config_error_carries_its_message() {
        let error = ConfigError::new("enabled must be a boolean");
        assert_eq!(error.message(), "enabled must be a boolean");
        assert_eq!(error.to_string(), "enabled must be a boolean");
    }
}
