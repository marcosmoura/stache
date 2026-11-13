//! Identity-isolated cache and runtime directories.
//!
//! A build's state lives under its own bundle identity: cache files live under
//! `~/Library/Caches/<identity>` and the control socket lives in a per-user
//! macOS temporary directory of the same name. A release build uses
//! [`RELEASE_IDENTITY`], a debug build [`DEVELOPMENT_IDENTITY`], so the two
//! never share a lock, socket, cache or durable state. Nothing here
//! reads the process environment implicitly; callers pass an
//! [`AppEnvironment`], which keeps tests away from the real home and cache.

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::{fmt, fs};

/// The release bundle identity.
pub const RELEASE_IDENTITY: &str = "com.marcosmoura.stache";

/// The development bundle identity.
pub const DEVELOPMENT_IDENTITY: &str = "com.marcosmoura.stache.next";

/// Bundle identity used for cache and runtime directories.
///
/// The development identity in debug builds, the release identity otherwise.
/// Selected only at compile time; no environment variable, config key or flag
/// reaches it.
pub const IDENTITY: &str = if cfg!(debug_assertions) {
    DEVELOPMENT_IDENTITY
} else {
    RELEASE_IDENTITY
};

/// The other of [`RELEASE_IDENTITY`] and [`DEVELOPMENT_IDENTITY`].
pub const SIBLING_IDENTITY: &str = if same_str(IDENTITY, RELEASE_IDENTITY) {
    DEVELOPMENT_IDENTITY
} else {
    RELEASE_IDENTITY
};

const fn same_str(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// The product name shown where no Tauri package information exists, such as
/// the command line. The desktop reads its name from the package information.
pub const PRODUCT_NAME: &str = "Stache";

/// Socket file name inside the runtime directory.
pub const SOCKET_FILE_NAME: &str = "ipc";

/// Cache lease file name inside the runtime directory.
pub const CACHE_LEASE_FILE_NAME: &str = "cache.lock";

/// Maximum socket path length in bytes on macOS.
///
/// `sun_path` holds 104 bytes including the terminating NUL, so a path of 103
/// bytes is the longest one `bind` accepts.
pub const MAX_SOCKET_PATH: usize = 103;

/// The environment variables used to choose the cache and runtime roots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppEnvironment {
    /// `HOME`, when set and non-empty.
    pub home: Option<PathBuf>,
    /// `TMPDIR`, the per-user temporary directory, when set and non-empty.
    pub temporary: Option<PathBuf>,
}

impl AppEnvironment {
    /// Reads the process environment once.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            home: non_empty_var("HOME"),
            temporary: non_empty_var("TMPDIR"),
        }
    }
}

fn non_empty_var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// Resolved cache and runtime directories for one identity.
///
/// A server and a client resolving the same [`AppEnvironment`] always agree on
/// the socket path. Both directories are checked for overlap so cache clearing
/// can never reach the runtime endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    cache: PathBuf,
    runtime: PathBuf,
    support: PathBuf,
    owner: u32,
}

impl AppPaths {
    /// Resolves the identity paths without creating any directory.
    ///
    /// # Errors
    /// Returns [`AppPathError`] when the environment cannot name a safe,
    /// non-overlapping pair of absolute directories, or the resolved temporary
    /// root is not a directory owned by the effective user.
    pub fn resolve(environment: &AppEnvironment) -> Result<Self, AppPathError> {
        Self::resolve_for(environment, IDENTITY)
    }

    /// Resolves the paths for an explicit `identity`. A test seam: every
    /// production caller uses [`AppPaths::resolve`], which passes [`IDENTITY`].
    ///
    /// # Errors
    /// The same as [`AppPaths::resolve`].
    pub fn resolve_for(environment: &AppEnvironment, identity: &str) -> Result<Self, AppPathError> {
        let home = environment.home.as_ref().ok_or(AppPathError::NoHome)?;
        let temporary = environment.temporary.as_ref().ok_or(AppPathError::NoTemporaryDirectory)?;
        let home = resolve_anchor(home)?;
        let temporary = resolve_anchor(temporary)?;
        let owner = rustix::process::geteuid().as_raw();
        let metadata = fs::metadata(&temporary)
            .map_err(|_| AppPathError::OwnerUnavailable { path: temporary.clone() })?;
        if !metadata.is_dir() || metadata.uid() != owner {
            return Err(AppPathError::UntrustedRoot { path: temporary });
        }
        let cache = resolve_anchor(&home.join("Library").join("Caches"))?.join(identity);
        let runtime = temporary.join(identity);
        if runtime.starts_with(&cache) || cache.starts_with(&runtime) {
            return Err(AppPathError::OverlappingDirectories { cache, runtime });
        }
        let support =
            resolve_anchor(&home.join("Library").join("Application Support"))?.join(identity);
        if support.starts_with(&cache)
            || cache.starts_with(&support)
            || support.starts_with(&runtime)
            || runtime.starts_with(&support)
        {
            return Err(AppPathError::OverlappingDirectories { cache: support, runtime });
        }
        Ok(Self { cache, runtime, support, owner })
    }

    /// The cache directory.
    #[must_use]
    pub fn cache_directory(&self) -> &Path { &self.cache }

    /// The runtime directory that owns the control socket.
    #[must_use]
    pub fn runtime_directory(&self) -> &Path { &self.runtime }

    /// The durable Application Support directory for this identity.
    ///
    /// Unlike the cache it is never cleared, unlike the runtime directory it
    /// survives a reboot, and it is not a user-selected configuration directory.
    /// Durable feature state such as the Keybindings Caps journal lives in a
    /// named subdirectory of it. Resolving this path creates nothing.
    #[must_use]
    pub fn application_support_directory(&self) -> &Path { &self.support }

    /// The expected owner: the current process's effective user ID.
    #[must_use]
    pub const fn owner(&self) -> u32 { self.owner }

    /// The control socket path.
    ///
    /// # Errors
    /// Returns [`AppPathError::SocketPathTooLong`] when the path does not fit
    /// in a Unix socket address.
    pub fn socket_path(&self) -> Result<PathBuf, AppPathError> {
        let path = self.runtime.join(SOCKET_FILE_NAME);
        socket_path_within_limit(path)
    }

    /// The cross-process cache lease path.
    ///
    /// It lives in the runtime directory, outside the deletable cache root, so
    /// clearing and recreating the cache keeps the same lock identity.
    #[must_use]
    pub fn cache_lease_path(&self) -> PathBuf { self.runtime.join(CACHE_LEASE_FILE_NAME) }
}

fn resolve_anchor(path: &Path) -> Result<PathBuf, AppPathError> {
    if !path.is_absolute() {
        return Err(AppPathError::UntrustedRoot { path: path.to_path_buf() });
    }
    match fs::canonicalize(path) {
        Ok(resolved) => return Ok(resolved),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(AppPathError::UntrustedRoot { path: path.to_path_buf() }),
    }
    let parent = path
        .parent()
        .ok_or_else(|| AppPathError::UntrustedRoot { path: path.to_path_buf() })?;
    let mut resolved = resolve_anchor(parent)?;
    match path.components().next_back() {
        Some(std::path::Component::ParentDir) => {
            resolved.pop();
        }
        Some(std::path::Component::Normal(name)) => resolved.push(name),
        _ => {}
    }
    Ok(resolved)
}

fn socket_path_within_limit(path: PathBuf) -> Result<PathBuf, AppPathError> {
    if path.as_os_str().as_bytes().len() > MAX_SOCKET_PATH {
        Err(AppPathError::SocketPathTooLong { path, limit: MAX_SOCKET_PATH })
    } else {
        Ok(path)
    }
}

/// A cache or runtime directory that cannot be resolved safely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppPathError {
    /// An anchor is relative, foreign-owned, or not a usable directory.
    UntrustedRoot {
        /// The rejected anchor.
        path: PathBuf,
    },
    /// `HOME` is unavailable, so the cache directory has no anchor.
    NoHome,
    /// `TMPDIR` is unavailable, so the per-user runtime directory has no anchor.
    NoTemporaryDirectory,
    /// The cache and runtime directories overlap.
    OverlappingDirectories {
        /// The cache directory.
        cache: PathBuf,
        /// The runtime directory.
        runtime: PathBuf,
    },
    /// A directory or the socket path is longer than a Unix socket allows.
    SocketPathTooLong {
        /// The rejected path.
        path: PathBuf,
        /// The maximum accepted byte length.
        limit: usize,
    },
    /// The owning user could not be read from the anchor directories.
    OwnerUnavailable {
        /// The anchor that could not be inspected.
        path: PathBuf,
    },
}

impl fmt::Display for AppPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UntrustedRoot { path } => {
                write!(f, "untrusted directory anchor: {}", path.display())
            }
            Self::NoHome => f.write_str("no cache directory available: set HOME"),
            Self::NoTemporaryDirectory => f.write_str("no runtime directory available: set TMPDIR"),
            Self::OverlappingDirectories { cache, runtime } => write!(
                f,
                "cache directory {} overlaps the runtime directory {}",
                cache.display(),
                runtime.display()
            ),
            Self::SocketPathTooLong { path, limit } => {
                write!(f, "socket path {} is longer than {limit} bytes", path.display())
            }
            Self::OwnerUnavailable { path } => {
                write!(f, "cannot determine the owner of {}", path.display())
            }
        }
    }
}

impl std::error::Error for AppPathError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(home: &Path, temporary: &Path) -> AppEnvironment {
        AppEnvironment {
            home: Some(home.to_path_buf()),
            temporary: Some(temporary.to_path_buf()),
        }
    }

    #[test]
    fn a_debug_build_selects_the_development_identity() {
        assert_eq!(IDENTITY, DEVELOPMENT_IDENTITY);
        assert_eq!(SIBLING_IDENTITY, RELEASE_IDENTITY);
        assert_eq!(RELEASE_IDENTITY, "com.marcosmoura.stache");
        assert_eq!(DEVELOPMENT_IDENTITY, "com.marcosmoura.stache.next");
    }

    #[test]
    fn identity_paths_are_exact_for_both_identities_and_never_shared() {
        let root = tempfile::tempdir().expect("temp dir");
        let environment = environment(root.path(), root.path());
        let canonical = fs::canonicalize(root.path()).expect("canonical root");
        let mut seen = Vec::new();
        for identity in [RELEASE_IDENTITY, DEVELOPMENT_IDENTITY] {
            let paths = AppPaths::resolve_for(&environment, identity).expect("resolve");
            assert_eq!(
                paths.cache_directory(),
                canonical.join("Library").join("Caches").join(identity)
            );
            assert_eq!(paths.runtime_directory(), canonical.join(identity));
            assert_eq!(
                paths.socket_path().expect("socket"),
                canonical.join(identity).join("ipc")
            );
            assert_eq!(
                paths.application_support_directory(),
                canonical.join("Library").join("Application Support").join(identity)
            );
            seen.push(paths);
        }
        let (release, development) = (&seen[0], &seen[1]);
        for (a, b) in [
            (release.cache_directory(), development.cache_directory()),
            (release.runtime_directory(), development.runtime_directory()),
            (
                release.application_support_directory(),
                development.application_support_directory(),
            ),
        ] {
            assert!(
                !a.starts_with(b) && !b.starts_with(a),
                "{} {}",
                a.display(),
                b.display()
            );
        }
        assert_eq!(AppPaths::resolve(&environment).expect("resolve"), *development);
    }

    #[test]
    fn the_socket_fits_the_limit_under_both_identities() {
        // A real macOS `TMPDIR` is `/var/folders/xx/<28 characters>/T/`.
        let temporary = Path::new("/private/var/folders/zz/zyxvpxvq6csfxvn_n0000000000000/T");
        for identity in [RELEASE_IDENTITY, DEVELOPMENT_IDENTITY] {
            let socket = temporary.join(identity).join(SOCKET_FILE_NAME);
            assert!(
                socket_path_within_limit(socket.clone()).is_ok(),
                "{}",
                socket.display()
            );
        }
    }

    #[test]
    fn the_durable_directory_is_isolated_from_cache_runtime_and_legacy() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::resolve(&environment(root.path(), root.path())).expect("resolve");
        let root = fs::canonicalize(root.path()).expect("canonical root");
        assert_eq!(
            paths.application_support_directory(),
            root.join("Library").join("Application Support").join(IDENTITY)
        );
        assert!(!paths.application_support_directory().starts_with(paths.cache_directory()));
        assert!(!paths.application_support_directory().starts_with(paths.runtime_directory()));
        assert!(!paths.runtime_directory().starts_with(paths.application_support_directory()));
    }

    #[test]
    fn the_cache_lease_lives_outside_the_cache_root() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::resolve(&environment(root.path(), root.path())).expect("resolve");
        assert_eq!(
            paths.cache_lease_path(),
            paths.runtime_directory().join(CACHE_LEASE_FILE_NAME)
        );
        assert!(!paths.cache_lease_path().starts_with(paths.cache_directory()));
    }

    #[test]
    fn the_cache_directory_is_never_beneath_the_runtime_directory() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::resolve(&environment(root.path(), root.path())).expect("resolve");
        assert!(!paths.cache_directory().starts_with(paths.runtime_directory()));
        assert!(!paths.runtime_directory().starts_with(paths.cache_directory()));
    }

    #[test]
    fn a_runtime_root_inside_the_cache_is_rejected() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = root.path().join("Library").join("Caches").join("com.marcosmoura.stache.next");
        fs::create_dir_all(&cache).expect("cache");
        let error = AppPaths::resolve(&environment(root.path(), &cache)).expect_err("overlap");
        assert!(matches!(error, AppPathError::OverlappingDirectories { .. }));
    }

    #[test]
    fn missing_anchors_fail_instead_of_falling_back_to_shared_directories() {
        let root = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            AppPaths::resolve(&AppEnvironment {
                home: None,
                temporary: Some(root.path().into())
            }),
            Err(AppPathError::NoHome)
        );
        assert_eq!(
            AppPaths::resolve(&AppEnvironment {
                home: Some(root.path().into()),
                temporary: None
            }),
            Err(AppPathError::NoTemporaryDirectory)
        );
    }

    #[test]
    fn an_overlong_socket_path_is_rejected_before_binding() {
        let root = tempfile::tempdir().expect("temp dir");
        let long = root.path().join("x".repeat(MAX_SOCKET_PATH));
        fs::create_dir(&long).expect("long root");
        let paths = AppPaths::resolve(&environment(root.path(), &long)).expect("paths");
        let error = paths.socket_path().expect_err("overlong socket");
        assert!(matches!(error, AppPathError::SocketPathTooLong { .. }));
    }

    #[test]
    fn the_owner_is_the_effective_uid_not_the_home_owner() {
        let root = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::resolve(&environment(Path::new("/"), root.path())).expect("resolve");
        assert_eq!(paths.owner(), rustix::process::geteuid().as_raw());
    }

    #[test]
    fn aliases_cannot_hide_a_runtime_beneath_cache() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = root.path().join("Library/Caches").join(IDENTITY);
        fs::create_dir_all(&cache).expect("cache");
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&cache, &alias).expect("alias");
        for temporary in [&alias, &cache.join("../").join(IDENTITY)] {
            assert!(matches!(
                AppPaths::resolve(&environment(root.path(), temporary)),
                Err(AppPathError::OverlappingDirectories { .. })
            ));
        }
    }

    #[test]
    fn relative_anchors_and_foreign_runtime_roots_are_refused() {
        let root = tempfile::tempdir().expect("temp dir");
        assert!(AppPaths::resolve(&environment(Path::new("relative"), root.path())).is_err());
        assert!(AppPaths::resolve(&environment(root.path(), Path::new("relative"))).is_err());
        if rustix::process::geteuid().as_raw() != 0 {
            assert!(matches!(
                AppPaths::resolve(&environment(root.path(), Path::new("/"))),
                Err(AppPathError::UntrustedRoot { .. })
            ));
        }
    }
}
