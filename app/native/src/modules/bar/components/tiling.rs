//! Tauri commands that expose tiling window manager data to the frontend.
//!
//! These commands provide a bridge between the internal tiling manager and the UI,
//! allowing the Spaces component to query workspace and window information.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::error::StacheError;
use crate::events;
use crate::modules::tiling;

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
// Tauri Commands
// ============================================================================

/// Gets all workspaces from the tiling manager.
///
/// Returns workspaces for all screens, or for a specific screen if specified.
///
/// # Errors
///
/// Returns an error if the tiling manager is not available or the screen is not found.
#[tauri::command]
pub async fn get_tiling_workspaces(
    _screen: Option<String>,
) -> Result<Vec<WorkspaceInfo>, StacheError> {
    use tiling::actor::{QueryResult, StateQuery};

    let handle = tiling::init::get_handle()
        .ok_or_else(|| StacheError::TilingError("Tiling not initialized".to_string()))?;

    // Get all workspaces
    let workspaces_result = handle
        .query(StateQuery::GetAllWorkspaces)
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    let workspaces = match workspaces_result {
        QueryResult::Workspaces(ws) => ws,
        _ => return Err(StacheError::TilingError("Unexpected query result".to_string())),
    };

    // Get all screens for name lookup
    let screens_result = handle
        .query(StateQuery::GetAllScreens)
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    let screens = match screens_result {
        QueryResult::Screens(s) => s,
        _ => Vec::new(),
    };

    // Convert to WorkspaceInfo format
    let infos: Vec<WorkspaceInfo> = workspaces
        .into_iter()
        .map(|ws| {
            let screen_name = screens
                .iter()
                .find(|s| s.id == ws.screen_id)
                .map_or_else(|| "unknown".to_string(), |s| s.name.clone());

            let tiled_window_ids: Vec<u32> = ws.window_ids.clone();

            WorkspaceInfo {
                name: ws.name,
                screen_id: ws.screen_id,
                screen_name,
                layout: tiling::commands::layout_to_string_pub(ws.layout),
                is_visible: ws.is_visible,
                is_focused: ws.is_focused,
                window_count: tiled_window_ids.len(),
                window_ids: tiled_window_ids,
            }
        })
        .collect();

    Ok(infos)
}

/// Gets all windows from the tiling manager.
///
/// Can filter by workspace name.
///
/// Only returns tiled (non-minimized) windows.
///
/// # Errors
///
/// Returns an error if the tiling manager is not available.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands require owned values
pub async fn get_tiling_windows(workspace: Option<String>) -> Result<Vec<WindowInfo>, StacheError> {
    use tiling::actor::{QueryResult, StateQuery};

    let handle = tiling::init::get_handle()
        .ok_or_else(|| StacheError::TilingError("Tiling not initialized".to_string()))?;

    // Get focused window ID
    let focus_result = handle
        .query(StateQuery::GetFocusState)
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    let focused_window_id = match focus_result {
        QueryResult::Focus(f) => f.focused_window_id,
        _ => None,
    };

    // Get all windows
    let windows_result = handle
        .query(StateQuery::GetAllWindows)
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    let windows = match windows_result {
        QueryResult::Windows(w) => w,
        _ => return Err(StacheError::TilingError("Unexpected query result".to_string())),
    };

    // Get all workspaces for name lookup
    let workspaces_result = handle
        .query(StateQuery::GetAllWorkspaces)
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    let workspaces = match workspaces_result {
        QueryResult::Workspaces(ws) => ws,
        _ => Vec::new(),
    };

    // Convert to WindowInfo format
    let infos: Vec<WindowInfo> = windows
        .into_iter()
        .filter(|w| !w.is_minimized && !w.is_hidden)
        .filter(|w| {
            if let Some(ref ws_name) = workspace {
                // Find the workspace name for this window
                workspaces
                    .iter()
                    .find(|ws| ws.id == w.workspace_id)
                    .is_some_and(|ws| ws.name.eq_ignore_ascii_case(ws_name))
            } else {
                true
            }
        })
        .map(|w| {
            let workspace_name = workspaces
                .iter()
                .find(|ws| ws.id == w.workspace_id)
                .map_or_else(|| "unknown".to_string(), |ws| ws.name.clone());

            WindowInfo {
                id: w.id,
                pid: w.pid,
                app_id: w.app_id,
                app_name: w.app_name,
                title: w.title,
                workspace: workspace_name,
                is_focused: focused_window_id == Some(w.id),
            }
        })
        .collect();

    Ok(infos)
}

/// Gets the currently focused workspace name.
///
/// # Errors
///
/// Returns an error if the tiling manager is not available.
#[tauri::command]
pub async fn get_tiling_focused_workspace() -> Result<Option<String>, StacheError> {
    use tiling::actor::{QueryResult, StateQuery};

    let handle = tiling::init::get_handle()
        .ok_or_else(|| StacheError::TilingError("Tiling not initialized".to_string()))?;

    let result = handle
        .query(StateQuery::GetFocusedWorkspace)
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    match result {
        QueryResult::Workspace(Some(ws)) => Ok(Some(ws.name)),
        QueryResult::Workspace(None) => Ok(None),
        _ => Err(StacheError::TilingError("Unexpected query result".to_string())),
    }
}

/// Gets the currently focused window.
///
/// # Errors
///
/// Returns an error if the tiling manager is not available.
#[tauri::command]
pub async fn get_tiling_focused_window() -> Result<Option<WindowInfo>, StacheError> {
    use tiling::actor::{QueryResult, StateQuery};

    let handle = tiling::init::get_handle()
        .ok_or_else(|| StacheError::TilingError("Tiling not initialized".to_string()))?;

    // Get focus state
    let focus_result = handle
        .query(StateQuery::GetFocusState)
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    let focused_window_id = match focus_result {
        QueryResult::Focus(f) => f.focused_window_id,
        _ => return Ok(None),
    };

    let Some(window_id) = focused_window_id else {
        return Ok(None);
    };

    // Get the window
    let window_result = handle
        .query(StateQuery::GetWindow { id: window_id })
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    let window = match window_result {
        QueryResult::Window(Some(w)) => w,
        _ => return Ok(None),
    };

    // Get workspaces for name lookup
    let workspaces_result = handle
        .query(StateQuery::GetAllWorkspaces)
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    let workspaces = match workspaces_result {
        QueryResult::Workspaces(ws) => ws,
        _ => Vec::new(),
    };

    let workspace_name = workspaces
        .iter()
        .find(|ws| ws.id == window.workspace_id)
        .map_or_else(|| "unknown".to_string(), |ws| ws.name.clone());

    Ok(Some(WindowInfo {
        id: window.id,
        pid: window.pid,
        app_id: window.app_id,
        app_name: window.app_name,
        title: window.title,
        workspace: workspace_name,
        is_focused: true,
    }))
}

/// Switches to a workspace by name.
///
/// # Errors
///
/// Returns an error if the workspace is not found or the tiling manager is not available.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands require owned values
pub async fn focus_tiling_workspace(app: AppHandle, name: String) -> Result<(), StacheError> {
    use tiling::actor::StateMessage;

    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(StacheError::InvalidArguments(
            "Workspace name cannot be empty".to_string(),
        ));
    }

    let handle = tiling::init::get_handle()
        .ok_or_else(|| StacheError::TilingError("Tiling not initialized".to_string()))?;

    let _ = handle.send(StateMessage::SwitchWorkspace { name: name.clone() });

    // Emit workspace changed event
    let _ = app.emit(
        events::tiling::WORKSPACE_CHANGED,
        serde_json::json!({
            "workspace": name,
        }),
    );

    Ok(())
}

/// Focuses a window by its ID.
///
/// # Errors
///
/// Returns an error if the window is not found or the tiling manager is not available.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri commands require owned AppHandle
pub async fn focus_tiling_window(app: AppHandle, window_id: u32) -> Result<(), StacheError> {
    use tiling::actor::{QueryResult, StateQuery};
    use tiling::effects::window_ops;

    let handle = tiling::init::get_handle()
        .ok_or_else(|| StacheError::TilingError("Tiling not initialized".to_string()))?;

    // Get workspace name for the event
    let window_result = handle
        .query(StateQuery::GetWindow { id: window_id })
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    let workspace_name = if let QueryResult::Window(Some(w)) = window_result {
        // Get workspace name
        let workspaces_result = handle
            .query(StateQuery::GetAllWorkspaces)
            .await
            .map_err(|e| StacheError::TilingError(e.to_string()))?;

        match workspaces_result {
            QueryResult::Workspaces(ws) => ws
                .iter()
                .find(|ws| ws.id == w.workspace_id)
                .map_or_else(|| "unknown".to_string(), |ws| ws.name.clone()),
            _ => "unknown".to_string(),
        }
    } else {
        return Err(StacheError::TilingError(format!("Window {window_id} not found")));
    };

    // Focus the window via AX API (this will trigger an AXFocusedWindowChanged event
    // which will update the state automatically via the observer)
    if !window_ops::focus_window(window_id) {
        return Err(StacheError::TilingError(format!(
            "Failed to focus window {window_id}"
        )));
    }

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
#[must_use]
pub fn is_tiling_enabled() -> bool { tiling::commands::is_tiling_enabled() }

/// Gets windows for the currently focused workspace.
///
/// This is a convenience function that combines getting the focused workspace
/// and then getting windows for that workspace.
///
/// Only returns tiled (non-minimized) windows.
///
/// # Errors
///
/// Returns an error if the tiling manager is not available.
#[tauri::command]
pub async fn get_tiling_current_workspace_windows() -> Result<Vec<WindowInfo>, StacheError> {
    use tiling::actor::{QueryResult, StateQuery};

    let handle = tiling::init::get_handle()
        .ok_or_else(|| StacheError::TilingError("Tiling not initialized".to_string()))?;

    // Get focused workspace
    let focused_ws_result = handle
        .query(StateQuery::GetFocusedWorkspace)
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    let focused_workspace = match focused_ws_result {
        QueryResult::Workspace(Some(ws)) => ws,
        _ => return Ok(Vec::new()),
    };

    // Get focused window ID
    let focus_result = handle
        .query(StateQuery::GetFocusState)
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    let focused_window_id = match focus_result {
        QueryResult::Focus(f) => f.focused_window_id,
        _ => None,
    };

    // Get all windows
    let windows_result = handle
        .query(StateQuery::GetAllWindows)
        .await
        .map_err(|e| StacheError::TilingError(e.to_string()))?;

    let windows = match windows_result {
        QueryResult::Windows(w) => w,
        _ => return Ok(Vec::new()),
    };

    // Filter to focused workspace and non-minimized windows
    let infos: Vec<WindowInfo> = windows
        .into_iter()
        .filter(|w| w.workspace_id == focused_workspace.id && !w.is_minimized && !w.is_hidden)
        .map(|w| WindowInfo {
            id: w.id,
            pid: w.pid,
            app_id: w.app_id,
            app_name: w.app_name,
            title: w.title,
            workspace: focused_workspace.name.clone(),
            is_focused: focused_window_id == Some(w.id),
        })
        .collect();

    Ok(infos)
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
