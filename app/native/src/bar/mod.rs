pub mod components;
pub mod constants;
mod ipc_listener;
mod menubar;
mod screen;
pub mod window;

use tauri::{App, Manager};

use crate::utils::window::{set_window_below_menu, set_window_sticky};

pub fn init(app: &App) {
    let app_handle = app.app_handle().clone();

    let Some(webview_window) = app_handle.get_webview_window("bar") else {
        eprintln!("stache: error: 'bar' window not found in tauri.conf.json");
        return;
    };

    set_window_sticky(&webview_window);
    set_window_below_menu(&webview_window);
    window::set_window_position(&webview_window);

    let webview_watcher_clone = webview_window.clone();
    screen::init_screen_watcher(move || window::set_window_position(&webview_watcher_clone));

    menubar::start_menu_bar_visibility_watcher(&webview_window);

    // Initialize components
    components::init(&webview_window);

    // Initialize IPC listener for CLI notifications
    ipc_listener::init(app_handle);

    // Show the window
    if let Err(e) = webview_window.show() {
        eprintln!("stache: error: failed to show bar window: {e}");
    }

    // Open devtools if in dev mode
    #[cfg(debug_assertions)]
    {
        webview_window.open_devtools();
    }
}
