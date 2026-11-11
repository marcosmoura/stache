//! Non-fatal warnings for configuration someone else can write.
//!
//! Configuration is code: `exec_on_startup` and keybinding commands run as
//! children of the app. [`inspect_trust`] reports the configuration directory,
//! registered files and symlink-target directories that another account could
//! modify. It never refuses anything; the loader's accept and reject rules do
//! not depend on its result.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::{fmt, fs};

use crate::config::ConfigPaths;

/// The doc anchor printed with every warning.
pub const TRUST_MODEL_DOC: &str = "docs/architecture/config.md#trust-model";

/// The effective uid of the current process, the production `owner`.
#[must_use]
pub fn current_owner() -> u32 { rustix::process::geteuid().as_raw() }

/// Why one path is flagged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustReason {
    /// Owned by a uid that is neither the expected owner nor root.
    ForeignOwner {
        /// The path's owner.
        uid: u32,
    },
    /// The mode has `0o020` set.
    GroupWritable,
    /// The mode has `0o002` set.
    WorldWritable,
}

impl fmt::Display for TrustReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignOwner { uid } => write!(f, "owned by uid {uid}"),
            Self::GroupWritable => f.write_str("group-writable"),
            Self::WorldWritable => f.write_str("world-writable"),
        }
    }
}

/// One configuration path that someone other than the user can write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustWarning {
    /// The canonical path, so a symlinked layout names the real directory or
    /// file whose mode or owner needs fixing.
    pub path: PathBuf,
    /// Every reason that applies, in [`TrustReason`] declaration order.
    pub reasons: Vec<TrustReason>,
    /// The permission bits (`0o7777`).
    pub mode: u32,
    /// The path's owner uid.
    pub uid: u32,
}

impl fmt::Display for TrustWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reasons: Vec<String> = self.reasons.iter().map(ToString::to_string).collect();
        write!(
            f,
            "warning: {} is {} (mode {:04o}, owner uid {}); Stache runs commands from its configuration, see {TRUST_MODEL_DOC}",
            self.path.display(),
            reasons.join(" and "),
            self.mode,
            self.uid
        )
    }
}

/// Examines the configuration directory, each existing file in `files` and,
/// for a file that is itself a symlink, the directory holding its canonical
/// target. Metadata follows symlinks.
///
/// A path is flagged when its owner is neither `owner` nor root, or its mode
/// is group- or world-writable. Missing paths and metadata errors are
/// skipped. Warnings name canonical paths; each is reported once, in
/// examination order: the directory, then each file followed by its target
/// directory.
#[must_use]
pub fn inspect_trust(
    paths: &ConfigPaths,
    files: impl IntoIterator<Item = PathBuf>,
    owner: u32,
) -> Vec<TrustWarning> {
    let mut candidates = vec![paths.directory().to_path_buf()];
    for file in files {
        let is_link = fs::symlink_metadata(&file).is_ok_and(|meta| meta.file_type().is_symlink());
        let target_directory = if is_link {
            fs::canonicalize(&file)
                .ok()
                .and_then(|target| target.parent().map(Path::to_path_buf))
        } else {
            None
        };
        candidates.push(file);
        candidates.extend(target_directory);
    }
    let mut seen = HashSet::new();
    let mut warnings = Vec::new();
    for path in candidates {
        let Ok(metadata) = fs::metadata(&path) else { continue };
        let path = fs::canonicalize(&path).unwrap_or(path);
        if !seen.insert(path.clone()) {
            continue;
        }
        if let Some(warning) = check(path, &metadata, owner) {
            warnings.push(warning);
        }
    }
    warnings
}

/// [`inspect_trust`] over every catalog file, with the effective uid.
#[must_use]
pub fn inspect_catalog_trust(
    catalog: &crate::config::ConfigCatalog,
    paths: &ConfigPaths,
) -> Vec<TrustWarning> {
    let files = catalog.ids().map(|id| paths.file(crate::config::ConfigFileId::module(id)));
    inspect_trust(paths, files, current_owner())
}

fn check(path: PathBuf, metadata: &fs::Metadata, owner: u32) -> Option<TrustWarning> {
    let mode = metadata.mode() & 0o7777;
    let uid = metadata.uid();
    let mut reasons = Vec::new();
    if uid != owner && uid != 0 {
        reasons.push(TrustReason::ForeignOwner { uid });
    }
    if mode & 0o020 != 0 {
        reasons.push(TrustReason::GroupWritable);
    }
    if mode & 0o002 != 0 {
        reasons.push(TrustReason::WorldWritable);
    }
    (!reasons.is_empty()).then_some(TrustWarning { path, reasons, mode, uid })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;

    fn chmod(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
    }

    fn me() -> u32 { current_owner() }

    fn real(path: &Path) -> PathBuf { fs::canonicalize(path).expect("canonical") }

    /// A `0755` config directory holding one `0644` file `a.toml`.
    fn layout() -> (tempfile::TempDir, ConfigPaths, PathBuf) {
        let root = tempfile::tempdir().expect("root");
        let directory = root.path().join("config");
        fs::create_dir(&directory).expect("dir");
        chmod(&directory, 0o755);
        let file = directory.join("a.toml");
        fs::write(&file, "").expect("file");
        chmod(&file, 0o644);
        (root, ConfigPaths::new(directory), file)
    }

    #[test]
    fn clean_private_and_ordinary_modes_produce_nothing() {
        for (dir_mode, file_mode) in [(0o700, 0o600), (0o755, 0o644)] {
            let (_root, paths, file) = layout();
            chmod(paths.directory(), dir_mode);
            chmod(&file, file_mode);
            assert!(inspect_trust(&paths, [file], me()).is_empty());
        }
    }

    #[test]
    fn writable_directories_are_flagged() {
        let (_root, paths, file) = layout();
        chmod(paths.directory(), 0o775);
        let warnings = inspect_trust(&paths, [file.clone()], me());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].path, real(paths.directory()));
        assert_eq!(warnings[0].reasons, vec![TrustReason::GroupWritable]);
        assert_eq!(warnings[0].mode, 0o775);
        assert_eq!(
            warnings[0].to_string(),
            format!(
                "warning: {} is group-writable (mode 0775, owner uid {}); Stache runs commands from its configuration, see docs/architecture/config.md#trust-model",
                real(paths.directory()).display(),
                me()
            )
        );
        chmod(paths.directory(), 0o777);
        let warnings = inspect_trust(&paths, [file], me());
        assert_eq!(warnings[0].reasons, vec![
            TrustReason::GroupWritable,
            TrustReason::WorldWritable
        ]);
        assert!(warnings[0].to_string().contains("is group-writable and world-writable"));
    }

    #[test]
    fn writable_files_are_flagged() {
        let (_root, paths, file) = layout();
        chmod(&file, 0o664);
        let warnings = inspect_trust(&paths, [file.clone()], me());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].path, real(&file));
        assert_eq!(warnings[0].reasons, vec![TrustReason::GroupWritable]);
        chmod(&file, 0o666);
        let warnings = inspect_trust(&paths, [file], me());
        assert_eq!(warnings[0].reasons, vec![
            TrustReason::GroupWritable,
            TrustReason::WorldWritable
        ]);
    }

    #[test]
    fn a_foreign_owner_is_flagged_and_root_is_accepted() {
        let (_root, paths, file) = layout();
        let foreign = me().wrapping_add(1).max(1);
        let warnings = inspect_trust(&paths, [file.clone()], foreign);
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].reasons, vec![TrustReason::ForeignOwner {
            uid: me()
        }]);
        assert_eq!(warnings[1].path, real(&file));

        // `/` and `/usr` are root-owned and not group- or world-writable.
        let root_owned = ConfigPaths::new("/usr");
        assert_eq!(fs::metadata("/usr").expect("usr").uid(), 0);
        assert!(inspect_trust(&root_owned, [PathBuf::from("/")], foreign).is_empty());
    }

    #[test]
    fn the_directory_of_a_symlinked_files_target_is_examined() {
        let (root, paths, _file) = layout();
        let dotfiles = root.path().join("dotfiles");
        fs::create_dir(&dotfiles).expect("dotfiles");
        chmod(&dotfiles, 0o755);
        let target = dotfiles.join("b.toml");
        fs::write(&target, "").expect("target");
        chmod(&target, 0o644);
        let link = paths.directory().join("b.toml");
        symlink(&target, &link).expect("link");
        assert!(inspect_trust(&paths, [link.clone()], me()).is_empty());

        chmod(&dotfiles, 0o777);
        let warnings = inspect_trust(&paths, [link], me());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].path, real(&dotfiles));
    }

    #[test]
    fn a_symlinked_configuration_directory_is_followed() {
        let root = tempfile::tempdir().expect("root");
        let real_dir = root.path().join("dotfiles");
        fs::create_dir(&real_dir).expect("real");
        chmod(&real_dir, 0o755);
        let file = real_dir.join("a.toml");
        fs::write(&file, "").expect("file");
        chmod(&file, 0o644);
        let link = root.path().join("stache");
        symlink(&real_dir, &link).expect("link");
        let paths = ConfigPaths::new(&link);
        assert!(inspect_trust(&paths, [link.join("a.toml")], me()).is_empty());

        chmod(&real_dir, 0o775);
        let warnings = inspect_trust(&paths, [link.join("a.toml")], me());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].path, real(&real_dir), "names the real directory");
    }

    #[test]
    fn missing_files_and_dangling_symlinks_are_skipped() {
        let (_root, paths, _file) = layout();
        let dangling = paths.directory().join("dangling.toml");
        symlink(paths.directory().join("nowhere"), &dangling).expect("link");
        let missing = paths.directory().join("missing.toml");
        assert!(inspect_trust(&paths, [dangling, missing], me()).is_empty());
    }

    #[test]
    fn each_path_is_reported_once_in_a_stable_order() {
        let (_root, paths, file) = layout();
        chmod(paths.directory(), 0o777);
        chmod(&file, 0o666);
        let second = paths.directory().join("b.toml");
        fs::write(&second, "").expect("second");
        chmod(&second, 0o666);
        // The directory reached again through a symlinked file's target.
        let link = paths.directory().join("c.toml");
        symlink(&second, &link).expect("link");
        let files = [file.clone(), second.clone(), file.clone(), link];
        let first = inspect_trust(&paths, files.clone(), me());
        let paths_seen: Vec<PathBuf> = first.iter().map(|warning| warning.path.clone()).collect();
        assert_eq!(paths_seen, vec![
            real(paths.directory()),
            real(&file),
            real(&second)
        ]);
        assert_eq!(inspect_trust(&paths, files, me()), first);
    }
}
