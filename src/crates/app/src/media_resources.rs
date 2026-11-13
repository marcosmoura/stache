//! Resolution of the vendored `MediaRemote` adapter artifacts.
//!
//! The adapter ships inside the app as `media-control/` (see
//! `resources/media-control/PROVENANCE.md`). Packaged builds read it from
//! Tauri's resource directory; debug builds without a bundle fall back to the
//! in-repo copy. Nothing here ever reads `_legacy`, and nothing here spawns a
//! process: the result is only validated absolute paths or a typed reason.
//!
//! The adapter binaries are arm64-only, so other hosts report
//! [`MediaUnavailable::UnsupportedArchitecture`] before touching the disk.

use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The directory name of the adapter inside the resource root.
pub const MEDIA_DIRECTORY: &str = "media-control";
/// The perl script file name.
pub const SCRIPT_NAME: &str = "mediaremote-adapter.pl";
/// The framework directory name.
pub const FRAMEWORK_NAME: &str = "MediaRemoteAdapter.framework";
/// The framework's loadable binary, directly inside the framework directory.
pub const FRAMEWORK_BINARY: &str = "MediaRemoteAdapter";
/// The entitlement test client file name.
pub const TEST_CLIENT_NAME: &str = "MediaRemoteAdapterTestClient";

/// One artifact the adapter needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaArtifact {
    /// `mediaremote-adapter.pl`.
    Script,
    /// `MediaRemoteAdapter.framework`.
    Framework,
    /// The binary inside the framework that perl loads.
    FrameworkBinary,
    /// `MediaRemoteAdapterTestClient`.
    TestClient,
}

impl fmt::Display for MediaArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Script => SCRIPT_NAME,
            Self::Framework => FRAMEWORK_NAME,
            Self::FrameworkBinary => FRAMEWORK_BINARY,
            Self::TestClient => TEST_CLIENT_NAME,
        })
    }
}

/// Why the adapter cannot be used. Media stays hidden; Bar is unaffected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaUnavailable {
    /// The host is not arm64; the vendored binaries are arm64-only.
    UnsupportedArchitecture,
    /// No resource directory holds the adapter.
    NoResourceDirectory,
    /// The directory was not an absolute path.
    NotAbsolute(PathBuf),
    /// An artifact does not exist.
    Missing(MediaArtifact),
    /// An artifact exists but is not the expected file or directory.
    WrongType(MediaArtifact),
    /// The test client is not executable.
    NotExecutable(MediaArtifact),
}

impl fmt::Display for MediaUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedArchitecture => f.write_str("the media adapter supports arm64 only"),
            Self::NoResourceDirectory => f.write_str("no media adapter resource directory"),
            Self::NotAbsolute(path) => write!(f, "{} is not absolute", path.display()),
            Self::Missing(artifact) => write!(f, "{artifact} is missing"),
            Self::WrongType(artifact) => write!(f, "{artifact} has the wrong file type"),
            Self::NotExecutable(artifact) => write!(f, "{artifact} is not executable"),
        }
    }
}

impl std::error::Error for MediaUnavailable {}

/// Validated absolute paths of the adapter artifacts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaResources {
    /// `mediaremote-adapter.pl`.
    pub script: PathBuf,
    /// `MediaRemoteAdapter.framework`.
    pub framework: PathBuf,
    /// `MediaRemoteAdapterTestClient`.
    pub test_client: PathBuf,
}

/// The in-repo vendored directory, present only in debug builds.
#[must_use]
pub fn development_directory() -> Option<PathBuf> {
    cfg!(debug_assertions)
        .then(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("resources").join(MEDIA_DIRECTORY))
}

/// Picks the adapter directory: the packaged one when it exists, otherwise the
/// development one (`None` in release builds).
#[must_use]
pub fn select_directory(
    resource_dir: Option<&Path>,
    development: Option<&Path>,
) -> Option<PathBuf> {
    resource_dir
        .map(|root| root.join(MEDIA_DIRECTORY))
        .filter(|packaged| packaged.is_dir())
        .or_else(|| development.map(Path::to_path_buf))
}

impl MediaResources {
    /// Resolves the adapter for the running app.
    ///
    /// `resource_dir` is Tauri's `app.path().resource_dir()`.
    ///
    /// # Errors
    /// Returns the typed reason the adapter is unavailable.
    pub fn resolve(resource_dir: Option<&Path>) -> Result<Self, MediaUnavailable> {
        let directory = select_directory(resource_dir, development_directory().as_deref());
        Self::resolve_in(directory.as_deref(), cfg!(target_arch = "aarch64"))
    }

    /// Validates `directory`; `supported_architecture` is injected for tests.
    ///
    /// Symlinks are followed, so a framework laid out with `Versions/Current`
    /// links and a flattened bundle copy both validate.
    ///
    /// # Errors
    /// Returns the typed reason the adapter is unavailable.
    pub fn resolve_in(
        directory: Option<&Path>,
        supported_architecture: bool,
    ) -> Result<Self, MediaUnavailable> {
        if !supported_architecture {
            return Err(MediaUnavailable::UnsupportedArchitecture);
        }
        let directory = directory.ok_or(MediaUnavailable::NoResourceDirectory)?;
        if !directory.is_absolute() {
            return Err(MediaUnavailable::NotAbsolute(directory.to_path_buf()));
        }
        let script = directory.join(SCRIPT_NAME);
        let framework = directory.join(FRAMEWORK_NAME);
        let test_client = directory.join(TEST_CLIENT_NAME);
        check(&script, MediaArtifact::Script, Kind::File)?;
        check(&framework, MediaArtifact::Framework, Kind::Directory)?;
        check(
            &framework.join(FRAMEWORK_BINARY),
            MediaArtifact::FrameworkBinary,
            Kind::File,
        )?;
        check(&test_client, MediaArtifact::TestClient, Kind::Executable)?;
        Ok(Self { script, framework, test_client })
    }
}

/// The directory name of the media artwork cache.
///
/// It lives inside the build identity's cache directory
/// (`~/Library/Caches/<identity>/media-artwork`). The removed legacy app used
/// `media_artwork` in the same release cache directory; that entry is never read.
pub const ARTWORK_CACHE_DIRECTORY: &str = "media-artwork";

/// The Bar media provider's inputs: the resolver over the packaged/dev
/// resources and the artwork store under `cache_dir`.
///
/// Nothing runs here: the resolver is called on the media worker and the store
/// creates its directory on the first write.
#[must_use]
pub fn media_parts(
    resource_dir: Option<PathBuf>,
    cache_dir: Option<&Path>,
) -> stache_bar::MediaParts {
    let store = std::sync::Arc::new(stache_bar::ArtworkStore::new(
        cache_dir.map(|dir| dir.join(ARTWORK_CACHE_DIRECTORY)),
    ));
    let resolver: stache_bar::MediaResolver = std::sync::Arc::new(move || {
        MediaResources::resolve(resource_dir.as_deref())
            .map(|resources| stache_bar::MediaPaths {
                script: resources.script,
                framework: resources.framework,
                test_client: resources.test_client,
            })
            .map_err(|reason| reason.to_string())
    });
    stache_bar::MediaParts::system(resolver, store)
}

#[derive(Clone, Copy)]
enum Kind {
    File,
    Directory,
    Executable,
}

fn check(path: &Path, artifact: MediaArtifact, kind: Kind) -> Result<(), MediaUnavailable> {
    let metadata = path.metadata().map_err(|_| MediaUnavailable::Missing(artifact))?;
    let right_type = match kind {
        Kind::Directory => metadata.is_dir(),
        Kind::File | Kind::Executable => metadata.is_file(),
    };
    if !right_type {
        return Err(MediaUnavailable::WrongType(artifact));
    }
    if matches!(kind, Kind::Executable) && metadata.permissions().mode() & 0o111 == 0 {
        return Err(MediaUnavailable::NotExecutable(artifact));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    fn write(path: &Path, mode: u32) {
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(path, "x").expect("write");
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
    }

    fn complete(root: &Path) -> PathBuf {
        let directory = root.join(MEDIA_DIRECTORY);
        write(&directory.join(SCRIPT_NAME), 0o644);
        write(&directory.join(TEST_CLIENT_NAME), 0o755);
        write(&directory.join(FRAMEWORK_NAME).join(FRAMEWORK_BINARY), 0o644);
        directory
    }

    #[test]
    fn a_complete_directory_resolves_to_absolute_paths() {
        let root = tempfile::tempdir().expect("root");
        let directory = complete(root.path());
        let resources = MediaResources::resolve_in(Some(&directory), true).expect("resolves");
        assert_eq!(resources.script, directory.join(SCRIPT_NAME));
        assert_eq!(resources.framework, directory.join(FRAMEWORK_NAME));
        assert_eq!(resources.test_client, directory.join(TEST_CLIENT_NAME));
        assert!(resources.script.is_absolute() && resources.framework.is_absolute());
    }

    #[test]
    fn a_symlinked_framework_layout_validates() {
        let root = tempfile::tempdir().expect("root");
        let directory = root.path().join(MEDIA_DIRECTORY);
        write(&directory.join(SCRIPT_NAME), 0o644);
        write(&directory.join(TEST_CLIENT_NAME), 0o755);
        let framework = directory.join(FRAMEWORK_NAME);
        write(&framework.join("Versions/A").join(FRAMEWORK_BINARY), 0o644);
        symlink("A", framework.join("Versions/Current")).expect("current");
        symlink(
            "Versions/Current/MediaRemoteAdapter",
            framework.join(FRAMEWORK_BINARY),
        )
        .expect("link");
        assert!(MediaResources::resolve_in(Some(&directory), true).is_ok());
    }

    #[test]
    fn each_missing_artifact_is_named() {
        for (name, artifact) in [
            (SCRIPT_NAME, MediaArtifact::Script),
            (TEST_CLIENT_NAME, MediaArtifact::TestClient),
            (FRAMEWORK_NAME, MediaArtifact::Framework),
        ] {
            let root = tempfile::tempdir().expect("root");
            let directory = complete(root.path());
            let target = directory.join(name);
            if target.is_dir() {
                fs::remove_dir_all(&target).expect("remove");
            } else {
                fs::remove_file(&target).expect("remove");
            }
            assert_eq!(
                MediaResources::resolve_in(Some(&directory), true),
                Err(MediaUnavailable::Missing(artifact))
            );
        }
        let root = tempfile::tempdir().expect("root");
        let directory = complete(root.path());
        fs::remove_file(directory.join(FRAMEWORK_NAME).join(FRAMEWORK_BINARY)).expect("remove");
        assert_eq!(
            MediaResources::resolve_in(Some(&directory), true),
            Err(MediaUnavailable::Missing(MediaArtifact::FrameworkBinary))
        );
    }

    #[test]
    fn wrong_types_are_rejected() {
        let root = tempfile::tempdir().expect("root");
        let directory = complete(root.path());
        fs::remove_file(directory.join(SCRIPT_NAME)).expect("remove");
        fs::create_dir(directory.join(SCRIPT_NAME)).expect("dir as script");
        assert_eq!(
            MediaResources::resolve_in(Some(&directory), true),
            Err(MediaUnavailable::WrongType(MediaArtifact::Script))
        );

        let root = tempfile::tempdir().expect("root");
        let directory = complete(root.path());
        fs::remove_dir_all(directory.join(FRAMEWORK_NAME)).expect("remove");
        write(&directory.join(FRAMEWORK_NAME), 0o644);
        assert_eq!(
            MediaResources::resolve_in(Some(&directory), true),
            Err(MediaUnavailable::WrongType(MediaArtifact::Framework))
        );
    }

    #[test]
    fn a_test_client_without_execute_bits_is_rejected() {
        let root = tempfile::tempdir().expect("root");
        let directory = complete(root.path());
        fs::set_permissions(
            directory.join(TEST_CLIENT_NAME),
            fs::Permissions::from_mode(0o644),
        )
        .expect("chmod");
        assert_eq!(
            MediaResources::resolve_in(Some(&directory), true),
            Err(MediaUnavailable::NotExecutable(MediaArtifact::TestClient))
        );
    }

    #[test]
    fn relative_and_absent_directories_are_rejected() {
        assert_eq!(
            MediaResources::resolve_in(Some(Path::new("media-control")), true),
            Err(MediaUnavailable::NotAbsolute(PathBuf::from("media-control")))
        );
        assert_eq!(
            MediaResources::resolve_in(None, true),
            Err(MediaUnavailable::NoResourceDirectory)
        );
    }

    #[test]
    fn a_non_arm64_host_is_unsupported_without_touching_the_disk() {
        let root = tempfile::tempdir().expect("root");
        let directory = complete(root.path());
        assert_eq!(
            MediaResources::resolve_in(Some(&directory), false),
            Err(MediaUnavailable::UnsupportedArchitecture)
        );
        assert_eq!(
            MediaResources::resolve_in(None, false),
            Err(MediaUnavailable::UnsupportedArchitecture)
        );
    }

    #[test]
    fn packaged_wins_when_present_and_development_is_the_fallback() {
        let resource_root = tempfile::tempdir().expect("resource root");
        let development = tempfile::tempdir().expect("development");
        // No packaged directory: fall back to development.
        assert_eq!(
            select_directory(Some(resource_root.path()), Some(development.path())),
            Some(development.path().to_path_buf())
        );
        // Packaged directory present: it wins.
        let packaged = resource_root.path().join(MEDIA_DIRECTORY);
        fs::create_dir(&packaged).expect("packaged");
        assert_eq!(
            select_directory(Some(resource_root.path()), Some(development.path())),
            Some(packaged)
        );
        // Release builds have no development directory and never invent one.
        let empty_root = tempfile::tempdir().expect("empty");
        assert_eq!(select_directory(Some(empty_root.path()), None), None);
        assert_eq!(select_directory(None, None), None);
    }

    #[test]
    fn the_development_directory_is_the_in_repo_copy_never_legacy() {
        if let Some(directory) = development_directory() {
            assert!(directory.is_absolute());
            assert!(directory.ends_with("resources/media-control"));
            assert!(!directory.to_string_lossy().contains("_legacy"));
            let resources = MediaResources::resolve_in(Some(&directory), true)
                .expect("the vendored artifacts validate");
            assert!(resources.test_client.is_absolute());
        }
    }
}
