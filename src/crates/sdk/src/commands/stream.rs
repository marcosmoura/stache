//! Bounded line streaming over one owned child.
//!
//! [`CommandExecutor::open_line_stream`] spawns one child with piped, nonblocking
//! stdout and stderr and no stdin, and returns a [`LineStream`] that the caller
//! pulls with [`LineStream::next_event`].
//!
//! # Bounds
//!
//! * A stdout line is assembled as raw bytes until `\n` and decoded once
//!   complete, so UTF-8 split across reads is never a problem. A line is
//!   accepted at up to `line_cap` bytes (excluding the newline); a longer line
//!   is never buffered beyond the cap, is reported once as
//!   [`StreamEvent::LineTooLong`] and is discarded up to the next newline.
//! * Invalid UTF-8 is a typed [`StreamEvent::InvalidUtf8`], never a panic.
//! * Stderr keeps only the last `stderr_tail` bytes (a ring). It is always
//!   drained so a noisy child never blocks on stderr, but at most
//!   [`STDERR_BUDGET`] bytes are drained per pull iteration, so a stderr flood
//!   cannot starve stdout events or cancellation.
//!
//! # Idle wakeups and cancel latency
//!
//! A stream can stay silent for hours, so an idle pull must not wake. When
//! stdout has no data a real child blocks in the OS with no timeout: `poll(2)`
//! on the child's stdout, its stderr and a private wake pipe (a self-pipe).
//! The first idle wait registers a [`CancellationToken::on_cancel`] listener
//! that writes one byte to that pipe, so cancellation ends the poll at once;
//! output and the child's exit (the pipes hang up) end it too. A silent stream
//! therefore wakes zero times, and the cancel latency of
//! [`LineStream::next_event`] is one wake-up, not a polling interval. If the
//! pipe cannot be created the wait falls back to a bounded poll that starts at
//! [`POLL_QUANTUM`] (10 ms) and doubles per fruitless wait up to
//! [`IDLE_CEILING`] (250 ms). Children without descriptors (test fakes) always
//! use [`Clock::wait`] with that schedule. [`LineStream::idle_wakeups`] counts
//! the waits that returned. [`LineStream::stop`] signals the child, whose exit
//! closes the pipes, and its grace loops stay bounded by the clock.
//!
//! # Overflow policy: the consumer's pace is the queue
//!
//! The executor starts no thread, so there is no producer queue to overflow.
//! The stream is pull-driven: stdout is read only while the consumer asks for
//! the next event, and the OS pipe is the bounded buffer. A slow consumer
//! therefore applies backpressure to the child (its writes block) instead of
//! losing lines or growing memory; at most one read chunk of decoded lines is
//! pending between pulls. Consumers that only care about the latest snapshot
//! coalesce on their side.
//!
//! # Ownership
//!
//! Only the exact child is signalled. [`LineStream::stop`] sends `SIGTERM` to
//! that pid, waits a bounded grace, sends `SIGKILL`, and reports truthfully
//! whether the child was reaped; an unreaped child is handed to the executor's
//! reaper with its slot. No process group is created: callers use this for
//! children that spawn no descendants, and signalling a group would add a
//! reuse hazard for no benefit there.

use std::collections::VecDeque;
use std::os::fd::{AsFd, OwnedFd};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;
use std::{fmt, io};

use super::{
    CANCEL_CLEANUP, Clock, CommandExecutor, Diagnostics, Launch, POLL_QUANTUM, PreparedCommand,
    Running, SpawnGate, StdioMode, StepFailure,
};
use crate::threading::{CancelListener, CancellationToken};

/// The default per-line cap (1 MiB).
pub const DEFAULT_LINE_CAP: usize = 1024 * 1024;
/// The largest per-line cap a caller may request (64 MiB).
pub const MAX_LINE_CAP: usize = 64 * 1024 * 1024;
/// The default retained stderr tail (8 KiB).
pub const STDERR_TAIL_BYTES: usize = 8 * 1024;
/// The largest retained stderr tail a caller may request (64 KiB).
pub const MAX_STDERR_TAIL: usize = 64 * 1024;
/// How long [`LineStream::stop`] waits after `SIGTERM` before `SIGKILL`.
pub const DEFAULT_TERM_GRACE: Duration = Duration::from_secs(2);

/// The longest bounded idle wait between reads. Only used by children without
/// descriptors (fakes) and when the wake pipe cannot be created.
pub const IDLE_CEILING: Duration = Duration::from_millis(250);

/// The next idle wait after `current`: doubled, capped at [`IDLE_CEILING`].
#[must_use]
pub fn next_idle_wait(current: Duration) -> Duration { current.saturating_mul(2).min(IDLE_CEILING) }

const STREAM_CHUNK: usize = 16 * 1024;
/// Stderr bytes drained per pull iteration.
const STDERR_BUDGET: usize = 4 * STREAM_CHUNK;

/// A rejected stream limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamLimitsError {
    /// The line cap was zero or above [`MAX_LINE_CAP`].
    LineCap,
    /// The stderr tail was above [`MAX_STDERR_TAIL`].
    StderrTail,
}

impl fmt::Display for StreamLimitsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LineCap => write!(f, "the line cap must be 1..={MAX_LINE_CAP} bytes"),
            Self::StderrTail => write!(f, "the stderr tail exceeds {MAX_STDERR_TAIL} bytes"),
        }
    }
}

impl std::error::Error for StreamLimitsError {}

/// The caps of one line stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamLimits {
    line_cap: usize,
    stderr_tail: usize,
    term_grace: Duration,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            line_cap: DEFAULT_LINE_CAP,
            stderr_tail: STDERR_TAIL_BYTES,
            term_grace: DEFAULT_TERM_GRACE,
        }
    }
}

impl StreamLimits {
    /// Validates the caller's limits.
    ///
    /// # Errors
    /// Returns [`StreamLimitsError`] for a zero or oversized line cap or an
    /// oversized stderr tail.
    pub const fn new(
        line_cap: usize,
        stderr_tail: usize,
        term_grace: Duration,
    ) -> Result<Self, StreamLimitsError> {
        if line_cap == 0 || line_cap > MAX_LINE_CAP {
            return Err(StreamLimitsError::LineCap);
        }
        if stderr_tail > MAX_STDERR_TAIL {
            return Err(StreamLimitsError::StderrTail);
        }
        Ok(Self {
            line_cap,
            stderr_tail,
            term_grace,
        })
    }

    /// The per-line byte cap.
    #[must_use]
    pub const fn line_cap(&self) -> usize { self.line_cap }
}

/// Why a stream could not be opened.
#[derive(Debug)]
pub enum StreamOpenError {
    /// Cancellation or the spawn gate stopped the open; no child exists.
    Cancelled,
    /// The child could not be started.
    Failed(StepFailure),
}

/// One thing that happened on a stream.
#[derive(Debug, PartialEq, Eq)]
pub enum StreamEvent {
    /// A complete line, without its newline, decoded as UTF-8.
    Line(String),
    /// A complete line was not valid UTF-8 and was dropped.
    InvalidUtf8 {
        /// Bytes in the dropped line.
        bytes: usize,
    },
    /// A line exceeded the cap; it is discarded up to the next newline.
    LineTooLong {
        /// The cap that was exceeded.
        limit: usize,
    },
    /// Stdout ended inside a line that had no newline; the bytes are dropped.
    Unterminated {
        /// Bytes in the dropped partial line.
        bytes: usize,
    },
    /// Stdout reached end of stream. `status` is set when the child had
    /// already exited. Further pulls repeat this event.
    Closed {
        /// The exit status when the child was already reaped.
        status: Option<ExitStatus>,
    },
    /// Reading stdout failed; further pulls repeat this event.
    ReadFailed(String),
    /// The cancellation token fired. The child is still owned: call
    /// [`LineStream::stop`].
    Cancelled,
}

/// How [`LineStream::stop`] ended, stating only what was observed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopOutcome {
    /// The child exited on its own or on `SIGTERM` and was reaped.
    Exited {
        /// The reaped status.
        status: ExitStatus,
    },
    /// The child ignored `SIGTERM`, was sent `SIGKILL` and was reaped.
    Killed {
        /// The reaped status.
        status: ExitStatus,
    },
    /// The child was not confirmed gone; its handle and slot are retained by
    /// the executor's reaper. A signal being sent does not prove termination.
    ReapTimeout {
        /// What was observed.
        diagnostic: String,
    },
}

enum LineItem {
    Line(String),
    InvalidUtf8(usize),
    TooLong(usize),
    Unterminated(usize),
}

impl From<LineItem> for StreamEvent {
    fn from(item: LineItem) -> Self {
        match item {
            LineItem::Line(line) => Self::Line(line),
            LineItem::InvalidUtf8(bytes) => Self::InvalidUtf8 { bytes },
            LineItem::TooLong(limit) => Self::LineTooLong { limit },
            LineItem::Unterminated(bytes) => Self::Unterminated { bytes },
        }
    }
}

/// Splits a byte stream into capped lines.
pub(super) struct LineAssembler {
    cap: usize,
    buffer: Vec<u8>,
    discarding: bool,
}

impl LineAssembler {
    pub(super) const fn new(cap: usize) -> Self {
        Self {
            cap,
            buffer: Vec::new(),
            discarding: false,
        }
    }

    fn push(&mut self, chunk: &[u8], out: &mut VecDeque<LineItem>) {
        let mut rest = chunk;
        while !rest.is_empty() {
            let newline = rest.iter().position(|byte| *byte == b'\n');
            let segment = &rest[..newline.unwrap_or(rest.len())];
            rest = newline.map_or(&[][..], |index| &rest[index + 1..]);
            if self.discarding {
                self.discarding = newline.is_none();
                continue;
            }
            if self.buffer.len() + segment.len() > self.cap {
                self.buffer = Vec::new();
                self.discarding = newline.is_none();
                out.push_back(LineItem::TooLong(self.cap));
                continue;
            }
            self.buffer.extend_from_slice(segment);
            if newline.is_some() {
                let bytes = std::mem::take(&mut self.buffer);
                let length = bytes.len();
                out.push_back(
                    String::from_utf8(bytes).map_or(LineItem::InvalidUtf8(length), LineItem::Line),
                );
            }
        }
    }

    fn finish(&mut self, out: &mut VecDeque<LineItem>) {
        if !self.discarding && !self.buffer.is_empty() {
            out.push_back(LineItem::Unterminated(self.buffer.len()));
        }
        self.buffer = Vec::new();
        self.discarding = false;
    }
}

/// The last bytes of stderr.
struct StderrTail {
    cap: usize,
    bytes: VecDeque<u8>,
    dropped: u64,
}

impl StderrTail {
    fn push(&mut self, data: &[u8]) {
        if self.cap == 0 {
            self.dropped += data.len() as u64;
            return;
        }
        let data = if data.len() > self.cap {
            self.dropped += (data.len() - self.cap) as u64;
            &data[data.len() - self.cap..]
        } else {
            data
        };
        let overflow = (self.bytes.len() + data.len()).saturating_sub(self.cap);
        self.bytes.drain(..overflow);
        self.dropped += overflow as u64;
        self.bytes.extend(data);
    }
}

/// A self-pipe that cancellation writes to, so a blocked `poll(2)` wakes.
struct CancelWaker {
    read: OwnedFd,
    token: CancellationToken,
    _listener: CancelListener,
}

impl CancelWaker {
    fn new(token: &CancellationToken) -> io::Result<Self> {
        use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
        use rustix::io::{FdFlags, fcntl_setfd};
        let (read, write) = rustix::pipe::pipe()?;
        for fd in [&read, &write] {
            fcntl_setfd(fd, FdFlags::CLOEXEC)?;
            fcntl_setfl(fd, fcntl_getfl(fd)? | OFlags::NONBLOCK)?;
        }
        let write = Arc::new(write);
        let listener = token.on_cancel(Arc::new(move || {
            // A full pipe already holds a wake byte, so a failed write is fine.
            let _ = rustix::io::write(&*write, &[1]);
        }));
        Ok(Self {
            read,
            token: token.clone(),
            _listener: listener,
        })
    }
}

/// One owned child whose stdout is read as bounded lines.
///
/// Dropping a stream that was not stopped kills the exact child and hands an
/// unreaped handle to the executor's reaper.
pub struct LineStream {
    child: Option<Running>,
    clock: Arc<dyn Clock>,
    diagnostics: Option<Diagnostics>,
    assembler: LineAssembler,
    pending: VecDeque<LineItem>,
    stderr: StderrTail,
    term_grace: Duration,
    stdout_done: bool,
    stderr_done: bool,
    idle_wait: Duration,
    waker: Option<CancelWaker>,
    wakeups: u64,
    failed: Option<String>,
    exited: Option<ExitStatus>,
}

impl fmt::Debug for LineStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LineStream")
            .field("stdout_done", &self.stdout_done)
            .field("exited", &self.exited)
            .finish_non_exhaustive()
    }
}

impl CommandExecutor {
    /// Spawns one child and returns its bounded line stream.
    ///
    /// The child has no stdin. Cancellation is checked first and the `gate` is
    /// checked at process creation, exactly as for captured runs.
    ///
    /// # Errors
    /// Returns [`StreamOpenError`] when cancelled, gated off or not startable.
    pub fn open_line_stream(
        &self,
        prepared: &PreparedCommand,
        limits: StreamLimits,
        cancel: &CancellationToken,
        gate: &dyn SpawnGate,
    ) -> Result<LineStream, StreamOpenError> {
        if cancel.is_cancelled() {
            return Err(StreamOpenError::Cancelled);
        }
        let running =
            self.launch(prepared, StdioMode::Capture, gate).map_err(|launch| match launch {
                Launch::Cancelled => StreamOpenError::Cancelled,
                Launch::Failed(failure) => StreamOpenError::Failed(failure),
            })?;
        Ok(LineStream {
            child: Some(running),
            clock: Arc::clone(&self.clock),
            diagnostics: self.diagnostics.clone(),
            assembler: LineAssembler::new(limits.line_cap),
            pending: VecDeque::new(),
            stderr: StderrTail {
                cap: limits.stderr_tail,
                bytes: VecDeque::new(),
                dropped: 0,
            },
            term_grace: limits.term_grace,
            stdout_done: false,
            stderr_done: false,
            idle_wait: POLL_QUANTUM,
            waker: None,
            wakeups: 0,
            failed: None,
            exited: None,
        })
    }
}

impl LineStream {
    /// Blocks until the next event, waking promptly on cancellation.
    ///
    /// Cancellation wins over queued lines so a stopping owner never processes
    /// stale data.
    pub fn next_event(&mut self, cancel: &CancellationToken) -> StreamEvent {
        loop {
            if cancel.is_cancelled() {
                return StreamEvent::Cancelled;
            }
            if let Some(item) = self.pending.pop_front() {
                return item.into();
            }
            if let Some(error) = &self.failed {
                return StreamEvent::ReadFailed(error.clone());
            }
            if self.stdout_done {
                return StreamEvent::Closed { status: self.poll_exit() };
            }
            if self.drain_stderr() {
                self.idle_wait = POLL_QUANTUM;
            }
            let Some(running) = self.child.as_mut() else {
                return StreamEvent::Closed { status: self.exited };
            };
            let mut buffer = [0_u8; STREAM_CHUNK];
            match running.child.read_stdout(&mut buffer) {
                Ok(0) => {
                    self.stdout_done = true;
                    self.assembler.finish(&mut self.pending);
                    self.drain_stderr();
                }
                Ok(count) => {
                    self.idle_wait = POLL_QUANTUM;
                    self.assembler.push(&buffer[..count], &mut self.pending);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let wait = self.idle_wait;
                    self.idle_wait = next_idle_wait(wait);
                    if self.waker.as_ref().is_none_or(|waker| !waker.token.same_as(cancel)) {
                        self.waker = CancelWaker::new(cancel).ok();
                    }
                    let wake = self.waker.as_ref().map(|waker| waker.read.as_fd());
                    // With a wake pipe the poll is unbounded: only output, hang-up or
                    // cancellation ends it. Without one it stays bounded.
                    let timeout = if wake.is_some() { None } else { Some(wait) };
                    if running.child.wait_readable(true, !self.stderr_done, wake, timeout).is_none()
                    {
                        self.clock.wait(Some(cancel), wait);
                    }
                    self.wakeups += 1;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => self.failed = Some(error.to_string()),
            }
        }
    }

    /// How many idle waits have returned so far. A silent real child adds none
    /// until it produces output, exits or the token is cancelled.
    #[must_use]
    pub const fn idle_wakeups(&self) -> u64 { self.wakeups }

    /// The retained stderr tail, lossily decoded.
    #[must_use]
    pub fn stderr_tail(&self) -> String {
        let (front, back) = self.stderr.bytes.as_slices();
        let mut bytes = front.to_vec();
        bytes.extend_from_slice(back);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Stderr bytes discarded because the tail was full.
    #[must_use]
    pub const fn stderr_dropped_bytes(&self) -> u64 { self.stderr.dropped }

    /// Stops the child: `SIGTERM` to the exact pid, a bounded grace, then
    /// `SIGKILL`, then a bounded wait for the reap.
    ///
    /// Works after cancellation. An unreaped child is retained by the executor.
    #[must_use]
    pub fn stop(mut self) -> StopOutcome {
        let Some(mut running) = self.child.take() else {
            return StopOutcome::ReapTimeout {
                diagnostic: "no child".to_owned(),
            };
        };
        if let Some(status) = self.exited {
            return StopOutcome::Exited { status };
        }
        if let Ok(Some(status)) = running.child.try_wait() {
            return StopOutcome::Exited { status };
        }
        let mut problems = Vec::new();
        if let Err(error) = running.child.terminate() {
            problems.push(format!("terminate failed: {error}"));
        }
        if let Some(status) = self.wait_for_exit(&mut running, self.term_grace, &mut problems) {
            return StopOutcome::Exited { status };
        }
        if let Err(error) = running.child.kill() {
            problems.push(format!("kill failed: {error}"));
        }
        if let Some(status) = self.wait_for_exit(&mut running, CANCEL_CLEANUP, &mut problems) {
            return StopOutcome::Killed { status };
        }
        let program = running.program.clone();
        let diagnostic = format!(
            "{} was not confirmed gone after SIGTERM and SIGKILL; its handle is retained for \
             reaping{}",
            program.display(),
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
        StopOutcome::ReapTimeout { diagnostic }
    }

    fn wait_for_exit(
        &self,
        running: &mut Running,
        limit: Duration,
        problems: &mut Vec<String>,
    ) -> Option<ExitStatus> {
        let started = self.clock.now();
        loop {
            match running.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) => {}
                Err(error) => problems.push(format!("wait failed: {error}")),
            }
            if self.clock.now().saturating_sub(started) >= limit {
                return None;
            }
            self.clock.wait(None, POLL_QUANTUM);
        }
    }

    fn poll_exit(&mut self) -> Option<ExitStatus> {
        if self.exited.is_none() {
            self.exited = self.child.as_mut().and_then(|r| r.child.try_wait().ok().flatten());
        }
        self.exited
    }

    /// Drains stderr; returns whether any bytes arrived.
    fn drain_stderr(&mut self) -> bool {
        let mut any = false;
        if self.stderr_done {
            return any;
        }
        let Some(running) = self.child.as_mut() else { return any };
        let mut buffer = [0_u8; STREAM_CHUNK];
        let mut budget = STDERR_BUDGET;
        while budget > 0 {
            match running.child.read_stderr(&mut buffer) {
                Ok(0) => {
                    self.stderr_done = true;
                    break;
                }
                Ok(count) => {
                    any = true;
                    self.stderr.push(&buffer[..count]);
                    budget = budget.saturating_sub(count);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        any
    }
}

impl Drop for LineStream {
    fn drop(&mut self) {
        let Some(mut running) = self.child.take() else { return };
        if matches!(running.child.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = running.child.kill();
        if matches!(running.child.try_wait(), Ok(Some(_))) {
            return;
        }
        let Running { child, reservation, program } = running;
        reservation.retain(child, program, self.clock.now());
    }
}

#[cfg(test)]
mod tests;
