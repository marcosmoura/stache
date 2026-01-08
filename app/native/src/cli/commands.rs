//! CLI command definitions using Clap.
//!
//! This module defines all CLI commands and their arguments.

use std::io;
use std::str::FromStr;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Generator, Shell, generate};

use crate::error::BarbaError;
use crate::utils::ipc::{self, BarbaNotification};
use crate::wallpaper::{self, WallpaperAction, WallpaperManagerError};
use crate::{audio, cache, config, schema};

/// Application version from Cargo.toml.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Barba Shell CLI - Command-line interface for Barba Shell.
#[derive(Parser, Debug)]
#[command(name = "barba")]
#[command(author, version = APP_VERSION, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Available CLI commands.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum Commands {
    /// Send events to the running Barba desktop app.
    ///
    /// These commands notify the running Barba instance about external changes
    /// detected by your automation tools (e.g., yabai, aerospace, skhd).
    #[command(subcommand)]
    Event(EventCommands),

    /// Wallpaper management commands.
    #[command(subcommand)]
    Wallpaper(WallpaperCommands),

    /// Cache management commands.
    ///
    /// Manage the application's cache directory.
    #[command(subcommand)]
    Cache(CacheCommands),

    /// Audio device management commands.
    ///
    /// List and inspect audio devices on the system.
    #[command(subcommand)]
    Audio(AudioCommands),

    /// Reload Barba configuration.
    ///
    /// Reloads the configuration file and applies changes without restarting
    /// the application.
    Reload,

    /// Output Barba configuration JSON Schema.
    ///
    /// Outputs a JSON Schema to stdout that describes the structure of the
    /// Barba configuration file. Can be redirected to a file for use with
    /// editors that support JSON Schema validation.
    Schema,

    /// Generate shell completions.
    ///
    /// Outputs shell completion script to stdout for the specified shell.
    /// Can be used with eval or redirected to a file.
    ///
    /// Usage:
    ///   eval "$(barba completions --shell zsh)"
    ///   barba completions --shell bash > ~/.local/share/bash-completion/completions/barba
    ///   barba completions --shell fish > ~/.config/fish/completions/barba.fish
    Completions {
        /// The shell to generate completions for.
        #[arg(long, short, value_enum)]
        shell: Shell,
    },

    /// Launch the desktop application.
    ///
    /// Launches Barba in desktop mode. This is equivalent to running `barba`
    /// without any arguments.
    #[command(name = "--desktop", hide = true)]
    Desktop,
}

/// Screen target for wallpaper commands.
///
/// Specifies which screen(s) should receive the wallpaper.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScreenTarget {
    /// Apply to all screens.
    #[default]
    All,
    /// Apply to the main screen only.
    Main,
    /// Apply to a specific screen by 1-based index.
    Index(usize),
}

impl FromStr for ScreenTarget {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "all" => Ok(Self::All),
            "main" => Ok(Self::Main),
            _ => s.parse::<usize>().map(Self::Index).map_err(|_| {
                format!(
                    "Invalid screen value '{s}'. Expected 'all', 'main', or a positive integer."
                )
            }),
        }
    }
}

impl std::fmt::Display for ScreenTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::All => write!(f, "all"),
            Self::Main => write!(f, "main"),
            Self::Index(idx) => write!(f, "{idx}"),
        }
    }
}

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
  barba wallpaper set /path/to/image.jpg               # Specific wallpaper for all screens
  barba wallpaper set /path/to/image.jpg --screen main # Specific wallpaper for main screen
  barba wallpaper set /path/to/image.jpg --screen 2    # Specific wallpaper for screen 2
  barba wallpaper set --random                         # Random wallpaper for all screens
  barba wallpaper set --random --screen main           # Random wallpaper for main screen
  barba wallpaper set --random --screen 2              # Random wallpaper for screen 2"#
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

// ============================================================================
// Event Commands
// ============================================================================

/// Event subcommands for notifying the running Barba instance.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum EventCommands {
    /// Notify Barba that the focused window changed.
    ///
    /// Use when your automation detects a window-focus change.
    /// Triggers Hyprspace queries to refresh current workspace and app state.
    #[command(name = "window-focus-changed")]
    WindowFocusChanged,

    /// Notify Barba that the active workspace changed.
    ///
    /// Requires the new workspace name so Barba can update its Hyprspace view
    /// and trigger a window refresh.
    #[command(name = "workspace-changed")]
    WorkspaceChanged {
        /// Workspace identifier reported by hyprspace (e.g. coding).
        name: String,
    },
}

// ============================================================================
// Cache Commands
// ============================================================================

/// Cache subcommands for managing the application's cache.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum CacheCommands {
    /// Clear the application's cache directory.
    ///
    /// Removes all cached files including processed wallpapers and media artwork.
    /// This can help resolve issues with stale data or free up disk space.
    #[command(after_long_help = r#"Examples:
  barba cache clear   # Clear all cached data"#)]
    Clear,

    /// Show the cache directory location.
    ///
    /// Displays the path to the application's cache directory.
    #[command(after_long_help = r#"Examples:
  barba cache path    # Print the cache directory path"#)]
    Path,
}

// ============================================================================
// Audio Commands
// ============================================================================

/// Audio subcommands for listing and inspecting audio devices.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum AudioCommands {
    /// List all audio devices on the system.
    ///
    /// Shows audio input and output devices with their names and types.
    /// By default, displays a human-readable table format.
    #[command(after_long_help = r#"Examples:
  barba audio list              # List all devices in table format
  barba audio list --json       # List all devices in JSON format
  barba audio list --input      # List only input devices
  barba audio list --output     # List only output devices
  barba audio list -io --json   # List all devices in JSON (explicit)"#)]
    List {
        /// Output in JSON format instead of table format.
        #[arg(long, short = 'j')]
        json: bool,

        /// Show only input devices.
        #[arg(long, short = 'i')]
        input: bool,

        /// Show only output devices.
        #[arg(long, short = 'o')]
        output: bool,
    },
}

impl Cli {
    /// Execute the CLI command.
    ///
    /// # Errors
    ///
    /// Returns an error if the command execution fails.
    pub fn execute(&self) -> Result<(), BarbaError> {
        match &self.command {
            Commands::Event(event_cmd) => Self::execute_event(event_cmd)?,
            Commands::Wallpaper(wallpaper_cmd) => Self::execute_wallpaper(wallpaper_cmd)?,
            Commands::Cache(cache_cmd) => Self::execute_cache(cache_cmd)?,
            Commands::Audio(audio_cmd) => Self::execute_audio(audio_cmd)?,

            Commands::Reload => {
                if !ipc::send_notification(&BarbaNotification::Reload) {
                    return Err(BarbaError::IpcError(
                        "Failed to send reload notification to Barba app".to_string(),
                    ));
                }
            }

            Commands::Schema => {
                let schema_output = schema::print_schema();
                println!("{schema_output}");
            }

            Commands::Completions { shell } => {
                Self::print_completions(*shell);
            }

            Commands::Desktop => {
                // This case should not be reached as main.rs handles --desktop
                unreachable!("Desktop mode should be handled by main.rs");
            }
        }

        Ok(())
    }

    /// Print shell completions to stdout.
    fn print_completions<G: Generator>(generator: G) {
        let mut cmd = Self::command();
        generate(generator, &mut cmd, "barba", &mut io::stdout());
    }

    /// Execute event subcommands.
    fn execute_event(cmd: &EventCommands) -> Result<(), BarbaError> {
        let notification = match cmd {
            EventCommands::WindowFocusChanged => BarbaNotification::WindowFocusChanged,
            EventCommands::WorkspaceChanged { name } => {
                BarbaNotification::WorkspaceChanged(name.clone())
            }
        };

        if ipc::send_notification(&notification) {
            Ok(())
        } else {
            Err(BarbaError::IpcError(
                "Failed to send notification to Barba app".to_string(),
            ))
        }
    }

    /// Execute cache subcommands.
    fn execute_cache(cmd: &CacheCommands) -> Result<(), BarbaError> {
        match cmd {
            CacheCommands::Clear => {
                let cache_dir = cache::get_cache_dir();
                if !cache_dir.exists() {
                    println!("Cache directory does not exist. Nothing to clear.");
                    return Ok(());
                }

                match cache::clear_cache() {
                    Ok(bytes_freed) => {
                        let formatted = cache::format_bytes(bytes_freed);
                        println!("Cache cleared successfully. Freed {formatted}.");
                    }
                    Err(err) => {
                        return Err(BarbaError::CacheError(format!(
                            "Failed to clear cache: {err}"
                        )));
                    }
                }
            }
            CacheCommands::Path => {
                let cache_dir = cache::get_cache_dir();
                println!("{}", cache_dir.display());
            }
        }
        Ok(())
    }

    /// Execute audio subcommands.
    fn execute_audio(cmd: &AudioCommands) -> Result<(), BarbaError> {
        match cmd {
            AudioCommands::List { json, input, output } => {
                let filter = match (input, output) {
                    (true, false) => audio::DeviceFilter::InputOnly,
                    (false, true) => audio::DeviceFilter::OutputOnly,
                    _ => audio::DeviceFilter::All,
                };

                let devices = audio::list_devices(filter);

                if *json {
                    let json_output = serde_json::to_string_pretty(&devices).map_err(|e| {
                        BarbaError::AudioError(format!("JSON serialization error: {e}"))
                    })?;
                    println!("{json_output}");
                } else {
                    let table = audio::format_devices_table(&devices);
                    println!("{table}");
                }
            }
        }
        Ok(())
    }

    /// Execute wallpaper subcommands.
    fn execute_wallpaper(cmd: &WallpaperCommands) -> Result<(), BarbaError> {
        // Initialize config and wallpaper manager for CLI commands
        Self::init_wallpaper_manager()?;

        match cmd {
            WallpaperCommands::Set { path, random, screen } => {
                Self::execute_wallpaper_set(path.as_deref(), *random, screen)
            }
            WallpaperCommands::GenerateAll => Self::execute_wallpaper_generate_all(),
            WallpaperCommands::List => Self::execute_wallpaper_list(),
        }
    }

    /// Initializes the configuration and wallpaper manager for CLI commands.
    fn init_wallpaper_manager() -> Result<(), BarbaError> {
        // Initialize configuration (required for wallpaper settings)
        config::init();

        // Initialize wallpaper manager
        wallpaper::init();

        // Check if wallpaper manager was initialized successfully
        if wallpaper::get_manager().is_none() {
            return Err(BarbaError::WallpaperError(
                "Wallpaper manager not initialized. Check your wallpaper configuration."
                    .to_string(),
            ));
        }

        Ok(())
    }

    /// Execute the wallpaper set command.
    fn execute_wallpaper_set(
        path: Option<&str>,
        random: bool,
        screen: &ScreenTarget,
    ) -> Result<(), BarbaError> {
        if path.is_some() && random {
            return Err(BarbaError::InvalidArguments(
                "Cannot specify both <path> and --random. Use one or the other.".to_string(),
            ));
        }

        if path.is_none() && !random {
            return Err(BarbaError::InvalidArguments(
                "Either <path> or --random must be specified.".to_string(),
            ));
        }

        // Convert ScreenTarget and path/random to WallpaperAction
        let action = match (path, random, screen) {
            // Random wallpaper
            (None, true, ScreenTarget::All) => WallpaperAction::Random,
            (None, true, ScreenTarget::Main) => WallpaperAction::RandomForScreen(0),
            (None, true, ScreenTarget::Index(idx)) => {
                // Convert 1-based CLI index to 0-based internal index
                WallpaperAction::RandomForScreen(idx.saturating_sub(1))
            }
            // Specific file
            (Some(file), false, ScreenTarget::All) => WallpaperAction::File(file.to_string()),
            (Some(file), false, ScreenTarget::Main) => {
                WallpaperAction::FileForScreen(0, file.to_string())
            }
            (Some(file), false, ScreenTarget::Index(idx)) => {
                // Convert 1-based CLI index to 0-based internal index
                WallpaperAction::FileForScreen(idx.saturating_sub(1), file.to_string())
            }
            // This case is handled by the validation above
            _ => unreachable!(),
        };

        wallpaper::perform_action(&action).map_err(Self::wallpaper_error_to_barba_error)?;

        println!("Wallpaper set successfully.");
        Ok(())
    }

    /// Execute the wallpaper list command.
    fn execute_wallpaper_list() -> Result<(), BarbaError> {
        let wallpapers =
            wallpaper::list_wallpapers().map_err(Self::wallpaper_error_to_barba_error)?;

        if wallpapers.is_empty() {
            println!("No wallpapers found.");
        } else {
            // Output as JSON array for easy parsing
            let json = serde_json::to_string_pretty(&wallpapers).map_err(|e| {
                BarbaError::WallpaperError(format!("JSON serialization error: {e}"))
            })?;
            println!("{json}");
        }

        Ok(())
    }

    /// Execute the wallpaper generate-all command.
    fn execute_wallpaper_generate_all() -> Result<(), BarbaError> {
        wallpaper::generate_all_streaming(io::stdout())
            .map_err(Self::wallpaper_error_to_barba_error)
    }

    /// Convert `WallpaperManagerError` to `BarbaError`.
    #[allow(clippy::needless_pass_by_value)]
    fn wallpaper_error_to_barba_error(err: WallpaperManagerError) -> BarbaError {
        BarbaError::WallpaperError(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_screen_target_from_str_all() {
        let target: ScreenTarget = "all".parse().unwrap();
        assert_eq!(target, ScreenTarget::All);
    }

    #[test]
    fn test_screen_target_from_str_main() {
        let target: ScreenTarget = "main".parse().unwrap();
        assert_eq!(target, ScreenTarget::Main);
    }

    #[test]
    fn test_screen_target_from_str_index() {
        let target: ScreenTarget = "2".parse().unwrap();
        assert_eq!(target, ScreenTarget::Index(2));
    }

    #[test]
    fn test_screen_target_from_str_case_insensitive() {
        let target: ScreenTarget = "ALL".parse().unwrap();
        assert_eq!(target, ScreenTarget::All);

        let target: ScreenTarget = "Main".parse().unwrap();
        assert_eq!(target, ScreenTarget::Main);
    }

    #[test]
    fn test_screen_target_from_str_invalid() {
        let result: Result<ScreenTarget, _> = "invalid".parse();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid screen value"));
    }

    #[test]
    fn test_screen_target_display() {
        assert_eq!(ScreenTarget::All.to_string(), "all");
        assert_eq!(ScreenTarget::Main.to_string(), "main");
        assert_eq!(ScreenTarget::Index(2).to_string(), "2");
    }

    #[test]
    fn test_screen_target_default() {
        let target = ScreenTarget::default();
        assert_eq!(target, ScreenTarget::All);
    }
}
