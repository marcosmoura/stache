//! The `config` command family.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Subcommand};
use stache_sdk::config::{
    PathReport, PathState, TrustWarning, inspect_catalog_trust, inspect_paths, stale_jsonc_hint,
};
use stache_sdk::ipc::{CoreFileState, CoreStatus, IO_TIMEOUT, IpcClient, RetryPolicy};
use stache_sdk::{ConfigCatalog, ConfigPaths};

use crate::{CliEnvironment, fail, migrate, write_atomic};

/// `config` subcommands.
#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Write the commented default files.
    Init(InitArgs),
    /// List the directory and its expected files.
    Path,
    /// Convert the legacy file or per-module `.jsonc` files to TOML.
    Migrate(MigrateArgs),
}

/// Options for `config init`.
#[derive(Args)]
pub struct InitArgs {
    /// Replace planned files that already exist.
    #[arg(short = 'f', long)]
    pub force: bool,
    /// Destination directory, overriding `--config`.
    #[arg(short = 'p', long, value_name = "DIR")]
    pub path: Option<PathBuf>,
    /// Print the preview; write nothing.
    #[arg(long)]
    pub stdout: bool,
}

/// Options for `config migrate`.
#[derive(Args)]
pub struct MigrateArgs {
    /// Source to read instead of discovery: a directory of per-module
    /// `<id>.jsonc` files, one such file, or a legacy single file.
    #[arg(long, value_name = "PATH")]
    pub from: Option<PathBuf>,
    /// Replace planned destination files that already exist.
    #[arg(short = 'f', long)]
    pub force: bool,
}

pub(crate) fn run<W: Write, E: Write>(
    command: ConfigCommand,
    catalog: &ConfigCatalog,
    paths: &ConfigPaths,
    environment: &CliEnvironment,
    out: &mut W,
    err: &mut E,
) -> ExitCode {
    match command {
        ConfigCommand::Init(args) => init(&args, catalog, paths, out, err),
        ConfigCommand::Path => path(catalog, paths, environment, out, err),
        ConfigCommand::Migrate(args) => {
            migrate::run(&args, catalog, paths, &environment.config, out, err)
        }
    }
}

fn init<W: Write, E: Write>(
    args: &InitArgs,
    catalog: &ConfigCatalog,
    paths: &ConfigPaths,
    out: &mut W,
    err: &mut E,
) -> ExitCode {
    let directory = args.path.as_deref().unwrap_or_else(|| paths.directory());
    let templates = templates(catalog);
    if args.stdout {
        let blocks: Vec<String> = templates
            .iter()
            .map(|(name, template)| format!("# file: {name}\n{template}"))
            .collect();
        let _ = write!(out, "{}", blocks.join("\n"));
        return ExitCode::SUCCESS;
    }
    let collisions = collisions(directory, &templates);
    if !args.force && !collisions.is_empty() {
        let names: Vec<String> = collisions
            .iter()
            .map(|path| path.file_name().unwrap_or_default().to_string_lossy().into_owned())
            .collect();
        return fail(
            err,
            &format!(
                "refusing to overwrite existing files: {} (use --force)",
                names.join(", ")
            ),
        );
    }
    if let Err(error) = fs::create_dir_all(directory) {
        return fail(err, &format!("cannot create {}: {error}", directory.display()));
    }
    let total = templates.len();
    for (written, (name, template)) in templates.iter().enumerate() {
        let path = directory.join(name);
        if let Err(error) = write_atomic(&path, template) {
            return fail(
                err,
                &format!(
                    "failed to write {}: {error}; wrote {written} of {total} files (no cross-file transaction)",
                    path.display()
                ),
            );
        }
        let _ = writeln!(out, "wrote {}", path.display());
    }
    ExitCode::SUCCESS
}

fn path<W: Write, E: Write>(
    catalog: &ConfigCatalog,
    paths: &ConfigPaths,
    environment: &CliEnvironment,
    out: &mut W,
    err: &mut E,
) -> ExitCode {
    let report = inspect_paths(catalog, paths);
    let stale = paths.stale_jsonc_files(catalog.ids());
    let legacy = paths.legacy_config_hint(&environment.config, catalog.ids());
    let trust = inspect_catalog_trust(catalog, paths);
    if let Some(status) = remote_status(environment)
        && status.directory == report.directory
    {
        render_core_status(&status, out, err);
        render_stale_hints(&stale, out);
        render_legacy_hint(legacy.as_deref(), out);
        render_trust_warnings(&trust, out);
        return ExitCode::SUCCESS;
    }
    render_path_report(&report, out);
    render_stale_hints(&stale, out);
    render_legacy_hint(legacy.as_deref(), out);
    render_trust_warnings(&trust, out);
    ExitCode::SUCCESS
}

fn remote_status(environment: &CliEnvironment) -> Option<CoreStatus> {
    let paths = environment.app_paths().ok()?;
    let socket = paths.socket_path().ok()?;
    IpcClient::with_options(socket, RetryPolicy::single(), IO_TIMEOUT).status().ok()
}

/// Renders the host's status report with the same markers as the local one.
pub(crate) fn render_core_status<W: Write, E: Write>(
    status: &CoreStatus,
    out: &mut W,
    err: &mut E,
) {
    let _ = writeln!(out, "{}", status.directory.display());
    for file in &status.files {
        let marker = match file.state {
            CoreFileState::Missing => "",
            CoreFileState::Valid if file.active => " (exists, active)",
            CoreFileState::Valid => " (exists, offline)",
            CoreFileState::Invalid => " (exists, invalid)",
        };
        let _ = writeln!(out, "  {}{marker}", file.file);
        if let Some(diagnostic) = &file.diagnostic {
            let _ = writeln!(err, "stache: {diagnostic}");
        }
        for warning in &file.warnings {
            let _ = writeln!(out, "    warning: unknown key `{warning}` is ignored");
        }
    }
    crate::write_access_notice(err, status.access.as_ref());
}

/// Renders the offline `config path` report.
pub(crate) fn render_path_report<W: Write>(report: &PathReport, out: &mut W) {
    let _ = writeln!(out, "{}", report.directory.display());
    for entry in &report.entries {
        let marker = match &entry.state {
            PathState::Missing => String::new(),
            PathState::Valid => " (exists, offline)".to_owned(),
            PathState::Invalid(_) => " (exists, invalid)".to_owned(),
        };
        let _ = writeln!(out, "  {}{marker}", entry.file.file_name());
        if let PathState::Invalid(diagnostic) = &entry.state {
            let _ = writeln!(out, "    {diagnostic}");
        }
        for warning in &entry.warnings {
            let _ = writeln!(out, "    warning: unknown key `{warning}` is ignored");
        }
    }
}

/// Prints one hint per `<id>.jsonc` that has no `<id>.toml` next to it.
fn render_stale_hints<W: Write>(stale: &[String], out: &mut W) {
    for name in stale {
        let _ = writeln!(out, "{}", stale_jsonc_hint(name));
    }
}

/// Prints the one legacy-config note, if any.
fn render_legacy_hint<W: Write>(hint: Option<&str>, out: &mut W) {
    if let Some(hint) = hint {
        let _ = writeln!(out, "{hint}");
    }
}

/// Prints one line per configuration path someone else can write.
fn render_trust_warnings<W: Write>(warnings: &[TrustWarning], out: &mut W) {
    for warning in warnings {
        let _ = writeln!(out, "{warning}");
    }
}

fn templates(catalog: &ConfigCatalog) -> Vec<(String, String)> {
    catalog
        .ids()
        .filter_map(|id| Some((catalog.file_name(id)?, catalog.template(id)?)))
        .collect()
}

fn collisions(directory: &Path, templates: &[(String, String)]) -> Vec<PathBuf> {
    templates
        .iter()
        .map(|(name, _)| directory.join(name))
        .filter(|path| path.exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use stache_sdk::{ConfigError, GlobalConfig, ModuleConfig, ModuleId};

    use super::*;

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, rename_all = "camelCase")]
    struct Sample {
        enabled: bool,
        items: Vec<String>,
    }

    impl ModuleConfig for Sample {
        fn enabled(&self) -> bool { self.enabled }

        fn validate(&self) -> Result<(), ConfigError> { Ok(()) }
    }

    fn catalog() -> ConfigCatalog {
        let mut catalog = ConfigCatalog::new();
        catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
        catalog.register::<Sample>(ModuleId::new("sample")).expect("sample");
        catalog
    }

    fn execute(
        command: ConfigCommand,
        catalog: &ConfigCatalog,
        directory: &Path,
    ) -> (ExitCode, String, String) {
        let paths = ConfigPaths::new(directory);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run(
            command,
            catalog,
            &paths,
            &crate::CliEnvironment::default(),
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).expect("utf-8 stdout"),
            String::from_utf8(err).expect("utf-8 stderr"),
        )
    }

    #[test]
    fn config_path_hints_at_a_jsonc_without_a_toml_and_stays_quiet_otherwise() {
        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("config");
        fs::create_dir_all(&directory).expect("dir");
        fs::write(directory.join("sample.jsonc"), "{}").expect("jsonc");
        let hint = "note: sample.jsonc is ignored; run `stache config migrate`\n";

        let (code, out, err) = execute(ConfigCommand::Path, &catalog(), &directory);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(out.ends_with(hint), "{out}");
        assert_eq!(out.matches("stache config migrate").count(), 1, "{out}");
        assert!(
            out.contains("  sample.toml\n"),
            "the marker line is unchanged: {out}"
        );

        fs::write(directory.join("sample.toml"), "").expect("toml");
        let (code, out, err) = execute(ConfigCommand::Path, &catalog(), &directory);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(!out.contains("migrate"), "{out}");
    }

    #[test]
    fn config_path_prints_trust_warnings_after_entries_and_notes() {
        use std::os::unix::fs::PermissionsExt;
        let chmod = |path: &Path, mode: u32| {
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
        };
        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("config");
        fs::create_dir_all(&directory).expect("dir");
        chmod(&directory, 0o755);
        fs::write(directory.join("sample.toml"), "").expect("toml");
        chmod(&directory.join("sample.toml"), 0o644);
        fs::write(directory.join("global.jsonc"), "{}").expect("jsonc");
        let (code, clean, err) = execute(ConfigCommand::Path, &catalog(), &directory);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(!clean.contains("warning:"), "{clean}");

        chmod(&directory, 0o775);
        chmod(&directory.join("sample.toml"), 0o666);
        let (code, out, err) = execute(ConfigCommand::Path, &catalog(), &directory);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        let owner = stache_sdk::config::current_owner();
        let tail = format!(
            "note: global.jsonc is ignored; run `stache config migrate`\n\
             warning: {} is group-writable (mode 0775, owner uid {owner}); Stache runs commands from its configuration, see docs/architecture/config.md#trust-model\n\
             warning: {} is group-writable and world-writable (mode 0666, owner uid {owner}); Stache runs commands from its configuration, see docs/architecture/config.md#trust-model\n",
            fs::canonicalize(&directory).expect("canonical").display(),
            fs::canonicalize(directory.join("sample.toml")).expect("canonical").display()
        );
        assert!(out.ends_with(&tail), "{out}");
        assert_eq!(
            out.strip_suffix(&tail),
            clean.strip_suffix("note: global.jsonc is ignored; run `stache config migrate`\n"),
            "entries and markers are unchanged"
        );
        assert!(!out.contains('\u{1b}'), "no ANSI: {out}");
    }

    #[test]
    fn init_stdout_prints_delimited_templates_in_file_order_without_writing() {
        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("config");
        let (code, out, err) = execute(
            ConfigCommand::Init(InitArgs {
                force: false,
                path: None,
                stdout: true,
            }),
            &catalog(),
            &directory,
        );
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        let global = catalog().template(GlobalConfig::ID).expect("global template");
        let sample = catalog().template(ModuleId::new("sample")).expect("sample template");
        assert_eq!(
            out,
            format!("# file: global.toml\n{global}\n# file: sample.toml\n{sample}")
        );
        assert!(!directory.exists());
    }

    #[test]
    fn init_collisions_change_nothing_without_force() {
        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("config");
        let (code, _out, _err) = execute(
            ConfigCommand::Init(InitArgs {
                force: false,
                path: None,
                stdout: false,
            }),
            &catalog(),
            &directory,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        let sample = directory.join("sample.toml");
        fs::write(&sample, "{ \"enabled\": true }").expect("marker write");
        let sentinel = fs::read_to_string(&sample).expect("marker read");

        let (code, _out, err) = execute(
            ConfigCommand::Init(InitArgs {
                force: false,
                path: None,
                stdout: false,
            }),
            &catalog(),
            &directory,
        );
        assert_eq!(code, ExitCode::FAILURE);
        assert!(err.contains("refusing to overwrite existing files"));
        assert_eq!(fs::read_to_string(&sample).expect("unchanged"), sentinel);
    }

    #[test]
    fn force_replaces_only_the_planned_files() {
        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("config");
        fs::create_dir_all(&directory).expect("dir");
        let unrelated = directory.join("notes.txt");
        fs::write(&unrelated, "keep me").expect("unrelated");

        let (code, _out, err) = execute(
            ConfigCommand::Init(InitArgs {
                force: true,
                path: None,
                stdout: false,
            }),
            &catalog(),
            &directory,
        );
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(fs::read_to_string(&unrelated).expect("unrelated"), "keep me");
        assert!(directory.join("global.toml").exists());
        assert!(directory.join("sample.toml").exists());
    }

    #[test]
    fn force_preserves_the_existing_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("config");
        fs::create_dir_all(&directory).expect("dir");
        let global = directory.join("global.toml");
        fs::write(&global, "{}\n").expect("global");
        fs::set_permissions(&global, fs::Permissions::from_mode(0o644)).expect("mode");

        let (code, _out, err) = execute(
            ConfigCommand::Init(InitArgs {
                force: true,
                path: None,
                stdout: false,
            }),
            &catalog(),
            &directory,
        );
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(
            fs::metadata(&global).expect("metadata").permissions().mode() & 0o777,
            0o644,
            "a forced write must keep the replaced file's mode"
        );
    }

    #[test]
    fn a_per_file_write_failure_is_reported_without_claiming_transactionality() {
        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("config");
        fs::create_dir_all(directory.join("global.toml")).expect("blocking directory");
        let (code, _out, err) = execute(
            ConfigCommand::Init(InitArgs {
                force: true,
                path: None,
                stdout: false,
            }),
            &catalog(),
            &directory,
        );
        assert_eq!(code, ExitCode::FAILURE);
        assert!(
            err.contains("wrote 0 of 2 files (no cross-file transaction)"),
            "{err}"
        );
        assert!(!directory.join("sample.toml").exists());
    }

    #[test]
    fn path_lists_expected_files_with_offline_and_invalid_markers() {
        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("config");
        fs::create_dir_all(&directory).expect("dir");
        fs::write(directory.join("global.toml"), "# defaults\n").expect("global");
        fs::write(directory.join("sample.toml"), "enabled = ").expect("broken");
        let (code, out, err) = execute(ConfigCommand::Path, &catalog(), &directory);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(out.starts_with(&format!("{}\n", directory.display())));
        assert!(out.contains("  global.toml (exists, offline)\n"));
        assert!(out.contains("  sample.toml (exists, invalid)\n"));
        assert!(out.contains(": "));
    }

    #[test]
    fn path_reports_missing_files_without_a_marker() {
        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("config");
        let (code, out, _err) = execute(ConfigCommand::Path, &catalog(), &directory);
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(out.contains("  global.toml\n"));
        assert!(out.contains("  sample.toml\n"));
    }

    #[test]
    fn path_reports_unknown_keys_as_warnings_without_invalidating_the_file() {
        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("config");
        fs::create_dir_all(&directory).expect("dir");
        fs::write(directory.join("global.toml"), "# defaults\n").expect("global");
        fs::write(
            directory.join("sample.toml"),
            "#:schema ./schemas/sample.schema.json\nenabled = true\nenable = true\n",
        )
        .expect("sample");
        let (code, out, err) = execute(ConfigCommand::Path, &catalog(), &directory);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(
            out,
            format!(
                "{}\n  global.toml (exists, offline)\n  sample.toml (exists, offline)\n    warning: unknown key `enable` is ignored\n",
                directory.display()
            )
        );
    }

    #[test]
    fn path_explains_an_empty_file() {
        let root = tempfile::tempdir().expect("temp dir");
        let directory = root.path().join("config");
        fs::create_dir_all(&directory).expect("dir");
        fs::write(directory.join("sample.toml"), " \n\t").expect("empty sample");
        let (code, out, err) = execute(ConfigCommand::Path, &catalog(), &directory);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(out.contains("  sample.toml (exists, invalid)\n"));
        assert!(out.contains("file is empty; add a `#` comment or a key to use defaults"));
    }
}
