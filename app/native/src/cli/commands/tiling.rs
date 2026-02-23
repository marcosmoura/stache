//! Tiling CLI commands.
//!
//! This module contains the tiling window manager subcommands for managing
//! windows, workspaces, and querying tiling state.

use clap::Subcommand;
use colored::Colorize;
use tabled::settings::object::Columns;
use tabled::settings::{Alignment, Modify, Style};
use tabled::{Table, Tabled};

use super::types::{CliLayoutType, Direction};
use crate::cli::output;
use crate::error::StacheError;
use crate::platform::ipc::{self, StacheNotification};
use crate::platform::ipc_socket::{self, IpcError, IpcQuery, IpcResponse};
use crate::tiling;

/// Tiling window manager subcommands.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum TilingCommands {
    /// Query tiling state (screens, workspaces, windows, apps).
    ///
    /// Without a subcommand, outputs all query results.
    /// Use --json for JSON output instead of human-readable tables.
    #[command(subcommand_negates_reqs = true)]
    Query {
        /// Output in JSON format instead of table format.
        #[arg(long, short = 'j', global = true)]
        json: bool,

        /// Show detailed information (more columns/fields).
        #[arg(long, short = 'd', global = true)]
        detailed: bool,

        /// Query subcommand (screens, workspaces, windows, apps).
        #[command(subcommand)]
        command: Option<TilingQueryCommands>,
    },

    /// Window manipulation commands.
    ///
    /// Use flags to specify the window operation to perform.
    Window(TilingWindowArgs),

    /// Workspace commands.
    ///
    /// Use flags to specify the workspace operation to perform.
    Workspace(TilingWorkspaceArgs),
}

/// Tiling query subcommands.
#[derive(Subcommand, Debug)]
#[command(next_display_order = None)]
pub enum TilingQueryCommands {
    /// List all connected screens.
    ///
    /// Returns information about all screens including name, frame,
    /// and whether it's the main or built-in display.
    #[command(after_long_help = r#"Examples:
  stache tiling query screens         # List all screens
  stache tiling query --json screens  # Output as JSON"#)]
    Screens,

    /// List workspaces.
    ///
    /// Returns information about workspaces including name, layout,
    /// screen assignment, and visibility state.
    #[command(after_long_help = r#"Examples:
  stache tiling query workspaces                    # List all workspaces
  stache tiling query workspaces --focused-screen   # Workspaces on focused screen
  stache tiling query workspaces --screen main      # Workspaces on main screen"#)]
    Workspaces {
        /// Only show workspaces on the focused screen.
        #[arg(long, conflicts_with = "screen")]
        focused_screen: bool,

        /// Filter by screen name (main, secondary, or screen name).
        #[arg(long)]
        screen: Option<String>,
    },

    /// List managed windows.
    ///
    /// Returns information about tracked windows including their
    /// workspace assignment, position, and state.
    #[command(after_long_help = r#"Examples:
  stache tiling query windows                       # List all windows
  stache tiling query windows --focused-workspace   # Windows in focused workspace
  stache tiling query windows --workspace coding    # Windows in 'coding' workspace
  stache tiling query -d windows                    # Show detailed window info"#)]
    Windows {
        /// Only show windows on the focused screen.
        #[arg(long, conflicts_with_all = ["screen", "workspace", "focused_workspace"])]
        focused_screen: bool,

        /// Only show windows in the focused workspace.
        #[arg(long, conflicts_with_all = ["screen", "workspace", "focused_screen"])]
        focused_workspace: bool,

        /// Filter by screen name.
        #[arg(long, conflicts_with_all = ["focused_screen", "focused_workspace"])]
        screen: Option<String>,

        /// Filter by workspace name.
        #[arg(long, conflicts_with_all = ["focused_screen", "focused_workspace"])]
        workspace: Option<String>,
    },

    /// List all running applications.
    ///
    /// Returns information about all running applications that can own windows,
    /// excluding apps that match ignore rules in the configuration.
    #[command(after_long_help = r#"Examples:
  stache tiling query apps            # List all running apps
  stache tiling query --json apps     # Output as JSON
  stache tiling query -d apps         # Show detailed app info"#)]
    Apps,
}

/// Tiling window command arguments.
///
/// Multiple operations can be combined in a single command.
/// Operations are executed in order: focus -> swap -> preset -> resize -> send.
#[derive(Debug, clap::Args)]
#[command(after_long_help = r#"Examples:
  stache tiling window --focus left                            # Focus window to the left
  stache tiling window --swap down                             # Swap with window below
  stache tiling window --resize width 100                      # Increase width by 100px
  stache tiling window --resize width 100 --resize height 50   # Resize both dimensions
  stache tiling window --swap right --resize width 150         # Swap then resize
  stache tiling window --send-to-screen main                   # Send to main screen"#)]
pub struct TilingWindowArgs {
    /// Focus a window in a direction or by ID.
    ///
    /// Direction: up, down, left, right, previous, next.
    /// Or specify a window ID directly.
    #[arg(long, value_name = "DIRECTION|WINDOW_ID")]
    pub focus: Option<String>,

    /// Swap focused window with another in a direction.
    ///
    /// Direction: up, down, left, right, previous, next.
    #[arg(long, value_name = "DIRECTION", value_enum)]
    pub swap: Option<Direction>,

    /// Apply a floating preset to the focused window.
    ///
    /// Uses a preset defined in the configuration file.
    #[arg(long, value_name = "PRESET_NAME")]
    pub preset: Option<String>,

    /// Resize the focused window.
    ///
    /// Specify dimension (width/height) and amount in pixels.
    /// Positive values increase size, negative values decrease.
    /// Can be specified multiple times to resize both dimensions.
    #[arg(long, value_names = ["DIMENSION", "AMOUNT"], num_args = 2, action = clap::ArgAction::Append, allow_negative_numbers = true)]
    pub resize: Vec<String>,

    /// Send focused window to another screen.
    ///
    /// Target: main, secondary, or screen name.
    #[arg(long = "send-to-screen", value_name = "SCREEN")]
    pub send_to_screen: Option<String>,

    /// Send focused window to another workspace.
    ///
    /// The window will be hidden if the target workspace is not visible.
    #[arg(long = "send-to-workspace", value_name = "WORKSPACE")]
    pub send_to_workspace: Option<String>,
}

/// Tiling workspace command arguments.
///
/// Multiple operations can be combined in a single command.
/// Operations are executed in order: focus -> layout -> balance -> send.
#[derive(Debug, clap::Args)]
#[command(after_long_help = r#"Examples:
  stache tiling workspace --balance                    # Balance windows in focused workspace
  stache tiling workspace --focus coding               # Switch to 'coding' workspace
  stache tiling workspace --layout dwindle                 # Use DWINDLE layout
  stache tiling workspace --layout dwindle --balance       # Change layout then balance
  stache tiling workspace --send-to-screen main        # Move workspace to main screen"#)]
pub struct TilingWorkspaceArgs {
    /// Focus a workspace by name.
    ///
    /// Switches to the specified workspace, hiding windows from
    /// the previous workspace and showing windows from the target.
    #[arg(long, value_name = "WORKSPACE")]
    pub focus: Option<String>,

    /// Change the layout of the focused workspace.
    ///
    /// Layout: dwindle, split, split-vertical, split-horizontal, monocle, master, grid, floating.
    #[arg(long, value_name = "LAYOUT", value_enum)]
    pub layout: Option<CliLayoutType>,

    /// Balance windows in the focused workspace.
    ///
    /// Resets all window size ratios to their default values,
    /// distributing space evenly according to the current layout.
    #[arg(long)]
    pub balance: bool,

    /// Send focused workspace to another screen.
    ///
    /// Target: main, secondary, or screen name.
    #[arg(long = "send-to-screen", value_name = "SCREEN")]
    pub send_to_screen: Option<String>,
}

/// Execute tiling subcommands.
pub fn execute(cmd: &TilingCommands) -> Result<(), StacheError> {
    match cmd {
        TilingCommands::Query { json, detailed, command } => {
            execute_query(*json, *detailed, command.as_ref())
        }
        TilingCommands::Window(args) => execute_window(args),
        TilingCommands::Workspace(args) => execute_workspace(args),
    }
}

/// Execute tiling query subcommands.
#[allow(clippy::unnecessary_wraps)] // Will return errors when fully implemented
fn execute_query(
    json: bool,
    detailed: bool,
    cmd: Option<&TilingQueryCommands>,
) -> Result<(), StacheError> {
    match cmd {
        None => {
            // No subcommand: show help
            let mut cmd = TilingQueryCommands::augment_subcommands(clap::Command::new("query"));
            cmd.print_help().ok();
            println!();
            Ok(())
        }
        Some(TilingQueryCommands::Screens) => {
            execute_query_screens(json);
            Ok(())
        }
        Some(TilingQueryCommands::Workspaces { focused_screen, screen }) => {
            execute_query_workspaces(json, *focused_screen, screen.as_deref());
            Ok(())
        }
        Some(TilingQueryCommands::Windows {
            focused_screen,
            focused_workspace,
            screen,
            workspace,
        }) => {
            execute_query_windows(
                json,
                detailed,
                *focused_screen,
                *focused_workspace,
                screen.as_deref(),
                workspace.as_deref(),
            );
            Ok(())
        }
        Some(TilingQueryCommands::Apps) => {
            execute_query_apps(json, detailed);
            Ok(())
        }
    }
}

/// Execute tiling query screens command.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn execute_query_screens(json: bool) {
    #[derive(Tabled)]
    struct ScreenRow {
        #[tabled(rename = "ID")]
        id: u32,
        #[tabled(rename = "Name")]
        name: String,
        #[tabled(rename = "Resolution")]
        resolution: String,
        #[tabled(rename = "Position")]
        position: String,
        #[tabled(rename = "Scale")]
        scale: String,
        #[tabled(rename = "Main")]
        main: String,
        #[tabled(rename = "Built-in")]
        builtin: String,
    }

    // Send IPC query to app
    let response = match ipc_socket::send_query(IpcQuery::Screens) {
        Ok(r) => r,
        Err(IpcError::AppNotRunning) => {
            if json {
                println!(r#"{{"error":"Stache app is not running"}}"#);
            } else {
                println!("{}", "Stache app is not running.".red());
            }
            return;
        }
        Err(e) => {
            if json {
                println!(r#"{{"error":"{e}"}}"#);
            } else {
                println!("{} {e}", "Error:".red());
            }
            return;
        }
    };

    match response {
        IpcResponse::Success { data } => {
            if json {
                output::print_highlighted_json(&data);
            } else {
                // Parse screens from response
                let screens: Vec<tiling::Screen> = serde_json::from_value(data).unwrap_or_default();

                if screens.is_empty() {
                    println!("{}", "No screens detected.".dimmed());
                    return;
                }

                let rows: Vec<ScreenRow> = screens
                    .iter()
                    .map(|s| {
                        let width = s.frame.width as u32;
                        let height = s.frame.height as u32;
                        let x = s.frame.x as i32;
                        let y = s.frame.y as i32;
                        let scale = s.scale_factor;
                        ScreenRow {
                            id: s.id,
                            name: s.name.clone(),
                            resolution: format!("{width}x{height}"),
                            position: format!("{x}, {y}"),
                            scale: format!("{scale}x"),
                            main: output::format_bool(s.is_main),
                            builtin: output::format_bool(s.is_builtin),
                        }
                    })
                    .collect();

                let table = Table::new(rows)
                    .with(Style::rounded())
                    .with(Modify::new(Columns::first()).with(Alignment::right()))
                    .with(Modify::new(Columns::new(2..5)).with(Alignment::right()))
                    .with(Modify::new(Columns::new(5..7)).with(Alignment::center()))
                    .to_string();

                let count = screens.len();
                println!("{}", format!("Screens ({count})").bold());
                println!("{table}");
            }
        }
        IpcResponse::Error { error } => {
            if json {
                println!(r#"{{"error":"{error}"}}"#);
            } else {
                println!("{} {error}", "Error:".red());
            }
        }
    }
}

/// Execute tiling query workspaces command.
#[allow(clippy::cast_possible_truncation)]
fn execute_query_workspaces(json: bool, focused_screen: bool, screen: Option<&str>) {
    #[derive(Tabled)]
    struct WorkspaceRow {
        #[tabled(rename = "Name")]
        name: String,
        #[tabled(rename = "Screen")]
        screen: String,
        #[tabled(rename = "Layout")]
        layout: String,
        #[tabled(rename = "Windows")]
        windows: usize,
        #[tabled(rename = "Visible")]
        visible: String,
        #[tabled(rename = "Focused")]
        focused: String,
    }

    // Send IPC query to app
    let query = IpcQuery::Workspaces {
        screen: screen.map(String::from),
        focused_screen,
    };

    let response = match ipc_socket::send_query(query) {
        Ok(r) => r,
        Err(IpcError::AppNotRunning) => {
            if json {
                println!(r#"{{"error":"Stache app is not running"}}"#);
            } else {
                println!("{}", "Stache app is not running.".red());
            }
            return;
        }
        Err(e) => {
            if json {
                println!(r#"{{"error":"{e}"}}"#);
            } else {
                println!("{} {e}", "Error:".red());
            }
            return;
        }
    };

    match response {
        IpcResponse::Success { data } => {
            if json {
                output::print_highlighted_json(&data);
            } else {
                // Parse workspaces from response
                let workspaces: Vec<serde_json::Value> =
                    serde_json::from_value(data).unwrap_or_default();

                if workspaces.is_empty() {
                    println!("{}", "No workspaces found.".dimmed());
                    return;
                }

                let rows: Vec<WorkspaceRow> = workspaces
                    .iter()
                    .map(|ws| WorkspaceRow {
                        name: ws["name"].as_str().unwrap_or("?").to_string(),
                        screen: output::truncate(ws["screenName"].as_str().unwrap_or("?"), 15),
                        layout: ws["layout"].as_str().unwrap_or("?").to_string(),
                        windows: ws["windowCount"].as_u64().unwrap_or(0) as usize,
                        visible: output::format_bool(ws["isVisible"].as_bool().unwrap_or(false)),
                        focused: output::format_bool(ws["isFocused"].as_bool().unwrap_or(false)),
                    })
                    .collect();

                let table = Table::new(rows)
                    .with(Style::rounded())
                    .with(Modify::new(Columns::new(3..4)).with(Alignment::right()))
                    .with(Modify::new(Columns::new(4..6)).with(Alignment::center()))
                    .to_string();

                let count = workspaces.len();
                println!("{}", format!("Workspaces ({count})").bold());
                println!("{table}");
            }
        }
        IpcResponse::Error { error } => {
            if json {
                println!(r#"{{"error":"{error}"}}"#);
            } else {
                println!("{} {error}", "Error:".red());
            }
        }
    }
}

/// Execute tiling query windows command.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::fn_params_excessive_bools
)]
fn execute_query_windows(
    json: bool,
    detailed: bool,
    focused_screen: bool,
    focused_workspace: bool,
    screen: Option<&str>,
    workspace: Option<&str>,
) {
    #[derive(Tabled)]
    struct WindowRow {
        #[tabled(rename = "ID")]
        id: u32,
        #[tabled(rename = "App")]
        app: String,
        #[tabled(rename = "Title")]
        title: String,
        #[tabled(rename = "Workspace")]
        workspace: String,
        #[tabled(rename = "Frame")]
        frame: String,
        #[tabled(rename = "Focused")]
        focused: String,
    }

    #[derive(Tabled)]
    struct WindowRowDetailed {
        #[tabled(rename = "ID")]
        id: u32,
        #[tabled(rename = "PID")]
        pid: i32,
        #[tabled(rename = "App")]
        app: String,
        #[tabled(rename = "Bundle ID")]
        bundle_id: String,
        #[tabled(rename = "Title")]
        title: String,
        #[tabled(rename = "Workspace")]
        workspace: String,
        #[tabled(rename = "Frame")]
        frame: String,
        #[tabled(rename = "Min")]
        minimized: String,
        #[tabled(rename = "Float")]
        floating: String,
        #[tabled(rename = "Focus")]
        focused: String,
    }

    // Send IPC query to app
    let query = IpcQuery::Windows {
        screen: screen.map(String::from),
        workspace: workspace.map(String::from),
        focused_screen,
        focused_workspace,
        detailed,
    };

    let response = match ipc_socket::send_query(query) {
        Ok(r) => r,
        Err(IpcError::AppNotRunning) => {
            if json {
                println!(r#"{{"error":"Stache app is not running"}}"#);
            } else {
                println!("{}", "Stache app is not running.".red());
            }
            return;
        }
        Err(e) => {
            if json {
                println!(r#"{{"error":"{e}"}}"#);
            } else {
                println!("{} {e}", "Error:".red());
            }
            return;
        }
    };

    match response {
        IpcResponse::Success { data } => {
            if json {
                output::print_highlighted_json(&data);
            } else {
                // Parse windows from response
                let windows: Vec<serde_json::Value> =
                    serde_json::from_value(data).unwrap_or_default();

                if windows.is_empty() {
                    println!("{}", "No windows found.".dimmed());
                    return;
                }

                let count = windows.len();
                println!("{}", format!("Windows ({count})").bold());

                if detailed {
                    let rows: Vec<WindowRowDetailed> = windows
                        .iter()
                        .map(|w| {
                            let frame = &w["frame"];
                            WindowRowDetailed {
                                id: w["id"].as_u64().unwrap_or(0) as u32,
                                pid: w["pid"].as_i64().unwrap_or(0) as i32,
                                app: output::truncate(w["appName"].as_str().unwrap_or("?"), 15),
                                bundle_id: output::truncate(w["appId"].as_str().unwrap_or("?"), 25),
                                title: output::truncate(
                                    w["title"]
                                        .as_str()
                                        .filter(|s| !s.is_empty())
                                        .unwrap_or("(no title)"),
                                    25,
                                ),
                                workspace: w["workspace"].as_str().unwrap_or("?").to_string(),
                                frame: format!(
                                    "{}x{} @ {}, {}",
                                    frame["width"].as_f64().unwrap_or(0.0) as u32,
                                    frame["height"].as_f64().unwrap_or(0.0) as u32,
                                    frame["x"].as_f64().unwrap_or(0.0) as i32,
                                    frame["y"].as_f64().unwrap_or(0.0) as i32
                                ),
                                minimized: output::format_bool(
                                    w["isMinimized"].as_bool().unwrap_or(false),
                                ),
                                floating: output::format_bool(
                                    w["isFloating"].as_bool().unwrap_or(false),
                                ),
                                focused: output::format_bool(
                                    w["isFocused"].as_bool().unwrap_or(false),
                                ),
                            }
                        })
                        .collect();

                    let table = Table::new(rows)
                        .with(Style::rounded())
                        .with(Modify::new(Columns::one(0)).with(Alignment::right()))
                        .with(Modify::new(Columns::one(1)).with(Alignment::right()))
                        .with(Modify::new(Columns::new(7..10)).with(Alignment::center()))
                        .to_string();

                    println!("{table}");
                } else {
                    let rows: Vec<WindowRow> = windows
                        .iter()
                        .map(|w| {
                            let frame = &w["frame"];
                            WindowRow {
                                id: w["id"].as_u64().unwrap_or(0) as u32,
                                app: output::truncate(w["appName"].as_str().unwrap_or("?"), 20),
                                title: output::truncate(
                                    w["title"]
                                        .as_str()
                                        .filter(|s| !s.is_empty())
                                        .unwrap_or("(no title)"),
                                    35,
                                ),
                                workspace: w["workspace"].as_str().unwrap_or("?").to_string(),
                                frame: format!(
                                    "{}x{} @ {}, {}",
                                    frame["width"].as_f64().unwrap_or(0.0) as u32,
                                    frame["height"].as_f64().unwrap_or(0.0) as u32,
                                    frame["x"].as_f64().unwrap_or(0.0) as i32,
                                    frame["y"].as_f64().unwrap_or(0.0) as i32
                                ),
                                focused: output::format_bool(
                                    w["isFocused"].as_bool().unwrap_or(false),
                                ),
                            }
                        })
                        .collect();

                    let table = Table::new(rows)
                        .with(Style::rounded())
                        .with(Modify::new(Columns::one(0)).with(Alignment::right()))
                        .with(Modify::new(Columns::one(4)).with(Alignment::right()))
                        .with(Modify::new(Columns::last()).with(Alignment::center()))
                        .to_string();

                    println!("{table}");
                }
            }
        }
        IpcResponse::Error { error } => {
            if json {
                println!(r#"{{"error":"{error}"}}"#);
            } else {
                println!("{} {error}", "Error:".red());
            }
        }
    }
}

/// Execute tiling query apps command.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn execute_query_apps(json: bool, _detailed: bool) {
    #[derive(Tabled)]
    struct AppRow {
        #[tabled(rename = "PID")]
        pid: i32,
        #[tabled(rename = "Name")]
        name: String,
        #[tabled(rename = "Bundle ID")]
        bundle_id: String,
        #[tabled(rename = "Visible")]
        visible: String,
    }

    // Send IPC query to app
    let response = match ipc_socket::send_query(IpcQuery::Apps) {
        Ok(r) => r,
        Err(IpcError::AppNotRunning) => {
            if json {
                println!(r#"{{"error":"Stache app is not running"}}"#);
            } else {
                println!("{}", "Stache app is not running.".red());
            }
            return;
        }
        Err(e) => {
            if json {
                println!(r#"{{"error":"{e}"}}"#);
            } else {
                println!("{} {e}", "Error:".red());
            }
            return;
        }
    };

    match response {
        IpcResponse::Success { data } => {
            if json {
                output::print_highlighted_json(&data);
            } else {
                // Parse apps from response
                let apps: Vec<serde_json::Value> = serde_json::from_value(data).unwrap_or_default();

                if apps.is_empty() {
                    println!("{}", "No running apps found.".dimmed());
                    return;
                }

                let count = apps.len();
                println!("{}", format!("Running Apps ({count})").bold());

                let rows: Vec<AppRow> = apps
                    .iter()
                    .map(|a| AppRow {
                        pid: a["pid"].as_i64().unwrap_or(0) as i32,
                        name: output::truncate(a["name"].as_str().unwrap_or("?"), 25),
                        bundle_id: output::truncate(a["bundleId"].as_str().unwrap_or("?"), 35),
                        visible: output::format_bool(!a["isHidden"].as_bool().unwrap_or(false)),
                    })
                    .collect();

                let table = Table::new(rows)
                    .with(Style::rounded())
                    .with(Modify::new(Columns::one(0)).with(Alignment::right()))
                    .with(Modify::new(Columns::last()).with(Alignment::center()))
                    .to_string();

                println!("{table}");
            }
        }
        IpcResponse::Error { error } => {
            if json {
                println!(r#"{{"error":"{error}"}}"#);
            } else {
                println!("{} {error}", "Error:".red());
            }
        }
    }
}

/// Execute tiling window commands.
///
/// Operations are executed in order: focus -> swap -> preset -> resize -> send.
/// Multiple operations can be combined in a single command.
#[allow(clippy::useless_let_if_seq)] // Clearer to track operation state this way
fn execute_window(args: &TilingWindowArgs) -> Result<(), StacheError> {
    let mut has_operation = false;

    // 1. Focus (changes which window we're operating on)
    if let Some(target) = &args.focus {
        ipc::send_notification(&StacheNotification::TilingWindowFocus(target.clone()));
        has_operation = true;
    }

    // 2. Swap position with another window
    if let Some(direction) = &args.swap {
        ipc::send_notification(&StacheNotification::TilingWindowSwap(
            format!("{direction:?}").to_lowercase(),
        ));
        has_operation = true;
    }

    // 3. Apply floating preset
    if let Some(name) = &args.preset {
        ipc::send_notification(&StacheNotification::TilingWindowPreset(name.clone()));
        has_operation = true;
    }

    // 4. Resize (can be multiple, collected as pairs in a flat Vec)
    if !args.resize.is_empty() {
        // Process resize args in pairs: [dim1, amt1, dim2, amt2, ...]
        for pair in args.resize.chunks(2) {
            if pair.len() == 2 {
                let dimension = &pair[0];
                let amount = &pair[1];
                // Validate dimension and amount
                if !["width", "height"].contains(&dimension.to_lowercase().as_str()) {
                    return Err(StacheError::InvalidArguments(format!(
                        "Invalid resize dimension '{dimension}'. Must be 'width' or 'height'."
                    )));
                }
                let amount_i32: i32 = amount.parse().map_err(|_| {
                    StacheError::InvalidArguments(format!(
                        "Invalid resize amount '{amount}'. Must be an integer."
                    ))
                })?;
                ipc::send_notification(&StacheNotification::TilingWindowResize {
                    dimension: dimension.to_lowercase(),
                    amount: amount_i32,
                });
            }
        }
        has_operation = true;
    }

    // 5. Send to screen
    if let Some(screen) = &args.send_to_screen {
        ipc::send_notification(&StacheNotification::TilingWindowSendToScreen(screen.clone()));
        has_operation = true;
    }

    // 6. Send to workspace
    if let Some(workspace) = &args.send_to_workspace {
        ipc::send_notification(&StacheNotification::TilingWindowSendToWorkspace(
            workspace.clone(),
        ));
        has_operation = true;
    }

    if has_operation {
        Ok(())
    } else {
        Err(StacheError::InvalidArguments(
            "No window operation specified. Use --help for available options.".to_string(),
        ))
    }
}

/// Execute tiling workspace commands.
///
/// Operations are executed in order: focus -> layout -> balance -> send.
/// Multiple operations can be combined in a single command.
#[allow(clippy::useless_let_if_seq)] // Clearer to track operation state this way
fn execute_workspace(args: &TilingWorkspaceArgs) -> Result<(), StacheError> {
    let mut has_operation = false;

    // 1. Focus workspace (switch to it first)
    if let Some(workspace) = &args.focus {
        ipc::send_notification(&StacheNotification::TilingFocusWorkspace(workspace.clone()));
        has_operation = true;
    }

    // 2. Change layout
    if let Some(layout) = &args.layout {
        ipc::send_notification(&StacheNotification::TilingSetLayout(layout.as_str().to_string()));
        has_operation = true;
    }

    // 3. Balance windows
    if args.balance {
        ipc::send_notification(&StacheNotification::TilingWorkspaceBalance);
        has_operation = true;
    }

    // 4. Send to screen
    if let Some(screen) = &args.send_to_screen {
        ipc::send_notification(&StacheNotification::TilingWorkspaceSendToScreen(screen.clone()));
        has_operation = true;
    }

    if has_operation {
        Ok(())
    } else {
        Err(StacheError::InvalidArguments(
            "No workspace operation specified. Use --help for available options.".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    // Helper struct to parse tiling commands in tests
    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: TilingCommands,
    }

    // ========================================================================
    // Query command parsing tests
    // ========================================================================

    #[test]
    fn test_tiling_query_screens_parse() {
        let cli = TestCli::try_parse_from(["test", "query", "screens"]).unwrap();
        match cli.command {
            TilingCommands::Query { json, detailed, command } => {
                assert!(!json);
                assert!(!detailed);
                assert!(matches!(command, Some(TilingQueryCommands::Screens)));
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_tiling_query_screens_json_parse() {
        let cli = TestCli::try_parse_from(["test", "query", "--json", "screens"]).unwrap();
        match cli.command {
            TilingCommands::Query { json, detailed, command } => {
                assert!(json);
                assert!(!detailed);
                assert!(matches!(command, Some(TilingQueryCommands::Screens)));
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_tiling_query_workspaces_parse() {
        let cli = TestCli::try_parse_from(["test", "query", "workspaces"]).unwrap();
        match cli.command {
            TilingCommands::Query { json, detailed, command } => {
                assert!(!json);
                assert!(!detailed);
                match command {
                    Some(TilingQueryCommands::Workspaces { focused_screen, screen }) => {
                        assert!(!focused_screen);
                        assert!(screen.is_none());
                    }
                    _ => panic!("Expected Workspaces command"),
                }
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_tiling_query_workspaces_focused_screen_parse() {
        let cli =
            TestCli::try_parse_from(["test", "query", "workspaces", "--focused-screen"]).unwrap();
        match cli.command {
            TilingCommands::Query { command, .. } => match command {
                Some(TilingQueryCommands::Workspaces { focused_screen, screen }) => {
                    assert!(focused_screen);
                    assert!(screen.is_none());
                }
                _ => panic!("Expected Workspaces command"),
            },
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_tiling_query_workspaces_screen_parse() {
        let cli =
            TestCli::try_parse_from(["test", "query", "workspaces", "--screen", "main"]).unwrap();
        match cli.command {
            TilingCommands::Query { command, .. } => match command {
                Some(TilingQueryCommands::Workspaces { focused_screen, screen }) => {
                    assert!(!focused_screen);
                    assert_eq!(screen, Some("main".to_string()));
                }
                _ => panic!("Expected Workspaces command"),
            },
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_tiling_query_windows_parse() {
        let cli = TestCli::try_parse_from(["test", "query", "windows"]).unwrap();
        match cli.command {
            TilingCommands::Query { command, .. } => match command {
                Some(TilingQueryCommands::Windows {
                    focused_screen,
                    focused_workspace,
                    screen,
                    workspace,
                }) => {
                    assert!(!focused_screen);
                    assert!(!focused_workspace);
                    assert!(screen.is_none());
                    assert!(workspace.is_none());
                }
                _ => panic!("Expected Windows command"),
            },
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_tiling_query_windows_focused_workspace_parse() {
        let cli =
            TestCli::try_parse_from(["test", "query", "windows", "--focused-workspace"]).unwrap();
        match cli.command {
            TilingCommands::Query { command, .. } => match command {
                Some(TilingQueryCommands::Windows { focused_workspace, .. }) => {
                    assert!(focused_workspace);
                }
                _ => panic!("Expected Windows command"),
            },
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_tiling_query_windows_workspace_parse() {
        let cli =
            TestCli::try_parse_from(["test", "query", "windows", "--workspace", "coding"]).unwrap();
        match cli.command {
            TilingCommands::Query { command, .. } => match command {
                Some(TilingQueryCommands::Windows { workspace, .. }) => {
                    assert_eq!(workspace, Some("coding".to_string()));
                }
                _ => panic!("Expected Windows command"),
            },
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_tiling_query_apps_parse() {
        let cli = TestCli::try_parse_from(["test", "query", "apps"]).unwrap();
        match cli.command {
            TilingCommands::Query { json, detailed, command } => {
                assert!(!json);
                assert!(!detailed);
                assert!(matches!(command, Some(TilingQueryCommands::Apps)));
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_tiling_query_detailed_flag_parse() {
        let cli = TestCli::try_parse_from(["test", "query", "-d", "windows"]).unwrap();
        match cli.command {
            TilingCommands::Query { json, detailed, command } => {
                assert!(!json);
                assert!(detailed);
                assert!(matches!(command, Some(TilingQueryCommands::Windows { .. })));
            }
            _ => panic!("Expected Query command"),
        }
    }

    #[test]
    fn test_tiling_query_detailed_long_flag_parse() {
        let cli = TestCli::try_parse_from(["test", "query", "--detailed", "apps"]).unwrap();
        match cli.command {
            TilingCommands::Query { json, detailed, command } => {
                assert!(!json);
                assert!(detailed);
                assert!(matches!(command, Some(TilingQueryCommands::Apps)));
            }
            _ => panic!("Expected Query command"),
        }
    }

    // ========================================================================
    // Window command parsing tests
    // ========================================================================

    #[test]
    fn test_tiling_window_focus_parse() {
        let cli = TestCli::try_parse_from(["test", "window", "--focus", "left"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => {
                assert_eq!(args.focus, Some("left".to_string()));
            }
            _ => panic!("Expected Window command"),
        }
    }

    #[test]
    fn test_tiling_window_swap_parse() {
        let cli = TestCli::try_parse_from(["test", "window", "--swap", "down"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => {
                assert_eq!(args.swap, Some(Direction::Down));
            }
            _ => panic!("Expected Window command"),
        }
    }

    #[test]
    fn test_tiling_window_preset_parse() {
        let cli = TestCli::try_parse_from(["test", "window", "--preset", "center"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => {
                assert_eq!(args.preset, Some("center".to_string()));
            }
            _ => panic!("Expected Window command"),
        }
    }

    #[test]
    fn test_tiling_window_resize_parse() {
        let cli = TestCli::try_parse_from(["test", "window", "--resize", "width", "100"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => {
                assert_eq!(args.resize, vec!["width", "100"]);
            }
            _ => panic!("Expected Window command"),
        }
    }

    #[test]
    fn test_tiling_window_resize_multiple_parse() {
        let cli = TestCli::try_parse_from([
            "test", "window", "--resize", "width", "100", "--resize", "height", "50",
        ])
        .unwrap();
        match cli.command {
            TilingCommands::Window(args) => {
                assert_eq!(args.resize, vec!["width", "100", "height", "50"]);
            }
            _ => panic!("Expected Window command"),
        }
    }

    #[test]
    fn test_tiling_window_resize_negative_parse() {
        let cli = TestCli::try_parse_from(["test", "window", "--resize", "width", "-50"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => {
                assert_eq!(args.resize, vec!["width", "-50"]);
            }
            _ => panic!("Expected Window command"),
        }
    }

    #[test]
    fn test_tiling_window_send_to_screen_parse() {
        let cli = TestCli::try_parse_from(["test", "window", "--send-to-screen", "main"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => {
                assert_eq!(args.send_to_screen, Some("main".to_string()));
            }
            _ => panic!("Expected Window command"),
        }
    }

    #[test]
    fn test_tiling_window_send_to_workspace_parse() {
        let cli =
            TestCli::try_parse_from(["test", "window", "--send-to-workspace", "coding"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => {
                assert_eq!(args.send_to_workspace, Some("coding".to_string()));
            }
            _ => panic!("Expected Window command"),
        }
    }

    #[test]
    fn test_tiling_window_combined_operations_parse() {
        let cli = TestCli::try_parse_from([
            "test", "window", "--swap", "right", "--resize", "width", "150",
        ])
        .unwrap();
        match cli.command {
            TilingCommands::Window(args) => {
                assert_eq!(args.swap, Some(Direction::Right));
                assert_eq!(args.resize, vec!["width", "150"]);
            }
            _ => panic!("Expected Window command"),
        }
    }

    // ========================================================================
    // Workspace command parsing tests
    // ========================================================================

    #[test]
    fn test_tiling_workspace_focus_parse() {
        let cli = TestCli::try_parse_from(["test", "workspace", "--focus", "coding"]).unwrap();
        match cli.command {
            TilingCommands::Workspace(args) => {
                assert_eq!(args.focus, Some("coding".to_string()));
            }
            _ => panic!("Expected Workspace command"),
        }
    }

    #[test]
    fn test_tiling_workspace_layout_parse() {
        let cli = TestCli::try_parse_from(["test", "workspace", "--layout", "dwindle"]).unwrap();
        match cli.command {
            TilingCommands::Workspace(args) => {
                assert_eq!(args.layout, Some(CliLayoutType::Dwindle));
            }
            _ => panic!("Expected Workspace command"),
        }
    }

    #[test]
    fn test_tiling_workspace_layout_monocle_parse() {
        let cli = TestCli::try_parse_from(["test", "workspace", "--layout", "monocle"]).unwrap();
        match cli.command {
            TilingCommands::Workspace(args) => {
                assert_eq!(args.layout, Some(CliLayoutType::Monocle));
            }
            _ => panic!("Expected Workspace command"),
        }
    }

    #[test]
    fn test_tiling_workspace_balance_parse() {
        let cli = TestCli::try_parse_from(["test", "workspace", "--balance"]).unwrap();
        match cli.command {
            TilingCommands::Workspace(args) => {
                assert!(args.balance);
            }
            _ => panic!("Expected Workspace command"),
        }
    }

    #[test]
    fn test_tiling_workspace_send_to_screen_parse() {
        let cli =
            TestCli::try_parse_from(["test", "workspace", "--send-to-screen", "main"]).unwrap();
        match cli.command {
            TilingCommands::Workspace(args) => {
                assert_eq!(args.send_to_screen, Some("main".to_string()));
            }
            _ => panic!("Expected Workspace command"),
        }
    }

    #[test]
    fn test_tiling_workspace_combined_operations_parse() {
        let cli =
            TestCli::try_parse_from(["test", "workspace", "--layout", "dwindle", "--balance"])
                .unwrap();
        match cli.command {
            TilingCommands::Workspace(args) => {
                assert_eq!(args.layout, Some(CliLayoutType::Dwindle));
                assert!(args.balance);
            }
            _ => panic!("Expected Workspace command"),
        }
    }

    // ========================================================================
    // Direction enum tests
    // ========================================================================

    #[test]
    fn test_direction_all_variants() {
        let cli = TestCli::try_parse_from(["test", "window", "--swap", "up"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => assert_eq!(args.swap, Some(Direction::Up)),
            _ => panic!("Expected Window command"),
        }

        let cli = TestCli::try_parse_from(["test", "window", "--swap", "down"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => assert_eq!(args.swap, Some(Direction::Down)),
            _ => panic!("Expected Window command"),
        }

        let cli = TestCli::try_parse_from(["test", "window", "--swap", "left"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => assert_eq!(args.swap, Some(Direction::Left)),
            _ => panic!("Expected Window command"),
        }

        let cli = TestCli::try_parse_from(["test", "window", "--swap", "right"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => assert_eq!(args.swap, Some(Direction::Right)),
            _ => panic!("Expected Window command"),
        }

        let cli = TestCli::try_parse_from(["test", "window", "--swap", "previous"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => assert_eq!(args.swap, Some(Direction::Previous)),
            _ => panic!("Expected Window command"),
        }

        let cli = TestCli::try_parse_from(["test", "window", "--swap", "next"]).unwrap();
        match cli.command {
            TilingCommands::Window(args) => assert_eq!(args.swap, Some(Direction::Next)),
            _ => panic!("Expected Window command"),
        }
    }

    // ========================================================================
    // CliLayoutType enum tests
    // ========================================================================

    #[test]
    fn test_cli_layout_type_all_variants() {
        let variants = [
            ("dwindle", CliLayoutType::Dwindle),
            ("split", CliLayoutType::Split),
            ("split-vertical", CliLayoutType::SplitVertical),
            ("split-horizontal", CliLayoutType::SplitHorizontal),
            ("monocle", CliLayoutType::Monocle),
            ("master", CliLayoutType::Master),
            ("grid", CliLayoutType::Grid),
            ("floating", CliLayoutType::Floating),
        ];

        for (name, expected) in variants {
            let cli = TestCli::try_parse_from(["test", "workspace", "--layout", name]).unwrap();
            match cli.command {
                TilingCommands::Workspace(args) => {
                    assert_eq!(args.layout, Some(expected), "Failed for layout: {name}");
                }
                _ => panic!("Expected Workspace command"),
            }
        }
    }
}
