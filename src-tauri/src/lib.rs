use tauri::plugin::{Builder, PluginApi};
use tauri::{Manager, Wry};

mod bar;
mod cli;
mod launch;
mod utils;

/// Runs the Tauri application.
///
/// # Panics
pub fn run() {
    let (should_launch_ui, cli_exit_code) = launch::get_launch_mode();

    tauri::Builder::default()
        .plugin(tauri_plugin_cli::init())
        .plugin({
            Builder::new("helper")
                .setup(|app, _api: PluginApi<Wry, ()>| {
                    cli::handle_cli_invocation(app, &std::env::args().collect::<Vec<String>>());

                    Ok(())
                })
                .build()
        })
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            cli::handle_cli_invocation(app, &args);
        }))
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            bar::components::hyprspace::get_hyprspace_current_workspace_windows,
            bar::components::hyprspace::get_hyprspace_focused_window,
            bar::components::hyprspace::get_hyprspace_focused_workspace,
            bar::components::hyprspace::get_hyprspace_workspaces,
            bar::components::hyprspace::go_to_hyprspace_workspace,
        ])
        .setup(move |app| {
            if !should_launch_ui {
                app.windows().iter().for_each(|(_, w)| {
                    let _ = w.hide();
                    let _ = w.close();
                });

                app.cleanup_before_exit();
                app.app_handle().exit(cli_exit_code);

                return Ok(());
            }

            Ok(bar::init(app))
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
