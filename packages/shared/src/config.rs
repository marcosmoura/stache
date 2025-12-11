//! Configuration types for Barba Shell.
//!
//! This module provides the configuration types and loading functionality
//! that can be used by both the desktop app and CLI.
//!
//! The configuration file supports JSONC format (JSON with comments).
//! Both single-line (`//`) and multi-line (`/* */`) comments are allowed.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::tiling::TilingConfig;

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
///
/// Example:
/// ```json
/// {
///   "wallpapers": {
///     "path": "/path/to/wallpapers",
///     "list": ["wallpaper1.jpg", "wallpaper2.png"],
///     "interval": 300,
///     "mode": "random",
///     "radius": 10,
///     "blur": 5
///   }
/// }
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct WallpaperConfig {
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
    ///
    /// Wallpapers are considered enabled if either a path is specified
    /// or the list contains at least one wallpaper.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { !self.path.is_empty() || !self.list.is_empty() }
}

/// Weather configuration for the status bar.
///
/// Example:
/// ```json
/// {
///   "weather": {
///     "visualCrossingApiKey": "YOUR_API_KEY",
///     "defaultLocation": "Prague"
///   }
/// }
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct WeatherConfig {
    /// API key for Visual Crossing Weather API.
    /// Get one at <https://www.visualcrossing.com/>
    pub visual_crossing_api_key: String,

    /// Default location for weather data when geolocation fails.
    /// Can be a city name, address, or coordinates.
    pub default_location: String,
}

impl WeatherConfig {
    /// Returns whether weather functionality is enabled.
    ///
    /// Weather is considered enabled if an API key is configured.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { !self.visual_crossing_api_key.is_empty() }
}

/// Bar configuration for the status bar UI components.
///
/// Contains settings for bar-specific features like weather.
///
/// Example:
/// ```json
/// {
///   "bar": {
///     "weather": {
///       "visualCrossingApiKey": "YOUR_API_KEY",
///       "defaultLocation": "Prague"
///     }
///   }
/// }
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BarConfig {
    /// Weather status bar configuration.
    pub weather: WeatherConfig,
}

/// Root configuration structure for Barba Shell.
///
/// This structure is designed to be extended with additional sections
/// as new features are added to the application.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BarbaConfig {
    /// Bar configuration for status bar UI components.
    ///
    /// Contains settings for weather.
    ///
    /// Example:
    /// ```json
    /// {
    ///   "bar": {
    ///     "weather": {
    ///       "visualCrossingApiKey": "YOUR_API_KEY"
    ///     }
    ///   }
    /// }
    /// ```
    pub bar: BarConfig,

    /// Desktop wallpaper configuration.
    ///
    /// Controls dynamic wallpaper rotation, effects, and display.
    ///
    /// Example:
    /// ```json
    /// {
    ///   "wallpapers": {
    ///     "path": "/path/to/wallpapers",
    ///     "mode": "random",
    ///     "interval": 300,
    ///     "radius": 12,
    ///     "blur": 8
    ///   }
    /// }
    /// ```
    pub wallpapers: WallpaperConfig,

    /// Global keyboard keybindings configuration.
    ///
    /// The key is the shortcut string (e.g., "Command+Control+R").
    /// The value is either a single command string or an array of commands.
    ///
    /// Example:
    /// ```json
    /// {
    ///   "keybindings": {
    ///     "Command+Control+R": ["barba reload", "hyprspace reload-config"],
    ///     "Command+Option+Control+1": "barba workspace-changed terminal"
    ///   }
    /// }
    /// ```
    pub keybindings: HashMap<String, ShortcutCommands>,

    /// Tiling window manager configuration.
    ///
    /// Example:
    /// ```json
    /// {
    ///   "tiling": {
    ///     "enabled": true,
    ///     "defaultLayout": "tiling",
    ///     "gaps": { "inner": 10, "outer": 15 },
    ///     "workspaces": [
    ///       { "name": "1", "layout": "tiling", "screen": "main" }
    ///     ]
    ///   }
    /// }
    /// ```
    pub tiling: TilingConfig,
}

/// Commands to execute for a keyboard shortcut.
///
/// Can be either a single command string or an array of commands
/// that will be executed sequentially.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ShortcutCommands {
    /// A single command to execute.
    Single(String),
    /// Multiple commands to execute sequentially (never in parallel).
    Multiple(Vec<String>),
}

impl ShortcutCommands {
    /// Returns all commands to execute.
    ///
    /// Empty strings are filtered out. If the result is empty (either from
    /// an empty string, empty array, or array of empty strings), the shortcut
    /// will be registered but no commands will be executed. This is useful
    /// for capturing/blocking global OS shortcuts.
    #[must_use]
    pub fn get_commands(&self) -> Vec<&str> {
        match self {
            Self::Single(cmd) => {
                let trimmed = cmd.trim();
                if trimmed.is_empty() {
                    vec![]
                } else {
                    vec![trimmed]
                }
            }
            Self::Multiple(cmds) => {
                cmds.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect()
            }
        }
    }

    /// Returns a display string for the command(s) for logging purposes.
    #[must_use]
    pub fn commands_display(&self) -> String {
        match self {
            Self::Single(cmd) => cmd.clone(),
            Self::Multiple(cmds) => format!("[{} commands]", cmds.len()),
        }
    }
}

/// Errors that can occur when loading the configuration.
#[derive(Debug)]
pub enum ConfigError {
    /// No configuration file was found in any of the expected locations.
    NotFound,
    /// The configuration file exists but could not be read.
    IoError(std::io::Error),
    /// The configuration file contains invalid JSON.
    ParseError(serde_json::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(
                f,
                "No configuration file found. Expected at ~/.barba.json or $XDG_CONFIG_HOME/barba/config.json"
            ),
            Self::IoError(err) => write!(f, "Failed to read configuration file: {err}"),
            Self::ParseError(err) => write!(f, "Failed to parse configuration file: {err}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IoError(err) => Some(err),
            Self::ParseError(err) => Some(err),
            Self::NotFound => None,
        }
    }
}

impl From<std::io::Error> for ConfigError {
    fn from(err: std::io::Error) -> Self { Self::IoError(err) }
}

impl From<serde_json::Error> for ConfigError {
    fn from(err: serde_json::Error) -> Self { Self::ParseError(err) }
}

/// Returns the possible configuration file paths in priority order.
///
/// The function checks the following locations:
/// 1. `$XDG_CONFIG_HOME/barba/config.json` (if `XDG_CONFIG_HOME` is set)
/// 2. `~/.config/barba/config.json` (XDG default on Linux, also checked on macOS)
/// 3. `~/Library/Application Support/barba/config.json` (macOS native)
/// 4. `~/.barba.json` (legacy/simple location)
#[must_use]
pub fn config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Check XDG_CONFIG_HOME first if explicitly set
    if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
        let path = PathBuf::from(xdg_config).join("barba").join("config.json");
        paths.push(path);
    }

    // Always check ~/.config/barba/config.json (common on macOS for CLI tools)
    if let Some(home) = dirs::home_dir() {
        let path = home.join(".config").join("barba").join("config.json");
        // Only add if not already in the list (XDG_CONFIG_HOME might be ~/.config)
        if !paths.contains(&path) {
            paths.push(path);
        }
    }

    // macOS native: ~/Library/Application Support/barba/config.json
    if let Some(config_dir) = dirs::config_dir() {
        let path = config_dir.join("barba").join("config.json");
        if !paths.contains(&path) {
            paths.push(path);
        }
    }

    // $HOME/.barba.json (legacy/simple location)
    if let Some(home) = dirs::home_dir() {
        let path = home.join(".barba.json");
        paths.push(path);
    }

    paths
}

/// Loads the configuration from the first available config file.
///
/// The configuration file supports JSONC format (JSON with comments).
/// Both single-line (`//`) and multi-line (`/* */`) comments are stripped
/// before parsing.
///
/// # Returns
///
/// Returns `Ok((BarbaConfig, PathBuf))` if a configuration file was found and parsed successfully.
/// Returns `Err(ConfigError::NotFound)` if no configuration file exists.
/// Returns other `Err` variants for I/O or parsing errors.
///
/// # Errors
///
/// Returns `ConfigError::NotFound` if no configuration file exists in any of the expected locations.
/// Returns `ConfigError::IoError` if a configuration file exists but could not be read.
/// Returns `ConfigError::ParseError` if the configuration file contains invalid JSON.
pub fn load_config() -> Result<(BarbaConfig, PathBuf), ConfigError> {
    for path in config_paths() {
        if path.exists() {
            let file = fs::File::open(&path)?;
            // Strip comments from JSONC before parsing
            let reader = json_comments::StripComments::new(file);
            let config: BarbaConfig = serde_json::from_reader(reader)?;
            return Ok((config, path));
        }
    }

    Err(ConfigError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_is_empty() {
        let config = BarbaConfig::default();
        assert!(config.keybindings.is_empty());
    }

    #[test]
    fn test_config_deserializes_single_command() {
        let json = r#"{
            "keybindings": {
                "Ctrl+Shift+S": "barba workspace-changed coding"
            }
        }"#;

        let config: BarbaConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.keybindings.len(), 1);

        let commands = config.keybindings.get("Ctrl+Shift+S").unwrap();
        assert_eq!(commands.get_commands(), vec!["barba workspace-changed coding"]);
    }

    #[test]
    fn test_config_deserializes_multiple_commands() {
        let json = r#"{
            "keybindings": {
                "Command+Control+R": [
                    "barba reload",
                    "hyprspace reload-config"
                ]
            }
        }"#;

        let config: BarbaConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.keybindings.len(), 1);

        let commands = config.keybindings.get("Command+Control+R").unwrap();
        assert_eq!(commands.get_commands(), vec![
            "barba reload",
            "hyprspace reload-config"
        ]);
    }

    #[test]
    fn test_config_deserializes_mixed_format() {
        let json = r#"{
            "keybindings": {
                "Command+Control+R": ["barba reload", "hyprspace reload-config"],
                "Command+Option+Control+1": "barba workspace-changed terminal"
            }
        }"#;

        let config: BarbaConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.keybindings.len(), 2);

        let multi_commands = config.keybindings.get("Command+Control+R").unwrap();
        assert_eq!(multi_commands.commands_display(), "[2 commands]");

        let single_command = config.keybindings.get("Command+Option+Control+1").unwrap();
        assert_eq!(
            single_command.commands_display(),
            "barba workspace-changed terminal"
        );
    }

    #[test]
    fn test_config_serializes_correctly() {
        let mut keybindings = HashMap::new();
        keybindings.insert(
            "Ctrl+Alt+T".to_string(),
            ShortcutCommands::Single("barba test".to_string()),
        );
        let config = BarbaConfig {
            bar: BarConfig::default(),
            wallpapers: WallpaperConfig::default(),
            keybindings,
            tiling: TilingConfig::default(),
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        assert!(json.contains("Ctrl+Alt+T"));
        assert!(json.contains("barba test"));
    }

    #[test]
    fn test_empty_json_produces_default() {
        let json = "{}";
        let config: BarbaConfig = serde_json::from_str(json).unwrap();
        assert!(config.keybindings.is_empty());
    }

    #[test]
    fn test_config_paths_are_not_empty() {
        let paths = config_paths();
        assert!(!paths.is_empty() || std::env::var("HOME").is_err());
    }

    #[test]
    fn test_empty_command_returns_no_commands() {
        let empty_single = ShortcutCommands::Single(String::new());
        assert!(empty_single.get_commands().is_empty());

        let whitespace_single = ShortcutCommands::Single("   ".to_string());
        assert!(whitespace_single.get_commands().is_empty());

        let empty_array = ShortcutCommands::Multiple(vec![]);
        assert!(empty_array.get_commands().is_empty());

        let empty_strings = ShortcutCommands::Multiple(vec![String::new(), "  ".to_string()]);
        assert!(empty_strings.get_commands().is_empty());
    }

    #[test]
    fn test_config_deserializes_empty_commands_for_shortcut_capture() {
        let json = r#"{
            "keybindings": {
                "Command+H": "",
                "Command+M": []
            }
        }"#;

        let config: BarbaConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.keybindings.len(), 2);

        let cmd_h = config.keybindings.get("Command+H").unwrap();
        assert!(cmd_h.get_commands().is_empty());

        let cmd_m = config.keybindings.get("Command+M").unwrap();
        assert!(cmd_m.get_commands().is_empty());
    }

    #[test]
    fn test_config_parses_jsonc_with_comments() {
        let jsonc = r#"{
            // This is a single-line comment
            "keybindings": {
                /* Multi-line comment */
                "Command+R": "barba reload", // inline comment
                "Command+H": ""
            }
        }"#;

        let reader = json_comments::StripComments::new(jsonc.as_bytes());
        let config: BarbaConfig = serde_json::from_reader(reader).unwrap();

        assert_eq!(config.keybindings.len(), 2);
        assert_eq!(
            config.keybindings.get("Command+R").unwrap().get_commands(),
            vec!["barba reload"]
        );
        assert!(config.keybindings.get("Command+H").unwrap().get_commands().is_empty());
    }

    #[test]
    fn test_wallpaper_config_is_enabled() {
        let empty = WallpaperConfig::default();
        assert!(!empty.is_enabled());

        let with_path = WallpaperConfig {
            path: "/some/path".to_string(),
            ..Default::default()
        };
        assert!(with_path.is_enabled());

        let with_list = WallpaperConfig {
            list: vec!["wallpaper.jpg".to_string()],
            ..Default::default()
        };
        assert!(with_list.is_enabled());
    }

    #[test]
    fn test_wallpaper_mode_default() {
        let mode = WallpaperMode::default();
        assert_eq!(mode, WallpaperMode::Random);
    }
}
