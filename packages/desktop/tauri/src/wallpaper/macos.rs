//! macOS wallpaper setting functionality.
//!
//! Uses the `wallpaper` crate to set the desktop wallpaper.

use std::path::Path;

/// Errors that can occur when setting the wallpaper.
#[derive(Debug)]
pub enum WallpaperError {
    /// The wallpaper file does not exist.
    FileNotFound(String),
    /// Failed to set the wallpaper.
    SetWallpaperFailed(String),
}

impl std::fmt::Display for WallpaperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FileNotFound(path) => write!(f, "Wallpaper file not found: {path}"),
            Self::SetWallpaperFailed(msg) => write!(f, "Failed to set wallpaper: {msg}"),
        }
    }
}

impl std::error::Error for WallpaperError {}

/// Sets the desktop wallpaper.
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
