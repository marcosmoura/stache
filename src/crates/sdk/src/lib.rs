//! The Stache module SDK.
//!
//! A module implements [`Module`] with one typed configuration, declares its
//! services and permissions in a [`ModuleDescriptor`], and receives a
//! [`ModuleContext`] for logging, events, workers, permissions and main-thread
//! dispatch. [`Registry`] owns every lifecycle transition; there are no global
//! registries, loggers or event buses.
//!
//! State rules: a disabled module holds no resources, a paused module is
//! stopped without a configuration edit, an unchanged configuration is a no-op,
//! and a rejected configuration never replaces the last accepted revision.
//!
//! Threading rules: the registry serializes operations per module, never holds
//! its map lock while a module runs, and joins named workers only after
//! cancelling them. Main-thread dispatch executes inline when already on the
//! main thread.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Public names intentionally carry their domain prefix (`Module`, `ModuleId`,
// `ModuleStatus`, ...) to keep the API explicit at call sites.
#![allow(clippy::module_name_repetitions)]
// Locks are intentionally held across module calls: that is how a module's
// lifecycle operations are serialized. The lint cannot see that intent.
#![allow(clippy::significant_drop_tightening)]

use std::sync::{Mutex, MutexGuard, PoisonError};

pub mod cache_lease;
pub mod cli;
pub mod commands;
pub mod config;
pub mod context;
mod dependencies;
pub mod durable;
pub mod error;
pub mod events;
pub mod input;
pub mod instance;
pub mod ipc;
mod lockfile;
pub mod logging;
pub mod module;
pub mod paths;
pub mod permissions;
pub mod registry;
pub mod shutdown;
pub mod status;
pub mod threading;
pub mod windows;

pub use cache_lease::{CacheLease, CacheLeaseError, LeaseClock, LeaseWait};
pub use cli::{CliContribution, CliExecutor, CommandCatalog, CommandCatalogError};
pub use config::{
    AcceptedConfig, AcceptedSubscription, AcceptedWait, AcceptedWatch, CatalogError, ChangeSet,
    ConfigCatalog, ConfigEdit, ConfigEditError, ConfigEnvironment, ConfigFileId, ConfigPaths,
    ConfigStore, ConfigStoreError, ConfigWatcher, DiagnosticKind, FileOutcome, GlobalConfig,
    Initial, LoggingConfig, PathEntry, PathError, PathReport, PathState, ReloadOutcome,
    SourceLocation, StartupCommands, WatchError, inspect_paths, set_enabled,
};
pub use context::ModuleContext;
pub use durable::{DurableError, DurableLease};
pub use error::{
    ConfigError, ErrorCategory, ModuleError, ModuleResult, Operation, RegistryError, RegistryResult,
};
pub use events::{EventBus, Subscription};
pub use input::{InputCapability, InputService};
pub use instance::{INSTANCE_FILE_NAME, InstanceError, InstanceLock};
pub use logging::{DiscardLogger, LogFilter, LogLevel, LogRecord, Logger, ScopedLogger};
pub use module::{Module, ModuleConfig, ModuleDescriptor, ModuleId, ServiceId};
pub use paths::{
    AppEnvironment, AppPathError, AppPaths, CACHE_LEASE_FILE_NAME, IDENTITY, MAX_SOCKET_PATH,
    SOCKET_FILE_NAME,
};
pub use permissions::{
    Permission, PermissionError, PermissionProvider, PermissionRequirement, PermissionStatus,
    PromptPolicy,
};
pub use registry::{
    ModuleHandle, ModuleInvalidation, Registry, ReloadReport, ReloadStatus, StopTarget,
};
pub use shutdown::{
    CANCELLATION_QUANTUM, OWNER_WAIT_BUDGET, ShutdownWait, wait_for_finish, wait_until,
};
pub use status::{
    ConfigDiagnostic, ModuleFailure, ModuleSnapshot, ModuleStatus, UnavailableReason,
};
pub use threading::{
    CancelListener, CancellationToken, MainThreadExecutor, ThreadError, WorkerGroup, WorkerHandle,
};
pub use windows::{
    MAX_WINDOW_DIMENSION, MAX_WINDOW_KEY_LENGTH, OwnedWindow, SurfaceId, UnavailableWindows,
    WindowBackend, WindowCapability, WindowError, WindowId, WindowKey, WindowKind, WindowRequest,
    WindowSize,
};

/// Locks a mutex and recovers the guard if a previous holder panicked.
///
/// Lifecycle state must stay inspectable after a module panics, so poisoning is
/// treated as recoverable rather than as a second failure.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
