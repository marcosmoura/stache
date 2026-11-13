//! Configuration trust warnings over a dotfiles-style symlinked layout.
//!
//! A temp home's `~/.config/stache` is a symlink into a temp dotfiles tree.
//! The real `config path` entry point and a real host startup (Keybindings
//! over its testing rig, fake teardown and command runner) show that ordinary
//! modes warn about nothing, that writable paths warn without refusing any
//! file, and that the host logs the warnings once per startup.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use stache_app::bindings::{HostStatus, ModuleState};
use stache_app::host::{
    HostServices, HostSettings, HostUi, KeybindingsFactory, ModuleFactories, new_host,
};
use stache_app::permissions::PermissionPoll;
use stache_app::shutdown::{
    ExitProcess, InstanceOwner, RecordDelivery, SystemClock, Teardown, TeardownRecordChannel,
    TerminalAction, TerminalDriver, TerminalProgress,
};
use stache_app::startup::CommandRunner;
use stache_cli::CliEnvironment;
use stache_keybindings::RuntimeState;
use stache_keybindings::test_support::Rig;
use stache_sdk::config::{ConfigEnvironment, current_owner};
use stache_sdk::{
    AppEnvironment, AppPaths, CommandCatalog, ConfigPaths, DiscardLogger, EventBus, LogFilter,
    LogLevel, LogRecord, Logger, MainThreadExecutor, Permission, PermissionError,
    PermissionProvider, PermissionStatus, ThreadError, UnavailableWindows,
};

const WAIT: Duration = Duration::from_secs(10);
const SUFFIX: &str =
    "Stache runs commands from its configuration, see docs/architecture/config.md#trust-model";

struct InlineMain;

impl MainThreadExecutor for InlineMain {
    fn is_main_thread(&self) -> bool { true }

    fn execute(&self, job: Box<dyn FnOnce() + Send + 'static>) -> Result<(), ThreadError> {
        job();
        Ok(())
    }
}

struct AllowAll;

impl PermissionProvider for AllowAll {
    fn status(&self, _: Permission) -> PermissionStatus { PermissionStatus::Granted }

    fn request(&self, _: Permission) -> Result<PermissionStatus, PermissionError> {
        Ok(PermissionStatus::Granted)
    }
}

/// Records startup commands instead of spawning them.
#[derive(Default)]
struct RecordingCommands(Mutex<Vec<Vec<String>>>);

impl CommandRunner for RecordingCommands {
    fn run(&self, command: &[String], _: &stache_sdk::CancellationToken) -> Result<(), String> {
        self.0.lock().expect("commands").push(command.to_vec());
        Ok(())
    }
}

#[derive(Default)]
struct CaptureLogger(Mutex<Vec<LogRecord>>);

impl Logger for CaptureLogger {
    fn log(&self, record: LogRecord) { self.0.lock().expect("records").push(record); }
}

impl CaptureLogger {
    fn trust(&self) -> Vec<(LogLevel, String)> {
        self.0
            .lock()
            .expect("records")
            .iter()
            .filter(|record| record.message.contains("#trust-model"))
            .map(|record| (record.level, record.message.clone()))
            .collect()
    }
}

struct NoUi;

impl HostUi for NoUi {
    fn status(&self, _: &HostStatus) {}

    fn config_changed(&self, _: &[String]) {}
}

struct NoRecords;

impl TeardownRecordChannel for NoRecords {
    fn record(&self, _: &str, _: Duration) -> RecordDelivery { RecordDelivery::Delivered }
}

struct ImmediateTerminal;

impl TerminalDriver for ImmediateTerminal {
    fn dispatch(&self, _: TerminalAction) -> TerminalProgress {
        let progress = TerminalProgress::new();
        let _ = progress.claim(Instant::now());
        progress.mark_delivered();
        progress
    }
}

struct NoExit;

impl ExitProcess for NoExit {
    fn exit(&self, _: i32) {}
}

struct NoInstance;

impl InstanceOwner for NoInstance {
    fn release(&self) {}
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

fn real(path: &Path) -> PathBuf { fs::canonicalize(path).expect("canonical") }

fn warning(path: &Path, reason: &str, mode: &str) -> String {
    format!(
        "warning: {} is {reason} (mode {mode}, owner uid {}); {SUFFIX}",
        path.display(),
        current_owner()
    )
}

/// A temp home whose `~/.config/stache` links to `dotfiles/stache`.
struct Home {
    root: tempfile::TempDir,
    dotfiles: PathBuf,
}

impl Home {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("home");
        chmod(root.path(), 0o755);
        let dotfiles = root.path().join("dotfiles").join("stache");
        fs::create_dir_all(&dotfiles).expect("dotfiles");
        chmod(&root.path().join("dotfiles"), 0o755);
        chmod(&dotfiles, 0o755);
        let config_home = root.path().join(".config");
        fs::create_dir(&config_home).expect(".config");
        chmod(&config_home, 0o755);
        symlink(&dotfiles, config_home.join("stache")).expect("symlink");
        for (name, text) in [
            ("global.toml", "exec_on_startup = [\"/bin/sh startup-hook\"]\n"),
            (
                "keybindings.toml",
                "enabled = true\nbindings = { \"Cmd+Q\" = \"/bin/sh hold-q\" }\n",
            ),
            ("tiling.toml", "enabled = false\n"),
        ] {
            fs::write(dotfiles.join(name), text).expect("write");
            chmod(&dotfiles.join(name), 0o644);
        }
        Self { root, dotfiles }
    }

    fn cli_environment(&self) -> CliEnvironment {
        CliEnvironment::new(
            ConfigEnvironment {
                xdg_config_home: None,
                home: Some(self.root.path().to_path_buf()),
            },
            self.app_environment(),
        )
    }

    fn app_environment(&self) -> AppEnvironment {
        AppEnvironment {
            home: Some(self.root.path().to_path_buf()),
            temporary: Some(self.root.path().to_path_buf()),
        }
    }

    fn config_path(&self) -> (ExitCode, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = stache_cli::run_with_io(
            ["stache", "config", "path"].map(OsString::from),
            &stache_app::catalog::catalog().expect("catalog"),
            &CommandCatalog::new(),
            &self.cli_environment(),
            false,
            &mut out,
            &mut err,
        );
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
        (code, String::from_utf8(out).expect("stdout"))
    }

    /// Starts a real host, waits for readiness and returns it running.
    fn start_host(&self) -> RunningHost {
        let records = Arc::new(NoRecords);
        let teardown = Teardown::new(
            Arc::new(SystemClock),
            records as Arc<dyn TeardownRecordChannel>,
            Arc::new(NoInstance) as Arc<dyn InstanceOwner>,
            Arc::new(DiscardLogger),
        );
        teardown.install_terminal(Arc::new(ImmediateTerminal));
        teardown.install_exit(Arc::new(NoExit));
        let (handle, driver) = new_host(Arc::clone(&teardown));
        let rig = Rig::new();
        let logger = Arc::new(CaptureLogger::default());
        let commands = Arc::new(RecordingCommands::default());
        let app = AppPaths::resolve(&self.app_environment()).expect("app paths");
        let config = ConfigPaths::resolve(None, &ConfigEnvironment {
            xdg_config_home: None,
            home: Some(self.root.path().to_path_buf()),
        })
        .expect("config paths");
        let settings = HostSettings {
            app,
            config,
            catalog: stache_app::catalog::catalog().expect("catalog"),
            factories: ModuleFactories::new().with(KeybindingsFactory::new(rig.runtime.clone())),
        };
        let services = HostServices {
            logger: logger.clone(),
            log_filter: Arc::new(LogFilter::default()),
            events: Arc::new(EventBus::new()),
            main_thread: Arc::new(InlineMain),
            permissions: Arc::new(AllowAll),
            windows: Arc::new(UnavailableWindows),
            commands: commands.clone(),
            poll: PermissionPoll {
                waiting: Duration::from_millis(5),
                backoff_start: Duration::from_millis(5),
                backoff_max: Duration::from_millis(5),
            },
            baseline: Vec::new(),
            settings: Arc::new(stache_app::access::UnavailableSettings),
        };
        driver
            .with_input_service(Arc::clone(&rig.input))
            .start(settings, services, Arc::new(NoUi))
            .expect("start host");
        teardown.start().expect("start the teardown coordinator");
        let running = RunningHost {
            handle,
            teardown,
            rig,
            logger,
            commands,
        };
        running.wait("keybindings active", |status| {
            status.ready
                && status.modules.iter().any(|module| {
                    module.id == "keybindings"
                        && module.state == ModuleState::Running
                        && module
                            .keybindings
                            .as_ref()
                            .is_some_and(|details| details.state == RuntimeState::Active)
                })
        });
        running
    }
}

struct RunningHost {
    handle: stache_app::host::HostHandle,
    teardown: Arc<Teardown>,
    rig: Rig,
    logger: Arc<CaptureLogger>,
    commands: Arc<RecordingCommands>,
}

impl RunningHost {
    fn wait(&self, label: &str, predicate: impl Fn(&HostStatus) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !predicate(&self.handle.status()) {
            assert!(Instant::now() < deadline, "timed out waiting for {label}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn assert_files_accepted(&self) {
        let status = self.handle.status();
        for module in &status.modules {
            assert!(module.diagnostic.is_none(), "{module:?}");
        }
        assert_eq!(self.rig.carbon.registered_chords(), [(0x0c, 0x0100)]);
        let deadline = Instant::now() + WAIT;
        while self.commands.0.lock().expect("commands").is_empty() {
            assert!(Instant::now() < deadline, "the startup command never ran");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn shutdown(self) {
        self.handle.request_shutdown();
        let _ = self.teardown.wait_for_outcome(WAIT).expect("teardown outcome");
        assert!(self.handle.wait_for_exit(WAIT), "the host exited");
    }
}

const ENTRIES_OFFLINE: [&str; 3] = [
    "  global.toml (exists, offline)\n",
    "  keybindings.toml (exists, offline)\n",
    "  tiling.toml (exists, offline)\n",
];

#[test]
fn a_symlinked_dotfiles_layout_warns_only_about_writable_paths_and_never_refuses() {
    let home = Home::new();

    // Ordinary modes: no warning offline, from the host or in its log.
    let (code, clean) = home.config_path();
    assert_eq!(code, ExitCode::SUCCESS);
    for entry in ENTRIES_OFFLINE {
        assert!(clean.contains(entry), "{clean}");
    }
    assert!(!clean.contains("warning:"), "{clean}");
    let host = home.start_host();
    host.assert_files_accepted();
    assert!(host.logger.trust().is_empty());
    let (code, live) = home.config_path();
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(live.contains("  keybindings.toml (exists, active)\n"), "{live}");
    assert!(!live.contains("warning:"), "{live}");

    // A group-writable dotfiles directory: one warning naming it.
    chmod(&home.dotfiles, 0o775);
    let directory = warning(&real(&home.dotfiles), "group-writable", "0775");
    let (code, out) = home.config_path();
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out, format!("{live}{directory}\n"), "markers unchanged");
    host.shutdown();

    // A world-writable `global.toml` adds a second warning; same markers.
    chmod(&home.dotfiles.join("global.toml"), 0o646);
    let file = warning(
        &real(&home.dotfiles.join("global.toml")),
        "world-writable",
        "0646",
    );
    let (code, out) = home.config_path();
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out, format!("{clean}{directory}\n{file}\n"), "markers unchanged");

    // A fresh startup logs both once, and the files stay accepted and active.
    let host = home.start_host();
    host.assert_files_accepted();
    assert_eq!(host.logger.trust(), vec![(
        LogLevel::Warn,
        format!("{directory}; {file}")
    )]);
    let (code, live_warned) = home.config_path();
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(live_warned, format!("{live}{directory}\n{file}\n"));
    host.shutdown();
}
