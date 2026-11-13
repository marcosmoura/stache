//! End-to-end CLI behavior over a real socket.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Arg, ArgMatches, Command};
use stache_cli::{CliEnvironment, run_with_io};
use stache_sdk::config::ConfigEnvironment;
use stache_sdk::ipc::{
    CoreAccess, CoreAccessState, CoreApplyOutcome, CoreFileOutcome, CoreFileState, CoreFileStatus,
    CoreHandler, CoreReload, CoreStatus, HandlerResult, IpcClient, IpcServer, Router,
};
use stache_sdk::{
    AppEnvironment, CliContribution, CliExecutor, CommandCatalog, ConfigCatalog, GlobalConfig,
};

struct FixedCore {
    reload: CoreReload,
    status: CoreStatus,
}

impl CoreHandler for FixedCore {
    fn reload(&self) -> HandlerResult<CoreReload> { Ok(self.reload.clone()) }

    fn status(&self) -> HandlerResult<CoreStatus> { Ok(self.status.clone()) }
}

fn environment(root: &Path) -> CliEnvironment {
    CliEnvironment::new(
        ConfigEnvironment {
            xdg_config_home: None,
            home: Some(root.to_path_buf()),
        },
        AppEnvironment {
            home: Some(root.to_path_buf()),
            temporary: Some(root.to_path_buf()),
        },
    )
}

fn config_catalog() -> ConfigCatalog {
    let mut catalog = ConfigCatalog::new();
    catalog.register::<GlobalConfig>(GlobalConfig::ID).expect("global");
    catalog
}

fn run(
    arguments: &[&str],
    catalog: &ConfigCatalog,
    commands: &CommandCatalog,
    environment: &CliEnvironment,
    ansi: bool,
) -> (ExitCode, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_with_io(
        arguments.iter().map(OsString::from),
        catalog,
        commands,
        environment,
        ansi,
        &mut out,
        &mut err,
    );
    (
        code,
        String::from_utf8(out).expect("utf-8 stdout"),
        String::from_utf8(err).expect("utf-8 stderr"),
    )
}

fn server(environment: &CliEnvironment, reload: CoreReload, status: CoreStatus) -> IpcServer {
    let paths = environment.app_paths().expect("paths");
    let router = Router::new(Arc::new(FixedCore { reload, status }));
    IpcServer::start(&paths, Arc::new(router)).expect("server")
}

const fn empty_reload() -> CoreReload {
    CoreReload {
        files: Vec::new(),
        access: None,
    }
}

fn empty_status(root: &Path) -> CoreStatus {
    CoreStatus {
        directory: root.join(".config").join("stache"),
        files: Vec::new(),
        access: None,
    }
}

#[test]
fn host_path_diagnostics_go_to_stderr_without_changing_path_markers() {
    let root = tempfile::tempdir().expect("root");
    let environment = environment(root.path());
    let directory = root.path().join(".config/stache");
    let _server = server(&environment, empty_reload(), CoreStatus {
        directory: directory.clone(),
        files: vec![CoreFileStatus {
            file: "global.toml".to_owned(),
            state: CoreFileState::Valid,
            active: false,
            diagnostic: Some("global.toml: rejected by host".to_owned()),
            warnings: Vec::new(),
        }],
        access: None,
    });
    let (code, out, err) = run(
        &["stache", "config", "path"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment,
        false,
    );
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        format!("{}\n  global.toml (exists, offline)\n", directory.display())
    );
    assert_eq!(err, "stache: global.toml: rejected by host\n");
}

#[test]
fn cache_clear_refuses_parent_aliases_into_the_runtime() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("home");
    let temporary = root.path().join("temporary");
    std::fs::create_dir_all(home.join("Library")).expect("home");
    std::fs::create_dir_all(&temporary).expect("temporary");
    std::os::unix::fs::symlink(&temporary, home.join("Library/Caches")).expect("alias");
    let runtime = temporary.join(stache_sdk::IDENTITY);
    std::fs::create_dir(&runtime).expect("runtime");
    std::fs::write(runtime.join("sentinel"), "keep").expect("sentinel");
    let environment = CliEnvironment::new(ConfigEnvironment::default(), AppEnvironment {
        home: Some(home),
        temporary: Some(temporary),
    });
    let (code, _, err) = run(
        &["stache", "cache", "clear"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment,
        false,
    );
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains("overlaps"), "{err}");
    assert_eq!(
        std::fs::read_to_string(runtime.join("sentinel")).expect("untouched"),
        "keep"
    );
}

struct FakeExecutor;

impl CliExecutor for FakeExecutor {
    fn execute(
        &self,
        matches: &ArgMatches,
        _ansi: bool,
        out: &mut dyn Write,
        _: &mut dyn Write,
    ) -> ExitCode {
        let value = matches.get_one::<String>("value").map_or("none", String::as_str);
        let _ = writeln!(out, "fake ran {value}");
        ExitCode::SUCCESS
    }
}

fn fake_catalog() -> CommandCatalog {
    let mut catalog = CommandCatalog::new();
    catalog
        .add(CliContribution::new(
            || Command::new("fake").arg(Arg::new("value")),
            FakeExecutor,
        ))
        .expect("contribute");
    catalog
}

#[test]
fn a_contributed_command_executes_through_the_generic_runner() {
    let root = tempfile::tempdir().expect("temp dir");
    let (code, out, err) = run(
        &["stache", "fake", "payload"],
        &config_catalog(),
        &fake_catalog(),
        &environment(root.path()),
        false,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(out, "fake ran payload\n");
}

#[test]
fn a_contribution_cannot_shadow_a_builtin_command() {
    let root = tempfile::tempdir().expect("temp dir");
    let mut catalog = CommandCatalog::new();
    catalog
        .add(CliContribution::new(|| Command::new("schema"), FakeExecutor))
        .expect("contribute");
    let (code, out, err) = run(
        &["stache", "schema"],
        &config_catalog(),
        &catalog,
        &environment(root.path()),
        false,
    );
    assert_eq!(code, ExitCode::FAILURE);
    assert!(out.is_empty());
    assert!(err.contains("collides with a built-in command"), "{err}");
}

#[test]
fn completions_include_contributed_commands() {
    let root = tempfile::tempdir().expect("temp dir");
    let (code, out, err) = run(
        &["stache", "completions", "-s", "zsh"],
        &config_catalog(),
        &fake_catalog(),
        &environment(root.path()),
        false,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.starts_with("#compdef stache"));
    assert!(out.contains("fake"));
    assert!(out.contains("reload"));
    assert!(!out.contains("tiling"));
    assert!(!out.contains("wallpaper"));
}

#[test]
fn reload_without_a_running_host_exits_one() {
    let root = tempfile::tempdir().expect("temp dir");
    let (code, out, err) = run(
        &["stache", "reload"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment(root.path()),
        false,
    );
    assert_eq!(code, ExitCode::FAILURE);
    assert!(out.is_empty());
    assert_eq!(
        err,
        "stache: IPC error: Failed to send reload command to Stache app: Stache app is not running\n"
    );
}

#[test]
fn a_successful_reload_is_silent() {
    let root = tempfile::tempdir().expect("temp dir");
    let environment = environment(root.path());
    let _server = server(&environment, empty_reload(), empty_status(root.path()));
    let (code, out, err) = run(
        &["stache", "reload"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment,
        false,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.is_empty());
    assert!(err.is_empty());
}

#[test]
fn a_rejected_file_is_reported_and_exits_one() {
    let root = tempfile::tempdir().expect("temp dir");
    let environment = environment(root.path());
    let reload = CoreReload {
        files: vec![
            CoreFileOutcome {
                file: "global.toml".to_owned(),
                outcome: CoreApplyOutcome::Applied { revision: 2 },
                diagnostic: None,
            },
            CoreFileOutcome {
                file: "proxyAudio.toml".to_owned(),
                outcome: CoreApplyOutcome::Rejected,
                diagnostic: Some("/config/proxyAudio.toml: unknown variant `fuzzy`".to_owned()),
            },
        ],
        access: None,
    };
    let _server = server(&environment, reload, empty_status(root.path()));
    let (code, out, err) = run(
        &["stache", "reload"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment,
        false,
    );
    assert_eq!(code, ExitCode::FAILURE);
    assert!(out.is_empty());
    assert!(
        err.contains("stache: /config/proxyAudio.toml: unknown variant `fuzzy`"),
        "{err}"
    );
}

#[test]
fn config_path_reports_active_files_from_a_reachable_host() {
    let root = tempfile::tempdir().expect("temp dir");
    let environment = environment(root.path());
    let directory = root.path().join(".config").join("stache");
    std::fs::create_dir_all(&directory).expect("config dir");
    std::fs::write(directory.join("global.toml"), "# defaults\n").expect("global");
    let status = CoreStatus {
        directory: directory.clone(),
        files: vec![
            CoreFileStatus {
                file: "global.toml".to_owned(),
                state: CoreFileState::Valid,
                active: true,
                diagnostic: None,
                warnings: Vec::new(),
            },
            CoreFileStatus {
                file: "proxyAudio.toml".to_owned(),
                state: CoreFileState::Missing,
                active: false,
                diagnostic: None,
                warnings: Vec::new(),
            },
        ],
        access: None,
    };
    let _server = server(&environment, empty_reload(), status);
    let (code, out, err) = run(
        &["stache", "config", "path"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment,
        false,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.starts_with(&format!("{}\n", directory.display())), "{out}");
    assert!(out.contains("  global.toml (exists, active)\n"), "{out}");
    assert!(out.contains("  proxyAudio.toml\n"), "{out}");
}

#[test]
fn config_path_falls_back_to_offline_without_a_host() {
    let root = tempfile::tempdir().expect("temp dir");
    let environment = environment(root.path());
    let directory = root.path().join(".config").join("stache");
    std::fs::create_dir_all(&directory).expect("config dir");
    std::fs::write(directory.join("global.toml"), "# defaults\n").expect("global");
    let (code, out, err) = run(
        &["stache", "config", "path"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment,
        false,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("  global.toml (exists, offline)\n"), "{out}");
}

#[test]
fn cache_clear_leaves_a_live_host_reachable() {
    let root = tempfile::tempdir().expect("temp dir");
    let environment = environment(root.path());
    let paths = environment.app_paths().expect("paths");
    let cache = paths.cache_directory().to_path_buf();
    std::fs::create_dir_all(cache.join("media")).expect("cache");
    std::fs::write(cache.join("media").join("art.bin"), [0_u8; 10]).expect("file");
    let _server = server(&environment, empty_reload(), empty_status(root.path()));

    let (code, out, err) = run(
        &["stache", "cache", "clear"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment,
        false,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(out, "Cache cleared successfully. Freed 10 bytes.\n");
    assert!(!cache.exists());
    assert!(paths.runtime_directory().exists());

    let client = IpcClient::new(paths.socket_path().expect("socket"));
    assert_eq!(
        client.status().expect("live host").directory,
        empty_status(root.path()).directory
    );
}

#[test]
fn schema_output_is_ansi_free_when_piped() {
    let root = tempfile::tempdir().expect("temp dir");
    let (code, out, err) = run(
        &["stache", "schema"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment(root.path()),
        false,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(!out.contains('\x1b'));
    serde_json::from_str::<serde_json::Value>(&out).expect("valid JSON");
    assert!(!std::path::Path::new(&root.path().join("com.marcosmoura.stache.next")).exists());
}

#[test]
fn schema_output_is_styled_only_on_a_terminal() {
    let root = tempfile::tempdir().expect("temp dir");
    let (code, out, err) = run(
        &["stache", "schema"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment(root.path()),
        true,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("\x1b[36m"), "keys should be styled");
    assert_eq!(strip_ansi(&out), {
        let (_, plain, _) = run(
            &["stache", "schema"],
            &config_catalog(),
            &CommandCatalog::new(),
            &environment(root.path()),
            false,
        );
        plain
    });
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::new();
    let mut characters = input.chars();
    while let Some(character) = characters.next() {
        if character == '\x1b' {
            for next in characters.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            output.push(character);
        }
    }
    output
}

#[test]
fn cache_path_is_a_bare_path() {
    let root = tempfile::tempdir().expect("temp dir");
    let (code, out, err) = run(
        &["stache", "cache", "path"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment(root.path()),
        false,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    let expected: PathBuf = root
        .path()
        .canonicalize()
        .expect("canonical home")
        .join("Library")
        .join("Caches")
        .join("com.marcosmoura.stache.next");
    assert_eq!(out, format!("{}\n", expected.display()));
}

#[test]
fn an_unexpected_host_payload_is_reported() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;

    let root = tempfile::tempdir().expect("temp dir");
    let environment = environment(root.path());
    let paths = environment.app_paths().expect("paths");
    std::fs::create_dir_all(paths.runtime_directory()).expect("runtime");
    let socket = paths.socket_path().expect("socket");
    let listener = UnixListener::bind(&socket).expect("bind");
    let responder = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut line = String::new();
        let _ = reader.read_line(&mut line);
        let _ = stream.write_all(b"{\"status\":\"ok\",\"data\":[]}\n");
    });
    let (code, out, err) = run(
        &["stache", "reload"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment,
        false,
    );
    responder.join().expect("responder");
    assert_eq!(code, ExitCode::FAILURE);
    assert!(out.is_empty());
    assert!(err.contains("invalid response"), "{err}");
}

const NOTICE: &str = "stache: modules are waiting for Accessibility permission\n";

const fn waiting(state: CoreAccessState) -> CoreAccess { CoreAccess { state, revoked: false } }

fn rejected_file() -> CoreFileOutcome {
    CoreFileOutcome {
        file: "proxyAudio.toml".to_owned(),
        outcome: CoreApplyOutcome::Rejected,
        diagnostic: Some("/config/proxyAudio.toml: bad value".to_owned()),
    }
}

fn reload_with(
    access: Option<CoreAccess>,
    files: Vec<CoreFileOutcome>,
) -> (ExitCode, String, String) {
    let root = tempfile::tempdir().expect("temp dir");
    let environment = environment(root.path());
    let _server = server(
        &environment,
        CoreReload { files, access },
        empty_status(root.path()),
    );
    run(
        &["stache", "reload"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment,
        false,
    )
}

#[test]
fn reload_prints_the_accessibility_notice_when_blocked_or_checking_and_exits_zero() {
    for state in [CoreAccessState::Blocked, CoreAccessState::Checking] {
        let (code, out, err) = reload_with(Some(waiting(state)), Vec::new());
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(out.is_empty());
        assert_eq!(err, NOTICE);
    }
}

#[test]
fn reload_prints_no_notice_when_open_or_unreported() {
    for access in [Some(waiting(CoreAccessState::Open)), None] {
        let (code, out, err) = reload_with(access, Vec::new());
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert!(out.is_empty());
        assert!(err.is_empty(), "{err}");
    }
}

#[test]
fn a_rejected_file_with_a_blocked_gate_exits_one_and_prints_both_diagnostics_first() {
    let (code, out, err) =
        reload_with(Some(waiting(CoreAccessState::Blocked)), vec![rejected_file()]);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(out.is_empty());
    assert_eq!(
        err,
        format!("stache: /config/proxyAudio.toml: bad value\n{NOTICE}")
    );
}

#[test]
fn config_path_prints_the_notice_after_the_host_diagnostics() {
    let root = tempfile::tempdir().expect("root");
    let environment = environment(root.path());
    let directory = root.path().join(".config/stache");
    let status = CoreStatus {
        directory: directory.clone(),
        files: vec![CoreFileStatus {
            file: "global.toml".to_owned(),
            state: CoreFileState::Valid,
            active: false,
            diagnostic: Some("global.toml: rejected by host".to_owned()),
            warnings: Vec::new(),
        }],
        access: Some(waiting(CoreAccessState::Blocked)),
    };
    let _server = server(&environment, empty_reload(), status);
    let (code, out, err) = run(
        &["stache", "config", "path"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment,
        false,
    );
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        format!("{}\n  global.toml (exists, offline)\n", directory.display())
    );
    assert_eq!(err, format!("stache: global.toml: rejected by host\n{NOTICE}"));
}

/// Every file under a directory with its bytes, in path order.
fn snapshot(directory: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            files.extend(snapshot(&path));
        } else {
            files.push((path.clone(), std::fs::read(&path).expect("bytes")));
        }
    }
    files.sort();
    files
}

fn config_path(root: &Path) -> String {
    let (code, out, err) = run(
        &["stache", "config", "path"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment(root),
        false,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    out
}

fn legacy_lines(out: &str) -> Vec<&str> {
    out.lines().filter(|line| line.contains("is a legacy config")).collect()
}

#[test]
fn config_path_names_a_legacy_source_without_touching_it() {
    let root = tempfile::tempdir().expect("temp dir");
    let directory = root.path().join(".config").join("stache");
    std::fs::create_dir_all(&directory).expect("config dir");
    let source = directory.join("config.jsonc");
    std::fs::write(&source, "{ \"bar\": { /* legacy */ } }\n").expect("legacy");
    let before = snapshot(root.path());

    let out = config_path(root.path());
    assert_eq!(legacy_lines(&out), [format!(
        "note: {} is a legacy config and is not read; run `stache config migrate`",
        source.display()
    )]);
    assert_eq!(
        snapshot(root.path()),
        before,
        "source and directory are byte-identical"
    );
}

#[test]
fn config_path_names_from_when_module_jsonc_files_are_also_present() {
    let root = tempfile::tempdir().expect("temp dir");
    let directory = root.path().join(".config").join("stache");
    std::fs::create_dir_all(&directory).expect("config dir");
    let source = directory.join("config.jsonc");
    std::fs::write(&source, "{}\n").expect("legacy");
    std::fs::write(directory.join("global.jsonc"), "{}\n").expect("module jsonc");
    let before = snapshot(root.path());

    let out = config_path(root.path());
    assert_eq!(legacy_lines(&out), [format!(
        "note: {0} is a legacy config and is not read; run `stache config migrate --from {0}`",
        source.display()
    )]);
    assert!(
        out.contains("note: global.jsonc is ignored; run `stache config migrate`\n"),
        "the stale hint stays as it is: {out}"
    );
    assert_eq!(snapshot(root.path()), before);
}

#[test]
fn config_path_has_no_legacy_note_without_a_source_or_with_global_toml() {
    let root = tempfile::tempdir().expect("temp dir");
    let directory = root.path().join(".config").join("stache");
    std::fs::create_dir_all(&directory).expect("config dir");
    assert!(legacy_lines(&config_path(root.path())).is_empty(), "no source");

    std::fs::write(directory.join("config.jsonc"), "{}\n").expect("legacy");
    std::fs::write(directory.join("global.toml"), "# defaults\n").expect("global");
    let before = snapshot(root.path());
    assert!(
        legacy_lines(&config_path(root.path())).is_empty(),
        "global.toml exists"
    );
    assert_eq!(snapshot(root.path()), before);
}
