//! IPC listener for CLI notifications.
//!
//! This module translates incoming CLI messages into app actions:
//!
//! - **Control commands** (tiling operations, reload) arrive over the `0600`
//!   user-restricted IPC socket ([`crate::platform::ipc_socket`]).
//! - **Benign events** (`WindowFocusChanged`, `WorkspaceChanged`) arrive over
//!   `NSDistributedNotificationCenter` and only refresh frontend state.

use std::time::Duration;

use tauri::{AppHandle, Emitter, Runtime};
use uuid::Uuid;

use crate::events;
use crate::modules::tiling;
use crate::modules::tiling::actor::{QueryResult, StateActorHandle, StateMessage, StateQuery};
use crate::platform::ipc::{self, StacheNotification};
use crate::platform::ipc_socket::{IpcCommand, IpcResponse};

/// Initializes the IPC listener for CLI notifications.
///
/// This sets up observers for distributed notifications from external tooling
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

/// Dispatches a control command received over the IPC socket.
///
/// Returns an acknowledgement only after the actor has accepted the command.
pub fn handle_command<R: Runtime>(app_handle: &AppHandle<R>, command: IpcCommand) -> IpcResponse {
    if command == IpcCommand::Reload {
        handle_notification(app_handle, StacheNotification::Reload);
        return IpcResponse::success(serde_json::json!({ "queued": true }));
    }

    match execute_tiling_command(command) {
        Ok(()) => IpcResponse::success(serde_json::json!({ "queued": true })),
        Err(error) => IpcResponse::error(error),
    }
}

fn get_tiling_handle() -> Result<StateActorHandle, String> {
    if !tiling::init::is_initialized() {
        return Err("Tiling is not initialized".to_string());
    }

    tiling::init::get_handle().ok_or_else(|| "Tiling handle is unavailable".to_string())
}

fn query_tiling(handle: &StateActorHandle, query: StateQuery) -> Result<QueryResult, String> {
    let runtime =
        build_tiling_runtime().ok_or_else(|| "Failed to create tiling runtime".to_string())?;
    runtime
        .block_on(handle.query_timeout(query, Duration::from_secs(5)))
        .map_err(|error| error.to_string())
}

fn focused_workspace_id(handle: &StateActorHandle) -> Result<Uuid, String> {
    query_tiling(handle, StateQuery::GetFocusedWorkspace)?
        .into_workspace()
        .flatten()
        .map(|workspace| workspace.id)
        .ok_or_else(|| "No focused workspace".to_string())
}

fn execute_tiling_command(command: IpcCommand) -> Result<(), String> {
    match command {
        IpcCommand::TilingPause => return tiling::pause_runtime(),
        IpcCommand::TilingResume => {
            let app_handle = tiling::init::get_app_handle()
                .ok_or_else(|| "Tiling app handle is unavailable".to_string())?;
            return tiling::resume(app_handle);
        }
        _ => {}
    }
    let handle = get_tiling_handle()?;

    match command {
        IpcCommand::Reload => unreachable!("reload is handled before tiling dispatch"),
        IpcCommand::TilingPause | IpcCommand::TilingResume => unreachable!("handled above"),
        IpcCommand::TilingFocusWorkspace { workspace } => {
            handle.switch_workspace(&workspace).map_err(|error| error.to_string())
        }
        IpcCommand::TilingSetLayout { layout } => {
            let layout = serde_json::from_value(serde_json::json!(layout))
                .map_err(|error| format!("Invalid layout: {error}"))?;
            handle
                .set_layout(focused_workspace_id(&handle)?, layout)
                .map_err(|error| error.to_string())
        }
        IpcCommand::TilingWindowFocus { target } => {
            let direction = tiling::actor::FocusDirection::parse(&target)
                .ok_or_else(|| format!("Invalid focus direction: {target}"))?;
            handle.focus_window(direction).map_err(|error| error.to_string())
        }
        IpcCommand::TilingWindowSwap { direction } => {
            let direction = tiling::actor::FocusDirection::parse(&direction)
                .ok_or_else(|| format!("Invalid swap direction: {direction}"))?;
            handle.swap_window_in_direction(direction).map_err(|error| error.to_string())
        }
        IpcCommand::TilingWindowResize { dimension, amount } => handle
            .resize_focused_window(&dimension, amount)
            .map_err(|error| error.to_string()),
        IpcCommand::TilingWindowToggleFloating => {
            let window_id = query_tiling(&handle, StateQuery::GetFocusedWindow)?
                .into_window()
                .flatten()
                .map(|window| window.id)
                .ok_or_else(|| "No focused window".to_string())?;
            handle.toggle_floating(window_id).map_err(|error| error.to_string())
        }
        IpcCommand::TilingWindowPreset { preset } => {
            handle.apply_preset(&preset).map_err(|error| error.to_string())
        }
        IpcCommand::TilingWindowSendToWorkspace { workspace } => {
            let workspace_id =
                query_tiling(&handle, StateQuery::GetWorkspaceByName { name: workspace })?
                    .into_workspace()
                    .flatten()
                    .map(|workspace| workspace.id)
                    .ok_or_else(|| "Workspace not found".to_string())?;
            let window_id = query_tiling(&handle, StateQuery::GetFocusedWindow)?
                .into_window()
                .flatten()
                .map(|window| window.id)
                .ok_or_else(|| "No focused window".to_string())?;
            handle
                .send(StateMessage::MoveWindowToWorkspace { window_id, workspace_id })
                .map_err(|error| error.to_string())
        }
        IpcCommand::TilingWindowSendToScreen { screen } => {
            handle.send_window_to_screen(&screen).map_err(|error| error.to_string())
        }
        IpcCommand::TilingWorkspaceBalance => handle
            .balance_workspace(focused_workspace_id(&handle)?)
            .map_err(|error| error.to_string()),
        IpcCommand::TilingWorkspaceSendToScreen { screen } => {
            handle.send_workspace_to_screen(&screen).map_err(|error| error.to_string())
        }
    }
}

impl From<IpcCommand> for StacheNotification {
    fn from(command: IpcCommand) -> Self {
        match command {
            IpcCommand::Reload => Self::Reload,
            IpcCommand::TilingPause => Self::TilingPause,
            IpcCommand::TilingResume => Self::TilingResume,
            IpcCommand::TilingFocusWorkspace { workspace } => Self::TilingFocusWorkspace(workspace),
            IpcCommand::TilingSetLayout { layout } => Self::TilingSetLayout(layout),
            IpcCommand::TilingWindowFocus { target } => Self::TilingWindowFocus(target),
            IpcCommand::TilingWindowSwap { direction } => Self::TilingWindowSwap(direction),
            IpcCommand::TilingWindowResize { dimension, amount } => {
                Self::TilingWindowResize { dimension, amount }
            }
            IpcCommand::TilingWindowToggleFloating => Self::TilingWindowToggleFloating,
            IpcCommand::TilingWindowPreset { preset } => Self::TilingWindowPreset(preset),
            IpcCommand::TilingWindowSendToWorkspace { workspace } => {
                Self::TilingWindowSendToWorkspace(workspace)
            }
            IpcCommand::TilingWindowSendToScreen { screen } => {
                Self::TilingWindowSendToScreen(screen)
            }
            IpcCommand::TilingWorkspaceBalance => Self::TilingWorkspaceBalance,
            IpcCommand::TilingWorkspaceSendToScreen { screen } => {
                Self::TilingWorkspaceSendToScreen(screen)
            }
        }
    }
}

fn build_tiling_runtime() -> Option<tokio::runtime::Runtime> {
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => Some(runtime),
        Err(err) => {
            tracing::warn!(error = %err, "tiling: failed to create runtime");
            None
        }
    }
}

/// Handles incoming Stache notifications.
#[allow(clippy::too_many_lines)]
fn handle_notification<R: Runtime>(app_handle: &AppHandle<R>, notification: StacheNotification) {
    match notification {
        StacheNotification::WindowFocusChanged => {
            // Emit event to all windows
            if let Err(err) = app_handle.emit(events::spaces::WINDOW_FOCUS_CHANGED, ()) {
                tracing::warn!(error = %err, "failed to emit window-focus-changed event");
            }
        }

        StacheNotification::WorkspaceChanged(workspace) => {
            // Emit event with workspace name
            if let Err(err) = app_handle.emit(events::spaces::WORKSPACE_CHANGED, &workspace) {
                tracing::warn!(error = %err, "failed to emit workspace-changed event");
            }
        }

        StacheNotification::Reload => {
            // Emit reload event to frontend so it can refresh/cleanup
            if let Err(err) = app_handle.emit(events::app::RELOAD, ()) {
                tracing::warn!(error = %err, "failed to emit reload event");
            }

            // In debug mode, just log. In release mode, restart the app.
            #[cfg(debug_assertions)]
            {
                tracing::info!("reload requested via CLI - restart the app to apply changes");
            }

            #[cfg(not(debug_assertions))]
            {
                tracing::info!("reload requested via CLI, restarting application");
                crate::app_shutdown::restart(app_handle);
            }
        }

        StacheNotification::TilingPause => {
            if let Err(error) = tiling::pause_runtime() {
                tracing::warn!(%error, "tiling: failed to pause runtime");
            }
        }

        StacheNotification::TilingResume => {
            if let Some(handle) = tiling::init::get_app_handle() {
                if let Err(error) = tiling::resume(handle) {
                    tracing::warn!(%error, "tiling: failed to resume runtime");
                }
            } else {
                tracing::warn!("tiling: app handle is unavailable");
            }
        }

        // Tiling notifications - forwarded to the tiling manager
        StacheNotification::TilingFocusWorkspace(workspace) => {
            let app_handle = app_handle.clone();
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    tracing::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    if let Err(e) = handle.switch_workspace(&workspace) {
                        tracing::warn!("tiling: failed to switch workspace: {e}");
                    } else {
                        tracing::debug!("tiling: switched to workspace: {workspace}");

                        // Emit WORKSPACE_CHANGED event
                        if let Err(e) = app_handle.emit(
                            events::tiling::WORKSPACE_CHANGED,
                            serde_json::json!({
                                "workspace": workspace,
                            }),
                        ) {
                            tracing::warn!("tiling: failed to emit workspace-changed: {e}");
                        }
                    }
                } else {
                    tracing::warn!("tiling: handle not available");
                }
            });
        }

        StacheNotification::TilingSetLayout(layout) => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    tracing::warn!("tiling: manager not initialized");
                    return;
                }

                // Parse the layout string into LayoutType
                let layout_type: Result<tiling::state::LayoutType, _> =
                    serde_json::from_value(serde_json::json!(layout));

                match layout_type {
                    Ok(layout_type) => {
                        let Some(handle) = tiling::init::get_handle() else {
                            return;
                        };
                        // Get focused workspace ID
                        let Some(rt) = build_tiling_runtime() else {
                            return;
                        };
                        let Ok(result) = rt.block_on(handle.get_focused_workspace()) else {
                            return;
                        };
                        let Some(Some(ws)) = result.into_workspace() else {
                            return;
                        };
                        if let Err(e) = handle.set_layout(ws.id, layout_type) {
                            tracing::warn!("tiling: failed to set layout: {e}");
                        } else {
                            tracing::debug!(
                                "tiling: set layout to {layout_type:?} for workspace '{}'",
                                ws.name
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("tiling: invalid layout '{layout}': {e}");
                    }
                }
            });
        }

        StacheNotification::TilingWindowFocus(target) => {
            let app_handle = app_handle.clone();
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    tracing::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    // Parse direction
                    if let Some(direction) = tiling::actor::FocusDirection::parse(&target) {
                        if let Err(e) = handle.focus_window(direction) {
                            tracing::warn!("tiling: failed to focus window: {e}");
                        } else {
                            tracing::debug!("tiling: focused window {target}");

                            // Emit WINDOW_FOCUS_CHANGED event
                            let _ = app_handle.emit(
                                events::tiling::WINDOW_FOCUS_CHANGED,
                                serde_json::json!({ "direction": target }),
                            );
                        }
                    } else {
                        tracing::warn!("tiling: invalid focus direction: {target}");
                    }
                }
            });
        }

        StacheNotification::TilingWindowSwap(direction) => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    tracing::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    // Parse direction
                    if let Some(dir) = tiling::actor::FocusDirection::parse(&direction) {
                        if let Err(e) = handle.swap_window_in_direction(dir) {
                            tracing::warn!("tiling: failed to swap window: {e}");
                        } else {
                            tracing::debug!("tiling: swapped window {direction}");
                        }
                    } else {
                        tracing::warn!("tiling: invalid swap direction: {direction}");
                    }
                }
            });
        }

        StacheNotification::TilingWindowResize { dimension, amount } => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    tracing::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    if let Err(e) = handle.resize_focused_window(&dimension, amount) {
                        tracing::warn!("tiling: failed to resize window: {e}");
                    } else {
                        tracing::debug!("tiling: resized window {dimension} by {amount}px");
                    }
                }
            });
        }

        StacheNotification::TilingWindowToggleFloating => {
            std::thread::spawn(move || {
                let Some(handle) = tiling::init::get_handle() else {
                    tracing::warn!("tiling: handle not available");
                    return;
                };
                let Some(rt) = build_tiling_runtime() else {
                    return;
                };
                let window_id = rt
                    .block_on(handle.get_focused_window())
                    .ok()
                    .and_then(QueryResult::into_window)
                    .flatten()
                    .map(|window| window.id);
                if let Some(window_id) = window_id
                    && let Err(error) = handle.toggle_floating(window_id)
                {
                    tracing::warn!(%error, "tiling: failed to toggle floating window");
                }
            });
        }

        StacheNotification::TilingWindowPreset(preset) => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    tracing::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    if let Err(e) = handle.apply_preset(&preset) {
                        tracing::warn!("tiling: failed to apply preset: {e}");
                    } else {
                        tracing::debug!("tiling: applied preset '{preset}'");
                    }
                }
            });
        }

        StacheNotification::TilingWindowSendToWorkspace(workspace) => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    tracing::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    // Get the workspace ID by name, then get focused window ID
                    let Some(rt) = build_tiling_runtime() else {
                        return;
                    };

                    // Get workspace by name
                    let ws_result = rt.block_on(handle.get_workspace_by_name(&workspace));
                    let workspace_id = ws_result
                        .ok()
                        .and_then(tiling::actor::QueryResult::into_workspace)
                        .flatten()
                        .map(|ws| ws.id);

                    // Get focused window
                    let win_result = rt.block_on(handle.get_focused_window());
                    let window_id = win_result
                        .ok()
                        .and_then(tiling::actor::QueryResult::into_window)
                        .flatten()
                        .map(|w| w.id);

                    match (window_id, workspace_id) {
                        (Some(win_id), Some(ws_id)) => {
                            if let Err(e) =
                                handle.send(tiling::actor::StateMessage::MoveWindowToWorkspace {
                                    window_id: win_id,
                                    workspace_id: ws_id,
                                })
                            {
                                tracing::warn!("tiling: failed to send window to workspace: {e}");
                            } else {
                                tracing::debug!(
                                    "tiling: sent window {win_id} to workspace '{workspace}'"
                                );
                            }
                        }
                        (None, _) => {
                            tracing::warn!("tiling: no focused window");
                        }
                        (_, None) => {
                            tracing::warn!("tiling: workspace '{workspace}' not found");
                        }
                    }
                }
            });
        }

        StacheNotification::TilingWindowSendToScreen(screen) => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    tracing::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    if let Err(e) = handle.send_window_to_screen(&screen) {
                        tracing::warn!("tiling: failed to send window to screen: {e}");
                    } else {
                        tracing::debug!("tiling: sent window to screen {screen}");
                    }
                }
            });
        }

        StacheNotification::TilingWorkspaceBalance => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    tracing::warn!("tiling: manager not initialized");
                    return;
                }

                let Some(handle) = tiling::init::get_handle() else {
                    return;
                };
                // Get focused workspace ID
                let Some(rt) = build_tiling_runtime() else {
                    return;
                };
                let Ok(result) = rt.block_on(handle.get_focused_workspace()) else {
                    return;
                };
                let Some(Some(ws)) = result.into_workspace() else {
                    return;
                };
                if let Err(e) = handle.balance_workspace(ws.id) {
                    tracing::warn!("tiling: failed to balance workspace: {e}");
                } else {
                    tracing::debug!("tiling: balanced workspace '{}'", ws.name);
                }
            });
        }

        StacheNotification::TilingWorkspaceSendToScreen(screen) => {
            std::thread::spawn(move || {
                if !tiling::init::is_initialized() {
                    tracing::warn!("tiling: manager not initialized");
                    return;
                }

                if let Some(handle) = tiling::init::get_handle() {
                    if let Err(e) = handle.send_workspace_to_screen(&screen) {
                        tracing::warn!("tiling: failed to send workspace to screen: {e}");
                    } else {
                        tracing::debug!("tiling: sent workspace to screen {screen}");
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::events;
    use crate::platform::ipc::StacheNotification;
    use crate::platform::ipc_socket::IpcCommand;

    #[test]
    fn test_tiling_runtime_can_be_created() {
        assert!(super::build_tiling_runtime().is_some());
    }

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

    #[test]
    fn test_commands_map_to_notifications() {
        assert_eq!(
            StacheNotification::from(IpcCommand::Reload),
            StacheNotification::Reload
        );
        assert_eq!(
            StacheNotification::from(IpcCommand::TilingFocusWorkspace {
                workspace: "coding".to_string(),
            }),
            StacheNotification::TilingFocusWorkspace("coding".to_string())
        );
        assert_eq!(
            StacheNotification::from(IpcCommand::TilingWindowResize {
                dimension: "width".to_string(),
                amount: 40,
            }),
            StacheNotification::TilingWindowResize {
                dimension: "width".to_string(),
                amount: 40,
            }
        );
        assert_eq!(
            StacheNotification::from(IpcCommand::TilingWorkspaceBalance),
            StacheNotification::TilingWorkspaceBalance
        );
    }
}
