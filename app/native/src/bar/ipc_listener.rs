//! IPC listener for CLI notifications.
//!
//! This module listens for distributed notifications from CLI commands
//! and translates them into Tauri events that the frontend can handle.

use tauri::{AppHandle, Emitter, Runtime};

use crate::events;
use crate::utils::ipc::{self, BarbaNotification};

/// Initializes the IPC listener for CLI notifications.
///
/// This sets up observers for distributed notifications from CLI commands
/// and translates them into Tauri events.
///
/// # Arguments
///
/// * `app_handle` - The Tauri app handle used to emit events and manage restart.
pub fn init<R: Runtime>(app_handle: AppHandle<R>) {
    // Register handler for Barba notifications
    ipc::register_notification_handler(move |notification| {
        handle_notification(&app_handle, notification);
    });

    // Start listening for notifications
    ipc::start_notification_listener();
}

/// Handles incoming Barba notifications.
fn handle_notification<R: Runtime>(app_handle: &AppHandle<R>, notification: BarbaNotification) {
    match notification {
        BarbaNotification::WindowFocusChanged => {
            // Emit event to all windows
            if let Err(err) = app_handle.emit(events::spaces::WINDOW_FOCUS_CHANGED, ()) {
                eprintln!("barba: failed to emit window-focus-changed event: {err}");
            }
        }

        BarbaNotification::WorkspaceChanged(workspace) => {
            // Emit event with workspace name
            if let Err(err) = app_handle.emit(events::spaces::WORKSPACE_CHANGED, &workspace) {
                eprintln!("barba: failed to emit workspace-changed event: {err}");
            }
        }

        BarbaNotification::Reload => {
            // Emit reload event to frontend so it can refresh/cleanup
            if let Err(err) = app_handle.emit(events::app::RELOAD, ()) {
                eprintln!("barba: failed to emit reload event: {err}");
            }

            // In debug mode, just log. In release mode, restart the app.
            #[cfg(debug_assertions)]
            {
                eprintln!("barba: reload requested via CLI. Restart the app to apply changes.");
            }

            #[cfg(not(debug_assertions))]
            {
                app_handle.restart();
            }
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
            "barba://spaces/window-focus-changed"
        );
        assert_eq!(
            events::spaces::WORKSPACE_CHANGED,
            "barba://spaces/workspace-changed"
        );
    }
}
