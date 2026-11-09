//! Inner and outer gaps.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

/// A gap value: uniform, per axis (inner) or per side (outer).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(untagged)]
pub enum GapValue {
    /// Same value for all sides and axes.
    Uniform(u32),
    /// Different values per axis.
    PerAxis {
        /// Horizontal gap.
        horizontal: u32,
        /// Vertical gap.
        vertical: u32,
    },
    /// Different values per side.
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
    fn validate(&self, field: &str) -> Result<(), String> {
        if let Self::PerSide { top, right, bottom, left } = self
            && (left.checked_add(*right).is_none() || top.checked_add(*bottom).is_none())
        {
            return Err(format!("{field}: opposite side gaps overflow"));
        }
        Ok(())
    }
}

/// Gaps for a single display or for every display.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "snake_case")]
pub struct GapsConfig {
    /// Gaps between windows.
    pub inner: GapValue,
    /// Gaps from display edges.
    pub outer: GapValue,
}

/// Gaps for one named display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
pub struct ScreenGapsConfig {
    /// Display: `main`/`primary`, `secondary` or a display name.
    pub screen: String,
    /// Gaps between windows.
    #[serde(default)]
    pub inner: GapValue,
    /// Gaps from display edges.
    #[serde(default)]
    pub outer: GapValue,
}

/// Gaps that apply globally or per display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(untagged)]
pub enum GapsConfigValue {
    /// Same gaps for every display.
    Global(GapsConfig),
    /// Per-display gaps.
    PerScreen(Vec<ScreenGapsConfig>),
}

impl Default for GapsConfigValue {
    fn default() -> Self { Self::Global(GapsConfig::default()) }
}

impl GapsConfigValue {
    pub(super) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Global(gaps) => {
                gaps.inner.validate("gaps.inner")?;
                gaps.outer.validate("gaps.outer")
            }
            Self::PerScreen(screens) => {
                for (index, screen) in screens.iter().enumerate() {
                    screen.inner.validate(&format!("gaps[{index}].inner"))?;
                    screen.outer.validate(&format!("gaps[{index}].outer"))?;
                }
                Ok(())
            }
        }
    }
}
