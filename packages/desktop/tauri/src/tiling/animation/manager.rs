//! Animation manager for coordinating window animations.
//!
//! This module manages the lifecycle of window animations using macOS
//! `CVDisplayLink` for display-synced updates, ensuring smooth 60Hz/120Hz
//! animations without jank.

#![allow(clippy::significant_drop_tightening)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use barba_shared::{AnimationConfig, AnimationSettings};
use parking_lot::Mutex;

use super::display_link::DisplayLink;
use super::easing::{EasingType, apply_easing, lerp_i32, lerp_u32};
use crate::tiling::observer::mark_layout_applied;
use crate::tiling::state::WindowFrame;
use crate::tiling::window;

/// Animation state for a single window.
#[derive(Debug, Clone)]
struct WindowAnimation {
    /// Starting frame of the animation.
    start_frame: WindowFrame,

    /// Target frame of the animation.
    target_frame: WindowFrame,

    /// When the animation started.
    start_time: Instant,

    /// Duration of the animation in milliseconds.
    duration_ms: u32,

    /// Easing function to use.
    easing: EasingType,

    /// Whether this animation has been cancelled.
    cancelled: bool,
}

impl WindowAnimation {
    /// Creates a new window animation.
    fn new(
        start_frame: WindowFrame,
        target_frame: WindowFrame,
        duration_ms: u32,
        easing: EasingType,
    ) -> Self {
        Self {
            start_frame,
            target_frame,
            start_time: Instant::now(),
            duration_ms,
            easing,
            cancelled: false,
        }
    }

    /// Returns the progress of the animation from 0.0 to 1.0.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn progress(&self) -> f64 {
        if self.duration_ms == 0 {
            return 1.0;
        }

        let elapsed = self.start_time.elapsed().as_millis() as f64;
        let duration = f64::from(self.duration_ms);
        (elapsed / duration).min(1.0)
    }

    /// Returns whether the animation is complete.
    #[must_use]
    pub fn is_complete(&self) -> bool { self.cancelled || self.progress() >= 1.0 }

    /// Calculates the current frame based on animation progress.
    #[must_use]
    pub fn current_frame(&self) -> WindowFrame {
        let progress = self.progress();
        let eased = apply_easing(self.easing, progress);

        WindowFrame {
            x: lerp_i32(self.start_frame.x, self.target_frame.x, eased),
            y: lerp_i32(self.start_frame.y, self.target_frame.y, eased),
            width: lerp_u32(self.start_frame.width, self.target_frame.width, eased),
            height: lerp_u32(self.start_frame.height, self.target_frame.height, eased),
        }
    }
}

/// Shared state for animations, accessible from display link callback.
struct AnimationState {
    /// Active animations by window ID.
    animations: HashMap<u64, WindowAnimation>,
    /// Whether animations should stop.
    should_stop: bool,
}

impl AnimationState {
    fn new() -> Self {
        Self {
            animations: HashMap::new(),
            should_stop: false,
        }
    }
}

/// Manages all active window animations using `CVDisplayLink`.
pub struct AnimationManager {
    /// Current animation configuration.
    config: AnimationConfig,

    /// Shared animation state.
    state: Arc<Mutex<AnimationState>>,

    /// The display link for frame callbacks.
    display_link: Option<DisplayLink>,

    /// Whether the animation loop is running.
    running: Arc<AtomicBool>,
}

impl AnimationManager {
    /// Creates a new animation manager.
    #[must_use]
    pub fn new(config: AnimationConfig) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(AnimationState::new())),
            display_link: DisplayLink::new(),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns whether animations are enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.config.is_enabled() }

    /// Returns the current animation settings.
    #[must_use]
    pub fn settings(&self) -> AnimationSettings { self.config.settings() }

    /// Gets the starting frame for a new animation.
    ///
    /// If there's an existing animation for this window, we use its TARGET frame
    /// (where it was going to end up) as the conceptual current position.
    /// This prevents animation "fighting" when layouts change rapidly.
    ///
    /// Otherwise, we query the actual window position from the system.
    fn get_start_frame_for_animation(&self, window_id: u64) -> Option<WindowFrame> {
        let state = self.state.lock();

        // If there's an existing animation, use its target as the "current" position
        // This means the window conceptually is where it was going to end up
        if let Some(anim) = state.animations.get(&window_id) {
            return Some(anim.target_frame);
        }

        // Drop the lock before the potentially blocking window query
        drop(state);

        // Otherwise, get the actual current frame from the window system
        window::get_window_by_id(window_id).ok().map(|w| w.frame)
    }

    /// Animates a window to a target frame.
    pub fn animate_window(&mut self, window_id: u64, target_frame: WindowFrame) {
        if !self.is_enabled() {
            // Animations disabled, apply immediately
            let _ = window::set_window_frame(window_id, &target_frame);
            return;
        }

        let settings = self.settings();
        let easing = super::easing_from_config(&settings.easing);

        // Get the starting frame: use previous animation's target if exists,
        // otherwise query the actual window position.
        let Some(start_frame) = self.get_start_frame_for_animation(window_id) else {
            // Can't get current frame, apply immediately
            let _ = window::set_window_frame(window_id, &target_frame);
            return;
        };

        // Skip animation if already at target
        if start_frame == target_frame {
            return;
        }

        // Create new animation (replaces any existing one)
        let animation = WindowAnimation::new(start_frame, target_frame, settings.duration, easing);

        {
            let mut state = self.state.lock();
            state.animations.insert(window_id, animation);
        }

        // Reset the layout cooldown so observer ignores move events during animation
        mark_layout_applied();

        // Start animation loop if not already running
        self.start_animation_loop();
    }

    /// Animates multiple windows simultaneously.
    pub fn animate_windows(&mut self, targets: Vec<(u64, WindowFrame)>) {
        if !self.is_enabled() {
            // Animations disabled, apply all immediately
            for (window_id, frame) in targets {
                let _ = window::set_window_frame(window_id, &frame);
            }
            return;
        }

        let settings = self.settings();
        let easing = super::easing_from_config(&settings.easing);

        let mut has_animations = false;

        for (window_id, target_frame) in targets {
            // Get the starting frame: use previous animation's target if exists,
            // otherwise query the actual window position.
            let Some(start_frame) = self.get_start_frame_for_animation(window_id) else {
                // Can't get current frame, apply immediately
                let _ = window::set_window_frame(window_id, &target_frame);
                continue;
            };

            // Skip if already at target
            if start_frame == target_frame {
                continue;
            }

            // Create animation
            let animation =
                WindowAnimation::new(start_frame, target_frame, settings.duration, easing);

            {
                let mut state = self.state.lock();
                state.animations.insert(window_id, animation);
            }

            has_animations = true;
        }

        // Start animation loop if we have animations
        if has_animations {
            // Reset the layout cooldown so observer ignores move events during animation
            mark_layout_applied();
            self.start_animation_loop();
        }
    }

    /// Starts the animation loop using `CVDisplayLink`.
    fn start_animation_loop(&mut self) {
        // Don't start if already running
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }

        // Reset stop flag
        {
            let mut state = self.state.lock();
            state.should_stop = false;
        }

        let state = Arc::clone(&self.state);
        let running = Arc::clone(&self.running);

        // Try to use CVDisplayLink for display-synced animations
        if let Some(ref mut display_link) = self.display_link {
            let started =
                display_link.start(move || Self::tick_animations_callback(&state, &running));

            if started {
                return;
            }
        }

        // Fallback: use a timer-based approach if CVDisplayLink fails
        self.tick_animations_fallback();
    }

    /// Callback for display link - updates all animations.
    /// Returns `true` to continue, `false` to stop.
    fn tick_animations_callback(
        state: &Arc<Mutex<AnimationState>>,
        running: &Arc<AtomicBool>,
    ) -> bool {
        let mut state_guard = state.lock();

        if state_guard.should_stop || state_guard.animations.is_empty() {
            running.store(false, Ordering::SeqCst);
            return false;
        }

        let mut completed = Vec::new();

        // Update all animations
        for (window_id, animation) in &state_guard.animations {
            if animation.is_complete() {
                // Ensure we end at exactly the target position
                let _ = window::set_window_frame(*window_id, &animation.target_frame);
                completed.push(*window_id);
            } else {
                // Update to current interpolated position
                let frame = animation.current_frame();
                let _ = window::set_window_frame(*window_id, &frame);
            }
        }

        // Remove completed animations
        for window_id in completed {
            state_guard.animations.remove(&window_id);
        }

        // Continue if there are still animations
        let should_continue = !state_guard.animations.is_empty();

        if !should_continue {
            running.store(false, Ordering::SeqCst);
        }

        should_continue
    }

    /// Fallback animation loop using thread sleep (if `CVDisplayLink` unavailable).
    fn tick_animations_fallback(&self) {
        use std::time::Duration;

        use crate::tiling::screen::get_max_refresh_rate;

        // Get the maximum refresh rate from all connected displays
        // and target that FPS for smooth interpolation
        let max_refresh_rate = get_max_refresh_rate();
        let frame_duration = Duration::from_micros(1_000_000 / u64::from(max_refresh_rate));

        loop {
            let mut state = self.state.lock();

            if state.should_stop || state.animations.is_empty() {
                break;
            }

            let mut completed = Vec::new();

            // Update all animations
            for (window_id, animation) in &state.animations {
                if animation.is_complete() {
                    // Ensure we end at exactly the target position
                    let _ = window::set_window_frame(*window_id, &animation.target_frame);
                    completed.push(*window_id);
                } else {
                    // Update to current interpolated position
                    let frame = animation.current_frame();
                    let _ = window::set_window_frame(*window_id, &frame);
                }
            }

            // Remove completed animations
            for window_id in completed {
                state.animations.remove(&window_id);
            }

            // Check if we should continue
            if state.animations.is_empty() {
                break;
            }

            // Release lock before sleeping
            drop(state);

            // Sleep briefly to avoid busy-waiting
            std::thread::sleep(frame_duration);
        }

        self.running.store(false, Ordering::SeqCst);
    }
}

impl Drop for AnimationManager {
    fn drop(&mut self) {
        // Stop animations on drop
        {
            let mut state = self.state.lock();
            for anim in state.animations.values_mut() {
                anim.cancelled = true;
            }
            state.animations.clear();
            state.should_stop = true;
        }

        if let Some(ref mut display_link) = self.display_link {
            display_link.stop();
        }

        self.running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn create_test_frame(x: i32, y: i32, width: u32, height: u32) -> WindowFrame {
        WindowFrame { x, y, width, height }
    }

    #[test]
    fn test_animation_progress() {
        let start = create_test_frame(0, 0, 100, 100);
        let target = create_test_frame(100, 100, 200, 200);
        let animation = WindowAnimation::new(start, target, 100, EasingType::Linear);

        // Initially at 0
        assert!(animation.progress() < 0.1);

        // After some time, should be higher
        std::thread::sleep(Duration::from_millis(50));
        let progress = animation.progress();
        assert!(progress > 0.3 && progress < 0.8);
    }

    #[test]
    fn test_animation_completion() {
        let start = create_test_frame(0, 0, 100, 100);
        let target = create_test_frame(100, 100, 200, 200);
        let animation = WindowAnimation::new(start, target, 50, EasingType::Linear);

        assert!(!animation.is_complete());

        std::thread::sleep(Duration::from_millis(60));
        assert!(animation.is_complete());
    }

    #[test]
    fn test_animation_current_frame() {
        let start = create_test_frame(0, 0, 100, 100);
        let target = create_test_frame(100, 100, 200, 200);
        let animation = WindowAnimation::new(start, target, 0, EasingType::Linear);

        // With duration 0, should immediately be at target
        let frame = animation.current_frame();
        assert_eq!(frame.x, 100);
        assert_eq!(frame.y, 100);
        assert_eq!(frame.width, 200);
        assert_eq!(frame.height, 200);
    }

    #[test]
    fn test_animation_cancelled() {
        let start = create_test_frame(0, 0, 100, 100);
        let target = create_test_frame(100, 100, 200, 200);
        let mut animation = WindowAnimation::new(start, target, 1000, EasingType::Linear);

        assert!(!animation.is_complete());
        animation.cancelled = true;
        assert!(animation.is_complete());
    }

    #[test]
    fn test_manager_disabled() {
        let manager = AnimationManager::new(AnimationConfig::Enabled(false));
        assert!(!manager.is_enabled());
    }

    #[test]
    fn test_manager_enabled() {
        let manager = AnimationManager::new(AnimationConfig::Enabled(true));
        assert!(manager.is_enabled());
    }

    #[test]
    fn test_manager_with_settings() {
        let settings = AnimationSettings {
            duration: 300,
            easing: barba_shared::EasingFunction::Spring,
        };
        let manager = AnimationManager::new(AnimationConfig::Settings(settings));
        assert!(manager.is_enabled());
        assert_eq!(manager.settings().duration, 300);
    }
}
