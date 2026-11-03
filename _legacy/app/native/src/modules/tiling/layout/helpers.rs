//! Helper functions for layout calculations.
//!
//! Provides split operations used by multiple layout algorithms.

use crate::modules::tiling::state::Rect;

/// Splits a frame horizontally (left/right) at the given ratio with a gap.
///
/// # Arguments
///
/// * `frame` - The frame to split
/// * `ratio` - Split ratio (0.0-1.0), where ratio is the left portion
/// * `gap` - Gap between the two resulting frames
///
/// # Returns
///
/// A tuple of (`left_frame`, `right_frame`)
#[must_use]
pub fn split_horizontal(frame: &Rect, ratio: f64, gap: f64) -> (Rect, Rect) {
    let available_width = frame.width - gap;
    let left_width = available_width * ratio;
    let right_width = available_width - left_width;

    let left = Rect::new(frame.x, frame.y, left_width, frame.height);
    let right = Rect::new(frame.x + left_width + gap, frame.y, right_width, frame.height);

    (left, right)
}

/// Splits a frame vertically (top/bottom) at the given ratio with a gap.
///
/// # Arguments
///
/// * `frame` - The frame to split
/// * `ratio` - Split ratio (0.0-1.0), where ratio is the top portion
/// * `gap` - Gap between the two resulting frames
///
/// # Returns
///
/// A tuple of (`top_frame`, `bottom_frame`)
#[must_use]
pub fn split_vertical(frame: &Rect, ratio: f64, gap: f64) -> (Rect, Rect) {
    let available_height = frame.height - gap;
    let top_height = available_height * ratio;
    let bottom_height = available_height - top_height;

    let top = Rect::new(frame.x, frame.y, frame.width, top_height);
    let bottom = Rect::new(frame.x, frame.y + top_height + gap, frame.width, bottom_height);

    (top, bottom)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn test_split_horizontal_no_gap() {
        let frame = Rect::new(0.0, 0.0, 100.0, 50.0);
        let (left, right) = split_horizontal(&frame, 0.6, 0.0);

        assert_eq!(left.width, 60.0);
        assert_eq!(right.width, 40.0);
        assert_eq!(left.x, 0.0);
        assert_eq!(right.x, 60.0);
        assert_eq!(left.height, 50.0);
        assert_eq!(right.height, 50.0);
    }

    #[test]
    fn test_split_horizontal_with_gap() {
        let frame = Rect::new(0.0, 0.0, 110.0, 50.0);
        let (left, right) = split_horizontal(&frame, 0.5, 10.0);

        // Available width = 110 - 10 = 100
        // Each side gets 50
        assert_eq!(left.width, 50.0);
        assert_eq!(right.width, 50.0);
        assert_eq!(left.x, 0.0);
        assert_eq!(right.x, 60.0); // 50 + 10 gap
    }

    #[test]
    fn test_split_horizontal_asymmetric_ratio() {
        let frame = Rect::new(0.0, 0.0, 100.0, 50.0);
        let (left, right) = split_horizontal(&frame, 0.7, 0.0);

        assert_eq!(left.width, 70.0);
        assert_eq!(right.width, 30.0);
    }

    #[test]
    fn test_split_horizontal_preserves_y_position() {
        let frame = Rect::new(10.0, 20.0, 100.0, 50.0);
        let (left, right) = split_horizontal(&frame, 0.5, 0.0);

        assert_eq!(left.x, 10.0);
        assert_eq!(left.y, 20.0);
        assert_eq!(right.x, 60.0);
        assert_eq!(right.y, 20.0);
    }

    #[test]
    fn test_split_vertical_no_gap() {
        let frame = Rect::new(0.0, 0.0, 100.0, 50.0);
        let (top, bottom) = split_vertical(&frame, 0.4, 0.0);

        assert_eq!(top.height, 20.0);
        assert_eq!(bottom.height, 30.0);
        assert_eq!(top.y, 0.0);
        assert_eq!(bottom.y, 20.0);
        assert_eq!(top.width, 100.0);
        assert_eq!(bottom.width, 100.0);
    }

    #[test]
    fn test_split_vertical_with_gap() {
        let frame = Rect::new(0.0, 0.0, 100.0, 110.0);
        let (top, bottom) = split_vertical(&frame, 0.5, 10.0);

        // Available height = 110 - 10 = 100
        // Each side gets 50
        assert_eq!(top.height, 50.0);
        assert_eq!(bottom.height, 50.0);
        assert_eq!(top.y, 0.0);
        assert_eq!(bottom.y, 60.0); // 50 + 10 gap
    }

    #[test]
    fn test_split_vertical_asymmetric_ratio() {
        let frame = Rect::new(0.0, 0.0, 100.0, 100.0);
        let (top, bottom) = split_vertical(&frame, 0.3, 0.0);

        assert_eq!(top.height, 30.0);
        assert_eq!(bottom.height, 70.0);
    }

    #[test]
    fn test_split_vertical_preserves_x_position() {
        let frame = Rect::new(10.0, 20.0, 100.0, 50.0);
        let (top, bottom) = split_vertical(&frame, 0.5, 0.0);

        assert_eq!(top.x, 10.0);
        assert_eq!(top.y, 20.0);
        assert_eq!(bottom.x, 10.0);
        assert_eq!(bottom.y, 45.0);
    }

    #[test]
    fn test_split_both_directions() {
        let frame = Rect::new(0.0, 0.0, 200.0, 100.0);

        // Split horizontally first
        let (left, right) = split_horizontal(&frame, 0.5, 10.0);
        assert_eq!(left.width, 95.0);
        assert_eq!(right.width, 95.0);

        // Then split the left vertically
        let (top_left, bottom_left) = split_vertical(&left, 0.5, 10.0);
        assert_eq!(top_left.width, 95.0);
        assert_eq!(bottom_left.width, 95.0);
        assert_eq!(top_left.height, 45.0);
        assert_eq!(bottom_left.height, 45.0);
    }
}
