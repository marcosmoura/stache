//! Cross-process coordination for the identity cache.
//!
//! A [`CacheLease`] is an advisory lock on a private lock file in the runtime
//! directory, never inside the cache root. Deleting and recreating the cache
//! therefore keeps the same lock identity. Shared leases cover ordinary cache
//! work; one exclusive lease covers whole-root `cache clear`.
//!
//! Participants never unlink the lock file, including clear and normal
//! shutdown. The kernel releases the lock when the holder exits, so a crash
//! cannot leave the cache permanently busy. This coordinates cooperating Stache
//! Next processes; it is not protection against OS or temporary-directory
//! cleaners, or against malicious same-UID mutation.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{fmt, fs, io};

use crate::ipc::ServerError;
use crate::lockfile::{self, LockError, LockIdentity};
use crate::paths::AppPaths;
use crate::threading::CancellationToken;

/// How long one acquisition attempt waits between lock polls.
const LEASE_QUANTUM: Duration = Duration::from_millis(10);

/// Time and wake source for a bounded lease wait.
///
/// Production uses the system clock. Tests inject a manual clock so a
/// five-second budget can expire without a real five-second wait.
pub trait LeaseClock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Instant;

    /// Sleeps at most `timeout`, waking immediately on cancellation.
    ///
    /// Returns `true` when the token is cancelled.
    fn sleep(&self, timeout: Duration, cancellation: &CancellationToken) -> bool;
}

struct SystemLeaseClock;

impl LeaseClock for SystemLeaseClock {
    fn now(&self) -> Instant { Instant::now() }

    fn sleep(&self, timeout: Duration, cancellation: &CancellationToken) -> bool {
        cancellation.wait(timeout)
    }
}

/// Budget, cancellation and clock for one bounded lease acquisition.
pub struct LeaseWait {
    budget: Duration,
    cancellation: CancellationToken,
    clock: Arc<dyn LeaseClock>,
}

impl LeaseWait {
    /// A wait bounded by `budget` on the system clock.
    #[must_use]
    pub fn new(budget: Duration, cancellation: CancellationToken) -> Self {
        Self::with_clock(budget, cancellation, Arc::new(SystemLeaseClock))
    }

    /// A wait with an injected clock.
    #[must_use]
    pub fn with_clock(
        budget: Duration,
        cancellation: CancellationToken,
        clock: Arc<dyn LeaseClock>,
    ) -> Self {
        Self { budget, cancellation, clock }
    }

    fn deadline(&self) -> Instant {
        let now = self.clock.now();
        now.checked_add(self.budget).unwrap_or(now)
    }

    fn sleep(&self, timeout: Duration) -> bool { self.clock.sleep(timeout, &self.cancellation) }
}

/// A held shared or exclusive cache lease.
///
/// Dropping the value releases the kernel lock and removes nothing. A shared
/// lease is never upgraded in place: drop it and acquire again before critical
/// work if exclusive access is required.
#[must_use]
pub struct CacheLease {
    // Held only for its file description: dropping it releases the lock.
    _file: fs::File,
    path: PathBuf,
    directory: PathBuf,
    owner: u32,
    identity: LockIdentity,
    directory_identity: LockIdentity,
}

impl CacheLease {
    /// Acquires a shared lease, waiting at most the budget in `wait`.
    ///
    /// # Errors
    /// Returns [`CacheLeaseError::Busy`] when the budget expires,
    /// [`CacheLeaseError::Cancelled`] when the token fires, or a typed
    /// unsafe, replaced or I/O error.
    pub fn shared(paths: &AppPaths, wait: &LeaseWait) -> Result<Self, CacheLeaseError> {
        Self::acquire(paths, wait, false)
    }

    /// Acquires the exclusive lease used by whole-root `cache clear`.
    ///
    /// # Errors
    /// Returns [`CacheLeaseError::Busy`] when the budget expires,
    /// [`CacheLeaseError::Cancelled`] when the token fires, or a typed
    /// unsafe, replaced or I/O error.
    pub fn exclusive(paths: &AppPaths, wait: &LeaseWait) -> Result<Self, CacheLeaseError> {
        Self::acquire(paths, wait, true)
    }

    fn acquire(
        paths: &AppPaths,
        wait: &LeaseWait,
        exclusive: bool,
    ) -> Result<Self, CacheLeaseError> {
        let directory = paths.runtime_directory().to_path_buf();
        crate::ipc::server::prepare_runtime_directory(&directory, paths.owner())
            .map_err(|error| prepare_error(&directory, error))?;
        let path = paths.cache_lease_path();
        let opened = lockfile::open(&path, &directory, paths.owner())?;
        if wait.cancellation.is_cancelled() {
            return Err(CacheLeaseError::Cancelled);
        }
        let deadline = wait.deadline();
        loop {
            let outcome = if exclusive {
                opened.file.try_lock()
            } else {
                opened.file.try_lock_shared()
            };
            match outcome {
                Ok(()) => break,
                Err(fs::TryLockError::WouldBlock) => {
                    let now = wait.clock.now();
                    if now >= deadline {
                        return Err(CacheLeaseError::Busy);
                    }
                    if wait.cancellation.is_cancelled() {
                        return Err(CacheLeaseError::Cancelled);
                    }
                    let slice = LEASE_QUANTUM.min(deadline.saturating_duration_since(now));
                    if wait.sleep(slice) {
                        return Err(CacheLeaseError::Cancelled);
                    }
                }
                Err(fs::TryLockError::Error(error)) => {
                    return Err(CacheLeaseError::Io { action: "lock", path, error });
                }
            }
        }
        let lease = Self {
            _file: opened.file,
            path,
            directory,
            owner: paths.owner(),
            identity: opened.identity,
            directory_identity: opened.directory,
        };
        if wait.cancellation.is_cancelled() {
            return Err(CacheLeaseError::Cancelled);
        }
        lease.verify()?;
        Ok(lease)
    }

    /// The lock file path.
    #[must_use]
    pub fn path(&self) -> &Path { &self.path }

    /// Rechecks the lock path and runtime directory against the held inode.
    ///
    /// Call this before critical work. A replaced or removed path fails closed
    /// with a typed error; reacquiring is the caller's choice by dropping this
    /// lease and acquiring again, and only before critical work begins.
    ///
    /// # Errors
    /// Returns [`CacheLeaseError::Replaced`] when the path no longer names the
    /// held inode, [`CacheLeaseError::Unsafe`] for unsafe metadata, or
    /// [`CacheLeaseError::Io`] when a path cannot be inspected.
    pub fn verify(&self) -> Result<(), CacheLeaseError> {
        lockfile::verify(
            &self.path,
            &self.directory,
            self.owner,
            self.identity,
            self.directory_identity,
        )
        .map_err(CacheLeaseError::from)
    }
}

fn prepare_error(directory: &Path, error: ServerError) -> CacheLeaseError {
    match error {
        ServerError::Io { action, path, error } => CacheLeaseError::Io { action, path, error },
        other => CacheLeaseError::Unsafe {
            path: directory.to_path_buf(),
            message: other.to_string(),
        },
    }
}

impl From<LockError> for CacheLeaseError {
    fn from(error: LockError) -> Self {
        match error {
            LockError::Replaced { path } => Self::Replaced { path },
            LockError::Unsafe { path, message } => Self::Unsafe { path, message },
            LockError::Io { action, path, error } => Self::Io { action, path, error },
        }
    }
}

/// Why a cache lease could not be acquired or verified.
#[derive(Debug)]
pub enum CacheLeaseError {
    /// An incompatible lease was held for the whole budget.
    Busy,
    /// Cancellation was requested before the lease could be acquired.
    Cancelled,
    /// The lock path or runtime directory no longer matches the held inode.
    Replaced {
        /// The path whose identity changed.
        path: PathBuf,
    },
    /// A managed path is a link, foreign-owned, the wrong type or unsafe mode.
    Unsafe {
        /// The rejected path.
        path: PathBuf,
        /// Why it was rejected.
        message: String,
    },
    /// A filesystem operation failed.
    Io {
        /// What was attempted.
        action: &'static str,
        /// The affected path.
        path: PathBuf,
        /// The underlying failure.
        error: io::Error,
    },
}

impl fmt::Display for CacheLeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("cache lease is busy"),
            Self::Cancelled => f.write_str("cache lease wait was cancelled"),
            Self::Replaced { path } => {
                write!(f, "cache lease identity changed at {}", path.display())
            }
            Self::Unsafe { path, message } => {
                write!(f, "{} is unsafe: {message}", path.display())
            }
            Self::Io { action, path, error } => {
                write!(f, "cannot {action} {}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for CacheLeaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            Self::Busy | Self::Cancelled | Self::Replaced { .. } | Self::Unsafe { .. } => None,
        }
    }
}
