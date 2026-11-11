//! Pure border appearance: colors, state precedence and the native mapping.
//!
//! `parse_color`, `is_enabled` and `focused_state` are the pure configuration
//! rules; [`border_appearance`] maps one configured state to the macOS
//! overlay's `BorderAppearance`.
//!
//! Approach only, never code (decision 5): the reference border tool's files
//! `src/border.c` (`border_draw`) and `src/misc/drawing.h` were studied for
//! what a border draws; the mapping is written from the Tiling configuration
//! and the part 02 spec.

use stache_macos::border_overlay::{BorderAppearance, BorderColor, BorderPaint, BorderPlacement};

use crate::config::{BorderOrder, BorderStateConfig, BordersConfig, LayoutType};

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

/// The width every border uses (resolved OQ-1): the active (focused) state's
/// width, as `focus_command` sends today. A disabled state has width 0.
#[must_use]
pub const fn active_width(active: &BorderStateConfig) -> u32 {
    match active {
        BorderStateConfig::Disabled(_) => 0,
        BorderStateConfig::SolidColor { width, .. }
        | BorderStateConfig::GradientColor { width, .. }
        | BorderStateConfig::GlowColor { width, .. } => *width,
    }
}

const fn border_color(rgba: Rgba) -> BorderColor {
    BorderColor {
        r: rgba.r,
        g: rgba.g,
        b: rgba.b,
        a: rgba.a,
    }
}

/// Fully transparent: what an unparsable color maps to. Today's argv path sends
/// a transparent color with the state's width, so nothing visible is drawn; the
/// overlay does the same (it is not removed, so a later valid recolor is an
/// in-place property change).
const TRANSPARENT: BorderColor = BorderColor { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

/// Maps one configured `state` to the overlay appearance, or `None` when no
/// overlay is wanted: a disabled state or `active_width` 0.
///
/// `active_width` is the width of the active (focused) state ([`active_width`]),
/// used for every border; `state.width` is never read. Colors parse as sRGB
/// channel values. Any unparsable
/// color becomes a transparent solid paint. `style` defaults to round and
/// `hidpi` to true; `order` picks the
/// placement (`below` when omitted). The gradient animation is not part of the
/// static appearance: `motion` stays `None` here and the model sets it per
/// window (`motion::state_motion`, `motion::motion_to_install`).
#[must_use]
pub fn border_appearance(
    config: &BordersConfig,
    state: &BorderStateConfig,
    active_width: u32,
) -> Option<BorderAppearance> {
    if active_width == 0 {
        return None;
    }
    let color = |text: &str| parse_color(text).ok().map(border_color);
    let paint = match state {
        BorderStateConfig::Disabled(_) => return None,
        BorderStateConfig::SolidColor { color: text, .. } => {
            BorderPaint::Solid(color(text).unwrap_or(TRANSPARENT))
        }
        BorderStateConfig::GradientColor { gradient, .. } => {
            match color(&gradient.from).zip(color(&gradient.to)) {
                Some((from, to)) => BorderPaint::Gradient {
                    from,
                    to,
                    angle: gradient.angle,
                },
                None => BorderPaint::Solid(TRANSPARENT),
            }
        }
        BorderStateConfig::GlowColor { glow, .. } => {
            color(glow).map_or(BorderPaint::Solid(TRANSPARENT), BorderPaint::Glow)
        }
    };
    Some(BorderAppearance {
        width: f64::from(active_width),
        hidpi: config.hidpi.unwrap_or(true),
        paint,
        corner_radius: None,
        motion: None,
        placement: match config.order.unwrap_or_default() {
            BorderOrder::Below => BorderPlacement::Below,
            BorderOrder::Above => BorderPlacement::Above,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(color: &str) -> BorderStateConfig {
        BorderStateConfig::SolidColor {
            width: 6,
            color: color.to_owned(),
        }
    }

    fn focus(layout: LayoutType, floating: bool) -> FocusInput { FocusInput { layout, floating } }

    /// `0xAARRGGBB` with half-away-from-zero rounding, as the argv path formats it.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn argb(rgba: &Rgba) -> u32 {
        let channel = |value: f64| (value * 255.0).round() as u32;
        channel(rgba.a) << 24 | channel(rgba.r) << 16 | channel(rgba.g) << 8 | channel(rgba.b)
    }

    #[test]
    fn color_parsing_fixtures_are_unchanged() {
        let parsed = |color: &str| argb(&parse_color(color).expect(color));
        assert_eq!(parsed("#F00"), 0xFFFF_0000);
        assert_eq!(parsed("#F008"), 0x88FF_0000);
        assert_eq!(parsed("#FF000080"), 0x80FF_0000);
        assert_eq!(parsed("0000FF"), 0xFF00_00FF);
        assert_eq!(parsed("  #00ff00  "), 0xFF00_FF00);
        assert_eq!(parsed("rgb(0, 128, 255)"), 0xFF00_80FF);
        assert_eq!(parsed("rgba(255, 0, 0, 0.5)"), 0x80FF_0000);
        assert!(parse_color("#12345").is_err());
        assert!(parse_color("rgba(1, 2, 3)").is_err());
        assert!(parse_color("rgba(1, 2, 3, 2)").is_err());
        assert!(parse_color("#ééé").is_err());
    }

    #[test]
    fn border_appearance_places_the_border_below_its_target() {
        let config = BordersConfig::default();
        let look = border_appearance(&config, &config.focused, 4).expect("appearance");
        assert_eq!(look.placement, BorderPlacement::Below);
    }

    #[test]
    fn order_selects_the_placement() {
        let state = solid("#ffffff");
        for (order, placement) in [
            (None, BorderPlacement::Below),
            (Some(BorderOrder::Below), BorderPlacement::Below),
            (Some(BorderOrder::Above), BorderPlacement::Above),
        ] {
            let config = BordersConfig {
                order,
                ..BordersConfig::default()
            };
            let look = border_appearance(&config, &state, 4).expect("appearance");
            assert_eq!(look.placement, placement, "{order:?}");
        }
    }

    #[test]
    fn state_precedence_matches_focused_state_config() {
        let mut config = BordersConfig {
            focused: solid("#111111"),
            floating: solid("#222222"),
            monocle: solid("#333333"),
            dwindle: Some(solid("#444444")),
            master: Some(BorderStateConfig::Disabled(false)),
            ..BordersConfig::default()
        };
        let pick = |config: &BordersConfig, layout, floating| {
            let BorderStateConfig::SolidColor { color, .. } =
                focused_state(config, focus(layout, floating))
            else {
                panic!("solid state");
            };
            color.clone()
        };
        assert_eq!(pick(&config, LayoutType::Floating, false), "#222222");
        assert_eq!(pick(&config, LayoutType::Dwindle, true), "#222222");
        assert_eq!(pick(&config, LayoutType::Monocle, false), "#333333");
        assert_eq!(pick(&config, LayoutType::Dwindle, false), "#444444");
        assert_eq!(pick(&config, LayoutType::Master, false), "#111111");
        assert_eq!(pick(&config, LayoutType::Grid, false), "#111111");
        config.floating = BorderStateConfig::Disabled(false);
        assert_eq!(pick(&config, LayoutType::Dwindle, true), "#444444");
        assert_eq!(pick(&config, LayoutType::Floating, false), "#111111");
        assert!(!is_enabled(&BorderStateConfig::Disabled(false)));
        assert!(is_enabled(&solid("#fff")));
    }

    mod native {
        use stache_macos::border_overlay::BorderPaint;

        use super::*;
        use crate::config::GradientConfig;

        const fn color(r: f64, g: f64, b: f64, a: f64) -> BorderColor { BorderColor { r, g, b, a } }

        fn gradient(angle: f64) -> BorderStateConfig {
            BorderStateConfig::GradientColor {
                width: 5,
                gradient: GradientConfig {
                    from: "#FF0000".to_owned(),
                    to: "#0000FF".to_owned(),
                    angle,
                },
                animation: None,
            }
        }

        fn glow(color: &str) -> BorderStateConfig {
            BorderStateConfig::GlowColor {
                width: 8,
                glow: color.to_owned(),
            }
        }

        fn map(config: &BordersConfig, state: &BorderStateConfig) -> Option<BorderAppearance> {
            border_appearance(config, state, active_width(&config.focused))
        }

        #[test]
        fn every_state_variant_maps_to_its_paint() {
            let config = BordersConfig::default();
            let solid = map(&config, &solid("rgb(0, 128, 255)")).expect("solid");
            let BorderPaint::Solid(c) = solid.paint else {
                panic!("solid paint")
            };
            assert!((c.g - 128.0 / 255.0).abs() < 1e-12 && c.a.to_bits() == 1.0f64.to_bits());
            let alpha = map(&config, &super::solid("rgba(255, 0, 0, 0.5)")).expect("rgba");
            assert_eq!(alpha.paint, BorderPaint::Solid(color(1.0, 0.0, 0.0, 0.5)));

            let angled = map(&config, &gradient(-90.0)).expect("gradient");
            assert_eq!(angled.paint, BorderPaint::Gradient {
                from: color(1.0, 0.0, 0.0, 1.0),
                to: color(0.0, 0.0, 1.0, 1.0),
                angle: -90.0,
            });

            let glowing = map(&config, &glow("#B4BEFE")).expect("glow");
            assert!(matches!(glowing.paint, BorderPaint::Glow(_)));
            assert!(glowing.corner_radius.is_none());
        }

        #[test]
        fn disabled_states_and_width_zero_map_to_no_overlay() {
            let config = BordersConfig::default();
            assert_eq!(map(&config, &BorderStateConfig::Disabled(false)), None);
            assert_eq!(map(&config, &BorderStateConfig::Disabled(true)), None);
            assert_eq!(border_appearance(&config, &super::solid("#fff"), 0), None);
            assert_eq!(active_width(&BorderStateConfig::Disabled(false)), 0);
            // A disabled focused state has width 0, so no border is drawn at all.
            let off = BordersConfig {
                focused: BorderStateConfig::Disabled(false),
                unfocused: super::solid("#fff"),
                ..BordersConfig::default()
            };
            assert_eq!(map(&off, &off.unfocused), None);
        }

        #[test]
        fn an_unparsable_color_is_a_transparent_solid_with_its_width() {
            let config = BordersConfig::default();
            let transparent = BorderPaint::Solid(color(0.0, 0.0, 0.0, 0.0));
            for state in [
                super::solid("zz"),
                glow("nope"),
                BorderStateConfig::GradientColor {
                    width: 5,
                    gradient: GradientConfig {
                        from: "#FF0000".to_owned(),
                        to: "zz".to_owned(),
                        angle: 45.0,
                    },
                    animation: None,
                },
            ] {
                let mapped = border_appearance(&config, &state, 6).expect("kept");
                assert_eq!(mapped.paint, transparent);
                assert!((mapped.width - 6.0).abs() < f64::EPSILON);
            }
        }

        #[test]
        fn hidpi_follows_the_config_with_its_default() {
            let state = super::solid("#fff");
            let default = map(&BordersConfig::default(), &state).expect("default");
            assert!(default.hidpi);
            let off = BordersConfig {
                hidpi: Some(false),
                ..BordersConfig::default()
            };
            assert!(!map(&off, &state).expect("off").hidpi);
        }

        #[test]
        fn every_border_uses_the_active_states_width() {
            let config = BordersConfig {
                focused: super::solid("#111111"),
                unfocused: BorderStateConfig::SolidColor {
                    width: 2,
                    color: "#222222".to_owned(),
                },
                monocle: BorderStateConfig::SolidColor {
                    width: 9,
                    color: "#333333".to_owned(),
                },
                ..BordersConfig::default()
            };
            let active = focused_state(&config, focus(LayoutType::Monocle, false));
            let width = active_width(active);
            assert_eq!(width, 9);
            for state in [active, &config.unfocused, &config.focused] {
                let mapped = border_appearance(&config, state, width).expect("drawn");
                assert!((mapped.width - 9.0).abs() < f64::EPSILON);
            }
            let grid = focused_state(&config, focus(LayoutType::Grid, false));
            assert_eq!(active_width(grid), 6);
        }
    }
}
