//! One-shot `exec_on_startup` execution.
//!
//! The global configuration stores a command string or an ordered list. The
//! desktop host executes it once after initial setup, sequentially, stopping at
//! the first failure. Commands use the legacy argument rules (quoted segments,
//! backslash escapes, no implicit shell), so existing configuration keeps its
//! meaning without depending on the archived hotkey crate.
//!
//! The job runs on its own owned thread so a long-lived program cannot delay
//! host readiness, tray actions or shutdown. Cancellation kills only the child
//! this job spawned directly, waits for it, and stops before the next command.

use std::sync::Arc;
use std::thread::{self, JoinHandle};

use stache_sdk::CancellationToken;
use stache_sdk::commands::{CommandEnvironment, CommandExecutor, SequenceOutcome, StepFailure};
use stache_sdk::shutdown::{ShutdownWait, wait_for_finish};

/// Executes the configured startup commands.
///
/// Implementations run on the owned startup thread; tests inject a recorder.
pub trait CommandRunner: Send + Sync {
    /// Runs every command in order and stops after the first failure or
    /// cancellation.
    ///
    /// # Errors
    /// Returns the failing step and a human-readable reason. Cancellation is
    /// reported as success because shutdown is not a command failure.
    fn run(&self, commands: &[String], cancel: &CancellationToken) -> Result<(), String>;
}

/// The host-owned startup thread.
pub struct StartupJob {
    cancel: CancellationToken,
    thread: Option<JoinHandle<()>>,
}

impl StartupJob {
    /// Starts the one-shot job. `finished` receives the result on the job thread.
    ///
    /// # Errors
    /// Returns an I/O failure when the thread cannot be created.
    pub fn spawn(
        commands: Vec<String>,
        runner: Arc<dyn CommandRunner>,
        finished: impl FnOnce(Result<(), String>) + Send + 'static,
    ) -> std::io::Result<Self> {
        let cancel = CancellationToken::new();
        let worker = cancel.clone();
        let thread = thread::Builder::new().name("stache-startup".to_owned()).spawn(move || {
            let result = runner.run(&commands, &worker);
            finished(result);
        })?;
        Ok(Self { cancel, thread: Some(thread) })
    }

    /// Cancels the job without joining it or waiting for its child.
    pub fn cancel(&self) { self.cancel.cancel(); }

    /// Cancels the job, kills its directly owned child, and joins the thread.
    ///
    /// Call off the main thread.
    pub fn stop(mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// Cancels the job and waits for its thread within the teardown bound.
    ///
    /// Returns `true` when the thread finished inside the bound. On `false` the
    /// handle is dropped, detaching the thread instead of joining it.
    pub fn stop_bounded(mut self, wait: &dyn ShutdownWait) -> bool {
        self.cancel.cancel();
        let Some(thread) = self.thread.take() else {
            return true;
        };
        if wait_for_finish(wait, &|| thread.is_finished()) {
            let _ = thread.join();
            true
        } else {
            false
        }
    }
}

/// Spawns each command as a direct child process and waits for it.
///
/// Parsing, resolution, the cancellation-aware wait loop and bounded child
/// ownership live in [`stache_sdk::commands`]; this adapter owns only the
/// startup-specific failure wording and the rule that cancellation is not a
/// failure.
#[derive(Debug)]
pub struct ProcessCommands {
    executor: CommandExecutor,
}

impl ProcessCommands {
    /// A runner over real processes with the process environment.
    #[must_use]
    pub fn new() -> Self {
        Self::with_executor(CommandExecutor::new(CommandEnvironment::from_env()))
    }

    /// A runner over an injected executor.
    #[must_use]
    pub fn with_executor(executor: CommandExecutor) -> Self {
        Self {
            executor: executor.with_diagnostics(|line| eprintln!("stache: startup: {line}")),
        }
    }
}

impl Default for ProcessCommands {
    fn default() -> Self { Self::new() }
}

impl CommandRunner for ProcessCommands {
    fn run(&self, commands: &[String], cancel: &CancellationToken) -> Result<(), String> {
        match self.executor.run_sequence(commands, cancel, cancel) {
            SequenceOutcome::Completed | SequenceOutcome::Cancelled(_) => Ok(()),
            SequenceOutcome::Failed { index, total, failure, .. } => {
                Err(format_failure(index + 1, total, &failure))
            }
        }
    }
}

/// Renders one failed startup step with the established wording.
fn format_failure(step: usize, total: usize, failure: &StepFailure) -> String {
    match failure {
        StepFailure::Empty => format!("startup command {step} is empty"),
        StepFailure::Parse(_) | StepFailure::Resolve(_) | StepFailure::Capacity => {
            format!("startup command {step}: {failure}")
        }
        StepFailure::Spawn(_) | StepFailure::Wait(_) => {
            format!("startup command {step} {failure}")
        }
        StepFailure::Exit(status) => format!("startup command {step}/{total} exited with {status}"),
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::Duration;

    use stache_sdk::commands::{CommandParseError, split_command};

    use super::*;

    #[test]
    fn quoted_arguments_keep_spaces_and_escapes() {
        assert_eq!(
            split_command(r#"open -a "My App" 'two words'"#).expect("split"),
            vec!["open", "-a", "My App", "two words"]
        );
        assert_eq!(split_command(r#"say "a \"quote\"""#).expect("split"), vec![
            "say",
            "a \"quote\""
        ]);
        assert_eq!(split_command("   ").expect("empty"), Vec::<String>::new());
    }

    #[test]
    fn malformed_quotes_are_rejected() {
        assert_eq!(
            split_command("\"open"),
            Err(CommandParseError::UnterminatedQuote('"'))
        );
        assert_eq!(split_command("open \\"), Err(CommandParseError::TrailingEscape));
    }

    #[test]
    fn commands_run_in_order_and_stop_at_the_first_failure() {
        let runner = ProcessCommands::new();
        let cancel = CancellationToken::new();
        assert!(runner.run(&["/usr/bin/true".to_owned()], &cancel).is_ok());
        let error = runner
            .run(
                &[
                    "/usr/bin/true".to_owned(),
                    "/usr/bin/false".to_owned(),
                    "/usr/bin/true".to_owned(),
                ],
                &cancel,
            )
            .expect_err("second command fails");
        assert!(error.contains("2/3"), "{error}");
        assert!(runner.run(&["definitely-missing-stache-binary".to_owned()], &cancel).is_err());
    }

    #[test]
    fn cancellation_kills_only_the_owned_child_and_stops_the_sequence() {
        let runner = ProcessCommands::new();
        let cancel = CancellationToken::new();
        let root = tempfile::tempdir().expect("temp dir");
        let marker = root.path().join("started");
        let started = std::time::Instant::now();
        let worker = std::thread::spawn({
            let cancel = cancel.clone();
            move || {
                runner.run(
                    &[
                        format!("/bin/sh -c 'touch {}; exec /bin/sleep 60'", marker.display()),
                        "/usr/bin/false".to_owned(),
                    ],
                    &cancel,
                )
            }
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !root.path().join("started").exists() {
            assert!(std::time::Instant::now() < deadline, "child did not start");
            std::thread::sleep(Duration::from_millis(5));
        }
        cancel.cancel();
        let result = worker.join().expect("startup worker");
        assert!(result.is_ok(), "cancellation is not a command failure");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the sleep was not interrupted"
        );
    }

    #[test]
    fn cancellation_leaves_an_unrelated_process_running() {
        let runner = ProcessCommands::new();
        let cancel = CancellationToken::new();
        let root = tempfile::tempdir().expect("temp dir");
        let marker = root.path().join("started");
        let mut unrelated = Command::new("/bin/sleep").arg("30").spawn().expect("unrelated child");
        let worker = std::thread::spawn({
            let cancel = cancel.clone();
            move || {
                runner.run(
                    &[format!(
                        "/bin/sh -c 'touch {}; exec /bin/sleep 60'",
                        marker.display()
                    )],
                    &cancel,
                )
            }
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !root.path().join("started").exists() {
            assert!(std::time::Instant::now() < deadline, "child did not start");
            std::thread::sleep(Duration::from_millis(5));
        }
        cancel.cancel();
        assert!(worker.join().expect("startup worker").is_ok());
        assert!(
            unrelated.try_wait().expect("unrelated child").is_none(),
            "the job kills only the child it spawned"
        );
        let _ = unrelated.kill();
        let _ = unrelated.wait();
    }

    #[test]
    fn failures_keep_the_established_startup_wording() {
        let runner = ProcessCommands::new();
        let cancel = CancellationToken::new();
        let run = |commands: &[&str]| {
            let commands: Vec<String> = commands.iter().map(|c| (*c).to_owned()).collect();
            runner.run(&commands, &cancel).expect_err("fails")
        };
        assert_eq!(run(&["\"open"]), "startup command 1: unterminated \" quote");
        assert_eq!(run(&["open \\"]), "startup command 1: trailing escape character");
        assert_eq!(
            run(&["/usr/bin/true", "'' x"]),
            "startup command 2: binary name cannot be empty"
        );
        assert_eq!(
            run(&["/usr/bin/definitely-missing"]),
            "startup command 1: /usr/bin/definitely-missing is not executable"
        );
        assert_eq!(
            run(&["definitely-missing-stache-binary"]),
            "startup command 1: `definitely-missing-stache-binary` was not found on PATH"
        );
        assert!(
            run(&["/usr/bin/true", "/usr/bin/false"])
                .starts_with("startup command 2/2 exited with ")
        );
    }

    #[test]
    fn spawn_and_wait_failures_use_the_could_not_wording() {
        let spawn = StepFailure::Spawn("denied".to_owned());
        assert_eq!(
            format_failure(3, 4, &spawn),
            "startup command 3 could not run: denied"
        );
        let wait = StepFailure::Wait("broken".to_owned());
        assert_eq!(
            format_failure(3, 4, &wait),
            "startup command 3 could not be waited for: broken"
        );
        assert_eq!(
            format_failure(1, 1, &StepFailure::Empty),
            "startup command 1 is empty"
        );
    }
}
