mod audio;
mod bar;
mod cmd_q;
mod config;
mod constants;
mod hotkey;
mod ipc;
mod notunes;
mod tiling;
mod utils;
mod wallpaper;

/// Runs the Tauri application.
///
/// # Panics
pub fn run() {
    // Initialize the configuration system early
    config::init();

    // Initialize wallpaper manager early so CLI commands can use it
    wallpaper::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .manage(bar::components::keepawake::KeepAwakeController::default())
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _| {
            // Single instance plugin ensures only one instance runs
            // CLI communication is handled via IPC socket
        }))
        .plugin(tauri_plugin_macos_permissions::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(hotkey::create_hotkey_plugin())
        .invoke_handler(tauri::generate_handler![
            bar::components::apps::open_app,
            bar::components::battery::get_battery_info,
            bar::components::cpu::get_cpu_info,
            bar::components::keepawake::is_system_awake,
            bar::components::keepawake::toggle_system_awake,
            bar::components::media::get_current_media_info,
            bar::components::weather::get_weather_config,
            tiling::commands::get_workspaces,
            tiling::commands::switch_workspace,
        ])
        .setup(move |app| {
            // Request accessibility permissions on startup if not already granted
            // This is needed for tiling window manager and global hotkeys
            tauri::async_runtime::block_on(async {
                if !tauri_plugin_macos_permissions::check_accessibility_permission().await {
                    tauri_plugin_macos_permissions::request_accessibility_permission().await;
                }
            });

            // Start watching the config file for changes
            config::watch_config_file(app.handle().clone());

            // Start IPC server for CLI communication
            ipc::start_ipc_server(app.handle().clone());

            // Initialize tiling window manager
            tiling::init(app.handle());

            // Initialize Bar components
            bar::init(app);

            // Start wallpaper manager (set initial wallpaper and start timer)
            wallpaper::start();

            // Initialize audio device manager
            audio::init();

            // Initialize noTunes (prevent Apple Music from auto-launching)
            notunes::init();

            // Initialize hold-to-quit (⌘Q) handler
            cmd_q::init(app.handle().clone());

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
