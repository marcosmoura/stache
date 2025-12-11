//! CLI command definitions using Clap.
//!
//! This module defines all CLI commands and their arguments.

use std::io;
use std::str::FromStr;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Generator, Shell, generate};

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
    /// Wallpaper management commands.
    #[command(subcommand)]
    Wallpaper(WallpaperCommands),

    /// Query tiling window manager state.
    ///
    /// Returns information about screens, workspaces, and windows in JSON format.
    #[command(subcommand)]
    Query(QueryCommands),

    /// Workspace management commands.
    ///
    /// Focus, switch layout, balance, or move workspaces between screens.
    #[command(subcommand)]
    Workspace(WorkspaceCommands),

    /// Window management commands.
    ///
    /// Move, focus, resize, or send windows to different workspaces/screens.
    #[command(subcommand)]
    Window(WindowCommands),

    /// Cache management commands.
    ///
    /// Manage the application's cache directory.
    #[command(subcommand)]
    Cache(CacheCommands),

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

// ============================================================================
// Query Commands
// ============================================================================

/// Query subcommands for inspecting tiling state.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum QueryCommands {
    /// List all connected screens.
    ///
    /// Returns JSON array with screen information including name, dimensions,
    /// and whether it's the main display.
    Screens,

    /// List workspaces.
    ///
    /// Returns JSON array with workspace information including name, layout,
    /// assigned screen, and window count.
    Workspaces {
        /// Get a specific workspace by name.
        #[arg(long)]
        name: Option<String>,

        /// Get the currently focused workspace.
        #[arg(long, conflicts_with = "name")]
        focused: bool,

        /// Only show workspaces on the currently focused screen.
        #[arg(long, conflicts_with_all = ["name", "focused"])]
        focused_screen: bool,

        /// Only show workspaces on the specified screen.
        #[arg(long, conflicts_with_all = ["name", "focused", "focused_screen"])]
        screen: Option<String>,
    },

    /// List windows.
    ///
    /// Returns JSON array with window information including title, app name,
    /// workspace, position, and dimensions.
    Windows {
        /// Only show windows in the currently focused workspace.
        #[arg(long)]
        focused_workspace: bool,

        /// Only show windows on the currently focused screen.
        #[arg(long, conflicts_with = "focused_workspace")]
        focused_screen: bool,

        /// Only show windows in the specified workspace.
        #[arg(long, conflicts_with_all = ["focused_workspace", "focused_screen"])]
        workspace: Option<String>,

        /// Only show windows on the specified screen.
        #[arg(long, conflicts_with_all = ["focused_workspace", "focused_screen", "workspace"])]
        screen: Option<String>,
    },
}

// ============================================================================
// Workspace Commands
// ============================================================================

/// Direction for workspace/window navigation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Move/focus up.
    Up,
    /// Move/focus down.
    Down,
    /// Move/focus left.
    Left,
    /// Move/focus right.
    Right,
    /// Next in order.
    Next,
    /// Previous in order.
    Previous,
}

impl FromStr for Direction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "up" => Ok(Self::Up),
            "down" => Ok(Self::Down),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "next" => Ok(Self::Next),
            "previous" | "prev" => Ok(Self::Previous),
            _ => Err(format!(
                "Invalid direction '{s}'. Expected one of: up, down, left, right, next, previous"
            )),
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Up => write!(f, "up"),
            Self::Down => write!(f, "down"),
            Self::Left => write!(f, "left"),
            Self::Right => write!(f, "right"),
            Self::Next => write!(f, "next"),
            Self::Previous => write!(f, "previous"),
        }
    }
}

/// Workspace subcommands for managing workspaces.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum WorkspaceCommands {
    /// Focus a workspace by name or direction.
    ///
    /// Target can be a workspace name (e.g., "1", "coding") or a direction
    /// (next, previous, up, down, left, right).
    #[command(after_long_help = r#"Examples:
  barba workspace focus 1          # Focus workspace named "1"
  barba workspace focus coding     # Focus workspace named "coding"
  barba workspace focus next       # Focus next workspace
  barba workspace focus previous   # Focus previous workspace"#)]
    Focus {
        /// Workspace name or direction (next, previous, up, down, left, right).
        target: String,
    },

    /// Set the layout for the current workspace.
    #[command(after_long_help = r#"Examples:
  barba workspace layout tiling          # Switch to tiling layout
  barba workspace layout monocle         # Switch to monocle layout
  barba workspace layout master          # Switch to master-stack layout
  barba workspace layout split           # Switch to split layout (auto-detect orientation)
  barba workspace layout split-vertical  # Switch to vertical split
  barba workspace layout split-horizontal # Switch to horizontal split
  barba workspace layout floating        # Switch to floating layout"#)]
    Layout {
        /// Layout mode: tiling, monocle, master, split, split-vertical, split-horizontal, floating, scrolling.
        layout: String,
    },

    /// Send the current workspace to another screen.
    #[command(
        name = "send-to-screen",
        after_long_help = r#"Examples:
  barba workspace send-to-screen main    # Send to main screen
  barba workspace send-to-screen left    # Send to screen on the left
  barba workspace send-to-screen DP-1    # Send to screen named "DP-1""#
    )]
    SendToScreen {
        /// Target screen: main, secondary, left, right, up, down, or screen name.
        screen: String,
    },

    /// Balance window sizes in the current layout.
    ///
    /// Resets all windows to their default size ratios for the current layout.
    Balance,
}

// ============================================================================
// Window Commands
// ============================================================================

/// Dimension to resize (width or height).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResizeDimension {
    /// Resize width.
    Width,
    /// Resize height.
    Height,
}

impl FromStr for ResizeDimension {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "width" | "w" => Ok(Self::Width),
            "height" | "h" => Ok(Self::Height),
            _ => Err(format!(
                "Invalid dimension '{s}'. Expected one of: width, height"
            )),
        }
    }
}

/// Window subcommands for managing windows.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum WindowCommands {
    /// Move the focused window in a direction (swap in tiling layouts).
    #[command(after_long_help = r#"Examples:
  barba window move up      # Move/swap window up
  barba window move down    # Move/swap window down
  barba window move left    # Move/swap window left
  barba window move right   # Move/swap window right"#)]
    Move {
        /// Direction to move: up, down, left, right.
        direction: Direction,
    },

    /// Focus a window in a direction.
    #[command(after_long_help = r#"Examples:
  barba window focus up       # Focus window above
  barba window focus down     # Focus window below
  barba window focus left     # Focus window to the left
  barba window focus right    # Focus window to the right
  barba window focus next     # Focus next window in order
  barba window focus previous # Focus previous window"#)]
    Focus {
        /// Direction to focus: up, down, left, right, next, previous.
        direction: Direction,
    },

    /// Send the focused window to a workspace.
    #[command(
        name = "send-to-workspace",
        after_long_help = r#"Examples:
  barba window send-to-workspace 2              # Send to workspace "2" and focus
  barba window send-to-workspace coding         # Send to workspace "coding" and focus
  barba window send-to-workspace 2 --focus=false  # Send without following"#
    )]
    SendToWorkspace {
        /// Target workspace name.
        workspace: String,
        /// Focus the window after sending (switches to the target workspace).
        /// Defaults to true.
        #[arg(long, short = 'f', default_value = "true", action = clap::ArgAction::Set)]
        focus: bool,
    },

    /// Send the focused window to another screen.
    #[command(
        name = "send-to-screen",
        after_long_help = r#"Examples:
  barba window send-to-screen main   # Send to main screen
  barba window send-to-screen left   # Send to screen on the left
  barba window send-to-screen DP-1   # Send to screen named "DP-1""#
    )]
    SendToScreen {
        /// Target screen: main, secondary, left, right, up, down, or screen name.
        screen: String,
    },

    /// Resize the focused window.
    #[command(after_long_help = r#"Examples:
  barba window resize width 100    # Increase width by 100 pixels
  barba window resize width -50    # Decrease width by 50 pixels
  barba window resize height 100   # Increase height by 100 pixels"#)]
    Resize {
        /// Dimension to resize: width, height.
        dimension: ResizeDimension,
        /// Amount to resize in pixels (positive or negative).
        #[arg(allow_hyphen_values = true)]
        amount: i32,
    },

    /// Apply a floating preset to the focused window.
    #[command(after_long_help = r#"Examples:
  barba window preset centered-small  # Apply "centered-small" preset
  barba window preset aligned-left    # Apply "aligned-left" preset"#)]
    Preset {
        /// Name of the floating preset to apply.
        name: String,
    },

    /// Close the focused window.
    ///
    /// Uses the macOS Accessibility API to press the window's close button.
    Close,
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

/// Payload for CLI events sent to the desktop app.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CliEventPayload {
    /// The name of the CLI command/event.
    pub name: String,
    /// Optional data associated with the command.
    pub data: Option<String>,
}

// ============================================================================
// IPC Data Types for Tiling Commands
// ============================================================================

/// Data for query windows command.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryWindowsData {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focused_workspace: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focused_screen: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
}

/// Data for query workspaces command.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryWorkspacesData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focused: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub focused_screen: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
}

/// Data for workspace focus command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkspaceFocusData {
    pub target: String,
}

/// Data for workspace layout command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkspaceLayoutData {
    pub layout: String,
}

/// Data for workspace send-to-screen command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkspaceSendToScreenData {
    pub screen: String,
}

/// Data for window move command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowMoveData {
    pub direction: Direction,
}

/// Data for window focus command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowFocusData {
    pub direction: Direction,
}

/// Data for window send-to-workspace command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowSendToWorkspaceData {
    pub workspace: String,
    pub focus: bool,
}

/// Data for window send-to-screen command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowSendToScreenData {
    pub screen: String,
}

/// Data for window resize command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowResizeData {
    pub dimension: ResizeDimension,
    pub amount: i32,
}

/// Data for window preset command.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowPresetData {
    pub name: String,
}

impl Cli {
    /// Execute the CLI command.
    pub fn execute(&self) -> Result<(), CliError> {
        match &self.command {
            Commands::Wallpaper(wallpaper_cmd) => Self::execute_wallpaper(wallpaper_cmd)?,
            Commands::Query(query_cmd) => Self::execute_query(query_cmd)?,
            Commands::Workspace(workspace_cmd) => Self::execute_workspace(workspace_cmd)?,
            Commands::Window(window_cmd) => Self::execute_window(window_cmd)?,
            Commands::Cache(cache_cmd) => Self::execute_cache(cache_cmd)?,

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

    /// Execute query subcommands.
    fn execute_query(cmd: &QueryCommands) -> Result<(), CliError> {
        match cmd {
            QueryCommands::Screens => {
                let payload = CliEventPayload {
                    name: "tiling-query-screens".to_string(),
                    data: None,
                };
                let response = ipc::send_to_desktop_app_with_response(&payload)?;
                println!("{response}");
            }
            QueryCommands::Workspaces {
                name,
                focused,
                focused_screen,
                screen,
            } => {
                let data = QueryWorkspacesData {
                    name: name.clone(),
                    focused: *focused,
                    focused_screen: *focused_screen,
                    screen: screen.clone(),
                };
                let payload = CliEventPayload {
                    name: "tiling-query-workspaces".to_string(),
                    data: Some(serde_json::to_string(&data).unwrap()),
                };
                let response = ipc::send_to_desktop_app_with_response(&payload)?;
                println!("{response}");
            }
            QueryCommands::Windows {
                focused_workspace,
                focused_screen,
                workspace,
                screen,
            } => {
                let data = QueryWindowsData {
                    focused_workspace: *focused_workspace,
                    focused_screen: *focused_screen,
                    workspace: workspace.clone(),
                    screen: screen.clone(),
                };
                let payload = CliEventPayload {
                    name: "tiling-query-windows".to_string(),
                    data: Some(serde_json::to_string(&data).unwrap()),
                };
                let response = ipc::send_to_desktop_app_with_response(&payload)?;
                println!("{response}");
            }
        }
        Ok(())
    }

    /// Execute workspace subcommands.
    fn execute_workspace(cmd: &WorkspaceCommands) -> Result<(), CliError> {
        match cmd {
            WorkspaceCommands::Focus { target } => {
                let data = WorkspaceFocusData { target: target.clone() };
                let payload = CliEventPayload {
                    name: "tiling-workspace-focus".to_string(),
                    data: Some(serde_json::to_string(&data).unwrap()),
                };
                ipc::send_to_desktop_app(&payload)?;
            }
            WorkspaceCommands::Layout { layout } => {
                if layout.parse::<barba_shared::LayoutMode>().is_err() {
                    return Err(CliError::InvalidArguments(format!(
                        "Invalid layout '{layout}'. Expected one of: tiling, monocle, master, split, split-vertical, split-horizontal, floating, scrolling"
                    )));
                }
                let data = WorkspaceLayoutData { layout: layout.clone() };
                let payload = CliEventPayload {
                    name: "tiling-workspace-layout".to_string(),
                    data: Some(serde_json::to_string(&data).unwrap()),
                };
                ipc::send_to_desktop_app(&payload)?;
            }
            WorkspaceCommands::SendToScreen { screen } => {
                let data = WorkspaceSendToScreenData { screen: screen.clone() };
                let payload = CliEventPayload {
                    name: "tiling-workspace-send-to-screen".to_string(),
                    data: Some(serde_json::to_string(&data).unwrap()),
                };
                ipc::send_to_desktop_app(&payload)?;
            }
            WorkspaceCommands::Balance => {
                let payload = CliEventPayload {
                    name: "tiling-workspace-balance".to_string(),
                    data: None,
                };
                ipc::send_to_desktop_app(&payload)?;
            }
        }
        Ok(())
    }

    /// Execute window subcommands.
    fn execute_window(cmd: &WindowCommands) -> Result<(), CliError> {
        match cmd {
            WindowCommands::Move { direction } => {
                let data = WindowMoveData { direction: direction.clone() };
                let payload = CliEventPayload {
                    name: "tiling-window-move".to_string(),
                    data: Some(serde_json::to_string(&data).unwrap()),
                };
                ipc::send_to_desktop_app(&payload)?;
            }
            WindowCommands::Focus { direction } => {
                let data = WindowFocusData { direction: direction.clone() };
                let payload = CliEventPayload {
                    name: "tiling-window-focus".to_string(),
                    data: Some(serde_json::to_string(&data).unwrap()),
                };
                ipc::send_to_desktop_app(&payload)?;
            }
            WindowCommands::SendToWorkspace { workspace, focus } => {
                let data = WindowSendToWorkspaceData {
                    workspace: workspace.clone(),
                    focus: *focus,
                };
                let payload = CliEventPayload {
                    name: "tiling-window-send-to-workspace".to_string(),
                    data: Some(serde_json::to_string(&data).unwrap()),
                };
                ipc::send_to_desktop_app(&payload)?;
            }
            WindowCommands::SendToScreen { screen } => {
                let data = WindowSendToScreenData { screen: screen.clone() };
                let payload = CliEventPayload {
                    name: "tiling-window-send-to-screen".to_string(),
                    data: Some(serde_json::to_string(&data).unwrap()),
                };
                ipc::send_to_desktop_app(&payload)?;
            }
            WindowCommands::Resize { dimension, amount } => {
                let data = WindowResizeData {
                    dimension: dimension.clone(),
                    amount: *amount,
                };
                let payload = CliEventPayload {
                    name: "tiling-window-resize".to_string(),
                    data: Some(serde_json::to_string(&data).unwrap()),
                };
                ipc::send_to_desktop_app(&payload)?;
            }
            WindowCommands::Preset { name } => {
                let data = WindowPresetData { name: name.clone() };
                let payload = CliEventPayload {
                    name: "tiling-window-preset".to_string(),
                    data: Some(serde_json::to_string(&data).unwrap()),
                };
                ipc::send_to_desktop_app(&payload)?;
            }
            WindowCommands::Close => {
                let payload = CliEventPayload {
                    name: "tiling-window-close".to_string(),
                    data: None,
                };
                ipc::send_to_desktop_app(&payload)?;
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
