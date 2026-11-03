//! System tray module for Stache.
//!
//! Provides a system tray icon with a menu for quick access to app actions and
//! module pause/resume toggles.

use std::collections::HashMap;

use parking_lot::Mutex;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, Submenu};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{App, Manager, Wry};

use crate::modules::lifecycle_registry::LifecycleRegistry;
use crate::modules::services::lifecycle::ModuleStatus;

/// Menu item ID for the reload action (production only).
#[cfg(not(debug_assertions))]
const RELOAD_ID: &str = "reload";

/// Menu item ID for the quit action.
const QUIT_ID: &str = "quit";

/// Retained tray state so the icon and check items survive after setup and can
/// be updated after a toggle.
pub struct TrayMenuState {
    /// Retained for lifetime: dropping the tray icon removes it from the system.
    #[allow(dead_code)]
    tray: TrayIcon<Wry>,
    modules_submenu: Submenu<Wry>,
    check_items: Mutex<HashMap<String, CheckMenuItem<Wry>>>,
}

/// Maps a module status to the `(checked, enabled, text)` a `CheckMenuItem`
/// needs.
fn item_state(status: &ModuleStatus, name: &str) -> (bool, bool, String) {
    match status {
        ModuleStatus::Running => (true, true, name.to_string()),
        ModuleStatus::Paused => (false, true, name.to_string()),
        ModuleStatus::ConfiguredOff => (false, false, name.to_string()),
        ModuleStatus::Unavailable(reason) => (false, false, format!("{name} ({reason})")),
    }
}

impl TrayMenuState {
    /// Builds one `CheckMenuItem` per registered module and appends them to the
    /// retained Modules submenu. Idempotent.
    fn install_modules(&self, app: &tauri::AppHandle, registry: &LifecycleRegistry) {
        {
            let guard = self.check_items.lock();
            if !guard.is_empty() {
                return;
            }
        }

        let mut items: Vec<CheckMenuItem<Wry>> = Vec::new();
        for module in registry.modules() {
            let (checked, enabled, text) = item_state(&module.status(), module.name());
            let Ok(item) =
                CheckMenuItem::with_id(app, module.id(), text, enabled, checked, None::<&str>)
            else {
                tracing::error!(module = module.id(), "tray: failed to create check menu item");
                continue;
            };
            items.push(item.clone());
            self.check_items.lock().insert(module.id().to_string(), item);
        }
        if items.is_empty() {
            return;
        }

        let refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> =
            items.iter().map(|i| i as &dyn tauri::menu::IsMenuItem<Wry>).collect();
        if let Err(e) = self.modules_submenu.append_items(&refs) {
            tracing::error!(error = %e, "tray: failed to append module items");
        }
    }

    /// Handles a module toggle: disables the item immediately on the main
    /// thread, runs the OS pause/resume off-thread, then refreshes the item on
    /// the main thread.
    fn handle_toggle(&self, app: &tauri::AppHandle, id: &str) {
        let Some(item) = self.check_items.lock().get(id).cloned() else {
            return;
        };
        let _ = item.set_enabled(false);

        let Some(registry) = app.try_state::<LifecycleRegistry>() else {
            let _ = item.set_enabled(true);
            return;
        };
        let Some(module) = registry.get(id) else {
            let _ = item.set_enabled(true);
            return;
        };

        let module_name = module.name();
        let id_owned = id.to_string();
        let app = app.clone();
        crate::platform::thread::spawn_named_thread("tray-toggle", move || {
            let result = match module.status() {
                ModuleStatus::ConfiguredOff => Err(format!("{id_owned} is disabled in config")),
                ModuleStatus::Unavailable(reason) => {
                    Err(format!("{id_owned} is unavailable: {reason}"))
                }
                ModuleStatus::Running => module.pause().map(|()| module.status()),
                ModuleStatus::Paused => module.resume().map(|()| module.status()),
            };
            let _ = app.run_on_main_thread(move || match result {
                Ok(status) => {
                    let (checked, enabled, text) = item_state(&status, module_name);
                    let _ = item.set_enabled(enabled);
                    let _ = item.set_checked(checked);
                    let _ = item.set_text(text);
                }
                Err(err) => {
                    tracing::warn!(module = %id_owned, error = %err, "tray: module toggle failed");
                    let _ = item.set_enabled(true);
                }
            });
        });
    }
}

/// Initializes the system tray icon and menu.
///
/// The base menu (Reload in release, `Modules` placeholder, Quit) is installed
/// immediately; module check items are appended by [`install_modules_submenu`]
/// only after background startup completes.
///
/// # Panics
///
/// Panics if:
/// - The menu items or menu cannot be created
/// - The default window icon is missing
/// - The tray icon fails to build
pub fn init(app: &App) {
    let handle = app.handle();

    let quit_item = MenuItem::with_id(handle, QUIT_ID, "Quit Stache", true, None::<&str>)
        .expect("failed to create quit menu item");

    #[cfg(not(debug_assertions))]
    let reload_item = MenuItem::with_id(handle, RELOAD_ID, "Reload Stache", true, None::<&str>)
        .expect("failed to create reload menu item");

    let empty_items: [&dyn tauri::menu::IsMenuItem<Wry>; 0] = [];
    let modules_submenu = Submenu::with_items(handle, "Modules", true, &empty_items)
        .expect("failed to create modules submenu");

    #[cfg(not(debug_assertions))]
    let menu = Menu::with_items(handle, &[&reload_item, &modules_submenu, &quit_item])
        .expect("failed to create system tray menu");

    #[cfg(debug_assertions)]
    let menu = Menu::with_items(handle, &[&modules_submenu, &quit_item])
        .expect("failed to create system tray menu");

    let tray = TrayIconBuilder::new()
        .icon(handle.default_window_icon().expect("missing default window icon").clone())
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            #[cfg(not(debug_assertions))]
            RELOAD_ID => crate::app_shutdown::restart(app),
            QUIT_ID => crate::app_shutdown::exit(app),
            id => {
                if let Some(state) = app.try_state::<TrayMenuState>() {
                    state.handle_toggle(app, id);
                }
            }
        })
        .build(handle)
        .expect("failed to build system tray icon");

    app.manage(TrayMenuState {
        tray,
        modules_submenu,
        check_items: Mutex::new(HashMap::new()),
    });

    tracing::debug!("system tray initialized");
}

/// Appends the Modules submenu check items. Called after background startup.
pub fn install_modules_submenu(app: &tauri::AppHandle) {
    let state = app.state::<TrayMenuState>();
    let registry = app.state::<LifecycleRegistry>();
    state.install_modules(app, &registry);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_state_maps_running() {
        assert_eq!(
            item_state(&ModuleStatus::Running, "Wallpapers"),
            (true, true, "Wallpapers".to_string())
        );
    }

    #[test]
    fn item_state_maps_paused() {
        assert_eq!(
            item_state(&ModuleStatus::Paused, "NoTunes"),
            (false, true, "NoTunes".to_string())
        );
    }

    #[test]
    fn item_state_maps_configured_off() {
        assert_eq!(
            item_state(&ModuleStatus::ConfiguredOff, "Tiling"),
            (false, false, "Tiling".to_string())
        );
    }

    #[test]
    fn item_state_maps_unavailable_with_reason() {
        assert_eq!(
            item_state(
                &ModuleStatus::Unavailable("Accessibility permission required".into()),
                "Menu Anywhere"
            ),
            (
                false,
                false,
                "Menu Anywhere (Accessibility permission required)".to_string()
            )
        );
    }
}
