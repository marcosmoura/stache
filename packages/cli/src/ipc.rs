//! IPC module for communicating with the Barba desktop application.
//!
//! This module provides inter-process communication between the CLI and the
//! running desktop application. It uses a Unix socket for communication.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::commands::CliEventPayload;
use crate::error::CliError;

/// Socket file name for IPC.
const SOCKET_NAME: &str = "barba.sock";

/// Default timeout in milliseconds for quick commands.
const DEFAULT_TIMEOUT_MS: u64 = 1000;

/// Extended timeout in milliseconds for long-running operations.
const EXTENDED_TIMEOUT_MS: u64 = 120_000; // 2 minutes

/// Gets the path to the IPC socket.
fn get_socket_path() -> PathBuf {
    // Use XDG_RUNTIME_DIR if available, otherwise fall back to home dir or /tmp
    std::env::var("XDG_RUNTIME_DIR").map_or_else(
        |_| {
            dirs::home_dir().map_or_else(
                || PathBuf::from("/tmp").join(SOCKET_NAME),
                |home| home.join(".local").join("run").join(SOCKET_NAME),
            )
        },
        |runtime_dir| PathBuf::from(runtime_dir).join(SOCKET_NAME),
    )
}

/// Sends a payload to the desktop app via Unix socket with the default timeout.
pub fn send_to_desktop_app(payload: &CliEventPayload) -> Result<(), CliError> {
    send_to_desktop_app_with_timeout(payload, DEFAULT_TIMEOUT_MS)
}

/// Sends a payload to the desktop app via Unix socket with an extended timeout.
///
/// Use this for long-running operations like wallpaper generation.
pub fn send_to_desktop_app_extended(payload: &CliEventPayload) -> Result<(), CliError> {
    send_to_desktop_app_with_timeout(payload, EXTENDED_TIMEOUT_MS)
}

/// Sends a payload to the desktop app via Unix socket with a custom timeout.
fn send_to_desktop_app_with_timeout(
    payload: &CliEventPayload,
    timeout_ms: u64,
) -> Result<(), CliError> {
    let socket_path = get_socket_path();

    // Check if socket exists
    if !socket_path.exists() {
        return Err(CliError::DesktopAppNotRunning);
    }

    // Connect to the socket with timeout
    let stream = UnixStream::connect(&socket_path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::ConnectionRefused
            || err.kind() == std::io::ErrorKind::NotFound
        {
            CliError::DesktopAppNotRunning
        } else {
            CliError::ConnectionFailed(err.to_string())
        }
    })?;

    // Set timeouts
    stream.set_read_timeout(Some(Duration::from_millis(timeout_ms))).ok();
    stream.set_write_timeout(Some(Duration::from_millis(DEFAULT_TIMEOUT_MS))).ok();

    send_message(&stream, payload)?;

    Ok(())
}

/// Sends a JSON message over the stream.
fn send_message(mut stream: &UnixStream, payload: &CliEventPayload) -> Result<(), CliError> {
    let json = serde_json::to_string(payload)
        .map_err(|err| CliError::SendFailed(format!("Failed to serialize payload: {err}")))?;

    // Write length-prefixed message
    #[allow(clippy::cast_possible_truncation)] // Message length will never exceed u32::MAX
    let len = json.len() as u32;
    stream
        .write_all(&len.to_le_bytes())
        .map_err(|err| CliError::SendFailed(format!("Failed to write message length: {err}")))?;

    stream
        .write_all(json.as_bytes())
        .map_err(|err| CliError::SendFailed(format!("Failed to write message: {err}")))?;

    stream
        .flush()
        .map_err(|err| CliError::SendFailed(format!("Failed to flush stream: {err}")))?;

    // Read the first byte to determine response type
    let mut first_byte = [0u8; 1];
    match stream.read_exact(&mut first_byte) {
        Ok(()) => {
            if first_byte[0] == b'O' {
                // Streaming output - read and print incrementally
                read_streaming_output(stream);
                Ok(())
            } else if first_byte[0] == b'1' {
                Ok(())
            } else {
                Err(CliError::SendFailed("Command was not acknowledged".to_string()))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
            // Timeout is OK, command was likely processed
            Ok(())
        }
        Err(err) => Err(CliError::SendFailed(format!("Failed to read response: {err}"))),
    }
}

/// Reads streaming output from the socket and prints it incrementally.
fn read_streaming_output(stream: &UnixStream) {
    use std::io::{BufRead, BufReader};

    let reader = BufReader::new(stream);

    for line in reader.lines() {
        match line {
            Ok(content) => {
                println!("{content}");
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                // Timeout - end of stream
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                // Connection closed - end of stream
                break;
            }
            Err(_) => {
                // Other errors - just stop reading
                break;
            }
        }
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
            std::env::set_var("XDG_RUNTIME_DIR", "/tmp/test-runtime");
        }
        let path = get_socket_path();
        assert_eq!(path, PathBuf::from("/tmp/test-runtime/barba.sock"));
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
    fn test_default_timeout_constant() {
        assert_eq!(DEFAULT_TIMEOUT_MS, 1000);
    }

    #[test]
    fn test_extended_timeout_constant() {
        assert_eq!(EXTENDED_TIMEOUT_MS, 120_000);
    }

    #[test]
    fn test_send_to_desktop_app_returns_error_when_socket_missing() {
        // SAFETY: This test modifies environment variables which is unsafe in multi-threaded contexts.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/nonexistent/path/for/testing");
        }

        let payload = CliEventPayload {
            name: "test".to_string(),
            data: None,
        };

        let result = send_to_desktop_app(&payload);
        assert!(result.is_err());

        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
    }
}
