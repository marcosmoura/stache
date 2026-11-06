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
pub mod keep_awake;
pub mod logging;
pub mod permissions;
pub mod shutdown;
#[cfg(feature = "smoke")]
pub mod smoke;
pub mod startup;
pub mod tray;
pub mod wallpapers;
pub mod windows;

use std::cell::RefCell;
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;
#[cfg(feature = "smoke")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use stache_cli::CliEnvironment;
use stache_macos::{
    AccessibilityPermission, ActivationObserver, AssertionOwner, MainThread, TerminateObservation,
};
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
use crate::keep_awake::PowerAssertionOwner;
use crate::logging::StderrLogger;
use crate::permissions::PermissionPoll;
use crate::shutdown::{
    BoundedRecordChannel, ExitAction, ExitGate, InstanceClaim, InstanceOwner, MainHopOwner,
    NativeSlot, SignalBridge, StderrRecordSink, SystemClock, SystemExit, Teardown, TerminalAction,
    TerminalDriver, TerminalProgress, exit_request_action, report_terminate_install_failure,
};
use crate::startup::ProcessCommands;
use crate::wallpapers::WallpapersFactory;
use crate::windows::{NativeWindows, TauriWindowHost, WindowsOwner};

thread_local! {
    /// The `!Send` activation guard. It is installed and removed on main only.
    static ACTIVATION: RefCell<Option<ActivationObserver>> = const { RefCell::new(None) };
}

/// Takes and drops the activation guard on the calling (main) thread.
fn take_activation() {
    ACTIVATION.with(|guard| {
        guard.borrow_mut().take();
    });
}

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
/// Every closure shares the coordinator's [`TerminalProgress`] claim, so a
/// closure that loses it does nothing at all.
struct DesktopTerminal {
    app: tauri::AppHandle,
    gate: ExitGate,
    main_thread: Arc<dyn MainThreadExecutor>,
    signals: Arc<Mutex<Option<SignalBridge>>>,
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
    fn dispatch(&self, action: TerminalAction) -> TerminalProgress {
        let progress = TerminalProgress::new();
        match action {
            TerminalAction::Reply => {
                let claimed = progress.clone();
                let delivered = progress.clone();
                let _ = self.main_thread.execute(Box::new(move || {
                    if !claimed.claim(Instant::now()) {
                        return;
                    }
                    let _ = stache_macos::reply_to_terminate();
                    delivered.mark_delivered();
                }));
            }
            TerminalAction::Exit(code) => {
                let claimed = progress.clone();
                let delivered = progress.clone();
                let app = self.app.clone();
                let fallback = self.app.clone();
                if self
                    .main_thread
                    .execute(Box::new(move || {
                        if !claimed.claim(Instant::now()) {
                            return;
                        }
                        // Tray removal is its own acknowledged owner hop, so
                        // exit stands alone here.
                        app.exit(code);
                        delivered.mark_delivered();
                    }))
                    .is_err()
                    && progress.claim(Instant::now())
                {
                    fallback.exit(code);
                    progress.mark_delivered();
                }
            }
            TerminalAction::Restart => {
                let claimed = progress.clone();
                let delivered = progress.clone();
                let app = self.app.clone();
                let _ = self.main_thread.execute(Box::new(move || {
                    if !claimed.claim(Instant::now()) {
                        return;
                    }
                    app.request_restart();
                    delivered.mark_delivered();
                }));
            }
        }
        progress
    }

    fn open_gate(&self, code: i32) { self.gate.record_exit(code); }

    fn close_signals(&self) { self.stop_signals(); }
}

/// A hook run inside setup after the tray, stack and window bookkeeping exist.
///
/// The smoke build also receives the coordinator so its scenarios can register
/// an owner before the first request; the production type has no such parameter.
#[cfg(feature = "smoke")]
type SetupHook = Box<
    dyn FnOnce(&tauri::AppHandle, &HostHandle, &Arc<NativeWindows>, &ExitGate, &Teardown)
        + Send
        + 'static,
>;

/// A hook run inside setup after the tray, stack and window bookkeeping exist.
#[cfg(not(feature = "smoke"))]
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
        Some(Box::new(move |app, host, windows, gate, _teardown| {
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
/// `hang` registers the temporary hanging owner in the HUD slot before the
/// scheduled terminate fires. A successful run ends inside `AppKit`.
///
/// # Errors
/// Never returns `Ok`; a return means setup failed or the run loop stopped
/// without completing termination.
#[cfg(feature = "smoke")]
pub fn run_smoke_terminate(config_paths: ConfigPaths, hang: bool) -> Result<(), String> {
    match run_with(
        config_paths,
        Some(Box::new(move |app, host, _, _, teardown| {
            if hang {
                smoke::register_hanging_owner(teardown);
            }
            smoke::start_terminate(app, host);
        })),
        Instant::now(),
    ) {
        Ok(()) => smoke::fail("the terminate proof left the run loop without terminating"),
        Err(error) => smoke::fail(&format!("desktop setup failed: {error}")),
    }
}

/// Runs the external-quit hold scenario.
///
/// `hang` registers the hanging owner; otherwise the normal teardown owners are
/// the only participants. The scenario opens the explicit test window and waits
/// 120 s for an external quit before it reports `smoke: FAIL`.
///
/// # Errors
/// Never returns `Ok`; a return means the run loop stopped before a quit.
#[cfg(feature = "smoke")]
pub fn run_smoke_hold(config_paths: ConfigPaths, hang: bool) -> Result<(), String> {
    match run_with(
        config_paths,
        Some(Box::new(move |app, host, windows, _, teardown| {
            if hang {
                smoke::register_hanging_owner(teardown);
            }
            smoke::start_hold(app, host, windows);
        })),
        Instant::now(),
    ) {
        Ok(()) => smoke::fail("the hold scenario left the run loop without an external quit"),
        Err(error) => smoke::fail(&format!("desktop setup failed: {error}")),
    }
}

/// Runs the coordinated relaunch scenario.
///
/// The first process writes a marker under its own runtime root and requests a
/// restart through the shipped host path. A process that finds the marker is the
/// successor: it must reach readiness on the held lock, print its line, then
/// request an ordinary shutdown. The verdict is the parent's reading of the log:
/// the restart path replaces the first process and `run_return` does not return.
///
/// # Errors
/// Returns a setup failure, or `Ok` after the successor shut down. A first
/// process that returns before the restart is an error.
#[cfg(feature = "smoke")]
pub fn run_smoke_relaunch(config_paths: ConfigPaths) -> Result<(), String> {
    let environment = AppEnvironment::from_env();
    let app_paths = AppPaths::resolve(&environment).map_err(|error| error.to_string())?;
    let marker = smoke::relaunch_marker(&app_paths);
    let successor = marker.is_file();
    let ran = Arc::new(AtomicBool::new(false));
    let hook_ran = Arc::clone(&ran);
    let result = run_with(
        config_paths,
        Some(Box::new(move |_app, host, _, _, _teardown| {
            hook_ran.store(true, Ordering::SeqCst);
            if successor {
                smoke::start_relaunch_successor(host);
            } else {
                smoke::start_relaunch_first(marker, host);
            }
        })),
        Instant::now(),
    );
    match result {
        Ok(()) if successor && ran.load(Ordering::SeqCst) => Ok(()),
        Ok(()) => Err("the relaunch process exited before its scenario completed".to_owned()),
        Err(error) => Err(error),
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
    let wallpapers_home = environment.home;
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
    // The composition root owns one Keep Awake assertion owner for the whole
    // module lifetime. The setup closure moves a clone into the production
    // factory; the original is registered below as the teardown acknowledgement
    // over the same ledger.
    let power_owner = AssertionOwner::system();
    let setup_power_owner = power_owner.clone();
    let setup_host = host.clone();
    let setup_gate = gate.clone();
    let outcome_gate = gate.clone();
    let terminal_gate = gate.clone();
    let terminal_main_thread = Arc::clone(&main_thread);
    let owner_main_thread = Arc::clone(&main_thread);
    let signals = Arc::new(Mutex::new(None::<SignalBridge>));
    let setup_signals = Arc::clone(&signals);
    let exit_teardown = Arc::clone(&teardown);
    let setup_teardown = Arc::clone(&teardown);

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
            // Wallpapers needs the identity paths, configuration directory and
            // home, which only the composition root holds.
            let wallpapers = WallpapersFactory::new(
                app_paths.clone(),
                config_paths.directory(),
                wallpapers_home,
                Arc::clone(&main_thread),
            )
            .map_err(|error| format!("cannot create the Wallpapers factory: {error}"))?;
            let factories =
                ModuleFactories::production(Arc::clone(&main_thread), setup_power_owner)
                    .with(wallpapers);
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
            // Windows only exist once this backend does, so a run that started
            // before this registration has none to acknowledge.
            setup_teardown.register_owner(
                NativeSlot::Windows,
                Arc::new(WindowsOwner::new(window_backend.clone())),
            );
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
                #[cfg(feature = "smoke")]
                hook(app.handle(), &hook_host, &window_backend, &gate, &setup_teardown);
                #[cfg(not(feature = "smoke"))]
                hook(app.handle(), &hook_host, &window_backend, &gate);
            }
            Ok(())
        })
        .build(context())
        .map_err(|error| error.to_string())?;

    teardown.install_exit(Arc::new(SystemExit));
    teardown.install_terminal(Arc::new(DesktopTerminal {
        app: app.handle().clone(),
        gate: terminal_gate,
        main_thread: terminal_main_thread,
        signals: Arc::clone(&signals),
    }));
    // The activation guard stays on main: the cell is set here, and only a
    // main-thread closure takes and drops it. Installing it before the
    // coordinator starts means no request can race its registration.
    let activation_host = app.state::<HostState>().0.clone();
    let activation = ActivationObserver::install(move || activation_host.refresh_permissions())
        .ok_or_else(|| "activation observer requires the main thread".to_owned())?;
    ACTIVATION.with(|guard| *guard.borrow_mut() = Some(activation));
    teardown.register_owner(
        NativeSlot::ApplicationObservers,
        Arc::new(MainHopOwner::new(
            "activation",
            Arc::clone(&owner_main_thread),
            || {
                take_activation();
                Ok(())
            },
        )),
    );
    let tray_app = app.handle().clone();
    teardown.register_owner(
        NativeSlot::Tray,
        Arc::new(MainHopOwner::new("tray", owner_main_thread, move || {
            // Missing means the tray was already removed or never created.
            let _ = tray_app.remove_tray_by_id(tray::TRAY_ID);
            Ok(())
        })),
    );
    // Keep Awake's native cleanup stays with its module `stop` and rollback. This
    // adapter only acknowledges that same ledger after the registry stage, so it
    // is registered before the coordinator starts and can never race a request.
    teardown.register_owner(
        NativeSlot::PowerAssertion,
        Arc::new(PowerAssertionOwner::new(power_owner)),
    );
    // The coordinator exists before the observation and before `run_return`,
    // so no request can arrive without a driver already waiting for it.
    teardown
        .start()
        .map_err(|error| format!("cannot start the teardown coordinator: {error}"))?;
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
    // The same cell the activation owner clears; a normal return after the
    // terminal action leaves nothing to remove.
    take_activation();
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
    let commands = commands::catalog_for(environment);
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
    stache_cli::run_with_io(args, &configurations, &commands, environment, ansi, out, err)
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
