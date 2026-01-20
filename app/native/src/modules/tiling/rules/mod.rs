//! Window rule matching for workspace assignment.
//!
//! This module provides functionality to match windows against rules
//! defined in the configuration to determine which workspace they belong to.
//!
//! # Rule Matching
//!
//! Rules use AND logic - all specified properties must match for a rule to match.
//! At least one property must be specified for a rule to be valid.
//!
//! # Examples
//!
//! ```text
//! // Rule: app-id = "com.apple.finder"
//! // Matches: Any Finder window
//!
//! // Rule: app-id = "com.apple.Safari", title = "Settings"
//! // Matches: Safari windows with "Settings" in title (AND logic)
//! ```

use crate::config::WindowRule;
use crate::modules::tiling::state::Window;

/// Checks if a window matches a rule.
///
/// All specified properties in the rule must match (AND logic).
/// Returns `false` if the rule has no matching criteria.
///
/// # Matching Behavior
///
/// - `app_id`: Exact match against bundle identifier (case-insensitive)
/// - `app_name`: Case-insensitive substring match
/// - `title`: Case-insensitive substring match
#[must_use]
pub fn matches_window(rule: &WindowRule, window: &Window) -> bool {
    // Rule must have at least one criterion
    if !rule.is_valid() {
        return false;
    }

    // Check app_id (bundle identifier) - case-insensitive exact match
    if rule.app_id.is_some() {
        if let Some(app_id_lower) = &rule.app_id_lower {
            // Fast path: use pre-computed lowercase
            if !window.app_id.to_ascii_lowercase().eq(app_id_lower) {
                return false;
            }
        } else if let Some(app_id) = &rule.app_id {
            // Fallback: case-insensitive comparison
            if !window.app_id.eq_ignore_ascii_case(app_id) {
                return false;
            }
        }
    }

    // Check app_name - case-insensitive substring match
    if rule.app_name.is_some() {
        let window_app_lower = window.app_name.to_lowercase();
        if let Some(app_name_lower) = &rule.app_name_lower {
            // Fast path: use pre-computed lowercase
            if !window_app_lower.contains(app_name_lower.as_str()) {
                return false;
            }
        } else if let Some(app_name) = &rule.app_name {
            // Fallback: compute lowercase
            if !window_app_lower.contains(&app_name.to_lowercase()) {
                return false;
            }
        }
    }

    // Check title - case-insensitive substring match
    if rule.title.is_some() {
        let window_title_lower = window.title.to_lowercase();
        if let Some(title_lower) = &rule.title_lower {
            // Fast path: use pre-computed lowercase
            if !window_title_lower.contains(title_lower.as_str()) {
                return false;
            }
        } else if let Some(title) = &rule.title {
            // Fallback: compute lowercase
            if !window_title_lower.contains(&title.to_lowercase()) {
                return false;
            }
        }
    }

    true
}

/// Result of finding a workspace match for a window.
#[derive(Debug, Clone)]
pub struct WorkspaceMatch {
    /// Name of the matching workspace.
    pub workspace_name: String,
    /// Index of the matching rule within the workspace's rules.
    pub rule_index: usize,
}

/// Finds the first workspace that has a rule matching the given window.
///
/// Workspaces are checked in order, and rules within each workspace are
/// checked in order. The first matching rule wins.
///
/// # Arguments
///
/// * `window` - The window to find a workspace for
/// * `workspaces` - Iterator of (`workspace_name`, rules) pairs
///
/// # Returns
///
/// `Some(WorkspaceMatch)` if a matching rule was found, `None` otherwise
pub fn find_matching_workspace<'a, I>(window: &Window, workspaces: I) -> Option<WorkspaceMatch>
where I: IntoIterator<Item = (&'a str, &'a [WindowRule])> {
    for (workspace_name, rules) in workspaces {
        for (rule_index, rule) in rules.iter().enumerate() {
            if matches_window(rule, window) {
                return Some(WorkspaceMatch {
                    workspace_name: workspace_name.to_string(),
                    rule_index,
                });
            }
        }
    }
    None
}

/// Checks if any rule in the list matches the window.
///
/// # Arguments
///
/// * `rules` - The rules to check against
/// * `window` - The window to check
///
/// # Returns
///
/// `true` if any rule matches the window
#[must_use]
pub fn any_rule_matches(rules: &[WindowRule], window: &Window) -> bool {
    rules.iter().any(|rule| matches_window(rule, window))
}

/// Counts how many rules match a window.
///
/// Useful for debugging and testing rule configurations.
#[must_use]
pub fn count_matching_rules(rules: &[WindowRule], window: &Window) -> usize {
    rules.iter().filter(|rule| matches_window(rule, window)).count()
}

// ============================================================================
// Window Filtering
// ============================================================================

/// Bundle IDs of apps that should not be tiled.
///
/// These are system components that generate high event volume but will never
/// have windows we want to manage.
pub const SKIP_TILING_BUNDLE_IDS: &[&str] = &[
    // Stache itself
    crate::constants::APP_BUNDLE_ID,
    // macOS System Components
    "com.apple.dock",
    "com.apple.SystemUIServer",
    "com.apple.controlcenter",
    "com.apple.notificationcenterui",
    "com.apple.Spotlight",
    "com.apple.WindowManager",
    "com.apple.loginwindow",
    "com.apple.screencaptureui",
    "com.apple.screensaver",
    "com.apple.SecurityAgent",
    "com.apple.UserNotificationCenter",
    "com.apple.universalcontrol",
    "com.apple.TouchBarServer",
    "com.apple.AirPlayUIAgent",
    "com.apple.wifi.WiFiAgent",
    "com.apple.bluetoothUIServer",
    "com.apple.CoreLocationAgent",
    "com.apple.VoiceOver",
    "com.apple.AssistiveControl",
    "com.apple.SpeechRecognitionCore",
    "com.apple.accessibility.universalAccessAuthWarn",
    "com.apple.launchpad.launcher",
    "com.apple.FolderActionsDispatcher",
];

/// App names to skip tiling when bundle ID is not available.
pub const SKIP_TILING_APP_NAMES: &[&str] = &[
    "Dock",
    "SystemUIServer",
    "Control Center",
    "Notification Center",
    "Spotlight",
    "Window Manager",
    "WindowManager",
    "loginwindow",
    "borders",
];

/// AX subrole for Picture-in-Picture windows.
///
/// PiP windows should not be tiled as they are meant to float above other content.
pub const PIP_SUBROLE: &str = "AXFloatingWindow";

/// Determines whether a window should be tiled.
///
/// Returns `false` for system windows and utilities that we know will never
/// have windows we want to manage.
#[must_use]
pub fn should_tile_window(bundle_id: &str, app_name: &str) -> bool {
    // Check bundle ID (most reliable)
    if !bundle_id.is_empty()
        && SKIP_TILING_BUNDLE_IDS.iter().any(|&id| bundle_id.eq_ignore_ascii_case(id))
    {
        return false;
    }

    // Check app name (fallback when bundle ID is not available)
    if !app_name.is_empty()
        && SKIP_TILING_APP_NAMES.iter().any(|&name| app_name.eq_ignore_ascii_case(name))
    {
        return false;
    }

    true
}

/// Checks if an app name should be skipped for tiling.
#[must_use]
pub fn should_skip_app_by_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    SKIP_TILING_APP_NAMES
        .iter()
        .any(|&skip_name| name.eq_ignore_ascii_case(skip_name))
}

/// Checks if a window subrole indicates a Picture-in-Picture window.
///
/// PiP windows have the subrole `"AXFloatingWindow"` and should not be tiled.
#[must_use]
pub fn is_pip_window(subrole: Option<&str>) -> bool { subrole.is_some_and(|sr| sr == PIP_SUBROLE) }

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::modules::tiling::state::Rect;

    /// Creates a test window with the given properties.
    fn make_window(bundle_id: &str, app_name: &str, title: &str) -> Window {
        Window {
            id: 1,
            pid: 1234,
            app_id: bundle_id.to_string(),
            app_name: app_name.to_string(),
            title: title.to_string(),
            workspace_id: Uuid::now_v7(),
            frame: Rect::new(0.0, 0.0, 800.0, 600.0),
            minimum_size: None,
            inferred_minimum_size: None,
            expected_frame: None,
            is_minimized: false,
            is_fullscreen: false,
            is_floating: false,
            is_hidden: false,
            tab_group_id: None,
            is_active_tab: true,
            matched_rule: None,
        }
    }

    /// Creates a rule with optional properties (with pre-computed lowercase).
    fn make_rule(app_id: Option<&str>, app_name: Option<&str>, title: Option<&str>) -> WindowRule {
        let mut rule = WindowRule {
            app_id: app_id.map(String::from),
            app_name: app_name.map(String::from),
            title: title.map(String::from),
            app_id_lower: None,
            app_name_lower: None,
            title_lower: None,
        };
        rule.prepare();
        rule
    }

    // ========================================================================
    // Basic matching tests
    // ========================================================================

    #[test]
    fn test_matches_window_empty_rule() {
        let window = make_window("com.apple.finder", "Finder", "Documents");
        let rule = make_rule(None, None, None);

        assert!(!rule.is_valid());
        assert!(!matches_window(&rule, &window));
    }

    #[test]
    fn test_matches_window_app_id_only() {
        let window = make_window("com.apple.finder", "Finder", "Documents");

        // Exact match
        let rule = make_rule(Some("com.apple.finder"), None, None);
        assert!(matches_window(&rule, &window));

        // Case-insensitive match
        let rule = make_rule(Some("COM.APPLE.FINDER"), None, None);
        assert!(matches_window(&rule, &window));

        // Non-match
        let rule = make_rule(Some("com.apple.safari"), None, None);
        assert!(!matches_window(&rule, &window));
    }

    #[test]
    fn test_matches_window_app_name_only() {
        let window = make_window("com.apple.finder", "Finder", "Documents");

        // Exact match
        let rule = make_rule(None, Some("Finder"), None);
        assert!(matches_window(&rule, &window));

        // Case-insensitive match
        let rule = make_rule(None, Some("finder"), None);
        assert!(matches_window(&rule, &window));

        // Substring match
        let rule = make_rule(None, Some("ind"), None);
        assert!(matches_window(&rule, &window));

        // Non-match
        let rule = make_rule(None, Some("Safari"), None);
        assert!(!matches_window(&rule, &window));
    }

    #[test]
    fn test_matches_window_title_only() {
        let window = make_window("com.apple.finder", "Finder", "Documents - MyFolder");

        // Exact match
        let rule = make_rule(None, None, Some("Documents - MyFolder"));
        assert!(matches_window(&rule, &window));

        // Case-insensitive match
        let rule = make_rule(None, None, Some("documents"));
        assert!(matches_window(&rule, &window));

        // Substring match
        let rule = make_rule(None, None, Some("MyFolder"));
        assert!(matches_window(&rule, &window));

        // Non-match
        let rule = make_rule(None, None, Some("Downloads"));
        assert!(!matches_window(&rule, &window));
    }

    // ========================================================================
    // AND logic tests
    // ========================================================================

    #[test]
    fn test_matches_window_and_logic_app_id_and_title() {
        let window = make_window("com.apple.safari", "Safari", "Settings");

        // Both match
        let rule = make_rule(Some("com.apple.safari"), None, Some("Settings"));
        assert!(matches_window(&rule, &window));

        // app_id matches, title doesn't
        let rule = make_rule(Some("com.apple.safari"), None, Some("History"));
        assert!(!matches_window(&rule, &window));

        // title matches, app_id doesn't
        let rule = make_rule(Some("com.apple.finder"), None, Some("Settings"));
        assert!(!matches_window(&rule, &window));
    }

    #[test]
    fn test_matches_window_and_logic_all_three() {
        let window = make_window("com.apple.safari", "Safari", "Settings");

        // All three match
        let rule = make_rule(Some("com.apple.safari"), Some("Safari"), Some("Settings"));
        assert!(matches_window(&rule, &window));

        // Two match, one doesn't (app_name fails)
        let rule = make_rule(Some("com.apple.safari"), Some("Chrome"), Some("Settings"));
        assert!(!matches_window(&rule, &window));

        // Only one matches
        let rule = make_rule(Some("com.google.chrome"), Some("Chrome"), Some("Settings"));
        assert!(!matches_window(&rule, &window));
    }

    // ========================================================================
    // find_matching_workspace tests
    // ========================================================================

    #[test]
    fn test_find_matching_workspace_no_match() {
        let window = make_window("com.apple.finder", "Finder", "Documents");

        let browser_rules = [make_rule(Some("com.apple.safari"), None, None)];
        let code_rules = [make_rule(Some("com.microsoft.vscode"), None, None)];

        let workspaces: Vec<(&str, &[WindowRule])> =
            vec![("browser", &browser_rules), ("code", &code_rules)];

        let result = find_matching_workspace(&window, workspaces);
        assert!(result.is_none());
    }

    #[test]
    fn test_find_matching_workspace_single_match() {
        let window = make_window("com.apple.safari", "Safari", "Google");

        let browser_rules = [make_rule(Some("com.apple.safari"), None, None)];
        let code_rules = [make_rule(Some("com.microsoft.vscode"), None, None)];

        let workspaces: Vec<(&str, &[WindowRule])> =
            vec![("browser", &browser_rules), ("code", &code_rules)];

        let result = find_matching_workspace(&window, workspaces);
        assert!(result.is_some());
        let match_result = result.unwrap();
        assert_eq!(match_result.workspace_name, "browser");
        assert_eq!(match_result.rule_index, 0);
    }

    #[test]
    fn test_find_matching_workspace_first_match_wins() {
        let window = make_window("com.apple.safari", "Safari", "Google");

        // Both workspaces have rules that would match
        let ws1_rules = [make_rule(Some("com.apple.safari"), None, None)];
        let ws2_rules = [make_rule(None, Some("Safari"), None)];

        let workspaces: Vec<(&str, &[WindowRule])> =
            vec![("workspace-1", &ws1_rules), ("workspace-2", &ws2_rules)];

        let result = find_matching_workspace(&window, workspaces);
        assert!(result.is_some());
        let match_result = result.unwrap();
        // First workspace should win
        assert_eq!(match_result.workspace_name, "workspace-1");
    }

    // ========================================================================
    // Helper function tests
    // ========================================================================

    #[test]
    fn test_any_rule_matches() {
        let window = make_window("com.apple.finder", "Finder", "Documents");

        let rules = [
            make_rule(Some("com.apple.safari"), None, None),
            make_rule(Some("com.apple.finder"), None, None),
        ];

        assert!(any_rule_matches(&rules, &window));

        let rules = [
            make_rule(Some("com.apple.safari"), None, None),
            make_rule(Some("com.google.chrome"), None, None),
        ];

        assert!(!any_rule_matches(&rules, &window));

        // Empty rules
        let rules: [WindowRule; 0] = [];
        assert!(!any_rule_matches(&rules, &window));
    }

    #[test]
    fn test_count_matching_rules() {
        let window = make_window("com.apple.finder", "Finder", "Documents");

        let rules = [
            make_rule(Some("com.apple.finder"), None, None), // Matches
            make_rule(None, Some("Finder"), None),           // Matches
            make_rule(None, None, Some("Documents")),        // Matches
            make_rule(Some("com.apple.safari"), None, None), // Doesn't match
            make_rule(Some("com.apple.finder"), None, Some("X")), // Doesn't match (AND fails)
        ];

        assert_eq!(count_matching_rules(&rules, &window), 3);
    }

    // ========================================================================
    // Window filtering tests
    // ========================================================================

    #[test]
    fn test_should_tile_window_system_apps() {
        assert!(!should_tile_window("com.apple.dock", "Dock"));
        assert!(!should_tile_window("com.apple.SystemUIServer", "SystemUIServer"));
        assert!(!should_tile_window("com.apple.Spotlight", "Spotlight"));
        assert!(!should_tile_window("com.marcosmoura.stache", "Stache"));
    }

    #[test]
    fn test_should_tile_window_regular_apps() {
        assert!(should_tile_window("com.apple.Safari", "Safari"));
        assert!(should_tile_window("com.apple.Terminal", "Terminal"));
        assert!(should_tile_window("com.google.Chrome", "Google Chrome"));
        assert!(should_tile_window("com.microsoft.vscode", "Visual Studio Code"));
    }

    #[test]
    fn test_should_skip_app_by_name() {
        assert!(should_skip_app_by_name("Dock"));
        assert!(should_skip_app_by_name("SystemUIServer"));
        assert!(should_skip_app_by_name("borders")); // JankyBorders binary name
        assert!(!should_skip_app_by_name("Safari"));
        assert!(!should_skip_app_by_name("Terminal"));
        assert!(!should_skip_app_by_name(""));
    }

    #[test]
    fn test_is_pip_window() {
        // PiP windows have subrole AXFloatingWindow
        assert!(is_pip_window(Some("AXFloatingWindow")));

        // Regular windows should not be detected as PiP
        assert!(!is_pip_window(Some("AXStandardWindow")));
        assert!(!is_pip_window(Some("AXDialog")));
        assert!(!is_pip_window(Some("")));
        assert!(!is_pip_window(None));
    }
}
