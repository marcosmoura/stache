pub mod components;
mod window;

use tauri::{App, Manager};

use crate::utils::window::{set_window_always_on_top, set_window_sticky};
use crate::widgets::window::monitor_click_outside;

pub fn init(app: &App) {
    let Some(webview_window) = app.app_handle().get_webview_window("widgets") else {
        eprintln!("stache: error: 'widgets' window not found in tauri.conf.json");
        return;
    };

    set_window_sticky(&webview_window);
    set_window_always_on_top(&webview_window);
    monitor_click_outside(app);

    // Initialize components
    components::init(&webview_window);

    // Open devtools if in dev mode
    #[cfg(debug_assertions)]
    {
        // webview_window.open_devtools();
    }
}
