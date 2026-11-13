//! Configuration edits: the tray's enable switch and generic key-path edits.
//!
//! An edit plans the complete replacement text first, validates it,
//! then commits only if the file still has the bytes that were inspected. A
//! conflict, an invalid disk file or a failed validation leaves the file
//! untouched. The actual write is a same-directory temporary file followed by a
//! rename, so readers never observe partial TOML. Replacing an existing file
//! keeps its permission bits; the rename still breaks hard links to the old
//! inode.
//!
//! Pause/resume never reaches this module: they are session-only registry
//! operations.

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use stache_sdk::config::{
    ConfigDiagnostic, ConfigEditOp, DiagnosticKind, apply_edit, content_hash,
};
use stache_sdk::{
    ConfigCatalog, ConfigEdit as ConfigEditResult, ConfigEditError, ConfigFileId, ConfigPaths,
    ModuleId, set_enabled as set_enabled_text,
};
use tempfile::NamedTempFile;

/// A validated edit that has not been written.
#[derive(Debug)]
pub struct PendingEdit {
    path: PathBuf,
    original: Option<Vec<u8>>,
    contents: String,
    changed: bool,
}

impl PendingEdit {
    /// The file this edit targets.
    #[must_use]
    pub fn path(&self) -> &Path { &self.path }

    /// Whether committing would replace the file.
    #[must_use]
    pub const fn is_change(&self) -> bool { self.changed }

    /// The content hash the file has after a successful commit: the planned
    /// text for a change, otherwise the inspected bytes (`None` when missing).
    #[must_use]
    pub fn hash(&self) -> Option<String> {
        if self.changed {
            Some(content_hash(self.contents.as_bytes()))
        } else {
            self.original.as_deref().map(content_hash)
        }
    }

    /// Writes the edit if the file still matches the inspected bytes.
    ///
    /// The replacement is fully prepared and synced before the final conflict
    /// check, so the compare and rename are as close together as the filesystem
    /// allows. A missing target is committed with a no-clobber rename, so a file
    /// created concurrently is never replaced. A symlink target is refused
    /// instead of being silently replaced.
    ///
    /// A non-cooperating writer can still change the file between the final
    /// check and the rename; comparison plus rename is not an atomic
    /// compare-and-swap on POSIX.
    ///
    /// # Errors
    /// Returns [`EnableEditError::Conflict`] when the file changed externally,
    /// [`EnableEditError::Symlink`] for a symlink target, or a typed write error.
    pub fn commit(self) -> Result<CommitOutcome, EnableEditError> {
        if !self.changed {
            return Ok(CommitOutcome::Unchanged);
        }
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|error| EnableEditError::Write {
            path: parent.to_path_buf(),
            error,
        })?;
        let mut temporary =
            NamedTempFile::new_in(parent).map_err(|error| EnableEditError::Write {
                path: parent.to_path_buf(),
                error,
            })?;
        temporary
            .write_all(self.contents.as_bytes())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| EnableEditError::Write { path: self.path.clone(), error })?;
        match &self.original {
            Some(original) => {
                self.verify_unchanged(original)?;
                self.copy_permissions(&temporary)?;
                temporary.persist(&self.path).map_err(|error| EnableEditError::Write {
                    path: self.path.clone(),
                    error: error.error,
                })?;
            }
            None => {
                temporary.persist_noclobber(&self.path).map_err(|error| {
                    if error.error.kind() == ErrorKind::AlreadyExists {
                        EnableEditError::Conflict { path: self.path.clone() }
                    } else {
                        EnableEditError::Write {
                            path: self.path.clone(),
                            error: error.error,
                        }
                    }
                })?;
            }
        }
        Ok(CommitOutcome::Written)
    }

    /// Copies the replaced file's permission bits onto the temporary file.
    ///
    /// `NamedTempFile` creates a private `0600` file, so without this a `0644`
    /// configuration would become owner-only after an edit. A rename still
    /// replaces the inode, so hard links to the old file are not updated; that
    /// is accepted.
    fn copy_permissions(&self, temporary: &NamedTempFile) -> Result<(), EnableEditError> {
        let permissions = fs::metadata(&self.path)
            .map_err(|error| EnableEditError::Write { path: self.path.clone(), error })?
            .permissions();
        temporary
            .as_file()
            .set_permissions(permissions)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| EnableEditError::Write { path: self.path.clone(), error })
    }

    fn verify_unchanged(&self, original: &[u8]) -> Result<(), EnableEditError> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(EnableEditError::Conflict { path: self.path.clone() });
            }
            Err(error) => {
                return Err(EnableEditError::Write { path: self.path.clone(), error });
            }
        };
        if metadata.file_type().is_symlink() {
            return Err(EnableEditError::Symlink { path: self.path.clone() });
        }
        match fs::read(&self.path) {
            Ok(current) if current == original => Ok(()),
            Ok(_) => Err(EnableEditError::Conflict { path: self.path.clone() }),
            Err(error) => Err(EnableEditError::Write { path: self.path.clone(), error }),
        }
    }
}

/// The result of committing a planned edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The file was replaced atomically.
    Written,
    /// The requested value was already present; nothing was written.
    Unchanged,
}

/// Why an enable edit was refused or could not be written.
#[derive(Debug)]
pub enum EnableEditError {
    /// The identifier is not a registered module configuration.
    Unknown(ModuleId),
    /// The global configuration has no module enable switch.
    Global,
    /// The file could not be read.
    Read {
        /// The affected path.
        path: PathBuf,
        /// The underlying failure.
        error: std::io::Error,
    },
    /// The current or candidate file is parse/type/validation invalid.
    Invalid(ConfigDiagnostic),
    /// A generic edit found the current file invalid; it is read-only.
    CurrentInvalid(Box<ConfigDiagnostic>),
    /// The file's bytes no longer match the hash the edit was based on.
    Stale {
        /// The affected path.
        path: PathBuf,
        /// The current hash, `None` for a missing file.
        current: Option<String>,
    },
    /// The edit cannot be applied to the document (bad path, index or value).
    Refused(Box<ConfigEditError>),
    /// The file changed after it was inspected.
    Conflict {
        /// The affected path.
        path: PathBuf,
    },
    /// The target is a symlink; it is never replaced.
    Symlink {
        /// The affected path.
        path: PathBuf,
    },
    /// The atomic write failed.
    Write {
        /// The affected path.
        path: PathBuf,
        /// The underlying failure.
        error: std::io::Error,
    },
}

impl std::fmt::Display for EnableEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(id) => write!(f, "no configuration is registered as `{id}`"),
            Self::Global => f.write_str("the global configuration has no enabled switch"),
            Self::Read { path, error } => write!(f, "cannot read {}: {error}", path.display()),
            Self::Invalid(diagnostic) => write!(f, "configuration is invalid: {diagnostic}"),
            Self::CurrentInvalid(diagnostic) => {
                write!(f, "the file is invalid and cannot be edited: {diagnostic}")
            }
            Self::Stale { path, .. } => {
                write!(
                    f,
                    "{} changed since it was read; the edit was not written",
                    path.display()
                )
            }
            Self::Refused(error) => error.fmt(f),
            Self::Conflict { path } => {
                write!(f, "{} changed on disk; the edit was not written", path.display())
            }
            Self::Symlink { path } => {
                write!(f, "{} is a symlink; the edit was not written", path.display())
            }
            Self::Write { path, error } => write!(f, "cannot write {}: {error}", path.display()),
        }
    }
}

impl std::error::Error for EnableEditError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { error, .. } | Self::Write { error, .. } => Some(error),
            Self::Invalid(diagnostic) => Some(diagnostic),
            Self::CurrentInvalid(diagnostic) => Some(diagnostic.as_ref()),
            Self::Refused(error) => Some(error.as_ref()),
            Self::Unknown(_)
            | Self::Global
            | Self::Conflict { .. }
            | Self::Symlink { .. }
            | Self::Stale { .. } => None,
        }
    }
}

/// Plans a validated `enabled` edit without writing anything.
///
/// A missing file starts from the generated template, so the first enable keeps
/// the normal `$schema` and field comments. An unreadable, invalid or
/// un-editable file is refused before any bytes change.
///
/// # Errors
/// Returns [`EnableEditError`] for an unknown or global ID, an invalid current
/// file, an invalid candidate, or a read failure.
pub fn plan_enabled(
    catalog: &ConfigCatalog,
    paths: &ConfigPaths,
    id: ModuleId,
    enabled: bool,
) -> Result<PendingEdit, EnableEditError> {
    if !catalog.contains(id) {
        return Err(EnableEditError::Unknown(id));
    }
    let file = ConfigFileId::module(id);
    if file == ConfigFileId::Global {
        return Err(EnableEditError::Global);
    }
    let path = paths.file(file);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(EnableEditError::Symlink { path });
        }
        Ok(_) | Err(_) => {}
    }
    let (text, original, existed) = match fs::read(&path) {
        Ok(bytes) => {
            let text = String::from_utf8(bytes.clone()).map_err(|_| EnableEditError::Read {
                path: path.clone(),
                error: std::io::Error::other("configuration is not UTF-8"),
            })?;
            (text, Some(bytes), true)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let template = catalog.template(id).ok_or(EnableEditError::Unknown(id))?;
            (template, None, false)
        }
        Err(error) => {
            return Err(EnableEditError::Read { path, error });
        }
    };
    if existed {
        catalog.inspect(id, &path, &text).map_err(EnableEditError::Invalid)?;
    }
    let edit = set_enabled_text(&text, enabled)
        .map_err(|error| EnableEditError::Invalid(invalid(&path, error.to_string())))?;
    let Some(candidate) = edit.text() else {
        return Ok(PendingEdit {
            path,
            original,
            contents: text,
            changed: false,
        });
    };
    catalog.inspect(id, &path, candidate).map_err(EnableEditError::Invalid)?;
    // Safety net: the edited text must carry the requested root `enabled`.
    if !matches!(
        set_enabled_text(candidate, enabled),
        Ok(ConfigEditResult::Unchanged)
    ) {
        return Err(EnableEditError::Invalid(invalid(
            &path,
            format!(
                "the edit did not set the root `enabled` key to {enabled}; the file was not changed"
            ),
        )));
    }
    Ok(PendingEdit {
        path,
        original,
        contents: candidate.to_owned(),
        changed: true,
    })
}

/// Plans one validated generic edit without writing anything.
///
/// The file is read once. A symlink is refused; the bytes must hash to
/// `base_hash` (`None` = the file was missing) or the edit is
/// [`EnableEditError::Stale`]; an existing file must currently be valid. A
/// missing file starts from the generated template. The candidate is decoded
/// and validated through the catalog exactly as a reload would.
///
/// # Errors
/// Returns [`EnableEditError`] for an unknown ID, a symlink, a read failure, a
/// stale hash, an invalid current file, a refused operation or an invalid
/// candidate.
pub fn plan_edit(
    catalog: &ConfigCatalog,
    paths: &ConfigPaths,
    id: ModuleId,
    op: &ConfigEditOp,
    base_hash: Option<&str>,
) -> Result<PendingEdit, EnableEditError> {
    if !catalog.contains(id) {
        return Err(EnableEditError::Unknown(id));
    }
    let path = paths.file(ConfigFileId::module(id));
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(EnableEditError::Symlink { path });
    }
    let original = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(EnableEditError::Read { path, error }),
    };
    let current = original.as_deref().map(content_hash);
    if current.as_deref() != base_hash {
        return Err(EnableEditError::Stale { path, current });
    }
    let text = match &original {
        Some(bytes) => {
            let text = String::from_utf8(bytes.clone()).map_err(|_| EnableEditError::Read {
                path: path.clone(),
                error: std::io::Error::other("configuration is not UTF-8"),
            })?;
            catalog
                .inspect(id, &path, &text)
                .map_err(|diagnostic| EnableEditError::CurrentInvalid(Box::new(diagnostic)))?;
            text
        }
        None => catalog.template(id).ok_or(EnableEditError::Unknown(id))?,
    };
    let edit = apply_edit(&text, op).map_err(|error| EnableEditError::Refused(Box::new(error)))?;
    let Some(candidate) = edit.text() else {
        return Ok(PendingEdit {
            path,
            original,
            contents: text,
            changed: false,
        });
    };
    catalog.inspect(id, &path, candidate).map_err(EnableEditError::Invalid)?;
    Ok(PendingEdit {
        path,
        original,
        contents: candidate.to_owned(),
        changed: true,
    })
}

/// Creates a missing file from the generated template with a no-clobber
/// rename. An existing file (including one created concurrently) is left as is.
///
/// # Errors
/// Returns [`EnableEditError`] for an unknown ID or a write failure.
pub fn create_from_template(
    catalog: &ConfigCatalog,
    paths: &ConfigPaths,
    id: ModuleId,
) -> Result<PathBuf, EnableEditError> {
    let path = paths.file(ConfigFileId::module(id));
    if fs::symlink_metadata(&path).is_ok() {
        return Ok(path);
    }
    let contents = catalog.template(id).ok_or(EnableEditError::Unknown(id))?;
    let pending = PendingEdit {
        path: path.clone(),
        original: None,
        contents,
        changed: true,
    };
    match pending.commit() {
        Ok(_) | Err(EnableEditError::Conflict { .. }) => Ok(path),
        Err(error) => Err(error),
    }
}

fn invalid(path: &Path, message: String) -> ConfigDiagnostic {
    ConfigDiagnostic {
        file: ConfigFileId::Global,
        path: path.to_path_buf(),
        kind: DiagnosticKind::Application,
        location: None,
        field: None,
        message,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::catalog::catalog;

    fn paths(root: &Path) -> ConfigPaths {
        let directory = root.join("config");
        ConfigPaths::new(directory)
    }

    fn write(path: &Path, text: &str) { fs::write(path, text).expect("write"); }

    #[test]
    fn enabling_preserves_comments_and_unrelated_values() {
        let root = tempfile::tempdir().expect("root");
        let catalog = catalog().expect("catalog");
        let paths = paths(root.path());
        fs::create_dir_all(paths.directory()).expect("config dir");
        let path = paths.file(ConfigFileId::module(ModuleId::new("proxyAudio")));
        let source = r#"#:schema https://example.invalid/proxyAudio.schema.json

# Audio routing is opt-in.
enabled = false # tray toggles this

[[output]]
name = "Speakers" # keep me
strategy = "exact"
"#;
        write(&path, source);
        let edit = plan_enabled(&catalog, &paths, ModuleId::new("proxyAudio"), true).expect("plan");
        assert!(edit.is_change());
        assert_eq!(edit.commit().expect("commit"), CommitOutcome::Written);
        let edited = fs::read_to_string(&path).expect("read");
        assert_eq!(edited, source.replace("enabled = false", "enabled = true"));
        catalog.inspect(ModuleId::new("proxyAudio"), &path, &edited).expect("valid");
        let entries: Vec<String> = fs::read_dir(paths.directory())
            .expect("read directory")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            vec!["proxyAudio.toml"],
            "no temporary file is left behind"
        );
    }

    #[test]
    fn an_enable_edit_preserves_the_existing_file_mode() {
        let root = tempfile::tempdir().expect("root");
        let catalog = catalog().expect("catalog");
        let paths = paths(root.path());
        fs::create_dir_all(paths.directory()).expect("config dir");
        let path = paths.file(ConfigFileId::module(ModuleId::new("proxyAudio")));
        write(&path, "enabled = false\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("mode");
        let edit = plan_enabled(&catalog, &paths, ModuleId::new("proxyAudio"), true).expect("plan");
        assert_eq!(edit.commit().expect("commit"), CommitOutcome::Written);
        assert_eq!(
            fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
            0o644,
            "the replacement must keep the original mode"
        );
    }

    #[test]
    fn a_conflicting_external_write_is_never_overwritten() {
        let root = tempfile::tempdir().expect("root");
        let catalog = catalog().expect("catalog");
        let paths = paths(root.path());
        fs::create_dir_all(paths.directory()).expect("config dir");
        let path = paths.file(ConfigFileId::module(ModuleId::new("proxyAudio")));
        write(&path, "enabled = false\n");
        let edit = plan_enabled(&catalog, &paths, ModuleId::new("proxyAudio"), true).expect("plan");
        write(&path, "enabled = false\noutput = [] # external\n");
        assert!(matches!(edit.commit(), Err(EnableEditError::Conflict { .. })));
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            "enabled = false\noutput = [] # external\n"
        );
    }

    #[test]
    fn a_file_created_after_planning_is_not_overwritten() {
        let root = tempfile::tempdir().expect("root");
        let catalog = catalog().expect("catalog");
        let paths = paths(root.path());
        let path = paths.file(ConfigFileId::module(ModuleId::new("proxyAudio")));
        let edit = plan_enabled(&catalog, &paths, ModuleId::new("proxyAudio"), true).expect("plan");
        fs::create_dir_all(paths.directory()).expect("config dir");
        write(&path, "enabled = false\noutput = [] # created concurrently\n");
        assert!(matches!(edit.commit(), Err(EnableEditError::Conflict { .. })));
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            "enabled = false\noutput = [] # created concurrently\n"
        );
    }

    #[test]
    fn a_symlink_target_is_refused_instead_of_replaced() {
        let root = tempfile::tempdir().expect("root");
        let catalog = catalog().expect("catalog");
        let paths = paths(root.path());
        fs::create_dir_all(paths.directory()).expect("config dir");
        let target = paths.directory().join("real.toml");
        write(&target, "enabled = false\n");
        let path = paths.file(ConfigFileId::module(ModuleId::new("proxyAudio")));
        std::os::unix::fs::symlink(&target, &path).expect("symlink");
        assert!(matches!(
            plan_enabled(&catalog, &paths, ModuleId::new("proxyAudio"), true),
            Err(EnableEditError::Symlink { .. })
        ));
        assert_eq!(fs::read_to_string(&target).expect("target"), "enabled = false\n");
        assert!(fs::symlink_metadata(&path).expect("path").file_type().is_symlink());
    }

    #[test]
    fn invalid_files_are_refused_before_editing() {
        let root = tempfile::tempdir().expect("root");
        let catalog = catalog().expect("catalog");
        let paths = paths(root.path());
        fs::create_dir_all(paths.directory()).expect("config dir");
        let path = paths.file(ConfigFileId::module(ModuleId::new("proxyAudio")));
        for source in [
            "enabled = \"yes\"",
            "enabled = ",
            "enabled = true\n[[output]]\nname = \"\"\n",
            "# only a comment\n[enabled]\nx = 1\n",
        ] {
            write(&path, source);
            assert!(matches!(
                plan_enabled(&catalog, &paths, ModuleId::new("proxyAudio"), false),
                Err(EnableEditError::Invalid(_))
            ));
            assert_eq!(fs::read_to_string(&path).expect("read"), source);
        }
    }

    #[test]
    fn a_missing_file_is_created_from_the_template() {
        let root = tempfile::tempdir().expect("root");
        let catalog = catalog().expect("catalog");
        let paths = paths(root.path());
        let path = paths.file(ConfigFileId::module(ModuleId::new("proxyAudio")));
        let edit = plan_enabled(&catalog, &paths, ModuleId::new("proxyAudio"), true).expect("plan");
        assert_eq!(edit.commit().expect("commit"), CommitOutcome::Written);
        let text = fs::read_to_string(&path).expect("created");
        assert!(text.starts_with("#:schema "), "{text}");
        assert!(text.contains("\nenabled = true\n"), "{text}");
        catalog.inspect(ModuleId::new("proxyAudio"), &path, &text).expect("valid");
    }

    #[test]
    fn an_unchanged_value_writes_nothing() {
        let root = tempfile::tempdir().expect("root");
        let catalog = catalog().expect("catalog");
        let paths = paths(root.path());
        fs::create_dir_all(paths.directory()).expect("config dir");
        let path = paths.file(ConfigFileId::module(ModuleId::new("proxyAudio")));
        write(&path, "enabled = false\n");
        let edit =
            plan_enabled(&catalog, &paths, ModuleId::new("proxyAudio"), false).expect("plan");
        assert!(!edit.is_change());
        assert_eq!(edit.commit().expect("commit"), CommitOutcome::Unchanged);
        assert_eq!(fs::read_to_string(&path).expect("read"), "enabled = false\n");
    }

    #[test]
    fn the_global_file_is_not_a_module_switch() {
        let root = tempfile::tempdir().expect("root");
        let catalog = catalog().expect("catalog");
        let paths = paths(root.path());
        assert!(matches!(
            plan_enabled(&catalog, &paths, ModuleId::new("global"), false),
            Err(EnableEditError::Global)
        ));
        assert!(matches!(
            plan_enabled(&catalog, &paths, ModuleId::new("missing"), false),
            Err(EnableEditError::Unknown(_))
        ));
    }
}
