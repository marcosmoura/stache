//! Wallpaper CLI commands.
//!
//! This module contains the wallpaper subcommands for managing desktop wallpapers.

use std::io;

use clap::Subcommand;

use super::types::ScreenTarget;
use crate::config;
use crate::error::StacheError;
use crate::modules::wallpaper::{self, WallpaperAction, WallpaperManagerError};

/// Wallpaper subcommands.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum WallpaperCommands {
    /// Set the desktop wallpaper.
    ///
    /// Set a specific wallpaper by providing a path, or use --random to set
    /// a random wallpaper from the configured wallpaper directory.
    #[command(
        verbatim_doc_comment,
        after_long_help = r#"Examples:
  stache wallpaper set /path/to/image.jpg               # Specific wallpaper for all screens
  stache wallpaper set /path/to/image.jpg --screen main # Specific wallpaper for main screen
  stache wallpaper set /path/to/image.jpg --screen 2    # Specific wallpaper for screen 2
  stache wallpaper set --random                         # Random wallpaper for all screens
  stache wallpaper set --random --screen main           # Random wallpaper for main screen
  stache wallpaper set --random --screen 2              # Random wallpaper for screen 2"#
    )]
    Set {
        /// The path to the image to use as wallpaper.
        #[arg(value_name = "PATH")]
        path: Option<String>,

        /// Set a random wallpaper from the configured wallpaper directory.
        #[arg(long, short)]
        random: bool,

        /// Specify which screen(s) to set the wallpaper on.
        /// Values: all, main, <index> (1-based).
        /// Default: all
        #[arg(long, short, default_value = "all")]
        screen: ScreenTarget,
    },

    /// Pre-generate all wallpapers.
    ///
    /// Processes all wallpapers from the configuration and stores them in the
    /// cache directory. Useful for pre-caching wallpapers to avoid delays
    /// when switching.
    #[command(name = "generate-all")]
    GenerateAll,

    /// List available wallpapers.
    ///
    /// Returns a JSON array of wallpaper paths from the configured wallpaper
    /// directory or list.
    List,
}

/// Execute wallpaper subcommands.
pub fn execute(cmd: &WallpaperCommands) -> Result<(), StacheError> {
    // Initialize config and wallpaper manager for CLI commands
    init_wallpaper_manager()?;

    match cmd {
        WallpaperCommands::Set { path, random, screen } => {
            execute_set(path.as_deref(), *random, screen)
        }
        WallpaperCommands::GenerateAll => execute_generate_all(),
        WallpaperCommands::List => execute_list(),
    }
}

/// Initializes the configuration and wallpaper manager for CLI commands.
fn init_wallpaper_manager() -> Result<(), StacheError> {
    // Initialize configuration (required for wallpaper settings)
    config::init();

    // Setup wallpaper manager (creates the manager instance)
    wallpaper::setup();

    // Check if wallpaper manager was initialized successfully
    if wallpaper::get_manager().is_none() {
        return Err(StacheError::WallpaperError(
            "Wallpaper manager not initialized. Check your wallpaper configuration.".to_string(),
        ));
    }

    Ok(())
}

/// Execute the wallpaper set command.
fn execute_set(path: Option<&str>, random: bool, screen: &ScreenTarget) -> Result<(), StacheError> {
    if path.is_some() && random {
        return Err(StacheError::InvalidArguments(
            "Cannot specify both <path> and --random. Use one or the other.".to_string(),
        ));
    }

    if path.is_none() && !random {
        return Err(StacheError::InvalidArguments(
            "Either <path> or --random must be specified.".to_string(),
        ));
    }

    // Convert ScreenTarget and path/random to WallpaperAction
    let action = match (path, random, screen) {
        // Random wallpaper
        (None, true, ScreenTarget::All) => WallpaperAction::Random,
        (None, true, ScreenTarget::Main) => WallpaperAction::RandomForScreen(0),
        (None, true, ScreenTarget::Index(idx)) => {
            WallpaperAction::RandomForScreen(idx.as_zero_based())
        }
        // Specific file
        (Some(file), false, ScreenTarget::All) => WallpaperAction::File(file.to_string()),
        (Some(file), false, ScreenTarget::Main) => {
            WallpaperAction::FileForScreen(0, file.to_string())
        }
        (Some(file), false, ScreenTarget::Index(idx)) => {
            WallpaperAction::FileForScreen(idx.as_zero_based(), file.to_string())
        }
        // This case is handled by the validation above
        _ => unreachable!(),
    };

    wallpaper::perform_action(&action).map_err(wallpaper_error_to_stache_error)?;

    println!("Wallpaper set successfully.");
    Ok(())
}

/// Execute the wallpaper list command.
fn execute_list() -> Result<(), StacheError> {
    let wallpapers = wallpaper::list_wallpapers().map_err(wallpaper_error_to_stache_error)?;

    if wallpapers.is_empty() {
        println!("No wallpapers found.");
    } else {
        // Output as JSON array for easy parsing
        let json = serde_json::to_string_pretty(&wallpapers)
            .map_err(|e| StacheError::WallpaperError(format!("JSON serialization error: {e}")))?;
        println!("{json}");
    }

    Ok(())
}

/// Execute the wallpaper generate-all command.
fn execute_generate_all() -> Result<(), StacheError> {
    wallpaper::generate_all_streaming(io::stdout()).map_err(wallpaper_error_to_stache_error)
}

/// Convert `WallpaperManagerError` to `StacheError`.
#[allow(clippy::needless_pass_by_value)]
fn wallpaper_error_to_stache_error(err: WallpaperManagerError) -> StacheError {
    StacheError::WallpaperError(err.to_string())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::super::types::ScreenIndex;
    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: WallpaperCommands,
    }

    #[test]
    fn test_wallpaper_list_parse() {
        let cli = TestCli::try_parse_from(["test", "list"]).unwrap();
        assert!(matches!(cli.command, WallpaperCommands::List));
    }

    #[test]
    fn test_wallpaper_generate_all_parse() {
        let cli = TestCli::try_parse_from(["test", "generate-all"]).unwrap();
        assert!(matches!(cli.command, WallpaperCommands::GenerateAll));
    }

    #[test]
    fn test_wallpaper_set_path_parse() {
        let cli = TestCli::try_parse_from(["test", "set", "/path/to/image.jpg"]).unwrap();
        match cli.command {
            WallpaperCommands::Set { path, random, screen } => {
                assert_eq!(path, Some("/path/to/image.jpg".to_string()));
                assert!(!random);
                assert_eq!(screen, ScreenTarget::All);
            }
            _ => panic!("Expected Set command"),
        }
    }

    #[test]
    fn test_wallpaper_set_random_parse() {
        let cli = TestCli::try_parse_from(["test", "set", "--random"]).unwrap();
        match cli.command {
            WallpaperCommands::Set { path, random, .. } => {
                assert!(path.is_none());
                assert!(random);
            }
            _ => panic!("Expected Set command"),
        }
    }

    #[test]
    fn test_wallpaper_set_screen_main_parse() {
        let cli = TestCli::try_parse_from(["test", "set", "--random", "--screen", "main"]).unwrap();
        match cli.command {
            WallpaperCommands::Set { screen, .. } => {
                assert_eq!(screen, ScreenTarget::Main);
            }
            _ => panic!("Expected Set command"),
        }
    }

    #[test]
    fn test_wallpaper_set_screen_index_parse() {
        let cli = TestCli::try_parse_from(["test", "set", "--random", "--screen", "2"]).unwrap();
        match cli.command {
            WallpaperCommands::Set { screen, .. } => {
                assert_eq!(screen, ScreenTarget::Index(ScreenIndex::new(2)));
            }
            _ => panic!("Expected Set command"),
        }
    }
}
