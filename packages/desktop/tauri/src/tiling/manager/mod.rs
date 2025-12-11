//! Tiling manager module.
//!
//! This module provides the core `TilingManager` struct and its initialization.
//! Implementation is split across submodules for clarity:
//!
//! - `discovery`: Window and app discovery, rule matching
//! - `layout_ops`: Layout application and management
//! - `workspace_ops`: Workspace switching, visibility, screen operations
//! - `window_ops`: Window focus, move, and send operations

#![allow(clippy::assigning_clones)]

mod discovery;
mod layout_ops;
mod window_ops;
mod workspace_ops;

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use barba_shared::TilingConfig;
use parking_lot::RwLock;
use tauri::{AppHandle, Emitter};

use super::error::TilingError;
use super::observer::{
    ScreenFocusedPayload, events, is_in_switch_cooldown, mark_layout_applied, mark_switch_completed,
};
use super::{accessibility, window, workspace};
use crate::config;

/// The main tiling manager that coordinates all tiling operations.
pub struct TilingManager {
    /// Workspace manager.
    pub workspace_manager: workspace::WorkspaceManager,

    /// The configuration.
    config: TilingConfig,

    /// Persistent mapping of workspace name → PIDs.
    /// This survives across workspace switches even when window IDs change.
    workspace_pids: HashMap<String, HashSet<i32>>,

    /// Tracks which workspaces have had their `preset_on_open` applied.
    /// This ensures the preset is only applied once when first switching to a workspace.
    preset_applied_workspaces: HashSet<String>,

    /// App handle for emitting events to the frontend.
    app_handle: Option<AppHandle>,
}

impl TilingManager {
    /// Creates a new tiling manager.
    ///
    /// # Errors
    ///
    /// Returns an error if initialization fails.
    pub fn new(config: &TilingConfig, app_handle: Option<AppHandle>) -> Result<Self, TilingError> {
        // IMPORTANT: Capture the focused app BEFORE we do anything that might change focus
        // (like unhiding apps). We'll use this to determine which workspace should be focused.
        let initial_focused_pid = window::get_frontmost_app_pid();

        let mut workspace_manager = workspace::WorkspaceManager::new(config.clone());
        workspace_manager.initialize()?;

        let mut manager = Self {
            workspace_manager,
            config: config.clone(),
            workspace_pids: HashMap::new(),
            preset_applied_workspaces: HashSet::new(),
            app_handle,
        };

        // Discover existing windows and assign them to workspaces.
        // This unhides all apps first, then discovers all their windows.
        manager.discover_and_assign_windows();

        // Update focused workspace per screen to match where windows actually are
        manager.sync_focused_workspaces_with_windows();

        // Determine the initially focused workspace based on the captured focused app PID
        manager.initialize_focus_state_from_pid(initial_focused_pid);

        // Hide windows on non-focused workspaces (on each screen).
        // This is safe because hide_workspace_apps is careful not to hide apps
        // that also have windows on focused workspaces (e.g., on other screens).
        manager.hide_non_focused_workspaces();

        // Apply layouts to all focused workspaces
        manager.apply_all_layouts();

        // Focus the first window on the focused workspace
        manager.focus_initial_window();

        // Ensure the focused window is on top after we unhid all apps during discovery
        manager.send_non_focused_windows_to_back();

        Ok(manager)
    }

    /// Emits a workspaces changed event to the frontend.
    pub fn emit_workspaces_changed(&self) {
        if let Some(ref app_handle) = self.app_handle {
            let state = self.workspace_manager.state();
            let focused_workspace = state.focused_workspace.as_deref();
            let focused_window = state.focused_window;

            let workspaces: Vec<barba_shared::WorkspaceInfo> = state
                .workspaces
                .iter()
                .map(|ws| {
                    ws.to_info(
                        focused_workspace == Some(&ws.name),
                        &state.screens,
                        &state.windows,
                        focused_window,
                    )
                })
                .collect();

            let _ = app_handle.emit(events::WORKSPACES_CHANGED, workspaces);
        }
    }

    /// Emits a screen focused event to the frontend.
    pub fn emit_screen_focused(&self, screen: &str, is_main: bool, previous_screen: Option<&str>) {
        if let Some(ref app_handle) = self.app_handle {
            let _ = app_handle.emit(events::SCREEN_FOCUSED, ScreenFocusedPayload {
                screen: screen.to_string(),
                is_main,
                previous_screen: previous_screen.map(ToString::to_string),
            });
        }
    }

    /// Handles a new window appearing.
    /// Only processes truly new windows - windows that reappear after being hidden
    /// during workspace switches are ignored since they're managed by `switch_workspace`.
    pub fn handle_new_window(&mut self, window_id: u64) {
        // If we already know about this window, ignore it completely.
        if self.workspace_manager.state().windows.contains_key(&window_id) {
            return;
        }

        // Get the window info early - we need the PID for cooldown check
        let Ok(win) = window::get_window_by_id(window_id) else {
            return;
        };

        // During a workspace switch cooldown, only skip windows from apps we're already managing.
        // New apps launching should always be processed, even during cooldown.
        if is_in_switch_cooldown() {
            // Check if this PID is already tracked in any workspace
            let pid_is_managed = self.workspace_pids.values().any(|pids| pids.contains(&win.pid));
            if pid_is_managed {
                // This is likely a window reappearing after being unhidden, skip it
                return;
            }
            // Otherwise, this is a genuinely new app - process it
        }

        // Skip dialogs, sheets, and other non-tileable window types
        if window::is_dialog_or_sheet(&win) {
            return;
        }

        // Skip windows that match ignore rules (higher priority than workspace rules)
        if self.should_ignore_window(&win) {
            return;
        }

        // Find which workspace this window belongs to
        let Some(workspace_name) = self.find_workspace_for_window(&win) else {
            return;
        };

        // Track PID for this workspace (persists even when window IDs change)
        self.workspace_pids.entry(workspace_name.clone()).or_default().insert(win.pid);

        // Check if this workspace is focused on its screen, get layout mode, and preset_on_open
        let (is_workspace_focused, is_floating_layout, preset_on_open) = {
            if let Some(ws) = self.workspace_manager.state().get_workspace(&workspace_name) {
                let focused_on_screen =
                    self.workspace_manager.state().focused_workspace_per_screen.get(&ws.screen);
                let is_focused = focused_on_screen == Some(&workspace_name);
                let is_floating = ws.layout == barba_shared::LayoutMode::Floating;

                // Get preset_on_open from workspace config
                let preset = self
                    .config
                    .workspaces
                    .iter()
                    .find(|wc| wc.name == workspace_name)
                    .and_then(|wc| wc.preset_on_open.clone());

                (is_focused, is_floating, preset)
            } else {
                (false, false, None)
            }
        };

        // Add window to state (with workspace assignment)
        let mut win = win;
        win.workspace = workspace_name.clone();
        self.workspace_manager.state_mut().windows.insert(window_id, win);

        // Add to workspace
        if let Some(ws) = self.workspace_manager.state_mut().get_workspace_mut(&workspace_name)
            && !ws.windows.contains(&window_id)
        {
            ws.windows.push(window_id);
        }

        // If this is a floating workspace with preset-on-open, apply the preset
        if is_floating_layout && let Some(preset_name) = preset_on_open {
            // Apply preset to the new window
            if let Err(e) = self.apply_preset_to_window(window_id, &preset_name) {
                eprintln!("barba: failed to apply preset-on-open: {e}");
            }
            // Mark the workspace as having had preset applied
            // This prevents re-applying when switching workspaces later
            self.preset_applied_workspaces.insert(workspace_name.clone());
        }

        // Only apply layout for truly new windows on the focused workspace
        if is_workspace_focused {
            let ws_name = workspace_name.clone();

            // Invalidate window list cache to get fresh data
            crate::tiling::window::invalidate_window_list_cache();

            // Apply layout immediately (will use managed state for windows not yet in CGWindowList)
            let _ = self.apply_layout(&ws_name);

            // Schedule a delayed layout to ensure window is properly positioned
            // after macOS has fully registered it
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(50));
                crate::tiling::window::invalidate_window_list_cache();
                if let Some(manager) = crate::tiling::try_get_manager() {
                    let mut guard = manager.write();
                    let _ = guard.apply_layout(&ws_name);
                }
            });
        } else {
            // Window appeared for a non-focused workspace - switch to that workspace
            // so the user sees the newly launched app
            let _ = self.switch_workspace(&workspace_name);
        }

        // Notify frontend about the new window
        self.emit_workspaces_changed();
    }

    /// Handles a window being destroyed.
    pub fn handle_window_destroyed(&mut self, window_id: u64) {
        // During cooldown, windows "disappearing" are likely just being hidden
        // as part of a workspace switch, not actually destroyed
        if is_in_switch_cooldown() {
            return;
        }

        // Find which workspace had this window
        let workspace_name: Option<String> = self
            .workspace_manager
            .state()
            .workspaces
            .iter()
            .find(|ws| ws.windows.contains(&window_id))
            .map(|ws| ws.name.clone());

        // Remove from workspace
        if let Some(ref ws_name) = workspace_name
            && let Some(ws) = self.workspace_manager.state_mut().get_workspace_mut(ws_name)
        {
            ws.windows.retain(|id| *id != window_id);
        }

        // Remove from state
        self.workspace_manager.state_mut().windows.remove(&window_id);

        // Re-apply layout if we found the workspace
        if let Some(ref ws_name) = workspace_name {
            let _ = self.apply_layout(ws_name);
        }

        // Notify frontend about the window removal
        self.emit_workspaces_changed();
    }

    /// Handles a screen configuration change (display added/removed/changed).
    ///
    /// This reinitializes screens and workspaces while preserving window state,
    /// then reapplies layouts to all visible workspaces.
    pub fn handle_screen_change(&mut self) {
        use crate::tiling::window::{clear_cache, invalidate_window_list_cache};

        // Clear the AX element cache as screen changes may affect window references
        clear_cache();
        // Also invalidate window list cache
        invalidate_window_list_cache();

        if let Err(e) = self.workspace_manager.reinitialize_screens() {
            eprintln!("barba: failed to reinitialize screens: {e}");
            return;
        }

        // Hide windows on non-focused workspaces
        self.hide_non_focused_workspaces();

        // Apply layouts to all focused workspaces
        self.apply_all_layouts();

        self.emit_workspaces_changed();
    }
}

/// Global tiling manager instance.
static TILING_MANAGER: OnceLock<RwLock<TilingManager>> = OnceLock::new();

/// Initializes the tiling window manager.
///
/// This should be called during app setup. It will:
/// 1. Check if tiling is enabled in config
/// 2. Request accessibility permissions if needed
/// 3. Enumerate screens and create default workspaces
/// 4. Start watching for window events
/// 5. Initialize the animation system
///
/// Errors are logged but not propagated - tiling is non-critical functionality.
pub fn init(app_handle: &AppHandle) {
    let config = config::get_config();

    if !config.tiling.enabled {
        return;
    }

    // Validate configuration and log any warnings/errors
    let _ = config.tiling.validate_and_log();

    // Check accessibility permissions
    if !accessibility::is_accessibility_enabled() {
        eprintln!("barba: accessibility permissions not granted, tiling will be limited");
    }

    // Initialize the animation system
    super::animation::init(&config.tiling.animations);

    // Initialize the manager
    let manager = match TilingManager::new(&config.tiling, Some(app_handle.clone())) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("barba: failed to initialize tiling window manager: {e}");
            return;
        }
    };

    // Store globally
    let _ = TILING_MANAGER.set(RwLock::new(manager));

    // Start screen configuration change watcher
    super::screen::start_screen_watcher();

    // Start AXObserver-based window event observer
    super::observer::start_observing(app_handle.clone());
}

/// Returns a reference to the global tiling manager if initialized.
pub fn try_get_manager() -> Option<&'static RwLock<TilingManager>> { TILING_MANAGER.get() }
