//! Canonical configuration-directory identity.
//!
//! Existing ancestors are resolved with `fs::canonicalize`, which follows
//! symlink aliases and removes `.`/`..`. A suffix that does not exist yet is
//! appended without following it, after normalizing its own `.` and `..`
//! components. Both host and client therefore compare the same identity for an
//! existing directory, a symlink alias and a not-yet-created child path.

use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Canonicalizes an absolute path, resolving the longest existing prefix.
///
/// # Errors
/// Returns an actionable message for a relative path, an unreadable ancestor,
/// a non-directory existing target or a suffix that escapes the root.
pub(crate) fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "configuration directory {} is not absolute",
            path.display()
        ));
    }

    let mut current = path.to_path_buf();
    let mut suffix: Vec<OsString> = Vec::new();
    loop {
        match fs::canonicalize(&current) {
            Ok(base) => {
                let resolved = append_suffix(base, &suffix)?;
                if fs::metadata(&resolved).is_ok_and(|metadata| !metadata.is_dir()) {
                    return Err(format!(
                        "configuration directory {} is not a directory",
                        resolved.display()
                    ));
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot resolve configuration directory {}: {error}",
                    current.display()
                ));
            }
        }

        match current.components().next_back() {
            Some(Component::Normal(name)) => {
                suffix.push(name.to_os_string());
                current.pop();
            }
            Some(Component::ParentDir) => {
                suffix.push(OsString::from(".."));
                current.pop();
            }
            Some(Component::CurDir) => {
                current.pop();
            }
            Some(Component::RootDir | Component::Prefix(_)) | None => {
                return Err(format!(
                    "cannot resolve configuration directory {}",
                    path.display()
                ));
            }
        }
    }
}

fn append_suffix(mut base: PathBuf, suffix: &[OsString]) -> Result<PathBuf, String> {
    for component in suffix.iter().rev() {
        if component.to_str() == Some("..") {
            if !base.pop() {
                return Err("configuration directory escapes the filesystem root".to_owned());
            }
        } else if component.to_str() != Some(".") {
            base.push(component);
        }
    }
    Ok(base)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn a_symlink_alias_resolves_to_the_same_directory() {
        let root = tempfile::tempdir().expect("root");
        let real = root.path().join("real");
        fs::create_dir_all(&real).expect("real");
        let alias = root.path().join("alias");
        symlink(&real, &alias).expect("alias");
        assert_eq!(
            canonical_directory(&alias).expect("alias"),
            canonical_directory(&real).expect("real")
        );
    }

    #[test]
    fn an_absent_suffix_is_appended_after_the_existing_ancestor() {
        let root = tempfile::tempdir().expect("root");
        let real = root.path().join("real");
        fs::create_dir_all(&real).expect("real");
        let alias = root.path().join("alias");
        symlink(&real, &alias).expect("alias");
        assert_eq!(
            canonical_directory(&alias.join("future").join("deeper")).expect("alias suffix"),
            fs::canonicalize(&real).expect("real").join("future").join("deeper")
        );
    }

    #[test]
    fn dot_and_dot_dot_are_normalized_in_an_absent_suffix() {
        let root = tempfile::tempdir().expect("root");
        let real = root.path().join("real");
        fs::create_dir_all(&real).expect("real");
        let path = real.join("future").join("..").join(".").join("other");
        assert_eq!(
            canonical_directory(&path).expect("normalized"),
            fs::canonicalize(&real).expect("real").join("other")
        );
    }

    #[test]
    fn a_relative_path_is_refused() {
        let error = canonical_directory(Path::new("relative")).expect_err("relative");
        assert!(error.contains("not absolute"));
    }

    #[test]
    fn an_existing_regular_file_is_not_a_directory_identity() {
        let root = tempfile::tempdir().expect("root");
        let file = root.path().join("file");
        fs::write(&file, b"x").expect("file");
        let error = canonical_directory(&file).expect_err("file");
        assert!(error.contains("not a directory"));
    }
}
