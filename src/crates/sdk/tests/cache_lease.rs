//! Cross-process cache lease behavior.
//!
//! Cross-process cases re-execute this test binary as a disposable holder or
//! contender, so contention, exit and kill are observed through the kernel
//! rather than simulated in one process.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{fs, panic};

use stache_sdk::{
    AppEnvironment, AppPaths, CacheLease, CacheLeaseError, CancellationToken, LeaseClock, LeaseWait,
};

fn paths(root: &Path) -> AppPaths {
    AppPaths::resolve(&AppEnvironment {
        home: Some(root.to_path_buf()),
        temporary: Some(root.to_path_buf()),
    })
    .expect("paths")
}

fn wait(budget: Duration) -> LeaseWait { LeaseWait::new(budget, CancellationToken::new()) }

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

fn current_exe() -> PathBuf { std::env::current_exe().expect("test executable") }

fn read_marker<R: io::Read>(reader: &mut BufReader<R>) -> String {
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read marker");
        assert!(read > 0, "the helper exited before printing a marker");
        let marker = line.trim();
        if matches!(marker, "ready" | "acquired" | "busy") {
            return marker.to_owned();
        }
        // Skip the test harness's own "running ..." lines.
    }
}

fn helper_command(root: &Path, mode: &str) -> Command {
    let mut command = Command::new(current_exe());
    command
        .args(["--exact", "--nocapture", "lease_process_helper"])
        .env("STACHE_LEASE_CHILD", mode)
        .env("STACHE_LEASE_ROOT", root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    command
}

fn attempt(root: &Path, mode: &str) -> String {
    let mut child = helper_command(root, mode).spawn().expect("spawn contender");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
    let marker = read_marker(&mut reader);
    assert!(child.wait().expect("wait contender").success());
    marker
}

struct Holder {
    child: Child,
    input: Option<ChildStdin>,
    // Retained so the helper's final harness output never hits a closed pipe.
    _output: BufReader<std::process::ChildStdout>,
}

impl Holder {
    fn spawn(root: &Path, mode: &str) -> Self {
        let mut child =
            helper_command(root, mode).stdin(Stdio::piped()).spawn().expect("spawn holder");
        let input = child.stdin.take();
        let mut output = BufReader::new(child.stdout.take().expect("stdout"));
        assert_eq!(read_marker(&mut output), "ready");
        Self { child, input, _output: output }
    }

    fn release(mut self) {
        if let Some(mut input) = self.input.take() {
            let _ = writeln!(input, "release");
        }
        assert!(self.child.wait().expect("wait holder").success());
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn lease_process_helper() {
    let Ok(mode) = std::env::var("STACHE_LEASE_CHILD") else {
        return;
    };
    let root = PathBuf::from(std::env::var("STACHE_LEASE_ROOT").expect("root"));
    let paths = paths(&root);
    let hold = mode.starts_with("hold");
    let budget = if hold {
        Duration::from_secs(10)
    } else {
        Duration::ZERO
    };
    let acquired = match mode.as_str() {
        "hold-shared" | "try-shared" => CacheLease::shared(&paths, &wait(budget)),
        "hold-exclusive" | "try-exclusive" => CacheLease::exclusive(&paths, &wait(budget)),
        other => panic!("unknown helper mode {other}"),
    };
    match acquired {
        Ok(lease) if hold => {
            println!("ready");
            io::stdout().flush().expect("flush");
            let mut line = String::new();
            let _ = io::stdin().read_line(&mut line);
            drop(lease);
        }
        Ok(_lease) => println!("acquired"),
        Err(CacheLeaseError::Busy) if !hold => println!("busy"),
        Err(error) => panic!("helper could not acquire: {error}"),
    }
    io::stdout().flush().expect("flush");
}

#[test]
fn shared_leases_coexist() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let _first = CacheLease::shared(&paths, &wait(Duration::from_secs(2))).expect("first");
    let _second = CacheLease::shared(&paths, &wait(Duration::from_secs(2))).expect("second");
}

#[test]
fn a_shared_lease_blocks_exclusive_and_exclusive_blocks_shared() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let shared = CacheLease::shared(&paths, &wait(Duration::from_secs(2))).expect("shared");
    assert!(matches!(
        CacheLease::exclusive(&paths, &wait(Duration::ZERO)),
        Err(CacheLeaseError::Busy)
    ));
    drop(shared);
    let exclusive =
        CacheLease::exclusive(&paths, &wait(Duration::from_secs(2))).expect("exclusive");
    assert!(matches!(
        CacheLease::shared(&paths, &wait(Duration::ZERO)),
        Err(CacheLeaseError::Busy)
    ));
    drop(exclusive);
}

#[test]
fn the_lock_lives_outside_the_cache_and_creates_the_runtime_directory() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    assert!(!paths.runtime_directory().exists());
    let lease = CacheLease::shared(&paths, &wait(Duration::from_secs(2))).expect("lease");
    assert_eq!(lease.path(), paths.cache_lease_path());
    assert!(lease.path().starts_with(paths.runtime_directory()));
    assert!(!lease.path().starts_with(paths.cache_directory()));
    let runtime = fs::symlink_metadata(paths.runtime_directory()).expect("runtime");
    assert!(runtime.is_dir());
    assert_eq!(runtime.mode() & 0o777, 0o700);
    assert_eq!(runtime.uid(), paths.owner());
}

#[test]
fn missing_cache_root_and_recreation_keep_the_lock_identity() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let lease = CacheLease::exclusive(&paths, &wait(Duration::from_secs(2))).expect("lease");
    let lock = lease.path().to_path_buf();
    drop(lease);
    let inode = fs::symlink_metadata(&lock).expect("lock").ino();
    fs::create_dir_all(paths.cache_directory()).expect("recreated cache");
    let lease = CacheLease::exclusive(&paths, &wait(Duration::from_secs(2))).expect("reacquire");
    lease.verify().expect("verified");
    drop(lease);
    assert_eq!(fs::symlink_metadata(&lock).expect("lock").ino(), inode);
}

#[test]
fn a_symlinked_lock_is_refused() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    fs::create_dir_all(paths.runtime_directory()).expect("runtime");
    let outside = root.path().join("outside");
    fs::write(&outside, b"foreign").expect("outside");
    std::os::unix::fs::symlink(&outside, paths.cache_lease_path()).expect("symlink");
    assert!(matches!(
        CacheLease::shared(&paths, &wait(Duration::ZERO)),
        Err(CacheLeaseError::Unsafe { .. })
    ));
}

#[test]
fn a_lock_with_unsafe_permissions_is_refused() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let lease = CacheLease::shared(&paths, &wait(Duration::from_secs(2))).expect("lease");
    let lock = lease.path().to_path_buf();
    drop(lease);
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).expect("mode");
    assert!(matches!(
        CacheLease::shared(&paths, &wait(Duration::ZERO)),
        Err(CacheLeaseError::Unsafe { .. })
    ));
}

#[test]
fn a_lock_with_an_extra_link_is_refused() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let lease = CacheLease::shared(&paths, &wait(Duration::from_secs(2))).expect("lease");
    let lock = lease.path().to_path_buf();
    drop(lease);
    fs::hard_link(&lock, paths.runtime_directory().join("second-link")).expect("link");
    assert!(matches!(
        CacheLease::shared(&paths, &wait(Duration::ZERO)),
        Err(CacheLeaseError::Unsafe { .. })
    ));
}

#[test]
fn verify_detects_a_replaced_lock() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let lease = CacheLease::exclusive(&paths, &wait(Duration::from_secs(2))).expect("lease");
    assert!(lease.verify().is_ok());
    let lock = lease.path().to_path_buf();
    let moved = paths.runtime_directory().join("moved.lock");
    fs::rename(&lock, &moved).expect("move");
    fs::write(&lock, b"").expect("new file");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).expect("mode");
    assert!(matches!(lease.verify(), Err(CacheLeaseError::Replaced { .. })));
}

#[test]
fn verify_detects_a_removed_lock() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let lease = CacheLease::exclusive(&paths, &wait(Duration::from_secs(2))).expect("lease");
    fs::remove_file(lease.path()).expect("remove");
    assert!(matches!(lease.verify(), Err(CacheLeaseError::Replaced { .. })));
}

#[test]
fn a_dropped_lease_releases_the_lock_and_keeps_the_file() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let lease = CacheLease::shared(&paths, &wait(Duration::from_secs(2))).expect("lease");
    let lock = lease.path().to_path_buf();
    let inode = fs::symlink_metadata(&lock).expect("lock").ino();
    drop(lease);
    let after = fs::symlink_metadata(&lock).expect("lock survives");
    assert_eq!(after.ino(), inode);
    let _released = CacheLease::exclusive(&paths, &wait(Duration::from_secs(2))).expect("released");
}

#[test]
fn a_fake_clock_expires_the_budget_without_sleeping() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let _held = CacheLease::exclusive(&paths, &wait(Duration::from_secs(2))).expect("held");
    let bounded = LeaseWait::with_clock(
        Duration::from_secs(5),
        CancellationToken::new(),
        FakeClock::new(),
    );
    let started = Instant::now();
    assert!(matches!(
        CacheLease::shared(&paths, &bounded),
        Err(CacheLeaseError::Busy)
    ));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "fake time must not require a real five-second wait"
    );
}

#[test]
fn a_cancelled_wait_returns_cancelled() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let _held = CacheLease::exclusive(&paths, &wait(Duration::from_secs(2))).expect("held");
    let token = CancellationToken::new();
    token.cancel();
    let wait = LeaseWait::new(Duration::from_secs(5), token);
    let started = Instant::now();
    assert!(matches!(
        CacheLease::shared(&paths, &wait),
        Err(CacheLeaseError::Cancelled)
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn a_holder_process_excludes_this_process() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let holder = Holder::spawn(root.path(), "hold-shared");
    assert!(matches!(
        CacheLease::exclusive(&paths, &wait(Duration::ZERO)),
        Err(CacheLeaseError::Busy)
    ));
    holder.release();
    let _released = CacheLease::exclusive(&paths, &wait(Duration::from_secs(2))).expect("released");
}

#[test]
fn this_process_excludes_another_process() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let exclusive = CacheLease::exclusive(&paths, &wait(Duration::from_secs(2))).expect("held");
    assert_eq!(attempt(root.path(), "try-shared"), "busy");
    drop(exclusive);
    let shared = CacheLease::shared(&paths, &wait(Duration::from_secs(2))).expect("shared");
    assert_eq!(attempt(root.path(), "try-exclusive"), "busy");
    drop(shared);
}

#[test]
fn killing_the_holder_process_releases_the_lock() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let mut holder = Holder::spawn(root.path(), "hold-exclusive");
    holder.child.kill().expect("kill holder");
    holder.child.wait().expect("reap holder");
    // Both kill and wait are done; the `Drop` guard's repeated calls are
    // harmless and cannot reacquire the lock.
    let started = Instant::now();
    let released = CacheLease::exclusive(&paths, &wait(Duration::from_secs(2)));
    assert!(released.is_ok(), "killed holder must release its lock");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn unwinding_releases_the_lock() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let status = panic::catch_unwind(|| {
        let lease = CacheLease::shared(&paths, &wait(Duration::from_secs(2))).expect("lease");
        drop(lease);
        panic!("simulated work failure");
    });
    assert!(status.is_err());
    let _released = CacheLease::exclusive(&paths, &wait(Duration::from_secs(2))).expect("released");
}
