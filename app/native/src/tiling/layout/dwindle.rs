//! Dwindle layout - proper binary space partitioning with spiral pattern.
//!
//! This implements the Dwindle algorithm similar to Hyprland's dwindle layout.
//! Each new window splits the **last window's space**, creating a spiral pattern.
//!
//! The layout adapts to screen orientation:
//! - **Landscape** (width >= height): First split is horizontal (left/right)
//! - **Portrait** (width < height): First split is vertical (top/bottom)
//!
//! ## Landscape Mode (e.g., 1920x1080)
//!
//! ```text
//! 1 window:   ┌───────────┐
//!             │           │
//!             │     1     │
//!             │           │
//!             └───────────┘
//!
//! 2 windows:  ┌─────┬─────┐
//!             │     │     │
//!             │  1  │  2  │
//!             │     │     │
//!             └─────┴─────┘
//!
//! 3 windows:  ┌─────┬─────┐
//!             │     │  2  │
//!             │  1  ├─────┤
//!             │     │  3  │
//!             └─────┴─────┘
//!
//! 4 windows:  ┌─────┬─────┐
//!             │     │  2  │
//!             │  1  ├──┬──┤
//!             │     │ 3│ 4│
//!             └─────┴──┴──┘
//! ```
//!
//! ## Portrait Mode (e.g., 1080x1920)
//!
//! ```text
//! 1 window:   ┌─────┐
//!             │     │
//!             │  1  │
//!             │     │
//!             └─────┘
//!
//! 2 windows:  ┌─────┐
//!             │  1  │
//!             ├─────┤
//!             │  2  │
//!             └─────┘
//!
//! 3 windows:  ┌─────┐
//!             │  1  │
//!             ├──┬──┤
//!             │ 2│ 3│
//!             └──┴──┘
//!
//! 4 windows:  ┌─────┐
//!             │  1  │
//!             ├──┬──┤
//!             │ 2│ 3│
//!             │  ├──┤
//!             │  │ 4│
//!             └──┴──┘
//! ```

use super::{helpers, Gaps, LayoutResult};
use crate::tiling::state::Rect;

/// Dwindle layout - windows arranged in a dwindling spiral pattern.
///
/// Each new window splits the last window's space, alternating between
/// horizontal and vertical splits. The initial split direction is determined
/// by the screen orientation:
/// - Landscape (width >= height): starts with horizontal split
/// - Portrait (width < height): starts with vertical split
///
/// # Arguments
///
/// * `window_ids` - IDs of windows to arrange (in order of creation)
/// * `screen_frame` - The visible frame of the screen
/// * `gaps` - Gap values for spacing between windows
#[must_use]
pub fn layout(window_ids: &[u32], screen_frame: &Rect, gaps: &Gaps) -> LayoutResult {
    if window_ids.is_empty() {
        return Vec::new();
    }

    if window_ids.len() == 1 {
        return vec![(window_ids[0], *screen_frame)];
    }

    // Determine initial split direction based on screen orientation
    // Landscape: start horizontal (left/right split)
    // Portrait: start vertical (top/bottom split)
    let is_landscape = screen_frame.width >= screen_frame.height;

    let mut result = Vec::with_capacity(window_ids.len());

    // Build the layout iteratively by splitting the last window's space
    // Start with first window taking full screen
    let mut frames: Vec<Rect> = vec![*screen_frame];

    for i in 1..window_ids.len() {
        // Get the frame we're going to split (the last one)
        let parent_frame = frames[i - 1];

        // Alternate split direction starting from the orientation-appropriate direction
        // For landscape: odd index = horizontal, even = vertical
        // For portrait: odd index = vertical, even = horizontal
        let split_horizontal = if is_landscape {
            i % 2 == 1 // 1st split horizontal, 2nd vertical, 3rd horizontal...
        } else {
            i % 2 == 0 // 1st split vertical, 2nd horizontal, 3rd vertical...
        };

        // Split the parent frame
        let (first_half, second_half) = if split_horizontal {
            helpers::split_horizontal(&parent_frame, 0.5, gaps.inner_h)
        } else {
            helpers::split_vertical(&parent_frame, 0.5, gaps.inner_v)
        };

        // Update the parent window's frame to first half
        frames[i - 1] = first_half;

        // New window gets second half
        frames.push(second_half);
    }

    // Build result with window IDs
    for (id, frame) in window_ids.iter().zip(frames.iter()) {
        result.push((*id, *frame));
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
    fn test_dwindle_empty() {
        let result = layout(&[], &landscape_frame(), &no_gaps());
        assert!(result.is_empty());
    }

    #[test]
    fn test_dwindle_single_window() {
        let frame = landscape_frame();
        let result = layout(&[1], &frame, &no_gaps());

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (1, frame));
    }

    // ========================================================================
    // Landscape Mode Tests
    // ========================================================================

    #[test]
    fn test_landscape_two_windows() {
        let frame = landscape_frame();
        let result = layout(&[1, 2], &frame, &no_gaps());

        assert_eq!(result.len(), 2);

        let (id1, frame1) = result[0];
        let (id2, frame2) = result[1];

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);

        // First split is horizontal (side by side) in landscape
        assert_eq!(frame1.width, frame.width / 2.0);
        assert_eq!(frame2.width, frame.width / 2.0);
        assert_eq!(frame1.height, frame.height);
        assert_eq!(frame2.height, frame.height);

        // Window 1 on left, Window 2 on right
        assert_eq!(frame1.x, 0.0);
        assert_eq!(frame2.x, frame.width / 2.0);
    }

    #[test]
    fn test_landscape_three_windows() {
        let frame = landscape_frame();
        let result = layout(&[1, 2, 3], &frame, &no_gaps());

        assert_eq!(result.len(), 3);

        let (_, frame1) = result[0];
        let (_, frame2) = result[1];
        let (_, frame3) = result[2];

        // Window 1: Left half of screen
        assert_eq!(frame1.width, frame.width / 2.0);
        assert_eq!(frame1.height, frame.height);
        assert_eq!(frame1.x, 0.0);

        // Window 2: Top-right quarter (second split is vertical)
        assert_eq!(frame2.width, frame.width / 2.0);
        assert_eq!(frame2.height, frame.height / 2.0);
        assert_eq!(frame2.x, frame.width / 2.0);
        assert_eq!(frame2.y, 0.0);

        // Window 3: Bottom-right quarter
        assert_eq!(frame3.width, frame.width / 2.0);
        assert_eq!(frame3.height, frame.height / 2.0);
        assert_eq!(frame3.x, frame.width / 2.0);
        assert_eq!(frame3.y, frame.height / 2.0);
    }

    #[test]
    fn test_landscape_four_windows() {
        let frame = landscape_frame();
        let result = layout(&[1, 2, 3, 4], &frame, &no_gaps());

        assert_eq!(result.len(), 4);

        let (_, frame1) = result[0];
        let (_, frame2) = result[1];
        let (_, frame3) = result[2];
        let (_, frame4) = result[3];

        // Window 1: Left half
        assert_eq!(frame1.width, frame.width / 2.0);
        assert_eq!(frame1.height, frame.height);

        // Window 2: Top-right quarter
        assert_eq!(frame2.width, frame.width / 2.0);
        assert_eq!(frame2.height, frame.height / 2.0);
        assert_eq!(frame2.x, frame.width / 2.0);
        assert_eq!(frame2.y, 0.0);

        // Window 3: Bottom-right, left half (third split is horizontal)
        assert_eq!(frame3.width, frame.width / 4.0);
        assert_eq!(frame3.height, frame.height / 2.0);
        assert_eq!(frame3.x, frame.width / 2.0);
        assert_eq!(frame3.y, frame.height / 2.0);

        // Window 4: Bottom-right, right half
        assert_eq!(frame4.width, frame.width / 4.0);
        assert_eq!(frame4.height, frame.height / 2.0);
        assert_eq!(frame4.x, frame.width * 0.75);
        assert_eq!(frame4.y, frame.height / 2.0);
    }

    // ========================================================================
    // Portrait Mode Tests
    // ========================================================================

    #[test]
    fn test_portrait_two_windows() {
        let frame = portrait_frame();
        let result = layout(&[1, 2], &frame, &no_gaps());

        assert_eq!(result.len(), 2);

        let (_, frame1) = result[0];
        let (_, frame2) = result[1];

        // First split is vertical (stacked) in portrait
        assert_eq!(frame1.width, frame.width);
        assert_eq!(frame2.width, frame.width);
        assert_eq!(frame1.height, frame.height / 2.0);
        assert_eq!(frame2.height, frame.height / 2.0);

        // Window 1 on top, Window 2 on bottom
        assert_eq!(frame1.y, 0.0);
        assert_eq!(frame2.y, frame.height / 2.0);
    }

    #[test]
    fn test_portrait_three_windows() {
        let frame = portrait_frame();
        let result = layout(&[1, 2, 3], &frame, &no_gaps());

        assert_eq!(result.len(), 3);

        let (_, frame1) = result[0];
        let (_, frame2) = result[1];
        let (_, frame3) = result[2];

        // Window 1: Top half of screen
        assert_eq!(frame1.width, frame.width);
        assert_eq!(frame1.height, frame.height / 2.0);
        assert_eq!(frame1.y, 0.0);

        // Window 2: Bottom-left quarter (second split is horizontal)
        assert_eq!(frame2.width, frame.width / 2.0);
        assert_eq!(frame2.height, frame.height / 2.0);
        assert_eq!(frame2.x, 0.0);
        assert_eq!(frame2.y, frame.height / 2.0);

        // Window 3: Bottom-right quarter
        assert_eq!(frame3.width, frame.width / 2.0);
        assert_eq!(frame3.height, frame.height / 2.0);
        assert_eq!(frame3.x, frame.width / 2.0);
        assert_eq!(frame3.y, frame.height / 2.0);
    }

    #[test]
    fn test_portrait_four_windows() {
        let frame = portrait_frame();
        let result = layout(&[1, 2, 3, 4], &frame, &no_gaps());

        assert_eq!(result.len(), 4);

        let (_, frame1) = result[0];
        let (_, frame2) = result[1];
        let (_, frame3) = result[2];
        let (_, frame4) = result[3];

        // Window 1: Top half
        assert_eq!(frame1.width, frame.width);
        assert_eq!(frame1.height, frame.height / 2.0);

        // Window 2: Bottom-left quarter
        assert_eq!(frame2.width, frame.width / 2.0);
        assert_eq!(frame2.height, frame.height / 2.0);
        assert_eq!(frame2.x, 0.0);
        assert_eq!(frame2.y, frame.height / 2.0);

        // Window 3: Bottom-right, top half (third split is vertical)
        assert_eq!(frame3.width, frame.width / 2.0);
        assert_eq!(frame3.height, frame.height / 4.0);
        assert_eq!(frame3.x, frame.width / 2.0);
        assert_eq!(frame3.y, frame.height / 2.0);

        // Window 4: Bottom-right, bottom half
        assert_eq!(frame4.width, frame.width / 2.0);
        assert_eq!(frame4.height, frame.height / 4.0);
        assert_eq!(frame4.x, frame.width / 2.0);
        assert_eq!(frame4.y, frame.height * 0.75);
    }

    // ========================================================================
    // General Tests (apply to both orientations)
    // ========================================================================

    #[test]
    fn test_dwindle_with_gaps() {
        let frame = landscape_frame();
        let gaps = Gaps::uniform(20.0, 0.0);
        let result = layout(&[1, 2], &frame, &gaps);

        let (_, frame1) = result[0];
        let (_, frame2) = result[1];

        // Gap between windows
        let gap = frame2.x - (frame1.x + frame1.width);
        assert!((gap - 20.0).abs() < 0.1);
    }

    #[test]
    fn test_dwindle_preserves_order() {
        let frame = landscape_frame();
        let result = layout(&[10, 20, 30, 40], &frame, &no_gaps());

        assert_eq!(result[0].0, 10);
        assert_eq!(result[1].0, 20);
        assert_eq!(result[2].0, 30);
        assert_eq!(result[3].0, 40);
    }

    #[test]
    fn test_dwindle_total_area_preserved_landscape() {
        let frame = landscape_frame();
        let result = layout(&[1, 2, 3, 4, 5, 6], &frame, &no_gaps());

        let total_area: f64 = result.iter().map(|(_, f)| f.area()).sum();
        let screen_area = frame.area();

        assert!(
            (total_area - screen_area).abs() < 1.0,
            "Total area {total_area} should equal screen area {screen_area}"
        );
    }

    #[test]
    fn test_dwindle_total_area_preserved_portrait() {
        let frame = portrait_frame();
        let result = layout(&[1, 2, 3, 4, 5, 6], &frame, &no_gaps());

        let total_area: f64 = result.iter().map(|(_, f)| f.area()).sum();
        let screen_area = frame.area();

        assert!(
            (total_area - screen_area).abs() < 1.0,
            "Total area {total_area} should equal screen area {screen_area}"
        );
    }

    #[test]
    fn test_dwindle_no_overlap() {
        let frame = landscape_frame();
        let result = layout(&[1, 2, 3, 4], &frame, &no_gaps());

        for (i, (_, frame_a)) in result.iter().enumerate() {
            for (j, (_, frame_b)) in result.iter().enumerate() {
                if i != j {
                    let overlaps = frame_a.x < frame_b.x + frame_b.width
                        && frame_a.x + frame_a.width > frame_b.x
                        && frame_a.y < frame_b.y + frame_b.height
                        && frame_a.y + frame_a.height > frame_b.y;

                    assert!(
                        !overlaps,
                        "Windows {i} and {j} overlap: {:?} and {:?}",
                        frame_a, frame_b
                    );
                }
            }
        }
    }

    #[test]
    fn test_dwindle_many_windows() {
        let frame = landscape_frame();
        let ids: Vec<u32> = (1..=8).collect();
        let result = layout(&ids, &frame, &no_gaps());

        assert_eq!(result.len(), 8);

        for (id, window_frame) in &result {
            assert!(
                window_frame.area() > 0.0,
                "Window {id} should have positive area"
            );
        }
    }

    #[test]
    fn test_square_screen_uses_landscape_behavior() {
        // Square screens (width == height) should use landscape behavior
        let frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let result = layout(&[1, 2], &frame, &no_gaps());

        let (_, frame1) = result[0];
        let (_, frame2) = result[1];

        // First split should be horizontal (side by side)
        assert_eq!(frame1.width, frame.width / 2.0);
        assert_eq!(frame2.width, frame.width / 2.0);
        assert_eq!(frame1.height, frame.height);
        assert_eq!(frame2.height, frame.height);
    }
}
