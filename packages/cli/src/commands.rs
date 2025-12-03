//! CLI command definitions using Clap.
//!
//! This module defines all CLI commands and their arguments.

use clap::{Parser, Subcommand};

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

    /// Reload Barba configuration.
    ///
    /// Reloads the configuration file and applies changes without restarting
    /// the application.
    Reload,

    /// Generate JSON schema for the configuration file.
    ///
    /// Outputs a JSON Schema to stdout that describes the structure of the
    /// Barba configuration file. Can be redirected to a file for use with
    /// editors that support JSON Schema validation.
    #[command(name = "generate-schema")]
    GenerateSchema,
}

/// Wallpaper subcommands.
#[derive(Subcommand, Debug)]
pub enum WallpaperCommands {
    /// Change the desktop wallpaper.
    ///
    /// Manually change the wallpaper. Supports: 'next' (next in sequence),
    /// 'previous' (previous in sequence), 'random' (random selection),
    /// or a numeric index.
    Set {
        /// Wallpaper action: next, previous, random, or index number.
        action: String,
    },

    /// Pre-generate all wallpapers.
    ///
    /// Processes all wallpapers from the configuration and stores them in the
    /// cache directory. Useful for pre-caching wallpapers to avoid delays
    /// when switching.
    #[command(name = "generate-all")]
    GenerateAll,
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

            Commands::Wallpaper(wallpaper_cmd) => match wallpaper_cmd {
                WallpaperCommands::Set { action } => {
                    // Validate the action
                    if !is_valid_wallpaper_action(action) {
                        return Err(CliError::InvalidWallpaperAction(action.clone()));
                    }

                    let payload = CliEventPayload {
                        name: "wallpaper-set".to_string(),
                        data: Some(action.clone()),
                    };
                    ipc::send_to_desktop_app(&payload)?;
                }
                WallpaperCommands::GenerateAll => {
                    let payload = CliEventPayload {
                        name: "wallpaper-generate-all".to_string(),
                        data: None,
                    };
                    ipc::send_to_desktop_app(&payload)?;
                }
            },

            Commands::Reload => {
                let payload = CliEventPayload {
                    name: "reload".to_string(),
                    data: None,
                };
                ipc::send_to_desktop_app(&payload)?;
            }

            Commands::GenerateSchema => {
                // Generate schema locally using the shared config types
                // This doesn't require the desktop app to be running
                let schema = barba_shared::generate_schema_json();
                println!("{schema}");
            }
        }

        Ok(())
    }
}

/// Validates a wallpaper action string.
fn is_valid_wallpaper_action(action: &str) -> bool {
    matches!(action, "next" | "previous" | "random") || action.parse::<usize>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_wallpaper_actions() {
        assert!(is_valid_wallpaper_action("next"));
        assert!(is_valid_wallpaper_action("previous"));
        assert!(is_valid_wallpaper_action("random"));
        assert!(is_valid_wallpaper_action("0"));
        assert!(is_valid_wallpaper_action("42"));
    }

    #[test]
    fn test_invalid_wallpaper_actions() {
        assert!(!is_valid_wallpaper_action("invalid"));
        assert!(!is_valid_wallpaper_action(""));
        assert!(!is_valid_wallpaper_action("-1"));
    }

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
    fn test_wallpaper_action_numeric_index() {
        assert!(is_valid_wallpaper_action("0"));
        assert!(is_valid_wallpaper_action("1"));
        assert!(is_valid_wallpaper_action("100"));
        assert!(is_valid_wallpaper_action("999"));
    }

    #[test]
    fn test_wallpaper_action_case_sensitive() {
        // Actions are case-sensitive
        assert!(!is_valid_wallpaper_action("Next"));
        assert!(!is_valid_wallpaper_action("NEXT"));
        assert!(!is_valid_wallpaper_action("Previous"));
        assert!(!is_valid_wallpaper_action("Random"));
    }
}
