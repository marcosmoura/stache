//! Animation system for smooth window transitions.
//!
//! This module provides frame interpolation and easing functions for
//! animating window position and size changes during layout operations.
//!
//! # Spring Animation Model
//!
//! The spring animation uses an analytical solution to the damped harmonic oscillator
//! equation, inspired by Hyprland's animation system. This provides smooth, physically
//! accurate spring animations.
//!
//! The spring uses the damped harmonic oscillator equation:
//! ```text
//! x''(t) + 2ζω₀x'(t) + ω₀²x(t) = ω₀²
//! ```
//!
//! Where:
//! - ζ (zeta) = damping ratio (controls bounciness)
//! - ω₀ = natural frequency = √(k/m)
//!
//! # Usage
//!
//! ```rust,ignore
//! use crate::tiling::animation::{AnimationSystem, WindowTransition};
//!
//! let animator = AnimationSystem::from_config();
//!
//! let transitions = vec![
//!     WindowTransition::new(window_id, current_frame, target_frame),
//! ];
//!
//! animator.animate(transitions);
//! ```

use std::time::{Duration, Instant};

use super::state::Rect;
use super::window::{
    resolve_window_ax_elements, set_window_frames_by_id, set_window_frames_direct,
};
use crate::config::{EasingType, get_config};

// ============================================================================
// Constants
// ============================================================================

/// Minimum animation duration in milliseconds.
const MIN_DURATION_MS: u32 = 50;

/// Maximum animation duration in milliseconds.
const MAX_DURATION_MS: u32 = 1000;

/// Target frame rate for animations (frames per second).
/// Using 60 FPS as a realistic target given macOS Accessibility API overhead.
/// The actual frame rate may be lower if window positioning takes longer.
const TARGET_FPS: u32 = 240;

/// Minimum distance (pixels) for animation. Below this, windows are moved instantly.
const MIN_ANIMATION_DISTANCE: f64 = 5.0;

// ============================================================================
// Spring Constants
// ============================================================================

/// Spring damping ratio (ζ). Controls bounciness.
///
/// - `< 1.0`: Underdamped (bouncy, overshoots)
/// - `= 1.0`: Critically damped (fastest, no overshoot)
/// - `> 1.0`: Overdamped (slow, no overshoot)
///
/// Value: 1.0 (critically damped for fastest settling without overshoot)
const SPRING_DAMPING_RATIO: f64 = 1.0;

/// Spring mass (m). Fixed at 1.0 - stiffness is adjusted to control speed.
const SPRING_MASS: f64 = 1.0;

/// Spring animation position threshold for completion (1% of travel distance).
const SPRING_POSITION_THRESHOLD: f64 = 0.01;

/// Settling time multiplier for critically damped springs.
///
/// For a critically damped spring to reach within 1% of target:
/// T ≈ 6.6 / ω₀, so ω₀ = 6.6 / T
///
/// This is derived from solving: e^(-ω₀T)(1 + ω₀T) = 0.01
const CRITICALLY_DAMPED_SETTLE_FACTOR: f64 = 6.6;

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
}

/// Spring physics parameters calculated from the target duration.
///
/// These parameters define the spring behavior for the damped harmonic oscillator.
#[derive(Debug, Clone, Copy)]
struct SpringParams {
    /// Natural frequency (ω₀ = √(k/m)).
    omega_0: f64,
    /// Stiffness (k). Calculated from duration: k = ω₀² * m.
    stiffness: f64,
    /// Damping ratio (ζ). Fixed at 1.0 for critically damped.
    damping_ratio: f64,
}

impl SpringParams {
    /// Calculates spring parameters from target duration.
    ///
    /// For a critically damped spring to settle within 1% of target at time T:
    /// ω₀ = 6.6 / T, then k = ω₀² * m
    fn from_duration(duration: Duration) -> Self {
        let target_secs = duration.as_secs_f64().max(0.01); // Minimum 10ms

        // Calculate natural frequency from target duration
        let omega_0 = CRITICALLY_DAMPED_SETTLE_FACTOR / target_secs;

        // Calculate stiffness: k = ω₀² * m
        let stiffness = omega_0 * omega_0 * SPRING_MASS;

        Self {
            omega_0,
            stiffness,
            damping_ratio: SPRING_DAMPING_RATIO,
        }
    }
}

/// State for a spring animation using analytical solution.
///
/// Uses the damped harmonic oscillator equation with analytical solutions
/// for underdamped, critically damped, and overdamped cases.
/// Physics parameters are calculated from the configured duration.
#[derive(Debug, Clone)]
struct SpringState {
    /// Elapsed time since animation start.
    elapsed: f64,
    /// Spring physics parameters.
    params: SpringParams,
}

impl SpringState {
    /// Creates a new spring state with physics parameters derived from target duration.
    fn new(target_duration: Duration) -> Self {
        Self {
            elapsed: 0.0,
            params: SpringParams::from_duration(target_duration),
        }
    }

    /// Updates the spring state and returns the new position.
    /// Returns `(position, is_settled)` where position is in range 0.0 to ~1.0+
    /// and `is_settled` is true when the animation is complete.
    fn update(&mut self, dt: f64) -> (f64, bool) {
        self.elapsed += dt;
        let position = self.calculate_position(self.elapsed);

        // Check if settled (close enough to target and minimum time elapsed)
        // Minimum 20ms to avoid premature settling during initial transients
        let is_settled = (position - 1.0).abs() < SPRING_POSITION_THRESHOLD && self.elapsed > 0.02;

        let final_position = if is_settled { 1.0 } else { position };

        // Clamp position to reasonable bounds (allow overshoot for underdamped)
        (final_position.clamp(0.0, 1.5), is_settled)
    }

    /// Calculates the spring position at time `t` using analytical solution.
    ///
    /// Uses the solution to the damped harmonic oscillator equation:
    /// x''(t) + 2ζω₀x'(t) + ω₀²x(t) = ω₀²
    ///
    /// Where:
    /// - ζ (zeta) = damping ratio
    /// - ω₀ = natural frequency (calculated from target duration)
    fn calculate_position(&self, t: f64) -> f64 {
        let omega_0 = self.params.omega_0;
        let zeta = self.params.damping_ratio;

        if zeta < 1.0 {
            // Underdamped: oscillates around target
            Self::underdamped_position(t, omega_0, zeta)
        } else if (zeta - 1.0).abs() < 0.001 {
            // Critically damped: fastest approach without overshoot
            Self::critically_damped_position(t, omega_0)
        } else {
            // Overdamped: slow approach without overshoot
            Self::overdamped_position(t, omega_0, zeta)
        }
    }

    /// Analytical solution for underdamped spring (ζ < 1).
    ///
    /// x(t) = 1 - e^(-ζω₀t) * [cos(ωd*t) + (ζ/√(1-ζ²)) * sin(ωd*t)]
    ///
    /// Where ωd = ω₀ * √(1-ζ²) is the damped frequency.
    #[inline]
    fn underdamped_position(t: f64, omega_0: f64, zeta: f64) -> f64 {
        let zeta_sq_complement = zeta.mul_add(-zeta, 1.0); // 1 - ζ²
        let omega_d = omega_0 * zeta_sq_complement.sqrt();
        let decay = (-zeta * omega_0 * t).exp();
        let cos_term = (omega_d * t).cos();
        let sin_term = (zeta / zeta_sq_complement.sqrt()) * (omega_d * t).sin();

        decay.mul_add(-(cos_term + sin_term), 1.0)
    }

    /// Analytical solution for critically damped spring (ζ = 1).
    ///
    /// x(t) = 1 - e^(-ω₀t) * (1 + ω₀*t)
    #[inline]
    fn critically_damped_position(t: f64, omega_0: f64) -> f64 {
        let decay = (-omega_0 * t).exp();
        decay.mul_add(-omega_0.mul_add(t, 1.0), 1.0)
    }

    /// Analytical solution for overdamped spring (ζ > 1).
    ///
    /// x(t) = 1 - e^(-ζω₀t) * [cosh(γt) + (ζ/√(ζ²-1)) * sinh(γt)]
    ///
    /// Where γ = ω₀ * √(ζ²-1).
    #[inline]
    fn overdamped_position(t: f64, omega_0: f64, zeta: f64) -> f64 {
        let zeta_sq_minus_one = zeta.mul_add(zeta, -1.0); // ζ² - 1
        let gamma = omega_0 * zeta_sq_minus_one.sqrt();
        let decay = (-zeta * omega_0 * t).exp();
        let cosh_term = (gamma * t).cosh();
        let sinh_term = (zeta / zeta_sq_minus_one.sqrt()) * (gamma * t).sinh();

        decay.mul_add(-(cosh_term + sinh_term), 1.0)
    }
}

// ============================================================================
// Easing Functions
// ============================================================================

/// Linear interpolation.
#[inline]
fn lerp(start: f64, end: f64, t: f64) -> f64 { (end - start).mul_add(t, start) }

/// Linear easing (no acceleration).
#[inline]
const fn ease_linear(t: f64) -> f64 { t }

/// Ease-in (slow start, accelerates).
/// Uses cubic function for smooth acceleration.
#[inline]
fn ease_in(t: f64) -> f64 { t * t * t }

/// Ease-out (fast start, decelerates).
/// Uses cubic function for smooth deceleration.
#[inline]
fn ease_out(t: f64) -> f64 {
    let t1 = t - 1.0;
    (t1 * t1).mul_add(t1, 1.0)
}

/// Ease-in-out (slow start and end).
/// Uses cubic function for smooth acceleration and deceleration.
#[inline]
fn ease_in_out(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let t1 = 2.0f64.mul_add(t, -2.0);
        (0.5 * t1 * t1).mul_add(t1, 1.0)
    }
}

/// Applies an easing function based on the easing type.
#[inline]
fn apply_easing(t: f64, easing: EasingType) -> f64 {
    match easing {
        EasingType::Linear => ease_linear(t),
        EasingType::EaseIn => ease_in(t),
        EasingType::EaseOut => ease_out(t),
        EasingType::EaseInOut => ease_in_out(t),
        EasingType::Spring => t, // Spring uses physics simulation, not easing
    }
}

// ============================================================================
// Animation System
// ============================================================================

/// Configuration for the animation system.
#[derive(Debug, Clone)]
pub struct AnimationConfig {
    /// Whether animations are enabled.
    pub enabled: bool,
    /// Animation duration.
    pub duration: Duration,
    /// Easing function type.
    pub easing: EasingType,
}

impl Default for AnimationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            duration: Duration::from_millis(200),
            easing: EasingType::EaseOut,
        }
    }
}

impl AnimationConfig {
    /// Creates animation config from the application configuration.
    #[must_use]
    pub fn from_config() -> Self {
        let config = get_config();
        let anim_config = &config.tiling.animations;

        Self {
            enabled: anim_config.enabled,
            duration: Duration::from_millis(u64::from(
                anim_config.duration.clamp(MIN_DURATION_MS, MAX_DURATION_MS),
            )),
            easing: anim_config.easing,
        }
    }
}

/// The animation system for smooth window transitions.
#[derive(Debug)]
pub struct AnimationSystem {
    /// Animation configuration.
    config: AnimationConfig,
}

impl Default for AnimationSystem {
    fn default() -> Self { Self::new() }
}

impl AnimationSystem {
    /// Creates a new animation system with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: AnimationConfig::default(),
        }
    }

    /// Creates an animation system from the application configuration.
    #[must_use]
    pub fn from_config() -> Self {
        Self {
            config: AnimationConfig::from_config(),
        }
    }

    /// Returns whether animations are enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.config.enabled }

    /// Returns the animation duration.
    #[must_use]
    pub const fn duration(&self) -> Duration { self.config.duration }

    /// Returns the easing type.
    #[must_use]
    pub const fn easing(&self) -> EasingType { self.config.easing }

    /// Animates a list of window transitions.
    ///
    /// If animations are disabled, windows are moved instantly.
    /// Windows that need to move less than `MIN_ANIMATION_DISTANCE` are moved instantly.
    ///
    /// # Arguments
    ///
    /// * `transitions` - List of window transitions to animate
    ///
    /// # Returns
    ///
    /// Number of windows that were successfully positioned.
    pub fn animate(&self, transitions: Vec<WindowTransition>) -> usize {
        if transitions.is_empty() {
            return 0;
        }

        // Separate transitions into animated and instant
        let (animated, instant): (Vec<_>, Vec<_>) = if self.config.enabled {
            transitions
                .into_iter()
                .partition(|t| t.max_distance() >= MIN_ANIMATION_DISTANCE)
        } else {
            // All instant when disabled
            (Vec::new(), transitions)
        };

        // Apply instant transitions immediately
        let mut success_count = 0;
        if !instant.is_empty() {
            let frames: Vec<(u32, Rect)> = instant.iter().map(|t| (t.window_id, t.to)).collect();
            success_count += set_window_frames_by_id(&frames);
        }

        // Animate remaining transitions
        if !animated.is_empty() {
            success_count += self.run_animation(&animated);
        }

        success_count
    }

    /// Runs the animation loop for the given transitions.
    fn run_animation(&self, transitions: &[WindowTransition]) -> usize {
        match self.config.easing {
            EasingType::Spring => self.run_spring_animation(transitions),
            _ => self.run_eased_animation(transitions),
        }
    }

    /// Runs a time-based eased animation.
    ///
    /// Caches AX element references at the start to avoid expensive window list
    /// queries on every frame.
    fn run_eased_animation(&self, transitions: &[WindowTransition]) -> usize {
        let frame_duration = Duration::from_secs(1) / TARGET_FPS;
        let start = Instant::now();
        let duration = self.config.duration;
        let easing = self.config.easing;

        // Resolve AX elements once at the start (expensive operation)
        let window_ids: Vec<u32> = transitions.iter().map(|t| t.window_id).collect();
        let ax_elements = resolve_window_ax_elements(&window_ids);

        // Build a vec of (index, ax_element) for windows we can animate
        let animatable: Vec<(usize, _)> = transitions
            .iter()
            .enumerate()
            .filter_map(|(i, t)| ax_elements.get(&t.window_id).map(|&ax| (i, ax)))
            .collect();

        if animatable.is_empty() {
            return 0;
        }

        loop {
            let elapsed = start.elapsed();
            let progress = (elapsed.as_secs_f64() / duration.as_secs_f64()).min(1.0);
            let eased_progress = apply_easing(progress, easing);

            // Calculate interpolated frames using cached AX elements
            let frames: Vec<(_, Rect)> = animatable
                .iter()
                .map(|&(idx, ax)| (ax, transitions[idx].interpolate(eased_progress)))
                .collect();

            // Apply frames using cached AX elements (fast path)
            let positioned = set_window_frames_direct(&frames);

            // Check if animation is complete
            if progress >= 1.0 {
                return positioned;
            }

            // Sleep until next frame
            let frame_end = start
                + Duration::from_secs_f64(
                    (elapsed.as_secs_f64() / frame_duration.as_secs_f64()).ceil()
                        * frame_duration.as_secs_f64(),
                );
            let now = Instant::now();
            if frame_end > now {
                std::thread::sleep(frame_end - now);
            }
        }
    }

    /// Runs a physics-based spring animation.
    ///
    /// Uses wall-clock time for spring simulation to ensure animations complete
    /// in the expected time regardless of frame rendering overhead.
    ///
    /// Caches AX element references at the start to avoid expensive window list
    /// queries on every frame.
    fn run_spring_animation(&self, transitions: &[WindowTransition]) -> usize {
        let frame_duration = Duration::from_secs(1) / TARGET_FPS;
        let max_duration = Duration::from_millis(u64::from(MAX_DURATION_MS));
        let target_duration = self.config.duration;
        let start = Instant::now();
        let mut last_frame_time = start;

        // Resolve AX elements once at the start (expensive operation)
        let window_ids: Vec<u32> = transitions.iter().map(|t| t.window_id).collect();
        let ax_elements = resolve_window_ax_elements(&window_ids);

        // Build a vec of (index, ax_element) for windows we can animate
        let animatable: Vec<(usize, _)> = transitions
            .iter()
            .enumerate()
            .filter_map(|(i, t)| ax_elements.get(&t.window_id).map(|&ax| (i, ax)))
            .collect();

        if animatable.is_empty() {
            return 0;
        }

        let mut spring_states: Vec<SpringState> =
            transitions.iter().map(|_| SpringState::new(target_duration)).collect();

        loop {
            // Calculate actual elapsed time since last frame (wall-clock time)
            let now = Instant::now();
            let dt = (now - last_frame_time).as_secs_f64();
            last_frame_time = now;

            // Update all springs and check if all have settled
            let mut all_settled = true;
            let frames: Vec<(_, Rect)> = animatable
                .iter()
                .map(|&(idx, ax)| {
                    let (progress, settled) = spring_states[idx].update(dt);
                    if !settled {
                        all_settled = false;
                    }
                    (ax, transitions[idx].interpolate(progress))
                })
                .collect();

            // Apply frames using cached AX elements (fast path)
            let positioned = set_window_frames_direct(&frames);

            // Check completion conditions
            if all_settled || start.elapsed() > max_duration {
                // Ensure final positions are exact
                let final_frames: Vec<(_, Rect)> =
                    animatable.iter().map(|&(idx, ax)| (ax, transitions[idx].to)).collect();
                return set_window_frames_direct(&final_frames).max(positioned);
            }

            // Sleep until next frame (may be shorter or zero if frame took long to render)
            let remaining = frame_duration.saturating_sub(now.elapsed());
            if !remaining.is_zero() {
                std::thread::sleep(remaining);
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Easing Function Tests
    // ========================================================================

    #[test]
    fn test_lerp() {
        assert!((lerp(0.0, 100.0, 0.0) - 0.0).abs() < f64::EPSILON);
        assert!((lerp(0.0, 100.0, 0.5) - 50.0).abs() < f64::EPSILON);
        assert!((lerp(0.0, 100.0, 1.0) - 100.0).abs() < f64::EPSILON);
        assert!((lerp(50.0, 150.0, 0.25) - 75.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ease_linear() {
        assert!((ease_linear(0.0) - 0.0).abs() < f64::EPSILON);
        assert!((ease_linear(0.5) - 0.5).abs() < f64::EPSILON);
        assert!((ease_linear(1.0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ease_in() {
        // Ease-in should be slower at the start
        assert!((ease_in(0.0) - 0.0).abs() < f64::EPSILON);
        assert!(ease_in(0.5) < 0.5); // Should be less than linear at midpoint
        assert!((ease_in(1.0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ease_out() {
        // Ease-out should be faster at the start
        assert!((ease_out(0.0) - 0.0).abs() < f64::EPSILON);
        assert!(ease_out(0.5) > 0.5); // Should be more than linear at midpoint
        assert!((ease_out(1.0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ease_in_out() {
        // Ease-in-out should be slower at both ends
        assert!((ease_in_out(0.0) - 0.0).abs() < f64::EPSILON);
        assert!((ease_in_out(0.5) - 0.5).abs() < f64::EPSILON); // Exactly 0.5 at midpoint
        assert!((ease_in_out(1.0) - 1.0).abs() < f64::EPSILON);

        // Should be slower than linear in first quarter
        assert!(ease_in_out(0.25) < 0.25);
        // Should be faster than linear in third quarter
        assert!(ease_in_out(0.75) > 0.75);
    }

    #[test]
    fn test_apply_easing() {
        // Test that apply_easing routes correctly
        assert!((apply_easing(0.5, EasingType::Linear) - ease_linear(0.5)).abs() < f64::EPSILON);
        assert!((apply_easing(0.5, EasingType::EaseIn) - ease_in(0.5)).abs() < f64::EPSILON);
        assert!((apply_easing(0.5, EasingType::EaseOut) - ease_out(0.5)).abs() < f64::EPSILON);
        assert!((apply_easing(0.5, EasingType::EaseInOut) - ease_in_out(0.5)).abs() < f64::EPSILON);
        // Spring just returns t (physics simulation handles it separately)
        assert!((apply_easing(0.5, EasingType::Spring) - 0.5).abs() < f64::EPSILON);
    }

    // ========================================================================
    // WindowTransition Tests
    // ========================================================================

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
        // X is the max distance
        let t1 = WindowTransition::new(
            1,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            Rect::new(500.0, 10.0, 110.0, 120.0),
        );
        assert!((t1.max_distance() - 500.0).abs() < f64::EPSILON);

        // Height is the max distance
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

        // At t=0, should be at start
        let at_start = transition.interpolate(0.0);
        assert_eq!(at_start, from);

        // At t=1, should be at end
        let at_end = transition.interpolate(1.0);
        assert_eq!(at_end, to);

        // At t=0.5, should be in the middle
        let at_mid = transition.interpolate(0.5);
        assert!((at_mid.x - 50.0).abs() < f64::EPSILON);
        assert!((at_mid.y - 100.0).abs() < f64::EPSILON);
        assert!((at_mid.width - 150.0).abs() < f64::EPSILON);
        assert!((at_mid.height - 200.0).abs() < f64::EPSILON);

        // Out of range should be clamped
        let clamped_low = transition.interpolate(-0.5);
        assert_eq!(clamped_low, from);

        let clamped_high = transition.interpolate(1.5);
        assert_eq!(clamped_high, to);
    }

    // ========================================================================
    // SpringState Tests
    // ========================================================================

    #[test]
    fn test_spring_state_initial() {
        let spring = SpringState::new(Duration::from_millis(200));
        assert!((spring.elapsed - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_spring_state_converges() {
        let mut spring = SpringState::new(Duration::from_millis(200));
        let dt = 1.0 / 60.0; // 60 FPS

        // Run for up to 1000 frames (should converge well before that)
        for _ in 0..1000 {
            let (pos, settled) = spring.update(dt);
            if settled {
                assert!((pos - 1.0).abs() < 0.01);
                return;
            }
        }

        // If we got here, spring should still be very close to 1.0
        let final_pos = spring.calculate_position(spring.elapsed);
        assert!((final_pos - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_spring_state_overshoots() {
        let mut spring = SpringState::new(Duration::from_millis(200));
        let dt = 1.0 / 60.0;
        let mut max_pos: f64 = 0.0;

        // Run for a while and track maximum position
        for _ in 0..120 {
            let (pos, _) = spring.update(dt);
            max_pos = max_pos.max(pos);
        }

        // Spring should reach target (critically damped springs don't overshoot)
        assert!(max_pos >= 1.0, "Spring should reach target");
    }

    #[test]
    fn test_spring_analytical_solution_starts_at_zero() {
        let spring = SpringState::new(Duration::from_millis(200));
        let pos = spring.calculate_position(0.0);
        assert!((pos - 0.0).abs() < 0.01, "Spring should start at 0");
    }

    #[test]
    fn test_spring_analytical_solution_approaches_one() {
        let spring = SpringState::new(Duration::from_millis(200));
        // After a long time, should be very close to 1.0
        let pos = spring.calculate_position(2.0);
        assert!(
            (pos - 1.0).abs() < 0.001,
            "Spring should approach 1.0, got {pos}"
        );
    }

    #[test]
    fn test_spring_respects_target_duration() {
        // Test that spring with 200ms duration settles around that time
        let spring = SpringState::new(Duration::from_millis(200));
        let pos_at_target = spring.calculate_position(0.2);
        // Should be within threshold of 1.0 at target duration
        assert!(
            (pos_at_target - 1.0).abs() < SPRING_POSITION_THRESHOLD * 2.0,
            "Spring should be near target at configured duration, got {pos_at_target}"
        );
    }

    #[test]
    fn test_spring_params_from_duration() {
        // Test physics calculation for 200ms duration
        let params = SpringParams::from_duration(Duration::from_millis(200));

        // ω₀ = 6.6 / 0.2 = 33
        let expected_omega = CRITICALLY_DAMPED_SETTLE_FACTOR / 0.2;
        assert!(
            (params.omega_0 - expected_omega).abs() < 0.01,
            "omega_0 should be {expected_omega}, got {}",
            params.omega_0
        );

        // k = ω₀² * m = 33² * 1 = 1089
        let expected_stiffness = expected_omega * expected_omega * SPRING_MASS;
        assert!(
            (params.stiffness - expected_stiffness).abs() < 0.01,
            "stiffness should be {expected_stiffness}, got {}",
            params.stiffness
        );

        assert!((params.damping_ratio - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_spring_faster_duration_higher_stiffness() {
        let fast = SpringParams::from_duration(Duration::from_millis(100));
        let slow = SpringParams::from_duration(Duration::from_millis(400));

        // Faster animation should have higher stiffness
        assert!(
            fast.stiffness > slow.stiffness,
            "100ms spring (k={}) should be stiffer than 400ms spring (k={})",
            fast.stiffness,
            slow.stiffness
        );
    }

    // ========================================================================
    // AnimationConfig Tests
    // ========================================================================

    #[test]
    fn test_animation_config_default() {
        let config = AnimationConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.duration, Duration::from_millis(200));
        assert_eq!(config.easing, EasingType::EaseOut);
    }

    // ========================================================================
    // AnimationSystem Tests
    // ========================================================================

    #[test]
    fn test_animation_system_new() {
        let system = AnimationSystem::new();
        assert!(!system.is_enabled());
    }

    #[test]
    fn test_animation_system_accessors() {
        let system = AnimationSystem::new();
        assert!(!system.is_enabled());
        assert_eq!(system.duration(), Duration::from_millis(200));
        assert_eq!(system.easing(), EasingType::EaseOut);
    }

    #[test]
    fn test_animation_system_empty_transitions() {
        let system = AnimationSystem::new();
        let result = system.animate(Vec::new());
        assert_eq!(result, 0);
    }

    // Note: Integration tests for actual window animation would require
    // a real display and accessibility permissions, so they're skipped here.
}
