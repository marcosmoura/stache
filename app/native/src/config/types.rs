//! Configuration types for Stache.
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
    /// Default: "Stache Virtual Input"
    pub name: String,

    /// Priority list for input device selection.
    /// Devices are checked in order; the first available device is selected.
    /// `AirPlay` devices are always given highest priority automatically.
    pub priority: Vec<AudioDevicePriority>,
}

impl Default for ProxyAudioInputConfig {
    fn default() -> Self {
        Self {
            name: "Stache Virtual Input".to_string(),
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
    /// Default: "Stache Virtual Output"
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
            name: "Stache Virtual Output".to_string(),
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
    /// - Use `~` for home directory (e.g., `~/.config/stache/.env`)
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

// ============================================================================
// Tiling Window Manager Configuration
// ============================================================================

/// Layout type for workspaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutType {
    /// Binary Space Partitioning - windows arranged in a tree structure.
    Dwindle,
    /// Split layout - windows split based on screen orientation.
    Split,
    /// Vertical split layout - windows arranged vertically.
    SplitVertical,
    /// Horizontal split layout - windows arranged horizontally.
    SplitHorizontal,
    /// Monocle layout - all windows maximized, stacked.
    Monocle,
    /// Master layout - one master window with stack.
    Master,
    /// Grid layout - windows arranged in a grid pattern.
    Grid,
    /// Floating layout - windows can be freely moved and resized.
    #[default]
    Floating,
}

/// Easing function for animations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EasingType {
    /// Linear interpolation.
    Linear,
    /// Ease in (slow start).
    EaseIn,
    /// Ease out (slow end).
    #[default]
    EaseOut,
    /// Ease in and out (slow start and end).
    EaseInOut,
    /// Spring physics animation.
    Spring,
}

/// Default position for floating windows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FloatingPosition {
    /// Center the window on screen.
    #[default]
    Center,
    /// Use the window's last known position.
    Default,
}

/// Animation configuration for window transitions.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct AnimationConfig {
    /// Whether animations are enabled.
    /// Default: false
    pub enabled: bool,

    /// Animation duration in milliseconds for large movements (500+ pixels).
    /// For smaller movements, duration is automatically scaled down.
    /// Default: 200
    pub duration: u32,

    /// Easing function for animations.
    /// Default: "ease-out"
    pub easing: EasingType,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            duration: 200,
            easing: EasingType::EaseOut,
        }
    }
}

/// A dimension value that can be either pixels or a percentage.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum DimensionValue {
    /// Value in pixels.
    Pixels(u32),
    /// Value as a percentage string (e.g., "50%").
    Percentage(String),
}

impl Default for DimensionValue {
    fn default() -> Self { Self::Pixels(0) }
}

impl DimensionValue {
    /// Resolves the dimension value to pixels given a reference size.
    #[must_use]
    pub fn resolve(&self, reference_size: f64) -> f64 {
        match self {
            Self::Pixels(px) => f64::from(*px),
            Self::Percentage(s) => {
                let trimmed = s.trim().trim_end_matches('%');
                trimmed.parse::<f64>().map(|pct| (pct / 100.0) * reference_size).unwrap_or(0.0)
            }
        }
    }
}

/// A gap value that can be uniform, per-axis, or per-side.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum GapValue {
    /// Same value for all sides/axes.
    Uniform(u32),
    /// Different values per axis (for inner gaps).
    PerAxis {
        /// Horizontal gap (left/right between windows).
        horizontal: u32,
        /// Vertical gap (top/bottom between windows).
        vertical: u32,
    },
    /// Different values per side (for outer gaps).
    PerSide {
        /// Top gap.
        top: u32,
        /// Right gap.
        right: u32,
        /// Bottom gap.
        bottom: u32,
        /// Left gap.
        left: u32,
    },
}

impl Default for GapValue {
    fn default() -> Self { Self::Uniform(0) }
}

impl GapValue {
    /// Returns the gap values as (horizontal, vertical) for inner gaps.
    #[must_use]
    pub fn as_inner(&self) -> (u32, u32) {
        match self {
            Self::Uniform(v) => (*v, *v),
            Self::PerAxis { horizontal, vertical } => (*horizontal, *vertical),
            Self::PerSide { left, right, top, bottom } => ((left + right) / 2, (top + bottom) / 2),
        }
    }

    /// Returns the gap values as (top, right, bottom, left) for outer gaps.
    #[must_use]
    pub const fn as_outer(&self) -> (u32, u32, u32, u32) {
        match self {
            Self::Uniform(v) => (*v, *v, *v, *v),
            Self::PerAxis { horizontal, vertical } => {
                (*vertical, *horizontal, *vertical, *horizontal)
            }
            Self::PerSide { top, right, bottom, left } => (*top, *right, *bottom, *left),
        }
    }
}

/// Gaps configuration for a single screen or global.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct GapsConfig {
    /// Inner gaps between windows.
    pub inner: GapValue,
    /// Outer gaps from screen edges.
    pub outer: GapValue,
}

/// Per-screen gaps configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScreenGapsConfig {
    /// Screen identifier: "main", "secondary", or screen name.
    pub screen: String,
    /// Inner gaps between windows.
    #[serde(default)]
    pub inner: GapValue,
    /// Outer gaps from screen edges.
    #[serde(default)]
    pub outer: GapValue,
}

/// Gaps configuration that can be global or per-screen.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum GapsConfigValue {
    /// Same gaps for all screens.
    Global(GapsConfig),
    /// Per-screen gap configuration.
    PerScreen(Vec<ScreenGapsConfig>),
}

impl Default for GapsConfigValue {
    fn default() -> Self { Self::Global(GapsConfig::default()) }
}

/// Floating window preset for quick positioning.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FloatingPreset {
    /// Unique name for this preset.
    pub name: String,

    /// Width: pixels (1440) or percentage ("50%").
    pub width: DimensionValue,

    /// Height: pixels (900) or percentage ("100%").
    pub height: DimensionValue,

    /// X position (ignored if center is true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<DimensionValue>,

    /// Y position (ignored if center is true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<DimensionValue>,

    /// If true, center the window on screen (x and y are ignored).
    #[serde(default)]
    pub center: bool,
}

/// Floating windows configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct FloatingConfig {
    /// Default position for new floating windows.
    /// Default: "center"
    pub default_position: FloatingPosition,

    /// Named presets for window positioning.
    pub presets: Vec<FloatingPreset>,
}

/// Position of the master window in the master layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MasterPosition {
    /// Master window on the left (landscape default).
    #[default]
    Left,
    /// Master window on the right.
    Right,
    /// Master window on top.
    Top,
    /// Master window on bottom.
    Bottom,
    /// Automatically choose based on screen orientation.
    /// - Landscape screens: left
    /// - Portrait screens: top
    Auto,
}

/// Master layout configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct MasterConfig {
    /// Ratio of master window size (0-100).
    /// Default: 60
    pub ratio: u32,
    /// Position of the master window.
    /// Default: auto (left for landscape, top for portrait)
    pub position: MasterPosition,
}

impl Default for MasterConfig {
    fn default() -> Self {
        Self {
            ratio: 60,
            position: MasterPosition::Auto,
        }
    }
}

/// Window matching rule for workspace assignment.
///
/// All specified properties must match (AND logic).
/// At least one property must be specified.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "kebab-case")]
pub struct WindowRule {
    /// Match by bundle identifier (e.g., "com.apple.finder").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,

    /// Match by window title (substring match).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Match by application name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
}

impl WindowRule {
    /// Returns true if the rule has at least one matching criterion.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.app_id.is_some() || self.title.is_some() || self.app_name.is_some()
    }
}

/// Helper function for default screen value.
fn default_screen() -> String { "main".to_string() }

/// Workspace configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceConfig {
    /// Unique name for the workspace.
    pub name: String,

    /// Layout mode for this workspace.
    /// Default: "floating"
    #[serde(default)]
    pub layout: LayoutType,

    /// Screen assignment: "main", "secondary", or screen name.
    /// Default: "main"
    #[serde(default = "default_screen")]
    pub screen: String,

    /// Rules for automatically assigning windows to this workspace.
    #[serde(default)]
    pub rules: Vec<WindowRule>,

    /// Floating preset to apply when windows open in this workspace.
    #[serde(
        default,
        rename = "preset-on-open",
        skip_serializing_if = "Option::is_none"
    )]
    pub preset_on_open: Option<String>,
}

/// Tiling window manager configuration.
///
/// Provides virtual workspace management with multiple layout modes,
/// configurable gaps, and window matching rules.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct TilingConfig {
    /// Whether the tiling window manager is enabled.
    /// Default: false
    pub enabled: bool,

    /// Workspace definitions.
    /// If empty and tiling is enabled, creates one default workspace per screen.
    pub workspaces: Vec<WorkspaceConfig>,

    /// Applications/windows to ignore (never managed by tiling).
    pub ignore: Vec<WindowRule>,

    /// Animation settings for window transitions.
    pub animations: AnimationConfig,

    /// Gap configuration (global or per-screen).
    pub gaps: GapsConfigValue,

    /// Floating window presets and settings.
    pub floating: FloatingConfig,

    /// Master layout settings.
    pub master: MasterConfig,
}

impl TilingConfig {
    /// Returns whether the tiling window manager is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }
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

    /// Tiling window manager configuration.
    ///
    /// Provides virtual workspace management with multiple layout modes.
    /// Disabled by default.
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
            let file = fs::File::open(&path)?;
            // Strip comments from JSONC before parsing
            let reader = json_comments::StripComments::new(file);
            let config: StacheConfig = serde_json::from_reader(reader)?;
            return Ok((config, path));
        }
    }

    Err(ConfigError::NotFound)
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn test_default_config_is_empty() {
        let config = StacheConfig::default();
        assert!(config.keybindings.is_empty());
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

    // ========================================================================
    // MatchStrategy tests
    // ========================================================================

    #[test]
    fn test_match_strategy_default_is_exact() {
        assert_eq!(MatchStrategy::default(), MatchStrategy::Exact);
    }

    #[test]
    fn test_match_strategy_serialization() {
        assert_eq!(
            serde_json::to_string(&MatchStrategy::Exact).unwrap(),
            r#""exact""#
        );
        assert_eq!(
            serde_json::to_string(&MatchStrategy::Contains).unwrap(),
            r#""contains""#
        );
        assert_eq!(
            serde_json::to_string(&MatchStrategy::StartsWith).unwrap(),
            r#""startsWith""#
        );
        assert_eq!(
            serde_json::to_string(&MatchStrategy::Regex).unwrap(),
            r#""regex""#
        );
    }

    #[test]
    fn test_match_strategy_deserialization() {
        assert_eq!(
            serde_json::from_str::<MatchStrategy>(r#""exact""#).unwrap(),
            MatchStrategy::Exact
        );
        assert_eq!(
            serde_json::from_str::<MatchStrategy>(r#""contains""#).unwrap(),
            MatchStrategy::Contains
        );
        assert_eq!(
            serde_json::from_str::<MatchStrategy>(r#""startsWith""#).unwrap(),
            MatchStrategy::StartsWith
        );
        assert_eq!(
            serde_json::from_str::<MatchStrategy>(r#""regex""#).unwrap(),
            MatchStrategy::Regex
        );
    }

    // ========================================================================
    // TargetMusicApp tests
    // ========================================================================

    #[test]
    fn test_target_music_app_default_is_tidal() {
        assert_eq!(TargetMusicApp::default(), TargetMusicApp::Tidal);
    }

    #[test]
    fn test_target_music_app_app_path() {
        assert_eq!(TargetMusicApp::Tidal.app_path(), Some("/Applications/TIDAL.app"));
        assert_eq!(
            TargetMusicApp::Spotify.app_path(),
            Some("/Applications/Spotify.app")
        );
        assert_eq!(TargetMusicApp::None.app_path(), None);
    }

    #[test]
    fn test_target_music_app_bundle_id() {
        assert_eq!(TargetMusicApp::Tidal.bundle_id(), Some("com.tidal.desktop"));
        assert_eq!(TargetMusicApp::Spotify.bundle_id(), Some("com.spotify.client"));
        assert_eq!(TargetMusicApp::None.bundle_id(), None);
    }

    #[test]
    fn test_target_music_app_display_name() {
        assert_eq!(TargetMusicApp::Tidal.display_name(), "Tidal");
        assert_eq!(TargetMusicApp::Spotify.display_name(), "Spotify");
        assert_eq!(TargetMusicApp::None.display_name(), "None");
    }

    #[test]
    fn test_target_music_app_serialization() {
        assert_eq!(
            serde_json::to_string(&TargetMusicApp::Tidal).unwrap(),
            r#""tidal""#
        );
        assert_eq!(
            serde_json::to_string(&TargetMusicApp::Spotify).unwrap(),
            r#""spotify""#
        );
        assert_eq!(
            serde_json::to_string(&TargetMusicApp::None).unwrap(),
            r#""none""#
        );
    }

    // ========================================================================
    // MenuAnywhereConfig tests
    // ========================================================================

    #[test]
    fn test_menu_anywhere_config_default() {
        let config = MenuAnywhereConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.modifiers, vec![
            MenuAnywhereModifier::Control,
            MenuAnywhereModifier::Command
        ]);
        assert_eq!(config.mouse_button, MenuAnywhereMouseButton::RightClick);
    }

    #[test]
    fn test_menu_anywhere_config_is_enabled() {
        let disabled = MenuAnywhereConfig::default();
        assert!(!disabled.is_enabled());

        let enabled = MenuAnywhereConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(enabled.is_enabled());
    }

    #[test]
    fn test_menu_anywhere_required_modifier_flags_control() {
        let config = MenuAnywhereConfig {
            modifiers: vec![MenuAnywhereModifier::Control],
            ..Default::default()
        };
        assert_eq!(config.required_modifier_flags(), 0x0004_0000);
    }

    #[test]
    fn test_menu_anywhere_required_modifier_flags_option() {
        let config = MenuAnywhereConfig {
            modifiers: vec![MenuAnywhereModifier::Option],
            ..Default::default()
        };
        assert_eq!(config.required_modifier_flags(), 0x0008_0000);
    }

    #[test]
    fn test_menu_anywhere_required_modifier_flags_command() {
        let config = MenuAnywhereConfig {
            modifiers: vec![MenuAnywhereModifier::Command],
            ..Default::default()
        };
        assert_eq!(config.required_modifier_flags(), 0x0010_0000);
    }

    #[test]
    fn test_menu_anywhere_required_modifier_flags_shift() {
        let config = MenuAnywhereConfig {
            modifiers: vec![MenuAnywhereModifier::Shift],
            ..Default::default()
        };
        assert_eq!(config.required_modifier_flags(), 0x0002_0000);
    }

    #[test]
    fn test_menu_anywhere_required_modifier_flags_combined() {
        let config = MenuAnywhereConfig {
            modifiers: vec![MenuAnywhereModifier::Control, MenuAnywhereModifier::Command],
            ..Default::default()
        };
        assert_eq!(config.required_modifier_flags(), 0x0004_0000 | 0x0010_0000);
    }

    #[test]
    fn test_menu_anywhere_required_modifier_flags_empty() {
        let config = MenuAnywhereConfig {
            modifiers: vec![],
            ..Default::default()
        };
        assert_eq!(config.required_modifier_flags(), 0);
    }

    #[test]
    fn test_menu_anywhere_required_modifier_flags_all() {
        let config = MenuAnywhereConfig {
            modifiers: vec![
                MenuAnywhereModifier::Control,
                MenuAnywhereModifier::Option,
                MenuAnywhereModifier::Command,
                MenuAnywhereModifier::Shift,
            ],
            ..Default::default()
        };
        let expected = 0x0004_0000 | 0x0008_0000 | 0x0010_0000 | 0x0002_0000;
        assert_eq!(config.required_modifier_flags(), expected);
    }

    // ========================================================================
    // ProxyAudioConfig tests
    // ========================================================================

    #[test]
    fn test_proxy_audio_config_default() {
        let config = ProxyAudioConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.input.name, "Stache Virtual Input");
        assert_eq!(config.output.name, "Stache Virtual Output");
        assert_eq!(config.output.buffer_size, 256);
    }

    #[test]
    fn test_proxy_audio_config_is_enabled() {
        let disabled = ProxyAudioConfig::default();
        assert!(!disabled.is_enabled());

        let enabled = ProxyAudioConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(enabled.is_enabled());
    }

    // ========================================================================
    // WeatherConfig tests
    // ========================================================================

    #[test]
    fn test_weather_config_default() {
        let config = WeatherConfig::default();
        assert!(config.api_keys.is_empty());
        assert!(config.default_location.is_empty());
    }

    #[test]
    fn test_weather_config_is_enabled() {
        let disabled = WeatherConfig::default();
        assert!(!disabled.is_enabled());

        let enabled = WeatherConfig {
            api_keys: ".env".to_string(),
            ..Default::default()
        };
        assert!(enabled.is_enabled());
    }

    // ========================================================================
    // NoTunesConfig tests
    // ========================================================================

    #[test]
    fn test_notunes_config_default() {
        let config = NoTunesConfig::default();
        assert!(config.enabled);
        assert_eq!(config.target_app, TargetMusicApp::Tidal);
    }

    #[test]
    fn test_notunes_config_is_enabled() {
        let enabled = NoTunesConfig::default();
        assert!(enabled.is_enabled());

        let disabled = NoTunesConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(!disabled.is_enabled());
    }

    // ========================================================================
    // ShortcutCommands tests
    // ========================================================================

    #[test]
    fn test_shortcut_commands_display_single() {
        let cmd = ShortcutCommands::Single("stache reload".to_string());
        assert_eq!(cmd.commands_display(), "stache reload");
    }

    #[test]
    fn test_shortcut_commands_display_multiple() {
        let cmd = ShortcutCommands::Multiple(vec![
            "cmd1".to_string(),
            "cmd2".to_string(),
            "cmd3".to_string(),
        ]);
        assert_eq!(cmd.commands_display(), "[3 commands]");
    }

    #[test]
    fn test_shortcut_commands_get_commands_trims_whitespace() {
        let cmd = ShortcutCommands::Single("  stache reload  ".to_string());
        assert_eq!(cmd.get_commands(), vec!["stache reload"]);
    }

    #[test]
    fn test_shortcut_commands_multiple_filters_empty() {
        let cmd = ShortcutCommands::Multiple(vec![
            "cmd1".to_string(),
            "".to_string(),
            "  ".to_string(),
            "cmd2".to_string(),
        ]);
        assert_eq!(cmd.get_commands(), vec!["cmd1", "cmd2"]);
    }

    // ========================================================================
    // ConfigError tests
    // ========================================================================

    #[test]
    fn test_config_error_display_not_found() {
        let err = ConfigError::NotFound;
        let display = format!("{err}");
        assert!(display.contains("No configuration file found"));
    }

    #[test]
    fn test_config_error_display_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err = ConfigError::IoError(io_err);
        let display = format!("{err}");
        assert!(display.contains("Failed to read configuration file"));
        assert!(display.contains("access denied"));
    }

    #[test]
    fn test_config_error_display_parse_error() {
        let json_err = serde_json::from_str::<StacheConfig>("invalid json").unwrap_err();
        let err = ConfigError::ParseError(json_err);
        let display = format!("{err}");
        assert!(display.contains("Failed to parse configuration file"));
    }

    #[test]
    fn test_config_error_source_not_found() {
        let err = ConfigError::NotFound;
        assert!(err.source().is_none());
    }

    #[test]
    fn test_config_error_source_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = ConfigError::IoError(io_err);
        assert!(err.source().is_some());
    }

    #[test]
    fn test_config_error_source_parse_error() {
        let json_err =
            serde_json::from_str::<StacheConfig>("{}}").expect_err("Should fail to parse");
        let err = ConfigError::ParseError(json_err);
        assert!(err.source().is_some());
    }

    #[test]
    fn test_config_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "test");
        let config_err: ConfigError = io_err.into();
        assert!(matches!(config_err, ConfigError::IoError(_)));
    }

    #[test]
    fn test_config_error_from_json_error() {
        let json_err = serde_json::from_str::<StacheConfig>("not json").unwrap_err();
        let config_err: ConfigError = json_err.into();
        assert!(matches!(config_err, ConfigError::ParseError(_)));
    }

    // ========================================================================
    // AudioDeviceDependency tests
    // ========================================================================

    #[test]
    fn test_audio_device_dependency_default() {
        let dep = AudioDeviceDependency::default();
        assert!(dep.name.is_empty());
        assert_eq!(dep.strategy, MatchStrategy::Exact);
    }

    #[test]
    fn test_audio_device_dependency_serialization() {
        let dep = AudioDeviceDependency {
            name: "MiniFuse".to_string(),
            strategy: MatchStrategy::StartsWith,
        };
        let json = serde_json::to_string(&dep).unwrap();
        assert!(json.contains("MiniFuse"));
        assert!(json.contains("startsWith"));
    }

    // ========================================================================
    // AudioDevicePriority tests
    // ========================================================================

    #[test]
    fn test_audio_device_priority_default() {
        let priority = AudioDevicePriority::default();
        assert!(priority.name.is_empty());
        assert_eq!(priority.strategy, MatchStrategy::Exact);
        assert!(priority.depends_on.is_none());
    }

    #[test]
    fn test_audio_device_priority_with_dependency() {
        let json = r#"{
            "name": "External Speakers",
            "strategy": "exact",
            "dependsOn": {
                "name": "MiniFuse",
                "strategy": "startsWith"
            }
        }"#;

        let priority: AudioDevicePriority = serde_json::from_str(json).unwrap();
        assert_eq!(priority.name, "External Speakers");
        assert!(priority.depends_on.is_some());
        let dep = priority.depends_on.unwrap();
        assert_eq!(dep.name, "MiniFuse");
        assert_eq!(dep.strategy, MatchStrategy::StartsWith);
    }

    // ========================================================================
    // WallpaperMode tests
    // ========================================================================

    #[test]
    fn test_wallpaper_mode_serialization() {
        assert_eq!(
            serde_json::to_string(&WallpaperMode::Random).unwrap(),
            r#""random""#
        );
        assert_eq!(
            serde_json::to_string(&WallpaperMode::Sequential).unwrap(),
            r#""sequential""#
        );
    }

    // ========================================================================
    // MenuAnywhereMouseButton tests
    // ========================================================================

    #[test]
    fn test_menu_anywhere_mouse_button_default() {
        assert_eq!(
            MenuAnywhereMouseButton::default(),
            MenuAnywhereMouseButton::RightClick
        );
    }

    #[test]
    fn test_menu_anywhere_mouse_button_serialization() {
        assert_eq!(
            serde_json::to_string(&MenuAnywhereMouseButton::RightClick).unwrap(),
            r#""rightClick""#
        );
        assert_eq!(
            serde_json::to_string(&MenuAnywhereMouseButton::MiddleClick).unwrap(),
            r#""middleClick""#
        );
    }
}
