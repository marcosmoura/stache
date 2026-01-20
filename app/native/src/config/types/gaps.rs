//! Gap configuration types.
//!
//! Configuration for inner and outer gaps in tiling layouts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
    /// Screen identifier: "main"/"primary", "secondary", or screen name.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dimension_value_resolve_pixels() {
        let dim = DimensionValue::Pixels(100);
        assert!((dim.resolve(1000.0) - 100.0).abs() < 0.01);
    }

    #[test]
    fn test_dimension_value_resolve_percentage() {
        let dim = DimensionValue::Percentage("50%".to_string());
        assert!((dim.resolve(1000.0) - 500.0).abs() < 0.01);
    }

    #[test]
    fn test_gap_value_as_inner() {
        let uniform = GapValue::Uniform(10);
        assert_eq!(uniform.as_inner(), (10, 10));

        let per_axis = GapValue::PerAxis { horizontal: 20, vertical: 30 };
        assert_eq!(per_axis.as_inner(), (20, 30));
    }

    #[test]
    fn test_gap_value_as_outer() {
        let uniform = GapValue::Uniform(10);
        assert_eq!(uniform.as_outer(), (10, 10, 10, 10));

        let per_side = GapValue::PerSide {
            top: 10,
            right: 20,
            bottom: 30,
            left: 40,
        };
        assert_eq!(per_side.as_outer(), (10, 20, 30, 40));
    }
}
