//! Bounded Unix socket server.
//!
//! The server owns its endpoint: it creates the runtime directory with `0700`,
//! binds the socket with `0600`, refuses a live or foreign endpoint, and
//! removes only the socket inode it created. Connections are capped, requests
//! are limited to 64 KiB and every read/write has a bounded timeout.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use std::{fmt, fs};

use crate::ipc::protocol::{Request, Response, WireErrorKind};
use crate::ipc::router::Router;
use crate::lock;
use crate::paths::{AppPathError, AppPaths};
use crate::shutdown::{ShutdownWait, wait_for_finish};

/// Maximum concurrent connections served at once.
pub const MAX_CONNECTIONS: usize = 8;

/// Maximum request line size in bytes.
pub const REQUEST_LIMIT: usize = 64 * 1024;

/// Read and write timeout for one connection.
pub const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Limits applied to one server instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerConfig {
    /// Concurrent connections; zero is clamped to one.
    pub max_connections: usize,
    /// Maximum request line length in bytes; zero is clamped to one.
    pub request_limit: usize,
    /// Nonzero read and write timeout for one connection.
    pub timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_connections: MAX_CONNECTIONS,
            request_limit: REQUEST_LIMIT,
            timeout: IO_TIMEOUT,
        }
    }
}

impl ServerConfig {
    fn normalized(self) -> Self {
        Self {
            max_connections: self.max_connections.max(1),
            request_limit: self.request_limit.max(1),
            timeout: self.timeout,
        }
    }
}

/// A server-side failure.
#[derive(Debug)]
pub enum ServerError {
    /// A zero timeout would disable bounded socket I/O.
    ZeroTimeout,
    /// The identity paths could not be resolved.
    Path(AppPathError),
    /// A filesystem operation failed.
    Io {
        /// The failed operation.
        action: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The operating-system error.
        error: io::Error,
    },
    /// The runtime path exists but is not a directory.
    NotADirectory {
        /// The offending path.
        path: PathBuf,
    },
    /// A managed path is a symbolic link.
    Symlink {
        /// The offending path.
        path: PathBuf,
    },
    /// The endpoint exists but is not a socket.
    NotASocket {
        /// The offending path.
        path: PathBuf,
    },
    /// A managed path is owned by another user.
    OwnerMismatch {
        /// The offending path.
        path: PathBuf,
        /// The expected owner.
        expected: u32,
        /// The observed owner.
        actual: u32,
    },
    /// Another server is listening on the endpoint.
    AlreadyRunning {
        /// The live endpoint.
        path: PathBuf,
    },
    /// The endpoint exists and could not be probed safely.
    EndpointBusy {
        /// The endpoint.
        path: PathBuf,
        /// The failed probe.
        error: io::Error,
    },
    /// The accept thread could not be spawned.
    Thread {
        /// The spawn error.
        error: io::Error,
    },
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroTimeout => f.write_str("socket timeout must be greater than zero"),
            Self::Path(error) => error.fmt(f),
            Self::Io { action, path, error } => {
                write!(f, "cannot {action} {}: {error}", path.display())
            }
            Self::NotADirectory { path } => {
                write!(f, "{} is not a directory", path.display())
            }
            Self::Symlink { path } => {
                write!(f, "{} is a symbolic link; refusing to use it", path.display())
            }
            Self::NotASocket { path } => write!(f, "{} is not a socket", path.display()),
            Self::OwnerMismatch { path, expected, actual } => write!(
                f,
                "{} is owned by uid {actual}, expected {expected}",
                path.display()
            ),
            Self::AlreadyRunning { path } => {
                write!(f, "another Stache server is listening on {}", path.display())
            }
            Self::EndpointBusy { path, error } => {
                write!(f, "cannot probe {}: {error}", path.display())
            }
            Self::Thread { error } => write!(f, "cannot start the socket server: {error}"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Path(error) => Some(error),
            Self::Io { error, .. } | Self::EndpointBusy { error, .. } | Self::Thread { error } => {
                Some(error)
            }
            _ => None,
        }
    }
}

impl From<AppPathError> for ServerError {
    fn from(error: AppPathError) -> Self { Self::Path(error) }
}

/// The acknowledgement of one bounded socket shutdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SocketShutdown {
    /// The accept thread and every handler finished, and the owned inode is gone.
    Acknowledged,
    /// The bound expired with the accept thread or a handler still running.
    Unacknowledged,
    /// The owned socket inode could not be removed.
    RemovalFailed(String),
}

/// A listening control server. Dropping it stops the accept loop and removes
/// the owned endpoint.
pub struct IpcServer {
    endpoint: Endpoint,
    gate: Arc<Gate>,
    /// Retained so stage-1 teardown can stop dispatch without the accept loop.
    router: Arc<Router>,
    worker: Option<JoinHandle<()>>,
    lock_file: Option<fs::File>,
}

impl IpcServer {
    /// Starts the server with the default limits.
    ///
    /// # Errors
    /// Returns [`ServerError`] when the runtime directory or endpoint cannot be
    /// used safely, another server is live, or the accept thread cannot start.
    pub fn start(paths: &AppPaths, router: Arc<Router>) -> Result<Self, ServerError> {
        Self::start_with(paths, router, ServerConfig::default())
    }

    /// Starts the server with explicit limits.
    ///
    /// # Errors
    /// Returns [`ServerError::ZeroTimeout`] for a zero timeout, or another
    /// [`ServerError`] when the directory/endpoint is unsafe, a server is live,
    /// or the accept thread cannot start.
    pub fn start_with(
        paths: &AppPaths,
        router: Arc<Router>,
        config: ServerConfig,
    ) -> Result<Self, ServerError> {
        if config.timeout.is_zero() {
            return Err(ServerError::ZeroTimeout);
        }
        let config = config.normalized();
        let socket = paths.socket_path()?;
        prepare_runtime_directory(paths.runtime_directory(), paths.owner())?;
        let lock_file = lock_runtime(paths.runtime_directory(), paths.owner())?;
        prepare_runtime_directory(paths.runtime_directory(), paths.owner())?;
        remove_stale_endpoint(&socket, paths.owner())?;
        let listener = UnixListener::bind(&socket).map_err(|error| ServerError::Io {
            action: "bind",
            path: socket.clone(),
            error,
        })?;
        let endpoint = Endpoint::claim(&socket, paths.owner())?;
        listener.set_nonblocking(true).map_err(|error| ServerError::Io {
            action: "configure",
            path: socket.clone(),
            error,
        })?;
        let gate = Arc::new(Gate::new(config.max_connections));
        let worker_gate = Arc::clone(&gate);
        let worker_router = Arc::clone(&router);
        let worker = match thread::Builder::new()
            .name("stache-ipc".to_owned())
            .spawn(move || serve(listener, worker_router, config, worker_gate))
        {
            Ok(worker) => worker,
            Err(error) => {
                let _ = endpoint.remove();
                return Err(ServerError::Thread { error });
            }
        };
        Ok(Self {
            endpoint,
            gate,
            router,
            worker: Some(worker),
            lock_file: Some(lock_file),
        })
    }

    /// The endpoint this server owns.
    #[must_use]
    pub fn socket_path(&self) -> &Path { &self.endpoint.path }

    /// Stops accepting connections and dispatch without joining anything.
    ///
    /// Takes no lock and joins no thread: an already-accepted connection may
    /// finish, and a request that was already read is answered with the
    /// `unavailable` wire kind.
    pub fn stop_accepting(&self) {
        self.gate.stop();
        self.router.stop();
    }

    /// Stops accepting, waits for the accept thread and every handler permit
    /// within the teardown bound, then removes the owned endpoint.
    ///
    /// One bound covers both facts: the accept thread finished and no handler
    /// still holds a gate permit. On expiry the thread is detached instead of
    /// joined. Endpoint removal never depends on that bound, so a connection
    /// holding its handler cannot delay it; a real removal failure is reported
    /// instead of being dropped.
    pub fn shutdown_bounded(mut self, wait: &dyn ShutdownWait) -> SocketShutdown {
        self.stop_accepting();
        let worker = self.worker.take();
        let gate = Arc::clone(&self.gate);
        let finished = wait_for_finish(wait, &|| {
            worker.as_ref().is_none_or(JoinHandle::is_finished) && gate.is_idle()
        });
        if finished && let Some(worker) = worker {
            let _ = worker.join();
        }
        let removal = self.endpoint.remove();
        self.lock_file.take();
        match removal {
            Err(error) => SocketShutdown::RemovalFailed(error),
            Ok(()) if finished => SocketShutdown::Acknowledged,
            Ok(()) => SocketShutdown::Unacknowledged,
        }
    }

    /// Stops accepting, joins the accept thread and removes the owned endpoint.
    ///
    /// This is the unbounded legacy path; teardown uses
    /// [`IpcServer::shutdown_bounded`]. Repeated calls are no-ops.
    pub fn shutdown(&mut self) {
        self.stop_accepting();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = self.endpoint.remove();
        self.lock_file.take();
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) { self.shutdown(); }
}

impl fmt::Debug for IpcServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IpcServer")
            .field("socket", &self.endpoint.path)
            .finish_non_exhaustive()
    }
}

struct Endpoint {
    path: PathBuf,
    device: u64,
    inode: u64,
    owned: AtomicBool,
}

impl Endpoint {
    fn claim(path: &Path, owner: u32) -> Result<Self, ServerError> {
        let metadata = fs::symlink_metadata(path).map_err(|error| ServerError::Io {
            action: "inspect",
            path: path.to_path_buf(),
            error,
        })?;
        if !metadata.file_type().is_socket() {
            return Err(ServerError::NotASocket { path: path.to_path_buf() });
        }
        if metadata.uid() != owner {
            return Err(ServerError::OwnerMismatch {
                path: path.to_path_buf(),
                expected: owner,
                actual: metadata.uid(),
            });
        }
        let endpoint = Self {
            path: path.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
            owned: AtomicBool::new(true),
        };
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            ServerError::Io {
                action: "restrict",
                path: path.to_path_buf(),
                error,
            }
        })?;
        Ok(endpoint)
    }

    /// Removes the path only if it is still the socket this process created.
    ///
    /// A missing path and a foreign inode are success: the owned endpoint is
    /// gone either way. A real removal error is reported.
    fn remove(&self) -> Result<(), String> {
        if !self.owned.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            return fs::remove_file(&self.path).map_err(|error| error.to_string());
        }
        Ok(())
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) { let _ = self.remove(); }
}

fn lock_runtime(directory: &Path, owner: u32) -> Result<fs::File, ServerError> {
    let original_directory = fs::symlink_metadata(directory).map_err(|error| ServerError::Io {
        action: "inspect runtime",
        path: directory.to_path_buf(),
        error,
    })?;
    verify_directory(directory, &original_directory, owner)?;
    let path = directory.join("server.lock");
    let fd = rustix::fs::open(
        &path,
        rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .map_err(|error| ServerError::Io {
        action: "open lock",
        path: path.clone(),
        error: error.into(),
    })?;
    let file = fs::File::from(fd);
    let metadata = file.metadata().map_err(|error| ServerError::Io {
        action: "inspect lock",
        path: path.clone(),
        error,
    })?;
    if !metadata.is_file()
        || metadata.uid() != owner
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(ServerError::EndpointBusy {
            path,
            error: io::Error::other("lock must be private, owned and a regular file"),
        });
    }
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => {
            ServerError::AlreadyRunning { path: directory.to_path_buf() }
        }
        fs::TryLockError::Error(error) => ServerError::Io {
            action: "lock runtime",
            path: path.clone(),
            error,
        },
    })?;
    let current = fs::symlink_metadata(&path).map_err(|error| ServerError::Io {
        action: "recheck lock",
        path: path.clone(),
        error,
    })?;
    if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
        return Err(ServerError::EndpointBusy {
            path,
            error: io::Error::other("lock file was replaced"),
        });
    }
    let current_directory = fs::symlink_metadata(directory).map_err(|error| ServerError::Io {
        action: "recheck runtime",
        path: directory.to_path_buf(),
        error,
    })?;
    verify_directory(directory, &current_directory, owner)?;
    if current_directory.dev() != original_directory.dev()
        || current_directory.ino() != original_directory.ino()
    {
        return Err(ServerError::EndpointBusy {
            path: directory.to_path_buf(),
            error: io::Error::other("runtime directory was replaced"),
        });
    }
    Ok(file)
}

pub(crate) fn prepare_runtime_directory(path: &Path, owner: u32) -> Result<(), ServerError> {
    if let Some(parent) = path.parent() {
        let metadata = fs::symlink_metadata(parent).map_err(|error| ServerError::Io {
            action: "inspect runtime root",
            path: parent.to_path_buf(),
            error,
        })?;
        verify_directory(parent, &metadata, owner)?;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => verify_directory(path, &metadata, owner)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .or_else(|error| {
                    if error.kind() == io::ErrorKind::AlreadyExists {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .map_err(|error| ServerError::Io {
                    action: "create",
                    path: path.to_path_buf(),
                    error,
                })?;
        }
        Err(error) => {
            return Err(ServerError::Io {
                action: "inspect",
                path: path.to_path_buf(),
                error,
            });
        }
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| ServerError::Io {
        action: "inspect",
        path: path.to_path_buf(),
        error,
    })?;
    verify_directory(path, &metadata, owner)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| ServerError::Io {
        action: "restrict",
        path: path.to_path_buf(),
        error,
    })
}

fn verify_directory(path: &Path, metadata: &fs::Metadata, owner: u32) -> Result<(), ServerError> {
    if metadata.file_type().is_symlink() {
        return Err(ServerError::Symlink { path: path.to_path_buf() });
    }
    if !metadata.is_dir() {
        return Err(ServerError::NotADirectory { path: path.to_path_buf() });
    }
    if metadata.uid() != owner {
        return Err(ServerError::OwnerMismatch {
            path: path.to_path_buf(),
            expected: owner,
            actual: metadata.uid(),
        });
    }
    Ok(())
}

fn remove_stale_endpoint(path: &Path, owner: u32) -> Result<(), ServerError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(ServerError::Io {
                action: "inspect",
                path: path.to_path_buf(),
                error,
            });
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(ServerError::Symlink { path: path.to_path_buf() });
    }
    if !metadata.file_type().is_socket() {
        return Err(ServerError::NotASocket { path: path.to_path_buf() });
    }
    if metadata.uid() != owner {
        return Err(ServerError::OwnerMismatch {
            path: path.to_path_buf(),
            expected: owner,
            actual: metadata.uid(),
        });
    }
    match UnixStream::connect(path) {
        Ok(_) => return Err(ServerError::AlreadyRunning { path: path.to_path_buf() }),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) => {}
        Err(error) => {
            return Err(ServerError::EndpointBusy {
                path: path.to_path_buf(),
                error,
            });
        }
    }
    let current = fs::symlink_metadata(path).map_err(|error| ServerError::Io {
        action: "recheck stale socket",
        path: path.to_path_buf(),
        error,
    })?;
    if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
        return Err(ServerError::EndpointBusy {
            path: path.to_path_buf(),
            error: io::Error::other("stale endpoint was replaced"),
        });
    }
    fs::remove_file(path).map_err(|error| ServerError::Io {
        action: "remove stale socket",
        path: path.to_path_buf(),
        error,
    })
}

struct Gate {
    limit: usize,
    stopped: AtomicBool,
    available: Mutex<usize>,
    wake: Condvar,
    #[cfg(test)]
    retained_handles: std::sync::atomic::AtomicUsize,
}

impl Gate {
    const fn new(limit: usize) -> Self {
        Self {
            limit,
            stopped: AtomicBool::new(false),
            available: Mutex::new(limit),
            wake: Condvar::new(),
            #[cfg(test)]
            retained_handles: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Whether no connection currently holds a permit.
    fn is_idle(&self) -> bool { *lock(&self.available) >= self.limit }

    fn acquire(&self) -> bool {
        let mut available = lock(&self.available);
        while *available == 0 {
            if self.stopped.load(Ordering::SeqCst) {
                return false;
            }
            available =
                self.wake.wait(available).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if self.stopped.load(Ordering::SeqCst) {
            return false;
        }
        *available -= 1;
        true
    }

    fn release(&self) {
        *lock(&self.available) += 1;
        self.wake.notify_one();
    }

    fn stop(&self) {
        let _available = lock(&self.available);
        self.stopped.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }

    fn wait_for_stop(&self, timeout: Duration) {
        let available = lock(&self.available);
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.wake.wait_timeout(available, timeout);
    }
}

struct Permit {
    gate: Arc<Gate>,
    completed: SyncSender<thread::ThreadId>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        // Publish completion before making the slot available: once acquire
        // succeeds, the accept loop can reap the specific finished handler.
        let _ = self.completed.send(thread::current().id());
        self.gate.release();
    }
}

fn reap_workers(
    workers: &mut HashMap<thread::ThreadId, JoinHandle<()>>,
    completed: &Receiver<thread::ThreadId>,
) {
    while let Ok(id) = completed.try_recv() {
        if let Some(worker) = workers.remove(&id) {
            let _ = worker.join();
        }
    }
}

#[allow(clippy::needless_pass_by_value)] // Ownership moves into the spawned accept thread.
fn serve(listener: UnixListener, router: Arc<Router>, config: ServerConfig, gate: Arc<Gate>) {
    // The parameters own the listener and reference counts because the whole
    // accept loop runs on the thread spawned by `start_with`.
    let mut workers = HashMap::new();
    let (completed, completions) = sync_channel(config.max_connections);
    loop {
        reap_workers(&mut workers, &completions);
        if gate.stopped.load(Ordering::SeqCst) {
            break;
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                gate.wait_for_stop(Duration::from_millis(50));
                continue;
            }
            Err(_) => {
                gate.wait_for_stop(Duration::from_millis(50));
                continue;
            }
        };
        if !gate.acquire() {
            break;
        }
        reap_workers(&mut workers, &completions);
        let worker_router = Arc::clone(&router);
        let permit = Permit {
            gate: Arc::clone(&gate),
            completed: completed.clone(),
        };
        let handle = thread::Builder::new().name("stache-ipc-conn".to_owned()).spawn(move || {
            let _permit = permit;
            handle_connection(stream, &worker_router, &config);
        });
        if let Ok(handle) = handle {
            workers.insert(handle.thread().id(), handle);
            #[cfg(test)]
            gate.retained_handles.fetch_max(workers.len(), Ordering::SeqCst);
        }
    }
    // Connection workers are detached, never joined: a handler that ignores
    // cancellation must not hold shutdown. Each keeps its permit, so the gate
    // still bounds how many can run while they finish.
    drop(workers);
}

fn handle_connection(mut stream: UnixStream, router: &Router, config: &ServerConfig) {
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(config.timeout)).is_err()
        || stream.set_write_timeout(Some(config.timeout)).is_err()
    {
        return;
    }
    let response = match read_request(&mut stream, config) {
        Ok(bytes) => match serde_json::from_slice::<Request>(&bytes) {
            Ok(request) => {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| router.route(request)))
                    .unwrap_or_else(|_| {
                        Response::failure(WireErrorKind::Handler, "handler panicked")
                    })
            }
            Err(error) => Response::failure(
                WireErrorKind::MalformedRequest,
                format!("invalid request: {error}"),
            ),
        },
        Err(ReadError::TooLarge) => Response::failure(
            WireErrorKind::MalformedRequest,
            format!("request exceeds {} bytes", config.request_limit),
        ),
        Err(ReadError::Empty) => {
            Response::failure(WireErrorKind::MalformedRequest, "empty request")
        }
        Err(ReadError::Timeout | ReadError::Incomplete | ReadError::Io) => return,
    };
    let Ok(mut encoded) = serde_json::to_vec(&response) else {
        return;
    };
    encoded.push(b'\n');
    let _ = stream.write_all(&encoded);
    let _ = stream.flush();
}

enum ReadError {
    Timeout,
    Empty,
    Incomplete,
    TooLarge,
    Io,
}

fn read_request(stream: &mut UnixStream, config: &ServerConfig) -> Result<Vec<u8>, ReadError> {
    let deadline = Instant::now() + config.timeout;
    let mut request = Vec::with_capacity(256);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ReadError::Timeout);
        }
        stream.set_read_timeout(Some(remaining)).map_err(|_| ReadError::Io)?;
        let mut chunk = [0_u8; 1024];
        match stream.read(&mut chunk) {
            Ok(0) => {
                return Err(if request.is_empty() {
                    ReadError::Empty
                } else {
                    ReadError::Incomplete
                });
            }
            Ok(read) => {
                if let Some(position) = chunk[..read].iter().position(|byte| *byte == b'\n') {
                    if request.is_empty() && position == 0 {
                        return Err(ReadError::Empty);
                    }
                    if request.len() + position > config.request_limit {
                        return Err(ReadError::TooLarge);
                    }
                    request.extend_from_slice(&chunk[..position]);
                    return Ok(request);
                }
                request.extend_from_slice(&chunk[..read]);
                if request.len() > config.request_limit {
                    return Err(ReadError::TooLarge);
                }
            }
            Err(error)
                if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) =>
            {
                return Err(ReadError::Timeout);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(ReadError::Io),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    use serde_json::json;

    use super::*;
    use crate::ipc::protocol::{CoreReload, CoreStatus};
    use crate::ipc::router::{CoreHandler, HandlerError, HandlerResult, IpcHandler};
    use crate::paths::AppEnvironment;

    #[test]
    fn completed_second_handler_allows_third_while_first_is_blocked() {
        struct Holding {
            entered: std::sync::mpsc::SyncSender<()>,
            release: Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl IpcHandler for Holding {
            fn handle(
                &self,
                method: &str,
                _: serde_json::Value,
            ) -> HandlerResult<serde_json::Value> {
                if method == "first" {
                    self.entered.send(()).expect("entered");
                    lock(&self.release)
                        .recv_timeout(Duration::from_secs(5))
                        .expect("release first");
                }
                Ok(json!(method))
            }
        }
        let root = root();
        let paths = paths(root.path());
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let mut router = Router::new(Arc::new(NoCore));
        router
            .register(
                "holding",
                Arc::new(Holding {
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                }),
            )
            .expect("register");
        let server = IpcServer::start_with(&paths, Arc::new(router), ServerConfig {
            max_connections: 2,
            ..ServerConfig::default()
        })
        .expect("server");
        let socket = server.socket_path().to_path_buf();
        let first_socket = socket.clone();
        let first = thread::spawn(move || {
            exchange(
                &first_socket,
                r#"{"module":"holding","method":"first","params":null}"#,
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(2)).expect("first is blocked");
        assert!(
            exchange(
                &socket,
                r#"{"module":"holding","method":"second","params":null}"#
            )
            .contains("second")
        );
        let (third_tx, third_rx) = std::sync::mpsc::sync_channel(1);
        let third = thread::spawn(move || {
            let result =
                exchange(&socket, r#"{"module":"holding","method":"third","params":null}"#);
            third_tx.send(result).expect("third result");
        });
        let completed_before_release = third_rx.recv_timeout(Duration::from_secs(2));
        release_tx.send(()).expect("release first");
        first.join().expect("first");
        third.join().expect("third");
        assert!(
            completed_before_release
                .expect("third must not wait for first")
                .contains("third")
        );
        assert!(server.gate.retained_handles.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn zero_timeout_is_rejected_before_creating_the_endpoint() {
        let root = root();
        let paths = paths(root.path());
        let error = IpcServer::start_with(&paths, router(), ServerConfig {
            timeout: Duration::ZERO,
            ..ServerConfig::default()
        })
        .expect_err("invalid timeout");
        assert!(matches!(error, ServerError::ZeroTimeout));
        assert!(!paths.runtime_directory().exists());
    }

    #[test]
    fn sequential_connections_keep_retained_handles_bounded() {
        let root = root();
        let paths = paths(root.path());
        let server = IpcServer::start_with(&paths, router(), ServerConfig {
            max_connections: 2,
            ..ServerConfig::default()
        })
        .expect("server");
        for _ in 0..40 {
            let response = exchange(
                server.socket_path(),
                r#"{"module":"demo","method":"echo","params":null}"#,
            );
            assert!(response.contains("ok"));
        }
        assert!(server.gate.retained_handles.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn panicking_handlers_release_the_only_connection_permit() {
        struct Panics;
        impl IpcHandler for Panics {
            fn handle(&self, _: &str, _: serde_json::Value) -> HandlerResult<serde_json::Value> {
                panic!("test panic")
            }
        }
        let root = root();
        let paths = paths(root.path());
        let mut router = Router::new(Arc::new(NoCore));
        router.register("panic", Arc::new(Panics)).expect("register");
        let server = IpcServer::start_with(&paths, Arc::new(router), ServerConfig {
            max_connections: 1,
            ..ServerConfig::default()
        })
        .expect("server");
        for _ in 0..3 {
            let response = exchange(
                server.socket_path(),
                r#"{"module":"panic","method":"run","params":null}"#,
            );
            assert!(response.contains("handler panicked"));
        }
        assert!(
            exchange(
                server.socket_path(),
                r#"{"module":"core","method":"status","params":null}"#
            )
            .contains("ok")
        );
    }

    #[test]
    fn stopping_races_blocked_acquire_without_losing_wakes() {
        for _ in 0..100 {
            let gate = Arc::new(Gate::new(1));
            assert!(gate.acquire());
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let (tx, rx) = std::sync::mpsc::channel();
            let waiter = {
                let gate = gate.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    tx.send(gate.acquire()).expect("send");
                })
            };
            barrier.wait();
            gate.stop();
            assert!(!rx.recv_timeout(Duration::from_secs(2)).expect("stop wakes acquire"));
            waiter.join().expect("waiter");
        }
    }

    #[test]
    fn simultaneous_stale_startup_has_one_reachable_winner() {
        let root = root();
        let paths = paths(root.path());
        prepare_runtime_directory(paths.runtime_directory(), paths.owner()).expect("runtime");
        drop(UnixListener::bind(paths.socket_path().expect("path")).expect("stale"));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let attempts: Vec<_> = (0..2)
            .map(|_| {
                let paths = paths.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    IpcServer::start(&paths, router())
                })
            })
            .collect();
        let mut winner = None;
        let mut losers = 0;
        for attempt in attempts {
            match attempt.join().expect("startup") {
                Ok(server) => {
                    assert!(winner.is_none());
                    winner = Some(server);
                }
                Err(ServerError::AlreadyRunning { .. }) => losers += 1,
                Err(error) => panic!("unexpected startup failure: {error}"),
            }
        }
        assert_eq!(losers, 1);
        assert!(
            exchange(
                winner.as_ref().expect("winner").socket_path(),
                r#"{"module":"core","method":"status","params":null}"#
            )
            .contains("ok")
        );
    }

    #[test]
    fn symlink_lock_and_claim_failure_leave_foreign_files_untouched() {
        let root = root();
        let paths = paths(root.path());
        prepare_runtime_directory(paths.runtime_directory(), paths.owner()).expect("runtime");
        let target = root.path().join("foreign");
        fs::write(&target, "keep").expect("target");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).expect("mode");
        std::os::unix::fs::symlink(&target, paths.runtime_directory().join("server.lock"))
            .expect("link");
        assert!(IpcServer::start(&paths, router()).is_err());
        assert!(Endpoint::claim(&target, paths.owner()).is_err());
        assert_eq!(fs::metadata(&target).expect("target").mode() & 0o777, 0o640);
        assert_eq!(fs::read_to_string(&target).expect("contents"), "keep");
    }

    struct NoCore;

    impl CoreHandler for NoCore {
        fn reload(&self) -> HandlerResult<CoreReload> {
            Ok(CoreReload {
                files: Vec::new(),
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

    struct SlowState {
        entered: usize,
        release: bool,
        max_seen: usize,
    }

    struct Slow {
        state: Mutex<SlowState>,
        wake: Condvar,
    }

    impl Slow {
        fn new() -> Self {
            Self {
                state: Mutex::new(SlowState {
                    entered: 0,
                    release: false,
                    max_seen: 0,
                }),
                wake: Condvar::new(),
            }
        }

        fn wait_for_entry(&self, expected: usize, timeout: Duration) {
            let mut state = lock(&self.state);
            let deadline = Instant::now() + timeout;
            while state.entered < expected {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let (next, _) = self
                    .wake
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state = next;
            }
        }

        fn release(&self) {
            lock(&self.state).release = true;
            self.wake.notify_all();
        }

        fn max_seen(&self) -> usize { lock(&self.state).max_seen }
    }

    impl IpcHandler for Slow {
        fn handle(&self, _: &str, _: serde_json::Value) -> HandlerResult<serde_json::Value> {
            let mut state = lock(&self.state);
            state.entered += 1;
            state.max_seen = state.max_seen.max(state.entered);
            self.wake.notify_all();
            while !state.release {
                state = self.wake.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            state.entered -= 1;
            Ok(json!("done"))
        }
    }

    fn router() -> Arc<Router> {
        let mut router = Router::new(Arc::new(NoCore));
        router.register("demo", Arc::new(Echo)).expect("register");
        Arc::new(router)
    }

    /// A handler that signals entry and then blocks until the test releases it.
    struct Holds {
        entered: std::sync::mpsc::SyncSender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl IpcHandler for Holds {
        fn handle(&self, method: &str, _: serde_json::Value) -> HandlerResult<serde_json::Value> {
            if method == "hold" {
                self.entered.send(()).expect("entered");
                lock(&self.release).recv_timeout(Duration::from_secs(5)).expect("release");
            }
            Ok(json!(method))
        }
    }

    /// Waits until the accept loop has retained `expected` connection workers.
    fn wait_for_workers(server: &IpcServer, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while server.gate.retained_handles.load(Ordering::SeqCst) < expected {
            assert!(
                Instant::now() < deadline,
                "the server never accepted the connection"
            );
            thread::yield_now();
        }
    }

    /// Waits until the endpoint no longer accepts new connections.
    fn wait_for_refusal(socket: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match UnixStream::connect(socket) {
                Err(_) => return,
                Ok(stream) => {
                    drop(stream);
                    assert!(
                        Instant::now() < deadline,
                        "the endpoint still accepts connections"
                    );
                    thread::yield_now();
                }
            }
        }
    }

    fn root() -> tempfile::TempDir { tempfile::tempdir().expect("temp dir") }

    fn paths(root: &Path) -> AppPaths {
        AppPaths::resolve(&AppEnvironment {
            home: Some(root.to_path_buf()),
            temporary: Some(root.to_path_buf()),
        })
        .expect("paths")
    }

    fn connect(socket: &Path) -> UnixStream {
        let stream = UnixStream::connect(socket).expect("connect");
        stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
        stream
    }

    fn exchange(socket: &Path, payload: &str) -> String {
        let mut stream = connect(socket);
        writeln!(stream, "{payload}").expect("write");
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).expect("read");
        response
    }

    #[test]
    fn the_runtime_directory_and_socket_are_owner_only() {
        let root = root();
        let paths = paths(root.path());
        let server = IpcServer::start(&paths, router()).expect("server");
        let runtime = fs::metadata(paths.runtime_directory()).expect("runtime metadata");
        assert_eq!(runtime.permissions().mode() & 0o777, 0o700);
        let socket = fs::symlink_metadata(server.socket_path()).expect("socket metadata");
        assert_eq!(socket.permissions().mode() & 0o777, 0o600);
        assert!(socket.file_type().is_socket());
    }

    #[test]
    fn a_malformed_request_gets_a_typed_error_and_keeps_serving() {
        let root = root();
        let paths = paths(root.path());
        let _server = IpcServer::start(&paths, router()).expect("server");
        let socket = paths.socket_path().expect("socket");
        let malformed = exchange(&socket, "not json");
        assert!(malformed.contains("\"status\":\"error\""), "{malformed}");
        assert!(
            malformed.contains("\"kind\":\"malformedRequest\""),
            "{malformed}"
        );
        let ok = exchange(&socket, r#"{"module":"demo","method":"echo","params":{"a":1}}"#);
        assert!(ok.contains("\"status\":\"ok\""), "{ok}");
    }

    #[test]
    fn unknown_modules_and_methods_fail_safely() {
        let root = root();
        let paths = paths(root.path());
        let _server = IpcServer::start(&paths, router()).expect("server");
        let socket = paths.socket_path().expect("socket");
        let unknown = exchange(&socket, r#"{"module":"ghost","method":"x","params":null}"#);
        assert!(unknown.contains("\"kind\":\"unknownModule\""), "{unknown}");
        let method = exchange(&socket, r#"{"module":"demo","method":"ghost","params":null}"#);
        assert!(method.contains("\"kind\":\"unknownMethod\""), "{method}");
    }

    #[test]
    fn oversized_requests_are_rejected_without_panicking() {
        let root = root();
        let paths = paths(root.path());
        let config = ServerConfig {
            request_limit: 128,
            ..ServerConfig::default()
        };
        let _server = IpcServer::start_with(&paths, router(), config).expect("server");
        let socket = paths.socket_path().expect("socket");
        let oversized = "A".repeat(1024);
        let response = exchange(&socket, &oversized);
        assert!(response.contains("request exceeds 128 bytes"), "{response}");
    }

    #[test]
    fn an_incomplete_request_times_out_without_a_response() {
        let root = root();
        let paths = paths(root.path());
        let config = ServerConfig {
            timeout: Duration::from_millis(150),
            ..ServerConfig::default()
        };
        let _server = IpcServer::start_with(&paths, router(), config).expect("server");
        let socket = paths.socket_path().expect("socket");
        let mut stream = connect(&socket);
        stream.write_all(b"{").expect("partial write");
        let started = Instant::now();
        let mut buffer = Vec::new();
        stream.read_to_end(&mut buffer).expect("read close");
        assert!(buffer.is_empty(), "{buffer:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_stale_endpoint_is_replaced() {
        let root = root();
        let paths = paths(root.path());
        fs::create_dir_all(paths.runtime_directory()).expect("runtime");
        let socket = paths.socket_path().expect("socket");
        drop(UnixListener::bind(&socket).expect("stale bind"));
        assert!(socket.exists());
        let server = IpcServer::start(&paths, router()).expect("server");
        assert!(server.socket_path().exists());
    }

    #[test]
    fn a_live_server_is_never_unlinked() {
        let root = root();
        let paths = paths(root.path());
        let first = IpcServer::start(&paths, router()).expect("first server");
        let error = IpcServer::start(&paths, router()).expect_err("second server");
        assert!(matches!(error, ServerError::AlreadyRunning { .. }), "{error}");
        let response = exchange(
            first.socket_path(),
            r#"{"module":"demo","method":"echo","params":null}"#,
        );
        assert!(response.contains("\"status\":\"ok\""), "{response}");
    }

    #[test]
    fn a_symlinked_endpoint_is_rejected_without_touching_the_target() {
        let root = root();
        let paths = paths(root.path());
        fs::create_dir_all(paths.runtime_directory()).expect("runtime");
        let socket = paths.socket_path().expect("socket");
        let target = root.path().join("target.sock");
        drop(UnixListener::bind(&target).expect("target"));
        std::os::unix::fs::symlink(&target, &socket).expect("symlink");
        let error = IpcServer::start(&paths, router()).expect_err("symlink");
        assert!(matches!(error, ServerError::Symlink { .. }), "{error}");
        assert!(target.exists(), "the symlink target must survive");
    }

    #[test]
    fn a_non_socket_endpoint_is_rejected_and_preserved() {
        let root = root();
        let paths = paths(root.path());
        fs::create_dir_all(paths.runtime_directory()).expect("runtime");
        let socket = paths.socket_path().expect("socket");
        fs::write(&socket, "do not delete").expect("file");
        let error = IpcServer::start(&paths, router()).expect_err("file endpoint");
        assert!(matches!(error, ServerError::NotASocket { .. }), "{error}");
        assert_eq!(fs::read_to_string(&socket).expect("preserved"), "do not delete");
    }

    #[test]
    fn a_symlinked_runtime_directory_is_rejected() {
        let root = root();
        let real = root.path().join("real");
        fs::create_dir(&real).expect("real");
        let paths = paths(root.path());
        std::os::unix::fs::symlink(&real, paths.runtime_directory()).expect("symlink");
        let error = IpcServer::start(&paths, router()).expect_err("symlink dir");
        assert!(matches!(error, ServerError::Symlink { .. }), "{error}");
    }

    #[test]
    fn a_directory_owned_by_another_user_is_rejected() {
        let root = root();
        let paths = paths(root.path());
        fs::create_dir_all(paths.runtime_directory()).expect("runtime");
        let expected = if paths.owner() == u32::MAX {
            0
        } else {
            paths.owner() + 1
        };
        let error = prepare_runtime_directory(paths.runtime_directory(), expected)
            .expect_err("owner mismatch");
        assert!(matches!(error, ServerError::OwnerMismatch { .. }), "{error}");
    }

    #[test]
    fn a_socket_owned_by_another_user_is_rejected() {
        let root = root();
        let paths = paths(root.path());
        fs::create_dir_all(paths.runtime_directory()).expect("runtime");
        let socket = paths.socket_path().expect("socket");
        let listener = UnixListener::bind(&socket).expect("bind");
        let foreign = if paths.owner() == u32::MAX {
            0
        } else {
            paths.owner() + 1
        };
        let error = Endpoint::claim(&socket, foreign).err().expect("socket owner mismatch");
        assert!(matches!(error, ServerError::OwnerMismatch { .. }), "{error}");
        drop(listener);
    }

    #[test]
    fn shutdown_removes_only_the_owned_endpoint() {
        let root = root();
        let paths = paths(root.path());
        let server = IpcServer::start(&paths, router()).expect("server");
        let socket = server.socket_path().to_path_buf();
        fs::remove_file(&socket).expect("unlink owned");
        let foreign = UnixListener::bind(&socket).expect("foreign bind");
        drop(server);
        assert!(socket.exists(), "a foreign endpoint must not be removed");
        drop(foreign);
        let _ = fs::remove_file(&socket);
    }

    #[test]
    fn shutdown_closes_the_endpoint_and_allows_a_restart() {
        let root = root();
        let paths = paths(root.path());
        let mut server = IpcServer::start(&paths, router()).expect("server");
        let socket = server.socket_path().to_path_buf();
        server.shutdown();
        assert!(!socket.exists());
        let restarted = IpcServer::start(&paths, router()).expect("restart");
        assert!(restarted.socket_path().exists());
    }

    #[test]
    fn concurrency_is_capped_at_the_configured_limit() {
        let root = root();
        let paths = paths(root.path());
        let slow = Arc::new(Slow::new());
        let mut router = Router::new(Arc::new(NoCore));
        router.register("slow", slow.clone()).expect("register");
        let config = ServerConfig {
            max_connections: 1,
            ..ServerConfig::default()
        };
        let _server = IpcServer::start_with(&paths, Arc::new(router), config).expect("server");
        let socket = paths.socket_path().expect("socket");
        let clients: Vec<_> = (0..3)
            .map(|_| {
                let socket = socket.clone();
                thread::spawn(move || {
                    exchange(&socket, r#"{"module":"slow","method":"run","params":null}"#)
                })
            })
            .collect();
        slow.wait_for_entry(2, Duration::from_millis(200));
        slow.release();
        for client in clients {
            let response = client.join().expect("client");
            assert!(response.contains("done"), "{response}");
        }
        assert_eq!(slow.max_seen(), 1, "the server accepted more than its limit");
    }

    #[test]
    fn an_empty_request_is_reported() {
        // A handler response is serialized once; a request with no newline and
        // an empty payload is reported as an empty request.
        let root = root();
        let paths = paths(root.path());
        let _server = IpcServer::start(&paths, router()).expect("server");
        let socket = paths.socket_path().expect("socket");
        let mut stream = connect(&socket);
        writeln!(stream).expect("empty line");
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).expect("read");
        assert!(response.contains("empty request"), "{response}");
    }

    #[test]
    fn the_gate_blocks_until_a_slot_frees() {
        let gate = Arc::new(Gate::new(1));
        assert!(gate.acquire());
        let blocked = Arc::clone(&gate);
        let (sender, receiver) = std::sync::mpsc::channel();
        let waiter = thread::spawn(move || {
            let acquired = blocked.acquire();
            let _ = sender.send(acquired);
        });
        assert!(
            receiver.recv_timeout(Duration::from_millis(50)).is_err(),
            "a second acquire must wait"
        );
        gate.release();
        assert!(receiver.recv_timeout(Duration::from_secs(1)).expect("acquired"));
        waiter.join().expect("waiter");
        gate.release();
    }

    #[test]
    fn stopping_the_gate_wakes_waiters() {
        let gate = Arc::new(Gate::new(1));
        assert!(gate.acquire());
        let blocked = Arc::clone(&gate);
        let (sender, receiver) = std::sync::mpsc::channel();
        let waiter = thread::spawn(move || {
            let _ = sender.send(blocked.acquire());
        });
        gate.stop();
        assert!(!receiver.recv_timeout(Duration::from_secs(1)).expect("released"));
        waiter.join().expect("waiter");
    }

    #[test]
    fn request_limit_counts_bytes_before_the_newline() {
        let root = root();
        let paths = paths(root.path());
        let config = ServerConfig {
            request_limit: 16,
            ..ServerConfig::default()
        };
        let _server = IpcServer::start_with(&paths, router(), config).expect("server");
        let socket = paths.socket_path().expect("socket");
        let response = exchange(&socket, &"B".repeat(17));
        assert!(response.contains("request exceeds 16 bytes"), "{response}");
    }

    #[test]
    fn sequential_connections_share_the_endpoint() {
        let root = root();
        let paths = paths(root.path());
        let _server = IpcServer::start(&paths, router()).expect("server");
        let socket = paths.socket_path().expect("socket");
        let requests = Arc::new(AtomicUsize::new(0));
        for _ in 0..5 {
            let response = exchange(&socket, r#"{"module":"demo","method":"echo","params":null}"#);
            assert!(response.contains("\"status\":\"ok\""), "{response}");
            requests.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(requests.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn stopping_dispatch_refuses_late_work_and_finishes_an_accepted_connection() {
        let root = root();
        let paths = paths(root.path());
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let mut builder = Router::new(Arc::new(NoCore));
        builder
            .register(
                "holds",
                Arc::new(Holds {
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                }),
            )
            .expect("register");
        let server = IpcServer::start_with(&paths, Arc::new(builder), ServerConfig {
            max_connections: 4,
            ..ServerConfig::default()
        })
        .expect("server");
        let socket = server.socket_path().to_path_buf();

        let accepted = {
            let socket = socket.clone();
            thread::spawn(move || {
                exchange(&socket, r#"{"module":"holds","method":"hold","params":null}"#)
            })
        };
        entered_rx.recv_timeout(Duration::from_secs(5)).expect("handler entered");

        // Accepted before the latch, but its newline arrives afterwards, so it
        // is read and routed while dispatch is already stopped.
        let mut late = connect(&socket);
        late.write_all(br#"{"module":"holds","method":"late","params":null}"#)
            .expect("partial write");
        wait_for_workers(&server, 2);

        server.stop_accepting();

        release_tx.send(()).expect("release the held handler");
        let response = accepted.join().expect("accepted connection");
        assert!(response.contains("\"status\":\"ok\""), "{response}");

        writeln!(late).expect("finish the request");
        let mut refusal = String::new();
        BufReader::new(late).read_line(&mut refusal).expect("refusal");
        assert!(refusal.contains("\"kind\":\"unavailable\""), "{refusal}");
        assert!(refusal.contains("shutting down"), "{refusal}");

        wait_for_refusal(&socket);
    }

    #[test]
    fn bounded_shutdown_reports_the_accept_thread_and_removes_the_endpoint() {
        let root = root();
        let paths = paths(root.path());
        let server = IpcServer::start(&paths, router()).expect("server");
        let socket = server.socket_path().to_path_buf();
        let wait = crate::shutdown::testing::FakeShutdownWait::with_budget(Duration::from_secs(2));
        assert_eq!(server.shutdown_bounded(&wait), SocketShutdown::Acknowledged);
        assert!(!socket.exists(), "the owned endpoint must be removed");
    }

    #[test]
    fn a_hung_accept_thread_is_abandoned_and_the_endpoint_is_still_removed() {
        let root = root();
        let paths = paths(root.path());
        let mut server = IpcServer::start(&paths, router()).expect("server");
        let socket = server.socket_path().to_path_buf();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let real = server.worker.replace(thread::spawn(move || {
            let _ = released.recv();
        }));

        let wait = crate::shutdown::testing::FakeShutdownWait::with_budget(Duration::from_secs(2));
        wait.expire_now();
        assert_eq!(
            server.shutdown_bounded(&wait),
            SocketShutdown::Unacknowledged,
            "a hung accept thread is detached"
        );
        assert!(!socket.exists(), "the owned endpoint must still be removed");

        drop(release);
        real.expect("the real accept thread").join().expect("accept thread stops");
    }

    #[test]
    fn a_failed_owned_inode_removal_is_reported() {
        let root = root();
        let paths = paths(root.path());
        let server = IpcServer::start(&paths, router()).expect("server");
        let socket = server.socket_path().to_path_buf();
        let directory = paths.runtime_directory().to_path_buf();
        // The socket is owned, but its directory refuses the unlink.
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o500))
            .expect("restrict the runtime directory");
        let wait = crate::shutdown::testing::FakeShutdownWait::with_budget(Duration::from_secs(2));
        let result = server.shutdown_bounded(&wait);
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .expect("restore the runtime directory");
        assert!(matches!(result, SocketShutdown::RemovalFailed(_)), "{result:?}");
        assert!(
            socket.exists(),
            "a failed removal leaves the inode for the next start"
        );
        fs::remove_file(&socket).expect("clean up the endpoint");
    }

    #[test]
    fn a_foreign_inode_at_the_owned_path_is_a_successful_removal() {
        let root = root();
        let paths = paths(root.path());
        let server = IpcServer::start(&paths, router()).expect("server");
        let socket = server.socket_path().to_path_buf();
        fs::remove_file(&socket).expect("unlink owned");
        let foreign = UnixListener::bind(&socket).expect("foreign bind");
        let wait = crate::shutdown::testing::FakeShutdownWait::with_budget(Duration::from_secs(2));
        assert_eq!(server.shutdown_bounded(&wait), SocketShutdown::Acknowledged);
        assert!(socket.exists(), "a foreign endpoint must not be removed");
        drop(foreign);
        let _ = fs::remove_file(&socket);
    }

    #[test]
    fn a_hung_handler_makes_the_bounded_shutdown_unacknowledged() {
        let root = root();
        let paths = paths(root.path());
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let mut builder = Router::new(Arc::new(NoCore));
        builder
            .register(
                "holds",
                Arc::new(Holds {
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                }),
            )
            .expect("register");
        let server = IpcServer::start_with(&paths, Arc::new(builder), ServerConfig::default())
            .expect("server");
        let socket = server.socket_path().to_path_buf();

        let client = {
            let socket = socket.clone();
            thread::spawn(move || {
                exchange(&socket, r#"{"module":"holds","method":"hold","params":null}"#)
            })
        };
        entered_rx.recv_timeout(Duration::from_secs(5)).expect("handler entered");

        let wait = crate::shutdown::testing::FakeShutdownWait::with_budget(Duration::from_secs(2));
        wait.expire_now();
        assert_eq!(
            server.shutdown_bounded(&wait),
            SocketShutdown::Unacknowledged,
            "a held permit outlives the bound"
        );
        assert!(!socket.exists(), "the owned endpoint is still removed");

        release_tx.send(()).expect("release the held handler");
        let response = client.join().expect("client");
        assert!(response.contains("\"status\":\"ok\""), "{response}");
    }

    #[test]
    fn a_held_connection_does_not_delay_endpoint_removal() {
        let root = root();
        let paths = paths(root.path());
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let mut builder = Router::new(Arc::new(NoCore));
        builder
            .register(
                "holds",
                Arc::new(Holds {
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                }),
            )
            .expect("register");
        let server = IpcServer::start_with(&paths, Arc::new(builder), ServerConfig::default())
            .expect("server");
        let socket = server.socket_path().to_path_buf();
        let foreign = paths.runtime_directory().join("foreign");
        fs::write(&foreign, "keep").expect("foreign inode");

        let client = {
            let socket = socket.clone();
            thread::spawn(move || {
                exchange(&socket, r#"{"module":"holds","method":"hold","params":null}"#)
            })
        };
        entered_rx.recv_timeout(Duration::from_secs(5)).expect("handler entered");

        let wait = crate::shutdown::testing::FakeShutdownWait::with_budget(Duration::from_secs(2));
        wait.expire_now();
        server.shutdown_bounded(&wait);

        assert!(
            !socket.exists(),
            "the owned endpoint must be removed while a handler is held"
        );
        assert!(foreign.exists(), "a foreign inode must survive");
        assert_eq!(fs::read_to_string(&foreign).expect("foreign contents"), "keep");

        release_tx.send(()).expect("release the held handler");
        let response = client.join().expect("client");
        assert!(response.contains("\"status\":\"ok\""), "{response}");
    }
}
