//! CLI composition primitives.
//!
//! The application composes the command line: it contributes subcommands and
//! their executors, and the CLI crate merges them with the built-ins. The SDK
//! knows only clap commands and the executor trait, never a concrete module.

use std::fmt;
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{ArgMatches, Command};

/// One contributed command's implementation.
///
/// The executor owns its standalone-versus-remote decision: a contributed
/// command may read local state, contact a running host, or both. `ansi`
/// reports whether stdout is a terminal so contributed output can follow the
/// same styling policy as built-in commands.
pub trait CliExecutor: Send + Sync {
    /// Runs the parsed subcommand, writing through the injected streams.
    fn execute(
        &self,
        matches: &ArgMatches,
        ansi: bool,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> ExitCode;
}

/// A clap subcommand together with the executor that serves it.
///
/// The command is built by a closure because clap commands are consumed when
/// assembled; every CLI invocation composes a fresh tree.
pub struct CliContribution {
    build: Box<dyn Fn() -> Command + Send + Sync>,
    executor: Arc<dyn CliExecutor>,
    name: String,
}

impl CliContribution {
    /// Creates a contribution.
    ///
    /// The command is built once to read its name; the same builder runs again
    /// when the root command is assembled.
    #[must_use]
    pub fn new(
        build: impl Fn() -> Command + Send + Sync + 'static,
        executor: impl CliExecutor + 'static,
    ) -> Self {
        let name = build().get_name().to_owned();
        Self {
            build: Box::new(build),
            executor: Arc::new(executor),
            name,
        }
    }

    /// The top-level subcommand name.
    #[must_use]
    pub fn name(&self) -> &str { &self.name }

    /// Builds a fresh clap subcommand.
    #[must_use]
    pub fn command(&self) -> Command { (self.build)() }

    /// The executor for this command.
    #[must_use]
    pub fn executor(&self) -> Arc<dyn CliExecutor> { Arc::clone(&self.executor) }
}

/// Contributed commands, in registration order.
#[derive(Default)]
pub struct CommandCatalog {
    contributions: Vec<CliContribution>,
}

impl CommandCatalog {
    /// Creates an empty catalog.
    #[must_use]
    pub const fn new() -> Self { Self { contributions: Vec::new() } }

    /// Adds one contribution.
    ///
    /// # Errors
    /// Returns [`CommandCatalogError::DuplicateCommand`] when the name is
    /// already registered.
    pub fn add(&mut self, contribution: CliContribution) -> Result<(), CommandCatalogError> {
        if self.contributions.iter().any(|known| known.name == contribution.name) {
            return Err(CommandCatalogError::DuplicateCommand { name: contribution.name });
        }
        self.contributions.push(contribution);
        Ok(())
    }

    /// Whether no command was contributed.
    #[must_use]
    pub const fn is_empty(&self) -> bool { self.contributions.is_empty() }

    /// The contribution count.
    #[must_use]
    pub const fn len(&self) -> usize { self.contributions.len() }

    /// Contributed names in registration order.
    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.contributions.iter().map(CliContribution::name)
    }

    /// Fresh clap subcommands in registration order.
    pub fn commands(&self) -> impl Iterator<Item = Command> + '_ {
        self.contributions.iter().map(CliContribution::command)
    }

    /// The executor registered for a subcommand name.
    #[must_use]
    pub fn executor(&self, name: &str) -> Option<Arc<dyn CliExecutor>> {
        self.contributions
            .iter()
            .find(|contribution| contribution.name == name)
            .map(CliContribution::executor)
    }
}

impl fmt::Debug for CommandCatalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandCatalog")
            .field("names", &self.names().collect::<Vec<_>>())
            .finish()
    }
}

/// A contributed command name conflicts with another contribution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandCatalogError {
    /// Two contributions use the same top-level name.
    DuplicateCommand {
        /// The duplicated name.
        name: String,
    },
}

impl fmt::Display for CommandCatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateCommand { name } => {
                write!(f, "a command named `{name}` is already contributed")
            }
        }
    }
}

impl std::error::Error for CommandCatalogError {}

#[cfg(test)]
mod tests {
    use clap::Arg;

    use super::*;

    struct Echo;

    impl CliExecutor for Echo {
        fn execute(
            &self,
            matches: &ArgMatches,
            ansi: bool,
            out: &mut dyn Write,
            _err: &mut dyn Write,
        ) -> ExitCode {
            let value = matches.get_one::<String>("value").map_or("none", String::as_str);
            let marker = if ansi { "ansi" } else { "plain" };
            let _ = writeln!(out, "echo {value} {marker}");
            ExitCode::SUCCESS
        }
    }

    fn contribution() -> CliContribution {
        CliContribution::new(
            || Command::new("echo").about("echo a value").arg(Arg::new("value")),
            Echo,
        )
    }

    #[test]
    fn contributions_keep_their_names_and_rebuild_fresh_commands() {
        let contribution = contribution();
        assert_eq!(contribution.name(), "echo");
        assert_eq!(contribution.command().get_name(), "echo");
        assert_eq!(
            contribution.command().get_about().map(ToString::to_string),
            Some("echo a value".to_owned())
        );
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let mut catalog = CommandCatalog::new();
        catalog.add(contribution()).expect("first");
        assert_eq!(
            catalog.add(contribution()),
            Err(CommandCatalogError::DuplicateCommand { name: "echo".to_owned() })
        );
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog.names().collect::<Vec<_>>(), ["echo"]);
    }

    #[test]
    fn executors_are_reachable_by_name() {
        let mut catalog = CommandCatalog::new();
        catalog.add(contribution()).expect("add");
        assert!(catalog.executor("echo").is_some());
        assert!(catalog.executor("missing").is_none());
        assert!(!catalog.is_empty());
    }
}
