//! Unix socket server implementation.
//!
//! This module provides the low-level socket server functionality
//! for receiving and processing IPC commands from the CLI.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter};

use super::handlers;
use super::types::{CliEventPayload, IpcPayload};

/// Socket file name for IPC.
const SOCKET_NAME: &str = "barba.sock";

/// Event channel for CLI events.
/// Follows the pattern: `module:event-name`
const CLI_EVENT_CHANNEL: &str = "cli:command-received";

/// Gets the path to the IPC socket.
pub fn get_socket_path() -> PathBuf {
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

/// Starts the IPC server to listen for CLI commands.
pub fn start(app_handle: AppHandle) {
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

/// Writes a JSON response to the stream with length-prefix.
pub fn write_json_response(stream: &mut UnixStream, json: &str) {
    #[allow(clippy::cast_possible_truncation)]
    let len = json.len() as u32;
    if stream.write_all(&len.to_le_bytes()).is_err() {
        return;
    }
    stream.write_all(json.as_bytes()).ok();
    stream.flush().ok();
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

    eprintln!(
        "barba: IPC received: {} (data: {:?})",
        payload.name, payload.data
    );

    // Try to handle with specialized handlers
    if let Some(handled) = handlers::dispatch(&payload, &mut stream) {
        if !handled {
            stream.write_all(b"0").ok();
        }
        return;
    }

    // Convert to CliEventPayload and emit to frontend
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
        assert_eq!(CLI_EVENT_CHANNEL, "cli:command-received");
    }
}
