//! One window transition between two frames, ported from the legacy
//! `WindowTransition` (without its native target; the job carries the window).

use super::easing::lerp;
use crate::geometry::Rect;

/// A transition from one frame to another.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transition {
    /// The starting frame.
    pub from: Rect,
    /// The target frame.
    pub to: Rect,
}

impl Transition {
    /// A transition.
    #[must_use]
    pub const fn new(from: Rect, to: Rect) -> Self { Self { from, to } }

    /// The largest distance any of x, y, width or height has to travel.
    #[must_use]
    pub fn max_distance(&self) -> f64 {
        let dx = (self.to.x - self.from.x).abs();
        let dy = (self.to.y - self.from.y).abs();
        let dw = (self.to.width - self.from.width).abs();
        let dh = (self.to.height - self.from.height).abs();
        dx.max(dy).max(dw).max(dh)
    }

    /// Whether the transition resizes.
    #[must_use]
    pub fn involves_resize(&self) -> bool {
        (self.to.width - self.from.width).abs() > 1.0
            || (self.to.height - self.from.height).abs() > 1.0
    }

    /// The frame at `progress`, clamped into 0 to 1, so a spring never carries a
    /// window past its target.
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
}
