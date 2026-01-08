//! CLI module for Barba Shell.
//!
//! This module provides command-line interface functionality for interacting
//! with Barba. When the desktop app is running, CLI commands communicate with
//! it directly. When not running, some commands may launch the app.

mod commands;

use clap::Parser;
pub use commands::Cli;

use crate::error::BarbaError;

/// Runs the CLI.
///
/// Parses command-line arguments and executes the appropriate command.
///
/// # Errors
///
/// Returns an error if the command execution fails.
pub fn run() -> Result<(), BarbaError> {
    let cli = Cli::parse();
    cli.execute()
}
