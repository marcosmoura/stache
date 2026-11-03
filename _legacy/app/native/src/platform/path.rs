//! Path utilities for shell-like path expansion.
//!
//! This module provides centralized path expansion functionality,
//! supporting tilde (`~`) expansion and relative path resolution.

use std::path::{Path, PathBuf};

/// Expands shell-like paths (tilde) to absolute paths.
///
/// The path can be:
/// - Absolute (starts with `/`): returned as-is
/// - Home-relative (starts with `~`): expanded to the user's home directory
/// - Relative: returned as-is (use `expand_and_resolve` for base directory resolution)
///
/// # Arguments
///
/// * `path` - The path string to expand
///
/// # Returns
///
/// The expanded path as a `PathBuf`.
///
/// # Examples
///
/// ```ignore
/// use stache_lib::utils::path::expand;
///
/// // Tilde expansion
/// let home_config = expand("~/.config/stache");
/// assert!(!home_config.to_string_lossy().starts_with("~"));
///
/// // Absolute paths unchanged
/// let absolute = expand("/usr/local/bin");
/// assert_eq!(absolute.to_string_lossy(), "/usr/local/bin");
///
/// // Relative paths unchanged
/// let relative = expand("config/file.json");
/// assert_eq!(relative.to_string_lossy(), "config/file.json");
/// ```
#[must_use]
pub fn expand(path: &str) -> PathBuf {
    let path = path.trim();

    if path.is_empty() {
        return PathBuf::new();
    }

    // Use shellexpand for tilde expansion
    let expanded = shellexpand::tilde(path);
    PathBuf::from(expanded.as_ref())
}

/// Expands shell-like paths and resolves relative paths against a base directory.
///
/// The path can be:
/// - Absolute (starts with `/`): returned as-is after tilde expansion
/// - Home-relative (starts with `~`): expanded to the user's home directory
/// - Relative: resolved relative to `base_dir`
///
/// # Arguments
///
/// * `path` - The path string to expand and resolve
/// * `base_dir` - The base directory for resolving relative paths
///
/// # Returns
///
/// The expanded and resolved absolute path.
///
/// # Examples
///
/// ```ignore
/// use std::path::Path;
/// use stache_lib::utils::path::expand_and_resolve;
///
/// let base = Path::new("/config/dir");
///
/// // Relative paths resolved against base
/// let resolved = expand_and_resolve(".env", base);
/// assert_eq!(resolved.to_string_lossy(), "/config/dir/.env");
///
/// // Tilde paths expanded (not resolved against base)
/// let home = expand_and_resolve("~/.secrets/.env", base);
/// assert!(!home.to_string_lossy().starts_with("~"));
///
/// // Absolute paths unchanged
/// let absolute = expand_and_resolve("/absolute/path", base);
/// assert_eq!(absolute.to_string_lossy(), "/absolute/path");
/// ```
#[must_use]
pub fn expand_and_resolve(path: &str, base_dir: &Path) -> PathBuf {
    let path = path.trim();

    if path.is_empty() {
        return PathBuf::new();
    }

    // First expand tilde
    let expanded = expand(path);

    // If the result is absolute, return it
    if expanded.is_absolute() {
        return expanded;
    }

    // Otherwise resolve against base directory
    base_dir.join(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_empty() {
        let result = expand("");
        assert_eq!(result, PathBuf::new());
    }

    #[test]
    fn test_expand_whitespace() {
        let result = expand("   ");
        assert_eq!(result, PathBuf::new());
    }

    #[test]
    fn test_expand_absolute_path() {
        let result = expand("/absolute/path/to/file");
        assert_eq!(result, PathBuf::from("/absolute/path/to/file"));
    }

    #[test]
    fn test_expand_relative_path() {
        let result = expand("relative/path");
        assert_eq!(result, PathBuf::from("relative/path"));
    }

    #[test]
    fn test_expand_tilde_path() {
        let result = expand("~/Documents/file.txt");
        // Should not start with ~
        assert!(!result.to_string_lossy().starts_with('~'));
        // Should end with the rest of the path
        assert!(result.to_string_lossy().ends_with("Documents/file.txt"));
    }

    #[test]
    fn test_expand_tilde_only() {
        let result = expand("~");
        // Should not be ~ anymore
        assert!(!result.to_string_lossy().starts_with('~'));
        // Should not be empty
        assert!(!result.to_string_lossy().is_empty());
    }

    #[test]
    fn test_expand_and_resolve_empty() {
        let base = PathBuf::from("/base/dir");
        let result = expand_and_resolve("", &base);
        assert_eq!(result, PathBuf::new());
    }

    #[test]
    fn test_expand_and_resolve_absolute() {
        let base = PathBuf::from("/base/dir");
        let result = expand_and_resolve("/absolute/path", &base);
        assert_eq!(result, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn test_expand_and_resolve_relative() {
        let base = PathBuf::from("/base/dir");
        let result = expand_and_resolve("relative/path", &base);
        assert_eq!(result, PathBuf::from("/base/dir/relative/path"));
    }

    #[test]
    fn test_expand_and_resolve_dot_env() {
        let base = PathBuf::from("/config/dir");
        let result = expand_and_resolve(".env", &base);
        assert_eq!(result, PathBuf::from("/config/dir/.env"));
    }

    #[test]
    fn test_expand_and_resolve_tilde() {
        let base = PathBuf::from("/base/dir");
        let result = expand_and_resolve("~/some/path", &base);
        // Tilde should be expanded, not resolved against base
        assert!(!result.to_string_lossy().starts_with('~'));
        assert!(result.to_string_lossy().ends_with("some/path"));
        // Should NOT contain /base/dir
        assert!(!result.to_string_lossy().contains("/base/dir"));
    }

    #[test]
    fn test_expand_and_resolve_whitespace_trimmed() {
        let base = PathBuf::from("/base/dir");
        let result = expand_and_resolve("  relative/path  ", &base);
        assert_eq!(result, PathBuf::from("/base/dir/relative/path"));
    }
}
