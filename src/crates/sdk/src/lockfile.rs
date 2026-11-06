//! Private lock files shared by the instance lock and cache leases.
//!
//! Both open a `0600` regular file without following symbolic links, check
//! that it is owned by this user and has one hard link, and compare the
//! identity of the held file and its directory with the paths after locking.
//! A missing, replaced or malformed path fails closed instead of being
//! recreated under a split identity.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

use rustix::fs::{Mode, OFlags};

/// Device and inode identity of a file or directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockIdentity {
    device: u64,
    inode: u64,
}

impl LockIdentity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

/// An opened private lock file with the identities captured before locking.
pub struct PrivateLock {
    pub(crate) file: fs::File,
    pub(crate) identity: LockIdentity,
    pub(crate) directory: LockIdentity,
}

/// Why a private lock file could not be opened or verified.
pub enum LockError {
    /// The lock path or its directory no longer names the opened inode.
    Replaced { path: PathBuf },
    /// The path is a link, foreign-owned, the wrong type or an unsafe mode.
    Unsafe { path: PathBuf, message: String },
    /// A filesystem operation failed.
    Io {
        action: &'static str,
        path: PathBuf,
        error: io::Error,
    },
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Replaced { path } => {
                write!(f, "lock identity changed at {}", path.display())
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

impl fmt::Debug for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Display::fmt(self, f) }
}

/// Opens or creates the private lock file inside an existing runtime directory.
pub fn open(path: &Path, directory: &Path, owner: u32) -> Result<PrivateLock, LockError> {
    let directory_metadata = fs::symlink_metadata(directory).map_err(|error| LockError::Io {
        action: "inspect runtime",
        path: directory.to_path_buf(),
        error,
    })?;
    ensure_directory(directory, &directory_metadata, owner)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(LockError::Unsafe {
                path: path.to_path_buf(),
                message: "lock must not be a symbolic link".to_owned(),
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(LockError::Io {
                action: "inspect lock",
                path: path.to_path_buf(),
                error,
            });
        }
    }
    let fd = rustix::fs::open(
        path,
        OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|error| LockError::Io {
        action: "open lock",
        path: path.to_path_buf(),
        error: error.into(),
    })?;
    let file = fs::File::from(fd);
    let metadata = file.metadata().map_err(|error| LockError::Io {
        action: "inspect lock",
        path: path.to_path_buf(),
        error,
    })?;
    if !is_private_file(&metadata, owner) {
        return Err(LockError::Unsafe {
            path: path.to_path_buf(),
            message: "lock must be a private, owned regular file".to_owned(),
        });
    }
    Ok(PrivateLock {
        file,
        identity: LockIdentity::of(&metadata),
        directory: LockIdentity::of(&directory_metadata),
    })
}

/// Rechecks the lock path and its directory against the captured identities.
pub fn verify(
    path: &Path,
    directory: &Path,
    owner: u32,
    identity: LockIdentity,
    directory_identity: LockIdentity,
) -> Result<(), LockError> {
    let current = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            LockError::Replaced { path: path.to_path_buf() }
        } else {
            LockError::Io {
                action: "recheck lock",
                path: path.to_path_buf(),
                error,
            }
        }
    })?;
    if current.file_type().is_symlink() {
        return Err(LockError::Unsafe {
            path: path.to_path_buf(),
            message: "lock must not be a symbolic link".to_owned(),
        });
    }
    if LockIdentity::of(&current) != identity {
        return Err(LockError::Replaced { path: path.to_path_buf() });
    }
    if !is_private_file(&current, owner) {
        return Err(LockError::Unsafe {
            path: path.to_path_buf(),
            message: "lock must be a private, owned regular file".to_owned(),
        });
    }
    let current_directory = fs::symlink_metadata(directory).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            LockError::Replaced { path: directory.to_path_buf() }
        } else {
            LockError::Io {
                action: "recheck runtime",
                path: directory.to_path_buf(),
                error,
            }
        }
    })?;
    if current_directory.file_type().is_symlink() {
        return Err(LockError::Unsafe {
            path: directory.to_path_buf(),
            message: "runtime directory must not be a symbolic link".to_owned(),
        });
    }
    if LockIdentity::of(&current_directory) != directory_identity {
        return Err(LockError::Replaced { path: directory.to_path_buf() });
    }
    if !current_directory.is_dir() || current_directory.uid() != owner {
        return Err(LockError::Unsafe {
            path: directory.to_path_buf(),
            message: "runtime directory must be an owned directory".to_owned(),
        });
    }
    Ok(())
}

fn ensure_directory(path: &Path, metadata: &fs::Metadata, owner: u32) -> Result<(), LockError> {
    if metadata.file_type().is_symlink() {
        return Err(LockError::Unsafe {
            path: path.to_path_buf(),
            message: "runtime directory must not be a symbolic link".to_owned(),
        });
    }
    if !metadata.is_dir() {
        return Err(LockError::Unsafe {
            path: path.to_path_buf(),
            message: "runtime path is not a directory".to_owned(),
        });
    }
    if metadata.uid() != owner {
        return Err(LockError::Unsafe {
            path: path.to_path_buf(),
            message: format!("runtime directory is owned by uid {}", metadata.uid()),
        });
    }
    Ok(())
}

fn is_private_file(metadata: &fs::Metadata, owner: u32) -> bool {
    metadata.is_file()
        && metadata.uid() == owner
        && metadata.mode() & 0o777 == 0o600
        && metadata.nlink() == 1
}
