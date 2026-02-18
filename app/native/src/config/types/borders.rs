//! Window border configuration types.
//!
//! Configuration for window borders with support for solid colors, gradients, and glow effects.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::color::{Rgba, parse_hex_color};
use super::workspaces::WindowRule;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_border_state_config_is_enabled() {
        let disabled = BorderStateConfig::Disabled(false);
        assert!(!disabled.is_enabled());

        let solid = BorderStateConfig::default_focused();
        assert!(solid.is_enabled());
    }

    #[test]
    fn test_border_state_config_width() {
        let disabled = BorderStateConfig::Disabled(false);
        assert_eq!(disabled.width(), None);

        let solid = BorderStateConfig::SolidColor {
            width: 4,
            color: "#ffffff".to_string(),
        };
        assert_eq!(solid.width(), Some(4));
    }

    #[test]
    fn test_borders_config_default() {
        let config = BordersConfig::default();
        assert!(!config.is_enabled());
        assert!(config.focused.is_enabled());
        assert!(config.unfocused.is_enabled());
    }
}
