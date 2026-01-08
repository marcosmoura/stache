//! Cache directory utilities.
//!
//! Provides a centralized way to get cache subdirectories for various components.
//! Uses `~/Library/Caches/{APP_BUNDLE_ID}/` on macOS for persistence across reboots,
//! with a fallback to `/tmp/{APP_BUNDLE_ID}/` if the cache directory is unavailable.

use std::path::PathBuf;

use crate::constants::APP_BUNDLE_ID;

/// Returns a cache subdirectory for the given component.
///
/// # Arguments
///
/// * `subdir` - The subdirectory name within the app's cache directory (e.g., `wallpapers`, `media_artwork`)
///
/// # Returns
///
/// A `PathBuf` pointing to `~/Library/Caches/{APP_BUNDLE_ID}/{subdir}` on macOS,
/// or `/tmp/{APP_BUNDLE_ID}/{subdir}` if the cache directory is unavailable.
///
/// # Example
///
/// ```ignore
/// let wallpaper_cache = get_cache_subdir("wallpapers");
/// // Returns: ~/Library/Caches/com.marcosmoura.barba/wallpapers
/// ```
#[must_use]
pub fn get_cache_subdir(subdir: &str) -> PathBuf {
    dirs::cache_dir().map_or_else(
        || PathBuf::from(format!("/tmp/{APP_BUNDLE_ID}/{subdir}")),
        |cache| cache.join(APP_BUNDLE_ID).join(subdir),
    )
}

/// Returns a cache subdirectory as a string with a trailing separator.
///
/// This is useful when building cache paths via string concatenation.
///
/// # Arguments
///
/// * `subdir` - The subdirectory name within the app's cache directory
///
/// # Returns
///
/// A `String` path ending with a `/` separator.
#[must_use]
pub fn get_cache_subdir_str(subdir: &str) -> String {
    let path = get_cache_subdir(subdir);
    let path_str = path.to_string_lossy().into_owned();
    if path_str.ends_with('/') {
        path_str
    } else {
        format!("{path_str}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_cache_subdir_contains_bundle_id() {
        let path = get_cache_subdir("test_subdir");
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains(APP_BUNDLE_ID),
            "Path should contain bundle ID: {path_str}"
        );
        assert!(
            path_str.contains("test_subdir"),
            "Path should contain subdir: {path_str}"
        );
    }

    #[test]
    fn test_get_cache_subdir_str_ends_with_separator() {
        let path_str = get_cache_subdir_str("artwork");
        assert!(path_str.ends_with('/'), "Path should end with /: {path_str}");
        assert!(
            path_str.contains(APP_BUNDLE_ID),
            "Path should contain bundle ID: {path_str}"
        );
    }

    #[test]
    fn test_different_subdirs_produce_different_paths() {
        let path1 = get_cache_subdir("wallpapers");
        let path2 = get_cache_subdir("media_artwork");
        assert_ne!(path1, path2);
    }

    #[test]
    fn test_cache_subdir_is_absolute_or_tmp() {
        let path = get_cache_subdir("test");
        let path_str = path.to_string_lossy();
        // Should either be in user's cache dir or /tmp
        assert!(path_str.starts_with('/'), "Path should be absolute: {path_str}");
    }
}
