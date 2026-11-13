//! One running desktop per bundle identity.
//!
//! The lock lives in the build identity's runtime directory, so a release and a
//! development build never share it. Opening it is the first desktop step:
//! a second launch exits before the tray, registry, configuration watcher or
//! socket server exists.
//!
//! The lock is a private regular file opened without following symlinks and
//! locked with an advisory exclusive lock. The kernel releases it when the
//! process exits, so a crash cannot leave an unrecoverable instance.

use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

use crate::lockfile::{self, LockError};
use crate::paths::AppPaths;

/// The instance lock file name inside the runtime directory.
pub const INSTANCE_FILE_NAME: &str = "instance.lock";

/// Why the desktop could not claim the single-instance lock.
#[derive(Debug)]
pub enum InstanceError {
    /// Another process already holds the lock.
    AlreadyRunning {
        /// The runtime directory owned by that process.
        runtime: PathBuf,
    },
    /// The runtime directory is missing, unsafe or not owned by this user.
    Runtime {
        /// The directory that could not be prepared.
        path: PathBuf,
        /// Why preparation failed.
        message: String,
    },
    /// The lock file could not be opened, inspected or locked.
    Io {
        /// What was attempted.
        action: &'static str,
        /// The affected path.
        path: PathBuf,
        /// The underlying failure.
        error: io::Error,
    },
}

impl fmt::Display for InstanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning { runtime } => {
                write!(f, "another instance already owns {}", runtime.display())
            }
            Self::Runtime { path, message } => {
                write!(f, "runtime directory {} is unsafe: {message}", path.display())
            }
            Self::Io { action, path, error } => {
                write!(f, "cannot {action} {}: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for InstanceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            Self::AlreadyRunning { .. } | Self::Runtime { .. } => None,
        }
    }
}

/// The exclusive claim held for the lifetime of one desktop process.
///
/// Dropping the value releases the kernel lock. The lock file is deliberately
/// left in place: removing an inode while a second process might open it would
/// allow both to believe they own the identity.
#[must_use]
pub struct InstanceLock {
    // Held only for its file description: dropping it releases the kernel lock.
    _file: fs::File,
    path: PathBuf,
}

impl InstanceLock {
    /// Claims the identity lock, creating the private runtime directory if
    /// necessary.
    ///
    /// # Errors
    /// Returns [`InstanceError::AlreadyRunning`] when another process owns the
    /// lock, or a typed I/O/runtime error when the directory or lock file is
    /// unsafe.
    pub fn acquire(paths: &AppPaths) -> Result<Self, InstanceError> {
        let runtime = paths.runtime_directory();
        crate::ipc::server::prepare_runtime_directory(runtime, paths.owner()).map_err(|error| {
            InstanceError::Runtime {
                path: runtime.to_path_buf(),
                message: error.to_string(),
            }
        })?;
        let path = runtime.join(INSTANCE_FILE_NAME);
        let lock = lockfile::open(&path, runtime, paths.owner()).map_err(instance_error)?;
        lock.file.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => {
                InstanceError::AlreadyRunning { runtime: runtime.to_path_buf() }
            }
            fs::TryLockError::Error(error) => InstanceError::Io {
                action: "lock",
                path: path.clone(),
                error,
            },
        })?;
        lockfile::verify(&path, runtime, paths.owner(), lock.identity, lock.directory)
            .map_err(instance_error)?;
        Ok(Self { _file: lock.file, path })
    }

    /// The lock file path.
    #[must_use]
    pub fn path(&self) -> &Path { &self.path }
}

fn instance_error(error: LockError) -> InstanceError {
    match error {
        LockError::Io { action, path, error } => InstanceError::Io { action, path, error },
        LockError::Replaced { path } => InstanceError::Runtime {
            path,
            message: "lock identity changed while acquiring it".to_owned(),
        },
        LockError::Unsafe { path, message } => InstanceError::Runtime { path, message },
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::paths::{AppEnvironment, IDENTITY};

    fn paths(root: &Path) -> AppPaths {
        AppPaths::resolve(&AppEnvironment {
            home: Some(root.to_path_buf()),
            temporary: Some(root.to_path_buf()),
        })
        .expect("paths")
    }

    #[test]
    fn the_lock_lives_in_the_build_identity_runtime() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        let lock = InstanceLock::acquire(&paths).expect("first");
        assert_eq!(lock.path(), paths.runtime_directory().join(INSTANCE_FILE_NAME));
        assert!(lock.path().to_string_lossy().contains(IDENTITY));
    }

    #[test]
    fn a_second_claim_is_refused_until_the_first_is_dropped() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        let first = InstanceLock::acquire(&paths).expect("first");
        assert!(matches!(
            InstanceLock::acquire(&paths),
            Err(InstanceError::AlreadyRunning { .. })
        ));
        drop(first);
        assert!(InstanceLock::acquire(&paths).is_ok());
    }

    #[test]
    fn a_symlinked_lock_is_refused() {
        let root = tempfile::tempdir().expect("root");
        let paths = paths(root.path());
        crate::ipc::server::prepare_runtime_directory(paths.runtime_directory(), paths.owner())
            .expect("runtime");
        let outside = root.path().join("outside");
        fs::write(&outside, b"foreign").expect("outside");
        std::os::unix::fs::symlink(&outside, paths.runtime_directory().join(INSTANCE_FILE_NAME))
            .expect("symlink");
        assert!(matches!(
            InstanceLock::acquire(&paths),
            Err(InstanceError::Io { .. } | InstanceError::Runtime { .. })
        ));
    }
}
