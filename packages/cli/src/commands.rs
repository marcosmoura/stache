//! CLI command definitions using Clap.
//!
//! This module defines all CLI commands and their arguments.

use std::io;
use std::str::FromStr;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Generator, Shell, generate};

use crate::audio::{self, DeviceFilter};
use crate::error::CliError;
use crate::ipc;

/// Application version from Cargo.toml.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Barba Shell CLI - Dispatches events to the running Barba instance.
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
    /// Notify Barba that the focused window changed.
    ///
    /// Use when your automation detects a window-focus change.
    /// Triggers Hyprspace queries to refresh current workspace and app state.
    #[command(name = "focus-changed")]
    FocusChanged,

    /// Notify Barba that the active workspace changed.
    ///
    /// Requires the new workspace name so Barba can update its Hyprspace view
    /// and trigger a window refresh.
    #[command(name = "workspace-changed")]
    WorkspaceChanged {
        /// Workspace identifier reported by hyprspace (e.g. coding).
        name: String,
    },

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

/// Data sent for the wallpaper set command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WallpaperSetData {
    /// The path to the image file to set as wallpaper, if specified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Whether to set a random wallpaper.
    pub random: bool,
    /// The target screen(s) for the wallpaper.
    pub screen: ScreenTarget,
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

/// Payload for CLI events sent to the desktop app.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CliEventPayload {
    /// The name of the CLI command/event.
    pub name: String,
    /// Optional data associated with the command.
    pub data: Option<String>,
}

impl Cli {
    /// Execute the CLI command.
    pub fn execute(&self) -> Result<(), CliError> {
        match &self.command {
            Commands::FocusChanged => {
                let payload = CliEventPayload {
                    name: "focus-changed".to_string(),
                    data: None,
                };
                ipc::send_to_desktop_app(&payload)?;
            }

            Commands::WorkspaceChanged { name } => {
                let payload = CliEventPayload {
                    name: "workspace-changed".to_string(),
                    data: Some(name.clone()),
                };
                ipc::send_to_desktop_app(&payload)?;
            }

            Commands::Wallpaper(wallpaper_cmd) => Self::execute_wallpaper(wallpaper_cmd)?,
            Commands::Cache(cache_cmd) => Self::execute_cache(cache_cmd)?,
            Commands::Audio(audio_cmd) => Self::execute_audio(audio_cmd)?,

            Commands::Reload => {
                let payload = CliEventPayload {
                    name: "reload".to_string(),
                    data: None,
                };
                ipc::send_to_desktop_app(&payload)?;
            }

            Commands::Schema => {
                let schema = barba_shared::print_schema();
                println!("{schema}");
            }

            Commands::Completions { shell } => {
                Self::print_completions(*shell);
            }
        }

        Ok(())
    }

    /// Print shell completions to stdout.
    fn print_completions<G: Generator>(generator: G) {
        let mut cmd = Self::command();
        generate(generator, &mut cmd, "barba", &mut io::stdout());
    }

    /// Execute cache subcommands.
    fn execute_cache(cmd: &CacheCommands) -> Result<(), CliError> {
        match cmd {
            CacheCommands::Clear => {
                let cache_dir = barba_shared::get_cache_dir();
                if !cache_dir.exists() {
                    println!("Cache directory does not exist. Nothing to clear.");
                    return Ok(());
                }

                match barba_shared::clear_cache() {
                    Ok(bytes_freed) => {
                        let formatted = barba_shared::format_bytes(bytes_freed);
                        println!("Cache cleared successfully. Freed {formatted}.");
                    }
                    Err(err) => {
                        return Err(CliError::CacheError(format!("Failed to clear cache: {err}")));
                    }
                }
            }
            CacheCommands::Path => {
                let cache_dir = barba_shared::get_cache_dir();
                println!("{}", cache_dir.display());
            }
        }
        Ok(())
    }

    /// Execute audio subcommands.
    fn execute_audio(cmd: &AudioCommands) -> Result<(), CliError> {
        match cmd {
            AudioCommands::List { json, input, output } => {
                let filter = match (input, output) {
                    (true, false) => DeviceFilter::InputOnly,
                    (false, true) => DeviceFilter::OutputOnly,
                    _ => DeviceFilter::All,
                };

                let devices = audio::list_devices(filter);

                if *json {
                    let json_output = serde_json::to_string_pretty(&devices).map_err(|e| {
                        CliError::AudioError(format!("JSON serialization error: {e}"))
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
    fn execute_wallpaper(cmd: &WallpaperCommands) -> Result<(), CliError> {
        match cmd {
            WallpaperCommands::Set { path, random, screen } => {
                if path.is_some() && *random {
                    return Err(CliError::InvalidArguments(
                        "Cannot specify both <path> and --random. Use one or the other."
                            .to_string(),
                    ));
                }

                if path.is_none() && !*random {
                    return Err(CliError::InvalidArguments(
                        "Either <path> or --random must be specified.".to_string(),
                    ));
                }

                let data = WallpaperSetData {
                    path: path.clone(),
                    random: *random,
                    screen: screen.clone(),
                };
                let payload = CliEventPayload {
                    name: "wallpaper-set".to_string(),
                    data: Some(serde_json::to_string(&data).unwrap()),
                };
                ipc::send_to_desktop_app(&payload)?;
            }
            WallpaperCommands::GenerateAll => {
                let payload = CliEventPayload {
                    name: "wallpaper-generate-all".to_string(),
                    data: None,
                };
                ipc::send_to_desktop_app_extended(&payload)?;
            }
            WallpaperCommands::List => {
                let payload = CliEventPayload {
                    name: "wallpaper-list".to_string(),
                    data: None,
                };
                let response = ipc::send_to_desktop_app_with_response(&payload)?;
                println!("{response}");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_event_payload_serialization() {
        let payload = CliEventPayload {
            name: "test-event".to_string(),
            data: Some("test-data".to_string()),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("test-event"));
        assert!(json.contains("test-data"));
    }

    #[test]
    fn test_cli_event_payload_serialization_without_data() {
        let payload = CliEventPayload {
            name: "test-event".to_string(),
            data: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("test-event"));
        assert!(json.contains("null"));
    }

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

    #[test]
    fn test_wallpaper_set_data_serialization_with_path() {
        let data = WallpaperSetData {
            path: Some("/path/to/image.jpg".to_string()),
            random: false,
            screen: ScreenTarget::All,
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("/path/to/image.jpg"));
        assert!(json.contains("\"random\":false"));
        assert!(json.contains("\"screen\":\"all\""));
    }

    #[test]
    fn test_wallpaper_set_data_serialization_with_random() {
        let data = WallpaperSetData {
            path: None,
            random: true,
            screen: ScreenTarget::Main,
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(!json.contains("path"));
        assert!(json.contains("\"random\":true"));
        assert!(json.contains("\"screen\":\"main\""));
    }

    #[test]
    fn test_wallpaper_set_data_serialization_with_screen_index() {
        let data = WallpaperSetData {
            path: Some("/path/to/image.jpg".to_string()),
            random: false,
            screen: ScreenTarget::Index(2),
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("\"screen\":{\"index\":2}"));
    }
}
