//! Blocking Unix socket client.
//!
//! The client sends one request per connection and retries only when the host
//! is not running, mirroring the legacy 3-attempt/100 ms policy. Protocol and
//! serialization failures are never retried.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{fmt, thread};

use serde::de::DeserializeOwned;

use crate::ipc::protocol::{CoreReload, CoreStatus, Request, Response, WireError};
use crate::ipc::router::CORE_MODULE;

/// Read and write timeout for one exchange.
pub const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// The maximum accepted response line, guarding against a hostile peer.
pub const MAX_RESPONSE_SIZE: usize = 1024 * 1024;

/// How often a request is retried while the host is starting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts including the first one.
    pub attempts: u32,
    /// Delay between attempts.
    pub delay: Duration,
}

impl RetryPolicy {
    /// The legacy policy: three attempts, 100 ms apart.
    #[must_use]
    pub const fn legacy() -> Self {
        Self {
            attempts: 3,
            delay: Duration::from_millis(100),
        }
    }

    /// One attempt for commands that must stay instant while offline.
    #[must_use]
    pub const fn single() -> Self {
        Self {
            attempts: 1,
            delay: Duration::ZERO,
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self { Self::legacy() }
}

/// A client-side failure.
#[derive(Debug)]
pub enum ClientError {
    /// The socket is absent or nothing is listening.
    NotRunning,
    /// The host did not answer within the timeout.
    Timeout,
    /// A socket read or write failed.
    Io(io::Error),
    /// The request could not be serialized.
    Serialize(String),
    /// The response could not be decoded.
    InvalidResponse(String),
    /// The host answered with a structured error.
    Remote(WireError),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => f.write_str("the Stache app is not running"),
            Self::Timeout => f.write_str("connection timed out"),
            Self::Io(error) => write!(f, "io error: {error}"),
            Self::Serialize(message) => write!(f, "cannot serialize request: {message}"),
            Self::InvalidResponse(message) => write!(f, "invalid response: {message}"),
            Self::Remote(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// A blocking client bound to one socket path.
#[derive(Clone, Debug)]
pub struct IpcClient {
    socket: PathBuf,
    retries: RetryPolicy,
    timeout: Duration,
}

impl IpcClient {
    /// Creates a client with the legacy retry policy.
    #[must_use]
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self::with_options(socket, RetryPolicy::legacy(), IO_TIMEOUT)
    }

    /// Creates a client with explicit limits, used by tests and hosts.
    #[must_use]
    pub fn with_options(
        socket: impl Into<PathBuf>,
        retries: RetryPolicy,
        timeout: Duration,
    ) -> Self {
        Self {
            socket: socket.into(),
            retries,
            timeout,
        }
    }

    /// The socket path this client uses.
    #[must_use]
    pub fn socket_path(&self) -> &Path { &self.socket }

    /// Sends one request and returns the decoded response.
    ///
    /// # Errors
    /// Returns [`ClientError::NotRunning`] after the retry budget is spent,
    /// [`ClientError::Timeout`] for an unresponsive host, and typed protocol or
    /// I/O errors otherwise.
    pub fn request(&self, request: &Request) -> Result<Response, ClientError> {
        let mut attempt = 1;
        loop {
            match self.attempt(request) {
                Ok(response) => return Ok(response),
                Err(ClientError::NotRunning) if attempt < self.retries.attempts => {
                    attempt += 1;
                    thread::sleep(self.retries.delay);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Calls a reserved core method and decodes its typed payload.
    ///
    /// # Errors
    /// Returns the transport failure or [`ClientError::InvalidResponse`] when
    /// the payload does not match the expected type.
    pub fn core<T: DeserializeOwned>(&self, method: &str) -> Result<T, ClientError> {
        match self.request(&Request::without_params(CORE_MODULE, method))? {
            Response::Ok { data } => serde_json::from_value(data)
                .map_err(|error| ClientError::InvalidResponse(error.to_string())),
            Response::Error { error } => Err(ClientError::Remote(error)),
        }
    }

    /// Rereads every configuration file through the running host.
    ///
    /// # Errors
    /// Returns a transport or typed response failure.
    pub fn reload(&self) -> Result<CoreReload, ClientError> { self.core("reload") }

    /// Reads the host's configuration status.
    ///
    /// # Errors
    /// Returns a transport or typed response failure.
    pub fn status(&self) -> Result<CoreStatus, ClientError> { self.core("status") }

    fn attempt(&self, request: &Request) -> Result<Response, ClientError> {
        let mut stream = UnixStream::connect(&self.socket).map_err(map_connect_error)?;
        stream.set_read_timeout(Some(self.timeout)).map_err(ClientError::Io)?;
        stream.set_write_timeout(Some(self.timeout)).map_err(ClientError::Io)?;

        let mut encoded = serde_json::to_vec(request)
            .map_err(|error| ClientError::Serialize(error.to_string()))?;
        encoded.push(b'\n');
        stream.write_all(&encoded).map_err(map_write_error)?;
        stream.flush().map_err(map_write_error)?;

        read_response(&mut stream, self.timeout, MAX_RESPONSE_SIZE)
    }
}

fn read_response(
    stream: &mut UnixStream,
    timeout: Duration,
    limit: usize,
) -> Result<Response, ClientError> {
    let deadline = Instant::now() + timeout;
    let mut response = Vec::with_capacity(256);
    let mut chunk = [0_u8; 4096];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ClientError::Timeout);
        }
        stream.set_read_timeout(Some(remaining)).map_err(ClientError::Io)?;
        match stream.read(&mut chunk) {
            Ok(0) => {
                return Err(ClientError::InvalidResponse(
                    "incomplete response: missing newline".to_owned(),
                ));
            }
            Ok(read) => {
                let newline = chunk[..read].iter().position(|byte| *byte == b'\n');
                let length = newline.unwrap_or(read);
                if length > limit.saturating_sub(response.len()) {
                    return Err(ClientError::InvalidResponse(format!(
                        "response exceeds {limit} bytes"
                    )));
                }
                response.extend_from_slice(&chunk[..length]);
                if newline.is_some() {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(map_read_error(error)),
        }
    }
    if response.is_empty() {
        return Err(ClientError::InvalidResponse("empty response".to_owned()));
    }
    serde_json::from_slice::<Response>(&response)
        .map_err(|error| ClientError::InvalidResponse(error.to_string()))
}

fn map_connect_error(error: io::Error) -> ClientError {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => ClientError::NotRunning,
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ClientError::Timeout,
        _ => ClientError::Io(error),
    }
}

fn map_write_error(error: io::Error) -> ClientError {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ClientError::Timeout,
        _ => ClientError::Io(error),
    }
}

fn map_read_error(error: io::Error) -> ClientError {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ClientError::Timeout,
        _ => ClientError::Io(error),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;
    use crate::ipc::protocol::{CoreApplyOutcome, CoreFileOutcome};
    use crate::ipc::router::{CoreHandler, HandlerError, HandlerResult, IpcHandler, Router};
    use crate::paths::AppPaths;

    #[test]
    fn post_connect_resets_and_broken_pipes_are_never_retryable() {
        for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::ConnectionReset] {
            assert!(
                matches!(map_write_error(io::Error::from(kind)), ClientError::Io(error) if error.kind() == kind)
            );
            assert!(
                matches!(map_read_error(io::Error::from(kind)), ClientError::Io(error) if error.kind() == kind)
            );
            assert!(
                matches!(map_connect_error(io::Error::from(kind)), ClientError::Io(error) if error.kind() == kind)
            );
        }
    }

    #[test]
    fn a_peer_closing_after_receiving_the_request_is_contacted_only_once() {
        let root = directory();
        let socket = root.path().join("close-after-action");
        let listener = UnixListener::bind(&socket).expect("listener");
        let observer = listener.try_clone().expect("observer");
        let receipts = Arc::new(AtomicUsize::new(0));
        let counter = receipts.clone();
        let peer = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(&mut stream).read_line(&mut line).expect("complete request");
            assert!(line.ends_with('\n'));
            serde_json::from_str::<Request>(&line).expect("valid request");
            counter.fetch_add(1, Ordering::SeqCst);
            stream.shutdown(std::net::Shutdown::Both).expect("close after action");
        });
        let error =
            IpcClient::with_options(&socket, RetryPolicy::legacy(), Duration::from_millis(200))
                .request(&Request::without_params("demo", "action"))
                .expect_err("lost reply");
        assert!(
            matches!(error, ClientError::InvalidResponse(_) | ClientError::Io(_)),
            "{error}"
        );
        peer.join().expect("peer");
        assert_eq!(receipts.load(Ordering::SeqCst), 1);
        observer.set_nonblocking(true).expect("nonblocking");
        assert!(
            matches!(observer.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
    }

    fn framed_response(payload: &[u8], limit: usize) -> Result<Response, ClientError> {
        let (mut reader, mut writer) = UnixStream::pair().expect("pair");
        writer.write_all(payload).expect("write fixture");
        writer.shutdown(std::net::Shutdown::Write).expect("EOF");
        read_response(&mut reader, Duration::from_secs(1), limit)
    }

    #[test]
    fn response_ceiling_includes_the_final_chunk_but_not_the_newline() {
        let body = br#"{"status":"ok","data":null}"#;
        let mut framed = body.to_vec();
        framed.push(b'\n');
        assert!(matches!(
            framed_response(&framed, body.len()),
            Ok(Response::Ok { .. })
        ));
        assert!(matches!(
            framed_response(&framed, body.len() - 1),
            Err(ClientError::InvalidResponse(_))
        ));
        let mut oversized = vec![b' '; 4096];
        oversized.extend_from_slice(&framed);
        assert!(matches!(
            framed_response(&oversized, 4096 + body.len() - 1),
            Err(ClientError::InvalidResponse(_))
        ));
    }

    #[test]
    fn valid_json_without_a_newline_is_incomplete() {
        assert!(
            matches!(framed_response(br#"{"status":"ok","data":null}"#, 1024), Err(ClientError::InvalidResponse(message)) if message.contains("missing newline"))
        );
    }

    #[test]
    fn trickling_response_has_a_total_deadline_and_is_not_retried() {
        let root = directory();
        let socket = root.path().join("trickle");
        let listener = UnixListener::bind(&socket).expect("listener");
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            BufReader::new(stream.try_clone().expect("clone"))
                .read_line(&mut line)
                .expect("request");
            for _ in 0..100 {
                if stream.write_all(b" ").is_err() {
                    break;
                }
                if stop_rx.recv_timeout(Duration::from_millis(10)).is_ok() {
                    break;
                }
            }
            listener.set_nonblocking(true).expect("nonblocking");
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
            );
        });
        let started = Instant::now();
        let error =
            IpcClient::with_options(socket, RetryPolicy::legacy(), Duration::from_millis(80))
                .status()
                .expect_err("deadline");
        assert!(matches!(error, ClientError::Timeout), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = stop_tx.send(());
        worker.join().expect("worker");
    }

    struct FakeCore;

    impl CoreHandler for FakeCore {
        fn reload(&self) -> HandlerResult<CoreReload> {
            Ok(CoreReload {
                files: vec![CoreFileOutcome {
                    file: "global.jsonc".to_owned(),
                    outcome: CoreApplyOutcome::Applied { revision: 2 },
                    diagnostic: None,
                }],
                access: None,
            })
        }

        fn status(&self) -> HandlerResult<CoreStatus> {
            Ok(CoreStatus {
                directory: "/config".into(),
                files: Vec::new(),
                access: None,
            })
        }
    }

    struct Echo;

    impl IpcHandler for Echo {
        fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> HandlerResult<serde_json::Value> {
            match method {
                "echo" => Ok(params),
                _ => Err(HandlerError::unknown_method(method)),
            }
        }
    }

    fn paths(root: &Path) -> AppPaths {
        AppPaths::resolve(&crate::paths::AppEnvironment {
            home: Some(root.to_path_buf()),
            temporary: Some(root.to_path_buf()),
        })
        .expect("paths")
    }

    fn router() -> Arc<Router> {
        let mut router = Router::new(Arc::new(FakeCore));
        router.register("demo", Arc::new(Echo)).expect("register");
        Arc::new(router)
    }

    fn directory() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(root.path()).expect("dir");
        root
    }

    #[test]
    fn typed_requests_round_trip_over_a_real_socket() {
        let root = directory();
        let paths = paths(root.path());
        let server = crate::ipc::server::IpcServer::start(&paths, router()).expect("server");
        let client = IpcClient::new(paths.socket_path().expect("socket"));
        let response = client
            .request(&Request::new("demo", "echo", json!({ "text": "hi" })).expect("request"))
            .expect("response");
        assert_eq!(response, Response::ok(json!({ "text": "hi" })));
        drop(server);
    }

    #[test]
    fn core_methods_are_decoded_into_their_types() {
        let root = directory();
        let paths = paths(root.path());
        let _server = crate::ipc::server::IpcServer::start(&paths, router()).expect("server");
        let client = IpcClient::new(paths.socket_path().expect("socket"));
        let report = client.reload().expect("reload");
        assert_eq!(report.files[0].file, "global.jsonc");
        let status = client.status().expect("status");
        assert_eq!(status.directory, Path::new("/config"));
    }

    #[test]
    fn a_missing_socket_is_reported_as_not_running() {
        let root = directory();
        let client = IpcClient::new(root.path().join("absent.sock"));
        let error = client.reload().expect_err("missing");
        assert!(matches!(error, ClientError::NotRunning), "{error}");
    }

    #[test]
    fn a_late_running_host_is_reached_within_the_retry_budget() {
        let root = directory();
        let paths = paths(root.path());
        let socket = paths.socket_path().expect("socket");
        let router = router();
        let delayed = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            crate::ipc::server::IpcServer::start(&paths, router)
        });
        let client = IpcClient::new(&socket);
        let status = client.status().expect("retry succeeds");
        assert_eq!(status.directory, Path::new("/config"));
        let server = delayed.join().expect("server thread").expect("server");
        drop(server);
    }

    #[test]
    fn protocol_errors_are_not_retried() {
        let root = directory();
        let socket = root.path().join("garbage.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        let thread = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let _ = stream.write_all(b"not json\n");
            }
            listener
        });
        let client = IpcClient::with_options(&socket, RetryPolicy::legacy(), IO_TIMEOUT);
        let error = client
            .request(&Request::without_params("core", "status"))
            .expect_err("invalid response");
        assert!(matches!(error, ClientError::InvalidResponse(_)), "{error}");
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        thread.join().expect("server thread");
    }
}
