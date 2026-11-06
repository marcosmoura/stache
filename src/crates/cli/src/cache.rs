//! The `cache` command family.
//!
//! Cache clearing touches only the identity cache directory. It refuses a
//! symlinked or foreign-owned directory and never follows a link, so the
//! runtime socket, configuration and the legacy application are unreachable
//! from this command. Whole-root removal happens under one exclusive cache
//! lease shared with every other cache participant.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Subcommand;
use stache_sdk::{AppPaths, CacheLease, CacheLeaseError, CancellationToken, LeaseClock, LeaseWait};

use crate::{fail, output};

/// How long `cache clear` waits for the exclusive cache lease.
const CACHE_CLEAR_LEASE_BUDGET: Duration = Duration::from_secs(5);

/// `cache` subcommands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum CacheCommand {
    /// Remove every cached file.
    Clear,
    /// Print the cache directory.
    Path,
}

pub(crate) fn run<W: Write, E: Write>(
    command: CacheCommand,
    paths: &AppPaths,
    out: &mut W,
    err: &mut E,
) -> ExitCode {
    match command {
        CacheCommand::Path => {
            let _ = writeln!(out, "{}", paths.cache_directory().display());
            ExitCode::SUCCESS
        }
        CacheCommand::Clear => clear(paths, out, err),
    }
}

fn clear<W: Write, E: Write>(paths: &AppPaths, out: &mut W, err: &mut E) -> ExitCode {
    // Production passes a token that is never cancelled: the process installs
    // no signal handler, so SIGINT terminates it before it can mutate a cache
    // it does not own. Tests cancel the token to prove that a cancelled wait
    // mutates nothing.
    clear_with(paths, &CancellationToken::new(), None, out, err)
}

fn clear_with<W: Write, E: Write>(
    paths: &AppPaths,
    cancellation: &CancellationToken,
    clock: Option<Arc<dyn LeaseClock>>,
    out: &mut W,
    err: &mut E,
) -> ExitCode {
    let cache = paths.cache_directory();
    let validated = match read_root(cache, paths.owner()) {
        Root::Missing => return nothing_to_clear(out),
        Root::Present(metadata) => metadata,
        Root::Refused(reason) => return fail(err, &reason),
        Root::Failed(error) => return fail(err, &format!("Failed to clear cache: {error}")),
    };
    let wait = clock.map_or_else(
        || LeaseWait::new(CACHE_CLEAR_LEASE_BUDGET, cancellation.clone()),
        |clock| LeaseWait::with_clock(CACHE_CLEAR_LEASE_BUDGET, cancellation.clone(), clock),
    );
    let lease = match CacheLease::exclusive(paths, &wait) {
        Ok(lease) => lease,
        Err(CacheLeaseError::Busy) => {
            return fail(
                err,
                "Failed to clear cache: cache is busy; retry after current operations finish.",
            );
        }
        Err(CacheLeaseError::Cancelled) => {
            return fail(err, "Failed to clear cache: operation cancelled.");
        }
        Err(error) => return fail(err, &format!("Failed to clear cache: {error}")),
    };
    // Revalidate under the exclusive lease: the root validated above must
    // still be the same directory before any byte is counted or removed.
    let current = match read_root(cache, paths.owner()) {
        Root::Missing => return nothing_to_clear(out),
        Root::Present(metadata) => metadata,
        Root::Refused(reason) => return fail(err, &reason),
        Root::Failed(error) => return fail(err, &format!("Failed to clear cache: {error}")),
    };
    if current.dev() != validated.dev() || current.ino() != validated.ino() {
        return fail(
            err,
            "Failed to clear cache: cache directory changed while clearing",
        );
    }
    let freed = match directory_size(cache) {
        Ok(bytes) => bytes,
        Err(error) => return fail(err, &format!("Failed to clear cache: {error}")),
    };
    if let Err(error) = lease.verify() {
        return fail(err, &format!("Failed to clear cache: {error}"));
    }
    if let Err(error) = fs::remove_dir_all(cache) {
        return fail(err, &format!("Failed to clear cache: {error}"));
    }
    let _ = writeln!(
        out,
        "Cache cleared successfully. Freed {}.",
        output::format_bytes(freed)
    );
    ExitCode::SUCCESS
}

fn nothing_to_clear(out: &mut impl Write) -> ExitCode {
    let _ = writeln!(out, "Cache directory does not exist. Nothing to clear.");
    ExitCode::SUCCESS
}

enum Root {
    Missing,
    Present(fs::Metadata),
    Refused(String),
    Failed(io::Error),
}

fn read_root(path: &Path, owner: u32) -> Root {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Root::Missing,
        Err(error) => return Root::Failed(error),
    };
    if metadata.file_type().is_symlink() {
        return Root::Refused(format!(
            "refusing to clear symlinked cache directory: {}",
            path.display()
        ));
    }
    if let Err(reason) = ensure_clearable(path, &metadata, owner) {
        return Root::Refused(reason);
    }
    Root::Present(metadata)
}

fn ensure_clearable(path: &Path, metadata: &fs::Metadata, owner: u32) -> Result<(), String> {
    if !metadata.is_dir() || metadata.file_type().is_socket() {
        return Err(format!(
            "refusing to clear non-directory cache path: {}",
            path.display()
        ));
    }
    if metadata.uid() != owner {
        return Err(format!(
            "refusing to clear cache directory owned by uid {}",
            metadata.uid()
        ));
    }
    Ok(())
}

fn directory_size(path: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            total += directory_size(&entry.path())?;
        } else if file_type.is_file() {
            total += entry.metadata()?.len();
        }
        // Symlinks and sockets are removed as links and contribute no bytes.
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::Mutex;
    use std::time::Instant;

    use stache_sdk::{AppEnvironment, CACHE_LEASE_FILE_NAME};

    use super::*;
    use crate::CliEnvironment;

    fn environment(root: &Path) -> CliEnvironment { CliEnvironment::for_test(root, root) }

    fn paths(root: &Path) -> AppPaths { environment(root).app_paths().expect("paths") }

    fn execute(root: &Path, command: CacheCommand) -> (ExitCode, String, String) {
        let paths = paths(root);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(command, &paths, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    fn execute_clear(
        root: &Path,
        cancellation: &CancellationToken,
        clock: Option<Arc<dyn LeaseClock>>,
    ) -> (ExitCode, String, String) {
        let paths = paths(root);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = clear_with(&paths, cancellation, clock, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    /// A clock that advances by the requested slice without sleeping.
    struct FakeClock {
        now: Mutex<Instant>,
    }

    impl FakeClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                now: Mutex::new(Instant::now()),
            })
        }
    }

    impl LeaseClock for FakeClock {
        fn now(&self) -> Instant { *self.now.lock().expect("clock") }

        fn sleep(&self, timeout: Duration, cancellation: &CancellationToken) -> bool {
            if cancellation.is_cancelled() {
                return true;
            }
            *self.now.lock().expect("clock") += timeout;
            false
        }
    }

    /// Reads lines until the helper prints its protocol marker.
    fn read_marker(reader: &mut impl BufRead) -> String {
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).expect("read readiness");
            assert!(read > 0, "holder exited before printing a marker");
            let marker = line.trim();
            if matches!(marker, "ready" | "acquired" | "busy") {
                return marker.to_owned();
            }
        }
    }

    /// A separate test process that holds a shared lease until released.
    struct Holder {
        child: Child,
        input: Option<ChildStdin>,
        // Retained so the helper's final harness output never hits a closed pipe.
        _output: BufReader<std::process::ChildStdout>,
    }

    impl Holder {
        fn spawn(root: &Path) -> Self {
            let mut child = Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "--nocapture", "cache::tests::lease_holder_child"])
                .env("STACHE_CACHE_LEASE_CHILD", root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("spawn holder");
            let input = child.stdin.take();
            let mut output = BufReader::new(child.stdout.take().expect("stdout"));
            assert_eq!(
                read_marker(&mut output),
                "ready",
                "holder did not acquire its lease"
            );
            Self { child, input, _output: output }
        }

        fn release(mut self) {
            if let Some(mut input) = self.input.take() {
                let _ = writeln!(input, "release");
            }
            let status = self.child.wait().expect("wait holder");
            assert!(status.success(), "holder failed");
        }
    }

    impl Drop for Holder {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    fn lease_holder_child() {
        let Ok(root) = std::env::var("STACHE_CACHE_LEASE_CHILD") else {
            return;
        };
        let paths = AppPaths::resolve(&AppEnvironment {
            home: Some(root.clone().into()),
            temporary: Some(root.into()),
        })
        .expect("paths");
        let lease = CacheLease::shared(
            &paths,
            &LeaseWait::new(Duration::from_secs(10), CancellationToken::new()),
        )
        .expect("shared lease");
        let mut stdout = std::io::stdout();
        writeln!(stdout, "ready").expect("readiness");
        stdout.flush().expect("flush");
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        drop(lease);
    }

    #[test]
    fn path_prints_the_bare_cache_directory() {
        let root = tempfile::tempdir().expect("temp dir");
        let (code, out, err) = execute(root.path(), CacheCommand::Path);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(
            out,
            format!(
                "{}\n",
                root.path()
                    .canonicalize()
                    .expect("canonical home")
                    .join("Library")
                    .join("Caches")
                    .join("com.marcosmoura.stache.next")
                    .display()
            )
        );
    }

    #[test]
    fn a_missing_cache_reports_the_legacy_message() {
        let root = tempfile::tempdir().expect("temp dir");
        let (code, out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(out, "Cache directory does not exist. Nothing to clear.\n");
    }

    #[test]
    fn clearing_reports_and_removes_only_the_cache_directory() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(cache.join("nested")).expect("cache");
        fs::write(cache.join("one.bin"), [0_u8; 3]).expect("one");
        fs::write(cache.join("nested").join("two.bin"), [0_u8; 5]).expect("two");
        let configuration = root.path().join(".config").join("stache");
        fs::create_dir_all(&configuration).expect("config");
        fs::write(configuration.join("global.jsonc"), "{}").expect("config file");
        let legacy = root.path().join("Library").join("Caches").join("com.marcosmoura.stache");
        fs::create_dir_all(&legacy).expect("legacy cache");
        fs::write(legacy.join("keep.bin"), "keep").expect("legacy file");

        let (code, out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(out, "Cache cleared successfully. Freed 8 bytes.\n");
        assert!(!cache.exists());
        assert!(configuration.join("global.jsonc").exists());
        assert!(legacy.join("keep.bin").exists());
    }

    #[test]
    fn a_symlinked_cache_directory_is_never_followed() {
        let root = tempfile::tempdir().expect("temp dir");
        let target = root.path().join("target");
        fs::create_dir(&target).expect("target");
        fs::write(target.join("keep"), "keep").expect("keep");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(cache.parent().expect("parent")).expect("cache parent");
        std::os::unix::fs::symlink(&target, &cache).expect("symlink");
        let (code, out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(out.is_empty());
        assert!(
            err.contains("refusing to clear symlinked cache directory"),
            "{err}"
        );
        assert_eq!(fs::read_to_string(target.join("keep")).expect("keep"), "keep");
    }

    #[test]
    fn a_symlink_inside_the_cache_is_removed_without_touching_its_target() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(&cache).expect("cache");
        let runtime = paths(root.path()).runtime_directory().to_path_buf();
        fs::create_dir_all(&runtime).expect("runtime");
        fs::write(runtime.join("control.sock"), "not really").expect("runtime file");
        std::os::unix::fs::symlink(&runtime, cache.join("runtime-link")).expect("symlink");
        let (code, _out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(!cache.exists());
        assert!(runtime.join("control.sock").exists());
    }

    #[test]
    fn a_non_directory_cache_path_is_refused() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(cache.parent().expect("parent")).expect("parent");
        fs::write(&cache, "not a cache").expect("file");
        let (code, _out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(
            err.contains("refusing to clear non-directory cache path"),
            "{err}"
        );
        assert_eq!(fs::read_to_string(&cache).expect("preserved"), "not a cache");
    }

    #[test]
    fn a_cache_directory_owned_by_another_user_is_refused() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(&cache).expect("cache");
        let metadata = fs::symlink_metadata(&cache).expect("metadata");
        let foreign = if metadata.uid() == u32::MAX {
            0
        } else {
            metadata.uid() + 1
        };
        let error = ensure_clearable(&cache, &metadata, foreign).expect_err("foreign owner");
        assert!(error.contains("owned by uid"), "{error}");
        assert!(cache.exists());
    }

    #[test]
    fn cached_files_keep_owner_only_permissions_after_cleanup() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(&cache).expect("cache");
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).expect("mode");
        let (code, _out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(!cache.exists());
    }

    #[test]
    fn a_held_shared_lease_fails_clear_as_busy_without_mutation() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = paths(root.path());
        let cache = paths.cache_directory().to_path_buf();
        fs::create_dir_all(&cache).expect("cache");
        fs::write(cache.join("keep.bin"), [0_u8; 4]).expect("file");
        let held = CacheLease::shared(
            &paths,
            &LeaseWait::new(Duration::from_secs(5), CancellationToken::new()),
        )
        .expect("shared lease");
        let started = Instant::now();
        let (code, out, err) = execute_clear(
            root.path(),
            &CancellationToken::new(),
            Some(FakeClock::new() as Arc<dyn LeaseClock>),
        );
        assert_eq!(code, ExitCode::FAILURE);
        assert!(out.is_empty(), "{out}");
        assert_eq!(
            err,
            "stache: Failed to clear cache: cache is busy; retry after current operations finish.\n"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a fake clock must expire the five-second budget without sleeping"
        );
        assert_eq!(CACHE_CLEAR_LEASE_BUDGET, Duration::from_secs(5));
        assert_eq!(
            fs::read(cache.join("keep.bin")).expect("cache content"),
            [0_u8; 4]
        );
        drop(held);
    }

    #[test]
    fn a_cancelled_wait_fails_clear_without_mutation() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(&cache).expect("cache");
        fs::write(cache.join("keep.bin"), [0_u8; 4]).expect("file");
        let token = CancellationToken::new();
        token.cancel();
        let (code, out, err) = execute_clear(
            root.path(),
            &token,
            Some(FakeClock::new() as Arc<dyn LeaseClock>),
        );
        assert_eq!(code, ExitCode::FAILURE);
        assert!(out.is_empty(), "{out}");
        assert_eq!(err, "stache: Failed to clear cache: operation cancelled.\n");
        assert_eq!(
            fs::read(cache.join("keep.bin")).expect("cache content"),
            [0_u8; 4]
        );
    }

    #[test]
    fn clear_against_another_process_holder_is_busy_and_mutates_nothing() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(&cache).expect("cache");
        fs::write(cache.join("keep.bin"), [0_u8; 4]).expect("file");
        let holder = Holder::spawn(root.path());
        let (code, out, err) = execute_clear(
            root.path(),
            &CancellationToken::new(),
            Some(FakeClock::new() as Arc<dyn LeaseClock>),
        );
        assert_eq!(code, ExitCode::FAILURE);
        assert!(out.is_empty(), "{out}");
        assert_eq!(
            err,
            "stache: Failed to clear cache: cache is busy; retry after current operations finish.\n"
        );
        assert_eq!(
            fs::read(cache.join("keep.bin")).expect("cache content"),
            [0_u8; 4]
        );
        holder.release();

        let (code, out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(out, "Cache cleared successfully. Freed 4 bytes.\n");
        assert!(!cache.exists());
    }

    #[test]
    fn clearing_preserves_the_lease_inode() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = paths(root.path());
        let cache = paths.cache_directory().to_path_buf();
        fs::create_dir_all(&cache).expect("cache");
        let (code, _out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        let lease = paths.cache_lease_path();
        assert_eq!(lease, paths.runtime_directory().join(CACHE_LEASE_FILE_NAME));
        let before = fs::symlink_metadata(&lease).expect("lease").ino();
        fs::create_dir_all(&cache).expect("recreated cache");
        let (code, _out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        let after = fs::symlink_metadata(&lease).expect("lease survives").ino();
        assert_eq!(before, after, "clear must not unlink or replace the lease");
    }
}
