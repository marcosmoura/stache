//! Configuration types for Barba Shell.
//!
//! This module provides the configuration types and loading functionality.
//! The configuration file supports JSONC format (JSON with comments).
//! Both single-line (`//`) and multi-line (`/* */`) comments are allowed.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Strategy for matching device names in the priority list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MatchStrategy {
    /// Exact match (case-insensitive). This is the default strategy.
    #[default]
    Exact,
    /// Device name contains the specified string (case-insensitive).
    Contains,
    /// Device name starts with the specified string (case-insensitive).
    StartsWith,
    /// Device name matches the specified regex pattern.
    Regex,
}

/// Dependency condition for audio device selection.
///
/// Specifies a device that must be present (connected) for the parent device
/// to be considered in the priority list. The dependent device itself will
/// never be switched to; it only serves as a condition.
///
/// Example: "External Speakers" might depend on "`MiniFuse` 2" being connected,
/// since the speakers are physically connected through the audio interface.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioDeviceDependency {
    /// The name (or pattern) of the device that must be present.
    pub name: String,

    /// The strategy for matching the dependency device name.
    /// - `exact`: Exact match (case-insensitive). Default if not specified.
    /// - `contains`: Device name contains the string (case-insensitive).
    /// - `startsWith`: Device name starts with the string (case-insensitive).
    /// - `regex`: Device name matches the regex pattern.
    #[serde(default)]
    pub strategy: MatchStrategy,
}

/// Priority entry for audio device selection.
///
/// Defines a single device in the priority list with its name and matching strategy.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct AudioDevicePriority {
    /// The name (or pattern) of the audio device to match.
    pub name: String,

    /// The strategy for matching the device name.
    /// - `exact`: Exact match (case-insensitive). Default if not specified.
    /// - `contains`: Device name contains the string (case-insensitive).
    /// - `startsWith`: Device name starts with the string (case-insensitive).
    /// - `regex`: Device name matches the regex pattern.
    #[serde(default)]
    pub strategy: MatchStrategy,

    /// Optional dependency condition.
    /// If specified, this device will only be considered if the dependent device
    /// is present (connected). The dependent device will never be switched to;
    /// it only serves as a condition for enabling this device.
    ///
    /// Example: External speakers connected via an audio interface might
    /// depend on the interface being present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<AudioDeviceDependency>,
}

/// Input device configuration for proxy audio.
///
/// Defines the virtual input device name and priority list for device selection.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct ProxyAudioInputConfig {
    /// Name of the virtual input device (used if a virtual device is installed).
    /// Default: "Barba Virtual Input"
    pub name: String,

    /// Priority list for input device selection.
    /// Devices are checked in order; the first available device is selected.
    /// `AirPlay` devices are always given highest priority automatically.
    pub priority: Vec<AudioDevicePriority>,
}

impl Default for ProxyAudioInputConfig {
    fn default() -> Self {
        Self {
            name: "Barba Virtual Input".to_string(),
            priority: Vec::new(),
        }
    }
}

/// Output device configuration for proxy audio.
///
/// Defines the virtual output device name, buffer size, and priority list.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct ProxyAudioOutputConfig {
    /// Name of the virtual output device (used if a virtual device is installed).
    /// Default: "Barba Virtual Output"
    pub name: String,

    /// Audio buffer size in frames. Smaller values reduce latency but may cause artifacts.
    /// Recommended values: 128 (low latency), 256 (balanced), 512 (stable).
    /// Default: 256
    pub buffer_size: u32,

    /// Priority list for output device selection.
    /// Devices are checked in order; the first available device is selected.
    /// `AirPlay` devices are always given highest priority automatically.
    pub priority: Vec<AudioDevicePriority>,
}

impl Default for ProxyAudioOutputConfig {
    fn default() -> Self {
        Self {
            name: "Barba Virtual Output".to_string(),
            buffer_size: 256,
            priority: Vec::new(),
        }
    }
}

/// Proxy audio configuration for automatic device routing.
///
/// This configuration enables intelligent audio device switching based on
/// device availability and priority. When enabled, the app automatically
/// switches to the highest-priority available device when devices connect
/// or disconnect.
///
/// `AirPlay` devices are always given the highest priority, even if not
/// explicitly listed in the priority configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
#[derive(Default)]
pub struct ProxyAudioConfig {
    /// Whether proxy audio functionality is enabled.
    /// When enabled, the app will automatically switch audio devices
    /// based on the priority configuration.
    /// Default: false
    pub enabled: bool,

    /// Input device configuration.
    pub input: ProxyAudioInputConfig,

    /// Output device configuration.
    pub output: ProxyAudioOutputConfig,
}

impl ProxyAudioConfig {
    /// Returns whether proxy audio functionality is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }
}

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
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct WeatherConfig {
    /// Path to an environment file containing API keys.
    ///
    /// The file should contain key-value pairs in the format `KEY=value`.
    /// Supported keys:
    /// - `VISUAL_CROSSING_API_KEY` - API key for Visual Crossing Weather API
    ///
    /// The path can be:
    /// - Relative to the config file directory (e.g., `.env`, `secrets/.env`)
    /// - Absolute path (e.g., `/Users/username/.secrets/.env`)
    /// - Use `~` for home directory (e.g., `~/.config/barba/.env`)
    ///
    /// Example `.env` file:
    /// ```env
    /// VISUAL_CROSSING_API_KEY=your_api_key_here
    /// ```
    pub api_keys: String,

    /// Default location for weather data when geolocation fails.
    /// Can be a city name, address, or coordinates.
    pub default_location: String,
}

impl WeatherConfig {
    /// Returns whether weather functionality is enabled.
    ///
    /// Weather is considered enabled if an API keys file is configured.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { !self.api_keys.is_empty() }
}

/// Mouse button options for `MenuAnywhere` trigger.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MenuAnywhereMouseButton {
    /// Trigger on right mouse button click.
    #[default]
    RightClick,
    /// Trigger on middle mouse button click.
    MiddleClick,
}

/// Keyboard modifier options for `MenuAnywhere` trigger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MenuAnywhereModifier {
    /// Control key (^).
    Control,
    /// Option/Alt key (⌥).
    Option,
    /// Command key (⌘).
    Command,
    /// Shift key (⇧).
    Shift,
}

/// Configuration for the `MenuAnywhere` feature.
///
/// `MenuAnywhere` allows you to summon the current application's menu bar
/// at any location on screen using a configurable keyboard + mouse trigger.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct MenuAnywhereConfig {
    /// Whether `MenuAnywhere` is enabled.
    /// Default: false
    pub enabled: bool,

    /// Keyboard modifiers that must be held when clicking.
    /// Default: `["control", "command"]`
    pub modifiers: Vec<MenuAnywhereModifier>,

    /// Mouse button that triggers the menu.
    /// Default: `"rightClick"`
    pub mouse_button: MenuAnywhereMouseButton,
}

impl Default for MenuAnywhereConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            modifiers: vec![MenuAnywhereModifier::Control, MenuAnywhereModifier::Command],
            mouse_button: MenuAnywhereMouseButton::RightClick,
        }
    }
}

impl MenuAnywhereConfig {
    /// Returns whether `MenuAnywhere` functionality is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }

    /// Returns the required modifier flags as a bitmask for Core Graphics events.
    ///
    /// The returned value uses the macOS `CGEventFlags` constants:
    /// - Control: `0x0004_0000`
    /// - Option:  `0x0008_0000`
    /// - Command: `0x0010_0000`
    /// - Shift:   `0x0002_0000`
    #[must_use]
    pub fn required_modifier_flags(&self) -> u64 {
        let mut flags = 0u64;
        for modifier in &self.modifiers {
            flags |= match modifier {
                MenuAnywhereModifier::Control => 0x0004_0000,
                MenuAnywhereModifier::Option => 0x0008_0000,
                MenuAnywhereModifier::Command => 0x0010_0000,
                MenuAnywhereModifier::Shift => 0x0002_0000,
            };
        }
        flags
    }
}

/// Bar configuration for the status bar UI components.
///
/// Contains settings for bar-specific features like weather.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BarConfig {
    /// Weather status bar configuration.
    pub weather: WeatherConfig,
}

/// Target music application for noTunes replacement.
///
/// When Apple Music or iTunes is blocked, this app will be launched instead.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TargetMusicApp {
    /// Tidal music streaming service.
    #[default]
    Tidal,
    /// Spotify music streaming service.
    Spotify,
    /// Do not launch any replacement app.
    None,
}

impl TargetMusicApp {
    /// Returns the application path for the target music app.
    #[must_use]
    pub const fn app_path(&self) -> Option<&'static str> {
        match self {
            Self::Tidal => Some("/Applications/TIDAL.app"),
            Self::Spotify => Some("/Applications/Spotify.app"),
            Self::None => None,
        }
    }

    /// Returns the bundle identifier for the target music app.
    #[must_use]
    pub const fn bundle_id(&self) -> Option<&'static str> {
        match self {
            Self::Tidal => Some("com.tidal.desktop"),
            Self::Spotify => Some("com.spotify.client"),
            Self::None => None,
        }
    }

    /// Returns the display name for the target music app.
    #[must_use]
    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::Tidal => "Tidal",
            Self::Spotify => "Spotify",
            Self::None => "None",
        }
    }
}

/// Configuration for the noTunes feature.
///
/// noTunes prevents Apple Music or iTunes from launching automatically
/// (e.g., when pressing media keys or connecting Bluetooth headphones)
/// and optionally launches a preferred music player instead.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct NoTunesConfig {
    /// Whether noTunes functionality is enabled.
    /// Default: true
    pub enabled: bool,

    /// The music app to launch when Apple Music/iTunes is blocked.
    /// Options: "tidal", "spotify", "none"
    /// Default: "tidal"
    pub target_app: TargetMusicApp,
}

impl Default for NoTunesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            target_app: TargetMusicApp::Tidal,
        }
    }
}

impl NoTunesConfig {
    /// Returns whether noTunes functionality is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }
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
    pub bar: BarConfig,

    /// Desktop wallpaper configuration.
    ///
    /// Controls dynamic wallpaper rotation, effects, and display.
    pub wallpapers: WallpaperConfig,

    /// Global keyboard keybindings configuration.
    ///
    /// The key is the shortcut string (e.g., "Command+Control+R").
    /// The value is either a single command string or an array of commands.
    pub keybindings: HashMap<String, ShortcutCommands>,

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
                "No configuration file found. Expected at ~/.config/barba/config.json, \
                ~/Library/Application Support/barba/config.json, or ~/.barba.json"
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
const LEGACY_CONFIG_FILE_NAMES: &[&str] = &[".barba.jsonc", ".barba.json"];

/// Returns the possible configuration file paths in priority order.
///
/// The function checks the following locations (both `.jsonc` and `.json` variants):
/// 1. `~/.config/barba/config.jsonc` or `config.json`
/// 2. `~/Library/Application Support/barba/config.jsonc` or `config.json` (macOS native)
/// 3. `~/.barba.jsonc` or `~/.barba.json` (legacy/simple location)
///
/// If `$XDG_CONFIG_HOME` is set, it takes priority over `~/.config`.
#[must_use]
pub fn config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Check XDG_CONFIG_HOME first if explicitly set
    if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
        let barba_dir = PathBuf::from(xdg_config).join("barba");
        for filename in CONFIG_FILE_NAMES {
            paths.push(barba_dir.join(filename));
        }
    }

    // Always check ~/.config/barba/ (common on macOS for CLI tools)
    if let Some(home) = dirs::home_dir() {
        let barba_dir = home.join(".config").join("barba");
        for filename in CONFIG_FILE_NAMES {
            let path = barba_dir.join(filename);
            // Only add if not already in the list (XDG_CONFIG_HOME might be ~/.config)
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }

    // macOS native: ~/Library/Application Support/barba/
    if let Some(config_dir) = dirs::config_dir() {
        let barba_dir = config_dir.join("barba");
        for filename in CONFIG_FILE_NAMES {
            let path = barba_dir.join(filename);
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }

    // Legacy: $HOME/.barba.jsonc or $HOME/.barba.json
    if let Some(home) = dirs::home_dir() {
        for filename in LEGACY_CONFIG_FILE_NAMES {
            paths.push(home.join(filename));
        }
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
                "Ctrl+Shift+S": "barba reload"
            }
        }"#;

        let config: BarbaConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.keybindings.len(), 1);

        let commands = config.keybindings.get("Ctrl+Shift+S").unwrap();
        assert_eq!(commands.get_commands(), vec!["barba reload"]);
    }

    #[test]
    fn test_config_deserializes_multiple_commands() {
        let json = r#"{
            "keybindings": {
                "Command+Control+R": [
                    "barba reload",
                    "open -a Terminal"
                ]
            }
        }"#;

        let config: BarbaConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.keybindings.len(), 1);

        let commands = config.keybindings.get("Command+Control+R").unwrap();
        assert_eq!(commands.get_commands(), vec!["barba reload", "open -a Terminal"]);
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
