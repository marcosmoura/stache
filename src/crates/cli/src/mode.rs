//! Bundle detection for mode selection after clap validation.
//!
//! The host selects desktop for a valid bundled invocation, `--desktop`, or
//! no parsed subcommand. Argument parsing belongs solely to `commands::compose`.

use std::path::Path;

use clap::error::ErrorKind;
use clap::{ArgMatches, Command};

/// The clap argument ID of the hidden internal-CLI flag.
pub const INTERNAL_CLI_ID: &str = "internal_cli";
/// The long flag name, without the leading dashes.
pub const INTERNAL_CLI_FLAG: &str = "stache-internal-cli";

/// Whether the executable is inside an application bundle (desktop precedence).
#[must_use]
pub fn is_app_bundle(current_exe: &Path) -> bool {
    current_exe.to_string_lossy().contains(".app/Contents/MacOS")
}

/// Whether a validated invocation asks for the internal CLI path.
///
/// The flag conflicts with `--desktop` in the parser itself. It also needs a
/// parsed subcommand, so a bare `stache --stache-internal-cli` is a usage error
/// rather than desktop work.
///
/// # Errors
/// Returns a usage error (exit code 2) when the flag has no subcommand.
pub fn internal_cli_requested(
    command: &mut Command,
    matches: &ArgMatches,
) -> Result<bool, clap::Error> {
    if !matches.get_flag(INTERNAL_CLI_ID) {
        return Ok(false);
    }
    if matches.subcommand_name().is_none() {
        return Err(command.error(
            ErrorKind::MissingSubcommand,
            format!("--{INTERNAL_CLI_FLAG} requires a command"),
        ));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::process::ExitCode;

    use clap::{ArgMatches, Command as ClapCommand};
    use stache_sdk::{CliContribution, CliExecutor, CommandCatalog};

    use super::*;
    use crate::commands::compose;
    use crate::{CliEnvironment, run_with_io};

    struct Tiling;

    impl CliExecutor for Tiling {
        fn execute(
            &self,
            matches: &ArgMatches,
            _: bool,
            out: &mut dyn Write,
            _: &mut dyn Write,
        ) -> ExitCode {
            let (name, sub) = matches.subcommand().expect("nested command");
            let _ = writeln!(
                out,
                "{name} {}",
                sub.get_one::<String>("filter").map_or("", String::as_str)
            );
            ExitCode::from(7)
        }
    }

    fn tiling_catalog() -> CommandCatalog {
        let mut catalog = CommandCatalog::new();
        catalog
            .add(CliContribution::new(
                || {
                    ClapCommand::new("tiling").subcommand(
                        ClapCommand::new("query").arg(clap::Arg::new("filter").long("filter")),
                    )
                },
                Tiling,
            ))
            .expect("tiling");
        catalog
    }

    fn requested(catalog: &CommandCatalog, args: &[&str]) -> Result<bool, clap::Error> {
        let mut command = compose(catalog);
        let matches = command.try_get_matches_from_mut(args)?;
        internal_cli_requested(&mut command, &matches)
    }

    #[test]
    fn the_flag_requires_a_subcommand_and_conflicts_with_desktop() {
        let catalog = CommandCatalog::new();
        assert!(!requested(&catalog, &["stache", "reload"]).expect("ordinary"));
        assert!(!requested(&catalog, &["stache"]).expect("bare"));
        assert!(requested(&catalog, &["stache", "--stache-internal-cli", "reload"]).expect("flag"));
        assert!(
            requested(&catalog, &[
                "stache",
                "-c",
                "/x",
                "--stache-internal-cli",
                "schema"
            ])
            .expect("explicit config")
        );

        let missing =
            requested(&catalog, &["stache", "--stache-internal-cli"]).expect_err("no command");
        assert_eq!(missing.exit_code(), 2);
        assert!(missing.use_stderr());
        assert!(missing.to_string().contains("requires a command"));

        for args in [
            &["stache", "--stache-internal-cli", "--desktop", "reload"][..],
            &["stache", "--desktop", "--stache-internal-cli"],
            &[
                "stache",
                "--stache-internal-cli",
                "--stache-internal-cli",
                "reload",
            ],
            &["stache", "reload", "--stache-internal-cli"],
            &["stache", "--stache-internal-cli", "tiling", "query"],
        ] {
            let error = requested(&catalog, args).expect_err("invalid");
            assert_eq!(error.exit_code(), 2, "{args:?}");
        }
    }

    #[test]
    fn help_and_version_win_before_validation() {
        let catalog = CommandCatalog::new();
        for args in [&["stache", "--stache-internal-cli", "--help"][..], &[
            "stache",
            "--stache-internal-cli",
            "--version",
        ]] {
            let error = requested(&catalog, args).expect_err("display");
            assert_eq!(error.exit_code(), 0);
            assert!(!error.use_stderr());
        }
    }

    #[test]
    fn a_contributed_tiling_command_runs_unchanged_through_the_flag() {
        let catalog = tiling_catalog();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with_io(
            [
                "stache",
                "--stache-internal-cli",
                "tiling",
                "query",
                "--filter",
                "all",
            ]
            .map(std::ffi::OsString::from),
            &stache_sdk::ConfigCatalog::new(),
            &catalog,
            &CliEnvironment::default(),
            false,
            &mut out,
            &mut err,
        );
        assert_eq!(code, ExitCode::from(7), "the executor's real status propagates");
        assert_eq!(String::from_utf8(out).expect("utf-8"), "query all\n");
        assert!(
            requested(&catalog, &["stache", "--stache-internal-cli", "tiling", "query"])
                .expect("registered")
        );
    }

    #[test]
    fn an_unregistered_tiling_command_is_an_unknown_command() {
        let error = requested(&CommandCatalog::new(), &[
            "stache",
            "--stache-internal-cli",
            "tiling",
        ])
        .expect_err("unknown");
        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("tiling"));
    }

    #[test]
    fn bundle_detection_preserves_legacy_executable_precedence() {
        for bundle in [
            "/Applications/Stache.app/Contents/MacOS/stache",
            "/Applications/Stache Next.app/Contents/MacOS/stache",
            "/Users/me/Builds/Anything.app/Contents/MacOS/stache",
        ] {
            assert!(is_app_bundle(Path::new(bundle)), "{bundle}");
        }
        assert!(!is_app_bundle(Path::new("/usr/local/bin/stache")));
    }
}
