//! Tiling window manager configuration types.
//!
//! Core configuration types for the tiling window manager including layouts,
//! animations, floating window settings, and master layout configuration.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::borders::BordersConfig;
use super::gaps::{DimensionValue, GapsConfigValue};
use super::workspaces::{WindowRule, WorkspaceConfig};

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
    /// Exponential ease out (very fast start, slow end) - snappiest feel.
    EaseOutExpo,
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

/// Tiling window manager configuration.
///
/// Provides virtual workspace management with multiple layout modes,
/// configurable gaps, and window matching rules.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
pub struct TilingConfig {
    /// Whether the tiling window manager is enabled.
    /// Default: false
    pub enabled: bool,

    /// Default layout for workspaces that don't specify a layout.
    /// Default: "dwindle"
    pub default_layout: LayoutType,

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

impl Default for TilingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_layout: LayoutType::Dwindle,
            workspaces: Vec::new(),
            ignore: Vec::new(),
            animations: AnimationConfig::default(),
            gaps: GapsConfigValue::default(),
            floating: FloatingConfig::default(),
            master: MasterConfig::default(),
            borders: BordersConfig::default(),
        }
    }
}

impl TilingConfig {
    /// Returns whether the tiling window manager is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layout_type_default_is_floating() {
        assert_eq!(LayoutType::default(), LayoutType::Floating);
    }

    #[test]
    fn test_easing_type_default_is_ease_out() {
        assert_eq!(EasingType::default(), EasingType::EaseOut);
    }

    #[test]
    fn test_animation_config_default() {
        let config = AnimationConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.duration, 200);
        assert_eq!(config.easing, EasingType::EaseOut);
    }

    #[test]
    fn test_master_config_default() {
        let config = MasterConfig::default();
        assert_eq!(config.ratio, 60);
        assert_eq!(config.position, MasterPosition::Auto);
    }

    #[test]
    fn test_tiling_config_default() {
        let config = TilingConfig::default();
        assert!(!config.is_enabled());
        assert_eq!(config.default_layout, LayoutType::Dwindle);
        assert!(config.workspaces.is_empty());
    }

    #[test]
    fn test_default_layout_serialization() {
        let json = r#"{"enabled": true, "defaultLayout": "master"}"#;
        let config: TilingConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert_eq!(config.default_layout, LayoutType::Master);
    }
}
