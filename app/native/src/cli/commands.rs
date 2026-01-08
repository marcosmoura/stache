//! CLI command definitions using Clap.
//!
//! This module defines all CLI commands and their arguments.

use std::io;
use std::str::FromStr;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Generator, Shell, generate};

use crate::error::StacheError;
use crate::utils::ipc::{self, StacheNotification};
use crate::wallpaper::{self, WallpaperAction, WallpaperManagerError};
use crate::{audio, cache, config, schema};

/// Application version from Cargo.toml.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stache CLI - Command-line interface for Stache.
#[derive(Parser, Debug)]
#[command(name = "stache")]
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
    /// Send events to the running Stache desktop app.
    ///
    /// These commands notify the running Stache instance about external changes
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

    /// Reload Stache configuration.
    ///
    /// Reloads the configuration file and applies changes without restarting
    /// the application.
    Reload,

    /// Output Stache configuration JSON Schema.
    ///
    /// Outputs a JSON Schema to stdout that describes the structure of the
    /// Stache configuration file. Can be redirected to a file for use with
    /// editors that support JSON Schema validation.
    Schema,

    /// Generate shell completions.
    ///
    /// Outputs shell completion script to stdout for the specified shell.
    /// Can be used with eval or redirected to a file.
    ///
    /// Usage:
    ///   eval "$(stache completions --shell zsh)"
    ///   stache completions --shell bash > ~/.local/share/bash-completion/completions/stache
    ///   stache completions --shell fish > ~/.config/fish/completions/stache.fish
    Completions {
        /// The shell to generate completions for.
        #[arg(long, short, value_enum)]
        shell: Shell,
    },

    /// Launch the desktop application.
    ///
    /// Launches Stache in desktop mode. This is equivalent to running `stache`
    /// without any arguments.
    #[command(name = "--desktop", hide = true)]
    Desktop,
}

/// A 1-based screen index for targeting specific displays.
///
/// This newtype provides type safety and validation for screen indices,
/// ensuring they are always 1-based (as users expect) rather than 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct ScreenIndex(usize);

impl ScreenIndex {
    /// Creates a new `ScreenIndex` from a 1-based index.
    ///
    /// # Arguments
    ///
    /// * `index` - A 1-based screen index (1 = first screen, 2 = second, etc.)
    #[must_use]
    pub const fn new(index: usize) -> Self { Self(index) }

    /// Returns the 1-based index value.
    #[must_use]
    #[allow(dead_code)] // Public API for consumers of ScreenIndex
    pub const fn get(self) -> usize { self.0 }

    /// Returns the 0-based index for internal use with arrays/APIs.
    #[must_use]
    pub const fn as_zero_based(self) -> usize { self.0.saturating_sub(1) }
}

impl std::fmt::Display for ScreenIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{}", self.0) }
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
    Index(ScreenIndex),
}

impl FromStr for ScreenTarget {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "all" => Ok(Self::All),
            "main" => Ok(Self::Main),
            _ => s.parse::<usize>().map(|idx| Self::Index(ScreenIndex::new(idx))).map_err(|_| {
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

// ============================================================================
// Event Commands
// ============================================================================

/// Event subcommands for notifying the running Stache instance.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum EventCommands {
    /// Notify Stache that the focused window changed.
    ///
    /// Use when your automation detects a window-focus change.
    /// Triggers Hyprspace queries to refresh current workspace and app state.
    #[command(name = "window-focus-changed")]
    WindowFocusChanged,

    /// Notify Stache that the active workspace changed.
    ///
    /// Requires the new workspace name so Stache can update its Hyprspace view
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
  stache cache clear   # Clear all cached data"#)]
    Clear,

    /// Show the cache directory location.
    ///
    /// Displays the path to the application's cache directory.
    #[command(after_long_help = r#"Examples:
  stache cache path    # Print the cache directory path"#)]
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
  stache audio list              # List all devices in table format
  stache audio list --json       # List all devices in JSON format
  stache audio list --input      # List only input devices
  stache audio list --output     # List only output devices
  stache audio list -io --json   # List all devices in JSON (explicit)"#)]
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
    pub fn execute(&self) -> Result<(), StacheError> {
        match &self.command {
            Commands::Event(event_cmd) => Self::execute_event(event_cmd)?,
            Commands::Wallpaper(wallpaper_cmd) => Self::execute_wallpaper(wallpaper_cmd)?,
            Commands::Cache(cache_cmd) => Self::execute_cache(cache_cmd)?,
            Commands::Audio(audio_cmd) => Self::execute_audio(audio_cmd)?,

            Commands::Reload => {
                if !ipc::send_notification(&StacheNotification::Reload) {
                    return Err(StacheError::IpcError(
                        "Failed to send reload notification to Stache app".to_string(),
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
        generate(generator, &mut cmd, "stache", &mut io::stdout());
    }

    /// Execute event subcommands.
    fn execute_event(cmd: &EventCommands) -> Result<(), StacheError> {
        let notification = match cmd {
            EventCommands::WindowFocusChanged => StacheNotification::WindowFocusChanged,
            EventCommands::WorkspaceChanged { name } => {
                StacheNotification::WorkspaceChanged(name.clone())
            }
        };

        if ipc::send_notification(&notification) {
            Ok(())
        } else {
            Err(StacheError::IpcError(
                "Failed to send notification to Stache app".to_string(),
            ))
        }
    }

    /// Execute cache subcommands.
    fn execute_cache(cmd: &CacheCommands) -> Result<(), StacheError> {
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
                        return Err(StacheError::CacheError(format!(
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
    fn execute_audio(cmd: &AudioCommands) -> Result<(), StacheError> {
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
                        StacheError::AudioError(format!("JSON serialization error: {e}"))
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
    fn execute_wallpaper(cmd: &WallpaperCommands) -> Result<(), StacheError> {
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
    fn init_wallpaper_manager() -> Result<(), StacheError> {
        // Initialize configuration (required for wallpaper settings)
        config::init();

        // Initialize wallpaper manager
        wallpaper::init();

        // Check if wallpaper manager was initialized successfully
        if wallpaper::get_manager().is_none() {
            return Err(StacheError::WallpaperError(
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
    ) -> Result<(), StacheError> {
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

        wallpaper::perform_action(&action).map_err(Self::wallpaper_error_to_stache_error)?;

        println!("Wallpaper set successfully.");
        Ok(())
    }

    /// Execute the wallpaper list command.
    fn execute_wallpaper_list() -> Result<(), StacheError> {
        let wallpapers =
            wallpaper::list_wallpapers().map_err(Self::wallpaper_error_to_stache_error)?;

        if wallpapers.is_empty() {
            println!("No wallpapers found.");
        } else {
            // Output as JSON array for easy parsing
            let json = serde_json::to_string_pretty(&wallpapers).map_err(|e| {
                StacheError::WallpaperError(format!("JSON serialization error: {e}"))
            })?;
            println!("{json}");
        }

        Ok(())
    }

    /// Execute the wallpaper generate-all command.
    fn execute_wallpaper_generate_all() -> Result<(), StacheError> {
        wallpaper::generate_all_streaming(io::stdout())
            .map_err(Self::wallpaper_error_to_stache_error)
    }

    /// Convert `WallpaperManagerError` to `StacheError`.
    #[allow(clippy::needless_pass_by_value)]
    fn wallpaper_error_to_stache_error(err: WallpaperManagerError) -> StacheError {
        StacheError::WallpaperError(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // ScreenTarget tests
    // ========================================================================

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
        assert_eq!(target, ScreenTarget::Index(ScreenIndex::new(2)));
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
        assert_eq!(ScreenTarget::Index(ScreenIndex::new(2)).to_string(), "2");
    }

    #[test]
    fn test_screen_target_default() {
        let target = ScreenTarget::default();
        assert_eq!(target, ScreenTarget::All);
    }

    // ========================================================================
    // ScreenIndex tests
    // ========================================================================

    #[test]
    fn test_screen_index_new_and_get() {
        let idx = ScreenIndex::new(3);
        assert_eq!(idx.get(), 3);
    }

    #[test]
    fn test_screen_index_as_zero_based() {
        assert_eq!(ScreenIndex::new(1).as_zero_based(), 0);
        assert_eq!(ScreenIndex::new(2).as_zero_based(), 1);
        assert_eq!(ScreenIndex::new(5).as_zero_based(), 4);
    }

    #[test]
    fn test_screen_index_as_zero_based_saturating() {
        // Edge case: 0 should saturate to 0 (not underflow)
        assert_eq!(ScreenIndex::new(0).as_zero_based(), 0);
    }

    #[test]
    fn test_screen_index_display() {
        assert_eq!(ScreenIndex::new(1).to_string(), "1");
        assert_eq!(ScreenIndex::new(42).to_string(), "42");
    }

    // ========================================================================
    // Additional ScreenTarget tests
    // ========================================================================

    #[test]
    fn test_screen_target_from_str_mixed_case() {
        let target: ScreenTarget = "AlL".parse().unwrap();
        assert_eq!(target, ScreenTarget::All);

        let target: ScreenTarget = "mAiN".parse().unwrap();
        assert_eq!(target, ScreenTarget::Main);
    }

    #[test]
    fn test_screen_target_from_str_numeric() {
        let target: ScreenTarget = "1".parse().unwrap();
        assert_eq!(target, ScreenTarget::Index(ScreenIndex::new(1)));

        let target: ScreenTarget = "10".parse().unwrap();
        assert_eq!(target, ScreenTarget::Index(ScreenIndex::new(10)));

        let target: ScreenTarget = "99".parse().unwrap();
        assert_eq!(target, ScreenTarget::Index(ScreenIndex::new(99)));
    }

    #[test]
    fn test_screen_target_from_str_negative() {
        let result: Result<ScreenTarget, _> = "-1".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_screen_target_from_str_float() {
        let result: Result<ScreenTarget, _> = "1.5".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_screen_target_from_str_empty() {
        let result: Result<ScreenTarget, _> = "".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_screen_target_from_str_whitespace() {
        // Whitespace is not trimmed, so this should fail
        let result: Result<ScreenTarget, _> = " all ".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_screen_target_serialization_all() {
        let target = ScreenTarget::All;
        let json = serde_json::to_string(&target).unwrap();
        assert_eq!(json, "\"all\"");
    }

    #[test]
    fn test_screen_target_serialization_main() {
        let target = ScreenTarget::Main;
        let json = serde_json::to_string(&target).unwrap();
        assert_eq!(json, "\"main\"");
    }

    #[test]
    fn test_screen_target_serialization_index() {
        let target = ScreenTarget::Index(ScreenIndex::new(3));
        let json = serde_json::to_string(&target).unwrap();
        // Index variant serializes with the index object
        assert!(json.contains("index") || json.contains("3"));
    }

    #[test]
    fn test_screen_target_debug() {
        let target = ScreenTarget::All;
        let debug_str = format!("{:?}", target);
        assert!(debug_str.contains("All"));
    }

    #[test]
    fn test_screen_target_clone() {
        let original = ScreenTarget::Index(ScreenIndex::new(5));
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    // ========================================================================
    // Additional ScreenIndex tests
    // ========================================================================

    #[test]
    fn test_screen_index_copy() {
        let idx = ScreenIndex::new(3);
        let copied = idx; // Copy
        assert_eq!(idx.get(), copied.get());
    }

    #[test]
    fn test_screen_index_clone() {
        let idx = ScreenIndex::new(3);
        let cloned = idx.clone();
        assert_eq!(idx.get(), cloned.get());
    }

    #[test]
    fn test_screen_index_equality() {
        let idx1 = ScreenIndex::new(5);
        let idx2 = ScreenIndex::new(5);
        let idx3 = ScreenIndex::new(6);

        assert_eq!(idx1, idx2);
        assert_ne!(idx1, idx3);
    }

    #[test]
    fn test_screen_index_serialization() {
        let idx = ScreenIndex::new(7);
        let json = serde_json::to_string(&idx).unwrap();
        assert_eq!(json, "7");
    }

    #[test]
    fn test_screen_index_debug() {
        let idx = ScreenIndex::new(42);
        let debug_str = format!("{:?}", idx);
        assert!(debug_str.contains("42"));
    }

    #[test]
    fn test_screen_index_large_value() {
        let idx = ScreenIndex::new(usize::MAX);
        assert_eq!(idx.get(), usize::MAX);
        // as_zero_based should handle large values with saturating subtraction
        assert_eq!(idx.as_zero_based(), usize::MAX - 1);
    }

    // ========================================================================
    // CLI parsing tests
    // ========================================================================

    #[test]
    fn test_cli_parses_reload() {
        let cli = Cli::try_parse_from(["stache", "reload"]).unwrap();
        assert!(matches!(cli.command, Commands::Reload));
    }

    #[test]
    fn test_cli_parses_schema() {
        let cli = Cli::try_parse_from(["stache", "schema"]).unwrap();
        assert!(matches!(cli.command, Commands::Schema));
    }

    #[test]
    fn test_cli_parses_completions_bash() {
        let cli = Cli::try_parse_from(["stache", "completions", "--shell", "bash"]).unwrap();
        match cli.command {
            Commands::Completions { shell } => assert_eq!(shell, Shell::Bash),
            _ => panic!("Expected Completions command"),
        }
    }

    #[test]
    fn test_cli_parses_completions_zsh() {
        let cli = Cli::try_parse_from(["stache", "completions", "--shell", "zsh"]).unwrap();
        match cli.command {
            Commands::Completions { shell } => assert_eq!(shell, Shell::Zsh),
            _ => panic!("Expected Completions command"),
        }
    }

    #[test]
    fn test_cli_parses_completions_fish() {
        let cli = Cli::try_parse_from(["stache", "completions", "--shell", "fish"]).unwrap();
        match cli.command {
            Commands::Completions { shell } => assert_eq!(shell, Shell::Fish),
            _ => panic!("Expected Completions command"),
        }
    }

    #[test]
    fn test_cli_parses_cache_clear() {
        let cli = Cli::try_parse_from(["stache", "cache", "clear"]).unwrap();
        match cli.command {
            Commands::Cache(CacheCommands::Clear) => {}
            _ => panic!("Expected Cache Clear command"),
        }
    }

    #[test]
    fn test_cli_parses_cache_path() {
        let cli = Cli::try_parse_from(["stache", "cache", "path"]).unwrap();
        match cli.command {
            Commands::Cache(CacheCommands::Path) => {}
            _ => panic!("Expected Cache Path command"),
        }
    }

    #[test]
    fn test_cli_parses_audio_list() {
        let cli = Cli::try_parse_from(["stache", "audio", "list"]).unwrap();
        match cli.command {
            Commands::Audio(AudioCommands::List { json, input, output }) => {
                assert!(!json);
                assert!(!input);
                assert!(!output);
            }
            _ => panic!("Expected Audio List command"),
        }
    }

    #[test]
    fn test_cli_parses_audio_list_json() {
        let cli = Cli::try_parse_from(["stache", "audio", "list", "--json"]).unwrap();
        match cli.command {
            Commands::Audio(AudioCommands::List { json, .. }) => {
                assert!(json);
            }
            _ => panic!("Expected Audio List command"),
        }
    }

    #[test]
    fn test_cli_parses_audio_list_input() {
        let cli = Cli::try_parse_from(["stache", "audio", "list", "--input"]).unwrap();
        match cli.command {
            Commands::Audio(AudioCommands::List { input, output, .. }) => {
                assert!(input);
                assert!(!output);
            }
            _ => panic!("Expected Audio List command"),
        }
    }

    #[test]
    fn test_cli_parses_audio_list_output() {
        let cli = Cli::try_parse_from(["stache", "audio", "list", "--output"]).unwrap();
        match cli.command {
            Commands::Audio(AudioCommands::List { input, output, .. }) => {
                assert!(!input);
                assert!(output);
            }
            _ => panic!("Expected Audio List command"),
        }
    }

    #[test]
    fn test_cli_parses_event_window_focus_changed() {
        let cli = Cli::try_parse_from(["stache", "event", "window-focus-changed"]).unwrap();
        match cli.command {
            Commands::Event(EventCommands::WindowFocusChanged) => {}
            _ => panic!("Expected Event WindowFocusChanged command"),
        }
    }

    #[test]
    fn test_cli_parses_event_workspace_changed() {
        let cli = Cli::try_parse_from(["stache", "event", "workspace-changed", "coding"]).unwrap();
        match cli.command {
            Commands::Event(EventCommands::WorkspaceChanged { name }) => {
                assert_eq!(name, "coding");
            }
            _ => panic!("Expected Event WorkspaceChanged command"),
        }
    }

    #[test]
    fn test_cli_parses_wallpaper_list() {
        let cli = Cli::try_parse_from(["stache", "wallpaper", "list"]).unwrap();
        match cli.command {
            Commands::Wallpaper(WallpaperCommands::List) => {}
            _ => panic!("Expected Wallpaper List command"),
        }
    }

    #[test]
    fn test_cli_parses_wallpaper_generate_all() {
        let cli = Cli::try_parse_from(["stache", "wallpaper", "generate-all"]).unwrap();
        match cli.command {
            Commands::Wallpaper(WallpaperCommands::GenerateAll) => {}
            _ => panic!("Expected Wallpaper GenerateAll command"),
        }
    }

    #[test]
    fn test_cli_parses_wallpaper_set_path() {
        let cli =
            Cli::try_parse_from(["stache", "wallpaper", "set", "/path/to/image.jpg"]).unwrap();
        match cli.command {
            Commands::Wallpaper(WallpaperCommands::Set { path, random, screen }) => {
                assert_eq!(path, Some("/path/to/image.jpg".to_string()));
                assert!(!random);
                assert_eq!(screen, ScreenTarget::All);
            }
            _ => panic!("Expected Wallpaper Set command"),
        }
    }

    #[test]
    fn test_cli_parses_wallpaper_set_random() {
        let cli = Cli::try_parse_from(["stache", "wallpaper", "set", "--random"]).unwrap();
        match cli.command {
            Commands::Wallpaper(WallpaperCommands::Set { path, random, .. }) => {
                assert!(path.is_none());
                assert!(random);
            }
            _ => panic!("Expected Wallpaper Set command"),
        }
    }

    #[test]
    fn test_cli_parses_wallpaper_set_screen_main() {
        let cli =
            Cli::try_parse_from(["stache", "wallpaper", "set", "--random", "--screen", "main"])
                .unwrap();
        match cli.command {
            Commands::Wallpaper(WallpaperCommands::Set { screen, .. }) => {
                assert_eq!(screen, ScreenTarget::Main);
            }
            _ => panic!("Expected Wallpaper Set command"),
        }
    }

    #[test]
    fn test_cli_parses_wallpaper_set_screen_index() {
        let cli = Cli::try_parse_from(["stache", "wallpaper", "set", "--random", "--screen", "2"])
            .unwrap();
        match cli.command {
            Commands::Wallpaper(WallpaperCommands::Set { screen, .. }) => {
                assert_eq!(screen, ScreenTarget::Index(ScreenIndex::new(2)));
            }
            _ => panic!("Expected Wallpaper Set command"),
        }
    }

    // ========================================================================
    // APP_VERSION constant test
    // ========================================================================

    #[test]
    fn test_app_version_is_not_empty() {
        assert!(!APP_VERSION.is_empty());
    }

    #[test]
    fn test_app_version_format() {
        // Version should be in semver format (X.Y.Z)
        let parts: Vec<&str> = APP_VERSION.split('.').collect();
        assert!(parts.len() >= 2, "Version should have at least major.minor");
    }
}
