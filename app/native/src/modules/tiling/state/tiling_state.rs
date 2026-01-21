//! The main `TilingState` struct with reactive collections.
//!
//! Uses `eyeball` and `eyeball-im` for observable state that can be subscribed to.
//!
//! # Performance
//!
//! This module maintains auxiliary `HashMap` indices for O(1) lookups by ID.
//! The indices are kept in sync with the `ObservableVector` collections through
//! the `upsert_*` and `remove_*` methods. Always use these methods instead of
//! directly manipulating the vectors.

use std::collections::HashMap;

use eyeball::Observable;
use eyeball_im::ObservableVector;
use uuid::Uuid;

use super::types::{FocusState, Screen, Window, Workspace};

/// The root state container for the tiling window manager.
///
/// All collections are observable, allowing subscribers to react to changes.
/// This follows a relational model where:
/// - `Screen.id` is the primary key for screens (from macOS)
/// - `Workspace.id` is the primary key for workspaces (UUID v7)
/// - `Window.id` is the primary key for windows (from macOS)
///
/// Relations:
/// - `Workspace.screen_id` → `Screen.id`
/// - `Window.workspace_id` → `Workspace.id`
///
/// # Indices
///
/// Auxiliary `HashMap` indices provide O(1) lookups:
/// - `screen_idx`: `screen_id` → index in `screens`
/// - `workspace_idx`: `workspace_id` → index in `workspaces`
/// - `window_idx`: `window_id` → index in `windows`
pub struct TilingState {
    /// All physical displays.
    pub screens: ObservableVector<Screen>,

    /// All virtual workspaces.
    pub workspaces: ObservableVector<Workspace>,

    /// All tracked windows.
    pub windows: ObservableVector<Window>,

    /// Global focus state.
    pub focus: Observable<FocusState>,

    /// Whether tiling is enabled.
    pub enabled: Observable<bool>,

    /// Focus history: remembers the last focused window in each workspace.
    /// Maps `workspace_id` -> `window_id`.
    focus_history: HashMap<Uuid, u32>,

    // ════════════════════════════════════════════════════════════════════════
    // Auxiliary Indices for O(1) Lookups
    // ════════════════════════════════════════════════════════════════════════
    /// Index: `screen_id` → position in `screens` vector.
    screen_idx: HashMap<u32, usize>,

    /// Index: `workspace_id` → position in `workspaces` vector.
    workspace_idx: HashMap<Uuid, usize>,

    /// Index: `window_id` → position in `windows` vector.
    window_idx: HashMap<u32, usize>,
}

impl Default for TilingState {
    fn default() -> Self { Self::new() }
}

impl TilingState {
    /// Create a new empty tiling state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            screens: ObservableVector::new(),
            workspaces: ObservableVector::new(),
            windows: ObservableVector::new(),
            focus: Observable::new(FocusState::new()),
            enabled: Observable::new(true),
            focus_history: HashMap::new(),
            screen_idx: HashMap::new(),
            workspace_idx: HashMap::new(),
            window_idx: HashMap::new(),
        }
    }

    // ========================================================================
    // Screen Operations (O(1) lookups via index)
    // ========================================================================

    /// Get a screen by ID. O(1) via index.
    #[must_use]
    pub fn get_screen(&self, id: u32) -> Option<Screen> {
        self.screen_idx.get(&id).and_then(|&idx| self.screens.get(idx)).cloned()
    }

    /// Get a screen by name. O(n) - no index for names.
    #[must_use]
    pub fn get_screen_by_name(&self, name: &str) -> Option<Screen> {
        self.screens.iter().find(|s| s.name == name).cloned()
    }

    /// Get the main screen. O(n) - no index for `is_main`.
    #[must_use]
    pub fn get_main_screen(&self) -> Option<Screen> {
        self.screens.iter().find(|s| s.is_main).cloned()
    }

    /// Get the index of a screen by ID. O(1) via index.
    #[must_use]
    pub fn screen_index(&self, id: u32) -> Option<usize> { self.screen_idx.get(&id).copied() }

    /// Insert or update a screen. Maintains index.
    pub fn upsert_screen(&mut self, screen: Screen) {
        let screen_id = screen.id;
        if let Some(&idx) = self.screen_idx.get(&screen_id) {
            self.screens.set(idx, screen);
        } else {
            let idx = self.screens.len();
            self.screens.push_back(screen);
            self.screen_idx.insert(screen_id, idx);
        }
    }

    /// Remove a screen by ID. Maintains index.
    pub fn remove_screen(&mut self, id: u32) -> Option<Screen> {
        let idx = self.screen_idx.remove(&id)?;
        let screen = self.screens.remove(idx);

        // Update indices for all screens that shifted down
        for (&screen_id, stored_idx) in &mut self.screen_idx {
            if *stored_idx > idx {
                *stored_idx -= 1;
            }
            // Sanity check in debug builds
            debug_assert_eq!(
                self.screens.get(*stored_idx).map(|s| s.id),
                Some(screen_id),
                "screen_idx out of sync after remove"
            );
        }

        Some(screen)
    }

    // ========================================================================
    // Workspace Operations (O(1) lookups via index)
    // ========================================================================

    /// Get a workspace by ID. O(1) via index.
    #[must_use]
    pub fn get_workspace(&self, id: Uuid) -> Option<Workspace> {
        self.workspace_idx.get(&id).and_then(|&idx| self.workspaces.get(idx)).cloned()
    }

    /// Get a workspace by name. O(n) - no index for names.
    #[must_use]
    pub fn get_workspace_by_name(&self, name: &str) -> Option<Workspace> {
        self.workspaces.iter().find(|w| w.name == name).cloned()
    }

    /// Get the focused workspace. O(n) - no index for `is_focused`.
    #[must_use]
    pub fn get_focused_workspace(&self) -> Option<Workspace> {
        self.workspaces.iter().find(|w| w.is_focused).cloned()
    }

    /// Get all workspaces for a screen. O(n).
    #[must_use]
    pub fn get_workspaces_for_screen(&self, screen_id: u32) -> Vec<Workspace> {
        self.workspaces.iter().filter(|w| w.screen_id == screen_id).cloned().collect()
    }

    /// Get all visible workspaces. O(n).
    #[must_use]
    pub fn get_visible_workspaces(&self) -> Vec<Workspace> {
        self.workspaces.iter().filter(|w| w.is_visible).cloned().collect()
    }

    /// Get the index of a workspace by ID. O(1) via index.
    #[must_use]
    pub fn workspace_index(&self, id: Uuid) -> Option<usize> {
        self.workspace_idx.get(&id).copied()
    }

    /// Get the index of a workspace by name. O(n) - no index for names.
    #[must_use]
    pub fn workspace_index_by_name(&self, name: &str) -> Option<usize> {
        self.workspaces.iter().position(|w| w.name == name)
    }

    /// Insert or update a workspace. Maintains index.
    pub fn upsert_workspace(&mut self, workspace: Workspace) {
        let workspace_id = workspace.id;
        if let Some(&idx) = self.workspace_idx.get(&workspace_id) {
            self.workspaces.set(idx, workspace);
        } else {
            let idx = self.workspaces.len();
            self.workspaces.push_back(workspace);
            self.workspace_idx.insert(workspace_id, idx);
        }
    }

    /// Remove a workspace by ID. Maintains index.
    pub fn remove_workspace(&mut self, id: Uuid) -> Option<Workspace> {
        let idx = self.workspace_idx.remove(&id)?;
        let workspace = self.workspaces.remove(idx);

        // Update indices for all workspaces that shifted down
        for (&ws_id, stored_idx) in &mut self.workspace_idx {
            if *stored_idx > idx {
                *stored_idx -= 1;
            }
            debug_assert_eq!(
                self.workspaces.get(*stored_idx).map(|w| w.id),
                Some(ws_id),
                "workspace_idx out of sync after remove"
            );
        }

        Some(workspace)
    }

    /// Update a workspace in place. O(1) lookup via index.
    pub fn update_workspace<F>(&mut self, id: Uuid, f: F) -> bool
    where F: FnOnce(&mut Workspace) {
        let Some(&idx) = self.workspace_idx.get(&id) else {
            return false;
        };
        let mut workspace = self.workspaces.remove(idx);
        f(&mut workspace);
        self.workspaces.insert(idx, workspace);
        true
    }

    // ========================================================================
    // Window Operations (O(1) lookups via index)
    // ========================================================================

    /// Get a window by ID. O(1) via index.
    #[must_use]
    pub fn get_window(&self, id: u32) -> Option<Window> {
        self.window_idx.get(&id).and_then(|&idx| self.windows.get(idx)).cloned()
    }

    /// Get all windows for a workspace. O(n).
    #[must_use]
    pub fn get_windows_for_workspace(&self, workspace_id: Uuid) -> Vec<Window> {
        self.windows
            .iter()
            .filter(|w| w.workspace_id == workspace_id)
            .cloned()
            .collect()
    }

    /// Get all windows for an application (by PID). O(n).
    #[must_use]
    pub fn get_windows_for_pid(&self, pid: i32) -> Vec<Window> {
        self.windows.iter().filter(|w| w.pid == pid).cloned().collect()
    }

    /// Get all windows in a tab group. O(n).
    #[must_use]
    pub fn get_windows_in_tab_group(&self, tab_group_id: Uuid) -> Vec<Window> {
        self.windows
            .iter()
            .filter(|w| w.tab_group_id == Some(tab_group_id))
            .cloned()
            .collect()
    }

    /// Get the focused window. O(1) via index.
    #[must_use]
    pub fn get_focused_window(&self) -> Option<Window> {
        let focus = Observable::get(&self.focus);
        focus.focused_window_id.and_then(|id| self.get_window(id))
    }

    /// Get all layoutable windows for a workspace (excludes minimized, hidden, inactive tabs). O(n).
    #[must_use]
    pub fn get_layoutable_windows(&self, workspace_id: Uuid) -> Vec<Window> {
        self.windows
            .iter()
            .filter(|w| w.workspace_id == workspace_id && w.is_layoutable())
            .cloned()
            .collect()
    }

    /// Get the index of a window by ID. O(1) via index.
    #[must_use]
    pub fn window_index(&self, id: u32) -> Option<usize> { self.window_idx.get(&id).copied() }

    /// Insert or update a window. Maintains index.
    pub fn upsert_window(&mut self, window: Window) {
        let window_id = window.id;
        if let Some(&idx) = self.window_idx.get(&window_id) {
            self.windows.set(idx, window);
        } else {
            let idx = self.windows.len();
            self.windows.push_back(window);
            self.window_idx.insert(window_id, idx);
        }
    }

    /// Remove a window by ID. Maintains index.
    pub fn remove_window(&mut self, id: u32) -> Option<Window> {
        let idx = self.window_idx.remove(&id)?;
        let window = self.windows.remove(idx);

        // Update indices for all windows that shifted down
        for (&win_id, stored_idx) in &mut self.window_idx {
            if *stored_idx > idx {
                *stored_idx -= 1;
            }
            debug_assert_eq!(
                self.windows.get(*stored_idx).map(|w| w.id),
                Some(win_id),
                "window_idx out of sync after remove"
            );
        }

        Some(window)
    }

    /// Update a window in place. O(1) lookup via index.
    pub fn update_window<F>(&mut self, id: u32, f: F) -> bool
    where F: FnOnce(&mut Window) {
        let Some(&idx) = self.window_idx.get(&id) else {
            return false;
        };
        let mut window = self.windows.remove(idx);
        f(&mut window);
        self.windows.insert(idx, window);
        true
    }

    // ========================================================================
    // Focus Operations
    // ========================================================================

    /// Set the focused window, workspace, and screen.
    pub fn set_focus(
        &mut self,
        window_id: Option<u32>,
        workspace_id: Option<Uuid>,
        screen_id: Option<u32>,
    ) {
        Observable::set(&mut self.focus, FocusState {
            focused_window_id: window_id,
            focused_workspace_id: workspace_id,
            focused_screen_id: screen_id,
        });
    }

    /// Clear all focus.
    pub fn clear_focus(&mut self) { Observable::set(&mut self.focus, FocusState::new()); }

    /// Get the current focus state.
    #[must_use]
    pub fn get_focus_state(&self) -> FocusState { Observable::get(&self.focus).clone() }

    /// Update focus state with a closure.
    pub fn update_focus<F>(&mut self, f: F)
    where F: FnOnce(&mut FocusState) {
        let mut focus = Observable::get(&self.focus).clone();
        f(&mut focus);
        Observable::set(&mut self.focus, focus);
    }

    /// Set the focused workspace ID.
    pub fn set_focused_workspace(&mut self, workspace_id: Option<Uuid>) {
        self.update_focus(|focus| {
            focus.focused_workspace_id = workspace_id;
        });
    }

    /// Set the focused window ID.
    pub fn set_focused_window(&mut self, window_id: Option<u32>) {
        self.update_focus(|focus| {
            focus.focused_window_id = window_id;
        });
    }

    /// Set the focused screen ID.
    pub fn set_focused_screen(&mut self, screen_id: Option<u32>) {
        self.update_focus(|focus| {
            focus.focused_screen_id = screen_id;
        });
    }

    // ========================================================================
    // Enabled State
    // ========================================================================

    /// Check if tiling is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool { *Observable::get(&self.enabled) }

    /// Set whether tiling is enabled.
    pub fn set_enabled(&mut self, enabled: bool) { Observable::set(&mut self.enabled, enabled); }

    // ========================================================================
    // Focus History
    // ========================================================================

    /// Record the last focused window for a workspace.
    ///
    /// Call this before switching away from a workspace to remember which
    /// window was focused there.
    pub fn record_focus_history(&mut self, workspace_id: Uuid, window_id: u32) {
        self.focus_history.insert(workspace_id, window_id);
    }

    /// Get the last focused window for a workspace.
    ///
    /// Returns the window ID that was last focused in this workspace,
    /// if one was recorded and the window still exists.
    #[must_use]
    pub fn get_focus_history(&self, workspace_id: Uuid) -> Option<u32> {
        self.focus_history.get(&workspace_id).copied()
    }

    /// Remove a window from all focus history entries.
    ///
    /// Call this when a window is destroyed to clean up stale references.
    pub fn remove_window_from_focus_history(&mut self, window_id: u32) {
        self.focus_history.retain(|_, &mut id| id != window_id);
    }

    // ========================================================================
    // ID-Only Queries (Zero-Clone)
    // ========================================================================
    // These methods return only IDs, avoiding expensive clones of full objects.
    // Use these in hot paths where you only need to identify entities.

    /// Get all screen IDs. O(n) but no cloning of Screen objects.
    #[must_use]
    pub fn get_all_screen_ids(&self) -> Vec<u32> { self.screens.iter().map(|s| s.id).collect() }

    /// Get all workspace IDs. O(n) but no cloning of Workspace objects.
    #[must_use]
    pub fn get_all_workspace_ids(&self) -> Vec<Uuid> {
        self.workspaces.iter().map(|w| w.id).collect()
    }

    /// Get all window IDs. O(n) but no cloning of Window objects.
    #[must_use]
    pub fn get_all_window_ids(&self) -> Vec<u32> { self.windows.iter().map(|w| w.id).collect() }

    /// Get window IDs for a workspace. O(n) but no cloning of Window objects.
    #[must_use]
    pub fn get_window_ids_for_workspace(&self, workspace_id: Uuid) -> Vec<u32> {
        self.windows
            .iter()
            .filter(|w| w.workspace_id == workspace_id)
            .map(|w| w.id)
            .collect()
    }

    /// Get layoutable window IDs for a workspace. O(n) but no cloning.
    #[must_use]
    pub fn get_layoutable_window_ids(&self, workspace_id: Uuid) -> Vec<u32> {
        self.windows
            .iter()
            .filter(|w| w.workspace_id == workspace_id && w.is_layoutable())
            .map(|w| w.id)
            .collect()
    }

    /// Get visible workspace IDs. O(n) but no cloning.
    #[must_use]
    pub fn get_visible_workspace_ids(&self) -> Vec<Uuid> {
        self.workspaces.iter().filter(|w| w.is_visible).map(|w| w.id).collect()
    }

    /// Check if a window exists. O(1) via index.
    #[must_use]
    pub fn has_window(&self, id: u32) -> bool { self.window_idx.contains_key(&id) }

    /// Check if a workspace exists. O(1) via index.
    #[must_use]
    pub fn has_workspace(&self, id: Uuid) -> bool { self.workspace_idx.contains_key(&id) }

    /// Check if a screen exists. O(1) via index.
    #[must_use]
    pub fn has_screen(&self, id: u32) -> bool { self.screen_idx.contains_key(&id) }

    // ========================================================================
    // Utility Methods
    // ========================================================================

    /// Get counts of all entities.
    #[must_use]
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.screens.len(), self.workspaces.len(), self.windows.len())
    }

    /// Check if the state is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.screens.is_empty() && self.workspaces.is_empty() && self.windows.is_empty()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;
    use crate::modules::tiling::state::types::{LayoutType, Rect, WindowIdList};

    fn make_screen(id: u32, name: &str, is_main: bool) -> Screen {
        Screen {
            id,
            name: name.to_string(),
            frame: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            visible_frame: Rect::new(0.0, 25.0, 1920.0, 1055.0),
            scale_factor: 1.0,
            is_main,
            is_builtin: false,
            refresh_rate: 60.0,
        }
    }

    fn make_workspace(name: &str, screen_id: u32) -> Workspace {
        Workspace {
            id: Uuid::now_v7(),
            name: name.to_string(),
            screen_id,
            layout: LayoutType::Dwindle,
            is_visible: false,
            is_focused: false,
            window_ids: WindowIdList::new(),
            focused_window_index: None,
            split_ratios: Vec::new(),
            configured_screen: None,
        }
    }

    fn make_window(id: u32, workspace_id: Uuid) -> Window {
        Window {
            id,
            pid: 1000,
            app_id: "com.test.app".to_string(),
            app_name: "Test App".to_string(),
            title: format!("Window {id}"),
            frame: Rect::new(0.0, 0.0, 800.0, 600.0),
            minimum_size: None,
            inferred_minimum_size: None,
            expected_frame: None,
            workspace_id,
            is_minimized: false,
            is_fullscreen: false,
            is_hidden: false,
            is_floating: false,
            tab_group_id: None,
            is_active_tab: true,
            matched_rule: None,
        }
    }

    #[test]
    fn test_new_state_is_empty() {
        let state = TilingState::new();
        assert!(state.is_empty());
        assert_eq!(state.counts(), (0, 0, 0));
        assert!(state.is_enabled());
    }

    #[test]
    fn test_screen_operations() {
        let mut state = TilingState::new();

        let screen1 = make_screen(1, "Main", true);
        let screen2 = make_screen(2, "External", false);

        state.upsert_screen(screen1.clone());
        state.upsert_screen(screen2.clone());

        assert_eq!(state.screens.len(), 2);
        assert_eq!(state.get_screen(1), Some(screen1.clone()));
        assert_eq!(state.get_screen_by_name("External"), Some(screen2));
        assert_eq!(state.get_main_screen(), Some(screen1.clone()));

        // Update existing
        let mut updated = screen1;
        updated.refresh_rate = 120.0;
        state.upsert_screen(updated);
        assert_eq!(state.screens.len(), 2);
        assert_eq!(state.get_screen(1).unwrap().refresh_rate, 120.0);

        // Remove
        state.remove_screen(1);
        assert_eq!(state.screens.len(), 1);
        assert!(state.get_screen(1).is_none());
    }

    #[test]
    fn test_workspace_operations() {
        let mut state = TilingState::new();
        let screen = make_screen(1, "Main", true);
        state.upsert_screen(screen);

        let ws1 = make_workspace("dev", 1);
        let ws2 = make_workspace("web", 1);
        let ws1_id = ws1.id;

        state.upsert_workspace(ws1);
        state.upsert_workspace(ws2);

        assert_eq!(state.workspaces.len(), 2);
        assert!(state.get_workspace(ws1_id).is_some());
        assert_eq!(state.get_workspace_by_name("dev").unwrap().id, ws1_id);
        assert_eq!(state.get_workspaces_for_screen(1).len(), 2);

        // Update in place
        state.update_workspace(ws1_id, |ws| {
            ws.is_focused = true;
            ws.is_visible = true;
        });
        assert!(state.get_workspace(ws1_id).unwrap().is_focused);
        assert_eq!(state.get_focused_workspace().unwrap().id, ws1_id);
        assert_eq!(state.get_visible_workspaces().len(), 1);
    }

    #[test]
    fn test_window_operations() {
        let mut state = TilingState::new();
        let ws = make_workspace("dev", 1);
        let ws_id = ws.id;
        state.upsert_workspace(ws);

        let win1 = make_window(100, ws_id);
        let win2 = make_window(200, ws_id);

        state.upsert_window(win1);
        state.upsert_window(win2);

        assert_eq!(state.windows.len(), 2);
        assert!(state.get_window(100).is_some());
        assert_eq!(state.get_windows_for_workspace(ws_id).len(), 2);
        assert_eq!(state.get_windows_for_pid(1000).len(), 2);

        // Update in place
        state.update_window(100, |w| {
            w.is_minimized = true;
        });
        assert!(state.get_window(100).unwrap().is_minimized);

        // Layoutable windows excludes minimized
        assert_eq!(state.get_layoutable_windows(ws_id).len(), 1);
    }

    #[test]
    fn test_focus_operations() {
        let mut state = TilingState::new();
        let ws = make_workspace("dev", 1);
        let ws_id = ws.id;
        state.upsert_workspace(ws);

        let win = make_window(100, ws_id);
        state.upsert_window(win);

        state.set_focus(Some(100), Some(ws_id), Some(1));
        assert!(Observable::get(&state.focus).has_focus());
        assert_eq!(state.get_focused_window().unwrap().id, 100);

        state.clear_focus();
        assert!(!Observable::get(&state.focus).has_focus());
        assert!(state.get_focused_window().is_none());
    }

    #[test]
    fn test_tab_group_queries() {
        let mut state = TilingState::new();
        let ws = make_workspace("dev", 1);
        let ws_id = ws.id;
        state.upsert_workspace(ws);

        let tab_group_id = Uuid::now_v7();

        let mut win1 = make_window(100, ws_id);
        win1.tab_group_id = Some(tab_group_id);
        win1.is_active_tab = true;

        let mut win2 = make_window(200, ws_id);
        win2.tab_group_id = Some(tab_group_id);
        win2.is_active_tab = false;

        let win3 = make_window(300, ws_id);

        state.upsert_window(win1);
        state.upsert_window(win2);
        state.upsert_window(win3);

        let tab_windows = state.get_windows_in_tab_group(tab_group_id);
        assert_eq!(tab_windows.len(), 2);

        // Layoutable excludes inactive tabs
        let layoutable = state.get_layoutable_windows(ws_id);
        assert_eq!(layoutable.len(), 2); // win1 (active tab) and win3 (not tabbed)
    }

    #[test]
    fn test_enabled_state() {
        let mut state = TilingState::new();
        assert!(state.is_enabled());

        state.set_enabled(false);
        assert!(!state.is_enabled());

        state.set_enabled(true);
        assert!(state.is_enabled());
    }
}
