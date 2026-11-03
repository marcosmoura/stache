//! Floating layout and preset handling.
//!
//! This module provides functionality for floating windows, including
//! preset-based positioning where windows can be quickly placed at
//! predefined sizes and positions.

use super::Gaps;
use crate::config::{DimensionValue, FloatingPreset, get_config};
use crate::modules::tiling::state::Rect;

// ============================================================================
// Preset Functions
// ============================================================================

/// Finds a preset by name from the configuration.
///
/// # Arguments
///
/// * `name` - The name of the preset to find (case-insensitive).
///
/// # Returns
///
/// The preset if found, or `None` if no preset with that name exists.
#[must_use]
pub fn find_preset(name: &str) -> Option<FloatingPreset> {
    let config = get_config();
    config
        .tiling
        .floating
        .presets
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .cloned()
}

/// Returns a list of all available preset names.
#[must_use]
pub fn list_preset_names() -> Vec<String> {
    let config = get_config();
    config.tiling.floating.presets.iter().map(|p| p.name.clone()).collect()
}

// ============================================================================
// Preset Frame Calculation
// ============================================================================

/// Calculates the window frame for a floating preset.
///
/// This handles percentage-based dimensions and positions, centering,
/// and proper gap handling for adjacent windows (e.g., half-left and
/// half-right windows have proper spacing between them).
///
/// # Arguments
///
/// * `preset` - The preset configuration to apply.
/// * `screen_frame` - The available screen area (already adjusted for menu bar, dock, etc.).
/// * `gaps` - Gap configuration for outer and inner margins.
///
/// # Returns
///
/// The calculated window frame as a `Rect`.
#[must_use]
pub fn calculate_preset_frame(preset: &FloatingPreset, screen_frame: &Rect, gaps: &Gaps) -> Rect {
    // Apply outer gaps to get the usable area
    let usable = gaps.apply_outer(screen_frame);

    // Check if dimensions are 50% (for inner gap handling)
    let width_is_half = is_half_percentage(&preset.width);
    let height_is_half = is_half_percentage(&preset.height);

    // Resolve width, accounting for inner gap if 50%
    let width = if width_is_half {
        // Two 50% windows side by side need an inner gap between them
        (usable.width - gaps.inner_h) / 2.0
    } else {
        preset.width.resolve(usable.width)
    };

    // Resolve height, accounting for inner gap if 50%
    let height = if height_is_half {
        // Two 50% windows stacked need an inner gap between them
        (usable.height - gaps.inner_v) / 2.0
    } else {
        preset.height.resolve(usable.height)
    };

    // Clamp dimensions to usable area
    let width = width.min(usable.width).max(1.0);
    let height = height.min(usable.height).max(1.0);

    // Calculate position
    let (x, y) = if preset.center {
        // Center the window in the usable area
        let center_x = usable.x + (usable.width - width) / 2.0;
        let center_y = usable.y + (usable.height - height) / 2.0;
        (center_x, center_y)
    } else {
        // Calculate x position, accounting for inner gap if width is 50%
        let x = preset.x.as_ref().map_or(usable.x, |dim| {
            if width_is_half && is_half_percentage(dim) {
                // Right half: position after left half + inner gap
                usable.x + width + gaps.inner_h
            } else {
                usable.x + dim.resolve(usable.width)
            }
        });

        // Calculate y position, accounting for inner gap if height is 50%
        let y = preset.y.as_ref().map_or(usable.y, |dim| {
            if height_is_half && is_half_percentage(dim) {
                // Bottom half: position after top half + inner gap
                usable.y + height + gaps.inner_v
            } else {
                usable.y + dim.resolve(usable.height)
            }
        });

        (x, y)
    };

    // Clamp position to keep window within usable area
    let x: f64 = x.max(usable.x).min(usable.x + usable.width - width);
    let y: f64 = y.max(usable.y).min(usable.y + usable.height - height);

    Rect::new(x, y, width, height)
}

/// Checks if a dimension value is exactly 50%.
fn is_half_percentage(dim: &DimensionValue) -> bool {
    match dim {
        DimensionValue::Percentage(s) => {
            let trimmed = s.trim().trim_end_matches('%');
            trimmed.parse::<f64>().is_ok_and(|pct| (pct - 50.0).abs() < 0.001)
        }
        DimensionValue::Pixels(_) => false,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn screen_frame() -> Rect { Rect::new(0.0, 0.0, 1920.0, 1080.0) }

    fn test_preset(width: &str, height: &str, center: bool) -> FloatingPreset {
        FloatingPreset {
            name: "test".to_string(),
            width: DimensionValue::Percentage(width.to_string()),
            height: DimensionValue::Percentage(height.to_string()),
            x: None,
            y: None,
            center,
        }
    }

    #[test]
    fn test_preset_centered() {
        let preset = test_preset("50%", "50%", true);
        let frame = calculate_preset_frame(&preset, &screen_frame(), &Gaps::zero());

        // Should be centered
        assert!((frame.x - 480.0).abs() < 1.0); // (1920 - 960) / 2
        assert!((frame.y - 270.0).abs() < 1.0); // (1080 - 540) / 2
        assert!((frame.width - 960.0).abs() < 1.0);
        assert!((frame.height - 540.0).abs() < 1.0);
    }

    #[test]
    fn test_preset_with_gaps() {
        let preset = test_preset("100%", "100%", false);
        let gaps = Gaps::uniform(10.0, 20.0);
        let frame = calculate_preset_frame(&preset, &screen_frame(), &gaps);

        // Should have outer gaps applied
        assert_eq!(frame.x, 20.0);
        assert_eq!(frame.y, 20.0);
        assert_eq!(frame.width, 1880.0); // 1920 - 40
        assert_eq!(frame.height, 1040.0); // 1080 - 40
    }

    #[test]
    fn test_half_width_with_inner_gap() {
        let preset = test_preset("50%", "100%", false);
        let gaps = Gaps::uniform(10.0, 0.0);
        let frame = calculate_preset_frame(&preset, &screen_frame(), &gaps);

        // Width should account for inner gap
        assert!((frame.width - (1920.0 - 10.0) / 2.0).abs() < 1.0);
    }

    #[test]
    fn test_is_half_percentage() {
        assert!(is_half_percentage(&DimensionValue::Percentage(
            "50%".to_string()
        )));
        assert!(is_half_percentage(&DimensionValue::Percentage(
            " 50% ".to_string()
        )));
        assert!(!is_half_percentage(&DimensionValue::Percentage(
            "25%".to_string()
        )));
        assert!(!is_half_percentage(&DimensionValue::Pixels(50)));
    }
}
