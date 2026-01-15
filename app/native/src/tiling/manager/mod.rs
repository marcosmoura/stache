//! Tiling window manager singleton.
//!
//! This module provides the central manager for the tiling system,
//! handling state management and coordination between screens,
//! workspaces, and windows.

mod helpers;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

// Re-export for use in other tiling modules during development
#[allow(unused_imports)]
pub use helpers::track_lock_time as debug_track_lock_time;
use helpers::{
    calculate_proportions_adjusting_adjacent, calculate_ratios_from_frames,
    cumulative_ratios_to_proportions, frames_approximately_equal, proportions_to_cumulative_ratios,
    track_lock_time,
};
use parking_lot::RwLock;
use tauri::{AppHandle, Emitter, Runtime};

use super::animation::{AnimationSystem, WindowTransition, get_interrupted_position};
use super::borders::{BorderManager, BorderState, get_border_manager};
use super::layout::{
    Gaps, LayoutResult, calculate_layout_with_gaps, calculate_layout_with_gaps_and_ratios,
};
use super::screen;
use super::state::{Rect, Screen, TilingState, TrackedWindow, Workspace};
use super::window::{
    WindowInfo, get_all_windows_including_hidden, set_window_frame, set_window_frames_by_id,
};
use super::workspace::{
    FocusHistory, assign_window_to_workspace, get_workspace_configs, hide_workspace_windows,
    should_ignore_window, show_workspace_windows,
};
use crate::config::{LayoutType, get_config};
use crate::events;

// ============================================================================
// Types
// ============================================================================

/// Information about a workspace switch operation.
#[derive(Debug, Clone)]
pub struct WorkspaceSwitchInfo {
    /// Name of the workspace switched to.
    pub workspace: String,
    /// Name of the screen the workspace is on.
    pub screen: String,
    /// Name of the previous workspace (if any).
    pub previous_workspace: Option<String>,
}

// ============================================================================
// Global Singleton
// ============================================================================

/// Global tiling manager instance.
static MANAGER: OnceLock<Arc<RwLock<TilingManager>>> = OnceLock::new();

/// Gets the global tiling manager instance.
///
/// Returns `None` if the manager hasn't been initialized yet.
#[must_use]
pub fn get_manager() -> Option<Arc<RwLock<TilingManager>>> { MANAGER.get().cloned() }

/// Initializes the global tiling manager.
///
/// This should be called once during application startup.
/// Subsequent calls will return `false` without reinitializing.
///
/// # Returns
///
/// `true` if initialization succeeded, `false` if already initialized or failed.
pub fn init_manager<R: Runtime>(app_handle: Option<AppHandle<R>>) -> bool {
    if MANAGER.get().is_some() {
        return false;
    }

    let manager = TilingManager::new();

    if MANAGER.set(Arc::new(RwLock::new(manager))).is_err() {
        eprintln!("stache: tiling: manager already initialized");
        return false;
    }

    // Initialize state (detect screens, create workspaces)
    if let Some(manager) = get_manager() {
        let mut mgr = manager.write();
        mgr.initialize();
        let enabled = mgr.is_enabled();
        drop(mgr); // Release lock before emitting event

        // Emit initialized event if we have an app handle
        if let Some(handle) = app_handle
            && let Err(e) = handle.emit(
                events::tiling::INITIALIZED,
                serde_json::json!({ "enabled": enabled }),
            )
        {
            eprintln!("stache: tiling: failed to emit initialized event: {e}");
        }
    }

    true
}

// ============================================================================
// Tiling Manager
// ============================================================================

use super::constants::timing::{
    FOCUS_COOLDOWN_MS, HIDE_SHOW_DELAY_MS, WORKSPACE_SWITCH_COOLDOWN_MS,
};

/// The central tiling window manager.
///
/// Manages screens, workspaces, and windows. Provides methods for
/// querying and manipulating the tiling state.
#[derive(Debug)]
pub struct TilingManager {
    /// Current state of the tiling system.
    state: TilingState,
    /// Whether the tiling system is enabled.
    enabled: bool,
    /// Whether the manager has completed initialization.
    /// Windows are positioned instantly (no animation) until this is true.
    initialized: bool,
    /// Focus history for workspace switching.
    focus_history: FocusHistory,
    /// Last programmatically focused window ID and when it was focused.
    /// Used to debounce focus events that arrive after we've already changed focus.
    last_programmatic_focus: Option<(u32, Instant)>,
    /// Last workspace switch timestamp.
    /// Used to debounce workspace switches triggered by focus events during a switch.
    last_workspace_switch: Option<Instant>,
    /// Animation system for smooth window transitions.
    animation_system: AnimationSystem,
    /// Cached gaps per screen (keyed by screen name).
    /// Rebuilt when screens change or on initialization.
    gaps_cache: HashMap<String, Gaps>,
}

impl TilingManager {
    /// Creates a new tiling manager.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: TilingState::new(),
            enabled: false,
            initialized: false,
            focus_history: FocusHistory::new(),
            last_programmatic_focus: None,
            last_workspace_switch: None,
            animation_system: AnimationSystem::from_config(),
            gaps_cache: HashMap::new(),
        }
    }

    /// Initializes the tiling manager state.
    ///
    /// This detects screens and creates workspaces from config.
    /// Note: Workspace visibility is NOT set here - it's set later in
    /// `set_initial_workspace_visibility()` after windows are tracked,
    /// so we can properly detect the focused window.
    pub fn initialize(&mut self) {
        // Check if tiling is enabled in config
        let config = get_config();
        self.enabled = config.tiling.is_enabled();

        if !self.enabled {
            return;
        }

        // Detect screens
        self.refresh_screens();

        // Create workspaces from config or defaults
        self.create_workspaces_from_config();

        // Note: set_initial_workspace_visibility() should be called later,
        // after windows are tracked and we can detect the focused window.
    }

    /// Returns whether the tiling system is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool { self.enabled }

    /// Marks the manager as initialized.
    ///
    /// After calling this, window positioning will use animations (if enabled in config)
    /// instead of instant positioning. This should be called at the end of the
    /// startup sequence, after initial layouts have been applied.
    pub fn mark_initialized(&mut self) {
        self.initialized = true;
        eprintln!("stache: tiling: manager initialized - animations enabled");
    }

    /// Returns a reference to the current state.
    #[must_use]
    pub const fn state(&self) -> &TilingState { &self.state }

    /// Returns a mutable reference to the current state.
    pub const fn state_mut(&mut self) -> &mut TilingState { &mut self.state }

    // ========================================================================
    // Screen Management
    // ========================================================================

    /// Refreshes the list of connected screens.
    pub fn refresh_screens(&mut self) {
        self.state.screens = screen::get_all_screens();

        // Update focused screen if needed
        if self.state.focused_screen_id.is_none()
            && let Some(main) = self.state.main_screen()
        {
            self.state.focused_screen_id = Some(main.id);
        }

        // Rebuild gaps cache for new screen configuration
        self.rebuild_gaps_cache();
    }

    /// Rebuilds the gaps cache for all screens.
    ///
    /// This computes and caches the resolved gap values for each screen,
    /// avoiding repeated calculations during layout application.
    fn rebuild_gaps_cache(&mut self) {
        let config = get_config();
        let bar_offset = f64::from(config.bar.height) + f64::from(config.bar.padding);

        self.gaps_cache.clear();

        for screen in &self.state.screens {
            let gaps =
                Gaps::from_config(&config.tiling.gaps, &screen.name, screen.is_main, bar_offset);
            self.gaps_cache.insert(screen.name.clone(), gaps);
        }
    }

    /// Gets the cached gaps for a screen.
    ///
    /// Falls back to computing gaps on cache miss (shouldn't happen in normal operation).
    fn get_gaps_for_screen(&self, screen: &Screen) -> Gaps {
        if let Some(gaps) = self.gaps_cache.get(&screen.name) {
            return *gaps;
        }

        // Cache miss - compute on demand (shouldn't happen normally)
        let config = get_config();
        let bar_offset = f64::from(config.bar.height) + f64::from(config.bar.padding);
        Gaps::from_config(&config.tiling.gaps, &screen.name, screen.is_main, bar_offset)
    }

    /// Gets all screens.
    #[must_use]
    pub fn get_screens(&self) -> &[Screen] { &self.state.screens }

    /// Gets a screen by ID.
    #[must_use]
    pub fn get_screen(&self, id: u32) -> Option<&Screen> { self.state.screen_by_id(id) }

    /// Gets a screen by name.
    #[must_use]
    pub fn get_screen_by_name(&self, name: &str) -> Option<&Screen> {
        self.state.screen_by_name(name)
    }

    /// Gets the main screen.
    #[must_use]
    pub fn get_main_screen(&self) -> Option<&Screen> { self.state.main_screen() }

    /// Gets the focused screen.
    #[must_use]
    pub fn get_focused_screen(&self) -> Option<&Screen> {
        self.state.focused_screen_id.and_then(|id| self.state.screen_by_id(id))
    }

    /// Handles a screen configuration change (screen connected/disconnected).
    ///
    /// This method:
    /// 1. Captures the old screen state
    /// 2. Refreshes the screen list
    /// 3. Identifies screens that were added or removed
    /// 4. For removed screens: moves windows to primary screen
    /// 5. For added screens: restores workspaces to their configured screens
    ///
    /// # Returns
    ///
    /// A tuple of (`screens_added`, `screens_removed`) counts.
    pub fn handle_screen_change(&mut self) -> (usize, usize) {
        // Capture old screen state
        let old_screen_ids: std::collections::HashSet<u32> =
            self.state.screens.iter().map(|s| s.id).collect();

        // Refresh screen list
        self.refresh_screens();

        // Identify new screens
        let new_screen_ids: std::collections::HashSet<u32> =
            self.state.screens.iter().map(|s| s.id).collect();

        let added_screens: Vec<u32> = new_screen_ids.difference(&old_screen_ids).copied().collect();
        let removed_screens: Vec<u32> =
            old_screen_ids.difference(&new_screen_ids).copied().collect();

        // Handle removed screens - move windows to primary
        if !removed_screens.is_empty() {
            self.handle_screens_removed(&removed_screens);
        }

        // Handle added screens - restore workspaces to configured screens
        if !added_screens.is_empty() {
            self.handle_screens_added();
        }

        (added_screens.len(), removed_screens.len())
    }

    /// Handles screens being removed (disconnected).
    ///
    /// For each workspace on a removed screen:
    /// 1. Reassign the workspace to the primary screen
    /// 2. Move all windows from that workspace to the primary screen
    /// 3. Apply layout on the primary screen
    fn handle_screens_removed(&mut self, removed_screen_ids: &[u32]) {
        let Some(main_screen) = self.state.main_screen().cloned() else {
            return;
        };

        let main_screen_id = main_screen.id;

        // Find workspaces on removed screens
        let affected_workspaces: Vec<String> = self
            .state
            .workspaces
            .iter()
            .filter(|ws| removed_screen_ids.contains(&ws.screen_id))
            .map(|ws| ws.name.clone())
            .collect();

        if affected_workspaces.is_empty() {
            return;
        }

        // Collect windows to move (window_id, workspace_name)
        let windows_to_move: Vec<(u32, String)> = affected_workspaces
            .iter()
            .flat_map(|ws_name| {
                self.state
                    .windows_for_workspace(ws_name)
                    .into_iter()
                    .map(|w| (w.id, ws_name.clone()))
            })
            .collect();

        // Reassign workspaces to main screen and mark them as NOT visible
        for ws_name in &affected_workspaces {
            if let Some(ws) = self.state.workspace_by_name_mut(ws_name) {
                ws.screen_id = main_screen_id;
                ws.is_visible = false;
            }
        }

        // Move windows to main screen (off-screen, since their workspaces are now hidden)
        for (window_id, _ws_name) in &windows_to_move {
            let _ = super::window::move_window_to_screen(*window_id, &main_screen, None);
        }

        // Update tracked window frames
        self.refresh_window_frames(&windows_to_move.iter().map(|(id, _)| *id).collect::<Vec<_>>());

        // Hide windows from migrated workspaces (they're now on hidden workspaces)
        for (window_id, _) in &windows_to_move {
            if let Err(e) = super::window::hide_window(*window_id) {
                eprintln!("stache: tiling: failed to hide window {window_id}: {e}");
            }
        }

        // Re-apply layout to the main screen's visible workspace
        if let Some(main_visible_ws) = self
            .state
            .workspaces
            .iter()
            .find(|ws| ws.screen_id == main_screen_id && ws.is_visible)
            .map(|ws| ws.name.clone())
        {
            self.apply_layout_forced(&main_visible_ws);
        }
    }

    /// Handles screens being added (connected).
    ///
    /// For each workspace that was configured for a now-available screen:
    /// 1. Reassign the workspace back to its configured screen
    /// 2. Move all windows from that workspace to the correct screen
    /// 3. Apply layout on the restored screen
    fn handle_screens_added(&mut self) {
        // Find workspaces that can be restored to their configured screens
        let workspaces_to_restore: Vec<(String, String, u32, u32)> = self
            .state
            .workspaces
            .iter()
            .filter_map(|ws| {
                // Skip workspaces without a configured screen
                if ws.configured_screen.is_empty() {
                    return None;
                }

                // Check if the configured screen is now available
                let configured_screen = self.state.screen_by_name(&ws.configured_screen)?;
                let configured_screen_id = configured_screen.id;

                // Only restore if currently on a different screen
                if ws.screen_id == configured_screen_id {
                    None
                } else {
                    Some((
                        ws.name.clone(),
                        ws.configured_screen.clone(),
                        ws.screen_id,
                        configured_screen_id,
                    ))
                }
            })
            .collect();

        if workspaces_to_restore.is_empty() {
            return;
        }

        for (ws_name, _configured_screen_name, _old_screen_id, new_screen_id) in
            &workspaces_to_restore
        {
            // Get the target screen info
            let Some(target_screen) = self.state.screen_by_id(*new_screen_id).cloned() else {
                continue;
            };

            // Collect windows to move
            let window_ids: Vec<u32> =
                self.state.windows_for_workspace(ws_name).iter().map(|w| w.id).collect();

            // Reassign workspace to configured screen
            if let Some(ws) = self.state.workspace_by_name_mut(ws_name) {
                ws.screen_id = *new_screen_id;
            }

            // Move windows to the restored screen
            for window_id in &window_ids {
                let _ = super::window::move_window_to_screen(*window_id, &target_screen, None);
            }

            // Update tracked window frames
            self.refresh_window_frames(&window_ids);

            // Apply layout if workspace is visible
            if let Some(ws) = self.state.workspace_by_name(ws_name)
                && ws.is_visible
            {
                self.apply_layout_forced(ws_name);
            }
        }

        // Ensure each screen has a visible workspace
        self.ensure_workspace_visibility_after_restore();
    }

    /// Ensures each screen has at least one visible workspace after restoration.
    ///
    /// For each screen without a visible workspace:
    /// 1. Mark the first workspace on that screen as visible
    /// 2. Show all windows in that workspace
    /// 3. Apply the layout
    fn ensure_workspace_visibility_after_restore(&mut self) {
        let screen_ids: Vec<u32> = self.state.screens.iter().map(|s| s.id).collect();

        // Collect workspaces that need to be made visible
        let workspaces_to_show: Vec<String> = screen_ids
            .iter()
            .filter_map(|&screen_id| {
                // Check if this screen has a visible workspace
                let has_visible = self
                    .state
                    .workspaces
                    .iter()
                    .any(|ws| ws.screen_id == screen_id && ws.is_visible);

                if has_visible {
                    return None;
                }

                // Find the first workspace on this screen
                self.state
                    .workspaces
                    .iter()
                    .find(|ws| ws.screen_id == screen_id)
                    .map(|ws| ws.name.clone())
            })
            .collect();

        // Make each workspace visible
        for ws_name in &workspaces_to_show {
            if let Some(ws) = self.state.workspace_by_name_mut(ws_name) {
                ws.is_visible = true;
            }

            // Show all windows in this workspace
            let window_ids: Vec<u32> =
                self.state.windows_for_workspace(ws_name).iter().map(|w| w.id).collect();

            for window_id in &window_ids {
                if let Err(e) = super::window::show_window(*window_id) {
                    eprintln!("stache: tiling: failed to show window {window_id}: {e}");
                }
            }

            // Apply layout
            self.apply_layout_forced(ws_name);
        }
    }

    /// Refreshes tracked window frames from their current on-screen positions.
    fn refresh_window_frames(&mut self, window_ids: &[u32]) {
        use super::window::get_all_windows;
        let current_windows = get_all_windows();

        for window_id in window_ids {
            if let Some(current) = current_windows.iter().find(|w| w.id == *window_id) {
                self.update_window_frame(*window_id, current.frame);
            }
        }
    }

    // ========================================================================
    // Workspace Management
    // ========================================================================

    /// Creates workspaces from configuration.
    fn create_workspaces_from_config(&mut self) {
        let config = get_config();
        let tiling_config = &config.tiling;

        if tiling_config.workspaces.is_empty() {
            // Create default workspaces (one per screen)
            self.create_default_workspaces();
        } else {
            // Create workspaces from config
            for ws_config in &tiling_config.workspaces {
                // Find the screen for this workspace
                // If the configured screen doesn't exist, fall back to main screen
                let screen_id = self
                    .resolve_screen_name(&ws_config.screen)
                    .or_else(|| self.state.main_screen().map(|s| s.id));

                if let Some(screen_id) = screen_id {
                    let workspace = Workspace::new_with_screen(
                        ws_config.name.clone(),
                        screen_id,
                        ws_config.screen.clone(),
                        ws_config.layout,
                    );
                    self.state.add_workspace(workspace);
                }
            }
        }

        // Ensure each screen has at least one workspace
        self.ensure_screen_workspaces();
    }

    /// Creates a default workspace for each screen.
    fn create_default_workspaces(&mut self) {
        let default_layout = LayoutType::default();

        // Collect screen info first to avoid borrow issues
        let screen_info: Vec<(usize, u32)> =
            self.state.screens.iter().enumerate().map(|(i, s)| (i, s.id)).collect();

        for (i, screen_id) in screen_info {
            let name = format!("workspace-{}", i + 1);
            let workspace = Workspace::new(name, screen_id, default_layout);
            self.state.add_workspace(workspace);
        }
    }

    /// Ensures each screen has at least one workspace.
    fn ensure_screen_workspaces(&mut self) {
        let default_layout = LayoutType::default();

        let screen_ids: Vec<u32> = self.state.screens.iter().map(|s| s.id).collect();

        for screen_id in screen_ids {
            let has_workspace = self.state.workspaces.iter().any(|w| w.screen_id == screen_id);

            if !has_workspace {
                let name = format!("default-{screen_id}");
                let workspace = Workspace::new(name, screen_id, default_layout);
                self.state.add_workspace(workspace);
            }
        }
    }

    /// Resolves a screen name to a screen ID.
    fn resolve_screen_name(&self, name: &str) -> Option<u32> {
        self.state.screen_by_name(name).map(|s| s.id)
    }

    /// Sets the initial workspace visibility based on the focused window.
    ///
    /// This should be called AFTER windows are tracked so we can properly
    /// detect which window is focused.
    ///
    /// # Behavior
    ///
    /// - If a focused window is found and tracked:
    ///   - The workspace containing that window is set as visible/focused on its screen
    ///   - For all other screens, the first workspace (in config order) is set as visible
    /// - If no focused window is found:
    ///   - The first workspace on each screen is set as visible
    ///   - The first workspace on the main screen is focused
    ///
    /// # Arguments
    ///
    /// * `focused_window_id` - Optional ID of the currently focused window
    pub fn set_initial_workspace_visibility(&mut self, focused_window_id: Option<u32>) {
        // Collect screen IDs
        let screen_ids: Vec<u32> = self.state.screens.iter().map(|s| s.id).collect();

        // Find the workspace containing the focused window (if any)
        let focused_workspace_info: Option<(String, u32)> =
            focused_window_id.and_then(|window_id| {
                self.state.window_by_id(window_id).and_then(|window| {
                    self.state
                        .workspace_by_name(&window.workspace_name)
                        .map(|ws| (ws.name.clone(), ws.screen_id))
                })
            });

        // Track which screen has the focused workspace
        let focused_screen_id = focused_workspace_info.as_ref().map(|(_, screen_id)| *screen_id);

        // Build a map of screen_id -> workspace_name to show
        // (collect all the info we need before mutating)
        let mut workspaces_to_show: Vec<(u32, String)> = Vec::new();

        for screen_id in &screen_ids {
            let workspace_name = if Some(*screen_id) == focused_screen_id {
                // This screen has the focused window - use its workspace
                focused_workspace_info.as_ref().map(|(name, _)| name.clone())
            } else {
                // Use the first workspace on this screen (config order)
                self.state
                    .workspaces
                    .iter()
                    .find(|w| w.screen_id == *screen_id)
                    .map(|w| w.name.clone())
            };

            if let Some(name) = workspace_name {
                workspaces_to_show.push((*screen_id, name));
            }
        }

        // Now apply visibility (no more immutable borrows active)
        for (screen_id, ws_name) in &workspaces_to_show {
            for ws in &mut self.state.workspaces {
                if ws.screen_id == *screen_id {
                    ws.is_visible = ws.name.eq_ignore_ascii_case(ws_name);
                }
            }
        }

        // Set focus to the appropriate workspace
        if let Some((focused_ws_name, focused_ws_screen_id)) = focused_workspace_info {
            // Focus the workspace containing the focused window
            for ws in &mut self.state.workspaces {
                ws.is_focused = ws.name.eq_ignore_ascii_case(&focused_ws_name);
            }
            self.state.focused_workspace = Some(focused_ws_name);
            self.state.focused_screen_id = Some(focused_ws_screen_id);
        } else {
            // No focused window - focus the first workspace on the main screen
            let main_screen_id = self.state.main_screen().map(|s| s.id);

            if let Some(main_id) = main_screen_id {
                if let Some(ws) = self
                    .state
                    .workspaces
                    .iter_mut()
                    .find(|w| w.screen_id == main_id && w.is_visible)
                {
                    ws.is_focused = true;
                    self.state.focused_workspace = Some(ws.name.clone());
                }
                self.state.focused_screen_id = Some(main_id);
            }
        }
    }

    /// Gets all workspaces.
    #[must_use]
    pub fn get_workspaces(&self) -> &[Workspace] { &self.state.workspaces }

    /// Gets workspaces for a specific screen.
    #[must_use]
    pub fn get_workspaces_for_screen(&self, screen_id: u32) -> Vec<&Workspace> {
        self.state.workspaces_for_screen(screen_id)
    }

    /// Gets the focused workspace.
    #[must_use]
    pub fn get_focused_workspace(&self) -> Option<&Workspace> { self.state.focused_workspace() }

    /// Sets the focused workspace by name.
    ///
    /// This is used during initialization to set the focused workspace before
    /// tracking windows, so that non-matching windows can be assigned to the
    /// correct fallback workspace.
    pub fn set_focused_workspace_name(&mut self, name: &str) {
        self.state.focused_workspace = Some(name.to_string());
    }

    /// Gets a workspace by name.
    #[must_use]
    pub fn get_workspace(&self, name: &str) -> Option<&Workspace> {
        self.state.workspace_by_name(name)
    }

    /// Switches to a workspace by name.
    ///
    /// This hides all windows in the current workspace and shows all windows
    /// in the target workspace. Only affects the screen containing the target
    /// workspace.
    ///
    /// # Returns
    ///
    /// `Some(WorkspaceSwitchInfo)` if the switch was successful, `None` if the
    /// workspace doesn't exist.
    #[allow(clippy::too_many_lines)]
    pub fn switch_workspace(&mut self, name: &str) -> Option<WorkspaceSwitchInfo> {
        // Find the target workspace
        let target_ws = match self.state.workspace_by_name(name) {
            Some(ws) => ws.clone(),
            None => return None,
        };

        let screen_id = target_ws.screen_id;

        // Get the screen name for the result
        let screen_name = self
            .state
            .screen_by_id(screen_id)
            .map_or_else(|| format!("screen-{screen_id}"), |s| s.name.clone());

        // Find the currently visible workspace on the same screen
        let current_ws_name = self
            .state
            .workspaces
            .iter()
            .find(|w| w.screen_id == screen_id && w.is_visible)
            .map(|w| w.name.clone());

        // If already on this workspace, still return success info
        if target_ws.is_visible && target_ws.is_focused {
            return Some(WorkspaceSwitchInfo {
                workspace: name.to_string(),
                screen: screen_name,
                previous_workspace: current_ws_name,
            });
        }

        // Collect PIDs to show (we need this for filtering hide list)
        let pids_to_show: std::collections::HashSet<i32> =
            self.state.windows_for_workspace(name).iter().map(|w| w.pid).collect();

        // Hide windows in current workspace (if different from target)
        if let Some(ref current_name) = current_ws_name
            && current_name != name
        {
            // Save focus state before hiding
            if let Some(focused_id) = self.get_focused_window_in_workspace(current_name) {
                self.focus_history.record(current_name, focused_id);
            }

            // Hide apps that ONLY have windows in the source workspace.
            // Apps with windows in both source and target workspaces are NOT hidden,
            // because hiding at the app level would hide their windows in the target
            // workspace too. Their source windows will simply remain visible in the
            // background until the user switches back.
            let all_source_windows = self.state.windows_for_workspace(current_name);

            let windows_to_hide: Vec<_> = all_source_windows
                .into_iter()
                .filter(|w| !pids_to_show.contains(&w.pid))
                .collect();

            if !windows_to_hide.is_empty() {
                let (hidden, _failures) = hide_workspace_windows(&windows_to_hide);
                if hidden > 0 {
                    // Give macOS time to process the hide operation before showing new windows.
                    std::thread::sleep(std::time::Duration::from_millis(HIDE_SHOW_DELAY_MS));
                }
            }

            // Hide borders for the current workspace
            Self::hide_borders_for_workspace(current_name);
        }

        // Strategy for minimal flicker:
        // 1. Pre-position windows while hidden (AX API works on hidden windows)
        // 2. Show windows (they should appear in correct positions)
        // 3. Re-apply layout immediately (in case macOS overrode positions during unhide)

        // Step 1: Pre-position windows while hidden
        self.apply_layout_forced(name);

        // Step 2: Show windows
        let windows_to_show: Vec<&TrackedWindow> = self.state.windows_for_workspace(name);
        let _ = show_workspace_windows(&windows_to_show);

        // Step 3: Immediately re-apply layout (no delay) to fix any position overrides
        self.apply_layout_forced(name);

        // Show borders for the target workspace
        Self::show_borders_for_workspace(name);

        // Note: Border colors are NOT updated here - they are only updated
        // when windows are focused (in update_focus_border_states)

        // Update workspace visibility
        for ws in &mut self.state.workspaces {
            if ws.screen_id == screen_id {
                ws.is_visible = ws.name.eq_ignore_ascii_case(name);
                ws.is_focused = ws.name.eq_ignore_ascii_case(name);
            } else {
                // Clear focus from workspaces on other screens
                ws.is_focused = false;
            }
        }

        // Update focused workspace
        self.state.focused_workspace = Some(name.to_string());
        self.state.focused_screen_id = Some(screen_id);

        // Record this workspace switch to debounce subsequent focus events
        self.record_workspace_switch();

        // Focus a window in the target workspace to ensure proper focus
        // Try focus history first, then fall back to the first window
        let window_to_focus = self.focus_history.get(name).or_else(|| {
            self.state.workspace_by_name(name).and_then(|ws| ws.window_ids.first().copied())
        });

        if let Some(window_id) = window_to_focus {
            // Invalidate CG cache since we just showed windows
            super::window::invalidate_cg_window_list_cache();

            // Focus the window
            if super::window::focus_window(window_id).is_ok() {
                // Update focused window index
                if let Some(ws) = self.state.workspace_by_name_mut(name)
                    && let Some(idx) = ws.window_ids.iter().position(|&id| id == window_id)
                {
                    ws.focused_window_index = Some(idx);
                }
                // Record programmatic focus to debounce incoming focus events
                self.last_programmatic_focus = Some((window_id, Instant::now()));
            }
        }

        Some(WorkspaceSwitchInfo {
            workspace: name.to_string(),
            screen: screen_name,
            previous_workspace: current_ws_name,
        })
    }

    /// Gets the focused window ID in a workspace.
    fn get_focused_window_in_workspace(&self, workspace_name: &str) -> Option<u32> {
        self.state
            .workspace_by_name(workspace_name)
            .and_then(Workspace::focused_window_id)
    }

    // ========================================================================
    // Layout Management
    // ========================================================================

    /// Applies the layout to a workspace.
    ///
    /// Calculates window positions based on the workspace's layout type and
    /// moves windows to their calculated positions.
    ///
    /// # Arguments
    ///
    /// * `workspace_name` - Name of the workspace to apply layout to
    ///
    /// # Returns
    ///
    /// Number of windows that were repositioned.
    #[must_use]
    pub fn apply_layout(&self, workspace_name: &str) -> usize {
        let Some(workspace) = self.state.workspace_by_name(workspace_name) else {
            return 0;
        };

        // Get the screen's visible frame for this workspace
        let Some(screen) = self.state.screen_by_id(workspace.screen_id) else {
            return 0;
        };

        // Floating layout doesn't reposition windows
        if workspace.layout == LayoutType::Floating {
            return 0;
        }

        // Get window IDs in order
        let window_ids: Vec<u32> = workspace.window_ids.clone();
        if window_ids.is_empty() {
            return 0;
        }

        // Get config values
        let config = get_config();
        let master_ratio = f64::from(config.tiling.master.ratio) / 100.0;

        // Use cached gaps for this screen
        let gaps = self.get_gaps_for_screen(screen);

        // Calculate the layout with gaps
        let layout_result = calculate_layout_with_gaps(
            workspace.layout,
            &window_ids,
            &screen.visible_frame,
            master_ratio,
            &gaps,
        );

        // Apply the calculated frames to windows
        let mut repositioned = 0;
        for (window_id, frame) in layout_result {
            if set_window_frame(window_id, &frame).is_ok() {
                repositioned += 1;

                // Update the tracked window's frame in state
                // Note: We can't mutate here since self is &self
                // The frame update will happen through window events
            }
        }

        repositioned
    }

    /// Applies layout to a workspace, with mutable access to update state.
    ///
    /// This version updates the tracked window frames in state after applying.
    /// Only windows that have actually changed position/size will be repositioned
    /// to avoid unnecessary AX API calls and reduce flickering.
    ///
    /// Use `force = true` when switching workspaces to ensure all windows are
    /// repositioned regardless of tracked state (which may be stale).
    #[allow(clippy::too_many_lines)]
    pub fn apply_layout_mut(&mut self, workspace_name: &str) -> usize {
        self.apply_layout_internal(workspace_name, false)
    }

    /// Applies layout to a workspace, forcing repositioning of all windows.
    ///
    /// This bypasses the diff check and repositions all windows unconditionally.
    /// Use this when switching workspaces where tracked frames may be stale.
    pub fn apply_layout_forced(&mut self, workspace_name: &str) -> usize {
        self.apply_layout_internal(workspace_name, true)
    }

    /// Internal implementation of layout application.
    #[allow(clippy::too_many_lines)]
    fn apply_layout_internal(&mut self, workspace_name: &str, force: bool) -> usize {
        let Some(workspace) = self.state.workspace_by_name(workspace_name) else {
            return 0;
        };

        // Get the screen's visible frame for this workspace
        let Some(screen) = self.state.screen_by_id(workspace.screen_id) else {
            return 0;
        };

        // Floating layout doesn't reposition windows
        if workspace.layout == LayoutType::Floating {
            return 0;
        }

        // Get window IDs in order
        let window_ids: Vec<u32> = workspace.window_ids.clone();
        if window_ids.is_empty() {
            return 0;
        }

        // Get config values
        let config = get_config();
        let master_ratio = f64::from(config.tiling.master.ratio) / 100.0;
        let layout_type = workspace.layout;
        let split_ratios = workspace.split_ratios.clone();

        // Use cached gaps for this screen
        let gaps = self.get_gaps_for_screen(screen);

        // Compute layout input hash for cache validation
        let layout_hash = crate::tiling::state::compute_layout_hash(
            layout_type,
            &window_ids,
            &screen.visible_frame,
            master_ratio,
            &split_ratios,
            gaps.compute_hash(),
        );

        // Check cache - if valid and not forcing, use cached positions
        let cached_positions = workspace.layout_cache.is_valid(layout_hash);
        let layout_result: LayoutResult = if cached_positions && !force {
            workspace.layout_cache.positions.clone()
        } else {
            // Calculate the layout with gaps and custom ratios
            let result = calculate_layout_with_gaps_and_ratios(
                layout_type,
                &window_ids,
                &screen.visible_frame,
                master_ratio,
                &gaps,
                &split_ratios,
                config.tiling.master.position,
            );

            // Update cache with new results (need mutable access)
            if let Some(ws) = self.state.workspace_by_name_mut(workspace_name) {
                ws.layout_cache.update(layout_hash, result.clone());
            }

            result
        };

        // Build a map of window_id -> (pid, current_frame) from tracked windows
        let window_info: std::collections::HashMap<u32, (i32, Rect)> =
            self.state.windows.iter().map(|w| (w.id, (w.pid, w.frame))).collect();

        // Calculate which windows need repositioning
        // If force=true, reposition all windows (used after workspace switch)
        // If force=false, only reposition windows that have actually moved (reduces flicker)
        let windows_to_reposition: Vec<(u32, Rect, i32, Rect)> = layout_result
            .into_iter()
            .filter_map(|(window_id, new_frame)| {
                let (pid, current_frame) = window_info.get(&window_id)?;

                if force {
                    // Force mode: always reposition
                    Some((window_id, new_frame, *pid, *current_frame))
                } else {
                    // Diff mode: only reposition if changed (threshold: 2 pixels)
                    let needs_reposition =
                        !frames_approximately_equal(current_frame, &new_frame, 2.0);

                    if needs_reposition {
                        Some((window_id, new_frame, *pid, *current_frame))
                    } else {
                        None
                    }
                }
            })
            .collect();

        if windows_to_reposition.is_empty() {
            return 0;
        }

        // Position windows - during initialization, use instant positioning (no animation)
        // to avoid windows visibly sliding to their initial positions on startup.
        // After initialization, the animation system decides whether to animate based on config.
        let repositioned = if self.initialized {
            // Build window transitions for animation
            // If a previous animation was interrupted, use the interrupted position as the
            // starting point instead of the tracked frame (which may be stale)
            let transitions: Vec<WindowTransition> = windows_to_reposition
                .iter()
                .map(|(window_id, new_frame, _, current_frame)| {
                    let from = get_interrupted_position(*window_id).unwrap_or(*current_frame);
                    WindowTransition::new(*window_id, from, *new_frame)
                })
                .collect();

            self.animation_system.animate(transitions)
        } else {
            // Instant positioning during startup - no animation
            let window_frames: Vec<(u32, Rect)> = windows_to_reposition
                .iter()
                .map(|(window_id, new_frame, _, _)| (*window_id, *new_frame))
                .collect();

            set_window_frames_by_id(&window_frames)
        };

        // Update tracked frames for all windows we attempted to reposition
        for (window_id, new_frame, _, _) in &windows_to_reposition {
            self.update_window_frame(*window_id, *new_frame);
        }

        repositioned
    }

    /// Changes the layout of a workspace and applies it.
    ///
    /// # Arguments
    ///
    /// * `workspace_name` - Name of the workspace
    /// * `layout` - New layout type
    ///
    /// # Returns
    ///
    /// `true` if the layout was changed and applied.
    pub fn set_workspace_layout(&mut self, workspace_name: &str, layout: LayoutType) -> bool {
        // Update the workspace's layout
        let Some(workspace) = self.state.workspace_by_name_mut(workspace_name) else {
            return false;
        };

        workspace.layout = layout;
        // Clear custom ratios and layout cache when layout changes
        workspace.split_ratios.clear();
        workspace.layout_cache.invalidate();

        // Get window IDs before applying layout
        let window_ids: Vec<u32> = workspace.window_ids.clone();
        let focused_idx = workspace.focused_window_index;

        // Apply the new layout
        self.apply_layout_mut(workspace_name);

        // Update border states for all windows in the workspace
        Self::update_all_border_states_for_layout(&window_ids, focused_idx, layout);

        true
    }

    /// Updates border states for all windows when layout changes.
    ///
    /// Note: This only updates the internal state, NOT the `JankyBorders` colors.
    /// Border colors are only updated through focus events (`update_focus_border_states`).
    ///
    /// Dispatched to main thread because border operations require it.
    fn update_all_border_states_for_layout(
        window_ids: &[u32],
        focused_idx: Option<usize>,
        layout: LayoutType,
    ) {
        if get_border_manager().is_none() {
            return;
        }

        let window_ids = window_ids.to_vec();
        let is_monocle = layout == LayoutType::Monocle;
        let is_floating = layout == LayoutType::Floating;

        crate::utils::thread::dispatch_on_main(move || {
            let Some(border_manager) = get_border_manager() else {
                return;
            };
            let mut manager = border_manager.write();

            for (idx, window_id) in window_ids.iter().enumerate() {
                let state = match layout {
                    LayoutType::Monocle => BorderState::Monocle,
                    LayoutType::Floating => BorderState::Floating,
                    _ => {
                        if focused_idx == Some(idx) {
                            BorderState::Focused
                        } else {
                            BorderState::Unfocused
                        }
                    }
                };
                manager.set_window_state_no_color_update(*window_id, state);
            }

            // Drop manager before calling janky to avoid holding lock unnecessarily
            drop(manager);

            // Update colors after all states are set
            // This ensures the colors reflect the new layout
            super::borders::janky::update_colors_for_state(is_monocle, is_floating);
        });
    }

    /// Balances a workspace by resetting all windows to equal proportions.
    ///
    /// This clears any custom split ratios from resize operations and
    /// re-applies the layout with default proportions.
    ///
    /// # Arguments
    ///
    /// * `workspace_name` - Name of the workspace to balance
    ///
    /// # Returns
    ///
    /// Number of windows that were repositioned.
    pub fn balance_workspace(&mut self, workspace_name: &str) -> usize {
        // Clear custom ratios and cache to reset to default proportions
        if let Some(workspace) = self.state.workspace_by_name_mut(workspace_name) {
            workspace.split_ratios.clear();
            workspace.layout_cache.invalidate();
        } else {
            return 0;
        }

        // Force-apply layout to reposition all windows with default ratios
        self.apply_layout_forced(workspace_name)
    }

    /// Calculates split ratios for a specific resized window and applies them.
    ///
    /// This is called when the user finishes resizing a window. It:
    /// 1. Keeps the resized window's new size as its proportion
    /// 2. Only adjusts the adjacent window that shares the resized edge
    /// 3. Preserves all other windows at their current sizes
    ///
    /// # Arguments
    ///
    /// * `workspace_name` - Name of the workspace
    /// * `resized_window_id` - ID of the window that was resized
    /// * `old_frame` - The window's frame before resizing
    /// * `new_frame` - The resized window's new frame
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    pub fn calculate_and_apply_ratios_for_window(
        &mut self,
        workspace_name: &str,
        resized_window_id: u32,
        old_frame: Rect,
        new_frame: Rect,
    ) {
        let Some(workspace) = self.state.workspace_by_name(workspace_name) else {
            return;
        };

        // Only calculate ratios for split layouts
        let is_split_layout = matches!(
            workspace.layout,
            LayoutType::Split | LayoutType::SplitVertical | LayoutType::SplitHorizontal
        );

        if !is_split_layout {
            // Still reapply layout to snap windows back
            self.apply_layout_forced(workspace_name);
            return;
        }

        let window_ids: Vec<u32> = workspace.window_ids.clone();
        let layout_type = workspace.layout;

        if window_ids.len() < 2 {
            // No ratios needed for single window
            return;
        }

        // Find the index of the resized window
        let Some(resized_idx) = window_ids.iter().position(|&id| id == resized_window_id) else {
            self.apply_layout_forced(workspace_name);
            return;
        };

        // Get the screen for this workspace
        let screen_id = workspace.screen_id;
        let Some(screen) = self.state.screen_by_id(screen_id) else {
            return;
        };
        let screen_frame = screen.visible_frame;
        let is_portrait = screen_frame.height > screen_frame.width;

        // Determine split direction
        let is_vertical = match layout_type {
            LayoutType::SplitVertical => true,
            LayoutType::SplitHorizontal => false,
            LayoutType::Split => is_portrait,
            _ => return,
        };

        // Use cached gaps for this screen
        let gaps = self.get_gaps_for_screen(screen);

        let count = window_ids.len();
        let available_space = if is_vertical {
            let total_gap = gaps.inner_v * (count - 1) as f64;
            screen_frame.height - total_gap
        } else {
            let total_gap = gaps.inner_h * (count - 1) as f64;
            screen_frame.width - total_gap
        };

        if available_space <= 0.0 {
            return;
        }

        // Get current proportions from existing ratios (or equal if none)
        let current_ratios = workspace.split_ratios.clone();
        let current_proportions = cumulative_ratios_to_proportions(&current_ratios, count);

        // Calculate old and new sizes
        let old_size = if is_vertical {
            old_frame.height
        } else {
            old_frame.width
        };
        let new_size = if is_vertical {
            new_frame.height
        } else {
            new_frame.width
        };

        // Calculate the size delta
        let size_delta = new_size - old_size;

        // Determine which edge moved by comparing positions
        let (old_start, old_end) = if is_vertical {
            (old_frame.y, old_frame.y + old_frame.height)
        } else {
            (old_frame.x, old_frame.x + old_frame.width)
        };

        let (new_start, new_end) = if is_vertical {
            (new_frame.y, new_frame.y + new_frame.height)
        } else {
            (new_frame.x, new_frame.x + new_frame.width)
        };

        let start_moved = (new_start - old_start).abs() > 2.0;
        let end_moved = (new_end - old_end).abs() > 2.0;

        // Determine which adjacent window to adjust
        // If left/top edge moved, adjust the previous window
        // If right/bottom edge moved, adjust the next window
        let adjacent_idx = if start_moved && resized_idx > 0 {
            resized_idx - 1
        } else if end_moved && resized_idx < count - 1 {
            resized_idx + 1
        } else if resized_idx > 0 {
            // Default to previous if available
            resized_idx - 1
        } else {
            // Default to next
            resized_idx + 1
        };

        // Calculate new proportions: only change the resized window and its adjacent
        let new_proportions = calculate_proportions_adjusting_adjacent(
            &current_proportions,
            resized_idx,
            adjacent_idx,
            size_delta / available_space,
        );

        // Convert back to cumulative ratios
        let ratios = proportions_to_cumulative_ratios(&new_proportions);

        // Update the workspace's ratios and invalidate cache
        if let Some(ws) = self.state.workspace_by_name_mut(workspace_name) {
            ws.split_ratios = ratios.into();
            ws.layout_cache.invalidate();
        }

        // Reapply layout with new ratios
        self.apply_layout_forced(workspace_name);
    }

    /// Sets custom split ratios for a workspace.
    ///
    /// # Arguments
    ///
    /// * `workspace_name` - Name of the workspace
    /// * `ratios` - New split ratios (cumulative 0.0-1.0)
    pub fn set_workspace_ratios(&mut self, workspace_name: &str, ratios: Vec<f64>) {
        if let Some(ws) = self.state.workspace_by_name_mut(workspace_name) {
            ws.split_ratios = ratios.into();
            ws.layout_cache.invalidate();
        }
    }

    /// Clears custom split ratios for a workspace, reverting to equal splits.
    pub fn clear_workspace_ratios(&mut self, workspace_name: &str) {
        if let Some(ws) = self.state.workspace_by_name_mut(workspace_name) {
            ws.split_ratios.clear();
            ws.layout_cache.invalidate();
        }
    }

    // ========================================================================
    // Window Management
    // ========================================================================

    /// Tracks all windows including hidden/minimized ones.
    ///
    /// Called during initialization to populate the window list.
    /// Uses `get_all_windows_including_hidden()` to ensure windows from
    /// hidden apps are also tracked.
    pub fn track_existing_windows(&mut self) {
        let windows = get_all_windows_including_hidden();
        let ignore_rules = get_config().tiling.ignore.clone();
        let workspace_configs = get_workspace_configs();

        // Get the focused workspace name for fallback
        let fallback_workspace = self
            .state
            .focused_workspace
            .clone()
            .unwrap_or_else(|| "workspace-1".to_string());

        for window_info in windows {
            // Skip ignored windows
            if should_ignore_window(&window_info, &ignore_rules) {
                eprintln!(
                    "stache: tiling: DEBUG ignoring window '{}' from app '{}'",
                    window_info.title, window_info.app_name
                );
                continue;
            }
            eprintln!(
                "stache: tiling: DEBUG tracking window '{}' from app '{}' (bundle: {})",
                window_info.title, window_info.app_name, window_info.bundle_id
            );

            // Skip windows without AX elements - these are "phantom" windows
            if !window_info.has_ax_element() {
                continue;
            }

            // Assign to workspace
            let assignment =
                assign_window_to_workspace(&window_info, &workspace_configs, &fallback_workspace);

            // Create tracked window
            let tracked = Self::create_tracked_window(&window_info, &assignment.workspace_name);
            let workspace_name = tracked.workspace_name.clone();

            // Add to state (returns visibility status, saving a lookup)
            let workspace_is_visible = self.add_window_to_state(tracked);

            // Create border for this window (visibility will be updated later during startup)
            // At this point, workspaces haven't had visibility set yet, so we'll create borders
            // as hidden and let the workspace visibility logic show/hide them.
            self.create_border_for_window(
                window_info.id,
                &window_info.frame,
                &workspace_name,
                workspace_is_visible,
            );
        }
    }

    /// Creates a `TrackedWindow` from `WindowInfo`.
    fn create_tracked_window(info: &WindowInfo, workspace_name: &str) -> TrackedWindow {
        TrackedWindow::new(
            info.id,
            info.pid,
            info.bundle_id.clone(),
            info.app_name.clone(),
            info.title.clone(),
            info.frame,
            workspace_name.to_string(),
        )
    }

    /// Adds a tracked window to the state.
    ///
    /// # Returns
    ///
    /// `true` if the workspace is visible, `false` otherwise.
    fn add_window_to_state(&mut self, window: TrackedWindow) -> bool {
        let workspace_name = window.workspace_name.clone();
        let window_id = window.id;

        // Add to windows list
        self.state.windows.push(window);

        // Add to workspace's window list and return visibility
        if let Some(ws) = self.state.workspace_by_name_mut(&workspace_name)
            && !ws.window_ids.contains(&window_id)
        {
            ws.window_ids.push(window_id);

            // Clear custom split ratios and layout cache when window count changes
            // This prevents layout issues from stale ratios
            ws.split_ratios.clear();
            ws.layout_cache.invalidate();
            ws.is_visible
        } else {
            false
        }
    }

    /// Tracks a new window.
    ///
    /// Assigns the window to a workspace based on rules and adds it to tracking.
    ///
    /// # Returns
    ///
    /// The name of the workspace the window was assigned to.
    pub fn track_window(&mut self, window_info: &WindowInfo) -> Option<String> {
        self.track_window_internal(window_info, true)
    }

    /// Assigns the window to a workspace without applying layout.
    ///
    /// Use this when you need to track multiple windows and apply layout once at the end,
    /// or when detecting tab swaps where layout shouldn't change.
    pub fn track_window_no_layout(&mut self, window_info: &WindowInfo) -> Option<String> {
        self.track_window_internal(window_info, false)
    }

    /// Internal implementation for tracking a window.
    fn track_window_internal(
        &mut self,
        window_info: &WindowInfo,
        apply_layout: bool,
    ) -> Option<String> {
        let ignore_rules = get_config().tiling.ignore.clone();

        // Skip ignored windows
        if should_ignore_window(window_info, &ignore_rules) {
            return None;
        }

        // Check if already tracked
        if self.state.window_by_id(window_info.id).is_some() {
            return None;
        }

        let workspace_configs = get_workspace_configs();
        let fallback_workspace = self
            .state
            .focused_workspace
            .clone()
            .unwrap_or_else(|| "workspace-1".to_string());

        let assignment =
            assign_window_to_workspace(window_info, &workspace_configs, &fallback_workspace);

        let tracked = Self::create_tracked_window(window_info, &assignment.workspace_name);
        let workspace_name = tracked.workspace_name.clone();
        let window_id = window_info.id;
        let window_frame = window_info.frame;

        // Add to state and get visibility status (single lookup)
        let workspace_is_visible = self.add_window_to_state(tracked);

        // Apply layout if requested and workspace is visible
        if apply_layout && workspace_is_visible {
            self.apply_layout_mut(&workspace_name);
        }

        // Create border for this window if borders are enabled
        self.create_border_for_window(
            window_id,
            &window_frame,
            &workspace_name,
            workspace_is_visible,
        );

        Some(workspace_name)
    }

    /// Creates a border for a tracked window.
    ///
    /// Border creation is dispatched to the main thread because some
    /// operations may require the main thread.
    ///
    /// NOTE: With `JankyBorders` integration, `frame` is no longer used since
    /// `JankyBorders` handles its own border positioning.
    fn create_border_for_window(
        &self,
        window_id: u32,
        _frame: &Rect,
        workspace_name: &str,
        workspace_is_visible: bool,
    ) {
        // Check if borders are initialized before dispatching
        if get_border_manager().is_none() {
            return; // Borders not enabled or not initialized
        }

        // Determine initial border state based on workspace layout and focus
        let state = self.determine_border_state(window_id, workspace_name);

        // Clone data for the closure (can't hold references across dispatch)
        // Note: frame is no longer needed since JankyBorders handles positioning
        let workspace_name = workspace_name.to_string();

        // Dispatch border tracking to main thread (may need main thread for some operations)
        crate::utils::thread::dispatch_on_main(move || {
            let Some(border_manager) = get_border_manager() else {
                return;
            };
            // Note: With JankyBorders integration, we only track window state.
            // JankyBorders handles the actual border rendering.
            border_manager.write().track_window(
                window_id,
                state,
                &workspace_name,
                workspace_is_visible,
            );
        });
    }

    /// Determines the border state for a window based on layout and focus.
    fn determine_border_state(&self, window_id: u32, workspace_name: &str) -> BorderState {
        let Some(ws) = self.state.workspace_by_name(workspace_name) else {
            return BorderState::Unfocused;
        };

        // Check layout type first
        match ws.layout {
            LayoutType::Monocle => BorderState::Monocle,
            LayoutType::Floating => BorderState::Floating,
            _ => {
                // Check if this window is focused
                let is_focused = ws
                    .focused_window_index
                    .and_then(|idx| ws.window_ids.get(idx))
                    .is_some_and(|&id| id == window_id);

                if is_focused {
                    BorderState::Focused
                } else {
                    BorderState::Unfocused
                }
            }
        }
    }

    /// Untracks a window by ID.
    ///
    /// Removes the window from tracking and updates the workspace.
    pub fn untrack_window(&mut self, window_id: u32) {
        self.untrack_window_internal(window_id, true);
    }

    /// Untracks a window by ID without applying layout.
    ///
    /// Use this when you need to untrack multiple windows and apply layout once at the end,
    /// or when detecting tab swaps where layout shouldn't change.
    pub fn untrack_window_no_layout(&mut self, window_id: u32) {
        self.untrack_window_internal(window_id, false);
    }

    /// Internal implementation for untracking a window.
    fn untrack_window_internal(&mut self, window_id: u32, apply_layout: bool) {
        // Remove from windows list
        let Some(idx) = self.state.windows.iter().position(|w| w.id == window_id) else {
            return;
        };

        let window = self.state.windows.remove(idx);
        let workspace_name = window.workspace_name;

        // Remove from workspace's window list and check visibility in single lookup
        let workspace_is_visible =
            if let Some(ws) = self.state.workspace_by_name_mut(&workspace_name) {
                let is_visible = ws.is_visible;
                ws.window_ids.retain(|&id| id != window_id);

                // Clear custom split ratios and layout cache when window count changes
                ws.split_ratios.clear();
                ws.layout_cache.invalidate();

                // Update focused window index if needed
                if let Some(focused_idx) = ws.focused_window_index
                    && focused_idx >= ws.window_ids.len()
                {
                    ws.focused_window_index = if ws.window_ids.is_empty() {
                        None
                    } else {
                        Some(ws.window_ids.len() - 1)
                    };
                }
                is_visible
            } else {
                false
            };

        // Remove from focus history
        self.focus_history.remove_window(window_id);

        // Remove border for this window
        Self::remove_border_for_window(window_id);

        // Invalidate AX element cache for this window
        super::window::invalidate_ax_element_cache(window_id);

        // Re-apply layout if requested and workspace is visible
        if apply_layout && workspace_is_visible {
            self.apply_layout_mut(&workspace_name);
        }
    }

    /// Removes the border for a window.
    ///
    /// Dispatched to main thread because `NSWindow` operations require it.
    fn remove_border_for_window(window_id: u32) {
        if get_border_manager().is_none() {
            return;
        }

        crate::utils::thread::dispatch_on_main(move || {
            if let Some(border_manager) = get_border_manager() {
                border_manager.write().untrack_window(window_id);
            }
        });
    }

    /// Updates a tracked window's frame.
    pub fn update_window_frame(&mut self, window_id: u32, frame: Rect) {
        if let Some(window) = self.state.windows.iter_mut().find(|w| w.id == window_id) {
            window.frame = frame;

            // Update border frame to match
            Self::update_border_frame(window_id, &frame);
        }
    }

    /// Updates the border frame for a window.
    ///
    /// NOTE: With `JankyBorders` integration, border frame updates are handled by
    /// `JankyBorders` itself via its own window event subscriptions. This function
    /// is kept for API compatibility but is a no-op.
    #[allow(unused_variables)]
    const fn update_border_frame(window_id: u32, frame: &Rect) {
        // JankyBorders handles its own border positioning via window server events.
        // No action needed from Stache.
    }

    /// Updates a tracked window's title.
    pub fn update_window_title(&mut self, window_id: u32, title: String) {
        if let Some(window) = self.state.windows.iter_mut().find(|w| w.id == window_id) {
            window.title = title;
        }
    }

    /// Swaps a tracked window's ID with a new ID.
    ///
    /// This is used for native tab handling where macOS changes which `CGWindowID`
    /// is the "representative" for a tabbed window. Instead of untracking the old
    /// ID and tracking the new one (which would trigger layout changes), we just
    /// swap the ID in place.
    ///
    /// Returns `true` if the swap was successful.
    pub fn swap_window_id(&mut self, old_id: u32, new_id: u32) -> bool {
        // Update in tracked windows list
        if let Some(window) = self.state.windows.iter_mut().find(|w| w.id == old_id) {
            window.id = new_id;

            let workspace_name = window.workspace_name.clone();

            // Update in workspace's window_ids list
            if let Some(ws) = self.state.workspace_by_name_mut(&workspace_name)
                && let Some(idx) = ws.window_ids.iter().position(|&id| id == old_id)
            {
                ws.window_ids[idx] = new_id;
            }

            // Update in focus history
            self.focus_history.swap_window_id(old_id, new_id);

            true
        } else {
            false
        }
    }

    /// Sets the focused window in a workspace.
    ///
    /// This also updates the focused workspace and screen to match where the
    /// focused window is located.
    pub fn set_focused_window(&mut self, workspace_name: &str, window_id: u32) {
        // Get the previously focused window ID before updating
        // Single lookup instead of redundant double lookup
        let old_focused_id = self
            .state
            .workspace_by_name(workspace_name)
            .and_then(|ws| ws.focused_window_index.and_then(|idx| ws.window_ids.get(idx).copied()));

        // First, update the focused window index in the workspace
        let screen_id = if let Some(ws) = self.state.workspace_by_name_mut(workspace_name)
            && let Some(idx) = ws.window_ids.iter().position(|&id| id == window_id)
        {
            ws.focused_window_index = Some(idx);
            Some(ws.screen_id)
        } else {
            None
        };

        // Update the focused workspace to match
        // This ensures focus commands operate on the correct workspace
        if screen_id.is_some() {
            // Update is_focused flag on all workspaces
            for ws in &mut self.state.workspaces {
                ws.is_focused = ws.name.eq_ignore_ascii_case(workspace_name);
            }
            self.state.focused_workspace = Some(workspace_name.to_string());
            self.state.focused_screen_id = screen_id;

            // Update border states for old and new focused windows
            self.update_focus_border_states(old_focused_id, window_id, workspace_name);
        }
    }

    /// Updates border states when focus changes.
    ///
    /// This is the ONLY place where border colors should be updated.
    /// It always updates `JankyBorders` colors based on the current layout,
    /// regardless of whether the border state changed.
    ///
    /// Dispatched to main thread because border operations require it.
    fn update_focus_border_states(
        &self,
        old_focused_id: Option<u32>,
        new_focused_id: u32,
        workspace_name: &str,
    ) {
        if get_border_manager().is_none() {
            return;
        }

        // Determine the appropriate state based on layout
        let ws = self.state.workspace_by_name(workspace_name);
        let layout = ws.map(|w| w.layout);
        let (focused_state, unfocused_state, is_monocle, is_floating) = match layout {
            Some(LayoutType::Monocle) => (BorderState::Monocle, BorderState::Monocle, true, false),
            Some(LayoutType::Floating) => {
                (BorderState::Floating, BorderState::Floating, false, true)
            }
            _ => (BorderState::Focused, BorderState::Unfocused, false, false),
        };

        crate::utils::thread::dispatch_on_main(move || {
            let Some(border_manager) = get_border_manager() else {
                return;
            };
            let mut manager = border_manager.write();

            // Update old focused window to unfocused state
            if let Some(old_id) = old_focused_id
                && old_id != new_focused_id
            {
                manager.set_window_state_no_color_update(old_id, unfocused_state);
            }

            // Update new focused window to focused state
            manager.set_window_state_no_color_update(new_focused_id, focused_state);

            // Drop manager before calling janky to avoid holding lock unnecessarily
            drop(manager);

            // Always update JankyBorders colors on focus change
            // This is the ONLY place where colors are updated
            super::borders::janky::update_colors_for_state(is_monocle, is_floating);
        });
    }

    /// Clears focus from all borders (sets all to unfocused state).
    ///
    /// Called when focus moves to an untracked window outside the tiling system.
    /// Dispatched to main thread because border operations require it.
    #[allow(clippy::unused_self)] // Method semantically belongs to the manager
    pub fn clear_all_focus_borders(&self) {
        if get_border_manager().is_none() {
            return;
        }

        crate::utils::thread::dispatch_on_main(move || {
            let Some(border_manager) = get_border_manager() else {
                return;
            };
            border_manager.write().set_all_unfocused();
        });
    }

    /// Shows borders for all windows in a workspace.
    ///
    /// Dispatched to main thread because `NSWindow` operations require it.
    pub fn show_borders_for_workspace(workspace_name: &str) {
        if get_border_manager().is_none() {
            return;
        }

        let workspace_name = workspace_name.to_string();
        crate::utils::thread::dispatch_on_main(move || {
            if let Some(border_manager) = get_border_manager() {
                border_manager.write().show_workspace(&workspace_name);
            }
        });
    }

    /// Hides borders for all windows in a workspace.
    ///
    /// Dispatched to main thread because some operations may require it.
    pub fn hide_borders_for_workspace(workspace_name: &str) {
        if get_border_manager().is_none() {
            return;
        }

        let workspace_name = workspace_name.to_string();
        crate::utils::thread::dispatch_on_main(move || {
            if let Some(border_manager) = get_border_manager() {
                border_manager.write().hide_workspace(&workspace_name);
            }
        });
    }

    /// Updates border colors based on workspace layout.
    ///
    /// This is called when switching workspaces to ensure the active color
    /// matches the layout (monocle, floating, or normal focused).
    fn update_border_colors_for_workspace(workspace: &Workspace) {
        use super::borders::janky;

        let is_monocle = workspace.layout == LayoutType::Monocle;
        let is_floating = workspace.layout == LayoutType::Floating;

        janky::update_colors_for_state(is_monocle, is_floating);
    }

    /// Gets all tracked windows.
    #[must_use]
    pub fn get_windows(&self) -> &[TrackedWindow] { &self.state.windows }

    /// Gets windows for a workspace.
    #[must_use]
    pub fn get_windows_for_workspace(&self, workspace_name: &str) -> Vec<&TrackedWindow> {
        self.state.windows_for_workspace(workspace_name)
    }

    /// Gets a window by ID.
    #[must_use]
    pub fn get_window(&self, id: u32) -> Option<&TrackedWindow> { self.state.window_by_id(id) }

    // ========================================================================
    // Window Focus Commands
    // ========================================================================

    /// Focuses a window by ID.
    ///
    /// # Returns
    ///
    /// `true` if the window was focused successfully.
    pub fn focus_window_by_id(&mut self, window_id: u32) -> bool {
        // Find the window
        let Some(window) = self.state.window_by_id(window_id) else {
            eprintln!("stache: tiling: focus_window_by_id: window {window_id} not found in state");
            return false;
        };

        let workspace_name = window.workspace_name.clone();

        // Focus the window
        match super::window::focus_window(window_id) {
            Ok(()) => {
                // Record this programmatic focus to debounce incoming focus events
                self.last_programmatic_focus = Some((window_id, Instant::now()));
                self.set_focused_window(&workspace_name, window_id);
                true
            }
            Err(e) => {
                eprintln!("stache: tiling: focus_window_by_id: focus_window failed: {e}");
                false
            }
        }
    }

    /// Checks if a focus event for the given window should be skipped.
    ///
    /// Returns `true` if we recently programmatically focused a different window
    /// and should ignore this focus event (it's likely a stale event from macOS).
    #[must_use]
    pub fn should_skip_focus_event(&self, window_id: u32) -> bool {
        if let Some((last_focused_id, when)) = self.last_programmatic_focus {
            // If we focused a different window recently, skip this event
            if last_focused_id != window_id && when.elapsed().as_millis() < FOCUS_COOLDOWN_MS {
                return true;
            }
        }
        false
    }

    /// Checks if a workspace switch should be skipped due to a recent switch.
    ///
    /// Returns `true` if we recently switched workspaces and should ignore
    /// this switch request (it's likely a stale focus event from macOS
    /// triggered by the hide/show operations of the previous switch).
    #[must_use]
    pub fn should_skip_workspace_switch(&self, _target_workspace: &str) -> bool {
        if let Some(when) = self.last_workspace_switch
            && when.elapsed().as_millis() < WORKSPACE_SWITCH_COOLDOWN_MS
        {
            return true;
        }
        false
    }

    /// Records that a workspace switch just occurred.
    fn record_workspace_switch(&mut self) { self.last_workspace_switch = Some(Instant::now()); }

    /// Focuses the next window in the current workspace (cycles).
    ///
    /// # Returns
    ///
    /// The ID of the focused window, or `None` if no window could be focused.
    pub fn focus_next_window(&mut self) -> Option<u32> {
        let workspace = self.state.focused_workspace()?;
        let workspace_name = workspace.name.clone();
        let window_ids = workspace.window_ids.clone();

        if window_ids.is_empty() {
            eprintln!("stache: tiling: focus_next: no windows in workspace");
            return None;
        }

        let current_idx = workspace.focused_window_index.unwrap_or(0);
        let next_idx = (current_idx + 1) % window_ids.len();
        let next_window_id = window_ids[next_idx];

        eprintln!(
            "stache: tiling: focus_next: current_idx={current_idx}, next_idx={next_idx}, window_id={next_window_id}, window_ids={window_ids:?}"
        );

        if self.focus_window_by_id(next_window_id) {
            // Update the focused index
            if let Some(ws) = self.state.workspace_by_name_mut(&workspace_name) {
                ws.focused_window_index = Some(next_idx);
            }
            eprintln!("stache: tiling: focus_next: success, new index={next_idx}");
            Some(next_window_id)
        } else {
            eprintln!("stache: tiling: focus_next: focus_window_by_id failed");
            None
        }
    }

    /// Focuses the previous window in the current workspace (cycles).
    ///
    /// # Returns
    ///
    /// The ID of the focused window, or `None` if no window could be focused.
    pub fn focus_previous_window(&mut self) -> Option<u32> {
        let workspace = self.state.focused_workspace()?;
        let workspace_name = workspace.name.clone();
        let window_ids = workspace.window_ids.clone();

        if window_ids.is_empty() {
            return None;
        }

        let current_idx = workspace.focused_window_index.unwrap_or(0);
        let prev_idx = if current_idx == 0 {
            window_ids.len() - 1
        } else {
            current_idx - 1
        };
        let prev_window_id = window_ids[prev_idx];

        if self.focus_window_by_id(prev_window_id) {
            // Update the focused index
            if let Some(ws) = self.state.workspace_by_name_mut(&workspace_name) {
                ws.focused_window_index = Some(prev_idx);
            }
            Some(prev_window_id)
        } else {
            None
        }
    }

    /// Focuses a window in the specified direction.
    ///
    /// Direction can be: "up", "down", "left", "right", "next", "previous",
    /// or a window ID.
    ///
    /// # Returns
    ///
    /// The ID of the focused window, or `None` if no window could be focused.
    pub fn focus_window_in_direction(&mut self, direction: &str) -> Option<u32> {
        match direction.to_lowercase().as_str() {
            "next" => self.focus_next_window(),
            "previous" | "prev" => self.focus_previous_window(),
            "up" | "down" | "left" | "right" => self.focus_window_spatial(direction),
            // Try to parse as window ID
            _ => direction.parse::<u32>().map_or_else(
                |_| {
                    eprintln!("stache: tiling: invalid focus target: {direction}");
                    None
                },
                |window_id| {
                    if self.focus_window_by_id(window_id) {
                        Some(window_id)
                    } else {
                        None
                    }
                },
            ),
        }
    }

    /// Focuses a window in a spatial direction (up/down/left/right).
    ///
    /// Finds the nearest window in the specified direction based on
    /// window positions.
    fn focus_window_spatial(&mut self, direction: &str) -> Option<u32> {
        let workspace = self.state.focused_workspace()?;
        let workspace_name = workspace.name.clone();
        let window_ids = workspace.window_ids.clone();

        if window_ids.is_empty() {
            return None;
        }

        // Get current focused window
        let focused_idx = workspace.focused_window_index.unwrap_or(0);
        let focused_id = window_ids.get(focused_idx)?;

        let focused_window = self.state.window_by_id(*focused_id)?.clone();

        // Find the best candidate in the specified direction
        let candidate = self.find_window_in_direction(&focused_window, direction, &window_ids);

        if let Some((window_id, new_idx)) = candidate
            && self.focus_window_by_id(window_id)
        {
            if let Some(ws) = self.state.workspace_by_name_mut(&workspace_name) {
                ws.focused_window_index = Some(new_idx);
            }
            return Some(window_id);
        }

        None
    }

    /// Finds the nearest window in the specified direction.
    ///
    /// # Returns
    ///
    /// A tuple of (`window_id`, index) for the best candidate, or `None`.
    fn find_window_in_direction(
        &self,
        from_window: &TrackedWindow,
        direction: &str,
        window_ids: &[u32],
    ) -> Option<(u32, usize)> {
        let from_center_x = from_window.frame.x + from_window.frame.width / 2.0;
        let from_center_y = from_window.frame.y + from_window.frame.height / 2.0;

        let mut best_candidate: Option<(u32, usize, f64)> = None;

        for (idx, &window_id) in window_ids.iter().enumerate() {
            if window_id == from_window.id {
                continue;
            }

            let Some(window) = self.state.window_by_id(window_id) else {
                continue;
            };

            let center_x = window.frame.x + window.frame.width / 2.0;
            let center_y = window.frame.y + window.frame.height / 2.0;

            // Check if this window is in the right direction
            let is_valid_direction = match direction {
                "up" => center_y < from_center_y,
                "down" => center_y > from_center_y,
                "left" => center_x < from_center_x,
                "right" => center_x > from_center_x,
                _ => false,
            };

            if !is_valid_direction {
                continue;
            }

            // Calculate distance (using squared distance to avoid sqrt)
            let dx = center_x - from_center_x;
            let dy = center_y - from_center_y;
            let distance = dx * dx + dy * dy;

            // For directional focus, prefer windows that are more aligned
            // with the direction (use a weighted distance)
            let weighted_distance = match direction {
                "up" | "down" => {
                    // Prefer vertically aligned windows
                    let alignment_penalty = dx.abs() * 2.0;
                    distance + alignment_penalty * alignment_penalty
                }
                "left" | "right" => {
                    // Prefer horizontally aligned windows
                    let alignment_penalty = dy.abs() * 2.0;
                    distance + alignment_penalty * alignment_penalty
                }
                _ => distance,
            };

            if best_candidate.is_none() || weighted_distance < best_candidate.unwrap().2 {
                best_candidate = Some((window_id, idx, weighted_distance));
            }
        }

        best_candidate.map(|(id, idx, _)| (id, idx))
    }

    // ========================================================================
    // Window Swap Commands
    // ========================================================================

    /// Swaps the focused window with another window in the specified direction.
    ///
    /// Direction can be: "up", "down", "left", "right", "next", "previous".
    ///
    /// # Returns
    ///
    /// `true` if windows were swapped successfully.
    pub fn swap_window_in_direction(&mut self, direction: &str) -> bool {
        let Some(workspace) = self.state.focused_workspace() else {
            return false;
        };

        let workspace_name = workspace.name.clone();
        let window_ids = workspace.window_ids.clone();

        if window_ids.len() < 2 {
            return false;
        }

        let focused_idx = workspace.focused_window_index.unwrap_or(0);
        let Some(&focused_id) = window_ids.get(focused_idx) else {
            return false;
        };

        // Find the target window to swap with
        let target = match direction.to_lowercase().as_str() {
            "next" => {
                let next_idx = (focused_idx + 1) % window_ids.len();
                Some((window_ids[next_idx], next_idx))
            }
            "previous" | "prev" => {
                let prev_idx = if focused_idx == 0 {
                    window_ids.len() - 1
                } else {
                    focused_idx - 1
                };
                Some((window_ids[prev_idx], prev_idx))
            }
            "up" | "down" | "left" | "right" => {
                let Some(focused_window) = self.state.window_by_id(focused_id).cloned() else {
                    return false;
                };
                self.find_window_in_direction(&focused_window, direction, &window_ids)
            }
            _ => None,
        };

        let Some((_target_id, target_idx)) = target else {
            return false;
        };

        // Swap the windows in the workspace's window_ids list, preserving their sizes
        if let Some(ws) = self.state.workspace_by_name_mut(&workspace_name) {
            let window_count = ws.window_ids.len();

            // Get current proportions (individual sizes) from ratios
            let mut proportions = cumulative_ratios_to_proportions(&ws.split_ratios, window_count);

            // Swap the proportions so each window keeps its size
            proportions.swap(focused_idx, target_idx);

            // Convert back to cumulative ratios
            let new_ratios = proportions_to_cumulative_ratios(&proportions);

            // Swap window IDs and update ratios
            ws.window_ids.swap(focused_idx, target_idx);
            ws.split_ratios = new_ratios.into();
            ws.layout_cache.invalidate();

            // Keep focus on the originally focused window (now at target_idx)
            ws.focused_window_index = Some(target_idx);
        }

        // Re-apply layout to reposition windows
        self.apply_layout_forced(&workspace_name);

        true
    }

    /// Swaps two windows by their IDs within the same workspace.
    ///
    /// This is used by drag-and-drop to swap windows when one is dropped on another.
    /// Unlike `swap_window_in_direction`, this doesn't require the windows to be
    /// adjacent or in a particular direction.
    ///
    /// # Arguments
    ///
    /// * `window_id_a` - The first window ID
    /// * `window_id_b` - The second window ID
    ///
    /// # Returns
    ///
    /// `true` if windows were swapped successfully.
    pub fn swap_windows_by_id(&mut self, window_id_a: u32, window_id_b: u32) -> bool {
        if window_id_a == window_id_b {
            return false;
        }

        // Find the workspace containing window A
        let Some(window_a) = self.state.window_by_id(window_id_a).cloned() else {
            return false;
        };

        let workspace_name = window_a.workspace_name;

        // Verify window B is in the same workspace
        let Some(window_b) = self.state.window_by_id(window_id_b).cloned() else {
            return false;
        };

        if window_b.workspace_name != workspace_name {
            return false;
        }

        // Get the workspace and find indices
        let Some(workspace) = self.state.workspace_by_name(&workspace_name) else {
            return false;
        };

        let window_ids = workspace.window_ids.clone();
        let idx_a = window_ids.iter().position(|&id| id == window_id_a);
        let idx_b = window_ids.iter().position(|&id| id == window_id_b);

        let (Some(idx_a), Some(idx_b)) = (idx_a, idx_b) else {
            return false;
        };

        // Perform the swap, preserving sizes
        if let Some(ws) = self.state.workspace_by_name_mut(&workspace_name) {
            let window_count = ws.window_ids.len();

            // Get current proportions (individual sizes) from ratios
            let mut proportions = cumulative_ratios_to_proportions(&ws.split_ratios, window_count);

            // Swap the proportions so each window keeps its size
            proportions.swap(idx_a, idx_b);

            // Convert back to cumulative ratios
            let new_ratios = proportions_to_cumulative_ratios(&proportions);

            // Swap window IDs and update ratios
            ws.window_ids.swap(idx_a, idx_b);
            ws.split_ratios = new_ratios.into();
            ws.layout_cache.invalidate();

            // Update focused window index if needed (keep focus on same window)
            if let Some(focused_idx) = ws.focused_window_index {
                if focused_idx == idx_a {
                    ws.focused_window_index = Some(idx_b);
                } else if focused_idx == idx_b {
                    ws.focused_window_index = Some(idx_a);
                }
            }
        }

        eprintln!(
            "stache: tiling: swap-by-id: swapped window {window_id_a} with {window_id_b} in workspace '{workspace_name}'"
        );

        // Re-apply layout to reposition windows
        self.apply_layout_forced(&workspace_name);

        true
    }

    // ========================================================================
    // Window Send Commands
    // ========================================================================

    /// Sends the focused window to another workspace.
    ///
    /// After sending, the window manager switches to the target workspace
    /// and focuses the moved window, so the user can continue working with it.
    ///
    /// # Returns
    ///
    /// `true` if the window was sent successfully.
    pub fn send_window_to_workspace(&mut self, target_workspace: &str) -> bool {
        let Some(workspace) = self.state.focused_workspace() else {
            eprintln!("stache: tiling: send-to-workspace: no focused workspace");
            return false;
        };

        let source_workspace = workspace.name.clone();
        let window_ids = workspace.window_ids.clone();

        if window_ids.is_empty() {
            eprintln!("stache: tiling: send-to-workspace: no windows in workspace");
            return false;
        }

        // Get the focused window
        let focused_idx = workspace.focused_window_index.unwrap_or(0);
        let Some(&window_id) = window_ids.get(focused_idx) else {
            return false;
        };

        // Check if target workspace exists
        if self.state.workspace_by_name(target_workspace).is_none() {
            eprintln!(
                "stache: tiling: send-to-workspace: workspace '{target_workspace}' not found"
            );
            return false;
        }

        // Don't send to the same workspace
        if source_workspace == target_workspace {
            eprintln!("stache: tiling: send-to-workspace: window already in '{target_workspace}'");
            return false;
        }

        // Remove window from source workspace
        if let Some(ws) = self.state.workspace_by_name_mut(&source_workspace) {
            ws.window_ids.retain(|&id| id != window_id);
            ws.split_ratios.clear();
            ws.layout_cache.invalidate();

            // Update focused index for source workspace
            if ws.window_ids.is_empty() {
                ws.focused_window_index = None;
            } else if focused_idx >= ws.window_ids.len() {
                ws.focused_window_index = Some(ws.window_ids.len() - 1);
            }
        }

        // Add window to target workspace and set it as the focused window
        let new_window_idx = if let Some(ws) = self.state.workspace_by_name_mut(target_workspace) {
            ws.window_ids.push(window_id);
            ws.split_ratios.clear();
            ws.layout_cache.invalidate();
            // Set the newly added window as the focused window in target workspace
            let idx = ws.window_ids.len() - 1;
            ws.focused_window_index = Some(idx);
            idx
        } else {
            0
        };

        // Update tracked window's workspace
        if let Some(window) = self.state.windows.iter_mut().find(|w| w.id == window_id) {
            window.workspace_name = target_workspace.to_string();
        }

        eprintln!(
            "stache: tiling: sent window {window_id} from '{source_workspace}' to '{target_workspace}' (index {new_window_idx})"
        );

        // Re-apply layout to source workspace (the window is gone, remaining windows need repositioning)
        self.apply_layout_forced(&source_workspace);

        // Switch to target workspace (this will show the window and apply layout)
        // This also handles the case where target workspace is on a different screen
        let target_workspace_owned = target_workspace.to_string();
        self.switch_workspace(&target_workspace_owned);

        // Focus the moved window to ensure it remains focused
        if super::window::focus_window(window_id).is_ok() {
            self.last_programmatic_focus = Some((window_id, std::time::Instant::now()));
            eprintln!("stache: tiling: focused moved window {window_id}");
        }

        true
    }

    /// Sends the focused window to another screen.
    ///
    /// The window is sent to the currently visible workspace on the target screen.
    ///
    /// # Returns
    ///
    /// `true` if the window was sent successfully.
    pub fn send_window_to_screen(&mut self, target_screen: &str) -> bool {
        // Find the target screen
        let screen = if target_screen == "main" {
            self.state.main_screen().cloned()
        } else if target_screen == "secondary" {
            // Find first non-main screen
            self.state.screens.iter().find(|s| !s.is_main).cloned()
        } else {
            self.state.screen_by_name(target_screen).cloned()
        };

        let Some(screen) = screen else {
            eprintln!("stache: tiling: send-to-screen: screen '{target_screen}' not found");
            return false;
        };

        // Find the visible workspace on the target screen
        let target_workspace = self
            .state
            .workspaces
            .iter()
            .find(|ws| ws.screen_id == screen.id && ws.is_visible)
            .map(|ws| ws.name.clone());

        let Some(target_workspace) = target_workspace else {
            eprintln!(
                "stache: tiling: send-to-screen: no visible workspace on screen '{target_screen}'"
            );
            return false;
        };

        // Use send_window_to_workspace to do the actual work
        self.send_window_to_workspace(&target_workspace)
    }

    /// Sends the focused workspace to another screen.
    ///
    /// The workspace is moved to the target screen and becomes visible there.
    /// If the workspace was visible on the source screen, another workspace
    /// on that screen becomes visible instead.
    ///
    /// # Arguments
    ///
    /// * `target_screen` - Screen name ("main", "secondary", or display name)
    ///
    /// # Returns
    ///
    /// `true` if the workspace was sent successfully.
    pub fn send_workspace_to_screen(&mut self, target_screen: &str) -> bool {
        // Get the focused workspace
        let Some(workspace) = self.state.focused_workspace() else {
            eprintln!("stache: tiling: send-workspace-to-screen: no focused workspace");
            return false;
        };

        let workspace_name = workspace.name.clone();
        let source_screen_id = workspace.screen_id;
        let was_visible = workspace.is_visible;

        // Find the target screen
        let target_screen_obj = if target_screen == "main" {
            self.state.main_screen().cloned()
        } else if target_screen == "secondary" {
            self.state.screens.iter().find(|s| !s.is_main).cloned()
        } else {
            self.state.screen_by_name(target_screen).cloned()
        };

        let Some(target_screen_obj) = target_screen_obj else {
            eprintln!(
                "stache: tiling: send-workspace-to-screen: screen '{target_screen}' not found"
            );
            return false;
        };

        let target_screen_id = target_screen_obj.id;

        // Don't move to same screen
        if source_screen_id == target_screen_id {
            eprintln!(
                "stache: tiling: send-workspace-to-screen: workspace '{workspace_name}' already on screen '{target_screen}'"
            );
            return false;
        }

        // Check if this is a "return home" move (moving back to original screen)
        let is_returning_home = self
            .state
            .workspace_by_name(&workspace_name)
            .and_then(|ws| ws.original_screen_id)
            .is_some_and(|orig_id| orig_id == target_screen_id);

        // Store original screen if this is the first move
        if let Some(ws) = self.state.workspace_by_name_mut(&workspace_name)
            && ws.original_screen_id.is_none()
        {
            ws.original_screen_id = Some(source_screen_id);
        }

        // Hide windows in the workspace before moving (if it was visible)
        if was_visible {
            let windows_to_hide: Vec<u32> = self
                .state
                .windows
                .iter()
                .filter(|w| w.workspace_name == workspace_name)
                .map(|w| w.id)
                .collect();

            for window_id in &windows_to_hide {
                if let Err(e) = super::window::hide_window(*window_id) {
                    eprintln!("stache: tiling: failed to hide window {window_id}: {e}");
                }
            }
        }

        // If the workspace was visible on source screen, make another workspace visible there
        if was_visible {
            // Find another workspace on the source screen to make visible
            let fallback_workspace = self
                .state
                .workspaces
                .iter()
                .find(|ws| ws.screen_id == source_screen_id && ws.name != workspace_name)
                .map(|ws| ws.name.clone());

            if let Some(fallback_name) = fallback_workspace {
                // Switch to the fallback workspace on source screen
                for ws in &mut self.state.workspaces {
                    if ws.screen_id == source_screen_id {
                        ws.is_visible = ws.name == fallback_name;
                    }
                }
                // Show and apply layout for fallback workspace
                self.show_and_apply_workspace(&fallback_name);
            }
        }

        // Update workspace's screen assignment
        if let Some(ws) = self.state.workspace_by_name_mut(&workspace_name) {
            ws.screen_id = target_screen_id;
            ws.is_visible = true;
            ws.is_focused = true;

            // Clear original screen if returning home
            if is_returning_home {
                ws.original_screen_id = None;
            }
        }

        // Hide currently visible workspace on target screen
        for ws in &mut self.state.workspaces {
            if ws.screen_id == target_screen_id && ws.name != workspace_name {
                ws.is_visible = false;
                ws.is_focused = false;
            }
        }

        // Update focused workspace/screen
        self.state.focused_workspace = Some(workspace_name.clone());
        self.state.focused_screen_id = Some(target_screen_id);

        // Show workspace windows and apply layout on target screen
        self.show_and_apply_workspace(&workspace_name);

        eprintln!(
            "stache: tiling: sent workspace '{}' from screen {} to '{}'{}",
            workspace_name,
            source_screen_id,
            target_screen,
            if is_returning_home {
                " (returned home)"
            } else {
                ""
            }
        );

        true
    }

    /// Helper to show windows and apply layout for a workspace.
    fn show_and_apply_workspace(&mut self, workspace_name: &str) {
        // Show windows
        let windows_to_show: Vec<_> = self
            .state
            .windows
            .iter()
            .filter(|w| w.workspace_name == *workspace_name)
            .collect();

        let (shown, _) = super::workspace::show_workspace_windows(&windows_to_show);
        if shown > 0 {
            eprintln!("stache: tiling: showed {shown} windows for workspace '{workspace_name}'");
        }

        // Apply layout
        self.apply_layout_forced(workspace_name);
    }

    // ========================================================================
    // Window Resize Commands
    // ========================================================================

    /// Resizes the focused window by adjusting its ratio.
    ///
    /// # Arguments
    ///
    /// * `dimension` - "width" or "height"
    /// * `delta` - Pixels to add (positive) or remove (negative)
    ///
    /// # Returns
    ///
    /// `true` if the resize was applied.
    pub fn resize_focused_window(&mut self, dimension: &str, delta: i32) -> bool {
        let Some(workspace) = self.state.focused_workspace() else {
            eprintln!("stache: tiling: resize: no focused workspace");
            return false;
        };

        let workspace_name = workspace.name.clone();
        let window_ids = workspace.window_ids.clone();
        let layout = workspace.layout;

        if window_ids.len() < 2 {
            eprintln!("stache: tiling: resize: need at least 2 windows to resize");
            return false;
        }

        let focused_idx = workspace.focused_window_index.unwrap_or(0);
        let Some(&focused_id) = window_ids.get(focused_idx) else {
            return false;
        };

        let Some(focused_window) = self.state.window_by_id(focused_id).cloned() else {
            return false;
        };

        // Calculate the new frame based on dimension and delta
        let delta_f = f64::from(delta);
        let new_frame = match dimension.to_lowercase().as_str() {
            "width" => Rect::new(
                focused_window.frame.x,
                focused_window.frame.y,
                (focused_window.frame.width + delta_f).max(100.0),
                focused_window.frame.height,
            ),
            "height" => Rect::new(
                focused_window.frame.x,
                focused_window.frame.y,
                focused_window.frame.width,
                (focused_window.frame.height + delta_f).max(100.0),
            ),
            _ => {
                eprintln!("stache: tiling: resize: invalid dimension: {dimension}");
                return false;
            }
        };

        // Use the existing ratio calculation method
        self.calculate_and_apply_ratios_for_window(
            &workspace_name,
            focused_id,
            focused_window.frame,
            new_frame,
        );

        eprintln!(
            "stache: tiling: resized window {focused_id} {dimension} by {delta} px (layout: {layout:?})"
        );

        true
    }

    /// Applies a floating preset to the focused window.
    ///
    /// The preset defines the window's size and position (centered, half-screen, etc.).
    /// Gaps are respected when calculating the final frame.
    ///
    /// # Arguments
    ///
    /// * `preset_name` - Name of the preset to apply (case-insensitive).
    ///
    /// # Returns
    ///
    /// `true` if the preset was applied successfully, `false` otherwise.
    #[allow(clippy::cast_possible_truncation)] // Frame dimensions for logging only
    pub fn apply_preset(&mut self, preset_name: &str) -> bool {
        // Find the preset
        let Some(preset) = super::layout::find_preset(preset_name) else {
            eprintln!("stache: tiling: preset not found: '{preset_name}'");
            let available = super::layout::list_preset_names();
            if available.is_empty() {
                eprintln!("stache: tiling: no presets configured");
            } else {
                eprintln!("stache: tiling: available presets: {}", available.join(", "));
            }
            return false;
        };

        // Get the focused workspace and window
        let Some(workspace) = self.state.focused_workspace() else {
            return false;
        };

        // Presets can only be applied to floating workspaces
        if workspace.layout != LayoutType::Floating {
            return false;
        }

        let focused_idx = workspace.focused_window_index.unwrap_or(0);
        let Some(&_window_id) = workspace.window_ids.get(focused_idx) else {
            return false;
        };

        let screen_id = workspace.screen_id;

        // Get the screen
        let Some(_screen) = self.state.screen_by_id(screen_id).cloned() else {
            return false;
        };

        // Presets can only be applied to floating workspaces
        if workspace.layout != LayoutType::Floating {
            eprintln!(
                "stache: tiling: apply_preset failed: workspace '{}' layout is {:?}, not Floating",
                workspace.name, workspace.layout
            );
            return false;
        }

        let focused_idx = workspace.focused_window_index.unwrap_or(0);
        let Some(&window_id) = workspace.window_ids.get(focused_idx) else {
            eprintln!(
                "stache: tiling: apply_preset failed: no window at index {} in workspace '{}' (has {} windows)",
                focused_idx,
                workspace.name,
                workspace.window_ids.len()
            );
            return false;
        };

        let screen_id = workspace.screen_id;

        // Get the screen
        let Some(screen) = self.state.screen_by_id(screen_id).cloned() else {
            eprintln!("stache: tiling: apply_preset failed: screen {screen_id} not found");
            return false;
        };

        // Get current window frame for animation
        let current_frame = self.state.window_by_id(window_id).map(|w| w.frame).unwrap_or_default();

        // Use cached gaps for this screen
        let gaps = self.get_gaps_for_screen(&screen);

        // Calculate the frame from the preset
        let target_frame =
            super::layout::calculate_preset_frame(&preset, &screen.visible_frame, &gaps);

        // Animate to the new frame
        let transition = WindowTransition::new(window_id, current_frame, target_frame);
        let _ = self.animation_system.animate(vec![transition]);

        // Update tracked window frame
        if let Some(window) = self.state.windows.iter_mut().find(|w| w.id == window_id) {
            window.frame = target_frame;
        }

        true
    }
}

impl Default for TilingManager {
    fn default() -> Self { Self::new() }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manager_new() {
        let manager = TilingManager::new();
        assert!(!manager.is_enabled());
        assert!(manager.state.screens.is_empty());
        assert!(manager.state.workspaces.is_empty());
    }

    #[test]
    fn test_manager_refresh_screens() {
        let mut manager = TilingManager::new();
        manager.refresh_screens();

        // Should have at least one screen
        assert!(!manager.state.screens.is_empty());

        // Should have exactly one main screen
        let main_count = manager.state.screens.iter().filter(|s| s.is_main).count();
        assert_eq!(main_count, 1);
    }

    #[test]
    fn test_manager_get_screens() {
        let mut manager = TilingManager::new();
        manager.refresh_screens();

        let screens = manager.get_screens();
        assert!(!screens.is_empty());
    }

    #[test]
    fn test_manager_get_main_screen() {
        let mut manager = TilingManager::new();
        manager.refresh_screens();

        let main = manager.get_main_screen();
        assert!(main.is_some());
        assert!(main.unwrap().is_main);
    }

    #[test]
    fn test_manager_create_default_workspaces() {
        let mut manager = TilingManager::new();
        manager.refresh_screens();
        manager.create_default_workspaces();

        // Should have one workspace per screen
        assert_eq!(manager.state.workspaces.len(), manager.state.screens.len());
    }

    #[test]
    fn test_manager_state_access() {
        let mut manager = TilingManager::new();

        // Test immutable access
        let _state = manager.state();

        // Test mutable access
        let state_mut = manager.state_mut();
        state_mut.focused_workspace = Some("test".to_string());

        assert_eq!(manager.state.focused_workspace, Some("test".to_string()));
    }

    // ========================================================================
    // Gaps Cache Tests
    // ========================================================================

    #[test]
    fn test_gaps_cache_populated_after_refresh_screens() {
        let mut manager = TilingManager::new();
        manager.refresh_screens();

        // Gaps cache should have one entry per screen
        assert_eq!(manager.gaps_cache.len(), manager.state.screens.len());
    }

    #[test]
    fn test_gaps_cache_has_entries_for_all_screens() {
        let mut manager = TilingManager::new();
        manager.refresh_screens();

        // Every screen should have a corresponding entry in the gaps cache
        for screen in &manager.state.screens {
            assert!(
                manager.gaps_cache.contains_key(&screen.name),
                "Gaps cache missing entry for screen: {}",
                screen.name
            );
        }
    }

    #[test]
    fn test_get_gaps_for_screen_returns_cached_value() {
        let mut manager = TilingManager::new();
        manager.refresh_screens();

        if let Some(screen) = manager.state.screens.first().cloned() {
            // Get gaps from cache
            let gaps1 = manager.get_gaps_for_screen(&screen);
            let gaps2 = manager.get_gaps_for_screen(&screen);

            // Should return consistent values (from cache)
            assert_eq!(gaps1.compute_hash(), gaps2.compute_hash());
        }
    }

    #[test]
    fn test_gaps_cache_main_screen_has_bar_offset() {
        let mut manager = TilingManager::new();
        manager.refresh_screens();

        // Find the main screen
        if let Some(main_screen) = manager.state.screens.iter().find(|s| s.is_main).cloned() {
            let gaps = manager.get_gaps_for_screen(&main_screen);

            // Main screen gaps should have bar offset included
            // The exact value depends on config, but outer_top should be non-zero
            // if bar is configured (which it is by default)
            let config = get_config();
            let bar_offset = f64::from(config.bar.height) + f64::from(config.bar.padding);

            if bar_offset > 0.0 {
                // outer_top should include bar_offset
                assert!(
                    gaps.outer_top >= bar_offset,
                    "Main screen gaps.outer_top ({}) should include bar_offset ({})",
                    gaps.outer_top,
                    bar_offset
                );
            }
        }
    }

    // Note: Helper function tests are now in helpers.rs
}
