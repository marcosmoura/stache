//! Split helpers shared by the layout algorithms.

use crate::geometry::Rect;

/// Splits `frame` left/right at `ratio` with `gap` between the halves.
pub fn split_horizontal(frame: &Rect, ratio: f64, gap: f64) -> (Rect, Rect) {
    let available = frame.width - gap;
    let left_width = available * ratio;
    let right_width = available - left_width;
    (
        Rect::new(frame.x, frame.y, left_width, frame.height),
        Rect::new(frame.x + left_width + gap, frame.y, right_width, frame.height),
    )
}

/// Splits `frame` top/bottom at `ratio` with `gap` between the halves.
pub fn split_vertical(frame: &Rect, ratio: f64, gap: f64) -> (Rect, Rect) {
    let available = frame.height - gap;
    let top_height = available * ratio;
    let bottom_height = available - top_height;
    (
        Rect::new(frame.x, frame.y, frame.width, top_height),
        Rect::new(frame.x, frame.y + top_height + gap, frame.width, bottom_height),
    )
}

/// Whether the usable area is landscape (`width >= height`).
pub fn is_landscape(frame: &Rect) -> bool { frame.width >= frame.height }

/// A count as `f64`. Window counts are tiny, so the conversion is exact.
#[allow(clippy::cast_precision_loss)]
pub const fn count_f64(count: usize) -> f64 { count as f64 }

/// Total inner gap for `count` items along one axis.
pub fn total_gap(gap: f64, count: usize) -> f64 { gap * count_f64(count.saturating_sub(1)) }
