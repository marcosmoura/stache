//! Window transition types for animation.

use super::easing::lerp;
use crate::modules::tiling::state::Rect;

// ============================================================================
// Types
// ============================================================================

/// A window transition from one frame to another.
#[derive(Debug, Clone)]
pub struct WindowTransition {
    /// Window ID.
    pub window_id: u32,
    /// Starting frame.
    pub from: Rect,
    /// Target frame.
    pub to: Rect,
}

impl WindowTransition {
    /// Creates a new window transition.
    #[must_use]
    pub const fn new(window_id: u32, from: Rect, to: Rect) -> Self { Self { window_id, from, to } }

    /// Returns the maximum distance any property needs to travel.
    #[must_use]
    pub fn max_distance(&self) -> f64 {
        let dx = (self.to.x - self.from.x).abs();
        let dy = (self.to.y - self.from.y).abs();
        let dw = (self.to.width - self.from.width).abs();
        let dh = (self.to.height - self.from.height).abs();
        dx.max(dy).max(dw).max(dh)
    }

    /// Returns whether this transition involves resizing.
    #[must_use]
    pub fn involves_resize(&self) -> bool {
        let dw = (self.to.width - self.from.width).abs();
        let dh = (self.to.height - self.from.height).abs();
        dw > 1.0 || dh > 1.0
    }

    /// Interpolates the frame at a given progress (0.0 to 1.0).
    #[must_use]
    pub fn interpolate(&self, progress: f64) -> Rect {
        let t = progress.clamp(0.0, 1.0);
        Rect::new(
            lerp(self.from.x, self.to.x, t),
            lerp(self.from.y, self.to.y, t),
            lerp(self.from.width, self.to.width, t),
            lerp(self.from.height, self.to.height, t),
        )
    }

    /// Interpolates only the position at a given progress.
    #[must_use]
    pub fn interpolate_position(&self, progress: f64) -> (f64, f64) {
        let t = progress.clamp(0.0, 1.0);
        (lerp(self.from.x, self.to.x, t), lerp(self.from.y, self.to.y, t))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_transition_new() {
        let from = Rect::new(0.0, 0.0, 100.0, 100.0);
        let to = Rect::new(100.0, 100.0, 200.0, 200.0);
        let transition = WindowTransition::new(123, from, to);

        assert_eq!(transition.window_id, 123);
        assert_eq!(transition.from, from);
        assert_eq!(transition.to, to);
    }

    #[test]
    fn test_window_transition_max_distance() {
        let t1 = WindowTransition::new(
            1,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Rect::new(500.0, 10.0, 110.0, 120.0),
        );
        assert!((t1.max_distance() - 500.0).abs() < f64::EPSILON);

        let t2 = WindowTransition::new(
            1,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Rect::new(10.0, 20.0, 130.0, 400.0),
        );
        assert!((t2.max_distance() - 300.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_window_transition_interpolate() {
        let from = Rect::new(0.0, 0.0, 100.0, 100.0);
        let to = Rect::new(100.0, 200.0, 200.0, 300.0);
        let transition = WindowTransition::new(1, from, to);

        let at_start = transition.interpolate(0.0);
        assert_eq!(at_start, from);

        let at_end = transition.interpolate(1.0);
        assert_eq!(at_end, to);

        let at_half = transition.interpolate(0.5);
        assert!((at_half.x - 50.0).abs() < f64::EPSILON);
        assert!((at_half.y - 100.0).abs() < f64::EPSILON);
        assert!((at_half.width - 150.0).abs() < f64::EPSILON);
        assert!((at_half.height - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_window_transition_involves_resize() {
        let position_only = WindowTransition::new(
            1,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Rect::new(50.0, 50.0, 100.0, 100.0),
        );
        assert!(!position_only.involves_resize());

        let with_resize = WindowTransition::new(
            1,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Rect::new(50.0, 50.0, 200.0, 100.0),
        );
        assert!(with_resize.involves_resize());
    }
}
