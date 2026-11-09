//! Pure logical-point geometry: rectangles, insets and the usable display area.
//!
//! Every value is a finite `f64` in logical points. Nothing here talks to the
//! operating system: callers hand in full logical display bounds, the native
//! visible-frame insets and any host reservation insets, and receive either a
//! usable rectangle or an explicit [`GeometryError`]. No function produces NaN
//! frames or panics.

use crate::layout::Gaps;

/// An axis-aligned rectangle in logical points. The origin may be negative.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl Rect {
    /// Creates a rectangle without validating it.
    #[must_use]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self { x, y, width, height }
    }

    /// Whether all four components are finite.
    #[must_use]
    pub const fn is_finite(&self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
    }

    /// Whether the rectangle is finite with strictly positive width and height.
    #[must_use]
    pub fn has_area(&self) -> bool { self.is_finite() && self.width > 0.0 && self.height > 0.0 }

    /// The right edge, `x + width`.
    #[must_use]
    pub fn max_x(&self) -> f64 { self.x + self.width }

    /// The bottom edge, `y + height`.
    #[must_use]
    pub fn max_y(&self) -> f64 { self.y + self.height }
}

/// Per-edge insets measured inward from a display edge, in logical points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    /// Top inset.
    pub top: f64,
    /// Right inset.
    pub right: f64,
    /// Bottom inset.
    pub bottom: f64,
    /// Left inset.
    pub left: f64,
}

impl Insets {
    /// No inset on any edge. A host that reserves nothing reports this.
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0, 0.0);

    /// Creates insets without validating them.
    #[must_use]
    pub const fn new(top: f64, right: f64, bottom: f64, left: f64) -> Self {
        Self { top, right, bottom, left }
    }

    fn validate(&self, what: &'static str) -> Result<(), GeometryError> {
        for value in [self.top, self.right, self.bottom, self.left] {
            if !value.is_finite() {
                return Err(GeometryError::NonFinite { what });
            }
            if value < 0.0 {
                return Err(GeometryError::Negative { what });
            }
        }
        Ok(())
    }
}

/// Why geometry could not be produced. These are explicit infeasible outcomes,
/// never silent clamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryError {
    /// An input was NaN or infinite.
    NonFinite {
        /// The offending input.
        what: &'static str,
    },
    /// An input that must be non-negative was negative.
    Negative {
        /// The offending input.
        what: &'static str,
    },
    /// Insets and gaps leave no positive usable width or height.
    NoUsableArea,
    /// A computed frame had no positive finite area (for example gaps larger
    /// than the cell they separate).
    DegenerateFrame,
}

/// Computes the usable area of one display.
///
/// Per edge the exclusion is `max(native inset, host reservation)`, and the
/// display's own outer gap is then added exactly once. The reservation is never
/// added to an already inset visible frame, so top 0 / native 24 /
/// reservation 40 / outer 6 yields an exclusion of 46, not 70.
///
/// `bounds` is the full logical display frame.
///
/// # Errors
///
/// Returns [`GeometryError`] for non-finite or negative inputs and when nothing
/// remains usable.
pub fn usable_area(
    bounds: Rect,
    native: Insets,
    reservation: Insets,
    outer: &Gaps,
) -> Result<Rect, GeometryError> {
    if !bounds.is_finite() {
        return Err(GeometryError::NonFinite { what: "display bounds" });
    }
    native.validate("native insets")?;
    reservation.validate("host reservation")?;
    outer.validate()?;

    let top = native.top.max(reservation.top) + outer.outer_top;
    let right = native.right.max(reservation.right) + outer.outer_right;
    let bottom = native.bottom.max(reservation.bottom) + outer.outer_bottom;
    let left = native.left.max(reservation.left) + outer.outer_left;

    let usable = Rect::new(
        bounds.x + left,
        bounds.y + top,
        bounds.width - left - right,
        bounds.height - top - bottom,
    );
    if !usable.is_finite() {
        return Err(GeometryError::NonFinite { what: "usable area" });
    }
    if usable.width <= 0.0 || usable.height <= 0.0 {
        return Err(GeometryError::NoUsableArea);
    }
    Ok(usable)
}
