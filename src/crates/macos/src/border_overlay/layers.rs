//! Pure description of one overlay's layer tree.
//!
//! The native host applies a [`LayerSpec`] to real Core Animation layers and
//! the fake records it, so tests assert exactly what the native side is told.

use super::geometry::{UnitPoint, glow_blur, glow_core, gradient_points};
use super::{BorderColor, BorderPaint, OverlayPlan, OverlayRect};

/// What the visible layer draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FillSpec {
    /// Nothing is drawn (width 0); the overlay is also ordered out.
    None,
    /// The ring layer filled with one color.
    Solid(BorderColor),
    /// A gradient layer masked by the ring shape, stops at 0 and 1.
    Gradient {
        /// Color at stop 0.
        from: BorderColor,
        /// Color at stop 1.
        to: BorderColor,
        /// `startPoint` in the gradient layer's unit space (y up, unverified).
        start: UnitPoint,
        /// `endPoint` in the gradient layer's unit space.
        end: UnitPoint,
    },
    /// A soft band of light and no crisp ring: only the glow layer shows, its
    /// contents a pre-rendered bitmap (see [`GlowRender`]), brightest at the
    /// window edge and fading outward.
    Glow {
        /// Band color.
        color: BorderColor,
        /// Shadow blur in points ([`super::glow_blur`]).
        blur: f64,
        /// How far the invisible core reaches outside the target, in points
        /// ([`super::glow_core`]).
        core: f64,
    },
}

/// The clip that keeps a glow outside the target frame (for either placement): the container layer is
/// masked by an even-odd path of its bounds minus this rounded hole.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipSpec {
    /// The hole (the target frame) in panel coordinates.
    pub hole: OverlayRect,
    /// The hole's corner radius.
    pub radius: f64,
}

/// One overlay's layer tree as plain data.
///
/// Layers: a container (bounds `bounds`, masked by `clip` when present) holding
/// the glow bitmap layer (framed at `bounds`), the ring shape layer and the
/// gradient layer (both framed at `ring_frame`). Exactly the layer named by
/// `fill` is visible; a glow shows only the glow layer (the ring stays hidden).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerSpec {
    /// Panel bounds, origin zero.
    pub bounds: OverlayRect,
    /// Layer `contentsScale`.
    pub scale: f64,
    /// Frame of the ring and gradient layers in panel coordinates.
    pub ring_frame: OverlayRect,
    /// Outer edge of the ring in `ring_frame` coordinates.
    pub outer: OverlayRect,
    /// Outer corner radius.
    pub outer_radius: f64,
    /// Inner edge of the ring in `ring_frame` coordinates.
    pub inner: OverlayRect,
    /// Inner corner radius.
    pub inner_radius: f64,
    /// The visible fill.
    pub fill: FillSpec,
    /// Container mask for glow, else none.
    pub clip: Option<ClipSpec>,
}

/// How many times the core's shadow is drawn into the glow bitmap.
///
/// Passes stack by source-over: with a single-pass alpha `a` the band reaches
/// `1 - (1 - a)^n`. Two passes take the window edge from about 0.75 to 0.94
/// and the point one width out from about 0.27 to 0.47, so the band reads as
/// clearly lit instead of a faint shadow. Unverified until a native run.
pub const GLOW_PASSES: u32 = 2;

/// Everything that decides the pixels of a glow bitmap.
///
/// The bitmap is a small 9-slice tile, not the panel: a synthetic target
/// whose straight edges are just long enough to hold both corners and the
/// soft band around them, surrounded by the panel's margin. The glow layer
/// covers the whole panel and stretches the tile with `contentsCenter`
/// ([`Self::center`]): corners keep their pixels, the edge strips stretch
/// along the edge (the band there varies only across it) and the transparent
/// interior stretches over the window. So the tile depends on the width,
/// corner radius, color and scale, never on the window size or position:
/// moves and resizes (live resize, animated tiling frames) never re-render.
///
/// Tile geometry (points): margin `m` (the panel margin around the target),
/// corner reach `k = radius + core + 2 × blur`, interior side
/// `n = ceil(2m + 2k + 2) - 2m` (so the tile side `2m + n` is whole points
/// and the stretched center strip `n - 2k` is at least 2 pt), tile side
/// `2m + n`. A target narrower or shorter than `n` (a tiny window) cannot be
/// sliced: the bitmap then covers the panel itself (as before, re-rendered
/// when its size changes) with the full unit `contentsCenter`.
///
/// The host renders the bitmap (bounds × scale) as shadow only: the filled
/// `core` rounded rect is drawn far outside the bitmap [`GLOW_PASSES`] times
/// under a same-colored shadow of blur `blur × scale` whose offset brings only
/// the shadow back onto `core`, clipped to the outside of `clip`. No crisp
/// edge is drawn anywhere; it sets the result as the glow layer's contents.
/// It re-renders only when this value changes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlowRender {
    /// Bitmap bounds, origin zero: the tile (or the panel for a tiny target).
    pub bounds: OverlayRect,
    /// Bitmap scale (`contentsScale`).
    pub scale: f64,
    /// Band color.
    pub color: BorderColor,
    /// Shadow blur in points.
    pub blur: f64,
    /// The invisible core: the hole outset by the core distance, in bitmap
    /// coordinates. Its shadow is the band.
    pub core: OverlayRect,
    /// Core corner radius (hole radius plus the core distance).
    pub core_radius: f64,
    /// Nothing is drawn inside this hole (bitmap coordinates).
    pub clip: ClipSpec,
    /// The glow layer's `contentsCenter` in unit coordinates of the bitmap:
    /// a centered strip for a tile, the whole unit rect for a panel bitmap.
    /// Symmetric, so it does not depend on the layer's y direction.
    pub center: OverlayRect,
}

/// The tile interior side (points) for a margin, corner radius, core and
/// blur; `glow_render` falls back to a panel bitmap when the target is smaller.
fn glow_tile_interior(margin: f64, radius: f64, core: f64, blur: f64) -> f64 {
    let reach = 2.0f64.mul_add(blur, radius + core);
    2.0f64.mul_add(
        -margin,
        2.0f64.mul_add(margin, 2.0f64.mul_add(reach, 2.0)).ceil(),
    )
}

/// The ring's two edges in the ring layer's coordinates.
///
/// This is what the ring layer's and the gradient mask's `path` draw
/// (even-odd). A focus width animation
/// (part 09) moves between two of these; with `outer == inner` nothing is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingShape {
    /// Outer edge.
    pub outer: OverlayRect,
    /// Outer corner radius.
    pub outer_radius: f64,
    /// Inner edge.
    pub inner: OverlayRect,
    /// Inner corner radius.
    pub inner_radius: f64,
}

impl LayerSpec {
    /// The layers this spec shows: the ring for solid, the gradient for a
    /// gradient, only the glow layer for a glow (it has no crisp ring).
    #[must_use]
    pub const fn shown_layers(&self) -> &'static [super::AnimLayer] {
        match self.fill {
            FillSpec::None => &[],
            FillSpec::Solid(_) => &[super::AnimLayer::Ring],
            FillSpec::Gradient { .. } => &[super::AnimLayer::Gradient],
            FillSpec::Glow { .. } => &[super::AnimLayer::Glow],
        }
    }

    /// The ring this spec draws.
    #[must_use]
    pub const fn ring_shape(&self) -> RingShape {
        RingShape {
            outer: self.outer,
            outer_radius: self.outer_radius,
            inner: self.inner,
            inner_radius: self.inner_radius,
        }
    }

    /// The glow bitmap this spec needs, `None` unless the fill is a glow
    /// (see [`GlowRender`] for the tile).
    #[must_use]
    pub fn glow_render(&self) -> Option<GlowRender> {
        let FillSpec::Glow { color, blur, core } = self.fill else {
            return None;
        };
        let clip = self.clip?;
        let hole = clip.hole;
        let margin = hole.x;
        let interior = glow_tile_interior(margin, clip.radius, core, blur);
        let tile = hole.width >= interior && hole.height >= interior;
        let (bounds, hole, center) = if tile {
            let side = 2.0f64.mul_add(margin, interior);
            let reach = 2.0f64.mul_add(blur, clip.radius + core);
            let start = (margin + reach) / side;
            let strip = 2.0f64.mul_add(-reach, interior) / side;
            (
                OverlayRect::new(0.0, 0.0, side, side),
                OverlayRect::new(margin, margin, interior, interior),
                OverlayRect::new(start, start, strip, strip),
            )
        } else {
            (self.bounds, hole, OverlayRect::new(0.0, 0.0, 1.0, 1.0))
        };
        Some(GlowRender {
            bounds,
            scale: self.scale,
            color,
            blur,
            core: OverlayRect::new(
                hole.x - core,
                hole.y - core,
                2.0f64.mul_add(core, hole.width),
                2.0f64.mul_add(core, hole.height),
            ),
            core_radius: clip.radius + core,
            clip: ClipSpec { hole, radius: clip.radius },
            center,
        })
    }
}

impl OverlayPlan {
    /// The layer tree this plan draws.
    #[must_use]
    pub fn layer_spec(&self) -> LayerSpec {
        let ring_frame = self.outer;
        let local = |rect: OverlayRect| OverlayRect {
            x: rect.x - ring_frame.x,
            y: rect.y - ring_frame.y,
            ..rect
        };
        let fill = if self.visible {
            match self.paint {
                BorderPaint::Solid(color) => FillSpec::Solid(color),
                BorderPaint::Gradient { from, to, angle } => {
                    let (start, end) =
                        gradient_points((ring_frame.width, ring_frame.height), angle);
                    FillSpec::Gradient { from, to, start, end }
                }
                BorderPaint::Glow(color) => FillSpec::Glow {
                    color,
                    blur: glow_blur(self.width),
                    core: glow_core(self.width),
                },
            }
        } else {
            FillSpec::None
        };
        let clip = (matches!(fill, FillSpec::Glow { .. })).then_some(ClipSpec {
            hole: self.target,
            radius: self.target_radius,
        });
        LayerSpec {
            bounds: OverlayRect::new(0.0, 0.0, self.panel_frame.width, self.panel_frame.height),
            scale: self.scale,
            ring_frame,
            outer: local(self.outer),
            outer_radius: self.outer_radius,
            inner: local(self.inner),
            inner_radius: self.inner_radius,
            fill,
            clip,
        }
    }
}
