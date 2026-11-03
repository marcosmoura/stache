//! Wallpaper configuration types.
//!
//! Configuration for dynamic wallpaper management with effects.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Wallpaper cycling mode.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum WallpaperMode {
    /// Select a random wallpaper each time.
    #[default]
    Random,
    /// Cycle through wallpapers in order.
    Sequential,
}

/// Wallpaper configuration for dynamic wallpaper management.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WallpaperConfig {
    /// Whether wallpaper management is enabled.
    /// Default: false
    pub enabled: bool,

    /// Directory containing wallpaper images.
    /// If specified, all image files in this directory will be used,
    /// overriding the `list` field.
    pub path: String,

    /// List of wallpaper filenames to use.
    /// If `path` is specified, this list is ignored.
    pub list: Vec<String>,

    /// Time in seconds between wallpaper changes.
    /// If set to 0, the wallpaper will not change after the initial setting.
    pub interval: u64,

    /// Wallpaper selection mode: "random" or "sequential".
    pub mode: WallpaperMode,

    /// Radius in pixels for rounded corners.
    pub radius: u32,

    /// Blur level in pixels for Gaussian blur effect.
    pub blur: u32,
}

impl WallpaperConfig {
    /// Returns whether wallpaper functionality is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }

    /// Returns whether there are wallpapers configured (path or list).
    #[must_use]
    pub const fn has_wallpapers(&self) -> bool { !self.path.is_empty() || !self.list.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wallpaper_config_default_is_disabled() {
        let config = WallpaperConfig::default();
        assert!(!config.is_enabled());
        assert!(!config.has_wallpapers());
    }

    #[test]
    fn test_wallpaper_config_enabled() {
        let config = WallpaperConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(config.is_enabled());
    }

    #[test]
    fn test_wallpaper_config_has_wallpapers() {
        let empty = WallpaperConfig::default();
        assert!(!empty.has_wallpapers());

        let with_path = WallpaperConfig {
            path: "/some/path".to_string(),
            ..Default::default()
        };
        assert!(with_path.has_wallpapers());

        let with_list = WallpaperConfig {
            list: vec!["wallpaper.jpg".to_string()],
            ..Default::default()
        };
        assert!(with_list.has_wallpapers());
    }

    #[test]
    fn test_wallpaper_mode_default() {
        let mode = WallpaperMode::default();
        assert_eq!(mode, WallpaperMode::Random);
    }
}
