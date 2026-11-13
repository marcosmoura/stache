//! Opt-in live gate for the non-activating panel. Built only with
//! `--features panel-native-check` and never run by the automated suite.
//!
//! It runs the real Tauri application against an isolated configuration root
//! and opens a `panel-check` surface (the UI's local fallback page) as a
//! non-activating panel at a frame on `NSScreen.screens[0]`, hosted in a native non-activating `NSPanel`, and
//! logs `activation: frontmost=<app> stache_active=<bool>` on every change. Production [`windows`](crate::windows) validation
//! still rejects a panel for any surface but `bar`; only this feature's
//! [`NativeWindows::open_test_panel`] bypasses the surface check.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use stache_macos::{DisplayControl, MainThread};
use stache_sdk::{
    CancellationToken, ConfigFileId, ConfigPaths, ModuleId, PhaseWait, WindowBackend, WindowOrigin,
    WindowPhase, WindowSize,
};

use crate::host::HostHandle;
use crate::windows::NativeWindows;

/// Confirmation flag the tester must pass.
pub const CONFIRM_FLAG: &str = "--i-understand-this-opens-a-real-panel";

/// Usage text.
pub const USAGE: &str = "usage: panel-native-check --x <POINTS_FROM_PRIMARY_LEFT> \
--y <POINTS_FROM_PRIMARY_TOP> --width <POINTS> --height <POINTS> [--hold-seconds <N>] \
--i-understand-this-opens-a-real-panel";

const BASELINE: &str = r#"{
  "enabled": false,
  "output": [{ "name": "stache-panel-check disabled baseline", "strategy": "exact" }]
}
"#;

/// Parsed command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelCheckArgs {
    /// Offset from the primary display's left edge, in points.
    pub x: i32,
    /// Offset from the primary display's top edge, in points.
    pub y: i32,
    /// Panel width in points.
    pub width: u32,
    /// Panel height in points.
    pub height: u32,
    /// How long the panel stays open before the harness closes it.
    pub hold: Duration,
}

/// Parses the command line (without the program name).
///
/// # Errors
/// Returns a message for a missing, unknown or malformed argument.
pub fn parse(arguments: &[String]) -> Result<PanelCheckArgs, String> {
    let (mut x, mut y, mut width, mut height) = (None, None, None, None);
    let mut hold = 60_u64;
    let mut confirmed = false;
    let mut iterator = arguments.iter();
    while let Some(flag) = iterator.next() {
        let mut value =
            |name: &str| iterator.next().cloned().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--x" => x = Some(value("--x")?.parse::<i32>().map_err(|e| e.to_string())?),
            "--y" => y = Some(value("--y")?.parse::<i32>().map_err(|e| e.to_string())?),
            "--width" => {
                width = Some(value("--width")?.parse::<u32>().map_err(|e| e.to_string())?);
            }
            "--height" => {
                height = Some(value("--height")?.parse::<u32>().map_err(|e| e.to_string())?);
            }
            "--hold-seconds" => {
                hold = value("--hold-seconds")?.parse::<u64>().map_err(|e| e.to_string())?;
            }
            CONFIRM_FLAG => confirmed = true,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if !confirmed {
        return Err(format!("{CONFIRM_FLAG} is required"));
    }
    match (x, y, width, height) {
        (Some(x), Some(y), Some(width), Some(height)) => Ok(PanelCheckArgs {
            x,
            y,
            width,
            height,
            hold: Duration::from_secs(hold.min(600)),
        }),
        _ => Err("--x, --y, --width and --height are required".to_owned()),
    }
}

/// Writes the disabled `proxyAudio` baseline so the audio runtime never starts.
///
/// # Errors
/// Returns a path or I/O failure.
pub fn prepare_disabled_config(config: &ConfigPaths) -> Result<(), String> {
    let path = config.file(ConfigFileId::module(ModuleId::new("proxyAudio")));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create config: {error}"))?;
    }
    std::fs::write(&path, BASELINE).map_err(|error| format!("cannot write config: {error}"))
}

fn say(message: &str) {
    eprintln!("panel-check: {message}");
}

fn fail(message: &str) -> ! {
    eprintln!("panel-check: FAIL: {message}");
    std::process::exit(1);
}

/// Starts the check on a worker thread; the main thread keeps running Tauri.
pub fn start(args: PanelCheckArgs, host: HostHandle, windows: Arc<NativeWindows>) {
    if let Err(error) = thread::Builder::new()
        .name("stache-panel-check".to_owned())
        .spawn(move || run(args, &host, &windows))
    {
        fail(&format!("cannot start the check: {error}"));
    }
}

fn run(args: PanelCheckArgs, host: &HostHandle, windows: &Arc<NativeWindows>) {
    let layout = DisplayControl::system(Arc::new(MainThread))
        .query()
        .unwrap_or_else(|error| fail(&format!("cannot query displays: {error:?}")));
    let primary = layout.primary().unwrap_or_else(|| fail("no primary display"));
    let ax = layout.ax_frame(primary).unwrap_or_else(|| fail("no primary frame"));
    // Rounded to whole points; the primary's AX origin is the desktop origin.
    #[allow(clippy::cast_possible_truncation)]
    let origin = WindowOrigin {
        x: ax.x.round() as i32 + args.x,
        y: ax.y.round() as i32 + args.y,
    };
    let size =
        WindowSize::new(args.width, args.height).unwrap_or_else(|error| fail(&error.to_string()));
    say(&format!(
        "primary display {:?} is NSScreen.screens[0] ({}x{} at scale {}); panel at ({}, {}) {}x{}",
        primary.name,
        ax.width,
        ax.height,
        primary.scale,
        origin.x,
        origin.y,
        args.width,
        args.height
    ));
    let id = windows
        .open_test_panel(origin, size)
        .unwrap_or_else(|error| fail(&format!("panel request failed: {error}")));
    let phases = windows.window_phases();
    let never = CancellationToken::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut revision = 0;
    loop {
        match phases.wait_change(id, revision, deadline, &[&never]) {
            PhaseWait::Changed(snapshot) => {
                say(&format!("phase {:?}", snapshot.phase));
                revision = snapshot.revision;
                match snapshot.phase {
                    WindowPhase::Loaded => break,
                    WindowPhase::Failed(reason) => fail(&format!("panel failed: {reason}")),
                    WindowPhase::Closing | WindowPhase::Closed => fail("panel closed early"),
                    WindowPhase::Requested | WindowPhase::Created => {}
                }
            }
            PhaseWait::TimedOut(snapshot) => {
                fail(&format!(
                    "no Loaded phase within 20 s (last {:?})",
                    snapshot.phase
                ));
            }
            PhaseWait::Cancelled => fail("cancelled"),
        }
    }
    say(&format!(
        "panel visible for {} s: run the checklist in docs/guides/testing.md",
        args.hold.as_secs()
    ));
    // Read-only activation logger: a click on the panel must not make Stache
    // active or change the frontmost application. Hover cannot be observed from
    // the test page; check it by eye (see the checklist).
    let until = Instant::now() + args.hold;
    let mut last = String::new();
    while Instant::now() < until {
        let now = stache_macos::panel::diagnostic_activation_state();
        if now != last {
            say(&format!("activation: {now}"));
            last = now;
        }
        thread::sleep(Duration::from_millis(100));
    }
    windows.close(id);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match phases.wait_change(id, revision, deadline, &[&never]) {
            PhaseWait::Changed(snapshot) => {
                say(&format!("phase {:?}", snapshot.phase));
                revision = snapshot.revision;
                if snapshot.phase == WindowPhase::Closed {
                    break;
                }
            }
            PhaseWait::TimedOut(_) | PhaseWait::Cancelled => {
                fail("panel did not report Closed within 10 s");
            }
        }
    }
    say("ok: Tauri closed the panel and its webview; requesting shutdown");
    host.request_shutdown();
}
