//! The bounded `/usr/bin/hidutil` boundary.
//!
//! Every call goes through the shared [`CommandExecutor`] captured mode: a
//! 5 s child lifetime, 1 MiB of standard output and 64 KiB of standard error,
//! exact child ownership, 2 s cleanup and the bounded reaper. Output that
//! overflows or arrives after the deadline is never parsed. Nothing here
//! signals a process by name.

use std::time::Duration;

use stache_sdk::CancellationToken;
use stache_sdk::commands::{
    CaptureLimits, CaptureOutcome, CaptureStream, Cleanup, CommandEnvironment, CommandExecutor,
    PreparedCommand, StepFailure,
};

/// The only executable this module may run.
pub const HIDUTIL_PATH: &str = "/usr/bin/hidutil";
/// Child lifetime.
pub const HIDUTIL_DEADLINE: Duration = Duration::from_secs(5);
/// Standard output cap in raw bytes.
pub const HIDUTIL_STDOUT_CAP: usize = 1024 * 1024;
/// Standard error cap in raw bytes.
pub const HIDUTIL_STDERR_CAP: usize = 64 * 1024;

/// How a `hidutil` call failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidutilErrorKind {
    /// Nothing ran: refused before a child existed (no reaper slot, spawn
    /// failure, cancellation before spawn).
    NotRun,
    /// The child ran and exited unsuccessfully.
    Exited,
    /// A stream exceeded its cap; nothing was parsed.
    Overflow,
    /// The deadline elapsed.
    TimedOut,
    /// Cancellation stopped a running child.
    Cancelled,
    /// A wait or read failed.
    Io,
}

/// A failed `hidutil` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidutilError {
    /// What happened.
    pub kind: HidutilErrorKind,
    /// A human-readable account, including cleanup diagnostics.
    pub message: String,
    /// Whether the child could not be confirmed gone and its handle is retained.
    pub child_unconfirmed: bool,
}

impl HidutilError {
    /// Whether the call may have changed the global mapping.
    ///
    /// Anything that started a child may have: a timeout does not undo a write.
    #[must_use]
    pub fn may_have_mutated(&self) -> bool { self.kind != HidutilErrorKind::NotRun }

    /// A refusal before any child existed.
    #[must_use]
    pub fn not_run(message: impl Into<String>) -> Self {
        Self {
            kind: HidutilErrorKind::NotRun,
            message: message.into(),
            child_unconfirmed: false,
        }
    }
}

impl std::fmt::Display for HidutilError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HidutilError {}

/// The `hidutil` operations the remap needs.
pub trait Hidutil: Send + Sync {
    /// Reads `UserKeyMapping` and returns the complete raw standard output.
    ///
    /// # Errors
    /// Returns a [`HidutilError`]; partial output is never returned.
    fn get(&self, cancel: &CancellationToken) -> Result<Vec<u8>, HidutilError>;

    /// Writes `UserKeyMapping` from a JSON `payload`.
    ///
    /// # Errors
    /// Returns a [`HidutilError`]; unless it [`NotRun`](HidutilErrorKind::NotRun)
    /// the write may have happened.
    fn set(&self, payload: &str, cancel: &CancellationToken) -> Result<(), HidutilError>;

    /// How many earlier children are still not confirmed gone.
    ///
    /// While any remain, a mapping that looks restored is not trusted.
    fn unreaped(&self) -> usize;
}

/// `hidutil` through a [`CommandExecutor`].
pub struct ProcessHidutil {
    executor: CommandExecutor,
}

impl ProcessHidutil {
    /// Wraps an executor. Tests inject a fake factory and clock.
    #[must_use]
    pub const fn new(executor: CommandExecutor) -> Self { Self { executor } }

    /// Real processes and the real clock.
    #[must_use]
    pub fn system() -> Self { Self::new(CommandExecutor::new(CommandEnvironment::default())) }

    fn limits() -> CaptureLimits {
        // The constants are within the shared maximums, so this cannot fail.
        CaptureLimits::new(HIDUTIL_DEADLINE, HIDUTIL_STDOUT_CAP, HIDUTIL_STDERR_CAP)
            .unwrap_or_else(|_| unreachable!("hidutil limits are valid"))
    }

    fn run(
        &self,
        arguments: Vec<String>,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, HidutilError> {
        let prepared = PreparedCommand {
            program: HIDUTIL_PATH.into(),
            arguments,
        };
        let gate = cancel.clone();
        match self.executor.run_captured_prepared(&prepared, Self::limits(), cancel, &gate) {
            CaptureOutcome::Completed(output) if output.status.success() => Ok(output.stdout),
            CaptureOutcome::Completed(output) => Err(HidutilError {
                kind: HidutilErrorKind::Exited,
                message: format!(
                    "hidutil exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                child_unconfirmed: false,
            }),
            CaptureOutcome::Overflow { stream, cleanup } => {
                let name = match stream {
                    CaptureStream::Stdout => "stdout",
                    CaptureStream::Stderr => "stderr",
                };
                Err(stopped(
                    HidutilErrorKind::Overflow,
                    &format!("hidutil {name} overflowed"),
                    &cleanup,
                ))
            }
            CaptureOutcome::TimedOut { cleanup } => Err(stopped(
                HidutilErrorKind::TimedOut,
                "hidutil exceeded its 5 s deadline",
                &cleanup,
            )),
            CaptureOutcome::Cancelled(None) => {
                Err(HidutilError::not_run("cancelled before hidutil ran"))
            }
            CaptureOutcome::Cancelled(Some(cleanup)) => Err(stopped(
                HidutilErrorKind::Cancelled,
                "hidutil was cancelled while running",
                &cleanup,
            )),
            CaptureOutcome::Failed { failure, cleanup } => Err(match (&failure, cleanup) {
                (
                    StepFailure::Capacity
                    | StepFailure::Spawn(_)
                    | StepFailure::Resolve(_)
                    | StepFailure::Parse(_)
                    | StepFailure::Empty,
                    None,
                ) => HidutilError::not_run(format!("hidutil did not run: {failure}")),
                (_, Some(cleanup)) => stopped(
                    HidutilErrorKind::Io,
                    &format!("hidutil failed: {failure}"),
                    &cleanup,
                ),
                (_, None) => HidutilError {
                    kind: HidutilErrorKind::Io,
                    message: format!("hidutil failed: {failure}"),
                    child_unconfirmed: false,
                },
            }),
        }
    }
}

fn stopped(kind: HidutilErrorKind, what: &str, cleanup: &Cleanup) -> HidutilError {
    match cleanup {
        Cleanup::Reaped => HidutilError {
            kind,
            message: format!("{what}; the child was killed and reaped"),
            child_unconfirmed: false,
        },
        Cleanup::Retained(diagnostic) => HidutilError {
            kind,
            message: format!("{what}; {diagnostic}"),
            child_unconfirmed: true,
        },
    }
}

impl Hidutil for ProcessHidutil {
    fn get(&self, cancel: &CancellationToken) -> Result<Vec<u8>, HidutilError> {
        self.run(
            vec!["property".into(), "--get".into(), "UserKeyMapping".into()],
            cancel,
        )
    }

    fn set(&self, payload: &str, cancel: &CancellationToken) -> Result<(), HidutilError> {
        self.run(
            vec!["property".into(), "--set".into(), payload.to_owned()],
            cancel,
        )
        .map(|_| ())
    }

    fn unreaped(&self) -> usize { self.executor.sweep().len() }
}
