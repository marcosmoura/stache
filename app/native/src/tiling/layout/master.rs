//! Master layout - one master window with remaining windows in a stack.
//!
//! The first window is the "master" and gets a larger portion of the screen.
//! Remaining windows are arranged in the remaining space.
//!
//! The master position can be configured:
//! - **Left**: Master on left, stack on right (stacked vertically)
//! - **Right**: Master on right, stack on left (stacked vertically)
//! - **Top**: Master on top, stack below (arranged horizontally)
//! - **Bottom**: Master on bottom, stack above (arranged horizontally)
//! - **Auto**: Adapts to screen orientation (left for landscape, top for portrait)
//!
//! ## Left/Right Position (horizontal split)
//!
//! ```text
//! Left:           Right:
//! ┌──────────┬─────┐  ┌─────┬──────────┐
//! │          │  2  │  │  2  │          │
//! │  Master  ├─────┤  ├─────┤  Master  │
//! │          │  3  │  │  3  │          │
//! └──────────┴─────┘  └─────┴──────────┘
//! ```
//!
//! ## Top/Bottom Position (vertical split)
//!
//! ```text
//! Top:              Bottom:
//! ┌─────────┐       ┌────┬────┐
//! │         │       │ 2  │ 3  │
//! │ Master  │       ├────┴────┤
//! │         │       │         │
//! ├────┬────┤       │ Master  │
//! │ 2  │ 3  │       │         │
//! └────┴────┘       └─────────┘
//! ```

use super::{Gaps, LayoutResult};
use crate::config::MasterPosition;
use crate::tiling::state::Rect;

/// Master layout - one master window with remaining windows in a stack.
///
/// The first window is the "master" and gets a larger portion of the screen.
/// The master position can be configured, or set to auto to adapt to screen orientation.
///
/// # Arguments
///
/// * `window_ids` - IDs of windows to arrange (first one is master)
/// * `screen_frame` - The visible frame of the screen
/// * `master_ratio` - Ratio of screen for master window (0.0-1.0, clamped to 0.1-0.9)
/// * `gaps` - Gap values for spacing
/// * `position` - Position of the master window (left/right/top/bottom/auto)
#[allow(clippy::cast_precision_loss)] // Window counts won't exceed f64 precision
#[must_use]
pub fn layout(
    window_ids: &[u32],
    screen_frame: &Rect,
    master_ratio: f64,
    gaps: &Gaps,
    position: MasterPosition,
) -> LayoutResult {
    if window_ids.is_empty() {
        return Vec::new();
    }

    // Clamp ratio to valid range
    let ratio = master_ratio.clamp(0.1, 0.9);

    // Single window - takes full screen
    if window_ids.len() == 1 {
        return vec![(window_ids[0], *screen_frame)];
    }

    // Resolve auto position based on screen orientation
    let resolved_position = match position {
        MasterPosition::Auto => {
            if screen_frame.width >= screen_frame.height {
                MasterPosition::Left
            } else {
                MasterPosition::Top
            }
        }
        other => other,
    };

    match resolved_position {
        MasterPosition::Left => layout_left(window_ids, screen_frame, ratio, gaps),
        MasterPosition::Right => layout_right(window_ids, screen_frame, ratio, gaps),
        MasterPosition::Top => layout_top(window_ids, screen_frame, ratio, gaps),
        MasterPosition::Bottom => layout_bottom(window_ids, screen_frame, ratio, gaps),
        MasterPosition::Auto => unreachable!(), // Already resolved above
    }
}

/// Master on left, stack on right (stacked vertically).
#[allow(clippy::cast_precision_loss)]
fn layout_left(window_ids: &[u32], screen_frame: &Rect, ratio: f64, gaps: &Gaps) -> LayoutResult {
    let mut result = Vec::with_capacity(window_ids.len());

    // Account for gap between master and stack
    let available_width = screen_frame.width - gaps.inner_h;

    // Master window (left side)
    let master_width = available_width * ratio;
    let master_frame = Rect::new(screen_frame.x, screen_frame.y, master_width, screen_frame.height);
    result.push((window_ids[0], master_frame));

    // Stack windows (right side, stacked vertically)
    let stack_x = screen_frame.x + master_width + gaps.inner_h;
    let stack_width = available_width - master_width;
    let stack_count = window_ids.len() - 1;
    let total_stack_gap = gaps.inner_v * (stack_count - 1) as f64;
    let stack_height = (screen_frame.height - total_stack_gap) / stack_count as f64;

    for (i, &id) in window_ids.iter().skip(1).enumerate() {
        let y = (i as f64).mul_add(stack_height + gaps.inner_v, screen_frame.y);
        let frame = Rect::new(stack_x, y, stack_width, stack_height);
        result.push((id, frame));
    }

    result
}

/// Master on right, stack on left (stacked vertically).
#[allow(clippy::cast_precision_loss)]
fn layout_right(window_ids: &[u32], screen_frame: &Rect, ratio: f64, gaps: &Gaps) -> LayoutResult {
    let mut result = Vec::with_capacity(window_ids.len());

    // Account for gap between master and stack
    let available_width = screen_frame.width - gaps.inner_h;

    // Stack windows (left side, stacked vertically)
    let stack_width = available_width * (1.0 - ratio);
    let stack_count = window_ids.len() - 1;
    let total_stack_gap = gaps.inner_v * (stack_count - 1) as f64;
    let stack_height = (screen_frame.height - total_stack_gap) / stack_count as f64;

    // Master window (right side)
    let master_width = available_width * ratio;
    let master_x = screen_frame.x + stack_width + gaps.inner_h;
    let master_frame = Rect::new(master_x, screen_frame.y, master_width, screen_frame.height);
    result.push((window_ids[0], master_frame));

    for (i, &id) in window_ids.iter().skip(1).enumerate() {
        let y = (i as f64).mul_add(stack_height + gaps.inner_v, screen_frame.y);
        let frame = Rect::new(screen_frame.x, y, stack_width, stack_height);
        result.push((id, frame));
    }

    result
}

/// Master on top, stack below (arranged horizontally).
#[allow(clippy::cast_precision_loss)]
fn layout_top(window_ids: &[u32], screen_frame: &Rect, ratio: f64, gaps: &Gaps) -> LayoutResult {
    let mut result = Vec::with_capacity(window_ids.len());

    // Account for gap between master and stack
    let available_height = screen_frame.height - gaps.inner_v;

    // Master window (top)
    let master_height = available_height * ratio;
    let master_frame = Rect::new(screen_frame.x, screen_frame.y, screen_frame.width, master_height);
    result.push((window_ids[0], master_frame));

    // Stack windows (bottom, arranged horizontally)
    let stack_y = screen_frame.y + master_height + gaps.inner_v;
    let stack_height = available_height - master_height;
    let stack_count = window_ids.len() - 1;
    let total_stack_gap = gaps.inner_h * (stack_count - 1) as f64;
    let stack_width = (screen_frame.width - total_stack_gap) / stack_count as f64;

    for (i, &id) in window_ids.iter().skip(1).enumerate() {
        let x = (i as f64).mul_add(stack_width + gaps.inner_h, screen_frame.x);
        let frame = Rect::new(x, stack_y, stack_width, stack_height);
        result.push((id, frame));
    }

    result
}

/// Master on bottom, stack above (arranged horizontally).
#[allow(clippy::cast_precision_loss)]
fn layout_bottom(window_ids: &[u32], screen_frame: &Rect, ratio: f64, gaps: &Gaps) -> LayoutResult {
    let mut result = Vec::with_capacity(window_ids.len());

    // Account for gap between master and stack
    let available_height = screen_frame.height - gaps.inner_v;

    // Stack windows (top, arranged horizontally)
    let stack_height = available_height * (1.0 - ratio);
    let stack_count = window_ids.len() - 1;
    let total_stack_gap = gaps.inner_h * (stack_count - 1) as f64;
    let stack_width = (screen_frame.width - total_stack_gap) / stack_count as f64;

    // Master window (bottom)
    let master_height = available_height * ratio;
    let master_y = screen_frame.y + stack_height + gaps.inner_v;
    let master_frame = Rect::new(screen_frame.x, master_y, screen_frame.width, master_height);
    result.push((window_ids[0], master_frame));

    for (i, &id) in window_ids.iter().skip(1).enumerate() {
        let x = (i as f64).mul_add(stack_width + gaps.inner_h, screen_frame.x);
        let frame = Rect::new(x, screen_frame.y, stack_width, stack_height);
        result.push((id, frame));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn landscape_frame() -> Rect { Rect::new(0.0, 0.0, 1920.0, 1080.0) }

    fn portrait_frame() -> Rect { Rect::new(0.0, 0.0, 1080.0, 1920.0) }

    fn no_gaps() -> Gaps { Gaps::default() }

    // ========================================================================
    // Basic Tests
    // ========================================================================

    #[test]
    fn test_master_empty() {
        let result = layout(&[], &landscape_frame(), 0.6, &no_gaps(), MasterPosition::Auto);
        assert!(result.is_empty());
    }

    #[test]
    fn test_master_single_window_landscape() {
        let frame = landscape_frame();
        let result = layout(&[1], &frame, 0.6, &no_gaps(), MasterPosition::Auto);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (1, frame));
    }

    #[test]
    fn test_master_single_window_portrait() {
        let frame = portrait_frame();
        let result = layout(&[1], &frame, 0.6, &no_gaps(), MasterPosition::Auto);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (1, frame));
    }

    // ========================================================================
    // Auto Mode Tests (Landscape = Left, Portrait = Top)
    // ========================================================================

    #[test]
    fn test_auto_landscape_two_windows() {
        let frame = landscape_frame();
        let ratio = 0.6;
        let result = layout(&[1, 2], &frame, ratio, &no_gaps(), MasterPosition::Auto);

        assert_eq!(result.len(), 2);

        let (master_id, master_frame) = result[0];
        let (stack_id, stack_frame) = result[1];

        assert_eq!(master_id, 1);
        assert_eq!(stack_id, 2);

        // Master on left, gets 60% width
        assert!((master_frame.width - frame.width * ratio).abs() < 1.0);
        assert_eq!(master_frame.height, frame.height);
        assert_eq!(master_frame.x, 0.0);

        // Stack on right, gets 40% width
        assert!((stack_frame.width - frame.width * (1.0 - ratio)).abs() < 1.0);
        assert_eq!(stack_frame.height, frame.height);
        assert!(stack_frame.x > master_frame.x);
    }

    #[test]
    fn test_auto_portrait_two_windows() {
        let frame = portrait_frame();
        let ratio = 0.6;
        let result = layout(&[1, 2], &frame, ratio, &no_gaps(), MasterPosition::Auto);

        assert_eq!(result.len(), 2);

        let (master_id, master_frame) = result[0];
        let (stack_id, stack_frame) = result[1];

        assert_eq!(master_id, 1);
        assert_eq!(stack_id, 2);

        // Master on top, gets 60% height
        assert_eq!(master_frame.width, frame.width);
        assert!((master_frame.height - frame.height * ratio).abs() < 1.0);
        assert_eq!(master_frame.y, 0.0);

        // Stack below, gets 40% height
        assert_eq!(stack_frame.width, frame.width);
        assert!((stack_frame.height - frame.height * (1.0 - ratio)).abs() < 1.0);
        assert!(stack_frame.y > master_frame.y);
    }

    // ========================================================================
    // Left Position Tests
    // ========================================================================

    #[test]
    fn test_left_position() {
        let frame = landscape_frame();
        let ratio = 0.6;
        let result = layout(&[1, 2, 3], &frame, ratio, &no_gaps(), MasterPosition::Left);

        assert_eq!(result.len(), 3);

        let (_, master_frame) = result[0];
        let (_, stack1_frame) = result[1];
        let (_, stack2_frame) = result[2];

        // Master gets left side
        assert!((master_frame.width - frame.width * ratio).abs() < 1.0);
        assert_eq!(master_frame.height, frame.height);
        assert_eq!(master_frame.x, 0.0);

        // Stack windows share right side, stacked vertically
        assert!(stack1_frame.x > master_frame.x);
        assert!(stack1_frame.y < stack2_frame.y);
    }

    // ========================================================================
    // Right Position Tests
    // ========================================================================

    #[test]
    fn test_right_position() {
        let frame = landscape_frame();
        let ratio = 0.6;
        let result = layout(&[1, 2, 3], &frame, ratio, &no_gaps(), MasterPosition::Right);

        assert_eq!(result.len(), 3);

        let (_, master_frame) = result[0];
        let (_, stack1_frame) = result[1];
        let (_, stack2_frame) = result[2];

        // Master gets right side
        assert!((master_frame.width - frame.width * ratio).abs() < 1.0);
        assert_eq!(master_frame.height, frame.height);
        assert!(master_frame.x > stack1_frame.x);

        // Stack windows share left side, stacked vertically
        assert_eq!(stack1_frame.x, 0.0);
        assert!(stack1_frame.y < stack2_frame.y);
    }

    // ========================================================================
    // Top Position Tests
    // ========================================================================

    #[test]
    fn test_top_position() {
        let frame = landscape_frame();
        let ratio = 0.6;
        let result = layout(&[1, 2, 3], &frame, ratio, &no_gaps(), MasterPosition::Top);

        assert_eq!(result.len(), 3);

        let (_, master_frame) = result[0];
        let (_, stack1_frame) = result[1];
        let (_, stack2_frame) = result[2];

        // Master gets top
        assert_eq!(master_frame.width, frame.width);
        assert!((master_frame.height - frame.height * ratio).abs() < 1.0);
        assert_eq!(master_frame.y, 0.0);

        // Stack windows share bottom, arranged horizontally
        assert!(stack1_frame.y > master_frame.y);
        assert!(stack1_frame.x < stack2_frame.x);
    }

    // ========================================================================
    // Bottom Position Tests
    // ========================================================================

    #[test]
    fn test_bottom_position() {
        let frame = landscape_frame();
        let ratio = 0.6;
        let result = layout(&[1, 2, 3], &frame, ratio, &no_gaps(), MasterPosition::Bottom);

        assert_eq!(result.len(), 3);

        let (_, master_frame) = result[0];
        let (_, stack1_frame) = result[1];
        let (_, stack2_frame) = result[2];

        // Master gets bottom
        assert_eq!(master_frame.width, frame.width);
        assert!((master_frame.height - frame.height * ratio).abs() < 1.0);
        assert!(master_frame.y > stack1_frame.y);

        // Stack windows share top, arranged horizontally
        assert_eq!(stack1_frame.y, 0.0);
        assert!(stack1_frame.x < stack2_frame.x);
    }

    // ========================================================================
    // Gaps Tests
    // ========================================================================

    #[test]
    fn test_left_with_gaps() {
        let frame = landscape_frame();
        let gaps = Gaps::uniform(16.0, 0.0);
        let result = layout(&[1, 2, 3], &frame, 0.5, &gaps, MasterPosition::Left);

        let (_, master) = result[0];
        let (_, stack1) = result[1];
        let (_, stack2) = result[2];

        // Gap between master and stack (horizontal)
        let gap_h = stack1.x - (master.x + master.width);
        assert!((gap_h - 16.0).abs() < 0.1);

        // Gap between stack windows (vertical)
        let gap_v = stack2.y - (stack1.y + stack1.height);
        assert!((gap_v - 16.0).abs() < 0.1);
    }

    #[test]
    fn test_top_with_gaps() {
        let frame = landscape_frame();
        let gaps = Gaps::uniform(16.0, 0.0);
        let result = layout(&[1, 2, 3], &frame, 0.5, &gaps, MasterPosition::Top);

        let (_, master) = result[0];
        let (_, stack1) = result[1];
        let (_, stack2) = result[2];

        // Gap between master and stack (vertical)
        let gap_v = stack1.y - (master.y + master.height);
        assert!((gap_v - 16.0).abs() < 0.1);

        // Gap between stack windows (horizontal)
        let gap_h = stack2.x - (stack1.x + stack1.width);
        assert!((gap_h - 16.0).abs() < 0.1);
    }

    // ========================================================================
    // Ratio Clamping Tests
    // ========================================================================

    #[test]
    fn test_master_ratio_clamping_low() {
        let frame = landscape_frame();
        let result = layout(&[1, 2], &frame, 0.0, &no_gaps(), MasterPosition::Left);

        let (_, master) = result[0];
        // Should be clamped to 10%
        assert!(master.width >= frame.width * 0.1 - 1.0);
    }

    #[test]
    fn test_master_ratio_clamping_high() {
        let frame = landscape_frame();
        let result = layout(&[1, 2], &frame, 1.0, &no_gaps(), MasterPosition::Left);

        let (_, master) = result[0];
        // Should be clamped to 90%
        assert!(master.width <= frame.width * 0.9 + 1.0);
    }

    // ========================================================================
    // Many Windows Tests
    // ========================================================================

    #[test]
    fn test_left_many_windows() {
        let frame = landscape_frame();
        let result = layout(&[1, 2, 3, 4, 5], &frame, 0.5, &no_gaps(), MasterPosition::Left);

        assert_eq!(result.len(), 5);

        let (_, master) = result[0];
        assert!((master.width - frame.width * 0.5).abs() < 1.0);

        // Stack has 4 windows, stacked vertically
        for (_, stack_frame) in result.iter().skip(1) {
            assert!((stack_frame.height - frame.height / 4.0).abs() < 1.0);
        }
    }

    #[test]
    fn test_top_many_windows() {
        let frame = portrait_frame();
        let result = layout(&[1, 2, 3, 4, 5], &frame, 0.5, &no_gaps(), MasterPosition::Top);

        assert_eq!(result.len(), 5);

        let (_, master) = result[0];
        assert!((master.height - frame.height * 0.5).abs() < 1.0);

        // Stack has 4 windows, arranged horizontally
        for (_, stack_frame) in result.iter().skip(1) {
            assert!((stack_frame.width - frame.width / 4.0).abs() < 1.0);
        }
    }

    #[test]
    fn test_square_screen_auto_uses_left() {
        // Square screens (width == height) with Auto should use Left
        let frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let result = layout(&[1, 2], &frame, 0.6, &no_gaps(), MasterPosition::Auto);

        let (_, master) = result[0];
        let (_, stack) = result[1];

        // Master on left (has width based on ratio)
        assert!((master.width - frame.width * 0.6).abs() < 1.0);
        assert_eq!(master.height, frame.height);
        assert!(stack.x > master.x);
    }
}
