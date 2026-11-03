//! The `cache` command family.
//!
//! Cache clearing touches only the identity cache directory. It refuses a
//! symlinked or foreign-owned directory and never follows a link, so the
//! runtime socket, configuration and the legacy application are unreachable
//! from this command.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::process::ExitCode;

use clap::Subcommand;
use stache_sdk::AppPaths;

use crate::{fail, output};

/// `cache` subcommands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum CacheCommand {
    /// Remove every cached file.
    Clear,
    /// Print the cache directory.
    Path,
}

pub(crate) fn run<W: Write, E: Write>(
    command: CacheCommand,
    paths: &AppPaths,
    out: &mut W,
    err: &mut E,
) -> ExitCode {
    match command {
        CacheCommand::Path => {
            let _ = writeln!(out, "{}", paths.cache_directory().display());
            ExitCode::SUCCESS
        }
        CacheCommand::Clear => clear(paths, out, err),
    }
}

fn clear<W: Write, E: Write>(paths: &AppPaths, out: &mut W, err: &mut E) -> ExitCode {
    let cache = paths.cache_directory();
    let metadata = match fs::symlink_metadata(cache) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let _ = writeln!(out, "Cache directory does not exist. Nothing to clear.");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            return fail(err, &format!("Failed to clear cache: {error}"));
        }
    };
    if metadata.file_type().is_symlink() {
        return fail(
            err,
            &format!(
                "refusing to clear symlinked cache directory: {}",
                cache.display()
            ),
        );
    }
    if let Err(reason) = ensure_clearable(cache, &metadata, paths.owner()) {
        return fail(err, &reason);
    }
    let freed = match directory_size(cache) {
        Ok(bytes) => bytes,
        Err(error) => return fail(err, &format!("Failed to clear cache: {error}")),
    };
    if let Err(error) = fs::remove_dir_all(cache) {
        return fail(err, &format!("Failed to clear cache: {error}"));
    }
    let _ = writeln!(
        out,
        "Cache cleared successfully. Freed {}.",
        output::format_bytes(freed)
    );
    ExitCode::SUCCESS
}

fn ensure_clearable(path: &Path, metadata: &fs::Metadata, owner: u32) -> Result<(), String> {
    if !metadata.is_dir() || metadata.file_type().is_socket() {
        return Err(format!(
            "refusing to clear non-directory cache path: {}",
            path.display()
        ));
    }
    if metadata.uid() != owner {
        return Err(format!(
            "refusing to clear cache directory owned by uid {}",
            metadata.uid()
        ));
    }
    Ok(())
}

fn directory_size(path: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            total += directory_size(&entry.path())?;
        } else if file_type.is_file() {
            total += entry.metadata()?.len();
        }
        // Symlinks and sockets are removed as links and contribute no bytes.
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::CliEnvironment;

    fn environment(root: &Path) -> CliEnvironment { CliEnvironment::for_test(root, root) }

    fn paths(root: &Path) -> AppPaths { environment(root).app_paths().expect("paths") }

    fn execute(root: &Path, command: CacheCommand) -> (ExitCode, String, String) {
        let paths = paths(root);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(command, &paths, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    #[test]
    fn path_prints_the_bare_cache_directory() {
        let root = tempfile::tempdir().expect("temp dir");
        let (code, out, err) = execute(root.path(), CacheCommand::Path);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(
            out,
            format!(
                "{}\n",
                root.path()
                    .canonicalize()
                    .expect("canonical home")
                    .join("Library")
                    .join("Caches")
                    .join("com.marcosmoura.stache.next")
                    .display()
            )
        );
    }

    #[test]
    fn a_missing_cache_reports_the_legacy_message() {
        let root = tempfile::tempdir().expect("temp dir");
        let (code, out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(out, "Cache directory does not exist. Nothing to clear.\n");
    }

    #[test]
    fn clearing_reports_and_removes_only_the_cache_directory() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(cache.join("nested")).expect("cache");
        fs::write(cache.join("one.bin"), [0_u8; 3]).expect("one");
        fs::write(cache.join("nested").join("two.bin"), [0_u8; 5]).expect("two");
        let configuration = root.path().join(".config").join("stache");
        fs::create_dir_all(&configuration).expect("config");
        fs::write(configuration.join("global.jsonc"), "{}").expect("config file");
        let legacy = root.path().join("Library").join("Caches").join("com.marcosmoura.stache");
        fs::create_dir_all(&legacy).expect("legacy cache");
        fs::write(legacy.join("keep.bin"), "keep").expect("legacy file");

        let (code, out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(out, "Cache cleared successfully. Freed 8 bytes.\n");
        assert!(!cache.exists());
        assert!(configuration.join("global.jsonc").exists());
        assert!(legacy.join("keep.bin").exists());
    }

    #[test]
    fn a_symlinked_cache_directory_is_never_followed() {
        let root = tempfile::tempdir().expect("temp dir");
        let target = root.path().join("target");
        fs::create_dir(&target).expect("target");
        fs::write(target.join("keep"), "keep").expect("keep");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(cache.parent().expect("parent")).expect("cache parent");
        std::os::unix::fs::symlink(&target, &cache).expect("symlink");
        let (code, out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(out.is_empty());
        assert!(
            err.contains("refusing to clear symlinked cache directory"),
            "{err}"
        );
        assert_eq!(fs::read_to_string(target.join("keep")).expect("keep"), "keep");
    }

    #[test]
    fn a_symlink_inside_the_cache_is_removed_without_touching_its_target() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(&cache).expect("cache");
        let runtime = paths(root.path()).runtime_directory().to_path_buf();
        fs::create_dir_all(&runtime).expect("runtime");
        fs::write(runtime.join("control.sock"), "not really").expect("runtime file");
        std::os::unix::fs::symlink(&runtime, cache.join("runtime-link")).expect("symlink");
        let (code, _out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(!cache.exists());
        assert!(runtime.join("control.sock").exists());
    }

    #[test]
    fn a_non_directory_cache_path_is_refused() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(cache.parent().expect("parent")).expect("parent");
        fs::write(&cache, "not a cache").expect("file");
        let (code, _out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(
            err.contains("refusing to clear non-directory cache path"),
            "{err}"
        );
        assert_eq!(fs::read_to_string(&cache).expect("preserved"), "not a cache");
    }

    #[test]
    fn a_cache_directory_owned_by_another_user_is_refused() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(&cache).expect("cache");
        let metadata = fs::symlink_metadata(&cache).expect("metadata");
        let foreign = if metadata.uid() == u32::MAX {
            0
        } else {
            metadata.uid() + 1
        };
        let error = ensure_clearable(&cache, &metadata, foreign).expect_err("foreign owner");
        assert!(error.contains("owned by uid"), "{error}");
        assert!(cache.exists());
    }

    #[test]
    fn cached_files_keep_owner_only_permissions_after_cleanup() {
        let root = tempfile::tempdir().expect("temp dir");
        let cache = paths(root.path()).cache_directory().to_path_buf();
        fs::create_dir_all(&cache).expect("cache");
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).expect("mode");
        let (code, _out, err) = execute(root.path(), CacheCommand::Clear);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(!cache.exists());
    }
}
