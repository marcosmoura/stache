//! File-backed, typed configuration.
//!
//! [`ConfigCatalog`] maps exact module identifiers to one authoritative Rust
//! type and derives defaults, templates and schemas from it. [`ConfigStore`]
//! reads those files and feeds accepted candidates to the existing [`crate::Registry`]
//! lifecycle; it never starts modules itself and keeps rejection diagnostics
//! separate from the registry status.

pub mod accepted;
pub mod bounds;
pub mod catalog;
pub mod diagnostics;
pub mod edit;
pub mod global;
pub mod loader;
pub mod paths;
pub mod schema;
pub mod store;
pub mod template;
pub mod trust;
pub mod watcher;

pub use accepted::{AcceptedConfig, AcceptedSubscription, AcceptedWait, AcceptedWatch};
pub use catalog::{CatalogError, ConfigCatalog, DecodedConfig};
pub use diagnostics::{ConfigDiagnostic, ConfigFileId, DiagnosticKind, SourceLocation};
pub use edit::{ConfigEdit, ConfigEditError, set_enabled};
pub use global::{GlobalConfig, LoggingConfig, StartupCommands};
pub use paths::{ConfigEnvironment, ConfigPaths, PathError, stale_jsonc_hint};
pub use store::{
    ConfigStore, ConfigStoreError, FileOutcome, Initial, PathEntry, PathReport, PathState,
    ReloadOutcome, inspect_paths,
};
pub use trust::{TrustReason, TrustWarning, current_owner, inspect_catalog_trust, inspect_trust};
pub use watcher::{ChangeSet, ConfigWatcher, WatchError};
