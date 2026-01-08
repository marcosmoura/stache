use tauri::WebviewWindow;

pub mod apps;
pub mod battery;
pub mod cpu;
pub mod hyprspace;
pub mod keepawake;
pub mod media;
pub mod weather;

pub fn init(window: &WebviewWindow) {
    keepawake::init(window);
    media::init(window);
}
