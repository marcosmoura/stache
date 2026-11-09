//! Debounced watcher for the configuration directory.
//!
//! The watcher observes the directory itself (never individual files) so an
//! editor that saves through a temporary file and a rename is handled the same
//! way as an in-place write. It only reports which catalog file changed; the
//! caller decides what to reload.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};
use std::{fmt, fs};

use notify_debouncer_full::notify::RecursiveMode;
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};

use crate::config::ConfigCatalog;
use crate::config::diagnostics::ConfigFileId;
use crate::config::paths::ConfigPaths;

/// Debounce window required by the configuration contract.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

type InnerDebouncer =
    Debouncer<notify_debouncer_full::notify::RecommendedWatcher, RecommendedCache>;

/// The catalog files affected by one debounced batch.
///
/// `rescan` is set when the watched directory itself appeared or disappeared;
/// a store should then reload every file because events inside it may have
/// happened before the directory watch was installed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    /// Whether the directory topology changed.
    pub rescan: bool,
    /// Individual files that changed.
    pub files: BTreeSet<ConfigFileId>,
}

impl ChangeSet {
    /// Whether the batch carries no information.
    #[must_use]
    pub fn is_empty(&self) -> bool { !self.rescan && self.files.is_empty() }
}

/// A failure to establish or keep the directory watch.
#[derive(Debug)]
pub enum WatchError {
    /// The directory and none of its ancestors exist.
    NoExistingAncestor(PathBuf),
    /// The watcher backend refused to watch a path.
    Backend(String),
    /// The debouncer thread is gone.
    Stopped,
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoExistingAncestor(path) => {
                write!(
                    f,
                    "cannot watch {}: no existing ancestor directory",
                    path.display()
                )
            }
            Self::Backend(message) => write!(f, "configuration watcher failed: {message}"),
            Self::Stopped => f.write_str("configuration watcher stopped"),
        }
    }
}

impl std::error::Error for WatchError {}

/// Watches the configuration directory and pulls debounced changes.
pub struct ConfigWatcher {
    debouncer: InnerDebouncer,
    receiver: Receiver<DebounceEventResult>,
    directory: PathBuf,
    watched: PathBuf,
    files: BTreeMap<String, ConfigFileId>,
}

impl ConfigWatcher {
    /// Starts watching the directory that contains the configured files.
    ///
    /// When the directory does not exist yet, the nearest existing ancestor is
    /// watched until the directory appears.
    ///
    /// # Errors
    /// Returns [`WatchError`] when no ancestor exists or the backend fails.
    pub fn new(paths: &ConfigPaths, catalog: &ConfigCatalog) -> Result<Self, WatchError> {
        let requested = std::path::absolute(paths.directory()).map_err(|error| {
            WatchError::Backend(format!(
                "cannot resolve {}: {error}",
                paths.directory().display()
            ))
        })?;
        let existing = nearest_existing(&requested)
            .ok_or_else(|| WatchError::NoExistingAncestor(requested.clone()))?;
        let anchor = fs::canonicalize(&existing).map_err(|error| {
            WatchError::Backend(format!("cannot resolve {}: {error}", existing.display()))
        })?;
        let directory =
            anchor.join(requested.strip_prefix(&existing).unwrap_or_else(|_| Path::new("")));
        let files = catalog
            .ids()
            .map(|id| (ConfigFileId::module(id).file_name(), ConfigFileId::module(id)))
            .collect();
        let (sender, receiver) = channel();
        let mut debouncer = new_debouncer(DEBOUNCE, None, sender)
            .map_err(|error| WatchError::Backend(error.to_string()))?;
        watch(&mut debouncer, &anchor)?;
        Ok(Self {
            debouncer,
            receiver,
            directory,
            watched: anchor,
            files,
        })
    }

    /// The directory being watched.
    #[must_use]
    pub fn directory(&self) -> &Path { &self.directory }

    /// Waits up to `timeout` for one debounced batch.
    ///
    /// Returns `None` when no relevant event arrived. Unrelated events observed
    /// while watching an ancestor are skipped without consuming the caller's
    /// timeout early.
    ///
    /// # Errors
    /// Returns [`WatchError::Stopped`] when the debouncer thread is gone.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<ChangeSet>, WatchError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            match self.receiver.recv_timeout(remaining) {
                Ok(Ok(events)) => {
                    let mut changes = ChangeSet::default();
                    for event in events {
                        for path in &event.paths {
                            self.classify(path, &mut changes);
                        }
                    }
                    self.reanchor(&mut changes)?;
                    if !changes.is_empty() {
                        return Ok(Some(changes));
                    }
                }
                Ok(Err(errors)) => {
                    let message =
                        errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
                    return Err(WatchError::Backend(message));
                }
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => return Err(WatchError::Stopped),
            }
        }
    }

    fn classify(&self, path: &Path, changes: &mut ChangeSet) {
        if path == self.directory {
            changes.rescan = true;
        }
        if !path.starts_with(&self.directory) {
            return;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return;
        };
        if let Some(file) = self.files.get(name) {
            changes.files.insert(*file);
        }
    }

    fn reanchor(&mut self, changes: &mut ChangeSet) -> Result<(), WatchError> {
        let target = if self.directory.exists() {
            self.directory.clone()
        } else {
            nearest_existing(&self.directory)
                .ok_or_else(|| WatchError::NoExistingAncestor(self.directory.clone()))?
        };
        if target == self.watched {
            return Ok(());
        }
        self.move_watch(target, changes)
    }

    fn move_watch(&mut self, target: PathBuf, changes: &mut ChangeSet) -> Result<(), WatchError> {
        watch(&mut self.debouncer, &target)?;
        // The old anchor may already be gone; a failed removal must not turn a
        // usable watch into an error.
        let _ = self.debouncer.unwatch(&self.watched);
        self.watched = target;
        changes.rescan = true;
        Ok(())
    }
}

fn watch(debouncer: &mut InnerDebouncer, path: &Path) -> Result<(), WatchError> {
    debouncer
        .watch(path, RecursiveMode::NonRecursive)
        .map_err(|error| WatchError::Backend(format!("{}: {error}", path.display())))
}

fn nearest_existing(path: &Path) -> Option<PathBuf> {
    path.ancestors().find(|candidate| candidate.exists()).map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::error::ConfigError;
    use crate::module::{ModuleConfig, ModuleId};

    #[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
    #[serde(default)]
    struct Sample {
        enabled: bool,
    }

    impl ModuleConfig for Sample {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
    }

    fn catalog() -> ConfigCatalog {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<Sample>(ModuleId::new("sample")).expect("register sample");
        catalog
    }

    fn directory(root: &tempfile::TempDir) -> ConfigPaths {
        ConfigPaths::new(root.path().join("stache"))
    }

    #[test]
    fn file_changes_report_only_the_matching_catalog_file() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = directory(&root);
        fs::create_dir_all(paths.directory()).expect("create config dir");
        let mut watcher = ConfigWatcher::new(&paths, &catalog()).expect("watcher");

        fs::write(
            paths.module_file(ModuleId::new("sample")),
            "{ \"enabled\": true }",
        )
        .expect("write");
        let changes = watcher.next(Duration::from_secs(5)).expect("watch").expect("change");
        assert_eq!(
            changes.files,
            BTreeSet::from([ConfigFileId::module(ModuleId::new("sample"))])
        );
    }

    #[test]
    fn editor_rename_reports_the_final_file_name() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = directory(&root);
        fs::create_dir_all(paths.directory()).expect("create config dir");
        let mut watcher = ConfigWatcher::new(&paths, &catalog()).expect("watcher");

        let temporary = paths.directory().join(".sample.toml.swp");
        fs::write(&temporary, "{ \"enabled\": true }").expect("write temp");
        fs::rename(&temporary, paths.module_file(ModuleId::new("sample"))).expect("rename");

        let changes = watcher.next(Duration::from_secs(5)).expect("watch").expect("change");
        assert!(changes.files.contains(&ConfigFileId::module(ModuleId::new("sample"))));
    }

    #[test]
    fn multiple_writes_inside_the_debounce_window_arrive_once() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = directory(&root);
        fs::create_dir_all(paths.directory()).expect("create config dir");
        let mut watcher = ConfigWatcher::new(&paths, &catalog()).expect("watcher");
        let file = paths.module_file(ModuleId::new("sample"));

        fs::write(&file, "{ \"enabled\": true }").expect("first write");
        fs::write(&file, "{ \"enabled\": false }").expect("second write");
        assert!(watcher.next(Duration::from_secs(5)).expect("watch").is_some());
        assert!(
            watcher.next(Duration::from_millis(500)).expect("quiet").is_none(),
            "the debouncer must coalesce one save burst"
        );
    }

    #[test]
    fn an_initially_absent_directory_is_picked_up_when_created() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = directory(&root);
        let mut watcher = ConfigWatcher::new(&paths, &catalog()).expect("watcher");
        let file = ConfigFileId::module(ModuleId::new("sample"));
        let path = paths.module_file(ModuleId::new("sample"));

        fs::create_dir_all(paths.directory()).expect("create config dir");
        fs::write(&path, "{}").expect("write");
        assert!(watcher.next(Duration::from_secs(5)).expect("watch").is_some());

        // The first batch may only report the directory rescan; the watch must
        // already have moved into it, so a later write names the file.
        fs::write(&path, "{ \"enabled\": true }").expect("second write");
        let changes = watcher.next(Duration::from_secs(5)).expect("watch").expect("change");
        assert!(
            changes.files.contains(&file),
            "the watch did not move into the new directory: {changes:?}"
        );
    }

    #[test]
    fn deletion_reports_the_catalog_file() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = directory(&root);
        fs::create_dir_all(paths.directory()).expect("create config dir");
        let file = paths.module_file(ModuleId::new("sample"));
        fs::write(&file, "{}").expect("write");
        let mut watcher = ConfigWatcher::new(&paths, &catalog()).expect("watcher");

        fs::remove_file(&file).expect("remove");
        let changes = watcher.next(Duration::from_secs(5)).expect("watch").expect("change");
        assert!(changes.files.contains(&ConfigFileId::module(ModuleId::new("sample"))));
    }
}
