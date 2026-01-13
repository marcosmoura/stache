//! Internal constants for tiling window manager tuning.
//!
//! This module centralizes all magic numbers and tuning constants used throughout
//! the tiling system. These values have been calibrated for optimal behavior on macOS.
//!
//! # Organization
//!
//! Constants are grouped by functionality:
//! - `timing` - Debouncing, cooldowns, and delays
//! - `window_size` - Window size thresholds for filtering
//! - `layout` - Layout calculation thresholds
//! - `animation` - Animation system parameters

//!
//! # Modification Guidelines
//!
//! These values are internal tuning parameters. Modify with caution:
//! - Timing values affect responsiveness and correctness
//! - Size thresholds affect which windows are tracked
//! - Animation values affect visual smoothness

/// Timing constants for event handling and debouncing.
pub mod timing {
    /// Duration to ignore focus events after programmatic focus (ms).
    ///
    /// When we programmatically focus a window, macOS sends focus notifications
    /// that we need to ignore to prevent feedback loops.
    pub const FOCUS_COOLDOWN_MS: u128 = 25;

    /// Duration to ignore workspace switch requests after a recent switch (ms).
    ///
    /// Prevents race conditions where focus events during a workspace switch
    /// trigger another switch.
    pub const WORKSPACE_SWITCH_COOLDOWN_MS: u128 = 25;

    /// Delay between hiding old workspace and showing new workspace (ms).
    ///
    /// Gives macOS time to process the hide operation before we start showing
    /// windows, reducing visual glitches.
    pub const HIDE_SHOW_DELAY_MS: u64 = 10;

    /// Screen change debounce delay (ms).
    ///
    /// Allows display configuration to stabilize after hotplug events before
    /// we process the change.
    pub const SCREEN_CHANGE_DELAY_MS: u64 = 100;

    /// Maximum time to wait for new window AX readiness (ms).
    ///
    /// When a window is created, it may take time for the Accessibility API
    /// to return valid data.
    pub const WINDOW_READY_TIMEOUT_MS: u64 = 25;

    /// Polling interval when waiting for window readiness (ms).
    pub const WINDOW_READY_POLL_INTERVAL_MS: u64 = 5;

    /// Event coalescing window (ms).
    ///
    /// Multiple rapid events (move, resize) within this window are coalesced
    /// into a single event to reduce CPU usage during drags.
    pub const EVENT_COALESCE_MS: u64 = 4;
}

/// Window size thresholds for filtering.
pub mod window_size {
    /// Minimum size for a window to be tracked (pixels).
    ///
    /// Windows smaller than this are likely menus, tooltips, or other
    /// transient elements that shouldn't be managed.
    pub const MIN_TRACKABLE_SIZE: f64 = 50.0;

    /// Maximum height for panel-like windows (pixels).
    ///
    /// Used to identify panel windows that should be ignored.
    pub const MAX_PANEL_HEIGHT: f64 = 200.0;

    /// Maximum width for panel-like windows (pixels).
    ///
    /// Used to identify panel windows that should be ignored.
    pub const MAX_PANEL_WIDTH: f64 = 450.0;

    /// Minimum size for untitled windows (pixels).
    ///
    /// Untitled windows smaller than this are likely transient dialogs.
    pub const MIN_UNTITLED_WINDOW_SIZE: f64 = 320.0;
}

/// Layout calculation thresholds.
pub mod layout {
    /// Minimum change in pixels to trigger window repositioning.
    ///
    /// Prevents micro-adjustments that would be imperceptible to users
    /// but waste CPU cycles.
    pub const REPOSITION_THRESHOLD_PX: f64 = 2.0;

    /// Maximum windows for grid layout optimization.
    ///
    /// Beyond this count, grid calculations use a simpler algorithm.
    pub const MAX_GRID_WINDOWS: usize = 12;
}

/// Animation system constants.
pub mod animation {
    /// Default frame rate when display refresh rate cannot be detected.
    pub const DEFAULT_FPS: u32 = 60;

    /// Minimum animation duration (ms).
    ///
    /// Animations shorter than this are effectively instant.
    pub const MIN_DURATION_MS: u32 = 100;

    /// Maximum animation duration (ms).
    ///
    /// Animations longer than this feel sluggish.
    pub const MAX_DURATION_MS: u32 = 1000;

    /// Minimum dynamic duration for distance-based calculations (ms).
    pub const MIN_DYNAMIC_DURATION_MS: u64 = 50;

    /// Threshold for spin-wait vs sleep (microseconds).
    ///
    /// For waits shorter than this, we spin-wait for precision.
    /// For longer waits, we sleep to reduce CPU usage.
    pub const SPIN_WAIT_THRESHOLD_US: u64 = 1000;

    /// Spring convergence threshold (position delta).
    ///
    /// When the spring position delta is less than this, the animation
    /// is considered complete.
    pub const SPRING_POSITION_THRESHOLD: f64 = 0.01;

    /// Vsync wait timeout multiplier.
    ///
    /// How long to wait for vsync as a multiple of the frame duration.
    pub const VSYNC_TIMEOUT_MULTIPLIER: f64 = 2.0;
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timing_constants_are_reasonable() {
        // Cooldowns should be short but non-zero
        assert!(timing::FOCUS_COOLDOWN_MS > 0);
        assert!(timing::FOCUS_COOLDOWN_MS < 100);

        assert!(timing::WORKSPACE_SWITCH_COOLDOWN_MS > 0);
        assert!(timing::WORKSPACE_SWITCH_COOLDOWN_MS < 100);

        // Delays should be short
        assert!(timing::HIDE_SHOW_DELAY_MS < 50);
        assert!(timing::SCREEN_CHANGE_DELAY_MS <= 200);

        // Polling should be faster than timeout
        assert!(timing::WINDOW_READY_POLL_INTERVAL_MS < timing::WINDOW_READY_TIMEOUT_MS);

        // Event coalescing should be close to frame time (4ms ~ 250fps, 8ms ~ 125fps)
        assert!(timing::EVENT_COALESCE_MS >= 2);
        assert!(timing::EVENT_COALESCE_MS <= 16);
    }

    #[test]
    fn test_window_size_constants_are_reasonable() {
        // Minimum trackable should be small but positive
        assert!(window_size::MIN_TRACKABLE_SIZE > 0.0);
        assert!(window_size::MIN_TRACKABLE_SIZE < 100.0);

        // Panel dimensions should be reasonable
        assert!(window_size::MAX_PANEL_HEIGHT > 0.0);
        assert!(window_size::MAX_PANEL_WIDTH > 0.0);

        // Untitled window minimum should be larger than general minimum
        assert!(window_size::MIN_UNTITLED_WINDOW_SIZE > window_size::MIN_TRACKABLE_SIZE);
    }

    #[test]
    fn test_animation_constants_are_reasonable() {
        // Duration range should make sense
        assert!(animation::MIN_DURATION_MS < animation::MAX_DURATION_MS);
        assert!(animation::MIN_DYNAMIC_DURATION_MS < u64::from(animation::MAX_DURATION_MS));

        // FPS should be reasonable
        assert!(animation::DEFAULT_FPS >= 30);
        assert!(animation::DEFAULT_FPS <= 240);

        // Spring threshold should be small but positive
        assert!(animation::SPRING_POSITION_THRESHOLD > 0.0);
        assert!(animation::SPRING_POSITION_THRESHOLD < 1.0);
    }

    #[test]
    fn test_layout_threshold_is_reasonable() {
        // Reposition threshold should be small but non-zero
        assert!(layout::REPOSITION_THRESHOLD_PX > 0.0);
        assert!(layout::REPOSITION_THRESHOLD_PX < 10.0);
    }
}
