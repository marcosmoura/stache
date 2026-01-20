//! Animation system for smooth window transitions.
//!
//! This module provides frame interpolation, easing functions, and spring physics
//! for animating window position and size changes during layout operations.
//!
//! # Architecture
//!
//! The animation system is split into several submodules:
//! - `easing` - Time-based easing curves (linear, ease-in, ease-out, etc.)
//! - `spring` - Physics-based spring animations
//! - `transition` - Window transition types
//! - `state` - Animation lifecycle and cancellation management
//! - `sync` - Display synchronization (vsync, `CVDisplayLink`, `CATransaction`)
//!
//! # Usage
//!
//! ```rust,ignore
//! use crate::tiling::effects::animation::{AnimationSystem, WindowTransition};
//!
//! let animator = AnimationSystem::from_config();
//! let transitions = vec![
//!     WindowTransition::new(window_id, current_frame, target_frame),
//! ];
//! animator.animate(transitions);
//! ```

mod easing;
mod spring;
mod state;
mod sync;
mod transition;

use std::ffi::c_void;
use std::time::{Duration, Instant};

use core_foundation::base::TCFType;
// Re-export public types and functions
pub use easing::{apply_easing, lerp};
pub use spring::{SpringParams, SpringState};
pub use state::{
    ANIMATION_SETTLE_DURATION_MS, begin_animation, cancel_animation, clear_animation_end_time,
    clear_interrupted_positions, get_interrupted_position, is_animation_active,
    is_animation_settling, set_animation_active, should_cancel, should_ignore_geometry_events,
    store_interrupted_positions,
};
pub use sync::{
    ca_transaction_begin_disabled, ca_transaction_commit, init_display_link, precision_sleep,
    set_high_priority_thread, target_fps, wait_for_next_frame,
};
pub use transition::WindowTransition;

use crate::config::{EasingType, get_config};
use crate::modules::tiling::effects::window_ops::{resolve_window_element, set_window_frame_fast};
use crate::modules::tiling::state::Rect;

// ============================================================================
// FFI for CFRelease
// ============================================================================

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

// ============================================================================
// Constants
// ============================================================================

/// Minimum animation duration (ms).
const MIN_DURATION_MS: u32 = 100;

/// Maximum animation duration (ms).
const MAX_DURATION_MS: u32 = 1000;

/// Minimum dynamic duration for distance-based calculations (ms).
const MIN_DYNAMIC_DURATION_MS: u64 = 50;

/// Minimum distance (pixels) for animation. Below this, windows are moved instantly.
const MIN_ANIMATION_DISTANCE: f64 = 5.0;

/// Reference distance (pixels) for full animation duration.
const REFERENCE_DISTANCE: f64 = 500.0;

// ============================================================================
// Animation Config
// ============================================================================

/// Configuration for the animation system.
#[derive(Debug, Clone)]
pub struct AnimationConfig {
    /// Whether animations are enabled.
    pub enabled: bool,
    /// Base animation duration (for large movements).
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

    /// Calculates the animation duration based on travel distance.
    #[must_use]
    pub fn calculate_duration(&self, max_distance: f64) -> Duration {
        const MIN_DISTANCE: f64 = 20.0;
        let min_duration = Duration::from_millis(MIN_DYNAMIC_DURATION_MS);

        if max_distance <= MIN_DISTANCE {
            return min_duration;
        }

        if max_distance >= REFERENCE_DISTANCE {
            return self.duration;
        }

        let normalized = (max_distance / REFERENCE_DISTANCE).sqrt();
        #[allow(clippy::cast_precision_loss)]
        let min_ms = MIN_DYNAMIC_DURATION_MS as f64;
        #[allow(clippy::cast_precision_loss)]
        let max_ms = self.duration.as_millis() as f64;
        let duration_ms = (max_ms - min_ms).mul_add(normalized, min_ms);

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Duration::from_millis(duration_ms as u64)
    }
}

// ============================================================================
// Animation System
// ============================================================================

/// The animation system for smooth window transitions.
#[derive(Debug)]
pub struct AnimationSystem {
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
    ///
    /// # Returns
    ///
    /// Number of windows that were successfully positioned.
    #[must_use]
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
            (Vec::new(), transitions)
        };

        // Apply instant transitions immediately
        let mut success_count = 0;
        if !instant.is_empty() {
            success_count += self.apply_instant(&instant);
        }

        // Animate remaining transitions
        if !animated.is_empty() {
            success_count += self.run_animation(&animated);
        }

        success_count
    }

    /// Applies transitions instantly (no animation).
    #[allow(clippy::unused_self)] // Self kept for consistency and future config access
    fn apply_instant(&self, transitions: &[WindowTransition]) -> usize {
        let mut count = 0;
        for t in transitions {
            if set_window_frame_fast(t.window_id, &t.to) {
                count += 1;
            }
        }
        count
    }

    /// Runs the animation loop for the given transitions.
    fn run_animation(&self, transitions: &[WindowTransition]) -> usize {
        let max_distance =
            transitions.iter().map(WindowTransition::max_distance).fold(0.0_f64, f64::max);

        let duration = self.config.calculate_duration(max_distance);

        match self.config.easing {
            EasingType::Spring => self.run_spring_animation(transitions, duration),
            _ => self.run_eased_animation(transitions, duration),
        }
    }

    /// Runs a time-based eased animation.
    fn run_eased_animation(&self, transitions: &[WindowTransition], duration: Duration) -> usize {
        set_animation_active(true);
        init_display_link();
        set_high_priority_thread();

        let fps = target_fps();
        let frame_duration = Duration::from_secs(1) / fps;
        let start = Instant::now();
        let easing = self.config.easing;

        // Resolve AX elements once at the start
        let animatable: Vec<_> = transitions
            .iter()
            .enumerate()
            .filter_map(|(i, t)| resolve_window_element(t.window_id).map(|ax| (i, ax)))
            .collect();

        if animatable.is_empty() {
            set_animation_active(false);
            return 0;
        }

        let window_ids: Vec<u32> = transitions.iter().map(|t| t.window_id).collect();

        loop {
            // Check for cancellation
            if should_cancel() {
                // Snap to final positions
                ca_transaction_begin_disabled();
                for &(idx, ax) in &animatable {
                    let frame = &transitions[idx].to;
                    let _ = set_frame_direct(ax, frame);
                }
                ca_transaction_commit();

                clear_interrupted_positions(&window_ids);
                cleanup_ax_elements(&animatable);
                set_animation_active(false);
                return animatable.len();
            }

            let elapsed = start.elapsed();
            let progress = (elapsed.as_secs_f64() / duration.as_secs_f64()).min(1.0);
            let eased_progress = apply_easing(progress, easing);

            ca_transaction_begin_disabled();
            for &(idx, ax) in &animatable {
                let frame = transitions[idx].interpolate(eased_progress);
                let _ = set_frame_direct(ax, &frame);
            }
            ca_transaction_commit();

            if progress >= 1.0 {
                clear_interrupted_positions(&window_ids);
                cleanup_ax_elements(&animatable);
                set_animation_active(false);
                return animatable.len();
            }

            wait_for_next_frame(frame_duration);
        }
    }

    /// Runs a physics-based spring animation.
    #[allow(clippy::unused_self)] // Self kept for consistency and future config access
    fn run_spring_animation(&self, transitions: &[WindowTransition], duration: Duration) -> usize {
        set_animation_active(true);
        init_display_link();
        set_high_priority_thread();

        let fps = target_fps();
        let frame_duration = Duration::from_secs(1) / fps;
        let max_duration = Duration::from_millis(u64::from(MAX_DURATION_MS));
        let start = Instant::now();
        let mut last_frame_time = start;

        // Resolve AX elements once at the start
        let animatable: Vec<_> = transitions
            .iter()
            .enumerate()
            .filter_map(|(i, t)| resolve_window_element(t.window_id).map(|ax| (i, ax)))
            .collect();

        if animatable.is_empty() {
            set_animation_active(false);
            return 0;
        }

        let window_ids: Vec<u32> = transitions.iter().map(|t| t.window_id).collect();
        let mut spring_states: Vec<SpringState> =
            transitions.iter().map(|_| SpringState::new(duration)).collect();

        loop {
            // Check for cancellation
            if should_cancel() {
                ca_transaction_begin_disabled();
                for &(idx, ax) in &animatable {
                    let frame = &transitions[idx].to;
                    let _ = set_frame_direct(ax, frame);
                }
                ca_transaction_commit();

                clear_interrupted_positions(&window_ids);
                cleanup_ax_elements(&animatable);
                set_animation_active(false);
                return animatable.len();
            }

            let now = Instant::now();
            let dt = (now - last_frame_time).as_secs_f64();
            last_frame_time = now;

            let mut all_settled = true;
            for &(idx, _) in &animatable {
                let (_, settled) = spring_states[idx].update(dt);
                if !settled {
                    all_settled = false;
                }
            }

            ca_transaction_begin_disabled();
            for &(idx, ax) in &animatable {
                let progress = spring_states[idx].calculate_position(spring_states[idx].elapsed);
                let frame = transitions[idx].interpolate(progress);
                let _ = set_frame_direct(ax, &frame);
            }
            ca_transaction_commit();

            if all_settled || start.elapsed() > max_duration {
                // Ensure exact final positions
                ca_transaction_begin_disabled();
                for &(idx, ax) in &animatable {
                    let frame = &transitions[idx].to;
                    let _ = set_frame_direct(ax, frame);
                }
                ca_transaction_commit();

                clear_interrupted_positions(&window_ids);
                cleanup_ax_elements(&animatable);
                set_animation_active(false);
                return animatable.len();
            }

            wait_for_next_frame(frame_duration);
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Sets frame directly using a resolved AX element.
fn set_frame_direct(element: *mut c_void, frame: &Rect) -> bool {
    use std::cell::OnceCell;

    use core_foundation::string::CFString;

    thread_local! {
        static CF_POSITION: OnceCell<CFString> = const { OnceCell::new() };
        static CF_SIZE: OnceCell<CFString> = const { OnceCell::new() };
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXUIElementSetAttributeValue(
            element: *mut c_void,
            attribute: *const c_void,
            value: *const c_void,
        ) -> i32;
        fn AXValueCreate(value_type: i32, value: *const c_void) -> *mut c_void;
    }

    const K_AX_VALUE_TYPE_CG_POINT: i32 = 1;
    const K_AX_VALUE_TYPE_CG_SIZE: i32 = 2;
    const K_AX_ERROR_SUCCESS: i32 = 0;

    if element.is_null() {
        return false;
    }

    let cf_pos = CF_POSITION
        .with(|cell| cell.get_or_init(|| CFString::new("AXPosition")).as_concrete_TypeRef().cast());
    let cf_size = CF_SIZE
        .with(|cell| cell.get_or_init(|| CFString::new("AXSize")).as_concrete_TypeRef().cast());

    unsafe {
        // Set position
        let point = core_graphics::geometry::CGPoint::new(frame.x, frame.y);
        let pos_value = AXValueCreate(K_AX_VALUE_TYPE_CG_POINT, (&raw const point).cast());
        if pos_value.is_null() {
            return false;
        }
        let pos_result = AXUIElementSetAttributeValue(element, cf_pos, pos_value.cast());
        CFRelease(pos_value.cast());

        // Set size
        let size = core_graphics::geometry::CGSize::new(frame.width, frame.height);
        let size_value = AXValueCreate(K_AX_VALUE_TYPE_CG_SIZE, (&raw const size).cast());
        if size_value.is_null() {
            return false;
        }
        let size_result = AXUIElementSetAttributeValue(element, cf_size, size_value.cast());
        CFRelease(size_value.cast());

        pos_result == K_AX_ERROR_SUCCESS && size_result == K_AX_ERROR_SUCCESS
    }
}

/// Releases AX elements after animation completes.
fn cleanup_ax_elements(animatable: &[(usize, *mut c_void)]) {
    for &(_, ax) in animatable {
        if !ax.is_null() {
            unsafe { CFRelease(ax.cast()) };
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_animation_config_default() {
        let config = AnimationConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.duration, Duration::from_millis(200));
    }

    #[test]
    fn test_animation_config_calculate_duration() {
        let config = AnimationConfig {
            enabled: true,
            duration: Duration::from_millis(200),
            easing: EasingType::EaseOut,
        };

        // Small distance gets minimum duration
        let small_duration = config.calculate_duration(10.0);
        assert_eq!(small_duration, Duration::from_millis(MIN_DYNAMIC_DURATION_MS));

        // Large distance gets full duration
        let large_duration = config.calculate_duration(500.0);
        assert_eq!(large_duration, config.duration);

        // Medium distance gets something in between
        let mid_duration = config.calculate_duration(250.0);
        assert!(mid_duration > Duration::from_millis(MIN_DYNAMIC_DURATION_MS));
        assert!(mid_duration < config.duration);
    }

    #[test]
    fn test_animation_system_new() {
        let system = AnimationSystem::new();
        assert!(!system.is_enabled());
        assert_eq!(system.duration(), Duration::from_millis(200));
    }

    #[test]
    fn test_animation_system_empty_transitions() {
        let system = AnimationSystem::new();
        let count = system.animate(vec![]);
        assert_eq!(count, 0);
    }
}
