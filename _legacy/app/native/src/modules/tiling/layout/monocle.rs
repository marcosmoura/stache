//! Monocle layout - all windows maximized to fill the screen.
//!
//! Every window gets the full screen frame. This is useful for
//! focusing on a single window at a time while keeping others accessible.

use super::LayoutResult;
use crate::modules::tiling::state::Rect;

/// Monocle layout - all windows maximized to fill the screen.
///
/// Every window gets the full screen frame.
///
/// # Arguments
///
/// * `window_ids` - IDs of windows to arrange
/// * `screen_frame` - The visible frame of the screen (already has outer gaps applied)
#[must_use]
pub fn layout(window_ids: &[u32], screen_frame: &Rect) -> LayoutResult {
    window_ids.iter().map(|&id| (id, *screen_frame)).collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_frame() -> Rect { Rect::new(0.0, 0.0, 1920.0, 1080.0) }

    #[test]
    fn test_monocle_empty() {
        let result = layout(&[], &screen_frame());
        assert!(result.is_empty());
    }

    #[test]
    fn test_monocle_single_window() {
        let frame = screen_frame();
        let result = layout(&[1], &frame);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (1, frame));
    }

    #[test]
    fn test_monocle_multiple_windows() {
        let frame = screen_frame();
        let result = layout(&[1, 2, 3], &frame);

        assert_eq!(result.len(), 3);
        for (id, window_frame) in &result {
            assert_eq!(*window_frame, frame, "Window {id} should be fullscreen");
        }
    }

    #[test]
    fn test_monocle_preserves_order() {
        let frame = screen_frame();
        let result = layout(&[5, 3, 8, 1], &frame);

        assert_eq!(result.len(), 4);
        assert_eq!(result[0].0, 5);
        assert_eq!(result[1].0, 3);
        assert_eq!(result[2].0, 8);
        assert_eq!(result[3].0, 1);
    }

    #[test]
    fn test_monocle_many_windows() {
        let frame = screen_frame();
        let ids: Vec<u32> = (1..=20).collect();
        let result = layout(&ids, &frame);

        assert_eq!(result.len(), 20);
        for (id, window_frame) in &result {
            assert_eq!(*window_frame, frame, "Window {id} should be fullscreen");
        }
    }

    #[test]
    fn test_monocle_with_offset_frame() {
        // Simulates a frame with outer gaps already applied
        let frame = Rect::new(20.0, 50.0, 1880.0, 1010.0);
        let result = layout(&[1, 2], &frame);

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].1, frame);
        assert_eq!(result[1].1, frame);
    }
}
