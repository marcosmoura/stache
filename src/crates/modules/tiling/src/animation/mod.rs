//! Opt-in window animation.
//!
//! `easing`, `spring` and `transition` are the pure curves and interpolation
//! ported from the legacy implementation. `worker` holds the animation
//! coordinator (a deterministic engine over a fake clock and sink that sends
//! one batch per application per tick and never waits for a write) and the
//! one owned worker thread around it; `control` is the lifecycle handle (spawned only
//! while `animations.enabled`, retired on disable and on stop) plus the live
//! sink that routes every write through the existing [`crate::FrameControl`]
//! issuance gate. Nothing here is a parallel write path, a global or a static.

mod control;
mod easing;
mod fallback;
mod metrics;
mod pacing;
mod size;
mod spring;
mod transition;
mod worker;

use std::time::Duration;

pub use control::{AnimationControl, AnimationReport, LiveSink};
pub use easing::apply_easing;
pub use fallback::FallbackReason;
pub use metrics::{MetricsSummary, describe as describe_metrics};
pub use spring::{SpringParams, SpringState};
pub use transition::Transition;
pub use worker::{
    Ack, AckPort, AnimationFrame, AnimationJob, AnimationShared, AnimationSink, AnimationWorker,
    Animator, FrameCounters, ReadKind, ReadReplyFn,
};

use crate::config::{AnimationConfig, EasingType};
use crate::geometry::Rect;

#[cfg(test)]
mod coordinator_tests;
#[cfg(test)]
mod fallback_tests;
#[cfg(test)]
mod pacing_tests;
#[cfg(test)]
mod pipeline_tests;
#[cfg(test)]
mod tests;

/// Shortest configured duration (ms), as legacy.
pub const MIN_DURATION_MS: u32 = 100;
/// Longest configured duration and the spring cap (ms), as legacy. It stays
/// below the 2 s frame plan deadline.
pub const MAX_DURATION_MS: u32 = 1000;
/// Shortest distance-scaled duration (ms).
pub const MIN_DYNAMIC_DURATION_MS: u64 = 50;
/// Below this travel (points) a window is written once, not animated.
pub const MIN_ANIMATION_DISTANCE: f64 = 5.0;
/// The travel that gets the full configured duration.
pub const REFERENCE_DISTANCE: f64 = 500.0;
/// Travel up to which the shortest duration applies.
pub const MIN_SCALED_DISTANCE: f64 = 20.0;
/// How long after the final write the window is left to settle (and its echoes
/// ignored) before the single readback.
pub const SETTLE: Duration = Duration::from_millis(50);
/// Windows animated at once (the planner's in-flight bound).
pub const MAX_ANIMATED: usize = 16;
/// How many frame periods a retargeted job waits for its one starting read
/// before it starts from the last acknowledged frame instead.
pub const RETARGET_READ_PERIODS: u32 = 2;
/// How long the settle readback may stay unanswered before the job reports the
/// final write's outcome without it.
pub const SETTLE_READ_BUDGET: Duration = Duration::from_secs(2);

/// The resolved parameters of an animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimationSettings {
    /// The configured duration for large movements.
    pub duration: Duration,
    /// The easing function.
    pub easing: EasingType,
}

impl Default for AnimationSettings {
    fn default() -> Self { Self::from_config(&AnimationConfig::default()) }
}

impl AnimationSettings {
    /// The settings of a configuration (duration clamped to 100..=1000 ms).
    #[must_use]
    pub fn from_config(config: &AnimationConfig) -> Self {
        Self {
            duration: Duration::from_millis(u64::from(
                config.duration.clamp(MIN_DURATION_MS, MAX_DURATION_MS),
            )),
            easing: config.easing,
        }
    }

    /// The duration for a movement of `max_distance` points: the shortest
    /// duration up to 20 points, the configured one from 500 points, a square
    /// root scale between.
    #[must_use]
    pub fn calculate_duration(&self, max_distance: f64) -> Duration {
        let min = Duration::from_millis(MIN_DYNAMIC_DURATION_MS);
        if max_distance.is_nan() || max_distance <= MIN_SCALED_DISTANCE {
            return min;
        }
        if max_distance >= REFERENCE_DISTANCE {
            return self.duration;
        }
        let normalized = (max_distance / REFERENCE_DISTANCE).sqrt();
        #[allow(clippy::cast_precision_loss)]
        let (min_ms, max_ms) = (MIN_DYNAMIC_DURATION_MS as f64, self.duration.as_millis() as f64);
        let ms = (max_ms - min_ms).mul_add(normalized, min_ms);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Duration::from_millis(ms as u64)
    }
}

/// Whether a plan that moves `from` to `to` runs as an animation: enabled and a
/// meaningful travel. Pure, so the planner decides it deterministically.
#[must_use]
pub fn wants_animation(config: &AnimationConfig, from: &Rect, to: &Rect) -> bool {
    config.enabled && Transition::new(*from, *to).max_distance() >= MIN_ANIMATION_DISTANCE
}
