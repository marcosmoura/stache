use std::env;
use std::path::{Path, PathBuf};

/// Resolve the absolute path to an executable binary.
///
/// This helper first checks if the provided command name is already an absolute path.
/// If not, it searches for the executable in a priority-ordered list of directories:
/// 1. For "barba" binary in debug mode: the `./target/debug` directory relative to the app.
/// 2. Any directory specified via the `BARBA_EXTRA_PATHS` env var (colon-separated).
/// 3. The current process `PATH`.
/// 4. A curated list of fallback directories commonly used on macOS for user-installed tools.
///
/// # Arguments
///
/// * `binary` - The command name or path to locate.
///
/// # Returns
///
/// Returns `Ok(PathBuf)` when the binary is found and executable, otherwise `Err` with a descriptive reason.
pub fn resolve_binary(binary: &str) -> Result<PathBuf, String> {
    if binary.is_empty() {
        return Err("Binary name cannot be empty".to_string());
    }

    let candidate = Path::new(binary);
    if candidate.is_absolute() {
        return if is_executable(candidate) {
            Ok(candidate.to_path_buf())
        } else {
            Err(format!("Binary at {} is not executable", candidate.display()))
        };
    }

    let mut search_paths = Vec::new();

    // In debug builds, prioritize the debug binary for "barba" commands
    // This allows testing config hotkeys with the locally-built CLI
    #[cfg(debug_assertions)]
    if binary == "barba"
        && let Some(debug_path) = get_debug_binary_path()
    {
        search_paths.push(debug_path);
    }

    if let Ok(extra) = env::var("BARBA_EXTRA_PATHS") {
        search_paths.extend(extra.split(':').map(PathBuf::from));
    }

    if let Some(path_var) = env::var_os("PATH") {
        search_paths.extend(env::split_paths(&path_var));
    }

    // Common locations where macOS users often install CLI tools (Homebrew, Cargo, custom bin).
    search_paths.extend([
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
    ]);

    if let Some(home) = home_dir_from_env() {
        search_paths.push(home.join(".cargo/bin"));
        search_paths.push(home.join(".local/bin"));
    }

    for directory in search_paths {
        if directory.as_os_str().is_empty() {
            continue;
        }

        let candidate_path = directory.join(binary);
        if is_executable(&candidate_path) {
            return Ok(candidate_path);
        }
    }

    Err(format!(
        "Unable to locate executable '{binary}' in known search paths"
    ))
}

/// Gets the path to the debug binary directory.
///
/// In debug builds, this returns the `target/debug` directory relative to the
/// current executable or the `CARGO_MANIFEST_DIR` if available.
#[cfg(debug_assertions)]
fn get_debug_binary_path() -> Option<PathBuf> {
    // First, try using CARGO_MANIFEST_DIR (available when running via `cargo run`)
    if let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") {
        // CARGO_MANIFEST_DIR points to packages/desktop/tauri, we need to go up to workspace root
        let workspace_root = Path::new(&manifest_dir)
            .parent()? // desktop
            .parent()? // packages
            .parent()?; // workspace root
        let debug_dir = workspace_root.join("target/debug");
        if debug_dir.exists() {
            return Some(debug_dir);
        }
    }

    // Fallback: try to find target/debug relative to the current executable
    if let Ok(exe_path) = env::current_exe() {
        // The executable might be in target/debug/barba-app or similar
        // Walk up looking for a target/debug directory
        let mut current = exe_path.parent();
        while let Some(dir) = current {
            if dir.file_name().and_then(|n| n.to_str()) == Some("debug")
                && let Some(target) = dir.parent()
                && target.file_name().and_then(|n| n.to_str()) == Some("target")
            {
                return Some(dir.to_path_buf());
            }
            current = dir.parent();
        }
    }

    None
}

fn home_dir_from_env() -> Option<PathBuf> { env::var_os("HOME").map(PathBuf::from) }

fn is_executable(path: &Path) -> bool {
    use std::fs;

    if !path.exists() {
        return false;
    }

    match fs::metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return false;
            }

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            }

            #[cfg(not(unix))]
            {
                true
            }
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_binary;

    #[test]
    fn returns_err_for_empty_binary() {
        assert!(resolve_binary("").is_err());
    }

    #[test]
    fn respects_absolute_paths() {
        let bin = "/bin/ls";
        if cfg!(target_os = "macos") {
            let resolved = resolve_binary(bin).expect("ls should exist");
            assert_eq!(resolved, std::path::Path::new(bin));
        }
    }

    #[test]
    fn resolve_binary_finds_system_binary() {
        // ls should be available on all unix systems
        if cfg!(unix) {
            let result = resolve_binary("ls");
            assert!(result.is_ok());
            let path = result.unwrap();
            assert!(path.exists());
            assert!(path.ends_with("ls"));
        }
    }

    #[test]
    fn resolve_binary_fails_for_nonexistent() {
        let result = resolve_binary("nonexistent_binary_12345");
        assert!(result.is_err());
    }
}
