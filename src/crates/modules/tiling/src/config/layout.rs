//! Layout names, animation and master-layout settings.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

/// Layout type for workspaces; `dwindle` is the single authoritative default.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Type,
)]
#[serde(rename_all = "snake_case")]
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
    /// The `snake_case` name used by configuration, CLI and state projections.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dwindle => "dwindle",
            Self::Split => "split",
            Self::SplitVertical => "split_vertical",
            Self::SplitHorizontal => "split_horizontal",
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
#[serde(rename_all = "snake_case")]
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
#[serde(default, rename_all = "snake_case")]
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
#[serde(rename_all = "snake_case")]
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
#[serde(default, rename_all = "snake_case")]
pub struct MasterConfig {
    /// Master window size as a percentage, 0 to 100.
    pub ratio: u32,
    /// Position of the master window.
    pub position: MasterPosition,
}

impl Default for MasterConfig {
    fn default() -> Self {
        Self {
            ratio: default_master_ratio(),
            position: MasterPosition::Auto,
        }
    }
}

fn validate_ratio(field: &str, ratio: u32) -> Result<(), String> {
    if ratio > 100 {
        return Err(format!("{field} must be between 0 and 100, got {ratio}"));
    }
    Ok(())
}

const fn default_master_ratio() -> u32 { 60 }

/// Master layout settings for one named display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
pub struct ScreenMasterConfig {
    /// Display: `main`/`primary`, `secondary` or a display name.
    pub screen: String,
    /// Master window size as a percentage, 0 to 100.
    #[serde(default = "default_master_ratio")]
    pub ratio: u32,
    /// Position of the master window.
    #[serde(default)]
    pub position: MasterPosition,
}

/// Master layout settings that apply globally or per display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(untagged)]
pub enum MasterConfigValue {
    /// Same settings for every display.
    Global(MasterConfig),
    /// Per-display settings.
    PerScreen(Vec<ScreenMasterConfig>),
}

impl Default for MasterConfigValue {
    fn default() -> Self { Self::Global(MasterConfig::default()) }
}

impl MasterConfigValue {
    pub(super) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Global(master) => validate_ratio("master.ratio", master.ratio),
            Self::PerScreen(screens) => {
                for (index, screen) in screens.iter().enumerate() {
                    validate_ratio(&format!("master[{index}].ratio"), screen.ratio)?;
                }
                Ok(())
            }
        }
    }

    /// Resolves the settings that apply to a display, with the same rules as
    /// per-display gaps: the first entry whose `screen` matches the display
    /// name or an applicable alias wins, else the first entry; an empty list
    /// yields the defaults.
    #[must_use]
    pub fn for_display(&self, name: &str, is_main: bool) -> MasterConfig {
        match self {
            Self::Global(master) => master.clone(),
            Self::PerScreen(screens) => screens
                .iter()
                .find(|entry| super::screen_matches(&entry.screen, name, is_main))
                .or_else(|| screens.first())
                .map_or_default(|entry| MasterConfig {
                    ratio: entry.ratio,
                    position: entry.position,
                }),
        }
    }
}
