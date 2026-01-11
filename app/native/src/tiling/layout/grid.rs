//! Grid layout - windows arranged in a grid pattern.
//!
//! The grid layout arranges windows in rows and columns, aiming for a layout
//! that is as close to square as possible. Each window occupies an equal portion
//! of the workspace, except when the count doesn't fit perfectly into a grid.
//!
//! # Layout Rules
//!
//! - Maximum 12 windows supported (extras are ignored)
//! - Maximum 4 rows or columns
//! - When windows don't fit perfectly, the first window (top-left) is made larger
//!
//! # Examples (Landscape)
//!
//! - 1 window: full screen
//! - 2 windows: side by side (1×2)
//! - 3 windows: W1 left half, W2-W3 stacked on right
//! - 4 windows: 2×2 grid
//! - 5 windows: W1 left half, W2-W5 in 2×2 on right
//! - 6 windows: 2×3 grid
//! - 7 windows: W1 left half, W2-W7 in 3×2 on right
//! - 8 windows: 2×4 grid
//! - 9 windows: 3×3 grid
//! - 10 windows: 3×4 grid with W1 spanning left column (3 cells)
//! - 11 windows: 3×4 grid with W1 spanning 2 cells in left column
//! - 12 windows: 3×4 grid

use super::{Gaps, LayoutResult};
use crate::tiling::state::Rect;

/// Maximum number of windows supported in grid layout.
const MAX_WINDOWS: usize = 12;

/// Grid layout - windows arranged in rows and columns.
///
/// Arranges windows in a grid that is as close to square as possible.
/// When the window count doesn't fit perfectly, the first window is made larger.
///
/// # Arguments
///
/// * `window_ids` - IDs of windows to arrange (max 12, extras ignored)
/// * `screen_frame` - The visible frame of the screen (after outer gaps applied)
/// * `gaps` - Gap values for inner spacing
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn layout(window_ids: &[u32], screen_frame: &Rect, gaps: &Gaps) -> LayoutResult {
    if window_ids.is_empty() {
        return Vec::new();
    }

    // Limit to maximum supported windows
    let window_ids = if window_ids.len() > MAX_WINDOWS {
        &window_ids[..MAX_WINDOWS]
    } else {
        window_ids
    };

    let count = window_ids.len();
    let is_landscape = screen_frame.width >= screen_frame.height;

    match count {
        1 => layout_single(window_ids, screen_frame),
        2 => layout_two(window_ids, screen_frame, gaps, is_landscape),
        3 | 5 => layout_master_stack(window_ids, screen_frame, gaps, 2, is_landscape),
        4 => layout_grid(window_ids, screen_frame, gaps, 2, 2),
        6 => layout_grid(window_ids, screen_frame, gaps, 2, 3),
        7 => layout_master_stack(window_ids, screen_frame, gaps, 3, is_landscape),
        8 => layout_grid(window_ids, screen_frame, gaps, 2, 4),
        9 => layout_grid(window_ids, screen_frame, gaps, 3, 3),
        10 => layout_master_3x4(window_ids, screen_frame, gaps, 3, is_landscape),
        11 => layout_master_3x4(window_ids, screen_frame, gaps, 2, is_landscape),
        _ => layout_grid(window_ids, screen_frame, gaps, 3, 4), // 12 or more (fallback)
    }
}

/// Layout for a single window - takes full screen.
fn layout_single(window_ids: &[u32], screen_frame: &Rect) -> LayoutResult {
    vec![(window_ids[0], *screen_frame)]
}

/// Layout for exactly 2 windows - side by side (landscape) or stacked (portrait).
fn layout_two(
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    is_landscape: bool,
) -> LayoutResult {
    if is_landscape {
        // Side by side
        let gap = gaps.inner_h;
        let width = (screen_frame.width - gap) / 2.0;
        vec![
            (
                window_ids[0],
                Rect::new(screen_frame.x, screen_frame.y, width, screen_frame.height),
            ),
            (
                window_ids[1],
                Rect::new(
                    screen_frame.x + width + gap,
                    screen_frame.y,
                    width,
                    screen_frame.height,
                ),
            ),
        ]
    } else {
        // Stacked vertically
        let gap = gaps.inner_v;
        let height = (screen_frame.height - gap) / 2.0;
        vec![
            (
                window_ids[0],
                Rect::new(screen_frame.x, screen_frame.y, screen_frame.width, height),
            ),
            (
                window_ids[1],
                Rect::new(
                    screen_frame.x,
                    screen_frame.y + height + gap,
                    screen_frame.width,
                    height,
                ),
            ),
        ]
    }
}

/// Layout for a regular grid where all windows have equal size.
///
/// Used for: 4 (2×2), 6 (2×3), 8 (2×4), 9 (3×3), 12 (3×4)
#[allow(clippy::cast_precision_loss)]
fn layout_grid(
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    rows: usize,
    cols: usize,
) -> LayoutResult {
    let count = window_ids.len();

    // Calculate cell dimensions
    let h_gaps_total = gaps.inner_h * (cols - 1) as f64;
    let v_gaps_total = gaps.inner_v * (rows - 1) as f64;
    let cell_width = (screen_frame.width - h_gaps_total) / cols as f64;
    let cell_height = (screen_frame.height - v_gaps_total) / rows as f64;

    let mut result = Vec::with_capacity(count);
    let mut idx = 0;

    for row in 0..rows {
        let y = (row as f64).mul_add(cell_height + gaps.inner_v, screen_frame.y);

        for col in 0..cols {
            if idx >= count {
                break;
            }

            let x = (col as f64).mul_add(cell_width + gaps.inner_h, screen_frame.x);
            result.push((window_ids[idx], Rect::new(x, y, cell_width, cell_height)));
            idx += 1;
        }
    }

    result
}

/// Master-stack layout: W1 takes left column, remaining windows fill the rest.
///
/// Used for: 3, 5, 7 windows
///
/// All columns have equal width. W1 spans all rows in the first column.
///
/// For landscape:
/// - 3 windows: 2×2 grid, W1 spans left column
///   ```text
///   +----+----+
///   |    | W2 |
///   | W1 +----+
///   |    | W3 |
///   +----+----+
///   ```
/// - 5 windows: 2×3 grid, W1 spans left column
///   ```text
///   +----+----+----+
///   |    | W2 | W3 |
///   | W1 +----+----+
///   |    | W4 | W5 |
///   +----+----+----+
///   ```
/// - 7 windows: 3×3 grid, W1 spans left column
///   ```text
///   +----+----+----+
///   |    | W2 | W3 |
///   | W1 +----+----+
///   |    | W4 | W5 |
///   +----+----+----+
///   |    | W6 | W7 |
///   +----+----+----+
///   ```
#[allow(clippy::cast_precision_loss, clippy::cast_lossless)]
fn layout_master_stack(
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    rows: usize,
    is_landscape: bool,
) -> LayoutResult {
    let count = window_ids.len();
    let stack_count = count - 1;
    let stack_cols = stack_count.div_ceil(rows);
    let total_cols = 1 + stack_cols; // W1 column + stack columns

    let mut result = Vec::with_capacity(count);

    if is_landscape {
        // Calculate cell dimensions with equal column widths
        let h_gaps_total = gaps.inner_h * (total_cols - 1) as f64;
        let v_gaps_total = gaps.inner_v * (rows - 1) as f64;
        let cell_width = (screen_frame.width - h_gaps_total) / total_cols as f64;
        let cell_height = (screen_frame.height - v_gaps_total) / rows as f64;

        // W1: first column, spans all rows
        let master_height = cell_height * rows as f64 + gaps.inner_v * (rows - 1) as f64;
        result.push((
            window_ids[0],
            Rect::new(screen_frame.x, screen_frame.y, cell_width, master_height),
        ));

        // Remaining windows fill the grid (columns 1+)
        let mut idx = 1;
        for row in 0..rows {
            let y = (row as f64).mul_add(cell_height + gaps.inner_v, screen_frame.y);

            for col in 1..total_cols {
                if idx >= count {
                    break;
                }
                let x = (col as f64).mul_add(cell_width + gaps.inner_h, screen_frame.x);
                result.push((window_ids[idx], Rect::new(x, y, cell_width, cell_height)));
                idx += 1;
            }
        }
    } else {
        // Portrait: W1 spans top row, stack fills remaining rows
        let total_rows = 1 + rows; // W1 row + stack rows
        let stack_rows_count = rows;

        let h_gaps_total = gaps.inner_h * (stack_cols - 1) as f64;
        let v_gaps_total = gaps.inner_v * (total_rows - 1) as f64;
        let cell_width = (screen_frame.width - h_gaps_total) / stack_cols as f64;
        let cell_height = (screen_frame.height - v_gaps_total) / total_rows as f64;

        // W1: first row, spans all columns
        let master_width = cell_width * stack_cols as f64 + gaps.inner_h * (stack_cols - 1) as f64;
        result.push((
            window_ids[0],
            Rect::new(screen_frame.x, screen_frame.y, master_width, cell_height),
        ));

        // Remaining windows fill the grid (rows 1+)
        let mut idx = 1;
        for row in 1..=stack_rows_count {
            let y = (row as f64).mul_add(cell_height + gaps.inner_v, screen_frame.y);

            for col in 0..stack_cols {
                if idx >= count {
                    break;
                }
                let x = (col as f64).mul_add(cell_width + gaps.inner_h, screen_frame.x);
                result.push((window_ids[idx], Rect::new(x, y, cell_width, cell_height)));
                idx += 1;
            }
        }
    }

    result
}

/// 3×4 grid layout with W1 spanning multiple cells in the left column.
///
/// Used for: 10, 11 windows
///
/// For 10 windows: W1 spans 3 cells (entire left column)
/// ```text
/// +----+----+----+----+
/// |    | W2 | W3 | W4 |
/// | W1 +----+----+----+
/// |    | W5 | W6 | W7 |
/// +----+----+----+----+
/// |    | W8 | W9 |W10 |
/// +----+----+----+----+
/// ```
///
/// For 11 windows: W1 spans 2 cells (top 2 rows of left column)
/// ```text
/// +----+----+----+----+
/// |    | W2 | W3 | W4 |
/// | W1 +----+----+----+
/// |    | W5 | W6 | W7 |
/// +----+----+----+----+
/// | W8 | W9 |W10 |W11 |
/// +----+----+----+----+
/// ```
#[allow(clippy::cast_precision_loss, clippy::cast_lossless)]
fn layout_master_3x4(
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    master_span: usize, // How many rows W1 spans (2 or 3)
    is_landscape: bool,
) -> LayoutResult {
    let count = window_ids.len();
    let (rows, cols) = if is_landscape { (3, 4) } else { (4, 3) };

    // Calculate cell dimensions
    let h_gaps_total = gaps.inner_h * (cols - 1) as f64;
    let v_gaps_total = gaps.inner_v * (rows - 1) as f64;
    let cell_width = (screen_frame.width - h_gaps_total) / cols as f64;
    let cell_height = (screen_frame.height - v_gaps_total) / rows as f64;

    let mut result = Vec::with_capacity(count);

    // W1: spans master_span rows in the first column
    let master_height = cell_height * master_span as f64 + gaps.inner_v * (master_span - 1) as f64;
    result.push((
        window_ids[0],
        Rect::new(screen_frame.x, screen_frame.y, cell_width, master_height),
    ));

    // Remaining windows fill the grid
    let mut idx = 1;
    for row in 0..rows {
        let y = (row as f64).mul_add(cell_height + gaps.inner_v, screen_frame.y);

        // For rows covered by W1, start from column 1
        // For rows after W1, start from column 0
        let start_col = usize::from(row < master_span);

        for col in start_col..cols {
            if idx >= count {
                break;
            }
            let x = (col as f64).mul_add(cell_width + gaps.inner_h, screen_frame.x);
            result.push((window_ids[idx], Rect::new(x, y, cell_width, cell_height)));
            idx += 1;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_frame() -> Rect { Rect::new(0.0, 0.0, 1920.0, 1080.0) }

    fn portrait_frame() -> Rect { Rect::new(0.0, 0.0, 1080.0, 1920.0) }

    fn no_gaps() -> Gaps { Gaps::default() }

    // ========================================================================
    // Basic Layout Tests
    // ========================================================================

    #[test]
    fn test_layout_empty() {
        let result = layout(&[], &screen_frame(), &no_gaps());
        assert!(result.is_empty());
    }

    #[test]
    fn test_layout_single_window() {
        let frame = screen_frame();
        let result = layout(&[1], &frame, &no_gaps());

        assert_eq!(result.len(), 1);
        let (id, window_frame) = result[0];
        assert_eq!(id, 1);
        assert_eq!(window_frame, frame);
    }

    #[test]
    fn test_layout_two_windows_landscape() {
        let frame = screen_frame();
        let result = layout(&[1, 2], &frame, &no_gaps());

        assert_eq!(result.len(), 2);

        let (_, w1) = result[0];
        let (_, w2) = result[1];

        // Should be side by side
        assert_eq!(w1.height, frame.height);
        assert_eq!(w2.height, frame.height);
        assert!((w1.width - frame.width / 2.0).abs() < 1.0);
        assert!((w2.width - frame.width / 2.0).abs() < 1.0);
        assert_eq!(w1.x, frame.x);
        assert!((w2.x - frame.width / 2.0).abs() < 1.0);
    }

    #[test]
    fn test_layout_two_windows_portrait() {
        let frame = portrait_frame();
        let result = layout(&[1, 2], &frame, &no_gaps());

        assert_eq!(result.len(), 2);

        let (_, w1) = result[0];
        let (_, w2) = result[1];

        // Should be stacked
        assert_eq!(w1.width, frame.width);
        assert_eq!(w2.width, frame.width);
        assert!((w1.height - frame.height / 2.0).abs() < 1.0);
        assert!((w2.height - frame.height / 2.0).abs() < 1.0);
    }

    // ========================================================================
    // Master-Stack Layout Tests (3, 5, 7 windows)
    // ========================================================================

    #[test]
    fn test_layout_three_windows_landscape() {
        // W1 left half, W2-W3 stacked on right
        let frame = screen_frame();
        let result = layout(&[1, 2, 3], &frame, &no_gaps());

        assert_eq!(result.len(), 3);

        let (_, w1) = result[0];
        let (_, w2) = result[1];
        let (_, w3) = result[2];

        let half_width = frame.width / 2.0;
        let half_height = frame.height / 2.0;

        // W1: left half, full height
        assert_eq!(w1.x, frame.x);
        assert_eq!(w1.y, frame.y);
        assert!((w1.width - half_width).abs() < 1.0);
        assert_eq!(w1.height, frame.height);

        // W2: top-right
        assert!((w2.x - half_width).abs() < 1.0);
        assert_eq!(w2.y, frame.y);
        assert!((w2.height - half_height).abs() < 1.0);

        // W3: bottom-right
        assert!((w3.x - half_width).abs() < 1.0);
        assert!((w3.y - half_height).abs() < 1.0);
    }

    #[test]
    fn test_layout_five_windows_landscape() {
        // 5 windows: 2 rows × 3 columns grid, W1 spans left column
        // +----+----+----+
        // |    | W2 | W3 |
        // | W1 +----+----+
        // |    | W4 | W5 |
        // +----+----+----+
        let frame = screen_frame();
        let result = layout(&[1, 2, 3, 4, 5], &frame, &no_gaps());

        assert_eq!(result.len(), 5);

        let (_, w1) = result[0];
        let col_width = frame.width / 3.0;

        // W1: first column, full height (spans 2 rows)
        assert_eq!(w1.x, frame.x);
        assert!((w1.width - col_width).abs() < 1.0);
        assert_eq!(w1.height, frame.height);

        // W2-W5 should be in columns 1-2
        for i in 1..5 {
            let (_, w) = result[i];
            assert!(w.x >= col_width - 1.0, "Window {} should be in cols 1-2", i + 1);
            assert!(
                (w.width - col_width).abs() < 1.0,
                "Window {} should have equal width",
                i + 1
            );
        }
    }

    #[test]
    fn test_layout_seven_windows_landscape() {
        // 7 windows: 3 rows × 3 columns grid, W1 spans left column
        // +----+----+----+
        // |    | W2 | W3 |
        // | W1 +----+----+
        // |    | W4 | W5 |
        // +----+----+----+
        // |    | W6 | W7 |
        // +----+----+----+
        let frame = screen_frame();
        let result = layout(&[1, 2, 3, 4, 5, 6, 7], &frame, &no_gaps());

        assert_eq!(result.len(), 7);

        let (_, w1) = result[0];
        let col_width = frame.width / 3.0;

        // W1: first column, full height (spans 3 rows)
        assert!((w1.width - col_width).abs() < 1.0);
        assert_eq!(w1.height, frame.height);

        // W2-W7 should be in columns 1-2
        for i in 1..7 {
            let (_, w) = result[i];
            assert!(w.x >= col_width - 1.0, "Window {} should be in cols 1-2", i + 1);
            assert!(
                (w.width - col_width).abs() < 1.0,
                "Window {} should have equal width",
                i + 1
            );
        }
    }

    // ========================================================================
    // Regular Grid Layout Tests (4, 6, 8, 9, 12 windows)
    // ========================================================================

    #[test]
    fn test_layout_four_windows_landscape() {
        // 2×2 grid
        let frame = screen_frame();
        let result = layout(&[1, 2, 3, 4], &frame, &no_gaps());

        assert_eq!(result.len(), 4);

        let (_, w1) = result[0];
        let half_width = frame.width / 2.0;
        let half_height = frame.height / 2.0;

        // All windows should be quarter size
        assert!((w1.width - half_width).abs() < 1.0);
        assert!((w1.height - half_height).abs() < 1.0);

        // Check positions
        assert_eq!(result[0].1.x, frame.x); // W1: top-left
        assert_eq!(result[0].1.y, frame.y);
        assert!((result[1].1.x - half_width).abs() < 1.0); // W2: top-right
        assert_eq!(result[2].1.x, frame.x); // W3: bottom-left
        assert!((result[3].1.x - half_width).abs() < 1.0); // W4: bottom-right
    }

    #[test]
    fn test_layout_six_windows_landscape() {
        // 2×3 grid
        let frame = screen_frame();
        let result = layout(&[1, 2, 3, 4, 5, 6], &frame, &no_gaps());

        assert_eq!(result.len(), 6);

        // All windows should have same dimensions
        let (_, w1) = result[0];
        let (_, w2) = result[1];
        assert!((w1.width - w2.width).abs() < 1.0);
        assert!((w1.height - w2.height).abs() < 1.0);

        // Should be 2 rows × 3 cols
        let cell_width = frame.width / 3.0;
        let cell_height = frame.height / 2.0;
        assert!((w1.width - cell_width).abs() < 1.0);
        assert!((w1.height - cell_height).abs() < 1.0);
    }

    #[test]
    fn test_layout_eight_windows_landscape() {
        // 2×4 grid
        let frame = screen_frame();
        let result = layout(&[1, 2, 3, 4, 5, 6, 7, 8], &frame, &no_gaps());

        assert_eq!(result.len(), 8);

        let cell_width = frame.width / 4.0;
        let cell_height = frame.height / 2.0;

        let (_, w1) = result[0];
        assert!((w1.width - cell_width).abs() < 1.0);
        assert!((w1.height - cell_height).abs() < 1.0);
    }

    #[test]
    fn test_layout_nine_windows_landscape() {
        // 3×3 grid
        let frame = screen_frame();
        let ids: Vec<u32> = (1..=9).collect();
        let result = layout(&ids, &frame, &no_gaps());

        assert_eq!(result.len(), 9);

        let cell_size = frame.width / 3.0;
        let cell_height = frame.height / 3.0;

        let (_, w1) = result[0];
        assert!((w1.width - cell_size).abs() < 1.0);
        assert!((w1.height - cell_height).abs() < 1.0);
    }

    #[test]
    fn test_layout_twelve_windows_landscape() {
        // 3×4 grid
        let frame = screen_frame();
        let ids: Vec<u32> = (1..=12).collect();
        let result = layout(&ids, &frame, &no_gaps());

        assert_eq!(result.len(), 12);

        let cell_width = frame.width / 4.0;
        let cell_height = frame.height / 3.0;

        let (_, w1) = result[0];
        assert!((w1.width - cell_width).abs() < 1.0);
        assert!((w1.height - cell_height).abs() < 1.0);
    }

    // ========================================================================
    // 3×4 Master Layout Tests (10, 11 windows)
    // ========================================================================

    #[test]
    fn test_layout_ten_windows_landscape() {
        // 3×4 grid with W1 spanning left column (3 cells)
        let frame = screen_frame();
        let ids: Vec<u32> = (1..=10).collect();
        let result = layout(&ids, &frame, &no_gaps());

        assert_eq!(result.len(), 10);

        let cell_width = frame.width / 4.0;

        let (_, w1) = result[0];

        // W1: spans full height (3 rows)
        assert_eq!(w1.x, frame.x);
        assert_eq!(w1.y, frame.y);
        assert!((w1.width - cell_width).abs() < 1.0);
        assert_eq!(w1.height, frame.height);

        // W2-W10 should all be on columns 1-3
        for i in 1..10 {
            let (_, w) = result[i];
            assert!(w.x >= cell_width - 1.0, "Window {} should be in cols 1-3", i + 1);
        }
    }

    #[test]
    fn test_layout_eleven_windows_landscape() {
        // 3×4 grid with W1 spanning 2 cells in left column
        let frame = screen_frame();
        let ids: Vec<u32> = (1..=11).collect();
        let result = layout(&ids, &frame, &no_gaps());

        assert_eq!(result.len(), 11);

        let cell_width = frame.width / 4.0;
        let cell_height = frame.height / 3.0;

        let (_, w1) = result[0];

        // W1: spans 2 rows
        assert_eq!(w1.x, frame.x);
        assert_eq!(w1.y, frame.y);
        assert!((w1.width - cell_width).abs() < 1.0);
        assert!((w1.height - cell_height * 2.0).abs() < 1.0);

        // W8 should be at row 2, col 0 (below W1)
        let (_, w8) = result[7];
        assert_eq!(w8.x, frame.x);
        assert!((w8.y - cell_height * 2.0).abs() < 1.0);
    }

    // ========================================================================
    // Edge Cases
    // ========================================================================

    #[test]
    fn test_layout_max_windows_exceeded() {
        // More than 12 windows should be truncated
        let frame = screen_frame();
        let ids: Vec<u32> = (1..=20).collect();
        let result = layout(&ids, &frame, &no_gaps());

        assert_eq!(result.len(), MAX_WINDOWS);
    }

    #[test]
    fn test_layout_with_gaps() {
        let frame = screen_frame();
        let gaps = Gaps::uniform(16.0, 0.0);
        let result = layout(&[1, 2], &frame, &gaps);

        let (_, w1) = result[0];
        let (_, w2) = result[1];

        // Check gap between windows
        let h_gap = w2.x - (w1.x + w1.width);
        assert!((h_gap - 16.0).abs() < 0.1);
    }

    #[test]
    fn test_layout_four_windows_with_gaps() {
        let frame = screen_frame();
        let gaps = Gaps::uniform(16.0, 0.0);
        let result = layout(&[1, 2, 3, 4], &frame, &gaps);

        let (_, w1) = result[0];
        let (_, w2) = result[1];
        let (_, w3) = result[2];

        // Check horizontal gap
        let h_gap = w2.x - (w1.x + w1.width);
        assert!((h_gap - 16.0).abs() < 0.1);

        // Check vertical gap
        let v_gap = w3.y - (w1.y + w1.height);
        assert!((v_gap - 16.0).abs() < 0.1);
    }

    // ========================================================================
    // No Overlapping Tests
    // ========================================================================

    fn rects_overlap(a: &Rect, b: &Rect) -> bool {
        let eps = 0.1;
        let a_right = a.x + a.width;
        let a_bottom = a.y + a.height;
        let b_right = b.x + b.width;
        let b_bottom = b.y + b.height;

        a.x + eps < b_right && a_right > b.x + eps && a.y + eps < b_bottom && a_bottom > b.y + eps
    }

    #[test]
    fn test_no_overlapping_windows() {
        let frame = screen_frame();
        let gaps = Gaps::uniform(16.0, 0.0);

        // Test all window counts from 1 to 12
        for count in 1..=12 {
            let ids: Vec<u32> = (1..=count).collect();
            let result = layout(&ids, &frame, &gaps);

            assert_eq!(result.len(), count as usize, "Wrong count for {count} windows");

            // Check no two windows overlap
            for i in 0..result.len() {
                for j in (i + 1)..result.len() {
                    let (id_i, rect_i) = &result[i];
                    let (id_j, rect_j) = &result[j];
                    assert!(
                        !rects_overlap(rect_i, rect_j),
                        "Windows {id_i} and {id_j} overlap for {count} windows!"
                    );
                }
            }
        }
    }

    // ========================================================================
    // Portrait Mode Tests
    // ========================================================================

    #[test]
    fn test_layout_three_windows_portrait() {
        // Portrait 3 windows: W1 at top, W2-W3 below
        // All rows have equal height (3 total rows)
        // +--------+
        // |   W1   |
        // +--------+
        // |   W2   |
        // +--------+
        // |   W3   |
        // +--------+
        let frame = portrait_frame();
        let result = layout(&[1, 2, 3], &frame, &no_gaps());

        assert_eq!(result.len(), 3);

        let (_, w1) = result[0];
        let row_height = frame.height / 3.0;

        // W1: first row, full width
        assert_eq!(w1.x, frame.x);
        assert_eq!(w1.y, frame.y);
        assert_eq!(w1.width, frame.width);
        assert!((w1.height - row_height).abs() < 1.0);
    }
}
