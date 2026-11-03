//! Stache Next command line.
//!
//! The crate knows the SDK catalog and the composition contract but never
//! imports the application or a module crate: the composition root supplies
//! both the configuration catalog and the contributed commands. Every command
//! writes through injected `Write` sinks so tests do not need a subprocess,
//! and standalone commands never start the desktop host.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cache;
pub mod commands;
pub mod config;
pub mod migrate;
pub mod mode;
pub mod output;
pub mod schema;

use std::ffi::OsString;
use std::fs;
use std::io::{ErrorKind, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

pub use cache::CacheCommand;
pub use config::{ConfigCommand, InitArgs, MigrateArgs};
pub use schema::{generate_files, run_generate};
use stache_sdk::config::ConfigEnvironment;
use stache_sdk::ipc::{ClientError, IpcClient};
use stache_sdk::{AppEnvironment, AppPathError, AppPaths, CommandCatalog, ConfigCatalog};

/// Injected process environment for one CLI invocation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CliEnvironment {
    /// Configuration directory discovery.
    pub config: ConfigEnvironment,
    /// Cache and runtime identity roots.
    pub app: AppEnvironment,
}

impl CliEnvironment {
    /// Reads the process environment once.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            config: ConfigEnvironment::from_env(),
            app: AppEnvironment::from_env(),
        }
    }

    /// Combines an explicit configuration and app environment.
    #[must_use]
    pub const fn new(config: ConfigEnvironment, app: AppEnvironment) -> Self {
        Self { config, app }
    }

    /// Resolves the identity cache and runtime paths.
    ///
    /// # Errors
    /// Returns [`AppPathError`] when no safe, non-overlapping pair exists.
    pub fn app_paths(&self) -> Result<AppPaths, AppPathError> { AppPaths::resolve(&self.app) }

    #[cfg(test)]
    pub(crate) fn for_test(home: &Path, temporary: &Path) -> Self {
        Self {
            config: ConfigEnvironment {
                xdg_config_home: None,
                home: Some(home.to_path_buf()),
            },
            app: AppEnvironment {
                home: Some(home.to_path_buf()),
                temporary: Some(temporary.to_path_buf()),
            },
        }
    }
}

/// Runs one command with the process environment and standard streams.
pub fn run(
    args: impl IntoIterator<Item = OsString>,
    configurations: &ConfigCatalog,
    commands: &CommandCatalog,
) -> ExitCode {
    let environment = CliEnvironment::from_env();
    let ansi = std::io::stdout().is_terminal();
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    run_with_io(
        args,
        configurations,
        commands,
        &environment,
        ansi,
        &mut out,
        &mut err,
    )
}

/// Runs one command with injected environment, terminal mode and output sinks.
pub fn run_with_io<I, W, E>(
    args: I,
    configurations: &ConfigCatalog,
    commands: &CommandCatalog,
    environment: &CliEnvironment,
    ansi: bool,
    out: &mut W,
    err: &mut E,
) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
    W: Write,
    E: Write,
{
    for name in commands.names() {
        if commands::BUILTIN_COMMANDS.contains(&name) {
            return fail(
                err,
                &format!("contributed command `{name}` collides with a built-in command"),
            );
        }
    }
    let mut command = commands::compose(commands);
    let matches = match command.try_get_matches_from_mut(args) {
        Ok(matches) => matches,
        Err(error) => {
            let code = u8::try_from(error.exit_code()).unwrap_or(1);
            if error.use_stderr() {
                let _ = write!(err, "{error}");
            } else {
                let _ = write!(out, "{error}");
            }
            return ExitCode::from(code);
        }
    };
    commands::run(&matches, configurations, commands, environment, ansi, out, err)
}

fn reload<E: Write>(environment: &CliEnvironment, err: &mut E) -> ExitCode {
    let paths = match environment.app_paths() {
        Ok(paths) => paths,
        Err(error) => return fail(err, &error.to_string()),
    };
    let socket = match paths.socket_path() {
        Ok(socket) => socket,
        Err(error) => return fail(err, &error.to_string()),
    };
    match IpcClient::new(socket).reload() {
        Ok(report) => {
            let reported = report.files.iter().any(|file| file.diagnostic.is_some());
            for file in &report.files {
                if let Some(diagnostic) = &file.diagnostic {
                    let _ = writeln!(err, "stache: {diagnostic}");
                }
            }
            if reported {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(ClientError::NotRunning) => fail(
            err,
            "IPC error: Failed to send reload command to Stache app: Stache app is not running",
        ),
        Err(error) => fail(err, &error.to_string()),
    }
}

/// Reports an application error in the stable CLI form.
pub(crate) fn fail(err: &mut impl Write, message: &str) -> ExitCode {
    let _ = writeln!(err, "stache: {message}");
    ExitCode::FAILURE
}

/// Writes a file through a temporary file in the same directory and a rename.
///
/// Replacing an existing file copies its permission bits onto the temporary
/// file, so a `0644` configuration stays `0644`. A new file keeps the
/// temporary file's private mode, and the rename still breaks hard links.
pub(crate) fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents.as_bytes())?;
    match fs::metadata(path) {
        Ok(metadata) => temporary.as_file().set_permissions(metadata.permissions())?,
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use stache_sdk::GlobalConfig;

    use super::*;

    fn run_args(args: &[&str], catalog: &ConfigCatalog) -> (ExitCode, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_with_io(
            args.iter().map(OsString::from),
            catalog,
            &CommandCatalog::new(),
            &CliEnvironment::default(),
            false,
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    fn catalog() -> ConfigCatalog {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        catalog
    }

    #[test]
    fn a_missing_home_is_reported_as_an_application_error() {
        let (code, _out, err) = run_args(&["stache", "config", "path"], &ConfigCatalog::new());
        assert_eq!(code, ExitCode::FAILURE);
        assert!(err.starts_with("stache: "));
    }

    #[test]
    fn malformed_flags_use_the_usage_exit_code() {
        let mut out = Cursor::new(Vec::new());
        let mut err = Cursor::new(Vec::new());
        let code = run_with_io(
            ["stache", "--definitely-not-a-flag"].map(OsString::from),
            &ConfigCatalog::new(),
            &CommandCatalog::new(),
            &CliEnvironment::default(),
            false,
            &mut out,
            &mut err,
        );
        assert_eq!(code, ExitCode::from(2));
        assert!(
            String::from_utf8(err.into_inner())
                .expect("utf-8")
                .contains("unexpected argument")
        );
    }

    #[test]
    fn config_flag_is_accepted_before_and_after_the_subcommand() {
        let catalog = catalog();
        for args in [
            vec!["stache", "--config", "/tmp/explicit", "config", "path"],
            vec!["stache", "config", "path", "-c", "/tmp/explicit"],
            vec!["stache", "--config=/tmp/explicit", "config", "path"],
        ] {
            let (code, out, err) = run_args(&args, &catalog);
            assert_eq!(code, ExitCode::SUCCESS, "{err}");
            assert!(out.starts_with("/tmp/explicit\n"), "{out}");
        }
    }

    #[test]
    fn init_path_flag_wins_over_the_global_config_flag() {
        let root = tempfile::tempdir().expect("temp dir");
        let global = root.path().join("global");
        let explicit = root.path().join("explicit");
        let (code, _out, err) = run_args(
            &[
                "stache",
                "--config",
                global.to_str().expect("utf-8"),
                "config",
                "init",
                "-p",
                explicit.to_str().expect("utf-8"),
            ],
            &catalog(),
        );
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(explicit.join("global.jsonc").exists());
        assert!(!global.exists());
    }
}
