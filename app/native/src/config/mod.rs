//! Configuration module for Barba Shell.
//!
//! This module provides configuration types, loading functionality, and file watching
//! for hot-reloading configuration changes.
//!
//! The configuration file supports JSONC format (JSON with comments).
//! Both single-line (`//`) and multi-line (`/* */`) comments are allowed.

pub mod env;
mod types;
mod watcher;

use std::path::PathBuf;
use std::sync::OnceLock;

pub use types::{
    AudioDeviceDependency, AudioDevicePriority, BarConfig, BarbaConfig, ConfigError, MatchStrategy,
    MenuAnywhereConfig, MenuAnywhereModifier, MenuAnywhereMouseButton, NoTunesConfig,
    ProxyAudioConfig, ProxyAudioInputConfig, ProxyAudioOutputConfig, ShortcutCommands,
    TargetMusicApp, WallpaperConfig, WallpaperMode, WeatherConfig, config_paths,
    load_config as load_config_with_path,
};
pub use watcher::watch_config_file;

/// Global configuration instance, loaded once at startup.
static CONFIG: OnceLock<BarbaConfig> = OnceLock::new();

/// Path to the currently loaded configuration file.
static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Loads the configuration and stores it in a global static.
///
/// This function is idempotent - calling it multiple times will return
/// the same configuration instance.
///
/// If no configuration file is found, returns a default empty configuration.
pub fn init() -> &'static BarbaConfig {
    CONFIG.get_or_init(|| match load_config_with_path() {
        Ok((config, path)) => {
            let _ = CONFIG_PATH.set(path);
            config
        }
        Err(ConfigError::NotFound) => BarbaConfig::default(),
        Err(err) => {
            eprintln!("barba: warning: failed to load configuration: {err}");
            BarbaConfig::default()
        }
    })
}

/// Returns the global configuration instance.
///
/// # Panics
///
/// Panics if called before `init()` has been called.
pub fn get_config() -> &'static BarbaConfig {
    CONFIG.get().expect("Configuration not initialized. Call init() first.")
}

/// Returns the path to the loaded configuration file, if any.
pub fn get_config_path() -> Option<&'static PathBuf> { CONFIG_PATH.get() }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shared_types_are_available() {
        // Verify that shared types are accessible
        let config = BarbaConfig::default();
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
        let single = ShortcutCommands::Single("barba reload".to_string());
        assert_eq!(single.get_commands(), vec!["barba reload"]);

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
    fn test_wallpaper_config_with_path_is_enabled() {
        let config = WallpaperConfig {
            path: "/path/to/wallpapers".to_string(),
            ..Default::default()
        };
        assert!(config.is_enabled());
    }

    #[test]
    fn test_wallpaper_config_with_list_is_enabled() {
        let config = WallpaperConfig {
            list: vec!["image.jpg".to_string()],
            ..Default::default()
        };
        assert!(config.is_enabled());
    }
}
