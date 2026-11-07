//! Durable journal storage and the identity-wide lease.
//!
//! Both are traits so the transaction logic runs over fakes with fault
//! injection at every durable step. The real implementations sit on
//! [`stache_sdk::durable`]: a private no-follow `0600` journal replaced
//! atomically (temporary, sync, rename, directory sync) and an exclusive lock
//! file that is never unlinked.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use stache_sdk::durable::{self, DurableError, DurableLease};
use stache_sdk::{AppPaths, CancellationToken, LeaseClock, LeaseWait};

/// The durable subdirectory of the identity's Application Support directory.
pub const DIRECTORY: &str = "keybindings";
/// The journal file name.
pub const JOURNAL_FILE: &str = "caps-remap.journal";
/// The persistent coordination lock file name. It is never unlinked.
pub const LOCK_FILE: &str = "caps-remap.lock";
/// Longest accepted journal.
pub const JOURNAL_MAX_BYTES: usize = 4 * 1024;
/// How long to wait for the identity-wide lease.
pub const LEASE_BUDGET: Duration = Duration::from_secs(5);

/// A journal storage failure. The message is for diagnostics only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageError(pub String);

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}

impl std::error::Error for StorageError {}

impl From<DurableError> for StorageError {
    fn from(error: DurableError) -> Self { Self(error.to_string()) }
}

/// Durable storage of the single journal.
pub trait JournalStorage: Send + Sync {
    /// Reads the journal bytes. `None` means there is no journal.
    ///
    /// # Errors
    /// Returns a [`StorageError`] for an unsafe, oversized or unreadable file.
    fn read(&self) -> Result<Option<Vec<u8>>, StorageError>;

    /// Atomically and durably replaces the journal.
    ///
    /// # Errors
    /// Returns a [`StorageError`]. On failure before the rename the previous
    /// journal is intact; after it the new bytes may or may not be durable.
    fn write(&self, bytes: &[u8]) -> Result<(), StorageError>;

    /// Durably removes the journal. Only call after restoration is confirmed.
    ///
    /// # Errors
    /// Returns a [`StorageError`].
    fn remove(&self) -> Result<(), StorageError>;
}

/// Why the lease could not be acquired or is no longer valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseFailure {
    /// Another holder kept the lease for the whole budget.
    Busy,
    /// Cancellation fired while waiting.
    Cancelled,
    /// A path was replaced, linked, foreign or otherwise unsafe, or an I/O
    /// operation failed.
    Unsafe(String),
}

impl std::fmt::Display for LeaseFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("the Caps coordination lease is busy"),
            Self::Cancelled => f.write_str("waiting for the Caps coordination lease was cancelled"),
            Self::Unsafe(message) => write!(f, "the Caps coordination lease is unsafe: {message}"),
        }
    }
}

impl std::error::Error for LeaseFailure {}

impl From<DurableError> for LeaseFailure {
    fn from(error: DurableError) -> Self {
        match error {
            DurableError::Busy => Self::Busy,
            DurableError::Cancelled => Self::Cancelled,
            other => Self::Unsafe(other.to_string()),
        }
    }
}

/// A held lease. Dropping releases it and removes nothing.
pub trait LeaseGuard: Send {
    /// Rechecks the lock path and directory identity. Called before every
    /// mutation.
    ///
    /// # Errors
    /// Returns [`LeaseFailure::Unsafe`] when the identity changed.
    fn verify(&self) -> Result<(), LeaseFailure>;
}

/// The identity-wide Caps lease.
pub trait CapsLeases: Send + Sync {
    /// Acquires the lease within [`LEASE_BUDGET`], checking `cancel` at most
    /// every 10 ms.
    ///
    /// # Errors
    /// Returns a [`LeaseFailure`]; no mutation has happened.
    fn acquire(&self, cancel: &CancellationToken) -> Result<Box<dyn LeaseGuard>, LeaseFailure>;
}

/// The real journal under `Application Support/<identity>/keybindings/`.
pub struct FsJournalStorage {
    paths: AppPaths,
}

impl FsJournalStorage {
    /// Storage for one identity.
    #[must_use]
    pub const fn new(paths: AppPaths) -> Self { Self { paths } }

    fn directory(&self) -> Result<PathBuf, StorageError> {
        durable::existing_directory(&self.paths, DIRECTORY)?
            .ok_or_else(|| StorageError("the durable directory does not exist".to_owned()))
    }
}

impl JournalStorage for FsJournalStorage {
    fn read(&self) -> Result<Option<Vec<u8>>, StorageError> {
        // Reading must not create the directory: no directory means no journal.
        let Some(directory) = durable::existing_directory(&self.paths, DIRECTORY)? else {
            return Ok(None);
        };
        Ok(durable::read_file(
            &directory,
            JOURNAL_FILE,
            JOURNAL_MAX_BYTES,
            self.paths.owner(),
        )?)
    }

    fn write(&self, bytes: &[u8]) -> Result<(), StorageError> {
        let directory = self.directory()?;
        Ok(durable::write_atomic(
            &directory,
            JOURNAL_FILE,
            bytes,
            self.paths.owner(),
        )?)
    }

    fn remove(&self) -> Result<(), StorageError> {
        let directory = self.directory()?;
        Ok(durable::remove_file(
            &directory,
            JOURNAL_FILE,
            self.paths.owner(),
        )?)
    }
}

/// The real lease over [`DurableLease`].
pub struct FsLeases {
    paths: AppPaths,
    clock: Option<Arc<dyn LeaseClock>>,
}

impl FsLeases {
    /// The lease for one identity on the system clock.
    #[must_use]
    pub const fn new(paths: AppPaths) -> Self { Self { paths, clock: None } }

    /// The lease with an injected clock, so a 5 s wait needs no real 5 s.
    #[must_use]
    pub fn with_clock(paths: AppPaths, clock: Arc<dyn LeaseClock>) -> Self {
        Self { paths, clock: Some(clock) }
    }
}

struct FsGuard(DurableLease);

impl LeaseGuard for FsGuard {
    fn verify(&self) -> Result<(), LeaseFailure> { self.0.verify().map_err(LeaseFailure::from) }
}

impl CapsLeases for FsLeases {
    fn acquire(&self, cancel: &CancellationToken) -> Result<Box<dyn LeaseGuard>, LeaseFailure> {
        let directory = durable::prepare_directory(&self.paths, DIRECTORY)?;
        let wait = self.clock.as_ref().map_or_else(
            || LeaseWait::new(LEASE_BUDGET, cancel.clone()),
            |clock| LeaseWait::with_clock(LEASE_BUDGET, cancel.clone(), Arc::clone(clock)),
        );
        let lease = DurableLease::exclusive(&directory, LOCK_FILE, self.paths.owner(), &wait)?;
        Ok(Box::new(FsGuard(lease)))
    }
}
