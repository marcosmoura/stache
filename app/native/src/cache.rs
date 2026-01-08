//! Cache directory utilities.
//!
//! Provides a centralized way to get the application's cache directory.
//! Uses `~/Library/Caches/{APP_BUNDLE_ID}/` on macOS for persistence across reboots,
//! with a fallback to `/tmp/{APP_BUNDLE_ID}/` if the cache directory is unavailable.

use std::path::PathBuf;

use crate::constants::APP_BUNDLE_ID;

/// Returns the root cache directory for the application.
///
/// # Returns
///
/// A `PathBuf` pointing to `~/Library/Caches/{APP_BUNDLE_ID}` on macOS,
/// or `/tmp/{APP_BUNDLE_ID}` if the cache directory is unavailable.
#[must_use]
pub fn get_cache_dir() -> PathBuf {
    dirs::cache_dir().map_or_else(
        || PathBuf::from(format!("/tmp/{APP_BUNDLE_ID}")),
        |cache| cache.join(APP_BUNDLE_ID),
    )
}

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
#[must_use]
pub fn get_cache_subdir(subdir: &str) -> PathBuf { get_cache_dir().join(subdir) }

/// Clears the entire cache directory.
///
/// Removes all files and subdirectories from the application's cache directory.
///
/// # Returns
///
/// * `Ok(bytes_freed)` - The approximate number of bytes freed
/// * `Err(error)` - If the operation failed
///
/// # Errors
///
/// Returns an error if:
/// - The cache directory doesn't exist (not an error, returns Ok(0))
/// - Permission denied when removing files
/// - I/O errors during removal
pub fn clear_cache() -> std::io::Result<u64> {
    let cache_dir = get_cache_dir();

    if !cache_dir.exists() {
        return Ok(0);
    }

    let bytes_freed = calculate_dir_size(&cache_dir)?;

    // Remove the cache directory and all its contents
    std::fs::remove_dir_all(&cache_dir)?;

    Ok(bytes_freed)
}

/// Calculates the total size of a directory in bytes.
fn calculate_dir_size(path: &PathBuf) -> std::io::Result<u64> {
    let mut total = 0u64;

    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                total += calculate_dir_size(&path)?;
            } else {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }

    Ok(total)
}

/// Formats a byte count as a human-readable string.
///
/// # Arguments
///
/// * `bytes` - The number of bytes
///
/// # Returns
///
/// A human-readable string like "1.5 MB" or "256 KB"
#[must_use]
#[allow(clippy::cast_precision_loss)] // Precision loss is acceptable for human-readable output
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_cache_dir_contains_bundle_id() {
        let path = get_cache_dir();
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains(APP_BUNDLE_ID),
            "Path should contain bundle ID: {path_str}"
        );
    }

    #[test]
    fn test_get_cache_subdir_contains_component() {
        let path = get_cache_subdir("wallpapers");
        let path_str = path.to_string_lossy();
        assert!(path_str.contains("wallpapers"), "Path should contain subdir");
        assert!(path_str.contains(APP_BUNDLE_ID), "Path should contain bundle ID");
    }

    #[test]
    fn test_format_bytes_bytes() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(1023), "1023 bytes");
    }

    #[test]
    fn test_format_bytes_kb() {
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1536), "1.50 KB");
    }

    #[test]
    fn test_format_bytes_mb() {
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
        assert_eq!(format_bytes(1024 * 1024 + 512 * 1024), "1.50 MB");
    }

    #[test]
    fn test_format_bytes_gb() {
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.00 GB");
    }
}
