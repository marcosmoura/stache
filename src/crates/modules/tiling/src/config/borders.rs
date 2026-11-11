//! Window border configuration.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

use super::color::validate_color;
use super::{EasingType, WindowRule};

/// Gradient border color.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
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

/// How a gradient border animates.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
pub enum BorderAnimationStyle {
    /// The two colors swap back and forth, `duration` per leg (focused window only).
    #[default]
    PingPong,
    /// The gradient angle turns 360 degrees once, `duration` per turn.
    Once,
    /// The gradient angle turns 360 degrees forever, `duration` per turn.
    Loop,
    /// Any other value; rejected by validation with the field path.
    #[serde(untagged)]
    #[schemars(skip)]
    #[specta(skip)]
    Unknown(String),
}

/// Animation for gradient borders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
pub struct BorderAnimationConfig {
    /// Animation style; omitted is `ping_pong`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<BorderAnimationStyle>,
    /// Duration in milliseconds of one color-swap leg (`ping_pong`) or one
    /// turn (`once`, `loop`); values below 16 are treated as 16.
    pub duration: u32,
    /// Easing of each leg or turn.
    #[serde(default = "default_border_easing")]
    pub easing: EasingType,
}

impl BorderAnimationConfig {
    /// The style in effect: omitted is `ping_pong`.
    #[must_use]
    pub fn effective_style(&self) -> BorderAnimationStyle { self.style.clone().unwrap_or_default() }
}

/// Focus color cross-fade between a border's previous and new colors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(rename_all = "snake_case")]
pub struct BorderTransitionConfig {
    /// Fade duration in milliseconds; 0 is instant, other values below 16 are
    /// treated as 16.
    pub duration: u32,
    /// Easing of the fade.
    #[serde(default)]
    pub easing: EasingType,
}

/// Where a border is ordered relative to its window.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Type,
)]
#[serde(rename_all = "snake_case")]
pub enum BorderOrder {
    /// Directly below the window; the window covers the inner part of the border.
    #[default]
    Below,
    /// Directly above the window, entirely outside its frame.
    Above,
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
            Self::GradientColor { gradient, animation, .. } => {
                if let Some(BorderAnimationStyle::Unknown(value)) =
                    animation.as_ref().and_then(|animation| animation.style.as_ref())
                {
                    return Err(format!(
                        "{field}.animation.style: unknown style `{value}`; expected `ping_pong`, \
                         `once` or `loop`"
                    ));
                }
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

/// Window border configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, Type)]
#[serde(default, rename_all = "snake_case")]
pub struct BordersConfig {
    /// Whether Stache draws borders around Tiling-managed windows. Borders are
    /// drawn by Stache itself; nothing external is started, changed or left
    /// behind, and disabling them removes every border.
    pub enabled: bool,
    /// Retina support (`hidpi`); omitted is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidpi: Option<bool>,
    /// Focus color cross-fade; omitted is instant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition: Option<BorderTransitionConfig>,
    /// Border order relative to its window; omitted is `below`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<BorderOrder>,
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
            hidpi: None,
            transition: None,
            order: None,
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
            ("split_vertical", &self.split_vertical),
            ("split_horizontal", &self.split_horizontal),
        ] {
            if let Some(state) = state {
                state.validate(&format!("borders.{name}"))?;
            }
        }
        Ok(())
    }
}
