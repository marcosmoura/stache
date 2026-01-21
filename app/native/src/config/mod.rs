//! Configuration module for Stache.
//!
//! This module provides configuration types, loading functionality, and file watching
//! for hot-reloading configuration changes.
//!
//! The configuration file supports JSONC format (JSON with comments).
//! Both single-line (`//`) and multi-line (`/* */`) comments are allowed.

pub mod env;
pub mod template;
pub mod types;
mod watcher;

use std::path::PathBuf;
use std::sync::OnceLock;

pub use types::{
    AnimationConfig, AudioDeviceDependency, AudioDevicePriority, BarConfig, BorderColor,
    BorderStateConfig, BordersConfig, CommandQuitConfig, ConfigError, DimensionValue, EasingType,
    FloatingConfig, FloatingPreset, GapValue, GapsConfig, GapsConfigValue, GradientConfig,
    LayoutType, MasterConfig, MasterPosition, MatchStrategy, MenuAnywhereConfig,
    MenuAnywhereModifier, MenuAnywhereMouseButton, NoTunesConfig, ProxyAudioConfig,
    ProxyAudioInputConfig, ProxyAudioOutputConfig, Rgba, ShortcutCommands, StacheConfig,
    TargetMusicApp, TilingConfig, WallpaperConfig, WallpaperMode, WeatherConfig, WindowRule,
    WorkspaceConfig, config_paths, load_config as load_config_default, load_config_from_path,
    parse_color, parse_hex_color, parse_rgba_color,
};
pub use watcher::watch_config_file;

/// Global configuration instance, loaded once at startup.
static CONFIG: OnceLock<StacheConfig> = OnceLock::new();

/// Path to the currently loaded configuration file.
static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Custom config path override (set via CLI --config flag).
static CUSTOM_CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Sets a custom configuration file path to use instead of the default search paths.
///
/// This must be called before `init()` or `get_config()` to take effect.
///
/// # Arguments
///
/// * `path` - The path to the custom configuration file
///
/// # Returns
///
/// `true` if the path was set successfully, `false` if a path was already set.
pub fn set_custom_config_path(path: PathBuf) -> bool { CUSTOM_CONFIG_PATH.set(path).is_ok() }

/// Loads the configuration from disk.
///
/// Returns the loaded configuration, or a default configuration if loading fails.
/// If no configuration file exists, creates a template configuration file.
fn load_or_default() -> StacheConfig {
    // Check for custom config path first
    let result = CUSTOM_CONFIG_PATH.get().map_or_else(load_config_default, load_config_from_path);

    match result {
        Ok((config, path)) => {
            let _ = CONFIG_PATH.set(path);
            config
        }
        Err(ConfigError::NotFound) => {
            // Create a template config file at the default location
            create_default_config_file();
            StacheConfig::default()
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to load configuration, using defaults");
            StacheConfig::default()
        }
    }
}

/// Creates a template configuration file at the default location.
///
/// This is called when no configuration file is found during startup.
fn create_default_config_file() {
    // Get the first (preferred) config path
    let Some(config_path) = config_paths().into_iter().next() else {
        tracing::debug!("no config path available for creating template");
        return;
    };

    // Only create if it doesn't exist
    if config_path.exists() {
        return;
    }

    match template::create_config_file(&config_path) {
        Ok(()) => {
            let _ = CONFIG_PATH.set(config_path.clone());
            tracing::info!(
                path = %config_path.display(),
                "created default configuration file"
            );
        }
        Err(err) => {
            tracing::debug!(
                error = %err,
                path = %config_path.display(),
                "failed to create default configuration file"
            );
        }
    }
}

/// Initializes and returns the global configuration instance.
///
/// This function is idempotent - calling it multiple times will return
/// the same configuration instance.
///
/// If no configuration file is found, returns a default empty configuration.
pub fn init() -> &'static StacheConfig { CONFIG.get_or_init(load_or_default) }

/// Returns the global configuration instance, initializing it if necessary.
///
/// This function is safe to call at any time - it will lazily initialize
/// the configuration if it hasn't been loaded yet.
///
/// If no configuration file is found, returns a default empty configuration.
pub fn get_config() -> &'static StacheConfig { CONFIG.get_or_init(load_or_default) }

/// Returns the path to the loaded configuration file, if any.
pub fn get_config_path() -> Option<&'static PathBuf> { CONFIG_PATH.get() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shared_types_are_available() {
        // Verify that shared types are accessible
        let config = StacheConfig::default();
        assert!(config.keybindings.is_empty());

        let wallpaper = WallpaperConfig::default();
        assert!(!wallpaper.is_enabled());

        let mode = WallpaperMode::default();
        assert_eq!(mode, WallpaperMode::Random);
    }

    #[test]
    fn test_weather_config() {
        let weather = WeatherConfig::default();
        assert!(!weather.is_enabled());
        assert!(weather.api_keys.is_empty());
        assert!(weather.default_location.is_empty());
    }

    #[test]
    fn test_shortcut_commands() {
        let single = ShortcutCommands::Single("stache reload".to_string());
        assert_eq!(single.get_commands(), vec!["stache reload"]);

        let multiple = ShortcutCommands::Multiple(vec!["cmd1".to_string(), "cmd2".to_string()]);
        assert_eq!(multiple.get_commands(), vec!["cmd1", "cmd2"]);
    }

    #[test]
    fn test_config_error() {
        let err = ConfigError::NotFound;
        let msg = err.to_string();
        assert!(msg.contains("No configuration file found"));
    }

    #[test]
    fn test_wallpaper_mode_sequential() {
        let mode = WallpaperMode::Sequential;
        assert_ne!(mode, WallpaperMode::Random);
    }

    #[test]
    fn test_wallpaper_config_with_path_has_wallpapers() {
        let config = WallpaperConfig {
            path: "/path/to/wallpapers".to_string(),
            ..Default::default()
        };
        assert!(config.has_wallpapers());
    }

    #[test]
    fn test_wallpaper_config_with_list_has_wallpapers() {
        let config = WallpaperConfig {
            list: vec!["image.jpg".to_string()],
            ..Default::default()
        };
        assert!(config.has_wallpapers());
    }

    #[test]
    fn test_bar_config_default_is_disabled() {
        let config = BarConfig::default();
        assert!(!config.is_enabled());
    }
}
