//! Helper functions for layout ratio calculations.
//!
//! This module contains standalone helper functions used by the tiling manager
//! for calculating window split ratios and proportions.

use super::super::layout::Gaps;
use super::super::state::{Rect, TrackedWindow};

/// Checks if two frames are approximately equal within a threshold.
///
/// This is used to avoid unnecessary window repositioning when the difference
/// is imperceptible (e.g., due to floating point rounding).
#[inline]
pub fn frames_approximately_equal(a: &Rect, b: &Rect, threshold: f64) -> bool {
    (a.x - b.x).abs() < threshold
        && (a.y - b.y).abs() < threshold
        && (a.width - b.width).abs() < threshold
        && (a.height - b.height).abs() < threshold
}

/// Calculates split ratios from current window frames.
///
/// Given a list of windows and the screen frame, calculates the cumulative
/// split ratios that would produce the current layout.
///
/// The ratio is calculated based on **where each split point should be**,
/// taking into account that either window on each side of the split could
/// have been resized:
/// - Window i could have been resized (changing its end position)
/// - Window i+1 could have been resized (changing its start position)
///
/// We use the **average** of both edges to find the intended split point,
/// which works regardless of which window was resized.
///
/// # Arguments
///
/// * `windows` - Windows in order
/// * `screen_frame` - The screen's visible frame
/// * `gaps` - Gap configuration
/// * `is_vertical` - Whether the split is vertical (top-to-bottom) or horizontal (left-to-right)
///
/// # Returns
///
/// A vector of N-1 ratios for N windows, representing cumulative split positions.
#[allow(clippy::cast_precision_loss)] // Window counts won't exceed f64 precision
pub fn calculate_ratios_from_frames(
    windows: &[&TrackedWindow],
    screen_frame: &Rect,
    gaps: &Gaps,
    is_vertical: bool,
) -> Vec<f64> {
    if windows.len() < 2 {
        return Vec::new();
    }

    let count = windows.len();
    let mut ratios = Vec::with_capacity(count - 1);

    if is_vertical {
        // Vertical split: calculate ratios based on Y positions
        let total_gap = gaps.inner_v * (count - 1) as f64;
        let available_height = screen_frame.height - total_gap;

        if available_height <= 0.0 {
            return Vec::new();
        }

        // For each split point between window i and window i+1
        for i in 0..(count - 1) {
            // Window i's bottom edge (where it ends)
            let window_i_bottom = windows[i].frame.y + windows[i].frame.height;

            // Window i+1's top edge (where it starts)
            let window_next_top = windows[i + 1].frame.y;

            // The split point is in the middle of the gap between windows.
            // Average the two edges to find where the user intended the split.
            // This works whether window i was resized (bottom moved) or
            // window i+1 was resized (top moved).
            let split_point_screen = f64::midpoint(window_i_bottom, window_next_top);

            // Convert to ratio: account for screen offset and gaps before this point
            // The split point in "ratio space" is the space used by windows 0..=i
            let gaps_before_split = gaps.inner_v * (i as f64 + 0.5); // Half gap at the split
            let split_in_available = split_point_screen - screen_frame.y - gaps_before_split;

            let ratio = split_in_available / available_height;
            ratios.push(ratio.clamp(0.05, 0.95));
        }
    } else {
        // Horizontal split: calculate ratios based on X positions
        let total_gap = gaps.inner_h * (count - 1) as f64;
        let available_width = screen_frame.width - total_gap;

        if available_width <= 0.0 {
            return Vec::new();
        }

        // For each split point between window i and window i+1
        for i in 0..(count - 1) {
            // Window i's right edge (where it ends)
            let window_i_right = windows[i].frame.x + windows[i].frame.width;

            // Window i+1's left edge (where it starts)
            let window_next_left = windows[i + 1].frame.x;

            // The split point is in the middle of the gap between windows.
            let split_point_screen = f64::midpoint(window_i_right, window_next_left);

            // Convert to ratio
            let gaps_before_split = gaps.inner_h * (i as f64 + 0.5);
            let split_in_available = split_point_screen - screen_frame.x - gaps_before_split;

            let ratio = split_in_available / available_width;
            ratios.push(ratio.clamp(0.05, 0.95));
        }
    }

    ratios
}

/// Converts cumulative split ratios to individual window proportions.
///
/// # Arguments
///
/// * `ratios` - Cumulative ratios (n-1 values for n windows)
/// * `window_count` - Total number of windows
///
/// # Returns
///
/// Vector of individual proportions that sum to 1.0.
/// If ratios is empty or wrong length, returns equal proportions.
#[allow(clippy::cast_precision_loss)] // Window counts won't exceed f64 precision
pub fn cumulative_ratios_to_proportions(ratios: &[f64], window_count: usize) -> Vec<f64> {
    if window_count == 0 {
        return Vec::new();
    }

    if window_count == 1 {
        return vec![1.0];
    }

    // If no ratios or wrong length, return equal proportions
    if ratios.len() != window_count - 1 {
        let equal = 1.0 / window_count as f64;
        return vec![equal; window_count];
    }

    // Convert cumulative ratios to individual proportions
    let mut proportions = Vec::with_capacity(window_count);
    let mut prev_ratio = 0.0;

    for &ratio in ratios {
        proportions.push(ratio - prev_ratio);
        prev_ratio = ratio;
    }

    // Last window takes the remaining space
    proportions.push(1.0 - prev_ratio);

    proportions
}

/// Converts individual window proportions to cumulative split ratios.
///
/// # Arguments
///
/// * `proportions` - Individual window proportions
///
/// # Returns
///
/// Vector of cumulative ratios (n-1 values for n windows).
pub fn proportions_to_cumulative_ratios(proportions: &[f64]) -> Vec<f64> {
    if proportions.len() < 2 {
        return Vec::new();
    }

    let mut ratios = Vec::with_capacity(proportions.len() - 1);
    let mut cumulative = 0.0;

    // Sum all but the last proportion
    for proportion in proportions.iter().take(proportions.len() - 1) {
        cumulative += proportion;
        ratios.push(cumulative);
    }

    ratios
}

/// Calculates new proportions by only adjusting the resized window and its adjacent window.
///
/// This preserves all other windows at their current sizes.
///
/// # Arguments
///
/// * `current_proportions` - Current individual proportions
/// * `resized_idx` - Index of the window that was resized
/// * `adjacent_idx` - Index of the adjacent window to adjust
/// * `delta` - The proportion change (positive = resized grew, negative = shrank)
///
/// # Returns
///
/// New proportions with only the resized and adjacent windows changed.
pub fn calculate_proportions_adjusting_adjacent(
    current_proportions: &[f64],
    resized_idx: usize,
    adjacent_idx: usize,
    delta: f64,
) -> Vec<f64> {
    let mut new_proportions = current_proportions.to_vec();

    // Apply the delta to the resized window
    new_proportions[resized_idx] += delta;

    // Apply the opposite delta to the adjacent window
    new_proportions[adjacent_idx] -= delta;

    // Clamp all proportions to valid range
    let min_proportion = 0.05;
    let max_proportion = 0.95;

    for proportion in &mut new_proportions {
        *proportion = proportion.clamp(min_proportion, max_proportion);
    }

    // Normalize to ensure they sum to 1.0
    let sum: f64 = new_proportions.iter().sum();
    if (sum - 1.0).abs() > 0.001 {
        for proportion in &mut new_proportions {
            *proportion /= sum;
        }
    }

    new_proportions
}

// ============================================================================
// Debug Lock Contention Monitoring
// ============================================================================

/// Threshold in milliseconds for lock contention warnings.
/// If a lock is held longer than this, a warning is logged.
#[cfg(debug_assertions)]
const LOCK_CONTENTION_THRESHOLD_MS: u64 = 5;

/// Tracks how long a lock-protected operation takes and logs a warning
/// if it exceeds the threshold.
///
/// This is a debug-only helper that has zero overhead in release builds.
/// It helps identify lock contention issues during development.
///
/// # Arguments
///
/// * `name` - A short name identifying the lock/operation (e.g., "manager.write")
/// * `f` - The function to execute while timing
///
/// # Returns
///
/// The return value of `f`.
///
/// # Example
///
/// ```ignore
/// let result = track_lock_time("manager.read", || {
///     let mgr = manager.read();
///     mgr.get_windows().len()
/// });
/// ```
#[cfg(debug_assertions)]
#[inline]
pub fn track_lock_time<T, F: FnOnce() -> T>(name: &str, f: F) -> T {
    use std::time::Instant;

    let start = Instant::now();
    let result = f();
    let elapsed = start.elapsed();

    let elapsed_ms = elapsed.as_millis();
    // Cast is safe: a lock held for >u64::MAX ms is unrealistic
    #[allow(clippy::cast_possible_truncation)]
    if elapsed_ms > u128::from(LOCK_CONTENTION_THRESHOLD_MS) {
        eprintln!(
            "stache: tiling: LOCK CONTENTION: {name} held for {elapsed_ms}ms (threshold: {LOCK_CONTENTION_THRESHOLD_MS}ms)"
        );
    }

    result
}

/// No-op version for release builds. The compiler will inline and eliminate this.
#[cfg(not(debug_assertions))]
#[inline(always)]
pub fn track_lock_time<T, F: FnOnce() -> T>(_name: &str, f: F) -> T { f() }

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frames_approximately_equal_identical() {
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert!(frames_approximately_equal(&a, &b, 2.0));
    }

    #[test]
    fn test_frames_approximately_equal_within_threshold() {
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(1.0, 1.0, 101.0, 101.0);
        assert!(frames_approximately_equal(&a, &b, 2.0));
    }

    #[test]
    fn test_frames_approximately_equal_outside_threshold() {
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(3.0, 0.0, 100.0, 100.0);
        assert!(!frames_approximately_equal(&a, &b, 2.0));
    }

    #[test]
    fn test_frames_approximately_equal_all_dimensions() {
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(0.0, 0.0, 100.0, 103.0);
        assert!(!frames_approximately_equal(&a, &b, 2.0));
    }

    #[test]
    fn test_cumulative_ratios_to_proportions_empty() {
        let result = cumulative_ratios_to_proportions(&[], 0);
        assert!(result.is_empty());
    }

    #[test]
    fn test_cumulative_ratios_to_proportions_single_window() {
        let result = cumulative_ratios_to_proportions(&[], 1);
        assert_eq!(result, vec![1.0]);
    }

    #[test]
    fn test_cumulative_ratios_to_proportions_two_windows() {
        let ratios = vec![0.5];
        let result = cumulative_ratios_to_proportions(&ratios, 2);
        assert_eq!(result.len(), 2);
        assert!((result[0] - 0.5).abs() < 0.001);
        assert!((result[1] - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_cumulative_ratios_to_proportions_three_windows() {
        let ratios = vec![0.33, 0.66];
        let result = cumulative_ratios_to_proportions(&ratios, 3);
        assert_eq!(result.len(), 3);
        assert!((result[0] - 0.33).abs() < 0.001);
        assert!((result[1] - 0.33).abs() < 0.001);
        assert!((result[2] - 0.34).abs() < 0.001);
    }

    #[test]
    fn test_proportions_to_cumulative_ratios_empty() {
        let result = proportions_to_cumulative_ratios(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_proportions_to_cumulative_ratios_single() {
        let result = proportions_to_cumulative_ratios(&[1.0]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_proportions_to_cumulative_ratios_two_windows() {
        let result = proportions_to_cumulative_ratios(&[0.5, 0.5]);
        assert_eq!(result.len(), 1);
        assert!((result[0] - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_proportions_to_cumulative_ratios_three_windows() {
        let result = proportions_to_cumulative_ratios(&[0.33, 0.33, 0.34]);
        assert_eq!(result.len(), 2);
        assert!((result[0] - 0.33).abs() < 0.001);
        assert!((result[1] - 0.66).abs() < 0.001);
    }

    #[test]
    fn test_calculate_proportions_adjusting_adjacent() {
        let current = vec![0.5, 0.5];
        let result = calculate_proportions_adjusting_adjacent(&current, 0, 1, 0.1);
        // Window 0 should grow by 0.1, window 1 should shrink by 0.1
        assert!((result[0] - 0.6).abs() < 0.01);
        assert!((result[1] - 0.4).abs() < 0.01);
    }

    #[test]
    fn test_calculate_proportions_adjusting_adjacent_clamped() {
        let current = vec![0.9, 0.1];
        // Try to grow window 0 beyond max - should be clamped
        let result = calculate_proportions_adjusting_adjacent(&current, 0, 1, 0.2);
        // Should be clamped to valid range
        assert!(result[0] <= 0.95);
        assert!(result[1] >= 0.05);
    }

    #[test]
    fn test_track_lock_time_returns_value() {
        let result = super::track_lock_time("test", || 42);
        assert_eq!(result, 42);
    }

    #[test]
    fn test_track_lock_time_executes_closure() {
        let mut executed = false;
        super::track_lock_time("test", || {
            executed = true;
        });
        assert!(executed);
    }
}
