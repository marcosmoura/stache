# Tray

Read when changing the tray icon, its menu structure, or the module pause/resume menu.

Rust paths are relative to `app/native/src/`.

## Purpose

Adds a status item to the macOS menu bar with Reload (release builds), a `Modules` submenu of toggleable features, and Quit. It is the only UI surface for pausing/resuming modules at runtime.

## Entry points

| Caller                             | Entry                                                                                       |
| ---------------------------------- | ------------------------------------------------------------------------------------------- |
| App startup, base phase (blocking) | `tray::init(app)` in `lib.rs::load_base_modules`                                            |
| After background startup completes | `modules::tray::install_modules_submenu(&handle)` at the end of `lib.rs::lazy_load_modules` |
| Menu clicks                        | `TrayIconBuilder::on_menu_event` → `TrayMenuState::handle_toggle` or `app_shutdown`         |

## Key types

- `modules/tray/mod.rs`: `TrayMenuState { tray, modules_submenu, check_items }` (managed app state), `item_state(&ModuleStatus, name)`, `init`, `install_modules_submenu`.
- `modules/services/lifecycle.rs`: `ModuleStatus::{ConfiguredOff, Running, Paused, Unavailable(String)}` and the `LifecycleModule` trait the menu drives.
- `modules/lifecycle_registry.rs`: `LifecycleRegistry` — the fixed list of modules registered in `lib.rs::run` before `load_base_modules`.

## Data flow

1. `init` builds `Quit Stache` and `Modules` (plus `Reload Stache` in release only; the id `reload` does not exist in debug), builds the `TrayIcon` with `show_menu_on_left_click(true)`, and stores `TrayMenuState` via `app.manage`.
2. `install_modules_submenu` snapshots `registry.modules()`, turns each into a `CheckMenuItem` whose **id is the module id** and whose label is `module.name()` plus any `Unavailable` reason, and appends them. Idempotent: it returns early if items already exist.
3. `item_state` maps status → `(checked, enabled, text)`: `Running` → checked+enabled, `Paused` → unchecked+enabled, `ConfiguredOff` → unchecked+disabled, `Unavailable(reason)` → unchecked+disabled with `"{name} ({reason})"`.
4. A click on a module item calls `handle_toggle`: disables the item immediately, looks the module up by id, and on a `tray-toggle` thread calls `pause()`/`resume()` (or synthesises the same error strings for `ConfiguredOff`/`Unavailable`). The result is applied back on the main thread through `run_on_main_thread`, which sets enabled/checked/text (or re-enables the item on error).
5. `Reload Stache` (release only) → `app_shutdown::restart`; `Quit Stache` → `app_shutdown::exit`.

## Module ids

`wallpapers`, `commandQuit`, `notunes`, `proxyAudio`, `menuAnywhere`, `tiling` (see the tiling doc for its lifecycle).

## Permissions and threading

- No permissions.
- All menu/tray APIs (`CheckMenuItem`, `Submenu`, `TrayIconBuilder`, `set_checked`, `set_text`) must run on the main thread. `handle_toggle` deliberately keeps only the module lookup and the item updates there; the actual `pause`/`resume` OS calls run off-thread, which is why `LifecycleRegistry::toggle` can block.
- The registry is a `Mutex<Vec<Arc<dyn LifecycleModule>>>`. Adding a module means registering it in `lib.rs::run`; `install_modules_submenu` only reads the snapshot.

## Invariants and pitfalls

- The submenu is installed **after** background init so the initial check states reflect real module status. If you install it earlier, modules that are still starting show as `Paused`.
- Item ids are module ids; renaming a module's `id()` changes only the menu routing key, but keep ids stable anyway.
- `TrayMenuState.tray` is retained only to keep the icon alive; dropping it removes the icon from the menu bar.
- Menu items are never rebuilt: toggling only mutates the existing `CheckMenuItem`. Adding a module after `install_modules_submenu` requires a restart.
- `ConfiguredOff` and `Unavailable` items are disabled, so the tray can only pause/resume modules that are enabled in config. `Paused` is runtime-only and does not persist.
- The icon uses `default_window_icon()` and `init` panics when the icon or a menu construction fails.

## Lifecycle and hot reload

The tray itself has no lifecycle entry. Config changes restart the app in release (`config/watcher.rs`), which recreates the tray and resets every module to its configured state — see [config](../architecture/config.md). Runtime pause state is lost on reload by design.

## Tests

`modules/tray/mod.rs` (`item_state_maps_running`, `item_state_maps_paused`, `item_state_maps_configured_off`, `item_state_maps_unavailable_with_reason`), `modules/lifecycle_registry.rs` (register/get, toggle semantics, lock-not-held-during-OS-call).
