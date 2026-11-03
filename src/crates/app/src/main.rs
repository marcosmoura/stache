//! The single `stache` binary: desktop host or command line.
//!
//! Mode selection runs before any Tauri startup, so a malformed CLI flag is
//! reported by clap and never launches the tray host.

use std::process::ExitCode;

fn main() -> ExitCode {
    let current_exe = std::env::current_exe().unwrap_or_default();
    stache_app::run_main(std::env::args_os().collect(), &current_exe)
}
