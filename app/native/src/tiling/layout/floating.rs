//! Floating layout - windows keep their current positions.

use super::LayoutResult;

/// Floating layout - windows keep their current positions.
///
/// Returns an empty result since no repositioning is needed.
/// The tiling manager will skip applying positions for floating windows.
#[allow(clippy::missing_const_for_fn)] // Can't be const due to Vec::new()
#[must_use]
pub fn layout(_window_ids: &[u32]) -> LayoutResult {
    // Floating windows don't get repositioned by the layout engine
    // Return empty to indicate no changes needed
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_floating_returns_empty() {
        let result = layout(&[1, 2, 3]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_floating_empty_input() {
        let result = layout(&[]);
        assert!(result.is_empty());
    }
}
