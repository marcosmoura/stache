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
/// Contains settings for bar-specific features like weather and dimensions.
/// The bar dimensions are used by the tiling window manager to account for
/// the status bar when calculating window layouts on the main screen.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct BarConfig {
    /// Height of the status bar in pixels.
    /// Default: 28
    pub height: u16,

    /// Padding around the status bar in pixels.
    /// This is added to the height when calculating the top gap for tiling.
    /// Default: 12
    pub padding: u16,

    /// Weather status bar configuration.
    pub weather: WeatherConfig,
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            height: 28,
            padding: 12,
            weather: WeatherConfig::default(),
        }
    }
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

// ============================================================================
// Window Border Configuration
// ============================================================================

/// RGBA color representation for border rendering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    /// Red channel (0.0 - 1.0).
    pub r: f64,
    /// Green channel (0.0 - 1.0).
    pub g: f64,
    /// Blue channel (0.0 - 1.0).
    pub b: f64,
    /// Alpha channel (0.0 - 1.0).
    pub a: f64,
}

impl Rgba {
    /// Creates a new RGBA color.
    #[must_use]
    pub const fn new(r: f64, g: f64, b: f64, a: f64) -> Self { Self { r, g, b, a } }

    /// Creates an opaque black color.
    #[must_use]
    pub const fn black() -> Self { Self::new(0.0, 0.0, 0.0, 1.0) }

    /// Creates an opaque white color.
    #[must_use]
    pub const fn white() -> Self { Self::new(1.0, 1.0, 1.0, 1.0) }
}

impl Default for Rgba {
    fn default() -> Self { Self::black() }
}

/// Parses a color string to RGBA.
///
/// Supports the following formats:
/// - `#RGB` - 3-digit hex (e.g., "#F00" for red)
/// - `#RGBA` - 4-digit hex with alpha (e.g., "#F00F" for opaque red)
/// - `#RRGGBB` - 6-digit hex (e.g., "#FF0000" for red)
/// - `#RRGGBBAA` - 8-digit hex with alpha (e.g., "#FF0000FF" for opaque red)
/// - `rgba(r, g, b, a)` - CSS rgba format (e.g., "rgba(255, 0, 0, 0.5)")
///
/// The `#` prefix is optional for hex colors.
///
/// # Errors
///
/// Returns an error string if the color format is invalid.
pub fn parse_color(color: &str) -> Result<Rgba, String> {
    let trimmed = color.trim();
    if trimmed.starts_with("rgba(") || trimmed.starts_with("rgb(") {
        parse_rgba_color(trimmed)
    } else {
        parse_hex_color(trimmed)
    }
}

/// Parses an `rgba()` or `rgb()` CSS color string to RGBA.
///
/// Supports the following formats:
/// - `rgb(r, g, b)` - CSS rgb format with 0-255 values
/// - `rgba(r, g, b, a)` - CSS rgba format with 0-255 values and alpha 0.0-1.0
///
/// # Examples
///
/// - `rgba(255, 0, 0, 0.5)` - Semi-transparent red
/// - `rgba(137, 180, 250, 0.2)` - Catppuccin blue with 20% opacity
/// - `rgb(255, 255, 255)` - White
///
/// # Errors
///
/// Returns an error string if the format is invalid.
pub fn parse_rgba_color(rgba: &str) -> Result<Rgba, String> {
    let trimmed = rgba.trim();

    // Check for rgb() or rgba() prefix
    let (inner, has_alpha) = if let Some(inner) = trimmed.strip_prefix("rgba(") {
        (
            inner.strip_suffix(')').ok_or("Missing closing parenthesis")?,
            true,
        )
    } else if let Some(inner) = trimmed.strip_prefix("rgb(") {
        (
            inner.strip_suffix(')').ok_or("Missing closing parenthesis")?,
            false,
        )
    } else {
        return Err("Color must start with 'rgb(' or 'rgba('".to_string());
    };

    // Split by comma and parse values
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();

    let expected_parts = if has_alpha { 4 } else { 3 };
    if parts.len() != expected_parts {
        return Err(format!(
            "Expected {} values for {}, got {}",
            expected_parts,
            if has_alpha { "rgba()" } else { "rgb()" },
            parts.len()
        ));
    }

    // Parse RGB values (0-255)
    let r: u8 = parts[0].parse().map_err(|_| format!("Invalid red value: {}", parts[0]))?;
    let g: u8 = parts[1].parse().map_err(|_| format!("Invalid green value: {}", parts[1]))?;
    let b: u8 = parts[2].parse().map_err(|_| format!("Invalid blue value: {}", parts[2]))?;

    // Parse alpha (0.0-1.0 for rgba, default 1.0 for rgb)
    let a: f64 = if has_alpha {
        parts[3].parse().map_err(|_| format!("Invalid alpha value: {}", parts[3]))?
    } else {
        1.0
    };

    // Validate alpha range
    if !(0.0..=1.0).contains(&a) {
        return Err(format!("Alpha value must be between 0.0 and 1.0, got {a}"));
    }

    Ok(Rgba {
        r: f64::from(r) / 255.0,
        g: f64::from(g) / 255.0,
        b: f64::from(b) / 255.0,
        a,
    })
}

/// Parses a hex color string to RGBA.
///
/// Supports the following formats:
/// - `#RGB` - 3-digit hex (e.g., "#F00" for red)
/// - `#RGBA` - 4-digit hex with alpha (e.g., "#F00F" for opaque red)
/// - `#RRGGBB` - 6-digit hex (e.g., "#FF0000" for red)
/// - `#RRGGBBAA` - 8-digit hex with alpha (e.g., "#FF0000FF" for opaque red)
///
/// The `#` prefix is optional.
///
/// # Errors
///
/// Returns an error string if the hex color is invalid.
pub fn parse_hex_color(hex: &str) -> Result<Rgba, String> {
    let hex = hex.trim().trim_start_matches('#');

    let (r, g, b, a) = match hex.len() {
        3 => {
            // RGB format
            let r = u8::from_str_radix(&hex[0..1], 16).map_err(|e| e.to_string())?;
            let g = u8::from_str_radix(&hex[1..2], 16).map_err(|e| e.to_string())?;
            let b = u8::from_str_radix(&hex[2..3], 16).map_err(|e| e.to_string())?;
            // Expand 4-bit to 8-bit by repeating (0xF -> 0xFF)
            (r * 17, g * 17, b * 17, 255)
        }
        4 => {
            // RGBA format
            let r = u8::from_str_radix(&hex[0..1], 16).map_err(|e| e.to_string())?;
            let g = u8::from_str_radix(&hex[1..2], 16).map_err(|e| e.to_string())?;
            let b = u8::from_str_radix(&hex[2..3], 16).map_err(|e| e.to_string())?;
            let a = u8::from_str_radix(&hex[3..4], 16).map_err(|e| e.to_string())?;
            (r * 17, g * 17, b * 17, a * 17)
        }
        6 => {
            // RRGGBB format
            let r = u8::from_str_radix(&hex[0..2], 16).map_err(|e| e.to_string())?;
            let g = u8::from_str_radix(&hex[2..4], 16).map_err(|e| e.to_string())?;
            let b = u8::from_str_radix(&hex[4..6], 16).map_err(|e| e.to_string())?;
            (r, g, b, 255)
        }
        8 => {
            // RRGGBBAA format
            let r = u8::from_str_radix(&hex[0..2], 16).map_err(|e| e.to_string())?;
            let g = u8::from_str_radix(&hex[2..4], 16).map_err(|e| e.to_string())?;
            let b = u8::from_str_radix(&hex[4..6], 16).map_err(|e| e.to_string())?;
            let a = u8::from_str_radix(&hex[6..8], 16).map_err(|e| e.to_string())?;
            (r, g, b, a)
        }
        _ => {
            return Err(format!(
                "Invalid hex color length: expected 3, 4, 6, or 8 characters, got {}",
                hex.len()
            ));
        }
    };

    Ok(Rgba {
        r: f64::from(r) / 255.0,
        g: f64::from(g) / 255.0,
        b: f64::from(b) / 255.0,
        a: f64::from(a) / 255.0,
    })
}

/// Gradient color configuration.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GradientConfig {
    /// Starting color (hex string).
    pub from: String,
    /// Ending color (hex string).
    pub to: String,
    /// Angle in degrees (0 = left to right, 90 = bottom to top).
    /// Default: 90
    #[serde(default = "default_gradient_angle")]
    pub angle: f64,
}

/// Default gradient angle (90 degrees = bottom to top).
const fn default_gradient_angle() -> f64 { 90.0 }

impl Default for GradientConfig {
    fn default() -> Self {
        Self {
            from: "#b4befe".to_string(),
            to: "#cba6f7".to_string(),
            angle: default_gradient_angle(),
        }
    }
}

/// Border state configuration - either disabled or with specific settings.
///
/// Can be:
/// - `false` to disable borders for this state
/// - An object with `width` and either `color` (solid), `gradient`, or `glow`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum BorderStateConfig {
    /// Disabled - don't draw border for this state.
    /// Use `false` in config to disable.
    Disabled(bool),

    /// Enabled with solid color.
    SolidColor {
        /// Border width in pixels.
        width: u32,
        /// Solid color (hex string).
        color: String,
    },

    /// Enabled with gradient color.
    GradientColor {
        /// Border width in pixels.
        width: u32,
        /// Gradient configuration.
        gradient: GradientConfig,
    },

    /// Enabled with glow effect.
    GlowColor {
        /// Border width in pixels.
        width: u32,
        /// Glow color (hex string).
        glow: String,
    },
}

impl BorderStateConfig {
    /// Returns whether this border state is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { !matches!(self, Self::Disabled(false)) }

    /// Returns the border width, or None if disabled.
    #[must_use]
    pub const fn width(&self) -> Option<u32> {
        match self {
            Self::Disabled(_) => None,
            Self::SolidColor { width, .. }
            | Self::GradientColor { width, .. }
            | Self::GlowColor { width, .. } => Some(*width),
        }
    }

    /// Returns true if this is a gradient color.
    #[must_use]
    pub const fn is_gradient(&self) -> bool { matches!(self, Self::GradientColor { .. }) }

    /// Returns true if this is a glow effect.
    #[must_use]
    pub const fn is_glow(&self) -> bool { matches!(self, Self::GlowColor { .. }) }

    /// Returns the primary color string (for solid, glow, or gradient's from color).
    #[must_use]
    pub fn color(&self) -> Option<String> {
        match self {
            Self::Disabled(_) => None,
            Self::SolidColor { color, .. } => Some(color.clone()),
            Self::GradientColor { gradient, .. } => Some(gradient.from.clone()),
            Self::GlowColor { glow, .. } => Some(glow.clone()),
        }
    }

    /// Returns the solid color as RGBA.
    ///
    /// # Errors
    ///
    /// Returns an error if the hex color string is invalid or if this is not a solid color.
    pub fn to_rgba(&self) -> Result<Rgba, String> {
        match self {
            Self::Disabled(_) => Err("Border state is disabled".to_string()),
            Self::SolidColor { color, .. } | Self::GlowColor { glow: color, .. } => {
                parse_hex_color(color)
            }
            Self::GradientColor { gradient, .. } => parse_hex_color(&gradient.from),
        }
    }

    /// Returns gradient colors and angle.
    ///
    /// # Errors
    ///
    /// Returns an error if the hex color strings are invalid or if this is disabled.
    pub fn to_gradient_rgba(&self) -> Result<(Rgba, Rgba, f64), String> {
        match self {
            Self::Disabled(_) => Err("Border state is disabled".to_string()),
            Self::SolidColor { color, .. } | Self::GlowColor { glow: color, .. } => {
                let rgba = parse_hex_color(color)?;
                Ok((rgba, rgba, 0.0))
            }
            Self::GradientColor { gradient, .. } => {
                let from = parse_hex_color(&gradient.from)?;
                let to = parse_hex_color(&gradient.to)?;
                Ok((from, to, gradient.angle))
            }
        }
    }

    /// Creates a default focused border state.
    #[must_use]
    pub fn default_focused() -> Self {
        Self::SolidColor {
            width: 4,
            color: "#b4befe".to_string(), // Catppuccin Mocha lavender
        }
    }

    /// Creates a default unfocused border state.
    #[must_use]
    pub fn default_unfocused() -> Self {
        Self::SolidColor {
            width: 4,
            color: "#6c7086".to_string(), // Catppuccin Mocha overlay0
        }
    }

    /// Creates a default monocle border state.
    #[must_use]
    pub fn default_monocle() -> Self {
        Self::SolidColor {
            width: 4,
            color: "#cba6f7".to_string(), // Catppuccin Mocha mauve
        }
    }

    /// Creates a default floating border state.
    #[must_use]
    pub fn default_floating() -> Self {
        Self::SolidColor {
            width: 4,
            color: "#94e2d5".to_string(), // Catppuccin Mocha teal
        }
    }
}

/// Window border configuration.
///
/// Borders are rendered as transparent overlay windows that frame managed windows.
/// They provide visual feedback for focus state, layout mode, and floating status.
///
/// Each border state (focused, unfocused, monocle, floating) can be:
/// - `false` to disable borders for that state
/// - An object with `width` and either `color` (solid) or `gradient`
///
/// # Example
///
/// ```jsonc
/// {
///   "borders": {
///     "enabled": true,
///     "focused": { "width": 4, "color": "#89b4fa" },
///     "unfocused": false,
///     "monocle": { "width": 4, "color": "#cba6f7" },
///     "floating": {
///       "width": 4,
///       "gradient": { "from": "#89b4fa", "to": "#a6e3a1", "angle": 180 }
///     }
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct BordersConfig {
    /// Whether window borders are enabled globally.
    /// Default: false
    pub enabled: bool,

    /// Border style: "round" or "square".
    /// Default: "round"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,

    /// Enable HiDPI/Retina support for borders.
    /// Default: true
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidpi: Option<bool>,

    /// Border configuration for focused windows.
    pub focused: BorderStateConfig,

    /// Border configuration for unfocused windows.
    /// Set to `false` to hide borders on unfocused windows.
    pub unfocused: BorderStateConfig,

    /// Border configuration for windows in monocle layout.
    pub monocle: BorderStateConfig,

    /// Border configuration for floating windows.
    pub floating: BorderStateConfig,

    /// Rules for windows that should not have borders.
    /// These rules are checked in addition to the global tiling ignore rules.
    #[serde(default)]
    pub ignore: Vec<WindowRule>,
}

impl Default for BordersConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            style: None, // Defaults to "round" in JankyBorders
            hidpi: None, // Defaults to true in JankyBorders
            focused: BorderStateConfig::default_focused(),
            unfocused: BorderStateConfig::default_unfocused(),
            monocle: BorderStateConfig::default_monocle(),
            floating: BorderStateConfig::default_floating(),
            ignore: Vec::new(),
        }
    }
}

impl BordersConfig {
    /// Returns whether window borders are enabled globally.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }

    /// Returns the border state config for a given state.
    #[must_use]
    #[allow(clippy::match_same_arms)] // Intentional: default fallback to unfocused
    pub fn get_state_config(&self, state: &str) -> &BorderStateConfig {
        match state {
            "focused" => &self.focused,
            "unfocused" => &self.unfocused,
            "monocle" => &self.monocle,
            "floating" => &self.floating,
            _ => &self.unfocused,
        }
    }
}

// ============================================================================
// Legacy BorderColor type (kept for backward compatibility in rendering)
// ============================================================================

/// Border color definition (internal use).
///
/// This is used internally by the `JankyBorders` integration. Use `BorderStateConfig` for configuration.
#[derive(Debug, Clone)]
pub enum BorderColor {
    /// A solid color.
    Solid(String),
    /// A gradient with start and end colors and optional angle.
    Gradient {
        from: String,
        to: String,
        angle: Option<f64>,
    },
    /// A glow effect with a color.
    Glow(String),
}

impl BorderColor {
    /// Creates from a `BorderStateConfig`.
    #[must_use]
    pub fn from_state_config(config: &BorderStateConfig) -> Option<Self> {
        match config {
            BorderStateConfig::Disabled(_) => None,
            BorderStateConfig::SolidColor { color, .. } => Some(Self::Solid(color.clone())),
            BorderStateConfig::GradientColor { gradient, .. } => Some(Self::Gradient {
                from: gradient.from.clone(),
                to: gradient.to.clone(),
                angle: Some(gradient.angle),
            }),
            BorderStateConfig::GlowColor { glow, .. } => Some(Self::Glow(glow.clone())),
        }
    }

    /// Returns the solid color as RGBA.
    ///
    /// For gradients, returns the `from` color.
    /// For glow, returns the glow color.
    ///
    /// # Errors
    ///
    /// Returns an error if the hex color string is invalid.
    pub fn to_rgba(&self) -> Result<Rgba, String> {
        match self {
            Self::Solid(hex) | Self::Glow(hex) => parse_hex_color(hex),
            Self::Gradient { from, .. } => parse_hex_color(from),
        }
    }

    /// Returns gradient colors and angle.
    ///
    /// For solid colors, returns the same color for both start and end.
    /// For glow, returns the glow color for both start and end.
    ///
    /// # Errors
    ///
    /// Returns an error if the hex color strings are invalid.
    pub fn to_gradient_rgba(&self) -> Result<(Rgba, Rgba, f64), String> {
        match self {
            Self::Solid(hex) | Self::Glow(hex) => {
                let color = parse_hex_color(hex)?;
                Ok((color, color, 0.0))
            }
            Self::Gradient { from, to, angle } => {
                let from_color = parse_hex_color(from)?;
                let to_color = parse_hex_color(to)?;
                Ok((from_color, to_color, angle.unwrap_or(135.0)))
            }
        }
    }

    /// Returns true if this is a gradient color.
    #[must_use]
    pub const fn is_gradient(&self) -> bool { matches!(self, Self::Gradient { .. }) }

    /// Returns true if this is a glow color.
    #[must_use]
    pub const fn is_glow(&self) -> bool { matches!(self, Self::Glow(_)) }
}

/// Window matching rule for workspace assignment.
///
/// All specified properties must match (AND logic).
/// At least one property must be specified.
///
/// # Performance
///
/// Call [`WindowRule::prepare()`] after loading rules from config to pre-compute
/// lowercase versions of string fields. This avoids repeated `to_lowercase()` calls
/// during window matching.
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

    // Cached lowercase versions for fast matching (computed by prepare())
    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) app_id_lower: Option<String>,

    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) title_lower: Option<String>,

    #[serde(skip)]
    #[schemars(skip)]
    pub(crate) app_name_lower: Option<String>,
}

impl WindowRule {
    /// Returns true if the rule has at least one matching criterion.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.app_id.is_some() || self.title.is_some() || self.app_name.is_some()
    }

    /// Pre-computes lowercase versions of string fields for faster matching.
    ///
    /// Call this after loading rules from config. The lowercase values are cached
    /// and reused by [`crate::tiling::rules::matches_window()`].
    pub fn prepare(&mut self) {
        self.app_id_lower = self.app_id.as_ref().map(|s| s.to_ascii_lowercase());
        self.title_lower = self.title.as_ref().map(|s| s.to_lowercase());
        self.app_name_lower = self.app_name.as_ref().map(|s| s.to_lowercase());
    }

    /// Returns the cached lowercase `app_id`, or the original if not cached.
    #[must_use]
    pub fn app_id_lowercase(&self) -> Option<&str> {
        self.app_id_lower.as_deref().or(self.app_id.as_deref())
    }

    /// Returns the cached lowercase title, or the original if not cached.
    #[must_use]
    pub fn title_lowercase(&self) -> Option<&str> {
        self.title_lower.as_deref().or(self.title.as_deref())
    }

    /// Returns the cached lowercase `app_name`, or the original if not cached.
    #[must_use]
    pub fn app_name_lowercase(&self) -> Option<&str> {
        self.app_name_lower.as_deref().or(self.app_name.as_deref())
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

    /// Window border configuration.
    /// Borders provide visual feedback for focus state and layout mode.
    pub borders: BordersConfig,
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
            let mut config: StacheConfig = serde_json::from_reader(reader)?;
            // Pre-compute cached values for faster runtime operations
            config.prepare();
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

    // ========================================================================
    // Rgba tests
    // ========================================================================

    #[test]
    fn test_rgba_new() {
        let color = Rgba::new(0.5, 0.25, 0.75, 1.0);
        assert!((color.r - 0.5).abs() < f64::EPSILON);
        assert!((color.g - 0.25).abs() < f64::EPSILON);
        assert!((color.b - 0.75).abs() < f64::EPSILON);
        assert!((color.a - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_rgba_black() {
        let color = Rgba::black();
        assert!((color.r - 0.0).abs() < f64::EPSILON);
        assert!((color.g - 0.0).abs() < f64::EPSILON);
        assert!((color.b - 0.0).abs() < f64::EPSILON);
        assert!((color.a - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_rgba_white() {
        let color = Rgba::white();
        assert!((color.r - 1.0).abs() < f64::EPSILON);
        assert!((color.g - 1.0).abs() < f64::EPSILON);
        assert!((color.b - 1.0).abs() < f64::EPSILON);
        assert!((color.a - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_rgba_default_is_black() {
        let color = Rgba::default();
        assert_eq!(color, Rgba::black());
    }

    // ========================================================================
    // parse_hex_color tests
    // ========================================================================

    #[test]
    fn test_parse_hex_color_6_digit() {
        let color = parse_hex_color("#FF0000").unwrap();
        assert!((color.r - 1.0).abs() < f64::EPSILON);
        assert!((color.g - 0.0).abs() < f64::EPSILON);
        assert!((color.b - 0.0).abs() < f64::EPSILON);
        assert!((color.a - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_hex_color_6_digit_no_hash() {
        let color = parse_hex_color("00FF00").unwrap();
        assert!((color.r - 0.0).abs() < f64::EPSILON);
        assert!((color.g - 1.0).abs() < f64::EPSILON);
        assert!((color.b - 0.0).abs() < f64::EPSILON);
        assert!((color.a - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_hex_color_8_digit_with_alpha() {
        let color = parse_hex_color("#0000FF80").unwrap();
        assert!((color.r - 0.0).abs() < f64::EPSILON);
        assert!((color.g - 0.0).abs() < f64::EPSILON);
        assert!((color.b - 1.0).abs() < f64::EPSILON);
        // 0x80 = 128, 128/255 ≈ 0.502
        assert!((color.a - 128.0 / 255.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_hex_color_3_digit() {
        let color = parse_hex_color("#F00").unwrap();
        assert!((color.r - 1.0).abs() < f64::EPSILON);
        assert!((color.g - 0.0).abs() < f64::EPSILON);
        assert!((color.b - 0.0).abs() < f64::EPSILON);
        assert!((color.a - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_hex_color_4_digit_with_alpha() {
        let color = parse_hex_color("#F008").unwrap();
        assert!((color.r - 1.0).abs() < f64::EPSILON);
        assert!((color.g - 0.0).abs() < f64::EPSILON);
        assert!((color.b - 0.0).abs() < f64::EPSILON);
        // 0x8 expanded to 0x88 = 136, 136/255 ≈ 0.533
        assert!((color.a - 136.0 / 255.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_hex_color_catppuccin_lavender() {
        // Catppuccin Mocha lavender: #b4befe
        let color = parse_hex_color("#b4befe").unwrap();
        assert!((color.r - 180.0 / 255.0).abs() < 0.001);
        assert!((color.g - 190.0 / 255.0).abs() < 0.001);
        assert!((color.b - 254.0 / 255.0).abs() < 0.001);
        assert!((color.a - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_hex_color_invalid_length() {
        let result = parse_hex_color("#12345");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid hex color length"));
    }

    #[test]
    fn test_parse_hex_color_invalid_characters() {
        let result = parse_hex_color("#GGGGGG");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_hex_color_trims_whitespace() {
        let color = parse_hex_color("  #FF0000  ").unwrap();
        assert!((color.r - 1.0).abs() < f64::EPSILON);
    }

    // ========================================================================
    // parse_rgba_color tests
    // ========================================================================

    #[test]
    fn test_parse_rgba_color_basic() {
        let color = parse_rgba_color("rgba(255, 0, 0, 1.0)").unwrap();
        assert!((color.r - 1.0).abs() < f64::EPSILON);
        assert!((color.g - 0.0).abs() < f64::EPSILON);
        assert!((color.b - 0.0).abs() < f64::EPSILON);
        assert!((color.a - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_rgba_color_with_alpha() {
        let color = parse_rgba_color("rgba(137, 180, 250, 0.2)").unwrap();
        assert!((color.r - 137.0 / 255.0).abs() < 0.001);
        assert!((color.g - 180.0 / 255.0).abs() < 0.001);
        assert!((color.b - 250.0 / 255.0).abs() < 0.001);
        assert!((color.a - 0.2).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_rgba_color_no_spaces() {
        let color = parse_rgba_color("rgba(255,128,64,0.5)").unwrap();
        assert!((color.r - 1.0).abs() < f64::EPSILON);
        assert!((color.g - 128.0 / 255.0).abs() < 0.001);
        assert!((color.b - 64.0 / 255.0).abs() < 0.001);
        assert!((color.a - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_rgb_color() {
        let color = parse_rgba_color("rgb(255, 255, 255)").unwrap();
        assert!((color.r - 1.0).abs() < f64::EPSILON);
        assert!((color.g - 1.0).abs() < f64::EPSILON);
        assert!((color.b - 1.0).abs() < f64::EPSILON);
        assert!((color.a - 1.0).abs() < f64::EPSILON); // Default alpha
    }

    #[test]
    fn test_parse_rgba_color_zero_alpha() {
        let color = parse_rgba_color("rgba(255, 0, 0, 0.0)").unwrap();
        assert!((color.a - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_rgba_color_invalid_format() {
        assert!(parse_rgba_color("rgb(255, 0)").is_err());
        assert!(parse_rgba_color("rgba(255, 0, 0)").is_err());
        assert!(parse_rgba_color("255, 0, 0, 1.0").is_err());
    }

    #[test]
    fn test_parse_rgba_color_invalid_alpha() {
        assert!(parse_rgba_color("rgba(255, 0, 0, 1.5)").is_err());
        assert!(parse_rgba_color("rgba(255, 0, 0, -0.5)").is_err());
    }

    #[test]
    fn test_parse_rgba_color_trims_whitespace() {
        let color = parse_rgba_color("  rgba(255, 0, 0, 1.0)  ").unwrap();
        assert!((color.r - 1.0).abs() < f64::EPSILON);
    }

    // ========================================================================
    // parse_color tests (unified parser)
    // ========================================================================

    #[test]
    fn test_parse_color_hex() {
        let color = parse_color("#FF0000").unwrap();
        assert!((color.r - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_color_rgba() {
        let color = parse_color("rgba(255, 0, 0, 0.5)").unwrap();
        assert!((color.r - 1.0).abs() < f64::EPSILON);
        assert!((color.a - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_color_rgb() {
        let color = parse_color("rgb(0, 255, 0)").unwrap();
        assert!((color.g - 1.0).abs() < f64::EPSILON);
    }

    // ========================================================================
    // BorderColor tests
    // ========================================================================

    #[test]
    fn test_border_color_solid() {
        let color = BorderColor::Solid("#b4befe".to_string());
        assert!(matches!(color, BorderColor::Solid(ref s) if s == "#b4befe"));
        assert!(!color.is_gradient());
    }

    #[test]
    fn test_border_color_solid_to_rgba() {
        let color = BorderColor::Solid("#FF0000".to_string());
        let rgba = color.to_rgba().unwrap();
        assert!((rgba.r - 1.0).abs() < f64::EPSILON);
        assert!((rgba.g - 0.0).abs() < f64::EPSILON);
        assert!((rgba.b - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_border_color_gradient_to_rgba() {
        let color = BorderColor::Gradient {
            from: "#FF0000".to_string(),
            to: "#00FF00".to_string(),
            angle: Some(45.0),
        };
        let rgba = color.to_rgba().unwrap();
        // to_rgba returns the start color
        assert!((rgba.r - 1.0).abs() < f64::EPSILON);
        assert!((rgba.g - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_border_color_gradient_to_gradient_rgba() {
        let color = BorderColor::Gradient {
            from: "#FF0000".to_string(),
            to: "#00FF00".to_string(),
            angle: Some(45.0),
        };
        let (from, to, angle) = color.to_gradient_rgba().unwrap();
        assert!((from.r - 1.0).abs() < f64::EPSILON);
        assert!((to.g - 1.0).abs() < f64::EPSILON);
        assert!((angle - 45.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_border_color_solid_to_gradient_rgba() {
        let color = BorderColor::Solid("#FF0000".to_string());
        let (from, to, angle) = color.to_gradient_rgba().unwrap();
        // Solid returns same color for both
        assert_eq!(from, to);
        assert!((angle - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_border_color_is_gradient() {
        let solid = BorderColor::Solid("#FF0000".to_string());
        assert!(!solid.is_gradient());

        let gradient = BorderColor::Gradient {
            from: "#FF0000".to_string(),
            to: "#00FF00".to_string(),
            angle: Some(90.0),
        };
        assert!(gradient.is_gradient());
    }
    // ========================================================================
    // BorderStateConfig tests
    // ========================================================================

    #[test]
    fn test_border_state_config_disabled() {
        let config = BorderStateConfig::Disabled(false);
        assert!(!config.is_enabled());
        assert!(config.width().is_none());
        assert!(!config.is_gradient());
    }

    #[test]
    fn test_border_state_config_solid() {
        let config = BorderStateConfig::SolidColor {
            width: 4,
            color: "#FF0000".to_string(),
        };
        assert!(config.is_enabled());
        assert_eq!(config.width(), Some(4));
        assert!(!config.is_gradient());
        let rgba = config.to_rgba().unwrap();
        assert!((rgba.r - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_border_state_config_gradient() {
        let config = BorderStateConfig::GradientColor {
            width: 6,
            gradient: GradientConfig {
                from: "#FF0000".to_string(),
                to: "#00FF00".to_string(),
                angle: 45.0,
            },
        };
        assert!(config.is_enabled());
        assert_eq!(config.width(), Some(6));
        assert!(config.is_gradient());
        let (from, to, angle) = config.to_gradient_rgba().unwrap();
        assert!((from.r - 1.0).abs() < f64::EPSILON);
        assert!((to.g - 1.0).abs() < f64::EPSILON);
        assert!((angle - 45.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_border_state_config_defaults() {
        let focused = BorderStateConfig::default_focused();
        assert!(focused.is_enabled());
        assert_eq!(focused.width(), Some(4));

        let unfocused = BorderStateConfig::default_unfocused();
        assert!(unfocused.is_enabled());

        let monocle = BorderStateConfig::default_monocle();
        assert!(monocle.is_enabled());

        let floating = BorderStateConfig::default_floating();
        assert!(floating.is_enabled());
    }

    // ========================================================================
    // BordersConfig tests
    // ========================================================================

    #[test]
    fn test_borders_config_default() {
        let config = BordersConfig::default();
        assert!(!config.enabled);
        assert!(config.focused.is_enabled());
        assert!(config.unfocused.is_enabled());
        assert!(config.monocle.is_enabled());
        assert!(config.floating.is_enabled());
        assert!(config.ignore.is_empty());
    }

    #[test]
    fn test_borders_config_is_enabled() {
        let disabled = BordersConfig::default();
        assert!(!disabled.is_enabled());

        let enabled = BordersConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(enabled.is_enabled());
    }

    #[test]
    fn test_borders_config_get_state_config() {
        let config = BordersConfig::default();
        assert!(config.get_state_config("focused").is_enabled());
        assert!(config.get_state_config("unfocused").is_enabled());
        assert!(config.get_state_config("monocle").is_enabled());
        assert!(config.get_state_config("floating").is_enabled());
        // Unknown state falls back to unfocused
        assert!(config.get_state_config("unknown").is_enabled());
    }

    #[test]
    fn test_borders_config_serialization() {
        let config = BordersConfig {
            enabled: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("true"));
        assert!(json.contains("focused"));
        assert!(json.contains("unfocused"));
    }

    #[test]
    fn test_borders_config_deserialization_with_disabled_state() {
        let json = r##"{
            "enabled": true,
            "focused": { "width": 4, "color": "#89b4fa" },
            "unfocused": false,
        }"##;
        let config: BordersConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert!(config.focused.is_enabled());
        assert!(!config.unfocused.is_enabled());
    }

    #[test]
    fn test_borders_config_deserialization_with_gradient() {
        let json = r##"{
            "enabled": true,
            "floating": {
                "width": 4,
                "gradient": { "from": "#89b4fa", "to": "#a6e3a1", "angle": 180 }
            }
        }"##;
        let config: BordersConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert!(config.floating.is_gradient());
        assert_eq!(config.floating.width(), Some(4));
    }

    #[test]
    fn test_borders_config_with_ignore_rules() {
        let json = r#"{
            "enabled": true,
            "ignore": [
                {"app-id": "com.apple.finder"},
                {"app-name": "Spotlight"}
            ]
        }"#;
        let config: BordersConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.ignore.len(), 2);
        assert_eq!(config.ignore[0].app_id, Some("com.apple.finder".to_string()));
        assert_eq!(config.ignore[1].app_name, Some("Spotlight".to_string()));
    }

    // ========================================================================
    // TilingConfig with borders tests
    // ========================================================================

    #[test]
    fn test_tiling_config_has_borders() {
        let config = TilingConfig::default();
        assert!(!config.borders.enabled);
        assert!(config.borders.focused.is_enabled());
    }

    #[test]
    fn test_tiling_config_borders_deserialization() {
        let json = r##"{
            "enabled": true,
            "borders": {
                "enabled": true,
                "focused": { "width": 6, "color": "#FF0000" }
            }
        }"##;
        let config: TilingConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert!(config.borders.enabled);
        assert_eq!(config.borders.focused.width(), Some(6));
    }
}
