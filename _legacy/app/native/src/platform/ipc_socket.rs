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
//!
//! # Control Commands
//!
//! The same socket also carries CLI control commands (workspace focus, layout,
//! window operations, reload). Commands are JSON objects with a `type` field
//! that does not collide with the query names above:
//!
//! ```json
//! {"type": "tilingFocusWorkspace", "workspace": "coding"}
//! ```
//!
//! The socket is restricted to the current user via `0600` permissions, and the
//! server bounds concurrent connections, applies I/O timeouts, and caps the
//! maximum request size so a hostile same-UID process cannot exhaust resources.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

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

/// Maximum number of concurrently served connections.
///
/// Threads are bounded: once this many connections are being handled, further
/// connections are queued in the kernel accept backlog until a slot frees.
const MAX_CONCURRENT_CONNECTIONS: usize = 8;

/// Maximum request line size in bytes.
///
/// Queries and commands are tiny JSON objects; 64 KiB is far beyond any
/// legitimate request and prevents a peer from forcing unbounded buffering.
const MAX_REQUEST_SIZE: usize = 64 * 1024;

/// Whether the server is running.
static SERVER_RUNNING: AtomicBool = AtomicBool::new(false);

// ============================================================================
// Query Types
// ============================================================================

/// Query types that can be sent from CLI to App.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum IpcQuery {
    /// Query all screens (v1).
    Screens,

    /// Query workspaces with optional filters (v1).
    Workspaces {
        #[serde(skip_serializing_if = "Option::is_none")]
        screen: Option<String>,
        #[serde(default, rename = "focusedScreen")]
        focused_screen: bool,
    },

    /// Query windows with optional filters (v1).
    Windows {
        #[serde(skip_serializing_if = "Option::is_none")]
        screen: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(default, rename = "focusedScreen")]
        focused_screen: bool,
        #[serde(default, rename = "focusedWorkspace")]
        focused_workspace: bool,
        /// Include detailed information about each window.
        #[serde(default)]
        detailed: bool,
    },

    /// Query all running applications (excluding ignored apps).
    Apps,

    /// Ping to check if app is running.
    Ping,

    // ========================================================================
    // Tiling v2 queries
    // ========================================================================
    /// Query overall tiling v2 state.
    #[serde(rename = "v2State")]
    V2State,

    /// Query all screens (v2).
    #[serde(rename = "v2Screens")]
    V2Screens,

    /// Query all workspaces (v2).
    #[serde(rename = "v2Workspaces")]
    V2Workspaces,

    /// Query windows (v2) with optional workspace filter.
    #[serde(rename = "v2Windows")]
    V2Windows {
        #[serde(skip_serializing_if = "Option::is_none", rename = "workspaceId")]
        workspace_id: Option<String>,
    },

    /// Check if tiling v2 is enabled.
    #[serde(rename = "v2Enabled")]
    V2Enabled,
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

/// Control commands sent from the CLI to the App over the socket.
///
/// These were historically posted over `NSDistributedNotificationCenter`,
/// which any process in the user session can observe or spoof. They now travel
/// exclusively over the `0600` user-restricted socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum IpcCommand {
    /// Reload/restart the desktop app.
    Reload,
    /// Pause the tiling runtime.
    TilingPause,
    /// Resume the tiling runtime.
    TilingResume,
    /// Focus a workspace by name.
    TilingFocusWorkspace { workspace: String },
    /// Change layout of the focused workspace.
    TilingSetLayout { layout: String },
    /// Focus a window by direction or ID.
    TilingWindowFocus { target: String },
    /// Swap focused window with a neighbor.
    TilingWindowSwap { direction: String },
    /// Resize the focused window.
    TilingWindowResize { dimension: String, amount: i32 },
    /// Toggle whether the focused window is floating.
    TilingWindowToggleFloating,
    /// Apply a floating preset to the focused window.
    TilingWindowPreset { preset: String },
    /// Send the focused window to a workspace.
    TilingWindowSendToWorkspace { workspace: String },
    /// Send the focused window to a screen.
    TilingWindowSendToScreen { screen: String },
    /// Balance the focused workspace.
    TilingWorkspaceBalance,
    /// Send the focused workspace to a screen.
    TilingWorkspaceSendToScreen { screen: String },
}

/// A message on the IPC socket: either a read-only query or a control command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IpcMessage {
    /// A read-only state query.
    Query(IpcQuery),
    /// A control command.
    Command(IpcCommand),
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
/// The server runs in a background thread and handles incoming messages.
///
/// # Arguments
///
/// * `handler` - A function that processes queries and commands and returns responses.
pub fn init<F>(handler: F)
where F: Fn(IpcMessage) -> IpcResponse + Send + Sync + 'static {
    if SERVER_RUNNING.swap(true, Ordering::SeqCst) {
        tracing::debug!("ipc server already running");
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
            tracing::error!(error = %e, path = %socket_path.display(), "failed to bind ipc socket");
            SERVER_RUNNING.store(false, Ordering::SeqCst);
            return;
        }
    };

    // Restrict socket permissions to owner-only (0o600) to prevent other
    // local users from querying or injecting commands into the tiling WM.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) =
            std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
        {
            tracing::warn!(error = %e, "failed to set socket permissions to 0600");
        }
    }

    tracing::info!(path = %socket_path.display(), "ipc server listening");

    // Spawn server thread
    let handler = Arc::new(handler);
    thread::Builder::new()
        .name("ipc-server".to_string())
        .spawn(move || {
            server_loop(listener, &SERVER_RUNNING, handler);
        })
        .expect("Failed to spawn IPC server thread");
}

/// Bounded semaphore that caps concurrent IPC connections.
///
/// Implemented with std primitives (`std::sync::Semaphore` is unavailable on
/// the pinned toolchain). `acquire` blocks the accept loop so excess
/// connections queue in the kernel backlog instead of spawning threads.
struct ConnectionLimiter {
    available: Mutex<usize>,
    condvar: Condvar,
}

impl ConnectionLimiter {
    const fn new(limit: usize) -> Self {
        Self {
            available: Mutex::new(limit),
            condvar: Condvar::new(),
        }
    }

    /// Blocks until a connection slot is available.
    fn acquire(&self) {
        let mut available = self.available.lock().unwrap_or_else(PoisonError::into_inner);
        while *available == 0 {
            available = self.condvar.wait(available).unwrap_or_else(PoisonError::into_inner);
        }
        *available -= 1;
    }

    /// Releases a connection slot.
    fn release(&self) {
        let mut available = self.available.lock().unwrap_or_else(PoisonError::into_inner);
        *available += 1;
        self.condvar.notify_one();
        drop(available);
    }
}

/// Main server loop that accepts connections.
///
/// Concurrency is bounded by [`ConnectionLimiter`]: at most
/// [`MAX_CONCURRENT_CONNECTIONS`] connections are handled at once. Excess
/// connections queue in the kernel accept backlog, which prevents an
/// unbounded thread-per-connection resource exhaustion.
#[allow(clippy::needless_pass_by_value)] // Ownership needed - moved into thread
fn server_loop<F>(listener: UnixListener, running: &'static AtomicBool, handler: Arc<F>)
where F: Fn(IpcMessage) -> IpcResponse + Send + Sync + 'static {
    let limiter = Arc::new(ConnectionLimiter::new(MAX_CONCURRENT_CONNECTIONS));

    for stream in listener.incoming() {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                tracing::warn!(error = %e, "ipc connection error");
                continue;
            }
        };

        // Blocking acquire provides backpressure: never more than
        // MAX_CONCURRENT_CONNECTIONS handler threads.
        limiter.acquire();

        let handler = handler.clone();
        let limiter = limiter.clone();
        thread::Builder::new()
            .name("ipc-conn".to_string())
            .spawn(move || {
                handle_connection(stream, handler.as_ref());
                limiter.release();
            })
            .expect("Failed to spawn IPC connection thread");
    }
}

/// Handles a single client connection.
///
/// Applies a read/write timeout and caps the request size so a misbehaving
/// peer cannot hold a connection open indefinitely or force unbounded reads.
#[allow(clippy::needless_pass_by_value)] // Ownership needed - stream is consumed
fn handle_connection<F>(stream: UnixStream, handler: &F)
where F: Fn(IpcMessage) -> IpcResponse {
    let timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let mut reader = BufReader::new(stream.try_clone().expect("Failed to clone stream"));
    let deadline = Instant::now() + timeout;
    let mut line = Vec::with_capacity(256);

    // Reapply the remaining time before each byte. A peer that periodically
    // sends one byte must still finish its request within the total deadline.
    let read_result = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request deadline",
            ));
        }
        let _ = reader.get_ref().set_read_timeout(Some(remaining));

        let mut byte = [0_u8; 1];
        match reader.read(&mut byte) {
            Ok(0) => break Ok(()),
            Ok(_) if byte[0] == b'\n' => break Ok(()),
            Ok(_) => {
                line.push(byte[0]);
                if line.len() > MAX_REQUEST_SIZE {
                    break Ok(());
                }
            }
            Err(err) => break Err(err),
        }
    };

    let response = match read_result {
        Err(_) => return,
        Ok(()) if line.is_empty() => IpcResponse::error("Empty request"),
        Ok(()) if line.len() > MAX_REQUEST_SIZE => IpcResponse::error("Request too large"),
        Ok(()) => match serde_json::from_slice::<IpcMessage>(&line) {
            Ok(message) => handler(message),
            Err(e) => IpcResponse::error(format!("Invalid request: {e}")),
        },
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
    send_message(IpcMessage::Query(query))
}

/// Sends a control command to the running app.
///
/// # Arguments
///
/// * `command` - The command to send.
///
/// # Returns
///
/// The ack/error response from the app, or an error if the app is not running.
#[allow(clippy::needless_pass_by_value)] // Simpler API for callers
pub fn send_command(command: IpcCommand) -> Result<IpcResponse, IpcError> {
    send_message(IpcMessage::Command(command))
}

/// Sends a message to the running app and returns the response.
///
/// Automatically retries on transient connection failures (up to 3 attempts).
#[allow(clippy::needless_pass_by_value)] // Simpler API for callers
fn send_message(message: IpcMessage) -> Result<IpcResponse, IpcError> {
    let mut last_error = IpcError::AppNotRunning;

    for attempt in 0..MAX_RETRIES {
        match send_message_once(&message) {
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
                    std::thread::sleep(Duration::from_millis(RETRY_DELAY_MS));
                }
            }
        }
    }

    Err(last_error)
}

/// Sends a message once without retrying.
fn send_message_once(message: &IpcMessage) -> Result<IpcResponse, IpcError> {
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
    let timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);
    stream.set_read_timeout(Some(timeout)).map_err(IpcError::Io)?;
    stream.set_write_timeout(Some(timeout)).map_err(IpcError::Io)?;

    // Send message
    let message_json = serde_json::to_string(message)
        .map_err(|e| IpcError::InvalidResponse(format!("Failed to serialize message: {e}")))?;

    writeln!(stream, "{message_json}").map_err(|e| {
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
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// Test server handle that keeps the socket's temp dir alive.
    struct TestServer {
        path: PathBuf,
        running: &'static AtomicBool,
        handle: thread::JoinHandle<()>,
        _dir: tempfile::TempDir,
    }

    /// Spawns a test server on a throwaway socket path.
    fn spawn_test_server<F>(handler: F) -> TestServer
    where F: Fn(IpcMessage) -> IpcResponse + Send + Sync + 'static {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let running: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(true)));
        let handler = Arc::new(handler);
        let handle = thread::spawn(move || server_loop(listener, running, handler));
        TestServer {
            path: socket_path,
            running,
            handle,
            _dir: dir,
        }
    }

    /// Stops a test server and joins its accept-loop thread.
    fn stop_test_server(server: TestServer) {
        server.running.store(false, Ordering::SeqCst);
        let _ = UnixStream::connect(&server.path);
        server.handle.join().expect("test server thread panicked");
    }

    /// Sends a raw line to a socket and returns the response line.
    fn send_raw(socket_path: &Path, payload: &str) -> String {
        let mut stream = UnixStream::connect(socket_path).unwrap();
        stream.set_read_timeout(Some(Duration::from_millis(5000))).unwrap();
        writeln!(stream, "{payload}").unwrap();
        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        response
    }

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
            detailed: false,
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
    fn test_ipc_command_serialization() {
        let command = IpcCommand::TilingWindowResize {
            dimension: "width".to_string(),
            amount: 40,
        };
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(
            json,
            r#"{"type":"tilingWindowResize","dimension":"width","amount":40}"#
        );

        let command = IpcCommand::Reload;
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(json, r#"{"type":"reload"}"#);

        let command = IpcCommand::TilingWorkspaceBalance;
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(json, r#"{"type":"tilingWorkspaceBalance"}"#);

        let command = IpcCommand::TilingWindowToggleFloating;
        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(json, r#"{"type":"tilingWindowToggleFloating"}"#);
    }

    #[test]
    fn test_ipc_message_query_and_command_round_trip() {
        let query_json = serde_json::to_string(&IpcMessage::Query(IpcQuery::Ping)).unwrap();
        assert_eq!(query_json, r#"{"type":"ping"}"#);
        assert!(matches!(
            serde_json::from_str::<IpcMessage>(&query_json).unwrap(),
            IpcMessage::Query(IpcQuery::Ping)
        ));

        let command_json = serde_json::to_string(&IpcMessage::Command(IpcCommand::Reload)).unwrap();
        assert_eq!(command_json, r#"{"type":"reload"}"#);
        assert!(matches!(
            serde_json::from_str::<IpcMessage>(&command_json).unwrap(),
            IpcMessage::Command(IpcCommand::Reload)
        ));
    }

    #[test]
    fn test_query_round_trip_over_socket() {
        let server = spawn_test_server(|msg| match msg {
            IpcMessage::Query(IpcQuery::Ping) => IpcResponse::success("pong"),
            _ => IpcResponse::error("unexpected message"),
        });

        let response = send_raw(&server.path, r#"{"type":"ping"}"#);
        assert_eq!(response.trim(), r#"{"data":"pong"}"#);

        stop_test_server(server);
    }

    #[test]
    fn test_command_round_trip_over_socket() {
        let server = spawn_test_server(|msg| match msg {
            IpcMessage::Command(IpcCommand::TilingWorkspaceBalance) => IpcResponse::success(true),
            _ => IpcResponse::error("unexpected message"),
        });

        let response = send_raw(&server.path, r#"{"type":"tilingWorkspaceBalance"}"#);
        assert_eq!(response.trim(), r#"{"data":true}"#);

        stop_test_server(server);
    }

    #[test]
    fn test_oversized_request_rejected() {
        let server = spawn_test_server(|_| IpcResponse::success("should not be reached"));

        let oversized = "A".repeat(MAX_REQUEST_SIZE + 1);
        let response = send_raw(&server.path, &oversized);
        assert!(
            response.contains("Request too large"),
            "expected size rejection, got: {response}"
        );

        stop_test_server(server);
    }

    #[test]
    fn test_invalid_request_rejected() {
        let server = spawn_test_server(|_| IpcResponse::success("should not be reached"));

        let response = send_raw(&server.path, "not json at all");
        assert!(
            response.contains("Invalid request"),
            "expected parse rejection, got: {response}"
        );

        stop_test_server(server);
    }

    #[test]
    fn test_concurrent_connections_bounded() {
        let in_flight_h = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let max_seen_h = Arc::clone(&max_seen);
        let server = spawn_test_server(move |_| {
            let now = in_flight_h.fetch_add(1, Ordering::SeqCst) + 1;
            max_seen_h.fetch_max(now, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(50));
            in_flight_h.fetch_sub(1, Ordering::SeqCst);
            IpcResponse::success(true)
        });

        let clients: Vec<_> = (0..MAX_CONCURRENT_CONNECTIONS + 2)
            .map(|_| {
                let path = server.path.clone();
                thread::spawn(move || send_raw(&path, r#"{"type":"ping"}"#))
            })
            .collect();

        for client in clients {
            let response = client.join().expect("client thread panicked");
            assert_eq!(response.trim(), r#"{"data":true}"#);
        }

        assert!(
            max_seen.load(Ordering::SeqCst) <= MAX_CONCURRENT_CONNECTIONS,
            "server exceeded the connection bound: {}",
            max_seen.load(Ordering::SeqCst)
        );

        stop_test_server(server);
    }

    #[test]
    fn test_app_not_running_when_no_socket() {
        // Remove socket if exists
        remove_socket();
        assert!(!is_app_running());
    }
}
