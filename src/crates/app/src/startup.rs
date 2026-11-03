//! One-shot `execOnStartup` execution.
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

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use stache_sdk::CancellationToken;

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

    /// Cancels the job, kills its directly owned child, and joins the thread.
    ///
    /// Call off the main thread.
    pub fn stop(mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Spawns each command as a child process and waits for it.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessCommands;

impl CommandRunner for ProcessCommands {
    fn run(&self, commands: &[String], cancel: &CancellationToken) -> Result<(), String> {
        let total = commands.len();
        for (index, command) in commands.iter().enumerate() {
            if cancel.is_cancelled() {
                return Ok(());
            }
            let parts = split_command(command)
                .map_err(|error| format!("startup command {}: {error}", index + 1))?;
            let Some((binary, arguments)) = parts.split_first() else {
                return Err(format!("startup command {} is empty", index + 1));
            };
            let path = resolve_binary(binary)
                .map_err(|error| format!("startup command {}: {error}", index + 1))?;
            let mut child = Command::new(&path)
                .args(arguments)
                .spawn()
                .map_err(|error| format!("startup command {} could not run: {error}", index + 1))?;
            loop {
                if cancel.is_cancelled() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(());
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if !status.success() {
                            return Err(format!(
                                "startup command {}/{total} exited with {status}",
                                index + 1
                            ));
                        }
                        break;
                    }
                    Ok(None) => {
                        let _ = cancel.wait(Duration::from_millis(20));
                    }
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(format!(
                            "startup command {} could not be waited for: {error}",
                            index + 1
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Why a command line could not be split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandParseError {
    /// A quote was opened and never closed.
    UnterminatedQuote(char),
    /// A backslash ended the command.
    TrailingEscape,
}

impl std::fmt::Display for CommandParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnterminatedQuote(quote) => write!(f, "unterminated {quote} quote"),
            Self::TrailingEscape => f.write_str("trailing escape character"),
        }
    }
}

impl std::error::Error for CommandParseError {}

/// Splits one command into arguments using the legacy quoting rules.
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

/// Resolves an absolute or `PATH`-named executable, including common macOS
/// user installation directories that a GUI launch does not inherit.
fn resolve_binary(binary: &str) -> Result<PathBuf, String> {
    if binary.is_empty() {
        return Err("binary name cannot be empty".to_owned());
    }
    let candidate = Path::new(binary);
    if candidate.is_absolute() {
        return if is_executable(candidate) {
            Ok(candidate.to_path_buf())
        } else {
            Err(format!("{} is not executable", candidate.display()))
        };
    }
    let mut directories: Vec<PathBuf> = Vec::new();
    if let Some(extra) = std::env::var_os("STACHE_EXTRA_PATHS") {
        directories.extend(std::env::split_paths(&extra));
    }
    if let Some(path) = std::env::var_os("PATH") {
        directories.extend(std::env::split_paths(&path));
    }
    directories.extend([
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
    ]);
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        directories.push(home.join(".cargo/bin"));
        directories.push(home.join(".local/bin"));
    }
    for directory in directories {
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

#[cfg(test)]
mod tests {
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
        let runner = ProcessCommands;
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
        let runner = ProcessCommands;
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
    fn absolute_paths_are_not_searched_on_path() {
        assert!(resolve_binary("/usr/bin/true").is_ok());
        assert!(resolve_binary("/usr/bin/definitely-missing").is_err());
    }
}
