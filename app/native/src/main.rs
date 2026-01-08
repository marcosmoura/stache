#![allow(clippy::multiple_crate_versions)]

//! Barba - macOS status bar replacement with tiling window manager integration.
//!
//! This binary serves as both the desktop application and CLI:
//! - When called with no arguments or with `--desktop`: launches the desktop app
//! - When called with subcommands (e.g., `barba wallpaper set`): runs CLI commands
//!
//! If the desktop app is already running, CLI commands communicate with it directly.
//! If not running, CLI commands launch the app in the background first.

// Emit a clear compile-time error if attempted to compile on unsupported platforms
#[cfg(not(target_os = "macos"))]
compile_error!("This application only supports macOS.");

fn main() {
    // Check if we should run as CLI or desktop app
    let args: Vec<String> = std::env::args().collect();

    // Run as desktop app if:
    // - No arguments (just the binary name)
    // - First arg is --desktop
    // - Running from within an .app bundle (detected by bundle path)
    let run_desktop = args.len() == 1
        || args.get(1).is_some_and(|arg| arg == "--desktop")
        || is_running_from_app_bundle();

    if run_desktop {
        barba_lib::run();
    } else if let Err(err) = barba_lib::cli::run() {
        eprintln!("barba: {err}");
        std::process::exit(1);
    }
}

/// Checks if the binary is running from within a macOS .app bundle.
///
/// When launched from Barba.app, the executable path will be something like:
/// `/Applications/Barba.app/Contents/MacOS/barba`
fn is_running_from_app_bundle() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(|s| s.contains(".app/Contents/MacOS")))
        .unwrap_or(false)
}
