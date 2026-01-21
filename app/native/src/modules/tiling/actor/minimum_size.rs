//! Minimum window size enforcement for layouts.
//!
//! This module provides functions to enforce minimum window sizes across different
//! layout types (Split, Dwindle, Grid). When windows have minimum size constraints,
//! the layout ratios are adjusted to ensure all windows meet their requirements.
//!
//! # Architecture
//!
//! The enforcement process works in two phases:
//! 1. Detect violations - Check if any window's calculated frame is smaller than its minimum
//! 2. Adjust ratios - Redistribute space to satisfy minimum constraints while preserving
//!    relative sizes where possible
//!
//! # Supported Layouts
//!
//! - **Split/SplitHorizontal/SplitVertical**: Linear split with cumulative ratios
//! - **Dwindle**: Binary tree structure with per-level ratios
//! - **Grid**: Grid-based layout with primary ratio adjustment

use crate::modules::tiling::layout::{Gaps, LayoutResult, MasterPosition, calculate_layout_full};
use crate::modules::tiling::state::{LayoutType, Rect, Window};

// ============================================================================
// Split Layout Enforcement
// ============================================================================

/// Enforces minimum window sizes for split layouts by adjusting ratios.
///
/// If any window would be smaller than its minimum size, this function:
/// 1. Calculates the minimum ratio each window needs
/// 2. Adjusts the split ratios to accommodate minimums
/// 3. Returns a new layout with adjusted positions
///
/// Returns `None` if no adjustments are needed.
#[allow(clippy::too_many_arguments)]
pub fn enforce_minimum_sizes_for_split(
    initial_result: &LayoutResult,
    layoutable_windows: &[Window],
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    layout: LayoutType,
    current_ratios: &[f64],
) -> Option<LayoutResult> {
    if window_ids.len() < 2 {
        return None; // Single window always gets full space
    }

    // Determine if horizontal or vertical split
    let is_horizontal = matches!(
        layout,
        LayoutType::SplitHorizontal
            | LayoutType::Split if screen_frame.width >= screen_frame.height
    );

    // Get usable dimension (accounting for outer gaps)
    let usable_frame = gaps.apply_outer(screen_frame);
    let total_dimension = if is_horizontal {
        usable_frame.width
    } else {
        usable_frame.height
    };

    // Account for inner gaps between windows
    let inner_gap = if is_horizontal {
        gaps.inner_h
    } else {
        gaps.inner_v
    };
    #[allow(clippy::cast_precision_loss)]
    let total_gaps = inner_gap * (window_ids.len() - 1) as f64;
    let available_space = total_dimension - total_gaps;

    if available_space <= 0.0 {
        return None; // Can't layout if no space
    }

    // Build minimum size map (using effective_minimum_size to include inferred minimums)
    let min_sizes: Vec<f64> = window_ids
        .iter()
        .map(|&id| {
            layoutable_windows
                .iter()
                .find(|w| w.id == id)
                .and_then(Window::effective_minimum_size)
                .map_or(0.0, |(min_w, min_h)| if is_horizontal { min_w } else { min_h })
        })
        .collect();

    // Check for violations in initial layout
    let mut has_violations = false;
    for (window_id, frame) in initial_result {
        if let Some((min_w, min_h)) = layoutable_windows
            .iter()
            .find(|w| w.id == *window_id)
            .and_then(Window::effective_minimum_size)
        {
            let current_dim = if is_horizontal {
                frame.width
            } else {
                frame.height
            };
            let min_dim = if is_horizontal { min_w } else { min_h };
            if current_dim < min_dim - 1.0 {
                has_violations = true;
                break;
            }
        }
    }

    if !has_violations {
        return None; // No adjustments needed
    }

    log::debug!("Minimum size violations detected in split layout, adjusting ratios");

    // Calculate minimum ratios for each window
    let min_ratios: Vec<f64> =
        min_sizes.iter().map(|&min| (min / available_space).min(1.0)).collect();

    // Check if total minimum requirements exceed available space
    let total_min_ratio: f64 = min_ratios.iter().sum();
    if total_min_ratio > 1.0 {
        log::warn!(
            "Total minimum size requirements ({:.2}%) exceed available space, \
             some windows will be smaller than their minimums",
            total_min_ratio * 100.0
        );
        // Scale down minimums proportionally
        let scale = 1.0 / total_min_ratio;
        let scaled_min_ratios: Vec<f64> = min_ratios.iter().map(|r| r * scale).collect();
        return Some(compute_layout_with_ratios(
            &scaled_min_ratios,
            window_ids,
            &usable_frame,
            gaps,
            is_horizontal,
        ));
    }

    // Compute adjusted ratios that respect minimums while preserving relative sizes
    // where possible
    let adjusted_ratios = compute_adjusted_ratios(current_ratios, &min_ratios, window_ids.len());

    Some(compute_layout_with_ratios(
        &adjusted_ratios,
        window_ids,
        &usable_frame,
        gaps,
        is_horizontal,
    ))
}

/// Computes adjusted window ratios that respect minimum sizes.
///
/// Takes cumulative ratios (0.0 to 1.0) and minimum ratios per window,
/// returns adjusted window size ratios (not cumulative).
pub fn compute_adjusted_ratios(
    cumulative_ratios: &[f64],
    min_ratios: &[f64],
    window_count: usize,
) -> Vec<f64> {
    // Convert cumulative ratios to per-window ratios
    #[allow(clippy::cast_precision_loss)]
    let mut window_ratios: Vec<f64> = if cumulative_ratios.is_empty() {
        // Default: equal distribution
        vec![1.0 / window_count as f64; window_count]
    } else {
        let mut ratios = Vec::with_capacity(window_count);
        for i in 0..window_count {
            let start = if i == 0 {
                0.0
            } else {
                cumulative_ratios[i - 1]
            };
            let end = if i < cumulative_ratios.len() {
                cumulative_ratios[i]
            } else {
                1.0
            };
            ratios.push(end - start);
        }
        ratios
    };

    // Ensure each window meets its minimum
    for i in 0..window_count {
        if window_ratios[i] < min_ratios[i] {
            let deficit = min_ratios[i] - window_ratios[i];
            window_ratios[i] = min_ratios[i];

            // Take space from other windows that have room
            let mut remaining_deficit = deficit;
            for j in 0..window_count {
                if j != i && remaining_deficit > 0.0 {
                    let available = window_ratios[j] - min_ratios[j];
                    if available > 0.0 {
                        let take = available.min(remaining_deficit);
                        window_ratios[j] -= take;
                        remaining_deficit -= take;
                    }
                }
            }
        }
    }

    // Normalize to ensure sum is exactly 1.0
    let sum: f64 = window_ratios.iter().sum();
    if (sum - 1.0).abs() > 0.001 {
        for ratio in &mut window_ratios {
            *ratio /= sum;
        }
    }

    window_ratios
}

/// Computes layout frames from window size ratios.
pub fn compute_layout_with_ratios(
    window_ratios: &[f64],
    window_ids: &[u32],
    usable_frame: &Rect,
    gaps: &Gaps,
    is_horizontal: bool,
) -> LayoutResult {
    use smallvec::SmallVec;

    let mut result: LayoutResult = SmallVec::new();
    let inner_gap = if is_horizontal {
        gaps.inner_h
    } else {
        gaps.inner_v
    };
    #[allow(clippy::cast_precision_loss)]
    let total_gaps = inner_gap * (window_ids.len() - 1) as f64;

    let total_dimension = if is_horizontal {
        usable_frame.width - total_gaps
    } else {
        usable_frame.height - total_gaps
    };

    let mut position = if is_horizontal {
        usable_frame.x
    } else {
        usable_frame.y
    };

    for (i, &window_id) in window_ids.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let size = total_dimension
            * window_ratios.get(i).copied().unwrap_or(1.0 / window_ids.len() as f64);

        let frame = if is_horizontal {
            Rect::new(position, usable_frame.y, size, usable_frame.height)
        } else {
            Rect::new(usable_frame.x, position, usable_frame.width, size)
        };

        result.push((window_id, frame));
        position += size + inner_gap;
    }

    result
}

// ============================================================================
// Dwindle Layout Enforcement
// ============================================================================

/// Enforces minimum window sizes for Dwindle layout by adjusting ratios.
///
/// Dwindle uses a binary tree structure where each ratio controls a split level.
/// This implementation uses proportional adjustments based on violation severity
/// for faster convergence (typically 1-3 iterations instead of 10).
#[allow(clippy::too_many_lines)]
pub fn enforce_minimum_sizes_for_dwindle(
    initial_result: &LayoutResult,
    layoutable_windows: &[Window],
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    current_ratios: &[f64],
) -> Option<LayoutResult> {
    // Reduced from 10 - proportional adjustments converge faster
    const MAX_ITERATIONS: usize = 3;

    if window_ids.len() < 2 {
        return None;
    }

    // Build minimum size lookup for O(1) access
    let min_sizes: std::collections::HashMap<u32, (f64, f64)> = layoutable_windows
        .iter()
        .filter_map(|w| w.effective_minimum_size().map(|min| (w.id, min)))
        .collect();

    // Early exit: no windows have minimum sizes
    if min_sizes.is_empty() {
        return None;
    }

    // Find violations and their indices
    let violations = find_minimum_size_violations(initial_result, layoutable_windows);
    if violations.is_empty() {
        return None;
    }

    log::debug!(
        "Minimum size violations in dwindle layout for {} windows",
        violations.len()
    );

    let mut ratios = if current_ratios.is_empty() {
        vec![0.5; window_ids.len().saturating_sub(1)]
    } else {
        current_ratios.to_vec()
    };

    // Ensure we have enough ratios
    while ratios.len() < window_ids.len().saturating_sub(1) {
        ratios.push(0.5);
    }

    let is_landscape = screen_frame.width >= screen_frame.height;

    for _iteration in 0..MAX_ITERATIONS {
        // Collect adjustment magnitudes based on violation severity
        let mut adjustments: Vec<(usize, f64)> = Vec::new();

        for &(window_idx, violation_axis) in &violations {
            // Get the window's frame and minimum size
            let Some((_, frame)) = initial_result.get(window_idx) else {
                continue;
            };
            let window_id = window_ids.get(window_idx).copied().unwrap_or(0);
            let Some(&(min_w, min_h)) = min_sizes.get(&window_id) else {
                continue;
            };

            // Calculate proportional adjustment based on violation magnitude
            let width_deficit = (min_w - frame.width).max(0.0);
            let height_deficit = (min_h - frame.height).max(0.0);

            let width_violated = violation_axis == 0 || violation_axis == 2;
            let height_violated = violation_axis == 1 || violation_axis == 2;

            if window_idx == 0 {
                // Window 0 gets space from the first split
                if !ratios.is_empty() {
                    let is_h = is_dwindle_split_horizontal(0, is_landscape);
                    let deficit = if is_h { width_deficit } else { height_deficit };
                    let total_dim = if is_h {
                        screen_frame.width
                    } else {
                        screen_frame.height
                    };
                    // Proportional adjustment: how much ratio change needed
                    let adjustment = (deficit / total_dim).min(0.3);
                    if adjustment > 0.01 {
                        adjustments.push((0, adjustment));
                    }
                }
            } else {
                let ratio_idx = window_idx - 1;
                if ratio_idx < ratios.len() {
                    let is_h_split = is_dwindle_split_horizontal(window_idx, is_landscape);

                    if (is_h_split && width_violated) || (!is_h_split && height_violated) {
                        let deficit = if is_h_split {
                            width_deficit
                        } else {
                            height_deficit
                        };
                        let total_dim = if is_h_split {
                            screen_frame.width
                        } else {
                            screen_frame.height
                        };
                        let adjustment = (deficit / total_dim).min(0.3);
                        if adjustment > 0.01 {
                            // Negative adjustment to give more space to second half
                            adjustments.push((ratio_idx, -adjustment));
                        }
                    }
                }
            }
        }

        // Apply all adjustments
        for (idx, adj) in adjustments {
            ratios[idx] = (ratios[idx] + adj).clamp(0.1, 0.9);
        }

        // Recompute layout with adjusted ratios
        let new_result = calculate_layout_full(
            LayoutType::Dwindle,
            window_ids,
            screen_frame,
            0.5,
            gaps,
            &ratios,
            MasterPosition::Auto,
        );

        // Check if violations are resolved
        let new_violations = find_minimum_size_violations(&new_result, layoutable_windows);
        if new_violations.is_empty() {
            return Some(new_result);
        }
    }

    // After max iterations, return best effort
    let final_result = calculate_layout_full(
        LayoutType::Dwindle,
        window_ids,
        screen_frame,
        0.5,
        gaps,
        &ratios,
        MasterPosition::Auto,
    );
    Some(final_result)
}

/// Determines if a Dwindle split at the given index is horizontal.
pub const fn is_dwindle_split_horizontal(split_index: usize, is_landscape: bool) -> bool {
    if is_landscape {
        !split_index.is_multiple_of(2)
    } else {
        split_index.is_multiple_of(2)
    }
}

// ============================================================================
// Grid Layout Enforcement
// ============================================================================

/// Enforces minimum window sizes for Grid layout by adjusting ratios.
#[allow(clippy::too_many_lines)]
pub fn enforce_minimum_sizes_for_grid(
    initial_result: &LayoutResult,
    layoutable_windows: &[Window],
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    current_ratios: &[f64],
) -> Option<LayoutResult> {
    // Reduced from 10 - proportional adjustments converge faster
    const MAX_ITERATIONS: usize = 3;

    if window_ids.len() < 2 {
        return None;
    }

    // Build minimum size lookup for O(1) access
    let min_sizes: std::collections::HashMap<u32, (f64, f64)> = layoutable_windows
        .iter()
        .filter_map(|w| w.effective_minimum_size().map(|min| (w.id, min)))
        .collect();

    // Early exit: no windows have minimum sizes
    if min_sizes.is_empty() {
        return None;
    }

    // Find violations
    let violations = find_minimum_size_violations(initial_result, layoutable_windows);
    if violations.is_empty() {
        return None;
    }

    log::debug!(
        "Minimum size violations in grid layout for {} windows",
        violations.len()
    );

    // Grid layout ratio interpretation varies by window count
    // For simplicity, we'll focus on the primary ratio (first one)
    let mut ratios = if current_ratios.is_empty() {
        vec![0.5]
    } else {
        current_ratios.to_vec()
    };

    let is_landscape = screen_frame.width >= screen_frame.height;

    for _iteration in 0..MAX_ITERATIONS {
        // Collect proportional adjustments based on violation severity
        let mut total_adjustment: f64 = 0.0;

        for &(window_idx, violation_axis) in &violations {
            // Get the window's frame and minimum size
            let Some((_, frame)) = initial_result.get(window_idx) else {
                continue;
            };
            let window_id = window_ids.get(window_idx).copied().unwrap_or(0);
            let Some(&(min_w, min_h)) = min_sizes.get(&window_id) else {
                continue;
            };

            // Calculate proportional adjustment based on violation magnitude
            let width_deficit = (min_w - frame.width).max(0.0);
            let height_deficit = (min_h - frame.height).max(0.0);

            let width_violated = violation_axis == 0 || violation_axis == 2;
            let height_violated = violation_axis == 1 || violation_axis == 2;

            // Determine relevant deficit based on layout orientation
            let relevant_deficit = if is_landscape && width_violated {
                width_deficit
            } else if !is_landscape && height_violated {
                height_deficit
            } else {
                continue;
            };

            let total_dim = if is_landscape {
                screen_frame.width
            } else {
                screen_frame.height
            };

            // Proportional adjustment: how much ratio change needed
            let adjustment = (relevant_deficit / total_dim).min(0.3);
            if adjustment < 0.01 {
                continue;
            }

            if window_ids.len() == 2 {
                // Two windows: side by side (landscape) or stacked (portrait)
                // First ratio controls the split
                if window_idx == 0 {
                    // First window needs more space
                    total_adjustment += adjustment;
                } else {
                    // Second window needs more space
                    total_adjustment -= adjustment;
                }
            } else if matches!(window_ids.len(), 3 | 5 | 7) {
                // Master-stack layouts: first ratio controls master width/height
                if window_idx == 0 {
                    // Master window needs more space
                    total_adjustment += adjustment;
                } else {
                    // Stack window needs more space - reduce master
                    total_adjustment -= adjustment;
                }
            }
            // For other window counts (4, 6, 8, 9+), ratio adjustment is more complex
            // and would require knowing the specific grid structure. For now, we'll
            // make best-effort adjustments to the primary ratio.
        }

        // Apply accumulated adjustment
        if total_adjustment.abs() > 0.01 && !ratios.is_empty() {
            ratios[0] = (ratios[0] + total_adjustment).clamp(0.1, 0.9);
        }

        // Recompute layout using calculate_layout_full
        let new_result = calculate_layout_full(
            LayoutType::Grid,
            window_ids,
            screen_frame,
            0.5, // master_ratio not used for grid
            gaps,
            &ratios,
            MasterPosition::Auto,
        );

        // Check if violations are resolved
        let new_violations = find_minimum_size_violations(&new_result, layoutable_windows);
        if new_violations.is_empty() {
            return Some(new_result);
        }
    }

    // Return best effort
    let final_result = calculate_layout_full(
        LayoutType::Grid,
        window_ids,
        screen_frame,
        0.5,
        gaps,
        &ratios,
        MasterPosition::Auto,
    );
    Some(final_result)
}

// ============================================================================
// Violation Detection
// ============================================================================

/// Finds minimum size violations in a layout result.
///
/// Returns a vector of `(window_index, violation_axis)` where:
/// - `violation_axis`: `0` = width, `1` = height, `2` = both
pub fn find_minimum_size_violations(
    result: &LayoutResult,
    layoutable_windows: &[Window],
) -> Vec<(usize, u8)> {
    let mut violations = Vec::new();

    for (idx, (window_id, frame)) in result.iter().enumerate() {
        if let Some(window) = layoutable_windows.iter().find(|w| w.id == *window_id) {
            // Use effective_minimum_size() to include both reported and inferred minimums
            if let Some((min_w, min_h)) = window.effective_minimum_size() {
                let width_violation = frame.width < min_w - 1.0;
                let height_violation = frame.height < min_h - 1.0;

                if width_violation || height_violation {
                    let axis = match (width_violation, height_violation) {
                        (true, false) => 0,
                        (false, true) => 1,
                        (true, true) => 2,
                        _ => continue,
                    };
                    violations.push((idx, axis));
                }
            }
        }
    }

    violations
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_adjusted_ratios_no_minimums() {
        // Equal distribution with no minimums
        let cumulative = vec![0.5]; // Two windows at 50-50
        let min_ratios = vec![0.0, 0.0];
        let result = compute_adjusted_ratios(&cumulative, &min_ratios, 2);

        assert_eq!(result.len(), 2);
        assert!((result[0] - 0.5).abs() < 0.01);
        assert!((result[1] - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_compute_adjusted_ratios_one_minimum() {
        // Two windows at 50-50, but first needs 70%
        let cumulative = vec![0.5];
        let min_ratios = vec![0.7, 0.0];
        let result = compute_adjusted_ratios(&cumulative, &min_ratios, 2);

        assert_eq!(result.len(), 2);
        assert!(result[0] >= 0.7, "First window should get at least 70%");
        // Sum should be 1.0
        let sum: f64 = result.iter().sum();
        assert!((sum - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_adjusted_ratios_both_minimums() {
        // Two windows at 50-50, first needs 30%, second needs 60%
        let cumulative = vec![0.5];
        let min_ratios = vec![0.3, 0.6];
        let result = compute_adjusted_ratios(&cumulative, &min_ratios, 2);

        assert_eq!(result.len(), 2);
        assert!(result[0] >= 0.3, "First window should get at least 30%");
        assert!(result[1] >= 0.6, "Second window should get at least 60%");
        // Sum should be 1.0
        let sum: f64 = result.iter().sum();
        assert!((sum - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_adjusted_ratios_empty_cumulative() {
        // No existing ratios, should use equal distribution
        let cumulative: Vec<f64> = vec![];
        let min_ratios = vec![0.0, 0.0, 0.0];
        let result = compute_adjusted_ratios(&cumulative, &min_ratios, 3);

        assert_eq!(result.len(), 3);
        // Should be approximately equal
        for ratio in &result {
            assert!((*ratio - 1.0 / 3.0).abs() < 0.01);
        }
    }

    #[test]
    fn test_compute_adjusted_ratios_three_windows() {
        // Three windows at 33-33-33, first needs 50%
        let cumulative = vec![0.33, 0.66];
        let min_ratios = vec![0.5, 0.0, 0.0];
        let result = compute_adjusted_ratios(&cumulative, &min_ratios, 3);

        assert_eq!(result.len(), 3);
        assert!(result[0] >= 0.5, "First window should get at least 50%");
        // Sum should be 1.0
        let sum: f64 = result.iter().sum();
        assert!((sum - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_layout_with_ratios_horizontal() {
        let ratios = vec![0.5, 0.5];
        let window_ids = vec![1, 2];
        let usable_frame = Rect::new(0.0, 0.0, 1000.0, 500.0);
        let gaps = Gaps::uniform(10.0, 0.0);

        let result = compute_layout_with_ratios(&ratios, &window_ids, &usable_frame, &gaps, true);

        assert_eq!(result.len(), 2);

        let (id1, frame1) = result[0];
        let (id2, frame2) = result[1];

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);

        // With 1000px width, 10px gap, available = 990px
        // Each window gets 495px
        assert!((frame1.width - 495.0).abs() < 1.0);
        assert!((frame2.width - 495.0).abs() < 1.0);
        // Second window should start after first + gap
        assert!((frame2.x - frame1.width - 10.0).abs() < 1.0);
    }

    #[test]
    fn test_compute_layout_with_ratios_vertical() {
        let ratios = vec![0.6, 0.4];
        let window_ids = vec![1, 2];
        let usable_frame = Rect::new(0.0, 0.0, 500.0, 1000.0);
        let gaps = Gaps::uniform(10.0, 0.0);

        let result = compute_layout_with_ratios(&ratios, &window_ids, &usable_frame, &gaps, false);

        assert_eq!(result.len(), 2);

        let (_, frame1) = result[0];
        let (_, frame2) = result[1];

        // With 1000px height, 10px gap, available = 990px
        // First window gets 60% = 594px
        // Second window gets 40% = 396px
        assert!((frame1.height - 594.0).abs() < 1.0);
        assert!((frame2.height - 396.0).abs() < 1.0);
    }

    #[test]
    fn test_enforce_minimum_sizes_no_violations() {
        use smallvec::smallvec;

        // Initial layout with no violations
        let initial_result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 500.0, 1000.0)),
            (2, Rect::new(510.0, 0.0, 490.0, 1000.0)),
        ];

        // Windows with no minimum sizes
        let layoutable_windows = vec![
            Window {
                id: 1,
                minimum_size: None,
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: None,
                ..Default::default()
            },
        ];
        let window_ids = vec![1, 2];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::uniform(10.0, 0.0);

        let result = enforce_minimum_sizes_for_split(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            LayoutType::SplitHorizontal,
            &[0.5],
        );

        // No adjustment needed
        assert!(result.is_none());
    }

    #[test]
    fn test_enforce_minimum_sizes_with_violation() {
        use smallvec::smallvec;

        // Initial layout where window 2 is too small
        let initial_result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 800.0, 1000.0)),
            (2, Rect::new(810.0, 0.0, 190.0, 1000.0)), // Too small!
        ];

        // Window 2 has minimum width of 400px
        let layoutable_windows = vec![
            Window {
                id: 1,
                minimum_size: None,
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: Some((400.0, 100.0)),
                ..Default::default()
            },
        ];
        let window_ids = vec![1, 2];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::uniform(10.0, 0.0);

        let result = enforce_minimum_sizes_for_split(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            LayoutType::SplitHorizontal,
            &[0.8],
        );

        // Should have adjusted
        assert!(result.is_some());
        let adjusted = result.unwrap();
        assert_eq!(adjusted.len(), 2);

        // Window 2 should now have at least 400px
        let (_, frame2) = adjusted[1];
        assert!(
            frame2.width >= 399.0,
            "Window 2 should have at least ~400px width, got {}",
            frame2.width
        );
    }

    #[test]
    fn test_enforce_minimum_sizes_single_window() {
        use smallvec::smallvec;

        // Single window - no enforcement needed
        let initial_result: LayoutResult = smallvec![(1, Rect::new(0.0, 0.0, 1000.0, 1000.0)),];

        let layoutable_windows = vec![Window {
            id: 1,
            minimum_size: Some((2000.0, 2000.0)),
            ..Default::default()
        }];
        let window_ids = vec![1];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::default();

        let result = enforce_minimum_sizes_for_split(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            LayoutType::Split,
            &[],
        );

        // Single window always gets full space, no adjustment
        assert!(result.is_none());
    }

    // ========================================================================
    // Dwindle Minimum Size Tests
    // ========================================================================

    #[test]
    fn test_enforce_minimum_sizes_dwindle_no_violations() {
        use smallvec::smallvec;

        // Dwindle layout with no violations
        let initial_result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 500.0, 1000.0)),
            (2, Rect::new(500.0, 0.0, 500.0, 1000.0)),
        ];

        let layoutable_windows = vec![
            Window {
                id: 1,
                minimum_size: None,
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: None,
                ..Default::default()
            },
        ];
        let window_ids = vec![1, 2];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::default();

        let result = enforce_minimum_sizes_for_dwindle(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            &[0.5],
        );

        // No adjustment needed
        assert!(result.is_none());
    }

    #[test]
    fn test_enforce_minimum_sizes_dwindle_with_violation() {
        use smallvec::smallvec;

        // Dwindle layout where window 2 is too small
        let initial_result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 900.0, 1000.0)),
            (2, Rect::new(900.0, 0.0, 100.0, 1000.0)), // Too small!
        ];

        let layoutable_windows = vec![
            Window {
                id: 1,
                minimum_size: None,
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: Some((300.0, 100.0)), // Needs at least 300px width
                ..Default::default()
            },
        ];
        let window_ids = vec![1, 2];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::default();

        let result = enforce_minimum_sizes_for_dwindle(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            &[0.9], // 90% to first window, 10% to second
        );

        // Should have adjusted
        assert!(result.is_some());
        let adjusted = result.unwrap();
        assert_eq!(adjusted.len(), 2);

        // Window 2 should now have at least 300px (or close to it after adjustment)
        let (_, frame2) = adjusted[1];
        assert!(
            frame2.width >= 290.0,
            "Window 2 should have at least ~300px width after adjustment, got {}",
            frame2.width
        );
    }

    #[test]
    fn test_is_dwindle_split_horizontal() {
        // Landscape mode: odd indices are horizontal
        assert!(is_dwindle_split_horizontal(1, true)); // First split horizontal
        assert!(!is_dwindle_split_horizontal(2, true)); // Second split vertical
        assert!(is_dwindle_split_horizontal(3, true)); // Third split horizontal

        // Portrait mode: even indices are horizontal
        assert!(!is_dwindle_split_horizontal(1, false)); // First split vertical
        assert!(is_dwindle_split_horizontal(2, false)); // Second split horizontal
        assert!(!is_dwindle_split_horizontal(3, false)); // Third split vertical
    }

    // ========================================================================
    // Grid Minimum Size Tests
    // ========================================================================

    #[test]
    fn test_enforce_minimum_sizes_grid_no_violations() {
        use smallvec::smallvec;

        // Grid layout with no violations
        let initial_result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 500.0, 1000.0)),
            (2, Rect::new(500.0, 0.0, 500.0, 1000.0)),
        ];

        let layoutable_windows = vec![
            Window {
                id: 1,
                minimum_size: None,
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: None,
                ..Default::default()
            },
        ];
        let window_ids = vec![1, 2];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::default();

        let result = enforce_minimum_sizes_for_grid(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            &[0.5],
        );

        // No adjustment needed
        assert!(result.is_none());
    }

    #[test]
    fn test_enforce_minimum_sizes_grid_two_windows_violation() {
        use smallvec::smallvec;

        // Grid layout where window 2 is too small
        let initial_result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 900.0, 1000.0)),
            (2, Rect::new(900.0, 0.0, 100.0, 1000.0)), // Too small!
        ];

        let layoutable_windows = vec![
            Window {
                id: 1,
                minimum_size: None,
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: Some((300.0, 100.0)),
                ..Default::default()
            },
        ];
        let window_ids = vec![1, 2];
        let screen_frame = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let gaps = Gaps::default();

        let result = enforce_minimum_sizes_for_grid(
            &initial_result,
            &layoutable_windows,
            &window_ids,
            &screen_frame,
            &gaps,
            &[0.9],
        );

        // Should have adjusted
        assert!(result.is_some());
        let adjusted = result.unwrap();
        assert_eq!(adjusted.len(), 2);

        // Window 2 should now have more space
        let (_, frame2) = adjusted[1];
        assert!(
            frame2.width > 100.0,
            "Window 2 should have more than 100px after adjustment, got {}",
            frame2.width
        );
    }

    #[test]
    fn test_find_minimum_size_violations() {
        use smallvec::smallvec;

        let result: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 500.0, 400.0)),   // OK
            (2, Rect::new(500.0, 0.0, 100.0, 400.0)), // Width violation
            (3, Rect::new(0.0, 400.0, 500.0, 50.0)),  // Height violation
        ];

        let windows = vec![
            Window {
                id: 1,
                minimum_size: Some((300.0, 300.0)),
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: Some((200.0, 300.0)),
                ..Default::default()
            },
            Window {
                id: 3,
                minimum_size: Some((300.0, 100.0)),
                ..Default::default()
            },
        ];

        let violations = find_minimum_size_violations(&result, &windows);

        assert_eq!(violations.len(), 2);
        // Window 2 (index 1) has width violation (axis 0)
        assert!(violations.iter().any(|&(idx, axis)| idx == 1 && axis == 0));
        // Window 3 (index 2) has height violation (axis 1)
        assert!(violations.iter().any(|&(idx, axis)| idx == 2 && axis == 1));
    }
}
