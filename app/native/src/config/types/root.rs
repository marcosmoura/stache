//! Root configuration types and loading functions.
//!
//! Contains the main `StacheConfig` struct and configuration file loading utilities.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::audio::ProxyAudioConfig;
use super::bar::BarConfig;
use super::command_quit::CommandQuitConfig;
use super::menu_anywhere::MenuAnywhereConfig;
use super::notunes::NoTunesConfig;
use super::tiling::TilingConfig;
use super::wallpaper::WallpaperConfig;

/// Commands to execute from configuration.
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

impl Default for ShortcutCommands {
    fn default() -> Self { Self::Multiple(Vec::new()) }
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

/// Root configuration structure for Stache.
///
/// This structure is designed to be extended with additional sections
/// as new features are added to the application.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct StacheConfig {
    /// Bar configuration for status bar UI components.
    ///
    /// Contains settings for weather.
    pub bar: BarConfig,

    /// Command Quit (hold ⌘Q to quit) configuration.
    ///
    /// Prevents accidental application quits by requiring users to hold
    /// ⌘Q for a configurable duration before quitting. Enabled by default.
    #[serde(rename = "commandQuit")]
    pub command_quit: CommandQuitConfig,

    /// Desktop wallpaper configuration.
    ///
    /// Controls dynamic wallpaper rotation, effects, and display.
    pub wallpapers: WallpaperConfig,

    /// Global keyboard keybindings configuration.
    ///
    /// The key is the shortcut string (e.g., "Command+Control+R" or "CapsLock+S").
    /// `CapsLock+<key>` is handled as a Stache-only pseudo modifier: tapping
    /// Caps Lock alone still toggles capitalization, while holding Caps Lock
    /// with a configured key executes the command.
    /// The value is either a single command string or an array of commands.
    pub keybindings: HashMap<String, ShortcutCommands>,

    /// Commands to execute once when Stache starts.
    ///
    /// The value is either a single command string or an array of commands.
    /// Multiple commands are executed sequentially.
    #[serde(rename = "execOnStartup")]
    pub exec_on_startup: ShortcutCommands,

    /// `MenuAnywhere` configuration.
    ///
    /// Allows summoning the current application's menu bar at the cursor position.
    #[serde(rename = "menuAnywhere")]
    pub menu_anywhere: MenuAnywhereConfig,

    /// Proxy audio configuration for automatic device routing.
    ///
    /// Enables intelligent audio device switching based on device availability
    /// and priority. `AirPlay` devices are always given highest priority.
    #[serde(rename = "proxyAudio")]
    pub proxy_audio: ProxyAudioConfig,

    /// noTunes configuration.
    ///
    /// Prevents Apple Music/iTunes from auto-launching and optionally
    /// launches a preferred music player instead.
    #[serde(rename = "notunes")]
    pub notunes: NoTunesConfig,

    /// Tiling window manager configuration.
    ///
    /// Provides virtual workspace management with multiple layout modes.
    /// Disabled by default.
    pub tiling: TilingConfig,
}

impl StacheConfig {
    /// Prepares the configuration for use by pre-computing cached values.
    ///
    /// This method should be called after loading the configuration to:
    /// - Pre-compute lowercase versions of window rule strings for faster matching
    ///
    /// This is called automatically by [`load_config()`].
    pub fn prepare(&mut self) {
        // Prepare ignore rules
        for rule in &mut self.tiling.ignore {
            rule.prepare();
        }

        // Prepare workspace rules
        for workspace in &mut self.tiling.workspaces {
            for rule in &mut workspace.rules {
                rule.prepare();
            }
        }

        // Prepare border ignore rules
        for rule in &mut self.tiling.borders.ignore {
            rule.prepare();
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
                "No configuration file found. Expected at ~/.config/stache/config.json, \
                ~/Library/Application Support/stache/config.json, or ~/.stache.json"
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

/// Configuration file names to search for (in priority order).
const CONFIG_FILE_NAMES: &[&str] = &["config.jsonc", "config.json"];

/// Legacy configuration file names in home directory.
const LEGACY_CONFIG_FILE_NAMES: &[&str] = &[".stache.jsonc", ".stache.json"];

/// Returns the possible configuration file paths in priority order.
///
/// The function checks the following locations (both `.jsonc` and `.json` variants):
/// 1. `~/.config/stache/config.jsonc` or `config.json`
/// 2. `~/Library/Application Support/stache/config.jsonc` or `config.json` (macOS native)
/// 3. `~/.stache.jsonc` or `~/.stache.json` (legacy/simple location)
///
/// If `$XDG_CONFIG_HOME` is set, it takes priority over `~/.config`.
#[must_use]
pub fn config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Check XDG_CONFIG_HOME first if explicitly set
    if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
        let stache_dir = PathBuf::from(xdg_config).join("stache");
        for filename in CONFIG_FILE_NAMES {
            paths.push(stache_dir.join(filename));
        }
    }

    // Always check ~/.config/stache/ (common on macOS for CLI tools)
    if let Some(home) = dirs::home_dir() {
        let stache_dir = home.join(".config").join("stache");
        for filename in CONFIG_FILE_NAMES {
            let path = stache_dir.join(filename);
            // Only add if not already in the list (XDG_CONFIG_HOME might be ~/.config)
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }

    // macOS native: ~/Library/Application Support/stache/
    if let Some(config_dir) = dirs::config_dir() {
        let stache_dir = config_dir.join("stache");
        for filename in CONFIG_FILE_NAMES {
            let path = stache_dir.join(filename);
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }

    // Legacy: $HOME/.stache.jsonc or $HOME/.stache.json
    if let Some(home) = dirs::home_dir() {
        for filename in LEGACY_CONFIG_FILE_NAMES {
            paths.push(home.join(filename));
        }
    }

    paths
}

/// Loads the configuration from a specific file path.
///
/// The configuration file supports JSONC format (JSON with comments).
/// Both single-line (`//`) and multi-line (`/* */`) comments are stripped
/// before parsing.
///
/// # Arguments
///
/// * `path` - The path to the configuration file
///
/// # Returns
///
/// Returns `Ok((StacheConfig, PathBuf))` if the configuration file was found and parsed successfully.
/// Returns `Err` variants for file not found, I/O errors, or parsing errors.
///
/// # Errors
///
/// Returns `ConfigError::NotFound` if the configuration file does not exist.
/// Returns `ConfigError::IoError` if the configuration file could not be read.
/// Returns `ConfigError::ParseError` if the configuration file contains invalid JSON.
pub fn load_config_from_path(path: &PathBuf) -> Result<(StacheConfig, PathBuf), ConfigError> {
    if !path.exists() {
        return Err(ConfigError::NotFound);
    }

    let file = fs::File::open(path)?;
    // Strip comments from JSONC before parsing
    let reader = json_comments::StripComments::new(file);
    let mut config: StacheConfig = serde_json::from_reader(reader)?;
    // Pre-compute cached values for faster runtime operations
    config.prepare();
    Ok((config, path.clone()))
}

/// Loads the configuration from the first available config file.
///
/// The configuration file supports JSONC format (JSON with comments).
/// Both single-line (`//`) and multi-line (`/* */`) comments are stripped
/// before parsing.
///
/// # Returns
///
/// Returns `Ok((StacheConfig, PathBuf))` if a configuration file was found and parsed successfully.
/// Returns `Err(ConfigError::NotFound)` if no configuration file exists.
/// Returns other `Err` variants for I/O or parsing errors.
///
/// # Errors
///
/// Returns `ConfigError::NotFound` if no configuration file exists in any of the expected locations.
/// Returns `ConfigError::IoError` if a configuration file exists but could not be read.
/// Returns `ConfigError::ParseError` if the configuration file contains invalid JSON.
pub fn load_config() -> Result<(StacheConfig, PathBuf), ConfigError> {
    for path in config_paths() {
        if path.exists() {
            return load_config_from_path(&path);
        }
    }

    Err(ConfigError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_is_empty() {
        let config = StacheConfig::default();
        assert!(config.keybindings.is_empty());
        assert!(config.exec_on_startup.get_commands().is_empty());
    }

    #[test]
    fn test_config_deserializes_exec_on_startup_commands() {
        let json = r#"{
            "execOnStartup": [
                "open -a Terminal",
                "stache reload"
            ]
        }"#;

        let config: StacheConfig = serde_json::from_str(json).unwrap();

        assert_eq!(config.exec_on_startup.get_commands(), vec![
            "open -a Terminal",
            "stache reload"
        ]);
    }

    #[test]
    fn test_config_deserializes_single_command() {
        let json = r#"{
            "keybindings": {
                "Ctrl+Shift+S": "stache reload"
            }
        }"#;

        let config: StacheConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.keybindings.len(), 1);

        let commands = config.keybindings.get("Ctrl+Shift+S").unwrap();
        assert_eq!(commands.get_commands(), vec!["stache reload"]);
    }

    #[test]
    fn test_config_deserializes_multiple_commands() {
        let json = r#"{
            "keybindings": {
                "Command+Control+R": [
                    "stache reload",
                    "open -a Terminal"
                ]
            }
        }"#;

        let config: StacheConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.keybindings.len(), 1);

        let commands = config.keybindings.get("Command+Control+R").unwrap();
        assert_eq!(commands.get_commands(), vec![
            "stache reload",
            "open -a Terminal"
        ]);
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
}
