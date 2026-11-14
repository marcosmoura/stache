//! Debounced watcher for the configuration directory.
//!
//! It uses `notify`'s recommended backend directly and debounces on the
//! caller's thread: a batch is reported once the directory was quiet for
//! [`DEBOUNCE`], or [`MAX_BATCH_DELAY`] after its first event at the latest.
//!
//! The watcher observes the directory itself (never individual files) so an
//! editor that saves through a temporary file and a rename is handled the same
//! way as an in-place write. It only reports which catalog file changed; the
//! caller decides what to reload.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};
use std::{fmt, fs};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::config::ConfigCatalog;
use crate::config::diagnostics::ConfigFileId;
use crate::config::paths::ConfigPaths;

/// Debounce window required by the configuration contract.
pub const DEBOUNCE: Duration = Duration::from_millis(200);

/// The longest a continuous stream of events can postpone one report.
pub const MAX_BATCH_DELAY: Duration = Duration::from_secs(2);

/// One message on the watcher's channel: a backend event or a stop request.
enum Message {
    Event(notify::Result<notify::Event>),
    Stop,
}

/// Wakes a blocked [`ConfigWatcher::wait`] from another thread.
///
/// It only sends on an in-process channel; it never calls into the backend.
#[derive(Clone)]
pub struct WatchStop {
    sender: Sender<Message>,
}

impl WatchStop {
    /// Ends the watcher's current (or next) blocking wait.
    pub fn stop(&self) { let _ = self.sender.send(Message::Stop); }
}

impl fmt::Debug for WatchStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WatchStop").finish_non_exhaustive()
    }
}

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
    pub const fn is_empty(&self) -> bool { !self.rescan && self.files.is_empty() }
}

/// A failure to establish or keep the directory watch.
#[derive(Debug)]
pub enum WatchError {
    /// The directory and none of its ancestors exist.
    NoExistingAncestor(PathBuf),
    /// The watcher backend refused to watch a path.
    Backend(String),
    /// The backend's event channel is gone.
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

/// Events collected since the first one of a batch that is not reported yet.
struct Pending {
    first: Instant,
    last: Instant,
    paths: Vec<PathBuf>,
}

impl Pending {
    /// When the batch is due: quiet for [`DEBOUNCE`], capped at [`MAX_BATCH_DELAY`].
    fn due(&self) -> Instant { (self.last + DEBOUNCE).min(self.first + MAX_BATCH_DELAY) }
}

/// What one collection step ended with.
enum Collected {
    Changes(ChangeSet),
    Timeout,
    Stopped,
}

/// Watches the configuration directory and pulls debounced changes.
///
/// The quiet-period wait runs on the caller's thread: nothing ticks while the
/// directory is quiet, and only the backend's own event thread runs.
pub struct ConfigWatcher {
    watcher: RecommendedWatcher,
    receiver: Receiver<Message>,
    stop: Sender<Message>,
    pending: Option<Pending>,
    directory: PathBuf,
    watched: PathBuf,
    files: BTreeMap<String, ConfigFileId>,
    #[cfg(test)]
    passes: Arc<AtomicUsize>,
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
        let events = sender.clone();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = events.send(Message::Event(event));
        })
        .map_err(|error| WatchError::Backend(error.to_string()))?;
        watch(&mut watcher, &anchor)?;
        Ok(Self {
            watcher,
            receiver,
            stop: sender,
            pending: None,
            directory,
            watched: anchor,
            files,
            #[cfg(test)]
            passes: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// The directory being watched.
    #[must_use]
    pub fn directory(&self) -> &Path { &self.directory }

    /// A handle that ends a blocked [`ConfigWatcher::wait`] from another thread.
    #[must_use]
    pub fn stop_handle(&self) -> WatchStop { WatchStop { sender: self.stop.clone() } }

    /// Blocks without a deadline until one debounced batch is ready.
    ///
    /// Returns `None` when [`WatchStop::stop`] was called. A batch that was
    /// still collecting when the stop arrived is dropped.
    ///
    /// # Errors
    /// Returns [`WatchError::Backend`] when the backend reports a failure.
    pub fn wait(&mut self) -> Result<Option<ChangeSet>, WatchError> {
        match self.collect(None)? {
            Collected::Changes(changes) => Ok(Some(changes)),
            Collected::Timeout | Collected::Stopped => Ok(None),
        }
    }

    /// Waits up to `timeout` for one debounced batch.
    ///
    /// Returns `None` when no relevant event arrived. Unrelated events observed
    /// while watching an ancestor are skipped without consuming the caller's
    /// timeout early. A stop request also returns `None`.
    ///
    /// # Errors
    /// Returns [`WatchError::Stopped`] when the backend's channel is gone.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<ChangeSet>, WatchError> {
        match self.collect(Some(Instant::now() + timeout))? {
            Collected::Changes(changes) => Ok(Some(changes)),
            Collected::Timeout | Collected::Stopped => Ok(None),
        }
    }

    /// Collects events until a batch is due, the caller's deadline passes or a
    /// stop arrives. Without a pending batch and without a deadline it blocks
    /// with no timeout.
    fn collect(&mut self, deadline: Option<Instant>) -> Result<Collected, WatchError> {
        loop {
            let due = self.pending.as_ref().map(Pending::due);
            let wake = match (due, deadline) {
                (Some(due), Some(deadline)) => Some(due.min(deadline)),
                (due, deadline) => due.or(deadline),
            };
            let received = match wake {
                None => self.receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
                Some(wake) => {
                    self.receiver.recv_timeout(wake.saturating_duration_since(Instant::now()))
                }
            };
            #[cfg(test)]
            self.passes.fetch_add(1, Ordering::Relaxed);
            match received {
                Ok(Message::Stop) => return Ok(Collected::Stopped),
                Ok(Message::Event(Err(error))) => {
                    return Err(WatchError::Backend(error.to_string()));
                }
                Ok(Message::Event(Ok(event))) => {
                    let now = Instant::now();
                    let pending = self.pending.get_or_insert_with(|| Pending {
                        first: now,
                        last: now,
                        paths: Vec::new(),
                    });
                    pending.last = now;
                    pending.paths.extend(event.paths);
                }
                Err(RecvTimeoutError::Disconnected) => return Err(WatchError::Stopped),
                Err(RecvTimeoutError::Timeout) => {}
            }
            let now = Instant::now();
            if let Some(pending) = self.pending.take_if(|pending| pending.due() <= now) {
                let mut changes = ChangeSet::default();
                for path in &pending.paths {
                    self.classify(path, &mut changes);
                }
                self.reanchor(&mut changes)?;
                if !changes.is_empty() {
                    return Ok(Collected::Changes(changes));
                }
            }
            if deadline.is_some_and(|deadline| deadline <= now) {
                return Ok(Collected::Timeout);
            }
        }
    }

    /// The number of completed wait passes. Test-only introspection.
    #[cfg(test)]
    fn pass_counter(&self) -> Arc<AtomicUsize> { Arc::clone(&self.passes) }

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
        watch(&mut self.watcher, &target)?;
        // The old anchor may already be gone; a failed removal must not turn a
        // usable watch into an error.
        let _ = self.watcher.unwatch(&self.watched);
        self.watched = target;
        changes.rescan = true;
        Ok(())
    }
}

fn watch(watcher: &mut RecommendedWatcher, path: &Path) -> Result<(), WatchError> {
    watcher
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

    #[test]
    fn a_burst_of_writes_yields_one_batch() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = directory(&root);
        fs::create_dir_all(paths.directory()).expect("create config dir");
        let mut watcher = ConfigWatcher::new(&paths, &catalog()).expect("watcher");
        let file = paths.module_file(ModuleId::new("sample"));

        for index in 0..5 {
            fs::write(&file, format!("{{ \"enabled\": {} }}", index % 2 == 0)).expect("write");
            std::thread::sleep(Duration::from_millis(20));
        }
        let changes = watcher.next(Duration::from_secs(5)).expect("watch").expect("change");
        assert!(changes.files.contains(&ConfigFileId::module(ModuleId::new("sample"))));
        assert!(
            watcher.next(Duration::from_millis(500)).expect("quiet").is_none(),
            "one burst inside the debounce window must be one batch"
        );
    }

    #[test]
    fn a_directory_that_disappears_and_returns_reports_a_rescan() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = directory(&root);
        fs::create_dir_all(paths.directory()).expect("create config dir");
        let mut watcher = ConfigWatcher::new(&paths, &catalog()).expect("watcher");

        fs::remove_dir(paths.directory()).expect("remove config dir");
        let gone = watcher.next(Duration::from_secs(5)).expect("watch").expect("change");
        assert!(gone.rescan, "removal must rescan: {gone:?}");

        fs::create_dir_all(paths.directory()).expect("recreate config dir");
        let back = watcher.next(Duration::from_secs(5)).expect("watch").expect("change");
        assert!(back.rescan, "return must rescan: {back:?}");
    }

    #[test]
    fn a_continuous_stream_cannot_postpone_a_batch_past_two_seconds() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = directory(&root);
        fs::create_dir_all(paths.directory()).expect("create config dir");
        let mut watcher = ConfigWatcher::new(&paths, &catalog()).expect("watcher");
        let file = paths.module_file(ModuleId::new("sample"));

        let writer = std::thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(5);
            let mut index = 0_u32;
            while Instant::now() < end {
                fs::write(&file, format!("# {index}\n")).expect("write");
                index += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let started = Instant::now();
        let first = watcher.next(Duration::from_secs(5)).expect("watch");
        let first_after = started.elapsed();
        assert!(first.is_some(), "the stream produced no batch");
        // 2 s cap plus slack for backend latency on a busy machine.
        assert!(
            first_after < MAX_BATCH_DELAY + Duration::from_millis(800),
            "first batch after {first_after:?}"
        );
        let second = watcher.next(Duration::from_secs(4)).expect("watch");
        assert!(second.is_some(), "the stream produced no further batch");
        writer.join().expect("writer");
    }

    #[test]
    fn a_quiet_directory_completes_no_pass_and_stop_ends_the_wait() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = directory(&root);
        fs::create_dir_all(paths.directory()).expect("create config dir");
        let mut watcher = ConfigWatcher::new(&paths, &catalog()).expect("watcher");
        // FSEvents may still report the directory's creation just after the
        // watch starts; that is file activity, so let it drain first.
        while watcher.next(Duration::from_millis(300)).expect("settle").is_some() {}
        let passes = watcher.pass_counter();
        let settled = passes.load(Ordering::Relaxed);
        let stop = watcher.stop_handle();
        let (done, finished) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let result = watcher.wait().map(|changes| changes.is_none());
            let _ = done.send(Instant::now());
            result
        });

        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(passes.load(Ordering::Relaxed), settled, "the idle wait woke up");

        let stopped = Instant::now();
        stop.stop();
        let returned = finished.recv_timeout(Duration::from_secs(1)).expect("wait returned");
        assert!(returned.duration_since(stopped) < Duration::from_millis(100));
        assert!(waiter.join().expect("waiter").expect("no backend error"));
        assert_eq!(passes.load(Ordering::Relaxed), settled + 1);
    }
}
