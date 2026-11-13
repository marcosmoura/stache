//! The tray icon, its clicks and its minimal native menu.
//!
//! The tray is created on the `AppKit` main thread before the host starts its
//! slow services. A left click toggles the popover under the icon; a right
//! click shows the native menu `Open Stache`, separator, `Quit Stache Next`.
//! Callbacks only queue work; they never join a module, wait for the socket
//! or block on configuration I/O. Clicks and menu rows are mapped by pure
//! functions so they are testable without `AppKit`.

use stache_macos::IconAnchor;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager as _};

use crate::host::HostHandle;

/// The tray icon identity.
pub const TRAY_ID: &str = "stache-next";

/// The tray tooltip.
pub const TOOLTIP: &str = "Stache Next";

/// One menu or click action.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrayAction {
    /// Toggle the popover under the icon.
    TogglePopover(IconAnchor),
    /// Remember the icon rectangle for a later `Open Stache`.
    NoteAnchor(IconAnchor),
    /// Show the popover under the last icon rectangle.
    OpenPopover,
    /// Quit through the shared shutdown coordinator.
    Quit,
}

/// One rendered menu row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayRow {
    /// A menu item: callback identifier and visible text.
    Item {
        /// Stable callback identifier.
        id: &'static str,
        /// Visible text.
        label: &'static str,
    },
    /// A separator.
    Separator,
}

/// The right-click menu, in order.
pub const MENU: [TrayRow; 3] = [
    TrayRow::Item {
        id: "open",
        label: "Open Stache",
    },
    TrayRow::Separator,
    TrayRow::Item {
        id: "quit",
        label: "Quit Stache Next",
    },
];

/// Creates the tray, its menu and its click handling.
///
/// # Errors
/// Returns a Tauri menu or tray construction error.
pub fn create(app: &AppHandle, host: HostHandle) -> tauri::Result<()> {
    let menu = menu(app)?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(TOOLTIP)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| {
            if let Some(action) = menu_action(event.id.as_ref()) {
                run(app, &host, action);
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let Some(action) = click_action(&event) {
                // Clicks never quit, so no host handle is needed here.
                run_click(tray.app_handle(), action);
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let mut items: Vec<Box<dyn IsMenuItem<tauri::Wry>>> = Vec::new();
    for row in &MENU {
        match row {
            TrayRow::Separator => items.push(Box::new(PredefinedMenuItem::separator(app)?)),
            TrayRow::Item { id, label } => {
                items.push(Box::new(MenuItem::with_id(
                    app,
                    *id,
                    *label,
                    true,
                    None::<&str>,
                )?));
            }
        }
    }
    let references: Vec<&dyn IsMenuItem<tauri::Wry>> =
        items.iter().map(std::convert::AsRef::as_ref).collect();
    Menu::with_items(app, &references)
}

/// The action of a menu row.
#[must_use]
pub fn menu_action(id: &str) -> Option<TrayAction> {
    match id {
        "open" => Some(TrayAction::OpenPopover),
        "quit" => Some(TrayAction::Quit),
        _ => None,
    }
}

/// The action of a tray icon event: a left button release toggles the
/// popover; a right button press (which opens the menu) records the icon
/// rectangle. Everything else is ignored.
#[must_use]
pub fn click_action(event: &TrayIconEvent) -> Option<TrayAction> {
    let TrayIconEvent::Click { rect, button, button_state, .. } = event else {
        return None;
    };
    let anchor = anchor(rect);
    match (button, button_state) {
        (MouseButton::Left, MouseButtonState::Up) => Some(TrayAction::TogglePopover(anchor)),
        (MouseButton::Right, MouseButtonState::Down) => Some(TrayAction::NoteAnchor(anchor)),
        _ => None,
    }
}

/// The click rectangle as reported: physical pixels of the icon's display.
fn anchor(rect: &tauri::Rect) -> IconAnchor {
    let (x, y) = match rect.position {
        tauri::Position::Physical(position) => (f64::from(position.x), f64::from(position.y)),
        tauri::Position::Logical(position) => (position.x, position.y),
    };
    let (width, height) = match rect.size {
        tauri::Size::Physical(size) => (f64::from(size.width), f64::from(size.height)),
        tauri::Size::Logical(size) => (size.width, size.height),
    };
    IconAnchor { x, y, width, height }
}

fn run(app: &AppHandle, host: &HostHandle, action: TrayAction) {
    if action == TrayAction::Quit {
        host.request_shutdown();
    } else {
        run_click(app, action);
    }
}

fn run_click(app: &AppHandle, action: TrayAction) {
    // The window backend is managed after the tray exists; until then a click
    // does nothing.
    let Some(windows) = app.try_state::<crate::WindowsState>() else {
        return;
    };
    let _ = match action {
        TrayAction::TogglePopover(anchor) => windows.0.toggle_popover(anchor),
        TrayAction::NoteAnchor(anchor) => {
            windows.0.note_tray_anchor(anchor);
            Ok(())
        }
        TrayAction::OpenPopover => windows.0.open_popover(),
        TrayAction::Quit => Ok(()),
    };
}

#[cfg(test)]
mod tests {
    use tauri::tray::TrayIconId;
    use tauri::{PhysicalPosition, PhysicalSize, Position, Rect, Size};

    use super::*;

    fn click(button: MouseButton, button_state: MouseButtonState) -> TrayIconEvent {
        TrayIconEvent::Click {
            id: TrayIconId::new(TRAY_ID),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect {
                position: Position::Physical(PhysicalPosition::new(2000, 0)),
                size: Size::Physical(PhysicalSize::new(48, 66)),
            },
            button,
            button_state,
        }
    }

    const ANCHOR: IconAnchor = IconAnchor {
        x: 2000.0,
        y: 0.0,
        width: 48.0,
        height: 66.0,
    };

    #[test]
    fn a_left_release_toggles_the_popover_at_the_icon() {
        assert_eq!(
            click_action(&click(MouseButton::Left, MouseButtonState::Up)),
            Some(TrayAction::TogglePopover(ANCHOR))
        );
    }

    #[test]
    fn a_right_press_only_remembers_the_icon_for_the_menu() {
        assert_eq!(
            click_action(&click(MouseButton::Right, MouseButtonState::Down)),
            Some(TrayAction::NoteAnchor(ANCHOR))
        );
    }

    #[test]
    fn presses_releases_and_other_buttons_are_ignored() {
        for (button, state) in [
            (MouseButton::Left, MouseButtonState::Down),
            (MouseButton::Right, MouseButtonState::Up),
            (MouseButton::Middle, MouseButtonState::Down),
            (MouseButton::Middle, MouseButtonState::Up),
        ] {
            assert_eq!(click_action(&click(button, state)), None, "{button:?} {state:?}");
        }
        let enter = TrayIconEvent::Enter {
            id: TrayIconId::new(TRAY_ID),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
        };
        assert_eq!(click_action(&enter), None);
    }

    #[test]
    fn the_menu_has_exactly_open_a_separator_and_quit() {
        assert_eq!(MENU, [
            TrayRow::Item {
                id: "open",
                label: "Open Stache",
            },
            TrayRow::Separator,
            TrayRow::Item {
                id: "quit",
                label: "Quit Stache Next",
            },
        ]);
        assert_eq!(menu_action("open"), Some(TrayAction::OpenPopover));
        assert_eq!(menu_action("quit"), Some(TrayAction::Quit));
        for gone in [
            "test-window",
            "access-guide",
            "enable:tiling",
            "pause:bar",
            "status",
        ] {
            assert_eq!(menu_action(gone), None, "{gone}");
        }
        assert_eq!(TOOLTIP, "Stache Next");
    }
}
