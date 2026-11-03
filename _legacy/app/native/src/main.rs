#![allow(clippy::multiple_crate_versions)]

//! Stache - macOS utility suite with status bar, automation, and desktop enhancements.
//!
//! This binary serves as both the desktop application and CLI:
//! - When called with no arguments or with `--desktop`: launches the desktop app
//! - When called with subcommands (e.g., `stache wallpaper set`): runs CLI commands
//!
//! If the desktop app is already running, CLI commands communicate with it directly.
//! If not running, CLI commands launch the app in the background first.

// Emit a clear compile-time error if attempted to compile on unsupported platforms
#[cfg(not(target_os = "macos"))]
compile_error!("This application only supports macOS.");

fn main() {
    // Check if we should run as CLI or desktop app
    let args: Vec<String> = std::env::args().collect();

    // Check for --config flag before other processing
    let config_path = extract_config_path(&args);

    // Run as desktop app if:
    // - No arguments (just the binary name)
    // - First arg is --desktop (after extracting --config)
    // - Running from within an .app bundle (detected by bundle path)
    let run_desktop = should_run_desktop(&args) || is_running_from_app_bundle();

    if run_desktop {
        // Set custom config path if provided
        if let Some(path) = config_path {
            let path_buf = std::path::PathBuf::from(&path);
            if !path_buf.exists() {
                eprintln!("stache: configuration file not found: {path}");
                std::process::exit(1);
            }
            stache_lib::config::set_custom_config_path(path_buf);
        }
        stache_lib::run();
    } else if let Err(err) = stache_lib::cli::run() {
        eprintln!("stache: {err}");
        std::process::exit(1);
    }
}

/// Extracts the config path from --config or -c flag.
fn extract_config_path(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--config" || arg == "-c" {
            return iter.next().cloned();
        }
        if let Some(path) = arg.strip_prefix("--config=") {
            return Some(path.to_string());
        }
    }
    None
}

/// Determines if we should run in desktop mode.
fn should_run_desktop(args: &[String]) -> bool {
    // Filter out --config/-c and its value to check remaining args
    let filtered: Vec<_> = {
        let mut result = Vec::new();
        let mut skip_next = false;
        for arg in args.iter().skip(1) {
            // Skip the binary name
            if skip_next {
                skip_next = false;
                continue;
            }
            if arg == "--config" || arg == "-c" {
                skip_next = true;
                continue;
            }
            if arg.starts_with("--config=") {
                continue;
            }
            result.push(arg.as_str());
        }
        result
    };

    // Run as desktop if no args (after filtering) or first arg is --desktop
    filtered.is_empty() || filtered.first().is_some_and(|&arg| arg == "--desktop")
}

/// Checks if the binary is running from within a macOS .app bundle.
///
/// When launched from Stache.app, the executable path will be something like:
/// `/Applications/Stache.app/Contents/MacOS/stache`
fn is_running_from_app_bundle() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(|s| s.contains(".app/Contents/MacOS")))
        .unwrap_or(false)
}
