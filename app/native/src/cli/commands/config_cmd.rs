//! Config CLI commands.
//!
//! Commands for managing the Stache configuration file.

use std::path::PathBuf;

use clap::Subcommand;

use crate::config::config_paths;
use crate::config::template::{create_config_file, generate_config_template};
use crate::error::StacheError;

/// Config management commands.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum ConfigCommands {
    /// Initialize a new configuration file with all options documented.
    ///
    /// Creates a new configuration file at the default location with all
    /// available options commented out. This allows you to see all possible
    /// configuration options and uncomment the ones you want to use.
    #[command(
        name = "init",
        after_long_help = r#"Examples:
  stache config init              # Create config at default location
  stache config init --force      # Overwrite existing config
  stache config init --path ~/my-config.jsonc  # Create at custom path
  stache config init --stdout     # Print template to stdout"#
    )]
    Init {
        /// Overwrite existing configuration file if it exists.
        #[arg(long, short)]
        force: bool,

        /// Custom path for the configuration file.
        /// If not specified, uses ~/.config/stache/config.jsonc
        #[arg(long, short, value_name = "PATH")]
        path: Option<PathBuf>,

        /// Print the configuration template to stdout instead of writing to a file.
        #[arg(long)]
        stdout: bool,
    },

    /// Show the path to the configuration file.
    ///
    /// Displays the path where Stache looks for configuration files,
    /// and indicates which one is currently in use (if any).
    Path,
}

/// Execute config subcommands.
///
/// # Errors
///
/// Returns an error if the command execution fails.
pub fn execute(cmd: &ConfigCommands) -> Result<(), StacheError> {
    match cmd {
        ConfigCommands::Init { force, path, stdout } => {
            if *stdout {
                print_config_template()
            } else {
                init_config(*force, path.clone())
            }
        }
        ConfigCommands::Path => show_config_path(),
    }
}

/// Print the configuration template to stdout.
#[allow(clippy::unnecessary_wraps)] // Consistent return type with other CLI functions
fn print_config_template() -> Result<(), StacheError> {
    println!("{}", generate_config_template());
    Ok(())
}

/// Initialize a new configuration file.
fn init_config(force: bool, custom_path: Option<PathBuf>) -> Result<(), StacheError> {
    let config_path = custom_path.unwrap_or_else(|| {
        // Use the first config path (preferred location)
        config_paths()
            .into_iter()
            .next()
            .unwrap_or_else(|| PathBuf::from("config.jsonc"))
    });

    // Check if file already exists
    if config_path.exists() && !force {
        return Err(StacheError::ConfigError(format!(
            "Configuration file already exists at: {}\nUse --force to overwrite.",
            config_path.display()
        )));
    }

    // Create the config file using the shared function
    create_config_file(&config_path).map_err(|e| {
        StacheError::ConfigError(format!(
            "Failed to create config file {}: {e}",
            config_path.display()
        ))
    })?;

    println!("Configuration file created at: {}", config_path.display());
    println!("\nAll options are commented out by default.");
    println!("Edit the file and uncomment the options you want to configure.");

    Ok(())
}

/// Show the configuration file path.
#[allow(clippy::unnecessary_wraps)] // Consistent return type with other CLI functions
fn show_config_path() -> Result<(), StacheError> {
    println!("Configuration file search paths (in priority order):\n");

    let paths = config_paths();
    let mut found_config = false;

    for (i, path) in paths.iter().enumerate() {
        let exists = path.exists();
        let marker = if exists && !found_config {
            found_config = true;
            " (active)"
        } else if exists {
            " (exists)"
        } else {
            ""
        };

        println!("  {}. {}{}", i + 1, path.display(), marker);
    }

    if !found_config {
        println!("\nNo configuration file found.");
        println!("Run 'stache config init' to create one.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_paths_returns_non_empty() {
        let paths = config_paths();
        // Should have at least one path (unless no HOME directory)
        assert!(!paths.is_empty() || std::env::var("HOME").is_err());
    }
}
