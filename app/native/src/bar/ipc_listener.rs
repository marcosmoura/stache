//! IPC listener for CLI notifications.
//!
//! This module listens for distributed notifications from CLI commands
//! and translates them into Tauri events that the frontend can handle.

use tauri::{AppHandle, Emitter, Runtime};

use crate::events;
use crate::tiling::{begin_animation, cancel_animation};
use crate::utils::ipc::{self, StacheNotification};

/// Initializes the IPC listener for CLI notifications.
///
/// This sets up observers for distributed notifications from CLI commands
/// and translates them into Tauri events.
///
/// # Arguments
///
/// * `app_handle` - The Tauri app handle used to emit events and manage restart.
pub fn init<R: Runtime>(app_handle: AppHandle<R>) {
    // Register handler for Stache notifications
    ipc::register_notification_handler(move |notification| {
        handle_notification(&app_handle, notification);
    });

    // Start listening for notifications
    ipc::start_notification_listener();
}

/// Handles incoming Stache notifications.
#[allow(clippy::too_many_lines)]
fn handle_notification<R: Runtime>(app_handle: &AppHandle<R>, notification: StacheNotification) {
    match notification {
        StacheNotification::WindowFocusChanged => {
            // Emit event to all windows
            if let Err(err) = app_handle.emit(events::spaces::WINDOW_FOCUS_CHANGED, ()) {
                eprintln!("stache: failed to emit window-focus-changed event: {err}");
            }
        }

        StacheNotification::WorkspaceChanged(workspace) => {
            // Emit event with workspace name
            if let Err(err) = app_handle.emit(events::spaces::WORKSPACE_CHANGED, &workspace) {
                eprintln!("stache: failed to emit workspace-changed event: {err}");
            }
        }

        StacheNotification::Reload => {
            // Emit reload event to frontend so it can refresh/cleanup
            if let Err(err) = app_handle.emit(events::app::RELOAD, ()) {
                eprintln!("stache: failed to emit reload event: {err}");
            }

            // In debug mode, just log. In release mode, restart the app.
            #[cfg(debug_assertions)]
            {
                eprintln!("stache: reload requested via CLI. Restart the app to apply changes.");
            }

            #[cfg(not(debug_assertions))]
            {
                app_handle.restart();
            }
        }

        // Tiling notifications - forwarded to the tiling manager
        StacheNotification::TilingFocusWorkspace(workspace) => {
            let app_handle = app_handle.clone();
            std::thread::spawn(move || {
                if let Some(manager) = crate::tiling::get_manager() {
                    cancel_animation();
                    let mut mgr = manager.write();
                    begin_animation();
                    if let Some(info) = mgr.switch_workspace(&workspace) {
                        eprintln!("stache: tiling: switched to workspace: {}", info.workspace);
                        let workspace_name = info.workspace.clone();
                        drop(mgr); // Release lock before emitting

                        // Emit WORKSPACE_CHANGED event
                        if let Err(e) = app_handle.emit(
                            events::tiling::WORKSPACE_CHANGED,
                            serde_json::json!({
                                "workspace": info.workspace,
                                "screen": info.screen,
                                "previousWorkspace": info.previous_workspace,
                            }),
                        ) {
                            eprintln!("stache: tiling: failed to emit workspace-changed: {e}");
                        }

                        // Emit WINDOW_FOCUS_CHANGED event (UI will refetch focused window)
                        let _ = app_handle.emit(
                            events::tiling::WINDOW_FOCUS_CHANGED,
                            serde_json::json!({ "workspace": workspace_name }),
                        );
                    } else {
                        eprintln!("stache: tiling: workspace not found: {workspace}");
                    }
                } else {
                    eprintln!("stache: tiling: manager not initialized");
                }
            });
        }

        StacheNotification::TilingSetLayout(layout) => {
            std::thread::spawn(move || {
                if let Some(manager) = crate::tiling::get_manager() {
                    // Parse the layout string into LayoutType
                    let layout_type: Result<crate::config::LayoutType, _> =
                        serde_json::from_value(serde_json::json!(layout));

                    match layout_type {
                        Ok(layout_type) => {
                            cancel_animation();
                            let mut mgr = manager.write();
                            begin_animation();

                            // Get the focused workspace name
                            let workspace_name =
                                mgr.get_focused_workspace().map(|ws| ws.name.clone());

                            if let Some(ws_name) = workspace_name {
                                if mgr.set_workspace_layout(&ws_name, layout_type) {
                                    drop(mgr);
                                    eprintln!(
                                        "stache: tiling: set layout to {layout_type:?} for workspace '{ws_name}'"
                                    );
                                } else {
                                    eprintln!(
                                        "stache: tiling: failed to set layout for workspace '{ws_name}'"
                                    );
                                }
                            } else {
                                eprintln!("stache: tiling: no focused workspace to set layout");
                            }
                        }
                        Err(e) => {
                            eprintln!("stache: tiling: invalid layout '{layout}': {e}");
                        }
                    }
                } else {
                    eprintln!("stache: tiling: manager not initialized");
                }
            });
        }

        StacheNotification::TilingWindowFocus(target) => {
            let app_handle = app_handle.clone();
            std::thread::spawn(move || {
                if let Some(manager) = crate::tiling::get_manager() {
                    cancel_animation();
                    let mut mgr = manager.write();
                    begin_animation();
                    if let Some(window_id) = mgr.focus_window_in_direction(&target) {
                        eprintln!("stache: tiling: focused window {window_id}");

                        // Get workspace name for the event
                        let workspace_name =
                            mgr.state().focused_workspace.clone().unwrap_or_default();

                        drop(mgr); // Release lock before emitting

                        // Emit WINDOW_FOCUS_CHANGED event
                        if let Err(e) = app_handle.emit(
                            events::tiling::WINDOW_FOCUS_CHANGED,
                            serde_json::json!({
                                "windowId": window_id,
                                "workspace": workspace_name,
                            }),
                        ) {
                            eprintln!("stache: tiling: failed to emit window-focus-changed: {e}");
                        }
                    } else {
                        eprintln!("stache: tiling: failed to focus window: {target}");
                    }
                } else {
                    eprintln!("stache: tiling: manager not initialized");
                }
            });
        }

        StacheNotification::TilingWindowSwap(direction) => {
            // Spawn thread so cancel_animation() can be called while previous animation runs
            std::thread::spawn(move || {
                if let Some(manager) = crate::tiling::get_manager() {
                    cancel_animation();
                    let mut mgr = manager.write();
                    begin_animation();
                    if mgr.swap_window_in_direction(&direction) {
                        eprintln!("stache: tiling: swapped window {direction}");
                    } else {
                        eprintln!("stache: tiling: failed to swap window: {direction}");
                    }
                } else {
                    eprintln!("stache: tiling: manager not initialized");
                }
            });
        }

        StacheNotification::TilingWindowResize { dimension, amount } => {
            std::thread::spawn(move || {
                if let Some(manager) = crate::tiling::get_manager() {
                    cancel_animation();
                    let mut mgr = manager.write();
                    begin_animation();
                    if mgr.resize_focused_window(&dimension, amount) {
                        eprintln!("stache: tiling: resized window {dimension} by {amount}px");
                    } else {
                        eprintln!("stache: tiling: failed to resize window");
                    }
                } else {
                    eprintln!("stache: tiling: manager not initialized");
                }
            });
        }

        StacheNotification::TilingWindowPreset(preset) => {
            std::thread::spawn(move || {
                if let Some(manager) = crate::tiling::get_manager() {
                    cancel_animation();
                    let mut mgr = manager.write();
                    begin_animation();
                    if mgr.apply_preset(&preset) {
                        eprintln!("stache: tiling: applied preset '{preset}'");
                    } else {
                        eprintln!("stache: tiling: failed to apply preset: {preset}");
                    }
                } else {
                    eprintln!("stache: tiling: manager not initialized");
                }
            });
        }

        StacheNotification::TilingWindowSendToWorkspace(workspace) => {
            std::thread::spawn(move || {
                if let Some(manager) = crate::tiling::get_manager() {
                    cancel_animation();
                    let mut mgr = manager.write();
                    begin_animation();
                    if mgr.send_window_to_workspace(&workspace) {
                        eprintln!("stache: tiling: sent window to workspace {workspace}");
                    } else {
                        eprintln!(
                            "stache: tiling: failed to send window to workspace: {workspace}"
                        );
                    }
                } else {
                    eprintln!("stache: tiling: manager not initialized");
                }
            });
        }

        StacheNotification::TilingWindowSendToScreen(screen) => {
            std::thread::spawn(move || {
                if let Some(manager) = crate::tiling::get_manager() {
                    cancel_animation();
                    let mut mgr = manager.write();
                    begin_animation();
                    if mgr.send_window_to_screen(&screen) {
                        eprintln!("stache: tiling: sent window to screen {screen}");
                    } else {
                        eprintln!("stache: tiling: failed to send window to screen: {screen}");
                    }
                } else {
                    eprintln!("stache: tiling: manager not initialized");
                }
            });
        }

        StacheNotification::TilingWorkspaceBalance => {
            std::thread::spawn(move || {
                if let Some(manager) = crate::tiling::get_manager() {
                    cancel_animation();
                    let mut mgr = manager.write();
                    begin_animation();

                    // Get the focused workspace name
                    let workspace_name = mgr.get_focused_workspace().map(|ws| ws.name.clone());

                    if let Some(ws_name) = workspace_name {
                        // Balance the workspace (clears ratios and re-applies layout)
                        let repositioned = mgr.balance_workspace(&ws_name);
                        drop(mgr);
                        eprintln!(
                            "stache: tiling: balanced {repositioned} windows in workspace '{ws_name}'"
                        );
                    } else {
                        eprintln!("stache: tiling: no focused workspace to balance");
                    }
                } else {
                    eprintln!("stache: tiling: manager not initialized");
                }
            });
        }

        StacheNotification::TilingWorkspaceSendToScreen(screen) => {
            std::thread::spawn(move || {
                if let Some(manager) = crate::tiling::get_manager() {
                    cancel_animation();
                    let mut mgr = manager.write();
                    begin_animation();
                    if mgr.send_workspace_to_screen(&screen) {
                        eprintln!("stache: tiling: sent workspace to screen: {screen}");
                    } else {
                        eprintln!("stache: tiling: failed to send workspace to screen: {screen}");
                    }
                } else {
                    eprintln!("stache: tiling: manager not initialized");
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::events;

    #[test]
    fn test_event_names() {
        assert_eq!(
            events::spaces::WINDOW_FOCUS_CHANGED,
            "stache://spaces/window-focus-changed"
        );
        assert_eq!(
            events::spaces::WORKSPACE_CHANGED,
            "stache://spaces/workspace-changed"
        );
    }
}
