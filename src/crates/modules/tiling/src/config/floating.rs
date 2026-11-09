//! Floating window defaults and presets.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

use super::DimensionValue;

/// Default position for newly floating windows.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Type,
)]
#[serde(rename_all = "snake_case")]
pub enum FloatingPosition {
    /// Center the window on its display.
    #[default]
    Center,
    /// Keep the application's frame.
    Default,
}

/// A floating window preset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
pub struct FloatingPreset {
    /// Unique preset name, compared case-insensitively.
    pub name: String,
    /// Width in pixels (`1440`) or percent (`"50%"`).
    pub width: DimensionValue,
    /// Height in pixels (`900`) or percent (`"100%"`).
    pub height: DimensionValue,
    /// X position; ignored when `center` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<DimensionValue>,
    /// Y position; ignored when `center` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<DimensionValue>,
    /// Center on the display; `x` and `y` are ignored.
    #[serde(default)]
    pub center: bool,
}

/// Floating window configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "snake_case")]
pub struct FloatingConfig {
    /// Default position for newly floating windows.
    pub default_position: FloatingPosition,
    /// Named presets.
    pub presets: Vec<FloatingPreset>,
}

impl FloatingConfig {
    /// Whether a case-insensitive preset name is defined.
    pub(super) fn has_preset(&self, name: &str) -> bool {
        self.presets
            .iter()
            .any(|preset| preset.name.to_lowercase() == name.to_lowercase())
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        for (index, preset) in self.presets.iter().enumerate() {
            let at = format!("floating.presets[{index}]");
            if self.presets[..index]
                .iter()
                .any(|earlier| earlier.name.to_lowercase() == preset.name.to_lowercase())
            {
                return Err(format!("{at}: duplicate preset name `{}`", preset.name));
            }
            preset.width.validate(&format!("{at}.width"), true)?;
            preset.height.validate(&format!("{at}.height"), true)?;
            if let Some(x) = &preset.x {
                x.validate(&format!("{at}.x"), false)?;
            }
            if let Some(y) = &preset.y {
                y.validate(&format!("{at}.y"), false)?;
            }
        }
        Ok(())
    }
}
