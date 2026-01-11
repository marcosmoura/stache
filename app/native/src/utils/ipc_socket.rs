//! Unix Domain Socket IPC for CLI<->App bidirectional communication.
//!
//! This module provides a socket-based IPC mechanism that allows the CLI
//! to query the running app's state and receive responses synchronously.
//!
//! # Architecture
//!
//! - The desktop app starts a Unix Domain Socket server on startup
//! - CLI commands connect to the socket, send a JSON query, and receive a JSON response
//! - If the socket doesn't exist or connection fails, the app is not running
//!
//! # Query Format
//!
//! Queries are JSON objects with a `type` field and optional parameters:
//!
//! ```json
//! {"type": "screens"}
//! {"type": "workspaces", "screen": "main"}
//! {"type": "windows", "workspace": "coding"}
//! ```
//!
//! # Response Format
//!
//! Responses are JSON with either `data` or `error`:
//!
//! ```json
//! {"data": [...]}
//! {"error": "Tiling not initialized"}
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use serde::{Deserialize, Serialize};

use crate::cache::get_cache_dir;

/// Socket filename within the cache directory.
const SOCKET_FILENAME: &str = "stache.sock";

/// Default timeout for socket operations in milliseconds.
const DEFAULT_TIMEOUT_MS: u64 = 5000;

/// Number of retry attempts for transient connection failures.
const MAX_RETRIES: u32 = 3;

/// Delay between retry attempts in milliseconds.
const RETRY_DELAY_MS: u64 = 100;

/// Whether the server is running.
static SERVER_RUNNING: AtomicBool = AtomicBool::new(false);

// ============================================================================
// Query Types
// ============================================================================

/// Query types that can be sent from CLI to App.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum IpcQuery {
    /// Query all screens.
    Screens,

    /// Query workspaces with optional filters.
    Workspaces {
        #[serde(skip_serializing_if = "Option::is_none")]
        screen: Option<String>,
        #[serde(default, rename = "focusedScreen")]
        focused_screen: bool,
    },

    /// Query windows with optional filters.
    Windows {
        #[serde(skip_serializing_if = "Option::is_none")]
        screen: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, rename = "focusedScreen")]
        focused_screen: bool,
        #[serde(default, rename = "focusedWorkspace")]
        focused_workspace: bool,
    },

    /// Ping to check if app is running.
    Ping,
}

/// Response from App to CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IpcResponse {
    /// Successful response with data.
    Success { data: serde_json::Value },
    /// Error response.
    Error { error: String },
}

impl IpcResponse {
    /// Creates a success response.
    pub fn success(data: impl Serialize) -> Self {
        Self::Success {
            data: serde_json::to_value(data).unwrap_or(serde_json::Value::Null),
        }
    }

    /// Creates an error response.
    pub fn error(message: impl Into<String>) -> Self { Self::Error { error: message.into() } }
}

// ============================================================================
// Socket Path
// ============================================================================

/// Gets the path to the IPC socket.
#[must_use]
pub fn get_socket_path() -> PathBuf { get_cache_dir().join(SOCKET_FILENAME) }

/// Checks if the socket file exists.
#[must_use]
#[allow(dead_code)]
pub fn socket_exists() -> bool { get_socket_path().exists() }

/// Removes the socket file if it exists.
fn remove_socket() {
    let path = get_socket_path();
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
}

// ============================================================================
// Server (App Side)
// ============================================================================

/// Starts the IPC socket server.
///
/// This should be called once during app initialization.
/// The server runs in a background thread and handles incoming queries.
///
/// # Arguments
///
/// * `handler` - A function that processes queries and returns responses.
pub fn start_server<F>(handler: F)
where F: Fn(IpcQuery) -> IpcResponse + Send + Sync + 'static {
    if SERVER_RUNNING.swap(true, Ordering::SeqCst) {
        eprintln!("stache: ipc: server already running");
        return;
    }

    // Remove any stale socket file
    remove_socket();

    let socket_path = get_socket_path();

    // Ensure parent directory exists
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Bind the socket
    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("stache: ipc: failed to bind socket: {e}");
            SERVER_RUNNING.store(false, Ordering::SeqCst);
            return;
        }
    };

    eprintln!("stache: ipc: server listening on {}", socket_path.display());

    // Spawn server thread
    let handler = Arc::new(handler);
    thread::Builder::new()
        .name("ipc-server".to_string())
        .spawn(move || {
            server_loop(listener, handler);
        })
        .expect("Failed to spawn IPC server thread");
}

/// Main server loop that accepts connections.
#[allow(clippy::needless_pass_by_value)] // Ownership needed - moved into thread
fn server_loop<F>(listener: UnixListener, handler: Arc<F>)
where F: Fn(IpcQuery) -> IpcResponse + Send + Sync + 'static {
    for stream in listener.incoming() {
        if !SERVER_RUNNING.load(Ordering::SeqCst) {
            break;
        }

        match stream {
            Ok(stream) => {
                let handler = handler.clone();
                // Handle each connection in a separate thread
                thread::spawn(move || {
                    handle_connection(stream, handler.as_ref());
                });
            }
            Err(e) => {
                eprintln!("stache: ipc: connection error: {e}");
            }
        }
    }
}

/// Handles a single client connection.
#[allow(clippy::needless_pass_by_value)] // Ownership needed - stream is consumed
fn handle_connection<F>(stream: UnixStream, handler: &F)
where F: Fn(IpcQuery) -> IpcResponse {
    // Set read timeout
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(DEFAULT_TIMEOUT_MS)));

    let mut reader = BufReader::new(stream.try_clone().expect("Failed to clone stream"));
    let mut line = String::new();

    // Read query line
    if reader.read_line(&mut line).is_err() {
        return;
    }

    // Parse query
    let response = match serde_json::from_str::<IpcQuery>(line.trim()) {
        Ok(query) => handler(query),
        Err(e) => IpcResponse::error(format!("Invalid query: {e}")),
    };

    // Send response
    let response_json = serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"error":"Failed to serialize response"}"#.to_string());

    // Get back the underlying stream from reader
    let mut stream = reader.into_inner();
    let _ = writeln!(stream, "{response_json}");
}

/// Stops the IPC server.
#[allow(dead_code)]
pub fn stop_server() {
    SERVER_RUNNING.store(false, Ordering::SeqCst);
    remove_socket();
}

/// Returns whether the server is running.
#[must_use]
#[allow(dead_code)]
pub fn is_server_running() -> bool { SERVER_RUNNING.load(Ordering::SeqCst) }

// ============================================================================
// Client (CLI Side)
// ============================================================================

/// Error type for IPC client operations.
#[derive(Debug)]
pub enum IpcError {
    /// App is not running (socket doesn't exist or can't connect).
    AppNotRunning,
    /// Connection timeout.
    Timeout,
    /// IO error.
    Io(std::io::Error),
    /// Invalid response from app.
    InvalidResponse(String),
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AppNotRunning => write!(f, "Stache app is not running"),
            Self::Timeout => write!(f, "Connection timed out"),
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::InvalidResponse(msg) => write!(f, "Invalid response: {msg}"),
        }
    }
}

impl std::error::Error for IpcError {}

/// Sends a query to the running app and returns the response.
///
/// Automatically retries on transient connection failures (up to 3 attempts).
///
/// # Arguments
///
/// * `query` - The query to send.
///
/// # Returns
///
/// The response from the app, or an error if the app is not running.
#[allow(clippy::needless_pass_by_value)] // Simpler API for callers
pub fn send_query(query: IpcQuery) -> Result<IpcResponse, IpcError> {
    let mut last_error = IpcError::AppNotRunning;

    for attempt in 0..MAX_RETRIES {
        match send_query_once(&query) {
            Ok(response) => return Ok(response),
            Err(e) => {
                last_error = e;

                // Only retry on transient errors (connection issues)
                // Don't retry on timeout or invalid response - those indicate real problems
                if !matches!(last_error, IpcError::AppNotRunning) {
                    break;
                }

                // Wait before retrying (except on last attempt)
                if attempt < MAX_RETRIES - 1 {
                    std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
                }
            }
        }
    }

    Err(last_error)
}

/// Sends a query once without retrying.
fn send_query_once(query: &IpcQuery) -> Result<IpcResponse, IpcError> {
    let socket_path = get_socket_path();

    // Check if socket exists
    if !socket_path.exists() {
        return Err(IpcError::AppNotRunning);
    }

    // Connect to socket
    let mut stream = UnixStream::connect(&socket_path).map_err(|e| {
        // Map common connection errors to AppNotRunning
        match e.kind() {
            std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::NotFound
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset => IpcError::AppNotRunning,
            _ => IpcError::Io(e),
        }
    })?;

    // Set timeouts
    let timeout = std::time::Duration::from_millis(DEFAULT_TIMEOUT_MS);
    stream.set_read_timeout(Some(timeout)).map_err(IpcError::Io)?;
    stream.set_write_timeout(Some(timeout)).map_err(IpcError::Io)?;

    // Send query
    let query_json = serde_json::to_string(query)
        .map_err(|e| IpcError::InvalidResponse(format!("Failed to serialize query: {e}")))?;

    writeln!(stream, "{query_json}").map_err(|e| {
        // Write errors often mean the connection dropped
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            IpcError::AppNotRunning
        } else {
            IpcError::Io(e)
        }
    })?;

    // Read response
    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader.read_line(&mut response_line).map_err(|e| match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => IpcError::Timeout,
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset => {
            IpcError::AppNotRunning
        }
        _ => IpcError::Io(e),
    })?;

    // Parse response
    serde_json::from_str(response_line.trim())
        .map_err(|e| IpcError::InvalidResponse(format!("Failed to parse response: {e}")))
}

/// Checks if the app is running by sending a ping query.
#[must_use]
#[allow(dead_code)]
pub fn is_app_running() -> bool {
    matches!(send_query(IpcQuery::Ping), Ok(IpcResponse::Success { .. }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_path() {
        let path = get_socket_path();
        assert!(path.to_string_lossy().contains("stache.sock"));
    }

    #[test]
    fn test_ipc_query_serialization() {
        let query = IpcQuery::Screens;
        let json = serde_json::to_string(&query).unwrap();
        assert_eq!(json, r#"{"type":"screens"}"#);

        let query = IpcQuery::Windows {
            screen: Some("main".to_string()),
            workspace: None,
            focused_screen: false,
            focused_workspace: true,
        };
        let json = serde_json::to_string(&query).unwrap();
        assert!(json.contains(r#""type":"windows""#));
        assert!(json.contains(r#""screen":"main""#));
        assert!(json.contains(r#""focusedWorkspace":true"#));
    }

    #[test]
    fn test_ipc_response_serialization() {
        let response = IpcResponse::success(vec![1, 2, 3]);
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(json, r#"{"data":[1,2,3]}"#);

        let response = IpcResponse::error("Not found");
        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(json, r#"{"error":"Not found"}"#);
    }

    #[test]
    fn test_app_not_running_when_no_socket() {
        // Remove socket if exists
        remove_socket();
        assert!(!is_app_running());
    }
}
