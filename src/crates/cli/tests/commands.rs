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
    CoreApplyOutcome, CoreFileOutcome, CoreFileState, CoreFileStatus, CoreHandler, CoreReload,
    CoreStatus, HandlerResult, IpcClient, IpcServer, Router,
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

const fn empty_reload() -> CoreReload { CoreReload { files: Vec::new() } }

fn empty_status(root: &Path) -> CoreStatus {
    CoreStatus {
        directory: root.join(".config").join("stache"),
        files: Vec::new(),
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
            file: "global.jsonc".to_owned(),
            state: CoreFileState::Valid,
            active: false,
            diagnostic: Some("global.jsonc: rejected by host".to_owned()),
            warnings: Vec::new(),
        }],
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
        format!("{}\n  global.jsonc (exists, offline)\n", directory.display())
    );
    assert_eq!(err, "stache: global.jsonc: rejected by host\n");
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
                file: "global.jsonc".to_owned(),
                outcome: CoreApplyOutcome::Applied { revision: 2 },
                diagnostic: None,
            },
            CoreFileOutcome {
                file: "proxyAudio.jsonc".to_owned(),
                outcome: CoreApplyOutcome::Rejected,
                diagnostic: Some("/config/proxyAudio.jsonc: unknown variant `fuzzy`".to_owned()),
            },
        ],
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
        err.contains("stache: /config/proxyAudio.jsonc: unknown variant `fuzzy`"),
        "{err}"
    );
}

#[test]
fn config_path_reports_active_files_from_a_reachable_host() {
    let root = tempfile::tempdir().expect("temp dir");
    let environment = environment(root.path());
    let directory = root.path().join(".config").join("stache");
    std::fs::create_dir_all(&directory).expect("config dir");
    std::fs::write(directory.join("global.jsonc"), "{}").expect("global");
    let status = CoreStatus {
        directory: directory.clone(),
        files: vec![
            CoreFileStatus {
                file: "global.jsonc".to_owned(),
                state: CoreFileState::Valid,
                active: true,
                diagnostic: None,
                warnings: Vec::new(),
            },
            CoreFileStatus {
                file: "proxyAudio.jsonc".to_owned(),
                state: CoreFileState::Missing,
                active: false,
                diagnostic: None,
                warnings: Vec::new(),
            },
        ],
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
    assert!(out.contains("  global.jsonc (exists, active)\n"), "{out}");
    assert!(out.contains("  proxyAudio.jsonc\n"), "{out}");
}

#[test]
fn config_path_falls_back_to_offline_without_a_host() {
    let root = tempfile::tempdir().expect("temp dir");
    let environment = environment(root.path());
    let directory = root.path().join(".config").join("stache");
    std::fs::create_dir_all(&directory).expect("config dir");
    std::fs::write(directory.join("global.jsonc"), "{}").expect("global");
    let (code, out, err) = run(
        &["stache", "config", "path"],
        &config_catalog(),
        &CommandCatalog::new(),
        &environment,
        false,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(out.contains("  global.jsonc (exists, offline)\n"), "{out}");
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
