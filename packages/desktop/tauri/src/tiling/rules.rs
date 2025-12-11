//! Unified rule matching for windows.
//!
//! This module provides a generic trait for matching windows against rules,
//! eliminating duplicate matching logic between `WindowRule` and `IgnoreRule`.

use crate::tiling::state::ManagedWindow;
use crate::tiling::window::RunningApp;

// ============================================================================
// Rule Matcher Trait
// ============================================================================

/// A trait for types that can be matched against windows.
///
/// This provides a unified interface for matching windows against various rule types,
/// including `WindowRule`, `IgnoreRule`, and potentially others.
///
/// Rules use AND logic: all specified criteria must match for the rule to match.
/// If a criterion is `None`, it is ignored (always matches).
pub trait RuleMatcher {
    /// Returns the optional title pattern to match against (case-insensitive substring).
    fn title(&self) -> Option<&str>;

    /// Returns the optional class pattern to match against (substring).
    fn class(&self) -> Option<&str>;

    /// Returns the optional app ID (bundle identifier) to match against.
    fn app_id(&self) -> Option<&str>;

    /// Returns the optional app name to match against (case-insensitive substring).
    fn name(&self) -> Option<&str>;

    /// Returns whether this rule has any matching criteria.
    fn is_empty(&self) -> bool {
        self.title().is_none()
            && self.class().is_none()
            && self.app_id().is_none()
            && self.name().is_none()
    }

    /// Checks if a window matches this rule.
    ///
    /// All specified criteria must match (AND logic).
    /// If no criteria are specified, the rule doesn't match anything.
    fn matches_window(&self, window: &ManagedWindow) -> bool {
        // If no criteria specified, the rule doesn't match anything
        if self.is_empty() {
            return false;
        }

        // Check app_id (bundle ID) - supports exact and substring match
        if let Some(rule_app_id) = self.app_id() {
            if let Some(ref bundle_id) = window.bundle_id {
                if !bundle_id.contains(rule_app_id) && bundle_id != rule_app_id {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check name (app name) - case-insensitive substring match
        if let Some(rule_name) = self.name() {
            let rule_name_lower = rule_name.to_lowercase();
            let app_name_lower = window.app_name.to_lowercase();
            if !app_name_lower.contains(&rule_name_lower) && app_name_lower != rule_name_lower {
                return false;
            }
        }

        // Check title - case-insensitive substring match
        if let Some(rule_title) = self.title()
            && !window.title.to_lowercase().contains(&rule_title.to_lowercase())
        {
            return false;
        }

        // Check class - substring match
        if let Some(rule_class) = self.class() {
            if let Some(ref window_class) = window.class {
                if !window_class.contains(rule_class) && window_class != rule_class {
                    return false;
                }
            } else {
                return false;
            }
        }

        true
    }

    /// Checks if a running app matches this rule.
    ///
    /// This is a simplified version for apps without full window info.
    /// Only `app_id` and `name` criteria are checked.
    fn matches_app(&self, app: &RunningApp) -> bool {
        // If no app-level criteria specified, no match
        if self.app_id().is_none() && self.name().is_none() {
            return false;
        }

        // Check app_id (bundle ID)
        if let Some(rule_app_id) = self.app_id() {
            if let Some(ref bundle_id) = app.bundle_id {
                if !bundle_id.contains(rule_app_id) && bundle_id != rule_app_id {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check name (app name)
        if let Some(rule_name) = self.name() {
            let rule_name_lower = rule_name.to_lowercase();
            let app_name_lower = app.name.to_lowercase();
            if !app_name_lower.contains(&rule_name_lower) && app_name_lower != rule_name_lower {
                return false;
            }
        }

        true
    }
}

// ============================================================================
// Implementations for barba_shared types
// ============================================================================

impl RuleMatcher for barba_shared::WindowRule {
    fn title(&self) -> Option<&str> { self.title.as_deref() }

    fn class(&self) -> Option<&str> { self.class.as_deref() }

    fn app_id(&self) -> Option<&str> { self.app_id.as_deref() }

    fn name(&self) -> Option<&str> { self.name.as_deref() }
}

impl RuleMatcher for barba_shared::IgnoreRule {
    fn title(&self) -> Option<&str> { self.title.as_deref() }

    fn class(&self) -> Option<&str> { self.class.as_deref() }

    fn app_id(&self) -> Option<&str> { self.app_id.as_deref() }

    fn name(&self) -> Option<&str> { self.name.as_deref() }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use barba_shared::{IgnoreRule, WindowRule};

    use super::*;
    use crate::tiling::state::WindowFrame;

    fn create_test_window(
        title: &str,
        app_name: &str,
        bundle_id: Option<&str>,
        class: Option<&str>,
    ) -> ManagedWindow {
        ManagedWindow {
            id: 1,
            pid: 1000,
            title: title.to_string(),
            app_name: app_name.to_string(),
            bundle_id: bundle_id.map(String::from),
            class: class.map(String::from),
            workspace: String::new(),
            is_floating: false,
            is_minimized: false,
            is_fullscreen: false,
            is_hidden: false,
            frame: WindowFrame::default(),
        }
    }

    #[test]
    fn test_empty_rule_matches_nothing() {
        let rule = WindowRule::default();
        let window = create_test_window("Test", "TestApp", Some("com.test.app"), None);

        assert!(rule.is_empty());
        assert!(!rule.matches_window(&window));
    }

    #[test]
    fn test_app_id_exact_match() {
        let rule = WindowRule {
            app_id: Some("com.apple.Safari".to_string()),
            ..Default::default()
        };
        let window = create_test_window("Safari", "Safari", Some("com.apple.Safari"), None);

        assert!(rule.matches_window(&window));
    }

    #[test]
    fn test_app_id_substring_match() {
        let rule = WindowRule {
            app_id: Some("Safari".to_string()),
            ..Default::default()
        };
        let window = create_test_window("Safari", "Safari", Some("com.apple.Safari"), None);

        assert!(rule.matches_window(&window));
    }

    #[test]
    fn test_name_case_insensitive() {
        let rule = WindowRule {
            name: Some("safari".to_string()),
            ..Default::default()
        };
        let window = create_test_window("Test", "Safari", None, None);

        assert!(rule.matches_window(&window));
    }

    #[test]
    fn test_title_substring_match() {
        let rule = WindowRule {
            title: Some("GitHub".to_string()),
            ..Default::default()
        };
        let window = create_test_window("GitHub - Pull Requests", "Chrome", None, None);

        assert!(rule.matches_window(&window));
    }

    #[test]
    fn test_class_match() {
        let rule = WindowRule {
            class: Some("AXWindow".to_string()),
            ..Default::default()
        };
        let window = create_test_window("Test", "TestApp", None, Some("AXWindow"));

        assert!(rule.matches_window(&window));
    }

    #[test]
    fn test_multiple_criteria_and_logic() {
        let rule = WindowRule {
            app_id: Some("com.apple.Safari".to_string()),
            title: Some("GitHub".to_string()),
            ..Default::default()
        };

        // Both match
        let window1 = create_test_window("GitHub - Home", "Safari", Some("com.apple.Safari"), None);
        assert!(rule.matches_window(&window1));

        // Only app_id matches
        let window2 = create_test_window("Apple", "Safari", Some("com.apple.Safari"), None);
        assert!(!rule.matches_window(&window2));

        // Only title matches
        let window3 = create_test_window("GitHub", "Chrome", Some("com.google.Chrome"), None);
        assert!(!rule.matches_window(&window3));
    }

    #[test]
    fn test_ignore_rule_works_same_as_window_rule() {
        let ignore_rule = IgnoreRule {
            app_id: Some("com.raycast.macos".to_string()),
            ..Default::default()
        };
        let window = create_test_window("Raycast", "Raycast", Some("com.raycast.macos"), None);

        assert!(ignore_rule.matches_window(&window));
    }

    #[test]
    fn test_matches_app() {
        let rule = WindowRule {
            app_id: Some("Safari".to_string()),
            name: Some("Safari".to_string()),
            ..Default::default()
        };
        let app = RunningApp {
            pid: 1000,
            name: "Safari".to_string(),
            bundle_id: Some("com.apple.Safari".to_string()),
        };

        assert!(rule.matches_app(&app));
    }

    #[test]
    fn test_missing_bundle_id_fails_app_id_match() {
        let rule = WindowRule {
            app_id: Some("com.apple.Safari".to_string()),
            ..Default::default()
        };
        let window = create_test_window("Safari", "Safari", None, None);

        assert!(!rule.matches_window(&window));
    }
}
