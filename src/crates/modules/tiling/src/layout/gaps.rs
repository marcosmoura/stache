//! Inner and outer gaps, and their resolution for one display.

use crate::config::{GapValue, GapsConfig, GapsConfigValue};
use crate::geometry::GeometryError;

/// Gap values in logical points.
///
/// Inner gaps separate windows; outer gaps separate windows from the display
/// edges. Outer gaps are applied once, by [`crate::usable_area`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Gaps {
    /// Horizontal gap between windows.
    pub inner_h: f64,
    /// Vertical gap between windows.
    pub inner_v: f64,
    /// Gap from the top edge.
    pub outer_top: f64,
    /// Gap from the right edge.
    pub outer_right: f64,
    /// Gap from the bottom edge.
    pub outer_bottom: f64,
    /// Gap from the left edge.
    pub outer_left: f64,
}

/// The display a gap configuration is resolved for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GapDisplay<'a> {
    /// The display name, matched ASCII case-insensitively like legacy.
    pub name: &'a str,
    /// Whether this is the main display.
    pub is_main: bool,
}

impl Gaps {
    /// No gaps at all.
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);

    /// Creates gaps from explicit values.
    #[must_use]
    pub const fn new(
        inner_h: f64,
        inner_v: f64,
        outer_top: f64,
        outer_right: f64,
        outer_bottom: f64,
        outer_left: f64,
    ) -> Self {
        Self {
            inner_h,
            inner_v,
            outer_top,
            outer_right,
            outer_bottom,
            outer_left,
        }
    }

    /// The same inner value on both axes and the same outer value on all sides.
    #[must_use]
    pub const fn uniform(inner: f64, outer: f64) -> Self {
        Self::new(inner, inner, outer, outer, outer, outer)
    }

    /// Resolves the gaps that apply to `display`.
    ///
    /// Mirrors legacy `layout/gaps.rs:142-169`. For per-display configuration
    /// the first entry whose `screen` matches the display name
    /// (case-insensitively) **or** is an applicable alias (`main`/`primary` on
    /// the main display, `secondary` on any other) wins, so a later exact name
    /// never outranks an earlier matching alias. With no match the first entry
    /// applies, and an empty list yields zero gaps.
    #[must_use]
    pub fn from_config(config: &GapsConfigValue, display: GapDisplay<'_>) -> Self {
        match config {
            GapsConfigValue::Global(gaps) => Self::from_pair(&gaps.inner, &gaps.outer),
            GapsConfigValue::PerScreen(screens) => {
                let matched = screens.iter().find(|entry| {
                    entry.screen.eq_ignore_ascii_case(display.name)
                        || ((entry.screen.eq_ignore_ascii_case("main")
                            || entry.screen.eq_ignore_ascii_case("primary"))
                            && display.is_main)
                        || (entry.screen.eq_ignore_ascii_case("secondary") && !display.is_main)
                });
                matched
                    .or_else(|| screens.first())
                    .map_or(Self::ZERO, |entry| Self::from_pair(&entry.inner, &entry.outer))
            }
        }
    }

    /// Resolves a global [`GapsConfig`].
    #[must_use]
    pub fn from_gaps_config(config: &GapsConfig) -> Self {
        Self::from_pair(&config.inner, &config.outer)
    }

    fn from_pair(inner: &GapValue, outer: &GapValue) -> Self {
        let (inner_h, inner_v) = inner_pair(inner);
        let (top, right, bottom, left) = outer_quad(outer);
        Self::new(inner_h, inner_v, top, right, bottom, left)
    }

    /// Checks that every gap is finite and non-negative.
    ///
    /// # Errors
    ///
    /// Returns [`GeometryError`] naming the problem.
    pub fn validate(&self) -> Result<(), GeometryError> {
        for value in [
            self.inner_h,
            self.inner_v,
            self.outer_top,
            self.outer_right,
            self.outer_bottom,
            self.outer_left,
        ] {
            if !value.is_finite() {
                return Err(GeometryError::NonFinite { what: "gaps" });
            }
            if value < 0.0 {
                return Err(GeometryError::Negative { what: "gaps" });
            }
        }
        Ok(())
    }
}

/// Inner gap as (horizontal, vertical). Per-side values average opposite sides
/// with integer division, as legacy `GapValue::as_inner` does.
fn inner_pair(value: &GapValue) -> (f64, f64) {
    match value {
        GapValue::Uniform(v) => (f64::from(*v), f64::from(*v)),
        GapValue::PerAxis { horizontal, vertical } => {
            (f64::from(*horizontal), f64::from(*vertical))
        }
        GapValue::PerSide { top, right, bottom, left } => (
            // Widened to u64 so the sum cannot overflow; the result is at most
            // u32::MAX and converts exactly.
            half_sum(*left, *right),
            half_sum(*top, *bottom),
        ),
    }
}

#[allow(clippy::cast_precision_loss)] // the value is at most u32::MAX, exact in f64
fn half_sum(a: u32, b: u32) -> f64 { u64::midpoint(u64::from(a), u64::from(b)) as f64 }

/// Outer gap as (top, right, bottom, left).
fn outer_quad(value: &GapValue) -> (f64, f64, f64, f64) {
    match value {
        GapValue::Uniform(v) => {
            let v = f64::from(*v);
            (v, v, v, v)
        }
        GapValue::PerAxis { horizontal, vertical } => {
            let (h, v) = (f64::from(*horizontal), f64::from(*vertical));
            (v, h, v, h)
        }
        GapValue::PerSide { top, right, bottom, left } => (
            f64::from(*top),
            f64::from(*right),
            f64::from(*bottom),
            f64::from(*left),
        ),
    }
}
