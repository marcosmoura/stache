//! Configuration documents for the settings editor: reads, edits, change
//! events and "open in editor".
//!
//! Files are addressed only by catalog file name (`tiling.toml`); a name that is
//! not in the catalog is a typed refusal and no command accepts a path. Reads
//! touch disk once and no module lock. Edits run on the host runner (see
//! [`crate::host::HostHandle::config_edit`]) so the commit, the single reload
//! and the `config-changed` event stay in command order. Every outcome is a
//! typed union on the wire, never a rejected promise.

use std::fs;
use std::io::ErrorKind;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use specta::Type;
use stache_sdk::config::edit_ops::JsonValue;
use stache_sdk::config::{
    ConfigDiagnostic, ConfigEditOp, content_hash, display_path, document_json,
};
use stache_sdk::{ConfigCatalog, ConfigFileId, ConfigPaths, ConfigStore, ModuleId, ReloadOutcome};

use crate::bindings::ConfigFileView;
use crate::config_edit::{CommitOutcome, EnableEditError, create_from_template, plan_edit};
use crate::host::HostState;

/// The catalog and directory the document commands read, managed by Tauri.
#[derive(Clone)]
pub struct ConfigFilesState {
    /// Authoritative configuration types.
    pub catalog: Arc<ConfigCatalog>,
    /// Configuration directory and file paths.
    pub paths: ConfigPaths,
}

/// The on-disk state of one document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ConfigDocumentState {
    /// The file is absent; defaults apply.
    Missing,
    /// The file parses and validates.
    Valid,
    /// The file exists but is rejected; it is read-only until fixed.
    Invalid,
    /// The file is a symlink; it is read-only and never replaced.
    Symlink,
}

/// A configuration diagnostic as the editor shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConfigDiagnosticView {
    /// Human-readable reason.
    pub message: String,
    /// 1-indexed line when the parser reported one.
    pub line: Option<u32>,
    /// 1-indexed column when the parser reported one.
    pub column: Option<u32>,
    /// The field path when known, for example `workspaces[1].layout`.
    pub field: Option<String>,
}

impl From<&ConfigDiagnostic> for ConfigDiagnosticView {
    fn from(diagnostic: &ConfigDiagnostic) -> Self {
        let location = diagnostic.location;
        Self {
            message: diagnostic.message.clone(),
            line: location.and_then(|at| u32::try_from(at.line).ok()),
            column: location.and_then(|at| u32::try_from(at.column).ok()),
            field: diagnostic.field.clone(),
        }
    }
}

/// One catalog file read as a document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConfigDocument {
    /// File name, for example `tiling.toml`.
    pub file: String,
    /// Absolute path.
    pub path: String,
    /// On-disk state.
    pub state: ConfigDocumentState,
    /// Content hash of the exact bytes (FNV-1a 64, hex), `None` when missing.
    /// Send it back as `baseHash` with an edit.
    pub hash: Option<String>,
    /// The parsed file: only keys present in it. `None` when missing or invalid.
    #[specta(type = Option<JsonValue>)]
    pub value: Option<Json>,
    /// The catalog default for the whole file.
    #[specta(type = JsonValue)]
    pub default: Json,
    /// The file's JSON Schema.
    #[specta(type = JsonValue)]
    pub schema: Json,
    /// Why the file is rejected, when it is.
    pub diagnostic: Option<ConfigDiagnosticView>,
    /// Unknown keys in an otherwise valid file.
    pub warnings: Vec<String>,
}

/// The result of `config_document`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ConfigDocumentOutcome {
    /// The document.
    Document {
        /// The document.
        document: Box<ConfigDocument>,
    },
    /// The name is not a catalog file.
    UnknownFile {
        /// The requested name.
        file: String,
    },
}

/// One edit request from the editor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ConfigEditRequest {
    /// Catalog file name, for example `tiling.toml`.
    pub file: String,
    /// The `hash` of the document the edit is based on (`null` = missing).
    pub base_hash: Option<String>,
    /// The operation.
    pub op: ConfigEditOp,
}

/// Why an edit was refused without a diagnostic of the candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum ConfigEditRefusal {
    /// The name is not a catalog file.
    UnknownFile,
    /// The file is a symlink.
    Symlink,
    /// The file could not be read.
    Unreadable,
    /// The current file is invalid and therefore read-only.
    CurrentInvalid,
    /// The path, index or value does not fit the document.
    Path,
    /// Writing the file failed.
    Write,
    /// The host is not running or did not answer in time.
    Unavailable,
}

/// The reload that followed a committed edit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ConfigReloadView {
    /// The new configuration was accepted.
    Applied {
        /// The accepted revision as an exact decimal string.
        revision: String,
    },
    /// The new bytes decode to the accepted configuration.
    Unchanged,
    /// The runtime rejected the file; the last good configuration stays.
    Rejected {
        /// Why.
        diagnostic: ConfigDiagnosticView,
    },
}

/// The result of `config_edit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum ConfigEditOutcome {
    /// The file was written and reloaded once.
    Committed {
        /// The new content hash.
        hash: String,
        /// The reload outcome.
        reload: ConfigReloadView,
    },
    /// The document already holds the requested value; nothing was written.
    Unchanged {
        /// The current hash.
        hash: Option<String>,
    },
    /// The file changed since `baseHash`; nothing was written. Refetch.
    Stale {
        /// The current hash, `None` when the file is missing.
        hash: Option<String>,
    },
    /// The candidate failed parsing, decoding or validation; nothing was
    /// written. `field` falls back to the edited path.
    Invalid {
        /// The catalog's diagnostic.
        diagnostic: ConfigDiagnosticView,
    },
    /// The edit was refused; nothing was written.
    Refused {
        /// Why.
        reason: ConfigEditRefusal,
        /// Human-readable detail.
        message: String,
    },
}

impl ConfigEditOutcome {
    pub(crate) fn unavailable(message: &str) -> Self {
        Self::Refused {
            reason: ConfigEditRefusal::Unavailable,
            message: message.to_owned(),
        }
    }
}

/// The result of `open_config_file`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum OpenConfigFileOutcome {
    /// The default app was asked to open the file.
    Opened,
    /// The name is not a catalog file.
    UnknownFile,
    /// Creating or opening the file failed.
    Failed {
        /// Why.
        message: String,
    },
}

/// Event emitted after every committed edit and every watcher batch.
///
/// A batch is announced when it touched catalog files, whatever the reload
/// outcome (including rejected and deleted files). A watcher batch that only echoes the host's own
/// committed bytes is not announced twice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
pub struct ConfigChanged {
    /// Catalog file names, in catalog order.
    pub files: Vec<String>,
}

/// The catalog identifier for a file name.
#[must_use]
pub fn resolve(catalog: &ConfigCatalog, file: &str) -> Option<ModuleId> {
    catalog.ids().find(|id| ConfigFileId::module(*id).file_name() == file)
}

/// Reads one catalog file as a document, touching disk once.
#[must_use]
pub fn read_document(
    catalog: &ConfigCatalog,
    paths: &ConfigPaths,
    file: &str,
) -> ConfigDocumentOutcome {
    let Some(id) = resolve(catalog, file) else {
        return ConfigDocumentOutcome::UnknownFile { file: file.to_owned() };
    };
    let file_id = ConfigFileId::module(id);
    let path = paths.file(file_id);
    let symlink =
        fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink());
    let mut document = ConfigDocument {
        file: file_id.file_name(),
        path: path.display().to_string(),
        state: ConfigDocumentState::Missing,
        hash: None,
        value: None,
        default: catalog.default_value(id).cloned().unwrap_or(Json::Null),
        schema: catalog
            .schema(id)
            .and_then(|schema| serde_json::to_value(schema).ok())
            .unwrap_or(Json::Null),
        diagnostic: None,
        warnings: Vec::new(),
    };
    let invalid = if symlink {
        ConfigDocumentState::Symlink
    } else {
        ConfigDocumentState::Invalid
    };
    match fs::read(&path) {
        Err(error) if error.kind() == ErrorKind::NotFound && !symlink => {}
        Err(error) => {
            document.state = invalid;
            document.diagnostic = Some((&ConfigDiagnostic::io(file_id, &path, &error)).into());
        }
        Ok(bytes) => {
            document.hash = Some(content_hash(&bytes));
            let text = String::from_utf8_lossy(&bytes);
            match catalog.inspect(id, &path, &text) {
                Ok(warnings) => {
                    document.state = if symlink {
                        ConfigDocumentState::Symlink
                    } else {
                        ConfigDocumentState::Valid
                    };
                    document.value = document_json(&text).ok();
                    document.warnings = warnings;
                }
                Err(diagnostic) => {
                    document.state = invalid;
                    document.diagnostic = Some((&diagnostic).into());
                }
            }
        }
    }
    ConfigDocumentOutcome::Document { document: Box::new(document) }
}

/// What a committed edit tells the host.
pub struct EditRun {
    /// The wire outcome.
    pub outcome: ConfigEditOutcome,
    /// The file and its new hash when bytes were written.
    pub written: Option<(ConfigFileId, String)>,
}

/// Plans, commits and reloads one edit. Runs on the host runner.
#[must_use]
pub fn run_edit(store: &ConfigStore, request: &ConfigEditRequest) -> EditRun {
    let refused = |reason, message: String| EditRun {
        outcome: ConfigEditOutcome::Refused { reason, message },
        written: None,
    };
    let Some(id) = resolve(store.catalog(), &request.file) else {
        return refused(
            ConfigEditRefusal::UnknownFile,
            format!("`{}` is not a configuration file", request.file),
        );
    };
    let file = ConfigFileId::module(id);
    let planned = plan_edit(
        store.catalog(),
        store.paths(),
        id,
        &request.op,
        request.base_hash.as_deref(),
    );
    let edit = match planned {
        Ok(edit) => edit,
        Err(error) => {
            return EditRun {
                outcome: refusal(store, file, &request.op, error),
                written: None,
            };
        }
    };
    let hash = edit.hash();
    match edit.commit() {
        Ok(CommitOutcome::Unchanged) => EditRun {
            outcome: ConfigEditOutcome::Unchanged { hash },
            written: None,
        },
        Ok(CommitOutcome::Written) => {
            let hash = hash.unwrap_or_default();
            let reload = match file {
                ConfigFileId::Global => Ok(store.reload_global()),
                ConfigFileId::Module(id) => store.reload_one(id),
            };
            let reload = match reload {
                Ok(ReloadOutcome::Applied { revision }) => {
                    ConfigReloadView::Applied { revision: revision.to_string() }
                }
                Ok(ReloadOutcome::Unchanged) => ConfigReloadView::Unchanged,
                Ok(ReloadOutcome::Rejected(diagnostic)) => ConfigReloadView::Rejected {
                    diagnostic: (&diagnostic).into(),
                },
                Err(error) => ConfigReloadView::Rejected {
                    diagnostic: ConfigDiagnosticView {
                        message: error.to_string(),
                        line: None,
                        column: None,
                        field: None,
                    },
                },
            };
            EditRun {
                outcome: ConfigEditOutcome::Committed { hash: hash.clone(), reload },
                written: Some((file, hash)),
            }
        }
        Err(error) => EditRun {
            outcome: refusal(store, file, &request.op, error),
            written: None,
        },
    }
}

fn refusal(
    store: &ConfigStore,
    file: ConfigFileId,
    op: &ConfigEditOp,
    error: EnableEditError,
) -> ConfigEditOutcome {
    let message = error.to_string();
    let refused = |reason| ConfigEditOutcome::Refused {
        reason,
        message: message.clone(),
    };
    match error {
        EnableEditError::Stale { current, .. } => ConfigEditOutcome::Stale { hash: current },
        EnableEditError::Conflict { .. } => ConfigEditOutcome::Stale {
            hash: fs::read(store.paths().file(file)).ok().map(|bytes| content_hash(&bytes)),
        },
        EnableEditError::Invalid(diagnostic) => {
            let mut view = ConfigDiagnosticView::from(&diagnostic);
            if view.field.is_none() {
                view.field = Some(display_path(&op.target()));
            }
            ConfigEditOutcome::Invalid { diagnostic: view }
        }
        EnableEditError::CurrentInvalid(_) => refused(ConfigEditRefusal::CurrentInvalid),
        EnableEditError::Symlink { .. } => refused(ConfigEditRefusal::Symlink),
        EnableEditError::Read { .. } => refused(ConfigEditRefusal::Unreadable),
        EnableEditError::Refused(_) => refused(ConfigEditRefusal::Path),
        EnableEditError::Unknown(_) | EnableEditError::Global => {
            refused(ConfigEditRefusal::UnknownFile)
        }
        EnableEditError::Write { .. } => refused(ConfigEditRefusal::Write),
    }
}

/// Creates a missing file from its template, then asks the default app to
/// open it.
#[must_use]
pub fn open_file(
    catalog: &ConfigCatalog,
    paths: &ConfigPaths,
    file: &str,
) -> OpenConfigFileOutcome {
    let Some(id) = resolve(catalog, file) else {
        return OpenConfigFileOutcome::UnknownFile;
    };
    let path = match create_from_template(catalog, paths, id) {
        Ok(path) => path,
        Err(error) => return OpenConfigFileOutcome::Failed { message: error.to_string() },
    };
    match std::process::Command::new("/usr/bin/open").arg(&path).status() {
        Ok(status) if status.success() => OpenConfigFileOutcome::Opened,
        Ok(status) => OpenConfigFileOutcome::Failed {
            message: format!("open exited with {status}"),
        },
        Err(error) => OpenConfigFileOutcome::Failed { message: error.to_string() },
    }
}

/// Reads one catalog file as a document for the settings editor.
#[tauri::command]
#[specta::specta]
pub async fn config_document(app: tauri::AppHandle, file: String) -> ConfigDocumentOutcome {
    use tauri::Manager as _;

    let state = app.state::<ConfigFilesState>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || read_document(&state.catalog, &state.paths, &file))
        .await
        .unwrap_or_else(|_| ConfigDocumentOutcome::UnknownFile { file: String::new() })
}

/// Every catalog file in catalog order with its state, diagnostic and warnings,
/// from the last published host status. Touches no disk or module lock.
#[tauri::command]
#[specta::specta]
#[allow(clippy::needless_pass_by_value)] // Tauri injects managed state by value.
#[must_use]
pub fn config_files(state: tauri::State<'_, HostState>) -> Vec<ConfigFileView> {
    state.0.status().config
}

/// Applies one surgical edit to a catalog file, validated and stale-checked.
#[tauri::command]
#[specta::specta]
pub async fn config_edit(app: tauri::AppHandle, request: ConfigEditRequest) -> ConfigEditOutcome {
    use tauri::Manager as _;

    let host = app.state::<HostState>().0.clone();
    tauri::async_runtime::spawn_blocking(move || host.config_edit(request))
        .await
        .unwrap_or_else(|_| ConfigEditOutcome::unavailable("the edit worker failed"))
}

/// Opens a catalog file with the default app, creating it from the template
/// first when missing.
#[tauri::command]
#[specta::specta]
pub async fn open_config_file(app: tauri::AppHandle, file: String) -> OpenConfigFileOutcome {
    use tauri::Manager as _;

    let state = app.state::<ConfigFilesState>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || open_file(&state.catalog, &state.paths, &file))
        .await
        .unwrap_or_else(|error| OpenConfigFileOutcome::Failed { message: error.to_string() })
}

#[cfg(test)]
#[path = "config_documents_tests.rs"]
mod tests;
