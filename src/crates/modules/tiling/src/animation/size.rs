//! Which components a frame writes.
//!
//! Position and size animate together: while the size is changing, every frame
//! carries the interpolated size along with the interpolated position, so a
//! window grows or shrinks smoothly instead of sliding at its old size and
//! snapping at the end. A move that keeps the size writes position only.
//!
//! | job     | stage                  | position     | size                                                    |
//! |---------|------------------------|--------------|---------------------------------------------------------|
//! | instant | final                  | as wanted    | as wanted (exactly as a direct write)                   |
//! | any     | first or intermediate  | interpolated | interpolated, only if the plan wanted size and it changes |
//! | any     | final                  | exact target | exact target, whenever the plan wanted size             |
//!
//! The final frame always writes the exact target (no "already written" skip),
//! so a frame the shard replaced or an application that rounded a size can
//! never leave the window off its target. The position is always part of a
//! frame, so no frame is empty. A slow application still skips frames (latest
//! frame wins); pacing is unchanged and follows the display's native refresh.

use stache_macos::windows::FrameStage;

use crate::outcome::FrameComponents;

/// A width and a height.
pub type Dims = (f64, f64);

/// Sizes closer than this (points) are the same size: no resize animates.
pub const SIZE_EPSILON: f64 = 0.5;

/// What one frame writes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageWrite {
    /// The components to write.
    pub components: FrameComponents,
    /// The size the frame carries (written when `components.size`).
    pub size: Dims,
}

/// The inputs that decide a job's frames.
#[derive(Clone, Copy, Debug)]
pub struct SizePlan {
    /// The components the plan wanted.
    pub wanted: FrameComponents,
    /// The size the window starts at.
    pub start: Dims,
    /// The size the window ends at.
    pub target: Dims,
    /// The job is a single exact write.
    pub instant: bool,
}

fn differs(a: Dims, b: Dims) -> bool {
    (a.0 - b.0).abs() > SIZE_EPSILON || (a.1 - b.1).abs() > SIZE_EPSILON
}

impl SizePlan {
    /// Whether this animation resizes the window.
    #[must_use]
    pub fn resizes(&self) -> bool { self.wanted.size && differs(self.start, self.target) }

    /// The write of a frame at `stage` whose interpolated size is
    /// `interpolated`.
    #[must_use]
    pub fn at(&self, stage: FrameStage, interpolated: Dims) -> StageWrite {
        if self.instant || stage == FrameStage::Direct {
            return StageWrite {
                components: self.wanted,
                size: self.target,
            };
        }
        let (size, write_size) = match stage {
            FrameStage::Final => (self.target, self.wanted.size),
            FrameStage::First | FrameStage::Intermediate | FrameStage::Direct => {
                (interpolated, self.resizes())
            }
        };
        StageWrite {
            // Never empty: the position always rides along.
            components: FrameComponents {
                position: true,
                size: write_size,
            },
            size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WANT_BOTH: FrameComponents = FrameComponents { position: true, size: true };

    fn plan(start: Dims, target: Dims, _cross: bool) -> SizePlan {
        SizePlan {
            wanted: WANT_BOTH,
            start,
            target,
            instant: false,
        }
    }

    #[test]
    fn a_resize_writes_the_interpolated_size_on_every_frame() {
        let p = plan((100.0, 100.0), (300.0, 50.0), false);
        for stage in [FrameStage::First, FrameStage::Intermediate] {
            let w = p.at(stage, (200.0, 75.0));
            assert_eq!(w.components, WANT_BOTH, "{stage:?}");
            assert_eq!(w.size, (200.0, 75.0), "{stage:?}");
        }
    }

    #[test]
    fn growth_and_shrink_both_animate_and_end_exact() {
        for (start, target) in [
            ((100.0, 100.0), (300.0, 200.0)),
            ((300.0, 200.0), (100.0, 100.0)),
        ] {
            let p = plan(start, target, false);
            let mid = p.at(FrameStage::Intermediate, (200.0, 150.0));
            assert!(mid.components.size);
            assert_eq!(mid.size, (200.0, 150.0));
            let last = p.at(FrameStage::Final, (200.0, 150.0));
            assert_eq!(last.components, WANT_BOTH);
            assert_eq!(last.size, target, "the final frame is exactly the target");
        }
    }

    #[test]
    fn a_move_that_keeps_the_size_writes_position_only_until_the_final_frame() {
        let p = plan((300.0, 200.0), (300.0, 200.0), false);
        let mid = p.at(FrameStage::Intermediate, (300.0, 200.0));
        assert_eq!(mid.components, FrameComponents { position: true, size: false });
        // The final frame still writes the wanted size exactly.
        assert_eq!(p.at(FrameStage::Final, (300.0, 200.0)).components, WANT_BOTH);
    }

    #[test]
    fn a_plan_that_did_not_want_size_never_writes_it() {
        let p = SizePlan {
            wanted: FrameComponents { position: true, size: false },
            ..plan((100.0, 100.0), (300.0, 300.0), false)
        };
        for stage in [
            FrameStage::First,
            FrameStage::Intermediate,
            FrameStage::Final,
        ] {
            assert!(!p.at(stage, (200.0, 200.0)).components.size, "{stage:?}");
        }
    }

    #[test]
    fn a_cross_display_move_animates_the_size_and_ends_exact() {
        let p = plan((800.0, 600.0), (1200.0, 900.0), true);
        let mid = p.at(FrameStage::Intermediate, (1000.0, 750.0));
        assert!(mid.components.size);
        assert_eq!(mid.size, (1000.0, 750.0));
        let last = p.at(FrameStage::Final, (1000.0, 750.0));
        assert_eq!(last.size, (1200.0, 900.0));
        assert!(last.components.size);
    }

    #[test]
    fn instant_jobs_write_exactly_the_wanted_components() {
        let p = SizePlan {
            instant: true,
            ..plan((100.0, 100.0), (300.0, 300.0), false)
        };
        let w = p.at(FrameStage::Final, (100.0, 100.0));
        assert_eq!(w.components, WANT_BOTH);
        assert_eq!(w.size, (300.0, 300.0));
    }

    #[test]
    fn sub_point_size_noise_does_not_animate_a_resize() {
        let p = plan((300.0, 200.0), (300.3, 200.2), false);
        assert!(!p.at(FrameStage::Intermediate, (300.1, 200.1)).components.size);
    }
}
