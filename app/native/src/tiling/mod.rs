// Allow unused imports/code in this module - many public exports are for external CLI/IPC use
#![allow(dead_code)]
#![allow(unused_imports)]

//! Tiling Window Manager for Stache.
//!
//! This module provides a tiling window manager with virtual workspace support,
//! multiple layout modes, and keyboard-centric window management.
//!
//! # Features
//!
//! - Virtual workspaces with configurable rules for window assignment
//! - Multiple layout modes: Dwindle, Monocle, Split, Master, Floating
//! - Multi-monitor support with per-screen workspace assignment
//! - Window animations for smooth transitions
//! - Gaps between windows and screen edges
//! - Floating window presets for quick positioning
//!
//! # Window Management Approach
//!
//! Unlike some tiling managers that move windows to screen corners, this
//! implementation uses macOS native window hiding (similar to Cmd+H) for
//! workspace switching. This provides cleaner transitions and better
//! integration with macOS focus handling.
//!
//! # Usage
//!
//! The tiling manager is controlled via CLI commands:
//!
//! ```bash
//! # Query commands
//! stache tiling query screens
//! stache tiling query workspaces
//! stache tiling query windows
//!
//! # Workspace commands
//! stache tiling workspace --focus <name>
//! stache tiling workspace --layout <layout>
//!
//! # Window commands
//! stache tiling window --focus <direction>
//! stache tiling window --swap <direction>
//! ```

pub mod animation;
pub mod app_monitor;
pub mod borders;
pub mod constants;
pub mod drag_state;
pub mod error;
mod event_coalescer;
mod event_handlers;
pub mod ffi;
pub mod layout;
pub mod manager;
pub mod mouse_monitor;
pub mod observer;
pub mod rules;
pub mod screen;
pub mod screen_monitor;
pub mod state;
pub mod window;
pub mod workspace;

#[cfg(test)]
pub mod testing;

// Re-export commonly used types
use std::sync::Mutex;

pub use animation::{
    AnimationConfig, AnimationSystem, WindowTransition, begin_animation, cancel_animation,
    get_interrupted_position,
};
pub use error::{TilingError, TilingResult};
pub use layout::{calculate_preset_frame, find_preset, list_preset_names};
pub use manager::{TilingManager, WorkspaceSwitchInfo, get_manager, init_manager};
pub use observer::{WindowEvent, WindowEventType};
pub use rules::{
    WorkspaceMatch, any_rule_matches, count_matching_rules, find_matching_workspace, matches_window,
};
pub use screen::{get_all_screens, get_main_screen, get_screen_by_name, get_screen_count};
pub use state::{Point, Rect, Screen, TilingState, TrackedWindow, Workspace};
pub use window::{
    AppInfo, CGWindowInfo, WindowInfo, focus_window, get_all_windows,
    get_all_windows_including_hidden, get_cg_window_list, get_cg_window_list_all,
    get_focused_window, get_running_apps, get_screen_for_window, hide_app, hide_window,
    move_window_to_screen, set_window_frame, show_window, unhide_app,
};
pub use workspace::{
    FocusHistory, WindowAssignment, WorkspaceSwitchResult, assign_window_to_workspace,
    find_workspace_for_window, get_visible_workspace_for_screen, get_workspaces_for_screen,
    hide_workspace_windows, should_ignore_window, show_workspace_windows, workspace_exists,
};

use crate::config::get_config;
use crate::{events, is_accessibility_granted};

// ============================================================================
// App Handle for Emitting Events
// ============================================================================

/// Stored Tauri app handle for emitting events from event handlers.
static APP_HANDLE: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

/// Stores the Tauri app handle for later use in event emission.
fn store_app_handle(handle: tauri::AppHandle) {
    if let Ok(mut stored) = APP_HANDLE.lock() {
        *stored = Some(handle);
    }
}

/// Emits a window focus changed event to the frontend.
///
/// This is called from event handlers when focus changes due to user interaction
/// (clicking on windows, etc.) rather than programmatic commands.
pub fn emit_window_focus_changed(window_id: u32, workspace: &str) {
    if let Ok(handle) = APP_HANDLE.lock()
        && let Some(ref app) = *handle
    {
        use tauri::Emitter;
        let _ = app.emit(
            events::tiling::WINDOW_FOCUS_CHANGED,
            serde_json::json!({
                "windowId": window_id,
                "workspace": workspace,
            }),
        );
    }
}

/// Emits a workspace changed event to the frontend.
///
/// This is called from event handlers when workspace visibility changes due to
/// focusing a window on a different workspace.
pub fn emit_workspace_changed(workspace: &str, screen: &str, previous_workspace: Option<&str>) {
    if let Ok(handle) = APP_HANDLE.lock()
        && let Some(ref app) = *handle
    {
        use tauri::Emitter;
        let _ = app.emit(
            events::tiling::WORKSPACE_CHANGED,
            serde_json::json!({
                "workspace": workspace,
                "screen": screen,
                "previousWorkspace": previous_workspace,
            }),
        );
    }
}

/// Emits a window tracked event to the frontend.
///
/// This is called when a new window is tracked by the tiling manager.
pub fn emit_window_tracked(window_id: u32, workspace: &str) {
    if let Ok(handle) = APP_HANDLE.lock()
        && let Some(ref app) = *handle
    {
        use tauri::Emitter;
        let _ = app.emit(
            events::tiling::WINDOW_TRACKED,
            serde_json::json!({
                "windowId": window_id,
                "workspace": workspace,
            }),
        );
    }
}

/// Emits a window untracked event to the frontend.
///
/// This is called when a window is no longer tracked by the tiling manager.
pub fn emit_window_untracked(window_id: u32, workspace: &str) {
    if let Ok(handle) = APP_HANDLE.lock()
        && let Some(ref app) = *handle
    {
        use tauri::Emitter;
        let _ = app.emit(
            events::tiling::WINDOW_UNTRACKED,
            serde_json::json!({
                "windowId": window_id,
                "workspace": workspace,
            }),
        );
    }
}

/// Emits a workspace windows changed event to the frontend.
///
/// This is called when windows are added to or removed from a workspace.
pub fn emit_workspace_windows_changed(workspace: &str, window_ids: &[u32]) {
    eprintln!(
        "stache: tiling: emit_workspace_windows_changed: workspace={workspace}, window_ids={window_ids:?}"
    );
    if let Ok(handle) = APP_HANDLE.lock()
        && let Some(ref app) = *handle
    {
        use tauri::Emitter;
        let result = app.emit(
            events::tiling::WORKSPACE_WINDOWS_CHANGED,
            serde_json::json!({
                "workspace": workspace,
                "windows": window_ids,
            }),
        );
        eprintln!("stache: tiling: emit_workspace_windows_changed: emit result={result:?}");
    } else {
        eprintln!("stache: tiling: emit_workspace_windows_changed: no app handle available");
    }
}

// ============================================================================
// Initialization
// ============================================================================

/// Initializes the tiling window manager.
///
/// This function checks if tiling is enabled in the configuration,
/// verifies accessibility permissions, and sets up window tracking
/// and event observers.
///
/// # Arguments
///
/// * `app_handle` - Optional Tauri app handle for emitting events.
///
/// # Behavior
///
/// - If `tiling.enabled` is `false` in config, this function returns immediately
/// - If accessibility permissions are not granted, returns early (warning already logged in lib.rs)
/// - On successful initialization, begins tracking windows and managing workspaces
pub fn init(app_handle: tauri::AppHandle) {
    let config = get_config();

    // Tiling is disabled by default
    if !config.tiling.is_enabled() {
        return;
    }

    // Use cached accessibility permission check from lib.rs
    if !is_accessibility_granted() {
        return;
    }

    // Store the app handle for event emission from event handlers
    store_app_handle(app_handle.clone());

    // Initialize the tiling manager
    if !init_manager(Some(app_handle)) {
        return;
    }

    eprintln!("stache: tiling: manager initialized");

    // Initialize the border system (must be before tracking windows)
    if borders::init() && config.tiling.borders.is_enabled() {
        eprintln!("stache: tiling: borders initialized");
    }

    // Track existing windows
    track_existing_windows();

    // Initialize the mouse monitor for drag detection
    if mouse_monitor::init() {
        // Set up the callback for when mouse is released
        mouse_monitor::set_mouse_up_callback(event_handlers::on_mouse_up);
    }

    // Initialize the observer for window events
    if observer::init(event_handlers::handle_window_event) {
        eprintln!(
            "stache: tiling: observer initialized, watching {} apps",
            observer::observer_count()
        );
    }

    // Initialize the screen monitor for hotplug detection
    if screen_monitor::init(event_handlers::handle_screen_change) {
        eprintln!("stache: tiling: screen monitor initialized");
    }

    // Initialize the app lifecycle monitor for tracking app launches and terminations
    if app_monitor::init(
        event_handlers::handle_app_launch,
        event_handlers::handle_app_terminate,
    ) {
        eprintln!("stache: tiling: app lifecycle monitor initialized");
    }

    // Apply startup behavior: switch to workspace containing focused window
    apply_startup_behavior();

    eprintln!("stache: tiling: initialization complete");
}

// ============================================================================
// Window Tracking
// ============================================================================

/// Tracks all existing windows on startup.
///
/// Determines the initial focused workspace based on the currently focused window,
/// then uses that as the fallback for windows that don't match any rules.
fn track_existing_windows() {
    let Some(manager) = get_manager() else {
        return;
    };

    // Determine the initial focused workspace BEFORE tracking windows
    // This ensures non-matching windows go to the correct workspace
    let initial_focused_workspace = determine_initial_focused_workspace();

    let mut mgr = manager.write();
    if !mgr.is_enabled() {
        return;
    }

    // Set the focused workspace before tracking so the fallback is correct
    if let Some(ref ws_name) = initial_focused_workspace {
        mgr.set_focused_workspace_name(ws_name);
    }

    mgr.track_existing_windows();
    let window_count = mgr.get_windows().len();
    let workspace_count = mgr.get_workspaces().len();
    drop(mgr);

    eprintln!("stache: tiling: tracked {window_count} windows across {workspace_count} workspaces");

    // Move windows to their assigned screens
    move_windows_to_assigned_screens();
}

/// Determines the initial focused workspace based on the currently focused window.
///
/// If the focused window matches a workspace rule, returns that workspace.
/// Otherwise, returns the first workspace on the main screen.
fn determine_initial_focused_workspace() -> Option<String> {
    let focused = get_focused_window()?;
    let workspace_configs = workspace::get_workspace_configs();

    // Check if the focused window matches any workspace rule
    let workspaces = workspace_configs.iter().map(|ws| (ws.name.as_str(), ws.rules.as_slice()));
    if let Some(match_result) = rules::find_matching_workspace(&focused, workspaces) {
        return Some(match_result.workspace_name);
    }

    // No rule matched - return the first workspace on the main screen
    let config = crate::config::get_config();
    config.tiling.workspaces.first().map(|ws| ws.name.clone())
}

/// Moves windows to their assigned workspace's screen.
///
/// For each tracked window, checks if it's on the correct screen (the screen
/// where its workspace is assigned). If not, moves it to the correct screen.
fn move_windows_to_assigned_screens() {
    let Some(manager) = get_manager() else {
        return;
    };

    let mgr = manager.read();
    if !mgr.is_enabled() {
        return;
    }

    let screens = mgr.get_screens().to_vec();

    // Collect window info for moving (to avoid holding lock during moves)
    let mut windows_to_move: Vec<(u32, String, String, Screen, Option<Screen>)> = Vec::new();

    for window in mgr.get_windows() {
        // Get the workspace this window belongs to
        let Some(workspace) = mgr.get_workspace(&window.workspace_name) else {
            continue;
        };

        // Get the target screen for this workspace
        let Some(target_screen) = screens.iter().find(|s| s.id == workspace.screen_id) else {
            continue;
        };

        // Determine which screen the window is currently on
        let current_screen = get_screen_for_window(&window.frame, &screens);

        // Check if window needs to be moved
        let needs_move = current_screen.is_none_or(|cs| cs.id != target_screen.id);

        if needs_move {
            windows_to_move.push((
                window.id,
                window.app_name.clone(),
                window.workspace_name.clone(),
                target_screen.clone(),
                current_screen.cloned(),
            ));
        }
    }

    drop(mgr); // Release lock before doing moves

    if windows_to_move.is_empty() {
        return;
    }

    // Move windows to their target screens and collect successful moves
    let mut moved_windows: Vec<(u32, Rect)> = Vec::new();

    for (window_id, _app_name, _workspace_name, target_screen, current_screen) in windows_to_move {
        if move_window_to_screen(window_id, &target_screen, current_screen.as_ref()) {
            // Get the window's new frame after moving
            if let Some(new_frame) = get_window_frame_by_id(window_id) {
                moved_windows.push((window_id, new_frame));
            }
        }
    }

    // Update tracked window frames in the manager
    if !moved_windows.is_empty() {
        let mut mgr = manager.write();
        for (window_id, new_frame) in &moved_windows {
            mgr.update_window_frame(*window_id, *new_frame);
        }
    }
}

/// Gets a window's current frame by ID.
fn get_window_frame_by_id(window_id: u32) -> Option<Rect> {
    let windows = get_all_windows();
    windows.iter().find(|w| w.id == window_id).map(|w| w.frame)
}

/// Applies startup behavior: set initial workspace visibility based on focused window.
///
/// This should be called AFTER windows are tracked. It will:
/// - Detect the currently focused window
/// - Set the workspace containing that window as visible/focused on its screen
/// - For all other screens, set the first workspace (in config order) as visible
#[allow(clippy::significant_drop_tightening)] // Lock guard scope is intentional
fn apply_startup_behavior() {
    let Some(manager) = get_manager() else {
        return;
    };

    // Get the currently focused window
    let focused = get_focused_window();

    // Get the focused window ID if it's tracked
    let focused_window_id = {
        let mgr = manager.read();
        if !mgr.is_enabled() {
            return;
        }

        focused.as_ref().and_then(|fw| mgr.get_window(fw.id).map(|_| fw.id))
    };

    // Set initial workspace visibility
    let mut mgr = manager.write();
    mgr.set_initial_workspace_visibility(focused_window_id);

    // Now hide windows from non-visible workspaces and show windows from visible ones
    apply_initial_window_visibility(&mgr);

    // Apply layouts to all visible workspaces (uses instant positioning since not yet initialized)
    apply_initial_layouts(&mut mgr);

    // Mark the manager as initialized - from now on, animations will be used (if enabled)
    mgr.mark_initialized();
}

/// Applies initial window visibility based on workspace visibility.
///
/// For visible workspaces: Shows (unhides) all windows and their borders.
/// For non-visible workspaces: Hides all windows and their borders.
fn apply_initial_window_visibility(mgr: &TilingManager) {
    use workspace::{hide_workspace_windows, show_workspace_windows};

    for ws in mgr.get_workspaces() {
        let windows: Vec<_> = mgr.get_windows_for_workspace(&ws.name);

        if ws.is_visible {
            TilingManager::show_borders_for_workspace(&ws.name);
            if !windows.is_empty() {
                let _ = show_workspace_windows(&windows);
            }
        } else {
            TilingManager::hide_borders_for_workspace(&ws.name);
            if !windows.is_empty() {
                let _ = hide_workspace_windows(&windows);
            }
        }
    }
}

/// Applies layouts to all visible workspaces on startup.
fn apply_initial_layouts(mgr: &mut TilingManager) {
    let visible_workspaces: Vec<String> = mgr
        .get_workspaces()
        .iter()
        .filter(|ws| ws.is_visible)
        .map(|ws| ws.name.clone())
        .collect();

    for ws_name in visible_workspaces {
        mgr.apply_layout_forced(&ws_name);
    }

    // Update border colors based on the focused workspace's layout
    // This ensures the correct colors (e.g., monocle) are applied at startup
    if let Some((is_monocle, is_floating)) = event_handlers::get_focused_workspace_layout(mgr) {
        borders::janky::update_colors_for_state(is_monocle, is_floating);
    }
}

// ============================================================================
// IPC Query Handler
// ============================================================================

use crate::utils::ipc_socket::{IpcQuery, IpcResponse};

/// Handles IPC queries for tiling state.
///
/// This is called by the IPC server when a query is received from the CLI.
#[must_use]
pub fn handle_ipc_query(query: IpcQuery) -> IpcResponse {
    match query {
        IpcQuery::Ping => IpcResponse::success(serde_json::json!({"status": "ok"})),
        IpcQuery::Screens => query_screens(),
        IpcQuery::Workspaces { screen, focused_screen } => {
            query_workspaces(screen.as_deref(), focused_screen)
        }
        IpcQuery::Windows {
            screen,
            workspace,
            focused_screen,
            focused_workspace,
        } => query_windows(
            screen.as_deref(),
            workspace.as_deref(),
            focused_screen,
            focused_workspace,
        ),
    }
}

/// Queries all screens.
fn query_screens() -> IpcResponse {
    let screens = get_all_screens();
    IpcResponse::success(screens)
}

/// Queries workspaces with optional filters.
#[allow(clippy::significant_drop_tightening)] // Guard needs to be held for entire function
fn query_workspaces(screen_filter: Option<&str>, focused_screen: bool) -> IpcResponse {
    let Some(manager) = get_manager() else {
        return IpcResponse::error("Tiling not initialized");
    };

    let mgr = manager.read();
    if !mgr.is_enabled() {
        return IpcResponse::error("Tiling not enabled");
    }

    let mut workspaces: Vec<serde_json::Value> = Vec::new();

    // Determine which screen(s) to filter by
    let filter_screen_id: Option<u32> = if focused_screen {
        mgr.get_focused_screen().map(|s| s.id)
    } else if let Some(name) = screen_filter {
        match mgr.get_screen_by_name(name) {
            Some(s) => Some(s.id),
            None => return IpcResponse::error(format!("Screen '{name}' not found")),
        }
    } else {
        None
    };

    for ws in mgr.get_workspaces() {
        // Apply screen filter if specified
        if let Some(screen_id) = filter_screen_id
            && ws.screen_id != screen_id
        {
            continue;
        }

        let screen_name = mgr
            .get_screen(ws.screen_id)
            .map_or_else(|| "unknown".to_string(), |s| s.name.clone());

        workspaces.push(serde_json::json!({
            "name": ws.name,
            "screenId": ws.screen_id,
            "screenName": screen_name,
            "layout": format!("{:?}", ws.layout).to_lowercase(),
            "isVisible": ws.is_visible,
            "isFocused": ws.is_focused,
            "windowCount": ws.window_ids.len(),
            "windowIds": ws.window_ids,
        }));
    }

    IpcResponse::success(workspaces)
}

/// Queries windows with optional filters.
#[allow(clippy::significant_drop_tightening)] // Guard needs to be held for entire function
fn query_windows(
    screen_filter: Option<&str>,
    workspace_filter: Option<&str>,
    focused_screen: bool,
    focused_workspace: bool,
) -> IpcResponse {
    let Some(manager) = get_manager() else {
        return IpcResponse::error("Tiling not initialized");
    };

    let mgr = manager.read();
    if !mgr.is_enabled() {
        return IpcResponse::error("Tiling not enabled");
    }

    // Get the currently focused window ID
    let focused_window_id = get_focused_window().map(|w| w.id);

    // Determine filter criteria
    let filter_screen_id: Option<u32> = if focused_screen {
        mgr.get_focused_screen().map(|s| s.id)
    } else if let Some(name) = screen_filter {
        match mgr.get_screen_by_name(name) {
            Some(s) => Some(s.id),
            None => return IpcResponse::error(format!("Screen '{name}' not found")),
        }
    } else {
        None
    };

    let filter_workspace: Option<String> = if focused_workspace {
        mgr.state().focused_workspace.clone()
    } else {
        workspace_filter.map(String::from)
    };

    // Validate workspace filter if specified
    if let Some(ref ws_name) = filter_workspace
        && mgr.get_workspace(ws_name).is_none()
    {
        return IpcResponse::error(format!("Workspace '{ws_name}' not found"));
    }

    // Build list of workspaces to include (for screen filtering)
    let workspace_names_for_screen: Option<Vec<String>> = filter_screen_id.map(|screen_id| {
        mgr.get_workspaces()
            .iter()
            .filter(|ws| ws.screen_id == screen_id)
            .map(|ws| ws.name.clone())
            .collect()
    });

    let mut windows: Vec<serde_json::Value> = Vec::new();

    for w in mgr.get_windows() {
        // Apply workspace filter
        if let Some(ref ws_name) = filter_workspace
            && !w.workspace_name.eq_ignore_ascii_case(ws_name)
        {
            continue;
        }

        // Apply screen filter (via workspace)
        if let Some(ref ws_names) = workspace_names_for_screen
            && !ws_names.iter().any(|name| name.eq_ignore_ascii_case(&w.workspace_name))
        {
            continue;
        }

        let is_focused = focused_window_id == Some(w.id);

        windows.push(serde_json::json!({
            "id": w.id,
            "pid": w.pid,
            "appId": w.app_id,
            "appName": w.app_name,
            "title": w.title,
            "workspace": w.workspace_name,
            "isFocused": is_focused,
            "frame": {
                "x": w.frame.x,
                "y": w.frame.y,
                "width": w.frame.width,
                "height": w.frame.height,
            },
        }));
    }

    IpcResponse::success(windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_compiles() {
        // Basic sanity check that the module structure is valid
        assert!(true);
    }

    #[test]
    fn test_exports() {
        // Verify that re-exports work
        let _point = Point::new(0.0, 0.0);
        let _rect = Rect::new(0.0, 0.0, 100.0, 100.0);
    }

    #[test]
    fn test_screen_functions_available() {
        let count = get_screen_count();
        assert!(count >= 1);

        let screens = get_all_screens();
        assert!(!screens.is_empty());

        let main = get_main_screen();
        assert!(main.is_some());
    }

    // Note: Drag-and-drop tests are now in event_handlers.rs
}
