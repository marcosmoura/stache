//! CLI module for Stache.
//!
//! This module provides command-line interface functionality for interacting
//! with Stache. When the desktop app is running, CLI commands communicate with
//! it directly. When not running, some commands may launch the app.

mod commands;
mod output;

use clap::Parser;
pub use commands::Cli;

use crate::error::StacheError;

/// Runs the CLI.
///
/// Parses command-line arguments and executes the appropriate command.
///
/// # Errors
///
/// Returns an error if the command execution fails.
pub fn run() -> Result<(), StacheError> {
    let cli = Cli::parse();
    cli.execute()
}
