//! Pure panel frame math.
//!
//! The panel lives on the primary display only: `NSScreen.screens[0]`, which
//! [`DisplayLayout::primary`] returns (`is_primary`), never `mainScreen`
//! (`is_main`). `DisplayInfo::frame` is in the `AppKit` space (bottom-left
//! origin, y up) while Tauri and tao position windows with a top-left origin on
//! the primary display. The conversion goes through
//! [`DisplayLayout::ax_frame`], which is the same top-left space.
//!
//! Everything is in logical points: the backing scale is never applied, so a
//! scale-2 display is not scaled twice. Fractional edges round half away from
//! zero (`f64::round`), the same way on every call, to the integer types the
//! window request uses.

use stache_macos::DisplayLayout;
use stache_macos::applications::DisplayRect;
use stache_sdk::{DisplayId, MAX_WINDOW_DIMENSION};

use crate::config::BarConfig;

/// The panel frame in logical points, top-left origin of the primary display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelFrame {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width, at least one.
    pub width: u32,
    /// Height, at least one.
    pub height: u32,
}

/// What the configuration and the display layout ask for.
#[derive(Clone, Debug, PartialEq)]
pub enum Desired {
    /// `height: 0` or a computed width `<= 0` (overview Decision 16).
    ZeroSize,
    /// The topology has no primary display.
    NoPrimary,
    /// The frame cannot be expressed: a non-finite edge or a dimension above
    /// [`MAX_WINDOW_DIMENSION`]. Nothing is wrapped or clamped.
    Invalid(String),
    /// One panel on the primary display.
    Panel {
        /// The primary display (SDK identity, supplied by the host).
        display: DisplayId,
        /// The frame to occupy.
        frame: PanelFrame,
        /// The constant top inset, `height + padding`.
        reservation_top: f64,
    },
}

fn round_i64(value: f64) -> Option<i64> {
    // Far beyond any display; also rejects NaN and infinities.
    if !value.is_finite() || value.abs() > 1.0e9 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)] // bounded above
    Some(value.round() as i64)
}

/// Computes the single desired panel for `config` on `layout`.
///
/// `primary` is the SDK identity of the layout's primary display, which the
/// host projected from the same layout.
#[must_use]
pub fn plan(
    config: &BarConfig,
    layout: Option<&DisplayLayout>,
    primary: Option<DisplayId>,
) -> Desired {
    if config.height == 0 {
        return Desired::ZeroSize;
    }
    let (Some(layout), Some(display)) = (layout, primary) else {
        return Desired::NoPrimary;
    };
    let Some(info) = layout.primary() else {
        return Desired::NoPrimary;
    };
    let Some(top_left): Option<DisplayRect> = layout.ax_frame(info) else {
        return Desired::NoPrimary;
    };
    let padding = i64::from(config.padding);
    let (Some(left), Some(top), Some(width)) = (
        round_i64(top_left.x),
        round_i64(top_left.y),
        round_i64(top_left.width),
    ) else {
        return Desired::Invalid("the primary display frame is not a finite number".to_owned());
    };
    let panel_width = width - 2 * padding;
    if panel_width <= 0 {
        return Desired::ZeroSize;
    }
    let (Ok(x), Ok(y)) = (i32::try_from(left + padding), i32::try_from(top + padding)) else {
        return Desired::Invalid("the panel origin does not fit a window coordinate".to_owned());
    };
    let height = u32::from(config.height);
    let Ok(panel_width) = u32::try_from(panel_width) else {
        return Desired::Invalid("the panel width does not fit a window dimension".to_owned());
    };
    if panel_width > MAX_WINDOW_DIMENSION || height > MAX_WINDOW_DIMENSION {
        return Desired::Invalid(format!(
            "the {panel_width}x{height} panel exceeds the {MAX_WINDOW_DIMENSION} point window limit"
        ));
    }
    Desired::Panel {
        display,
        frame: PanelFrame {
            x,
            y,
            width: panel_width,
            height,
        },
        reservation_top: f64::from(config.height) + f64::from(config.padding),
    }
}

#[cfg(test)]
mod tests {
    use stache_macos::DisplayInfo;
    use stache_macos::desktop::DisplayId as NativeId;

    use super::*;

    fn info(id: u32, rect: DisplayRect, scale: f64, primary: bool, main: bool) -> DisplayInfo {
        DisplayInfo {
            id: NativeId::new(id),
            frame: rect,
            visible_frame: rect,
            scale,
            refresh_hz: None,
            name: None,
            is_primary: primary,
            is_main: main,
            is_builtin: primary,
        }
    }

    fn config(height: u16, padding: u16) -> BarConfig {
        BarConfig {
            enabled: true,
            height,
            padding,
            ..BarConfig::default()
        }
    }

    const PRIMARY: DisplayId = DisplayId::new(1);

    fn laptop() -> DisplayLayout {
        DisplayLayout::new(vec![info(
            1,
            DisplayRect::new(0.0, 0.0, 1512.0, 982.0),
            2.0,
            true,
            true,
        )])
    }

    #[test]
    fn the_panel_is_inset_by_padding_from_the_primary_top_left() {
        let desired = plan(&config(28, 14), Some(&laptop()), Some(PRIMARY));
        assert_eq!(desired, Desired::Panel {
            display: PRIMARY,
            frame: PanelFrame {
                x: 14,
                y: 14,
                width: 1484,
                height: 28
            },
            reservation_top: 42.0,
        });
    }

    #[test]
    fn scale_two_is_not_applied_a_second_time() {
        let one = DisplayLayout::new(vec![info(
            1,
            DisplayRect::new(0.0, 0.0, 1512.0, 982.0),
            1.0,
            true,
            true,
        )]);
        assert_eq!(
            plan(&config(28, 14), Some(&one), Some(PRIMARY)),
            plan(&config(28, 14), Some(&laptop()), Some(PRIMARY)),
        );
    }

    #[test]
    fn the_primary_is_screens_zero_even_when_it_is_not_leftmost_or_main() {
        // The external display sits left of the primary (negative x) and is the
        // key-window `mainScreen`; the built-in is `screens[0]`.
        let layout = DisplayLayout::new(vec![
            info(1, DisplayRect::new(0.0, 0.0, 1512.0, 982.0), 2.0, true, false),
            info(
                2,
                DisplayRect::new(-2560.0, 0.0, 2560.0, 1440.0),
                1.0,
                false,
                true,
            ),
        ]);
        let Desired::Panel { display, frame, .. } =
            plan(&config(28, 14), Some(&layout), Some(PRIMARY))
        else {
            panic!("expected a panel");
        };
        assert_eq!(display, PRIMARY);
        assert_eq!(frame, PanelFrame {
            x: 14,
            y: 14,
            width: 1484,
            height: 28
        });
    }

    #[test]
    fn a_primary_listed_second_still_wins_over_the_first_listed_display() {
        let layout = DisplayLayout::new(vec![
            info(
                2,
                DisplayRect::new(-1920.0, -100.0, 1920.0, 1080.0),
                1.0,
                false,
                true,
            ),
            info(1, DisplayRect::new(0.0, 0.0, 1440.0, 900.0), 2.0, true, false),
        ]);
        let Desired::Panel { frame, .. } = plan(&config(30, 10), Some(&layout), Some(PRIMARY))
        else {
            panic!("expected a panel");
        };
        assert_eq!(frame, PanelFrame {
            x: 10,
            y: 10,
            width: 1420,
            height: 30
        });
    }

    #[test]
    fn a_primary_with_a_nonzero_appkit_origin_is_converted_to_the_top_left_space() {
        // AppKit y is bottom-left: with primary_height 900 a frame at y 100
        // has its top at 900 - (100 + 900) = -100 in the top-left space.
        // (A real primary sits at the origin; this proves the conversion.)
        let layout = DisplayLayout::new(vec![info(
            1,
            DisplayRect::new(5.0, 100.0, 1000.0, 900.0),
            1.0,
            true,
            true,
        )]);
        let Desired::Panel { frame, .. } = plan(&config(20, 4), Some(&layout), Some(PRIMARY))
        else {
            panic!("expected a panel");
        };
        assert_eq!((frame.x, frame.y), (9, -96));
    }

    #[test]
    fn fractional_edges_round_half_away_from_zero_deterministically() {
        let layout = DisplayLayout::new(vec![info(
            1,
            DisplayRect::new(-0.5, 0.0, 1000.5, 900.0),
            1.0,
            true,
            true,
        )]);
        let Desired::Panel { frame, .. } = plan(&config(20, 4), Some(&layout), Some(PRIMARY))
        else {
            panic!("expected a panel");
        };
        // -0.5 rounds to -1, 1000.5 to 1001.
        assert_eq!((frame.x, frame.width), (3, 993));
    }

    #[test]
    fn zero_height_is_a_valid_no_panel_state_even_without_a_display() {
        assert_eq!(
            plan(&config(0, 14), Some(&laptop()), Some(PRIMARY)),
            Desired::ZeroSize
        );
        assert_eq!(plan(&config(0, 14), None, None), Desired::ZeroSize);
    }

    #[test]
    fn a_nonpositive_computed_width_is_the_zero_size_state() {
        // 2 * 756 == 1512 leaves width 0; more padding goes negative.
        assert_eq!(
            plan(&config(28, 756), Some(&laptop()), Some(PRIMARY)),
            Desired::ZeroSize
        );
        assert_eq!(
            plan(&config(28, 900), Some(&laptop()), Some(PRIMARY)),
            Desired::ZeroSize
        );
    }

    #[test]
    fn zero_padding_is_preserved() {
        let Desired::Panel { frame, reservation_top, .. } =
            plan(&config(28, 0), Some(&laptop()), Some(PRIMARY))
        else {
            panic!("expected a panel");
        };
        assert_eq!(frame, PanelFrame {
            x: 0,
            y: 0,
            width: 1512,
            height: 28
        });
        assert!((reservation_top - 28.0).abs() < f64::EPSILON);
    }

    #[test]
    fn no_primary_is_explicitly_unavailable() {
        assert_eq!(plan(&config(28, 14), None, None), Desired::NoPrimary);
        let empty = DisplayLayout::new(Vec::new());
        assert_eq!(plan(&config(28, 14), Some(&empty), None), Desired::NoPrimary);
    }

    #[test]
    fn dimensions_above_the_window_limit_are_invalid_not_clamped() {
        let wide = DisplayLayout::new(vec![info(
            1,
            DisplayRect::new(0.0, 0.0, 20_000.0, 900.0),
            1.0,
            true,
            true,
        )]);
        assert!(matches!(
            plan(&config(28, 14), Some(&wide), Some(PRIMARY)),
            Desired::Invalid(_)
        ));
        // The widest accepted panel is exactly the limit.
        let edge = DisplayLayout::new(vec![info(
            1,
            DisplayRect::new(0.0, 0.0, 8192.0, 900.0),
            1.0,
            true,
            true,
        )]);
        let Desired::Panel { frame, .. } = plan(&config(28, 0), Some(&edge), Some(PRIMARY)) else {
            panic!("expected a panel");
        };
        assert_eq!(frame.width, 8192);
        // The tallest configurable bar still fits the dimension limit check.
        assert!(matches!(
            plan(&config(u16::MAX, 0), Some(&edge), Some(PRIMARY)),
            Desired::Invalid(_)
        ));
    }

    #[test]
    fn non_finite_geometry_is_invalid() {
        let layout = DisplayLayout::new(vec![info(
            1,
            DisplayRect::new(f64::NAN, 0.0, 100.0, 100.0),
            1.0,
            true,
            true,
        )]);
        assert!(matches!(
            plan(&config(10, 1), Some(&layout), Some(PRIMARY)),
            Desired::Invalid(_)
        ));
    }

    #[test]
    fn the_reservation_is_the_constant_height_plus_padding_sum() {
        let Desired::Panel { reservation_top, .. } =
            plan(&config(100, 50), Some(&laptop()), Some(PRIMARY))
        else {
            panic!("expected a panel");
        };
        assert!((reservation_top - 150.0).abs() < f64::EPSILON);
    }
}
