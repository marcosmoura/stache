//! System tray module for Stache.
//!
//! Provides a system tray icon with a menu for quick access to app actions.

use tauri::App;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;

/// Menu item ID for the quit action.
const QUIT_ID: &str = "quit";

/// Initializes the system tray icon and menu.
///
/// Creates a tray icon using the app's default icon with a context menu
/// containing application actions like quit.
///
/// # Panics
///
/// Panics if:
/// - The menu items or menu cannot be created
/// - The default window icon is missing
/// - The tray icon fails to build
pub fn init(app: &App) {
    let handle = app.handle();

    // Create the menu items
    let quit_item = MenuItem::with_id(handle, QUIT_ID, "Quit Stache", true, None::<&str>)
        .expect("failed to create quit menu item");

    // Build the menu
    let menu = Menu::with_items(handle, &[&quit_item]).expect("failed to create system tray menu");

    // Build and attach the tray icon
    TrayIconBuilder::new()
        .icon(handle.default_window_icon().expect("missing default window icon").clone())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            if event.id.as_ref() == QUIT_ID {
                tracing::info!("quit requested via system tray");
                app.exit(0);
            }
        })
        .build(handle)
        .expect("failed to build system tray icon");

    tracing::debug!("system tray initialized");
}
