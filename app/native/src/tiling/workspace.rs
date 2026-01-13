//! Workspace management for the tiling window manager.
//!
//! This module provides functionality for switching between workspaces,
//! assigning windows to workspaces, and managing workspace visibility.
//!
//! # Workspace Switching
//!
//! When switching workspaces, windows in the old workspace are hidden using
//! `NSRunningApplication.hide()`, and windows in the new workspace are shown
//! using `NSRunningApplication.unhide()`. This provides a clean transition
//! without moving windows off-screen.
//!
//! # Window Assignment
//!
//! Windows are assigned to workspaces based on matching rules defined in the
//! configuration. If no rule matches, windows go to the currently focused
//! workspace.

use std::collections::HashMap;

use super::constants::window_size::{
    MAX_PANEL_HEIGHT, MAX_PANEL_WIDTH, MIN_TRACKABLE_SIZE, MIN_UNTITLED_WINDOW_SIZE,
};
use super::rules::{find_matching_workspace, matches_window};
use super::state::{TrackedWindow, Workspace};
use super::window::{WindowInfo, hide_app, unhide_app};
use crate::config::{WindowRule, WorkspaceConfig, get_config};

// ============================================================================
// Workspace Operations
// ============================================================================

/// Result of a workspace switch operation.
#[derive(Debug, Clone)]
pub struct WorkspaceSwitchResult {
    /// Name of the previous workspace.
    pub previous: Option<String>,
    /// Name of the new workspace.
    pub current: String,
    /// Number of windows hidden.
    pub windows_hidden: usize,
    /// Number of windows shown.
    pub windows_shown: usize,
    /// PIDs that failed to hide.
    pub hide_failures: Vec<i32>,
    /// PIDs that failed to show.
    pub show_failures: Vec<i32>,
}

/// Hides all windows belonging to a workspace.
///
/// Uses `NSRunningApplication.hide()` to hide applications. If multiple
/// windows from the same app are in the workspace, only one hide call
/// is made per app.
///
/// # Arguments
///
/// * `windows` - The windows to hide
///
/// # Returns
///
/// A tuple of (`success_count`, `failed_pids`)
pub fn hide_workspace_windows(windows: &[&TrackedWindow]) -> (usize, Vec<i32>) {
    let mut hidden = 0;
    let mut failures = Vec::new();

    // Group by PID to avoid hiding the same app multiple times
    let mut pids_to_hide: Vec<i32> = windows.iter().map(|w| w.pid).collect();
    pids_to_hide.sort_unstable();
    pids_to_hide.dedup();

    for pid in pids_to_hide {
        if hide_app(pid).is_ok() {
            hidden += windows.iter().filter(|w| w.pid == pid).count();
        } else {
            failures.push(pid);
        }
    }

    (hidden, failures)
}

/// Shows all windows belonging to a workspace.
///
/// Uses `NSRunningApplication.unhide()` to show applications.
///
/// # Arguments
///
/// * `windows` - The windows to show
///
/// # Returns
///
/// A tuple of (`success_count`, `failed_pids`)
pub fn show_workspace_windows(windows: &[&TrackedWindow]) -> (usize, Vec<i32>) {
    let mut shown = 0;
    let mut failures = Vec::new();

    // Group by PID to avoid unhiding the same app multiple times
    let mut pids_to_show: Vec<i32> = windows.iter().map(|w| w.pid).collect();
    pids_to_show.sort_unstable();
    pids_to_show.dedup();

    for pid in pids_to_show {
        if unhide_app(pid).is_ok() {
            shown += windows.iter().filter(|w| w.pid == pid).count();
        } else {
            failures.push(pid);
        }
    }

    (shown, failures)
}

// ============================================================================
// Window Assignment
// ============================================================================

/// Result of assigning a window to a workspace.
#[derive(Debug, Clone)]
pub struct WindowAssignment {
    /// The workspace the window was assigned to.
    pub workspace_name: String,
    /// Whether the assignment was based on a matching rule.
    pub matched_rule: bool,
    /// Index of the rule that matched (if any).
    pub rule_index: Option<usize>,
}

/// Finds the appropriate workspace for a window based on rules.
///
/// Checks all workspace rules in order. If no rule matches, returns the
/// fallback workspace name.
///
/// # Arguments
///
/// * `window` - The window to assign
/// * `workspace_configs` - Workspace configurations with rules
/// * `fallback` - Name of the workspace to use if no rules match
///
/// # Returns
///
/// The assignment result including which workspace and whether it matched a rule.
pub fn assign_window_to_workspace(
    window: &WindowInfo,
    workspace_configs: &[WorkspaceConfig],
    fallback: &str,
) -> WindowAssignment {
    // Build iterator of (workspace_name, rules) pairs
    let workspaces = workspace_configs.iter().map(|ws| (ws.name.as_str(), ws.rules.as_slice()));

    if let Some(match_result) = find_matching_workspace(window, workspaces) {
        WindowAssignment {
            workspace_name: match_result.workspace_name,
            matched_rule: true,
            rule_index: Some(match_result.rule_index),
        }
    } else {
        WindowAssignment {
            workspace_name: fallback.to_string(),
            matched_rule: false,
            rule_index: None,
        }
    }
}

/// Built-in list of bundle IDs that should always be ignored.
///
/// These are macOS system components, utility apps, and apps that shouldn't
/// be managed by the tiling window manager.
const BUILTIN_IGNORE_BUNDLE_IDS: &[&str] = &[
    // macOS System Components
    "com.apple.dock",                                  // Dock
    "com.apple.SystemUIServer",                        // Menu bar extras
    "com.apple.controlcenter",                         // Control Center
    "com.apple.notificationcenterui",                  // Notification Center
    "com.apple.Spotlight",                             // Spotlight
    "com.apple.WindowManager",                         // Stage Manager
    "com.apple.loginwindow",                           // Login window
    "com.apple.screencaptureui",                       // Screenshot UI
    "com.apple.screensaver",                           // Screen saver
    "com.apple.SecurityAgent",                         // Security dialogs
    "com.apple.UserNotificationCenter",                // User notifications
    "com.apple.universalcontrol",                      // Universal Control
    "com.apple.TouchBarServer",                        // Touch Bar
    "com.apple.AirPlayUIAgent",                        // AirPlay
    "com.apple.wifi.WiFiAgent",                        // WiFi menu
    "com.apple.bluetoothUIServer",                     // Bluetooth menu
    "com.apple.CoreLocationAgent",                     // Location services
    "com.apple.VoiceOver",                             // VoiceOver
    "com.apple.AssistiveControl",                      // Assistive control
    "com.apple.SpeechRecognitionCore",                 // Speech recognition
    "com.apple.accessibility.universalAccessAuthWarn", // Accessibility warnings
    // macOS Apps that shouldn't be tiled
    "com.apple.launchpad.launcher",      // Launchpad
    "com.apple.FolderActionsDispatcher", // Folder Actions
    // Stache itself
    "com.marcosmoura.stache", // This app
    // Window border utilities
    "com.knollsoft.JankyBorders",   // JankyBorders
    "com.linearmouse.JankyBorders", // JankyBorders (alternate bundle)
];

/// Built-in list of app names that should always be ignored.
///
/// Used as a fallback when bundle ID is not available.
const BUILTIN_IGNORE_APP_NAMES: &[&str] = &[
    "Dock",
    "SystemUIServer",
    "Control Center",
    "Notification Center",
    "Spotlight",
    "Window Manager",
    "WindowManager",
    "loginwindow",
    "Screenshot",
    "SecurityAgent",
    "Stache",
    "JankyBorders",
    "borders", // JankyBorders process name
];

/// Checks if a window should be ignored (not managed by tiling).
///
/// This checks both:
/// 1. Built-in ignore list (macOS system windows, Stache, `JankyBorders`, etc.)
/// 2. User-configured ignore rules
///
/// # Arguments
///
/// * `window` - The window to check
/// * `ignore_rules` - User-configured rules for windows to ignore
///
/// # Returns
///
/// `true` if the window should be ignored
pub fn should_ignore_window(window: &WindowInfo, ignore_rules: &[WindowRule]) -> bool {
    // Check built-in ignore list first
    if should_ignore_builtin(window) {
        return true;
    }

    // Check user-configured ignore rules
    ignore_rules.iter().any(|rule| matches_window(rule, window))
}

/// Checks if a window matches the built-in ignore list.
fn should_ignore_builtin(window: &WindowInfo) -> bool {
    // Check bundle ID
    if !window.bundle_id.is_empty()
        && BUILTIN_IGNORE_BUNDLE_IDS
            .iter()
            .any(|&id| window.bundle_id.eq_ignore_ascii_case(id))
    {
        return true;
    }

    // Check app name (fallback when bundle ID is not available)
    if !window.app_name.is_empty()
        && BUILTIN_IGNORE_APP_NAMES
            .iter()
            .any(|&name| window.app_name.eq_ignore_ascii_case(name))
    {
        return true;
    }

    // Check if window is a non-standard/auxiliary window
    is_auxiliary_window(window)
}

/// Checks if a window is likely an auxiliary window (popup, toolbar, dialog, etc.)
/// rather than a "real" application window.
///
/// Filters:
/// - Zero/negative dimensions
/// - Very small windows (< 50×50)
/// - Small panels/dialogs (< 450×200) - like "Find on page" bars
/// - Untitled windows that aren't substantial size (< 800 in either dimension)
/// - Thin bars without titles
fn is_auxiliary_window(window: &WindowInfo) -> bool {
    let width = window.frame.width;
    let height = window.frame.height;
    let has_title = !window.title.is_empty();

    // Zero or negative dimensions - definitely not a real window
    if width <= 0.0 || height <= 0.0 {
        return true;
    }

    // Very small windows (icons, badges, tooltips)
    if width < MIN_TRACKABLE_SIZE && height < MIN_TRACKABLE_SIZE {
        return true;
    }

    // Small panels/dialogs - these are typically find bars, search panels, etc.
    // Examples: "Find on page" (349×106), small dialogs
    if width < MAX_PANEL_WIDTH && height < MAX_PANEL_HEIGHT {
        return true;
    }

    // Untitled windows need to be substantial to be considered "real"
    // Popups, previews, overlays are typically very small in both dimensions.
    // Note: Some apps (like Ghostty, terminals) don't set titles immediately,
    // so we only filter if BOTH dimensions are small.
    if !has_title && width < MIN_UNTITLED_WINDOW_SIZE && height < MIN_UNTITLED_WINDOW_SIZE {
        return true;
    }

    // Thin horizontal bars without titles (toolbars, status bars)
    if !has_title && height < 50.0 && width > height * 10.0 {
        return true;
    }

    // Thin vertical bars without titles (sidebars)
    if !has_title && width < 50.0 && height > width * 10.0 {
        return true;
    }

    false
}

// ============================================================================
// Focus Tracking
// ============================================================================

/// Tracks focus history for workspaces.
///
/// Remembers which window was focused in each workspace so focus can be
/// restored when switching back.
#[derive(Debug, Clone, Default)]
pub struct FocusHistory {
    /// Map of workspace name to the last focused window ID.
    history: HashMap<String, u32>,
}

impl FocusHistory {
    /// Creates a new focus history.
    #[must_use]
    pub fn new() -> Self { Self::default() }

    /// Records the focused window for a workspace.
    pub fn record(&mut self, workspace_name: &str, window_id: u32) {
        self.history.insert(workspace_name.to_string(), window_id);
    }

    /// Gets the last focused window for a workspace.
    #[must_use]
    pub fn get(&self, workspace_name: &str) -> Option<u32> {
        self.history.get(workspace_name).copied()
    }

    /// Clears the focus history for a workspace.
    pub fn clear(&mut self, workspace_name: &str) { self.history.remove(workspace_name); }

    /// Removes a window from all workspace histories.
    ///
    /// Call this when a window is closed.
    pub fn remove_window(&mut self, window_id: u32) {
        self.history.retain(|_, &mut id| id != window_id);
    }

    /// Swaps an old window ID with a new one in the history.
    ///
    /// Used for native tab handling where the representative window ID changes.
    pub fn swap_window_id(&mut self, old_id: u32, new_id: u32) {
        for id in self.history.values_mut() {
            if *id == old_id {
                *id = new_id;
            }
        }
    }
}

// ============================================================================
// Workspace Utilities
// ============================================================================

/// Finds which workspace contains a window.
///
/// # Arguments
///
/// * `window_id` - The window ID to find
/// * `workspaces` - All workspaces to search
///
/// # Returns
///
/// The workspace containing the window, if found.
pub fn find_workspace_for_window(window_id: u32, workspaces: &[Workspace]) -> Option<&Workspace> {
    workspaces.iter().find(|ws| ws.window_ids.contains(&window_id))
}

/// Gets the visible workspace for a screen.
///
/// # Arguments
///
/// * `screen_id` - The screen to check
/// * `workspaces` - All workspaces
///
/// # Returns
///
/// The visible workspace on that screen, if any.
pub fn get_visible_workspace_for_screen(
    screen_id: u32,
    workspaces: &[Workspace],
) -> Option<&Workspace> {
    workspaces.iter().find(|ws| ws.screen_id == screen_id && ws.is_visible)
}

/// Gets all workspaces for a screen.
///
/// # Arguments
///
/// * `screen_id` - The screen to check
/// * `workspaces` - All workspaces
///
/// # Returns
///
/// Workspaces assigned to that screen.
pub fn get_workspaces_for_screen(screen_id: u32, workspaces: &[Workspace]) -> Vec<&Workspace> {
    workspaces.iter().filter(|ws| ws.screen_id == screen_id).collect()
}

/// Validates that a workspace name exists.
///
/// # Arguments
///
/// * `name` - The workspace name to check
/// * `workspaces` - All workspaces
///
/// # Returns
///
/// `true` if a workspace with that name exists.
pub fn workspace_exists(name: &str, workspaces: &[Workspace]) -> bool {
    workspaces.iter().any(|ws| ws.name.eq_ignore_ascii_case(name))
}

// ============================================================================
// Config Helpers
// ============================================================================

/// Gets workspace configs from the global configuration.
#[must_use]
pub fn get_workspace_configs() -> Vec<WorkspaceConfig> { get_config().tiling.workspaces.clone() }

/// Gets ignore rules from the global configuration.
#[must_use]
pub fn get_ignore_rules() -> Vec<WindowRule> { get_config().tiling.ignore.clone() }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiling::state::Rect;

    fn make_window(bundle_id: &str, app_name: &str, title: &str) -> WindowInfo {
        WindowInfo::new(
            1,
            1234,
            bundle_id.to_string(),
            app_name.to_string(),
            title.to_string(),
            Rect::new(0.0, 0.0, 800.0, 600.0),
            false,
            false,
            true,
            false,
        )
    }

    fn make_rule(app_id: Option<&str>, app_name: Option<&str>, title: Option<&str>) -> WindowRule {
        WindowRule {
            app_id: app_id.map(String::from),
            app_name: app_name.map(String::from),
            title: title.map(String::from),
        }
    }

    fn make_workspace_config(name: &str, rules: Vec<WindowRule>) -> WorkspaceConfig {
        WorkspaceConfig {
            name: name.to_string(),
            layout: crate::config::LayoutType::default(),
            screen: "main".to_string(),
            rules,
            preset_on_open: None,
        }
    }

    // ========================================================================
    // Window Assignment Tests
    // ========================================================================

    #[test]
    fn test_assign_window_no_rules() {
        let window = make_window("com.apple.finder", "Finder", "Documents");
        let configs: Vec<WorkspaceConfig> = vec![];

        let result = assign_window_to_workspace(&window, &configs, "default");

        assert_eq!(result.workspace_name, "default");
        assert!(!result.matched_rule);
        assert!(result.rule_index.is_none());
    }

    #[test]
    fn test_assign_window_matching_rule() {
        let window = make_window("com.apple.safari", "Safari", "Google");

        let configs = vec![
            make_workspace_config("browser", vec![make_rule(Some("com.apple.safari"), None, None)]),
            make_workspace_config("code", vec![make_rule(
                Some("com.microsoft.vscode"),
                None,
                None,
            )]),
        ];

        let result = assign_window_to_workspace(&window, &configs, "default");

        assert_eq!(result.workspace_name, "browser");
        assert!(result.matched_rule);
        assert_eq!(result.rule_index, Some(0));
    }

    #[test]
    fn test_assign_window_no_matching_rule() {
        let window = make_window("com.apple.finder", "Finder", "Documents");

        let configs = vec![
            make_workspace_config("browser", vec![make_rule(Some("com.apple.safari"), None, None)]),
            make_workspace_config("code", vec![make_rule(
                Some("com.microsoft.vscode"),
                None,
                None,
            )]),
        ];

        let result = assign_window_to_workspace(&window, &configs, "general");

        assert_eq!(result.workspace_name, "general");
        assert!(!result.matched_rule);
    }

    #[test]
    fn test_assign_window_multiple_rules_in_workspace() {
        let window = make_window("com.google.chrome", "Chrome", "GitHub");

        let configs = vec![make_workspace_config("browser", vec![
            make_rule(Some("com.apple.safari"), None, None),
            make_rule(Some("com.google.chrome"), None, None),
            make_rule(Some("org.mozilla.firefox"), None, None),
        ])];

        let result = assign_window_to_workspace(&window, &configs, "default");

        assert_eq!(result.workspace_name, "browser");
        assert!(result.matched_rule);
        assert_eq!(result.rule_index, Some(1)); // Chrome is the second rule
    }

    // ========================================================================
    // Ignore Rules Tests
    // ========================================================================

    #[test]
    fn test_should_ignore_window_no_rules() {
        let window = make_window("com.apple.finder", "Finder", "Documents");
        let rules: Vec<WindowRule> = vec![];

        assert!(!should_ignore_window(&window, &rules));
    }

    #[test]
    fn test_should_ignore_window_matching_rule() {
        let window = make_window("com.apple.systempreferences", "System Settings", "General");

        let rules = vec![
            make_rule(Some("com.apple.systempreferences"), None, None),
            make_rule(None, Some("Finder"), None),
        ];

        assert!(should_ignore_window(&window, &rules));
    }

    #[test]
    fn test_should_ignore_window_no_match() {
        let window = make_window("com.apple.safari", "Safari", "Google");

        let rules = vec![
            make_rule(Some("com.apple.systempreferences"), None, None),
            make_rule(None, Some("Finder"), None),
        ];

        assert!(!should_ignore_window(&window, &rules));
    }

    // ========================================================================
    // Focus History Tests
    // ========================================================================

    #[test]
    fn test_focus_history_new() {
        let history = FocusHistory::new();
        assert!(history.get("workspace1").is_none());
    }

    #[test]
    fn test_focus_history_record_and_get() {
        let mut history = FocusHistory::new();

        history.record("coding", 123);
        history.record("browser", 456);

        assert_eq!(history.get("coding"), Some(123));
        assert_eq!(history.get("browser"), Some(456));
        assert!(history.get("other").is_none());
    }

    #[test]
    fn test_focus_history_overwrite() {
        let mut history = FocusHistory::new();

        history.record("coding", 123);
        history.record("coding", 789);

        assert_eq!(history.get("coding"), Some(789));
    }

    #[test]
    fn test_focus_history_clear() {
        let mut history = FocusHistory::new();

        history.record("coding", 123);
        history.record("browser", 456);

        history.clear("coding");

        assert!(history.get("coding").is_none());
        assert_eq!(history.get("browser"), Some(456));
    }

    #[test]
    fn test_focus_history_remove_window() {
        let mut history = FocusHistory::new();

        history.record("coding", 123);
        history.record("browser", 123); // Same window in multiple workspaces
        history.record("other", 456);

        history.remove_window(123);

        assert!(history.get("coding").is_none());
        assert!(history.get("browser").is_none());
        assert_eq!(history.get("other"), Some(456));
    }

    // ========================================================================
    // Workspace Utility Tests
    // ========================================================================

    #[test]
    fn test_find_workspace_for_window() {
        use crate::config::LayoutType;

        let mut ws1 = Workspace::new("coding".to_string(), 1, LayoutType::Dwindle);
        ws1.window_ids = vec![100, 101, 102];

        let mut ws2 = Workspace::new("browser".to_string(), 1, LayoutType::Monocle);
        ws2.window_ids = vec![200, 201];

        let workspaces = vec![ws1, ws2];

        assert_eq!(
            find_workspace_for_window(101, &workspaces).map(|w| &w.name),
            Some(&"coding".to_string())
        );
        assert_eq!(
            find_workspace_for_window(200, &workspaces).map(|w| &w.name),
            Some(&"browser".to_string())
        );
        assert!(find_workspace_for_window(999, &workspaces).is_none());
    }

    #[test]
    fn test_get_visible_workspace_for_screen() {
        use crate::config::LayoutType;

        let mut ws1 = Workspace::new("coding".to_string(), 1, LayoutType::Dwindle);
        ws1.is_visible = true;

        let mut ws2 = Workspace::new("browser".to_string(), 1, LayoutType::Monocle);
        ws2.is_visible = false;

        let mut ws3 = Workspace::new("other".to_string(), 2, LayoutType::Floating);
        ws3.is_visible = true;

        let workspaces = vec![ws1, ws2, ws3];

        let visible = get_visible_workspace_for_screen(1, &workspaces);
        assert_eq!(visible.map(|w| &w.name), Some(&"coding".to_string()));

        let visible = get_visible_workspace_for_screen(2, &workspaces);
        assert_eq!(visible.map(|w| &w.name), Some(&"other".to_string()));

        assert!(get_visible_workspace_for_screen(99, &workspaces).is_none());
    }

    #[test]
    fn test_get_workspaces_for_screen() {
        use crate::config::LayoutType;

        let ws1 = Workspace::new("ws1".to_string(), 1, LayoutType::Dwindle);
        let ws2 = Workspace::new("ws2".to_string(), 1, LayoutType::Monocle);
        let ws3 = Workspace::new("ws3".to_string(), 2, LayoutType::Floating);

        let workspaces = vec![ws1, ws2, ws3];

        let screen1 = get_workspaces_for_screen(1, &workspaces);
        assert_eq!(screen1.len(), 2);

        let screen2 = get_workspaces_for_screen(2, &workspaces);
        assert_eq!(screen2.len(), 1);
    }

    #[test]
    fn test_workspace_exists() {
        use crate::config::LayoutType;

        let workspaces = vec![
            Workspace::new("coding".to_string(), 1, LayoutType::Dwindle),
            Workspace::new("browser".to_string(), 1, LayoutType::Monocle),
        ];

        assert!(workspace_exists("coding", &workspaces));
        assert!(workspace_exists("Coding", &workspaces)); // Case insensitive
        assert!(workspace_exists("BROWSER", &workspaces));
        assert!(!workspace_exists("unknown", &workspaces));
    }
}
