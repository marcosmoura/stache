//! Direct-process command execution shared by startup and module runtimes.
//!
//! Commands are split with the legacy quoting rules and spawned directly: there
//! is no shell, no expansion, no pipe or redirect. Two modes share the parser,
//! the executable resolver, the retained-child ownership rules and the
//! cancellation-aware wait loop:
//!
//! * the **inherited** sequential mode ([`CommandExecutor::run_sequence`])
//!   keeps the caller's working directory, environment and stdio, waits for
//!   each step and stops at the first failure;
//! * the **captured** mode ([`CommandExecutor::run_captured`]) bounds the
//!   deadline and each output stream and reports typed overflow and timeout
//!   outcomes instead of truncated successes.
//!
//! # Ownership
//!
//! Only the directly spawned child is ever signalled. Cancellation kills it and
//! polls for [`CANCEL_CLEANUP`]; a child that is still unreaped is transferred,
//! handle included, to the executor's own bounded reaper, which keeps its slot
//! until the child is confirmed gone. The executor starts no thread: the caller
//! owns dispatch capacity, and retained children are swept whenever the executor
//! is used again or [`CommandExecutor::sweep`] is called. Returning a retained
//! outcome never proves termination.

use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{fmt, io, thread};

use crate::lock;
use crate::threading::CancellationToken;

#[cfg(test)]
mod tests;

/// Longest interval between cancellation and exit checks.
pub const POLL_QUANTUM: Duration = Duration::from_millis(10);
/// How long cancellation cleanup polls a direct child before the handle moves
/// to the owned reaper.
pub const CANCEL_CLEANUP: Duration = Duration::from_secs(2);
/// Retained-child slots reserved by one executor instance.
pub const RETAINED_SLOTS: usize = 64;
/// The most bytes a caller may allow for one captured stream (1 MiB).
pub const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
/// The hidden root flag that selects CLI mode for a re-entered Stache binary.
pub const INTERNAL_CLI_FLAG: &str = "--stache-internal-cli";
/// The one bare executable name that resolves to the running Stache binary.
pub const STACHE_EXECUTABLE: &str = "stache";

const READ_CHUNK: usize = 4096;

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Why a command line could not be split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandParseError {
    /// A quote was opened and never closed.
    UnterminatedQuote(char),
    /// A backslash ended the command.
    TrailingEscape,
}

impl fmt::Display for CommandParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnterminatedQuote(quote) => write!(f, "unterminated {quote} quote"),
            Self::TrailingEscape => f.write_str("trailing escape character"),
        }
    }
}

impl std::error::Error for CommandParseError {}

/// Splits one command into arguments using the legacy quoting rules.
///
/// An empty invocation yields no parts, while an empty quoted argument yields
/// an empty part.
///
/// # Errors
/// Returns a parse error for an unterminated quote or trailing escape.
pub fn split_command(command: &str) -> Result<Vec<String>, CommandParseError> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut has_token = false;

    for character in command.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            has_token = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            } else if active_quote == '"' && character == '\\' {
                escaped = true;
            } else {
                current.push(character);
            }
            has_token = true;
            continue;
        }
        match character {
            '\'' | '"' => {
                quote = Some(character);
                has_token = true;
            }
            '\\' => {
                escaped = true;
                has_token = true;
            }
            character if character.is_whitespace() => {
                if has_token {
                    parts.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            _ => {
                current.push(character);
                has_token = true;
            }
        }
    }

    if escaped {
        return Err(CommandParseError::TrailingEscape);
    }
    if let Some(active_quote) = quote {
        return Err(CommandParseError::UnterminatedQuote(active_quote));
    }
    if has_token {
        parts.push(current);
    }
    Ok(parts)
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// The injected environment that resolution reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandEnvironment {
    /// `STACHE_EXTRA_PATHS`, searched first.
    pub extra_paths: Option<OsString>,
    /// `PATH`, searched second.
    pub path: Option<OsString>,
    /// `HOME`, used for `~/.cargo/bin` and `~/.local/bin`.
    pub home: Option<PathBuf>,
    /// The running Stache executable that bare `stache` resolves to.
    pub next_executable: Option<PathBuf>,
}

impl CommandEnvironment {
    /// Reads the process environment and the running executable once.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            extra_paths: std::env::var_os("STACHE_EXTRA_PATHS"),
            path: std::env::var_os("PATH"),
            home: std::env::var_os("HOME").map(PathBuf::from),
            next_executable: std::env::current_exe().ok(),
        }
    }

    /// The directories searched for a non-absolute name, in order.
    ///
    /// Empty components of `STACHE_EXTRA_PATHS` and `PATH` are preserved as the
    /// empty path, exactly as `std::env::split_paths` yields them; joining a
    /// name onto one searches the current directory.
    #[must_use]
    pub fn search_directories(&self) -> Vec<PathBuf> {
        let mut directories: Vec<PathBuf> = Vec::new();
        if let Some(extra) = &self.extra_paths {
            directories.extend(std::env::split_paths(extra));
        }
        if let Some(path) = &self.path {
            directories.extend(std::env::split_paths(path));
        }
        directories.extend([
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/opt/homebrew/sbin"),
        ]);
        if let Some(home) = &self.home {
            directories.push(home.join(".cargo/bin"));
            directories.push(home.join(".local/bin"));
        }
        directories
    }
}

/// Resolves an absolute or searched executable name.
///
/// The exact name `stache` resolves only to the injected running executable and
/// never searches `PATH` or any legacy location.
///
/// # Errors
/// Returns a user-facing reason when the name is empty, not executable or not
/// found.
pub fn resolve_binary(binary: &str, environment: &CommandEnvironment) -> Result<PathBuf, String> {
    if binary.is_empty() {
        return Err("binary name cannot be empty".to_owned());
    }
    if binary == STACHE_EXECUTABLE {
        return match &environment.next_executable {
            Some(path) if is_executable(path) => Ok(path.clone()),
            Some(path) => Err(format!("{} is not executable", path.display())),
            None => Err("the running Stache executable is unavailable".to_owned()),
        };
    }
    let candidate = Path::new(binary);
    if candidate.is_absolute() {
        return if is_executable(candidate) {
            Ok(candidate.to_path_buf())
        } else {
            Err(format!("{} is not executable", candidate.display()))
        };
    }
    for directory in environment.search_directories() {
        let candidate = directory.join(binary);
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!("`{binary}` was not found on PATH"))
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// A parsed, resolved command ready to spawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedCommand {
    /// The executable to spawn.
    pub program: PathBuf,
    /// Arguments after the program; the OS supplies `argv[0]`.
    pub arguments: Vec<String>,
}

/// Parses and resolves one command.
///
/// For the exact parsed name `stache` the program is the running executable and
/// the arguments are `[--stache-internal-cli, ...original arguments]`; every
/// original argument keeps its order, including empty ones.
///
/// # Errors
/// Returns the typed parse, empty or resolution failure.
pub fn prepare(
    command: &str,
    environment: &CommandEnvironment,
) -> Result<PreparedCommand, StepFailure> {
    let parts = split_command(command).map_err(StepFailure::Parse)?;
    let Some((binary, arguments)) = parts.split_first() else {
        return Err(StepFailure::Empty);
    };
    let program = resolve_binary(binary, environment).map_err(StepFailure::Resolve)?;
    let mut rewritten = Vec::with_capacity(arguments.len() + 1);
    if binary == STACHE_EXECUTABLE {
        rewritten.push(INTERNAL_CLI_FLAG.to_owned());
    }
    rewritten.extend(arguments.iter().cloned());
    Ok(PreparedCommand { program, arguments: rewritten })
}

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

/// Why one command step failed.
#[derive(Debug)]
pub enum StepFailure {
    /// The command line could not be split.
    Parse(CommandParseError),
    /// The command had no executable.
    Empty,
    /// The executable could not be resolved.
    Resolve(String),
    /// All retained-child slots are in use, so no child was spawned.
    Capacity,
    /// The child could not be spawned.
    Spawn(String),
    /// The child exited unsuccessfully.
    Exit(ExitStatus),
    /// The child could not be waited for or its output could not be read.
    Wait(String),
}

impl fmt::Display for StepFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => error.fmt(f),
            Self::Empty => f.write_str("command is empty"),
            Self::Resolve(reason) => f.write_str(reason),
            Self::Capacity => write!(f, "all {RETAINED_SLOTS} retained-child slots are in use"),
            Self::Spawn(error) => write!(f, "could not run: {error}"),
            Self::Exit(status) => write!(f, "exited with {status}"),
            Self::Wait(error) => write!(f, "could not be waited for: {error}"),
        }
    }
}

/// What happened to a child that cancellation, a deadline or an error stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cleanup {
    /// The child was killed and confirmed reaped.
    Reaped,
    /// The child may still be running; its handle is owned by the reaper.
    Retained(String),
}

/// How an inherited-stdio sequence ended.
#[derive(Debug)]
pub enum SequenceOutcome {
    /// Every step exited successfully.
    Completed,
    /// Cancellation stopped the sequence. `Some` when a child had been spawned.
    Cancelled(Option<Cleanup>),
    /// One step failed; later steps never ran.
    Failed {
        /// Zero-based failing step.
        index: usize,
        /// Number of steps in the sequence.
        total: usize,
        /// What failed.
        failure: StepFailure,
        /// Cleanup of the failing child, when one needed it.
        cleanup: Option<Cleanup>,
    },
}

/// Which captured stream a limit applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl fmt::Display for CaptureStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

/// A rejected capture limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitError {
    /// The deadline was zero.
    ZeroDeadline,
    /// A stream cap exceeded [`MAX_CAPTURE_BYTES`].
    CapTooLarge(CaptureStream),
}

impl fmt::Display for LimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDeadline => f.write_str("the capture deadline must be positive"),
            Self::CapTooLarge(stream) => {
                write!(f, "the {stream} cap exceeds {MAX_CAPTURE_BYTES} bytes")
            }
        }
    }
}

impl std::error::Error for LimitError {}

/// The deadline and separate output caps of one captured call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureLimits {
    deadline: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
}

impl CaptureLimits {
    /// Validates the caller's limits.
    ///
    /// # Errors
    /// Returns [`LimitError`] for a zero deadline or a cap above
    /// [`MAX_CAPTURE_BYTES`].
    pub const fn new(
        deadline: Duration,
        stdout_cap: usize,
        stderr_cap: usize,
    ) -> Result<Self, LimitError> {
        if deadline.is_zero() {
            return Err(LimitError::ZeroDeadline);
        }
        if stdout_cap > MAX_CAPTURE_BYTES {
            return Err(LimitError::CapTooLarge(CaptureStream::Stdout));
        }
        if stderr_cap > MAX_CAPTURE_BYTES {
            return Err(LimitError::CapTooLarge(CaptureStream::Stderr));
        }
        Ok(Self {
            deadline,
            stdout_cap,
            stderr_cap,
        })
    }

    /// The caller's deadline.
    #[must_use]
    pub const fn deadline(&self) -> Duration { self.deadline }
}

/// Complete output of a child that exited inside its limits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedOutput {
    /// The exit status; a nonzero status is still a completed capture.
    pub status: ExitStatus,
    /// Everything the child wrote to standard output before it exited.
    pub stdout: Vec<u8>,
    /// Everything the child wrote to standard error before it exited.
    pub stderr: Vec<u8>,
}

/// How a captured call ended.
#[derive(Debug)]
pub enum CaptureOutcome {
    /// The child exited inside its deadline and caps.
    Completed(CapturedOutput),
    /// A stream exceeded its cap; no partial output is returned.
    Overflow {
        /// The stream that overflowed.
        stream: CaptureStream,
        /// Cleanup of the child.
        cleanup: Cleanup,
    },
    /// The deadline elapsed. The command may already have acted.
    TimedOut {
        /// Cleanup of the child.
        cleanup: Cleanup,
    },
    /// Cancellation stopped the call. `Some` when a child had been spawned.
    Cancelled(Option<Cleanup>),
    /// The call failed before or while running.
    Failed {
        /// What failed.
        failure: StepFailure,
        /// Cleanup of the child, when one needed it.
        cleanup: Option<Cleanup>,
    },
}

// ---------------------------------------------------------------------------
// Injection points
// ---------------------------------------------------------------------------

/// How a child's stdio is wired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdioMode {
    /// Inherit stdin, stdout and stderr; the legacy mode.
    Inherit,
    /// Null stdin and piped, nonblocking stdout and stderr.
    Capture,
}

/// Everything needed to spawn one child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnSpec {
    /// The resolved executable.
    pub program: PathBuf,
    /// Arguments after the program.
    pub arguments: Vec<String>,
    /// Stdio wiring.
    pub stdio: StdioMode,
}

/// A directly owned child process.
pub trait ChildProcess: Send {
    /// Polls for exit without blocking.
    ///
    /// # Errors
    /// Returns the OS failure.
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;

    /// Kills only this child.
    ///
    /// # Errors
    /// Returns the OS failure.
    fn kill(&mut self) -> io::Result<()>;

    /// Reads available standard output without blocking.
    ///
    /// `Ok(0)` is end of stream and [`io::ErrorKind::WouldBlock`] means no data
    /// is ready.
    ///
    /// # Errors
    /// Returns the read failure.
    fn read_stdout(&mut self, buffer: &mut [u8]) -> io::Result<usize>;

    /// Reads available standard error; see [`ChildProcess::read_stdout`].
    ///
    /// # Errors
    /// Returns the read failure.
    fn read_stderr(&mut self, buffer: &mut [u8]) -> io::Result<usize>;
}

/// Creates child processes.
pub trait ProcessFactory: Send + Sync {
    /// Spawns one child.
    ///
    /// # Errors
    /// Returns the spawn failure.
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>>;
}

/// Time and waiting, injectable so tests need no real delay.
pub trait Clock: Send + Sync {
    /// A monotonic offset.
    fn now(&self) -> Duration;

    /// Waits at most `quantum`, waking early on cancellation.
    ///
    /// Returns `true` when `cancel` was cancelled. With no token this is a plain
    /// bounded sleep.
    fn wait(&self, cancel: Option<&CancellationToken>, quantum: Duration) -> bool;
}

/// The real monotonic clock.
#[derive(Debug)]
pub struct SystemClock {
    start: Instant,
}

impl Default for SystemClock {
    fn default() -> Self { Self { start: Instant::now() } }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration { self.start.elapsed() }

    fn wait(&self, cancel: Option<&CancellationToken>, quantum: Duration) -> bool {
        cancel.map_or_else(
            || {
                thread::sleep(quantum);
                false
            },
            |token| token.wait(quantum),
        )
    }
}

/// The last check before a child is created.
///
/// A gate rechecks the accepted generation, revision and cancellation at the
/// moment of process creation, not merely when the work was dequeued. The
/// returned permit is held across the spawn, so a gate may keep a lock that
/// serializes revision changes against process creation.
pub trait SpawnGate: Send + Sync {
    /// Returns a permit to spawn now, or `None` when the work is stale.
    fn admit(&self) -> Option<Box<dyn Send + '_>>;
}

impl SpawnGate for CancellationToken {
    fn admit(&self) -> Option<Box<dyn Send + '_>> {
        (!self.is_cancelled()).then(|| Box::new(()) as Box<dyn Send>)
    }
}

/// Spawns real processes with `std::process`.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsProcessFactory;

struct OsChild {
    child: Child,
    stdout: Option<OwnedFd>,
    stderr: Option<OwnedFd>,
}

fn nonblocking(fd: impl Into<OwnedFd>) -> io::Result<OwnedFd> {
    let fd = fd.into();
    let flags = rustix::fs::fcntl_getfl(&fd)?;
    rustix::fs::fcntl_setfl(&fd, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(fd)
}

fn read_fd(fd: Option<&OwnedFd>, buffer: &mut [u8]) -> io::Result<usize> {
    match fd {
        Some(fd) => Ok(rustix::io::read(fd, buffer)?),
        None => Ok(0),
    }
}

impl ProcessFactory for OsProcessFactory {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>> {
        let mut command = Command::new(&spec.program);
        command.args(&spec.arguments);
        if spec.stdio == StdioMode::Capture {
            command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        let mut child = command.spawn()?;
        let streams = (|| -> io::Result<_> {
            let stdout = child.stdout.take().map(nonblocking).transpose()?;
            let stderr = child.stderr.take().map(nonblocking).transpose()?;
            Ok((stdout, stderr))
        })();
        match streams {
            Ok((stdout, stderr)) => Ok(Box::new(OsChild { child, stdout, stderr })),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }
}

impl ChildProcess for OsChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> { self.child.try_wait() }

    fn kill(&mut self) -> io::Result<()> { self.child.kill() }

    fn read_stdout(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        read_fd(self.stdout.as_ref(), buffer)
    }

    fn read_stderr(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        read_fd(self.stderr.as_ref(), buffer)
    }
}

// ---------------------------------------------------------------------------
// Reaper
// ---------------------------------------------------------------------------

/// A child the executor could not confirm gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedChild {
    /// The executable that was spawned.
    pub program: PathBuf,
    /// How long the handle has been retained.
    pub retained_for: Duration,
}

struct Retained {
    child: Box<dyn ChildProcess>,
    program: PathBuf,
    since: Duration,
}

#[derive(Default)]
struct ReaperState {
    /// Slots in use: live reservations plus retained children.
    used: usize,
    retained: Vec<Retained>,
}

/// A slot held for the lifetime of one child.
struct Reservation {
    reaper: Arc<Mutex<ReaperState>>,
    armed: bool,
}

impl Reservation {
    /// Gives the slot to the reaper together with the child.
    fn retain(mut self, child: Box<dyn ChildProcess>, program: PathBuf, since: Duration) {
        self.armed = false;
        lock(&self.reaper).retained.push(Retained { child, program, since });
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.armed {
            let mut state = lock(&self.reaper);
            state.used = state.used.saturating_sub(1);
        }
    }
}

struct Running {
    child: Box<dyn ChildProcess>,
    reservation: Reservation,
    program: PathBuf,
}

enum Launch {
    Cancelled,
    Failed(StepFailure),
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

type Diagnostics = Arc<dyn Fn(&str) + Send + Sync>;

/// Runs commands for one owner (the startup runner or one module runtime).
///
/// The executor owns its [`RETAINED_SLOTS`] retained-child slots; two executors
/// never share a reaper.
pub struct CommandExecutor {
    factory: Arc<dyn ProcessFactory>,
    clock: Arc<dyn Clock>,
    environment: CommandEnvironment,
    state: Arc<Mutex<ReaperState>>,
    diagnostics: Option<Diagnostics>,
}

impl fmt::Debug for CommandExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandExecutor")
            .field("environment", &self.environment)
            .field("slots_in_use", &lock(&self.state).used)
            .finish_non_exhaustive()
    }
}

impl CommandExecutor {
    /// An executor over real processes and the real clock.
    #[must_use]
    pub fn new(environment: CommandEnvironment) -> Self {
        Self::with_parts(
            Arc::new(OsProcessFactory),
            Arc::new(SystemClock::default()),
            environment,
        )
    }

    /// An executor over an injected factory and clock.
    #[must_use]
    pub fn with_parts(
        factory: Arc<dyn ProcessFactory>,
        clock: Arc<dyn Clock>,
        environment: CommandEnvironment,
    ) -> Self {
        Self {
            factory,
            clock,
            environment,
            state: Arc::new(Mutex::new(ReaperState::default())),
            diagnostics: None,
        }
    }

    /// Reports retained-child diagnostics to `sink`.
    #[must_use]
    pub fn with_diagnostics(mut self, sink: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.diagnostics = Some(Arc::new(sink));
        self
    }

    /// The environment resolution reads.
    #[must_use]
    pub const fn environment(&self) -> &CommandEnvironment { &self.environment }

    /// Polls every retained child once and releases the slots of reaped ones.
    ///
    /// Returns the children still retained. A child whose kill failed earlier is
    /// signalled again.
    #[must_use]
    pub fn sweep(&self) -> Vec<RetainedChild> {
        let now = self.clock.now();
        let mut state = lock(&self.state);
        let mut released = 0;
        state.retained.retain_mut(|entry| {
            if matches!(entry.child.try_wait(), Ok(Some(_))) {
                released += 1;
                false
            } else {
                let _ = entry.child.kill();
                true
            }
        });
        state.used = state.used.saturating_sub(released);
        state
            .retained
            .iter()
            .map(|entry| RetainedChild {
                program: entry.program.clone(),
                retained_for: now.saturating_sub(entry.since),
            })
            .collect()
    }

    /// Runs inherited-stdio commands in order, stopping at the first failure.
    ///
    /// The caller normalizes blank commands. Cancellation is checked before each
    /// step's resolution, again at process creation through `gate`, while
    /// waiting and between steps.
    pub fn run_sequence(
        &self,
        commands: &[String],
        cancel: &CancellationToken,
        gate: &dyn SpawnGate,
    ) -> SequenceOutcome {
        let total = commands.len();
        for (index, command) in commands.iter().enumerate() {
            if cancel.is_cancelled() {
                return SequenceOutcome::Cancelled(None);
            }
            let failed =
                |failure, cleanup| SequenceOutcome::Failed { index, total, failure, cleanup };
            let prepared = match prepare(command, &self.environment) {
                Ok(prepared) => prepared,
                Err(failure) => return failed(failure, None),
            };
            let mut running = match self.launch(&prepared, StdioMode::Inherit, gate) {
                Ok(running) => running,
                Err(Launch::Cancelled) => return SequenceOutcome::Cancelled(None),
                Err(Launch::Failed(failure)) => return failed(failure, None),
            };
            loop {
                if cancel.is_cancelled() {
                    return SequenceOutcome::Cancelled(Some(self.terminate(running)));
                }
                match running.child.try_wait() {
                    Ok(Some(status)) => {
                        drop(running);
                        if status.success() {
                            break;
                        }
                        return failed(StepFailure::Exit(status), None);
                    }
                    Ok(None) => {
                        self.clock.wait(Some(cancel), POLL_QUANTUM);
                    }
                    Err(error) => {
                        let cleanup = self.terminate(running);
                        return failed(StepFailure::Wait(error.to_string()), Some(cleanup));
                    }
                }
            }
        }
        SequenceOutcome::Completed
    }

    /// Parses, resolves and runs one command in captured mode.
    pub fn run_captured(
        &self,
        command: &str,
        limits: CaptureLimits,
        cancel: &CancellationToken,
        gate: &dyn SpawnGate,
    ) -> CaptureOutcome {
        match prepare(command, &self.environment) {
            Ok(prepared) => self.run_captured_prepared(&prepared, limits, cancel, gate),
            Err(failure) => CaptureOutcome::Failed { failure, cleanup: None },
        }
    }

    /// Runs an already prepared command in captured mode.
    ///
    /// Output is drained while the child runs. A timeout, overflow or
    /// cancellation kills only the direct child.
    pub fn run_captured_prepared(
        &self,
        prepared: &PreparedCommand,
        limits: CaptureLimits,
        cancel: &CancellationToken,
        gate: &dyn SpawnGate,
    ) -> CaptureOutcome {
        if cancel.is_cancelled() {
            return CaptureOutcome::Cancelled(None);
        }
        let mut running = match self.launch(prepared, StdioMode::Capture, gate) {
            Ok(running) => running,
            Err(Launch::Cancelled) => return CaptureOutcome::Cancelled(None),
            Err(Launch::Failed(failure)) => {
                return CaptureOutcome::Failed { failure, cleanup: None };
            }
        };
        let started = self.clock.now();
        let mut capture = Capture::default();
        loop {
            if cancel.is_cancelled() {
                return CaptureOutcome::Cancelled(Some(self.terminate(running)));
            }
            if let Err(error) = capture.drain(running.child.as_mut(), &limits) {
                return self.capture_stopped(running, error);
            }
            match running.child.try_wait() {
                Ok(Some(status)) => {
                    if let Err(error) = capture.drain(running.child.as_mut(), &limits) {
                        return self.capture_stopped(running, error);
                    }
                    drop(running);
                    return CaptureOutcome::Completed(CapturedOutput {
                        status,
                        stdout: capture.stdout,
                        stderr: capture.stderr,
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    return self.capture_stopped(running, DrainStop::Io(error.to_string()));
                }
            }
            if self.clock.now().saturating_sub(started) >= limits.deadline {
                return CaptureOutcome::TimedOut {
                    cleanup: self.terminate(running),
                };
            }
            self.clock.wait(Some(cancel), POLL_QUANTUM);
        }
    }

    fn capture_stopped(&self, running: Running, stop: DrainStop) -> CaptureOutcome {
        let cleanup = self.terminate(running);
        match stop {
            DrainStop::Overflow(stream) => CaptureOutcome::Overflow { stream, cleanup },
            DrainStop::Io(error) => CaptureOutcome::Failed {
                failure: StepFailure::Wait(error),
                cleanup: Some(cleanup),
            },
        }
    }

    /// Reserves a slot, passes the gate and spawns one child.
    fn launch(
        &self,
        prepared: &PreparedCommand,
        stdio: StdioMode,
        gate: &dyn SpawnGate,
    ) -> Result<Running, Launch> {
        let _ = self.sweep();
        let reservation = {
            let mut state = lock(&self.state);
            if state.used >= RETAINED_SLOTS {
                return Err(Launch::Failed(StepFailure::Capacity));
            }
            state.used += 1;
            Reservation {
                reaper: Arc::clone(&self.state),
                armed: true,
            }
        };
        let Some(permit) = gate.admit() else {
            return Err(Launch::Cancelled);
        };
        let spec = SpawnSpec {
            program: prepared.program.clone(),
            arguments: prepared.arguments.clone(),
            stdio,
        };
        let spawned = self.factory.spawn(&spec);
        drop(permit);
        match spawned {
            Ok(child) => Ok(Running {
                child,
                reservation,
                program: prepared.program.clone(),
            }),
            Err(error) => Err(Launch::Failed(StepFailure::Spawn(error.to_string()))),
        }
    }

    /// Kills the direct child, polls for [`CANCEL_CLEANUP`] and otherwise hands
    /// the exact handle to the reaper.
    fn terminate(&self, mut running: Running) -> Cleanup {
        let mut problems = Vec::new();
        if let Err(error) = running.child.kill() {
            problems.push(format!("kill failed: {error}"));
        }
        let started = self.clock.now();
        loop {
            match running.child.try_wait() {
                Ok(Some(_)) => return Cleanup::Reaped,
                Ok(None) => {}
                Err(error) => {
                    problems.push(format!("wait failed: {error}"));
                }
            }
            if self.clock.now().saturating_sub(started) >= CANCEL_CLEANUP {
                break;
            }
            self.clock.wait(None, POLL_QUANTUM);
        }
        let program = running.program.clone();
        let diagnostic = format!(
            "{} was not confirmed gone after {} ms; its handle is retained for reaping{}",
            program.display(),
            CANCEL_CLEANUP.as_millis(),
            if problems.is_empty() {
                String::new()
            } else {
                format!(" ({})", problems.join("; "))
            },
        );
        if let Some(sink) = &self.diagnostics {
            sink(&diagnostic);
        }
        let Running { child, reservation, .. } = running;
        reservation.retain(child, program, self.clock.now());
        Cleanup::Retained(diagnostic)
    }
}

enum DrainStop {
    Overflow(CaptureStream),
    Io(String),
}

#[derive(Default)]
struct Capture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_done: bool,
    stderr_done: bool,
}

impl Capture {
    /// Reads everything currently available from both streams.
    fn drain(
        &mut self,
        child: &mut dyn ChildProcess,
        limits: &CaptureLimits,
    ) -> Result<(), DrainStop> {
        let mut buffer = [0_u8; READ_CHUNK];
        for stream in [CaptureStream::Stdout, CaptureStream::Stderr] {
            let (target, done, cap) = match stream {
                CaptureStream::Stdout => {
                    (&mut self.stdout, &mut self.stdout_done, limits.stdout_cap)
                }
                CaptureStream::Stderr => {
                    (&mut self.stderr, &mut self.stderr_done, limits.stderr_cap)
                }
            };
            while !*done {
                let read = match stream {
                    CaptureStream::Stdout => child.read_stdout(&mut buffer),
                    CaptureStream::Stderr => child.read_stderr(&mut buffer),
                };
                match read {
                    Ok(0) => *done = true,
                    Ok(count) => {
                        target.extend_from_slice(&buffer[..count]);
                        if target.len() > cap {
                            return Err(DrainStop::Overflow(stream));
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(DrainStop::Io(error.to_string())),
                }
            }
        }
        Ok(())
    }
}
