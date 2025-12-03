#![allow(clippy::multiple_crate_versions)]

//! Barba CLI - Command-line interface for Barba Shell.
//!
//! This standalone CLI communicates with the running Barba desktop application
//! to dispatch commands and events.

// Emit a clear compile-time error if attempted to compile on unsupported platforms
#[cfg(not(target_os = "macos"))]
compile_error!("This application only supports macOS.");

mod commands;
mod error;
mod ipc;

use clap::Parser;
use commands::Cli;

fn main() {
    let cli = Cli::parse();

    if let Err(err) = cli.execute() {
        eprintln!("barba: {err}");
        std::process::exit(1);
    }
}
