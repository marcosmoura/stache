//! KB-04-03/04: the real journal and lease over temporary roots, with fake
//! `hidutil`. Symlinks, replaced inodes, a busy lease on fake time and
//! concurrent identities.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use stache_sdk::durable::{self, DurableLease};
use stache_sdk::{AppEnvironment, AppPaths, CancellationToken, LeaseClock, LeaseWait};

use super::super::engine::{ApplyError, CapsRemap, RestoreOutcome};
use super::super::hidutil::{Hidutil, HidutilError};
use super::super::mapping::{Mapping, OWNED};
use super::super::storage::{
    DIRECTORY, FsJournalStorage, FsLeases, JOURNAL_FILE, LOCK_FILE, LeaseFailure,
};
use super::super::testing::World;

const KEY_A: Mapping = Mapping { src: 100, dst: 200 };

fn paths(root: &std::path::Path) -> AppPaths {
    AppPaths::resolve(&AppEnvironment {
        home: Some(root.to_path_buf()),
        temporary: Some(root.to_path_buf()),
    })
    .expect("paths")
}

fn token() -> CancellationToken { CancellationToken::new() }

struct ManualClock {
    now: Mutex<Instant>,
    slept: Mutex<Duration>,
}

impl ManualClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            now: Mutex::new(Instant::now()),
            slept: Mutex::new(Duration::ZERO),
        })
    }
}

impl LeaseClock for ManualClock {
    fn now(&self) -> Instant { *self.now.lock().unwrap_or_else(PoisonError::into_inner) }

    fn sleep(&self, timeout: Duration, cancellation: &CancellationToken) -> bool {
        assert!(
            timeout <= Duration::from_millis(10),
            "cancellation checks stay within 10 ms"
        );
        *self.slept.lock().unwrap_or_else(PoisonError::into_inner) += timeout;
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) += timeout;
        cancellation.is_cancelled()
    }
}

fn real_remap(paths: &AppPaths, world: &World, clock: Option<Arc<ManualClock>>) -> CapsRemap {
    let leases = clock.map_or_else(
        || FsLeases::new(paths.clone()),
        |clock| FsLeases::with_clock(paths.clone(), clock),
    );
    CapsRemap::new(
        world.hidutil(),
        Arc::new(FsJournalStorage::new(paths.clone())),
        Arc::new(leases),
    )
}

fn durable_directory(paths: &AppPaths) -> std::path::PathBuf {
    paths.application_support_directory().join(DIRECTORY)
}

#[test]
fn kb_04_04_the_real_journal_is_private_durable_and_outside_cache_runtime_and_config() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let world = World::new(vec![KEY_A]);
    let remap = real_remap(&paths, &world, None);
    remap.apply(&token()).expect("apply");
    let directory = durable_directory(&paths);
    let journal = directory.join(JOURNAL_FILE);
    assert_eq!(
        fs::metadata(&journal).expect("journal").permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&directory).expect("dir").permissions().mode() & 0o777,
        0o700
    );
    assert!(!journal.starts_with(paths.cache_directory()));
    assert!(!journal.starts_with(paths.runtime_directory()));
    assert!(
        directory
            .to_string_lossy()
            .contains("Application Support/com.marcosmoura.stache.next/keybindings")
    );
    assert_eq!(remap.restore(&token()), RestoreOutcome::Restored);
    assert!(!journal.exists(), "retired after verified restoration");
    assert!(directory.join(LOCK_FILE).exists(), "the lock is never unlinked");
}

#[test]
fn kb_04_07_inspecting_without_a_journal_creates_no_directory_lease_or_process() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let world = World::new(vec![]);
    let remap = real_remap(&paths, &world, None);
    assert!(!remap.journal_present());
    assert_eq!(remap.restore(&token()), RestoreOutcome::NoJournal);
    assert!(!paths.application_support_directory().exists());
    assert_eq!((world.gets(), world.sets()), (0, 0));
}

#[test]
fn kb_04_04_a_busy_real_lease_expires_after_five_seconds_of_fake_time_with_no_mutation() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let world = World::new(vec![KEY_A]);
    let clock = ManualClock::new();
    let holder_directory = durable::prepare_directory(&paths, DIRECTORY).expect("directory");
    let holder = DurableLease::exclusive(
        &holder_directory,
        LOCK_FILE,
        paths.owner(),
        &LeaseWait::new(Duration::from_secs(1), token()),
    )
    .expect("another process holds the lease");
    let remap = real_remap(&paths, &world, Some(Arc::clone(&clock)));
    assert_eq!(remap.apply(&token()), Err(ApplyError::Lease(LeaseFailure::Busy)));
    assert_eq!(
        *clock.slept.lock().unwrap_or_else(PoisonError::into_inner),
        Duration::from_secs(5)
    );
    assert_eq!((world.gets(), world.sets()), (0, 0));
    assert!(!holder_directory.join(JOURNAL_FILE).exists());
    drop(holder);
    remap.apply(&token()).expect("free");
}

#[test]
fn kb_04_04_a_cancelled_lease_wait_mutates_nothing() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let world = World::new(vec![KEY_A]);
    let holder_directory = durable::prepare_directory(&paths, DIRECTORY).expect("directory");
    let _holder = DurableLease::exclusive(
        &holder_directory,
        LOCK_FILE,
        paths.owner(),
        &LeaseWait::new(Duration::from_secs(1), token()),
    )
    .expect("holder");
    let cancel = token();
    cancel.cancel();
    let remap = real_remap(&paths, &world, Some(ManualClock::new()));
    assert_eq!(
        remap.apply(&cancel),
        Err(ApplyError::Lease(LeaseFailure::Cancelled))
    );
    assert_eq!((world.gets(), world.sets()), (0, 0));
}

#[test]
fn kb_04_04_different_identities_and_config_roots_do_not_share_a_lease_but_one_identity_does() {
    let first_root = tempfile::tempdir().expect("first");
    let second_root = tempfile::tempdir().expect("second");
    let first = paths(first_root.path());
    let second = paths(second_root.path());
    let world_a = World::new(vec![KEY_A]);
    let world_b = World::new(vec![KEY_A]);
    // A holder on identity one does not affect identity two.
    let directory = durable::prepare_directory(&first, DIRECTORY).expect("directory");
    let _held = DurableLease::exclusive(
        &directory,
        LOCK_FILE,
        first.owner(),
        &LeaseWait::new(Duration::from_secs(1), token()),
    )
    .expect("holder");
    real_remap(&second, &world_b, Some(ManualClock::new()))
        .apply(&token())
        .expect("independent");
    assert_eq!(world_b.mappings(), vec![KEY_A, OWNED]);
    // The same identity from any config root is the same lease, whatever the
    // configuration directory is: the path derives from the identity only.
    let same_identity = real_remap(&first, &world_a, Some(ManualClock::new()));
    assert_eq!(
        same_identity.apply(&token()),
        Err(ApplyError::Lease(LeaseFailure::Busy))
    );
    assert_eq!(world_a.sets(), 0);
}

#[test]
fn kb_04_04_a_symlinked_journal_or_directory_fails_closed() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let world = World::new(vec![KEY_A]);
    let remap = real_remap(&paths, &world, None);
    remap.apply(&token()).expect("apply");
    let directory = durable_directory(&paths);
    let journal = directory.join(JOURNAL_FILE);
    let outside = root.path().join("outside");
    fs::rename(&journal, &outside).expect("move");
    std::os::unix::fs::symlink(&outside, &journal).expect("link");
    let outcome = remap.restore(&token());
    assert!(matches!(outcome, RestoreOutcome::Blocked(_)), "{outcome:?}");
    assert!(outside.exists());
    assert_eq!(world.mappings(), vec![KEY_A, OWNED]);
    assert!(matches!(remap.apply(&token()), Err(ApplyError::Blocked(_))));

    // A symlinked durable directory is refused by lease and storage alike.
    let other_root = tempfile::tempdir().expect("other");
    let other = paths_for_link(other_root.path());
    let world = World::new(vec![KEY_A]);
    let error = real_remap(&other, &world, None).apply(&token()).expect_err("symlink");
    assert!(
        matches!(error, ApplyError::Lease(LeaseFailure::Unsafe(_))),
        "{error:?}"
    );
    assert_eq!((world.gets(), world.sets()), (0, 0));
}

fn paths_for_link(root: &std::path::Path) -> AppPaths {
    let paths = paths(root);
    fs::create_dir_all(paths.application_support_directory()).expect("identity directory");
    let elsewhere = root.join("elsewhere");
    fs::create_dir(&elsewhere).expect("elsewhere");
    std::os::unix::fs::symlink(&elsewhere, durable_directory(&paths)).expect("link");
    paths
}

/// Replaces the lock inode on the first `get`, which runs after the lease is held.
struct ReplaceLockOnGet {
    inner: Arc<dyn Hidutil>,
    lock: std::path::PathBuf,
    done: Mutex<bool>,
}

impl Hidutil for ReplaceLockOnGet {
    fn get(&self, cancel: &CancellationToken) -> Result<Vec<u8>, HidutilError> {
        let mut done = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        if !*done {
            *done = true;
            fs::remove_file(&self.lock).expect("unlink lock");
            fs::write(&self.lock, b"").expect("recreate lock");
            fs::set_permissions(&self.lock, fs::Permissions::from_mode(0o600)).expect("mode");
        }
        drop(done);
        self.inner.get(cancel)
    }

    fn set(&self, payload: &str, cancel: &CancellationToken) -> Result<(), HidutilError> {
        self.inner.set(payload, cancel)
    }

    fn unreaped(&self) -> usize { self.inner.unreaped() }
}

#[test]
fn kb_04_04_a_lock_inode_replaced_while_held_stops_the_write_ahead_journal() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let world = World::new(vec![KEY_A]);
    let directory = durable_directory(&paths);
    let hidutil = Arc::new(ReplaceLockOnGet {
        inner: world.hidutil(),
        lock: directory.join(LOCK_FILE),
        done: Mutex::new(false),
    });
    let remap = CapsRemap::new(
        hidutil,
        Arc::new(FsJournalStorage::new(paths.clone())),
        Arc::new(FsLeases::new(paths)),
    );
    let error = remap.apply(&token()).expect_err("replaced identity");
    assert!(matches!(error, ApplyError::Journal(_)), "{error:?}");
    assert_eq!(
        world.sets(),
        0,
        "no native mutation after the identity check failed"
    );
    assert!(!directory.join(JOURNAL_FILE).exists());
}

#[test]
fn kb_04_03_a_corrupt_real_journal_is_never_deleted() {
    let root = tempfile::tempdir().expect("root");
    let paths = paths(root.path());
    let directory = durable::prepare_directory(&paths, DIRECTORY).expect("directory");
    durable::write_atomic(&directory, JOURNAL_FILE, b"{garbage", paths.owner()).expect("plant");
    let world = World::new(vec![KEY_A, OWNED]);
    let remap = real_remap(&paths, &world, None);
    let outcome = remap.restore(&token());
    assert!(matches!(outcome, RestoreOutcome::Blocked(_)), "{outcome:?}");
    assert_eq!(
        fs::read(directory.join(JOURNAL_FILE)).expect("kept"),
        b"{garbage"
    );
    assert_eq!(world.sets(), 0);
    // A loose-mode journal is untrusted too.
    durable::write_atomic(&directory, JOURNAL_FILE, b"x", paths.owner()).expect("plant");
    fs::set_permissions(directory.join(JOURNAL_FILE), fs::Permissions::from_mode(0o644))
        .expect("mode");
    assert!(matches!(remap.restore(&token()), RestoreOutcome::Blocked(_)));
}
