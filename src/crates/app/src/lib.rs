//! Tray-only desktop host and the single-binary entry point.
//!
//! [`run_main`] selects the desktop or the command line before any Tauri code
//! runs. The command line never starts the host, a webview, the socket server
//! or a module. The desktop path claims the single-instance lock first, creates
//! the tray on the main thread, and only then starts the background host.

#![forbid(unsafe_code)]

pub mod bindings;
pub mod catalog;
pub mod commands;
pub mod config_edit;
pub mod host;
pub mod logging;
pub mod permissions;
pub mod shutdown;
#[cfg(feature = "smoke")]
pub mod smoke;
pub mod startup;
pub mod tray;
pub mod windows;

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
#[cfg(feature = "smoke")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use stache_cli::CliEnvironment;
use stache_macos::{AccessibilityPermission, ActivationObserver, MainThread, TerminateObservation};
use stache_sdk::{
    AppEnvironment, AppPaths, ConfigPaths, EventBus, InstanceError, InstanceLock, LogFilter,
    LogLevel, LogRecord, Logger, MainThreadExecutor, ModuleId, Operation,
};
use tauri::{Emitter, Manager, RunEvent};
use tauri_specta::Event as _;

use crate::bindings::{HostStatus, StatusChanged};
use crate::host::{
    HostDriver, HostHandle, HostServices, HostSettings, HostState, HostUi, ModuleFactories,
    new_host,
};
use crate::logging::StderrLogger;
use crate::permissions::PermissionPoll;
use crate::shutdown::{
    BoundedRecordChannel, ExitAction, ExitGate, InstanceClaim, InstanceOwner, ReplyGuard,
    SignalBridge, StderrRecordSink, SystemClock, Teardown, TerminalAction, TerminalDriver,
    exit_request_action, report_terminate_install_failure,
};
use crate::startup::ProcessCommands;
use crate::windows::{NativeWindows, TauriWindowHost};

#[derive(Clone)]
struct WindowsState(Arc<NativeWindows>);

struct AppUi {
    app: tauri::AppHandle,
    main_thread: Arc<dyn MainThreadExecutor>,
}

impl HostUi for AppUi {
    fn status(&self, status: &HostStatus) {
        let _ = self.app.emit(StatusChanged::NAME, StatusChanged { status: status.clone() });
        let app = self.app.clone();
        let snapshot = status.clone();
        let _ = self.main_thread.execute(Box::new(move || {
            let _ = tray::update(&app, &snapshot);
        }));
    }
}

/// Runs the one terminal action chosen by the coordinator.
///
/// The reply path never calls `app.exit`: once `AppKit` has a pending
/// `applicationShouldTerminate:`, only a single `true` reply may finish it.
struct DesktopTerminal {
    app: tauri::AppHandle,
    gate: ExitGate,
    main_thread: Arc<dyn MainThreadExecutor>,
    signals: Arc<Mutex<Option<SignalBridge>>>,
    reply: ReplyGuard,
}

impl DesktopTerminal {
    fn stop_signals(&self) {
        let bridge =
            { self.signals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() };
        if let Some(bridge) = bridge {
            bridge.stop();
        }
    }
}

impl TerminalDriver for DesktopTerminal {
    fn dispatch(&self, action: TerminalAction) {
        match action {
            TerminalAction::Reply => {
                let reply = self.reply.clone();
                // A stalled main thread can delay this closure. Bounding that
                // is the next part's forced-exit work, which shares this reply
                // guard; this part adds no forced exit.
                let _ = self.main_thread.execute(Box::new(move || {
                    if reply.claim() {
                        let _ = stache_macos::reply_to_terminate();
                    }
                }));
            }
            TerminalAction::Exit(code) => {
                self.stop_signals();
                self.gate.record_exit(code);
                let app = self.app.clone();
                let fallback = self.app.clone();
                if self
                    .main_thread
                    .execute(Box::new(move || {
                        app.remove_tray_by_id(tray::TRAY_ID);
                        app.exit(code);
                    }))
                    .is_err()
                {
                    fallback.exit(code);
                }
            }
            TerminalAction::Restart => {
                self.stop_signals();
                self.gate.record_exit(0);
                self.app.request_restart();
            }
        }
    }
}

/// A hook run inside setup after the tray, stack and window bookkeeping exist.
type SetupHook = Box<
    dyn FnOnce(&tauri::AppHandle, &HostHandle, &Arc<NativeWindows>, &ExitGate) + Send + 'static,
>;

/// Runs the desktop host for one resolved configuration directory.
///
/// A second launch returns success before creating a tray, registry, listener
/// or webview.
///
/// # Errors
/// Returns a human-readable failure for path, instance-lock, catalog or Tauri
/// startup errors.
pub fn run(config_paths: ConfigPaths) -> Result<(), String> {
    run_with(config_paths, None, Instant::now())
}

/// Runs the desktop with the opt-in native smoke harness.
///
/// # Errors
/// Returns the same failures as [`run`].
#[cfg(feature = "smoke")]
pub fn run_smoke(config_paths: ConfigPaths) -> Result<(), String> {
    let smoke_config = config_paths.clone();
    let completed = Arc::new(AtomicBool::new(false));
    let hook_completed = Arc::clone(&completed);
    let final_gate = Arc::new(Mutex::new(None::<ExitGate>));
    let hook_gate = Arc::clone(&final_gate);
    let result = run_with(
        config_paths,
        Some(Box::new(move |app, host, windows, gate| {
            *hook_gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(gate.clone());
            smoke::start(smoke::SmokeContext {
                app: app.clone(),
                host: host.clone(),
                windows: Arc::clone(windows),
                config: smoke_config,
                completed: hook_completed,
            });
        })),
        Instant::now(),
    );
    let host_finished = final_gate
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|gate| gate.exit_code() == Some(0));
    match result {
        Ok(()) if completed.load(Ordering::SeqCst) && host_finished => Ok(()),
        Ok(()) => Err("smoke exited before checks and host shutdown completed".to_owned()),
        Err(error) => Err(error),
    }
}

/// Runs the opt-in terminate observation proof.
///
/// The shipped desktop path installs the observation and starts the coordinator
/// before the run loop, so this scenario exercises exactly that path: it only
/// schedules a real `terminate:` and inspects the running coordination. A
/// successful run ends inside `AppKit` and never returns.
///
/// # Errors
/// Never returns `Ok`; a return means setup failed or the run loop stopped
/// without completing termination.
#[cfg(feature = "smoke")]
pub fn run_smoke_terminate(config_paths: ConfigPaths) -> Result<(), String> {
    match run_with(
        config_paths,
        Some(Box::new(|app, host, _, _| smoke::start_terminate(app, host))),
        Instant::now(),
    ) {
        Ok(()) => smoke::fail("the terminate proof left the run loop without terminating"),
        Err(error) => smoke::fail(&format!("desktop setup failed: {error}")),
    }
}

#[allow(clippy::too_many_lines)] // One ordered Tauri setup and teardown sequence.
fn run_with(
    config_paths: ConfigPaths,
    hook: Option<SetupHook>,
    startup_started: Instant,
) -> Result<(), String> {
    let environment = AppEnvironment::from_env();
    let app_paths = AppPaths::resolve(&environment).map_err(|error| error.to_string())?;
    let instance = match InstanceLock::acquire(&app_paths) {
        Ok(instance) => instance,
        Err(InstanceError::AlreadyRunning { .. }) => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let catalog = catalog::catalog().map_err(|error| error.to_string())?;
    let filter = Arc::new(LogFilter::default());
    let logger: Arc<dyn Logger> = Arc::new(StderrLogger::new(Arc::clone(&filter)));
    let bindings = bindings::builder();
    let invoke = bindings.invoke_handler();
    let gate = ExitGate::new();
    let main_thread: Arc<dyn MainThreadExecutor> = Arc::new(MainThread);
    let signal_logger = Arc::clone(&logger);
    let setup_logger = Arc::clone(&logger);
    let install_logger = Arc::clone(&logger);
    // The claim is shareable so a coordinated restart can release it before
    // Tauri relaunches the next process.
    let instance = InstanceClaim::new(instance);
    let teardown = Teardown::new(
        Arc::new(SystemClock),
        Arc::new(BoundedRecordChannel::new(Arc::new(StderrRecordSink))),
        Arc::new(instance.clone()),
        Arc::clone(&logger),
    );
    let (host, driver) = new_host(Arc::clone(&teardown));
    let setup_host = host.clone();
    let setup_gate = gate.clone();
    let outcome_gate = gate.clone();
    let terminal_gate = gate.clone();
    let terminal_main_thread = Arc::clone(&main_thread);
    let signals = Arc::new(Mutex::new(None::<SignalBridge>));
    let setup_signals = Arc::clone(&signals);
    let exit_teardown = Arc::clone(&teardown);

    let app = tauri::Builder::default()
        .manage(HostState(host.clone()))
        .invoke_handler(invoke)
        .setup(move |app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            bindings.mount_events(app);
            tray::create(app.handle(), setup_host.clone())?;
            let webviews = app.webview_windows().len();
            if webviews != 0 {
                return Err("desktop launch must not create a webview".into());
            }
            setup_logger.log(LogRecord {
                level: LogLevel::Info,
                module: ModuleId::new("host"),
                operation: Operation::Start,
                message: tray_ready_message(startup_started.elapsed(), webviews),
            });
            let window_host = Arc::new(TauriWindowHost::new(app.handle().clone()));
            let window_backend = Arc::new(NativeWindows::new(
                Arc::clone(&main_thread),
                window_host,
                Arc::clone(&logger),
            ));
            let ui = Arc::new(AppUi {
                app: app.handle().clone(),
                main_thread: Arc::clone(&main_thread),
            });
            // The production factories take the last executor clone: modules
            // dispatch native work through it and never retain a second one.
            let factories = ModuleFactories::production(Arc::clone(&main_thread));
            let services = HostServices {
                logger: Arc::clone(&logger),
                log_filter: filter,
                events: Arc::new(EventBus::new()),
                main_thread,
                permissions: Arc::new(AccessibilityPermission),
                windows: window_backend.clone(),
                commands: Arc::new(ProcessCommands),
                poll: PermissionPoll::default(),
            };
            let settings = HostSettings {
                app: app_paths,
                config: config_paths,
                catalog,
                factories,
            };
            if let Err(error) = start_host(driver, settings, services, ui) {
                setup_gate.record_exit(1);
                return Err(error);
            }
            app.manage(WindowsState(window_backend.clone()));
            app.manage(gate.clone());
            let hook_host = setup_host.clone();
            match SignalBridge::install(move || setup_host.request_shutdown()) {
                Ok(bridge) => {
                    *setup_signals.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(bridge);
                }
                Err(error) => signal_logger.log(stache_sdk::LogRecord {
                    level: stache_sdk::LogLevel::Warn,
                    module: stache_sdk::ModuleId::new("host"),
                    operation: stache_sdk::Operation::Start,
                    message: format!("signal handling is unavailable: {error}"),
                }),
            }
            if let Some(hook) = hook {
                hook(app.handle(), &hook_host, &window_backend, &gate);
            }
            Ok(())
        })
        .build(context())
        .map_err(|error| error.to_string())?;

    teardown.install_terminal(Arc::new(DesktopTerminal {
        app: app.handle().clone(),
        gate: terminal_gate,
        main_thread: terminal_main_thread,
        signals: Arc::clone(&signals),
        reply: ReplyGuard::new(),
    }));
    // The coordinator exists before the observation and before `run_return`,
    // so no request can arrive without a driver already waiting for it.
    teardown
        .start()
        .map_err(|error| format!("cannot start the teardown coordinator: {error}"))?;
    let activation_host = app.state::<HostState>().0.clone();
    let activation = ActivationObserver::install(move || activation_host.refresh_permissions())
        .ok_or_else(|| "activation observer requires the main thread".to_owned())?;
    if let Err(error) = TerminateObservation::install({
        let teardown = Arc::clone(&teardown);
        move || match teardown.begin_terminate() {
            crate::shutdown::TeardownAnswer::Later => stache_macos::TerminateAnswer::Later,
            crate::shutdown::TeardownAnswer::Now => stache_macos::TerminateAnswer::Now,
        }
    }) {
        report_terminate_install_failure(&host, install_logger.as_ref(), &error.to_string());
    }
    let code = app.run_return(move |app, event| match event {
        RunEvent::ExitRequested { code, api, .. } => {
            let gate = app.state::<ExitGate>();
            match exit_request_action(code, gate.is_allowed()) {
                ExitAction::Allow => {}
                ExitAction::PreventOnly => api.prevent_exit(),
                ExitAction::PreventAndShutdown => {
                    api.prevent_exit();
                    app.state::<HostState>().0.request_shutdown_with_code(code);
                }
            }
        }
        RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Destroyed,
            ..
        } => app.state::<WindowsState>().0.window_closed(&label),
        // An unobserved terminate path never reaches the coordinator; the one
        // bypass record keeps that gap visible instead of silent.
        RunEvent::Exit => {
            exit_teardown.note_unrecorded_exit();
        }
        _ => {}
    });
    let intended = outcome_gate.exit_code();
    drop(activation);
    instance.release();
    finish_result(code, intended)
}

/// Formats the one-shot host startup measurement read by release tooling.
fn tray_ready_message(ready_after: Duration, webviews: usize) -> String {
    format!(
        "startup: tray ready after {} ms, webviews={webviews}",
        ready_after.as_millis()
    )
}

/// Preserves the host's intended exit code through the CLI boundary.
///
/// `run_return` can return zero even when `app.exit(1)` was requested, so the
/// recorded host outcome wins for failure. A non-zero runtime result still fails
/// when the host recorded success, and an untouched gate cannot turn an early
/// loop completion into success by itself.
///
/// # Errors
/// Returns a human-readable failure for a non-zero host or runtime exit.
fn finish_result(code: i32, intended: Option<i32>) -> Result<(), String> {
    if let Some(host) = intended
        && host != 0
    {
        return Err(format!("desktop host exited with code {host}"));
    }
    if code != 0 {
        return Err(format!("desktop runtime exited with code {code}"));
    }
    Ok(())
}

fn start_host(
    driver: HostDriver,
    settings: HostSettings,
    services: HostServices,
    ui: Arc<AppUi>,
) -> Result<(), Box<dyn std::error::Error>> {
    driver
        .start(settings, services, ui)
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)
}

fn context() -> tauri::Context<tauri::Wry> { tauri::generate_context!() }

/// Runs the selected mode with the process environment and streams.
///
/// A desktop failure is reported as `stache: {error}` with exit code 1, so the
/// single binary has one error convention.
#[must_use]
pub fn run_main(args: Vec<OsString>, current_exe: &Path) -> ExitCode {
    let started = Instant::now();
    run_main_with(args, current_exe, move |paths| run_with(paths, None, started))
}

/// [`run_main`] with an injected desktop entry point.
///
/// The process streams are passed unlocked: desktop mode runs for the whole
/// session, and host threads log to stderr, so holding the stdout/stderr locks
/// here would block every background log line forever.
fn run_main_with(
    args: Vec<OsString>,
    current_exe: &Path,
    desktop: impl FnOnce(ConfigPaths) -> Result<(), String>,
) -> ExitCode {
    let environment = CliEnvironment::from_env();
    let ansi = std::io::stdout().is_terminal();
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    dispatch(
        args,
        current_exe,
        &environment,
        ansi,
        &mut out,
        &mut err,
        desktop,
    )
}

/// Runs the selected mode with injected streams and desktop entry point.
pub fn dispatch<I, W, E>(
    args: I,
    current_exe: &Path,
    environment: &CliEnvironment,
    ansi: bool,
    out: &mut W,
    err: &mut E,
    desktop: impl FnOnce(ConfigPaths) -> Result<(), String>,
) -> ExitCode
where
    I: IntoIterator<Item = OsString>,
    W: Write,
    E: Write,
{
    let args: Vec<OsString> = args.into_iter().collect();
    let commands = commands::catalog();
    let matches = match stache_cli::commands::compose(&commands).try_get_matches_from(&args) {
        Ok(matches) => matches,
        Err(error) => {
            let code = u8::try_from(error.exit_code()).unwrap_or(2);
            if error.use_stderr() {
                let _ = write!(err, "{error}");
            } else {
                let _ = write!(out, "{error}");
            }
            return ExitCode::from(code);
        }
    };
    if stache_cli::mode::is_app_bundle(current_exe)
        || matches.subcommand_name().is_none()
        || matches.get_flag("desktop")
    {
        let paths = match ConfigPaths::resolve(
            matches.get_one::<std::path::PathBuf>("config").cloned(),
            &environment.config,
        ) {
            Ok(paths) => paths,
            Err(error) => {
                let _ = writeln!(err, "stache: {error}");
                return ExitCode::FAILURE;
            }
        };
        return match desktop(paths) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(err, "stache: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let configurations = match catalog::catalog() {
        Ok(catalog) => catalog,
        Err(error) => {
            let _ = writeln!(err, "stache: {error}");
            return ExitCode::FAILURE;
        }
    };
    stache_cli::run_with_io(
        args,
        &configurations,
        &commands::catalog(),
        environment,
        ansi,
        out,
        err,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use stache_sdk::AppEnvironment;
    use stache_sdk::config::ConfigEnvironment;

    use super::*;

    fn environment(home: &Path, temporary: &Path) -> CliEnvironment {
        CliEnvironment::new(
            ConfigEnvironment {
                xdg_config_home: None,
                home: Some(home.to_path_buf()),
            },
            AppEnvironment {
                home: Some(home.to_path_buf()),
                temporary: Some(temporary.to_path_buf()),
            },
        )
    }

    #[test]
    fn a_non_zero_host_exit_is_a_cli_failure() {
        assert!(finish_result(0, None).is_ok());
        assert!(finish_result(0, Some(1)).is_err());
        assert!(finish_result(0, Some(7)).is_err());
        assert!(finish_result(1, None).is_err());
        assert!(finish_result(2, Some(0)).is_err());
        assert!(finish_result(0, Some(0)).is_ok());
    }

    #[test]
    fn startup_measurement_reports_milliseconds_and_webviews() {
        assert_eq!(
            tray_ready_message(Duration::from_millis(123), 0),
            "startup: tray ready after 123 ms, webviews=0"
        );
    }

    #[test]
    fn desktop_identity_and_window_policy_are_isolated() {
        let config = super::context();
        assert_eq!(config.config().identifier, "com.marcosmoura.stache.next");
        assert!(config.config().app.windows.is_empty());
    }

    #[test]
    fn validation_and_help_precede_every_desktop_choice() {
        let root = tempfile::tempdir().expect("root");
        for executable in [
            "/usr/local/bin/stache",
            "/Applications/Next.app/Contents/MacOS/stache",
        ] {
            for (args, expected) in [
                (vec!["stache", "--config"], 2),
                (vec!["stache", "--desktop", "--garbage"], 2),
                (vec!["stache", "--garbage"], 2),
                (vec!["stache", "--desktop", "--help"], 0),
                (vec!["stache", "--version"], 0),
            ] {
                let code = dispatch(
                    args.into_iter().map(OsString::from),
                    Path::new(executable),
                    &environment(root.path(), root.path()),
                    false,
                    &mut Vec::new(),
                    &mut Vec::new(),
                    |_| panic!("desktop must not start"),
                );
                assert_eq!(code, ExitCode::from(expected));
            }
        }
    }

    #[test]
    fn valid_desktop_choices_receive_the_selected_config_directory() {
        let root = tempfile::tempdir().expect("root");
        let selected = root.path().join("selected");
        for (executable, command) in [
            ("/usr/local/bin/stache", vec![]),
            ("/usr/local/bin/stache", vec!["--desktop"]),
            ("/Applications/Next.app/Contents/MacOS/stache", vec!["schema"]),
        ] {
            let mut args = vec![
                OsString::from("stache"),
                OsString::from("--config"),
                selected.clone().into_os_string(),
            ];
            args.extend(command.into_iter().map(OsString::from));
            let code = dispatch(
                args,
                Path::new(executable),
                &environment(root.path(), root.path()),
                false,
                &mut Vec::new(),
                &mut Vec::new(),
                |paths| {
                    assert_eq!(paths.directory(), selected);
                    Ok(())
                },
            );
            assert_eq!(code, ExitCode::SUCCESS);
        }
    }

    #[test]
    fn desktop_mode_does_not_hold_the_process_streams() {
        let root = tempfile::tempdir().expect("temp dir");
        let args = [
            OsString::from("stache"),
            OsString::from("--desktop"),
            OsString::from("--config"),
            root.path().as_os_str().to_owned(),
        ];
        let code = run_main_with(args.to_vec(), Path::new("/usr/local/bin/stache"), |_| {
            // A host thread logging while the desktop runs must not block.
            let (done, finished) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = writeln!(std::io::stderr(), "background log line");
                let _ = writeln!(std::io::stdout(), "background output line");
                let _ = done.send(());
            });
            finished
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|_| "a background thread blocked on a process stream".to_owned())
        });
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[test]
    fn builtins_never_start_the_desktop() {
        let root = tempfile::tempdir().expect("temp dir");
        let started = AtomicBool::new(false);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = dispatch(
            ["stache", "cache", "path"].map(OsString::from),
            Path::new("/usr/local/bin/stache"),
            &environment(root.path(), root.path()),
            false,
            &mut out,
            &mut err,
            |_| {
                started.store(true, Ordering::SeqCst);
                Ok(())
            },
        );
        assert_eq!(code, ExitCode::SUCCESS, "{err:?}");
        assert!(!started.load(Ordering::SeqCst));
        assert!(
            String::from_utf8(out).expect("utf-8").starts_with(
                root.path()
                    .canonicalize()
                    .expect("canonical")
                    .join("Library")
                    .to_str()
                    .expect("utf-8")
            )
        );
    }

    #[test]
    fn desktop_mode_calls_only_the_desktop_entry_point() {
        let root = tempfile::tempdir().expect("temp dir");
        let started = AtomicBool::new(false);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = dispatch(
            ["stache"].map(OsString::from),
            Path::new("/usr/local/bin/stache"),
            &environment(root.path(), root.path()),
            false,
            &mut out,
            &mut err,
            |_| {
                started.store(true, Ordering::SeqCst);
                Err("sentinel".to_owned())
            },
        );
        assert_eq!(code, ExitCode::FAILURE);
        assert!(started.load(Ordering::SeqCst));
        assert_eq!(String::from_utf8(err).expect("utf-8"), "stache: sentinel\n");
    }
}
