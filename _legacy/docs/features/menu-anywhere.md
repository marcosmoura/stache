# MenuAnywhere

Read when changing the MenuAnywhere trigger, its Accessibility menu rebuild, or the module's pause/resume behaviour.

Rust paths are relative to `app/native/src/`.

## Purpose

Shows the frontmost application's menu bar at the cursor when the user clicks with a configured mouse button while holding a configured set of modifiers. The real menu is rebuilt from the Accessibility tree as an `NSMenu`, so clicking an item performs the original `AXPress`.

## Entry points

| Caller                                     | Entry                                                           |
| ------------------------------------------ | --------------------------------------------------------------- |
| App startup (background, `spawn_blocking`) | `menu_anywhere::init(handle)` in `lib.rs`                       |
| Tray                                       | `menu_anywhere::MenuAnywhereLifecycle` (registered in `lib.rs`) |

`init` returns silently when `menuAnywhere.enabled` is false or Accessibility is not granted. Otherwise it stores the `AppHandle`, spawns `event_monitor::start(&config)` on a thread, and sets `IS_RUNNING`.

## Key types

- `config/types/menu_anywhere.rs`: `MenuAnywhereConfig { enabled, modifiers, mouse_button }`, `MenuAnywhereModifier`, `MenuAnywhereMouseButton`, `required_modifier_flags()`.
- `modules/menu_anywhere/event_monitor.rs`: `start`, `set_enabled(bool)`, `tap_state() -> Option<bool>`, `RetainedEventTap`.
- `modules/menu_anywhere/menu_builder.rs`: `build_frontmost_app_menu()`, `build_menu_from_ax_element`, `ax_modifiers_to_ns`.

## Data flow

1. `event_monitor::start` pre-computes the expected CG event type (`rightMouseDown` = 3, `otherMouseDown` = 25 for middle click) and the required `CGEventFlags` mask into atomics, creates a HID `CGEventTap`, adds it to the current run loop and runs that loop.
2. The callback rejects everything except the expected event type, then requires **exact** equality between the event's modifier flags and the configured mask (`(flags & 0x001E_0000) == required`).
3. `trigger_menu_display` stores the click location and calls `showMenuAtPendingLocation` on the main thread via `performSelectorOnMainThread:`. The handler flips the y coordinate (`screen_height - y`), builds the menu, and pops it up with `popUpMenuPositioningItem:atLocation:inView:`.
4. `build_frontmost_app_menu` takes the frontmost PID, reads `AXMenuBar`, and recursively rebuilds `AXMenuBarItem`/`AXMenuItem` nodes: title, `AXEnabled`, `AXMenuItemMarkChar`, `AXMenuItemCmdChar`/`AXMenuItemCmdModifiers`. The Apple menu (first child of the root) is skipped.
5. Each item retains its `AXUIElement`. `menuItemClicked:` activates the frontmost app if needed and performs `AXPress` on that element.

The callback returns null for a matching event, so the click is swallowed and the app underneath does not also open its context menu.

## Config

Root key `menuAnywhere`. Defaults: `enabled: false`, `modifiers: ["control", "command"]`, `mouseButton: "rightClick"`. `required_modifier_flags` maps Control `0x0004_0000`, Option `0x0008_0000`, Command `0x0010_0000`, Shift `0x0002_0000`. See [config](../architecture/config.md).

## Permissions and threading

- Accessibility is mandatory: without it `CGEventTapCreate` returns null and the AX reads that build the menu fail. Stache checks the permission once at startup (`is_accessibility_granted`, `AXIsProcessTrusted` + prompt) and caches it in a `OnceLock`, so granting it later requires a restart.
- The tap callback runs on the monitor thread; everything that touches AppKit (`NSMenu`, `popUpMenuPositioningItem`, AX reads) is dispatched to the main thread. Keep it that way — building the menu on the tap thread is a crash risk.
- `build_frontmost_app_menu` is wrapped in `catch_unwind` because AX trees can produce panics on odd apps.

## Invariants and pitfalls

- Modifier matching is exact. Holding Shift on top of `Control+Command` will not trigger, and neither will a subset.
- `MenuAnywhereLifecycle::start` returns early while `IS_RUNNING` is set, but `init` itself has no guard: calling it directly twice creates a second tap thread.
- Pause/resume only flip `CGEventTapEnable` on the retained port; they do not rebuild the menu or re-read config.
- Apps that expose no `AXMenuBar` produce no menu.
- Status mapping is explicit in `menu_anywhere_status`: `ConfiguredOff` → not enabled, `Unavailable` when Accessibility is missing or the tap was never created, `Paused` when the tap exists but is disabled.

## Lifecycle and hot reload

`MenuAnywhereLifecycle` id `menuAnywhere`. `pause` → `set_enabled(false)`, `resume` → `set_enabled(true)`; both return an error if the tap was never created. Pausing is runtime-only and resets on restart.

## Tests

`modules/menu_anywhere/mod.rs` (`menu_anywhere_status_maps_states`), `event_monitor.rs` (flag constants, `CGPoint` layout, `Send`/`Sync` assertion), `menu_builder.rs` (`ax_modifiers_to_ns` matrix, cached CF string helpers, null-element guards), `config/types/menu_anywhere.rs` (defaults, flag mask).
