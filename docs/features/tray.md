# Tray

Read when changing the menu bar item, its clicks or its menu. **Shipped behavior.** The
source of truth is `src/crates/app/src/tray.rs`.

## Purpose

The tray is the always-present entry point of the desktop app. A left click toggles the
[popover](popover.md) under the icon; a right click shows a minimal native menu. Module
status and actions live in the popover, not in the menu.

There is no Dock icon. The app supplies no application menu, so Tauri installs its default
macOS one; under the Accessory activation policy no menu bar is displayed, but the menu
object and its Quit key equivalent exist, which is why Cmd-Q reaches AppKit.

## Creation

- `tray::create` runs during Tauri setup, after `set_activation_policy(Accessory)` and
  before the host starts its slow services. The tray id is `stache-next`; the tooltip is
  `Stache Next` (static); the icon is the app's `default_window_icon()`;
  `show_menu_on_left_click(false)`.
- The menu is built once. Nothing rebuilds it on a status change: there is no
  `tray::update`.
- The coordinator's `tray` owner removes the tray by id through its own acknowledged
  main-thread hop, separate from the final exit; a missing tray counts as removed.

## Clicks and menu

`click_action(&TrayIconEvent)` and `menu_action(id)` are pure, so the mapping is testable
without AppKit:

| Input                            | Action                                                                   |
| -------------------------------- | ------------------------------------------------------------------------ |
| Left button release              | `TogglePopover(rect)`: show the popover under the icon, or hide it       |
| Right button press               | `NoteAnchor(rect)`: remember the icon rectangle; AppKit shows the menu   |
| Menu `Open Stache` (`open`)      | `OpenPopover`: show the popover under the last remembered icon rectangle |
| Menu `Quit Stache Next` (`quit`) | `Quit`: `HostHandle::request_shutdown`                                   |
| Anything else                    | ignored                                                                  |

The menu is exactly `Open Stache`, a separator, `Quit Stache Next` (`tray::MENU`). The
rectangle is the click event's icon rectangle, in physical pixels of the icon's display.

Quit calls the one idempotent teardown coordinator. Cmd-Q and other AppKit-initiated quits
reach the same coordinator through the observed `applicationShouldTerminate:`; their
real-session behavior is not verified yet, so the [roadmap](../roadmap.md) follow-up stays
open.

## Former menu rows

The former menu rows (status header, gate actions, per-module Enable, Pause/Resume, Retry,
diagnostics, warnings and Quit) live in the popover's
[Overview](popover.md#overview), with the same state labels and eligibility rules.

## Invariants and pitfalls

- Click and menu callbacks never join a module, wait for the socket or read configuration
  inline; they queue native work or one host command.
- A click before the window backend is managed (early setup) does nothing.
- All menu construction runs on the main thread.

## Tests

`src/crates/app/src/tray.rs`: a left release toggles at the icon rectangle, a right press
only records it, other presses, releases, buttons and enter events are ignored, and the
menu is exactly the two items and a separator with no former row id mapping to an action.
The popover lifecycle the clicks drive is tested in `windows.rs` (see
[popover](popover.md#tests)). A real tray click remains a manual check.
