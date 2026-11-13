//! The built-in command tree and dispatch.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{ArgMatches, Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use clap_complete::Shell;
use stache_sdk::{CommandCatalog, ConfigCatalog, ConfigPaths};

use crate::{CliEnvironment, cache, config, fail, schema};

/// Top-level subcommands that modules may not override.
pub const BUILTIN_COMMANDS: &[&str] = &["cache", "completions", "config", "reload", "schema"];

/// Arguments accepted by every command.
#[derive(Args)]
struct GlobalArgs {
    /// Configuration directory (defaults to `$XDG_CONFIG_HOME/stache` or
    /// `~/.config/stache`).
    #[arg(short = 'c', long = "config", global = true, value_name = "DIR")]
    config: Option<PathBuf>,
}

/// Top-level command line.
#[derive(Parser)]
#[command(
    name = "stache",
    version,
    about = format!("{} command line", stache_sdk::PRODUCT_NAME),
    disable_help_subcommand = true
)]
struct Cli {
    #[command(flatten)]
    global: GlobalArgs,
    #[command(subcommand)]
    command: Option<Command>,
    /// Launch the desktop host.
    #[arg(long, hide = true)]
    desktop: bool,
    /// Run as a re-entered child of the running app: always select CLI mode.
    ///
    /// Hidden, root-only and not repeatable. It exists so a command that runs
    /// the bare name `stache` re-enters this executable even from a bundle.
    #[arg(
        long = "stache-internal-cli",
        id = crate::mode::INTERNAL_CLI_ID,
        hide = true,
        conflicts_with = "desktop"
    )]
    internal: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Manage cache files.
    Cache {
        #[command(subcommand)]
        command: cache::CacheCommand,
    },
    /// Generate a shell completion script.
    Completions {
        /// The shell to generate completions for.
        #[arg(long, short, value_name = "SHELL")]
        shell: Shell,
    },
    /// Read, generate or migrate configuration files.
    Config {
        #[command(subcommand)]
        command: config::ConfigCommand,
    },
    /// Ask the running app to reread every configuration file.
    Reload,
    /// Print one JSON Schema.
    Schema {
        /// Schema identifier: `global` or a registered module ID.
        id: Option<String>,
    },
}

impl Cli {
    fn config_directory(matches: &ArgMatches) -> Option<PathBuf> {
        matches.get_one::<PathBuf>("config").cloned()
    }
}

/// Builds the built-in command tree.
pub(crate) fn builtin_command() -> clap::Command { Cli::command() }

/// Builds the complete tree: built-ins plus contributed subcommands.
#[must_use]
pub fn compose(commands: &CommandCatalog) -> clap::Command {
    let mut command = builtin_command();
    for contribution in commands.commands() {
        command = command.subcommand(contribution);
    }
    command
}

/// Runs one parsed invocation.
pub(crate) fn run<W: Write, E: Write>(
    matches: &ArgMatches,
    configurations: &ConfigCatalog,
    commands: &CommandCatalog,
    environment: &CliEnvironment,
    ansi: bool,
    out: &mut W,
    err: &mut E,
) -> ExitCode {
    let Some((name, sub)) = matches.subcommand() else {
        return fail(err, "no command was provided");
    };
    match name {
        "cache" => {
            let command = match cache::CacheCommand::from_arg_matches(sub) {
                Ok(command) => command,
                Err(error) => return usage(err, &error),
            };
            let paths = match environment.app_paths() {
                Ok(paths) => paths,
                Err(error) => return fail(err, &error.to_string()),
            };
            cache::run(command, &paths, out, err)
        }
        "completions" => {
            let Some(shell) = sub.get_one::<Shell>("shell") else {
                return fail(err, "missing --shell");
            };
            clap_complete::generate(*shell, &mut compose(commands), "stache", out);
            ExitCode::SUCCESS
        }
        "config" => {
            let command = match config::ConfigCommand::from_arg_matches(sub) {
                Ok(command) => command,
                Err(error) => return usage(err, &error),
            };
            let explicit = Cli::config_directory(matches);
            let paths = match ConfigPaths::resolve(explicit, &environment.config) {
                Ok(paths) => paths,
                Err(error) => return fail(err, &error.to_string()),
            };
            config::run(command, configurations, &paths, environment, out, err)
        }
        "reload" => crate::reload(environment, err),
        "schema" => {
            let id = sub.get_one::<String>("id").map(String::as_str);
            schema::print(configurations, id, ansi, out, err)
        }
        _ => match commands.executor(name) {
            Some(executor) => executor.execute(sub, ansi, out, err),
            None => fail(err, &format!("unknown command `{name}`")),
        },
    }
}

fn usage(err: &mut impl Write, error: &clap::Error) -> ExitCode {
    let _ = write!(err, "{error}");
    ExitCode::from(u8::try_from(error.exit_code()).unwrap_or(2))
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    #[test]
    fn the_internal_flag_is_hidden_and_matches_the_sdk_contract() {
        assert_eq!(
            format!("--{}", crate::mode::INTERNAL_CLI_FLAG),
            stache_sdk::commands::INTERNAL_CLI_FLAG
        );
        let help = builtin_command().render_help().to_string();
        assert!(!help.contains("internal-cli"), "{help}");
    }

    #[test]
    fn the_builtin_tree_registers_only_shipped_commands() {
        let command = builtin_command();
        let names: Vec<&str> = command.get_subcommands().map(clap::Command::get_name).collect();
        assert_eq!(names, BUILTIN_COMMANDS);
        assert!(names.iter().all(|name| *name != "tiling" && *name != "wallpaper"));
    }

    #[test]
    fn completions_only_expose_registered_commands() {
        let mut command = compose(&CommandCatalog::new());
        let mut output = Vec::new();
        clap_complete::generate(Shell::Zsh, &mut command, "stache", &mut output);
        let script = String::from_utf8(output).expect("utf-8");
        for name in BUILTIN_COMMANDS {
            assert!(script.contains(name), "missing {name}");
        }
        assert!(!script.contains("tiling"));
        assert!(!script.contains("wallpaper"));
    }

    #[test]
    fn a_malformed_flag_is_a_usage_error() {
        let error = compose(&CommandCatalog::new())
            .try_get_matches_from(["stache", "--definitely-not-a-flag"])
            .expect_err("invalid flag");
        assert_eq!(error.kind(), ErrorKind::UnknownArgument);
    }

    #[test]
    fn legacy_command_shapes_still_parse() {
        for (arguments, expected) in [
            (vec!["stache", "reload"], "reload"),
            (vec!["stache", "schema"], "schema"),
            (vec!["stache", "schema", "proxyAudio"], "schema"),
            (vec!["stache", "cache", "clear"], "cache"),
            (vec!["stache", "cache", "path"], "cache"),
            (vec!["stache", "completions", "--shell", "zsh"], "completions"),
            (vec!["stache", "-c", "/tmp/config", "schema"], "schema"),
            (vec!["stache", "schema", "--config=/tmp/config"], "schema"),
        ] {
            let matches = compose(&CommandCatalog::new())
                .try_get_matches_from(&arguments)
                .unwrap_or_else(|error| panic!("{arguments:?}: {error}"));
            assert_eq!(matches.subcommand_name(), Some(expected), "{arguments:?}");
        }
        assert!(!builtin_command().render_version().is_empty());
    }
}
