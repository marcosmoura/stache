//! Durable identity storage: a private directory, an identity-wide lease and
//! crash-safe small files.
//!
//! Durable state lives in a named subdirectory of the Application Support
//! directory of [`AppPaths`], never in the clearable cache, the ephemeral
//! runtime directory or a user-selected configuration directory. Every path is
//! opened without following links, must be owned by this user and is private
//! (`0700` directories, `0600` regular files with one link).
//!
//! [`DurableLease`] is an exclusive advisory lock on a private lock file in that
//! directory. The file is never unlinked; the kernel releases the lock when the
//! holder exits. The identity and the directory are checked against the held
//! inode after locking and again by [`DurableLease::verify`], so a replaced or
//! removed path fails closed. This coordinates cooperating Stache
//! processes. It is not a compare-and-swap against other programs and not
//! protection against a same-user process that rewrites the files.
//!
//! [`write_atomic`] replaces a file by writing a temporary sibling, syncing it,
//! renaming it over the target and syncing the directory, so a reader sees the
//! old bytes or the new bytes and a failure before the rename changes nothing.

use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

use rustix::fs::{Mode, OFlags};

use crate::cache_lease::{LEASE_QUANTUM, LeaseWait};
use crate::lockfile::{self, LockError, LockIdentity};
use crate::paths::AppPaths;

/// Why a durable path, lease or file operation failed.
#[derive(Debug)]
pub enum DurableError {
    /// The lease stayed held by another holder for the whole budget.
    Busy,
    /// Cancellation was requested before the lease could be acquired.
    Cancelled,
    /// A path no longer names the inode or directory that was opened.
    Replaced {
        /// The path whose identity changed.
        path: PathBuf,
    },
    /// A path is a link, foreign-owned, the wrong type, an unsafe mode or too
    /// large.
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

impl fmt::Display for DurableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("durable lease is busy"),
            Self::Cancelled => f.write_str("durable lease wait was cancelled"),
            Self::Replaced { path } => write!(f, "identity changed at {}", path.display()),
            Self::Unsafe { path, message } => write!(f, "{} is unsafe: {message}", path.display()),
            Self::Io { action, path, error } => {
                write!(f, "cannot {action} {}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for DurableError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl From<LockError> for DurableError {
    fn from(error: LockError) -> Self {
        match error {
            LockError::Replaced { path } => Self::Replaced { path },
            LockError::Unsafe { path, message } => Self::Unsafe { path, message },
            LockError::Io { action, path, error } => Self::Io { action, path, error },
        }
    }
}

fn io_error(action: &'static str, path: &Path, error: impl Into<io::Error>) -> DurableError {
    DurableError::Io {
        action,
        path: path.to_path_buf(),
        error: error.into(),
    }
}

fn check_directory(path: &Path, metadata: &fs::Metadata, owner: u32) -> Result<(), DurableError> {
    let unsafe_path = |message: &str| DurableError::Unsafe {
        path: path.to_path_buf(),
        message: message.to_owned(),
    };
    if metadata.file_type().is_symlink() {
        return Err(unsafe_path("directory must not be a symbolic link"));
    }
    if !metadata.is_dir() {
        return Err(unsafe_path("path is not a directory"));
    }
    if metadata.uid() != owner {
        return Err(unsafe_path("directory is owned by another user"));
    }
    Ok(())
}

fn create_private_directory(path: &Path, owner: u32) -> Result<(), DurableError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => check_directory(path, &metadata, owner)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_error("create", path, error)),
            }
            let metadata =
                fs::symlink_metadata(path).map_err(|error| io_error("inspect", path, error))?;
            check_directory(path, &metadata, owner)?;
        }
        Err(error) => return Err(io_error("inspect", path, error)),
    }
    Ok(())
}

/// Creates (when missing) and validates the durable directory `name` under the
/// identity's Application Support directory.
///
/// The Application Support root itself is the platform's standard location and
/// is created with default permissions only when absent; the identity directory
/// and `name` are created `0700`.
///
/// # Errors
/// Returns [`DurableError`] for a link, a foreign owner, a non-directory or a
/// filesystem failure.
pub fn prepare_directory(paths: &AppPaths, name: &str) -> Result<PathBuf, DurableError> {
    let identity = paths.application_support_directory();
    if let Some(root) = identity.parent() {
        fs::create_dir_all(root).map_err(|error| io_error("create", root, error))?;
    }
    create_private_directory(identity, paths.owner())?;
    let directory = identity.join(name);
    create_private_directory(&directory, paths.owner())?;
    Ok(directory)
}

/// Returns the durable directory `name` when it already exists, without
/// creating anything.
///
/// # Errors
/// Returns [`DurableError`] when it exists but is unsafe.
pub fn existing_directory(paths: &AppPaths, name: &str) -> Result<Option<PathBuf>, DurableError> {
    let directory = paths.application_support_directory().join(name);
    match fs::symlink_metadata(&directory) {
        Ok(metadata) => {
            check_directory(&directory, &metadata, paths.owner())?;
            Ok(Some(directory))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error("inspect", &directory, error)),
    }
}

/// A held exclusive durable lease.
///
/// Dropping releases the kernel lock and removes nothing.
#[must_use]
pub struct DurableLease {
    // Held only for its file description: dropping it releases the lock.
    _file: fs::File,
    path: PathBuf,
    directory: PathBuf,
    owner: u32,
    identity: LockIdentity,
    directory_identity: LockIdentity,
}

impl fmt::Debug for DurableLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DurableLease").field("path", &self.path).finish_non_exhaustive()
    }
}

impl DurableLease {
    /// Acquires the exclusive lease on `directory/file_name`, waiting at most
    /// the budget in `wait` and checking cancellation at most every 10 ms.
    ///
    /// # Errors
    /// Returns [`DurableError::Busy`] when the budget expires,
    /// [`DurableError::Cancelled`] when the token fires, or a typed unsafe,
    /// replaced or I/O error. Nothing is mutated on failure except creating the
    /// lock file itself.
    pub fn exclusive(
        directory: &Path,
        file_name: &str,
        owner: u32,
        wait: &LeaseWait,
    ) -> Result<Self, DurableError> {
        let path = directory.join(file_name);
        let opened = lockfile::open(&path, directory, owner)?;
        if wait.cancellation.is_cancelled() {
            return Err(DurableError::Cancelled);
        }
        let deadline = wait.deadline();
        loop {
            match opened.file.try_lock() {
                Ok(()) => break,
                Err(fs::TryLockError::WouldBlock) => {
                    let now = wait.clock.now();
                    if now >= deadline {
                        return Err(DurableError::Busy);
                    }
                    if wait.cancellation.is_cancelled() {
                        return Err(DurableError::Cancelled);
                    }
                    let slice = LEASE_QUANTUM.min(deadline.saturating_duration_since(now));
                    if wait.sleep(slice) {
                        return Err(DurableError::Cancelled);
                    }
                }
                Err(fs::TryLockError::Error(error)) => {
                    return Err(io_error("lock", &path, error));
                }
            }
        }
        let lease = Self {
            _file: opened.file,
            path,
            directory: directory.to_path_buf(),
            owner,
            identity: opened.identity,
            directory_identity: opened.directory,
        };
        if wait.cancellation.is_cancelled() {
            return Err(DurableError::Cancelled);
        }
        lease.verify()?;
        Ok(lease)
    }

    /// Rechecks the lock path and directory against the held inode.
    ///
    /// Call before each mutation. A replaced or removed path fails closed.
    ///
    /// # Errors
    /// Returns [`DurableError::Replaced`], [`DurableError::Unsafe`] or
    /// [`DurableError::Io`].
    pub fn verify(&self) -> Result<(), DurableError> {
        lockfile::verify(
            &self.path,
            &self.directory,
            self.owner,
            self.identity,
            self.directory_identity,
        )
        .map_err(DurableError::from)
    }

    /// The lock file path.
    #[must_use]
    pub fn path(&self) -> &Path { &self.path }
}

fn is_private_file(metadata: &fs::Metadata, owner: u32) -> bool {
    metadata.is_file()
        && metadata.uid() == owner
        && metadata.mode() & 0o777 == 0o600
        && metadata.nlink() == 1
}

/// Reads a private regular file of at most `max` bytes. `None` means absent.
///
/// # Errors
/// Returns [`DurableError::Unsafe`] for a link, foreign owner, unsafe mode or a
/// file larger than `max`, and [`DurableError::Io`] for a read failure.
pub fn read_file(
    directory: &Path,
    name: &str,
    max: usize,
    owner: u32,
) -> Result<Option<Vec<u8>>, DurableError> {
    let path = directory.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !is_private_file(&metadata, owner) {
                return Err(DurableError::Unsafe {
                    path,
                    message: "must be a private, owned regular file".to_owned(),
                });
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("inspect", &path, error)),
    }
    let fd = match rustix::fs::open(
        &path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
        Err(error) => return Err(io_error("open", &path, error)),
    };
    let file = fs::File::from(fd);
    let metadata = file.metadata().map_err(|error| io_error("inspect", &path, error))?;
    if !is_private_file(&metadata, owner) {
        return Err(DurableError::Unsafe {
            path,
            message: "must be a private, owned regular file".to_owned(),
        });
    }
    let mut bytes = Vec::new();
    file.take(u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read", &path, error))?;
    if bytes.len() > max {
        return Err(DurableError::Unsafe {
            path,
            message: format!("is larger than {max} bytes"),
        });
    }
    Ok(Some(bytes))
}

fn temp_name(name: &str) -> String { format!(".{name}.tmp") }

fn sync_directory(directory: &Path) -> Result<(), DurableError> {
    let fd = rustix::fs::open(
        directory,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| io_error("open directory", directory, error))?;
    fs::File::from(fd)
        .sync_all()
        .map_err(|error| io_error("sync directory", directory, error))
}

fn checked_directory(directory: &Path, owner: u32) -> Result<(), DurableError> {
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| io_error("inspect directory", directory, error))?;
    check_directory(directory, &metadata, owner)
}

/// Atomically replaces `directory/name` with `bytes`.
///
/// Writes `.name.tmp` (`0600`, exclusive, no-follow), syncs it, renames it over
/// the target and syncs the directory. A failure before the rename leaves the
/// previous file untouched.
///
/// # Errors
/// Returns [`DurableError`] when any step fails.
pub fn write_atomic(
    directory: &Path,
    name: &str,
    bytes: &[u8],
    owner: u32,
) -> Result<(), DurableError> {
    checked_directory(directory, owner)?;
    let target = directory.join(name);
    let temp = directory.join(temp_name(name));
    match fs::remove_file(&temp) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("remove stale temporary", &temp, error)),
    }
    let fd = rustix::fs::open(
        &temp,
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|error| io_error("create temporary", &temp, error))?;
    let mut file = fs::File::from(fd);
    let staged = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| io_error("write temporary", &temp, error));
    drop(file);
    if let Err(error) = staged {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temp, &target) {
        let _ = fs::remove_file(&temp);
        return Err(io_error("rename", &target, error));
    }
    sync_directory(directory)
}

/// Removes `directory/name` and syncs the directory. An absent file is success.
///
/// # Errors
/// Returns [`DurableError`] when the removal or the sync fails.
pub fn remove_file(directory: &Path, name: &str, owner: u32) -> Result<(), DurableError> {
    checked_directory(directory, owner)?;
    let target = directory.join(name);
    match fs::remove_file(&target) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("remove", &target, error)),
    }
    sync_directory(directory)
}

#[cfg(test)]
mod tests;
