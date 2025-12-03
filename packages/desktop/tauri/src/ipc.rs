//! IPC server module for receiving commands from the CLI.
//!
//! This module provides a Unix socket server that listens for commands
//! from the standalone CLI application.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

/// Socket file name for IPC.
const SOCKET_NAME: &str = "barba.sock";

/// Event channel for CLI events.
const CLI_EVENT_CHANNEL: &str = "tauri_cli_event";

/// Gets the path to the IPC socket.
fn get_socket_path() -> PathBuf {
    // Use XDG_RUNTIME_DIR if available, otherwise fall back to home dir or /tmp
    std::env::var("XDG_RUNTIME_DIR").map_or_else(
        |_| {
            dirs::home_dir().map_or_else(
                || PathBuf::from("/tmp").join(SOCKET_NAME),
                |home| {
                    let run_dir = home.join(".local").join("run");
                    // Create the directory if it doesn't exist
                    std::fs::create_dir_all(&run_dir).ok();
                    run_dir.join(SOCKET_NAME)
                },
            )
        },
        |runtime_dir| PathBuf::from(runtime_dir).join(SOCKET_NAME),
    )
}

/// Payload received from CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct IpcPayload {
    name: String,
    data: Option<String>,
}

/// Payload for CLI events emitted to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct CliEventPayload {
    /// The name of the CLI command/event.
    pub name: String,
    /// Optional data associated with the command.
    pub data: Option<String>,
}

/// Starts the IPC server to listen for CLI commands.
pub fn start_ipc_server(app_handle: AppHandle) {
    let socket_path = get_socket_path();

    // Remove existing socket if it exists
    if socket_path.exists() {
        std::fs::remove_file(&socket_path).ok();
    }

    // Create the listener
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(err) => {
            eprintln!("barba: failed to create IPC socket: {err}");
            return;
        }
    };

    // Set non-blocking mode for graceful shutdown
    listener.set_nonblocking(true).ok();

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running;

    // Spawn the server thread
    std::thread::spawn(move || {
        run_server(&listener, &app_handle, &running_clone);

        // Cleanup socket on exit
        std::fs::remove_file(&socket_path).ok();
    });

    // Store the running flag for shutdown
    // (In a real implementation, you'd want to store this somewhere accessible)
}

/// Runs the IPC server loop.
fn run_server(listener: &UnixListener, app_handle: &AppHandle, running: &Arc<AtomicBool>) {
    while running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let app = app_handle.clone();
                std::thread::spawn(move || {
                    handle_client(stream, &app);
                });
            }
            Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                // No connection, sleep briefly
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(err) => {
                eprintln!("barba: IPC accept error: {err}");
            }
        }
    }
}

/// Handles a client connection.
fn handle_client(mut stream: UnixStream, app_handle: &AppHandle) {
    // Set blocking mode for this connection
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();

    // Read length-prefixed message
    let mut len_buf = [0u8; 4];
    if stream.read_exact(&mut len_buf).is_err() {
        return;
    }

    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 1024 * 1024 {
        // Sanity check: max 1MB
        return;
    }

    let mut msg_buf = vec![0u8; len];
    if stream.read_exact(&mut msg_buf).is_err() {
        return;
    }

    // Parse the payload
    let payload: IpcPayload = match serde_json::from_slice(&msg_buf) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("barba: failed to parse IPC message: {err}");
            stream.write_all(b"0").ok();
            return;
        }
    };

    // Handle special commands that don't need to be forwarded
    match payload.name.as_str() {
        "wallpaper-set" => {
            if let Some(data) = &payload.data {
                // Parse the JSON data
                #[derive(serde::Deserialize)]
                struct WallpaperSetData {
                    action: Option<String>,
                    file: Option<String>,
                }

                match serde_json::from_str::<WallpaperSetData>(data) {
                    Ok(set_data) => {
                        // Determine the action to perform
                        let wallpaper_action = if let Some(filename) = set_data.file {
                            Some(crate::wallpaper::WallpaperAction::File(filename))
                        } else if let Some(action_str) = set_data.action {
                            crate::wallpaper::parse_action(&action_str)
                        } else {
                            None
                        };

                        if let Some(ref action) = wallpaper_action
                            && let Err(err) = crate::wallpaper::perform_action(action)
                        {
                            eprintln!("barba: wallpaper error: {err}");
                        }
                    }
                    Err(err) => {
                        eprintln!("barba: failed to parse wallpaper data: {err}");
                    }
                }
            }
            stream.write_all(b"1").ok();
            return;
        }
        "wallpaper-generate-all" => {
            match crate::wallpaper::generate_all() {
                Ok(output) => {
                    // Send output with 'O' prefix to indicate output follows
                    let response = format!("O{output}");
                    stream.write_all(response.as_bytes()).ok();
                }
                Err(err) => {
                    eprintln!("barba: wallpaper generation error: {err}");
                    stream.write_all(b"0").ok();
                }
            }
            return;
        }
        "generate-schema" => {
            let schema = crate::config::generate_schema_json();
            println!("{schema}");
            stream.write_all(b"1").ok();
            return;
        }
        _ => {}
    }

    // Convert to CliEventPayload and emit
    let event_payload = CliEventPayload {
        name: payload.name,
        data: payload.data,
    };

    if let Err(err) = app_handle.emit(CLI_EVENT_CHANNEL, event_payload) {
        eprintln!("barba: failed to emit CLI event: {err}");
        stream.write_all(b"0").ok();
    } else {
        stream.write_all(b"1").ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_path_uses_runtime_dir() {
        // SAFETY: This test modifies environment variables which is unsafe in multi-threaded contexts.
        // This is safe here because tests run serially and we restore the original value.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/tmp/test-runtime-desktop");
        }
        let path = get_socket_path();
        assert_eq!(path, PathBuf::from("/tmp/test-runtime-desktop/barba.sock"));
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
    }

    #[test]
    fn test_socket_path_falls_back_to_home_dir() {
        // SAFETY: This test modifies environment variables which is unsafe in multi-threaded contexts.
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
        let path = get_socket_path();
        // Should contain barba.sock regardless of fallback path
        assert!(path.to_string_lossy().ends_with("barba.sock"));
    }

    #[test]
    fn test_socket_name_constant() {
        assert_eq!(SOCKET_NAME, "barba.sock");
    }

    #[test]
    fn test_cli_event_channel_constant() {
        assert_eq!(CLI_EVENT_CHANNEL, "tauri_cli_event");
    }

    #[test]
    fn test_ipc_payload_serialization() {
        let payload = IpcPayload {
            name: "test-command".to_string(),
            data: Some("test-data".to_string()),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("test-command"));
        assert!(json.contains("test-data"));
    }

    #[test]
    fn test_ipc_payload_deserialization() {
        let json = r#"{"name":"reload","data":null}"#;
        let payload: IpcPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.name, "reload");
        assert!(payload.data.is_none());
    }

    #[test]
    fn test_ipc_payload_with_data_deserialization() {
        let json = r#"{"name":"workspace-changed","data":"coding"}"#;
        let payload: IpcPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.name, "workspace-changed");
        assert_eq!(payload.data, Some("coding".to_string()));
    }

    #[test]
    fn test_cli_event_payload_serialization() {
        let payload = CliEventPayload {
            name: "focus-changed".to_string(),
            data: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("focus-changed"));
    }
}
