//! Configuration module for Barba Shell.
//!
//! This module provides a centralized configuration system that reads from either:
//! - `$HOME/.barba.json`
//! - `$XDG_CONFIG_HOME/barba/config.json`
//!
//! The configuration file supports JSONC format (JSON with comments).
//! Both single-line (`//`) and multi-line (`/* */`) comments are allowed.
//!
//! The module also watches for changes to the config file and triggers an app
//! restart when the configuration is modified.
//!
//! The configuration is designed to be extensible for future features beyond hotkeys.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

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

/// Global configuration instance, loaded once at startup.
static CONFIG: OnceLock<BarbaConfig> = OnceLock::new();

/// Path to the currently loaded configuration file.
static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Root configuration structure for Barba Shell.
///
/// This structure is designed to be extended with additional sections
/// as new features are added to the application.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BarbaConfig {
    /// Global keyboard shortcuts configuration.
    ///
    /// The key is the shortcut string (e.g., "Command+Control+R").
    /// The value is either a single command string or an array of commands.
    ///
    /// Example:
    /// ```json
    /// {
    ///   "shortcuts": {
    ///     "Command+Control+R": ["barba reload", "hyprspace reload-config"],
    ///     "Command+Option+Control+1": "barba workspace-changed terminal"
    ///   }
    /// }
    /// ```
    pub shortcuts: HashMap<String, ShortcutCommands>,

    /// Dynamic wallpaper configuration.
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
    pub wallpapers: WallpaperConfig,
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
fn config_paths() -> Vec<PathBuf> {
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
/// Also stores the path to the loaded config file for file watching.
///
/// # Returns
///
/// Returns `Ok(BarbaConfig)` if a configuration file was found and parsed successfully.
/// Returns `Err(ConfigError::NotFound)` if no configuration file exists.
/// Returns other `Err` variants for I/O or parsing errors.
pub fn load_config() -> Result<BarbaConfig, ConfigError> {
    for path in config_paths() {
        if path.exists() {
            let file = fs::File::open(&path)?;
            // Strip comments from JSONC before parsing
            let reader = json_comments::StripComments::new(file);
            let config: BarbaConfig = serde_json::from_reader(reader)?;
            // Store the path for file watching
            let _ = CONFIG_PATH.set(path);
            return Ok(config);
        }
    }

    Err(ConfigError::NotFound)
}

/// Loads the configuration and stores it in a global static.
///
/// This function is idempotent - calling it multiple times will return
/// the same configuration instance.
///
/// If no configuration file is found, returns a default empty configuration.
pub fn init() -> &'static BarbaConfig {
    CONFIG.get_or_init(|| match load_config() {
        Ok(config) => config,
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

/// Returns the global configuration instance if it has been initialized.
#[allow(dead_code)]
pub fn try_get_config() -> Option<&'static BarbaConfig> { CONFIG.get() }

/// Returns the path to the loaded configuration file, if any.
pub fn get_config_path() -> Option<&'static PathBuf> { CONFIG_PATH.get() }

/// Starts watching the configuration file for changes.
///
/// When the config file is modified, the app will restart to apply the new configuration.
/// This function spawns a background thread that watches the file.
///
/// # Arguments
///
/// * `app_handle` - The Tauri app handle used to trigger a restart (release builds only)
#[allow(unused_variables, clippy::needless_pass_by_value)]
pub fn watch_config_file<R: tauri::Runtime>(app_handle: AppHandle<R>) {
    let Some(config_path) = get_config_path().cloned() else {
        // No config file loaded, nothing to watch
        return;
    };

    let config_filename =
        config_path.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();

    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();

        // Create a watcher
        let mut watcher: RecommendedWatcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(err) => {
                eprintln!("barba: warning: failed to create config watcher: {err}");
                return;
            }
        };

        // Watch the config file's parent directory to catch file replacements
        // (some editors save by writing to a temp file then renaming)
        let watch_path = config_path.parent().unwrap_or(&config_path);

        if let Err(err) = watcher.watch(watch_path, RecursiveMode::NonRecursive) {
            eprintln!("barba: warning: failed to watch config file: {err}");
            return;
        }

        loop {
            match rx.recv() {
                Ok(Ok(event)) => {
                    // Check if this event affects our config file by filename
                    let affects_config = event
                        .paths
                        .iter()
                        .any(|p| p.file_name().is_some_and(|name| name == config_filename));

                    if !affects_config {
                        continue;
                    }

                    // In debug mode, just log a message since restart kills the dev server.
                    // In release mode, restart the app to apply the new configuration.
                    #[cfg(debug_assertions)]
                    {
                        eprintln!(
                            "barba: config file changed. Restart the app to apply new settings."
                        );
                    }

                    #[cfg(not(debug_assertions))]
                    {
                        app_handle.restart();
                    }
                }
                Ok(Err(err)) => {
                    eprintln!("barba: warning: config watch error: {err}");
                }
                Err(_) => {
                    // Channel closed, watcher dropped
                    break;
                }
            }
        }
    });
}

/// Generates a JSON Schema for the Barba configuration.
///
/// The schema includes all configuration options with their types,
/// descriptions, and default values.
#[must_use]
pub fn generate_schema() -> schemars::Schema {
    let mut schema = schemars::schema_for!(BarbaConfig);

    // Add $id for proper schema identification
    if let Some(obj) = schema.as_object_mut() {
        obj.insert(
            "$id".to_string(),
            serde_json::json!(
                "https://raw.githubusercontent.com/marcosmoura/barba-shell/main/barba.schema.json"
            ),
        );
    }

    schema
}

/// Generates a JSON Schema string for the Barba configuration.
///
/// Returns a pretty-printed JSON string that can be saved to a file
/// or used for validation.
#[must_use]
pub fn generate_schema_json() -> String {
    let schema = generate_schema();
    serde_json::to_string_pretty(&schema).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_is_empty() {
        let config = BarbaConfig::default();
        assert!(config.shortcuts.is_empty());
    }

    #[test]
    fn test_config_deserializes_single_command() {
        let json = r#"{
            "shortcuts": {
                "Ctrl+Shift+S": "barba workspace-changed coding"
            }
        }"#;

        let config: BarbaConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.shortcuts.len(), 1);

        let commands = config.shortcuts.get("Ctrl+Shift+S").unwrap();
        assert_eq!(commands.get_commands(), vec!["barba workspace-changed coding"]);
    }

    #[test]
    fn test_config_deserializes_multiple_commands() {
        let json = r#"{
            "shortcuts": {
                "Command+Control+R": [
                    "barba reload",
                    "hyprspace reload-config"
                ]
            }
        }"#;

        let config: BarbaConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.shortcuts.len(), 1);

        let commands = config.shortcuts.get("Command+Control+R").unwrap();
        assert_eq!(commands.get_commands(), vec![
            "barba reload",
            "hyprspace reload-config"
        ]);
    }

    #[test]
    fn test_config_deserializes_mixed_format() {
        let json = r#"{
            "shortcuts": {
                "Command+Control+R": ["barba reload", "hyprspace reload-config"],
                "Command+Option+Control+1": "barba workspace-changed terminal"
            }
        }"#;

        let config: BarbaConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.shortcuts.len(), 2);

        let multi_commands = config.shortcuts.get("Command+Control+R").unwrap();
        assert_eq!(multi_commands.commands_display(), "[2 commands]");

        let single_command = config.shortcuts.get("Command+Option+Control+1").unwrap();
        assert_eq!(
            single_command.commands_display(),
            "barba workspace-changed terminal"
        );
    }

    #[test]
    fn test_config_serializes_correctly() {
        let mut shortcuts = HashMap::new();
        shortcuts.insert(
            "Ctrl+Alt+T".to_string(),
            ShortcutCommands::Single("barba test".to_string()),
        );
        let config = BarbaConfig {
            shortcuts,
            wallpapers: WallpaperConfig::default(),
        };

        let json = serde_json::to_string_pretty(&config).unwrap();
        assert!(json.contains("Ctrl+Alt+T"));
        assert!(json.contains("barba test"));
    }

    #[test]
    fn test_empty_json_produces_default() {
        let json = "{}";
        let config: BarbaConfig = serde_json::from_str(json).unwrap();
        assert!(config.shortcuts.is_empty());
    }

    #[test]
    fn test_config_paths_are_not_empty() {
        // On most systems, we should have at least one path
        let paths = config_paths();
        // This test might fail in very unusual environments
        assert!(!paths.is_empty() || std::env::var("HOME").is_err());
    }

    #[test]
    fn test_empty_command_returns_no_commands() {
        // Empty string should return empty vec (shortcut capture mode)
        let empty_single = ShortcutCommands::Single(String::new());
        assert!(empty_single.get_commands().is_empty());

        // Whitespace-only string should also return empty vec
        let whitespace_single = ShortcutCommands::Single("   ".to_string());
        assert!(whitespace_single.get_commands().is_empty());

        // Empty array should return empty vec
        let empty_array = ShortcutCommands::Multiple(vec![]);
        assert!(empty_array.get_commands().is_empty());

        // Array with only empty strings should return empty vec
        let empty_strings = ShortcutCommands::Multiple(vec![String::new(), "  ".to_string()]);
        assert!(empty_strings.get_commands().is_empty());
    }

    #[test]
    fn test_config_deserializes_empty_commands_for_shortcut_capture() {
        let json = r#"{
            "shortcuts": {
                "Command+H": "",
                "Command+M": []
            }
        }"#;

        let config: BarbaConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.shortcuts.len(), 2);

        // Both should have no commands but still be registered
        let cmd_h = config.shortcuts.get("Command+H").unwrap();
        assert!(cmd_h.get_commands().is_empty());

        let cmd_m = config.shortcuts.get("Command+M").unwrap();
        assert!(cmd_m.get_commands().is_empty());
    }

    #[test]
    fn test_config_parses_jsonc_with_comments() {
        // Test that JSONC with comments is parsed correctly
        let jsonc = r#"{
            // This is a single-line comment
            "shortcuts": {
                /* Multi-line comment
                   explaining this shortcut */
                "Command+R": "barba reload", // inline comment
                "Command+H": "" // Disable hide window
            }
        }"#;

        // Use json_comments to strip comments like load_config does
        let reader = json_comments::StripComments::new(jsonc.as_bytes());
        let config: BarbaConfig = serde_json::from_reader(reader).unwrap();

        assert_eq!(config.shortcuts.len(), 2);
        assert_eq!(config.shortcuts.get("Command+R").unwrap().get_commands(), vec![
            "barba reload"
        ]);
        assert!(config.shortcuts.get("Command+H").unwrap().get_commands().is_empty());
    }

    #[test]
    fn test_generate_schema_produces_valid_json() {
        let schema_json = generate_schema_json();
        assert!(!schema_json.is_empty());

        // Verify it's valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&schema_json).unwrap();

        // Verify it has the expected structure
        assert!(parsed["$id"].as_str().unwrap().contains("barba.schema.json"));
        assert_eq!(parsed["$schema"], "https://json-schema.org/draft/2020-12/schema");
        assert_eq!(parsed["title"], "BarbaConfig");
        assert!(parsed["properties"]["shortcuts"].is_object());
        assert!(parsed["properties"]["wallpapers"].is_object());
    }
}
