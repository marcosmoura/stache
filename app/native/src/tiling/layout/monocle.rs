//! Monocle layout - all windows maximized to fill the screen.

use super::LayoutResult;
use crate::tiling::state::Rect;

/// Monocle layout - all windows maximized to fill the screen.
///
/// Every window gets the full screen frame. This is useful for
/// focusing on a single window at a time while keeping others accessible.
///
/// # Arguments
///
/// * `window_ids` - IDs of windows to arrange
/// * `screen_frame` - The visible frame of the screen (already has outer gaps applied)
#[must_use]
pub fn layout(window_ids: &[u32], screen_frame: &Rect) -> LayoutResult {
    window_ids.iter().map(|&id| (id, *screen_frame)).collect()
}

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
}
