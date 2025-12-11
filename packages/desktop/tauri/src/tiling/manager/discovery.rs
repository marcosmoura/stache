//! Window and app discovery and rule matching.
//!
//! This module handles discovering windows on the system and matching them
//! to workspaces based on configured rules.

#![allow(clippy::cast_possible_wrap)]

use std::collections::HashSet;

use super::TilingManager;
use crate::tiling::rules::RuleMatcher;
use crate::tiling::state::ManagedWindow;
use crate::tiling::window;

impl TilingManager {
    /// Discovers existing windows and assigns them to workspaces based on rules.
    ///
    /// This method performs the following steps:
    /// 1. Unhides ALL running applications to ensure all windows are discoverable
    /// 2. Waits briefly for macOS to register the unhidden windows
    /// 3. Discovers all windows and assigns them to workspaces based on rules
    /// 4. Tracks PIDs for each workspace
    ///
    /// Note: After this method completes, `hide_non_focused_workspaces` should be
    /// called to hide windows that don't belong to the focused workspace.
    pub(super) fn discover_and_assign_windows(&mut self) {
        // First, get ALL running apps and unhide them to ensure all windows are discoverable
        let running_apps = window::get_all_running_apps();
        let all_pids: HashSet<i32> = running_apps.iter().map(|app| app.pid).collect();

        // Unhide all apps in parallel
        std::thread::scope(|s| {
            for pid in &all_pids {
                let pid = *pid;
                s.spawn(move || {
                    let _ = window::unhide_app(pid);
                });
            }
        });

        // Wait for macOS to register the unhidden windows
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Track PIDs for apps based on rules (do this before window discovery
        // so we know which workspaces have apps even if their windows aren't visible yet)
        for app in &running_apps {
            if let Some(workspace_name) = self.find_workspace_for_app(app) {
                self.workspace_pids.entry(workspace_name).or_default().insert(app.pid);
            }
        }

        // Now discover ALL windows (including ones that were just unhidden)
        let windows = match window::get_all_windows_including_hidden() {
            Ok(w) => w,
            Err(e) => {
                eprintln!("barba: failed to discover windows: {e}");
                return;
            }
        };

        for win in windows {
            // Skip dialogs, sheets, and other non-tileable window types
            if window::is_dialog_or_sheet(&win) {
                continue;
            }

            // Skip windows that match ignore rules (higher priority than workspace rules)
            if self.should_ignore_window(&win) {
                continue;
            }

            // Find which workspace this window belongs to based on rules
            if let Some(workspace_name) = self.find_workspace_for_window(&win) {
                // Track PID for this workspace (persists even when window IDs change)
                self.workspace_pids.entry(workspace_name.clone()).or_default().insert(win.pid);

                // Add window to state and workspace
                let window_id = win.id;
                let mut win = win;
                win.workspace = workspace_name.clone(); // Mark as assigned to this workspace
                self.workspace_manager.state_mut().windows.insert(window_id, win);

                if let Some(ws) =
                    self.workspace_manager.state_mut().get_workspace_mut(&workspace_name)
                    && !ws.windows.contains(&window_id)
                {
                    ws.windows.push(window_id);
                }
            }
        }
    }

    /// Finds the workspace an app should be assigned to based on rules.
    pub(super) fn find_workspace_for_app(&self, app: &window::RunningApp) -> Option<String> {
        // First check global rules (with workspace target)
        for rule in &self.config.rules {
            if let Some(ref workspace) = rule.workspace
                && rule.matches_app(app)
            {
                return Some(workspace.clone());
            }
        }

        // Then check per-workspace rules
        for ws_config in &self.config.workspaces {
            for rule in &ws_config.rules {
                if rule.matches_app(app) {
                    return Some(ws_config.name.clone());
                }
            }
        }
        None
    }

    /// Finds the workspace a window should be assigned to based on rules.
    /// Rules take precedence and will move windows across screens if needed.
    ///
    /// Rule priority:
    /// 1. Global rules (TilingConfig.rules) with workspace target
    /// 2. Per-workspace rules (WorkspaceConfig.rules)
    /// 3. Current screen's workspace
    /// 4. Focused workspace (fallback)
    pub fn find_workspace_for_window(&self, win: &ManagedWindow) -> Option<String> {
        // First, check global rules (with workspace target)
        for rule in &self.config.rules {
            if let Some(ref workspace) = rule.workspace
                && rule.matches_window(win)
            {
                return Some(workspace.clone());
            }
        }

        // Then, check per-workspace rules
        for ws_config in &self.config.workspaces {
            for rule in &ws_config.rules {
                if rule.matches_window(win) {
                    return Some(ws_config.name.clone());
                }
            }
        }

        // If no rule matches, find a workspace on the window's current screen
        let window_screen_id = self.find_screen_for_window(win);
        if let Some(screen_id) = window_screen_id {
            // Find any workspace on this screen
            for ws in &self.workspace_manager.state().workspaces {
                if ws.screen == screen_id {
                    return Some(ws.name.clone());
                }
            }
        }

        // Fallback to focused workspace or first workspace
        self.workspace_manager
            .state()
            .focused_workspace
            .clone()
            .or_else(|| self.workspace_manager.state().workspaces.first().map(|ws| ws.name.clone()))
    }

    /// Finds which screen a window is on based on its position.
    pub(super) fn find_screen_for_window(&self, win: &ManagedWindow) -> Option<String> {
        let screens = &self.workspace_manager.state().screens;

        // Find which screen contains the center of the window
        let window_center_x = win.frame.x + (win.frame.width as i32 / 2);
        let window_center_y = win.frame.y + (win.frame.height as i32 / 2);

        for screen in screens {
            let sx = screen.frame.x;
            let sy = screen.frame.y;
            let sw = screen.frame.width as i32;
            let sh = screen.frame.height as i32;

            if window_center_x >= sx
                && window_center_x < sx + sw
                && window_center_y >= sy
                && window_center_y < sy + sh
            {
                return Some(screen.id.clone());
            }
        }

        // Fallback to main screen if we can't determine
        screens.iter().find(|s| s.is_main).map(|s| s.id.clone())
    }

    /// Checks if a window should be ignored based on the ignore rules.
    ///
    /// Returns `true` if the window matches any ignore rule and should not be tracked.
    /// Ignore rules have higher priority than workspace rules.
    pub fn should_ignore_window(&self, win: &ManagedWindow) -> bool {
        self.config.ignore.iter().any(|rule| rule.matches_window(win))
    }
}

#[cfg(test)]
mod tests {
    use barba_shared::{ScreenTarget, TilingConfig, WindowRule, WorkspaceConfig};

    use super::super::TilingManager;
    use crate::tiling::rules::RuleMatcher;
    use crate::tiling::state::{ManagedWindow, WindowFrame};
    use crate::tiling::workspace::WorkspaceManager;

    /// Helper to create a test window with minimal fields
    fn create_test_window(
        id: u64,
        title: &str,
        bundle_id: Option<&str>,
        class: Option<&str>,
    ) -> ManagedWindow {
        ManagedWindow {
            id,
            pid: 1000 + id as i32,
            title: title.to_string(),
            app_name: title.to_string(),
            bundle_id: bundle_id.map(String::from),
            class: class.map(String::from),
            workspace: String::new(),
            is_floating: false,
            is_minimized: false,
            is_fullscreen: false,
            is_hidden: false,
            frame: WindowFrame {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            },
        }
    }

    /// Helper to create a test TilingManager with custom config
    fn create_test_manager(config: TilingConfig) -> TilingManager {
        let workspace_manager = WorkspaceManager::new(config.clone());
        TilingManager {
            config,
            workspace_manager,
            workspace_pids: std::collections::HashMap::new(),
            app_handle: None,
        }
    }

    // =========================================================================
    // RuleMatcher tests (rule.matches_window)
    // =========================================================================

    #[test]
    fn test_rule_match_by_app_id_exact() {
        let window = create_test_window(1, "Safari", Some("com.apple.Safari"), None);

        let rule = WindowRule {
            app_id: Some("com.apple.Safari".to_string()),
            ..Default::default()
        };

        assert!(rule.matches_window(&window));
    }

    #[test]
    fn test_rule_match_by_app_id_substring() {
        let window = create_test_window(1, "Safari", Some("com.apple.Safari"), None);

        // Substring match
        let rule = WindowRule {
            app_id: Some("Safari".to_string()),
            ..Default::default()
        };

        assert!(rule.matches_window(&window));
    }

    #[test]
    fn test_rule_match_by_app_id_no_match() {
        let window = create_test_window(1, "Safari", Some("com.apple.Safari"), None);

        let rule = WindowRule {
            app_id: Some("com.google.Chrome".to_string()),
            ..Default::default()
        };

        assert!(!rule.matches_window(&window));
    }

    #[test]
    fn test_rule_match_by_title_case_insensitive() {
        let window = create_test_window(1, "My Document - Firefox", None, None);

        let rule = WindowRule {
            title: Some("firefox".to_string()),
            ..Default::default()
        };

        assert!(rule.matches_window(&window));
    }

    #[test]
    fn test_rule_match_by_title_substring() {
        let window = create_test_window(1, "Project - Visual Studio Code", None, None);

        let rule = WindowRule {
            title: Some("Visual Studio".to_string()),
            ..Default::default()
        };

        assert!(rule.matches_window(&window));
    }

    #[test]
    fn test_rule_match_by_class() {
        let window = create_test_window(1, "Terminal", None, Some("NSWindow"));

        let rule = WindowRule {
            class: Some("NSWindow".to_string()),
            ..Default::default()
        };

        assert!(rule.matches_window(&window));
    }

    #[test]
    fn test_rule_match_multiple_criteria_and_logic() {
        let window =
            create_test_window(1, "Safari - Google", Some("com.apple.Safari"), Some("NSWindow"));

        // All criteria match
        let rule = WindowRule {
            app_id: Some("Safari".to_string()),
            title: Some("Google".to_string()),
            class: Some("NSWindow".to_string()),
            ..Default::default()
        };
        assert!(rule.matches_window(&window));

        // One criterion doesn't match (title)
        let rule = WindowRule {
            app_id: Some("Safari".to_string()),
            title: Some("Firefox".to_string()), // Doesn't match
            class: Some("NSWindow".to_string()),
            ..Default::default()
        };
        assert!(!rule.matches_window(&window));
    }

    #[test]
    fn test_rule_empty_doesnt_match() {
        let window = create_test_window(1, "Any Window", Some("com.any.app"), None);

        let empty_rule = WindowRule::default();
        assert!(!empty_rule.matches_window(&window));
    }

    #[test]
    fn test_rule_match_window_without_bundle_id() {
        let window = create_test_window(1, "Unknown App", None, None);

        // Rule requires app_id but window doesn't have one
        let rule = WindowRule {
            app_id: Some("com.some.app".to_string()),
            ..Default::default()
        };
        assert!(!rule.matches_window(&window));

        // But title-only rule should work
        let rule = WindowRule {
            title: Some("Unknown".to_string()),
            ..Default::default()
        };
        assert!(rule.matches_window(&window));
    }

    // =========================================================================
    // find_workspace_for_window tests
    // =========================================================================

    #[test]
    fn test_find_workspace_global_rule_takes_priority() {
        let config = TilingConfig {
            workspaces: vec![
                WorkspaceConfig {
                    name: "browser".to_string(),
                    screen: ScreenTarget::Main,
                    rules: vec![WindowRule {
                        app_id: Some("com.apple.Safari".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                WorkspaceConfig {
                    name: "code".to_string(),
                    screen: ScreenTarget::Main,
                    ..Default::default()
                },
            ],
            // Global rule points Safari to "code" workspace instead
            rules: vec![WindowRule {
                app_id: Some("com.apple.Safari".to_string()),
                workspace: Some("code".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };

        let manager = create_test_manager(config);
        let window = create_test_window(1, "Safari", Some("com.apple.Safari"), None);

        // Global rule should take priority
        let result = manager.find_workspace_for_window(&window);
        assert_eq!(result, Some("code".to_string()));
    }

    #[test]
    fn test_find_workspace_per_workspace_rule() {
        let config = TilingConfig {
            workspaces: vec![
                WorkspaceConfig {
                    name: "browser".to_string(),
                    screen: ScreenTarget::Main,
                    rules: vec![WindowRule {
                        app_id: Some("com.apple.Safari".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                WorkspaceConfig {
                    name: "terminal".to_string(),
                    screen: ScreenTarget::Main,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let manager = create_test_manager(config);
        let window = create_test_window(1, "Safari", Some("com.apple.Safari"), None);

        let result = manager.find_workspace_for_window(&window);
        assert_eq!(result, Some("browser".to_string()));
    }

    #[test]
    fn test_find_workspace_no_match_falls_back() {
        let config = TilingConfig {
            workspaces: vec![
                WorkspaceConfig {
                    name: "1".to_string(),
                    screen: ScreenTarget::Main,
                    ..Default::default()
                },
                WorkspaceConfig {
                    name: "2".to_string(),
                    screen: ScreenTarget::Main,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let mut manager = create_test_manager(config);

        // Set focused workspace
        manager.workspace_manager.state_mut().focused_workspace = Some("2".to_string());

        let window = create_test_window(1, "Some App", Some("com.unknown.app"), None);

        // No rules match, should fall back to focused workspace
        let result = manager.find_workspace_for_window(&window);
        assert_eq!(result, Some("2".to_string()));
    }

    #[test]
    fn test_find_workspace_first_matching_rule_wins() {
        let config = TilingConfig {
            workspaces: vec![
                WorkspaceConfig {
                    name: "first".to_string(),
                    screen: ScreenTarget::Main,
                    rules: vec![WindowRule {
                        app_id: Some("Safari".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                WorkspaceConfig {
                    name: "second".to_string(),
                    screen: ScreenTarget::Main,
                    rules: vec![WindowRule {
                        app_id: Some("Safari".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let manager = create_test_manager(config);
        let window = create_test_window(1, "Safari", Some("com.apple.Safari"), None);

        // First matching workspace should win
        let result = manager.find_workspace_for_window(&window);
        assert_eq!(result, Some("first".to_string()));
    }
}
