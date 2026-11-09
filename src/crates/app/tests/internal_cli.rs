//! The re-entered internal CLI path, from the shared runner's rewritten argv to
//! the application's mode dispatch.
//!
//! Nothing here launches a desktop, tray, webview or second host: the desktop
//! selector panics if it is ever called, the process factory only records the
//! spawn request, and the one IPC endpoint is a fake in a temporary directory.

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, ExitStatus};
use std::sync::{Arc, Mutex};

use stache_app::dispatch;
use stache_cli::CliEnvironment;
use stache_sdk::commands::{
    ChildProcess, CommandEnvironment, CommandExecutor, INTERNAL_CLI_FLAG, ProcessFactory,
    SequenceOutcome, SpawnSpec, SystemClock,
};
use stache_sdk::config::ConfigEnvironment;
use stache_sdk::ipc::{CoreHandler, CoreReload, CoreStatus, HandlerResult, IpcServer, Router};
use stache_sdk::{AppEnvironment, AppPaths, CancellationToken};

struct Recorder(Mutex<Vec<SpawnSpec>>);

struct Instant;

impl ChildProcess for Instant {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> { Ok(Some(ExitStatus::from_raw(0))) }

    fn kill(&mut self) -> io::Result<()> { Ok(()) }

    fn read_stdout(&mut self, _: &mut [u8]) -> io::Result<usize> { Ok(0) }

    fn read_stderr(&mut self, _: &mut [u8]) -> io::Result<usize> { Ok(0) }
}

impl ProcessFactory for Recorder {
    fn spawn(&self, spec: &SpawnSpec) -> io::Result<Box<dyn ChildProcess>> {
        self.0.lock().expect("recorder").push(spec.clone());
        Ok(Box::new(Instant))
    }
}

/// The two executable identities the contract covers.
fn identities(root: &Path) -> Vec<PathBuf> {
    let paths = [
        root.join("bin").join("stache"),
        root.join("Stache Next.app").join("Contents").join("MacOS").join("stache"),
    ];
    for path in &paths {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        std::fs::write(path, "#!/bin/sh\n").expect("executable");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    paths.to_vec()
}

fn environment(home: &Path) -> CliEnvironment {
    CliEnvironment::new(
        ConfigEnvironment {
            xdg_config_home: None,
            home: Some(home.to_path_buf()),
        },
        AppEnvironment {
            home: Some(home.to_path_buf()),
            temporary: Some(home.to_path_buf()),
        },
    )
}

/// Runs a configured command line through the shared runner and returns the
/// argv the spawned child would receive.
fn child_argv(command: &str, executable: &Path) -> Vec<OsString> {
    let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
    let executor = CommandExecutor::with_parts(
        Arc::clone(&recorder) as Arc<dyn ProcessFactory>,
        Arc::new(SystemClock::default()),
        CommandEnvironment {
            next_executable: Some(executable.to_path_buf()),
            ..CommandEnvironment::default()
        },
    );
    let cancel = CancellationToken::new();
    let outcome = executor.run_sequence(&[command.to_owned()], &cancel, &cancel);
    assert!(matches!(outcome, SequenceOutcome::Completed), "{outcome:?}");
    let specs = recorder.0.lock().expect("recorder");
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].program, executable, "the running executable is spawned");
    // The OS supplies argv[0]; the program path stands in for it here.
    std::iter::once(executable.as_os_str().to_owned())
        .chain(specs[0].arguments.iter().map(OsString::from))
        .collect()
}

struct Output {
    code: ExitCode,
    out: String,
    err: String,
}

fn run(args: &[OsString], executable: &Path, home: &Path) -> Output {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = dispatch(
        args.iter().cloned(),
        executable,
        &environment(home),
        false,
        &mut out,
        &mut err,
        |_| panic!("the internal CLI path must never start the desktop"),
    );
    Output {
        code,
        out: String::from_utf8(out).expect("utf-8"),
        err: String::from_utf8(err).expect("utf-8"),
    }
}

fn args(list: &[&str]) -> Vec<OsString> { list.iter().map(OsString::from).collect() }

#[test]
fn the_rewritten_child_argv_executes_cli_only_for_both_executable_identities() {
    let root = tempfile::tempdir().expect("root");
    for executable in identities(root.path()) {
        let argv = child_argv("stache schema keybindings", &executable);
        assert_eq!(argv[1], INTERNAL_CLI_FLAG);
        let result = run(&argv, &executable, root.path());
        assert_eq!(
            result.code,
            ExitCode::SUCCESS,
            "{}: {}",
            executable.display(),
            result.err
        );
        assert!(
            result.out.contains("/schemas/keybindings.schema.json"),
            "{}: {}",
            executable.display(),
            result.out
        );
    }
}

#[test]
fn explicit_config_arguments_and_empty_arguments_survive_the_rewrite() {
    let root = tempfile::tempdir().expect("root");
    let selected = root.path().join("selected dir");
    for executable in identities(root.path()) {
        let command = format!("stache --config '{}' config path", selected.display());
        let argv = child_argv(&command, &executable);
        let result = run(&argv, &executable, root.path());
        assert_eq!(result.code, ExitCode::SUCCESS, "{}", result.err);
        assert!(
            result.out.starts_with(&format!("{}\n", selected.display())),
            "{}",
            result.out
        );
    }
    // An empty argument is preserved in order, and then rejected by the parser.
    let executable = identities(root.path()).remove(0);
    let argv = child_argv(r#"stache schema "" "#, &executable);
    assert_eq!(argv[1..], [INTERNAL_CLI_FLAG, "schema", ""].map(OsString::from));
}

struct FakeCore;

impl CoreHandler for FakeCore {
    fn reload(&self) -> HandlerResult<CoreReload> {
        Ok(CoreReload {
            files: Vec::new(),
            access: None,
        })
    }

    fn status(&self) -> HandlerResult<CoreStatus> {
        Ok(CoreStatus {
            directory: PathBuf::new(),
            files: Vec::new(),
            access: None,
        })
    }
}

#[test]
fn reload_works_through_the_internal_path_against_a_fake_host() {
    let root = tempfile::tempdir().expect("root");
    let paths = AppPaths::resolve(&environment(root.path()).app).expect("app paths");
    let server = IpcServer::start(&paths, Arc::new(Router::new(Arc::new(FakeCore))))
        .expect("fake IPC endpoint");
    for executable in identities(root.path()) {
        let argv = child_argv("stache reload", &executable);
        let result = run(&argv, &executable, root.path());
        assert_eq!(
            result.code,
            ExitCode::SUCCESS,
            "{}: {}",
            executable.display(),
            result.err
        );
    }
    drop(server);
    // Without a host the real exit status and message propagate; it is never an
    // instance-lock desktop success.
    for executable in identities(root.path()) {
        let argv = child_argv("stache reload", &executable);
        let result = run(&argv, &executable, root.path());
        assert_eq!(result.code, ExitCode::FAILURE);
        assert!(
            result.err.contains("Stache app is not running"),
            "{}",
            result.err
        );
    }
}

#[test]
fn invalid_requests_are_usage_errors_and_never_desktop_work() {
    let root = tempfile::tempdir().expect("root");
    for executable in identities(root.path()) {
        for (list, needle) in [
            (&["stache", "--stache-internal-cli"][..], "requires a command"),
            (
                &["stache", "--stache-internal-cli", "--desktop", "reload"],
                "--desktop",
            ),
            (
                &["stache", "--desktop", "--stache-internal-cli", "reload"],
                "--desktop",
            ),
            (&["stache", "--desktop", "--stache-internal-cli"], "--desktop"),
            (
                &[
                    "stache",
                    "--stache-internal-cli",
                    "--stache-internal-cli",
                    "reload",
                ],
                "cannot be used multiple times",
            ),
            (
                &["stache", "reload", "--stache-internal-cli"],
                "--stache-internal-cli",
            ),
            (
                &["stache", "--stache-internal-cli", "--bogus", "reload"],
                "--bogus",
            ),
            (
                &[
                    "stache",
                    "--stache-internal-cli",
                    "tiling",
                    "window",
                    "--swap",
                    "prev",
                ],
                "invalid value 'prev'",
            ),
            (
                &[
                    "stache",
                    "--stache-internal-cli",
                    "wallpaper",
                    "set",
                    "--random",
                    "--screen",
                    "two",
                ],
                "Invalid screen value 'two'",
            ),
        ] {
            let result = run(&args(list), &executable, root.path());
            assert_eq!(
                result.code,
                ExitCode::from(2),
                "{list:?}: {}{}",
                result.out,
                result.err
            );
            assert!(result.err.contains(needle), "{list:?}: {}", result.err);
            assert!(result.out.is_empty(), "{list:?}");
        }
        // A literal marker after `--` is data for the parser, not a second flag.
        let result = run(
            &args(&[
                "stache",
                "--stache-internal-cli",
                "schema",
                "--",
                "--stache-internal-cli",
            ]),
            &executable,
            root.path(),
        );
        assert_ne!(
            result.code,
            ExitCode::SUCCESS,
            "extra schema arguments are rejected"
        );
        assert!(
            !result.err.contains("cannot be used multiple times"),
            "the marker after `--` was parsed as a flag: {}",
            result.err
        );
        assert!(
            result.err.contains("unknown schema `--stache-internal-cli`"),
            "the marker reached `schema` as data: {}",
            result.err
        );
    }
}

#[test]
fn help_and_version_keep_parser_precedence_with_the_flag() {
    let root = tempfile::tempdir().expect("root");
    for executable in identities(root.path()) {
        for list in [
            &["stache", "--stache-internal-cli", "--help"][..],
            &["stache", "--stache-internal-cli", "--version"],
            &["stache", "--stache-internal-cli", "schema", "--help"],
        ] {
            let result = run(&args(list), &executable, root.path());
            assert_eq!(result.code, ExitCode::SUCCESS, "{list:?}: {}", result.err);
            assert!(!result.out.is_empty());
            assert!(!result.out.contains("internal-cli"), "the flag stays hidden");
        }
        let help = run(
            &args(&["stache", "--stache-internal-cli", "--help"]),
            &executable,
            root.path(),
        );
        assert!(help.out.contains("Usage"), "{}", help.out);
    }
}

#[test]
fn ordinary_bundle_and_direct_precedence_is_unchanged_without_the_flag() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let root = tempfile::tempdir().expect("root");
    let [standalone, bundle] = <[PathBuf; 2]>::try_from(identities(root.path())).expect("two");
    let starts = AtomicUsize::new(0);
    let go = |list: &[&str], executable: &Path| {
        dispatch(
            list.iter().map(OsString::from),
            executable,
            &environment(root.path()),
            false,
            &mut Vec::new(),
            &mut Vec::new(),
            |_| {
                starts.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
    };
    assert_eq!(go(&["stache", "schema"], &bundle), ExitCode::SUCCESS);
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "a bundled command still selects the desktop"
    );
    assert_eq!(go(&["stache", "schema"], &standalone), ExitCode::SUCCESS);
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "a direct command stays CLI-only"
    );
    assert_eq!(go(&["stache", "--desktop"], &standalone), ExitCode::SUCCESS);
    assert_eq!(go(&["stache"], &standalone), ExitCode::SUCCESS);
    assert_eq!(starts.load(Ordering::SeqCst), 3);
}
