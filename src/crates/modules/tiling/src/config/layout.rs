//! Layout names, animation and master-layout settings.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

/// Layout type for workspaces; `dwindle` is the single authoritative default.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Type,
)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutType {
    /// Binary space partitioning.
    #[default]
    Dwindle,
    /// Split based on screen orientation.
    Split,
    /// Windows arranged vertically.
    SplitVertical,
    /// Windows arranged horizontally.
    SplitHorizontal,
    /// All windows maximized and stacked.
    Monocle,
    /// One master window with a stack.
    Master,
    /// Windows arranged in a grid.
    Grid,
    /// Windows move and resize freely.
    Floating,
}

impl LayoutType {
    /// The kebab-case name used by configuration, CLI and state projections.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dwindle => "dwindle",
            Self::Split => "split",
            Self::SplitVertical => "split-vertical",
            Self::SplitHorizontal => "split-horizontal",
            Self::Monocle => "monocle",
            Self::Master => "master",
            Self::Grid => "grid",
            Self::Floating => "floating",
        }
    }
}

/// Easing function for animations.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Type,
)]
#[serde(rename_all = "kebab-case")]
pub enum EasingType {
    /// Linear interpolation.
    Linear,
    /// Slow start.
    EaseIn,
    /// Slow end.
    #[default]
    EaseOut,
    /// Slow start and end.
    EaseInOut,
    /// Exponential ease out: very fast start, slow end.
    EaseOutExpo,
    /// Spring physics.
    Spring,
}

/// Animation configuration for window transitions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct AnimationConfig {
    /// Whether animations are enabled.
    pub enabled: bool,
    /// Animation duration in milliseconds for large movements.
    pub duration: u32,
    /// Easing function for animations.
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

/// Position of the master window in the master layout.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Type,
)]
#[serde(rename_all = "kebab-case")]
pub enum MasterPosition {
    /// Master window on the left.
    Left,
    /// Master window on the right.
    Right,
    /// Master window on top.
    Top,
    /// Master window on the bottom.
    Bottom,
    /// Left on landscape displays, top on portrait displays.
    #[default]
    Auto,
}

/// Master layout configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct MasterConfig {
    /// Master window size as a percentage, 0 to 100.
    pub ratio: u32,
    /// Position of the master window.
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

impl MasterConfig {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.ratio > 100 {
            return Err(format!(
                "master.ratio must be between 0 and 100, got {}",
                self.ratio
            ));
        }
        Ok(())
    }
}
