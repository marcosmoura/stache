//! Pure overlay geometry: coordinates, outset, corner radius and scale.

use super::{
    BorderAppearance, BorderPaint, BorderPlacement, MAX_BORDER_WIDTH, OVERLAY_PADDING, OverlayRect,
    OverlayScreen,
};

const fn finite(value: f64) -> f64 { if value.is_finite() { value } else { 0.0 } }

/// Converts a global top-left frame to `AppKit`'s bottom-left coordinates.
///
/// `primary` is the primary screen's `AppKit` frame (`NSScreen.screens[0]`). The
/// top-left space has its origin at the primary screen's top-left corner and
/// y grows downward, so `y' = primary.y + primary.height - (y + height)`.
#[must_use]
pub fn appkit_frame_from_top_left(frame: OverlayRect, primary: OverlayRect) -> OverlayRect {
    OverlayRect {
        x: primary.x + frame.x,
        y: primary.y + primary.height - (frame.y + frame.height),
        width: frame.width,
        height: frame.height,
    }
}

/// Grows a frame by `width + padding` on every side.
///
/// Width is clamped to at most [`MAX_BORDER_WIDTH`]; padding to `>= 0`. The
/// result is symmetric, so it is valid in either coordinate space.
#[must_use]
pub fn outset_frame(frame: OverlayRect, width: f64, padding: f64) -> OverlayRect {
    let outset = clamp_width(width) + finite(padding).max(0.0);
    OverlayRect {
        x: frame.x - outset,
        y: frame.y - outset,
        width: frame.width.max(0.0) + 2.0 * outset,
        height: frame.height.max(0.0) + 2.0 * outset,
    }
}

const fn clamp_width(width: f64) -> f64 { finite(width).clamp(0.0, MAX_BORDER_WIDTH) }

/// Selects the inner-edge corner radius in points.
///
/// Borders are always round (square corners are not supported). Uses the
/// queried radius when the symbol resolved (a reported 0 is a real square
/// window), else `fallback`. Negative or non-finite answers also use `fallback`.
#[must_use]
pub fn select_corner_radius(resolved: Option<f64>, fallback: f64) -> f64 {
    match resolved {
        Some(radius) if radius.is_finite() && radius >= 0.0 => radius,
        _ => finite(fallback).max(0.0),
    }
}

/// Provisional per-OS fallback corner radius in points.
///
/// UNVERIFIED until the native gate measures real windows (Ghostty, Spotify):
/// macOS 26 windows are rounder than earlier releases, so 26 and later use a
/// larger value. These numbers are placeholders, not measurements.
#[must_use]
pub const fn fallback_corner_radius(os_major: u32) -> f64 {
    if os_major >= 26 { 16.0 } else { 10.0 }
}

/// The layer `contentsScale`: the display factor when `hidpi`, else 1.0.
#[must_use]
pub fn scale_for_overlay(hidpi: bool, display_scale: f64) -> f64 {
    if hidpi && display_scale.is_finite() && display_scale >= 1.0 {
        display_scale
    } else {
        1.0
    }
}

/// The backing scale of the screen holding most of `frame` (`AppKit` coordinates).
///
/// Ties and no overlap use the first screen (the primary). `None` when there
/// are no screens.
#[must_use]
pub fn display_scale_for_frame(frame: OverlayRect, screens: &[OverlayScreen]) -> Option<f64> {
    let overlap = |screen: &OverlayScreen| {
        let width = (frame.x + frame.width).min(screen.frame.x + screen.frame.width)
            - frame.x.max(screen.frame.x);
        let height = (frame.y + frame.height).min(screen.frame.y + screen.frame.height)
            - frame.y.max(screen.frame.y);
        width.max(0.0) * height.max(0.0)
    };
    let mut best: Option<(&OverlayScreen, f64)> = None;
    for screen in screens {
        let area = overlap(screen);
        if best.is_none_or(|(_, top)| area > top) {
            best = Some((screen, area));
        }
    }
    best.map(|(screen, _)| screen.scale)
}

/// The fraction of the configured width that is visible outside the target.
///
/// Calibration history (the user's comparison with another border tool at the
/// same width): native run 1 drew the full width and looked 1.5 times too thick;
/// native run 2 drew two thirds and looked too thin; the user then chose five
/// sixths. The remaining sixth lies under the target window for
/// [`BorderPlacement::Below`] (and is simply not drawn for `Above`).
pub const VISIBLE_FRACTION: f64 = 5.0 / 6.0;

/// Extra points added to the target's corner radius for the ring's edge at
/// the window boundary with [`BorderPlacement::Below`] (user decision, native run 1).
pub const INNER_RADIUS_ADJUSTMENT: f64 = 1.0;

/// Extra points added to the target's corner radius for the ring's inner edge
/// with [`BorderPlacement::Above`]: one more than below (user decision, native run 11).
pub const ABOVE_INNER_RADIUS_ADJUSTMENT: f64 = 2.0;

/// Everything a host needs to draw one overlay, in plain data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayPlan {
    /// Overlay window frame, `AppKit` coordinates.
    pub panel_frame: OverlayRect,
    /// Outer edge of the stroke in the panel's local space.
    pub outer: OverlayRect,
    /// Inner edge of the stroke in the panel's local space: the target frame
    /// for `Above`, the target inset by the hidden sixth for `Below`.
    pub inner: OverlayRect,
    /// The target's frame in the panel's local space.
    pub target: OverlayRect,
    /// Corner radius of the ring's edge at the target frame (`radius + 1`
    /// below, `radius + 2` above, 0 for a square target); the glow clip uses it.
    pub target_radius: f64,
    /// Inner corner radius.
    pub inner_radius: f64,
    /// Outer corner radius (concentric with the inner edge).
    pub outer_radius: f64,
    /// The configured stroke width (clamped); drives the glow blur.
    pub width: f64,
    /// Layer `contentsScale`.
    pub scale: f64,
    /// What fills the ring.
    pub paint: BorderPaint,
    /// Whether anything is drawn (width greater than zero).
    pub visible: bool,
}

/// Plans one overlay from a top-left target frame.
///
/// `resolved_radius` is the queried radius (`None` when unavailable) and
/// `fallback_radius` the per-OS constant. `screens[0]` is the primary screen.
///
/// With `width` w, `visible = w * VISIBLE_FRACTION`, `hidden = w - visible`
/// and target radius `r` (resolved, fallback or explicit, clamped to half the
/// shorter side; 0 stays 0 and gets no adjustment), `e = r + 1` for `Below`
/// and `e = r + 2` for `Above`:
///
/// - the outer edge is the target outset by `visible`, radius `e + visible`;
/// - `Above`: the inner edge is the target frame, radius `e`;
/// - `Below`: the inner edge is the target inset by `hidden` (clamped to half
///   the shorter side), radius `max(e - hidden, 0)`. It is concentric with
///   the `e` curve at the window boundary, so the window, which covers the
///   hidden part, meets the ring without a gap at the corners.
///
/// The panel margin [`OVERLAY_PADDING`] (plus the glow radius) is transparent
/// space around the ring, not a visible gap.
#[must_use]
pub fn plan_overlay(
    target: OverlayRect,
    appearance: &BorderAppearance,
    resolved_radius: Option<f64>,
    fallback_radius: f64,
    screens: &[OverlayScreen],
) -> OverlayPlan {
    let width = clamp_width(appearance.width);
    let visible_width = width * VISIBLE_FRACTION;
    let target = OverlayRect {
        x: finite(target.x),
        y: finite(target.y),
        width: finite(target.width).max(0.0),
        height: finite(target.height).max(0.0),
    };
    let primary = screens.first().map_or_default(|screen| screen.frame);
    let glow = match appearance.paint {
        BorderPaint::Glow(_) if width > 0.0 => glow_radius(width),
        _ => 0.0,
    };
    let edge = OVERLAY_PADDING + glow;
    let panel_top_left = outset_frame(target, visible_width, edge);
    let panel_frame = appkit_frame_from_top_left(panel_top_left, primary);
    let display = display_scale_for_frame(panel_frame, screens).unwrap_or(1.0);

    let margin = visible_width + edge.max(0.0);
    let local_target = OverlayRect::new(margin, margin, target.width, target.height);
    let outer = OverlayRect::new(
        edge,
        edge,
        2.0f64.mul_add(visible_width, target.width),
        2.0f64.mul_add(visible_width, target.height),
    );
    let half_short = target.width.min(target.height) / 2.0;
    let radius = appearance
        .corner_radius
        .filter(|radius| radius.is_finite() && *radius >= 0.0)
        .unwrap_or_else(|| select_corner_radius(resolved_radius, fallback_radius))
        .min(half_short);
    let adjustment = match appearance.placement {
        BorderPlacement::Below => INNER_RADIUS_ADJUSTMENT,
        BorderPlacement::Above => ABOVE_INNER_RADIUS_ADJUSTMENT,
    };
    let target_radius = if radius > 0.0 {
        radius + adjustment
    } else {
        0.0
    };
    let (inner, inner_radius) = match appearance.placement {
        BorderPlacement::Above => (local_target, target_radius.min(half_short)),
        BorderPlacement::Below => {
            let inward = (width - visible_width).min(half_short);
            let inner = OverlayRect::new(
                local_target.x + inward,
                local_target.y + inward,
                2.0f64.mul_add(-inward, target.width),
                2.0f64.mul_add(-inward, target.height),
            );
            let radius = (target_radius - inward).max(0.0).min(inner.width.min(inner.height) / 2.0);
            (inner, radius)
        }
    };
    let outer_radius = if target_radius > 0.0 {
        (target_radius + visible_width).min(outer.width.min(outer.height) / 2.0)
    } else {
        0.0
    };
    OverlayPlan {
        panel_frame,
        outer,
        inner,
        target: local_target,
        target_radius: target_radius.min(half_short),
        inner_radius,
        outer_radius,
        width,
        scale: scale_for_overlay(appearance.hidpi, display),
        paint: appearance.paint,
        visible: width > 0.0,
    }
}

/// The ring drawn at `fraction` (0 to 1) of the configured width, in the ring
/// layer coordinates of the full-width plan `full` (part 09, focus width
/// animation).
///
/// The ring is re-planned with `plan_overlay` at the scaled width, so the
/// placement (below or above), the visible fraction and the corner rules apply
/// at every width, then translated so its target frame sits where the full
/// plan's does. At `fraction` 0 the outer edge equals the inner edge (the
/// target frame for either placement) and the even-odd ring draws nothing;
/// both edges keep the target's corner radius, so the path has the same
/// elements as a full rounded ring and Core Animation can interpolate it.
#[must_use]
pub fn ring_at_fraction(
    full: &OverlayPlan,
    target: OverlayRect,
    appearance: &BorderAppearance,
    radii: (Option<f64>, f64),
    screens: &[OverlayScreen],
    fraction: f64,
) -> super::RingShape {
    let fraction = finite(fraction).clamp(0.0, 1.0);
    let scaled = BorderAppearance {
        width: full.width * fraction,
        ..*appearance
    };
    let part = plan_overlay(target, &scaled, radii.0, radii.1, screens);
    let dx = full.target.x - part.target.x - full.outer.x;
    let dy = full.target.y - part.target.y - full.outer.y;
    let shift = |rect: OverlayRect| OverlayRect {
        x: rect.x + dx,
        y: rect.y + dy,
        ..rect
    };
    super::RingShape {
        outer: shift(part.outer),
        outer_radius: part.outer_radius,
        inner: shift(part.inner),
        inner_radius: part.inner_radius,
    }
}

/// The transparent margin a glow overlay adds around the ring, in points:
/// [`GLOW_RADIUS_FACTOR`] times the width.
///
/// The soft band is the shadow of a core that reaches [`glow_core`] past the
/// window edge, blurred by [`glow_blur`]. Core Graphics blurs with a Gaussian
/// of sigma about half the blur, so the band is about 2 % alpha at twice the
/// width and gone (3 sigma) at `0.5w + 2.25w = 2.75w`; the 3w margin keeps the
/// panel from cutting it.
#[must_use]
pub fn glow_radius(width: f64) -> f64 { clamp_width(width) * GLOW_RADIUS_FACTOR }

/// The glow band's shadow blur in points: [`GLOW_BLUR_FACTOR`] times the width.
#[must_use]
pub fn glow_blur(width: f64) -> f64 { clamp_width(width) * GLOW_BLUR_FACTOR }

/// How far the glow's invisible core reaches outside the window edge, in
/// points: [`GLOW_CORE_FACTOR`] times the width. The window edge then lies
/// inside the core, so the band is brightest right at the window.
#[must_use]
pub fn glow_core(width: f64) -> f64 { clamp_width(width) * GLOW_CORE_FACTOR }

/// Glow panel margin per point of width (see [`glow_radius`]); must stay at
/// least `GLOW_CORE_FACTOR + 1.5 × GLOW_BLUR_FACTOR` (core plus 3 sigma).
pub const GLOW_RADIUS_FACTOR: f64 = 3.0;

/// Glow blur per point of width (native run 12: a soft band, no crisp ring).
/// With two passes ([`super::GLOW_PASSES`]) a 6 pt glow is about 94 % at the
/// window edge, 75 % at 3 pt, 47 % at 6 pt, 13 % at 9 pt and 4 % at 12 pt: a
/// 6–8 pt band fading out by twice the width. Unverified natively.
pub const GLOW_BLUR_FACTOR: f64 = 1.5;

/// Glow core reach per point of width (see [`glow_core`]).
pub const GLOW_CORE_FACTOR: f64 = 0.5;

/// A point in a layer's unit coordinate space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UnitPoint {
    /// Horizontal, 0 left to 1 right.
    pub x: f64,
    /// Vertical, 0 bottom to 1 top (see [`gradient_points`]).
    pub y: f64,
}

/// The `CAGradientLayer` `startPoint` and `endPoint` that draw a two-stop
/// gradient exactly along `angle` degrees on a layer of `size` points.
///
/// Convention (spec part 02, "Gradient direction"): `angle` is normalized to
/// `[0, 360)` (non-finite is 0); the gradient runs along `(cos a, sin a)` with
/// x to the right and y **up**, so 0 is left to right and 90 is bottom to top.
/// The `from` color sits exactly on the isoline through the corner furthest
/// against the direction, `to` on the isoline through the opposite corner,
/// and isolines are perpendicular to the direction in points (the CSS
/// `linear-gradient` corner rule).
///
/// A `CAGradientLayer` evaluates the gradient in the unit square and then
/// stretches it to the layer, so on a non-square layer a unit-space
/// perpendicular is skewed. Compensation: with `M = diag(w, h)`, the unit-space
/// gradient vector is `M * d`, which makes the point-space gradient direction
/// `M^-1 * (end - start)` equal to `d`. Points may lie outside `[0, 1]`.
/// That the layer evaluates the gradient in unit space (and so needs this
/// compensation) is also unverified until the native gate.
///
/// ORIENTATION ASSUMPTION, UNVERIFIED until the native gate: the layer is a
/// sublayer of a layer-backed `NSView`'s layer, which is not flipped, so unit
/// y grows upward (origin bottom-left). If the gate shows the opposite, only
/// the y coordinates here change.
#[must_use]
pub fn gradient_points(size: (f64, f64), angle: f64) -> (UnitPoint, UnitPoint) {
    let width = finite(size.0).max(0.0);
    let height = finite(size.1).max(0.0);
    let degrees = if angle.is_finite() {
        angle.rem_euclid(360.0)
    } else {
        0.0
    };
    let (sin, cos) = degrees.to_radians().sin_cos();
    // Unit-space gradient vector `M * d`.
    let (ux, uy) = (width * cos, height * sin);
    let squared = ux.mul_add(ux, uy * uy);
    if squared < f64::EPSILON {
        return (UnitPoint { x: 0.0, y: 0.5 }, UnitPoint { x: 1.0, y: 0.5 });
    }
    // Distance from the center to the extreme corners along the vector is
    // `(|ux| + |uy|) / 2` in `u`-units; scale so the stops land on them.
    let k = ux.abs().midpoint(uy.abs()) / squared;
    (
        UnitPoint {
            x: ux.mul_add(-k, 0.5),
            y: uy.mul_add(-k, 0.5),
        },
        UnitPoint {
            x: ux.mul_add(k, 0.5),
            y: uy.mul_add(k, 0.5),
        },
    )
}
