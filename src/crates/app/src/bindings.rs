//! Typed command and event surface shared by the host and the bindings export.
//!
//! The host mounts this builder and the `generate-bindings` binary exports the
//! same collection, so the generated client cannot drift from the commands the
//! desktop registers. Everything here is pure: constructing the builder starts
//! no Tauri runtime, tray, window or module.
//!
//! Commands return concrete DTOs. The generated TypeScript is the only client
//! name source for commands and events; no hand-written mirror exists.

use serde::{Deserialize, Serialize};
use specta::Type;
use stache_keep_awake::{
    AssertionState, KeepAwakeCondition, KeepAwakeConditionKind, KeepAwakeConfig,
    KeepAwakeRuntimeStatus, LockObservation, LockState,
};
use stache_notunes::NoTunesConfig;
use stache_proxy_audio::{AudioDeviceInfo, AudioListKind, AudioListParams, ProxyAudioConfig};
use stache_sdk::{GlobalConfig, IDENTITY, ModuleSnapshot, ModuleStatus, UnavailableReason};
use tauri_specta::{Builder, collect_commands, collect_events};

use crate::host::HostState;
pub use crate::keep_awake::KeepAwakeStatus;

/// Application identity shown by the test window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// Product name.
    pub name: String,
    /// Cargo package version.
    pub version: String,
    /// Bundle identifier.
    pub identifier: String,
}

/// Returns the static application identity.
#[tauri::command]
#[specta::specta]
#[must_use]
pub fn app_info() -> AppInfo {
    AppInfo {
        name: "Stache Next".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        identifier: IDENTITY.to_owned(),
    }
}

/// Observed lifecycle state of one module, without SDK-only payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ModuleState {
    /// Configured but not started.
    Stopped,
    /// Disabled by configuration.
    Disabled,
    /// Owning runtime resources.
    Running,
    /// Stopped without a configuration edit.
    Paused,
    /// Waiting for a permission grant.
    WaitingForPermission,
    /// A prerequisite is missing.
    Unavailable,
    /// A lifecycle operation failed.
    Failed,
}

impl From<&ModuleStatus> for ModuleState {
    fn from(status: &ModuleStatus) -> Self {
        match status {
            ModuleStatus::Stopped => Self::Stopped,
            ModuleStatus::Disabled => Self::Disabled,
            ModuleStatus::Running => Self::Running,
            ModuleStatus::Paused => Self::Paused,
            ModuleStatus::WaitingForPermission(_) => Self::WaitingForPermission,
            ModuleStatus::Unavailable(_) => Self::Unavailable,
            ModuleStatus::Failed(_) => Self::Failed,
        }
    }
}

/// UI-facing status of one module.
///
/// `state` and `detail` describe the runtime; `diagnostic` describes a rejected
/// configuration. A rejected reload never disguises a running module as failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ModuleStatusView {
    /// Stable module identifier.
    pub id: String,
    /// Observed lifecycle state.
    pub state: ModuleState,
    /// Desired enabled flag.
    pub enabled: bool,
    /// Session-only pause flag.
    pub paused: bool,
    /// Last accepted configuration revision as an exact decimal string.
    ///
    /// JavaScript numbers cannot hold every SDK `u64` exactly, so the value is
    /// opaque text on the wire. Never parse it into a `number`.
    pub revision: String,
    /// Permission, missing prerequisite or failure detail.
    pub detail: Option<String>,
    /// Diagnostic for the most recently rejected configuration.
    pub diagnostic: Option<String>,
}

impl ModuleStatusView {
    /// Projects one registry snapshot without retaining SDK payloads.
    #[must_use]
    pub fn from_snapshot(snapshot: &ModuleSnapshot) -> Self {
        Self {
            id: snapshot.id.as_str().to_owned(),
            state: ModuleState::from(&snapshot.status),
            enabled: snapshot.enabled,
            paused: snapshot.paused,
            revision: snapshot.revision.to_string(),
            detail: detail(&snapshot.status),
            diagnostic: snapshot.diagnostic.as_ref().map(|value| value.message.clone()),
        }
    }
}

fn detail(status: &ModuleStatus) -> Option<String> {
    match status {
        ModuleStatus::WaitingForPermission(permission) => {
            Some(format!("waiting for {permission} permission"))
        }
        ModuleStatus::Unavailable(UnavailableReason::MissingService(service)) => {
            Some(format!("missing service `{service}`"))
        }
        ModuleStatus::Unavailable(UnavailableReason::MissingExecutable(executable)) => {
            Some(format!("missing executable `{executable}`"))
        }
        ModuleStatus::Failed(failure) => Some(format!(
            "{} failed ({}): {}",
            failure.operation, failure.category, failure.message
        )),
        ModuleStatus::Stopped
        | ModuleStatus::Disabled
        | ModuleStatus::Running
        | ModuleStatus::Paused => None,
    }
}

/// The local state of one catalog file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ConfigFileState {
    /// The file is absent.
    Missing,
    /// The file parses and validates on disk.
    Valid,
    /// The file exists but is rejected.
    Invalid,
}

/// One configuration file as seen by the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConfigFileView {
    /// File name, for example `proxyAudio.jsonc`.
    pub file: String,
    /// Absolute path.
    pub path: String,
    /// Local parse/validation state.
    pub state: ConfigFileState,
    /// Whether the current bytes were accepted for a loaded global or module.
    pub active: bool,
    /// The held host diagnostic, or the current invalid-file diagnostic.
    pub diagnostic: Option<String>,
    /// Unknown keys in an otherwise valid file.
    pub warnings: Vec<String>,
}

/// Point-in-time status published to the tray and the test window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    /// Whether initial host setup completed.
    pub ready: bool,
    /// Persistent host conditions followed by the last action failure, joined
    /// with `"; "`. A later successful action clears only its own part.
    pub error: Option<String>,
    /// Every registered module, in registration order.
    pub modules: Vec<ModuleStatusView>,
    /// Every catalog file in file order.
    pub config: Vec<ConfigFileView>,
}

/// Event emitted when [`HostStatus`] changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
pub struct StatusChanged {
    /// The new status.
    pub status: HostStatus,
}

/// Returns the last published host status without touching module locks.
#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)] // Tauri injects managed state by value.
#[must_use]
pub fn host_status(state: tauri::State<'_, HostState>) -> HostStatus { state.0.status() }

/// Converts a catalog path entry into its wire view.
#[must_use]
pub fn config_file_view(
    entry: &stache_sdk::PathEntry,
    store: &stache_sdk::ConfigStore,
) -> ConfigFileView {
    let (state, local) = match &entry.state {
        stache_sdk::PathState::Missing => (ConfigFileState::Missing, None),
        stache_sdk::PathState::Valid => (ConfigFileState::Valid, None),
        stache_sdk::PathState::Invalid(diagnostic) => {
            (ConfigFileState::Invalid, Some(diagnostic.to_string()))
        }
    };
    ConfigFileView {
        file: entry.file.file_name(),
        path: entry.path.display().to_string(),
        state,
        active: state == ConfigFileState::Valid && store.current_source_accepted(entry.file),
        diagnostic: store.diagnostic(entry.file).map(|diagnostic| diagnostic.to_string()).or(local),
        warnings: entry.warnings.clone(),
    }
}

/// Builds the typed command and event collection mounted by the desktop host.
///
/// The config types are exported explicitly; they are the authoritative catalog
/// types used by `global.jsonc`, `keepAwake.jsonc`, `proxyAudio.jsonc` and
/// `notunes.jsonc`. The Keep Awake status envelope and its reachable runtime
/// DTOs are exported from the same collection, so the generated client cannot
/// mirror them by hand. The pilot's read-only listing DTOs are exported without
/// a Tauri command: the pilot has no UI and its socket method is not a webview
/// surface.
#[must_use]
pub fn builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![app_info, host_status])
        .events(collect_events![StatusChanged])
        .typ::<GlobalConfig>()
        .typ::<KeepAwakeConfig>()
        .typ::<KeepAwakeStatus>()
        .typ::<KeepAwakeRuntimeStatus>()
        .typ::<AssertionState>()
        .typ::<LockState>()
        .typ::<LockObservation>()
        .typ::<KeepAwakeCondition>()
        .typ::<KeepAwakeConditionKind>()
        .typ::<NoTunesConfig>()
        .typ::<ProxyAudioConfig>()
        .typ::<AudioDeviceInfo>()
        .typ::<AudioListKind>()
        .typ::<AudioListParams>()
}
