//! Tauri commands that expose tiling window manager data to the frontend.
//!
//! These commands provide a bridge between the internal tiling manager and the UI,
//! allowing the Spaces component to query workspace and window information.

use parking_lot::{RwLockReadGuard, RwLockWriteGuard};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::error::StacheError;
use crate::events;
use crate::tiling::{self, TilingManager};

// ============================================================================
// Response Types
// ============================================================================

/// Information about a workspace, serialized for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
    /// Unique name of the workspace.
    pub name: String,
    /// The screen ID this workspace is on.
    pub screen_id: u32,
    /// The screen name this workspace is on.
    pub screen_name: String,
    /// Current layout type (as a lowercase string).
    pub layout: String,
    /// Whether this workspace is currently visible on its screen.
    pub is_visible: bool,
    /// Whether this workspace is currently focused.
    pub is_focused: bool,
    /// Number of windows in this workspace.
    pub window_count: usize,
    /// IDs of windows in this workspace.
    pub window_ids: Vec<u32>,
}

/// Information about a window, serialized for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    /// Unique window ID.
    pub id: u32,
    /// Process ID of the owning application.
    pub pid: i32,
    /// Bundle identifier of the application.
    pub app_id: String,
    /// Name of the application.
    pub app_name: String,
    /// Window title.
    pub title: String,
    /// Name of the workspace this window belongs to.
    pub workspace: String,
    /// Whether this window is currently focused.
    pub is_focused: bool,
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Result type for manager access operations.
type ManagerResult<T> = Result<T, StacheError>;

/// Gets the tiling manager with read access, validating it's initialized and enabled.
fn with_manager_read<F, T>(f: F) -> ManagerResult<T>
where F: FnOnce(RwLockReadGuard<'_, TilingManager>) -> ManagerResult<T> {
    let manager = tiling::get_manager()
        .ok_or_else(|| StacheError::TilingError("Tiling not initialized".to_string()))?;

    let mgr = manager.read();
    if !mgr.is_enabled() {
        return Err(StacheError::TilingError("Tiling not enabled".to_string()));
    }

    f(mgr)
}

/// Gets the tiling manager with write access, validating it's initialized and enabled.
fn with_manager_write<F, T>(f: F) -> ManagerResult<T>
where F: FnOnce(RwLockWriteGuard<'_, TilingManager>) -> ManagerResult<T> {
    let manager = tiling::get_manager()
        .ok_or_else(|| StacheError::TilingError("Tiling not initialized".to_string()))?;

    let mgr = manager.write();
    if !mgr.is_enabled() {
        return Err(StacheError::TilingError("Tiling not enabled".to_string()));
    }

    f(mgr)
}

/// Converts a tracked window to a `WindowInfo` struct.
fn to_window_info(w: &tiling::TrackedWindow, focused_window_id: Option<u32>) -> WindowInfo {
    WindowInfo {
        id: w.id,
        pid: w.pid,
        app_id: w.app_id.clone(),
        app_name: w.app_name.clone(),
        title: w.title.clone(),
        workspace: w.workspace_name.clone(),
        is_focused: focused_window_id == Some(w.id),
    }
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Gets all workspaces from the tiling manager.
///
/// Returns workspaces for all screens, or for a specific screen if specified.
#[tauri::command]
pub fn get_tiling_workspaces(screen: Option<String>) -> Result<Vec<WorkspaceInfo>, StacheError> {
    with_manager_read(|mgr| {
        // Determine screen filter
        let filter_screen_id: Option<u32> = if let Some(name) = screen {
            match mgr.get_screen_by_name(&name) {
                Some(s) => Some(s.id),
                None => return Err(StacheError::TilingError(format!("Screen '{name}' not found"))),
            }
        } else {
            None
        };

        let workspaces: Vec<WorkspaceInfo> = mgr
            .get_workspaces()
            .iter()
            .filter(|ws| filter_screen_id.is_none_or(|id| ws.screen_id == id))
            .map(|ws| {
                let screen_name = mgr
                    .get_screen(ws.screen_id)
                    .map_or_else(|| "unknown".to_string(), |s| s.name.clone());

                // Get only tiled (non-minimized) window IDs
                let tiled_window_ids = mgr.get_tiled_window_ids(&ws.name);

                WorkspaceInfo {
                    name: ws.name.clone(),
                    screen_id: ws.screen_id,
                    screen_name,
                    layout: format!("{:?}", ws.layout).to_lowercase(),
                    is_visible: ws.is_visible,
                    is_focused: ws.is_focused,
                    window_count: tiled_window_ids.len(),
                    window_ids: tiled_window_ids,
                }
            })
            .collect();

        Ok(workspaces)
    })
}

/// Gets all windows from the tiling manager.
///
/// Can filter by workspace name.
///
/// Only returns tiled (non-minimized) windows.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands require owned values
pub fn get_tiling_windows(workspace: Option<String>) -> Result<Vec<WindowInfo>, StacheError> {
    with_manager_read(|mgr| {
        let focused_window_id = tiling::get_focused_window().map(|w| w.id);

        // Filter out minimized windows - they should not appear in the UI
        let windows: Vec<WindowInfo> = mgr
            .get_windows()
            .iter()
            .filter(|w| {
                !w.is_minimized
                    && workspace
                        .as_ref()
                        .is_none_or(|ws_name| w.workspace_name.eq_ignore_ascii_case(ws_name))
            })
            .map(|w| to_window_info(w, focused_window_id))
            .collect();

        Ok(windows)
    })
}

/// Gets the currently focused workspace name.
#[tauri::command]
pub fn get_tiling_focused_workspace() -> Result<Option<String>, StacheError> {
    with_manager_read(|mgr| Ok(mgr.state().focused_workspace.clone()))
}

/// Gets the currently focused window.
#[tauri::command]
pub fn get_tiling_focused_window() -> Result<Option<WindowInfo>, StacheError> {
    with_manager_read(|mgr| {
        let Some(focused) = tiling::get_focused_window() else {
            return Ok(None);
        };

        let window_info = mgr
            .get_windows()
            .iter()
            .find(|w| w.id == focused.id)
            .map(|w| to_window_info(w, Some(focused.id)));

        Ok(window_info)
    })
}

/// Switches to a workspace by name.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands require owned values
pub fn focus_tiling_workspace(app: AppHandle, name: String) -> Result<(), StacheError> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(StacheError::InvalidArguments(
            "Workspace name cannot be empty".to_string(),
        ));
    }

    let (switch_info,) = with_manager_write(|mut mgr| {
        if mgr.get_workspace(&name).is_none() {
            return Err(StacheError::TilingError(format!("Workspace '{name}' not found")));
        }

        let info = mgr.switch_workspace(&name);

        Ok((info,))
    })?;

    // Emit workspace changed event if switch was successful
    if let Some(info) = switch_info {
        let _ = app.emit(
            events::tiling::WORKSPACE_CHANGED,
            serde_json::json!({
                "workspace": info.workspace,
                "screen": info.screen,
            }),
        );
    }

    Ok(())
}

/// Focuses a window by its ID.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands require owned AppHandle
pub fn focus_tiling_window(app: AppHandle, window_id: u32) -> Result<(), StacheError> {
    let workspace_name = with_manager_write(|mut mgr| {
        let window = mgr
            .get_windows()
            .iter()
            .find(|w| w.id == window_id)
            .map(|w| w.workspace_name.clone());

        let Some(workspace) = window else {
            return Err(StacheError::TilingError(format!("Window {window_id} not found")));
        };

        mgr.focus_window_by_id(window_id);
        Ok(workspace)
    })?;

    // Emit focus changed event
    let _ = app.emit(
        events::tiling::WINDOW_FOCUS_CHANGED,
        serde_json::json!({
            "windowId": window_id,
            "workspace": workspace_name,
        }),
    );

    Ok(())
}

/// Checks if the tiling manager is initialized and enabled.
///
/// This is used by the frontend to check if it should render tiling-dependent UI.
#[tauri::command]
pub fn is_tiling_enabled() -> bool { tiling::get_manager().is_some_and(|m| m.read().is_enabled()) }

/// Gets windows for the currently focused workspace.
///
/// This is a convenience function that combines getting the focused workspace
/// and then getting windows for that workspace.
///
/// Only returns tiled (non-minimized) windows.
#[tauri::command]
pub fn get_tiling_current_workspace_windows() -> Result<Vec<WindowInfo>, StacheError> {
    with_manager_read(|mgr| {
        let Some(focused_workspace) = mgr.state().focused_workspace.clone() else {
            return Ok(Vec::new());
        };

        let focused_window_id = tiling::get_focused_window().map(|w| w.id);

        // Filter out minimized windows - they should not appear in the UI
        let windows: Vec<WindowInfo> = mgr
            .get_windows()
            .iter()
            .filter(|w| {
                w.workspace_name.eq_ignore_ascii_case(&focused_workspace) && !w.is_minimized
            })
            .map(|w| to_window_info(w, focused_window_id))
            .collect();

        Ok(windows)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_info_serializes_correctly() {
        let info = WorkspaceInfo {
            name: "terminal".to_string(),
            screen_id: 1,
            screen_name: "Built-in Display".to_string(),
            layout: "dwindle".to_string(),
            is_visible: true,
            is_focused: true,
            window_count: 2,
            window_ids: vec![100, 101],
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"name\":\"terminal\""));
        assert!(json.contains("\"screenId\":1"));
        assert!(json.contains("\"screenName\":\"Built-in Display\""));
        assert!(json.contains("\"layout\":\"dwindle\""));
        assert!(json.contains("\"isVisible\":true"));
        assert!(json.contains("\"isFocused\":true"));
        assert!(json.contains("\"windowCount\":2"));
    }

    #[test]
    fn window_info_serializes_correctly() {
        let info = WindowInfo {
            id: 123,
            pid: 456,
            app_id: "com.apple.Terminal".to_string(),
            app_name: "Terminal".to_string(),
            title: "bash".to_string(),
            workspace: "terminal".to_string(),
            is_focused: true,
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"id\":123"));
        assert!(json.contains("\"pid\":456"));
        assert!(json.contains("\"appId\":\"com.apple.Terminal\""));
        assert!(json.contains("\"appName\":\"Terminal\""));
        assert!(json.contains("\"title\":\"bash\""));
        assert!(json.contains("\"workspace\":\"terminal\""));
        assert!(json.contains("\"isFocused\":true"));
    }
}
