//! Border manager for the tiling window manager.
//!
//! This module provides `BorderManager`, a singleton that tracks border state
//! for all managed windows and coordinates with `JankyBorders` for rendering.
//!
//! # Architecture
//!
//! The border manager tracks which windows should have borders and their current
//! state (focused, unfocused, monocle, floating). It delegates actual border
//! rendering to `JankyBorders`, sending configuration updates when state changes.
//!
//! # Thread Safety
//!
//! The manager uses `parking_lot::RwLock` for thread-safe access. All public
//! methods acquire the appropriate lock before modifying state.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;

use super::janky;
use crate::config::{BordersConfig, WindowRule, get_config};
use crate::tiling::rules::matches_window;
use crate::tiling::window::WindowInfo;

// ============================================================================
// Global Singleton
// ============================================================================

/// Global border manager instance.
static BORDER_MANAGER: OnceLock<Arc<RwLock<BorderManager>>> = OnceLock::new();

/// Gets the global border manager instance.
///
/// Returns `None` if the manager hasn't been initialized yet.
#[must_use]
pub fn get_border_manager() -> Option<Arc<RwLock<BorderManager>>> { BORDER_MANAGER.get().cloned() }

/// Initializes the global border manager.
///
/// This should be called once during application startup, after the tiling
/// manager has been initialized.
///
/// # Returns
///
/// `true` if initialization succeeded, `false` if already initialized or disabled.
pub fn init_border_manager() -> bool {
    if BORDER_MANAGER.get().is_some() {
        return false;
    }

    let config = get_config();
    if !config.tiling.borders.is_enabled() {
        return false;
    }

    let manager = BorderManager::new(&config.tiling.borders);

    if BORDER_MANAGER.set(Arc::new(RwLock::new(manager))).is_err() {
        eprintln!("stache: borders: manager already initialized");
        return false;
    }

    // Apply initial configuration to JankyBorders
    janky::apply_config(&config.tiling.borders);

    eprintln!("stache: borders: manager initialized");

    true
}

// ============================================================================
// Border State
// ============================================================================

/// State of a window for border color selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum BorderState {
    /// Window is currently focused.
    Focused,
    /// Window is visible but not focused.
    #[default]
    Unfocused,
    /// Window is in monocle layout (maximized).
    Monocle,
    /// Window is floating (not tiled).
    Floating,
}

// ============================================================================
// Border Info
// ============================================================================

/// Information about a tracked window's border state.
struct BorderInfo {
    /// Current state of the border.
    state: BorderState,
    /// The workspace this window belongs to.
    workspace: String,
    /// Whether the border should be visible (based on workspace visibility).
    visible: bool,
}

// ============================================================================
// Border Manager
// ============================================================================

/// Manages border state for all tracked windows.
///
/// The border manager is responsible for:
/// - Tracking which windows should have borders
/// - Tracking border state (focused, unfocused, monocle, floating)
/// - Updating `JankyBorders` when state changes
#[derive(Default)]
pub struct BorderManager {
    /// Map of window IDs to their border info.
    borders: HashMap<u32, BorderInfo>,
    /// Whether the border system is enabled.
    enabled: bool,
    /// Additional ignore rules for borders (beyond global tiling ignore).
    ignore_rules: Vec<WindowRule>,
    /// Current layout state for color updates.
    current_is_monocle: bool,
    /// Current floating state for color updates.
    current_is_floating: bool,
}

impl BorderManager {
    /// Creates a new border manager from configuration.
    #[must_use]
    pub fn new(config: &BordersConfig) -> Self {
        Self {
            borders: HashMap::new(),
            enabled: config.enabled,
            ignore_rules: config.ignore.clone(),
            current_is_monocle: false,
            current_is_floating: false,
        }
    }

    /// Returns whether the border system is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }

    /// Sets whether borders are enabled.
    ///
    /// When disabled, `JankyBorders` colors are not updated.
    /// When re-enabled, configuration is re-applied.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }

        self.enabled = enabled;

        if enabled {
            // Re-apply configuration when re-enabled
            janky::refresh();
        }

        eprintln!(
            "stache: borders: {}",
            if enabled { "enabled" } else { "disabled" }
        );
    }

    /// Checks if a window should have a border.
    ///
    /// A window should NOT have a border if it matches any ignore rule.
    #[must_use]
    pub fn should_have_border(&self, window: &WindowInfo) -> bool {
        if !self.enabled {
            return false;
        }

        // Check border-specific ignore rules
        !self.ignore_rules.iter().any(|rule| matches_window(rule, window))
    }

    /// Registers a window for border tracking.
    ///
    /// # Arguments
    ///
    /// * `window_id` - The ID of the window to track.
    /// * `state` - The initial border state.
    /// * `workspace` - The workspace the window belongs to.
    /// * `visible` - Whether the window's workspace is currently visible.
    ///
    /// # Returns
    ///
    /// `true` if the window was registered, `false` if already tracked or disabled.
    pub fn track_window(
        &mut self,
        window_id: u32,
        state: BorderState,
        workspace: &str,
        visible: bool,
    ) -> bool {
        if !self.enabled {
            return false;
        }

        // Don't track duplicates
        if self.borders.contains_key(&window_id) {
            return false;
        }

        self.borders.insert(window_id, BorderInfo {
            state,
            workspace: workspace.to_string(),
            visible,
        });

        // Update JankyBorders colors if this is a focused window
        if state == BorderState::Focused && visible {
            self.update_janky_colors();
        }

        true
    }

    /// Unregisters a window from border tracking.
    ///
    /// # Arguments
    ///
    /// * `window_id` - The ID of the window to untrack.
    ///
    /// # Returns
    ///
    /// `true` if the window was untracked, `false` if not found.
    pub fn untrack_window(&mut self, window_id: u32) -> bool {
        self.borders.remove(&window_id).is_some()
    }

    /// Updates the state of a tracked window.
    ///
    /// This updates the internal state and may trigger `JankyBorders` color updates.
    ///
    /// # Arguments
    ///
    /// * `window_id` - The ID of the window.
    /// * `state` - The new border state.
    pub fn update_window_state(&mut self, window_id: u32, state: BorderState) {
        if let Some(info) = self.borders.get_mut(&window_id) {
            let state_changed = info.state != state;
            if state_changed {
                eprintln!(
                    "stache: borders: window {} state: {:?} -> {:?}",
                    window_id, info.state, state
                );
                info.state = state;

                // Update JankyBorders colors if this is a visible window becoming focused/active
                // We need to update for Focused, Monocle, and Floating states
                if info.visible {
                    match state {
                        BorderState::Focused | BorderState::Monocle | BorderState::Floating => {
                            // Update layout state flags based on the new state
                            let is_monocle = state == BorderState::Monocle;
                            let is_floating = state == BorderState::Floating;

                            // Only update if layout state actually changed OR this is a focus change
                            if self.current_is_monocle != is_monocle
                                || self.current_is_floating != is_floating
                            {
                                self.current_is_monocle = is_monocle;
                                self.current_is_floating = is_floating;
                            }

                            // Always update colors on focus change
                            eprintln!(
                                "stache: borders: triggering color update (monocle={is_monocle}, floating={is_floating})"
                            );
                            janky::update_colors_for_state(is_monocle, is_floating);
                        }
                        BorderState::Unfocused => {
                            // No color update needed for unfocused - JankyBorders handles this
                            eprintln!("stache: borders: window {window_id} now unfocused");
                        }
                    }
                }
            }
        }
    }

    /// Updates the workspace of a tracked window.
    ///
    /// # Arguments
    ///
    /// * `window_id` - The ID of the window.
    /// * `workspace` - The new workspace name.
    pub fn update_window_workspace(&mut self, window_id: u32, workspace: &str) {
        if let Some(info) = self.borders.get_mut(&window_id) {
            info.workspace = workspace.to_string();
        }
    }

    /// Marks windows in a workspace as visible.
    ///
    /// # Arguments
    ///
    /// * `workspace` - The name of the workspace.
    pub fn show_workspace(&mut self, workspace: &str) {
        for info in self.borders.values_mut() {
            if info.workspace == workspace {
                info.visible = true;
            }
        }
    }

    /// Marks windows in a workspace as hidden.
    ///
    /// # Arguments
    ///
    /// * `workspace` - The name of the workspace.
    pub fn hide_workspace(&mut self, workspace: &str) {
        for info in self.borders.values_mut() {
            if info.workspace == workspace {
                info.visible = false;
            }
        }
    }

    /// Updates the layout state and refreshes `JankyBorders` colors.
    ///
    /// Call this when the layout changes to monocle or floating.
    ///
    /// # Arguments
    ///
    /// * `is_monocle` - Whether the current layout is monocle.
    /// * `is_floating` - Whether the current layout is floating.
    pub fn set_layout_state(&mut self, is_monocle: bool, is_floating: bool) {
        if self.current_is_monocle != is_monocle || self.current_is_floating != is_floating {
            self.current_is_monocle = is_monocle;
            self.current_is_floating = is_floating;
            self.update_janky_colors();
        }
    }

    /// Sets all visible borders to unfocused state.
    ///
    /// Called when focus moves outside the tiling system (to an untracked window).
    pub fn set_all_unfocused(&mut self) {
        for info in self.borders.values_mut() {
            if info.visible && info.state == BorderState::Focused {
                info.state = BorderState::Unfocused;
            }
        }
        // JankyBorders will use inactive_color for unfocused windows automatically
    }

    /// Refreshes `JankyBorders` configuration.
    ///
    /// Call this when the border configuration changes.
    pub fn refresh(&mut self) {
        if self.enabled {
            janky::refresh();
        }
    }

    /// Returns the number of tracked windows.
    #[must_use]
    pub fn window_count(&self) -> usize { self.borders.len() }

    /// Returns the border state for a window.
    #[must_use]
    pub fn get_window_state(&self, window_id: u32) -> Option<BorderState> {
        self.borders.get(&window_id).map(|info| info.state)
    }

    /// Checks if a window is being tracked.
    #[must_use]
    pub fn is_tracked(&self, window_id: u32) -> bool { self.borders.contains_key(&window_id) }

    /// Updates `JankyBorders` colors based on current state.
    fn update_janky_colors(&self) {
        if self.enabled {
            janky::update_colors_for_state(self.current_is_monocle, self.current_is_floating);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::state::Rect;

    #[test]
    fn test_border_manager_default() {
        let manager = BorderManager::default();
        assert!(!manager.is_enabled());
        assert_eq!(manager.window_count(), 0);
    }

    #[test]
    fn test_border_manager_from_config() {
        let config = BordersConfig {
            enabled: true,
            ..Default::default()
        };
        let manager = BorderManager::new(&config);
        assert!(manager.is_enabled());
    }

    #[test]
    fn test_set_enabled() {
        let mut manager = BorderManager::default();
        assert!(!manager.is_enabled());

        manager.enabled = true; // Bypass JankyBorders call for test
        assert!(manager.is_enabled());

        manager.enabled = false;
        assert!(!manager.is_enabled());
    }

    #[test]
    fn test_track_window() {
        let config = BordersConfig {
            enabled: true,
            ..Default::default()
        };
        let mut manager = BorderManager::new(&config);

        assert!(manager.track_window(123, BorderState::Focused, "workspace1", true));
        assert!(manager.is_tracked(123));
        assert_eq!(manager.window_count(), 1);

        // Can't track duplicate
        assert!(!manager.track_window(123, BorderState::Unfocused, "workspace1", true));
    }

    #[test]
    fn test_untrack_window() {
        let config = BordersConfig {
            enabled: true,
            ..Default::default()
        };
        let mut manager = BorderManager::new(&config);

        manager.track_window(123, BorderState::Focused, "workspace1", true);
        assert!(manager.untrack_window(123));
        assert!(!manager.is_tracked(123));

        // Can't untrack non-existent
        assert!(!manager.untrack_window(123));
    }

    #[test]
    fn test_get_window_state() {
        let config = BordersConfig {
            enabled: true,
            ..Default::default()
        };
        let mut manager = BorderManager::new(&config);

        manager.track_window(123, BorderState::Monocle, "workspace1", true);
        assert_eq!(manager.get_window_state(123), Some(BorderState::Monocle));
        assert_eq!(manager.get_window_state(999), None);
    }

    #[test]
    fn test_update_window_state() {
        let config = BordersConfig {
            enabled: true,
            ..Default::default()
        };
        let mut manager = BorderManager::new(&config);

        manager.track_window(123, BorderState::Unfocused, "workspace1", true);
        manager.update_window_state(123, BorderState::Focused);
        assert_eq!(manager.get_window_state(123), Some(BorderState::Focused));
    }

    #[test]
    fn test_should_have_border_when_disabled() {
        let manager = BorderManager::default();
        let window = WindowInfo::new_for_test_with_app(
            1,
            1,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            "com.test.app",
            "TestApp",
        );
        assert!(!manager.should_have_border(&window));
    }

    #[test]
    fn test_should_have_border_with_ignore_rule() {
        let config = BordersConfig {
            enabled: true,
            ignore: vec![WindowRule {
                app_id: Some("com.test.ignored".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let manager = BorderManager::new(&config);

        let ignored_window = WindowInfo::new_for_test_with_app(
            1,
            1,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            "com.test.ignored",
            "IgnoredApp",
        );
        assert!(!manager.should_have_border(&ignored_window));

        let normal_window = WindowInfo::new_for_test_with_app(
            2,
            2,
            Rect::new(0.0, 0.0, 100.0, 100.0),
            "com.test.normal",
            "NormalApp",
        );
        assert!(manager.should_have_border(&normal_window));
    }

    #[test]
    fn test_border_state_default() {
        assert_eq!(BorderState::default(), BorderState::Unfocused);
    }

    #[test]
    fn test_border_state_equality() {
        assert_eq!(BorderState::Focused, BorderState::Focused);
        assert_ne!(BorderState::Focused, BorderState::Unfocused);
        assert_ne!(BorderState::Monocle, BorderState::Floating);
    }
}
