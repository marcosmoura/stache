//! Bounded `borders` CLI fallback over the SDK [`CommandExecutor`].

use std::time::Duration;

use stache_sdk::commands::{
    CaptureLimits, CaptureOutcome, CommandExecutor, PreparedCommand, SpawnGate, resolve_binary,
};
use stache_sdk::threading::CancellationToken;

use super::encode::{EncodeError, encode_mach_args};

/// The external binary name; resolved through the SDK, never a shell.
pub const BORDERS_BINARY: &str = "borders";
/// Helper child lifetime bound.
pub const CLI_CHILD_DEADLINE: Duration = Duration::from_secs(2);
/// Per-stream capture cap (within the SDK's 1 MiB maximum).
pub const CLI_OUTPUT_CAP: usize = 64 * 1024;

/// How one CLI fallback ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliOutcome {
    /// The helper exited with status 0.
    Succeeded,
    /// The helper exited unsuccessfully (code, when it had one).
    ExitedWithFailure(Option<i32>),
    /// The binary could not be resolved; nothing was spawned.
    Unavailable(String),
    /// The argv is unsafe (NUL or empty argument); nothing was spawned.
    InvalidArguments(EncodeError),
    /// The deadline elapsed; the command may already have acted. `retained`
    /// names a child that could not be reaped in time.
    TimedOut {
        /// Description of a retained child, if any.
        retained: Option<String>,
    },
    /// A stream exceeded its cap; the command may already have acted.
    OutputOverflow,
    /// Cancellation or a stale gate stopped it before or during the run.
    Cancelled,
    /// Spawn, wait or capacity failure.
    Failed(String),
}

/// Runs the helper through one owned [`CommandExecutor`]; arguments are passed
/// as an argv vector, never through a shell.
#[derive(Debug)]
pub struct CliRunner {
    executor: CommandExecutor,
}

impl CliRunner {
    /// A runner over `executor` (which owns the retained-child slots).
    #[must_use]
    pub const fn new(executor: CommandExecutor) -> Self { Self { executor } }

    /// Polls the executor's retained helper children once and returns a
    /// description of each one still not reaped (empty when none). Only helper
    /// children this runner spawned are ever in the executor.
    #[must_use]
    pub fn sweep(&self) -> Vec<String> {
        self.executor
            .sweep()
            .into_iter()
            .map(|child| format!("{} (retained {:?})", child.program.display(), child.retained_for))
            .collect()
    }

    /// Runs `borders <args...>` with a 2 s deadline and 64 KiB output caps.
    ///
    /// Crate-visible only: callers go through
    /// [`ServiceAttempt::cli_fallback`](super::ServiceAttempt::cli_fallback), so
    /// the helper is unreachable without a successful lookup.
    pub(crate) fn run(
        &self,
        args: &[String],
        cancel: &CancellationToken,
        gate: &dyn SpawnGate,
    ) -> CliOutcome {
        // Same safety rules as the Mach encoding: an interior NUL or empty
        // argument is rejected rather than reinterpreted.
        if let Err(error) = encode_mach_args(args) {
            return CliOutcome::InvalidArguments(error);
        }
        let program = match resolve_binary(BORDERS_BINARY, self.executor.environment()) {
            Ok(program) => program,
            Err(reason) => return CliOutcome::Unavailable(reason),
        };
        let Ok(limits) = CaptureLimits::new(CLI_CHILD_DEADLINE, CLI_OUTPUT_CAP, CLI_OUTPUT_CAP)
        else {
            return CliOutcome::Failed("invalid capture limits".to_owned());
        };
        let prepared = PreparedCommand {
            program,
            arguments: args.to_vec(),
        };
        match self.executor.run_captured_prepared(&prepared, limits, cancel, gate) {
            CaptureOutcome::Completed(output) if output.status.success() => CliOutcome::Succeeded,
            CaptureOutcome::Completed(output) => {
                CliOutcome::ExitedWithFailure(output.status.code())
            }
            CaptureOutcome::Overflow { .. } => CliOutcome::OutputOverflow,
            CaptureOutcome::TimedOut { cleanup } => CliOutcome::TimedOut {
                retained: match cleanup {
                    stache_sdk::commands::Cleanup::Reaped => None,
                    stache_sdk::commands::Cleanup::Retained(text) => Some(text),
                },
            },
            CaptureOutcome::Cancelled(_) => CliOutcome::Cancelled,
            CaptureOutcome::Failed { failure, .. } => CliOutcome::Failed(failure.to_string()),
        }
    }
}
