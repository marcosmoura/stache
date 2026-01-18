//! Layout algorithms for the tiling window manager.
//!
//! This module provides different layout algorithms for arranging windows
//! within a workspace. Each layout takes a list of windows and a screen's
//! visible frame, and returns the calculated frames for each window.
//!
//! # Supported Layouts
//!
//! - **Floating**: Windows keep their current positions (no tiling)
//! - **Monocle**: All windows maximized to fill the screen
//! - **Dwindle**: Binary Space Partitioning - windows arranged in a dwindling spiral
//! - **Split**: Windows split evenly (auto, vertical, or horizontal)
//! - **Master**: One master window with remaining windows in a stack
//! - **Grid**: Windows arranged in a balanced grid pattern
//!
//! # Performance
//!
//! Layout results use `SmallVec` to avoid heap allocations for workspaces with
//! up to 16 windows (the common case). This reduces memory allocation overhead
//! during animations and rapid layout recalculations.

mod dwindle;
mod floating;
mod gaps;
mod grid;
mod helpers;
mod master;
mod monocle;
mod split;

pub use floating::{calculate_preset_frame, find_preset, list_preset_names};
pub use gaps::Gaps;
use smallvec::SmallVec;

use crate::config::{LayoutType, MasterPosition};
use crate::tiling::state::Rect;

// ============================================================================
// Layout Result
// ============================================================================

/// Inline capacity for layout results.
///
/// Most workspaces have fewer than 16 windows, so this allows layout results
/// to be stored on the stack without heap allocation in the common case.
pub const LAYOUT_INLINE_CAP: usize = 16;

/// Result of a layout calculation.
///
/// Maps window IDs to their calculated frames. Uses `SmallVec` to avoid heap
/// allocations for workspaces with up to 16 windows.
pub type LayoutResult = SmallVec<[(u32, Rect); LAYOUT_INLINE_CAP]>;

// ============================================================================
// Main Layout Function
// ============================================================================

/// Calculates window frames for a given layout type.
///
/// # Arguments
///
/// * `layout` - The layout algorithm to use
/// * `window_ids` - IDs of windows to arrange (in order)
/// * `screen_frame` - The visible frame of the screen to tile within
/// * `master_ratio` - Ratio for master layout (0.0-1.0, default 0.5)
///
/// # Returns
///
/// A vector of (`window_id`, frame) pairs for each window.
#[must_use]
pub fn calculate_layout(
    layout: LayoutType,
    window_ids: &[u32],
    screen_frame: &Rect,
    master_ratio: f64,
) -> LayoutResult {
    calculate_layout_with_gaps(layout, window_ids, screen_frame, master_ratio, &Gaps::default())
}

/// Calculates window frames for a given layout type with gaps.
///
/// # Arguments
///
/// * `layout` - The layout algorithm to use
/// * `window_ids` - IDs of windows to arrange (in order)
/// * `screen_frame` - The visible frame of the screen to tile within
/// * `master_ratio` - Ratio for master layout (0.0-1.0, default 0.5)
/// * `gaps` - Gap values for spacing
///
/// # Returns
///
/// A vector of (`window_id`, frame) pairs for each window.
#[must_use]
pub fn calculate_layout_with_gaps(
    layout: LayoutType,
    window_ids: &[u32],
    screen_frame: &Rect,
    master_ratio: f64,
    gaps: &Gaps,
) -> LayoutResult {
    calculate_layout_with_gaps_and_ratios(
        layout,
        window_ids,
        screen_frame,
        master_ratio,
        gaps,
        &[],
        MasterPosition::Auto,
    )
}

/// Calculates window frames for a given layout type with gaps and custom ratios.
///
/// # Arguments
///
/// * `layout` - The layout algorithm to use
/// * `window_ids` - IDs of windows to arrange (in order)
/// * `screen_frame` - The visible frame of the screen to tile within
/// * `master_ratio` - Ratio for master layout (0.0-1.0, default 0.5)
/// * `gaps` - Gap values for spacing
/// * `split_ratios` - Custom split ratios for split layouts (cumulative 0.0-1.0)
/// * `master_position` - Position of master window (left/right/top/bottom/auto)
///
/// # Returns
///
/// A vector of (`window_id`, frame) pairs for each window.
#[must_use]
pub fn calculate_layout_with_gaps_and_ratios(
    layout: LayoutType,
    window_ids: &[u32],
    screen_frame: &Rect,
    master_ratio: f64,
    gaps: &Gaps,
    split_ratios: &[f64],
    master_position: MasterPosition,
) -> LayoutResult {
    if window_ids.is_empty() {
        return SmallVec::new();
    }

    // Apply outer gaps to get usable area
    let usable_frame = gaps.apply_outer(screen_frame);

    match layout {
        LayoutType::Floating => floating::layout(window_ids),
        LayoutType::Monocle => monocle::layout(window_ids, &usable_frame),
        LayoutType::Dwindle => dwindle::layout(window_ids, &usable_frame, gaps),
        LayoutType::Split => split::layout_auto(window_ids, &usable_frame, gaps, split_ratios),
        LayoutType::SplitVertical => {
            split::layout_vertical(window_ids, &usable_frame, gaps, split_ratios)
        }
        LayoutType::SplitHorizontal => {
            split::layout_horizontal(window_ids, &usable_frame, gaps, split_ratios)
        }
        LayoutType::Master => {
            master::layout(window_ids, &usable_frame, master_ratio, gaps, master_position)
        }
        LayoutType::Grid => grid::layout(window_ids, &usable_frame, gaps),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_frame() -> Rect { Rect::new(0.0, 0.0, 1920.0, 1080.0) }

    #[test]
    fn test_calculate_layout_empty() {
        let frame = screen_frame();
        let result = calculate_layout(LayoutType::Dwindle, &[], &frame, 0.5);
        assert!(result.is_empty());
    }

    #[test]
    fn test_calculate_layout_routes_correctly() {
        let frame = screen_frame();
        let windows = vec![1, 2];

        // Each layout type should produce results
        let layouts = [
            LayoutType::Monocle,
            LayoutType::Dwindle,
            LayoutType::Split,
            LayoutType::SplitVertical,
            LayoutType::SplitHorizontal,
            LayoutType::Master,
            LayoutType::Grid,
        ];

        for layout in layouts {
            let result = calculate_layout(layout, &windows, &frame, 0.5);
            assert_eq!(result.len(), 2, "Layout {layout:?} should produce 2 results");
        }

        // Floating should return empty (no repositioning)
        let floating = calculate_layout(LayoutType::Floating, &windows, &frame, 0.5);
        assert!(floating.is_empty());
    }

    #[test]
    fn test_calculate_layout_with_outer_gaps() {
        let frame = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let gaps = Gaps {
            inner_h: 10.0,
            inner_v: 10.0,
            outer_top: 50.0,
            outer_right: 20.0,
            outer_bottom: 20.0,
            outer_left: 20.0,
        };

        let result = calculate_layout_with_gaps(LayoutType::Monocle, &[1], &frame, 0.5, &gaps);

        assert_eq!(result.len(), 1);
        let (_, window_frame) = result[0];

        // Window should respect outer gaps
        assert_eq!(window_frame.x, 20.0); // outer_left
        assert_eq!(window_frame.y, 50.0); // outer_top
        assert_eq!(window_frame.width, 960.0); // 1000 - 20 - 20
        assert_eq!(window_frame.height, 730.0); // 800 - 50 - 20
    }

    #[test]
    fn test_dwindle_uses_dwindle_algorithm() {
        // The DWINDLE layout should now use the dwindle algorithm
        // which creates a spiral pattern, not a grid
        let frame = screen_frame();
        let result = calculate_layout(LayoutType::Dwindle, &[1, 2, 3, 4], &frame, 0.5);

        assert_eq!(result.len(), 4);

        let (_, frame1) = result[0];
        let (_, frame2) = result[1];
        let (_, frame3) = result[2];
        let (_, frame4) = result[3];

        // Window 1 should take left half
        assert!((frame1.width - frame.width / 2.0).abs() < 1.0);
        assert_eq!(frame1.height, frame.height);

        // Window 2 should be top-right
        assert!((frame2.width - frame.width / 2.0).abs() < 1.0);
        assert!((frame2.height - frame.height / 2.0).abs() < 1.0);

        // Window 3 should be bottom-right left quarter
        assert!((frame3.width - frame.width / 4.0).abs() < 1.0);

        // Window 4 should be bottom-right right quarter
        assert!((frame4.width - frame.width / 4.0).abs() < 1.0);
    }
}
