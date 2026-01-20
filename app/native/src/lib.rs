//! Stache - A macOS status bar with tiling window manager integration.
//!
//! This library provides both the desktop application and CLI functionality.
//! The desktop app uses Tauri to render a status bar with workspace information,
//! media controls, system status, and more.

// Infrastructure modules
pub mod cache;
pub mod cli;
pub mod config;
pub mod constants;
pub mod error;
pub mod events;
pub mod schema;
mod utils;

// New infrastructure (being phased in)
pub mod core;
pub mod platform;
pub mod services;

// Feature modules
pub mod modules;

// Re-exports for backward compatibility and convenience
use std::sync::OnceLock;

pub use modules::{audio, tiling};
use modules::{bar, cmd_q, hotkey, menu_anywhere, notunes, wallpaper, widgets};

/// Cached accessibility permission status.
static ACCESSIBILITY_GRANTED: OnceLock<bool> = OnceLock::new();

/// Returns whether accessibility permissions have been granted.
///
/// This function checks once at startup and caches the result.
/// If permissions are not granted on first check, it prompts the user.
#[must_use]
pub fn is_accessibility_granted() -> bool {
    *ACCESSIBILITY_GRANTED.get_or_init(utils::accessibility::check_and_prompt)
}

/// Runs the Tauri desktop application.
///
/// This initializes all components and starts the GUI event loop.
///
/// # Panics
///
/// Panics if Tauri fails to initialize or the event loop encounters an error.
pub fn run() {
    // Initialize the configuration system early
    config::init();

    // Check accessibility permissions once at startup for features that need it
    // (tiling window manager, menu anywhere, etc.)
    let accessibility_granted = is_accessibility_granted();
    if !accessibility_granted {
        eprintln!("stache: accessibility permissions not granted. Some features will be disabled.");
    }

    // Initialize wallpaper manager early so CLI commands can use it
    wallpaper::setup();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _| {
            // Single instance plugin ensures only one instance runs
        }))
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_zustand::init())
        .manage(bar::components::keepawake::KeepAwakeController::default())
        .plugin(tauri_plugin_shell::init())
        .plugin(hotkey::create_hotkey_plugin())
        .invoke_handler(tauri::generate_handler![
            bar::components::apps::open_app,
            bar::components::battery::get_battery_info,
            bar::components::cpu::get_cpu_info,
            bar::components::keepawake::is_system_awake,
            bar::components::keepawake::toggle_system_awake,
            bar::components::media::get_current_media_info,
            bar::components::tiling::focus_tiling_window,
            bar::components::tiling::focus_tiling_workspace,
            bar::components::tiling::get_tiling_current_workspace_windows,
            bar::components::tiling::get_tiling_focused_window,
            bar::components::tiling::get_tiling_focused_workspace,
            bar::components::tiling::get_tiling_windows,
            bar::components::tiling::get_tiling_workspaces,
            bar::components::tiling::is_tiling_enabled,
            bar::components::weather::get_weather_config,
            bar::window::get_bar_window_frame,
        ])
        .setup(move |app| {
            // Make the app not appear in the dock
            if let Err(e) = app.handle().set_activation_policy(tauri::ActivationPolicy::Prohibited)
            {
                eprintln!("stache: warning: failed to set activation policy: {e}");
            }

            // Start watching the config file for changes
            config::watch_config_file(app.handle().clone());

            // Start IPC socket server for CLI queries
            utils::ipc_socket::init(|query| {
                tiling::init::handle_ipc_query(&query)
                    .unwrap_or_else(|| utils::ipc_socket::IpcResponse::error("Unknown query"))
            });

            // Initialize Bar components
            bar::init(app);

            // Initialize Widgets components
            widgets::init(app);

            // Start wallpaper manager
            wallpaper::init();

            // Initialize audio device manager
            audio::init();

            // Initialize noTunes
            notunes::init();

            // Initialize hold-to-quit (⌘Q) handler
            cmd_q::init(app.handle().clone());

            // Initialize MenuAnywhere
            menu_anywhere::init(app.handle().clone());

            // Initialize tiling window manager if enabled
            let tiling_config = config::get_config().tiling.clone();
            if tiling_config.is_enabled() {
                eprintln!("stache: tiling enabled, starting init...");
                tiling::init(app.handle().clone());
                eprintln!("stache: tiling init returned");
            }

            eprintln!("stache: setup complete!");
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                // Clean up IPC socket on exit
                utils::ipc_socket::stop_server();
            }
        });
}
