//! Gap configuration and handling for layouts.

use crate::config::{GapsConfig, GapsConfigValue};
use crate::tiling::state::Rect;

/// Resolved gap values for layout calculations.
#[derive(Debug, Clone, Copy, Default)]
pub struct Gaps {
    /// Inner gap between windows (horizontal).
    pub inner_h: f64,
    /// Inner gap between windows (vertical).
    pub inner_v: f64,
    /// Outer gap from screen top edge.
    pub outer_top: f64,
    /// Outer gap from screen right edge.
    pub outer_right: f64,
    /// Outer gap from screen bottom edge.
    pub outer_bottom: f64,
    /// Outer gap from screen left edge.
    pub outer_left: f64,
}

impl Gaps {
    /// Creates gaps with uniform inner and outer values.
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

    /// Resolves gaps from configuration for a specific screen.
    ///
    /// # Arguments
    ///
    /// * `config` - The gaps configuration value
    /// * `screen_name` - Name of the screen to resolve gaps for
    /// * `is_main_screen` - Whether this is the main screen
    #[must_use]
    pub fn from_config(config: &GapsConfigValue, screen_name: &str, is_main_screen: bool) -> Self {
        match config {
            GapsConfigValue::Global(g) => Self::from_gaps_config(g),
            GapsConfigValue::PerScreen(screens) => {
                // Find matching screen config
                let screen_config = screens.iter().find(|s| {
                    s.screen.eq_ignore_ascii_case(screen_name)
                        || (s.screen.eq_ignore_ascii_case("main") && is_main_screen)
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
        }
    }

    /// Converts a `GapsConfig` to `Gaps`.
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

    /// Applies outer gaps to a screen frame, returning the usable area.
    #[must_use]
    pub fn apply_outer(&self, frame: &Rect) -> Rect {
        Rect::new(
            frame.x + self.outer_left,
            frame.y + self.outer_top,
            frame.width - self.outer_left - self.outer_right,
            frame.height - self.outer_top - self.outer_bottom,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gaps_uniform() {
        let gaps = Gaps::uniform(10.0, 20.0);
        assert_eq!(gaps.inner_h, 10.0);
        assert_eq!(gaps.inner_v, 10.0);
        assert_eq!(gaps.outer_top, 20.0);
        assert_eq!(gaps.outer_right, 20.0);
        assert_eq!(gaps.outer_bottom, 20.0);
        assert_eq!(gaps.outer_left, 20.0);
    }

    #[test]
    fn test_gaps_is_zero() {
        assert!(Gaps::default().is_zero());
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
}
