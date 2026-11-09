//! Window border configuration.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

use super::color::validate_color;
use super::{EasingType, WindowRule};

/// Gradient border color.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "camelCase")]
pub struct GradientConfig {
    /// Starting color.
    pub from: String,
    /// Ending color.
    pub to: String,
    /// Angle in degrees; 0 is left to right, 90 bottom to top.
    #[serde(default = "default_gradient_angle")]
    pub angle: f64,
}

const fn default_gradient_angle() -> f64 { 90.0 }

impl Default for GradientConfig {
    fn default() -> Self {
        Self {
            from: "#b4befe".to_owned(),
            to: "#cba6f7".to_owned(),
            angle: default_gradient_angle(),
        }
    }
}

/// Color animation for gradient borders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "camelCase")]
pub struct BorderAnimationConfig {
    /// Duration in milliseconds of one color-swap leg.
    pub duration: u32,
    /// Easing of each leg.
    #[serde(default = "default_border_easing")]
    pub easing: EasingType,
}

const fn default_border_easing() -> EasingType { EasingType::Linear }

/// One border state: `false` or an object with a width and a color source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(untagged)]
pub enum BorderStateConfig {
    /// `false` disables the border for this state.
    Disabled(bool),
    /// Solid color.
    SolidColor {
        /// Width in pixels.
        width: u32,
        /// Solid color.
        color: String,
    },
    /// Gradient color.
    GradientColor {
        /// Width in pixels.
        width: u32,
        /// Gradient.
        gradient: GradientConfig,
        /// Optional color animation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        animation: Option<BorderAnimationConfig>,
    },
    /// Glow effect.
    GlowColor {
        /// Width in pixels.
        width: u32,
        /// Glow color.
        glow: String,
    },
}

impl BorderStateConfig {
    fn solid(color: &str) -> Self {
        Self::SolidColor {
            width: 4,
            color: color.to_owned(),
        }
    }

    fn validate(&self, field: &str) -> Result<(), String> {
        match self {
            Self::Disabled(false) => Ok(()),
            Self::Disabled(true) => Err(format!(
                "{field}: `true` has no width or color; use `false` or an object with a width and \
                 a color, gradient or glow"
            )),
            Self::SolidColor { color, .. } => validate_color(&format!("{field}.color"), color),
            Self::GlowColor { glow, .. } => validate_color(&format!("{field}.glow"), glow),
            Self::GradientColor { gradient, .. } => {
                validate_color(&format!("{field}.gradient.from"), &gradient.from)?;
                validate_color(&format!("{field}.gradient.to"), &gradient.to)?;
                if !gradient.angle.is_finite() {
                    return Err(format!("{field}.gradient.angle must be finite"));
                }
                Ok(())
            }
        }
    }
}

/// Border corner style.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Type,
)]
#[serde(rename_all = "lowercase")]
pub enum BorderStyle {
    /// Rounded corners.
    #[default]
    Round,
    /// Square corners.
    Square,
}

/// Window border configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "camelCase")]
pub struct BordersConfig {
    /// Whether borders are enabled. Setting this to `true` is your explicit
    /// consent for Stache to overwrite the shared `JankyBorders` settings it names:
    /// width, style, hidpi, active and inactive colors and the blacklist. Their
    /// previous values are not read or restored. Stache never starts, restarts or
    /// kills the `JankyBorders` service (it must already be running) and, when
    /// borders stop or are disabled, leaves the last settings it sent in place.
    pub enabled: bool,
    /// Border style; omitted uses the external service default (round).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<BorderStyle>,
    /// Retina support (`hidpi`); omitted uses the external service default (true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidpi: Option<bool>,
    /// Focused windows.
    pub focused: BorderStateConfig,
    /// Unfocused windows; `false` hides them.
    pub unfocused: BorderStateConfig,
    /// Windows in monocle layout.
    pub monocle: BorderStateConfig,
    /// Floating windows.
    pub floating: BorderStateConfig,
    /// Windows in dwindle layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dwindle: Option<BorderStateConfig>,
    /// Windows in master layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<BorderStateConfig>,
    /// Windows in grid layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<BorderStateConfig>,
    /// Windows in split layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<BorderStateConfig>,
    /// Windows in vertical split layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_vertical: Option<BorderStateConfig>,
    /// Windows in horizontal split layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_horizontal: Option<BorderStateConfig>,
    /// Windows that never get a border, in addition to the global ignore rules.
    #[serde(default)]
    pub ignore: Vec<WindowRule>,
}

impl Default for BordersConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            style: None,
            hidpi: None,
            focused: BorderStateConfig::solid("#b4befe"),
            unfocused: BorderStateConfig::solid("#6c7086"),
            monocle: BorderStateConfig::solid("#cba6f7"),
            floating: BorderStateConfig::solid("#94e2d5"),
            dwindle: None,
            master: None,
            grid: None,
            split: None,
            split_vertical: None,
            split_horizontal: None,
            ignore: Vec::new(),
        }
    }
}

impl BordersConfig {
    pub(super) fn validate(&self) -> Result<(), String> {
        self.focused.validate("borders.focused")?;
        self.unfocused.validate("borders.unfocused")?;
        self.monocle.validate("borders.monocle")?;
        self.floating.validate("borders.floating")?;
        for (name, state) in [
            ("dwindle", &self.dwindle),
            ("master", &self.master),
            ("grid", &self.grid),
            ("split", &self.split),
            ("splitVertical", &self.split_vertical),
            ("splitHorizontal", &self.split_horizontal),
        ] {
            if let Some(state) = state {
                state.validate(&format!("borders.{name}"))?;
            }
        }
        Ok(())
    }
}
