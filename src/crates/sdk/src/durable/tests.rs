//! Durable directory, lease and file tests over temp roots and a manual clock.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::*;
use crate::cache_lease::LeaseClock;
use crate::paths::AppEnvironment;
use crate::threading::CancellationToken;

fn owner() -> u32 { rustix::process::geteuid().as_raw() }

fn paths(root: &Path) -> AppPaths {
    AppPaths::resolve(&AppEnvironment {
        home: Some(root.to_path_buf()),
        temporary: Some(root.to_path_buf()),
    })
    .expect("paths")
}

struct ManualClock {
    now: Mutex<Instant>,
    sleeps: Mutex<Vec<Duration>>,
}

impl ManualClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(Instant::now()),
            sleeps: Mutex::new(Vec::new()),
        })
    }

    fn sleeps(&self) -> Vec<Duration> {
        self.sleeps.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl LeaseClock for ManualClock {
    fn now(&self) -> Instant { *self.now.lock().unwrap_or_else(PoisonError::into_inner) }

    fn sleep(&self, timeout: Duration, cancellation: &CancellationToken) -> bool {
        self.sleeps.lock().unwrap_or_else(PoisonError::into_inner).push(timeout);
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) += timeout;
        cancellation.is_cancelled()
    }
}

fn wait(clock: &Arc<ManualClock>, cancel: &CancellationToken) -> LeaseWait {
    LeaseWait::with_clock(Duration::from_secs(5), cancel.clone(), Arc::clone(clock) as _)
}

#[test]
fn the_durable_directory_is_private_and_outside_cache_and_runtime() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let directory = prepare_directory(&paths, "keybindings").expect("prepare");
    assert_eq!(
        directory,
        paths.application_support_directory().join("keybindings")
    );
    assert_eq!(
        fs::metadata(&directory).expect("meta").permissions().mode() & 0o777,
        0o700
    );
    assert!(!directory.starts_with(paths.cache_directory()));
    assert!(!directory.starts_with(paths.runtime_directory()));
    // Idempotent.
    assert_eq!(
        prepare_directory(&paths, "keybindings").expect("again"),
        directory
    );
}

#[test]
fn an_absent_durable_directory_is_not_created_by_inspection() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    assert!(existing_directory(&paths, "keybindings").expect("inspect").is_none());
    assert!(!paths.application_support_directory().exists());
}

#[test]
fn a_symlinked_durable_directory_is_refused() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    fs::create_dir_all(paths.application_support_directory()).expect("identity");
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir(&elsewhere).expect("elsewhere");
    std::os::unix::fs::symlink(
        &elsewhere,
        paths.application_support_directory().join("keybindings"),
    )
    .expect("link");
    assert!(matches!(
        prepare_directory(&paths, "keybindings"),
        Err(DurableError::Unsafe { .. })
    ));
    assert!(matches!(
        existing_directory(&paths, "keybindings"),
        Err(DurableError::Unsafe { .. })
    ));
}

#[test]
fn atomic_writes_replace_whole_files_and_leave_no_temporary() {
    let root = tempfile::tempdir().expect("root");
    let directory = prepare_directory(&paths(root.path()), "keybindings").expect("prepare");
    assert_eq!(
        read_file(&directory, "journal", 64, owner()).expect("read"),
        None
    );
    write_atomic(&directory, "journal", b"one", owner()).expect("first");
    write_atomic(&directory, "journal", b"two", owner()).expect("second");
    assert_eq!(
        read_file(&directory, "journal", 64, owner()).expect("read"),
        Some(b"two".to_vec())
    );
    assert!(!directory.join(".journal.tmp").exists());
    let mode = fs::metadata(directory.join("journal")).expect("meta").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    remove_file(&directory, "journal", owner()).expect("remove");
    remove_file(&directory, "journal", owner()).expect("remove is idempotent");
    assert_eq!(
        read_file(&directory, "journal", 64, owner()).expect("read"),
        None
    );
}

#[test]
fn a_stale_temporary_never_blocks_a_write() {
    let root = tempfile::tempdir().expect("root");
    let directory = prepare_directory(&paths(root.path()), "keybindings").expect("prepare");
    fs::write(directory.join(".journal.tmp"), b"half").expect("stale");
    write_atomic(&directory, "journal", b"whole", owner()).expect("write");
    assert_eq!(
        read_file(&directory, "journal", 64, owner()).expect("read"),
        Some(b"whole".to_vec())
    );
}

#[test]
fn reads_refuse_links_loose_modes_foreign_hard_links_and_oversize_files() {
    let root = tempfile::tempdir().expect("root");
    let directory = prepare_directory(&paths(root.path()), "keybindings").expect("prepare");
    let outside = root.path().join("outside");
    fs::write(&outside, b"x").expect("outside");
    std::os::unix::fs::symlink(&outside, directory.join("linked")).expect("link");
    assert!(matches!(
        read_file(&directory, "linked", 8, owner()),
        Err(DurableError::Unsafe { .. })
    ));
    write_atomic(&directory, "loose", b"x", owner()).expect("write");
    fs::set_permissions(directory.join("loose"), fs::Permissions::from_mode(0o644)).expect("chmod");
    assert!(matches!(
        read_file(&directory, "loose", 8, owner()),
        Err(DurableError::Unsafe { .. })
    ));
    write_atomic(&directory, "hard", b"x", owner()).expect("write");
    fs::hard_link(directory.join("hard"), root.path().join("second")).expect("hard link");
    assert!(matches!(
        read_file(&directory, "hard", 8, owner()),
        Err(DurableError::Unsafe { .. })
    ));
    write_atomic(&directory, "big", &[7; 9], owner()).expect("write");
    assert!(matches!(
        read_file(&directory, "big", 8, owner()),
        Err(DurableError::Unsafe { .. })
    ));
    assert!(read_file(&directory, "big", 9, owner()).expect("exact cap").is_some());
}

#[test]
fn the_lease_excludes_a_second_holder_until_the_five_second_budget_expires() {
    let root = tempfile::tempdir().expect("root");
    let directory = prepare_directory(&paths(root.path()), "keybindings").expect("prepare");
    let clock = ManualClock::new();
    let cancel = CancellationToken::new();
    let first = DurableLease::exclusive(&directory, "lease", owner(), &wait(&clock, &cancel))
        .expect("first holder");
    let error = DurableLease::exclusive(&directory, "lease", owner(), &wait(&clock, &cancel))
        .expect_err("second holder waits then fails");
    assert!(matches!(error, DurableError::Busy));
    let sleeps = clock.sleeps();
    assert!(sleeps.iter().all(|slice| *slice <= Duration::from_millis(10)));
    assert_eq!(sleeps.iter().sum::<Duration>(), Duration::from_secs(5));
    // The file is never unlinked by release, and the lock is reusable.
    drop(first);
    assert!(directory.join("lease").exists());
    let _free = DurableLease::exclusive(&directory, "lease", owner(), &wait(&clock, &cancel))
        .expect("free");
}

#[test]
fn a_cancelled_wait_stops_within_one_quantum() {
    let root = tempfile::tempdir().expect("root");
    let directory = prepare_directory(&paths(root.path()), "keybindings").expect("prepare");
    let clock = ManualClock::new();
    let cancel = CancellationToken::new();
    let _held = DurableLease::exclusive(&directory, "lease", owner(), &wait(&clock, &cancel))
        .expect("holder");
    let waiter = CancellationToken::new();
    waiter.cancel();
    let error = DurableLease::exclusive(&directory, "lease", owner(), &wait(&clock, &waiter))
        .expect_err("cancelled");
    assert!(matches!(error, DurableError::Cancelled));
    assert!(clock.sleeps().is_empty());
}

#[test]
fn a_replaced_lock_path_or_directory_fails_closed() {
    let root = tempfile::tempdir().expect("root");
    let directory = prepare_directory(&paths(root.path()), "keybindings").expect("prepare");
    let clock = ManualClock::new();
    let cancel = CancellationToken::new();
    let lease = DurableLease::exclusive(&directory, "lease", owner(), &wait(&clock, &cancel))
        .expect("holder");
    lease.verify().expect("intact");
    fs::remove_file(directory.join("lease")).expect("unlink");
    fs::write(directory.join("lease"), b"").expect("recreate");
    fs::set_permissions(directory.join("lease"), fs::Permissions::from_mode(0o600)).expect("mode");
    assert!(matches!(lease.verify(), Err(DurableError::Replaced { .. })));

    let other = prepare_directory(&paths(root.path()), "second").expect("second");
    let lease =
        DurableLease::exclusive(&other, "lease", owner(), &wait(&clock, &cancel)).expect("holder");
    let moved = root.path().join("moved");
    fs::rename(&other, &moved).expect("move directory away");
    fs::create_dir(&other).expect("new directory");
    assert!(lease.verify().is_err());
}

#[test]
fn a_symlinked_lock_file_is_refused() {
    let root = tempfile::tempdir().expect("root");
    let directory = prepare_directory(&paths(root.path()), "keybindings").expect("prepare");
    let outside = root.path().join("outside");
    fs::write(&outside, b"").expect("outside");
    std::os::unix::fs::symlink(&outside, directory.join("lease")).expect("link");
    let clock = ManualClock::new();
    let error = DurableLease::exclusive(
        &directory,
        "lease",
        owner(),
        &wait(&clock, &CancellationToken::new()),
    )
    .expect_err("symlink lock");
    assert!(matches!(error, DurableError::Unsafe { .. }));
}
