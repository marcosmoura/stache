//! IPC listener for CLI notifications.
//!
//! This module listens for distributed notifications from CLI commands
//! and translates them into Tauri events that the frontend can handle.

use tauri::{AppHandle, Emitter, Runtime};

use crate::events;
use crate::modules::tiling;
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
                if !tiling::init::is_initialized() {
                    log::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    if let Err(e) = handle.switch_workspace(&workspace) {
                        log::warn!("tiling: failed to switch workspace: {e}");
                    } else {
                        log::debug!("tiling: switched to workspace: {workspace}");

                        // Emit WORKSPACE_CHANGED event
                        if let Err(e) = app_handle.emit(
                            events::tiling::WORKSPACE_CHANGED,
                            serde_json::json!({
                                "workspace": workspace,
                            }),
                        ) {
                            log::warn!("tiling: failed to emit workspace-changed: {e}");
                        }
                    }
                } else {
                    log::warn!("tiling: handle not available");
                }
            });
        }

        StacheNotification::TilingSetLayout(layout) => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    log::warn!("tiling: manager not initialized");
                    return;
                }

                // Parse the layout string into LayoutType
                let layout_type: Result<tiling::state::LayoutType, _> =
                    serde_json::from_value(serde_json::json!(layout));

                match layout_type {
                    Ok(layout_type) => {
                        if let Some(handle) = tiling::init::get_handle() {
                            // Get focused workspace ID
                            let rt = tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                                .unwrap();
                            if let Ok(result) = rt.block_on(handle.get_focused_workspace()) {
                                if let Some(Some(ws)) = result.into_workspace() {
                                    if let Err(e) = handle.set_layout(ws.id, layout_type) {
                                        log::warn!("tiling: failed to set layout: {e}");
                                    } else {
                                        log::debug!(
                                            "tiling: set layout to {layout_type:?} for workspace '{}'",
                                            ws.name
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("tiling: invalid layout '{layout}': {e}");
                    }
                }
            });
        }

        StacheNotification::TilingWindowFocus(target) => {
            let app_handle = app_handle.clone();
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    log::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    // Parse direction
                    if let Some(direction) = tiling::actor::FocusDirection::from_str(&target) {
                        if let Err(e) = handle.focus_window(direction) {
                            log::warn!("tiling: failed to focus window: {e}");
                        } else {
                            log::debug!("tiling: focused window {target}");

                            // Emit WINDOW_FOCUS_CHANGED event
                            let _ = app_handle.emit(
                                events::tiling::WINDOW_FOCUS_CHANGED,
                                serde_json::json!({ "direction": target }),
                            );
                        }
                    } else {
                        log::warn!("tiling: invalid focus direction: {target}");
                    }
                }
            });
        }

        StacheNotification::TilingWindowSwap(direction) => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    log::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    // Parse direction
                    if let Some(dir) = tiling::actor::FocusDirection::from_str(&direction) {
                        if let Err(e) = handle.swap_window_in_direction(dir) {
                            log::warn!("tiling: failed to swap window: {e}");
                        } else {
                            log::debug!("tiling: swapped window {direction}");
                        }
                    } else {
                        log::warn!("tiling: invalid swap direction: {direction}");
                    }
                }
            });
        }

        StacheNotification::TilingWindowResize { dimension, amount } => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    log::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    if let Err(e) = handle.resize_focused_window(&dimension, amount) {
                        log::warn!("tiling: failed to resize window: {e}");
                    } else {
                        log::debug!("tiling: resized window {dimension} by {amount}px");
                    }
                }
            });
        }

        StacheNotification::TilingWindowPreset(preset) => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    log::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    if let Err(e) = handle.apply_preset(&preset) {
                        log::warn!("tiling: failed to apply preset: {e}");
                    } else {
                        log::debug!("tiling: applied preset '{preset}'");
                    }
                }
            });
        }

        StacheNotification::TilingWindowSendToWorkspace(workspace) => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    log::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    // Get the workspace ID by name, then get focused window ID
                    let rt =
                        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();

                    // Get workspace by name
                    let ws_result = rt.block_on(handle.get_workspace_by_name(&workspace));
                    let workspace_id =
                        ws_result.ok().and_then(|r| r.into_workspace()).flatten().map(|ws| ws.id);

                    // Get focused window
                    let win_result = rt.block_on(handle.get_focused_window());
                    let window_id =
                        win_result.ok().and_then(|r| r.into_window()).flatten().map(|w| w.id);

                    match (window_id, workspace_id) {
                        (Some(wid), Some(wsid)) => {
                            if let Err(e) =
                                handle.send(tiling::actor::StateMessage::MoveWindowToWorkspace {
                                    window_id: wid,
                                    workspace_id: wsid,
                                })
                            {
                                log::warn!("tiling: failed to send window to workspace: {e}");
                            } else {
                                log::debug!("tiling: sent window {wid} to workspace '{workspace}'");
                            }
                        }
                        (None, _) => {
                            log::warn!("tiling: no focused window");
                        }
                        (_, None) => {
                            log::warn!("tiling: workspace '{workspace}' not found");
                        }
                    }
                }
            });
        }

        StacheNotification::TilingWindowSendToScreen(screen) => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    log::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    if let Err(e) = handle.send_window_to_screen(&screen) {
                        log::warn!("tiling: failed to send window to screen: {e}");
                    } else {
                        log::debug!("tiling: sent window to screen {screen}");
                    }
                }
            });
        }

        StacheNotification::TilingWorkspaceBalance => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    log::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    // Get focused workspace ID
                    let rt =
                        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                    if let Ok(result) = rt.block_on(handle.get_focused_workspace()) {
                        if let Some(Some(ws)) = result.into_workspace() {
                            if let Err(e) = handle.balance_workspace(ws.id) {
                                log::warn!("tiling: failed to balance workspace: {e}");
                            } else {
                                log::debug!("tiling: balanced workspace '{}'", ws.name);
                            }
                        }
                    }
                }
            });
        }

        StacheNotification::TilingWorkspaceSendToScreen(screen) => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    log::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    if let Err(e) = handle.send_workspace_to_screen(&screen) {
                        log::warn!("tiling: failed to send workspace to screen: {e}");
                    } else {
                        log::debug!("tiling: sent workspace to screen {screen}");
                    }
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
