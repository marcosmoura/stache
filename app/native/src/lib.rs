//! Stache - A macOS status bar with tiling window manager integration.
//!
//! This library provides both the desktop application and CLI functionality.
//! The desktop app uses Tauri to render a status bar with workspace information,
//! media controls, system status, and more.

// Core modules
pub mod audio;
pub mod cache;
pub mod cli;
pub mod config;
pub mod constants;
pub mod error;
pub mod events;
pub mod schema;

// Desktop app modules
mod bar;
mod cmd_q;
mod hotkey;
mod menu_anywhere;
mod notunes;
mod utils;
mod wallpaper;
mod widgets;

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

    // Initialize wallpaper manager early so CLI commands can use it
    wallpaper::init();

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
            bar::components::hyprspace::focus_window_by_window_id,
            bar::components::hyprspace::get_hyprspace_current_workspace_windows,
            bar::components::hyprspace::get_hyprspace_focused_window,
            bar::components::hyprspace::get_hyprspace_focused_workspace,
            bar::components::hyprspace::get_hyprspace_workspaces,
            bar::components::hyprspace::go_to_hyprspace_workspace,
            bar::components::keepawake::is_system_awake,
            bar::components::keepawake::toggle_system_awake,
            bar::components::media::get_current_media_info,
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

            // Initialize Bar components
            bar::init(app);

            // Initialize Widgets components
            widgets::init(app);

            // Start wallpaper manager (set initial wallpaper and start timer)
            wallpaper::start();

            // Initialize audio device manager
            audio::init();

            // Initialize noTunes (prevent Apple Music from auto-launching)
            notunes::init();

            // Initialize hold-to-quit (⌘Q) handler
            cmd_q::init(app.handle().clone());

            // Initialize MenuAnywhere (summon menu bar at cursor)
            menu_anywhere::init(app.handle().clone());

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
