//! Pure border policy: exactly what `JankyBorders` is told, never how or when.
//!
//! Ported from the legacy `modules/tiling/borders.rs` and
//! `config/types/{borders,color}.rs` (parity evidence only). Nothing here sends,
//! sleeps, reads a clock or touches native state, so every function is a
//! fixture-testable pure value transformation.
//!
//! Argument vectors are compared and deduplicated as `Vec<String>` (argument
//! boundaries), never as a joined string: `["a b", "c"]` and `["a", "b c"]` are
//! different commands.

use std::time::Duration;

use crate::animation::apply_easing;
use crate::config::{
    BorderAnimationConfig, BorderStateConfig, BorderStyle, BordersConfig, EasingType,
    GradientConfig, LayoutType, WindowRule,
};
use crate::rules::{SKIP_TILING_APP_NAMES, SKIP_TILING_BUNDLE_IDS};

/// Maximum animation frames per second (low-rate: `JankyBorders` has a FIFO Mach
/// queue).
pub const ANIMATION_FPS: u64 = 8;
/// The shortest period between two animation frames (`1000 / 8` ms).
pub const FRAME_PERIOD: Duration = Duration::from_millis(1_000 / ANIMATION_FPS);
/// Shortest animation leg, as legacy.
const MIN_LEG_MS: u32 = 16;
/// The transparent color a disabled state sends.
const TRANSPARENT: &str = "0x00000000";

/// An RGBA color, channels in `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    /// Red.
    pub r: f64,
    /// Green.
    pub g: f64,
    /// Blue.
    pub b: f64,
    /// Alpha.
    pub a: f64,
}

/// Parses `#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA` (the `#` is optional) and
/// `rgb()` / `rgba()`.
///
/// The legacy converter parsed hex only, so a configured `rgba(...)` that the
/// config accepted became fully transparent; here the validated syntax is
/// honoured instead of silently erased.
///
/// # Errors
/// A description of the malformed color.
pub fn parse_color(color: &str) -> Result<Rgba, String> {
    let trimmed = color.trim();
    if trimmed.starts_with("rgba(") || trimmed.starts_with("rgb(") {
        parse_functional(trimmed)
    } else {
        parse_hex(trimmed)
    }
}

fn nibble(hex: &str, at: usize) -> Result<u8, String> {
    u8::from_str_radix(&hex[at..=at], 16).map_err(|error| error.to_string())
}

fn byte(hex: &str, at: usize) -> Result<u8, String> {
    u8::from_str_radix(&hex[at..at + 2], 16).map_err(|error| error.to_string())
}

fn parse_hex(value: &str) -> Result<Rgba, String> {
    let hex = value.trim_start_matches('#');
    if !hex.is_ascii() {
        return Err("expected hexadecimal digits".to_owned());
    }
    let (r, g, b, a) = match hex.len() {
        3 => (
            nibble(hex, 0)? * 17,
            nibble(hex, 1)? * 17,
            nibble(hex, 2)? * 17,
            255,
        ),
        4 => (
            nibble(hex, 0)? * 17,
            nibble(hex, 1)? * 17,
            nibble(hex, 2)? * 17,
            nibble(hex, 3)? * 17,
        ),
        6 => (byte(hex, 0)?, byte(hex, 2)?, byte(hex, 4)?, 255),
        8 => (byte(hex, 0)?, byte(hex, 2)?, byte(hex, 4)?, byte(hex, 6)?),
        other => {
            return Err(format!(
                "invalid hex color length: expected 3, 4, 6 or 8 characters, got {other}"
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

fn parse_functional(value: &str) -> Result<Rgba, String> {
    let (inner, has_alpha) = if let Some(inner) = value.strip_prefix("rgba(") {
        (inner, true)
    } else if let Some(inner) = value.strip_prefix("rgb(") {
        (inner, false)
    } else {
        return Err("color must start with rgb( or rgba(".to_owned());
    };
    let inner = inner.strip_suffix(')').ok_or("missing closing parenthesis")?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    let expected = if has_alpha { 4 } else { 3 };
    if parts.len() != expected {
        return Err(format!("expected {expected} values, got {}", parts.len()));
    }
    let channel = |text: &str| -> Result<f64, String> {
        text.parse::<u8>()
            .map(|value| f64::from(value) / 255.0)
            .map_err(|_| format!("invalid channel `{text}`"))
    };
    let a = if has_alpha {
        let alpha: f64 = parts[3].parse().map_err(|_| format!("invalid alpha `{}`", parts[3]))?;
        if !(0.0..=1.0).contains(&alpha) {
            return Err(format!("alpha must be between 0 and 1, got {alpha}"));
        }
        alpha
    } else {
        1.0
    };
    Ok(Rgba {
        r: channel(parts[0])?,
        g: channel(parts[1])?,
        b: channel(parts[2])?,
        a,
    })
}

/// `0xAARRGGBB`, each channel rounded half away from zero.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn rgba_to_janky(rgba: &Rgba) -> String {
    let a = (rgba.a * 255.0).round() as u8;
    let r = (rgba.r * 255.0).round() as u8;
    let g = (rgba.g * 255.0).round() as u8;
    let b = (rgba.b * 255.0).round() as u8;
    format!("0x{a:02X}{r:02X}{g:02X}{b:02X}")
}

fn color_to_janky(color: &str) -> Option<String> {
    parse_color(color).ok().map(|rgba| rgba_to_janky(&rgba))
}

/// `gradient(...)` for the angle boundaries of the legacy converter: after
/// normalizing to `0..360`, angles in `[0, 90)` and `[180, 270)` run
/// top-right to bottom-left, the others top-left to bottom-right.
fn gradient_to_janky(from: &str, to: &str, angle: f64) -> String {
    let angle = ((angle % 360.0) + 360.0) % 360.0;
    if (0.0..90.0).contains(&angle) || (180.0..270.0).contains(&angle) {
        format!("gradient(top_right={from},bottom_left={to})")
    } else {
        format!("gradient(top_left={from},bottom_right={to})")
    }
}

/// The `JankyBorders` color and width of one state: solid, gradient or glow;
/// a disabled state (or an unparsable color) is transparent. A disabled state
/// has width 0, an unparsable color keeps its width (as legacy).
#[must_use]
pub fn state_settings(state: &BorderStateConfig) -> (String, u32) {
    match state {
        BorderStateConfig::Disabled(_) => (TRANSPARENT.to_owned(), 0),
        BorderStateConfig::SolidColor { width, color } => (
            color_to_janky(color).unwrap_or_else(|| TRANSPARENT.to_owned()),
            *width,
        ),
        BorderStateConfig::GradientColor { width, gradient, .. } => {
            let color = color_to_janky(&gradient.from)
                .zip(color_to_janky(&gradient.to))
                .map(|(from, to)| gradient_to_janky(&from, &to, gradient.angle));
            (color.unwrap_or_else(|| TRANSPARENT.to_owned()), *width)
        }
        BorderStateConfig::GlowColor { width, glow } => (
            color_to_janky(glow)
                .map_or_else(|| TRANSPARENT.to_owned(), |color| format!("glow({color})")),
            *width,
        ),
    }
}

/// Whether a state draws anything (`false` is the only disabled value).
#[must_use]
pub const fn is_enabled(state: &BorderStateConfig) -> bool {
    !matches!(state, BorderStateConfig::Disabled(false))
}

/// The pure facts a border update depends on: the focused window's layout and
/// whether it floats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocusInput {
    /// The layout of the focused window's workspace.
    pub layout: LayoutType,
    /// The focused window is individually floating.
    pub floating: bool,
}

/// The state that colors the active border (legacy `focused_state_config`):
/// floating (layout or window) when its state is enabled, else the layout's
/// override when present *and enabled*, else `focused`.
#[must_use]
pub const fn focused_state(config: &BordersConfig, focus: FocusInput) -> &BorderStateConfig {
    if (matches!(focus.layout, LayoutType::Floating) || focus.floating)
        && is_enabled(&config.floating)
    {
        return &config.floating;
    }
    let layout = match focus.layout {
        LayoutType::Monocle => Some(&config.monocle),
        LayoutType::Dwindle => config.dwindle.as_ref(),
        LayoutType::Master => config.master.as_ref(),
        LayoutType::Grid => config.grid.as_ref(),
        LayoutType::Split => config.split.as_ref(),
        LayoutType::SplitVertical => config.split_vertical.as_ref(),
        LayoutType::SplitHorizontal => config.split_horizontal.as_ref(),
        LayoutType::Floating => Some(&config.floating),
    };
    match layout {
        Some(state) if is_enabled(state) => state,
        _ => &config.focused,
    }
}

/// The blacklist value: system bundle ids, system names, the tiling `ignore`
/// rules then the border `ignore` rules (each rule's `app_id` then `app_name`),
/// joined with commas.
#[must_use]
pub fn blacklist(tiling_ignore: &[WindowRule], border_ignore: &[WindowRule]) -> String {
    let mut apps: Vec<&str> = Vec::new();
    apps.extend(SKIP_TILING_BUNDLE_IDS);
    apps.extend(SKIP_TILING_APP_NAMES);
    for rule in tiling_ignore.iter().chain(border_ignore) {
        apps.extend(rule.app_id.as_deref());
        apps.extend(rule.app_name.as_deref());
    }
    apps.join(",")
}

/// The base batch: width, style, hidpi, active and inactive colors, blacklist.
/// Width and the active color come from `focused`; legacy init did the same.
#[must_use]
pub fn base_args(config: &BordersConfig, blacklist: &str) -> Vec<String> {
    let (active, _) = state_settings(&config.focused);
    let (inactive, _) = state_settings(&config.unfocused);
    let width = match &config.focused {
        BorderStateConfig::Disabled(_) => 4,
        BorderStateConfig::SolidColor { width, .. }
        | BorderStateConfig::GradientColor { width, .. }
        | BorderStateConfig::GlowColor { width, .. } => *width,
    };
    let style = match config.style.unwrap_or(BorderStyle::Round) {
        BorderStyle::Round => 'r',
        BorderStyle::Square => 's',
    };
    let hidpi = if config.hidpi.unwrap_or(true) {
        "on"
    } else {
        "off"
    };
    vec![
        format!("width={width}"),
        format!("style={style}"),
        format!("hidpi={hidpi}"),
        format!("active_color={active}"),
        format!("inactive_color={inactive}"),
        format!("blacklist={blacklist}"),
    ]
}

/// A gradient animation's pure parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct AnimationPlan {
    from: Rgba,
    to: Rgba,
    angle: f64,
    leg: Duration,
    easing: EasingType,
}

impl AnimationPlan {
    fn new(gradient: &GradientConfig, animation: &BorderAnimationConfig) -> Option<Self> {
        Some(Self {
            from: parse_color(&gradient.from).ok()?,
            to: parse_color(&gradient.to).ok()?,
            angle: gradient.angle,
            leg: Duration::from_millis(u64::from(animation.duration.max(MIN_LEG_MS))),
            easing: animation.easing,
        })
    }

    /// The `active_color` value of the ping-pong at `progress` in the current
    /// leg (`0.0..=1.0`, already direction-adjusted).
    fn color_at(&self, progress: f64) -> String {
        let lerp = |a: f64, b: f64| (b - a).mul_add(progress, a);
        let mix = |a: &Rgba, b: &Rgba| Rgba {
            r: lerp(a.r, b.r),
            g: lerp(a.g, b.g),
            b: lerp(a.b, b.b),
            a: lerp(a.a, b.a),
        };
        let from = rgba_to_janky(&mix(&self.from, &self.to));
        let to = rgba_to_janky(&mix(&self.to, &self.from));
        format!("active_color={}", gradient_to_janky(&from, &to, self.angle))
    }
}

/// The ping-pong cursor of an [`AnimationPlan`]. It reads no clock: callers pass
/// the time elapsed since the cursor was created.
#[derive(Clone, Debug, PartialEq)]
pub struct PingPong {
    plan: AnimationPlan,
    forward: bool,
    /// Elapsed time at which the current leg began.
    leg_start: Duration,
}

impl PingPong {
    /// A cursor at the start of a forward leg.
    #[must_use]
    pub const fn new(plan: AnimationPlan) -> Self {
        Self {
            plan,
            forward: true,
            leg_start: Duration::ZERO,
        }
    }

    /// The `active_color=...` argument at `elapsed`; the direction flips (and
    /// the leg restarts) once a leg reached its end, as legacy.
    pub fn frame(&mut self, elapsed: Duration) -> String {
        let in_leg = elapsed.saturating_sub(self.leg_start);
        let raw = (in_leg.as_secs_f64() / self.plan.leg.as_secs_f64()).min(1.0);
        let eased = apply_easing(raw, self.plan.easing);
        let progress = if self.forward { eased } else { 1.0 - eased };
        let argument = self.plan.color_at(progress);
        if raw >= 1.0 {
            self.forward = !self.forward;
            self.leg_start = elapsed;
        }
        argument
    }
}

/// The command for one focus state: the argv and, for an animated gradient,
/// the animation to run after it.
#[derive(Clone, Debug, PartialEq)]
pub struct FocusCommand {
    /// `width`, `active_color`, `inactive_color`.
    pub args: Vec<String>,
    /// The ping-pong to drive after the command, if the state animates.
    pub animation: Option<AnimationPlan>,
}

/// The focus-change command for `focus` (legacy `on_focus_changed`).
#[must_use]
pub fn focus_command(config: &BordersConfig, focus: FocusInput) -> FocusCommand {
    let active = focused_state(config, focus);
    let (active_color, width) = state_settings(active);
    let (inactive_color, _) = state_settings(&config.unfocused);
    let animation = match active {
        BorderStateConfig::GradientColor {
            gradient,
            animation: Some(animation),
            ..
        } => AnimationPlan::new(gradient, animation),
        _ => None,
    };
    FocusCommand {
        args: vec![
            format!("width={width}"),
            format!("active_color={active_color}"),
            format!("inactive_color={inactive_color}"),
        ],
        animation,
    }
}
