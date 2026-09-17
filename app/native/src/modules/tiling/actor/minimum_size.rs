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

use crate::modules::tiling::layout::{
    Gaps, LayoutResult, MasterPosition, calculate_layout_full, has_valid_cumulative_ratios,
    is_dwindle_split_horizontal, is_split_horizontal,
};
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
    let is_horizontal = is_split_horizontal(layout, screen_frame.width >= screen_frame.height);

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

    tracing::debug!("Minimum size violations detected in split layout, adjusting ratios");

    // Calculate minimum ratios for each window
    let min_ratios: Vec<f64> =
        min_sizes.iter().map(|&min| (min / available_space).min(1.0)).collect();

    // Check if total minimum requirements exceed available space
    let total_min_ratio: f64 = min_ratios.iter().sum();
    if total_min_ratio > 1.0 {
        tracing::warn!(
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
    if window_count == 0 {
        return Vec::new();
    }

    // Convert cumulative ratios to per-window ratios
    #[allow(clippy::cast_precision_loss)]
    let mut window_ratios: Vec<f64> =
        if has_valid_cumulative_ratios(cumulative_ratios, window_count) {
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
        } else {
            // Default: equal distribution
            vec![1.0 / window_count as f64; window_count]
        };

    // Ensure each window meets its minimum
    for i in 0..window_count {
        let minimum = min_ratios
            .get(i)
            .copied()
            .filter(|ratio| ratio.is_finite())
            .unwrap_or_default()
            .clamp(0.0, 1.0);

        if window_ratios[i] < minimum {
            let deficit = minimum - window_ratios[i];
            window_ratios[i] = minimum;

            // Take space from other windows that have room
            let mut remaining_deficit = deficit;
            for (j, ratio) in window_ratios.iter_mut().enumerate() {
                if j != i && remaining_deficit > 0.0 {
                    let other_minimum = min_ratios
                        .get(j)
                        .copied()
                        .filter(|ratio| ratio.is_finite())
                        .unwrap_or_default()
                        .clamp(0.0, 1.0);
                    let available = *ratio - other_minimum;
                    if available > 0.0 {
                        let take = available.min(remaining_deficit);
                        *ratio -= take;
                        remaining_deficit -= take;
                    }
                }
            }
        }
    }

    // Normalize to ensure sum is exactly 1.0
    let sum: f64 = window_ratios.iter().sum();
    if sum.is_finite() && sum > f64::EPSILON && (sum - 1.0).abs() > 0.001 {
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

    // Start from the actual layout and carry the newest frames and violations
    // through every solver step. Reusing the initial frames makes later
    // iterations repeat stale adjustments.
    let mut result = initial_result.clone();
    let mut violations = find_minimum_size_violations(&result, layoutable_windows);
    if violations.is_empty() {
        return None;
    }

    tracing::debug!(
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

    let usable_frame = gaps.apply_outer(screen_frame);
    let is_landscape = usable_frame.width >= usable_frame.height;

    for iteration in 0..MAX_ITERATIONS {
        // Collect adjustment magnitudes based on violation severity
        let mut adjustments: Vec<(usize, f64)> = Vec::new();

        for &(window_idx, violation_axis) in &violations {
            // Get the window's frame and minimum size
            let Some((_, frame)) = result.get(window_idx) else {
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
                        usable_frame.width
                    } else {
                        usable_frame.height
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
                    let is_h_split = is_dwindle_split_horizontal(ratio_idx, is_landscape);

                    if (is_h_split && width_violated) || (!is_h_split && height_violated) {
                        let deficit = if is_h_split {
                            width_deficit
                        } else {
                            height_deficit
                        };
                        let total_dim = if is_h_split {
                            usable_frame.width
                        } else {
                            usable_frame.height
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

        if adjustments.is_empty() {
            tracing::warn!(
                "Minimum-size dwindle solver stalled at iteration {iteration}; \
                 remaining constraints are infeasible or not controlled by a split ratio"
            );
            return Some(result);
        }

        let previous_ratios = ratios.clone();

        // Apply all adjustments.
        for (idx, adj) in adjustments {
            ratios[idx] = (ratios[idx] + adj).clamp(0.1, 0.9);
        }

        if ratios == previous_ratios {
            tracing::warn!(
                "Minimum-size dwindle solver reached ratio bounds with unresolved constraints"
            );
            return Some(result);
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

        let old_deficit = minimum_size_deficit(&result, layoutable_windows);
        let new_deficit = minimum_size_deficit(&new_result, layoutable_windows);
        if new_deficit >= old_deficit - 1.0 {
            tracing::warn!(
                "Minimum-size dwindle solver made no progress at iteration {iteration}; \
                 keeping the least-violating layout"
            );
            return Some(result);
        }

        result = new_result;
        violations = new_violations;
    }

    tracing::warn!("Minimum-size dwindle solver reached its iteration limit");
    Some(result)
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

    let usable_frame = gaps.apply_outer(screen_frame);
    let is_landscape = usable_frame.width >= usable_frame.height;
    if let Some((rows, cols)) = regular_grid_dimensions(window_ids.len(), is_landscape) {
        return enforce_regular_grid_minimums(
            initial_result,
            layoutable_windows,
            window_ids,
            screen_frame,
            gaps,
            rows,
            cols,
        );
    }

    if !matches!(window_ids.len(), 3 | 5 | 7) {
        tracing::warn!(
            "Minimum-size grid solver has no ratio topology for {} windows; \
             leaving the original layout unchanged",
            window_ids.len()
        );
        return None;
    }

    tracing::debug!(
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

    let mut result = initial_result.clone();
    let mut violations = violations;

    for iteration in 0..MAX_ITERATIONS {
        // Collect proportional adjustments based on violation severity
        let mut total_adjustment: f64 = 0.0;

        for &(window_idx, violation_axis) in &violations {
            // Get the window's frame and minimum size
            let Some((_, frame)) = result.get(window_idx) else {
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
                usable_frame.width
            } else {
                usable_frame.height
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

        if total_adjustment.abs() <= 0.01 || ratios.is_empty() {
            tracing::warn!(
                "Minimum-size grid solver stalled at iteration {iteration}; \
                 remaining constraints are infeasible or not controlled by the master ratio"
            );
            return Some(result);
        }

        let previous_ratio = ratios[0];
        ratios[0] = (ratios[0] + total_adjustment).clamp(0.1, 0.9);
        if (ratios[0] - previous_ratio).abs() < f64::EPSILON {
            tracing::warn!(
                "Minimum-size grid solver reached the master-ratio bound with unresolved constraints"
            );
            return Some(result);
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

        let old_deficit = minimum_size_deficit(&result, layoutable_windows);
        let new_deficit = minimum_size_deficit(&new_result, layoutable_windows);
        if new_deficit >= old_deficit - 1.0 {
            tracing::warn!(
                "Minimum-size grid solver made no progress at iteration {iteration}; \
                 keeping the least-violating layout"
            );
            return Some(result);
        }

        result = new_result;
        violations = new_violations;
    }

    tracing::warn!("Minimum-size grid solver reached its iteration limit");
    Some(result)
}

/// Returns the rows and columns for grid layouts with independent row/column
/// ratios. Master-stack arrangements are intentionally handled separately.
const fn regular_grid_dimensions(count: usize, is_landscape: bool) -> Option<(usize, usize)> {
    match count {
        2 if is_landscape => Some((1, 2)),
        2 => Some((2, 1)),
        4 => Some((2, 2)),
        6 => Some((2, 3)),
        8 => Some((2, 4)),
        9 => Some((3, 3)),
        12 => Some((3, 4)),
        _ => None,
    }
}

/// Solves regular grids directly from their row/column topology.
///
/// A column must satisfy the widest minimum of its cells and a row the tallest
/// minimum. This handles every regular grid count instead of repeatedly
/// nudging an unrelated primary ratio.
#[allow(clippy::too_many_arguments)]
fn enforce_regular_grid_minimums(
    initial_result: &LayoutResult,
    layoutable_windows: &[Window],
    window_ids: &[u32],
    screen_frame: &Rect,
    gaps: &Gaps,
    rows: usize,
    cols: usize,
) -> Option<LayoutResult> {
    let usable_frame = gaps.apply_outer(screen_frame);
    #[allow(clippy::cast_precision_loss)]
    let available_width = usable_frame.width - gaps.inner_h * (cols - 1) as f64;
    #[allow(clippy::cast_precision_loss)]
    let available_height = usable_frame.height - gaps.inner_v * (rows - 1) as f64;
    if available_width <= 0.0 || available_height <= 0.0 {
        tracing::warn!("Minimum-size grid solver has no usable space");
        return None;
    }

    let mut minimum_widths = vec![0.0_f64; cols];
    let mut minimum_heights = vec![0.0_f64; rows];
    let mut current_widths = vec![0.0_f64; cols];
    let mut current_heights = vec![0.0_f64; rows];

    for (index, window_id) in window_ids.iter().copied().enumerate() {
        let row = index / cols;
        let col = index % cols;
        if row >= rows {
            break;
        }

        if let Some((minimum_width, minimum_height)) = layoutable_windows
            .iter()
            .find(|window| window.id == window_id)
            .and_then(Window::effective_minimum_size)
        {
            minimum_widths[col] = minimum_widths[col].max(minimum_width);
            minimum_heights[row] = minimum_heights[row].max(minimum_height);
        }

        if let Some((_, frame)) = initial_result.get(index) {
            current_widths[col] = current_widths[col].max(frame.width);
            current_heights[row] = current_heights[row].max(frame.height);
        }
    }

    let widths = distribute_remaining_space(&minimum_widths, &current_widths, available_width)?;
    let heights = distribute_remaining_space(&minimum_heights, &current_heights, available_height)?;
    let mut ratios = cumulative_ratios(&widths, available_width);
    ratios.extend(cumulative_ratios(&heights, available_height));

    let adjusted = calculate_layout_full(
        LayoutType::Grid,
        window_ids,
        screen_frame,
        0.5,
        gaps,
        &ratios,
        MasterPosition::Auto,
    );
    if find_minimum_size_violations(&adjusted, layoutable_windows).is_empty() {
        Some(adjusted)
    } else {
        tracing::warn!("Minimum-size grid solver could not satisfy a regular-grid constraint");
        None
    }
}

/// Allocates an axis's usable pixels after reserving every group's minimum.
fn distribute_remaining_space(
    minimums: &[f64],
    current_sizes: &[f64],
    available: f64,
) -> Option<Vec<f64>> {
    let required: f64 = minimums.iter().sum();
    if required > available + 1.0 {
        tracing::warn!(
            "Minimum-size constraint is infeasible: requires {required}px, only {available}px available"
        );
        return None;
    }

    let remaining = (available - required).max(0.0);
    let weights: Vec<f64> = current_sizes
        .iter()
        .map(|size| {
            if size.is_finite() {
                (*size).max(0.0)
            } else {
                0.0
            }
        })
        .collect();
    let weight_sum: f64 = weights.iter().sum();
    #[allow(clippy::cast_precision_loss)]
    let equal_weight = 1.0 / minimums.len() as f64;

    Some(
        minimums
            .iter()
            .enumerate()
            .map(|(index, minimum)| {
                let weight = if weight_sum > f64::EPSILON {
                    weights[index] / weight_sum
                } else {
                    equal_weight
                };
                minimum + remaining * weight
            })
            .collect(),
    )
}

/// Converts axis lengths into cumulative layout boundaries.
fn cumulative_ratios(sizes: &[f64], available: f64) -> Vec<f64> {
    let mut used = 0.0;
    sizes
        .iter()
        .take(sizes.len().saturating_sub(1))
        .map(|size| {
            used += size;
            (used / available).clamp(0.05, 0.95)
        })
        .collect()
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

/// Returns the total missing width and height across constrained windows.
///
/// The solver uses this to keep the best layout and stop rather than repeat an
/// adjustment which cannot improve the remaining constraints.
fn minimum_size_deficit(result: &LayoutResult, layoutable_windows: &[Window]) -> f64 {
    result
        .iter()
        .filter_map(|(window_id, frame)| {
            layoutable_windows
                .iter()
                .find(|window| window.id == *window_id)
                .and_then(Window::effective_minimum_size)
                .map(|(min_width, min_height)| {
                    (min_width - frame.width).max(0.0) + (min_height - frame.height).max(0.0)
                })
        })
        .sum()
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
    fn test_compute_adjusted_ratios_short_cumulative_ratios_fall_back_to_equal() {
        let result = compute_adjusted_ratios(&[0.5], &[0.7, 0.0, 0.0], 3);

        assert_eq!(result.len(), 3);
        assert!(result.iter().all(|ratio| ratio.is_finite()));
        assert!(result[0] >= 0.7);
        assert!((result.iter().sum::<f64>() - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_adjusted_ratios_non_finite_cumulative_ratios_fall_back_to_equal() {
        let result = compute_adjusted_ratios(&[f64::NAN, 0.8], &[0.0, 0.0, 0.0], 3);

        assert_eq!(result.len(), 3);
        assert!(result.iter().all(|ratio| ratio.is_finite()));
        assert!(result.iter().all(|ratio| (*ratio - 1.0 / 3.0).abs() < 0.01));
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
    fn explicit_horizontal_split_uses_width_on_a_portrait_display() {
        use smallvec::smallvec;

        let initial: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 100.0, 1000.0)),
            (2, Rect::new(100.0, 0.0, 900.0, 1000.0)),
        ];
        let windows = vec![
            Window {
                id: 1,
                minimum_size: Some((300.0, 0.0)),
                ..Default::default()
            },
            Window { id: 2, ..Default::default() },
        ];
        let ids = vec![1, 2];
        let portrait = Rect::new(0.0, 0.0, 1000.0, 1600.0);

        let adjusted = enforce_minimum_sizes_for_split(
            &initial,
            &windows,
            &ids,
            &portrait,
            &Gaps::default(),
            LayoutType::SplitHorizontal,
            &[0.1],
        )
        .expect("the width deficit must be adjusted");

        assert!(adjusted[0].1.width >= 299.0);
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
    fn dwindle_first_window_width_deficit_uses_the_landscape_first_split() {
        use smallvec::smallvec;

        let initial: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 100.0, 1000.0)),
            (2, Rect::new(100.0, 0.0, 900.0, 1000.0)),
        ];
        let windows = vec![
            Window {
                id: 1,
                minimum_size: Some((300.0, 0.0)),
                ..Default::default()
            },
            Window { id: 2, ..Default::default() },
        ];

        let adjusted = enforce_minimum_sizes_for_dwindle(
            &initial,
            &windows,
            &[1, 2],
            &Rect::new(0.0, 0.0, 1000.0, 1000.0),
            &Gaps::default(),
            &[0.1],
        )
        .expect("the first split controls the first window width");

        assert!(adjusted[0].1.width >= 299.0);
    }

    #[test]
    fn test_is_dwindle_split_horizontal() {
        // The helper takes zero-based ratio indices. Landscape starts horizontal.
        assert!(is_dwindle_split_horizontal(0, true));
        assert!(!is_dwindle_split_horizontal(1, true));
        assert!(is_dwindle_split_horizontal(2, true));

        // Portrait starts vertical.
        assert!(!is_dwindle_split_horizontal(0, false));
        assert!(is_dwindle_split_horizontal(1, false));
        assert!(!is_dwindle_split_horizontal(2, false));
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
    fn regular_grid_adjusts_the_constrained_column() {
        use smallvec::smallvec;

        let initial: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 200.0, 500.0)),
            (2, Rect::new(200.0, 0.0, 800.0, 500.0)),
            (3, Rect::new(0.0, 500.0, 200.0, 500.0)),
            (4, Rect::new(200.0, 500.0, 800.0, 500.0)),
        ];
        let windows = vec![
            Window {
                id: 1,
                minimum_size: Some((350.0, 0.0)),
                ..Default::default()
            },
            Window { id: 2, ..Default::default() },
            Window {
                id: 3,
                minimum_size: Some((300.0, 0.0)),
                ..Default::default()
            },
            Window { id: 4, ..Default::default() },
        ];
        let ids = vec![1, 2, 3, 4];
        let adjusted = enforce_minimum_sizes_for_grid(
            &initial,
            &windows,
            &ids,
            &Rect::new(0.0, 0.0, 1000.0, 1000.0),
            &Gaps::default(),
            &[0.2, 0.5],
        )
        .expect("the regular grid has enough width for the constrained column");

        assert!(adjusted[0].1.width >= 349.0);
        assert!(adjusted[2].1.width >= 349.0);
        assert!(find_minimum_size_violations(&adjusted, &windows).is_empty());
    }

    #[test]
    fn regular_grid_leaves_infeasible_constraints_unchanged() {
        use smallvec::smallvec;

        let initial: LayoutResult = smallvec![
            (1, Rect::new(0.0, 0.0, 500.0, 500.0)),
            (2, Rect::new(500.0, 0.0, 500.0, 500.0)),
            (3, Rect::new(0.0, 500.0, 500.0, 500.0)),
            (4, Rect::new(500.0, 500.0, 500.0, 500.0)),
        ];
        let windows = vec![
            Window {
                id: 1,
                minimum_size: Some((700.0, 0.0)),
                ..Default::default()
            },
            Window {
                id: 2,
                minimum_size: Some((700.0, 0.0)),
                ..Default::default()
            },
            Window { id: 3, ..Default::default() },
            Window { id: 4, ..Default::default() },
        ];
        let ids = vec![1, 2, 3, 4];

        assert!(
            enforce_minimum_sizes_for_grid(
                &initial,
                &windows,
                &ids,
                &Rect::new(0.0, 0.0, 1000.0, 1000.0),
                &Gaps::default(),
                &[],
            )
            .is_none()
        );
    }

    #[test]
    fn master_stack_grid_uses_latest_frames_when_adjusting_the_master_ratio() {
        let ids = vec![1, 2, 3];
        let screen = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let initial = calculate_layout_full(
            LayoutType::Grid,
            &ids,
            &screen,
            0.5,
            &Gaps::default(),
            &[0.8],
            MasterPosition::Auto,
        );
        let windows = vec![
            Window { id: 1, ..Default::default() },
            Window {
                id: 2,
                minimum_size: Some((300.0, 0.0)),
                ..Default::default()
            },
            Window {
                id: 3,
                minimum_size: Some((300.0, 0.0)),
                ..Default::default()
            },
        ];

        let adjusted =
            enforce_minimum_sizes_for_grid(&initial, &windows, &ids, &screen, &Gaps::default(), &[
                0.8,
            ])
            .expect("the stack has enough available width after reducing the master");

        assert!(adjusted[1].1.width >= 299.0);
        assert!(adjusted[2].1.width >= 299.0);
        assert!(find_minimum_size_violations(&adjusted, &windows).is_empty());
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
