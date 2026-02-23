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
mod logging;
pub mod schema;
mod utils;

// New infrastructure (being phased in)
pub mod platform;
pub mod services;

// Feature modules
pub mod modules;

// Re-exports for backward compatibility and convenience
use std::sync::OnceLock;

pub use modules::{audio, tiling};
use modules::{bar, cmd_q, hotkey, menu_anywhere, notunes, tray, wallpaper, widgets};
use tauri::App;

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

fn load_base_modules(app: &App) {
    // Start watching the config file for changes
    config::watch_config_file(app.handle().clone());

    // Start IPC socket server for CLI queries
    utils::ipc_socket::init(|query| {
        tiling::init::handle_ipc_query(&query)
            .unwrap_or_else(|| utils::ipc_socket::IpcResponse::error("Unknown query"))
    });

    // Initialize system tray
    tracing::debug!("initializing system tray");
    tray::init(app);

    // Initialize Bar (UI-critical, blocks until window is ready)
    tracing::debug!("initializing bar");
    bar::init(app);

    // Initialize Widgets (UI-critical)
    tracing::debug!("initializing widgets");
    widgets::init(app);
}

fn lazy_load_modules(app: &App, config: &config::StacheConfig) {
    let handle = app.handle().clone();
    let tiling_config = config.tiling.clone();
    let cmd_q_config = config.command_quit.clone();

    tauri::async_runtime::spawn(async move {
        tracing::debug!("starting parallel background initialization");

        // All these modules are independent - initialize in parallel
        let results = tokio::join!(
            tokio::task::spawn_blocking(|| {
                tracing::debug!("initializing wallpaper manager");
                wallpaper::init();
            }),
            tokio::task::spawn_blocking(|| {
                tracing::debug!("initializing audio manager");
                audio::init();
            }),
            tokio::task::spawn_blocking(|| {
                tracing::debug!("initializing notunes");
                notunes::init();
            }),
            tokio::task::spawn_blocking({
                let h = handle.clone();
                move || {
                    tracing::debug!("initializing cmd-q handler");
                    cmd_q::init(h, &cmd_q_config);
                }
            }),
            tokio::task::spawn_blocking({
                let h = handle.clone();
                move || {
                    tracing::debug!("initializing menu anywhere");
                    menu_anywhere::init(h);
                }
            }),
        );

        // Log any panics from spawned tasks
        let (wallpaper_r, audio_r, notunes_r, cmd_q_r, menu_r) = results;
        if let Err(e) = wallpaper_r {
            tracing::error!("wallpaper init panicked: {e}");
        }
        if let Err(e) = audio_r {
            tracing::error!("audio init panicked: {e}");
        }
        if let Err(e) = notunes_r {
            tracing::error!("notunes init panicked: {e}");
        }
        if let Err(e) = cmd_q_r {
            tracing::error!("cmd_q init panicked: {e}");
        }
        if let Err(e) = menu_r {
            tracing::error!("menu_anywhere init panicked: {e}");
        }

        // Initialize tiling window manager if enabled (after other modules)
        if tiling_config.is_enabled() {
            tracing::info!("tiling window manager enabled, initializing");
            tiling::init(handle.clone());
            tracing::debug!("tiling initialization complete");
        }

        tracing::info!("background initialization complete");
    });
}

/// Runs the Tauri desktop application.
///
/// This initializes all components and starts the GUI event loop.
///
/// # Panics
///
/// Panics if Tauri fails to initialize or the event loop encounters an error.
pub fn run() {
    // Initialize logging first
    logging::init();

    // Initialize the configuration system early
    config::init();

    // Check accessibility permissions once at startup for features that need it
    // (tiling window manager, menu anywhere, etc.)
    let accessibility_granted = is_accessibility_granted();
    if !accessibility_granted {
        tracing::warn!("accessibility permissions not granted - some features will be disabled");
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
            // Make the app not appear in the dock (must be synchronous, first thing)
            if let Err(e) = app.handle().set_activation_policy(tauri::ActivationPolicy::Prohibited)
            {
                tracing::warn!(error = %e, "failed to set activation policy");
            }

            // Load critical base modules (blocking)
            load_base_modules(app);

            // Spawn parallel initialization for background modules
            lazy_load_modules(app, config::get_config());

            tracing::info!("setup complete (background tasks spawned)");
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                tracing::info!("application exiting, cleaning up");
                // Clean up IPC socket on exit
                utils::ipc_socket::stop_server();
            }
        });
}
