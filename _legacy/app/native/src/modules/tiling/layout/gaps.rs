//! Gap configuration for layout calculations.
//!
//! Gaps define the spacing between windows (inner) and around the edges
//! of the screen (outer). This module provides a self-contained `Gaps`
//! struct that can be used independently of the v1 configuration system.

use std::hash::{Hash, Hasher};

use crate::config::{GapsConfig, GapsConfigValue};
use crate::modules::tiling::state::Rect;

/// Gap values for layout calculations.
///
/// Inner gaps define spacing between windows.
/// Outer gaps define spacing from screen edges.
#[derive(Debug, Clone, Copy, Default)]
pub struct Gaps {
    /// Horizontal gap between windows.
    pub inner_h: f64,
    /// Vertical gap between windows.
    pub inner_v: f64,
    /// Gap from screen top edge.
    pub outer_top: f64,
    /// Gap from screen right edge.
    pub outer_right: f64,
    /// Gap from screen bottom edge.
    pub outer_bottom: f64,
    /// Gap from screen left edge.
    pub outer_left: f64,
}

impl Gaps {
    /// Create new gaps with all values set to zero.
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            inner_h: 0.0,
            inner_v: 0.0,
            outer_top: 0.0,
            outer_right: 0.0,
            outer_bottom: 0.0,
            outer_left: 0.0,
        }
    }

    /// Create gaps with uniform inner and outer values.
    #[must_use]
    pub const fn uniform(inner: f64, outer: f64) -> Self {
        Self {
            inner_h: inner,
            inner_v: inner,
            outer_top: outer,
            outer_right: outer,
            outer_bottom: outer,
            outer_left: outer,
        }
    }

    /// Create gaps with separate inner and outer values for each direction.
    #[must_use]
    pub const fn new(
        inner_h: f64,
        inner_v: f64,
        outer_top: f64,
        outer_right: f64,
        outer_bottom: f64,
        outer_left: f64,
    ) -> Self {
        Self {
            inner_h,
            inner_v,
            outer_top,
            outer_right,
            outer_bottom,
            outer_left,
        }
    }

    /// Returns true if all gaps are zero.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.inner_h == 0.0
            && self.inner_v == 0.0
            && self.outer_top == 0.0
            && self.outer_right == 0.0
            && self.outer_bottom == 0.0
            && self.outer_left == 0.0
    }

    /// Apply outer gaps to a screen frame, returning the usable area.
    #[must_use]
    pub fn apply_outer(&self, frame: &Rect) -> Rect {
        Rect::new(
            frame.x + self.outer_left,
            frame.y + self.outer_top,
            frame.width - self.outer_left - self.outer_right,
            frame.height - self.outer_top - self.outer_bottom,
        )
    }

    /// Compute a hash of the gap values for cache validation.
    #[must_use]
    pub fn compute_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;

        let mut hasher = DefaultHasher::new();
        self.inner_h.to_bits().hash(&mut hasher);
        self.inner_v.to_bits().hash(&mut hasher);
        self.outer_top.to_bits().hash(&mut hasher);
        self.outer_right.to_bits().hash(&mut hasher);
        self.outer_bottom.to_bits().hash(&mut hasher);
        self.outer_left.to_bits().hash(&mut hasher);
        hasher.finish()
    }

    /// Create gaps with an additional offset added to the top outer gap.
    /// Useful for accounting for the status bar.
    #[must_use]
    pub const fn with_top_offset(mut self, offset: f64) -> Self {
        self.outer_top += offset;
        self
    }

    /// Resolves gaps from configuration for a specific screen.
    ///
    /// On the main screen, the bar offset (bar height + padding) is automatically
    /// added to the top gap to account for the status bar.
    ///
    /// # Arguments
    ///
    /// * `config` - The gaps configuration value
    /// * `screen_name` - Name of the screen to resolve gaps for
    /// * `is_main_screen` - Whether this is the main screen
    /// * `bar_offset` - Additional top offset for the status bar (only applied on main screen)
    #[must_use]
    pub fn from_config(
        config: &GapsConfigValue,
        screen_name: &str,
        is_main_screen: bool,
        bar_offset: f64,
    ) -> Self {
        let mut gaps = match config {
            GapsConfigValue::Global(g) => Self::from_gaps_config(g),
            GapsConfigValue::PerScreen(screens) => {
                // Find matching screen config
                let screen_config = screens.iter().find(|s| {
                    s.screen.eq_ignore_ascii_case(screen_name)
                        || ((s.screen.eq_ignore_ascii_case("main")
                            || s.screen.eq_ignore_ascii_case("primary"))
                            && is_main_screen)
                        || (s.screen.eq_ignore_ascii_case("secondary") && !is_main_screen)
                });

                // Use matched screen, or fall back to first screen, or use defaults
                let config = screen_config.or_else(|| screens.first());

                config.map_or_else(Self::default, |s| {
                    let (inner_h, inner_v) = s.inner.as_inner();
                    let (outer_top, outer_right, outer_bottom, outer_left) = s.outer.as_outer();

                    Self {
                        inner_h: f64::from(inner_h),
                        inner_v: f64::from(inner_v),
                        outer_top: f64::from(outer_top),
                        outer_right: f64::from(outer_right),
                        outer_bottom: f64::from(outer_bottom),
                        outer_left: f64::from(outer_left),
                    }
                })
            }
        };

        // Add bar offset to top gap on main screen only
        if is_main_screen {
            gaps.outer_top += bar_offset;
        }

        gaps
    }

    /// Converts a [`GapsConfig`] to [`Gaps`].
    fn from_gaps_config(config: &GapsConfig) -> Self {
        let (inner_h, inner_v) = config.inner.as_inner();
        let (outer_top, outer_right, outer_bottom, outer_left) = config.outer.as_outer();

        Self {
            inner_h: f64::from(inner_h),
            inner_v: f64::from(inner_v),
            outer_top: f64::from(outer_top),
            outer_right: f64::from(outer_right),
            outer_bottom: f64::from(outer_bottom),
            outer_left: f64::from(outer_left),
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn test_gaps_zero() {
        let gaps = Gaps::zero();
        assert!(gaps.is_zero());
        assert_eq!(gaps.inner_h, 0.0);
        assert_eq!(gaps.outer_top, 0.0);
    }

    #[test]
    fn test_gaps_uniform() {
        let gaps = Gaps::uniform(10.0, 20.0);
        assert_eq!(gaps.inner_h, 10.0);
        assert_eq!(gaps.inner_v, 10.0);
        assert_eq!(gaps.outer_top, 20.0);
        assert_eq!(gaps.outer_right, 20.0);
        assert_eq!(gaps.outer_bottom, 20.0);
        assert_eq!(gaps.outer_left, 20.0);
        assert!(!gaps.is_zero());
    }

    #[test]
    fn test_gaps_new() {
        let gaps = Gaps::new(5.0, 10.0, 15.0, 20.0, 25.0, 30.0);
        assert_eq!(gaps.inner_h, 5.0);
        assert_eq!(gaps.inner_v, 10.0);
        assert_eq!(gaps.outer_top, 15.0);
        assert_eq!(gaps.outer_right, 20.0);
        assert_eq!(gaps.outer_bottom, 25.0);
        assert_eq!(gaps.outer_left, 30.0);
    }

    #[test]
    fn test_gaps_is_zero() {
        assert!(Gaps::default().is_zero());
        assert!(Gaps::zero().is_zero());
        assert!(!Gaps::uniform(10.0, 0.0).is_zero());
        assert!(!Gaps::uniform(0.0, 10.0).is_zero());
    }

    #[test]
    fn test_gaps_apply_outer() {
        let gaps = Gaps::uniform(10.0, 20.0);
        let frame = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let usable = gaps.apply_outer(&frame);

        assert_eq!(usable.x, 20.0);
        assert_eq!(usable.y, 20.0);
        assert_eq!(usable.width, 1880.0); // 1920 - 20 - 20
        assert_eq!(usable.height, 1040.0); // 1080 - 20 - 20
    }

    #[test]
    fn test_gaps_apply_outer_asymmetric() {
        let gaps = Gaps::new(0.0, 0.0, 50.0, 20.0, 20.0, 20.0);
        let frame = Rect::new(0.0, 0.0, 1000.0, 800.0);
        let usable = gaps.apply_outer(&frame);

        assert_eq!(usable.x, 20.0); // outer_left
        assert_eq!(usable.y, 50.0); // outer_top
        assert_eq!(usable.width, 960.0); // 1000 - 20 - 20
        assert_eq!(usable.height, 730.0); // 800 - 50 - 20
    }

    #[test]
    fn test_gaps_with_top_offset() {
        let gaps = Gaps::uniform(10.0, 20.0).with_top_offset(40.0);
        assert_eq!(gaps.outer_top, 60.0); // 20 + 40
        assert_eq!(gaps.outer_bottom, 20.0); // unchanged
    }

    #[test]
    fn test_compute_hash_deterministic() {
        let gaps = Gaps::uniform(10.0, 20.0);
        let hash1 = gaps.compute_hash();
        let hash2 = gaps.compute_hash();
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_different_values() {
        let gaps1 = Gaps::uniform(10.0, 20.0);
        let gaps2 = Gaps::uniform(15.0, 20.0);
        assert_ne!(gaps1.compute_hash(), gaps2.compute_hash());
    }

    #[test]
    fn test_compute_hash_different_outer_values() {
        let gaps1 = Gaps::new(10.0, 10.0, 20.0, 20.0, 20.0, 20.0);
        let gaps2 = Gaps::new(10.0, 10.0, 25.0, 20.0, 20.0, 20.0);
        assert_ne!(gaps1.compute_hash(), gaps2.compute_hash());
    }
}
