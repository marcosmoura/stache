//! Split layouts - windows arranged in rows or columns.
//!
//! Supports three modes:
//! - **Auto**: Splits based on screen orientation (horizontal for landscape, vertical for portrait)
//! - **Horizontal**: Windows side by side (columns)
//! - **Vertical**: Windows stacked (rows)
//!
//! All modes support custom split ratios for manual resizing.

use super::{Gaps, LayoutResult};
use crate::tiling::state::Rect;

/// Auto-split layout - splits based on screen orientation.
///
/// - Wide screens (landscape): horizontal split (windows side by side)
/// - Tall screens (portrait): vertical split (windows stacked)
///
/// # Arguments
///
/// * `window_ids` - IDs of windows to arrange
/// * `screen_frame` - The visible frame of the screen
/// * `gaps` - Gap values for spacing
/// * `ratios` - Custom split ratios (cumulative positions 0.0-1.0)
#[must_use]
pub fn layout_auto(
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    ratios: &[f64],
) -> LayoutResult {
    if screen_frame.width >= screen_frame.height {
        layout_horizontal(window_ids, screen_frame, gaps, ratios)
    } else {
        layout_vertical(window_ids, screen_frame, gaps, ratios)
    }
}

/// Horizontal split layout - windows arranged side by side (columns).
///
/// # Arguments
///
/// * `window_ids` - IDs of windows to arrange
/// * `screen_frame` - The visible frame of the screen
/// * `gaps` - Gap values for spacing
/// * `ratios` - Custom split ratios (cumulative positions 0.0-1.0).
///   If empty or wrong length, equal splits are used.
#[allow(clippy::cast_precision_loss)] // Window counts won't exceed f64 precision
#[must_use]
pub fn layout_horizontal(
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    ratios: &[f64],
) -> LayoutResult {
    if window_ids.is_empty() {
        return Vec::new();
    }

    let count = window_ids.len();

    // Check if we have valid custom ratios (N-1 ratios for N windows)
    let use_custom_ratios = ratios.len() == count.saturating_sub(1) && count > 1;

    if use_custom_ratios {
        layout_horizontal_with_ratios(window_ids, screen_frame, gaps, ratios)
    } else {
        layout_horizontal_equal(window_ids, screen_frame, gaps)
    }
}

/// Vertical split layout - windows stacked top to bottom (rows).
///
/// # Arguments
///
/// * `window_ids` - IDs of windows to arrange
/// * `screen_frame` - The visible frame of the screen
/// * `gaps` - Gap values for spacing
/// * `ratios` - Custom split ratios (cumulative positions 0.0-1.0).
///   If empty or wrong length, equal splits are used.
#[allow(clippy::cast_precision_loss)] // Window counts won't exceed f64 precision
#[must_use]
pub fn layout_vertical(
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    ratios: &[f64],
) -> LayoutResult {
    if window_ids.is_empty() {
        return Vec::new();
    }

    let count = window_ids.len();

    // Check if we have valid custom ratios (N-1 ratios for N windows)
    let use_custom_ratios = ratios.len() == count.saturating_sub(1) && count > 1;

    if use_custom_ratios {
        layout_vertical_with_ratios(window_ids, screen_frame, gaps, ratios)
    } else {
        layout_vertical_equal(window_ids, screen_frame, gaps)
    }
}

// ============================================================================
// Internal Implementation
// ============================================================================

/// Horizontal split with equal widths.
#[allow(clippy::cast_precision_loss)]
fn layout_horizontal_equal(window_ids: &[u32], screen_frame: &Rect, gaps: &Gaps) -> LayoutResult {
    let count = window_ids.len();
    let total_gap = gaps.inner_h * (count - 1) as f64;
    let width_per_window = (screen_frame.width - total_gap) / count as f64;

    window_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            let x = (i as f64).mul_add(width_per_window + gaps.inner_h, screen_frame.x);
            let frame = Rect::new(x, screen_frame.y, width_per_window, screen_frame.height);
            (id, frame)
        })
        .collect()
}

/// Vertical split with equal heights.
#[allow(clippy::cast_precision_loss)]
fn layout_vertical_equal(window_ids: &[u32], screen_frame: &Rect, gaps: &Gaps) -> LayoutResult {
    let count = window_ids.len();
    let total_gap = gaps.inner_v * (count - 1) as f64;
    let height_per_window = (screen_frame.height - total_gap) / count as f64;

    window_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            let y = (i as f64).mul_add(height_per_window + gaps.inner_v, screen_frame.y);
            let frame = Rect::new(screen_frame.x, y, screen_frame.width, height_per_window);
            (id, frame)
        })
        .collect()
}

/// Horizontal split with custom ratios.
#[allow(clippy::cast_precision_loss)]
fn layout_horizontal_with_ratios(
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    ratios: &[f64],
) -> LayoutResult {
    let count = window_ids.len();
    let total_gap = gaps.inner_h * (count - 1) as f64;
    let available_width = screen_frame.width - total_gap;

    let mut result = Vec::with_capacity(count);
    let mut prev_ratio = 0.0;

    for (i, &id) in window_ids.iter().enumerate() {
        let next_ratio = if i < ratios.len() { ratios[i] } else { 1.0 };

        // Calculate this window's width as a proportion of the available space
        let ratio_width = (next_ratio - prev_ratio) * available_width;

        // Calculate x position (account for previous windows + gaps)
        let x = (i as f64).mul_add(gaps.inner_h, screen_frame.x + prev_ratio * available_width);

        let frame = Rect::new(x, screen_frame.y, ratio_width, screen_frame.height);
        result.push((id, frame));

        prev_ratio = next_ratio;
    }

    result
}

/// Vertical split with custom ratios.
#[allow(clippy::cast_precision_loss)]
fn layout_vertical_with_ratios(
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    ratios: &[f64],
) -> LayoutResult {
    let count = window_ids.len();
    let total_gap = gaps.inner_v * (count - 1) as f64;
    let available_height = screen_frame.height - total_gap;

    let mut result = Vec::with_capacity(count);
    let mut prev_ratio = 0.0;

    for (i, &id) in window_ids.iter().enumerate() {
        let next_ratio = if i < ratios.len() { ratios[i] } else { 1.0 };

        // Calculate this window's height as a proportion of the available space
        let ratio_height = (next_ratio - prev_ratio) * available_height;

        // Calculate y position (account for previous windows + gaps)
        let y = (i as f64).mul_add(gaps.inner_v, screen_frame.y + prev_ratio * available_height);

        let frame = Rect::new(screen_frame.x, y, screen_frame.width, ratio_height);
        result.push((id, frame));

        prev_ratio = next_ratio;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_frame() -> Rect { Rect::new(0.0, 0.0, 1920.0, 1080.0) }

    fn no_gaps() -> Gaps { Gaps::default() }

    // ========================================================================
    // Horizontal Split Tests
    // ========================================================================

    #[test]
    fn test_horizontal_empty() {
        let result = layout_horizontal(&[], &screen_frame(), &no_gaps(), &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_horizontal_single_window() {
        let frame = screen_frame();
        let result = layout_horizontal(&[1], &frame, &no_gaps(), &[]);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (1, frame));
    }

    #[test]
    fn test_horizontal_two_windows() {
        let frame = screen_frame();
        let result = layout_horizontal(&[1, 2], &frame, &no_gaps(), &[]);

        assert_eq!(result.len(), 2);

        let (_, left) = result[0];
        let (_, right) = result[1];

        // Windows should be side by side
        assert_eq!(left.height, frame.height);
        assert_eq!(right.height, frame.height);
        assert_eq!(left.width, frame.width / 2.0);
        assert_eq!(right.width, frame.width / 2.0);
        assert_eq!(left.x, frame.x);
        assert_eq!(right.x, frame.x + frame.width / 2.0);
    }

    #[test]
    fn test_horizontal_with_gaps() {
        let frame = screen_frame();
        let gaps = Gaps::uniform(16.0, 0.0);
        let result = layout_horizontal(&[1, 2], &frame, &gaps, &[]);

        let (_, left) = result[0];
        let (_, right) = result[1];

        // Gap between windows
        let gap_between = right.x - (left.x + left.width);
        assert!((gap_between - 16.0).abs() < 0.1);
    }

    #[test]
    fn test_horizontal_with_custom_ratios() {
        let frame = screen_frame();
        // 70% for first window, 30% for second
        let ratios = vec![0.7];
        let result = layout_horizontal(&[1, 2], &frame, &no_gaps(), &ratios);

        let (_, left) = result[0];
        let (_, right) = result[1];

        assert!((left.width - frame.width * 0.7).abs() < 1.0);
        assert!((right.width - frame.width * 0.3).abs() < 1.0);
    }

    // ========================================================================
    // Vertical Split Tests
    // ========================================================================

    #[test]
    fn test_vertical_empty() {
        let result = layout_vertical(&[], &screen_frame(), &no_gaps(), &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_vertical_single_window() {
        let frame = screen_frame();
        let result = layout_vertical(&[1], &frame, &no_gaps(), &[]);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (1, frame));
    }

    #[test]
    fn test_vertical_two_windows() {
        let frame = screen_frame();
        let result = layout_vertical(&[1, 2], &frame, &no_gaps(), &[]);

        assert_eq!(result.len(), 2);

        let (_, top) = result[0];
        let (_, bottom) = result[1];

        // Windows should be stacked vertically
        assert_eq!(top.width, frame.width);
        assert_eq!(bottom.width, frame.width);
        assert_eq!(top.height, frame.height / 2.0);
        assert_eq!(bottom.height, frame.height / 2.0);
        assert_eq!(top.y, frame.y);
        assert_eq!(bottom.y, frame.y + frame.height / 2.0);
    }

    #[test]
    fn test_vertical_with_gaps() {
        let frame = screen_frame();
        let gaps = Gaps::uniform(16.0, 0.0);
        let result = layout_vertical(&[1, 2], &frame, &gaps, &[]);

        let (_, top) = result[0];
        let (_, bottom) = result[1];

        // Gap between windows
        let gap_between = bottom.y - (top.y + top.height);
        assert!((gap_between - 16.0).abs() < 0.1);
    }

    #[test]
    fn test_vertical_with_custom_ratios() {
        let frame = screen_frame();
        // 60% for first window, 40% for second
        let ratios = vec![0.6];
        let result = layout_vertical(&[1, 2], &frame, &no_gaps(), &ratios);

        let (_, top) = result[0];
        let (_, bottom) = result[1];

        assert!((top.height - frame.height * 0.6).abs() < 1.0);
        assert!((bottom.height - frame.height * 0.4).abs() < 1.0);
    }

    // ========================================================================
    // Auto Split Tests
    // ========================================================================

    #[test]
    fn test_auto_landscape() {
        // Wide screen - should split horizontally
        let frame = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let result = layout_auto(&[1, 2], &frame, &no_gaps(), &[]);

        let (_, w1) = result[0];
        let (_, w2) = result[1];

        // Horizontal split = side by side (same height)
        assert_eq!(w1.height, frame.height);
        assert_eq!(w2.height, frame.height);
    }

    #[test]
    fn test_auto_portrait() {
        // Tall screen - should split vertically
        let frame = Rect::new(0.0, 0.0, 1080.0, 1920.0);
        let result = layout_auto(&[1, 2], &frame, &no_gaps(), &[]);

        let (_, w1) = result[0];
        let (_, w2) = result[1];

        // Vertical split = stacked (same width)
        assert_eq!(w1.width, frame.width);
        assert_eq!(w2.width, frame.width);
    }

    #[test]
    fn test_auto_square() {
        // Square screen - should split horizontally (width >= height)
        let frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let result = layout_auto(&[1, 2], &frame, &no_gaps(), &[]);

        let (_, w1) = result[0];
        let (_, w2) = result[1];

        // Horizontal split = side by side
        assert_eq!(w1.height, frame.height);
        assert_eq!(w2.height, frame.height);
    }
}
