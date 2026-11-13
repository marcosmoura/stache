//! Pure placement of the tray popover panel.
//!
//! Every rectangle here is in logical points, top-left origin of the primary
//! display (`NSScreen.screens[0]`), y growing downwards, the same space as
//! [`TopLeftFrame`]. The tray click reports the icon rectangle in physical
//! pixels of the icon's display, so [`locate_icon`] first finds that display
//! by trying each display's scale.

use crate::panel::TopLeftFrame;

/// Distance kept between the panel and the edges of the visible frame, and
/// between the menu bar and the panel's top.
pub const POPOVER_MARGIN: f64 = 8.0;

/// The tray icon rectangle as the tray click reports it: physical pixels of
/// the icon's display, top-left origin of the primary display.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IconAnchor {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

/// One display for placement, in top-left logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PopoverScreen {
    /// The whole display.
    pub frame: TopLeftFrame,
    /// The display without the menu bar and the Dock.
    pub visible: TopLeftFrame,
    /// Backing scale factor (points to pixels).
    pub scale: f64,
}

/// The icon rectangle in logical points and the visible frame of its display.
///
/// The first display (by order) whose scale puts the icon's
/// centre inside its frame wins; with none, the first display and a scale of
/// 1 are assumed. `None` only without any display.
#[must_use]
pub fn locate_icon(
    anchor: IconAnchor,
    screens: &[PopoverScreen],
) -> Option<(TopLeftFrame, TopLeftFrame)> {
    let logical = |scale: f64| {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        TopLeftFrame {
            x: anchor.x / scale,
            y: anchor.y / scale,
            width: anchor.width / scale,
            height: anchor.height / scale,
        }
    };
    screens
        .iter()
        .find_map(|screen| {
            let icon = logical(screen.scale);
            contains(
                screen.frame,
                icon.x + icon.width / 2.0,
                icon.y + icon.height / 2.0,
            )
            .then_some((icon, screen.visible))
        })
        .or_else(|| screens.first().map(|screen| (logical(1.0), screen.visible)))
}

fn contains(frame: TopLeftFrame, x: f64, y: f64) -> bool {
    x >= frame.x && x < frame.x + frame.width && y >= frame.y && y < frame.y + frame.height
}

/// The panel's top-left origin under the icon.
///
/// Horizontally centred on the icon, its top [`POPOVER_MARGIN`] below the menu bar (the lower of the icon's bottom and
/// the visible frame's top), clamped inside the visible frame with the same
/// margin. A frame smaller than the panel pins it to the left and top margins.
#[must_use]
pub fn popover_origin(icon: TopLeftFrame, size: (f64, f64), visible: TopLeftFrame) -> (f64, f64) {
    let (width, height) = size;
    let centred = icon.x + icon.width / 2.0 - width / 2.0;
    let left = visible.x + POPOVER_MARGIN;
    let right = visible.x + visible.width - POPOVER_MARGIN - width;
    let x = centred.min(right).max(left);
    let top = (icon.y + icon.height).max(visible.y) + POPOVER_MARGIN;
    let bottom = visible.y + visible.height - POPOVER_MARGIN - height;
    let y = top.min(bottom).max(visible.y + POPOVER_MARGIN);
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn frame(x: f64, y: f64, width: f64, height: f64) -> TopLeftFrame {
        TopLeftFrame { x, y, width, height }
    }

    /// A 1512×982 built-in display with a 33 pt menu bar.
    const BUILTIN: TopLeftFrame = frame(0.0, 33.0, 1512.0, 949.0);
    const PANEL: (f64, f64) = (480.0, 640.0);

    fn icon_at(x: f64) -> TopLeftFrame { frame(x, 0.0, 24.0, 33.0) }

    #[test]
    fn an_icon_in_the_middle_centres_the_panel_under_it() {
        assert_eq!(popover_origin(icon_at(744.0), PANEL, BUILTIN), (516.0, 41.0));
    }

    #[test]
    fn an_icon_at_the_left_edge_clamps_to_the_left_margin() {
        assert_eq!(popover_origin(icon_at(4.0), PANEL, BUILTIN), (8.0, 41.0));
    }

    #[test]
    fn an_icon_at_the_right_edge_clamps_to_the_right_margin() {
        assert_eq!(popover_origin(icon_at(1480.0), PANEL, BUILTIN), (1024.0, 41.0));
    }

    #[test]
    fn a_secondary_display_with_a_negative_origin_keeps_its_own_frame() {
        // A 1920×1080 display left of and above the primary, 25 pt menu bar.
        let visible = frame(-1920.0, -1080.0 + 25.0, 1920.0, 1055.0);
        let icon = frame(-100.0, -1080.0, 22.0, 25.0);
        assert_eq!(popover_origin(icon, PANEL, visible), (-488.0, -1047.0));
        let left = frame(-1915.0, -1080.0, 22.0, 25.0);
        assert_eq!(popover_origin(left, PANEL, visible).0, -1912.0);
    }

    #[test]
    fn a_frame_smaller_than_the_panel_pins_it_to_the_top_left_margins() {
        let small = frame(100.0, 25.0, 300.0, 400.0);
        assert_eq!(
            popover_origin(frame(250.0, 0.0, 22.0, 25.0), PANEL, small),
            (108.0, 33.0)
        );
    }

    #[test]
    fn the_icon_is_found_on_the_display_its_scale_puts_it_on() {
        let primary = PopoverScreen {
            frame: frame(0.0, 0.0, 1512.0, 982.0),
            visible: BUILTIN,
            scale: 2.0,
        };
        let external = PopoverScreen {
            frame: frame(1512.0, -200.0, 2560.0, 1440.0),
            visible: frame(1512.0, -175.0, 2560.0, 1415.0),
            scale: 1.0,
        };
        let screens = [primary, external];
        // 2 px per point on the built-in display.
        let builtin = IconAnchor {
            x: 2000.0,
            y: 0.0,
            width: 48.0,
            height: 66.0,
        };
        let (icon, visible) = locate_icon(builtin, &screens).expect("located");
        assert_eq!(icon, frame(1000.0, 0.0, 24.0, 33.0));
        assert_eq!(visible, BUILTIN);
        // 1 px per point on the external display (2000 / 2 would be on the
        // built-in, but its centre at y = -188 is above it).
        let outside = IconAnchor {
            x: 3000.0,
            y: -200.0,
            width: 24.0,
            height: 25.0,
        };
        let (icon, visible) = locate_icon(outside, &screens).expect("located");
        assert_eq!((icon.x, icon.y), (3000.0, -200.0));
        assert_eq!(visible, external.visible);
        assert!(locate_icon(outside, &[]).is_none());
    }
}
