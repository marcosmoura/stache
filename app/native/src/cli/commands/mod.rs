//! CLI command definitions using Clap.
//!
//! This module defines all CLI commands and their arguments, organized into
//! domain-specific submodules:
//!
//! - `audio` - Audio device management commands
//! - `cache` - Cache management commands
//! - `tiling` - Tiling window manager commands
//! - `types` - Shared types used across commands
//! - `wallpaper` - Wallpaper management commands

use std::io;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Generator, Shell, generate};

use crate::error::StacheError;
use crate::platform::ipc::{self, StacheNotification};
use crate::{config, schema};

pub mod audio;
pub mod cache;
pub mod config_cmd;
pub mod tiling;
pub mod types;
pub mod wallpaper;

// Re-export commonly used types for convenience
pub use audio::AudioCommands;
pub use cache::CacheCommands;
pub use config_cmd::ConfigCommands;
pub use tiling::TilingCommands;
pub use wallpaper::WallpaperCommands;

/// Application version from Cargo.toml.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stache CLI - Command-line interface for Stache.
#[derive(Parser, Debug)]
#[command(name = "stache")]
#[command(author, version = APP_VERSION, about, long_about = None)]
#[command(propagate_version = true)]
pub struct Cli {
    /// Path to a custom configuration file.
    ///
    /// Overrides the default configuration file search paths.
    /// Supports JSONC format (JSON with comments).
    #[arg(long, short, global = true, value_name = "PATH")]
    pub config: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

/// Available CLI commands.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum Commands {
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

    /// Tiling window manager commands.
    ///
    /// Manage windows, workspaces, and query tiling state.
    #[command(subcommand)]
    Tiling(TilingCommands),

    /// Configuration file management commands.
    ///
    /// Initialize, view, and manage the configuration file.
    #[command(subcommand)]
    Config(ConfigCommands),

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

impl Cli {
    /// Returns the custom config path if specified via --config flag.
    #[must_use]
    pub fn config_path(&self) -> Option<std::path::PathBuf> {
        self.config.as_ref().map(std::path::PathBuf::from)
    }

    /// Execute the CLI command.
    ///
    /// # Errors
    ///
    /// Returns an error if the command execution fails.
    pub fn execute(&self) -> Result<(), StacheError> {
        // Set custom config path if provided
        if let Some(ref path) = self.config {
            let path_buf = std::path::PathBuf::from(path);
            if !path_buf.exists() {
                return Err(StacheError::ConfigError(format!(
                    "Configuration file not found: {path}"
                )));
            }
            config::set_custom_config_path(path_buf);
        }

        match &self.command {
            Commands::Wallpaper(cmd) => wallpaper::execute(cmd),
            Commands::Cache(cmd) => cache::execute(cmd),
            Commands::Audio(cmd) => audio::execute(cmd),
            Commands::Tiling(cmd) => tiling::execute(cmd),
            Commands::Config(cmd) => config_cmd::execute(cmd),

            Commands::Reload => {
                if !ipc::send_notification(&StacheNotification::Reload) {
                    return Err(StacheError::IpcError(
                        "Failed to send reload notification to Stache app".to_string(),
                    ));
                }
                Ok(())
            }

            Commands::Schema => {
                let schema_output = schema::print_schema();
                println!("{schema_output}");
                Ok(())
            }

            Commands::Completions { shell } => {
                Self::print_completions(*shell);
                Ok(())
            }

            Commands::Desktop => {
                // This case should not be reached as main.rs handles --desktop
                unreachable!("Desktop mode should be handled by main.rs");
            }
        }
    }

    /// Print shell completions to stdout.
    fn print_completions<G: Generator>(generator: G) {
        let mut cmd = Self::command();
        generate(generator, &mut cmd, "stache", &mut io::stdout());
    }
}

#[cfg(test)]
mod tests {
    use super::tiling::TilingQueryCommands;
    use super::types::{CliLayoutType, ScreenIndex, ScreenTarget};
    use super::*;

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
        assert!(
            APP_VERSION.split('.').count() >= 2,
            "Version should have at least major.minor"
        );
    }

    // ========================================================================
    // --config flag tests
    // ========================================================================

    #[test]
    fn test_cli_parses_config_flag() {
        let cli =
            Cli::try_parse_from(["stache", "--config", "/path/to/config.json", "schema"]).unwrap();
        assert_eq!(cli.config, Some("/path/to/config.json".to_string()));
        assert!(matches!(cli.command, Commands::Schema));
    }

    #[test]
    fn test_cli_parses_config_short_flag() {
        let cli = Cli::try_parse_from(["stache", "-c", "/path/to/config.json", "reload"]).unwrap();
        assert_eq!(cli.config, Some("/path/to/config.json".to_string()));
        assert!(matches!(cli.command, Commands::Reload));
    }

    #[test]
    fn test_cli_parses_config_flag_after_subcommand() {
        // The --config flag is global so can appear before or after subcommand
        let cli =
            Cli::try_parse_from(["stache", "schema", "--config", "/path/to/config.json"]).unwrap();
        assert_eq!(cli.config, Some("/path/to/config.json".to_string()));
    }

    #[test]
    fn test_cli_parses_no_config_flag() {
        let cli = Cli::try_parse_from(["stache", "schema"]).unwrap();
        assert!(cli.config.is_none());
    }

    #[test]
    fn test_cli_config_path_returns_pathbuf() {
        let cli =
            Cli::try_parse_from(["stache", "--config", "/path/to/config.json", "schema"]).unwrap();
        let path = cli.config_path();
        assert!(path.is_some());
        assert_eq!(path.unwrap().to_str().unwrap(), "/path/to/config.json");
    }

    #[test]
    fn test_cli_config_path_returns_none_when_not_specified() {
        let cli = Cli::try_parse_from(["stache", "schema"]).unwrap();
        assert!(cli.config_path().is_none());
    }

    // ========================================================================
    // Tiling command parsing tests (integration)
    // ========================================================================

    #[test]
    fn test_cli_parses_tiling_query_screens() {
        let cli = Cli::try_parse_from(["stache", "tiling", "query", "screens"]).unwrap();
        match cli.command {
            Commands::Tiling(TilingCommands::Query { json, detailed, command }) => {
                assert!(!json);
                assert!(!detailed);
                assert!(matches!(command, Some(TilingQueryCommands::Screens)));
            }
            _ => panic!("Expected Tiling Query command"),
        }
    }

    #[test]
    fn test_cli_parses_tiling_window_focus() {
        let cli = Cli::try_parse_from(["stache", "tiling", "window", "--focus", "left"]).unwrap();
        match cli.command {
            Commands::Tiling(TilingCommands::Window(args)) => {
                assert_eq!(args.focus, Some("left".to_string()));
            }
            _ => panic!("Expected Tiling Window command"),
        }
    }

    #[test]
    fn test_cli_parses_tiling_workspace_layout() {
        let cli =
            Cli::try_parse_from(["stache", "tiling", "workspace", "--layout", "dwindle"]).unwrap();
        match cli.command {
            Commands::Tiling(TilingCommands::Workspace(args)) => {
                assert_eq!(args.layout, Some(CliLayoutType::Dwindle));
            }
            _ => panic!("Expected Tiling Workspace command"),
        }
    }
}
