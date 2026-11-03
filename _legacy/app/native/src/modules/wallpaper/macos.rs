//! macOS wallpaper setting functionality.
//!
//! Uses native macOS APIs to set the desktop wallpaper for each screen.

use std::path::Path;

use objc::runtime::{Class, Object};
use objc::{msg_send, sel, sel_impl};

use super::processing;

/// Errors that can occur when setting the wallpaper.
#[derive(Debug)]
pub enum WallpaperError {
    /// The wallpaper file does not exist.
    FileNotFound(String),
    /// Failed to set the wallpaper.
    SetWallpaperFailed(String),
    /// Invalid screen index.
    InvalidScreen(usize),
}

impl std::fmt::Display for WallpaperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileNotFound(path) => write!(f, "Wallpaper file not found: {path}"),
            Self::SetWallpaperFailed(msg) => write!(f, "Failed to set wallpaper: {msg}"),
            Self::InvalidScreen(idx) => write!(f, "Invalid screen index: {idx}"),
        }
    }
}

impl std::error::Error for WallpaperError {}

/// Returns the number of available screens.
///
/// Delegates to the shared screen detection in processing module.
#[must_use]
#[inline]
pub fn screen_count() -> usize { processing::get_screen_count() }

/// Sets the desktop wallpaper for all screens.
///
/// # Arguments
///
/// * `path` - Path to the image file to set as wallpaper
///
/// # Errors
///
/// Returns an error if the file doesn't exist or the wallpaper setting fails.
pub fn set_wallpaper(path: &Path) -> Result<(), WallpaperError> {
    if !path.exists() {
        return Err(WallpaperError::FileNotFound(path.display().to_string()));
    }

    let path_str = path.display().to_string();

    wallpaper::set_from_path(&path_str)
        .map_err(|e| WallpaperError::SetWallpaperFailed(e.to_string()))
}

/// Sets the desktop wallpaper for a specific screen.
///
/// # Arguments
///
/// * `path` - Path to the image file to set as wallpaper
/// * `screen_index` - The 0-based index of the screen
///
/// # Errors
///
/// Returns an error if the file doesn't exist, the screen index is invalid,
/// or the wallpaper setting fails.
#[allow(clippy::cast_possible_truncation)]
pub fn set_wallpaper_for_screen(path: &Path, screen_index: usize) -> Result<(), WallpaperError> {
    if !path.exists() {
        return Err(WallpaperError::FileNotFound(path.display().to_string()));
    }

    unsafe {
        // Get NSScreen class and screens array
        let Some(screen_class) = Class::get("NSScreen") else {
            return Err(WallpaperError::SetWallpaperFailed(
                "Failed to get NSScreen class".to_string(),
            ));
        };

        let screens: *mut Object = msg_send![screen_class, screens];
        if screens.is_null() {
            return Err(WallpaperError::SetWallpaperFailed(
                "Failed to get screens".to_string(),
            ));
        }

        let count: usize = msg_send![screens, count];
        if screen_index >= count {
            return Err(WallpaperError::InvalidScreen(screen_index));
        }

        // Get the specific screen
        let screen: *mut Object = msg_send![screens, objectAtIndex: screen_index];
        if screen.is_null() {
            return Err(WallpaperError::InvalidScreen(screen_index));
        }

        // Get NSWorkspace shared workspace
        let Some(workspace_class) = Class::get("NSWorkspace") else {
            return Err(WallpaperError::SetWallpaperFailed(
                "Failed to get NSWorkspace class".to_string(),
            ));
        };

        let workspace: *mut Object = msg_send![workspace_class, sharedWorkspace];
        if workspace.is_null() {
            return Err(WallpaperError::SetWallpaperFailed(
                "Failed to get shared workspace".to_string(),
            ));
        }

        // Create NSURL from path
        let Some(url_class) = Class::get("NSURL") else {
            return Err(WallpaperError::SetWallpaperFailed(
                "Failed to get NSURL class".to_string(),
            ));
        };

        let path_str = path.display().to_string();
        let Some(string_class) = Class::get("NSString") else {
            return Err(WallpaperError::SetWallpaperFailed(
                "Failed to get NSString class".to_string(),
            ));
        };

        let path_ns: *mut Object = msg_send![string_class, alloc];
        let path_ns: *mut Object =
            msg_send![path_ns, initWithBytes:path_str.as_ptr() length:path_str.len() encoding:4u64]; // NSUTF8StringEncoding = 4

        let url: *mut Object = msg_send![url_class, fileURLWithPath: path_ns];
        if url.is_null() {
            return Err(WallpaperError::SetWallpaperFailed(
                "Failed to create URL from path".to_string(),
            ));
        }

        // Set wallpaper for screen: setDesktopImageURL:forScreen:options:error:
        let options: *mut Object = msg_send![Class::get("NSDictionary").unwrap(), dictionary];
        let mut error: *mut Object = std::ptr::null_mut();

        let success: bool = msg_send![workspace, setDesktopImageURL:url forScreen:screen options:options error:&mut error];

        if !success {
            let error_msg = if error.is_null() {
                "Unknown error".to_string()
            } else {
                let desc: *mut Object = msg_send![error, localizedDescription];
                if desc.is_null() {
                    "Unknown error".to_string()
                } else {
                    let bytes: *const u8 = msg_send![desc, UTF8String];
                    if bytes.is_null() {
                        "Unknown error".to_string()
                    } else {
                        std::ffi::CStr::from_ptr(bytes.cast()).to_string_lossy().to_string()
                    }
                }
            };
            return Err(WallpaperError::SetWallpaperFailed(error_msg));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wallpaper_error_file_not_found_display() {
        let err = WallpaperError::FileNotFound("/path/to/missing.jpg".to_string());
        let msg = err.to_string();
        assert!(msg.contains("not found"));
        assert!(msg.contains("/path/to/missing.jpg"));
    }

    #[test]
    fn test_wallpaper_error_set_failed_display() {
        let err = WallpaperError::SetWallpaperFailed("permission denied".to_string());
        let msg = err.to_string();
        assert!(msg.contains("Failed to set wallpaper"));
        assert!(msg.contains("permission denied"));
    }

    #[test]
    fn test_wallpaper_error_invalid_screen_display() {
        let err = WallpaperError::InvalidScreen(5);
        let msg = err.to_string();
        assert!(msg.contains("Invalid screen index"));
        assert!(msg.contains('5'));
    }

    #[test]
    fn test_wallpaper_error_is_debug() {
        let err = WallpaperError::FileNotFound("test.jpg".to_string());
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("FileNotFound"));
    }

    #[test]
    fn test_screen_count_returns_at_least_one() {
        let count = screen_count();
        assert!(count >= 1, "Screen count should be at least 1");
    }

    #[test]
    fn test_set_wallpaper_returns_error_for_nonexistent_file() {
        let result = set_wallpaper(std::path::Path::new("/nonexistent/path/to/wallpaper.jpg"));
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), WallpaperError::FileNotFound(_)));
    }

    #[test]
    fn test_set_wallpaper_for_screen_returns_error_for_nonexistent_file() {
        let result =
            set_wallpaper_for_screen(std::path::Path::new("/nonexistent/path/to/wallpaper.jpg"), 0);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), WallpaperError::FileNotFound(_)));
    }
}
